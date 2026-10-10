//! Worktree audit: removal outcomes, and a read-only scan every 10 minutes that compares the
//! worktree catalog with the filesystem, Git and the processes BiBCode owns.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use sysinfo::Pid;
use tokio::{task::JoinHandle, time::MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use super::{
    cite, emit,
    findings::{FindingInput, Severity},
    is_enabled, record_finding,
    redact::redact_path,
    runtime::Detector,
    sampler::descendants_of,
};
use crate::{
    diagnostics::ProcessAttributionRegistry,
    persistence::Repositories,
    worktree_catalog::{AdoptedWorktreeAvailability, WorktreeCatalogService},
};

const SCAN_EVERY: Duration = Duration::from_secs(10 * 60);
/// How soon a tick that could not scan (its sources not bound yet, or the previous scan's
/// filesystem work still running) looks again.
const BIND_RETRY: Duration = Duration::from_secs(1);
/// The comparison with the filesystem and Git still running after this is abandoned; the
/// registry and process facts are audited regardless.
const SCAN_BUDGET: Duration = Duration::from_secs(30);
/// A project whose `git worktree list` takes longer leaves its managed directory uncompared.
const GIT_DEADLINE: Duration = Duration::from_secs(8);
/// Reading the working directories of BiBCode's processes taking longer (on macOS it touches
/// each directory, which a hung network mount can stall) leaves them unread for the scan.
const PROCESS_DEADLINE: Duration = Duration::from_secs(5);
/// Entries read from each managed worktree directory.
const MAX_DIR_ENTRIES: usize = 500;
/// The catalog projection's own limit of adopted worktrees per project.
const MAX_PROJECT_ENTRIES: usize = 512;
/// A `removing` guard held longer than this is stuck.
const STUCK_AFTER: Duration = Duration::from_secs(10 * 60);
/// What Linux appends to the path of a working directory that was removed. `process_cwd`
/// keeps it, and marks a removed directory the same way on macOS.
const DELETED_MARKER: &str = " (deleted)";

/// Records one `worktree.remove` or `worktree.removeFromBibCode` outcome: a `worktree.removal`
/// event, and a `worktree.removal-failed` finding for a failure that is not the protocol
/// working as designed. `outcome` carries the error's kebab-case `reason`, and `cancelled`
/// says the client interrupted the request. Does nothing while the profiler is off.
pub fn record_removal_outcome(
    mode: &str,
    outcome: Result<(), &str>,
    duration: Duration,
    timed_out_drain: bool,
    cancelled: bool,
) {
    if !is_enabled() {
        return;
    }
    let reason = outcome.err();
    let capacity = matches!(reason, Some("cleanup-capacity" | "operation-capacity"));
    let duration_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
    let event_id = emit(
        "worktree.removal",
        json!({
            "mode": mode,
            "ok": reason.is_none(),
            "reason": reason,
            "durationMs": duration_ms,
            "drainTimedOut": timed_out_drain,
            "capacity": capacity,
            "cancelled": cancelled,
        }),
    );
    if let Some(reason) = reason
        && let Some(finding) =
            removal_finding(mode, reason, duration_ms, timed_out_drain, cancelled)
    {
        record_finding(FindingInput {
            evidence: cite(finding.evidence, event_id),
            ..finding
        });
    }
}

/// The `worktree.removal-failed` finding of a failed removal, if it deserves one. Its
/// evidence keeps `cancelled`, so a failure while the client was interrupting stays
/// recognisable.
fn removal_finding(
    mode: &str,
    reason: &str,
    duration_ms: u64,
    drain_timed_out: bool,
    cancelled: bool,
) -> Option<FindingInput> {
    let (severity, cause) = removal_failure(reason, drain_timed_out, cancelled)?;
    Some(finding(
        "worktree.removal-failed",
        severity,
        cause,
        "Worktree removal failed",
        format!("A {mode} removal failed with {cause} after {duration_ms} ms."),
        json!({
            "mode": mode,
            "reason": reason,
            "durationMs": duration_ms,
            "drainTimedOut": drain_timed_out,
            "cancelled": cancelled,
        }),
    ))
}

/// How a failed removal is reported: `None` when its event tells the whole story (the
/// protocol working as designed, or the client stopping the request), else the finding's
/// severity and fingerprint reason.
fn removal_failure(
    reason: &str,
    drain_timed_out: bool,
    cancelled: bool,
) -> Option<(Severity, &str)> {
    if drain_timed_out {
        // It answers `session-running`, like a session that was running from the start.
        return Some((Severity::Warn, "admission-drain-timeout"));
    }
    let severity = match reason {
        "stale-plan"
        | "stale-generation"
        | "session-running"
        | "thread-not-found"
        | "operation-shutting-down" => return None,
        // A client interrupt surfaces as `Cancelled`, folded into `orchestration-failed`.
        "orchestration-failed" if cancelled => return None,
        "cleanup-capacity"
        | "operation-capacity"
        | "dirty-confirmation-required"
        | "prune-confirmation-required"
        | "locked"
        | "protected-target"
        | "environment-unsupported"
        | "command-conflict"
        | "ownership-conflict"
        | "replacement-conflict"
        | "repository-mismatch"
        | "workspace-unavailable" => Severity::Warn,
        // `git-failed`, `internal`, `orchestration-failed`, `workspace-identity-unavailable`,
        // and any reason not classified here, `unknown` included.
        _ => Severity::Error,
    };
    Some((severity, reason))
}

/// What the scan reads. The profiler starts before the server builds the catalog, so the
/// server binds these later; until then every tick is skipped.
#[derive(Clone)]
pub struct AuditSources {
    pub catalog: WorktreeCatalogService,
    pub repositories: Repositories,
}

pub struct WorktreeAuditDetector {
    sources: Arc<OnceLock<AuditSources>>,
    processes: ProcessAttributionRegistry,
    /// Set while a scan or its filesystem work runs, so there is at most one at a time.
    scanning: Arc<AtomicBool>,
}

impl WorktreeAuditDetector {
    pub fn new(
        sources: Arc<OnceLock<AuditSources>>,
        processes: ProcessAttributionRegistry,
    ) -> Self {
        Self {
            sources,
            processes,
            scanning: Arc::default(),
        }
    }
}

impl Detector for WorktreeAuditDetector {
    fn name(&self) -> &'static str {
        "worktree-audit"
    }

    fn start(&self, cancel: CancellationToken) -> JoinHandle<()> {
        let (sources, processes, scanning) = (
            self.sources.clone(),
            self.processes.clone(),
            self.scanning.clone(),
        );
        tokio::spawn(async move {
            // The first tick is immediate: turning the profiler on scans at once.
            let mut ticks = tokio::time::interval(SCAN_EVERY);
            ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
            let mut reported_busy = false;
            loop {
                tokio::select! {
                    () = cancel.cancelled() => return,
                    _ = ticks.tick() => {}
                }
                let Some(bound) = sources.get() else {
                    // The server binds the sources moments after start-up: look again soon,
                    // not in ten minutes.
                    ticks.reset_after(BIND_RETRY);
                    continue;
                };
                let Some(permit) = ScanGuard::acquire(&scanning) else {
                    // A dropped scan's filesystem work (on a hung mount, say) still holds the
                    // permit. Look again soon, so a quick off-and-on still scans promptly.
                    if !reported_busy {
                        tracing::warn!(
                            "worktree audit is waiting for the previous scan's filesystem work"
                        );
                        reported_busy = true;
                    }
                    ticks.reset_after(BIND_RETRY);
                    continue;
                };
                reported_busy = false;
                tokio::select! {
                    () = cancel.cancelled() => return,
                    // The profiler shuts down after the catalog: stop with the catalog.
                    () = bound.catalog.shutting_down() => return,
                    () = scan(bound, &processes, permit, LIMITS) => {}
                }
            }
        })
    }
}

