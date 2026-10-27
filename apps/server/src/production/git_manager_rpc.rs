//! Registers the Git Manager RPC contract surface.

use std::{path::PathBuf, sync::Arc, time::Duration};

use base64::{Engine, engine::general_purpose::STANDARD as BASE64_STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{
    git::{
        FileTrash, GitCommandError, GitManagerBlockedReason, GitRepository, NativeFileTrash,
        ProcessOutput, StatusBroadcaster, canonical_worktree_path_key,
        manager::{
            graph::{
                GitManagerGraphError, MAX_DIFF_BUFFER_SIZE, MAX_DIFF_LINE_CHARACTERS,
                MAX_REASONABLE_DIFF_SIZE, page,
            },
            guards::{GuardInput, evaluate_guards},
            in_progress::detect_in_progress_operation,
            merge::{self, GitManagerMergeError, GitManagerMergePreview},
            operations::{
                CoAuthor, CommitRequest, DiscardError, DiscardRequest,
                GitManagerOperationError as DomainOperationError, GitManagerOperationRequest,
                PartialSelectionError, PartialSelectionRequest, commit_arguments,
                commit_message_body, discard_partial, discard_paths, parse_undo_commit_message,
                run_branch_or_sync_operation, stage_partial, unstage_partial,
            },
            patch::{diff_generation, parse_working_tree_diff},
            refs::{
                GitManagerImageBlobError, GitManagerRefsError, GitManagerRefsSnapshot,
                build_refs_snapshot, read_image_blob,
            },
            stash::{self, GitManagerStashError},
            tags::{self, GitManagerTagError},
        },
        validate_pathspecs,
    },
    persistence::Repositories,
    rpc::{RpcRegistry, RpcRequest, RpcResult, RpcStreamChunk},
    source_control::{
        ProviderKind as SourceControlProviderKind, PullRequestService, ResolvePullRequestInput,
        checks::ProviderChecksResult,
    },
    worktree_catalog::{
        CatalogRefreshTrigger, ProjectMutationAttempt, WorkspaceAdmissionLease,
        WorkspaceAvailabilityRegistry, WorktreeCatalogService,
    },
};

const OPERATION_STREAM_CAPACITY: usize = 8;

pub const GIT_MANAGER_UNARY_METHODS: &[&str] = &[
    "gitManager.commit",
    "gitManager.discard",
    "gitManager.discardPartial",
    "gitManager.getCommits",
    "gitManager.getDiff",
    "gitManager.getRefs",
    "gitManager.getRemoteTags",
    "gitManager.getStashes",
    "gitManager.listPullRequests",
    "gitManager.previewMerge",
    "gitManager.stagePartial",
    "gitManager.undoCommit",
    "gitManager.unstagePartial",
];

pub const GIT_MANAGER_STREAM_METHODS: &[&str] =
    &["gitManager.runOperation", "subscribeGitManagerSignal"];

#[derive(Clone, Copy, Debug, Default)]
pub struct GitManagerRpcServices;

impl GitManagerRpcServices {
    #[cfg(test)]
    async fn handle_read_unary(
        self,
        request: RpcRequest,
        cancellation: CancellationToken,
    ) -> RpcResult {
        ConfiguredGitManagerRpcServices::default()
            .handle_read_unary(request, cancellation)
            .await
    }

    #[must_use]
    pub fn with_dependencies(
        repository: Arc<GitRepository>,
        broadcaster: StatusBroadcaster,
        catalog: WorktreeCatalogService,
        repositories: Repositories,
        availability: WorkspaceAvailabilityRegistry,
        trash: Arc<dyn FileTrash>,
    ) -> ConfiguredGitManagerRpcServices {
        ConfiguredGitManagerRpcServices {
            repository,
            broadcaster,
            catalog: Some(catalog),
            repositories: Some(repositories),
            availability: Some(availability),
            trash,
            pull_requests: PullRequestService::default(),
        }
    }
}

#[derive(Clone)]
pub struct ConfiguredGitManagerRpcServices {
    repository: Arc<GitRepository>,
    broadcaster: StatusBroadcaster,
    catalog: Option<WorktreeCatalogService>,
    repositories: Option<Repositories>,
    availability: Option<WorkspaceAvailabilityRegistry>,
    trash: Arc<dyn FileTrash>,
    pull_requests: PullRequestService,
}

/// For tests and stand-alone registration: its repository has no shared host
/// observation, so provider reads name only hosts identified by name. The server
/// passes its shared repository to `GitManagerRpcServices::with_dependencies`.
impl Default for ConfiguredGitManagerRpcServices {
    fn default() -> Self {
        let repository = Arc::new(GitRepository::default());
        Self {
            broadcaster: StatusBroadcaster::new(repository.clone(), Duration::from_secs(3_600), 8),
            repository,
            catalog: None,
            repositories: None,
            availability: None,
            trash: Arc::new(NativeFileTrash::default()),
            pull_requests: PullRequestService::default(),
        }
    }
}

impl From<GitManagerRpcServices> for ConfiguredGitManagerRpcServices {
    fn from(_services: GitManagerRpcServices) -> Self {
        Self::default()
    }
}

impl ConfiguredGitManagerRpcServices {
    #[must_use]
    pub fn with_pull_request_service(mut self, pull_requests: PullRequestService) -> Self {
        self.pull_requests = pull_requests;
        self
    }

    async fn not_implemented_unary(&self, request: RpcRequest) -> RpcResult {
        Err(not_implemented_error(&request.tag))
    }

    pub fn operation_stream(
        &self,
        request: RpcRequest,
        cancellation: CancellationToken,
    ) -> mpsc::Receiver<RpcStreamChunk> {
        let (sender, receiver) = mpsc::channel(OPERATION_STREAM_CAPACITY);
        let services = self.clone();
        tokio::spawn(async move {
            let input = match decode::<GitManagerOperationRequest>(request.payload, &request.tag) {
                Ok(input) => input,
                Err(error) => {
                    let _ = sender.send(Err(error)).await;
                    return;
                }
            };
            let operation = input.operation();
            let cwd = input.cwd().to_path_buf();
            let project_id = input.project_id().to_owned();
            let (Some(catalog), Some(repositories), Some(availability)) = (
                services.catalog.clone(),
                services.repositories.clone(),
                services.availability.clone(),
            ) else {
                send_terminal_operation_error(
                    &sender,
                    DomainOperationError {
                        operation: operation.to_owned(),
                        code: "service-unavailable".to_owned(),
                        message: "Git Manager operations are unavailable in this server runtime."
                            .to_owned(),
                        blocked: None,
                        outputs: Vec::new(),
                    },
                )
                .await;
                return;
            };
            let resolved_project_id = match resolve_project_id(&repositories, &cwd).await {
                Ok(resolved_project_id) if resolved_project_id == project_id => resolved_project_id,
                Ok(_) => {
                    send_terminal_operation_error(
                        &sender,
                        DomainOperationError {
                            operation: operation.to_owned(),
                            code: "project-mismatch".to_owned(),
                            message:
                                "The selected checkout does not belong to the requested project."
                                    .to_owned(),
                            blocked: None,
                            outputs: Vec::new(),
                        },
                    )
                    .await;
                    return;
                }
                Err(code) => {
                    send_terminal_operation_error(
                        &sender,
                        DomainOperationError {
                            operation: operation.to_owned(),
                            code: code.to_owned(),
                            message:
                                "The selected checkout is not owned by exactly one Coddy Studio project."
                                    .to_owned(),
                            blocked: None,
                            outputs: Vec::new(),
                        },
                    )
                    .await;
                    return;
                }
            };
            let admission = match availability.acquire_path_admission([cwd.as_path()]).await {
                Ok(admission) => admission,
                Err(_) => {
                    send_terminal_operation_error(
                        &sender,
                        DomainOperationError {
                            operation: operation.to_owned(),
                            code: "workspace-unavailable".to_owned(),
                            message: "The selected checkout is unavailable.".to_owned(),
                            blocked: None,
                            outputs: Vec::new(),
                        },
                    )
                    .await;
                    return;
                }
            };
            if sender
                .send(Ok(vec![json!({
                    "_tag": "started",
                    "operation": operation,
                })]))
                .await
                .is_err()
            {
                return;
            }

            debug_assert_eq!(resolved_project_id, project_id);
            let operation_cancellation = cancellation.child_token();
            let loss = admission.loss_cancellation();
            let operation_future = run_branch_or_sync_operation(
                services.repository.clone(),
                services.broadcaster.clone(),
                catalog,
                input,
                operation_cancellation.clone(),
            );
            tokio::pin!(operation_future);
            let result = tokio::select! {
                biased;
                () = loss.cancelled() => {
                    operation_cancellation.cancel();
                    let _ = operation_future.await;
                    Err(DomainOperationError {
                        operation: operation.to_owned(),
                        code: "workspace-unavailable".to_owned(),
                        message: "The selected checkout became unavailable during the operation."
                            .to_owned(),
                        blocked: None,
                        outputs: Vec::new(),
                    })
                }
                result = &mut operation_future => result,
            };
            match result {
                Ok(outcome) => {
                    if send_operation_outputs(&sender, &outcome.operation, &outcome.outputs)
                        .await
                        .is_err()
                    {
                        return;
                    }
                    let _ = sender
                        .send(Ok(vec![json!({
                            "_tag": "finished",
                            "operation": outcome.operation,
                            "message": outcome.message,
                        })]))
                        .await;
                }
                Err(error) => send_terminal_operation_error(&sender, error).await,
            }
        });
        receiver
    }

    pub async fn read_unary(
        &self,
        request: RpcRequest,
        cancellation: CancellationToken,
    ) -> RpcResult {
        self.handle_read_unary(request, cancellation).await
    }

    pub async fn mutation_unary(
        &self,
        request: RpcRequest,
        cancellation: CancellationToken,
    ) -> RpcResult {
        self.handle_mutation_unary(request, cancellation).await
    }

    pub fn git_manager_signal_stream(
        &self,
        request: RpcRequest,
        cancellation: CancellationToken,
    ) -> mpsc::Receiver<RpcStreamChunk> {
        let (sender, receiver) = mpsc::channel(OPERATION_STREAM_CAPACITY);
        let broadcaster = self.broadcaster.clone();
        tokio::spawn(async move {
            let input = match decode::<GitManagerCwdInput>(request.payload, &request.tag) {
                Ok(input) => input,
                Err(error) => {
                    let _ = sender.send(Err(error)).await;
                    return;
                }
            };
            let cwd = input.cwd.to_string_lossy().replace('\\', "/");
            let mut subscription = match broadcaster
                .subscribe_git_manager_signal(input.cwd, cancellation)
                .await
            {
                Ok(subscription) => subscription,
                Err(error) => {
                    let _ = sender
                        .send(Err(git_error("subscribeGitManagerSignal", error)))
                        .await;
                    return;
                }
            };
            while let Some(signal) = subscription.recv().await {
                if sender
                    .send(Ok(vec![json!({
                        "cwd": cwd,
                        "generation": signal.generation,
                        "watcherDegraded": signal.watcher_degraded,
                    })]))
                    .await
                    .is_err()
                {
                    return;
                }
            }
        });
        receiver
    }

    async fn handle_read_unary(
        &self,
        request: RpcRequest,
        cancellation: CancellationToken,
    ) -> RpcResult {
        match request.tag.as_str() {
            "gitManager.getRefs" => {
                let input: GitManagerCwdInput = decode(request.payload, &request.tag)?;
                encode_result(
                    self.refs_snapshot(&request.tag, &input.cwd, &cancellation)
                        .await,
                )
            }
            "gitManager.getCommits" => {
                let input: GitManagerGetCommitsInput = decode(request.payload, &request.tag)?;
                encode_result(
                    page(
                        &self.repository,
                        &input.cwd,
                        input.pinned_tips.as_deref(),
                        input.offset.unwrap_or(0),
                        input.limit.unwrap_or(100),
                        &cancellation,
                    )
                    .await
                    .map_err(|error| graph_error(&request.tag, error)),
                )
            }
            "gitManager.getDiff" => {
                let input: GitManagerGetDiffInput = decode(request.payload, &request.tag)?;
                get_diff(&self.repository, input, &cancellation).await
            }
            "gitManager.getStashes" => {
                let input: GitManagerCwdInput = decode(request.payload, &request.tag)?;
                encode_result(
                    stash::list_stashes(&self.repository, &input.cwd, &cancellation)
                        .await
                        .map_err(|error| stash_error(&request.tag, error)),
                )
            }
            "gitManager.getRemoteTags" => {
                let input: GitManagerRemoteTagsInput = decode(request.payload, &request.tag)?;
                encode_result(
                    tags::list_remote_tags(
                        &self.repository,
                        &input.cwd,
                        &input.remote,
                        &cancellation,
                    )
                    .await
                    .map_err(|error| tag_read_error(&request.tag, error)),
                )
            }
            "gitManager.previewMerge" => {
                let input: GitManagerPreviewMergeInput = decode(request.payload, &request.tag)?;
                let preview = merge::preview(
                    &self.repository,
                    &input.cwd,
                    &input.source,
                    input.target.as_deref(),
                    &cancellation,
                )
                .await
                .map_err(|error| merge_error(&request.tag, error))?;
                Ok(merge_preview_value(preview))
            }
            "gitManager.listPullRequests" => {
                let input: GitManagerCwdInput = decode(request.payload, &request.tag)?;
                self.list_pull_requests(&input.cwd, &cancellation).await
            }
            _ => self.not_implemented_unary(request).await,
        }
    }

    async fn handle_mutation_unary(
        &self,
        request: RpcRequest,
        cancellation: CancellationToken,
    ) -> RpcResult {
        match request.tag.as_str() {
            "gitManager.commit" => {
                let input: GitManagerCommitInput = decode(request.payload, &request.tag)?;
                validate_commit_input(&input)?;
                let commit = CommitRequest {
                    summary: input.summary,
                    description: Some(input.description),
                    amend: input.amend,
                    no_verify: input.no_verify,
                    signoff: input.signoff,
                    allow_empty: input.allow_empty,
                    co_authors: input.co_authors,
                };
                let args = commit_arguments(&commit);
                let message = commit_message_body(&commit).into_bytes();
                let allow_empty = commit.allow_empty;
                self.run_mutation(
                    "gitManager.commit",
                    input.cwd,
                    cancellation,
                    move |services, cwd, token| async move {
                        let snapshot = services
                            .refs_snapshot("gitManager.commit", &cwd, &token)
                            .await?;
                        if let Some(reason) = operation_blocked(&snapshot, "commit") {
                            return Err(blocked_error("gitManager.commit", reason));
                        }
                        let outcome = services
                            .repository
                            .commit_with_options(&cwd, &args, &message, allow_empty, &token)
                            .await
                            .map_err(|error| mutation_git_error("gitManager.commit", error))?;
                        Ok(json!({ "sha": outcome.sha, "empty": outcome.empty }))
                    },
                )
                .await
            }
            "gitManager.undoCommit" => {
                let input: GitManagerCwdInput = decode(request.payload, &request.tag)?;
                self.run_mutation(
                    "gitManager.undoCommit",
                    input.cwd,
                    cancellation,
                    move |services, cwd, token| async move {
                        let snapshot = services
                            .refs_snapshot("gitManager.undoCommit", &cwd, &token)
                            .await?;
                        if let Some(reason) = operation_blocked(&snapshot, "undo-commit") {
                            return Err(blocked_error("gitManager.undoCommit", reason));
                        }
                        let current = snapshot
                            .local_branches
                            .iter()
                            .find(|reference| reference.current)
                            .ok_or_else(|| {
                                operation_error(
                                    "gitManager.undoCommit",
                                    "commit-not-local",
                                    "Undo is blocked: HEAD is not a local branch commit.",
                                )
                            })?;
                        if current.upstream.is_some() && current.ahead == 0 {
                            return Err(operation_error(
                                "gitManager.undoCommit",
                                "commit-not-local",
                                "Undo is blocked: the HEAD commit is not local.",
                            ));
                        }
                        if !services
                            .repository
                            .git_manager_head_tags(&cwd, &token)
                            .await
                            .map_err(|error| mutation_git_error("gitManager.undoCommit", error))?
                            .is_empty()
                        {
                            return Err(operation_error(
                                "gitManager.undoCommit",
                                "tagged-commit",
                                "Undo is blocked: the HEAD commit has a tag.",
                            ));
                        }
                        let undone = services
                            .repository
                            .undo_head_commit(&cwd, &token)
                            .await
                            .map_err(|error| mutation_git_error("gitManager.undoCommit", error))?;
                        let mut draft = parse_undo_commit_message(&undone.message);
                        if draft.summary.trim().is_empty() {
                            draft.summary = "(no commit message)".to_owned();
                        }
                        Ok(json!({
                            "summary": draft.summary,
                            "description": draft.description,
                            "coAuthors": draft.co_authors,
                        }))
                    },
                )
                .await
            }
            "gitManager.stagePartial"
            | "gitManager.unstagePartial"
            | "gitManager.discardPartial" => {
                let operation = match request.tag.as_str() {
                    "gitManager.stagePartial" => PartialMutation::Stage,
                    "gitManager.unstagePartial" => PartialMutation::Unstage,
                    "gitManager.discardPartial" => PartialMutation::Discard,
                    _ => unreachable!("partial mutation tag was matched above"),
                };
                let input: GitManagerPartialSelectionInput =
                    decode(request.payload, operation.rpc_name())?;
                validate_pathspecs(
                    operation.git_operation_name(),
                    &input.cwd,
                    std::slice::from_ref(&input.path),
                )
                .map_err(|_| {
                    operation_error(
                        operation.rpc_name(),
                        "invalid-path",
                        "The requested partial-selection path is invalid.",
                    )
                })?;
                let project_id = input.project_id;
                let selection = PartialSelectionRequest {
                    cwd: input.cwd.clone(),
                    path: input.path,
                    selected_lines: input.selected_lines,
                    base_generation: input.base_generation,
                };
                self.run_mutation_for_project(
                    operation.rpc_name(),
                    input.cwd,
                    project_id,
                    operation.guard_name(),
                    cancellation,
                    move |services, _cwd, token| async move {
                        let outcome = match operation {
                            PartialMutation::Stage => {
                                stage_partial(&services.repository, &selection, &token).await
                            }
                            PartialMutation::Unstage => {
                                unstage_partial(&services.repository, &selection, &token).await
                            }
                            PartialMutation::Discard => {
                                discard_partial(&services.repository, &selection, &token).await
                            }
                        }
                        .map_err(|error| partial_selection_error(operation.rpc_name(), error))?;
                        let mut value = json!({ "generation": outcome.generation });
                        if let Some(reason) = outcome.fallback_reason
                            && let Some(fields) = value.as_object_mut()
                        {
                            fields.insert("fallbackReason".to_owned(), json!(reason));
                        }
                        Ok(value)
                    },
                )
                .await
            }
            "gitManager.discard" => {
                let input: GitManagerDiscardInput = decode(request.payload, &request.tag)?;
                if input.paths.is_empty() {
                    return Err(operation_error(
                        "gitManager.discard",
                        "invalid-request",
                        "At least one discard path is required.",
                    ));
                }
                validate_pathspecs("GitManager.discard", &input.cwd, &input.paths).map_err(
                    |_| {
                        operation_error(
                            "gitManager.discard",
                            "invalid-path",
                            "A requested discard path is invalid.",
                        )
                    },
                )?;
                let trash = self.trash.clone();
                self.run_mutation(
                    "gitManager.discard",
                    input.cwd.clone(),
                    cancellation,
                    move |services, cwd, token| async move {
                        let snapshot = services
                            .refs_snapshot("gitManager.discard", &cwd, &token)
                            .await?;
                        if let Some(reason) = operation_blocked(&snapshot, "discard") {
                            return Err(blocked_error("gitManager.discard", reason));
                        }
                        let outcome = discard_paths(
                            &services.repository,
                            trash,
                            DiscardRequest {
                                cwd,
                                paths: input.paths,
                                permit_permanent: input.permit_permanent,
                            },
                            &token,
                        )
                        .await
                        .map_err(|error| discard_error("gitManager.discard", error))?;
                        Ok(json!({
                            "trashed": outcome.trashed,
                            "permanentlyDiscarded": outcome.permanently_discarded,
                            "trashUnavailable": outcome.trash_unavailable,
                        }))
                    },
                )
                .await
            }
            _ => self.not_implemented_unary(request).await,
        }
    }

    async fn refs_snapshot(
        &self,
        operation: &str,
        cwd: &std::path::Path,
        cancellation: &CancellationToken,
    ) -> Result<GitManagerRefsSnapshot, Value> {
        let mut snapshot = build_refs_snapshot(&self.repository, cwd, cancellation)
            .await
            .map_err(|error| refs_error(operation, error))?;
        snapshot.in_progress_operation =
            detect_in_progress_operation(&self.repository, cwd, cancellation)
                .await
                .map_err(|_| {
                    operation_error(
                        operation,
                        "repository-state-unavailable",
                        "Git repository operation state could not be inspected.",
                    )
                })?;
        Ok(snapshot)
    }

    async fn run_mutation<F, Fut>(
        &self,
        operation: &'static str,
        cwd: PathBuf,
        cancellation: CancellationToken,
        action: F,
    ) -> RpcResult
    where
        F: FnOnce(Self, PathBuf, CancellationToken) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = RpcResult> + Send + 'static,
    {
        self.run_mutation_with_optional_project(operation, cwd, None, None, cancellation, action)
            .await
    }

    async fn run_mutation_for_project<F, Fut>(
        &self,
        operation: &'static str,
        cwd: PathBuf,
        project_id: String,
        guard_operation: &'static str,
        cancellation: CancellationToken,
        action: F,
    ) -> RpcResult
    where
        F: FnOnce(Self, PathBuf, CancellationToken) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = RpcResult> + Send + 'static,
    {
        self.run_mutation_with_optional_project(
            operation,
            cwd,
            Some(project_id),
            Some(guard_operation),
            cancellation,
            action,
        )
        .await
    }

    async fn run_mutation_with_optional_project<F, Fut>(
        &self,
        operation: &'static str,
        cwd: PathBuf,
        requested_project_id: Option<String>,
        guard_operation: Option<&'static str>,
        cancellation: CancellationToken,
        action: F,
    ) -> RpcResult
    where
        F: FnOnce(Self, PathBuf, CancellationToken) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = RpcResult> + Send + 'static,
    {
        let services = self.clone();
        await_server_owned_mutation(async move {
            let (Some(catalog), Some(repositories), Some(availability)) = (
                services.catalog.clone(),
                services.repositories.clone(),
                services.availability.clone(),
            ) else {
                return Err(operation_error(
                    operation,
                    "service-unavailable",
                    "Git Manager commit operations are unavailable in this server runtime.",
                ));
            };
            let project_id = resolve_project_id(&repositories, &cwd)
                .await
                .map_err(|code| {
                    operation_error(
                        operation,
                        code,
                        "The selected checkout is not owned by exactly one Coddy Studio project.",
                    )
                })?;
            if requested_project_id
                .as_deref()
                .is_some_and(|requested| requested != project_id)
            {
                return Err(operation_error(
                    operation,
                    "project-mismatch",
                    "The selected checkout does not belong to the requested project.",
                ));
            }
            catalog
                .refresh(&project_id, CatalogRefreshTrigger::Explicit)
                .await
                .map_err(|_| {
                    operation_error(
                        operation,
                        "repository-unavailable",
                        "The selected repository could not be revalidated.",
                    )
                })?;
            let admission = availability
                .acquire_path_admission([cwd.as_path()])
                .await
                .map_err(|_| {
                    operation_error(
                        operation,
                        "workspace-unavailable",
                        "The selected checkout is unavailable.",
                    )
                })?;
            let lock_cancellation = cancellation.child_token();
            let operation_cancellation = lock_cancellation.clone();
            let locked_services = services.clone();
            let locked_cwd = cwd.clone();
            match catalog
                .try_with_project_mutation_lock_cancellation(
                    &project_id,
                    &lock_cancellation,
                    || async move {
                        if let Some(guard_operation) = guard_operation {
                            let snapshot = locked_services
                                .refs_snapshot(operation, &locked_cwd, &operation_cancellation)
                                .await?;
                            if let Some(reason) = operation_blocked(&snapshot, guard_operation) {
                                return Err(blocked_error(operation, reason));
                            }
                        }
                        locked_services
                            .run_fenced_mutation(
                                operation,
                                locked_cwd,
                                admission,
                                operation_cancellation,
                                action,
                            )
                            .await
                    },
                )
                .await
            {
                ProjectMutationAttempt::Acquired(result) => result,
                ProjectMutationAttempt::InFlight => Err(blocked_error(
                    operation,
                    GitManagerBlockedReason {
                        operation: operation_guard_name(operation).to_owned(),
                        code: "operation-in-flight".to_owned(),
                        message: "Blocked: another Git Manager operation is already running."
                            .to_owned(),
                    },
                )),
                ProjectMutationAttempt::Cancelled => Err(operation_error(
                    operation,
                    "cancelled",
                    "The Git Manager operation was cancelled before admission.",
                )),
            }
        })
        .await
    }

    async fn run_fenced_mutation<F, Fut>(
        self,
        operation: &'static str,
        cwd: PathBuf,
        admission: WorkspaceAdmissionLease,
        cancellation: CancellationToken,
        action: F,
    ) -> RpcResult
    where
        F: FnOnce(Self, PathBuf, CancellationToken) -> Fut,
        Fut: std::future::Future<Output = RpcResult>,
    {
        let loss = admission.loss_cancellation();
        let mutation = tokio::select! {
            biased;
            () = loss.cancelled() => {
                cancellation.cancel();
                return Err(operation_error(
                    operation,
                    "workspace-unavailable",
                    "The selected checkout became unavailable before mutation.",
                ));
            }
            () = cancellation.cancelled() => {
                return Err(operation_error(
                    operation,
                    "cancelled",
                    "The Git Manager operation was cancelled before mutation.",
                ));
            }
            mutation = self.broadcaster.begin_mutation(&cwd) => mutation,
        };
        let operation_future = action(self, cwd, cancellation.clone());
        tokio::pin!(operation_future);
        let result = tokio::select! {
            biased;
            () = loss.cancelled() => {
                cancellation.cancel();
                let _ = operation_future.await;
                Err(operation_error(
                    operation,
                    "workspace-unavailable",
                    "The selected checkout became unavailable during mutation.",
                ))
            }
            result = &mut operation_future => result,
        };
        mutation.finish().await;
        result
    }

    async fn list_pull_requests(
        &self,
        cwd: &std::path::Path,
        cancellation: &CancellationToken,
    ) -> RpcResult {
        let summary = self
            .repository
            .summary_status(cwd, cancellation)
            .await
            .map_err(|error| git_error("gitManager.listPullRequests", error))?;
        let Some(reference) = summary.ref_name else {
            return Ok(unavailable_pull_requests());
        };
        let Some(provider) = summary
            .source_control_provider
            .map(|provider| SourceControlProviderKind::from(provider.kind))
        else {
            return Ok(unavailable_pull_requests());
        };
        if matches!(
            provider,
            SourceControlProviderKind::Bitbucket | SourceControlProviderKind::Unknown
        ) {
            return Ok(unavailable_pull_requests());
        }

        let pull_request = self
            .pull_requests
            .resolve_current_optional(
                ResolvePullRequestInput {
                    cwd: cwd.to_path_buf(),
                    provider,
                    reference,
                },
                cancellation,
            )
            .await
            .map_err(|_| {
                operation_error(
                    "gitManager.listPullRequests",
                    "provider-command-failed",
                    "The source-control provider could not load pull requests.",
                )
            })?;
        let Some(pull_request) = pull_request else {
            return Ok(json!({
                "status": "available",
                "pullRequests": [],
                "checks": [],
            }));
        };
        let checks = self
            .pull_requests
            .read_checks(provider, cwd, pull_request.number, cancellation)
            .await
            .map_err(|_| {
                operation_error(
                    "gitManager.listPullRequests",
                    "provider-command-failed",
                    "The source-control provider could not load pull-request checks.",
                )
            })?;
        let (status, checks) = match checks {
            ProviderChecksResult::Available(checks) => ("available", checks),
            ProviderChecksResult::Unavailable => ("unavailable", Vec::new()),
        };
        Ok(json!({
            "status": status,
            "pullRequests": [pull_request],
            "checks": checks,
        }))
    }
}

fn unavailable_pull_requests() -> Value {
    json!({
        "status": "unavailable",
        "pullRequests": [],
        "checks": [],
    })
}

pub fn register_git_manager_rpc(
    registry: &mut RpcRegistry,
    services: impl Into<ConfiguredGitManagerRpcServices>,
) {
    let services = services.into();
    for method in GIT_MANAGER_UNARY_METHODS.iter().filter(|method| {
        !matches!(
            **method,
            "gitManager.getRefs"
                | "gitManager.getCommits"
                | "gitManager.getDiff"
                | "gitManager.getStashes"
                | "gitManager.getRemoteTags"
                | "gitManager.previewMerge"
                | "gitManager.listPullRequests"
                | "gitManager.commit"
                | "gitManager.undoCommit"
                | "gitManager.discard"
                | "gitManager.stagePartial"
                | "gitManager.unstagePartial"
                | "gitManager.discardPartial"
        )
    }) {
        let services = services.clone();
        registry.register_unary(*method, move |request, _cancellation| {
            let services = services.clone();
            async move { services.not_implemented_unary(request).await }
        });
    }
    for method in [
        "gitManager.getRefs",
        "gitManager.getCommits",
        "gitManager.getDiff",
        "gitManager.getStashes",
        "gitManager.getRemoteTags",
        "gitManager.previewMerge",
        "gitManager.listPullRequests",
    ] {
        let services = services.clone();
        registry.register_unary(method, move |request, cancellation| {
            let services = services.clone();
            async move { services.handle_read_unary(request, cancellation).await }
        });
    }
    for method in [
        "gitManager.commit",
        "gitManager.undoCommit",
        "gitManager.discard",
        "gitManager.stagePartial",
        "gitManager.unstagePartial",
        "gitManager.discardPartial",
    ] {
        let services = services.clone();
        registry.register_unary(method, move |request, cancellation| {
            let services = services.clone();
            async move { services.handle_mutation_unary(request, cancellation).await }
        });
    }

    let operation_services = services.clone();
    registry.register_stream("gitManager.runOperation", move |request, cancellation| {
        operation_services.operation_stream(request, cancellation)
    });
    registry.register_stream("subscribeGitManagerSignal", move |request, cancellation| {
        services.git_manager_signal_stream(request, cancellation)
    });
}

async fn send_operation_outputs(
    sender: &mpsc::Sender<RpcStreamChunk>,
    operation: &str,
    outputs: &[ProcessOutput],
) -> Result<(), ()> {
    for output in outputs {
        for (stream, text) in [("stdout", &output.stdout), ("stderr", &output.stderr)] {
            if text.is_empty() {
                continue;
            }
            sender
                .send(Ok(vec![json!({
                    "_tag": "output",
                    "operation": operation,
                    "stream": stream,
                    "text": text,
                })]))
                .await
                .map_err(|_| ())?;
        }
    }
    Ok(())
}

async fn send_terminal_operation_error(
    sender: &mpsc::Sender<RpcStreamChunk>,
    error: DomainOperationError,
) {
    if send_operation_outputs(sender, &error.operation, &error.outputs)
        .await
        .is_err()
    {
        return;
    }
    let _ = sender
        .send(Ok(vec![json!({
            "_tag": "failed",
            "operation": error.operation,
            "code": error.code,
            "message": error.message,
            "blocked": error.blocked,
        })]))
        .await;
}

#[derive(Deserialize)]
struct GitManagerCwdInput {
    cwd: PathBuf,
}

#[derive(Deserialize)]
struct GitManagerRemoteTagsInput {
    cwd: PathBuf,
    remote: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitManagerCommitInput {
    cwd: PathBuf,
    summary: String,
    description: String,
    amend: bool,
    no_verify: bool,
    signoff: bool,
    allow_empty: bool,
    co_authors: Vec<CoAuthor>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitManagerDiscardInput {
    cwd: PathBuf,
    paths: Vec<String>,
    permit_permanent: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitManagerPartialSelectionInput {
    cwd: PathBuf,
    project_id: String,
    path: String,
    selected_lines: Vec<usize>,
    base_generation: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitManagerGetCommitsInput {
    cwd: PathBuf,
    pinned_tips: Option<Vec<String>>,
    offset: Option<usize>,
    limit: Option<usize>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "_tag", rename_all = "kebab-case")]
enum GitManagerDiffSource {
    WorkingTree { path: String, staged: bool },
    Commit { sha: String, path: String },
    Stash { sha: String, path: String },
}

impl GitManagerDiffSource {
    fn path(&self) -> &str {
        match self {
            Self::WorkingTree { path, .. }
            | Self::Commit { path, .. }
            | Self::Stash { path, .. } => path,
        }
    }
}

#[derive(Deserialize)]
struct GitManagerGetDiffInput {
    cwd: PathBuf,
    source: GitManagerDiffSource,
}

#[derive(Deserialize)]
struct GitManagerPreviewMergeInput {
    cwd: PathBuf,
    source: String,
    #[serde(default)]
    target: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DiffSizeClass {
    Patch,
    LargeText,
    Unrenderable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PartialMutation {
    Stage,
    Unstage,
    Discard,
}

impl PartialMutation {
    const fn rpc_name(self) -> &'static str {
        match self {
            Self::Stage => "gitManager.stagePartial",
            Self::Unstage => "gitManager.unstagePartial",
            Self::Discard => "gitManager.discardPartial",
        }
    }

    const fn git_operation_name(self) -> &'static str {
        match self {
            Self::Stage => "GitManager.stagePartial",
            Self::Unstage => "GitManager.unstagePartial",
            Self::Discard => "GitManager.discardPartial",
        }
    }

    const fn guard_name(self) -> &'static str {
        match self {
            Self::Stage => "stage-partial",
            Self::Unstage => "unstage-partial",
            Self::Discard => "discard-partial",
        }
    }
}

fn validate_commit_input(input: &GitManagerCommitInput) -> Result<(), Value> {
    if input.summary.trim().is_empty() || input.summary.trim() != input.summary {
        return Err(operation_error(
            "gitManager.commit",
            "invalid-request",
            "The commit summary must be trimmed and non-empty.",
        ));
    }
    if input.co_authors.iter().any(|author| {
        author.name.trim().is_empty()
            || author.name.trim() != author.name
            || author.email.trim().is_empty()
            || author.email.trim() != author.email
    }) {
        return Err(operation_error(
            "gitManager.commit",
            "invalid-request",
            "Every co-author must have a trimmed non-empty name and email.",
        ));
    }
    Ok(())
}

pub(crate) async fn resolve_project_id(
    repositories: &Repositories,
    cwd: &std::path::Path,
) -> Result<String, &'static str> {
    let requested_key = canonical_worktree_path_key(cwd)
        .await
        .map_err(|_| "project-resolution-failed")?;
    let projects = repositories
        .list_projects()
        .await
        .map_err(|_| "project-resolution-failed")?;
    let mut matched = None;
    for project in projects
        .into_iter()
        .filter(|project| project.deleted_at.is_none())
    {
        let root_matches =
            canonical_worktree_path_key(std::path::Path::new(&project.workspace_root))
                .await
                .is_ok_and(|key| key == requested_key);
        let mut thread_matches = false;
        if !root_matches {
            for path in repositories
                .list_threads_by_project(project.project_id.clone())
                .await
                .map_err(|_| "project-resolution-failed")?
                .into_iter()
                .filter(|thread| thread.deleted_at.is_none())
                .filter_map(|thread| thread.worktree_path)
            {
                if canonical_worktree_path_key(std::path::Path::new(&path))
                    .await
                    .is_ok_and(|key| key == requested_key)
                {
                    thread_matches = true;
                    break;
                }
            }
        }
        if root_matches || thread_matches {
            if matched.is_some() {
                return Err("project-ambiguous");
            }
            matched = Some(project.project_id);
        }
    }
    matched.ok_or("project-not-found")
}

fn operation_blocked(
    snapshot: &GitManagerRefsSnapshot,
    operation: &str,
) -> Option<GitManagerBlockedReason> {
    evaluate_guards(&GuardInput::from_snapshot(snapshot, false))
        .into_values()
        .flatten()
        .find(|reason| reason.operation == operation)
}

fn operation_guard_name(operation: &str) -> &'static str {
    match operation {
        "gitManager.commit" => "commit",
        "gitManager.undoCommit" => "undo-commit",
        "gitManager.discard" => "discard",
        "gitManager.stagePartial" => "stage-partial",
        "gitManager.unstagePartial" => "unstage-partial",
        "gitManager.discardPartial" => "discard-partial",
        _ => "git-manager-mutation",
    }
}

fn blocked_error(operation: &str, reason: GitManagerBlockedReason) -> Value {
    json!({
        "_tag": "GitManagerOperationError",
        "operation": operation,
        "code": reason.code,
        "message": reason.message,
        "blocked": reason,
    })
}

fn mutation_git_error(operation: &str, error: GitCommandError) -> Value {
    let message = error.safe_failure_message();
    operation_error(
        operation,
        "git-command-failed",
        message
            .as_deref()
            .unwrap_or("Git could not complete the requested mutation."),
    )
}

fn discard_error(operation: &str, error: DiscardError) -> Value {
    match error {
        DiscardError::Git(error) => mutation_git_error(operation, error),
    }
}

fn partial_selection_error(operation: &str, error: PartialSelectionError) -> Value {
    match error {
        PartialSelectionError::Stale => operation_error(
            operation,
            "stale-selection",
            "The selected diff changed; refresh it and select the lines again.",
        ),
        PartialSelectionError::DiffTooLarge => operation_error(
            operation,
            "diff-too-large",
            "The selected diff is too large for safe partial staging.",
        ),
        PartialSelectionError::Git(_) => operation_error(
            operation,
            "git-command-failed",
            "Git could not apply the selected lines.",
        ),
    }
}

async fn await_server_owned_mutation(
    operation: impl std::future::Future<Output = RpcResult> + Send + 'static,
) -> RpcResult {
    match tokio::spawn(operation).await {
        Ok(result) => result,
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(_) => Err(operation_error(
            "gitManager.mutation",
            "operation-ended",
            "The server-owned Git Manager operation ended without a result.",
        )),
    }
}

async fn get_diff(
    repository: &GitRepository,
    input: GitManagerGetDiffInput,
    cancellation: &CancellationToken,
) -> RpcResult {
    let operation = "gitManager.getDiff";
    validate_pathspecs(
        "GitManager.getDiff",
        &input.cwd,
        &[input.source.path().to_owned()],
    )
    .map_err(|_| {
        operation_error(
            operation,
            "invalid-path",
            "The requested diff path is invalid.",
        )
    })?;
    if let GitManagerDiffSource::Commit { sha, path } = &input.source
        && is_image_path(path)
    {
        if !valid_object_id(sha) {
            return Err(operation_error(
                operation,
                "invalid-commit",
                "The requested commit identifier is invalid.",
            ));
        }
        let metadata = repository
            .git_manager_commit_diff(&input.cwd, sha, path, cancellation)
            .await
            .map_err(|error| git_error(operation, error))?;
        return render_commit_image_diff(
            input.source.clone(),
            &input.cwd,
            sha,
            path,
            metadata,
            cancellation,
        )
        .await;
    }
    let output = match &input.source {
        GitManagerDiffSource::WorkingTree { path, staged } => {
            let mut output = repository
                .git_manager_working_tree_diff(&input.cwd, path, *staged, cancellation)
                .await
                .map_err(|error| git_error(operation, error))?;
            if output.stdout.is_empty() && !*staged {
                let untracked = repository
                    .git_manager_untracked_paths(&input.cwd, path, cancellation)
                    .await
                    .map_err(|error| git_error(operation, error))?;
                if !untracked.stdout.is_empty() {
                    output = repository
                        .git_manager_untracked_diff(&input.cwd, path, cancellation)
                        .await
                        .map_err(|error| git_error(operation, error))?;
                    if !matches!(output.exit_code, 0 | 1) {
                        return Err(operation_error(
                            operation,
                            "git-command-failed",
                            "Git could not read the untracked file diff.",
                        ));
                    }
                }
            }
            output
        }
        GitManagerDiffSource::Commit { sha, path } => {
            if !valid_object_id(sha) {
                return Err(operation_error(
                    operation,
                    "invalid-commit",
                    "The requested commit identifier is invalid.",
                ));
            }
            repository
                .git_manager_commit_diff(&input.cwd, sha, path, cancellation)
                .await
                .map_err(|error| git_error(operation, error))?
        }
        GitManagerDiffSource::Stash { sha, path } => {
            if !valid_object_id(sha) {
                return Err(operation_error(
                    operation,
                    "invalid-stash",
                    "The requested stash identifier is invalid.",
                ));
            }
            stash::diff(repository, &input.cwd, sha, path, cancellation)
                .await
                .map_err(|error| stash_error(operation, error))?
        }
    };
    Ok(render_diff(input.source, output))
}

async fn render_commit_image_diff(
    source: GitManagerDiffSource,
    cwd: &std::path::Path,
    sha: &str,
    path: &str,
    metadata_output: ProcessOutput,
    cancellation: &CancellationToken,
) -> RpcResult {
    let before_commitish = format!("{sha}^");
    let (before, after) = tokio::join!(
        read_image_blob(cwd, &before_commitish, path, cancellation),
        read_image_blob(cwd, sha, path, cancellation),
    );
    let before = match before {
        Ok(value) => value,
        Err(GitManagerImageBlobError::TooLarge { .. }) => {
            return Ok(unrenderable_image_diff(source));
        }
        Err(GitManagerImageBlobError::Process(_)) => {
            return Err(operation_error(
                "gitManager.getDiff",
                "git-command-failed",
                "Git could not read the requested image blob.",
            ));
        }
    };
    let after = match after {
        Ok(value) => value,
        Err(GitManagerImageBlobError::TooLarge { .. }) => {
            return Ok(unrenderable_image_diff(source));
        }
        Err(GitManagerImageBlobError::Process(_)) => {
            return Err(operation_error(
                "gitManager.getDiff",
                "git-command-failed",
                "Git could not read the requested image blob.",
            ));
        }
    };
    let byte_length = before
        .as_ref()
        .map_or(0, Vec::len)
        .checked_add(after.as_ref().map_or(0, Vec::len))
        .unwrap_or(MAX_DIFF_BUFFER_SIZE + 1);
    if byte_length > MAX_DIFF_BUFFER_SIZE {
        return Ok(unrenderable_image_diff(source));
    }
    if before.is_none() && after.is_none() {
        return Err(operation_error(
            "gitManager.getDiff",
            "image-unavailable",
            "Neither side of the requested image diff exists.",
        ));
    }
    let generation = diff_generation(&parse_working_tree_diff(&metadata_output.stdout));
    let mime_type = mime_guess::from_path(path)
        .first_raw()
        .unwrap_or("application/octet-stream");
    Ok(json!({
        "_tag": "image",
        "generation": generation,
        "source": source,
        "byteLength": byte_length,
        "longestLineLength": 0,
        "before": image_diff_side(before.as_deref(), mime_type),
        "after": image_diff_side(after.as_deref(), mime_type),
    }))
}

fn image_diff_side(bytes: Option<&[u8]>, mime_type: &str) -> Value {
    bytes.map_or_else(
        || json!({ "contentBase64": null, "mimeType": null }),
        |bytes| {
            json!({
                "contentBase64": BASE64_STANDARD.encode(bytes),
                "mimeType": mime_type,
            })
        },
    )
}

fn unrenderable_image_diff(source: GitManagerDiffSource) -> Value {
    json!({
        "_tag": "unrenderable",
        "generation": 0,
        "source": source,
        "byteLength": MAX_DIFF_BUFFER_SIZE + 1,
        "longestLineLength": 0,
    })
}

fn is_image_path(path: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "ico" | "webp" | "bmp" | "avif"
            )
        })
}

