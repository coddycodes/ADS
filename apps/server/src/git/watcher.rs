mod layout;
#[cfg(target_os = "linux")]
mod pruned;
mod registration;

use layout::{component_matches, metadata_layout};
use registration::{
    Coverage, DirectoryChange, PendingPath, PendingRegistrations, WatchRegistration,
};

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{self, ErrorKind, Write},
    path::{Path, PathBuf},
    process,
    sync::{
        Arc, Mutex, MutexGuard, Weak,
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
    },
    time::Duration,
};

#[cfg(not(target_os = "linux"))]
use notify::Watcher;
use notify::{
    Config, Event, EventKind, RecursiveMode,
    event::{AccessKind, AccessMode, MetadataKind, ModifyKind},
};
use tokio::sync::{Notify, watch};
use tokio_util::sync::CancellationToken;

use super::{host_path_platform, normalize_worktree_path_key};

const READINESS_TIMEOUT: Duration = Duration::from_secs(5);
const READINESS_DIRECTORY_PREFIX: &str = "bibcode-git-watch-ready";
static NEXT_READINESS_DIRECTORY_ID: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
static NATIVE_WATCHER_TEST_PERMIT: NativeWatcherTestLock = NativeWatcherTestLock::new();

#[cfg(test)]
pub(crate) async fn acquire_native_watcher_test_permit() -> NativeWatcherTestPermit<'static> {
    NATIVE_WATCHER_TEST_PERMIT
        .acquire(Duration::from_secs(300))
        .await
}

#[cfg(test)]
struct NativeWatcherTestLock {
    permit: tokio::sync::Mutex<()>,
    holder: Mutex<Option<(String, std::time::Instant)>>,
}

#[cfg(test)]
impl NativeWatcherTestLock {
    const fn new() -> Self {
        Self {
            permit: tokio::sync::Mutex::const_new(()),
            holder: Mutex::new(None),
        }
    }

    async fn acquire(&self, timeout: Duration) -> NativeWatcherTestPermit<'_> {
        let test_name = std::thread::current()
            .name()
            .unwrap_or("unnamed test thread")
            .to_owned();
        // Tokio time may be paused by the caller. Short real sleeps also ensure
        // dropping this wait cannot leave a 300-second blocking task at shutdown.
        let deadline = async {
            let started = std::time::Instant::now();
            while started.elapsed() < timeout {
                let remaining = timeout.saturating_sub(started.elapsed());
                tokio::task::spawn_blocking(move || {
                    std::thread::sleep(remaining.min(Duration::from_millis(25)));
                })
                .await
                .expect("native watcher permit wall-clock wait joins");
            }
        };
        let guard = tokio::select! {
            biased;
            guard = self.permit.lock() => guard,
            () = deadline => {
                let holder = self.holder.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                let detail = holder.as_ref().map_or_else(
                    || "holder released during timeout reporting".to_owned(),
                    |(name, started)| format!("held by {name} for {:?}", started.elapsed()),
                );
                drop(holder);
                panic!("native watcher test permit timed out after {timeout:?} for {test_name}; {detail}");
            }
        };
        *self
            .holder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((test_name, std::time::Instant::now()));
        NativeWatcherTestPermit {
            lock: self,
            _guard: guard,
        }
    }
}

#[cfg(test)]
pub(crate) struct NativeWatcherTestPermit<'a> {
    lock: &'a NativeWatcherTestLock,
    _guard: tokio::sync::MutexGuard<'a, ()>,
}

#[cfg(test)]
impl Drop for NativeWatcherTestPermit<'_> {
    fn drop(&mut self) {
        // Clear the diagnostic while still holding the async permit.
        self.lock
            .holder
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
    }
}

#[cfg(test)]
pub(super) async fn test_timeout<T>(
    context: &str,
    future: impl std::future::Future<Output = T>,
) -> T {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {context}"))
}

#[derive(Clone, Debug)]
pub(crate) struct GitWatchRequest {
    pub(crate) worktree_root: PathBuf,
    pub(crate) git_dir: PathBuf,
    pub(crate) common_dir: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GitWatchEvent {
    WorkingTree,
    Metadata,
    RefMetadata,
    Overflow,
    Unavailable,
}

#[derive(Clone, Copy, Default)]
struct GitWatchSignal {
    event: Option<GitWatchEvent>,
    ref_version: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum GitWatcherHealth {
    Healthy,
    FallbackRequired,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum GitWatchError {
    #[error("failed to resolve Git watcher root {path}: {source}")]
    Root {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("Git watcher service is shut down")]
    Shutdown,
}

#[derive(Clone)]
pub(crate) struct GitWatchService {
    inner: Arc<Inner>,
}

struct Inner {
    backend_factory: Arc<dyn GitWatcherBackendFactory>,
    state: Mutex<State>,
    setups: Arc<SetupTracker>,
    setup_cancellation: CancellationToken,
    readiness_timeout: Duration,
    /// The first spacing between full re-registrations after overflows.
    rescan_backoff: Duration,
}

/// The first spacing between full re-registrations; it doubles while overflows
/// continue, up to [`RESCAN_BACKOFF_MAX`].
const RESCAN_BACKOFF: Duration = Duration::from_secs(30);
const RESCAN_BACKOFF_MAX: Duration = Duration::from_secs(30 * 60);

/// A lost-event overflow asks for a full re-registration, which walks the whole
/// tree. On Linux that walk queues one open event per watched directory, so a
/// tree larger than the kernel's event queue overflows again on every
/// re-registration and the watcher would loop forever. The gate spaces full
/// re-registrations out instead; meanwhile status runs on its fallback reads.
#[derive(Default)]
struct RescanGate {
    not_before: Option<tokio::time::Instant>,
    delay: Duration,
    deferred: bool,
    retry_armed: bool,
}

impl RescanGate {
    /// Whether a full re-registration may start now. A refused one is deferred
    /// until `not_before`.
    fn admit(&mut self, now: tokio::time::Instant, base: Duration) -> bool {
        if let Some(not_before) = self.not_before {
            if now < not_before {
                self.deferred = true;
                return false;
            }
            // A quiet stretch resets the spacing.
            if now >= not_before + RESCAN_BACKOFF_MAX {
                self.delay = Duration::ZERO;
            }
        }
        self.delay = if self.delay.is_zero() {
            base
        } else {
            (self.delay * 2).min(RESCAN_BACKOFF_MAX)
        };
        self.not_before = Some(now + self.delay);
        self.deferred = false;
        true
    }
}

#[derive(Default)]
struct SetupTracker {
    active: AtomicUsize,
    idle: Notify,
}

#[derive(Default)]
struct SetupCompletion {
    finished: std::sync::atomic::AtomicBool,
    notify: Notify,
}

struct SetupGuard {
    tracker: Arc<SetupTracker>,
    completion: Arc<SetupCompletion>,
}

#[derive(Default)]
struct State {
    shutdown: bool,
    next_generation: u64,
    next_subscriber_id: u64,
    entries: HashMap<GitWatchIdentity, WatchEntry>,
    retiring: HashMap<GitWatchIdentity, Arc<SetupCompletion>>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct GitWatchIdentity {
    worktree_root: String,
    git_dir: String,
    common_dir: String,
}

struct WatchEntry {
    generation: u64,
    subscribers: HashSet<u64>,
    sender: watch::Sender<GitWatchSignal>,
    health: Arc<AtomicU8>,
    backend: Option<InstalledWatcher>,
    registration_requests: PendingRegistrations,
    registration_event: Option<GitWatchEvent>,
    cancellation: CancellationToken,
    roots: GitWatchIdentity,
    root_aliases: Vec<GitWatchIdentity>,
    registered_roots: Vec<(String, RecursiveMode)>,
    setup: Arc<SetupCompletion>,
    rescans: RescanGate,
}

struct InstalledWatcher {
    backend: Box<dyn GitWatcherBackend>,
    registration: WatchRegistration,
}

enum BackendUpdate {
    Installed {
        watcher: InstalledWatcher,
        refresh: Option<GitWatchEvent>,
    },
    Unavailable {
        watcher: Option<InstalledWatcher>,
        refresh: Option<GitWatchEvent>,
    },
}

struct RegistrationWork {
    watcher: InstalledWatcher,
    requests: PendingRegistrations,
    event: GitWatchEvent,
    cancellation: CancellationToken,
    setup: SetupGuard,
}

struct AdmittedRoots {
    identity: GitWatchIdentity,
    alias: GitWatchIdentity,
    worktree_root: PathBuf,
    git_dir: PathBuf,
    common_dir: PathBuf,
}

pub(crate) struct GitWatchSubscription {
    inner: Weak<Inner>,
    identity: GitWatchIdentity,
    generation: u64,
    subscriber_id: u64,
    receiver: watch::Receiver<GitWatchSignal>,
    ref_version: u64,
    health: Arc<AtomicU8>,
    setup: Arc<SetupCompletion>,
}

type BackendCallback = Arc<dyn Fn(notify::Result<Event>) + Send + Sync>;

trait GitWatcherBackendFactory: Send + Sync {
    fn create(
        &self,
        callback: BackendCallback,
        config: Config,
    ) -> notify::Result<Box<dyn GitWatcherBackend>>;
}

trait GitWatcherBackend: Send {
    fn watch(&mut self, path: &Path, recursive_mode: RecursiveMode) -> notify::Result<()>;
    fn unwatch(&mut self, path: &Path) -> notify::Result<()>;

    fn readiness_probe_written(&mut self, _sentinel: &Path) {}
}

#[cfg(test)]
#[derive(Default)]
pub(super) struct GitWatchSetupGate {
    entered: AtomicBool,
    entered_notify: Notify,
    released: Mutex<bool>,
    release_notify: std::sync::Condvar,
}

#[cfg(test)]
impl GitWatchSetupGate {
    fn block(&self) {
        self.entered.store(true, Ordering::Release);
        self.entered_notify.notify_waiters();
        let released = self.released.lock().expect("setup-gate release lock");
        let (released, _) = self
            .release_notify
            .wait_timeout_while(released, Duration::from_secs(10), |released| !*released)
            .expect("setup-gate release wait");
        assert!(*released, "timed out waiting for setup-gate release");
    }

    pub(super) async fn wait_until_entered(&self) {
        loop {
            let entered = self.entered_notify.notified();
            if self.entered.load(Ordering::Acquire) {
                return;
            }
            entered.await;
        }
    }

    pub(super) fn release(&self) {
        *self.released.lock().expect("setup-gate release lock") = true;
        self.release_notify.notify_all();
    }
}

#[cfg(test)]
struct GatedWatcherBackendFactory {
    gate: Arc<GitWatchSetupGate>,
}

#[cfg(test)]
struct GatedWatcherBackend {
    callback: BackendCallback,
    gate: Arc<GitWatchSetupGate>,
    watch_calls: usize,
}

#[cfg(test)]
impl GitWatcherBackendFactory for GatedWatcherBackendFactory {
    fn create(
        &self,
        callback: BackendCallback,
        _config: Config,
    ) -> notify::Result<Box<dyn GitWatcherBackend>> {
        Ok(Box::new(GatedWatcherBackend {
            callback,
            gate: Arc::clone(&self.gate),
            watch_calls: 0,
        }))
    }
}

#[cfg(test)]
impl GitWatcherBackend for GatedWatcherBackend {
    fn watch(&mut self, _path: &Path, _recursive_mode: RecursiveMode) -> notify::Result<()> {
        self.watch_calls += 1;
        if self.watch_calls == 1 {
            self.gate.block();
        }
        Ok(())
    }

    fn unwatch(&mut self, _path: &Path) -> notify::Result<()> {
        Ok(())
    }

    fn readiness_probe_written(&mut self, sentinel: &Path) {
        (self.callback)(Ok(
            Event::new(EventKind::Any).add_path(sentinel.to_path_buf())
        ));
    }
}

struct NativeWatcherBackendFactory;

struct NativeWatcherBackend {
    // Linux prunes Git-ignored directories from the worktree's recursive watch (#88);
    // macOS and Windows watch a whole tree with one native handle.
    #[cfg(target_os = "linux")]
    watcher: pruned::PrunedWatcher,
    #[cfg(not(target_os = "linux"))]
    watcher: notify::RecommendedWatcher,
}

impl GitWatcherBackendFactory for NativeWatcherBackendFactory {
    fn create(
        &self,
        callback: BackendCallback,
        config: Config,
    ) -> notify::Result<Box<dyn GitWatcherBackend>> {
        #[cfg(target_os = "linux")]
        let watcher = pruned::PrunedWatcher::new(move |event| callback(event), config)?;
        #[cfg(not(target_os = "linux"))]
        let watcher = notify::RecommendedWatcher::new(move |event| callback(event), config)?;
        Ok(Box::new(NativeWatcherBackend { watcher }))
    }
}

impl GitWatcherBackend for NativeWatcherBackend {
    fn watch(&mut self, path: &Path, recursive_mode: RecursiveMode) -> notify::Result<()> {
        self.watcher.watch(path, recursive_mode)
    }

    fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
        self.watcher.unwatch(path)
    }
}

impl GitWatchService {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                backend_factory: Arc::new(NativeWatcherBackendFactory),
                state: Mutex::new(State::default()),
                setups: Arc::new(SetupTracker::default()),
                setup_cancellation: CancellationToken::new(),
                readiness_timeout: READINESS_TIMEOUT,
                rescan_backoff: RESCAN_BACKOFF,
            }),
        }
    }

    #[cfg(test)]
    fn with_backend_factory(backend_factory: Arc<dyn GitWatcherBackendFactory>) -> Self {
        let mut service = Self::new();
        Arc::get_mut(&mut service.inner)
            .expect("new watcher service is uniquely owned")
            .backend_factory = backend_factory;
        service
    }

    #[cfg(test)]
    pub(super) fn with_exhausted_backend_for_test() -> Self {
        struct ExhaustedBackendFactory;

        impl GitWatcherBackendFactory for ExhaustedBackendFactory {
            fn create(
                &self,
                _callback: BackendCallback,
                _config: Config,
            ) -> notify::Result<Box<dyn GitWatcherBackend>> {
                #[cfg(unix)]
                let error = io::Error::from_raw_os_error(libc::EMFILE);
                #[cfg(not(unix))]
                let error = io::Error::other("too many open files (EMFILE)");
                Err(notify::Error::io(error))
            }
        }

        Self::with_backend_factory(Arc::new(ExhaustedBackendFactory))
    }

    #[cfg(test)]
    pub(super) fn blocked_during_setup_for_test() -> (Self, Arc<GitWatchSetupGate>) {
        let gate = Arc::new(GitWatchSetupGate::default());
        let service = Self::with_backend_factory(Arc::new(GatedWatcherBackendFactory {
            gate: Arc::clone(&gate),
        }));
        (service, gate)
    }

    #[cfg(test)]
    pub(super) async fn wait_for_setup_cancellation_for_test(&self) {
        self.inner.setup_cancellation.cancelled().await;
    }

    #[cfg(test)]
    fn with_backend_factory_and_rescan_backoff(
        backend_factory: Arc<dyn GitWatcherBackendFactory>,
        rescan_backoff: Duration,
    ) -> Self {
        let mut service = Self::with_backend_factory(backend_factory);
        Arc::get_mut(&mut service.inner)
            .expect("new watcher service is uniquely owned")
            .rescan_backoff = rescan_backoff;
        service
    }

    #[cfg(test)]
    fn with_backend_factory_and_readiness_timeout(
        backend_factory: Arc<dyn GitWatcherBackendFactory>,
        readiness_timeout: Duration,
    ) -> Self {
        let mut service = Self::with_backend_factory(backend_factory);
        Arc::get_mut(&mut service.inner)
            .expect("new watcher service is uniquely owned")
            .readiness_timeout = readiness_timeout;
        service
    }

    pub(crate) async fn subscribe(
        &self,
        request: GitWatchRequest,
    ) -> Result<GitWatchSubscription, GitWatchError> {
        let roots = admit_roots(request).await?;
        loop {
            let retired = self
                .inner
                .lock_state()
                .retiring
                .get(&roots.identity)
                .cloned();
            if let Some(retired) = retired {
                retired.wait().await;
                self.inner.clear_retired(&roots.identity, &retired);
                continue;
            }
            let (subscription, install) = {
                let mut state = self.inner.lock_state();
                if state.shutdown {
                    return Err(GitWatchError::Shutdown);
                }
                if state.retiring.contains_key(&roots.identity) {
                    continue;
                }
                let subscriber_id = state.next_subscriber_id;
                state.next_subscriber_id = state.next_subscriber_id.wrapping_add(1);
                if let Some(entry) = state.entries.get_mut(&roots.identity) {
                    if !entry.root_aliases.contains(&roots.alias) {
                        entry.root_aliases.push(roots.alias.clone());
                    }
                    entry.subscribers.insert(subscriber_id);
                    (
                        GitWatchSubscription::new(
                            Arc::downgrade(&self.inner),
                            roots.identity.clone(),
                            entry,
                            subscriber_id,
                        ),
                        None,
                    )
                } else {
                    let generation = state.next_generation;
                    state.next_generation = state.next_generation.wrapping_add(1);
                    let (sender, receiver) = watch::channel(GitWatchSignal::default());
                    let health = Arc::new(AtomicU8::new(0));
                    let setup = Arc::new(SetupCompletion::default());
                    let watch_roots = watch_roots(&roots);
                    let registration = WatchRegistration::new(watch_roots.clone());
                    let cancellation = self.inner.setup_cancellation.child_token();
                    let registered_roots = normalize_registered_roots(&watch_roots);
                    let mut root_aliases = vec![roots.identity.clone()];
                    if roots.alias != roots.identity {
                        root_aliases.push(roots.alias.clone());
                    }
                    state.entries.insert(
                        roots.identity.clone(),
                        WatchEntry {
                            generation,
                            subscribers: HashSet::from([subscriber_id]),
                            sender,
                            health: Arc::clone(&health),
                            backend: None,
                            registration_requests: PendingRegistrations::default(),
                            registration_event: None,
                            cancellation: cancellation.clone(),
                            roots: roots.identity.clone(),
                            root_aliases,
                            registered_roots,
                            setup: Arc::clone(&setup),
                            rescans: RescanGate::default(),
                        },
                    );
                    (
                        GitWatchSubscription {
                            inner: Arc::downgrade(&self.inner),
                            identity: roots.identity.clone(),
                            generation,
                            subscriber_id,
                            receiver,
                            ref_version: 0,
                            health,
                            setup: Arc::clone(&setup),
                        },
                        Some((
                            generation,
                            watch_roots,
                            registration,
                            cancellation,
                            SetupGuard::reserve(Arc::clone(&self.inner.setups), setup),
                        )),
                    )
                }
            };
            if let Some((generation, watch_roots, registration, cancellation, setup)) = install {
                self.install_backend(
                    subscription.identity.clone(),
                    generation,
                    watch_roots,
                    registration,
                    cancellation,
                    setup,
                );
            }
            subscription.setup.wait().await;
            if self.inner.lock_state().shutdown {
                return Err(GitWatchError::Shutdown);
            }
            return Ok(subscription);
        }
    }

    #[cfg(test)]
    pub(super) fn active_count_for_test(&self) -> usize {
        self.inner.lock_state().entries.len()
    }

    #[cfg(test)]
    pub(super) fn only_generation_for_test(&self) -> u64 {
        let state = self.inner.lock_state();
        assert_eq!(state.entries.len(), 1, "one watcher entry is active");
        state
            .entries
            .values()
            .next()
            .expect("one watcher entry exists")
            .generation
    }

    #[cfg(test)]
    pub(super) fn force_only_entry_fallback_for_test(&self) {
        let mut state = self.inner.lock_state();
        assert_eq!(state.entries.len(), 1, "one watcher entry is active");
        publish(
            state
                .entries
                .values_mut()
                .next()
                .expect("one watcher entry exists"),
            GitWatchEvent::Overflow,
        );
    }

    #[cfg(test)]
    pub(super) fn only_health_for_test(&self) -> GitWatcherHealth {
        let state = self.inner.lock_state();
        assert_eq!(state.entries.len(), 1, "one watcher entry is active");
        if state
            .entries
            .values()
            .next()
            .expect("one watcher entry exists")
            .health
            .load(Ordering::Acquire)
            == 0
        {
            GitWatcherHealth::Healthy
        } else {
            GitWatcherHealth::FallbackRequired
        }
    }

    #[cfg(test)]
    fn setup_count_for_test(&self) -> usize {
        self.inner.setups.active.load(Ordering::Acquire)
    }

    pub(crate) async fn shutdown(&self) {
        self.inner.setup_cancellation.cancel();
        let entries = {
            let mut state = self.inner.lock_state();
            state.shutdown = true;
            state.retiring.clear();
            for entry in state.entries.values() {
                entry.cancellation.cancel();
            }
            std::mem::take(&mut state.entries)
        };
        drop(entries);
        self.inner.setups.wait_until_idle().await;
    }

    fn install_backend(
        &self,
        identity: GitWatchIdentity,
        generation: u64,
        watch_roots: Vec<(PathBuf, RecursiveMode)>,
        registration: WatchRegistration,
        setup_cancellation: CancellationToken,
        setup: SetupGuard,
    ) {
        let inner = Arc::clone(&self.inner);
        let blocking_inner = Arc::clone(&inner);
        let blocking_identity = identity.clone();
        let readiness_timeout = inner.readiness_timeout;
        let runtime = tokio::runtime::Handle::current();
        let callback_runtime = runtime.clone();
        let completion_runtime = runtime.clone();
        let blocking_task = tokio::task::spawn_blocking(move || {
            let callback_inner = Arc::downgrade(&blocking_inner);
            let callback_identity = blocking_identity;
            let normal_callback = Arc::new(move |event| {
                let Some(inner) = callback_inner.upgrade() else {
                    return;
                };
                inner.handle_backend_event(
                    &callback_identity,
                    generation,
                    event,
                    &callback_runtime,
                );
            });
            let backend = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                install_backend(
                    blocking_inner.backend_factory.as_ref(),
                    normal_callback,
                    &watch_roots,
                    registration,
                    &setup_cancellation,
                    readiness_timeout,
                    &runtime,
                )
            }))
            .unwrap_or_else(|_| Err(notify::Error::generic("Git watcher setup panicked")));
            (setup, backend)
        });
        drop(tokio::spawn(async move {
            if let Ok((setup, backend)) = blocking_task.await {
                let completion = setup.completion.clone();
                inner.commit_backend(
                    &identity,
                    generation,
                    match backend {
                        Ok(watcher) => BackendUpdate::Installed {
                            watcher,
                            refresh: None,
                        },
                        Err(_) => BackendUpdate::Unavailable {
                            watcher: None,
                            refresh: None,
                        },
                    },
                    &completion_runtime,
                );
                drop(setup);
                inner.clear_retired(&identity, &completion);
            }
        }));
    }
}