/// Runs one scan now, outside the detector's schedule: for tests that need each scan's
/// result in turn.
#[doc(hidden)]
pub async fn scan_once(sources: &AuditSources, processes: &ProcessAttributionRegistry) {
    if let Some(permit) = ScanGuard::acquire(&Arc::default()) {
        scan(sources, processes, permit, LIMITS).await;
    }
}

/// The one-scan flag, released when the scan and every blocking step it started have ended.
struct ScanGuard(Arc<AtomicBool>);

impl ScanGuard {
    fn acquire(flag: &Arc<AtomicBool>) -> Option<Self> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self(flag.clone()))
    }
}

impl Drop for ScanGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(Clone, Copy)]
struct ScanLimits {
    budget: Duration,
    git: Duration,
    processes: Duration,
}

const LIMITS: ScanLimits = ScanLimits {
    budget: SCAN_BUDGET,
    git: GIT_DEADLINE,
    processes: PROCESS_DEADLINE,
};

/// Reads the working directory of each process BiBCode owns, given the registered pids:
/// `process_cwds`, or a stand-in in tests.
type ProcessReader = fn(Vec<u32>, &CancellationToken) -> Vec<(PathBuf, u32)>;

/// Whether `process_cwd` reads anything on this platform.
const READS_PROCESS_CWDS: bool = cfg!(any(target_os = "linux", target_os = "macos"));

/// The `processesRead` field: whether the working directories were read, or `None` (null)
/// where this platform reads none, so no scan there can check `worktree.held-open`.
fn processes_read(read: bool, platform_reads: bool) -> Option<bool> {
    platform_reads.then_some(read)
}

/// Which parts of a scan's facts were read in full. The rest of the scan is still audited.
#[derive(Clone, Copy, Debug)]
struct Coverage {
    /// The working directories of BiBCode's processes.
    processes: bool,
    /// The comparison with the filesystem and Git.
    compared: bool,
}

async fn scan(
    sources: &AuditSources,
    processes: &ProcessAttributionRegistry,
    permit: ScanGuard,
    limits: ScanLimits,
) {
    let started = Instant::now();
    let (facts, coverage) = gather(sources, processes, permit, limits, process_cwds).await;
    let findings = audit(&facts);
    emit(
        "worktree.audit",
        json!({
            "entries": facts.catalog.len(),
            "dirs": facts.base_dirs.len(),
            "registrations": facts.git_registrations.len(),
            "processes": facts.held_open.len(),
            "findings": findings.len(),
            "compared": coverage.compared,
            "processesRead": processes_read(coverage.processes, READS_PROCESS_CWDS),
            "durationMs": u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        }),
    );
    for finding in findings {
        record_finding(finding);
    }
}

