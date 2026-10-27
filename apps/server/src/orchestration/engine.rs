use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    future::Future,
    path::Path,
    pin::Pin,
    sync::{
        Arc, Condvar, Mutex as StdMutex, Weak,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use rusqlite::{OptionalExtension, Row, Transaction, params};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::{
    sync::{Mutex as AsyncMutex, Notify, OwnedMutexGuard, broadcast, mpsc, oneshot},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::orchestration::delivery::{
    CommandAdmission, TurnDeliveryFailureReason, TurnDeliveryState, TurnDeliveryTransition,
};
use crate::persistence::{
    CheckpointDiffBlob, CommandReceipt, CommitFence, Database, NewOrchestrationEvent,
    OrchestrationEvent, PersistenceError, ProjectionPendingApproval, ProjectionProject,
    ProjectionState, ProjectionThread, ProjectionThreadActivity, ProjectionThreadMessage,
    ProjectionThreadProposedPlan, ProjectionThreadSession, ProjectionTurn, Repositories,
    can_promote_queued_provider_turn_on, finalize_command_receipt_on, hold_queued_provider_turns,
};
use crate::{
    checkpointing,
    git::{canonical_worktree_path_key, host_path_platform, normalize_worktree_path_key},
};

const TURN_UPSERT_SQL: &str = "INSERT INTO projection_turns (thread_id, turn_id, pending_message_id, source_proposed_plan_thread_id, source_proposed_plan_id, assistant_message_id, state, requested_at, started_at, completed_at, checkpoint_turn_count, checkpoint_ref, checkpoint_status, checkpoint_files_json) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT (thread_id, turn_id) DO UPDATE SET pending_message_id=excluded.pending_message_id, source_proposed_plan_thread_id=excluded.source_proposed_plan_thread_id, source_proposed_plan_id=excluded.source_proposed_plan_id, assistant_message_id=excluded.assistant_message_id, state=excluded.state, requested_at=excluded.requested_at, started_at=excluded.started_at, completed_at=excluded.completed_at, checkpoint_turn_count=excluded.checkpoint_turn_count, checkpoint_ref=excluded.checkpoint_ref, checkpoint_status=excluded.checkpoint_status, checkpoint_files_json=excluded.checkpoint_files_json";
const DEFAULT_WORKTREE_DISCOVERY_JSON: &str =
    r#"{"visibility":"hidden","initialPromptDismissedAt":null,"baselinePaths":[]}"#;
const PROJECTOR_NAMES: [&str; 9] = [
    "projection.projects",
    "projection.thread-messages",
    "projection.thread-proposed-plans",
    "projection.thread-activities",
    "projection.thread-sessions",
    "projection.thread-turns",
    "projection.checkpoints",
    "projection.pending-approvals",
    "projection.threads",
];
const HISTORICAL_PROJECT_ROOT_INSPECTION_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ProjectionMode {
    #[default]
    Legacy,
    UpdateExistingAssistantMessage,
}

#[derive(Clone, Copy, Debug, Default)]
struct ProjectionContext {
    mode: ProjectionMode,
    existing_assistant_message_updated: bool,
}

impl ProjectionContext {
    fn new(mode: ProjectionMode) -> Self {
        Self {
            mode,
            existing_assistant_message_updated: false,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub enum OptionalNullable<T> {
    #[default]
    Missing,
    Present(Option<T>),
}

impl<T> OptionalNullable<T> {
    fn is_missing(&self) -> bool {
        matches!(self, Self::Missing)
    }

    fn as_ref(&self) -> Option<Option<&T>> {
        match self {
            Self::Missing => None,
            Self::Present(value) => Some(value.as_ref()),
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for OptionalNullable<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Option::<T>::deserialize(deserializer).map(Self::Present)
    }
}

impl<T: Serialize> Serialize for OptionalNullable<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Missing => serializer.serialize_none(),
            Self::Present(value) => value.serialize(serializer),
        }
    }
}

fn optional_nullable_is_missing<T>(value: &OptionalNullable<T>) -> bool {
    value.is_missing()
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TurnDeliveryResolutionAction {
    Retry,
    Dismiss,
    Cancel,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ThreadMessageInput {
    #[serde(rename = "messageId")]
    pub message_id: String,
    pub role: String,
    pub text: String,
    pub attachments: Vec<Value>,
}

/// One historical message appended by `thread.history.import`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImportedThreadMessage {
    #[serde(rename = "messageId")]
    pub message_id: String,
    pub role: String,
    pub text: String,
    #[serde(rename = "createdAt")]
    pub created_at: String,
}

/// The most messages one `thread.history.import` command appends.
pub const MAX_IMPORTED_THREAD_MESSAGES: usize = 200;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionInput {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    pub status: String,
    #[serde(rename = "providerName")]
    pub provider_name: Option<String>,
    #[serde(
        rename = "providerInstanceId",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub provider_instance_id: Option<String>,
    #[serde(rename = "runtimeMode", default = "default_runtime_mode")]
    pub runtime_mode: String,
    #[serde(rename = "activeTurnId")]
    pub active_turn_id: Option<String>,
    #[serde(rename = "lastError")]
    pub last_error: Option<String>,
    /// Which side reported `last_error` — a `RuntimeErrorClass`. `last_error`
    /// alone is mixed-provenance: it carries both a provider's own failure and
    /// Coddy Studio's restart notice, so the UI cannot attribute it without this.
    #[serde(
        rename = "lastErrorClass",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub last_error_class: Option<String>,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProposedPlanInput {
    pub id: String,
    #[serde(rename = "turnId")]
    pub turn_id: Option<String>,
    #[serde(rename = "planMarkdown")]
    pub plan_markdown: String,
    #[serde(rename = "implementedAt", default)]
    pub implemented_at: Option<String>,
    #[serde(rename = "implementationThreadId", default)]
    pub implementation_thread_id: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActivityInput {
    pub id: String,
    pub tone: String,
    pub kind: String,
    pub summary: String,
    pub payload: Value,
    #[serde(rename = "turnId")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sequence: Option<i64>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadTurnStartBootstrapCreateThread {
    pub project_id: String,
    pub title: String,
    pub model_selection: Value,
    pub runtime_mode: String,
    pub interaction_mode: String,
    pub branch: Option<String>,
    #[serde(default)]
    pub worktree_path: Option<String>,
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadTurnStartBootstrapPrepareWorktree {
    #[serde(default)]
    pub project_cwd: String,
    pub base_branch: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_from_origin: Option<bool>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadTurnStartBootstrap {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_thread: Option<ThreadTurnStartBootstrapCreateThread>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepare_worktree: Option<ThreadTurnStartBootstrapPrepareWorktree>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_setup_script: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BootstrapWorktree {
    pub repository_root: String,
    pub branch: String,
    pub path: String,
    pub remove_branch: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BootstrapSetupInput {
    pub thread_id: String,
    pub project_id: Option<String>,
    pub project_cwd: Option<String>,
    pub worktree_path: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BootstrapSetupResult {
    NoScript,
    Started {
        script_id: String,
        script_name: String,
        terminal_id: String,
    },
}

pub type BoxBootstrapFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

pub trait ThreadTurnBootstrapEffects: Send + Sync {
    fn prepare_worktree<'a>(
        &'a self,
        input: ThreadTurnStartBootstrapPrepareWorktree,
        cancellation: &'a CancellationToken,
    ) -> BoxBootstrapFuture<'a, BootstrapWorktree>;

    fn run_setup_script<'a>(
        &'a self,
        input: BootstrapSetupInput,
    ) -> BoxBootstrapFuture<'a, BootstrapSetupResult>;
}

pub type BoxProjectCommandFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

pub trait ProjectCommandEffects: Send + Sync {
    fn normalize_workspace_root_lexically(&self, workspace_root: &str) -> String;

    fn canonicalize_workspace_root<'a>(
        &'a self,
        workspace_root: &'a str,
        allow_missing: bool,
    ) -> BoxProjectCommandFuture<'a, String>;

    fn prepare_project_create<'a>(
        &'a self,
        workspace_root: &'a str,
        create_if_missing: bool,
        initialize_git: bool,
    ) -> BoxProjectCommandFuture<'a, ()>;
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum OrchestrationCommand {
    #[serde(rename = "project.create")]
    ProjectCreate {
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "projectId")]
        project_id: String,
        title: String,
        #[serde(rename = "workspaceRoot")]
        workspace_root: String,
        #[serde(
            rename = "createWorkspaceRootIfMissing",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        create_workspace_root_if_missing: Option<bool>,
        #[serde(
            rename = "initializeGit",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        initialize_git: Option<bool>,
        #[serde(
            rename = "defaultModelSelection",
            default,
            skip_serializing_if = "optional_nullable_is_missing"
        )]
        default_model_selection: OptionalNullable<Value>,
        #[serde(rename = "createdAt")]
        created_at: String,
    },
    #[serde(rename = "project.meta.update")]
    ProjectMetaUpdate {
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "projectId")]
        project_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(
            rename = "workspaceRoot",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        workspace_root: Option<String>,
        #[serde(
            rename = "defaultModelSelection",
            default,
            skip_serializing_if = "optional_nullable_is_missing"
        )]
        default_model_selection: OptionalNullable<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scripts: Option<Vec<Value>>,
        #[serde(
            rename = "worktreeDiscovery",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        worktree_discovery: Option<Value>,
    },
    #[serde(rename = "project.repository-identity.set")]
    ProjectRepositoryIdentitySet {
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "projectId")]
        project_id: String,
        #[serde(rename = "repositoryIdentity", default)]
        repository_identity: Option<Value>,
    },
    #[serde(rename = "project.delete")]
    ProjectDelete {
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "projectId")]
        project_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        force: Option<bool>,
    },
    #[serde(rename = "worktree.adopt-resolved")]
    WorktreeAdoptResolved {
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "projectId")]
        project_id: String,
        #[serde(rename = "worktreeKey")]
        worktree_key: String,
        path: String,
        branch: Option<String>,
        head: Option<String>,
        #[serde(rename = "modelSelection")]
        model_selection: Value,
        #[serde(rename = "runtimeMode")]
        runtime_mode: String,
        #[serde(rename = "interactionMode")]
        interaction_mode: String,
    },
    #[serde(rename = "worktree.detach-resolved")]
    WorktreeDetachResolved {
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "projectId")]
        project_id: String,
        #[serde(rename = "threadId")]
        thread_id: String,
        path: String,
        #[serde(rename = "gitOutcome")]
        git_outcome: String,
        detail: Option<String>,
        #[serde(rename = "orphanCleanupPending")]
        orphan_cleanup_pending: bool,
    },
    #[serde(rename = "worktree.branch-reconcile-resolved")]
    WorktreeBranchReconcileResolved {
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "projectId")]
        project_id: String,
        #[serde(rename = "threadId")]
        thread_id: String,
        branch: Option<String>,
    },
    #[serde(rename = "thread.create")]
    ThreadCreate {
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "projectId")]
        project_id: String,
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        /// Host thread of a `panel` thread; only `worktree.createPanel` sets it.
        #[serde(
            rename = "hostThreadId",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        host_thread_id: Option<String>,
        #[serde(rename = "modelSelection")]
        model_selection: Value,
        #[serde(rename = "runtimeMode")]
        runtime_mode: String,
        #[serde(rename = "interactionMode", default = "default_interaction_mode")]
        interaction_mode: String,
        branch: Option<String>,
        #[serde(rename = "worktreePath", default)]
        worktree_path: Option<String>,
        #[serde(rename = "createdAt")]
        created_at: String,
    },
    /// Server-internal: appends an imported CLI session's transcript without starting a turn.
    #[serde(rename = "thread.history.import")]
    ThreadHistoryImport {
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "threadId")]
        thread_id: String,
        messages: Vec<ImportedThreadMessage>,
    },
    #[serde(rename = "thread.delete")]
    ThreadDelete {
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "threadId")]
        thread_id: String,
    },
    #[serde(rename = "thread.archive")]
    ThreadArchive {
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "threadId")]
        thread_id: String,
    },
    #[serde(rename = "thread.unarchive")]
    ThreadUnarchive {
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "threadId")]
        thread_id: String,
    },
    #[serde(rename = "thread.meta.update")]
    ThreadMetaUpdate {
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(
            rename = "modelSelection",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        model_selection: Option<Value>,
        #[serde(default, skip_serializing_if = "optional_nullable_is_missing")]
        branch: OptionalNullable<String>,
        #[serde(
            rename = "worktreePath",
            default,
            skip_serializing_if = "optional_nullable_is_missing"
        )]
        worktree_path: OptionalNullable<String>,
    },
    #[serde(rename = "thread.runtime-mode.set")]
    ThreadRuntimeModeSet {
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "runtimeMode")]
        runtime_mode: String,
        #[serde(rename = "createdAt")]
        created_at: String,
    },
    #[serde(rename = "thread.interaction-mode.set")]
    ThreadInteractionModeSet {
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "interactionMode")]
        interaction_mode: String,
        #[serde(rename = "createdAt")]
        created_at: String,
    },
    #[serde(rename = "thread.turn.start")]
    ThreadTurnStart {
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "threadId")]
        thread_id: String,
        message: ThreadMessageInput,
        #[serde(
            rename = "modelSelection",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        model_selection: Option<Value>,
        #[serde(rename = "titleSeed", default, skip_serializing_if = "Option::is_none")]
        title_seed: Option<String>,
        #[serde(rename = "runtimeMode", default = "default_runtime_mode")]
        runtime_mode: String,
        #[serde(rename = "interactionMode", default = "default_interaction_mode")]
        interaction_mode: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bootstrap: Option<Box<ThreadTurnStartBootstrap>>,
        #[serde(
            rename = "sourceProposedPlan",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        source_proposed_plan: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        queued: Option<bool>,
        #[serde(rename = "createdAt")]
        created_at: String,
    },
    #[serde(rename = "thread.turn.steer")]
    ThreadTurnSteer {
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(rename = "createdAt")]
        created_at: String,
    },
    #[serde(rename = "thread.turn.promote")]
    ThreadTurnPromote {
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(rename = "createdAt")]
        created_at: String,
    },
    #[serde(rename = "thread.turn.interrupt")]
    ThreadTurnInterrupt {
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "turnId", default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
        #[serde(rename = "createdAt")]
        created_at: String,
    },
    #[serde(rename = "thread.turn-delivery.resolve")]
    ThreadTurnDeliveryResolve {
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
        action: TurnDeliveryResolutionAction,
        #[serde(rename = "createdAt")]
        created_at: String,
    },
    #[serde(rename = "thread.approval.respond")]
    ThreadApprovalRespond {
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "requestId")]
        request_id: String,
        decision: String,
        #[serde(rename = "createdAt")]
        created_at: String,
    },
    #[serde(rename = "thread.user-input.respond")]
    ThreadUserInputRespond {
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "requestId")]
        request_id: String,
        answers: Value,
        #[serde(rename = "createdAt")]
        created_at: String,
    },
    #[serde(rename = "thread.checkpoint.revert")]
    ThreadCheckpointRevert {
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "turnCount")]
        turn_count: i64,
        #[serde(rename = "createdAt")]
        created_at: String,
    },
    #[serde(rename = "thread.session.stop")]
    ThreadSessionStop {
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "createdAt")]
        created_at: String,
    },
    #[serde(rename = "thread.session.set")]
    ThreadSessionSet {
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "threadId")]
        thread_id: String,
        session: SessionInput,
        #[serde(rename = "createdAt")]
        created_at: String,
    },
    #[serde(rename = "thread.message.assistant.delta")]
    ThreadMessageAssistantDelta {
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
        delta: String,
        #[serde(rename = "turnId", default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
        #[serde(rename = "createdAt")]
        created_at: String,
    },
    #[serde(rename = "thread.message.assistant.complete")]
    ThreadMessageAssistantComplete {
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "messageId")]
        message_id: String,
        #[serde(rename = "turnId", default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
        #[serde(rename = "createdAt")]
        created_at: String,
    },
    #[serde(rename = "thread.proposed-plan.upsert")]
    ThreadProposedPlanUpsert {
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "proposedPlan")]
        proposed_plan: ProposedPlanInput,
        #[serde(rename = "createdAt")]
        created_at: String,
    },
    #[serde(rename = "thread.turn.diff.complete")]
    ThreadTurnDiffComplete {
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "turnId")]
        turn_id: String,
        #[serde(rename = "checkpointTurnCount")]
        checkpoint_turn_count: i64,
        #[serde(rename = "checkpointRef")]
        checkpoint_ref: String,
        status: String,
        files: Value,
        #[serde(rename = "assistantMessageId")]
        assistant_message_id: Option<String>,
        #[serde(rename = "completedAt")]
        completed_at: String,
        #[serde(rename = "createdAt")]
        created_at: String,
    },
    #[serde(rename = "thread.activity.append")]
    ThreadActivityAppend {
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "threadId")]
        thread_id: String,
        activity: ActivityInput,
        #[serde(rename = "createdAt")]
        created_at: String,
    },
    #[serde(rename = "thread.revert.complete")]
    ThreadRevertComplete {
        #[serde(rename = "commandId")]
        command_id: String,
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "turnCount")]
        turn_count: i64,
        #[serde(rename = "createdAt")]
        created_at: String,
    },
}

fn default_runtime_mode() -> String {
    "full-access".to_owned()
}
fn default_interaction_mode() -> String {
    "default".to_owned()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DispatchResult {
    pub sequence: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disposition: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct EngineOptions {
    pub queue_capacity: usize,
    pub test_hooks: TestHooks,
}

#[derive(Clone, Debug, Default)]
pub struct TestHooks {
    fail_next_projector: Arc<StdMutex<Option<FailProjector>>>,
    pause_after_admission_commit: Arc<StdMutex<Option<AdmissionCommitPause>>>,
    #[cfg(test)]
    generic_external_preparation_attempts: Arc<AtomicUsize>,
    pause_before_command_persist: Arc<StdMutex<Option<AdmissionCommitPause>>>,
    pause_before_command_finalization: Arc<StdMutex<Option<PersistenceCommitPause>>>,
    pause_after_command_finalization: Arc<StdMutex<Option<PersistenceCommitPause>>>,
    fail_delivery_transitions: Arc<AtomicUsize>,
    delivery_transition_attempts: Arc<AtomicUsize>,
}

#[derive(Clone, Debug)]
pub struct AdmissionCommitPause {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[derive(Clone, Debug)]
pub struct PersistenceCommitPause {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<(StdMutex<bool>, Condvar)>,
}

impl AdmissionCommitPause {
    pub async fn wait_until_entered(&self) {
        self.entered.notified().await;
    }

    pub fn release(&self) {
        self.release.notify_one();
    }
}

impl PersistenceCommitPause {
    pub async fn wait_until_entered(&self) {
        self.entered.notified().await;
    }

    pub fn release(&self) {
        let (released, changed) = self.release.as_ref();
        *released.lock().expect("persistence pause mutex") = true;
        changed.notify_one();
    }

    fn block_until_released(&self) {
        self.entered.notify_one();
        let (released, changed) = self.release.as_ref();
        let mut released = released.lock().expect("persistence pause mutex");
        while !*released {
            released = changed
                .wait(released)
                .expect("persistence pause mutex after wait");
        }
    }
}

#[derive(Clone, Debug)]
struct FailProjector {
    projector: String,
    event_type: Option<String>,
    remaining: usize,
}

impl TestHooks {
    pub fn fail_next_projector(&self, projector: impl Into<String>, event_type: Option<&str>) {
        self.fail_next_projectors(projector, event_type, 1);
    }

    pub fn fail_next_projectors(
        &self,
        projector: impl Into<String>,
        event_type: Option<&str>,
        count: usize,
    ) {
        let mut guard = self.fail_next_projector.lock().expect("failpoint mutex");
        *guard = (count > 0).then(|| FailProjector {
            projector: projector.into(),
            event_type: event_type.map(str::to_owned),
            remaining: count,
        });
    }

    pub fn pause_after_next_admission_commit(&self) -> AdmissionCommitPause {
        let pause = AdmissionCommitPause {
            entered: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new(tokio::sync::Notify::new()),
        };
        *self
            .pause_after_admission_commit
            .lock()
            .expect("admission pause mutex") = Some(pause.clone());
        pause
    }

    pub fn pause_before_next_command_persist(&self) -> AdmissionCommitPause {
        let pause = AdmissionCommitPause {
            entered: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new(tokio::sync::Notify::new()),
        };
        *self
            .pause_before_command_persist
            .lock()
            .expect("command persist pause mutex") = Some(pause.clone());
        pause
    }

    #[cfg(test)]
    pub(crate) fn note_generic_external_preparation_attempt(&self) {
        self.generic_external_preparation_attempts
            .fetch_add(1, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn generic_external_preparation_attempts(&self) -> usize {
        self.generic_external_preparation_attempts
            .load(Ordering::SeqCst)
    }

    pub fn pause_before_next_command_finalization(&self) -> PersistenceCommitPause {
        let pause = PersistenceCommitPause {
            entered: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new((StdMutex::new(false), Condvar::new())),
        };
        *self
            .pause_before_command_finalization
            .lock()
            .expect("command finalization pause mutex") = Some(pause.clone());
        pause
    }

    pub fn pause_after_next_command_finalization(&self) -> PersistenceCommitPause {
        let pause = PersistenceCommitPause {
            entered: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new((StdMutex::new(false), Condvar::new())),
        };
        *self
            .pause_after_command_finalization
            .lock()
            .expect("post-finalization pause mutex") = Some(pause.clone());
        pause
    }

    pub fn fail_next_delivery_transitions(&self, count: usize) {
        self.fail_delivery_transitions
            .store(count, Ordering::SeqCst);
    }

    pub fn delivery_transition_attempts(&self) -> usize {
        self.delivery_transition_attempts.load(Ordering::SeqCst)
    }

    fn maybe_fail_delivery_transition(&self) -> Result<(), OrchestrationError> {
        self.delivery_transition_attempts
            .fetch_add(1, Ordering::SeqCst);
        if self
            .fail_delivery_transitions
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                if remaining > 0 {
                    Some(remaining - 1)
                } else {
                    None
                }
            })
            .is_ok()
        {
            return Err(OrchestrationError::InjectedProjectorFailure {
                projector: "provider.turn-delivery-transition".to_owned(),
                event_type: "thread.turn-delivery-updated".to_owned(),
            });
        }
        Ok(())
    }

    async fn maybe_pause_after_admission_commit(&self) {
        let pause = self
            .pause_after_admission_commit
            .lock()
            .expect("admission pause mutex")
            .take();
        if let Some(pause) = pause {
            pause.entered.notify_one();
            pause.release.notified().await;
        }
    }

    async fn maybe_pause_before_command_persist(&self) {
        let pause = self
            .pause_before_command_persist
            .lock()
            .expect("command persist pause mutex")
            .take();
        if let Some(pause) = pause {
            pause.entered.notify_one();
            pause.release.notified().await;
        }
    }

    fn maybe_pause_before_command_finalization(&self) {
        let pause = self
            .pause_before_command_finalization
            .lock()
            .expect("command finalization pause mutex")
            .take();
        if let Some(pause) = pause {
            pause.block_until_released();
        }
    }

    fn maybe_pause_after_command_finalization(&self) {
        let pause = self
            .pause_after_command_finalization
            .lock()
            .expect("post-finalization pause mutex")
            .take();
        if let Some(pause) = pause {
            pause.block_until_released();
        }
    }

    fn maybe_fail(&self, projector: &str, event_type: &str) -> Result<(), OrchestrationError> {
        let mut guard = self.fail_next_projector.lock().expect("failpoint mutex");
        let should_fail = guard
            .as_ref()
            .map(|failpoint| {
                failpoint.projector == projector
                    && failpoint
                        .event_type
                        .as_deref()
                        .is_none_or(|candidate| candidate == event_type)
            })
            .unwrap_or(false);
        if should_fail {
            if let Some(failpoint) = guard.as_mut() {
                failpoint.remaining = failpoint.remaining.saturating_sub(1);
                if failpoint.remaining == 0 {
                    *guard = None;
                }
            }
            return Err(OrchestrationError::InjectedProjectorFailure {
                projector: projector.to_owned(),
                event_type: event_type.to_owned(),
            });
        }
        Ok(())
    }
}

#[derive(Debug, Error, Clone)]
pub enum OrchestrationError {
    #[error("orchestration command invariant failed ({command_type}): {detail}")]
    Invariant {
        command_type: String,
        detail: String,
    },
    #[error("command previously rejected ({command_id}): {detail}")]
    PreviouslyRejected { command_id: String, detail: String },
    #[error("command payload conflicts with the accepted command ({command_id})")]
    CommandConflict { command_id: String },
    #[error(
        "worktree ownership conflict for project {project_id}: {owner_count} canonical workspace threads"
    )]
    WorktreeOwnershipConflict {
        project_id: String,
        owner_count: usize,
    },
    #[error("workspace ownership changed while command was waiting ({command_id})")]
    WorkspaceOwnershipChanged { command_id: String },
    #[error(
        "workspace ownership identity could not be resolved for {path} ({command_id}): {detail}"
    )]
    WorkspaceOwnershipIdentity {
        command_id: String,
        path: String,
        detail: String,
    },
    #[error("orchestration worker has already shut down")]
    WorkerClosed,
    #[error("orchestration worker cancelled")]
    Cancelled,
    #[error("orchestration response channel dropped")]
    ResponseDropped,
    #[error(transparent)]
    Persistence(#[from] Arc<PersistenceError>),
    #[error("projector {projector} failed for {event_type}")]
    InjectedProjectorFailure {
        projector: String,
        event_type: String,
    },
    #[error("thread turn bootstrap failed during {stage}: {detail}")]
    Bootstrap { stage: &'static str, detail: String },
    #[error("project command preparation failed: {detail}")]
    ProjectPreparation { detail: String },
}

#[derive(Clone, Debug)]
struct CommandModel {
    projects: BTreeMap<String, ProjectState>,
    threads: BTreeMap<String, ThreadState>,
    project_roots_canonicalized: bool,
}

#[derive(Clone, Debug)]
struct ProjectState {
    workspace_root: String,
    worktree_discovery: Value,
    deleted_at: Option<String>,
}

#[derive(Clone, Debug)]
struct ThreadState {
    project_id: String,
    kind: String,
    host_thread_id: Option<String>,
    runtime_mode: String,
    interaction_mode: String,
    branch: Option<String>,
    worktree_path: Option<String>,
    worktree_path_key: Option<String>,
    archived_at: Option<String>,
    deleted_at: Option<String>,
}

fn canonical_default_thread_id(model: &CommandModel, project_id: &str) -> Option<String> {
    model.threads.iter().find_map(|(thread_id, thread)| {
        (thread.project_id == project_id && thread.kind == "default" && thread.deleted_at.is_none())
            .then(|| thread_id.clone())
    })
}

fn canonical_worktree_owners<'a>(
    model: &'a CommandModel,
    project_id: &'a str,
    path_key: &'a str,
) -> impl Iterator<Item = (&'a String, &'a ThreadState)> {
    model.threads.iter().filter(move |(_, thread)| {
        thread.project_id == project_id
            && thread.kind == "workspace"
            && thread.deleted_at.is_none()
            && thread.worktree_path_key.as_deref() == Some(path_key)
    })
}

fn normalized_worktree_path_key(path: &str) -> String {
    normalize_worktree_path_key(Path::new(path), host_path_platform())
}

async fn command_workspace_ownership_keys(
    repositories: &Repositories,
    command: &OrchestrationCommand,
) -> Result<Vec<String>, OrchestrationError> {
    let mut paths = Vec::new();
    match command {
        OrchestrationCommand::ProjectCreate { workspace_root, .. } => {
            paths.push(workspace_root.clone());
        }
        OrchestrationCommand::ProjectMetaUpdate {
            project_id,
            workspace_root: Some(next_root),
            ..
        } => {
            if let Some(project) = repositories
                .get_project(project_id.clone())
                .await
                .map_err(wrap_persistence)?
            {
                paths.push(project.workspace_root);
            }
            paths.push(next_root.clone());
        }
        OrchestrationCommand::ProjectDelete { project_id, .. } => {
            if let Some(project) = repositories
                .get_project(project_id.clone())
                .await
                .map_err(wrap_persistence)?
            {
                paths.push(project.workspace_root);
            }
            paths.extend(
                repositories
                    .list_threads_by_project(project_id.clone())
                    .await
                    .map_err(wrap_persistence)?
                    .into_iter()
                    .filter(|thread| thread.kind == "workspace" && thread.deleted_at.is_none())
                    .filter_map(|thread| thread.worktree_path),
            );
        }
        OrchestrationCommand::WorktreeAdoptResolved { path, .. }
        | OrchestrationCommand::WorktreeDetachResolved { path, .. } => paths.push(path.clone()),
        OrchestrationCommand::ThreadCreate {
            kind,
            worktree_path: Some(path),
            ..
        } if kind.as_deref().unwrap_or("workspace") == "workspace" => paths.push(path.clone()),
        OrchestrationCommand::ThreadDelete { thread_id, .. } => {
            if let Some(thread) = repositories
                .get_thread(thread_id.clone())
                .await
                .map_err(wrap_persistence)?
                && thread.kind == "workspace"
                && thread.deleted_at.is_none()
                && let Some(path) = thread.worktree_path
            {
                paths.push(path);
            }
        }
        OrchestrationCommand::ThreadMetaUpdate {
            thread_id,
            worktree_path: OptionalNullable::Present(next_path),
            ..
        } => {
            if let Some(thread) = repositories
                .get_thread(thread_id.clone())
                .await
                .map_err(wrap_persistence)?
                && thread.kind == "workspace"
                && thread.deleted_at.is_none()
            {
                if let Some(path) = thread.worktree_path {
                    paths.push(path);
                }
                if let Some(path) = next_path {
                    paths.push(path.clone());
                }
            }
        }
        OrchestrationCommand::ThreadTurnStart {
            thread_id,
            bootstrap: Some(bootstrap),
            ..
        } => {
            if repositories
                .get_thread(thread_id.clone())
                .await
                .map_err(wrap_persistence)?
                .is_none()
                && let Some(path) = bootstrap
                    .create_thread
                    .as_ref()
                    .and_then(|create| create.worktree_path.as_ref())
            {
                paths.push(path.clone());
            }
        }
        _ => {}
    }
    let mut keys = Vec::with_capacity(paths.len());
    for path in paths {
        keys.push(
            canonical_worktree_path_key(Path::new(&path))
                .await
                .map_err(|error| OrchestrationError::WorkspaceOwnershipIdentity {
                    command_id: command.command_id().to_owned(),
                    path,
                    detail: error.to_string(),
                })?,
        );
    }
    keys.sort();
    keys.dedup();
    Ok(keys)
}

enum DurableWorktreeAdoptionResult {
    Absent,
    Valid {
        thread_id: String,
        disposition: String,
    },
    Malformed {
        detail: &'static str,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DurableWorktreeRemovalResult {
    pub thread_removed: bool,
    pub git_outcome: String,
    pub detail: Option<String>,
    pub orphan_cleanup_pending: bool,
}

fn durable_worktree_removal_result(
    events: &VecDeque<OrchestrationEvent>,
) -> Result<DurableWorktreeRemovalResult, OrchestrationError> {
    let command_type = "worktree.detach-resolved".to_owned();
    let mut results = events
        .iter()
        .filter(|event| event.event.metadata.get("removalResult").is_some());
    let event = results
        .next()
        .ok_or_else(|| OrchestrationError::Invariant {
            command_type: command_type.clone(),
            detail: "Accepted removal receipt has no durable result.".to_owned(),
        })?;
    if results.next().is_some() || event.event.event_type != "project.meta-updated" {
        return Err(OrchestrationError::Invariant {
            command_type,
            detail: "Removal receipt has an invalid durable result event.".to_owned(),
        });
    }
    let result = event
        .event
        .metadata
        .get("removalResult")
        .and_then(Value::as_object)
        .ok_or_else(|| OrchestrationError::Invariant {
            command_type: "worktree.detach-resolved".to_owned(),
            detail: "Removal result must be an object.".to_owned(),
        })?;
    let thread_removed = result
        .get("threadRemoved")
        .and_then(Value::as_bool)
        .filter(|removed| *removed)
        .ok_or_else(|| OrchestrationError::Invariant {
            command_type: "worktree.detach-resolved".to_owned(),
            detail: "Removal result must confirm the thread was removed.".to_owned(),
        })?;
    let git_outcome = result
        .get("gitOutcome")
        .and_then(Value::as_str)
        .filter(|outcome| matches!(*outcome, "not-requested" | "removed" | "cleaned" | "failed"))
        .ok_or_else(|| OrchestrationError::Invariant {
            command_type: "worktree.detach-resolved".to_owned(),
            detail: "Removal result Git outcome is invalid.".to_owned(),
        })?
        .to_owned();
    let detail = match result.get("detail") {
        None | Some(Value::Null) => None,
        Some(Value::String(detail)) => Some(detail.clone()),
        Some(_) => {
            return Err(OrchestrationError::Invariant {
                command_type: "worktree.detach-resolved".to_owned(),
                detail: "Removal result detail is invalid.".to_owned(),
            });
        }
    };
    let orphan_cleanup_pending = result
        .get("orphanCleanupPending")
        .and_then(Value::as_bool)
        .ok_or_else(|| OrchestrationError::Invariant {
            command_type: "worktree.detach-resolved".to_owned(),
            detail: "Removal result cleanup state is invalid.".to_owned(),
        })?;
    Ok(DurableWorktreeRemovalResult {
        thread_removed,
        git_outcome,
        detail,
        orphan_cleanup_pending,
    })
}

fn durable_worktree_adoption_result(
    events: &VecDeque<OrchestrationEvent>,
) -> DurableWorktreeAdoptionResult {
    let mut result_events = events
        .iter()
        .filter(|event| event.event.metadata.get("adoptionResult").is_some());
    let Some(event) = result_events.next() else {
        return DurableWorktreeAdoptionResult::Absent;
    };
    if result_events.next().is_some() {
        return DurableWorktreeAdoptionResult::Malformed {
            detail: "Adoption receipt has multiple durable results.",
        };
    }
    if event.event.event_type != "project.meta-updated" {
        return DurableWorktreeAdoptionResult::Malformed {
            detail: "Adoption result is attached to the wrong event type.",
        };
    }
    let Some(result) = event
        .event
        .metadata
        .get("adoptionResult")
        .and_then(Value::as_object)
    else {
        return DurableWorktreeAdoptionResult::Malformed {
            detail: "Adoption result must be an object.",
        };
    };
    let Some(thread_id) = result.get("threadId").and_then(Value::as_str) else {
        return DurableWorktreeAdoptionResult::Malformed {
            detail: "Adoption result thread ID must be a string.",
        };
    };
    if thread_id.trim().is_empty() {
        return DurableWorktreeAdoptionResult::Malformed {
            detail: "Adoption result thread ID must not be empty.",
        };
    }
    let Some(disposition) = result.get("disposition").and_then(Value::as_str) else {
        return DurableWorktreeAdoptionResult::Malformed {
            detail: "Adoption result disposition must be a string.",
        };
    };
    if !matches!(disposition, "created" | "existing" | "restored") {
        return DurableWorktreeAdoptionResult::Malformed {
            detail: "Adoption result disposition is unknown.",
        };
    }
    let immutable_thread_result = match immutable_worktree_adoption_thread_result(events) {
        Ok(result) => result,
        Err(detail) => return DurableWorktreeAdoptionResult::Malformed { detail },
    };
    let consistent = match disposition {
        "created" => immutable_thread_result
            .as_ref()
            .is_some_and(|result| result.0 == thread_id && result.1 == "created"),
        "restored" => immutable_thread_result
            .as_ref()
            .is_some_and(|result| result.0 == thread_id && result.1 == "restored"),
        "existing" => immutable_thread_result.is_none(),
        _ => false,
    };
    if !consistent {
        return DurableWorktreeAdoptionResult::Malformed {
            detail: "Adoption result conflicts with immutable command events.",
        };
    }
    DurableWorktreeAdoptionResult::Valid {
        thread_id: thread_id.to_owned(),
        disposition: disposition.to_owned(),
    }
}

fn immutable_worktree_adoption_thread_result(
    events: &VecDeque<OrchestrationEvent>,
) -> Result<Option<(String, &'static str)>, &'static str> {
    let mut thread_events = events.iter().filter(|event| {
        matches!(
            event.event.event_type.as_str(),
            "thread.created" | "thread.unarchived"
        )
    });
    let Some(event) = thread_events.next() else {
        return Ok(None);
    };
    if thread_events.next().is_some() {
        return Err("Adoption receipt has multiple immutable thread events.");
    }
    let thread_id = event.event.aggregate_id.as_str();
    if thread_id.trim().is_empty()
        || event.event.payload.get("threadId").and_then(Value::as_str) != Some(thread_id)
    {
        return Err("Adoption thread event has an invalid thread identity.");
    }
    let disposition = match event.event.event_type.as_str() {
        "thread.created" => "created",
        "thread.unarchived" => "restored",
        _ => return Err("Adoption thread event type is invalid."),
    };
    Ok(Some((thread_id.to_owned(), disposition)))
}

fn replayable_worktree_adoption_result(
    events: &VecDeque<OrchestrationEvent>,
) -> Result<(String, String), OrchestrationError> {
    match durable_worktree_adoption_result(events) {
        DurableWorktreeAdoptionResult::Valid {
            thread_id,
            disposition,
        } => Ok((thread_id, disposition)),
        DurableWorktreeAdoptionResult::Malformed { detail } => Err(OrchestrationError::Invariant {
            command_type: "worktree.adopt-resolved".to_owned(),
            detail: detail.to_owned(),
        }),
        DurableWorktreeAdoptionResult::Absent => {
            let Some((thread_id, disposition)) = immutable_worktree_adoption_thread_result(events)
                .map_err(|detail| OrchestrationError::Invariant {
                    command_type: "worktree.adopt-resolved".to_owned(),
                    detail: detail.to_owned(),
                })?
            else {
                return Err(OrchestrationError::Invariant {
                    command_type: "worktree.adopt-resolved".to_owned(),
                    detail: "Accepted adoption receipt has no immutable result.".to_owned(),
                });
            };
            Ok((thread_id, disposition.to_owned()))
        }
    }
}

fn worktree_adoption_result(
    command: &OrchestrationCommand,
    committed: &VecDeque<OrchestrationEvent>,
) -> Result<Option<(String, String)>, OrchestrationError> {
    if !matches!(command, OrchestrationCommand::WorktreeAdoptResolved { .. }) {
        return Ok(None);
    }
    replayable_worktree_adoption_result(committed).map(Some)
}

async fn durable_replayed_worktree_adoption_result(
    repositories: &Repositories,
    command_id: &str,
    result_sequence: i64,
) -> Result<(String, String), OrchestrationError> {
    let from_sequence_exclusive = result_sequence.saturating_sub(2).max(0);
    let command_events = repositories
        .read_events_from_sequence(from_sequence_exclusive, 2)
        .await
        .map_err(wrap_persistence)?
        .into_iter()
        .filter(|event| event.event.command_id.as_deref() == Some(command_id))
        .collect::<VecDeque<_>>();
    replayable_worktree_adoption_result(&command_events)
}

async fn replayed_worktree_adoption_result(
    repositories: &Repositories,
    command: &OrchestrationCommand,
    result_sequence: i64,
) -> Result<Option<(String, String)>, OrchestrationError> {
    if !matches!(command, OrchestrationCommand::WorktreeAdoptResolved { .. }) {
        return Ok(None);
    }
    durable_replayed_worktree_adoption_result(repositories, command.command_id(), result_sequence)
        .await
        .map(Some)
}

struct CommandEnvelope {
    command: OrchestrationCommand,
    admission: Option<CommandAdmission>,
    lifetime: Option<CommandLifetimeGuard>,
    command_claim: CommandAdmissionClaim,
    ownership: Option<WorkspaceOwnershipLease>,
    response: oneshot::Sender<Result<DispatchResult, OrchestrationError>>,
    on_commit: Option<Box<dyn FnOnce() + Send + 'static>>,
}

struct CommandDispatchOptions<'a> {
    on_commit: Option<Box<dyn FnOnce() + Send + 'static>>,
    handoff_cancellation: Option<&'a CancellationToken>,
}

#[derive(Clone, Default)]
struct CommandAdmissionFence {
    keys: Arc<StdMutex<HashMap<String, Weak<AsyncMutex<()>>>>>,
}

#[derive(Clone)]
pub(crate) struct CommandAdmissionClaim {
    inner: Arc<CommandAdmissionClaimInner>,
}

struct CommandAdmissionClaimInner {
    command_id: String,
    _guard: OwnedMutexGuard<()>,
}

impl CommandAdmissionFence {
    async fn acquire(&self, command_id: &str) -> CommandAdmissionClaim {
        let gate = {
            let mut registry = self
                .keys
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.retain(|_, gate| gate.strong_count() > 0);
            registry
                .get(command_id)
                .and_then(Weak::upgrade)
                .unwrap_or_else(|| {
                    let gate = Arc::new(AsyncMutex::new(()));
                    registry.insert(command_id.to_owned(), Arc::downgrade(&gate));
                    gate
                })
        };
        CommandAdmissionClaim {
            inner: Arc::new(CommandAdmissionClaimInner {
                command_id: command_id.to_owned(),
                _guard: gate.lock_owned().await,
            }),
        }
    }

    #[cfg(test)]
    fn retained_key_count(&self) -> usize {
        let mut registry = self
            .keys
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        registry.retain(|_, gate| gate.strong_count() > 0);
        registry.len()
    }
}

impl CommandAdmissionClaim {
    pub(crate) fn ensure_owns(&self, command_id: &str) -> Result<(), OrchestrationError> {
        if self.inner.command_id == command_id {
            Ok(())
        } else {
            Err(OrchestrationError::CommandConflict {
                command_id: command_id.to_owned(),
            })
        }
    }
}

impl std::fmt::Debug for CommandAdmissionClaim {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CommandAdmissionClaim")
            .field("command_id", &self.inner.command_id)
            .finish_non_exhaustive()
    }
}

/// One lock per thread that its checkpoint revert and its turn deliveries each hold for their
/// whole run, so no turn starts between a revert's busy check and its completion, and a revert
/// waits for a delivery already in flight. It is in memory only: the effects worker never
/// replays a revert requested before a restart, so no revert can outlive the process.
#[derive(Clone, Default)]
struct ThreadTurnFence {
    threads: Arc<StdMutex<HashMap<String, Weak<AsyncMutex<()>>>>>,
}

/// Held while a thread's revert or turn delivery runs; dropping it releases the thread.
pub(crate) struct ThreadTurnReservation {
    _guard: OwnedMutexGuard<()>,
}

impl ThreadTurnFence {
    async fn reserve(&self, thread_id: &str) -> ThreadTurnReservation {
        ThreadTurnReservation {
            _guard: self.gate(thread_id).lock_owned().await,
        }
    }

    fn gate(&self, thread_id: &str) -> Arc<AsyncMutex<()>> {
        let mut threads = self
            .threads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        threads.retain(|_, gate| gate.strong_count() > 0);
        threads
            .get(thread_id)
            .and_then(Weak::upgrade)
            .unwrap_or_else(|| {
                let gate = Arc::new(AsyncMutex::new(()));
                threads.insert(thread_id.to_owned(), Arc::downgrade(&gate));
                gate
            })
    }

    fn is_reserved(&self, thread_id: &str) -> bool {
        self.threads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(thread_id)
            .and_then(Weak::upgrade)
            .is_some_and(|gate| gate.try_lock().is_err())
    }
}

#[derive(Clone, Default)]
struct WorkspaceOwnershipFence {
    keys: Arc<StdMutex<HashMap<String, Weak<WorkspaceOwnershipKey>>>>,
    #[cfg(test)]
    contenders: Arc<StdMutex<HashMap<String, Arc<WorkspaceOwnershipContender>>>>,
}

struct WorkspaceOwnershipKey {
    gate: Arc<AsyncMutex<()>>,
    generation: AtomicU64,
    last_removal_generation: AtomicU64,
}

#[cfg(test)]
struct WorkspaceOwnershipContender {
    attempts: AtomicUsize,
    changed: tokio::sync::Notify,
}

struct WorkspaceOwnershipLeaseInner {
    keys: Vec<(String, Arc<WorkspaceOwnershipKey>)>,
    _guards: Vec<OwnedMutexGuard<()>>,
    kind: WorkspaceOwnershipLeaseKind,
    committed: AtomicBool,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum WorkspaceOwnershipLeaseKind {
    Mutation,
    Removal,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum WorkspaceOwnershipAcquireError {
    Revalidate,
    Changed,
}

#[derive(Clone)]
pub(crate) struct WorkspaceOwnershipLease {
    inner: Arc<WorkspaceOwnershipLeaseInner>,
}

impl WorkspaceOwnershipFence {
    async fn acquire(
        &self,
        mut keys: Vec<String>,
        kind: WorkspaceOwnershipLeaseKind,
    ) -> Result<WorkspaceOwnershipLease, WorkspaceOwnershipAcquireError> {
        keys.sort();
        keys.dedup();
        let states = {
            let mut registry = self
                .keys
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.retain(|_, state| state.strong_count() > 0);
            keys.iter()
                .map(|key| {
                    let state = registry
                        .get(key)
                        .and_then(Weak::upgrade)
                        .unwrap_or_else(|| {
                            let state = Arc::new(WorkspaceOwnershipKey {
                                gate: Arc::new(AsyncMutex::new(())),
                                generation: AtomicU64::new(0),
                                last_removal_generation: AtomicU64::new(0),
                            });
                            registry.insert(key.clone(), Arc::downgrade(&state));
                            state
                        });
                    (key.clone(), state)
                })
                .collect::<Vec<_>>()
        };
        let expected = states
            .iter()
            .map(|(_, state)| state.generation.load(Ordering::SeqCst))
            .collect::<Vec<_>>();
        #[cfg(test)]
        for (key, _) in &states {
            self.note_contender_attempt(key);
        }
        let mut guards = Vec::with_capacity(states.len());
        for (_, state) in &states {
            guards.push(state.gate.clone().lock_owned().await);
        }
        let changed = states
            .iter()
            .zip(&expected)
            .filter(|((_, state), expected)| state.generation.load(Ordering::SeqCst) != **expected)
            .collect::<Vec<_>>();
        if !changed.is_empty() {
            let invalidated_by_removal = changed.iter().any(|((_, state), expected)| {
                state.last_removal_generation.load(Ordering::SeqCst) > **expected
            });
            drop(guards);
            if kind == WorkspaceOwnershipLeaseKind::Removal || invalidated_by_removal {
                return Err(WorkspaceOwnershipAcquireError::Changed);
            }
            return Err(WorkspaceOwnershipAcquireError::Revalidate);
        }
        Ok(WorkspaceOwnershipLease {
            inner: Arc::new(WorkspaceOwnershipLeaseInner {
                keys: states,
                _guards: guards,
                kind,
                committed: AtomicBool::new(false),
            }),
        })
    }

    #[cfg(test)]
    async fn contender_checkpoint(&self, path: &Path) -> (String, usize) {
        let key = canonical_worktree_path_key(path)
            .await
            .expect("test workspace identity");
        let attempts = self.contender_for(&key).attempts.load(Ordering::SeqCst);
        (key, attempts)
    }

    #[cfg(test)]
    async fn wait_for_contender_after(&self, checkpoint: &(String, usize)) {
        let (key, attempts) = checkpoint;
        let contender = self.contender_for(key);
        loop {
            let changed = contender.changed.notified();
            if contender.attempts.load(Ordering::SeqCst) > *attempts {
                return;
            }
            changed.await;
        }
    }

    #[cfg(test)]
    fn contender_for(&self, key: &str) -> Arc<WorkspaceOwnershipContender> {
        self.contenders
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(key.to_owned())
            .or_insert_with(|| {
                Arc::new(WorkspaceOwnershipContender {
                    attempts: AtomicUsize::new(0),
                    changed: tokio::sync::Notify::new(),
                })
            })
            .clone()
    }

    #[cfg(test)]
    fn note_contender_attempt(&self, key: &str) {
        let contender = self.contender_for(key);
        contender.attempts.fetch_add(1, Ordering::SeqCst);
        contender.changed.notify_waiters();
    }
}

impl WorkspaceOwnershipLease {
    fn covers(&self, keys: &[String]) -> bool {
        keys.iter()
            .all(|key| self.inner.keys.iter().any(|(held, _)| held == key))
    }

    fn commit(&self) {
        if self
            .inner
            .committed
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            for (_, state) in &self.inner.keys {
                let generation = state.generation.fetch_add(1, Ordering::SeqCst) + 1;
                if self.inner.kind == WorkspaceOwnershipLeaseKind::Removal {
                    state
                        .last_removal_generation
                        .store(generation, Ordering::SeqCst);
                }
            }
        }
    }
}

pub(crate) struct CommandLifetimeGuard {
    cancellation: CancellationToken,
    commit_fence: CommitFence,
    _resource: Box<dyn Send + 'static>,
}

impl CommandLifetimeGuard {
    pub(crate) fn new(
        resource: impl Send + 'static,
        cancellation: CancellationToken,
        commit_fence: CommitFence,
    ) -> Self {
        Self {
            cancellation,
            commit_fence,
            _resource: Box::new(resource),
        }
    }
}

struct DeliveryTransitionEnvelope {
    transition: TurnDeliveryTransition,
    reason: Option<TurnDeliveryFailureReason>,
    response: oneshot::Sender<Result<bool, OrchestrationError>>,
}

enum WorkerEnvelope {
    Command(Box<CommandEnvelope>),
    DeliveryTransition(DeliveryTransitionEnvelope),
}

#[derive(Clone)]
pub struct OrchestrationEngine {
    repositories: Repositories,
    sender: mpsc::Sender<WorkerEnvelope>,
    events: broadcast::Sender<OrchestrationEvent>,
    shutdown: CancellationToken,
    worker: Arc<tokio::sync::Mutex<Option<JoinHandle<()>>>>,
    bootstrap_effects: Arc<StdMutex<Option<Arc<dyn ThreadTurnBootstrapEffects>>>>,
    turn_delivery_wake: Arc<StdMutex<Option<Arc<Notify>>>>,
    project_command_effects: Arc<StdMutex<Option<Arc<dyn ProjectCommandEffects>>>>,
    command_admission: CommandAdmissionFence,
    workspace_ownership: WorkspaceOwnershipFence,
    thread_turns: ThreadTurnFence,
    #[cfg(test)]
    test_hooks: TestHooks,
}

impl OrchestrationEngine {
    pub async fn start(
        database: Database,
        options: EngineOptions,
    ) -> Result<Self, OrchestrationError> {
        let repositories = Repositories::new(database);
        bootstrap_projectors(&repositories, &options.test_hooks).await?;
        rebuild_all_thread_derived_fields(repositories.database()).await?;
        let initial_model = load_command_model(&repositories).await?;
        let (sender, receiver) = mpsc::channel(options.queue_capacity.max(1));
        let (events, _) = broadcast::channel(128);
        let shutdown = CancellationToken::new();
        let project_command_effects = Arc::new(StdMutex::new(None));
        let turn_delivery_wake = Arc::new(StdMutex::new(None));
        let worker = spawn_worker(
            repositories.clone(),
            initial_model,
            receiver,
            events.clone(),
            shutdown.clone(),
            options.test_hooks.clone(),
            project_command_effects.clone(),
            turn_delivery_wake.clone(),
        );
        Ok(Self {
            repositories,
            sender,
            events,
            shutdown,
            worker: Arc::new(tokio::sync::Mutex::new(Some(worker))),
            bootstrap_effects: Arc::new(StdMutex::new(None)),
            turn_delivery_wake,
            project_command_effects,
            command_admission: CommandAdmissionFence::default(),
            workspace_ownership: WorkspaceOwnershipFence::default(),
            thread_turns: ThreadTurnFence::default(),
            #[cfg(test)]
            test_hooks: options.test_hooks,
        })
    }

    pub async fn dispatch(
        &self,
        command: OrchestrationCommand,
    ) -> Result<DispatchResult, OrchestrationError> {
        self.dispatch_inner(command, None, None, None, None, None)
            .await
    }

    pub(crate) async fn acquire_command_admission(
        &self,
        command_id: &str,
    ) -> Result<CommandAdmissionClaim, OrchestrationError> {
        tokio::select! {
            biased;
            () = self.shutdown.cancelled() => Err(OrchestrationError::Cancelled),
            claim = self.command_admission.acquire(command_id) => Ok(claim),
        }
    }

    pub(crate) async fn get_command_receipt_with_claim_cancellation(
        &self,
        claim: &CommandAdmissionClaim,
        command_id: &str,
        cancellation: &CancellationToken,
    ) -> Result<Option<CommandReceipt>, OrchestrationError> {
        claim.ensure_owns(command_id)?;
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(OrchestrationError::Cancelled),
            receipt = self.repositories.get_command_receipt(command_id.to_owned()) => {
                receipt.map_err(wrap_persistence)
            }
        }
    }

    pub(crate) async fn verify_legacy_worktree_policy_replay(
        &self,
        claim: &CommandAdmissionClaim,
        receipt: &CommandReceipt,
        command_id: &str,
        project_id: &str,
        expected_policy: &Value,
    ) -> Result<(), OrchestrationError> {
        claim.ensure_owns(command_id)?;
        if receipt.payload_digest.is_some() {
            return Err(OrchestrationError::CommandConflict {
                command_id: command_id.to_owned(),
            });
        }
        if receipt.status != "accepted" {
            return Err(OrchestrationError::PreviouslyRejected {
                command_id: command_id.to_owned(),
                detail: receipt
                    .error
                    .clone()
                    .unwrap_or_else(|| "Previously rejected.".to_owned()),
            });
        }
        if receipt.aggregate_kind != "project" || receipt.aggregate_id != project_id {
            return Err(OrchestrationError::CommandConflict {
                command_id: command_id.to_owned(),
            });
        }
        let from_sequence_exclusive = receipt.result_sequence.saturating_sub(1).max(0);
        let events = self
            .repositories
            .read_events_from_sequence(from_sequence_exclusive, 1)
            .await
            .map_err(wrap_persistence)?;
        let [event] = events.as_slice() else {
            return Err(OrchestrationError::CommandConflict {
                command_id: command_id.to_owned(),
            });
        };
        let compatible_payload = event.event.payload.as_object().is_some_and(|payload| {
            payload.len() == 3
                && payload.get("projectId").and_then(Value::as_str) == Some(project_id)
                && payload.get("updatedAt").is_some_and(Value::is_string)
                && payload.get("worktreeDiscovery") == Some(expected_policy)
        });
        if event.sequence != receipt.result_sequence
            || event.event.command_id.as_deref() != Some(command_id)
            || event.event.event_type != "project.meta-updated"
            || event.event.aggregate_kind != "project"
            || event.event.aggregate_id != project_id
            || !event
                .event
                .metadata
                .as_object()
                .is_some_and(serde_json::Map::is_empty)
            || !compatible_payload
        {
            return Err(OrchestrationError::CommandConflict {
                command_id: command_id.to_owned(),
            });
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn command_admission_registry_len_for_test(&self) -> usize {
        self.command_admission.retained_key_count()
    }

    pub(crate) async fn replay_admitted_worktree_adoption(
        &self,
        claim: &CommandAdmissionClaim,
        command_id: &str,
        payload_digest: &str,
    ) -> Result<Option<DispatchResult>, OrchestrationError> {
        claim.ensure_owns(command_id)?;
        let Some(receipt) = self
            .repositories
            .get_command_receipt(command_id.to_owned())
            .await
            .map_err(wrap_persistence)?
        else {
            return Ok(None);
        };
        if receipt.payload_digest.as_deref() != Some(payload_digest) {
            return Err(OrchestrationError::CommandConflict {
                command_id: command_id.to_owned(),
            });
        }
        if receipt.status != "accepted" {
            return Err(OrchestrationError::PreviouslyRejected {
                command_id: command_id.to_owned(),
                detail: receipt
                    .error
                    .unwrap_or_else(|| "Previously rejected.".to_owned()),
            });
        }
        let (thread_id, disposition) = durable_replayed_worktree_adoption_result(
            &self.repositories,
            command_id,
            receipt.result_sequence,
        )
        .await?;
        Ok(Some(DispatchResult {
            sequence: receipt.result_sequence,
            thread_id: Some(thread_id),
            project_id: None,
            disposition: Some(disposition),
        }))
    }

    pub(crate) async fn replay_admitted_worktree_removal(
        &self,
        claim: &CommandAdmissionClaim,
        command_id: &str,
        payload_digest: &str,
    ) -> Result<Option<DurableWorktreeRemovalResult>, OrchestrationError> {
        claim.ensure_owns(command_id)?;
        let Some(receipt) = self
            .repositories
            .get_command_receipt(command_id.to_owned())
            .await
            .map_err(wrap_persistence)?
        else {
            return Ok(None);
        };
        if receipt.payload_digest.as_deref() != Some(payload_digest) {
            return Err(OrchestrationError::CommandConflict {
                command_id: command_id.to_owned(),
            });
        }
        if matches!(receipt.status.as_str(), "reserved" | "prepared") {
            return Ok(None);
        }
        if receipt.status != "accepted" {
            return Err(OrchestrationError::PreviouslyRejected {
                command_id: command_id.to_owned(),
                detail: receipt
                    .error
                    .unwrap_or_else(|| "Previously rejected.".to_owned()),
            });
        }
        let from_sequence_exclusive = receipt.result_sequence.saturating_sub(1_024).max(0);
        let events = self
            .repositories
            .read_events_from_sequence(from_sequence_exclusive, 1_024)
            .await
            .map_err(wrap_persistence)?
            .into_iter()
            .filter(|event| event.event.command_id.as_deref() == Some(command_id))
            .collect::<VecDeque<_>>();
        durable_worktree_removal_result(&events).map(Some)
    }

    pub(crate) async fn reserve_generic_command_admission(
        &self,
        claim: &CommandAdmissionClaim,
        command: &OrchestrationCommand,
        payload_digest: &str,
    ) -> Result<bool, OrchestrationError> {
        let command_id = command.command_id().to_owned();
        claim.ensure_owns(&command_id)?;
        let aggregate = command.aggregate_ref();
        let accepted_at = current_timestamp(self.repositories.database()).await?;
        let result_sequence = current_max_sequence(&self.repositories).await?;
        let (receipt, _) = self
            .repositories
            .reserve_command_receipt(CommandReceipt {
                command_id: command_id.clone(),
                aggregate_kind: aggregate.0.to_owned(),
                aggregate_id: aggregate.1.to_owned(),
                accepted_at,
                result_sequence,
                status: "reserved".to_owned(),
                error: None,
                payload_digest: Some(payload_digest.to_owned()),
            })
            .await
            .map_err(wrap_persistence)?;
        if receipt.payload_digest.as_deref() != Some(payload_digest)
            || receipt.aggregate_kind != aggregate.0
            || receipt.aggregate_id != aggregate.1
        {
            return Err(OrchestrationError::CommandConflict { command_id });
        }
        match receipt.status.as_str() {
            "reserved" => Ok(true),
            "accepted" => Ok(false),
            _ => Err(OrchestrationError::PreviouslyRejected {
                command_id,
                detail: receipt
                    .error
                    .unwrap_or_else(|| "Previously rejected.".to_owned()),
            }),
        }
    }

    pub(crate) async fn release_generic_command_admission(
        &self,
        claim: &CommandAdmissionClaim,
        command: &OrchestrationCommand,
        payload_digest: &str,
    ) -> Result<bool, OrchestrationError> {
        claim.ensure_owns(command.command_id())?;
        let aggregate = command.aggregate_ref();
        self.repositories
            .release_reserved_command_receipt(
                command.command_id().to_owned(),
                aggregate.0.to_owned(),
                aggregate.1.to_owned(),
                payload_digest.to_owned(),
            )
            .await
            .map_err(wrap_persistence)
    }

    pub(crate) async fn reserve_worktree_removal_admission(
        &self,
        claim: &CommandAdmissionClaim,
        command_id: &str,
        project_id: &str,
        payload_digest: &str,
    ) -> Result<(Option<DurableWorktreeRemovalResult>, bool), OrchestrationError> {
        claim.ensure_owns(command_id)?;
        let accepted_at = current_timestamp(self.repositories.database()).await?;
        let result_sequence = current_max_sequence(&self.repositories).await?;
        let (receipt, inserted) = self
            .repositories
            .reserve_command_receipt(CommandReceipt {
                command_id: command_id.to_owned(),
                aggregate_kind: "project".to_owned(),
                aggregate_id: project_id.to_owned(),
                accepted_at,
                result_sequence,
                status: "reserved".to_owned(),
                error: None,
                payload_digest: Some(payload_digest.to_owned()),
            })
            .await
            .map_err(wrap_persistence)?;
        if receipt.payload_digest.as_deref() != Some(payload_digest)
            || receipt.aggregate_kind != "project"
            || receipt.aggregate_id != project_id
        {
            return Err(OrchestrationError::CommandConflict {
                command_id: command_id.to_owned(),
            });
        }
        let prepared_retry = !inserted && receipt.status == "prepared";
        let replay = self
            .replay_admitted_worktree_removal(claim, command_id, payload_digest)
            .await?;
        Ok((replay, prepared_retry))
    }

    pub(crate) async fn prepare_worktree_removal_admission(
        &self,
        claim: &CommandAdmissionClaim,
        command_id: &str,
        project_id: &str,
        payload_digest: &str,
    ) -> Result<(), OrchestrationError> {
        claim.ensure_owns(command_id)?;
        let receipt = self
            .repositories
            .prepare_reserved_command_receipt(command_id.to_owned(), payload_digest.to_owned())
            .await
            .map_err(wrap_persistence)?
            .ok_or_else(|| OrchestrationError::CommandConflict {
                command_id: command_id.to_owned(),
            })?;
        if receipt.payload_digest.as_deref() != Some(payload_digest)
            || receipt.aggregate_kind != "project"
            || receipt.aggregate_id != project_id
        {
            return Err(OrchestrationError::CommandConflict {
                command_id: command_id.to_owned(),
            });
        }
        if matches!(receipt.status.as_str(), "prepared" | "accepted") {
            Ok(())
        } else {
            Err(OrchestrationError::PreviouslyRejected {
                command_id: command_id.to_owned(),
                detail: receipt
                    .error
                    .unwrap_or_else(|| "Previously rejected.".to_owned()),
            })
        }
    }

    pub(crate) async fn verify_prepared_worktree_removal_admission(
        &self,
        claim: &CommandAdmissionClaim,
        command_id: &str,
        project_id: &str,
        payload_digest: &str,
    ) -> Result<(), OrchestrationError> {
        claim.ensure_owns(command_id)?;
        if self
            .repositories
            .verify_prepared_command_receipt(
                command_id.to_owned(),
                "project".to_owned(),
                project_id.to_owned(),
                payload_digest.to_owned(),
            )
            .await
            .map_err(wrap_persistence)?
        {
            Ok(())
        } else {
            Err(OrchestrationError::CommandConflict {
                command_id: command_id.to_owned(),
            })
        }
    }

    #[cfg(test)]
    pub(crate) async fn dispatch_with_commit(
        &self,
        command: OrchestrationCommand,
        on_commit: impl FnOnce() + Send + 'static,
    ) -> Result<DispatchResult, OrchestrationError> {
        self.dispatch_inner(command, None, None, None, None, Some(Box::new(on_commit)))
            .await
    }

    #[cfg(test)]
    pub(crate) async fn dispatch_with_admission(
        &self,
        command: OrchestrationCommand,
        admission: CommandAdmission,
        on_commit: impl FnOnce() + Send + 'static,
    ) -> Result<DispatchResult, OrchestrationError> {
        self.dispatch_inner(
            command,
            Some(admission),
            None,
            None,
            None,
            Some(Box::new(on_commit)),
        )
        .await
    }

    pub(crate) async fn dispatch_with_admission_and_command_claim(
        &self,
        command: OrchestrationCommand,
        admission: CommandAdmission,
        command_claim: CommandAdmissionClaim,
        on_commit: impl FnOnce() + Send + 'static,
    ) -> Result<DispatchResult, OrchestrationError> {
        self.dispatch_inner(
            command,
            Some(admission),
            None,
            Some(command_claim),
            None,
            Some(Box::new(on_commit)),
        )
        .await
    }

    pub(crate) async fn dispatch_with_admission_and_command_claim_until_handoff(
        &self,
        command: OrchestrationCommand,
        admission: CommandAdmission,
        command_claim: CommandAdmissionClaim,
        cancellation: &CancellationToken,
    ) -> Result<DispatchResult, OrchestrationError> {
        self.dispatch_plain_with_commit(
            command,
            Some(admission),
            None,
            Some(command_claim),
            None,
            CommandDispatchOptions {
                on_commit: None,
                handoff_cancellation: Some(cancellation),
            },
        )
        .await
    }

    pub(crate) async fn dispatch_with_command_claim(
        &self,
        command: OrchestrationCommand,
        command_claim: CommandAdmissionClaim,
    ) -> Result<DispatchResult, OrchestrationError> {
        self.dispatch_inner(command, None, None, Some(command_claim), None, None)
            .await
    }

    pub(crate) async fn dispatch_with_command_claim_until_handoff(
        &self,
        command: OrchestrationCommand,
        command_claim: CommandAdmissionClaim,
        cancellation: &CancellationToken,
    ) -> Result<DispatchResult, OrchestrationError> {
        self.dispatch_plain_with_commit(
            command,
            None,
            None,
            Some(command_claim),
            None,
            CommandDispatchOptions {
                on_commit: None,
                handoff_cancellation: Some(cancellation),
            },
        )
        .await
    }

    pub(crate) async fn dispatch_with_admission_ownership_and_command_claim_until_handoff(
        &self,
        command: OrchestrationCommand,
        admission: CommandAdmission,
        command_claim: CommandAdmissionClaim,
        ownership: WorkspaceOwnershipLease,
        cancellation: &CancellationToken,
    ) -> Result<DispatchResult, OrchestrationError> {
        self.dispatch_plain_with_commit(
            command,
            Some(admission),
            None,
            Some(command_claim),
            Some(ownership),
            CommandDispatchOptions {
                on_commit: None,
                handoff_cancellation: Some(cancellation),
            },
        )
        .await
    }

    pub(crate) async fn dispatch_with_admission_lifetime_and_command_claim(
        &self,
        command: OrchestrationCommand,
        admission: CommandAdmission,
        lifetime: CommandLifetimeGuard,
        command_claim: CommandAdmissionClaim,
        on_commit: impl FnOnce() + Send + 'static,
    ) -> Result<DispatchResult, OrchestrationError> {
        self.dispatch_inner(
            command,
            Some(admission),
            Some(lifetime),
            Some(command_claim),
            None,
            Some(Box::new(on_commit)),
        )
        .await
    }

    async fn dispatch_inner(
        &self,
        command: OrchestrationCommand,
        admission: Option<CommandAdmission>,
        lifetime: Option<CommandLifetimeGuard>,
        command_claim: Option<CommandAdmissionClaim>,
        ownership: Option<WorkspaceOwnershipLease>,
        on_commit: Option<Box<dyn FnOnce() + Send + 'static>>,
    ) -> Result<DispatchResult, OrchestrationError> {
        self.dispatch_plain_with_commit(
            command,
            admission,
            lifetime,
            command_claim,
            ownership,
            CommandDispatchOptions {
                on_commit,
                handoff_cancellation: None,
            },
        )
        .await
    }

    async fn dispatch_plain(
        &self,
        command: OrchestrationCommand,
    ) -> Result<DispatchResult, OrchestrationError> {
        self.dispatch_plain_with_commit(
            command,
            None,
            None,
            None,
            None,
            CommandDispatchOptions {
                on_commit: None,
                handoff_cancellation: None,
            },
        )
        .await
    }

    async fn dispatch_plain_with_commit(
        &self,
        command: OrchestrationCommand,
        admission: Option<CommandAdmission>,
        lifetime: Option<CommandLifetimeGuard>,
        command_claim: Option<CommandAdmissionClaim>,
        ownership: Option<WorkspaceOwnershipLease>,
        options: CommandDispatchOptions<'_>,
    ) -> Result<DispatchResult, OrchestrationError> {
        let CommandDispatchOptions {
            on_commit,
            handoff_cancellation,
        } = options;
        if self.shutdown.is_cancelled() {
            return Err(OrchestrationError::Cancelled);
        }
        let command_claim = if let Some(command_claim) = command_claim {
            command_claim.ensure_owns(command.command_id())?;
            command_claim
        } else {
            self.acquire_command_admission(command.command_id()).await?
        };
        let ownership = if let Some(ownership) = ownership {
            let required_ownership_keys =
                command_workspace_ownership_keys(&self.repositories, &command).await?;
            if !ownership.covers(&required_ownership_keys) {
                return Err(OrchestrationError::WorkspaceOwnershipChanged {
                    command_id: command.command_id().to_owned(),
                });
            }
            Some(ownership)
        } else {
            loop {
                let required_ownership_keys =
                    command_workspace_ownership_keys(&self.repositories, &command).await?;
                if required_ownership_keys.is_empty() {
                    break None;
                }
                match self
                    .workspace_ownership
                    .acquire(
                        required_ownership_keys,
                        WorkspaceOwnershipLeaseKind::Mutation,
                    )
                    .await
                {
                    Ok(ownership) => break Some(ownership),
                    Err(WorkspaceOwnershipAcquireError::Revalidate) => continue,
                    Err(WorkspaceOwnershipAcquireError::Changed) => {
                        return Err(OrchestrationError::WorkspaceOwnershipChanged {
                            command_id: command.command_id().to_owned(),
                        });
                    }
                }
            }
        };
        let (response_tx, response_rx) = oneshot::channel();
        let send = self
            .sender
            .send(WorkerEnvelope::Command(Box::new(CommandEnvelope {
                command,
                admission,
                lifetime,
                command_claim,
                ownership,
                response: response_tx,
                on_commit,
            })));
        if let Some(cancellation) = handoff_cancellation {
            tokio::select! {
                biased;
                () = cancellation.cancelled() => return Err(OrchestrationError::Cancelled),
                result = send => result,
            }
        } else {
            send.await
        }
        .map_err(|_| OrchestrationError::WorkerClosed)?;
        response_rx
            .await
            .map_err(|_| OrchestrationError::ResponseDropped)?
    }

    pub(crate) fn bootstrap_effects(&self) -> Option<Arc<dyn ThreadTurnBootstrapEffects>> {
        self.bootstrap_effects
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn set_bootstrap_effects(&self, effects: Arc<dyn ThreadTurnBootstrapEffects>) {
        *self
            .bootstrap_effects
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(effects);
    }

    pub(crate) fn set_turn_delivery_waker(&self, wake: Arc<Notify>) {
        *self
            .turn_delivery_wake
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(wake);
    }

    /// Waits until no revert or turn delivery of the thread runs, then holds the thread until the
    /// reservation drops. See `ThreadTurnFence`.
    pub(crate) async fn reserve_thread_turns(&self, thread_id: &str) -> ThreadTurnReservation {
        self.thread_turns.reserve(thread_id).await
    }

    /// Whether a revert or turn delivery of the thread runs now.
    pub(crate) fn thread_turns_reserved(&self, thread_id: &str) -> bool {
        self.thread_turns.is_reserved(thread_id)
    }

    pub(crate) fn wake_turn_delivery(&self) {
        if let Some(wake) = self
            .turn_delivery_wake
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
        {
            wake.notify_one();
        }
    }

    pub fn set_project_command_effects(&self, effects: Arc<dyn ProjectCommandEffects>) {
        *self
            .project_command_effects
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(effects);
    }

    #[cfg(test)]
    pub(crate) fn test_hooks(&self) -> TestHooks {
        self.test_hooks.clone()
    }

    pub(crate) async fn run_bootstrap_setup(
        &self,
        bootstrap_command_id: &str,
        thread_id: &str,
        project_id: Option<String>,
        project_cwd: Option<String>,
        worktree_path: String,
        created_at: String,
    ) -> Result<bool, OrchestrationError> {
        let result = match self.bootstrap_effects() {
            Some(effects) => {
                effects
                    .run_setup_script(BootstrapSetupInput {
                        thread_id: thread_id.to_owned(),
                        project_id,
                        project_cwd,
                        worktree_path: worktree_path.clone(),
                    })
                    .await
            }
            None => Err("production bootstrap effects are not registered".to_owned()),
        };
        match result {
            Ok(BootstrapSetupResult::NoScript) => Ok(false),
            Ok(BootstrapSetupResult::Started {
                script_id,
                script_name,
                terminal_id,
            }) => {
                let payload = json!({"scriptId":script_id,"scriptName":script_name,"terminalId":terminal_id,"worktreePath":worktree_path});
                for (kind, summary, created_at) in [
                    (
                        "setup-script.requested",
                        "Starting setup script",
                        created_at.clone(),
                    ),
                    ("setup-script.started", "Setup script started", created_at),
                ] {
                    self.append_bootstrap_activity(
                        bootstrap_command_id,
                        thread_id,
                        "info",
                        kind,
                        summary,
                        payload.clone(),
                        created_at,
                    )
                    .await?;
                }
                Ok(true)
            }
            Err(detail) => {
                self.append_bootstrap_activity(
                    bootstrap_command_id,
                    thread_id,
                    "error",
                    "setup-script.failed",
                    "Setup script failed to start",
                    json!({"detail":detail,"worktreePath":worktree_path}),
                    created_at,
                )
                .await?;
                Err(OrchestrationError::Bootstrap {
                    stage: "setup script launch",
                    detail,
                })
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn append_bootstrap_activity(
        &self,
        bootstrap_command_id: &str,
        thread_id: &str,
        tone: &str,
        kind: &str,
        summary: &str,
        payload: Value,
        created_at: String,
    ) -> Result<(), OrchestrationError> {
        self.dispatch_plain(OrchestrationCommand::ThreadActivityAppend {
            command_id: format!("server:bootstrap:{bootstrap_command_id}:activity:{kind}"),
            thread_id: thread_id.to_owned(),
            activity: ActivityInput {
                id: format!("bootstrap:{bootstrap_command_id}:{kind}"),
                tone: tone.to_owned(),
                kind: kind.to_owned(),
                summary: summary.to_owned(),
                payload,
                turn_id: None,
                sequence: None,
                created_at: created_at.clone(),
            },
            created_at,
        })
        .await?;
        Ok(())
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<OrchestrationEvent> {
        self.events.subscribe()
    }

    #[cfg(test)]
    pub(crate) fn event_subscriber_count_for_test(&self) -> usize {
        self.events.receiver_count()
    }

    pub async fn read_events(
        &self,
        from_sequence_exclusive: i64,
    ) -> Result<Vec<OrchestrationEvent>, OrchestrationError> {
        read_all_events(&self.repositories, from_sequence_exclusive)
            .await
            .map_err(Into::into)
    }

    pub fn repositories(&self) -> Repositories {
        self.repositories.clone()
    }

    pub(crate) async fn acquire_workspace_removal_ownership(
        &self,
        path: &Path,
    ) -> Result<WorkspaceOwnershipLease, OrchestrationError> {
        let key = canonical_worktree_path_key(path).await.map_err(|error| {
            OrchestrationError::WorkspaceOwnershipIdentity {
                command_id: "worktree.remove".to_owned(),
                path: path.to_string_lossy().into_owned(),
                detail: error.to_string(),
            }
        })?;
        self.workspace_ownership
            .acquire(vec![key], WorkspaceOwnershipLeaseKind::Removal)
            .await
            .map_err(|_| OrchestrationError::WorkspaceOwnershipChanged {
                command_id: "worktree.remove".to_owned(),
            })
    }

    pub(crate) async fn transition_turn_delivery(
        &self,
        transition: TurnDeliveryTransition,
    ) -> Result<bool, OrchestrationError> {
        self.transition_turn_delivery_with_reason(transition, None)
            .await
    }

    pub(crate) async fn transition_turn_delivery_with_reason(
        &self,
        transition: TurnDeliveryTransition,
        reason: Option<TurnDeliveryFailureReason>,
    ) -> Result<bool, OrchestrationError> {
        if self.shutdown.is_cancelled() {
            return Err(OrchestrationError::Cancelled);
        }
        let (response, receive) = oneshot::channel();
        self.sender
            .send(WorkerEnvelope::DeliveryTransition(
                DeliveryTransitionEnvelope {
                    transition,
                    reason,
                    response,
                },
            ))
            .await
            .map_err(|_| OrchestrationError::WorkerClosed)?;
        receive
            .await
            .map_err(|_| OrchestrationError::ResponseDropped)?
    }

    pub async fn shutdown(&self) {
        self.shutdown.cancel();
        if let Some(worker) = self.worker.lock().await.take() {
            let _ = worker.await;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_worker(
    repositories: Repositories,
    mut model: CommandModel,
    mut receiver: mpsc::Receiver<WorkerEnvelope>,
    events: broadcast::Sender<OrchestrationEvent>,
    shutdown: CancellationToken,
    hooks: TestHooks,
    project_command_effects: Arc<StdMutex<Option<Arc<dyn ProjectCommandEffects>>>>,
    turn_delivery_wake: Arc<StdMutex<Option<Arc<Notify>>>>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                envelope = receiver.recv() => {
                    let Some(envelope) = envelope else {
                        break;
                    };
                    match envelope {
                        WorkerEnvelope::Command(envelope) => {
                            let CommandEnvelope {
                                command,
                                admission,
                                lifetime,
                                command_claim,
                                ownership,
                                response,
                                on_commit,
                            } = *envelope;
                            let has_admission = admission.is_some();
                            let failed_turn_reservation = admission
                                .as_ref()
                                .and_then(|admission| admission.provider_turn.as_ref().map(|_| {
                                    let aggregate = command.aggregate_ref();
                                    (
                                        command.command_id().to_owned(),
                                        aggregate.0.to_owned(),
                                        aggregate.1.to_owned(),
                                        admission.payload_digest.clone(),
                                    )
                                }));
                            let cancellation = lifetime
                                .as_ref()
                                .map(|lifetime| lifetime.cancellation.clone());
                            let commit_fence = lifetime
                                .as_ref()
                                .map(|lifetime| lifetime.commit_fence.clone());
                            let effects = project_command_effects
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner())
                                .clone();
                            let wake_delivery = match &command {
                                OrchestrationCommand::ThreadSessionSet { session, .. } => session.status == "ready",
                                OrchestrationCommand::ThreadTurnPromote { .. } | OrchestrationCommand::ThreadTurnSteer { .. } | OrchestrationCommand::ThreadTurnDeliveryResolve { .. } => true,
                                // A reopened chat panel's queue becomes promotable again.
                                OrchestrationCommand::ThreadUnarchive { .. } => true,
                                OrchestrationCommand::ThreadActivityAppend { activity, .. } => matches!(activity.kind.as_str(), "approval.resolved" | "user-input.resolved" | "provider.user-input.respond.failed"),
                                _ => false,
                            };
                            let mut result = process_envelope(
                                &repositories,
                                &mut model,
                                &events,
                                &hooks,
                                effects.as_deref(),
                                ProcessEnvelopeInput {
                                    command,
                                    admission,
                                    command_claim: &command_claim,
                                    cancellation: cancellation.as_ref(),
                                    commit_fence,
                                },
                            )
                            .await;
                            if result.is_err()
                                && let Some((command_id, aggregate_kind, aggregate_id, digest)) =
                                    failed_turn_reservation
                            {
                                let cleanup = match command_claim.ensure_owns(&command_id) {
                                    Ok(()) => repositories
                                        .release_reserved_command_receipt(
                                            command_id,
                                            aggregate_kind,
                                            aggregate_id,
                                            digest,
                                        )
                                        .await
                                        .map_err(wrap_persistence),
                                    Err(error) => Err(error),
                                };
                                if let Err(error) = cleanup {
                                    result = Err(error);
                                }
                            }
                            if result.as_ref().is_ok_and(|outcome| outcome.accepted_new)
                            {
                                if wake_delivery && let Some(wake) = turn_delivery_wake.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).as_ref() {
                                    wake.notify_one();
                                }
                                if let Some(ownership) = &ownership {
                                    ownership.commit();
                                }
                                if let Some(on_commit) = on_commit {
                                    on_commit();
                                    if has_admission {
                                        hooks.maybe_pause_after_admission_commit().await;
                                    }
                                }
                            }
                            let _ = response.send(result.map(|outcome| outcome.result));
                            drop(lifetime);
                            drop(ownership);
                            drop(command_claim);
                        }
                        WorkerEnvelope::DeliveryTransition(DeliveryTransitionEnvelope { transition, reason, response }) => {
                            let requeued = transition.next_state == TurnDeliveryState::Queued;
                            let result = persist_turn_delivery_transition(&repositories, &events, &hooks, transition, reason).await;
                            if requeued && matches!(result, Ok(true))
                                && let Some(wake) = turn_delivery_wake.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).as_ref() {
                                wake.notify_one();
                            }
                            let _ = response.send(result);
                        }
                    }
                }
            }
        }
    })
}

struct ProcessEnvelopeInput<'a> {
    command: OrchestrationCommand,
    admission: Option<CommandAdmission>,
    command_claim: &'a CommandAdmissionClaim,
    cancellation: Option<&'a CancellationToken>,
    commit_fence: Option<CommitFence>,
}

async fn process_envelope(
    repositories: &Repositories,
    model: &mut CommandModel,
    events: &broadcast::Sender<OrchestrationEvent>,
    hooks: &TestHooks,
    project_command_effects: Option<&dyn ProjectCommandEffects>,
    input: ProcessEnvelopeInput<'_>,
) -> Result<ProcessEnvelopeOutcome, OrchestrationError> {
    let ProcessEnvelopeInput {
        mut command,
        admission,
        command_claim,
        cancellation,
        commit_fence,
    } = input;
    ensure_command_active(cancellation)?;
    let command_id = command.command_id().to_owned();
    let requested_aggregate = command.aggregate_ref();
    let requested_aggregate = (
        requested_aggregate.0.to_owned(),
        requested_aggregate.1.to_owned(),
    );
    let requested_project_id = match &command {
        OrchestrationCommand::ProjectCreate { project_id, .. } => Some(project_id.clone()),
        _ => None,
    };
    if let Some(receipt) = repositories
        .get_command_receipt(command_id.clone())
        .await
        .map_err(wrap_persistence)?
    {
        if let Some(admission) = &admission
            && receipt.payload_digest.as_deref() != Some(admission.payload_digest.as_str())
        {
            return Err(OrchestrationError::CommandConflict { command_id });
        }
        if receipt.status == "accepted" {
            let replay_adoption =
                replayed_worktree_adoption_result(repositories, &command, receipt.result_sequence)
                    .await?;
            let project_id = requested_project_id.as_ref().map(|requested_id| {
                if receipt.aggregate_kind == "project" {
                    receipt.aggregate_id.clone()
                } else {
                    requested_id.clone()
                }
            });
            return Ok(ProcessEnvelopeOutcome {
                result: DispatchResult {
                    sequence: receipt.result_sequence,
                    thread_id: replay_adoption
                        .as_ref()
                        .map(|result| result.0.clone())
                        .or_else(|| {
                            project_id.as_deref().and_then(|project_id| {
                                canonical_default_thread_id(model, project_id)
                            })
                        }),
                    project_id,
                    disposition: replay_adoption.map(|result| result.1),
                },
                accepted_new: false,
            });
        }
        if matches!(receipt.status.as_str(), "reserved" | "prepared") {
            let requested_digest = admission
                .as_ref()
                .map(|admission| admission.payload_digest.as_str());
            if receipt.payload_digest.as_deref() != requested_digest
                || receipt.aggregate_kind != requested_aggregate.0
                || receipt.aggregate_id != requested_aggregate.1
            {
                return Err(OrchestrationError::CommandConflict { command_id });
            }
        } else {
            return Err(OrchestrationError::PreviouslyRejected {
                command_id,
                detail: receipt
                    .error
                    .unwrap_or_else(|| "Previously rejected.".to_owned()),
            });
        }
    }
    let occurred_at = match command.occurred_at() {
        Some(value) => value.to_owned(),
        None => current_timestamp(repositories.database()).await?,
    };

    if let OrchestrationCommand::ThreadTurnDeliveryResolve {
        command_id,
        thread_id,
        message_id,
        action,
        ..
    } = &command
    {
        let payload_digest = admission
            .as_ref()
            .map(|admission| admission.payload_digest.clone());
        let committed = persist_turn_delivery_resolution(
            repositories,
            events,
            hooks,
            command_id.clone(),
            thread_id.clone(),
            message_id.clone(),
            *action,
            occurred_at.clone(),
            payload_digest.clone(),
        )
        .await?;
        if let Some(saved) = committed {
            return Ok(ProcessEnvelopeOutcome {
                result: DispatchResult {
                    sequence: saved.sequence,
                    thread_id: None,
                    project_id: None,
                    disposition: None,
                },
                accepted_new: true,
            });
        }

        let detail = format!(
            "Message '{message_id}' does not have a cancellable, uncertain, or failed delivery on thread '{thread_id}'."
        );
        repositories
            .finalize_command_receipt(CommandReceipt {
                command_id: command_id.clone(),
                aggregate_kind: "thread".to_owned(),
                aggregate_id: thread_id.clone(),
                accepted_at: occurred_at,
                result_sequence: current_max_sequence(repositories).await.unwrap_or(0),
                status: "rejected".to_owned(),
                error: Some(detail.clone()),
                payload_digest,
            })
            .await
            .map_err(wrap_persistence)?;
        return Err(OrchestrationError::Invariant {
            command_type: command.command_type().to_owned(),
            detail,
        });
    }

    canonicalize_project_command(model, &mut command, project_command_effects).await?;
    let prepared_worktree = canonicalize_worktree_command(&mut command).await?;
    let project_create_identity = match &command {
        OrchestrationCommand::ProjectCreate {
            project_id,
            workspace_root,
            ..
        } => Some((project_id.clone(), workspace_root.clone())),
        _ => None,
    };
    if let Some((requested_id, workspace_root)) = &project_create_identity {
        let existing_project_id = model.projects.iter().find_map(|(project_id, project)| {
            (project_id != requested_id
                && project.deleted_at.is_none()
                && project.workspace_root == *workspace_root)
                .then(|| project_id.clone())
        });
        if let Some(existing_project_id) = existing_project_id {
            let sequence = current_max_sequence(repositories).await?;
            repositories
                .finalize_command_receipt(CommandReceipt {
                    command_id: command_id.clone(),
                    aggregate_kind: "project".to_owned(),
                    aggregate_id: existing_project_id.clone(),
                    accepted_at: occurred_at,
                    result_sequence: sequence,
                    status: "accepted".to_owned(),
                    error: None,
                    payload_digest: admission
                        .as_ref()
                        .map(|admission| admission.payload_digest.clone()),
                })
                .await
                .map_err(wrap_persistence)?;
            return Ok(ProcessEnvelopeOutcome {
                result: DispatchResult {
                    sequence,
                    thread_id: canonical_default_thread_id(model, &existing_project_id),
                    project_id: Some(existing_project_id),
                    disposition: None,
                },
                accepted_new: true,
            });
        }
    }

    ensure_command_active(cancellation)?;
    let projection_mode = if matches!(
        command,
        OrchestrationCommand::ThreadMessageAssistantComplete { .. }
    ) {
        ProjectionMode::UpdateExistingAssistantMessage
    } else {
        ProjectionMode::Legacy
    };
    let mut planned = match plan_command(
        repositories,
        model,
        &command,
        &prepared_worktree,
        &occurred_at,
    )
    .await
    {
        Ok(planned) => planned,
        Err(error) => {
            ensure_command_active(cancellation)?;
            let aggregate = command.aggregate_ref();
            persist_rejected_command(
                repositories,
                hooks,
                CommandReceipt {
                    command_id: command.command_id().to_owned(),
                    aggregate_kind: aggregate.0.to_owned(),
                    aggregate_id: aggregate.1.to_owned(),
                    accepted_at: occurred_at,
                    result_sequence: current_max_sequence(repositories).await.unwrap_or(0),
                    status: "rejected".to_owned(),
                    error: Some(error.to_string()),
                    payload_digest: admission
                        .as_ref()
                        .map(|admission| admission.payload_digest.clone()),
                },
                commit_fence,
            )
            .await?;
            return Err(error);
        }
    };

    if let Some(turn) = admission
        .as_ref()
        .and_then(|value| value.provider_turn.as_ref())
    {
        planned.push(make_event(
            "thread.turn-delivery-updated",
            "thread",
            &turn.thread_id,
            &turn.created_at,
            &turn.command_id,
            json!({}),
            json!({
                "threadId": turn.thread_id,
                "messageId": turn.message_id,
                "state": turn_delivery_state_name(turn.state),
                "provider": turn.provider_kind,
                "delivery": delivery_value(turn_delivery_state_name(turn.state), &turn.provider_kind, &turn.provider_instance_id, turn.mode.as_str(), false, None),
                "detail": null,
                "updatedAt": turn.created_at,
            }),
        ));
    }

    reserve_project_create_side_effect(
        repositories,
        command_claim,
        &command,
        admission.as_ref(),
        &occurred_at,
        project_command_effects,
    )
    .await?;
    prepare_project_create(&command, project_command_effects).await?;
    hooks.maybe_pause_before_command_persist().await;
    ensure_command_active(cancellation)?;
    let aggregate = command.aggregate_ref();
    let persisted = persist_command(
        repositories,
        hooks,
        &command,
        &planned,
        &command_id,
        aggregate,
        admission,
        commit_fence,
        projection_mode,
    )
    .await?;
    let adoption_result = worktree_adoption_result(&command, &persisted.committed)?;
    apply_to_model(model, &persisted.committed);
    apply_prepared_worktree_identity(
        model,
        &command,
        &prepared_worktree,
        adoption_result.as_ref(),
    );
    for event in &persisted.committed {
        let _ = events.send(event.clone());
    }
    let project_id = project_create_identity.map(|(project_id, _)| project_id);
    Ok(ProcessEnvelopeOutcome {
        result: DispatchResult {
            sequence: persisted.result_sequence,
            thread_id: adoption_result
                .as_ref()
                .map(|result| result.0.clone())
                .or_else(|| {
                    project_id
                        .as_deref()
                        .and_then(|project_id| canonical_default_thread_id(model, project_id))
                }),
            project_id,
            disposition: adoption_result.map(|result| result.1),
        },
        accepted_new: true,
    })
}

fn ensure_command_active(
    cancellation: Option<&CancellationToken>,
) -> Result<(), OrchestrationError> {
    if cancellation.is_some_and(CancellationToken::is_cancelled) {
        Err(OrchestrationError::Cancelled)
    } else {
        Ok(())
    }
}

struct ProcessEnvelopeOutcome {
    result: DispatchResult,
    accepted_new: bool,
}

async fn canonicalize_project_command(
    model: &mut CommandModel,
    command: &mut OrchestrationCommand,
    effects: Option<&dyn ProjectCommandEffects>,
) -> Result<(), OrchestrationError> {
    canonicalize_project_command_with_historical_timeout(
        model,
        command,
        effects,
        HISTORICAL_PROJECT_ROOT_INSPECTION_TIMEOUT,
    )
    .await
}

async fn canonicalize_project_command_with_historical_timeout(
    model: &mut CommandModel,
    command: &mut OrchestrationCommand,
    effects: Option<&dyn ProjectCommandEffects>,
    historical_root_inspection_timeout: Duration,
) -> Result<(), OrchestrationError> {
    let Some(effects) = effects else {
        return Ok(());
    };
    if !matches!(
        command,
        OrchestrationCommand::ProjectCreate { .. }
            | OrchestrationCommand::ProjectMetaUpdate {
                workspace_root: Some(_),
                ..
            }
    ) {
        return Ok(());
    }
    if !model.project_roots_canonicalized {
        let historical_root_inspection_deadline =
            tokio::time::Instant::now() + historical_root_inspection_timeout;
        let cached_roots = model
            .projects
            .iter()
            .filter(|(_, project)| project.deleted_at.is_none())
            .map(|(project_id, project)| (project_id.clone(), project.workspace_root.clone()))
            .collect::<Vec<_>>();
        for (project_id, workspace_root) in cached_roots {
            let lexical_identity = effects.normalize_workspace_root_lexically(&workspace_root);
            let identity = match tokio::time::timeout_at(
                historical_root_inspection_deadline,
                effects.canonicalize_workspace_root(&workspace_root, true),
            )
            .await
            {
                Ok(Ok(canonical)) => canonical,
                Ok(Err(error)) => {
                    tracing::warn!(
                        project_id,
                        workspace_root,
                        fallback_identity = lexical_identity,
                        %error,
                        "historical project workspace root inspection failed; using lexical identity"
                    );
                    lexical_identity
                }
                Err(_) => {
                    tracing::warn!(
                        project_id,
                        workspace_root,
                        fallback_identity = lexical_identity,
                        inspection_timeout_ms =
                            u64::try_from(historical_root_inspection_timeout.as_millis())
                                .unwrap_or(u64::MAX),
                        "historical project workspace root inspection timed out; using lexical identity"
                    );
                    lexical_identity
                }
            };
            if let Some(project) = model.projects.get_mut(&project_id) {
                project.workspace_root = identity;
            }
        }
        model.project_roots_canonicalized = true;
    }
    let workspace_root = match command {
        OrchestrationCommand::ProjectCreate { workspace_root, .. } => Some((workspace_root, true)),
        OrchestrationCommand::ProjectMetaUpdate {
            workspace_root: Some(workspace_root),
            ..
        } => Some((workspace_root, false)),
        _ => None,
    };
    if let Some((workspace_root, allow_missing)) = workspace_root {
        *workspace_root = effects
            .canonicalize_workspace_root(workspace_root, allow_missing)
            .await
            .map_err(|detail| OrchestrationError::ProjectPreparation { detail })?;
    }
    Ok(())
}

#[derive(Default)]
struct PreparedWorktreeCommand {
    path_key: Option<String>,
}

impl PreparedWorktreeCommand {
    fn path_key<'a>(
        &'a self,
        command: &OrchestrationCommand,
    ) -> Result<&'a str, OrchestrationError> {
        self.path_key
            .as_deref()
            .ok_or_else(|| OrchestrationError::Invariant {
                command_type: command.command_type().to_owned(),
                detail: "The worktree command has no prepared physical path identity.".to_owned(),
            })
    }
}

async fn canonicalize_worktree_command(
    command: &mut OrchestrationCommand,
) -> Result<PreparedWorktreeCommand, OrchestrationError> {
    let command_id = command.command_id().to_owned();
    let path_key = match command {
        OrchestrationCommand::WorktreeAdoptResolved { path, .. }
        | OrchestrationCommand::WorktreeDetachResolved { path, .. } => {
            Some(canonicalize_command_worktree_path(&command_id, path).await?)
        }
        OrchestrationCommand::ThreadCreate {
            worktree_path: Some(path),
            ..
        } => Some(canonicalize_command_worktree_path(&command_id, path).await?),
        OrchestrationCommand::ThreadMetaUpdate {
            worktree_path: OptionalNullable::Present(Some(path)),
            ..
        } => Some(canonicalize_command_worktree_path(&command_id, path).await?),
        OrchestrationCommand::ThreadTurnStart {
            bootstrap: Some(bootstrap),
            ..
        } => {
            if let Some(path) = bootstrap
                .create_thread
                .as_mut()
                .and_then(|create| create.worktree_path.as_mut())
            {
                Some(canonicalize_command_worktree_path(&command_id, path).await?)
            } else {
                None
            }
        }
        OrchestrationCommand::ProjectMetaUpdate {
            worktree_discovery: Some(policy),
            ..
        } => {
            canonicalize_policy_baseline(&command_id, policy).await?;
            None
        }
        _ => None,
    };
    Ok(PreparedWorktreeCommand { path_key })
}

async fn canonicalize_command_worktree_path(
    command_id: &str,
    path: &str,
) -> Result<String, OrchestrationError> {
    canonical_worktree_path_key(Path::new(path))
        .await
        .map_err(|error| OrchestrationError::WorkspaceOwnershipIdentity {
            command_id: command_id.to_owned(),
            path: path.to_owned(),
            detail: error.to_string(),
        })
}

async fn canonicalize_policy_baseline(
    command_id: &str,
    policy: &mut Value,
) -> Result<(), OrchestrationError> {
    let Some(paths) = policy
        .as_object_mut()
        .and_then(|policy| policy.get_mut("baselinePaths"))
        .and_then(Value::as_array_mut)
    else {
        return Ok(());
    };
    for path in paths {
        let Some(value) = path.as_str() else {
            continue;
        };
        let _identity_key = canonicalize_command_worktree_path(command_id, value).await?;
    }
    Ok(())
}

fn apply_prepared_worktree_identity(
    model: &mut CommandModel,
    command: &OrchestrationCommand,
    prepared: &PreparedWorktreeCommand,
    adoption_result: Option<&(String, String)>,
) {
    let Some(path_key) = prepared.path_key.as_ref() else {
        return;
    };
    let thread_id = match command {
        OrchestrationCommand::WorktreeAdoptResolved { .. } => {
            adoption_result.map(|(thread_id, _)| thread_id.as_str())
        }
        OrchestrationCommand::ThreadCreate {
            thread_id,
            worktree_path: Some(_),
            ..
        }
        | OrchestrationCommand::ThreadMetaUpdate { thread_id, .. } => Some(thread_id.as_str()),
        OrchestrationCommand::ThreadTurnStart {
            thread_id,
            bootstrap: Some(bootstrap),
            ..
        } if bootstrap
            .create_thread
            .as_ref()
            .is_some_and(|create| create.worktree_path.is_some()) =>
        {
            Some(thread_id.as_str())
        }
        _ => None,
    };
    if let Some(thread) = thread_id.and_then(|thread_id| model.threads.get_mut(thread_id)) {
        thread.worktree_path_key = Some(path_key.clone());
    }
}

async fn prepare_project_create(
    command: &OrchestrationCommand,
    effects: Option<&dyn ProjectCommandEffects>,
) -> Result<(), OrchestrationError> {
    let (
        Some(effects),
        OrchestrationCommand::ProjectCreate {
            workspace_root,
            create_workspace_root_if_missing,
            initialize_git,
            ..
        },
    ) = (effects, command)
    else {
        return Ok(());
    };
    effects
        .prepare_project_create(
            workspace_root,
            create_workspace_root_if_missing.unwrap_or(false),
            initialize_git.unwrap_or(false),
        )
        .await
        .map_err(|detail| OrchestrationError::ProjectPreparation { detail })
}

async fn reserve_project_create_side_effect(
    repositories: &Repositories,
    command_claim: &CommandAdmissionClaim,
    command: &OrchestrationCommand,
    admission: Option<&CommandAdmission>,
    occurred_at: &str,
    effects: Option<&dyn ProjectCommandEffects>,
) -> Result<(), OrchestrationError> {
    let (
        Some(_),
        OrchestrationCommand::ProjectCreate {
            command_id,
            project_id,
            ..
        },
    ) = (effects, command)
    else {
        return Ok(());
    };
    command_claim.ensure_owns(command_id)?;
    let payload_digest = admission.map(|admission| admission.payload_digest.clone());
    let (receipt, _) = repositories
        .reserve_command_receipt(CommandReceipt {
            command_id: command_id.clone(),
            aggregate_kind: "project".to_owned(),
            aggregate_id: project_id.clone(),
            accepted_at: occurred_at.to_owned(),
            result_sequence: current_max_sequence(repositories).await?,
            status: "reserved".to_owned(),
            error: None,
            payload_digest: payload_digest.clone(),
        })
        .await
        .map_err(wrap_persistence)?;
    if receipt.status == "reserved"
        && receipt.aggregate_kind == "project"
        && receipt.aggregate_id == *project_id
        && receipt.payload_digest == payload_digest
    {
        Ok(())
    } else {
        Err(OrchestrationError::CommandConflict {
            command_id: command_id.clone(),
        })
    }
}

async fn plan_command(
    repositories: &Repositories,
    model: &CommandModel,
    command: &OrchestrationCommand,
    prepared_worktree: &PreparedWorktreeCommand,
    occurred_at: &str,
) -> Result<Vec<NewOrchestrationEvent>, OrchestrationError> {
    let metadata = Value::Object(Default::default());
    match command {
        OrchestrationCommand::ProjectCreate {
            command_id,
            project_id,
            title,
            workspace_root,
            default_model_selection,
            created_at,
            ..
        } => {
            if model.projects.contains_key(project_id) {
                return invariant(
                    command,
                    format!("Project '{project_id}' already exists and cannot be created twice."),
                );
            }
            let default_thread_id = Uuid::new_v4().to_string();
            let project_selection = default_model_selection
                .as_ref()
                .and_then(|value| value.cloned())
                .unwrap_or(Value::Null);
            let selection = default_model_selection
                .as_ref()
                .and_then(|value| value.cloned())
                .unwrap_or_else(|| json!({"instanceId":"codex","model":"gpt-5.4"}));
            Ok(vec![
                make_event(
                    "project.created",
                    "project",
                    project_id,
                    created_at,
                    command_id,
                    metadata.clone(),
                    json!({"projectId":project_id,"title":title,"workspaceRoot":workspace_root,"defaultModelSelection":project_selection,"scripts":[],"worktreeDiscovery":default_worktree_discovery(),"createdAt":created_at,"updatedAt":created_at}),
                ),
                make_event(
                    "thread.created",
                    "thread",
                    &default_thread_id,
                    created_at,
                    command_id,
                    metadata,
                    json!({"threadId":default_thread_id,"projectId":project_id,"title":title,"kind":"default","modelSelection":selection,"runtimeMode":"full-access","interactionMode":"default","branch":null,"worktreePath":null,"createdAt":created_at,"updatedAt":created_at}),
                ),
            ])
        }
        OrchestrationCommand::ProjectMetaUpdate {
            command_id,
            project_id,
            title,
            workspace_root,
            default_model_selection,
            scripts,
            worktree_discovery,
        } => {
            require_project(model, command, project_id)?;
            if let Some(workspace_root) = workspace_root
                && let Some(conflicting_project_id) =
                    model.projects.iter().find_map(|(candidate_id, project)| {
                        (candidate_id != project_id
                            && project.deleted_at.is_none()
                            && project.workspace_root == *workspace_root)
                            .then(|| candidate_id.clone())
                    })
            {
                return invariant(
                    command,
                    format!(
                        "Workspace root '{workspace_root}' is already registered by project '{conflicting_project_id}'."
                    ),
                );
            }
            let mut payload = json!({"projectId":project_id,"updatedAt":occurred_at});
            insert_optional(&mut payload, "title", title.as_ref().map(|v| json!(v)));
            insert_optional(
                &mut payload,
                "workspaceRoot",
                workspace_root.as_ref().map(|v| json!(v)),
            );
            insert_optional(
                &mut payload,
                "defaultModelSelection",
                default_model_selection
                    .as_ref()
                    .map(|value| value.cloned().unwrap_or(Value::Null)),
            );
            insert_optional(&mut payload, "scripts", scripts.as_ref().map(|v| json!(v)));
            insert_optional(
                &mut payload,
                "worktreeDiscovery",
                worktree_discovery.as_ref().map(|value| json!(value)),
            );
            Ok(vec![make_event(
                "project.meta-updated",
                "project",
                project_id,
                occurred_at,
                command_id,
                metadata,
                payload,
            )])
        }
        OrchestrationCommand::ProjectRepositoryIdentitySet {
            command_id,
            project_id,
            repository_identity,
        } => {
            require_project(model, command, project_id)?;
            // Keep updatedAt: an identity refresh is not user activity and must not reorder projects.
            let updated_at = repositories
                .get_project(project_id.clone())
                .await
                .map_err(wrap_persistence)?
                .map_or_else(|| occurred_at.to_owned(), |project| project.updated_at);
            Ok(vec![make_event(
                "project.meta-updated",
                "project",
                project_id,
                occurred_at,
                command_id,
                metadata,
                json!({
                    "projectId": project_id,
                    "repositoryIdentity": repository_identity,
                    "updatedAt": updated_at,
                }),
            )])
        }
        OrchestrationCommand::ProjectDelete {
            command_id,
            project_id,
            force,
        } => {
            require_project(model, command, project_id)?;
            let active: Vec<_> = model
                .threads
                .iter()
                .filter(|(_, thread)| {
                    thread.project_id == *project_id && thread.deleted_at.is_none()
                })
                .collect();
            if active
                .iter()
                .any(|(_, thread)| thread.kind == "workspace" && thread.worktree_path.is_some())
            {
                return invariant(
                    command,
                    format!(
                        "Project '{project_id}' contains an adopted worktree owner; detach it through the dedicated server-resolved worktree API before deleting the project."
                    ),
                );
            }
            if force != &Some(true) && active.iter().any(|(_, thread)| thread.kind != "default") {
                return invariant(
                    command,
                    format!(
                        "Project '{project_id}' is not empty and cannot be deleted without force=true."
                    ),
                );
            }
            let mut events: Vec<_> = active
                .into_iter()
                .map(|(thread_id, _)| {
                    make_event(
                        "thread.deleted",
                        "thread",
                        thread_id,
                        occurred_at,
                        command_id,
                        metadata.clone(),
                        json!({"threadId":thread_id,"deletedAt":occurred_at}),
                    )
                })
                .collect();
            events.push(make_event(
                "project.deleted",
                "project",
                project_id,
                occurred_at,
                command_id,
                metadata,
                json!({"projectId":project_id,"deletedAt":occurred_at}),
            ));
            Ok(events)
        }
        OrchestrationCommand::WorktreeAdoptResolved {
            command_id,
            project_id,
            worktree_key,
            path,
            branch,
            head,
            model_selection,
            runtime_mode,
            interaction_mode,
        } => {
            let project = require_project(model, command, project_id)?;
            let path_key = prepared_worktree.path_key(command)?;
            let owners = canonical_worktree_owners(model, project_id, path_key).collect::<Vec<_>>();
            if owners.len() > 1 {
                return Err(OrchestrationError::WorktreeOwnershipConflict {
                    project_id: project_id.clone(),
                    owner_count: owners.len(),
                });
            }
            let mut planned = Vec::with_capacity(2);
            let adoption_result = if let Some((thread_id, thread)) = owners.first() {
                if thread.archived_at.is_some() {
                    planned.push(make_event(
                        "thread.unarchived",
                        "thread",
                        thread_id,
                        occurred_at,
                        command_id,
                        json!({"worktreeKey":worktree_key}),
                        json!({"threadId":thread_id,"updatedAt":occurred_at}),
                    ));
                    ((*thread_id).clone(), "restored")
                } else {
                    ((*thread_id).clone(), "existing")
                }
            } else {
                let thread_id = Uuid::new_v4().to_string();
                let title = resolved_worktree_title(branch.as_deref(), head.as_deref(), path);
                planned.push(make_event(
                    "thread.created",
                    "thread",
                    &thread_id,
                    occurred_at,
                    command_id,
                    json!({"worktreeKey":worktree_key}),
                    json!({
                        "threadId":thread_id,
                        "projectId":project_id,
                        "title":title,
                        "kind":"workspace",
                        "modelSelection":model_selection,
                        "runtimeMode":runtime_mode,
                        "interactionMode":interaction_mode,
                        "branch":branch,
                        "worktreePath":path,
                        "createdAt":occurred_at,
                        "updatedAt":occurred_at
                    }),
                ));
                (thread_id, "created")
            };
            let policy =
                compact_adoption_policy(command, &project.worktree_discovery, path_key).await?;
            planned.push(make_event(
                "project.meta-updated",
                "project",
                project_id,
                occurred_at,
                command_id,
                json!({
                    "worktreeKey":worktree_key,
                    "adoptionResult": {
                        "threadId": adoption_result.0,
                        "disposition": adoption_result.1
                    }
                }),
                json!({
                    "projectId":project_id,
                    "worktreeDiscovery":policy,
                    "updatedAt":occurred_at
                }),
            ));
            Ok(planned)
        }
        OrchestrationCommand::WorktreeDetachResolved {
            command_id,
            project_id,
            thread_id,
            path: _,
            git_outcome,
            detail,
            orphan_cleanup_pending,
        } => {
            let project = require_project(model, command, project_id)?;
            let thread = require_thread(model, command, thread_id)?;
            let path_key = prepared_worktree.path_key(command)?;
            if thread.project_id != *project_id
                || thread.kind != "workspace"
                || thread.worktree_path_key.as_deref() != Some(path_key)
            {
                return invariant(
                    command,
                    format!(
                        "Thread '{thread_id}' is not the resolved workspace owner for project '{project_id}'."
                    ),
                );
            }
            let owners = canonical_worktree_owners(model, project_id, path_key).collect::<Vec<_>>();
            if owners.len() != 1 || owners[0].0.as_str() != thread_id {
                return Err(OrchestrationError::WorktreeOwnershipConflict {
                    project_id: project_id.clone(),
                    owner_count: owners.len(),
                });
            }
            let mut dependent_panels = model
                .threads
                .iter()
                .filter(|(_, candidate)| {
                    candidate.project_id == *project_id
                        && candidate.kind == "panel"
                        && candidate.deleted_at.is_none()
                        && candidate.worktree_path_key.as_deref() == Some(path_key)
                })
                .map(|(panel_id, _)| panel_id)
                .collect::<Vec<_>>();
            dependent_panels.sort();
            let mut planned = dependent_panels
                .into_iter()
                .map(|panel_id| {
                    make_event(
                        "thread.deleted",
                        "thread",
                        panel_id,
                        occurred_at,
                        command_id,
                        metadata.clone(),
                        json!({"threadId":panel_id,"deletedAt":occurred_at}),
                    )
                })
                .collect::<Vec<_>>();
            planned.push(make_event(
                "thread.deleted",
                "thread",
                thread_id,
                occurred_at,
                command_id,
                metadata.clone(),
                json!({"threadId":thread_id,"deletedAt":occurred_at}),
            ));
            let policy =
                compact_detach_policy(command, &project.worktree_discovery, path_key).await?;
            planned.push(make_event(
                "project.meta-updated",
                "project",
                project_id,
                occurred_at,
                command_id,
                json!({
                    "detachedThreadId":thread_id,
                    "removalResult": {
                        "threadRemoved": true,
                        "gitOutcome": git_outcome,
                        "detail": detail,
                        "orphanCleanupPending": orphan_cleanup_pending
                    }
                }),
                json!({
                    "projectId":project_id,
                    "worktreeDiscovery":policy,
                    "updatedAt":occurred_at
                }),
            ));
            Ok(planned)
        }
        OrchestrationCommand::WorktreeBranchReconcileResolved {
            command_id,
            project_id,
            thread_id,
            branch,
        } => {
            require_project(model, command, project_id)?;
            let thread = require_thread(model, command, thread_id)?;
            if thread.project_id != *project_id
                || thread.kind != "workspace"
                || thread.archived_at.is_some()
                || thread.worktree_path.is_none()
            {
                return invariant(
                    command,
                    format!(
                        "Thread '{thread_id}' is not an active adopted workspace in project '{project_id}'."
                    ),
                );
            }
            if thread.branch == *branch {
                return invariant(
                    command,
                    format!("Thread '{thread_id}' already records the resolved branch."),
                );
            }
            Ok(vec![make_event(
                "thread.meta-updated",
                "thread",
                thread_id,
                occurred_at,
                command_id,
                metadata,
                json!({"threadId":thread_id,"branch":branch,"updatedAt":occurred_at}),
            )])
        }
        OrchestrationCommand::ThreadCreate {
            command_id,
            thread_id,
            project_id,
            title,
            kind,
            host_thread_id,
            model_selection,
            runtime_mode,
            interaction_mode,
            branch,
            worktree_path,
            created_at,
        } => {
            require_project(model, command, project_id)?;
            if kind.as_deref() == Some("default")
                && model.threads.values().any(|thread| {
                    thread.project_id == project_id.as_str()
                        && thread.kind == "default"
                        && thread.deleted_at.is_none()
                })
            {
                return invariant(
                    command,
                    format!("Project '{project_id}' already has a canonical default thread."),
                );
            }
            if model.threads.contains_key(thread_id) {
                return invariant(
                    command,
                    format!("Thread '{thread_id}' already exists and cannot be created twice."),
                );
            }
            let mut payload = json!({"threadId":thread_id,"projectId":project_id,"title":title,"modelSelection":model_selection,"runtimeMode":runtime_mode,"interactionMode":interaction_mode,"branch":branch,"worktreePath":worktree_path,"createdAt":created_at,"updatedAt":created_at});
            insert_optional(&mut payload, "kind", kind.as_ref().map(|v| json!(v)));
            insert_optional(
                &mut payload,
                "hostThreadId",
                host_thread_id.as_ref().map(|v| json!(v)),
            );
            Ok(vec![make_event(
                "thread.created",
                "thread",
                thread_id,
                created_at,
                command_id,
                metadata,
                payload,
            )])
        }
        OrchestrationCommand::ThreadDelete {
            command_id,
            thread_id,
        } => {
            let thread = require_thread(model, command, thread_id)?;
            if thread.kind == "default" {
                return invariant(
                    command,
                    format!("Default thread '{thread_id}' cannot be deleted directly."),
                );
            }
            if thread.kind == "workspace" && thread.worktree_path.is_some() {
                return invariant(
                    command,
                    format!(
                        "Thread '{thread_id}' owns an adopted worktree; detach it through the dedicated server-resolved worktree API."
                    ),
                );
            }
            // Chat panels (live or closed, so archived) go with their host thread,
            // in stable id order (the model is a BTreeMap), before the host itself.
            Ok(model
                .threads
                .iter()
                .filter(|(_, candidate)| {
                    candidate.kind == "panel"
                        && candidate.deleted_at.is_none()
                        && candidate.host_thread_id.as_deref() == Some(thread_id.as_str())
                })
                .map(|(panel_id, _)| panel_id)
                .chain([thread_id])
                .map(|deleted_id| {
                    make_event(
                        "thread.deleted",
                        "thread",
                        deleted_id,
                        occurred_at,
                        command_id,
                        metadata.clone(),
                        json!({"threadId":deleted_id,"deletedAt":occurred_at}),
                    )
                })
                .collect())
        }
        OrchestrationCommand::ThreadArchive {
            command_id,
            thread_id,
        } => {
            let thread = require_thread(model, command, thread_id)?;
            if thread.kind == "default" {
                return invariant(
                    command,
                    format!("Default thread '{thread_id}' cannot be archived directly."),
                );
            }
            if thread.archived_at.is_some() {
                return invariant(
                    command,
                    format!("Thread '{thread_id}' is already archived."),
                );
            }
            Ok(vec![make_event(
                "thread.archived",
                "thread",
                thread_id,
                occurred_at,
                command_id,
                metadata,
                json!({"threadId":thread_id,"archivedAt":occurred_at,"updatedAt":occurred_at}),
            )])
        }
        OrchestrationCommand::ThreadUnarchive {
            command_id,
            thread_id,
        } => {
            let thread = require_thread(model, command, thread_id)?;
            if thread.archived_at.is_none() {
                return invariant(command, format!("Thread '{thread_id}' is not archived."));
            }
            Ok(vec![make_event(
                "thread.unarchived",
                "thread",
                thread_id,
                occurred_at,
                command_id,
                metadata,
                json!({"threadId":thread_id,"updatedAt":occurred_at}),
            )])
        }
        OrchestrationCommand::ThreadMetaUpdate {
            command_id,
            thread_id,
            title,
            model_selection,
            branch,
            worktree_path,
        } => {
            let thread = require_thread(model, command, thread_id)?;
            if thread.kind == "workspace"
                && let OptionalNullable::Present(Some(_)) = worktree_path
            {
                let path_key = prepared_worktree.path_key(command)?;
                if model.threads.iter().any(|(candidate_id, candidate)| {
                    candidate_id != thread_id
                        && candidate.project_id == thread.project_id
                        && candidate.kind == "workspace"
                        && candidate.deleted_at.is_none()
                        && candidate.worktree_path_key.as_deref() == Some(path_key)
                }) {
                    return Err(OrchestrationError::WorktreeOwnershipConflict {
                        project_id: thread.project_id.clone(),
                        owner_count: 2,
                    });
                }
            }
            let mut payload = json!({"threadId":thread_id,"updatedAt":occurred_at});
            insert_optional(&mut payload, "title", title.as_ref().map(|v| json!(v)));
            insert_optional(&mut payload, "modelSelection", model_selection.clone());
            insert_optional(
                &mut payload,
                "branch",
                branch
                    .as_ref()
                    .map(|value| value.map_or(Value::Null, |value| json!(value))),
            );
            insert_optional(
                &mut payload,
                "worktreePath",
                worktree_path
                    .as_ref()
                    .map(|value| value.map_or(Value::Null, |value| json!(value))),
            );
            Ok(vec![make_event(
                "thread.meta-updated",
                "thread",
                thread_id,
                occurred_at,
                command_id,
                metadata,
                payload,
            )])
        }
        OrchestrationCommand::ThreadRuntimeModeSet {
            command_id,
            thread_id,
            runtime_mode,
            ..
        } => {
            require_thread(model, command, thread_id)?;
            Ok(vec![make_event(
                "thread.runtime-mode-set",
                "thread",
                thread_id,
                occurred_at,
                command_id,
                metadata,
                json!({"threadId":thread_id,"runtimeMode":runtime_mode,"updatedAt":occurred_at}),
            )])
        }
        OrchestrationCommand::ThreadInteractionModeSet {
            command_id,
            thread_id,
            interaction_mode,
            ..
        } => {
            require_thread(model, command, thread_id)?;
            Ok(vec![make_event(
                "thread.interaction-mode-set",
                "thread",
                thread_id,
                occurred_at,
                command_id,
                metadata,
                json!({"threadId":thread_id,"interactionMode":interaction_mode,"updatedAt":occurred_at}),
            )])
        }
        OrchestrationCommand::ThreadTurnStart {
            command_id,
            thread_id,
            message,
            model_selection,
            title_seed,
            bootstrap,
            source_proposed_plan,
            queued,
            created_at,
            ..
        } => {
            let (thread, created) = match model.threads.get(thread_id) {
                Some(thread) if thread.deleted_at.is_none() => (thread.clone(), None),
                Some(_) => {
                    return invariant(command, format!("Thread '{thread_id}' is deleted."));
                }
                None => {
                    let Some(create) = bootstrap
                        .as_deref()
                        .and_then(|bootstrap| bootstrap.create_thread.as_ref())
                    else {
                        return invariant(
                            command,
                            format!(
                                "Thread '{thread_id}' does not exist for command '{}'.",
                                command.command_type()
                            ),
                        );
                    };
                    require_project(model, command, &create.project_id)?;
                    let event = make_event(
                        "thread.created",
                        "thread",
                        thread_id,
                        &create.created_at,
                        command_id,
                        metadata.clone(),
                        json!({
                            "threadId":thread_id,
                            "projectId":create.project_id,
                            "title":create.title,
                            "modelSelection":create.model_selection,
                            "runtimeMode":create.runtime_mode,
                            "interactionMode":create.interaction_mode,
                            "branch":create.branch,
                            "worktreePath":create.worktree_path,
                            "createdAt":create.created_at,
                            "updatedAt":create.created_at,
                        }),
                    );
                    (
                        ThreadState {
                            project_id: create.project_id.clone(),
                            kind: "workspace".to_owned(),
                            host_thread_id: None,
                            runtime_mode: create.runtime_mode.clone(),
                            interaction_mode: create.interaction_mode.clone(),
                            branch: create.branch.clone(),
                            worktree_path: create.worktree_path.clone(),
                            worktree_path_key: prepared_worktree.path_key.clone(),
                            archived_at: None,
                            deleted_at: None,
                        },
                        Some(event),
                    )
                }
            };
            if let Some(source) = source_proposed_plan {
                let source_thread_id = required_command_string(command, source, "threadId")?;
                let source_plan_id = required_command_string(command, source, "planId")?;
                let source_thread = require_thread(model, command, &source_thread_id)?;
                if source_thread.project_id != thread.project_id {
                    return invariant(
                        command,
                        format!(
                            "Proposed plan '{source_plan_id}' belongs to thread '{source_thread_id}' in a different project."
                        ),
                    );
                }
                let plan_exists = repositories
                    .list_proposed_plans_by_thread(source_thread_id.clone())
                    .await
                    .map_err(wrap_persistence)?
                    .iter()
                    .any(|plan| plan.plan_id == source_plan_id);
                if !plan_exists {
                    return invariant(
                        command,
                        format!(
                            "Proposed plan '{source_plan_id}' does not exist on thread '{source_thread_id}'."
                        ),
                    );
                }
            }
            let user = make_event(
                "thread.message-sent",
                "thread",
                thread_id,
                created_at,
                command_id,
                metadata.clone(),
                json!({"threadId":thread_id,"messageId":message.message_id,"role":"user","text":message.text,"attachments":message.attachments,"turnId":null,"streaming":false,"createdAt":created_at,"updatedAt":created_at}),
            );
            if *queued == Some(true) {
                return Ok(created.into_iter().chain([user]).collect());
            }
            let mut payload = json!({"threadId":thread_id,"messageId":message.message_id,"runtimeMode":thread.runtime_mode,"interactionMode":thread.interaction_mode,"createdAt":created_at});
            insert_optional(&mut payload, "modelSelection", model_selection.clone());
            insert_optional(
                &mut payload,
                "titleSeed",
                title_seed.as_ref().map(|v| json!(v)),
            );
            insert_optional(
                &mut payload,
                "sourceProposedPlan",
                source_proposed_plan.clone(),
            );
            let mut start = make_event(
                "thread.turn-start-requested",
                "thread",
                thread_id,
                created_at,
                command_id,
                metadata,
                payload,
            );
            start.causation_event_id = Some(user.event_id.clone());
            Ok(created.into_iter().chain([user, start]).collect())
        }
        OrchestrationCommand::ThreadTurnInterrupt {
            command_id,
            thread_id,
            turn_id,
            created_at,
        } => event_with_optional(
            command,
            "thread.turn-interrupt-requested",
            command_id,
            thread_id,
            created_at,
            metadata,
            json!({"threadId":thread_id,"createdAt":created_at}),
            "turnId",
            turn_id.as_ref().map(|v| json!(v)),
            model,
        ),
        OrchestrationCommand::ThreadTurnSteer {
            command_id,
            thread_id,
            message_id,
            created_at,
        } => {
            require_thread(model, command, thread_id)?;
            let row = repositories
                .get_queued_provider_turn_head(thread_id.clone())
                .await
                .map_err(wrap_persistence)?;
            let Some(row) = row.filter(|row| row.message_id == *message_id) else {
                return invariant(command, "Only the queued head can be steered.".to_owned());
            };
            let session = repositories
                .get_thread_session(thread_id.clone())
                .await
                .map_err(wrap_persistence)?;
            let Some(turn_id) = session
                .filter(|session| session.status == "running")
                .and_then(|session| session.active_turn_id)
                .filter(|id| !id.trim().is_empty())
            else {
                return invariant(command, "Steering requires a running turn.".to_owned());
            };
            if !crate::provider::supports_turn_steer(&row.provider_kind) {
                return invariant(command, "provider does not support steering".to_owned());
            }
            Ok(vec![
                make_event(
                    "thread.turn-steer-requested",
                    "thread",
                    thread_id,
                    created_at,
                    command_id,
                    metadata.clone(),
                    json!({"threadId":thread_id, "messageId":message_id, "turnId":turn_id, "createdAt":created_at}),
                ),
                make_event(
                    "thread.turn-delivery-updated",
                    "thread",
                    thread_id,
                    created_at,
                    command_id,
                    metadata,
                    json!({"threadId":thread_id, "messageId":message_id, "state":"pending", "provider":row.provider_kind,
                        "mode":"steer", "held":false, "detail":null, "updatedAt":created_at,
                        "delivery":delivery_value("pending", &row.provider_kind, &row.provider_instance_id, "steer", false, None)}),
                ),
            ])
        }
        OrchestrationCommand::ThreadTurnPromote {
            command_id,
            thread_id,
            message_id,
            created_at,
        } => {
            require_thread(model, command, thread_id)?;
            if !repositories
                .can_promote_queued_provider_turn(
                    thread_id.clone(),
                    message_id.clone(),
                    command_id.starts_with("server:"),
                )
                .await
                .map_err(wrap_persistence)?
            {
                return invariant(
                    command,
                    "Only the queued head of a settled thread can be promoted.".to_owned(),
                );
            }
            let row = repositories
                .get_provider_turn_delivery_for_message(thread_id.clone(), message_id.clone())
                .await
                .map_err(wrap_persistence)?;
            let Some(row) = row else {
                return invariant(command, "Queued delivery no longer exists.".to_owned());
            };
            let stored =
                serde_json::from_value::<OrchestrationCommand>(row.payload).map_err(|_| {
                    OrchestrationError::Invariant {
                        command_type: command.command_type().to_owned(),
                        detail: "Queued turn payload is invalid.".to_owned(),
                    }
                })?;
            let OrchestrationCommand::ThreadTurnStart {
                thread_id: stored_thread,
                message,
                model_selection,
                runtime_mode,
                interaction_mode,
                title_seed,
                source_proposed_plan,
                ..
            } = stored
            else {
                return invariant(
                    command,
                    "Queued delivery must contain a turn start.".to_owned(),
                );
            };
            if stored_thread != *thread_id || message.message_id != *message_id {
                return invariant(
                    command,
                    "Queued turn identity does not match its delivery.".to_owned(),
                );
            }
            let mut payload = json!({"threadId":thread_id, "messageId":message_id, "runtimeMode":runtime_mode, "interactionMode":interaction_mode, "createdAt":created_at});
            insert_optional(&mut payload, "modelSelection", model_selection);
            insert_optional(
                &mut payload,
                "titleSeed",
                title_seed.map(|value| json!(value)),
            );
            insert_optional(&mut payload, "sourceProposedPlan", source_proposed_plan);
            Ok(vec![
                make_event(
                    "thread.turn-start-requested",
                    "thread",
                    thread_id,
                    created_at,
                    command_id,
                    metadata.clone(),
                    payload,
                ),
                make_event(
                    "thread.turn-delivery-updated",
                    "thread",
                    thread_id,
                    created_at,
                    command_id,
                    metadata,
                    json!({"threadId":thread_id, "messageId":message_id, "state":"pending", "provider":row.provider_kind, "mode":"start", "held":false, "detail":null, "updatedAt":created_at,
                        "delivery":delivery_value("pending", &row.provider_kind, &row.provider_instance_id, "start", false, None)}),
                ),
            ])
        }
        OrchestrationCommand::ThreadTurnDeliveryResolve { .. } => invariant(
            command,
            "Delivery resolution must be persisted through its atomic transition path.".to_owned(),
        ),
        OrchestrationCommand::ThreadApprovalRespond {
            command_id,
            thread_id,
            request_id,
            decision,
            created_at,
        } => {
            require_thread(model, command, thread_id)?;
            Ok(vec![make_event(
                "thread.approval-response-requested",
                "thread",
                thread_id,
                created_at,
                command_id,
                json!({"requestId":request_id}),
                json!({"threadId":thread_id,"requestId":request_id,"decision":decision,"createdAt":created_at}),
            )])
        }
        OrchestrationCommand::ThreadUserInputRespond {
            command_id,
            thread_id,
            request_id,
            answers,
            created_at,
        } => {
            require_thread(model, command, thread_id)?;
            Ok(vec![make_event(
                "thread.user-input-response-requested",
                "thread",
                thread_id,
                created_at,
                command_id,
                json!({"requestId":request_id}),
                json!({"threadId":thread_id,"requestId":request_id,"answers":answers,"createdAt":created_at}),
            )])
        }
        OrchestrationCommand::ThreadCheckpointRevert {
            command_id,
            thread_id,
            turn_count,
            created_at,
        } => simple_thread_event(
            model,
            command,
            "thread.checkpoint-revert-requested",
            command_id,
            thread_id,
            created_at,
            metadata,
            json!({"threadId":thread_id,"turnCount":turn_count,"createdAt":created_at}),
        ),
        OrchestrationCommand::ThreadSessionStop {
            command_id,
            thread_id,
            created_at,
        } => simple_thread_event(
            model,
            command,
            "thread.session-stop-requested",
            command_id,
            thread_id,
            created_at,
            metadata,
            json!({"threadId":thread_id,"createdAt":created_at}),
        ),
        OrchestrationCommand::ThreadSessionSet {
            command_id,
            thread_id,
            session,
            created_at,
        } => simple_thread_event(
            model,
            command,
            "thread.session-set",
            command_id,
            thread_id,
            created_at,
            metadata,
            json!({"threadId":thread_id,"session":session}),
        ),
        OrchestrationCommand::ThreadMessageAssistantDelta {
            command_id,
            thread_id,
            message_id,
            delta,
            turn_id,
            created_at,
        } => simple_thread_event(
            model,
            command,
            "thread.message-sent",
            command_id,
            thread_id,
            created_at,
            metadata,
            json!({"threadId":thread_id,"messageId":message_id,"role":"assistant","text":delta,"turnId":turn_id,"streaming":true,"createdAt":created_at,"updatedAt":created_at}),
        ),
        OrchestrationCommand::ThreadMessageAssistantComplete {
            command_id,
            thread_id,
            message_id,
            turn_id,
            created_at,
        } => simple_thread_event(
            model,
            command,
            "thread.message-sent",
            command_id,
            thread_id,
            created_at,
            metadata,
            json!({"threadId":thread_id,"messageId":message_id,"role":"assistant","text":"","turnId":turn_id,"streaming":false,"createdAt":created_at,"updatedAt":created_at}),
        ),
        OrchestrationCommand::ThreadProposedPlanUpsert {
            command_id,
            thread_id,
            proposed_plan,
            created_at,
        } => simple_thread_event(
            model,
            command,
            "thread.proposed-plan-upserted",
            command_id,
            thread_id,
            created_at,
            metadata,
            json!({"threadId":thread_id,"proposedPlan":proposed_plan}),
        ),
        OrchestrationCommand::ThreadTurnDiffComplete {
            command_id,
            thread_id,
            turn_id,
            checkpoint_turn_count,
            checkpoint_ref,
            status,
            files,
            assistant_message_id,
            completed_at,
            created_at,
        } => simple_thread_event(
            model,
            command,
            "thread.turn-diff-completed",
            command_id,
            thread_id,
            created_at,
            metadata,
            json!({"threadId":thread_id,"turnId":turn_id,"checkpointTurnCount":checkpoint_turn_count,"checkpointRef":checkpoint_ref,"status":status,"files":files,"assistantMessageId":assistant_message_id,"completedAt":completed_at}),
        ),
        OrchestrationCommand::ThreadActivityAppend {
            command_id,
            thread_id,
            activity,
            created_at,
        } => {
            let event_metadata = activity
                .payload
                .get("requestId")
                .and_then(Value::as_str)
                .map_or(metadata, |request_id| json!({"requestId":request_id}));
            simple_thread_event(
                model,
                command,
                "thread.activity-appended",
                command_id,
                thread_id,
                created_at,
                event_metadata,
                json!({"threadId":thread_id,"activity":activity}),
            )
        }
        OrchestrationCommand::ThreadRevertComplete {
            command_id,
            thread_id,
            turn_count,
            created_at,
        } => simple_thread_event(
            model,
            command,
            "thread.reverted",
            command_id,
            thread_id,
            created_at,
            metadata,
            json!({"threadId":thread_id,"turnCount":turn_count}),
        ),
        OrchestrationCommand::ThreadHistoryImport {
            command_id,
            thread_id,
            messages,
        } => {
            require_thread(model, command, thread_id)?;
            // Checked by the engine worker, so a turn admitted after the importer's own check
            // still keeps CLI history out of the thread's new conversation.
            if repositories
                .get_thread(thread_id.clone())
                .await
                .map_err(wrap_persistence)?
                .is_some_and(|thread| {
                    thread.latest_turn_id.is_some() || thread.latest_user_message_at.is_some()
                })
            {
                return invariant(
                    command,
                    "This thread was used before its history import.".to_owned(),
                );
            }
            if messages.is_empty() || messages.len() > MAX_IMPORTED_THREAD_MESSAGES {
                return invariant(
                    command,
                    format!(
                        "A history import carries 1 to {MAX_IMPORTED_THREAD_MESSAGES} messages."
                    ),
                );
            }
            if let Some(message) = messages
                .iter()
                .find(|message| !matches!(message.role.as_str(), "user" | "assistant"))
            {
                return invariant(
                    command,
                    format!(
                        "Imported message '{}' has unsupported role '{}'.",
                        message.message_id, message.role
                    ),
                );
            }
            // Marks history so effects take no checkpoint baseline for a turn that never ran.
            let mut metadata = metadata;
            metadata["historyImport"] = json!(true);
            Ok(messages
                .iter()
                .map(|message| {
                    make_event(
                        "thread.message-sent",
                        "thread",
                        thread_id,
                        &message.created_at,
                        command_id,
                        metadata.clone(),
                        json!({"threadId":thread_id,"messageId":message.message_id,"role":message.role,"text":message.text,"attachments":[],"turnId":null,"streaming":false,"createdAt":message.created_at,"updatedAt":message.created_at}),
                    )
                })
                .collect())
        }
    }
}

fn resolved_worktree_title(branch: Option<&str>, head: Option<&str>, path: &str) -> String {
    branch
        .map(str::to_owned)
        .or_else(|| head.map(|value| value.chars().take(7).collect()))
        .or_else(|| {
            path.rsplit(['/', '\\'])
                .find(|component| !component.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| path.to_owned())
}

async fn compact_adoption_policy(
    command: &OrchestrationCommand,
    current: &Value,
    adopted_path_key: &str,
) -> Result<Value, OrchestrationError> {
    let mut policy = current.clone();
    let baseline = policy
        .as_object_mut()
        .and_then(|policy| policy.get_mut("baselinePaths"))
        .and_then(Value::as_array_mut)
        .ok_or_else(|| OrchestrationError::Invariant {
            command_type: command.command_type().to_owned(),
            detail: "Persisted worktree discovery policy has no baselinePaths array.".to_owned(),
        })?;
    compact_policy_baseline(command, baseline, adopted_path_key).await?;
    Ok(policy)
}

async fn compact_detach_policy(
    command: &OrchestrationCommand,
    current: &Value,
    detached_path_key: &str,
) -> Result<Value, OrchestrationError> {
    let mut policy = current.clone();
    let baseline = policy
        .as_object_mut()
        .and_then(|policy| policy.get_mut("baselinePaths"))
        .and_then(Value::as_array_mut)
        .ok_or_else(|| OrchestrationError::Invariant {
            command_type: command.command_type().to_owned(),
            detail: "Persisted worktree discovery policy has no baselinePaths array.".to_owned(),
        })?;
    compact_policy_baseline(command, baseline, detached_path_key).await?;
    Ok(policy)
}

async fn compact_policy_baseline(
    command: &OrchestrationCommand,
    baseline: &mut Vec<Value>,
    removed_path_key: &str,
) -> Result<(), OrchestrationError> {
    let mut retained = Vec::with_capacity(baseline.len());
    for path in std::mem::take(baseline) {
        let Some(value) = path.as_str() else {
            retained.push(path);
            continue;
        };
        let path_key = canonicalize_command_worktree_path(command.command_id(), value).await?;
        if path_key != removed_path_key {
            retained.push(path);
        }
    }
    *baseline = retained;
    Ok(())
}

fn invariant<T>(command: &OrchestrationCommand, detail: String) -> Result<T, OrchestrationError> {
    Err(OrchestrationError::Invariant {
        command_type: command.command_type().to_owned(),
        detail,
    })
}

fn require_project<'a>(
    model: &'a CommandModel,
    command: &OrchestrationCommand,
    project_id: &str,
) -> Result<&'a ProjectState, OrchestrationError> {
    let Some(project) = model.projects.get(project_id) else {
        return invariant(
            command,
            format!(
                "Project '{project_id}' does not exist for command '{}'.",
                command.command_type()
            ),
        );
    };
    if project.deleted_at.is_some() {
        return invariant(command, format!("Project '{project_id}' is deleted."));
    }
    Ok(project)
}

fn require_thread<'a>(
    model: &'a CommandModel,
    command: &OrchestrationCommand,
    thread_id: &str,
) -> Result<&'a ThreadState, OrchestrationError> {
    let Some(thread) = model.threads.get(thread_id) else {
        return Err(OrchestrationError::Invariant {
            command_type: command.command_type().to_owned(),
            detail: format!(
                "Thread '{thread_id}' does not exist for command '{}'.",
                command.command_type()
            ),
        });
    };
    if thread.deleted_at.is_some() {
        return Err(OrchestrationError::Invariant {
            command_type: command.command_type().to_owned(),
            detail: format!("Thread '{thread_id}' is deleted."),
        });
    }
    Ok(thread)
}

#[allow(clippy::too_many_arguments)]
fn simple_thread_event(
    model: &CommandModel,
    command: &OrchestrationCommand,
    event_type: &str,
    command_id: &str,
    thread_id: &str,
    occurred_at: &str,
    metadata: Value,
    payload: Value,
) -> Result<Vec<NewOrchestrationEvent>, OrchestrationError> {
    require_thread(model, command, thread_id)?;
    Ok(vec![make_event(
        event_type,
        "thread",
        thread_id,
        occurred_at,
        command_id,
        metadata,
        payload,
    )])
}

#[allow(clippy::too_many_arguments)]
fn event_with_optional(
    command: &OrchestrationCommand,
    event_type: &str,
    command_id: &str,
    thread_id: &str,
    occurred_at: &str,
    metadata: Value,
    mut payload: Value,
    key: &str,
    value: Option<Value>,
    model: &CommandModel,
) -> Result<Vec<NewOrchestrationEvent>, OrchestrationError> {
    insert_optional(&mut payload, key, value);
    simple_thread_event(
        model,
        command,
        event_type,
        command_id,
        thread_id,
        occurred_at,
        metadata,
        payload,
    )
}

fn insert_optional(target: &mut Value, key: &str, value: Option<Value>) {
    if let (Some(object), Some(value)) = (target.as_object_mut(), value) {
        object.insert(key.to_owned(), value);
    }
}

fn delivery_value(
    state: &str,
    provider: &str,
    provider_instance_id: &str,
    mode: &str,
    held: bool,
    detail: Option<&str>,
) -> Value {
    let mut delivery = json!({"state":state, "provider":provider, "providerInstanceId":provider_instance_id, "mode":mode, "held":held});
    insert_optional(&mut delivery, "detail", detail.map(|value| json!(value)));
    delivery
}

fn required_command_string(
    command: &OrchestrationCommand,
    value: &Value,
    key: &str,
) -> Result<String, OrchestrationError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| OrchestrationError::Invariant {
            command_type: command.command_type().to_owned(),
            detail: format!("Command field '{key}' must be a string."),
        })
}

struct PersistCommandOutcome {
    committed: VecDeque<OrchestrationEvent>,
    result_sequence: i64,
}

#[allow(clippy::too_many_arguments)]
async fn persist_command(
    repositories: &Repositories,
    hooks: &TestHooks,
    command: &OrchestrationCommand,
    events: &[NewOrchestrationEvent],
    command_id: &str,
    aggregate: (&str, &str),
    admission: Option<CommandAdmission>,
    commit_fence: Option<CommitFence>,
    projection_mode: ProjectionMode,
) -> Result<PersistCommandOutcome, OrchestrationError> {
    let repositories = repositories.clone();
    let hooks = hooks.clone();
    let mut event_list = events.to_vec();
    let command_id = command_id.to_owned();
    let aggregate_kind = aggregate.0.to_owned();
    let aggregate_id = aggregate.1.to_owned();
    let promotion = match command {
        OrchestrationCommand::ThreadTurnPromote {
            thread_id,
            message_id,
            command_id,
            created_at,
        } => Some((
            thread_id.clone(),
            message_id.clone(),
            command_id.starts_with("server:"),
            created_at.clone(),
        )),
        _ => None,
    };
    let steering = match command {
        OrchestrationCommand::ThreadTurnSteer {
            thread_id,
            message_id,
            created_at,
            ..
        } => Some((
            thread_id.clone(),
            message_id.clone(),
            created_at.clone(),
            events[0].payload["turnId"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        )),
        _ => None,
    };
    let hold = match command {
        OrchestrationCommand::ThreadTurnInterrupt {
            thread_id,
            created_at,
            ..
        } => Some((thread_id.clone(), created_at.clone())),
        OrchestrationCommand::ThreadSessionSet {
            thread_id,
            session,
            created_at,
            ..
        } if matches!(session.status.as_str(), "error" | "interrupted") => {
            Some((thread_id.clone(), created_at.clone()))
        }
        _ => None,
    };
    let committed = repositories
        .database()
        .call(move |connection| {
            let transaction = connection.transaction()?;
            if let Some((thread_id, message_id, automatic, created_at)) = promotion {
                // Recheck inside the event transaction: another owner may have cancelled,
                // promoted, interrupted, or started a turn since command planning.
                if !can_promote_queued_provider_turn_on(&transaction, &thread_id, &message_id, automatic)? {
                    return Ok(None);
                }
                transaction.execute("UPDATE provider_turn_outbox SET state = 'pending', mode = 'start', held = 0, last_error = NULL, updated_at = ? WHERE thread_id = ? AND message_id = ? AND state = 'queued'",
                    params![created_at, thread_id, message_id])?;
            }
            if let Some((thread_id, message_id, created_at, turn_id)) = steering {
                let head = crate::persistence::queued_provider_turn_head_on(&transaction, &thread_id)?;
                if !head.is_some_and(|row| row.message_id == message_id && crate::provider::supports_turn_steer(&row.provider_kind)) {
                    return Ok(None);
                }
                let updated = transaction.execute(
                    "UPDATE provider_turn_outbox SET state = 'pending', mode = 'steer', held = 0, last_error = NULL, updated_at = ?
                     WHERE thread_id = ? AND message_id = ? AND state = 'queued'
                       AND EXISTS(SELECT 1 FROM projection_thread_sessions WHERE thread_id = provider_turn_outbox.thread_id AND status = 'running' AND active_turn_id = ?)",
                    params![created_at, thread_id, message_id, turn_id],
                )?;
                if updated != 1 { return Ok(None); }
            }
            if let Some((thread_id, updated_at)) = hold {
                for delivery_id in hold_queued_provider_turns(&transaction, &thread_id, &updated_at)? {
                    let (message_id, provider, mode, detail, state, provider_instance_id) = transaction.query_row(
                        "SELECT message_id, provider_kind, mode, last_error, state, provider_instance_id FROM provider_turn_outbox WHERE command_id = ?",
                        [&delivery_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, Option<String>>(3)?, row.get::<_, String>(4)?, row.get::<_, String>(5)?)),
                    )?;
                    event_list.push(make_event("thread.turn-delivery-updated", "thread", &thread_id, &updated_at, &command_id,
                        json!({"deliveryCommandId":delivery_id}),
                        json!({"threadId":thread_id, "messageId":message_id, "state":state, "provider":provider, "mode":mode, "held":true, "detail":detail, "updatedAt":updated_at,
                            "delivery":delivery_value(&state, &provider, &provider_instance_id, &mode, true, detail.as_deref())})));
                }
            }
            let mut committed = VecDeque::new();
            for planned in &event_list {
                if projection_mode == ProjectionMode::UpdateExistingAssistantMessage
                    && !streaming_assistant_message_exists_tx(&transaction, &planned.payload)?
                {
                    continue;
                }
                let saved = append_event_tx(&transaction, planned.clone())?;
                let mut projection_context = ProjectionContext::new(projection_mode);
                for projector in PROJECTOR_NAMES {
                    hooks
                        .maybe_fail(projector, &saved.event.event_type)
                        .map_err(projector_failure_to_persistence)?;
                    apply_projector_tx(
                        &transaction,
                        projector,
                        &saved,
                        &mut projection_context,
                    )?;
                    upsert_projection_state_tx(
                        &transaction,
                        projector,
                        saved.sequence,
                        &saved.event.occurred_at,
                    )?;
                }
                if saved.event.aggregate_kind == "thread" {
                    rebuild_thread_derived_fields_tx(&transaction, &saved.event.aggregate_id)?;
                }
                committed.push_back(saved);
            }
            let accepted_at = event_list
                .last()
                .map(|event| event.occurred_at.clone())
                .ok_or_else(|| {
                    PersistenceError::Corrupt("planned command emitted no events".to_owned())
                })?;
            let result_sequence = match committed.back() {
                Some(event) => event.sequence,
                None => transaction.query_row(
                    "SELECT COALESCE(MAX(sequence), 0) FROM orchestration_events",
                    [],
                    |row| row.get(0),
                )?,
            };
            upsert_command_receipt_tx(
                &transaction,
                CommandReceipt {
                    command_id: command_id.clone(),
                    aggregate_kind,
                    aggregate_id,
                    accepted_at,
                    result_sequence,
                    status: "accepted".to_owned(),
                    error: None,
                    payload_digest: admission.as_ref().map(|value| value.payload_digest.clone()),
                },
            )?;
            if let Some(admission) = admission {
                for reference in admission.attachment_refs {
                    transaction.execute(
                        "INSERT INTO orchestration_attachment_refs (command_id, attachment_id, content_digest, size_bytes) VALUES (?, ?, ?, ?)",
                        params![command_id, reference.attachment_id, reference.content_digest, reference.size_bytes],
                    )?;
                }
                if let Some(turn) = admission.provider_turn {
                    transaction.execute(
                        "INSERT INTO provider_turn_outbox (command_id, thread_id, message_id, provider_instance_id, provider_kind, provider_session_id, delivery_key, payload_json, state, mode, attempts, last_error, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, NULL, ?, ?)",
                        params![turn.command_id, turn.thread_id, turn.message_id, turn.provider_instance_id, turn.provider_kind, turn.provider_session_id, turn.delivery_key, json_string(&turn.payload)?, turn_delivery_state_name(turn.state), turn.mode.as_str(), turn.created_at, turn.created_at],
                    )?;
                }
            }
            hooks.maybe_pause_before_command_finalization();
            let _commit_permit = commit_fence
                .as_ref()
                .map(CommitFence::acquire)
                .transpose()?;
            hooks.maybe_pause_after_command_finalization();
            transaction.commit()?;
            Ok(Some(PersistCommandOutcome {
                committed,
                result_sequence,
            }))
        })
        .await
        .map_err(wrap_persistence)?
        .ok_or_else(|| OrchestrationError::Invariant { command_type: command.command_type().to_owned(), detail: "Queued delivery changed before promotion or steering.".to_owned() })?;
    Ok(committed)
}

async fn persist_rejected_command(
    repositories: &Repositories,
    hooks: &TestHooks,
    receipt: CommandReceipt,
    commit_fence: Option<CommitFence>,
) -> Result<(), OrchestrationError> {
    let database = repositories.database().clone();
    let hooks = hooks.clone();
    database
        .call(move |connection| {
            let transaction = connection.transaction()?;
            upsert_command_receipt_tx(&transaction, receipt)?;
            hooks.maybe_pause_before_command_finalization();
            let _commit_permit = commit_fence
                .as_ref()
                .map(CommitFence::acquire)
                .transpose()?;
            hooks.maybe_pause_after_command_finalization();
            transaction.commit()?;
            Ok(())
        })
        .await
        .map_err(wrap_persistence)
}

async fn persist_turn_delivery_transition(
    repositories: &Repositories,
    events: &broadcast::Sender<OrchestrationEvent>,
    hooks: &TestHooks,
    transition: TurnDeliveryTransition,
    reason: Option<TurnDeliveryFailureReason>,
) -> Result<bool, OrchestrationError> {
    hooks.maybe_fail_delivery_transition()?;
    let database = repositories.database().clone();
    let hooks = hooks.clone();
    let committed = database
        .call(move |connection| {
            let transaction = connection.transaction()?;
            let current = transaction
                .query_row(
                    "SELECT thread_id, message_id, provider_kind, state, attempts, mode, held, provider_instance_id, last_error, failure_reason FROM provider_turn_outbox WHERE command_id = ?",
                    [&transition.command_id],
                    |row| Ok(((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?, row.get::<_, i64>(4)?, row.get::<_, String>(5)?, row.get::<_, bool>(6)?, row.get::<_, String>(7)?), (row.get::<_, Option<String>>(8)?, row.get::<_, Option<String>>(9)?))),
                )
                .optional()?;
            let Some(((thread_id, message_id, provider, state, attempts, mut mode, held, provider_instance_id), (last_error, failure_reason))) = current else {
                return Ok(DeliveryTransitionCommit::Stale);
            };
            if attempts != transition.expected_attempt
                || !transition
                    .expected_states
                    .iter()
                    .any(|expected| turn_delivery_state_name(*expected) == state)
            {
                return Ok(DeliveryTransitionCommit::Stale);
            }
            let next_state = turn_delivery_state_name(transition.next_state);
            // A claimed delivery that fails again the same way returns to exactly what the thread
            // already shows (pending, the same detail): claiming emits nothing. Record the
            // attempt, but announce nothing, or every retry re-sends the thread (a provider that
            // could not start updated it every second).
            let unchanged_retry = state == turn_delivery_state_name(TurnDeliveryState::Sending)
                && transition.next_state == TurnDeliveryState::Pending
                && transition.detail.is_some()
                && transition.detail == last_error
                && failure_reason.is_none();
            let found_without_turn = matches!(
                transition.next_state,
                TurnDeliveryState::Delivered | TurnDeliveryState::Dismissed
            ) && reason == Some(TurnDeliveryFailureReason::FoundWithoutTurn);
            let reason = reason.filter(|reason| matches!((transition.next_state, reason),
                (TurnDeliveryState::Failed, TurnDeliveryFailureReason::ModelSelectionRefused)
                | (TurnDeliveryState::Delivered, TurnDeliveryFailureReason::StartedNewConversation)))
                .map(TurnDeliveryFailureReason::as_str);
            if transition.next_state == TurnDeliveryState::Queued {
                let Some(row) = crate::persistence::requeue_provider_turn_on(
                    &transaction, &transition.command_id, transition.expected_attempt, &transition.updated_at,
                )? else { return Ok(DeliveryTransitionCommit::Stale); };
                mode = row.mode.as_str().to_owned();
                transaction.execute("UPDATE provider_turn_outbox SET last_error = ?, failure_reason = NULL WHERE command_id = ?", params![transition.detail, transition.command_id])?;
            } else {
                let updated = transaction.execute(
                    "UPDATE provider_turn_outbox SET state = ?, last_error = ?, failure_reason = ?, updated_at = ? WHERE command_id = ? AND state = ? AND attempts = ?",
                    params![next_state, transition.detail, reason, transition.updated_at, transition.command_id, state, transition.expected_attempt],
                )?;
                if updated == 0 { return Ok(DeliveryTransitionCommit::Stale); }
                if unchanged_retry {
                    transaction.commit()?;
                    return Ok(DeliveryTransitionCommit::Unannounced);
                }
            }
            let turn_id = if transition.next_state == TurnDeliveryState::Delivered && mode == "steer" {
                transition.turn_id.or(crate::persistence::provider_turn_steer_target_on(&transaction, &thread_id, &message_id)?)
            } else { None };
            let mut payload = json!({
                "threadId": thread_id, "messageId": message_id, "state": next_state, "provider": provider,
                "mode":mode, "held":held, "detail":transition.detail,
                "delivery":delivery_value(next_state, &provider, &provider_instance_id, &mode, held, transition.detail.as_deref()),
                "updatedAt":transition.updated_at,
            });
            insert_optional(&mut payload, "turnId", turn_id.map(Value::String));
            if let Some(reason) = reason {
                payload["delivery"]["reason"] = json!(reason);
            }
            let metadata = if found_without_turn {
                json!({ "foundWithoutTurn": true })
            } else {
                json!({})
            };
            let planned = make_event(
                "thread.turn-delivery-updated", "thread", &thread_id, &transition.updated_at,
                &format!("server:turn-delivery:{}", transition.command_id), metadata, payload,
            );
            let saved = append_event_tx(&transaction, planned)?;
            for projector in PROJECTOR_NAMES {
                hooks
                    .maybe_fail(projector, &saved.event.event_type)
                    .map_err(projector_failure_to_persistence)?;
                apply_projector_tx(
                    &transaction,
                    projector,
                    &saved,
                    &mut ProjectionContext::default(),
                )?;
                upsert_projection_state_tx(
                    &transaction,
                    projector,
                    saved.sequence,
                    &saved.event.occurred_at,
                )?;
            }
            rebuild_thread_derived_fields_tx(&transaction, &saved.event.aggregate_id)?;
            transaction.commit()?;
            Ok(DeliveryTransitionCommit::Announced(Box::new(saved)))
        })
        .await
        .map_err(wrap_persistence)?;
    match committed {
        DeliveryTransitionCommit::Announced(event) => {
            let _ = events.send(*event);
            Ok(true)
        }
        DeliveryTransitionCommit::Unannounced => Ok(true),
        DeliveryTransitionCommit::Stale => Ok(false),
    }
}

/// How a delivery transition was committed: `Stale` when the row had moved on, `Unannounced` when
/// it changed nothing the thread shows.
enum DeliveryTransitionCommit {
    Stale,
    Unannounced,
    Announced(Box<OrchestrationEvent>),
}

#[allow(clippy::too_many_arguments)]
async fn persist_turn_delivery_resolution(
    repositories: &Repositories,
    events: &broadcast::Sender<OrchestrationEvent>,
    hooks: &TestHooks,
    resolution_command_id: String,
    thread_id: String,
    message_id: String,
    action: TurnDeliveryResolutionAction,
    updated_at: String,
    payload_digest: Option<String>,
) -> Result<Option<OrchestrationEvent>, OrchestrationError> {
    let database = repositories.database().clone();
    let hooks = hooks.clone();
    let committed = database
        .call(move |connection| {
            let transaction = connection.transaction()?;
            let current = transaction
                .query_row(
                    "SELECT command_id, provider_kind, state, last_error, mode, held, provider_instance_id FROM provider_turn_outbox WHERE thread_id = ? AND message_id = ?",
                    params![thread_id, message_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, String>(4)?,
                            row.get::<_, bool>(5)?,
                            row.get::<_, String>(6)?,
                        ))
                    },
                )
                .optional()?;
            let Some((delivery_command_id, provider, state, last_error, mode, held, provider_instance_id)) = current else {
                return Ok(None);
            };
            let resolvable = match action {
                TurnDeliveryResolutionAction::Retry => state == "uncertain" || state == "failed",
                TurnDeliveryResolutionAction::Dismiss => {
                    matches!(state.as_str(), "pending" | "sending" | "uncertain" | "failed")
                }
                TurnDeliveryResolutionAction::Cancel => state == "queued",
            };
            if !resolvable {
                return Ok(None);
            }

            let (next_state, detail, updated) = match action {
                TurnDeliveryResolutionAction::Cancel => (
                    "dismissed",
                    last_error,
                    transaction.execute(
                        "UPDATE provider_turn_outbox SET state = 'dismissed', updated_at = ? WHERE command_id = ? AND state = 'queued'",
                        params![updated_at, delivery_command_id],
                    )?,
                ),
                TurnDeliveryResolutionAction::Retry => (
                    "pending",
                    None,
                    transaction.execute(
                        "UPDATE provider_turn_outbox SET state = 'pending', attempts = 0, last_error = NULL, failure_reason = NULL, updated_at = ? WHERE command_id = ? AND state IN ('uncertain', 'failed')",
                        params![updated_at, delivery_command_id],
                    )?,
                ),
                TurnDeliveryResolutionAction::Dismiss => (
                    "dismissed",
                    last_error,
                    transaction.execute(
                        "UPDATE provider_turn_outbox SET state = 'dismissed', failure_reason = NULL, updated_at = ? WHERE command_id = ? AND state IN ('pending', 'sending', 'uncertain', 'failed')",
                        params![updated_at, delivery_command_id],
                    )?,
                ),
            };
            if updated == 0 {
                return Ok(None);
            }

            let mut payload = json!({
                "threadId": thread_id,
                "messageId": message_id,
                "state": next_state,
                "provider": provider,
                "detail": detail,
                "delivery": delivery_value(next_state, &provider, &provider_instance_id, &mode, held, detail.as_deref()),
                "updatedAt": updated_at,
            });
            if matches!(action, TurnDeliveryResolutionAction::Cancel) {
                payload["withdrawn"] = json!(true);
            }
            let planned = make_event(
                "thread.turn-delivery-updated",
                "thread",
                &thread_id,
                &updated_at,
                &resolution_command_id,
                json!({"deliveryCommandId":delivery_command_id,"deliveryPreviousState":state}),
                payload,
            );
            let saved = append_event_tx(&transaction, planned)?;
            for projector in PROJECTOR_NAMES {
                hooks
                    .maybe_fail(projector, &saved.event.event_type)
                    .map_err(projector_failure_to_persistence)?;
                apply_projector_tx(
                    &transaction,
                    projector,
                    &saved,
                    &mut ProjectionContext::default(),
                )?;
                upsert_projection_state_tx(
                    &transaction,
                    projector,
                    saved.sequence,
                    &saved.event.occurred_at,
                )?;
            }
            rebuild_thread_derived_fields_tx(&transaction, &thread_id)?;
            upsert_command_receipt_tx(
                &transaction,
                CommandReceipt {
                    command_id: resolution_command_id,
                    aggregate_kind: "thread".to_owned(),
                    aggregate_id: thread_id,
                    accepted_at: updated_at,
                    result_sequence: saved.sequence,
                    status: "accepted".to_owned(),
                    error: None,
                    payload_digest,
                },
            )?;
            transaction.commit()?;
            Ok(Some(saved))
        })
        .await
        .map_err(wrap_persistence)?;
    if let Some(event) = &committed {
        let _ = events.send(event.clone());
    }
    Ok(committed)
}

fn turn_delivery_state_name(state: TurnDeliveryState) -> &'static str {
    match state {
        TurnDeliveryState::Queued => "queued",
        TurnDeliveryState::Pending => "pending",
        TurnDeliveryState::Sending => "sending",
        TurnDeliveryState::Delivered => "delivered",
        TurnDeliveryState::Uncertain => "uncertain",
        TurnDeliveryState::Dismissed => "dismissed",
        TurnDeliveryState::Failed => "failed",
    }
}

fn apply_to_model(model: &mut CommandModel, events: &VecDeque<OrchestrationEvent>) {
    for event in events {
        match event.event.event_type.as_str() {
            "project.created" => {
                if let Some(project_id) =
                    event.event.payload.get("projectId").and_then(Value::as_str)
                {
                    model.projects.insert(
                        project_id.to_owned(),
                        ProjectState {
                            workspace_root: event
                                .event
                                .payload
                                .get("workspaceRoot")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            worktree_discovery: event
                                .event
                                .payload
                                .get("worktreeDiscovery")
                                .cloned()
                                .unwrap_or_else(default_worktree_discovery),
                            deleted_at: None,
                        },
                    );
                }
            }
            "project.meta-updated" => {
                if let Some(project) = model.projects.get_mut(&event.event.aggregate_id)
                    && let Some(workspace_root) = event
                        .event
                        .payload
                        .get("workspaceRoot")
                        .and_then(Value::as_str)
                {
                    project.workspace_root = workspace_root.to_owned();
                }
                if let Some(project) = model.projects.get_mut(&event.event.aggregate_id)
                    && let Some(worktree_discovery) = event.event.payload.get("worktreeDiscovery")
                {
                    project.worktree_discovery = worktree_discovery.clone();
                }
            }
            "thread.created" => {
                let payload = &event.event.payload;
                if let Some(thread_id) = payload.get("threadId").and_then(Value::as_str) {
                    model.threads.insert(
                        thread_id.to_owned(),
                        ThreadState {
                            project_id: payload
                                .get("projectId")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            kind: payload
                                .get("kind")
                                .and_then(Value::as_str)
                                .unwrap_or("workspace")
                                .to_owned(),
                            host_thread_id: payload
                                .get("hostThreadId")
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                            runtime_mode: payload
                                .get("runtimeMode")
                                .and_then(Value::as_str)
                                .unwrap_or("full-access")
                                .to_owned(),
                            interaction_mode: payload
                                .get("interactionMode")
                                .and_then(Value::as_str)
                                .unwrap_or("default")
                                .to_owned(),
                            branch: payload
                                .get("branch")
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                            worktree_path: payload
                                .get("worktreePath")
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                            worktree_path_key: payload
                                .get("worktreePath")
                                .and_then(Value::as_str)
                                .map(normalized_worktree_path_key),
                            archived_at: None,
                            deleted_at: None,
                        },
                    );
                }
            }
            "project.deleted" => {
                if let Some(project) = model.projects.get_mut(&event.event.aggregate_id) {
                    project.deleted_at = Some(event.event.occurred_at.clone());
                }
            }
            "thread.deleted" => {
                if let Some(thread) = model.threads.get_mut(&event.event.aggregate_id) {
                    thread.deleted_at = Some(event.event.occurred_at.clone());
                }
            }
            "thread.archived" => {
                if let Some(thread) = model.threads.get_mut(&event.event.aggregate_id) {
                    thread.archived_at = Some(event.event.occurred_at.clone());
                }
            }
            "thread.unarchived" => {
                if let Some(thread) = model.threads.get_mut(&event.event.aggregate_id) {
                    thread.archived_at = None;
                }
            }
            "thread.runtime-mode-set" => {
                if let Some(thread) = model.threads.get_mut(&event.event.aggregate_id)
                    && let Some(value) = event
                        .event
                        .payload
                        .get("runtimeMode")
                        .and_then(Value::as_str)
                {
                    thread.runtime_mode = value.to_owned();
                }
            }
            "thread.interaction-mode-set" => {
                if let Some(thread) = model.threads.get_mut(&event.event.aggregate_id)
                    && let Some(value) = event
                        .event
                        .payload
                        .get("interactionMode")
                        .and_then(Value::as_str)
                {
                    thread.interaction_mode = value.to_owned();
                }
            }
            "thread.meta-updated" => {
                if let Some(thread) = model.threads.get_mut(&event.event.aggregate_id) {
                    if let Some(branch) = event.event.payload.get("branch") {
                        thread.branch = branch.as_str().map(str::to_owned);
                    }
                    if let Some(worktree_path) = event.event.payload.get("worktreePath") {
                        thread.worktree_path = worktree_path.as_str().map(str::to_owned);
                        thread.worktree_path_key =
                            worktree_path.as_str().map(normalized_worktree_path_key);
                    }
                }
            }
            _ => {}
        }
    }
}

async fn bootstrap_projectors(
    repositories: &Repositories,
    hooks: &TestHooks,
) -> Result<(), OrchestrationError> {
    for projector in PROJECTOR_NAMES {
        let start_sequence = repositories
            .get_projection_state(projector.to_owned())
            .await
            .map_err(wrap_persistence)?
            .map(|state| state.last_applied_sequence)
            .unwrap_or(0);
        let events = read_all_events(repositories, start_sequence)
            .await
            .map_err(OrchestrationError::from)?;
        for event in events {
            let database = repositories.database().clone();
            let projector = projector.to_owned();
            let occurred_at = event.event.occurred_at.clone();
            let hooks = hooks.clone();
            database
                .call(move |connection| {
                    let transaction = connection.transaction()?;
                    hooks
                        .maybe_fail(&projector, &event.event.event_type)
                        .map_err(projector_failure_to_persistence)?;
                    apply_projector_tx(
                        &transaction,
                        &projector,
                        &event,
                        &mut ProjectionContext::default(),
                    )?;
                    upsert_projection_state_tx(
                        &transaction,
                        &projector,
                        event.sequence,
                        &occurred_at,
                    )?;
                    if projector == "projection.threads" && event.event.aggregate_kind == "thread" {
                        rebuild_thread_derived_fields_tx(&transaction, &event.event.aggregate_id)?;
                    }
                    transaction.commit()?;
                    Ok(())
                })
                .await
                .map_err(wrap_persistence)?;
        }
    }
    Ok(())
}

async fn read_all_events(
    repositories: &Repositories,
    from_sequence_exclusive: i64,
) -> Result<Vec<OrchestrationEvent>, Arc<PersistenceError>> {
    let mut pages = repositories.event_pages(from_sequence_exclusive);
    let mut all = Vec::new();
    while let Some(page) = pages.next_page().await.map_err(Arc::new)? {
        all.extend(page);
    }
    Ok(all)
}

async fn load_command_model(
    repositories: &Repositories,
) -> Result<CommandModel, OrchestrationError> {
    let mut projects = BTreeMap::new();
    let mut threads = BTreeMap::new();
    for project in repositories
        .list_projects()
        .await
        .map_err(wrap_persistence)?
    {
        let mut worktree_discovery = project.worktree_discovery.clone();
        canonicalize_policy_baseline("engine.start", &mut worktree_discovery).await?;
        projects.insert(
            project.project_id.clone(),
            ProjectState {
                workspace_root: project.workspace_root.clone(),
                worktree_discovery,
                deleted_at: project.deleted_at.clone(),
            },
        );
        for thread in repositories
            .list_threads_by_project(project.project_id.clone())
            .await
            .map_err(wrap_persistence)?
        {
            let worktree_path_key = match thread.worktree_path.as_deref() {
                Some(path) => Some(canonical_worktree_path_key(Path::new(path)).await.map_err(
                    |error| OrchestrationError::WorkspaceOwnershipIdentity {
                        command_id: "engine.start".to_owned(),
                        path: path.to_owned(),
                        detail: error.to_string(),
                    },
                )?),
                None => None,
            };
            threads.insert(
                thread.thread_id.clone(),
                ThreadState {
                    project_id: thread.project_id.clone(),
                    kind: thread.kind.clone(),
                    host_thread_id: thread.host_thread_id.clone(),
                    runtime_mode: thread.runtime_mode.clone(),
                    interaction_mode: thread.interaction_mode.clone(),
                    branch: thread.branch.clone(),
                    worktree_path: thread.worktree_path.clone(),
                    worktree_path_key,
                    archived_at: thread.archived_at.clone(),
                    deleted_at: thread.deleted_at.clone(),
                },
            );
        }
    }
    Ok(CommandModel {
        projects,
        threads,
        project_roots_canonicalized: false,
    })
}

async fn current_max_sequence(repositories: &Repositories) -> Result<i64, OrchestrationError> {
    repositories
        .max_event_sequence()
        .await
        .map_err(wrap_persistence)
}

async fn current_timestamp(database: &Database) -> Result<String, OrchestrationError> {
    database
        .clone()
        .call(|connection| {
            connection
                .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now')", [], |row| {
                    row.get(0)
                })
                .map_err(Into::into)
        })
        .await
        .map_err(wrap_persistence)
}

async fn rebuild_all_thread_derived_fields(database: &Database) -> Result<(), OrchestrationError> {
    let database = database.clone();
    database
        .call(|connection| {
            let thread_ids = {
                let mut statement = connection
                    .prepare("SELECT thread_id FROM projection_threads ORDER BY thread_id ASC")?;
                statement
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };
            let transaction = connection.transaction()?;
            for thread_id in thread_ids {
                rebuild_thread_derived_fields_tx(&transaction, &thread_id)?;
            }
            transaction.commit()?;
            Ok(())
        })
        .await
        .map_err(wrap_persistence)
}

fn apply_projector_tx(
    transaction: &Transaction<'_>,
    projector: &str,
    event: &OrchestrationEvent,
    context: &mut ProjectionContext,
) -> Result<(), PersistenceError> {
    match projector {
        "projection.projects" => apply_projects_projector_tx(transaction, event),
        "projection.thread-messages" => apply_messages_projector_tx(transaction, event, context),
        "projection.thread-proposed-plans" => apply_plans_projector_tx(transaction, event),
        "projection.thread-activities" => apply_activities_projector_tx(transaction, event),
        "projection.thread-sessions" => apply_sessions_projector_tx(transaction, event),
        "projection.thread-turns" => apply_turns_projector_tx(transaction, event, context),
        "projection.checkpoints" => apply_checkpoints_projector_tx(transaction, event),
        "projection.pending-approvals" => apply_pending_approvals_projector_tx(transaction, event),
        "projection.threads" => apply_threads_projector_tx(transaction, event),
        _ => Ok(()),
    }
}

fn apply_projects_projector_tx(
    transaction: &Transaction<'_>,
    event: &OrchestrationEvent,
) -> Result<(), PersistenceError> {
    let payload = &event.event.payload;
    match event.event.event_type.as_str() {
        "project.created" => {
            transaction.execute(
        "INSERT INTO projection_projects (project_id, title, workspace_root, default_model_selection_json, scripts_json, worktree_discovery_json, created_at, updated_at, deleted_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL) \
         ON CONFLICT (project_id) DO UPDATE SET \
           title = excluded.title, workspace_root = excluded.workspace_root, \
           default_model_selection_json = excluded.default_model_selection_json, scripts_json = excluded.scripts_json, worktree_discovery_json = excluded.worktree_discovery_json, \
           created_at = excluded.created_at, updated_at = excluded.updated_at, deleted_at = excluded.deleted_at",
        params![
            required_str(payload, "projectId")?,
            required_str(payload, "title")?,
            required_str(payload, "workspaceRoot")?,
            optional_json_string(payload.get("defaultModelSelection"))?,
            {
                let scripts = payload
                    .get("scripts")
                    .cloned()
                    .unwrap_or_else(|| Value::Array(Vec::new()));
                json_string(&scripts)?
            },
            json_string(
                &payload
                    .get("worktreeDiscovery")
                    .cloned()
                    .unwrap_or_else(default_worktree_discovery),
            )?,
            required_str(payload, "createdAt")?,
            required_str(payload, "updatedAt")?,
        ],
    )?;
        }
        "project.meta-updated" => {
            transaction.execute(
            "UPDATE projection_projects SET title = COALESCE(?, title), workspace_root = COALESCE(?, workspace_root), default_model_selection_json = CASE WHEN ? THEN ? ELSE default_model_selection_json END, scripts_json = COALESCE(?, scripts_json), worktree_discovery_json = CASE WHEN ? THEN ? ELSE worktree_discovery_json END, repository_identity_json = CASE WHEN ? THEN ? ELSE repository_identity_json END, updated_at = ? WHERE project_id = ?",
            params![
                optional_string(payload.get("title")),
                optional_string(payload.get("workspaceRoot")),
                payload.get("defaultModelSelection").is_some(),
                optional_json_string(payload.get("defaultModelSelection"))?,
                payload.get("scripts").map(json_string).transpose()?,
                payload.get("worktreeDiscovery").is_some(),
                payload.get("worktreeDiscovery").map(json_string).transpose()?,
                payload.get("repositoryIdentity").is_some(),
                optional_json_string(payload.get("repositoryIdentity").filter(|value| !value.is_null()))?,
                required_str(payload, "updatedAt")?,
                required_str(payload, "projectId")?,
            ],
        )?;
        }
        "project.deleted" => {
            transaction.execute("UPDATE projection_projects SET deleted_at = ?, updated_at = ? WHERE project_id = ?", params![required_str(payload, "deletedAt")?, required_str(payload, "deletedAt")?, required_str(payload, "projectId")?])?;
        }
        _ => {}
    }
    Ok(())
}

fn apply_threads_projector_tx(
    transaction: &Transaction<'_>,
    event: &OrchestrationEvent,
) -> Result<(), PersistenceError> {
    if event.event.event_type == "thread.created" {
        let payload = &event.event.payload;
        transaction.execute(
            "INSERT INTO projection_threads (thread_id, project_id, title, kind, model_selection_json, runtime_mode, interaction_mode, branch, worktree_path, latest_turn_id, created_at, updated_at, archived_at, latest_user_message_at, pending_approval_count, pending_user_input_count, has_actionable_proposed_plan, deleted_at, host_thread_id) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, NULL, NULL, 0, 0, 0, NULL, ?) \
             ON CONFLICT (thread_id) DO UPDATE SET \
               project_id = excluded.project_id, title = excluded.title, kind = excluded.kind, host_thread_id = excluded.host_thread_id, \
               model_selection_json = excluded.model_selection_json, runtime_mode = excluded.runtime_mode, \
               interaction_mode = excluded.interaction_mode, branch = excluded.branch, worktree_path = excluded.worktree_path, \
               created_at = excluded.created_at, updated_at = excluded.updated_at, archived_at = excluded.archived_at, \
               latest_user_message_at = excluded.latest_user_message_at, pending_approval_count = excluded.pending_approval_count, \
               pending_user_input_count = excluded.pending_user_input_count, has_actionable_proposed_plan = excluded.has_actionable_proposed_plan, deleted_at = excluded.deleted_at",
            params![
                required_str(payload, "threadId")?,
                required_str(payload, "projectId")?,
                required_str(payload, "title")?,
                payload.get("kind").and_then(Value::as_str).unwrap_or("workspace"),
                {
                    let model_selection = payload
                        .get("modelSelection")
                        .cloned()
                        .unwrap_or(Value::Null);
                    json_string(&model_selection)?
                },
                required_str(payload, "runtimeMode")?,
                required_str(payload, "interactionMode")?,
                optional_string(payload.get("branch")),
                optional_string(payload.get("worktreePath")),
                required_str(payload, "createdAt")?,
                required_str(payload, "updatedAt")?,
                optional_string(payload.get("hostThreadId")),
            ],
        )?;
    } else {
        let payload = &event.event.payload;
        match event.event.event_type.as_str() {
            "thread.deleted" => {
                transaction.execute("UPDATE projection_threads SET deleted_at = ?, updated_at = ? WHERE thread_id = ?", params![required_str(payload,"deletedAt")?, required_str(payload,"deletedAt")?, required_str(payload,"threadId")?])?;
                transaction.execute(
                    "DELETE FROM provider_session_runtime WHERE thread_id = ?",
                    [required_str(payload, "threadId")?],
                )?;
            }
            "thread.archived" => {
                transaction.execute("UPDATE projection_threads SET archived_at = ?, updated_at = ? WHERE thread_id = ?", params![required_str(payload,"archivedAt")?, required_str(payload,"updatedAt")?, required_str(payload,"threadId")?])?;
            }
            "thread.unarchived" => {
                transaction.execute("UPDATE projection_threads SET archived_at = NULL, updated_at = ? WHERE thread_id = ?", params![required_str(payload,"updatedAt")?, required_str(payload,"threadId")?])?;
            }
            "thread.meta-updated" => {
                transaction.execute("UPDATE projection_threads SET title = COALESCE(?, title), model_selection_json = COALESCE(?, model_selection_json), branch = CASE WHEN ? THEN ? ELSE branch END, worktree_path = CASE WHEN ? THEN ? ELSE worktree_path END, updated_at = ? WHERE thread_id = ?", params![optional_string(payload.get("title")), payload.get("modelSelection").map(json_string).transpose()?, payload.get("branch").is_some(), optional_string(payload.get("branch")), payload.get("worktreePath").is_some(), optional_string(payload.get("worktreePath")), required_str(payload,"updatedAt")?, required_str(payload,"threadId")?])?;
            }
            "thread.runtime-mode-set" => {
                transaction.execute("UPDATE projection_threads SET runtime_mode = ?, updated_at = ? WHERE thread_id = ?", params![required_str(payload,"runtimeMode")?, required_str(payload,"updatedAt")?, required_str(payload,"threadId")?])?;
            }
            "thread.interaction-mode-set" => {
                transaction.execute("UPDATE projection_threads SET interaction_mode = ?, updated_at = ? WHERE thread_id = ?", params![required_str(payload,"interactionMode")?, required_str(payload,"updatedAt")?, required_str(payload,"threadId")?])?;
            }
            "thread.session-set" => {
                transaction.execute("UPDATE projection_threads SET latest_turn_id = ?, updated_at = ? WHERE thread_id = ?", params![optional_string(payload.pointer("/session/activeTurnId")), event.event.occurred_at, required_str(payload,"threadId")?])?;
            }
            "thread.turn-diff-completed" => {
                transaction.execute("UPDATE projection_threads SET latest_turn_id = ?, updated_at = ? WHERE thread_id = ?", params![required_str(payload,"turnId")?, event.event.occurred_at, required_str(payload,"threadId")?])?;
            }
            _ if event.event.aggregate_kind == "thread" => {
                transaction.execute(
                    "UPDATE projection_threads SET updated_at = ? WHERE thread_id = ?",
                    params![event.event.occurred_at, event.event.aggregate_id],
                )?;
            }
            _ => {}
        }
    }
    if event.event.aggregate_kind == "thread" {
        rebuild_thread_derived_fields_tx(transaction, &event.event.aggregate_id)?;
    }
    Ok(())
}

fn apply_messages_projector_tx(
    transaction: &Transaction<'_>,
    event: &OrchestrationEvent,
    context: &mut ProjectionContext,
) -> Result<(), PersistenceError> {
    if event.event.event_type == "thread.turn-start-requested" {
        let payload = &event.event.payload;
        // Promotion begins a new turn after the preceding response. Apply its
        // timestamp through the event projector so live writes and replay agree;
        // queued receipt order and steered messages keep their enqueue time.
        transaction.execute(
            "UPDATE projection_thread_messages SET created_at = ?, updated_at = ? WHERE thread_id = ? AND message_id = ? AND role = 'user'",
            params![
                required_str(payload, "createdAt")?,
                required_str(payload, "createdAt")?,
                required_str(payload, "threadId")?,
                required_str(payload, "messageId")?,
            ],
        )?;
        return Ok(());
    }
    if event.event.event_type == "thread.reverted" {
        transaction.execute(
            "DELETE FROM projection_thread_messages WHERE thread_id = ? AND turn_id IN (SELECT json_extract(payload_json, '$.turnId') FROM orchestration_events WHERE event_type = 'thread.turn-diff-completed' AND stream_id = ? AND CAST(json_extract(payload_json, '$.checkpointTurnCount') AS INTEGER) > ?)",
            params![required_str(&event.event.payload, "threadId")?, required_str(&event.event.payload, "threadId")?, required_i64(&event.event.payload, "turnCount")?],
        )?;
        return Ok(());
    }
    if event.event.event_type == "thread.turn-delivery-updated" {
        let payload = &event.event.payload;
        if payload.get("withdrawn").and_then(Value::as_bool) == Some(true) {
            transaction.execute(
                "DELETE FROM projection_thread_messages WHERE message_id = ? AND thread_id = ?",
                params![
                    required_str(payload, "messageId")?,
                    required_str(payload, "threadId")?
                ],
            )?;
            return Ok(());
        }
        transaction.execute(
            "UPDATE projection_thread_messages SET delivery_state = ?, delivery_provider = ?, delivery_provider_instance_id = COALESCE(?, delivery_provider_instance_id), delivery_detail = ?, delivery_reason = ?, delivery_mode = COALESCE(?, delivery_mode), delivery_held = COALESCE(?, delivery_held), turn_id = COALESCE(?, turn_id), updated_at = ? WHERE message_id = ? AND thread_id = ?",
            params![
                required_str(payload, "state")?,
                required_str(payload, "provider")?,
                optional_string(payload.pointer("/delivery/providerInstanceId")),
                optional_string(payload.get("detail")),
                optional_string(payload.pointer("/delivery/reason")),
                optional_string(payload.pointer("/delivery/mode").or_else(|| payload.get("mode"))),
                payload.pointer("/delivery/held").or_else(|| payload.get("held")).and_then(Value::as_bool),
                optional_string(payload.get("turnId")),
                required_str(payload, "updatedAt")?,
                required_str(payload, "messageId")?,
                required_str(payload, "threadId")?,
            ],
        )?;
        return Ok(());
    }
    if event.event.event_type != "thread.message-sent" {
        return Ok(());
    }
    let payload = &event.event.payload;
    if context.mode == ProjectionMode::UpdateExistingAssistantMessage {
        context.existing_assistant_message_updated = transaction.execute(
            "UPDATE projection_thread_messages SET is_streaming = 0, updated_at = ? \
             WHERE message_id = ? AND thread_id = ? AND turn_id IS ? \
               AND role = 'assistant' AND is_streaming = 1",
            params![
                required_str(payload, "updatedAt")?,
                required_str(payload, "messageId")?,
                required_str(payload, "threadId")?,
                optional_string(payload.get("turnId")),
            ],
        )? == 1;
        return Ok(());
    }
    transaction.execute(
        "INSERT INTO projection_thread_messages (message_id, thread_id, turn_id, role, text, attachments_json, is_streaming, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT (message_id) DO UPDATE SET \
           thread_id = excluded.thread_id, turn_id = excluded.turn_id, role = excluded.role, \
           text = CASE WHEN excluded.is_streaming = 1 THEN projection_thread_messages.text || excluded.text WHEN excluded.text = '' THEN projection_thread_messages.text ELSE excluded.text END, \
           attachments_json = COALESCE(excluded.attachments_json, projection_thread_messages.attachments_json), \
           is_streaming = excluded.is_streaming, updated_at = excluded.updated_at",
        params![
            required_str(payload, "messageId")?,
            required_str(payload, "threadId")?,
            optional_string(payload.get("turnId")),
            required_str(payload, "role")?,
            required_str(payload, "text")?,
            payload.get("attachments").map(json_string).transpose()?,
            payload.get("streaming").and_then(Value::as_bool).unwrap_or(false),
            required_str(payload, "createdAt")?,
            required_str(payload, "updatedAt")?,
        ],
    )?;
    Ok(())
}

fn streaming_assistant_message_exists_tx(
    transaction: &Transaction<'_>,
    payload: &Value,
) -> Result<bool, PersistenceError> {
    Ok(transaction
        .query_row(
            "SELECT 1 FROM projection_thread_messages \
             WHERE message_id = ? AND thread_id = ? AND turn_id IS ? \
               AND role = 'assistant' AND is_streaming = 1",
            params![
                required_str(payload, "messageId")?,
                required_str(payload, "threadId")?,
                optional_string(payload.get("turnId")),
            ],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn latest_assistant_message_id_tx(
    transaction: &Transaction<'_>,
    thread_id: &str,
    turn_id: &str,
) -> Result<Option<String>, PersistenceError> {
    transaction
        .query_row(
            "SELECT message_id FROM projection_thread_messages \
             WHERE thread_id = ? AND turn_id = ? AND role = 'assistant' \
             ORDER BY created_at DESC, message_id DESC LIMIT 1",
            params![thread_id, turn_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

fn apply_plans_projector_tx(
    transaction: &Transaction<'_>,
    event: &OrchestrationEvent,
) -> Result<(), PersistenceError> {
    if event.event.event_type == "thread.reverted" {
        transaction.execute("DELETE FROM projection_thread_proposed_plans WHERE thread_id = ? AND turn_id IN (SELECT json_extract(payload_json, '$.turnId') FROM orchestration_events WHERE event_type = 'thread.turn-diff-completed' AND stream_id = ? AND CAST(json_extract(payload_json, '$.checkpointTurnCount') AS INTEGER) > ?)", params![required_str(&event.event.payload,"threadId")?, required_str(&event.event.payload,"threadId")?, required_i64(&event.event.payload,"turnCount")?])?;
        return Ok(());
    }
    if event.event.event_type != "thread.proposed-plan-upserted" {
        return Ok(());
    }
    let payload = &event.event.payload;
    let plan = payload
        .get("proposedPlan")
        .ok_or_else(|| PersistenceError::Corrupt("missing proposedPlan payload".to_owned()))?;
    transaction.execute(
        "INSERT INTO projection_thread_proposed_plans (plan_id, thread_id, turn_id, plan_markdown, implemented_at, implementation_thread_id, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT (plan_id) DO UPDATE SET \
           thread_id = excluded.thread_id, turn_id = excluded.turn_id, plan_markdown = excluded.plan_markdown, \
           implemented_at = excluded.implemented_at, implementation_thread_id = excluded.implementation_thread_id, \
           created_at = excluded.created_at, updated_at = excluded.updated_at",
        params![
            required_str(plan, "id")?,
            required_str(payload, "threadId")?,
            optional_string(plan.get("turnId")),
            required_str(plan, "planMarkdown")?,
            optional_string(plan.get("implementedAt")),
            optional_string(plan.get("implementationThreadId")),
            required_str(plan, "createdAt")?,
            required_str(plan, "updatedAt")?,
        ],
    )?;
    Ok(())
}

fn apply_activities_projector_tx(
    transaction: &Transaction<'_>,
    event: &OrchestrationEvent,
) -> Result<(), PersistenceError> {
    if event.event.event_type == "thread.reverted" {
        transaction.execute("DELETE FROM projection_thread_activities WHERE thread_id = ? AND turn_id IN (SELECT json_extract(payload_json, '$.turnId') FROM orchestration_events WHERE event_type = 'thread.turn-diff-completed' AND stream_id = ? AND CAST(json_extract(payload_json, '$.checkpointTurnCount') AS INTEGER) > ?)", params![required_str(&event.event.payload,"threadId")?, required_str(&event.event.payload,"threadId")?, required_i64(&event.event.payload,"turnCount")?])?;
        return Ok(());
    }
    if event.event.event_type != "thread.activity-appended" {
        return Ok(());
    }
    let payload = &event.event.payload;
    let activity = payload
        .get("activity")
        .ok_or_else(|| PersistenceError::Corrupt("missing activity payload".to_owned()))?;
    let thread_id = required_str(payload, "threadId")?;
    let turn_id = optional_string(activity.get("turnId"));
    let kind = required_str(activity, "kind")?;
    let activity_id = required_str(activity, "id")?;
    let is_context_window = kind == "context-window.updated";
    let replaces_context_window = is_context_window
        && activity
            .pointer("/payload/usedTokens")
            .and_then(Value::as_f64)
            .is_some_and(|used_tokens| used_tokens >= 0.0);
    if is_context_window && !replaces_context_window {
        let conflicts_with_valid_context_window = transaction.query_row(
            "SELECT EXISTS(
               SELECT 1 FROM projection_thread_activities
               WHERE activity_id = ?1
                 AND kind = 'context-window.updated'
                 AND json_type(payload_json, '$.usedTokens') IN ('integer', 'real')
                 AND json_extract(payload_json, '$.usedTokens') >= 0
             )",
            [&activity_id],
            |row| row.get::<_, bool>(0),
        )?;
        if conflicts_with_valid_context_window {
            return Ok(());
        }
    }
    if replaces_context_window {
        transaction.execute(
            "DELETE FROM projection_thread_activities
             WHERE thread_id = ?1
               AND turn_id IS ?2
               AND kind = 'context-window.updated'
               AND json_type(payload_json, '$.usedTokens') IN ('integer', 'real')
               AND json_extract(payload_json, '$.usedTokens') >= 0",
            params![thread_id, turn_id],
        )?;
    }
    transaction.execute(
        "INSERT INTO projection_thread_activities (activity_id, thread_id, turn_id, tone, kind, summary, payload_json, sequence, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT (activity_id) DO UPDATE SET \
           thread_id = excluded.thread_id, turn_id = excluded.turn_id, tone = excluded.tone, kind = excluded.kind, \
           summary = excluded.summary, payload_json = excluded.payload_json, sequence = excluded.sequence, created_at = excluded.created_at",
        params![
            activity_id,
            thread_id,
            turn_id,
            required_str(activity, "tone")?,
            kind,
            required_str(activity, "summary")?,
            {
                let activity_payload = activity.get("payload").cloned().unwrap_or(Value::Null);
                json_string(&activity_payload)?
            },
            activity.get("sequence").and_then(Value::as_i64).unwrap_or(event.sequence),
            required_str(activity, "createdAt")?,
        ],
    )?;
    Ok(())
}

fn apply_sessions_projector_tx(
    transaction: &Transaction<'_>,
    event: &OrchestrationEvent,
) -> Result<(), PersistenceError> {
    if event.event.event_type != "thread.session-set" {
        return Ok(());
    }
    let payload = &event.event.payload;
    let session = payload
        .get("session")
        .ok_or_else(|| PersistenceError::Corrupt("missing session payload".to_owned()))?;
    transaction.execute(
        "INSERT INTO projection_thread_sessions (thread_id, status, provider_name, provider_instance_id, runtime_mode, active_turn_id, last_error, last_error_class, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT (thread_id) DO UPDATE SET \
           status = excluded.status, provider_name = excluded.provider_name, provider_instance_id = excluded.provider_instance_id, \
           runtime_mode = excluded.runtime_mode, active_turn_id = excluded.active_turn_id, last_error = excluded.last_error, last_error_class = excluded.last_error_class, updated_at = excluded.updated_at",
        params![
            required_str(payload, "threadId")?,
            required_str(session, "status")?,
            optional_string(session.get("providerName")),
            optional_string(session.get("providerInstanceId")),
            session.get("runtimeMode").and_then(Value::as_str).unwrap_or("full-access"),
            optional_string(session.get("activeTurnId")),
            optional_string(session.get("lastError")),
            optional_string(session.get("lastErrorClass")),
            required_str(session, "updatedAt")?,
        ],
    )?;
    Ok(())
}

fn apply_turns_projector_tx(
    transaction: &Transaction<'_>,
    event: &OrchestrationEvent,
    context: &mut ProjectionContext,
) -> Result<(), PersistenceError> {
    let payload = &event.event.payload;
    match event.event.event_type.as_str() {
        "thread.turn-start-requested" => {
            let thread_id = required_str(payload, "threadId")?;
            transaction.execute(
                "DELETE FROM projection_turns WHERE thread_id = ? AND turn_id IS NULL",
                [&thread_id],
            )?;
            transaction.execute("INSERT INTO projection_turns (thread_id, turn_id, pending_message_id, source_proposed_plan_thread_id, source_proposed_plan_id, assistant_message_id, state, requested_at, started_at, completed_at, checkpoint_turn_count, checkpoint_ref, checkpoint_status, checkpoint_files_json) VALUES (?, NULL, ?, ?, ?, NULL, 'running', ?, NULL, NULL, NULL, NULL, NULL, '[]')", params![thread_id, required_str(payload,"messageId")?, optional_string(payload.pointer("/sourceProposedPlan/threadId")), optional_string(payload.pointer("/sourceProposedPlan/planId")), required_str(payload,"createdAt")?])?;
        }
        // A delivery recorded delivered because the provider already had the message and no turn
        // of it still runs (an explicit retry, or restart recovery with the session idle, found
        // it) started no turn here. Its pending marker would stay running and hold queued
        // follow-ups, so it goes. Only that explicit outcome clears it: an accepted turn keeps its
        // marker until its session update, and so does a found turn that may still run.
        "thread.turn-delivery-updated"
            if event
                .event
                .metadata
                .get("foundWithoutTurn")
                .and_then(Value::as_bool)
                == Some(true) =>
        {
            transaction.execute(
                "DELETE FROM projection_turns WHERE thread_id = ? AND pending_message_id = ? AND turn_id IS NULL AND state = 'running'",
                params![
                    required_str(payload, "threadId")?,
                    required_str(payload, "messageId")?
                ],
            )?;
        }
        "thread.session-set" => {
            let session = payload
                .get("session")
                .ok_or_else(|| PersistenceError::Corrupt("missing session payload".to_owned()))?;
            let thread_id = required_str(payload, "threadId")?;
            let status = required_str(session, "status")?;
            let updated_at = required_str(session, "updatedAt")?;
            if status == "running" {
                if let Some(turn_id) = optional_string(session.get("activeTurnId")) {
                    transaction.execute("UPDATE projection_turns SET state = 'completed', completed_at = ? WHERE thread_id = ? AND turn_id IS NOT NULL AND turn_id <> ? AND state = 'running'", params![updated_at, thread_id, turn_id])?;
                    let existing_turn = transaction
                        .query_row(
                            "SELECT 1 FROM projection_turns WHERE thread_id = ? AND turn_id = ?",
                            params![thread_id, turn_id],
                            |_| Ok(()),
                        )
                        .optional()?
                        .is_some();
                    let updated = if existing_turn {
                        let updated = transaction.execute("UPDATE projection_turns SET state = 'running', started_at = COALESCE(started_at, ?) WHERE thread_id = ? AND turn_id = ?", params![updated_at, thread_id, turn_id])?;
                        transaction.execute(
                            "DELETE FROM projection_turns WHERE thread_id = ? AND turn_id IS NULL",
                            [&thread_id],
                        )?;
                        updated
                    } else {
                        transaction.execute("UPDATE projection_turns SET turn_id = ?, state = 'running', started_at = COALESCE(started_at, ?) WHERE row_id = (SELECT row_id FROM projection_turns WHERE thread_id = ? AND turn_id IS NULL ORDER BY row_id DESC LIMIT 1)", params![turn_id, updated_at, thread_id])?
                    };
                    if updated == 0 {
                        transaction.execute(
                            TURN_UPSERT_SQL,
                            params![
                                thread_id,
                                turn_id,
                                Option::<String>::None,
                                Option::<String>::None,
                                Option::<String>::None,
                                Option::<String>::None,
                                "running",
                                updated_at,
                                Some(updated_at.clone()),
                                Option::<String>::None,
                                Option::<i64>::None,
                                Option::<String>::None,
                                Option::<String>::None,
                                "[]"
                            ],
                        )?;
                    }
                }
            } else {
                let state = if status == "error" {
                    "error"
                } else if status == "interrupted" {
                    "interrupted"
                } else {
                    "completed"
                };
                transaction.execute("UPDATE projection_turns SET state = ?, completed_at = ? WHERE thread_id = ? AND turn_id IS NOT NULL AND state = 'running'", params![state, updated_at, thread_id])?;
            }
        }
        "thread.message-sent" => {
            if required_str(payload, "role")? != "assistant" {
                return Ok(());
            }
            let Some(turn_id) = optional_string(payload.get("turnId")) else {
                return Ok(());
            };
            let thread_id = required_str(payload, "threadId")?;
            if context.mode == ProjectionMode::UpdateExistingAssistantMessage
                && !context.existing_assistant_message_updated
            {
                return Ok(());
            }
            let running = transaction.query_row("SELECT 1 FROM projection_thread_sessions WHERE thread_id = ? AND status = 'running' AND active_turn_id = ?", params![thread_id, turn_id], |_| Ok(())).optional()?.is_some();
            let streaming = payload
                .get("streaming")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let settles = !streaming && !running;
            let updated_at = required_str(payload, "updatedAt")?;
            let created_at = required_str(payload, "createdAt")?;
            let assistant_message_id =
                latest_assistant_message_id_tx(transaction, &thread_id, &turn_id)?
                    .unwrap_or(required_str(payload, "messageId")?);
            let updated = transaction.execute("UPDATE projection_turns SET assistant_message_id = ?, state = CASE WHEN ? THEN CASE WHEN state IN ('interrupted', 'error') THEN state ELSE 'completed' END ELSE state END, started_at = COALESCE(started_at, ?), completed_at = CASE WHEN ? THEN COALESCE(completed_at, ?) ELSE completed_at END WHERE thread_id = ? AND turn_id = ?", params![assistant_message_id.clone(), settles, created_at.clone(), settles, updated_at.clone(), thread_id.clone(), turn_id.clone()])?;
            if updated == 0 {
                transaction.execute(
                    TURN_UPSERT_SQL,
                    params![
                        thread_id,
                        turn_id,
                        Option::<String>::None,
                        Option::<String>::None,
                        Option::<String>::None,
                        Some(assistant_message_id),
                        if settles { "completed" } else { "running" },
                        created_at,
                        Some(created_at.clone()),
                        settles.then_some(updated_at),
                        Option::<i64>::None,
                        Option::<String>::None,
                        Option::<String>::None,
                        "[]"
                    ],
                )?;
            }
        }
        "thread.turn-interrupt-requested" => {
            let Some(turn_id) = optional_string(payload.get("turnId")) else {
                return Ok(());
            };
            let thread_id = required_str(payload, "threadId")?;
            let created_at = required_str(payload, "createdAt")?;
            let updated = transaction.execute("UPDATE projection_turns SET state = 'interrupted', started_at = COALESCE(started_at, ?), completed_at = COALESCE(completed_at, ?) WHERE thread_id = ? AND turn_id = ?", params![created_at.clone(), created_at.clone(), thread_id.clone(), turn_id.clone()])?;
            if updated == 0 {
                transaction.execute(
                    TURN_UPSERT_SQL,
                    params![
                        thread_id,
                        turn_id,
                        Option::<String>::None,
                        Option::<String>::None,
                        Option::<String>::None,
                        Option::<String>::None,
                        "interrupted",
                        created_at,
                        Some(created_at.clone()),
                        Some(created_at.clone()),
                        Option::<i64>::None,
                        Option::<String>::None,
                        Option::<String>::None,
                        "[]"
                    ],
                )?;
            }
        }
        "thread.turn-diff-completed" => {
            let thread_id = required_str(payload, "threadId")?;
            let turn_id = required_str(payload, "turnId")?;
            let completed_at = required_str(payload, "completedAt")?;
            let running = transaction.query_row("SELECT 1 FROM projection_thread_sessions WHERE thread_id = ? AND status = 'running' AND active_turn_id = ?", params![thread_id, turn_id], |_| Ok(())).optional()?.is_some();
            let status = required_str(payload, "status")?;
            let next_state = if running {
                "running"
            } else if status == "error" {
                "error"
            } else {
                "completed"
            };
            let checkpoint_turn_count = required_i64(payload, "checkpointTurnCount")?;
            transaction.execute("UPDATE projection_turns SET checkpoint_turn_count = NULL, checkpoint_ref = NULL, checkpoint_status = NULL, checkpoint_files_json = '[]' WHERE thread_id = ? AND turn_id <> ? AND checkpoint_turn_count = ?", params![thread_id, turn_id, checkpoint_turn_count])?;
            let files = json_string(payload.get("files").unwrap_or(&Value::Array(Vec::new())))?;
            let updated = transaction.execute("UPDATE projection_turns SET assistant_message_id = ?, state = ?, started_at = COALESCE(started_at, ?), completed_at = ?, checkpoint_turn_count = ?, checkpoint_ref = ?, checkpoint_status = ?, checkpoint_files_json = ? WHERE thread_id = ? AND turn_id = ?", params![optional_string(payload.get("assistantMessageId")), next_state, completed_at, completed_at, checkpoint_turn_count, required_str(payload,"checkpointRef")?, status, files, thread_id, turn_id])?;
            if updated == 0 {
                transaction.execute(
                    TURN_UPSERT_SQL,
                    params![
                        thread_id,
                        turn_id,
                        Option::<String>::None,
                        Option::<String>::None,
                        Option::<String>::None,
                        optional_string(payload.get("assistantMessageId")),
                        next_state,
                        completed_at,
                        Some(completed_at.clone()),
                        Some(completed_at.clone()),
                        Some(checkpoint_turn_count),
                        Some(required_str(payload, "checkpointRef")?),
                        Some(status),
                        files
                    ],
                )?;
            }
        }
        "thread.reverted" => {
            transaction.execute(
                "DELETE FROM projection_turns WHERE thread_id = ? AND (checkpoint_turn_count IS NULL OR checkpoint_turn_count > ?)",
                params![
                    required_str(payload, "threadId")?,
                    required_i64(payload, "turnCount")?
                ],
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn apply_checkpoints_projector_tx(
    transaction: &Transaction<'_>,
    event: &OrchestrationEvent,
) -> Result<(), PersistenceError> {
    if event.event.event_type != "thread.checkpoint.saved" {
        return Ok(());
    }
    let payload = &event.event.payload;
    let thread_id = required_str(payload, "threadId")?;
    let turn_id = required_str(payload, "turnId")?;
    let checkpoint_turn_count = required_i64(payload, "checkpointTurnCount")?;
    let completed_at = required_str(payload, "completedAt")?;
    transaction.execute(
        "UPDATE projection_turns SET checkpoint_turn_count = NULL, checkpoint_ref = NULL, checkpoint_status = NULL, checkpoint_files_json = '[]' \
         WHERE thread_id = ? AND checkpoint_turn_count = ?",
        params![thread_id, checkpoint_turn_count],
    )?;
    transaction.execute(
        TURN_UPSERT_SQL,
        params![
            thread_id,
            turn_id,
            Option::<String>::None,
            Option::<String>::None,
            Option::<String>::None,
            optional_string(payload.get("assistantMessageId")),
            if required_str(payload, "status")? == "error" {
                "error".to_owned()
            } else {
                "completed".to_owned()
            },
            completed_at,
            Some(completed_at.clone()),
            Some(completed_at.clone()),
            Some(checkpoint_turn_count),
            Some(required_str(payload, "checkpointRef")?),
            Some(required_str(payload, "status")?),
            {
                let checkpoint_files = payload
                    .get("files")
                    .cloned()
                    .unwrap_or_else(checkpointing::empty_files);
                json_string(&checkpoint_files)?
            },
        ],
    )?;
    let diff = required_str(payload, "diff")?;
    checkpointing::upsert_diff_blob(
        transaction,
        &thread_id,
        checkpoint_turn_count.saturating_sub(1),
        checkpoint_turn_count,
        &diff,
        &completed_at,
    )?;
    Ok(())
}

fn apply_pending_approvals_projector_tx(
    transaction: &Transaction<'_>,
    event: &OrchestrationEvent,
) -> Result<(), PersistenceError> {
    let payload = &event.event.payload;
    let (request_id, thread_id, turn_id, status, decision, created_at, resolved_at) =
        match event.event.event_type.as_str() {
            "thread.approval-response-requested" => (
                required_str(payload, "requestId")?,
                required_str(payload, "threadId")?,
                None,
                "resolved".to_owned(),
                optional_string(payload.get("decision")),
                required_str(payload, "createdAt")?,
                Some(required_str(payload, "createdAt")?),
            ),
            "thread.activity-appended" => {
                let activity = payload.get("activity").ok_or_else(|| {
                    PersistenceError::Corrupt("missing activity payload".to_owned())
                })?;
                let kind = required_str(activity, "kind")?;
                if kind != "approval.requested" && kind != "approval.resolved" {
                    return Ok(());
                }
                let request_id = activity
                    .pointer("/payload/requestId")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        event
                            .event
                            .metadata
                            .get("requestId")
                            .and_then(Value::as_str)
                    })
                    .ok_or_else(|| {
                        PersistenceError::Corrupt("approval activity missing requestId".to_owned())
                    })?
                    .to_owned();
                let created_at = required_str(activity, "createdAt")?;
                let resolved = kind == "approval.resolved";
                (
                    request_id,
                    required_str(payload, "threadId")?,
                    optional_string(activity.get("turnId")),
                    if resolved { "resolved" } else { "pending" }.to_owned(),
                    if resolved {
                        optional_string(activity.pointer("/payload/decision"))
                    } else {
                        None
                    },
                    created_at.clone(),
                    resolved.then_some(created_at),
                )
            }
            _ => return Ok(()),
        };
    let existing = transaction
        .query_row(
            "SELECT thread_id, turn_id, status, created_at FROM projection_pending_approvals WHERE request_id = ?",
            [&request_id],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?)),
        )
        .optional()?;
    if status == "pending" && existing.as_ref().is_some_and(|row| row.2 == "resolved") {
        return Ok(());
    }
    transaction.execute(
        "INSERT INTO projection_pending_approvals (request_id, thread_id, turn_id, status, decision, created_at, resolved_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT (request_id) DO UPDATE SET \
           thread_id = excluded.thread_id, turn_id = excluded.turn_id, status = excluded.status, decision = excluded.decision, \
           created_at = excluded.created_at, resolved_at = excluded.resolved_at",
        params![
            request_id,
            existing.as_ref().map_or(thread_id, |row| row.0.clone()),
            existing.as_ref().and_then(|row| row.1.clone()).or(turn_id),
            status,
            decision,
            existing.as_ref().map_or(created_at, |row| row.3.clone()),
            resolved_at,
        ],
    )?;
    Ok(())
}

fn rebuild_thread_derived_fields_tx(
    transaction: &Transaction<'_>,
    thread_id: &str,
) -> Result<(), PersistenceError> {
    let latest_turn_id = transaction
        .query_row(
            "SELECT turn_id FROM projection_turns WHERE thread_id = ? AND turn_id IS NOT NULL ORDER BY requested_at DESC, turn_id DESC LIMIT 1",
            [thread_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    let latest_user_message_at = transaction
        .query_row(
            "SELECT created_at FROM projection_thread_messages WHERE thread_id = ? AND role = 'user' ORDER BY created_at DESC, message_id DESC LIMIT 1",
            [thread_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    let pending_approval_count = transaction.query_row(
        "SELECT COUNT(*) FROM projection_pending_approvals WHERE thread_id = ? AND status = 'pending'",
        [thread_id],
        |row| row.get::<_, i64>(0),
    )?;
    let has_actionable_proposed_plan = transaction.query_row(
        "SELECT CASE WHEN EXISTS(SELECT 1 FROM projection_thread_proposed_plans WHERE thread_id = ? AND implemented_at IS NULL) THEN 1 ELSE 0 END",
        [thread_id],
        |row| row.get::<_, i64>(0),
    )?;
    // A refused or unknown-fate delivery was only visible inside the open chat.
    // Deriving it here — in the same transaction as the delivery transition that
    // triggered this rebuild — makes it atomic with that transition, keeps the
    // projection engine its owner, and clears it automatically once the row moves
    // to pending, delivered or dismissed. Nothing about the provider session,
    // its active turn, or any turn row is touched.
    let unresolved_delivery = transaction
        .query_row(
            "SELECT state, last_error FROM provider_turn_outbox WHERE thread_id = ? AND state IN ('failed', 'uncertain') ORDER BY updated_at DESC, command_id DESC LIMIT 1",
            [thread_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                ))
            },
        )
        .optional()?;
    let (unresolved_delivery_state, unresolved_delivery_detail) =
        unresolved_delivery.map_or((None, None), |(state, detail)| (Some(state), detail));
    let session_updated_at = transaction
        .query_row(
            "SELECT updated_at FROM projection_thread_sessions WHERE thread_id = ? LIMIT 1",
            [thread_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    let latest_turn_updated_at = transaction
        .query_row(
            "SELECT COALESCE(completed_at, started_at, requested_at) FROM projection_turns WHERE thread_id = ? ORDER BY requested_at DESC, turn_id DESC LIMIT 1",
            [thread_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    let updated_at = session_updated_at
        .or(latest_turn_updated_at)
        .or_else(|| latest_user_message_at.clone());
    transaction.execute(
        "UPDATE projection_threads SET latest_turn_id = ?, latest_user_message_at = ?, pending_approval_count = ?, pending_user_input_count = 0, has_actionable_proposed_plan = ?, unresolved_delivery_state = ?, unresolved_delivery_detail = ?, updated_at = COALESCE(?, updated_at) WHERE thread_id = ?",
        params![
            latest_turn_id,
            latest_user_message_at,
            pending_approval_count,
            has_actionable_proposed_plan,
            unresolved_delivery_state,
            unresolved_delivery_detail,
            updated_at,
            thread_id,
        ],
    )?;
    Ok(())
}

fn append_event_tx(
    transaction: &Transaction<'_>,
    event: NewOrchestrationEvent,
) -> Result<OrchestrationEvent, PersistenceError> {
    Ok(transaction.query_row(
        "INSERT INTO orchestration_events ( \
           event_id, aggregate_kind, stream_id, stream_version, event_type, occurred_at, \
           command_id, causation_event_id, correlation_id, actor_kind, payload_json, metadata_json \
         ) VALUES (?, ?, ?, COALESCE(( \
           SELECT stream_version + 1 FROM orchestration_events \
           WHERE aggregate_kind = ? AND stream_id = ? \
           ORDER BY stream_version DESC LIMIT 1 \
         ), 0), ?, ?, ?, ?, ?, ?, ?, ?) \
         RETURNING sequence, event_id, event_type, aggregate_kind, stream_id, occurred_at, \
           command_id, causation_event_id, correlation_id, payload_json, metadata_json",
        params![
            event.event_id,
            event.aggregate_kind,
            event.aggregate_id,
            event.aggregate_kind,
            event.aggregate_id,
            event.event_type,
            event.occurred_at,
            event.command_id,
            event.causation_event_id,
            event.correlation_id,
            infer_actor_kind(&event),
            json_string(&event.payload)?,
            json_string(&event.metadata)?,
        ],
        decode_event_row,
    )?)
}

fn upsert_command_receipt_tx(
    transaction: &Transaction<'_>,
    receipt: CommandReceipt,
) -> Result<(), PersistenceError> {
    finalize_command_receipt_on(transaction, receipt)
}

fn upsert_projection_state_tx(
    transaction: &Transaction<'_>,
    projector: &str,
    sequence: i64,
    updated_at: &str,
) -> Result<(), PersistenceError> {
    transaction.execute(
        "INSERT INTO projection_state (projector, last_applied_sequence, updated_at) VALUES (?, ?, ?) \
         ON CONFLICT (projector) DO UPDATE SET last_applied_sequence = excluded.last_applied_sequence, updated_at = excluded.updated_at",
        params![projector, sequence, updated_at],
    )?;
    Ok(())
}

fn infer_actor_kind(event: &NewOrchestrationEvent) -> &'static str {
    match event.command_id.as_deref() {
        Some(command_id) if command_id.starts_with("provider:") => "provider",
        Some(command_id) if command_id.starts_with("server:") => "server",
        Some(_) => "client",
        None if event.metadata.get("providerTurnId").is_some()
            || event.metadata.get("providerItemId").is_some()
            || event.metadata.get("adapterKey").is_some() =>
        {
            "provider"
        }
        None => "server",
    }
}

fn decode_event_row(row: &Row<'_>) -> rusqlite::Result<OrchestrationEvent> {
    Ok(OrchestrationEvent {
        sequence: row.get(0)?,
        event: NewOrchestrationEvent {
            event_id: row.get(1)?,
            event_type: row.get(2)?,
            aggregate_kind: row.get(3)?,
            aggregate_id: row.get(4)?,
            occurred_at: row.get(5)?,
            command_id: row.get(6)?,
            causation_event_id: row.get(7)?,
            correlation_id: row.get(8)?,
            payload: serde_json::from_str(&row.get::<_, String>(9)?).map_err(to_sql_error)?,
            metadata: serde_json::from_str(&row.get::<_, String>(10)?).map_err(to_sql_error)?,
        },
    })
}

fn json_string(value: &Value) -> Result<String, PersistenceError> {
    serde_json::to_string(value).map_err(to_corrupt_error)
}

fn default_worktree_discovery() -> Value {
    serde_json::from_str(DEFAULT_WORKTREE_DISCOVERY_JSON)
        .expect("default worktree discovery JSON is valid")
}

fn optional_json_string(value: Option<&Value>) -> Result<Option<String>, PersistenceError> {
    value.map(json_string).transpose()
}

fn optional_string(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::to_owned)
}

fn required_str(payload: &Value, key: &str) -> Result<String, PersistenceError> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| PersistenceError::Corrupt(format!("missing string payload field '{key}'")))
}

fn required_i64(payload: &Value, key: &str) -> Result<i64, PersistenceError> {
    payload
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| PersistenceError::Corrupt(format!("missing integer payload field '{key}'")))
}

fn to_corrupt_error(error: serde_json::Error) -> PersistenceError {
    PersistenceError::Corrupt(format!("could not encode JSON for SQLite TEXT: {error}"))
}

fn to_sql_error(error: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

fn wrap_persistence(error: PersistenceError) -> OrchestrationError {
    if matches!(error, PersistenceError::CommitRejected) {
        return OrchestrationError::Cancelled;
    }
    if let PersistenceError::CommandReceiptConflict(command_id) = &error {
        return OrchestrationError::CommandConflict {
            command_id: command_id.clone(),
        };
    }
    if let PersistenceError::Corrupt(detail) = &error
        && let Some((projector, event_type)) = decode_projector_failure(detail)
    {
        return OrchestrationError::InjectedProjectorFailure {
            projector,
            event_type,
        };
    }
    OrchestrationError::Persistence(Arc::new(error))
}

fn projector_failure_to_persistence(error: OrchestrationError) -> PersistenceError {
    match error {
        OrchestrationError::InjectedProjectorFailure {
            projector,
            event_type,
        } => PersistenceError::Corrupt(format!("__projector_failure__:{projector}:{event_type}")),
        other => PersistenceError::Corrupt(other.to_string()),
    }
}

fn decode_projector_failure(detail: &str) -> Option<(String, String)> {
    let prefix = "__projector_failure__:";
    let remainder = detail.strip_prefix(prefix)?;
    let (projector, event_type) = remainder.split_once(':')?;
    Some((projector.to_owned(), event_type.to_owned()))
}

fn make_event(
    event_type: &str,
    aggregate_kind: &str,
    aggregate_id: &str,
    occurred_at: &str,
    command_id: &str,
    metadata: Value,
    payload: Value,
) -> NewOrchestrationEvent {
    NewOrchestrationEvent {
        event_id: Uuid::new_v4().to_string(),
        event_type: event_type.to_owned(),
        aggregate_kind: aggregate_kind.to_owned(),
        aggregate_id: aggregate_id.to_owned(),
        occurred_at: occurred_at.to_owned(),
        command_id: Some(command_id.to_owned()),
        causation_event_id: None,
        correlation_id: Some(command_id.to_owned()),
        payload,
        metadata,
    }
}

impl OrchestrationCommand {
    pub fn command_type(&self) -> &'static str {
        match self {
            Self::ProjectCreate { .. } => "project.create",
            Self::ProjectMetaUpdate { .. } => "project.meta.update",
            Self::ProjectRepositoryIdentitySet { .. } => "project.repository-identity.set",
            Self::ProjectDelete { .. } => "project.delete",
            Self::WorktreeAdoptResolved { .. } => "worktree.adopt-resolved",
            Self::WorktreeDetachResolved { .. } => "worktree.detach-resolved",
            Self::WorktreeBranchReconcileResolved { .. } => "worktree.branch-reconcile-resolved",
            Self::ThreadCreate { .. } => "thread.create",
            Self::ThreadHistoryImport { .. } => "thread.history.import",
            Self::ThreadDelete { .. } => "thread.delete",
            Self::ThreadArchive { .. } => "thread.archive",
            Self::ThreadUnarchive { .. } => "thread.unarchive",
            Self::ThreadMetaUpdate { .. } => "thread.meta.update",
            Self::ThreadRuntimeModeSet { .. } => "thread.runtime-mode.set",
            Self::ThreadInteractionModeSet { .. } => "thread.interaction-mode.set",
            Self::ThreadTurnStart { .. } => "thread.turn.start",
            Self::ThreadTurnSteer { .. } => "thread.turn.steer",
            Self::ThreadTurnPromote { .. } => "thread.turn.promote",
            Self::ThreadTurnInterrupt { .. } => "thread.turn.interrupt",
            Self::ThreadTurnDeliveryResolve { .. } => "thread.turn-delivery.resolve",
            Self::ThreadApprovalRespond { .. } => "thread.approval.respond",
            Self::ThreadUserInputRespond { .. } => "thread.user-input.respond",
            Self::ThreadCheckpointRevert { .. } => "thread.checkpoint.revert",
            Self::ThreadSessionStop { .. } => "thread.session.stop",
            Self::ThreadSessionSet { .. } => "thread.session.set",
            Self::ThreadMessageAssistantDelta { .. } => "thread.message.assistant.delta",
            Self::ThreadMessageAssistantComplete { .. } => "thread.message.assistant.complete",
            Self::ThreadProposedPlanUpsert { .. } => "thread.proposed-plan.upsert",
            Self::ThreadTurnDiffComplete { .. } => "thread.turn.diff.complete",
            Self::ThreadActivityAppend { .. } => "thread.activity.append",
            Self::ThreadRevertComplete { .. } => "thread.revert.complete",
        }
    }

    pub fn command_id(&self) -> &str {
        match self {
            Self::ProjectCreate { command_id, .. }
            | Self::ProjectMetaUpdate { command_id, .. }
            | Self::ProjectRepositoryIdentitySet { command_id, .. }
            | Self::ProjectDelete { command_id, .. }
            | Self::WorktreeAdoptResolved { command_id, .. }
            | Self::WorktreeDetachResolved { command_id, .. }
            | Self::WorktreeBranchReconcileResolved { command_id, .. }
            | Self::ThreadCreate { command_id, .. }
            | Self::ThreadHistoryImport { command_id, .. }
            | Self::ThreadDelete { command_id, .. }
            | Self::ThreadArchive { command_id, .. }
            | Self::ThreadUnarchive { command_id, .. }
            | Self::ThreadMetaUpdate { command_id, .. }
            | Self::ThreadRuntimeModeSet { command_id, .. }
            | Self::ThreadInteractionModeSet { command_id, .. }
            | Self::ThreadTurnStart { command_id, .. }
            | Self::ThreadTurnSteer { command_id, .. }
            | Self::ThreadTurnPromote { command_id, .. }
            | Self::ThreadTurnInterrupt { command_id, .. }
            | Self::ThreadTurnDeliveryResolve { command_id, .. }
            | Self::ThreadApprovalRespond { command_id, .. }
            | Self::ThreadUserInputRespond { command_id, .. }
            | Self::ThreadCheckpointRevert { command_id, .. }
            | Self::ThreadSessionStop { command_id, .. }
            | Self::ThreadSessionSet { command_id, .. }
            | Self::ThreadMessageAssistantDelta { command_id, .. }
            | Self::ThreadMessageAssistantComplete { command_id, .. }
            | Self::ThreadProposedPlanUpsert { command_id, .. }
            | Self::ThreadTurnDiffComplete { command_id, .. }
            | Self::ThreadActivityAppend { command_id, .. }
            | Self::ThreadRevertComplete { command_id, .. } => command_id,
        }
    }

    pub fn occurred_at(&self) -> Option<&str> {
        match self {
            Self::ProjectMetaUpdate { .. }
            | Self::ProjectRepositoryIdentitySet { .. }
            | Self::ProjectDelete { .. }
            | Self::WorktreeAdoptResolved { .. }
            | Self::WorktreeDetachResolved { .. }
            | Self::WorktreeBranchReconcileResolved { .. }
            | Self::ThreadHistoryImport { .. }
            | Self::ThreadDelete { .. }
            | Self::ThreadArchive { .. }
            | Self::ThreadUnarchive { .. }
            | Self::ThreadMetaUpdate { .. } => None,
            Self::ProjectCreate { created_at, .. }
            | Self::ThreadCreate { created_at, .. }
            | Self::ThreadRuntimeModeSet { created_at, .. }
            | Self::ThreadInteractionModeSet { created_at, .. }
            | Self::ThreadTurnStart { created_at, .. }
            | Self::ThreadTurnSteer { created_at, .. }
            | Self::ThreadTurnPromote { created_at, .. }
            | Self::ThreadTurnInterrupt { created_at, .. }
            | Self::ThreadTurnDeliveryResolve { created_at, .. }
            | Self::ThreadApprovalRespond { created_at, .. }
            | Self::ThreadUserInputRespond { created_at, .. }
            | Self::ThreadCheckpointRevert { created_at, .. }
            | Self::ThreadSessionStop { created_at, .. }
            | Self::ThreadSessionSet { created_at, .. }
            | Self::ThreadMessageAssistantDelta { created_at, .. }
            | Self::ThreadMessageAssistantComplete { created_at, .. }
            | Self::ThreadProposedPlanUpsert { created_at, .. }
            | Self::ThreadTurnDiffComplete { created_at, .. }
            | Self::ThreadActivityAppend { created_at, .. }
            | Self::ThreadRevertComplete { created_at, .. } => Some(created_at),
        }
    }

    pub fn aggregate_ref(&self) -> (&str, &str) {
        match self {
            Self::ProjectCreate { project_id, .. } => ("project", project_id),
            Self::ProjectMetaUpdate { project_id, .. }
            | Self::ProjectRepositoryIdentitySet { project_id, .. }
            | Self::ProjectDelete { project_id, .. } => ("project", project_id),
            Self::WorktreeAdoptResolved { project_id, .. } => ("project", project_id),
            Self::WorktreeDetachResolved { project_id, .. } => ("project", project_id),
            Self::WorktreeBranchReconcileResolved { thread_id, .. } => ("thread", thread_id),
            Self::ThreadCreate { thread_id, .. }
            | Self::ThreadHistoryImport { thread_id, .. }
            | Self::ThreadDelete { thread_id, .. }
            | Self::ThreadArchive { thread_id, .. }
            | Self::ThreadUnarchive { thread_id, .. }
            | Self::ThreadMetaUpdate { thread_id, .. }
            | Self::ThreadRuntimeModeSet { thread_id, .. }
            | Self::ThreadInteractionModeSet { thread_id, .. }
            | Self::ThreadTurnStart { thread_id, .. }
            | Self::ThreadTurnSteer { thread_id, .. }
            | Self::ThreadTurnPromote { thread_id, .. }
            | Self::ThreadTurnInterrupt { thread_id, .. }
            | Self::ThreadTurnDeliveryResolve { thread_id, .. }
            | Self::ThreadApprovalRespond { thread_id, .. }
            | Self::ThreadUserInputRespond { thread_id, .. }
            | Self::ThreadCheckpointRevert { thread_id, .. }
            | Self::ThreadSessionStop { thread_id, .. }
            | Self::ThreadSessionSet { thread_id, .. }
            | Self::ThreadMessageAssistantDelta { thread_id, .. }
            | Self::ThreadMessageAssistantComplete { thread_id, .. }
            | Self::ThreadProposedPlanUpsert { thread_id, .. }
            | Self::ThreadTurnDiffComplete { thread_id, .. }
            | Self::ThreadActivityAppend { thread_id, .. }
            | Self::ThreadRevertComplete { thread_id, .. } => ("thread", thread_id),
        }
    }

    #[must_use]
    pub fn is_server_internal(&self) -> bool {
        matches!(
            self,
            Self::ProjectRepositoryIdentitySet { .. }
                | Self::WorktreeAdoptResolved { .. }
                | Self::WorktreeDetachResolved { .. }
                | Self::WorktreeBranchReconcileResolved { .. }
                | Self::ThreadHistoryImport { .. }
        )
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub projects: Vec<ProjectionProject>,
    pub threads: Vec<ProjectionThread>,
    pub messages: Vec<ProjectionThreadMessage>,
    pub activities: Vec<ProjectionThreadActivity>,
    pub sessions: Vec<ProjectionThreadSession>,
    pub approvals: Vec<ProjectionPendingApproval>,
    pub proposed_plans: Vec<ProjectionThreadProposedPlan>,
    pub turns: Vec<ProjectionTurn>,
    pub checkpoints: Vec<ProjectionCheckpointRow>,
    pub states: Vec<ProjectionState>,
    pub receipts: Vec<CommandReceipt>,
    pub diffs: Vec<CheckpointDiffBlob>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProjectionCheckpointRow {
    pub thread_id: String,
    pub turn_id: String,
    pub checkpoint_turn_count: i64,
    pub checkpoint_ref: String,
    pub status: String,
    pub files: Value,
    pub assistant_message_id: Option<String>,
    pub completed_at: String,
}

pub async fn load_snapshot(repositories: &Repositories) -> Result<Snapshot, OrchestrationError> {
    let projects = repositories
        .list_projects()
        .await
        .map_err(wrap_persistence)?;
    let mut threads = Vec::new();
    let mut messages = Vec::new();
    let mut activities = Vec::new();
    let mut sessions = Vec::new();
    let mut approvals = Vec::new();
    let mut proposed_plans = Vec::new();
    let mut turns = Vec::new();
    let mut checkpoints = Vec::new();
    let mut seen_threads = Vec::new();
    for project in &projects {
        for thread in repositories
            .list_threads_by_project(project.project_id.clone())
            .await
            .map_err(wrap_persistence)?
        {
            seen_threads.push(thread.thread_id.clone());
            messages.extend(
                repositories
                    .list_messages_by_thread(thread.thread_id.clone())
                    .await
                    .map_err(wrap_persistence)?,
            );
            activities.extend(
                repositories
                    .list_activities_by_thread(thread.thread_id.clone())
                    .await
                    .map_err(wrap_persistence)?,
            );
            if let Some(session) = repositories
                .get_thread_session(thread.thread_id.clone())
                .await
                .map_err(wrap_persistence)?
            {
                sessions.push(session);
            }
            approvals.extend(
                repositories
                    .list_pending_approvals_by_thread(thread.thread_id.clone())
                    .await
                    .map_err(wrap_persistence)?,
            );
            proposed_plans.extend(
                repositories
                    .list_proposed_plans_by_thread(thread.thread_id.clone())
                    .await
                    .map_err(wrap_persistence)?,
            );
            turns.extend(
                repositories
                    .list_turns_by_thread(thread.thread_id.clone())
                    .await
                    .map_err(wrap_persistence)?,
            );
            checkpoints.extend(
                repositories
                    .list_checkpoints_by_thread(thread.thread_id.clone())
                    .await
                    .map_err(wrap_persistence)?
                    .into_iter()
                    .map(ProjectionCheckpointRow::from),
            );
            threads.push(thread);
        }
    }
    let states = repositories
        .list_projection_states()
        .await
        .map_err(wrap_persistence)?;
    let receipts = list_receipts(repositories.database()).await?;
    let diffs = list_diffs(repositories.database()).await?;
    Ok(Snapshot {
        projects,
        threads,
        messages,
        activities,
        sessions,
        approvals,
        proposed_plans,
        turns,
        checkpoints,
        states,
        receipts,
        diffs,
    }
    .in_canonical_order())
}

/// Loads only what a shell snapshot shows: projects, the live or archived threads and, for
/// those threads, the latest turn, the session and the rows their conversation previews
/// come from. It runs as one SQLite job, so the read is consistent and short, and it never
/// reads the message and activity history. The shell builder gives the same wire output
/// from it as from [`load_snapshot`].
pub async fn load_shell_snapshot(
    repositories: &Repositories,
    archived: bool,
) -> Result<Snapshot, OrchestrationError> {
    let rows = repositories
        .load_shell_snapshot_rows(archived)
        .await
        .map_err(wrap_persistence)?;
    Ok(Snapshot {
        projects: rows.projects,
        threads: rows.threads,
        messages: rows.preview_messages,
        activities: rows.preview_activities,
        sessions: rows.sessions,
        approvals: Vec::new(),
        proposed_plans: Vec::new(),
        turns: rows.latest_turns,
        checkpoints: Vec::new(),
        states: rows.states,
        receipts: Vec::new(),
        diffs: Vec::new(),
    }
    .in_canonical_order())
}

/// Loads one thread and the rows its detail shows, ordered as [`load_snapshot`] orders
/// them. Like [`load_snapshot`], it leaves out a thread whose project row is missing. It
/// also leaves out a deleted thread, which no detail shows, without reading its rows: a view
/// of a deleted thread keeps asking for it (#113).
pub async fn load_thread_snapshot(
    repositories: &Repositories,
    thread_id: &str,
) -> Result<Snapshot, OrchestrationError> {
    let mut snapshot = Snapshot {
        projects: Vec::new(),
        threads: Vec::new(),
        messages: Vec::new(),
        activities: Vec::new(),
        sessions: Vec::new(),
        approvals: Vec::new(),
        proposed_plans: Vec::new(),
        turns: Vec::new(),
        checkpoints: Vec::new(),
        states: repositories
            .list_projection_states()
            .await
            .map_err(wrap_persistence)?,
        receipts: Vec::new(),
        diffs: Vec::new(),
    };
    let Some(thread) = repositories
        .get_thread(thread_id.to_owned())
        .await
        .map_err(wrap_persistence)?
        .filter(|thread| thread.deleted_at.is_none())
    else {
        return Ok(snapshot);
    };
    if repositories
        .get_project(thread.project_id.clone())
        .await
        .map_err(wrap_persistence)?
        .is_none()
    {
        return Ok(snapshot);
    }
    let thread_id = thread.thread_id.clone();
    snapshot.messages = repositories
        .list_messages_by_thread(thread_id.clone())
        .await
        .map_err(wrap_persistence)?;
    snapshot.activities = repositories
        .list_activities_by_thread(thread_id.clone())
        .await
        .map_err(wrap_persistence)?;
    snapshot.sessions = repositories
        .get_thread_session(thread_id.clone())
        .await
        .map_err(wrap_persistence)?
        .into_iter()
        .collect();
    snapshot.proposed_plans = repositories
        .list_proposed_plans_by_thread(thread_id.clone())
        .await
        .map_err(wrap_persistence)?;
    snapshot.turns = repositories
        .list_turns_by_thread(thread_id.clone())
        .await
        .map_err(wrap_persistence)?;
    snapshot.checkpoints = repositories
        .list_checkpoints_by_thread(thread_id)
        .await
        .map_err(wrap_persistence)?
        .into_iter()
        .map(ProjectionCheckpointRow::from)
        .collect();
    snapshot.threads.push(thread);
    Ok(snapshot.in_canonical_order())
}

impl Snapshot {
    /// The row order every snapshot loader returns, whichever rows it read.
    fn in_canonical_order(mut self) -> Self {
        self.threads
            .sort_by(|left, right| left.thread_id.cmp(&right.thread_id));
        // Repository reads order queued messages by their durable admission sequence.
        // Keep that per-thread order through this stable sort; ordinary messages retain
        // the existing canonical order. The wire snapshot therefore identifies the same
        // queue head as server promotion, even with tied or skewed client timestamps.
        self.messages.sort_by(|left, right| {
            match (
                left.delivery_state.as_deref() == Some("queued"),
                right.delivery_state.as_deref() == Some("queued"),
            ) {
                (true, true) => std::cmp::Ordering::Equal,
                (true, false) => std::cmp::Ordering::Greater,
                (false, true) => std::cmp::Ordering::Less,
                (false, false) => left.message_id.cmp(&right.message_id),
            }
        });
        self.sessions
            .sort_by(|left, right| left.thread_id.cmp(&right.thread_id));
        self.approvals
            .sort_by(|left, right| left.request_id.cmp(&right.request_id));
        self.proposed_plans
            .sort_by(|left, right| left.plan_id.cmp(&right.plan_id));
        self.turns.sort_by(|left, right| {
            left.thread_id
                .cmp(&right.thread_id)
                .then_with(|| left.turn_id.cmp(&right.turn_id))
        });
        self.checkpoints.sort_by(|left, right| {
            left.thread_id
                .cmp(&right.thread_id)
                .then_with(|| left.checkpoint_turn_count.cmp(&right.checkpoint_turn_count))
        });
        self
    }
}

impl From<crate::persistence::ProjectionCheckpoint> for ProjectionCheckpointRow {
    fn from(checkpoint: crate::persistence::ProjectionCheckpoint) -> Self {
        Self {
            thread_id: checkpoint.thread_id,
            turn_id: checkpoint.turn_id,
            checkpoint_turn_count: checkpoint.checkpoint_turn_count,
            checkpoint_ref: checkpoint.checkpoint_ref,
            status: checkpoint.status,
            files: checkpoint.files,
            assistant_message_id: checkpoint.assistant_message_id,
            completed_at: checkpoint.completed_at,
        }
    }
}

async fn list_receipts(database: &Database) -> Result<Vec<CommandReceipt>, OrchestrationError> {
    database
        .clone()
        .call(|connection| {
            let mut statement = connection.prepare(
                "SELECT command_id, aggregate_kind, aggregate_id, accepted_at, result_sequence, status, error, payload_digest \
                 FROM orchestration_command_receipts ORDER BY accepted_at ASC, command_id ASC",
            )?;
            statement
                .query_map([], |row| {
                    Ok(CommandReceipt {
                        command_id: row.get(0)?,
                        aggregate_kind: row.get(1)?,
                        aggregate_id: row.get(2)?,
                        accepted_at: row.get(3)?,
                        result_sequence: row.get(4)?,
                        status: row.get(5)?,
                        error: row.get(6)?,
                        payload_digest: row.get(7)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(Into::into)
        })
        .await
        .map_err(wrap_persistence)
}

async fn list_diffs(database: &Database) -> Result<Vec<CheckpointDiffBlob>, OrchestrationError> {
    database
        .clone()
        .call(|connection| {
            let mut statement = connection.prepare(
                "SELECT thread_id, from_turn_count, to_turn_count, diff, created_at FROM checkpoint_diff_blobs ORDER BY thread_id ASC, to_turn_count ASC",
            )?;
            statement
                .query_map([], |row| {
                    Ok(CheckpointDiffBlob {
                        thread_id: row.get(0)?,
                        from_turn_count: row.get(1)?,
                        to_turn_count: row.get(2)?,
                        diff: row.get(3)?,
                        created_at: row.get(4)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(Into::into)
        })
        .await
        .map_err(wrap_persistence)
}

#[cfg(test)]
mod tests {
    use crate::orchestration::{
        AttachmentReference, CommandAdmission, NewProviderTurnDelivery, TurnDeliveryState,
        TurnDeliveryTransition, canonical_command_digest,
    };
    use crate::persistence::run_migrations;
    use tempfile::TempDir;

    use super::*;

    mod worktree_detach {
        use super::*;

        const PROJECT_ID: &str = "detach-project";
        const PATH: &str = if cfg!(windows) {
            r"C:\Repo\External"
        } else {
            "/repo/external"
        };

        async fn detach_engine() -> OrchestrationEngine {
            let database = Database::open_in_memory().await.expect("database");
            database
                .call(|connection| {
                    run_migrations(connection, None)?;
                    Ok(())
                })
                .await
                .expect("migrations");
            let engine = OrchestrationEngine::start(database, EngineOptions::default())
                .await
                .expect("engine");
            engine
                .dispatch(project_create_command("detach-project-create", PROJECT_ID))
                .await
                .expect("project");
            engine
                .dispatch(OrchestrationCommand::ProjectMetaUpdate {
                    command_id: "detach-policy".to_owned(),
                    project_id: PROJECT_ID.to_owned(),
                    title: None,
                    workspace_root: None,
                    default_model_selection: OptionalNullable::Missing,
                    scripts: None,
                    worktree_discovery: Some(json!({
                        "visibility": "shown",
                        "initialPromptDismissedAt": null,
                        "baselinePaths": [PATH, "/repo/other"]
                    })),
                })
                .await
                .expect("policy");
            engine
        }

        async fn create_thread(
            engine: &OrchestrationEngine,
            command_id: &str,
            thread_id: &str,
            kind: &str,
            path: &str,
        ) {
            engine
                .dispatch(OrchestrationCommand::ThreadCreate {
                    command_id: command_id.to_owned(),
                    thread_id: thread_id.to_owned(),
                    project_id: PROJECT_ID.to_owned(),
                    title: thread_id.to_owned(),
                    kind: Some(kind.to_owned()),
                    host_thread_id: None,
                    model_selection: json!({"instanceId":"codex","model":"gpt-5"}),
                    runtime_mode: "full-access".to_owned(),
                    interaction_mode: "default".to_owned(),
                    branch: Some("feature/detach".to_owned()),
                    worktree_path: Some(path.to_owned()),
                    created_at: "2026-08-09T00:00:01Z".to_owned(),
                })
                .await
                .expect("thread");
        }

        fn detach(command_id: &str) -> OrchestrationCommand {
            OrchestrationCommand::WorktreeDetachResolved {
                command_id: command_id.to_owned(),
                project_id: PROJECT_ID.to_owned(),
                thread_id: "workspace-owner".to_owned(),
                path: PATH.to_owned(),
                git_outcome: "not-requested".to_owned(),
                detail: None,
                orphan_cleanup_pending: false,
            }
        }

        #[tokio::test]
        async fn removal_runtime_cleanup_covers_detach_and_generic_delete() {
            for detach_owner in [true, false] {
                let engine = detach_engine().await;
                for (id, kind, path, status) in [
                    ("workspace-owner", "workspace", PATH, "ready"),
                    ("panel-a", "panel", PATH, "suspended"),
                    ("panel-b", "panel", PATH, "running"),
                    ("other-panel", "panel", "/repo/other", "suspended"),
                ] {
                    create_thread(&engine, &format!("create-{id}"), id, kind, path).await;
                    engine
                        .repositories()
                        .upsert_provider_session_runtime(
                            crate::persistence::ProviderSessionRuntime {
                                thread_id: id.into(),
                                provider_name: "codex".into(),
                                provider_instance_id: Some("codex".into()),
                                adapter_key: "codex-app-server".into(),
                                runtime_mode: "full-access".into(),
                                status: status.into(),
                                last_seen_at: "2026-09-27T00:00:00Z".into(),
                                resume_cursor: Some(json!({"threadId":id})),
                                runtime_payload: None,
                            },
                        )
                        .await
                        .unwrap();
                }
                let command = if detach_owner {
                    detach("remove-runtime")
                } else {
                    OrchestrationCommand::ThreadDelete {
                        command_id: "remove-runtime".into(),
                        thread_id: "panel-a".into(),
                    }
                };
                engine.dispatch(command).await.unwrap();
                let rows = engine
                    .repositories()
                    .list_provider_session_runtimes()
                    .await
                    .unwrap();
                let ids = rows
                    .iter()
                    .map(|row| row.thread_id.as_str())
                    .collect::<Vec<_>>();
                assert_eq!(
                    ids,
                    if detach_owner {
                        vec!["other-panel"]
                    } else {
                        vec!["other-panel", "panel-b", "workspace-owner"]
                    }
                );
                assert_eq!(
                    rows[0].resume_cursor,
                    Some(json!({"threadId":"other-panel"}))
                );
                engine.shutdown().await;
            }
        }

        #[tokio::test]
        async fn guarded_suspension_write_requires_a_live_thread() {
            let engine = detach_engine().await;
            for id in ["deleted-panel", "live-panel"] {
                create_thread(&engine, &format!("create-{id}"), id, "panel", PATH).await;
            }
            engine
                .dispatch(OrchestrationCommand::ThreadDelete {
                    command_id: "delete-panel".into(),
                    thread_id: "deleted-panel".into(),
                })
                .await
                .unwrap();
            let repositories = engine.repositories();
            for (id, exists) in [
                ("deleted-panel", false),
                ("absent-panel", false),
                ("live-panel", true),
            ] {
                for cursor in ["first", "updated"] {
                    let written = repositories
                        .upsert_provider_session_runtime_if_thread_live(
                            crate::persistence::ProviderSessionRuntime {
                                thread_id: id.into(),
                                provider_name: "codex".into(),
                                provider_instance_id: Some("codex".into()),
                                adapter_key: "codex-app-server".into(),
                                runtime_mode: "full-access".into(),
                                status: "suspended".into(),
                                last_seen_at: "2026-09-27T00:00:00Z".into(),
                                resume_cursor: Some(json!({"threadId":cursor})),
                                runtime_payload: None,
                            },
                        )
                        .await
                        .unwrap();
                    assert_eq!(written, exists);
                    let row = repositories
                        .get_provider_session_runtime(id.into())
                        .await
                        .unwrap();
                    assert_eq!(row.is_some(), exists, "guarded write for {id}");
                    if let Some(row) = row {
                        assert_eq!(row.resume_cursor, Some(json!({"threadId":cursor})));
                    }
                }
            }
            let mut stale = repositories
                .get_provider_session_runtime("live-panel".into())
                .await
                .unwrap()
                .unwrap();
            stale.thread_id = "deleted-panel".into();
            repositories
                .upsert_provider_session_runtime(stale.clone())
                .await
                .unwrap();
            stale.resume_cursor = Some(json!({"threadId":"must-not-overwrite"}));
            assert!(
                !repositories
                    .upsert_provider_session_runtime_if_thread_live(stale)
                    .await
                    .unwrap()
            );
            assert_eq!(
                repositories
                    .get_provider_session_runtime("deleted-panel".into())
                    .await
                    .unwrap()
                    .unwrap()
                    .resume_cursor,
                Some(json!({"threadId":"updated"}))
            );
            engine.shutdown().await;
        }

        #[tokio::test]
        async fn worktree_detach_deletes_panels_owner_and_compacts_baseline_atomically() {
            let engine = detach_engine().await;
            create_thread(
                &engine,
                "workspace-create",
                "workspace-owner",
                "workspace",
                PATH,
            )
            .await;
            create_thread(&engine, "panel-b-create", "panel-b", "panel", PATH).await;
            create_thread(&engine, "panel-a-create", "panel-a", "panel", PATH).await;
            create_thread(
                &engine,
                "other-panel-create",
                "other-panel",
                "panel",
                "/repo/other",
            )
            .await;

            let command = detach("detach-atomic");
            assert!(command.is_server_internal());
            let accepted = engine.dispatch(command.clone()).await.expect("detach");
            let snapshot = load_snapshot(&engine.repositories())
                .await
                .expect("snapshot");
            for thread_id in ["panel-a", "panel-b", "workspace-owner"] {
                assert!(
                    snapshot
                        .threads
                        .iter()
                        .find(|thread| thread.thread_id == thread_id)
                        .is_some_and(|thread| thread.deleted_at.is_some()),
                    "{thread_id} is deleted"
                );
            }
            assert!(
                snapshot
                    .threads
                    .iter()
                    .find(|thread| thread.thread_id == "other-panel")
                    .is_some_and(|thread| thread.deleted_at.is_none())
            );
            assert_eq!(
                snapshot.projects[0].worktree_discovery,
                json!({
                    "visibility": "shown",
                    "initialPromptDismissedAt": null,
                    "baselinePaths": ["/repo/other"]
                })
            );
            let events = engine
                .read_events(0)
                .await
                .expect("events")
                .into_iter()
                .filter(|event| event.event.command_id.as_deref() == Some("detach-atomic"))
                .map(|event| (event.event.event_type, event.event.aggregate_id))
                .collect::<Vec<_>>();
            assert_eq!(
                events,
                vec![
                    ("thread.deleted".to_owned(), "panel-a".to_owned()),
                    ("thread.deleted".to_owned(), "panel-b".to_owned()),
                    ("thread.deleted".to_owned(), "workspace-owner".to_owned()),
                    ("project.meta-updated".to_owned(), PROJECT_ID.to_owned()),
                ]
            );

            let event_count = engine.read_events(0).await.expect("events").len();
            assert_eq!(engine.dispatch(command).await.expect("retry"), accepted);
            assert_eq!(
                engine.read_events(0).await.expect("events").len(),
                event_count
            );
            engine.shutdown().await;
        }

        #[tokio::test]
        async fn thread_delete_cascades_to_live_and_archived_host_panels() {
            let engine = detach_engine().await;
            for (id, kind, host) in [
                ("host", "workspace", None),
                ("other-host", "workspace", None),
                ("panel-live", "panel", Some("host")),
                ("panel-archived", "panel", Some("host")),
                ("panel-gone", "panel", Some("host")),
                ("other-panel", "panel", Some("other-host")),
            ] {
                engine
                    .dispatch(OrchestrationCommand::ThreadCreate {
                        command_id: format!("create-{id}"),
                        thread_id: id.to_owned(),
                        project_id: PROJECT_ID.to_owned(),
                        title: id.to_owned(),
                        kind: Some(kind.to_owned()),
                        host_thread_id: host.map(str::to_owned),
                        model_selection: json!({"instanceId":"codex","model":"gpt-5"}),
                        runtime_mode: "full-access".to_owned(),
                        interaction_mode: "default".to_owned(),
                        branch: None,
                        worktree_path: None,
                        created_at: "2026-10-08T00:00:01Z".to_owned(),
                    })
                    .await
                    .expect("thread");
            }
            engine
                .dispatch(OrchestrationCommand::ThreadArchive {
                    command_id: "archive-panel".into(),
                    thread_id: "panel-archived".into(),
                })
                .await
                .expect("archive");
            engine
                .dispatch(OrchestrationCommand::ThreadDelete {
                    command_id: "delete-panel-gone".into(),
                    thread_id: "panel-gone".into(),
                })
                .await
                .expect("delete panel");
            engine
                .repositories()
                .upsert_provider_session_runtime(crate::persistence::ProviderSessionRuntime {
                    thread_id: "panel-archived".into(),
                    provider_name: "codex".into(),
                    provider_instance_id: Some("codex".into()),
                    adapter_key: "codex-app-server".into(),
                    runtime_mode: "full-access".into(),
                    status: "suspended".into(),
                    last_seen_at: "2026-10-08T00:00:02Z".into(),
                    resume_cursor: Some(json!({"threadId":"panel-archived"})),
                    runtime_payload: None,
                })
                .await
                .unwrap();

            engine
                .dispatch(OrchestrationCommand::ThreadDelete {
                    command_id: "delete-host".into(),
                    thread_id: "host".into(),
                })
                .await
                .expect("delete host");

            let events = engine
                .read_events(0)
                .await
                .expect("events")
                .into_iter()
                .filter(|event| event.event.command_id.as_deref() == Some("delete-host"))
                .map(|event| (event.event.event_type, event.event.aggregate_id))
                .collect::<Vec<_>>();
            assert_eq!(
                events,
                vec![
                    ("thread.deleted".to_owned(), "panel-archived".to_owned()),
                    ("thread.deleted".to_owned(), "panel-live".to_owned()),
                    ("thread.deleted".to_owned(), "host".to_owned()),
                ]
            );
            let snapshot = load_snapshot(&engine.repositories())
                .await
                .expect("snapshot");
            for (thread_id, deleted) in [
                ("panel-archived", true),
                ("panel-live", true),
                ("host", true),
                ("other-host", false),
                ("other-panel", false),
            ] {
                assert_eq!(
                    snapshot
                        .threads
                        .iter()
                        .find(|thread| thread.thread_id == thread_id)
                        .map(|thread| thread.deleted_at.is_some()),
                    Some(deleted),
                    "{thread_id} deleted"
                );
            }
            assert!(
                engine
                    .repositories()
                    .get_provider_session_runtime("panel-archived".into())
                    .await
                    .unwrap()
                    .is_none()
            );
            engine.shutdown().await;
        }

        #[tokio::test]
        async fn generic_delete_fails_closed_for_an_adopted_owner_and_its_project() {
            let engine = detach_engine().await;
            create_thread(
                &engine,
                "generic-delete-owner-create",
                "workspace-owner",
                "workspace",
                PATH,
            )
            .await;
            create_thread(
                &engine,
                "generic-delete-panel-create",
                "owner-panel",
                "panel",
                PATH,
            )
            .await;

            let owner_delete = engine
                .dispatch(OrchestrationCommand::ThreadDelete {
                    command_id: "generic-delete-owner".to_owned(),
                    thread_id: "workspace-owner".to_owned(),
                })
                .await;
            assert!(
                matches!(owner_delete, Err(OrchestrationError::Invariant { .. })),
                "generic deletion cannot retire an adopted owner: {owner_delete:?}"
            );

            let project_delete = engine
                .dispatch(OrchestrationCommand::ProjectDelete {
                    command_id: "generic-delete-owner-project".to_owned(),
                    project_id: PROJECT_ID.to_owned(),
                    force: Some(true),
                })
                .await;
            assert!(
                matches!(project_delete, Err(OrchestrationError::Invariant { .. })),
                "force cannot bypass exact adopted-owner retirement: {project_delete:?}"
            );

            engine
                .dispatch(OrchestrationCommand::ThreadDelete {
                    command_id: "generic-delete-panel".to_owned(),
                    thread_id: "owner-panel".to_owned(),
                })
                .await
                .expect("a dependent panel remains ordinary non-owner orchestration");
            let snapshot = load_snapshot(&engine.repositories())
                .await
                .expect("snapshot");
            assert!(
                snapshot
                    .threads
                    .iter()
                    .find(|thread| thread.thread_id == "workspace-owner")
                    .is_some_and(|thread| thread.deleted_at.is_none())
            );
            assert!(
                snapshot
                    .projects
                    .iter()
                    .find(|project| project.project_id == PROJECT_ID)
                    .is_some_and(|project| project.deleted_at.is_none())
            );
            engine.shutdown().await;
        }

        #[tokio::test]
        async fn worktree_detach_accepts_an_archived_owner_and_rejects_a_second_owner() {
            let archived = detach_engine().await;
            create_thread(
                &archived,
                "archived-create",
                "workspace-owner",
                "workspace",
                PATH,
            )
            .await;
            archived
                .dispatch(OrchestrationCommand::ThreadArchive {
                    command_id: "archive-owner".to_owned(),
                    thread_id: "workspace-owner".to_owned(),
                })
                .await
                .expect("archive");
            archived
                .dispatch(detach("detach-archived"))
                .await
                .expect("archived detach");
            assert!(
                archived
                    .repositories()
                    .get_thread("workspace-owner".to_owned())
                    .await
                    .expect("thread")
                    .is_some_and(|thread| thread.deleted_at.is_some())
            );
            archived.shutdown().await;

            let conflict = detach_engine().await;
            create_thread(
                &conflict,
                "first-create",
                "workspace-owner",
                "workspace",
                PATH,
            )
            .await;
            create_thread(
                &conflict,
                "second-create",
                "workspace-second",
                "workspace",
                PATH,
            )
            .await;
            assert!(matches!(
                conflict.dispatch(detach("detach-conflict")).await,
                Err(OrchestrationError::WorktreeOwnershipConflict { owner_count: 2, .. })
            ));
            assert_eq!(
                conflict
                    .repositories()
                    .list_threads_by_project(PROJECT_ID.to_owned())
                    .await
                    .expect("threads")
                    .into_iter()
                    .filter(|thread| thread.kind == "workspace" && thread.deleted_at.is_none())
                    .count(),
                2
            );
            conflict.shutdown().await;
        }
    }

    mod worktree_adoption {
        use super::*;

        const PROJECT_ID: &str = "adoption-project";
        const PATH: &str = if cfg!(windows) {
            r"C:\Repo\External"
        } else {
            "/repo/external"
        };

        async fn adoption_engine(hooks: TestHooks) -> OrchestrationEngine {
            let database = Database::open_in_memory().await.expect("database");
            database
                .call(|connection| {
                    run_migrations(connection, None)?;
                    Ok(())
                })
                .await
                .expect("migrations");
            let engine = OrchestrationEngine::start(
                database,
                EngineOptions {
                    test_hooks: hooks,
                    ..EngineOptions::default()
                },
            )
            .await
            .expect("engine");
            engine
                .dispatch(project_create_command(
                    "adoption-project-create",
                    PROJECT_ID,
                ))
                .await
                .expect("project");
            engine
                .dispatch(OrchestrationCommand::ProjectMetaUpdate {
                    command_id: "adoption-policy".to_owned(),
                    project_id: PROJECT_ID.to_owned(),
                    title: None,
                    workspace_root: None,
                    default_model_selection: OptionalNullable::Missing,
                    scripts: None,
                    worktree_discovery: Some(json!({
                        "visibility": "shown",
                        "initialPromptDismissedAt": "2026-08-09T00:00:00Z",
                        "baselinePaths": [PATH, "/repo/other"]
                    })),
                })
                .await
                .expect("policy");
            engine
        }

        fn adopt(
            command_id: &str,
            branch: Option<&str>,
            head: Option<&str>,
        ) -> OrchestrationCommand {
            OrchestrationCommand::WorktreeAdoptResolved {
                command_id: command_id.to_owned(),
                project_id: PROJECT_ID.to_owned(),
                worktree_key: "worktree-key".to_owned(),
                path: PATH.to_owned(),
                branch: branch.map(str::to_owned),
                head: head.map(str::to_owned),
                model_selection: json!({"instanceId":"codex","model":"gpt-5"}),
                runtime_mode: "full-access".to_owned(),
                interaction_mode: "plan".to_owned(),
            }
        }

        fn detach_owner(command_id: &str, thread_id: &str) -> OrchestrationCommand {
            OrchestrationCommand::WorktreeDetachResolved {
                command_id: command_id.to_owned(),
                project_id: PROJECT_ID.to_owned(),
                thread_id: thread_id.to_owned(),
                path: PATH.to_owned(),
                git_outcome: "not-requested".to_owned(),
                detail: None,
                orphan_cleanup_pending: false,
            }
        }

        async fn create_thread(
            engine: &OrchestrationEngine,
            command_id: &str,
            thread_id: &str,
            kind: &str,
        ) {
            create_thread_at(engine, command_id, thread_id, kind, PATH).await;
        }

        async fn create_thread_at(
            engine: &OrchestrationEngine,
            command_id: &str,
            thread_id: &str,
            kind: &str,
            worktree_path: &str,
        ) {
            engine
                .dispatch(OrchestrationCommand::ThreadCreate {
                    command_id: command_id.to_owned(),
                    thread_id: thread_id.to_owned(),
                    project_id: PROJECT_ID.to_owned(),
                    title: thread_id.to_owned(),
                    kind: Some(kind.to_owned()),
                    host_thread_id: None,
                    model_selection: json!({"instanceId":"codex","model":"gpt-5"}),
                    runtime_mode: "full-access".to_owned(),
                    interaction_mode: "default".to_owned(),
                    branch: Some("old".to_owned()),
                    worktree_path: Some(worktree_path.to_owned()),
                    created_at: "2026-08-09T00:00:01Z".to_owned(),
                })
                .await
                .expect("thread");
        }

        #[tokio::test]
        async fn repository_identity_set_updates_projection_without_touching_updated_at() {
            let engine = adoption_engine(TestHooks::default()).await;
            let before = engine
                .repositories()
                .get_project(PROJECT_ID.into())
                .await
                .unwrap()
                .unwrap();
            let identity = json!({"canonicalKey":"github.com/acme/repo","locator":{"source":"git-remote","remoteName":"origin","remoteUrl":"git@github.com:acme/repo.git"},"name":"repo"});
            engine
                .dispatch(OrchestrationCommand::ProjectRepositoryIdentitySet {
                    command_id: "identity-1".into(),
                    project_id: PROJECT_ID.into(),
                    repository_identity: Some(identity.clone()),
                })
                .await
                .unwrap();
            let after = engine
                .repositories()
                .get_project(PROJECT_ID.into())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(after.repository_identity, Some(identity));
            assert_eq!(after.updated_at, before.updated_at);

            engine
                .dispatch(OrchestrationCommand::ProjectRepositoryIdentitySet {
                    command_id: "identity-2".into(),
                    project_id: PROJECT_ID.into(),
                    repository_identity: None,
                })
                .await
                .unwrap();
            let cleared = engine
                .repositories()
                .get_project(PROJECT_ID.into())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(cleared.repository_identity, None);
        }

        #[test]
        fn repository_identity_set_is_server_internal() {
            let command: OrchestrationCommand = serde_json::from_value(json!({
                "type":"project.repository-identity.set",
                "commandId":"c",
                "projectId":"p",
                "repositoryIdentity":null
            }))
            .unwrap();
            assert!(command.is_server_internal());
        }

        #[tokio::test]
        async fn adoption_joins_the_same_physical_workspace_through_a_filesystem_alias() {
            let engine = adoption_engine(TestHooks::default()).await;
            let root = tempfile::tempdir().expect("physical workspace parent");
            let physical = root.path().join("physical-worktree");
            std::fs::create_dir(&physical).expect("physical worktree");
            let alias = root.path().join("worktree-alias");
            #[cfg(unix)]
            std::os::unix::fs::symlink(&physical, &alias).expect("worktree alias");
            #[cfg(windows)]
            junction::create(&physical, &alias).expect("worktree junction alias");
            #[cfg(windows)]
            let alias_path = alias.clone();
            let physical = physical.to_string_lossy().into_owned();
            let alias = alias.to_string_lossy().into_owned();
            create_thread_at(
                &engine,
                "physical-owner-create",
                "physical-owner",
                "workspace",
                &physical,
            )
            .await;
            engine
                .dispatch(OrchestrationCommand::ProjectMetaUpdate {
                    command_id: "physical-alias-policy".to_owned(),
                    project_id: PROJECT_ID.to_owned(),
                    title: None,
                    workspace_root: None,
                    default_model_selection: OptionalNullable::Missing,
                    scripts: None,
                    worktree_discovery: Some(json!({
                        "visibility": "shown",
                        "initialPromptDismissedAt": "2026-08-09T00:00:00Z",
                        "baselinePaths": [alias]
                    })),
                })
                .await
                .expect("alias policy");

            let result = engine
                .dispatch(OrchestrationCommand::WorktreeAdoptResolved {
                    command_id: "adopt-physical-alias".to_owned(),
                    project_id: PROJECT_ID.to_owned(),
                    worktree_key: "physical-alias-key".to_owned(),
                    path: alias,
                    branch: Some("feature/physical".to_owned()),
                    head: Some("abc1234".to_owned()),
                    model_selection: json!({"instanceId":"codex","model":"gpt-5"}),
                    runtime_mode: "full-access".to_owned(),
                    interaction_mode: "plan".to_owned(),
                })
                .await
                .expect("adopt alias");

            assert_eq!(result.thread_id.as_deref(), Some("physical-owner"));
            assert_eq!(result.disposition.as_deref(), Some("existing"));
            let threads = engine
                .repositories()
                .list_threads_by_project(PROJECT_ID.to_owned())
                .await
                .expect("threads");
            let owner = threads
                .iter()
                .find(|thread| thread.thread_id == "physical-owner")
                .expect("physical owner");
            assert_eq!(owner.worktree_path.as_deref(), Some(physical.as_str()));
            assert_eq!(
                threads
                    .iter()
                    .filter(|thread| thread.kind == "workspace" && thread.deleted_at.is_none())
                    .count(),
                1,
                "one physical checkout must have one durable workspace owner"
            );
            engine.shutdown().await;
            #[cfg(windows)]
            junction::delete(&alias_path).expect("remove worktree junction alias");
        }

        async fn replace_adoption_result_metadata(
            engine: &OrchestrationEngine,
            command_id: &str,
            adoption_result: Option<Value>,
        ) {
            let command_id = command_id.to_owned();
            engine
                .repositories()
                .database()
                .call(move |connection| {
                    let metadata_json = connection.query_row(
                        "SELECT metadata_json FROM orchestration_events WHERE command_id = ? AND event_type = 'project.meta-updated'",
                        [&command_id],
                        |row| row.get::<_, String>(0),
                    )?;
                    let mut metadata = serde_json::from_str::<Value>(&metadata_json).map_err(
                        |error| PersistenceError::Corrupt(format!("invalid test metadata: {error}")),
                    )?;
                    let object = metadata.as_object_mut().ok_or_else(|| {
                        PersistenceError::Corrupt("test metadata is not an object".to_owned())
                    })?;
                    if let Some(adoption_result) = adoption_result {
                        object.insert("adoptionResult".to_owned(), adoption_result);
                    } else {
                        object.remove("adoptionResult");
                    }
                    let changed = connection.execute(
                        "UPDATE orchestration_events SET metadata_json = ? WHERE command_id = ? AND event_type = 'project.meta-updated'",
                        params![metadata.to_string(), command_id],
                    )?;
                    if changed != 1 {
                        return Err(PersistenceError::Corrupt(format!(
                            "expected one adoption metadata row, changed {changed}"
                        )));
                    }
                    Ok(())
                })
                .await
                .expect("replace adoption result metadata");
        }

        fn assert_adoption_replay_invariant(result: Result<DispatchResult, OrchestrationError>) {
            assert!(matches!(
                result,
                Err(OrchestrationError::Invariant { command_type, .. })
                    if command_type == "worktree.adopt-resolved"
            ));
        }

        #[tokio::test]
        async fn creates_an_ordinary_workspace_and_compacts_policy_atomically() {
            let engine = adoption_engine(TestHooks::default()).await;

            let command = adopt("adopt-created", Some("feature/external"), Some("abc1234"));
            let result = engine.dispatch(command.clone()).await.expect("adoption");

            assert_eq!(result.disposition.as_deref(), Some("created"));
            let thread_id = result.thread_id.clone().expect("created thread id");
            let snapshot = load_snapshot(&engine.repositories())
                .await
                .expect("snapshot");
            let thread = snapshot
                .threads
                .iter()
                .find(|thread| thread.thread_id == thread_id)
                .expect("created thread");
            assert_eq!(thread.project_id, PROJECT_ID);
            assert_eq!(thread.kind, "workspace");
            assert_eq!(thread.title, "feature/external");
            assert_eq!(thread.branch.as_deref(), Some("feature/external"));
            assert_eq!(thread.worktree_path.as_deref(), Some(PATH));
            assert_eq!(thread.runtime_mode, "full-access");
            assert_eq!(thread.interaction_mode, "plan");
            assert!(
                snapshot
                    .sessions
                    .iter()
                    .all(|session| session.thread_id != thread_id)
            );
            assert_eq!(
                snapshot.projects[0].worktree_discovery,
                json!({
                    "visibility": "shown",
                    "initialPromptDismissedAt": "2026-08-09T00:00:00Z",
                    "baselinePaths": ["/repo/other"]
                })
            );
            let command_events = engine
                .read_events(0)
                .await
                .expect("events")
                .into_iter()
                .filter(|event| event.event.command_id.as_deref() == Some("adopt-created"))
                .map(|event| event.event.event_type)
                .collect::<Vec<_>>();
            assert_eq!(
                command_events,
                vec!["thread.created", "project.meta-updated"]
            );
            assert_eq!(
                engine.dispatch(command).await.expect("idempotent replay"),
                result
            );
            engine.shutdown().await;
        }

        #[tokio::test]
        async fn malformed_adoption_result_metadata_never_falls_back_to_thread_events_or_owners() {
            let engine = adoption_engine(TestHooks::default()).await;
            let command = adopt(
                "adopt-malformed-result",
                Some("feature/external"),
                Some("abc1234"),
            );
            let accepted = engine
                .dispatch(command.clone())
                .await
                .expect("initial adoption");
            let thread_id = accepted.thread_id.expect("created thread id");
            let malformed = [
                json!("not-an-object"),
                json!({"disposition":"created"}),
                json!({"threadId":thread_id,"disposition":"future"}),
                json!({"threadId":"","disposition":"created"}),
            ];

            for adoption_result in malformed {
                replace_adoption_result_metadata(
                    &engine,
                    "adopt-malformed-result",
                    Some(adoption_result),
                )
                .await;
                assert_adoption_replay_invariant(engine.dispatch(command.clone()).await);
            }
            engine.shutdown().await;
        }

        #[tokio::test]
        async fn valid_adoption_result_metadata_must_match_immutable_thread_events() {
            let created = adoption_engine(TestHooks::default()).await;
            let created_command = adopt(
                "adopt-created-inconsistent",
                Some("feature/external"),
                Some("abc1234"),
            );
            let created_result = created
                .dispatch(created_command.clone())
                .await
                .expect("created adoption");
            let created_thread_id = created_result.thread_id.expect("created thread id");
            for inconsistent in [
                json!({"threadId":"different-thread","disposition":"created"}),
                json!({"threadId":created_thread_id,"disposition":"restored"}),
                json!({"threadId":created_thread_id,"disposition":"existing"}),
            ] {
                replace_adoption_result_metadata(
                    &created,
                    "adopt-created-inconsistent",
                    Some(inconsistent),
                )
                .await;
                assert_adoption_replay_invariant(created.dispatch(created_command.clone()).await);
            }
            created.shutdown().await;

            let restored = adoption_engine(TestHooks::default()).await;
            create_thread(
                &restored,
                "restored-inconsistent-owner-create",
                "restored-inconsistent-owner",
                "workspace",
            )
            .await;
            restored
                .dispatch(OrchestrationCommand::ThreadArchive {
                    command_id: "restored-inconsistent-owner-archive".to_owned(),
                    thread_id: "restored-inconsistent-owner".to_owned(),
                })
                .await
                .expect("archive owner");
            let restored_command = adopt("adopt-restored-inconsistent", None, Some("abc1234"));
            restored
                .dispatch(restored_command.clone())
                .await
                .expect("restored adoption");
            replace_adoption_result_metadata(
                &restored,
                "adopt-restored-inconsistent",
                Some(json!({
                    "threadId":"different-thread",
                    "disposition":"restored"
                })),
            )
            .await;
            assert_adoption_replay_invariant(restored.dispatch(restored_command).await);
            restored.shutdown().await;
        }

        #[tokio::test]
        async fn absent_legacy_metadata_replays_only_created_and_restored_thread_evidence() {
            let created = adoption_engine(TestHooks::default()).await;
            let created_command = adopt(
                "adopt-legacy-created",
                Some("feature/external"),
                Some("abc1234"),
            );
            let created_result = created
                .dispatch(created_command.clone())
                .await
                .expect("created adoption");
            replace_adoption_result_metadata(&created, "adopt-legacy-created", None).await;
            created
                .dispatch(detach_owner(
                    "delete-legacy-created-owner",
                    &created_result.thread_id.clone().expect("created thread id"),
                ))
                .await
                .expect("delete created owner");
            assert_eq!(
                created
                    .dispatch(created_command)
                    .await
                    .expect("legacy created replay"),
                created_result
            );
            created.shutdown().await;

            let restored = adoption_engine(TestHooks::default()).await;
            create_thread(
                &restored,
                "legacy-restored-owner-create",
                "legacy-restored-owner",
                "workspace",
            )
            .await;
            restored
                .dispatch(OrchestrationCommand::ThreadArchive {
                    command_id: "legacy-restored-owner-archive".to_owned(),
                    thread_id: "legacy-restored-owner".to_owned(),
                })
                .await
                .expect("archive owner");
            let restored_command = adopt("adopt-legacy-restored", None, Some("abc1234"));
            let restored_result = restored
                .dispatch(restored_command.clone())
                .await
                .expect("restored adoption");
            replace_adoption_result_metadata(&restored, "adopt-legacy-restored", None).await;
            restored
                .dispatch(detach_owner(
                    "delete-legacy-restored-owner",
                    "legacy-restored-owner",
                ))
                .await
                .expect("delete restored owner");
            assert_eq!(
                restored
                    .dispatch(restored_command)
                    .await
                    .expect("legacy restored replay"),
                restored_result
            );
            restored.shutdown().await;
        }

        #[tokio::test]
        async fn absent_legacy_existing_metadata_never_infers_a_current_owner_after_restart() {
            let engine = adoption_engine(TestHooks::default()).await;
            create_thread(
                &engine,
                "legacy-existing-owner-create",
                "legacy-existing-owner",
                "workspace",
            )
            .await;
            let command = adopt(
                "adopt-legacy-existing",
                Some("feature/external"),
                Some("abc1234"),
            );
            let accepted = engine
                .dispatch(command.clone())
                .await
                .expect("existing adoption");
            assert_eq!(accepted.thread_id.as_deref(), Some("legacy-existing-owner"));
            assert_eq!(accepted.disposition.as_deref(), Some("existing"));
            replace_adoption_result_metadata(&engine, "adopt-legacy-existing", None).await;
            engine
                .dispatch(
                    serde_json::from_value(json!({
                        "type":"thread.meta.update",
                        "commandId":"retarget-legacy-existing-owner",
                        "threadId":"legacy-existing-owner",
                        "worktreePath":format!("{PATH}-retargeted")
                    }))
                    .expect("retarget command"),
                )
                .await
                .expect("retarget owner");
            create_thread(
                &engine,
                "replacement-owner-create",
                "replacement-owner",
                "workspace",
            )
            .await;
            let database = engine.repositories().database().clone();
            engine.shutdown().await;

            let restarted = OrchestrationEngine::start(database, EngineOptions::default())
                .await
                .expect("restart engine");
            assert_adoption_replay_invariant(restarted.dispatch(command).await);
            restarted.shutdown().await;
        }

        #[tokio::test]
        async fn restart_replay_finds_an_owner_with_an_equivalent_lexical_path() {
            let engine = adoption_engine(TestHooks::default()).await;
            let persisted_path = Path::new(PATH)
                .join(".")
                .join("alias")
                .join("..")
                .to_string_lossy()
                .into_owned();
            create_thread_at(
                &engine,
                "equivalent-owner-create",
                "equivalent-owner",
                "workspace",
                &persisted_path,
            )
            .await;
            let database = engine.repositories().database().clone();
            engine.shutdown().await;

            let restarted = OrchestrationEngine::start(database, EngineOptions::default())
                .await
                .expect("restart engine");
            let result = restarted
                .dispatch(adopt(
                    "adopt-equivalent-owner",
                    Some("feature/external"),
                    Some("abc1234"),
                ))
                .await
                .expect("equivalent owner adoption");

            assert_eq!(result.thread_id.as_deref(), Some("equivalent-owner"));
            assert_eq!(result.disposition.as_deref(), Some("existing"));
            assert_eq!(
                restarted
                    .repositories()
                    .list_threads_by_project(PROJECT_ID.to_owned())
                    .await
                    .expect("threads")
                    .into_iter()
                    .filter(|thread| thread.kind == "workspace" && thread.deleted_at.is_none())
                    .count(),
                1,
                "equivalent persisted paths must retain one canonical owner"
            );
            assert_eq!(
                restarted
                    .read_events(0)
                    .await
                    .expect("events")
                    .into_iter()
                    .filter(|event| {
                        event.event.command_id.as_deref() == Some("adopt-equivalent-owner")
                    })
                    .map(|event| event.event.event_type)
                    .collect::<Vec<_>>(),
                vec!["project.meta-updated"]
            );
            restarted
                .dispatch(
                    serde_json::from_value(json!({
                        "type":"thread.meta.update",
                        "commandId":"retarget-equivalent-owner",
                        "threadId":"equivalent-owner",
                        "worktreePath":format!("{PATH}-retargeted")
                    }))
                    .expect("retarget command"),
                )
                .await
                .expect("retarget owner");
            restarted
                .dispatch(OrchestrationCommand::ProjectMetaUpdate {
                    command_id: "restore-adoption-baseline".to_owned(),
                    project_id: PROJECT_ID.to_owned(),
                    title: None,
                    workspace_root: None,
                    default_model_selection: OptionalNullable::Missing,
                    scripts: None,
                    worktree_discovery: Some(json!({
                        "visibility":"shown",
                        "initialPromptDismissedAt":"2026-08-09T00:00:00Z",
                        "baselinePaths":[PATH]
                    })),
                })
                .await
                .expect("restore baseline");
            let after_retarget = restarted
                .dispatch(adopt(
                    "adopt-after-retarget",
                    Some("feature/external"),
                    Some("abc1234"),
                ))
                .await
                .expect("adopt after retarget");
            assert_eq!(after_retarget.disposition.as_deref(), Some("created"));
            assert_ne!(
                after_retarget.thread_id.as_deref(),
                Some("equivalent-owner"),
                "retargeting must release the old normalized ownership key"
            );
            restarted.shutdown().await;
        }

        #[tokio::test]
        async fn concurrent_different_public_admissions_conflict_at_persist_time() {
            let engine = adoption_engine(TestHooks::default()).await;
            let first_payload = json!({
                "commandId":"adopt-concurrent-conflict",
                "projectId":PROJECT_ID,
                "worktreeKey":"worktree-key",
                "expectedGeneration":3,
                "threadDefaults":{
                    "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                    "runtimeMode":"full-access",
                    "interactionMode":"plan"
                }
            });
            let mut second_payload = first_payload.clone();
            second_payload["threadDefaults"]["interactionMode"] = json!("default");
            let first_command = adopt(
                "adopt-concurrent-conflict",
                Some("feature/external"),
                Some("abc1234"),
            );
            let mut second_command = first_command.clone();
            if let OrchestrationCommand::WorktreeAdoptResolved {
                interaction_mode, ..
            } = &mut second_command
            {
                *interaction_mode = "default".to_owned();
            }
            let first_admission = CommandAdmission {
                payload_digest: canonical_command_digest(&first_payload).expect("first digest"),
                attachment_refs: Vec::new(),
                provider_turn: None,
            };
            let second_admission = CommandAdmission {
                payload_digest: canonical_command_digest(&second_payload).expect("second digest"),
                attachment_refs: Vec::new(),
                provider_turn: None,
            };

            let (first, second) = tokio::join!(
                engine.dispatch_with_admission(first_command, first_admission, || {}),
                engine.dispatch_with_admission(second_command, second_admission, || {})
            );

            assert_eq!(
                [first.as_ref().is_ok(), second.as_ref().is_ok()]
                    .into_iter()
                    .filter(|accepted| *accepted)
                    .count(),
                1
            );
            assert_eq!(
                [first.as_ref().err(), second.as_ref().err()]
                    .into_iter()
                    .filter(|error| {
                        error.is_some_and(|error| {
                            matches!(error, OrchestrationError::CommandConflict { .. })
                        })
                    })
                    .count(),
                1
            );
            assert_eq!(
                engine
                    .read_events(0)
                    .await
                    .expect("events")
                    .into_iter()
                    .filter(|event| {
                        event.event.command_id.as_deref() == Some("adopt-concurrent-conflict")
                    })
                    .count(),
                2,
                "only the accepted adoption may append its transactional event pair"
            );
            engine.shutdown().await;
        }

        #[tokio::test]
        async fn returns_active_restores_archived_ignores_panels_and_rejects_conflicts() {
            let active = adoption_engine(TestHooks::default()).await;
            create_thread(&active, "active-create", "active-owner", "workspace").await;
            let existing_command = adopt("adopt-existing", Some("feature/external"), None);
            let active_result = active
                .dispatch(existing_command.clone())
                .await
                .expect("existing");
            assert_eq!(active_result.thread_id.as_deref(), Some("active-owner"));
            assert_eq!(active_result.disposition.as_deref(), Some("existing"));
            active
                .dispatch(detach_owner("delete-active-owner", "active-owner"))
                .await
                .expect("delete accepted owner");
            let events_before_retry = active
                .read_events(0)
                .await
                .expect("events before retry")
                .len();
            let replay = active
                .dispatch(existing_command)
                .await
                .expect("accepted adoption replay");
            assert_eq!(replay, active_result);
            assert_eq!(
                active
                    .read_events(0)
                    .await
                    .expect("events after retry")
                    .len(),
                events_before_retry,
                "an accepted replay must not emit replacement effects"
            );
            active.shutdown().await;

            let archived = adoption_engine(TestHooks::default()).await;
            create_thread(&archived, "archive-create", "archived-owner", "workspace").await;
            archived
                .dispatch(OrchestrationCommand::ThreadArchive {
                    command_id: "archive-owner".to_owned(),
                    thread_id: "archived-owner".to_owned(),
                })
                .await
                .expect("archive");
            let restored = archived
                .dispatch(adopt("adopt-restored", None, Some("abcdef123456")))
                .await
                .expect("restored");
            assert_eq!(restored.thread_id.as_deref(), Some("archived-owner"));
            assert_eq!(restored.disposition.as_deref(), Some("restored"));
            assert!(
                archived
                    .repositories()
                    .get_thread("archived-owner".to_owned())
                    .await
                    .expect("thread query")
                    .expect("thread")
                    .archived_at
                    .is_none()
            );
            archived.shutdown().await;

            let panel = adoption_engine(TestHooks::default()).await;
            create_thread(&panel, "panel-create", "panel-owner", "panel").await;
            let panel_result = panel
                .dispatch(adopt("adopt-with-panel", None, Some("abcdef123456")))
                .await
                .expect("panel ignored");
            assert_ne!(panel_result.thread_id.as_deref(), Some("panel-owner"));
            assert_eq!(panel_result.disposition.as_deref(), Some("created"));
            panel.shutdown().await;

            let conflict = adoption_engine(TestHooks::default()).await;
            create_thread(&conflict, "conflict-create-1", "owner-1", "workspace").await;
            create_thread(&conflict, "conflict-create-2", "owner-2", "workspace").await;
            let before = conflict.read_events(0).await.expect("before").len();
            let error = conflict
                .dispatch(adopt("adopt-conflict", Some("feature/external"), None))
                .await
                .expect_err("conflict");
            assert!(matches!(
                error,
                OrchestrationError::WorktreeOwnershipConflict { .. }
            ));
            assert_eq!(conflict.read_events(0).await.expect("after").len(), before);
            conflict.shutdown().await;
        }

        #[tokio::test]
        async fn rolls_back_thread_and_policy_when_either_projector_fails() {
            for (projector, event_type) in [
                ("projection.threads", "thread.created"),
                ("projection.projects", "project.meta-updated"),
            ] {
                let hooks = TestHooks::default();
                let engine = adoption_engine(hooks.clone()).await;
                let before_events = engine.read_events(0).await.expect("before events").len();
                let before_policy = load_snapshot(&engine.repositories())
                    .await
                    .expect("before snapshot")
                    .projects[0]
                    .worktree_discovery
                    .clone();
                hooks.fail_next_projector(projector, Some(event_type));

                let error = engine
                    .dispatch(adopt(
                        &format!("adopt-fail-{event_type}"),
                        Some("feature/external"),
                        None,
                    ))
                    .await
                    .expect_err("projector failure");

                assert!(matches!(
                    error,
                    OrchestrationError::InjectedProjectorFailure { .. }
                ));
                assert_eq!(
                    engine.read_events(0).await.expect("after events").len(),
                    before_events
                );
                let snapshot = load_snapshot(&engine.repositories())
                    .await
                    .expect("snapshot");
                assert_eq!(snapshot.projects[0].worktree_discovery, before_policy);
                assert!(
                    snapshot
                        .threads
                        .iter()
                        .all(|thread| thread.worktree_path.as_deref() != Some(PATH))
                );
                assert!(
                    engine
                        .repositories()
                        .get_command_receipt(format!("adopt-fail-{event_type}"))
                        .await
                        .expect("receipt query")
                        .is_none()
                );
                engine.shutdown().await;
            }
        }
    }

    struct DropFlag(Arc<std::sync::atomic::AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }

    fn project_create_command(command_id: &str, project_id: &str) -> OrchestrationCommand {
        project_create_command_at(command_id, project_id, "C:/repo")
    }

    fn project_create_command_at(
        command_id: &str,
        project_id: &str,
        workspace_root: &str,
    ) -> OrchestrationCommand {
        serde_json::from_value(json!({
            "type":"project.create", "commandId":command_id, "projectId":project_id,
            "title":"Project", "workspaceRoot":workspace_root, "defaultModelSelection":null,
            "createdAt":"2026-08-01T00:00:00Z"
        }))
        .expect("project command")
    }

    async fn delivery_engine(hooks: TestHooks) -> (OrchestrationEngine, String) {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let engine = OrchestrationEngine::start(
            database,
            EngineOptions {
                test_hooks: hooks,
                ..EngineOptions::default()
            },
        )
        .await
        .expect("engine");
        engine
            .dispatch(project_create_command(
                "delivery-project",
                "delivery-project",
            ))
            .await
            .expect("project");
        let thread_id = engine
            .repositories()
            .list_threads_by_project("delivery-project".to_owned())
            .await
            .expect("threads")
            .into_iter()
            .find(|thread| thread.kind == "default")
            .expect("default thread")
            .thread_id;
        (engine, thread_id)
    }

    #[tokio::test]
    async fn panel_host_thread_survives_projection_rebuild() {
        let (engine, host_id) = delivery_engine(TestHooks::default()).await;
        engine
            .dispatch(OrchestrationCommand::ThreadCreate {
                command_id: "create-panel".to_owned(),
                thread_id: "panel-thread".to_owned(),
                project_id: "delivery-project".to_owned(),
                title: "Panel".to_owned(),
                kind: Some("panel".to_owned()),
                host_thread_id: Some(host_id.clone()),
                model_selection: json!({"instanceId":"codex","model":"gpt-5"}),
                runtime_mode: "full-access".to_owned(),
                interaction_mode: "default".to_owned(),
                branch: None,
                worktree_path: None,
                created_at: "2026-08-01T00:00:01Z".to_owned(),
            })
            .await
            .expect("panel");
        let host_of = |thread_id: &'static str| {
            let repositories = engine.repositories();
            async move {
                repositories
                    .get_thread(thread_id.to_owned())
                    .await
                    .expect("thread read")
                    .expect("thread exists")
                    .host_thread_id
            }
        };
        assert_eq!(host_of("panel-thread").await, Some(host_id.clone()));

        engine
            .repositories()
            .database()
            .call(|connection| {
                connection.execute("UPDATE projection_threads SET host_thread_id = NULL", [])?;
                connection.execute(
                    "DELETE FROM projection_state WHERE projector = 'projection.threads'",
                    [],
                )?;
                Ok(())
            })
            .await
            .expect("reset thread projection");
        bootstrap_projectors(&engine.repositories(), &TestHooks::default())
            .await
            .expect("replay threads");

        assert_eq!(host_of("panel-thread").await, Some(host_id.clone()));
        let host = engine
            .repositories()
            .get_thread(host_id)
            .await
            .expect("host read")
            .expect("host exists");
        assert_eq!(host.host_thread_id, None);
    }

    fn delivery_turn_for_message(
        command_id: &str,
        thread_id: &str,
        message_id: &str,
        text: &str,
        created_at: &str,
    ) -> OrchestrationCommand {
        serde_json::from_value(json!({
            "type":"thread.turn.start", "commandId":command_id, "threadId":thread_id,
            "message":{"messageId":message_id,"role":"user","text":text,"attachments":[{
                "type":"file","id":"notes-1","name":"notes.txt","mimeType":"text/plain","sizeBytes":5
            }]},
            "modelSelection":{"instanceId":"codex","model":"gpt-5"},
            "createdAt":created_at
        }))
        .expect("turn command")
    }

    fn delivery_turn(command_id: &str, thread_id: &str, text: &str) -> OrchestrationCommand {
        delivery_turn_for_message(
            command_id,
            thread_id,
            "delivery-message",
            text,
            "2026-08-01T00:00:01Z",
        )
    }

    fn delivery_admission_for_message(
        command: &OrchestrationCommand,
        thread_id: &str,
        message_id: &str,
        created_at: &str,
    ) -> CommandAdmission {
        CommandAdmission {
            payload_digest: canonical_command_digest(command).expect("digest"),
            attachment_refs: vec![AttachmentReference {
                attachment_id: "notes-1".to_owned(),
                content_digest: Some("digest-1".to_owned()),
                size_bytes: 5,
            }],
            provider_turn: Some(NewProviderTurnDelivery {
                state: crate::orchestration::TurnDeliveryState::Pending,
                mode: crate::orchestration::TurnDeliveryMode::Start,
                command_id: command.command_id().to_owned(),
                thread_id: thread_id.to_owned(),
                message_id: message_id.to_owned(),
                provider_instance_id: "codex".to_owned(),
                provider_kind: "codex".to_owned(),
                provider_session_id: None,
                delivery_key: "delivery-key".to_owned(),
                payload: serde_json::to_value(command).expect("payload"),
                created_at: created_at.to_owned(),
            }),
        }
    }

    fn delivery_admission(command: &OrchestrationCommand, thread_id: &str) -> CommandAdmission {
        delivery_admission_for_message(
            command,
            thread_id,
            "delivery-message",
            "2026-08-01T00:00:01Z",
        )
    }

    #[tokio::test]
    async fn queued_turn_start_records_message_without_turn_start_requested() {
        let (engine, thread_id) = delivery_engine(TestHooks::default()).await;
        let mut command = delivery_turn("queued-start", &thread_id, "next task");
        if let OrchestrationCommand::ThreadTurnStart { queued, .. } = &mut command {
            *queued = Some(true);
        }
        let mut admission = delivery_admission(&command, &thread_id);
        admission.provider_turn.as_mut().unwrap().state = TurnDeliveryState::Queued;
        let before = engine
            .read_events(0)
            .await
            .unwrap()
            .last()
            .unwrap()
            .sequence;
        engine
            .dispatch_with_admission(command, admission, || {})
            .await
            .unwrap();
        let events = engine.read_events(before).await.unwrap();
        assert_eq!(
            events
                .iter()
                .map(|event| event.event.event_type.as_str())
                .collect::<Vec<_>>(),
            ["thread.message-sent", "thread.turn-delivery-updated"]
        );
        assert_eq!(events[0].event.payload["role"], "user");
        assert_eq!(events[0].event.payload["turnId"], Value::Null);
        assert_eq!(
            events[1].event.payload["delivery"],
            json!({"state":"queued", "provider":"codex", "providerInstanceId":"codex", "mode":"start", "held":false})
        );
        assert!(
            engine
                .repositories()
                .list_turns_by_thread(thread_id.clone())
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            engine
                .repositories()
                .get_thread_session(thread_id.clone())
                .await
                .unwrap()
                .is_none()
        );
        let snapshot = load_snapshot(&engine.repositories()).await.unwrap();
        let message = snapshot
            .messages
            .iter()
            .find(|message| message.thread_id == thread_id)
            .unwrap();
        assert_eq!(message.delivery_state.as_deref(), Some("queued"));
        assert_eq!(message.delivery_mode.as_deref(), Some("start"));
        assert_eq!(message.delivery_held, Some(false));
        let row = engine
            .repositories()
            .get_provider_turn_delivery("queued-start".into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, TurnDeliveryState::Queued);
        assert_eq!(row.attempts, 0);
        engine.shutdown().await;
    }

    /// The web's W1 waiting rule relies on equal admission timestamps and promotion
    /// moving only the projected message's timestamp later, leaving outbox order intact.
    #[tokio::test]
    async fn delivery_timestamps_match_at_admission_and_only_message_moves_on_promotion() {
        const ADMITTED_AT: &str = "2026-08-01T00:00:01Z";
        const PROMOTED_AT: &str = "2026-08-01T00:00:04Z";

        for queued_admission in [false, true] {
            let (engine, thread_id) = delivery_engine(TestHooks::default()).await;
            let mut command = delivery_turn("timestamp-command", &thread_id, "next task");
            if let OrchestrationCommand::ThreadTurnStart { queued, .. } = &mut command {
                *queued = Some(queued_admission);
            }
            let mut admission = delivery_admission(&command, &thread_id);
            if queued_admission {
                admission.provider_turn.as_mut().unwrap().state = TurnDeliveryState::Queued;
            }
            engine
                .dispatch_with_admission(command, admission, || {})
                .await
                .expect("admission");

            let row = engine
                .repositories()
                .get_provider_turn_delivery("timestamp-command".into())
                .await
                .unwrap()
                .unwrap();
            let snapshot = load_snapshot(&engine.repositories()).await.unwrap();
            let message = snapshot
                .messages
                .iter()
                .find(|message| message.message_id == "delivery-message")
                .unwrap();
            assert_eq!(row.created_at, ADMITTED_AT);
            assert_eq!(row.created_at, message.created_at);
            assert_eq!(
                row.state,
                if queued_admission {
                    TurnDeliveryState::Queued
                } else {
                    TurnDeliveryState::Pending
                }
            );

            if queued_admission {
                engine
                    .dispatch(
                        serde_json::from_value(json!({
                            "type":"thread.session.set", "commandId":"session", "threadId":thread_id,
                            "session":{"threadId":thread_id, "status":"ready", "providerName":"codex",
                                "providerInstanceId":"codex", "runtimeMode":"full-access",
                                "activeTurnId":null, "lastError":null, "updatedAt":"2026-08-01T00:00:03Z"},
                            "createdAt":"2026-08-01T00:00:03Z"
                        }))
                        .unwrap(),
                    )
                    .await
                    .expect("session ready for promotion");
                engine
                    .dispatch(
                        serde_json::from_value(json!({
                            "type":"thread.turn.promote", "commandId":"promote",
                            "threadId":thread_id, "messageId":"delivery-message",
                            "createdAt":PROMOTED_AT
                        }))
                        .unwrap(),
                    )
                    .await
                    .expect("promotion");

                let promoted_row = engine
                    .repositories()
                    .get_provider_turn_delivery("timestamp-command".into())
                    .await
                    .unwrap()
                    .unwrap();
                let snapshot = load_snapshot(&engine.repositories()).await.unwrap();
                let promoted_message = snapshot
                    .messages
                    .iter()
                    .find(|message| message.message_id == "delivery-message")
                    .unwrap();
                assert_eq!(promoted_row.state, TurnDeliveryState::Pending);
                assert_eq!(promoted_row.created_at, row.created_at);
                assert_eq!(promoted_message.created_at, PROMOTED_AT);
                assert!(promoted_message.created_at.as_str() > ADMITTED_AT);
            }
            engine.shutdown().await;
        }
    }

    fn delivery_resolution(
        command_id: &str,
        thread_id: &str,
        message_id: &str,
        action: &str,
        created_at: &str,
    ) -> OrchestrationCommand {
        serde_json::from_value(json!({
            "type":"thread.turn-delivery.resolve",
            "commandId":command_id,
            "threadId":thread_id,
            "messageId":message_id,
            "action":action,
            "createdAt":created_at,
        }))
        .expect("delivery resolution command")
    }

    #[tokio::test]
    async fn delivery_provider_instance_survives_queue_actions_and_legacy_replay() {
        for (action, status) in [("promote", "ready"), ("steer", "running")] {
            let (engine, thread_id) = delivery_engine(TestHooks::default()).await;
            let mut command = delivery_turn("instance-command", &thread_id, "next task");
            if let OrchestrationCommand::ThreadTurnStart {
                queued,
                model_selection,
                ..
            } = &mut command
            {
                *queued = Some(true);
                *model_selection = Some(json!({"instanceId":"codex-personal", "model":"gpt-5"}));
            }
            let mut admission = delivery_admission(&command, &thread_id);
            let delivery = admission.provider_turn.as_mut().unwrap();
            delivery.state = TurnDeliveryState::Queued;
            delivery.provider_instance_id = "codex-personal".to_owned();
            engine
                .dispatch_with_admission(command, admission, || {})
                .await
                .expect("queued admission");
            engine
                .dispatch(
                    serde_json::from_value(json!({
                        "type":"thread.turn.interrupt", "commandId":"hold", "threadId":thread_id,
                        "createdAt":"2026-08-01T00:00:02Z"
                    }))
                    .unwrap(),
                )
                .await
                .expect("hold queued delivery");
            engine
                .dispatch(
                    serde_json::from_value(json!({
                        "type":"thread.session.set", "commandId":"session", "threadId":thread_id,
                        "session":{"threadId":thread_id, "status":status, "providerName":"codex",
                            "providerInstanceId":"codex-personal", "runtimeMode":"full-access",
                            "activeTurnId":if status == "running" { Some("turn-1") } else { None },
                            "lastError":null, "updatedAt":"2026-08-01T00:00:03Z"},
                        "createdAt":"2026-08-01T00:00:03Z"
                    }))
                    .unwrap(),
                )
                .await
                .expect("session ready for queue action");
            engine
                .dispatch(
                    serde_json::from_value(json!({
                        "type":format!("thread.turn.{action}"), "commandId":"queue-action",
                        "threadId":thread_id, "messageId":"delivery-message",
                        "createdAt":"2026-08-01T00:00:04Z"
                    }))
                    .unwrap(),
                )
                .await
                .expect("queue action");

            let deliveries = engine
                .read_events(0)
                .await
                .expect("events")
                .into_iter()
                .filter(|event| event.event.event_type == "thread.turn-delivery-updated")
                .collect::<Vec<_>>();
            assert_eq!(deliveries.len(), 3, "admission, hold, and {action}");
            for event in &deliveries {
                assert_eq!(
                    event.event.payload["delivery"]["providerInstanceId"],
                    "codex-personal"
                );
                assert!(event.event.payload.get("providerInstanceId").is_none());
            }
            assert_eq!(deliveries[1].event.payload["delivery"]["held"], true);

            // Replaying a later event from an older server must not erase the instance.
            let mut legacy = deliveries.last().unwrap().event.clone();
            legacy.event_id = Uuid::new_v4().to_string();
            legacy.payload["delivery"]
                .as_object_mut()
                .unwrap()
                .remove("providerInstanceId");
            engine
                .repositories()
                .append_event(legacy)
                .await
                .expect("legacy event");
            for rebuild in [false, true] {
                if rebuild {
                    engine.repositories().database().call(|connection| {
                        connection.execute("DELETE FROM projection_thread_messages", [])?;
                        connection.execute("DELETE FROM projection_state WHERE projector = 'projection.thread-messages'", [])?;
                        Ok(())
                    }).await.expect("reset message projection");
                }
                bootstrap_projectors(&engine.repositories(), &TestHooks::default())
                    .await
                    .expect("replay messages");
                let message = engine
                    .repositories()
                    .get_message("delivery-message".to_owned())
                    .await
                    .expect("message lookup")
                    .expect("message");
                assert_eq!(
                    message.delivery_provider_instance_id.as_deref(),
                    Some("codex-personal")
                );
                assert_eq!(message.delivery_state.as_deref(), Some("pending"));
                assert_eq!(
                    message.delivery_mode.as_deref(),
                    Some(if action == "steer" { "steer" } else { "start" })
                );
            }
            engine.shutdown().await;
        }
    }

    async fn admit_delivery(
        engine: &OrchestrationEngine,
        command_id: &str,
        thread_id: &str,
        message_id: &str,
        created_at: &str,
    ) {
        let command =
            delivery_turn_for_message(command_id, thread_id, message_id, command_id, created_at);
        engine
            .dispatch_with_admission(
                command.clone(),
                delivery_admission_for_message(&command, thread_id, message_id, created_at),
                || {},
            )
            .await
            .expect("admit delivery");
    }

    #[tokio::test]
    async fn delivery_replay_owns_commit_once_and_conflicts_on_digest_mismatch() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let (engine, thread_id) = delivery_engine(TestHooks::default()).await;
        let command = delivery_turn("delivery-command", &thread_id, "first");
        let admission = delivery_admission(&command, &thread_id);
        let commits = Arc::new(AtomicUsize::new(0));
        let first_commits = commits.clone();
        let first = engine
            .dispatch_with_admission(command.clone(), admission.clone(), move || {
                first_commits.fetch_add(1, Ordering::SeqCst);
            })
            .await
            .expect("first accepted");
        let replay_commits = commits.clone();
        let same_replay = engine
            .dispatch_with_admission(command, admission, move || {
                replay_commits.fetch_add(1, Ordering::SeqCst);
            })
            .await
            .expect("same replay");
        let different_command = delivery_turn("delivery-command", &thread_id, "different");
        let different = engine
            .dispatch_with_admission(
                different_command.clone(),
                delivery_admission(&different_command, &thread_id),
                || {},
            )
            .await;

        assert_eq!(same_replay.sequence, first.sequence);
        assert_eq!(commits.load(Ordering::SeqCst), 1);
        assert!(matches!(
            different,
            Err(OrchestrationError::CommandConflict { .. })
        ));
        assert_eq!(
            engine
                .repositories()
                .list_provider_turn_deliveries(vec![TurnDeliveryState::Pending])
                .await
                .expect("pending deliveries")
                .len(),
            1
        );
        assert_eq!(
            engine
                .repositories()
                .list_referenced_attachment_ids()
                .await
                .expect("attachment refs"),
            vec!["notes-1".to_owned()]
        );
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn delivery_projector_failure_rolls_back_receipt_events_outbox_and_references() {
        let hooks = TestHooks::default();
        let (engine, thread_id) = delivery_engine(hooks.clone()).await;
        let before = engine.read_events(0).await.expect("events before").len();
        hooks.fail_next_projector("projection.thread-messages", Some("thread.message-sent"));
        let command = delivery_turn("delivery-rollback", &thread_id, "rollback");
        let result = engine
            .dispatch_with_admission(
                command.clone(),
                delivery_admission(&command, &thread_id),
                || {},
            )
            .await;
        assert!(matches!(
            result,
            Err(OrchestrationError::InjectedProjectorFailure { .. })
        ));
        assert_eq!(
            engine.read_events(0).await.expect("events after").len(),
            before
        );
        assert!(
            engine
                .repositories()
                .get_command_receipt("delivery-rollback".to_owned())
                .await
                .expect("receipt")
                .is_none()
        );
        assert!(
            engine
                .repositories()
                .get_provider_turn_delivery("delivery-rollback".to_owned())
                .await
                .expect("outbox")
                .is_none()
        );
        assert!(
            engine
                .repositories()
                .list_referenced_attachment_ids()
                .await
                .expect("refs")
                .is_empty()
        );
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn delivery_transition_is_attempt_conditioned_and_projects_once() {
        let (engine, thread_id) = delivery_engine(TestHooks::default()).await;
        let command = delivery_turn("delivery-transition", &thread_id, "transition");
        engine
            .dispatch_with_admission(
                command.clone(),
                delivery_admission(&command, &thread_id),
                || {},
            )
            .await
            .expect("admitted");
        let claimed = engine
            .repositories()
            .claim_provider_turn(
                "delivery-transition".to_owned(),
                "2026-08-01T00:00:02Z".to_owned(),
            )
            .await
            .expect("claim")
            .expect("claimed");
        let before = engine.read_events(0).await.expect("before").len();
        let transition = TurnDeliveryTransition {
            turn_id: None,
            command_id: claimed.command_id,
            expected_states: vec![TurnDeliveryState::Sending],
            expected_attempt: claimed.attempts,
            next_state: TurnDeliveryState::Delivered,
            detail: None,
            updated_at: "2026-08-01T00:00:03Z".to_owned(),
        };
        assert!(
            engine
                .transition_turn_delivery(transition.clone())
                .await
                .expect("transition")
        );
        assert!(
            !engine
                .transition_turn_delivery(transition)
                .await
                .expect("stale transition")
        );
        assert_eq!(
            engine.read_events(0).await.expect("after").len(),
            before + 1
        );
        let message = engine
            .repositories()
            .get_message("delivery-message".to_owned())
            .await
            .expect("message")
            .expect("message");
        assert_eq!(message.delivery_state.as_deref(), Some("delivered"));
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn unresolved_delivery_is_derived_onto_the_thread_and_clears_itself() {
        // A refused or unknown-fate delivery used to be visible only inside the
        // open chat. This field is derived in the same transaction as the
        // delivery transition, so it must appear when the delivery stops being
        // resolvable and disappear again the moment it is resolved — without any
        // separate command, and without touching session or turn state.
        let (engine, thread_id) = delivery_engine(TestHooks::default()).await;
        let before = engine
            .repositories()
            .get_thread(thread_id.clone())
            .await
            .expect("thread")
            .expect("projected thread");
        assert_eq!(before.unresolved_delivery_state, None);

        admit_delivery(
            &engine,
            "derived-delivery",
            &thread_id,
            "derived-delivery-message",
            "2026-08-01T00:00:01Z",
        )
        .await;
        let claimed = engine
            .repositories()
            .claim_provider_turn(
                "derived-delivery".to_owned(),
                "2026-08-01T00:00:02Z".to_owned(),
            )
            .await
            .expect("claim")
            .expect("claimed");
        assert!(
            engine
                .transition_turn_delivery(TurnDeliveryTransition {
                    turn_id: None,
                    command_id: claimed.command_id.clone(),
                    expected_states: vec![TurnDeliveryState::Sending],
                    expected_attempt: 1,
                    next_state: TurnDeliveryState::Failed,
                    detail: Some("the provider refused this message".to_owned()),
                    updated_at: "2026-08-01T00:00:03Z".to_owned(),
                })
                .await
                .expect("mark failed")
        );

        let failed = engine
            .repositories()
            .get_thread(thread_id.clone())
            .await
            .expect("thread")
            .expect("projected thread");
        assert_eq!(failed.unresolved_delivery_state.as_deref(), Some("failed"));
        assert_eq!(
            failed.unresolved_delivery_detail.as_deref(),
            Some("the provider refused this message")
        );
        // The provider session and its active turn must be untouched: this is a
        // delivery fact, not a provider-session fact.
        let session = engine
            .repositories()
            .get_thread_session(thread_id.clone())
            .await
            .expect("session read");
        assert!(
            session.is_none_or(|session| session.status != "error"),
            "a rejected delivery must not mark the provider session errored"
        );

        // Resolving it clears the derived field with no extra command.
        assert!(
            engine
                .transition_turn_delivery(TurnDeliveryTransition {
                    turn_id: None,
                    command_id: claimed.command_id,
                    expected_states: vec![TurnDeliveryState::Failed],
                    expected_attempt: 1,
                    next_state: TurnDeliveryState::Dismissed,
                    detail: None,
                    updated_at: "2026-08-01T00:00:04Z".to_owned(),
                })
                .await
                .expect("dismiss")
        );
        let cleared = engine
            .repositories()
            .get_thread(thread_id)
            .await
            .expect("thread")
            .expect("projected thread");
        assert_eq!(cleared.unresolved_delivery_state, None);
        assert_eq!(cleared.unresolved_delivery_detail, None);
    }

    async fn assert_delivery_reason(engine: &OrchestrationEngine, expected: Option<&str>) {
        let reason = engine.repositories().database().call(|connection| {
            Ok(connection.query_row(
                "SELECT failure_reason FROM provider_turn_outbox WHERE command_id = 'reason-command'",
                [], |row| row.get::<_, Option<String>>(0),
            )?)
        }).await.expect("outbox reason");
        assert_eq!(reason.as_deref(), expected);
        let message = engine
            .repositories()
            .get_message("reason-message".to_owned())
            .await
            .expect("message lookup")
            .expect("message");
        assert_eq!(message.delivery_reason.as_deref(), expected);
        let events = engine.read_events(0).await.expect("events");
        let event = events
            .iter()
            .rev()
            .find(|event| event.event.event_type == "thread.turn-delivery-updated")
            .expect("delivery event");
        assert_eq!(
            event.event.payload["delivery"].get("reason"),
            expected.map(|value| json!(value)).as_ref()
        );
        assert_eq!(
            event.event.payload["delivery"]["providerInstanceId"],
            "codex"
        );
        assert!(event.event.payload.get("providerInstanceId").is_none());
        assert_eq!(
            message.delivery_provider_instance_id.as_deref(),
            Some("codex")
        );
    }

    #[tokio::test]
    async fn delivery_failure_reason_replays_and_clears_on_retry_or_dismiss() {
        use crate::orchestration::delivery::TurnDeliveryFailureReason;

        for action in ["retry", "dismiss"] {
            let (engine, thread_id) = delivery_engine(TestHooks::default()).await;
            admit_delivery(
                &engine,
                "reason-command",
                &thread_id,
                "reason-message",
                "2026-08-01T00:00:01Z",
            )
            .await;
            assert_delivery_reason(&engine, None).await;
            assert!(
                engine
                    .transition_turn_delivery_with_reason(
                        TurnDeliveryTransition {
                            turn_id: None,
                            command_id: "reason-command".to_owned(),
                            expected_states: vec![TurnDeliveryState::Pending],
                            expected_attempt: 0,
                            next_state: TurnDeliveryState::Failed,
                            detail: Some("model refused".to_owned()),
                            updated_at: "2026-08-01T00:00:02Z".to_owned(),
                        },
                        Some(TurnDeliveryFailureReason::ModelSelectionRefused)
                    )
                    .await
                    .expect("refusal")
            );
            assert_delivery_reason(&engine, Some("modelSelectionRefused")).await;

            engine.repositories().database().call(|connection| {
                connection.execute("DELETE FROM projection_thread_messages", [])?;
                connection.execute("DELETE FROM projection_state WHERE projector = 'projection.thread-messages'", [])?;
                Ok(())
            }).await.expect("reset message projection");
            bootstrap_projectors(&engine.repositories(), &TestHooks::default())
                .await
                .expect("replay messages");
            assert_delivery_reason(&engine, Some("modelSelectionRefused")).await;

            engine
                .dispatch(delivery_resolution(
                    "resolve-reason",
                    &thread_id,
                    "reason-message",
                    action,
                    "2026-08-01T00:00:03Z",
                ))
                .await
                .expect("resolve refusal");
            assert_delivery_reason(&engine, None).await;
            if action == "retry" {
                let claimed = engine
                    .repositories()
                    .claim_provider_turn(
                        "reason-command".to_owned(),
                        "2026-08-01T00:00:04Z".to_owned(),
                    )
                    .await
                    .expect("claim retried delivery")
                    .expect("claimed");
                assert!(
                    engine
                        .transition_turn_delivery_with_reason(
                            TurnDeliveryTransition {
                                turn_id: None,
                                command_id: claimed.command_id,
                                expected_states: vec![TurnDeliveryState::Sending],
                                expected_attempt: claimed.attempts,
                                next_state: TurnDeliveryState::Delivered,
                                detail: None,
                                updated_at: "2026-08-01T00:00:05Z".to_owned(),
                            },
                            Some(TurnDeliveryFailureReason::ModelSelectionRefused)
                        )
                        .await
                        .expect("delivered ignores reason")
                );
                assert_delivery_reason(&engine, None).await;
            }
            engine.shutdown().await;
        }
    }

    #[tokio::test]
    async fn delivered_new_conversation_reason_is_state_scoped_and_replays_without_losing_text() {
        use crate::orchestration::delivery::TurnDeliveryFailureReason;
        for (state, reason, expected) in [
            (
                TurnDeliveryState::Delivered,
                TurnDeliveryFailureReason::StartedNewConversation,
                Some("startedNewConversation"),
            ),
            (
                TurnDeliveryState::Failed,
                TurnDeliveryFailureReason::StartedNewConversation,
                None,
            ),
            (
                TurnDeliveryState::Delivered,
                TurnDeliveryFailureReason::ModelSelectionRefused,
                None,
            ),
        ] {
            let (engine, thread_id) = delivery_engine(TestHooks::default()).await;
            admit_delivery(
                &engine,
                "reason-command",
                &thread_id,
                "reason-message",
                "2026-08-01T00:00:01Z",
            )
            .await;
            let original = engine
                .repositories()
                .get_message("reason-message".to_owned())
                .await
                .unwrap()
                .unwrap();
            assert!(
                engine
                    .transition_turn_delivery_with_reason(
                        TurnDeliveryTransition {
                            turn_id: None,
                            command_id: "reason-command".to_owned(),
                            expected_states: vec![TurnDeliveryState::Pending],
                            expected_attempt: 0,
                            next_state: state,
                            detail: None,
                            updated_at: "2026-08-01T00:00:02Z".to_owned(),
                        },
                        Some(reason)
                    )
                    .await
                    .unwrap()
            );
            assert_delivery_reason(&engine, expected).await;
            engine.repositories().database().call(|connection| {
                connection.execute("DELETE FROM projection_thread_messages", [])?;
                connection.execute("DELETE FROM projection_state WHERE projector = 'projection.thread-messages'", [])?;
                Ok(())
            }).await.unwrap();
            bootstrap_projectors(&engine.repositories(), &TestHooks::default())
                .await
                .unwrap();
            assert_delivery_reason(&engine, expected).await;
            let replayed = engine
                .repositories()
                .get_message("reason-message".to_owned())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(replayed.text, original.text);
            assert_eq!(replayed.attachments, original.attachments);
            assert!(
                engine
                    .transition_turn_delivery(TurnDeliveryTransition {
                        turn_id: None,
                        command_id: "reason-command".to_owned(),
                        expected_states: vec![state],
                        expected_attempt: 0,
                        next_state: state,
                        detail: None,
                        updated_at: "2026-08-01T00:00:03Z".to_owned(),
                    })
                    .await
                    .unwrap()
            );
            assert_delivery_reason(&engine, None).await;
            engine.shutdown().await;
        }
    }

    #[tokio::test]
    async fn delivery_resolution_retry_resets_and_replays_once() {
        let (engine, thread_id) = delivery_engine(TestHooks::default()).await;
        admit_delivery(
            &engine,
            "delivery-retry-first",
            &thread_id,
            "delivery-retry-message",
            "2026-08-01T00:00:01Z",
        )
        .await;
        let claimed = engine
            .repositories()
            .claim_provider_turn(
                "delivery-retry-first".to_owned(),
                "2026-08-01T00:00:02Z".to_owned(),
            )
            .await
            .expect("claim")
            .expect("claimed");
        assert!(
            engine
                .transition_turn_delivery(TurnDeliveryTransition {
                    turn_id: None,
                    command_id: claimed.command_id,
                    expected_states: vec![TurnDeliveryState::Sending],
                    expected_attempt: 1,
                    next_state: TurnDeliveryState::Uncertain,
                    detail: Some("connection closed before acknowledgement".to_owned()),
                    updated_at: "2026-08-01T00:00:03Z".to_owned(),
                })
                .await
                .expect("mark uncertain")
        );
        admit_delivery(
            &engine,
            "delivery-retry-later",
            &thread_id,
            "delivery-retry-later-message",
            "2026-08-01T00:00:04Z",
        )
        .await;

        let before = engine.read_events(0).await.expect("events before").len();
        let resolution = delivery_resolution(
            "delivery-retry-resolution",
            &thread_id,
            "delivery-retry-message",
            "retry",
            "2026-08-01T00:00:05Z",
        );
        let first = engine
            .dispatch(resolution.clone())
            .await
            .expect("retry resolution");
        let replay = engine.dispatch(resolution).await.expect("retry replay");

        assert_eq!(replay.sequence, first.sequence);
        assert_eq!(
            engine.read_events(0).await.expect("events after").len(),
            before + 1
        );
        let row = engine
            .repositories()
            .get_provider_turn_delivery("delivery-retry-first".to_owned())
            .await
            .expect("delivery")
            .expect("retry row");
        assert_eq!(row.state, TurnDeliveryState::Pending);
        assert_eq!(row.attempts, 0);
        assert_eq!(row.last_error, None);
        let pending = engine
            .repositories()
            .list_provider_turn_deliveries(vec![TurnDeliveryState::Pending])
            .await
            .expect("pending rows");
        assert_eq!(
            pending
                .iter()
                .map(|row| row.command_id.as_str())
                .collect::<Vec<_>>(),
            ["delivery-retry-first", "delivery-retry-later"]
        );
        let message = engine
            .repositories()
            .get_message("delivery-retry-message".to_owned())
            .await
            .expect("message")
            .expect("retry message");
        assert_eq!(message.delivery_state.as_deref(), Some("pending"));
        assert_eq!(message.delivery_detail, None);
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn delivery_resolution_dismiss_retains_detail_and_unblocks_later_row() {
        let (engine, thread_id) = delivery_engine(TestHooks::default()).await;
        admit_delivery(
            &engine,
            "delivery-dismiss-first",
            &thread_id,
            "delivery-dismiss-message",
            "2026-08-01T00:00:01Z",
        )
        .await;
        assert!(
            engine
                .transition_turn_delivery(TurnDeliveryTransition {
                    turn_id: None,
                    command_id: "delivery-dismiss-first".to_owned(),
                    expected_states: vec![TurnDeliveryState::Pending],
                    expected_attempt: 0,
                    next_state: TurnDeliveryState::Failed,
                    detail: Some("provider rejected the request".to_owned()),
                    updated_at: "2026-08-01T00:00:02Z".to_owned(),
                })
                .await
                .expect("mark failed")
        );
        admit_delivery(
            &engine,
            "delivery-dismiss-later",
            &thread_id,
            "delivery-dismiss-later-message",
            "2026-08-01T00:00:03Z",
        )
        .await;

        let before = engine.read_events(0).await.expect("events before").len();
        let resolution = delivery_resolution(
            "delivery-dismiss-resolution",
            &thread_id,
            "delivery-dismiss-message",
            "dismiss",
            "2026-08-01T00:00:04Z",
        );
        let first = engine
            .dispatch(resolution.clone())
            .await
            .expect("dismiss resolution");
        let replay = engine.dispatch(resolution).await.expect("dismiss replay");

        assert_eq!(replay.sequence, first.sequence);
        assert_eq!(
            engine.read_events(0).await.expect("events after").len(),
            before + 1
        );
        let dismissed = engine
            .repositories()
            .get_provider_turn_delivery("delivery-dismiss-first".to_owned())
            .await
            .expect("delivery")
            .expect("dismissed row");
        assert_eq!(dismissed.state, TurnDeliveryState::Dismissed);
        assert_eq!(
            dismissed.last_error.as_deref(),
            Some("provider rejected the request")
        );
        let active = engine
            .repositories()
            .list_provider_turn_deliveries(vec![
                TurnDeliveryState::Pending,
                TurnDeliveryState::Sending,
                TurnDeliveryState::Uncertain,
                TurnDeliveryState::Failed,
            ])
            .await
            .expect("active rows");
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].command_id, "delivery-dismiss-later");
        let message = engine
            .repositories()
            .get_message("delivery-dismiss-message".to_owned())
            .await
            .expect("message")
            .expect("dismissed message");
        assert_eq!(message.delivery_state.as_deref(), Some("dismissed"));
        assert_eq!(
            message.delivery_detail.as_deref(),
            Some("provider rejected the request")
        );
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn delivery_resolution_dismiss_cancels_pending_and_sending_messages() {
        for initial_state in [TurnDeliveryState::Pending, TurnDeliveryState::Sending] {
            let (engine, thread_id) = delivery_engine(TestHooks::default()).await;
            admit_delivery(
                &engine,
                "delivery-cancel",
                &thread_id,
                "delivery-cancel-message",
                "2026-08-01T00:00:01Z",
            )
            .await;
            if initial_state == TurnDeliveryState::Sending {
                engine
                    .repositories()
                    .claim_provider_turn(
                        "delivery-cancel".to_owned(),
                        "2026-08-01T00:00:02Z".to_owned(),
                    )
                    .await
                    .expect("claim cancellation fixture")
                    .expect("claimed cancellation fixture");
            }

            engine
                .dispatch(delivery_resolution(
                    "delivery-cancel-resolution",
                    &thread_id,
                    "delivery-cancel-message",
                    "dismiss",
                    "2026-08-01T00:00:03Z",
                ))
                .await
                .expect("cancel admitted delivery");

            let delivery = engine
                .repositories()
                .get_provider_turn_delivery("delivery-cancel".to_owned())
                .await
                .expect("cancelled delivery")
                .expect("cancelled delivery row");
            assert_eq!(delivery.state, TurnDeliveryState::Dismissed);
            engine.shutdown().await;
        }
    }

    #[tokio::test]
    async fn delivery_resolution_projector_failure_rolls_back_transition_and_receipt() {
        let hooks = TestHooks::default();
        let (engine, thread_id) = delivery_engine(hooks.clone()).await;
        admit_delivery(
            &engine,
            "delivery-resolution-rollback",
            &thread_id,
            "delivery-resolution-rollback-message",
            "2026-08-01T00:00:01Z",
        )
        .await;
        assert!(
            engine
                .transition_turn_delivery(TurnDeliveryTransition {
                    turn_id: None,
                    command_id: "delivery-resolution-rollback".to_owned(),
                    expected_states: vec![TurnDeliveryState::Pending],
                    expected_attempt: 0,
                    next_state: TurnDeliveryState::Uncertain,
                    detail: Some("unknown provider outcome".to_owned()),
                    updated_at: "2026-08-01T00:00:02Z".to_owned(),
                })
                .await
                .expect("mark uncertain")
        );
        hooks.fail_next_projector(
            "projection.thread-messages",
            Some("thread.turn-delivery-updated"),
        );
        let before = engine.read_events(0).await.expect("events before").len();
        let result = engine
            .dispatch(delivery_resolution(
                "delivery-resolution-rollback-command",
                &thread_id,
                "delivery-resolution-rollback-message",
                "retry",
                "2026-08-01T00:00:03Z",
            ))
            .await;

        assert!(matches!(
            result,
            Err(OrchestrationError::InjectedProjectorFailure { .. })
        ));
        assert_eq!(
            engine.read_events(0).await.expect("events after").len(),
            before
        );
        assert!(
            engine
                .repositories()
                .get_command_receipt("delivery-resolution-rollback-command".to_owned())
                .await
                .expect("receipt")
                .is_none()
        );
        let row = engine
            .repositories()
            .get_provider_turn_delivery("delivery-resolution-rollback".to_owned())
            .await
            .expect("delivery")
            .expect("rollback row");
        assert_eq!(row.state, TurnDeliveryState::Uncertain);
        assert_eq!(row.last_error.as_deref(), Some("unknown provider outcome"));
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn delivery_rejected_replay_matches_digest_and_conflicts_on_change() {
        let (engine, _) = delivery_engine(TestHooks::default()).await;
        let rejected = delivery_turn("delivery-rejected", "missing-thread", "same");
        let admission = delivery_admission(&rejected, "missing-thread");
        assert!(matches!(
            engine
                .dispatch_with_admission(rejected.clone(), admission.clone(), || {})
                .await,
            Err(OrchestrationError::Invariant { .. })
        ));
        assert!(matches!(
            engine
                .dispatch_with_admission(rejected, admission, || {})
                .await,
            Err(OrchestrationError::PreviouslyRejected { .. })
        ));
        let changed = delivery_turn("delivery-rejected", "missing-thread", "changed");
        assert!(matches!(
            engine
                .dispatch_with_admission(
                    changed.clone(),
                    delivery_admission(&changed, "missing-thread"),
                    || {},
                )
                .await,
            Err(OrchestrationError::CommandConflict { .. })
        ));
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn dispatch_commit_ownership_crosses_only_the_engine_admission_boundary() {
        use std::sync::atomic::{AtomicBool, Ordering};

        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let observer = database
            .enable_queue_backpressure_observation_for_integration_test()
            .expect("database observer");
        let engine = OrchestrationEngine::start(
            database.clone(),
            EngineOptions {
                queue_capacity: 1,
                ..EngineOptions::default()
            },
        )
        .await
        .expect("engine");

        let (entered_tx, entered_rx) = oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let blocker_database = database.clone();
        let blocker = tokio::spawn(async move {
            blocker_database
                .call(move |_| {
                    let _ = entered_tx.send(());
                    release_rx.recv().expect("release database blocker");
                    Ok(())
                })
                .await
        });
        entered_rx.await.expect("database blocker entered");

        let admitted_dropped = Arc::new(AtomicBool::new(false));
        let admitted_committed = Arc::new(AtomicBool::new(false));
        let admitted_probe = DropFlag(admitted_dropped.clone());
        let admitted_commit = admitted_committed.clone();
        let admitted_engine = engine.clone();
        let admitted = tokio::spawn(async move {
            admitted_engine
                .dispatch_with_commit(
                    project_create_command_at("admitted", "project-admitted", "C:/repo-admitted"),
                    move || {
                        admitted_commit.store(true, Ordering::SeqCst);
                        drop(admitted_probe);
                    },
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if database
                    .queue_backpressure_snapshot_for_integration_test()
                    .reserved_or_queued_jobs
                    == 1
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("admitted command reaches persistence");

        let queued_engine = engine.clone();
        let queued = tokio::spawn(async move {
            queued_engine
                .dispatch(project_create_command_at(
                    "queued",
                    "project-queued",
                    "C:/repo-queued",
                ))
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while engine.sender.capacity() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("engine queue fills");

        let pending_dropped = Arc::new(AtomicBool::new(false));
        let pending_committed = Arc::new(AtomicBool::new(false));
        let pending_probe = DropFlag(pending_dropped.clone());
        let pending_commit = pending_committed.clone();
        let pending_engine = engine.clone();
        let pending = tokio::spawn(async move {
            pending_engine
                .dispatch_with_commit(
                    project_create_command_at(
                        "not-admitted",
                        "project-not-admitted",
                        "C:/repo-not-admitted",
                    ),
                    move || {
                        pending_commit.store(true, Ordering::SeqCst);
                        drop(pending_probe);
                    },
                )
                .await
        });
        tokio::task::yield_now().await;
        pending.abort();
        let _ = pending.await;
        assert!(pending_dropped.load(Ordering::SeqCst));
        assert!(!pending_committed.load(Ordering::SeqCst));

        admitted.abort();
        let _ = admitted.await;
        assert!(
            !admitted_dropped.load(Ordering::SeqCst),
            "the worker owns an admitted command's commit guard"
        );
        release_tx.send(()).expect("release database");
        blocker.await.expect("blocker task").expect("blocker call");
        queued.await.expect("queued task").expect("queued command");
        tokio::time::timeout(Duration::from_secs(5), async {
            while !admitted_committed.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("admitted command commits");
        assert!(admitted_dropped.load(Ordering::SeqCst));
        assert!(
            engine
                .read_events(0)
                .await
                .expect("events")
                .iter()
                .all(|event| event.event.command_id.as_deref() != Some("not-admitted"))
        );

        drop(observer);
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn bootstrap_delivery_commits_thread_turn_receipt_and_outbox_before_effects() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct BootstrapEffectProbe(Arc<AtomicUsize>);

        impl ThreadTurnBootstrapEffects for BootstrapEffectProbe {
            fn prepare_worktree<'a>(
                &'a self,
                input: ThreadTurnStartBootstrapPrepareWorktree,
                _cancellation: &'a CancellationToken,
            ) -> BoxBootstrapFuture<'a, BootstrapWorktree> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    Ok(BootstrapWorktree {
                        repository_root: input.project_cwd.clone(),
                        branch: "coddy-studio/bootstrap".to_owned(),
                        path: format!("{}/bootstrap", input.project_cwd),
                        remove_branch: true,
                    })
                })
            }

            fn run_setup_script<'a>(
                &'a self,
                _input: BootstrapSetupInput,
            ) -> BoxBootstrapFuture<'a, BootstrapSetupResult> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(BootstrapSetupResult::NoScript) })
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
        let hooks = TestHooks::default();
        let engine = OrchestrationEngine::start(
            database,
            EngineOptions {
                test_hooks: hooks.clone(),
                ..EngineOptions::default()
            },
        )
        .await
        .expect("engine");
        engine
            .dispatch(project_create_command(
                "bootstrap-project",
                "bootstrap-project",
            ))
            .await
            .expect("project");

        let effects = Arc::new(AtomicUsize::new(0));
        engine.set_bootstrap_effects(Arc::new(BootstrapEffectProbe(effects.clone())));
        let command = serde_json::from_value::<OrchestrationCommand>(json!({
            "type":"thread.turn.start",
            "commandId":"bootstrap-delivery",
            "threadId":"bootstrap-thread",
            "message":{"messageId":"bootstrap-message","role":"user","text":"build","attachments":[]},
            "modelSelection":{"instanceId":"codex","model":"gpt-5"},
            "bootstrap":{
                "createThread":{
                    "projectId":"bootstrap-project",
                    "title":"Bootstrap",
                    "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                    "runtimeMode":"full-access",
                    "interactionMode":"default",
                    "branch":null,
                    "worktreePath":null,
                    "createdAt":"2026-08-01T00:00:01Z"
                },
                "prepareWorktree":{
                    "projectCwd":"C:/repo",
                    "baseBranch":"main",
                    "branch":"coddy-studio/bootstrap"
                },
                "runSetupScript":true
            },
            "createdAt":"2026-08-01T00:00:01Z"
        }))
        .expect("bootstrap turn");
        let admission = CommandAdmission {
            payload_digest: canonical_command_digest(&command).expect("digest"),
            attachment_refs: vec![],
            provider_turn: Some(NewProviderTurnDelivery {
                state: crate::orchestration::TurnDeliveryState::Pending,
                mode: crate::orchestration::TurnDeliveryMode::Start,
                command_id: "bootstrap-delivery".to_owned(),
                thread_id: "bootstrap-thread".to_owned(),
                message_id: "bootstrap-message".to_owned(),
                provider_instance_id: "codex".to_owned(),
                provider_kind: "codex".to_owned(),
                provider_session_id: None,
                delivery_key: "bootstrap-key".to_owned(),
                payload: serde_json::to_value(&command).expect("payload"),
                created_at: "2026-08-01T00:00:01Z".to_owned(),
            }),
        };
        let pause = hooks.pause_after_next_admission_commit();
        let dispatch_engine = engine.clone();
        let dispatch = tokio::spawn(async move {
            dispatch_engine
                .dispatch_with_admission(command, admission, || {})
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), pause.wait_until_entered())
            .await
            .expect("bootstrap admission commits");
        dispatch.abort();
        let _ = dispatch.await;

        assert_eq!(effects.load(Ordering::SeqCst), 0);
        let event_types = engine
            .read_events(0)
            .await
            .expect("events")
            .into_iter()
            .filter(|event| event.event.aggregate_id == "bootstrap-thread")
            .map(|event| event.event.event_type)
            .collect::<Vec<_>>();
        assert_eq!(
            event_types,
            [
                "thread.created",
                "thread.message-sent",
                "thread.turn-start-requested",
                "thread.turn-delivery-updated",
            ]
        );
        assert!(
            engine
                .repositories()
                .get_command_receipt("bootstrap-delivery".to_owned())
                .await
                .expect("receipt")
                .is_some()
        );
        assert_eq!(
            engine
                .repositories()
                .get_provider_turn_delivery("bootstrap-delivery".to_owned())
                .await
                .expect("outbox")
                .expect("delivery")
                .state,
            TurnDeliveryState::Pending
        );
        pause.release();
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn bootstrap_delivery_cancellation_before_enqueue_creates_nothing() {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let observer = database
            .enable_queue_backpressure_observation_for_integration_test()
            .expect("database observer");
        let engine = OrchestrationEngine::start(
            database.clone(),
            EngineOptions {
                queue_capacity: 1,
                ..EngineOptions::default()
            },
        )
        .await
        .expect("engine");
        engine
            .dispatch(project_create_command(
                "bootstrap-project",
                "bootstrap-project",
            ))
            .await
            .expect("project");

        let (entered_tx, entered_rx) = oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let blocker_database = database.clone();
        let blocker = tokio::spawn(async move {
            blocker_database
                .call(move |_| {
                    let _ = entered_tx.send(());
                    release_rx.recv().expect("release database blocker");
                    Ok(())
                })
                .await
        });
        entered_rx.await.expect("database blocker entered");
        let admitted_engine = engine.clone();
        let admitted = tokio::spawn(async move {
            admitted_engine
                .dispatch(project_create_command_at(
                    "queue-admitted",
                    "queue-admitted",
                    "C:/queue-admitted",
                ))
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while database
                .queue_backpressure_snapshot_for_integration_test()
                .reserved_or_queued_jobs
                != 1
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("admitted command reaches persistence");
        let queued_engine = engine.clone();
        let queued = tokio::spawn(async move {
            queued_engine
                .dispatch(project_create_command_at(
                    "queue-filled",
                    "queue-filled",
                    "C:/queue-filled",
                ))
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while engine.sender.capacity() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("engine queue fills");

        let command = serde_json::from_value::<OrchestrationCommand>(json!({
            "type":"thread.turn.start", "commandId":"bootstrap-not-enqueued",
            "threadId":"bootstrap-not-enqueued",
            "message":{"messageId":"bootstrap-message","role":"user","text":"build","attachments":[]},
            "bootstrap":{"createThread":{
                "projectId":"bootstrap-project", "title":"Bootstrap",
                "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access", "interactionMode":"default",
                "branch":null, "worktreePath":null, "createdAt":"2026-08-01T00:00:01Z"
            }},
            "createdAt":"2026-08-01T00:00:01Z"
        }))
        .expect("bootstrap command");
        let admission = CommandAdmission {
            payload_digest: canonical_command_digest(&command).expect("digest"),
            attachment_refs: vec![],
            provider_turn: Some(NewProviderTurnDelivery {
                state: crate::orchestration::TurnDeliveryState::Pending,
                mode: crate::orchestration::TurnDeliveryMode::Start,
                command_id: "bootstrap-not-enqueued".to_owned(),
                thread_id: "bootstrap-not-enqueued".to_owned(),
                message_id: "bootstrap-message".to_owned(),
                provider_instance_id: "codex".to_owned(),
                provider_kind: "codex".to_owned(),
                provider_session_id: None,
                delivery_key: "bootstrap-not-enqueued-key".to_owned(),
                payload: serde_json::to_value(&command).expect("payload"),
                created_at: "2026-08-01T00:00:01Z".to_owned(),
            }),
        };
        let pending_engine = engine.clone();
        let pending = tokio::spawn(async move {
            pending_engine
                .dispatch_with_admission(command, admission, || {})
                .await
        });
        tokio::task::yield_now().await;
        pending.abort();
        let _ = pending.await;

        release_tx.send(()).expect("release database");
        blocker.await.expect("blocker").expect("database call");
        admitted.await.expect("admitted").expect("admitted result");
        queued.await.expect("queued").expect("queued result");
        assert!(
            engine
                .repositories()
                .get_thread("bootstrap-not-enqueued".to_owned())
                .await
                .expect("thread lookup")
                .is_none()
        );
        assert!(
            engine
                .repositories()
                .get_command_receipt("bootstrap-not-enqueued".to_owned())
                .await
                .expect("receipt lookup")
                .is_none()
        );
        assert!(
            engine
                .repositories()
                .get_provider_turn_delivery("bootstrap-not-enqueued".to_owned())
                .await
                .expect("outbox lookup")
                .is_none()
        );

        drop(observer);
        engine.shutdown().await;
    }

    struct NoopBootstrapEffects;

    impl ThreadTurnBootstrapEffects for NoopBootstrapEffects {
        fn prepare_worktree<'a>(
            &'a self,
            input: ThreadTurnStartBootstrapPrepareWorktree,
            _cancellation: &'a CancellationToken,
        ) -> BoxBootstrapFuture<'a, BootstrapWorktree> {
            Box::pin(async move {
                Ok(BootstrapWorktree {
                    repository_root: input.project_cwd.clone(),
                    branch: input.base_branch,
                    path: input.project_cwd,
                    remove_branch: false,
                })
            })
        }

        fn run_setup_script<'a>(
            &'a self,
            _input: BootstrapSetupInput,
        ) -> BoxBootstrapFuture<'a, BootstrapSetupResult> {
            Box::pin(async { Ok(BootstrapSetupResult::NoScript) })
        }
    }

    struct NeverCompletingHistoricalRootEffects;

    impl ProjectCommandEffects for NeverCompletingHistoricalRootEffects {
        fn normalize_workspace_root_lexically(&self, workspace_root: &str) -> String {
            workspace_root.replace("/./", "/")
        }

        fn canonicalize_workspace_root<'a>(
            &'a self,
            workspace_root: &'a str,
            _allow_missing: bool,
        ) -> BoxProjectCommandFuture<'a, String> {
            if workspace_root == "historical/./workspace" {
                Box::pin(std::future::pending())
            } else {
                Box::pin(async { Ok("/canonical/requested-workspace".to_owned()) })
            }
        }

        fn prepare_project_create<'a>(
            &'a self,
            _workspace_root: &'a str,
            _create_if_missing: bool,
            _initialize_git: bool,
        ) -> BoxProjectCommandFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
    }

    #[derive(Default)]
    struct PausedProjectCreateEffects {
        canonicalize_entered: tokio::sync::Notify,
        canonicalize_release: tokio::sync::Notify,
        prepare_calls: AtomicUsize,
    }

    impl ProjectCommandEffects for PausedProjectCreateEffects {
        fn normalize_workspace_root_lexically(&self, workspace_root: &str) -> String {
            workspace_root.to_owned()
        }

        fn canonicalize_workspace_root<'a>(
            &'a self,
            workspace_root: &'a str,
            _allow_missing: bool,
        ) -> BoxProjectCommandFuture<'a, String> {
            Box::pin(async move {
                self.canonicalize_entered.notify_one();
                self.canonicalize_release.notified().await;
                Ok(workspace_root.to_owned())
            })
        }

        fn prepare_project_create<'a>(
            &'a self,
            _workspace_root: &'a str,
            _create_if_missing: bool,
            _initialize_git: bool,
        ) -> BoxProjectCommandFuture<'a, ()> {
            self.prepare_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }
    }

    #[derive(Default)]
    struct PausedPreparedProjectCreateEffects {
        prepare_entered: tokio::sync::Notify,
        prepare_release: tokio::sync::Notify,
    }

    impl ProjectCommandEffects for PausedPreparedProjectCreateEffects {
        fn normalize_workspace_root_lexically(&self, workspace_root: &str) -> String {
            workspace_root.to_owned()
        }

        fn canonicalize_workspace_root<'a>(
            &'a self,
            workspace_root: &'a str,
            _allow_missing: bool,
        ) -> BoxProjectCommandFuture<'a, String> {
            Box::pin(async move { Ok(workspace_root.to_owned()) })
        }

        fn prepare_project_create<'a>(
            &'a self,
            _workspace_root: &'a str,
            _create_if_missing: bool,
            _initialize_git: bool,
        ) -> BoxProjectCommandFuture<'a, ()> {
            Box::pin(async move {
                self.prepare_entered.notify_one();
                self.prepare_release.notified().await;
                Ok(())
            })
        }
    }

    #[tokio::test]
    async fn project_create_claims_command_identity_before_external_preparation() {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine starts");
        let effects = Arc::new(PausedProjectCreateEffects::default());
        engine.set_project_command_effects(effects.clone());
        let command: OrchestrationCommand = serde_json::from_value(json!({
            "type":"project.create",
            "commandId":"shared-side-effect-command",
            "projectId":"project-side-effect",
            "title":"Project",
            "workspaceRoot":"/server/resolved/workspace",
            "defaultModelSelection":null,
            "createWorkspaceRootIfMissing":true,
            "initializeGit":true,
            "createdAt":"2026-08-10T00:00:00Z"
        }))
        .expect("project create");
        let dispatch_engine = engine.clone();
        let dispatch = tokio::spawn(async move {
            dispatch_engine
                .dispatch_with_admission(
                    command,
                    CommandAdmission {
                        payload_digest: "generic-project-payload".to_owned(),
                        attachment_refs: Vec::new(),
                        provider_turn: None,
                    },
                    || {},
                )
                .await
        });
        effects.canonicalize_entered.notified().await;
        let removal_engine = engine.clone();
        let mut removal = tokio::spawn(async move {
            let claim = removal_engine
                .acquire_command_admission("shared-side-effect-command")
                .await?;
            removal_engine
                .reserve_worktree_removal_admission(
                    &claim,
                    "shared-side-effect-command",
                    "project-side-effect",
                    "removal-payload",
                )
                .await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut removal)
                .await
                .is_err(),
            "removal must wait while project creation owns external preparation"
        );
        effects.canonicalize_release.notify_one();

        dispatch
            .await
            .expect("dispatch joins")
            .expect("project creation retains its claim");
        assert_eq!(effects.prepare_calls.load(Ordering::SeqCst), 1);
        assert!(matches!(
            removal.await.expect("removal joins"),
            Err(OrchestrationError::CommandConflict { .. })
        ));
        let receipt = engine
            .repositories()
            .get_command_receipt("shared-side-effect-command".to_owned())
            .await
            .expect("receipt read")
            .expect("receipt");
        assert_eq!(receipt.status, "accepted");
        assert_eq!(
            receipt.payload_digest.as_deref(),
            Some("generic-project-payload")
        );
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn removal_cannot_take_command_identity_after_generic_external_preparation_begins() {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine starts");
        let effects = Arc::new(PausedPreparedProjectCreateEffects::default());
        engine.set_project_command_effects(effects.clone());
        let command: OrchestrationCommand = serde_json::from_value(json!({
            "type":"project.create",
            "commandId":"generic-first-side-effect-command",
            "projectId":"project-side-effect",
            "title":"Project",
            "workspaceRoot":"/server/resolved/generic-first-workspace",
            "defaultModelSelection":null,
            "createWorkspaceRootIfMissing":true,
            "initializeGit":true,
            "createdAt":"2026-08-10T00:00:00Z"
        }))
        .expect("project create");
        let dispatch_engine = engine.clone();
        let dispatch = tokio::spawn(async move {
            dispatch_engine
                .dispatch_with_admission(
                    command,
                    CommandAdmission {
                        payload_digest: "generic-project-payload".to_owned(),
                        attachment_refs: Vec::new(),
                        provider_turn: None,
                    },
                    || {},
                )
                .await
        });
        effects.prepare_entered.notified().await;

        let removal_engine = engine.clone();
        let mut removal = tokio::spawn(async move {
            let claim = removal_engine
                .acquire_command_admission("generic-first-side-effect-command")
                .await?;
            removal_engine
                .reserve_worktree_removal_admission(
                    &claim,
                    "generic-first-side-effect-command",
                    "project-side-effect",
                    "removal-payload",
                )
                .await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut removal)
                .await
                .is_err()
        );
        effects.prepare_release.notify_one();
        dispatch
            .await
            .expect("dispatch joins")
            .expect("generic command retains reservation");
        assert!(matches!(
            removal.await.expect("removal joins"),
            Err(OrchestrationError::CommandConflict { .. })
        ));
        let receipt = engine
            .repositories()
            .get_command_receipt("generic-first-side-effect-command".to_owned())
            .await
            .expect("receipt read")
            .expect("receipt");
        assert_eq!(receipt.status, "accepted");
        assert_eq!(
            receipt.payload_digest.as_deref(),
            Some("generic-project-payload")
        );
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn command_admission_claim_serializes_cleanup_waiters_and_does_not_leak_keys() {
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
        let command: OrchestrationCommand = serde_json::from_value(json!({
            "type":"project.delete",
            "commandId":"cleanup-owned-command",
            "projectId":"cleanup-owned-project",
            "force":true
        }))
        .expect("generic command");
        let claimant_a = engine
            .acquire_command_admission("cleanup-owned-command")
            .await
            .expect("claimant A owns command admission");
        assert!(
            engine
                .reserve_generic_command_admission(&claimant_a, &command, "cleanup-owned-digest")
                .await
                .expect("claimant A reserves")
        );

        let waiter_engine = engine.clone();
        let mut waiter_b = tokio::spawn(async move {
            waiter_engine
                .acquire_command_admission("cleanup-owned-command")
                .await
        });
        let cancelled_engine = engine.clone();
        let cancelled = tokio::spawn(async move {
            cancelled_engine
                .acquire_command_admission("cleanup-owned-command")
                .await
        });
        tokio::task::yield_now().await;
        cancelled.abort();
        assert!(
            cancelled
                .await
                .expect_err("cancelled waiter stops")
                .is_cancelled()
        );

        let removal_engine = engine.clone();
        let mut changed_removal = tokio::spawn(async move {
            let claim = removal_engine
                .acquire_command_admission("cleanup-owned-command")
                .await?;
            let reservation = removal_engine
                .reserve_worktree_removal_admission(
                    &claim,
                    "cleanup-owned-command",
                    "different-project",
                    "different-removal-digest",
                )
                .await;
            Ok::<_, OrchestrationError>((claim, reservation))
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut waiter_b)
                .await
                .is_err(),
            "identical claimant B must wait through A cleanup"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut changed_removal)
                .await
                .is_err(),
            "changed removal C must not inspect or reserve while A is live"
        );
        assert!(
            engine
                .release_generic_command_admission(&claimant_a, &command, "cleanup-owned-digest")
                .await
                .expect("claimant A performs exact cleanup")
        );
        drop(claimant_a);

        let claimant_b = waiter_b
            .await
            .expect("claimant B task joins")
            .expect("claimant B acquires after A cleanup");
        assert!(
            engine
                .reserve_generic_command_admission(&claimant_b, &command, "cleanup-owned-digest")
                .await
                .expect("claimant B resumes after A cleanup")
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut changed_removal)
                .await
                .is_err(),
            "changed removal C must remain blocked while B owns the retry"
        );
        assert!(
            engine
                .release_generic_command_admission(&claimant_b, &command, "cleanup-owned-digest")
                .await
                .expect("claimant B performs exact cleanup")
        );
        drop(claimant_b);
        let (_claimant_c, removal_reservation) = changed_removal
            .await
            .expect("changed removal task joins")
            .expect("changed removal acquires after generic terminality");
        assert!(
            removal_reservation
                .expect("changed removal may reserve after cleanup")
                .0
                .is_none()
        );

        for index in 0..64 {
            let claim = engine
                .acquire_command_admission(&format!("bounded-command-{index}"))
                .await
                .expect("bounded claim");
            drop(claim);
        }
        assert!(
            engine.command_admission_registry_len_for_test() <= 1,
            "dead command IDs must not accumulate in the weak registry"
        );
        engine.shutdown().await;

        let restarted = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine restarts with a fresh command gate");
        let restart_claim = restarted
            .acquire_command_admission("cleanup-owned-command")
            .await
            .expect("fresh process gate acquires durable command ID");
        let resumed = restarted
            .reserve_worktree_removal_admission(
                &restart_claim,
                "cleanup-owned-command",
                "different-project",
                "different-removal-digest",
            )
            .await
            .expect("durable reserved removal remains resumable");
        assert!(resumed.0.is_none());
        restarted.shutdown().await;
    }

    #[tokio::test]
    async fn admitted_worker_retains_command_claim_after_rpc_cancellation() {
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
            database,
            EngineOptions {
                test_hooks: hooks.clone(),
                ..EngineOptions::default()
            },
        )
        .await
        .expect("engine starts");
        let command =
            project_create_command_at("handoff-command", "handoff-project", "C:/handoff-project");
        let digest = canonical_command_digest(&command).expect("command digest");
        let claim = engine
            .acquire_command_admission("handoff-command")
            .await
            .expect("RPC owns command claim");
        let pause = hooks.pause_before_next_command_persist();
        let dispatch_engine = engine.clone();
        let dispatch = tokio::spawn(async move {
            dispatch_engine
                .dispatch_with_admission_and_command_claim(
                    command,
                    CommandAdmission {
                        payload_digest: digest,
                        attachment_refs: Vec::new(),
                        provider_turn: None,
                    },
                    claim,
                    || {},
                )
                .await
        });
        pause.wait_until_entered().await;
        dispatch.abort();
        assert!(dispatch.await.expect_err("RPC task stops").is_cancelled());

        let waiter_engine = engine.clone();
        let mut waiter = tokio::spawn(async move {
            waiter_engine
                .acquire_command_admission("handoff-command")
                .await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut waiter)
                .await
                .is_err(),
            "worker handoff must retain the claim after caller cancellation"
        );
        pause.release();
        let replay_claim = waiter
            .await
            .expect("waiter task joins")
            .expect("claim releases after worker terminality");
        let receipt = engine
            .repositories()
            .get_command_receipt("handoff-command".to_owned())
            .await
            .expect("receipt read")
            .expect("accepted receipt");
        assert_eq!(receipt.status, "accepted");
        drop(replay_claim);
        engine
            .acquire_command_admission("handoff-command")
            .await
            .expect("claim releases exactly once without deadlock");
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn cancelled_receipt_lookup_releases_its_command_claim_while_database_is_blocked() {
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
            .expect("engine");
        let observer = database
            .enable_queue_backpressure_observation_for_integration_test()
            .expect("exclusive database queue observer");
        let (entered_tx, entered_rx) = oneshot::channel();
        let release = Arc::new((StdMutex::new(false), Condvar::new()));
        let blocker_release = release.clone();
        let blocker_database = database.clone();
        let blocker = tokio::spawn(async move {
            blocker_database
                .call(move |_| {
                    let _ = entered_tx.send(());
                    let (released, changed) = blocker_release.as_ref();
                    let mut released = released.lock().expect("database blocker mutex");
                    while !*released {
                        released = changed
                            .wait(released)
                            .expect("database blocker mutex after wait");
                    }
                    Ok(())
                })
                .await
        });
        entered_rx.await.expect("database blocker enters");

        let claim = engine
            .acquire_command_admission("cancelled-receipt-lookup")
            .await
            .expect("initial command claim");
        let cancellation = CancellationToken::new();
        let lookup_engine = engine.clone();
        let lookup_cancellation = cancellation.clone();
        let lookup = tokio::spawn(async move {
            let result = lookup_engine
                .get_command_receipt_with_claim_cancellation(
                    &claim,
                    "cancelled-receipt-lookup",
                    &lookup_cancellation,
                )
                .await;
            drop(claim);
            result
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while database
                .queue_backpressure_snapshot_for_integration_test()
                .reserved_or_queued_jobs
                < 1
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("receipt lookup queues behind the database blocker");
        cancellation.cancel();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(5), lookup)
                .await
                .expect("cancelled lookup completes")
                .expect("lookup task joins"),
            Err(OrchestrationError::Cancelled)
        ));
        let next_claim = tokio::time::timeout(
            Duration::from_secs(5),
            engine.acquire_command_admission("cancelled-receipt-lookup"),
        )
        .await
        .expect("next claimant does not wait for the blocked receipt lookup")
        .expect("next claimant acquires");
        drop(next_claim);

        {
            let (released, changed) = release.as_ref();
            *released.lock().expect("database blocker mutex") = true;
            changed.notify_one();
        }
        blocker
            .await
            .expect("database blocker joins")
            .expect("database blocker succeeds");
        drop(observer);
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn owner_waiter_reloads_the_current_source_path_after_an_ordinary_owner_move() {
        let temp = TempDir::new().expect("temporary workspace directory");
        let project_root = temp.path().join("project");
        let original = project_root.join("original");
        let intermediate = project_root.join("intermediate");
        let final_path = project_root.join("final");
        for path in [&project_root, &original, &intermediate, &final_path] {
            std::fs::create_dir_all(path).expect("workspace path");
        }
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
            database,
            EngineOptions {
                test_hooks: hooks.clone(),
                ..EngineOptions::default()
            },
        )
        .await
        .expect("engine starts");
        engine
            .dispatch(project_create_command_at(
                "owner-reload-project",
                "owner-reload-project",
                &project_root.to_string_lossy(),
            ))
            .await
            .expect("project");
        engine
            .dispatch(
                serde_json::from_value(json!({
                    "type":"thread.create",
                    "commandId":"owner-reload-thread-create",
                    "threadId":"owner-reload-thread",
                    "projectId":"owner-reload-project",
                    "title":"Owner",
                    "kind":"workspace",
                    "modelSelection":{},
                    "runtimeMode":"full-access",
                    "interactionMode":"default",
                    "branch":null,
                    "worktreePath":original,
                    "createdAt":"2026-08-10T00:00:01Z"
                }))
                .expect("thread create"),
            )
            .await
            .expect("thread");

        let pause = hooks.pause_before_next_command_persist();
        let first_engine = engine.clone();
        let first_target = intermediate.clone();
        let first = tokio::spawn(async move {
            first_engine
                .dispatch(
                    serde_json::from_value(json!({
                        "type":"thread.meta.update",
                        "commandId":"owner-reload-first",
                        "threadId":"owner-reload-thread",
                        "worktreePath":first_target
                    }))
                    .expect("first retarget"),
                )
                .await
        });
        pause.wait_until_entered().await;

        let final_blocker = engine
            .acquire_workspace_removal_ownership(&final_path)
            .await
            .expect("block the second retarget before its first ownership validation");
        let final_contender = engine
            .workspace_ownership
            .contender_checkpoint(&final_path)
            .await;
        let second_engine = engine.clone();
        let second_target = final_path.clone();
        let second = tokio::spawn(async move {
            second_engine
                .dispatch(
                    serde_json::from_value(json!({
                        "type":"thread.meta.update",
                        "commandId":"owner-reload-second",
                        "threadId":"owner-reload-thread",
                        "worktreePath":second_target
                    }))
                    .expect("second retarget"),
                )
                .await
        });
        tokio::time::timeout(
            Duration::from_secs(5),
            engine
                .workspace_ownership
                .wait_for_contender_after(&final_contender),
        )
        .await
        .expect("second retarget begins its stale ownership acquisition");
        pause.release();
        first
            .await
            .expect("first retarget joins")
            .expect("first retarget");
        let removal_lease = tokio::time::timeout(
            Duration::from_secs(5),
            engine.acquire_workspace_removal_ownership(&intermediate),
        )
        .await
        .expect("removal acquires the new owner path")
        .expect("removal ownership");
        let intermediate_contender = engine
            .workspace_ownership
            .contender_checkpoint(&intermediate)
            .await;
        drop(final_blocker);
        tokio::time::timeout(
            Duration::from_secs(5),
            engine
                .workspace_ownership
                .wait_for_contender_after(&intermediate_contender),
        )
        .await
        .expect("the stale waiter reloads and fences the newly current source path");
        assert!(!second.is_finished());
        drop(removal_lease);
        second
            .await
            .expect("second retarget joins")
            .expect("second retarget");
        let expected_final_path = final_path.to_string_lossy().into_owned();
        assert_eq!(
            engine
                .repositories()
                .get_thread("owner-reload-thread".to_owned())
                .await
                .expect("thread read")
                .expect("thread")
                .worktree_path
                .as_deref(),
            Some(expected_final_path.as_str())
        );
        engine.shutdown().await;
    }

    fn projector_event(event_type: &str, payload: Value, metadata: Value) -> OrchestrationEvent {
        OrchestrationEvent {
            sequence: 99,
            event: NewOrchestrationEvent {
                event_id: Uuid::new_v4().to_string(),
                event_type: event_type.to_owned(),
                aggregate_kind: "thread".to_owned(),
                aggregate_id: "projector-thread".to_owned(),
                occurred_at: "2026-07-10T10:00:00.000Z".to_owned(),
                command_id: Some("projector-edge".to_owned()),
                causation_event_id: None,
                correlation_id: None,
                payload,
                metadata,
            },
        }
    }

    #[tokio::test]
    async fn project_create_without_an_explicit_selection_returns_its_canonical_default_thread() {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine starts");

        let command = json!({
            "type": "project.create",
            "commandId": "project-default-selection",
            "projectId": "project-default-selection",
            "title": "Project",
            "workspaceRoot": "C:/repo",
            "defaultModelSelection": null,
            "createdAt": "2026-07-20T00:00:00.000Z"
        });
        let result = engine
            .dispatch(serde_json::from_value(command.clone()).expect("command decodes"))
            .await
            .expect("project creates");
        assert_eq!(
            result.project_id.as_deref(),
            Some("project-default-selection")
        );

        let snapshot = load_snapshot(&engine.repositories())
            .await
            .expect("snapshot loads");
        let default_thread = snapshot
            .threads
            .iter()
            .find(|thread| thread.kind == "default")
            .expect("default thread");
        assert_eq!(
            default_thread.model_selection,
            json!({"instanceId":"codex","model":"gpt-5.4"})
        );
        assert_eq!(
            result.thread_id.as_deref(),
            Some(default_thread.thread_id.as_str())
        );

        let replay = engine
            .dispatch(serde_json::from_value(command).expect("command decodes"))
            .await
            .expect("accepted project create replays");
        assert_eq!(replay.thread_id, result.thread_id);
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn project_create_canonical_duplicate_returns_existing_default_thread() {
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine starts");
        engine.set_project_command_effects(Arc::new(NeverCompletingHistoricalRootEffects));

        let create = |command_id: &str, project_id: &str, workspace_root: &str| {
            serde_json::from_value(json!({
                "type": "project.create",
                "commandId": command_id,
                "projectId": project_id,
                "title": "Project",
                "workspaceRoot": workspace_root,
                "defaultModelSelection": null,
                "createdAt": "2026-07-20T00:00:00.000Z"
            }))
            .expect("command decodes")
        };
        let created = engine
            .dispatch(create("create-project", "project-1", "requested-workspace"))
            .await
            .expect("project creates");
        let duplicate = engine
            .dispatch(create(
                "create-project-duplicate",
                "project-duplicate",
                "other-workspace",
            ))
            .await
            .expect("canonical duplicate resolves");

        assert_eq!(duplicate.project_id.as_deref(), Some("project-1"));
        assert_eq!(duplicate.thread_id, created.thread_id);
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn historical_cached_workspace_root_inspection_timeout_uses_lexical_identity() {
        let mut model = CommandModel {
            projects: BTreeMap::from([(
                "historical-project".to_owned(),
                ProjectState {
                    workspace_root: "historical/./workspace".to_owned(),
                    worktree_discovery: default_worktree_discovery(),
                    deleted_at: None,
                },
            )]),
            threads: BTreeMap::new(),
            project_roots_canonicalized: false,
        };
        let mut command = serde_json::from_value::<OrchestrationCommand>(json!({
            "type": "project.create",
            "commandId": "create-requested-workspace",
            "projectId": "requested-project",
            "title": "Requested Project",
            "workspaceRoot": "requested-workspace",
            "createWorkspaceRootIfMissing": true,
            "initializeGit": false,
            "createdAt": "2026-07-17T00:00:00.000Z"
        }))
        .expect("project create command");

        canonicalize_project_command_with_historical_timeout(
            &mut model,
            &mut command,
            Some(&NeverCompletingHistoricalRootEffects),
            std::time::Duration::ZERO,
        )
        .await
        .expect("requested workspace root remains strict and resolves");

        assert_eq!(
            model
                .projects
                .get("historical-project")
                .expect("historical project")
                .workspace_root,
            "historical/workspace"
        );
        assert!(model.project_roots_canonicalized);
        let OrchestrationCommand::ProjectCreate { workspace_root, .. } = command else {
            panic!("expected project create command");
        };
        assert_eq!(workspace_root, "/canonical/requested-workspace");
    }

    /// A delivery recorded delivered because the provider already had the message (found by an
    /// explicit retry) clears its own pending turn marker, which would
    /// otherwise stay running and hold queued follow-ups. Another message's marker is kept, and so
    /// is the marker of an ordinary delivered turn, which its own session update settles.
    #[tokio::test]
    async fn a_delivery_delivered_without_a_turn_clears_its_pending_turn() {
        let database = Database::open_in_memory().await.expect("database");
        let running = database
            .call(|connection| {
                run_migrations(connection, None)?;
                let transaction = connection.transaction()?;
                let requested = |message_id: &str| {
                    projector_event(
                        "thread.turn-start-requested",
                        json!({
                            "threadId":"projector-thread", "messageId":message_id,
                            "createdAt":"2026-07-10T10:00:00.000Z"
                        }),
                        json!({}),
                    )
                };
                let delivered = |message_id: &str, found_without_turn: bool| {
                    projector_event(
                        "thread.turn-delivery-updated",
                        json!({
                            "threadId":"projector-thread", "messageId":message_id,
                            "state":"delivered", "provider":"opencode",
                            "updatedAt":"2026-07-10T10:00:01.000Z"
                        }),
                        if found_without_turn {
                            json!({ "foundWithoutTurn": true })
                        } else {
                            json!({})
                        },
                    )
                };
                let mut context = ProjectionContext::default();
                apply_turns_projector_tx(&transaction, &requested("other"), &mut context)?;
                apply_turns_projector_tx(&transaction, &delivered("found", true), &mut context)?;
                apply_turns_projector_tx(&transaction, &delivered("other", false), &mut context)?;
                let kept: i64 = transaction.query_row(
                    "SELECT COUNT(*) FROM projection_turns WHERE turn_id IS NULL AND state = 'running'",
                    [],
                    |row| row.get(0),
                )?;
                apply_turns_projector_tx(&transaction, &requested("found"), &mut context)?;
                apply_turns_projector_tx(&transaction, &delivered("found", true), &mut context)?;
                let left: i64 = transaction.query_row(
                    "SELECT COUNT(*) FROM projection_turns WHERE turn_id IS NULL AND state = 'running'",
                    [],
                    |row| row.get(0),
                )?;
                Ok((kept, left))
            })
            .await
            .expect("projection");
        assert_eq!(running, (1, 0));
    }

    #[tokio::test]
    async fn projector_edges_reject_corrupt_payloads_and_preserve_resolved_approvals() {
        const CREATED_AT: &str = "2026-07-10T10:00:00.000Z";

        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                let transaction = connection.transaction()?;

                apply_projects_projector_tx(
                    &transaction,
                    &projector_event(
                        "project.created",
                        json!({
                            "projectId":"projector-project",
                            "title":"Projector",
                            "workspaceRoot":"C:/projector",
                            "defaultModelSelection":null,
                            "createdAt":CREATED_AT,
                            "updatedAt":CREATED_AT
                        }),
                        json!({}),
                    ),
                )?;

                for error in [
                    apply_plans_projector_tx(
                        &transaction,
                        &projector_event(
                            "thread.proposed-plan-upserted",
                            json!({"threadId":"projector-thread"}),
                            json!({}),
                        ),
                    ),
                    apply_activities_projector_tx(
                        &transaction,
                        &projector_event(
                            "thread.activity-appended",
                            json!({"threadId":"projector-thread"}),
                            json!({}),
                        ),
                    ),
                    apply_sessions_projector_tx(
                        &transaction,
                        &projector_event(
                            "thread.session-set",
                            json!({"threadId":"projector-thread"}),
                            json!({}),
                        ),
                    ),
                    apply_turns_projector_tx(
                        &transaction,
                        &projector_event(
                            "thread.session-set",
                            json!({"threadId":"projector-thread"}),
                            json!({}),
                        ),
                        &mut ProjectionContext::default(),
                    ),
                    apply_pending_approvals_projector_tx(
                        &transaction,
                        &projector_event(
                            "thread.activity-appended",
                            json!({"threadId":"projector-thread"}),
                            json!({}),
                        ),
                    ),
                ] {
                    assert!(matches!(error, Err(PersistenceError::Corrupt(_))));
                }

                apply_pending_approvals_projector_tx(
                    &transaction,
                    &projector_event(
                        "thread.approval-response-requested",
                        json!({
                            "requestId":"resolved-request",
                            "threadId":"projector-thread",
                            "decision":"approved",
                            "createdAt":CREATED_AT
                        }),
                        json!({}),
                    ),
                )?;
                apply_pending_approvals_projector_tx(
                    &transaction,
                    &projector_event(
                        "thread.activity-appended",
                        json!({
                            "threadId":"projector-thread",
                            "activity":{
                                "id":"approval-activity",
                                "kind":"approval.requested",
                                "createdAt":CREATED_AT,
                                "payload":{}
                            }
                        }),
                        json!({"requestId":"resolved-request"}),
                    ),
                )?;
                let status: String = transaction.query_row(
                    "SELECT status FROM projection_pending_approvals WHERE request_id = ?",
                    ["resolved-request"],
                    |row| row.get(0),
                )?;
                assert_eq!(status, "resolved");

                let missing_request_id = apply_pending_approvals_projector_tx(
                    &transaction,
                    &projector_event(
                        "thread.activity-appended",
                        json!({
                            "threadId":"projector-thread",
                            "activity":{
                                "id":"missing-request",
                                "kind":"approval.requested",
                                "createdAt":CREATED_AT,
                                "payload":{}
                            }
                        }),
                        json!({}),
                    ),
                );
                assert!(matches!(
                    missing_request_id,
                    Err(PersistenceError::Corrupt(_))
                ));

                transaction.rollback()?;
                Ok(())
            })
            .await
            .expect("projector edges execute");
    }

    #[tokio::test]
    async fn context_window_projection_keeps_latest_valid_per_turn() {
        const CREATED_AT: &str = "2026-08-08T00:00:00.000Z";

        let temp = TempDir::new().expect("temporary database directory");
        let database_path = temp.path().join("context-window.sqlite");
        let database = Database::create_new(&database_path)
            .await
            .expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine starts");
        for command in [
            json!({
                "type":"project.create", "commandId":"context-project", "projectId":"p1",
                "title":"Project", "workspaceRoot":"C:/repo", "createdAt":CREATED_AT
            }),
            json!({
                "type":"thread.create", "commandId":"context-thread", "threadId":"t1",
                "projectId":"p1", "title":"Thread", "kind":"workspace",
                "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access", "interactionMode":"default", "branch":null,
                "worktreePath":null, "createdAt":CREATED_AT
            }),
            json!({
                "type":"thread.turn.diff.complete", "commandId":"context-turn-0", "threadId":"t1",
                "turnId":"turn-0", "checkpointTurnCount":1, "checkpointRef":"checkpoint-0",
                "status":"ready", "files":[], "assistantMessageId":null,
                "completedAt":CREATED_AT, "createdAt":CREATED_AT
            }),
            json!({
                "type":"thread.turn.diff.complete", "commandId":"context-turn-1", "threadId":"t1",
                "turnId":"turn-1", "checkpointTurnCount":2, "checkpointRef":"checkpoint-1",
                "status":"ready", "files":[], "assistantMessageId":null,
                "completedAt":CREATED_AT, "createdAt":CREATED_AT
            }),
        ] {
            engine
                .dispatch(serde_json::from_value(command).expect("command decodes"))
                .await
                .expect("fixture command succeeds");
        }

        let database = engine.repositories().database().clone();
        database
            .call(|connection| {
                let transaction = connection.transaction()?;
                let activities = [
                    json!({
                        "id":"activity-cw-valid", "tone":"info",
                        "kind":"context-window.updated", "summary":"Context window updated",
                        "payload":{"usedTokens":1_000}, "turnId":"turn-1", "sequence":1,
                        "createdAt":CREATED_AT
                    }),
                    json!({
                        "id":"activity-cw-malformed", "tone":"info",
                        "kind":"context-window.updated", "summary":"Context window updated",
                        "payload":{}, "turnId":"turn-1", "sequence":2, "createdAt":CREATED_AT
                    }),
                    json!({
                        "id":"activity-other-turn", "tone":"info",
                        "kind":"context-window.updated", "summary":"Context window updated",
                        "payload":{"usedTokens":500}, "turnId":"turn-0", "sequence":3,
                        "createdAt":CREATED_AT
                    }),
                    json!({
                        "id":"activity-cw-latest", "tone":"info",
                        "kind":"context-window.updated", "summary":"Context window updated",
                        "payload":{"usedTokens":2_000}, "turnId":"turn-1", "sequence":4,
                        "createdAt":CREATED_AT
                    }),
                    json!({
                        "id":"activity-cw-latest", "tone":"info",
                        "kind":"context-window.updated", "summary":"Context window updated",
                        "payload":{}, "turnId":"turn-1", "sequence":5, "createdAt":CREATED_AT
                    }),
                ];
                let mut last_sequence = 0;
                for activity in activities {
                    let activity_id = required_str(&activity, "id")?;
                    let saved = append_event_tx(
                        &transaction,
                        make_event(
                            "thread.activity-appended",
                            "thread",
                            "t1",
                            CREATED_AT,
                            &format!("provider:{activity_id}"),
                            json!({}),
                            json!({"threadId":"t1", "activity":activity}),
                        ),
                    )?;
                    apply_activities_projector_tx(&transaction, &saved)?;
                    last_sequence = saved.sequence;
                }
                upsert_projection_state_tx(
                    &transaction,
                    "projection.thread-activities",
                    last_sequence,
                    CREATED_AT,
                )?;
                transaction.commit()?;
                Ok(())
            })
            .await
            .expect("activity projection transaction commits");
        engine.shutdown().await;
        drop(engine);
        drop(database);

        let reopened_database = Database::open_existing(&database_path)
            .await
            .expect("database reopens");
        let reopened = OrchestrationEngine::start(reopened_database, EngineOptions::default())
            .await
            .expect("engine restarts");
        let snapshot = load_snapshot(&reopened.repositories())
            .await
            .expect("snapshot loads after restart");
        assert_eq!(
            snapshot
                .activities
                .iter()
                .map(|activity| activity.activity_id.as_str())
                .collect::<Vec<_>>(),
            [
                "activity-cw-malformed",
                "activity-other-turn",
                "activity-cw-latest"
            ]
        );
        let latest = snapshot
            .activities
            .iter()
            .find(|activity| activity.activity_id == "activity-cw-latest")
            .expect("latest valid context activity survives");
        assert_eq!(latest.payload, json!({"usedTokens":2_000}));
        assert_eq!(latest.sequence, Some(4));

        reopened
            .dispatch(OrchestrationCommand::ThreadRevertComplete {
                command_id: "context-revert".to_owned(),
                thread_id: "t1".to_owned(),
                turn_count: 1,
                created_at: CREATED_AT.to_owned(),
            })
            .await
            .expect("revert projects");
        let snapshot = load_snapshot(&reopened.repositories())
            .await
            .expect("snapshot loads after revert");
        assert_eq!(
            snapshot
                .activities
                .iter()
                .map(|activity| activity.activity_id.as_str())
                .collect::<Vec<_>>(),
            ["activity-other-turn"]
        );
        reopened.shutdown().await;
    }

    #[tokio::test]
    async fn history_import_refuses_a_thread_that_already_started_a_turn() {
        const CREATED_AT: &str = "2026-10-08T10:00:00.000Z";
        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let engine = OrchestrationEngine::start(database, EngineOptions::default())
            .await
            .expect("engine");
        for value in [
            json!({"type":"project.create","commandId":"project","projectId":"p1","title":"P","workspaceRoot":"C:/repo","createdAt":CREATED_AT}),
            json!({"type":"thread.create","commandId":"used","threadId":"used","projectId":"p1","title":"Used","modelSelection":{"instanceId":"codex","model":"gpt-5"},"runtimeMode":"full-access","createdAt":CREATED_AT}),
            json!({"type":"thread.create","commandId":"fresh","threadId":"fresh","projectId":"p1","title":"Fresh","modelSelection":{"instanceId":"codex","model":"gpt-5"},"runtimeMode":"full-access","createdAt":CREATED_AT}),
            // Another client starts a turn before the import's history lands.
            json!({"type":"thread.turn.start","commandId":"turn","threadId":"used","message":{"messageId":"m-user","role":"user","text":"hello","attachments":[]},"createdAt":CREATED_AT}),
        ] {
            engine
                .dispatch(serde_json::from_value(value).unwrap())
                .await
                .expect("setup command");
        }
        let import = |thread_id: &str| OrchestrationCommand::ThreadHistoryImport {
            command_id: format!("{thread_id}:history"),
            thread_id: thread_id.to_owned(),
            messages: vec![ImportedThreadMessage {
                message_id: format!("{thread_id}:000000"),
                role: "user".to_owned(),
                text: "from the CLI".to_owned(),
                created_at: CREATED_AT.to_owned(),
            }],
        };
        assert!(matches!(
            engine.dispatch(import("used")).await,
            Err(OrchestrationError::Invariant { .. })
        ));
        engine
            .dispatch(import("fresh"))
            .await
            .expect("an unused thread accepts its history");
        engine.shutdown().await;
    }

    #[tokio::test]
    async fn unit_build_covers_engine_projection_failure_bootstrap_and_lifecycle_paths() {
        const CREATED_AT: &str = "2026-07-10T10:00:00.000Z";

        let database = Database::open_in_memory().await.expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let hooks = TestHooks::default();
        hooks.fail_next_projector("projection.projects", Some("project.created"));
        let engine = OrchestrationEngine::start(
            database,
            EngineOptions {
                queue_capacity: 0,
                test_hooks: hooks,
            },
        )
        .await
        .expect("engine starts");
        let command =
            |value| serde_json::from_value::<OrchestrationCommand>(value).expect("command decodes");

        let helper_command = command(json!({
            "type":"project.delete",
            "commandId":"helper-command",
            "projectId":"missing-project"
        }));
        let missing = OptionalNullable::<Value>::Missing;
        assert!(missing.is_missing());
        assert!(optional_nullable_is_missing(&missing));
        assert_eq!(
            required_command_string(&helper_command, &json!({"key":"value"}), "key").unwrap(),
            "value"
        );
        assert!(required_command_string(&helper_command, &json!({}), "key").is_err());
        assert!(invariant::<()>(&helper_command, "injected".to_owned()).is_err());
        let json_error = serde_json::from_str::<Value>("{").unwrap_err();
        assert!(matches!(
            to_corrupt_error(json_error),
            PersistenceError::Corrupt(_)
        ));
        let json_error = serde_json::from_str::<Value>("{").unwrap_err();
        assert!(matches!(
            to_sql_error(json_error),
            rusqlite::Error::FromSqlConversionFailure(..)
        ));
        TestHooks::default().fail_next_projector("projection.threads".to_owned(), None);

        let project = json!({
            "type":"project.create",
            "commandId":"project",
            "projectId":"p1",
            "title":"Project",
            "workspaceRoot":"C:/repo",
            "createWorkspaceRootIfMissing":true,
            "defaultModelSelection":null,
            "createdAt":CREATED_AT
        });
        assert!(matches!(
            engine.dispatch(command(project.clone())).await,
            Err(OrchestrationError::InjectedProjectorFailure { .. })
        ));
        engine
            .dispatch(command(project))
            .await
            .expect("project retry succeeds");

        engine
            .dispatch(command(json!({
                "type":"thread.turn.start",
                "commandId":"bootstrap-turn",
                "threadId":"bootstrap-thread",
                "message":{"messageId":"bootstrap-message","role":"user","text":"build","attachments":[]},
                "bootstrap":{
                    "createThread":{
                        "projectId":"p1",
                        "title":"Bootstrap",
                        "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                        "runtimeMode":"full-access",
                        "interactionMode":"default",
                        "branch":null,
                        "worktreePath":null,
                        "createdAt":CREATED_AT
                    },
                    "prepareWorktree":{
                        "projectCwd":"C:/repo",
                        "baseBranch":"main"
                    }
                },
                "createdAt":CREATED_AT
            })))
            .await
            .expect("bootstrap admission does not run effects");

        let commands = [
            json!({"type":"project.meta.update","commandId":"project-meta","projectId":"p1","title":"Renamed","workspaceRoot":"C:/repo-renamed","defaultModelSelection":{"instanceId":"codex","model":"gpt-5"}}),
            json!({"type":"thread.create","commandId":"thread","threadId":"t1","projectId":"p1","title":"Thread","modelSelection":{"instanceId":"codex","model":"gpt-5"},"runtimeMode":"full-access","interactionMode":"default","branch":null,"worktreePath":null,"createdAt":CREATED_AT}),
            json!({"type":"thread.meta.update","commandId":"thread-meta","threadId":"t1","title":"Thread 2","branch":"main","worktreePath":null}),
            json!({"type":"thread.runtime-mode.set","commandId":"runtime-mode","threadId":"t1","runtimeMode":"approval-required","createdAt":CREATED_AT}),
            json!({"type":"thread.interaction-mode.set","commandId":"interaction-mode","threadId":"t1","interactionMode":"plan","createdAt":CREATED_AT}),
            json!({"type":"thread.turn.start","commandId":"turn-start","threadId":"t1","message":{"messageId":"m-user","role":"user","text":"hello","attachments":[]},"titleSeed":"Coverage title","createdAt":CREATED_AT}),
            json!({"type":"thread.session.set","commandId":"session","threadId":"t1","session":{"threadId":"t1","status":"running","providerName":"codex","providerInstanceId":"codex","runtimeMode":"approval-required","activeTurnId":"turn-1","lastError":null,"updatedAt":CREATED_AT},"createdAt":CREATED_AT}),
            json!({"type":"thread.message.assistant.delta","commandId":"delta","threadId":"t1","messageId":"m-assistant","delta":"hello","turnId":"turn-1","createdAt":CREATED_AT}),
            json!({"type":"thread.proposed-plan.upsert","commandId":"plan","threadId":"t1","proposedPlan":{"id":"plan-1","turnId":"turn-1","planMarkdown":"Do it","createdAt":CREATED_AT,"updatedAt":CREATED_AT},"createdAt":CREATED_AT}),
            json!({"type":"thread.activity.append","commandId":"activity","threadId":"t1","activity":{"id":"activity-1","tone":"tool","kind":"command","summary":"ran","payload":{"requestId":"request-1"},"turnId":"turn-1","createdAt":CREATED_AT},"createdAt":CREATED_AT}),
            json!({"type":"thread.turn.diff.complete","commandId":"diff","threadId":"t1","turnId":"turn-1","completedAt":CREATED_AT,"checkpointRef":"checkpoint-1","status":"ready","files":[{"path":"a.rs","kind":"modified","additions":2,"deletions":1}],"assistantMessageId":"m-assistant","checkpointTurnCount":1,"createdAt":CREATED_AT}),
            json!({"type":"thread.message.assistant.complete","commandId":"complete","threadId":"t1","messageId":"m-assistant","turnId":"turn-1","createdAt":CREATED_AT}),
        ];
        for value in commands {
            engine
                .dispatch(command(value))
                .await
                .expect("command succeeds");
        }

        assert!(matches!(
            engine
                .dispatch(command(json!({
                    "type":"project.delete",
                    "commandId":"project-delete-not-empty",
                    "projectId":"p1"
                })))
                .await,
            Err(OrchestrationError::Invariant { .. })
        ));
        assert!(matches!(
            engine
                .dispatch(command(json!({
                    "type":"thread.turn.start",
                    "commandId":"missing-source-plan",
                    "threadId":"t1",
                    "message":{"messageId":"missing-plan-message","role":"user","text":"implement","attachments":[]},
                    "sourceProposedPlan":{"threadId":"t1","planId":"missing-plan"},
                    "createdAt":CREATED_AT
                })))
                .await,
            Err(OrchestrationError::Invariant { .. })
        ));

        engine
            .repositories()
            .upsert_command_receipt(CommandReceipt {
                command_id: "previously-rejected".to_owned(),
                aggregate_kind: "project".to_owned(),
                aggregate_id: "p1".to_owned(),
                accepted_at: CREATED_AT.to_owned(),
                result_sequence: 0,
                status: "rejected".to_owned(),
                error: None,
                payload_digest: None,
            })
            .await
            .expect("receipt fixture inserts");
        assert!(matches!(
            engine
                .dispatch(command(json!({
                    "type":"project.meta.update",
                    "commandId":"previously-rejected",
                    "projectId":"p1",
                    "title":"Ignored"
                })))
                .await,
            Err(OrchestrationError::PreviouslyRejected { detail, .. })
                if detail == "Previously rejected."
        ));

        let mut subscriber = engine.subscribe_events();
        engine
            .dispatch(command(json!({
                "type":"thread.session.stop",
                "commandId":"session-stop",
                "threadId":"t1",
                "createdAt":CREATED_AT
            })))
            .await
            .expect("session stops");
        assert_eq!(
            subscriber
                .recv()
                .await
                .expect("streamed event")
                .event
                .event_type,
            "thread.session-stop-requested"
        );

        let events = engine.read_events(0).await.expect("events");
        assert!(events.len() >= 18);
        for value in [
            json!({"type":"thread.proposed-plan.upsert","commandId":"plan-2","threadId":"t1","proposedPlan":{"id":"plan-2","turnId":"turn-2","planMarkdown":"Do the second thing","createdAt":CREATED_AT,"updatedAt":CREATED_AT},"createdAt":CREATED_AT}),
            json!({"type":"thread.turn.start","commandId":"turn-start-2","threadId":"t1","message":{"messageId":"m-user-2","role":"user","text":"continue","attachments":[]},"createdAt":CREATED_AT}),
            json!({"type":"thread.turn.diff.complete","commandId":"diff-2","threadId":"t1","turnId":"turn-2","completedAt":CREATED_AT,"checkpointRef":"checkpoint-2","status":"ready","files":[],"assistantMessageId":null,"checkpointTurnCount":2,"createdAt":CREATED_AT}),
            json!({"type":"thread.create","commandId":"thread-2","threadId":"t2","projectId":"p1","title":"Thread 2","modelSelection":{"instanceId":"codex","model":"gpt-5"},"runtimeMode":"full-access","interactionMode":"default","branch":null,"worktreePath":null,"createdAt":CREATED_AT}),
            json!({"type":"thread.session.set","commandId":"session-2","threadId":"t2","session":{"threadId":"t2","status":"running","providerName":"codex","providerInstanceId":"codex","runtimeMode":"full-access","activeTurnId":null,"lastError":null,"updatedAt":CREATED_AT},"createdAt":CREATED_AT}),
            json!({"type":"thread.create","commandId":"thread-3","threadId":"t4","projectId":"p1","title":"Thread 3","modelSelection":{"instanceId":"codex","model":"gpt-5"},"runtimeMode":"full-access","interactionMode":"default","branch":null,"worktreePath":null,"createdAt":CREATED_AT}),
            json!({"type":"thread.session.set","commandId":"session-3","threadId":"t4","session":{"threadId":"t4","status":"ready","providerName":"codex","providerInstanceId":"codex","runtimeMode":"full-access","activeTurnId":null,"lastError":null,"updatedAt":CREATED_AT},"createdAt":CREATED_AT}),
        ] {
            engine
                .dispatch(command(value))
                .await
                .expect("additional projection command succeeds");
        }
        for request_id in ["approval-b", "approval-a"] {
            engine
                .repositories()
                .upsert_pending_approval(ProjectionPendingApproval {
                    request_id: request_id.to_owned(),
                    thread_id: "t1".to_owned(),
                    turn_id: Some("turn-2".to_owned()),
                    status: "pending".to_owned(),
                    decision: None,
                    created_at: CREATED_AT.to_owned(),
                    resolved_at: None,
                })
                .await
                .expect("approval fixture inserts");
        }
        let snapshot = load_snapshot(&engine.repositories())
            .await
            .expect("snapshot");
        assert_eq!(snapshot.projects[0].title, "Renamed");
        assert!(
            snapshot
                .threads
                .iter()
                .any(|thread| thread.thread_id == "t1")
        );
        assert!(
            snapshot
                .messages
                .iter()
                .any(|message| { message.message_id == "m-assistant" && message.text == "hello" })
        );
        assert_eq!(snapshot.diffs.len(), 0);
        assert_eq!(
            required_str(&json!({"value":"text"}), "value").unwrap(),
            "text"
        );
        assert!(required_str(&json!({}), "value").is_err());
        assert_eq!(required_i64(&json!({"value":7}), "value").unwrap(), 7);
        assert!(required_i64(&json!({}), "value").is_err());

        let effects = Arc::new(NoopBootstrapEffects);
        let bootstrap_cancellation = CancellationToken::new();
        let worktree = effects
            .prepare_worktree(
                ThreadTurnStartBootstrapPrepareWorktree {
                    project_cwd: "C:/repo".to_owned(),
                    base_branch: "main".to_owned(),
                    branch: None,
                    start_from_origin: None,
                },
                &bootstrap_cancellation,
            )
            .await
            .expect("noop worktree");
        assert_eq!(
            effects
                .run_setup_script(BootstrapSetupInput {
                    thread_id: "t1".to_owned(),
                    project_id: Some("p1".to_owned()),
                    project_cwd: Some("C:/repo".to_owned()),
                    worktree_path: worktree.path.clone(),
                })
                .await,
            Ok(BootstrapSetupResult::NoScript)
        );
        let poisoned_effects = engine.bootstrap_effects.clone();
        let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _guard = poisoned_effects.lock().expect("mutex initially healthy");
            panic!("poison bootstrap effects mutex");
        }));
        assert!(panic_result.is_err());
        assert!(engine.bootstrap_effects().is_none());
        engine.set_bootstrap_effects(effects);
        assert!(engine.bootstrap_effects().is_some());

        engine.shutdown().await;
        assert!(matches!(
            engine
                .dispatch(command(json!({
                    "type":"thread.archive",
                    "commandId":"after-shutdown",
                    "threadId":"t1"
                })))
                .await,
            Err(OrchestrationError::Cancelled)
        ));
    }
}