impl SetupGuard {
    fn reserve(tracker: Arc<SetupTracker>, completion: Arc<SetupCompletion>) -> Self {
        tracker.active.fetch_add(1, Ordering::AcqRel);
        Self {
            tracker,
            completion,
        }
    }
}

impl Drop for SetupGuard {
    fn drop(&mut self) {
        self.completion.finish();
        if self.tracker.active.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.tracker.idle.notify_waiters();
        }
    }
}

impl SetupCompletion {
    fn finish(&self) {
        self.finished.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            if self.finished.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
}

impl SetupTracker {
    async fn wait_until_idle(&self) {
        loop {
            let notified = self.idle.notified();
            if self.active.load(Ordering::Acquire) == 0 {
                return;
            }
            notified.await;
        }
    }
}

impl Inner {
    fn lock_state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn release(&self, identity: &GitWatchIdentity, generation: u64, subscriber_id: u64) {
        let removed = {
            let mut state = self.lock_state();
            let Some(entry) = state.entries.get_mut(identity) else {
                return;
            };
            if entry.generation != generation {
                return;
            }
            entry.subscribers.remove(&subscriber_id);
            if !entry.subscribers.is_empty() {
                return;
            }
            let entry = state.entries.remove(identity).expect("admitted watcher");
            entry.cancellation.cancel();
            if entry.backend.is_none() && !entry.setup.finished.load(Ordering::Acquire) {
                state.retiring.insert(identity.clone(), entry.setup.clone());
            }
            entry
        };
        drop(removed);
    }

    fn clear_retired(&self, identity: &GitWatchIdentity, completion: &Arc<SetupCompletion>) {
        let mut state = self.lock_state();
        if state
            .retiring
            .get(identity)
            .is_some_and(|current| Arc::ptr_eq(current, completion))
            && completion.finished.load(Ordering::Acquire)
        {
            state.retiring.remove(identity);
        }
    }

    fn handle_backend_event(
        self: &Arc<Self>,
        identity: &GitWatchIdentity,
        generation: u64,
        event: notify::Result<Event>,
        runtime: &tokio::runtime::Handle,
    ) {
        let work = {
            let mut state = self.lock_state();
            let Some(entry) = state.entries.get_mut(identity) else {
                return;
            };
            if entry.generation != generation {
                return;
            }
            let invalidation = match event {
                Ok(event) => {
                    // The backend is out while it installs or registers watches.
                    if host_path_platform() == super::HostPathPlatform::Windows
                        && entry.backend.is_none()
                        && windows_installing_nested_root_artifact(
                            &event,
                            &entry.roots,
                            &entry.root_aliases,
                            &entry.registered_roots,
                            host_path_platform(),
                        )
                    {
                        return;
                    }
                    let invalidation = classify_event(
                        &event,
                        &entry.roots,
                        &entry.root_aliases,
                        &entry.registered_roots,
                        host_path_platform(),
                    );
                    if event.need_rescan() {
                        let now = tokio::time::Instant::now();
                        if entry.rescans.admit(now, self.rescan_backoff) {
                            entry.registration_requests.rescan();
                            entry.registration_event = Some(GitWatchEvent::RefMetadata);
                        } else if !entry.rescans.retry_armed
                            && let Some(not_before) = entry.rescans.not_before
                        {
                            entry.rescans.retry_armed = true;
                            self.schedule_deferred_rescan(
                                identity.clone(),
                                generation,
                                entry.cancellation.clone(),
                                not_before,
                                runtime,
                            );
                        }
                    } else if event_changes_directories(event.kind) {
                        for path in &event.paths {
                            let key = normalize_event_path_key(
                                path,
                                &entry.roots,
                                &entry.root_aliases,
                                host_path_platform(),
                            );
                            // Native recursion covers everything below a store root, so
                            // file renames and nested directories need no registration.
                            if registration::coverage(
                                &entry.registered_roots,
                                &key,
                                host_path_platform(),
                            ) == Coverage::StoreRoot
                            {
                                entry.registration_requests.add(
                                    key,
                                    PendingPath {
                                        native_path: path.clone(),
                                        change: if matches!(event.kind, EventKind::Create(_)) {
                                            DirectoryChange::Created
                                        } else {
                                            DirectoryChange::Replaced
                                        },
                                    },
                                );
                                let refresh = invalidation.unwrap_or(GitWatchEvent::RefMetadata);
                                if entry.registration_event != Some(GitWatchEvent::RefMetadata) {
                                    entry.registration_event = Some(refresh);
                                }
                            }
                        }
                    }
                    invalidation
                }
                Err(_) => Some(GitWatchEvent::Unavailable),
            };
            if let Some(event) = invalidation {
                publish(entry, event);
            }
            self.take_registration_work(entry)
        };
        if let Some(work) = work {
            self.start_registration(identity.clone(), generation, work, runtime);
        }
    }

    fn schedule_deferred_rescan(
        self: &Arc<Self>,
        identity: GitWatchIdentity,
        generation: u64,
        cancellation: CancellationToken,
        not_before: tokio::time::Instant,
        runtime: &tokio::runtime::Handle,
    ) {
        let inner = Arc::downgrade(self);
        let handle = runtime.clone();
        runtime.spawn(async move {
            tokio::select! {
                () = cancellation.cancelled() => return,
                () = tokio::time::sleep_until(not_before) => {}
            }
            if let Some(inner) = inner.upgrade() {
                inner.run_deferred_rescan(&identity, generation, &handle);
            }
        });
    }

    fn run_deferred_rescan(
        self: &Arc<Self>,
        identity: &GitWatchIdentity,
        generation: u64,
        runtime: &tokio::runtime::Handle,
    ) {
        let work = {
            let mut state = self.lock_state();
            let Some(entry) = state.entries.get_mut(identity).filter(|entry| {
                entry.generation == generation && !entry.cancellation.is_cancelled()
            }) else {
                return;
            };
            entry.rescans.retry_armed = false;
            if !entry.rescans.deferred {
                return;
            }
            if !entry
                .rescans
                .admit(tokio::time::Instant::now(), self.rescan_backoff)
            {
                // Another overflow moved the deadline while this timer waited: keep
                // the deferred recovery armed for the new one.
                if let Some(not_before) = entry.rescans.not_before {
                    entry.rescans.retry_armed = true;
                    self.schedule_deferred_rescan(
                        identity.clone(),
                        generation,
                        entry.cancellation.clone(),
                        not_before,
                        runtime,
                    );
                }
                return;
            }
            entry.registration_requests.rescan();
            entry.registration_event = Some(GitWatchEvent::RefMetadata);
            self.take_registration_work(entry)
        };
        if let Some(work) = work {
            self.start_registration(identity.clone(), generation, work, runtime);
        }
    }

    fn take_registration_work(&self, entry: &mut WatchEntry) -> Option<RegistrationWork> {
        if entry.registration_requests.is_empty() || entry.cancellation.is_cancelled() {
            return None;
        }
        let watcher = entry.backend.take()?;
        let completion = Arc::new(SetupCompletion::default());
        entry.setup = completion.clone();
        Some(RegistrationWork {
            watcher,
            requests: std::mem::take(&mut entry.registration_requests),
            event: entry
                .registration_event
                .take()
                .unwrap_or(GitWatchEvent::WorkingTree),
            cancellation: entry.cancellation.clone(),
            setup: SetupGuard::reserve(self.setups.clone(), completion),
        })
    }

    fn start_registration(
        self: &Arc<Self>,
        identity: GitWatchIdentity,
        generation: u64,
        work: RegistrationWork,
        runtime: &tokio::runtime::Handle,
    ) {
        let inner = self.clone();
        let runtime = runtime.clone();
        runtime.clone().spawn_blocking(move || {
            let RegistrationWork {
                mut watcher,
                requests,
                event,
                cancellation,
                setup,
            } = work;
            let full_rescan = requests.is_full_rescan();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                watcher
                    .registration
                    .refresh(watcher.backend.as_mut(), requests, &cancellation)
            }))
            .unwrap_or_else(|_| Err(notify::Error::generic("Git watch registration panicked")));
            let refresh = if full_rescan {
                Some(GitWatchEvent::RefMetadata)
            } else {
                result
                    .as_ref()
                    .is_ok_and(|changed| *changed)
                    .then_some(event)
            };
            let update = match result {
                Ok(_) => BackendUpdate::Installed { watcher, refresh },
                Err(_) => BackendUpdate::Unavailable {
                    watcher: Some(watcher),
                    refresh,
                },
            };
            let completion = setup.completion.clone();
            inner.commit_backend(&identity, generation, update, &runtime);
            drop(setup);
            inner.clear_retired(&identity, &completion);
        });
    }

    fn commit_backend(
        self: &Arc<Self>,
        identity: &GitWatchIdentity,
        generation: u64,
        update: BackendUpdate,
        runtime: &tokio::runtime::Handle,
    ) {
        let work = {
            let mut state = self.lock_state();
            let Some(entry) = state.entries.get_mut(identity).filter(|entry| {
                entry.generation == generation && !entry.cancellation.is_cancelled()
            }) else {
                drop(state);
                drop(update);
                return;
            };
            let refresh = match update {
                BackendUpdate::Installed { watcher, refresh } => {
                    entry.backend = Some(watcher);
                    refresh
                }
                BackendUpdate::Unavailable { watcher, refresh } => {
                    entry.backend = watcher;
                    publish(entry, GitWatchEvent::Unavailable);
                    refresh
                }
            };
            if let Some(event) = refresh {
                publish(entry, event);
            }
            self.take_registration_work(entry)
        };
        if let Some(work) = work {
            self.start_registration(identity.clone(), generation, work, runtime);
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.setup_cancellation.cancel();
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.shutdown = true;
        state.entries.clear();
    }
}

impl Drop for GitWatchSubscription {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.upgrade() {
            inner.release(&self.identity, self.generation, self.subscriber_id);
        }
    }
}

impl GitWatchSubscription {
    fn new(
        inner: Weak<Inner>,
        identity: GitWatchIdentity,
        entry: &WatchEntry,
        subscriber_id: u64,
    ) -> Self {
        Self {
            inner,
            identity,
            generation: entry.generation,
            subscriber_id,
            receiver: entry.sender.subscribe(),
            ref_version: entry.sender.borrow().ref_version,
            health: Arc::clone(&entry.health),
            setup: Arc::clone(&entry.setup),
        }
    }