/// Reads one scan's facts, and which parts were read in full. Read-only: it never refreshes
/// the catalog. The registry and the processes come first and are always audited; the
/// process step and the comparison each give up at their own deadline.
async fn gather(
    sources: &AuditSources,
    processes: &ProcessAttributionRegistry,
    permit: ScanGuard,
    limits: ScanLimits,
    read_processes: ProcessReader,
) -> (AuditFacts, Coverage) {
    // Dropping the scan cancels its Git commands and stops its filesystem work between
    // operations. Each blocking step holds the permit: a dropped scan cannot abort one, so no
    // second scan starts until it ends.
    let cancel = CancellationToken::new();
    let _cancel_on_drop = cancel.clone().drop_guard();
    let permit = Arc::new(permit);
    let guards = sources.catalog.availability_registry().guarded();
    let registered = processes
        .registered_sources()
        .into_iter()
        .map(|(identity, _)| identity.pid)
        .collect();
    let process_cancel = cancel.child_token();
    let process_step = blocking(&permit, &process_cancel, move |cancel| {
        read_processes(registered, cancel)
    });
    let held_open = match tokio::time::timeout(limits.processes, process_step).await {
        // `None`: the step failed, which is logged.
        Ok(read) => read,
        Err(_) => {
            process_cancel.cancel();
            tracing::warn!(
                deadline_secs = limits.processes.as_secs(),
                "worktree audit could not read process working directories in time"
            );
            None
        }
    };
    let processes_read = held_open.is_some();
    let held_open = held_open.unwrap_or_default();
    let comparison =
        match tokio::time::timeout(limits.budget, compare(sources, &permit, &cancel, limits)).await
        {
            Ok(comparison) => comparison,
            Err(_) => {
                cancel.cancel();
                tracing::warn!(
                    budget_secs = limits.budget.as_secs(),
                    "worktree audit comparison ran out of time; only the registry and \
                     processes were audited"
                );
                None
            }
        };
    let compared = comparison.is_some();
    let comparison = comparison.unwrap_or_default();
    // Every guarded workspace by thread, under its persisted path once the comparison has
    // read it, and every other owner.
    let mut entries: HashMap<String, (PathBuf, Option<(AdoptedWorktreeAvailability, Duration)>)> =
        guards
            .into_iter()
            .map(|(guard, held)| {
                let state = (guard.availability, held);
                (guard.thread_id, (PathBuf::from(guard.path), Some(state)))
            })
            .collect();
    for (thread_id, path) in comparison.owned {
        entries
            .entry(thread_id)
            .and_modify(|entry| entry.0.clone_from(&path))
            .or_insert((path, None));
    }
    let mut facts = AuditFacts {
        base_dirs: comparison.base_dirs,
        git_registrations: comparison.registrations,
        managed_dirs: comparison.managed_dirs,
        held_open,
        ..AuditFacts::default()
    };
    for (path, guard) in entries.into_values() {
        if let Some((AdoptedWorktreeAvailability::Removing, held)) = guard {
            facts.removing_since.push((path.clone(), held));
        }
        facts.catalog.push(CatalogEntryFact {
            path,
            availability: guard.map_or(AdoptedWorktreeAvailability::Present, |(state, _)| state),
        });
    }
    let coverage = Coverage {
        processes: processes_read,
        compared,
    };
    (facts, coverage)
}

/// What the comparison read, with every path in its physical spelling.
#[derive(Default)]
struct Comparison {
    /// Each owning thread and its worktree path.
    owned: Vec<(String, PathBuf)>,
    base_dirs: Vec<PathBuf>,
    registrations: HashMap<PathBuf, bool>,
    managed_dirs: Vec<PathBuf>,
}

/// Reads, in order, the managed directories' listings, then each project's owners, then its
/// registrations, so a worktree created or removed meanwhile is never half-seen:
/// - a directory listed before its `git worktree add` registered it is registered by the time
///   Git is asked, and one removed meanwhile is dropped by the final existence check;
/// - a removal prunes a registration before it detaches the owner, so owners read first never
///   miss a registration's owner.
///
/// `None` when the projects or the filesystem could not be read.
async fn compare(
    sources: &AuditSources,
    permit: &Arc<ScanGuard>,
    cancel: &CancellationToken,
    limits: ScanLimits,
) -> Option<Comparison> {
    let projects: Vec<_> = sources
        .repositories
        .list_projects()
        .await
        .inspect_err(|error| tracing::warn!(%error, "worktree audit could not list projects"))
        .ok()?
        .into_iter()
        .filter(|project| project.deleted_at.is_none())
        .collect();
    let git = sources.catalog.git_repository();
    let mut parents = Vec::with_capacity(projects.len());
    for project in &projects {
        parents.push(match &git {
            Some(git) => {
                git.managed_worktree_parent(Path::new(&project.workspace_root))
                    .await
            }
            None => None,
        });
    }
    let (parents, listings) =
        blocking(permit, cancel, move |cancel| list_managed(parents, cancel)).await?;
    let mut owned = Vec::new();
    let mut registrations = Vec::new();
    // Each managed directory, and whether every project creating worktrees in it had its
    // registrations read. Only those are compared: a registration that could not be read is
    // not one that is absent.
    let mut complete: HashMap<PathBuf, bool> = HashMap::new();
    // A thread can own a worktree in another project's managed directory (two checkouts of
    // one repository), so one unread owner list makes every comparison unsafe.
    let mut all_owners_known = true;
    for (project, parent) in projects.into_iter().zip(parents) {
        match sources
            .repositories
            .load_worktree_catalog_projection(project.project_id, MAX_PROJECT_ENTRIES)
            .await
        {
            Ok(Some(projection)) => {
                all_owners_known &= !projection.truncated;
                owned.extend(projection.threads.into_iter().filter_map(|thread| {
                    Some((thread.thread_id, PathBuf::from(thread.worktree_path?)))
                }));
            }
            // Deleted since it was listed: it owns nothing.
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%error, "worktree audit could not read a project's worktree owners");
                all_owners_known = false;
            }
        }
        let (Some(git), Some(parent)) = (&git, parent) else {
            continue;
        };
        let root = PathBuf::from(project.workspace_root);
        // A project outside Git, a checkout that is gone, or a repository too slow to answer
        // leaves its directory uncompared.
        let read = tokio::time::timeout(limits.git, git.worktree_records(&root, cancel)).await;
        let read = match read {
            Ok(Ok((records, _))) => {
                registrations.extend(
                    records
                        .into_iter()
                        .map(|record| (record.path, record.is_prunable)),
                );
                true
            }
            Ok(Err(_)) | Err(_) => false,
        };
        *complete.entry(parent).or_insert(true) &= read;
    }
    if !all_owners_known {
        complete.values_mut().for_each(|complete| *complete = false);
    }
    blocking(permit, cancel, move |cancel| {
        resolve(owned, complete, listings, registrations, cancel)
    })
    .await
}

/// Each project's managed directory in its physical spelling, and what one level of each
/// holds: directories only, never following a link, at most `MAX_DIR_ENTRIES` read.
fn list_managed(
    parents: Vec<Option<PathBuf>>,
    cancel: &CancellationToken,
) -> (Vec<Option<PathBuf>>, HashMap<PathBuf, Vec<PathBuf>>) {
    let parents: Vec<_> = parents
        .into_iter()
        .map(|parent| parent.map(|parent| physical(&parent)))
        .collect();
    let mut listings = HashMap::new();
    for parent in parents.iter().flatten() {
        if cancel.is_cancelled() || listings.contains_key(parent) {
            continue;
        }
        // A managed directory that does not exist yet has nothing to list.
        let Ok(listing) = std::fs::read_dir(parent) else {
            continue;
        };
        let children = listing
            .take(MAX_DIR_ENTRIES)
            .take_while(|_| !cancel.is_cancelled())
            .filter_map(Result::ok)
            // `file_type` does not follow links, so a link is never listed as a directory.
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.path())
            .collect();
        listings.insert(parent.clone(), children);
    }
    (parents, listings)
}

