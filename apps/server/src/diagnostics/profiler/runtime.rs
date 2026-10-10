//! `ProfilerRuntime`: owns the profiler directory's lock, the store, the findings book and
//! the supervisor that follows the `enableProfiler` setting live.

use std::{
    fs::{File, OpenOptions, TryLockError},
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use serde_json::Value;
use time::OffsetDateTime;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time::{Instant, MissedTickBehavior},
};
use tokio_util::sync::CancellationToken;

use super::{
    ProfilerHandle,
    findings::FindingsBook,
    install, os_records,
    panic_hook::{PANIC_QUEUE, PanicRecord, install_panic_hook, panic_finding},
    profiler_dir,
    reader::{StatusLive, status_json},
    sentinel::{self, SessionRecord},
    store::{self, ProfilerStore, StoreConfig, StoreStats},
    uninstall,
};

/// How often retention and findings compaction run while the profiler is on. Otherwise they
/// run only when an events file rotates or the runtime starts.
const MAINTENANCE_EVERY: Duration = Duration::from_secs(3600);
/// How often `crashes/` is checked for a dump of this process that it survived, while the
/// profiler is on (the crash monitor runs only then). Also checked as the profiler turns on.
const DUMP_SCAN_EVERY: Duration = Duration::from_secs(60);
const LOCK_FILE: &str = ".lock";
/// A runtime restarted in-process may find its predecessor still releasing the lock.
const LOCK_ATTEMPTS: u32 = 10;
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(50);
/// Set by the first runtime that takes a directory in this process. A `running` record with
/// this process's pid is then an earlier runtime that stopped without marking its session
/// clean (an in-process restart after a failed start), not a crashed run.
static SESSION_BEGUN: AtomicBool = AtomicBool::new(false);

/// A background check that runs only while the profiler is on.
pub trait Detector: Send + Sync {
    fn name(&self) -> &'static str;
    /// Spawns the detector. It must return promptly once `cancel` fires.
    fn start(&self, cancel: CancellationToken) -> JoinHandle<()>;
}

pub struct ProfilerRuntime {
    dir: PathBuf,
    /// `None` when the directory could not be locked: the runtime is degraded and records
    /// nothing.
    handle: Option<Arc<ProfilerHandle>>,
    /// Degraded because another process holds the directory lock, not because of an I/O error.
    locked: bool,
    parts: Mutex<Option<Parts>>,
    stop: CancellationToken,
    /// Fires once teardown has released the directory; every `shutdown` call waits for it.
    done: CancellationToken,
    /// Set by `end_session`; teardown reads it just before releasing the directory.
    mark_clean: Arc<AtomicBool>,
}

/// What teardown stops, in order.
struct Parts {
    /// The runtime the profiler started on; teardown runs there even when the profiler is
    /// dropped on a thread outside it.
    runtime: tokio::runtime::Handle,
    supervisor: JoinHandle<()>,
    /// Classifies the previous run; teardown waits for it (at most the 5 s harvest timeout)
    /// so its finding reaches the store before the session is marked clean.
    classification: Option<JoinHandle<()>>,
    /// Raises the hook's panics as findings; teardown waits for the ones already handed over.
    panics: JoinHandle<()>,
    store: ProfilerStore,
    /// Held for the runtime's lifetime: the store cuts files back when it reopens them, so a
    /// second process writing the same directory would destroy data.
    lock: File,
}

