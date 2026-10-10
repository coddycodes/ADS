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
    emit,
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
/// How soon a detector started before its sources were bound looks again.
const BIND_RETRY: Duration = Duration::from_secs(1);
/// A scan still collecting its facts after this is abandoned.
const SCAN_BUDGET: Duration = Duration::from_secs(30);
/// Entries read from each managed worktree directory.
const MAX_DIR_ENTRIES: usize = 500;
/// The catalog projection's own limit of adopted worktrees per project.
const MAX_PROJECT_ENTRIES: usize = 512;
/// A `removing` guard held longer than this is stuck.
const STUCK_AFTER: Duration = Duration::from_secs(10 * 60);

/// Records one `worktree.remove` or `worktree.removeFromBibCode` outcome: a `worktree.removal`
/// event, and for a failure a `worktree.removal-failed` finding. `outcome` carries the error's
/// kebab-case `reason`. Does nothing while the profiler is off.
pub fn record_removal_outcome(
    mode: &str,
    outcome: Result<(), &str>,
    duration: Duration,
    timed_out_drain: bool,
) {
    if !is_enabled() {
        return;
    }
    let reason = outcome.err();
    let capacity = matches!(reason, Some("cleanup-capacity" | "operation-capacity"));
    let duration_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
    emit(
        "worktree.removal",
        json!({
            "mode": mode,
            "ok": reason.is_none(),
            "reason": reason,
            "durationMs": duration_ms,
            "drainTimedOut": timed_out_drain,
            "capacity": capacity,
        }),
    );
    let Some(reason) = reason else {
        return;
    };
    // A drain timeout answers `session-running`, like a session that was already running.
    let cause = if timed_out_drain {
        "admission-drain-timeout"
    } else {
        reason
    };
    record_finding(finding(
        "worktree.removal-failed",
        if capacity {
            Severity::Warn
        } else {
            Severity::Error
        },
        cause,
        "Worktree removal failed",
        format!("A {mode} removal failed with {cause} after {duration_ms} ms."),
        json!({
            "mode": mode,
            "reason": reason,
            "durationMs": duration_ms,
            "drainTimedOut": timed_out_drain,
        }),
    ));
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
    /// Set while a scan runs, so there is at most one at a time.
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
                    continue;
                };
                // Cancelling drops the scan, which stops its Git commands.
                tokio::select! {
                    () = cancel.cancelled() => return,
                    () = scan(bound, &processes, permit) => {}
                }
            }
        })
    }
}

/// The one-scan flag, released when dropped: by the scan's filesystem work once it ends, or
/// by a scan cancelled before that work started.
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

async fn scan(sources: &AuditSources, processes: &ProcessAttributionRegistry, permit: ScanGuard) {
    let started = Instant::now();
    let Ok(facts) = tokio::time::timeout(SCAN_BUDGET, collect(sources, processes, permit)).await
    else {
        tracing::warn!(
            budget_secs = SCAN_BUDGET.as_secs(),
            "worktree audit scan ran out of time and was abandoned"
        );
        return;
    };
    let findings = audit(&facts);
    emit(
        "worktree.audit",
        json!({
            "entries": facts.catalog.len(),
            "dirs": facts.base_dirs.len(),
            "registrations": facts.git_registrations.len(),
            "processes": facts.held_open.len(),
            "findings": findings.len(),
            "durationMs": u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        }),
    );
    for finding in findings {
        record_finding(finding);
    }
}

