//! Linux: a worktree's recursive watch without the directories Git ignores (#88).
//!
//! notify's recursive inotify watch adds one watch per directory below the root, so a
//! checkout with `node_modules`, `target` or nested agent worktrees holds hundreds of
//! thousands of watches. Registering them overflows the kernel's event queue (every walk
//! opens each directory), the overflow requests a rescan, and the rescan walks again.
//! Git status never reads those directories, so this tree watches every other directory
//! non-recursively and skips them. Directories that appear later, and changed ignore
//! rules, are reconciled by a worker thread with the same Git answers.

use std::{
    collections::{HashMap, HashSet},
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak, mpsc},
    time::Duration,
};

use notify::{RecursiveMode, Watcher};
use tokio_util::sync::CancellationToken;

use crate::git::{OutputPolicy, ProcessRequest, ProcessRunner, repository::git_read_environment};

/// Skipped when Git cannot say what it ignores.
const FALLBACK_SKIPPED_NAMES: &[&str] = &["node_modules", "target"];
const GIT_TIMEOUT: Duration = Duration::from_secs(10);
const GIT_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

type Callback = Arc<dyn Fn(notify::Result<notify::Event>) + Send + Sync>;

/// Runs Git through the supervised process runner, on `runtime` or a short-lived one.
#[derive(Clone)]
struct Git {
    runtime: Option<tokio::runtime::Handle>,
}

impl Git {
    fn current() -> Self {
        Self {
            runtime: tokio::runtime::Handle::try_current().ok(),
        }
    }

    /// Standard output of `git <args>` in `root`, or `None` when Git failed. `check-ignore`
    /// exits 1 when nothing matched, which is an answer.
    fn run(&self, root: &Path, args: &[&str], stdin: Option<Vec<u8>>) -> Option<Vec<u8>> {
        let request = ProcessRequest {
            operation: "GitWatch.ignoredDirectories".to_owned(),
            command: PathBuf::from("git"),
            args: args.iter().map(OsString::from).collect(),
            cwd: root.to_path_buf(),
            env: git_read_environment(),
            stdin,
            timeout: GIT_TIMEOUT,
            max_output_bytes: GIT_OUTPUT_BYTES,
            output_policy: OutputPolicy::Error,
            append_truncation_marker: false,
            allow_non_zero_exit: true,
        };
        let cancellation = CancellationToken::new();
        let run = ProcessRunner.run_bytes(request, &cancellation);
        let output = match &self.runtime {
            Some(runtime) => runtime.block_on(run),
            None => tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()?
                .block_on(run),
        }
        .ok()?;
        matches!(output.exit_code, 0 | 1).then_some(output.stdout)
    }

    /// Which of `directories` (below `root`) an ignore rule names, by path, so a rule
    /// anchored to one place does not skip a same-named directory elsewhere.
    fn ignored(&self, root: &Path, directories: &[PathBuf]) -> Option<HashSet<PathBuf>> {
        let input = directories
            .iter()
            .filter_map(|directory| directory.strip_prefix(root).ok())
            .flat_map(|relative| {
                use std::os::unix::ffi::OsStrExt as _;
                relative
                    .as_os_str()
                    .as_bytes()
                    .iter()
                    .copied()
                    .chain(*b"/\0")
            })
            .collect::<Vec<u8>>();
        if input.is_empty() {
            return Some(HashSet::new());
        }
        let matched = self.run(root, &["check-ignore", "-z", "--stdin"], Some(input))?;
        Some(paths_below(root, &matched))
    }
}

/// `root`-relative NUL-separated paths, each with a trailing slash, as absolute paths.
fn paths_below(root: &Path, output: &[u8]) -> HashSet<PathBuf> {
    output
        .split(|byte| *byte == 0)
        .filter_map(|entry| entry.strip_suffix(b"/"))
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            use std::os::unix::ffi::OsStrExt as _;
            root.join(OsStr::from_bytes(entry))
        })
        .collect()
}

/// What a pruned tree leaves out.
#[derive(Debug, Default)]
pub(super) struct Skip {
    /// Directories an ignore rule names, as absolute paths.
    ignored: HashSet<PathBuf>,
    /// Git could not answer: common build directories are skipped by name instead.
    by_name: bool,
    /// This worktree's Git directory, whose `modules/` holds its submodules' repositories.
    git_dir: Option<PathBuf>,
}

