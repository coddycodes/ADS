use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::assets::{AssetAccess, AssetError, AssetIssueRequest, AssetResource};
use crate::git::StatusMutationGuard;
use crate::review::{ReviewDiffPreviewInput, ReviewError, ReviewService};
use crate::transfer::archive::ArchiveLimits;
use crate::transfer::{self, TransferAccess, TransferError};
use crate::worktree_catalog::{
    WorkspaceAdmissionCancellation, WorkspaceAdmissionLease, WorkspaceAvailabilityRegistry,
};
use futures_util::FutureExt;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{Mutex, broadcast};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use super::paths::{self, normalize_root};
use super::search::emit_index_phase;
#[cfg(test)]
use super::search::{WorkspaceIndexPhase, WorkspaceIndexPhaseSink};
use super::watcher::{WatchScope, WatchScopeFuture, WatchSubscription, WorkspaceWatcher};
use super::{EntryKind, SearchLimits, WorkspaceError, WorkspaceSearchIndex, WorkspaceService};

const PROJECT_ENTRIES_MAX_LIMIT: usize = 200;

pub const TASK_SIX_RPC_METHODS: [&str; 13] = [
    "projects.searchEntries",
    "projects.listEntries",
    "projects.readFile",
    "projects.writeFile",
    "projects.createEntry",
    "projects.renameEntry",
    "projects.deleteEntry",
    "projects.duplicateEntry",
    "projects.createDownloadUrl",
    "projects.createUploadUrl",
    "filesystem.browse",
    "assets.createUrl",
    "review.getDiffPreview",
];

type AssetContextFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<PathBuf>, String>> + Send + 'a>>;
pub type WorkspaceMutationFuture<'a> =
    Pin<Box<dyn Future<Output = Option<StatusMutationGuard>> + Send + 'a>>;

pub trait AssetContextResolver: Send + Sync {
    fn resolve_workspace_root<'a>(&'a self, thread_id: &'a str) -> AssetContextFuture<'a>;
}

pub trait WorkspaceMutationObserver: Send + Sync {
    fn begin_workspace_mutation<'a>(&'a self, cwd: &'a Path) -> WorkspaceMutationFuture<'a>;
}

#[derive(Clone, Default)]
pub struct WorkspaceRpcDependencies {
    pub asset_access: Option<AssetAccess>,
    pub transfer_access: Option<TransferAccess>,
    /// Budgets a folder download is planned against when its URL is minted. Production uses
    /// the default; a test binds a tighter one rather than building an oversized workspace.
    pub archive_limits: ArchiveLimits,
    pub asset_context_resolver: Option<Arc<dyn AssetContextResolver>>,
    pub review_service: Option<ReviewService>,
    pub mutation_observer: Option<Arc<dyn WorkspaceMutationObserver>>,
}

struct WorkspaceMutationOwnership {
    // Field order is lifecycle-significant: finalization closes before admission release
    // can wake a waiting workspace removal.
    _finalization: Option<crate::persistence::CommitPermit>,
    _admission: Option<WorkspaceAdmissionLease>,
}

/// How often a watched root is swept, and the longest sustained change can defer its report.
///
/// The sweep stats directories rather than files, so the interval trades latency against a cost
/// proportional to the workspace's directory count. A burst is reported at the first sweep that
/// sees nothing new, so a lone change still surfaces one sweep after it is seen; the window only
/// caps the wait while changes keep arriving, because every report costs each subscriber a full
/// re-list.
const WATCH_POLL_INTERVAL: Duration = Duration::from_secs(2);
const WATCH_COALESCE_WINDOW: Duration = Duration::from_secs(10);
const WATCH_CHANNEL_CAPACITY: usize = 8;
const WATCH_BROADCAST_CAPACITY: usize = 8;

/// Cached workspace snapshots plus the invalidation counter that fences them.
///
/// A scan runs outside this lock, so an invalidation can arrive while one is in flight. The counter
/// records that, letting a finished scan tell whether the state it observed is still the current one
/// before it publishes.
#[derive(Default)]
struct IndexCache {
    snapshots: HashMap<PathBuf, WorkspaceSearchIndex>,
    generations: HashMap<PathBuf, u64>,
}

#[cfg(test)]
#[derive(Clone, Default)]
struct WorkspaceIndexTestHooks {
    cache_timer_started: Option<tokio::sync::mpsc::UnboundedSender<()>>,
    build_wait_started: Option<tokio::sync::mpsc::UnboundedSender<()>>,
    build_entered: Option<tokio::sync::mpsc::UnboundedSender<()>>,
    build_gate: Option<Arc<tokio::sync::Semaphore>>,
    watch_handoff_entered: Option<tokio::sync::mpsc::UnboundedSender<()>>,
    watch_handoff_gate: Option<Arc<tokio::sync::Semaphore>>,
    watch_event_entered: Option<tokio::sync::mpsc::UnboundedSender<()>>,
    watch_event_gate: Option<Arc<tokio::sync::Semaphore>>,
    watch_send_failure_entered: Option<tokio::sync::mpsc::UnboundedSender<()>>,
    watch_send_failure_gate: Option<Arc<tokio::sync::Semaphore>>,
}

impl IndexCache {
    fn invalidate(&mut self, canonical: &Path) {
        self.snapshots.remove(canonical);
        let generation = self.generations.entry(canonical.to_path_buf()).or_default();
        *generation = generation.wrapping_add(1);
    }

    fn generation(&self, canonical: &Path) -> u64 {
        self.generations.get(canonical).copied().unwrap_or_default()
    }
}

/// A live entry-change subscription, together with the workspace admission it runs under.
///
/// The lease is held for the subscription's lifetime so a sweep cannot outlive the availability
/// guard, and `loss_cancellation` is how the caller learns the workspace was fenced -- dropping this
/// releases the lease so a pending removal can proceed.
pub struct EntryChangeSubscription {
    changes: broadcast::Receiver<()>,
    admission: Option<WorkspaceAdmissionLease>,
}

impl EntryChangeSubscription {
    pub async fn recv(&mut self) -> Result<(), broadcast::error::RecvError> {
        self.changes.recv().await
    }

    #[must_use]
    pub fn loss_cancellation(&self) -> Option<WorkspaceAdmissionCancellation> {
        self.admission
            .as_ref()
            .map(WorkspaceAdmissionLease::loss_cancellation)
    }
}

#[derive(Clone)]
pub struct WorkspaceRpc {
    service: WorkspaceService,
    indexes: Arc<Mutex<IndexCache>>,
    /// One lock per root, held across a scan so concurrent callers wait rather than each scanning.
    index_builds: Arc<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>>,
    index_scans: Arc<AtomicU64>,
    #[cfg(test)]
    index_phase_sink: Option<WorkspaceIndexPhaseSink>,
    #[cfg(test)]
    index_test_hooks: WorkspaceIndexTestHooks,
    dependencies: WorkspaceRpcDependencies,
    availability_registry: Option<WorkspaceAvailabilityRegistry>,
    watches: Arc<Mutex<HashMap<PathBuf, broadcast::Sender<()>>>>,
    watch_tasks: TaskTracker,
    watch_shutdown: CancellationToken,
    watch_timing: (Duration, Duration),
    search_limits: SearchLimits,
}

/// Supplies the swept directories from the cached index for a root.
///
/// The last non-empty set is retained because invalidating the index empties the cache until a
/// client lists again. Falling back to an empty set in that window would narrow the sweep to the
/// root alone and miss a second change arriving in a nested directory.
struct IndexWatchScope {
    indexes: Arc<Mutex<IndexCache>>,
    last: Arc<Mutex<Vec<String>>>,
}

impl WatchScope for IndexWatchScope {
    fn directories(&self, root: PathBuf) -> WatchScopeFuture {
        let indexes = Arc::clone(&self.indexes);
        let last = Arc::clone(&self.last);
        Box::pin(async move {
            let index = indexes.lock().await.snapshots.get(&root).cloned();
            let directories = match index {
                Some(index) => index.directory_paths().await,
                None => Vec::new(),
            };
            let mut last = last.lock().await;
            if directories.is_empty() {
                return last.clone();
            }
            *last = directories.clone();
            directories
        })
    }
}

/// The index a watch last told its subscribers to list, and the cache generation it is current at.
///
/// While the generation stays put, every list served returns this index. Once anything else drops
/// the cache (a mutation, an explicit refresh), a subscriber may show a different list, so an equal
/// rebuild is no longer old news.
struct AnnouncedIndex {
    index: WorkspaceSearchIndex,
    generation: u64,
}

impl WorkspaceRpc {
    pub fn new(service: WorkspaceService) -> Self {
        Self::with_dependencies(service, WorkspaceRpcDependencies::default())
    }

    pub fn with_dependencies(
        service: WorkspaceService,
        dependencies: WorkspaceRpcDependencies,
    ) -> Self {
        Self {
            service,
            indexes: Arc::new(Mutex::new(IndexCache::default())),
            index_builds: Arc::new(Mutex::new(HashMap::new())),
            index_scans: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            index_phase_sink: None,
            #[cfg(test)]
            index_test_hooks: WorkspaceIndexTestHooks::default(),
            dependencies,
            availability_registry: None,
            watches: Arc::new(Mutex::new(HashMap::new())),
            watch_tasks: TaskTracker::new(),
            watch_shutdown: CancellationToken::new(),
            watch_timing: (WATCH_POLL_INTERVAL, WATCH_COALESCE_WINDOW),
            search_limits: SearchLimits::default(),
        }
    }

    #[cfg(test)]
    fn with_index_phase_sink_for_test(mut self, sink: WorkspaceIndexPhaseSink) -> Self {
        self.index_phase_sink = Some(sink);
        self
    }

    #[cfg(test)]
    fn with_index_cache_timer_started_for_test(
        mut self,
        started: tokio::sync::mpsc::UnboundedSender<()>,
    ) -> Self {
        self.index_test_hooks.cache_timer_started = Some(started);
        self
    }

    #[cfg(test)]
    fn with_index_build_gate_for_test(
        mut self,
        gate: Arc<tokio::sync::Semaphore>,
        wait_started: tokio::sync::mpsc::UnboundedSender<()>,
        entered: tokio::sync::mpsc::UnboundedSender<()>,
    ) -> Self {
        self.index_test_hooks.build_gate = Some(gate);
        self.index_test_hooks.build_wait_started = Some(wait_started);
        self.index_test_hooks.build_entered = Some(entered);
        self
    }

    #[cfg(test)]
    fn with_watch_handoff_gate_for_test(
        mut self,
        gate: Arc<tokio::sync::Semaphore>,
        entered: tokio::sync::mpsc::UnboundedSender<()>,
    ) -> Self {
        self.index_test_hooks.watch_handoff_gate = Some(gate);
        self.index_test_hooks.watch_handoff_entered = Some(entered);
        self
    }

    #[cfg(test)]
    fn with_watch_delivery_gates_for_test(
        mut self,
        event_gate: Arc<tokio::sync::Semaphore>,
        event_entered: tokio::sync::mpsc::UnboundedSender<()>,
        send_failure_gate: Arc<tokio::sync::Semaphore>,
        send_failure_entered: tokio::sync::mpsc::UnboundedSender<()>,
    ) -> Self {
        self.index_test_hooks.watch_event_gate = Some(event_gate);
        self.index_test_hooks.watch_event_entered = Some(event_entered);
        self.index_test_hooks.watch_send_failure_gate = Some(send_failure_gate);
        self.index_test_hooks.watch_send_failure_entered = Some(send_failure_entered);
        self
    }

    /// Overrides the entry index bounds.
    ///
    /// Exists so tests can reach the bounds with a handful of files; production callers keep the
    /// defaults.
    #[must_use]
    pub fn with_search_limits(mut self, limits: SearchLimits) -> Self {
        self.search_limits = limits;
        self
    }

