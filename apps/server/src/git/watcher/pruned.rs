//! Linux: a worktree's recursive watch without the directories Git ignores (#88).
//!
//! notify's recursive inotify watch adds one watch per directory below the root, so a
//! checkout with `node_modules`, `target` or nested agent worktrees holds hundreds of
//! thousands of watches. Registering them overflows the kernel's event queue (every walk
//! opens each directory), the overflow requests a rescan, and the rescan walks again.
//! Git status never reads those directories, so this tree watches every other directory
//! non-recursively and skips them, and adds directories that appear later the same way.

use std::{
    collections::{HashMap, HashSet},
    ffi::{OsStr, OsString},
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex, Weak, mpsc},
    time::Duration,
};

use notify::{RecursiveMode, Watcher};

/// Directory names skipped when they appear after registration, and when Git cannot say
/// what it ignores. Names Git reported as ignored anywhere in the tree are added to these.
const HEAVY_DIRECTORY_NAMES: &[&str] = &["node_modules", "target"];
const IGNORED_LIST_TIMEOUT: Duration = Duration::from_secs(10);

/// What a pruned tree leaves out.
#[derive(Debug, Default)]
pub(super) struct Skip {
    /// Directories Git reported as ignored, as absolute paths.
    ignored: HashSet<PathBuf>,
    /// Their names, for directories created after registration.
    ignored_names: HashSet<OsString>,
}

impl Skip {
    /// The Git-ignored directories of the worktree at `root`, or the heavy names alone when
    /// Git cannot list them.
    pub(super) fn of_worktree(root: &Path) -> Self {
        let mut skip = Self {
            ignored: HashSet::new(),
            ignored_names: HEAVY_DIRECTORY_NAMES.iter().map(OsString::from).collect(),
        };
        match ignored_directories(root) {
            Some(directories) => {
                for directory in directories {
                    if let Some(name) = directory.file_name() {
                        skip.ignored_names.insert(name.to_owned());
                    }
                    skip.ignored.insert(directory);
                }
            }
            None => tracing::warn!(
                "Git could not list ignored directories; the watch skips only common build directories"
            ),
        }
        skip
    }

    /// Whether `directory` (below the root) is left out: ignored, or the root of another
    /// repository or worktree, which has its own watch.
    fn skips(&self, directory: &Path, created_later: bool) -> bool {
        self.ignored.contains(directory)
            || (created_later
                && directory
                    .file_name()
                    .is_some_and(|name| self.ignored_names.contains(name)))
            || fs::symlink_metadata(directory.join(".git")).is_ok()
    }
}

/// The directories an ignore pattern names: `git ls-files --others --ignored
/// --exclude-standard --directory` lists candidates without their contents, but it also
/// lists a directory whose present contents all happen to be ignored, which may still
/// receive tracked files; `git check-ignore` keeps the ones a pattern matches.
fn ignored_directories(root: &Path) -> Option<Vec<PathBuf>> {
    let listed = run_git(
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
    )?;
    let candidates = listed
        .split(|byte| *byte == 0)
        .filter(|entry| entry.ends_with(b"/") && entry.len() > 1)
        .flat_map(|entry| entry.iter().copied().chain([0]))
        .collect::<Vec<u8>>();
    if candidates.is_empty() {
        return Some(Vec::new());
    }
    let matched = run_git(root, &["check-ignore", "-z", "--stdin"], Some(candidates))?;
    Some(
        matched
            .split(|byte| *byte == 0)
            .filter_map(|entry| entry.strip_suffix(b"/"))
            .filter(|entry| !entry.is_empty())
            .map(|entry| {
                use std::os::unix::ffi::OsStrExt as _;
                root.join(OsStr::from_bytes(entry))
            })
            .collect(),
    )
}

/// Runs `git -C root <args>` with `input` on stdin and returns its stdout. `check-ignore`
/// exits 1 when nothing matched, which is an answer, not a failure.
fn run_git(root: &Path, args: &[&str], input: Option<Vec<u8>>) -> Option<Vec<u8>> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(Stdio::null())
        .stdout(Stdio::piped());
    crate::process::isolate_appimage_environment(&mut command);
    let mut child = command.spawn().ok()?;
    if let Some(input) = input {
        let mut stdin = child.stdin.take()?;
        std::thread::spawn(move || {
            use std::io::Write as _;
            let _ = stdin.write_all(&input);
        });
    }
    let output = wait_bounded(child, IGNORED_LIST_TIMEOUT)?;
    matches!(output.status.code(), Some(0 | 1)).then_some(output.stdout)
}

/// Waits for `child` at most `timeout`, killing it after that.
fn wait_bounded(mut child: std::process::Child, timeout: Duration) -> Option<std::process::Output> {
    let stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut stdout) = stdout {
            use std::io::Read as _;
            let _ = stdout.read_to_end(&mut bytes);
        }
        bytes
    });
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let stdout = reader.join().ok()?;
    Some(std::process::Output {
        status,
        stdout,
        stderr: Vec::new(),
    })
}

/// `top` and every directory below it that `skip` keeps, parents before children. Symbolic
/// links are not followed, as Git does not follow them.
pub(super) fn kept_directories(top: &Path, skip: &Skip, created_later: bool) -> Vec<PathBuf> {
    let mut kept = vec![top.to_path_buf()];
    let mut next = 0;
    while let Some(directory) = kept.get(next).cloned() {
        next += 1;
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                let child = entry.path();
                if !skip.skips(&child, created_later) {
                    kept.push(child);
                }
            }
        }
    }
    kept
}

struct PrunedRoot {
    skip: Skip,
    directories: HashSet<PathBuf>,
}

type Roots = Arc<Mutex<HashMap<PathBuf, PrunedRoot>>>;