impl Skip {
    /// The ignored directories of the worktree at `root`. `git ls-files --others --ignored
    /// --directory` lists candidates without their contents, but also a directory whose
    /// present contents all happen to be ignored, which may still get tracked files;
    /// `git check-ignore` keeps the ones a rule names.
    fn of_worktree(git: &Git, root: &Path) -> Self {
        let listed = git.run(
            root,
            &[
                "ls-files",
                "-z",
                "--others",
                "--ignored",
                "--exclude-standard",
                "--directory",
            ],
            None,
        );
        let ignored = listed.and_then(|listed| {
            let candidates = paths_below(root, &listed).into_iter().collect::<Vec<_>>();
            git.ignored(root, &candidates)
        });
        let git_dir = git_dir(root);
        match ignored {
            Some(ignored) => Self {
                ignored,
                by_name: false,
                git_dir,
            },
            None => {
                tracing::warn!(
                    "Git could not list ignored directories; the watch skips only common build directories"
                );
                Self {
                    ignored: HashSet::new(),
                    by_name: true,
                    git_dir,
                }
            }
        }
    }

    fn skips_by_name(&self, directory: &Path) -> bool {
        directory.file_name().is_some_and(|name| {
            FALLBACK_SKIPPED_NAMES
                .iter()
                .any(|skipped| name == OsStr::new(skipped))
        })
    }

    /// Whether `directory` (below the root) is left out of the pruned walk: ignored, or a
    /// repository root, which is either watched separately or (a submodule) recursively.
    fn skips(&self, directory: &Path) -> bool {
        self.ignored.contains(directory)
            || (self.by_name && self.skips_by_name(directory))
            || self.repository(directory).is_some()
    }