fn render_diff(source: GitManagerDiffSource, output: ProcessOutput) -> Value {
    let truncated = output.stdout_truncated;
    let generation = diff_generation(&parse_working_tree_diff(&output.stdout));
    let byte_length = if truncated {
        MAX_DIFF_BUFFER_SIZE + 1
    } else {
        output.stdout.len()
    };
    let longest_line_length = if byte_length <= MAX_REASONABLE_DIFF_SIZE {
        output
            .stdout
            .lines()
            .map(|line| line.chars().count())
            .max()
            .unwrap_or(0)
    } else {
        0
    };
    let metadata = json!({
        "generation": generation,
        "source": source,
        "byteLength": byte_length,
        "longestLineLength": longest_line_length,
    });
    let mut value = match classify_diff_size(byte_length, longest_line_length, truncated) {
        DiffSizeClass::Patch => json!({
            "_tag": "patch",
            "patch": output.stdout,
        }),
        DiffSizeClass::LargeText => json!({ "_tag": "large-text" }),
        DiffSizeClass::Unrenderable => json!({ "_tag": "unrenderable" }),
    };
    if let (Some(value), Some(metadata)) = (value.as_object_mut(), metadata.as_object()) {
        value.extend(metadata.clone());
    }
    value
}

fn classify_diff_size(
    byte_length: usize,
    longest_line_length: usize,
    truncated: bool,
) -> DiffSizeClass {
    if truncated || byte_length > MAX_DIFF_BUFFER_SIZE {
        DiffSizeClass::Unrenderable
    } else if byte_length > MAX_REASONABLE_DIFF_SIZE
        || longest_line_length > MAX_DIFF_LINE_CHARACTERS
    {
        DiffSizeClass::LargeText
    } else {
        DiffSizeClass::Patch
    }
}

