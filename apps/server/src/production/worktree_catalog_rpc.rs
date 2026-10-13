use std::{
    collections::HashSet,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::{
    sync::{Mutex, Semaphore, TryAcquireError, oneshot, watch},
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use super::repository_identity::{
    GitConfigStamp, RepositoryIdentityThrottle, git_config_stamp, reconcile_repository_identity,
};
use crate::{
    crypto::sha256_hex,
    diagnostics::profiler::worktree_audit::record_removal_outcome,
    git::{
        CreateWorktreeInput, GitCommandError, GitPrunableWorktree, GitRepository,
        GitWorktreeInventory, GitWorktreeRecord, GitWorktreeRemovalInspection, StatusBroadcaster,
        canonical_worktree_path_key, git_worktree_prune_impact_digest, host_path_platform,
        normalize_worktree_path_key, worktree_key, worktree_repository_key,
    },
    orchestration::{
        CommandAdmission, OrchestrationCommand, OrchestrationEngine, OrchestrationError,
        canonical_command_digest,
        engine::{CommandAdmissionClaim, OptionalNullable, WorkspaceOwnershipLease},
    },
    persistence::{CommandReceipt, Repositories},
    rpc::{RpcRegistry, RpcRequest, RpcResult, RpcStreamChunk},
    worktree_catalog::{
        AdoptionValidationError, AdoptionValidationErrorReason, CatalogError, CatalogErrorReason,
        CatalogFuture, CatalogHealthySnapshotObserver, CatalogRefreshTrigger,
        WorkspaceRemovalIdentity, WorktreeAdoptionState, WorktreeCatalogService,
        WorktreeCatalogSnapshot, WorktreeDirectoryState, WorktreeRegistrationState,
    },
};

const MAX_BASELINE_PATHS: usize = 512;
const PRODUCTION_MAX_IN_FLIGHT_WORKTREE_OPERATIONS: usize = 64;
const REMOVAL_ADMISSION_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
/// The profiler's name for `worktree.removeFromBibCode`, beside the `worktree.remove` modes.
const DETACH_ONLY: &str = "detach-only";

pub type WorktreeRemovalQuiesceFuture =
    Pin<Box<dyn Future<Output = WorktreeRemovalQuiesceLease> + Send + 'static>>;
pub type WorktreeRemovalLiveSessionsFuture =
    Pin<Box<dyn Future<Output = Result<Vec<String>, String>> + Send + 'static>>;
pub type WorktreeRemovalCleanupAdmissionFuture = Pin<
    Box<
        dyn Future<
                Output = Result<
                    WorktreeRemovalCleanupAdmission,
                    WorktreeRemovalCleanupAdmissionError,
                >,
            > + Send
            + 'static,
    >,
>;

#[derive(Clone)]
pub struct WorktreeRemovalCleanupAdmission {
    _resource: Arc<dyn Send + Sync + 'static>,
}

impl WorktreeRemovalCleanupAdmission {
    fn unlimited() -> Self {
        Self {
            _resource: Arc::new(()),
        }
    }

    pub(crate) fn retaining(resource: impl Send + Sync + 'static) -> Self {
        Self {
            _resource: Arc::new(resource),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorktreeRemovalCleanupAdmissionError {
    Capacity,
}

pub struct WorktreeRemovalQuiesceLease {
    orphan_cleanup_pending: bool,
    cancellation: CancellationToken,
    detached: bool,
}

#[derive(Clone)]
pub struct WorktreeRemovalQuiesceRequest {
    identity: WorkspaceRemovalIdentity,
    project_id: String,
    repository_key: Option<String>,
    known_thread_ids: Vec<String>,
}

impl WorktreeRemovalQuiesceRequest {
    #[must_use]
    pub fn project(
        identity: WorkspaceRemovalIdentity,
        project_id: String,
        known_thread_ids: Vec<String>,
    ) -> Self {
        Self {
            identity,
            project_id,
            repository_key: None,
            known_thread_ids,
        }
    }

    #[must_use]
    pub fn repository(
        identity: WorkspaceRemovalIdentity,
        project_id: String,
        repository_key: String,
        known_thread_ids: Vec<String>,
    ) -> Self {
        Self {
            identity,
            project_id,
            repository_key: Some(repository_key),
            known_thread_ids,
        }
    }

    pub(crate) fn identity(&self) -> &WorkspaceRemovalIdentity {
        &self.identity
    }

    pub(crate) fn project_id(&self) -> &str {
        &self.project_id
    }

    pub(crate) fn repository_key(&self) -> Option<&str> {
        self.repository_key.as_deref()
    }

    pub(crate) fn known_thread_ids(&self) -> &[String] {
        &self.known_thread_ids
    }

    pub(crate) fn replace_identity(&mut self, identity: WorkspaceRemovalIdentity) {
        self.identity = identity;
    }
}

impl WorktreeRemovalQuiesceLease {
    #[must_use]
    pub fn complete() -> Self {
        Self {
            orphan_cleanup_pending: false,
            cancellation: CancellationToken::new(),
            detached: false,
        }
    }

    #[must_use]
    pub fn pending(cancellation: CancellationToken) -> Self {
        Self {
            orphan_cleanup_pending: true,
            cancellation,
            detached: false,
        }
    }

    #[must_use]
    pub fn orphan_cleanup_pending(&self) -> bool {
        self.orphan_cleanup_pending
    }

    pub fn commit_detached(mut self) {
        self.detached = true;
    }
}

impl Drop for WorktreeRemovalQuiesceLease {
    fn drop(&mut self) {
        if !self.detached {
            self.cancellation.cancel();
        }
    }
}

pub trait WorktreeRemovalQuiescer: Send + Sync + 'static {
    fn live_session_thread_ids(&self, thread_ids: Vec<String>)
    -> WorktreeRemovalLiveSessionsFuture;

    fn admit_cleanup(&self) -> WorktreeRemovalCleanupAdmissionFuture {
        Box::pin(async { Ok(WorktreeRemovalCleanupAdmission::unlimited()) })
    }

    fn quiesce(
        &self,
        admission: WorktreeRemovalCleanupAdmission,
        request: WorktreeRemovalQuiesceRequest,
    ) -> WorktreeRemovalQuiesceFuture;
}

pub type WorktreeRemovalGitFuture<T> =
    Pin<Box<dyn Future<Output = Result<T, GitCommandError>> + Send + 'static>>;

pub trait WorktreeRemovalGit: Send + Sync + 'static {
    fn inventory(
        &self,
        anchor: PathBuf,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<GitWorktreeInventory>;
    fn inspect(
        &self,
        anchor: PathBuf,
        record: GitWorktreeRecord,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<GitWorktreeRemovalInspection>;
    fn preview_prune(
        &self,
        anchor: PathBuf,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<Vec<GitPrunableWorktree>>;
    fn remove(
        &self,
        anchor: PathBuf,
        record: GitWorktreeRecord,
        force_dirty: bool,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<()>;
    fn prune(
        &self,
        anchor: PathBuf,
        record: GitWorktreeRecord,
        expected_impact_digest: String,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<()>;
}

impl WorktreeRemovalGit for GitRepository {
    fn inventory(
        &self,
        anchor: PathBuf,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<GitWorktreeInventory> {
        let repository = self.clone();
        Box::pin(async move { repository.worktree_inventory(&anchor, &cancellation).await })
    }

    fn inspect(
        &self,
        anchor: PathBuf,
        record: GitWorktreeRecord,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<GitWorktreeRemovalInspection> {
        let repository = self.clone();
        Box::pin(async move {
            repository
                .inspect_worktree_removal(&anchor, &record, &cancellation)
                .await
        })
    }

    fn preview_prune(
        &self,
        anchor: PathBuf,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<Vec<GitPrunableWorktree>> {
        let repository = self.clone();
        Box::pin(async move {
            repository
                .preview_worktree_prune(&anchor, &cancellation)
                .await
        })
    }

    fn remove(
        &self,
        anchor: PathBuf,
        record: GitWorktreeRecord,
        force_dirty: bool,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<()> {
        let repository = self.clone();
        Box::pin(async move {
            repository
                .remove_worktree_verified(&anchor, &record, force_dirty, &cancellation)
                .await
        })
    }

    fn prune(
        &self,
        anchor: PathBuf,
        record: GitWorktreeRecord,
        expected_impact_digest: String,
        cancellation: CancellationToken,
    ) -> WorktreeRemovalGitFuture<()> {
        let repository = self.clone();
        Box::pin(async move {
            repository
                .prune_worktrees_verified(&anchor, &record, &expected_impact_digest, &cancellation)
                .await
        })
    }
}

#[derive(Default)]
struct NoopWorktreeRemovalQuiescer;

impl WorktreeRemovalQuiescer for NoopWorktreeRemovalQuiescer {
    fn live_session_thread_ids(
        &self,
        _thread_ids: Vec<String>,
    ) -> WorktreeRemovalLiveSessionsFuture {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn quiesce(
        &self,
        _admission: WorktreeRemovalCleanupAdmission,
        _request: WorktreeRemovalQuiesceRequest,
    ) -> WorktreeRemovalQuiesceFuture {
        Box::pin(async { WorktreeRemovalQuiesceLease::complete() })
    }
}

#[derive(Clone)]
pub struct WorktreeCatalogRpcServices {
    catalog: WorktreeCatalogService,
    orchestration: OrchestrationEngine,
    creation_git: Option<Arc<GitRepository>>,
    removal_quiescer: Arc<dyn WorktreeRemovalQuiescer>,
    removal_git: Option<Arc<dyn WorktreeRemovalGit>>,
    status_broadcaster: Option<StatusBroadcaster>,
    operations: WorktreeCatalogOperationRuntime,
    #[cfg(test)]
    removal_admission_timeout: Duration,
    #[cfg(test)]
    removal_drain_entered: Option<Arc<Semaphore>>,
}

#[derive(Clone)]
pub struct WorktreeCatalogOperationRuntime {
    state: Arc<Mutex<WorktreeCatalogOperationState>>,
    /// Cancelled by `shutdown` so background work stops with the runtime.
    cancellation: CancellationToken,
    admission: Arc<Semaphore>,
}

struct WorktreeCatalogOperationState {
    accepting: bool,
    tasks: Vec<JoinHandle<()>>,
}

struct CatalogMutationOutcome {
    result: RpcResult,
    affected_paths: Vec<PathBuf>,
    executed: bool,
}

impl CatalogMutationOutcome {
    fn not_executed(result: RpcResult) -> Self {
        Self {
            result,
            affected_paths: Vec::new(),
            executed: false,
        }
    }

    async fn finish(self, services: &WorktreeCatalogRpcServices) -> RpcResult {
        if let Some(paths) = self.status_mutation_paths() {
            services.notify_status_mutations(paths.to_vec()).await;
        }
        self.result
    }

    fn status_mutation_paths(&self) -> Option<&[PathBuf]> {
        self.executed.then_some(self.affected_paths.as_slice())
    }
}

impl WorktreeCatalogOperationRuntime {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(WorktreeCatalogOperationState {
                accepting: true,
                tasks: Vec::new(),
            })),
            cancellation: CancellationToken::new(),
            admission: Arc::new(Semaphore::new(PRODUCTION_MAX_IN_FLIGHT_WORKTREE_OPERATIONS)),
        }
    }

    /// Runs best-effort follow-up work owned by the runtime, outside the caller's locks, and drops
    /// it once shutdown has begun. `shutdown` cancels the token and waits for it.
    async fn spawn_background<F, Fut>(&self, work: F)
    where
        F: FnOnce(CancellationToken) -> Fut,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let mut state = self.state.lock().await;
        if !state.accepting {
            return;
        }
        state.tasks.retain(|task| !task.is_finished());
        state
            .tasks
            .push(tokio::spawn(work(self.cancellation.child_token())));
    }

    pub(crate) async fn run(
        &self,
        operation: impl Future<Output = RpcResult> + Send + 'static,
    ) -> RpcResult {
        let operation_permit = match self.admission.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(TryAcquireError::NoPermits) => {
                return Err(encode(WorktreeOperationError::capacity()));
            }
            Err(TryAcquireError::Closed) => {
                return Err(encode(WorktreeOperationError::shutting_down()));
            }
        };
        let (result_sender, result_receiver) = oneshot::channel();
        {
            let mut state = self.state.lock().await;
            if !state.accepting {
                return Err(encode(WorktreeOperationError::shutting_down()));
            }
            state.tasks.retain(|task| !task.is_finished());
            state.tasks.push(tokio::spawn(async move {
                let _operation_permit = operation_permit;
                let _ = result_sender.send(operation.await);
            }));
        }
        result_receiver.await.unwrap_or_else(|_| {
            Err(encode(CatalogError::new(
                CatalogErrorReason::Internal,
                "The server-owned worktree operation ended without a result.",
            )))
        })
    }

    pub async fn shutdown(&self) {
        let tasks = {
            let mut state = self.state.lock().await;
            state.accepting = false;
            self.cancellation.cancel();
            self.admission.close();
            std::mem::take(&mut state.tasks)
        };
        for task in tasks {
            let _ = task.await;
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorktreeOperationError {
    #[serde(rename = "_tag")]
    tag: &'static str,
    reason: WorktreeOperationErrorReason,
    message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "kebab-case")]
enum WorktreeOperationErrorReason {
    OperationCapacity,
    OperationShuttingDown,
}

impl WorktreeOperationError {
    fn capacity() -> Self {
        Self {
            tag: "WorktreeOperationError",
            reason: WorktreeOperationErrorReason::OperationCapacity,
            message: "The server is already processing the maximum number of worktree operations."
                .to_owned(),
        }
    }

    fn shutting_down() -> Self {
        Self {
            tag: "WorktreeOperationError",
            reason: WorktreeOperationErrorReason::OperationShuttingDown,
            message: "The worktree operation runtime is shutting down.".to_owned(),
        }
    }
}

impl WorktreeCatalogRpcServices {
    #[must_use]
    pub fn new(catalog: WorktreeCatalogService, orchestration: OrchestrationEngine) -> Self {
        let operations = WorktreeCatalogOperationRuntime::new();
        catalog.set_healthy_snapshot_observer(Arc::new(BranchReconciliationObserver {
            orchestration: orchestration.clone(),
            git: catalog.git_repository(),
            identity_throttle: RepositoryIdentityThrottle::default(),
            config_stamp: read_git_config_stamp,
            stamp_reads: Arc::default(),
            stamp_timeout: CONFIG_STAMP_TIMEOUT,
            operations: operations.clone(),
        }));
        let creation_git = catalog.git_repository();
        let removal_git = creation_git
            .clone()
            .map(|repository| repository as Arc<dyn WorktreeRemovalGit>);
        Self {
            catalog,
            orchestration,
            creation_git,
            removal_quiescer: Arc::new(NoopWorktreeRemovalQuiescer),
            removal_git,
            status_broadcaster: None,
            operations,
            #[cfg(test)]
            removal_admission_timeout: REMOVAL_ADMISSION_DRAIN_TIMEOUT,
            #[cfg(test)]
            removal_drain_entered: None,
        }
    }

    #[must_use]
    pub fn with_removal_quiescer(mut self, quiescer: Arc<dyn WorktreeRemovalQuiescer>) -> Self {
        self.removal_quiescer = quiescer;
        self
    }

    #[must_use]
    pub fn with_removal_git(mut self, git: Arc<dyn WorktreeRemovalGit>) -> Self {
        self.removal_git = Some(git);
        self
    }

    #[must_use]
    pub fn with_status_broadcaster(mut self, broadcaster: StatusBroadcaster) -> Self {
        self.status_broadcaster = Some(broadcaster);
        self
    }

    #[must_use]
    pub fn operation_runtime(&self) -> WorktreeCatalogOperationRuntime {
        self.operations.clone()
    }

    pub(crate) fn catalog(&self) -> &WorktreeCatalogService {
        &self.catalog
    }

    pub(crate) fn status_broadcaster(&self) -> Option<&StatusBroadcaster> {
        self.status_broadcaster.as_ref()
    }

    /// Caller retains the catalog project/repository lock and operation runtime.
    /// The same managed creation transaction owns Git rollback and the durable owner.
    pub(crate) async fn create_managed_from_ref_locked(
        &self,
        project_id: &str,
        branch: &str,
        cancellation: &CancellationToken,
    ) -> RpcResult {
        let defaults = self
            .orchestration
            .repositories()
            .list_threads_by_project(project_id.to_owned())
            .await
            .map_err(|_| {
                encode(adoption_error(
                    WorktreeAdoptionErrorReason::Internal,
                    "Workspace defaults could not be read.",
                    None,
                ))
            })?
            .into_iter()
            .find(|thread| thread.kind == "default" && thread.deleted_at.is_none())
            .ok_or_else(|| {
                encode(adoption_error(
                    WorktreeAdoptionErrorReason::ProjectNotFound,
                    "The project's default workspace is unavailable.",
                    None,
                ))
            })?;
        let input = WorktreeCreateManagedInput {
            command_id: uuid::Uuid::new_v4().to_string(),
            project_id: project_id.to_owned(),
            thread_id: uuid::Uuid::new_v4().to_string(),
            title: branch.to_owned(),
            ref_name: branch.to_owned(),
            new_ref_name: None,
            base_ref_name: Some(branch.to_owned()),
            thread_defaults: WorktreeAdoptThreadDefaults {
                model_selection: defaults.model_selection,
                runtime_mode: defaults.runtime_mode,
                interaction_mode: defaults.interaction_mode,
            },
        };
        let digest = canonical_command_digest(&input).map_err(|_| {
            encode(adoption_error(
                WorktreeAdoptionErrorReason::Internal,
                "The managed creation payload could not be admitted.",
                None,
            ))
        })?;
        let claim =
            acquire_worktree_command_claim(&self.orchestration, &input.command_id, cancellation)
                .await
                .map_err(|error| encode(adoption_orchestration_error(error, None)))?;
        let write_services = Self {
            creation_git: self
                .creation_git
                .as_ref()
                .map(|repository| Arc::new(repository.for_checkout_write())),
            ..self.clone()
        };
        create_managed_worktree_locked(&write_services, input, digest, claim, cancellation)
            .await
            .finish(self)
            .await
    }

    async fn notify_status_mutations(&self, paths: Vec<PathBuf>) {
        let Some(broadcaster) = &self.status_broadcaster else {
            return;
        };
        let mut notified = HashSet::new();
        let paths = paths
            .into_iter()
            .filter(|path| notified.insert(path.clone()))
            .collect::<Vec<_>>();
        for path in &paths {
            let mutation = broadcaster.begin_mutation(path).await;
            mutation.finish().await;
        }
        broadcaster
            .invalidate_fetch_after_catalog_mutation(&paths)
            .await;
    }
}

#[derive(Clone)]
struct BranchReconciliationObserver {
    orchestration: OrchestrationEngine,
    git: Option<Arc<GitRepository>>,
    identity_throttle: RepositoryIdentityThrottle,
    /// Reads the primary checkout's `.git/config` stamp; a seam so tests can stall it.
    config_stamp: ConfigStampSource,
    /// Projects with a stamp read still running; see `StampReadGuard`.
    stamp_reads: Arc<std::sync::Mutex<HashSet<String>>>,
    stamp_timeout: Duration,
    operations: WorktreeCatalogOperationRuntime,
}

type ConfigStampSource =
    fn(PathBuf) -> Pin<Box<dyn Future<Output = Option<GitConfigStamp>> + Send>>;

fn read_git_config_stamp(
    checkout: PathBuf,
) -> Pin<Box<dyn Future<Output = Option<GitConfigStamp>> + Send>> {
    Box::pin(async move { git_config_stamp(&checkout).await })
}

/// Held by the tracked read task, so it lasts until the physical read ends or shutdown cancels it (a
/// timeout only stops the waiter): a hung mount leaves at most one stuck read per project.
struct StampReadGuard {
    reads: Arc<std::sync::Mutex<HashSet<String>>>,
    project_id: String,
}

impl StampReadGuard {
    fn begin(reads: &Arc<std::sync::Mutex<HashSet<String>>>, project_id: &str) -> Option<Self> {
        let inserted = reads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(project_id.to_owned());
        inserted.then(|| Self {
            reads: reads.clone(),
            project_id: project_id.to_owned(),
        })
    }
}

impl Drop for StampReadGuard {
    fn drop(&mut self) {
        self.reads
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.project_id);
    }
}

/// A slow or disconnected network mount must not hold the stamp read open; an unknown stamp falls
/// back to the interval throttle.
const CONFIG_STAMP_TIMEOUT: Duration = Duration::from_secs(3);

impl CatalogHealthySnapshotObserver for BranchReconciliationObserver {
    fn observe(
        &self,
        project_id: String,
        snapshot: Arc<WorktreeCatalogSnapshot>,
    ) -> CatalogFuture<()> {
        let observer = self.clone();
        Box::pin(async move {
            observer.reconcile(project_id, snapshot).await;
        })
    }
}

impl BranchReconciliationObserver {
    /// At most once per interval per project, or at once when the primary checkout's
    /// `.git/config` changed, and off the catalog's refresh lock.
    async fn refresh_repository_identity(
        &self,
        project_id: &str,
        snapshot: &WorktreeCatalogSnapshot,
    ) {
        let Some(git) = self.git.clone() else {
            return;
        };
        // The primary checkout's config is the one every worktree of the repository reads `origin`
        // from, so this catches `git remote set-url` run in any of them. The stat can block on a
        // network mount, so it runs in the tracked background task, never on the catalog's refresh.
        let primary = snapshot
            .worktrees
            .iter()
            .find(|worktree| worktree.is_primary)
            .map(|primary| PathBuf::from(&primary.path));
        let orchestration = self.orchestration.clone();
        let throttle = self.identity_throttle.clone();
        let config_stamp = self.config_stamp;
        let stamp_reads = self.stamp_reads.clone();
        let stamp_timeout = self.stamp_timeout;
        let operations = self.operations.clone();
        let project_id = project_id.to_owned();
        self.operations
            .spawn_background(move |cancellation| async move {
                let config = match primary {
                    Some(path) => match StampReadGuard::begin(&stamp_reads, &project_id) {
                        // A read from an earlier scan is still stuck: the stamp is unknown, and
                        // the interval throttle decides without starting a second read.
                        None => None,
                        Some(guard) => {
                            let (sender, receiver) = oneshot::channel();
                            operations
                                .spawn_background(move |cancellation| async move {
                                    let _guard = guard;
                                    tokio::select! {
                                        biased;
                                        () = cancellation.cancelled() => {}
                                        stamp = config_stamp(path) => {
                                            let _ = sender.send(stamp);
                                        }
                                    }
                                })
                                .await;
                            tokio::select! {
                                biased;
                                () = cancellation.cancelled() => return,
                                // A timeout stops only the wait: the read keeps its slot.
                                stamp = tokio::time::timeout(stamp_timeout, receiver) => {
                                    stamp.ok().and_then(Result::ok).flatten()
                                }
                            }
                        }
                    },
                    None => None,
                };
                if !throttle.admit(&project_id, std::time::Instant::now(), config) {
                    return;
                }
                reconcile_repository_identity(&orchestration, &git, &project_id, &cancellation)
                    .await;
            })
            .await;
    }

    async fn reconcile(&self, project_id: String, snapshot: Arc<WorktreeCatalogSnapshot>) {
        if !snapshot.authoritative
            || !matches!(
                snapshot.scan_status,
                crate::worktree_catalog::CatalogScanStatus::Ready
            )
        {
            return;
        }
        self.refresh_repository_identity(&project_id, &snapshot)
            .await;
        let threads = match self
            .orchestration
            .repositories()
            .list_threads_by_project(project_id.clone())
            .await
        {
            Ok(threads) => threads,
            Err(error) => {
                tracing::warn!(project_id, %error, "could not load adopted threads for branch reconciliation");
                return;
            }
        };
        let mut candidates = snapshot
            .worktrees
            .iter()
            .filter_map(|descriptor| {
                (descriptor.adoption_state == WorktreeAdoptionState::Active)
                    .then_some(descriptor.adopted_thread_id.as_deref())
                    .flatten()
                    .map(|thread_id| {
                        (
                            thread_id.to_owned(),
                            descriptor.branch.clone(),
                            descriptor.head.clone(),
                        )
                    })
            })
            .collect::<Vec<_>>();
        candidates.sort_by(|left, right| left.0.cmp(&right.0));
        candidates.dedup_by(|left, right| left.0 == right.0);
        for (thread_id, branch, head) in candidates {
            let Some(thread) = threads.iter().find(|thread| {
                thread.thread_id == thread_id
                    && thread.kind == "workspace"
                    && thread.archived_at.is_none()
                    && thread.deleted_at.is_none()
                    && thread.worktree_path.is_some()
            }) else {
                continue;
            };
            if thread.branch == branch {
                continue;
            }
            let command_id =
                branch_reconciliation_command_id(&thread_id, branch.as_deref(), head.as_deref());
            if let Err(error) = self
                .orchestration
                .dispatch(OrchestrationCommand::WorktreeBranchReconcileResolved {
                    command_id,
                    project_id: project_id.clone(),
                    thread_id: thread_id.clone(),
                    branch,
                })
                .await
            {
                tracing::warn!(project_id, thread_id, %error, "adopted worktree branch reconciliation failed");
            }
        }
    }
}

fn branch_reconciliation_command_id(
    thread_id: &str,
    branch: Option<&str>,
    head: Option<&str>,
) -> String {
    let mut identity = b"bibcode.worktree.branch-reconcile.v1\0".to_vec();
    identity.extend_from_slice(branch.unwrap_or("<detached>").as_bytes());
    identity.push(0);
    identity.extend_from_slice(head.unwrap_or("<unknown>").as_bytes());
    format!(
        "worktree-branch-reconcile:{thread_id}:{}",
        sha256_hex(identity)
    )
}

async fn acquire_worktree_command_claim(
    orchestration: &OrchestrationEngine,
    command_id: &str,
    cancellation: &CancellationToken,
) -> Result<CommandAdmissionClaim, OrchestrationError> {
    tokio::select! {
        biased;
        () = cancellation.cancelled() => Err(OrchestrationError::Cancelled),
        claim = orchestration.acquire_command_admission(command_id) => claim,
    }
}

pub fn register_worktree_catalog_rpc(
    registry: &mut RpcRegistry,
    services: WorktreeCatalogRpcServices,
) {
    let stream_services = services.clone();
    registry.register_latest_stream("subscribeWorktreeCatalog", move |request, cancellation| {
        catalog_stream(stream_services.clone(), request, cancellation)
    });

    let refresh_services = services.clone();
    registry.register_unary(
        "vcs.refreshWorktreeCatalog",
        move |request, _cancellation| {
            let services = refresh_services.clone();
            async move {
                let input = decode::<WorktreeCatalogRefreshInput>(request)?;
                services
                    .catalog
                    .refresh(&input.project_id, input.reason.into())
                    .await
                    .map(|snapshot| encode(snapshot.as_ref()))
                    .map_err(encode)
            }
        },
    );

    let adoption_services = services.clone();
    registry.register_unary("worktree.adopt", move |request, cancellation| {
        let services = adoption_services.clone();
        async move { adopt_worktree(&services, request, cancellation).await }
    });

    let create_managed_services = services.clone();
    registry.register_unary("worktree.createManaged", move |request, cancellation| {
        let services = create_managed_services.clone();
        async move { create_managed_worktree(&services, request, cancellation).await }
    });

    let create_panel_services = services.clone();
    registry.register_unary("worktree.createPanel", move |request, cancellation| {
        let services = create_panel_services.clone();
        async move { create_worktree_panel(&services, request, cancellation).await }
    });

    let retarget_services = services.clone();
    registry.register_unary("worktree.retarget", move |request, cancellation| {
        let services = retarget_services.clone();
        async move { retarget_worktree_thread(&services, request, cancellation).await }
    });

    let plan_services = services.clone();
    registry.register_unary("worktree.getRemovalPlan", move |request, cancellation| {
        let services = plan_services.clone();
        async move { get_removal_plan(&services, request, cancellation).await }
    });

    let detach_services = services.clone();
    registry.register_unary(
        "worktree.removeFromBibCode",
        move |request, cancellation| {
            let services = detach_services.clone();
            async move { remove_from_bibcode(&services, request, cancellation).await }
        },
    );

    let removal_services = services.clone();
    registry.register_unary("worktree.remove", move |request, cancellation| {
        let services = removal_services.clone();
        async move { remove_worktree(&services, request, cancellation).await }
    });

    registry.register_unary(
        "worktree.updateDiscoveryPolicy",
        move |request, cancellation| {
            let services = services.clone();
            async move { update_discovery_policy(&services, request, cancellation).await }
        },
    );
}

async fn adopt_worktree(
    services: &WorktreeCatalogRpcServices,
    request: RpcRequest,
    cancellation: CancellationToken,
) -> RpcResult {
    let input = decode::<WorktreeAdoptInput>(request)?;
    let payload_digest = canonical_command_digest(&input).map_err(|_| {
        encode(adoption_error(
            WorktreeAdoptionErrorReason::Internal,
            "The adoption payload could not be admitted.",
            None,
        ))
    })?;
    let command_claim =
        acquire_worktree_command_claim(&services.orchestration, &input.command_id, &cancellation)
            .await
            .map_err(|error| encode(adoption_orchestration_error(error, None)))?;
    if let Some(result) = services
        .orchestration
        .replay_admitted_worktree_adoption(&command_claim, &input.command_id, &payload_digest)
        .await
        .map_err(|error| encode(adoption_orchestration_error(error, None)))?
    {
        return adoption_dispatch_result(result, None);
    }
    let project_id = input.project_id.clone();
    let operation_runtime = services.operations.clone();
    let services = services.clone();
    operation_runtime
        .run(async move {
            let operation_cancellation = cancellation.clone();
            let lock_cancellation = cancellation.clone();
            let catalog = services.catalog.clone();
            catalog
                .with_project_mutation_lock_cancellation(
                    &project_id,
                    &lock_cancellation,
                    || async move {
                        adopt_worktree_locked(
                            &services,
                            input,
                            payload_digest,
                            command_claim,
                            &operation_cancellation,
                        )
                        .await
                    },
                )
                .await
                .unwrap_or_else(|| {
                    Err(encode(adoption_orchestration_error(
                        OrchestrationError::Cancelled,
                        None,
                    )))
                })
        })
        .await
}

async fn adopt_worktree_locked(
    services: &WorktreeCatalogRpcServices,
    input: WorktreeAdoptInput,
    payload_digest: String,
    command_claim: CommandAdmissionClaim,
    cancellation: &CancellationToken,
) -> RpcResult {
    let (mut snapshot, refreshed) = match services.catalog.latest(&input.project_id).await {
        Some(snapshot) => (snapshot, false),
        None => services
            .catalog
            .refresh(&input.project_id, CatalogRefreshTrigger::Explicit)
            .await
            .map(|snapshot| (snapshot, true))
            .map_err(|error| encode(adoption_catalog_error(error, None)))?,
    };
    if !refreshed && (!snapshot.authoritative || snapshot.generation != input.expected_generation) {
        snapshot = services
            .catalog
            .refresh(&input.project_id, CatalogRefreshTrigger::Explicit)
            .await
            .map_err(|error| encode(adoption_catalog_error(error, Some(snapshot.generation))))?;
    }
    if !snapshot.authoritative || input.expected_generation > snapshot.generation {
        return Err(encode(adoption_error(
            WorktreeAdoptionErrorReason::StaleGeneration,
            "The requested catalog generation is no longer usable for adoption.",
            Some(snapshot.generation),
        )));
    }
    let descriptor = snapshot
        .worktrees
        .iter()
        .find(|worktree| worktree.worktree_key == input.worktree_key)
        .ok_or_else(|| {
            encode(adoption_error(
                WorktreeAdoptionErrorReason::WorktreeNotFound,
                "The selected worktree is not present in the current catalog.",
                Some(snapshot.generation),
            ))
        })?;
    let existing_owner = matches!(
        descriptor.adoption_state,
        WorktreeAdoptionState::Active | WorktreeAdoptionState::Archived
    );
    if (!descriptor.eligible_for_adoption && !existing_owner)
        || descriptor.is_primary
        || descriptor.is_bare
        || descriptor.registration_state != WorktreeRegistrationState::Registered
        || descriptor.directory_state != WorktreeDirectoryState::Present
    {
        return Err(encode(adoption_error(
            WorktreeAdoptionErrorReason::Ineligible,
            "The selected worktree is not currently eligible for adoption.",
            Some(snapshot.generation),
        )));
    }
    let resolved = services
        .catalog
        .resolve_adoption_candidate(&input.project_id, &input.worktree_key)
        .await
        .map_err(|error| encode(adoption_validation_error(error)))?;
    let result = services
        .orchestration
        .dispatch_with_admission_and_command_claim_until_handoff(
            OrchestrationCommand::WorktreeAdoptResolved {
                command_id: input.command_id,
                project_id: input.project_id.clone(),
                worktree_key: resolved.worktree_key,
                path: resolved.path,
                branch: resolved.branch,
                head: resolved.head,
                model_selection: input.thread_defaults.model_selection,
                runtime_mode: input.thread_defaults.runtime_mode,
                interaction_mode: input.thread_defaults.interaction_mode,
            },
            CommandAdmission {
                payload_digest,
                attachment_refs: Vec::new(),
                provider_turn: None,
            },
            command_claim,
            cancellation,
        )
        .await
        .map_err(|error| {
            encode(adoption_orchestration_error(
                error,
                Some(snapshot.generation),
            ))
        })?;
    services
        .catalog
        .invalidate_after_mutation(&input.project_id)
        .await;
    adoption_dispatch_result(result, Some(snapshot.generation))
}

async fn replay_thread_operation(
    services: &WorktreeCatalogRpcServices,
    command_claim: &CommandAdmissionClaim,
    command_id: &str,
    payload_digest: &str,
) -> Result<bool, Value> {
    command_claim
        .ensure_owns(command_id)
        .map_err(|error| encode(adoption_orchestration_error(error, None)))?;
    let Some(receipt) = services
        .orchestration
        .repositories()
        .get_command_receipt(command_id.to_owned())
        .await
        .map_err(|_| {
            encode(adoption_error(
                WorktreeAdoptionErrorReason::Internal,
                "The durable command receipt could not be read.",
                None,
            ))
        })?
    else {
        return Ok(false);
    };
    if receipt.payload_digest.as_deref() != Some(payload_digest) {
        return Err(encode(adoption_error(
            WorktreeAdoptionErrorReason::CommandConflict,
            "The command ID was already used for another worktree operation.",
            None,
        )));
    }
    if receipt.status == "accepted" {
        return Ok(true);
    }
    Err(encode(adoption_error(
        WorktreeAdoptionErrorReason::OrchestrationFailed,
        receipt
            .error
            .unwrap_or_else(|| "The previous worktree operation was rejected.".to_owned()),
        None,
    )))
}

async fn create_managed_worktree(
    services: &WorktreeCatalogRpcServices,
    request: RpcRequest,
    cancellation: CancellationToken,
) -> RpcResult {
    let input = decode::<WorktreeCreateManagedInput>(request)?;
    let payload_digest = canonical_command_digest(&input).map_err(|_| {
        encode(adoption_error(
            WorktreeAdoptionErrorReason::Internal,
            "The managed creation payload could not be admitted.",
            None,
        ))
    })?;
    let command_claim =
        acquire_worktree_command_claim(&services.orchestration, &input.command_id, &cancellation)
            .await
            .map_err(|error| encode(adoption_orchestration_error(error, None)))?;
    if replay_thread_operation(services, &command_claim, &input.command_id, &payload_digest).await?
    {
        let thread = services
            .orchestration
            .repositories()
            .get_thread(input.thread_id.clone())
            .await
            .map_err(|_| {
                encode(adoption_error(
                    WorktreeAdoptionErrorReason::Internal,
                    "The managed workspace could not be reloaded.",
                    None,
                ))
            })?
            .and_then(|thread| thread.worktree_path.clone().map(|path| (thread, path)))
            .ok_or_else(|| {
                encode(adoption_error(
                    WorktreeAdoptionErrorReason::OrchestrationFailed,
                    "The accepted managed workspace is no longer available.",
                    None,
                ))
            })?;
        return Ok(encode(WorktreeManagedCreateResult {
            thread_id: input.thread_id,
            path: thread.1,
            ref_name: thread.0.branch.unwrap_or(input.ref_name),
        }));
    }
    let project_id = input.project_id.clone();
    let operation_runtime = services.operations.clone();
    let services = services.clone();
    operation_runtime
        .run(async move {
            let lock_cancellation = cancellation.clone();
            let catalog = services.catalog.clone();
            let notifier = services.clone();
            let outcome = catalog
                .with_project_mutation_lock_cancellation(
                    &project_id,
                    &lock_cancellation,
                    || async move {
                        create_managed_worktree_locked(
                            &services,
                            input,
                            payload_digest,
                            command_claim,
                            &cancellation,
                        )
                        .await
                    },
                )
                .await
                .unwrap_or_else(|| {
                    CatalogMutationOutcome::not_executed(Err(encode(adoption_orchestration_error(
                        OrchestrationError::Cancelled,
                        None,
                    ))))
                });
            outcome.finish(&notifier).await
        })
        .await
}

async fn create_managed_worktree_locked(
    services: &WorktreeCatalogRpcServices,
    input: WorktreeCreateManagedInput,
    payload_digest: String,
    command_claim: CommandAdmissionClaim,
    cancellation: &CancellationToken,
) -> CatalogMutationOutcome {
    let mut affected_paths = Vec::new();
    let mut executed = false;
    let result = async {
        let project = services
            .orchestration
            .repositories()
            .get_project(input.project_id.clone())
            .await
            .map_err(|_| {
                encode(adoption_error(
                    WorktreeAdoptionErrorReason::Internal,
                    "The project could not be read.",
                    None,
                ))
            })?
            .filter(|project| project.deleted_at.is_none())
            .ok_or_else(|| {
                encode(adoption_error(
                    WorktreeAdoptionErrorReason::ProjectNotFound,
                    "Project was not found.",
                    None,
                ))
            })?;
        let repository = services.creation_git.clone().ok_or_else(|| {
            encode(adoption_error(
                WorktreeAdoptionErrorReason::EnvironmentUnsupported,
                "Managed worktree creation is unavailable in this environment.",
                None,
            ))
        })?;
        let project_root = PathBuf::from(&project.workspace_root);
        affected_paths.push(
            tokio::fs::canonicalize(&project_root)
                .await
                .unwrap_or(project_root.clone()),
        );
        executed = true;
        let created = repository
            .create_worktree(
                CreateWorktreeInput {
                    cwd: project_root,
                    ref_name: input.ref_name.clone(),
                    new_ref_name: input.new_ref_name.clone(),
                    base_ref_name: input.base_ref_name.clone(),
                    path: None,
                },
                cancellation,
            )
            .await
            .map_err(encode)?;
        affected_paths.push(
            tokio::fs::canonicalize(Path::new(&created.worktree.path))
                .await
                .unwrap_or_else(|_| PathBuf::from(&created.worktree.path)),
        );
        let rollback = created.rollback.clone().ok_or_else(|| {
            encode(adoption_error(
                WorktreeAdoptionErrorReason::Internal,
                "Managed worktree creation completed without exact rollback ownership.",
                None,
            ))
        })?;
        let terminal_cancellation = CancellationToken::new();
        let dispatch = services
            .orchestration
            .dispatch_with_admission_and_command_claim_until_handoff(
                OrchestrationCommand::ThreadCreate {
                    command_id: input.command_id,
                    thread_id: input.thread_id.clone(),
                    project_id: input.project_id.clone(),
                    title: input.title,
                    kind: Some("workspace".to_owned()),
                    host_thread_id: None,
                    model_selection: input.thread_defaults.model_selection,
                    runtime_mode: input.thread_defaults.runtime_mode,
                    interaction_mode: input.thread_defaults.interaction_mode,
                    branch: Some(created.worktree.ref_name.clone()),
                    worktree_path: Some(created.worktree.path.clone()),
                    created_at: now_iso(),
                },
                CommandAdmission {
                    payload_digest,
                    attachment_refs: Vec::new(),
                    provider_turn: None,
                },
                command_claim,
                &terminal_cancellation,
            )
            .await;
        if let Err(error) = dispatch {
            let rollback = repository
                .rollback_managed_worktree_creation(Path::new(&project.workspace_root), &rollback)
                .await;
            let detail = match rollback {
                Ok(()) => error.to_string(),
                Err(rollback) => format!(
                    "{error}; exact managed-worktree rollback also failed: {}",
                    rollback.detail
                ),
            };
            return Err(encode(adoption_error(
                WorktreeAdoptionErrorReason::OrchestrationFailed,
                detail,
                None,
            )));
        }
        services
            .catalog
            .note_managed_creation(&input.project_id, &rollback.created_path)
            .await;
        services
            .catalog
            .invalidate_after_mutation(&input.project_id)
            .await;
        Ok(encode(WorktreeManagedCreateResult {
            thread_id: input.thread_id,
            path: created.worktree.path,
            ref_name: created.worktree.ref_name,
        }))
    }
    .await;
    CatalogMutationOutcome {
        result,
        affected_paths,
        executed,
    }
}

async fn create_worktree_panel(
    services: &WorktreeCatalogRpcServices,
    request: RpcRequest,
    cancellation: CancellationToken,
) -> RpcResult {
    let input = decode::<WorktreeCreatePanelInput>(request)?;
    let payload_digest = canonical_command_digest(&input).map_err(|_| {
        encode(adoption_error(
            WorktreeAdoptionErrorReason::Internal,
            "The panel creation payload could not be admitted.",
            None,
        ))
    })?;
    let command_claim =
        acquire_worktree_command_claim(&services.orchestration, &input.command_id, &cancellation)
            .await
            .map_err(|error| encode(adoption_orchestration_error(error, None)))?;
    if replay_thread_operation(services, &command_claim, &input.command_id, &payload_digest).await?
    {
        return Ok(encode(WorktreeThreadResult {
            thread_id: input.thread_id,
        }));
    }
    let repositories = services.orchestration.repositories();
    let host = repositories
        .get_thread(input.host_thread_id.clone())
        .await
        .map_err(|_| {
            encode(adoption_error(
                WorktreeAdoptionErrorReason::Internal,
                "The host thread could not be read.",
                None,
            ))
        })?
        .filter(|thread| thread.deleted_at.is_none())
        .ok_or_else(|| {
            encode(adoption_error(
                WorktreeAdoptionErrorReason::WorktreeNotFound,
                "The host thread was not found.",
                None,
            ))
        })?;
    let project_id = host.project_id;
    let operation_runtime = services.operations.clone();
    let services = services.clone();
    operation_runtime
        .run(async move {
            let lock_cancellation = cancellation.clone();
            let catalog = services.catalog.clone();
            let locked_project_id = project_id.clone();
            catalog
                .with_project_mutation_lock_cancellation(
                    &project_id,
                    &lock_cancellation,
                    || async move {
                        let host = services
                            .orchestration
                            .repositories()
                            .get_thread(input.host_thread_id.clone())
                            .await
                            .map_err(|_| {
                                encode(adoption_error(
                                    WorktreeAdoptionErrorReason::Internal,
                                    "The host thread could not be revalidated.",
                                    None,
                                ))
                            })?
                            .filter(|thread| {
                                thread.deleted_at.is_none()
                                    && thread.project_id == locked_project_id
                            })
                            .ok_or_else(|| {
                                encode(adoption_error(
                                    WorktreeAdoptionErrorReason::WorktreeNotFound,
                                    "The host thread changed before panel creation.",
                                    None,
                                ))
                            })?;
                        let result = services
                            .orchestration
                            .dispatch_with_admission_and_command_claim_until_handoff(
                                OrchestrationCommand::ThreadCreate {
                                    command_id: input.command_id,
                                    thread_id: input.thread_id.clone(),
                                    project_id: locked_project_id.clone(),
                                    title: input.title,
                                    kind: Some("panel".to_owned()),
                                    host_thread_id: Some(host.thread_id),
                                    model_selection: input.thread_defaults.model_selection,
                                    runtime_mode: input.thread_defaults.runtime_mode,
                                    interaction_mode: input.thread_defaults.interaction_mode,
                                    branch: host.branch,
                                    worktree_path: host.worktree_path,
                                    created_at: now_iso(),
                                },
                                CommandAdmission {
                                    payload_digest,
                                    attachment_refs: Vec::new(),
                                    provider_turn: None,
                                },
                                command_claim,
                                &cancellation,
                            )
                            .await
                            .map_err(|error| encode(adoption_orchestration_error(error, None)))?;
                        let _ = result;
                        Ok(encode(WorktreeThreadResult {
                            thread_id: input.thread_id,
                        }))
                    },
                )
                .await
                .unwrap_or_else(|| {
                    Err(encode(adoption_orchestration_error(
                        OrchestrationError::Cancelled,
                        None,
                    )))
                })
        })
        .await
}

async fn retarget_worktree_thread(
    services: &WorktreeCatalogRpcServices,
    request: RpcRequest,
    cancellation: CancellationToken,
) -> RpcResult {
    let input = decode::<WorktreeRetargetInput>(request)?;
    let payload_digest = canonical_command_digest(&input).map_err(|_| {
        encode(adoption_error(
            WorktreeAdoptionErrorReason::Internal,
            "The retarget payload could not be admitted.",
            None,
        ))
    })?;
    let command_claim =
        acquire_worktree_command_claim(&services.orchestration, &input.command_id, &cancellation)
            .await
            .map_err(|error| encode(adoption_orchestration_error(error, None)))?;
    if replay_thread_operation(services, &command_claim, &input.command_id, &payload_digest).await?
    {
        return Ok(encode(WorktreeThreadResult {
            thread_id: input.thread_id,
        }));
    }
    let project_id = input.project_id.clone();
    let operation_runtime = services.operations.clone();
    let services = services.clone();
    operation_runtime
        .run(async move {
            let lock_cancellation = cancellation.clone();
            let catalog = services.catalog.clone();
            let locked_project_id = project_id.clone();
            let notifier = services.clone();
            let outcome = catalog
                .with_project_mutation_lock_cancellation(
                    &project_id,
                    &lock_cancellation,
                    || async move {
                        let (mut snapshot, refreshed) =
                            match services.catalog.latest(&locked_project_id).await {
                                Some(snapshot) => (snapshot, false),
                                None => services
                                    .catalog
                                    .refresh(&locked_project_id, CatalogRefreshTrigger::Explicit)
                                    .await
                                    .map(|snapshot| (snapshot, true))
                                    .map_err(|error| encode(adoption_catalog_error(error, None)))?,
                            };
                        if !refreshed
                            && (!snapshot.authoritative
                                || snapshot.generation != input.expected_generation)
                        {
                            snapshot = services
                                .catalog
                                .refresh(&locked_project_id, CatalogRefreshTrigger::Explicit)
                                .await
                                .map_err(|error| {
                                    encode(adoption_catalog_error(error, Some(snapshot.generation)))
                                })?;
                        }
                        if !snapshot.authoritative
                            || input.expected_generation > snapshot.generation
                        {
                            return Err(encode(adoption_error(
                                WorktreeAdoptionErrorReason::StaleGeneration,
                                "The selected worktree catalog generation changed.",
                                Some(snapshot.generation),
                            )));
                        }
                        let descriptor = snapshot
                            .worktrees
                            .iter()
                            .find(|descriptor| descriptor.worktree_key == input.worktree_key)
                            .ok_or_else(|| {
                                encode(adoption_error(
                                    WorktreeAdoptionErrorReason::WorktreeNotFound,
                                    "The selected worktree is not present.",
                                    Some(snapshot.generation),
                                ))
                            })?;
                        if descriptor.is_primary
                            || descriptor.is_bare
                            || descriptor.registration_state
                                != WorktreeRegistrationState::Registered
                            || descriptor.directory_state != WorktreeDirectoryState::Present
                            || descriptor
                                .adopted_thread_id
                                .as_deref()
                                .is_some_and(|owner| owner != input.thread_id)
                        {
                            return Err(encode(adoption_error(
                                WorktreeAdoptionErrorReason::OwnershipConflict,
                                "The selected worktree cannot be assigned to this thread.",
                                Some(snapshot.generation),
                            )));
                        }
                        let old_path = services
                            .orchestration
                            .repositories()
                            .get_thread(input.thread_id.clone())
                            .await
                            .map_err(|_| {
                                encode(adoption_error(
                                    WorktreeAdoptionErrorReason::Internal,
                                    "The retargeted workspace could not be read.",
                                    Some(snapshot.generation),
                                ))
                            })?
                            .and_then(|thread| thread.worktree_path.map(PathBuf::from));
                        let resolved = services
                            .catalog
                            .resolve_adoption_candidate(&locked_project_id, &input.worktree_key)
                            .await
                            .map_err(|error| encode(adoption_validation_error(error)))?;
                        let mut status_paths = Vec::with_capacity(2);
                        if let Some(old_path) = old_path {
                            status_paths
                                .push(tokio::fs::canonicalize(&old_path).await.unwrap_or(old_path));
                        }
                        let new_path = PathBuf::from(&resolved.path);
                        status_paths
                            .push(tokio::fs::canonicalize(&new_path).await.unwrap_or(new_path));
                        services
                            .orchestration
                            .dispatch_with_admission_and_command_claim_until_handoff(
                                OrchestrationCommand::ThreadMetaUpdate {
                                    command_id: input.command_id,
                                    thread_id: input.thread_id.clone(),
                                    title: None,
                                    model_selection: None,
                                    branch: OptionalNullable::Present(resolved.branch),
                                    worktree_path: OptionalNullable::Present(Some(resolved.path)),
                                },
                                CommandAdmission {
                                    payload_digest,
                                    attachment_refs: Vec::new(),
                                    provider_turn: None,
                                },
                                command_claim,
                                &cancellation,
                            )
                            .await
                            .map_err(|error| {
                                encode(adoption_orchestration_error(
                                    error,
                                    Some(snapshot.generation),
                                ))
                            })?;
                        services
                            .catalog
                            .invalidate_after_mutation(&locked_project_id)
                            .await;
                        Ok((
                            WorktreeThreadResult {
                                thread_id: input.thread_id,
                            },
                            status_paths,
                        ))
                    },
                )
                .await;
            let outcome = match outcome {
                Some(Ok((result, affected_paths))) => CatalogMutationOutcome {
                    result: Ok(encode(result)),
                    executed: !affected_paths.is_empty(),
                    affected_paths,
                },
                Some(Err(error)) => CatalogMutationOutcome::not_executed(Err(error)),
                None => CatalogMutationOutcome::not_executed(Err(encode(
                    adoption_orchestration_error(OrchestrationError::Cancelled, None),
                ))),
            };
            outcome.finish(&notifier).await
        })
        .await
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorktreeRemoveFromBibCodeInput {
    command_id: String,
    project_id: String,
    thread_id: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorktreeRemoveInput {
    command_id: String,
    project_id: String,
    thread_id: String,
    mode: WorktreeRemovalMode,
    expected_generation: u64,
    plan_token: String,
    force_dirty: bool,
    confirm_repository_wide_prune: bool,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
enum WorktreeRemovalMode {
    DeleteGitWorktree,
    CleanupStaleRegistration,
}

impl WorktreeRemovalMode {
    /// The wire name.
    fn label(self) -> &'static str {
        match self {
            Self::DeleteGitWorktree => "delete-git-worktree",
            Self::CleanupStaleRegistration => "cleanup-stale-registration",
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorktreeGetRemovalPlanInput {
    project_id: String,
    thread_id: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorktreeRemovalPlan {
    plan_token: String,
    generation: u64,
    availability: &'static str,
    registered: bool,
    locked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    lock_reason: Option<String>,
    tracked_change_count: u64,
    untracked_file_count: u64,
    prune_impact: Vec<WorktreePruneImpact>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorktreePruneImpact {
    path: String,
    prune_reason: String,
    locked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    lock_reason: Option<String>,
}

struct ResolvedRemovalPlan {
    wire: WorktreeRemovalPlan,
    anchor: PathBuf,
    repository_key: String,
    record: Option<GitWorktreeRecord>,
    prune_impact: Vec<GitPrunableWorktree>,
}

struct ResolvedRemovalThread {
    path: PathBuf,
    known_thread_ids: Vec<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorktreeRemovalResult {
    thread_removed: bool,
    git_outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    orphan_cleanup_pending: bool,
}

#[derive(Clone, Copy, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum WorktreeRemovalErrorReason {
    ThreadNotFound,
    EnvironmentUnsupported,
    CommandConflict,
    CleanupCapacity,
    SessionRunning,
    OwnershipConflict,
    StaleGeneration,
    StalePlan,
    DirtyConfirmationRequired,
    PruneConfirmationRequired,
    ProtectedTarget,
    Locked,
    ReplacementConflict,
    RepositoryMismatch,
    GitFailed,
    OrchestrationFailed,
    Internal,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorktreeRemovalError {
    #[serde(rename = "_tag")]
    tag: &'static str,
    reason: WorktreeRemovalErrorReason,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_generation: Option<u64>,
}

async fn get_removal_plan(
    services: &WorktreeCatalogRpcServices,
    request: RpcRequest,
    cancellation: CancellationToken,
) -> RpcResult {
    let input = decode::<WorktreeGetRemovalPlanInput>(request)?;
    let project_id = input.project_id.clone();
    let catalog = services.catalog.clone();
    catalog
        .with_project_mutation_lock_cancellation(&project_id, &cancellation, || async {
            let plan = build_removal_plan(
                services,
                &input.project_id,
                &input.thread_id,
                None,
                true,
                &cancellation,
            )
            .await?;
            Ok(encode(plan.wire))
        })
        .await
        .unwrap_or_else(|| {
            Err(encode(removal_orchestration_error(
                OrchestrationError::Cancelled,
            )))
        })
}

async fn remove_from_bibcode(
    services: &WorktreeCatalogRpcServices,
    request: RpcRequest,
    cancellation: CancellationToken,
) -> RpcResult {
    let started = Instant::now();
    let input = decode::<WorktreeRemoveFromBibCodeInput>(request)?;
    let trace = Arc::new(RemovalTrace::default());
    // The token moves into the operation; a client interrupt cancels this clone too.
    let interrupted = cancellation.clone();
    let result: RpcResult = async {
        let payload_digest = removal_payload_digest(&input)?;
        let command_claim = acquire_worktree_command_claim(
            &services.orchestration,
            &input.command_id,
            &cancellation,
        )
        .await
        .map_err(|error| encode(removal_orchestration_error(error)))?;
        if let Some(result) =
            replay_removal(services, &command_claim, &input.command_id, &payload_digest).await?
        {
            trace.replayed.store(true, Ordering::Relaxed);
            return Ok(encode(result));
        }
        let operation_runtime = services.operations.clone();
        let services = services.clone();
        let (trace, interrupted) = (trace.clone(), interrupted.clone());
        operation_runtime
            .run(async move {
                // Recorded here, so a caller that stops waiting does not lose the outcome.
                trace.handed_off.store(true, Ordering::Relaxed);
                let result = remove_from_bibcode_owned(
                    &services,
                    input,
                    payload_digest,
                    command_claim,
                    cancellation,
                    &trace,
                )
                .await;
                trace.record(DETACH_ONLY, &result, started, &interrupted);
                result
            })
            .await
    }
    .await;
    // Failed before the server-owned operation started: admission, claim, or replay.
    if !trace.handed_off.load(Ordering::Relaxed) {
        trace.record(DETACH_ONLY, &result, started, &interrupted);
    }
    result
}

async fn remove_from_bibcode_owned(
    services: &WorktreeCatalogRpcServices,
    input: WorktreeRemoveFromBibCodeInput,
    payload_digest: String,
    command_claim: CommandAdmissionClaim,
    cancellation: CancellationToken,
    trace: &RemovalTrace,
) -> RpcResult {
    let cleanup_admission = admit_removal_cleanup(services).await?;
    let cleanup_lifecycle = cleanup_admission.clone();
    let reservation = reserve_removal(services, &command_claim, &input, &payload_digest).await?;
    if let Some(result) = reservation.result {
        trace.replayed.store(true, Ordering::Relaxed);
        return Ok(encode(result));
    }
    let project_id = input.project_id.clone();
    let catalog = services.catalog.clone();
    let outcome = catalog
        .with_project_mutation_lock_cancellation(&project_id, &cancellation, || async {
            let mut affected_paths = Vec::new();
            let mut executed = false;
            let result = async {
                if let Some(result) =
                    replay_removal(services, &command_claim, &input.command_id, &payload_digest)
                        .await?
                {
                    trace.replayed.store(true, Ordering::Relaxed);
                    return Ok(encode(result));
                }
                let (ownership, resolved) = resolve_removal_thread_with_ownership(
                    services,
                    &input.project_id,
                    &input.thread_id,
                )
                .await?;
                let status_path = tokio::fs::canonicalize(&resolved.path)
                    .await
                    .unwrap_or_else(|_| resolved.path.clone());
                let registry = services.catalog.availability_registry();
                let guard = registry
                    .mark_removing(&input.thread_id, &resolved.path)
                    .await
                    .map_err(encode)?;
                let quiesce = services
                    .removal_quiescer
                    .quiesce(
                        cleanup_admission,
                        WorktreeRemovalQuiesceRequest::project(
                            guard.identity(),
                            input.project_id.clone(),
                            resolved.known_thread_ids,
                        ),
                    )
                    .await;
                let orphan_cleanup_pending = quiesce.orphan_cleanup_pending();
                let result = persist_detach(
                    services,
                    input.command_id,
                    input.project_id.clone(),
                    input.thread_id.clone(),
                    resolved.path.clone(),
                    payload_digest,
                    "not-requested",
                    None,
                    orphan_cleanup_pending,
                    command_claim.clone(),
                    ownership.clone(),
                    &cancellation,
                )
                .await?;
                executed = true;
                affected_paths.push(status_path);
                quiesce.commit_detached();
                registry
                    .clear_recovered(&input.thread_id, &resolved.path)
                    .await
                    .map_err(encode)?;
                drop(guard);
                services
                    .catalog
                    .invalidate_after_mutation(&input.project_id)
                    .await;
                drop(cleanup_lifecycle);
                Ok(encode(result))
            }
            .await;
            CatalogMutationOutcome {
                result,
                affected_paths,
                executed,
            }
        })
        .await
        .unwrap_or_else(|| {
            CatalogMutationOutcome::not_executed(Err(encode(removal_orchestration_error(
                OrchestrationError::Cancelled,
            ))))
        });
    outcome.finish(services).await
}

async fn remove_worktree(
    services: &WorktreeCatalogRpcServices,
    request: RpcRequest,
    cancellation: CancellationToken,
) -> RpcResult {
    let started = Instant::now();
    let input = decode::<WorktreeRemoveInput>(request)?;
    let mode = input.mode.label();
    let trace = Arc::new(RemovalTrace::default());
    // The token moves into the operation; a client interrupt cancels this clone too.
    let interrupted = cancellation.clone();
    let result: RpcResult = async {
        let payload_digest = removal_payload_digest(&input)?;
        let command_claim = acquire_worktree_command_claim(
            &services.orchestration,
            &input.command_id,
            &cancellation,
        )
        .await
        .map_err(|error| encode(removal_orchestration_error(error)))?;
        if let Some(result) =
            replay_removal(services, &command_claim, &input.command_id, &payload_digest).await?
        {
            trace.replayed.store(true, Ordering::Relaxed);
            return Ok(encode(result));
        }
        let operation_runtime = services.operations.clone();
        let services = services.clone();
        let (trace, interrupted) = (trace.clone(), interrupted.clone());
        operation_runtime
            .run(async move {
                // Recorded here, so a caller that stops waiting does not lose the outcome.
                trace.handed_off.store(true, Ordering::Relaxed);
                let result = remove_worktree_owned(
                    &services,
                    input,
                    payload_digest,
                    command_claim,
                    cancellation,
                    &trace,
                )
                .await;
                trace.record(mode, &result, started, &interrupted);
                result
            })
            .await
    }
    .await;
    // Failed before the server-owned operation started: admission, claim, or replay.
    if !trace.handed_off.load(Ordering::Relaxed) {
        trace.record(mode, &result, started, &interrupted);
    }
    result
}

/// What one removal learned about its own outcome, for the profiler.
#[derive(Default)]
struct RemovalTrace {
    /// The server-owned operation started, and records the outcome itself.
    handed_off: AtomicBool,
    /// Already-admitted work did not drain within the removal's bound.
    drain_timed_out: AtomicBool,
    /// The result replays an earlier accepted removal: not a new outcome.
    replayed: AtomicBool,
}

impl RemovalTrace {
    fn record(
        &self,
        mode: &str,
        result: &RpcResult,
        started: Instant,
        cancellation: &CancellationToken,
    ) {
        if self.replayed.load(Ordering::Relaxed) {
            return;
        }
        record_removal_outcome(
            mode,
            removal_reason(result),
            started.elapsed(),
            self.drain_timed_out.load(Ordering::Relaxed),
            cancellation.is_cancelled(),
        );
    }
}

/// The kebab-case `reason` of an encoded removal, operation, catalog, or availability error.
/// Anything else reads as `unknown`, so a finding's fingerprint can never carry a path.
fn removal_reason(result: &RpcResult) -> Result<(), &str> {
    match result {
        Ok(_) => Ok(()),
        Err(error) => Err(error
            .get("reason")
            .and_then(Value::as_str)
            .filter(|reason| {
                !reason.is_empty()
                    && reason.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                    })
            })
            .unwrap_or("unknown")),
    }
}

async fn remove_worktree_owned(
    services: &WorktreeCatalogRpcServices,
    input: WorktreeRemoveInput,
    payload_digest: String,
    command_claim: CommandAdmissionClaim,
    cancellation: CancellationToken,
    trace: &RemovalTrace,
) -> RpcResult {
    let cleanup_admission = admit_removal_cleanup(services).await?;
    let cleanup_lifecycle = cleanup_admission.clone();
    let reservation = reserve_removal(services, &command_claim, &input, &payload_digest).await?;
    if let Some(result) = reservation.result {
        trace.replayed.store(true, Ordering::Relaxed);
        return Ok(encode(result));
    }
    let project_id = input.project_id.clone();
    let catalog = services.catalog.clone();
    let outcome = catalog
        .with_project_mutation_lock_cancellation(&project_id, &cancellation, || async {
            let mut affected_paths = Vec::new();
            let mut executed = false;
            let result = async {
            if let Some(result) =
                replay_removal(services, &command_claim, &input.command_id, &payload_digest).await?
            {
                trace.replayed.store(true, Ordering::Relaxed);
                return Ok(encode(result));
            }
            let (ownership, resolved) = resolve_removal_thread_with_ownership(
                services,
                &input.project_id,
                &input.thread_id,
            )
            .await?;
            if !reservation.prepared_retry {
                ensure_no_running_session(
                    &services.orchestration.repositories(),
                    services.removal_quiescer.as_ref(),
                    &resolved.known_thread_ids,
                )
                .await?;
            }
            affected_paths.push(
                tokio::fs::canonicalize(&resolved.path)
                    .await
                    .unwrap_or_else(|_| resolved.path.clone()),
            );
            let registry = services.catalog.availability_registry();
            let guard = registry
                .mark_removing(&input.thread_id, &resolved.path)
                .await
                .map_err(encode)?;
            let mut plan = build_removal_plan(
                services,
                &input.project_id,
                &input.thread_id,
                Some(input.expected_generation),
                false,
                &cancellation,
            )
            .await?;
            let resumed_git_success = reservation.prepared_retry
                && matches!(input.mode, WorktreeRemovalMode::DeleteGitWorktree)
                && plan.wire.availability == "missing-unregistered";
            if plan.wire.plan_token != input.plan_token && !resumed_git_success {
                return Err(encode(removal_error(
                    WorktreeRemovalErrorReason::StalePlan,
                    "The removal impact changed after it was confirmed.",
                    Some(plan.wire.generation),
                )));
            }
            validate_git_removal_preflight(&input, &plan, resumed_git_success).map_err(encode)?;
            if !resumed_git_success {
                // Removing fences new admissions; already-admitted work can still publish
                // a starting/running session until its admission is released.
                #[cfg(not(test))]
                let drain_timeout = REMOVAL_ADMISSION_DRAIN_TIMEOUT;
                #[cfg(test)]
                let drain_timeout = services.removal_admission_timeout;
                #[cfg(test)]
                if let Some(entered) = &services.removal_drain_entered {
                    entered.add_permits(1);
                }
                tokio::time::timeout(
                    drain_timeout,
                    registry.wait_for_removal_admissions(&guard.identity()),
                )
                .await
                .map_err(|_| {
                    trace.drain_timed_out.store(true, Ordering::Relaxed);
                    encode(removal_session_running())
                })?;
                let request = WorktreeRemovalQuiesceRequest::repository(
                    guard.identity(),
                    input.project_id.clone(),
                    plan.repository_key.clone(),
                    resolved.known_thread_ids.clone(),
                );
                let repositories = services.orchestration.repositories();
                let thread_ids = super::worktree_runtime::removal_thread_ids(
                    repositories.clone(),
                    &request,
                )
                .await
                .map_err(|_| encode(removal_internal()))?;
                ensure_no_running_session(
                    &repositories,
                    services.removal_quiescer.as_ref(),
                    &thread_ids,
                )
                .await?;
            }
            services
                .orchestration
                .prepare_worktree_removal_admission(
                    &command_claim,
                    &input.command_id,
                    &input.project_id,
                    &payload_digest,
                )
                .await
                .map_err(|error| encode(removal_orchestration_error(error)))?;
            if cancellation.is_cancelled() {
                return Err(encode(removal_orchestration_error(
                    OrchestrationError::Cancelled,
                )));
            }
            let quiesce = services
                .removal_quiescer
                .quiesce(
                    cleanup_admission,
                    WorktreeRemovalQuiesceRequest::repository(
                        guard.identity(),
                        input.project_id.clone(),
                        plan.repository_key.clone(),
                        resolved.known_thread_ids,
                    ),
                )
                .await;
            let orphan_cleanup_pending = quiesce.orphan_cleanup_pending();
            services
                .orchestration
                .verify_prepared_worktree_removal_admission(
                    &command_claim,
                    &input.command_id,
                    &input.project_id,
                    &payload_digest,
                )
                .await
                .map_err(|error| encode(removal_orchestration_error(error)))?;
            if cancellation.is_cancelled() {
                return Err(encode(removal_orchestration_error(
                    OrchestrationError::Cancelled,
                )));
            }
            let terminal_cancellation = CancellationToken::new();
            if !resumed_git_success {
                let trusted_anchor = services
                    .catalog
                    .resolve_trusted_repository_anchor(
                        &input.project_id,
                        &resolved.path,
                        &terminal_cancellation,
                    )
                    .await
                    .map_err(|error| {
                        encode(removal_catalog_error(error, Some(plan.wire.generation)))
                    })?;
                if trusted_anchor.repository_key != plan.repository_key {
                    return Err(encode(removal_error(
                        WorktreeRemovalErrorReason::RepositoryMismatch,
                        "The trusted removal anchor changed repository identity before Git mutation.",
                        Some(plan.wire.generation),
                    )));
                }
                plan.anchor = trusted_anchor.path;
            }
            executed = true;
            let git_result = if resumed_git_success {
                Ok(("removed", None))
            } else {
                apply_git_removal(services, &input, &plan, &terminal_cancellation).await
            };
            let (git_outcome, detail) = match git_result {
                Ok(outcome) => outcome,
                Err(error)
                    if matches!(input.mode, WorktreeRemovalMode::CleanupStaleRegistration)
                        && error.reason == WorktreeRemovalErrorReason::GitFailed =>
                {
                    ("failed", Some(error.message))
                }
                Err(error) => return Err(encode(error)),
            };
            let result = persist_detach(
                services,
                input.command_id,
                input.project_id.clone(),
                input.thread_id.clone(),
                resolved.path.clone(),
                payload_digest,
                git_outcome,
                detail,
                orphan_cleanup_pending,
                command_claim.clone(),
                ownership.clone(),
                &terminal_cancellation,
            )
            .await?;
            quiesce.commit_detached();
            registry
                .clear_recovered(&input.thread_id, &resolved.path)
                .await
                .map_err(encode)?;
            drop(guard);
            if matches!(git_outcome, "removed" | "cleaned") {
                services
                    .catalog
                    .invalidate_repository_after_mutation(&input.project_id)
                    .await;
            } else {
                services
                    .catalog
                    .invalidate_after_mutation(&input.project_id)
                    .await;
            }
            drop(cleanup_lifecycle);
            Ok(encode(result))
            }
            .await;
            CatalogMutationOutcome {
                result,
                affected_paths,
                executed,
            }
        })
        .await
        .unwrap_or_else(|| {
            CatalogMutationOutcome::not_executed(Err(encode(removal_orchestration_error(
                OrchestrationError::Cancelled,
            ))))
        });
    outcome.finish(services).await
}

async fn ensure_no_running_session(
    repositories: &Repositories,
    quiescer: &dyn WorktreeRemovalQuiescer,
    thread_ids: &[String],
) -> Result<(), Value> {
    let mut active_thread_ids = Vec::new();
    for thread_id in thread_ids {
        let session = repositories
            .get_thread_session(thread_id.clone())
            .await
            .map_err(|_| encode(removal_internal()))?;
        if session.is_some_and(|session| matches!(session.status.as_str(), "running" | "starting"))
        {
            active_thread_ids.push(thread_id.clone());
        }
    }
    if active_thread_ids.is_empty() {
        return Ok(());
    }
    match quiescer.live_session_thread_ids(active_thread_ids).await {
        Ok(live_thread_ids) if live_thread_ids.is_empty() => Ok(()),
        Ok(_) | Err(_) => Err(encode(removal_session_running())),
    }
}

async fn resolve_removal_thread(
    services: &WorktreeCatalogRpcServices,
    project_id: &str,
    thread_id: &str,
) -> Result<ResolvedRemovalThread, Value> {
    let repositories = services.orchestration.repositories();
    let project = repositories
        .get_project(project_id.to_owned())
        .await
        .map_err(|_| encode(removal_internal()))?
        .filter(|project| project.deleted_at.is_none())
        .ok_or_else(|| encode(removal_not_found()))?;
    let thread = repositories
        .get_thread(thread_id.to_owned())
        .await
        .map_err(|_| encode(removal_internal()))?
        .filter(|thread| {
            thread.project_id == project_id
                && thread.kind == "workspace"
                && thread.deleted_at.is_none()
        })
        .ok_or_else(|| encode(removal_not_found()))?;
    let persisted_path = thread
        .worktree_path
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| encode(removal_not_found()))?;
    let path_key = canonical_worktree_path_key(&persisted_path)
        .await
        .map_err(|_| encode(removal_internal()))?;
    let path = PathBuf::from(&path_key);
    let project_threads = repositories
        .list_threads_by_project(project_id.to_owned())
        .await
        .map_err(|_| encode(removal_internal()))?;
    let mut owner_ids = Vec::new();
    for candidate in &project_threads {
        if candidate.kind != "workspace" || candidate.deleted_at.is_some() {
            continue;
        }
        let Some(candidate_path) = candidate.worktree_path.as_deref() else {
            continue;
        };
        let candidate_key = canonical_worktree_path_key(Path::new(candidate_path))
            .await
            .map_err(|_| encode(removal_internal()))?;
        if candidate_key == path_key {
            owner_ids.push(candidate.thread_id.as_str());
        }
    }
    if owner_ids.len() != 1 || owner_ids[0] != thread_id {
        return Err(encode(removal_error(
            WorktreeRemovalErrorReason::OwnershipConflict,
            "Multiple workspace threads claim the removal target.",
            None,
        )));
    }
    let project_root_key = canonical_worktree_path_key(Path::new(&project.workspace_root))
        .await
        .map_err(|_| encode(removal_internal()))?;
    let mut known_thread_ids = Vec::new();
    for candidate in project_threads
        .into_iter()
        .filter(|candidate| candidate.deleted_at.is_none())
    {
        let candidate_key = match candidate.worktree_path.as_deref() {
            Some(candidate_path) => canonical_worktree_path_key(Path::new(candidate_path))
                .await
                .map_err(|_| encode(removal_internal()))?,
            None => project_root_key.clone(),
        };
        if candidate_key == path_key {
            known_thread_ids.push(candidate.thread_id);
        }
    }
    known_thread_ids.push(thread_id.to_owned());
    known_thread_ids.sort();
    known_thread_ids.dedup();
    Ok(ResolvedRemovalThread {
        path,
        known_thread_ids,
    })
}

async fn resolve_removal_thread_with_ownership(
    services: &WorktreeCatalogRpcServices,
    project_id: &str,
    thread_id: &str,
) -> Result<(WorkspaceOwnershipLease, ResolvedRemovalThread), Value> {
    // The first read discovers a server-owned normalized key. The second read is the
    // authoritative ownership preflight and remains protected through Git and detach.
    let candidate = resolve_removal_thread(services, project_id, thread_id).await?;
    let ownership = services
        .orchestration
        .acquire_workspace_removal_ownership(&candidate.path)
        .await
        .map_err(|error| encode(removal_orchestration_error(error)))?;
    let resolved = resolve_removal_thread(services, project_id, thread_id).await?;
    if candidate.path != resolved.path {
        return Err(encode(removal_error(
            WorktreeRemovalErrorReason::OwnershipConflict,
            "Workspace ownership changed during removal preflight.",
            None,
        )));
    }
    Ok((ownership, resolved))
}

async fn build_removal_plan(
    services: &WorktreeCatalogRpcServices,
    project_id: &str,
    thread_id: &str,
    expected_generation: Option<u64>,
    refresh: bool,
    cancellation: &CancellationToken,
) -> Result<ResolvedRemovalPlan, Value> {
    let resolved = resolve_removal_thread(services, project_id, thread_id).await?;
    let path = resolved.path;
    let snapshot = if refresh {
        services
            .catalog
            .refresh(project_id, CatalogRefreshTrigger::Explicit)
            .await
    } else {
        services.catalog.latest(project_id).await.ok_or_else(|| {
            CatalogError::new(CatalogErrorReason::StaleGeneration, "Removal plan expired.")
        })
    }
    .map_err(|error| encode(removal_catalog_error(error, expected_generation)))?;
    if !snapshot.authoritative {
        return Err(encode(removal_error(
            WorktreeRemovalErrorReason::RepositoryMismatch,
            "The repository cannot be verified for removal.",
            Some(snapshot.generation),
        )));
    }
    if expected_generation.is_some_and(|expected| expected != snapshot.generation) {
        return Err(encode(removal_error(
            WorktreeRemovalErrorReason::StaleGeneration,
            "The confirmed catalog generation is no longer current.",
            Some(snapshot.generation),
        )));
    }
    let trusted_anchor = services
        .catalog
        .resolve_trusted_repository_anchor(project_id, &path, cancellation)
        .await
        .map_err(|error| encode(removal_catalog_error(error, Some(snapshot.generation))))?;
    if trusted_anchor.repository_key != snapshot.repository_key {
        return Err(encode(removal_error(
            WorktreeRemovalErrorReason::RepositoryMismatch,
            "The catalog removal anchor no longer matches the confirmed repository.",
            Some(snapshot.generation),
        )));
    }
    let anchor = trusted_anchor.path;
    let repository = services.removal_git.clone().ok_or_else(|| {
        encode(removal_error(
            WorktreeRemovalErrorReason::EnvironmentUnsupported,
            "Native Git removal is unavailable in this environment.",
            Some(snapshot.generation),
        ))
    })?;
    let inventory = repository
        .inventory(anchor.clone(), cancellation.clone())
        .await
        .map_err(|_| encode(removal_git_failed(Some(snapshot.generation))))?;
    let common_dir = tokio::fs::canonicalize(&inventory.common_dir)
        .await
        .map_err(|_| encode(removal_git_failed(Some(snapshot.generation))))?;
    let observed_repository_key = worktree_repository_key(&common_dir, host_path_platform())
        .as_str()
        .to_owned();
    if observed_repository_key != snapshot.repository_key {
        return Err(encode(removal_error(
            WorktreeRemovalErrorReason::RepositoryMismatch,
            "The project repository identity changed during removal planning.",
            Some(snapshot.generation),
        )));
    }
    let path_key = canonical_worktree_path_key(&path)
        .await
        .map_err(|_| encode(removal_git_failed(Some(snapshot.generation))))?;
    let mut record = None;
    for candidate in &inventory.records {
        let candidate_key = canonical_worktree_path_key(&candidate.path)
            .await
            .map_err(|_| encode(removal_git_failed(Some(snapshot.generation))))?;
        if candidate_key == path_key {
            record = Some(candidate.clone());
            break;
        }
    }
    if record
        .as_ref()
        .is_some_and(|record| record.is_primary || record.is_bare)
    {
        return Err(encode(removal_error(
            WorktreeRemovalErrorReason::ProtectedTarget,
            "Primary and bare worktrees cannot be removed.",
            Some(snapshot.generation),
        )));
    }
    let directory_present = match tokio::fs::symlink_metadata(&path).await {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => {
            return Err(encode(removal_git_failed(Some(snapshot.generation))));
        }
    };
    if record.is_none() && directory_present {
        return Err(encode(removal_error(
            WorktreeRemovalErrorReason::ReplacementConflict,
            "An unregistered replacement exists at the workspace location.",
            Some(snapshot.generation),
        )));
    }
    let availability = match (&record, directory_present) {
        (Some(_), true) => "present",
        (Some(_), false) => "missing-registered",
        (None, false) => "missing-unregistered",
        (None, true) => unreachable!("replacement conflict returned above"),
    };
    let inspection = if availability == "present" {
        repository
            .inspect(
                anchor.clone(),
                record.as_ref().expect("present record").clone(),
                cancellation.clone(),
            )
            .await
            .map_err(|_| encode(removal_git_failed(Some(snapshot.generation))))?
    } else {
        Default::default()
    };
    let prune_impact = if record.as_ref().is_some_and(|record| record.is_prunable) {
        repository
            .preview_prune(anchor.clone(), cancellation.clone())
            .await
            .map_err(|_| encode(removal_git_failed(Some(snapshot.generation))))?
    } else {
        Vec::new()
    };
    let locked = record.as_ref().is_some_and(|record| record.locked);
    let lock_reason = record
        .as_ref()
        .and_then(|record| record.lock_reason.clone());
    let public_impact = prune_impact
        .iter()
        .map(|impact| WorktreePruneImpact {
            path: normalize_worktree_path_key(&impact.path, host_path_platform()),
            prune_reason: impact.prune_reason.clone(),
            locked: impact.locked,
            lock_reason: impact.lock_reason.clone(),
        })
        .collect::<Vec<_>>();
    let mut wire = WorktreeRemovalPlan {
        plan_token: String::new(),
        generation: snapshot.generation,
        availability,
        registered: record.is_some(),
        locked,
        lock_reason,
        tracked_change_count: inspection.tracked_change_count,
        untracked_file_count: inspection.untracked_file_count,
        prune_impact: public_impact,
    };
    wire.plan_token = removal_plan_token(
        project_id,
        thread_id,
        &anchor,
        &snapshot.repository_key,
        record
            .as_ref()
            .map_or_else(
                || format!("missing:{path_key}"),
                |record| {
                    worktree_key(&common_dir, &record.path, host_path_platform())
                        .as_str()
                        .to_owned()
                },
            )
            .as_str(),
        &wire,
    );
    Ok(ResolvedRemovalPlan {
        wire,
        anchor,
        repository_key: snapshot.repository_key.clone(),
        record,
        prune_impact,
    })
}

async fn apply_git_removal(
    services: &WorktreeCatalogRpcServices,
    input: &WorktreeRemoveInput,
    plan: &ResolvedRemovalPlan,
    cancellation: &CancellationToken,
) -> Result<(&'static str, Option<String>), WorktreeRemovalError> {
    let repository = services.removal_git.clone().ok_or_else(removal_internal)?;
    match input.mode {
        WorktreeRemovalMode::DeleteGitWorktree => {
            if plan.wire.availability != "present" {
                return Err(removal_error(
                    WorktreeRemovalErrorReason::StalePlan,
                    "The worktree is no longer present.",
                    Some(plan.wire.generation),
                ));
            }
            if plan.wire.locked {
                return Err(removal_error(
                    WorktreeRemovalErrorReason::Locked,
                    "The Git worktree registration is locked.",
                    Some(plan.wire.generation),
                ));
            }
            if !input.force_dirty
                && (plan.wire.tracked_change_count != 0 || plan.wire.untracked_file_count != 0)
            {
                return Err(removal_error(
                    WorktreeRemovalErrorReason::DirtyConfirmationRequired,
                    "Dirty worktree deletion requires explicit confirmation.",
                    Some(plan.wire.generation),
                ));
            }
            repository
                .remove(
                    plan.anchor.clone(),
                    plan.record.as_ref().expect("present record").clone(),
                    input.force_dirty,
                    cancellation.clone(),
                )
                .await
                .map_err(|_| removal_git_failed(Some(plan.wire.generation)))?;
            Ok(("removed", None))
        }
        WorktreeRemovalMode::CleanupStaleRegistration => {
            if plan.wire.availability == "present" {
                return Err(removal_error(
                    WorktreeRemovalErrorReason::ProtectedTarget,
                    "A present worktree cannot be treated as stale registration cleanup.",
                    Some(plan.wire.generation),
                ));
            }
            let Some(record) = plan.record.as_ref() else {
                return Ok(("cleaned", None));
            };
            if record.locked {
                return Err(removal_error(
                    WorktreeRemovalErrorReason::Locked,
                    "The stale Git worktree registration is locked.",
                    Some(plan.wire.generation),
                ));
            }
            if repository
                .remove(
                    plan.anchor.clone(),
                    record.clone(),
                    false,
                    cancellation.clone(),
                )
                .await
                .is_ok()
            {
                return Ok(("cleaned", None));
            }
            if plan.prune_impact.is_empty() {
                return Err(removal_git_failed(Some(plan.wire.generation)));
            }
            if !input.confirm_repository_wide_prune {
                return Err(removal_error(
                    WorktreeRemovalErrorReason::PruneConfirmationRequired,
                    "Repository-wide prune impact requires explicit confirmation.",
                    Some(plan.wire.generation),
                ));
            }
            repository
                .prune(
                    plan.anchor.clone(),
                    record.clone(),
                    git_worktree_prune_impact_digest(&plan.prune_impact),
                    cancellation.clone(),
                )
                .await
                .map_err(|_| removal_git_failed(Some(plan.wire.generation)))?;
            Ok(("cleaned", None))
        }
    }
}

fn validate_git_removal_preflight(
    input: &WorktreeRemoveInput,
    plan: &ResolvedRemovalPlan,
    resumed_git_success: bool,
) -> Result<(), WorktreeRemovalError> {
    match input.mode {
        WorktreeRemovalMode::DeleteGitWorktree => {
            if plan.wire.availability != "present" && !resumed_git_success {
                return Err(removal_error(
                    WorktreeRemovalErrorReason::StalePlan,
                    "The worktree is no longer present.",
                    Some(plan.wire.generation),
                ));
            }
            if plan.wire.locked {
                return Err(removal_error(
                    WorktreeRemovalErrorReason::Locked,
                    "The Git worktree registration is locked.",
                    Some(plan.wire.generation),
                ));
            }
            if !input.force_dirty
                && (plan.wire.tracked_change_count != 0 || plan.wire.untracked_file_count != 0)
            {
                return Err(removal_error(
                    WorktreeRemovalErrorReason::DirtyConfirmationRequired,
                    "Dirty worktree deletion requires explicit confirmation.",
                    Some(plan.wire.generation),
                ));
            }
        }
        WorktreeRemovalMode::CleanupStaleRegistration => {
            if plan.wire.availability == "present" {
                return Err(removal_error(
                    WorktreeRemovalErrorReason::ProtectedTarget,
                    "A present worktree cannot be treated as stale registration cleanup.",
                    Some(plan.wire.generation),
                ));
            }
            if plan.wire.locked {
                return Err(removal_error(
                    WorktreeRemovalErrorReason::Locked,
                    "The stale Git worktree registration is locked.",
                    Some(plan.wire.generation),
                ));
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn persist_detach(
    services: &WorktreeCatalogRpcServices,
    command_id: String,
    project_id: String,
    thread_id: String,
    path: PathBuf,
    payload_digest: String,
    git_outcome: &str,
    detail: Option<String>,
    orphan_cleanup_pending: bool,
    command_claim: CommandAdmissionClaim,
    ownership: WorkspaceOwnershipLease,
    cancellation: &CancellationToken,
) -> Result<WorktreeRemovalResult, Value> {
    services
        .orchestration
        .dispatch_with_admission_ownership_and_command_claim_until_handoff(
            OrchestrationCommand::WorktreeDetachResolved {
                command_id,
                project_id,
                thread_id,
                path: path.to_string_lossy().into_owned(),
                git_outcome: git_outcome.to_owned(),
                detail: detail.clone(),
                orphan_cleanup_pending,
            },
            CommandAdmission {
                payload_digest,
                attachment_refs: Vec::new(),
                provider_turn: None,
            },
            command_claim,
            ownership,
            cancellation,
        )
        .await
        .map_err(|error| encode(removal_orchestration_error(error)))?;
    Ok(WorktreeRemovalResult {
        thread_removed: true,
        git_outcome: git_outcome.to_owned(),
        detail,
        orphan_cleanup_pending,
    })
}

async fn replay_removal(
    services: &WorktreeCatalogRpcServices,
    command_claim: &CommandAdmissionClaim,
    command_id: &str,
    payload_digest: &str,
) -> Result<Option<WorktreeRemovalResult>, Value> {
    services
        .orchestration
        .replay_admitted_worktree_removal(command_claim, command_id, payload_digest)
        .await
        .map(|result| {
            result.map(|result| WorktreeRemovalResult {
                thread_removed: result.thread_removed,
                git_outcome: result.git_outcome,
                detail: result.detail,
                orphan_cleanup_pending: result.orphan_cleanup_pending,
            })
        })
        .map_err(|error| encode(removal_orchestration_error(error)))
}

async fn admit_removal_cleanup(
    services: &WorktreeCatalogRpcServices,
) -> Result<WorktreeRemovalCleanupAdmission, Value> {
    services
        .removal_quiescer
        .admit_cleanup()
        .await
        .map_err(|error| match error {
            WorktreeRemovalCleanupAdmissionError::Capacity => encode(removal_error(
                WorktreeRemovalErrorReason::CleanupCapacity,
                "Workspace cleanup capacity is busy; retry the removal.",
                None,
            )),
        })
}

async fn reserve_removal(
    services: &WorktreeCatalogRpcServices,
    command_claim: &CommandAdmissionClaim,
    input: &impl RemovalAdmissionInput,
    payload_digest: &str,
) -> Result<RemovalAdmissionReservation, Value> {
    services
        .orchestration
        .reserve_worktree_removal_admission(
            command_claim,
            input.command_id(),
            input.project_id(),
            payload_digest,
        )
        .await
        .map(|(result, prepared_retry)| RemovalAdmissionReservation {
            result: result.map(|result| WorktreeRemovalResult {
                thread_removed: result.thread_removed,
                git_outcome: result.git_outcome,
                detail: result.detail,
                orphan_cleanup_pending: result.orphan_cleanup_pending,
            }),
            prepared_retry,
        })
        .map_err(|error| encode(removal_orchestration_error(error)))
}

struct RemovalAdmissionReservation {
    result: Option<WorktreeRemovalResult>,
    prepared_retry: bool,
}

trait RemovalAdmissionInput {
    fn command_id(&self) -> &str;
    fn project_id(&self) -> &str;
}

impl RemovalAdmissionInput for WorktreeRemoveFromBibCodeInput {
    fn command_id(&self) -> &str {
        &self.command_id
    }

    fn project_id(&self) -> &str {
        &self.project_id
    }
}

impl RemovalAdmissionInput for WorktreeRemoveInput {
    fn command_id(&self) -> &str {
        &self.command_id
    }

    fn project_id(&self) -> &str {
        &self.project_id
    }
}

fn removal_payload_digest(input: &impl Serialize) -> Result<String, Value> {
    canonical_command_digest(input).map_err(|_| encode(removal_internal()))
}

fn removal_plan_token(
    project_id: &str,
    thread_id: &str,
    anchor: &Path,
    repository_key: &str,
    path_key: &str,
    plan: &WorktreeRemovalPlan,
) -> String {
    let mut fields = vec![
        project_id.to_owned(),
        thread_id.to_owned(),
        normalize_worktree_path_key(anchor, host_path_platform()),
        repository_key.to_owned(),
        path_key.to_owned(),
        plan.generation.to_string(),
        plan.availability.to_owned(),
        plan.registered.to_string(),
        plan.locked.to_string(),
        plan.lock_reason.clone().unwrap_or_default(),
        plan.tracked_change_count.to_string(),
        plan.untracked_file_count.to_string(),
    ];
    let mut impact = plan.prune_impact.clone();
    impact.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.prune_reason.cmp(&right.prune_reason))
    });
    for entry in impact {
        fields.push(entry.path);
        fields.push(entry.prune_reason);
    }
    let mut input = b"bibcode.worktree.removal-plan.v1".to_vec();
    input.extend_from_slice(&(fields.len() as u64).to_be_bytes());
    for field in fields {
        input.extend_from_slice(&(field.len() as u64).to_be_bytes());
        input.extend_from_slice(field.as_bytes());
    }
    sha256_hex(input)
}

fn removal_error(
    reason: WorktreeRemovalErrorReason,
    message: impl Into<String>,
    current_generation: Option<u64>,
) -> WorktreeRemovalError {
    WorktreeRemovalError {
        tag: "WorktreeRemovalError",
        reason,
        message: crate::worktree_catalog::bounded_message(message.into()),
        current_generation,
    }
}

fn removal_session_running() -> WorktreeRemovalError {
    removal_error(
        WorktreeRemovalErrorReason::SessionRunning,
        "Stop the running session before deleting this worktree.",
        None,
    )
}

fn removal_not_found() -> WorktreeRemovalError {
    removal_error(
        WorktreeRemovalErrorReason::ThreadNotFound,
        "The workspace thread was not found.",
        None,
    )
}

fn removal_internal() -> WorktreeRemovalError {
    removal_error(
        WorktreeRemovalErrorReason::Internal,
        "The removal request could not be completed.",
        None,
    )
}

fn removal_git_failed(current_generation: Option<u64>) -> WorktreeRemovalError {
    removal_error(
        WorktreeRemovalErrorReason::GitFailed,
        "Git could not verify the requested worktree mutation.",
        current_generation,
    )
}

fn removal_catalog_error(
    error: CatalogError,
    current_generation: Option<u64>,
) -> WorktreeRemovalError {
    let reason = match error.reason {
        CatalogErrorReason::ProjectNotFound => WorktreeRemovalErrorReason::ThreadNotFound,
        CatalogErrorReason::EnvironmentUnsupported => {
            WorktreeRemovalErrorReason::EnvironmentUnsupported
        }
        CatalogErrorReason::StaleGeneration => WorktreeRemovalErrorReason::StaleGeneration,
        CatalogErrorReason::CommandConflict => WorktreeRemovalErrorReason::CommandConflict,
        CatalogErrorReason::RepositoryUnavailable => WorktreeRemovalErrorReason::RepositoryMismatch,
        CatalogErrorReason::PolicyUpdateFailed | CatalogErrorReason::Internal => {
            WorktreeRemovalErrorReason::Internal
        }
    };
    removal_error(reason, error.message, current_generation)
}

fn removal_orchestration_error(error: OrchestrationError) -> WorktreeRemovalError {
    match error {
        OrchestrationError::CommandConflict { .. } => removal_error(
            WorktreeRemovalErrorReason::CommandConflict,
            "The command ID was already used with a different removal payload.",
            None,
        ),
        OrchestrationError::WorktreeOwnershipConflict { .. }
        | OrchestrationError::WorkspaceOwnershipChanged { .. } => removal_error(
            WorktreeRemovalErrorReason::OwnershipConflict,
            "Workspace ownership conflicts with removal.",
            None,
        ),
        OrchestrationError::Invariant { .. } => removal_internal(),
        error => removal_error(
            WorktreeRemovalErrorReason::OrchestrationFailed,
            format!("Workspace removal could not be persisted: {error}"),
            None,
        ),
    }
}

fn catalog_stream(
    services: WorktreeCatalogRpcServices,
    request: RpcRequest,
    cancellation: CancellationToken,
) -> watch::Receiver<Option<RpcStreamChunk>> {
    let (sender, receiver) = watch::channel(None);
    tokio::spawn(async move {
        let input = match decode::<WorktreeCatalogInput>(request) {
            Ok(input) => input,
            Err(error) => {
                sender.send_replace(Some(Err(error)));
                return;
            }
        };
        let subscription = tokio::select! {
            _ = cancellation.cancelled() => return,
            result = services.catalog.subscribe(&input.project_id) => result,
        };
        let mut subscription = match subscription {
            Ok(subscription) => subscription,
            Err(error) => {
                sender.send_replace(Some(Err(encode(error))));
                return;
            }
        };
        if cancellation.is_cancelled() {
            return;
        }
        sender.send_replace(Some(Ok(vec![encode(
            subscription.initial_latest().as_ref(),
        )])));
        loop {
            tokio::select! {
                _ = cancellation.cancelled() => return,
                snapshot = subscription.changed() => {
                    let Some(snapshot) = snapshot else { return };
                    sender.send_replace(Some(Ok(vec![encode(snapshot.as_ref())])));
                }
            }
        }
    });
    receiver
}

async fn update_discovery_policy(
    services: &WorktreeCatalogRpcServices,
    request: RpcRequest,
    cancellation: CancellationToken,
) -> RpcResult {
    let input = decode::<WorktreeDiscoveryPolicyUpdateInput>(request)?;
    let payload_digest = canonical_command_digest(&input)
        .map_err(|error| encode(policy_error(format!("Policy admission failed: {error}"))))?;
    let command_claim =
        acquire_worktree_command_claim(&services.orchestration, &input.command_id, &cancellation)
            .await
            .map_err(|error| encode(policy_orchestration_error(error)))?;
    let existing_receipt = services
        .orchestration
        .get_command_receipt_with_claim_cancellation(
            &command_claim,
            &input.command_id,
            &cancellation,
        )
        .await
        .map_err(|error| encode(policy_orchestration_error(error)))?;
    let project_id = input.project_id.clone();
    let services = services.clone();
    let mutation_cancellation = cancellation.clone();
    let operation_runtime = services.operations.clone();
    operation_runtime
        .run(async move {
            let operation_cancellation = mutation_cancellation.clone();
            let catalog = services.catalog.clone();
            catalog
                .with_project_mutation_lock_cancellation(
                    &project_id,
                    &mutation_cancellation,
                    || async {
                        update_discovery_policy_locked(
                            &services,
                            input,
                            payload_digest,
                            command_claim,
                            existing_receipt,
                            operation_cancellation,
                        )
                        .await
                    },
                )
                .await
                .unwrap_or_else(|| {
                    Err(encode(policy_orchestration_error(
                        OrchestrationError::Cancelled,
                    )))
                })
        })
        .await
}

async fn update_discovery_policy_locked(
    services: &WorktreeCatalogRpcServices,
    input: WorktreeDiscoveryPolicyUpdateInput,
    payload_digest: String,
    command_claim: CommandAdmissionClaim,
    existing_receipt: Option<CommandReceipt>,
    cancellation: CancellationToken,
) -> RpcResult {
    let project = services
        .orchestration
        .repositories()
        .get_project(input.project_id.clone())
        .await
        .map_err(|error| encode(policy_error(error.to_string())))?
        .filter(|project| project.deleted_at.is_none())
        .ok_or_else(project_not_found)?;
    let mut policy =
        serde_json::from_value::<ProjectWorktreeDiscoveryPolicy>(project.worktree_discovery)
            .map_err(|error| {
                encode(policy_error(format!(
                    "Persisted discovery policy is invalid: {error}"
                )))
            })?;

    if let Some(visibility) = input.visibility {
        policy.visibility = visibility;
    }
    if let Some(dismiss) = input.dismiss_initial_prompt {
        policy.initial_prompt_dismissed_at = dismiss.then(now_iso);
    }
    if let Some(expected_generation) = input.acknowledge_generation {
        let snapshot = services
            .catalog
            .acknowledgeable(&input.project_id, expected_generation)
            .await
            .map_err(|current| {
                tracing::warn!(
                    expected_generation,
                    current_generation = ?current,
                    "worktree discovery policy refused a stale catalog generation"
                );
                encode(stale_generation(expected_generation, current))
            })?;
        policy.baseline_paths = compact_eligible_baseline(&snapshot)?;
    }

    let policy_value = serde_json::to_value(&policy).map_err(|error| {
        encode(policy_error(format!(
            "Discovery policy could not be serialized: {error}"
        )))
    })?;
    let legacy_receipt = existing_receipt
        .as_ref()
        .is_some_and(|receipt| receipt.payload_digest.is_none());
    if let Some(receipt) = existing_receipt.as_ref()
        && receipt.payload_digest.is_none()
        && receipt.status == "accepted"
    {
        services
            .orchestration
            .verify_legacy_worktree_policy_replay(
                &command_claim,
                receipt,
                &input.command_id,
                &input.project_id,
                &policy_value,
            )
            .await
            .map_err(|error| encode(policy_orchestration_error(error)))?;
        return Ok(policy_value);
    }
    let command = OrchestrationCommand::ProjectMetaUpdate {
        command_id: input.command_id,
        project_id: input.project_id,
        title: None,
        workspace_root: None,
        default_model_selection: OptionalNullable::Missing,
        scripts: None,
        worktree_discovery: Some(policy_value.clone()),
    };
    if legacy_receipt {
        services
            .orchestration
            .dispatch_with_command_claim_until_handoff(command, command_claim, &cancellation)
            .await
    } else {
        services
            .orchestration
            .dispatch_with_admission_and_command_claim_until_handoff(
                command,
                CommandAdmission {
                    payload_digest,
                    attachment_refs: Vec::new(),
                    provider_turn: None,
                },
                command_claim,
                &cancellation,
            )
            .await
    }
    .map_err(|error| encode(policy_orchestration_error(error)))?;
    Ok(policy_value)
}

pub fn compact_eligible_baseline(snapshot: &WorktreeCatalogSnapshot) -> Result<Vec<String>, Value> {
    if !snapshot.authoritative {
        return Err(encode(CatalogError::new(
            CatalogErrorReason::StaleGeneration,
            "Discovery candidates can only be acknowledged from an authoritative catalog generation.",
        )));
    }
    let mut seen = HashSet::new();
    Ok(snapshot
        .worktrees
        .iter()
        .filter(|worktree| worktree.eligible_for_adoption)
        .filter(|worktree| seen.insert(worktree.path.clone()))
        .map(|worktree| worktree.path.clone())
        .take(MAX_BASELINE_PATHS)
        .collect())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorktreeCatalogInput {
    project_id: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case")]
enum WorktreeCatalogRefreshReason {
    Focus,
    #[default]
    Explicit,
}

impl From<WorktreeCatalogRefreshReason> for CatalogRefreshTrigger {
    fn from(reason: WorktreeCatalogRefreshReason) -> Self {
        match reason {
            WorktreeCatalogRefreshReason::Focus => Self::Focus,
            WorktreeCatalogRefreshReason::Explicit => Self::Explicit,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorktreeCatalogRefreshInput {
    project_id: String,
    #[serde(default)]
    reason: WorktreeCatalogRefreshReason,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorktreeAdoptInput {
    command_id: String,
    project_id: String,
    worktree_key: String,
    expected_generation: u64,
    thread_defaults: WorktreeAdoptThreadDefaults,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorktreeAdoptThreadDefaults {
    model_selection: Value,
    runtime_mode: String,
    interaction_mode: String,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorktreeCreateManagedInput {
    command_id: String,
    project_id: String,
    thread_id: String,
    title: String,
    ref_name: String,
    new_ref_name: Option<String>,
    base_ref_name: Option<String>,
    thread_defaults: WorktreeAdoptThreadDefaults,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorktreeCreatePanelInput {
    command_id: String,
    host_thread_id: String,
    thread_id: String,
    title: String,
    thread_defaults: WorktreeAdoptThreadDefaults,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorktreeRetargetInput {
    command_id: String,
    project_id: String,
    thread_id: String,
    worktree_key: String,
    expected_generation: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorktreeThreadResult {
    thread_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorktreeManagedCreateResult {
    thread_id: String,
    path: String,
    ref_name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorktreeAdoptResult {
    thread_id: String,
    disposition: String,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
enum WorktreeAdoptionErrorReason {
    ProjectNotFound,
    EnvironmentUnsupported,
    WorktreeNotFound,
    StaleGeneration,
    Ineligible,
    WorkspaceMissing,
    RepositoryMismatch,
    OwnershipConflict,
    CommandConflict,
    OrchestrationFailed,
    Internal,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WorktreeAdoptionError {
    #[serde(rename = "_tag")]
    tag: &'static str,
    reason: WorktreeAdoptionErrorReason,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_generation: Option<u64>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorktreeDiscoveryPolicyUpdateInput {
    command_id: String,
    project_id: String,
    visibility: Option<WorktreeDiscoveryVisibility>,
    acknowledge_generation: Option<u64>,
    dismiss_initial_prompt: Option<bool>,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum WorktreeDiscoveryVisibility {
    Hidden,
    Shown,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectWorktreeDiscoveryPolicy {
    #[serde(default = "hidden_visibility")]
    visibility: WorktreeDiscoveryVisibility,
    #[serde(default)]
    initial_prompt_dismissed_at: Option<String>,
    #[serde(default)]
    baseline_paths: Vec<String>,
}

fn hidden_visibility() -> WorktreeDiscoveryVisibility {
    WorktreeDiscoveryVisibility::Hidden
}

fn decode<T: for<'de> Deserialize<'de>>(request: RpcRequest) -> Result<T, Value> {
    serde_json::from_value(request.payload).map_err(|error| {
        json!({
            "_tag": "RpcRequestInvalid",
            "method": request.tag,
            "detail": error.to_string(),
        })
    })
}

fn encode(value: impl Serialize) -> Value {
    serde_json::to_value(value).unwrap_or_else(|error| {
        json!({
            "_tag": "WorktreeCatalogError",
            "reason": "internal",
            "message": error.to_string(),
        })
    })
}

fn project_not_found() -> Value {
    encode(CatalogError::new(
        CatalogErrorReason::ProjectNotFound,
        "Project was not found.",
    ))
}

fn policy_error(message: String) -> CatalogError {
    CatalogError::new(CatalogErrorReason::PolicyUpdateFailed, message)
}

fn policy_orchestration_error(error: OrchestrationError) -> CatalogError {
    match error {
        OrchestrationError::CommandConflict { .. } => CatalogError::new(
            CatalogErrorReason::CommandConflict,
            "The command ID was already used by another worktree mutation.",
        ),
        error => policy_error(error.to_string()),
    }
}

fn adoption_error(
    reason: WorktreeAdoptionErrorReason,
    message: impl Into<String>,
    current_generation: Option<u64>,
) -> WorktreeAdoptionError {
    WorktreeAdoptionError {
        tag: "WorktreeAdoptionError",
        reason,
        message: crate::worktree_catalog::bounded_message(message.into()),
        current_generation,
    }
}

fn adoption_catalog_error(
    error: CatalogError,
    current_generation: Option<u64>,
) -> WorktreeAdoptionError {
    let reason = match error.reason {
        CatalogErrorReason::ProjectNotFound => WorktreeAdoptionErrorReason::ProjectNotFound,
        CatalogErrorReason::EnvironmentUnsupported => {
            WorktreeAdoptionErrorReason::EnvironmentUnsupported
        }
        CatalogErrorReason::StaleGeneration => WorktreeAdoptionErrorReason::StaleGeneration,
        CatalogErrorReason::RepositoryUnavailable
        | CatalogErrorReason::CommandConflict
        | CatalogErrorReason::PolicyUpdateFailed
        | CatalogErrorReason::Internal => WorktreeAdoptionErrorReason::Internal,
    };
    adoption_error(reason, error.message, current_generation)
}

fn adoption_validation_error(error: AdoptionValidationError) -> WorktreeAdoptionError {
    let reason = match error.reason {
        AdoptionValidationErrorReason::WorktreeNotFound => {
            WorktreeAdoptionErrorReason::WorktreeNotFound
        }
        AdoptionValidationErrorReason::Ineligible => WorktreeAdoptionErrorReason::Ineligible,
        AdoptionValidationErrorReason::WorkspaceMissing => {
            WorktreeAdoptionErrorReason::WorkspaceMissing
        }
        AdoptionValidationErrorReason::RepositoryMismatch => {
            WorktreeAdoptionErrorReason::RepositoryMismatch
        }
        AdoptionValidationErrorReason::CatalogUnavailable => WorktreeAdoptionErrorReason::Internal,
    };
    adoption_error(reason, error.message, error.current_generation)
}

fn adoption_orchestration_error(
    error: OrchestrationError,
    current_generation: Option<u64>,
) -> WorktreeAdoptionError {
    match error {
        OrchestrationError::CommandConflict { .. } => adoption_error(
            WorktreeAdoptionErrorReason::CommandConflict,
            "The command ID was already used with a different adoption payload.",
            current_generation,
        ),
        OrchestrationError::WorktreeOwnershipConflict { .. } => adoption_error(
            WorktreeAdoptionErrorReason::OwnershipConflict,
            "Worktree adoption conflicts with canonical workspace ownership.",
            current_generation,
        ),
        OrchestrationError::Invariant { .. } => adoption_error(
            WorktreeAdoptionErrorReason::Internal,
            "The durable worktree adoption result is invalid.",
            current_generation,
        ),
        error => adoption_error(
            WorktreeAdoptionErrorReason::OrchestrationFailed,
            format!("Worktree adoption could not be persisted: {error}"),
            current_generation,
        ),
    }
}

fn adoption_dispatch_result(
    result: crate::orchestration::engine::DispatchResult,
    current_generation: Option<u64>,
) -> RpcResult {
    let thread_id = result.thread_id.ok_or_else(|| {
        encode(adoption_error(
            WorktreeAdoptionErrorReason::Internal,
            "Worktree adoption completed without a thread result.",
            current_generation,
        ))
    })?;
    let disposition = result.disposition.ok_or_else(|| {
        encode(adoption_error(
            WorktreeAdoptionErrorReason::Internal,
            "Worktree adoption completed without a disposition.",
            current_generation,
        ))
    })?;
    Ok(encode(WorktreeAdoptResult {
        thread_id,
        disposition,
    }))
}

fn stale_generation(expected: u64, current: Option<u64>) -> CatalogError {
    let current = current.map_or_else(|| "unavailable".to_owned(), |value| value.to_string());
    CatalogError::new(
        CatalogErrorReason::StaleGeneration,
        format!(
            "Catalog generation {expected} is no longer authoritative; current generation is {current}."
        ),
    )
}

fn now_iso() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| OffsetDateTime::now_utc().unix_timestamp().to_string())
}

#[cfg(test)]
mod operation_runtime_tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use serde_json::json;
    use tokio::sync::{Notify, Semaphore};

    use super::{
        CatalogMutationOutcome, WorktreeCatalogOperationRuntime, WorktreeCatalogRefreshReason,
        removal_reason,
    };
    use crate::worktree_catalog::CatalogRefreshTrigger;

    const EXPECTED_MAX_IN_FLIGHT_WORKTREE_OPERATIONS: usize = 64;

    #[test]
    fn removal_reasons_are_kebab_case_tokens_or_unknown() {
        assert_eq!(removal_reason(&Ok(json!({}))), Ok(()));
        assert_eq!(
            removal_reason(&Err(json!({"reason":"git-failed"}))),
            Err("git-failed")
        );
        for error in [
            json!({}),
            json!({"reason":"/home/user/repo"}),
            json!({"reason":""}),
            json!({"reason":"Stale Plan"}),
            json!({"reason":7}),
        ] {
            assert_eq!(
                removal_reason(&Err(error.clone())),
                Err("unknown"),
                "{error}"
            );
        }
    }

    #[test]
    fn refresh_reason_defaults_to_explicit_and_maps_only_focus_to_focus() {
        assert_eq!(
            CatalogRefreshTrigger::from(WorktreeCatalogRefreshReason::default()),
            CatalogRefreshTrigger::Explicit
        );
        assert_eq!(
            CatalogRefreshTrigger::from(WorktreeCatalogRefreshReason::Focus),
            CatalogRefreshTrigger::Focus
        );
    }

    #[test]
    fn executed_error_remains_a_status_mutation_outcome() {
        let path = PathBuf::from("workspace");
        let executed_error = CatalogMutationOutcome {
            result: Err(json!({"_tag":"expected failure"})),
            affected_paths: vec![path.clone()],
            executed: true,
        };
        let rejected_before_execution = CatalogMutationOutcome::not_executed(Err(json!({
            "_tag":"rejected"
        })));

        assert_eq!(
            executed_error.status_mutation_paths(),
            Some([path].as_slice())
        );
        assert_eq!(rejected_before_execution.status_mutation_paths(), None);
    }

    #[tokio::test]
    async fn post_handoff_operations_retain_finite_admission_after_waiters_disconnect() {
        let runtime = WorktreeCatalogOperationRuntime::new();
        let entered = Arc::new(Semaphore::new(0));
        let completed = Arc::new(Semaphore::new(0));
        let release = Arc::new(Semaphore::new(0));
        let mut callers = Vec::new();

        for _ in 0..EXPECTED_MAX_IN_FLIGHT_WORKTREE_OPERATIONS {
            let runtime = runtime.clone();
            let entered = entered.clone();
            let completed = completed.clone();
            let release = release.clone();
            callers.push(tokio::spawn(async move {
                runtime
                    .run(async move {
                        entered.add_permits(1);
                        let _permit = release.acquire().await.expect("release remains open");
                        completed.add_permits(1);
                        Ok(json!({ "completed": true }))
                    })
                    .await
            }));
        }

        entered
            .acquire_many(EXPECTED_MAX_IN_FLIGHT_WORKTREE_OPERATIONS as u32)
            .await
            .expect("every admitted operation enters")
            .forget();
        for caller in callers {
            caller.abort();
        }

        let saturated = runtime
            .run(async { Ok(json!({ "mustNotRun": true })) })
            .await
            .expect_err("a full runtime rejects without adding an unbounded waiter");
        assert_eq!(saturated["_tag"], "WorktreeOperationError");
        assert_eq!(saturated["reason"], "operation-capacity");

        release.add_permits(EXPECTED_MAX_IN_FLIGHT_WORKTREE_OPERATIONS);
        completed
            .acquire_many(EXPECTED_MAX_IN_FLIGHT_WORKTREE_OPERATIONS as u32)
            .await
            .expect("every admitted operation completes")
            .forget();
        assert_eq!(
            runtime
                .run(async { Ok(json!({ "admittedAfterCompletion": true })) })
                .await
                .expect("completion releases the owned operation permit"),
            json!({ "admittedAfterCompletion": true })
        );
        runtime.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_closes_admission_and_drains_the_owned_operation_permit() {
        let runtime = WorktreeCatalogOperationRuntime::new();
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Semaphore::new(0));
        let operation_runtime = runtime.clone();
        let operation_entered = entered.clone();
        let operation_release = release.clone();
        let waiter = tokio::spawn(async move {
            operation_runtime
                .run(async move {
                    operation_entered.notify_one();
                    let _permit = operation_release
                        .acquire()
                        .await
                        .expect("release remains open");
                    Ok(json!({ "completed": true }))
                })
                .await
        });
        entered.notified().await;

        let shutdown_runtime = runtime.clone();
        let shutdown = tokio::spawn(async move { shutdown_runtime.shutdown().await });
        tokio::task::yield_now().await;
        assert!(
            !shutdown.is_finished(),
            "shutdown drains admitted operation owners"
        );

        release.add_permits(1);
        assert_eq!(
            waiter
                .await
                .expect("waiter joins")
                .expect("operation succeeds"),
            json!({ "completed": true })
        );
        shutdown.await.expect("shutdown joins");

        let closed = runtime
            .run(async { Ok(json!({ "mustNotRun": true })) })
            .await
            .expect_err("shutdown rejects new operation ownership");
        assert_eq!(closed["_tag"], "WorktreeOperationError");
        assert_eq!(closed["reason"], "operation-shutting-down");
    }
}

#[cfg(test)]
mod mutation_invalidation_tests {
    use std::{
        path::{Path, PathBuf},
        process::Command,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use serde_json::json;
    use tempfile::TempDir;
    use tokio::sync::Semaphore;
    use tokio_util::sync::CancellationToken;

    use super::{
        WorktreeCatalogRpcServices, WorktreeRemovalQuiesceFuture, WorktreeRemovalQuiesceLease,
        WorktreeRemovalQuiesceRequest, WorktreeRemovalQuiescer, create_managed_worktree,
        get_removal_plan, remove_from_bibcode, remove_worktree,
    };
    use crate::{
        git::GitRepository,
        orchestration::{
            EngineOptions, OrchestrationCommand, OrchestrationEngine, engine::TestHooks,
        },
        persistence::{
            CommandReceipt, Database, ProjectionThreadSession, Repositories, run_migrations,
        },
        rpc::{RequestId, RpcRequest},
        worktree_catalog::{CatalogRefreshTrigger, WorktreeCatalogService},
    };

    struct HandlerFixture {
        _root: TempDir,
        main: std::path::PathBuf,
        hooks: TestHooks,
        engine: OrchestrationEngine,
        catalog: WorktreeCatalogService,
        services: WorktreeCatalogRpcServices,
    }

    impl HandlerFixture {
        async fn new(quiescer: Arc<dyn WorktreeRemovalQuiescer>) -> Self {
            let root = tempfile::tempdir().expect("mutation timing fixture");
            let main = root.path().join("main");
            std::fs::create_dir(&main).expect("primary directory");
            git(&main, &["init", "--initial-branch=main"]);
            git(&main, &["config", "user.email", "rpc@example.invalid"]);
            git(&main, &["config", "user.name", "RPC Test"]);
            std::fs::write(main.join("README.md"), "fixture\n").expect("fixture file");
            git(&main, &["add", "README.md"]);
            git(&main, &["commit", "-m", "initial"]);

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
                    queue_capacity: 16,
                    test_hooks: hooks.clone(),
                },
            )
            .await
            .expect("orchestration");
            engine
                .dispatch(
                    serde_json::from_value(json!({
                        "type":"project.create",
                        "commandId":"project-create",
                        "projectId":"project-1",
                        "title":"Catalog RPC",
                        "workspaceRoot":main,
                        "defaultModelSelection":null,
                        "createdAt":"2026-08-23T00:00:00Z"
                    }))
                    .expect("project command"),
                )
                .await
                .expect("project created");
            let repository = Arc::new(GitRepository::default());
            let catalog = WorktreeCatalogService::new(
                Arc::new(engine.repositories()),
                Arc::clone(&repository),
            );
            catalog
                .refresh("project-1", CatalogRefreshTrigger::Explicit)
                .await
                .expect("seed catalog");
            let services = WorktreeCatalogRpcServices::new(catalog.clone(), engine.clone())
                .with_removal_quiescer(quiescer);
            Self {
                _root: root,
                main,
                hooks,
                engine,
                catalog,
                services,
            }
        }

        async fn shutdown(self) {
            self.services.operation_runtime().shutdown().await;
            self.catalog.shutdown().await;
            self.engine.shutdown().await;
        }
    }

    struct BlockingQuiescer {
        entered: Arc<Semaphore>,
        release: Arc<Semaphore>,
    }

    impl WorktreeRemovalQuiescer for BlockingQuiescer {
        fn live_session_thread_ids(
            &self,
            _thread_ids: Vec<String>,
        ) -> super::WorktreeRemovalLiveSessionsFuture {
            Box::pin(async { Ok(Vec::new()) })
        }

        fn quiesce(
            &self,
            _admission: super::WorktreeRemovalCleanupAdmission,
            _request: WorktreeRemovalQuiesceRequest,
        ) -> WorktreeRemovalQuiesceFuture {
            let entered = Arc::clone(&self.entered);
            let release = Arc::clone(&self.release);
            Box::pin(async move {
                entered.add_permits(1);
                release
                    .acquire()
                    .await
                    .expect("quiesce release remains open")
                    .forget();
                WorktreeRemovalQuiesceLease::complete()
            })
        }
    }

    struct SessionStoppingQuiescer {
        repositories: Repositories,
        calls: Arc<AtomicUsize>,
        live_sessions: Arc<Mutex<Result<Vec<String>, String>>>,
    }

    impl WorktreeRemovalQuiescer for SessionStoppingQuiescer {
        fn live_session_thread_ids(
            &self,
            thread_ids: Vec<String>,
        ) -> super::WorktreeRemovalLiveSessionsFuture {
            let live_sessions = Arc::clone(&self.live_sessions);
            Box::pin(async move {
                let live = live_sessions.lock().expect("live sessions lock").clone()?;
                Ok(thread_ids
                    .into_iter()
                    .filter(|id| live.contains(id))
                    .collect())
            })
        }

        fn quiesce(
            &self,
            _admission: super::WorktreeRemovalCleanupAdmission,
            request: WorktreeRemovalQuiesceRequest,
        ) -> WorktreeRemovalQuiesceFuture {
            let repositories = self.repositories.clone();
            let live_sessions = Arc::clone(&self.live_sessions);
            self.calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                for thread_id in request.known_thread_ids() {
                    if let Ok(threads) = &mut *live_sessions.lock().expect("live sessions lock") {
                        threads.retain(|live_thread_id| live_thread_id != thread_id);
                    }
                    if let Some(mut session) = repositories
                        .get_thread_session(thread_id.clone())
                        .await
                        .expect("read session to stop")
                    {
                        session.status = "stopped".to_owned();
                        session.active_turn_id = None;
                        repositories
                            .upsert_thread_session(session)
                            .await
                            .expect("stop session");
                    }
                }
                WorktreeRemovalQuiesceLease::complete()
            })
        }
    }

    struct RemovalFixture {
        handler: HandlerFixture,
        linked: PathBuf,
        quiesce_calls: Arc<AtomicUsize>,
        live_sessions: Arc<Mutex<Result<Vec<String>, String>>>,
    }

    impl RemovalFixture {
        async fn new() -> Self {
            let mut handler =
                HandlerFixture::new(Arc::new(super::NoopWorktreeRemovalQuiescer)).await;
            let linked = handler._root.path().join("removed");
            git(
                &handler.main,
                &[
                    "worktree",
                    "add",
                    "-b",
                    "feature/remove",
                    linked.to_str().expect("linked path"),
                ],
            );
            let quiesce_calls = Arc::new(AtomicUsize::new(0));
            let live_sessions = Arc::new(Mutex::new(Ok(Vec::new())));
            handler.services =
                handler
                    .services
                    .with_removal_quiescer(Arc::new(SessionStoppingQuiescer {
                        repositories: handler.engine.repositories(),
                        calls: Arc::clone(&quiesce_calls),
                        live_sessions: Arc::clone(&live_sessions),
                    }));
            let fixture = Self {
                handler,
                linked,
                quiesce_calls,
                live_sessions,
            };
            fixture
                .create_thread("project-1", "removed-thread", "workspace")
                .await;
            fixture
                .handler
                .catalog
                .refresh("project-1", CatalogRefreshTrigger::Explicit)
                .await
                .expect("removal catalog");
            fixture
        }

        async fn create_thread(&self, project_id: &str, thread_id: &str, kind: &str) {
            self.handler
                .engine
                .dispatch(OrchestrationCommand::ThreadCreate {
                    command_id: format!("create-{thread_id}"),
                    thread_id: thread_id.to_owned(),
                    project_id: project_id.to_owned(),
                    title: thread_id.to_owned(),
                    kind: Some(kind.to_owned()),
                    host_thread_id: None,
                    model_selection: json!({"instanceId":"codex","model":"gpt-5"}),
                    runtime_mode: "full-access".to_owned(),
                    interaction_mode: "default".to_owned(),
                    branch: Some("feature/remove".to_owned()),
                    worktree_path: Some(self.linked.to_string_lossy().into_owned()),
                    created_at: "2026-08-23T00:00:01Z".to_owned(),
                })
                .await
                .expect("thread created");
        }

        fn set_live_sessions(&self, thread_ids: &[&str]) {
            *self.live_sessions.lock().expect("live sessions lock") =
                Ok(thread_ids.iter().map(|id| (*id).to_owned()).collect());
        }

        async fn set_session(&self, thread_id: &str, status: &str) {
            self.handler
                .engine
                .repositories()
                .upsert_thread_session(ProjectionThreadSession {
                    thread_id: thread_id.to_owned(),
                    status: status.to_owned(),
                    provider_name: Some("codex".to_owned()),
                    provider_instance_id: Some("codex".to_owned()),
                    runtime_mode: "full-access".to_owned(),
                    active_turn_id: (status == "running").then(|| "turn-running".to_owned()),
                    last_error: None,
                    last_error_class: None,
                    updated_at: "2026-08-23T00:00:02Z".to_owned(),
                })
                .await
                .expect("session projection");
        }

        async fn payload(&self) -> serde_json::Value {
            let plan = get_removal_plan(
                &self.handler.services,
                request(
                    "worktree.getRemovalPlan",
                    json!({
                        "projectId":"project-1", "threadId":"removed-thread"
                    }),
                ),
                CancellationToken::new(),
            )
            .await
            .expect("removal plan");
            json!({
                "commandId":"remove-command", "projectId":"project-1", "threadId":"removed-thread",
                "mode":"delete-git-worktree", "expectedGeneration":plan["generation"],
                "planToken":plan["planToken"], "forceDirty":false, "confirmRepositoryWidePrune":false
            })
        }

        async fn remove(&self, payload: serde_json::Value) -> crate::rpc::RpcResult {
            remove_worktree(
                &self.handler.services,
                request("worktree.remove", payload),
                CancellationToken::new(),
            )
            .await
        }

        async fn assert_refused(&self, result: crate::rpc::RpcResult) {
            let error = result.expect_err("active session must refuse removal");
            assert_eq!(
                error,
                json!({
                    "_tag":"WorktreeRemovalError", "reason":"session-running",
                    "message":"Stop the running session before deleting this worktree."
                })
            );
            assert_eq!(self.quiesce_calls.load(Ordering::SeqCst), 0);
            assert!(
                self.linked.join("README.md").exists(),
                "checkout contents retained"
            );
            let inventory = GitRepository::default()
                .worktree_inventory(&self.handler.main, &CancellationToken::new())
                .await
                .expect("repository remains readable");
            let linked = self
                .linked
                .canonicalize()
                .expect("checkout remains present");
            assert!(
                inventory
                    .records
                    .iter()
                    .any(|record| record.path.canonicalize().ok().as_ref() == Some(&linked)),
                "checkout remains registered with Git"
            );
            let repositories = self.handler.engine.repositories();
            let receipt = repositories
                .get_command_receipt("remove-command".to_owned())
                .await
                .expect("receipt read")
                .expect("reservation");
            assert_eq!(receipt.status, "reserved", "no durable removal admission");
            assert!(
                repositories
                    .get_thread("removed-thread".to_owned())
                    .await
                    .expect("thread read")
                    .expect("thread")
                    .deleted_at
                    .is_none()
            );
            let registry = self.handler.catalog.availability_registry();
            let _admission = registry
                .acquire_admission("removed-thread", [self.linked.as_path()])
                .await
                .expect("availability restored after refusal");
        }
    }

    #[tokio::test]
    async fn removal_deletes_the_checkout_with_a_stale_running_projection() {
        let fixture = RemovalFixture::new().await;
        fixture.set_session("removed-thread", "running").await;
        let result = fixture
            .remove(fixture.payload().await)
            .await
            .expect("stale running projection does not block removal");
        assert_eq!(result["gitOutcome"], "removed");
        assert_eq!(result["threadRemoved"], true);
        assert!(!fixture.linked.exists());
        assert_eq!(fixture.quiesce_calls.load(Ordering::SeqCst), 1);
        fixture.handler.shutdown().await;
    }

    mod quiesce_recovery {
        use super::*;
        use crate::{
            git::{
                GitCommandError, GitPrunableWorktree, GitWorktreeInventory, GitWorktreeRecord,
                GitWorktreeRemovalInspection,
            },
            production::{
                provider_runtime::ProviderRuntimeSupervisor,
                worktree_catalog_rpc::{WorktreeRemovalGit, WorktreeRemovalGitFuture},
                worktree_runtime::{
                    WorktreeRuntime,
                    removal_test_support::{self, RemovalTestDriver},
                },
            },
            terminal::{TerminalAttachInput, TerminalManager, TerminalOpenInput, TerminalStatus},
        };

        struct ObservedQuiescer {
            runtime: WorktreeRuntime,
            calls: Arc<AtomicUsize>,
            cancel_after: Option<CancellationToken>,
        }

        impl WorktreeRemovalQuiescer for ObservedQuiescer {
            fn live_session_thread_ids(
                &self,
                ids: Vec<String>,
            ) -> super::super::WorktreeRemovalLiveSessionsFuture {
                self.runtime.live_session_thread_ids(ids)
            }
            fn admit_cleanup(&self) -> super::super::WorktreeRemovalCleanupAdmissionFuture {
                self.runtime.admit_cleanup()
            }
            fn quiesce(
                &self,
                admission: super::super::WorktreeRemovalCleanupAdmission,
                request: WorktreeRemovalQuiesceRequest,
            ) -> WorktreeRemovalQuiesceFuture {
                let future = self.runtime.quiesce(admission, request);
                let calls = self.calls.clone();
                let cancellation = self.cancel_after.clone();
                Box::pin(async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    let lease = future.await;
                    if let Some(cancellation) = cancellation {
                        cancellation.cancel();
                    }
                    lease
                })
            }
        }

        struct FailingMutationGit(GitRepository);

        impl WorktreeRemovalGit for FailingMutationGit {
            fn inventory(
                &self,
                anchor: PathBuf,
                cancellation: CancellationToken,
            ) -> WorktreeRemovalGitFuture<GitWorktreeInventory> {
                WorktreeRemovalGit::inventory(&self.0, anchor, cancellation)
            }
            fn inspect(
                &self,
                anchor: PathBuf,
                record: GitWorktreeRecord,
                cancellation: CancellationToken,
            ) -> WorktreeRemovalGitFuture<GitWorktreeRemovalInspection> {
                WorktreeRemovalGit::inspect(&self.0, anchor, record, cancellation)
            }
            fn preview_prune(
                &self,
                anchor: PathBuf,
                cancellation: CancellationToken,
            ) -> WorktreeRemovalGitFuture<Vec<GitPrunableWorktree>> {
                WorktreeRemovalGit::preview_prune(&self.0, anchor, cancellation)
            }
            fn remove(
                &self,
                anchor: PathBuf,
                _: GitWorktreeRecord,
                _: bool,
                _: CancellationToken,
            ) -> WorktreeRemovalGitFuture<()> {
                Box::pin(async move {
                    Err(GitCommandError {
                        tag: "GitCommandError",
                        operation: "remove".into(),
                        command: "git worktree remove".into(),
                        cwd: anchor.to_string_lossy().into_owned().into(),
                        diagnostics: None,
                        detail: "injected removal failure after quiesce".into(),
                    })
                })
            }
            fn prune(
                &self,
                _: PathBuf,
                _: GitWorktreeRecord,
                _: String,
                _: CancellationToken,
            ) -> WorktreeRemovalGitFuture<()> {
                panic!("delete mode must not prune")
            }
        }

        struct RecoveryFixture {
            removal: RemovalFixture,
            provider: Arc<ProviderRuntimeSupervisor>,
            driver: Arc<RemovalTestDriver>,
            runtime: WorktreeRuntime,
            terminals: TerminalManager,
        }

        impl RecoveryFixture {
            async fn new(cancel_after: Option<CancellationToken>) -> Self {
                let mut removal = RemovalFixture::new().await;
                let engine = &removal.handler.engine;
                let driver = Arc::new(RemovalTestDriver::default());
                let provider = removal_test_support::supervisor(engine, driver.clone());
                provider
                    .launch(removal_test_support::launch(
                        "removed-thread",
                        removal.linked.clone(),
                    ))
                    .await
                    .unwrap();
                let (runtime, terminals) = removal_test_support::runtime(
                    engine,
                    provider.clone(),
                    removal.handler.catalog.availability_registry(),
                );
                terminals
                    .open(TerminalOpenInput::new(
                        "removed-thread",
                        "terminal",
                        removal.linked.clone(),
                        80,
                        24,
                    ))
                    .await
                    .unwrap();
                removal.handler.services =
                    removal
                        .handler
                        .services
                        .with_removal_quiescer(Arc::new(ObservedQuiescer {
                            runtime: runtime.clone(),
                            calls: removal.quiesce_calls.clone(),
                            cancel_after,
                        }));
                Self {
                    removal,
                    provider,
                    driver,
                    runtime,
                    terminals,
                }
            }

            async fn assert_resumable(&self) {
                assert!(self.removal.linked.join("README.md").exists());
                let repositories = self.removal.handler.engine.repositories();
                let row = repositories
                    .get_provider_session_runtime("removed-thread".into())
                    .await
                    .unwrap()
                    .expect("resume state retained");
                assert_eq!(row.status, "suspended");
                assert_eq!(
                    row.resume_cursor,
                    Some(json!({"threadId":"native-removal-session"}))
                );
                let session = repositories
                    .get_thread_session("removed-thread".into())
                    .await
                    .unwrap()
                    .unwrap();
                assert!(!matches!(session.status.as_str(), "running" | "starting"));
                assert!(session.active_turn_id.is_none());
                assert!(
                    self.provider
                        .capture_session_identity("removed-thread")
                        .await
                        .unwrap()
                        .is_none()
                );
                assert_eq!(self.driver.shutdowns.load(Ordering::SeqCst), 1);
                assert_eq!(
                    self.terminals
                        .attach(TerminalAttachInput::existing("removed-thread", "terminal"))
                        .await
                        .unwrap()
                        .initial
                        .status,
                    TerminalStatus::Exited
                );
                assert!(
                    repositories
                        .get_thread("removed-thread".into())
                        .await
                        .unwrap()
                        .unwrap()
                        .deleted_at
                        .is_none()
                );
                self.removal
                    .handler
                    .catalog
                    .availability_registry()
                    .acquire_admission("removed-thread", [self.removal.linked.as_path()])
                    .await
                    .expect("failure restores availability");
            }

            async fn shutdown(self) {
                self.runtime.shutdown().await;
                self.provider.shutdown().await.unwrap();
                self.terminals.shutdown().await;
                self.removal.handler.shutdown().await;
            }
        }

        #[tokio::test]
        async fn git_failure_after_quiesce_preserves_idle_resume_state() {
            let mut f = RecoveryFixture::new(None).await;
            f.removal.handler.services = f
                .removal
                .handler
                .services
                .with_removal_git(Arc::new(FailingMutationGit(GitRepository::default())));
            let error = f
                .removal
                .remove(f.removal.payload().await)
                .await
                .unwrap_err();
            assert_eq!(error["reason"], "git-failed");
            f.assert_resumable().await;
            f.shutdown().await;
        }

        #[tokio::test]
        async fn cancellation_after_quiesce_preserves_idle_resume_state() {
            let cancellation = CancellationToken::new();
            let f = RecoveryFixture::new(Some(cancellation.clone())).await;
            let error = remove_worktree(
                &f.removal.handler.services,
                request("worktree.remove", f.removal.payload().await),
                cancellation,
            )
            .await
            .unwrap_err();
            assert_eq!(
                error,
                super::super::encode(super::super::removal_orchestration_error(
                    crate::orchestration::OrchestrationError::Cancelled
                ))
            );
            assert_eq!(f.removal.quiesce_calls.load(Ordering::SeqCst), 1);
            f.assert_resumable().await;
            f.shutdown().await;
        }

        #[tokio::test]
        async fn cancellation_before_quiesce_preserves_live_resources() {
            let mut f = RecoveryFixture::new(None).await;
            let entered = Arc::new(Semaphore::new(0));
            f.removal.handler.services.removal_drain_entered = Some(entered.clone());
            let payload = f.removal.payload().await;
            let registry = f.removal.handler.catalog.availability_registry();
            let admission = registry
                .acquire_admission("removed-thread", [f.removal.linked.as_path()])
                .await
                .unwrap();
            let cancellation = CancellationToken::new();
            let operation_cancellation = cancellation.clone();
            let services = f.removal.handler.services.clone();
            let task = tokio::spawn(async move {
                remove_worktree(
                    &services,
                    request("worktree.remove", payload),
                    operation_cancellation,
                )
                .await
            });
            tokio::time::timeout(Duration::from_secs(30), entered.acquire())
                .await
                .unwrap()
                .unwrap()
                .forget();
            cancellation.cancel();
            drop(admission);
            let error = task.await.unwrap().unwrap_err();
            assert_eq!(
                error,
                super::super::encode(super::super::removal_orchestration_error(
                    crate::orchestration::OrchestrationError::Cancelled
                ))
            );
            assert_eq!(f.removal.quiesce_calls.load(Ordering::SeqCst), 0);
            assert_eq!(f.driver.shutdowns.load(Ordering::SeqCst), 0);
            assert!(
                f.provider
                    .capture_session_identity("removed-thread")
                    .await
                    .unwrap()
                    .is_some()
            );
            assert_eq!(
                f.terminals
                    .attach(TerminalAttachInput::existing("removed-thread", "terminal"))
                    .await
                    .unwrap()
                    .initial
                    .status,
                TerminalStatus::Running
            );
            assert!(f.removal.linked.exists());
            f.shutdown().await;
        }

        #[tokio::test]
        async fn successful_removal_cleans_idle_and_previously_suspended_runtime_rows() {
            let f = RecoveryFixture::new(None).await;
            f.removal
                .create_thread("project-1", "panel-thread", "panel")
                .await;
            let repositories = f.removal.handler.engine.repositories();
            let mut row = repositories
                .get_provider_session_runtime("removed-thread".into())
                .await
                .unwrap()
                .unwrap();
            row.thread_id = "panel-thread".into();
            row.status = "suspended".into();
            repositories
                .upsert_provider_session_runtime(row)
                .await
                .unwrap();
            let result = f.removal.remove(f.removal.payload().await).await.unwrap();
            assert_eq!(result["gitOutcome"], "removed");
            assert!(!f.removal.linked.exists());
            assert!(
                repositories
                    .list_provider_session_runtimes()
                    .await
                    .unwrap()
                    .is_empty()
            );
            f.shutdown().await;
        }

        #[tokio::test]
        async fn post_git_detach_failure_keeps_cursor_until_prepared_retry() {
            let f = RecoveryFixture::new(None).await;
            let payload = f.removal.payload().await;
            f.removal
                .handler
                .hooks
                .fail_next_projector("projection.threads", Some("thread.deleted"));
            f.removal
                .remove(payload.clone())
                .await
                .expect_err("injected detach failure");
            assert!(
                !f.removal.linked.exists(),
                "Git already removed the checkout"
            );
            let repositories = f.removal.handler.engine.repositories();
            let row = repositories
                .get_provider_session_runtime("removed-thread".into())
                .await
                .unwrap()
                .expect("resume cursor survives failed detach");
            assert_eq!(row.status, "suspended");
            assert_eq!(
                row.resume_cursor,
                Some(json!({"threadId":"native-removal-session"}))
            );
            assert!(
                repositories
                    .get_thread("removed-thread".into())
                    .await
                    .unwrap()
                    .unwrap()
                    .deleted_at
                    .is_none()
            );
            let result = f.removal.remove(payload).await.unwrap();
            assert_eq!(result["threadRemoved"], true);
            assert!(
                repositories
                    .get_provider_session_runtime("removed-thread".into())
                    .await
                    .unwrap()
                    .is_none()
            );
            f.shutdown().await;
        }

        #[tokio::test]
        async fn detach_only_stops_running_sessions_and_cleans_deleted_runtime_rows() {
            let f = RecoveryFixture::new(None).await;
            f.removal.set_session("removed-thread", "running").await;
            f.removal
                .create_thread("project-1", "panel-thread", "panel")
                .await;
            let repositories = f.removal.handler.engine.repositories();
            let mut row = repositories
                .get_provider_session_runtime("removed-thread".into())
                .await
                .unwrap()
                .unwrap();
            row.thread_id = "panel-thread".into();
            row.status = "suspended".into();
            repositories
                .upsert_provider_session_runtime(row)
                .await
                .unwrap();
            let result = remove_from_bibcode(&f.removal.handler.services, request("worktree.removeFromBibCode", json!({"commandId":"detach", "projectId":"project-1", "threadId":"removed-thread"})), CancellationToken::new()).await.unwrap();
            assert_eq!(result["threadRemoved"], true);
            assert!(f.removal.linked.exists());
            assert_eq!(f.driver.shutdowns.load(Ordering::SeqCst), 1);
            assert!(
                f.provider
                    .capture_session_identity("removed-thread")
                    .await
                    .unwrap()
                    .is_none()
            );
            assert!(
                repositories
                    .list_provider_session_runtimes()
                    .await
                    .unwrap()
                    .is_empty()
            );
            f.shutdown().await;
        }
    }

    #[tokio::test]
    async fn removal_does_not_pair_a_stale_status_with_another_threads_live_session() {
        let fixture = RemovalFixture::new().await;
        fixture
            .create_thread("project-1", "panel-thread", "panel")
            .await;
        fixture.set_session("removed-thread", "running").await;
        fixture.set_session("panel-thread", "ready").await;
        fixture.set_live_sessions(&["panel-thread"]);
        let result = fixture
            .remove(fixture.payload().await)
            .await
            .expect("only the idle panel session is live");
        assert_eq!(result["gitOutcome"], "removed");
        assert_eq!(result["threadRemoved"], true);
        assert!(!fixture.linked.exists());
        fixture.handler.shutdown().await;
    }

    #[tokio::test]
    async fn removal_refuses_running_owner_before_quiesce_or_git() {
        let fixture = RemovalFixture::new().await;
        fixture.set_live_sessions(&["removed-thread"]);
        fixture.set_session("removed-thread", "running").await;
        fixture
            .assert_refused(fixture.remove(fixture.payload().await).await)
            .await;
        fixture.handler.shutdown().await;
    }

    #[tokio::test]
    async fn removal_refuses_running_panel_in_the_same_checkout() {
        let fixture = RemovalFixture::new().await;
        fixture.set_live_sessions(&["panel-thread"]);
        fixture
            .create_thread("project-1", "panel-thread", "panel")
            .await;
        fixture.set_session("panel-thread", "running").await;
        fixture
            .assert_refused(fixture.remove(fixture.payload().await).await)
            .await;
        fixture.handler.shutdown().await;
    }

    #[tokio::test]
    async fn removal_refuses_starting_session() {
        let fixture = RemovalFixture::new().await;
        fixture.set_live_sessions(&["removed-thread"]);
        fixture.set_session("removed-thread", "starting").await;
        fixture
            .assert_refused(fixture.remove(fixture.payload().await).await)
            .await;
        fixture.handler.shutdown().await;
    }

    #[tokio::test]
    async fn removal_refuses_running_session_in_another_project_sharing_the_repository() {
        let fixture = RemovalFixture::new().await;
        fixture.set_live_sessions(&["alias-panel"]);
        fixture.handler.engine.dispatch(serde_json::from_value(json!({
            "type":"project.create", "commandId":"project-alias-create", "projectId":"project-alias",
            "title":"Alias", "workspaceRoot":fixture.linked, "defaultModelSelection":null,
            "createdAt":"2026-08-23T00:00:01Z"
        })).expect("alias project command")).await.expect("alias project");
        fixture
            .handler
            .catalog
            .refresh("project-alias", CatalogRefreshTrigger::Explicit)
            .await
            .expect("pin alias repository");
        fixture
            .create_thread("project-alias", "alias-panel", "panel")
            .await;
        fixture.set_session("alias-panel", "running").await;
        fixture
            .assert_refused(fixture.remove(fixture.payload().await).await)
            .await;
        fixture.handler.shutdown().await;
    }

    #[tokio::test]
    async fn removal_refuses_when_running_session_liveness_cannot_be_read() {
        let fixture = RemovalFixture::new().await;
        fixture.set_session("removed-thread", "running").await;
        *fixture.live_sessions.lock().expect("live sessions lock") =
            Err("provider supervisor unavailable".to_owned());
        fixture
            .assert_refused(fixture.remove(fixture.payload().await).await)
            .await;
        fixture.handler.shutdown().await;
    }

    #[tokio::test]
    async fn removal_stops_a_ready_session_through_quiesce_and_deletes_the_checkout() {
        let fixture = RemovalFixture::new().await;
        fixture.set_live_sessions(&["removed-thread"]);
        fixture.set_session("removed-thread", "ready").await;
        let result = fixture
            .remove(fixture.payload().await)
            .await
            .expect("ready removal");
        assert_eq!(result["gitOutcome"], "removed");
        assert_eq!(result["threadRemoved"], true);
        assert!(!fixture.linked.exists());
        assert_eq!(fixture.quiesce_calls.load(Ordering::SeqCst), 1);
        assert!(
            fixture
                .live_sessions
                .lock()
                .expect("live sessions lock")
                .as_ref()
                .expect("liveness lookup")
                .is_empty(),
            "quiesce stops the live ready session"
        );
        assert_eq!(
            fixture
                .handler
                .engine
                .repositories()
                .get_thread_session("removed-thread".to_owned())
                .await
                .expect("session read")
                .expect("session")
                .status,
            "stopped"
        );
        fixture.handler.shutdown().await;
    }

    #[tokio::test]
    async fn removal_waits_for_admitted_work_before_checking_the_session() {
        let mut fixture = RemovalFixture::new().await;
        fixture.set_live_sessions(&["removed-thread"]);
        let entered = Arc::new(Semaphore::new(0));
        fixture.handler.services.removal_drain_entered = Some(Arc::clone(&entered));
        fixture.set_session("removed-thread", "ready").await;
        let payload = fixture.payload().await;
        let registry = fixture.handler.catalog.availability_registry();
        let admission = registry
            .acquire_admission("removed-thread", [fixture.linked.as_path()])
            .await
            .expect("turn admission");
        let services = fixture.handler.services.clone();
        let removal = tokio::spawn(async move {
            remove_worktree(
                &services,
                request("worktree.remove", payload),
                CancellationToken::new(),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(30), entered.acquire())
            .await
            .expect("removal reaches the admission drain")
            .expect("drain signal")
            .forget();
        assert!(
            !removal.is_finished(),
            "removal waits for the held admission"
        );
        assert!(
            registry.guard_thread("removed-thread").await.is_err(),
            "new admissions are fenced"
        );
        assert_eq!(
            fixture.quiesce_calls.load(Ordering::SeqCst),
            0,
            "drain precedes quiesce"
        );
        fixture.set_session("removed-thread", "running").await;
        drop(admission);
        fixture
            .assert_refused(removal.await.expect("removal joins"))
            .await;
        fixture.handler.shutdown().await;
    }

    #[tokio::test]
    async fn removal_refuses_when_an_admission_does_not_drain_before_the_deadline() {
        let mut fixture = RemovalFixture::new().await;
        fixture.set_live_sessions(&["removed-thread"]);
        fixture.handler.services.removal_admission_timeout = Duration::from_millis(20);
        let payload = fixture.payload().await;
        let registry = fixture.handler.catalog.availability_registry();
        let _admission = registry
            .acquire_admission("removed-thread", [fixture.linked.as_path()])
            .await
            .expect("held admission");
        let result = tokio::time::timeout(Duration::from_secs(2), fixture.remove(payload))
            .await
            .expect("bounded admission drain");
        fixture.assert_refused(result).await;
        fixture.handler.shutdown().await;
    }

    #[tokio::test]
    async fn removal_resumes_after_git_success_even_with_a_running_session_and_replays() {
        let fixture = RemovalFixture::new().await;
        fixture.set_live_sessions(&["removed-thread"]);
        let payload = fixture.payload().await;
        fixture
            .handler
            .engine
            .repositories()
            .reserve_command_receipt(CommandReceipt {
                command_id: "remove-command".to_owned(),
                aggregate_kind: "project".to_owned(),
                aggregate_id: "project-1".to_owned(),
                accepted_at: "2026-08-23T00:00:02Z".to_owned(),
                result_sequence: 0,
                status: "prepared".to_owned(),
                error: None,
                payload_digest: Some(
                    crate::orchestration::canonical_command_digest(&payload).expect("digest"),
                ),
            })
            .await
            .expect("prepared receipt");
        git(
            &fixture.handler.main,
            &[
                "worktree",
                "remove",
                fixture.linked.to_str().expect("linked path"),
            ],
        );
        fixture.set_session("removed-thread", "running").await;
        let result = fixture
            .remove(payload.clone())
            .await
            .expect("resume durable detach");
        assert_eq!(result["gitOutcome"], "removed");
        assert_eq!(result["threadRemoved"], true);
        assert_eq!(fixture.quiesce_calls.load(Ordering::SeqCst), 1);
        fixture.set_session("removed-thread", "running").await;
        fixture.set_live_sessions(&["removed-thread"]);
        assert_eq!(
            fixture
                .remove(payload)
                .await
                .expect("accepted receipt replay"),
            result
        );
        assert_eq!(fixture.quiesce_calls.load(Ordering::SeqCst), 1);
        fixture.handler.shutdown().await;
    }

    #[tokio::test]
    async fn detach_only_still_stops_and_detaches_a_running_session() {
        let fixture = RemovalFixture::new().await;
        fixture.set_live_sessions(&["removed-thread"]);
        fixture.set_session("removed-thread", "running").await;
        let result = remove_from_bibcode(&fixture.handler.services, request("worktree.removeFromBibCode", json!({
            "commandId":"detach-command", "projectId":"project-1", "threadId":"removed-thread"
        })), CancellationToken::new()).await.expect("detach running session");
        assert_eq!(result["gitOutcome"], "not-requested");
        assert_eq!(result["threadRemoved"], true);
        assert!(fixture.linked.join("README.md").exists());
        assert_eq!(fixture.quiesce_calls.load(Ordering::SeqCst), 1);
        fixture.handler.shutdown().await;
    }

    #[tokio::test]
    async fn managed_creation_invalidates_only_after_accepted_terminal_receipt() {
        let fixture = HandlerFixture::new(Arc::new(super::NoopWorktreeRemovalQuiescer)).await;
        git(&fixture.main, &["branch", "feature/managed"]);
        let pause = fixture.hooks.pause_before_next_command_persist();
        let services = fixture.services.clone();
        let handler = tokio::spawn(async move {
            create_managed_worktree(
                &services,
                request(
                    "worktree.createManaged",
                    json!({
                        "commandId":"managed-create",
                        "projectId":"project-1",
                        "threadId":"managed-thread",
                        "title":"Managed",
                        "refName":"feature/managed",
                        "newRefName":null,
                        "baseRefName":null,
                        "threadDefaults":{
                            "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                            "runtimeMode":"full-access",
                            "interactionMode":"default"
                        }
                    }),
                ),
                CancellationToken::new(),
            )
            .await
        });

        tokio::time::timeout(Duration::from_secs(30), pause.wait_until_entered())
            .await
            .expect("managed creation reaches terminal persistence");
        assert_eq!(
            fixture.catalog.invalidation_count_for_test("project-1"),
            Some(0)
        );
        assert!(
            !fixture
                .engine
                .repositories()
                .get_command_receipt("managed-create".to_owned())
                .await
                .expect("managed receipt read")
                .is_some_and(|receipt| receipt.status == "accepted")
        );
        pause.release();
        handler
            .await
            .expect("managed handler joins")
            .expect("managed creation succeeds");
        assert_eq!(
            fixture
                .engine
                .repositories()
                .get_command_receipt("managed-create".to_owned())
                .await
                .expect("managed receipt read")
                .expect("managed receipt")
                .status,
            "accepted"
        );
        assert_eq!(
            fixture.catalog.invalidation_count_for_test("project-1"),
            Some(1)
        );
        fixture.shutdown().await;
    }

    #[tokio::test]
    async fn removal_invalidates_only_after_accepted_terminal_receipt() {
        let entered = Arc::new(Semaphore::new(0));
        let release = Arc::new(Semaphore::new(0));
        let fixture = HandlerFixture::new(Arc::new(BlockingQuiescer {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        }))
        .await;
        let linked = fixture._root.path().join("removed");
        git(
            &fixture.main,
            &[
                "worktree",
                "add",
                "-b",
                "feature/remove",
                linked.to_str().expect("UTF-8 linked path"),
            ],
        );
        fixture
            .engine
            .dispatch(OrchestrationCommand::ThreadCreate {
                command_id: "thread-create".to_owned(),
                thread_id: "removed-thread".to_owned(),
                project_id: "project-1".to_owned(),
                title: "Removed".to_owned(),
                kind: Some("workspace".to_owned()),
                host_thread_id: None,
                model_selection: json!({"instanceId":"codex","model":"gpt-5"}),
                runtime_mode: "full-access".to_owned(),
                interaction_mode: "default".to_owned(),
                branch: Some("feature/remove".to_owned()),
                worktree_path: Some(linked.to_string_lossy().into_owned()),
                created_at: "2026-08-23T00:00:01Z".to_owned(),
            })
            .await
            .expect("thread created");
        fixture
            .catalog
            .refresh("project-1", CatalogRefreshTrigger::Explicit)
            .await
            .expect("catalog includes removal owner");
        let services = fixture.services.clone();
        let handler = tokio::spawn(async move {
            remove_from_bibcode(
                &services,
                request(
                    "worktree.removeFromBibCode",
                    json!({
                        "commandId":"remove-command",
                        "projectId":"project-1",
                        "threadId":"removed-thread"
                    }),
                ),
                CancellationToken::new(),
            )
            .await
        });
        entered
            .acquire()
            .await
            .expect("removal reaches quiesce")
            .forget();
        let pause = fixture.hooks.pause_before_next_command_persist();
        release.add_permits(1);
        tokio::time::timeout(Duration::from_secs(30), pause.wait_until_entered())
            .await
            .expect("removal reaches terminal persistence");
        assert_eq!(
            fixture.catalog.invalidation_count_for_test("project-1"),
            Some(0)
        );
        assert!(
            !fixture
                .engine
                .repositories()
                .get_command_receipt("remove-command".to_owned())
                .await
                .expect("removal receipt read")
                .is_some_and(|receipt| receipt.status == "accepted")
        );
        pause.release();
        handler
            .await
            .expect("removal handler joins")
            .expect("removal succeeds");
        assert_eq!(
            fixture
                .engine
                .repositories()
                .get_command_receipt("remove-command".to_owned())
                .await
                .expect("removal receipt read")
                .expect("removal receipt")
                .status,
            "accepted"
        );
        assert_eq!(
            fixture.catalog.invalidation_count_for_test("project-1"),
            Some(1)
        );
        fixture.shutdown().await;
    }

    fn request(tag: &str, payload: serde_json::Value) -> RpcRequest {
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

    fn git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git command");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[cfg(test)]
mod repository_identity_refresh_tests {
    use std::{process::Command, sync::Arc, time::Duration};

    use tempfile::TempDir;

    use super::{
        BranchReconciliationObserver, CatalogHealthySnapshotObserver, RepositoryIdentityThrottle,
        WorktreeCatalogOperationRuntime,
    };
    use crate::{
        git::GitRepository,
        orchestration::{EngineOptions, OrchestrationEngine},
        persistence::{Database, run_migrations},
        worktree_catalog::{
            CatalogScanStatus, WorktreeAdoptionState, WorktreeCatalogSnapshot, WorktreeDescriptor,
            WorktreeDirectoryState, WorktreeRegistrationState,
        },
    };

    fn git(dir: &std::path::Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(dir)
                .status()
                .unwrap()
                .success()
        );
    }

    async fn stored_key(engine: &OrchestrationEngine) -> Option<String> {
        engine
            .repositories()
            .get_project("p".to_owned())
            .await
            .unwrap()
            .unwrap()
            .repository_identity
            .map(|identity| identity["canonicalKey"].as_str().unwrap().to_owned())
    }

    async fn engine_with_project(checkout: &TempDir) -> OrchestrationEngine {
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
        engine
            .dispatch(
                serde_json::from_value(serde_json::json!({
                    "type":"project.create","commandId":"create","projectId":"p","title":"P",
                    "workspaceRoot":checkout.path().to_string_lossy().replace('\\', "/"),
                    "defaultModelSelection":null,"createdAt":"2026-10-05T00:00:00Z"
                }))
                .unwrap(),
            )
            .await
            .unwrap();
        engine
    }

    fn checkout_with_origin(origin: &str) -> TempDir {
        let checkout = TempDir::new().unwrap();
        git(checkout.path(), &["init", "-q"]);
        git(checkout.path(), &["remote", "add", "origin", origin]);
        checkout
    }

    async fn wait_for_key(engine: &OrchestrationEngine, key: &str) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while stored_key(engine).await.as_deref() != Some(key) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("the identity becomes {key}"));
    }

    fn observer(engine: &OrchestrationEngine) -> BranchReconciliationObserver {
        BranchReconciliationObserver {
            orchestration: engine.clone(),
            git: Some(Arc::new(GitRepository::default())),
            identity_throttle: RepositoryIdentityThrottle::default(),
            config_stamp: super::read_git_config_stamp,
            stamp_reads: Arc::default(),
            stamp_timeout: super::CONFIG_STAMP_TIMEOUT,
            operations: WorktreeCatalogOperationRuntime::new(),
        }
    }

    fn snapshot(worktrees: Vec<WorktreeDescriptor>) -> Arc<WorktreeCatalogSnapshot> {
        Arc::new(WorktreeCatalogSnapshot {
            repository_key: "repository".to_owned(),
            generation: 1,
            authoritative: true,
            observed_at: "2026-10-05T00:00:00Z".to_owned(),
            scan_status: CatalogScanStatus::Ready,
            worktrees,
            adopted_workspaces: Vec::new(),
        })
    }

    fn primary(checkout: &TempDir) -> WorktreeDescriptor {
        WorktreeDescriptor {
            worktree_key: "primary".to_owned(),
            path: checkout.path().to_string_lossy().into_owned(),
            branch: None,
            head: None,
            is_primary: true,
            is_bare: false,
            locked: false,
            lock_reason: None,
            registration_state: WorktreeRegistrationState::Registered,
            directory_state: WorktreeDirectoryState::Present,
            adoption_state: WorktreeAdoptionState::None,
            adopted_thread_id: None,
            eligible_for_adoption: false,
        }
    }

    /// A healthy scan refreshes the identity off the scan's own task, and a second scan inside the
    /// throttle interval does not read `origin` again when no primary checkout config is known.
    #[tokio::test]
    async fn healthy_scans_refresh_the_identity_once_per_throttle_interval() {
        let checkout = checkout_with_origin("git@github.com:acme/repo.git");
        let engine = engine_with_project(&checkout).await;
        let observer = observer(&engine);
        let operations = observer.operations.clone();
        let snapshot = snapshot(Vec::new());

        observer.observe("p".to_owned(), snapshot.clone()).await;
        wait_for_key(&engine, "github.com/acme/repo").await;

        git(
            checkout.path(),
            &[
                "remote",
                "set-url",
                "origin",
                "https://gitlab.com/acme/other.git",
            ],
        );
        observer.observe("p".to_owned(), snapshot).await;
        operations.shutdown().await;
        assert_eq!(
            stored_key(&engine).await.as_deref(),
            Some("github.com/acme/repo")
        );
        engine.shutdown().await;
    }

    /// `git remote set-url` rewrites the primary checkout's `.git/config`, so the next healthy scan
    /// reconciles inside the throttle interval; a config with the same size and mtime does not.
    #[tokio::test]
    async fn a_changed_git_config_reconciles_on_the_next_healthy_scan() {
        let checkout = checkout_with_origin("git@github.com:acme/repo.git");
        let engine = engine_with_project(&checkout).await;
        let observer = observer(&engine);
        let operations = observer.operations.clone();
        let snapshot = snapshot(vec![primary(&checkout)]);

        observer.observe("p".to_owned(), snapshot.clone()).await;
        wait_for_key(&engine, "github.com/acme/repo").await;

        git(
            checkout.path(),
            &[
                "remote",
                "set-url",
                "origin",
                "https://gitlab.com/acme/other.git",
            ],
        );
        observer.observe("p".to_owned(), snapshot.clone()).await;
        wait_for_key(&engine, "gitlab.com/acme/other").await;

        // Same length, and the previous mtime restored: the stamp is unchanged.
        let config = checkout.path().join(".git").join("config");
        let modified = std::fs::metadata(&config).unwrap().modified().unwrap();
        git(
            checkout.path(),
            &[
                "remote",
                "set-url",
                "origin",
                "https://gitlab.com/acme/aaaaa.git",
            ],
        );
        std::fs::File::options()
            .write(true)
            .open(&config)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        observer.observe("p".to_owned(), snapshot).await;
        operations.shutdown().await;
        assert_eq!(
            stored_key(&engine).await.as_deref(),
            Some("gitlab.com/acme/other")
        );
        engine.shutdown().await;
    }

    /// The stamp read can hang on a network mount: the observer still returns at once, the stalled
    /// read is bounded by the runtime's shutdown, and nothing reconciles meanwhile.
    #[tokio::test]
    async fn a_stalled_git_config_stat_never_holds_the_observer() {
        let checkout = checkout_with_origin("git@github.com:acme/repo.git");
        let engine = engine_with_project(&checkout).await;
        let mut observer = observer(&engine);
        observer.config_stamp = |_| Box::pin(std::future::pending());
        let operations = observer.operations.clone();

        tokio::time::timeout(
            Duration::from_secs(1),
            observer.observe("p".to_owned(), snapshot(vec![primary(&checkout)])),
        )
        .await
        .expect("the observer returns without awaiting the stamp read");
        tokio::time::timeout(Duration::from_secs(1), operations.shutdown())
            .await
            .expect("shutdown cancels the stalled stamp read");
        assert_eq!(stored_key(&engine).await, None);
        engine.shutdown().await;
    }

    /// A stat that outlives its timeout keeps its project's slot, so further scans do not start
    /// more blocking reads against the same stuck mount, yet still fall back to the interval.
    #[tokio::test]
    async fn a_stuck_git_config_stat_is_not_restarted_by_later_scans() {
        static READS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let checkout = checkout_with_origin("git@github.com:acme/repo.git");
        let engine = engine_with_project(&checkout).await;
        let mut observer = observer(&engine);
        observer.stamp_timeout = Duration::from_millis(50);
        observer.config_stamp = |_| {
            READS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(std::future::pending())
        };
        let operations = observer.operations.clone();
        let snapshot = snapshot(vec![primary(&checkout)]);

        for _ in 0..3 {
            observer.observe("p".to_owned(), snapshot.clone()).await;
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // The occupied slot reads as an unknown stamp, so the interval throttle still reconciles.
        wait_for_key(&engine, "github.com/acme/repo").await;
        operations.shutdown().await;
        assert_eq!(READS.load(std::sync::atomic::Ordering::SeqCst), 1);
        // Shutdown cancels and joins the stuck read, which releases its slot.
        assert!(observer.stamp_reads.lock().unwrap().is_empty());
        engine.shutdown().await;
    }
}