    pub(crate) async fn recv(&mut self) -> Option<GitWatchEvent> {
        loop {
            if self.receiver.changed().await.is_err() {
                return None;
            }
            if let Some(event) = self.take_signal() {
                return Some(event);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn try_recv(&mut self) -> Option<GitWatchEvent> {
        self.take_current()
    }

    pub(crate) fn health(&self) -> GitWatcherHealth {
        if self.health.load(Ordering::Acquire) == 0 {
            GitWatcherHealth::Healthy
        } else {
            GitWatcherHealth::FallbackRequired
        }
    }

    #[cfg(test)]
    fn take_current(&mut self) -> Option<GitWatchEvent> {
        if !self.receiver.has_changed().unwrap_or(false) {
            return None;
        }
        self.take_signal()
    }

    fn take_signal(&mut self) -> Option<GitWatchEvent> {
        let signal = *self.receiver.borrow_and_update();
        if signal.ref_version != self.ref_version {
            self.ref_version = signal.ref_version;
            // Ordinary events cannot overwrite an unconsumed ref invalidation.
            // Fallback health is sticky and observed separately by the scheduler.
            Some(GitWatchEvent::RefMetadata)
        } else {
            signal.event
        }
    }
}

fn install_backend(
    factory: &dyn GitWatcherBackendFactory,
    normal_callback: BackendCallback,
    watch_roots: &[(PathBuf, RecursiveMode)],
    mut registration: WatchRegistration,
    cancellation: &CancellationToken,
    readiness_timeout: Duration,
    runtime: &tokio::runtime::Handle,
) -> notify::Result<InstalledWatcher> {
    let readiness = Arc::new(ReadinessAcknowledgement::default());
    let probe = create_readiness_probe(watch_roots)?;
    let sentinel_key = normalize_worktree_path_key(&probe.sentinel, host_path_platform());
    let callback_readiness = Arc::clone(&readiness);
    let callback = Arc::new(move |event: notify::Result<Event>| {
        let is_readiness = event.as_ref().is_ok_and(|event| {
            event
                .paths
                .iter()
                .any(|path| normalize_worktree_path_key(path, host_path_platform()) == sentinel_key)
        });
        normal_callback(event);
        if is_readiness {
            callback_readiness.observe();
        }
    });
    let config = Config::default().with_follow_symlinks(false);
    let mut backend = match factory.create(callback, config) {
        Ok(backend) => backend,
        Err(error) => {
            cleanup_readiness_files(&probe)?;
            return Err(error);
        }
    };
    if let Err(error) = registration.install(backend.as_mut(), cancellation) {
        let _ = registration.remove_subtree(backend.as_mut(), None);
        cleanup_readiness_files(&probe)?;
        return Err(error);
    }
    if let Err(error) = backend.watch(&probe.root, RecursiveMode::NonRecursive) {
        let _ = registration.remove_subtree(backend.as_mut(), None);
        cleanup_readiness_files(&probe)?;
        return Err(error);
    }
    let readiness_result = (|| {
        let mut sentinel = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&probe.sentinel)
            .map_err(notify::Error::io)?;
        sentinel
            .write_all(b"ready")
            .and_then(|()| sentinel.sync_all())
            .map_err(notify::Error::io)?;
        drop(sentinel);
        backend.readiness_probe_written(&probe.sentinel);
        runtime.block_on(async {
            tokio::select! {
                biased;
                () = cancellation.cancelled() => {
                    Err(notify::Error::generic("Git watcher readiness was cancelled"))
                }
                result = tokio::time::timeout(readiness_timeout, readiness.wait()) => {
                    result.map_err(|_| notify::Error::generic("Git watcher readiness timed out"))
                }
            }
        })
    })();
    let readiness_unwatch = backend.unwatch(&probe.root);
    let readiness_cleanup = cleanup_readiness_files(&probe);
    if let Err(error) = readiness_result
        .and(readiness_unwatch)
        .and(readiness_cleanup)
    {
        let _ = registration.remove_subtree(backend.as_mut(), None);
        return Err(error);
    }
    Ok(InstalledWatcher {
        backend,
        registration,
    })
}

#[derive(Default)]
struct ReadinessAcknowledgement {
    observed: AtomicBool,
    notify: Notify,
}

impl ReadinessAcknowledgement {
    fn observe(&self) {
        self.observed.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            if self.observed.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
}

struct ReadinessProbe {
    root: PathBuf,
    sentinel: PathBuf,
}

fn create_readiness_probe(
    admitted_roots: &[(PathBuf, RecursiveMode)],
) -> notify::Result<ReadinessProbe> {
    create_readiness_probe_with(
        admitted_roots,
        &std::env::temp_dir(),
        |path| fs::create_dir(path),
        |path| fs::canonicalize(path),
        |path| fs::remove_dir(path),
    )
}

fn create_readiness_probe_with<Create, Canonicalize, Remove>(
    admitted_roots: &[(PathBuf, RecursiveMode)],
    temporary_root: &Path,
    create_dir: Create,
    canonicalize: Canonicalize,
    remove_dir: Remove,
) -> notify::Result<ReadinessProbe>
where
    Create: Fn(&Path) -> io::Result<()>,
    Canonicalize: Fn(&Path) -> io::Result<PathBuf>,
    Remove: Fn(&Path) -> io::Result<()>,
{
    for _ in 0..1_024 {
        let id = NEXT_READINESS_DIRECTORY_ID.fetch_add(1, Ordering::Relaxed);
        let root = temporary_root.join(format!(
            "{READINESS_DIRECTORY_PREFIX}-{}-{id}",
            process::id()
        ));
        match create_dir(&root) {
            Ok(()) => {
                let root = match canonicalize(&root) {
                    Ok(root) => root,
                    Err(error) => {
                        return Err(readiness_probe_failure(
                            &root,
                            notify::Error::io(error),
                            &remove_dir,
                        ));
                    }
                };
                let root_key = normalize_worktree_path_key(&root, host_path_platform());
                if admitted_roots.iter().any(|(admitted, _)| {
                    let admitted = normalize_worktree_path_key(admitted, host_path_platform());
                    relative_to_root(&root_key, &admitted).is_some()
                }) {
                    return Err(readiness_probe_failure(
                        &root,
                        notify::Error::generic(
                            "Git watcher readiness directory overlaps an admitted root",
                        ),
                        &remove_dir,
                    ));
                }
                return Ok(ReadinessProbe {
                    sentinel: root.join("ready"),
                    root,
                });
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => return Err(notify::Error::io(error)),
        }
    }
    Err(notify::Error::generic(
        "Git watcher readiness directory allocation was exhausted",
    ))
}

fn readiness_probe_failure(
    root: &Path,
    primary: notify::Error,
    remove_dir: &impl Fn(&Path) -> io::Result<()>,
) -> notify::Error {
    match remove_dir(root) {
        Ok(()) => primary,
        Err(error) if error.kind() == ErrorKind::NotFound => primary,
        Err(cleanup) => notify::Error::generic(&format!(
            "{primary}; readiness probe cleanup failed for {}: {cleanup}",
            root.display()
        )),
    }
}

fn cleanup_readiness_files(probe: &ReadinessProbe) -> notify::Result<()> {
    let sentinel = match fs::remove_file(&probe.sentinel) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(notify::Error::io(error)),
    };
    let root = fs::remove_dir(&probe.root).map_err(notify::Error::io);
    sentinel.and(root)
}

#[cfg(test)]
fn is_readiness_directory(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        name.to_string_lossy()
            .starts_with(READINESS_DIRECTORY_PREFIX)
    })
}

fn publish(entry: &mut WatchEntry, event: GitWatchEvent) {
    if matches!(event, GitWatchEvent::Overflow | GitWatchEvent::Unavailable) {
        entry.health.store(1, Ordering::Release);
    }
    entry.sender.send_modify(|signal| {
        signal.event = Some(event);
        if event == GitWatchEvent::RefMetadata {
            signal.ref_version = signal.ref_version.wrapping_add(1);
        }
    });
}

async fn admit_roots(request: GitWatchRequest) -> Result<AdmittedRoots, GitWatchError> {
    let platform = host_path_platform();
    let alias = identity_for_paths(
        &request.worktree_root,
        &request.git_dir,
        &request.common_dir,
        platform,
    );
    let (worktree_root, git_dir, common_dir) = tokio::try_join!(
        canonical_root(request.worktree_root),
        canonical_root(request.git_dir),
        canonical_root(request.common_dir),
    )?;
    Ok(AdmittedRoots {
        identity: identity_for_paths(&worktree_root, &git_dir, &common_dir, platform),
        alias,
        worktree_root,
        git_dir,
        common_dir,
    })
}

fn identity_for_paths(
    worktree_root: impl AsRef<Path>,
    git_dir: impl AsRef<Path>,
    common_dir: impl AsRef<Path>,
    platform: super::HostPathPlatform,
) -> GitWatchIdentity {
    GitWatchIdentity {
        worktree_root: normalize_worktree_path_key(worktree_root.as_ref(), platform),
        git_dir: normalize_worktree_path_key(git_dir.as_ref(), platform),
        common_dir: normalize_worktree_path_key(common_dir.as_ref(), platform),
    }
}

fn classify_event(
    event: &Event,
    roots: &GitWatchIdentity,
    root_aliases: &[GitWatchIdentity],
    registered_roots: &[(String, RecursiveMode)],
    platform: super::HostPathPlatform,
) -> Option<GitWatchEvent> {
    if event.need_rescan() {
        return Some(GitWatchEvent::Overflow);
    }
    if !event_may_change_status(event.kind) {
        return None;
    }
    if event.paths.is_empty() {
        return Some(GitWatchEvent::Unavailable);
    }
    let mut working_tree = false;
    let mut metadata = false;
    let mut ref_metadata = false;
    for path in &event.paths {
        let path = normalize_event_path_key(path, roots, root_aliases, platform);
        let registered_root = registered_roots.iter().any(|(root, _)| *root == path);
        if matches!(
            event.kind,
            EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))
        ) && (registered_root || path == roots.git_dir || path == roots.common_dir)
        {
            return Some(GitWatchEvent::Unavailable);
        }
        let relative = match relative_to_root(&path, &roots.git_dir)
            .or_else(|| relative_to_root(&path, &roots.common_dir))
        {
            Some(relative) => relative,
            None => {
                let Some(relative) = relative_to_root(&path, &roots.worktree_root) else {
                    continue;
                };
                match relative.split_once('/').unwrap_or((relative, "")) {
                    (first, remaining) if component_matches(first, ".git", platform) => remaining,
                    _ => {
                        working_tree = true;
                        continue;
                    }
                }
            }
        };
        match metadata_layout(relative, platform).event(event.kind) {
            Some(GitWatchEvent::RefMetadata) => ref_metadata = true,
            Some(GitWatchEvent::Metadata) => metadata = true,
            _ => {}
        }
    }
    if ref_metadata {
        Some(GitWatchEvent::RefMetadata)
    } else if metadata {
        Some(GitWatchEvent::Metadata)
    } else {
        working_tree.then_some(GitWatchEvent::WorkingTree)
    }
}

fn normalize_event_path_key(
    path: &Path,
    roots: &GitWatchIdentity,
    root_aliases: &[GitWatchIdentity],
    platform: super::HostPathPlatform,
) -> String {
    let path = normalize_worktree_path_key(path, platform);
    for aliases in root_aliases {
        for (alias, canonical) in [
            (&aliases.git_dir, &roots.git_dir),
            (&aliases.common_dir, &roots.common_dir),
            (&aliases.worktree_root, &roots.worktree_root),
        ] {
            if let Some(relative) = relative_to_root(&path, alias) {
                return if relative.is_empty() {
                    canonical.clone()
                } else {
                    format!("{canonical}/{relative}")
                };
            }
        }
    }
    path
}

/// Windows reports installing a store's native recursive watch beneath the
/// non-recursive metadata root as a `Modify(Any)` of the store itself. While the
/// backend is out installing or registering watches, that event is the
/// watcher's own: the store's recursive watch reports real changes inside it,
/// and the identical event after the backend returns stays observable.
fn windows_installing_nested_root_artifact(
    event: &Event,
    roots: &GitWatchIdentity,
    root_aliases: &[GitWatchIdentity],
    registered_roots: &[(String, RecursiveMode)],
    platform: super::HostPathPlatform,
) -> bool {
    event.kind == EventKind::Modify(ModifyKind::Any)
        && event.paths.len() == 1
        && registration::coverage(
            registered_roots,
            &normalize_event_path_key(&event.paths[0], roots, root_aliases, platform),
            platform,
        ) == Coverage::StoreRoot
}

fn event_may_change_status(kind: EventKind) -> bool {
    match kind {
        EventKind::Access(AccessKind::Close(AccessMode::Write)) => true,
        EventKind::Access(_) => false,
        EventKind::Modify(ModifyKind::Metadata(MetadataKind::AccessTime)) => false,
        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_) => true,
        EventKind::Any | EventKind::Other => true,
    }
}

fn relative_to_root<'a>(path: &'a str, root: &str) -> Option<&'a str> {
    if path == root {
        return Some("");
    }
    let remainder = path.strip_prefix(root)?;
    remainder.strip_prefix('/')
}

async fn canonical_root(path: PathBuf) -> Result<PathBuf, GitWatchError> {
    tokio::fs::canonicalize(&path)
        .await
        .map_err(|source| GitWatchError::Root { path, source })
}

fn watch_roots(roots: &AdmittedRoots) -> Vec<(PathBuf, RecursiveMode)> {
    let mut watched = vec![(roots.worktree_root.clone(), RecursiveMode::Recursive)];
    for root in [&roots.common_dir, &roots.git_dir] {
        let key = normalize_worktree_path_key(root, host_path_platform());
        // Worktree recursion covers a main checkout's `.git`, and the common
        // directory's `worktrees` store covers a linked worktree's administrative
        // directory.
        if registration::coverage(
            &normalize_registered_roots(&watched),
            &key,
            host_path_platform(),
        ) == Coverage::Uncovered
        {
            watched.push((root.clone(), RecursiveMode::NonRecursive));
        }
    }
    watched
}

fn event_changes_directories(kind: EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(notify::event::CreateKind::Folder | notify::event::CreateKind::Any)
            | EventKind::Remove(notify::event::RemoveKind::Folder | notify::event::RemoveKind::Any)
            | EventKind::Modify(ModifyKind::Name(_))
    )
}