fn valid_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn decode<T: for<'de> Deserialize<'de>>(payload: Value, operation: &str) -> Result<T, Value> {
    serde_json::from_value(payload).map_err(|_| {
        operation_error(
            operation,
            "invalid-request",
            "The Git Manager request is invalid.",
        )
    })
}

fn encode_result<T: Serialize>(result: Result<T, Value>) -> RpcResult {
    result.and_then(|value| {
        serde_json::to_value(value).map_err(|_| {
            operation_error(
                "gitManager.read",
                "serialization-failed",
                "The Git Manager result could not be encoded.",
            )
        })
    })
}

fn refs_error(operation: &str, error: GitManagerRefsError) -> Value {
    match error {
        GitManagerRefsError::Git(error) => git_error(operation, error),
        GitManagerRefsError::MalformedRefs
        | GitManagerRefsError::Worktrees(_)
        | GitManagerRefsError::Tags(_) => operation_error(
            operation,
            "malformed-git-output",
            "Git returned malformed repository ref state.",
        ),
        GitManagerRefsError::RepositoryState(_) => operation_error(
            operation,
            "repository-state-unavailable",
            "Git repository operation state could not be inspected.",
        ),
    }
}

fn graph_error(operation: &str, error: GitManagerGraphError) -> Value {
    match error {
        GitManagerGraphError::TipsUnresolvable => operation_error(
            operation,
            "history-tips-unresolvable",
            "The pinned history snapshot is no longer available; refresh the history.",
        ),
        GitManagerGraphError::MalformedHistory => operation_error(
            operation,
            "malformed-git-output",
            "Git returned malformed commit history.",
        ),
        GitManagerGraphError::Git(error) => git_error(operation, error),
    }
}

