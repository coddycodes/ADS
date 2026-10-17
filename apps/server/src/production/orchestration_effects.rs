use std::{
    collections::BTreeMap,
    ffi::OsString,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::{
    sync::{broadcast, mpsc},
    task::JoinHandle,
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use uuid::Uuid;

use crate::{
    git::{
        CreateWorktreeInput, GitRepository, OutputPolicy, ProcessError, ProcessRequest,
        ProcessRunner,
    },
    orchestration::engine::{
        ActivityInput, BootstrapSetupInput, BootstrapSetupResult, BootstrapWorktree,
        BoxBootstrapFuture, BoxProjectCommandFuture, OrchestrationCommand, OrchestrationEngine,
        ProjectCommandEffects, ThreadTurnBootstrapEffects, ThreadTurnStartBootstrapPrepareWorktree,
    },
    persistence::{OrchestrationEvent, PersistenceError},
};

pub use super::host_paths::process_compatible_path;
use super::host_paths::{
    HostPathError, normalize_host_path_lexically, resolve_host_directory,
    resolve_host_directory_identity,
};
use super::repository_identity::{backfill_repository_identities, reconcile_repository_identity};

const GIT_TIMEOUT: Duration = Duration::from_secs(30);
const GIT_OUTPUT_LIMIT: usize = 8 * 1024 * 1024;

pub type BoxEffectFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetupScriptLaunch {
    pub thread_id: String,
    pub terminal_id: String,
    pub script_id: String,
    pub script_name: String,
    pub command: String,
    pub cwd: PathBuf,
    pub worktree_path: PathBuf,
    pub env: BTreeMap<String, String>,
}

/// What the provider did when a checkpoint revert asked it to drop the newest turns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderRewind {
    /// The provider dropped the turns from its conversation.
    Rewound,
    /// The provider cannot rewind its conversation, so the revert restores only the files.
    FilesOnly { provider_label: String },
}

pub trait OrchestrationEffectCallbacks: Send + Sync {
    fn workspace_for_thread<'a>(
        &'a self,
        thread_id: &'a str,
    ) -> BoxEffectFuture<'a, Option<PathBuf>>;

    /// `rewind_key` identifies the revert by the transcript it starts from, so a retried rewind
    /// is applied once.
    fn rollback_provider<'a>(
        &'a self,
        thread_id: &'a str,
        turns: i64,
        rewind_key: &'a str,
    ) -> BoxEffectFuture<'a, ProviderRewind>;

    fn stop_provider<'a>(&'a self, thread_id: &'a str) -> BoxEffectFuture<'a, ()>;

    fn close_terminals<'a>(&'a self, thread_id: &'a str) -> BoxEffectFuture<'a, ()>;

    fn refresh_workspace<'a>(&'a self, cwd: &'a Path) -> BoxEffectFuture<'a, ()>;

    /// Closes the thread's preview tabs and gateway targets.
    fn close_previews<'a>(&'a self, _thread_id: &'a str) -> BoxEffectFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }

    fn setup_script_is_running<'a>(
        &'a self,
        _thread_id: &'a str,
        _terminal_id: &'a str,
    ) -> BoxEffectFuture<'a, bool> {
        Box::pin(async { Ok(false) })
    }

    fn launch_setup_script<'a>(&'a self, _input: SetupScriptLaunch) -> BoxEffectFuture<'a, ()> {
        Box::pin(async {
            Err(
                "the production terminal callback is unavailable for setup script launch"
                    .to_owned(),
            )
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct EffectsOptions {
    pub queue_capacity: usize,
}

impl Default for EffectsOptions {
    fn default() -> Self {
        Self { queue_capacity: 64 }
    }
}

#[derive(Debug, Error)]
pub enum OrchestrationEffectsError {
    #[error("workspace root does not exist: {0}")]
    WorkspaceMissing(PathBuf),
    #[error("workspace root is not a directory: {0}")]
    WorkspaceNotDirectory(PathBuf),
    #[error("failed to access workspace root {path}: {source}")]
    WorkspaceIo {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("native git operation failed: {0}")]
    Git(#[from] ProcessError),
    #[error("orchestration persistence failed: {0}")]
    Persistence(#[from] PersistenceError),
    #[error("orchestration effect failed: {0}")]
    Effect(String),
}

pub async fn normalize_project_workspace_root(
    workspace_root: &Path,
    create_if_missing: bool,
) -> Result<PathBuf, OrchestrationEffectsError> {
    resolve_host_directory(workspace_root, create_if_missing)
        .await
        .map_err(map_host_path_error)
}

async fn canonicalize_project_workspace_root(
    workspace_root: &Path,
    allow_missing: bool,
) -> Result<PathBuf, OrchestrationEffectsError> {
    resolve_host_directory_identity(workspace_root, allow_missing)
        .await
        .map_err(map_host_path_error)
}

fn map_host_path_error(error: HostPathError) -> OrchestrationEffectsError {
    match error {
        HostPathError::Missing(path) => OrchestrationEffectsError::WorkspaceMissing(path),
        HostPathError::NotDirectory(path) => OrchestrationEffectsError::WorkspaceNotDirectory(path),
        HostPathError::HomeDirectoryUnavailable(path) => OrchestrationEffectsError::WorkspaceIo {
            path,
            source: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "home directory is unavailable",
            ),
        },
        HostPathError::Io { path, source } => {
            OrchestrationEffectsError::WorkspaceIo { path, source }
        }
    }
}

#[derive(Default)]
struct ProductionProjectCommandEffects;

impl ProjectCommandEffects for ProductionProjectCommandEffects {
    fn normalize_workspace_root_lexically(&self, workspace_root: &str) -> String {
        normalize_host_path_lexically(Path::new(workspace_root))
            .to_string_lossy()
            .into_owned()
    }

    fn canonicalize_workspace_root<'a>(
        &'a self,
        workspace_root: &'a str,
        allow_missing: bool,
    ) -> BoxProjectCommandFuture<'a, String> {
        Box::pin(async move {
            canonicalize_project_workspace_root(Path::new(workspace_root), allow_missing)
                .await
                .map(|path| path.to_string_lossy().into_owned())
                .map_err(|error| error.to_string())
        })
    }

    fn prepare_project_create<'a>(
        &'a self,
        workspace_root: &'a str,
        create_if_missing: bool,
        initialize_git: bool,
    ) -> BoxProjectCommandFuture<'a, ()> {
        Box::pin(async move {
            let normalized =
                normalize_project_workspace_root(Path::new(workspace_root), create_if_missing)
                    .await
                    .map_err(|error| error.to_string())?;
            let normalized_display = normalized.to_string_lossy();
            if normalized_display != workspace_root {
                return Err(format!(
                    "workspace root changed while preparing project creation: expected {workspace_root}, resolved {normalized_display}"
                ));
            }
            if initialize_git {
                GitRepository::default()
                    .init(&normalized, &CancellationToken::new())
                    .await
                    .map_err(|error| error.to_string())?;
            }
            Ok(())
        })
    }
}