    /// Overrides the change-sweep cadence.
    ///
    /// Exists so tests can drive the sweep faster than the production interval instead of sleeping
    /// on it; production callers keep the defaults.
    #[must_use]
    pub fn with_watch_timing(mut self, poll_interval: Duration, coalesce_window: Duration) -> Self {
        self.watch_timing = (poll_interval, coalesce_window);
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

    pub async fn handle(&self, method: &str, payload: Value) -> Result<Value, Value> {
        self.handle_with_cancellation(method, payload, CancellationToken::new())
            .await
    }

    pub(crate) async fn handle_with_cancellation(
        &self,
        method: &str,
        payload: Value,
        cancellation: CancellationToken,
    ) -> Result<Value, Value> {
        match method {
            "projects.readFile" => {
                let input: PathInput = decode(payload)?;
                let _admission = self.acquire_path(&input.cwd).await?;
                self.service
                    .read_file(Path::new(&input.cwd), &input.relative_path)
                    .await
                    .and_then(encode)
                    .map_err(|error| {
                        error.to_project_wire(
                            "ProjectReadFileError",
                            &input.cwd,
                            &input.relative_path,
                        )
                    })
            }
            "projects.writeFile" => {
                let input: WriteInput = decode(payload)?;
                let admission = self.acquire_path(&input.cwd).await?;
                let finalization = Self::begin_mutation(admission.as_ref())?;
                let rpc = self.clone();
                let cwd = input.cwd.clone();
                let cancellation_error = WorkspaceError::Cancelled.to_project_wire(
                    "ProjectWriteFileError",
                    &input.cwd,
                    &input.relative_path,
                );
                let operation = async move {
                    let result = rpc
                        .service
                        .write_file(Path::new(&input.cwd), &input.relative_path, &input.contents)
                        .await;
                    match result {
                        Ok(outcome) => {
                            if outcome.path_set_changed
                                || outcome.index_classification_may_have_changed
                            {
                                rpc.invalidate_index(&input.cwd).await;
                            }
                            Ok(json!({ "relativePath": outcome.relative_path }))
                        }
                        Err(error) => {
                            rpc.invalidate_index(&input.cwd).await;
                            Err(error.to_project_wire(
                                "ProjectWriteFileError",
                                &input.cwd,
                                &input.relative_path,
                            ))
                        }
                    }
                };
                self.run_workspace_mutation(
                    WorkspaceMutationOwnership {
                        _finalization: finalization,
                        _admission: admission,
                    },
                    cwd,
                    cancellation,
                    cancellation_error,
                    operation,
                )
                .await
            }
            "projects.createEntry" => {
                let input: CreateInput = decode(payload)?;
                let admission = self.acquire_path(&input.cwd).await?;
                let finalization = Self::begin_mutation(admission.as_ref())?;
                let rpc = self.clone();
                let cwd = input.cwd.clone();
                let cancellation_error = WorkspaceError::Cancelled.to_project_wire(
                    "ProjectCreateEntryError",
                    &input.cwd,
                    &input.relative_path,
                );
                let operation = async move {
                    let result = rpc
                        .service
                        .create_entry(Path::new(&input.cwd), &input.relative_path, input.kind)
                        .await;
                    rpc.invalidate_index(&input.cwd).await;
                    result
                        .map(|relative_path| json!({ "relativePath": relative_path }))
                        .map_err(|error| {
                            error.to_project_wire(
                                "ProjectCreateEntryError",
                                &input.cwd,
                                &input.relative_path,
                            )
                        })
                };
                self.run_workspace_mutation(
                    WorkspaceMutationOwnership {
                        _finalization: finalization,
                        _admission: admission,
                    },
                    cwd,
                    cancellation,
                    cancellation_error,
                    operation,
                )
                .await
            }
            "projects.renameEntry" => {
                let input: RenameInput = decode(payload)?;
                let admission = self.acquire_path(&input.cwd).await?;
                let finalization = Self::begin_mutation(admission.as_ref())?;
                let rpc = self.clone();
                let cwd = input.cwd.clone();
                let cancellation_error = WorkspaceError::Cancelled.to_project_wire(
                    "ProjectRenameEntryError",
                    &input.cwd,
                    &input.from_relative_path,
                );
                let operation = async move {
                    let result = rpc
                        .service
                        .rename_entry(
                            Path::new(&input.cwd),
                            &input.from_relative_path,
                            &input.to_relative_path,
                        )
                        .await;
                    rpc.invalidate_index(&input.cwd).await;
                    result
                        .map(|relative_path| json!({ "relativePath": relative_path }))
                        .map_err(|error| {
                            error.to_project_wire(
                                "ProjectRenameEntryError",
                                &input.cwd,
                                &input.from_relative_path,
                            )
                        })
                };
                self.run_workspace_mutation(
                    WorkspaceMutationOwnership {
                        _finalization: finalization,
                        _admission: admission,
                    },
                    cwd,
                    cancellation,
                    cancellation_error,
                    operation,
                )
                .await
            }
            "projects.deleteEntry" => {
                let input: PathInput = decode(payload)?;
                let admission = self.acquire_path(&input.cwd).await?;
                let finalization = Self::begin_mutation(admission.as_ref())?;
                let rpc = self.clone();
                let cwd = input.cwd.clone();
                let cancellation_error = WorkspaceError::Cancelled.to_project_wire(
                    "ProjectDeleteEntryError",
                    &input.cwd,
                    &input.relative_path,
                );
                let operation = async move {
                    let result = rpc
                        .service
                        .delete_entry(Path::new(&input.cwd), &input.relative_path)
                        .await;
                    rpc.invalidate_index(&input.cwd).await;
                    result
                        .map(|relative_path| json!({ "relativePath": relative_path }))
                        .map_err(|error| {
                            error.to_project_wire(
                                "ProjectDeleteEntryError",
                                &input.cwd,
                                &input.relative_path,
                            )
                        })
                };
                self.run_workspace_mutation(
                    WorkspaceMutationOwnership {
                        _finalization: finalization,
                        _admission: admission,
                    },
                    cwd,
                    cancellation,
                    cancellation_error,
                    operation,
                )
                .await
            }
            "projects.duplicateEntry" => {
                let input: PathInput = decode(payload)?;
                let admission = self.acquire_path(&input.cwd).await?;
                let finalization = Self::begin_mutation(admission.as_ref())?;
                let rpc = self.clone();
                let cwd = input.cwd.clone();
                let cancellation_error = WorkspaceError::Cancelled.to_project_wire(
                    "ProjectDuplicateEntryError",
                    &input.cwd,
                    &input.relative_path,
                );
                let operation = async move {
                    let result = rpc
                        .service
                        .duplicate_entry(Path::new(&input.cwd), &input.relative_path)
                        .await;
                    rpc.invalidate_index(&input.cwd).await;
                    result
                        .map(|relative_path| json!({ "relativePath": relative_path }))
                        .map_err(|error| {
                            error.to_project_wire(
                                "ProjectDuplicateEntryError",
                                &input.cwd,
                                &input.relative_path,
                            )
                        })
                };
                self.run_workspace_mutation(
                    WorkspaceMutationOwnership {
                        _finalization: finalization,
                        _admission: admission,
                    },
                    cwd,
                    cancellation,
                    cancellation_error,
                    operation,
                )
                .await
            }
            "projects.listEntries" => {
                let input: ListInput = decode(payload)?;
                let _admission = self.acquire_path(&input.cwd).await?;
                // Out-of-band filesystem changes are invisible to the cached snapshot, so a
                // request must opt in before paying for a full rebuild.
                if input.refresh == Some(true) {
                    self.refresh_index(Path::new(&input.cwd)).await;
                }
                let index = self.index(&input.cwd).await.map_err(|error| {
                    entries_wire_error("ProjectListEntriesError", &input.cwd, &error)
                })?;
                let limit = input
                    .limit
                    .map(|limit| limit.clamp(1, PROJECT_ENTRIES_MAX_LIMIT));
                encode(index.list(limit).await).map_err(|error| {
                    entries_wire_error("ProjectListEntriesError", &input.cwd, &error)
                })
            }
            "projects.searchEntries" => {
                let input: SearchInput = decode(payload)?;
                let _admission = self.acquire_path(&input.cwd).await?;
                let index = self.index(&input.cwd).await.map_err(|error| {
                    entries_wire_error("ProjectSearchEntriesError", &input.cwd, &error)
                })?;
                encode(
                    index
                        .search(&input.query, input.limit.min(PROJECT_ENTRIES_MAX_LIMIT))
                        .await,
                )
                .map_err(|error| {
                    entries_wire_error("ProjectSearchEntriesError", &input.cwd, &error)
                })
            }
            "filesystem.browse" => {
                let input: BrowseInput = decode(payload)?;
                let _admission = match &input.cwd {
                    Some(cwd) => self.acquire_path(cwd).await?,
                    None => None,
                };
                self.service
                    .browse(
                        &input.partial_path,
                        input.cwd.as_deref().map(Path::new),
                        input.mode.as_deref() == Some("directory"),
                    )
                    .await
                    .and_then(encode)
                    .map_err(|error| filesystem_wire_error(&input, &error))
            }
            "assets.createUrl" => {
                let input: AssetCreateUrlInput = decode(payload)?;
                self.handle_asset_create_url(input).await
            }
            "projects.createDownloadUrl" => {
                let input: ProjectCreateDownloadUrlInput = decode(payload)?;
                self.handle_create_download_url(input).await
            }
            "projects.createUploadUrl" => {
                let input: ProjectCreateUploadUrlInput = decode(payload)?;
                self.handle_create_upload_url(input).await
            }
            "review.getDiffPreview" => {
                let input: ReviewDiffPreviewInput = decode(payload)?;
                let _admission = self.acquire_path(&input.cwd).await?;
                self.handle_review_get_diff_preview(input).await
            }
            _ => Err(json!({
                "_tag": "Defect",
                "message": WorkspaceError::UnsupportedMethod(method.to_owned()).to_string(),
            })),
        }
    }

    /// How many full workspace scans have run.
    ///
    /// Concurrent listers of the same cold root must share one scan, and this is the value that
    /// proves it rather than inferring it from timing.
    pub fn index_scans(&self) -> u64 {
        self.index_scans.load(Ordering::Relaxed)
    }

    /// Number of workspace roots currently being swept for out-of-band changes.
    ///
    /// A sweep is only meant to exist while something is subscribed to it, so this is the value that
    /// proves sweeps are retired rather than accumulating for the process lifetime.
    pub async fn active_entry_watches(&self) -> usize {
        self.watches.lock().await.len()
    }

    /// Subscribes to out-of-band changes under `cwd`.
    ///
    /// The signal carries no paths: the index remains the single source of truth for entry data, so
    /// a subscriber re-reads it rather than applying a diff that could drift out of agreement with
    /// it. The first subscriber for a root starts the sweep; it stops once the last one goes away.
    pub async fn subscribe_entry_changes(
        &self,
        cwd: &Path,
    ) -> Result<EntryChangeSubscription, Value> {
        // Admission first, like every other request against a workspace path: a root that is being
        // removed, or already lost, must not acquire a sweep at all.
        let admission = self.acquire_path(&cwd.to_string_lossy()).await?;
        let changes = self
            .subscribe_entry_change_signal(cwd)
            .await
            .map_err(|error| {
                entries_wire_error("ProjectListEntriesError", &cwd.to_string_lossy(), &error)
            })?;
        Ok(EntryChangeSubscription { changes, admission })
    }

    async fn subscribe_entry_change_signal(
        &self,
        cwd: &Path,
    ) -> Result<broadcast::Receiver<()>, WorkspaceError> {
        let canonical = normalize_root(cwd, false).await?;
        // An existing sweep is already tracking this root, so its baseline and the snapshot are
        // already consistent; join it without paying for another scan.
        let watches = self.watches.lock().await;
        if self.watch_shutdown.is_cancelled() {
            return Err(WorkspaceError::Cancelled);
        }
        if let Some(sender) = watches.get(&canonical) {
            return Ok(sender.subscribe());
        }
        drop(watches);
        let (poll_interval, coalesce_window) = self.watch_timing;
        let watcher = WorkspaceWatcher::new(poll_interval, coalesce_window, WATCH_CHANNEL_CAPACITY);
        let scope = Arc::new(IndexWatchScope {
            indexes: Arc::clone(&self.indexes),
            last: Arc::new(Mutex::new(Vec::new())),
        });
        // Install the sweep before rebuilding. A mutation after this baseline is either included in
        // the rebuild or remains observable as a later directory-stamp change; it cannot be
        // absorbed into a post-scan baseline while the old snapshot stays cached.
        let mut subscription = watcher.watch(canonical.clone(), scope).await;
        #[cfg(test)]
        pause_workspace_watch_test(
            self.index_test_hooks.watch_handoff_entered.as_ref(),
            self.index_test_hooks.watch_handoff_gate.as_ref(),
        )
        .await;
        // Starting a sweep is a resync point. The sweep derives its directories from the snapshot,
        // so the snapshot has to exist -- and it has to be no older than the baseline about to be
        // stamped. A cached snapshot may predate changes made while nothing was watching, and those
        // are already on disk when the baseline is taken, so they could never surface as a
        // difference. Rebuild rather than inherit that blind spot.
        self.refresh_index(cwd).await;
        let announced = match self
            .announceable_index(&canonical, CancellationToken::new())
            .await
        {
            Ok(announced) => announced,
            Err(error) => {
                subscription.cancel();
                subscription.stopped().await;
                return Err(error);
            }
        };
        let mut registry = self.watches.lock().await;
        if self.watch_shutdown.is_cancelled() {
            drop(registry);
            subscription.cancel();
            subscription.stopped().await;
            return Err(WorkspaceError::Cancelled);
        }
        // Re-check: another subscriber may have installed a sweep while this one was scanning.
        if let Some(sender) = registry.get(&canonical) {
            let receiver = sender.subscribe();
            drop(registry);
            subscription.cancel();
            subscription.stopped().await;
            return Ok(receiver);
        }
        let (sender, receiver) = broadcast::channel(WATCH_BROADCAST_CAPACITY);
        registry.insert(canonical.clone(), sender.clone());
        let rpc = self.clone();
        let task = self.watch_tasks.spawn(async move {
            rpc.run_watch(canonical, subscription, sender, announced, poll_interval)
                .await;
        });
        drop(task);
        drop(registry);
        Ok(receiver)
    }

    /// Announces the root's entry changes to its subscribers until shutdown or the last one leaves.
    ///
    /// Every announcement costs each subscriber a full re-read, so a sweep event is announced only
    /// when the rebuilt index lists something different from `announced` (#111): a formatter's
    /// atomic save moves its directory twice yet lists the same entries. Anything that leaves no
    /// trustworthy baseline (a failed or raced rebuild, a list rebuilt outside the sweep) is
    /// announced, so subscribers re-read rather than keep a list nobody compared.
    async fn run_watch(
        self,
        canonical: PathBuf,
        mut subscription: WatchSubscription,
        sender: broadcast::Sender<()>,
        mut announced: Option<AnnouncedIndex>,
        poll_interval: Duration,
    ) {
        let mut idle_check = tokio::time::interval(poll_interval);
        idle_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                () = self.watch_shutdown.cancelled() => break,
                event = subscription.recv() => {
                    if event.is_none() {
                        break;
                    }
                    // Drop the snapshot before rebuilding, so neither the rebuild nor a subscriber
                    // that lists meanwhile is served the stale index.
                    let previous = {
                        let mut cache = self.indexes.lock().await;
                        let generation = cache.generation(&canonical);
                        cache.invalidate(&canonical);
                        announced
                            .take()
                            .filter(|announced| announced.generation == generation)
                    };
                    #[cfg(test)]
                    pause_workspace_watch_test(
                        self.index_test_hooks.watch_event_entered.as_ref(),
                        self.index_test_hooks.watch_event_gate.as_ref(),
                    )
                    .await;
                    // The idle check keeps running meanwhile: once nothing listens, the rebuild is
                    // cancelled rather than left to finish for nobody.
                    let cancellation = self.watch_shutdown.child_token();
                    let rebuild = self.rebuild_swept_index(&canonical, cancellation.clone());
                    tokio::pin!(rebuild);
                    announced = loop {
                        tokio::select! {
                            rebuilt = &mut rebuild => break rebuilt,
                            _ = idle_check.tick() => {
                                if sender.receiver_count() == 0 {
                                    cancellation.cancel();
                                }
                            }
                        }
                    };
                    if self.watch_shutdown.is_cancelled() {
                        break;
                    }
                    if let (Some(current), Some(previous)) = (&announced, &previous)
                        && current.index.same_entries(&previous.index).await
                    {
                        continue;
                    }
                    // A send failure means every receiver has gone; nothing is left to serve.
                    if sender.send(()).is_err() {
                        #[cfg(test)]
                        pause_workspace_watch_test(
                            self.index_test_hooks.watch_send_failure_entered.as_ref(),
                            self.index_test_hooks.watch_send_failure_gate.as_ref(),
                        )
                        .await;
                        if Self::retire_idle_watch(&canonical, &sender, &self.watches).await {
                            subscription.cancel();
                            subscription.stopped().await;
                            return;
                        }
                    }
                }
                // Losing the last receiver is otherwise invisible until the next filesystem event,
                // which on a quiet workspace never comes — the sweep would outlive the panel that
                // asked for it, and on an in-process server the whole run.
                _ = idle_check.tick() => {
                    if Self::retire_idle_watch(&canonical, &sender, &self.watches).await {
                        subscription.cancel();
                        subscription.stopped().await;
                        return;
                    }
                }
            }
        }
        subscription.cancel();
        subscription.stopped().await;
        Self::retire_watch(&canonical, &sender, &self.watches).await;
    }

    /// Rebuilds a swept root's index under the same admission a list request takes. `None` when the
    /// root is unavailable or lost meanwhile, `cancellation` fired, the scan failed (`index`
    /// records that), or something else dropped the cache while it ran.
    async fn rebuild_swept_index(
        &self,
        canonical: &Path,
        cancellation: CancellationToken,
    ) -> Option<AnnouncedIndex> {
        let admission = self.acquire_path(&canonical.to_string_lossy()).await.ok()?;
        // Removal waits for a lost workspace's admissions to drain, and no subscriber owns this
        // one, so a loss stops the scan itself, which then settles promptly.
        let rebuild = self.announceable_index(canonical, cancellation.clone());
        let Some(loss) = admission
            .as_ref()
            .map(WorkspaceAdmissionLease::loss_cancellation)
        else {
            return rebuild.await.ok().flatten();
        };
        tokio::pin!(rebuild);
        tokio::select! {
            biased;
            rebuilt = &mut rebuild => rebuilt.ok().flatten(),
            () = loss.cancelled() => {
                cancellation.cancel();
                let _ = rebuild.await;
                None
            }
        }
    }

    /// The root's index, built or cached, paired with the cache generation it is current at; `None`
    /// inside when something dropped the cache while it was built.
    async fn announceable_index(
        &self,
        canonical: &Path,
        cancellation: CancellationToken,
    ) -> Result<Option<AnnouncedIndex>, WorkspaceError> {
        let generation = self.indexes.lock().await.generation(canonical);
        let index = self
            .index_with_cancellation(&canonical.to_string_lossy(), cancellation)
            .await?;
        let current = self.indexes.lock().await.generation(canonical);
        Ok((current == generation).then_some(AnnouncedIndex { index, generation }))
    }

    /// Stops new entry-change subscriptions, cancels every active sweep, and waits for their
    /// filesystem watcher tasks to exit.
    pub async fn shutdown(&self) {
        let registered = {
            let mut watches = self.watches.lock().await;
            self.watch_shutdown.cancel();
            self.watch_tasks.close();
            std::mem::take(&mut *watches)
        };
        drop(registered);
        self.watch_tasks.wait().await;
    }

    /// Retires the watch when nothing is listening. Returns whether it was retired.
    ///
    /// The receiver count is re-checked while holding the registry lock, which is the same lock a
    /// new subscriber takes before calling `subscribe`, so a subscriber arriving concurrently either
    /// keeps this watch alive or installs its own.
    async fn retire_idle_watch(
        canonical: &Path,
        sender: &broadcast::Sender<()>,
        watches: &Arc<Mutex<HashMap<PathBuf, broadcast::Sender<()>>>>,
    ) -> bool {
        let mut watches = watches.lock().await;
        if sender.receiver_count() > 0 {
            return false;
        }
        if watches
            .get(canonical)
            .is_some_and(|current| current.same_channel(sender))
        {
            watches.remove(canonical);
        }
        true
    }

    async fn retire_watch(
        canonical: &Path,
        sender: &broadcast::Sender<()>,
        watches: &Arc<Mutex<HashMap<PathBuf, broadcast::Sender<()>>>>,
    ) {
        let mut watches = watches.lock().await;
        // Only retire the entry still registered for this root: a later subscriber may already have
        // replaced it after the last receiver went away.
        if watches
            .get(canonical)
            .is_some_and(|current| current.same_channel(sender))
        {
            watches.remove(canonical);
        }
    }

    pub async fn refresh_index(&self, cwd: &Path) {
        let Ok(canonical) = normalize_root(cwd, false).await else {
            return;
        };
        self.indexes.lock().await.invalidate(&canonical);
    }

    async fn acquire_path(&self, cwd: &str) -> Result<Option<WorkspaceAdmissionLease>, Value> {
        let Some(registry) = &self.availability_registry else {
            return Ok(None);
        };
        registry
            .acquire_path_admission([Path::new(cwd)])
            .await
            .map(Some)
            .map_err(workspace_unavailable_wire)
    }

    async fn acquire_thread(
        &self,
        thread_id: &str,
    ) -> Result<Option<WorkspaceAdmissionLease>, Value> {
        let Some(registry) = &self.availability_registry else {
            return Ok(None);
        };
        registry
            .acquire_admission(thread_id, std::iter::empty())
            .await
            .map(Some)
            .map_err(workspace_unavailable_wire)
    }

    fn begin_mutation(
        admission: Option<&WorkspaceAdmissionLease>,
    ) -> Result<Option<crate::persistence::CommitPermit>, Value> {
        admission
            .map(WorkspaceAdmissionLease::begin_finalization)
            .transpose()
            .map_err(workspace_unavailable_wire)
    }

    async fn index(&self, cwd: &str) -> Result<WorkspaceSearchIndex, WorkspaceError> {
        self.index_with_cancellation(cwd, CancellationToken::new())
            .await
    }

    /// `index`, with a build that stops once `cancellation` fires.
    async fn index_with_cancellation(
        &self,
        cwd: &str,
        cancellation: CancellationToken,
    ) -> Result<WorkspaceSearchIndex, WorkspaceError> {
        let canonical = normalize_root(Path::new(cwd), false).await?;
        let cache_started = Instant::now();
        #[cfg(test)]
        if let Some(started) = &self.index_test_hooks.cache_timer_started {
            let _ = started.send(());
        }
        if let Some(index) = self.indexes.lock().await.snapshots.get(&canonical).cloned() {
            emit_index_phase(
                #[cfg(test)]
                self.index_phase_sink.as_ref(),
                "WorkspaceSearchIndex.refresh",
                "cache_hit",
                cache_started,
                index.entry_count().await,
                "hit",
            );
            return Ok(index);
        }
        // Single-flight per root. Without it every concurrent lister runs its own full scan of the
        // same tree, which on a large workspace is seconds of duplicated work per caller.
        let build = {
            let mut builds = self.index_builds.lock().await;
            Arc::clone(builds.entry(canonical.clone()).or_default())
        };
        let wait_started = Instant::now();
        #[cfg(test)]
        if let Some(started) = &self.index_test_hooks.build_wait_started {
            let _ = started.send(());
        }
        // A cancelled caller gives up its turn rather than wait out another caller's scan.
        let _building = tokio::select! {
            building = build.lock() => building,
            () = cancellation.cancelled() => return Err(WorkspaceError::Cancelled),
        };
        // The caller that held the lock may have finished while this one waited.
        if let Some(index) = self.indexes.lock().await.snapshots.get(&canonical).cloned() {
            emit_index_phase(
                #[cfg(test)]
                self.index_phase_sink.as_ref(),
                "WorkspaceSearchIndex.refresh",
                "cache_wait",
                wait_started,
                index.entry_count().await,
                "shared",
            );
            return Ok(index);
        }
        emit_index_phase(
            #[cfg(test)]
            self.index_phase_sink.as_ref(),
            "WorkspaceSearchIndex.refresh",
            "cache_wait",
            wait_started,
            0,
            "miss",
        );
        let generation = self.indexes.lock().await.generation(&canonical);
        #[cfg(test)]
        if let Some(gate) = &self.index_test_hooks.build_gate {
            if let Some(entered) = &self.index_test_hooks.build_entered {
                let _ = entered.send(());
            }
            let _permit = tokio::select! {
                permit = gate.acquire() => permit.expect("test build gate remains open"),
                () = cancellation.cancelled() => return Err(WorkspaceError::Cancelled),
            };
        }
        let index = WorkspaceSearchIndex::new(canonical.clone(), self.search_limits);
        #[cfg(test)]
        let index = index.with_phase_sink(self.index_phase_sink.clone());
        self.index_scans.fetch_add(1, Ordering::Relaxed);
        let build_started = Instant::now();
        if let Err(error) = index.refresh(cancellation).await {
            emit_index_phase(
                #[cfg(test)]
                self.index_phase_sink.as_ref(),
                "WorkspaceSearchIndex.refresh",
                "cache_build",
                build_started,
                0,
                "error",
            );
            return Err(error);
        }
        let entry_count = index.entry_count().await;
        let mut cache = self.indexes.lock().await;
        // Publish only when nothing invalidated while the scan ran. An invalidation that arrived
        // mid-scan describes a state this snapshot never observed, so caching it would serve data
        // already known to be stale; leaving the slot empty makes the next request rebuild.
        let cache_outcome = if cache.generation(&canonical) == generation {
            cache.snapshots.insert(canonical, index.clone());
            "built"
        } else {
            "stale"
        };
        drop(cache);
        emit_index_phase(
            #[cfg(test)]
            self.index_phase_sink.as_ref(),
            "WorkspaceSearchIndex.refresh",
            "cache_build",
            build_started,
            entry_count,
            cache_outcome,
        );
        Ok(index)
    }

    pub(crate) async fn invalidate_index(&self, cwd: &str) {
        let Ok(canonical) = tokio::fs::canonicalize(cwd).await else {
            return;
        };
        self.indexes.lock().await.invalidate(&canonical);
    }

    /// Mints a short-lived signed URL for downloading a file, or a folder as a zip archive.
    ///
    /// The token binds the canonical workspace root and the relative path, so the HTTP route
    /// never has to trust a client-supplied path.
    async fn handle_create_download_url(
        &self,
        input: ProjectCreateDownloadUrlInput,
    ) -> Result<Value, Value> {
        let access = self.transfer_access(&input.cwd, &input.relative_path)?;
        let _admission = self.acquire_path(&input.cwd).await?;
        let root = normalize_root(Path::new(&input.cwd), false)
            .await
            .map_err(|error| transfer_wire(&input.cwd, &input.relative_path, &error))?;
        let (target, relative) = paths::resolve_relative(&root, &input.relative_path)
            .map_err(|error| transfer_wire(&input.cwd, &input.relative_path, &error))?;
        let (_, canonical) = paths::canonical_existing_within(&root, &target)
            .await
            .map_err(|error| transfer_wire(&input.cwd, &input.relative_path, &error))?;
        let metadata = tokio::fs::metadata(&canonical).await.map_err(|error| {
            transfer_wire(
                &input.cwd,
                &input.relative_path,
                &WorkspaceError::operation("stat", &canonical, error),
            )
        })?;
        let kind = if metadata.is_dir() {
            // Sizing the folder here is what makes an oversized download refusable: the panel
            // shows this error, whereas the redeem-time refusal reaches the user only as a bare
            // HTTP status. The route keeps its own pre-scan as defence in depth, because the
            // tree can grow between the mint and the redemption.
            self.dependencies
                .archive_limits
                .plan(&canonical)
                .await
                .map_err(|error| archive_limit_wire(&input.cwd, &input.relative_path, &error))?;
            "archive"
        } else {
            "file"
        };
        let file_name = transfer::download_file_name(&canonical, metadata.is_dir());
        let issued = access
            .issue_download(&root, &relative)
            .map_err(|error| transfer_error_wire(&input.cwd, &input.relative_path, &error))?;
        Ok(json!({
            "relativeUrl": issued.relative_url,
            "expiresAt": issued.expires_at,
            "fileName": file_name,
            "kind": kind,
        }))
    }

    /// Mints a short-lived signed URL for uploading a single named file into an existing folder.
    ///
    /// An empty `relativeDirectory` targets the workspace root itself. The token binds the file
    /// name as well as the folder, so a leaked URL can write only the file the caller named here.
    async fn handle_create_upload_url(
        &self,
        input: ProjectCreateUploadUrlInput,
    ) -> Result<Value, Value> {
        let access = self.transfer_access(&input.cwd, &input.relative_directory)?;
        // Checked before the path lease: an unusable name costs nothing to refuse.
        if transfer::upload::validate_upload_file_name(&input.file_name).is_err() {
            return Err(json!({
                "_tag": "ProjectTransferError",
                "cwd": input.cwd,
                "relativePath": input.file_name,
                "failure": "operation_failed",
                "message": transfer::upload::upload_file_name_rule(),
            }));
        }
        let _admission = self.acquire_path(&input.cwd).await?;
        let root = normalize_root(Path::new(&input.cwd), false)
            .await
            .map_err(|error| transfer_wire(&input.cwd, &input.relative_directory, &error))?;
        let relative = if input.relative_directory.is_empty() {
            String::new()
        } else {
            let (target, relative) = paths::resolve_relative(&root, &input.relative_directory)
                .map_err(|error| transfer_wire(&input.cwd, &input.relative_directory, &error))?;
            let (_, canonical) = paths::canonical_existing_within(&root, &target)
                .await
                .map_err(|error| transfer_wire(&input.cwd, &input.relative_directory, &error))?;
            if !canonical.is_dir() {
                return Err(json!({
                    "_tag": "ProjectTransferError",
                    "cwd": input.cwd,
                    "relativePath": input.relative_directory,
                    "failure": "not_found",
                    "message": "Upload target is not a folder.",
                }));
            }
            relative
        };
        let issued = access
            .issue_upload(&root, &relative, &input.file_name)
            .map_err(|error| transfer_error_wire(&input.cwd, &input.relative_directory, &error))?;
        Ok(json!({
            "relativeUrl": issued.relative_url,
            "expiresAt": issued.expires_at,
            "maxBytes": crate::transfer::MAX_UPLOAD_BYTES,
        }))
    }

    fn transfer_access(&self, cwd: &str, relative_path: &str) -> Result<&TransferAccess, Value> {
        self.dependencies.transfer_access.as_ref().ok_or_else(|| {
            json!({
                "_tag": "ProjectTransferError",
                "cwd": cwd,
                "relativePath": relative_path,
                "failure": "not_configured",
                "message": "File transfers are not configured on this server.",
            })
        })
    }

    async fn handle_asset_create_url(&self, input: AssetCreateUrlInput) -> Result<Value, Value> {
        let asset_access = self
            .dependencies
            .asset_access
            .as_ref()
            .ok_or_else(|| defect("assets.createUrl is not configured"))?;
        let mut admissions = Vec::new();
        let workspace_root = match &input.resource {
            AssetResource::WorkspaceFile { thread_id, .. } => {
                if let Some(admission) = self.acquire_thread(thread_id).await? {
                    admissions.push(admission);
                }
                let resolver = self
                    .dependencies
                    .asset_context_resolver
                    .as_ref()
                    .ok_or_else(|| {
                        defect("assets.createUrl requires a workspace context resolver")
                    })?;
                match resolver.resolve_workspace_root(thread_id).await {
                    Ok(Some(root)) => {
                        if let Some(admission) = self.acquire_path(&root.to_string_lossy()).await? {
                            admissions.push(admission);
                        }
                        Some(root)
                    }
                    Ok(None) => {
                        return Err(asset_wire(
                            &input.resource,
                            "AssetWorkspaceContextNotFoundError",
                        ));
                    }
                    Err(message) => {
                        let mut value =
                            asset_wire(&input.resource, "AssetWorkspaceContextResolutionError");
                        value
                            .as_object_mut()
                            .expect("asset error")
                            .insert("detail".to_owned(), json!(message));
                        return Err(value);
                    }
                }
            }
            AssetResource::ProjectFavicon { cwd } => {
                if let Some(admission) = self.acquire_path(cwd).await? {
                    admissions.push(admission);
                }
                None
            }
            AssetResource::Attachment { .. } => None,
        };
        let issued = asset_access
            .issue(AssetIssueRequest {
                resource: input.resource.clone(),
                workspace_root,
            })
            .await
            .map_err(|error| asset_wire_from_error(&input.resource, &error))?;
        drop(admissions);
        encode(issued).map_err(|error| defect(&error.to_string()))
    }

    async fn run_workspace_mutation<Operation>(
        &self,
        ownership: WorkspaceMutationOwnership,
        cwd: String,
        cancellation: CancellationToken,
        cancellation_error: Value,
        operation: Operation,
    ) -> Result<Value, Value>
    where
        Operation: Future<Output = Result<Value, Value>> + Send + 'static,
    {
        let rpc = self.clone();
        let observer = self.dependencies.mutation_observer.clone();
        await_server_owned_workspace(async move {
            let _ownership = ownership;
            let status_mutation = match observer {
                Some(observer) => {
                    tokio::select! {
                        biased;
                        () = cancellation.cancelled() => return Err(cancellation_error.clone()),
                        mutation = observer.begin_workspace_mutation(Path::new(&cwd)) => mutation,
                    }
                }
                None => None,
            };
            let result = std::panic::AssertUnwindSafe(await_workspace_mutation_terminal(
                &cancellation,
                cancellation_error,
                operation,
            ))
            .catch_unwind()
            .await;
            let result = match result {
                Ok(result) => result,
                Err(panic) => {
                    rpc.invalidate_index(&cwd).await;
                    std::panic::resume_unwind(panic);
                }
            };
            if let Some(status_mutation) = status_mutation {
                status_mutation.finish().await;
            }
            result
        })
        .await
    }

    async fn handle_review_get_diff_preview(
        &self,
        input: ReviewDiffPreviewInput,
    ) -> Result<Value, Value> {
        let review_service = self
            .dependencies
            .review_service
            .as_ref()
            .ok_or_else(|| defect("review.getDiffPreview is not configured"))?;
        let result = review_service
            .get_diff_preview(input)
            .await
            .map_err(review_wire_error)?;
        encode(result).map_err(|error| defect(&error.to_string()))
    }
}