/// The filesystem half of the comparison: physical paths for the owners and registrations,
/// and the listed directories of complete managed directories that still exist. Returns early
/// once `cancel` fires.
fn resolve(
    owned: Vec<(String, PathBuf)>,
    complete: HashMap<PathBuf, bool>,
    listings: HashMap<PathBuf, Vec<PathBuf>>,
    registrations: Vec<(PathBuf, bool)>,
    cancel: &CancellationToken,
) -> Comparison {
    let managed_dirs: Vec<PathBuf> = complete
        .into_iter()
        .filter_map(|(parent, complete)| complete.then_some(parent))
        .collect();
    let mut comparison = Comparison::default();
    for (parent, children) in listings {
        if managed_dirs.contains(&parent) {
            comparison.base_dirs.extend(
                children
                    .into_iter()
                    .take_while(|_| !cancel.is_cancelled())
                    // Removed since it was listed: a removal finishing, not a leftover.
                    .filter(|child| std::fs::symlink_metadata(child).is_ok()),
            );
        }
    }
    for (path, prunable) in registrations
        .into_iter()
        .take_while(|_| !cancel.is_cancelled())
    {
        let path = physical(&path);
        // A leftover is registered in a complete managed directory while its directory is
        // gone. One still on disk is the user's own: "Remove from BiBCode" keeps it.
        let leftover = prunable
            && path
                .parent()
                .is_some_and(|parent| managed_dirs.iter().any(|dir| dir == parent));
        comparison.registrations.insert(path, leftover);
    }
    comparison.owned = owned
        .into_iter()
        .take_while(|_| !cancel.is_cancelled())
        .map(|(thread_id, path)| (thread_id, physical(&path)))
        .collect();
    comparison.managed_dirs = managed_dirs;
    comparison
}

/// Runs filesystem work off the async workers, holding the scan permit until it ends. A
/// panic is logged and reads as `None`, never as a clean result.
async fn blocking<T: Send + 'static>(
    permit: &Arc<ScanGuard>,
    cancel: &CancellationToken,
    work: impl FnOnce(&CancellationToken) -> T + Send + 'static,
) -> Option<T> {
    let (permit, cancel) = (permit.clone(), cancel.clone());
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work(&cancel)
    })
    .await
    .inspect_err(|error| tracing::warn!(%error, "worktree audit filesystem work failed"))
    .ok()
}

/// The working directory of each process BiBCode owns: the registered roots, and the
/// descendants of this process (a build under a terminal, say).
fn process_cwds(registered: Vec<u32>, cancel: &CancellationToken) -> Vec<(PathBuf, u32)> {
    let mut pids: HashSet<u32> = descendants_of(Pid::from_u32(std::process::id()))
        .into_iter()
        .map(|(pid, _)| pid.as_u32())
        .collect();
    pids.extend(registered);
    // ponytail: a registration can outlive its process, so a reused pid is read as well; it
    // matters only if that process also works inside a removed worktree.
    pids.into_iter()
        .take_while(|_| !cancel.is_cancelled())
        .filter_map(|pid| Some((process_cwd(pid)?, pid)))
        .collect()
}

/// The physical spelling of `path`. A missing path resolves through its nearest existing
/// ancestor, so a removed worktree under a linked directory still matches the spelling Git
/// and the kernel report. Unlike `canonical_worktree_path_key` it keeps the case, so
/// summaries can still have the home directory redacted.
fn physical(path: &Path) -> PathBuf {
    let mut missing = Vec::new();
    let mut ancestor = path;
    loop {
        if let Ok(mut resolved) = std::fs::canonicalize(ancestor) {
            resolved.extend(missing.iter().rev());
            return resolved;
        }
        match (ancestor.file_name(), ancestor.parent()) {
            (Some(name), Some(parent)) => {
                missing.push(name);
                ancestor = parent;
            }
            _ => return path.to_path_buf(),
        }
    }
}

/// The working directory, with Linux's own `(deleted)` marker kept: the evidence that the
/// directory was removed under the process.
#[cfg(target_os = "linux")]
fn process_cwd(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}

/// The working directory, marked as Linux marks one that was removed when it no longer
/// exists.
#[cfg(target_os = "macos")]
fn process_cwd(pid: u32) -> Option<PathBuf> {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

    let pid = i32::try_from(pid).ok()?;
    let size = i32::try_from(std::mem::size_of::<libc::proc_vnodepathinfo>()).ok()?;
    let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::uninit();
    // SAFETY: `info` is writable storage of `size` bytes, the size this flavor fills.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if written != size {
        return None;
    }
    // SAFETY: the call filled all `size` bytes.
    let info = unsafe { info.assume_init() };
    // `vip_path` is a NUL-terminated `char[MAXPATHLEN]`, split into rows by libc.
    let path: Vec<u8> = info
        .pvi_cdir
        .vip_path
        .iter()
        .flatten()
        .map(|&byte| byte as u8)
        .take_while(|&byte| byte != 0)
        .collect();
    if path.is_empty() {
        return None;
    }
    let path = PathBuf::from(OsStr::from_bytes(&path));
    if matches!(std::fs::symlink_metadata(&path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
    {
        let mut marked = path.into_os_string();
        marked.push(DELETED_MARKER);
        return Some(PathBuf::from(marked));
    }
    Some(path)
}

/// Other platforms report no working directories, so they have no held-open findings.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_cwd(_pid: u32) -> Option<PathBuf> {
    None
}