impl ProfilerRuntime {
    /// Locks the directory, loads the findings and starts the store and the supervisor.
    /// Blocks on file I/O, and for up to half a second while a predecessor releases the
    /// directory lock, so async callers run it through `spawn_blocking`; it must run inside a
    /// Tokio runtime.
    pub fn start(
        state_dir: &Path,
        enabled: watch::Receiver<bool>,
        detectors: Vec<Box<dyn Detector>>,
    ) -> Self {
        let dir = profiler_dir(state_dir);
        let stop = CancellationToken::new();
        let lock = match lock_dir(&dir) {
            Ok(lock) => lock,
            Err(error) => {
                tracing::warn!(
                    dir = %dir.display(),
                    %error,
                    "profiler storage is unavailable; the profiler records nothing in this run"
                );
                let done = CancellationToken::new();
                done.cancel();
                return Self {
                    dir,
                    handle: None,
                    locked: error.kind() == io::ErrorKind::WouldBlock,
                    parts: Mutex::new(None),
                    stop,
                    done,
                    mark_clean: Arc::default(),
                };
            }
        };
        // `load` rewrites findings.ndjson, so it must finish before the writer can append.
        let mut findings = FindingsBook::load(&dir);
        let store = ProfilerStore::start(StoreConfig::new(dir.clone()));
        // The same compaction again as the writer's first message: `load` only logs a failed
        // rewrite, while the writer reports it as failing storage until a write succeeds.
        if let Some(lines) = findings.compacted_lines(OffsetDateTime::now_utc()) {
            store.sender().replace_findings(lines);
        }
        let (panics, panic_records) = mpsc::channel(PANIC_QUEUE);
        let linked_dumps = Mutex::new(findings.crash_ids().collect());
        let handle = Arc::new(ProfilerHandle {
            enabled: Default::default(),
            sender: store.sender(),
            findings: Mutex::new(findings),
            recording_since: Mutex::new(None),
            panics,
            linked_dumps,
        });
        // ponytail: one server per process in production. A second concurrent runtime (tests)
        // keeps its own handle, and the global `emit` reaches whichever runtime installed first.
        install(handle.clone());
        // The sentinel and the panic hook work with the profiler off too.
        let earlier_in_process = SESSION_BEGUN.swap(true, Ordering::Relaxed);
        let previous = sentinel::begin_session(&dir)
            .filter(|previous| !(earlier_in_process && previous.pid == std::process::id()));
        install_panic_hook();
        let runtime = tokio::runtime::Handle::current();
        let classification = previous.map(|previous| {
            runtime.spawn(classify_previous_run(handle.clone(), previous, dir.clone()))
        });
        let panics = runtime.spawn(raise_panics(handle.clone(), panic_records, stop.clone()));
        // This run's dumps: written for this process since it started.
        let dumps = DumpScan {
            dir: dir.clone(),
            since: OffsetDateTime::now_utc(),
        };
        let supervisor = runtime.spawn(supervise(
            handle.clone(),
            enabled,
            detectors,
            dumps,
            stop.clone(),
        ));
        Self {
            dir,
            handle: Some(handle),
            locked: false,
            parts: Mutex::new(Some(Parts {
                runtime,
                supervisor,
                classification,
                panics,
                store,
                lock,
            })),
            stop,
            done: CancellationToken::new(),
            mark_clean: Arc::default(),
        }
    }

    /// The `ProfilerStatus` contract shape. Reads the directory, so call it off the async
    /// workers.
    pub fn status(&self) -> Value {
        let stats = lock(&self.parts)
            .as_ref()
            .map_or_else(StoreStats::default, |parts| parts.store.stats());
        let mut live = StatusLive {
            // Degraded, or failing writes.
            storage_failing: self.handle.is_none() || stats.failing,
            storage_locked: self.locked,
            event_count: stats.written,
            dropped_events: stats.dropped,
            ..StatusLive::default()
        };
        if let Some(handle) = &self.handle {
            live.enabled = handle.enabled.load(Ordering::Relaxed);
            live.recording_since = lock(&handle.recording_since).clone();
            live = live.with_findings(&lock(&handle.findings));
        }
        // After the findings lock is released: this reads the directory.
        status_json(&self.dir, live)
    }

    /// The orderly end of a run: `shutdown`, with the session marked clean as teardown's last
    /// step before the directory is released, so whoever takes the directory next never reads
    /// a stale `running` record. A degraded runtime does not own the directory and writes
    /// nothing. A plain `shutdown` or a drop leaves the session `running`.
    pub async fn end_session(&self) {
        self.mark_clean.store(true, Ordering::Relaxed);
        self.shutdown().await;
    }

    /// Uninstalls the global handle, stops the detectors, flushes the store and releases the
    /// directory lock, then returns. Every call waits for the same teardown, including calls
    /// after an earlier caller stopped waiting (the update quiesce has a deadline).
    pub async fn shutdown(&self) {
        self.begin_teardown();
        self.done.cancelled().await;
    }

    /// Starts teardown once. It runs in its own task, so it completes even when nobody waits,
    /// and stops the detectors before the lock is released, so a replacement runtime can never
    /// run a second detector set beside them.
    fn begin_teardown(&self) {
        self.stop.cancel();
        if let Some(handle) = &self.handle {
            uninstall(handle);
        }
        let Some(Parts {
            runtime,
            supervisor,
            classification,
            panics,
            store,
            lock: dir_lock,
        }) = lock(&self.parts).take()
        else {
            return;
        };
        let done = self.done.clone();
        let (dir, mark_clean) = (self.dir.clone(), self.mark_clean.clone());
        runtime.spawn(async move {
            let _ = supervisor.await;
            if let Some(classification) = classification {
                let _ = classification.await;
            }
            let _ = panics.await;
            // The store joins its writer thread, which must not block an async worker.
            let _ = tokio::task::spawn_blocking(move || {
                store.shutdown();
                if mark_clean.load(Ordering::Relaxed) {
                    sentinel::end_session(&dir);
                }
                // Windows frees a lock left to `CloseHandle` asynchronously; unlocking first
                // lets a restarted runtime take it at once.
                let _ = dir_lock.unlock();
                drop(dir_lock);
            })
            .await;
            done.cancel();
        });
    }
}