fn tag_read_error(operation: &str, error: GitManagerTagError) -> Value {
    match error {
        GitManagerTagError::InvalidName => operation_error(
            operation,
            "invalid-remote-name",
            "The remote name is invalid.",
        ),
        GitManagerTagError::NotFound
        | GitManagerTagError::Malformed
        | GitManagerTagError::CommandFailed
        | GitManagerTagError::Git(_) => operation_error(
            operation,
            "git-command-failed",
            "Git could not list the remote's tags.",
        ),
    }
}

fn stash_error(operation: &str, error: GitManagerStashError) -> Value {
    match error {
        GitManagerStashError::NotFound => operation_error(
            operation,
            "stash-not-found",
            "The requested stash is no longer present; refresh the stash list.",
        ),
        GitManagerStashError::Malformed => operation_error(
            operation,
            "malformed-git-output",
            "Git returned malformed stash state.",
        ),
        GitManagerStashError::CommandFailed | GitManagerStashError::Git(_) => operation_error(
            operation,
            "git-command-failed",
            "Git could not complete the requested stash read.",
        ),
    }
}

fn merge_error(operation: &str, error: GitManagerMergeError) -> Value {
    match error {
        GitManagerMergeError::InvalidSource => operation_error(
            operation,
            "invalid-merge-source",
            "The requested merge source could not be resolved.",
        ),
        GitManagerMergeError::InvalidTarget => operation_error(
            operation,
            "local-branch-not-found",
            "The merge target branch no longer exists; refresh the repository refs.",
        ),
        GitManagerMergeError::GitTooOld { found } => operation_error(
            operation,
            "git-too-old",
            &format!("Merge preview needs Git 2.38 or later on this environment (found {found})."),
        ),
        GitManagerMergeError::MergeTreeFailed { detail } => operation_error(
            operation,
            "merge-tree-failed",
            &format!("Git could not compute the merge preview: {detail}"),
        ),
        GitManagerMergeError::CurrentUnavailable => operation_error(
            operation,
            "head-unavailable",
            "The current HEAD could not be resolved for merge preview.",
        ),
        GitManagerMergeError::MalformedComparison => operation_error(
            operation,
            "malformed-git-output",
            "Git returned malformed merge comparison state.",
        ),
        GitManagerMergeError::Git(_) => operation_error(
            operation,
            "git-command-failed",
            "Git could not complete the merge preview.",
        ),
    }
}

