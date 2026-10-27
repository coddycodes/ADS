//! `agentSessions.scan` and `agentSessions.import`: Claude Code and Codex CLI sessions recorded
//! on this host, offered as project threads whose next turn resumes the CLI conversation.

use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    path::{Path, PathBuf},
    time::SystemTime,
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::{
    provider_inventory::instance_custom_models,
    provider_runtime::{native_adapter_key, resolve_provider_route_settings},
};
use crate::{
    agent_sessions::{self, AgentSessionProvider, ParsedSession, SessionSource},
    orchestration::{
        CommandAdmission, OrchestrationCommand, OrchestrationEngine, canonical_command_digest,
        engine::{ImportedThreadMessage, MAX_IMPORTED_THREAD_MESSAGES},
    },
    persistence::{ProjectionProject, ProviderSessionRuntime},
    provider::{
        claude,
        environment::{claude_config_directory, codex_home_directory},
    },
    rpc::{RpcRegistry, RpcRequest, RpcResult},
    server_settings::ProviderSettingsStore,
};

/// The built-in instance scanned for each driver; other instances are out of scope.
const SOURCES: [(AgentSessionProvider, &str); 2] = [
    (AgentSessionProvider::ClaudeAgent, "claudeAgent"),
    (AgentSessionProvider::Codex, "codex"),
];
/// Mirrors `DEFAULT_MODEL_BY_PROVIDER` in `packages/contracts/src/model.ts`.
const CLAUDE_DEFAULT_MODEL: &str = "claude-sonnet-5";
const CODEX_DEFAULT_MODEL: &str = "gpt-5.4";
const IMPORTED_RUNTIME_MODE: &str = "full-access";
const USED_BEFORE_IMPORT: &str =
    "This session's thread was used before its import finished, so it keeps its own conversation.";
