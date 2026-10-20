//! Serialized Git Manager mutation operations.

use std::{
    collections::BTreeSet,
    ffi::OsString,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::git::{
    GitCommandError, GitManagerBlockedReason, GitManagerInProgressKind, GitManagerRefsSnapshot,
    GitRepository, NETWORK_TRANSFER_STALLED, OutputPolicy, ProcessOutput, ProcessRequest,
    ProcessRunner, StatusBroadcaster, reports_stalled_transfer, validate_pathspecs,
};
use crate::worktree_catalog::{ProjectMutationAttempt, WorktreeCatalogService};

use super::{
    conflicts::ConflictSide,
    guards::{GuardInput, evaluate_guards},
    in_progress::detect_in_progress_operation,
    merge,
    patch::{diff_generation, format_selection_patch, parse_working_tree_diff},
    refs::build_refs_snapshot,
    rewrite::{build_reorder_todo, build_squash_todo, resolve_last_retained_commit_ref},
    stash,
    tags::{self, GitManagerTagError},
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CoAuthor {
    pub name: String,
    pub email: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommitRequest {
    pub summary: String,
    pub description: Option<String>,
    pub amend: bool,
    pub no_verify: bool,
    pub signoff: bool,
    pub allow_empty: bool,
    pub co_authors: Vec<CoAuthor>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UndoCommitDraft {
    pub summary: String,
    pub description: String,
    pub co_authors: Vec<CoAuthor>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscardRequest {
    pub cwd: PathBuf,
    pub paths: Vec<String>,
    pub permit_permanent: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DiscardOutcome {
    pub trashed: Vec<String>,
    pub permanently_discarded: Vec<String>,
    pub trash_unavailable: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PartialSelectionRequest {
    pub cwd: PathBuf,
    pub path: String,
    pub selected_lines: Vec<usize>,
    pub base_generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PartialSelectionOutcome {
    pub generation: u64,
    pub patch_byte_length: usize,
    pub fallback_reason: Option<&'static str>,
}

#[derive(Debug, Error)]
pub enum PartialSelectionError {
    #[error(transparent)]
    Git(#[from] GitCommandError),
    #[error("the selected diff generation is stale")]
    Stale,
    #[error("the selected diff is too large to stage safely")]
    DiffTooLarge,
}

const RENAME_WHOLE_FILE_FALLBACK: &str =
    "Partial staging is unavailable for renamed paths; the whole file was staged.";

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("the operating-system trash is unavailable")]
pub struct TrashUnavailable;

pub type FileTrashFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(), TrashUnavailable>> + Send + 'a>>;

pub trait FileTrash: Send + Sync {
    fn trash<'a>(
        &'a self,
        path: PathBuf,
        cancellation: &'a CancellationToken,
    ) -> FileTrashFuture<'a>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NativeFileTrash {
    runner: ProcessRunner,
}

impl FileTrash for NativeFileTrash {
    fn trash<'a>(
        &'a self,
        path: PathBuf,
        cancellation: &'a CancellationToken,
    ) -> FileTrashFuture<'a> {
        Box::pin(async move {
            let request = native_trash_request(&path).await?;
            self.runner
                .run(request, cancellation)
                .await
                .map(|_| ())
                .map_err(|_| TrashUnavailable)
        })
    }
}

#[derive(Debug, Error)]
pub enum DiscardError {
    #[error(transparent)]
    Git(#[from] GitCommandError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitManagerFailureCode {
    Completed,
    AlreadyUpToDate,
    Authentication,
    NonFastForward,
    StaleInfo,
    LocalChangesOverwritten,
    Conflicts,
    ConflictsEncountered,
    OutstandingFilesNotStaged,
    NothingToStash,
    StashUntrackedCollision,
    NoUpstream,
    Cancelled,
    TimedOut,
    NetworkStalled,
    Unknown,
}

impl GitManagerFailureCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::AlreadyUpToDate => "already-up-to-date",
            Self::Authentication => "authentication",
            Self::NonFastForward => "non-fast-forward",
            Self::StaleInfo => "stale-info",
            Self::LocalChangesOverwritten => "local-changes-overwritten",
            Self::Conflicts => "conflicts",
            Self::ConflictsEncountered => "conflicts-encountered",
            Self::OutstandingFilesNotStaged => "outstanding-files-not-staged",
            Self::NothingToStash => "nothing-to-stash",
            Self::StashUntrackedCollision => "stash-untracked-collision",
            Self::NoUpstream => "no-upstream",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed-out",
            Self::NetworkStalled => "network-stalled",
            Self::Unknown => "unknown",
        }
    }
}

#[must_use]
pub fn classify_operation_failure(exit_code: i32, stderr: &str) -> GitManagerFailureCode {
    let output = stderr;
    let stderr = output.to_ascii_lowercase();
    if exit_code == 0 && stderr.contains("is up to date") {
        GitManagerFailureCode::AlreadyUpToDate
    } else if exit_code == 0 {
        GitManagerFailureCode::Completed
    } else if stderr.contains("was interrupted") || stderr.contains("cancelled") {
        GitManagerFailureCode::Cancelled
    } else if reports_stalled_transfer(output) {
        GitManagerFailureCode::NetworkStalled
    } else if stderr.contains("timed out") {
        GitManagerFailureCode::TimedOut
    } else if [
        "authentication failed",
        "could not read username",
        "could not read password",
        "permission denied (publickey)",
    ]
    .iter()
    .any(|needle| stderr.contains(needle))
    {
        GitManagerFailureCode::Authentication
    } else if stderr.contains("non-fast-forward") || stderr.contains("updates were rejected") {
        GitManagerFailureCode::NonFastForward
    } else if stderr.contains("stale info") {
        GitManagerFailureCode::StaleInfo
    } else if stderr.contains("your local changes to the following files would be overwritten") {
        GitManagerFailureCode::LocalChangesOverwritten
    } else if stderr.contains("needs merge") || stderr.contains("you must edit all merge conflicts")
    {
        GitManagerFailureCode::OutstandingFilesNotStaged
    } else if stderr.contains("could not restore untracked files from stash") {
        GitManagerFailureCode::StashUntrackedCollision
    } else if stderr.contains("could not apply") {
        GitManagerFailureCode::ConflictsEncountered
    } else if stderr.contains("conflict (") || stderr.contains("automatic merge failed") {
        GitManagerFailureCode::Conflicts
    } else if stderr.contains("there is no tracking information") {
        GitManagerFailureCode::NoUpstream
    } else {
        GitManagerFailureCode::Unknown
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum GitManagerCheckoutStrategy {
    Stash,
    Bring,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum GitManagerResetMode {
    Hard,
    Soft,
    Mixed,
}

impl GitManagerResetMode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Hard => "hard",
            Self::Soft => "soft",
            Self::Mixed => "mixed",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum GitManagerPendingOperation {
    Merge,
    Rebase,
    CherryPick,
    Revert,
}

impl GitManagerPendingOperation {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::Rebase => "rebase",
            Self::CherryPick => "cherry-pick",
            Self::Revert => "revert",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(
    tag = "_tag",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum GitManagerOperationRequest {
    BranchCreate {
        cwd: PathBuf,
        project_id: String,
        name: String,
        start_point: Option<String>,
        checkout: bool,
    },
    BranchCheckout {
        cwd: PathBuf,
        project_id: String,
        name: String,
        strategy: Option<GitManagerCheckoutStrategy>,
    },
    BranchRename {
        cwd: PathBuf,
        project_id: String,
        name: String,
        new_name: String,
    },
    BranchDelete {
        cwd: PathBuf,
        project_id: String,
        name: String,
        force: bool,
        delete_remote: bool,
    },
    Fetch {
        cwd: PathBuf,
        project_id: String,
        remote: String,
    },
    Pull {
        cwd: PathBuf,
        project_id: String,
        remote: String,
    },
    Push {
        cwd: PathBuf,
        project_id: String,
        remote: String,
        local_branch: String,
        remote_branch: Option<String>,
        #[serde(default)]
        push_tags: bool,
    },
    PublishBranch {
        cwd: PathBuf,
        project_id: String,
        remote: String,
        local_branch: String,
        remote_branch: Option<String>,
        #[serde(default)]
        push_tags: bool,
    },
    ForcePush {
        cwd: PathBuf,
        project_id: String,
        remote: String,
        local_branch: String,
        remote_branch: Option<String>,
        #[serde(default)]
        push_tags: bool,
    },
    StashPush {
        cwd: PathBuf,
        project_id: String,
        message: String,
        paths: Vec<String>,
        #[serde(default)]
        include_untracked: bool,
    },
    StashApply {
        cwd: PathBuf,
        project_id: String,
        index: u64,
    },
    StashPop {
        cwd: PathBuf,
        project_id: String,
        index: u64,
    },
    StashDrop {
        cwd: PathBuf,
        project_id: String,
        index: u64,
    },
    Merge {
        cwd: PathBuf,
        project_id: String,
        source: String,
        no_verify: bool,
    },
    SquashMerge {
        cwd: PathBuf,
        project_id: String,
        source: String,
        no_verify: bool,
    },
    MergeInto {
        cwd: PathBuf,
        project_id: String,
        source: String,
        target: String,
    },
    Rebase {
        cwd: PathBuf,
        project_id: String,
        base: String,
        target: String,
    },
    CherryPick {
        cwd: PathBuf,
        project_id: String,
        shas: Vec<String>,
    },
    Squash {
        cwd: PathBuf,
        project_id: String,
        shas: Vec<String>,
        message: String,
    },
    Reorder {
        cwd: PathBuf,
        project_id: String,
        shas: Vec<String>,
        insert_before_sha: Option<String>,
    },
    Revert {
        cwd: PathBuf,
        project_id: String,
        sha: String,
    },
    Reset {
        cwd: PathBuf,
        project_id: String,
        sha: String,
        mode: GitManagerResetMode,
    },
    Continue {
        cwd: PathBuf,
        project_id: String,
        operation: GitManagerPendingOperation,
    },
    Abort {
        cwd: PathBuf,
        project_id: String,
        operation: GitManagerPendingOperation,
    },
    ResolveConflict {
        cwd: PathBuf,
        project_id: String,
        path: String,
        side: ConflictSide,
    },
    TagCreate {
        cwd: PathBuf,
        project_id: String,
        name: String,
        sha: String,
        #[serde(default)]
        push_remote: Option<String>,
    },
    TagDelete {
        cwd: PathBuf,
        project_id: String,
        name: String,
    },
    TagPush {
        cwd: PathBuf,
        project_id: String,
        name: String,
        remote: String,
    },
}

impl GitManagerOperationRequest {
    #[must_use]
    pub const fn operation(&self) -> &'static str {
        match self {
            Self::BranchCreate { .. } => "branch-create",
            Self::BranchCheckout { .. } => "branch-checkout",
            Self::BranchRename { .. } => "branch-rename",
            Self::BranchDelete { .. } => "branch-delete",
            Self::Fetch { .. } => "fetch",
            Self::Pull { .. } => "pull",
            Self::Push { .. } => "push",
            Self::PublishBranch { .. } => "publish-branch",
            Self::ForcePush { .. } => "force-push",
            Self::StashPush { .. } => "stash-push",
            Self::StashApply { .. } => "stash-apply",
            Self::StashPop { .. } => "stash-pop",
            Self::StashDrop { .. } => "stash-drop",
            Self::Merge { .. } => "merge",
            Self::SquashMerge { .. } => "squash-merge",
            Self::MergeInto { .. } => "merge-into",
            Self::Rebase { .. } => "rebase",
            Self::CherryPick { .. } => "cherry-pick",
            Self::Squash { .. } => "squash",
            Self::Reorder { .. } => "reorder",
            Self::Revert { .. } => "revert",
            Self::Reset { .. } => "reset",
            Self::Continue { .. } => "continue",
            Self::Abort { .. } => "abort",
            Self::ResolveConflict { .. } => "resolve-conflict",
            Self::TagCreate { .. } => "tag-create",
            Self::TagDelete { .. } => "tag-delete",
            Self::TagPush { .. } => "tag-push",
        }
    }

    #[must_use]
    pub fn cwd(&self) -> &Path {
        match self {
            Self::BranchCreate { cwd, .. }
            | Self::BranchCheckout { cwd, .. }
            | Self::BranchRename { cwd, .. }
            | Self::BranchDelete { cwd, .. }
            | Self::Fetch { cwd, .. }
            | Self::Pull { cwd, .. }
            | Self::Push { cwd, .. }
            | Self::PublishBranch { cwd, .. }
            | Self::ForcePush { cwd, .. }
            | Self::StashPush { cwd, .. }
            | Self::StashApply { cwd, .. }
            | Self::StashPop { cwd, .. }
            | Self::StashDrop { cwd, .. }
            | Self::Merge { cwd, .. }
            | Self::SquashMerge { cwd, .. }
            | Self::MergeInto { cwd, .. }
            | Self::Rebase { cwd, .. }
            | Self::CherryPick { cwd, .. }
            | Self::Squash { cwd, .. }
            | Self::Reorder { cwd, .. }
            | Self::Revert { cwd, .. }
            | Self::Reset { cwd, .. }
            | Self::Continue { cwd, .. }
            | Self::Abort { cwd, .. }
            | Self::ResolveConflict { cwd, .. }
            | Self::TagCreate { cwd, .. }
            | Self::TagDelete { cwd, .. }
            | Self::TagPush { cwd, .. } => cwd,
        }
    }

    #[must_use]
    /// Operations that can move a worktree's HEAD or change the branch set.
    /// The worktree catalog must rescan after them instead of waiting for its
    /// poller; fetch, push, and stash operations leave every HEAD in place.
    fn changes_worktree_heads(&self) -> bool {
        !matches!(
            self,
            Self::Fetch { .. }
                | Self::Push { .. }
                | Self::PublishBranch { .. }
                | Self::ForcePush { .. }
                | Self::StashPush { .. }
                | Self::StashApply { .. }
                | Self::StashPop { .. }
                | Self::StashDrop { .. }
        )
    }

    pub fn project_id(&self) -> &str {
        match self {
            Self::BranchCreate { project_id, .. }
            | Self::BranchCheckout { project_id, .. }
            | Self::BranchRename { project_id, .. }
            | Self::BranchDelete { project_id, .. }
            | Self::Fetch { project_id, .. }
            | Self::Pull { project_id, .. }
            | Self::Push { project_id, .. }
            | Self::PublishBranch { project_id, .. }
            | Self::ForcePush { project_id, .. }
            | Self::StashPush { project_id, .. }
            | Self::StashApply { project_id, .. }
            | Self::StashPop { project_id, .. }
            | Self::StashDrop { project_id, .. }
            | Self::Merge { project_id, .. }
            | Self::SquashMerge { project_id, .. }
            | Self::MergeInto { project_id, .. }
            | Self::Rebase { project_id, .. }
            | Self::CherryPick { project_id, .. }
            | Self::Squash { project_id, .. }
            | Self::Reorder { project_id, .. }
            | Self::Revert { project_id, .. }
            | Self::Reset { project_id, .. }
            | Self::Continue { project_id, .. }
            | Self::Abort { project_id, .. }
            | Self::ResolveConflict { project_id, .. }
            | Self::TagCreate { project_id, .. }
            | Self::TagDelete { project_id, .. }
            | Self::TagPush { project_id, .. } => project_id,
        }
    }

    #[must_use]
    pub const fn is_implemented_through_phase_16(&self) -> bool {
        matches!(
            self,
            Self::BranchCreate { .. }
                | Self::BranchCheckout {
                    strategy: None | Some(GitManagerCheckoutStrategy::Bring),
                    ..
                }
                | Self::BranchRename { .. }
                | Self::BranchDelete { .. }
                | Self::Fetch { .. }
                | Self::Pull { .. }
                | Self::Push { .. }
                | Self::PublishBranch { .. }
                | Self::ForcePush { .. }
                | Self::StashPush { .. }
                | Self::StashApply { .. }
                | Self::StashPop { .. }
                | Self::StashDrop { .. }
                | Self::Merge { .. }
                | Self::SquashMerge { .. }
                | Self::MergeInto { .. }
                | Self::Rebase { .. }
                | Self::CherryPick { .. }
                | Self::Squash { .. }
                | Self::Reorder { .. }
                | Self::Revert { .. }
                | Self::Reset { .. }
                | Self::Continue { .. }
                | Self::Abort { .. }
                | Self::ResolveConflict { .. }
                | Self::TagCreate { .. }
                | Self::TagDelete { .. }
                | Self::TagPush { .. }
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitManagerOperationOutcome {
    pub operation: String,
    pub outputs: Vec<ProcessOutput>,
    pub message: String,
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("{message}")]
pub struct GitManagerOperationError {
    pub operation: String,
    pub code: String,
    pub message: String,
    pub blocked: Option<Box<GitManagerBlockedReason>>,
    pub outputs: Vec<ProcessOutput>,
}

pub async fn stage_partial(
    repository: &GitRepository,
    request: &PartialSelectionRequest,
    cancellation: &CancellationToken,
) -> Result<PartialSelectionOutcome, PartialSelectionError> {
    let fresh = read_partial_diff(repository, request, false, cancellation).await?;
    let mut parsed = parse_working_tree_diff(&fresh.patch);
    reject_stale(&parsed, request)?;

    if diff_records_rename(&fresh.patch) {
        repository
            .stage_files(
                &request.cwd,
                std::slice::from_ref(&request.path),
                cancellation,
            )
            .await?;
        tracing::debug!(
            operation = "gitManager.stagePartial",
            code = "rename-whole-file-fallback",
            selected_line_count = request.selected_lines.len(),
            "Git Manager partial staging used a whole-file fallback"
        );
        let generation = refreshed_generation(repository, request, false, cancellation).await?;
        return Ok(PartialSelectionOutcome {
            generation,
            patch_byte_length: 0,
            fallback_reason: Some(RENAME_WHOLE_FILE_FALLBACK),
        });
    }

    let Some(mut patch) = format_selection_patch(&parsed, &request.selected_lines) else {
        return Ok(noop_partial_outcome(diff_generation(&parsed)));
    };
    if fresh.untracked {
        repository
            .git_manager_intent_to_add(&request.cwd, &request.path, cancellation)
            .await?;
        let reread = repository
            .git_manager_working_tree_diff(&request.cwd, &request.path, false, cancellation)
            .await?;
        if reread.stdout_truncated {
            clear_intent(repository, request).await?;
            return Err(PartialSelectionError::DiffTooLarge);
        }
        parsed = parse_working_tree_diff(&reread.stdout);
        if diff_generation(&parsed) != request.base_generation {
            clear_intent(repository, request).await?;
            return Err(PartialSelectionError::Stale);
        }
        let Some(reread_patch) = format_selection_patch(&parsed, &request.selected_lines) else {
            clear_intent(repository, request).await?;
            return Ok(noop_partial_outcome(diff_generation(&parsed)));
        };
        patch = reread_patch;
    }

    let patch_byte_length = patch.len();
    log_partial_apply("gitManager.stagePartial", request, patch_byte_length);
    let result = repository
        .git_manager_apply_partial_patch(
            "GitManager.stagePartial.apply",
            &request.cwd,
            patch.into_bytes(),
            true,
            false,
            cancellation,
        )
        .await;
    if fresh.untracked && result.is_err() {
        let _ = clear_intent(repository, request).await;
    }
    result?;
    let generation = refreshed_generation(repository, request, false, cancellation).await?;
    Ok(PartialSelectionOutcome {
        generation,
        patch_byte_length,
        fallback_reason: None,
    })
}

pub async fn unstage_partial(
    repository: &GitRepository,
    request: &PartialSelectionRequest,
    cancellation: &CancellationToken,
) -> Result<PartialSelectionOutcome, PartialSelectionError> {
    apply_staged_reverse_patch(repository, request, cancellation).await
}

pub async fn discard_partial(
    repository: &GitRepository,
    request: &PartialSelectionRequest,
    cancellation: &CancellationToken,
) -> Result<PartialSelectionOutcome, PartialSelectionError> {
    let fresh = read_partial_diff(repository, request, false, cancellation).await?;
    let mut parsed = parse_working_tree_diff(&fresh.patch);
    reject_stale(&parsed, request)?;
    let Some(mut patch) = format_selection_patch(&parsed, &request.selected_lines) else {
        return Ok(noop_partial_outcome(diff_generation(&parsed)));
    };

    if fresh.untracked {
        repository
            .git_manager_intent_to_add(&request.cwd, &request.path, cancellation)
            .await?;
        let reread = repository
            .git_manager_working_tree_diff(&request.cwd, &request.path, false, cancellation)
            .await?;
        if reread.stdout_truncated {
            clear_intent(repository, request).await?;
            return Err(PartialSelectionError::DiffTooLarge);
        }
        parsed = parse_working_tree_diff(&reread.stdout);
        if diff_generation(&parsed) != request.base_generation {
            clear_intent(repository, request).await?;
            return Err(PartialSelectionError::Stale);
        }
        let Some(reread_patch) = format_selection_patch(&parsed, &request.selected_lines) else {
            clear_intent(repository, request).await?;
            return Ok(noop_partial_outcome(diff_generation(&parsed)));
        };
        patch = reread_patch;
    }

    let patch_byte_length = patch.len();
    log_partial_apply("gitManager.discardPartial", request, patch_byte_length);
    let result = repository
        .git_manager_apply_partial_patch(
            "GitManager.discardPartial.apply",
            &request.cwd,
            patch.into_bytes(),
            false,
            true,
            cancellation,
        )
        .await;
    if let Err(error) = result {
        if fresh.untracked {
            let _ = clear_intent(repository, request).await;
        }
        return Err(error.into());
    }
    if fresh.untracked {
        clear_intent(repository, request).await?;
    }
    let generation = refreshed_generation(repository, request, false, cancellation).await?;
    Ok(PartialSelectionOutcome {
        generation,
        patch_byte_length,
        fallback_reason: None,
    })
}

#[derive(Debug)]
struct FreshPartialDiff {
    patch: String,
    untracked: bool,
}

async fn apply_staged_reverse_patch(
    repository: &GitRepository,
    request: &PartialSelectionRequest,
    cancellation: &CancellationToken,
) -> Result<PartialSelectionOutcome, PartialSelectionError> {
    let fresh = read_partial_diff(repository, request, true, cancellation).await?;
    let parsed = parse_working_tree_diff(&fresh.patch);
    reject_stale(&parsed, request)?;
    let Some(patch) = format_selection_patch(&parsed, &request.selected_lines) else {
        return Ok(noop_partial_outcome(diff_generation(&parsed)));
    };
    let patch_byte_length = patch.len();
    log_partial_apply("gitManager.unstagePartial", request, patch_byte_length);
    repository
        .git_manager_apply_partial_patch(
            "GitManager.unstagePartial.apply",
            &request.cwd,
            patch.into_bytes(),
            true,
            true,
            cancellation,
        )
        .await?;
    let generation = refreshed_generation(repository, request, true, cancellation).await?;
    Ok(PartialSelectionOutcome {
        generation,
        patch_byte_length,
        fallback_reason: None,
    })
}

async fn read_partial_diff(
    repository: &GitRepository,
    request: &PartialSelectionRequest,
    staged: bool,
    cancellation: &CancellationToken,
) -> Result<FreshPartialDiff, PartialSelectionError> {
    let output = repository
        .git_manager_working_tree_diff(&request.cwd, &request.path, staged, cancellation)
        .await?;
    if output.stdout_truncated {
        return Err(PartialSelectionError::DiffTooLarge);
    }
    if staged || !output.stdout.is_empty() {
        return Ok(FreshPartialDiff {
            patch: output.stdout,
            untracked: false,
        });
    }

    let untracked = repository
        .git_manager_untracked_paths(&request.cwd, &request.path, cancellation)
        .await?;
    if untracked.stdout.is_empty() {
        return Ok(FreshPartialDiff {
            patch: output.stdout,
            untracked: false,
        });
    }
    let output = repository
        .git_manager_untracked_diff(&request.cwd, &request.path, cancellation)
        .await?;
    if output.stdout_truncated {
        return Err(PartialSelectionError::DiffTooLarge);
    }
    Ok(FreshPartialDiff {
        patch: output.stdout,
        untracked: true,
    })
}

fn reject_stale(
    parsed: &super::patch::ParsedFileDiff,
    request: &PartialSelectionRequest,
) -> Result<(), PartialSelectionError> {
    let generation = diff_generation(parsed);
    if generation == request.base_generation {
        return Ok(());
    }
    tracing::debug!(
        operation = "gitManager.partial",
        code = "stale-selection",
        selected_line_count = request.selected_lines.len(),
        "Git Manager rejected a stale partial selection"
    );
    Err(PartialSelectionError::Stale)
}

async fn refreshed_generation(
    repository: &GitRepository,
    request: &PartialSelectionRequest,
    staged: bool,
    cancellation: &CancellationToken,
) -> Result<u64, PartialSelectionError> {
    let fresh = read_partial_diff(repository, request, staged, cancellation).await?;
    Ok(diff_generation(&parse_working_tree_diff(&fresh.patch)))
}

async fn clear_intent(
    repository: &GitRepository,
    request: &PartialSelectionRequest,
) -> Result<(), PartialSelectionError> {
    repository
        .git_manager_clear_intent_to_add(&request.cwd, &request.path, &CancellationToken::new())
        .await?;
    Ok(())
}

fn noop_partial_outcome(generation: u64) -> PartialSelectionOutcome {
    PartialSelectionOutcome {
        generation,
        patch_byte_length: 0,
        fallback_reason: None,
    }
}

fn log_partial_apply(
    operation: &'static str,
    request: &PartialSelectionRequest,
    patch_byte_length: usize,
) {
    tracing::debug!(
        operation,
        code = "partial-patch-apply",
        selected_line_count = request.selected_lines.len(),
        patch_byte_length,
        "Git Manager is applying a partial selection"
    );
}

fn diff_records_rename(diff: &str) -> bool {
    diff.lines()
        .any(|line| line.starts_with("rename from ") || line.starts_with("rename to "))
        || diff
            .find("diff --git ")
            .map_or(diff, |patch_start| &diff[..patch_start])
            .split_ascii_whitespace()
            .any(|field| {
                field
                    .strip_prefix('R')
                    .is_some_and(|score| score.bytes().take_while(u8::is_ascii_digit).count() > 0)
            })
}

pub async fn run_branch_or_sync_operation(
    repository: Arc<GitRepository>,
    broadcaster: StatusBroadcaster,
    catalog: WorktreeCatalogService,
    request: GitManagerOperationRequest,
    cancellation: CancellationToken,
) -> Result<GitManagerOperationOutcome, GitManagerOperationError> {
    let operation = request.operation();
    if !request.is_implemented_through_phase_16() {
        return Err(operation_error(
            operation,
            "not-implemented",
            "This Git Manager operation is not implemented until a later phase.",
        ));
    }
    let project_id = request.project_id().to_owned();
    let locked_repository = Arc::clone(&repository);
    let locked_broadcaster = broadcaster.clone();
    let locked_request = request.clone();
    let operation_cancellation = cancellation.clone();
    let result = match catalog
        .try_with_project_mutation_lock_cancellation(&project_id, &cancellation, || async move {
            // vcs.* writes take this per-worktree guard but not the project lock, so it is
            // held from the snapshot onward: validation and execution see the same HEAD.
            let mutation = tokio::select! {
                biased;
                () = operation_cancellation.cancelled() => {
                    return Err(cancelled_error(operation));
                }
                mutation = locked_broadcaster.begin_mutation(locked_request.cwd()) => mutation,
            };
            let result = async {
                let mut snapshot = build_refs_snapshot(
                    &locked_repository,
                    locked_request.cwd(),
                    &operation_cancellation,
                )
                .await
                .map_err(|error| refs_snapshot_error(operation, error))?;
                snapshot.in_progress_operation = detect_in_progress_operation(
                    &locked_repository,
                    locked_request.cwd(),
                    &operation_cancellation,
                )
                .await
                .map_err(|_| {
                    operation_error(
                        operation,
                        "repository-state-unavailable",
                        "Git repository operation state could not be revalidated.",
                    )
                })?;
                if let Some(reason) = blocked_reason_for_operation(&snapshot, &locked_request) {
                    return Err(blocked_operation_error(operation, reason));
                }
                if operation_cancellation.is_cancelled() {
                    return Err(cancelled_error(operation));
                }
                execute_branch_or_sync_operation(
                    &locked_repository,
                    &snapshot,
                    &locked_request,
                    &operation_cancellation,
                )
                .await
            }
            .await;
            mutation.finish().await;
            result
        })
        .await
    {
        ProjectMutationAttempt::Acquired(result) => result,
        ProjectMutationAttempt::InFlight => Err(blocked_operation_error(
            operation,
            GitManagerBlockedReason {
                operation: operation.to_owned(),
                code: "operation-in-flight".to_owned(),
                message: "Blocked: another Git Manager operation is already running.".to_owned(),
            },
        )),
        ProjectMutationAttempt::Cancelled => Err(cancelled_error(operation)),
    };
    if result.is_ok() && request.changes_worktree_heads() {
        // Every project view sharing this repository shows worktree branches
        // and heads from the catalog; rescan now rather than on the next poll.
        catalog
            .invalidate_repository_after_mutation(&project_id)
            .await;
    }
    result
}

fn blocked_reason_for_operation(
    snapshot: &GitManagerRefsSnapshot,
    request: &GitManagerOperationRequest,
) -> Option<GitManagerBlockedReason> {
    let blocked = evaluate_guards(&GuardInput::from_snapshot(snapshot, false));
    let (branch, guard_operation) = match request {
        GitManagerOperationRequest::BranchCheckout { name, .. }
            if name.starts_with("refs/remotes/") =>
        {
            (None, "checkout")
        }
        GitManagerOperationRequest::BranchCheckout { name, .. } => (
            Some(name.strip_prefix("refs/heads/").unwrap_or(name)),
            "checkout",
        ),
        GitManagerOperationRequest::BranchRename { name, .. } => {
            (Some(name.as_str()), "rename-branch")
        }
        GitManagerOperationRequest::BranchDelete { name, .. } => {
            (Some(name.as_str()), "delete-branch")
        }
        GitManagerOperationRequest::Pull { .. } => (snapshot.head_ref.as_deref(), "pull"),
        GitManagerOperationRequest::Push { local_branch, .. } => {
            (Some(local_branch.as_str()), "push")
        }
        GitManagerOperationRequest::PublishBranch { local_branch, .. } => {
            (Some(local_branch.as_str()), "publish-branch")
        }
        GitManagerOperationRequest::ForcePush { local_branch, .. } => {
            (Some(local_branch.as_str()), "force-push")
        }
        GitManagerOperationRequest::Merge { source, .. }
        | GitManagerOperationRequest::SquashMerge { source, .. } => {
            (Some(source.as_str()), "merge")
        }
        GitManagerOperationRequest::MergeInto { target, .. } => {
            (Some(target.as_str()), "merge-into")
        }
        GitManagerOperationRequest::StashPush { .. } => {
            (snapshot.head_ref.as_deref(), "stash-push")
        }
        GitManagerOperationRequest::StashApply { .. } => {
            (snapshot.head_ref.as_deref(), "stash-apply")
        }
        GitManagerOperationRequest::StashPop { .. } => (snapshot.head_ref.as_deref(), "stash-pop"),
        GitManagerOperationRequest::StashDrop { .. } => {
            (snapshot.head_ref.as_deref(), "stash-drop")
        }
        GitManagerOperationRequest::Fetch { .. } => (snapshot.head_ref.as_deref(), "fetch"),
        GitManagerOperationRequest::BranchCreate { .. } => {
            (snapshot.head_ref.as_deref(), "branch-create")
        }
        GitManagerOperationRequest::Rebase { target, .. } => (Some(target.as_str()), "rebase"),
        GitManagerOperationRequest::CherryPick { .. } => {
            (snapshot.head_ref.as_deref(), "cherry-pick")
        }
        GitManagerOperationRequest::Squash { .. } => (snapshot.head_ref.as_deref(), "squash"),
        GitManagerOperationRequest::Reorder { .. } => (snapshot.head_ref.as_deref(), "reorder"),
        GitManagerOperationRequest::Revert { .. } => (snapshot.head_ref.as_deref(), "revert"),
        GitManagerOperationRequest::Reset { .. } => (snapshot.head_ref.as_deref(), "reset"),
        GitManagerOperationRequest::Continue { .. }
        | GitManagerOperationRequest::Abort { .. }
        | GitManagerOperationRequest::ResolveConflict { .. } => return None,
        _ => return None,
    };
    let branch_reasons = branch.and_then(|branch| blocked.get(branch));
    let bring_changes = matches!(
        request,
        GitManagerOperationRequest::BranchCheckout {
            strategy: Some(GitManagerCheckoutStrategy::Bring),
            ..
        }
    );
    let applies = |reason: &&GitManagerBlockedReason| {
        reason.operation == guard_operation
            && !(bring_changes && reason.code == "dirty-working-tree")
    };
    branch_reasons
        .and_then(|reasons| reasons.iter().find(applies))
        .or_else(|| {
            blocked.values().flatten().find(|reason| {
                applies(reason)
                    && matches!(
                        reason.code.as_str(),
                        "merge-in-progress" | "no-remote" | "dirty-working-tree"
                    )
            })
        })
        .cloned()
}

async fn execute_branch_or_sync_operation(
    repository: &GitRepository,
    snapshot: &GitManagerRefsSnapshot,
    request: &GitManagerOperationRequest,
    cancellation: &CancellationToken,
) -> Result<GitManagerOperationOutcome, GitManagerOperationError> {
    let operation = request.operation();
    let outputs = match request {
        GitManagerOperationRequest::BranchCreate {
            cwd,
            name,
            start_point,
            checkout,
            ..
        } => one_output(
            operation,
            repository
                .git_manager_create_branch(
                    cwd,
                    name,
                    start_point.as_deref(),
                    *checkout,
                    cancellation,
                )
                .await,
        )?,
        GitManagerOperationRequest::BranchCheckout { cwd, name, .. } => {
            let explicitly_remote = name.starts_with("refs/remotes/");
            let local_name = name.strip_prefix("refs/heads/").unwrap_or(name);
            if !explicitly_remote
                && snapshot
                    .local_branches
                    .iter()
                    .any(|reference| reference.name == local_name)
            {
                one_output(
                    operation,
                    repository
                        .git_manager_checkout_local_branch(cwd, local_name, cancellation)
                        .await,
                )?
            } else if name.starts_with("refs/heads/") {
                return Err(operation_error(
                    operation,
                    "local-branch-not-found",
                    "The selected local branch no longer exists. Refresh and choose a branch again.",
                ));
            } else if let Some(remote_ref) = remote_tracking_ref(snapshot, name) {
                let local_name = remote_ref
                    .name
                    .split_once('/')
                    .map_or(remote_ref.name.as_str(), |(_, branch)| branch);
                if snapshot
                    .local_branches
                    .iter()
                    .any(|reference| reference.name == local_name)
                {
                    return Err(operation_error(
                        operation,
                        "branch-exists",
                        &format!(
                            "A local branch named '{local_name}' already exists. Select it in the local branches list, or rename it before checking out '{}'.",
                            remote_ref.name
                        ),
                    ));
                }
                one_output(
                    operation,
                    repository
                        .git_manager_checkout_remote_branch(
                            cwd,
                            local_name,
                            &format!("refs/remotes/{}", remote_ref.name),
                            cancellation,
                        )
                        .await,
                )?
            } else if explicitly_remote {
                return Err(operation_error(
                    operation,
                    "remote-branch-not-found",
                    "The selected remote branch no longer exists. Fetch and choose a branch again.",
                ));
            } else {
                one_output(
                    operation,
                    repository
                        .git_manager_checkout_local_branch(cwd, name, cancellation)
                        .await,
                )?
            }
        }
        GitManagerOperationRequest::BranchRename {
            cwd,
            name,
            new_name,
            ..
        } => {
            let outputs = repository
                .git_manager_rename_branch(cwd, name, new_name, cancellation)
                .await
                .map_err(|error| git_command_error(operation, error, Vec::new()))?;
            require_last_success(operation, outputs)?
        }
        GitManagerOperationRequest::BranchDelete {
            cwd,
            name,
            force,
            delete_remote,
            ..
        } => {
            let remote_target = if *delete_remote {
                Some(remote_delete_target(snapshot, name).ok_or_else(|| {
                    operation_error(
                        operation,
                        "no-upstream",
                        "Remote deletion is blocked because this branch has no upstream.",
                    )
                })?)
            } else {
                None
            };
            let mut outputs = one_output(
                operation,
                repository
                    .git_manager_delete_branch(cwd, name, *force, cancellation)
                    .await,
            )?;
            if let Some((remote, remote_branch)) = remote_target {
                let remote_output = repository
                    .git_manager_delete_remote_branch(cwd, remote, remote_branch, cancellation)
                    .await
                    .map_err(|error| git_command_error(operation, error, outputs.clone()))?;
                outputs.push(remote_output);
                outputs = require_last_success(operation, outputs)?;
            }
            outputs
        }
        GitManagerOperationRequest::Fetch { cwd, remote, .. } => one_output(
            operation,
            repository
                .git_manager_fetch(cwd, remote, cancellation)
                .await,
        )?,
        GitManagerOperationRequest::Pull { cwd, remote, .. } => {
            let outputs = repository
                .git_manager_pull(cwd, remote, cancellation)
                .await
                .map_err(|error| git_command_error(operation, error, Vec::new()))?;
            require_last_success(operation, outputs)?
        }
        GitManagerOperationRequest::Push {
            cwd,
            remote,
            local_branch,
            remote_branch,
            push_tags,
            ..
        }
        | GitManagerOperationRequest::PublishBranch {
            cwd,
            remote,
            local_branch,
            remote_branch,
            push_tags,
            ..
        }
        | GitManagerOperationRequest::ForcePush {
            cwd,
            remote,
            local_branch,
            remote_branch,
            push_tags,
            ..
        } => one_output(
            operation,
            repository
                .git_manager_push(
                    cwd,
                    remote,
                    local_branch,
                    remote_branch.as_deref(),
                    matches!(request, GitManagerOperationRequest::PublishBranch { .. }),
                    matches!(request, GitManagerOperationRequest::ForcePush { .. }),
                    *push_tags,
                    cancellation,
                )
                .await,
        )?,
        GitManagerOperationRequest::StashPush {
            cwd,
            message,
            paths,
            include_untracked,
            ..
        } => {
            if message.is_empty() || message.trim() != message {
                return Err(operation_error(
                    operation,
                    "invalid-request",
                    "The stash message must be trimmed and non-empty.",
                ));
            }
            validate_pathspecs("GitManager.stashPush", cwd, paths).map_err(|_| {
                operation_error(
                    operation,
                    "invalid-path",
                    "A requested stash path is invalid.",
                )
            })?;
            let outputs = stash::push(
                repository,
                cwd,
                message,
                paths,
                *include_untracked,
                cancellation,
            )
            .await
            .map_err(|error| git_command_error(operation, error, Vec::new()))?;
            let outputs = require_last_success(operation, outputs)?;
            if *include_untracked {
                require_stash_entry(operation, outputs)?
            } else {
                outputs
            }
        }
        GitManagerOperationRequest::StashApply { cwd, index, .. } => one_output(
            operation,
            stash::apply(repository, cwd, *index, cancellation).await,
        )?,
        GitManagerOperationRequest::StashPop { cwd, index, .. } => one_output(
            operation,
            stash::pop(repository, cwd, *index, cancellation).await,
        )?,
        GitManagerOperationRequest::StashDrop { cwd, index, .. } => one_output(
            operation,
            stash::drop_stash(repository, cwd, *index, cancellation).await,
        )?,
        GitManagerOperationRequest::Merge {
            cwd,
            source,
            no_verify,
            ..
        } => {
            validate_merge_source(operation, source)?;
            let outputs = merge::merge(repository, cwd, source, *no_verify, cancellation)
                .await
                .map_err(|error| git_command_error(operation, error, Vec::new()))?;
            require_last_success(operation, outputs)?
        }
        GitManagerOperationRequest::SquashMerge {
            cwd,
            source,
            no_verify,
            ..
        } => {
            validate_merge_source(operation, source)?;
            let outputs = merge::squash_merge(repository, cwd, source, *no_verify, cancellation)
                .await
                .map_err(|error| git_command_error(operation, error, Vec::new()))?;
            require_last_success(operation, outputs)?
        }
        GitManagerOperationRequest::MergeInto {
            cwd,
            source,
            target,
            ..
        } => {
            validate_merge_source(operation, source)?;
            validate_revision(operation, target)?;
            if snapshot.head_ref.as_deref() == Some(target.as_str()) {
                return Err(operation_error(
                    operation,
                    "target-is-current",
                    "This branch is checked out here; use Merge into the current branch instead.",
                ));
            }
            if !snapshot
                .local_branches
                .iter()
                .any(|branch| branch.name == *target)
            {
                return Err(local_branch_not_found(operation));
            }
            let outcome = merge::merge_into(repository, cwd, source, target, cancellation)
                .await
                .map_err(|error| merge_into_error(operation, target, error))?;
            let label = source
                .strip_prefix("refs/heads/")
                .or_else(|| source.strip_prefix("refs/remotes/"))
                .unwrap_or(source);
            return Ok(GitManagerOperationOutcome {
                operation: operation.to_owned(),
                message: if outcome.up_to_date {
                    "Already up to date.".to_owned()
                } else {
                    format!("Merged {label} into {target}.")
                },
                outputs: outcome.outputs,
            });
        }
        GitManagerOperationRequest::Rebase {
            cwd, base, target, ..
        } => {
            validate_revision(operation, base)?;
            validate_revision(operation, target)?;
            one_output(
                operation,
                repository
                    .git_manager_rebase(cwd, base, target, cancellation)
                    .await,
            )?
        }
        GitManagerOperationRequest::CherryPick { cwd, shas, .. } => {
            validate_revisions(operation, shas, 1)?;
            one_output(
                operation,
                repository
                    .git_manager_cherry_pick(cwd, shas, cancellation)
                    .await,
            )?
        }
        GitManagerOperationRequest::Squash {
            cwd, shas, message, ..
        } => {
            validate_revisions(operation, shas, 2)?;
            if message.trim().is_empty() || message.trim() != message {
                return Err(operation_error(
                    operation,
                    "invalid-request",
                    "The squash message must be trimmed and non-empty.",
                ));
            }
            let log_order = repository
                .git_manager_rewrite_log_order(cwd, cancellation)
                .await
                .map_err(|error| git_command_error(operation, error, Vec::new()))?;
            let (rewrite_order, oldest_touched) = rewrite_range(&log_order, shas, None, false)
                .map_err(|message| operation_error(operation, "invalid-request", message))?;
            let squashed = shas
                .iter()
                .filter(|sha| *sha != &oldest_touched)
                .cloned()
                .collect::<BTreeSet<_>>();
            let todo = build_squash_todo(&rewrite_order, &squashed, &oldest_touched);
            let parent = repository
                .git_manager_commit_parent(cwd, &oldest_touched, cancellation)
                .await
                .map_err(|error| git_command_error(operation, error, Vec::new()))?;
            let retained = resolve_last_retained_commit_ref(&oldest_touched, parent.as_deref());
            one_output(
                operation,
                repository
                    .git_manager_interactive_rebase(
                        cwd,
                        &todo,
                        Some(message),
                        retained.as_deref(),
                        cancellation,
                    )
                    .await,
            )?
        }
        GitManagerOperationRequest::Reorder {
            cwd,
            shas,
            insert_before_sha,
            ..
        } => {
            validate_revisions(operation, shas, 1)?;
            if let Some(insert_before_sha) = insert_before_sha.as_deref() {
                validate_revision(operation, insert_before_sha)?;
                if shas.iter().any(|sha| sha == insert_before_sha) {
                    return Err(operation_error(
                        operation,
                        "invalid-request",
                        "The reorder insertion target cannot be one of the moved commits.",
                    ));
                }
            }
            let log_order = repository
                .git_manager_rewrite_log_order(cwd, cancellation)
                .await
                .map_err(|error| git_command_error(operation, error, Vec::new()))?;
            let (rewrite_order, oldest_touched) = rewrite_range(
                &log_order,
                shas,
                insert_before_sha.as_deref(),
                insert_before_sha.is_none(),
            )
            .map_err(|message| operation_error(operation, "invalid-request", message))?;
            let todo = build_reorder_todo(&rewrite_order, shas, insert_before_sha.as_deref());
            let parent = repository
                .git_manager_commit_parent(cwd, &oldest_touched, cancellation)
                .await
                .map_err(|error| git_command_error(operation, error, Vec::new()))?;
            let retained = resolve_last_retained_commit_ref(&oldest_touched, parent.as_deref());
            one_output(
                operation,
                repository
                    .git_manager_interactive_rebase(
                        cwd,
                        &todo,
                        None,
                        retained.as_deref(),
                        cancellation,
                    )
                    .await,
            )?
        }
        GitManagerOperationRequest::Revert { cwd, sha, .. } => {
            validate_revision(operation, sha)?;
            let merge_commit = repository
                .git_manager_is_merge_commit(cwd, sha, cancellation)
                .await
                .map_err(|error| git_command_error(operation, error, Vec::new()))?;
            one_output(
                operation,
                repository
                    .git_manager_revert(cwd, sha, merge_commit, cancellation)
                    .await,
            )?
        }
        GitManagerOperationRequest::Reset { cwd, sha, mode, .. } => {
            validate_revision(operation, sha)?;
            one_output(
                operation,
                repository
                    .git_manager_reset(cwd, sha, mode.as_str(), cancellation)
                    .await,
            )?
        }
        GitManagerOperationRequest::Continue {
            cwd,
            operation: pending,
            ..
        } => {
            validate_recovery_operation(snapshot, *pending, operation)?;
            let skip_empty = *pending == GitManagerPendingOperation::Rebase
                && repository
                    .git_manager_index_is_empty(cwd, cancellation)
                    .await
                    .map_err(|error| git_command_error(operation, error, Vec::new()))?;
            one_output(
                operation,
                repository
                    .git_manager_continue(cwd, pending.as_str(), skip_empty, cancellation)
                    .await,
            )?
        }
        GitManagerOperationRequest::Abort {
            cwd,
            operation: pending,
            ..
        } => {
            validate_recovery_operation(snapshot, *pending, operation)?;
            one_output(
                operation,
                repository
                    .git_manager_abort(cwd, pending.as_str(), cancellation)
                    .await,
            )?
        }
        GitManagerOperationRequest::ResolveConflict {
            cwd, path, side, ..
        } => {
            validate_pathspecs(
                "GitManager.resolveConflict",
                cwd,
                std::slice::from_ref(path),
            )
            .map_err(|_| {
                operation_error(
                    operation,
                    "invalid-path",
                    "The requested conflict path is invalid.",
                )
            })?;
            let selected_side_deleted = repository
                .git_manager_conflict_side_deleted(cwd, path, *side, cancellation)
                .await
                .map_err(|error| git_command_error(operation, error, Vec::new()))?
                .ok_or_else(|| {
                    operation_error(
                        operation,
                        "conflict-not-found",
                        "The requested path is no longer conflicted.",
                    )
                })?;
            let outputs = repository
                .git_manager_resolve_conflict(cwd, path, *side, selected_side_deleted, cancellation)
                .await
                .map_err(|error| git_command_error(operation, error, Vec::new()))?;
            require_last_success(operation, outputs)?
        }
        GitManagerOperationRequest::TagCreate {
            cwd,
            name,
            sha,
            push_remote,
            ..
        } => {
            let created = tags::create_tag(repository, cwd, name, sha, cancellation)
                .await
                .map_err(|error| tag_operation_error(operation, error))?;
            let mut outputs = require_last_success(operation, vec![created])?;
            if let Some(remote) = push_remote {
                // The tag exists locally from here on; a failed push must say so
                // instead of reading as "nothing happened".
                let failure =
                    match tags::push_tag(repository, cwd, remote, name, cancellation).await {
                        Ok(pushed) if pushed.exit_code == 0 => {
                            outputs.push(pushed);
                            None
                        }
                        Ok(pushed) => Some(
                            pushed
                                .stderr
                                .lines()
                                .map(str::trim)
                                .find(|line| !line.is_empty())
                                .unwrap_or("Git could not reach the remote.")
                                .to_owned(),
                        ),
                        Err(error) => Some(error.to_string()),
                    };
                if let Some(detail) = failure {
                    return Err(operation_error(
                        operation,
                        "tag-push-failed",
                        &format!(
                            "Created tag {name} locally, but pushing it to {remote} failed: {detail}"
                        ),
                    ));
                }
            }
            outputs
        }
        GitManagerOperationRequest::TagDelete { cwd, name, .. } => {
            let output = tags::delete_tag(repository, cwd, name, cancellation)
                .await
                .map_err(|error| tag_operation_error(operation, error))?;
            require_last_success(operation, vec![output])?
        }
        GitManagerOperationRequest::TagPush {
            cwd, name, remote, ..
        } => {
            let output = tags::push_tag(repository, cwd, remote, name, cancellation)
                .await
                .map_err(|error| tag_operation_error(operation, error))?;
            require_last_success(operation, vec![output])?
        }
    };
    Ok(GitManagerOperationOutcome {
        operation: operation.to_owned(),
        message: if outputs.iter().any(|output| {
            merge::is_already_up_to_date(output)
                || classify_operation_failure(output.exit_code, &output.stdout)
                    == GitManagerFailureCode::AlreadyUpToDate
                || classify_operation_failure(output.exit_code, &output.stderr)
                    == GitManagerFailureCode::AlreadyUpToDate
        }) {
            "Already up to date.".to_owned()
        } else {
            "Git operation completed.".to_owned()
        },
        outputs,
    })
}

fn validate_merge_source(operation: &str, source: &str) -> Result<(), GitManagerOperationError> {
    if source.is_empty() || source.trim() != source || source.starts_with('-') {
        return Err(operation_error(
            operation,
            "invalid-request",
            "The merge source must be a trimmed non-option revision.",
        ));
    }
    Ok(())
}

fn validate_revision(operation: &str, revision: &str) -> Result<(), GitManagerOperationError> {
    if revision.is_empty() || revision.trim() != revision || revision.starts_with('-') {
        return Err(operation_error(
            operation,
            "invalid-request",
            "A Git revision must be trimmed, non-empty, and cannot start with an option prefix.",
        ));
    }
    Ok(())
}

fn validate_revisions(
    operation: &str,
    revisions: &[String],
    minimum: usize,
) -> Result<(), GitManagerOperationError> {
    if revisions.len() < minimum {
        return Err(operation_error(
            operation,
            "invalid-request",
            "The operation does not include enough commits.",
        ));
    }
    let mut unique = BTreeSet::new();
    for revision in revisions {
        validate_revision(operation, revision)?;
        if !unique.insert(revision) {
            return Err(operation_error(
                operation,
                "invalid-request",
                "The operation includes the same commit more than once.",
            ));
        }
    }
    Ok(())
}

fn rewrite_range(
    log_order: &[String],
    moved_shas: &[String],
    insert_before_sha: Option<&str>,
    through_root: bool,
) -> Result<(Vec<String>, String), &'static str> {
    if log_order.is_empty() {
        return Err("The current branch has no commits to rewrite.");
    }
    let mut oldest_index = 0;
    for sha in moved_shas {
        let index = log_order
            .iter()
            .position(|candidate| candidate == sha)
            .ok_or("A selected commit is no longer on the current first-parent history.")?;
        oldest_index = oldest_index.max(index);
    }
    if let Some(insert_before_sha) = insert_before_sha {
        let index = log_order
            .iter()
            .position(|candidate| candidate == insert_before_sha)
            .ok_or("The reorder insertion target is no longer on the current history.")?;
        oldest_index = oldest_index.max(index);
    } else if through_root {
        oldest_index = log_order.len() - 1;
    }
    let rewrite_order = log_order[..=oldest_index].to_vec();
    let oldest_touched = rewrite_order
        .last()
        .expect("a non-empty prefix was selected")
        .clone();
    Ok((rewrite_order, oldest_touched))
}

fn validate_recovery_operation(
    snapshot: &GitManagerRefsSnapshot,
    requested: GitManagerPendingOperation,
    operation: &str,
) -> Result<(), GitManagerOperationError> {
    let detected = snapshot
        .in_progress_operation
        .as_ref()
        .and_then(|pending| match pending.kind {
            GitManagerInProgressKind::Merge => Some(GitManagerPendingOperation::Merge),
            GitManagerInProgressKind::Rebase => Some(GitManagerPendingOperation::Rebase),
            GitManagerInProgressKind::CherryPick => Some(GitManagerPendingOperation::CherryPick),
            GitManagerInProgressKind::Revert => Some(GitManagerPendingOperation::Revert),
            GitManagerInProgressKind::Squash => None,
        });
    if detected == Some(requested) {
        return Ok(());
    }
    Err(operation_error(
        operation,
        "operation-not-in-progress",
        "The requested recovery operation does not match the repository state.",
    ))
}

fn remote_tracking_ref<'a>(
    snapshot: &'a GitManagerRefsSnapshot,
    requested: &str,
) -> Option<&'a crate::git::GitManagerRefEntry> {
    if let Some(name) = requested.strip_prefix("refs/remotes/") {
        return snapshot
            .remote_branches
            .iter()
            .find(|reference| reference.name == name);
    }
    snapshot
        .remote_branches
        .iter()
        .find(|reference| reference.name == requested)
        .or_else(|| {
            let mut matches = snapshot.remote_branches.iter().filter(|reference| {
                reference
                    .name
                    .split_once('/')
                    .is_some_and(|(_, branch)| branch == requested)
            });
            let first = matches.next()?;
            matches.next().is_none().then_some(first)
        })
}

fn remote_delete_target<'a>(
    snapshot: &'a GitManagerRefsSnapshot,
    branch: &str,
) -> Option<(&'a str, &'a str)> {
    snapshot
        .local_branches
        .iter()
        .find(|reference| reference.name == branch)?
        .upstream
        .as_deref()?
        .split_once('/')
}

fn one_output(
    operation: &str,
    output: Result<ProcessOutput, GitCommandError>,
) -> Result<Vec<ProcessOutput>, GitManagerOperationError> {
    let output = output.map_err(|error| git_command_error(operation, error, Vec::new()))?;
    require_last_success(operation, vec![output])
}

fn require_last_success(
    operation: &str,
    outputs: Vec<ProcessOutput>,
) -> Result<Vec<ProcessOutput>, GitManagerOperationError> {
    let Some(output) = outputs.last() else {
        return Ok(outputs);
    };
    if output.exit_code == 0 {
        return Ok(outputs);
    }
    let mut code = classify_operation_failure(output.exit_code, &output.stderr);
    if code == GitManagerFailureCode::Unknown {
        code = classify_operation_failure(output.exit_code, &output.stdout);
    }
    Err(GitManagerOperationError {
        operation: operation.to_owned(),
        code: code.as_str().to_owned(),
        message: failure_message(code).to_owned(),
        blocked: None,
        outputs,
    })
}

/// `git stash push` exits 0 with "No local changes to save" when it creates no
/// entry. A caller that relies on the entry (Leave my changes, before a branch
/// switch) must stop there rather than continue as if the work were saved.
fn require_stash_entry(
    operation: &str,
    outputs: Vec<ProcessOutput>,
) -> Result<Vec<ProcessOutput>, GitManagerOperationError> {
    let saved_nothing = outputs.last().is_some_and(|output| {
        output.stdout.contains("No local changes to save")
            || output.stderr.contains("No local changes to save")
    });
    if !saved_nothing {
        return Ok(outputs);
    }
    let code = GitManagerFailureCode::NothingToStash;
    Err(GitManagerOperationError {
        operation: operation.to_owned(),
        code: code.as_str().to_owned(),
        message: failure_message(code).to_owned(),
        blocked: None,
        outputs,
    })
}

fn git_command_error(
    operation: &str,
    error: GitCommandError,
    outputs: Vec<ProcessOutput>,
) -> GitManagerOperationError {
    let exit_code = error
        .diagnostics
        .as_deref()
        .and_then(|diagnostics| diagnostics.exit_code)
        .unwrap_or(-1);
    let code = classify_operation_failure(exit_code, &error.detail);
    GitManagerOperationError {
        operation: operation.to_owned(),
        code: code.as_str().to_owned(),
        message: failure_message(code).to_owned(),
        blocked: None,
        outputs,
    }
}

fn tag_operation_error(operation: &str, error: GitManagerTagError) -> GitManagerOperationError {
    match error {
        GitManagerTagError::InvalidName => operation_error(
            operation,
            "invalid-tag-name",
            "The tag name or target is invalid.",
        ),
        GitManagerTagError::NotFound => operation_error(
            operation,
            "tag-not-found",
            "The requested tag no longer exists; refresh the repository refs.",
        ),
        GitManagerTagError::Malformed | GitManagerTagError::CommandFailed => operation_error(
            operation,
            "git-command-failed",
            "Git could not complete the requested tag operation.",
        ),
        GitManagerTagError::Git(error) => git_command_error(operation, error, Vec::new()),
    }
}

fn refs_snapshot_error(
    operation: &str,
    error: super::refs::GitManagerRefsError,
) -> GitManagerOperationError {
    match error {
        super::refs::GitManagerRefsError::Git(error) => {
            git_command_error(operation, error, Vec::new())
        }
        super::refs::GitManagerRefsError::MalformedRefs
        | super::refs::GitManagerRefsError::RepositoryState(_)
        | super::refs::GitManagerRefsError::Worktrees(_)
        | super::refs::GitManagerRefsError::Tags(_) => operation_error(
            operation,
            "repository-state-unavailable",
            "Git repository state could not be revalidated.",
        ),
    }
}

const fn failure_message(code: GitManagerFailureCode) -> &'static str {
    match code {
        GitManagerFailureCode::Completed => "Git operation completed.",
        GitManagerFailureCode::AlreadyUpToDate => "Already up to date.",
        GitManagerFailureCode::Authentication => {
            "Authentication failed. Check the configured credentials and try again."
        }
        GitManagerFailureCode::NonFastForward => {
            "The remote rejected this update because it is not a fast-forward."
        }
        GitManagerFailureCode::StaleInfo => {
            "The remote branch changed; fetch its latest state before retrying."
        }
        GitManagerFailureCode::LocalChangesOverwritten => {
            "Git stopped because local changes would be overwritten."
        }
        GitManagerFailureCode::Conflicts => "Git stopped because the operation produced conflicts.",
        GitManagerFailureCode::ConflictsEncountered => {
            "Git stopped because the history rewrite produced conflicts."
        }
        GitManagerFailureCode::OutstandingFilesNotStaged => {
            "Git cannot continue until every conflict is resolved and staged."
        }
        GitManagerFailureCode::NothingToStash => {
            "Git found no changes it can stash, so nothing was saved. Changes inside a submodule \
             can't be stashed; commit them in the submodule first."
        }
        GitManagerFailureCode::StashUntrackedCollision => {
            "The stash was kept: a new file in it already exists here, so Git did not restore \
             it. Move that file aside, then apply the stash again."
        }
        GitManagerFailureCode::NoUpstream => "The current branch has no configured upstream.",
        GitManagerFailureCode::Cancelled => "The Git Manager operation was cancelled.",
        GitManagerFailureCode::TimedOut => "The Git Manager operation timed out.",
        GitManagerFailureCode::NetworkStalled => NETWORK_TRANSFER_STALLED,
        GitManagerFailureCode::Unknown => "Git could not complete the requested operation.",
    }
}

fn blocked_operation_error(
    operation: &str,
    reason: GitManagerBlockedReason,
) -> GitManagerOperationError {
    GitManagerOperationError {
        operation: operation.to_owned(),
        code: reason.code.clone(),
        message: reason.message.clone(),
        blocked: Some(Box::new(reason)),
        outputs: Vec::new(),
    }
}

fn cancelled_error(operation: &str) -> GitManagerOperationError {
    operation_error(
        operation,
        GitManagerFailureCode::Cancelled.as_str(),
        failure_message(GitManagerFailureCode::Cancelled),
    )
}

fn local_branch_not_found(operation: &str) -> GitManagerOperationError {
    operation_error(
        operation,
        "local-branch-not-found",
        "The target branch no longer exists; refresh the repository refs.",
    )
}

/// Git's first stderr line, or the exit code when it printed none (`--quiet`).
fn failure_detail(output: &ProcessOutput) -> String {
    merge::first_stderr_line(&output.stderr).map_or_else(
        || format!("Git exited with {}.", output.exit_code),
        str::to_owned,
    )
}

fn merge_into_error(
    operation: &str,
    target: &str,
    error: merge::MergeIntoError,
) -> GitManagerOperationError {
    use merge::{GitManagerMergeError, MergeIntoError};
    match error {
        MergeIntoError::Merge(GitManagerMergeError::GitTooOld { found }) => operation_error(
            operation,
            "git-too-old",
            &format!(
                "Merging into another branch needs Git 2.38 or later on this environment (found {found})."
            ),
        ),
        MergeIntoError::Merge(GitManagerMergeError::Git(error)) => {
            git_command_error(operation, error, Vec::new())
        }
        MergeIntoError::Merge(GitManagerMergeError::InvalidSource) => operation_error(
            operation,
            "invalid-merge-source",
            "The requested merge source could not be resolved.",
        ),
        // The preview reports an invalid target the same way.
        MergeIntoError::Merge(GitManagerMergeError::InvalidTarget)
        | MergeIntoError::TargetMissing => local_branch_not_found(operation),
        MergeIntoError::Merge(GitManagerMergeError::MergeTreeFailed { detail }) => {
            operation_error(
                operation,
                "unknown",
                &format!("Git could not compute the merge: {detail}"),
            )
        }
        MergeIntoError::Merge(_) => {
            operation_error(operation, "unknown", "Git could not compute the merge.")
        }
        MergeIntoError::TargetIsSymbolic { destination } => operation_error(
            operation,
            "invalid-request",
            &format!(
                "{target} is an alias for {}. Merge into that branch instead.",
                destination.strip_prefix("refs/heads/").unwrap_or(&destination)
            ),
        ),
        MergeIntoError::Conflicts { file_count } => operation_error(
            operation,
            "conflicts",
            &format!(
                "{file_count} files would conflict. Check out {target} and merge there to resolve them."
            ),
        ),
        MergeIntoError::UnrelatedHistories => operation_error(
            operation,
            "unrelated-histories",
            "These branches have unrelated histories and cannot be merged.",
        ),
        MergeIntoError::TargetMoved => operation_error(
            operation,
            "non-fast-forward",
            &format!("{target} changed while merging. Review the preview and try again."),
        ),
        MergeIntoError::InUse { path } => blocked_operation_error(
            operation,
            GitManagerBlockedReason {
                operation: operation.to_owned(),
                code: "worktree-checked-out".to_owned(),
                message: path.map_or_else(
                    || {
                        "Cannot update this branch: Git reports it is in use by another worktree."
                            .to_owned()
                    },
                    |path| {
                        format!(
                            "Cannot update this branch: it is checked out in the worktree at {path}."
                        )
                    },
                ),
            },
        ),
        MergeIntoError::CommitFailed(output) => GitManagerOperationError {
            operation: operation.to_owned(),
            code: "unknown".to_owned(),
            message: format!(
                "Git could not create the merge commit: {}",
                failure_detail(&output)
            ),
            blocked: None,
            outputs: vec![output],
        },
        MergeIntoError::PublishFailed(output) => GitManagerOperationError {
            operation: operation.to_owned(),
            code: "unknown".to_owned(),
            message: format!(
                "Git could not update {target}: {}",
                failure_detail(&output)
            ),
            blocked: None,
            outputs: vec![output],
        },
    }
}

fn operation_error(operation: &str, code: &str, message: &str) -> GitManagerOperationError {
    GitManagerOperationError {
        operation: operation.to_owned(),
        code: code.to_owned(),
        message: message.to_owned(),
        blocked: None,
        outputs: Vec::new(),
    }
}

#[must_use]
pub fn commit_arguments(request: &CommitRequest) -> Vec<String> {
    let mut args = vec!["commit".to_owned()];
    if request.amend {
        args.push("--amend".to_owned());
    }
    if request.no_verify {
        args.push("--no-verify".to_owned());
    }
    if request.signoff {
        args.push("--signoff".to_owned());
    }
    if request.allow_empty {
        args.push("--allow-empty".to_owned());
    }
    args.extend(["-F".to_owned(), "-".to_owned()]);
    args
}

#[must_use]
pub fn commit_message_body(request: &CommitRequest) -> String {
    let mut message = request.summary.clone();
    if let Some(description) = request
        .description
        .as_deref()
        .filter(|description| !description.is_empty())
    {
        message.push_str("\n\n");
        message.push_str(description);
    }
    for (index, co_author) in request.co_authors.iter().enumerate() {
        if index == 0 {
            message.push_str("\n\n");
        } else {
            message.push('\n');
        }
        message.push_str("Co-Authored-By: ");
        message.push_str(&co_author.name);
        message.push_str(" <");
        message.push_str(&co_author.email);
        message.push('>');
    }
    message.push('\n');
    message
}

#[must_use]
pub fn parse_undo_commit_message(message: &str) -> UndoCommitDraft {
    let mut lines = message
        .lines()
        .map(|line| line.trim_end_matches('\r'))
        .collect::<Vec<_>>();
    while lines.last().is_some_and(|line| line.trim().is_empty()) {
        lines.pop();
    }
    let trailer_start = lines
        .iter()
        .rposition(|line| line.trim().is_empty())
        .map_or(lines.len(), |index| index + 1);
    let trailer_block = &lines[trailer_start..];
    let has_coauthor_trailers = !trailer_block.is_empty()
        && trailer_block.iter().all(|line| is_trailer_line(line))
        && trailer_block
            .iter()
            .any(|line| parse_coauthor_trailer(line).is_some());
    let mut retained = Vec::new();
    let mut co_authors = Vec::new();
    for (index, line) in lines.into_iter().enumerate() {
        if has_coauthor_trailers
            && index >= trailer_start
            && let Some(co_author) = parse_coauthor_trailer(line)
        {
            co_authors.push(co_author);
        } else {
            retained.push(line);
        }
    }

    let summary = retained.first().copied().unwrap_or_default().to_owned();
    let mut description = retained.get(1..).unwrap_or_default();
    while description
        .first()
        .is_some_and(|line| line.trim().is_empty())
    {
        description = &description[1..];
    }
    while description
        .last()
        .is_some_and(|line| line.trim().is_empty())
    {
        description = &description[..description.len() - 1];
    }
    UndoCommitDraft {
        summary,
        description: description.join("\n"),
        co_authors,
    }
}

fn is_trailer_line(line: &str) -> bool {
    line.split_once(':').is_some_and(|(key, value)| {
        !value.trim().is_empty()
            && !key.is_empty()
            && key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

fn parse_coauthor_trailer(line: &str) -> Option<CoAuthor> {
    let (key, value) = line.split_once(':')?;
    if !key.eq_ignore_ascii_case("Co-Authored-By") {
        return None;
    }
    let value = value.trim();
    let email_end = value.strip_suffix('>')?;
    let email_start = email_end.rfind('<')?;
    let name = email_end[..email_start].trim();
    let email = email_end[email_start + 1..].trim();
    (!name.is_empty() && !email.is_empty()).then(|| CoAuthor {
        name: name.to_owned(),
        email: email.to_owned(),
    })
}

pub async fn discard_paths(
    repository: &GitRepository,
    trash: Arc<dyn FileTrash>,
    request: DiscardRequest,
    cancellation: &CancellationToken,
) -> Result<DiscardOutcome, DiscardError> {
    validate_pathspecs("GitManager.discard", &request.cwd, &request.paths)?;
    let tracked = repository
        .git_manager_tracked_paths(&request.cwd, &request.paths, cancellation)
        .await?;
    let mut outcome = DiscardOutcome::default();
    let mut restore_only = Vec::new();
    for path in &request.paths {
        let absolute_path = request.cwd.join(path);
        let tracked_path = tracked
            .iter()
            .any(|candidate| path_matches(path, candidate));
        if tracked_path
            && matches!(
                tokio::fs::symlink_metadata(&absolute_path).await,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound
            )
        {
            restore_only.push(path.clone());
            continue;
        }
        match trash.trash(absolute_path, cancellation).await {
            Ok(()) => outcome.trashed.push(path.clone()),
            Err(_) if request.permit_permanent => {
                outcome.permanently_discarded.push(path.clone());
            }
            Err(_) => outcome.trash_unavailable.push(path.clone()),
        }
    }

    let mut discarded_paths = outcome.trashed.clone();
    discarded_paths.extend(outcome.permanently_discarded.iter().cloned());
    discarded_paths.extend(restore_only);
    if discarded_paths.is_empty() {
        return Ok(outcome);
    }
    let tracked_to_unstage = tracked
        .into_iter()
        .filter(|tracked_path| {
            discarded_paths
                .iter()
                .any(|path| path_matches(path, tracked_path))
        })
        .collect::<Vec<_>>();
    repository
        .unstage_files(&request.cwd, &tracked_to_unstage, cancellation)
        .await?;
    let tracked_to_restore = repository
        .git_manager_tracked_paths(&request.cwd, &discarded_paths, cancellation)
        .await?;
    repository
        .git_manager_restore_tracked_paths(&request.cwd, &tracked_to_restore, cancellation)
        .await?;
    if !outcome.permanently_discarded.is_empty() {
        repository
            .discard_files(&request.cwd, &outcome.permanently_discarded, cancellation)
            .await?;
    }
    Ok(outcome)
}

fn path_matches(requested: &str, candidate: &str) -> bool {
    candidate == requested
        || candidate
            .strip_prefix(requested)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

async fn native_trash_request(path: &Path) -> Result<ProcessRequest, TrashUnavailable> {
    let cwd = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or(TrashUnavailable)?
        .to_path_buf();
    let (command, args) = native_trash_command(path).await?;
    Ok(ProcessRequest {
        operation: "GitManager.discard.trash".to_owned(),
        command,
        args,
        cwd,
        env: Vec::new(),
        stdin: None,
        timeout: Duration::from_secs(30),
        max_output_bytes: 64 * 1024,
        output_policy: OutputPolicy::Truncate,
        append_truncation_marker: false,
        allow_non_zero_exit: false,
    })
}

#[cfg(target_os = "linux")]
async fn native_trash_command(path: &Path) -> Result<(PathBuf, Vec<OsString>), TrashUnavailable> {
    Ok((
        PathBuf::from("gio"),
        vec![OsString::from("trash"), OsString::from("--"), path.into()],
    ))
}

#[cfg(target_os = "macos")]
async fn native_trash_command(path: &Path) -> Result<(PathBuf, Vec<OsString>), TrashUnavailable> {
    Ok((
        PathBuf::from("/usr/bin/osascript"),
        vec![
            OsString::from("-e"),
            OsString::from("on run argv"),
            OsString::from("-e"),
            OsString::from("tell application \"Finder\" to delete POSIX file (item 1 of argv)"),
            OsString::from("-e"),
            OsString::from("end run"),
            OsString::from("--"),
            path.into(),
        ],
    ))
}

#[cfg(windows)]
async fn native_trash_command(path: &Path) -> Result<(PathBuf, Vec<OsString>), TrashUnavailable> {
    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|_| TrashUnavailable)?;
    let method = if metadata.is_dir() {
        "DeleteDirectory"
    } else {
        "DeleteFile"
    };
    let script = format!(
        "Add-Type -AssemblyName Microsoft.VisualBasic; \
         [Microsoft.VisualBasic.FileIO.FileSystem]::{method}($args[0], \
         [Microsoft.VisualBasic.FileIO.UIOption]::OnlyErrorDialogs, \
         [Microsoft.VisualBasic.FileIO.RecycleOption]::SendToRecycleBin)"
    );
    Ok((
        PathBuf::from("powershell.exe"),
        vec![
            OsString::from("-NoProfile"),
            OsString::from("-NonInteractive"),
            OsString::from("-Command"),
            OsString::from(script),
            path.into(),
        ],
    ))
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
async fn native_trash_command(_path: &Path) -> Result<(PathBuf, Vec<OsString>), TrashUnavailable> {
    Err(TrashUnavailable)
}

#[cfg(test)]
mod tests {
    use std::{
        path::{Path, PathBuf},
        process::Command,
        sync::{Arc, Mutex},
    };

    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::git::GitRepository;

    fn git_output(cwd: &Path, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Git Manager Test")
            .env("GIT_AUTHOR_EMAIL", "git-manager@example.test")
            .env("GIT_COMMITTER_NAME", "Git Manager Test")
            .env("GIT_COMMITTER_EMAIL", "git-manager@example.test")
            .output()
            .expect("git fixture starts")
    }

    fn git(cwd: &Path, args: &[&str]) -> String {
        let output = git_output(cwd, args);
        assert!(
            output.status.success(),
            "git fixture failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn operation_snapshot(cwd: &Path) -> GitManagerRefsSnapshot {
        GitManagerRefsSnapshot {
            generation: 1,
            head_ref: Some("main".into()),
            detached_sha: None,
            is_dirty: false,
            default_branch: Some("main".into()),
            remotes: Vec::new(),
            local_branches: vec![crate::git::GitManagerRefEntry {
                name: "main".into(),
                tip_sha: git(cwd, &["rev-parse", "HEAD"]),
                upstream: None,
                ahead: 0,
                behind: 0,
                current: true,
                is_default: true,
                worktree_path: Some(cwd.to_string_lossy().into_owned()),
                blocked: Vec::new(),
            }],
            remote_branches: Vec::new(),
            tags: Vec::new(),
            worktrees: Vec::new(),
            in_progress_operation: None,
            conflicted_paths: Vec::new(),
        }
    }

    struct RecordingTrash {
        destination: PathBuf,
        trashed: Mutex<Vec<PathBuf>>,
    }

    struct UnavailableTrash;

    impl FileTrash for UnavailableTrash {
        fn trash<'a>(
            &'a self,
            _path: PathBuf,
            _cancellation: &'a CancellationToken,
        ) -> FileTrashFuture<'a> {
            Box::pin(async { Err(TrashUnavailable) })
        }
    }

    impl FileTrash for RecordingTrash {
        fn trash<'a>(
            &'a self,
            path: PathBuf,
            _cancellation: &'a CancellationToken,
        ) -> FileTrashFuture<'a> {
            Box::pin(async move {
                let destination = self.destination.join(
                    path.file_name()
                        .expect("discard fixture path has a file name"),
                );
                tokio::fs::rename(&path, destination)
                    .await
                    .map_err(|_| TrashUnavailable)?;
                self.trashed
                    .lock()
                    .expect("recording trash mutex")
                    .push(path);
                Ok(())
            })
        }
    }

    fn assert_failure_code(stderr: &str, expected: GitManagerFailureCode) {
        assert_eq!(classify_operation_failure(1, stderr), expected);
    }

    #[test]
    fn classifies_authentication_failures() {
        for stderr in [
            "fatal: Authentication failed",
            "fatal: could not read Username for 'https://example.test'",
            "fatal: could not read Password for 'https://example.test'",
            "remote: Permission denied (publickey)",
        ] {
            assert_failure_code(stderr, GitManagerFailureCode::Authentication);
        }
    }

    #[test]
    fn classifies_non_fast_forward_failures() {
        for stderr in [
            "! [rejected] main -> main (non-fast-forward)",
            "rejected because the remote contains work; updates were rejected",
        ] {
            assert_failure_code(stderr, GitManagerFailureCode::NonFastForward);
        }
    }

    #[test]
    fn classifies_stale_info_failures() {
        assert_failure_code("rejected: stale info", GitManagerFailureCode::StaleInfo);
    }

    #[test]
    fn classifies_stalled_network_transfers_with_an_actionable_message() {
        for stderr in [
            "error: RPC failed; curl 28 Operation too slow. Less than 1000 bytes/sec transferred the last 60 seconds\nfatal: expected flush after ref listing",
            "fatal: unable to access 'https://example.test/repository.git/': Operation too slow. Less than 1000 bytes/sec transferred the last 60 seconds",
            crate::git::NETWORK_TRANSFER_STALLED,
        ] {
            assert_failure_code(stderr, GitManagerFailureCode::NetworkStalled);
        }
        let error = require_last_success(
            "fetch",
            vec![ProcessOutput {
                exit_code: 128,
                stdout: String::new(),
                stderr: "error: RPC failed; curl 28 Operation too slow. Less than 1000 bytes/sec transferred the last 60 seconds".to_owned(),
                stdout_truncated: false,
                stderr_truncated: false,
            }],
        )
        .expect_err("a stalled fetch fails");
        assert_eq!(error.code, "network-stalled");
        assert_eq!(error.message, crate::git::NETWORK_TRANSFER_STALLED);
    }

    #[test]
    fn classifies_local_changes_overwritten_failures() {
        assert_failure_code(
            "Your local changes to the following files would be overwritten by checkout",
            GitManagerFailureCode::LocalChangesOverwritten,
        );
    }

    #[test]
    fn classifies_conflict_failures() {
        for stderr in [
            "CONFLICT (content): Merge conflict in tracked.txt",
            "Automatic merge failed; fix conflicts and then commit the result.",
        ] {
            assert_failure_code(stderr, GitManagerFailureCode::Conflicts);
        }
    }

    #[test]
    fn classifies_rebase_outcomes_without_a_second_stderr_matcher() {
        assert_eq!(
            classify_operation_failure(0, "Current branch topic is up to date."),
            GitManagerFailureCode::AlreadyUpToDate
        );
        assert_eq!(
            classify_operation_failure(0, ""),
            GitManagerFailureCode::Completed
        );
        assert_eq!(
            classify_operation_failure(1, "error: could not apply abcdef1... topic"),
            GitManagerFailureCode::ConflictsEncountered
        );
        assert_eq!(
            classify_operation_failure(
                1,
                "file.txt: needs merge\nYou must edit all merge conflicts and then mark them as resolved using git add"
            ),
            GitManagerFailureCode::OutstandingFilesNotStaged
        );
    }

    #[test]
    fn conflicting_pop_stdout_is_reported_as_a_conflict() {
        let error = require_last_success(
            "stash-pop",
            vec![ProcessOutput {
                exit_code: 1,
                stdout: "CONFLICT (content): Merge conflict in tracked.txt\n".into(),
                stderr: String::new(),
                stdout_truncated: false,
                stderr_truncated: false,
            }],
        )
        .expect_err("conflicting pop fails");

        assert_eq!(error.code, "conflicts");
    }

    #[test]
    fn stash_outcomes_that_save_or_restore_nothing_are_reported() {
        let output = |exit_code: i32, stdout: &str, stderr: &str| ProcessOutput {
            exit_code,
            stdout: stdout.into(),
            stderr: stderr.into(),
            stdout_truncated: false,
            stderr_truncated: false,
        };
        let collision = require_last_success(
            "stash-pop",
            vec![output(
                1,
                "docs/notes.md already exists, no checkout\n",
                "error: could not restore untracked files from stash\n",
            )],
        )
        .expect_err("colliding untracked restore fails");
        assert_eq!(collision.code, "stash-untracked-collision");

        let nothing = require_stash_entry(
            "stash-push",
            vec![output(0, "No local changes to save\n", "")],
        )
        .expect_err("an empty stash push is not a saved stash");
        assert_eq!(nothing.code, "nothing-to-stash");
        assert_eq!(nothing.outputs.len(), 1);

        let saved = vec![output(
            0,
            "Saved working directory and index state On main: WIP\n",
            "",
        )];
        assert_eq!(
            require_stash_entry("stash-push", saved.clone()).expect("saved stash"),
            saved
        );
    }

    #[test]
    fn phase_09_operation_requests_decode_the_contract_fields() {
        let stash: GitManagerOperationRequest = serde_json::from_value(serde_json::json!({
            "_tag": "stash-push",
            "cwd": "/repo",
            "projectId": "project-1",
            "message": "save all work",
            "paths": ["untracked.txt"]
        }))
        .expect("stash request");
        assert!(matches!(
            stash,
            GitManagerOperationRequest::StashPush { message, paths, include_untracked, .. }
                if message == "save all work" && paths == ["untracked.txt"] && !include_untracked
        ));
        let leave_changes: GitManagerOperationRequest = serde_json::from_value(serde_json::json!({
            "_tag": "stash-push",
            "cwd": "/repo",
            "projectId": "project-1",
            "message": "WIP before switching to develop",
            "paths": [],
            "includeUntracked": true
        }))
        .expect("stash request with untracked files");
        assert!(matches!(
            leave_changes,
            GitManagerOperationRequest::StashPush { paths, include_untracked, .. }
                if paths.is_empty() && include_untracked
        ));

        let merge: GitManagerOperationRequest = serde_json::from_value(serde_json::json!({
            "_tag": "merge",
            "cwd": "/repo",
            "projectId": "project-1",
            "source": "feature",
            "noVerify": true
        }))
        .expect("merge request");
        assert!(matches!(
            merge,
            GitManagerOperationRequest::Merge { source, no_verify, .. }
                if source == "feature" && no_verify
        ));
    }

    #[test]
    fn phase_09_operations_are_admitted_by_the_executor() {
        for request in [
            GitManagerOperationRequest::StashApply {
                cwd: PathBuf::from("/repo"),
                project_id: "project-1".into(),
                index: 0,
            },
            GitManagerOperationRequest::Merge {
                cwd: PathBuf::from("/repo"),
                project_id: "project-1".into(),
                source: "feature".into(),
                no_verify: false,
            },
        ] {
            assert!(request.is_implemented_through_phase_16());
        }
    }

    #[test]
    fn phase_13_operation_requests_decode_all_contract_fields() {
        let rebase: GitManagerOperationRequest = serde_json::from_value(serde_json::json!({
            "_tag": "rebase",
            "cwd": "/repo",
            "projectId": "project-1",
            "base": "main",
            "target": "topic"
        }))
        .expect("rebase request");
        assert!(matches!(
            rebase,
            GitManagerOperationRequest::Rebase { base, target, .. }
                if base == "main" && target == "topic"
        ));

        let squash: GitManagerOperationRequest = serde_json::from_value(serde_json::json!({
            "_tag": "squash",
            "cwd": "/repo",
            "projectId": "project-1",
            "shas": ["bbbbbbb", "aaaaaaa"],
            "message": "combined"
        }))
        .expect("squash request");
        assert!(matches!(
            squash,
            GitManagerOperationRequest::Squash { shas, message, .. }
                if shas == ["bbbbbbb", "aaaaaaa"] && message == "combined"
        ));

        let recovery: GitManagerOperationRequest = serde_json::from_value(serde_json::json!({
            "_tag": "resolve-conflict",
            "cwd": "/repo",
            "projectId": "project-1",
            "path": "nested/file.txt",
            "side": "theirs"
        }))
        .expect("resolution request");
        assert!(matches!(
            recovery,
            GitManagerOperationRequest::ResolveConflict {
                path,
                side: super::ConflictSide::Theirs,
                ..
            } if path == "nested/file.txt"
        ));
    }

    #[test]
    fn phase_16_tag_requests_retain_every_contract_operand() {
        let create: GitManagerOperationRequest = serde_json::from_value(serde_json::json!({
            "_tag": "tag-create",
            "cwd": "/repo",
            "projectId": "project-1",
            "name": "release/v1",
            "sha": "0123456789abcdef0123456789abcdef01234567"
        }))
        .expect("tag-create request");
        let delete: GitManagerOperationRequest = serde_json::from_value(serde_json::json!({
            "_tag": "tag-delete",
            "cwd": "/repo",
            "projectId": "project-1",
            "name": "release/v1"
        }))
        .expect("tag-delete request");
        let push: GitManagerOperationRequest = serde_json::from_value(serde_json::json!({
            "_tag": "tag-push",
            "cwd": "/repo",
            "projectId": "project-1",
            "name": "release/v1",
            "remote": "origin"
        }))
        .expect("tag-push request");

        assert!(matches!(
            create,
            GitManagerOperationRequest::TagCreate { name, sha, .. }
                if name == "release/v1"
                    && sha == "0123456789abcdef0123456789abcdef01234567"
        ));
        assert!(matches!(
            delete,
            GitManagerOperationRequest::TagDelete { name, .. } if name == "release/v1"
        ));
        assert!(matches!(
            push,
            GitManagerOperationRequest::TagPush { name, remote, .. }
                if name == "release/v1" && remote == "origin"
        ));
    }

    #[tokio::test]
    async fn cherry_pick_operation_arm_applies_the_requested_commit() {
        let repository = tempfile::tempdir().expect("temporary repository");
        git(repository.path(), &["init", "-q", "-b", "main"]);
        git(repository.path(), &["config", "core.autocrlf", "false"]);
        git(
            repository.path(),
            &["config", "user.email", "git-manager@example.test"],
        );
        git(
            repository.path(),
            &["config", "user.name", "Git Manager Test"],
        );
        std::fs::write(repository.path().join("base.txt"), "base\n").expect("base file");
        git(repository.path(), &["add", "base.txt"]);
        git(repository.path(), &["commit", "-q", "-m", "base"]);
        git(repository.path(), &["checkout", "-q", "-b", "source"]);
        std::fs::write(repository.path().join("picked.txt"), "picked\n").expect("picked file");
        git(repository.path(), &["add", "picked.txt"]);
        git(repository.path(), &["commit", "-q", "-m", "picked"]);
        let picked_sha = git(repository.path(), &["rev-parse", "HEAD"]);
        git(repository.path(), &["checkout", "-q", "main"]);
        let snapshot = operation_snapshot(repository.path());
        let request = GitManagerOperationRequest::CherryPick {
            cwd: repository.path().to_path_buf(),
            project_id: "project-1".into(),
            shas: vec![picked_sha],
        };

        let outcome = execute_branch_or_sync_operation(
            &GitRepository::default(),
            &snapshot,
            &request,
            &CancellationToken::new(),
        )
        .await
        .expect("cherry-pick operation");

        assert_eq!(outcome.operation, "cherry-pick");
        assert_eq!(outcome.outputs.len(), 1);
        assert_eq!(
            std::fs::read_to_string(repository.path().join("picked.txt"))
                .expect("picked file exists"),
            "picked\n"
        );
    }

    #[test]
    fn classifies_no_upstream_failures() {
        assert_failure_code(
            "There is no tracking information for the current branch.",
            GitManagerFailureCode::NoUpstream,
        );
    }

    #[test]
    fn classifies_cancelled_failures() {
        assert_failure_code(
            "Git command was interrupted.",
            GitManagerFailureCode::Cancelled,
        );
    }

    #[test]
    fn classifies_timed_out_failures() {
        assert_failure_code("Git command timed out.", GitManagerFailureCode::TimedOut);
    }

    #[test]
    fn unknown_failures_use_the_fallback_code() {
        assert_failure_code(
            "fatal: an unfamiliar failure",
            GitManagerFailureCode::Unknown,
        );
    }

    #[test]
    fn delete_guard_revalidation_returns_the_prune_first_structured_reason() {
        let worktree_path = "/repo/missing-topic";
        let snapshot = crate::git::GitManagerRefsSnapshot {
            generation: 1,
            head_ref: Some("main".into()),
            detached_sha: None,
            is_dirty: false,
            default_branch: Some("main".into()),
            remotes: vec!["origin".into()],
            local_branches: vec![crate::git::GitManagerRefEntry {
                name: "topic".into(),
                tip_sha: "0123456789012345678901234567890123456789".into(),
                upstream: Some("origin/topic".into()),
                ahead: 0,
                behind: 0,
                current: false,
                is_default: false,
                worktree_path: Some(worktree_path.into()),
                blocked: Vec::new(),
            }],
            remote_branches: Vec::new(),
            tags: Vec::new(),
            worktrees: vec![crate::git::GitManagerWorktreeEntry {
                path: worktree_path.into(),
                head_sha: "0123456789012345678901234567890123456789".into(),
                branch: Some("topic".into()),
                is_primary: false,
                is_bare: false,
                is_detached: false,
                locked: false,
                lock_reason: None,
                prunable: true,
            }],
            in_progress_operation: None,
            conflicted_paths: Vec::new(),
        };
        let request = GitManagerOperationRequest::BranchDelete {
            cwd: PathBuf::from("/repo"),
            project_id: "project-1".into(),
            name: "topic".into(),
            force: true,
            delete_remote: false,
        };

        let reason = blocked_reason_for_operation(&snapshot, &request)
            .expect("missing registered worktree blocks branch deletion");

        assert_eq!(reason.operation, "delete-branch");
        assert_eq!(reason.code, "worktree-checked-out");
        assert!(
            reason
                .message
                .contains("remove or prune the worktree first")
        );
    }

    #[test]
    fn rebase_guard_revalidation_blocks_a_target_held_by_another_worktree() {
        let worktree_path = "/repo/topic-worktree";
        let snapshot = crate::git::GitManagerRefsSnapshot {
            generation: 1,
            head_ref: Some("main".into()),
            detached_sha: None,
            is_dirty: false,
            default_branch: Some("main".into()),
            remotes: Vec::new(),
            local_branches: vec![crate::git::GitManagerRefEntry {
                name: "topic".into(),
                tip_sha: "0123456789012345678901234567890123456789".into(),
                upstream: None,
                ahead: 0,
                behind: 0,
                current: false,
                is_default: false,
                worktree_path: Some(worktree_path.into()),
                blocked: Vec::new(),
            }],
            remote_branches: Vec::new(),
            tags: Vec::new(),
            worktrees: Vec::new(),
            in_progress_operation: None,
            conflicted_paths: Vec::new(),
        };
        let request = GitManagerOperationRequest::Rebase {
            cwd: PathBuf::from("/repo"),
            project_id: "project-1".into(),
            base: "main".into(),
            target: "topic".into(),
        };

        let reason = blocked_reason_for_operation(&snapshot, &request)
            .expect("occupied rebase target is blocked before Git");

        assert_eq!(reason.operation, "rebase");
        assert_eq!(reason.code, "worktree-checked-out");
        assert!(reason.message.contains(worktree_path));
    }

    fn merge_into_snapshot(
        worktree_path: Option<&str>,
        dirty: bool,
    ) -> crate::git::GitManagerRefsSnapshot {
        crate::git::GitManagerRefsSnapshot {
            generation: 1,
            head_ref: Some("main".into()),
            detached_sha: None,
            is_dirty: dirty,
            default_branch: Some("main".into()),
            remotes: Vec::new(),
            local_branches: vec![crate::git::GitManagerRefEntry {
                name: "release".into(),
                tip_sha: "0123456789012345678901234567890123456789".into(),
                upstream: None,
                ahead: 0,
                behind: 0,
                current: false,
                is_default: false,
                worktree_path: worktree_path.map(Into::into),
                blocked: Vec::new(),
            }],
            remote_branches: Vec::new(),
            tags: Vec::new(),
            worktrees: Vec::new(),
            in_progress_operation: None,
            conflicted_paths: Vec::new(),
        }
    }

    fn merge_into_request() -> GitManagerOperationRequest {
        GitManagerOperationRequest::MergeInto {
            cwd: PathBuf::from("/repo"),
            project_id: "project-1".into(),
            source: "refs/heads/feature".into(),
            target: "release".into(),
        }
    }

    #[test]
    fn merge_into_blocks_a_target_held_by_another_worktree() {
        let reason = blocked_reason_for_operation(
            &merge_into_snapshot(Some("/repo/release-worktree"), false),
            &merge_into_request(),
        )
        .expect("occupied merge-into target is blocked");

        assert_eq!(reason.operation, "merge-into");
        assert_eq!(reason.code, "worktree-checked-out");
        assert!(reason.message.contains("/repo/release-worktree"));
    }

    #[test]
    fn merge_into_ignores_a_dirty_selected_worktree() {
        assert_eq!(
            blocked_reason_for_operation(&merge_into_snapshot(None, true), &merge_into_request()),
            None
        );
    }

    #[test]
    fn merge_into_failures_map_to_the_wire_codes() {
        use merge::{GitManagerMergeError, MergeIntoError};

        let too_old = merge_into_error(
            "merge-into",
            "release",
            MergeIntoError::Merge(GitManagerMergeError::GitTooOld {
                found: crate::git::GitVersion {
                    major: 2,
                    minor: 34,
                    patch: 1,
                },
            }),
        );
        assert_eq!(too_old.code, "git-too-old");
        assert_eq!(
            too_old.message,
            "Merging into another branch needs Git 2.38 or later on this environment (found 2.34.1)."
        );

        let invalid_target = merge_into_error(
            "merge-into",
            "release~1",
            MergeIntoError::Merge(GitManagerMergeError::InvalidTarget),
        );
        assert_eq!(invalid_target.code, "local-branch-not-found");

        let in_use = merge_into_error(
            "merge-into",
            "release",
            MergeIntoError::InUse {
                path: Some("/w/release".into()),
            },
        );
        assert_eq!(in_use.code, "worktree-checked-out");
        assert!(in_use.blocked.is_some());
        assert!(in_use.message.contains("/w/release"));
    }

    #[test]
    fn merge_into_failure_messages_never_end_in_an_empty_detail() {
        use merge::MergeIntoError;

        let silent = ProcessOutput {
            exit_code: 1,
            stdout: String::new(),
            stderr: " \n".into(),
            stdout_truncated: false,
            stderr_truncated: false,
        };
        let publish = merge_into_error(
            "merge-into",
            "release",
            MergeIntoError::PublishFailed(silent.clone()),
        );
        assert_eq!(
            publish.message,
            "Git could not update release: Git exited with 1."
        );
        let commit = merge_into_error(
            "merge-into",
            "release",
            MergeIntoError::CommitFailed(silent),
        );
        assert_eq!(
            commit.message,
            "Git could not create the merge commit: Git exited with 1."
        );
    }

    #[test]
    fn builds_commit_arguments_from_the_request_options() {
        let request = CommitRequest {
            summary: "Fix the parser".into(),
            description: Some("Handles NUL records.".into()),
            amend: true,
            no_verify: true,
            signoff: true,
            allow_empty: false,
            co_authors: vec![CoAuthor {
                name: "Ann Author".into(),
                email: "ann@example.test".into(),
            }],
        };
        let args = commit_arguments(&request);
        assert_eq!(args[0], "commit");
        assert!(args.contains(&"--amend".to_owned()));
        assert!(args.contains(&"--no-verify".to_owned()));
        assert!(args.contains(&"--signoff".to_owned()));
        assert!(!args.contains(&"--allow-empty".to_owned()));
        assert!(args.contains(&"-F".to_owned()) && args.contains(&"-".to_owned()));
        assert_eq!(
            commit_message_body(&request),
            "Fix the parser\n\nHandles NUL records.\n\nCo-Authored-By: Ann Author <ann@example.test>\n"
        );
    }

    #[test]
    fn builds_several_coauthors_as_one_trailing_block() {
        let request = CommitRequest {
            summary: "Fix the parser".into(),
            description: Some("Handles trailer blocks.".into()),
            amend: false,
            no_verify: false,
            signoff: false,
            allow_empty: false,
            co_authors: vec![
                CoAuthor {
                    name: "Ann Author".into(),
                    email: "ann@example.test".into(),
                },
                CoAuthor {
                    name: "Bob Builder".into(),
                    email: "bob@example.test".into(),
                },
            ],
        };

        assert_eq!(
            commit_message_body(&request),
            "Fix the parser\n\nHandles trailer blocks.\n\n\
             Co-Authored-By: Ann Author <ann@example.test>\n\
             Co-Authored-By: Bob Builder <bob@example.test>\n"
        );
    }

    #[test]
    fn splits_a_message_without_trailers_unchanged() {
        assert_eq!(
            parse_undo_commit_message("Fix the parser\n\nHandles NUL records.\n"),
            UndoCommitDraft {
                summary: "Fix the parser".into(),
                description: "Handles NUL records.".into(),
                co_authors: Vec::new(),
            }
        );
    }

    #[test]
    fn splits_one_trailing_coauthor_from_the_description() {
        assert_eq!(
            parse_undo_commit_message(
                "Fix the parser\n\nHandles NUL records.\n\n\
                 Co-Authored-By: Ann Author <ann@example.test>\n",
            ),
            UndoCommitDraft {
                summary: "Fix the parser".into(),
                description: "Handles NUL records.".into(),
                co_authors: vec![CoAuthor {
                    name: "Ann Author".into(),
                    email: "ann@example.test".into(),
                }],
            }
        );
    }

    #[test]
    fn splits_several_coauthors_from_one_trailing_block() {
        assert_eq!(
            parse_undo_commit_message(
                "Fix the parser\n\nHandles NUL records.\n\n\
                 Co-Authored-By: Ann Author <ann@example.test>\n\
                 Co-Authored-By: Bob Builder <bob@example.test>\n",
            ),
            UndoCommitDraft {
                summary: "Fix the parser".into(),
                description: "Handles NUL records.".into(),
                co_authors: vec![
                    CoAuthor {
                        name: "Ann Author".into(),
                        email: "ann@example.test".into(),
                    },
                    CoAuthor {
                        name: "Bob Builder".into(),
                        email: "bob@example.test".into(),
                    },
                ],
            }
        );
    }

    #[test]
    fn preserves_a_coauthor_shaped_line_in_the_middle_of_the_body() {
        assert_eq!(
            parse_undo_commit_message(
                "Fix the parser\n\nBefore the example.\n\
                 Co-Authored-By: Documentation Example <docs@example.test>\n\
                 After the example.\n",
            ),
            UndoCommitDraft {
                summary: "Fix the parser".into(),
                description: "Before the example.\n\
                              Co-Authored-By: Documentation Example <docs@example.test>\n\
                              After the example."
                    .into(),
                co_authors: Vec::new(),
            }
        );
    }

    #[test]
    fn ignores_trailing_whitespace_around_the_trailer_block() {
        assert_eq!(
            parse_undo_commit_message(
                "Fix the parser\r\n\r\nHandles NUL records.\r\n \t\r\n\
                 Co-Authored-By: Ann Author <ann@example.test>   \r\n \t\r\n",
            ),
            UndoCommitDraft {
                summary: "Fix the parser".into(),
                description: "Handles NUL records.".into(),
                co_authors: vec![CoAuthor {
                    name: "Ann Author".into(),
                    email: "ann@example.test".into(),
                }],
            }
        );
    }

    #[test]
    fn restores_the_commit_draft_without_coauthor_trailers() {
        let draft = parse_undo_commit_message(
            "Fix the parser\n\nHandles NUL records.\n\
             Co-Authored-By: Documentation Example <docs@example.test>\n\
             explains the trailer syntax above.\n\n\
             Co-Authored-By: Ann Author <ann@example.test>\n\
             Signed-off-by: Dev User <dev@example.test>\n\
             Co-Authored-By: Bob Builder <bob@example.test>\n",
        );

        assert_eq!(draft.summary, "Fix the parser");
        assert_eq!(
            draft.description,
            "Handles NUL records.\nCo-Authored-By: Documentation Example <docs@example.test>\n\
             explains the trailer syntax above.\n\nSigned-off-by: Dev User <dev@example.test>"
        );
        assert_eq!(
            draft.co_authors,
            [
                CoAuthor {
                    name: "Ann Author".into(),
                    email: "ann@example.test".into(),
                },
                CoAuthor {
                    name: "Bob Builder".into(),
                    email: "bob@example.test".into(),
                },
            ]
        );
    }

    #[tokio::test]
    async fn discard_trashes_files_then_restores_tracked_content() {
        fn git(cwd: &Path, args: &[&str]) {
            let output = Command::new("git")
                .args(args)
                .current_dir(cwd)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_AUTHOR_NAME", "Git Manager Test")
                .env("GIT_AUTHOR_EMAIL", "git-manager@example.test")
                .env("GIT_COMMITTER_NAME", "Git Manager Test")
                .env("GIT_COMMITTER_EMAIL", "git-manager@example.test")
                .output()
                .expect("git fixture starts");
            assert!(
                output.status.success(),
                "git fixture failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let fixture = tempfile::tempdir().expect("temporary repository");
        let trash_directory = tempfile::tempdir().expect("temporary trash");
        git(fixture.path(), &["init", "-q", "-b", "main"]);
        git(fixture.path(), &["config", "core.autocrlf", "false"]);
        std::fs::write(fixture.path().join("tracked.txt"), "base\n").expect("base file");
        git(fixture.path(), &["add", "tracked.txt"]);
        git(fixture.path(), &["commit", "-q", "-m", "base"]);
        std::fs::write(fixture.path().join("tracked.txt"), "changed\n")
            .expect("changed tracked file");
        std::fs::write(fixture.path().join("untracked.txt"), "untracked\n")
            .expect("untracked file");
        let trash = Arc::new(RecordingTrash {
            destination: trash_directory.path().to_path_buf(),
            trashed: Mutex::new(Vec::new()),
        });

        let result = discard_paths(
            &GitRepository::default(),
            trash,
            DiscardRequest {
                cwd: fixture.path().to_path_buf(),
                paths: vec!["tracked.txt".into(), "untracked.txt".into()],
                permit_permanent: false,
            },
            &CancellationToken::new(),
        )
        .await
        .expect("discard succeeds");

        assert_eq!(result.trashed, ["tracked.txt", "untracked.txt"]);
        assert!(result.permanently_discarded.is_empty());
        assert!(result.trash_unavailable.is_empty());
        assert_eq!(
            std::fs::read_to_string(fixture.path().join("tracked.txt"))
                .expect("tracked file restored"),
            "base\n"
        );
        assert!(!fixture.path().join("untracked.txt").exists());
    }

    #[tokio::test]
    async fn discard_requires_permission_before_permanently_removing_an_untracked_file() {
        fn git(cwd: &Path, args: &[&str]) {
            let output = Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .expect("git fixture starts");
            assert!(
                output.status.success(),
                "git fixture failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let fixture = tempfile::tempdir().expect("temporary repository");
        git(fixture.path(), &["init", "-q", "-b", "main"]);
        git(fixture.path(), &["config", "core.autocrlf", "false"]);
        std::fs::write(fixture.path().join("tracked.txt"), "base\n").expect("base file");
        git(fixture.path(), &["add", "tracked.txt"]);
        git(
            fixture.path(),
            &[
                "-c",
                "user.name=Git Manager Test",
                "-c",
                "user.email=git-manager@example.test",
                "commit",
                "-q",
                "-m",
                "base",
            ],
        );
        std::fs::write(fixture.path().join("untracked.txt"), "untracked\n")
            .expect("untracked file");
        let repository = GitRepository::default();

        let unavailable = discard_paths(
            &repository,
            Arc::new(UnavailableTrash),
            DiscardRequest {
                cwd: fixture.path().to_path_buf(),
                paths: vec!["untracked.txt".into()],
                permit_permanent: false,
            },
            &CancellationToken::new(),
        )
        .await
        .expect("trash unavailability is an outcome");
        assert_eq!(unavailable.trash_unavailable, ["untracked.txt"]);
        assert!(fixture.path().join("untracked.txt").exists());

        let permanent = discard_paths(
            &repository,
            Arc::new(UnavailableTrash),
            DiscardRequest {
                cwd: fixture.path().to_path_buf(),
                paths: vec!["untracked.txt".into()],
                permit_permanent: true,
            },
            &CancellationToken::new(),
        )
        .await
        .expect("confirmed permanent discard succeeds");
        assert_eq!(permanent.permanently_discarded, ["untracked.txt"]);
        assert!(!fixture.path().join("untracked.txt").exists());
    }

    #[tokio::test]
    async fn discard_restores_an_absent_tracked_file_without_trash_confirmation() {
        fn git(cwd: &Path, args: &[&str]) {
            let output = Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output()
                .expect("git fixture starts");
            assert!(
                output.status.success(),
                "git fixture failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let fixture = tempfile::tempdir().expect("temporary repository");
        git(fixture.path(), &["init", "-q", "-b", "main"]);
        git(fixture.path(), &["config", "core.autocrlf", "false"]);
        std::fs::write(fixture.path().join("tracked.txt"), "base\n").expect("base file");
        git(fixture.path(), &["add", "tracked.txt"]);
        git(
            fixture.path(),
            &[
                "-c",
                "user.name=Git Manager Test",
                "-c",
                "user.email=git-manager@example.test",
                "commit",
                "-q",
                "-m",
                "base",
            ],
        );
        let repository = GitRepository::default();

        for permit_permanent in [false, true] {
            std::fs::remove_file(fixture.path().join("tracked.txt")).expect("delete tracked file");
            let outcome = discard_paths(
                &repository,
                Arc::new(UnavailableTrash),
                DiscardRequest {
                    cwd: fixture.path().to_path_buf(),
                    paths: vec!["tracked.txt".into()],
                    permit_permanent,
                },
                &CancellationToken::new(),
            )
            .await
            .expect("deleted tracked file is restored");

            assert!(outcome.trash_unavailable.is_empty());
            assert!(outcome.permanently_discarded.is_empty());
            assert_eq!(
                std::fs::read_to_string(fixture.path().join("tracked.txt"))
                    .expect("tracked file restored"),
                "base\n"
            );
        }
    }
}