impl Drop for ProfilerRuntime {
    /// A runtime dropped without `shutdown` (a server start that failed after creating it)
    /// must not leave its handle installed, its detectors running, or the directory locked.
    fn drop(&mut self) {
        self.begin_teardown();
    }
}

/// Takes the exclusive directory lock, retrying briefly while it is held. A lock still held
/// after that is an error, like any I/O failure.
fn lock_dir(dir: &Path) -> io::Result<File> {
    std::fs::create_dir_all(dir)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join(LOCK_FILE))?;
    let mut attempts = 1;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) if attempts < LOCK_ATTEMPTS => {
                attempts += 1;
                std::thread::sleep(LOCK_RETRY_DELAY);
            }
            Err(TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "another BiBCode process is writing to this profiler directory",
                ));
            }
            Err(TryLockError::Error(error)) => return Err(error),
        }
    }
}

/// Reads the crash dump and the OS crash records for a run that never marked its session
/// clean and records the finding. The finding goes straight into the book, not through
/// `record_finding`, so it is listed whether or not the profiler is on.
async fn classify_previous_run(handle: Arc<ProfilerHandle>, previous: SessionRecord, dir: PathBuf) {
    let record = os_records::harvest_previous_crash(&previous).await;
    let dump = {
        let (handle, previous) = (handle.clone(), previous.clone());
        tokio::task::spawn_blocking(move || {
            let sidecars = sentinel::dump_sidecars(&dir);
            let mut linked = lock(&handle.linked_dumps);
            let sidecar = sentinel::dump_of_run(&sidecars, &previous, &linked)?.clone();
            linked.insert(sidecar.id.clone());
            drop(linked);
            Some(sentinel::Dump::read(&dir, sidecar))
        })
        .await
        .ok()
        .flatten()
    };
    let run = sentinel::classify(
        &previous,
        sentinel::boot_id().as_deref(),
        sentinel::oom_kill_count(),
        record,
        dump,
    );
    if let Some(input) = sentinel::finding_for(&previous, run) {
        lock(&handle.findings).record(input, OffsetDateTime::now_utc(), &handle.sender);
    }
}

/// Raises a `crash.panic` finding for each panic the hook hands over, straight into the book
/// like the previous run's finding, so panics count with the profiler off too. Once stopped,
/// it raises the ones already handed over, then ends.
async fn raise_panics(
    handle: Arc<ProfilerHandle>,
    mut records: mpsc::Receiver<PanicRecord>,
    stop: CancellationToken,
) {
    let raise = |record| {
        lock(&handle.findings).record(
            panic_finding(record),
            OffsetDateTime::now_utc(),
            &handle.sender,
        );
    };
    loop {
        tokio::select! {
            biased;
            Some(record) = records.recv() => raise(record),
            () = stop.cancelled() => break,
        }
    }
    while let Ok(record) = records.try_recv() {
        raise(record);
    }
}

/// One task applies every toggle change in order: a stop awaits every detector before the
/// next start, so two detector sets never run at once.
async fn supervise(
    handle: Arc<ProfilerHandle>,
    mut setting: watch::Receiver<bool>,
    detectors: Vec<Box<dyn Detector>>,
    dumps: DumpScan,
    stop: CancellationToken,
) {
    let mut maintenance =
        tokio::time::interval_at(Instant::now() + MAINTENANCE_EVERY, MAINTENANCE_EVERY);
    maintenance.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut dump_scan = tokio::time::interval(DUMP_SCAN_EVERY);
    dump_scan.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut running: Option<Running> = None;
    loop {
        let want = *setting.borrow_and_update();
        running = match (want, running) {
            (true, None) => {
                dump_scan.reset_immediately();
                Some(Running::start(&handle, &detectors, &stop))
            }
            (false, Some(set)) => {
                set.stop(&handle).await;
                None
            }
            (_, unchanged) => unchanged,
        };
        tokio::select! {
            biased;
            () = stop.cancelled() => break,
            changed = setting.changed() => {
                if changed.is_err() {
                    // The settings owner is gone: keep the current state until shutdown.
                    stop.cancelled().await;
                    break;
                }
            }
            _ = maintenance.tick(), if running.is_some() => maintain(&handle),
            _ = dump_scan.tick(), if running.is_some() => dumps.link_new(&handle).await,
        }
    }
    if let Some(set) = running {
        set.stop(&handle).await;
    }
}

/// Finds the dumps of this run that BiBCode survived (a Windows invalid-parameter handler, or
/// a crash the OS did not escalate) and raises a `crash.native` finding for each new one.
struct DumpScan {
    dir: PathBuf,
    /// When this run started; older dumps belong to earlier runs.
    since: OffsetDateTime,
}