const IMPORTED_INTERACTION_MODE: &str = "default";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScanInput {
    project_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImportInput {
    project_id: String,
    sessions: Vec<SessionRef>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionRef {
    provider: AgentSessionProvider,
    session_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Candidate {
    provider: AgentSessionProvider,
    session_id: String,
    title: String,
    message_count: usize,
    last_active_at: String,
    already_imported: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread_id: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Imported {
    session_id: String,
    thread_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Skipped {
    session_id: String,
    reason: String,
}

#[derive(Clone)]
struct Services {
    orchestration: OrchestrationEngine,
    settings_root: PathBuf,
}

struct ResolvedSource {
    source: SessionSource,
    instance_id: &'static str,
}

pub fn register_agent_sessions_rpc(
    registry: &mut RpcRegistry,
    orchestration: OrchestrationEngine,
    settings_root: PathBuf,
) {
    let services = Services {
        orchestration,
        settings_root,
    };
    let scan_services = services.clone();
    registry.register_unary("agentSessions.scan", move |request, cancellation| {
        let services = scan_services.clone();
        async move { cancellable(&cancellation, scan(&services, request, &cancellation)).await }
    });
    registry.register_unary("agentSessions.import", move |request, cancellation| {
        let services = services.clone();
        async move { import(&services, request, &cancellation).await }
    });
}

fn error(message: impl Into<String>) -> Value {
    json!({ "_tag": "AgentSessionsError", "message": message.into() })
}

fn decode<T: for<'de> Deserialize<'de>>(request: RpcRequest) -> Result<T, Value> {
    serde_json::from_value(request.payload)
        .map_err(|cause| error(format!("The request was invalid: {cause}")))
}

async fn cancellable(
    cancellation: &CancellationToken,
    work: impl Future<Output = RpcResult>,
) -> RpcResult {
    tokio::select! {
        biased;
        () = cancellation.cancelled() => Err(error("The request was cancelled.")),
        result = work => result,
    }
}

fn thread_id(instance_id: &str, session_id: &str) -> String {
    format!("import:{instance_id}:{session_id}")
}

/// Native sessions that Coddy Studio threads other than their own import already run. Those
/// transcripts sit in the same homes, and a second thread resuming one would share it.
async fn sessions_owned_by_other_threads(services: &Services) -> Result<HashSet<String>, Value> {
    let owners = services
        .orchestration
        .repositories()
        .list_provider_session_owners()
        .await
        .map_err(|_| error("Coddy Studio could not read its provider sessions. Try again."))?;
    Ok(owners
        .into_iter()
        .filter(|(session_id, owner)| !is_import_of(owner, session_id))
        .map(|(session_id, _)| session_id)
        .collect())
}

fn is_import_of(thread_id: &str, session_id: &str) -> bool {
    thread_id
        .strip_prefix("import:")
        .and_then(|rest| rest.split_once(':'))
        .is_some_and(|(_, imported)| imported == session_id)
}

/// The accepted history command is the last step of an import.
async fn history_imported(
    repositories: &crate::persistence::Repositories,
    thread_id: &str,
) -> crate::persistence::Result<bool> {
    Ok(repositories
        .get_command_receipt(format!("{thread_id}:history"))
        .await?
        .is_some_and(|receipt| receipt.status == "accepted"))
}

async fn live_project(services: &Services, project_id: &str) -> Result<ProjectionProject, Value> {
    services
        .orchestration
        .repositories()
        .get_project(project_id.to_owned())
        .await
        .map_err(|_| error("The project could not be read. Try again."))?
        .filter(|project| project.deleted_at.is_none())
        .ok_or_else(|| error("The project no longer exists."))
}

/// The configured home of each built-in instance that is enabled; a disabled driver could not
/// resume an imported session.
async fn resolve_sources(settings_root: &PathBuf) -> Vec<ResolvedSource> {
    let mut sources = Vec::new();
    for (provider, instance_id) in SOURCES {
        let Ok(route) = resolve_provider_route_settings(settings_root, instance_id, None).await
        else {
            continue;
        };
        if route.provider != provider.kind() {
            continue;
        }
        let environment = route
            .environment
            .iter()
            .map(|(name, value)| (OsString::from(name), OsString::from(value)))
            .collect::<Vec<_>>();
        let home = match (provider, route.codex) {
            (AgentSessionProvider::ClaudeAgent, _) => claude_config_directory(&environment),
            // A configured home (or shadow home) shares its sessions with `shared_home_path`;
            // otherwise the driver sets no CODEX_HOME and Codex resolves its own home from the
            // instance environment.
            (AgentSessionProvider::Codex, Some(codex)) => Some(
                codex
                    .home
                    .effective_home_path
                    .is_none()
                    .then(|| codex_home_directory(&environment))
                    .flatten()
                    .unwrap_or(codex.home.shared_home_path),
            ),
            (AgentSessionProvider::Codex, None) => None,
        };
        if let Some(home) = home {
            sources.push(ResolvedSource {
                source: SessionSource { provider, home },
                instance_id,
            });
        }
    }
    sources
}

fn canonical_workspace_root(project: &ProjectionProject) -> PathBuf {
    let root = PathBuf::from(&project.workspace_root);
    std::fs::canonicalize(&root).unwrap_or(root)
}

async fn scan(
    services: &Services,
    request: RpcRequest,
    cancellation: &CancellationToken,
) -> RpcResult {
    let input = decode::<ScanInput>(request)?;
    let project = live_project(services, &input.project_id).await?;
    let sources = resolve_sources(&services.settings_root).await;
    let imported = services
        .orchestration
        .repositories()
        .list_threads_by_project(project.project_id.clone())
        .await
        .map_err(|_| error("The project's threads could not be read. Try again."))?
        .into_iter()
        .map(|thread| (thread.thread_id, thread.deleted_at.is_none()))
        .collect::<HashMap<_, _>>();
    let instances = sources
        .iter()
        .map(|resolved| (resolved.source.provider, resolved.instance_id))
        .collect::<HashMap<_, _>>();
    let session_sources = sources
        .into_iter()
        .map(|resolved| resolved.source)
        .collect::<Vec<_>>();
    let owned = sessions_owned_by_other_threads(services).await?;
    // An interrupted RPC drops this future, so the blocking scan watches the token itself.
    let scan_cancellation = cancellation.clone();
    let result = tokio::task::spawn_blocking(move || {
        let workspace_root = canonical_workspace_root(&project);
        agent_sessions::scan(
            &session_sources,
            &workspace_root,
            SystemTime::now(),
            &owned,
            &scan_cancellation,
        )
    })
    .await
    .map_err(|_| error("The CLI session scan stopped unexpectedly. Try again."))?;
    let repositories = services.orchestration.repositories();
    let mut candidates = Vec::with_capacity(result.candidates.len());
    for candidate in result.candidates {
        let id = thread_id(
            instances[&candidate.provider],
            &candidate.session.session_id,
        );
        // A deleted import cannot be imported again. A live thread whose history never
        // committed (an interrupted import) stays selectable so the import can finish.
        let (already_imported, thread_id) = match imported.get(&id) {
            None => (false, None),
            Some(false) => (true, None),
            Some(true) => {
                let completed = history_imported(&repositories, &id)
                    .await
                    .map_err(|_| error("The project's threads could not be read. Try again."))?;
                (completed, completed.then_some(id))
            }
        };
        candidates.push(Candidate {
            provider: candidate.provider,
            session_id: candidate.session.session_id,
            title: candidate.session.title,
            message_count: candidate.session.message_count,
            last_active_at: candidate.last_active_at,
            already_imported,
            thread_id,
        });
    }
    Ok(json!({ "candidates": candidates, "truncated": result.truncated }))
}

async fn import(
    services: &Services,
    request: RpcRequest,
    cancellation: &CancellationToken,
) -> RpcResult {
    let input = decode::<ImportInput>(request)?;
    if input.sessions.is_empty() || input.sessions.len() > agent_sessions::MAX_CANDIDATES {
        return Err(error(format!(
            "Choose between 1 and {} sessions to import.",
            agent_sessions::MAX_CANDIDATES
        )));
    }
    let project = live_project(services, &input.project_id).await?;
    let sources = resolve_sources(&services.settings_root).await;
    let settings = ProviderSettingsStore::new(&services.settings_root)
        .get_document()
        .await
        .unwrap_or(Value::Null);
    let workspace_root = canonical_workspace_root(&project);
    let owned = sessions_owned_by_other_threads(services).await?;
    let mut imported = Vec::new();
    let mut skipped = Vec::new();
    for session in input.sessions {
        if cancellation.is_cancelled() {
            return Err(error("The import was cancelled."));
        }
        let outcome = match sources
            .iter()
            .find(|resolved| resolved.source.provider == session.provider)
        {
            Some(_) if owned.contains(&session.session_id) => {
                Err("This session already belongs to a Coddy Studio thread.".to_owned())
            }
            Some(source) => {
                import_session(
                    services,
                    &project,
                    &workspace_root,
                    source,
                    &session.session_id,
                    &settings,
                    cancellation,
                )
                .await
            }
            None => Err(format!(
                "{} is unavailable on the server. Check Settings → Providers.",
                provider_label(session.provider)
            )),
        };
        match outcome {
            Ok(thread_id) => imported.push(Imported {
                session_id: session.session_id,
                thread_id,
            }),
            Err(reason) => skipped.push(Skipped {
                session_id: session.session_id,
                reason,
            }),
        }
    }
    Ok(json!({ "imported": imported, "skipped": skipped }))
}

fn provider_label(provider: AgentSessionProvider) -> &'static str {
    match provider {
        AgentSessionProvider::ClaudeAgent => "Claude Code",
        AgentSessionProvider::Codex => "Codex",
    }
}

/// Creates the thread, records the CLI session as its suspended runtime, then appends the
/// history. Deterministic command IDs make a retry after a partial failure replay the steps
/// that already committed; the accepted history command marks the import complete.
async fn import_session(
    services: &Services,
    project: &ProjectionProject,
    workspace_root: &Path,
    resolved: &ResolvedSource,
    session_id: &str,
    settings: &Value,
    cancellation: &CancellationToken,
) -> Result<String, String> {
    let thread_id = thread_id(resolved.instance_id, session_id);
    let create_command_id = format!("{thread_id}:create");
    let history_command_id = format!("{thread_id}:history");
    let repositories = services.orchestration.repositories();
    let unreadable = |_| "Coddy Studio could not read its threads. Try again.".to_owned();
    let existing = repositories
        .get_thread(thread_id.clone())
        .await
        .map_err(unreadable)?;
    if let Some(thread) = &existing {
        if thread.deleted_at.is_some() {
            return Err("This session was imported before and its thread was deleted.".to_owned());
        }
        if thread.project_id != project.project_id {
            return Err("This session was already imported into another project.".to_owned());
        }
    }
    if repositories
        .get_command_receipt(history_command_id.clone())
        .await
        .map_err(unreadable)?
        .is_some_and(|receipt| receipt.status == "accepted")
    {
        return Err("Already imported.".to_owned());
    }
    if existing.as_ref().is_some_and(|thread| {
        thread.latest_turn_id.is_some() || thread.latest_user_message_at.is_some()
    }) {
        return Err(USED_BEFORE_IMPORT.to_owned());
    }

    let source = resolved.source.clone();
    let requested = session_id.to_owned();
    let root = workspace_root.to_path_buf();
    let session = tokio::task::spawn_blocking(move || {
        agent_sessions::load(&source, &requested, &root, SystemTime::now())
    })
    .await
    .map_err(|_| "The session transcript could not be read.".to_owned())?
    .map_err(|failure| failure.reason().to_owned())?;

    let model_selection = import_model_selection(
        resolved.source.provider,
        resolved.instance_id,
        session.model.as_deref(),
        settings,
        project.default_model_selection.as_ref(),
    );
    let created_at = session
        .messages
        .first()
        .map(|message| message.created_at.clone())
        .unwrap_or_default();
    // An interrupted import already created its thread; finish linking and history.
    if existing.is_none() {
        dispatch(
            services,
            OrchestrationCommand::ThreadCreate {
                command_id: create_command_id,
                thread_id: thread_id.clone(),
                project_id: project.project_id.clone(),
                title: session.title.clone(),
                kind: None,
                host_thread_id: None,
                model_selection: model_selection.clone(),
                runtime_mode: IMPORTED_RUNTIME_MODE.to_owned(),
                interaction_mode: IMPORTED_INTERACTION_MODE.to_owned(),
                branch: None,
                worktree_path: None,
                created_at,
            },
            &thread_id,
            cancellation,
        )
        .await?;
    }
    write_suspended_runtime(
        services,
        &thread_id,
        resolved,
        &session,
        &model_selection,
        workspace_root,
    )
    .await?;
    dispatch(
        services,
        OrchestrationCommand::ThreadHistoryImport {
            command_id: history_command_id,
            thread_id: thread_id.clone(),
            messages: imported_messages(&thread_id, session),
        },
        &thread_id,
        cancellation,
    )
    .await?;
    Ok(thread_id)
}

/// The digest binds a step to its thread, so a retry replays it whatever the transcript now
/// says, while a different command reusing the ID conflicts.
async fn dispatch(
    services: &Services,
    command: OrchestrationCommand,
    thread_id: &str,
    cancellation: &CancellationToken,
) -> Result<(), String> {
    let command_id = command.command_id().to_owned();
    let payload_digest = canonical_command_digest(&json!({
        "type": command.command_type(),
        "threadId": thread_id,
    }))?;
    let claim = tokio::select! {
        biased;
        () = cancellation.cancelled() => return Err("The import was cancelled.".to_owned()),
        claim = services.orchestration.acquire_command_admission(&command_id) => {
            claim.map_err(|failure| failure.to_string())?
        }
    };
    services
        .orchestration
        .dispatch_with_admission_and_command_claim_until_handoff(
            command,
            CommandAdmission {
                payload_digest,
                attachment_refs: Vec::new(),
                provider_turn: None,
            },
            claim,
            cancellation,
        )
        .await
        .map(|_| ())
        .map_err(|failure| match failure {
            // Invariant details explain themselves; the wrapper names internal command types.
            crate::orchestration::OrchestrationError::Invariant { detail, .. } => detail,
            failure => failure.to_string(),
        })
}

/// The row a suspended session of this provider leaves, with the CLI session as its cursor,
/// so the thread's next turn resumes the CLI conversation.
async fn write_suspended_runtime(
    services: &Services,
    thread_id: &str,
    resolved: &ResolvedSource,
    session: &ParsedSession,
    model_selection: &Value,
    workspace_root: &Path,
) -> Result<(), String> {
    let provider = resolved.source.provider.kind();
    let (resume_cursor, runtime_payload) = match resolved.source.provider {
        // Only `sessionId`: the launch reads `threadId` first.
        AgentSessionProvider::ClaudeAgent => (
            json!({ "sessionId": session.session_id }),
            json!({ "transport": "stream-json" }),
        ),
        AgentSessionProvider::Codex => (
            json!({ "threadId": session.session_id }),
            json!({ "model": model_selection["model"], "cwd": workspace_root }),
        ),
    };
    let repositories = services.orchestration.repositories();
    const UNLINKED: &str = "The session could not be linked to its thread. Try again.";
    let unlinked = |_| UNLINKED.to_owned();
    let written = repositories
        .insert_provider_session_runtime_if_absent_for_live_thread(ProviderSessionRuntime {
            thread_id: thread_id.to_owned(),
            provider_name: provider.to_owned(),
            provider_instance_id: Some(resolved.instance_id.to_owned()),
            adapter_key: native_adapter_key(provider).to_owned(),
            runtime_mode: IMPORTED_RUNTIME_MODE.to_owned(),
            status: "suspended".to_owned(),
            last_seen_at: super::control::now_iso(),
            resume_cursor: Some(resume_cursor.clone()),
            runtime_payload: Some(runtime_payload),
        })
        .await
        .map_err(unlinked)?;
    if written {
        return Ok(());
    }
    // A row from an earlier attempt of this import is kept; any other belongs to a
    // conversation the thread started itself.
    let current = repositories
        .get_provider_session_runtime(thread_id.to_owned())
        .await
        .map_err(unlinked)?;
    match current {
        Some(row)
            if row.resume_cursor.as_ref() == Some(&resume_cursor)
                && row.provider_instance_id.as_deref() == Some(resolved.instance_id) =>
        {
            Ok(())
        }
        Some(_) => Err(USED_BEFORE_IMPORT.to_owned()),
        None => {
            // A turn another client admitted during the import may not have launched yet.
            let used = repositories
                .get_thread(thread_id.to_owned())
                .await
                .map_err(unlinked)?
                .is_some_and(|thread| {
                    thread.latest_turn_id.is_some() || thread.latest_user_message_at.is_some()
                });
            Err(if used { USED_BEFORE_IMPORT } else { UNLINKED }.to_owned())
        }
    }
}

fn imported_messages(thread_id: &str, session: ParsedSession) -> Vec<ImportedThreadMessage> {
    debug_assert!(session.messages.len() <= MAX_IMPORTED_THREAD_MESSAGES);
    session
        .messages
        .into_iter()
        .enumerate()
        .map(|(index, message)| ImportedThreadMessage {
            message_id: format!("{thread_id}:{index:06}"),
            role: message.role.to_owned(),
            text: message.text,
            created_at: message.created_at,
        })
        .collect()
}

/// The transcript's model when the instance offers it, else the project's default for this
/// instance, the driver's session default, or the built-in default.
fn import_model_selection(
    provider: AgentSessionProvider,
    instance_id: &str,
    transcript_model: Option<&str>,
    settings: &Value,
    project_default: Option<&Value>,
) -> Value {
    let transcript_model = transcript_model.and_then(|model| match provider {
        // Codex reports the model it ran, which it accepts again.
        AgentSessionProvider::Codex => Some(model.to_owned()),
        AgentSessionProvider::ClaudeAgent => {
            known_claude_model(model, &instance_custom_models(settings, instance_id))
        }
    });
    let model = transcript_model
        .or_else(|| {
            project_default
                .filter(|selection| selection["instanceId"] == instance_id)
                .and_then(|selection| selection["model"].as_str())
                .map(str::to_owned)
        })
        .or_else(|| {
            settings["providerSessionDefaults"][provider.kind()]["model"]
                .as_str()
                .map(str::trim)
                .filter(|model| !model.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| {
            match provider {
                AgentSessionProvider::ClaudeAgent => CLAUDE_DEFAULT_MODEL,
                AgentSessionProvider::Codex => CODEX_DEFAULT_MODEL,
            }
            .to_owned()
        });
    json!({ "instanceId": instance_id, "model": model })
}

/// Claude records dated API IDs (`claude-opus-4-5-20251101`); the catalog lists undated slugs.
fn known_claude_model(model: &str, custom_models: &[String]) -> Option<String> {
    let undated = model
        .rsplit_once('-')
        .filter(|(_, date)| date.len() == 8 && date.bytes().all(|byte| byte.is_ascii_digit()))
        .map(|(slug, _)| slug);
    let catalog = claude::model::all_models(custom_models);
    [Some(model), undated]
        .into_iter()
        .flatten()
        .find(|candidate| {
            catalog
                .iter()
                .any(|entry| entry["slug"].as_str() == Some(candidate))
        })
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{
        orchestration::EngineOptions,
        persistence::{Database, run_migrations},
        production::provider_runtime::launch_request_for_command,
    };

    /// The row an import writes is the one the next turn's launch resumes from.
    #[tokio::test]
    async fn the_next_turn_resumes_the_imported_cli_session() {
        let temp = tempfile::tempdir().unwrap();
        let database = Database::open_in_memory().await.unwrap();
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .unwrap();
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .unwrap();
        let services = Services {
            orchestration: engine.clone(),
            settings_root: temp.path().to_path_buf(),
        };
        engine
            .dispatch(
                serde_json::from_value(json!({
                    "type":"project.create","commandId":"project","projectId":"p",
                    "title":"P","workspaceRoot":temp.path(),"createdAt":"2026-10-01T00:00:00Z"
                }))
                .unwrap(),
            )
            .await
            .unwrap();
        for (provider, instance_id, session_id, cursor) in [
            (
                AgentSessionProvider::ClaudeAgent,
                "claudeAgent",
                "0f8b5c2e-4d6a-4b1c-9e3f-2a7d8c9b1e40",
                "sessionId",
            ),
            (
                AgentSessionProvider::Codex,
                "codex",
                "0199a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b",
                "threadId",
            ),
        ] {
            let thread_id = thread_id(instance_id, session_id);
            let selection = import_model_selection(provider, instance_id, None, &Value::Null, None);
            engine
                .dispatch(OrchestrationCommand::ThreadCreate {
                    command_id: format!("{thread_id}:create"),
                    thread_id: thread_id.clone(),
                    project_id: "p".to_owned(),
                    title: "Imported".to_owned(),
                    kind: None,
                    host_thread_id: None,
                    model_selection: selection.clone(),
                    runtime_mode: IMPORTED_RUNTIME_MODE.to_owned(),
                    interaction_mode: IMPORTED_INTERACTION_MODE.to_owned(),
                    branch: None,
                    worktree_path: None,
                    created_at: "2026-10-01T00:00:00Z".to_owned(),
                })
                .await
                .unwrap();
            let resolved = ResolvedSource {
                source: SessionSource {
                    provider,
                    home: temp.path().to_path_buf(),
                },
                instance_id,
            };
            let session = ParsedSession {
                session_id: session_id.to_owned(),
                title: "Imported".to_owned(),
                model: None,
                message_count: 0,
                messages: Vec::new(),
            };
            write_suspended_runtime(
                &services,
                &thread_id,
                &resolved,
                &session,
                &selection,
                temp.path(),
            )
            .await
            .unwrap();
            let turn = serde_json::from_value(json!({
                "type":"thread.turn.start","commandId":format!("{thread_id}:turn"),
                "threadId":thread_id,
                "message":{"messageId":"m","role":"user","text":"continue","attachments":[]},
                "createdAt":"2026-10-02T00:00:00Z"
            }))
            .unwrap();
            let launch = launch_request_for_command(&engine, &services.settings_root, &turn, None)
                .await
                .unwrap();
            assert_eq!(launch.provider, provider.kind());
            assert_eq!(launch.resume_cursor, Some(json!({ cursor: session_id })));
        }
        engine.shutdown().await;
    }

    /// Without `homePath` or `CODEX_HOME`, Codex keeps its sessions under the HOME its instance
    /// sets, not the server's.
    #[tokio::test]
    async fn codex_sessions_are_scanned_in_the_instance_home() {
        if std::env::var_os("CODEX_HOME").is_some() {
            // The server's CODEX_HOME decides for the instance too.
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("instance-home");
        std::fs::write(
            temp.path().join("settings.json"),
            json!({"providerInstances":{"codex":{
                "driver":"codex","environment":[{"name":"HOME","value":home}]
            }}})
            .to_string(),
        )
        .unwrap();
        let sources = resolve_sources(&temp.path().to_path_buf()).await;
        let codex = sources
            .iter()
            .find(|resolved| resolved.source.provider == AgentSessionProvider::Codex)
            .expect("codex source");
        assert_eq!(codex.source.home, home.join(".codex"));
    }

    /// A turn another client admitted while the transcript was read keeps its own conversation.
    #[tokio::test]
    async fn linking_refuses_a_thread_that_started_its_own_turn() {
        let temp = tempfile::tempdir().unwrap();
        let database = Database::open_in_memory().await.unwrap();
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .unwrap();
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .unwrap();
        let services = Services {
            orchestration: engine.clone(),
            settings_root: temp.path().to_path_buf(),
        };
        let session_id = "0199a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b";
        let thread_id = thread_id("codex", session_id);
        let selection = import_model_selection(
            AgentSessionProvider::Codex,
            "codex",
            None,
            &Value::Null,
            None,
        );
        for value in [
            json!({"type":"project.create","commandId":"project","projectId":"p","title":"P","workspaceRoot":temp.path(),"createdAt":"2026-10-01T00:00:00Z"}),
            json!({"type":"thread.create","commandId":format!("{thread_id}:create"),"threadId":thread_id,"projectId":"p","title":"Imported","modelSelection":selection,"runtimeMode":IMPORTED_RUNTIME_MODE,"createdAt":"2026-10-01T00:00:00Z"}),
            json!({"type":"thread.turn.start","commandId":"other-client-turn","threadId":thread_id,"message":{"messageId":"m","role":"user","text":"hello","attachments":[]},"createdAt":"2026-10-02T00:00:00Z"}),
        ] {
            engine
                .dispatch(serde_json::from_value(value).unwrap())
                .await
                .unwrap();
        }
        let resolved = ResolvedSource {
            source: SessionSource {
                provider: AgentSessionProvider::Codex,
                home: temp.path().to_path_buf(),
            },
            instance_id: "codex",
        };
        let session = ParsedSession {
            session_id: session_id.to_owned(),
            title: "Imported".to_owned(),
            model: None,
            message_count: 0,
            messages: Vec::new(),
        };
        assert_eq!(
            write_suspended_runtime(
                &services,
                &thread_id,
                &resolved,
                &session,
                &selection,
                temp.path(),
            )
            .await,
            Err(USED_BEFORE_IMPORT.to_owned())
        );
        assert!(
            engine
                .repositories()
                .get_provider_session_runtime(thread_id)
                .await
                .unwrap()
                .is_none()
        );
        engine.shutdown().await;
    }

    #[test]
    fn imported_threads_keep_a_model_the_instance_offers() {
        let settings = json!({
            "providerSessionDefaults": { "claudeAgent": { "model": "claude-opus-4-8" } },
            "providerInstances": {
                "claudeAgent": { "driver": "claudeAgent", "config": { "customModels": ["house-model"] } }
            }
        });
        let select = |model: Option<&str>, project: Option<&Value>| {
            import_model_selection(
                AgentSessionProvider::ClaudeAgent,
                "claudeAgent",
                model,
                &settings,
                project,
            )["model"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        assert_eq!(
            select(Some("claude-opus-4-5-20251101"), None),
            "claude-opus-4-5"
        );
        assert_eq!(select(Some("house-model"), None), "house-model");
        assert_eq!(select(Some("retired-model"), None), "claude-opus-4-8");
        let project = json!({ "instanceId": "claudeAgent", "model": "claude-haiku-4-5" });
        assert_eq!(select(None, Some(&project)), "claude-haiku-4-5");
        let other = json!({ "instanceId": "codex", "model": "gpt-5.4" });
        assert_eq!(select(None, Some(&other)), "claude-opus-4-8");
        assert_eq!(
            import_model_selection(
                AgentSessionProvider::Codex,
                "codex",
                None,
                &Value::Null,
                None
            ),
            json!({ "instanceId": "codex", "model": CODEX_DEFAULT_MODEL })
        );
    }
}
