//! Detects workspace changes made outside the application.
//!
//! The workspace index is a set of paths, not file contents, so only path-set changes can
//! invalidate it. On every filesystem BiBCode targets, adding, renaming, or removing an entry
//! updates the containing directory's mtime, while editing a file in place does not. The sweep
//! therefore stats the directories the index already knows about instead of walking the tree, which
//! keeps the cost proportional to the directory count rather than the file count.
//!
//! See `docs/plans/2026-08-18-workspace-change-detection-design.md`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// A directory's last-modified stamp, or `None` when it could not be stat'd — a directory that
/// disappears is itself a change, so an unreadable directory is a distinct state rather than an
/// error to swallow.
type DirectoryStamps = BTreeMap<PathBuf, Option<SystemTime>>;

/// Emitted when the sweep observes that the workspace's path set may have changed.
///
/// The event carries no paths. The index is the single source of truth for entry data, so
/// subscribers rebuild from it rather than applying a diff that could drift out of agreement with
/// it. That also keeps the event independent of how many files a change storm touched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchEvent {
    /// Directories whose stamp differed, purely for diagnostics.
    pub changed_paths: Vec<PathBuf>,
}

/// Supplies the directories to stat for a workspace root.
///
/// The index owns this set, so the watcher asks for it each sweep instead of caching it: a rebuild
/// can add or remove directories, and a stale set would go blind to new ones.
pub trait WatchScope: Send + Sync {
    fn directories(&self, root: PathBuf) -> WatchScopeFuture;
}

pub type WatchScopeFuture = std::pin::Pin<Box<dyn Future<Output = Vec<String>> + Send + 'static>>;

#[derive(Clone)]
pub struct WorkspaceWatcher {
    poll_interval: Duration,
    coalesce_window: Duration,
    channel_capacity: usize,
    active: Arc<AtomicUsize>,
}

impl WorkspaceWatcher {
    pub fn new(
        poll_interval: Duration,
        coalesce_window: Duration,
        channel_capacity: usize,
    ) -> Self {
        Self {
            poll_interval,
            coalesce_window,
            channel_capacity: channel_capacity.max(1),
            active: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Starts sweeping `root`.
    ///
    /// The baseline is captured before the task is spawned, so a change that lands immediately
    /// after subscribing is reported rather than being absorbed into a first sweep that had not
    /// run yet.
    pub async fn watch(&self, root: PathBuf, scope: Arc<dyn WatchScope>) -> WatchSubscription {
        let baseline = {
            let directories = scope.directories(root.clone()).await;
            directory_stamps(&root, &directories).await
        };
        self.watch_from_baseline(root, scope, baseline)
    }

    fn watch_from_baseline(
        &self,
        root: PathBuf,
        scope: Arc<dyn WatchScope>,
        baseline: DirectoryStamps,
    ) -> WatchSubscription {
        let cancellation = CancellationToken::new();
        let task_cancellation = cancellation.clone();
        let (sender, receiver) = mpsc::channel(self.channel_capacity);
        let active = Arc::clone(&self.active);
        let poll_interval = self.poll_interval;
        let coalesce_window = self.coalesce_window;
        active.fetch_add(1, Ordering::Relaxed);
        let task = tokio::spawn(async move {
            let mut previous = baseline;
            let mut pending = Vec::new();
            let mut deadline = None;
            let mut interval = tokio::time::interval(poll_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    () = task_cancellation.cancelled() => break,
                    _ = interval.tick() => {
                        let directories = scope.directories(root.clone()).await;
                        let current = directory_stamps(&root, &directories).await;
                        let changed = changed_paths(&previous, &current);
                        previous = current;
                        let quiet = changed.is_empty();
                        if !quiet {
                            pending.extend(changed);
                            // Arm the window on the first change of a burst and leave it alone.
                            // Re-arming on every change would let sustained activity — a build, an
                            // install, an agent writing files — push the flush out indefinitely.
                            if deadline.is_none() {
                                deadline = Some(tokio::time::Instant::now() + coalesce_window);
                            }
                        }
                        // A burst is announced once, at the first sweep that sees nothing new;
                        // the window only bounds how long sustained change defers it. Every
                        // announcement costs each subscriber a full re-list.
                        if !pending.is_empty()
                            && (quiet
                                || deadline
                                    .is_some_and(|when| tokio::time::Instant::now() >= when))
                        {
                            let event = WatchEvent {
                                changed_paths: std::mem::take(&mut pending),
                            };
                            // A full channel means a subscriber has not consumed the previous
                            // signal yet. The signal is idempotent, so the undelivered one still
                            // stands and nothing is lost by dropping this send; the pending paths
                            // are already folded into it.
                            let _ = sender.try_send(event);
                            deadline = None;
                        }
                    }
                }
            }
            active.fetch_sub(1, Ordering::Relaxed);
        });
        WatchSubscription {
            receiver,
            cancellation,
            task: Some(task),
        }
    }

    pub fn active_watchers(&self) -> usize {
        self.active.load(Ordering::Relaxed)
    }
}

pub struct WatchSubscription {
    receiver: mpsc::Receiver<WatchEvent>,
    cancellation: CancellationToken,
    task: Option<JoinHandle<()>>,
}

impl WatchSubscription {
    pub async fn recv(&mut self) -> Option<WatchEvent> {
        self.receiver.recv().await
    }

    pub fn try_recv(&mut self) -> Result<WatchEvent, mpsc::error::TryRecvError> {
        self.receiver.try_recv()
    }

    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    pub async fn stopped(&mut self) {
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for WatchSubscription {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Stats the workspace root and each supplied relative directory.
///
/// The root is always included so that entries created directly at the top level are seen even when
/// the index lists no directories at all.
pub async fn directory_stamps(root: &Path, relative_directories: &[String]) -> DirectoryStamps {
    let root = root.to_path_buf();
    let relative_directories = relative_directories.to_vec();
    tokio::task::spawn_blocking(move || {
        let mut stamps = BTreeMap::new();
        stamps.insert(root.clone(), modified_at(&root));
        for relative in &relative_directories {
            let path = root.join(relative);
            let stamp = modified_at(&path);
            stamps.insert(path, stamp);
        }
        stamps
    })
    .await
    .unwrap_or_default()
}

fn modified_at(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .ok()
        .filter(|metadata| metadata.is_dir())
        .and_then(|metadata| metadata.modified().ok())
}

fn changed_paths(previous: &DirectoryStamps, current: &DirectoryStamps) -> Vec<PathBuf> {
    previous
        .keys()
        .chain(current.keys())
        .filter(|path| previous.get(*path) != current.get(*path))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}