fn merge_preview_value(preview: merge::GitManagerMergePreviewResult) -> Value {
    let mut value = json!({
        "source": preview.source,
        "current": preview.current,
        "ahead": preview.ahead,
        "behind": preview.behind,
    });
    let fields = value
        .as_object_mut()
        .expect("merge preview metadata is an object");
    match preview.preview {
        GitManagerMergePreview::Clean => {
            fields.insert("_tag".to_owned(), json!("clean"));
        }
        GitManagerMergePreview::Conflicted { file_count } => {
            fields.insert("_tag".to_owned(), json!("conflicted"));
            fields.insert("fileCount".to_owned(), json!(file_count));
        }
        GitManagerMergePreview::UnrelatedHistories => {
            fields.insert("_tag".to_owned(), json!("unrelated-histories"));
        }
    }
    value
}

fn git_error(operation: &str, error: GitCommandError) -> Value {
    let message = error.safe_failure_message();
    operation_error(
        operation,
        "git-command-failed",
        message
            .as_deref()
            .unwrap_or("Git could not complete the requested read."),
    )
}

fn operation_error(operation: &str, code: &str, message: &str) -> Value {
    json!({
        "_tag": "GitManagerOperationError",
        "operation": operation,
        "code": code,
        "message": message,
        "blocked": null,
    })
}