#[cfg(test)]
async fn pause_workspace_watch_test(
    entered: Option<&tokio::sync::mpsc::UnboundedSender<()>>,
    gate: Option<&Arc<tokio::sync::Semaphore>>,
) {
    if let Some(entered) = entered {
        let _ = entered.send(());
    }
    if let Some(gate) = gate {
        let _permit = gate
            .acquire()
            .await
            .expect("workspace watch test gate open");
    }
}

async fn await_server_owned_workspace(
    operation: impl Future<Output = Result<Value, Value>> + Send + 'static,
) -> Result<Value, Value> {
    match tokio::spawn(operation).await {
        Ok(result) => result,
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(error) => Err(defect(&error.to_string())),
    }
}

async fn await_workspace_mutation_terminal(
    cancellation: &CancellationToken,
    cancellation_error: Value,
    operation: impl Future<Output = Result<Value, Value>>,
) -> Result<Value, Value> {
    tokio::pin!(operation);
    tokio::select! {
        biased;
        () = cancellation.cancelled() => {
            let _ = operation.await;
            Err(cancellation_error)
        }
        result = &mut operation => result,
    }
}

fn decode<T: for<'de> Deserialize<'de>>(payload: Value) -> Result<T, Value> {
    serde_json::from_value(payload).map_err(|error| {
        json!({
            "_tag": "InvalidRequest",
            "message": error.to_string(),
        })
    })
}