fn normalize_registered_roots(
    watch_roots: &[(PathBuf, RecursiveMode)],
) -> Vec<(String, RecursiveMode)> {
    let platform = host_path_platform();
    watch_roots
        .iter()
        .map(|(path, mode)| (normalize_worktree_path_key(path, platform), *mode))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    use notify::{
        Event, EventKind,
        event::{AccessKind, DataChange, Flag, ModifyKind, RenameMode},
    };

    use super::*;
    use crate::git::HostPathPlatform;

    #[tokio::test(start_paused = true)]
    async fn native_watcher_permit_timeout_uses_wall_clock_and_names_the_holder() {
        let lock = Arc::new(NativeWatcherTestLock::new());
        let holder_name = std::thread::current()
            .name()
            .expect("named test thread")
            .to_owned();
        let held = lock.acquire(Duration::from_secs(1)).await;
        let started = std::time::Instant::now();
        let timeout = Duration::from_millis(250);
        let waiting = {
            let lock = Arc::clone(&lock);
            tokio::spawn(async move {
                let _permit = lock.acquire(timeout).await;
            })
        };
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(301)).await;
        assert!(
            !waiting.is_finished(),
            "paused Tokio time cannot expire a permit wait"
        );

        let error = waiting
            .await
            .expect_err("a held permit has a real deadline");
        assert!(
            started.elapsed() >= timeout,
            "deadline uses elapsed wall-clock time"
        );
        let message = *error
            .into_panic()
            .downcast::<String>()
            .expect("permit timeout diagnostic");
        assert!(
            message.contains(&format!("held by {holder_name} for ")),
            "{message}"
        );
        assert!(message.contains("timed out after"), "{message}");

        drop(held);
        let _next = lock.acquire(Duration::from_secs(1)).await;
    }

    #[tokio::test(start_paused = true)]
    async fn native_watcher_permit_waiter_acquires_after_release_with_paused_time() {
        let lock = NativeWatcherTestLock::new();
        let held = lock.acquire(Duration::from_secs(300)).await;
        let waiting = lock.acquire(Duration::from_secs(300));
        tokio::pin!(waiting);
        assert!(futures_util::poll!(&mut waiting).is_pending());
        tokio::time::advance(Duration::from_secs(301)).await;
        assert!(futures_util::poll!(&mut waiting).is_pending());
        drop(held);
        assert!(futures_util::poll!(&mut waiting).is_ready());
    }

    #[test]
    fn ref_metadata_is_distinct_from_index_and_config() {
        let roots =
            identity_for_paths("/repo", "/repo/.git", "/repo/.git", HostPathPlatform::Posix);
        let classify = |relative: &str| {
            classify_event(
                &Event::new(
                    if relative == "worktrees" || relative == "worktrees/topic" {
                        EventKind::Create(notify::event::CreateKind::Folder)
                    } else {
                        EventKind::Any
                    },
                )
                .add_path(PathBuf::from(format!("/repo/.git/{relative}"))),
                &roots,
                &[],
                &[],
                HostPathPlatform::Posix,
            )
        };
        for relative in [
            "HEAD",
            "FETCH_HEAD",
            "ORIG_HEAD",
            "MERGE_HEAD",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "packed-refs",
            "refs/heads/main",
            "refs/remotes/origin/main",
            "refs/tags/v1",
            "refs/stash",
            "logs",
            "logs/refs",
            "logs/refs/stash",
            "worktrees",
            "worktrees/topic",
            "worktrees/topic/HEAD",
            "reftable/tables.list",
            "reftable/0x000000000001.ref",
        ] {
            assert_eq!(
                classify(relative),
                Some(GitWatchEvent::RefMetadata),
                "{relative} must invalidate refs"
            );
        }
        for relative in [
            "index",
            "config",
            "logs/HEAD",
            "logs/refs/heads/main",
            // A submodule's Git directory: its commit or checkout changes the
            // superproject's status, not the superproject's refs.
            "modules/sub",
            "modules/sub/HEAD",
            "modules/sub/index",
            "modules/sub/refs/heads/main",
            "modules/sub/logs/HEAD",
        ] {
            assert_eq!(
                classify(relative),
                Some(GitWatchEvent::Metadata),
                "{relative} must refresh local status only"
            );
        }
        for relative in [
            "objects/ab/cdef",
            "objects/pack/pack-1.pack",
            "lfs/objects/ab/cd/data",
            "modules/sub/objects",
            "modules/sub/objects/ab/cdef",
        ] {
            assert_eq!(classify(relative), None, "{relative} is an object store");
        }
    }

    #[test]
    fn sibling_metadata_only_invalidates_head_and_reftable() {
        let roots = identity_for_paths(
            "/linked",
            "/repo/.git/worktrees/current",
            "/repo/.git",
            HostPathPlatform::Posix,
        );
        for (relative, expected) in [
            ("worktrees/sibling/index", None),
            ("worktrees/sibling/logs/HEAD", None),
            ("worktrees/sibling/HEAD", Some(GitWatchEvent::RefMetadata)),
            (
                "worktrees/sibling/reftable/tables.list",
                Some(GitWatchEvent::RefMetadata),
            ),
            ("worktrees/current/index", Some(GitWatchEvent::Metadata)),
            // Submodule Git directories are per worktree.
            (
                "worktrees/current/modules/sub/HEAD",
                Some(GitWatchEvent::Metadata),
            ),
            ("worktrees/current/modules/sub/objects/ab/cd", None),
            ("worktrees/sibling/modules/sub/HEAD", None),
            ("lfs/objects/data", None),
            ("modules/sub/objects/data", None),
        ] {
            let event = Event::new(EventKind::Any)
                .add_path(PathBuf::from(format!("/repo/.git/{relative}")));
            assert_eq!(
                classify_event(&event, &roots, &[], &[], HostPathPlatform::Posix),
                expected,
                "{relative}"
            );
        }
    }

    #[tokio::test]
    async fn native_watch_plan_keeps_worktree_recursion_and_targets_external_metadata() {
        for linked in [false, true] {
            let root = tempfile::tempdir().expect("watch exclusion fixture");
            let worktree = root.path().join("worktree");
            let common = if linked {
                root.path().join("common.git")
            } else {
                worktree.join(".git")
            };
            let git_dir = if linked {
                common.join("worktrees/current")
            } else {
                common.clone()
            };
            for path in [
                &worktree.join("src/nested"),
                &git_dir,
                &common.join("refs/heads"),
                &common.join("logs/refs"),
                &common.join("objects/ab"),
                &common.join("lfs/objects"),
                &common.join("modules/sub/objects/ab"),
            ] {
                std::fs::create_dir_all(path).expect("fixture directory");
            }
            let backend = FakeWatcherBackendFactory::default();
            let service = GitWatchService::with_backend_factory(Arc::new(backend.clone()));
            let subscription = service
                .subscribe(request(&worktree, &git_dir, &common))
                .await
                .expect("watch subscription");
            let watches = backend.state.watches.lock().expect("watches").clone();
            assert!(watches.iter().any(|(_, path, mode)| path
                == &std::fs::canonicalize(&worktree).unwrap()
                && *mode == RecursiveMode::Recursive));
            if !linked {
                assert_eq!(
                    watches.len(),
                    1,
                    "main .git is covered by native worktree recursion"
                );
                drop(subscription);
                service.shutdown().await;
                continue;
            }
            for excluded in [
                common.join("objects/ab"),
                common.join("lfs/objects"),
                common.join("modules/sub/objects/ab"),
            ] {
                let excluded = std::fs::canonicalize(excluded).expect("excluded directory");
                assert!(
                    !watches.iter().any(|(_, path, mode)| path == &excluded
                        || (*mode == RecursiveMode::Recursive && excluded.starts_with(path))),
                    "object store received a native watch: {}",
                    excluded.display()
                );
            }
            drop(subscription);
            service.shutdown().await;
        }
    }

    #[test]
    fn windows_metadata_layout_uses_normalized_component_case() {
        for store in ["REFS", "LOGS", "REFTABLE", "WORKTREES"] {
            assert!(
                metadata_layout(store, HostPathPlatform::Windows)
                    .store
                    .is_some(),
                "{store} must register recursively"
            );
        }
        for store in ["OBJECTS", "LFS", "MODULES"] {
            assert!(
                metadata_layout(store, HostPathPlatform::Windows)
                    .store
                    .is_none(),
                "{store} must not register recursively"
            );
        }

        let roots = identity_for_paths(
            "C:/repo",
            "C:/repo/.git",
            "C:/repo/.git",
            HostPathPlatform::Windows,
        );
        for (relative, expected) in [
            ("REFS/HEADS/MAIN", Some(GitWatchEvent::RefMetadata)),
            ("LOGS/REFS/STASH", Some(GitWatchEvent::RefMetadata)),
            ("REFTABLE/TABLES.LIST", Some(GitWatchEvent::RefMetadata)),
            ("WORKTREES/X/HEAD", Some(GitWatchEvent::RefMetadata)),
            (
                "WORKTREES/X/REFTABLE/TABLES.LIST",
                Some(GitWatchEvent::RefMetadata),
            ),
            ("WORKTREES/X/INDEX", None),
            ("OBJECTS/AB/123", None),
            ("LFS/OBJECTS/123", None),
            ("MODULES/SUB/OBJECTS/123", None),
            ("MODULES/SUB/HEAD", Some(GitWatchEvent::Metadata)),
            ("MODULES/SUB/REFS/HEADS/MAIN", Some(GitWatchEvent::Metadata)),
        ] {
            let event = Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Content)))
                .add_path(PathBuf::from(format!("C:/repo/.GIT/{relative}")));
            assert_eq!(
                classify_event(&event, &roots, &[], &[], HostPathPlatform::Windows),
                expected,
                "{relative}"
            );
        }
        let event = Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Content)))
            .add_path(PathBuf::from("C:/repo/.GIT"));
        assert_eq!(
            classify_event(&event, &roots, &[], &[], HostPathPlatform::Windows),
            Some(GitWatchEvent::Metadata)
        );
    }

    #[test]
    fn windows_setup_artifact_is_only_a_store_modify_any_beneath_a_non_recursive_root() {
        let roots = identity_for_paths(
            r"C:\linked",
            r"C:\repo.git\worktrees\topic",
            r"C:\repo.git",
            HostPathPlatform::Windows,
        );
        let registered = vec![
            (roots.worktree_root.clone(), RecursiveMode::Recursive),
            (roots.common_dir.clone(), RecursiveMode::NonRecursive),
        ];
        let artifact = |kind: EventKind, paths: &[&str]| {
            let event = paths
                .iter()
                .fold(Event::new(kind), |event, path| event.add_path(path.into()));
            windows_installing_nested_root_artifact(
                &event,
                &roots,
                std::slice::from_ref(&roots),
                &registered,
                HostPathPlatform::Windows,
            )
        };
        let modify_any = EventKind::Modify(ModifyKind::Any);
        for store in [
            r"C:\repo.git\refs",
            r"C:\REPO.GIT\LOGS",
            r"c:\repo.git\Reftable",
            r"C:\repo.git\worktrees",
        ] {
            assert!(artifact(modify_any, &[store]), "{store}");
        }
        let not_artifacts: [(EventKind, &[&str], &str); 7] = [
            (modify_any, &[r"C:\repo.git\refs\heads"], "inside a store"),
            (modify_any, &[r"C:\repo.git\index"], "plain root entry"),
            (modify_any, &[r"C:\repo.git\objects"], "object store"),
            (modify_any, &[r"C:\repo.git"], "the root itself"),
            (modify_any, &[r"C:\linked\refs"], "recursive worktree"),
            (
                EventKind::Modify(ModifyKind::Data(DataChange::Content)),
                &[r"C:\repo.git\refs"],
                "another change kind",
            ),
            (
                modify_any,
                &[r"C:\repo.git\refs", r"C:\repo.git\logs"],
                "more than one path",
            ),
        ];
        for (kind, paths, context) in not_artifacts {
            assert!(!artifact(kind, paths), "{context}");
        }
    }

    #[tokio::test]
    async fn nested_root_modify_any_is_suppressed_only_during_windows_installation() {
        let root = tempfile::tempdir().expect("watch fixture");
        let worktree = root.path().join("worktree");
        let git_dir = root.path().join("main.git/worktrees/topic");
        let common_dir = root.path().join("main.git");
        let refs_dir = common_dir.join("refs");
        for path in [&worktree, &git_dir, &refs_dir] {
            std::fs::create_dir_all(path).expect("watch root");
        }
        let backend = FakeWatcherBackendFactory::emitting_nested_root_modify_any_during_watch();
        let service = GitWatchService::with_backend_factory(Arc::new(backend.clone()));
        let mut subscription = service
            .subscribe(request(&worktree, &git_dir, &common_dir))
            .await
            .expect("watch subscription");
        let refs_root = std::fs::canonicalize(&refs_dir).expect("canonical refs store");
        assert!(
            backend
                .state
                .watches
                .lock()
                .expect("watches lock")
                .iter()
                .any(|(_, path, mode)| path == &refs_root && *mode == RecursiveMode::Recursive),
            "the refs store is watched recursively beneath the non-recursive common root"
        );

        #[cfg(windows)]
        assert_eq!(
            subscription.try_recv(),
            None,
            "the synchronous Windows setup artifact is private"
        );
        #[cfg(not(windows))]
        assert_eq!(
            subscription.try_recv(),
            Some(GitWatchEvent::RefMetadata),
            "non-Windows platforms never suppress the event"
        );

        let event = Event::new(EventKind::Modify(ModifyKind::Any)).add_path(refs_root.clone());
        backend.emit(0, Ok(event.clone()));
        assert_eq!(
            subscription.try_recv(),
            Some(GitWatchEvent::RefMetadata),
            "the identical event after backend commit remains observable"
        );

        let aliases = identity_for_paths(&worktree, &git_dir, &common_dir, host_path_platform());
        let roots = identity_for_paths(
            std::fs::canonicalize(&worktree).expect("canonical worktree"),
            std::fs::canonicalize(&git_dir).expect("canonical Git directory"),
            std::fs::canonicalize(&common_dir).expect("canonical common directory"),
            host_path_platform(),
        );
        let root_aliases = [roots.clone(), aliases];
        let registered = vec![
            (roots.worktree_root.clone(), RecursiveMode::Recursive),
            (roots.common_dir.clone(), RecursiveMode::NonRecursive),
        ];
        assert_eq!(
            classify_event(
                &event,
                &roots,
                &root_aliases,
                &registered,
                host_path_platform(),
            ),
            Some(GitWatchEvent::RefMetadata),
            "the classifier never permanently suppresses this metadata shape"
        );
        for event in [
            Event::new(EventKind::Create(notify::event::CreateKind::File))
                .add_path(refs_root.join("heads/new")),
            Event::new(EventKind::Remove(notify::event::RemoveKind::File))
                .add_path(refs_root.join("heads/old")),
            Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Content)))
                .add_path(refs_root.join("heads/main")),
            Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
                .add_path(refs_root.join("heads/main.lock"))
                .add_path(refs_root.join("heads/main")),
        ] {
            assert_eq!(
                classify_event(
                    &event,
                    &roots,
                    &root_aliases,
                    &registered,
                    host_path_platform(),
                ),
                Some(GitWatchEvent::RefMetadata),
                "child create, delete, content, and rename changes remain invalidating"
            );
        }
    }

    #[tokio::test]
    async fn registration_queue_overflow_always_publishes_ref_metadata() {
        for fail_rescan in [false, true] {
            let root = tempfile::tempdir().expect("overflow fixture");
            let worktree = root.path().join("worktree");
            let git_dir = root.path().join("common");
            for path in [&worktree, &git_dir.join("logs/other")] {
                fs::create_dir_all(path).unwrap();
            }
            let backend = FakeWatcherBackendFactory::default();
            let service = GitWatchService::with_backend_factory(Arc::new(backend.clone()));
            let mut subscription = service
                .subscribe(request(&worktree, &git_dir, &git_dir))
                .await
                .unwrap();
            if fail_rescan {
                let next_watch = backend.state.all_watches.lock().unwrap().len() + 1;
                backend
                    .state
                    .fail_watch_at
                    .store(next_watch, Ordering::Relaxed);
            }
            // Only store roots are queued (at most four per non-recursive metadata
            // root), so backend events cannot fill the bounded queue: fill it here.
            let work = {
                let mut state = service.inner.lock_state();
                let entry = state.entries.get_mut(&subscription.identity).unwrap();
                for index in 0..257 {
                    let path = git_dir.join(format!("logs/other/{index}"));
                    entry.registration_requests.add(
                        path.to_string_lossy().into_owned(),
                        PendingPath {
                            native_path: path,
                            change: DirectoryChange::Created,
                        },
                    );
                }
                entry.registration_event = Some(GitWatchEvent::Metadata);
                service
                    .inner
                    .take_registration_work(entry)
                    .expect("overflow registration")
            };
            service.inner.start_registration(
                subscription.identity.clone(),
                subscription.generation,
                work,
                &tokio::runtime::Handle::current(),
            );
            service.inner.setups.wait_until_idle().await;
            assert_eq!(subscription.try_recv(), Some(GitWatchEvent::RefMetadata));
            assert_eq!(
                subscription.health(),
                if fail_rescan {
                    GitWatcherHealth::FallbackRequired
                } else {
                    GitWatcherHealth::Healthy
                }
            );
            drop(subscription);
            service.shutdown().await;
        }
    }

    #[derive(Clone, Default)]
    struct FakeWatcherBackendFactory {
        state: Arc<FakeBackendState>,
    }

    #[derive(Default)]
    struct FakeBackendState {
        callbacks: Mutex<Vec<BackendCallback>>,
        configs: Mutex<Vec<Config>>,
        watches: Mutex<Vec<(usize, PathBuf, RecursiveMode)>>,
        all_watches: Mutex<Vec<(usize, PathBuf, RecursiveMode)>>,
        unwatches: Mutex<Vec<(usize, PathBuf)>>,
        all_unwatches: Mutex<Vec<(usize, PathBuf)>>,
        created: AtomicUsize,
        dropped: AtomicUsize,
        fail_watch_at: AtomicUsize,
        max_files_watch: AtomicBool,
        suppress_readiness_callback: AtomicBool,
        readiness_probe_writes: AtomicUsize,
        readiness_probe_written: Notify,
        emit_nested_root_modify_any_during_watch: AtomicBool,
        watch_barrier: Mutex<Option<Arc<SetupBarrier>>>,
    }

    #[derive(Default)]
    struct SetupBarrier {
        entered: std::sync::atomic::AtomicBool,
        entered_notify: tokio::sync::Notify,
        released: Mutex<bool>,
        release_notify: std::sync::Condvar,
    }

    struct ReleaseSetupBarrier(Arc<SetupBarrier>);

    impl Drop for ReleaseSetupBarrier {
        fn drop(&mut self) {
            self.0.release();
        }
    }

    impl SetupBarrier {
        fn block(&self) {
            self.entered.store(true, Ordering::Release);
            self.entered_notify.notify_waiters();
            let released = self.released.lock().expect("barrier release lock");
            let (released, _) = self
                .release_notify
                .wait_timeout_while(released, Duration::from_secs(10), |released| !*released)
                .expect("barrier release wait");
            assert!(*released, "timed out waiting for barrier release");
        }

        async fn wait_until_entered(&self) {
            loop {
                let notified = self.entered_notify.notified();
                if self.entered.load(Ordering::Acquire) {
                    return;
                }
                notified.await;
            }
        }

        fn release(&self) {
            *self.released.lock().expect("barrier release lock") = true;
            self.release_notify.notify_all();
        }
    }

    #[test]
    fn readiness_canonicalize_failure_reports_cleanup_failure_without_losing_the_root() {
        let removed = Mutex::new(Vec::new());
        let error = create_readiness_probe_with(
            &[],
            Path::new("temporary"),
            |_| Ok(()),
            |_| {
                Err(io::Error::new(
                    ErrorKind::PermissionDenied,
                    "canonicalize denied",
                ))
            },
            |root| {
                removed
                    .lock()
                    .expect("removed roots lock")
                    .push(root.to_path_buf());
                Err(io::Error::other("cleanup denied"))
            },
        );
        let error = match error {
            Err(error) => error,
            Ok(_) => panic!("canonicalization and cleanup both fail"),
        };

        let detail = error.to_string();
        assert!(detail.contains("canonicalize denied"), "{detail}");
        assert!(detail.contains("cleanup denied"), "{detail}");
        let removed = removed.into_inner().expect("removed roots lock");
        assert_eq!(removed.len(), 1);
        assert!(is_readiness_directory(&removed[0]));
    }

    #[test]
    fn readiness_overlap_reports_cleanup_failure_for_the_canonical_root() {
        let admitted = tempfile::tempdir().expect("admitted root");
        let admitted = fs::canonicalize(admitted.path()).expect("canonical admitted root");
        let canonical_probe = admitted.join("probe");
        let removed = Mutex::new(Vec::new());
        let error = create_readiness_probe_with(
            &[(admitted, RecursiveMode::Recursive)],
            Path::new("temporary"),
            |_| Ok(()),
            |_| Ok(canonical_probe.clone()),
            |root| {
                removed
                    .lock()
                    .expect("removed roots lock")
                    .push(root.to_path_buf());
                Err(io::Error::other("overlap cleanup denied"))
            },
        );
        let error = match error {
            Err(error) => error,
            Ok(_) => panic!("overlap and cleanup both fail"),
        };

        let detail = error.to_string();
        assert!(detail.contains("overlaps an admitted root"), "{detail}");
        assert!(detail.contains("overlap cleanup denied"), "{detail}");
        assert_eq!(
            removed.into_inner().expect("removed roots lock"),
            [canonical_probe]
        );
    }

    struct FakeWatcherBackend {
        id: usize,
        watch_calls: usize,
        state: Arc<FakeBackendState>,
    }

    impl FakeWatcherBackendFactory {
        fn failing_on_watch(call: usize) -> Self {
            let factory = Self::default();
            factory.state.fail_watch_at.store(call, Ordering::Relaxed);
            factory
        }

        fn blocked_during_watch() -> (Self, Arc<SetupBarrier>) {
            let factory = Self::default();
            let barrier = Arc::new(SetupBarrier::default());
            *factory
                .state
                .watch_barrier
                .lock()
                .expect("watch barrier lock") = Some(Arc::clone(&barrier));
            (factory, barrier)
        }

        /// Mimics Windows: installing a recursive watch beneath a watched parent
        /// reports a `Modify(Any)` of the newly watched directory.
        fn emitting_nested_root_modify_any_during_watch() -> Self {
            let factory = Self::default();
            factory
                .state
                .emit_nested_root_modify_any_during_watch
                .store(true, Ordering::Relaxed);
            factory
        }

        fn emit(&self, backend: usize, event: notify::Result<Event>) {
            let callback = self.state.callbacks.lock().expect("callbacks lock")[backend].clone();
            callback(event);
        }

        fn created(&self) -> usize {
            self.state.created.load(Ordering::Relaxed)
        }

        fn dropped(&self) -> usize {
            self.state.dropped.load(Ordering::Relaxed)
        }

        fn configs(&self) -> Vec<Config> {
            self.state.configs.lock().expect("configs lock").clone()
        }
    }

    impl GitWatcherBackendFactory for FakeWatcherBackendFactory {
        fn create(
            &self,
            callback: BackendCallback,
            config: Config,
        ) -> notify::Result<Box<dyn GitWatcherBackend>> {
            let id = self.state.created.fetch_add(1, Ordering::Relaxed);
            self.state
                .configs
                .lock()
                .expect("configs lock")
                .push(config);
            self.state
                .callbacks
                .lock()
                .expect("callbacks lock")
                .push(callback);
            Ok(Box::new(FakeWatcherBackend {
                id,
                watch_calls: 0,
                state: Arc::clone(&self.state),
            }))
        }
    }

    impl GitWatcherBackend for FakeWatcherBackend {
        fn watch(&mut self, path: &Path, recursive_mode: RecursiveMode) -> notify::Result<()> {
            self.watch_calls += 1;
            if let Some(barrier) = self
                .state
                .watch_barrier
                .lock()
                .expect("watch barrier lock")
                .clone()
            {
                barrier.block();
            }
            if self.state.fail_watch_at.load(Ordering::Relaxed) == self.watch_calls {
                if self.state.max_files_watch.load(Ordering::Relaxed) {
                    return Err(notify::Error::new(notify::ErrorKind::MaxFilesWatch));
                }
                return Err(notify::Error::generic("injected watch failure"));
            }
            self.state
                .all_watches
                .lock()
                .expect("all watches lock")
                .push((self.id, path.to_path_buf(), recursive_mode));
            if !is_readiness_directory(path) {
                self.state.watches.lock().expect("watches lock").push((
                    self.id,
                    path.to_path_buf(),
                    recursive_mode,
                ));
            }
            if self
                .state
                .emit_nested_root_modify_any_during_watch
                .load(Ordering::Relaxed)
                && recursive_mode == RecursiveMode::Recursive
                && self.watch_calls > 1
            {
                let callback =
                    self.state.callbacks.lock().expect("callbacks lock")[self.id].clone();
                callback(Ok(
                    Event::new(EventKind::Modify(ModifyKind::Any)).add_path(path.to_path_buf())
                ));
            }
            Ok(())
        }

        fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
            self.state
                .all_unwatches
                .lock()
                .expect("all unwatches lock")
                .push((self.id, path.to_path_buf()));
            if !is_readiness_directory(path) {
                self.state
                    .unwatches
                    .lock()
                    .expect("unwatches lock")
                    .push((self.id, path.to_path_buf()));
            }
            Ok(())
        }

        fn readiness_probe_written(&mut self, sentinel: &Path) {
            self.state
                .readiness_probe_writes
                .fetch_add(1, Ordering::SeqCst);
            self.state.readiness_probe_written.notify_waiters();
            if self
                .state
                .suppress_readiness_callback
                .load(Ordering::SeqCst)
            {
                return;
            }
            let callback = self.state.callbacks.lock().expect("callbacks lock")[self.id].clone();
            callback(Ok(
                Event::new(EventKind::Any).add_path(sentinel.to_path_buf())
            ));
        }
    }

    impl Drop for FakeWatcherBackend {
        fn drop(&mut self) {
            self.state.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn request(worktree: &Path, git_dir: &Path, common_dir: &Path) -> GitWatchRequest {
        GitWatchRequest {
            worktree_root: worktree.to_path_buf(),
            git_dir: git_dir.to_path_buf(),
            common_dir: common_dir.to_path_buf(),
        }
    }

    async fn expect_fake_event(
        subscription: &mut GitWatchSubscription,
        expected: GitWatchEvent,
        context: &str,
    ) {
        let actual = tokio::time::timeout(Duration::from_secs(1), subscription.recv())
            .await
            .unwrap_or_else(|_| panic!("fake watcher event deadline: {context}"))
            .unwrap_or_else(|| panic!("fake watcher closed: {context}"));
        assert_eq!(actual, expected, "{context}");
    }

    async fn expect_native_event(subscription: &mut GitWatchSubscription, expected: GitWatchEvent) {
        let actual = tokio::time::timeout(std::time::Duration::from_secs(5), subscription.recv())
            .await
            .expect("native watcher event deadline")
            .expect("native watcher remains open");
        assert_eq!(actual, expected);
    }

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    static NATIVE_READINESS_ID: AtomicUsize = AtomicUsize::new(0);

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    #[derive(Default)]
    struct NativeReadiness {
        observed: AtomicBool,
        notify: Notify,
    }

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    impl NativeReadiness {
        fn observe(&self) {
            self.observed.store(true, Ordering::Release);
            self.notify.notify_waiters();
        }

        async fn wait(&self) {
            loop {
                let notified = self.notify.notified();
                if self.observed.load(Ordering::Acquire) {
                    return;
                }
                notified.await;
            }
        }
    }

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    #[derive(Clone)]
    struct NativeReadinessRegistration {
        root: PathBuf,
        sentinel: PathBuf,
        sentinel_key: String,
        readiness: Arc<NativeReadiness>,
    }

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    #[derive(Default)]
    struct NativeReadinessState {
        registrations: Mutex<Vec<NativeReadinessRegistration>>,
    }

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    impl NativeReadinessState {
        fn registrations(&self) -> Vec<NativeReadinessRegistration> {
            self.registrations
                .lock()
                .expect("native readiness registrations lock")
                .iter()
                .filter(|registration| !is_readiness_directory(&registration.root))
                .cloned()
                .collect()
        }

        fn intercept(&self, event: &notify::Result<Event>) -> Option<Vec<Arc<NativeReadiness>>> {
            let event = event.as_ref().ok()?;
            if event.paths.is_empty() {
                return None;
            }
            let registrations = self
                .registrations
                .lock()
                .expect("native readiness registrations lock");
            event
                .paths
                .iter()
                .map(|path| {
                    let key = normalize_worktree_path_key(path, host_path_platform());
                    registrations
                        .iter()
                        .find(|registration| registration.sentinel_key == key)
                        .map(|registration| Arc::clone(&registration.readiness))
                })
                .collect()
        }
    }

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    struct NativeReadinessBackendFactory {
        id: usize,
        state: Arc<NativeReadinessState>,
    }

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    impl NativeReadinessBackendFactory {
        fn new(state: Arc<NativeReadinessState>) -> Self {
            Self {
                id: NATIVE_READINESS_ID.fetch_add(1, Ordering::Relaxed),
                state,
            }
        }
    }

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    impl GitWatcherBackendFactory for NativeReadinessBackendFactory {
        fn create(
            &self,
            callback: BackendCallback,
            config: Config,
        ) -> notify::Result<Box<dyn GitWatcherBackend>> {
            let state = Arc::clone(&self.state);
            let callback = Arc::new(move |event: notify::Result<Event>| {
                if let Some(readiness) = state.intercept(&event) {
                    for signal in readiness {
                        signal.observe();
                    }
                    return;
                }
                callback(event);
            });
            let backend = NativeWatcherBackendFactory.create(callback, config)?;
            Ok(Box::new(NativeReadinessBackend {
                id: self.id,
                backend,
                state: Arc::clone(&self.state),
            }))
        }
    }

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    struct NativeReadinessBackend {
        id: usize,
        backend: Box<dyn GitWatcherBackend>,
        state: Arc<NativeReadinessState>,
    }

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    impl GitWatcherBackend for NativeReadinessBackend {
        fn watch(&mut self, path: &Path, recursive_mode: RecursiveMode) -> notify::Result<()> {
            self.backend.watch(path, recursive_mode)?;
            let index = self
                .state
                .registrations
                .lock()
                .expect("native readiness registrations lock")
                .len();
            let sentinel = path.join(format!(".bibcode-watcher-ready-{}-{index}", self.id));
            self.state
                .registrations
                .lock()
                .expect("native readiness registrations lock")
                .push(NativeReadinessRegistration {
                    root: path.to_path_buf(),
                    sentinel_key: normalize_worktree_path_key(&sentinel, host_path_platform()),
                    sentinel,
                    readiness: Arc::new(NativeReadiness::default()),
                });
            Ok(())
        }

        fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
            self.backend.unwatch(path)
        }
    }

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    struct NativeFixture {
        subscription: GitWatchSubscription,
        _service: GitWatchService,
        worktree: PathBuf,
        git_dir: PathBuf,
        common_dir: PathBuf,
        upstream_dir: PathBuf,
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        rename_source: PathBuf,
        readiness: Arc<NativeReadinessState>,
        _root: tempfile::TempDir,
        _native_test_guard: NativeWatcherTestPermit<'static>,
    }

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    impl NativeFixture {
        async fn new(seed_worktree_file: Option<(&str, &[u8])>) -> Self {
            let native_test_guard = acquire_native_watcher_test_permit().await;
            let root = tempfile::tempdir().expect("watch fixture");
            let worktree = root.path().join("worktree");
            let git_dir = root.path().join("git-dir");
            let common_dir = root.path().join("common-dir");
            let upstream_dir = common_dir.join("refs/remotes/origin");
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            let rename_source = root.path().join("rename-source.txt");
            for path in [&worktree, &git_dir, &upstream_dir] {
                std::fs::create_dir_all(path).expect("watch root");
            }
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            std::fs::write(&rename_source, b"first").expect("rename source");
            if let Some((name, contents)) = seed_worktree_file {
                let path = worktree.join(name);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).expect("seed worktree parent");
                }
                std::fs::write(path, contents).expect("seed worktree file");
            }
            let readiness = Arc::new(NativeReadinessState::default());
            let service = GitWatchService::with_backend_factory(Arc::new(
                NativeReadinessBackendFactory::new(Arc::clone(&readiness)),
            ));
            let mut subscription = test_timeout(
                "native watch subscription setup",
                service.subscribe(request(&worktree, &git_dir, &common_dir)),
            )
            .await
            .expect("native watch subscription");
            assert_native_watching_available(&subscription);
            let registrations = readiness.registrations();
            for root in [&worktree, &git_dir, &common_dir, &common_dir.join("refs")] {
                let canonical = std::fs::canonicalize(root).expect("canonical watched directory");
                assert!(
                    registrations
                        .iter()
                        .any(|registration| registration.root == canonical),
                    "missing native directory watch: {}",
                    root.display()
                );
            }
            for registration in &registrations {
                assert_eq!(
                    registration.sentinel.parent(),
                    Some(registration.root.as_path())
                );
                std::fs::write(&registration.sentinel, b"ready")
                    .expect("native readiness sentinel");
            }
            for registration in &registrations {
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    registration.readiness.wait(),
                )
                .await
                .expect("native readiness deadline");
            }
            for registration in &registrations {
                std::fs::remove_file(&registration.sentinel)
                    .expect("remove native readiness sentinel");
            }
            assert_eq!(subscription.try_recv(), None);
            assert_eq!(subscription.health(), GitWatcherHealth::Healthy);
            Self {
                subscription,
                _service: service,
                worktree,
                git_dir,
                common_dir,
                upstream_dir,
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                rename_source,
                readiness,
                _root: root,
                _native_test_guard: native_test_guard,
            }
        }

        async fn expect(&mut self, expected: GitWatchEvent) {
            expect_native_event(&mut self.subscription, expected).await;
        }

        fn readiness_registration_count(&self) -> usize {
            self.readiness.registrations().len()
        }

        fn readiness_acknowledgement_count(&self) -> usize {
            self.readiness
                .registrations()
                .iter()
                .filter(|registration| registration.readiness.observed.load(Ordering::Acquire))
                .count()
        }
    }

    #[cfg(any(unix, target_os = "windows"))]
    fn assert_native_watching_available(subscription: &GitWatchSubscription) {
        assert_eq!(
            subscription.health(),
            GitWatcherHealth::Healthy,
            "native watching is unavailable: subscription requires fallback; native watcher creation, registration, or readiness failed (check inotify instance/watch limits on Linux)"
        );
    }

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn native_readiness_probes_every_registered_root_without_publication() {
        let mut fixture = NativeFixture::new(None).await;
        assert!(fixture.readiness_registration_count() >= 3);
        assert_eq!(
            fixture.readiness_acknowledgement_count(),
            fixture.readiness_registration_count()
        );
        assert_eq!(fixture.subscription.try_recv(), None);
        assert_eq!(fixture.subscription.health(), GitWatcherHealth::Healthy);
    }

    #[tokio::test]
    async fn canonical_aliases_share_one_backend_until_the_final_subscriber_leaves() {
        let root = tempfile::tempdir().expect("watch fixture");
        let worktree = root.path().join("worktree");
        let git_dir = worktree.join(".git");
        std::fs::create_dir_all(&git_dir).expect("Git directory");

        let backend = FakeWatcherBackendFactory::default();
        let service = GitWatchService::with_backend_factory(Arc::new(backend.clone()));
        let first = service
            .subscribe(request(&worktree, &git_dir, &git_dir))
            .await
            .expect("first watch");
        let alias = worktree.join("..").join("worktree");
        let second = service
            .subscribe(request(&alias, &alias.join(".git"), &alias.join(".git")))
            .await
            .expect("alias watch");

        assert_eq!(service.active_count_for_test(), 1);
        assert_eq!(backend.created(), 1);
        assert_eq!(
            backend.state.watches.lock().expect("watches lock").len(),
            1,
            "aliases share one native recursive worktree watch"
        );
        drop(first);
        assert_eq!(service.active_count_for_test(), 1);
        assert_eq!(backend.dropped(), 0);
        drop(second);
        assert_eq!(service.active_count_for_test(), 0);
        assert_eq!(backend.dropped(), 1);
    }

    #[tokio::test]
    async fn readiness_root_is_registered_last_outside_user_roots_then_removed_without_publication()
    {
        let root = tempfile::tempdir().expect("watch fixture");
        let worktree = root.path().join("worktree");
        let git_dir = worktree.join(".git");
        std::fs::create_dir_all(&git_dir).expect("Git directory");
        let canonical_worktree = std::fs::canonicalize(&worktree).expect("canonical worktree");
        let canonical_git_dir = std::fs::canonicalize(&git_dir).expect("canonical Git directory");
        let backend = FakeWatcherBackendFactory::default();
        let service = GitWatchService::with_backend_factory(Arc::new(backend.clone()));

        let mut subscription = service
            .subscribe(request(&worktree, &git_dir, &git_dir))
            .await
            .expect("watch subscription");

        let watches = backend
            .state
            .all_watches
            .lock()
            .expect("all watches lock")
            .clone();
        assert_eq!(watches.len(), 2);
        assert_eq!(
            watches[0],
            (0, canonical_worktree.clone(), RecursiveMode::Recursive)
        );
        let readiness_root = &watches[1].1;
        assert_eq!(watches[1].2, RecursiveMode::NonRecursive);
        assert!(is_readiness_directory(readiness_root));
        assert!(!readiness_root.starts_with(&canonical_worktree));
        assert!(!readiness_root.starts_with(&canonical_git_dir));
        assert_eq!(
            backend.state.readiness_probe_writes.load(Ordering::SeqCst),
            1
        );
        assert_eq!(
            backend
                .state
                .all_unwatches
                .lock()
                .expect("all unwatches lock")
                .as_slice(),
            &[(0, readiness_root.clone())]
        );
        assert!(!readiness_root.exists());
        assert_eq!(subscription.try_recv(), None);
        assert_eq!(subscription.health(), GitWatcherHealth::Healthy);
    }

    #[tokio::test]
    async fn readiness_timeout_cleans_temp_watch_and_returns_sticky_fallback_subscription() {
        let root = tempfile::tempdir().expect("watch fixture");
        let worktree = root.path().join("worktree");
        let git_dir = worktree.join(".git");
        std::fs::create_dir_all(&git_dir).expect("Git directory");
        let backend = FakeWatcherBackendFactory::default();
        backend
            .state
            .suppress_readiness_callback
            .store(true, Ordering::SeqCst);
        let service = GitWatchService::with_backend_factory_and_readiness_timeout(
            Arc::new(backend.clone()),
            Duration::from_millis(50),
        );

        let mut subscription = tokio::time::timeout(
            Duration::from_secs(5),
            service.subscribe(request(&worktree, &git_dir, &git_dir)),
        )
        .await
        .expect("readiness timeout remains bounded")
        .expect("readiness timeout degrades instead of failing subscription");

        assert_eq!(subscription.health(), GitWatcherHealth::FallbackRequired);
        assert_eq!(subscription.recv().await, Some(GitWatchEvent::Unavailable));
        let readiness_root = backend
            .state
            .all_watches
            .lock()
            .expect("all watches lock")
            .last()
            .expect("readiness root was registered")
            .1
            .clone();
        assert!(is_readiness_directory(&readiness_root));
        assert!(
            backend
                .state
                .all_unwatches
                .lock()
                .expect("all unwatches lock")
                .iter()
                .any(|(_, path)| path == &readiness_root)
        );
        assert!(!readiness_root.exists());
    }

    #[tokio::test]
    async fn backend_configuration_disables_recursive_symlink_following() {
        let root = tempfile::tempdir().expect("watch fixture");
        let worktree = root.path().join("worktree");
        let git_dir = worktree.join(".git");
        std::fs::create_dir_all(&git_dir).expect("Git directory");
        let backend = FakeWatcherBackendFactory::default();
        let service = GitWatchService::with_backend_factory(Arc::new(backend.clone()));

        let _subscription = service
            .subscribe(request(&worktree, &git_dir, &git_dir))
            .await
            .expect("watch subscription");

        assert_eq!(backend.configs().len(), 1);
        assert!(!backend.configs()[0].follow_symlinks());
    }

    #[tokio::test]
    async fn registered_worktree_root_removal_requires_fallback() {
        let root = tempfile::tempdir().expect("watch fixture");
        let worktree = root.path().join("worktree");
        let git_dir = worktree.join(".git");
        std::fs::create_dir_all(&git_dir).expect("Git directory");
        let backend = FakeWatcherBackendFactory::default();
        let service = GitWatchService::with_backend_factory(Arc::new(backend.clone()));
        let mut subscription = service
            .subscribe(request(&worktree, &git_dir, &git_dir))
            .await
            .expect("watch subscription");
        let registered_worktree = backend.state.watches.lock().expect("watches lock")[0]
            .1
            .clone();

        backend.emit(
            0,
            Ok(
                Event::new(EventKind::Remove(notify::event::RemoveKind::Folder))
                    .add_path(registered_worktree),
            ),
        );

        assert_eq!(subscription.recv().await, Some(GitWatchEvent::Unavailable));
        assert_eq!(subscription.health(), GitWatcherHealth::FallbackRequired);
    }

    #[tokio::test]
    async fn admitted_root_loss_requires_fallback_but_child_removal_does_not() {
        let root = tempfile::tempdir().expect("watch fixture");
        let worktree = root.path().join("worktree");
        let git_dir = root.path().join("main.git/worktrees/topic");
        let common_dir = root.path().join("main.git");
        std::fs::create_dir_all(&worktree).expect("worktree");
        std::fs::create_dir_all(&git_dir).expect("Git directory");
        std::fs::create_dir_all(common_dir.join("refs")).expect("refs directory");
        let backend = FakeWatcherBackendFactory::default();
        let service = GitWatchService::with_backend_factory(Arc::new(backend.clone()));
        let mut subscription = service
            .subscribe(request(&worktree, &git_dir, &common_dir))
            .await
            .expect("linked watch subscription");
        for (index, root) in [&worktree, &git_dir, &common_dir].into_iter().enumerate() {
            let registered_root = std::fs::canonicalize(root).expect("canonical admitted root");
            let event = if index % 2 == 0 {
                Event::new(EventKind::Remove(notify::event::RemoveKind::Folder))
                    .add_path(registered_root.clone())
            } else {
                Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
                    .add_path(registered_root.clone())
                    .add_path(registered_root.with_extension("retired"))
            };
            backend.emit(0, Ok(event));
            expect_fake_event(
                &mut subscription,
                GitWatchEvent::Unavailable,
                &format!("registered root {index} loss must require fallback"),
            )
            .await;
        }

        backend.emit(
            0,
            Ok(
                Event::new(EventKind::Remove(notify::event::RemoveKind::File))
                    .add_path(worktree.join("ordinary-child.txt")),
            ),
        );
        expect_fake_event(
            &mut subscription,
            GitWatchEvent::WorkingTree,
            "ordinary child removal remains a working-tree signal",
        )
        .await;
        assert_eq!(subscription.health(), GitWatcherHealth::FallbackRequired);
    }

    #[tokio::test]
    async fn recursive_metadata_coverage_observes_new_stores_and_keeps_child_events() {
        let root = tempfile::tempdir().expect("watch fixture");
        let worktree = root.path().join("worktree");
        let git_dir = root.path().join("main.git/worktrees/topic");
        let common_dir = root.path().join("main.git");
        for path in [&worktree, &git_dir] {
            std::fs::create_dir_all(path).expect("root");
        }
        let backend = FakeWatcherBackendFactory::default();
        let service = GitWatchService::with_backend_factory(Arc::new(backend.clone()));
        let mut subscription = service
            .subscribe(request(&worktree, &git_dir, &common_dir))
            .await
            .expect("watch subscription");
        let canonical_common = std::fs::canonicalize(&common_dir).expect("common root");
        assert!(
            backend
                .state
                .watches
                .lock()
                .expect("watches")
                .iter()
                .any(|(_, path, mode)| path == &canonical_common
                    && *mode == RecursiveMode::NonRecursive)
        );
        for relative in [
            "logs/refs/stash",
            "worktrees/sibling/HEAD",
            "reftable/tables.list",
            "refs/heads/new",
        ] {
            let parent = common_dir
                .join(relative)
                .parent()
                .expect("metadata parent")
                .to_path_buf();
            let store = common_dir.join(relative.split('/').next().expect("store component"));
            // A new store is reported by the non-recursive common root as its own
            // entry; directories inside an existing store by that store's watch.
            let created = if store.exists() {
                parent.clone()
            } else {
                store
            };
            std::fs::create_dir_all(&parent).expect("late metadata parent");
            backend.emit(
                0,
                Ok(
                    Event::new(EventKind::Create(notify::event::CreateKind::Folder))
                        .add_path(created),
                ),
            );
            service.inner.setups.wait_until_idle().await;
            let canonical_parent = std::fs::canonicalize(&parent).expect("canonical parent");
            assert!(
                backend
                    .state
                    .watches
                    .lock()
                    .expect("watches")
                    .iter()
                    .any(|(_, path, mode)| path == &canonical_parent
                        || (*mode == RecursiveMode::Recursive
                            && canonical_parent.starts_with(path)))
            );
            backend.emit(
                0,
                Ok(Event::new(EventKind::Modify(ModifyKind::Any))
                    .add_path(common_dir.join(relative))),
            );
            assert_eq!(
                subscription.recv().await,
                Some(GitWatchEvent::RefMetadata),
                "{relative}"
            );
        }
        backend.emit(
            0,
            Ok(Event::new(EventKind::Any).add_path(common_dir.join("objects/ab/object"))),
        );
        assert_eq!(subscription.try_recv(), None);
    }

    fn linked_fixture(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let worktree = root.join("worktree");
        let common_dir = root.join("main.git");
        let git_dir = common_dir.join("worktrees/topic");
        for path in [
            &worktree,
            &git_dir,
            &common_dir.join("worktrees/sibling"),
            &common_dir.join("refs/remotes/origin"),
            &common_dir.join("logs/refs/remotes/origin"),
            &common_dir.join("reftable"),
        ] {
            std::fs::create_dir_all(path).expect("linked fixture directory");
        }
        (worktree, git_dir, common_dir)
    }

    #[tokio::test]
    async fn nested_ref_directories_rely_on_native_recursion_without_a_full_rescan() {
        let root = tempfile::tempdir().expect("nested ref fixture");
        let (worktree, git_dir, common_dir) = linked_fixture(root.path());
        let backend = FakeWatcherBackendFactory::default();
        let service = GitWatchService::with_backend_factory(Arc::new(backend.clone()));
        let mut subscription = service
            .subscribe(request(&worktree, &git_dir, &common_dir))
            .await
            .expect("linked watch subscription");
        let installed = backend.state.watches.lock().expect("watches").clone();

        // A large fetch creates many remote-tracking namespaces and their reflogs,
        // more than the registration queue holds.
        for index in 0..300 {
            for store in ["refs", "logs/refs"] {
                let directory = common_dir.join(format!("{store}/remotes/origin/topic-{index}"));
                std::fs::create_dir(&directory).expect("nested ref directory");
                backend.emit(
                    0,
                    Ok(
                        Event::new(EventKind::Create(notify::event::CreateKind::Folder))
                            .add_path(directory),
                    ),
                );
                assert_eq!(
                    service.setup_count_for_test(),
                    0,
                    "native recursion already covers {store}/remotes/origin/topic-{index}"
                );
            }
        }
        service.inner.setups.wait_until_idle().await;

        assert!(
            backend
                .state
                .unwatches
                .lock()
                .expect("unwatches")
                .is_empty(),
            "nested ref directories must not force a full rescan"
        );
        assert_eq!(
            *backend.state.watches.lock().expect("watches"),
            installed,
            "no watch is added for directories inside a recursive store"
        );
        assert_eq!(subscription.try_recv(), Some(GitWatchEvent::RefMetadata));
        assert_eq!(subscription.health(), GitWatcherHealth::Healthy);
    }

    #[tokio::test]
    async fn ref_lock_renames_invalidate_refs_without_registration_work() {
        let root = tempfile::tempdir().expect("lock rename fixture");
        let (worktree, git_dir, common_dir) = linked_fixture(root.path());
        let backend = FakeWatcherBackendFactory::default();
        let service = GitWatchService::with_backend_factory(Arc::new(backend.clone()));
        let mut subscription = service
            .subscribe(request(&worktree, &git_dir, &common_dir))
            .await
            .expect("linked watch subscription");

        for (lock, target) in [
            ("refs/heads/main.lock", "refs/heads/main"),
            ("refs/remotes/origin/main.lock", "refs/remotes/origin/main"),
            ("reftable/tables.list.lock", "reftable/tables.list"),
            ("worktrees/sibling/HEAD.lock", "worktrees/sibling/HEAD"),
            ("worktrees/topic/HEAD.lock", "worktrees/topic/HEAD"),
            ("packed-refs.lock", "packed-refs"),
        ] {
            let (lock, target) = (common_dir.join(lock), common_dir.join(target));
            for event in [
                Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::From)))
                    .add_path(lock.clone()),
                Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::To)))
                    .add_path(target.clone()),
                Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
                    .add_path(lock.clone())
                    .add_path(target.clone()),
            ] {
                backend.emit(0, Ok(event));
                assert_eq!(
                    service.setup_count_for_test(),
                    0,
                    "a file rename inside {} needs no registration",
                    target.display()
                );
            }
            expect_fake_event(
                &mut subscription,
                GitWatchEvent::RefMetadata,
                &target.display().to_string(),
            )
            .await;
        }
        assert_eq!(subscription.health(), GitWatcherHealth::Healthy);
    }

    #[tokio::test]
    async fn removed_and_recreated_refs_remain_observed_under_the_common_root() {
        let root = tempfile::tempdir().expect("watch fixture");
        let worktree = root.path().join("worktree");
        let git_dir = root.path().join("main.git/worktrees/topic");
        let common_dir = root.path().join("main.git");
        let refs_dir = common_dir.join("refs");
        std::fs::create_dir_all(&worktree).expect("worktree");
        std::fs::create_dir_all(&git_dir).expect("Git directory");
        std::fs::create_dir_all(&refs_dir).expect("refs directory");
        let backend = FakeWatcherBackendFactory::default();
        let service = GitWatchService::with_backend_factory(Arc::new(backend.clone()));
        let mut first = service
            .subscribe(request(&worktree, &git_dir, &common_dir))
            .await
            .expect("first watch generation");
        let registered_refs = std::fs::canonicalize(&refs_dir).expect("refs path");

        std::fs::remove_dir_all(&refs_dir).expect("remove refs root");
        backend.emit(
            0,
            Ok(
                Event::new(EventKind::Remove(notify::event::RemoveKind::Folder))
                    .add_path(registered_refs.clone()),
            ),
        );
        assert_eq!(first.recv().await, Some(GitWatchEvent::RefMetadata));
        std::fs::create_dir_all(&refs_dir).expect("recreate refs root");
        backend.emit(
            0,
            Ok(
                Event::new(EventKind::Create(notify::event::CreateKind::Folder))
                    .add_path(registered_refs.clone()),
            ),
        );
        assert_eq!(first.recv().await, Some(GitWatchEvent::RefMetadata));
        assert_eq!(first.health(), GitWatcherHealth::Healthy);
        drop(first);

        let mut replacement = service
            .subscribe(request(&worktree, &git_dir, &common_dir))
            .await
            .expect("replacement generation");
        assert_eq!(replacement.health(), GitWatcherHealth::Healthy);
        backend.emit(
            0,
            Ok(
                Event::new(EventKind::Remove(notify::event::RemoveKind::Folder))
                    .add_path(registered_refs),
            ),
        );
        assert_eq!(replacement.try_recv(), None);
        assert_eq!(replacement.health(), GitWatcherHealth::Healthy);
        backend.emit(
            1,
            Ok(
                Event::new(EventKind::Remove(notify::event::RemoveKind::File))
                    .add_path(refs_dir.join("heads/main")),
            ),
        );
        assert_eq!(replacement.recv().await, Some(GitWatchEvent::RefMetadata));
        assert_eq!(replacement.health(), GitWatcherHealth::Healthy);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn late_registration_is_retired_before_a_replacement_backend_is_admitted() {
        let root = tempfile::tempdir().expect("registration lifetime fixture");
        let worktree = root.path().join("worktree");
        let git_dir = root.path().join("common");
        std::fs::create_dir_all(&worktree).expect("worktree");
        std::fs::create_dir_all(&git_dir).expect("Git root");
        let backend = FakeWatcherBackendFactory::default();
        let service = GitWatchService::with_backend_factory(Arc::new(backend.clone()));
        let first = service
            .subscribe(request(&worktree, &git_dir, &git_dir))
            .await
            .expect("first watch");
        let barrier = Arc::new(SetupBarrier::default());
        let _release = ReleaseSetupBarrier(barrier.clone());
        *backend.state.watch_barrier.lock().expect("barrier lock") = Some(barrier.clone());
        let late = git_dir.join("reftable");
        std::fs::create_dir(&late).expect("late directory");
        backend.emit(
            0,
            Ok(Event::new(EventKind::Create(notify::event::CreateKind::Folder)).add_path(late)),
        );
        tokio::time::timeout(Duration::from_secs(5), barrier.wait_until_entered())
            .await
            .expect("late registration enters the backend");
        drop(first);
        assert_eq!(
            backend.dropped(),
            0,
            "registration still owns the retired backend"
        );
        let next = {
            let service = service.clone();
            let request = request(&worktree, &git_dir, &git_dir);
            tokio::spawn(async move { service.subscribe(request).await })
        };
        tokio::task::yield_now().await;
        assert!(!next.is_finished());
        assert_eq!(backend.created(), 1);
        barrier.release();
        let next = next
            .await
            .expect("replacement task")
            .expect("replacement watch");
        assert_eq!(backend.dropped(), 1);
        assert_eq!(backend.created(), 2);
        drop(next);
        service.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_waits_for_late_directory_registration() {
        let root = tempfile::tempdir().expect("registration shutdown fixture");
        let worktree = root.path().join("worktree");
        let git_dir = root.path().join("common");
        std::fs::create_dir_all(&worktree).expect("worktree");
        std::fs::create_dir_all(&git_dir).expect("Git root");
        let backend = FakeWatcherBackendFactory::default();
        let service = GitWatchService::with_backend_factory(Arc::new(backend.clone()));
        let _subscription = service
            .subscribe(request(&worktree, &git_dir, &git_dir))
            .await
            .expect("watch");
        let barrier = Arc::new(SetupBarrier::default());
        let _release = ReleaseSetupBarrier(barrier.clone());
        *backend.state.watch_barrier.lock().expect("barrier lock") = Some(barrier.clone());
        let late = git_dir.join("reftable");
        std::fs::create_dir(&late).expect("late directory");
        backend.emit(
            0,
            Ok(Event::new(EventKind::Create(notify::event::CreateKind::Folder)).add_path(late)),
        );
        tokio::time::timeout(Duration::from_secs(5), barrier.wait_until_entered())
            .await
            .expect("late registration enters the backend");
        let shutdown = {
            let service = service.clone();
            tokio::spawn(async move { service.shutdown().await })
        };
        service.wait_for_setup_cancellation_for_test().await;
        assert!(
            !shutdown.is_finished(),
            "shutdown must await the owned registration"
        );
        barrier.release();
        shutdown.await.expect("shutdown task");
        assert_eq!(backend.dropped(), 1);
    }

    #[tokio::test]
    async fn shutdown_waits_for_canceled_blocking_setup_to_drop_its_backend() {
        let root = tempfile::tempdir().expect("watch fixture");
        let worktree = root.path().join("worktree");
        let git_dir = worktree.join(".git");
        std::fs::create_dir_all(&git_dir).expect("Git directory");
        let (backend, barrier) = FakeWatcherBackendFactory::blocked_during_watch();
        let service = GitWatchService::with_backend_factory(Arc::new(backend.clone()));
        let subscribing_service = service.clone();
        let subscribe = tokio::spawn(async move {
            subscribing_service
                .subscribe(request(&worktree, &git_dir, &git_dir))
                .await
        });

        barrier.wait_until_entered().await;
        assert_eq!(service.setup_count_for_test(), 1);
        subscribe.abort();
        let join_error = match subscribe.await {
            Err(error) => error,
            Ok(_) => panic!("subscribe task was not canceled"),
        };
        assert!(join_error.is_cancelled());

        let mut shutdown = Box::pin(service.shutdown());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut shutdown)
                .await
                .is_err(),
            "shutdown must retain ownership of a canceled subscribe's blocking setup"
        );
        assert_eq!(backend.dropped(), 0);

        barrier.release();
        tokio::time::timeout(std::time::Duration::from_secs(5), shutdown)
            .await
            .expect("shutdown waits only until setup releases");
        assert_eq!(backend.dropped(), 1);
        assert_eq!(service.setup_count_for_test(), 0);
    }

    #[tokio::test]
    async fn canonical_alias_subscribe_waits_for_the_shared_blocking_setup() {
        let root = tempfile::tempdir().expect("watch fixture");
        let worktree = root.path().join("worktree");
        let git_dir = worktree.join(".git");
        std::fs::create_dir_all(&git_dir).expect("Git directory");
        let (backend, barrier) = FakeWatcherBackendFactory::blocked_during_watch();
        let service = GitWatchService::with_backend_factory(Arc::new(backend.clone()));
        let first_service = service.clone();
        let first_worktree = worktree.clone();
        let first_git_dir = git_dir.clone();
        let first = tokio::spawn(async move {
            first_service
                .subscribe(request(&first_worktree, &first_git_dir, &first_git_dir))
                .await
        });
        barrier.wait_until_entered().await;

        let alias = worktree.join("..").join("worktree");
        let alias_service = service.clone();
        let second = tokio::spawn(async move {
            alias_service
                .subscribe(request(&alias, &alias.join(".git"), &alias.join(".git")))
                .await
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let completed_before_setup = second.is_finished();
        barrier.release();
        assert!(
            !completed_before_setup,
            "an alias must not report a healthy subscription before shared setup completes"
        );
        let first = first
            .await
            .expect("first join")
            .expect("first subscription");
        let second = second
            .await
            .expect("second join")
            .expect("second subscription");
        assert_eq!(backend.created(), 1);
        drop(first);
        drop(second);
    }

    #[tokio::test]
    async fn retains_only_the_latest_pending_signal_without_losing_later_identical_events() {
        let root = tempfile::tempdir().expect("watch fixture");
        let worktree = root.path().join("worktree");
        let git_dir = root.path().join("git-dir");
        let common_dir = root.path().join("common-dir");
        for path in [&worktree, &git_dir, &common_dir] {
            std::fs::create_dir_all(path).expect("watch root");
        }
        let backend = FakeWatcherBackendFactory::default();
        let service = GitWatchService::with_backend_factory(Arc::new(backend.clone()));
        let mut subscription = service
            .subscribe(request(&worktree, &git_dir, &common_dir))
            .await
            .expect("watch subscription");

        backend.emit(
            0,
            Ok(
                Event::new(EventKind::Create(notify::event::CreateKind::File))
                    .add_path(worktree.join("tracked.txt")),
            ),
        );
        backend.emit(
            0,
            Ok(
                Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Content)))
                    .add_path(git_dir.join("HEAD")),
            ),
        );
        backend.emit(
            0,
            Ok(Event::new(EventKind::Any).add_path(git_dir.join("index"))),
        );
        assert_eq!(subscription.recv().await, Some(GitWatchEvent::RefMetadata));
        assert_eq!(subscription.try_recv(), None);

        backend.emit(
            0,
            Ok(
                Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Content)))
                    .add_path(git_dir.join("HEAD")),
            ),
        );
        assert_eq!(subscription.recv().await, Some(GitWatchEvent::RefMetadata));
    }

    #[test]
    fn rescan_gate_doubles_its_spacing_and_resets_after_quiet() {
        let base = Duration::from_secs(10);
        let start = tokio::time::Instant::now();
        let at = |secs: u64| start + Duration::from_secs(secs);
        let mut gate = RescanGate::default();
        assert!(gate.admit(at(0), base));
        assert!(!gate.admit(at(5), base), "inside the first 10 s");
        assert!(gate.deferred);
        assert!(gate.admit(at(10), base));
        assert!(!gate.admit(at(29), base), "the spacing doubled to 20 s");
        assert!(gate.admit(at(30), base));
        assert_eq!(gate.delay, Duration::from_secs(40));
        // A quiet stretch past the deadline resets the spacing.
        let quiet = 70 + RESCAN_BACKOFF_MAX.as_secs();
        assert!(gate.admit(at(quiet), base));
        assert_eq!(gate.delay, base);
        assert!(!gate.deferred);
    }

    /// On Linux a full re-registration's own walk can overflow the kernel queue
    /// again; a following overflow must not start another full walk at once.
    #[tokio::test]
    async fn overflows_space_out_full_reregistrations() {
        let root = tempfile::tempdir().expect("watch fixture");
        let worktree = root.path().join("worktree");
        let git_dir = worktree.join(".git");
        std::fs::create_dir_all(&git_dir).expect("Git directory");
        let worktree = std::fs::canonicalize(&worktree).expect("canonical worktree");
        let backend = FakeWatcherBackendFactory::default();
        let service = GitWatchService::with_backend_factory_and_rescan_backoff(
            Arc::new(backend.clone()),
            Duration::from_millis(400),
        );
        let _subscription = service
            .subscribe(request(
                &worktree,
                &worktree.join(".git"),
                &worktree.join(".git"),
            ))
            .await
            .expect("watch subscription");
        let installs = || {
            backend
                .state
                .all_watches
                .lock()
                .expect("watches lock")
                .iter()
                .filter(|(_, path, mode)| path == &worktree && *mode == RecursiveMode::Recursive)
                .count()
        };
        service.inner.setups.wait_until_idle().await;
        assert_eq!(installs(), 1);

        backend.emit(0, Ok(Event::new(EventKind::Other).set_flag(Flag::Rescan)));
        service.inner.setups.wait_until_idle().await;
        assert_eq!(installs(), 2, "the first overflow re-registers at once");

        // The overflow its own walk caused arrives straight after.
        backend.emit(0, Ok(Event::new(EventKind::Other).set_flag(Flag::Rescan)));
        backend.emit(0, Ok(Event::new(EventKind::Other).set_flag(Flag::Rescan)));
        service.inner.setups.wait_until_idle().await;
        assert_eq!(installs(), 2, "overflows inside the spacing are deferred");

        tokio::time::sleep(Duration::from_millis(600)).await;
        service.inner.setups.wait_until_idle().await;
        assert_eq!(installs(), 3, "the deferred re-registration runs once");
    }

    #[tokio::test]
    async fn fallback_health_is_sticky_for_late_subscribers_until_a_new_generation() {
        let root = tempfile::tempdir().expect("watch fixture");
        let worktree = root.path().join("worktree");
        let git_dir = worktree.join(".git");
        std::fs::create_dir_all(&git_dir).expect("Git directory");
        let backend = FakeWatcherBackendFactory::default();
        let service = GitWatchService::with_backend_factory(Arc::new(backend.clone()));
        let mut first = service
            .subscribe(request(&worktree, &git_dir, &git_dir))
            .await
            .expect("first subscriber");

        backend.emit(0, Ok(Event::new(EventKind::Other).set_flag(Flag::Rescan)));
        // The overflow's background rescan publishes a ref invalidation once it
        // reinstalls the watches. A pending ref invalidation has priority in the
        // latest-value channel, so it can supersede the Overflow event; fallback
        // health is what records the overflow. Let the rescan settle, then consume
        // its invalidation so the ordinary event below is the next one observed.
        service.inner.setups.wait_until_idle().await;
        assert_eq!(first.health(), GitWatcherHealth::FallbackRequired);
        assert_eq!(
            first.try_recv(),
            Some(GitWatchEvent::RefMetadata),
            "the overflow rescan invalidates refs after reinstalling watches"
        );
        backend.emit(
            0,
            Ok(
                Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Content)))
                    .add_path(worktree.join("tracked.txt")),
            ),
        );
        expect_fake_event(
            &mut first,
            GitWatchEvent::WorkingTree,
            "ordinary events continue after sticky fallback health",
        )
        .await;
        assert_eq!(first.health(), GitWatcherHealth::FallbackRequired);

        let late = service
            .subscribe(request(&worktree, &git_dir, &git_dir))
            .await
            .expect("late subscriber");
        assert_eq!(late.health(), GitWatcherHealth::FallbackRequired);
        drop(first);
        drop(late);

        let mut replacement = service
            .subscribe(request(&worktree, &git_dir, &git_dir))
            .await
            .expect("replacement generation");
        assert_eq!(replacement.health(), GitWatcherHealth::Healthy);
        backend.emit(0, Ok(Event::new(EventKind::Other).set_flag(Flag::Rescan)));
        assert_eq!(replacement.try_recv(), None);
        assert_eq!(replacement.health(), GitWatcherHealth::Healthy);

        backend.emit(
            1,
            Ok(
                Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Content)))
                    .add_path(worktree.join("tracked.txt")),
            ),
        );
        assert_eq!(replacement.recv().await, Some(GitWatchEvent::WorkingTree));
        assert_eq!(replacement.health(), GitWatcherHealth::Healthy);
    }

    #[tokio::test]
    async fn overflow_and_backend_errors_require_fallback_without_clean_publication() {
        let root = tempfile::tempdir().expect("watch fixture");
        let worktree = root.path().join("worktree");
        let git_dir = root.path().join("git-dir");
        for path in [&worktree, &git_dir] {
            std::fs::create_dir_all(path).expect("watch root");
        }
        let backend = FakeWatcherBackendFactory::default();
        let service = GitWatchService::with_backend_factory(Arc::new(backend.clone()));
        let mut subscription = service
            .subscribe(request(&worktree, &git_dir, &git_dir))
            .await
            .expect("watch subscription");

        backend.emit(0, Ok(Event::new(EventKind::Other).set_flag(Flag::Rescan)));
        // The rescan's ref invalidation can supersede the Overflow event in the
        // latest-value channel; fallback health records the overflow.
        service.inner.setups.wait_until_idle().await;
        assert_eq!(subscription.health(), GitWatcherHealth::FallbackRequired);
        assert_eq!(
            subscription.try_recv(),
            Some(GitWatchEvent::RefMetadata),
            "native overflow rescan invalidates refs after reinstalling watches"
        );

        backend.emit(0, Err(notify::Error::generic("backend interrupted")));
        assert_eq!(subscription.recv().await, Some(GitWatchEvent::Unavailable));
        assert_eq!(subscription.health(), GitWatcherHealth::FallbackRequired);

        backend.emit(0, Ok(Event::new(EventKind::Other)));
        assert_eq!(
            subscription.recv().await,
            Some(GitWatchEvent::Unavailable),
            "a pathless interruption cannot be classified as a clean no-op"
        );
    }

    #[tokio::test]
    async fn exhausted_backend_creation_returns_fallback_promptly() {
        let root = tempfile::tempdir().expect("watch fixture");
        let git_dir = root.path().join(".git");
        fs::create_dir(&git_dir).expect("Git directory");
        let service = GitWatchService::with_exhausted_backend_for_test();

        let mut subscription = tokio::time::timeout(
            Duration::from_secs(2),
            service.subscribe(request(root.path(), &git_dir, &git_dir)),
        )
        .await
        .expect("EMFILE must return before the native readiness deadline")
        .expect("EMFILE must preserve the subscription");

        assert_eq!(subscription.health(), GitWatcherHealth::FallbackRequired);
        expect_fake_event(
            &mut subscription,
            GitWatchEvent::Unavailable,
            "EMFILE fallback",
        )
        .await;
        tokio::time::timeout(Duration::from_secs(2), service.shutdown())
            .await
            .expect("EMFILE fallback shuts down promptly");
    }

    #[tokio::test]
    async fn exhausted_watch_registration_returns_fallback_promptly() {
        let root = tempfile::tempdir().expect("watch fixture");
        let worktree = root.path().join("worktree");
        let git_dir = root.path().join("git-dir");
        for path in [&worktree, &git_dir] {
            fs::create_dir(path).expect("watch root");
        }
        let backend = FakeWatcherBackendFactory::failing_on_watch(2);
        backend.state.max_files_watch.store(true, Ordering::Relaxed);
        let service = GitWatchService::with_backend_factory(Arc::new(backend.clone()));

        let mut subscription = tokio::time::timeout(
            Duration::from_secs(2),
            service.subscribe(request(&worktree, &git_dir, &git_dir)),
        )
        .await
        .expect("MaxFilesWatch must return before the native readiness deadline")
        .expect("MaxFilesWatch must preserve the subscription");

        assert_eq!(subscription.health(), GitWatcherHealth::FallbackRequired);
        expect_fake_event(
            &mut subscription,
            GitWatchEvent::Unavailable,
            "watch-limit fallback",
        )
        .await;
        assert_eq!(
            backend
                .state
                .unwatches
                .lock()
                .expect("unwatches lock")
                .len(),
            1
        );
        assert_eq!(
            backend.dropped(),
            1,
            "partial registration releases the backend"
        );
        tokio::time::timeout(Duration::from_secs(2), service.shutdown())
            .await
            .expect("watch-limit fallback shuts down promptly");
    }

    #[tokio::test]
    async fn backend_creation_failure_publishes_unavailable() {
        struct UnavailableBackendFactory;

        impl GitWatcherBackendFactory for UnavailableBackendFactory {
            fn create(
                &self,
                _callback: BackendCallback,
                _config: Config,
            ) -> notify::Result<Box<dyn GitWatcherBackend>> {
                Err(notify::Error::generic("injected backend creation failure"))
            }
        }

        let root = tempfile::tempdir().expect("watch fixture");
        let worktree = root.path().join("worktree");
        let git_dir = root.path().join("git-dir");
        for path in [&worktree, &git_dir] {
            std::fs::create_dir_all(path).expect("watch root");
        }
        let service = GitWatchService::with_backend_factory(Arc::new(UnavailableBackendFactory));
        let mut subscription = service
            .subscribe(request(&worktree, &git_dir, &git_dir))
            .await
            .expect("fallback subscription remains available");

        assert_eq!(subscription.recv().await, Some(GitWatchEvent::Unavailable));
        assert_eq!(subscription.health(), GitWatcherHealth::FallbackRequired);
    }

    #[tokio::test]
    async fn partial_setup_rolls_back_and_publishes_unavailable() {
        let root = tempfile::tempdir().expect("watch fixture");
        let worktree = root.path().join("worktree");
        let git_dir = root.path().join("git-dir");
        let common_dir = root.path().join("common-dir");
        for path in [&worktree, &git_dir, &common_dir] {
            std::fs::create_dir_all(path).expect("watch root");
        }
        let backend = FakeWatcherBackendFactory::failing_on_watch(2);
        let service = GitWatchService::with_backend_factory(Arc::new(backend.clone()));
        let mut subscription = service
            .subscribe(request(&worktree, &git_dir, &common_dir))
            .await
            .expect("fallback subscription remains available");

        assert_eq!(subscription.recv().await, Some(GitWatchEvent::Unavailable));
        assert_eq!(subscription.health(), GitWatcherHealth::FallbackRequired);
        assert_eq!(
            backend
                .state
                .unwatches
                .lock()
                .expect("unwatches lock")
                .len(),
            1
        );
        assert_eq!(backend.dropped(), 1);
    }

    #[tokio::test]
    async fn retired_generation_callbacks_cannot_publish_into_reattachment() {
        let root = tempfile::tempdir().expect("watch fixture");
        let worktree = root.path().join("worktree");
        let git_dir = root.path().join("git-dir");
        for path in [&worktree, &git_dir] {
            std::fs::create_dir_all(path).expect("watch root");
        }
        let backend = FakeWatcherBackendFactory::default();
        let service = GitWatchService::with_backend_factory(Arc::new(backend.clone()));
        let first = service
            .subscribe(request(&worktree, &git_dir, &git_dir))
            .await
            .expect("first generation");
        drop(first);
        let mut second = service
            .subscribe(request(&worktree, &git_dir, &git_dir))
            .await
            .expect("second generation");
        let event = || {
            Ok(
                Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Content)))
                    .add_path(worktree.join("tracked.txt")),
            )
        };

        backend.emit(0, event());
        assert_eq!(second.try_recv(), None);
        backend.emit(1, event());
        assert_eq!(second.recv().await, Some(GitWatchEvent::WorkingTree));
        assert_eq!(backend.created(), 2);
        assert_eq!(backend.dropped(), 1);
    }

    #[tokio::test]
    async fn linked_worktree_watches_nested_common_refs_without_duplicate_roots() {
        let root = tempfile::tempdir().expect("watch fixture");
        let worktree = root.path().join("worktree");
        let git_dir = root.path().join("main.git/worktrees/topic");
        let common_dir = root.path().join("main.git");
        let upstream_dir = common_dir.join("refs/remotes/origin");
        for path in [&worktree, &git_dir, &upstream_dir] {
            std::fs::create_dir_all(path).expect("watch root");
        }
        let backend = FakeWatcherBackendFactory::default();
        let service = GitWatchService::with_backend_factory(Arc::new(backend.clone()));
        let mut subscription = service
            .subscribe(request(&worktree, &git_dir, &common_dir))
            .await
            .expect("linked-worktree watch");

        let watches = backend.state.watches.lock().expect("watches lock").clone();
        let canonical_worktree = std::fs::canonicalize(&worktree).expect("canonical worktree");
        let canonical_common_dir =
            std::fs::canonicalize(&common_dir).expect("canonical common directory");
        let expected = [
            (canonical_worktree, RecursiveMode::Recursive),
            (canonical_common_dir.clone(), RecursiveMode::NonRecursive),
            (
                canonical_common_dir.join("worktrees"),
                RecursiveMode::Recursive,
            ),
            (canonical_common_dir.join("refs"), RecursiveMode::Recursive),
        ];
        assert_eq!(
            watches.len(),
            expected.len(),
            "no duplicate native registrations"
        );
        for (expected_path, expected_mode) in expected {
            assert!(
                watches
                    .iter()
                    .any(|(_, path, mode)| path == &expected_path && *mode == expected_mode),
                "{}",
                expected_path.display()
            );
        }
        backend.emit(
            0,
            Ok(
                Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Content)))
                    .add_path(upstream_dir.join("main")),
            ),
        );
        expect_fake_event(
            &mut subscription,
            GitWatchEvent::RefMetadata,
            "nested common ref changes publish metadata",
        )
        .await;
    }

    #[tokio::test]
    async fn dropping_the_service_reaps_the_backend_and_late_callbacks_are_inert() {
        let root = tempfile::tempdir().expect("watch fixture");
        let worktree = root.path().join("worktree");
        let git_dir = root.path().join("git-dir");
        for path in [&worktree, &git_dir] {
            std::fs::create_dir_all(path).expect("watch root");
        }
        let backend = FakeWatcherBackendFactory::default();
        let service = GitWatchService::with_backend_factory(Arc::new(backend.clone()));
        let mut subscription = service
            .subscribe(request(&worktree, &git_dir, &git_dir))
            .await
            .expect("watch subscription");

        drop(service);
        assert_eq!(backend.dropped(), 1);
        let callback_backend = backend.clone();
        std::thread::spawn(move || {
            callback_backend.emit(
                0,
                Ok(
                    Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Content)))
                        .add_path(worktree.join("late.txt")),
                ),
            );
        })
        .join()
        .expect("late callback thread");
        assert_eq!(subscription.recv().await, None);
    }

    #[tokio::test]
    async fn shutdown_reaps_all_backends_rejects_reattachment_and_ignores_late_callbacks() {
        let root = tempfile::tempdir().expect("watch fixture");
        let worktree = root.path().join("worktree");
        let git_dir = root.path().join("git-dir");
        for path in [&worktree, &git_dir] {
            std::fs::create_dir_all(path).expect("watch root");
        }
        let backend = FakeWatcherBackendFactory::default();
        let service = GitWatchService::with_backend_factory(Arc::new(backend.clone()));
        let mut subscription = service
            .subscribe(request(&worktree, &git_dir, &git_dir))
            .await
            .expect("watch subscription");

        service.shutdown().await;
        assert_eq!(service.active_count_for_test(), 0);
        assert_eq!(backend.dropped(), 1);
        backend.emit(
            0,
            Ok(
                Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Content)))
                    .add_path(worktree.join("late.txt")),
            ),
        );
        assert_eq!(subscription.recv().await, None);
        assert!(matches!(
            service
                .subscribe(request(&worktree, &git_dir, &git_dir))
                .await,
            Err(GitWatchError::Shutdown)
        ));
    }

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn native_backend_observes_atomic_create_and_rename() {
        let mut fixture = NativeFixture::new(None).await;
        let temporary = fixture.worktree.join("tracked.txt.tmp");
        let tracked = fixture.worktree.join("tracked.txt");
        std::fs::write(&temporary, b"first").expect("atomic temporary write");
        std::fs::rename(&temporary, &tracked).expect("atomic rename");
        fixture.expect(GitWatchEvent::WorkingTree).await;
    }

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn native_backend_observes_content_modify() {
        use std::io::Write;

        let mut fixture = NativeFixture::new(Some(("tracked.txt", b"first"))).await;
        let tracked = fixture.worktree.join("tracked.txt");
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&tracked)
            .expect("open tracked file");
        file.write_all(b" second").expect("content write");
        file.sync_all().expect("flush content write");
        drop(file);
        fixture.expect(GitWatchEvent::WorkingTree).await;
    }

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn native_backend_observes_content_modify_with_nested_git_directory() {
        use std::io::Write;

        let _native_test_guard = acquire_native_watcher_test_permit().await;
        let root = tempfile::tempdir().expect("nested Git watch fixture");
        let worktree = root.path().join("worktree");
        let git_dir = worktree.join(".git");
        std::fs::create_dir_all(git_dir.join("refs")).expect("nested Git directory");
        let tracked = worktree.join("tracked.txt");
        std::fs::write(&tracked, b"first").expect("seed tracked file");
        let readiness = Arc::new(NativeReadinessState::default());
        let service = GitWatchService::with_backend_factory(Arc::new(
            NativeReadinessBackendFactory::new(Arc::clone(&readiness)),
        ));
        let mut subscription = test_timeout(
            "nested native watch subscription setup",
            service.subscribe(request(&worktree, &git_dir, &git_dir)),
        )
        .await
        .expect("nested native watch subscription");
        assert_native_watching_available(&subscription);
        let registrations = readiness.registrations();
        assert_eq!(
            registrations.len(),
            1,
            "native worktree recursion includes its Git directory and refs"
        );
        std::fs::write(&registrations[0].sentinel, b"ready").expect("native readiness sentinel");
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            registrations[0].readiness.wait(),
        )
        .await
        .expect("nested native readiness deadline");
        std::fs::remove_file(&registrations[0].sentinel).expect("remove native readiness sentinel");
        assert_eq!(subscription.try_recv(), None);

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&tracked)
            .expect("open tracked file");
        file.write_all(b" second").expect("content write");
        file.sync_all().expect("flush content write");
        drop(file);
        expect_native_event(&mut subscription, GitWatchEvent::WorkingTree).await;
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn native_backend_observes_rename() {
        let mut fixture = NativeFixture::new(None).await;
        let renamed = fixture.worktree.join("renamed.txt");
        std::fs::rename(&fixture.rename_source, &renamed).expect("tracked rename");
        fixture.expect(GitWatchEvent::WorkingTree).await;
    }

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn native_backend_observes_delete() {
        let mut fixture = NativeFixture::new(Some(("obsolete/tracked.txt", b"first"))).await;
        std::fs::remove_dir_all(fixture.worktree.join("obsolete")).expect("tracked delete");
        fixture.expect(GitWatchEvent::WorkingTree).await;
    }

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn native_backend_observes_metadata_stores_created_after_attachment() {
        for relative in [
            "logs/refs/stash",
            "worktrees/new/HEAD",
            "reftable/tables.list",
        ] {
            let mut fixture = NativeFixture::new(None).await;
            let path = fixture.common_dir.join(relative);
            std::fs::create_dir_all(path.parent().expect("metadata parent"))
                .expect("new metadata directory");
            std::fs::write(path, b"new ref state\n").expect("new metadata file");
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    match fixture.subscription.recv().await {
                        Some(GitWatchEvent::Metadata) => {}
                        Some(GitWatchEvent::RefMetadata) => break,
                        event => panic!("unexpected new metadata event for {relative}: {event:?}"),
                    }
                }
            })
            .await
            .unwrap_or_else(|_| panic!("new metadata store {relative} was missed"));
            assert_eq!(fixture.subscription.health(), GitWatcherHealth::Healthy);
        }
    }

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn native_backend_observes_head() {
        let mut fixture = NativeFixture::new(None).await;
        std::fs::write(fixture.git_dir.join("HEAD"), b"ref: refs/heads/main\n")
            .expect("HEAD write");
        fixture.expect(GitWatchEvent::RefMetadata).await;
    }

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn native_backend_observes_index() {
        let mut fixture = NativeFixture::new(None).await;
        std::fs::write(fixture.git_dir.join("index"), b"index contents").expect("index write");
        fixture.expect(GitWatchEvent::Metadata).await;
    }

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn native_backend_observes_packed_refs() {
        let mut fixture = NativeFixture::new(None).await;
        let lock = fixture.common_dir.join("packed-refs.lock");
        std::fs::write(&lock, b"# pack-refs with: peeled fully-peeled\n")
            .expect("packed refs lock write");
        std::fs::rename(lock, fixture.common_dir.join("packed-refs")).expect("packed refs commit");
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match fixture.subscription.recv().await {
                    Some(GitWatchEvent::Metadata) => {} // Earlier lock-file notifications.
                    Some(GitWatchEvent::RefMetadata) => break,
                    event => panic!("unexpected packed-refs event: {event:?}"),
                }
            }
        })
        .await
        .expect("packed-refs rename invalidates the signal");
    }

    #[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
    #[tokio::test]
    async fn native_backend_observes_nested_refs() {
        let mut fixture = NativeFixture::new(None).await;
        std::fs::write(fixture.upstream_dir.join("main"), b"0123456789abcdef\n")
            .expect("nested upstream ref write");
        fixture.expect(GitWatchEvent::RefMetadata).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_backend_does_not_follow_worktree_symlink_to_an_outside_target() {
        use std::os::unix::fs::symlink;

        let _native_test_guard = acquire_native_watcher_test_permit().await;
        let root = tempfile::tempdir().expect("watch fixture");
        let worktree = root.path().join("worktree");
        let outside = root.path().join("outside");
        let git_dir = root.path().join("git-dir");
        let common_dir = root.path().join("common-dir");
        for path in [&worktree, &outside, &git_dir, &common_dir.join("refs")] {
            std::fs::create_dir_all(path).expect("watch root");
        }
        symlink(&outside, worktree.join("outside-link")).expect("outside symlink");
        let service = GitWatchService::new();
        let mut subscription = test_timeout(
            "native symlink watch subscription setup",
            service.subscribe(request(&worktree, &git_dir, &common_dir)),
        )
        .await
        .expect("native watch subscription");
        assert_native_watching_available(&subscription);

        std::fs::write(outside.join("not-in-worktree.txt"), b"outside")
            .expect("outside target write");
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(300), subscription.recv())
                .await
                .is_err(),
            "outside-target activity must not traverse the worktree symlink"
        );
    }

    #[test]
    fn fake_platform_shapes_classify_mutations_without_escaping_admitted_roots() {
        let windows = identity_for_paths(
            r"C:\repo",
            r"C:\repo\.git",
            r"C:\repo\.git",
            HostPathPlatform::Windows,
        );
        let linux = identity_for_paths(
            "/repo",
            "/repo/.git/worktrees/topic",
            "/repo/.git",
            HostPathPlatform::Posix,
        );
        let macos = identity_for_paths(
            "/Users/dev/repo",
            "/Users/dev/repo/.git/worktrees/topic",
            "/Users/dev/repo/.git",
            HostPathPlatform::Posix,
        );
        let windows_registered = vec![(windows.worktree_root.clone(), RecursiveMode::Recursive)];
        let linux_registered = vec![
            (linux.worktree_root.clone(), RecursiveMode::Recursive),
            (linux.git_dir.clone(), RecursiveMode::NonRecursive),
            (linux.common_dir.clone(), RecursiveMode::NonRecursive),
        ];
        let macos_registered = vec![
            (macos.worktree_root.clone(), RecursiveMode::Recursive),
            (macos.git_dir.clone(), RecursiveMode::NonRecursive),
            (macos.common_dir.clone(), RecursiveMode::NonRecursive),
        ];

        assert_eq!(
            classify_event(
                &Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
                    .add_path(r"C:\repo\tracked.txt.tmp".into())
                    .add_path(r"C:\repo\tracked.txt".into()),
                &windows,
                std::slice::from_ref(&windows),
                &windows_registered,
                HostPathPlatform::Windows,
            ),
            Some(GitWatchEvent::WorkingTree)
        );
        assert_eq!(
            classify_event(
                &Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Content)))
                    .add_path("/repo/.git/worktrees/topic/HEAD".into()),
                &linux,
                std::slice::from_ref(&linux),
                &linux_registered,
                HostPathPlatform::Posix,
            ),
            Some(GitWatchEvent::RefMetadata)
        );
        assert_eq!(
            classify_event(
                &Event::new(EventKind::Modify(ModifyKind::Name(RenameMode::Both)))
                    .add_path("/repo/tracked.txt".into())
                    .add_path("/repo/.git/worktrees/topic/index".into()),
                &linux,
                std::slice::from_ref(&linux),
                &linux_registered,
                HostPathPlatform::Posix,
            ),
            Some(GitWatchEvent::Metadata),
            "one backend burst touching Git metadata invalidates metadata even when it also touches the worktree"
        );
        assert_eq!(
            classify_event(
                &Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Content)))
                    .add_path(r"C:\repo\..\outside\tracked.txt".into()),
                &windows,
                std::slice::from_ref(&windows),
                &windows_registered,
                HostPathPlatform::Windows,
            ),
            None
        );
        assert_eq!(
            classify_event(
                &Event::new(EventKind::Any)
                    .add_path("/Users/dev/repo/.git/refs/remotes/origin/main".into()),
                &macos,
                std::slice::from_ref(&macos),
                &macos_registered,
                HostPathPlatform::Posix,
            ),
            Some(GitWatchEvent::RefMetadata)
        );
        assert_eq!(
            classify_event(
                &Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Content)))
                    .add_path("/repo-other/tracked.txt".into()),
                &linux,
                std::slice::from_ref(&linux),
                &linux_registered,
                HostPathPlatform::Posix,
            ),
            None,
            "lexical containment rejects sibling roots with a shared prefix"
        );
        assert_eq!(
            classify_event(
                &Event::new(EventKind::Access(AccessKind::Close(AccessMode::Write)))
                    .add_path(r"C:\repo\tracked.txt.tmp".into()),
                &windows,
                std::slice::from_ref(&windows),
                &windows_registered,
                HostPathPlatform::Windows,
            ),
            Some(GitWatchEvent::WorkingTree),
            "temporary names remain invalidating for write-close events"
        );
        assert_eq!(
            classify_event(
                &Event::new(EventKind::Access(AccessKind::Read))
                    .add_path(r"C:\repo\tracked.txt.tmp".into()),
                &windows,
                std::slice::from_ref(&windows),
                &windows_registered,
                HostPathPlatform::Windows,
            ),
            None
        );
    }
}