    /// What kind of repository root `directory` is, if any. A submodule's Git directory
    /// lies in this worktree's `modules/`, read now so a submodule initialized after the
    /// watch started is recognized; anything else with a `.git` (another repository, a
    /// linked worktree) has its own watch and is not part of this worktree's status.
    fn repository(&self, directory: &Path) -> Option<Repository> {
        fs::symlink_metadata(directory.join(".git")).ok()?;
        let modules = self
            .git_dir
            .as_deref()
            .and_then(|git_dir| fs::canonicalize(git_dir.join("modules")).ok());
        let submodule = git_dir(directory)
            .and_then(|git_dir| fs::canonicalize(git_dir).ok())
            .zip(modules)
            .is_some_and(|(git_dir, modules)| git_dir.starts_with(modules));
        Some(if submodule {
            Repository::Submodule
        } else {
            Repository::Separate
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Repository {
    Submodule,
    Separate,
}

/// The Git directory of the worktree at `root`: `.git` itself, or where a `.git` file's
/// `gitdir:` points, relative to `root` when it is not absolute.
fn git_dir(root: &Path) -> Option<PathBuf> {
    let marker = root.join(".git");
    let metadata = fs::symlink_metadata(&marker).ok()?;
    if metadata.is_dir() {
        return Some(marker);
    }
    let text = fs::read_to_string(&marker).ok()?;
    let gitdir = Path::new(text.trim().strip_prefix("gitdir:")?.trim());
    Some(root.join(gitdir))
}

fn subdirectories(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    // Symbolic links are not followed, as Git does not follow them.
    entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect()
}

/// What a walk watches: directories one by one, and submodules with notify's recursive
/// watch. The parent's status shows a submodule's changes; its own ignore rules are not
/// applied, as before the pruned watch.
// ponytail: ignored directories inside a submodule stay watched; prune them with the
// submodule as a nested root if that ever matters.
#[derive(Debug, Default)]
struct Walk {
    directories: Vec<PathBuf>,
    submodules: Vec<PathBuf>,
}

impl Walk {
    /// Sorts the children of a walked directory: submodules apart, kept ones returned.
    fn children(&mut self, skip: &Skip, directory: &Path) -> Vec<PathBuf> {
        let mut kept = Vec::new();
        for child in subdirectories(directory) {
            if skip.repository(&child) == Some(Repository::Submodule) {
                self.submodules.push(child);
            } else if !skip.skips(&child) {
                kept.push(child);
            }
        }
        kept
    }
}

/// `top` and every directory below it that `skip` keeps, parents before children.
fn kept_directories(top: &Path, skip: &Skip) -> Walk {
    let mut walk = Walk {
        directories: vec![top.to_path_buf()],
        submodules: Vec::new(),
    };
    let mut next = 0;
    while let Some(directory) = walk.directories.get(next).cloned() {
        next += 1;
        let kept = walk.children(skip, &directory);
        walk.directories.extend(kept);
    }
    walk
}

/// What to watch in trees created after registration: one `check-ignore` per level decides
/// which directories to enter.
fn kept_new_directories(git: &Git, root: &Path, skip: &Skip, tops: Vec<PathBuf>) -> Walk {
    let mut walk = Walk::default();
    let mut level = Vec::new();
    for top in tops {
        if skip.repository(&top) == Some(Repository::Submodule) {
            walk.submodules.push(top);
        } else if !skip.skips(&top) {
            level.push(top);
        }
    }
    while !level.is_empty() {
        // When Git cannot answer, common build directories are skipped by name.
        let ignored = if skip.by_name {
            None
        } else {
            git.ignored(root, &level)
        };
        let mut next = Vec::new();
        for directory in level {
            let skipped = match &ignored {
                Some(ignored) => ignored.contains(&directory),
                None => skip.skips_by_name(&directory),
            };
            if skipped {
                continue;
            }
            let kept = walk.children(skip, &directory);
            next.extend(kept);
            walk.directories.push(directory);
        }
        level = next;
    }
    walk
}

struct PrunedRoot {
    skip: Skip,
    directories: HashSet<PathBuf>,
    submodules: HashSet<PathBuf>,
}

type Roots = Arc<Mutex<HashMap<PathBuf, PrunedRoot>>>;

enum Change {
    Created(PathBuf),
    /// An ignore file changed: `.gitignore` anywhere or `.git/info/exclude`.
    Rules(PathBuf),
}

/// A notify watcher whose recursive watches on worktree roots are pruned.
pub(super) struct PrunedWatcher {
    watcher: Arc<Mutex<notify::RecommendedWatcher>>,
    roots: Roots,
    git: Git,
}

impl PrunedWatcher {
    /// `callback` receives every event and the failures of watches added later. Those are
    /// added by a worker thread: notify's `watch` must not run on its own event thread.
    pub(super) fn new(
        callback: impl Fn(notify::Result<notify::Event>) + Send + Sync + 'static,
        config: notify::Config,
    ) -> notify::Result<Self> {
        let callback: Callback = Arc::new(callback);
        let (changes, changed) = mpsc::channel::<Change>();
        let event_callback = Arc::clone(&callback);
        let watcher = notify::RecommendedWatcher::new(
            move |event: notify::Result<notify::Event>| {
                if let Ok(event) = &event {
                    for path in &event.paths {
                        if is_ignore_file(path) {
                            // Reading the rules (Git does, to answer) must not re-read them.
                            if changes_contents(event.kind) {
                                let _ = changes.send(Change::Rules(path.clone()));
                            }
                        } else if matches!(
                            event.kind,
                            // A directory renamed into place arrives as a rename, and notify
                            // only re-watches it under a recursive parent. The worker keeps
                            // only paths that are directories now.
                            notify::EventKind::Create(_)
                                | notify::EventKind::Modify(notify::event::ModifyKind::Name(_))
                        ) {
                            let _ = changes.send(Change::Created(path.clone()));
                        }
                    }
                }
                event_callback(event);
            },
            config,
        )?;
        let watcher = Arc::new(Mutex::new(watcher));
        let roots: Roots = Arc::default();
        let git = Git::current();
        let worker = Worker {
            watcher: Arc::downgrade(&watcher),
            roots: Arc::downgrade(&roots),
            git: git.clone(),
            callback,
        };
        std::thread::Builder::new()
            .name("git-watch-pruned".to_owned())
            .spawn(move || worker.run(&changed))
            .map_err(notify::Error::io)?;
        Ok(Self {
            watcher,
            roots,
            git,
        })
    }

    pub(super) fn watch(
        &mut self,
        path: &Path,
        recursive_mode: RecursiveMode,
    ) -> notify::Result<()> {
        let worktree = recursive_mode == RecursiveMode::Recursive
            && fs::symlink_metadata(path.join(".git")).is_ok();
        if !worktree {
            return lock(&self.watcher).watch(path, recursive_mode);
        }
        let skip = Skip::of_worktree(&self.git, path);
        let walk = kept_directories(path, &skip);
        let (mut directories, mut submodules) = (HashSet::new(), HashSet::new());
        let mut watcher = lock(&self.watcher);
        let planned = walk
            .directories
            .into_iter()
            .map(|directory| (directory, RecursiveMode::NonRecursive))
            .chain(
                walk.submodules
                    .into_iter()
                    .map(|submodule| (submodule, RecursiveMode::Recursive)),
            );
        for (directory, mode) in planned {
            match watcher.watch(&directory, mode) {
                Ok(()) if mode == RecursiveMode::Recursive => {
                    submodules.insert(directory);
                }
                Ok(()) => {
                    directories.insert(directory);
                }
                // A directory removed during the walk.
                Err(error) if matches!(error.kind, notify::ErrorKind::PathNotFound) => {}
                Err(error) => {
                    // Including this one: a recursive watch can fail partway.
                    let _ = watcher.unwatch(&directory);
                    for directory in directories.iter().chain(&submodules) {
                        let _ = watcher.unwatch(directory);
                    }
                    return Err(error);
                }
            }
        }
        drop(watcher);
        lock(&self.roots).insert(
            path.to_path_buf(),
            PrunedRoot {
                skip,
                directories,
                submodules,
            },
        );
        Ok(())
    }

    pub(super) fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
        let Some(root) = lock(&self.roots).remove(path) else {
            return lock(&self.watcher).unwatch(path);
        };
        let mut watcher = lock(&self.watcher);
        // A directory removed since is no longer watched; that is not an error here.
        for directory in root.directories.iter().chain(&root.submodules) {
            let _ = watcher.unwatch(directory);
        }
        Ok(())
    }
}

fn changes_contents(kind: notify::EventKind) -> bool {
    use notify::event::{AccessKind, AccessMode, ModifyKind};
    matches!(
        kind,
        notify::EventKind::Create(_)
            | notify::EventKind::Remove(_)
            | notify::EventKind::Modify(
                ModifyKind::Data(_) | ModifyKind::Name(_) | ModifyKind::Any
            )
            | notify::EventKind::Access(AccessKind::Close(AccessMode::Write))
    )
}

fn is_ignore_file(path: &Path) -> bool {
    path.file_name() == Some(OsStr::new(".gitignore")) || path.ends_with(".git/info/exclude")
}

struct Worker {
    watcher: Weak<Mutex<notify::RecommendedWatcher>>,
    roots: Weak<Mutex<HashMap<PathBuf, PrunedRoot>>>,
    git: Git,
    callback: Callback,
}

impl Worker {
    /// Handles changes in batches until the watcher is gone.
    fn run(&self, changed: &mpsc::Receiver<Change>) {
        while let Ok(first) = changed.recv() {
            let mut created: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
            let mut rules = HashSet::new();
            let (Some(watcher), Some(roots)) = (self.watcher.upgrade(), self.roots.upgrade())
            else {
                return;
            };
            for change in std::iter::once(first).chain(changed.try_iter()) {
                let path = match &change {
                    Change::Created(path) | Change::Rules(path) => path,
                };
                let Some(root) = lock(&roots)
                    .keys()
                    .filter(|root| path.starts_with(root))
                    .max_by_key(|root| root.as_os_str().len())
                    .cloned()
                else {
                    continue;
                };
                match change {
                    Change::Rules(_) => {
                        rules.insert(root);
                    }
                    Change::Created(path) => {
                        // Inside a submodule, notify's recursive watch already covers it. A
                        // recreated submodule root itself needs a new recursive watch.
                        let in_submodule = lock(&roots).get(&root).is_some_and(|pruned| {
                            pruned
                                .submodules
                                .iter()
                                .any(|submodule| path.starts_with(submodule) && path != *submodule)
                        });
                        if path != root
                            && !in_submodule
                            && fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_dir())
                        {
                            created.entry(root).or_default().push(path);
                        }
                    }
                }
            }
            for root in rules {
                self.reconcile_rules(&watcher, &roots, &root);
            }
            for (root, tops) in created {
                let kept = {
                    let roots = lock(&roots);
                    let Some(pruned) = roots.get(&root) else {
                        continue;
                    };
                    kept_new_directories(&self.git, &root, &pruned.skip, tops)
                };
                self.add(&watcher, &roots, &root, kept);
            }
        }
    }

    /// Ignore rules changed: recompute what the root skips and watch what it now keeps.
    fn reconcile_rules(
        &self,
        watcher: &Mutex<notify::RecommendedWatcher>,
        roots: &Mutex<HashMap<PathBuf, PrunedRoot>>,
        root: &Path,
    ) {
        let skip = Skip::of_worktree(&self.git, root);
        let kept = kept_directories(root, &skip);
        if let Some(pruned) = lock(roots).get_mut(root) {
            pruned.skip = skip;
        }
        self.add(watcher, roots, root, kept);
    }

    /// Watches a walk's directories, including ones watched before they were removed and
    /// created again, and its submodules recursively. A failure other than a vanished
    /// directory reaches the callback as an error.
    fn add(
        &self,
        watcher: &Mutex<notify::RecommendedWatcher>,
        roots: &Mutex<HashMap<PathBuf, PrunedRoot>>,
        root: &Path,
        walk: Walk,
    ) {
        let (mut directories, mut submodules) = (Vec::new(), Vec::new());
        {
            let mut watcher = lock(watcher);
            let planned = walk
                .directories
                .into_iter()
                .map(|directory| (directory, RecursiveMode::NonRecursive))
                .chain(
                    walk.submodules
                        .into_iter()
                        .map(|submodule| (submodule, RecursiveMode::Recursive)),
                );
            for (directory, mode) in planned {
                match watcher.watch(&directory, mode) {
                    Ok(()) if mode == RecursiveMode::Recursive => submodules.push(directory),
                    Ok(()) => directories.push(directory),
                    Err(error) if matches!(error.kind, notify::ErrorKind::PathNotFound) => {}
                    Err(error) => {
                        // A recursive watch can fail partway, after watching part of the tree.
                        let _ = watcher.unwatch(&directory);
                        (self.callback)(Err(error));
                        break;
                    }
                }
            }
        }
        if let Some(pruned) = lock(roots).get_mut(root) {
            pruned.directories.extend(directories);
            pruned.submodules.extend(submodules);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    fn git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?}");
    }

    fn worktree() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        git(root, &["init", "-q"]);
        fs::write(root.join(".gitignore"), "node_modules/\n/build/\n").unwrap();
        for directory in [
            "src/lib",
            "node_modules/pkg/deep",
            "packages/app/node_modules/x",
            "build/out",
            ".claude/worktrees/agent/src",
            "vendor/sub/src",
        ] {
            fs::create_dir_all(root.join(directory)).unwrap();
        }
        // A linked worktree's `.git` file points into another repository's `worktrees/`,
        // here one that lives under an ordinary `modules` directory.
        fs::write(
            root.join(".claude/worktrees/agent/.git"),
            "gitdir: /home/dev/modules/project/.git/worktrees/agent\n",
        )
        .unwrap();
        // A submodule's Git directory lives in this repository's `.git/modules/`.
        let submodule = root.join("vendor/sub");
        let separate = root.join(".git/modules/vendor/sub");
        fs::create_dir_all(separate.parent().unwrap()).unwrap();
        let status = Command::new("git")
            .args(["init", "-q", "--separate-git-dir"])
            .arg(&separate)
            .arg(&submodule)
            .status()
            .unwrap();
        assert!(status.success());
        fs::write(submodule.join(".gitignore"), "dist/\n").unwrap();
        fs::create_dir_all(submodule.join("dist/bundle")).unwrap();
        temp
    }

    fn relative(root: &Path, paths: Vec<PathBuf>) -> HashSet<String> {
        paths
            .into_iter()
            .map(|path| {
                path.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }

    #[test]
    fn the_tree_keeps_tracked_and_submodule_directories_and_skips_ignored_and_linked_ones() {
        let temp = worktree();
        let root = temp.path();
        let skip = Skip::of_worktree(&Git::current(), root);
        let walk = kept_directories(root, &skip);
        assert_eq!(walk.submodules, [root.join("vendor/sub")]);
        let kept = relative(root, walk.directories);
        for expected in [
            "",
            "src",
            "src/lib",
            ".git",
            "packages",
            "packages/app",
            ".claude",
            "vendor",
        ] {
            assert!(kept.contains(expected), "{expected} is watched: {kept:?}");
        }
        for skipped in [
            "node_modules",
            "packages/app/node_modules",
            "build",
            ".claude/worktrees/agent",
            // Watched recursively, as a submodule.
            "vendor/sub",
        ] {
            assert!(!kept.contains(skipped), "{skipped} is skipped: {kept:?}");
        }
    }

    #[test]
    fn only_writes_to_an_ignore_file_reread_the_rules() {
        use notify::event::{AccessKind, AccessMode, DataChange, ModifyKind};
        // Git opens `.gitignore` to answer; that read must not start another reconcile.
        assert!(!changes_contents(notify::EventKind::Access(
            AccessKind::Open(AccessMode::Any)
        )));
        assert!(!changes_contents(notify::EventKind::Access(
            AccessKind::Close(AccessMode::Read)
        )));
        assert!(changes_contents(notify::EventKind::Access(
            AccessKind::Close(AccessMode::Write)
        )));
        assert!(changes_contents(notify::EventKind::Modify(
            ModifyKind::Data(DataChange::Any)
        )));
    }

    #[test]
    fn a_new_tree_is_checked_against_ignore_rules_by_path() {
        let temp = worktree();
        let root = temp.path();
        let git = Git::current();
        let skip = Skip::of_worktree(&git, root);
        // `/build/` is anchored: a `build` directory elsewhere is source.
        fs::create_dir_all(root.join("src/build/gen")).unwrap();
        fs::create_dir_all(root.join("packages/new/node_modules/dep")).unwrap();
        let kept = relative(
            root,
            kept_new_directories(
                &git,
                root,
                &skip,
                vec![root.join("src/build"), root.join("packages/new")],
            )
            .directories,
        );
        assert_eq!(
            kept,
            HashSet::from([
                "src/build".to_owned(),
                "src/build/gen".to_owned(),
                "packages/new".to_owned(),
            ])
        );
    }

    #[test]
    fn events_follow_kept_new_and_recreated_directories_and_rule_changes() {
        let temp = worktree();
        let root = temp.path().canonicalize().unwrap();
        let (sender, events) = mpsc::channel();
        let mut watcher = PrunedWatcher::new(
            move |event: notify::Result<notify::Event>| {
                if let Ok(event) = event {
                    for path in event.paths {
                        let _ = sender.send(path);
                    }
                }
            },
            notify::Config::default(),
        )
        .unwrap();
        watcher.watch(&root, RecursiveMode::Recursive).unwrap();
        // Every event path until `wanted` arrives, or `None` after 5 s.
        let until = |events: &mpsc::Receiver<PathBuf>, wanted: &Path| {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut seen = Vec::new();
            while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
                match events.recv_timeout(left) {
                    Ok(path) if path == wanted => return Some(seen),
                    Ok(path) => seen.push(path),
                    Err(_) => return None,
                }
            }
            None
        };
        // Writes `file` until its event arrives: the worker adds new directories a moment
        // after they appear.
        let observed = |events: &mpsc::Receiver<PathBuf>, file: &Path| {
            for _ in 0..20 {
                fs::write(file, "x").unwrap();
                if until(events, file).is_some() {
                    return true;
                }
            }
            false
        };

        fs::write(root.join("node_modules/pkg/deep/file"), "x").unwrap();
        fs::write(root.join("src/lib/file.rs"), "x").unwrap();
        let before = until(&events, &root.join("src/lib/file.rs")).expect("a kept directory");
        assert!(
            !before
                .iter()
                .any(|path| path.starts_with(root.join("node_modules"))),
            "ignored directories are not watched: {before:?}"
        );

        fs::create_dir(root.join("src/fresh")).unwrap();
        assert!(observed(&events, &root.join("src/fresh/file.rs")));

        // Removed and created again, as a branch switch does.
        fs::remove_dir_all(root.join("src/lib")).unwrap();
        fs::create_dir(root.join("src/lib")).unwrap();
        assert!(observed(&events, &root.join("src/lib/again.rs")));

        // Renamed into place, as `git mv` does: inotify reports MOVED_TO, not a create, and
        // drops the watch on the old name.
        fs::rename(root.join("src/lib"), root.join("src/renamed")).unwrap();
        assert!(
            observed(&events, &root.join("src/renamed/moved.rs")),
            "a renamed directory stays watched"
        );

        // Renamed onto an ignored name, it stays unwatched. The worker handles changes in
        // order, so once the later directory is watched the rename has been handled.
        fs::rename(root.join("src/renamed"), root.join("src/node_modules")).unwrap();
        fs::create_dir(root.join("src/after")).unwrap();
        assert!(observed(&events, &root.join("src/after/file.rs")));
        fs::write(root.join("src/node_modules/hidden.rs"), "x").unwrap();
        fs::write(root.join("src/after/file.rs"), "y").unwrap();
        let before = until(&events, &root.join("src/after/file.rs")).expect("a kept directory");
        assert!(
            !before.contains(&root.join("src/node_modules/hidden.rs")),
            "a rename onto an ignored name is not watched: {before:?}"
        );

        // Un-ignoring `build/` brings it into the watch.
        fs::write(root.join(".gitignore"), "node_modules/\n").unwrap();
        assert!(observed(&events, &root.join("build/out/file")));

        watcher.unwatch(&root).unwrap();
    }
}
