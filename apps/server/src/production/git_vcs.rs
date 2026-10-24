use std::{
    ffi::OsString,
    future::Future,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

#[cfg(not(windows))]
use std::process::Stdio;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[cfg(not(windows))]
use tokio::process::Command;
use tokio::sync::{Notify, mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::{
    git::{
        ChangeRequest, CreateWorktreeInput, GitCommandError, GitProcessRunner, GitRepository,
        GitStatusSummaryService, OutputPolicy, ProcessRequest, ProcessRunner,
        STATUS_SAFETY_INTERVAL, StatusBroadcaster, StatusReadFence, VcsStatusLocalResult,
        VcsStatusRemoteResult, VcsStatusStreamEvent, clone_destination_leaf, validate_pathspecs,
    },
    maintenance::RpcPermit,
    persistence::{Repositories, WorktreeRemovalReceipt},
    pull_requests::CreatedRequestObserver,
    rpc::{RpcRegistry, RpcRequest, RpcResult, RpcSessionContext, RpcStreamChunk},
    source_control::{
        ChangeRequestState, CreatePullRequestInput, CreatePullRequestOptions, ProviderHosts,
        ProviderKind, PullRequestService, ResolvePullRequestInput, ResolvedPullRequest,
        SourceControlDiscovery,
    },
    terminal::TerminalManager,
    workspace::{WorkspaceMutationFuture, WorkspaceMutationObserver},
    worktree_catalog::{WorkspaceAdmissionLease, WorkspaceAvailabilityRegistry},
};

use super::clone_operations::{CloneRequest, CloneRuntime};
use super::host_paths::resolve_host_directory;

const STREAM_CAPACITY: usize = 8;

#[derive(Clone, Default)]
pub(crate) struct WorktreeRemovalTaskTracker {
    inner: Arc<WorktreeRemovalTaskTrackerInner>,
}

#[derive(Default)]
struct WorktreeRemovalTaskTrackerInner {
    state: Mutex<WorktreeRemovalTaskState>,
    drained: Notify,
}

#[derive(Default)]
struct WorktreeRemovalTaskState {
    closed: bool,
    active: usize,
}

struct WorktreeRemovalTaskGuard {
    tracker: WorktreeRemovalTaskTracker,
}

impl Drop for WorktreeRemovalTaskGuard {
    fn drop(&mut self) {
        let notify = {
            let mut state = self
                .tracker
                .inner
                .state
                .lock()
                .expect("worktree removal task mutex poisoned");
            debug_assert!(state.active > 0);
            state.active = state.active.saturating_sub(1);
            state.active == 0
        };
        if notify {
            self.tracker.inner.drained.notify_waiters();
        }
    }
}

impl WorktreeRemovalTaskTracker {
    fn spawn<F, T>(&self, operation: F) -> Result<tokio::task::JoinHandle<T>, String>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        let guard = {
            let mut state = self
                .inner
                .state
                .lock()
                .expect("worktree removal task mutex poisoned");
            if state.closed {
                return Err("the server is shutting down".to_owned());
            }
            state.active = state.active.saturating_add(1);
            WorktreeRemovalTaskGuard {
                tracker: self.clone(),
            }
        };
        Ok(tokio::spawn(async move {
            let _guard = guard;
            operation.await
        }))
    }

    pub(crate) async fn close_and_drain(&self) {
        {
            let mut state = self
                .inner
                .state
                .lock()
                .expect("worktree removal task mutex poisoned");
            state.closed = true;
        }
        loop {
            let notified = self.inner.drained.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self
                .inner
                .state
                .lock()
                .expect("worktree removal task mutex poisoned")
                .active
                == 0
            {
                return;
            }
            notified.await;
        }
    }
}

#[cfg(test)]
mod worktree_removal_task_tracker_tests {
    use super::WorktreeRemovalTaskTracker;

    #[tokio::test]
    async fn runtime_drain_waits_for_protected_removal_and_rejects_late_tasks() {
        let tracker = WorktreeRemovalTaskTracker::default();
        let (release, released) = tokio::sync::oneshot::channel();
        let operation = tracker
            .spawn(async move {
                let _ = released.await;
            })
            .expect("protected task admitted");
        let draining_tracker = tracker.clone();
        let drain = tokio::spawn(async move {
            draining_tracker.close_and_drain().await;
        });
        tokio::task::yield_now().await;
        assert!(
            !drain.is_finished(),
            "drain returned before removal finished"
        );

        release.send(()).expect("release protected task");
        operation.await.expect("protected task completed");
        drain.await.expect("runtime drain completed");
        assert!(
            tracker.spawn(async {}).is_err(),
            "shutdown admitted a late removal task"
        );
    }
}

/// Runs after a finished Git Manager or VCS mutation and after a reported local
/// change (a terminal command exiting) for a worktree.
///
/// It forgets that worktree's shared open pull-request lookups first: a push,
/// a created request, or a command such as `gh pr create` can change the
/// hosting service's answer, and the status publication that follows must ask
/// again rather than reuse an answer from before the change.
///
/// It then starts a fresh passive summary cycle instead of waiting for the
/// 30-second deadline, because passive summaries feed labels such as the
/// sidebar branch. Both are user-paced, so restarting an in-flight cycle is
/// cheap. The observer only schedules the nudge; it never blocks the caller.
fn observe_local_changes(
    broadcaster: &StatusBroadcaster,
    summary: &GitStatusSummaryService,
    pull_requests: &PullRequestService,
) {
    let summary = summary.clone();
    let pull_requests = pull_requests.clone();
    broadcaster.set_local_change_observer(move |cwd| {
        pull_requests.invalidate_open_pull_request_lookups(cwd);
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let summary = summary.clone();
        let cwd = cwd.to_path_buf();
        handle.spawn(async move { summary.notify_local_change(&cwd).await });
    });
}

pub const GIT_VCS_UNARY_METHODS: &[&str] = &[
    "shell.openInEditor",
    "vcs.pull",
    "vcs.refreshStatus",
    "vcs.listRefs",
    "vcs.listCommits",
    "vcs.clone",
    "vcs.cancelClone",
    "vcs.createRef",
    "vcs.switchRef",
    "vcs.init",
    "vcs.stageFiles",
    "vcs.unstageFiles",
    "vcs.discardFiles",
    "vcs.generateCommitMessage",
    "git.resolvePullRequest",
    "git.preparePullRequestThread",
    "server.discoverSourceControl",
    "sourceControl.lookupRepository",
    "sourceControl.cloneRepository",
    "sourceControl.publishRepository",
];

pub const GIT_VCS_STREAM_METHODS: &[&str] = &[
    "subscribeVcsStatus",
    "subscribeVcsStatusSummary",
    "git.runStackedAction",
];

#[derive(Clone)]
pub struct GitVcsRpcServices {
    trace_diagnostics: Option<crate::diagnostics::TraceDiagnosticsStore>,
    repository: Arc<GitRepository>,
    broadcaster: StatusBroadcaster,
    summary: GitStatusSummaryService,
    discovery: SourceControlDiscovery,
    /// Written only by an explicit Settings discovery (`recordHosts`).
    provider_hosts: Arc<ProviderHosts>,
    pull_requests: PullRequestService,
    /// Told about requests `git.runStackedAction` creates, which bypass
    /// `pullRequests.runAction` and its list-totals invalidation.
    created_request_observer: Option<Arc<dyn CreatedRequestObserver>>,
    github_command: PathBuf,
    gitlab_command: PathBuf,
    github_runner: Arc<dyn GitProcessRunner>,
    availability_registry: Option<WorkspaceAvailabilityRegistry>,
    terminal: Option<TerminalManager>,
    repositories: Option<Repositories>,
    worktree_removal_tasks: WorktreeRemovalTaskTracker,
    /// Server-owned clones, keyed by destination.
    clone_operations: CloneRuntime,
    #[cfg(test)]
    status_stream_enrichment_test_hook: Option<Arc<StatusStreamEnrichmentTestHook>>,
}

#[cfg(test)]
struct StatusStreamEnrichmentTestHook {
    started: Notify,
    release: tokio::sync::Semaphore,
}

#[cfg(test)]
impl Default for StatusStreamEnrichmentTestHook {
    fn default() -> Self {
        Self {
            started: Notify::new(),
            release: tokio::sync::Semaphore::new(0),
        }
    }
}

#[cfg(test)]
impl StatusStreamEnrichmentTestHook {
    async fn block(&self, cancellation: &CancellationToken) {
        self.started.notify_one();
        tokio::select! {
            biased;
            () = cancellation.cancelled() => {}
            permit = self.release.acquire() => {
                if let Ok(permit) = permit {
                    permit.forget();
                }
            }
        }
    }
}

#[cfg(test)]
impl Default for GitVcsRpcServices {
    fn default() -> Self {
        let provider_hosts = Arc::new(ProviderHosts::default());
        Self::with_repository(
            Arc::new(GitRepository::default().with_provider_hosts(provider_hosts.clone())),
            provider_hosts,
        )
    }
}

impl GitVcsRpcServices {
    pub fn with_repository(
        repository: Arc<GitRepository>,
        provider_hosts: Arc<ProviderHosts>,
    ) -> Self {
        let (automatic_remote_refresh_interval, _) = watch::channel(STATUS_SAFETY_INTERVAL);
        Self::with_repository_and_automatic_fetch_interval(
            repository,
            automatic_remote_refresh_interval,
            provider_hosts,
        )
    }

    pub fn with_repository_and_automatic_fetch_interval(
        repository: Arc<GitRepository>,
        automatic_remote_refresh_interval: watch::Sender<Duration>,
        provider_hosts: Arc<ProviderHosts>,
    ) -> Self {
        Self::with_repository_dependencies(
            repository,
            automatic_remote_refresh_interval,
            None,
            None,
            provider_hosts,
        )
    }

    pub fn with_repository_and_terminal(
        repository: Arc<GitRepository>,
        terminal: TerminalManager,
        provider_hosts: Arc<ProviderHosts>,
    ) -> Self {
        let (automatic_remote_refresh_interval, _) = watch::channel(STATUS_SAFETY_INTERVAL);
        Self::with_repository_dependencies(
            repository,
            automatic_remote_refresh_interval,
            Some(terminal),
            None,
            provider_hosts,
        )
    }

    pub fn with_repository_terminal_and_automatic_fetch_interval(
        repository: Arc<GitRepository>,
        terminal: TerminalManager,
        automatic_remote_refresh_interval: watch::Sender<Duration>,
        provider_hosts: Arc<ProviderHosts>,
    ) -> Self {
        Self::with_repository_dependencies(
            repository,
            automatic_remote_refresh_interval,
            Some(terminal),
            None,
            provider_hosts,
        )
    }

    pub fn with_production_dependencies(
        repository: Arc<GitRepository>,
        terminal: TerminalManager,
        repositories: Repositories,
        automatic_remote_refresh_interval: watch::Sender<Duration>,
        provider_hosts: Arc<ProviderHosts>,
    ) -> Self {
        Self::with_repository_dependencies(
            repository,
            automatic_remote_refresh_interval,
            Some(terminal),
            Some(repositories),
            provider_hosts,
        )
    }

    fn with_repository_dependencies(
        repository: Arc<GitRepository>,
        automatic_remote_refresh_interval: watch::Sender<Duration>,
        terminal: Option<TerminalManager>,
        repositories: Option<Repositories>,
        provider_hosts: Arc<ProviderHosts>,
    ) -> Self {
        let pull_requests = PullRequestService::default();
        let broadcaster = StatusBroadcaster::with_automatic_remote_refresh_interval(
            Arc::clone(&repository),
            STATUS_SAFETY_INTERVAL,
            automatic_remote_refresh_interval,
            STREAM_CAPACITY,
        );
        let summary = GitStatusSummaryService::new(Arc::clone(&repository), pull_requests.clone());
        observe_local_changes(&broadcaster, &summary, &pull_requests);
        let clone_operations = CloneRuntime::new(Arc::clone(&repository));
        Self {
            trace_diagnostics: None,
            broadcaster,
            summary,
            provider_hosts,
            repository,
            discovery: SourceControlDiscovery::default(),
            pull_requests,
            created_request_observer: None,
            github_command: PathBuf::from("gh"),
            gitlab_command: PathBuf::from("glab"),
            github_runner: Arc::new(ProcessRunner),
            availability_registry: None,
            terminal,
            repositories,
            worktree_removal_tasks: WorktreeRemovalTaskTracker::default(),
            clone_operations,
            #[cfg(test)]
            status_stream_enrichment_test_hook: None,
        }
    }

    /// Pin discovery, repository lookup, and pull requests (including passive
    /// summaries) to test-owned executables before starting subscriptions.
    /// Git remains the real executable; other probes use this directory.
    #[doc(hidden)]
    #[must_use]
    pub fn with_hosting_executable_dir_for_integration_test(mut self, directory: PathBuf) -> Self {
        self.discovery =
            SourceControlDiscovery::with_executable_dir_for_integration_test(directory.clone());
        self.github_command = directory.join("gh");
        self.gitlab_command = directory.join("glab");
        self.pull_requests = PullRequestService::with_provider_commands(
            self.github_command.to_string_lossy().into_owned(),
            self.gitlab_command.to_string_lossy().into_owned(),
            directory.join("az").to_string_lossy().into_owned(),
        );
        self.summary =
            GitStatusSummaryService::new(Arc::clone(&self.repository), self.pull_requests.clone());
        observe_local_changes(&self.broadcaster, &self.summary, &self.pull_requests);
        self
    }

    #[cfg(all(test, unix))]
    fn with_repository_and_github_command(
        repository: Arc<GitRepository>,
        github_command: PathBuf,
    ) -> Self {
        let mut services = Self::with_repository(repository, Arc::default());
        services.github_command = github_command;
        services
    }

    #[cfg(test)]
    fn with_repository_and_github_runner_for_test(
        repository: Arc<GitRepository>,
        github_runner: Arc<dyn GitProcessRunner>,
    ) -> Self {
        let mut services = Self::with_repository(repository, Arc::default());
        services.github_runner = github_runner;
        services
    }

    #[cfg(test)]
    fn with_status_stream_enrichment_test_hook(
        mut self,
        hook: Arc<StatusStreamEnrichmentTestHook>,
    ) -> Self {
        self.status_stream_enrichment_test_hook = Some(hook);
        self
    }

    /// The Pull Requests service, so a request created here refreshes its list totals.
    #[must_use]
    pub fn with_created_request_observer(
        mut self,
        observer: Arc<dyn CreatedRequestObserver>,
    ) -> Self {
        self.created_request_observer = Some(observer);
        self
    }

    #[must_use]
    pub fn with_availability_registry(
        mut self,
        availability_registry: WorkspaceAvailabilityRegistry,
    ) -> Self {
        self.availability_registry = Some(availability_registry);
        self
    }

    pub(crate) fn worktree_removal_tasks(&self) -> WorktreeRemovalTaskTracker {
        self.worktree_removal_tasks.clone()
    }

    pub(crate) fn clone_operations(&self) -> CloneRuntime {
        self.clone_operations.clone()
    }

    pub(crate) fn status_broadcaster(&self) -> StatusBroadcaster {
        self.broadcaster.clone()
    }
}

impl WorkspaceMutationObserver for GitVcsRpcServices {
    fn begin_workspace_mutation<'a>(&'a self, cwd: &'a Path) -> WorkspaceMutationFuture<'a> {
        Box::pin(async move { Some(self.broadcaster.begin_mutation(cwd).await) })
    }
}

pub fn register_git_vcs_rpc(registry: &mut RpcRegistry, mut services: GitVcsRpcServices) {
    services.trace_diagnostics = registry.trace_diagnostics();
    for method in GIT_VCS_UNARY_METHODS {
        let services = services.clone();
        registry.register_unary_with_context(*method, move |request, context, cancellation| {
            let services = services.clone();
            async move {
                services
                    .handle_unary_with_context(request, context, cancellation)
                    .await
            }
        });
    }

    let stream_services = services.clone();
    registry.register_stream(GIT_VCS_STREAM_METHODS[0], move |request, cancellation| {
        stream_services.status_stream(request, cancellation)
    });
    let summary_services = services.clone();
    registry.register_latest_stream(GIT_VCS_STREAM_METHODS[1], move |request, cancellation| {
        summary_services.summary_stream(request, cancellation)
    });
    registry.register_stream(GIT_VCS_STREAM_METHODS[2], move |request, cancellation| {
        services.stacked_action_stream(request, cancellation)
    });
}

async fn await_server_owned_rpc(
    method: &'static str,
    operation: impl Future<Output = RpcResult> + Send + 'static,
) -> RpcResult {
    match tokio::spawn(operation).await {
        Ok(result) => result,
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(error) => Err(request_error(method, &error.to_string())),
    }
}

impl GitVcsRpcServices {
    async fn handle_unary_with_context(
        &self,
        request: RpcRequest,
        context: RpcSessionContext,
        cancellation: CancellationToken,
    ) -> RpcResult {
        let workspace_admission = self.guard_payload_cwd(&request.payload).await?;
        let operation_cancellation = cancellation.child_token();
        match request.tag.as_str() {
            "vcs.pull" => {
                let input: CwdInput = decode(request.payload, "vcs.pull")?;
                let repository = Arc::clone(&self.repository);
                let token = operation_cancellation.clone();
                self.run_owned_git_mutation(
                    "vcs.pull",
                    input.cwd.clone(),
                    workspace_admission,
                    operation_cancellation,
                    async move {
                        encode_result(repository.pull_current_branch(&input.cwd, &token).await)
                    },
                )
                .await
            }
            "vcs.createRef" => {
                let input: CreateRefInput = decode(request.payload, "vcs.createRef")?;
                let repository = Arc::clone(&self.repository);
                let token = operation_cancellation.clone();
                self.run_owned_git_mutation(
                    "vcs.createRef",
                    input.cwd.clone(),
                    workspace_admission,
                    operation_cancellation,
                    async move {
                        encode_result(
                            repository
                                .create_ref(
                                    &input.cwd,
                                    &input.ref_name,
                                    input.switch_ref.unwrap_or(false),
                                    &token,
                                )
                                .await
                                .map(|ref_name| json!({ "refName": ref_name })),
                        )
                    },
                )
                .await
            }
            "vcs.switchRef" => {
                let input: SwitchRefInput = decode(request.payload, "vcs.switchRef")?;
                let repository = Arc::clone(&self.repository);
                let token = operation_cancellation.clone();
                self.run_owned_git_mutation(
                    "vcs.switchRef",
                    input.cwd.clone(),
                    workspace_admission,
                    operation_cancellation,
                    async move {
                        encode_result(
                            repository
                                .switch_ref(&input.cwd, &input.ref_name, &token)
                                .await
                                .map(|ref_name| json!({ "refName": ref_name })),
                        )
                    },
                )
                .await
            }
            "vcs.init" => {
                let input: InitInput = decode(request.payload, "vcs.init")?;
                if input.kind.as_deref().is_some_and(|kind| kind != "git") {
                    return Err(vcs_error(
                        "vcs.init",
                        &input.cwd,
                        "Only the git VCS driver can initialize repositories.",
                    ));
                }
                let repository = Arc::clone(&self.repository);
                let token = operation_cancellation.clone();
                self.run_owned_git_mutation(
                    "vcs.init",
                    input.cwd.clone(),
                    workspace_admission,
                    operation_cancellation,
                    async move { encode_null(repository.init(&input.cwd, &token).await) },
                )
                .await
            }
            "vcs.stageFiles" | "vcs.unstageFiles" | "vcs.discardFiles" => {
                let tag = request.tag;
                let input: FilePathsInput = decode(request.payload, &tag)?;
                if input.file_paths.is_empty() {
                    return Ok(Value::Null);
                }
                let operation = match tag.as_str() {
                    "vcs.stageFiles" => "GitVcsDriver.stageFiles",
                    "vcs.unstageFiles" => "GitVcsDriver.unstageFiles",
                    _ => "GitVcsDriver.discardFiles",
                };
                validate_pathspecs(operation, &input.cwd, &input.file_paths)
                    .map_err(serialize_error)?;
                let repository = Arc::clone(&self.repository);
                let token = operation_cancellation.clone();
                let method = match tag.as_str() {
                    "vcs.stageFiles" => "vcs.stageFiles",
                    "vcs.unstageFiles" => "vcs.unstageFiles",
                    _ => "vcs.discardFiles",
                };
                self.run_owned_git_mutation(
                    method,
                    input.cwd.clone(),
                    workspace_admission,
                    operation_cancellation,
                    async move {
                        let result = match tag.as_str() {
                            "vcs.stageFiles" => {
                                repository
                                    .stage_files(&input.cwd, &input.file_paths, &token)
                                    .await
                            }
                            "vcs.unstageFiles" => {
                                repository
                                    .unstage_files(&input.cwd, &input.file_paths, &token)
                                    .await
                            }
                            _ => {
                                repository
                                    .discard_files(&input.cwd, &input.file_paths, &token)
                                    .await
                            }
                        };
                        encode_null(result)
                    },
                )
                .await
            }
            "git.preparePullRequestThread" => {
                let input: PreparePullRequestInput =
                    decode(request.payload, "git.preparePullRequestThread")?;
                let (pull_request, branch) = await_git_rpc_operation(
                    workspace_admission.as_ref(),
                    operation_cancellation.clone(),
                    self.resolve_pull_request_preparation(&input, &operation_cancellation),
                )
                .await?;
                let repository = Arc::clone(&self.repository);
                let token = operation_cancellation.clone();
                self.run_owned_git_mutation(
                    "git.preparePullRequestThread",
                    input.cwd.clone(),
                    workspace_admission,
                    operation_cancellation,
                    async move {
                        repository
                            .switch_ref(&input.cwd, &branch, &token)
                            .await
                            .map_err(serialize_error)?;
                        Ok(json!({ "pullRequest": pull_request, "branch": branch }))
                    },
                )
                .await
            }
            "sourceControl.publishRepository" => {
                let input: PublishRepositoryInput =
                    decode(request.payload, "sourceControl.publishRepository")?;
                let (remote_name, visibility) = preflight_publish_repository(&input)?;
                let services = self.clone();
                let token = operation_cancellation.clone();
                self.run_owned_git_mutation(
                    "sourceControl.publishRepository",
                    input.cwd.clone(),
                    workspace_admission,
                    operation_cancellation,
                    async move {
                        services
                            .publish_repository(input, remote_name, visibility, &token)
                            .await
                    },
                )
                .await
            }
            _ => {
                await_git_rpc_operation(
                    workspace_admission.as_ref(),
                    operation_cancellation.clone(),
                    self.handle_admitted_unary(request, context, operation_cancellation),
                )
                .await
            }
        }
    }

    async fn run_owned_git_mutation(
        &self,
        method: &'static str,
        cwd: PathBuf,
        workspace_admission: Option<WorkspaceAdmissionLease>,
        cancellation: CancellationToken,
        operation: impl Future<Output = RpcResult> + Send + 'static,
    ) -> RpcResult {
        let broadcaster = self.broadcaster.clone();
        await_server_owned_rpc(method, async move {
            let loss = workspace_admission
                .as_ref()
                .map(WorkspaceAdmissionLease::loss_cancellation);
            let mutation = match loss {
                Some(loss) => {
                    tokio::select! {
                        biased;
                        () = loss.cancelled() => {
                            cancellation.cancel();
                            return Err(serialize_error(
                                loss.unavailable().expect("workspace loss retains its error"),
                            ));
                        }
                        () = cancellation.cancelled() => {
                            return Err(request_error(method, "The Git mutation was cancelled before admission."));
                        }
                        mutation = broadcaster.begin_mutation(&cwd) => mutation,
                    }
                }
                None => {
                    tokio::select! {
                        biased;
                        () = cancellation.cancelled() => {
                            return Err(request_error(method, "The Git mutation was cancelled before admission."));
                        }
                        mutation = broadcaster.begin_mutation(&cwd) => mutation,
                    }
                }
            };
            let result = await_git_mutation_terminal(
                workspace_admission.as_ref(),
                cancellation,
                operation,
            )
            .await;
            mutation.finish().await;
            result
        })
        .await
    }

    async fn handle_admitted_unary(
        &self,
        request: RpcRequest,
        context: RpcSessionContext,
        cancellation: CancellationToken,
    ) -> RpcResult {
        match request.tag.as_str() {
            "shell.openInEditor" => self.open_in_editor(request.payload).await,
            "vcs.refreshStatus" => {
                let input: CwdInput = decode(request.payload, "vcs.refreshStatus")?;
                let publication = self
                    .broadcaster
                    .refresh_status(&input.cwd, &cancellation)
                    .await
                    .map_err(serialize_error)?;
                let fence = publication.fence;
                let mut status = publication.value;
                let status = finish_fenced_enrichment(&self.broadcaster, fence, async {
                    enrich_remote_pull_request(
                        &self.pull_requests,
                        &input.cwd,
                        &status.local,
                        &mut status.remote,
                        &cancellation,
                    )
                    .await;
                    status
                })
                .await
                .map_err(serialize_error)?;
                serde_json::to_value(status)
                    .map_err(|error| request_error("vcs.refreshStatus", &error.to_string()))
            }
            "vcs.listRefs" => {
                let input: ListRefsInput = decode(request.payload, "vcs.listRefs")?;
                encode_result(
                    self.repository
                        .list_refs(
                            &input.cwd,
                            input.query.as_deref(),
                            input.cursor.unwrap_or(0),
                            input.limit.unwrap_or(50).clamp(1, 200),
                            input.include_matching_remote_refs.unwrap_or(false),
                            input.ref_kind.as_deref(),
                            &cancellation,
                        )
                        .await,
                )
            }
            "vcs.listCommits" => {
                let input: ListCommitsInput = decode(request.payload, "vcs.listCommits")?;
                encode_result(
                    self.repository
                        .list_commits(
                            &input.cwd,
                            input.limit.unwrap_or(50).clamp(1, 200),
                            input.cursor.unwrap_or(0),
                            &cancellation,
                        )
                        .await,
                )
            }
            "vcs.createWorktree" => {
                let input: CreateWorktree = decode(request.payload, "vcs.createWorktree")?;
                encode_result(
                    self.repository
                        .create_worktree(
                            CreateWorktreeInput {
                                cwd: input.cwd,
                                ref_name: input.ref_name,
                                new_ref_name: input.new_ref_name,
                                base_ref_name: input.base_ref_name,
                                path: input.path,
                            },
                            &cancellation,
                        )
                        .await,
                )
            }
            "vcs.removeWorktree" => {
                let input: RemoveWorktree = decode(request.payload, "vcs.removeWorktree")?;
                if let Some(repositories) = &self.repositories {
                    return self
                        .remove_worktree_with_durable_admission(
                            input,
                            repositories.clone(),
                            context.admission_permit(),
                            cancellation,
                        )
                        .await;
                }
                let _terminal_removal = match &self.terminal {
                    Some(_) if input.thread_ids.is_empty() => {
                        return Err(vcs_error(
                            "vcs.removeWorktree",
                            &input.cwd,
                            "Worktree removal requires the owning terminal thread IDs.",
                        ));
                    }
                    Some(terminal) => Some(
                        terminal
                            .begin_worktree_removal(&input.thread_ids, &input.path)
                            .await
                            .map_err(|error| {
                                vcs_error(
                                    "vcs.removeWorktree",
                                    &input.cwd,
                                    &format!(
                                        "Could not fence worktree terminal activity before removal: {error}"
                                    ),
                                )
                            })?,
                    ),
                    None => None,
                };
                encode_null(
                    self.repository
                        .remove_worktree(
                            &input.cwd,
                            &input.path,
                            input.force.unwrap_or(false),
                            &cancellation,
                        )
                        .await,
                )
            }
            "vcs.clone" => {
                let input: CloneInput = decode(request.payload, "vcs.clone")?;
                // One validated folder name before anything reads the disk or the runtime.
                let leaf = clone_destination_leaf(&input.url, input.directory_name.as_deref())
                    .map_err(|error| {
                        vcs_error("vcs.clone", &input.parent_dir, &error.to_string())
                    })?;
                let parent_dir = resolve_host_directory(&input.parent_dir, false)
                    .await
                    .map_err(|error| {
                        vcs_error("vcs.clone", &input.parent_dir, &error.to_string())
                    })?;
                // The starter's permit rides with the clone, so an update drain names a
                // detached clone until Git has stopped and its cleanup has finished.
                let path = self
                    .clone_operations
                    .start_or_join(
                        CloneRequest {
                            url: input.url,
                            parent_dir,
                            leaf,
                            attach: input.attach,
                            detach: input.detach,
                        },
                        context.admission_permit(),
                        &cancellation,
                    )
                    .await?;
                Ok(json!({ "path": display_path(path) }))
            }
            "vcs.cancelClone" => {
                let input: CancelCloneInput = decode(request.payload, "vcs.cancelClone")?;
                // A path as the folder name could alias another clone's destination key.
                let leaf = clone_destination_leaf(&input.url, input.directory_name.as_deref())
                    .map_err(|error| {
                        vcs_error("vcs.cancelClone", &input.parent_dir, &error.to_string())
                    })?;
                let parent_dir = resolve_host_directory(&input.parent_dir, false)
                    .await
                    .map_err(|error| {
                        vcs_error("vcs.cancelClone", &input.parent_dir, &error.to_string())
                    })?;
                let cancelled = self
                    .clone_operations
                    .cancel(&input.url, &parent_dir, &leaf)
                    .await;
                Ok(json!({ "cancelled": cancelled }))
            }
            "vcs.generateCommitMessage" => {
                let input: CommitMessageInput =
                    decode(request.payload, "vcs.generateCommitMessage")?;
                let context = self
                    .repository
                    .commit_context(&input.cwd, &cancellation)
                    .await
                    .map_err(serialize_error)?;
                let message = summarize_commit_context(&context, input.file_paths.as_deref());
                Ok(json!({ "message": message }))
            }
            "git.resolvePullRequest" => {
                let input: PullRequestInput = decode(request.payload, "git.resolvePullRequest")?;
                let pull_request = self.resolve_pull_request(&input, &cancellation).await?;
                Ok(json!({ "pullRequest": pull_request }))
            }
            "server.discoverSourceControl" => {
                let input: DiscoverSourceControlInput =
                    decode(request.payload, "server.discoverSourceControl")?;
                let discovery = self
                    .discovery
                    .discover(PathBuf::from("."), &cancellation)
                    .await;
                // Only an explicit Settings scan records hosts; background reads (for
                // example the publish dialog) leave the observation untouched.
                if input.record_hosts {
                    self.provider_hosts.replace_from_discovery(&discovery);
                }
                Ok(encode_value(discovery))
            }
            "sourceControl.lookupRepository" => {
                let input: LookupRepositoryInput =
                    decode(request.payload, "sourceControl.lookupRepository")?;
                self.lookup_repository(input, cancellation).await
            }
            "sourceControl.cloneRepository" => {
                let input: CloneRepositoryInput =
                    decode(request.payload, "sourceControl.cloneRepository")?;
                self.clone_source_repository(input, context.admission_permit(), &cancellation)
                    .await
            }
            _ => Err(request_error(
                &request.tag,
                "RPC method is not registered here.",
            )),
        }
    }

    async fn remove_worktree_with_durable_admission(
        &self,
        input: RemoveWorktree,
        repositories: Repositories,
        admission: Option<RpcPermit>,
        cancellation: CancellationToken,
    ) -> RpcResult {
        let Some(owner_thread_id) = input.owner_thread_id.clone() else {
            return Err(vcs_error(
                "vcs.removeWorktree",
                &input.cwd,
                "Worktree removal requires its owning workspace thread ID.",
            ));
        };
        if !input
            .thread_ids
            .iter()
            .any(|thread_id| thread_id == &owner_thread_id)
        {
            return Err(vcs_error(
                "vcs.removeWorktree",
                &input.cwd,
                "The terminal fence must include the owning workspace thread ID.",
            ));
        }
        let existing_receipt = repositories
            .get_worktree_removal_receipt(owner_thread_id.clone())
            .await
            .map_err(|error| {
                vcs_error(
                    "vcs.removeWorktree",
                    &input.cwd,
                    &format!("Could not read the durable worktree removal receipt: {error}"),
                )
            })?;
        if let Some(receipt) = &existing_receipt {
            ensure_receipt_matches_request(receipt, &input)?;
            if receipt.state == "removed" {
                return Ok(Value::Null);
            }
        }

        let owner = repositories
            .get_thread(owner_thread_id.clone())
            .await
            .map_err(|error| {
                vcs_error(
                    "vcs.removeWorktree",
                    &input.cwd,
                    &format!("Could not verify the owning workspace thread: {error}"),
                )
            })?
            .ok_or_else(|| {
                vcs_error(
                    "vcs.removeWorktree",
                    &input.cwd,
                    "The owning workspace thread no longer exists.",
                )
            })?;
        if owner.kind == "panel"
            || owner.deleted_at.is_some()
            || owner
                .worktree_path
                .as_deref()
                .is_none_or(|path| !same_removal_path(Path::new(path), &input.path))
        {
            return Err(vcs_error(
                "vcs.removeWorktree",
                &input.cwd,
                "The supplied workspace thread does not own this worktree path.",
            ));
        }
        let project = repositories
            .get_project(owner.project_id.clone())
            .await
            .map_err(|error| {
                vcs_error(
                    "vcs.removeWorktree",
                    &input.cwd,
                    &format!("Could not verify the worktree project: {error}"),
                )
            })?
            .ok_or_else(|| {
                vcs_error(
                    "vcs.removeWorktree",
                    &input.cwd,
                    "The worktree project no longer exists.",
                )
            })?;
        if project.deleted_at.is_some()
            || !same_removal_path(Path::new(&project.workspace_root), &input.cwd)
        {
            return Err(vcs_error(
                "vcs.removeWorktree",
                &input.cwd,
                "The supplied repository path does not own this workspace thread.",
            ));
        }
        let project_threads = repositories
            .list_threads_by_project(owner.project_id)
            .await
            .map_err(|error| {
                vcs_error(
                    "vcs.removeWorktree",
                    &input.cwd,
                    &format!("Could not verify worktree path ownership: {error}"),
                )
            })?;
        for thread in project_threads.iter().filter(|thread| {
            thread.thread_id != owner_thread_id
                && thread.kind != "panel"
                && thread.deleted_at.is_none()
                && thread
                    .worktree_path
                    .as_deref()
                    .is_some_and(|path| same_removal_path(Path::new(path), &input.path))
        }) {
            let superseded = repositories
                .get_worktree_removal_receipt(thread.thread_id.clone())
                .await
                .map_err(|error| {
                    vcs_error(
                        "vcs.removeWorktree",
                        &input.cwd,
                        &format!("Could not verify competing worktree ownership: {error}"),
                    )
                })?
                .is_some_and(|receipt| receipt.state == "removed");
            if !superseded {
                return Err(vcs_error(
                    "vcs.removeWorktree",
                    &input.cwd,
                    "Another workspace thread now owns this path, so the stale removal was rejected.",
                ));
            }
        }

        if cancellation.is_cancelled() {
            return Err(vcs_error(
                "vcs.removeWorktree",
                &input.cwd,
                "Worktree removal was interrupted before safety admission.",
            ));
        }
        let terminal = self.terminal.as_ref().ok_or_else(|| {
            vcs_error(
                "vcs.removeWorktree",
                &input.cwd,
                "The production terminal safety coordinator is unavailable.",
            )
        })?;
        let terminal_removal = terminal
            .begin_worktree_removal(&input.thread_ids, &input.path)
            .await
            .map_err(|error| {
                vcs_error(
                    "vcs.removeWorktree",
                    &input.cwd,
                    &format!("Could not fence worktree terminal activity before removal: {error}"),
                )
            })?;

        let receipt = match existing_receipt {
            Some(receipt) => receipt,
            None => {
                let nonce = self
                    .repository
                    .prepare_worktree_removal_identity(&input.cwd, &input.path, &cancellation)
                    .await
                    .map_err(serialize_error)?;
                let receipt = repositories
                    .prepare_worktree_removal_receipt(WorktreeRemovalReceipt {
                        owner_thread_id: owner_thread_id.clone(),
                        project_cwd: display_path(&input.cwd),
                        worktree_path: display_path(&input.path),
                        identity_nonce: nonce.clone(),
                        state: "prepared".to_owned(),
                        created_at: String::new(),
                        updated_at: String::new(),
                    })
                    .await
                    .map_err(|error| {
                        vcs_error(
                            "vcs.removeWorktree",
                            &input.cwd,
                            &format!("Could not persist the worktree removal receipt: {error}"),
                        )
                    })?;
                ensure_receipt_matches_request(&receipt, &input)?;
                if receipt.identity_nonce != nonce || receipt.state != "prepared" {
                    return Err(vcs_error(
                        "vcs.removeWorktree",
                        &input.cwd,
                        "A conflicting durable worktree removal receipt already exists.",
                    ));
                }
                receipt
            }
        };

        let repository = Arc::clone(&self.repository);
        let cwd = input.cwd;
        let error_cwd = cwd.clone();
        let path = input.path;
        let force = input.force.unwrap_or(false);
        let operation = self
            .worktree_removal_tasks
            .spawn(async move {
                let _admission = admission;
                let _terminal_removal = terminal_removal;
                let safety_token = CancellationToken::new();
                repository
                    .remove_worktree_with_identity(
                        &cwd,
                        &path,
                        force,
                        &receipt.identity_nonce,
                        &safety_token,
                    )
                    .await
                    .map_err(serialize_error)?;
                repositories
                    .complete_worktree_removal_receipt(
                        receipt.owner_thread_id,
                        receipt.identity_nonce,
                    )
                    .await
                    .map_err(|error| {
                        vcs_error(
                            "vcs.removeWorktree",
                            &cwd,
                            &format!(
                                "Git removed the worktree, but its durable receipt failed: {error}"
                            ),
                        )
                    })?;
                Ok::<(), Value>(())
            })
            .map_err(|error| {
                vcs_error(
                    "vcs.removeWorktree",
                    &error_cwd,
                    &format!("The protected worktree removal task was not admitted: {error}"),
                )
            })?;
        operation.await.map_err(|error| {
            vcs_error(
                "vcs.removeWorktree",
                &error_cwd,
                &format!("The protected worktree removal task failed: {error}"),
            )
        })??;
        Ok(Value::Null)
    }

    fn status_stream(
        &self,
        request: RpcRequest,
        cancellation: CancellationToken,
    ) -> mpsc::Receiver<RpcStreamChunk> {
        let (sender, receiver) = mpsc::channel(STREAM_CAPACITY);
        let broadcaster = self.broadcaster.clone();
        let pull_requests = self.pull_requests.clone();
        let availability = self.availability_registry.clone();
        #[cfg(test)]
        let enrichment_test_hook = self.status_stream_enrichment_test_hook.clone();
        tokio::spawn(async move {
            let input = match decode::<CwdInput>(request.payload, "subscribeVcsStatus") {
                Ok(input) => input,
                Err(error) => {
                    let _ = sender.send(Err(error)).await;
                    return;
                }
            };
            let workspace_admission = match guard_git_path(availability.as_ref(), &input.cwd).await
            {
                Ok(admission) => admission,
                Err(error) => {
                    let _ = sender.send(Err(error)).await;
                    return;
                }
            };
            let operation_cancellation = cancellation.child_token();
            let loss = workspace_admission
                .as_ref()
                .map(WorkspaceAdmissionLease::loss_cancellation);
            let loss_token = loss
                .as_ref()
                .map(|loss| loss.cancellation_token())
                .unwrap_or_default();
            let mut subscription = match await_git_rpc_operation(
                workspace_admission.as_ref(),
                operation_cancellation.clone(),
                async {
                    broadcaster
                        .subscribe(input.cwd.clone(), operation_cancellation.clone())
                        .await
                        .map_err(serialize_error)
                },
            )
            .await
            {
                Ok(subscription) => subscription,
                Err(error) => {
                    let _ = sender.send(Err(error)).await;
                    return;
                }
            };
            let mut enrichment: Option<(CancellationToken, tokio::task::JoinHandle<()>)> = None;
            loop {
                tokio::select! {
                    biased;
                    () = loss_token.cancelled() => {
                        operation_cancellation.cancel();
                        if let Some(error) = loss.as_ref().and_then(|loss| loss.unavailable()) {
                            let _ = sender.send(Err(serialize_error(error))).await;
                        }
                        break;
                    }
                    _ = cancellation.cancelled() => break,
                    publication = subscription.recv_publication() => {
                        let publication = match publication {
                            Some(Ok(publication)) => publication,
                            Some(Err(error)) => {
                                let _ = sender.send(Err(serialize_error(error))).await;
                                break;
                            }
                            None => break,
                        };
                        stop_status_stream_enrichment(&mut enrichment).await;
                        let enrichment_remote = match &publication.value {
                            VcsStatusStreamEvent::Snapshot { remote, .. }
                            | VcsStatusStreamEvent::RemoteUpdated { remote } => remote.clone(),
                            VcsStatusStreamEvent::LocalUpdated { .. } => None,
                        };
                        if !send_status_stream_event(
                            &sender,
                            &broadcaster,
                            &publication.fence,
                            publication.value,
                        )
                        .await
                        {
                            break;
                        }
                        if let Some(remote) = enrichment_remote {
                            let task_cancellation = operation_cancellation.child_token();
                            let task_token = task_cancellation.clone();
                            let task_sender = sender.clone();
                            let task_broadcaster = broadcaster.clone();
                            let task_pull_requests = pull_requests.clone();
                            let task_cwd = input.cwd.clone();
                            let task_local = publication.local;
                            let task_fence = publication.fence;
                            #[cfg(test)]
                            let task_test_hook = enrichment_test_hook.clone();
                            let task = tokio::spawn(async move {
                                #[cfg(test)]
                                if let Some(hook) = &task_test_hook {
                                    hook.block(&task_token).await;
                                }
                                if task_token.is_cancelled() {
                                    return;
                                }
                                let mut enriched = remote.clone();
                                enrich_remote_pull_request(
                                    &task_pull_requests,
                                    &task_cwd,
                                    &task_local,
                                    &mut enriched,
                                    &task_token,
                                )
                                .await;
                                if task_token.is_cancelled() || enriched == remote {
                                    return;
                                }
                                let _ = send_status_stream_event(
                                    &task_sender,
                                    &task_broadcaster,
                                    &task_fence,
                                    VcsStatusStreamEvent::RemoteUpdated {
                                        remote: Some(enriched),
                                    },
                                )
                                .await;
                            });
                            enrichment = Some((task_cancellation, task));
                        }
                    }
                }
            }
            operation_cancellation.cancel();
            stop_status_stream_enrichment(&mut enrichment).await;
        });
        receiver
    }

    fn summary_stream(
        &self,
        request: RpcRequest,
        cancellation: CancellationToken,
    ) -> watch::Receiver<Option<RpcStreamChunk>> {
        let (sender, receiver) = watch::channel(None);
        let summary = self.summary.clone();
        let availability = self.availability_registry.clone();
        tokio::spawn(async move {
            let input = match decode::<CwdInput>(request.payload, "subscribeVcsStatusSummary") {
                Ok(input) => input,
                Err(error) => {
                    sender.send_replace(Some(Err(error)));
                    return;
                }
            };
            let workspace_admission = match guard_git_path(availability.as_ref(), &input.cwd).await
            {
                Ok(admission) => admission,
                Err(error) => {
                    sender.send_replace(Some(Err(error)));
                    return;
                }
            };
            let loss = workspace_admission
                .as_ref()
                .map(WorkspaceAdmissionLease::loss_cancellation);
            let loss_token = loss
                .as_ref()
                .map(|loss| loss.cancellation_token())
                .unwrap_or_default();
            let subscription = tokio::select! {
                biased;
                () = cancellation.cancelled() => return,
                result = summary.subscribe(input.cwd.clone()) => result,
            };
            let mut subscription = match subscription {
                Ok(subscription) => subscription,
                Err(error) => {
                    sender.send_replace(Some(Err(serialize_error(error))));
                    return;
                }
            };
            // A joining subscriber starts with the producer's value already marked seen.
            subscription.mark_changed();
            loop {
                let item = tokio::select! {
                    biased;
                    () = sender.closed() => return,
                    () = loss_token.cancelled() => {
                        if let Some(error) = loss.as_ref().and_then(|loss| loss.unavailable()) {
                            sender.send_replace(Some(Err(serialize_error(error))));
                        }
                        return;
                    }
                    () = cancellation.cancelled() => return,
                    changed = subscription.changed() => {
                        if changed.is_err() {
                            return;
                        }
                        subscription.borrow_and_update().clone()
                    }
                };
                let Some(item) = item else {
                    continue;
                };
                let chunk = match item {
                    Ok(summary) => serde_json::to_value(summary)
                        .map(|summary| vec![summary])
                        .map_err(|error| {
                            request_error("subscribeVcsStatusSummary", &error.to_string())
                        }),
                    Err(error) => Err(serialize_error(error)),
                };
                sender.send_replace(Some(chunk));
            }
        });
        receiver
    }

    fn stacked_action_stream(
        &self,
        request: RpcRequest,
        cancellation: CancellationToken,
    ) -> mpsc::Receiver<RpcStreamChunk> {
        let (sender, receiver) = mpsc::channel(STREAM_CAPACITY);
        let repository = Arc::clone(&self.repository);
        let broadcaster = self.broadcaster.clone();
        let pull_requests = self.pull_requests.clone();
        let created_request_observer = self.created_request_observer.clone();
        let availability = self.availability_registry.clone();
        let trace_diagnostics = self.trace_diagnostics.clone();
        tokio::spawn(async move {
            let started = tokio::time::Instant::now();
            let input = match decode::<StackedActionInput>(request.payload, "git.runStackedAction")
            {
                Ok(input) => input,
                Err(error) => {
                    let _ = sender.send(Err(error)).await;
                    return;
                }
            };
            let workspace_admission = match guard_git_path(availability.as_ref(), &input.cwd).await
            {
                Ok(admission) => admission,
                Err(error) => {
                    let _ = sender.send(Err(error)).await;
                    return;
                }
            };
            let operation_cancellation = cancellation.child_token();
            let phases = action_phases(&input.action, input.feature_branch.unwrap_or(false));
            let action_started = await_git_rpc_operation(
                workspace_admission.as_ref(),
                operation_cancellation.clone(),
                async {
                    send_event(
                        &sender,
                        json!({
                            "actionId": input.action_id, "cwd": input.cwd, "action": input.action,
                            "kind": "action_started", "phases": phases,
                        }),
                    )
                    .await
                    .map_err(|()| request_error("git.runStackedAction", "stream receiver closed"))
                },
            )
            .await;
            if let Err(error) = action_started {
                if is_workspace_unavailable(&error) {
                    let _ = sender.send(Err(error)).await;
                }
                return;
            }
            let mut safe_failure = None;
            let mut failed_phase = None;
            let result = match validate_stacked_action_input(&input) {
                Err(error) => Err(error),
                Ok(()) => {
                    await_git_rpc_operation(
                        workspace_admission.as_ref(),
                        operation_cancellation.clone(),
                        async {
                            let mutation = broadcaster.begin_mutation(&input.cwd).await;
                            let result = run_stacked_action(
                                &repository,
                                &pull_requests,
                                &input,
                                &operation_cancellation,
                            )
                            .await;
                            mutation.finish().await;
                            if let (Ok(result), Some(observer)) =
                                (&result, created_request_observer.as_deref())
                            {
                                notify_created_request(
                                    &repository,
                                    observer,
                                    &input.cwd,
                                    result,
                                    &operation_cancellation,
                                )
                                .await;
                            }
                            result.map_err(|failure| {
                                safe_failure = Some(failure.diagnostic);
                                failed_phase = failure.phase;
                                failure.wire
                            })
                        },
                    )
                    .await
                }
            };
            if let Err(error) = &result
                && let Some(store) = &trace_diagnostics
            {
                let message = if is_workspace_unavailable(error) {
                    "The workspace became unavailable during the Git action."
                } else {
                    safe_failure
                        .as_deref()
                        .unwrap_or("Git action could not complete.")
                };
                if let Err(error) = store.record_timed_failure(
                    "git.runStackedAction",
                    &json!({"detail": message}),
                    started.elapsed(),
                ) {
                    tracing::warn!(error_kind = ?error.kind(), "failed to persist Git action diagnostics");
                }
            }
            if crate::diagnostics::profiler::is_enabled() {
                record_stacked_action_profile(
                    &input,
                    &result,
                    failed_phase,
                    safe_failure.as_deref(),
                    operation_cancellation.is_cancelled(),
                    started.elapsed(),
                );
            }
            if let Err(error) = &result
                && is_workspace_unavailable(error)
            {
                let _ = sender.send(Err(error.clone())).await;
                return;
            }
            let event = match result {
                Ok(result) => json!({
                    "actionId": input.action_id, "cwd": input.cwd, "action": input.action,
                    "kind": "action_finished", "result": result,
                }),
                Err(error) => json!({
                    "actionId": input.action_id, "cwd": input.cwd, "action": input.action,
                    "kind": "action_failed", "phase": Value::Null,
                    "message": error.get("detail").and_then(Value::as_str).unwrap_or("Git action failed."),
                }),
            };
            if let Err(error) = await_git_rpc_operation(
                workspace_admission.as_ref(),
                operation_cancellation,
                async {
                    send_event(&sender, event).await.map_err(|()| {
                        request_error("git.runStackedAction", "stream receiver closed")
                    })
                },
            )
            .await
                && is_workspace_unavailable(&error)
            {
                let _ = sender.send(Err(error)).await;
            }
        });
        receiver
    }

    async fn resolve_pull_request(
        &self,
        input: &PullRequestInput,
        cancellation: &CancellationToken,
    ) -> Result<Value, Value> {
        let local = self
            .repository
            .local_status(&input.cwd, cancellation)
            .await
            .map_err(serialize_error)?;
        let provider = local_provider_kind(&local);
        let pull_request = self
            .pull_requests
            .resolve(
                ResolvePullRequestInput {
                    cwd: input.cwd.clone(),
                    provider,
                    reference: input.reference.clone(),
                },
                cancellation,
            )
            .await
            .map_err(serialize_error)?;
        serde_json::to_value(pull_request)
            .map_err(|error| request_error("git.resolvePullRequest", &error.to_string()))
    }

    async fn resolve_pull_request_preparation(
        &self,
        input: &PreparePullRequestInput,
        cancellation: &CancellationToken,
    ) -> Result<(Value, String), Value> {
        let PreparePullRequestMode::Local = &input.mode;
        let pull_request = self
            .resolve_pull_request(
                &PullRequestInput {
                    cwd: input.cwd.clone(),
                    reference: input.reference.clone(),
                },
                cancellation,
            )
            .await?;
        let branch = pull_request["headBranch"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        if branch.is_empty() {
            return Err(request_error(
                "git.preparePullRequestThread",
                "Pull request has no head branch.",
            ));
        }
        Ok((pull_request, branch))
    }

    async fn lookup_repository(
        &self,
        input: LookupRepositoryInput,
        cancellation: CancellationToken,
    ) -> RpcResult {
        let (command, args) = match input.provider.as_str() {
            "github" => (
                self.github_command.as_path(),
                vec![
                    "repo",
                    "view",
                    &input.repository,
                    "--json",
                    "nameWithOwner,url,sshUrl",
                ],
            ),
            "gitlab" => (
                self.gitlab_command.as_path(),
                vec!["repo", "view", &input.repository, "--output", "json"],
            ),
            provider => {
                return Err(source_control_error(
                    provider,
                    "lookupRepository",
                    "Provider repository lookup is unavailable.",
                ));
            }
        };
        run_provider_json(
            command,
            &args,
            input.cwd.as_deref(),
            cancellation,
            &input.provider,
            "lookupRepository",
        )
        .await
    }

    async fn clone_source_repository(
        &self,
        input: CloneRepositoryInput,
        admission: Option<RpcPermit>,
        cancellation: &CancellationToken,
    ) -> RpcResult {
        let remote_url = input
            .remote_url
            .or_else(|| input.repository.clone())
            .ok_or_else(|| {
                source_control_error(
                    input.provider.as_deref().unwrap_or("unknown"),
                    "cloneRepository",
                    "Enter a repository path or clone URL before cloning.",
                )
            })?;
        let destination = input.destination_path;
        let parent = destination.parent().ok_or_else(|| {
            request_error(
                "sourceControl.cloneRepository",
                "Destination has no parent directory.",
            )
        })?;
        let provider = input
            .provider
            .clone()
            .unwrap_or_else(|| "unknown".to_owned());
        // One validated folder name before the parent folder is created.
        let directory_name = destination.file_name().and_then(|value| value.to_str());
        let leaf = clone_destination_leaf(&remote_url, directory_name).map_err(|error| {
            source_control_error(&provider, "cloneRepository", &error.to_string())
        })?;
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            source_control_error(&provider, "cloneRepository", &error.to_string())
        })?;
        // Destination keys use the canonical parent, so a symlinked or `..` alias of a running
        // clone's folder joins it instead of meeting its half-written folder.
        let parent_dir = resolve_host_directory(parent, false)
            .await
            .map_err(|error| {
                source_control_error(&provider, "cloneRepository", &error.to_string())
            })?;
        let cwd = self
            .clone_operations
            .start_or_join(
                CloneRequest {
                    url: remote_url.clone(),
                    parent_dir,
                    leaf,
                    attach: false,
                    detach: false,
                },
                admission,
                cancellation,
            )
            .await
            .map_err(|error| {
                // The declared union is [SourceControlRepositoryError, EnvironmentRpcError], so
                // every runtime failure travels as a SourceControlRepositoryError.
                let detail = error["detail"]
                    .as_str()
                    .or_else(|| error["message"].as_str())
                    .unwrap_or("The clone failed.");
                source_control_error(&provider, "cloneRepository", detail)
            })?;
        Ok(json!({ "cwd": display_path(cwd), "remoteUrl": remote_url, "repository": Value::Null }))
    }

    async fn publish_repository(
        &self,
        input: PublishRepositoryInput,
        remote_name: String,
        visibility: &'static str,
        cancellation: &CancellationToken,
    ) -> RpcResult {
        let output = self
            .github_runner
            .run(
                ProcessRequest {
                    operation: "source-control.publishRepository.create".into(),
                    command: self.github_command.clone(),
                    args: vec![
                        "repo".into(),
                        "create".into(),
                        input.repository.clone().into(),
                        visibility.into(),
                        "--source".into(),
                        ".".into(),
                        format!("--remote={remote_name}").into(),
                    ],
                    cwd: input.cwd.clone(),
                    env: vec![],
                    stdin: None,
                    timeout: Duration::from_secs(60),
                    max_output_bytes: 128_000,
                    output_policy: OutputPolicy::Error,
                    append_truncation_marker: false,
                    allow_non_zero_exit: true,
                },
                cancellation,
            )
            .await
            .map_err(|error| {
                source_control_error(&input.provider, "publishRepository", &error.to_string())
            })?;
        if output.exit_code != 0 {
            return Err(source_control_error(
                &input.provider,
                "publishRepository",
                "GitHub CLI could not create the repository.",
            ));
        }
        let branch = self
            .repository
            .push_current_branch_to_remote(&input.cwd, &remote_name, cancellation)
            .await
            .map_err(serialize_error)?;
        let repository_url = format!("https://github.com/{}", input.repository);
        let remote_url = if input.protocol.as_deref() == Some("ssh") {
            format!("git@github.com:{}.git", input.repository)
        } else {
            format!("{repository_url}.git")
        };
        let ssh_url = format!("git@github.com:{}.git", input.repository);
        let upstream_branch = format!("{remote_name}/{branch}");
        Ok(json!({
            "repository": {
                "provider": "github",
                "nameWithOwner": input.repository,
                "url": repository_url,
                "sshUrl": ssh_url,
            },
            "remoteName": remote_name,
            "remoteUrl": remote_url,
            "branch": branch,
            "upstreamBranch": upstream_branch,
            "status": "pushed",
        }))
    }

    async fn open_in_editor(&self, payload: Value) -> RpcResult {
        open_in_editor_with(
            payload,
            &crate::production::editor_launch::EditorProbeEnv::from_process(),
            launch_editor,
        )
    }

    async fn guard_payload_cwd(
        &self,
        payload: &Value,
    ) -> Result<Option<WorkspaceAdmissionLease>, Value> {
        let Some(cwd) = payload.get("cwd").and_then(Value::as_str) else {
            return Ok(None);
        };
        guard_git_path(self.availability_registry.as_ref(), Path::new(cwd)).await
    }
}

