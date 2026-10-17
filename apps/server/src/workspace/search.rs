use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::ffi::OsString;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::git::{OutputPolicy, ProcessError, ProcessOutput, ProcessRequest, ProcessRunner};

use super::WorkspaceError;
use super::paths::to_posix;

const ENTRY_OVERHEAD_BYTES: usize = 24;
const GIT_SCAN_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct WorkspaceIndexPhase {
    pub(super) operation: &'static str,
    pub(super) phase: &'static str,
    pub(super) elapsed_ms: u64,
    pub(super) entry_count: usize,
    pub(super) cache_outcome: &'static str,
}

#[cfg(test)]
pub(super) type WorkspaceIndexPhaseSink = tokio::sync::mpsc::UnboundedSender<WorkspaceIndexPhase>;

pub(super) fn emit_index_phase(
    #[cfg(test)] sink: Option<&WorkspaceIndexPhaseSink>,
    operation: &'static str,
    phase: &'static str,
    started: Instant,
    entry_count: usize,
    cache_outcome: &'static str,
) {
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    tracing::debug!(
        operation,
        phase,
        elapsed_ms,
        entry_count,
        cache_outcome,
        "workspace index phase completed"
    );
    #[cfg(test)]
    if let Some(sink) = sink {
        let _ = sink.send(WorkspaceIndexPhase {
            operation,
            phase,
            elapsed_ms,
            entry_count,
            cache_outcome,
        });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceEntry {
    pub path: String,
    pub kind: EntryKind,
    #[serde(default, skip_serializing_if = "is_false")]
    pub ignored: bool,
}

fn is_false(value: &bool) -> bool {
    !value
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResult {
    pub entries: Vec<WorkspaceEntry>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct SearchLimits {
    pub max_entries: usize,
    pub max_memory_bytes: usize,
    pub max_path_bytes: usize,
}

impl Default for SearchLimits {
    fn default() -> Self {
        Self {
            max_entries: 25_000,
            max_memory_bytes: 16 * 1024 * 1024,
            max_path_bytes: 4096,
        }
    }
}

#[derive(Default)]
struct SearchSnapshot {
    entries: Vec<WorkspaceEntry>,
    memory_bytes: usize,
    truncated: bool,
    /// The directories change detection stats (see `BoundedEntries::swept_directories`).
    directories: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GitListedPathKind {
    Cached,
    Deleted,
    Untracked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitListedPath {
    path: String,
    kind: GitListedPathKind,
}

fn parse_main_listing(output: &str) -> Option<Vec<GitListedPath>> {
    if output.is_empty() {
        return Some(Vec::new());
    }
    let records = output.strip_suffix('\0')?;
    if records.is_empty() {
        return None;
    }
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut paths: Vec<GitListedPath> = Vec::new();
    for record in records.split('\0') {
        let (tag, path) = record.split_once(' ')?;
        if path.is_empty() {
            return None;
        }
        let kind = match tag {
            "H" | "S" => GitListedPathKind::Cached,
            "R" => GitListedPathKind::Deleted,
            "?" => GitListedPathKind::Untracked,
            _ => return None,
        };
        if let Some(index) = seen.get(path).copied() {
            let existing = paths[index].kind;
            match (existing, kind) {
                (left, right) if left == right => {}
                (GitListedPathKind::Cached, GitListedPathKind::Deleted) => {
                    paths[index].kind = GitListedPathKind::Deleted;
                }
                (GitListedPathKind::Deleted, GitListedPathKind::Cached) => {}
                _ => return None,
            }
            continue;
        }
        seen.insert(path.to_owned(), paths.len());
        paths.push(GitListedPath {
            path: path.to_owned(),
            kind,
        });
    }
    Some(paths)
}

fn parse_ignored_listing(output: &str) -> Option<Vec<String>> {
    if output.is_empty() {
        return Some(Vec::new());
    }
    let records = output.strip_suffix('\0')?;
    let mut seen = HashSet::new();
    let mut paths = Vec::new();
    for path in records.split('\0') {
        if path.is_empty() {
            return None;
        }
        if seen.insert(path.to_owned()) {
            paths.push(path.to_owned());
        }
    }
    Some(paths)
}

type BoxWorkspaceGitCommandFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ProcessOutput, ProcessError>> + Send + 'a>>;

trait WorkspaceGitCommandRunner: Send + Sync {
    fn run<'a>(
        &'a self,
        request: ProcessRequest,
        cancellation: &'a CancellationToken,
    ) -> BoxWorkspaceGitCommandFuture<'a>;
}

impl WorkspaceGitCommandRunner for ProcessRunner {
    fn run<'a>(
        &'a self,
        request: ProcessRequest,
        cancellation: &'a CancellationToken,
    ) -> BoxWorkspaceGitCommandFuture<'a> {
        Box::pin(ProcessRunner::run(self, request, cancellation))
    }
}

#[derive(Clone)]
pub struct WorkspaceSearchIndex {
    root: PathBuf,
    limits: SearchLimits,
    snapshot: Arc<RwLock<SearchSnapshot>>,
    #[cfg(test)]
    phase_sink: Option<WorkspaceIndexPhaseSink>,
}

impl WorkspaceSearchIndex {
    pub fn new(root: PathBuf, limits: SearchLimits) -> Self {
        Self {
            root,
            limits,
            snapshot: Arc::new(RwLock::new(SearchSnapshot::default())),
            #[cfg(test)]
            phase_sink: None,
        }
    }

    #[cfg(test)]
    pub(super) fn with_phase_sink(mut self, sink: Option<WorkspaceIndexPhaseSink>) -> Self {
        self.phase_sink = sink;
        self
    }

    #[cfg(test)]
    fn phase_sink(&self) -> Option<&WorkspaceIndexPhaseSink> {
        self.phase_sink.as_ref()
    }

    pub async fn refresh(&self, cancellation: CancellationToken) -> Result<(), WorkspaceError> {
        if cancellation.is_cancelled() {
            return Err(WorkspaceError::Cancelled);
        }
        let root = self.root.clone();
        let limits = self.limits;
        let scan_cancel = cancellation.clone();
        let scanned = if let Some(snapshot) = scan_git(
            &root,
            limits,
            &scan_cancel,
            #[cfg(test)]
            self.phase_sink(),
        )
        .await?
        {
            snapshot
        } else {
            let started = Instant::now();
            let scanned = tokio::task::spawn_blocking(move || scan(&root, limits, &scan_cancel))
                .await
                .map_err(|error| {
                    WorkspaceError::operation("scan", &self.root, std::io::Error::other(error))
                })?;
            let cache_outcome = match &scanned {
                Ok(_) => "build",
                Err(WorkspaceError::Cancelled) => "cancelled",
                Err(_) => "error",
            };
            emit_index_phase(
                #[cfg(test)]
                self.phase_sink(),
                "WorkspaceSearchIndex.filesystemSnapshot",
                "filesystem_walk",
                started,
                scanned
                    .as_ref()
                    .map_or(0, |snapshot| snapshot.entries.len())
                    .min(limits.max_entries),
                cache_outcome,
            );
            scanned?
        };
        if cancellation.is_cancelled() {
            return Err(WorkspaceError::Cancelled);
        }
        *self.snapshot.write().await = scanned;
        Ok(())
    }

    pub async fn list(&self, limit: Option<usize>) -> SearchResult {
        let snapshot = self.snapshot.read().await;
        let effective_limit = limit.unwrap_or(snapshot.entries.len());
        SearchResult {
            truncated: snapshot.truncated || snapshot.entries.len() > effective_limit,
            entries: snapshot
                .entries
                .iter()
                .take(effective_limit)
                .cloned()
                .collect(),
        }
    }

    /// Whether `other` lists exactly what this index lists: the same entries in the same order, and
    /// the same truncation.
    pub async fn same_entries(&self, other: &Self) -> bool {
        if Arc::ptr_eq(&self.snapshot, &other.snapshot) {
            return true;
        }
        let (left, right) = (self.snapshot.read().await, other.snapshot.read().await);
        left.truncated == right.truncated && left.entries == right.entries
    }

    /// Workspace-relative paths of every directory in the current snapshot.
    ///
    /// Change detection stats these directories rather than walking the tree: the snapshot is a set
    /// of paths, and adding, renaming, or removing an entry moves its parent directory's mtime.
    /// Deriving the set from the snapshot also keeps the sweep inside the same bounds the scan
    /// already spent, including which ignored directories it walked into.
    pub async fn directory_paths(&self) -> Vec<String> {
        self.snapshot.read().await.directories.clone()
    }

    pub async fn search(&self, query: &str, limit: usize) -> SearchResult {
        let normalized = query
            .trim()
            .trim_start_matches(['@', '.', '/'])
            .to_lowercase();
        let snapshot = self.snapshot.read().await;
        let mut matches = snapshot
            .entries
            .iter()
            .filter_map(|entry| fuzzy_score(&entry.path, &normalized).map(|score| (score, entry)))
            .collect::<Vec<_>>();
        matches.sort_by(|(left_score, left), (right_score, right)| {
            left_score
                .cmp(right_score)
                .then_with(|| entry_kind_rank(left.kind).cmp(&entry_kind_rank(right.kind)))
                .then_with(|| left.path.cmp(&right.path))
        });
        let effective_limit = limit.max(1);
        SearchResult {
            truncated: snapshot.truncated || matches.len() > effective_limit,
            entries: matches
                .into_iter()
                .take(effective_limit)
                .map(|(_, entry)| entry.clone())
                .collect(),
        }
    }

    pub async fn memory_bytes(&self) -> usize {
        self.snapshot.read().await.memory_bytes
    }

    pub(super) async fn entry_count(&self) -> usize {
        self.snapshot.read().await.entries.len()
    }
}

async fn scan_git(
    root: &Path,
    limits: SearchLimits,
    cancellation: &CancellationToken,
    #[cfg(test)] sink: Option<&WorkspaceIndexPhaseSink>,
) -> Result<Option<SearchSnapshot>, WorkspaceError> {
    scan_git_with_runner(
        root,
        limits,
        cancellation,
        &ProcessRunner,
        #[cfg(test)]
        sink,
    )
    .await
}

async fn scan_git_with_runner<R: WorkspaceGitCommandRunner + ?Sized>(
    root: &Path,
    limits: SearchLimits,
    cancellation: &CancellationToken,
    runner: &R,
    #[cfg(test)] sink: Option<&WorkspaceIndexPhaseSink>,
) -> Result<Option<SearchSnapshot>, WorkspaceError> {
    let git_started = Instant::now();
    let command_cancellation = cancellation.child_token();
    let main = async {
        let result =
            run_git_main_listing(root, limits.max_memory_bytes, &command_cancellation, runner)
                .await;
        if !matches!(result, Ok(Some(_))) {
            command_cancellation.cancel();
        }
        result
    };
    let ignored = async {
        let result =
            run_git_ignored_listing(root, limits.max_memory_bytes, &command_cancellation, runner)
                .await;
        if !matches!(result, Ok(Some(_))) {
            command_cancellation.cancel();
        }
        result
    };
    let (main, ignored_roots) = tokio::join!(main, ignored);
    if cancellation.is_cancelled() {
        emit_index_phase(
            #[cfg(test)]
            sink,
            "WorkspaceSearchIndex.gitSnapshot",
            "git_snapshot",
            git_started,
            0,
            "cancelled",
        );
        return Err(WorkspaceError::Cancelled);
    }
    let (Ok(Some(listed)), Ok(Some(ignored_roots))) = (main, ignored_roots) else {
        emit_index_phase(
            #[cfg(test)]
            sink,
            "WorkspaceSearchIndex.gitSnapshot",
            "git_snapshot",
            git_started,
            0,
            "fallback",
        );
        return Ok(None);
    };
    emit_index_phase(
        #[cfg(test)]
        sink,
        "WorkspaceSearchIndex.gitSnapshot",
        "git_snapshot",
        git_started,
        listed
            .len()
            .saturating_add(ignored_roots.len())
            .min(limits.max_entries),
        "build",
    );

    let mut ignored_directories = ignored_roots
        .iter()
        .filter(|path| path.ends_with('/'))
        .map(|path| path.trim_end_matches('/').to_owned())
        .collect::<Vec<_>>();
    ignored_directories.sort();
    let mut candidates = BTreeMap::new();
    for listed_path in listed
        .iter()
        .filter(|listed_path| listed_path.kind != GitListedPathKind::Deleted)
    {
        let path = &listed_path.path;
        for (separator, _) in path.match_indices('/') {
            candidates
                .entry(path[..separator].to_owned())
                .or_insert((EntryKind::Directory, false));
        }
        candidates.insert(path.to_owned(), (EntryKind::File, false));
    }
    for ignored_path in &ignored_roots {
        let path = ignored_path.trim_end_matches('/');
        for (separator, _) in path.match_indices('/') {
            candidates
                .entry(path[..separator].to_owned())
                .or_insert((EntryKind::Directory, false));
        }
        candidates
            .entry(path.to_owned())
            .and_modify(|candidate| candidate.1 = true)
            .or_insert((
                if ignored_path.ends_with('/') {
                    EntryKind::Directory
                } else {
                    EntryKind::File
                },
                true,
            ));
    }
    let ignored_directory_prefixes = ignored_directories.clone();
    let remaining = limits.max_entries.saturating_sub(candidates.len());
    let scan_root = root.to_path_buf();
    let scan_cancellation = cancellation.clone();
    let ignored_started = Instant::now();
    let ignored_contents = tokio::task::spawn_blocking(move || {
        scan_ignored_directory_contents(
            &scan_root,
            ignored_directories,
            remaining,
            &scan_cancellation,
        )
    })
    .await;
    let ignored_contents = match ignored_contents {
        Ok(ignored_contents) => ignored_contents,
        Err(error) => {
            emit_index_phase(
                #[cfg(test)]
                sink,
                "WorkspaceSearchIndex.gitSnapshot",
                "ignored_walk",
                ignored_started,
                0,
                "error",
            );
            return Err(WorkspaceError::operation(
                "scan-ignored-directories",
                root,
                std::io::Error::other(error),
            ));
        }
    };
    if cancellation.is_cancelled() {
        emit_index_phase(
            #[cfg(test)]
            sink,
            "WorkspaceSearchIndex.gitSnapshot",
            "ignored_walk",
            ignored_started,
            ignored_contents.len().min(limits.max_entries),
            "cancelled",
        );
        return Err(WorkspaceError::Cancelled);
    }
    emit_index_phase(
        #[cfg(test)]
        sink,
        "WorkspaceSearchIndex.gitSnapshot",
        "ignored_walk",
        ignored_started,
        ignored_contents.len().min(limits.max_entries),
        "build",
    );
    for (path, kind) in ignored_contents {
        candidates
            .entry(path)
            .and_modify(|candidate| candidate.1 = true)
            .or_insert((kind, true));
    }
    // `git ls-files` reports files, so directories are only inferred from the paths of the files
    // inside them. A directory holding no files is therefore invisible to the listing above even
    // though it exists on disk, which is what "New Folder…" creates. Walk for directories to close
    // that gap; the walk prunes at ignored roots, so its cost tracks the directory count rather
    // than the file count.
    let remaining_directories = limits.max_entries.saturating_sub(candidates.len());
    let scan_root = root.to_path_buf();
    let scan_cancellation = cancellation.clone();
    let ignored_prefixes = ignored_directory_prefixes.clone();
    let directories_started = Instant::now();
    let walked_directories = tokio::task::spawn_blocking(move || {
        scan_directories(
            &scan_root,
            &ignored_prefixes,
            remaining_directories,
            &scan_cancellation,
        )
    })
    .await;
    let walked_directories = match walked_directories {
        Ok(walked_directories) => walked_directories,
        Err(error) => {
            emit_index_phase(
                #[cfg(test)]
                sink,
                "WorkspaceSearchIndex.gitSnapshot",
                "directory_walk",
                directories_started,
                0,
                "error",
            );
            return Err(WorkspaceError::operation(
                "scan-directories",
                root,
                std::io::Error::other(error),
            ));
        }
    };
    if cancellation.is_cancelled() {
        emit_index_phase(
            #[cfg(test)]
            sink,
            "WorkspaceSearchIndex.gitSnapshot",
            "directory_walk",
            directories_started,
            walked_directories.len().min(limits.max_entries),
            "cancelled",
        );
        return Err(WorkspaceError::Cancelled);
    }
    emit_index_phase(
        #[cfg(test)]
        sink,
        "WorkspaceSearchIndex.gitSnapshot",
        "directory_walk",
        directories_started,
        walked_directories.len().min(limits.max_entries),
        "build",
    );
    for path in walked_directories {
        candidates
            .entry(path)
            .or_insert((EntryKind::Directory, false));
    }
    let mut entries = BoundedEntries::default();
    for (path, (kind, _)) in &candidates {
        if *kind == EntryKind::Directory {
            entries.sweep_directory(path, limits);
        }
    }
    for (path, (kind, ignored)) in candidates {
        entries.insert(path, kind, ignored, limits);
    }
    Ok(Some(entries.into_snapshot(limits)))
}

/// Relative paths of every directory under `root`, pruning `.git` and ignored roots.
///
/// Only directories are recorded. Ignored roots are skipped because their contents are already
/// walked separately, and descending into them here would pay for `node_modules` twice.
fn scan_directories(
    root: &Path,
    ignored_directories: &[String],
    limit: usize,
    cancellation: &CancellationToken,
) -> Vec<String> {
    let mut directories = Vec::new();
    let mut queue = VecDeque::from([root.to_path_buf()]);
    while let Some(directory) = queue.pop_front() {
        if cancellation.is_cancelled() || directories.len() >= limit {
            break;
        }
        let Ok(children) = std::fs::read_dir(&directory) else {
            continue;
        };
        for child in children.filter_map(Result::ok) {
            if cancellation.is_cancelled() || directories.len() >= limit {
                break;
            }
            let Ok(file_type) = child.file_type() else {
                continue;
            };
            if !file_type.is_dir() || file_type.is_symlink() {
                continue;
            }
            let path = child.path();
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let relative = to_posix(relative);
            if relative == ".git"
                || relative.starts_with(".git/")
                || ignored_directories.iter().any(|ignored| {
                    relative == *ignored || relative.starts_with(&format!("{ignored}/"))
                })
            {
                continue;
            }
            queue.push_back(path);
            directories.push(relative);
        }
    }
    directories
}

fn scan_ignored_directory_contents(
    root: &Path,
    ignored_directories: Vec<String>,
    limit: usize,
    cancellation: &CancellationToken,
) -> Vec<(String, EntryKind)> {
    let mut entries = Vec::new();
    let mut queue = VecDeque::from(ignored_directories);
    while let Some(relative_directory) = queue.pop_front() {
        if cancellation.is_cancelled() || entries.len() >= limit {
            break;
        }
        let Ok(children) = std::fs::read_dir(root.join(&relative_directory)) else {
            continue;
        };
        let mut children = children
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_type().ok().map(|file_type| (entry, file_type)))
            .filter(|(_, file_type)| !file_type.is_symlink())
            .collect::<Vec<_>>();
        children.sort_by(|(left, left_type), (right, right_type)| {
            left_type
                .is_file()
                .cmp(&right_type.is_file())
                .then_with(|| left.file_name().cmp(&right.file_name()))
        });
        for (child, file_type) in children {
            if cancellation.is_cancelled() || entries.len() >= limit {
                break;
            }
            let path = child.path();
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let relative = to_posix(relative);
            let kind = if file_type.is_dir() {
                queue.push_back(relative.clone());
                EntryKind::Directory
            } else if file_type.is_file() {
                EntryKind::File
            } else {
                continue;
            };
            entries.push((relative, kind));
        }
    }
    entries
}

async fn run_git_main_listing<R: WorkspaceGitCommandRunner + ?Sized>(
    root: &Path,
    max_output_bytes: usize,
    cancellation: &CancellationToken,
    runner: &R,
) -> Result<Option<Vec<GitListedPath>>, WorkspaceError> {
    let Some(output) = run_git_ls_files(
        root,
        [
            "-t",
            "--cached",
            "--others",
            "--deleted",
            "--exclude-standard",
        ],
        max_output_bytes,
        cancellation,
        runner,
    )
    .await?
    else {
        return Ok(None);
    };
    Ok(parse_main_listing(&output))
}

async fn run_git_ignored_listing<R: WorkspaceGitCommandRunner + ?Sized>(
    root: &Path,
    max_output_bytes: usize,
    cancellation: &CancellationToken,
    runner: &R,
) -> Result<Option<Vec<String>>, WorkspaceError> {
    let Some(output) = run_git_ls_files(
        root,
        ["--others", "--ignored", "--exclude-standard", "--directory"],
        max_output_bytes,
        cancellation,
        runner,
    )
    .await?
    else {
        return Ok(None);
    };
    Ok(parse_ignored_listing(&output))
}

async fn run_git_ls_files<const N: usize, R: WorkspaceGitCommandRunner + ?Sized>(
    root: &Path,
    modes: [&str; N],
    max_output_bytes: usize,
    cancellation: &CancellationToken,
    runner: &R,
) -> Result<Option<String>, WorkspaceError> {
    let mut args = vec![OsString::from("-c"), OsString::from("core.quotePath=false")];
    args.extend([OsString::from("ls-files"), OsString::from("-z")]);
    args.extend(modes.into_iter().map(OsString::from));
    args.push(OsString::from("--"));
    match runner
        .run(
            ProcessRequest {
                operation: "WorkspaceSearchIndex.gitSnapshot".to_owned(),
                command: PathBuf::from("git"),
                args,
                cwd: root.to_path_buf(),
                env: vec![(OsString::from("GIT_OPTIONAL_LOCKS"), OsString::from("0"))],
                stdin: None,
                timeout: GIT_SCAN_TIMEOUT,
                max_output_bytes,
                output_policy: OutputPolicy::Error,
                append_truncation_marker: false,
                allow_non_zero_exit: true,
            },
            cancellation,
        )
        .await
    {
        Ok(output)
            if output.exit_code == 0 && !output.stdout_truncated && !output.stderr_truncated =>
        {
            Ok(Some(output.stdout))
        }
        Ok(_) => Ok(None),
        Err(ProcessError::Cancelled { .. }) => Err(WorkspaceError::Cancelled),
        Err(_) => Ok(None),
    }
}

/// The entry list a scan builds, bounded by `SearchLimits`.
#[derive(Default)]
struct BoundedEntries {
    entries: BTreeMap<String, WorkspaceEntry>,
    /// The directories `entries` lists. Past the entry limit each file takes the place of the first
    /// one, and looking it up here keeps that from scanning the whole list once per file.
    listed_directories: BTreeSet<String>,
    memory_bytes: usize,
    truncated: bool,
    /// Every directory the scan saw, for change detection. Past the entry limit the list keeps
    /// files and evicts directories, so the sweep cannot take its directories from `entries`.
    swept_directories: Vec<String>,
}

impl BoundedEntries {
    fn insert(
        &mut self,
        path: String,
        kind: EntryKind,
        ignored: bool,
        limits: SearchLimits,
    ) -> bool {
        let entry_bytes = path.len().saturating_add(ENTRY_OVERHEAD_BYTES);
        if path.len() > limits.max_path_bytes
            || self.memory_bytes.saturating_add(entry_bytes) > limits.max_memory_bytes
        {
            self.truncated = true;
            return false;
        }
        if self.entries.len() >= limits.max_entries {
            self.truncated = true;
            if kind != EntryKind::File {
                return false;
            }
            let Some(directory_path) = self.listed_directories.pop_first() else {
                return false;
            };
            if let Some(removed) = self.entries.remove(&directory_path) {
                self.memory_bytes = self
                    .memory_bytes
                    .saturating_sub(removed.path.len().saturating_add(ENTRY_OVERHEAD_BYTES));
            }
        }
        self.memory_bytes += entry_bytes;
        if kind == EntryKind::Directory {
            self.listed_directories.insert(path.clone());
        }
        self.entries.insert(
            path.clone(),
            WorkspaceEntry {
                path,
                kind,
                ignored,
            },
        );
        true
    }

    /// Offers `path` for change detection, within the entry list's count and path bounds.
    fn sweep_directory(&mut self, path: &str, limits: SearchLimits) {
        if self.swept_directories.len() < limits.max_entries && path.len() <= limits.max_path_bytes
        {
            self.swept_directories.push(path.to_owned());
        }
    }

    /// Every listed directory is swept. The others are swept while they fit the memory the entry
    /// list left, so the index stays within one memory bound.
    fn into_snapshot(self, limits: SearchLimits) -> SearchSnapshot {
        let mut memory_bytes = self.memory_bytes;
        let mut directories = self.swept_directories;
        directories.retain(|path| {
            if self.listed_directories.contains(path) {
                return false;
            }
            let entry_bytes = path.len().saturating_add(ENTRY_OVERHEAD_BYTES);
            let fits = memory_bytes.saturating_add(entry_bytes) <= limits.max_memory_bytes;
            if fits {
                memory_bytes += entry_bytes;
            }
            fits
        });
        directories.extend(self.listed_directories);
        SearchSnapshot {
            entries: self.entries.into_values().collect(),
            memory_bytes,
            truncated: self.truncated,
            directories,
        }
    }
}

fn entry_kind_rank(kind: EntryKind) -> u8 {
    match kind {
        EntryKind::File => 0,
        EntryKind::Directory => 1,
    }
}

fn scan(
    root: &Path,
    limits: SearchLimits,
    cancellation: &CancellationToken,
) -> Result<SearchSnapshot, WorkspaceError> {
    if !root.is_dir() {
        return Err(WorkspaceError::RootNotDirectory {
            path: root.to_path_buf(),
        });
    }
    let ignore_rules = read_ignore_rules(root);
    let mut entries = BoundedEntries::default();
    let mut stack = vec![root.to_path_buf()];
    while let Some(directory) = stack.pop() {
        if cancellation.is_cancelled() {
            return Err(WorkspaceError::Cancelled);
        }
        let mut children = std::fs::read_dir(&directory)
            .map_err(|error| WorkspaceError::operation("read-directory", &directory, error))?
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        children.sort_by_key(std::fs::DirEntry::file_name);
        for child in children.into_iter().rev() {
            if cancellation.is_cancelled() {
                return Err(WorkspaceError::Cancelled);
            }
            let path = child.path();
            let relative = path
                .strip_prefix(root)
                .map_err(|error| WorkspaceError::InvalidRequest(error.to_string()))?;
            let relative = to_posix(relative);
            let file_type = child
                .file_type()
                .map_err(|error| WorkspaceError::operation("stat", &path, error))?;
            if should_ignore(&relative, file_type.is_dir(), &ignore_rules) || file_type.is_symlink()
            {
                continue;
            }
            let kind = if file_type.is_dir() {
                EntryKind::Directory
            } else if file_type.is_file() {
                EntryKind::File
            } else {
                continue;
            };
            let directory = (kind == EntryKind::Directory).then(|| relative.clone());
            let inserted = entries.insert(relative, kind, false, limits);
            if let Some(directory) = directory.filter(|_| inserted) {
                entries.sweep_directory(&directory, limits);
                stack.push(path);
            }
        }
    }
    Ok(entries.into_snapshot(limits))
}

fn read_ignore_rules(root: &Path) -> HashSet<String> {
    std::fs::read_to_string(root.join(".gitignore"))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with('!'))
        .map(|line| line.trim_end_matches('/').to_owned())
        .collect()
}

fn should_ignore(relative: &str, is_directory: bool, rules: &HashSet<String>) -> bool {
    let first = relative.split('/').next().unwrap_or(relative);
    if matches!(first, ".git" | "node_modules" | ".convex") {
        return true;
    }
    rules.iter().any(|rule| {
        let root_only = rule.starts_with('/');
        let rule = rule.trim_start_matches('/');
        let directory_rule = relative == rule || relative.starts_with(&format!("{rule}/"));
        let basename_rule = !root_only
            && !rule.contains('/')
            && relative.rsplit('/').next().is_some_and(|name| name == rule);
        directory_rule || basename_rule || (is_directory && relative == rule)
    })
}

fn fuzzy_score(path: &str, query: &str) -> Option<(u8, usize)> {
    if query.is_empty() {
        return Some((3, 0));
    }
    let lower = path.to_lowercase();
    let basename = lower.rsplit('/').next().unwrap_or(&lower);
    if basename == query {
        return Some((0, 0));
    }
    if let Some(index) = basename.find(query) {
        return Some((1, index));
    }
    if let Some(index) = lower.find(query) {
        return Some((2, index));
    }
    if let Some(offset) = subsequence_offset(basename, query) {
        return Some((3, offset));
    }
    subsequence_offset(&lower, query).map(|offset| (4, offset))
}

fn subsequence_offset(candidate: &str, query: &str) -> Option<usize> {
    let mut offset = 0;
    for character in query.chars() {
        let found = candidate[offset..].find(character)?;
        offset += found + character.len_utf8();
    }
    Some(offset)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::sync::Semaphore;
    use tokio::time::timeout;

    use super::*;

    const TEST_TIMEOUT: Duration = Duration::from_secs(5);

    #[derive(Clone)]
    struct PausingWorkspaceGitRunner {
        started: Arc<Semaphore>,
        release: Arc<Semaphore>,
        settled: Arc<Semaphore>,
        requests: Arc<Mutex<Vec<ProcessRequest>>>,
    }

    impl PausingWorkspaceGitRunner {
        fn new() -> Self {
            Self {
                started: Arc::new(Semaphore::new(0)),
                release: Arc::new(Semaphore::new(0)),
                settled: Arc::new(Semaphore::new(0)),
                requests: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    impl WorkspaceGitCommandRunner for PausingWorkspaceGitRunner {
        fn run<'a>(
            &'a self,
            request: ProcessRequest,
            cancellation: &'a CancellationToken,
        ) -> BoxWorkspaceGitCommandFuture<'a> {
            self.requests.lock().unwrap().push(request.clone());
            self.started.add_permits(1);
            Box::pin(async move {
                let result = tokio::select! {
                    permit = self.release.acquire() => {
                        permit.expect("release semaphore closed").forget();
                        Ok(ProcessOutput {
                        exit_code: 0,
                        stdout: String::new(),
                        stderr: String::new(),
                        stdout_truncated: false,
                        stderr_truncated: false,
                        })
                    },
                    () = cancellation.cancelled() => Err(ProcessError::Cancelled {
                        operation: request.operation,
                    }),
                };
                self.settled.add_permits(1);
                result
            })
        }
    }

    #[derive(Clone, Copy)]
    enum RejectedGitListing {
        Spawn,
        NonZero,
        Timeout,
        Truncated,
        Malformed,
    }

    #[derive(Clone)]
    struct RejectingWorkspaceGitRunner {
        failure: RejectedGitListing,
        fail_main: bool,
        started: Arc<Semaphore>,
        settled: Arc<Semaphore>,
    }

    impl RejectingWorkspaceGitRunner {
        fn new(failure: RejectedGitListing, fail_main: bool) -> Self {
            Self {
                failure,
                fail_main,
                started: Arc::new(Semaphore::new(0)),
                settled: Arc::new(Semaphore::new(0)),
            }
        }
    }

    impl WorkspaceGitCommandRunner for RejectingWorkspaceGitRunner {
        fn run<'a>(
            &'a self,
            request: ProcessRequest,
            cancellation: &'a CancellationToken,
        ) -> BoxWorkspaceGitCommandFuture<'a> {
            let is_main = request.args.iter().any(|arg| arg == "-t");
            self.started.add_permits(1);
            Box::pin(async move {
                let result = if is_main == self.fail_main {
                    match self.failure {
                        RejectedGitListing::Spawn => Err(ProcessError::Spawn {
                            operation: request.operation,
                            command: "git".to_owned(),
                            source: std::io::Error::new(
                                std::io::ErrorKind::NotFound,
                                "missing git",
                            ),
                        }),
                        RejectedGitListing::Timeout => Err(ProcessError::Timeout {
                            operation: request.operation,
                            timeout_ms: 1,
                        }),
                        RejectedGitListing::NonZero => Ok(process_output(1, "")),
                        RejectedGitListing::Truncated => Ok(ProcessOutput {
                            stdout_truncated: true,
                            ..process_output(0, "")
                        }),
                        RejectedGitListing::Malformed => Ok(process_output(0, "malformed")),
                    }
                } else {
                    cancellation.cancelled().await;
                    Err(ProcessError::Cancelled {
                        operation: request.operation,
                    })
                };
                self.settled.add_permits(1);
                result
            })
        }
    }

    #[derive(Clone)]
    struct DelayedWorkspaceGitRunner {
        delay: Duration,
        started: Arc<AtomicUsize>,
    }

    impl DelayedWorkspaceGitRunner {
        fn new(delay: Duration) -> Self {
            Self {
                delay,
                started: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl WorkspaceGitCommandRunner for DelayedWorkspaceGitRunner {
        fn run<'a>(
            &'a self,
            request: ProcessRequest,
            cancellation: &'a CancellationToken,
        ) -> BoxWorkspaceGitCommandFuture<'a> {
            let is_main = request.args.iter().any(|arg| arg == "-t");
            self.started.fetch_add(1, Ordering::Relaxed);
            Box::pin(async move {
                tokio::select! {
                    () = cancellation.cancelled() => Err(ProcessError::Cancelled {
                        operation: request.operation,
                    }),
                    result = tokio::time::timeout(request.timeout, tokio::time::sleep(self.delay)) => {
                        result.map_err(|_| ProcessError::Timeout {
                            operation: request.operation,
                            timeout_ms: request.timeout.as_millis(),
                        })?;
                        Ok(ProcessOutput {
                            exit_code: 0,
                            stdout: if is_main { "? delayed.txt\0".to_owned() } else { String::new() },
                            stderr: String::new(),
                            stdout_truncated: false,
                            stderr_truncated: false,
                        })
                    },
                }
            })
        }
    }

    fn process_output(exit_code: i32, stdout: &str) -> ProcessOutput {
        ProcessOutput {
            exit_code,
            stdout: stdout.to_owned(),
            stderr: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
        }
    }

    async fn consume_permits(semaphore: &Semaphore, count: u32, checkpoint: &str) {
        timeout(TEST_TIMEOUT, semaphore.acquire_many(count))
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {checkpoint}"))
            .unwrap_or_else(|_| panic!("{checkpoint} semaphore closed"))
            .forget();
    }

    #[test]
    fn tagged_main_listing_distinguishes_cached_deleted_and_untracked() {
        let parsed = parse_main_listing("H tracked.rs\0S sparse.rs\0R deleted.rs\0? new.rs\0")
            .expect("valid tagged listing");

        assert_eq!(parsed[0].kind, GitListedPathKind::Cached);
        assert_eq!(parsed[1].kind, GitListedPathKind::Cached);
        assert_eq!(parsed[2].kind, GitListedPathKind::Deleted);
        assert_eq!(parsed[3].kind, GitListedPathKind::Untracked);
    }

    #[test]
    fn tagged_main_listing_rejects_malformed_unknown_and_ambiguous_records() {
        for invalid in [
            "\0",
            "\0H path.rs\0",
            "H path.rs\0\0",
            "X path.rs\0",
            "H\0",
            "H \0",
            "H path.rs",
        ] {
            assert!(
                parse_main_listing(invalid).is_none(),
                "accepted {invalid:?}"
            );
        }
        for invalid in [
            "H same.rs\0? same.rs\0",
            "? same.rs\0H same.rs\0",
            "S same.rs\0? same.rs\0",
            "? same.rs\0S same.rs\0",
            "R same.rs\0? same.rs\0",
            "? same.rs\0R same.rs\0",
        ] {
            assert!(
                parse_main_listing(invalid).is_none(),
                "accepted {invalid:?}"
            );
        }
    }

    #[test]
    fn tagged_main_listing_resolves_only_deterministic_duplicates() {
        assert!(parse_main_listing("").unwrap().is_empty());
        for (output, kind) in [
            ("H same.rs\0H same.rs\0", GitListedPathKind::Cached),
            ("H same.rs\0S same.rs\0", GitListedPathKind::Cached),
            ("S same.rs\0H same.rs\0", GitListedPathKind::Cached),
            ("H same.rs\0R same.rs\0", GitListedPathKind::Deleted),
            ("R same.rs\0H same.rs\0", GitListedPathKind::Deleted),
            ("S same.rs\0R same.rs\0", GitListedPathKind::Deleted),
            ("R same.rs\0S same.rs\0", GitListedPathKind::Deleted),
            ("R same.rs\0R same.rs\0", GitListedPathKind::Deleted),
            ("? same.rs\0? same.rs\0", GitListedPathKind::Untracked),
        ] {
            assert_eq!(
                parse_main_listing(output).unwrap().as_slice(),
                [GitListedPath {
                    path: "same.rs".to_owned(),
                    kind,
                }],
                "failed {output:?}"
            );
        }
    }

    #[tokio::test]
    async fn git_snapshot_starts_two_processes_concurrently() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().to_path_buf();
        let expected_root = root_path.clone();
        let runner = PausingWorkspaceGitRunner::new();
        let scan_runner = runner.clone();
        let scan = tokio::spawn(async move {
            scan_git_with_runner(
                &root_path,
                SearchLimits::default(),
                &CancellationToken::new(),
                &scan_runner,
                None,
            )
            .await
        });

        consume_permits(&runner.started, 2, "both Git commands to start").await;
        {
            let requests = runner.requests.lock().unwrap();
            let main = requests
                .iter()
                .find(|request| request.args.iter().any(|arg| arg == "-t"))
                .unwrap();
            assert_eq!(
                main.args,
                [
                    "-c",
                    "core.quotePath=false",
                    "ls-files",
                    "-z",
                    "-t",
                    "--cached",
                    "--others",
                    "--deleted",
                    "--exclude-standard",
                    "--",
                ]
                .map(OsString::from)
            );
            assert_git_request_contract(main, &expected_root);
            let ignored = requests
                .iter()
                .find(|request| !request.args.iter().any(|arg| arg == "-t"))
                .unwrap();
            assert_eq!(
                ignored.args,
                [
                    "-c",
                    "core.quotePath=false",
                    "ls-files",
                    "-z",
                    "--others",
                    "--ignored",
                    "--exclude-standard",
                    "--directory",
                    "--",
                ]
                .map(OsString::from)
            );
            assert_git_request_contract(ignored, &expected_root);
        }
        runner.release.add_permits(2);
        assert!(
            timeout(TEST_TIMEOUT, scan)
                .await
                .expect("scan timed out")
                .unwrap()
                .unwrap()
                .is_some()
        );
        consume_permits(&runner.settled, 2, "both Git commands to settle").await;
    }

    #[tokio::test(start_paused = true)]
    async fn git_snapshot_accepts_slow_success_inside_bound_and_falls_back_beyond_it() {
        let root = tempfile::tempdir().unwrap();
        let slow = DelayedWorkspaceGitRunner::new(Duration::from_secs(4));
        let snapshot = scan_git_with_runner(
            root.path(),
            SearchLimits::default(),
            &CancellationToken::new(),
            &slow,
            None,
        )
        .await
        .unwrap();
        assert!(
            snapshot.is_some(),
            "four-second Git reads must remain authoritative"
        );
        assert_eq!(slow.started.load(Ordering::Relaxed), 2);

        let beyond_bound =
            DelayedWorkspaceGitRunner::new(GIT_SCAN_TIMEOUT + Duration::from_secs(1));
        assert!(
            scan_git_with_runner(
                root.path(),
                SearchLimits::default(),
                &CancellationToken::new(),
                &beyond_bound,
                None,
            )
            .await
            .unwrap()
            .is_none(),
            "Git reads beyond the bound must use the filesystem fallback"
        );
        assert_eq!(beyond_bound.started.load(Ordering::Relaxed), 2);
    }

    fn assert_git_request_contract(request: &ProcessRequest, root: &Path) {
        assert_eq!(request.operation, "WorkspaceSearchIndex.gitSnapshot");
        assert_eq!(request.command, PathBuf::from("git"));
        assert_eq!(request.cwd, root);
        assert_eq!(
            request.env,
            [(OsString::from("GIT_OPTIONAL_LOCKS"), OsString::from("0"))]
        );
        assert_eq!(request.stdin, None);
        assert_eq!(request.timeout, Duration::from_secs(10));
        assert_eq!(
            request.max_output_bytes,
            SearchLimits::default().max_memory_bytes
        );
        assert_eq!(request.output_policy, OutputPolicy::Error);
        assert!(!request.append_truncation_marker);
        assert!(request.allow_non_zero_exit);
    }

    #[tokio::test]
    async fn rejected_git_listing_cancels_and_awaits_sibling_before_fallback() {
        for failure in [
            RejectedGitListing::Spawn,
            RejectedGitListing::NonZero,
            RejectedGitListing::Timeout,
            RejectedGitListing::Truncated,
            RejectedGitListing::Malformed,
        ] {
            for fail_main in [true, false] {
                let root = tempfile::tempdir().unwrap();
                let runner = RejectingWorkspaceGitRunner::new(failure, fail_main);

                let snapshot = timeout(
                    TEST_TIMEOUT,
                    scan_git_with_runner(
                        root.path(),
                        SearchLimits::default(),
                        &CancellationToken::new(),
                        &runner,
                        None,
                    ),
                )
                .await
                .expect("rejected Git scan timed out")
                .unwrap();

                assert!(snapshot.is_none());
                consume_permits(&runner.started, 2, "both rejected commands to start").await;
                consume_permits(&runner.settled, 2, "both rejected commands to settle").await;
            }
        }
    }

    #[tokio::test]
    async fn caller_cancellation_after_both_starts_awaits_both_commands() {
        let root = tempfile::tempdir().unwrap();
        let root_path = root.path().to_path_buf();
        let cancellation = CancellationToken::new();
        let scan_cancellation = cancellation.clone();
        let runner = PausingWorkspaceGitRunner::new();
        let scan_runner = runner.clone();
        let scan = tokio::spawn(async move {
            scan_git_with_runner(
                &root_path,
                SearchLimits::default(),
                &scan_cancellation,
                &scan_runner,
                None,
            )
            .await
        });

        consume_permits(&runner.started, 2, "both cancellable commands to start").await;
        cancellation.cancel();
        assert!(matches!(
            timeout(TEST_TIMEOUT, scan)
                .await
                .expect("cancelled scan timed out")
                .unwrap(),
            Err(WorkspaceError::Cancelled)
        ));
        consume_permits(&runner.settled, 2, "both cancelled commands to settle").await;
    }

    #[tokio::test]
    async fn index_phases_match_git_fallback_error_and_cancellation() {
        let root = tempfile::tempdir().unwrap();
        let (fallback_observer, mut fallback_phases) = tokio::sync::mpsc::unbounded_channel();
        let runner = RejectingWorkspaceGitRunner::new(RejectedGitListing::NonZero, true);

        assert!(
            scan_git_with_runner(
                root.path(),
                SearchLimits::default(),
                &CancellationToken::new(),
                &runner,
                Some(&fallback_observer),
            )
            .await
            .unwrap()
            .is_none()
        );
        assert_eq!(
            std::iter::from_fn(|| fallback_phases.try_recv().ok())
                .map(|phase| (phase.phase, phase.cache_outcome))
                .collect::<Vec<_>>(),
            [("git_snapshot", "fallback")]
        );

        let index = WorkspaceSearchIndex::new(root.path().to_path_buf(), SearchLimits::default())
            .with_phase_sink(Some(fallback_observer.clone()));
        index.refresh(CancellationToken::new()).await.unwrap();
        assert_eq!(
            std::iter::from_fn(|| fallback_phases.try_recv().ok())
                .map(|phase| (phase.phase, phase.cache_outcome))
                .collect::<Vec<_>>(),
            [("git_snapshot", "fallback"), ("filesystem_walk", "build"),]
        );

        let missing = root.path().join("missing");
        let index = WorkspaceSearchIndex::new(missing, SearchLimits::default())
            .with_phase_sink(Some(fallback_observer));
        assert!(matches!(
            index.refresh(CancellationToken::new()).await,
            Err(WorkspaceError::RootNotDirectory { .. })
        ));
        assert_eq!(
            std::iter::from_fn(|| fallback_phases.try_recv().ok())
                .map(|phase| (phase.phase, phase.cache_outcome))
                .collect::<Vec<_>>(),
            [("git_snapshot", "fallback"), ("filesystem_walk", "error"),]
        );

        let cancellation = CancellationToken::new();
        let scan_cancellation = cancellation.clone();
        let runner = PausingWorkspaceGitRunner::new();
        let scan_runner = runner.clone();
        let (cancellation_observer, mut cancelled_phases) = tokio::sync::mpsc::unbounded_channel();
        let scan = tokio::spawn(async move {
            scan_git_with_runner(
                root.path(),
                SearchLimits::default(),
                &scan_cancellation,
                &scan_runner,
                Some(&cancellation_observer),
            )
            .await
        });
        consume_permits(&runner.started, 2, "both observed commands to start").await;
        cancellation.cancel();
        assert!(matches!(
            timeout(TEST_TIMEOUT, scan).await.unwrap().unwrap(),
            Err(WorkspaceError::Cancelled)
        ));
        assert_eq!(
            std::iter::from_fn(|| cancelled_phases.try_recv().ok())
                .map(|phase| (phase.phase, phase.cache_outcome))
                .collect::<Vec<_>>(),
            [("git_snapshot", "cancelled")]
        );
    }

    #[tokio::test]
    async fn real_git_snapshot_classifies_tracked_deleted_untracked_and_ignored_paths() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(root.path())
                .status()
                .unwrap()
                .success()
        );
        std::fs::write(
            root.path().join(".gitignore"),
            "ignored-directory/\n*.ignored\n",
        )
        .unwrap();
        std::fs::write(root.path().join("tracked.txt"), "tracked").unwrap();
        std::fs::write(root.path().join("deleted.txt"), "deleted").unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["add", ".gitignore", "tracked.txt", "deleted.txt"])
                .current_dir(root.path())
                .status()
                .unwrap()
                .success()
        );
        std::fs::remove_file(root.path().join("deleted.txt")).unwrap();
        std::fs::write(root.path().join("untracked.txt"), "untracked").unwrap();
        std::fs::write(root.path().join("cache.ignored"), "ignored").unwrap();
        std::fs::create_dir(root.path().join("ignored-directory")).unwrap();
        std::fs::write(
            root.path().join("ignored-directory/generated.txt"),
            "ignored",
        )
        .unwrap();
        std::fs::create_dir(root.path().join("empty-directory")).unwrap();

        let index = WorkspaceSearchIndex::new(root.path().to_path_buf(), SearchLimits::default());
        index.refresh(CancellationToken::new()).await.unwrap();
        let entries = index.list(None).await.entries;

        for path in ["tracked.txt", "untracked.txt"] {
            assert_eq!(
                entries.iter().find(|entry| entry.path == path),
                Some(&WorkspaceEntry {
                    path: path.to_owned(),
                    kind: EntryKind::File,
                    ignored: false,
                })
            );
        }
        assert!(!entries.iter().any(|entry| entry.path == "deleted.txt"));
        for path in ["cache.ignored", "ignored-directory/generated.txt"] {
            assert!(
                entries
                    .iter()
                    .any(|entry| entry.path == path && entry.ignored),
                "missing ignored {path}: {entries:?}"
            );
        }
        assert!(entries.iter().any(|entry| {
            entry.path == "empty-directory" && entry.kind == EntryKind::Directory && !entry.ignored
        }));
        assert!(!entries.iter().any(|entry| entry.path.starts_with(".git/")));
    }

    #[tokio::test]
    async fn repository_snapshot_exposes_ignored_roots_and_their_contents() {
        let root = tempfile::tempdir().unwrap();
        let init = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(root.path())
            .status()
            .unwrap();
        assert!(init.success());
        std::fs::write(root.path().join(".gitignore"), "ignored-*/\n").unwrap();
        std::fs::write(root.path().join("tracked.txt"), "tracked").unwrap();
        std::fs::write(root.path().join("untracked.txt"), "untracked").unwrap();
        std::fs::create_dir(root.path().join("ignored-cache")).unwrap();
        std::fs::write(root.path().join("ignored-cache/generated.txt"), "generated").unwrap();
        let add = std::process::Command::new("git")
            .args(["add", ".gitignore", "tracked.txt"])
            .current_dir(root.path())
            .status()
            .unwrap();
        assert!(add.success());

        let index = WorkspaceSearchIndex::new(root.path().to_path_buf(), SearchLimits::default());
        index.refresh(CancellationToken::new()).await.unwrap();
        let entries = index.list(None).await.entries;
        let paths = entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect::<Vec<_>>();

        assert!(paths.contains(&"tracked.txt"));
        assert!(paths.contains(&"untracked.txt"));
        let ignored = entries
            .iter()
            .find(|entry| entry.path == "ignored-cache")
            .expect("ignored directory should be listed");
        assert_eq!(serde_json::to_value(ignored).unwrap()["ignored"], true);
        let ignored_file = entries
            .iter()
            .find(|entry| entry.path == "ignored-cache/generated.txt")
            .expect("ignored directory contents should be listed");
        assert_eq!(serde_json::to_value(ignored_file).unwrap()["ignored"], true);
    }

    #[tokio::test]
    async fn repository_snapshot_keeps_bounded_ignored_directory_children() {
        let root = tempfile::tempdir().unwrap();
        let init = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(root.path())
            .status()
            .unwrap();
        assert!(init.success());
        std::fs::write(root.path().join(".gitignore"), "/packages/\n").unwrap();
        for directory in ["packages/alpha", "packages/bravo"] {
            std::fs::create_dir_all(root.path().join(directory)).unwrap();
            std::fs::write(root.path().join(directory).join("generated.txt"), "").unwrap();
        }
        let add = std::process::Command::new("git")
            .args(["add", ".gitignore"])
            .current_dir(root.path())
            .status()
            .unwrap();
        assert!(add.success());

        let index = WorkspaceSearchIndex::new(
            root.path().to_path_buf(),
            SearchLimits {
                max_entries: 4,
                max_memory_bytes: usize::MAX,
                max_path_bytes: usize::MAX,
            },
        );
        index.refresh(CancellationToken::new()).await.unwrap();
        let paths = index
            .list(None)
            .await
            .entries
            .into_iter()
            .map(|entry| entry.path)
            .collect::<Vec<_>>();

        assert!(paths.contains(&"packages".to_owned()));
        assert!(paths.contains(&"packages/alpha".to_owned()));
        assert!(paths.contains(&"packages/bravo".to_owned()));
    }

    #[tokio::test]
    async fn filesystem_snapshot_preserves_nested_directory_excluded_only_at_root() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".gitignore"), "/prps/\n").unwrap();
        for directory in ["prps", "docs/prps"] {
            std::fs::create_dir_all(root.path().join(directory)).unwrap();
            std::fs::write(root.path().join(directory).join("plan.md"), "").unwrap();
        }

        let index = WorkspaceSearchIndex::new(root.path().to_path_buf(), SearchLimits::default());
        index.refresh(CancellationToken::new()).await.unwrap();
        let paths = index
            .list(None)
            .await
            .entries
            .into_iter()
            .map(|entry| entry.path)
            .collect::<Vec<_>>();

        assert!(!paths.iter().any(|path| path.starts_with("prps")));
        assert!(paths.contains(&"docs/prps".to_owned()));
        assert!(paths.contains(&"docs/prps/plan.md".to_owned()));
    }

    /// Past the entry limit a file takes the place of the first directory still listed, while
    /// change detection keeps every directory the scan saw.
    #[tokio::test]
    async fn a_full_index_trades_its_first_directories_for_files() {
        for git in [false, true] {
            let root = tempfile::tempdir().unwrap();
            if git {
                let status = std::process::Command::new("git")
                    .args(["init", "-q"])
                    .current_dir(root.path())
                    .status()
                    .unwrap();
                assert!(status.success());
            }
            for path in ["a/x.txt", "b/y.txt", "c/z.txt"] {
                let path = root.path().join(path);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, "").unwrap();
            }
            let index = WorkspaceSearchIndex::new(
                root.path().to_path_buf(),
                SearchLimits {
                    max_entries: 4,
                    ..SearchLimits::default()
                },
            );
            index.refresh(CancellationToken::new()).await.unwrap();
            let listed = index.list(None).await;
            let paths = listed
                .entries
                .into_iter()
                .map(|entry| entry.path)
                .collect::<Vec<_>>();
            assert!(listed.truncated, "git: {git}");
            // The two scans meet the limit in different orders: Git's listing is sorted, the walk
            // is depth-first.
            let expected = if git {
                ["a/x.txt", "b", "b/y.txt", "c/z.txt"]
            } else {
                ["a/x.txt", "b/y.txt", "c", "c/z.txt"]
            };
            assert_eq!(paths, expected, "git: {git}");
            assert_eq!(index.directory_paths().await.len(), 3, "git: {git}");
            // The directories the list dropped still cost memory while they are swept.
            let listed_bytes = paths
                .iter()
                .map(|path| path.len() + ENTRY_OVERHEAD_BYTES)
                .sum::<usize>();
            let dropped = if git { ["a", "c"] } else { ["a", "b"] };
            assert_eq!(
                index.memory_bytes().await,
                listed_bytes + dropped.len() * (1 + ENTRY_OVERHEAD_BYTES),
                "git: {git}"
            );
        }
    }

    /// Every listed directory is swept, and the others only within the memory the list left.
    #[tokio::test]
    async fn listed_directories_are_swept_when_the_memory_bound_is_spent() {
        let root = tempfile::tempdir().unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(root.path())
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::write(root.path().join("a.txt"), "").unwrap();
        std::fs::create_dir_all(root.path().join("c")).unwrap();
        std::fs::write(root.path().join("c/x"), "").unwrap();
        std::fs::create_dir_all(root.path().join("b".repeat(60))).unwrap();
        let index = WorkspaceSearchIndex::new(
            root.path().to_path_buf(),
            SearchLimits {
                max_memory_bytes: 100,
                ..SearchLimits::default()
            },
        );
        index.refresh(CancellationToken::new()).await.unwrap();
        let listed = index.list(None).await.entries;
        assert!(listed.iter().any(|entry| entry.path == "c"));
        assert_eq!(index.directory_paths().await, ["c"]);
        assert!(index.memory_bytes().await <= 100);
    }

    /// The swept directories stay inside the index's path bound, and a directory the list also
    /// holds is counted once.
    #[tokio::test]
    async fn swept_directories_respect_the_path_and_memory_bounds() {
        let root = tempfile::tempdir().unwrap();
        for path in ["a/x.txt", "long-directory-name/y.txt"] {
            let path = root.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "").unwrap();
        }
        let index = WorkspaceSearchIndex::new(
            root.path().to_path_buf(),
            SearchLimits {
                max_path_bytes: 12,
                ..SearchLimits::default()
            },
        );
        index.refresh(CancellationToken::new()).await.unwrap();
        assert_eq!(index.directory_paths().await, ["a"]);
        let entry_bytes = index
            .list(None)
            .await
            .entries
            .iter()
            .map(|entry| entry.path.len() + ENTRY_OVERHEAD_BYTES)
            .sum::<usize>();
        // A swept directory that is also listed is not counted twice.
        assert_eq!(index.memory_bytes().await, entry_bytes);
    }

    #[tokio::test]
    async fn list_returns_only_the_requested_prefix_and_reports_truncation() {
        let root = tempfile::tempdir().unwrap();
        for path in ["alpha.txt", "bravo.txt", "charlie.txt"] {
            std::fs::write(root.path().join(path), "").unwrap();
        }
        let index = WorkspaceSearchIndex::new(root.path().to_path_buf(), SearchLimits::default());
        index.refresh(CancellationToken::new()).await.unwrap();

        let full = index.list(None).await;
        assert_eq!(full.entries.len(), 3);
        assert!(!full.truncated);

        let bounded = index.list(Some(2)).await;
        assert_eq!(bounded.entries, full.entries[..2]);
        assert!(bounded.truncated);

        let within_snapshot = index.list(Some(200)).await;
        assert_eq!(within_snapshot.entries, full.entries);
        assert!(!within_snapshot.truncated);

        let snapshot_limited = WorkspaceSearchIndex::new(
            root.path().to_path_buf(),
            SearchLimits {
                max_entries: 2,
                max_memory_bytes: usize::MAX,
                max_path_bytes: usize::MAX,
            },
        );
        snapshot_limited
            .refresh(CancellationToken::new())
            .await
            .unwrap();
        let snapshot_truncated = snapshot_limited.list(Some(200)).await;
        assert_eq!(snapshot_truncated.entries.len(), 2);
        assert!(snapshot_truncated.truncated);
    }

    #[tokio::test]
    async fn search_index_covers_ignores_scoring_limits_and_cancellation() {
        let root = tempfile::tempdir().unwrap();
        for directory in ["src", "node_modules", ".git", "ignored"] {
            std::fs::create_dir(root.path().join(directory)).unwrap();
        }
        std::fs::write(
            root.path().join(".gitignore"),
            "ignored\n*.tmp\n!important\n",
        )
        .unwrap();
        std::fs::write(root.path().join("src/main.rs"), "fn main() {}").unwrap();
        std::fs::write(root.path().join("src/map_renderer.rs"), "").unwrap();
        std::fs::write(root.path().join("ignored/secret.rs"), "").unwrap();
        std::fs::write(root.path().join("node_modules/package.js"), "").unwrap();

        let index = WorkspaceSearchIndex::new(root.path().to_path_buf(), SearchLimits::default());
        index.refresh(CancellationToken::new()).await.unwrap();
        let listed = index.list(None).await;
        assert!(
            listed
                .entries
                .iter()
                .any(|entry| entry.path == "src/main.rs")
        );
        assert!(
            !listed
                .entries
                .iter()
                .any(|entry| entry.path.contains("ignored"))
        );
        assert!(index.memory_bytes().await > 0);
        assert_eq!(
            index.search("main.rs", 10).await.entries[0].path,
            "src/main.rs"
        );
        assert_eq!(
            index.search("renderer", 10).await.entries[0].path,
            "src/map_renderer.rs"
        );
        assert_eq!(
            index.search("@smr", 10).await.entries[0].path,
            "src/map_renderer.rs"
        );
        assert!(index.search("does-not-exist", 0).await.entries.is_empty());

        let limited = WorkspaceSearchIndex::new(
            root.path().to_path_buf(),
            SearchLimits {
                max_entries: 1,
                max_memory_bytes: usize::MAX,
                max_path_bytes: usize::MAX,
            },
        );
        limited.refresh(CancellationToken::new()).await.unwrap();
        assert!(limited.list(None).await.truncated);

        let memory_limited = WorkspaceSearchIndex::new(
            root.path().to_path_buf(),
            SearchLimits {
                max_entries: usize::MAX,
                max_memory_bytes: 1,
                max_path_bytes: 1,
            },
        );
        memory_limited
            .refresh(CancellationToken::new())
            .await
            .unwrap();
        assert!(memory_limited.list(None).await.truncated);

        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(matches!(
            index.refresh(cancelled).await,
            Err(WorkspaceError::Cancelled)
        ));
        let file = root.path().join("not-directory");
        std::fs::write(&file, "file").unwrap();
        let invalid = WorkspaceSearchIndex::new(file, SearchLimits::default());
        assert!(matches!(
            invalid.refresh(CancellationToken::new()).await,
            Err(WorkspaceError::RootNotDirectory { .. })
        ));
    }

    #[test]
    fn search_helpers_cover_ranking_ignore_and_subsequence_edges() {
        let rules = HashSet::from(["build".to_owned(), "secret.txt".to_owned()]);
        assert!(should_ignore(".git/config", false, &rules));
        assert!(should_ignore("build/output.js", false, &rules));
        assert!(should_ignore("src/secret.txt", false, &rules));
        assert!(!should_ignore("src/public.txt", false, &rules));

        assert_eq!(fuzzy_score("src/main.rs", ""), Some((3, 0)));
        assert_eq!(fuzzy_score("src/main.rs", "main.rs"), Some((0, 0)));
        assert_eq!(fuzzy_score("src/main.rs", "main"), Some((1, 0)));
        assert_eq!(fuzzy_score("source/main.rs", "source"), Some((2, 0)));
        assert!(fuzzy_score("src/map_renderer.rs", "mpr").is_some());
        assert_eq!(fuzzy_score("src/main.rs", "zzz"), None);
        assert_eq!(subsequence_offset("renderer", "rer"), Some(6));
        assert_eq!(subsequence_offset("renderer", "zzz"), None);
        assert!(entry_kind_rank(EntryKind::File) < entry_kind_rank(EntryKind::Directory));
    }
}