/// Reads one scan's facts. Read-only: it never refreshes the catalog, and runs one
/// `git worktree list` per project through the Git runner and its timeout.
async fn collect(
    sources: &AuditSources,
    processes: &ProcessAttributionRegistry,
    permit: ScanGuard,
) -> AuditFacts {
    // Dropping the scan (its budget ran out, or the profiler stopped) cancels its Git commands
    // and stops its filesystem work between operations.
    let cancel = CancellationToken::new();
    let _cancel_on_drop = cancel.clone().drop_guard();
    let mut guards: HashMap<String, (AdoptedWorktreeAvailability, Duration, String)> = sources
        .catalog
        .availability_registry()
        .guarded()
        .into_iter()
        .map(|(guard, held)| (guard.thread_id, (guard.availability, held, guard.path)))
        .collect();
    let projects = sources
        .repositories
        .list_projects()
        .await
        .inspect_err(|error| tracing::warn!(%error, "worktree audit could not list projects"))
        .unwrap_or_default();
    let git = sources.catalog.git_repository();
    let mut entries = Vec::new();
    // Each managed directory, and whether every project creating worktrees in it had its
    // registrations read, with every owner read too. Only those directories are compared,
    // because an owner or registration that could not be read is not one that is absent.
    let mut managed_parents: HashMap<PathBuf, bool> = HashMap::new();
    let mut registrations = Vec::new();
    // A thread can own a worktree in another project's managed directory (two checkouts of
    // one repository), so one unread owner list makes every directory's comparison unsafe.
    let mut all_owners_known = true;
    for project in projects
        .into_iter()
        .filter(|project| project.deleted_at.is_none())
    {
        let projection = sources
            .repositories
            .load_worktree_catalog_projection(project.project_id, MAX_PROJECT_ENTRIES)
            .await
            .ok()
            .flatten();
        all_owners_known &= projection
            .as_ref()
            .is_some_and(|projection| !projection.truncated);
        entries.extend(
            projection
                .into_iter()
                .flat_map(|projection| projection.threads)
                .filter_map(|thread| {
                    let guard = guards
                        .remove(&thread.thread_id)
                        .map(|(availability, held, _)| (availability, held));
                    Some((PathBuf::from(thread.worktree_path?), guard))
                }),
        );
        let Some(git) = &git else {
            continue;
        };
        let root = PathBuf::from(project.workspace_root);
        let registered = match git.worktree_records(&root, &cancel).await {
            Ok((records, _)) => {
                registrations.extend(records.into_iter().map(|record| record.path));
                true
            }
            Err(_) => false,
        };
        if let Some(parent) = git.managed_worktree_parent(&root).await {
            *managed_parents.entry(parent).or_insert(true) &= registered;
        }
    }
    if !all_owners_known {
        managed_parents
            .values_mut()
            .for_each(|complete| *complete = false);
    }
    // Guards that outlive their thread, such as a detached worktree whose cleanup still runs.
    entries.extend(
        guards
            .into_values()
            .map(|(availability, held, path)| (PathBuf::from(path), Some((availability, held)))),
    );
    let registered_pids = processes
        .registered_sources()
        .into_iter()
        .map(|(identity, _)| identity.pid)
        .collect();
    let task_cancel = cancel.clone();
    // The permit moves into the blocking work, which a dropped scan cannot abort: no second
    // scan starts until it ends.
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        resolve(
            entries,
            managed_parents,
            registrations,
            registered_pids,
            &task_cancel,
        )
    })
    .await
    .unwrap_or_default()
}

/// The filesystem half of a scan: resolves every path to its physical spelling, lists the
/// complete managed worktree directories without following links, and reads the working
/// directories of BiBCode's processes. Returns early once `cancel` fires.
fn resolve(
    entries: Vec<(PathBuf, Option<(AdoptedWorktreeAvailability, Duration)>)>,
    managed_parents: HashMap<PathBuf, bool>,
    registrations: Vec<PathBuf>,
    registered_pids: Vec<u32>,
    cancel: &CancellationToken,
) -> AuditFacts {
    let mut facts = AuditFacts::default();
    for (path, guard) in entries.into_iter().take_while(|_| !cancel.is_cancelled()) {
        let path = physical(&path);
        if let Some((AdoptedWorktreeAvailability::Removing, held)) = guard {
            facts.removing_since.push((path.clone(), held));
        }
        facts.catalog.push(CatalogEntryFact {
            path,
            availability: guard.map_or(AdoptedWorktreeAvailability::Present, |(state, _)| state),
        });
    }
    let mut parents: HashMap<PathBuf, bool> = HashMap::new();
    for (parent, complete) in managed_parents
        .into_iter()
        .take_while(|_| !cancel.is_cancelled())
    {
        *parents.entry(physical(&parent)).or_insert(true) &= complete;
    }
    for (parent, _) in parents.iter().filter(|(_, complete)| **complete) {
        // A managed directory that does not exist yet has nothing to list.
        let Ok(listing) = std::fs::read_dir(parent) else {
            continue;
        };
        facts.base_dirs.extend(
            listing
                .take(MAX_DIR_ENTRIES)
                .take_while(|_| !cancel.is_cancelled())
                .filter_map(Result::ok)
                // `file_type` does not follow links, so a link is never listed as a directory.
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                .map(|entry| entry.path()),
        );
    }
    for path in registrations
        .into_iter()
        .take_while(|_| !cancel.is_cancelled())
    {
        let path = physical(&path);
        let managed = path
            .parent()
            .is_some_and(|parent| parents.get(parent) == Some(&true));
        facts.git_registrations.insert(path, managed);
    }
    if cancel.is_cancelled() {
        return facts;
    }
    // Registered roots, and the descendants they started (a build under a terminal, say).
    let mut pids: HashSet<u32> = descendants_of(Pid::from_u32(std::process::id()))
        .into_iter()
        .map(|(pid, _)| pid.as_u32())
        .collect();
    pids.extend(registered_pids);
    // ponytail: a registration can outlive its process, so a reused pid is read as well; it
    // matters only if that process also works inside a removed worktree.
    facts.held_open = pids
        .into_iter()
        .take_while(|_| !cancel.is_cancelled())
        .filter_map(|pid| Some((process_cwd(pid)?, pid)))
        .collect();
    facts
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

#[cfg(target_os = "linux")]
fn process_cwd(pid: u32) -> Option<PathBuf> {
    use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

    let cwd = std::fs::read_link(format!("/proc/{pid}/cwd")).ok()?;
    // A removed directory reads as "<path> (deleted)".
    Some(
        match cwd.as_os_str().as_bytes().strip_suffix(b" (deleted)") {
            Some(path) => PathBuf::from(OsStr::from_bytes(path)),
            None => cwd,
        },
    )
}

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
    (!path.is_empty()).then(|| PathBuf::from(OsStr::from_bytes(&path)))
}