pub fn install_project_command_effects(engine: &OrchestrationEngine) {
    engine.set_project_command_effects(Arc::new(ProductionProjectCommandEffects));
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectSetupScript {
    id: String,
    name: String,
    command: String,
    run_on_worktree_create: bool,
}

struct ProductionBootstrapEffects {
    repositories: crate::persistence::Repositories,
    repository: Arc<GitRepository>,
    callbacks: Arc<dyn OrchestrationEffectCallbacks>,
}

impl ThreadTurnBootstrapEffects for ProductionBootstrapEffects {
    fn prepare_worktree<'a>(
        &'a self,
        input: ThreadTurnStartBootstrapPrepareWorktree,
        cancellation: &'a CancellationToken,
    ) -> BoxBootstrapFuture<'a, BootstrapWorktree> {
        Box::pin(async move {
            let cwd = PathBuf::from(&input.project_cwd);
            let remove_branch = input.branch.is_some();
            let ref_name = if input.start_from_origin == Some(true) {
                fetch_origin_base(&cwd, &input.base_branch, cancellation).await?
            } else {
                input.base_branch.clone()
            };
            let result = self
                .repository
                .ensure_worktree(
                    CreateWorktreeInput {
                        cwd,
                        ref_name,
                        new_ref_name: input.branch,
                        base_ref_name: None,
                        path: None,
                    },
                    cancellation,
                )
                .await
                .map_err(|error| error.to_string())?;
            let path = PathBuf::from(&result.worktree.path);
            let worktree = BootstrapWorktree {
                repository_root: input.project_cwd,
                branch: result.worktree.ref_name,
                path: result.worktree.path,
                remove_branch,
            };
            if let Err(error) = self.callbacks.refresh_workspace(&path).await {
                return Err(format!("worktree workspace refresh failed: {error}"));
            }
            Ok(worktree)
        })
    }

    fn run_setup_script<'a>(
        &'a self,
        input: BootstrapSetupInput,
    ) -> BoxBootstrapFuture<'a, BootstrapSetupResult> {
        Box::pin(async move {
            let project = if let Some(project_id) = input.project_id {
                self.repositories
                    .get_project(project_id)
                    .await
                    .map_err(|error| error.to_string())?
            } else if let Some(project_cwd) = input.project_cwd {
                self.repositories
                    .list_projects()
                    .await
                    .map_err(|error| error.to_string())?
                    .into_iter()
                    .find(|project| project.workspace_root == project_cwd)
            } else {
                None
            }
            .ok_or_else(|| "project was not found for setup script execution".to_owned())?;
            let scripts: Vec<ProjectSetupScript> = serde_json::from_value(project.scripts)
                .map_err(|error| format!("project setup scripts are invalid: {error}"))?;
            let Some(script) = scripts
                .into_iter()
                .find(|script| script.run_on_worktree_create)
            else {
                return Ok(BootstrapSetupResult::NoScript);
            };
            let terminal_id = format!("setup-{}", script.id);
            if self
                .callbacks
                .setup_script_is_running(&input.thread_id, &terminal_id)
                .await?
            {
                return Ok(BootstrapSetupResult::Started {
                    script_id: script.id,
                    script_name: script.name,
                    terminal_id,
                });
            }
            let worktree_path = PathBuf::from(&input.worktree_path);
            let env = BTreeMap::from([
                ("BIBCODE_PROJECT_ROOT".to_owned(), project.workspace_root),
                (
                    "BIBCODE_WORKTREE_PATH".to_owned(),
                    input.worktree_path.clone(),
                ),
            ]);
            self.callbacks
                .launch_setup_script(SetupScriptLaunch {
                    thread_id: input.thread_id,
                    terminal_id: terminal_id.clone(),
                    script_id: script.id.clone(),
                    script_name: script.name.clone(),
                    command: script.command,
                    cwd: worktree_path.clone(),
                    worktree_path,
                    env,
                })
                .await?;
            Ok(BootstrapSetupResult::Started {
                script_id: script.id,
                script_name: script.name,
                terminal_id,
            })
        })
    }
}

async fn fetch_origin_base(
    cwd: &Path,
    base_branch: &str,
    cancellation: &CancellationToken,
) -> Result<String, String> {
    run_bootstrap_git(cwd, ["fetch", "origin"], cancellation).await?;
    let branch = base_branch.strip_prefix("origin/").unwrap_or(base_branch);
    let remote_ref = format!("refs/remotes/origin/{branch}");
    let output =
        run_bootstrap_git(cwd, ["rev-parse", "--verify", &remote_ref], cancellation).await?;
    let commit = output.trim();
    if commit.is_empty() {
        return Err(format!(
            "origin branch '{base_branch}' resolved to an empty commit"
        ));
    }
    Ok(commit.to_owned())
}

async fn run_bootstrap_git<const N: usize>(
    cwd: &Path,
    args: [&str; N],
    cancellation: &CancellationToken,
) -> Result<String, String> {
    ProcessRunner
        .run(
            ProcessRequest {
                operation: format!("bootstrap.git.{}", args[0]),
                command: PathBuf::from("git"),
                args: args.into_iter().map(OsString::from).collect(),
                cwd: cwd.to_path_buf(),
                env: Vec::new(),
                stdin: None,
                timeout: GIT_TIMEOUT,
                max_output_bytes: GIT_OUTPUT_LIMIT,
                output_policy: OutputPolicy::Error,
                append_truncation_marker: false,
                allow_non_zero_exit: false,
            },
            cancellation,
        )
        .await
        .map(|output| output.stdout)
        .map_err(bootstrap_process_error)
}

fn bootstrap_process_error(error: ProcessError) -> String {
    let detail = match &error {
        ProcessError::NonZeroExit { stderr, stdout, .. } => [stderr.as_ref(), stdout.as_ref()]
            .into_iter()
            .map(str::trim)
            .find(|value| !value.is_empty()),
        _ => None,
    };
    let summary = detail.map_or_else(|| error.to_string(), |detail| format!("{error}: {detail}"));
    crate::diagnostics::redact_sensitive_text(&summary)
}

pub struct OrchestrationEffects {
    cancellation: CancellationToken,
    /// Identity reconciles run off the sequential worker so a slow `git` read never delays turn
    /// baselines; shutdown cancels and drains them.
    identity_tasks: TaskTracker,
    producer: tokio::sync::Mutex<Option<JoinHandle<()>>>,
    worker: tokio::sync::Mutex<Option<JoinHandle<()>>>,
}

impl OrchestrationEffects {
    pub async fn start(
        engine: OrchestrationEngine,
        repository: Arc<GitRepository>,
        callbacks: Arc<dyn OrchestrationEffectCallbacks>,
        options: EffectsOptions,
    ) -> Result<Self, OrchestrationEffectsError> {
        // Read the log head before subscribing: every later commit then reaches the subscription
        // or lies after this floor, where the producer reads it from the log, while history at or
        // below the floor is never replayed.
        let floor = engine.repositories().max_event_sequence().await?;
        let subscription = engine.subscribe_events();
        let cancellation = CancellationToken::new();
        let (sender, receiver) = mpsc::channel(options.queue_capacity.max(1));

        install_project_command_effects(&engine);
        engine.set_bootstrap_effects(Arc::new(ProductionBootstrapEffects {
            repositories: engine.repositories(),
            repository: repository.clone(),
            callbacks: callbacks.clone(),
        }));

        let identity_tasks = TaskTracker::new();
        identity_tasks.spawn(backfill_repository_identities(
            engine.clone(),
            repository.clone(),
            cancellation.child_token(),
        ));
        let worker = tokio::spawn(run_worker(
            engine.clone(),
            repository,
            identity_tasks.clone(),
            callbacks,
            receiver,
            cancellation.clone(),
        ));
        let producer = tokio::spawn(run_producer(
            engine,
            sender,
            cancellation.clone(),
            subscription,
            floor,
        ));

        Ok(Self {
            cancellation,
            identity_tasks,
            producer: tokio::sync::Mutex::new(Some(producer)),
            worker: tokio::sync::Mutex::new(Some(worker)),
        })
    }

    pub async fn shutdown(&self) {
        self.cancellation.cancel();
        if let Some(producer) = self.producer.lock().await.take() {
            let _ = producer.await;
        }
        if let Some(worker) = self.worker.lock().await.take() {
            let _ = worker.await;
        }
        self.identity_tasks.close();
        self.identity_tasks.wait().await;
    }
}

async fn run_producer(
    engine: OrchestrationEngine,
    sender: mpsc::Sender<OrchestrationEvent>,
    cancellation: CancellationToken,
    subscription: broadcast::Receiver<OrchestrationEvent>,
    floor: i64,
) {
    // The highest log position already handled. Anything at or below it is history or was
    // already forwarded from the log, so it is skipped rather than re-running its side effect.
    let mut handled_through = floor;
    // Commits between reading the floor and subscribing never reach the subscription.
    if !forward_logged_events(&engine, &sender, &mut handled_through, LogRead::Startup).await {
        return;
    }
    forward_subscription(
        &engine,
        &sender,
        &cancellation,
        subscription,
        handled_through,
    )
    .await;
}