impl DumpScan {
    async fn link_new(&self, handle: &Arc<ProfilerHandle>) {
        let (handle, dir, since) = (handle.clone(), self.dir.clone(), self.since);
        let boot = sentinel::boot_id();
        let _ = tokio::task::spawn_blocking(move || {
            // Oldest first, so the findings record the dumps in the order they happened.
            for sidecar in sentinel::dump_sidecars(&dir).into_iter().rev() {
                if !sentinel::dump_is_of(&sidecar, std::process::id(), since, boot.as_deref())
                    || !lock(&handle.linked_dumps).insert(sidecar.id.clone())
                {
                    continue;
                }
                let finding = sentinel::Dump::read(&dir, sidecar).running_finding();
                // Persisted at once: the next start knows the run survived this dump only
                // from the finding that cites it.
                lock(&handle.findings).record_now(
                    finding,
                    OffsetDateTime::now_utc(),
                    &handle.sender,
                );
            }
        })
        .await;
    }
}

struct Running {
    cancel: CancellationToken,
    tasks: Vec<(&'static str, JoinHandle<()>)>,
}

impl Running {
    fn start(
        handle: &ProfilerHandle,
        detectors: &[Box<dyn Detector>],
        stop: &CancellationToken,
    ) -> Self {
        handle.enabled.store(true, Ordering::Relaxed);
        *lock(&handle.recording_since) = Some(store::now_rfc3339());
        let cancel = stop.child_token();
        let tasks = detectors
            .iter()
            .map(|detector| (detector.name(), detector.start(cancel.clone())))
            .collect();
        Self { cancel, tasks }
    }

    async fn stop(self, handle: &ProfilerHandle) {
        self.cancel.cancel();
        for (name, task) in self.tasks {
            if let Err(error) = task.await
                && error.is_panic()
            {
                tracing::warn!(detector = name, "profiler detector panicked");
            }
        }
        handle.enabled.store(false, Ordering::Relaxed);
        *lock(&handle.recording_since) = None;
    }
}

/// Queues a findings rewrite, then retention, on the writer thread. The lines are computed
/// and queued under the findings lock, so a finding recorded meanwhile is appended after the
/// rewrite rather than lost by it. Retention runs second so it counts the compacted file.
fn maintain(handle: &ProfilerHandle) {
    let mut book = lock(&handle.findings);
    if let Some(lines) = book.compacted_lines(OffsetDateTime::now_utc()) {
        handle.sender.replace_findings(lines);
    }
    drop(book);
    handle.sender.maintain();
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding_line(last_seen: &str) -> String {
        format!(
            r#"{{"id":"rpc.slow-000000000001","fingerprint":"000000000001","kind":"rpc.slow","severity":"warn","firstSeen":"{last_seen}","lastSeen":"{last_seen}","count":1,"source":"server","title":"t","summary":"s","state":"open","evidence":{{}}}}"#
        )
    }

    /// Polls a condition the store's writer thread (real time) makes true, while letting the
    /// paused-clock tasks run.
    async fn eventually(condition: impl Fn() -> bool) {
        for _ in 0..1000 {
            if condition() {
                return;
            }
            tokio::task::yield_now().await;
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("condition not reached");
    }

    fn line_count(path: &Path) -> usize {
        std::fs::read_to_string(path).map_or(0, |text| text.lines().count())
    }

    #[tokio::test(start_paused = true)]
    async fn maintenance_runs_hourly_while_enabled() {
        let state = tempfile::tempdir().unwrap();
        let dir = profiler_dir(state.path());
        std::fs::create_dir_all(&dir).unwrap();
        let stale = dir.join("events-000000.ndjson");
        std::fs::write(&stale, b"{}\n").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - Duration::from_secs(10 * 24 * 3600))
            .unwrap();
        let findings = dir.join("findings.ndjson");
        // Open findings survive compaction at any age.
        let line = finding_line("2026-10-09T00:00:00Z");
        std::fs::write(&findings, format!("{line}\n")).unwrap();

        let (_tx, rx) = watch::channel(true);
        let runtime = ProfilerRuntime::start(state.path(), rx, vec![]);
        tokio::time::sleep(Duration::from_millis(10)).await;
        // Appended updates of one finding pile up until the next compaction. Queued behind the
        // writer's startup compaction, so it lands after it. No event is sent, so no events
        // file opens and retention does not run before maintenance.
        let handle = runtime.handle.clone().unwrap();
        handle
            .sender
            .send_finding(serde_json::from_str(&line).unwrap());
        eventually(|| line_count(&findings) == 2).await;
        assert!(stale.exists());

        tokio::time::advance(MAINTENANCE_EVERY).await;
        eventually(|| !stale.exists() && line_count(&findings) == 1).await;
        runtime.shutdown().await;
    }
}