/// The path without the `(deleted)` marker, and whether it had one.
fn strip_deleted(path: &Path) -> (PathBuf, bool) {
    #[cfg(unix)]
    {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

        if let Some(removed) = path
            .as_os_str()
            .as_bytes()
            .strip_suffix(DELETED_MARKER.as_bytes())
        {
            return (PathBuf::from(OsStr::from_bytes(removed)), true);
        }
    }
    (path.to_path_buf(), false)
}

/// One adopted worktree of the catalog.
#[derive(Clone, Debug)]
pub struct CatalogEntryFact {
    pub path: PathBuf,
    pub availability: AdoptedWorktreeAvailability,
}

/// What one scan observed. Paths are compared as they are, so the scan resolves them first.
/// The directory facts cover only the complete managed worktree directories: those whose
/// owners and registrations were all read.
#[derive(Debug, Default)]
pub struct AuditFacts {
    /// Every guarded or owned worktree.
    pub catalog: Vec<CatalogEntryFact>,
    /// The directories found one level under the managed worktree directories.
    pub base_dirs: Vec<PathBuf>,
    /// Every path Git registers as a worktree, and whether it is a leftover: registered in a
    /// managed worktree directory while its own directory is gone.
    pub git_registrations: HashMap<PathBuf, bool>,
    /// Each catalog path held in `removing`, and for how long.
    pub removing_since: Vec<(PathBuf, Duration)>,
    /// The working directory of each process BiBCode owns, with its pid. One that was removed
    /// ends in `(deleted)`.
    pub held_open: Vec<(PathBuf, u32)>,
    /// The managed worktree directories.
    pub managed_dirs: Vec<PathBuf>,
}

/// The findings one scan's facts show. Fingerprints hold only the kind and a reason, never a
/// path, so each problem is one finding however many worktrees show it.
pub fn audit(facts: &AuditFacts) -> Vec<FindingInput> {
    let owned: HashSet<&Path> = facts
        .catalog
        .iter()
        .map(|entry| entry.path.as_path())
        .collect();
    let mut findings = Vec::new();
    for entry in &facts.catalog {
        if let Some(reason @ ("missing-registered" | "missing-unregistered")) =
            guard_reason(entry.availability)
        {
            let path = redact(&entry.path);
            findings.push(finding(
                "worktree.missing",
                Severity::Warn,
                reason,
                "Worktree directory is missing",
                format!("{path} is {reason}."),
                json!({ "path": path }),
            ));
        }
    }
    for (path, held) in &facts.removing_since {
        if *held > STUCK_AFTER {
            let (path, minutes) = (redact(path), held.as_secs() / 60);
            findings.push(finding(
                "worktree.stuck-removal",
                Severity::Error,
                "removing",
                "Worktree removal is stuck",
                format!("{path} has been removing for {minutes} minutes."),
                json!({ "path": path, "minutes": minutes }),
            ));
        }
    }
    for dir in &facts.base_dirs {
        if !owned.contains(dir.as_path()) && !facts.git_registrations.contains_key(dir) {
            let path = redact(dir);
            findings.push(finding(
                "worktree.orphan-dir",
                Severity::Warn,
                "unowned-unregistered",
                "Leftover worktree directory",
                format!(
                    "{path} is in a managed worktree directory, but no catalog entry owns it \
                     and Git does not register it."
                ),
                json!({ "path": path }),
            ));
        }
    }
    for (registered, leftover) in &facts.git_registrations {
        if *leftover && !owned.contains(registered.as_path()) {
            let path = redact(registered);
            findings.push(finding(
                "worktree.registration-mismatch",
                Severity::Warn,
                "unowned-registration",
                "Worktree registration without an owner",
                format!(
                    "Git registers {path} in a managed worktree directory, but the directory \
                     is gone and no catalog entry owns it."
                ),
                json!({ "path": path }),
            ));
        }
    }
    for (cwd, pid) in &facts.held_open {
        let (cwd, removed) = strip_deleted(cwd);
        let guarded = facts.catalog.iter().find_map(|entry| {
            guard_reason(entry.availability)
                .filter(|_| cwd.starts_with(&entry.path))
                .map(|reason| (entry.path.as_path(), reason))
        });
        let held = match guarded {
            // A removal stops its processes first: only one outlasting a stuck removal counts.
            Some((path, "removing")) => facts
                .removing_since
                .iter()
                .any(|(removing, held)| removing == path && *held > STUCK_AFTER)
                .then_some((path, "removing")),
            Some(guarded) => Some(guarded),
            None => (removed && facts.managed_dirs.iter().any(|dir| cwd.starts_with(dir)))
                .then_some((cwd.as_path(), "removed")),
        };
        if let Some((path, reason)) = held {
            let path = redact(path);
            findings.push(finding(
                "worktree.held-open",
                Severity::Warn,
                reason,
                "A process holds a removed worktree open",
                format!("Process {pid} works in {path}, which is {reason}."),
                json!({ "path": path, "pid": pid }),
            ));
        }
    }
    findings
}

/// The availabilities that keep a workspace from being used, by their wire names.
fn guard_reason(availability: AdoptedWorktreeAvailability) -> Option<&'static str> {
    match availability {
        AdoptedWorktreeAvailability::MissingRegistered => Some("missing-registered"),
        AdoptedWorktreeAvailability::MissingUnregistered => Some("missing-unregistered"),
        AdoptedWorktreeAvailability::Removing => Some("removing"),
        AdoptedWorktreeAvailability::Present
        | AdoptedWorktreeAvailability::VerificationUnavailable => None,
    }
}

fn finding(
    kind: &'static str,
    severity: Severity,
    reason: &str,
    title: &str,
    summary: String,
    evidence: Value,
) -> FindingInput {
    FindingInput {
        kind,
        severity,
        source: "server".into(),
        fingerprint_fields: vec![reason.to_owned()],
        title: title.into(),
        summary,
        evidence,
    }
}