fn not_implemented_error(operation: &str) -> Value {
    json!({
        "_tag": "GitManagerOperationError",
        "operation": operation,
        "code": "not-implemented",
        "message": "This Git Manager operation is not implemented yet.",
        "blocked": null,
    })
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, process::Command, sync::Arc, time::Duration};

    use tempfile::TempDir;
    use tokio::sync::{Semaphore, oneshot};

    use super::*;

    #[test]
    fn git_failure_messages_keep_known_exit_status_without_forwarding_private_detail() {
        let error = GitCommandError {
            tag: "GitCommandError",
            operation: "private-operation-sentinel".into(),
            command: "private-command-sentinel".into(),
            cwd: "private-cwd-sentinel".into(),
            diagnostics: Some(Box::new(crate::git::GitCommandDiagnostics {
                exit_code: Some(23),
                ..Default::default()
            })),
            detail: "stdout-sentinel Bearer credential-sentinel".into(),
        };
        for value in [
            git_error("gitManager.getRefs", error.clone()),
            mutation_git_error("gitManager.commit", error),
        ] {
            let message = value["message"].as_str().expect("message");
            assert!(message.contains("status 23"), "{message}");
            assert!(!message.contains("sentinel"));
            assert_eq!(value["code"], "git-command-failed");
        }
    }

    #[tokio::test]
    async fn git_launch_failure_survives_repository_and_manager_boundaries() {
        let sandbox = crate::test_support::TestSandbox::new("git-manager-launch-failure");
        let repository = GitRepository::default();
        let error = repository
            .run(
                "private-operation-sentinel",
                &sandbox.root().join("absent-workspace"),
                &["status".into()],
                &CancellationToken::new(),
            )
            .await
            .expect_err("absent owned cwd prevents the process from starting");
        for value in [
            git_error("gitManager.getRefs", error.clone()),
            mutation_git_error("gitManager.commit", error),
        ] {
            let message = value["message"].as_str().expect("message");
            assert!(message.contains("Could not start git"), "{message}");
            assert!(
                message.contains("required file or directory")
                    || message.contains("required path is not a directory"),
                "{message}"
            );
            assert!(message.contains("repository folder"));
            assert!(!message.contains("not installed"));
            assert!(!message.contains("sentinel"));
            assert!(!message.contains(sandbox.root().to_string_lossy().as_ref()));
            assert_eq!(value["code"], "git-command-failed");
        }
    }
    use crate::{
        persistence::{Database, ProjectionProject, Repositories, run_migrations},
        rpc::{ACTIVE_RPC_METHODS, MethodMode, RequestId},
        worktree_catalog::{WorkspaceAvailabilityRegistry, WorktreeCatalogService},
    };

    fn git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git test fixture starts");
        assert!(
            output.status.success(),
            "git fixture command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn repository_with_change() -> TempDir {
        let repository = TempDir::new().expect("temporary repository");
        git(repository.path(), &["init", "-q", "-b", "main"]);
        git(
            repository.path(),
            &["config", "user.name", "Git Manager Test"],
        );
        git(
            repository.path(),
            &["config", "user.email", "git-manager@example.test"],
        );
        fs::write(repository.path().join("tracked.txt"), "base\n").expect("base file");
        git(repository.path(), &["add", "tracked.txt"]);
        git(repository.path(), &["commit", "-q", "-m", "base"]);
        fs::write(repository.path().join("tracked.txt"), "changed\n").expect("changed file");
        repository
    }

    fn request(tag: &str, payload: Value) -> RpcRequest {
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

    fn registry_with_non_git_manager_methods() -> RpcRegistry {
        let mut registry = RpcRegistry::empty();
        for method in ACTIVE_RPC_METHODS
            .iter()
            .filter(|method| !method.name.starts_with("gitManager."))
            .filter(|method| method.name != "subscribeGitManagerSignal")
        {
            match method.mode {
                MethodMode::Unary => registry
                    .register_unary(method.name, |_request, _cancellation| async {
                        Ok(json!({}))
                    }),
                MethodMode::Stream => {
                    registry.register_stream(method.name, |_request, _cancellation| {
                        let (_sender, receiver) = mpsc::channel(1);
                        receiver
                    });
                }
            }
        }
        registry
    }

    #[test]
    fn registers_every_git_manager_method_needed_by_production_startup() {
        let mut registry = registry_with_non_git_manager_methods();
        register_git_manager_rpc(&mut registry, GitManagerRpcServices);
        registry
            .validate_complete()
            .expect("the production Git Manager registry is complete");
    }

    #[test]
    fn registry_validation_fails_when_git_manager_registration_is_omitted() {
        let registry = registry_with_non_git_manager_methods();
        let error = registry
            .validate_complete()
            .expect_err("Git Manager methods are required at startup");
        assert!(error.contains("gitManager.commit"));
        assert!(error.contains("subscribeGitManagerSignal"));
    }

    #[test]
    fn diff_size_ladder_uses_the_server_side_contract_boundaries() {
        assert_eq!(
            classify_diff_size(MAX_REASONABLE_DIFF_SIZE, 5_000, false),
            DiffSizeClass::Patch
        );
        assert_eq!(
            classify_diff_size(MAX_REASONABLE_DIFF_SIZE + 1, 0, false),
            DiffSizeClass::LargeText
        );
        assert_eq!(
            classify_diff_size(128, 5_001, false),
            DiffSizeClass::LargeText
        );
        assert_eq!(
            classify_diff_size(MAX_DIFF_BUFFER_SIZE + 1, 0, true),
            DiffSizeClass::Unrenderable
        );
    }

    #[test]
    fn all_phase_09_read_handlers_require_only_the_read_scope() {
        for method in [
            "gitManager.getRefs",
            "gitManager.getCommits",
            "gitManager.getDiff",
            "gitManager.getStashes",
            "gitManager.previewMerge",
        ] {
            assert_eq!(
                crate::auth::required_scope(method),
                Some("orchestration:read")
            );
        }
    }

    #[test]
    fn partial_staging_handlers_require_the_operate_scope() {
        for method in [
            "gitManager.stagePartial",
            "gitManager.unstagePartial",
            "gitManager.discardPartial",
        ] {
            assert_eq!(
                crate::auth::required_scope(method),
                Some("orchestration:operate")
            );
        }
    }

    #[tokio::test]
    async fn read_handler_returns_working_tree_and_commit_patches() {
        let repository = repository_with_change();
        let cwd = repository.path().to_string_lossy().into_owned();
        let working = GitManagerRpcServices
            .handle_read_unary(
                request(
                    "gitManager.getDiff",
                    json!({
                        "cwd": cwd,
                        "source": {
                            "_tag": "working-tree",
                            "path": "tracked.txt",
                            "staged": false
                        }
                    }),
                ),
                CancellationToken::new(),
            )
            .await
            .expect("working-tree diff");
        assert_eq!(working["_tag"], "patch");
        assert!(
            working["patch"]
                .as_str()
                .is_some_and(|patch| patch.contains("changed"))
        );

        let sha = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repository.path())
            .output()
            .expect("read commit sha");
        let sha = String::from_utf8(sha.stdout)
            .expect("UTF-8 sha")
            .trim()
            .to_owned();
        let commit = GitManagerRpcServices
            .handle_read_unary(
                request(
                    "gitManager.getDiff",
                    json!({
                        "cwd": cwd,
                        "source": { "_tag": "commit", "sha": sha, "path": "tracked.txt" }
                    }),
                ),
                CancellationToken::new(),
            )
            .await
            .expect("commit diff");
        assert_eq!(commit["_tag"], "patch");
        assert!(
            commit["patch"]
                .as_str()
                .is_some_and(|patch| patch.contains("base"))
        );
    }

    #[tokio::test]
    async fn stash_diff_resolves_the_stable_sha() {
        let repository = repository_with_change();
        git(repository.path(), &["stash", "push", "-q", "-m", "saved"]);
        let sha = Command::new("git")
            .args(["rev-parse", "refs/stash"])
            .current_dir(repository.path())
            .output()
            .expect("read stash sha");
        let sha = String::from_utf8(sha.stdout)
            .expect("UTF-8 stash sha")
            .trim()
            .to_owned();
        let diff = GitManagerRpcServices
            .handle_read_unary(
                request(
                    "gitManager.getDiff",
                    json!({
                        "cwd": repository.path(),
                        "source": {
                            "_tag": "stash",
                            "sha": sha,
                            "path": "tracked.txt"
                        }
                    }),
                ),
                CancellationToken::new(),
            )
            .await
            .expect("stash diff");
        assert_eq!(diff["_tag"], "patch");
        assert!(
            diff["patch"]
                .as_str()
                .is_some_and(|patch| patch.contains("changed"))
        );
    }

    #[tokio::test]
    async fn pull_request_read_without_a_supported_provider_is_structured_unavailable() {
        let repository = repository_with_change();

        let result = GitManagerRpcServices
            .handle_read_unary(
                request(
                    "gitManager.listPullRequests",
                    json!({ "cwd": repository.path() }),
                ),
                CancellationToken::new(),
            )
            .await
            .expect("unsupported provider is a result, not an RPC failure");

        assert_eq!(result["status"], "unavailable");
        assert_eq!(result["pullRequests"], json!([]));
        assert_eq!(result["checks"], json!([]));
    }

    fn primary_branch(
        snapshot: &crate::worktree_catalog::WorktreeCatalogSnapshot,
    ) -> Option<String> {
        snapshot
            .worktrees
            .iter()
            .find(|worktree| worktree.is_primary)
            .and_then(|worktree| worktree.branch.clone())
    }

    #[tokio::test]
    async fn branch_checkout_publishes_the_new_head_to_catalog_subscribers_without_polling() {
        let repository_fixture = TempDir::new().expect("temporary repository");
        let repo = repository_fixture.path();
        git(repo, &["init", "-q", "-b", "main"]);
        git(repo, &["config", "user.name", "Git Manager Test"]);
        git(repo, &["config", "user.email", "git-manager@example.test"]);
        fs::write(repo.join("tracked.txt"), "base\n").expect("base file");
        git(repo, &["add", "tracked.txt"]);
        git(repo, &["commit", "-q", "-m", "base"]);
        git(repo, &["branch", "feature"]);
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
                project_id: "project-checkout".to_owned(),
                title: "Checkout".to_owned(),
                workspace_root: repo.to_string_lossy().into_owned(),
                default_model_selection: None,
                scripts: json!([]),
                worktree_discovery: json!({}),
                worktree_repository_key: None,
                created_at: "2026-08-31T00:00:00Z".to_owned(),
                updated_at: "2026-08-31T00:00:00Z".to_owned(),
                deleted_at: None,
                repository_identity: None,
            })
            .await
            .expect("project projection");
        let repository = Arc::new(GitRepository::default());
        let broadcaster = StatusBroadcaster::new(repository.clone(), Duration::from_secs(3_600), 8);
        let availability = WorkspaceAvailabilityRegistry::new();
        // A one-hour poll interval proves that only an explicit invalidation can
        // deliver the checked-out branch within the assertion window.
        let catalog = WorktreeCatalogService::new_with_options_for_test(
            Arc::new(repositories.clone()),
            repository.clone(),
            availability.clone(),
            crate::worktree_catalog::CatalogServiceOptions {
                poll_interval: Duration::from_secs(3_600),
                ..crate::worktree_catalog::CatalogServiceOptions::default()
            },
        );
        let services = GitManagerRpcServices::with_dependencies(
            repository,
            broadcaster,
            catalog.clone(),
            repositories,
            availability,
            Arc::new(NativeFileTrash::default()),
        );
        let mut subscription = catalog
            .subscribe("project-checkout")
            .await
            .expect("catalog subscription");
        assert_eq!(
            primary_branch(&subscription.initial_latest()).as_deref(),
            Some("main")
        );

        let mut stream = services.operation_stream(
            request(
                "gitManager.runOperation",
                json!({
                    "_tag": "branch-checkout", "cwd": repo, "projectId": "project-checkout",
                    "name": "feature", "strategy": "bring"
                }),
            ),
            CancellationToken::new(),
        );
        let mut finished = false;
        while let Some(chunk) = tokio::time::timeout(Duration::from_secs(15), stream.recv())
            .await
            .expect("operation stream timeout")
        {
            for event in chunk.expect("operation chunk") {
                assert_ne!(event["_tag"], "failed", "checkout failed: {event}");
                finished |= event["_tag"] == "finished";
            }
        }
        assert!(finished, "the checkout operation finished");

        let updated = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let snapshot = subscription.changed().await.expect("catalog stays alive");
                if primary_branch(&snapshot).as_deref() == Some("feature") {
                    return snapshot;
                }
            }
        })
        .await
        .expect("the catalog publishes the checked-out branch without waiting for its poller");
        assert!(updated.authoritative);
    }

    #[tokio::test]
    async fn configured_commit_handler_commits_the_visible_index_without_a_socket() {
        let repository_fixture = repository_with_change();
        git(repository_fixture.path(), &["add", "tracked.txt"]);
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
                project_id: "project-commit".to_owned(),
                title: "Commit".to_owned(),
                workspace_root: repository_fixture.path().to_string_lossy().into_owned(),
                default_model_selection: None,
                scripts: json!([]),
                worktree_discovery: json!({}),
                worktree_repository_key: None,
                created_at: "2026-08-31T00:00:00Z".to_owned(),
                updated_at: "2026-08-31T00:00:00Z".to_owned(),
                deleted_at: None,
                repository_identity: None,
            })
            .await
            .expect("project projection");
        let repository = Arc::new(GitRepository::default());
        let broadcaster = StatusBroadcaster::new(repository.clone(), Duration::from_secs(3_600), 8);
        let availability = WorkspaceAvailabilityRegistry::new();
        let catalog = WorktreeCatalogService::new_with_availability_registry(
            Arc::new(repositories.clone()),
            repository.clone(),
            availability.clone(),
        );
        let services = GitManagerRpcServices::with_dependencies(
            repository,
            broadcaster,
            catalog.clone(),
            repositories,
            availability,
            Arc::new(NativeFileTrash::default()),
        );

        let result = services
            .handle_mutation_unary(
                request(
                    "gitManager.commit",
                    json!({
                        "cwd": repository_fixture.path(),
                        "summary": "Visible index commit",
                        "description": "through stdin",
                        "amend": false,
                        "noVerify": false,
                        "signoff": false,
                        "allowEmpty": false,
                        "coAuthors": []
                    }),
                ),
                CancellationToken::new(),
            )
            .await
            .expect("configured commit succeeds");

        assert_eq!(result["empty"], false);
        assert!(result["sha"].as_str().is_some_and(|sha| !sha.is_empty()));
        let message = Command::new("git")
            .args(["log", "-1", "--format=%B"])
            .current_dir(repository_fixture.path())
            .output()
            .expect("read committed message");
        assert_eq!(
            String::from_utf8(message.stdout).expect("UTF-8 commit message"),
            "Visible index commit\n\nthrough stdin\n\n"
        );

        let empty_discard = services
            .handle_mutation_unary(
                request(
                    "gitManager.discard",
                    json!({
                        "cwd": repository_fixture.path(),
                        "paths": [],
                        "permitPermanent": false
                    }),
                ),
                CancellationToken::new(),
            )
            .await
            .expect_err("discard paths are non-empty on the wire");
        assert_eq!(empty_discard["code"], "invalid-request");

        git(
            repository_fixture.path(),
            &["tag", "-a", "protected", "-m", "protected"],
        );
        let tagged_head = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repository_fixture.path())
            .output()
            .expect("read tagged head");
        let tagged_head = String::from_utf8(tagged_head.stdout)
            .expect("UTF-8 tagged head")
            .trim()
            .to_owned();
        let tagged = services
            .handle_mutation_unary(
                request(
                    "gitManager.undoCommit",
                    json!({ "cwd": repository_fixture.path() }),
                ),
                CancellationToken::new(),
            )
            .await
            .expect_err("annotated tags block undo");
        assert_eq!(tagged["code"], "tagged-commit");
        let current_head = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(repository_fixture.path())
            .output()
            .expect("read current head");
        assert_eq!(
            String::from_utf8(current_head.stdout)
                .expect("UTF-8 current head")
                .trim(),
            tagged_head
        );
        git(repository_fixture.path(), &["tag", "-d", "protected"]);

        let (entered, entered_rx) = oneshot::channel();
        let release = Arc::new(Semaphore::new(0));
        let held_catalog = catalog.clone();
        let held_release = release.clone();
        let held = tokio::spawn(async move {
            held_catalog
                .with_project_mutation_lock("project-commit", || async move {
                    let _ = entered.send(());
                    held_release
                        .acquire()
                        .await
                        .expect("lock release remains open")
                        .forget();
                })
                .await;
        });
        entered_rx.await.expect("catalog mutation lock acquired");
        let rejected = services
            .handle_mutation_unary(
                request(
                    "gitManager.commit",
                    json!({
                        "cwd": repository_fixture.path(),
                        "summary": "Concurrent commit",
                        "description": "",
                        "amend": false,
                        "noVerify": false,
                        "signoff": false,
                        "allowEmpty": false,
                        "coAuthors": []
                    }),
                ),
                CancellationToken::new(),
            )
            .await
            .expect_err("a held catalog lock rejects the second mutation");
        assert_eq!(rejected["code"], "operation-in-flight");
        assert_eq!(rejected["blocked"]["code"], "operation-in-flight");
        release.add_permits(1);
        held.await.expect("held catalog mutation joins");
    }
}