async fn guard_git_path(
    registry: Option<&WorkspaceAvailabilityRegistry>,
    cwd: &Path,
) -> Result<Option<WorkspaceAdmissionLease>, Value> {
    let Some(registry) = registry else {
        return Ok(None);
    };
    registry
        .acquire_path_admission([cwd])
        .await
        .map(Some)
        .map_err(|error| {
            serde_json::to_value(error).expect("workspace unavailable error serializes")
        })
}

async fn await_git_rpc_operation<T>(
    admission: Option<&WorkspaceAdmissionLease>,
    operation_cancellation: CancellationToken,
    operation: impl Future<Output = Result<T, Value>>,
) -> Result<T, Value> {
    let Some(admission) = admission else {
        return operation.await;
    };
    let loss = admission.loss_cancellation();
    tokio::select! {
        biased;
        () = loss.cancelled() => {
            operation_cancellation.cancel();
            Err(serialize_error(
                loss.unavailable()
                    .expect("workspace loss cancellation retains its exact error"),
            ))
        }
        result = operation => result,
    }
}

async fn await_git_mutation_terminal(
    admission: Option<&WorkspaceAdmissionLease>,
    cancellation: CancellationToken,
    operation: impl Future<Output = RpcResult>,
) -> RpcResult {
    let Some(admission) = admission else {
        return operation.await;
    };
    let loss = admission.loss_cancellation();
    tokio::pin!(operation);
    tokio::select! {
        biased;
        () = loss.cancelled() => {
            cancellation.cancel();
            let _ = operation.await;
            Err(serialize_error(
                loss.unavailable()
                    .expect("workspace loss cancellation retains its exact error"),
            ))
        }
        result = &mut operation => result,
    }
}