/// Other platforms report no working directories, so they have no held-open findings.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_cwd(_pid: u32) -> Option<PathBuf> {
    None
}

/// One adopted worktree of the catalog.
#[derive(Clone, Debug)]
pub struct CatalogEntryFact {
    pub path: PathBuf,
    pub availability: AdoptedWorktreeAvailability,
}

/// What one scan observed. Paths are compared as they are, so the scan resolves them first.
#[derive(Debug, Default)]
pub struct AuditFacts {
    pub catalog: Vec<CatalogEntryFact>,
    /// The directories found one level under the managed worktree directories.
    pub base_dirs: Vec<PathBuf>,
    /// Every path Git registers as a worktree, and whether it lies in a managed worktree
    /// directory.
    pub git_registrations: HashMap<PathBuf, bool>,
    /// Each catalog path held in `removing`, and for how long.
    pub removing_since: Vec<(PathBuf, Duration)>,
    /// The working directory of each process BiBCode owns, with its pid.
    pub held_open: Vec<(PathBuf, u32)>,
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
    for (registered, managed) in &facts.git_registrations {
        if *managed && !owned.contains(registered.as_path()) {
            let path = redact(registered);
            findings.push(finding(
                "worktree.registration-mismatch",
                Severity::Warn,
                "unowned-registration",
                "Worktree registration without an owner",
                format!(
                    "Git registers {path} in a managed worktree directory, but no catalog \
                     entry owns it."
                ),
                json!({ "path": path }),
            ));
        }
    }
    for (cwd, pid) in &facts.held_open {
        let held = facts.catalog.iter().find_map(|entry| {
            guard_reason(entry.availability)
                .filter(|_| cwd.starts_with(&entry.path))
                .map(|reason| (entry, reason))
        });
        if let Some((entry, reason)) = held {
            let path = redact(&entry.path);
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
    fn a_managed_registration_without_an_entry_is_a_mismatch() {
        let facts = AuditFacts {
            catalog: vec![entry(
                "/base/repo/owned",
                AdoptedWorktreeAvailability::Present,
            )],
            git_registrations: HashMap::from([
                (PathBuf::from("/base/repo/owned"), true),
                (PathBuf::from("/base/repo/leaked"), true),
                // The user's own worktree, outside the managed directories.
                (PathBuf::from("/elsewhere/own"), false),
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
    fn a_process_inside_a_removed_or_removing_worktree_holds_it_open() {
        let facts = AuditFacts {
            catalog: vec![
                entry("/wt/going", AdoptedWorktreeAvailability::Removing),
                entry("/wt/gone", AdoptedWorktreeAvailability::MissingUnregistered),
                entry("/wt/live", AdoptedWorktreeAvailability::Present),
            ],
            held_open: vec![
                (PathBuf::from("/wt/going/src"), 41),
                (PathBuf::from("/wt/gone"), 42),
                (PathBuf::from("/wt/live"), 43),
                (PathBuf::from("/wt/going-other"), 44),
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
                    serde_json::json!(42)
                ),
            ]
        );
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