/// Forwards what the subscription delivers after `handled_through`, and reads the log to fill the
/// gap whenever the subscription lags.
async fn forward_subscription(
    engine: &OrchestrationEngine,
    sender: &mpsc::Sender<OrchestrationEvent>,
    cancellation: &CancellationToken,
    mut subscription: broadcast::Receiver<OrchestrationEvent>,
    mut handled_through: i64,
) {
    loop {
        tokio::select! {
            () = cancellation.cancelled() => return,
            received = subscription.recv() => match received {
                Ok(event) => {
                    if event.sequence <= handled_through {
                        continue;
                    }
                    handled_through = event.sequence;
                    if is_reactor_event(&event)
                        && sender.send(event).await.is_err()
                    {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    if !forward_logged_events(engine, sender, &mut handled_through, LogRead::LagRecovery)
                        .await
                    {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    }
}

/// Why the producer reads the event log instead of the subscription.
#[derive(Clone, Copy, Debug)]
enum LogRead {
    /// Once at startup, for commits made before the subscription existed.
    Startup,
    /// After the subscription lagged and dropped events.
    LagRecovery,
}

/// Forwards the reactor events logged after `handled_through`, one bounded page at a time.
/// Returns `false` once the worker has stopped.
async fn forward_logged_events(
    engine: &OrchestrationEngine,
    sender: &mpsc::Sender<OrchestrationEvent>,
    handled_through: &mut i64,
    read: LogRead,
) -> bool {
    let repositories = engine.repositories();
    let mut pages = repositories.event_pages(*handled_through);
    loop {
        let page = match pages.next_page().await {
            Ok(Some(page)) => page,
            Ok(None) => return true,
            Err(error) => {
                match read {
                    LogRead::Startup => tracing::warn!(
                        %error,
                        "failed to read orchestration effect events committed before the effects subscription"
                    ),
                    LogRead::LagRecovery => tracing::warn!(
                        %error,
                        "failed to recover orchestration effect events after receiver lag"
                    ),
                }
                return true;
            }
        };
        for event in page {
            *handled_through = event.sequence;
            if is_reactor_event(&event) && sender.send(event).await.is_err() {
                return false;
            }
        }
    }
}

fn is_reactor_event(event: &OrchestrationEvent) -> bool {
    matches!(
        event.event.event_type.as_str(),
        "thread.turn-start-requested"
            | "thread.message-sent"
            | "thread.turn-diff-completed"
            | "thread.checkpoint-revert-requested"
            | "thread.deleted"
            | "project.created"
            | "project.meta-updated"
    )
}

async fn run_worker(
    engine: OrchestrationEngine,
    repository: Arc<GitRepository>,
    identity_tasks: TaskTracker,
    callbacks: Arc<dyn OrchestrationEffectCallbacks>,
    mut receiver: mpsc::Receiver<OrchestrationEvent>,
    cancellation: CancellationToken,
) {
    loop {
        tokio::select! {
            () = cancellation.cancelled() => return,
            event = receiver.recv() => {
                let Some(event) = event else { return };
                if let Err(error) = process_event(&engine, &repository, &identity_tasks, callbacks.as_ref(), &event, &cancellation).await {
                    tracing::warn!(event_type = %event.event.event_type, sequence = event.sequence, %error, "orchestration side effect failed");
                    append_failure_activity(&engine, &event, &error.to_string()).await;
                }
            }
        }
    }
}

async fn process_event(
    engine: &OrchestrationEngine,
    repository: &Arc<GitRepository>,
    identity_tasks: &TaskTracker,
    callbacks: &dyn OrchestrationEffectCallbacks,
    event: &OrchestrationEvent,
    cancellation: &CancellationToken,
) -> Result<(), OrchestrationEffectsError> {
    match event.event.event_type.as_str() {
        "thread.turn-start-requested" => {
            ensure_baseline(engine, callbacks, event, cancellation).await
        }
        "thread.message-sent" if needs_turn_baseline(event) => {
            ensure_baseline(engine, callbacks, event, cancellation).await
        }
        "thread.turn-diff-completed"
            if event.event.payload.get("status").and_then(Value::as_str) == Some("missing") =>
        {
            capture_missing_checkpoint(engine, callbacks, event, cancellation).await
        }
        "thread.checkpoint-revert-requested" => {
            revert_checkpoint(engine, callbacks, event, cancellation).await
        }
        "thread.deleted" => {
            cleanup_deleted_thread(callbacks, event).await;
            Ok(())
        }
        "project.created" => {
            reconcile_event_project(engine, repository, identity_tasks, event, cancellation);
            Ok(())
        }
        // Only a moved workspace changes the checkout; the identity update itself carries no
        // workspaceRoot, so this cannot loop.
        "project.meta-updated" if event.event.payload.get("workspaceRoot").is_some() => {
            reconcile_event_project(engine, repository, identity_tasks, event, cancellation);
            Ok(())
        }
        _ => Ok(()),
    }
}

fn reconcile_event_project(
    engine: &OrchestrationEngine,
    repository: &Arc<GitRepository>,
    identity_tasks: &TaskTracker,
    event: &OrchestrationEvent,
    cancellation: &CancellationToken,
) {
    let Some(project_id) = event.event.payload.get("projectId").and_then(Value::as_str) else {
        return;
    };
    let (engine, repository, project_id, cancellation) = (
        engine.clone(),
        repository.clone(),
        project_id.to_owned(),
        cancellation.child_token(),
    );
    identity_tasks.spawn(async move {
        reconcile_repository_identity(&engine, &repository, &project_id, &cancellation).await;
    });
}

async fn resolve_workspace(
    engine: &OrchestrationEngine,
    callbacks: &dyn OrchestrationEffectCallbacks,
    thread_id: &str,
) -> Result<Option<PathBuf>, OrchestrationEffectsError> {
    if let Some(cwd) = callbacks
        .workspace_for_thread(thread_id)
        .await
        .map_err(OrchestrationEffectsError::Effect)?
    {
        return Ok(Some(cwd));
    }
    let repositories = engine.repositories();
    let Some(thread) = repositories.get_thread(thread_id.to_owned()).await? else {
        return Ok(None);
    };
    if let Some(worktree_path) = thread.worktree_path {
        return Ok(Some(process_compatible_path(worktree_path.into())));
    }
    Ok(repositories
        .get_project(thread.project_id)
        .await?
        .map(|project| process_compatible_path(PathBuf::from(project.workspace_root))))
}

/// A user message that starts a turn needs the turn's checkpoint baseline. Imported CLI history
/// starts no turn, so it must not capture one (or spawn Git once per historical prompt).
fn needs_turn_baseline(event: &OrchestrationEvent) -> bool {
    let payload = &event.event.payload;
    payload.get("role").and_then(Value::as_str) == Some("user")
        && payload.get("streaming").and_then(Value::as_bool) == Some(false)
        && payload.get("turnId").is_none_or(Value::is_null)
        && event
            .event
            .metadata
            .get("historyImport")
            .and_then(Value::as_bool)
            != Some(true)
}

async fn ensure_baseline(
    engine: &OrchestrationEngine,
    callbacks: &dyn OrchestrationEffectCallbacks,
    event: &OrchestrationEvent,
    cancellation: &CancellationToken,
) -> Result<(), OrchestrationEffectsError> {
    let Some(thread_id) = event.event.payload.get("threadId").and_then(Value::as_str) else {
        return Ok(());
    };
    let Some(cwd) = resolve_workspace(engine, callbacks, thread_id).await? else {
        return Ok(());
    };
    if !is_git_repository(&cwd, Path::new("git"), cancellation).await? {
        return Ok(());
    }
    let turn_count = engine
        .repositories()
        .list_checkpoints_by_thread(thread_id.to_owned())
        .await?
        .into_iter()
        .map(|checkpoint| checkpoint.checkpoint_turn_count)
        .max()
        .unwrap_or(0);
    let reference = checkpoint_ref(thread_id, turn_count);
    if !has_ref(&cwd, Path::new("git"), &reference, cancellation).await? {
        capture_checkpoint_with_cancellation(
            &cwd,
            Path::new("git"),
            thread_id,
            turn_count,
            cancellation,
        )
        .await?;
    }
    Ok(())
}

async fn capture_missing_checkpoint(
    engine: &OrchestrationEngine,
    callbacks: &dyn OrchestrationEffectCallbacks,
    event: &OrchestrationEvent,
    cancellation: &CancellationToken,
) -> Result<(), OrchestrationEffectsError> {
    let payload = &event.event.payload;
    let Some(thread_id) = payload.get("threadId").and_then(Value::as_str) else {
        return Ok(());
    };
    let Some(turn_id) = payload.get("turnId").and_then(Value::as_str) else {
        return Ok(());
    };
    let Some(turn_count) = payload.get("checkpointTurnCount").and_then(Value::as_i64) else {
        return Ok(());
    };
    let Some(cwd) = resolve_workspace(engine, callbacks, thread_id).await? else {
        return Ok(());
    };
    if !is_git_repository(&cwd, Path::new("git"), cancellation).await? {
        return Ok(());
    }

    let existing = engine
        .repositories()
        .get_turn_by_id(thread_id.to_owned(), turn_id.to_owned())
        .await?;
    if existing
        .as_ref()
        .and_then(|turn| turn.checkpoint_status.as_deref())
        .is_some_and(|status| status != "missing")
    {
        return Ok(());
    }

    let target_ref = checkpoint_ref(thread_id, turn_count);
    capture_checkpoint_with_cancellation(
        &cwd,
        Path::new("git"),
        thread_id,
        turn_count,
        cancellation,
    )
    .await?;
    callbacks
        .refresh_workspace(&cwd)
        .await
        .map_err(OrchestrationEffectsError::Effect)?;

    let from_ref = checkpoint_ref(thread_id, turn_count.saturating_sub(1));
    let files = if has_ref(&cwd, Path::new("git"), &from_ref, cancellation).await? {
        diff_file_summaries(&cwd, Path::new("git"), &from_ref, &target_ref, cancellation).await?
    } else {
        Vec::new()
    };
    let completed_at = payload
        .get("completedAt")
        .and_then(Value::as_str)
        .unwrap_or(&event.event.occurred_at)
        .to_owned();
    let assistant_message_id = payload
        .get("assistantMessageId")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| Some(format!("assistant:{turn_id}")));

    engine
        .dispatch(OrchestrationCommand::ThreadTurnDiffComplete {
            command_id: server_id("checkpoint-turn-diff-complete"),
            thread_id: thread_id.to_owned(),
            turn_id: turn_id.to_owned(),
            checkpoint_turn_count: turn_count,
            checkpoint_ref: target_ref,
            status: "ready".to_owned(),
            files: Value::Array(files),
            assistant_message_id,
            completed_at: completed_at.clone(),
            created_at: completed_at.clone(),
        })
        .await
        .map_err(|error| OrchestrationEffectsError::Effect(error.to_string()))?;
    engine
        .dispatch(OrchestrationCommand::ThreadActivityAppend {
            command_id: server_id("checkpoint-captured-activity"),
            thread_id: thread_id.to_owned(),
            activity: ActivityInput {
                id: Uuid::new_v4().to_string(),
                tone: "info".to_owned(),
                kind: "checkpoint.captured".to_owned(),
                summary: "Checkpoint captured".to_owned(),
                payload: json!({"turnCount":turn_count,"status":"ready"}),
                turn_id: Some(turn_id.to_owned()),
                sequence: None,
                created_at: completed_at.clone(),
            },
            created_at: completed_at,
        })
        .await
        .map_err(|error| OrchestrationEffectsError::Effect(error.to_string()))?;
    Ok(())
}

async fn revert_checkpoint(
    engine: &OrchestrationEngine,
    callbacks: &dyn OrchestrationEffectCallbacks,
    event: &OrchestrationEvent,
    cancellation: &CancellationToken,
) -> Result<(), OrchestrationEffectsError> {
    let payload = &event.event.payload;
    let thread_id = required_str(payload, "threadId")?;
    let turn_count = payload
        .get("turnCount")
        .and_then(Value::as_i64)
        .ok_or_else(|| OrchestrationEffectsError::Effect("missing turnCount".to_owned()))?;
    // A running turn would write over the restored files or be cut off by the rewind, and the
    // client's own guard does not cover another client.
    if engine
        .repositories()
        .get_thread_session(thread_id.to_owned())
        .await?
        .is_some_and(|session| session.status == "running")
    {
        return Err(OrchestrationEffectsError::Effect(
            "Interrupt the current turn before reverting checkpoints.".to_owned(),
        ));
    }
    let cwd = resolve_workspace(engine, callbacks, thread_id)
        .await?
        .ok_or_else(|| {
            OrchestrationEffectsError::Effect(
                "No active provider session with workspace cwd is bound to this thread.".to_owned(),
            )
        })?;
    if !is_git_repository(&cwd, Path::new("git"), cancellation).await? {
        return Err(OrchestrationEffectsError::Effect(
            "Checkpoints are unavailable because this project is not a git repository.".to_owned(),
        ));
    }

    let checkpoints = engine
        .repositories()
        .list_checkpoints_by_thread(thread_id.to_owned())
        .await?;
    let current_turn_count = checkpoints
        .iter()
        .map(|checkpoint| checkpoint.checkpoint_turn_count)
        .max()
        .unwrap_or(0);
    if turn_count > current_turn_count {
        return Err(OrchestrationEffectsError::Effect(format!(
            "Checkpoint turn count {turn_count} exceeds current turn count {current_turn_count}."
        )));
    }
    let target_ref = if turn_count == 0 {
        checkpoint_ref(thread_id, 0)
    } else {
        checkpoints
            .iter()
            .find(|checkpoint| checkpoint.checkpoint_turn_count == turn_count)
            .map(|checkpoint| checkpoint.checkpoint_ref.clone())
            .ok_or_else(|| {
                OrchestrationEffectsError::Effect(format!(
                    "Checkpoint ref for turn {turn_count} is unavailable in read model."
                ))
            })?
    };
    // The provider rewind cannot be undone, so it runs before any file is touched and only once
    // the checkpoint is known to resolve. A refused rewind then changes nothing.
    let missing_checkpoint = || {
        OrchestrationEffectsError::Effect(format!(
            "Filesystem checkpoint is unavailable for turn {turn_count}."
        ))
    };
    if checkpoint_revision(
        &cwd,
        Path::new("git"),
        &target_ref,
        turn_count == 0,
        cancellation,
    )
    .await?
    .is_none()
    {
        return Err(missing_checkpoint());
    }
    let rolled_back_turns = current_turn_count.saturating_sub(turn_count);
    // The rewind's idempotency key: the transcript it counts from, named by its newest turn. A
    // retry from the same transcript (its earlier completion was not recorded) reuses it; once
    // the revert is recorded the newest turn changes, and so does the key.
    let rewind_key = checkpoints
        .iter()
        .max_by_key(|checkpoint| checkpoint.checkpoint_turn_count)
        .map(|checkpoint| format!("{thread_id}/{}", checkpoint.turn_id))
        .unwrap_or_else(|| format!("{thread_id}/{current_turn_count}"));
    if rolled_back_turns > 0
        && let ProviderRewind::FilesOnly { provider_label } = callbacks
            .rollback_provider(thread_id, rolled_back_turns, &rewind_key)
            .await
            .map_err(OrchestrationEffectsError::Effect)?
    {
        // The provider keeps its conversation, so the thread keeps its messages, checkpoints and
        // turn count too. Only the files go back, and the work log says so.
        if !restore_checkpoint(
            &cwd,
            Path::new("git"),
            &target_ref,
            turn_count == 0,
            cancellation,
        )
        .await?
        {
            return Err(missing_checkpoint());
        }
        callbacks
            .refresh_workspace(&cwd)
            .await
            .map_err(OrchestrationEffectsError::Effect)?;
        let created_at = now_iso();
        return engine
            .dispatch(OrchestrationCommand::ThreadActivityAppend {
                command_id: server_id("checkpoint-revert-files-only"),
                thread_id: thread_id.to_owned(),
                activity: ActivityInput {
                    id: Uuid::new_v4().to_string(),
                    tone: "info".to_owned(),
                    kind: "checkpoint.revert.files-only".to_owned(),
                    summary: "Files restored, conversation not rewound".to_owned(),
                    payload: json!({
                        "turnCount": turn_count,
                        "detail": format!(
                            "{provider_label} can't rewind its conversation, so it still has the later messages."
                        ),
                    }),
                    turn_id: None,
                    sequence: None,
                    created_at: created_at.clone(),
                },
                created_at,
            })
            .await
            .map(|_| ())
            .map_err(|error| OrchestrationEffectsError::Effect(error.to_string()));
    }
    // Once the provider has rewound, the revert must be recorded, or a retry would rewind the
    // provider a second time. Shutdown therefore waits for the restore instead of interrupting
    // it, and a restore that fails is still recorded.
    let uninterruptible = CancellationToken::new();
    let cancellation = if rolled_back_turns > 0 {
        &uninterruptible
    } else {
        cancellation
    };
    let restored = match restore_checkpoint(
        &cwd,
        Path::new("git"),
        &target_ref,
        turn_count == 0,
        cancellation,
    )
    .await
    {
        Ok(true) => Ok(()),
        Ok(false) => Err(missing_checkpoint()),
        Err(error) => Err(error),
    };
    if restored.is_ok() {
        callbacks
            .refresh_workspace(&cwd)
            .await
            .map_err(OrchestrationEffectsError::Effect)?;
    } else if rolled_back_turns == 0 {
        // Nothing was rewound, so the revert stays undone and can simply be retried.
        return restored;
    }
    for checkpoint in checkpoints
        .iter()
        .filter(|checkpoint| checkpoint.checkpoint_turn_count > turn_count)
    {
        delete_ref(
            &cwd,
            Path::new("git"),
            &checkpoint.checkpoint_ref,
            cancellation,
        )
        .await?;
    }
    let created_at = now_iso();
    engine
        .dispatch(OrchestrationCommand::ThreadRevertComplete {
            command_id: server_id("checkpoint-revert-complete"),
            thread_id: thread_id.to_owned(),
            turn_count,
            created_at,
        })
        .await
        .map_err(|error| OrchestrationEffectsError::Effect(error.to_string()))?;
    restored.map_err(|error| {
        OrchestrationEffectsError::Effect(format!(
            "The conversation was rewound, but the files could not be restored: {error}"
        ))
    })
}

async fn cleanup_deleted_thread(
    callbacks: &dyn OrchestrationEffectCallbacks,
    event: &OrchestrationEvent,
) {
    let Some(thread_id) = event.event.payload.get("threadId").and_then(Value::as_str) else {
        return;
    };
    if let Err(error) = callbacks.stop_provider(thread_id).await {
        tracing::debug!(thread_id, %error, "thread deletion cleanup skipped provider session stop");
    }
    if let Err(error) = callbacks.close_terminals(thread_id).await {
        tracing::debug!(thread_id, %error, "thread deletion cleanup skipped terminal close");
    }
    if let Err(error) = callbacks.close_previews(thread_id).await {
        tracing::debug!(thread_id, %error, "thread deletion cleanup skipped preview close");
    }
}

async fn append_failure_activity(
    engine: &OrchestrationEngine,
    event: &OrchestrationEvent,
    detail: &str,
) {
    let Some(thread_id) = event.event.payload.get("threadId").and_then(Value::as_str) else {
        return;
    };
    let (kind, summary, turn_id, payload) =
        if event.event.event_type == "thread.checkpoint-revert-requested" {
            (
                "checkpoint.revert.failed",
                "Checkpoint revert failed",
                None,
                json!({"turnCount":event.event.payload.get("turnCount"),"detail":detail}),
            )
        } else {
            (
                "checkpoint.capture.failed",
                "Checkpoint capture failed",
                event
                    .event
                    .payload
                    .get("turnId")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                json!({"detail":detail}),
            )
        };
    let created_at = now_iso();
    let _ = engine
        .dispatch(OrchestrationCommand::ThreadActivityAppend {
            command_id: server_id("checkpoint-effect-failure"),
            thread_id: thread_id.to_owned(),
            activity: ActivityInput {
                id: Uuid::new_v4().to_string(),
                tone: "error".to_owned(),
                kind: kind.to_owned(),
                summary: summary.to_owned(),
                payload,
                turn_id,
                sequence: None,
                created_at: created_at.clone(),
            },
            created_at,
        })
        .await;
}

pub fn checkpoint_ref(thread_id: &str, turn_count: i64) -> String {
    format!(
        "refs/bibcode/checkpoints/{}/turn/{turn_count}",
        URL_SAFE_NO_PAD.encode(thread_id)
    )
}

pub async fn capture_checkpoint(
    cwd: &Path,
    thread_id: &str,
    turn_count: i64,
) -> Result<(), OrchestrationEffectsError> {
    capture_checkpoint_with_cancellation(
        cwd,
        Path::new("git"),
        thread_id,
        turn_count,
        &CancellationToken::new(),
    )
    .await
}

async fn capture_checkpoint_with_cancellation(
    cwd: &Path,
    git_command: &Path,
    thread_id: &str,
    turn_count: i64,
    cancellation: &CancellationToken,
) -> Result<(), OrchestrationEffectsError> {
    let common_dir = run_git(
        cwd,
        git_command,
        &["rev-parse", "--git-common-dir"],
        &[],
        false,
        cancellation,
    )
    .await?
    .stdout;
    let common_dir = PathBuf::from(common_dir.trim());
    let common_dir = if common_dir.is_absolute() {
        common_dir
    } else {
        cwd.join(common_dir)
    };
    let temporary_index = common_dir.join(format!("bibcode-checkpoint-index-{}", Uuid::new_v4()));
    let reference = checkpoint_ref(thread_id, turn_count);
    let env = checkpoint_environment(&temporary_index);

    let result = async {
        let head = run_git(
            cwd,
            git_command,
            &["rev-parse", "--verify", "HEAD^{commit}"],
            &env,
            true,
            cancellation,
        )
        .await?;
        if head.exit_code == 0 {
            run_git(
                cwd,
                git_command,
                &["read-tree", "HEAD"],
                &env,
                false,
                cancellation,
            )
            .await?;
        }
        run_git(
            cwd,
            git_command,
            &["add", "-A", "--", "."],
            &env,
            false,
            cancellation,
        )
        .await?;
        let tree = run_git(cwd, git_command, &["write-tree"], &env, false, cancellation)
            .await?
            .stdout;
        let message = format!("bibcode checkpoint ref={reference}");
        let commit = run_git(
            cwd,
            git_command,
            &["commit-tree", tree.trim(), "-m", &message],
            &env,
            false,
            cancellation,
        )
        .await?
        .stdout;
        run_git(
            cwd,
            git_command,
            &["update-ref", &reference, commit.trim()],
            &[],
            false,
            cancellation,
        )
        .await?;
        Ok(())
    }
    .await;
    let _ = tokio::fs::remove_file(temporary_index).await;
    result
}

async fn is_git_repository(
    cwd: &Path,
    git_command: &Path,
    cancellation: &CancellationToken,
) -> Result<bool, OrchestrationEffectsError> {
    let output = run_git(
        cwd,
        git_command,
        &["rev-parse", "--is-inside-work-tree"],
        &[],
        true,
        cancellation,
    )
    .await?;
    Ok(output.exit_code == 0 && output.stdout.trim() == "true")
}

async fn has_ref(
    cwd: &Path,
    git_command: &Path,
    reference: &str,
    cancellation: &CancellationToken,
) -> Result<bool, OrchestrationEffectsError> {
    Ok(run_git(
        cwd,
        git_command,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{reference}^{{commit}}"),
        ],
        &[],
        true,
        cancellation,
    )
    .await?
    .exit_code
        == 0)
}

/// The revision a checkpoint restores from: its ref, or `HEAD` when allowed and the ref is gone.
async fn checkpoint_revision(
    cwd: &Path,
    git_command: &Path,
    reference: &str,
    fallback_to_head: bool,
    cancellation: &CancellationToken,
) -> Result<Option<String>, OrchestrationEffectsError> {
    if has_ref(cwd, git_command, reference, cancellation).await? {
        return Ok(Some(reference.to_owned()));
    }
    let head_exists = fallback_to_head
        && run_git(
            cwd,
            git_command,
            &["rev-parse", "--verify", "HEAD^{commit}"],
            &[],
            true,
            cancellation,
        )
        .await?
        .exit_code
            == 0;
    Ok(head_exists.then(|| "HEAD".to_owned()))
}

async fn restore_checkpoint(
    cwd: &Path,
    git_command: &Path,
    reference: &str,
    fallback_to_head: bool,
    cancellation: &CancellationToken,
) -> Result<bool, OrchestrationEffectsError> {
    let Some(revision) =
        checkpoint_revision(cwd, git_command, reference, fallback_to_head, cancellation).await?
    else {
        return Ok(false);
    };
    run_git(
        cwd,
        git_command,
        &[
            "restore",
            "--source",
            &revision,
            "--worktree",
            "--staged",
            "--",
            ".",
        ],
        &[],
        false,
        cancellation,
    )
    .await?;
    run_git(
        cwd,
        git_command,
        &["clean", "-fd", "--", "."],
        &[],
        false,
        cancellation,
    )
    .await?;
    if run_git(
        cwd,
        git_command,
        &["rev-parse", "--verify", "HEAD^{commit}"],
        &[],
        true,
        cancellation,
    )
    .await?
    .exit_code
        == 0
    {
        run_git(
            cwd,
            git_command,
            &["reset", "--quiet", "--", "."],
            &[],
            false,
            cancellation,
        )
        .await?;
    }
    Ok(true)
}

async fn delete_ref(
    cwd: &Path,
    git_command: &Path,
    reference: &str,
    cancellation: &CancellationToken,
) -> Result<(), OrchestrationEffectsError> {
    run_git(
        cwd,
        git_command,
        &["update-ref", "-d", reference],
        &[],
        true,
        cancellation,
    )
    .await?;
    Ok(())
}

async fn diff_file_summaries(
    cwd: &Path,
    git_command: &Path,
    from_ref: &str,
    to_ref: &str,
    cancellation: &CancellationToken,
) -> Result<Vec<Value>, OrchestrationEffectsError> {
    let output = run_git(
        cwd,
        git_command,
        &[
            "diff",
            "--numstat",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            &format!("{from_ref}^{{commit}}"),
            &format!("{to_ref}^{{commit}}"),
        ],
        &[],
        false,
        cancellation,
    )
    .await?;
    let mut files = output
        .stdout
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\t');
            let additions = fields.next()?;
            let deletions = fields.next()?;
            let path = fields.next()?;
            Some(json!({
                "path": path,
                "kind": "modified",
                "additions": additions.parse::<i64>().unwrap_or(0),
                "deletions": deletions.parse::<i64>().unwrap_or(0),
            }))
        })
        .collect::<Vec<_>>();
    files.sort_by(|left, right| left["path"].as_str().cmp(&right["path"].as_str()));
    Ok(files)
}

async fn run_git(
    cwd: &Path,
    git_command: &Path,
    args: &[&str],
    env: &[(OsString, OsString)],
    allow_non_zero_exit: bool,
    cancellation: &CancellationToken,
) -> Result<crate::git::ProcessOutput, OrchestrationEffectsError> {
    ProcessRunner
        .run(
            ProcessRequest {
                operation: "OrchestrationEffects.git".to_owned(),
                command: git_command.to_path_buf(),
                args: args.iter().map(OsString::from).collect(),
                cwd: cwd.to_path_buf(),
                env: env.to_vec(),
                stdin: None,
                timeout: GIT_TIMEOUT,
                max_output_bytes: GIT_OUTPUT_LIMIT,
                output_policy: OutputPolicy::Error,
                append_truncation_marker: false,
                allow_non_zero_exit,
            },
            cancellation,
        )
        .await
        .map_err(Into::into)
}

fn checkpoint_environment(index: &Path) -> Vec<(OsString, OsString)> {
    [
        ("GIT_INDEX_FILE", index.to_string_lossy().as_ref()),
        ("GIT_AUTHOR_NAME", "BiBCode"),
        ("GIT_AUTHOR_EMAIL", "bibcode@users.noreply.github.com"),
        ("GIT_COMMITTER_NAME", "BiBCode"),
        ("GIT_COMMITTER_EMAIL", "bibcode@users.noreply.github.com"),
    ]
    .into_iter()
    .map(|(key, value)| (OsString::from(key), OsString::from(value)))
    .collect()
}

fn required_str<'a>(value: &'a Value, field: &str) -> Result<&'a str, OrchestrationEffectsError> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| OrchestrationEffectsError::Effect(format!("missing {field}")))
}