fn is_workspace_unavailable(error: &Value) -> bool {
    error.get("_tag").and_then(Value::as_str) == Some("WorkspaceUnavailableError")
}

fn open_in_editor_with(
    payload: Value,
    env: &crate::production::editor_launch::EditorProbeEnv,
    launch: impl FnOnce(&EditorLaunchStrategy) -> std::io::Result<()>,
) -> RpcResult {
    use crate::production::editor_launch::{editor_definition, fallback_program, resolve_editor};

    let input: LaunchEditorInput = decode(payload, "shell.openInEditor")?;
    if input.editor == "file-manager" {
        return launch_file_manager(&input.cwd)
            .map(|()| Value::Null)
            .map_err(|error| {
                json!({
                    "_tag": "ExternalLauncherEditorSpawnError", "editor": input.editor,
                    "target": display_path(&input.cwd), "command": "open", "args": [],
                    "cause": error.to_string(),
                })
            });
    }
    let Some(definition) = editor_definition(&input.editor) else {
        return Err(
            json!({ "_tag": "ExternalLauncherUnknownEditorError", "editor": input.editor }),
        );
    };
    let target = display_path(&input.cwd);
    let resolved = resolve_editor(definition.id, env).or_else(|| {
        fallback_program(definition).map(|program| {
            crate::production::editor_launch::ResolvedEditor {
                id: definition.id,
                program,
                args: definition.args,
                flatpak_app_id: None,
            }
        })
    });
    // Unreachable while every catalog entry in `EDITOR_DEFINITIONS` has at least one `Path`
    // candidate: `fallback_program` always finds one, so `resolved` is always `Some`. A future
    // candidate-only editor (no `Path` candidate) should report
    // `ExternalLauncherCommandNotFoundError` here instead of reusing the unknown-editor error.
    let Some(resolved) = resolved else {
        return Err(
            json!({ "_tag": "ExternalLauncherUnknownEditorError", "editor": input.editor }),
        );
    };
    let args = resolved.args_for(&target);
    let strategy = editor_launch_strategy(&resolved.program, args.clone(), target.clone());
    launch(&strategy).map(|()| Value::Null).map_err(|error| {
        json!({
            "_tag": "ExternalLauncherEditorSpawnError", "editor": input.editor,
            "target": target, "command": resolved.program, "args": args,
            "cause": error.to_string(),
        })
    })
}

#[cfg(not(target_os = "linux"))]
fn launch_file_manager(target: &Path) -> std::io::Result<()> {
    open::that_detached(target)
}

#[cfg(target_os = "linux")]
fn launch_file_manager(target: &Path) -> std::io::Result<()> {
    let mut last_error = None;
    for mut command in open::commands(target) {
        crate::process::isolate_appimage_environment(&mut command);
        match launch_file_manager_command(command) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.expect("open::commands provides at least one launcher"))
}

#[cfg(target_os = "linux")]
fn launch_file_manager_command(mut command: std::process::Command) -> std::io::Result<()> {
    use std::os::unix::process::CommandExt;

    command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    let (spawned, result) = std::sync::mpsc::sync_channel(1);
    // Create the owner before spawning: thread creation failure cannot orphan a
    // child. The caller waits only for spawn, while this thread always reaps it.
    std::thread::Builder::new()
        .name("file-manager-reaper".to_owned())
        .spawn(move || match command.spawn() {
            Ok(mut child) => {
                let _ = spawned.send(Ok(()));
                if let Err(error) = child.wait() {
                    tracing::warn!(%error, "could not reap file-manager launcher");
                }
            }
            Err(error) => {
                let _ = spawned.send(Err(error));
            }
        })?;
    result.recv().map_err(std::io::Error::other)?
}