fn workspace_unavailable_wire(error: impl serde::Serialize) -> Value {
    serde_json::to_value(error).expect("workspace availability error serializes")
}

fn encode<T: serde::Serialize>(value: T) -> Result<Value, WorkspaceError> {
    serde_json::to_value(value).map_err(|error| WorkspaceError::InvalidRequest(error.to_string()))
}

fn entries_wire_error(tag: &str, cwd: &str, error: &WorkspaceError) -> Value {
    json!({
        "_tag": tag,
        "cwd": cwd,
        "failure": match error {
            WorkspaceError::RootNotFound { .. } => "workspace_root_not_found",
            WorkspaceError::RootNotDirectory { .. } => "workspace_root_not_directory",
            WorkspaceError::Cancelled => "search_index_scan_timed_out",
            _ => "search_index_search_failed",
        },
        "message": error.to_string(),
    })
}

fn filesystem_wire_error(input: &BrowseInput, error: &WorkspaceError) -> Value {
    json!({
        "_tag": "FilesystemBrowseError",
        "partialPath": input.partial_path,
        "cwd": input.cwd,
        "failure": match error {
            WorkspaceError::WindowsPathUnsupported { .. } => "windows_path_unsupported",
            WorkspaceError::CurrentProjectRequired { .. } => "current_project_required",
            _ => "read_directory_failed",
        },
        "message": error.to_string(),
    })
}

fn asset_wire(resource: &AssetResource, tag: &str) -> Value {
    json!({
        "_tag": tag,
        "resource": resource,
        "message": asset_message(tag),
    })
}

fn asset_message(tag: &str) -> &'static str {
    match tag {
        "AssetWorkspaceContextNotFoundError" => "Workspace context was not found.",
        "AssetWorkspaceContextResolutionError" => "Failed to resolve workspace context.",
        "AssetWorkspaceRootNormalizationError" => "Failed to normalize the workspace root.",
        "AssetWorkspacePathValidationError" => {
            "Workspace file path must be relative to the project root."
        }
        "AssetPreviewTypeValidationError" => "Only browser documents and images can be previewed.",
        "AssetWorkspaceAssetInspectionError" => "Failed to inspect the workspace asset.",
        "AssetWorkspaceAssetNotFoundError" => "Workspace asset was not found.",
        "AssetWorkspaceResolutionError" => "Failed to resolve workspace.",
        "AssetAttachmentNotFoundError" => "Attachment was not found.",
        "AssetProjectFaviconResolutionError" => "Failed to resolve project favicon.",
        "AssetProjectFaviconInspectionError" => "Failed to inspect the project favicon.",
        "AssetProjectFaviconNotFoundError" => "Project favicon was not found.",
        "AssetSigningKeyLoadError" => "Failed to load the asset signing key.",
        _ => "Asset access failed.",
    }
}

fn asset_wire_from_error(resource: &AssetResource, error: &AssetError) -> Value {
    match error {
        AssetError::WorkspaceContextRequired => {
            asset_wire(resource, "AssetWorkspaceContextNotFoundError")
        }
        AssetError::UnsupportedPreviewType(_) => {
            asset_wire(resource, "AssetPreviewTypeValidationError")
        }
        AssetError::NotFound(_) => match resource {
            AssetResource::WorkspaceFile { .. } => {
                asset_wire(resource, "AssetWorkspaceAssetNotFoundError")
            }
            AssetResource::Attachment { .. } => {
                asset_wire(resource, "AssetAttachmentNotFoundError")
            }
            AssetResource::ProjectFavicon { .. } => {
                asset_wire(resource, "AssetProjectFaviconNotFoundError")
            }
        },
        AssetError::Workspace(workspace_error) => match resource {
            AssetResource::ProjectFavicon { .. } => match workspace_error {
                WorkspaceError::RootNotFound { .. } | WorkspaceError::RootNotDirectory { .. } => {
                    asset_wire(resource, "AssetWorkspaceRootNormalizationError")
                }
                _ => asset_wire(resource, "AssetProjectFaviconInspectionError"),
            },
            _ => match workspace_error {
                WorkspaceError::RootNotFound { .. } | WorkspaceError::RootNotDirectory { .. } => {
                    asset_wire(resource, "AssetWorkspaceRootNormalizationError")
                }
                WorkspaceError::PathOutsideRoot { .. }
                | WorkspaceError::ResolvedPathOutsideRoot { .. } => {
                    asset_wire(resource, "AssetWorkspacePathValidationError")
                }
                _ => asset_wire(resource, "AssetWorkspaceAssetInspectionError"),
            },
        },
        AssetError::Encoding(_) => asset_wire(resource, "AssetSigningKeyLoadError"),
    }
}

fn transfer_wire(cwd: &str, relative_path: &str, error: &WorkspaceError) -> Value {
    json!({
        "_tag": "ProjectTransferError",
        "cwd": cwd,
        "relativePath": relative_path,
        "failure": transfer_failure(error),
        "message": error.to_string(),
    })
}

fn transfer_failure(error: &WorkspaceError) -> &'static str {
    match error {
        WorkspaceError::PathOutsideRoot { .. } | WorkspaceError::ResolvedPathOutsideRoot { .. } => {
            "outside_root"
        }
        WorkspaceError::NotFound { .. } | WorkspaceError::RootNotFound { .. } => "not_found",
        WorkspaceError::Operation { source, .. }
            if source.kind() == std::io::ErrorKind::NotFound =>
        {
            "not_found"
        }
        _ => "operation_failed",
    }
}

/// A folder whose contents blow an archive budget is refused when its download URL is minted,
/// with a message that names the limit and its unit so the person reading the toast knows why.
fn archive_limit_wire(cwd: &str, relative_path: &str, error: &TransferError) -> Value {
    // Names the budget that was blown and the one thing that can be done about it: UI.md asks an
    // error to say what happened and what the reader can do next.
    let message = match error {
        TransferError::TooManyEntries { limit } => format!(
            "Folder exceeds the {limit}-entry download limit. Download a subfolder instead."
        ),
        TransferError::TooManyBytes { limit } => format!(
            "Folder exceeds the {} download limit. Download a subfolder instead.",
            transfer::describe_bytes(*limit)
        ),
        other => return transfer_error_wire(cwd, relative_path, other),
    };
    json!({
        "_tag": "ProjectTransferError",
        "cwd": cwd,
        "relativePath": relative_path,
        "failure": "operation_failed",
        "message": message,
    })
}

fn transfer_error_wire(cwd: &str, relative_path: &str, error: &TransferError) -> Value {
    match error {
        TransferError::Workspace(workspace) => transfer_wire(cwd, relative_path, workspace),
        _ => json!({
            "_tag": "ProjectTransferError",
            "cwd": cwd,
            "relativePath": relative_path,
            "failure": "operation_failed",
            "message": error.to_string(),
        }),
    }
}

fn review_wire_error(error: ReviewError) -> Value {
    json!({
        "_tag": "Defect",
        "message": error.to_string(),
    })
}