/// A notify watcher whose recursive watches on worktree roots are pruned.
pub(super) struct PrunedWatcher {
    watcher: Arc<Mutex<notify::RecommendedWatcher>>,
    roots: Roots,
}

impl PrunedWatcher {
    /// `callback` receives every event; directories the events create are watched by a
    /// worker thread, because notify's `watch` must not run on its own event thread.
    pub(super) fn new(
        callback: impl Fn(notify::Result<notify::Event>) + Send + 'static,
        config: notify::Config,
    ) -> notify::Result<Self> {
        let (created, created_paths) = mpsc::channel::<PathBuf>();
        let watcher = notify::RecommendedWatcher::new(
            move |event: notify::Result<notify::Event>| {
                if let Ok(event) = &event
                    && matches!(event.kind, notify::EventKind::Create(_))
                {
                    for path in &event.paths {
                        let _ = created.send(path.clone());
                    }
                }
                callback(event);
            },
            config,
        )?;
        let watcher = Arc::new(Mutex::new(watcher));
        let roots: Roots = Arc::default();
        let (worker_watcher, worker_roots) = (Arc::downgrade(&watcher), Arc::downgrade(&roots));
        std::thread::Builder::new()
            .name("git-watch-pruned".to_owned())
            .spawn(move || {
                watch_created_directories(&created_paths, &worker_watcher, &worker_roots)
            })
            .map_err(|error| notify::Error::io(error))?;
        Ok(Self { watcher, roots })
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
        let skip = Skip::of_worktree(path);
        let directories = kept_directories(path, &skip, false);
        let mut watcher = lock(&self.watcher);
        let mut watched = HashSet::with_capacity(directories.len());
        for directory in directories {
            match watcher.watch(&directory, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    watched.insert(directory);
                }
                // A directory removed during the walk.
                Err(error) if matches!(error.kind, notify::ErrorKind::PathNotFound) => {}
                Err(error) => {
                    for directory in &watched {
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
                directories: watched,
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
        for directory in &root.directories {
            let _ = watcher.unwatch(directory);
        }
        Ok(())
    }
}

/// Watches each created directory below a pruned root that its skip keeps, with the
/// directories below it, until the watcher is gone.
fn watch_created_directories(
    created: &mpsc::Receiver<PathBuf>,
    watcher: &Weak<Mutex<notify::RecommendedWatcher>>,
    roots: &Weak<Mutex<HashMap<PathBuf, PrunedRoot>>>,
) {
    while let Ok(path) = created.recv() {
        let (Some(watcher), Some(roots)) = (watcher.upgrade(), roots.upgrade()) else {
            return;
        };
        if !fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_dir()) {
            continue;
        }
        let mut roots = lock(&roots);
        let Some(root) = roots
            .iter_mut()
            .filter(|(root, _)| path.starts_with(root) && path != **root)
            .max_by_key(|(root, _)| root.as_os_str().len())
            .map(|(_, root)| root)
        else {
            continue;
        };
        if root.directories.contains(&path) || root.skip.skips(&path, true) {
            continue;
        }
        let mut watcher = lock(&watcher);
        for directory in kept_directories(&path, &root.skip, true) {
            if watcher
                .watch(&directory, RecursiveMode::NonRecursive)
                .is_ok()
            {
                root.directories.insert(directory);
            }
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
        fs::write(root.join(".gitignore"), "node_modules/\nbuild/\n").unwrap();
        for directory in [
            "src/lib",
            "node_modules/pkg/deep",
            "packages/app/node_modules/x",
            "build/out",
            ".claude/worktrees/agent/src",
        ] {
            fs::create_dir_all(root.join(directory)).unwrap();
        }
        // A nested worktree marks its root with a `.git` file.
        fs::write(
            root.join(".claude/worktrees/agent/.git"),
            "gitdir: elsewhere\n",
        )
        .unwrap();
        temp
    }

    #[test]
    fn the_tree_keeps_tracked_directories_and_skips_ignored_and_nested_ones() {
        let temp = worktree();
        let root = temp.path();
        let skip = Skip::of_worktree(root);
        let kept = kept_directories(root, &skip, false)
            .into_iter()
            .map(|path| {
                path.strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<HashSet<_>>();
        for expected in [
            "",
            "src",
            "src/lib",
            ".git",
            "packages",
            "packages/app",
            ".claude",
        ] {
            assert!(kept.contains(expected), "{expected} is watched: {kept:?}");
        }
        for skipped in [
            "node_modules",
            "packages/app/node_modules",
            "build",
            ".claude/worktrees/agent",
        ] {
            assert!(!kept.contains(skipped), "{skipped} is skipped: {kept:?}");
        }
        assert!(!kept.iter().any(|path| path.starts_with("node_modules/")));
    }

    #[test]
    fn a_directory_created_later_with_an_ignored_name_is_skipped() {
        let temp = worktree();
        let root = temp.path();
        let skip = Skip::of_worktree(root);
        let fresh = root.join("packages/new/node_modules");
        fs::create_dir_all(fresh.join("dep")).unwrap();
        assert!(skip.skips(&fresh, true));
        let kept = kept_directories(&root.join("packages/new"), &skip, true);
        assert_eq!(kept, [root.join("packages/new")]);
    }

    #[test]
    fn events_arrive_from_kept_and_new_directories_but_not_ignored_ones() {
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
        // The worker adds the new directory; give it a moment before writing inside.
        std::thread::sleep(Duration::from_millis(200));
        fs::write(root.join("src/fresh/file.rs"), "x").unwrap();
        assert!(until(&events, &root.join("src/fresh/file.rs")).is_some());

        watcher.unwatch(&root).unwrap();
    }
}