fn redact(path: &Path) -> String {
    redact_path(&path.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        git::{BoxGitProcessFuture, GitProcessRunner, GitRepository, ProcessError, ProcessRequest},
        persistence::{Database, ProjectionProject, run_migrations},
        worktree_catalog::{RemovalGuard, WorkspaceAvailabilityRegistry, WorkspaceLossTransition},
    };

    const MINUTE: Duration = Duration::from_secs(60);

    fn entry(path: &str, availability: AdoptedWorktreeAvailability) -> CatalogEntryFact {
        CatalogEntryFact {
            path: PathBuf::from(path),
            availability,
        }
    }

    fn kinds(facts: &AuditFacts) -> Vec<(&'static str, Vec<String>)> {
        audit(facts)
            .into_iter()
            .map(|finding| (finding.kind, finding.fingerprint_fields))
            .collect()
    }

    /// Windows reads no working directories, so its scans never claim to have read them.
    #[test]
    fn processes_read_is_null_where_no_working_directory_is_read() {
        assert_eq!(processes_read(true, false), None);
        assert_eq!(processes_read(false, false), None);
        assert_eq!(processes_read(true, true), Some(true));
        assert_eq!(processes_read(false, true), Some(false));
    }

    #[test]
    fn a_missing_entry_is_a_missing_finding() {
        let facts = AuditFacts {
            catalog: vec![
                entry("/wt/a", AdoptedWorktreeAvailability::MissingRegistered),
                entry("/wt/b", AdoptedWorktreeAvailability::Present),
            ],
            ..AuditFacts::default()
        };
        assert_eq!(
            kinds(&facts),
            [("worktree.missing", vec!["missing-registered".to_owned()])]
        );
    }

    #[test]
    fn removing_longer_than_ten_minutes_is_stuck() {
        let stuck = AuditFacts {
            catalog: vec![entry("/wt/a", AdoptedWorktreeAvailability::Removing)],
            removing_since: vec![(PathBuf::from("/wt/a"), 11 * MINUTE)],
            ..AuditFacts::default()
        };
        assert_eq!(
            kinds(&stuck),
            [("worktree.stuck-removal", vec!["removing".to_owned()])]
        );
        let recent = AuditFacts {
            removing_since: vec![(PathBuf::from("/wt/a"), 9 * MINUTE)],
            ..stuck
        };
        assert_eq!(kinds(&recent), []);
    }

    #[test]
    fn an_unowned_unregistered_directory_is_an_orphan() {
        let facts = AuditFacts {
            catalog: vec![entry(
                "/base/repo/owned",
                AdoptedWorktreeAvailability::Present,
            )],
            base_dirs: vec![
                PathBuf::from("/base/repo/owned"),
                PathBuf::from("/base/repo/registered"),
                PathBuf::from("/base/repo/orphan"),
            ],
            git_registrations: HashMap::from([
                (PathBuf::from("/base/repo/owned"), true),
                (PathBuf::from("/base/repo/registered"), true),
            ]),
            ..AuditFacts::default()
        };
        let found = kinds(&facts);
        assert!(found.contains(&(
            "worktree.orphan-dir",
            vec!["unowned-unregistered".to_owned()]
        )));
        assert_eq!(
            found
                .iter()
                .filter(|(kind, _)| *kind == "worktree.orphan-dir")
                .count(),
            1
        );
    }

    #[test]
    fn an_unowned_registration_whose_directory_is_gone_is_a_mismatch() {
        let facts = AuditFacts {
            catalog: vec![entry(
                "/base/repo/owned",
                AdoptedWorktreeAvailability::Present,
            )],
            git_registrations: HashMap::from([
                (PathBuf::from("/base/repo/owned"), false),
                (PathBuf::from("/base/repo/leftover"), true),
                // Detached with "Remove from BiBCode": still on disk, the user's own.
                (PathBuf::from("/base/repo/detached"), false),
            ]),
            ..AuditFacts::default()
        };
        assert_eq!(
            kinds(&facts),
            [(
                "worktree.registration-mismatch",
                vec!["unowned-registration".to_owned()]
            )]
        );
    }

    #[test]
    fn a_process_inside_a_removed_or_long_removing_worktree_holds_it_open() {
        let facts = AuditFacts {
            catalog: vec![
                entry("/wt/stuck", AdoptedWorktreeAvailability::Removing),
                entry("/wt/quiescing", AdoptedWorktreeAvailability::Removing),
                entry("/wt/gone", AdoptedWorktreeAvailability::MissingUnregistered),
                entry("/base/repo/live", AdoptedWorktreeAvailability::Present),
            ],
            removing_since: vec![
                (PathBuf::from("/wt/stuck"), 11 * MINUTE),
                (PathBuf::from("/wt/quiescing"), 9 * MINUTE),
            ],
            managed_dirs: vec![PathBuf::from("/base/repo")],
            held_open: vec![
                (PathBuf::from("/wt/stuck/src"), 41),
                // A normal removal is still stopping this one.
                (PathBuf::from("/wt/quiescing"), 42),
                (PathBuf::from("/wt/gone (deleted)"), 43),
                (PathBuf::from("/base/repo/live"), 44),
                (PathBuf::from("/wt/stuck-other"), 45),
                // Removed under the process, in a managed directory.
                (PathBuf::from("/base/repo/removed/src (deleted)"), 46),
                // Removed, but not in a managed directory: not BiBCode's worktree.
                (PathBuf::from("/elsewhere/tmp (deleted)"), 47),
            ],
            ..AuditFacts::default()
        };
        let held: Vec<_> = audit(&facts)
            .into_iter()
            .filter(|finding| finding.kind == "worktree.held-open")
            .map(|finding| (finding.fingerprint_fields, finding.evidence["pid"].clone()))
            .collect();
        assert_eq!(
            held,
            [
                (vec!["removing".to_owned()], serde_json::json!(41)),
                (
                    vec!["missing-unregistered".to_owned()],
                    serde_json::json!(43)
                ),
                (vec!["removed".to_owned()], serde_json::json!(46)),
            ]
        );
    }

    #[test]
    fn removal_failures_are_classified_by_reason() {
        use Severity::{Error, Warn};
        // (reason, drain timed out, cancelled) -> finding severity and reason; `None` is the
        // event alone. Every `WorktreeRemovalErrorReason`, both `WorktreeOperationError`
        // reasons, the availability errors, and reasons nobody classified.
        type Row = (&'static str, bool, bool, Option<(Severity, &'static str)>);
        let table: &[Row] = &[
            ("stale-plan", false, false, None),
            ("stale-generation", false, false, None),
            ("session-running", false, false, None),
            ("thread-not-found", false, false, None),
            ("operation-shutting-down", false, false, None),
            ("orchestration-failed", false, true, None),
            (
                "session-running",
                true,
                false,
                Some((Warn, "admission-drain-timeout")),
            ),
            (
                "cleanup-capacity",
                false,
                false,
                Some((Warn, "cleanup-capacity")),
            ),
            (
                "operation-capacity",
                false,
                false,
                Some((Warn, "operation-capacity")),
            ),
            (
                "dirty-confirmation-required",
                false,
                false,
                Some((Warn, "dirty-confirmation-required")),
            ),
            (
                "prune-confirmation-required",
                false,
                false,
                Some((Warn, "prune-confirmation-required")),
            ),
            ("locked", false, false, Some((Warn, "locked"))),
            (
                "protected-target",
                false,
                false,
                Some((Warn, "protected-target")),
            ),
            (
                "environment-unsupported",
                false,
                false,
                Some((Warn, "environment-unsupported")),
            ),
            (
                "command-conflict",
                false,
                false,
                Some((Warn, "command-conflict")),
            ),
            (
                "ownership-conflict",
                false,
                false,
                Some((Warn, "ownership-conflict")),
            ),
            (
                "replacement-conflict",
                false,
                false,
                Some((Warn, "replacement-conflict")),
            ),
            (
                "repository-mismatch",
                false,
                false,
                Some((Warn, "repository-mismatch")),
            ),
            (
                "workspace-unavailable",
                false,
                false,
                Some((Warn, "workspace-unavailable")),
            ),
            ("git-failed", false, false, Some((Error, "git-failed"))),
            // A failure is still one when the client has gone.
            ("git-failed", false, true, Some((Error, "git-failed"))),
            ("internal", false, false, Some((Error, "internal"))),
            (
                "orchestration-failed",
                false,
                false,
                Some((Error, "orchestration-failed")),
            ),
            (
                "workspace-identity-unavailable",
                false,
                false,
                Some((Error, "workspace-identity-unavailable")),
            ),
            ("unknown", false, false, Some((Error, "unknown"))),
            (
                "a-future-reason",
                false,
                false,
                Some((Error, "a-future-reason")),
            ),
        ];
        for &(reason, drain_timed_out, cancelled, expected) in table {
            assert_eq!(
                removal_failure(reason, drain_timed_out, cancelled),
                expected,
                "{reason} drain_timed_out={drain_timed_out} cancelled={cancelled}"
            );
        }
    }

    /// Hangs every Git command until it is cancelled.
    struct HangingGit;

    impl GitProcessRunner for HangingGit {
        fn run<'a>(
            &'a self,
            request: ProcessRequest,
            cancellation: &'a CancellationToken,
        ) -> BoxGitProcessFuture<'a> {
            Box::pin(async move {
                cancellation.cancelled().await;
                Err(ProcessError::Cancelled {
                    operation: request.operation,
                })
            })
        }
    }

    /// One project at `<root>/main` whose Git never answers, with a missing workspace and a
    /// removal stuck for 11 minutes in the registry.
    async fn stalled_repository(root: &Path) -> (AuditSources, RemovalGuard) {
        let main = root.join("main");
        std::fs::create_dir(&main).unwrap();
        let database = Database::open_in_memory().await.unwrap();
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .unwrap();
        let repositories = Repositories::new(database);
        repositories
            .upsert_project(ProjectionProject {
                project_id: "project".to_owned(),
                title: "project".to_owned(),
                workspace_root: main.to_string_lossy().into_owned(),
                default_model_selection: None,
                scripts: json!([]),
                worktree_discovery: json!({}),
                worktree_repository_key: None,
                created_at: "2026-10-09T00:00:00Z".to_owned(),
                updated_at: "2026-10-09T00:00:00Z".to_owned(),
                deleted_at: None,
                repository_identity: None,
            })
            .await
            .unwrap();
        let registry = WorkspaceAvailabilityRegistry::new();
        assert!(
            registry
                .mark_unavailable(WorkspaceLossTransition {
                    thread_id: "thread-missing".to_owned(),
                    repository_key: "repository".to_owned(),
                    generation: 1,
                    path: root.join("gone"),
                    availability: AdoptedWorktreeAvailability::MissingRegistered,
                })
                .await
                .unwrap()
        );
        let removal = registry
            .mark_removing("thread-removing", &root.join("going"))
            .await
            .unwrap();
        registry.backdate_for_test("thread-removing", 11 * MINUTE);
        let catalog = WorktreeCatalogService::new_with_availability_registry(
            Arc::new(repositories.clone()),
            Arc::new(GitRepository::with_runner_for_test(Arc::new(HangingGit))),
            registry,
        );
        (
            AuditSources {
                catalog,
                repositories,
            },
            removal,
        )
    }

    #[tokio::test]
    async fn a_hung_repository_leaves_only_its_comparison_out() {
        let root = tempfile::tempdir().unwrap();
        let (sources, _removal) = stalled_repository(root.path()).await;
        // A leftover directory the stalled registrations would have to be compared with.
        std::fs::create_dir_all(root.path().join(".bibcode-worktrees/main/orphan")).unwrap();
        let limits = ScanLimits {
            budget: Duration::from_secs(10),
            git: Duration::from_millis(100),
            processes: PROCESS_DEADLINE,
        };
        let started = Instant::now();
        let (facts, coverage) = gather(
            &sources,
            &ProcessAttributionRegistry::new(),
            ScanGuard::acquire(&Arc::default()).unwrap(),
            limits,
            process_cwds,
        )
        .await;
        assert!(started.elapsed() < limits.budget, "the Git deadline held");
        assert!(coverage.processes, "{coverage:?}");
        let found = kinds(&facts);
        assert!(found.contains(&("worktree.missing", vec!["missing-registered".to_owned()])));
        assert!(found.contains(&("worktree.stuck-removal", vec!["removing".to_owned()])));
        assert!(
            !found.iter().any(|(kind, _)| *kind == "worktree.orphan-dir"),
            "{found:?}"
        );

        // A comparison that overruns the whole budget is abandoned; the registry still counts.
        let limits = ScanLimits {
            budget: Duration::from_millis(100),
            git: Duration::from_secs(60),
            processes: PROCESS_DEADLINE,
        };
        let (facts, coverage) = gather(
            &sources,
            &ProcessAttributionRegistry::new(),
            ScanGuard::acquire(&Arc::default()).unwrap(),
            limits,
            process_cwds,
        )
        .await;
        assert!(!coverage.compared);
        let found = kinds(&facts);
        assert!(found.contains(&("worktree.missing", vec!["missing-registered".to_owned()])));
        assert!(found.contains(&("worktree.stuck-removal", vec!["removing".to_owned()])));
    }

    #[tokio::test]
    async fn a_process_step_past_its_deadline_is_unread_and_the_registry_still_counts() {
        let root = tempfile::tempdir().unwrap();
        let (sources, _removal) = stalled_repository(root.path()).await;
        // A working directory on a hung network mount: the read returns only once the scan
        // has given up on it.
        fn stalled(_: Vec<u32>, cancel: &CancellationToken) -> Vec<(PathBuf, u32)> {
            while !cancel.is_cancelled() {
                std::thread::sleep(Duration::from_millis(5));
            }
            vec![(PathBuf::from("/never/reported"), 1)]
        }
        let limits = ScanLimits {
            budget: Duration::from_secs(10),
            git: Duration::from_millis(100),
            processes: Duration::from_millis(50),
        };
        let (facts, coverage) = tokio::time::timeout(
            Duration::from_secs(10),
            gather(
                &sources,
                &ProcessAttributionRegistry::new(),
                ScanGuard::acquire(&Arc::default()).unwrap(),
                limits,
                stalled,
            ),
        )
        .await
        .expect("the process deadline held");
        assert!(!coverage.processes, "{coverage:?}");
        assert!(facts.held_open.is_empty());
        let found = kinds(&facts);
        assert!(found.contains(&("worktree.missing", vec!["missing-registered".to_owned()])));
        assert!(found.contains(&("worktree.stuck-removal", vec!["removing".to_owned()])));
    }

    #[test]
    fn a_removal_failure_keeps_whether_the_client_was_interrupting() {
        let finding = removal_finding("delete-git-worktree", "git-failed", 12, false, true)
            .expect("a git failure is a finding");
        assert_eq!(finding.severity, Severity::Error);
        assert_eq!(finding.evidence["cancelled"], true);
        assert_eq!(finding.fingerprint_fields, ["git-failed"]);
        assert!(
            removal_finding(
                "delete-git-worktree",
                "orchestration-failed",
                12,
                false,
                true
            )
            .is_none()
        );
    }

    #[tokio::test]
    async fn a_tick_that_finds_the_previous_scan_still_running_looks_again_soon() {
        let root = tempfile::tempdir().unwrap();
        let (sources, _removal) = stalled_repository(root.path()).await;
        let detector = WorktreeAuditDetector::new(
            Arc::new(OnceLock::from(sources)),
            ProcessAttributionRegistry::new(),
        );
        // A dropped scan's filesystem work, still holding the permit.
        let lingering = ScanGuard::acquire(&detector.scanning).unwrap();
        let cancel = CancellationToken::new();
        let task = detector.start(cancel.clone());
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(lingering);
        // The next look comes within a second, not ten minutes: a scan takes the permit, and
        // holds it while Git stalls.
        tokio::time::timeout(Duration::from_secs(5), async {
            while !detector.scanning.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the detector scans once the permit is free");
        cancel.cancel();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn a_failed_blocking_step_is_never_a_clean_result() {
        let permit = Arc::new(ScanGuard::acquire(&Arc::default()).unwrap());
        let result: Option<()> = blocking(&permit, &CancellationToken::new(), |_| {
            panic!("filesystem work failed")
        })
        .await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn the_detector_stops_with_the_catalog() {
        let root = tempfile::tempdir().unwrap();
        let (sources, _removal) = stalled_repository(root.path()).await;
        let catalog = sources.catalog.clone();
        let detector = WorktreeAuditDetector::new(
            Arc::new(OnceLock::from(sources)),
            ProcessAttributionRegistry::new(),
        );
        // Never cancelled: only the catalog's shutdown can end the scan, stalled in Git.
        let task = detector.start(CancellationToken::new());
        tokio::time::sleep(Duration::from_millis(50)).await;
        catalog.shutdown().await;
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("the detector stops with the catalog")
            .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_path_resolves_through_its_nearest_existing_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join("link")).unwrap();
        assert_eq!(
            physical(&dir.path().join("link/gone/deeper")),
            std::fs::canonicalize(&real).unwrap().join("gone/deeper")
        );
    }

    #[test]
    fn summaries_redact_the_home_directory_and_fingerprints_carry_no_paths() {
        let home = dirs::home_dir().expect("home dir");
        let path = home.join("work/wt");
        let facts = AuditFacts {
            catalog: vec![CatalogEntryFact {
                path: path.clone(),
                availability: AdoptedWorktreeAvailability::MissingRegistered,
            }],
            base_dirs: vec![home.join("work/orphan")],
            ..AuditFacts::default()
        };
        let findings = audit(&facts);
        assert_eq!(findings.len(), 2);
        let home = home.to_string_lossy();
        for finding in findings {
            assert!(
                !finding.summary.contains(home.as_ref()),
                "{}",
                finding.summary
            );
            assert!(finding.summary.contains('~'), "{}", finding.summary);
            assert!(!finding.evidence.to_string().contains(home.as_ref()));
            assert!(
                finding
                    .fingerprint_fields
                    .iter()
                    .all(|field| !field.contains('/'))
            );
        }
    }
}