fn defect(message: &str) -> Value {
    json!({
        "_tag": "Defect",
        "message": message,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListInput {
    cwd: String,
    limit: Option<usize>,
    refresh: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PathInput {
    cwd: String,
    relative_path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WriteInput {
    cwd: String,
    relative_path: String,
    contents: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateInput {
    cwd: String,
    relative_path: String,
    kind: EntryKind,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenameInput {
    cwd: String,
    from_relative_path: String,
    to_relative_path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchInput {
    cwd: String,
    query: String,
    limit: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BrowseInput {
    partial_path: String,
    cwd: Option<String>,
    mode: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AssetCreateUrlInput {
    resource: AssetResource,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectCreateDownloadUrlInput {
    cwd: String,
    relative_path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProjectCreateUploadUrlInput {
    cwd: String,
    relative_directory: String,
    file_name: String,
}

#[cfg(test)]
mod tests {
    use std::panic::AssertUnwindSafe;

    use futures_util::FutureExt;

    use super::*;
    use crate::git::{
        BoxGitProcessFuture, GitProcessRunner, GitRepository, ProcessOutput, ProcessRequest,
        StatusBroadcaster, VcsStatusStreamEvent,
    };
    use crate::workspace::service::{WorkspaceWriteHook, WorkspaceWriteHookFuture};
    use crate::worktree_catalog::{AdoptedWorktreeAvailability, WorkspaceLossTransition};
    use std::time::Duration;

    const PROJECT_SAVE_STATUS_DEADLINE: Duration = Duration::from_millis(750);

    fn record_index_phases() -> (
        WorkspaceIndexPhaseSink,
        tokio::sync::mpsc::UnboundedReceiver<WorkspaceIndexPhase>,
    ) {
        tokio::sync::mpsc::unbounded_channel()
    }

    fn drain_index_phases(
        phases: &mut tokio::sync::mpsc::UnboundedReceiver<WorkspaceIndexPhase>,
    ) -> Vec<WorkspaceIndexPhase> {
        std::iter::from_fn(|| phases.try_recv().ok()).collect()
    }

    fn phase_summary(phases: &[WorkspaceIndexPhase]) -> Vec<(&'static str, &'static str)> {
        phases
            .iter()
            .map(|phase| (phase.phase, phase.cache_outcome))
            .collect()
    }

    fn run_index_git(root: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("git must be installed");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn initialize_index_git_workspace(root: &Path) {
        run_index_git(root, &["init", "--quiet"]);
        let excludes = root.join(".git/bibcode-test-global-excludes");
        std::fs::write(&excludes, "").expect("empty fixture excludes file");
        let excludes = excludes.to_string_lossy().replace('\\', "/");
        run_index_git(root, &["config", "--local", "core.excludesFile", &excludes]);
    }

    fn index_test_workspace() -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("workspace root");
        initialize_index_git_workspace(root.path());
        std::fs::write(root.path().join("tracked.txt"), "tracked").expect("fixture");
        root
    }

    fn index_benchmark_workspace() -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("benchmark workspace root");
        initialize_index_git_workspace(root.path());
        std::fs::write(root.path().join(".gitignore"), "/ignored-root/\n")
            .expect("benchmark ignore rules");
        for index in 0..200 {
            for directory in ["tracked", "untracked"] {
                let path = root
                    .path()
                    .join(directory)
                    .join(format!("entry-{index:03}.txt"));
                std::fs::create_dir_all(path.parent().expect("fixture parent"))
                    .expect("fixture directory");
                std::fs::write(path, "fixture").expect("fixture file");
            }
        }
        for directory in 0..16 {
            for file in 0..100 {
                let path = root
                    .path()
                    .join("ignored-root")
                    .join(format!("group-{directory:02}"))
                    .join(format!("entry-{file:03}.txt"));
                std::fs::create_dir_all(path.parent().expect("ignored fixture parent"))
                    .expect("ignored fixture directory");
                std::fs::write(path, "ignored").expect("ignored fixture file");
            }
        }
        for directory in 0..32 {
            std::fs::create_dir_all(
                root.path()
                    .join("empty")
                    .join(format!("directory-{directory:02}")),
            )
            .expect("empty fixture directory");
        }
        run_index_git(root.path(), &["add", "--", ".gitignore", "tracked"]);
        root
    }

    #[tokio::test]
    async fn watch_handoff_cannot_absorb_a_change_after_the_resync_snapshot() {
        let root = tempfile::tempdir().expect("workspace root");
        std::fs::create_dir_all(root.path().join("nested")).expect("nested directory");
        std::fs::write(root.path().join("nested/tracked.txt"), "tracked").expect("tracked fixture");
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
        let rpc = WorkspaceRpc::new(WorkspaceService::default())
            .with_watch_timing(Duration::from_millis(25), Duration::from_millis(20))
            .with_watch_handoff_gate_for_test(Arc::clone(&gate), entered_tx);
        let cwd = root.path().to_string_lossy().into_owned();

        rpc.handle("projects.listEntries", json!({"cwd": cwd}))
            .await
            .expect("warm snapshot");
        let subscribing_rpc = rpc.clone();
        let subscribing_root = root.path().to_path_buf();
        let subscribing = tokio::spawn(async move {
            subscribing_rpc
                .subscribe_entry_changes(&subscribing_root)
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), entered_rx.recv())
            .await
            .expect("watch handoff entered deadline")
            .expect("watch handoff entered");

        std::fs::write(root.path().join("nested/during-handoff.txt"), "created")
            .expect("handoff mutation");
        gate.add_permits(1);
        let _subscription = subscribing
            .await
            .expect("subscription task")
            .expect("subscription succeeds");

        let entries = rpc
            .handle("projects.listEntries", json!({"cwd": cwd}))
            .await
            .expect("list after subscription");
        assert!(entries["entries"].as_array().is_some_and(|entries| {
            entries
                .iter()
                .any(|entry| entry["path"] == "nested/during-handoff.txt")
        }));
    }

    #[tokio::test]
    async fn subscriber_arriving_after_a_failed_send_keeps_the_watch_alive() {
        let root = tempfile::tempdir().expect("workspace root");
        std::fs::create_dir_all(root.path().join("nested")).expect("nested directory");
        std::fs::write(root.path().join("nested/tracked.txt"), "tracked").expect("tracked fixture");
        let event_gate = Arc::new(tokio::sync::Semaphore::new(0));
        let failure_gate = Arc::new(tokio::sync::Semaphore::new(0));
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (failure_tx, mut failure_rx) = tokio::sync::mpsc::unbounded_channel();
        let rpc = WorkspaceRpc::new(WorkspaceService::default())
            .with_watch_timing(Duration::from_millis(25), Duration::from_millis(20))
            .with_watch_delivery_gates_for_test(
                Arc::clone(&event_gate),
                event_tx,
                Arc::clone(&failure_gate),
                failure_tx,
            );
        let first = rpc
            .subscribe_entry_changes(root.path())
            .await
            .expect("first subscription");

        std::fs::write(root.path().join("nested/first-change.txt"), "first")
            .expect("first outside change");
        tokio::time::timeout(Duration::from_secs(5), event_rx.recv())
            .await
            .expect("watch event entered deadline")
            .expect("watch event entered");
        drop(first);
        event_gate.add_permits(1);
        tokio::time::timeout(Duration::from_secs(5), failure_rx.recv())
            .await
            .expect("send failure entered deadline")
            .expect("send failure entered");

        let mut replacement = rpc
            .subscribe_entry_changes(root.path())
            .await
            .expect("replacement subscription");
        failure_gate.add_permits(1);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(rpc.active_entry_watches().await, 1);

        std::fs::write(root.path().join("nested/second-change.txt"), "second")
            .expect("second outside change");
        tokio::time::timeout(Duration::from_secs(5), replacement.recv())
            .await
            .expect("replacement signal deadline")
            .expect("replacement signal");
    }

    /// Removal waits for a lost workspace's admissions to drain, and a sweep's rebuild takes one
    /// that no subscriber owns. The rebuild has to let go when the workspace is lost instead of
    /// holding removal until its scan finishes.
    #[tokio::test]
    async fn losing_the_workspace_drops_a_sweep_rebuild_holding_its_admission() {
        let root = tempfile::tempdir().expect("workspace root");
        std::fs::create_dir_all(root.path().join("nested")).expect("nested directory");
        std::fs::write(root.path().join("nested/tracked.txt"), "tracked").expect("tracked fixture");
        let physical_root = std::fs::canonicalize(root.path()).expect("canonical workspace root");
        let registry = WorkspaceAvailabilityRegistry::new();
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let (wait_tx, _wait_rx) = tokio::sync::mpsc::unbounded_channel();
        let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
        let rpc = WorkspaceRpc::new(WorkspaceService::default())
            .with_availability_registry(registry.clone())
            .with_watch_timing(Duration::from_millis(25), Duration::from_millis(20))
            .with_index_build_gate_for_test(Arc::clone(&gate), wait_tx, entered_tx);
        let subscribing_rpc = rpc.clone();
        let subscribing_root = root.path().to_path_buf();
        let subscribing = tokio::spawn(async move {
            subscribing_rpc
                .subscribe_entry_changes(&subscribing_root)
                .await
        });
        entered_rx.recv().await.expect("resync build entered");
        gate.add_permits(1);
        let subscription = subscribing
            .await
            .expect("subscription task")
            .expect("subscription");
        // Take the permit back so the sweep's rebuild waits at the gate.
        gate.acquire().await.expect("gate open").forget();
        // Like `project_entries_stream`, release the subscription once the workspace is lost.
        let loss = subscription
            .loss_cancellation()
            .expect("admitted subscription");
        tokio::spawn(async move {
            loss.cancelled().await;
            drop(subscription);
        });

        std::fs::write(root.path().join("nested/added.txt"), "added").expect("outside change");
        tokio::time::timeout(Duration::from_secs(5), entered_rx.recv())
            .await
            .expect("sweep rebuild deadline")
            .expect("sweep rebuild entered");
        let transition = WorkspaceLossTransition {
            thread_id: "workspace-thread".to_owned(),
            repository_key: "repository-key".to_owned(),
            generation: 1,
            path: physical_root,
            availability: AdoptedWorktreeAvailability::MissingRegistered,
        };
        assert!(
            registry
                .mark_unavailable(transition.clone())
                .await
                .expect("physical identity resolves")
        );

        tokio::time::timeout(
            Duration::from_secs(2),
            registry.wait_for_transition_admissions(&transition),
        )
        .await
        .expect("the sweep's rebuild must release its admission once the workspace is lost");
    }

    /// Shutdown waits for every watch task, so a sweep's rebuild queued behind a foreground scan of
    /// the same root must give up its turn rather than hold shutdown until that scan finishes.
    #[tokio::test]
    async fn shutdown_does_not_wait_for_a_sweep_rebuild_queued_behind_another_scan() {
        let root = tempfile::tempdir().expect("workspace root");
        std::fs::create_dir_all(root.path().join("nested")).expect("nested directory");
        std::fs::write(root.path().join("nested/tracked.txt"), "tracked").expect("tracked fixture");
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let (wait_tx, mut wait_rx) = tokio::sync::mpsc::unbounded_channel();
        let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
        let rpc = WorkspaceRpc::new(WorkspaceService::default())
            .with_watch_timing(Duration::from_millis(25), Duration::from_millis(20))
            .with_index_build_gate_for_test(Arc::clone(&gate), wait_tx, entered_tx);
        let subscribing_rpc = rpc.clone();
        let subscribing_root = root.path().to_path_buf();
        let subscribing = tokio::spawn(async move {
            subscribing_rpc
                .subscribe_entry_changes(&subscribing_root)
                .await
        });
        entered_rx.recv().await.expect("resync build entered");
        gate.add_permits(1);
        let _subscription = subscribing
            .await
            .expect("subscription task")
            .expect("subscription");
        gate.acquire().await.expect("gate open").forget();
        wait_rx
            .recv()
            .await
            .expect("resync build waited for its turn");

        // A rescan holds the root's build turn while its scan waits at the gate.
        let listing_rpc = rpc.clone();
        let cwd = root.path().to_string_lossy().into_owned();
        let listing = tokio::spawn(async move {
            listing_rpc
                .handle("projects.listEntries", json!({"cwd": cwd, "refresh": true}))
                .await
        });
        entered_rx.recv().await.expect("rescan build entered");
        wait_rx.recv().await.expect("rescan waited for its turn");

        // At the root, which every sweep stats: the rescan emptied the cache that tells the sweep
        // about `nested`.
        std::fs::write(root.path().join("added.txt"), "added").expect("outside change");
        tokio::time::timeout(Duration::from_secs(5), wait_rx.recv())
            .await
            .expect("sweep rebuild deadline")
            .expect("sweep rebuild queued");

        tokio::time::timeout(Duration::from_secs(2), rpc.shutdown())
            .await
            .expect("shutdown must not wait for another request's scan");
        gate.add_permits(1);
        listing
            .await
            .expect("rescan task")
            .expect("rescan completes");
    }

    /// A sweep lives only while something listens, and that includes a rebuild in progress: once
    /// the last subscriber leaves, the rebuild stops instead of scanning for nobody.
    #[tokio::test]
    async fn a_sweep_rebuild_stops_when_its_last_subscriber_leaves() {
        let root = tempfile::tempdir().expect("workspace root");
        std::fs::create_dir_all(root.path().join("nested")).expect("nested directory");
        std::fs::write(root.path().join("nested/tracked.txt"), "tracked").expect("tracked fixture");
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        let (wait_tx, _wait_rx) = tokio::sync::mpsc::unbounded_channel();
        let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
        let rpc = WorkspaceRpc::new(WorkspaceService::default())
            .with_watch_timing(Duration::from_millis(25), Duration::from_millis(20))
            .with_index_build_gate_for_test(Arc::clone(&gate), wait_tx, entered_tx);
        let subscribing_rpc = rpc.clone();
        let subscribing_root = root.path().to_path_buf();
        let subscribing = tokio::spawn(async move {
            subscribing_rpc
                .subscribe_entry_changes(&subscribing_root)
                .await
        });
        entered_rx.recv().await.expect("resync build entered");
        gate.add_permits(1);
        let subscription = subscribing
            .await
            .expect("subscription task")
            .expect("subscription");
        gate.acquire().await.expect("gate open").forget();

        std::fs::write(root.path().join("nested/added.txt"), "added").expect("outside change");
        tokio::time::timeout(Duration::from_secs(5), entered_rx.recv())
            .await
            .expect("sweep rebuild deadline")
            .expect("sweep rebuild entered");
        drop(subscription);

        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while rpc.active_entry_watches().await > 0 && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(
            rpc.active_entry_watches().await,
            0,
            "the sweep must retire without waiting for its rebuild"
        );
    }

    #[tokio::test]
    async fn workspace_shutdown_rejects_reattachment_and_awaits_every_watch_task() {
        let root = tempfile::tempdir().expect("workspace root");
        std::fs::write(root.path().join("tracked.txt"), "tracked").expect("tracked fixture");
        let rpc = WorkspaceRpc::new(WorkspaceService::default())
            .with_watch_timing(Duration::from_millis(25), Duration::from_millis(20));
        let _subscription = rpc
            .subscribe_entry_changes(root.path())
            .await
            .expect("active subscription");
        assert_eq!(rpc.watch_tasks.len(), 1);

        rpc.shutdown().await;

        assert_eq!(rpc.active_entry_watches().await, 0);
        assert!(rpc.watch_tasks.is_empty());
        assert!(
            rpc.subscribe_entry_changes(root.path()).await.is_err(),
            "shutdown must close subscription admission"
        );
    }

    fn nearest_rank(values: &[u64], percentile: usize) -> u64 {
        let mut sorted = values.to_vec();
        sorted.sort_unstable();
        sorted[(sorted.len() * percentile).div_ceil(100) - 1]
    }

    #[tokio::test]
    async fn index_phase_observer_distinguishes_cold_git_build_and_warm_hit() {
        let root = index_test_workspace();
        let (observer, mut phases) = record_index_phases();
        let rpc =
            WorkspaceRpc::new(WorkspaceService::default()).with_index_phase_sink_for_test(observer);

        rpc.handle(
            "projects.listEntries",
            json!({"cwd": root.path(), "limit": 200}),
        )
        .await
        .expect("cold list");
        let cold_phases = drain_index_phases(&mut phases);
        assert_eq!(
            phase_summary(&cold_phases),
            [
                ("cache_wait", "miss"),
                ("git_snapshot", "build"),
                ("ignored_walk", "build"),
                ("directory_walk", "build"),
                ("cache_build", "built"),
            ]
        );
        assert!(cold_phases.iter().all(|phase| {
            let operation_matches = match phase.phase {
                "cache_wait" | "cache_build" | "cache_hit" => {
                    phase.operation == "WorkspaceSearchIndex.refresh"
                }
                "git_snapshot" | "ignored_walk" | "directory_walk" => {
                    phase.operation == "WorkspaceSearchIndex.gitSnapshot"
                }
                _ => false,
            };
            operation_matches && phase.entry_count <= SearchLimits::default().max_entries
        }));

        rpc.handle(
            "projects.listEntries",
            json!({"cwd": root.path(), "limit": 200}),
        )
        .await
        .expect("warm list");
        assert_eq!(
            phase_summary(&drain_index_phases(&mut phases)),
            [("cache_hit", "hit")]
        );
        assert_eq!(rpc.index_scans(), 1);
    }

    #[tokio::test]
    #[ignore = "native performance evidence; run explicitly"]
    async fn benchmark_file_manager_index_phases() {
        const DEFAULT_SAMPLES: usize = 30;
        const TRACKED_WORKLOAD_FILES: usize = 200;
        const TRACKED_CONTROL_FILES: usize = 1;
        const UNTRACKED_WORKLOAD_FILES: usize = 200;
        const ORDINARY_DIRECTORIES: usize = 2;
        const IGNORED_DIRECTORIES: usize = 17;
        const IGNORED_FILES: usize = 1_600;
        const EMPTY_DIRECTORIES: usize = 33;
        const EXPECTED_ENTRIES: usize = 2_053;
        const EXPECTED_IGNORED_ENTRIES: usize = 1_617;

        assert_eq!(
            TRACKED_WORKLOAD_FILES
                + TRACKED_CONTROL_FILES
                + UNTRACKED_WORKLOAD_FILES
                + ORDINARY_DIRECTORIES
                + IGNORED_DIRECTORIES
                + IGNORED_FILES
                + EMPTY_DIRECTORIES,
            EXPECTED_ENTRIES
        );
        assert_eq!(
            IGNORED_DIRECTORIES + IGNORED_FILES,
            EXPECTED_IGNORED_ENTRIES
        );
        let samples = std::env::var("BIBCODE_FILE_INDEX_BENCHMARK_SAMPLES")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(DEFAULT_SAMPLES);
        assert!(samples > 0);
        let root = index_benchmark_workspace();
        let cwd = root.path().to_string_lossy().into_owned();
        let (observer, mut phases) = record_index_phases();
        let rpc =
            WorkspaceRpc::new(WorkspaceService::default()).with_index_phase_sink_for_test(observer);
        let mut cache_build = Vec::with_capacity(samples);
        let mut git_snapshot = Vec::with_capacity(samples);
        let mut ignored_walk = Vec::with_capacity(samples);
        let mut directory_walk = Vec::with_capacity(samples);
        let mut cache_hit = Vec::with_capacity(samples);

        for _ in 0..samples {
            rpc.refresh_index(root.path()).await;
            let scans_before = rpc.index_scans();
            let result = rpc
                .handle("projects.listEntries", json!({ "cwd": cwd }))
                .await
                .expect("cold benchmark list");
            let entries = result["entries"].as_array().expect("benchmark entries");
            assert_eq!(entries.len(), EXPECTED_ENTRIES);
            assert_eq!(
                entries
                    .iter()
                    .filter(|entry| entry["ignored"] == true)
                    .count(),
                EXPECTED_IGNORED_ENTRIES
            );
            assert_eq!(result["truncated"], false);
            assert_eq!(rpc.index_scans(), scans_before + 1);

            let cold = drain_index_phases(&mut phases);
            assert_eq!(
                phase_summary(&cold),
                [
                    ("cache_wait", "miss"),
                    ("git_snapshot", "build"),
                    ("ignored_walk", "build"),
                    ("directory_walk", "build"),
                    ("cache_build", "built"),
                ]
            );
            for (name, samples) in [
                ("cache_build", &mut cache_build),
                ("git_snapshot", &mut git_snapshot),
                ("ignored_walk", &mut ignored_walk),
                ("directory_walk", &mut directory_walk),
            ] {
                samples.push(
                    cold.iter()
                        .find(|phase| phase.phase == name)
                        .unwrap_or_else(|| panic!("missing {name}"))
                        .elapsed_ms,
                );
            }

            let scans_before_warm = rpc.index_scans();
            rpc.handle("projects.listEntries", json!({ "cwd": cwd }))
                .await
                .expect("warm benchmark list");
            let warm = drain_index_phases(&mut phases);
            assert_eq!(phase_summary(&warm), [("cache_hit", "hit")]);
            assert_eq!(rpc.index_scans(), scans_before_warm);
            cache_hit.push(warm[0].elapsed_ms);
        }

        let summarize = |values: &[u64]| {
            json!({
                "samples_ms": values,
                "p50_ms": nearest_rank(values, 50),
                "p95_ms": nearest_rank(values, 95),
            })
        };
        println!(
            "FILE_INDEX_BENCHMARK {}",
            json!({
                "cold_samples": samples,
                "warm_samples": samples,
                "fixture": {
                    "tracked_workload_files": TRACKED_WORKLOAD_FILES,
                    "tracked_control_files": TRACKED_CONTROL_FILES,
                    "untracked_workload_files": UNTRACKED_WORKLOAD_FILES,
                    "ordinary_directories": ORDINARY_DIRECTORIES,
                    "ignored_directories": IGNORED_DIRECTORIES,
                    "ignored_files": IGNORED_FILES,
                    "empty_directories": EMPTY_DIRECTORIES,
                    "entries": EXPECTED_ENTRIES,
                    "ignored_entries": EXPECTED_IGNORED_ENTRIES,
                },
                "cache_build": summarize(&cache_build),
                "git_snapshot": summarize(&git_snapshot),
                "ignored_walk": summarize(&ignored_walk),
                "directory_walk": summarize(&directory_walk),
                "cache_hit": summarize(&cache_hit),
                "filesystem_walk": null,
            })
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn index_phase_observer_reports_one_physical_build_and_a_shared_waiter() {
        let root = index_test_workspace();
        let (observer, mut phases) = record_index_phases();
        let (build_wait_started, mut build_waits) = tokio::sync::mpsc::unbounded_channel();
        let (build_entered, mut build_entries) = tokio::sync::mpsc::unbounded_channel();
        let build_gate = Arc::new(tokio::sync::Semaphore::new(1));
        let retained = build_gate
            .clone()
            .acquire_owned()
            .await
            .expect("retain build gate");
        let rpc = WorkspaceRpc::new(WorkspaceService::default())
            .with_index_phase_sink_for_test(observer)
            .with_index_build_gate_for_test(build_gate, build_wait_started, build_entered);
        let cwd = root.path().to_string_lossy().into_owned();
        let leader_rpc = rpc.clone();
        let leader_cwd = cwd.clone();
        let leader = tokio::spawn(async move {
            leader_rpc
                .handle("projects.listEntries", json!({"cwd": leader_cwd}))
                .await
        });
        build_waits.recv().await.expect("owner reaches build wait");
        build_entries
            .recv()
            .await
            .expect("owner reaches build gate");

        let waiter_rpc = rpc.clone();
        let waiter = tokio::spawn(async move {
            waiter_rpc
                .handle("projects.listEntries", json!({"cwd": cwd}))
                .await
        });
        build_waits.recv().await.expect("waiter reaches build wait");
        drop(retained);
        leader.await.unwrap().unwrap();
        waiter.await.unwrap().unwrap();

        let summary = phase_summary(&drain_index_phases(&mut phases));
        assert_eq!(
            summary
                .iter()
                .filter(|(phase, _)| *phase == "cache_build")
                .count(),
            1
        );
        assert_eq!(
            summary
                .iter()
                .filter(|(phase, _)| *phase == "git_snapshot")
                .count(),
            1
        );
        assert!(summary.contains(&("cache_wait", "miss")));
        assert!(summary.contains(&("cache_wait", "shared")));
        assert_eq!(rpc.index_scans(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn index_phase_cache_hit_timer_starts_before_the_cache_mutex_lookup() {
        let root = index_test_workspace();
        let rpc = WorkspaceRpc::new(WorkspaceService::default());
        rpc.handle(
            "projects.listEntries",
            json!({"cwd": root.path(), "limit": 200}),
        )
        .await
        .expect("warm cache");
        let (observer, mut phases) = record_index_phases();
        let (timer_started, mut timer_starts) = tokio::sync::mpsc::unbounded_channel();
        let rpc = rpc
            .with_index_phase_sink_for_test(observer)
            .with_index_cache_timer_started_for_test(timer_started);
        let cache_lock = rpc.indexes.lock().await;
        let request_rpc = rpc.clone();
        let cwd = root.path().to_path_buf();
        let request = tokio::spawn(async move {
            request_rpc
                .handle("projects.listEntries", json!({"cwd": cwd}))
                .await
        });

        timer_starts
            .recv()
            .await
            .expect("cache timer starts while the cache is locked");
        assert!(!request.is_finished());
        drop(cache_lock);
        request.await.unwrap().unwrap();
        assert_eq!(
            phase_summary(&drain_index_phases(&mut phases)),
            [("cache_hit", "hit")]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn index_phase_cache_build_error_is_terminal_and_never_reports_built() {
        let root = index_test_workspace();
        let (observer, mut phases) = record_index_phases();
        let (build_wait_started, _build_waits) = tokio::sync::mpsc::unbounded_channel();
        let (build_entered, mut build_entries) = tokio::sync::mpsc::unbounded_channel();
        let build_gate = Arc::new(tokio::sync::Semaphore::new(1));
        let retained = build_gate
            .clone()
            .acquire_owned()
            .await
            .expect("retain build gate");
        let rpc = WorkspaceRpc::new(WorkspaceService::default())
            .with_index_phase_sink_for_test(observer)
            .with_index_build_gate_for_test(build_gate, build_wait_started, build_entered);
        let request_rpc = rpc.clone();
        let cwd = root.path().to_path_buf();
        let request = tokio::spawn(async move {
            request_rpc
                .handle("projects.listEntries", json!({"cwd": cwd}))
                .await
        });
        build_entries.recv().await.expect("build reaches gate");
        std::fs::remove_dir_all(root.path()).expect("remove root before physical build");
        drop(retained);

        assert!(request.await.unwrap().is_err());
        let summary = phase_summary(&drain_index_phases(&mut phases));
        assert!(summary.contains(&("cache_build", "error")));
        assert!(!summary.contains(&("cache_build", "built")));
        assert!(!summary.contains(&("cache_build", "cancelled")));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn index_phase_stale_cache_build_is_terminal_and_never_reports_built() {
        let root = index_test_workspace();
        let (observer, mut phases) = record_index_phases();
        let (build_wait_started, _build_waits) = tokio::sync::mpsc::unbounded_channel();
        let (build_entered, mut build_entries) = tokio::sync::mpsc::unbounded_channel();
        let build_gate = Arc::new(tokio::sync::Semaphore::new(1));
        let retained = build_gate
            .clone()
            .acquire_owned()
            .await
            .expect("retain build gate");
        let rpc = WorkspaceRpc::new(WorkspaceService::default())
            .with_index_phase_sink_for_test(observer)
            .with_index_build_gate_for_test(build_gate, build_wait_started, build_entered);
        let request_rpc = rpc.clone();
        let cwd = root.path().to_path_buf();
        let request = tokio::spawn(async move {
            request_rpc
                .handle("projects.listEntries", json!({"cwd": cwd}))
                .await
        });
        build_entries.recv().await.expect("build reaches gate");
        rpc.refresh_index(root.path()).await;
        drop(retained);

        request.await.unwrap().unwrap();
        let summary = phase_summary(&drain_index_phases(&mut phases));
        assert!(summary.contains(&("cache_build", "stale")));
        assert!(!summary.contains(&("cache_build", "built")));
    }

    struct ImmediateStatusGitRunner {
        local_started: tokio::sync::mpsc::UnboundedSender<tokio::time::Instant>,
    }

    impl GitProcessRunner for ImmediateStatusGitRunner {
        fn run<'a>(
            &'a self,
            request: ProcessRequest,
            _cancellation: &'a CancellationToken,
        ) -> BoxGitProcessFuture<'a> {
            let dirty = std::fs::read_to_string(request.cwd.join("tracked.txt"))
                .is_ok_and(|contents| contents != "base\n");
            let (exit_code, stdout) = match request.operation.as_str() {
                "GitVcsDriver.statusDetailsLocal.status" => {
                    self.local_started
                        .send(tokio::time::Instant::now())
                        .expect("local status start receiver remains open");
                    let dirty_record = dirty.then_some(
                        "1 .M N... 100644 100644 100644 deadbeef deadbeef tracked.txt\n",
                    );
                    (
                        0,
                        format!("# branch.head main\n{}", dirty_record.unwrap_or_default()),
                    )
                }
                "GitVcsDriver.statusDetailsLocal.unstagedNumstat" => {
                    let stdout = if dirty { "1\t1\ttracked.txt\n" } else { "" };
                    (0, stdout.to_owned())
                }
                "GitVcsDriver.currentRef" => (0, "main\n".to_owned()),
                "GitVcsDriver.statusDetailsRemote.status" => (0, "# branch.head main\n".to_owned()),
                "GitVcsDriver.defaultRef.candidate" => {
                    let is_main = request
                        .args
                        .last()
                        .is_some_and(|value| value == "refs/heads/main");
                    (i32::from(!is_main), String::new())
                }
                "GitVcsDriver.defaultRef.originHead" | "GitVcsDriver.remoteProvider" => {
                    (1, String::new())
                }
                _ => (0, String::new()),
            };
            Box::pin(async move {
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

    struct StatusMutationObserver {
        broadcaster: StatusBroadcaster,
        invalidated: tokio::sync::mpsc::UnboundedSender<tokio::time::Instant>,
    }

    impl WorkspaceMutationObserver for StatusMutationObserver {
        fn begin_workspace_mutation<'a>(&'a self, cwd: &'a Path) -> WorkspaceMutationFuture<'a> {
            Box::pin(async move {
                let mutation = self.broadcaster.begin_mutation(cwd).await;
                let invalidated_at = tokio::time::Instant::now();
                self.invalidated
                    .send(invalidated_at)
                    .expect("invalidation receiver remains open");
                Some(mutation)
            })
        }
    }

    struct PausingMutationObserver {
        entered: Arc<tokio::sync::Semaphore>,
        release: Arc<tokio::sync::Semaphore>,
    }

    struct PausingStatusMutationObserver {
        broadcaster: StatusBroadcaster,
        entered: Arc<tokio::sync::Semaphore>,
        release: Arc<tokio::sync::Semaphore>,
    }

    struct PausingFailingWriteHook {
        entered: Arc<tokio::sync::Semaphore>,
        release: Arc<tokio::sync::Semaphore>,
        completed: Arc<tokio::sync::Semaphore>,
    }

    impl WorkspaceWriteHook for PausingFailingWriteHook {
        fn after_write<'a>(&'a self, _target: &'a Path) -> WorkspaceWriteHookFuture<'a> {
            let entered = self.entered.clone();
            let release = self.release.clone();
            let completed = self.completed.clone();
            Box::pin(async move {
                entered.add_permits(1);
                release
                    .acquire()
                    .await
                    .expect("write hook release")
                    .forget();
                completed.add_permits(1);
                Err(WorkspaceError::Cancelled)
            })
        }
    }

    struct FailingWriteHook;

    impl WorkspaceWriteHook for FailingWriteHook {
        fn after_write<'a>(&'a self, _target: &'a Path) -> WorkspaceWriteHookFuture<'a> {
            Box::pin(async { Err(WorkspaceError::Cancelled) })
        }
    }

    struct PanickingWriteHook;

    impl WorkspaceWriteHook for PanickingWriteHook {
        fn after_write<'a>(&'a self, _target: &'a Path) -> WorkspaceWriteHookFuture<'a> {
            Box::pin(async { panic!("post-write hook panic") })
        }
    }

    impl WorkspaceMutationObserver for PausingMutationObserver {
        fn begin_workspace_mutation<'a>(&'a self, _cwd: &'a Path) -> WorkspaceMutationFuture<'a> {
            let entered = self.entered.clone();
            let release = self.release.clone();
            Box::pin(async move {
                entered.add_permits(1);
                release
                    .acquire()
                    .await
                    .expect("mutation observer release")
                    .forget();
                None
            })
        }
    }

    impl WorkspaceMutationObserver for PausingStatusMutationObserver {
        fn begin_workspace_mutation<'a>(&'a self, cwd: &'a Path) -> WorkspaceMutationFuture<'a> {
            let entered = self.entered.clone();
            let release = self.release.clone();
            Box::pin(async move {
                entered.add_permits(1);
                release
                    .acquire()
                    .await
                    .expect("status mutation observer release")
                    .forget();
                Some(self.broadcaster.begin_mutation(cwd).await)
            })
        }
    }

    #[tokio::test]
    async fn project_write_starts_and_publishes_local_status_within_750_ms() {
        let root = tempfile::tempdir().expect("workspace root");
        std::fs::write(root.path().join("tracked.txt"), "base\n").expect("clean tracked file");
        let (local_started, mut local_starts) = tokio::sync::mpsc::unbounded_channel();
        let repository = Arc::new(GitRepository::with_runner_for_test(Arc::new(
            ImmediateStatusGitRunner { local_started },
        )));
        let broadcaster = StatusBroadcaster::new(repository, Duration::from_secs(3_600), 4);
        let mut subscription = broadcaster
            .subscribe(root.path().to_path_buf(), CancellationToken::new())
            .await
            .expect("status subscription starts");
        assert!(matches!(
            subscription.recv().await,
            Some(VcsStatusStreamEvent::Snapshot { ref local, .. })
                if !local.has_working_tree_changes
        ));
        local_starts
            .recv()
            .await
            .expect("initial local status start");
        let (invalidated, mut invalidations) = tokio::sync::mpsc::unbounded_channel();
        let observer = Arc::new(StatusMutationObserver {
            broadcaster,
            invalidated,
        });
        let rpc = WorkspaceRpc::with_dependencies(
            WorkspaceService::default(),
            WorkspaceRpcDependencies {
                mutation_observer: Some(observer),
                ..WorkspaceRpcDependencies::default()
            },
        );
        let save_started = tokio::time::Instant::now();

        let (save_settled_at, (invalidated_at, local_started_at, published_at)) =
            tokio::time::timeout(PROJECT_SAVE_STATUS_DEADLINE, async {
                let save = async {
                    rpc.handle(
                        "projects.writeFile",
                        json!({
                            "cwd": root.path(),
                            "relativePath": "tracked.txt",
                            "contents": "changed in editor\n",
                        }),
                    )
                    .await
                    .expect("project save succeeds");
                    tokio::time::Instant::now()
                };
                let publication = async {
                    let invalidated_at = invalidations.recv().await.expect("save invalidation");
                    let local_started_at = local_starts.recv().await.expect("local status start");
                    loop {
                        let event = subscription
                            .recv()
                            .await
                            .expect("status subscription remains open");
                        if matches!(
                            event,
                            VcsStatusStreamEvent::LocalUpdated { ref local }
                                if local.has_working_tree_changes
                        ) {
                            break (
                                invalidated_at,
                                local_started_at,
                                tokio::time::Instant::now(),
                            );
                        }
                    }
                };
                tokio::join!(save, publication)
            })
            .await
            .expect("save settlement and local status publication complete within 750 ms");

        assert!(save_settled_at.duration_since(save_started) <= PROJECT_SAVE_STATUS_DEADLINE);
        assert!(invalidated_at >= save_started);
        assert!(local_started_at.duration_since(invalidated_at) <= PROJECT_SAVE_STATUS_DEADLINE);
        assert!(published_at.duration_since(invalidated_at) <= PROJECT_SAVE_STATUS_DEADLINE);
    }

    #[tokio::test]
    async fn project_entry_mutations_settle_one_watcher_fallback_refresh_on_success_and_error() {
        let root = tempfile::tempdir().expect("workspace root");
        let broadcaster = StatusBroadcaster::new(
            Arc::new(GitRepository::default()),
            Duration::from_secs(3_600),
            4,
        );
        let (invalidated, _invalidations) = tokio::sync::mpsc::unbounded_channel();
        let rpc = WorkspaceRpc::with_dependencies(
            WorkspaceService::default(),
            WorkspaceRpcDependencies {
                mutation_observer: Some(Arc::new(StatusMutationObserver {
                    broadcaster: broadcaster.clone(),
                    invalidated,
                })),
                ..WorkspaceRpcDependencies::default()
            },
        );

        let successful = [
            (
                "projects.createEntry",
                json!({"cwd":root.path(),"relativePath":"created.txt","kind":"file"}),
            ),
            (
                "projects.duplicateEntry",
                json!({"cwd":root.path(),"relativePath":"created.txt"}),
            ),
            (
                "projects.renameEntry",
                json!({
                    "cwd":root.path(),
                    "fromRelativePath":"created.txt",
                    "toRelativePath":"renamed.txt"
                }),
            ),
            (
                "projects.deleteEntry",
                json!({"cwd":root.path(),"relativePath":"renamed.txt"}),
            ),
        ];
        for (index, (method, payload)) in successful.into_iter().enumerate() {
            rpc.handle(method, payload)
                .await
                .unwrap_or_else(|error| panic!("{method} succeeds: {error}"));
            assert_eq!(
                broadcaster
                    .local_refresh_generation_for_test(root.path())
                    .await,
                u64::try_from(index + 1).expect("small generation"),
                "{method} settles exactly one immediate refresh"
            );
        }

        let failing = [
            (
                "projects.createEntry",
                json!({"cwd":root.path(),"relativePath":"created copy.txt","kind":"file"}),
            ),
            (
                "projects.renameEntry",
                json!({
                    "cwd":root.path(),
                    "fromRelativePath":"missing.txt",
                    "toRelativePath":"still-missing.txt"
                }),
            ),
            (
                "projects.deleteEntry",
                json!({"cwd":root.path(),"relativePath":"missing.txt"}),
            ),
            (
                "projects.duplicateEntry",
                json!({"cwd":root.path(),"relativePath":"missing.txt"}),
            ),
        ];
        for (index, (method, payload)) in failing.into_iter().enumerate() {
            rpc.handle(method, payload)
                .await
                .expect_err("filesystem error is preserved");
            assert_eq!(
                broadcaster
                    .local_refresh_generation_for_test(root.path())
                    .await,
                u64::try_from(4 + index + 1).expect("small generation"),
                "{method} error settles exactly one immediate refresh"
            );
        }
    }

    #[tokio::test]
    async fn project_entry_mutations_outlive_a_dropped_caller_and_settle_once() {
        let root = tempfile::tempdir().expect("workspace root");
        std::fs::write(root.path().join("source.txt"), "source").expect("duplicate source");
        let broadcaster = StatusBroadcaster::new(
            Arc::new(GitRepository::default()),
            Duration::from_secs(3_600),
            4,
        );
        let observer = Arc::new(PausingStatusMutationObserver {
            broadcaster: broadcaster.clone(),
            entered: Arc::new(tokio::sync::Semaphore::new(0)),
            release: Arc::new(tokio::sync::Semaphore::new(0)),
        });
        let rpc = WorkspaceRpc::with_dependencies(
            WorkspaceService::default(),
            WorkspaceRpcDependencies {
                mutation_observer: Some(observer.clone()),
                ..WorkspaceRpcDependencies::default()
            },
        );
        let operations = [
            (
                "projects.createEntry",
                json!({"cwd":root.path(),"relativePath":"created.txt","kind":"file"}),
            ),
            (
                "projects.duplicateEntry",
                json!({"cwd":root.path(),"relativePath":"source.txt"}),
            ),
            (
                "projects.renameEntry",
                json!({
                    "cwd":root.path(),
                    "fromRelativePath":"created.txt",
                    "toRelativePath":"renamed.txt"
                }),
            ),
            (
                "projects.deleteEntry",
                json!({"cwd":root.path(),"relativePath":"renamed.txt"}),
            ),
        ];

        for (index, (method, payload)) in operations.into_iter().enumerate() {
            let owned_rpc = rpc.clone();
            let response = tokio::spawn(async move { owned_rpc.handle(method, payload).await });
            observer
                .entered
                .acquire()
                .await
                .expect("operation reaches status observer")
                .forget();
            response.abort();
            let _ = response.await;
            observer.release.add_permits(1);
            let baseline = u64::try_from(index).expect("small generation");
            tokio::time::timeout(Duration::from_secs(5), async {
                while broadcaster
                    .local_refresh_generation_for_test(root.path())
                    .await
                    <= baseline
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap_or_else(|_| panic!("{method} detached settlement deadline"));
            assert_eq!(
                broadcaster
                    .local_refresh_generation_for_test(root.path())
                    .await,
                u64::try_from(index + 1).expect("small generation"),
                "{method} dropped caller still settles exactly once"
            );
        }

        assert!(root.path().join("source copy.txt").is_file());
        assert!(!root.path().join("created.txt").exists());
        assert!(!root.path().join("renamed.txt").exists());
    }

    #[tokio::test]
    async fn admitted_write_outlives_a_dropped_response_and_finalizes_after_terminal_error() {
        let root = tempfile::tempdir().expect("workspace root");
        std::fs::write(root.path().join("tracked.txt"), "base\n").expect("tracked fixture");
        let (local_started, _local_starts) = tokio::sync::mpsc::unbounded_channel();
        let broadcaster = StatusBroadcaster::new(
            Arc::new(GitRepository::with_runner_for_test(Arc::new(
                ImmediateStatusGitRunner { local_started },
            ))),
            Duration::from_secs(3_600),
            4,
        );
        let (invalidated, _invalidations) = tokio::sync::mpsc::unbounded_channel();
        let observer = Arc::new(StatusMutationObserver {
            broadcaster: broadcaster.clone(),
            invalidated,
        });
        let hook = Arc::new(PausingFailingWriteHook {
            entered: Arc::new(tokio::sync::Semaphore::new(0)),
            release: Arc::new(tokio::sync::Semaphore::new(0)),
            completed: Arc::new(tokio::sync::Semaphore::new(0)),
        });
        let rpc = WorkspaceRpc::with_dependencies(
            WorkspaceService::default().with_write_hook_for_test(hook.clone()),
            WorkspaceRpcDependencies {
                mutation_observer: Some(observer),
                ..WorkspaceRpcDependencies::default()
            },
        );
        rpc.handle(
            "projects.listEntries",
            json!({"cwd":root.path(),"limit":200}),
        )
        .await
        .expect("initial index");
        assert_eq!(rpc.index_scans.load(Ordering::Relaxed), 1);
        let writing_rpc = rpc.clone();
        let cwd = root.path().to_path_buf();
        let cancellation = CancellationToken::new();
        let write_cancellation = cancellation.clone();
        let response_waiter = tokio::spawn(async move {
            writing_rpc
                .handle_with_cancellation(
                    "projects.writeFile",
                    json!({
                        "cwd":cwd,
                        "relativePath":"created/nested.txt",
                        "contents":"written before terminal error"
                    }),
                    write_cancellation,
                )
                .await
        });
        hook.entered
            .acquire()
            .await
            .expect("write reaches terminal hook")
            .forget();

        cancellation.cancel();
        response_waiter.abort();
        let _ = response_waiter.await;
        tokio::task::yield_now().await;
        let before_release = broadcaster
            .local_refresh_generation_for_test(root.path())
            .await;
        hook.release.add_permits(1);
        let completed = tokio::time::timeout(Duration::from_millis(200), hook.completed.acquire())
            .await
            .is_ok();
        tokio::time::timeout(Duration::from_secs(5), async {
            while broadcaster
                .local_refresh_generation_for_test(root.path())
                .await
                == 0
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached terminal write settlement deadline");
        let entries = rpc
            .handle(
                "projects.listEntries",
                json!({"cwd":root.path(),"limit":200}),
            )
            .await
            .expect("post-error index rebuild");

        assert_eq!(before_release, 0);
        assert!(completed);
        assert_eq!(
            broadcaster
                .local_refresh_generation_for_test(root.path())
                .await,
            1
        );
        assert_eq!(rpc.index_scans.load(Ordering::Relaxed), 2);
        assert!(entries["entries"].as_array().is_some_and(|entries| {
            entries
                .iter()
                .any(|entry| entry["path"] == "created/nested.txt")
        }));
    }

    #[tokio::test]
    async fn terminal_write_error_preserves_error_after_index_invalidation_and_status_settlement() {
        let root = tempfile::tempdir().expect("workspace root");
        let broadcaster = StatusBroadcaster::new(
            Arc::new(GitRepository::default()),
            Duration::from_secs(3_600),
            4,
        );
        let (invalidated, _invalidations) = tokio::sync::mpsc::unbounded_channel();
        let rpc = WorkspaceRpc::with_dependencies(
            WorkspaceService::default().with_write_hook_for_test(Arc::new(FailingWriteHook)),
            WorkspaceRpcDependencies {
                mutation_observer: Some(Arc::new(StatusMutationObserver {
                    broadcaster: broadcaster.clone(),
                    invalidated,
                })),
                ..WorkspaceRpcDependencies::default()
            },
        );
        rpc.handle(
            "projects.listEntries",
            json!({"cwd":root.path(),"limit":200}),
        )
        .await
        .expect("initial index");
        assert_eq!(rpc.index_scans(), 1);

        let error = rpc
            .handle(
                "projects.writeFile",
                json!({
                    "cwd":root.path(),
                    "relativePath":"partial.txt",
                    "contents":"written before error"
                }),
            )
            .await
            .expect_err("post-write hook returns the original error");
        let entries = rpc
            .handle(
                "projects.listEntries",
                json!({"cwd":root.path(),"limit":200}),
            )
            .await
            .expect("invalidated index rebuilds");

        assert_eq!(
            error,
            json!({
                "_tag": "ProjectWriteFileError",
                "cwd": root.path(),
                "relativePath": "partial.txt",
                "failure": "operation_failed",
                "message": "workspace operation was cancelled",
            })
        );
        assert_eq!(rpc.index_scans(), 2);
        assert_eq!(
            broadcaster
                .local_refresh_generation_for_test(root.path())
                .await,
            1
        );
        assert!(
            entries["entries"]
                .as_array()
                .is_some_and(|entries| entries.iter().any(|entry| entry["path"] == "partial.txt"))
        );
    }

    #[tokio::test]
    async fn post_write_panic_invalidates_index_settles_guard_and_propagates() {
        let root = tempfile::tempdir().expect("workspace root");
        let broadcaster = StatusBroadcaster::new(
            Arc::new(GitRepository::default()),
            Duration::from_secs(3_600),
            4,
        );
        let (invalidated, _invalidations) = tokio::sync::mpsc::unbounded_channel();
        let rpc = WorkspaceRpc::with_dependencies(
            WorkspaceService::default().with_write_hook_for_test(Arc::new(PanickingWriteHook)),
            WorkspaceRpcDependencies {
                mutation_observer: Some(Arc::new(StatusMutationObserver {
                    broadcaster: broadcaster.clone(),
                    invalidated,
                })),
                ..WorkspaceRpcDependencies::default()
            },
        );
        rpc.handle(
            "projects.listEntries",
            json!({"cwd":root.path(),"limit":200}),
        )
        .await
        .expect("initial index");
        assert_eq!(rpc.index_scans(), 1);

        let panic = AssertUnwindSafe(rpc.handle(
            "projects.writeFile",
            json!({
                "cwd": root.path(),
                "relativePath": "created-before-panic.txt",
                "contents": "written before panic"
            }),
        ))
        .catch_unwind()
        .await;
        let entries = rpc
            .handle(
                "projects.listEntries",
                json!({"cwd":root.path(),"limit":200}),
            )
            .await
            .expect("post-panic index rebuild");

        assert!(panic.is_err());
        assert_eq!(
            std::fs::read_to_string(root.path().join("created-before-panic.txt"))
                .expect("partial write remains"),
            "written before panic"
        );
        assert_eq!(rpc.index_scans(), 2);
        assert_eq!(
            broadcaster
                .local_refresh_generation_for_test(root.path())
                .await,
            1
        );
        assert!(entries["entries"].as_array().is_some_and(|entries| {
            entries
                .iter()
                .any(|entry| entry["path"] == "created-before-panic.txt")
        }));
    }

    #[tokio::test]
    async fn owned_workspace_task_panic_settles_the_guard_and_reaches_the_outer_unwind_boundary() {
        let root = tempfile::tempdir().expect("workspace root");
        let broadcaster = StatusBroadcaster::new(
            Arc::new(GitRepository::default()),
            Duration::from_secs(3_600),
            4,
        );
        let owned_broadcaster = broadcaster.clone();
        let cwd = root.path().to_path_buf();

        let panic = AssertUnwindSafe(await_server_owned_workspace(async move {
            let _mutation = owned_broadcaster.begin_mutation(&cwd).await;
            panic!("owned workspace mutation panic");
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn paused_workspace_write_retains_admission_until_the_rpc_finishes() {
        let root = tempfile::tempdir().expect("workspace root");
        let physical_root = std::fs::canonicalize(root.path()).expect("canonical workspace root");
        let registry = WorkspaceAvailabilityRegistry::new();
        let observer = Arc::new(PausingMutationObserver {
            entered: Arc::new(tokio::sync::Semaphore::new(0)),
            release: Arc::new(tokio::sync::Semaphore::new(0)),
        });
        let rpc = WorkspaceRpc::with_dependencies(
            WorkspaceService::default(),
            WorkspaceRpcDependencies {
                mutation_observer: Some(observer.clone()),
                ..WorkspaceRpcDependencies::default()
            },
        )
        .with_availability_registry(registry.clone());
        let writing = tokio::spawn(async move {
            rpc.handle(
                "projects.writeFile",
                json!({
                    "cwd":physical_root,
                    "relativePath":"paused.txt",
                    "contents":"complete before removal"
                }),
            )
            .await
        });
        observer
            .entered
            .acquire()
            .await
            .expect("write reaches mutation observer")
            .forget();

        let removal_registry = registry.clone();
        let removal_path = root.path().to_path_buf();
        let mut removal = tokio::spawn(async move {
            removal_registry
                .mark_removing("workspace-thread", &removal_path)
                .await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut removal)
                .await
                .is_err(),
            "Removing cannot finalize while the admitted write RPC is paused"
        );
        assert!(!writing.is_finished());
        observer.release.add_permits(1);
        writing
            .await
            .expect("write task joins")
            .expect("write RPC succeeds");
        drop(removal.await.expect("removal task joins"));
        assert_eq!(
            std::fs::read_to_string(root.path().join("paused.txt")).expect("written file"),
            "complete before removal"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn workspace_rpc_admissions_span_operations_and_mutation_finalization() {
        let root = tempfile::tempdir().expect("workspace root");
        let registry = WorkspaceAvailabilityRegistry::new();
        let rpc = WorkspaceRpc::new(WorkspaceService::default())
            .with_availability_registry(registry.clone());
        let physical_root = std::fs::canonicalize(root.path()).expect("canonical workspace root");
        let admission = rpc
            .acquire_path(&root.path().to_string_lossy())
            .await
            .expect("path admitted")
            .expect("configured registry returns a lease");
        let transition = WorkspaceLossTransition {
            thread_id: "workspace-thread".to_owned(),
            repository_key: "repository-key".to_owned(),
            generation: 1,
            path: physical_root.clone(),
            availability: AdoptedWorktreeAvailability::MissingRegistered,
        };
        let loss_registry = registry.clone();
        let loss_transition = transition.clone();
        let loss_cancellation = admission.loss_cancellation();
        let loss =
            tokio::spawn(async move { loss_registry.mark_unavailable(loss_transition).await });
        assert!(
            loss.await
                .expect("loss task joins")
                .expect("physical identity resolves")
        );
        assert!(loss_cancellation.is_cancelled());
        drop(admission);
        registry
            .clear_recovered("workspace-thread", &physical_root)
            .await
            .expect("physical identity resolves");

        let admission = rpc
            .acquire_path(&root.path().to_string_lossy())
            .await
            .expect("path admitted before the next loss")
            .expect("configured registry returns a lease");
        let finalization = WorkspaceRpc::begin_mutation(Some(&admission))
            .expect("mutation finalization begins")
            .expect("configured registry returns a finalization permit");
        let loss_registry = registry.clone();
        let removal_path = physical_root.clone();
        let mut loss = tokio::spawn(async move {
            loss_registry
                .mark_removing("workspace-thread", &removal_path)
                .await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut loss)
                .await
                .is_err(),
            "workspace loss waits until the filesystem mutation's final commit window ends"
        );
        drop(finalization);
        drop(admission);
        drop(loss.await.expect("final loss task joins"));
    }

    #[tokio::test]
    async fn rpc_error_mappers_cover_workspace_filesystem_and_asset_variants() {
        let missing_root = std::env::temp_dir().join("bibcode-missing-workspace-rpc-root");
        let missing_cwd = missing_root.to_string_lossy().into_owned();
        let rpc = WorkspaceRpc::new(WorkspaceService::default());
        for (method, payload, tag) in [
            (
                "projects.readFile",
                json!({"cwd":missing_cwd,"relativePath":"missing.txt"}),
                "ProjectReadFileError",
            ),
            (
                "projects.writeFile",
                json!({"cwd":missing_cwd,"relativePath":"missing.txt","contents":"x"}),
                "ProjectWriteFileError",
            ),
            (
                "projects.createEntry",
                json!({"cwd":missing_cwd,"relativePath":"missing.txt","kind":"file"}),
                "ProjectCreateEntryError",
            ),
            (
                "projects.renameEntry",
                json!({"cwd":missing_cwd,"fromRelativePath":"from.txt","toRelativePath":"to.txt"}),
                "ProjectRenameEntryError",
            ),
            (
                "projects.deleteEntry",
                json!({"cwd":missing_cwd,"relativePath":"missing.txt"}),
                "ProjectDeleteEntryError",
            ),
            (
                "projects.duplicateEntry",
                json!({"cwd":missing_cwd,"relativePath":"missing.txt"}),
                "ProjectDuplicateEntryError",
            ),
            (
                "projects.listEntries",
                json!({"cwd":missing_cwd}),
                "ProjectListEntriesError",
            ),
            (
                "projects.searchEntries",
                json!({"cwd":missing_cwd,"query":"x","limit":10}),
                "ProjectSearchEntriesError",
            ),
        ] {
            let error = rpc.handle(method, payload).await.unwrap_err();
            assert_eq!(error["_tag"], tag);
        }
        rpc.refresh_index(&missing_root).await;
        rpc.invalidate_index(&missing_cwd).await;

        let root_error = WorkspaceError::RootNotFound {
            path: missing_root.clone(),
        };
        assert_eq!(
            entries_wire_error("Entries", &missing_cwd, &root_error)["failure"],
            "workspace_root_not_found"
        );
        assert_eq!(
            entries_wire_error(
                "Entries",
                &missing_cwd,
                &WorkspaceError::RootNotDirectory {
                    path: missing_root.clone(),
                },
            )["failure"],
            "workspace_root_not_directory"
        );
        assert_eq!(
            entries_wire_error("Entries", &missing_cwd, &WorkspaceError::Cancelled)["failure"],
            "search_index_scan_timed_out"
        );
        assert_eq!(
            entries_wire_error(
                "Entries",
                &missing_cwd,
                &WorkspaceError::InvalidRequest("bad".to_owned()),
            )["failure"],
            "search_index_search_failed"
        );

        let browse = BrowseInput {
            partial_path: "relative".to_owned(),
            cwd: None,
            mode: None,
        };
        assert_eq!(
            filesystem_wire_error(
                &browse,
                &WorkspaceError::WindowsPathUnsupported {
                    partial_path: "C:\\temp".to_owned(),
                },
            )["failure"],
            "windows_path_unsupported"
        );
        assert_eq!(
            filesystem_wire_error(
                &browse,
                &WorkspaceError::CurrentProjectRequired {
                    partial_path: "relative".to_owned(),
                },
            )["failure"],
            "current_project_required"
        );
        assert_eq!(
            filesystem_wire_error(&browse, &WorkspaceError::Cancelled)["failure"],
            "read_directory_failed"
        );

        for tag in [
            "AssetWorkspaceContextNotFoundError",
            "AssetWorkspaceContextResolutionError",
            "AssetWorkspaceRootNormalizationError",
            "AssetWorkspacePathValidationError",
            "AssetPreviewTypeValidationError",
            "AssetWorkspaceAssetInspectionError",
            "AssetWorkspaceAssetNotFoundError",
            "AssetWorkspaceResolutionError",
            "AssetAttachmentNotFoundError",
            "AssetProjectFaviconResolutionError",
            "AssetProjectFaviconInspectionError",
            "AssetProjectFaviconNotFoundError",
            "AssetSigningKeyLoadError",
            "UnknownAssetError",
        ] {
            assert!(!asset_message(tag).is_empty());
        }

        let workspace = AssetResource::WorkspaceFile {
            thread_id: "thread-1".to_owned(),
            path: "preview.html".to_owned(),
        };
        let attachment = AssetResource::Attachment {
            attachment_id: "attachment-1".to_owned(),
        };
        let favicon = AssetResource::ProjectFavicon {
            cwd: missing_cwd.clone(),
        };
        let operation_error = || {
            WorkspaceError::operation(
                "stat",
                missing_root.clone(),
                std::io::Error::other("failed"),
            )
        };
        for (resource, error, tag) in [
            (
                &workspace,
                AssetError::WorkspaceContextRequired,
                "AssetWorkspaceContextNotFoundError",
            ),
            (
                &workspace,
                AssetError::UnsupportedPreviewType("txt".to_owned()),
                "AssetPreviewTypeValidationError",
            ),
            (
                &workspace,
                AssetError::NotFound("missing".to_owned()),
                "AssetWorkspaceAssetNotFoundError",
            ),
            (
                &attachment,
                AssetError::NotFound("missing".to_owned()),
                "AssetAttachmentNotFoundError",
            ),
            (
                &favicon,
                AssetError::NotFound("missing".to_owned()),
                "AssetProjectFaviconNotFoundError",
            ),
            (
                &workspace,
                AssetError::Workspace(WorkspaceError::RootNotFound {
                    path: missing_root.clone(),
                }),
                "AssetWorkspaceRootNormalizationError",
            ),
            (
                &workspace,
                AssetError::Workspace(WorkspaceError::PathOutsideRoot {
                    relative_path: "../outside".to_owned(),
                }),
                "AssetWorkspacePathValidationError",
            ),
            (
                &workspace,
                AssetError::Workspace(operation_error()),
                "AssetWorkspaceAssetInspectionError",
            ),
            (
                &favicon,
                AssetError::Workspace(WorkspaceError::RootNotDirectory {
                    path: missing_root.clone(),
                }),
                "AssetWorkspaceRootNormalizationError",
            ),
            (
                &favicon,
                AssetError::Workspace(operation_error()),
                "AssetProjectFaviconInspectionError",
            ),
        ] {
            assert_eq!(asset_wire_from_error(resource, &error)["_tag"], tag);
        }
        let encoding = serde_json::from_str::<Value>("").unwrap_err();
        assert_eq!(
            asset_wire_from_error(&workspace, &AssetError::Encoding(encoding))["_tag"],
            "AssetSigningKeyLoadError"
        );
        assert_eq!(
            review_wire_error(ReviewError::Backend("failed".to_owned()))["_tag"],
            "Defect"
        );
        assert_eq!(defect("failed")["message"], "failed");
    }
}