fn launch_editor(strategy: &EditorLaunchStrategy) -> std::io::Result<()> {
    match strategy {
        #[cfg(windows)]
        EditorLaunchStrategy::ShellAssociation {
            application,
            target,
        } => open::with_detached(target, application.clone()),
        #[cfg(not(windows))]
        EditorLaunchStrategy::Process { command, args } => {
            let mut command = Command::new(command);
            command
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(false);
            crate::process::isolate_appimage_environment(&mut command);
            command.spawn().map(|_| ())
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
enum EditorLaunchStrategy {
    #[cfg(windows)]
    ShellAssociation { application: String, target: String },
    #[cfg(not(windows))]
    Process { command: String, args: Vec<String> },
}

fn editor_launch_strategy(
    command: &str,
    args: Vec<String>,
    target: String,
) -> EditorLaunchStrategy {
    #[cfg(windows)]
    {
        let _ = args;
        EditorLaunchStrategy::ShellAssociation {
            application: command.to_owned(),
            target,
        }
    }
    #[cfg(not(windows))]
    {
        let _ = target;
        EditorLaunchStrategy::Process {
            command: command.to_owned(),
            args,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiscoverSourceControlInput {
    /// Settings → Source Control and its Rescan: record the authenticated hosts found.
    #[serde(default)]
    record_hosts: bool,
}
#[derive(Deserialize)]
struct CwdInput {
    cwd: PathBuf,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListRefsInput {
    cwd: PathBuf,
    query: Option<String>,
    cursor: Option<usize>,
    limit: Option<usize>,
    include_matching_remote_refs: Option<bool>,
    ref_kind: Option<String>,
}
#[derive(Deserialize)]
struct ListCommitsInput {
    cwd: PathBuf,
    limit: Option<usize>,
    cursor: Option<usize>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateWorktree {
    cwd: PathBuf,
    ref_name: String,
    new_ref_name: Option<String>,
    base_ref_name: Option<String>,
    path: Option<PathBuf>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoveWorktree {
    cwd: PathBuf,
    path: PathBuf,
    force: Option<bool>,
    #[serde(default)]
    owner_thread_id: Option<String>,
    #[serde(default)]
    thread_ids: Vec<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CloneInput {
    url: String,
    parent_dir: PathBuf,
    directory_name: Option<String>,
    /// Join-only; sent only by clients that saw `vcsCloneReattach`.
    #[serde(default)]
    attach: bool,
    /// The caller leaving does not stop a clone it started.
    #[serde(default)]
    detach: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CancelCloneInput {
    url: String,
    parent_dir: PathBuf,
    directory_name: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateRefInput {
    cwd: PathBuf,
    ref_name: String,
    switch_ref: Option<bool>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SwitchRefInput {
    cwd: PathBuf,
    ref_name: String,
}
#[derive(Deserialize)]
struct InitInput {
    cwd: PathBuf,
    kind: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FilePathsInput {
    cwd: PathBuf,
    file_paths: Vec<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommitMessageInput {
    cwd: PathBuf,
    file_paths: Option<Vec<String>>,
}
#[derive(Deserialize)]
struct PullRequestInput {
    cwd: PathBuf,
    reference: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct PreparePullRequestInput {
    cwd: PathBuf,
    reference: String,
    mode: PreparePullRequestMode,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum PreparePullRequestMode {
    Local,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StackedActionInput {
    action_id: String,
    cwd: PathBuf,
    action: String,
    commit_message: Option<String>,
    file_paths: Option<Vec<String>>,
    feature_branch: Option<bool>,
    commit_staged_index_as_is: Option<bool>,
    pull_request_title: Option<String>,
    pull_request_body: Option<String>,
    pull_request_base_branch: Option<String>,
    pull_request_head_branch: Option<String>,
    pull_request_options: Option<CreatePullRequestOptions>,
}

#[derive(Debug, Serialize)]
struct StackedActionResult {
    action: String,
    branch: Value,
    commit: Value,
    push: Value,
    pr: PullRequestStep,
    toast: Value,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum PullRequestStep {
    SkippedNotRequested,
    Created(PullRequestStepDetails),
    OpenedExisting(PullRequestStepDetails),
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PullRequestStepDetails {
    url: String,
    number: u64,
    base_branch: String,
    head_branch: String,
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    warning: Option<String>,
}
#[derive(Deserialize)]
struct LookupRepositoryInput {
    provider: String,
    repository: String,
    cwd: Option<PathBuf>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CloneRepositoryInput {
    provider: Option<String>,
    repository: Option<String>,
    remote_url: Option<String>,
    destination_path: PathBuf,
    #[allow(dead_code)]
    protocol: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublishRepositoryInput {
    cwd: PathBuf,
    provider: String,
    repository: String,
    visibility: String,
    remote_name: Option<String>,
    protocol: Option<String>,
}
#[derive(Deserialize)]
struct LaunchEditorInput {
    cwd: PathBuf,
    editor: String,
}

/// Options the provider cannot honor are refused before anything is committed or pushed.
fn validate_options_for_provider(
    options: &CreatePullRequestOptions,
    provider: ProviderKind,
) -> Result<(), &'static str> {
    if options.is_empty() {
        return Ok(());
    }
    match provider {
        ProviderKind::Github
            if options.remove_source_branch.is_some() || options.squash.is_some() =>
        {
            Err("Merge options apply only to GitLab merge requests.")
        }
        ProviderKind::Github => Ok(()),
        ProviderKind::Gitlab => {
            let numeric = |value: &String| value.parse::<u64>().is_ok();
            if options
                .assignees
                .iter()
                .chain(&options.reviewers)
                .all(numeric)
                && options
                    .milestone
                    .as_ref()
                    .is_none_or(|milestone| numeric(&milestone.id))
            {
                Ok(())
            } else {
                Err("Choose GitLab people and milestones from the project's lists.")
            }
        }
        _ => Err("Create options are available for GitHub and GitLab only."),
    }
}

fn validate_stacked_action_input(input: &StackedActionInput) -> Result<(), Value> {
    if !matches!(
        input.action.as_str(),
        "commit" | "push" | "create_pr" | "commit_push" | "commit_push_pr"
    ) {
        return Err(request_error(
            "git.runStackedAction",
            &format!("Unsupported Git action '{}'.", input.action),
        ));
    }
    if input.feature_branch.unwrap_or(false)
        && !matches!(
            input.action.as_str(),
            "commit" | "commit_push" | "commit_push_pr"
        )
    {
        return Err(request_error(
            "git.runStackedAction",
            "Feature-branch checkout is only supported for commit actions.",
        ));
    }
    if (input.pull_request_title.is_some()
        || input.pull_request_body.is_some()
        || input.pull_request_base_branch.is_some()
        || input.pull_request_head_branch.is_some()
        || input.pull_request_options.is_some())
        && !matches!(input.action.as_str(), "create_pr" | "commit_push_pr")
    {
        return Err(request_error(
            "git.runStackedAction",
            "A pull request title, body, source or target branch applies only to pull request actions.",
        ));
    }
    if let Some(options) = &input.pull_request_options {
        let valid = |value: &str| !value.trim().is_empty() && value.chars().count() <= 255;
        if options.assignees.len() > 20
            || options.reviewers.len() > 20
            || options.labels.len() > 50
            || !options
                .assignees
                .iter()
                .chain(&options.reviewers)
                .chain(&options.labels)
                .all(|value| valid(value))
            || options
                .milestone
                .as_ref()
                .is_some_and(|milestone| !valid(&milestone.id) || !valid(&milestone.title))
        {
            return Err(request_error(
                "git.runStackedAction",
                "Pull request options exceed the supported limits.",
            ));
        }
    }
    if matches!(input.action.as_str(), "create_pr" | "commit_push_pr")
        && input
            .pull_request_base_branch
            .as_deref()
            .is_none_or(|branch| branch.trim().is_empty())
    {
        return Err(request_error(
            "git.runStackedAction",
            "Select a target branch before creating a pull request.",
        ));
    }
    Ok(())
}

/// Keeps diagnostic facts separate from the unchanged client error envelope.
struct StackedActionFailure {
    wire: Value,
    diagnostic: Box<str>,
    /// The step that failed (`branch`, `commit`, `push` or `pr`), or `None` before the first.
    phase: Option<&'static str>,
}

impl std::fmt::Debug for StackedActionFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StackedActionFailure")
            .field("diagnostic", &self.diagnostic)
            .field("phase", &self.phase)
            .finish_non_exhaustive()
    }
}

impl From<Value> for StackedActionFailure {
    fn from(wire: Value) -> Self {
        Self {
            wire,
            diagnostic: "Git action could not complete.".into(),
            phase: None,
        }
    }
}

impl From<GitCommandError> for StackedActionFailure {
    fn from(error: GitCommandError) -> Self {
        let diagnostic = error
            .safe_failure_message()
            .unwrap_or_else(|| "Git could not complete the requested operation.".to_owned())
            .into();
        Self {
            wire: serialize_error(error),
            diagnostic,
            phase: None,
        }
    }
}

impl From<crate::source_control::SourceControlProviderError> for StackedActionFailure {
    fn from(error: crate::source_control::SourceControlProviderError) -> Self {
        let diagnostic = error
            .safe_failure_message()
            .unwrap_or_else(|| "The source-control operation could not complete.".to_owned())
            .into();
        Self {
            wire: serialize_error(error),
            diagnostic,
            phase: None,
        }
    }
}

/// Reports the action's terminal outcome to the profiler (#158). Only the failing step, the
/// error's class and its operation tag leave here: never the wire detail, which can quote a
/// commit message or hook output.
fn record_stacked_action_profile(
    input: &StackedActionInput,
    result: &Result<StackedActionResult, Value>,
    phase: Option<&'static str>,
    safe_failure: Option<&str>,
    cancelled: bool,
    duration: Duration,
) {
    use crate::diagnostics::profiler::git_actions::{
        FailureReason, StackedActionFailure as ProfiledFailure, record_stacked_action_outcome,
    };
    let outcome = match result {
        Ok(_) => Ok(()),
        Err(error) => {
            let reason = FailureReason::from_tag(error.get("_tag").and_then(Value::as_str));
            Err(ProfiledFailure {
                phase,
                reason: if cancelled && reason != FailureReason::WorkspaceUnavailable {
                    FailureReason::Cancelled
                } else {
                    reason
                },
                operation: error.get("operation").and_then(Value::as_str),
                message: safe_failure.unwrap_or("Git action could not complete."),
            })
        }
    };
    record_stacked_action_outcome(
        &input.action,
        input.feature_branch.unwrap_or(false),
        &input.cwd.to_string_lossy(),
        outcome,
        duration,
    );
}

/// Runs the action and names the step a failure happened in.
async fn run_stacked_action(
    repository: &GitRepository,
    pull_requests: &PullRequestService,
    input: &StackedActionInput,
    cancellation: &CancellationToken,
) -> Result<StackedActionResult, StackedActionFailure> {
    let mut phase = None;
    run_stacked_action_steps(repository, pull_requests, input, cancellation, &mut phase)
        .await
        .map_err(|failure| StackedActionFailure { phase, ..failure })
}

async fn run_stacked_action_steps(
    repository: &GitRepository,
    pull_requests: &PullRequestService,
    input: &StackedActionInput,
    cancellation: &CancellationToken,
    phase: &mut Option<&'static str>,
) -> Result<StackedActionResult, StackedActionFailure> {
    validate_stacked_action_input(input)?;
    let wants_commit = matches!(
        input.action.as_str(),
        "commit" | "commit_push" | "commit_push_pr"
    );
    let wants_pr = matches!(input.action.as_str(), "create_pr" | "commit_push_pr");
    let feature_branch = input.feature_branch.unwrap_or(false);
    let commit_staged_index_as_is = input.commit_staged_index_as_is.unwrap_or(false);
    let base_branch = input.pull_request_base_branch.as_deref().map(str::trim);
    let requested_head = input.pull_request_head_branch.as_deref().map(str::trim);
    for (branch, role) in [(base_branch, "target"), (requested_head, "source")] {
        let Some(branch) = branch else { continue };
        // Do not let Git expand checkout expressions such as @{-1}.
        if branch.is_empty() || branch.starts_with('-') || branch.contains("@{") {
            return Err(request_error(
                "git.runStackedAction",
                &format!("Select a valid {role} branch."),
            )
            .into());
        }
        repository
            .run(
                "git.runStackedAction",
                &input.cwd,
                &[
                    "check-ref-format".to_owned(),
                    "--branch".to_owned(),
                    branch.to_owned(),
                ],
                cancellation,
            )
            .await
            .map_err(|error| {
                if error
                    .diagnostics
                    .as_ref()
                    .and_then(|diagnostics| diagnostics.exit_code)
                    .is_some_and(|code| code > 0)
                {
                    request_error(
                        "git.runStackedAction",
                        &format!("Select a valid {role} branch."),
                    )
                    .into()
                } else {
                    StackedActionFailure::from(error)
                }
            })?;
    }
    let initial_local = repository
        .local_status(&input.cwd, cancellation)
        .await
        .map_err(StackedActionFailure::from)?;
    if input.action == "commit_push_pr"
        && requested_head.is_some_and(|head| Some(head) != initial_local.ref_name.as_deref())
    {
        return Err(request_error(
            "git.runStackedAction",
            "Commit and create a pull request uses the current branch. Select the current source branch.",
        ).into());
    }
    let selected_head = requested_head.filter(|_| input.action == "create_pr");
    let selected_head_is_local = if let Some(head) = selected_head {
        let local_ref = format!("refs/heads/{head}");
        let remote_ref = format!("refs/remotes/origin/{head}");
        let refs = repository
            .run(
                "git.runStackedAction.sourceBranch",
                &input.cwd,
                &[
                    "for-each-ref".to_owned(),
                    "--format=%(refname)".to_owned(),
                    local_ref.clone(),
                    remote_ref.clone(),
                ],
                cancellation,
            )
            .await
            .map_err(StackedActionFailure::from)?;
        let is_local = refs.stdout.lines().any(|reference| reference == local_ref);
        if !is_local && !refs.stdout.lines().any(|reference| reference == remote_ref) {
            return Err(request_error(
                "git.runStackedAction",
                "Select an existing local or origin source branch. Fetch to refresh remote branches.",
            ).into());
        }
        Some(is_local)
    } else {
        None
    };
    if wants_pr
        && !feature_branch
        && base_branch == selected_head.or(initial_local.ref_name.as_deref())
    {
        return Err(request_error(
            "git.runStackedAction",
            "Select a target branch different from the source branch.",
        )
        .into());
    }
    if feature_branch && !initial_local.has_working_tree_changes {
        return Err(request_error(
            "git.runStackedAction",
            "Cannot create a feature branch because there are no changes to commit.",
        )
        .into());
    }
    if input.action == "create_pr"
        && initial_local.has_working_tree_changes
        && selected_head.is_none_or(|head| Some(head) == initial_local.ref_name.as_deref())
    {
        return Err(request_error(
            "git.runStackedAction",
            "Commit local changes before creating a PR.",
        )
        .into());
    }
    if !feature_branch
        && (wants_pr || input.action == "push")
        && selected_head.is_none()
        && initial_local.ref_name.is_none()
    {
        let detail = if wants_pr {
            "Cannot create a pull request from detached HEAD."
        } else {
            "Cannot push from detached HEAD."
        };
        return Err(request_error("git.runStackedAction", detail).into());
    }
    // Resolve the provider before any branch, commit or push, so an unidentified
    // host never publishes the branch and then fails to create the request.
    let pull_request_provider = if wants_pr {
        Some(pull_request_provider(repository, &input.cwd, &initial_local, cancellation).await?)
    } else {
        None
    };
    let options = input.pull_request_options.clone().unwrap_or_default();
    if let Some(provider) = pull_request_provider
        && let Err(detail) = validate_options_for_provider(&options, provider)
    {
        return Err(request_error("git.runStackedAction", detail).into());
    }
    let wants_push = if input.action == "create_pr" {
        if let Some(is_local) = selected_head_is_local {
            is_local
        } else {
            let remote = repository
                .remote_status(&input.cwd, cancellation)
                .await
                .map_err(StackedActionFailure::from)?;
            remote.is_none_or(|status| !status.has_upstream || status.ahead_count > 0)
        }
    } else {
        matches!(
            input.action.as_str(),
            "push" | "commit_push" | "commit_push_pr"
        )
    };
    let resolved_message = if wants_commit {
        Some(match input.commit_message.as_deref().map(str::trim) {
            Some(message) if !message.is_empty() => message.to_owned(),
            _ => {
                let context = repository
                    .commit_context(&input.cwd, cancellation)
                    .await
                    .map_err(StackedActionFailure::from)?;
                let message = summarize_commit_context(&context, input.file_paths.as_deref());
                if message.is_empty() {
                    "Update working tree".to_owned()
                } else {
                    message
                }
            }
        })
    } else {
        None
    };
    let branch = if feature_branch {
        *phase = Some("branch");
        let subject = resolved_message
            .as_deref()
            .and_then(|message| message.lines().next())
            .unwrap_or("update");
        let preferred = sanitize_feature_branch_name(subject);
        let existing = local_branch_names(repository, &input.cwd, cancellation).await?;
        let name = resolve_feature_branch_name(&existing, &preferred);
        if base_branch == Some(name.as_str()) {
            return Err(request_error(
                "git.runStackedAction",
                "Select a target branch different from the source branch.",
            )
            .into());
        }
        repository
            .create_ref(&input.cwd, &name, true, cancellation)
            .await
            .map_err(StackedActionFailure::from)?;
        json!({ "status": "created", "name": name })
    } else {
        json!({ "status": "skipped_not_requested" })
    };
    let commit = if wants_commit {
        *phase = Some("commit");
        let message = resolved_message.as_deref().unwrap_or("Update working tree");
        let sha = repository
            .commit(
                &input.cwd,
                message,
                input.file_paths.as_deref(),
                commit_staged_index_as_is,
                cancellation,
            )
            .await
            .map_err(StackedActionFailure::from)?;
        sha.map_or_else(
            || json!({ "status": "skipped_no_changes" }),
            |sha| json!({ "status": "created", "commitSha": sha, "subject": message.lines().next().unwrap_or(message) }),
        )
    } else {
        json!({ "status": "skipped_not_requested" })
    };
    let push = if wants_push {
        *phase = Some("push");
        let branch = if let Some(head) = selected_head {
            let mut args = vec![
                "push".to_owned(),
                "--no-force".to_owned(),
                "--no-follow-tags".to_owned(),
                "--no-tags".to_owned(),
            ];
            if Some(head) == initial_local.ref_name.as_deref() {
                // Ref resolution can hide a configured upstream outside the
                // fetch mapping of a single-branch clone. Preserve its config.
                let upstream_remote = repository
                    .run(
                        "git.runStackedAction.sourceUpstream",
                        &input.cwd,
                        &[
                            "config".to_owned(),
                            "--default".to_owned(),
                            String::new(),
                            "--get".to_owned(),
                            format!("branch.{head}.remote"),
                        ],
                        cancellation,
                    )
                    .await
                    .map_err(StackedActionFailure::from)?;
                if upstream_remote.stdout.trim().is_empty() {
                    args.push("--set-upstream".to_owned());
                }
            }
            args.extend([
                "--".to_owned(),
                "origin".to_owned(),
                format!("refs/heads/{head}:refs/heads/{head}"),
            ]);
            repository
                .for_bounded_transfer()
                .run(
                    "git.runStackedAction.pushSourceBranch",
                    &input.cwd,
                    &args,
                    cancellation,
                )
                .await
                .map_err(StackedActionFailure::from)?;
            head.to_owned()
        } else {
            repository
                .push_current_branch(&input.cwd, cancellation)
                .await
                .map_err(StackedActionFailure::from)?
        };
        json!({ "status": "pushed", "branch": branch })
    } else {
        json!({ "status": "skipped_not_requested" })
    };
    let pull_request = if let Some(provider) = pull_request_provider {
        *phase = Some("pr");
        let current_local = repository
            .local_status(&input.cwd, cancellation)
            .await
            .map_err(StackedActionFailure::from)?;
        let head_branch = selected_head
            .or(current_local.ref_name.as_deref())
            .ok_or_else(|| {
                request_error(
                    "git.runStackedAction",
                    "Cannot create a pull request from detached HEAD.",
                )
            })?;
        if let Some(existing) = resolve_open_pull_request(
            pull_requests,
            &input.cwd,
            provider,
            head_branch,
            cancellation,
        )
        .await
        {
            let mut step = resolved_pull_request_step(&existing);
            if !options.is_empty() {
                let noun = if provider == ProviderKind::Gitlab {
                    "merge request"
                } else {
                    "pull request"
                };
                step.warning = Some(format!(
                    "An open {noun} already existed for this branch; the new options were not applied."
                ));
            }
            PullRequestStep::OpenedExisting(step)
        } else {
            let reviewed_title = input
                .pull_request_title
                .as_deref()
                .map(str::trim)
                .filter(|title| !title.is_empty())
                .map(str::to_owned);
            let title = match reviewed_title.or_else(|| {
                resolved_message
                    .as_deref()
                    .and_then(|message| message.lines().next())
                    .map(str::trim)
                    .filter(|title| !title.is_empty())
                    .map(str::to_owned)
            }) {
                Some(title) => title,
                None if current_local.ref_name.as_deref() != Some(head_branch) => {
                    format!("Update {head_branch}")
                }
                None => repository
                    .list_commits(&input.cwd, 1, 0, cancellation)
                    .await
                    .map_err(StackedActionFailure::from)?
                    .commits
                    .into_iter()
                    .next()
                    .map_or_else(|| format!("Update {head_branch}"), |commit| commit.subject),
            };
            let base_branch = base_branch
                .ok_or_else(|| {
                    request_error(
                        "git.runStackedAction",
                        "Select a target branch before creating a pull request.",
                    )
                })?
                .to_owned();
            let created = pull_requests
                .create_with_options(
                    CreatePullRequestInput {
                        cwd: input.cwd.clone(),
                        provider,
                        base_branch,
                        head_branch: head_branch.to_owned(),
                        title,
                        body: input.pull_request_body.clone().unwrap_or_default(),
                    },
                    &options,
                    cancellation,
                )
                .await
                .map_err(StackedActionFailure::from)?;
            let mut step = resolved_pull_request_step(&created.pull_request);
            step.warning = created.warning;
            PullRequestStep::Created(step)
        }
    } else {
        PullRequestStep::SkippedNotRequested
    };
    Ok(StackedActionResult {
        action: input.action.clone(),
        branch,
        commit,
        push,
        pr: pull_request,
        toast: json!({ "title": "Git action completed", "cta": { "kind": "none" } }),
    })
}

/// A request created here bypasses `pullRequests.runAction`, so tell the Pull
/// Requests service; its cached Open total would otherwise miss it for up to 30 s.
async fn notify_created_request(
    repository: &GitRepository,
    observer: &dyn CreatedRequestObserver,
    cwd: &Path,
    result: &StackedActionResult,
    cancellation: &CancellationToken,
) {
    if !matches!(result.pr, PullRequestStep::Created(_)) {
        return;
    }
    match repository.origin_url(cwd, cancellation).await {
        Ok(Some(remote)) => observer.request_created(cwd, &remote),
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(%error, "could not read origin to invalidate created request totals")
        }
    }
}

/// The request's provider from the pre-publication status, or an actionable error
/// that states nothing was published.
async fn pull_request_provider(
    repository: &GitRepository,
    cwd: &std::path::Path,
    local: &VcsStatusLocalResult,
    cancellation: &CancellationToken,
) -> Result<ProviderKind, Value> {
    let provider = local_provider_kind(local);
    if provider != ProviderKind::Unknown {
        return Ok(provider);
    }
    if !local.has_primary_remote {
        return Err(request_error(
            "git.runStackedAction",
            "Nothing was published. Add an origin remote to create a pull request.",
        ));
    }
    let host = repository
        .origin_host(cwd, cancellation)
        .await
        .map_err(serialize_error)?
        .unwrap_or_else(|| "this repository's host".to_owned());
    Err(request_error(
        "git.runStackedAction",
        &format!(
            "Nothing was published. BiBCode hasn't identified {host} yet. Open Pull Requests for this project or run Rescan in Settings → Source Control, then try again."
        ),
    ))
}

fn local_provider_kind(local: &VcsStatusLocalResult) -> ProviderKind {
    local
        .source_control_provider
        .as_ref()
        .map_or(ProviderKind::Unknown, |provider| provider.kind.into())
}

async fn resolve_open_pull_request(
    pull_requests: &PullRequestService,
    cwd: &std::path::Path,
    provider: ProviderKind,
    reference: &str,
    cancellation: &CancellationToken,
) -> Option<ResolvedPullRequest> {
    if provider == ProviderKind::Unknown {
        return None;
    }
    pull_requests
        .resolve_current(
            ResolvePullRequestInput {
                cwd: cwd.to_path_buf(),
                provider,
                reference: reference.to_owned(),
            },
            cancellation,
        )
        .await
        .ok()
        .filter(|pull_request| pull_request.state == ChangeRequestState::Open)
}

fn resolved_pull_request_step(pull_request: &ResolvedPullRequest) -> PullRequestStepDetails {
    PullRequestStepDetails {
        url: pull_request.url.clone(),
        number: pull_request.number,
        base_branch: pull_request.base_branch.clone(),
        head_branch: pull_request.head_branch.clone(),
        title: pull_request.title.clone(),
        warning: None,
    }
}

async fn finish_fenced_enrichment<T>(
    broadcaster: &StatusBroadcaster,
    fence: StatusReadFence,
    enrichment: impl Future<Output = T>,
) -> Result<T, GitCommandError> {
    let value = enrichment.await;
    broadcaster.publish_if_fence_current(&fence, || value)
}

async fn send_status_stream_event(
    sender: &mpsc::Sender<RpcStreamChunk>,
    broadcaster: &StatusBroadcaster,
    fence: &StatusReadFence,
    event: VcsStatusStreamEvent,
) -> bool {
    let chunk = serde_json::to_value(event)
        .map(|event| vec![event])
        .map_err(|error| request_error("subscribeVcsStatus", &error.to_string()));
    let permit = match sender.reserve().await {
        Ok(permit) => permit,
        Err(_) => return false,
    };
    let _ = broadcaster.publish_if_fence_current(fence, || permit.send(chunk));
    true
}

async fn stop_status_stream_enrichment(
    enrichment: &mut Option<(CancellationToken, tokio::task::JoinHandle<()>)>,
) {
    if let Some((cancellation, task)) = enrichment.take() {
        cancellation.cancel();
        task.abort();
        let _ = task.await;
    }
}

async fn enrich_remote_pull_request(
    pull_requests: &PullRequestService,
    cwd: &std::path::Path,
    local: &VcsStatusLocalResult,
    remote: &mut VcsStatusRemoteResult,
    cancellation: &CancellationToken,
) {
    let Some(reference) = local.ref_name.as_deref() else {
        return;
    };
    let provider = local_provider_kind(local);
    if provider == ProviderKind::Unknown {
        return;
    }
    // Shared with the status stream and other refreshes for a few seconds;
    // see `PullRequestService::resolve_current_optional_shared`.
    let Some(pull_request) = pull_requests
        .resolve_current_optional_shared(
            ResolvePullRequestInput {
                cwd: cwd.to_path_buf(),
                provider,
                reference: reference.to_owned(),
            },
            cancellation,
        )
        .await
        .ok()
        .flatten()
        .filter(|pull_request| pull_request.state == ChangeRequestState::Open)
    else {
        return;
    };
    remote.pr = Some(ChangeRequest {
        number: pull_request.number,
        title: pull_request.title,
        url: pull_request.url,
        base_ref: pull_request.base_branch,
        head_ref: pull_request.head_branch,
        state: "open".to_owned(),
    });
}

async fn local_branch_names(
    repository: &GitRepository,
    cwd: &std::path::Path,
    cancellation: &CancellationToken,
) -> Result<Vec<String>, Value> {
    repository
        .local_branch_names(cwd, cancellation)
        .await
        .map_err(serialize_error)
}

fn sanitize_branch_fragment(raw: &str) -> String {
    let mut fragment = String::with_capacity(raw.len().min(64));
    for character in raw.trim().to_lowercase().chars() {
        if matches!(character, '\'' | '"' | '`') {
            continue;
        }
        if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
            if character != '-' || !fragment.ends_with('-') {
                fragment.push(character);
            }
        } else if character == '/' {
            if !fragment.ends_with('/') {
                fragment.push('/');
            }
        } else if !fragment.ends_with('-') {
            fragment.push('-');
        }
        if fragment.len() >= 64 {
            fragment.truncate(64);
            break;
        }
    }
    let fragment = fragment
        .trim_matches(|character| matches!(character, '.' | '/' | '_' | '-'))
        .to_owned();
    if fragment.is_empty() {
        "update".to_owned()
    } else {
        fragment
    }
}

fn sanitize_feature_branch_name(raw: &str) -> String {
    let fragment = sanitize_branch_fragment(raw);
    if fragment.starts_with("feature/") {
        fragment
    } else {
        format!("feature/{fragment}")
    }
}

fn resolve_feature_branch_name(existing: &[String], preferred: &str) -> String {
    if !existing
        .iter()
        .any(|name| name.eq_ignore_ascii_case(preferred))
    {
        return preferred.to_owned();
    }
    let mut suffix = 2;
    loop {
        let candidate = format!("{preferred}-{suffix}");
        if !existing
            .iter()
            .any(|name| name.eq_ignore_ascii_case(&candidate))
        {
            return candidate;
        }
        suffix += 1;
    }
}

fn action_phases(action: &str, feature_branch: bool) -> Vec<&'static str> {
    let mut phases = if feature_branch {
        vec!["branch"]
    } else {
        vec![]
    };
    phases.extend(match action {
        "commit" => vec!["commit"],
        "push" => vec!["push"],
        "create_pr" => vec!["push", "pr"],
        "commit_push" => vec!["commit", "push"],
        "commit_push_pr" => vec!["commit", "push", "pr"],
        _ => vec![],
    });
    phases
}

async fn send_event(sender: &mpsc::Sender<RpcStreamChunk>, event: Value) -> Result<(), ()> {
    sender.send(Ok(vec![event])).await.map_err(|_| ())
}

async fn run_provider_json(
    command: &Path,
    args: &[&str],
    cwd: Option<&std::path::Path>,
    cancellation: CancellationToken,
    provider: &str,
    operation: &str,
) -> RpcResult {
    let output = ProcessRunner
        .run(
            ProcessRequest {
                operation: format!("source-control.{operation}"),
                command: command.to_path_buf(),
                args: args.iter().map(OsString::from).collect(),
                cwd: cwd
                    .unwrap_or_else(|| std::path::Path::new("."))
                    .to_path_buf(),
                env: vec![],
                stdin: None,
                timeout: Duration::from_secs(30),
                max_output_bytes: 256_000,
                output_policy: OutputPolicy::Error,
                append_truncation_marker: false,
                allow_non_zero_exit: true,
            },
            &cancellation,
        )
        .await
        .map_err(|error| source_control_error(provider, operation, &error.to_string()))?;
    if output.exit_code != 0 {
        return Err(source_control_error(
            provider,
            operation,
            "Provider command failed.",
        ));
    }
    serde_json::from_str(&output.stdout)
        .map_err(|error| source_control_error(provider, operation, &error.to_string()))
}

fn decode<T: for<'de> Deserialize<'de>>(payload: Value, method: &str) -> Result<T, Value> {
    serde_json::from_value(payload).map_err(|error| request_error(method, &error.to_string()))
}
fn encode_result<T: serde::Serialize, E: serde::Serialize>(result: Result<T, E>) -> RpcResult {
    result.map(encode_value).map_err(serialize_error)
}
fn encode_null<E: serde::Serialize>(result: Result<(), E>) -> RpcResult {
    result.map(|()| Value::Null).map_err(serialize_error)
}
fn encode_value<T: serde::Serialize>(value: T) -> Value {
    serde_json::to_value(value).unwrap_or_else(
        |error| json!({ "_tag": "RpcSerializationError", "message": error.to_string() }),
    )
}
fn serialize_error<E: serde::Serialize>(error: E) -> Value {
    encode_value(error)
}
fn request_error(method: &str, detail: &str) -> Value {
    json!({ "_tag": "RpcRequestInvalid", "method": method, "detail": detail })
}
fn vcs_error(operation: &str, cwd: &std::path::Path, detail: &str) -> Value {
    json!({ "_tag": "GitCommandError", "operation": operation, "command": "git", "cwd": display_path(cwd), "detail": detail })
}
fn ensure_receipt_matches_request(
    receipt: &WorktreeRemovalReceipt,
    input: &RemoveWorktree,
) -> Result<(), Value> {
    if !same_removal_path(Path::new(&receipt.project_cwd), &input.cwd)
        || !same_removal_path(Path::new(&receipt.worktree_path), &input.path)
    {
        return Err(vcs_error(
            "vcs.removeWorktree",
            &input.cwd,
            "The removal request does not match the durable receipt for this workspace thread.",
        ));
    }
    Ok(())
}

fn same_removal_path(left: &Path, right: &Path) -> bool {
    fn key(path: &Path) -> String {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir().unwrap_or_default().join(path)
        };
        let mut value = absolute.to_string_lossy().replace('\\', "/");
        while value.len() > 1 && value.ends_with('/') {
            value.pop();
        }
        #[cfg(windows)]
        value.make_ascii_lowercase();
        value
    }
    key(left) == key(right)
}
fn source_control_error(provider: &str, operation: &str, detail: &str) -> Value {
    json!({ "_tag": "SourceControlRepositoryError", "provider": provider, "operation": operation, "detail": detail })
}
fn display_path(path: impl AsRef<std::path::Path>) -> String {
    path.as_ref().to_string_lossy().into_owned()
}

fn preflight_publish_repository(
    input: &PublishRepositoryInput,
) -> Result<(String, &'static str), Value> {
    let reject = |detail| source_control_error(&input.provider, "publishRepository", detail);
    if input.provider != "github" {
        return Err(reject(
            "Native repository publishing is unavailable for this provider.",
        ));
    }
    if input.repository.trim().is_empty()
        || input.repository.trim() != input.repository
        || input.repository.starts_with('-')
    {
        return Err(reject(
            "Repository name must be a trimmed non-empty non-option string.",
        ));
    }
    let remote_name = input.remote_name.as_deref().unwrap_or("origin");
    if remote_name.trim().is_empty()
        || remote_name.trim() != remote_name
        || remote_name.starts_with('-')
    {
        return Err(reject(
            "Remote name must be a trimmed non-empty non-option string.",
        ));
    }
    if input
        .protocol
        .as_deref()
        .is_some_and(|protocol| !matches!(protocol, "auto" | "ssh" | "https"))
    {
        return Err(reject("Repository protocol must be auto, ssh, or https."));
    }
    let visibility = match input.visibility.as_str() {
        "private" => "--private",
        "public" => "--public",
        _ => return Err(reject("Repository visibility must be private or public.")),
    };
    Ok((remote_name.to_owned(), visibility))
}

fn summarize_commit_context(context: &str, paths: Option<&[String]>) -> String {
    if let Some(paths) = paths
        && !paths.is_empty()
    {
        return format!("Update {}", paths.join(", "));
    }
    if context.trim().is_empty() {
        return String::new();
    }
    context
        .lines()
        .find(|line| line.starts_with("diff --git "))
        .and_then(|line| line.split(" b/").nth(1))
        .map_or_else(
            || "Update working tree".into(),
            |path| format!("Update {path}"),
        )
}

#[cfg(test)]
mod mutation_ownership_tests {
    use std::{
        panic::AssertUnwindSafe,
        sync::{Arc, Mutex, atomic::Ordering},
        time::Duration,
    };

    use futures_util::FutureExt;
    use serde_json::json;
    use tokio::sync::{Notify, Semaphore};
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::{
        RequestId,
        git::{
            BoxGitProcessFuture, GitProcessRunner, ProcessError, ProcessOutput, ProcessRequest,
            VcsStatusStreamEvent,
        },
    };

    struct ControlledMutationRunner {
        local_status_started: Notify,
        working_tree_dirty: std::sync::atomic::AtomicBool,
        blocked_operation: Option<&'static str>,
        mutation_started: Notify,
        release_mutation: Semaphore,
    }

    struct SuccessfulGithubRunner;

    impl GitProcessRunner for SuccessfulGithubRunner {
        fn run<'a>(
            &'a self,
            _request: ProcessRequest,
            _cancellation: &'a CancellationToken,
        ) -> BoxGitProcessFuture<'a> {
            Box::pin(async {
                Ok(ProcessOutput {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                    stdout_truncated: false,
                    stderr_truncated: false,
                })
            })
        }
    }

    #[derive(Default)]
    struct RecordingGithubRunner {
        requests: Mutex<Vec<ProcessRequest>>,
    }

    impl GitProcessRunner for RecordingGithubRunner {
        fn run<'a>(
            &'a self,
            request: ProcessRequest,
            _cancellation: &'a CancellationToken,
        ) -> BoxGitProcessFuture<'a> {
            self.requests
                .lock()
                .expect("GitHub requests lock")
                .push(request);
            Box::pin(async {
                Ok(ProcessOutput {
                    exit_code: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                    stdout_truncated: false,
                    stderr_truncated: false,
                })
            })
        }
    }

    struct PublishGitRunner {
        requests: Mutex<Vec<ProcessRequest>>,
        fail_push: bool,
    }

    impl PublishGitRunner {
        fn new(fail_push: bool) -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                fail_push,
            }
        }
    }

    impl GitProcessRunner for PublishGitRunner {
        fn run<'a>(
            &'a self,
            request: ProcessRequest,
            _cancellation: &'a CancellationToken,
        ) -> BoxGitProcessFuture<'a> {
            let operation = request.operation.clone();
            self.requests
                .lock()
                .expect("Git requests lock")
                .push(request);
            Box::pin(async move {
                match operation.as_str() {
                    "GitVcsDriver.currentRef" => Ok(ProcessOutput {
                        exit_code: 0,
                        stdout: "main\n".to_owned(),
                        stderr: String::new(),
                        stdout_truncated: false,
                        stderr_truncated: false,
                    }),
                    "GitVcsDriver.pushCurrentBranchToRemote" if self.fail_push => {
                        Err(ProcessError::NonZeroExit {
                            operation,
                            exit_code: 1,
                            stdout_length: 0,
                            stderr_length: 4,
                            stdout: String::new().into_boxed_str(),
                            stderr: "fail".into(),
                        })
                    }
                    "GitVcsDriver.pushCurrentBranchToRemote" => Ok(ProcessOutput {
                        exit_code: 0,
                        stdout: String::new(),
                        stderr: String::new(),
                        stdout_truncated: false,
                        stderr_truncated: false,
                    }),
                    operation => panic!("unexpected publish Git operation {operation}"),
                }
            })
        }
    }

    impl ControlledMutationRunner {
        fn new() -> Self {
            Self {
                local_status_started: Notify::new(),
                working_tree_dirty: std::sync::atomic::AtomicBool::new(false),
                blocked_operation: None,
                mutation_started: Notify::new(),
                release_mutation: Semaphore::new(0),
            }
        }

        fn blocking(operation: &'static str) -> Self {
            Self {
                blocked_operation: Some(operation),
                ..Self::new()
            }
        }

        async fn wait_for_mutation_start(&self) {
            self.mutation_started.notified().await;
        }
    }

    impl GitProcessRunner for ControlledMutationRunner {
        fn run<'a>(
            &'a self,
            request: ProcessRequest,
            cancellation: &'a CancellationToken,
        ) -> BoxGitProcessFuture<'a> {
            let reads_local_status = request.operation == "GitVcsDriver.statusDetailsLocal.status";
            if reads_local_status {
                self.local_status_started.notify_waiters();
            }
            let marks_working_tree_dirty = matches!(
                request.operation.as_str(),
                "GitVcsDriver.initRepo"
                    | "GitVcsDriver.pushCurrentBranch"
                    | "GitVcsDriver.pushCurrentBranchToRemote"
            );
            Box::pin(async move {
                if self.blocked_operation == Some(request.operation.as_str()) {
                    self.mutation_started.notify_one();
                    self.release_mutation
                        .acquire()
                        .await
                        .expect("mutation release remains open")
                        .forget();
                    if cancellation.is_cancelled() {
                        return Err(ProcessError::Cancelled {
                            operation: request.operation,
                        });
                    }
                }
                if marks_working_tree_dirty {
                    self.working_tree_dirty.store(true, Ordering::SeqCst);
                }
                let (exit_code, stdout) = match request.operation.as_str() {
                    "GitVcsDriver.statusDetailsLocal.status" => {
                        let dirty =
                            reads_local_status && self.working_tree_dirty.load(Ordering::SeqCst);
                        let record = dirty.then_some(
                            "1 .M N... 100644 100644 100644 deadbeef deadbeef tracked.txt\n",
                        );
                        (
                            0,
                            format!("# branch.head main\n{}", record.unwrap_or_default()),
                        )
                    }
                    "GitVcsDriver.statusDetailsRemote.status" => {
                        (0, "# branch.head main\n".to_owned())
                    }
                    "GitVcsDriver.statusDetailsLocal.unstagedNumstat" => {
                        (0, "1\t1\ttracked.txt\n".to_owned())
                    }
                    "GitVcsDriver.defaultRef.originHead" | "GitVcsDriver.remoteProvider" => {
                        (1, String::new())
                    }
                    "GitVcsDriver.currentRef" => (0, "main\n".to_owned()),
                    "GitVcsDriver.defaultRef.candidate" => {
                        let is_main = request
                            .args
                            .last()
                            .is_some_and(|value| value == "refs/heads/main");
                        (i32::from(!is_main), String::new())
                    }
                    _ => (0, String::new()),
                };
                Ok(ProcessOutput {
                    exit_code,
                    stdout,
                    stderr: String::new(),
                    stdout_truncated: false,
                    stderr_truncated: false,
                })
            })
        }
    }

    fn request(tag: &str, cwd: &Path) -> RpcRequest {
        RpcRequest {
            id: RequestId::try_from("1").expect("request id"),
            tag: tag.to_owned(),
            payload: json!({ "cwd": cwd }),
            headers: Vec::new(),
            trace_id: None,
            span_id: None,
            sampled: None,
        }
    }

    fn publish_request(id: &str, cwd: &Path) -> RpcRequest {
        RpcRequest {
            id: RequestId::try_from(id).expect("request id"),
            tag: "sourceControl.publishRepository".to_owned(),
            payload: json!({
                "cwd":cwd,"provider":"github","repository":"owner/name",
                "visibility":"private"
            }),
            headers: Vec::new(),
            trace_id: None,
            span_id: None,
            sampled: None,
        }
    }

    fn custom_publish_request(id: &str, cwd: &Path, repository: &str, remote: &str) -> RpcRequest {
        let mut request = publish_request(id, cwd);
        request.payload["repository"] = json!(repository);
        request.payload["remoteName"] = json!(remote);
        request
    }

    #[tokio::test]
    async fn blocked_pull_request_enrichment_cannot_delay_base_or_local_publications() {
        let root = tempfile::tempdir().expect("repository root");
        let runner = Arc::new(ControlledMutationRunner::new());
        let repository = Arc::new(GitRepository::with_runner_for_test(runner));
        let hook = Arc::new(StatusStreamEnrichmentTestHook::default());
        let services = GitVcsRpcServices::with_repository(repository, Arc::default())
            .with_status_stream_enrichment_test_hook(Arc::clone(&hook));
        let cancellation = CancellationToken::new();
        let mut stream = services.status_stream(
            request("subscribeVcsStatus", root.path()),
            cancellation.clone(),
        );
        let snapshot = tokio::time::timeout(Duration::from_secs(5), stream.recv())
            .await
            .expect("initial snapshot deadline")
            .expect("status stream remains open")
            .expect("initial snapshot succeeds");
        assert_eq!(snapshot[0]["_tag"], "snapshot");
        services
            .broadcaster
            .publish_status_event_for_test(
                root.path(),
                VcsStatusStreamEvent::RemoteUpdated {
                    remote: Some(VcsStatusRemoteResult {
                        has_upstream: true,
                        ahead_count: 1,
                        behind_count: 0,
                        ahead_of_default_count: Some(1),
                        pr: None,
                    }),
                },
            )
            .await;
        tokio::time::timeout(Duration::from_secs(5), hook.started.notified())
            .await
            .expect("enrichment start deadline");
        let remote = tokio::time::timeout(Duration::from_millis(200), stream.recv())
            .await
            .expect("base publication must not wait for PR enrichment")
            .expect("status stream remains open")
            .expect("base publication succeeds");
        assert_eq!(remote[0]["_tag"], "remoteUpdated");

        services
            .broadcaster
            .publish_status_event_for_test(
                root.path(),
                VcsStatusStreamEvent::LocalUpdated {
                    local: VcsStatusLocalResult {
                        is_repo: true,
                        repository_unavailable_reason: None,
                        source_control_provider: None,
                        has_primary_remote: false,
                        is_default_ref: false,
                        ref_name: Some("main".to_owned()),
                        default_ref_name: Some("main".to_owned()),
                        has_working_tree_changes: true,
                        working_tree: Default::default(),
                        operation_in_progress: None,
                    },
                },
            )
            .await;
        let local = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let chunk = stream
                    .recv()
                    .await
                    .expect("status stream remains open")
                    .expect("local publication succeeds");
                if chunk[0]["_tag"] == "localUpdated" {
                    return chunk;
                }
            }
        })
        .await
        .expect("local publication must overtake blocked PR enrichment");
        assert_eq!(local[0]["_tag"], "localUpdated");

        cancellation.cancel();
        hook.release.add_permits(1);
    }

    #[tokio::test]
    async fn mutation_retires_one_shared_read_for_two_clients_and_publishes_one_trailing_refresh() {
        let root = tempfile::tempdir().expect("repository root");
        let runner = Arc::new(ControlledMutationRunner::new());
        let repository = Arc::new(GitRepository::with_runner_for_test(runner.clone()));
        let services = GitVcsRpcServices::with_repository(repository, Arc::default());
        let mut subscription = services
            .broadcaster
            .subscribe(root.path().to_path_buf(), CancellationToken::new())
            .await
            .expect("initial status subscription");
        assert!(matches!(
            subscription.recv().await,
            Some(VcsStatusStreamEvent::Snapshot { .. })
        ));
        let stale_read_gate = services
            .broadcaster
            .install_full_read_execution_gate_for_test();

        let first_services = services.clone();
        let first_cwd = root.path().to_path_buf();
        let mut first = tokio::spawn(async move {
            first_services
                .handle_unary_with_context(
                    request("vcs.refreshStatus", &first_cwd),
                    RpcSessionContext::unauthenticated(),
                    CancellationToken::new(),
                )
                .await
        });
        stale_read_gate.wait_until_entered().await;
        let second_services = services.clone();
        let second_cwd = root.path().to_path_buf();
        let mut second = tokio::spawn(async move {
            second_services
                .handle_unary_with_context(
                    request("vcs.refreshStatus", &second_cwd),
                    RpcSessionContext::unauthenticated(),
                    CancellationToken::new(),
                )
                .await
        });
        while services
            .broadcaster
            .full_read_lease_count_for_test(root.path())
            .await
            != 2
        {
            tokio::task::yield_now().await;
        }

        let mutation = services
            .handle_unary_with_context(
                request("vcs.init", root.path()),
                RpcSessionContext::unauthenticated(),
                CancellationToken::new(),
            )
            .await;
        let retired = tokio::time::timeout(Duration::from_millis(200), async {
            (
                (&mut first).await.expect("first client task"),
                (&mut second).await.expect("second client task"),
            )
        })
        .await;
        let retired_in_time = retired.is_ok();
        let (first, second) = match retired {
            Ok(results) => results,
            Err(_) => (
                first.await.expect("released first client task"),
                second.await.expect("released second client task"),
            ),
        };
        let trailing = tokio::time::timeout(Duration::from_millis(200), async {
            loop {
                let event = subscription.recv().await?;
                if matches!(
                    event,
                    VcsStatusStreamEvent::LocalUpdated { ref local }
                        if local.has_working_tree_changes
                ) {
                    break Some(event);
                }
            }
        })
        .await
        .ok()
        .flatten();

        assert!(mutation.is_ok());
        assert!(
            retired_in_time,
            "mutation must retire the shared pre-mutation read"
        );
        assert!(first.is_err() && second.is_err());
        assert!(matches!(
            trailing,
            Some(VcsStatusStreamEvent::LocalUpdated { local })
                if local.has_working_tree_changes
        ));
    }

    #[tokio::test]
    async fn stacked_push_runs_inside_the_shared_mutation_boundary() {
        let root = tempfile::tempdir().expect("repository root");
        let runner = Arc::new(ControlledMutationRunner::new());
        let repository = Arc::new(GitRepository::with_runner_for_test(runner.clone()));
        let services = GitVcsRpcServices::with_repository(repository, Arc::default());
        let mut subscription = services
            .broadcaster
            .subscribe(root.path().to_path_buf(), CancellationToken::new())
            .await
            .expect("initial status subscription");
        subscription.recv().await.expect("initial status snapshot");
        let stale_services = services.clone();
        let stale_cwd = root.path().to_path_buf();
        let stale_read_gate = services
            .broadcaster
            .install_full_read_execution_gate_for_test();
        let stale = tokio::spawn(async move {
            stale_services
                .handle_unary_with_context(
                    request("vcs.refreshStatus", &stale_cwd),
                    RpcSessionContext::unauthenticated(),
                    CancellationToken::new(),
                )
                .await
        });
        stale_read_gate.wait_until_entered().await;
        assert_eq!(
            services
                .broadcaster
                .full_read_lease_count_for_test(root.path())
                .await,
            1
        );
        let mut stream = services.stacked_action_stream(
            RpcRequest {
                id: RequestId::try_from("2").expect("request id"),
                tag: "git.runStackedAction".to_owned(),
                payload: json!({
                    "actionId":"push-action",
                    "cwd":root.path(),
                    "action":"push"
                }),
                headers: Vec::new(),
                trace_id: None,
                span_id: None,
                sampled: None,
            },
            CancellationToken::new(),
        );

        let started = stream
            .recv()
            .await
            .expect("stacked start chunk")
            .expect("stacked start succeeds");
        assert_eq!(started[0]["kind"], "action_started");
        let finished = stream
            .recv()
            .await
            .expect("stacked finish chunk")
            .expect("stacked push succeeds");
        assert_eq!(finished[0]["kind"], "action_finished");
        assert_eq!(finished[0]["result"]["push"]["status"], "pushed");
        assert!(stale.await.expect("stale read task").is_err());
        let trailing = tokio::time::timeout(Duration::from_millis(200), async {
            loop {
                let event = subscription.recv().await?;
                if matches!(
                    event,
                    VcsStatusStreamEvent::LocalUpdated { ref local }
                        if local.has_working_tree_changes
                ) {
                    break Some(event);
                }
            }
        })
        .await
        .ok()
        .flatten();
        assert!(trailing.is_some());
    }

    #[tokio::test]
    async fn publish_repository_retires_a_stale_read_and_publishes_a_trailing_refresh() {
        let root = tempfile::tempdir().expect("repository root");
        let runner = Arc::new(ControlledMutationRunner::new());
        let repository = Arc::new(GitRepository::with_runner_for_test(runner.clone()));
        let services = GitVcsRpcServices::with_repository_and_github_runner_for_test(
            repository,
            Arc::new(SuccessfulGithubRunner),
        );
        let mut subscription = services
            .broadcaster
            .subscribe(root.path().to_path_buf(), CancellationToken::new())
            .await
            .expect("initial status subscription");
        subscription.recv().await.expect("initial status snapshot");
        let stale_services = services.clone();
        let stale_cwd = root.path().to_path_buf();
        let stale_read_gate = services
            .broadcaster
            .install_full_read_execution_gate_for_test();
        let stale = tokio::spawn(async move {
            stale_services
                .handle_unary_with_context(
                    request("vcs.refreshStatus", &stale_cwd),
                    RpcSessionContext::unauthenticated(),
                    CancellationToken::new(),
                )
                .await
        });
        stale_read_gate.wait_until_entered().await;
        assert_eq!(
            services
                .broadcaster
                .full_read_lease_count_for_test(root.path())
                .await,
            1
        );

        let published = services
            .handle_unary_with_context(
                publish_request("5", root.path()),
                RpcSessionContext::unauthenticated(),
                CancellationToken::new(),
            )
            .await
            .expect("publish succeeds");

        assert_eq!(published["status"], "pushed");
        assert!(stale.await.expect("stale read task").is_err());
        loop {
            if matches!(
                subscription.recv().await,
                Some(VcsStatusStreamEvent::LocalUpdated { local })
                    if local.has_working_tree_changes
            ) {
                break;
            }
        }
    }

    #[tokio::test]
    async fn admitted_publish_repository_outlives_its_cancelled_response_waiter() {
        let root = tempfile::tempdir().expect("repository root");
        let runner = Arc::new(ControlledMutationRunner::blocking(
            "GitVcsDriver.pushCurrentBranchToRemote",
        ));
        let repository = Arc::new(GitRepository::with_runner_for_test(runner.clone()));
        let services = GitVcsRpcServices::with_repository_and_github_runner_for_test(
            repository,
            Arc::new(SuccessfulGithubRunner),
        );
        let cancellation = CancellationToken::new();
        let task_services = services.clone();
        let task_cancellation = cancellation.clone();
        let cwd = root.path().to_path_buf();
        let response_waiter = tokio::spawn(async move {
            task_services
                .handle_unary_with_context(
                    publish_request("6", &cwd),
                    RpcSessionContext::unauthenticated(),
                    task_cancellation,
                )
                .await
        });
        runner.wait_for_mutation_start().await;

        cancellation.cancel();
        response_waiter.abort();
        let _ = response_waiter.await;
        assert_eq!(
            services
                .broadcaster
                .local_refresh_generation_for_test(root.path())
                .await,
            0
        );
        runner.release_mutation.add_permits(1);
        loop {
            if services
                .broadcaster
                .local_refresh_generation_for_test(root.path())
                .await
                == 1
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn publish_repository_partial_push_error_settles_once() {
        let root = tempfile::tempdir().expect("repository root");
        let runner = Arc::new(ControlledMutationRunner::blocking(
            "GitVcsDriver.pushCurrentBranchToRemote",
        ));
        let repository = Arc::new(GitRepository::with_runner_for_test(runner.clone()));
        let services = GitVcsRpcServices::with_repository_and_github_runner_for_test(
            repository,
            Arc::new(SuccessfulGithubRunner),
        );
        let cancellation = CancellationToken::new();
        let task_services = services.clone();
        let task_cancellation = cancellation.clone();
        let cwd = root.path().to_path_buf();
        let response = tokio::spawn(async move {
            task_services
                .handle_unary_with_context(
                    publish_request("7", &cwd),
                    RpcSessionContext::unauthenticated(),
                    task_cancellation,
                )
                .await
        });
        runner.wait_for_mutation_start().await;
        cancellation.cancel();
        runner.release_mutation.add_permits(1);

        assert!(response.await.expect("publish response joins").is_err());
        assert_eq!(
            services
                .broadcaster
                .local_refresh_generation_for_test(root.path())
                .await,
            1
        );
    }

    #[tokio::test]
    async fn publish_repository_pushes_the_selected_remote_with_exact_arguments() {
        let root = tempfile::tempdir().expect("repository root");
        let git = Arc::new(PublishGitRunner::new(false));
        let github = Arc::new(RecordingGithubRunner::default());
        let services = GitVcsRpcServices::with_repository_and_github_runner_for_test(
            Arc::new(GitRepository::with_runner_for_test(git.clone())),
            github.clone(),
        );

        let result = services
            .handle_unary_with_context(
                custom_publish_request("8", root.path(), "owner/name", "upstream"),
                RpcSessionContext::unauthenticated(),
                CancellationToken::new(),
            )
            .await
            .expect("custom remote publish succeeds");

        assert_eq!(result["remoteName"], "upstream");
        assert_eq!(result["upstreamBranch"], "upstream/main");
        let github_requests = github.requests.lock().expect("GitHub requests lock");
        assert_eq!(github_requests.len(), 1);
        assert_eq!(
            github_requests[0].args,
            [
                "repo",
                "create",
                "owner/name",
                "--private",
                "--source",
                ".",
                "--remote=upstream"
            ]
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
        );
        let git_requests = git.requests.lock().expect("Git requests lock");
        assert_eq!(git_requests.len(), 2);
        assert_eq!(
            git_requests[1].args,
            [
                "-c",
                "http.lowSpeedLimit=1000",
                "-c",
                "http.lowSpeedTime=60",
                "push",
                "--set-upstream",
                "--",
                "upstream",
                "main"
            ]
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
        );
        assert_eq!(
            git_requests[1].timeout,
            crate::git::BOUNDED_TRANSFER_TIMEOUT,
            "publishing has no Cancel, so its push keeps the bounded deadline"
        );
    }

    #[tokio::test]
    async fn custom_publish_push_failure_settles_once_after_github_success() {
        let root = tempfile::tempdir().expect("repository root");
        let git = Arc::new(PublishGitRunner::new(true));
        let github = Arc::new(RecordingGithubRunner::default());
        let services = GitVcsRpcServices::with_repository_and_github_runner_for_test(
            Arc::new(GitRepository::with_runner_for_test(git)),
            github.clone(),
        );

        let result = services
            .handle_unary_with_context(
                custom_publish_request("9", root.path(), "owner/name", "upstream"),
                RpcSessionContext::unauthenticated(),
                CancellationToken::new(),
            )
            .await;

        assert!(result.is_err());
        assert_eq!(
            github.requests.lock().expect("GitHub requests lock").len(),
            1
        );
        assert_eq!(
            services
                .broadcaster
                .local_refresh_generation_for_test(root.path())
                .await,
            1
        );
    }

    #[tokio::test]
    async fn option_shaped_publish_inputs_start_no_process_or_mutation() {
        let root = tempfile::tempdir().expect("repository root");
        let git = Arc::new(PublishGitRunner::new(false));
        let github = Arc::new(RecordingGithubRunner::default());
        let services = GitVcsRpcServices::with_repository_and_github_runner_for_test(
            Arc::new(GitRepository::with_runner_for_test(git.clone())),
            github.clone(),
        );

        for request in [
            custom_publish_request("10", root.path(), "--public", "upstream"),
            custom_publish_request("11", root.path(), "owner/name", "--all"),
        ] {
            assert!(
                services
                    .handle_unary_with_context(
                        request,
                        RpcSessionContext::unauthenticated(),
                        CancellationToken::new(),
                    )
                    .await
                    .is_err()
            );
        }
        assert!(git.requests.lock().expect("Git requests lock").is_empty());
        assert!(
            github
                .requests
                .lock()
                .expect("GitHub requests lock")
                .is_empty()
        );
        assert_eq!(
            services
                .broadcaster
                .local_refresh_generation_for_test(root.path())
                .await,
            0
        );
    }

    #[tokio::test]
    async fn admitted_unary_mutation_outlives_its_cancelled_response_waiter() {
        let root = tempfile::tempdir().expect("repository root");
        let runner = Arc::new(ControlledMutationRunner::blocking("GitVcsDriver.initRepo"));
        let repository = Arc::new(GitRepository::with_runner_for_test(runner.clone()));
        let services = GitVcsRpcServices::with_repository(repository, Arc::default());
        let cancellation = CancellationToken::new();
        let task_services = services.clone();
        let task_cancellation = cancellation.clone();
        let cwd = root.path().to_path_buf();
        let response_waiter = tokio::spawn(async move {
            task_services
                .handle_unary_with_context(
                    request("vcs.init", &cwd),
                    RpcSessionContext::unauthenticated(),
                    task_cancellation,
                )
                .await
        });
        runner.wait_for_mutation_start().await;

        cancellation.cancel();
        response_waiter.abort();
        let _ = response_waiter.await;
        tokio::task::yield_now().await;
        let before_release = services
            .broadcaster
            .local_refresh_generation_for_test(root.path())
            .await;
        runner.release_mutation.add_permits(1);
        tokio::time::timeout(Duration::from_millis(200), async {
            loop {
                if services
                    .broadcaster
                    .local_refresh_generation_for_test(root.path())
                    .await
                    == 1
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("owned mutation reaches terminal settlement");

        assert_eq!(before_release, 0);
        assert_eq!(
            services
                .broadcaster
                .local_refresh_generation_for_test(root.path())
                .await,
            1
        );
    }

    #[tokio::test]
    async fn rejected_and_empty_unary_requests_do_not_fence_status_reads() {
        let root = tempfile::tempdir().expect("repository root");
        let repository = Arc::new(GitRepository::with_runner_for_test(Arc::new(
            ControlledMutationRunner::new(),
        )));
        let services = GitVcsRpcServices::with_repository(repository, Arc::default());
        for (tag, payload, succeeds) in [
            (
                "vcs.init",
                json!({"cwd":root.path(),"kind":"mercurial"}),
                false,
            ),
            (
                "vcs.stageFiles",
                json!({"cwd":root.path(),"filePaths":[]}),
                true,
            ),
            (
                "vcs.unstageFiles",
                json!({"cwd":root.path(),"filePaths":[]}),
                true,
            ),
            (
                "vcs.discardFiles",
                json!({"cwd":root.path(),"filePaths":[]}),
                true,
            ),
            (
                "vcs.stageFiles",
                json!({"cwd":root.path(),"filePaths":["../escape"]}),
                false,
            ),
            (
                "git.preparePullRequestThread",
                json!({"cwd":root.path(),"reference":"current","mode":"unsupported"}),
                false,
            ),
            (
                "sourceControl.publishRepository",
                json!({
                    "cwd":root.path(),"provider":"github","repository":" owner/name",
                    "visibility":"private"
                }),
                false,
            ),
            (
                "sourceControl.publishRepository",
                json!({
                    "cwd":root.path(),"provider":"github","repository":"owner/name",
                    "visibility":"private","protocol":"ftp"
                }),
                false,
            ),
        ] {
            let result = services
                .handle_unary_with_context(
                    RpcRequest {
                        id: RequestId::try_from("3").expect("request id"),
                        tag: tag.to_owned(),
                        payload,
                        headers: Vec::new(),
                        trace_id: None,
                        span_id: None,
                        sampled: None,
                    },
                    RpcSessionContext::unauthenticated(),
                    CancellationToken::new(),
                )
                .await;
            assert_eq!(result.is_ok(), succeeds, "{tag}");
        }

        assert_eq!(
            services
                .broadcaster
                .local_refresh_generation_for_test(root.path())
                .await,
            0
        );
    }

    #[tokio::test]
    async fn unsupported_stacked_action_keeps_action_started_order_without_a_mutation_fence() {
        let root = tempfile::tempdir().expect("repository root");
        let services = GitVcsRpcServices::with_repository(
            Arc::new(GitRepository::with_runner_for_test(Arc::new(
                ControlledMutationRunner::new(),
            ))),
            Arc::default(),
        );
        let mut stream = services.stacked_action_stream(
            RpcRequest {
                id: RequestId::try_from("4").expect("request id"),
                tag: "git.runStackedAction".to_owned(),
                payload: json!({
                    "actionId":"unsupported-action",
                    "cwd":root.path(),
                    "action":"unsupported"
                }),
                headers: Vec::new(),
                trace_id: None,
                span_id: None,
                sampled: None,
            },
            CancellationToken::new(),
        );

        assert_eq!(
            stream.recv().await.expect("start chunk").expect("start")[0]["kind"],
            "action_started"
        );
        assert_eq!(
            stream
                .recv()
                .await
                .expect("failure chunk")
                .expect("failure")[0]["kind"],
            "action_failed"
        );
        assert_eq!(
            services
                .broadcaster
                .local_refresh_generation_for_test(root.path())
                .await,
            0
        );
    }

    #[tokio::test]
    async fn blocked_old_pull_request_enrichment_is_rejected_after_switch_epoch() {
        let root = tempfile::tempdir().expect("repository root");
        let broadcaster = StatusBroadcaster::new(
            Arc::new(GitRepository::default()),
            Duration::from_secs(3_600),
            4,
        );
        let cancellation = CancellationToken::new();
        let old_fence = broadcaster
            .acquire_read_fence(root.path(), &cancellation)
            .await
            .expect("old status fence");
        let enrichment_started = Arc::new(Notify::new());
        let release_enrichment = Arc::new(Semaphore::new(0));
        let old_enrichment = {
            let broadcaster = broadcaster.clone();
            let enrichment_started = Arc::clone(&enrichment_started);
            let release_enrichment = Arc::clone(&release_enrichment);
            tokio::spawn(async move {
                finish_fenced_enrichment(&broadcaster, old_fence, async move {
                    enrichment_started.notify_one();
                    release_enrichment
                        .acquire()
                        .await
                        .expect("enrichment release remains open")
                        .forget();
                    "old-pr"
                })
                .await
            })
        };
        enrichment_started.notified().await;

        let mutation = broadcaster.begin_mutation(root.path()).await;
        mutation.finish().await;
        let new_fence = broadcaster
            .acquire_read_fence(root.path(), &cancellation)
            .await
            .expect("new status fence");
        assert_eq!(
            finish_fenced_enrichment(&broadcaster, new_fence, async { "new-pr" })
                .await
                .expect("new snapshot enrichment is current"),
            "new-pr"
        );

        release_enrichment.add_permits(1);
        assert!(
            old_enrichment
                .await
                .expect("old enrichment task joins")
                .is_err(),
            "old enriched result must not be delivered"
        );
    }

    #[tokio::test]
    async fn owned_git_task_panic_settles_the_guard_and_reaches_the_outer_unwind_boundary() {
        let root = tempfile::tempdir().expect("repository root");
        let broadcaster = StatusBroadcaster::new(
            Arc::new(GitRepository::default()),
            Duration::from_secs(3_600),
            4,
        );
        let owned_broadcaster = broadcaster.clone();
        let cwd = root.path().to_path_buf();

        let panic = AssertUnwindSafe(await_server_owned_rpc("vcs.init", async move {
            let _mutation = owned_broadcaster.begin_mutation(&cwd).await;
            panic!("owned Git mutation panic");
        }))
        .catch_unwind()
        .await;

        assert!(panic.is_err());
        assert_eq!(
            broadcaster
                .local_refresh_generation_for_test(root.path())
                .await,
            1
        );
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::{EditorLaunchStrategy, editor_launch_strategy, open_in_editor_with};
    use serde_json::json;

    #[test]
    fn windows_editor_launch_uses_the_shell_resolved_application() {
        let strategy = editor_launch_strategy(
            "cursor",
            vec!["--goto".to_owned(), "C:\\repo\\keybindings.json".to_owned()],
            "C:\\repo\\keybindings.json".to_owned(),
        );

        assert_eq!(
            strategy,
            EditorLaunchStrategy::ShellAssociation {
                application: "cursor".to_owned(),
                target: "C:\\repo\\keybindings.json".to_owned(),
            }
        );
    }

    #[test]
    fn editor_spawn_errors_are_typed_without_launching_an_external_application() {
        let env = crate::production::editor_launch::EditorProbeEnv {
            path_entries: Vec::new(),
            home: None,
            flatpak_export_dirs: Vec::new(),
            local_app_data: None,
            absolute_candidates_root: None,
        };
        let error = open_in_editor_with(
            json!({
                "cwd": "C:\\repo",
                "editor": "rustrover",
            }),
            &env,
            |_| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "missing fixture editor",
                ))
            },
        )
        .expect_err("injected launcher failure");

        assert_eq!(error["_tag"], "ExternalLauncherEditorSpawnError");
        assert_eq!(error["editor"], "rustrover");
        assert_eq!(error["target"], "C:\\repo");
        assert_eq!(error["command"], "rustrover");
        assert_eq!(error["args"], json!(["C:\\repo"]));
        assert_eq!(error["cause"], "missing fixture editor");
    }
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;
    use crate::RequestId;
    use std::{collections::VecDeque, sync::Mutex};

    fn rpc_request(tag: &str, payload: Value) -> RpcRequest {
        RpcRequest {
            id: RequestId::try_from("1").expect("request id"),
            tag: tag.to_owned(),
            payload,
            headers: Vec::new(),
            trace_id: None,
            span_id: None,
            sampled: None,
        }
    }

    async fn git(sandbox: &crate::test_support::TestSandbox, cwd: &Path, args: &[&str]) {
        ProcessRunner
            .run(
                ProcessRequest {
                    operation: "test.gitVcs.git".to_owned(),
                    command: sandbox.executable_on_path("git"),
                    args: args.iter().map(OsString::from).collect(),
                    cwd: cwd.to_path_buf(),
                    env: sandbox
                        .environment([("GIT_CONFIG_NOSYSTEM", "1")])
                        .into_iter()
                        .map(|(key, value)| (key.into(), value.into()))
                        .collect(),
                    stdin: None,
                    timeout: Duration::from_secs(30),
                    max_output_bytes: 128_000,
                    output_policy: OutputPolicy::Error,
                    append_truncation_marker: false,
                    allow_non_zero_exit: false,
                },
                &CancellationToken::new(),
            )
            .await
            .unwrap_or_else(|error| panic!("git {args:?} failed: {error}"));
    }

    struct CapturedGitRunner {
        command: PathBuf,
        environment: Vec<(OsString, OsString)>,
    }

    impl CapturedGitRunner {
        fn new(sandbox: &crate::test_support::TestSandbox) -> Self {
            Self {
                command: sandbox.executable_on_path("git"),
                environment: sandbox
                    .environment([("GIT_CONFIG_NOSYSTEM", "1")])
                    .into_iter()
                    .map(|(key, value)| (key.into(), value.into()))
                    .collect(),
            }
        }
    }

    impl crate::git::GitProcessRunner for CapturedGitRunner {
        fn run<'a>(
            &'a self,
            mut request: ProcessRequest,
            cancellation: &'a CancellationToken,
        ) -> crate::git::BoxGitProcessFuture<'a> {
            request.command.clone_from(&self.command);
            let mut environment = self.environment.clone();
            environment.extend(request.env);
            request.env = environment;
            Box::pin(async move { ProcessRunner.run(request, cancellation).await })
        }
    }

    struct FixtureGitRunner {
        outputs: Mutex<VecDeque<crate::git::ProcessOutput>>,
    }

    impl FixtureGitRunner {
        fn main_branch_push() -> Self {
            Self {
                outputs: Mutex::new(
                    [
                        crate::git::ProcessOutput {
                            exit_code: 0,
                            stdout: "main\n".to_owned(),
                            stderr: String::new(),
                            stdout_truncated: false,
                            stderr_truncated: false,
                        },
                        crate::git::ProcessOutput {
                            exit_code: 1,
                            stdout: String::new(),
                            stderr: String::new(),
                            stdout_truncated: false,
                            stderr_truncated: false,
                        },
                        crate::git::ProcessOutput {
                            exit_code: 0,
                            stdout: String::new(),
                            stderr: String::new(),
                            stdout_truncated: false,
                            stderr_truncated: false,
                        },
                    ]
                    .into(),
                ),
            }
        }
    }

    impl crate::git::GitProcessRunner for FixtureGitRunner {
        fn run<'a>(
            &'a self,
            _request: ProcessRequest,
            _cancellation: &'a CancellationToken,
        ) -> crate::git::BoxGitProcessFuture<'a> {
            let output = self
                .outputs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .pop_front()
                .expect("fixture Git output");
            Box::pin(async move { Ok(output) })
        }
    }

    async fn unary(services: &GitVcsRpcServices, tag: &str, payload: Value) -> RpcResult {
        services
            .handle_unary_with_context(
                rpc_request(tag, payload),
                RpcSessionContext::unauthenticated(),
                CancellationToken::new(),
            )
            .await
    }

    #[tokio::test]
    async fn workspace_loss_cancels_an_admitted_git_operation_with_the_exact_error() {
        let registry = WorkspaceAvailabilityRegistry::new();
        let path = PathBuf::from("/repo/worktrees/git-guard");
        let admission = registry
            .acquire_path_admission([path.as_path()])
            .await
            .expect("Git operation should be admitted before loss");
        let operation_cancellation = CancellationToken::new();
        let operation_cancellation_probe = operation_cancellation.clone();
        let task = tokio::spawn(async move {
            await_git_rpc_operation(
                Some(&admission),
                operation_cancellation,
                std::future::pending::<RpcResult>(),
            )
            .await
        });

        assert!(
            registry
                .mark_unavailable(crate::worktree_catalog::WorkspaceLossTransition {
                    thread_id: "thread-git".to_owned(),
                    repository_key: "repository-git".to_owned(),
                    generation: 1,
                    path: path.clone(),
                    availability:
                        crate::worktree_catalog::AdoptedWorktreeAvailability::MissingRegistered,
                })
                .await
                .expect("physical identity resolves")
        );

        let error = task
            .await
            .expect("Git admission task should join")
            .expect_err("workspace loss must win over the admitted Git operation");
        assert_eq!(error["_tag"], "WorkspaceUnavailableError");
        assert_eq!(error["threadId"], "thread-git");
        assert!(operation_cancellation_probe.is_cancelled());
    }

    async fn named_summary(
        summaries: &mut watch::Receiver<
            Option<Result<crate::git::VcsStatusSummary, GitCommandError>>,
        >,
        expected_ref: &str,
        label: &str,
    ) -> crate::git::VcsStatusSummary {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let current = summaries
                    .borrow_and_update()
                    .as_ref()
                    .and_then(|result| result.as_ref().ok())
                    .cloned();
                if let Some(summary) = current
                    && summary.ref_name.as_deref() == Some(expected_ref)
                {
                    return summary;
                }
                summaries
                    .changed()
                    .await
                    .expect("summary producer stays alive");
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{label}"))
    }

    #[tokio::test]
    async fn finished_local_mutation_refreshes_the_passive_summary_immediately() {
        let sandbox = crate::test_support::TestSandbox::new("passive-summary-mutation");
        let cwd = sandbox.root().to_path_buf();
        git(&sandbox, &cwd, &["init", "-b", "main"]).await;
        git(&sandbox, &cwd, &["config", "user.name", "Summary Test"]).await;
        git(
            &sandbox,
            &cwd,
            &["config", "user.email", "summary@example.test"],
        )
        .await;
        std::fs::write(cwd.join("tracked.txt"), "base\n").expect("base file");
        git(&sandbox, &cwd, &["add", "tracked.txt"]).await;
        git(&sandbox, &cwd, &["commit", "-q", "-m", "base"]).await;
        git(&sandbox, &cwd, &["branch", "feature"]).await;
        let repository = Arc::new(GitRepository::with_runner_for_test(Arc::new(
            CapturedGitRunner::new(&sandbox),
        )));
        let services = GitVcsRpcServices::with_repository(repository, Arc::default())
            .with_hosting_executable_dir_for_integration_test(
                crate::test_support::hermetic_providers::missing_hosting_executable_dir(&cwd),
            );
        let mut summaries = services
            .summary
            .subscribe(cwd.clone())
            .await
            .expect("summary subscription");
        named_summary(&mut summaries, "main", "initial passive summary names main").await;

        let mutation = services.broadcaster.begin_mutation(&cwd).await;
        git(&sandbox, &cwd, &["switch", "-q", "feature"]).await;
        mutation.finish().await;

        let refreshed = named_summary(
            &mut summaries,
            "feature",
            "the passive summary reflects the finished checkout without waiting for its 30-second cycle",
        )
        .await;
        assert!(!refreshed.stale);
    }

    /// A second window (or a reconnecting remote client) joins a producer that already
    /// published. It must get the current summary now, not after the next 30-second cycle.
    #[tokio::test]
    async fn joining_summary_stream_receives_the_current_summary_immediately() {
        let sandbox = crate::test_support::TestSandbox::new("passive-summary-join");
        let cwd = sandbox.root().to_path_buf();
        git(&sandbox, &cwd, &["init", "-b", "main"]).await;
        git(&sandbox, &cwd, &["config", "user.name", "Summary Test"]).await;
        git(
            &sandbox,
            &cwd,
            &["config", "user.email", "summary@example.test"],
        )
        .await;
        git(
            &sandbox,
            &cwd,
            &["commit", "--allow-empty", "-q", "-m", "base"],
        )
        .await;
        let repository = Arc::new(GitRepository::with_runner_for_test(Arc::new(
            CapturedGitRunner::new(&sandbox),
        )));
        let services = GitVcsRpcServices::with_repository(repository, Arc::default())
            .with_hosting_executable_dir_for_integration_test(
                crate::test_support::hermetic_providers::missing_hosting_executable_dir(&cwd),
            );
        let mut first = services
            .summary
            .subscribe(cwd.clone())
            .await
            .expect("summary subscription");
        named_summary(&mut first, "main", "first subscriber sees main").await;

        let mut joined = services.summary_stream(
            rpc_request("subscribeVcsStatusSummary", json!({ "cwd": cwd })),
            CancellationToken::new(),
        );
        let chunk = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(chunk) = joined.borrow_and_update().clone() {
                    return chunk;
                }
                joined.changed().await.expect("summary stream stays open");
            }
        })
        .await
        .expect("a joining subscriber gets the current summary before the next 30-second cycle")
        .expect("summary chunk");
        assert_eq!(chunk[0]["refName"], "main");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn hosting_fixture_directory_also_pins_passive_summary_pull_requests() {
        let sandbox = crate::test_support::TestSandbox::new("summary-hosting-fixture");
        let cwd = sandbox.root().to_path_buf();
        git(&sandbox, &cwd, &["init", "-b", "main"]).await;
        git(&sandbox, &cwd, &["config", "user.name", "Summary Test"]).await;
        git(
            &sandbox,
            &cwd,
            &["config", "user.email", "summary@example.test"],
        )
        .await;
        git(&sandbox, &cwd, &["commit", "--allow-empty", "-m", "base"]).await;
        git(
            &sandbox,
            &cwd,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/acme/repo.git",
            ],
        )
        .await;
        let fixture = sandbox.executable_script(
            "gh",
            r#"test "$1" = pr && test "$2" = list && test "$4" = main || exit 1
printf '%s\n' '[{"number":42,"title":"Fixture PR","url":"https://github.com/acme/repo/pull/42","baseRefName":"base","headRefName":"main","state":"OPEN"}]'"#,
            "",
        );
        std::fs::rename(fixture, cwd.join("gh")).expect("hosting fixture name");
        let repository = Arc::new(GitRepository::with_runner_for_test(Arc::new(
            CapturedGitRunner::new(&sandbox),
        )));
        let services = GitVcsRpcServices::with_repository(repository, Arc::default())
            .with_hosting_executable_dir_for_integration_test(cwd.clone());
        let mut summaries = services
            .summary
            .subscribe(cwd)
            .await
            .expect("summary subscription");
        let pr = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(Ok(summary)) = summaries.borrow_and_update().as_ref()
                    && let Some(pr) = &summary.pr
                {
                    break pr.clone();
                }
                summaries
                    .changed()
                    .await
                    .expect("summary producer stays alive");
            }
        })
        .await
        .expect("the summary must resolve its PR through the hosting fixture");
        assert_eq!(pr.number, 42);
        assert_eq!(pr.title, "Fixture PR");
    }

    #[tokio::test]
    async fn workspace_loss_ends_an_admitted_status_stream_with_the_exact_error() {
        let sandbox = crate::test_support::TestSandbox::new("workspace-loss-status");
        git(&sandbox, sandbox.root(), &["init", "-b", "main"]).await;
        let registry = WorkspaceAvailabilityRegistry::new();
        let repository = Arc::new(GitRepository::with_runner_for_test(Arc::new(
            CapturedGitRunner::new(&sandbox),
        )));
        let services = GitVcsRpcServices::with_repository(repository, Arc::default())
            .with_availability_registry(registry.clone());
        let mut stream = services.status_stream(
            rpc_request("subscribeVcsStatus", json!({"cwd": sandbox.root()})),
            CancellationToken::new(),
        );
        stream
            .recv()
            .await
            .expect("initial status chunk")
            .expect("initial status snapshot");

        assert!(
            registry
                .mark_unavailable(crate::worktree_catalog::WorkspaceLossTransition {
                    thread_id: "thread-status".to_owned(),
                    repository_key: "repository-status".to_owned(),
                    generation: 1,
                    path: sandbox.root().to_path_buf(),
                    availability:
                        crate::worktree_catalog::AdoptedWorktreeAvailability::MissingRegistered,
                })
                .await
                .expect("physical identity resolves")
        );

        let error = stream
            .recv()
            .await
            .expect("workspace loss error chunk")
            .expect_err("workspace loss must end the admitted status stream");
        assert_eq!(error["_tag"], "WorkspaceUnavailableError");
        assert_eq!(error["threadId"], "thread-status");
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn github_cli_and_git_runner_fixtures_are_instance_owned_in_parallel() {
        async fn publish(label: &str) -> (Value, Value) {
            let sandbox = crate::test_support::TestSandbox::new(label);
            let github_command = sandbox.executable_script(
                "gh",
                &format!(
                    "case \"$1:$2\" in\n  repo:view) printf '%s\\n' '{{\"nameWithOwner\":\"owner/{label}\",\"url\":\"https://github.test/owner/{label}\",\"sshUrl\":\"git@github.test:owner/{label}.git\"}}' ;;\n  repo:create) exit 0 ;;\n  *) exit 64 ;;\nesac"
                ),
                "",
            );
            let repository = Arc::new(GitRepository::with_runner_for_test(Arc::new(
                FixtureGitRunner::main_branch_push(),
            )));
            let services =
                GitVcsRpcServices::with_repository_and_github_command(repository, github_command);
            let cwd = sandbox.root().to_path_buf();
            let lookup = unary(
                &services,
                "sourceControl.lookupRepository",
                json!({
                    "provider":"github",
                    "repository":format!("owner/{label}"),
                    "cwd":cwd,
                }),
            )
            .await
            .expect("GitHub lookup fixture should resolve");
            let published = unary(
                &services,
                "sourceControl.publishRepository",
                json!({
                    "cwd":sandbox.root(),
                    "provider":"github",
                    "repository":format!("owner/{label}"),
                    "visibility":"private",
                    "protocol":"ssh",
                    "remoteName":"origin"
                }),
            )
            .await
            .expect("GitHub publish fixture should use its owned Git runner");
            (lookup, published)
        }

        let ((left_lookup, left_published), (right_lookup, right_published)) =
            tokio::join!(publish("left"), publish("right"));
        assert_eq!(left_lookup["nameWithOwner"], "owner/left");
        assert_eq!(right_lookup["nameWithOwner"], "owner/right");
        assert_eq!(left_published["branch"], "main");
        assert_eq!(right_published["branch"], "main");
    }

    #[tokio::test]
    async fn native_git_vcs_service_covers_repository_lifecycle_and_validation_paths() {
        let sandbox = crate::test_support::TestSandbox::new("native-git-vcs");
        let repository = sandbox.root().join("repository");
        tokio::fs::create_dir_all(&repository)
            .await
            .expect("repository directory should create");
        let cwd = repository.to_string_lossy().into_owned();
        let git_repository = Arc::new(GitRepository::with_runner_for_test(Arc::new(
            CapturedGitRunner::new(&sandbox),
        )));
        let mut services = GitVcsRpcServices::with_repository(git_repository, Arc::default());

        assert!(
            unary(&services, "vcs.init", json!({"cwd":cwd,"kind":"mercurial"}),)
                .await
                .is_err()
        );
        assert_eq!(
            unary(&services, "vcs.init", json!({"cwd":cwd,"kind":"git"}))
                .await
                .expect("repository should initialize"),
            Value::Null,
        );
        git(
            &sandbox,
            &repository,
            &["config", "user.name", "BiBCode Test"],
        )
        .await;
        git(
            &sandbox,
            &repository,
            &["config", "user.email", "bibcode@example.test"],
        )
        .await;

        tokio::fs::write(repository.join("tracked.txt"), "first\n")
            .await
            .expect("tracked file should write");
        let status = unary(&services, "vcs.refreshStatus", json!({"cwd":cwd}))
            .await
            .expect("status should refresh");
        assert!(
            status["workingTree"]["files"]
                .as_array()
                .is_some_and(|files| !files.is_empty())
        );
        assert_eq!(
            unary(
                &services,
                "vcs.generateCommitMessage",
                json!({"cwd":cwd,"filePaths":["tracked.txt"]}),
            )
            .await
            .expect("commit message should generate")["message"],
            "Update tracked.txt",
        );
        assert_eq!(
            unary(
                &services,
                "vcs.stageFiles",
                json!({"cwd":cwd,"filePaths":["tracked.txt"]}),
            )
            .await
            .expect("file should stage"),
            Value::Null,
        );
        git(
            &sandbox,
            &repository,
            &["commit", "--quiet", "-m", "initial"],
        )
        .await;
        git(&sandbox, &repository, &["branch", "-M", "main"]).await;

        let refs = unary(
            &services,
            "vcs.listRefs",
            json!({
                "cwd":cwd,
                "query":"",
                "cursor":0,
                "limit":500,
                "includeMatchingRemoteRefs":true,
                "refKind":"branch",
            }),
        )
        .await
        .expect("refs should list");
        assert!(
            refs["refs"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
        );
        let commits = unary(
            &services,
            "vcs.listCommits",
            json!({"cwd":cwd,"cursor":0,"limit":500}),
        )
        .await
        .expect("commits should list");
        assert!(
            commits["commits"]
                .as_array()
                .is_some_and(|items| !items.is_empty())
        );

        assert_eq!(
            unary(
                &services,
                "vcs.createRef",
                json!({"cwd":cwd,"refName":"feature","switchRef":true}),
            )
            .await
            .expect("feature ref should create")["refName"],
            "feature",
        );
        assert_eq!(
            unary(
                &services,
                "vcs.switchRef",
                json!({"cwd":cwd,"refName":"main"}),
            )
            .await
            .expect("base ref should switch")["refName"],
            "main",
        );

        tokio::fs::write(repository.join("tracked.txt"), "second\n")
            .await
            .expect("tracked file should change");
        unary(
            &services,
            "vcs.stageFiles",
            json!({"cwd":cwd,"filePaths":["tracked.txt"]}),
        )
        .await
        .expect("changed file should stage");
        unary(
            &services,
            "vcs.unstageFiles",
            json!({"cwd":cwd,"filePaths":["tracked.txt"]}),
        )
        .await
        .expect("changed file should unstage");
        unary(
            &services,
            "vcs.discardFiles",
            json!({"cwd":cwd,"filePaths":["tracked.txt"]}),
        )
        .await
        .expect("changed file should discard");

        let clone_parent = sandbox.root().join("clones");
        tokio::fs::create_dir_all(&clone_parent)
            .await
            .expect("clone parent should create");
        let cloned = unary(
            &services,
            "vcs.clone",
            json!({
                "url":repository,
                "parentDir":clone_parent,
                "directoryName":"copy",
            }),
        )
        .await
        .expect("repository should clone");
        assert!(
            cloned["path"]
                .as_str()
                .is_some_and(|path| path.ends_with("copy"))
        );

        assert!(
            unary(&services, "vcs.pull", json!({"cwd":cwd}))
                .await
                .is_err()
        );
        assert!(
            unary(
                &services,
                "git.resolvePullRequest",
                json!({"cwd":cwd,"reference":"current"}),
            )
            .await
            .is_err()
        );
        assert!(
            unary(
                &services,
                "sourceControl.lookupRepository",
                json!({"provider":"unknown","repository":"owner/name","cwd":cwd}),
            )
            .await
            .is_err()
        );
        let source_clone = sandbox.root().join("source-clone");
        assert!(
            unary(
                &services,
                "sourceControl.cloneRepository",
                json!({
                    "remoteUrl":format!("file://{}", repository.display()),
                    "destinationPath":source_clone,
                }),
            )
            .await
            .expect("source repository should clone")["cwd"]
                .is_string()
        );
        assert!(
            unary(
                &services,
                "sourceControl.publishRepository",
                json!({
                    "cwd":cwd,
                    "provider":"github",
                    "repository":"owner/name",
                    "visibility":"friends-only",
                }),
            )
            .await
            .is_err()
        );
        assert!(
            unary(
                &services,
                "git.preparePullRequestThread",
                json!({
                    "cwd":cwd,
                    "reference":"current",
                    "mode":"unsupported",
                    "threadId":"thread-1",
                }),
            )
            .await
            .is_err()
        );
        assert!(
            unary(
                &services,
                "shell.openInEditor",
                json!({"cwd":cwd,"editor":"missing-editor"}),
            )
            .await
            .is_err()
        );
        assert!(unary(&services, "unknown.method", json!({})).await.is_err());
        assert!(
            unary(&services, "vcs.listRefs", json!({"cwd":42}))
                .await
                .is_err()
        );

        let mut invalid_status = services.status_stream(
            rpc_request("subscribeVcsStatus", json!({"cwd":42})),
            CancellationToken::new(),
        );
        assert!(invalid_status.recv().await.expect("status error").is_err());
        let mut invalid_action = services.stacked_action_stream(
            rpc_request("git.runStackedAction", json!({"actionId":"invalid"})),
            CancellationToken::new(),
        );
        assert!(invalid_action.recv().await.expect("action error").is_err());

        let mut unsupported_action = services.stacked_action_stream(
            rpc_request(
                "git.runStackedAction",
                json!({
                    "actionId":"unsupported-action",
                    "cwd":cwd,
                    "action":"unsupported"
                }),
            ),
            CancellationToken::new(),
        );
        let started = unsupported_action
            .recv()
            .await
            .expect("action start chunk")
            .expect("action start event");
        assert_eq!(started[0]["kind"], "action_started");
        let failed = unsupported_action
            .recv()
            .await
            .expect("action failure chunk")
            .expect("action failure event");
        assert_eq!(failed[0]["kind"], "action_failed");
        assert!(
            failed[0]["message"]
                .as_str()
                .is_some_and(|message| message.contains("Unsupported Git action"))
        );

        let mut unavailable_status = services.status_stream(
            rpc_request("subscribeVcsStatus", json!({"cwd":"\u{0}"})),
            CancellationToken::new(),
        );
        assert!(
            unavailable_status
                .recv()
                .await
                .expect("unavailable status chunk")
                .is_err()
        );

        let branches =
            local_branch_names(&services.repository, &repository, &CancellationToken::new())
                .await
                .expect("local branches should list");
        assert!(branches.iter().any(|branch| branch == "main"));
        assert_eq!(
            sanitize_branch_fragment(" Feature: It's Ready! "),
            "feature-its-ready"
        );
        assert_eq!(sanitize_branch_fragment("..."), "update");
        assert_eq!(
            sanitize_feature_branch_name("Ready Now"),
            "feature/ready-now"
        );
        assert_eq!(
            sanitize_feature_branch_name("feature/already"),
            "feature/already",
        );
        assert_eq!(
            resolve_feature_branch_name(
                &["feature/update".to_owned(), "FEATURE/UPDATE-2".to_owned()],
                "feature/update",
            ),
            "feature/update-3",
        );
        assert_eq!(
            action_phases("commit_push_pr", true),
            vec!["branch", "commit", "push", "pr"]
        );
        assert_eq!(action_phases("unknown", false), Vec::<&str>::new());

        let (event_sender, mut event_receiver) = mpsc::channel(1);
        send_event(&event_sender, json!({"kind":"test"}))
            .await
            .expect("stream event should send");
        assert_eq!(
            event_receiver
                .recv()
                .await
                .expect("stream event")
                .expect("successful stream event"),
            vec![json!({"kind":"test"})],
        );
        drop(event_receiver);
        assert!(send_event(&event_sender, json!({})).await.is_err());

        assert_eq!(
            run_provider_json(
                Path::new("/bin/sh"),
                &["-c", "printf '{\"ok\":true}'"],
                Some(&repository),
                CancellationToken::new(),
                "fixture",
                "success",
            )
            .await
            .expect("provider JSON should decode"),
            json!({"ok":true}),
        );
        assert!(
            run_provider_json(
                Path::new("/bin/sh"),
                &["-c", "exit 1"],
                None,
                CancellationToken::new(),
                "fixture",
                "failure",
            )
            .await
            .is_err()
        );
        assert!(
            run_provider_json(
                Path::new("/bin/sh"),
                &["-c", "printf invalid"],
                None,
                CancellationToken::new(),
                "fixture",
                "invalid-json",
            )
            .await
            .is_err()
        );

        #[cfg(unix)]
        {
            let bare_remote = sandbox.root().join("published.git");
            tokio::fs::create_dir(&bare_remote)
                .await
                .expect("bare remote directory");
            git(&sandbox, &bare_remote, &["init", "--bare"]).await;
            let bare_remote_text = bare_remote.to_string_lossy().into_owned();
            git(
                &sandbox,
                &repository,
                &["remote", "add", "origin", bare_remote_text.as_str()],
            )
            .await;

            let gh = sandbox.executable_script(
                "gh",
                r#"case "$1:$2" in
  repo:view)
    printf '%s\n' '{"nameWithOwner":"owner/name","url":"https://github.test/owner/name","sshUrl":"git@github.test:owner/name.git"}'
    ;;
  repo:create)
    exit 0
    ;;
  *)
    exit 64
    ;;
esac
"#,
                "",
            );

            services.github_command = gh;

            let lookup = unary(
                &services,
                "sourceControl.lookupRepository",
                json!({
                    "provider":"github",
                    "repository":"owner/name",
                    "cwd":cwd
                }),
            )
            .await
            .expect("GitHub repository lookup should use the fixture CLI");
            assert_eq!(lookup["nameWithOwner"], "owner/name");

            let published = unary(
                &services,
                "sourceControl.publishRepository",
                json!({
                    "cwd":cwd,
                    "provider":"github",
                    "repository":"owner/name",
                    "visibility":"private",
                    "protocol":"ssh",
                    "remoteName":"origin"
                }),
            )
            .await
            .expect("GitHub repository publish should use the fixture CLI");
            assert_eq!(published["status"], "pushed");
            assert_eq!(published["branch"], "main");
            assert_eq!(published["remoteUrl"], "git@github.com:owner/name.git");
            assert_eq!(published["upstreamBranch"], "origin/main");
        }

        let invalid_action = StackedActionInput {
            action_id: "action-1".to_owned(),
            cwd: repository.clone(),
            action: "unsupported".to_owned(),
            commit_message: None,
            file_paths: None,
            feature_branch: None,
            commit_staged_index_as_is: None,
            pull_request_title: None,
            pull_request_body: None,
            pull_request_base_branch: Some("main".to_owned()),
            pull_request_head_branch: None,
            pull_request_options: None,
        };
        assert!(
            run_stacked_action(
                &services.repository,
                &services.pull_requests,
                &invalid_action,
                &CancellationToken::new(),
            )
            .await
            .is_err()
        );
        assert!(matches!(
            editor_launch_strategy(
                "missing-editor",
                vec!["--goto".to_owned(), repository.display().to_string()],
                repository.display().to_string(),
            ),
            EditorLaunchStrategy::Process { .. }
        ));

        assert_eq!(summarize_commit_context("", None), "");
        assert_eq!(
            summarize_commit_context("diff --git a/a.txt b/a.txt\n", None),
            "Update a.txt",
        );
        assert_eq!(
            summarize_commit_context("plain context", None),
            "Update working tree",
        );
        assert!(request_error("method", "detail").is_object());
        assert!(vcs_error("operation", &repository, "detail").is_object());
        assert!(source_control_error("unknown", "lookup", "detail").is_object());
    }

    #[test]
    fn options_must_fit_the_provider_before_anything_is_published() {
        let gitlab_ids = CreatePullRequestOptions {
            assignees: vec!["7".into()],
            milestone: Some(crate::source_control::MilestoneRef {
                id: "3".into(),
                title: "Sprint 9".into(),
            }),
            squash: Some(true),
            ..CreatePullRequestOptions::default()
        };
        assert!(validate_options_for_provider(&gitlab_ids, ProviderKind::Gitlab).is_ok());
        assert!(validate_options_for_provider(&gitlab_ids, ProviderKind::Github).is_err());
        let usernames = CreatePullRequestOptions {
            reviewers: vec!["alice".into()],
            ..CreatePullRequestOptions::default()
        };
        assert!(validate_options_for_provider(&usernames, ProviderKind::Gitlab).is_err());
        assert!(validate_options_for_provider(&usernames, ProviderKind::Github).is_ok());
        let draft = CreatePullRequestOptions {
            draft: true,
            ..CreatePullRequestOptions::default()
        };
        assert!(validate_options_for_provider(&draft, ProviderKind::AzureDevops).is_err());
        assert!(validate_options_for_provider(&draft, ProviderKind::Bitbucket).is_err());
        let none = CreatePullRequestOptions::default();
        assert!(validate_options_for_provider(&none, ProviderKind::AzureDevops).is_ok());
    }

    #[test]
    fn options_are_refused_on_non_request_actions() {
        let input: StackedActionInput = serde_json::from_value(json!({
            "actionId": "a", "cwd": "/repo", "action": "push",
            "pullRequestOptions": {"draft": true, "assignees": [], "reviewers": [], "labels": [],
                "milestone": null, "removeSourceBranch": null, "squash": null}
        }))
        .unwrap();
        assert!(validate_stacked_action_input(&input).is_err());
    }

    #[test]
    fn options_beyond_the_limits_are_refused() {
        let assignees: Vec<String> = (0..21).map(|index| index.to_string()).collect();
        let input: StackedActionInput = serde_json::from_value(json!({
            "actionId": "a", "cwd": "/repo", "action": "create_pr", "pullRequestBaseBranch": "main",
            "pullRequestOptions": {"draft": false, "assignees": assignees, "reviewers": [],
                "labels": [], "milestone": null, "removeSourceBranch": null, "squash": null}
        }))
        .unwrap();
        assert!(validate_stacked_action_input(&input).is_err());
    }

    /// A reviewed pull request: the fixture repository tracks a local bare
    /// remote for pushes while its fetch URL names GitHub so provider detection
    /// resolves, and the `gh` stub records every argument it receives.
    #[cfg(unix)]
    #[tokio::test]
    async fn create_pr_uses_the_reviewed_title_and_body_and_never_duplicates() {
        let sandbox = crate::test_support::TestSandbox::new("git-vcs-reviewed-pr");
        let repository = sandbox.root().join("repository");
        tokio::fs::create_dir_all(&repository)
            .await
            .expect("repository directory");
        let bare_remote = sandbox.root().join("remote.git");
        tokio::fs::create_dir_all(&bare_remote)
            .await
            .expect("bare remote directory");
        git(&sandbox, &bare_remote, &["init", "--bare", "-b", "main"]).await;
        git(&sandbox, &repository, &["init", "-q", "-b", "main"]).await;
        git(
            &sandbox,
            &repository,
            &["config", "user.email", "fixture@example.test"],
        )
        .await;
        git(&sandbox, &repository, &["config", "user.name", "Fixture"]).await;
        tokio::fs::write(repository.join("base.txt"), "base\n")
            .await
            .expect("base file");
        git(&sandbox, &repository, &["add", "base.txt"]).await;
        git(&sandbox, &repository, &["commit", "-q", "-m", "base"]).await;
        let bare_remote_text = bare_remote.to_string_lossy().into_owned();
        git(
            &sandbox,
            &repository,
            &["remote", "add", "origin", bare_remote_text.as_str()],
        )
        .await;
        git(
            &sandbox,
            &repository,
            &["push", "-q", "-u", "origin", "main"],
        )
        .await;
        git(
            &sandbox,
            &repository,
            &["switch", "-q", "-c", "feature/reviewed"],
        )
        .await;
        tokio::fs::write(repository.join("feature.txt"), "feature\n")
            .await
            .expect("feature file");
        git(&sandbox, &repository, &["add", "feature.txt"]).await;
        git(
            &sandbox,
            &repository,
            &["commit", "-q", "-m", "commit subject"],
        )
        .await;
        git(
            &sandbox,
            &repository,
            &[
                "remote",
                "set-url",
                "origin",
                "https://github.com/owner/name.git",
            ],
        )
        .await;
        git(
            &sandbox,
            &repository,
            &[
                "remote",
                "set-url",
                "--push",
                "origin",
                bare_remote_text.as_str(),
            ],
        )
        .await;

        let calls = sandbox.root().join("gh-calls");
        let calls_text = calls.to_string_lossy().into_owned();
        let gh = sandbox.executable_script(
            "gh",
            &format!(
                "printf '%s\\037' \"$@\" >> '{calls_text}'\nprintf '\\036' >> '{calls_text}'\ncase \"$1:$2\" in\n  pr:list) if [ -f '{calls_text}.existing' ]; then printf '%s\\n' '[{{\"number\":9,\"title\":\"Existing\",\"url\":\"https://github.com/owner/name/pull/9\",\"baseRefName\":\"main\",\"headRefName\":\"feature/reviewed\",\"state\":\"OPEN\"}}]'; else printf '[]\\n'; fi ;;\n  pr:create) printf '%s\\n' 'https://github.com/owner/name/pull/7' ;;\n  *) exit 64 ;;\nesac\n"
            ),
            "",
        );
        let pull_requests = PullRequestService::with_provider_commands(
            gh.to_string_lossy(),
            "unused-glab",
            "unused-az",
        );
        let git_repository = GitRepository::default();
        let reviewed: StackedActionInput = serde_json::from_value(json!({
            "actionId":"action-reviewed", "cwd":repository, "action":"create_pr",
            "pullRequestTitle":"  Reviewed title  ",
            "pullRequestBody":"Reviewed body\n\nwith detail",
            "pullRequestBaseBranch":"release/next"
        }))
        .unwrap();

        // Invalid targets are refused before either stack publishes anything.
        for action in ["create_pr", "commit_push_pr"] {
            for target in [
                None,
                Some(" "),
                Some("-bad"),
                Some("bad..ref"),
                Some("@{-1}"),
                Some("HEAD"),
                Some("feature/reviewed"),
            ] {
                let input: StackedActionInput = serde_json::from_value(json!({
                    "actionId": "invalid-target", "cwd": repository, "action": action,
                    "pullRequestBaseBranch": target,
                }))
                .unwrap();
                let error = run_stacked_action(
                    &git_repository,
                    &pull_requests,
                    &input,
                    &CancellationToken::new(),
                )
                .await
                .expect_err("target required and validated");
                assert!(
                    error.wire["detail"]
                        .as_str()
                        .unwrap()
                        .contains("target branch"),
                    "{error:?}"
                );
                assert!(
                    !calls.exists(),
                    "no provider command before target validation"
                );
                assert!(
                    !bare_remote.join("refs/heads/feature/reviewed").exists(),
                    "no branch published"
                );
            }
        }

        // Merge options are GitLab-only and are refused before anything is published.
        let github_merge_options: StackedActionInput = serde_json::from_value(json!({
            "actionId":"merge-options", "cwd":repository, "action":"create_pr",
            "pullRequestBaseBranch":"release/next",
            "pullRequestOptions": {"draft": false, "assignees": [], "reviewers": [], "labels": [],
                "milestone": null, "removeSourceBranch": null, "squash": true}
        }))
        .unwrap();
        let error = run_stacked_action(
            &git_repository,
            &pull_requests,
            &github_merge_options,
            &CancellationToken::new(),
        )
        .await
        .expect_err("GitLab-only merge options are refused for GitHub");
        assert!(
            error.wire["detail"]
                .as_str()
                .unwrap()
                .contains("Merge options apply only to GitLab"),
            "{error:?}"
        );
        assert!(!calls.exists(), "no provider command before the refusal");
        assert!(
            !bare_remote.join("refs/heads/feature/reviewed").exists(),
            "no branch published"
        );

        let result = run_stacked_action(
            &git_repository,
            &pull_requests,
            &reviewed,
            &CancellationToken::new(),
        )
        .await
        .expect("reviewed pull request creates");
        let result = serde_json::to_value(result).unwrap();

        assert_eq!(result["push"]["status"], "pushed");
        assert_eq!(result["pr"]["status"], "created");
        assert_eq!(result["pr"]["number"], 7);
        assert_eq!(result["pr"]["title"], "Reviewed title");
        assert_eq!(result["pr"]["baseBranch"], "release/next");
        assert_eq!(result["pr"]["headBranch"], "feature/reviewed");
        let recorded = tokio::fs::read_to_string(&calls).await.expect("gh calls");
        let invocations = recorded
            .split('\u{1e}')
            .filter(|call| !call.is_empty())
            .map(|call| {
                call.split('\u{1f}')
                    .filter(|argument| !argument.is_empty())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(invocations.len(), 2, "{recorded}");
        assert_eq!(invocations[0][..2], ["pr", "list"]);
        assert_eq!(
            invocations[1],
            [
                "pr",
                "create",
                "--base",
                "release/next",
                "--head",
                "feature/reviewed",
                "--title",
                "Reviewed title",
                "--body",
                "Reviewed body\n\nwith detail",
            ]
        );

        // A retry after the pull request exists must not create a duplicate.
        tokio::fs::write(format!("{calls_text}.existing"), "1")
            .await
            .expect("existing marker");
        tokio::fs::write(&calls, "").await.expect("reset gh calls");
        let retried = run_stacked_action(
            &git_repository,
            &pull_requests,
            &reviewed,
            &CancellationToken::new(),
        )
        .await
        .expect("retry resolves the existing pull request");
        let retried = serde_json::to_value(retried).unwrap();
        assert_eq!(retried["push"]["status"], "skipped_not_requested");
        assert_eq!(retried["pr"]["status"], "opened_existing");
        assert_eq!(retried["pr"]["number"], 9);
        let recorded = tokio::fs::read_to_string(&calls).await.expect("gh calls");
        assert!(!recorded.contains("create"), "{recorded}");
        assert!(retried["pr"].get("warning").is_none(), "{retried}");

        // Options chosen for a request that already exists are reported, not dropped silently.
        let mut with_options = serde_json::to_value(json!({
            "actionId":"action-reviewed-options", "cwd":repository, "action":"create_pr",
            "pullRequestTitle":"Reviewed title", "pullRequestBaseBranch":"release/next",
            "pullRequestOptions": {"draft": true, "assignees": [], "reviewers": [], "labels": [],
                "milestone": null, "removeSourceBranch": null, "squash": null}
        }))
        .unwrap();
        with_options["cwd"] = json!(repository);
        let with_options: StackedActionInput = serde_json::from_value(with_options).unwrap();
        let existing = run_stacked_action(
            &git_repository,
            &pull_requests,
            &with_options,
            &CancellationToken::new(),
        )
        .await
        .expect("existing request resolves");
        let existing = serde_json::to_value(existing).unwrap();
        assert_eq!(existing["pr"]["status"], "opened_existing");
        assert!(
            existing["pr"]["warning"]
                .as_str()
                .unwrap()
                .contains("the new options were not applied"),
            "{existing}"
        );

        // Explicit current-source publication keeps first-push upstream setup,
        // without replacing an upstream the user already chose.
        git(
            &sandbox,
            &repository,
            &["switch", "-q", "-c", "feature/explicit-current"],
        )
        .await;
        tokio::fs::remove_file(format!("{calls_text}.existing"))
            .await
            .unwrap();
        let explicit_current: StackedActionInput = serde_json::from_value(json!({
            "actionId": "explicit-current", "cwd": repository, "action": "create_pr",
            "pullRequestHeadBranch": "feature/explicit-current", "pullRequestBaseBranch": "main",
        }))
        .unwrap();
        for (configured_merge, expected_upstream) in [
            (None, "refs/remotes/origin/feature/explicit-current"),
            (Some("refs/heads/main"), "refs/remotes/origin/main"),
            (
                Some("refs/heads/unfetched"),
                "refs/remotes/origin/unfetched",
            ),
        ] {
            if let Some(merge) = configured_merge {
                git(
                    &sandbox,
                    &repository,
                    &["config", "branch.feature/explicit-current.merge", merge],
                )
                .await;
            }
            run_stacked_action(
                &git_repository,
                &pull_requests,
                &explicit_current,
                &CancellationToken::new(),
            )
            .await
            .expect("explicit current-source publication");
            let upstream = git_repository
                .run(
                    "test.selectedSourceUpstream",
                    &repository,
                    &[
                        "for-each-ref".to_owned(),
                        "--format=%(upstream)".to_owned(),
                        "refs/heads/feature/explicit-current".to_owned(),
                    ],
                    &CancellationToken::new(),
                )
                .await
                .unwrap();
            assert_eq!(upstream.stdout.trim(), expected_upstream);
        }
        git(
            &sandbox,
            &repository,
            &[
                "config",
                "remote.origin.fetch",
                "+refs/heads/main:refs/remotes/origin/main",
            ],
        )
        .await;
        run_stacked_action(
            &git_repository,
            &pull_requests,
            &explicit_current,
            &CancellationToken::new(),
        )
        .await
        .expect("an unmapped upstream stays configured");
        let configured_merge = git_repository
            .run(
                "test.selectedSourceMerge",
                &repository,
                &[
                    "config".to_owned(),
                    "--get".to_owned(),
                    "branch.feature/explicit-current.merge".to_owned(),
                ],
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(configured_merge.stdout.trim(), "refs/heads/unfetched");

        let misplaced = StackedActionInput {
            action: "commit".to_owned(),
            pull_request_title: None,
            pull_request_body: None,
            pull_request_base_branch: Some("main".to_owned()),
            ..reviewed
        };
        let error = run_stacked_action(
            &git_repository,
            &pull_requests,
            &misplaced,
            &CancellationToken::new(),
        )
        .await
        .expect_err("a target outside a pull request action is rejected");
        assert_eq!(
            error.wire["detail"],
            "A pull request title, body, source or target branch applies only to pull request actions."
        );
    }

    #[tokio::test]
    async fn create_pr_uses_selected_source_without_touching_dirty_checkout() {
        let sandbox = crate::test_support::TestSandbox::new("git-vcs-selected-pr-source");
        let repository = sandbox.root().join("repository");
        let bare_remote = sandbox.root().join("remote.git");
        for directory in [&repository, &bare_remote] {
            tokio::fs::create_dir_all(directory).await.unwrap();
        }
        git(&sandbox, &bare_remote, &["init", "--bare", "-b", "main"]).await;
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "fixture@example.test"],
            vec!["config", "user.name", "Fixture"],
        ] {
            git(&sandbox, &repository, &args).await;
        }
        tokio::fs::write(repository.join("base.txt"), "base\n")
            .await
            .unwrap();
        git(&sandbox, &repository, &["add", "base.txt"]).await;
        git(&sandbox, &repository, &["commit", "-q", "-m", "base"]).await;
        let bare_remote_text = bare_remote.to_string_lossy().into_owned();
        git(
            &sandbox,
            &repository,
            &["remote", "add", "origin", &bare_remote_text],
        )
        .await;
        git(
            &sandbox,
            &repository,
            &["push", "-q", "-u", "origin", "main"],
        )
        .await;
        git(
            &sandbox,
            &repository,
            &["switch", "-q", "-c", "feature/source"],
        )
        .await;
        git(
            &sandbox,
            &repository,
            &["push", "-q", "origin", "feature/source"],
        )
        .await;
        git(
            &sandbox,
            &repository,
            &["commit", "-q", "--allow-empty", "-m", "Selected commit"],
        )
        .await;
        git(
            &sandbox,
            &repository,
            &["tag", "-a", "unrequested", "-m", "Do not publish"],
        )
        .await;
        git(&sandbox, &repository, &["switch", "-q", "main"]).await;
        git(
            &sandbox,
            &repository,
            &["commit", "-q", "--allow-empty", "-m", "Current commit"],
        )
        .await;
        for args in [
            vec!["config", "push.default", "matching"],
            vec!["config", "push.followTags", "true"],
            vec![
                "config",
                "remote.origin.push",
                "refs/heads/main:refs/heads/unrequested",
            ],
            vec![
                "remote",
                "set-url",
                "origin",
                "https://github.com/owner/name.git",
            ],
            vec!["remote", "set-url", "--push", "origin", &bare_remote_text],
        ] {
            git(&sandbox, &repository, &args).await;
        }
        tokio::fs::write(repository.join("base.txt"), "staged\n")
            .await
            .unwrap();
        git(&sandbox, &repository, &["add", "base.txt"]).await;
        tokio::fs::write(repository.join("base.txt"), "dirty\n")
            .await
            .unwrap();
        tokio::fs::write(repository.join("untracked.txt"), "untracked\n")
            .await
            .unwrap();
        let original_index = std::fs::read(repository.join(".git/index")).unwrap();
        let original_head = std::fs::read(repository.join(".git/refs/heads/main")).unwrap();
        let original_remote = std::fs::read(bare_remote.join("refs/heads/main")).unwrap();
        let selected_head =
            std::fs::read(repository.join(".git/refs/heads/feature/source")).unwrap();
        let calls = sandbox.root().join("gh-calls");
        let calls_text = calls.to_string_lossy().into_owned();
        let existing_marker = sandbox.root().join("existing");
        let gh = sandbox.executable_script(
            "gh",
            &format!(
                "printf '%s\\037' \"$@\" >> '{calls_text}'\nprintf '\\036' >> '{calls_text}'\ncase \"$1:$2\" in\n  pr:list) if [ -f '{}' ]; then printf '%s\\n' '[{{\"number\":9,\"title\":\"Existing\",\"url\":\"https://github.com/owner/name/pull/9\",\"baseRefName\":\"main\",\"headRefName\":\"feature/source\",\"state\":\"OPEN\"}}]'; else printf '[]\\n'; fi ;;\n  pr:create) printf '%s\\n' 'https://github.com/owner/name/pull/7' ;;\n  *) exit 64 ;;\nesac\n",
                existing_marker.display(),
            ),
            "",
        );
        let pull_requests = PullRequestService::with_provider_commands(
            gh.to_string_lossy(),
            "unused-glab",
            "unused-az",
        );
        let git_repository =
            GitRepository::with_runner_for_test(Arc::new(CapturedGitRunner::new(&sandbox)));
        let cancellation = CancellationToken::new();

        for (action, source, feature_branch) in [
            ("create_pr", "", false),
            ("create_pr", "-bad", false),
            ("create_pr", "bad..ref", false),
            ("create_pr", "@{-1}", false),
            ("create_pr", "HEAD", false),
            ("create_pr", "missing", false),
            ("create_pr", "feature", false),
            ("create_pr", "unrequested", false),
            ("create_pr", "main", false),
            ("commit_push_pr", "feature/source", false),
            ("commit_push_pr", "feature/source", true),
            ("push", "feature/source", false),
        ] {
            let input: StackedActionInput = serde_json::from_value(json!({
                "actionId": "invalid-source", "cwd": repository, "action": action,
                "pullRequestHeadBranch": source,
                "pullRequestBaseBranch": if action == "push" { None } else if source == "main" { Some("main") } else { Some("release/next") },
                "featureBranch": feature_branch, "commitMessage": "Do not commit",
            }))
            .unwrap();
            let error = run_stacked_action(&git_repository, &pull_requests, &input, &cancellation)
                .await
                .expect_err("invalid source rejected before mutations");
            assert!(
                error.wire["detail"].as_str().unwrap().contains("branch"),
                "{error:?}"
            );
            assert!(!calls.exists(), "no provider call for {source}");
            assert_eq!(
                std::fs::read(repository.join(".git/index")).unwrap(),
                original_index
            );
            assert_eq!(
                std::fs::read(repository.join(".git/refs/heads/main")).unwrap(),
                original_head
            );
            assert_eq!(
                std::fs::read(bare_remote.join("refs/heads/feature/source")).unwrap(),
                original_remote
            );
        }

        let input: StackedActionInput = serde_json::from_value(json!({
            "actionId": "selected-source", "cwd": repository, "action": "create_pr",
            "pullRequestHeadBranch": "feature/source", "pullRequestBaseBranch": "main",
        }))
        .unwrap();
        let result = run_stacked_action(&git_repository, &pull_requests, &input, &cancellation)
            .await
            .expect("selected branch publishes with a dirty current checkout");
        let result = serde_json::to_value(result).unwrap();
        assert_eq!(result["push"]["branch"], "feature/source");
        assert_eq!(result["pr"]["headBranch"], "feature/source");
        assert_eq!(result["pr"]["baseBranch"], "main");
        assert_eq!(result["pr"]["title"], "Update feature/source");
        assert_eq!(
            std::fs::read(bare_remote.join("refs/heads/feature/source")).unwrap(),
            selected_head
        );
        assert_eq!(
            std::fs::read(bare_remote.join("refs/heads/main")).unwrap(),
            original_remote
        );
        assert!(!bare_remote.join("refs/heads/unrequested").exists());
        assert!(!bare_remote.join("refs/tags/unrequested").exists());
        let upstream = git_repository
            .run(
                "test.selectedSourceUpstream",
                &repository,
                &[
                    "for-each-ref".to_owned(),
                    "--format=%(upstream)".to_owned(),
                    "refs/heads/feature/source".to_owned(),
                ],
                &cancellation,
            )
            .await
            .unwrap();
        assert!(
            upstream.stdout.trim().is_empty(),
            "another source's upstream stays untouched"
        );
        let recorded = std::fs::read_to_string(&calls).unwrap();
        assert!(
            recorded.contains("--head\u{1f}feature/source\u{1f}"),
            "{recorded}"
        );
        assert!(recorded.contains("--base\u{1f}main\u{1f}"), "{recorded}");
        assert!(
            recorded.contains("--title\u{1f}Update feature/source\u{1f}"),
            "{recorded}"
        );

        git(
            &sandbox,
            &repository,
            &["branch", "-f", "feature/source", "main"],
        )
        .await;
        tokio::fs::write(&calls, "").await.unwrap();
        run_stacked_action(&git_repository, &pull_requests, &input, &cancellation)
            .await
            .expect_err("a diverged source is never force-pushed");
        assert_eq!(
            std::fs::read(bare_remote.join("refs/heads/feature/source")).unwrap(),
            selected_head
        );
        assert!(
            std::fs::read_to_string(&calls).unwrap().is_empty(),
            "no provider call after push rejection"
        );
        git(
            &sandbox,
            &repository,
            &[
                "branch",
                "-f",
                "feature/source",
                std::str::from_utf8(&selected_head).unwrap().trim(),
            ],
        )
        .await;

        tokio::fs::write(&existing_marker, "1").await.unwrap();
        tokio::fs::write(&calls, "").await.unwrap();
        let retried = run_stacked_action(&git_repository, &pull_requests, &input, &cancellation)
            .await
            .expect("existing request resolves by selected head");
        assert_eq!(
            serde_json::to_value(retried).unwrap()["pr"]["status"],
            "opened_existing"
        );
        let recorded = std::fs::read_to_string(&calls).unwrap();
        assert!(
            recorded.contains("--head\u{1f}feature/source\u{1f}"),
            "{recorded}"
        );
        assert!(!recorded.contains("create"), "{recorded}");

        // A fetched origin-only branch is already published; even an unusable
        // push URL must not prevent creating its request.
        git(&sandbox, &repository, &["branch", "-D", "feature/source"]).await;
        git(
            &sandbox,
            &repository,
            &[
                "remote",
                "set-url",
                "--push",
                "origin",
                "/missing-push-remote.git",
            ],
        )
        .await;
        tokio::fs::remove_file(&existing_marker).await.unwrap();
        let remote_only =
            run_stacked_action(&git_repository, &pull_requests, &input, &cancellation)
                .await
                .expect("origin-only source needs no push or checkout");
        let remote_only = serde_json::to_value(remote_only).unwrap();
        assert_eq!(remote_only["push"]["status"], "skipped_not_requested");
        assert_eq!(remote_only["pr"]["headBranch"], "feature/source");
        let local = git_repository
            .local_status(&repository, &cancellation)
            .await
            .unwrap();
        assert_eq!(local.ref_name.as_deref(), Some("main"));
        assert_eq!(
            std::fs::read(repository.join(".git/refs/heads/main")).unwrap(),
            original_head
        );
        assert_eq!(
            std::fs::read(repository.join(".git/index")).unwrap(),
            original_index
        );
        assert_eq!(
            std::fs::read_to_string(repository.join("base.txt")).unwrap(),
            "dirty\n"
        );
        assert_eq!(
            std::fs::read_to_string(repository.join("untracked.txt")).unwrap(),
            "untracked\n"
        );
        assert!(!repository.join(".git/refs/heads/feature/source").exists());
    }

    /// The provider is resolved before anything is published: an unidentified host
    /// is refused with the next steps and the branch stays local; once an explicit
    /// probe recorded the host, the same action publishes, creates the request and
    /// tells the Pull Requests service, so its cached list totals refresh.
    #[tokio::test]
    async fn create_pr_resolves_the_provider_before_publishing_the_branch() {
        let sandbox = crate::test_support::TestSandbox::new("git-vcs-pr-provider-first");
        let repository = sandbox.root().join("repository");
        let bare_remote = sandbox.root().join("remote.git");
        tokio::fs::create_dir_all(&repository)
            .await
            .expect("repository directory");
        tokio::fs::create_dir_all(&bare_remote)
            .await
            .expect("bare remote directory");
        git(&sandbox, &bare_remote, &["init", "--bare", "-b", "main"]).await;
        git(&sandbox, &repository, &["init", "-q", "-b", "main"]).await;
        git(
            &sandbox,
            &repository,
            &["config", "user.email", "fixture@example.test"],
        )
        .await;
        git(&sandbox, &repository, &["config", "user.name", "Fixture"]).await;
        tokio::fs::write(repository.join("base.txt"), "base\n")
            .await
            .expect("base file");
        git(&sandbox, &repository, &["add", "base.txt"]).await;
        git(&sandbox, &repository, &["commit", "-q", "-m", "base"]).await;
        let bare_remote_text = bare_remote.to_string_lossy().into_owned();
        git(
            &sandbox,
            &repository,
            &["remote", "add", "origin", bare_remote_text.as_str()],
        )
        .await;
        git(
            &sandbox,
            &repository,
            &["push", "-q", "-u", "origin", "main"],
        )
        .await;
        git(
            &sandbox,
            &repository,
            &["switch", "-q", "-c", "feature/unpublished"],
        )
        .await;
        tokio::fs::write(repository.join("feature.txt"), "feature\n")
            .await
            .expect("feature file");
        git(&sandbox, &repository, &["add", "feature.txt"]).await;
        git(&sandbox, &repository, &["commit", "-q", "-m", "feature"]).await;
        git(&sandbox, &repository, &["switch", "-q", "main"]).await;
        git(
            &sandbox,
            &repository,
            &[
                "remote",
                "set-url",
                "origin",
                "https://git.acme.example/team/repo.git",
            ],
        )
        .await;
        git(
            &sandbox,
            &repository,
            &[
                "remote",
                "set-url",
                "--push",
                "origin",
                bare_remote_text.as_str(),
            ],
        )
        .await;
        let calls = sandbox.root().join("glab-calls");
        let calls_text = calls.to_string_lossy().into_owned();
        let glab = sandbox.executable_script(
            "glab",
            &format!(
                "printf '%s\\n' \"$*\" >> '{calls_text}'\ncase \"$1:$2\" in\n  mr:list) printf '[]\\n' ;;\n  api:--method) printf '%s\\n' '{{\"iid\":3,\"title\":\"feature\",\"web_url\":\"https://git.acme.example/team/repo/-/merge_requests/3\",\"source_branch\":\"feature/unpublished\",\"target_branch\":\"main\",\"state\":\"opened\"}}' ;;\n  *) exit 64 ;;\nesac\n"
            ),
            "",
        );
        let recorder = Arc::new(RecordedCreatedRequests::default());
        let hosts = Arc::new(ProviderHosts::default());
        let mut services = GitVcsRpcServices::with_repository(
            Arc::new(GitRepository::default().with_provider_hosts(hosts.clone())),
            hosts,
        )
        .with_created_request_observer(recorder.clone());
        services.pull_requests = PullRequestService::with_provider_commands(
            "unused-gh",
            glab.to_string_lossy(),
            "unused-az",
        );
        let run = || async {
            let mut stream = services.stacked_action_stream(
                rpc_request(
                    "git.runStackedAction",
                    json!({
                        "actionId": "action-provider-first",
                        "cwd": repository,
                        "action": "create_pr",
                        "pullRequestTitle": "feature",
                        "pullRequestBaseBranch": "release/next",
                        "pullRequestHeadBranch": "feature/unpublished",
                    }),
                ),
                CancellationToken::new(),
            );
            let started = stream.recv().await.expect("start chunk").expect("start");
            assert_eq!(started[0]["kind"], "action_started");
            stream.recv().await.expect("final chunk").expect("final")[0].clone()
        };
        let published = bare_remote.join("refs/heads/feature/unpublished");

        let refused = run().await;
        assert_eq!(refused["kind"], "action_failed");
        assert_eq!(
            refused["message"],
            "Nothing was published. BiBCode hasn't identified git.acme.example yet. Open Pull Requests for this project or run Rescan in Settings → Source Control, then try again."
        );
        assert!(!published.exists(), "the branch was not published");
        assert!(!calls.exists(), "no provider command ran");
        assert!(recorder.0.lock().expect("recorder").is_empty());

        services.provider_hosts.record(
            "git.acme.example",
            crate::source_control::ProviderKind::Gitlab,
        );
        let finished = run().await;
        assert_eq!(finished["kind"], "action_finished", "{finished}");
        let result = &finished["result"];
        assert_eq!(result["push"]["status"], "pushed");
        assert_eq!(result["pr"]["status"], "created");
        assert_eq!(result["pr"]["number"], 3);
        assert!(published.exists());
        let calls = std::fs::read_to_string(&calls).expect("glab calls");
        assert!(
            calls.contains("mr list --source-branch feature/unpublished"),
            "{calls}"
        );
        assert!(
            calls.contains("source_branch=feature/unpublished")
                && calls.contains("target_branch=release/next")
                && calls.contains("title=feature"),
            "{calls}"
        );
        assert_eq!(
            *recorder.0.lock().expect("recorder"),
            [(
                repository.clone(),
                "https://git.acme.example/team/repo.git".to_owned()
            )],
            "the created request refreshes the Pull Requests totals"
        );
    }

    /// Records the requests `git.runStackedAction` reports as created.
    #[derive(Default)]
    struct RecordedCreatedRequests(Mutex<Vec<(PathBuf, String)>>);

    impl CreatedRequestObserver for RecordedCreatedRequests {
        fn request_created(&self, cwd: &Path, remote: &str) {
            self.0
                .lock()
                .expect("recorder")
                .push((cwd.to_path_buf(), remote.to_owned()));
        }
    }

    /// A feature-branch `commit_push_pr` for an unidentified host is refused before
    /// the branch or the commit exists, so the checkout stays exactly as it was.
    #[tokio::test]
    async fn commit_push_pr_on_a_feature_branch_refuses_before_branching() {
        let sandbox = crate::test_support::TestSandbox::new("git-vcs-pr-feature-branch");
        let repository = sandbox.root().join("repository");
        tokio::fs::create_dir_all(&repository)
            .await
            .expect("repository directory");
        git(&sandbox, &repository, &["init", "-q", "-b", "main"]).await;
        git(
            &sandbox,
            &repository,
            &["config", "user.email", "fixture@example.test"],
        )
        .await;
        git(&sandbox, &repository, &["config", "user.name", "Fixture"]).await;
        tokio::fs::write(repository.join("base.txt"), "base\n")
            .await
            .expect("base file");
        git(&sandbox, &repository, &["add", "base.txt"]).await;
        git(&sandbox, &repository, &["commit", "-q", "-m", "base"]).await;
        git(
            &sandbox,
            &repository,
            &[
                "remote",
                "add",
                "origin",
                "https://git.acme.example/team/repo.git",
            ],
        )
        .await;
        tokio::fs::write(repository.join("base.txt"), "changed\n")
            .await
            .expect("working tree change");
        let git_repository = GitRepository::default();
        let input = StackedActionInput {
            action_id: "action-feature-branch".to_owned(),
            cwd: repository.clone(),
            action: "commit_push_pr".to_owned(),
            commit_message: Some("feature work".to_owned()),
            file_paths: None,
            feature_branch: Some(true),
            commit_staged_index_as_is: None,
            pull_request_title: None,
            pull_request_body: None,
            pull_request_base_branch: Some("main".to_owned()),
            pull_request_head_branch: None,
            pull_request_options: None,
        };

        let error = run_stacked_action(
            &git_repository,
            &PullRequestService::with_provider_commands("unused-gh", "unused-glab", "unused-az"),
            &input,
            &CancellationToken::new(),
        )
        .await
        .expect_err("an unidentified host is refused");
        assert_eq!(
            error.wire["detail"],
            "Nothing was published. BiBCode hasn't identified git.acme.example yet. Open Pull Requests for this project or run Rescan in Settings → Source Control, then try again."
        );
        let heads = std::fs::read_dir(repository.join(".git/refs/heads"))
            .expect("branch refs")
            .map(|entry| entry.expect("branch ref").file_name())
            .collect::<Vec<_>>();
        assert_eq!(heads, ["main"], "no feature branch was created");
        let local = git_repository
            .local_status(&repository, &CancellationToken::new())
            .await
            .expect("local status");
        assert_eq!(local.ref_name.as_deref(), Some("main"));
        assert!(local.has_working_tree_changes, "nothing was committed");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn hosting_fixture_directory_pins_discovery_lookup_and_pull_request_commands() {
        let sandbox = tempfile::tempdir().expect("hosting sandbox");
        let directory =
            crate::test_support::hermetic_providers::missing_hosting_executable_dir(sandbox.path());
        let services =
            GitVcsRpcServices::with_repository(Arc::new(GitRepository::default()), Arc::default())
                .with_hosting_executable_dir_for_integration_test(directory.clone());
        assert_eq!(services.github_command, directory.join("gh"));
        assert_eq!(services.gitlab_command, directory.join("glab"));
        for (provider, executable) in [
            (ProviderKind::Github, "gh"),
            (ProviderKind::Gitlab, "glab"),
            (ProviderKind::AzureDevops, "az"),
        ] {
            assert_eq!(
                services
                    .pull_requests
                    .current_provider_command(provider)
                    .unwrap()
                    .executable,
                directory.join(executable),
                "pull-request commands must use the same hosting directory"
            );
        }
        let discovery = unary(&services, "server.discoverSourceControl", json!({}))
            .await
            .expect("discovery");
        for provider in ["github", "gitlab"] {
            let discovered = discovery["sourceControlProviders"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["kind"] == provider)
                .expect("hosting probe");
            assert_eq!(discovered["status"], "missing");
            let error = unary(
                &services,
                "sourceControl.lookupRepository",
                json!({"provider":provider,"repository":"acme/repo","cwd":sandbox.path()}),
            )
            .await
            .expect_err("missing hosting executable");
            assert_eq!(error["_tag"], "SourceControlRepositoryError");
            assert_eq!(error["provider"], provider);
            assert_eq!(error["operation"], "lookupRepository");
        }
    }

    /// Only an explicit Settings scan records hosts: a background discovery read,
    /// such as the publish dialog's, leaves the host observation untouched.
    #[cfg(unix)]
    #[tokio::test]
    async fn only_an_explicit_source_control_discovery_records_hosts() {
        let sandbox = crate::test_support::TestSandbox::new("git-vcs-discovery-hosts");
        let bin = sandbox.root().join("bin");
        std::fs::create_dir_all(&bin).expect("bin directory");
        let glab = bin.join("glab");
        let auth_output = sandbox.root().join("glab-auth.txt");
        std::fs::write(
            &auth_output,
            "git.acme.example\n  Logged in to git.acme.example as alice\n",
        )
        .expect("authenticated fixture");
        crate::test_support::executable_fixture::write_executable(
            &glab,
            format!(
                "#!/bin/sh\ncase \"$1\" in\n  --version) echo 'glab 1.114.0' ;;\n  auth) cat '{}' >&2 ;;\nesac\n",
                auth_output.display()
            ),
        );
        let hosts = Arc::new(ProviderHosts::default());
        let repository = Arc::new(GitRepository::default().with_provider_hosts(hosts.clone()));
        let services = GitVcsRpcServices {
            discovery: SourceControlDiscovery::with_executable_dir_for_integration_test(bin),
            ..GitVcsRpcServices::with_repository(repository, hosts.clone())
        };
        let discover = |payload: Value| {
            services.handle_unary_with_context(
                rpc_request("server.discoverSourceControl", payload),
                RpcSessionContext::unauthenticated(),
                CancellationToken::new(),
            )
        };

        let background = discover(json!({})).await.expect("background discovery");
        assert!(background.to_string().contains("git.acme.example"));
        assert_eq!(hosts.provider("git.acme.example"), None);

        discover(json!({ "recordHosts": true }))
            .await
            .expect("explicit discovery");
        assert_eq!(
            hosts.provider("git.acme.example"),
            Some(ProviderKind::Gitlab)
        );

        std::fs::write(
            &auth_output,
            "error: unexpected response from the keyring\n",
        )
        .expect("unrecognized fixture");
        let unknown = discover(json!({ "recordHosts": true }))
            .await
            .expect("rescan");
        assert!(
            unknown["sourceControlProviders"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["kind"] == "gitlab" && item["auth"]["status"] == "unknown")
        );
        assert_eq!(
            hosts.provider("git.acme.example"),
            Some(ProviderKind::Gitlab)
        );

        std::fs::write(
            &auth_output,
            include_str!("../../tests/fixtures/pull_requests/glab_logged_out.txt"),
        )
        .expect("logged out fixture");
        let logged_out = discover(json!({ "recordHosts": true }))
            .await
            .expect("rescan");
        assert!(logged_out["sourceControlProviders"].as_array().unwrap().iter().any(|item|
            item["kind"] == "gitlab" && item["auth"]["status"] == "unauthenticated"
        ));
        assert_eq!(hosts.provider("git.acme.example"), None);
    }

    #[tokio::test]
    async fn every_unary_method_rejects_malformed_payloads_through_its_typed_decoder() {
        let services = GitVcsRpcServices::default();
        for method in GIT_VCS_UNARY_METHODS {
            let result = services
                .handle_unary_with_context(
                    rpc_request(method, json!("not-an-object")),
                    RpcSessionContext::unauthenticated(),
                    CancellationToken::new(),
                )
                .await;
            assert!(
                result.is_err(),
                "{method} unexpectedly accepted a string payload"
            );
        }
    }

    #[cfg(target_os = "linux")]
    fn read_launch_capture(path: &Path) -> Vec<u8> {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match std::fs::read(path) {
                Ok(environment) => return environment,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "launcher did not finish"
                    );
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("read launcher environment: {error}"),
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn editor_launch_ignores_appimage_environment() {
        use crate::test_support::appimage_environment::{
            ENVIRONMENT_CASES, assert_child_environment, check_inherited_environment,
        };

        check_inherited_environment(
            "production::git_vcs::tests::editor_launch_ignores_appimage_environment",
            ENVIRONMENT_CASES,
            |expected| {
                let directory = tempfile::tempdir().expect("editor capture directory");
                let capture = directory.path().join("environment");
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("editor launch runtime");
                {
                    let _entered = runtime.enter();
                    launch_editor(&EditorLaunchStrategy::Process {
                        command: "/bin/sh".to_owned(),
                        args: vec![
                            "-c".to_owned(),
                            "/usr/bin/env -0 > \"$1.tmp\" && /bin/mv \"$1.tmp\" \"$1\"".to_owned(),
                            "editor-fixture".to_owned(),
                            capture.to_string_lossy().into_owned(),
                        ],
                    })
                    .expect("editor starts");
                }
                assert_child_environment(&read_launch_capture(&capture), expected);
            },
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn file_manager_launch_ignores_appimage_environment() {
        use crate::test_support::appimage_environment::{
            ENVIRONMENT_CASES, assert_child_environment, check_inherited_environment,
        };
        use crate::test_support::reexec;

        const CAPTURE: &str = "BIBCODE_FILE_MANAGER_ENVIRONMENT_CAPTURE";
        const TEST: &str =
            "production::git_vcs::tests::file_manager_launch_ignores_appimage_environment";
        if let Some(child) = reexec::enter(TEST, "file-manager") {
            let capture =
                PathBuf::from(std::env::var_os(CAPTURE).expect("file-manager capture path"));
            let env = crate::production::editor_launch::EditorProbeEnv {
                path_entries: Vec::new(),
                home: None,
                flatpak_export_dirs: Vec::new(),
                local_app_data: None,
                absolute_candidates_root: None,
            };
            open_in_editor_with(
                json!({ "cwd": capture, "editor": "file-manager" }),
                &env,
                |_| panic!("file manager must use its own launcher"),
            )
            .expect("file manager starts");
            let pid = String::from_utf8(read_launch_capture(&capture.with_extension("pid")))
                .expect("opener fixture PID text")
                .parse::<u32>()
                .expect("numeric opener fixture PID");
            let process = PathBuf::from(format!("/proc/{pid}"));
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while process.exists() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "opener was not reaped"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            child.complete();
            return;
        }

        check_inherited_environment(TEST, ENVIRONMENT_CASES, |expected| {
            // Only this nested process sees the fixture PATH; the parallel harness and
            // the shared AppImage fixture both keep their complete original environment.
            for opener in ["xdg-open", "gio"] {
                let directory = tempfile::tempdir().expect("file-manager fixture directory");
                let executable = directory.path().join(opener);
                crate::test_support::executable_fixture::write_executable(
                    &executable,
                    "#!/bin/sh\nfor target; do :; done\nprintf '%s' \"$$\" > \"$target.pid.tmp\" && /bin/mv \"$target.pid.tmp\" \"$target.pid\"\n/usr/bin/env -0 > \"$target.tmp\" && /bin/mv \"$target.tmp\" \"$target\"\nexit 9\n",
                );
                let capture = directory.path().join("environment");
                reexec::run(TEST, "file-manager", None, |command| {
                    command.env(CAPTURE, &capture).env("PATH", directory.path());
                });
                let mut expected = expected.clone();
                expected.insert("PATH", Some(directory.path().as_os_str().to_owned()));
                assert_child_environment(&read_launch_capture(&capture), &expected);
            }
        });
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn file_manager_launch_returns_typed_spawn_error() {
        use crate::test_support::reexec;

        const TEST: &str =
            "production::git_vcs::tests::file_manager_launch_returns_typed_spawn_error";
        if let Some(child) = reexec::enter(TEST, "missing-opener") {
            let env = crate::production::editor_launch::EditorProbeEnv {
                path_entries: Vec::new(),
                home: None,
                flatpak_export_dirs: Vec::new(),
                local_app_data: None,
                absolute_candidates_root: None,
            };
            let error = open_in_editor_with(
                json!({ "cwd": "/repo", "editor": "file-manager" }),
                &env,
                |_| panic!("file manager must use its own launcher"),
            )
            .expect_err("every opener is missing");
            assert_eq!(error["_tag"], "ExternalLauncherEditorSpawnError");
            assert_eq!(error["editor"], "file-manager");
            assert_eq!(error["target"], "/repo");
            assert_eq!(error["command"], "open");
            assert_eq!(error["args"], json!([]));
            assert!(
                !error["cause"]
                    .as_str()
                    .expect("spawn error detail")
                    .is_empty()
            );
            child.complete();
            return;
        }

        let directory = tempfile::tempdir().expect("empty opener path");
        reexec::run(TEST, "missing-opener", None, |command| {
            command.env("PATH", directory.path());
        });
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn file_manager_launch_returns_while_running_and_reaps_its_child() {
        use crate::test_support::reexec;

        const TEST: &str = "production::git_vcs::tests::file_manager_launch_returns_while_running_and_reaps_its_child";
        let Some(child) = reexec::enter(TEST, "lifecycle") else {
            reexec::run(TEST, "lifecycle", None, |_| {});
            return;
        };

        // Only this isolated fixture adopts detached grandchildren. That lets the
        // regression clean up a double-forked opener when its ownership check fails.
        // SAFETY: prctl has no pointer arguments here and changes only this process.
        assert_eq!(unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) }, 0);
        let directory = tempfile::tempdir().expect("file-manager lifecycle fixture");
        let pid_path = directory.path().join("pid");
        let release_path = directory.path().join("release");
        let mut command = std::process::Command::new("/bin/sh");
        command
            .args([
                "-c",
                "printf '%s' \"$$\" > \"$1.tmp\" && /bin/mv \"$1.tmp\" \"$1\"; while [ ! -f \"$2\" ]; do /bin/sleep 0.01; done; exit 17",
                "file-manager-fixture",
            ])
            .arg(&pid_path)
            .arg(&release_path);
        let (returned, received) = std::sync::mpsc::sync_channel(1);
        let launcher = std::thread::spawn(move || {
            let _ = returned.send(launch_file_manager_command(command));
        });
        let result = received.recv_timeout(Duration::from_secs(5));
        if result.is_err() {
            std::fs::write(&release_path, "exit").expect("release blocked opener");
        }
        launcher.join().expect("file-manager launch thread");
        result
            .expect("launch must return before the file manager exits")
            .expect("file-manager fixture starts");

        let pid = String::from_utf8(read_launch_capture(&pid_path))
            .expect("fixture PID text")
            .parse::<libc::pid_t>()
            .expect("numeric fixture PID");
        // SAFETY: getpgid reads the process group of the live fixture child.
        let process_group = unsafe { libc::getpgid(pid) };
        let process_path = PathBuf::from(format!("/proc/{pid}"));
        let was_running = std::fs::read_to_string(process_path.join("stat"))
            .expect("live fixture process")
            .split_once(") ")
            .expect("process state")
            .1
            .starts_with(|state: char| state != 'Z');
        std::fs::write(&release_path, "exit").expect("release opener");

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while process_path.exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut status = 0;
        // SAFETY: waitpid targets only the fixture PID and writes a valid status.
        // It also cleans up a zombie if the production launcher failed to reap it.
        let waited = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        let error = std::io::Error::last_os_error();
        assert!(was_running, "launch returned only after the opener exited");
        assert_eq!(process_group, pid, "opener must have its own process group");
        assert_eq!(waited, -1, "launcher must reap its exited child");
        assert_eq!(error.raw_os_error(), Some(libc::ECHILD));
        child.complete();
    }

    #[cfg(not(windows))]
    #[test]
    fn open_in_editor_launches_zed_with_the_bare_target() {
        let temp = tempfile::tempdir().unwrap();
        let zed = temp.path().join("zed");
        std::fs::write(&zed, b"#!/bin/sh\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&zed, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let env = crate::production::editor_launch::EditorProbeEnv {
            path_entries: vec![temp.path().to_path_buf()],
            home: None,
            flatpak_export_dirs: Vec::new(),
            local_app_data: None,
            absolute_candidates_root: None,
        };
        let captured = std::sync::Mutex::new(None);
        let result = open_in_editor_with(
            json!({ "cwd": "/repo/src/main.ts:4:2", "editor": "zed" }),
            &env,
            |strategy| {
                *captured.lock().unwrap() = Some(strategy.clone());
                Ok(())
            },
        );
        assert!(result.is_ok());
        assert_eq!(
            captured.into_inner().unwrap(),
            Some(EditorLaunchStrategy::Process {
                command: "zed".to_owned(),
                args: vec!["/repo/src/main.ts:4:2".to_owned()],
            })
        );
    }

    #[cfg(not(windows))]
    fn flatpak_export_env(
        exports: std::path::PathBuf,
    ) -> crate::production::editor_launch::EditorProbeEnv {
        let wrapper = exports.join("dev.zed.Zed");
        std::fs::write(&wrapper, b"#!/bin/sh\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        crate::production::editor_launch::EditorProbeEnv {
            path_entries: Vec::new(),
            home: None,
            flatpak_export_dirs: vec![exports],
            local_app_data: None,
            absolute_candidates_root: None,
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn open_in_editor_launches_flatpak_zed_with_a_filesystem_grant_for_a_file_target() {
        let temp = tempfile::tempdir().unwrap();
        let exports = temp.path().join("exports/bin");
        std::fs::create_dir_all(&exports).unwrap();
        let env = flatpak_export_env(exports);
        let captured = std::sync::Mutex::new(None);
        open_in_editor_with(
            json!({ "cwd": "/repo/src/main.ts:4:2", "editor": "zed" }),
            &env,
            |strategy| {
                *captured.lock().unwrap() = Some(strategy.clone());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            captured.into_inner().unwrap(),
            Some(EditorLaunchStrategy::Process {
                command: "flatpak".to_owned(),
                args: vec![
                    "run".to_owned(),
                    "--filesystem=/repo/src".to_owned(),
                    "dev.zed.Zed".to_owned(),
                    "/repo/src/main.ts:4:2".to_owned(),
                ],
            })
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn open_in_editor_launches_flatpak_zed_with_a_filesystem_grant_for_a_directory_target() {
        let temp = tempfile::tempdir().unwrap();
        let exports = temp.path().join("exports/bin");
        std::fs::create_dir_all(&exports).unwrap();
        let env = flatpak_export_env(exports);
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let project_str = project.to_string_lossy().into_owned();
        let captured = std::sync::Mutex::new(None);
        open_in_editor_with(
            json!({ "cwd": project_str, "editor": "zed" }),
            &env,
            |strategy| {
                *captured.lock().unwrap() = Some(strategy.clone());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(
            captured.into_inner().unwrap(),
            Some(EditorLaunchStrategy::Process {
                command: "flatpak".to_owned(),
                args: vec![
                    "run".to_owned(),
                    format!("--filesystem={project_str}"),
                    "dev.zed.Zed".to_owned(),
                    project_str.clone(),
                ],
            })
        );
    }

    #[test]
    fn open_in_editor_still_tries_the_path_name_when_nothing_resolved() {
        let env = crate::production::editor_launch::EditorProbeEnv {
            path_entries: Vec::new(),
            home: None,
            flatpak_export_dirs: Vec::new(),
            local_app_data: None,
            absolute_candidates_root: None,
        };
        let error =
            open_in_editor_with(json!({ "cwd": "/repo", "editor": "vscode" }), &env, |_| {
                Err(std::io::Error::other("spawn failed"))
            })
            .unwrap_err();
        assert_eq!(error["_tag"], "ExternalLauncherEditorSpawnError");
        assert_eq!(error["command"], "code");
    }
}

/// `vcs.refreshStatus` and the status stream share one short-lived open
/// pull-request lookup per (worktree, provider, branch). These tests drive the
/// real RPC handlers against a fake `gh` that records each `pr list` call.
#[cfg(all(test, unix))]
mod open_pull_request_lookup_tests {
    use super::*;
    use crate::RequestId;
    use crate::test_support::TestSandbox;

    /// Records the `--head` branch of every `pr list`, then answers from
    /// marker files: `fail` exits 1, `slow` sleeps its contents in seconds,
    /// and `open` returns one open PR for the requested branch.
    const FAKE_GH: &str = r#"#!/bin/sh
case "$1:$2" in
  pr:list)
    printf '%s\n' "$4" >> 'CALLS'
    if [ -f 'STATE/fail' ]; then echo 'forge unavailable' >&2; exit 1; fi
    if [ -f 'STATE/slow' ]; then sleep "$(cat 'STATE/slow')"; fi
    if [ -f 'STATE/open' ]; then
      printf '[{"number":42,"title":"PR for %s","url":"https://github.com/acme/repo/pull/42","baseRefName":"main","headRefName":"%s","state":"OPEN"}]\n' "$4" "$4"
    else
      printf '[]\n'
    fi ;;
  pr:create) : > 'STATE/open'; printf 'https://github.com/acme/repo/pull/42\n' ;;
  *) exit 64 ;;
esac
"#;

    struct Fixture {
        _sandbox: TestSandbox,
        repository: PathBuf,
        state: PathBuf,
        calls: PathBuf,
        services: GitVcsRpcServices,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let sandbox = TestSandbox::new(label);
            let repository = sandbox.path("repo");
            let bin = sandbox.path("bin");
            let state = sandbox.path("state");
            for directory in [&repository, &bin, &state] {
                std::fs::create_dir(directory).expect("fixture directory");
            }
            git(&repository, &["init", "-q", "-b", "main"]);
            git(
                &repository,
                &["commit", "-q", "--allow-empty", "-m", "base"],
            );
            git(
                &repository,
                &[
                    "remote",
                    "add",
                    "origin",
                    "https://github.com/acme/repo.git",
                ],
            );
            let calls = sandbox.path("gh-calls");
            TestSandbox::write_executable(
                &bin.join("gh"),
                &FAKE_GH
                    .replace("CALLS", &calls.to_string_lossy())
                    .replace("STATE", &state.to_string_lossy()),
            );
            let hosts = Arc::new(ProviderHosts::default());
            let services = GitVcsRpcServices::with_repository(
                Arc::new(GitRepository::default().with_provider_hosts(hosts.clone())),
                hosts,
            )
            .with_hosting_executable_dir_for_integration_test(bin);
            Self {
                _sandbox: sandbox,
                repository,
                state,
                calls,
                services,
            }
        }

        fn set(&self, marker: &str, contents: &str) {
            std::fs::write(self.state.join(marker), contents).expect("fixture marker");
        }

        fn clear(&self, marker: &str) {
            std::fs::remove_file(self.state.join(marker)).expect("fixture marker removal");
        }

        /// The `--head` branch of every `gh pr list` so far.
        fn lookups(&self) -> Vec<String> {
            std::fs::read_to_string(&self.calls)
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        }

        async fn unary(&self, tag: &str, payload: Value) -> Value {
            self.services
                .handle_unary_with_context(
                    RpcRequest {
                        id: RequestId::try_from("1").expect("request id"),
                        tag: tag.to_owned(),
                        payload,
                        headers: Vec::new(),
                        trace_id: None,
                        span_id: None,
                        sampled: None,
                    },
                    RpcSessionContext::unauthenticated(),
                    CancellationToken::new(),
                )
                .await
                .unwrap_or_else(|error| panic!("{tag} failed: {error}"))
        }

        async fn refresh(&self) -> Value {
            self.unary("vcs.refreshStatus", json!({ "cwd": self.repository }))
                .await
        }
    }

    fn git(cwd: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(["-c", "user.name=BiBCode Test"])
            .args(["-c", "user.email=bibcode@example.test"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[tokio::test]
    async fn repeated_refresh_within_the_ttl_neither_reruns_nor_waits_on_the_forge() {
        let fixture = Fixture::new("pr-lookup-repeat");
        fixture.set("open", "");
        let first = fixture.refresh().await;
        assert_eq!(first["pr"]["number"], 42, "{first}");
        assert_eq!(fixture.lookups(), ["main"]);

        // A forge that now takes five seconds must not be on the reply path.
        fixture.set("slow", "5");
        let second = tokio::time::timeout(Duration::from_secs(2), fixture.refresh())
            .await
            .expect("a refresh inside the TTL must not wait on the forge CLI");
        assert_eq!(second["pr"]["number"], 42, "{second}");
        assert_eq!(
            fixture.lookups(),
            ["main"],
            "a refresh inside the TTL must not run the forge CLI again"
        );
    }

    #[tokio::test]
    async fn concurrent_refreshes_share_one_forge_lookup() {
        let fixture = Fixture::new("pr-lookup-coalesce");
        fixture.set("open", "");
        fixture.set("slow", "1");
        let (first, second, third) =
            tokio::join!(fixture.refresh(), fixture.refresh(), fixture.refresh());
        for status in [first, second, third] {
            assert_eq!(status["pr"]["number"], 42, "{status}");
        }
        assert_eq!(
            fixture.lookups(),
            ["main"],
            "concurrent refreshes must join one forge CLI run"
        );
    }

    #[tokio::test]
    async fn status_stream_and_refresh_share_one_forge_lookup() {
        let fixture = Fixture::new("pr-lookup-stream");
        fixture.set("open", "");
        let cancellation = CancellationToken::new();
        let mut stream = fixture.services.status_stream(
            RpcRequest {
                id: RequestId::try_from("2").expect("request id"),
                tag: "subscribeVcsStatus".to_owned(),
                payload: json!({ "cwd": fixture.repository }),
                headers: Vec::new(),
                trace_id: None,
                span_id: None,
                sampled: None,
            },
            cancellation.clone(),
        );
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let chunk = stream
                    .recv()
                    .await
                    .expect("status stream stays open")
                    .expect("status stream event");
                if chunk[0]["remote"]["pr"]["number"] == 42 {
                    break;
                }
            }
        })
        .await
        .expect("the stream must enrich its snapshot with the open PR");

        let status = fixture.refresh().await;
        assert_eq!(status["pr"]["number"], 42, "{status}");
        // Let the stream enrich the refresh's publication too.
        let _ = tokio::time::timeout(Duration::from_millis(500), async {
            while stream.recv().await.is_some() {}
        })
        .await;
        assert_eq!(
            fixture.lookups(),
            ["main"],
            "the unary reply and the stream must share one forge lookup"
        );
        cancellation.cancel();
    }

    #[tokio::test]
    async fn a_branch_change_runs_a_fresh_lookup_for_the_new_branch() {
        let fixture = Fixture::new("pr-lookup-branch");
        fixture.set("open", "");
        fixture.refresh().await;
        fixture.refresh().await;
        assert_eq!(fixture.lookups(), ["main"]);

        git(&fixture.repository, &["switch", "-q", "-c", "feature"]);
        let status = fixture.refresh().await;
        assert_eq!(status["pr"]["headRef"], "feature", "{status}");
        assert_eq!(fixture.lookups(), ["main", "feature"]);
    }

    #[tokio::test]
    async fn a_pull_request_created_through_bibcode_is_visible_on_the_next_refresh() {
        let fixture = Fixture::new("pr-lookup-create");
        git(&fixture.repository, &["switch", "-q", "-c", "feature"]);
        let before = fixture.refresh().await;
        assert!(before["pr"].is_null(), "{before}");
        fixture.refresh().await;
        assert_eq!(fixture.lookups(), ["feature"]);

        fixture
            .services
            .pull_requests
            .create(
                CreatePullRequestInput {
                    cwd: fixture.repository.clone(),
                    provider: ProviderKind::Github,
                    base_branch: "main".to_owned(),
                    head_branch: "feature".to_owned(),
                    title: "Feature".to_owned(),
                    body: String::new(),
                },
                &CancellationToken::new(),
            )
            .await
            .expect("the fake forge creates the PR");
        let after = fixture.refresh().await;
        assert_eq!(after["pr"]["number"], 42, "{after}");
        assert_eq!(fixture.lookups(), ["feature", "feature"]);
    }

    #[tokio::test]
    async fn a_finished_git_mutation_runs_a_fresh_lookup() {
        let fixture = Fixture::new("pr-lookup-mutation");
        fixture.set("open", "");
        fixture.refresh().await;
        fixture.refresh().await;
        assert_eq!(fixture.lookups(), ["main"]);

        // Any finished BiBCode Git mutation (a push from the Git Manager
        // included) may have changed what the forge reports for this branch.
        std::fs::write(fixture.repository.join("added.txt"), "added\n").expect("fixture file");
        fixture
            .unary(
                "vcs.stageFiles",
                json!({ "cwd": fixture.repository, "filePaths": ["added.txt"] }),
            )
            .await;
        fixture.refresh().await;
        fixture.refresh().await;
        assert_eq!(fixture.lookups(), ["main", "main"]);
    }

    #[tokio::test]
    async fn a_reported_local_change_runs_a_fresh_lookup() {
        let fixture = Fixture::new("pr-lookup-local-change");
        let cancellation = CancellationToken::new();
        // A terminal exit reaches the observer only for a watched worktree.
        let mut stream = fixture.services.status_stream(
            RpcRequest {
                id: RequestId::try_from("3").expect("request id"),
                tag: "subscribeVcsStatus".to_owned(),
                payload: json!({ "cwd": fixture.repository }),
                headers: Vec::new(),
                trace_id: None,
                span_id: None,
                sampled: None,
            },
            cancellation.clone(),
        );
        stream
            .recv()
            .await
            .expect("status stream stays open")
            .expect("status snapshot");
        let before = fixture.refresh().await;
        assert!(before["pr"].is_null(), "{before}");
        assert_eq!(fixture.lookups(), ["main"]);

        // `gh pr create` in a BiBCode terminal, then the command exits.
        fixture.set("open", "");
        fixture
            .services
            .broadcaster
            .notify_local_change(&fixture.repository)
            .await;
        let after = fixture.refresh().await;
        assert_eq!(after["pr"]["number"], 42, "{after}");
        cancellation.cancel();
    }

    #[tokio::test]
    async fn an_expired_lookup_asks_the_forge_again() {
        let fixture = Fixture::new("pr-lookup-expiry");
        fixture
            .services
            .pull_requests
            .set_open_lookup_ttl_for_test(Duration::from_secs(2));
        fixture.set("open", "");
        fixture.refresh().await;
        fixture.refresh().await;
        assert_eq!(fixture.lookups(), ["main"]);
        tokio::time::sleep(Duration::from_millis(2500)).await;
        fixture.refresh().await;
        assert_eq!(fixture.lookups(), ["main", "main"]);
    }

    #[tokio::test]
    async fn a_failed_lookup_is_not_remembered_as_no_pull_request() {
        let fixture = Fixture::new("pr-lookup-failure");
        fixture.set("fail", "");
        let failed = fixture.refresh().await;
        assert!(failed["pr"].is_null(), "{failed}");
        assert_eq!(fixture.lookups(), ["main"]);

        fixture.clear("fail");
        fixture.set("open", "");
        let recovered = fixture.refresh().await;
        assert_eq!(recovered["pr"]["number"], 42, "{recovered}");
        assert_eq!(fixture.lookups(), ["main", "main"]);
    }
}

#[cfg(test)]
mod stacked_failure_tests {
    use super::*;

    #[test]
    fn private_diagnostics_never_copy_wire_output_or_command_context() {
        let secret = "private-output-credential-sentinel";
        let git = GitCommandError {
            tag: "GitCommandError",
            operation: secret.into(),
            command: secret.into(),
            cwd: secret.into(),
            diagnostics: Some(Box::new(crate::git::GitCommandDiagnostics {
                exit_code: Some(23),
                ..Default::default()
            })),
            detail: secret.into(),
        };
        let facts = crate::git::ProcessError::Spawn {
            operation: secret.into(),
            command: secret.into(),
            source: std::io::Error::new(std::io::ErrorKind::PermissionDenied, secret),
        }
        .safe_facts();
        let provider = crate::source_control::SourceControlProviderError {
            tag: "SourceControlProviderError",
            provider: ProviderKind::Gitlab,
            operation: "createPullRequest".into(),
            cwd: secret.into(),
            command: Some(secret.into()),
            reference: Some(secret.into()),
            detail: secret.into(),
            command_failure: Some(Box::new(crate::source_control::ProviderCommandFailure {
                code: "host_rejected",
                host_detail: Some(secret.into()),
                process_failure: Some(facts),
            })),
        };
        for failure in [
            StackedActionFailure::from(git),
            StackedActionFailure::from(provider),
            StackedActionFailure::from(json!({"detail": secret})),
        ] {
            assert_eq!(
                failure.wire["detail"], secret,
                "client envelope is preserved"
            );
            assert!(!failure.diagnostic.contains(secret));
            assert!(!format!("{failure:?}").contains(secret));
        }
    }
}