fn now_iso() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| OffsetDateTime::now_utc().unix_timestamp().to_string())
}

fn server_id(tag: &str) -> String {
    format!("server:{tag}:{}", Uuid::new_v4())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::orchestration::EngineOptions;
    use crate::persistence::{
        Database, NewOrchestrationEvent, ProjectionProject, Repositories, run_migrations,
    };
    use crate::test_support::TestSandbox;

    use super::*;

    /// An engine over an empty in-memory event log.
    async fn empty_event_log() -> OrchestrationEngine {
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
            .expect("engine")
    }

    #[tokio::test]
    async fn imported_history_captures_no_turn_baseline() {
        const AT: &str = "2026-10-08T00:00:00Z";
        let engine = empty_event_log().await;
        for value in [
            json!({"type":"project.create","commandId":"project","projectId":"p","title":"P","workspaceRoot":"C:/repo","createdAt":AT}),
            json!({"type":"thread.create","commandId":"thread","threadId":"t","projectId":"p","title":"T","modelSelection":{"instanceId":"codex","model":"gpt-5"},"runtimeMode":"full-access","createdAt":AT}),
            json!({"type":"thread.history.import","commandId":"history","threadId":"t","messages":[
                {"messageId":"h-0","role":"user","text":"old prompt","createdAt":AT},
                {"messageId":"h-1","role":"assistant","text":"old answer","createdAt":AT}
            ]}),
            json!({"type":"thread.turn.start","commandId":"turn","threadId":"t","message":{"messageId":"live","role":"user","text":"continue","attachments":[]},"createdAt":AT}),
        ] {
            engine
                .dispatch(serde_json::from_value(value).unwrap())
                .await
                .expect("command");
        }
        let baselines = engine
            .read_events(0)
            .await
            .expect("events")
            .into_iter()
            .filter(|event| event.event.event_type == "thread.message-sent")
            .map(|event| {
                (
                    event.event.payload["messageId"].clone(),
                    needs_turn_baseline(&event),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            baselines,
            [
                (json!("h-0"), false),
                (json!("h-1"), false),
                (json!("live"), true),
            ]
        );
        engine.shutdown().await;
    }

    async fn append_reactor_event(
        repositories: &Repositories,
        event_type: &'static str,
        index: usize,
    ) -> OrchestrationEvent {
        repositories
            .append_event(NewOrchestrationEvent {
                event_id: format!("effects-lag-{index}"),
                event_type: event_type.to_owned(),
                aggregate_kind: "thread".to_owned(),
                aggregate_id: format!("thread-{index}"),
                occurred_at: "2026-08-01T00:00:00Z".to_owned(),
                command_id: None,
                causation_event_id: None,
                correlation_id: None,
                payload: json!({ "threadId": format!("thread-{index}") }),
                metadata: json!({}),
            })
            .await
            .expect("event")
    }

    /// Runs the producer from `floor` over a subscription of `capacity` that received `broadcast`
    /// before its first receive and then closed. Returns the sequence of every forwarded event.
    async fn forwarded_sequences(
        engine: &OrchestrationEngine,
        floor: i64,
        broadcast: Vec<OrchestrationEvent>,
        capacity: usize,
    ) -> Vec<i64> {
        let (events, subscription) = broadcast::channel(capacity);
        for event in broadcast {
            let _ = events.send(event);
        }
        drop(events);
        let (sender, mut receiver) = mpsc::channel(1024);
        tokio::time::timeout(
            Duration::from_secs(10),
            run_producer(
                engine.clone(),
                sender,
                CancellationToken::new(),
                subscription,
                floor,
            ),
        )
        .await
        .expect("the producer stops when its subscription closes");
        let mut forwarded = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            forwarded.push(event.sequence);
        }
        forwarded
    }

    /// Events committed after the startup read, but dropped before the producer's first receive,
    /// are recovered from the log once each. The events the subscription still holds after that
    /// read are skipped instead of re-running side effects such as a checkpoint revert or a thread
    /// deletion, and history at or below the floor is never replayed.
    #[tokio::test]
    async fn lag_recovery_forwards_events_committed_after_the_startup_read_exactly_once() {
        let engine = empty_event_log().await;
        let repositories = engine.repositories();
        for index in 0..3 {
            append_reactor_event(&repositories, "thread.checkpoint-revert-requested", index).await;
        }
        let floor = repositories.max_event_sequence().await.expect("floor");
        let (sender, mut receiver) = mpsc::channel(1024);
        let mut handled_through = floor;
        assert!(
            forward_logged_events(&engine, &sender, &mut handled_through, LogRead::Startup).await,
            "the startup read finds nothing after the floor"
        );
        // More than one page commits before the first receive, so the subscription has lagged.
        let (events, subscription) = broadcast::channel(4);
        let mut expected = Vec::new();
        for index in 3..3 + crate::persistence::EVENT_PAGE_SIZE + 5 {
            let event = append_reactor_event(&repositories, "thread.deleted", index).await;
            expected.push(event.sequence);
            let _ = events.send(event);
        }
        drop(events);

        tokio::time::timeout(
            Duration::from_secs(10),
            forward_subscription(
                &engine,
                &sender,
                &CancellationToken::new(),
                subscription,
                handled_through,
            ),
        )
        .await
        .expect("the producer stops when its subscription closes");

        let mut forwarded = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            forwarded.push(event.sequence);
        }
        assert_eq!(forwarded, expected);
        engine.shutdown().await;
    }

    /// Events committed after the floor was read but before the subscription existed never reach
    /// the subscription, so the producer reads them from the log before its first receive.
    #[tokio::test]
    async fn events_between_the_floor_and_the_subscription_are_forwarded_without_a_lag() {
        let engine = empty_event_log().await;
        let repositories = engine.repositories();
        for index in 0..3 {
            append_reactor_event(&repositories, "thread.checkpoint-revert-requested", index).await;
        }
        let floor = repositories.max_event_sequence().await.expect("floor");
        let mut expected = Vec::new();
        for index in 3..5 {
            expected.push(
                append_reactor_event(&repositories, "thread.deleted", index)
                    .await
                    .sequence,
            );
        }
        let mut broadcast = Vec::new();
        for index in 5..8 {
            let event = append_reactor_event(&repositories, "thread.deleted", index).await;
            expected.push(event.sequence);
            broadcast.push(event);
        }

        assert_eq!(
            forwarded_sequences(&engine, floor, broadcast, 64).await,
            expected
        );
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn unit_build_covers_workspace_normalization_and_checkpoint_git_lifecycle() {
        let parent = tempfile::tempdir().expect("workspace parent");
        let missing = parent.path().join("nested/project");
        assert!(matches!(
            normalize_project_workspace_root(&missing, false).await,
            Err(OrchestrationEffectsError::WorkspaceMissing(_))
        ));
        let normalized = normalize_project_workspace_root(&missing, true)
            .await
            .expect("workspace creates");
        assert!(normalized.is_absolute());
        assert_eq!(process_compatible_path(normalized.clone()), normalized);
        let file = parent.path().join("file");
        tokio::fs::write(&file, "not a directory")
            .await
            .expect("file fixture");
        assert!(matches!(
            normalize_project_workspace_root(&file, false).await,
            Err(OrchestrationEffectsError::WorkspaceNotDirectory(_))
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let read_only = parent.path().join("read-only");
            tokio::fs::create_dir(&read_only).await.unwrap();
            tokio::fs::set_permissions(&read_only, std::fs::Permissions::from_mode(0o500))
                .await
                .unwrap();
            assert!(matches!(
                normalize_project_workspace_root(&read_only.join("child"), true).await,
                Err(OrchestrationEffectsError::WorkspaceIo { .. })
            ));
            tokio::fs::set_permissions(&read_only, std::fs::Permissions::from_mode(0o700))
                .await
                .unwrap();
        }

        let command_root = parent.path().join("command-project");
        let project_effects = ProductionProjectCommandEffects;
        let canonical_command_root = project_effects
            .canonicalize_workspace_root(command_root.to_string_lossy().as_ref(), true)
            .await
            .expect("project command canonicalizes without creating");
        assert!(Path::new(&canonical_command_root).is_absolute());
        assert!(!command_root.exists());
        project_effects
            .prepare_project_create(&canonical_command_root, true, false)
            .await
            .expect("project command prepares");
        assert!(command_root.is_dir());

        let repository = TestSandbox::new("orchestration-checkpoint");
        let git_command = repository.executable_on_path("git");
        let cancellation = CancellationToken::new();
        assert!(
            !is_git_repository(repository.root(), &git_command, &cancellation)
                .await
                .expect("non-repository probe")
        );
        run_git(
            repository.root(),
            &git_command,
            &["init"],
            &[],
            false,
            &cancellation,
        )
        .await
        .expect("git init");
        run_git(
            repository.root(),
            &git_command,
            &["config", "core.autocrlf", "false"],
            &[],
            false,
            &cancellation,
        )
        .await
        .expect("disable fixture line-ending conversion");
        run_git(
            repository.root(),
            &git_command,
            &["config", "user.name", "BiBCode Test"],
            &[],
            false,
            &cancellation,
        )
        .await
        .expect("git user name");
        run_git(
            repository.root(),
            &git_command,
            &["config", "user.email", "bibcode@example.test"],
            &[],
            false,
            &cancellation,
        )
        .await
        .expect("git user email");
        tokio::fs::write(repository.root().join("tracked.txt"), "baseline\n")
            .await
            .expect("baseline file");
        run_git(
            repository.root(),
            &git_command,
            &["add", "."],
            &[],
            false,
            &cancellation,
        )
        .await
        .expect("git add");
        run_git(
            repository.root(),
            &git_command,
            &["commit", "-m", "baseline"],
            &[],
            false,
            &cancellation,
        )
        .await
        .expect("git commit");
        assert!(
            is_git_repository(repository.root(), &git_command, &cancellation)
                .await
                .expect("repository probe")
        );

        capture_checkpoint_with_cancellation(
            repository.root(),
            &git_command,
            "thread/one",
            0,
            &cancellation,
        )
        .await
        .expect("baseline checkpoint");
        let baseline_ref = checkpoint_ref("thread/one", 0);
        assert!(
            has_ref(
                repository.root(),
                &git_command,
                &baseline_ref,
                &cancellation,
            )
            .await
            .expect("baseline ref")
        );
        tokio::fs::write(repository.root().join("tracked.txt"), "changed\n")
            .await
            .expect("changed file");
        tokio::fs::write(repository.root().join("new.txt"), "new\n")
            .await
            .expect("new file");
        capture_checkpoint_with_cancellation(
            repository.root(),
            &git_command,
            "thread/one",
            1,
            &cancellation,
        )
        .await
        .expect("changed checkpoint");
        let changed_ref = checkpoint_ref("thread/one", 1);
        let files = diff_file_summaries(
            repository.root(),
            &git_command,
            &baseline_ref,
            &changed_ref,
            &cancellation,
        )
        .await
        .expect("checkpoint diff");
        assert_eq!(files.len(), 2);
        assert!(
            restore_checkpoint(
                repository.root(),
                &git_command,
                &baseline_ref,
                false,
                &cancellation,
            )
            .await
            .expect("checkpoint restore")
        );
        assert_eq!(
            tokio::fs::read_to_string(repository.root().join("tracked.txt"))
                .await
                .expect("restored file"),
            "baseline\n"
        );
        assert!(!repository.root().join("new.txt").exists());
        assert!(
            restore_checkpoint(
                repository.root(),
                &git_command,
                "refs/bibcode/checkpoints/missing",
                true,
                &cancellation,
            )
            .await
            .expect("head fallback")
        );
        assert!(
            !restore_checkpoint(
                repository.root(),
                &git_command,
                "refs/bibcode/checkpoints/missing",
                false,
                &cancellation,
            )
            .await
            .expect("missing checkpoint")
        );
        delete_ref(repository.root(), &git_command, &changed_ref, &cancellation)
            .await
            .expect("checkpoint ref deletes");
        assert!(
            !has_ref(repository.root(), &git_command, &changed_ref, &cancellation,)
                .await
                .expect("deleted ref")
        );

        assert_eq!(checkpoint_environment(Path::new("index")).len(), 5);
        assert_eq!(
            required_str(&json!({"field":"value"}), "field").unwrap(),
            "value"
        );
        assert!(required_str(&json!({}), "field").is_err());
        assert!(!now_iso().is_empty());
        assert!(server_id("unit").starts_with("server:unit:"));
        assert!(checkpoint_ref("thread/one", 2).contains("dGhyZWFkL29uZQ"));

        struct CleanupCallbacks(AtomicUsize);

        impl OrchestrationEffectCallbacks for CleanupCallbacks {
            fn workspace_for_thread<'a>(
                &'a self,
                _: &'a str,
            ) -> BoxEffectFuture<'a, Option<PathBuf>> {
                self.0.fetch_add(1, Ordering::Relaxed);
                Box::pin(async { Ok(None) })
            }

            fn rollback_provider<'a>(
                &'a self,
                _: &'a str,
                _: i64,
                _rewind_key: &'a str,
            ) -> BoxEffectFuture<'a, ProviderRewind> {
                self.0.fetch_add(1, Ordering::Relaxed);
                Box::pin(async { Ok(ProviderRewind::Rewound) })
            }

            fn stop_provider<'a>(&'a self, _: &'a str) -> BoxEffectFuture<'a, ()> {
                self.0.fetch_add(1, Ordering::Relaxed);
                Box::pin(async { Err("provider".to_owned()) })
            }

            fn close_terminals<'a>(&'a self, _: &'a str) -> BoxEffectFuture<'a, ()> {
                self.0.fetch_add(1, Ordering::Relaxed);
                Box::pin(async { Err("terminals".to_owned()) })
            }

            fn refresh_workspace<'a>(&'a self, _: &'a Path) -> BoxEffectFuture<'a, ()> {
                self.0.fetch_add(1, Ordering::Relaxed);
                Box::pin(async { Ok(()) })
            }
        }

        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let callbacks = Arc::new(CleanupCallbacks(AtomicUsize::new(0)));
        callbacks
            .workspace_for_thread("thread")
            .await
            .expect("workspace callback");
        callbacks
            .rollback_provider("thread", 1, "rewind-key")
            .await
            .expect("rollback callback");
        callbacks
            .refresh_workspace(parent.path())
            .await
            .expect("refresh callback");
        let repositories = Repositories::new(database.clone());
        repositories
            .upsert_project(ProjectionProject {
                project_id: "malformed".to_owned(),
                title: "Malformed".to_owned(),
                workspace_root: parent
                    .path()
                    .join("malformed")
                    .to_string_lossy()
                    .into_owned(),
                default_model_selection: None,
                scripts: json!({"not":"an array"}),
                worktree_discovery: json!({"visibility":"hidden","initialPromptDismissedAt":null,"baselinePaths":[]}),
                worktree_repository_key: None,
                created_at: "2026-07-16T00:00:00Z".to_owned(),
                updated_at: "2026-07-16T00:00:00Z".to_owned(),
                deleted_at: None,
                repository_identity: None,
            })
            .await
            .expect("malformed project fixture");
        repositories
            .upsert_project(ProjectionProject {
                project_id: "empty".to_owned(),
                title: "Empty".to_owned(),
                workspace_root: parent.path().join("empty").to_string_lossy().into_owned(),
                default_model_selection: None,
                scripts: json!([]),
                worktree_discovery: json!({"visibility":"hidden","initialPromptDismissedAt":null,"baselinePaths":[]}),
                worktree_repository_key: None,
                created_at: "2026-07-16T00:00:01Z".to_owned(),
                updated_at: "2026-07-16T00:00:01Z".to_owned(),
                deleted_at: None,
                repository_identity: None,
            })
            .await
            .expect("empty project fixture");
        repositories
            .upsert_project(ProjectionProject {
                project_id: "setup".to_owned(),
                title: "Setup".to_owned(),
                workspace_root: parent.path().join("setup").to_string_lossy().into_owned(),
                default_model_selection: None,
                scripts: json!([{
                    "id":"install",
                    "name":"Install",
                    "command":"vp install",
                    "runOnWorktreeCreate":true
                }]),
                worktree_discovery: json!({"visibility":"hidden","initialPromptDismissedAt":null,"baselinePaths":[]}),
                worktree_repository_key: None,
                created_at: "2026-07-16T00:00:02Z".to_owned(),
                updated_at: "2026-07-16T00:00:02Z".to_owned(),
                deleted_at: None,
                repository_identity: None,
            })
            .await
            .expect("setup project fixture");
        let bootstrap = ProductionBootstrapEffects {
            repositories,
            repository: Arc::new(GitRepository::default()),
            callbacks: callbacks.clone(),
        };
        let bootstrap_cancellation = CancellationToken::new();
        assert!(
            bootstrap
                .prepare_worktree(
                    ThreadTurnStartBootstrapPrepareWorktree {
                        project_cwd: file.to_string_lossy().into_owned(),
                        base_branch: "main".to_owned(),
                        branch: Some("unit-invalid".to_owned()),
                        start_from_origin: None,
                    },
                    &bootstrap_cancellation
                )
                .await
                .is_err()
        );
        assert!(
            bootstrap_process_error(ProcessError::Spawn {
                operation: "unit".to_owned(),
                command: "missing".to_owned(),
                source: std::io::Error::new(std::io::ErrorKind::NotFound, "missing"),
            })
            .contains("failed to spawn")
        );
        assert!(
            bootstrap_process_error(ProcessError::NonZeroExit {
                operation: "unit".to_owned(),
                exit_code: 1,
                stdout_length: 6,
                stderr_length: 0,
                stdout: "detail".into(),
                stderr: "".into(),
            })
            .contains("detail")
        );
        assert!(
            bootstrap
                .run_setup_script(BootstrapSetupInput {
                    thread_id: "thread".to_owned(),
                    project_id: None,
                    project_cwd: None,
                    worktree_path: parent.path().to_string_lossy().into_owned(),
                })
                .await
                .is_err()
        );
        assert!(
            bootstrap
                .run_setup_script(BootstrapSetupInput {
                    thread_id: "thread".to_owned(),
                    project_id: Some("malformed".to_owned()),
                    project_cwd: None,
                    worktree_path: parent.path().to_string_lossy().into_owned(),
                })
                .await
                .is_err()
        );
        assert_eq!(
            bootstrap
                .run_setup_script(BootstrapSetupInput {
                    thread_id: "thread".to_owned(),
                    project_id: None,
                    project_cwd: Some(parent.path().join("empty").to_string_lossy().into_owned()),
                    worktree_path: parent.path().to_string_lossy().into_owned(),
                })
                .await
                .expect("empty scripts should be a no-op"),
            BootstrapSetupResult::NoScript
        );
        assert!(
            bootstrap
                .run_setup_script(BootstrapSetupInput {
                    thread_id: "thread".to_owned(),
                    project_id: Some("setup".to_owned()),
                    project_cwd: None,
                    worktree_path: parent.path().to_string_lossy().into_owned(),
                })
                .await
                .expect_err("unavailable setup terminal fails")
                .contains("unavailable")
        );
        database
            .call(|connection| {
                connection.execute("DROP TABLE projection_projects", [])?;
                Ok(())
            })
            .await
            .expect("drop project projection");
        for input in [
            BootstrapSetupInput {
                thread_id: "thread".to_owned(),
                project_id: Some("missing".to_owned()),
                project_cwd: None,
                worktree_path: parent.path().to_string_lossy().into_owned(),
            },
            BootstrapSetupInput {
                thread_id: "thread".to_owned(),
                project_id: None,
                project_cwd: Some("missing".to_owned()),
                worktree_path: parent.path().to_string_lossy().into_owned(),
            },
        ] {
            assert!(bootstrap.run_setup_script(input).await.is_err());
        }
        assert_eq!(callbacks.0.load(Ordering::Relaxed), 3);
    }

    #[tokio::test]
    async fn setup_script_restart_reuses_the_deterministic_terminal() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        struct SetupCallbacks {
            running: AtomicBool,
            launches: AtomicUsize,
        }

        impl OrchestrationEffectCallbacks for SetupCallbacks {
            fn workspace_for_thread<'a>(
                &'a self,
                _: &'a str,
            ) -> BoxEffectFuture<'a, Option<PathBuf>> {
                Box::pin(async { Ok(None) })
            }

            fn rollback_provider<'a>(
                &'a self,
                _: &'a str,
                _: i64,
                _rewind_key: &'a str,
            ) -> BoxEffectFuture<'a, ProviderRewind> {
                Box::pin(async { Ok(ProviderRewind::Rewound) })
            }

            fn stop_provider<'a>(&'a self, _: &'a str) -> BoxEffectFuture<'a, ()> {
                Box::pin(async { Ok(()) })
            }

            fn close_terminals<'a>(&'a self, _: &'a str) -> BoxEffectFuture<'a, ()> {
                Box::pin(async { Ok(()) })
            }

            fn refresh_workspace<'a>(&'a self, _: &'a Path) -> BoxEffectFuture<'a, ()> {
                Box::pin(async { Ok(()) })
            }

            fn setup_script_is_running<'a>(
                &'a self,
                _thread_id: &'a str,
                terminal_id: &'a str,
            ) -> BoxEffectFuture<'a, bool> {
                assert_eq!(terminal_id, "setup-install");
                Box::pin(async move { Ok(self.running.load(Ordering::SeqCst)) })
            }

            fn launch_setup_script<'a>(
                &'a self,
                input: SetupScriptLaunch,
            ) -> BoxEffectFuture<'a, ()> {
                assert_eq!(input.terminal_id, "setup-install");
                Box::pin(async move {
                    self.launches.fetch_add(1, Ordering::SeqCst);
                    self.running.store(true, Ordering::SeqCst);
                    Ok(())
                })
            }
        }

        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let repositories = Repositories::new(database);
        repositories
            .upsert_project(ProjectionProject {
                project_id: "setup-project".to_owned(),
                title: "Setup".to_owned(),
                workspace_root: "C:/repo".to_owned(),
                default_model_selection: None,
                scripts: json!([{
                    "id":"install",
                    "name":"Install",
                    "command":"vp install",
                    "runOnWorktreeCreate":true
                }]),
                worktree_discovery: json!({"visibility":"hidden","initialPromptDismissedAt":null,"baselinePaths":[]}),
                worktree_repository_key: None,
                created_at: "2026-08-01T00:00:00Z".to_owned(),
                updated_at: "2026-08-01T00:00:00Z".to_owned(),
                deleted_at: None,
                repository_identity: None,
            })
            .await
            .expect("project");
        let callbacks = Arc::new(SetupCallbacks {
            running: AtomicBool::new(false),
            launches: AtomicUsize::new(0),
        });
        let bootstrap = ProductionBootstrapEffects {
            repositories,
            repository: Arc::new(GitRepository::default()),
            callbacks: callbacks.clone(),
        };
        let input = BootstrapSetupInput {
            thread_id: "thread".to_owned(),
            project_id: Some("setup-project".to_owned()),
            project_cwd: None,
            worktree_path: "C:/repo/.worktrees/setup".to_owned(),
        };

        let first = bootstrap
            .run_setup_script(input.clone())
            .await
            .expect("first launch");
        let restarted = bootstrap
            .run_setup_script(input)
            .await
            .expect("restart lookup");

        assert_eq!(first, restarted);
        assert_eq!(callbacks.launches.load(Ordering::SeqCst), 1);
    }
}
