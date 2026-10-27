use std::{
    collections::{HashMap, HashSet, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use tokio::sync::{Mutex as AsyncMutex, mpsc, watch};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[cfg(test)]
use super::watcher::test_timeout;

use super::{
    GitCommandError, GitRepository, GitWatchError, GitWatchRequest, GitWatchService,
    GitWatcherHealth, VcsStatusLocalResult, VcsStatusRemoteResult, VcsStatusResult,
    VcsStatusStreamEvent,
    fetch_owner::RepositoryFetchOwner,
    repository::GitManagerSignalHead,
    status_owner::{
        StatusMutationGuard, StatusOutputKind, StatusReadFence, StatusReadKey, StatusReadOwner,
        StatusSignalCallbacks, WatcherSignalSource, is_retired_read_error,
        run_status_signal_scheduler,
    },
};

// Bound setup retries for pruned or superseded subscribers and stale or retired reads.
const SETUP_ADMISSION_ATTEMPTS: usize = 3;

#[derive(Clone)]
pub struct StatusBroadcaster {
    inner: Arc<Inner>,
}

struct Inner {
    repository: Arc<GitRepository>,
    local_status_refresh_interval: Duration,
    subscriber_capacity: usize,
    fetch_owner: RepositoryFetchOwner,
    watcher: GitWatchService,
    status_owner: StatusReadOwner,
    subscription_setups: TaskTracker,
    subscription_cancellation: CancellationToken,
    state: Mutex<State>,
    #[cfg(test)]
    fetch_attachment_finished: Arc<tokio::sync::Notify>,
    #[cfg(test)]
    status_scheduler_finished: Arc<tokio::sync::Notify>,
    #[cfg(test)]
    status_scheduler_started: Arc<tokio::sync::Notify>,
    #[cfg(test)]
    active_status_schedulers: Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(test)]
    retirement_wait_started: Arc<tokio::sync::Notify>,
    #[cfg(test)]
    retirement_wait_starts: std::sync::atomic::AtomicUsize,
    #[cfg(test)]
    subscription_setup_wait_started: Arc<tokio::sync::Notify>,
    #[cfg(test)]
    retirement_epoch_gate: Mutex<Option<Arc<RetirementEpochGate>>>,
    #[cfg(test)]
    subscription_registration_gate: Mutex<Option<Arc<SubscriptionRegistrationGate>>>,
    #[cfg(test)]
    lifecycle_insertion_gate: Mutex<Option<Arc<LifecycleInsertionGate>>>,
    #[cfg(test)]
    retirement_sentinel_finished: Arc<tokio::sync::Notify>,
    #[cfg(test)]
    post_retirement_wait_gate: Mutex<Option<Arc<SubscribeAttemptGate>>>,
    #[cfg(test)]
    post_initial_read_gate: Mutex<Option<Arc<SubscribeAttemptGate>>>,
    #[cfg(test)]
    registration_outcome_probe: Mutex<Option<Arc<RegistrationOutcomeProbe>>>,
}

#[derive(Default)]
struct State {
    closed: bool,
    next_subscriber_id: u64,
    repositories: HashMap<PathBuf, RepositoryState>,
    retiring: HashMap<PathBuf, Vec<TaskTracker>>,
}

struct RepositoryState {
    lifecycle_id: u64,
    repository_key: Option<PathBuf>,
    fetch_attachment_in_flight: bool,
    fetch_attachment_requested: bool,
    local: VcsStatusLocalResult,
    remote: Option<Option<VcsStatusRemoteResult>>,
    remote_fence: Option<StatusReadFence>,
    remote_ref_name: Option<Option<String>>,
    pending_local_reconcile: bool,
    remote_refresh_requests: watch::Sender<u64>,
    git_manager_signature: Option<u64>,
    git_manager_signal: watch::Sender<GitManagerSignal>,
    git_manager_read_lock: Arc<AsyncMutex<()>>,
    git_manager_common_dir: Option<PathBuf>,
    subscribers: HashMap<u64, RepositorySubscriber>,
    poller_cancellation: CancellationToken,
    retirement_cancellation: CancellationToken,
    tasks: TaskTracker,
}

enum RepositorySubscriber {
    Status {
        sender: mpsc::Sender<StatusPublication<VcsStatusStreamEvent>>,
        fell_behind: Arc<AtomicBool>,
    },
    GitManager,
}

#[derive(Clone, Copy)]
enum SubscriptionKind {
    Status,
    GitManager,
}

enum BroadcasterSubscription {
    Status(StatusSubscription),
    GitManager(GitManagerSignalSubscription),
}

struct RepositoryRetirementFence(CancellationToken);

impl Drop for RepositoryRetirementFence {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

pub struct StatusSubscription {
    receiver: mpsc::Receiver<StatusPublication<VcsStatusStreamEvent>>,
    fell_behind: Arc<AtomicBool>,
    cancellation: CancellationToken,
    broadcaster: StatusBroadcaster,
    cwd: PathBuf,
    subscriber_id: u64,
}

pub struct GitManagerSignalSubscription {
    receiver: watch::Receiver<GitManagerSignal>,
    pending_initial: bool,
    cancellation: CancellationToken,
    broadcaster: StatusBroadcaster,
    cwd: PathBuf,
    subscriber_id: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GitManagerSignal {
    pub generation: u64,
    pub watcher_degraded: bool,
}

#[derive(Clone)]
pub(crate) struct StatusPublication<T> {
    pub value: T,
    pub local: VcsStatusLocalResult,
    pub fence: StatusReadFence,
}

struct StatusRefreshTarget {
    broadcaster: StatusBroadcaster,
    cwd: PathBuf,
    lifecycle_id: u64,
    cancellation: CancellationToken,
}

impl StatusSignalCallbacks for StatusRefreshTarget {
    async fn refresh_local(&self) {
        let _ = self
            .broadcaster
            .refresh_local_for_lifecycle(&self.cwd, Some(self.lifecycle_id), &self.cancellation)
            .await;
    }
    async fn refresh_refs(&self) {
        if let Err(error) = self
            .broadcaster
            .refresh_git_manager_signal_for_lifecycle(
                &self.cwd,
                Some(self.lifecycle_id),
                &self.cancellation,
            )
            .await
            && !self.cancellation.is_cancelled()
        {
            log_signature_failure(&error);
        }
    }
    fn on_health_change(&self, health: GitWatcherHealth) {
        self.broadcaster
            .publish_watcher_health(&self.cwd, self.lifecycle_id, health);
    }
}

fn log_signature_failure(error: &GitCommandError) {
    tracing::warn!(operation = %error.operation, "Git Manager signature observation failed; retaining the previous signature");
}

impl StatusBroadcaster {
    #[must_use]
    pub fn new(
        repository: Arc<GitRepository>,
        local_status_refresh_interval: Duration,
        subscriber_capacity: usize,
    ) -> Self {
        let (automatic_remote_refresh_interval, _) = watch::channel(local_status_refresh_interval);
        Self::with_automatic_remote_refresh_interval(
            repository,
            local_status_refresh_interval,
            automatic_remote_refresh_interval,
            subscriber_capacity,
        )
    }

    #[must_use]
    pub fn with_automatic_remote_refresh_interval(
        repository: Arc<GitRepository>,
        local_status_refresh_interval: Duration,
        automatic_remote_refresh_interval: watch::Sender<Duration>,
        subscriber_capacity: usize,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                fetch_owner: RepositoryFetchOwner::new(
                    Arc::clone(&repository),
                    automatic_remote_refresh_interval,
                ),
                watcher: GitWatchService::new(),
                repository,
                local_status_refresh_interval,
                subscriber_capacity: subscriber_capacity.max(1),
                status_owner: StatusReadOwner::new(),
                subscription_setups: TaskTracker::new(),
                subscription_cancellation: CancellationToken::new(),
                state: Mutex::new(State::default()),
                #[cfg(test)]
                fetch_attachment_finished: Arc::new(tokio::sync::Notify::new()),
                #[cfg(test)]
                status_scheduler_finished: Arc::new(tokio::sync::Notify::new()),
                #[cfg(test)]
                status_scheduler_started: Arc::new(tokio::sync::Notify::new()),
                #[cfg(test)]
                active_status_schedulers: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                #[cfg(test)]
                retirement_wait_started: Arc::new(tokio::sync::Notify::new()),
                #[cfg(test)]
                retirement_wait_starts: std::sync::atomic::AtomicUsize::new(0),
                #[cfg(test)]
                subscription_setup_wait_started: Arc::new(tokio::sync::Notify::new()),
                #[cfg(test)]
                retirement_epoch_gate: Mutex::new(None),
                #[cfg(test)]
                subscription_registration_gate: Mutex::new(None),
                #[cfg(test)]
                lifecycle_insertion_gate: Mutex::new(None),
                #[cfg(test)]
                retirement_sentinel_finished: Arc::new(tokio::sync::Notify::new()),
                #[cfg(test)]
                post_retirement_wait_gate: Mutex::new(None),
                #[cfg(test)]
                post_initial_read_gate: Mutex::new(None),
                #[cfg(test)]
                registration_outcome_probe: Mutex::new(None),
            }),
        }
    }

    #[cfg(test)]
    fn with_watcher_for_test(repository: Arc<GitRepository>, watcher: GitWatchService) -> Self {
        let mut broadcaster = Self::new(repository, Duration::from_secs(3_600), 4);
        Arc::get_mut(&mut broadcaster.inner)
            .expect("new test broadcaster is uniquely owned")
            .watcher = watcher;
        broadcaster
    }

    pub async fn subscribe(
        &self,
        cwd: PathBuf,
        cancellation: CancellationToken,
    ) -> Result<StatusSubscription, GitCommandError> {
        match self
            .subscribe_kind(cwd, cancellation, SubscriptionKind::Status)
            .await?
        {
            BroadcasterSubscription::Status(subscription) => Ok(subscription),
            BroadcasterSubscription::GitManager(_) => unreachable!("status subscription kind"),
        }
    }

    pub async fn subscribe_git_manager_signal(
        &self,
        cwd: PathBuf,
        cancellation: CancellationToken,
    ) -> Result<GitManagerSignalSubscription, GitCommandError> {
        match self
            .subscribe_kind(cwd, cancellation, SubscriptionKind::GitManager)
            .await?
        {
            BroadcasterSubscription::GitManager(subscription) => Ok(subscription),
            BroadcasterSubscription::Status(_) => unreachable!("Git Manager subscription kind"),
        }
    }

    async fn subscribe_kind(
        &self,
        cwd: PathBuf,
        cancellation: CancellationToken,
        kind: SubscriptionKind,
    ) -> Result<BroadcasterSubscription, GitCommandError> {
        let setup_cancellation = self.inner.subscription_cancellation.child_token();
        let broadcaster = self.clone();
        let error_cwd = cwd.clone();
        let join_error_cwd = cwd.clone();
        let caller_cancellation = cancellation.child_token();
        let _caller_lifetime = caller_cancellation.clone().drop_guard();
        let caller_setup_cancellation = setup_cancellation.clone();
        let subscriber_cancellation = cancellation.clone();
        let setup = {
            let state = self.lock_state();
            if state.closed {
                return Err(broadcaster_shutdown_error(&cwd));
            }
            self.inner.subscription_setups.spawn(async move {
                let result = tokio::select! {
                    biased;
                    () = caller_cancellation.cancelled() => {
                        caller_setup_cancellation.cancel();
                        Err(broadcaster_shutdown_error(&error_cwd))
                    }
                    result = broadcaster.subscribe_inner(
                        cwd,
                        setup_cancellation,
                        subscriber_cancellation,
                        kind,
                    ) => result,
                };
                result
            })
        };
        let result = setup
            .await
            .unwrap_or_else(|_| Err(broadcaster_shutdown_error(&join_error_cwd)));
        if self.lock_state().closed || cancellation.is_cancelled() {
            Err(broadcaster_shutdown_error(&join_error_cwd))
        } else {
            result
        }
    }

    async fn subscribe_inner(
        &self,
        cwd: PathBuf,
        setup_cancellation: CancellationToken,
        subscriber_cancellation: CancellationToken,
        kind: SubscriptionKind,
    ) -> Result<BroadcasterSubscription, GitCommandError> {
        let cwd = tokio::fs::canonicalize(&cwd).await.unwrap_or(cwd);
        let mut admission_attempts = 0;
        loop {
            self.await_retired_lifecycle(&cwd).await;
            #[cfg(test)]
            let post_retirement_wait_gate = self
                .inner
                .post_retirement_wait_gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            #[cfg(test)]
            if let Some(gate) = post_retirement_wait_gate {
                gate.block().await;
            }
            if self.lock_state().closed
                || setup_cancellation.is_cancelled()
                || subscriber_cancellation.is_cancelled()
            {
                return Err(broadcaster_shutdown_error(&cwd));
            }
            if admission_attempts == SETUP_ADMISSION_ATTEMPTS {
                return Err(setup_admission_error(&cwd));
            }
            let local_refresh_requests = self.inner.status_owner.subscribe_local_refresh(&cwd);
            let roots = self
                .inner
                .repository
                .resolve_watch_roots(&cwd, &setup_cancellation)
                .await;
            let resolved_common_dir = roots.as_ref().ok().map(|roots| roots.common_dir.clone());
            let roots = match roots {
                Ok(roots) => Some(roots),
                Err(_) => {
                    self.inner
                        .repository
                        .fallback_watch_roots(&cwd, &setup_cancellation)
                        .await
                }
            };
            if self.lock_state().closed {
                return Err(broadcaster_shutdown_error(&cwd));
            }
            let watcher = match roots {
                Some(roots) => match self
                    .inner
                    .watcher
                    .subscribe(GitWatchRequest {
                        worktree_root: roots.worktree_root,
                        git_dir: roots.git_dir,
                        common_dir: roots.common_dir,
                    })
                    .await
                {
                    Ok(subscription) => WatcherSignalSource::new(Some(subscription)),
                    Err(GitWatchError::Shutdown) => return Err(broadcaster_shutdown_error(&cwd)),
                    Err(GitWatchError::Root { .. }) => WatcherSignalSource::new(None),
                },
                None => WatcherSignalSource::new(None),
            };
            let repository = Arc::clone(&self.inner.repository);
            let load_cwd = cwd.clone();
            let initial_read_started = tokio::time::Instant::now();
            let read = match self
                .inner
                .status_owner
                .read_local(
                    StatusReadKey {
                        canonical_cwd: cwd.clone(),
                        output_kind: StatusOutputKind::Local,
                    },
                    &setup_cancellation,
                    move |shared_cancellation| async move {
                        repository
                            .local_status(&load_cwd, &shared_cancellation)
                            .await
                    },
                )
                .await
            {
                Ok(read) => read,
                Err(error) if is_retired_read_error(&error) => {
                    admission_attempts += 1;
                    continue;
                }
                Err(error) => return Err(error),
            };
            #[cfg(test)]
            let post_initial_read_gate = self
                .inner
                .post_initial_read_gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            #[cfg(test)]
            if let Some(gate) = post_initial_read_gate {
                gate.block().await;
            }
            if self.lock_state().closed {
                return Err(broadcaster_shutdown_error(&cwd));
            }
            let initial_read_duration = initial_read_started.elapsed();
            let fence = read.fence();
            let (sender, receiver) = mpsc::channel(self.inner.subscriber_capacity);
            let fell_behind = Arc::new(AtomicBool::new(false));
            #[cfg(test)]
            let retirement_sentinel_finished = Arc::clone(&self.inner.retirement_sentinel_finished);

            let mut previous_local = None;
            let registration = self.inner.status_owner.publish_if_current(read, |local| {
                let mut state = self.lock_state();
                if state.closed {
                    return None;
                }
                if state.retiring.contains_key(&cwd) {
                    return Some(Err(()));
                }
                let subscriber_id = state.next_subscriber_id;
                state.next_subscriber_id = state.next_subscriber_id.wrapping_add(1);
                let start_poller = !state.repositories.contains_key(&cwd);
                let entry = state.repositories.entry(cwd.clone()).or_insert_with(|| {
                    let (remote_refresh_requests, _) = watch::channel(0);
                    let (git_manager_signal, _) = watch::channel(GitManagerSignal {
                        generation: 0,
                        watcher_degraded: watcher.current_health()
                            == GitWatcherHealth::FallbackRequired,
                    });
                    let tasks = TaskTracker::new();
                    let retirement_cancellation = CancellationToken::new();
                    let retirement_wait = retirement_cancellation.clone();
                    tasks.spawn(async move {
                        retirement_wait.cancelled().await;
                        #[cfg(test)]
                        retirement_sentinel_finished.notify_waiters();
                    });
                    RepositoryState {
                        lifecycle_id: subscriber_id,
                        repository_key: None,
                        fetch_attachment_in_flight: false,
                        fetch_attachment_requested: false,
                        local: local.clone(),
                        remote: None,
                        remote_fence: None,
                        remote_ref_name: None,
                        pending_local_reconcile: false,
                        remote_refresh_requests,
                        git_manager_signature: None,
                        git_manager_signal,
                        git_manager_read_lock: Arc::new(AsyncMutex::new(())),
                        git_manager_common_dir: resolved_common_dir.clone(),
                        subscribers: HashMap::new(),
                        poller_cancellation: CancellationToken::new(),
                        retirement_cancellation,
                        tasks,
                    }
                });
                if entry.local != *local {
                    let ref_changed = entry.local.ref_name != local.ref_name;
                    previous_local = Some(std::mem::replace(&mut entry.local, local.clone()));
                    reconcile_remote_after_local_publication(entry, ref_changed);
                    publish(
                        entry,
                        VcsStatusStreamEvent::LocalUpdated {
                            local: local.clone(),
                        },
                        &fence,
                    );
                } else {
                    reconcile_remote_after_local_publication(entry, false);
                }
                entry.subscribers.insert(
                    subscriber_id,
                    match kind {
                        SubscriptionKind::Status => RepositorySubscriber::Status {
                            sender,
                            fell_behind: Arc::clone(&fell_behind),
                        },
                        SubscriptionKind::GitManager => RepositorySubscriber::GitManager,
                    },
                );
                let lifecycle_insertion_reservation = start_poller.then(|| entry.tasks.token());
                let initial_remote = (entry.remote_fence.as_ref() == Some(&fence)
                    && entry.remote_ref_name.as_ref() == Some(&local.ref_name))
                .then(|| entry.remote.clone().flatten())
                .flatten();
                if let Some(RepositorySubscriber::Status { sender, .. }) =
                    entry.subscribers.get(&subscriber_id)
                {
                    sender
                        .try_send(StatusPublication {
                            value: VcsStatusStreamEvent::Snapshot {
                                local: local.clone(),
                                remote: initial_remote,
                            },
                            local: local.clone(),
                            fence: fence.clone(),
                        })
                        .expect("new bounded subscription has capacity for its snapshot");
                }
                Some(Ok((
                    subscriber_id,
                    start_poller,
                    entry.poller_cancellation.clone(),
                    local_refresh_requests,
                    entry.remote_refresh_requests.subscribe(),
                    entry.remote_refresh_requests.clone(),
                    entry.git_manager_signal.subscribe(),
                    entry.lifecycle_id,
                    entry.repository_key.clone(),
                    entry.tasks.clone(),
                    lifecycle_insertion_reservation,
                )))
            });
            let (local, registration) = match registration {
                Ok(registration) => registration,
                Err(error) if is_retired_read_error(&error) => {
                    // A rejected read never invokes the registration callback.
                    admission_attempts += 1;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let registration = match registration {
                None => return Err(broadcaster_shutdown_error(&cwd)),
                Some(Err(())) => {
                    #[cfg(test)]
                    if let Some(probe) = self
                        .inner
                        .registration_outcome_probe
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take()
                    {
                        probe.report(true);
                    }
                    continue;
                }
                Some(Ok(registration)) => {
                    #[cfg(test)]
                    if let Some(probe) = self
                        .inner
                        .registration_outcome_probe
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take()
                    {
                        probe.report(false);
                    }
                    registration
                }
            };
            let (
                subscriber_id,
                start_poller,
                poller_cancellation,
                local_refresh_requests,
                remote_refresh_requests,
                remote_reconcile,
                git_manager_signal,
                lifecycle_id,
                repository_key,
                lifecycle_tasks,
                lifecycle_insertion_reservation,
            ) = registration;
            self.observe_repository_availability_change(
                &cwd,
                Some(lifecycle_id),
                previous_local.as_ref(),
                &local,
            );
            admission_attempts += 1;
            // Own release() cleanup across every setup await, retry, and cancellation.
            let subscription = match kind {
                SubscriptionKind::Status => BroadcasterSubscription::Status(StatusSubscription {
                    receiver,
                    fell_behind,
                    cancellation: subscriber_cancellation.clone(),
                    broadcaster: self.clone(),
                    cwd: cwd.clone(),
                    subscriber_id,
                }),
                SubscriptionKind::GitManager => {
                    BroadcasterSubscription::GitManager(GitManagerSignalSubscription {
                        receiver: git_manager_signal,
                        pending_initial: true,
                        cancellation: subscriber_cancellation.clone(),
                        broadcaster: self.clone(),
                        cwd: cwd.clone(),
                        subscriber_id,
                    })
                }
            };
            #[cfg(test)]
            let registration_gate = self
                .inner
                .subscription_registration_gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            #[cfg(test)]
            if let Some(gate) = registration_gate {
                gate.block(&lifecycle_tasks).await;
            }
            let admitted = {
                let state = self.lock_state();
                let admitted = !state.closed
                    && state.repositories.get(&cwd).is_some_and(|entry| {
                        entry.lifecycle_id == lifecycle_id
                            && entry.subscribers.contains_key(&subscriber_id)
                    });
                if admitted && let Some(repository_key) = repository_key {
                    self.inner.fetch_owner.attach(
                        repository_key,
                        cwd.clone(),
                        subscriber_id,
                        local.ref_name.clone(),
                        remote_reconcile,
                    );
                }
                admitted
            };
            if !admitted {
                continue;
            }
            #[cfg(test)]
            let insertion_gate = self
                .inner
                .lifecycle_insertion_gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            #[cfg(test)]
            if let Some(gate) = insertion_gate {
                gate.block(&lifecycle_tasks).await;
            }
            if start_poller {
                self.spawn_local_status_poller(
                    cwd.clone(),
                    lifecycle_id,
                    poller_cancellation.clone(),
                    local_refresh_requests,
                    (watcher, initial_read_duration),
                    &lifecycle_tasks,
                );
                self.spawn_remote_reconciliation(
                    cwd.clone(),
                    lifecycle_id,
                    poller_cancellation.clone(),
                    remote_refresh_requests,
                    &lifecycle_tasks,
                );
                self.spawn_fetch_attachment(cwd.clone(), lifecycle_id, resolved_common_dir);
                lifecycle_tasks.close();
            }
            drop(lifecycle_insertion_reservation);
            let admitted = {
                let state = self.lock_state();
                !state.closed
                    && state.repositories.get(&cwd).is_some_and(|entry| {
                        entry.lifecycle_id == lifecycle_id
                            && entry.subscribers.contains_key(&subscriber_id)
                    })
            };
            if !admitted {
                continue;
            }
            return Ok(subscription);
        }
    }

    pub async fn refresh_local(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<VcsStatusLocalResult, GitCommandError> {
        let cwd = tokio::fs::canonicalize(cwd)
            .await
            .unwrap_or_else(|_| cwd.to_path_buf());
        let lifecycle_id = self
            .lock_state()
            .repositories
            .get(&cwd)
            .map(|entry| entry.lifecycle_id);
        self.refresh_local_for_lifecycle(&cwd, lifecycle_id, cancellation)
            .await
    }

    async fn refresh_local_for_lifecycle(
        &self,
        cwd: &Path,
        lifecycle_id: Option<u64>,
        cancellation: &CancellationToken,
    ) -> Result<VcsStatusLocalResult, GitCommandError> {
        let cwd = tokio::fs::canonicalize(cwd)
            .await
            .unwrap_or_else(|_| cwd.to_path_buf());
        let repository = Arc::clone(&self.inner.repository);
        let load_cwd = cwd.clone();
        let read = self
            .inner
            .status_owner
            .read_local(
                StatusReadKey {
                    canonical_cwd: cwd.clone(),
                    output_kind: StatusOutputKind::Local,
                },
                cancellation,
                move |shared_cancellation| async move {
                    repository
                        .local_status(&load_cwd, &shared_cancellation)
                        .await
                },
            )
            .await?;
        let fence = read.fence();
        let (local, retirement) = self.inner.status_owner.publish_if_current(read, |local| {
            if let Some(lifecycle_id) = lifecycle_id {
                self.publish_local(&cwd, lifecycle_id, local, &fence)
            } else {
                None
            }
        })?;
        self.finish_repository_retirement(&cwd, retirement);
        Ok(local)
    }

    fn publish_local(
        &self,
        cwd: &Path,
        lifecycle_id: u64,
        local: &VcsStatusLocalResult,
        fence: &StatusReadFence,
    ) -> Option<RepositoryRetirementFence> {
        let event = VcsStatusStreamEvent::LocalUpdated {
            local: local.clone(),
        };
        let mut state = self.lock_state();
        let mut remove_repository = false;
        let mut accepted = false;
        let mut previous_local = None;
        if let Some(entry) = state
            .repositories
            .get_mut(cwd)
            .filter(|entry| entry.lifecycle_id == lifecycle_id)
        {
            accepted = true;
            if entry.local != *local {
                let ref_changed = entry.local.ref_name != local.ref_name;
                previous_local = Some(std::mem::replace(&mut entry.local, local.clone()));
                reconcile_remote_after_local_publication(entry, ref_changed);
                publish(entry, event, fence);
                remove_repository = entry.subscribers.is_empty();
            } else {
                reconcile_remote_after_local_publication(entry, false);
            }
        }
        let retirement = if remove_repository {
            state
                .repositories
                .remove(cwd)
                .map(|entry| Self::retire_repository(&mut state, cwd, entry))
        } else {
            None
        };
        drop(state);
        self.observe_repository_availability_change(
            cwd,
            Some(lifecycle_id),
            previous_local.as_ref(),
            local,
        );
        if accepted {
            self.inner
                .fetch_owner
                .update_worktree_ref(cwd, local.ref_name.clone());
        }
        retirement
    }

    fn observe_repository_availability_change(
        &self,
        cwd: &Path,
        lifecycle_id: Option<u64>,
        previous: Option<&VcsStatusLocalResult>,
        local: &VcsStatusLocalResult,
    ) {
        if previous.is_some_and(|previous| {
            previous.is_repo != local.is_repo
                || previous.repository_unavailable_reason != local.repository_unavailable_reason
        }) {
            self.inner.status_owner.observe_local_change(cwd);
        }
        if local.is_repo
            && previous.is_some_and(|previous| !previous.is_repo)
            && let Some(lifecycle_id) = lifecycle_id
        {
            self.spawn_fetch_attachment(cwd.to_path_buf(), lifecycle_id, None);
        }
    }

    pub async fn notify_local_change(&self, cwd: &Path) {
        if self.lock_state().closed {
            return;
        }
        let cwd = tokio::select! {
            biased;
            () = self.inner.subscription_cancellation.cancelled() => return,
            result = tokio::fs::canonicalize(cwd) => {
                result.unwrap_or_else(|_| cwd.to_path_buf())
            }
        };
        let worktree = {
            let state = self.lock_state();
            if state.closed {
                return;
            }
            state
                .repositories
                .keys()
                .filter(|worktree| cwd.starts_with(worktree))
                .max_by_key(|worktree| worktree.components().count())
                .cloned()
        };
        if let Some(worktree) = worktree {
            self.inner.status_owner.request_local_refresh(&worktree);
        }
    }

    /// Registers the observer that runs after a finished mutation or a
    /// reported local change (`notify_local_change`). Status reads, including
    /// watcher-driven reads, also reach it when repository availability changes.
    /// It receives the canonical worktree path and must only schedule work,
    /// never block.
    pub fn set_local_change_observer(&self, observer: impl Fn(&Path) + Send + Sync + 'static) {
        self.inner
            .status_owner
            .set_local_change_observer(Arc::new(observer));
    }

    pub async fn begin_mutation(&self, cwd: &Path) -> StatusMutationGuard {
        let cwd = tokio::fs::canonicalize(cwd)
            .await
            .unwrap_or_else(|_| cwd.to_path_buf());
        self.inner.status_owner.begin_mutation(cwd).await
    }

    #[cfg(test)]
    pub(crate) async fn acquire_read_fence(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<StatusReadFence, GitCommandError> {
        let cwd = tokio::fs::canonicalize(cwd)
            .await
            .unwrap_or_else(|_| cwd.to_path_buf());
        self.inner
            .status_owner
            .acquire_read_fence(&cwd, cancellation)
            .await
    }

    pub(crate) fn publish_if_fence_current<R>(
        &self,
        fence: &StatusReadFence,
        publish: impl FnOnce() -> R,
    ) -> Result<R, GitCommandError> {
        self.inner
            .status_owner
            .publish_if_fence_current(fence, publish)
    }

    pub(crate) async fn invalidate_fetch_after_catalog_mutation(&self, paths: &[PathBuf]) {
        let mut repository_keys = HashSet::new();
        for path in paths {
            let cwd = tokio::fs::canonicalize(path)
                .await
                .unwrap_or_else(|_| path.clone());
            if let Some(repository_key) = self.inner.fetch_owner.repository_key_for_worktree(&cwd) {
                repository_keys.insert(repository_key);
                continue;
            }
            if let Ok(repository_key) = self
                .inner
                .repository
                .resolve_common_dir(&cwd, &CancellationToken::new())
                .await
            {
                repository_keys.insert(repository_key);
            }
        }
        self.inner
            .fetch_owner
            .invalidate_after_catalog_mutation(&repository_keys.into_iter().collect::<Vec<_>>());
    }

    #[cfg(test)]
    pub(crate) async fn full_read_lease_count_for_test(&self, cwd: &Path) -> usize {
        let canonical_cwd = tokio::fs::canonicalize(cwd)
            .await
            .unwrap_or_else(|_| cwd.to_path_buf());
        self.inner
            .status_owner
            .lease_count_for_test(&StatusReadKey {
                canonical_cwd,
                output_kind: StatusOutputKind::Full,
            })
    }

    #[cfg(test)]
    pub(crate) fn install_full_read_execution_gate_for_test(
        &self,
    ) -> Arc<super::status_owner::StatusReadExecutionGate> {
        self.inner
            .status_owner
            .install_read_execution_gate_for_test(StatusOutputKind::Full)
    }

    #[cfg(test)]
    pub(crate) async fn local_refresh_generation_for_test(&self, cwd: &Path) -> u64 {
        let canonical_cwd = tokio::fs::canonicalize(cwd)
            .await
            .unwrap_or_else(|_| cwd.to_path_buf());
        self.inner
            .status_owner
            .local_refresh_generation_for_test(&canonical_cwd)
    }

    #[cfg(test)]
    pub(crate) async fn wait_for_local_refresh_generation_after_for_test(
        &self,
        cwd: &Path,
        baseline: u64,
    ) {
        let canonical_cwd = tokio::fs::canonicalize(cwd)
            .await
            .unwrap_or_else(|_| cwd.to_path_buf());
        self.inner
            .status_owner
            .wait_for_local_refresh_generation_after_for_test(&canonical_cwd, baseline)
            .await;
    }

    #[cfg(test)]
    pub(crate) async fn physical_local_read_count_for_test(&self, cwd: &Path) -> usize {
        let canonical_cwd = tokio::fs::canonicalize(cwd)
            .await
            .unwrap_or_else(|_| cwd.to_path_buf());
        self.inner
            .status_owner
            .physical_read_count_for_test(&StatusReadKey {
                canonical_cwd,
                output_kind: StatusOutputKind::Local,
            })
    }

    #[cfg(test)]
    pub(crate) async fn wait_for_physical_local_read_after_for_test(
        &self,
        cwd: &Path,
        baseline: usize,
    ) {
        let canonical_cwd = tokio::fs::canonicalize(cwd)
            .await
            .unwrap_or_else(|_| cwd.to_path_buf());
        self.inner
            .status_owner
            .wait_for_physical_read_after_for_test(
                &StatusReadKey {
                    canonical_cwd,
                    output_kind: StatusOutputKind::Local,
                },
                baseline,
            )
            .await;
    }

    #[cfg(test)]
    pub(crate) async fn shut_down_watcher_for_test(&self) {
        self.inner.watcher.shutdown().await;
    }

    #[cfg(test)]
    async fn refresh_remote(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<(), GitCommandError> {
        let cwd = tokio::fs::canonicalize(cwd)
            .await
            .unwrap_or_else(|_| cwd.to_path_buf());
        let lifecycle_id = self
            .lock_state()
            .repositories
            .get(&cwd)
            .map(|entry| entry.lifecycle_id);
        self.refresh_remote_for_lifecycle(&cwd, lifecycle_id, cancellation)
            .await
    }

    #[cfg(test)]
    pub(crate) async fn publish_status_event_for_test(
        &self,
        cwd: &Path,
        event: VcsStatusStreamEvent,
    ) {
        let cwd = tokio::fs::canonicalize(cwd)
            .await
            .unwrap_or_else(|_| cwd.to_path_buf());
        let fence = self
            .inner
            .status_owner
            .acquire_read_fence(&cwd, &CancellationToken::new())
            .await
            .expect("test status fence");
        let mut state = self.lock_state();
        let entry = state
            .repositories
            .get_mut(&cwd)
            .expect("test status subscription exists");
        publish(entry, event, &fence);
    }

    async fn refresh_git_manager_signal_for_lifecycle(
        &self,
        cwd: &Path,
        lifecycle_id: Option<u64>,
        cancellation: &CancellationToken,
    ) -> Result<(), GitCommandError> {
        let (read_lock, common_dir) = {
            let state = self.lock_state();
            let Some(entry) = state
                .repositories
                .get(cwd)
                .filter(|entry| Some(entry.lifecycle_id) == lifecycle_id)
            else {
                return Ok(());
            };
            (
                Arc::clone(&entry.git_manager_read_lock),
                entry.git_manager_common_dir.clone(),
            )
        };
        let _read = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Ok(()),
            guard = read_lock.lock() => guard,
        };
        let common_dir = match common_dir {
            Some(common_dir) => common_dir,
            None => {
                let common_dir = self
                    .inner
                    .repository
                    .resolve_common_dir(cwd, cancellation)
                    .await?;
                if let Some(entry) = self
                    .lock_state()
                    .repositories
                    .get_mut(cwd)
                    .filter(|entry| Some(entry.lifecycle_id) == lifecycle_id)
                {
                    entry.git_manager_common_dir = Some(common_dir.clone());
                }
                common_dir
            }
        };
        loop {
            let fence = self
                .inner
                .status_owner
                .acquire_read_fence(cwd, cancellation)
                .await?;
            let (refs, head, stash_reflog) = tokio::try_join!(
                self.inner
                    .repository
                    .git_manager_signal_refs(cwd, cancellation),
                self.inner
                    .repository
                    .git_manager_signal_head(cwd, cancellation),
                self.inner.repository.git_manager_signal_stash_reflog(
                    cwd,
                    &common_dir,
                    cancellation
                ),
            )?;
            let signature = hash_git_manager_signature(&refs.stdout, &head, &stash_reflog);
            let publication = self
                .inner
                .status_owner
                .publish_if_fence_current(&fence, || {
                    let mut state = self.lock_state();
                    if let Some(entry) = state.repositories.get_mut(cwd).filter(|entry| {
                        Some(entry.lifecycle_id) == lifecycle_id && !cancellation.is_cancelled()
                    }) {
                        update_git_manager_signature(
                            &mut entry.git_manager_signature,
                            &entry.git_manager_signal,
                            signature,
                        );
                    }
                });
            if publication.is_ok()
                || cancellation.is_cancelled()
                || !self
                    .lock_state()
                    .repositories
                    .get(cwd)
                    .is_some_and(|entry| Some(entry.lifecycle_id) == lifecycle_id)
            {
                return publication;
            }
            // The mutation epoch changed during this read. Re-admission waits for
            // settlement and retains this invalidation without another watcher event.
        }
    }

    fn publish_watcher_health(&self, cwd: &Path, lifecycle_id: u64, health: GitWatcherHealth) {
        let state = self.lock_state();
        if let Some(entry) = state.repositories.get(cwd).filter(|entry| {
            entry.lifecycle_id == lifecycle_id && !entry.poller_cancellation.is_cancelled()
        }) {
            entry.git_manager_signal.send_if_modified(|signal| {
                let degraded = health == GitWatcherHealth::FallbackRequired;
                if signal.watcher_degraded == degraded {
                    return false;
                }
                signal.watcher_degraded = degraded;
                true
            });
        }
    }

    async fn refresh_remote_for_lifecycle(
        &self,
        cwd: &Path,
        lifecycle_id: Option<u64>,
        cancellation: &CancellationToken,
    ) -> Result<(), GitCommandError> {
        let fence = self
            .inner
            .status_owner
            .acquire_read_fence(cwd, cancellation)
            .await?;
        let observed = self
            .inner
            .repository
            .observed_remote_status(cwd, cancellation)
            .await?;
        let (retirement, request_local_refresh) = self
            .inner
            .status_owner
            .publish_if_fence_current(&fence, || {
                let mut state = self.lock_state();
                let entry = state
                    .repositories
                    .get_mut(cwd)
                    .filter(|entry| Some(entry.lifecycle_id) == lifecycle_id)?;
                if entry.local.ref_name != observed.ref_name {
                    let request_local_refresh = !entry.pending_local_reconcile;
                    entry.pending_local_reconcile = true;
                    return Some((None, request_local_refresh));
                }
                let remote = observed.remote;
                let changed = entry.remote.as_ref() != Some(&remote);
                entry.remote = Some(remote.clone());
                entry.remote_fence = Some(fence.clone());
                entry.remote_ref_name = Some(observed.ref_name);
                if changed {
                    publish(
                        entry,
                        VcsStatusStreamEvent::RemoteUpdated { remote },
                        &fence,
                    );
                }
                if entry.subscribers.is_empty() {
                    Some((
                        state
                            .repositories
                            .remove(cwd)
                            .map(|entry| Self::retire_repository(&mut state, cwd, entry)),
                        false,
                    ))
                } else {
                    Some((None, false))
                }
            })?
            .unwrap_or((None, false));
        if request_local_refresh {
            self.inner.status_owner.request_local_refresh(cwd);
        }
        self.finish_repository_retirement(cwd, retirement);
        // Reconciliation must settle before independent signature I/O can yield.
        // Otherwise a successful local read can consume the pending mismatch,
        // then the delayed remote result reopens it and requests a duplicate read.
        if let Err(error) = self
            .refresh_git_manager_signal_for_lifecycle(cwd, lifecycle_id, cancellation)
            .await
            && !cancellation.is_cancelled()
        {
            log_signature_failure(&error);
        }
        Ok(())
    }

    pub(crate) async fn refresh_status(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<StatusPublication<VcsStatusResult>, GitCommandError> {
        let cwd = tokio::fs::canonicalize(cwd)
            .await
            .unwrap_or_else(|_| cwd.to_path_buf());
        let repository = Arc::clone(&self.inner.repository);
        let load_cwd = cwd.clone();
        let read = self
            .inner
            .status_owner
            .read_full(
                StatusReadKey {
                    canonical_cwd: cwd.clone(),
                    output_kind: StatusOutputKind::Full,
                },
                cancellation,
                move |shared_cancellation| async move {
                    repository.status(&load_cwd, &shared_cancellation).await
                },
            )
            .await?;
        let fence = read.fence();
        let (status, retirement) = self
            .inner
            .status_owner
            .publish_if_current(read, |status| self.publish_status(&cwd, status, &fence))?;
        self.finish_repository_retirement(&cwd, retirement);
        Ok(StatusPublication {
            local: status.local.clone(),
            value: status,
            fence,
        })
    }

    fn publish_status(
        &self,
        cwd: &Path,
        status: &VcsStatusResult,
        fence: &StatusReadFence,
    ) -> Option<RepositoryRetirementFence> {
        self.inner
            .fetch_owner
            .update_worktree_ref(cwd, status.local.ref_name.clone());
        let remote = status.local.is_repo.then(|| status.remote.clone());
        let event = VcsStatusStreamEvent::Snapshot {
            local: status.local.clone(),
            remote: remote.clone(),
        };
        let mut state = self.lock_state();
        let mut remove_repository = false;
        let mut previous_local = None;
        let mut lifecycle_id = None;
        if let Some(entry) = state.repositories.get_mut(cwd) {
            lifecycle_id = Some(entry.lifecycle_id);
            let changed = entry.local != status.local || entry.remote.as_ref() != Some(&remote);
            previous_local = Some(std::mem::replace(&mut entry.local, status.local.clone()));
            entry.remote = Some(remote);
            entry.remote_fence = Some(fence.clone());
            entry.remote_ref_name = Some(status.local.ref_name.clone());
            if changed {
                publish(entry, event, fence);
            }
            remove_repository = entry.subscribers.is_empty();
        }
        let retirement = if remove_repository {
            state
                .repositories
                .remove(cwd)
                .map(|entry| Self::retire_repository(&mut state, cwd, entry))
        } else {
            None
        };
        drop(state);
        self.observe_repository_availability_change(
            cwd,
            lifecycle_id,
            previous_local.as_ref(),
            &status.local,
        );
        retirement
    }

    #[must_use]
    /// Returns the number of worktree polling entries. Physical-repository
    /// automatic-fetch ownership is tracked separately.
    pub fn active_poller_count(&self) -> usize {
        self.lock_state().repositories.len()
    }

    #[cfg(test)]
    fn active_status_subscriber_count_for_test(&self) -> usize {
        self.lock_state()
            .repositories
            .values()
            .flat_map(|entry| entry.subscribers.values())
            .filter(|subscriber| matches!(subscriber, RepositorySubscriber::Status { .. }))
            .count()
    }

    #[cfg(test)]
    fn active_watcher_count_for_test(&self) -> usize {
        self.inner.watcher.active_count_for_test()
    }

    #[cfg(test)]
    async fn wait_for_status_scheduler_for_test(&self) {
        loop {
            let finished = self.inner.status_scheduler_finished.notified();
            if self.active_watcher_count_for_test() == 0 {
                return;
            }
            test_timeout("status scheduler shutdown", finished).await;
        }
    }

    #[cfg(test)]
    async fn wait_for_status_scheduler_started_for_test(&self) {
        loop {
            let started = self.inner.status_scheduler_started.notified();
            if self
                .inner
                .active_status_schedulers
                .load(std::sync::atomic::Ordering::SeqCst)
                != 0
            {
                return;
            }
            test_timeout("status scheduler startup", started).await;
        }
    }

    #[cfg(test)]
    async fn wait_for_local_read_leases_for_test(&self, cwd: &Path, expected: usize) {
        self.inner
            .status_owner
            .wait_for_lease_count_for_test(
                &StatusReadKey {
                    canonical_cwd: cwd.to_path_buf(),
                    output_kind: StatusOutputKind::Local,
                },
                expected,
            )
            .await;
    }

    #[cfg(test)]
    fn retirement_wait_count_for_test(&self) -> usize {
        self.inner.retirement_wait_starts.load(Ordering::Acquire)
    }

    #[cfg(test)]
    async fn wait_for_retirement_wait_started_for_test(&self, baseline: usize) {
        loop {
            let started = self.inner.retirement_wait_started.notified();
            if self.retirement_wait_count_for_test() > baseline {
                return;
            }
            test_timeout("retirement wait startup", started).await;
        }
    }

    #[cfg(test)]
    async fn wait_for_subscription_setup_wait_started_for_test(&self) {
        test_timeout(
            "shutdown reaching subscription setup settlement",
            self.inner.subscription_setup_wait_started.notified(),
        )
        .await;
    }

    #[cfg(test)]
    fn install_retirement_epoch_gate_for_test(&self) -> Arc<RetirementEpochGate> {
        let gate = Arc::new(RetirementEpochGate::default());
        *self
            .inner
            .retirement_epoch_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&gate));
        gate
    }

    #[cfg(test)]
    fn install_subscription_registration_gate_for_test(&self) -> Arc<SubscriptionRegistrationGate> {
        let gate = Arc::new(SubscriptionRegistrationGate::default());
        *self
            .inner
            .subscription_registration_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&gate));
        gate
    }

    #[cfg(test)]
    fn install_lifecycle_insertion_gate_for_test(&self) -> Arc<LifecycleInsertionGate> {
        let gate = Arc::new(LifecycleInsertionGate::default());
        *self
            .inner
            .lifecycle_insertion_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&gate));
        gate
    }

    #[cfg(test)]
    fn install_post_retirement_wait_gate_for_test(&self) -> Arc<SubscribeAttemptGate> {
        let gate = Arc::new(SubscribeAttemptGate::default());
        *self
            .inner
            .post_retirement_wait_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&gate));
        gate
    }

    #[cfg(test)]
    fn install_post_initial_read_gate_for_test(&self) -> Arc<SubscribeAttemptGate> {
        let gate = Arc::new(SubscribeAttemptGate::default());
        *self
            .inner
            .post_initial_read_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&gate));
        gate
    }

    #[cfg(test)]
    fn install_registration_outcome_probe_for_test(&self) -> Arc<RegistrationOutcomeProbe> {
        let probe = Arc::new(RegistrationOutcomeProbe::default());
        *self
            .inner
            .registration_outcome_probe
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::clone(&probe));
        probe
    }

    #[cfg(test)]
    fn hold_lifecycle_task_until_released_for_test(
        &self,
        cwd: &Path,
        release: Arc<tokio::sync::Semaphore>,
        cancellation_observed: tokio::sync::mpsc::UnboundedSender<()>,
    ) {
        let state = self.lock_state();
        let entry = state
            .repositories
            .get(cwd)
            .expect("active lifecycle exists");
        let cancellation = entry.poller_cancellation.clone();
        let tasks = entry.tasks.clone();
        drop(state);
        tasks.spawn(async move {
            test_timeout("held lifecycle task cancellation", cancellation.cancelled()).await;
            let _ = cancellation_observed.send(());
            test_timeout("held lifecycle task release", release.acquire())
                .await
                .expect("lifecycle-task release remains open")
                .forget();
        });
    }

    #[cfg(test)]
    fn retiring_tracker_is_nonempty_for_test(&self, cwd: &Path) -> bool {
        self.lock_state()
            .retiring
            .get(cwd)
            .is_some_and(|trackers| trackers.iter().any(|tracker| !tracker.is_empty()))
    }

    #[cfg(test)]
    async fn hold_active_watcher_until_released_for_test(
        &self,
        cwd: &Path,
        request: GitWatchRequest,
        release: Arc<tokio::sync::Semaphore>,
        cancellation_observed: tokio::sync::mpsc::UnboundedSender<()>,
    ) {
        let subscription = test_timeout(
            "test watcher hold subscription",
            self.inner.watcher.subscribe(request),
        )
        .await
        .expect("test watcher hold subscribes");
        let cancellation = self
            .lock_state()
            .repositories
            .get(cwd)
            .expect("active lifecycle exists")
            .poller_cancellation
            .clone();
        let tasks = self
            .lock_state()
            .repositories
            .get(cwd)
            .expect("active lifecycle exists")
            .tasks
            .clone();
        tasks.spawn(async move {
            test_timeout("held watcher cancellation", cancellation.cancelled()).await;
            let _ = cancellation_observed.send(());
            test_timeout("held watcher release", release.acquire())
                .await
                .expect("watcher-hold release remains open")
                .forget();
            drop(subscription);
        });
    }

    pub(crate) async fn shutdown(&self) {
        let (repositories, retiring) = {
            let mut state = self.lock_state();
            state.closed = true;
            self.inner.subscription_cancellation.cancel();
            self.inner.subscription_setups.close();
            (
                std::mem::take(&mut state.repositories),
                std::mem::take(&mut state.retiring),
            )
        };
        let mut tasks = Vec::new();
        for (cwd, entry) in repositories {
            entry.poller_cancellation.cancel();
            self.inner.status_owner.cancel_reads(&cwd);
            entry.retirement_cancellation.cancel();
            entry.tasks.close();
            for subscriber_id in entry.subscribers.keys().copied() {
                self.inner.fetch_owner.detach(&cwd, subscriber_id);
            }
            tasks.push(entry.tasks);
        }
        for retired in retiring.into_values() {
            tasks.extend(retired);
        }
        self.inner.fetch_owner.shutdown().await;
        self.inner.watcher.shutdown().await;
        #[cfg(test)]
        self.inner.subscription_setup_wait_started.notify_one();
        self.inner.subscription_setups.wait().await;
        self.inner.status_owner.shutdown().await;
        for tasks in tasks {
            tasks.wait().await;
        }
    }

    fn spawn_local_status_poller(
        &self,
        cwd: PathBuf,
        lifecycle_id: u64,
        cancellation: CancellationToken,
        local_refresh_requests: watch::Receiver<u64>,
        watcher_startup: (WatcherSignalSource, Duration),
        tasks: &TaskTracker,
    ) {
        let (watcher, initial_read_duration) = watcher_startup;
        let broadcaster = self.clone();
        #[cfg(test)]
        let scheduler_finished = Arc::clone(&self.inner.status_scheduler_finished);
        #[cfg(test)]
        let scheduler_started = Arc::clone(&self.inner.status_scheduler_started);
        #[cfg(test)]
        let active_schedulers = Arc::clone(&self.inner.active_status_schedulers);
        tasks.spawn(async move {
            #[cfg(test)]
            let _active = ActiveStatusSchedulerForTest::new(
                active_schedulers,
                scheduler_started,
                scheduler_finished,
            );
            let refresh_cancellation = cancellation.clone();
            let safety_interval = broadcaster.inner.local_status_refresh_interval;
            let _ = run_status_signal_scheduler(
                watcher,
                local_refresh_requests,
                cancellation,
                initial_read_duration,
                safety_interval,
                StatusRefreshTarget {
                    broadcaster,
                    cwd,
                    lifecycle_id,
                    cancellation: refresh_cancellation,
                },
            )
            .await;
        });
    }

    fn spawn_remote_reconciliation(
        &self,
        cwd: PathBuf,
        lifecycle_id: u64,
        cancellation: CancellationToken,
        mut remote_refresh_requests: watch::Receiver<u64>,
        tasks: &TaskTracker,
    ) {
        let broadcaster = self.clone();
        tasks.spawn(async move {
            let _ = broadcaster
                .refresh_remote_for_lifecycle(&cwd, Some(lifecycle_id), &cancellation)
                .await;
            loop {
                tokio::select! {
                    _ = cancellation.cancelled() => break,
                    changed = remote_refresh_requests.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        remote_refresh_requests.borrow_and_update();
                        let _ = broadcaster
                            .refresh_remote_for_lifecycle(&cwd, Some(lifecycle_id), &cancellation)
                            .await;
                    }
                }
            }
        });
    }

    fn spawn_fetch_attachment(
        &self,
        cwd: PathBuf,
        lifecycle_id: u64,
        resolved_common_dir: Option<PathBuf>,
    ) {
        let mut state = self.lock_state();
        if state.closed {
            return;
        }
        let Some(entry) = state.repositories.get_mut(&cwd).filter(|entry| {
            entry.lifecycle_id == lifecycle_id && !entry.poller_cancellation.is_cancelled()
        }) else {
            return;
        };
        if entry.repository_key.is_some() {
            return;
        }
        if entry.fetch_attachment_in_flight {
            entry.fetch_attachment_requested = true;
            return;
        }
        entry.fetch_attachment_in_flight = true;
        entry.fetch_attachment_requested = false;
        let cancellation = entry.poller_cancellation.clone();
        let tasks = entry.tasks.clone();
        let broadcaster = self.clone();
        #[cfg(test)]
        let attachment_finished = Arc::clone(&self.inner.fetch_attachment_finished);
        tasks.spawn(async move {
            #[cfg(test)]
            let _finished = NotifyOnDrop(attachment_finished);
            let mut resolved_common_dir = resolved_common_dir;
            loop {
                let retired_read = broadcaster
                    .resolve_fetch_attachment(
                        &cwd,
                        lifecycle_id,
                        &cancellation,
                        resolved_common_dir.take(),
                    )
                    .await;
                let retry = {
                    let mut state = broadcaster.lock_state();
                    let Some(entry) = state
                        .repositories
                        .get_mut(&cwd)
                        .filter(|entry| entry.lifecycle_id == lifecycle_id)
                    else {
                        break;
                    };
                    // Reservation release and a repair's trailing request share this lock,
                    // so a repair arriving as resolution finishes cannot be lost.
                    let retry = (retired_read || entry.fetch_attachment_requested)
                        && entry.local.is_repo
                        && entry.repository_key.is_none()
                        && !cancellation.is_cancelled();
                    entry.fetch_attachment_requested = false;
                    if !retry {
                        entry.fetch_attachment_in_flight = false;
                    }
                    retry
                };
                if !retry {
                    break;
                }
            }
        });
    }

    async fn resolve_fetch_attachment(
        &self,
        cwd: &Path,
        lifecycle_id: u64,
        cancellation: &CancellationToken,
        resolved_common_dir: Option<PathBuf>,
    ) -> bool {
        let Ok(fence) = self
            .inner
            .status_owner
            .acquire_read_fence(cwd, cancellation)
            .await
        else {
            return false;
        };
        let repository_key = match resolved_common_dir {
            Some(repository_key) => Some(repository_key),
            None => tokio::select! {
                biased;
                () = cancellation.cancelled() => return false,
                result = self.inner.repository.resolve_common_dir(cwd, cancellation) => result.ok(),
            },
        };
        self.inner
            .status_owner
            .publish_if_fence_current(&fence, || {
                let mut state = self.lock_state();
                let Some(entry) = state.repositories.get_mut(cwd).filter(|entry| {
                    entry.lifecycle_id == lifecycle_id && !cancellation.is_cancelled()
                }) else {
                    return false;
                };
                if entry.fetch_attachment_requested && entry.local.is_repo {
                    return true;
                }
                if entry.local.is_repo
                    && let Some(repository_key) = repository_key
                {
                    entry.repository_key = Some(repository_key.clone());
                    for subscriber_id in entry.subscribers.keys().copied() {
                        self.inner.fetch_owner.attach(
                            repository_key.clone(),
                            cwd.to_path_buf(),
                            subscriber_id,
                            entry.local.ref_name.clone(),
                            entry.remote_refresh_requests.clone(),
                        );
                    }
                }
                false
            })
            // Re-admission waits for a mutation's settlement. Only the same active,
            // available lifecycle may resolve again; this adds no timer or status polling.
            .unwrap_or(true)
    }

    fn release(&self, cwd: &Path, subscriber_id: u64) {
        let retirement = {
            let mut state = self.lock_state();
            let should_remove = if let Some(entry) = state.repositories.get_mut(cwd) {
                entry.subscribers.remove(&subscriber_id);
                entry.subscribers.is_empty()
            } else {
                false
            };
            if should_remove && let Some(entry) = state.repositories.remove(cwd) {
                Some(Self::retire_repository(&mut state, cwd, entry))
            } else {
                None
            }
        };
        self.finish_repository_retirement(cwd, retirement);
        self.inner.fetch_owner.detach(cwd, subscriber_id);
    }

    fn retire_repository(
        state: &mut State,
        cwd: &Path,
        entry: RepositoryState,
    ) -> RepositoryRetirementFence {
        entry.poller_cancellation.cancel();
        entry.tasks.close();
        state
            .retiring
            .entry(cwd.to_path_buf())
            .or_default()
            .push(entry.tasks);
        RepositoryRetirementFence(entry.retirement_cancellation)
    }

    fn finish_repository_retirement(
        &self,
        cwd: &Path,
        retirement: Option<RepositoryRetirementFence>,
    ) {
        if let Some(_retirement) = retirement {
            #[cfg(test)]
            if let Some(gate) = self
                .inner
                .retirement_epoch_gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone()
            {
                gate.block();
            }
            self.inner.status_owner.cancel_reads(cwd);
        }
    }

    async fn await_retired_lifecycle(&self, cwd: &Path) {
        loop {
            let tasks = self
                .lock_state()
                .retiring
                .get(cwd)
                .cloned()
                .unwrap_or_default();
            if tasks.is_empty() {
                return;
            }
            #[cfg(test)]
            {
                self.inner
                    .retirement_wait_starts
                    .fetch_add(1, Ordering::Release);
                self.inner.retirement_wait_started.notify_waiters();
            }
            for tasks in tasks {
                tasks.wait().await;
            }
            let mut state = self.lock_state();
            let remove = if let Some(tasks) = state.retiring.get_mut(cwd) {
                tasks.retain(|tasks| !tasks.is_empty());
                tasks.is_empty()
            } else {
                false
            };
            if remove {
                state.retiring.remove(cwd);
            }
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
struct NotifyOnDrop(Arc<tokio::sync::Notify>);

#[cfg(test)]
impl Drop for NotifyOnDrop {
    fn drop(&mut self) {
        self.0.notify_waiters();
    }
}

#[cfg(test)]
struct ActiveStatusSchedulerForTest {
    active: Arc<std::sync::atomic::AtomicUsize>,
    finished: Arc<tokio::sync::Notify>,
}

#[cfg(test)]
#[derive(Default)]
struct RetirementEpochGate {
    entered: std::sync::atomic::AtomicBool,
    entered_notify: tokio::sync::Notify,
    released: Mutex<bool>,
    release_notify: std::sync::Condvar,
}

#[cfg(test)]
struct SubscriptionRegistrationGate {
    has_entered: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
    release: tokio::sync::Semaphore,
    lifecycle_tasks: Mutex<Option<TaskTracker>>,
}

#[cfg(test)]
#[derive(Default)]
struct LifecycleInsertionGate(SubscriptionRegistrationGate);

#[cfg(test)]
struct SubscribeAttemptGate {
    entered: std::sync::atomic::AtomicBool,
    entered_notify: tokio::sync::Notify,
    release: tokio::sync::Semaphore,
}

#[cfg(test)]
impl Default for SubscribeAttemptGate {
    fn default() -> Self {
        Self {
            entered: std::sync::atomic::AtomicBool::new(false),
            entered_notify: tokio::sync::Notify::new(),
            release: tokio::sync::Semaphore::new(0),
        }
    }
}

#[cfg(test)]
impl SubscribeAttemptGate {
    async fn block(&self) {
        self.entered
            .store(true, std::sync::atomic::Ordering::Release);
        self.entered_notify.notify_waiters();
        self.release
            .acquire()
            .await
            .expect("subscribe-attempt gate remains open")
            .forget();
    }

    async fn wait_until_entered(&self) {
        loop {
            let entered = self.entered_notify.notified();
            if self.entered.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            test_timeout("subscribe-attempt gate entry", entered).await;
        }
    }

    fn release(&self) {
        self.release.add_permits(1);
    }
}

#[cfg(test)]
#[derive(Default)]
struct RegistrationOutcomeProbe {
    reported: std::sync::atomic::AtomicBool,
    retried: std::sync::atomic::AtomicBool,
    notify: tokio::sync::Notify,
}

#[cfg(test)]
impl RegistrationOutcomeProbe {
    fn report(&self, retried: bool) {
        self.retried
            .store(retried, std::sync::atomic::Ordering::Release);
        self.reported
            .store(true, std::sync::atomic::Ordering::Release);
        self.notify.notify_waiters();
    }

    async fn wait(&self) -> bool {
        loop {
            let reported = self.notify.notified();
            if self.reported.load(std::sync::atomic::Ordering::Acquire) {
                return self.retried.load(std::sync::atomic::Ordering::Acquire);
            }
            test_timeout("subscription registration outcome", reported).await;
        }
    }
}

#[cfg(test)]
impl LifecycleInsertionGate {
    async fn block(&self, lifecycle_tasks: &TaskTracker) {
        self.0.block(lifecycle_tasks).await;
    }

    async fn wait_until_entered(&self) {
        self.0.wait_until_entered().await;
    }

    fn lifecycle_tracker_is_nonempty(&self) -> bool {
        self.0
            .lifecycle_tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .is_some_and(|tasks| !tasks.is_empty())
    }

    fn release(&self) {
        self.0.release();
    }
}

#[cfg(test)]
impl Default for SubscriptionRegistrationGate {
    fn default() -> Self {
        Self {
            has_entered: std::sync::atomic::AtomicBool::new(false),
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Semaphore::new(0),
            lifecycle_tasks: Mutex::new(None),
        }
    }
}

#[cfg(test)]
impl SubscriptionRegistrationGate {
    async fn block(&self, lifecycle_tasks: &TaskTracker) {
        *self
            .lifecycle_tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(lifecycle_tasks.clone());
        self.has_entered
            .store(true, std::sync::atomic::Ordering::Release);
        self.entered.notify_waiters();
        self.release
            .acquire()
            .await
            .expect("subscription-registration gate remains open")
            .forget();
    }

    async fn wait_until_entered(&self) {
        loop {
            let entered = self.entered.notified();
            if self.has_entered.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            test_timeout("subscription-registration gate entry", entered).await;
        }
    }

    fn release(&self) {
        self.release.add_permits(1);
    }

    fn lifecycle_tracker_is_closed(&self) -> bool {
        self.lifecycle_tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .is_some_and(TaskTracker::is_closed)
    }

    fn close_lifecycle_tracker(&self) {
        if let Some(tasks) = self
            .lifecycle_tasks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            tasks.close();
        }
    }
}

#[cfg(test)]
impl RetirementEpochGate {
    fn block(&self) {
        self.entered
            .store(true, std::sync::atomic::Ordering::Release);
        self.entered_notify.notify_waiters();
        let released = self.released.lock().expect("epoch-gate release lock");
        let (released, _) = self
            .release_notify
            .wait_timeout_while(released, Duration::from_secs(10), |released| !*released)
            .expect("epoch-gate release wait");
        assert!(*released, "timed out waiting for epoch-gate release");
    }

    async fn wait_until_entered(&self) {
        loop {
            let entered = self.entered_notify.notified();
            if self.entered.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }
            test_timeout("retirement-epoch gate entry", entered).await;
        }
    }

    fn release(&self) {
        *self.released.lock().expect("epoch-gate release lock") = true;
        self.release_notify.notify_all();
    }
}

#[cfg(test)]
impl ActiveStatusSchedulerForTest {
    fn new(
        active: Arc<std::sync::atomic::AtomicUsize>,
        started: Arc<tokio::sync::Notify>,
        finished: Arc<tokio::sync::Notify>,
    ) -> Self {
        active.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        started.notify_waiters();
        Self { active, finished }
    }
}

#[cfg(test)]
impl Drop for ActiveStatusSchedulerForTest {
    fn drop(&mut self) {
        self.active
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        self.finished.notify_waiters();
    }
}

impl StatusSubscription {
    pub async fn recv(&mut self) -> Option<VcsStatusStreamEvent> {
        self.recv_publication()
            .await
            .and_then(Result::ok)
            .map(|publication| publication.value)
    }

    pub(crate) async fn recv_publication(
        &mut self,
    ) -> Option<Result<StatusPublication<VcsStatusStreamEvent>, GitCommandError>> {
        tokio::select! {
            biased;
            _ = self.cancellation.cancelled() => None,
            event = self.receiver.recv() => match event {
                Some(publication) => Some(Ok(publication)),
                None if self.fell_behind.swap(false, Ordering::Acquire) => {
                    Some(Err(fell_behind_error(&self.cwd)))
                }
                None => None,
            },
        }
    }
}

impl Drop for StatusSubscription {
    fn drop(&mut self) {
        self.broadcaster.release(&self.cwd, self.subscriber_id);
    }
}

impl GitManagerSignalSubscription {
    pub async fn recv(&mut self) -> Option<GitManagerSignal> {
        if self.cancellation.is_cancelled() {
            return None;
        }
        if self.pending_initial {
            self.pending_initial = false;
            return Some(*self.receiver.borrow_and_update());
        }
        tokio::select! {
            biased;
            _ = self.cancellation.cancelled() => None,
            changed = self.receiver.changed() => {
                changed.ok().map(|()| *self.receiver.borrow_and_update())
            }
        }
    }
}

impl Drop for GitManagerSignalSubscription {
    fn drop(&mut self) {
        self.broadcaster.release(&self.cwd, self.subscriber_id);
    }
}

fn hash_git_manager_signature(refs: &str, head: &GitManagerSignalHead, stash_reflog: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    refs.hash(&mut hasher);
    head.hash(&mut hasher);
    stash_reflog.hash(&mut hasher);
    hasher.finish()
}

fn update_git_manager_signature(
    current: &mut Option<u64>,
    signal: &watch::Sender<GitManagerSignal>,
    observed: u64,
) {
    if *current == Some(observed) {
        return;
    }
    *current = Some(observed);
    signal.send_modify(|value| value.generation = value.generation.saturating_add(1));
}

fn clear_remote_for_ref_change(entry: &mut RepositoryState) {
    entry.remote = None;
    entry.remote_fence = None;
    entry.remote_ref_name = None;
    entry.pending_local_reconcile = false;
    entry
        .remote_refresh_requests
        .send_modify(|generation| *generation = generation.wrapping_add(1));
}

fn reconcile_remote_after_local_publication(entry: &mut RepositoryState, ref_changed: bool) {
    if ref_changed {
        clear_remote_for_ref_change(entry);
    } else if std::mem::take(&mut entry.pending_local_reconcile) {
        entry
            .remote_refresh_requests
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }
}

fn broadcaster_shutdown_error(cwd: &Path) -> GitCommandError {
    GitCommandError {
        tag: "GitCommandError",
        operation: "GitStatusBroadcaster.subscribe".into(),
        command: "git".into(),
        cwd: cwd.to_string_lossy().into_owned().into_boxed_str(),
        diagnostics: None,
        detail: "Git status broadcaster is shut down.".into(),
    }
}

/// The client runtime matches this exact value and resubscribes.
pub(crate) const GIT_STATUS_FELL_BEHIND_OPERATION: &str = "GitStatusBroadcaster.fellBehind";

fn fell_behind_error(cwd: &Path) -> GitCommandError {
    GitCommandError {
        tag: "GitCommandError",
        operation: GIT_STATUS_FELL_BEHIND_OPERATION.into(),
        command: "git".into(),
        cwd: cwd.to_string_lossy().into_owned().into_boxed_str(),
        diagnostics: None,
        detail: "The Git status stream fell behind. Subscribe again.".into(),
    }
}

fn setup_admission_error(cwd: &Path) -> GitCommandError {
    GitCommandError {
        tag: "GitCommandError",
        operation: GIT_STATUS_FELL_BEHIND_OPERATION.into(),
        command: "git".into(),
        cwd: cwd.to_string_lossy().into_owned().into_boxed_str(),
        diagnostics: None,
        detail:
            "The Git status subscription fell behind while it was being set up. Subscribe again."
                .into(),
    }
}

fn publish(entry: &mut RepositoryState, event: VcsStatusStreamEvent, fence: &StatusReadFence) {
    let publication = StatusPublication {
        value: event,
        local: entry.local.clone(),
        fence: fence.clone(),
    };
    entry.subscribers.retain(|_, subscriber| match subscriber {
        RepositorySubscriber::Status {
            sender,
            fell_behind,
        } => {
            match sender.try_send(publication.clone()) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    // Publish the terminal before dropping the sender closes the bounded queue.
                    fell_behind.store(true, Ordering::Release);
                    false
                }
                Err(mpsc::error::TrySendError::Closed(_)) => false,
            }
        }
        RepositorySubscriber::GitManager => true,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{
        BoxGitProcessFuture, GitProcessRunner, ProcessError, ProcessOutput, ProcessRequest,
        ProcessRunner, VcsRepositoryUnavailableReason,
    };
    use crate::test_support::TestSandbox;
    use std::{
        collections::BTreeMap,
        ffi::OsString,
        fs,
        process::Command,
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use tokio::sync::{Notify, Semaphore, mpsc};

    #[test]
    fn fell_behind_errors_match_the_shared_client_retry_fixtures() {
        for (kind, error) in [
            ("live", fell_behind_error(Path::new("/repo"))),
            ("setup", setup_admission_error(Path::new("/repo"))),
        ] {
            let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../packages/contracts/fixtures/rpc-wire/contract-shapes")
                .join(format!(
                    "subscribeVcsStatus__fell-behind-{kind}-failure.json"
                ));
            let fixture: serde_json::Value = serde_json::from_str(
                &fs::read_to_string(fixture_path).expect("generated Git status retry fixture"),
            )
            .expect("valid Git status retry fixture");

            assert_eq!(fixture["_tag"], "Exit");
            assert_eq!(fixture["exit"]["_tag"], "Failure");
            assert_eq!(fixture["exit"]["cause"][0]["_tag"], "Fail");
            assert_eq!(
                serde_json::to_value(error).expect("serialize actual server stream failure"),
                fixture["exit"]["cause"][0]["error"],
                "the {kind} server failure must match the fixture driving client resubscription"
            );
        }
    }

    fn signal_head_output(operation: &str) -> Option<ProcessOutput> {
        let stdout = match operation {
            "GitManager.signal.headRef" => "main\n",
            "GitManager.signal.headSha" => "deadbeef\n",
            _ => return None,
        };
        Some(ProcessOutput {
            exit_code: 0,
            stdout: stdout.to_owned(),
            stderr: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
        })
    }

    struct BlockingRemoteGitRunner {
        command: PathBuf,
        environment: Vec<(OsString, OsString)>,
        expected_git_config: PathBuf,
        remote_started: mpsc::UnboundedSender<()>,
        remote_cancelled: mpsc::UnboundedSender<()>,
        remote_outcome: Option<mpsc::UnboundedSender<BlockingRemoteOutcome>>,
        local_status_started: mpsc::UnboundedSender<()>,
        watch_root_calls: AtomicUsize,
        local_status_calls: AtomicUsize,
        release_local_status: Option<Arc<Semaphore>>,
        release_remote: Arc<Semaphore>,
    }

    #[derive(Debug, Eq, PartialEq)]
    enum BlockingRemoteOutcome {
        Cancelled,
        Released,
    }

    /// Reports a blocked remote refresh's terminal branch exactly once, also
    /// when the refresh future is dropped without reaching a branch.
    struct BlockingRemoteOutcomeReport {
        sender: Option<mpsc::UnboundedSender<BlockingRemoteOutcome>>,
    }

    impl BlockingRemoteOutcomeReport {
        fn report(&mut self, outcome: BlockingRemoteOutcome) {
            if let Some(sender) = self.sender.take() {
                let _ = sender.send(outcome);
            }
        }
    }

    impl Drop for BlockingRemoteOutcomeReport {
        fn drop(&mut self) {
            self.report(BlockingRemoteOutcome::Cancelled);
        }
    }

    struct EpochGitRunner {
        branch: Mutex<String>,
        ref_calls: AtomicUsize,
        remote_calls: AtomicUsize,
        ref_started: mpsc::UnboundedSender<()>,
        remote_started: mpsc::UnboundedSender<()>,
        release_ref: Arc<Semaphore>,
        release_remote: Arc<Semaphore>,
    }

    struct RemoteMismatchGitRunner {
        local_calls: AtomicUsize,
        remote_calls: AtomicUsize,
        local_failures: AtomicUsize,
        remote_mismatches: usize,
        operation_changed: Notify,
    }

    struct SubscriptionSetupGitRunner {
        roots: String,
        blocked_operation: &'static str,
        started: mpsc::UnboundedSender<()>,
        release: Arc<Semaphore>,
    }

    struct IdleObservationGitRunner {
        roots: String,
        common_dir: PathBuf,
        operations: Mutex<Vec<String>>,
        operation_changed: Notify,
    }

    impl IdleObservationGitRunner {
        fn operation_count(&self, operation: &str) -> usize {
            self.operations
                .lock()
                .expect("idle observation operations lock")
                .iter()
                .filter(|recorded| recorded.as_str() == operation)
                .count()
        }

        async fn wait_for_operation_count(&self, operation: &str, expected: usize) {
            test_timeout(&format!("{expected} {operation} operations"), async {
                loop {
                    let changed = self.operation_changed.notified();
                    if self.operation_count(operation) >= expected {
                        return;
                    }
                    changed.await;
                }
            })
            .await;
        }
    }

    impl GitProcessRunner for IdleObservationGitRunner {
        fn run<'a>(
            &'a self,
            request: ProcessRequest,
            _cancellation: &'a CancellationToken,
        ) -> BoxGitProcessFuture<'a> {
            self.operations
                .lock()
                .expect("idle observation operations lock")
                .push(request.operation.clone());
            self.operation_changed.notify_waiters();
            Box::pin(async move {
                if let Some(output) = signal_head_output(&request.operation) {
                    return Ok(output);
                }
                let (exit_code, stdout) = match request.operation.as_str() {
                    "GitVcsDriver.detectRepository" => (0, "true\n".to_owned()),
                    "GitVcsDriver.resolveWatchRoots" => (0, self.roots.clone()),
                    "GitVcsDriver.resolveCommonDir" => {
                        (0, format!("{}\n", self.common_dir.display()))
                    }
                    "GitVcsDriver.statusDetailsLocal.status"
                    | "GitVcsDriver.statusDetailsRemote.status" => {
                        (0, "# branch.head main\n".to_owned())
                    }
                    "GitVcsDriver.statusDetailsLocal.remotes" => (0, String::new()),
                    "GitVcsDriver.defaultRef.originHead"
                    | "GitVcsDriver.remoteProvider"
                    | "GitVcsDriver.statusDetailsRemote.defaultDelta"
                    | "GitManager.getRefs.inProgressPaths" => (1, String::new()),
                    "GitVcsDriver.defaultRef.candidate" => (0, String::new()),
                    "GitVcsDriver.currentRef" => (0, "main\n".to_owned()),
                    "GitManager.signal.refs" => {
                        (0, "deadbeef\trefs/heads/main\t/repository\n".to_owned())
                    }
                    operation => panic!("unexpected idle observation Git operation {operation}"),
                };
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

    impl GitProcessRunner for SubscriptionSetupGitRunner {
        fn run<'a>(
            &'a self,
            request: ProcessRequest,
            _cancellation: &'a CancellationToken,
        ) -> BoxGitProcessFuture<'a> {
            Box::pin(async move {
                if let Some(output) = signal_head_output(&request.operation) {
                    return Ok(output);
                }
                if request.operation == self.blocked_operation {
                    let _ = self.started.send(());
                    self.release
                        .acquire()
                        .await
                        .expect("subscription-setup release remains open")
                        .forget();
                }
                let (exit_code, stdout) = match request.operation.as_str() {
                    "GitVcsDriver.detectRepository" => (0, "true\n".to_owned()),
                    "GitVcsDriver.resolveWatchRoots" => (0, self.roots.clone()),
                    "GitVcsDriver.statusDetailsLocal.status" => {
                        (0, "# branch.head main\n".to_owned())
                    }
                    "GitVcsDriver.statusDetailsLocal.stagedNumstat"
                    | "GitVcsDriver.statusDetailsLocal.unstagedNumstat"
                    | "GitVcsDriver.statusDetailsLocal.remotes" => (0, String::new()),
                    "GitVcsDriver.defaultRef.originHead" => (1, String::new()),
                    "GitVcsDriver.defaultRef.candidate" => (0, String::new()),
                    "GitVcsDriver.statusDetailsRemote.status" => {
                        (0, "# branch.head main\n".to_owned())
                    }
                    "GitVcsDriver.currentRef" => (0, "main\n".to_owned()),
                    "GitVcsDriver.remoteProvider" | "GitManager.getRefs.inProgressPaths" => {
                        (1, String::new())
                    }
                    "GitManager.signal.refs" => {
                        (0, "deadbeef\trefs/heads/main\t/repository\n".to_owned())
                    }
                    operation => panic!("unexpected subscription-setup Git operation {operation}"),
                };
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

    impl EpochGitRunner {
        fn set_branch(&self, branch: &str) {
            *self.branch.lock().expect("branch lock") = branch.to_owned();
        }

        fn branch(&self) -> String {
            self.branch.lock().expect("branch lock").clone()
        }
    }

    impl GitProcessRunner for EpochGitRunner {
        fn run<'a>(
            &'a self,
            request: ProcessRequest,
            _cancellation: &'a CancellationToken,
        ) -> BoxGitProcessFuture<'a> {
            Box::pin(async move {
                if let Some(output) = signal_head_output(&request.operation) {
                    return Ok(output);
                }
                let branch = self.branch();
                let (exit_code, stdout) = match request.operation.as_str() {
                    "GitVcsDriver.detectRepository" => (0, "true\n".to_owned()),
                    "GitVcsDriver.resolveWatchRoots" => (0, String::new()),
                    "GitVcsDriver.resolveCommonDir" => (0, format!("{}\n", request.cwd.display())),
                    "GitVcsDriver.statusDetailsLocal.status" => {
                        (0, format!("# branch.head {branch}\n"))
                    }
                    "GitVcsDriver.statusDetailsLocal.remotes" => (0, String::new()),
                    "GitVcsDriver.remoteProvider" => (1, String::new()),
                    "GitVcsDriver.currentRef" => {
                        if self.ref_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                            let _ = self.ref_started.send(());
                            self.release_ref
                                .acquire()
                                .await
                                .expect("ref release remains open")
                                .forget();
                        }
                        (0, format!("{branch}\n"))
                    }
                    "GitVcsDriver.statusDetailsRemote.status" => {
                        let call = self.remote_calls.fetch_add(1, Ordering::SeqCst);
                        if call == 0 {
                            let _ = self.remote_started.send(());
                            self.release_remote
                                .acquire()
                                .await
                                .expect("remote release remains open")
                                .forget();
                        }
                        let ahead = if branch == "old" { 1 } else { 2 };
                        (
                            0,
                            format!(
                                "# branch.head {branch}\n# branch.upstream origin/{branch}\n# branch.ab +{ahead} -0\n"
                            ),
                        )
                    }
                    "GitVcsDriver.defaultRef.originHead"
                    | "GitVcsDriver.defaultRef.candidate"
                    | "GitManager.getRefs.inProgressPaths" => (1, String::new()),
                    "GitManager.signal.refs" => {
                        (0, format!("deadbeef\trefs/heads/{branch}\t/repository\n"))
                    }
                    operation => panic!("unexpected epoch Git operation {operation}"),
                };
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

    struct DelayedSignalRunner {
        inner: RemoteMismatchGitRunner,
        started: Notify,
        release: Semaphore,
        fail_head: bool,
    }

    impl GitProcessRunner for DelayedSignalRunner {
        fn run<'a>(
            &'a self,
            request: ProcessRequest,
            cancellation: &'a CancellationToken,
        ) -> BoxGitProcessFuture<'a> {
            Box::pin(async move {
                if request.operation == "GitManager.signal.headRef" {
                    self.started.notify_one();
                    self.release
                        .acquire()
                        .await
                        .expect("signature release")
                        .forget();
                    if self.fail_head {
                        return Ok(RemoteMismatchGitRunner::output(128, String::new()));
                    }
                }
                self.inner.run(request, cancellation).await
            })
        }
    }

    #[tokio::test]
    async fn remote_status_publishes_before_slow_or_failed_signature_inputs() {
        for (fail_head, fail_reflog) in [(false, false), (true, false), (false, true)] {
            let root = tempfile::tempdir().expect("remote publication fixture");
            let cwd = fs::canonicalize(root.path()).expect("canonical fixture");
            if fail_reflog {
                fs::create_dir_all(cwd.join(".git/logs/refs/stash"))
                    .expect("invalid reflog directory");
            }
            let runner = Arc::new(DelayedSignalRunner {
                inner: RemoteMismatchGitRunner::new(0, 0),
                started: Notify::new(),
                release: Semaphore::new(0),
                fail_head,
            });
            let broadcaster = StatusBroadcaster::new(
                Arc::new(GitRepository::with_runner_for_test(runner.clone())),
                Duration::from_secs(180),
                4,
            );
            let mut events =
                install_epoch_repository_for_lifecycle(&broadcaster, &cwd, 1, "main", 9);
            let previous_signature = Some(0x5a17);
            broadcaster
                .lock_state()
                .repositories
                .get_mut(&cwd)
                .unwrap()
                .git_manager_signature = previous_signature;
            let pending = {
                let broadcaster = broadcaster.clone();
                let cwd = cwd.clone();
                tokio::spawn(async move {
                    broadcaster
                        .refresh_remote(&cwd, &CancellationToken::new())
                        .await
                })
            };
            runner.started.notified().await;
            let publication = events.try_recv();
            runner.release.add_permits(1);
            let result = pending.await.expect("remote refresh joins");
            assert!(
                matches!(publication, Ok(StatusPublication { value: VcsStatusStreamEvent::RemoteUpdated { remote: Some(remote) }, .. }) if remote.ahead_count == 1),
                "signature I/O delayed remote publication"
            );
            result.expect("signature failure must not fail remote refresh");
            if fail_head || fail_reflog {
                assert_eq!(
                    broadcaster.lock_state().repositories[&cwd].git_manager_signature,
                    previous_signature
                );
            }
            broadcaster.shutdown().await;
        }
    }

    impl RemoteMismatchGitRunner {
        fn new(local_failures: usize, remote_mismatches: usize) -> Self {
            Self {
                local_calls: AtomicUsize::new(0),
                remote_calls: AtomicUsize::new(0),
                local_failures: AtomicUsize::new(local_failures),
                remote_mismatches,
                operation_changed: Notify::new(),
            }
        }

        async fn wait_for_calls(&self, local: usize, remote: usize) {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let changed = self.operation_changed.notified();
                    if self.local_calls.load(Ordering::SeqCst) >= local
                        && self.remote_calls.load(Ordering::SeqCst) >= remote
                    {
                        return;
                    }
                    changed.await;
                }
            })
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "local/remote call deadline: expected {local}/{remote}, observed {}/{}",
                    self.local_calls.load(Ordering::SeqCst),
                    self.remote_calls.load(Ordering::SeqCst)
                )
            });
        }

        fn output(exit_code: i32, stdout: String) -> ProcessOutput {
            ProcessOutput {
                exit_code,
                stdout,
                stderr: String::new(),
                stdout_truncated: false,
                stderr_truncated: false,
            }
        }
    }

    impl GitProcessRunner for RemoteMismatchGitRunner {
        fn run<'a>(
            &'a self,
            request: ProcessRequest,
            _cancellation: &'a CancellationToken,
        ) -> BoxGitProcessFuture<'a> {
            Box::pin(async move {
                if let Some(output) = signal_head_output(&request.operation) {
                    return Ok(output);
                }
                match request.operation.as_str() {
                    "GitVcsDriver.statusDetailsLocal.status" => {
                        self.local_calls.fetch_add(1, Ordering::SeqCst);
                        self.operation_changed.notify_waiters();
                        if self
                            .local_failures
                            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                                (remaining > 0).then(|| remaining - 1)
                            })
                            .is_ok()
                        {
                            return Err(ProcessError::NonZeroExit {
                                operation: request.operation,
                                exit_code: 1,
                                stdout_length: 0,
                                stderr_length: 24,
                                stdout: String::new().into_boxed_str(),
                                stderr: "controlled local failure".into(),
                            });
                        }
                        Ok(Self::output(0, "# branch.head main\n".to_owned()))
                    }
                    "GitVcsDriver.statusDetailsRemote.status" => {
                        let call = self.remote_calls.fetch_add(1, Ordering::SeqCst);
                        self.operation_changed.notify_waiters();
                        let branch = if call < self.remote_mismatches {
                            "stale"
                        } else {
                            "main"
                        };
                        Ok(Self::output(
                            0,
                            format!(
                                "# branch.head {branch}\n# branch.upstream origin/{branch}\n# branch.ab +1 -0\n"
                            ),
                        ))
                    }
                    "GitVcsDriver.detectRepository" => Ok(Self::output(0, "true\n".to_owned())),
                    "GitVcsDriver.statusDetailsLocal.remotes"
                    | "GitVcsDriver.remoteProvider"
                    | "GitVcsDriver.defaultRef.originHead"
                    | "GitVcsDriver.defaultRef.candidate"
                    | "GitManager.getRefs.inProgressPaths" => Ok(Self::output(1, String::new())),
                    "GitManager.signal.refs" => Ok(Self::output(
                        0,
                        "deadbeef\trefs/heads/main\t/repository\n".to_owned(),
                    )),
                    operation => panic!("unexpected remote-mismatch Git operation {operation}"),
                }
            })
        }
    }

    impl GitProcessRunner for BlockingRemoteGitRunner {
        fn run<'a>(
            &'a self,
            mut request: ProcessRequest,
            cancellation: &'a CancellationToken,
        ) -> BoxGitProcessFuture<'a> {
            request.command.clone_from(&self.command);
            let mut environment = self.environment.iter().cloned().collect::<BTreeMap<_, _>>();
            environment.extend(request.env);
            request.env = environment.into_iter().collect();
            assert!(
                request.env.iter().any(|(name, value)| {
                    name == "GIT_CONFIG_GLOBAL" && value == self.expected_git_config.as_os_str()
                }),
                "production Git request did not receive the test-owned global config"
            );
            assert!(request.env.iter().all(|(name, _)| {
                !matches!(
                    name.to_string_lossy().to_ascii_uppercase().as_str(),
                    "GIT_DIR" | "GIT_WORK_TREE" | "GIT_INDEX_FILE" | "GIT_CONFIG_SYSTEM"
                )
            }));
            Box::pin(async move {
                if request.operation == "GitVcsDriver.resolveWatchRoots" {
                    self.watch_root_calls.fetch_add(1, Ordering::SeqCst);
                }
                let output = |exit_code: i32, stdout: String| ProcessOutput {
                    exit_code,
                    stdout,
                    stderr: String::new(),
                    stdout_truncated: false,
                    stderr_truncated: false,
                };
                match request.operation.as_str() {
                    "GitVcsDriver.detectRepository" => {
                        return Ok(output(0, "true\n".to_owned()));
                    }
                    "GitVcsDriver.statusDetailsLocal.status" => {
                        self.local_status_calls.fetch_add(1, Ordering::SeqCst);
                        let _ = self.local_status_started.send(());
                        if let Some(release) = &self.release_local_status {
                            tokio::select! {
                                permit = release.acquire() => {
                                    permit.expect("local-status release owner remains alive").forget();
                                }
                                () = cancellation.cancelled() => {
                                    return Err(ProcessError::Cancelled {
                                        operation: request.operation,
                                    });
                                }
                            }
                        }
                        let dirty = fs::read_to_string(request.cwd.join("tracked.txt"))
                            .is_ok_and(|contents| contents != "base\n");
                        let mut stdout = "# branch.head main\n".to_owned();
                        if dirty {
                            stdout.push_str(
                                "1 .M N... 100644 100644 100644 deadbeef deadbeef tracked.txt\n",
                            );
                        }
                        return Ok(output(0, stdout));
                    }
                    "GitVcsDriver.statusDetailsLocal.stagedNumstat"
                    | "GitVcsDriver.statusDetailsLocal.remotes" => {
                        return Ok(output(0, String::new()));
                    }
                    "GitVcsDriver.statusDetailsLocal.unstagedNumstat" => {
                        let dirty = fs::read_to_string(request.cwd.join("tracked.txt"))
                            .is_ok_and(|contents| contents != "base\n");
                        return Ok(output(
                            0,
                            if dirty {
                                "1\t1\ttracked.txt\n".to_owned()
                            } else {
                                String::new()
                            },
                        ));
                    }
                    "GitVcsDriver.defaultRef.originHead" => {
                        return Ok(output(1, String::new()));
                    }
                    "GitVcsDriver.defaultRef.candidate" => {
                        let is_main = request
                            .args
                            .last()
                            .is_some_and(|value| value == "refs/heads/main");
                        return Ok(output(i32::from(!is_main), String::new()));
                    }
                    _ => {}
                }
                if request.operation == "GitVcsDriver.statusDetailsRemote.status" {
                    let _ = self.remote_started.send(());
                    // The production owner may drop this future on cancellation
                    // before it is polled again, so the outcome is reported from a
                    // drop guard as well: a dropped blocked refresh is a cancelled
                    // one, and only an explicit release counts as `Released`.
                    let mut outcome = BlockingRemoteOutcomeReport {
                        sender: self.remote_outcome.clone(),
                    };
                    tokio::select! {
                        biased;
                        () = cancellation.cancelled() => {
                            let _ = self.remote_cancelled.send(());
                            outcome.report(BlockingRemoteOutcome::Cancelled);
                            return Err(ProcessError::Cancelled {
                                operation: request.operation,
                            });
                        }
                        permit = self.release_remote.acquire() => {
                            permit.expect("remote release owner remains alive").forget();
                            outcome.report(BlockingRemoteOutcome::Released);
                        }
                    }
                }
                ProcessRunner
                    .run_with_clean_environment_for_test(request, cancellation)
                    .await
            })
        }
    }

    struct SharedRepositoryFetchRunner {
        common_dir: PathBuf,
        fetches: AtomicUsize,
        fetch_failures: AtomicUsize,
        remote_status_calls: Mutex<BTreeMap<PathBuf, usize>>,
        remote_status_changed: Notify,
        dirty: AtomicBool,
        common_dir_started: Option<mpsc::UnboundedSender<()>>,
        release_common_dir: Option<Arc<Semaphore>>,
        fetch_started: Option<mpsc::UnboundedSender<()>>,
        fetch_cancelled: Option<mpsc::UnboundedSender<()>>,
        release_fetch: Option<Arc<Semaphore>>,
    }

    impl SharedRepositoryFetchRunner {
        fn branch(cwd: &Path) -> &'static str {
            if cwd.ends_with("feature") {
                "feature/test"
            } else {
                "main"
            }
        }

        fn remote(cwd: &Path) -> &'static str {
            if cwd.ends_with("feature") {
                "backup"
            } else {
                "origin"
            }
        }

        fn fetch_count(&self) -> usize {
            self.fetches.load(Ordering::SeqCst)
        }

        fn remote_status_count(&self, cwd: &Path) -> usize {
            self.remote_status_calls
                .lock()
                .expect("remote status count lock")
                .get(cwd)
                .copied()
                .unwrap_or(0)
        }

        async fn wait_for_remote_status_count(&self, cwd: &Path, expected: usize) {
            loop {
                let notified = self.remote_status_changed.notified();
                if self.remote_status_count(cwd) == expected {
                    return;
                }
                notified.await;
            }
        }
    }

    impl GitProcessRunner for SharedRepositoryFetchRunner {
        fn run<'a>(
            &'a self,
            request: ProcessRequest,
            cancellation: &'a CancellationToken,
        ) -> BoxGitProcessFuture<'a> {
            if request.operation == "GitVcsDriver.statusDetailsRemote.status" {
                *self
                    .remote_status_calls
                    .lock()
                    .expect("remote status count lock")
                    .entry(request.cwd.clone())
                    .or_default() += 1;
                self.remote_status_changed.notify_waiters();
            }
            Box::pin(async move {
                if let Some(output) = signal_head_output(&request.operation) {
                    return Ok(output);
                }
                let branch = Self::branch(&request.cwd);
                let remote = Self::remote(&request.cwd);
                let branch_status = || {
                    let ahead = if self.fetch_count() == 0 {
                        0
                    } else if branch == "main" {
                        1
                    } else {
                        2
                    };
                    format!(
                        "# branch.head {branch}\n# branch.upstream {remote}/{branch}\n# branch.ab +{ahead} -0\n"
                    )
                };
                let (exit_code, stdout) = match request.operation.as_str() {
                    "GitVcsDriver.detectRepository" => (0, "true\n".to_owned()),
                    "GitVcsDriver.resolveWatchRoots" => (0, String::new()),
                    "GitVcsDriver.statusDetailsLocal.status" => {
                        let mut status = branch_status();
                        if self.dirty.load(Ordering::SeqCst) {
                            status.push_str(
                                "1 .M N... 100644 100644 100644 deadbeef deadbeef tracked.txt\n",
                            );
                        }
                        (0, status)
                    }
                    "GitVcsDriver.statusDetailsLocal.unstagedNumstat" => {
                        (0, "1\t1\ttracked.txt\n".to_owned())
                    }
                    "GitVcsDriver.statusDetailsRemote.status" => (0, branch_status()),
                    "GitVcsDriver.statusDetailsLocal.remotes"
                    | "GitVcsDriver.refreshRemoteStatus.remotes" => {
                        (0, "backup\norigin\n".to_owned())
                    }
                    "GitVcsDriver.defaultRef.originHead" => {
                        (0, "refs/remotes/origin/main\n".to_owned())
                    }
                    "GitVcsDriver.remoteProvider" | "GitManager.getRefs.inProgressPaths" => {
                        (1, String::new())
                    }
                    "GitVcsDriver.statusDetailsRemote.defaultDelta" => {
                        (0, format!("{}\n", usize::from(self.fetch_count() > 0) * 2))
                    }
                    "GitVcsDriver.currentRef" => (0, format!("{branch}\n")),
                    "GitVcsDriver.resolveCommonDir" => {
                        if let Some(started) = &self.common_dir_started {
                            let _ = started.send(());
                        }
                        if let Some(release) = &self.release_common_dir {
                            release
                                .acquire()
                                .await
                                .expect("common-dir release remains open")
                                .forget();
                        }
                        (0, format!("{}\n", self.common_dir.display()))
                    }
                    "GitVcsDriver.automaticFetch.upstreams" => {
                        (0, "feature/test\0backup\nmain\0origin\n".to_owned())
                    }
                    "GitVcsDriver.refreshRemoteStatus.upstream" => {
                        (0, format!("{remote}/{branch}\n"))
                    }
                    "GitManager.signal.refs" => (
                        0,
                        format!("deadbeef\trefs/heads/{branch}\t{}\n", request.cwd.display()),
                    ),
                    "GitVcsDriver.automaticFetch.fetch"
                    | "GitVcsDriver.refreshRemoteStatus.fetch" => {
                        self.fetches.fetch_add(1, Ordering::SeqCst);
                        if let Some(started) = &self.fetch_started {
                            let _ = started.send(());
                        }
                        if let Some(release) = &self.release_fetch {
                            tokio::select! {
                                permit = release.acquire() => {
                                    permit.expect("fetch release remains open").forget();
                                }
                                () = cancellation.cancelled() => {
                                    if let Some(cancelled) = &self.fetch_cancelled {
                                        let _ = cancelled.send(());
                                    }
                                    return Err(ProcessError::Cancelled {
                                        operation: request.operation,
                                    });
                                }
                            }
                        }
                        if self
                            .fetch_failures
                            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                                (remaining > 0).then(|| remaining - 1)
                            })
                            .is_ok()
                        {
                            return Err(ProcessError::NonZeroExit {
                                operation: request.operation,
                                exit_code: 1,
                                stdout_length: 0,
                                stderr_length: 24,
                                stdout: String::new().into_boxed_str(),
                                stderr: "controlled fetch failure".into(),
                            });
                        }
                        (0, String::new())
                    }
                    operation => panic!("unexpected shared-repository Git operation {operation}"),
                };
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

    async fn next_remote_update(subscription: &mut StatusSubscription) -> VcsStatusRemoteResult {
        loop {
            if let Some(VcsStatusStreamEvent::RemoteUpdated {
                remote: Some(remote),
            }) = subscription.recv().await
            {
                return remote;
            }
        }
    }

    fn isolated_git_environment(sandbox: &TestSandbox) -> (PathBuf, Vec<(OsString, OsString)>) {
        let hooks = sandbox.path("hooks");
        fs::create_dir(&hooks).expect("isolated hooks directory");
        let isolated_config = sandbox.path("isolated-global.gitconfig");
        fs::write(
            &isolated_config,
            format!(
                "[commit]\n\tgpgSign = false\n[core]\n\thooksPath = {}\n",
                hooks.to_string_lossy().replace('\\', "/")
            ),
        )
        .expect("isolated global config");

        let hostile_config = sandbox.path("hostile-global.gitconfig");
        fs::write(
            &hostile_config,
            "[commit]\n\tgpgSign = true\n[core]\n\thooksPath = missing-hooks\n",
        )
        .expect("hostile global config");
        let hostile_git_dir = sandbox.path("hostile-git-dir");
        let hostile_work_tree = sandbox.path("hostile-work-tree");
        let hostile_index = sandbox.path("hostile-index");
        let mut environment = sandbox.environment([
            (
                "GIT_CONFIG_GLOBAL",
                hostile_config.to_string_lossy().into_owned(),
            ),
            (
                "GIT_CONFIG_SYSTEM",
                hostile_config.to_string_lossy().into_owned(),
            ),
            ("GIT_DIR", hostile_git_dir.to_string_lossy().into_owned()),
            (
                "GIT_WORK_TREE",
                hostile_work_tree.to_string_lossy().into_owned(),
            ),
            (
                "GIT_INDEX_FILE",
                hostile_index.to_string_lossy().into_owned(),
            ),
        ]);
        environment.retain(|name, _| {
            !name
                .as_bytes()
                .get(..4)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"GIT_"))
        });
        environment.extend([
            (
                "GIT_CONFIG_GLOBAL".to_owned(),
                isolated_config.to_string_lossy().into_owned(),
            ),
            ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
            ("GIT_TERMINAL_PROMPT".to_owned(), "0".to_owned()),
        ]);
        (
            isolated_config,
            environment
                .into_iter()
                .map(|(name, value)| (name.into(), value.into()))
                .collect(),
        )
    }

    fn initialize_test_repository(
        sandbox: &TestSandbox,
        command: &Path,
        environment: &[(OsString, OsString)],
    ) -> PathBuf {
        let repository = sandbox.path("repository");
        fs::create_dir(&repository).expect("temporary Git repository");
        for args in [
            &["init", "--quiet", "-b", "main"][..],
            &["config", "user.name", "Coddy Studio Test"][..],
            &["config", "user.email", "coddy-studio@example.invalid"][..],
        ] {
            let output = Command::new(command)
                .args(args)
                .current_dir(&repository)
                .env_clear()
                .envs(environment.iter().cloned())
                .output()
                .expect("Git fixture command starts");
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        fs::write(repository.join("tracked.txt"), "base\n").expect("clean fixture file");
        for args in [
            &["add", "--", "tracked.txt"][..],
            &["commit", "--quiet", "-m", "initial"][..],
        ] {
            let output = Command::new(command)
                .args(args)
                .current_dir(&repository)
                .env_clear()
                .envs(environment.iter().cloned())
                .output()
                .expect("Git fixture command starts");
            assert!(
                output.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        repository
    }

    #[test]
    fn subscriber_capacity_is_never_zero() {
        let broadcaster =
            StatusBroadcaster::new(Arc::new(GitRepository::default()), Duration::ZERO, 0);
        assert_eq!(broadcaster.inner.subscriber_capacity, 1);
    }

    struct IsolatedGitRunner {
        command: PathBuf,
        environment: Vec<(OsString, OsString)>,
    }

    impl GitProcessRunner for IsolatedGitRunner {
        fn run<'a>(
            &'a self,
            mut request: ProcessRequest,
            cancellation: &'a CancellationToken,
        ) -> BoxGitProcessFuture<'a> {
            request.command.clone_from(&self.command);
            let mut environment = self.environment.iter().cloned().collect::<BTreeMap<_, _>>();
            environment.extend(request.env);
            request.env = environment.into_iter().collect();
            Box::pin(async move {
                ProcessRunner
                    .run_with_clean_environment_for_test(request, cancellation)
                    .await
            })
        }
    }

    #[tokio::test]
    async fn exhausted_watcher_preserves_snapshot_health_and_local_refresh() {
        let sandbox = TestSandbox::new("git-broadcaster-exhausted-watcher");
        let command = sandbox.executable_on_path("git");
        let (_, environment) = isolated_git_environment(&sandbox);
        let cwd = fs::canonicalize(initialize_test_repository(&sandbox, &command, &environment))
            .expect("canonical repository");
        let repository = GitRepository::with_runner_for_test(Arc::new(IsolatedGitRunner {
            command,
            environment,
        }));
        let broadcaster = StatusBroadcaster::with_watcher_for_test(
            Arc::new(repository),
            GitWatchService::with_exhausted_backend_for_test(),
        );
        let mut subscription = tokio::time::timeout(
            Duration::from_secs(5),
            broadcaster.subscribe(cwd.clone(), CancellationToken::new()),
        )
        .await
        .expect("status subscription returns promptly when native watcher creation fails")
        .expect("fallback status subscription");
        let snapshot = tokio::time::timeout(Duration::from_secs(5), subscription.recv())
            .await
            .expect("fallback subscription publishes its initial snapshot");
        assert!(
            matches!(snapshot, Some(VcsStatusStreamEvent::Snapshot { local, .. })
            if local.is_repo && !local.has_working_tree_changes)
        );
        assert!(
            broadcaster.lock_state().repositories[&cwd]
                .git_manager_signal
                .borrow()
                .watcher_degraded
        );

        fs::write(
            cwd.join("tracked.txt"),
            "changed while native watching is unavailable\n",
        )
        .expect("local worktree edit");
        tokio::time::timeout(
            Duration::from_secs(5),
            broadcaster.refresh_local(&cwd, &CancellationToken::new()),
        )
        .await
        .expect("explicit local refresh completes under watcher fallback")
        .expect("fallback local refresh");
        let local = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let VcsStatusStreamEvent::LocalUpdated { local } = subscription
                    .recv()
                    .await
                    .expect("fallback status stream remains open")
                {
                    break local;
                }
            }
        })
        .await
        .expect("explicit refresh publishes LocalUpdated under watcher fallback");
        assert!(local.has_working_tree_changes);
        assert!(
            local
                .working_tree
                .files
                .iter()
                .any(|file| file.path == "tracked.txt")
        );
        assert!(
            broadcaster.lock_state().repositories[&cwd]
                .git_manager_signal
                .borrow()
                .watcher_degraded
        );
        drop(subscription);
        tokio::time::timeout(Duration::from_secs(5), broadcaster.shutdown())
            .await
            .expect("fallback lifecycle shuts down promptly");
    }

    async fn assert_broken_repository_repair_is_watched(metadata: &str, damaged: &str) {
        let _native_watcher_permit = super::super::acquire_native_watcher_test_permit().await;
        let sandbox = TestSandbox::new("git-broadcaster-broken-repository-watch");
        let command = sandbox.executable_on_path("git");
        let (_, environment) = isolated_git_environment(&sandbox);
        let cwd = sandbox.path("repository");
        fs::create_dir(&cwd).expect("repository directory");
        let output = Command::new(&command)
            .args(["init", "--quiet", "-b", "main"])
            .current_dir(&cwd)
            .env_clear()
            .envs(environment.iter().cloned())
            .output()
            .expect("fixture Git starts");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let cwd = fs::canonicalize(cwd).expect("canonical repository");
        let metadata_path = cwd.join(".git").join(metadata);
        let original = fs::read(&metadata_path).expect("original Git metadata");
        fs::write(&metadata_path, damaged).expect("damage Git metadata");
        let repository = GitRepository::with_runner_for_test(Arc::new(IsolatedGitRunner {
            command,
            environment,
        }));
        let broadcaster =
            StatusBroadcaster::new(Arc::new(repository), Duration::from_secs(3_600), 4);
        let fetch_finished = broadcaster.inner.fetch_attachment_finished.notified();
        tokio::pin!(fetch_finished);
        fetch_finished.as_mut().enable();
        let mut subscription = test_timeout(
            "status subscription setup",
            broadcaster.subscribe(cwd.clone(), CancellationToken::new()),
        )
        .await
        .expect("broken repository status subscription");
        let Some(VcsStatusStreamEvent::Snapshot { local, .. }) =
            test_timeout("subscription event", subscription.recv()).await
        else {
            panic!("initial status snapshot");
        };
        assert!(!local.is_repo);
        assert_eq!(
            local.repository_unavailable_reason,
            Some(VcsRepositoryUnavailableReason::Unreadable)
        );
        tokio::time::timeout(Duration::from_secs(5), fetch_finished)
            .await
            .expect("initial fetch attachment finishes before the repair");
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let event = test_timeout("subscription event", subscription.recv())
                    .await
                    .expect("initial remote observation");
                if matches!(event, VcsStatusStreamEvent::RemoteUpdated { remote: None }) {
                    break;
                }
            }
        })
        .await
        .expect(
            "initial remote observation settles before repair so it cannot request a local read",
        );
        {
            let state = broadcaster.lock_state();
            let entry = state.repositories.get(&cwd).expect("active lifecycle");
            assert!(
                entry.git_manager_common_dir.is_none(),
                "watch fallback is not a Git Manager identity"
            );
            assert!(
                entry.repository_key.is_none(),
                "watch fallback does not attach automatic fetch"
            );
        }
        assert_eq!(
            broadcaster.active_watcher_count_for_test(),
            1,
            "damaged repository is watched"
        );
        assert_eq!(
            broadcaster.inner.watcher.only_health_for_test(),
            GitWatcherHealth::Healthy
        );
        tokio::time::timeout(
            Duration::from_secs(5),
            broadcaster.wait_for_status_scheduler_started_for_test(),
        )
        .await
        .expect("status scheduler starts before repair");

        fs::write(&metadata_path, original).expect("repair Git metadata");
        let repaired = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let event = test_timeout("subscription event", subscription.recv())
                    .await
                    .expect("status subscription remains open");
                if let VcsStatusStreamEvent::LocalUpdated { local } = event
                    && local.is_repo
                {
                    break local;
                }
            }
        })
        .await
        .expect("native watcher notices repair without Retry, refresh, or the hourly safety read");
        assert!(repaired.repository_unavailable_reason.is_none());
        test_timeout(
            "repair attaches automatic fetch",
            broadcaster
                .inner
                .fetch_owner
                .wait_for_worktree_count_for_test(1),
        )
        .await;
        assert_eq!(
            broadcaster.lock_state().repositories[&cwd].repository_key,
            Some(cwd.join(".git")),
            "repair resolves fetch identity through Git"
        );
        assert!(
            broadcaster
                .inner
                .fetch_owner
                .has_subscriber_for_test(&cwd, subscription.subscriber_id)
        );
        drop(subscription);
        test_timeout("broadcaster shutdown", broadcaster.shutdown()).await;
        assert_eq!(broadcaster.active_watcher_count_for_test(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn broken_repository_head_repair_is_watched() {
        assert_broken_repository_repair_is_watched("HEAD", "not a ref").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn broken_repository_config_repair_is_watched() {
        assert_broken_repository_repair_is_watched("config", "[invalid").await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn broken_repository_watch_fallback_keeps_plain_folders_degraded() {
        let _native_watcher_permit = super::super::acquire_native_watcher_test_permit().await;
        let sandbox = TestSandbox::new("git-broadcaster-plain-folder-watch");
        let command = sandbox.executable_on_path("git");
        let (_, environment) = isolated_git_environment(&sandbox);
        let cwd = fs::canonicalize(sandbox.root()).expect("canonical plain folder");
        let repository = GitRepository::with_runner_for_test(Arc::new(IsolatedGitRunner {
            command,
            environment,
        }));
        let broadcaster =
            StatusBroadcaster::new(Arc::new(repository), Duration::from_secs(3_600), 4);
        let mut subscription = test_timeout(
            "status subscription setup",
            broadcaster.subscribe(cwd.clone(), CancellationToken::new()),
        )
        .await
        .expect("plain folder status subscription");
        let Some(VcsStatusStreamEvent::Snapshot { local, .. }) =
            test_timeout("subscription event", subscription.recv()).await
        else {
            panic!("initial status snapshot");
        };
        assert!(!local.is_repo);
        assert_eq!(
            local.repository_unavailable_reason,
            Some(VcsRepositoryUnavailableReason::Absent)
        );
        assert_eq!(broadcaster.active_watcher_count_for_test(), 0);
        assert!(
            broadcaster.lock_state().repositories[&cwd]
                .git_manager_signal
                .borrow()
                .watcher_degraded
        );
        drop(subscription);
        test_timeout("broadcaster shutdown", broadcaster.shutdown()).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn watcher_is_installed_before_the_initial_status_read_completes() {
        let _native_watcher_permit = super::super::acquire_native_watcher_test_permit().await;
        let sandbox = TestSandbox::new("git-broadcaster-watch-before-read");
        let command = sandbox.executable_on_path("git");
        let (expected_git_config, environment) = isolated_git_environment(&sandbox);
        let repository =
            fs::canonicalize(initialize_test_repository(&sandbox, &command, &environment))
                .expect("canonical repository fixture");
        let (remote_started, _) = mpsc::unbounded_channel();
        let (remote_cancelled, _) = mpsc::unbounded_channel();
        let (local_status_started, mut local_status_started_rx) = mpsc::unbounded_channel();
        let release_local_status = Arc::new(Semaphore::new(0));
        let runner = Arc::new(BlockingRemoteGitRunner {
            command,
            environment,
            expected_git_config,
            remote_started,
            remote_cancelled,
            remote_outcome: None,
            local_status_started,
            watch_root_calls: AtomicUsize::new(0),
            local_status_calls: AtomicUsize::new(0),
            release_local_status: Some(Arc::clone(&release_local_status)),
            release_remote: Arc::new(Semaphore::new(1)),
        });
        let git = GitRepository::with_runner_for_test(runner.clone());
        let broadcaster = StatusBroadcaster::new(Arc::new(git), Duration::from_secs(3_600), 4);

        let first_broadcaster = broadcaster.clone();
        let first_cwd = repository.clone();
        let first = tokio::spawn(async move {
            test_timeout(
                "status subscription setup",
                first_broadcaster.subscribe(first_cwd, CancellationToken::new()),
            )
            .await
        });
        let second_broadcaster = broadcaster.clone();
        let second_cwd = repository.clone();
        let second = tokio::spawn(async move {
            test_timeout(
                "status subscription setup",
                second_broadcaster.subscribe(second_cwd, CancellationToken::new()),
            )
            .await
        });
        test_timeout(
            "local_status_started_rx event",
            local_status_started_rx.recv(),
        )
        .await
        .expect("initial local status read starts");

        assert_eq!(broadcaster.active_watcher_count_for_test(), 1);
        assert_eq!(broadcaster.active_poller_count(), 0);
        let shared_read = tokio::time::timeout(
            Duration::from_secs(5),
            broadcaster.wait_for_local_read_leases_for_test(&repository, 2),
        )
        .await;
        assert!(
            shared_read.is_ok(),
            "both concurrent subscribers must share the in-flight local read; roots={}, physical_local={}, leases={}, watchers={}",
            runner.watch_root_calls.load(Ordering::SeqCst),
            runner.local_status_calls.load(Ordering::SeqCst),
            broadcaster
                .inner
                .status_owner
                .lease_count_for_test(&StatusReadKey {
                    canonical_cwd: repository.clone(),
                    output_kind: StatusOutputKind::Local,
                }),
            broadcaster.active_watcher_count_for_test(),
        );
        assert_eq!(runner.watch_root_calls.load(Ordering::SeqCst), 2);
        assert_eq!(runner.local_status_calls.load(Ordering::SeqCst), 1);
        assert!(local_status_started_rx.try_recv().is_err());

        release_local_status.add_permits(1);
        let mut first_subscription = test_timeout("first task completion", first)
            .await
            .expect("first subscription task joins")
            .expect("first status subscription starts");
        let mut second_subscription = test_timeout("second task completion", second)
            .await
            .expect("second subscription task joins")
            .expect("second status subscription starts");
        assert!(matches!(
            test_timeout("first_subscription event", first_subscription.recv()).await,
            Some(VcsStatusStreamEvent::Snapshot { .. })
        ));
        assert!(matches!(
            test_timeout("second_subscription event", second_subscription.recv()).await,
            Some(VcsStatusStreamEvent::Snapshot { .. })
        ));
        assert_eq!(broadcaster.active_watcher_count_for_test(), 1);
        assert_eq!(broadcaster.active_poller_count(), 1);
        tokio::time::timeout(
            Duration::from_secs(5),
            broadcaster.wait_for_status_scheduler_started_for_test(),
        )
        .await
        .expect("status scheduler starts before the native edit");
        use std::io::Write;
        let mut tracked = fs::OpenOptions::new()
            .append(true)
            .open(repository.join("tracked.txt"))
            .expect("open tracked worktree file");
        tracked
            .write_all(b"changed externally\n")
            .expect("size-changing external worktree edit");
        tracked.sync_all().expect("durable external worktree edit");
        drop(tracked);
        tokio::time::timeout(Duration::from_secs(5), local_status_started_rx.recv())
            .await
            .expect("native watcher schedules a local status refresh")
            .expect("local-status start signal remains open");
        release_local_status.add_permits(1);
        let dirty = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(VcsStatusStreamEvent::LocalUpdated { local }) =
                    test_timeout("first_subscription event", first_subscription.recv()).await
                    && local.has_working_tree_changes
                {
                    break local;
                }
            }
        })
        .await
        .expect("watcher-triggered local status read publishes the durable edit");
        assert!(
            dirty
                .working_tree
                .files
                .iter()
                .any(|file| file.path == "tracked.txt")
        );
        drop(first_subscription);
        drop(second_subscription);
        tokio::time::timeout(
            Duration::from_secs(5),
            broadcaster.wait_for_status_scheduler_for_test(),
        )
        .await
        .expect("final subscriber tears down the watcher");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn subscribers_share_one_watcher_and_last_drop_allows_clean_reattachment_and_shutdown() {
        let _native_watcher_permit = super::super::acquire_native_watcher_test_permit().await;
        let sandbox = TestSandbox::new("git-broadcaster-watch-lifecycle");
        let command = sandbox.executable_on_path("git");
        let (expected_git_config, environment) = isolated_git_environment(&sandbox);
        let repository = initialize_test_repository(&sandbox, &command, &environment);
        let (remote_started, _) = mpsc::unbounded_channel();
        let (remote_cancelled, _) = mpsc::unbounded_channel();
        let (local_status_started, _) = mpsc::unbounded_channel();
        let git = GitRepository::with_runner_for_test(Arc::new(BlockingRemoteGitRunner {
            command,
            environment,
            expected_git_config,
            remote_started,
            remote_cancelled,
            remote_outcome: None,
            local_status_started,
            watch_root_calls: AtomicUsize::new(0),
            local_status_calls: AtomicUsize::new(0),
            release_local_status: None,
            release_remote: Arc::new(Semaphore::new(16)),
        }));
        let broadcaster = StatusBroadcaster::new(Arc::new(git), Duration::from_secs(3_600), 4);

        let first = test_timeout(
            "status subscription setup",
            broadcaster.subscribe(repository.clone(), CancellationToken::new()),
        )
        .await
        .expect("first status subscription starts");
        let second = test_timeout(
            "status subscription setup",
            broadcaster.subscribe(repository.clone(), CancellationToken::new()),
        )
        .await
        .expect("second status subscription starts");
        assert_eq!(broadcaster.active_watcher_count_for_test(), 1);
        drop(first);
        assert_eq!(broadcaster.active_watcher_count_for_test(), 1);
        drop(second);
        tokio::time::timeout(
            Duration::from_secs(5),
            broadcaster.wait_for_status_scheduler_for_test(),
        )
        .await
        .expect("last subscriber tears down the shared watcher");
        assert_eq!(broadcaster.active_poller_count(), 0);

        let mut reattached = test_timeout(
            "status subscription setup",
            broadcaster.subscribe(repository, CancellationToken::new()),
        )
        .await
        .expect("fresh status subscription starts after teardown");
        assert!(matches!(
            test_timeout("reattached event", reattached.recv()).await,
            Some(VcsStatusStreamEvent::Snapshot { .. })
        ));
        assert_eq!(broadcaster.active_watcher_count_for_test(), 1);

        test_timeout("broadcaster shutdown", broadcaster.shutdown()).await;
        assert_eq!(broadcaster.active_poller_count(), 0);
        assert_eq!(broadcaster.active_watcher_count_for_test(), 0);
        assert!(
            test_timeout("reattached event", reattached.recv())
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn retirement_wait_signal_is_observed_by_a_late_waiter() {
        let broadcaster = StatusBroadcaster::new(
            Arc::new(GitRepository::default()),
            Duration::from_secs(3_600),
            4,
        );
        let cwd = PathBuf::from("retiring-repository");
        let tasks = TaskTracker::new();
        let held_task = tasks.token();
        tasks.close();
        broadcaster
            .lock_state()
            .retiring
            .insert(cwd.clone(), vec![tasks]);
        let baseline = broadcaster.retirement_wait_count_for_test();
        let retirement = broadcaster.await_retired_lifecycle(&cwd);
        tokio::pin!(retirement);
        assert!(futures_util::poll!(&mut retirement).is_pending());

        tokio::time::timeout(
            Duration::from_millis(100),
            broadcaster.wait_for_retirement_wait_started_for_test(baseline),
        )
        .await
        .expect("a late waiter must observe the already-started retirement wait");
        let next_baseline = broadcaster.retirement_wait_count_for_test();
        assert!(
            tokio::time::timeout(
                Duration::from_millis(10),
                broadcaster.wait_for_retirement_wait_started_for_test(next_baseline),
            )
            .await
            .is_err(),
            "an earlier retirement wait must not satisfy a new baseline"
        );
        drop(held_task);
        tokio::time::timeout(Duration::from_secs(5), retirement)
            .await
            .expect("retirement settles after the held task is released");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn final_release_settles_the_old_watcher_generation_before_reattachment() {
        let _native_watcher_permit = super::super::acquire_native_watcher_test_permit().await;
        let sandbox = TestSandbox::new("git-broadcaster-watcher-generation-retirement");
        let command = sandbox.executable_on_path("git");
        let (expected_git_config, environment) = isolated_git_environment(&sandbox);
        let repository =
            fs::canonicalize(initialize_test_repository(&sandbox, &command, &environment))
                .expect("canonical repository fixture");
        let (remote_started, _) = mpsc::unbounded_channel();
        let (remote_cancelled, _) = mpsc::unbounded_channel();
        let (local_status_started, _) = mpsc::unbounded_channel();
        let git = GitRepository::with_runner_for_test(Arc::new(BlockingRemoteGitRunner {
            command,
            environment,
            expected_git_config,
            remote_started,
            remote_cancelled,
            remote_outcome: None,
            local_status_started,
            watch_root_calls: AtomicUsize::new(0),
            local_status_calls: AtomicUsize::new(0),
            release_local_status: None,
            release_remote: Arc::new(Semaphore::new(16)),
        }));
        let broadcaster = StatusBroadcaster::new(Arc::new(git), Duration::from_secs(3_600), 4);
        let mut first = test_timeout(
            "status subscription setup",
            broadcaster.subscribe(repository.clone(), CancellationToken::new()),
        )
        .await
        .expect("first status subscription starts");
        assert!(matches!(
            test_timeout("first event", first.recv()).await,
            Some(VcsStatusStreamEvent::Snapshot { .. })
        ));
        broadcaster
            .wait_for_status_scheduler_started_for_test()
            .await;

        let git_dir = fs::canonicalize(repository.join(".git"))
            .expect("canonical repository metadata directory");
        let release_old_watcher = Arc::new(Semaphore::new(0));
        let (old_cancellation_observed, mut old_cancellation_observed_rx) =
            mpsc::unbounded_channel();
        broadcaster
            .hold_active_watcher_until_released_for_test(
                &repository,
                GitWatchRequest {
                    worktree_root: repository.clone(),
                    git_dir: git_dir.clone(),
                    common_dir: git_dir,
                },
                Arc::clone(&release_old_watcher),
                old_cancellation_observed,
            )
            .await;
        broadcaster
            .inner
            .watcher
            .force_only_entry_fallback_for_test();
        let old_generation = broadcaster.inner.watcher.only_generation_for_test();
        assert_eq!(
            broadcaster.inner.watcher.only_health_for_test(),
            super::super::GitWatcherHealth::FallbackRequired
        );

        drop(first);
        test_timeout(
            "old_cancellation_observed_rx event",
            old_cancellation_observed_rx.recv(),
        )
        .await
        .expect("old lifecycle observes final-release cancellation");
        let reattach_broadcaster = broadcaster.clone();
        let reattach_cwd = repository.clone();
        let retirement_baseline = broadcaster.retirement_wait_count_for_test();
        let mut reattach = tokio::spawn(async move {
            test_timeout(
                "status subscription setup",
                reattach_broadcaster.subscribe(reattach_cwd, CancellationToken::new()),
            )
            .await
        });

        tokio::select! {
            biased;
            () = broadcaster.wait_for_retirement_wait_started_for_test(retirement_baseline) => {
                assert!(
                    !reattach.is_finished(),
                    "reattachment remains fenced while the old watcher task is held"
                );
                release_old_watcher.add_permits(1);
            }
            joined = &mut reattach => {
                let _stale = joined
                    .expect("reattachment task joins")
                    .expect("reattachment unexpectedly completes");
                release_old_watcher.add_permits(1);
                assert_ne!(
                    broadcaster.inner.watcher.only_generation_for_test(),
                    old_generation,
                    "reattachment inherited the old watcher generation"
                );
            }
        }

        let mut reattached = tokio::time::timeout(Duration::from_secs(5), reattach)
            .await
            .expect("reattachment completes after old lifecycle settlement")
            .expect("reattachment task joins")
            .expect("fresh status subscription starts");
        assert!(matches!(
            test_timeout("reattached event", reattached.recv()).await,
            Some(VcsStatusStreamEvent::Snapshot { .. })
        ));
        assert_ne!(
            broadcaster.inner.watcher.only_generation_for_test(),
            old_generation
        );
        assert_eq!(
            broadcaster.inner.watcher.only_health_for_test(),
            super::super::GitWatcherHealth::Healthy
        );
        test_timeout("broadcaster shutdown", broadcaster.shutdown()).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn subscribe_after_shutdown_is_rejected_without_recreating_state() {
        let sandbox = TestSandbox::new("git-broadcaster-closed-admission");
        let command = sandbox.executable_on_path("git");
        let (expected_git_config, environment) = isolated_git_environment(&sandbox);
        let repository = initialize_test_repository(&sandbox, &command, &environment);
        let (remote_started, _) = mpsc::unbounded_channel();
        let (remote_cancelled, _) = mpsc::unbounded_channel();
        let (local_status_started, _) = mpsc::unbounded_channel();
        let git = GitRepository::with_runner_for_test(Arc::new(BlockingRemoteGitRunner {
            command,
            environment,
            expected_git_config,
            remote_started,
            remote_cancelled,
            remote_outcome: None,
            local_status_started,
            watch_root_calls: AtomicUsize::new(0),
            local_status_calls: AtomicUsize::new(0),
            release_local_status: None,
            release_remote: Arc::new(Semaphore::new(16)),
        }));
        let broadcaster = StatusBroadcaster::new(Arc::new(git), Duration::from_secs(3_600), 4);

        broadcaster.shutdown().await;
        let result = broadcaster
            .subscribe(repository, CancellationToken::new())
            .await;

        assert!(result.is_err(), "closed broadcaster admitted a subscriber");
        assert_eq!(broadcaster.active_poller_count(), 0);
        assert_eq!(broadcaster.active_watcher_count_for_test(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_waits_for_an_inflight_root_resolution() {
        assert_shutdown_waits_for_subscription_stage(
            "GitVcsDriver.resolveWatchRoots",
            "git-broadcaster-shutdown-root-resolution",
        )
        .await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_waits_for_an_inflight_initial_status_read() {
        assert_shutdown_waits_for_subscription_stage(
            "GitVcsDriver.statusDetailsLocal.status",
            "git-broadcaster-shutdown-initial-status",
        )
        .await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_waits_for_an_inflight_watcher_setup() {
        let sandbox = TestSandbox::new("git-broadcaster-shutdown-watcher-setup");
        let command = sandbox.executable_on_path("git");
        let (_, environment) = isolated_git_environment(&sandbox);
        let repository =
            fs::canonicalize(initialize_test_repository(&sandbox, &command, &environment))
                .expect("canonical repository fixture");
        let git_dir = fs::canonicalize(repository.join(".git"))
            .expect("canonical repository metadata directory");
        let (initial_status_started, _) = mpsc::unbounded_channel();
        let runner = Arc::new(SubscriptionSetupGitRunner {
            roots: format!(
                "{}\n{}\n{}\n",
                repository.display(),
                git_dir.display(),
                git_dir.display()
            ),
            blocked_operation: "GitVcsDriver.statusDetailsLocal.status",
            started: initial_status_started,
            release: Arc::new(Semaphore::new(1)),
        });
        let (watcher, watcher_setup) = GitWatchService::blocked_during_setup_for_test();
        let watcher_shutdown = watcher.clone();
        let broadcaster = StatusBroadcaster::with_watcher_for_test(
            Arc::new(GitRepository::with_runner_for_test(runner)),
            watcher,
        );
        let subscribe_broadcaster = broadcaster.clone();
        let subscribe_cwd = repository.clone();
        let subscribe = tokio::spawn(async move {
            subscribe_broadcaster
                .subscribe(subscribe_cwd, CancellationToken::new())
                .await
        });
        watcher_setup.wait_until_entered().await;
        let shutdown_broadcaster = broadcaster.clone();
        let shutdown = tokio::spawn(async move { shutdown_broadcaster.shutdown().await });
        watcher_shutdown
            .wait_for_setup_cancellation_for_test()
            .await;
        assert!(
            !shutdown.is_finished(),
            "watcher shutdown waits for its synchronous setup"
        );
        watcher_setup.release();
        let result = subscribe.await.expect("subscription task joins");
        assert!(
            result.is_err(),
            "watcher shutdown was downgraded to fallback"
        );
        shutdown.await.expect("shutdown task joins");
        assert_eq!(broadcaster.active_poller_count(), 0);
        assert_eq!(broadcaster.active_watcher_count_for_test(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_after_existing_repository_registration_does_not_panic_the_subscriber() {
        let sandbox = TestSandbox::new("git-broadcaster-shutdown-existing-registration");
        let cwd = sandbox.path("repository");
        fs::create_dir_all(&cwd).expect("repository fixture");
        let cwd = fs::canonicalize(cwd).expect("canonical repository fixture");
        let (ref_started, _) = mpsc::unbounded_channel();
        let (remote_started, _) = mpsc::unbounded_channel();
        let runner = Arc::new(EpochGitRunner {
            branch: Mutex::new("main".to_owned()),
            ref_calls: AtomicUsize::new(0),
            remote_calls: AtomicUsize::new(0),
            ref_started,
            remote_started,
            release_ref: Arc::new(Semaphore::new(16)),
            release_remote: Arc::new(Semaphore::new(16)),
        });
        let broadcaster = StatusBroadcaster::new(
            Arc::new(GitRepository::with_runner_for_test(runner)),
            Duration::from_secs(3_600),
            4,
        );
        let mut existing = broadcaster
            .subscribe(cwd.clone(), CancellationToken::new())
            .await
            .expect("first subscriber starts");
        assert!(matches!(
            existing.recv().await,
            Some(VcsStatusStreamEvent::Snapshot { .. })
        ));
        broadcaster
            .lock_state()
            .repositories
            .get_mut(&cwd)
            .expect("existing repository")
            .repository_key = Some(cwd.clone());

        let gate = broadcaster.install_subscription_registration_gate_for_test();
        let second = {
            let broadcaster = broadcaster.clone();
            let cwd = cwd.clone();
            tokio::spawn(async move {
                broadcaster
                    .subscribe_inner(
                        cwd,
                        CancellationToken::new(),
                        CancellationToken::new(),
                        SubscriptionKind::Status,
                    )
                    .await
            })
        };
        gate.wait_until_entered().await;
        let setup_wait = broadcaster.inner.subscription_setup_wait_started.notified();
        tokio::pin!(setup_wait);
        setup_wait.as_mut().enable();
        let shutdown = {
            let broadcaster = broadcaster.clone();
            tokio::spawn(async move { broadcaster.shutdown().await })
        };
        setup_wait.await;
        gate.release();

        let result = second
            .await
            .expect("post-registration subscriber must not panic");
        assert_eq!(
            result
                .err()
                .expect("shutdown rejects the in-flight subscriber")
                .detail
                .as_ref(),
            "Git status broadcaster is shut down."
        );
        shutdown.await.expect("shutdown task joins");
        while existing.recv().await.is_some() {}
        assert_eq!(
            broadcaster.inner.fetch_owner.repository_count_for_test(),
            0,
            "post-registration shutdown must not attach a fetch owner"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shutdown_closes_a_first_lifecycle_tracker_before_registration_returns() {
        let sandbox = TestSandbox::new("git-broadcaster-shutdown-first-registration");
        let cwd = sandbox.path("repository");
        fs::create_dir_all(&cwd).expect("repository fixture");
        let cwd = fs::canonicalize(cwd).expect("canonical repository fixture");
        let (ref_started, _) = mpsc::unbounded_channel();
        let (remote_started, _) = mpsc::unbounded_channel();
        let runner = Arc::new(EpochGitRunner {
            branch: Mutex::new("main".to_owned()),
            ref_calls: AtomicUsize::new(0),
            remote_calls: AtomicUsize::new(0),
            ref_started,
            remote_started,
            release_ref: Arc::new(Semaphore::new(16)),
            release_remote: Arc::new(Semaphore::new(16)),
        });
        let broadcaster = StatusBroadcaster::new(
            Arc::new(GitRepository::with_runner_for_test(runner)),
            Duration::from_secs(3_600),
            4,
        );
        let gate = broadcaster.install_subscription_registration_gate_for_test();
        let subscription = {
            let broadcaster = broadcaster.clone();
            let cwd = cwd.clone();
            tokio::spawn(async move { broadcaster.subscribe(cwd, CancellationToken::new()).await })
        };
        gate.wait_until_entered().await;
        let setup_wait = broadcaster.inner.subscription_setup_wait_started.notified();
        tokio::pin!(setup_wait);
        setup_wait.as_mut().enable();
        let shutdown = {
            let broadcaster = broadcaster.clone();
            tokio::spawn(async move { broadcaster.shutdown().await })
        };
        setup_wait.await;

        if !gate.lifecycle_tracker_is_closed() {
            gate.release();
            let result = subscription.await.expect("subscription task joins");
            drop(result);
            gate.close_lifecycle_tracker();
            shutdown
                .await
                .expect("unfenced shutdown joins after cleanup");
            panic!("shutdown published an open first-lifecycle tracker for retirement");
        }

        gate.release();
        assert_eq!(
            subscription
                .await
                .expect("subscription task joins")
                .err()
                .expect("shutdown rejects the first subscriber")
                .detail
                .as_ref(),
            "Git status broadcaster is shut down."
        );
        shutdown.await.expect("shutdown task joins");
        assert_eq!(broadcaster.active_poller_count(), 0);
    }

    struct SetupAdmissionGitRunner {
        inner: EpochGitRunner,
        local_calls: AtomicUsize,
        fail_next_local_read: AtomicBool,
        local_read_gate: Mutex<Option<Arc<SubscribeAttemptGate>>>,
    }

    impl SetupAdmissionGitRunner {
        fn new(inner: EpochGitRunner) -> Self {
            Self {
                inner,
                local_calls: AtomicUsize::new(0),
                fail_next_local_read: AtomicBool::new(false),
                local_read_gate: Mutex::new(None),
            }
        }

        fn gate_local_read(&self) -> Arc<SubscribeAttemptGate> {
            let gate = Arc::new(SubscribeAttemptGate::default());
            *self.local_read_gate.lock().expect("local read gate lock") = Some(Arc::clone(&gate));
            gate
        }
    }

    impl GitProcessRunner for SetupAdmissionGitRunner {
        fn run<'a>(
            &'a self,
            request: ProcessRequest,
            cancellation: &'a CancellationToken,
        ) -> BoxGitProcessFuture<'a> {
            Box::pin(async move {
                if request.operation == "GitVcsDriver.statusDetailsLocal.status" {
                    self.local_calls.fetch_add(1, Ordering::SeqCst);
                    let gate = self
                        .local_read_gate
                        .lock()
                        .expect("local read gate lock")
                        .take();
                    if let Some(gate) = gate {
                        tokio::select! {
                            biased;
                            () = cancellation.cancelled() => {
                                return Err(ProcessError::Cancelled {
                                    operation: request.operation,
                                });
                            }
                            () = gate.block() => {}
                        }
                    }
                    if self.fail_next_local_read.swap(false, Ordering::SeqCst) {
                        return Err(ProcessError::NonZeroExit {
                            operation: request.operation,
                            exit_code: 1,
                            stdout_length: 0,
                            stderr_length: 24,
                            stdout: String::new().into_boxed_str(),
                            stderr: "controlled local failure".into(),
                        });
                    }
                }
                // Only the test's explicit local refreshes may fill the setup channel.
                if request.operation == "GitVcsDriver.statusDetailsRemote.status" {
                    cancellation.cancelled().await;
                    return Err(ProcessError::Cancelled {
                        operation: request.operation,
                    });
                }
                self.inner.run(request, cancellation).await
            })
        }
    }

    enum SetupAdmissionGate {
        BeforeAttachment(Arc<SubscriptionRegistrationGate>),
        AfterAttachment(Arc<LifecycleInsertionGate>),
    }

    impl SetupAdmissionGate {
        async fn wait(&self) {
            tokio::time::timeout(Duration::from_secs(5), async {
                match self {
                    Self::BeforeAttachment(gate) => gate.wait_until_entered().await,
                    Self::AfterAttachment(gate) => gate.wait_until_entered().await,
                }
            })
            .await
            .expect("subscription reaches the setup admission gate");
        }

        fn release(&self) {
            match self {
                Self::BeforeAttachment(gate) => gate.release(),
                Self::AfterAttachment(gate) => gate.release(),
            }
        }
    }

    struct SetupAdmissionFixture {
        _sandbox: TestSandbox,
        cwd: PathBuf,
        runner: Arc<SetupAdmissionGitRunner>,
        broadcaster: StatusBroadcaster,
        keeper: Option<GitManagerSignalSubscription>,
    }

    impl SetupAdmissionFixture {
        async fn new() -> Self {
            let sandbox = TestSandbox::new("git-broadcaster-setup-admission");
            let cwd = sandbox.path("repository");
            fs::create_dir_all(&cwd).expect("repository fixture");
            let cwd = fs::canonicalize(cwd).expect("canonical repository fixture");
            let (ref_started, _) = mpsc::unbounded_channel();
            let (remote_started, _) = mpsc::unbounded_channel();
            let runner = Arc::new(SetupAdmissionGitRunner::new(EpochGitRunner {
                branch: Mutex::new("main".to_owned()),
                ref_calls: AtomicUsize::new(0),
                remote_calls: AtomicUsize::new(0),
                ref_started,
                remote_started,
                release_ref: Arc::new(Semaphore::new(16)),
                release_remote: Arc::new(Semaphore::new(16)),
            }));
            let broadcaster = StatusBroadcaster::new(
                Arc::new(GitRepository::with_runner_for_test(runner.clone())),
                Duration::from_secs(3_600),
                2,
            );
            let keeper = broadcaster
                .subscribe_git_manager_signal(cwd.clone(), CancellationToken::new())
                .await
                .expect("keeper keeps the lifecycle alive without status backpressure");
            broadcaster
                .inner
                .fetch_owner
                .wait_for_worktree_count_for_test(1)
                .await;
            Self {
                _sandbox: sandbox,
                cwd,
                runner,
                broadcaster,
                keeper: Some(keeper),
            }
        }

        fn gate(&self, after_attachment: bool) -> SetupAdmissionGate {
            if after_attachment {
                SetupAdmissionGate::AfterAttachment(
                    self.broadcaster.install_lifecycle_insertion_gate_for_test(),
                )
            } else {
                SetupAdmissionGate::BeforeAttachment(
                    self.broadcaster
                        .install_subscription_registration_gate_for_test(),
                )
            }
        }

        fn subscribe(
            &self,
            cancellation: CancellationToken,
        ) -> tokio::task::JoinHandle<Result<StatusSubscription, GitCommandError>> {
            let broadcaster = self.broadcaster.clone();
            let cwd = self.cwd.clone();
            tokio::spawn(async move { broadcaster.subscribe(cwd, cancellation).await })
        }

        fn latest_subscriber_id(&self) -> u64 {
            self.broadcaster.lock_state().next_subscriber_id - 1
        }

        fn is_attached(&self, subscriber_id: u64) -> bool {
            self.broadcaster
                .inner
                .fetch_owner
                .has_subscriber_for_test(&self.cwd, subscriber_id)
        }

        async fn prune(&self) -> u64 {
            let subscriber_id = self.latest_subscriber_id();
            for branch in ["abandoned-event", "current"] {
                self.runner.inner.set_branch(branch);
                self.broadcaster
                    .refresh_local(&self.cwd, &CancellationToken::new())
                    .await
                    .expect("local update fills then prunes the setup subscriber");
            }
            assert!(
                !self.broadcaster.lock_state().repositories[&self.cwd]
                    .subscribers
                    .contains_key(&subscriber_id)
            );
            subscriber_id
        }

        async fn assert_fresh_stream(&self, subscription: &mut StatusSubscription) {
            assert!(matches!(
                subscription.recv().await,
                Some(VcsStatusStreamEvent::Snapshot { local, .. })
                    if local.ref_name.as_deref() == Some("current")
            ));
            assert!(
                matches!(
                    subscription.receiver.try_recv(),
                    Err(mpsc::error::TryRecvError::Empty)
                ),
                "abandoned snapshots and events must not reach the fresh stream"
            );
            self.runner.inner.set_branch("after-setup");
            self.broadcaster
                .refresh_local(&self.cwd, &CancellationToken::new())
                .await
                .expect("healthy stream refreshes");
            assert!(matches!(
                subscription.recv().await,
                Some(VcsStatusStreamEvent::LocalUpdated { local })
                    if local.ref_name.as_deref() == Some("after-setup")
            ));
        }
    }

    #[tokio::test]
    async fn live_status_backpressure_drains_before_terminal_error_and_preserves_peers() {
        let fixture = SetupAdmissionFixture::new().await;
        let mut slow = fixture
            .broadcaster
            .subscribe(fixture.cwd.clone(), CancellationToken::new())
            .await
            .expect("slow subscriber finishes setup");
        let mut healthy = fixture
            .broadcaster
            .subscribe(fixture.cwd.clone(), CancellationToken::new())
            .await
            .expect("healthy subscriber finishes setup");
        for subscription in [&mut slow, &mut healthy] {
            assert!(matches!(
                subscription.recv().await,
                Some(VcsStatusStreamEvent::Snapshot { .. })
            ));
        }

        for branch in ["queued-first", "queued-second", "pruned", "after-prune"] {
            fixture.runner.inner.set_branch(branch);
            fixture
                .broadcaster
                .refresh_local(&fixture.cwd, &CancellationToken::new())
                .await
                .expect("live local update");
            assert!(matches!(
                healthy.recv().await,
                Some(VcsStatusStreamEvent::LocalUpdated { local })
                    if local.ref_name.as_deref() == Some(branch)
            ));
        }
        assert!(
            !fixture.broadcaster.lock_state().repositories[&fixture.cwd]
                .subscribers
                .contains_key(&slow.subscriber_id)
        );
        for branch in ["queued-first", "queued-second"] {
            assert!(matches!(
                slow.recv_publication().await,
                Some(Ok(StatusPublication {
                    value: VcsStatusStreamEvent::LocalUpdated { local }, ..
                }))
                    if local.ref_name.as_deref() == Some(branch)
            ));
        }
        let Some(Err(error)) = slow.recv_publication().await else {
            panic!(
                "a pruned subscriber must receive a terminal error after its queued publications"
            );
        };
        assert_eq!(error.tag, "GitCommandError");
        assert_eq!(error.operation.as_ref(), "GitStatusBroadcaster.fellBehind");
        assert_eq!(error.command.as_ref(), "git");
        assert_eq!(error.cwd.as_ref(), fixture.cwd.to_string_lossy());
        assert_eq!(
            error.detail.as_ref(),
            "The Git status stream fell behind. Subscribe again."
        );
        assert!(slow.recv_publication().await.is_none());
        assert!(slow.recv_publication().await.is_none());
        fixture.broadcaster.shutdown().await;
    }

    #[tokio::test]
    async fn shutdown_status_subscription_ends_without_terminal_error() {
        let fixture = SetupAdmissionFixture::new().await;
        let mut subscription = fixture
            .broadcaster
            .subscribe(fixture.cwd.clone(), CancellationToken::new())
            .await
            .expect("status subscription");
        assert!(subscription.recv().await.is_some());
        fixture.broadcaster.shutdown().await;
        assert!(subscription.recv_publication().await.is_none());
        assert!(subscription.recv_publication().await.is_none());
    }

    #[tokio::test]
    async fn cancelled_status_subscription_ends_without_terminal_error() {
        let fixture = SetupAdmissionFixture::new().await;
        let cancellation = CancellationToken::new();
        let mut subscription = fixture
            .broadcaster
            .subscribe(fixture.cwd.clone(), cancellation.clone())
            .await
            .expect("status subscription");
        assert!(subscription.recv().await.is_some());
        cancellation.cancel();
        assert!(subscription.recv_publication().await.is_none());
        assert!(subscription.recv_publication().await.is_none());
        fixture.broadcaster.shutdown().await;
    }

    #[tokio::test]
    async fn released_status_subscription_ends_without_terminal_error() {
        let fixture = SetupAdmissionFixture::new().await;
        let mut subscription = fixture
            .broadcaster
            .subscribe(fixture.cwd.clone(), CancellationToken::new())
            .await
            .expect("status subscription");
        assert!(subscription.recv().await.is_some());
        fixture
            .broadcaster
            .release(&fixture.cwd, subscription.subscriber_id);
        assert!(subscription.recv_publication().await.is_none());
        fixture.broadcaster.shutdown().await;
    }

    async fn assert_setup_admission_retries(after_attachment: bool) {
        let fixture = SetupAdmissionFixture::new().await;
        let gate = fixture.gate(after_attachment);
        let subscribing = fixture.subscribe(CancellationToken::new());
        gate.wait().await;
        let abandoned_id = fixture.prune().await;
        assert_eq!(fixture.is_attached(abandoned_id), after_attachment);
        fixture
            .broadcaster
            .inner
            .subscription_registration_gate
            .lock()
            .expect("registration gate lock")
            .take();
        gate.release();
        let mut subscription = subscribing
            .await
            .expect("subscription task joins")
            .expect("backpressure during setup retries instead of reporting shutdown");
        assert_ne!(subscription.subscriber_id, abandoned_id);
        assert!(!fixture.is_attached(abandoned_id));
        assert!(fixture.is_attached(subscription.subscriber_id));
        fixture.assert_fresh_stream(&mut subscription).await;
        drop(subscription);
        fixture.broadcaster.shutdown().await;
    }

    #[tokio::test]
    async fn setup_admission_retries_before_fetch_attachment() {
        assert_setup_admission_retries(false).await;
    }

    #[tokio::test]
    async fn setup_admission_retries_after_fetch_attachment_without_leaking() {
        assert_setup_admission_retries(true).await;
    }

    #[tokio::test]
    async fn setup_retired_read_retries_after_final_subscriber_retires() {
        let mut fixture = SetupAdmissionFixture::new().await;
        let next_subscriber_id = fixture.broadcaster.lock_state().next_subscriber_id;
        let local_calls = fixture.runner.local_calls.load(Ordering::SeqCst);
        let gate = fixture.runner.gate_local_read();
        let subscribing = fixture.subscribe(CancellationToken::new());
        tokio::time::timeout(Duration::from_secs(5), gate.wait_until_entered())
            .await
            .expect("subscription's setup read is running");

        // Final release advances the epoch and cancels this physical read before
        // it can return, so read_local itself reports the retirement.
        fixture.runner.inner.set_branch("current");
        drop(fixture.keeper.take());
        assert_eq!(fixture.broadcaster.active_poller_count(), 0);

        let mut subscription = tokio::time::timeout(Duration::from_secs(5), subscribing)
            .await
            .expect("retired setup read retry settles")
            .expect("subscription task joins")
            .expect("a read retired during setup retries with a fresh snapshot");
        assert_eq!(
            fixture.runner.local_calls.load(Ordering::SeqCst),
            local_calls + 2
        );
        assert_eq!(
            subscription.subscriber_id, next_subscriber_id,
            "the retired read must not register a subscriber"
        );
        fixture.assert_fresh_stream(&mut subscription).await;
        drop(subscription);
        fixture.broadcaster.shutdown().await;
    }

    #[tokio::test]
    async fn setup_retired_read_exhaustion_reports_backpressure_without_registering() {
        let fixture = SetupAdmissionFixture::new().await;
        let next_subscriber_id = fixture.broadcaster.lock_state().next_subscriber_id;
        let local_calls = fixture.runner.local_calls.load(Ordering::SeqCst);
        let mut gate = fixture.runner.gate_local_read();
        let mut subscribing = fixture.subscribe(CancellationToken::new());
        for attempt in 1..=3 {
            tokio::time::timeout(Duration::from_secs(5), async {
                tokio::select! {
                    result = &mut subscribing => panic!(
                        "setup stopped before retired-read attempt {attempt}: {:?}",
                        result.expect("subscription task joins").err()
                    ),
                    () = gate.wait_until_entered() => {}
                }
            })
            .await
            .expect("each setup read starts before retirement");
            let next_gate = fixture.runner.gate_local_read();
            fixture
                .broadcaster
                .inner
                .status_owner
                .cancel_reads(&fixture.cwd);
            gate = next_gate;
        }

        let error = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                result = &mut subscribing => result
                    .expect("subscription task joins")
                    .err()
                    .expect("persistently retired setup reads have a bounded failure"),
                () = gate.wait_until_entered() => panic!("setup must stop after three retired reads"),
            }
        })
        .await
        .expect("retired setup reads exhaust their bound");
        assert_eq!(error.operation.as_ref(), "GitStatusBroadcaster.fellBehind");
        assert_eq!(
            error.detail.as_ref(),
            "The Git status subscription fell behind while it was being set up. Subscribe again."
        );
        assert_eq!(
            fixture.runner.local_calls.load(Ordering::SeqCst),
            local_calls + 3
        );
        assert_eq!(
            fixture.broadcaster.lock_state().next_subscriber_id,
            next_subscriber_id
        );
        assert_eq!(
            fixture
                .broadcaster
                .active_status_subscriber_count_for_test(),
            0
        );
        assert!(!fixture.is_attached(next_subscriber_id));
        fixture.broadcaster.shutdown().await;
    }

    #[tokio::test]
    async fn setup_git_read_failure_is_returned_without_retrying() {
        let fixture = SetupAdmissionFixture::new().await;
        let next_subscriber_id = fixture.broadcaster.lock_state().next_subscriber_id;
        let local_calls = fixture.runner.local_calls.load(Ordering::SeqCst);
        fixture
            .runner
            .fail_next_local_read
            .store(true, Ordering::SeqCst);

        let error = fixture
            .subscribe(CancellationToken::new())
            .await
            .expect("subscription task joins")
            .err()
            .expect("a genuine Git failure must be returned without retrying");
        assert_eq!(
            error.operation.as_ref(),
            "GitVcsDriver.statusDetailsLocal.status"
        );
        assert!(error.detail.contains("controlled local failure"));
        assert_eq!(
            fixture.runner.local_calls.load(Ordering::SeqCst),
            local_calls + 1
        );
        assert_eq!(
            fixture.broadcaster.lock_state().next_subscriber_id,
            next_subscriber_id
        );
        assert!(!fixture.is_attached(next_subscriber_id));
        fixture.broadcaster.shutdown().await;
    }

    #[tokio::test]
    async fn setup_stale_read_retries_after_final_subscriber_retires() {
        let mut fixture = SetupAdmissionFixture::new().await;
        let next_subscriber_id = fixture.broadcaster.lock_state().next_subscriber_id;
        let gate = fixture
            .broadcaster
            .install_post_initial_read_gate_for_test();
        let subscribing = fixture.subscribe(CancellationToken::new());
        tokio::time::timeout(Duration::from_secs(5), gate.wait_until_entered())
            .await
            .expect("subscription completes its read before registration");

        // Like dropping the retried first subscriber during reattachment, final
        // release retires the epoch after this read but before its publication.
        drop(fixture.keeper.take());
        assert_eq!(fixture.broadcaster.active_poller_count(), 0);
        fixture.runner.inner.set_branch("current");
        gate.release();

        let mut subscription = tokio::time::timeout(Duration::from_secs(5), subscribing)
            .await
            .expect("stale setup retry settles")
            .expect("subscription task joins")
            .expect("a read retired before registration retries with a fresh snapshot");
        assert_eq!(
            subscription.subscriber_id, next_subscriber_id,
            "the rejected publication must not register a subscriber"
        );
        fixture.assert_fresh_stream(&mut subscription).await;
        drop(subscription);
        fixture.broadcaster.shutdown().await;
    }

    #[tokio::test]
    async fn setup_stale_read_exhaustion_reports_backpressure_without_registering() {
        let fixture = SetupAdmissionFixture::new().await;
        let next_subscriber_id = fixture.broadcaster.lock_state().next_subscriber_id;
        let mut gate = fixture
            .broadcaster
            .install_post_initial_read_gate_for_test();
        let mut subscribing = fixture.subscribe(CancellationToken::new());
        for attempt in 1..=3 {
            tokio::time::timeout(Duration::from_secs(5), async {
                tokio::select! {
                    result = &mut subscribing => panic!(
                        "setup stopped before stale-read attempt {attempt}: {:?}",
                        result.expect("subscription task joins").err()
                    ),
                    () = gate.wait_until_entered() => {}
                }
            })
            .await
            .expect("subscription completes each read before registration");
            assert_eq!(
                fixture.broadcaster.lock_state().next_subscriber_id,
                next_subscriber_id
            );
            fixture
                .broadcaster
                .begin_mutation(&fixture.cwd)
                .await
                .finish()
                .await;
            let next_gate = fixture
                .broadcaster
                .install_post_initial_read_gate_for_test();
            gate.release();
            gate = next_gate;
        }

        let error = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                result = &mut subscribing => result
                    .expect("subscription task joins")
                    .err()
                    .expect("persistently stale setup has a bounded failure"),
                () = gate.wait_until_entered() => panic!("setup must stop after three stale reads"),
            }
        })
        .await
        .expect("stale setup exhausts its bound");
        assert_eq!(error.operation.as_ref(), "GitStatusBroadcaster.fellBehind");
        assert_eq!(
            error.detail.as_ref(),
            "The Git status subscription fell behind while it was being set up. Subscribe again."
        );
        assert_eq!(
            fixture.broadcaster.lock_state().next_subscriber_id,
            next_subscriber_id
        );
        assert_eq!(
            fixture
                .broadcaster
                .active_status_subscriber_count_for_test(),
            0
        );
        assert_eq!(
            fixture
                .broadcaster
                .inner
                .fetch_owner
                .worktree_count_for_test(),
            1
        );
        fixture.broadcaster.shutdown().await;
    }

    #[tokio::test]
    async fn setup_admission_exhaustion_reports_backpressure_and_detaches_every_attempt() {
        let fixture = SetupAdmissionFixture::new().await;
        let mut gate = fixture.gate(true);
        let subscribing = fixture.subscribe(CancellationToken::new());
        let mut abandoned = Vec::new();
        for attempt in 0..3 {
            gate.wait().await;
            for &subscriber_id in &abandoned {
                assert!(!fixture.is_attached(subscriber_id));
            }
            let subscriber_id = fixture.prune().await;
            assert!(fixture.is_attached(subscriber_id));
            abandoned.push(subscriber_id);
            let next_gate = (attempt < 2).then(|| fixture.gate(true));
            gate.release();
            if let Some(next_gate) = next_gate {
                gate = next_gate;
            }
        }
        let error = subscribing
            .await
            .expect("subscription task joins")
            .err()
            .expect("persistently backpressured setup has a bounded failure");
        assert_eq!(error.operation.as_ref(), "GitStatusBroadcaster.fellBehind");
        assert_eq!(
            error.detail.as_ref(),
            "The Git status subscription fell behind while it was being set up. Subscribe again."
        );
        assert_eq!(fixture.broadcaster.lock_state().next_subscriber_id, 4);
        assert_eq!(
            fixture.broadcaster.lock_state().repositories[&fixture.cwd]
                .subscribers
                .len(),
            1
        );
        for subscriber_id in abandoned {
            assert!(!fixture.is_attached(subscriber_id));
        }
        fixture.broadcaster.shutdown().await;
    }

    async fn assert_setup_admission_retries_a_superseded_lifecycle(kind: SubscriptionKind) {
        let mut fixture = SetupAdmissionFixture::new().await;
        let gate = fixture.gate(true);
        let subscribing = {
            let broadcaster = fixture.broadcaster.clone();
            let cwd = fixture.cwd.clone();
            tokio::spawn(async move {
                broadcaster
                    .subscribe_kind(cwd, CancellationToken::new(), kind)
                    .await
            })
        };
        gate.wait().await;
        let abandoned_id = fixture.latest_subscriber_id();
        let old_lifecycle =
            fixture.broadcaster.lock_state().repositories[&fixture.cwd].lifecycle_id;
        fixture.broadcaster.release(&fixture.cwd, abandoned_id);
        drop(fixture.keeper.take());
        fixture.keeper = Some(
            fixture
                .broadcaster
                .subscribe_git_manager_signal(fixture.cwd.clone(), CancellationToken::new())
                .await
                .expect("replacement lifecycle starts"),
        );
        let new_lifecycle =
            fixture.broadcaster.lock_state().repositories[&fixture.cwd].lifecycle_id;
        assert_ne!(new_lifecycle, old_lifecycle);
        gate.release();
        let mut subscription = subscribing
            .await
            .expect("subscription task joins")
            .expect("superseded setup retries in the current lifecycle");
        assert!(!fixture.is_attached(abandoned_id));
        assert_eq!(
            fixture.broadcaster.lock_state().repositories[&fixture.cwd].lifecycle_id,
            new_lifecycle
        );
        match &mut subscription {
            BroadcasterSubscription::Status(subscription) => assert!(matches!(
                subscription.recv().await,
                Some(VcsStatusStreamEvent::Snapshot { .. })
            )),
            BroadcasterSubscription::GitManager(subscription) => {
                assert!(subscription.recv().await.is_some());
            }
        }
        drop(subscription);
        fixture.broadcaster.shutdown().await;
    }

    #[tokio::test]
    async fn setup_admission_retries_a_superseded_lifecycle() {
        assert_setup_admission_retries_a_superseded_lifecycle(SubscriptionKind::Status).await;
    }

    #[tokio::test]
    async fn setup_admission_retries_a_superseded_git_manager_lifecycle() {
        assert_setup_admission_retries_a_superseded_lifecycle(SubscriptionKind::GitManager).await;
    }

    async fn assert_setup_admission_cancellation(drop_caller: bool) {
        let mut fixture = SetupAdmissionFixture::new().await;
        let cancellation = CancellationToken::new();
        let first_gate = fixture.gate(true);
        let subscribing = fixture.subscribe(cancellation.clone());
        first_gate.wait().await;
        let first_id = fixture.prune().await;
        let second_gate = fixture.gate(true);
        first_gate.release();
        second_gate.wait().await;
        let second_id = fixture.latest_subscriber_id();
        assert!(fixture.is_attached(second_id));
        if drop_caller {
            subscribing.abort();
            assert!(matches!(subscribing.await, Err(error) if error.is_cancelled()));
        } else {
            cancellation.cancel();
            assert!(subscribing.await.expect("cancelled caller joins").is_err());
        }
        fixture.broadcaster.inner.subscription_setups.close();
        tokio::time::timeout(
            Duration::from_secs(5),
            fixture.broadcaster.inner.subscription_setups.wait(),
        )
        .await
        .expect("caller cancellation stops setup without releasing its gate");
        assert_eq!(
            fixture.latest_subscriber_id(),
            second_id,
            "cancellation starts no further attempt"
        );
        assert!(!fixture.is_attached(first_id));
        assert!(!fixture.is_attached(second_id));
        drop(fixture.keeper.take());
        assert!(fixture.broadcaster.lock_state().repositories.is_empty());
        assert_eq!(
            fixture
                .broadcaster
                .inner
                .fetch_owner
                .worktree_count_for_test(),
            0
        );
        fixture.broadcaster.shutdown().await;
    }

    #[tokio::test]
    async fn setup_admission_cancellation_during_retry_cleans_up() {
        assert_setup_admission_cancellation(false).await;
    }

    #[tokio::test]
    async fn setup_admission_dropped_caller_during_retry_cleans_up() {
        assert_setup_admission_cancellation(true).await;
    }

    async fn assert_setup_admission_shutdown_wins(failed_attempts: usize) {
        let fixture = SetupAdmissionFixture::new().await;
        let mut gate = fixture.gate(true);
        let subscribing = fixture.subscribe(CancellationToken::new());
        for _ in 1..failed_attempts {
            gate.wait().await;
            fixture.prune().await;
            let next_gate = fixture.gate(true);
            gate.release();
            gate = next_gate;
        }
        gate.wait().await;
        let abandoned_id = fixture.prune().await;
        let between_retries = fixture
            .broadcaster
            .install_post_retirement_wait_gate_for_test();
        gate.release();
        tokio::time::timeout(Duration::from_secs(5), between_retries.wait_until_entered())
            .await
            .expect("retry reaches the boundary before registration");
        assert!(!fixture.is_attached(abandoned_id));
        let shutdown = {
            let broadcaster = fixture.broadcaster.clone();
            tokio::spawn(async move { broadcaster.shutdown().await })
        };
        fixture
            .broadcaster
            .wait_for_subscription_setup_wait_started_for_test()
            .await;
        between_retries.release();
        let error = subscribing
            .await
            .expect("subscription task joins")
            .err()
            .expect("shutdown rejects the pending retry");
        assert_eq!(
            error.detail.as_ref(),
            "Git status broadcaster is shut down."
        );
        assert_eq!(
            fixture.latest_subscriber_id(),
            abandoned_id,
            "shutdown starts no further attempt"
        );
        shutdown.await.expect("shutdown joins");
        assert!(fixture.broadcaster.lock_state().repositories.is_empty());
        assert_eq!(
            fixture
                .broadcaster
                .inner
                .fetch_owner
                .worktree_count_for_test(),
            0
        );
    }

    #[tokio::test]
    async fn setup_admission_shutdown_between_retries_wins() {
        assert_setup_admission_shutdown_wins(1).await;
    }

    #[tokio::test]
    async fn setup_admission_shutdown_at_the_retry_bound_wins() {
        assert_setup_admission_shutdown_wins(3).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pre_admission_backpressure_retirement_keeps_an_epoch_fence() {
        let sandbox = TestSandbox::new("git-broadcaster-pre-admission-retirement");
        let cwd = sandbox.path("repository");
        fs::create_dir_all(&cwd).expect("repository fixture");
        let cwd = fs::canonicalize(cwd).expect("canonical repository fixture");
        let (ref_started, _) = mpsc::unbounded_channel();
        let (remote_started, _) = mpsc::unbounded_channel();
        let runner = Arc::new(SetupAdmissionGitRunner::new(EpochGitRunner {
            branch: Mutex::new("main".to_owned()),
            ref_calls: AtomicUsize::new(0),
            remote_calls: AtomicUsize::new(0),
            ref_started,
            remote_started,
            release_ref: Arc::new(Semaphore::new(16)),
            release_remote: Arc::new(Semaphore::new(16)),
        }));
        let broadcaster = StatusBroadcaster::new(
            Arc::new(GitRepository::with_runner_for_test(runner)),
            Duration::from_secs(3_600),
            1,
        );
        let registration_gate = broadcaster.install_subscription_registration_gate_for_test();
        let subscription = {
            let broadcaster = broadcaster.clone();
            let cwd = cwd.clone();
            tokio::spawn(async move { broadcaster.subscribe(cwd, CancellationToken::new()).await })
        };
        registration_gate.wait_until_entered().await;
        broadcaster
            .inner
            .subscription_registration_gate
            .lock()
            .expect("registration gate lock")
            .take();
        let old_fence = broadcaster
            .acquire_read_fence(&cwd, &CancellationToken::new())
            .await
            .expect("pre-admission lifecycle fence");
        let mut local = VcsStatusLocalResult::non_repository();
        local.is_repo = true;
        local.ref_name = Some("main".to_owned());
        let status = VcsStatusResult {
            local,
            remote: VcsStatusRemoteResult {
                has_upstream: false,
                ahead_count: 0,
                behind_count: 0,
                ahead_of_default_count: Some(0),
                pr: None,
            },
        };
        let retirement = broadcaster
            .publish_if_fence_current(&old_fence, || {
                broadcaster.publish_status(&cwd, &status, &old_fence)
            })
            .expect("pre-admission publication fence is current");
        let retirement_gate = broadcaster.install_retirement_epoch_gate_for_test();
        let finish = {
            let broadcaster = broadcaster.clone();
            let cwd = cwd.clone();
            tokio::task::spawn_blocking(move || {
                broadcaster.finish_repository_retirement(&cwd, retirement);
            })
        };
        retirement_gate.wait_until_entered().await;

        if !broadcaster.retiring_tracker_is_nonempty_for_test(&cwd) {
            retirement_gate.release();
            finish.await.expect("unfenced retirement joins");
            registration_gate.release();
            let result = subscription.await.expect("subscription task joins");
            drop(result);
            broadcaster.shutdown().await;
            panic!("pre-admission retirement exposed an empty epoch tracker");
        }

        retirement_gate.release();
        finish.await.expect("retirement joins");
        registration_gate.release();
        let mut subscription = subscription
            .await
            .expect("subscription task joins")
            .expect("backpressure-pruned first subscriber retries after retirement");
        assert!(matches!(
            subscription.recv().await,
            Some(VcsStatusStreamEvent::Snapshot { .. })
        ));
        assert!(
            broadcaster
                .publish_if_fence_current(&old_fence, || ())
                .is_err()
        );
        drop(subscription);
        broadcaster.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn post_admission_retirement_waits_for_lifecycle_task_insertion() {
        struct GatedRemoteGitRunner {
            inner: EpochGitRunner,
            release_remote: Arc<Semaphore>,
        }

        impl GitProcessRunner for GatedRemoteGitRunner {
            fn run<'a>(
                &'a self,
                request: ProcessRequest,
                cancellation: &'a CancellationToken,
            ) -> BoxGitProcessFuture<'a> {
                Box::pin(async move {
                    if request.operation == "GitVcsDriver.statusDetailsRemote.status" {
                        tokio::select! {
                            biased;
                            () = cancellation.cancelled() => {
                                return Err(ProcessError::Cancelled {
                                    operation: request.operation,
                                });
                            }
                            permit = self.release_remote.acquire() => {
                                permit.expect("remote publication gate remains open").forget();
                            }
                        }
                    }
                    self.inner.run(request, cancellation).await
                })
            }
        }

        let sandbox = TestSandbox::new("git-broadcaster-post-admission-retirement");
        let cwd = sandbox.path("repository");
        fs::create_dir_all(&cwd).expect("repository fixture");
        let cwd = fs::canonicalize(cwd).expect("canonical repository fixture");
        let (ref_started, _) = mpsc::unbounded_channel();
        let (remote_started, _) = mpsc::unbounded_channel();
        // Only the explicit publication below should backpressure this one-slot
        // stream. Hold remote updates until the replacement consumes its snapshot;
        // cancellation lets the retired lifecycle settle without a gate permit.
        let release_remote = Arc::new(Semaphore::new(0));
        let runner = Arc::new(GatedRemoteGitRunner {
            inner: EpochGitRunner {
                branch: Mutex::new("main".to_owned()),
                ref_calls: AtomicUsize::new(0),
                remote_calls: AtomicUsize::new(0),
                ref_started,
                remote_started,
                release_ref: Arc::new(Semaphore::new(16)),
                release_remote: Arc::new(Semaphore::new(16)),
            },
            release_remote: Arc::clone(&release_remote),
        });
        let broadcaster = StatusBroadcaster::new(
            Arc::new(GitRepository::with_runner_for_test(runner)),
            Duration::from_secs(3_600),
            1,
        );
        let insertion_gate = broadcaster.install_lifecycle_insertion_gate_for_test();
        let first = {
            let broadcaster = broadcaster.clone();
            let cwd = cwd.clone();
            tokio::spawn(async move { broadcaster.subscribe(cwd, CancellationToken::new()).await })
        };
        insertion_gate.wait_until_entered().await;
        let old_fence = broadcaster
            .acquire_read_fence(&cwd, &CancellationToken::new())
            .await
            .expect("post-admission lifecycle fence");
        let mut local = VcsStatusLocalResult::non_repository();
        local.is_repo = true;
        local.ref_name = Some("main".to_owned());
        let status = VcsStatusResult {
            local,
            remote: VcsStatusRemoteResult {
                has_upstream: false,
                ahead_count: 0,
                behind_count: 0,
                ahead_of_default_count: Some(0),
                pr: None,
            },
        };
        let retirement = broadcaster
            .publish_if_fence_current(&old_fence, || {
                broadcaster.publish_status(&cwd, &status, &old_fence)
            })
            .expect("post-admission publication fence is current");
        let sentinel_finished = broadcaster.inner.retirement_sentinel_finished.notified();
        tokio::pin!(sentinel_finished);
        sentinel_finished.as_mut().enable();
        broadcaster.finish_repository_retirement(&cwd, retirement);
        sentinel_finished.await;

        if !insertion_gate.lifecycle_tracker_is_nonempty() {
            insertion_gate.release();
            let result = first.await.expect("unreserved first subscription joins");
            drop(result);
            broadcaster.shutdown().await;
            panic!("post-admission retirement lost lifecycle task-insertion ownership");
        }

        let retirement_baseline = broadcaster.retirement_wait_count_for_test();
        let reattaching = {
            let broadcaster = broadcaster.clone();
            let cwd = cwd.clone();
            tokio::spawn(async move { broadcaster.subscribe(cwd, CancellationToken::new()).await })
        };
        tokio::time::timeout(
            Duration::from_secs(5),
            broadcaster.wait_for_retirement_wait_started_for_test(retirement_baseline),
        )
        .await
        .expect("reattachment reaches the old lifecycle retirement wait");
        assert!(
            !reattaching.is_finished(),
            "reattachment remains fenced until lifecycle task insertion finishes"
        );

        insertion_gate.release();
        let mut first = first
            .await
            .expect("first subscription task joins")
            .expect("backpressure-pruned first subscriber retries after insertion settles");
        assert!(matches!(
            first.recv().await,
            Some(VcsStatusStreamEvent::Snapshot { .. })
        ));
        drop(first);
        let mut reattached = reattaching
            .await
            .expect("reattachment task joins")
            .expect("reattachment succeeds after old insertion settles");
        assert!(matches!(
            reattached.recv().await,
            Some(VcsStatusStreamEvent::Snapshot { .. })
        ));
        release_remote.add_permits(1);
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(5), reattached.recv())
                .await
                .expect("remote update follows the consumed snapshot"),
            Some(VcsStatusStreamEvent::RemoteUpdated { .. })
        ));
        drop(reattached);
        broadcaster.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn registration_retries_when_retirement_starts_after_the_initial_wait() {
        let _native_watcher_permit = super::super::acquire_native_watcher_test_permit().await;
        let sandbox = TestSandbox::new("git-broadcaster-registration-retirement-toctou");
        let cwd = sandbox.path("repository");
        fs::create_dir_all(&cwd).expect("repository fixture");
        let cwd = fs::canonicalize(cwd).expect("canonical repository fixture");
        let git_dir = cwd.join(".git");
        fs::create_dir_all(&git_dir).expect("repository metadata fixture");
        let (started, _) = mpsc::unbounded_channel();
        let runner = Arc::new(SubscriptionSetupGitRunner {
            roots: format!(
                "{}\n{}\n{}\n",
                cwd.display(),
                git_dir.display(),
                git_dir.display()
            ),
            blocked_operation: "",
            started,
            release: Arc::new(Semaphore::new(16)),
        });
        let broadcaster = StatusBroadcaster::new(
            Arc::new(GitRepository::with_runner_for_test(runner)),
            Duration::from_secs(3_600),
            4,
        );
        let mut first = test_timeout(
            "status subscription setup",
            broadcaster.subscribe(cwd.clone(), CancellationToken::new()),
        )
        .await
        .expect("first lifecycle starts");
        assert!(matches!(
            test_timeout("first event", first.recv()).await,
            Some(VcsStatusStreamEvent::Snapshot { .. })
        ));
        let old_watcher_generation = broadcaster.inner.watcher.only_generation_for_test();
        let held_task_release = Arc::new(Semaphore::new(0));
        let (cancellation_observed, mut cancellation_observed_rx) = mpsc::unbounded_channel();
        broadcaster.hold_lifecycle_task_until_released_for_test(
            &cwd,
            Arc::clone(&held_task_release),
            cancellation_observed,
        );
        let post_wait_gate = broadcaster.install_post_retirement_wait_gate_for_test();
        let registration_probe = broadcaster.install_registration_outcome_probe_for_test();
        let subscribing = {
            let broadcaster = broadcaster.clone();
            let cwd = cwd.clone();
            tokio::spawn(async move {
                test_timeout(
                    "status subscription setup",
                    broadcaster.subscribe(cwd, CancellationToken::new()),
                )
                .await
            })
        };
        post_wait_gate.wait_until_entered().await;

        drop(first);
        test_timeout(
            "cancellation_observed_rx event",
            cancellation_observed_rx.recv(),
        )
        .await
        .expect("old lifecycle cancellation reaches its held task");
        post_wait_gate.release();
        let retried = registration_probe.wait().await;
        if !retried {
            held_task_release.add_permits(1);
            let result = test_timeout("subscribing task completion", subscribing)
                .await
                .expect("unfenced subscription joins");
            drop(result);
            test_timeout("broadcaster shutdown", broadcaster.shutdown()).await;
            panic!("registration created a lifecycle while old retirement was pending");
        }
        assert!(
            !subscribing.is_finished(),
            "retry waits for old lifecycle task settlement"
        );

        held_task_release.add_permits(1);
        let mut subscription = test_timeout("subscribing task completion", subscribing)
            .await
            .expect("retrying subscription task joins")
            .expect("subscription succeeds after retirement settles");
        assert!(matches!(
            test_timeout("subscription event", subscription.recv()).await,
            Some(VcsStatusStreamEvent::Snapshot { .. })
        ));
        assert_ne!(
            broadcaster.inner.watcher.only_generation_for_test(),
            old_watcher_generation,
            "retry installs a fresh watcher generation"
        );
        assert_eq!(
            broadcaster
                .lock_state()
                .repositories
                .get(&cwd)
                .expect("fresh lifecycle remains registered")
                .subscribers
                .len(),
            1,
            "retry admits exactly the final subscriber"
        );
        drop(subscription);
        test_timeout("broadcaster shutdown", broadcaster.shutdown()).await;
    }

    async fn assert_shutdown_waits_for_subscription_stage(
        blocked_operation: &'static str,
        sandbox_name: &str,
    ) {
        let _native_watcher_permit = super::super::acquire_native_watcher_test_permit().await;
        let sandbox = TestSandbox::new(sandbox_name);
        let command = sandbox.executable_on_path("git");
        let (_, environment) = isolated_git_environment(&sandbox);
        let repository =
            fs::canonicalize(initialize_test_repository(&sandbox, &command, &environment))
                .expect("canonical repository fixture");
        let git_dir = fs::canonicalize(repository.join(".git"))
            .expect("canonical repository metadata directory");
        let (started, mut started_rx) = mpsc::unbounded_channel();
        let release = Arc::new(Semaphore::new(0));
        let runner = Arc::new(SubscriptionSetupGitRunner {
            roots: format!(
                "{}\n{}\n{}\n",
                repository.display(),
                git_dir.display(),
                git_dir.display()
            ),
            blocked_operation,
            started,
            release: Arc::clone(&release),
        });
        let broadcaster = StatusBroadcaster::new(
            Arc::new(GitRepository::with_runner_for_test(runner)),
            Duration::from_secs(3_600),
            4,
        );
        let subscribe_broadcaster = broadcaster.clone();
        let subscribe_cwd = repository.clone();
        let subscribe = tokio::spawn(async move {
            test_timeout(
                "status subscription setup",
                subscribe_broadcaster.subscribe(subscribe_cwd, CancellationToken::new()),
            )
            .await
        });
        test_timeout("started_rx event", started_rx.recv())
            .await
            .expect("subscription setup reaches the blocked stage");
        let shutdown_broadcaster = broadcaster.clone();
        let mut shutdown = tokio::spawn(async move {
            test_timeout("broadcaster shutdown", shutdown_broadcaster.shutdown()).await
        });

        tokio::select! {
            biased;
            () = broadcaster.wait_for_subscription_setup_wait_started_for_test() => {
                assert!(
                    !shutdown.is_finished(),
                    "shutdown remains pending while subscription setup is held"
                );
            }
            joined = &mut shutdown => {
                joined.expect("shutdown task joins");
                release.add_permits(1);
                let result = test_timeout("subscribe task completion", subscribe).await.expect("subscription task joins");
                drop(result);
                panic!("shutdown completed before the in-flight {blocked_operation} settled");
            }
        }

        release.add_permits(1);
        let result = test_timeout("subscribe task completion", subscribe)
            .await
            .expect("subscription task joins");
        assert!(
            result.is_err(),
            "subscription completed after shutdown began at {blocked_operation}"
        );
        test_timeout("shutdown task completion", shutdown)
            .await
            .expect("shutdown task joins");
        assert_eq!(broadcaster.active_poller_count(), 0);
        assert_eq!(broadcaster.active_watcher_count_for_test(), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn local_invalidation_starts_while_remote_refresh_is_blocked() {
        let sandbox = TestSandbox::new("git-broadcaster-invalidation");
        let command = sandbox.executable_on_path("git");
        let (expected_git_config, environment) = isolated_git_environment(&sandbox);
        let repository = initialize_test_repository(&sandbox, &command, &environment);
        let canonical_repository =
            fs::canonicalize(&repository).expect("canonical repository fixture");
        let (remote_started, mut remote_started_rx) = mpsc::unbounded_channel();
        let (remote_cancelled, _) = mpsc::unbounded_channel();
        let (remote_outcome, mut remote_outcome_rx) = mpsc::unbounded_channel();
        let (local_status_started, mut local_status_started_rx) = mpsc::unbounded_channel();
        let release_remote = Arc::new(Semaphore::new(0));
        let git = GitRepository::with_runner_for_test(Arc::new(BlockingRemoteGitRunner {
            command,
            environment,
            expected_git_config,
            remote_started,
            remote_cancelled,
            remote_outcome: Some(remote_outcome),
            local_status_started,
            watch_root_calls: AtomicUsize::new(0),
            local_status_calls: AtomicUsize::new(0),
            release_local_status: None,
            release_remote: release_remote.clone(),
        }));
        let broadcaster = StatusBroadcaster::new(Arc::new(git), Duration::from_secs(30), 4);
        let mut subscription = broadcaster
            .subscribe(repository.clone(), CancellationToken::new())
            .await
            .expect("status subscription starts");
        // Every wait here is bounded: on a loaded CI runner this test once sat
        // on an unbounded `recv()` until the job's 60-minute timeout cancelled
        // the whole preflight instead of failing with a message.
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(30), subscription.recv())
                .await
                .expect("initial snapshot arrives"),
            Some(VcsStatusStreamEvent::Snapshot { ref local, .. })
                if !local.has_working_tree_changes
        ));
        tokio::time::timeout(Duration::from_secs(30), local_status_started_rx.recv())
            .await
            .expect("initial local status scan starts")
            .expect("initial local status scan was observed");
        tokio::time::timeout(Duration::from_secs(5), remote_started_rx.recv())
            .await
            .expect("initial remote status scan starts")
            .expect("remote status checkpoint owner remains alive");
        let remote_owner_cancellation = broadcaster
            .lock_state()
            .repositories
            .get(&canonical_repository)
            .expect("active repository lifecycle remains registered")
            .poller_cancellation
            .clone();
        assert!(!remote_owner_cancellation.is_cancelled());

        fs::write(repository.join("tracked.txt"), "changed in editor\n")
            .expect("mutate tracked fixture file");
        broadcaster.notify_local_change(&repository).await;

        tokio::time::timeout(Duration::from_secs(5), local_status_started_rx.recv())
            .await
            .expect("local invalidation remained blocked behind remote refresh")
            .expect("local status checkpoint owner remains alive");
        let event = tokio::time::timeout(Duration::from_secs(5), subscription.recv())
            .await
            .expect("dirty local status is published while remote refresh remains blocked")
            .expect("status subscription remains open");
        assert!(matches!(
            event,
            VcsStatusStreamEvent::LocalUpdated { local }
                if local.has_working_tree_changes
                    && local.working_tree.files.iter().any(|file| file.path == "tracked.txt")
        ));

        drop(subscription);
        assert_eq!(broadcaster.active_poller_count(), 0);
        assert!(
            remote_owner_cancellation.is_cancelled(),
            "final subscriber drop cancels the remote lifecycle synchronously"
        );
        // Make the runner's release branch ready only after the production
        // owner has cancelled its lifecycle. Its biased select must still
        // observe cancellation first. Awaiting the runner's explicit terminal
        // branch avoids sampling a nested callback before it has published.
        release_remote.add_permits(1);
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(30), remote_outcome_rx.recv())
                .await
                .expect("blocked remote runner reaches its terminal branch after cancellation")
                .expect("blocked remote runner reports its terminal branch"),
            BlockingRemoteOutcome::Cancelled,
            "final subscriber cancellation wins over the ready release permit"
        );
        tokio::time::timeout(
            Duration::from_secs(30),
            broadcaster.await_retired_lifecycle(&canonical_repository),
        )
        .await
        .expect("retired lifecycle tasks report completion after the final subscriber drops");
    }

    #[tokio::test(start_paused = true)]
    async fn ref_poll_is_replaced_by_watcher_and_safety_status_reads() {
        let _watcher_permit = super::super::acquire_native_watcher_test_permit().await;
        let sandbox = TestSandbox::new("git-broadcaster-no-ref-poll");
        let worktree = sandbox.path("repository");
        let git_dir = worktree.join(".git");
        let refs = git_dir.join("refs");
        fs::create_dir_all(&refs).expect("watchable Git roots");
        let worktree = fs::canonicalize(worktree).expect("canonical worktree");
        let git_dir = fs::canonicalize(git_dir).expect("canonical Git directory");
        let runner = Arc::new(IdleObservationGitRunner {
            roots: format!(
                "{}\n{}\n{}\n",
                worktree.display(),
                git_dir.display(),
                git_dir.display()
            ),
            common_dir: git_dir,
            operations: Mutex::new(Vec::new()),
            operation_changed: Notify::new(),
        });
        let (automatic_fetch_interval, _) = watch::channel(Duration::ZERO);
        let broadcaster = StatusBroadcaster::with_automatic_remote_refresh_interval(
            Arc::new(GitRepository::with_runner_for_test(runner.clone())),
            Duration::from_secs(60),
            automatic_fetch_interval,
            4,
        );
        let mut subscription = test_timeout(
            "status subscription setup",
            broadcaster.subscribe(worktree, CancellationToken::new()),
        )
        .await
        .expect("status subscription");
        assert!(matches!(
            test_timeout("subscription event", subscription.recv()).await,
            Some(VcsStatusStreamEvent::Snapshot { .. })
        ));
        runner
            .wait_for_operation_count("GitVcsDriver.statusDetailsRemote.status", 1)
            .await;
        let initial_local_reads = runner.operation_count("GitVcsDriver.statusDetailsLocal.status");
        assert_eq!(
            initial_local_reads, 1,
            "subscription performs one initial read"
        );

        tokio::time::advance(Duration::from_secs(59)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            runner.operation_count("GitVcsDriver.statusDetailsLocal.status"),
            initial_local_reads,
            "idle time before the safety deadline starts no status Git"
        );
        assert_eq!(
            runner.operation_count("GitVcsDriver.currentRef"),
            0,
            "idle ownership starts no periodic symbolic-ref Git"
        );

        tokio::time::advance(Duration::from_secs(1)).await;
        runner
            .wait_for_operation_count(
                "GitVcsDriver.statusDetailsLocal.status",
                initial_local_reads + 1,
            )
            .await;
        assert_eq!(
            runner.operation_count("GitVcsDriver.statusDetailsLocal.status"),
            initial_local_reads + 1,
            "the safety deadline starts exactly one local status read"
        );
        assert_eq!(
            runner.operation_count("GitVcsDriver.currentRef"),
            0,
            "the safety read does not start symbolic-ref Git"
        );

        drop(subscription);
        test_timeout("broadcaster shutdown", broadcaster.shutdown()).await;
    }

    #[tokio::test(start_paused = true)]
    async fn linked_worktrees_share_one_fetch_and_reconcile_their_own_branches() {
        let sandbox = TestSandbox::new("git-broadcaster-shared-fetch");
        let main = sandbox.path("main");
        let feature = sandbox.path("feature");
        let common_dir = sandbox.path("common.git");
        fs::create_dir_all(&main).expect("main worktree fixture");
        fs::create_dir_all(&feature).expect("feature worktree fixture");
        fs::create_dir_all(&common_dir).expect("common directory fixture");
        let main = fs::canonicalize(main).expect("canonical main worktree");
        let feature = fs::canonicalize(feature).expect("canonical feature worktree");
        let runner = Arc::new(SharedRepositoryFetchRunner {
            common_dir,
            fetches: AtomicUsize::new(0),
            fetch_failures: AtomicUsize::new(0),
            remote_status_calls: Mutex::new(BTreeMap::new()),
            remote_status_changed: Notify::new(),
            dirty: AtomicBool::new(false),
            common_dir_started: None,
            release_common_dir: None,
            fetch_started: None,
            fetch_cancelled: None,
            release_fetch: None,
        });
        let repository = Arc::new(GitRepository::with_runner_for_test(runner.clone()));
        let (automatic_fetch_interval, _) = watch::channel(Duration::from_secs(30));
        let broadcaster = StatusBroadcaster::with_automatic_remote_refresh_interval(
            repository,
            Duration::from_secs(30),
            automatic_fetch_interval,
            4,
        );

        let mut main_subscription = broadcaster
            .subscribe(main.clone(), CancellationToken::new())
            .await
            .expect("main status subscription");
        let mut feature_subscription = broadcaster
            .subscribe(feature.clone(), CancellationToken::new())
            .await
            .expect("feature status subscription");
        assert!(matches!(
            main_subscription.receiver.try_recv(),
            Ok(StatusPublication {
                value: VcsStatusStreamEvent::Snapshot { .. },
                ..
            })
        ));
        assert!(matches!(
            feature_subscription.receiver.try_recv(),
            Ok(StatusPublication {
                value: VcsStatusStreamEvent::Snapshot { .. },
                ..
            })
        ));
        runner.wait_for_remote_status_count(&main, 1).await;
        runner.wait_for_remote_status_count(&feature, 1).await;
        let initial_main_remote = next_remote_update(&mut main_subscription).await;
        let initial_feature_remote = next_remote_update(&mut feature_subscription).await;
        assert_eq!(runner.fetch_count(), 0);
        assert_eq!(runner.remote_status_count(&main), 1);
        assert_eq!(runner.remote_status_count(&feature), 1);
        assert_eq!(initial_main_remote.ahead_count, 0);
        assert_eq!(initial_feature_remote.ahead_count, 0);
        broadcaster
            .inner
            .fetch_owner
            .wait_for_worktree_count_for_test(2)
            .await;
        assert_eq!(broadcaster.inner.fetch_owner.repository_count_for_test(), 1);
        assert_eq!(broadcaster.inner.fetch_owner.worktree_count_for_test(), 2);

        tokio::time::advance(Duration::from_secs(30)).await;
        runner.wait_for_remote_status_count(&main, 2).await;
        runner.wait_for_remote_status_count(&feature, 2).await;
        let main_remote = next_remote_update(&mut main_subscription).await;
        let feature_remote = next_remote_update(&mut feature_subscription).await;

        assert_eq!(runner.fetch_count(), 1);
        assert_eq!(runner.remote_status_count(&main), 2);
        assert_eq!(runner.remote_status_count(&feature), 2);
        assert_eq!(main_remote.ahead_count, 1);
        assert_eq!(feature_remote.ahead_count, 2);
    }

    #[tokio::test(start_paused = true)]
    async fn local_invalidation_publishes_within_750_ms_while_fetch_is_blocked() {
        let sandbox = TestSandbox::new("git-broadcaster-blocked-shared-fetch");
        let cwd = sandbox.path("main");
        let common_dir = sandbox.path("common.git");
        fs::create_dir_all(&cwd).expect("worktree fixture");
        fs::create_dir_all(&common_dir).expect("common directory fixture");
        let (fetch_started, mut fetch_started_rx) = mpsc::unbounded_channel();
        let (fetch_cancelled, mut fetch_cancelled_rx) = mpsc::unbounded_channel();
        let release_fetch = Arc::new(Semaphore::new(0));
        let runner = Arc::new(SharedRepositoryFetchRunner {
            common_dir,
            fetches: AtomicUsize::new(0),
            fetch_failures: AtomicUsize::new(0),
            remote_status_calls: Mutex::new(BTreeMap::new()),
            remote_status_changed: Notify::new(),
            dirty: AtomicBool::new(false),
            common_dir_started: None,
            release_common_dir: None,
            fetch_started: Some(fetch_started),
            fetch_cancelled: Some(fetch_cancelled),
            release_fetch: Some(release_fetch),
        });
        let repository = Arc::new(GitRepository::with_runner_for_test(runner.clone()));
        let (automatic_fetch_interval, _) = watch::channel(Duration::from_secs(30));
        let broadcaster = StatusBroadcaster::with_automatic_remote_refresh_interval(
            repository,
            Duration::from_secs(30),
            automatic_fetch_interval,
            4,
        );
        let mut subscription = broadcaster
            .subscribe(cwd.clone(), CancellationToken::new())
            .await
            .expect("status subscription");
        assert!(matches!(
            subscription.receiver.try_recv(),
            Ok(StatusPublication {
                value: VcsStatusStreamEvent::Snapshot { .. },
                ..
            })
        ));
        broadcaster
            .inner
            .fetch_owner
            .wait_for_worktree_count_for_test(1)
            .await;
        assert_eq!(broadcaster.inner.fetch_owner.repository_count_for_test(), 1);

        tokio::time::advance(Duration::from_secs(30)).await;
        fetch_started_rx
            .recv()
            .await
            .expect("physical repository fetch starts");
        runner.dirty.store(true, Ordering::SeqCst);
        broadcaster.notify_local_change(&cwd).await;

        let event = tokio::time::timeout(Duration::from_millis(750), async {
            loop {
                if let Some(VcsStatusStreamEvent::LocalUpdated { local }) =
                    subscription.recv().await
                    && local.has_working_tree_changes
                {
                    return local;
                }
            }
        })
        .await
        .expect("blocked fetch must not delay local publication");
        assert!(
            event
                .working_tree
                .files
                .iter()
                .any(|file| file.path == "tracked.txt")
        );

        drop(subscription);
        fetch_cancelled_rx
            .recv()
            .await
            .expect("final detach cancels the blocked fetch");
    }

    #[tokio::test(start_paused = true)]
    async fn shared_fetch_interval_zero_live_update_and_failure_backoff_are_preserved() {
        let sandbox = TestSandbox::new("git-broadcaster-shared-fetch-interval");
        let cwd = sandbox.path("main");
        let common_dir = sandbox.path("common.git");
        fs::create_dir_all(&cwd).expect("worktree fixture");
        fs::create_dir_all(&common_dir).expect("common directory fixture");
        let (fetch_started, mut fetch_started_rx) = mpsc::unbounded_channel();
        let runner = Arc::new(SharedRepositoryFetchRunner {
            common_dir,
            fetches: AtomicUsize::new(0),
            fetch_failures: AtomicUsize::new(1),
            remote_status_calls: Mutex::new(BTreeMap::new()),
            remote_status_changed: Notify::new(),
            dirty: AtomicBool::new(false),
            common_dir_started: None,
            release_common_dir: None,
            fetch_started: Some(fetch_started),
            fetch_cancelled: None,
            release_fetch: None,
        });
        let repository = Arc::new(GitRepository::with_runner_for_test(runner.clone()));
        let (automatic_fetch_interval, _) = watch::channel(Duration::ZERO);
        let broadcaster = StatusBroadcaster::with_automatic_remote_refresh_interval(
            repository,
            Duration::from_secs(30),
            automatic_fetch_interval.clone(),
            4,
        );
        let _subscription = broadcaster
            .subscribe(cwd, CancellationToken::new())
            .await
            .expect("status subscription");
        broadcaster
            .inner
            .fetch_owner
            .wait_for_worktree_count_for_test(1)
            .await;
        assert_eq!(broadcaster.inner.fetch_owner.repository_count_for_test(), 1);

        tokio::time::advance(Duration::from_secs(60)).await;
        assert_eq!(runner.fetch_count(), 0, "zero disables automatic fetch");

        automatic_fetch_interval.send_replace(Duration::from_secs(5));
        broadcaster
            .inner
            .fetch_owner
            .wait_for_interval_for_test(Duration::from_secs(5))
            .await;
        tokio::time::advance(Duration::from_secs(5)).await;
        fetch_started_rx
            .recv()
            .await
            .expect("first physical fetch starts");
        assert_eq!(
            runner.fetch_count(),
            1,
            "live interval update rearms the owner"
        );

        tokio::time::advance(Duration::from_secs(29)).await;
        assert_eq!(
            runner.fetch_count(),
            1,
            "failure applies the 30-second backoff"
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        fetch_started_rx
            .recv()
            .await
            .expect("backoff-delayed physical fetch starts");
        assert_eq!(runner.fetch_count(), 2);
    }

    #[tokio::test]
    async fn detach_before_common_dir_resolution_cannot_attach_a_stale_subscriber() {
        let sandbox = TestSandbox::new("git-broadcaster-stale-fetch-attach");
        let cwd = sandbox.path("main");
        let common_dir = sandbox.path("common.git");
        fs::create_dir_all(&cwd).expect("worktree fixture");
        fs::create_dir_all(&common_dir).expect("common directory fixture");
        let (common_dir_started, mut common_dir_started_rx) = mpsc::unbounded_channel();
        let release_common_dir = Arc::new(Semaphore::new(0));
        let runner = Arc::new(SharedRepositoryFetchRunner {
            common_dir,
            fetches: AtomicUsize::new(0),
            fetch_failures: AtomicUsize::new(0),
            remote_status_calls: Mutex::new(BTreeMap::new()),
            remote_status_changed: Notify::new(),
            dirty: AtomicBool::new(false),
            common_dir_started: Some(common_dir_started),
            release_common_dir: Some(release_common_dir.clone()),
            fetch_started: None,
            fetch_cancelled: None,
            release_fetch: None,
        });
        let repository = Arc::new(GitRepository::with_runner_for_test(runner));
        let (automatic_fetch_interval, _) = watch::channel(Duration::ZERO);
        let broadcaster = StatusBroadcaster::with_automatic_remote_refresh_interval(
            repository,
            Duration::from_secs(30),
            automatic_fetch_interval,
            4,
        );
        let subscription = broadcaster
            .subscribe(cwd, CancellationToken::new())
            .await
            .expect("status subscription returns before common-dir resolution");
        common_dir_started_rx
            .recv()
            .await
            .expect("common-dir resolution starts");

        let attachment_finished = broadcaster.inner.fetch_attachment_finished.notified();
        drop(subscription);
        release_common_dir.add_permits(1);
        attachment_finished.await;

        assert_eq!(broadcaster.active_poller_count(), 0);
        assert_eq!(broadcaster.inner.fetch_owner.repository_count_for_test(), 0);
    }

    struct FetchRepairFixture {
        _sandbox: TestSandbox,
        cwd: PathBuf,
        common_dir: PathBuf,
        broadcaster: StatusBroadcaster,
        resolution_started: mpsc::UnboundedReceiver<()>,
        release_resolution: Arc<Semaphore>,
    }

    impl FetchRepairFixture {
        fn new() -> Self {
            let sandbox = TestSandbox::new("git-broadcaster-repaired-fetch");
            let cwd = sandbox.path("main");
            let common_dir = sandbox.path("common.git");
            fs::create_dir_all(&cwd).expect("worktree fixture");
            fs::create_dir_all(&common_dir).expect("common directory fixture");
            let cwd = fs::canonicalize(cwd).expect("canonical worktree fixture");
            let common_dir =
                fs::canonicalize(common_dir).expect("canonical common directory fixture");
            let (started, resolution_started) = mpsc::unbounded_channel();
            let release_resolution = Arc::new(Semaphore::new(0));
            let runner = Arc::new(SharedRepositoryFetchRunner {
                common_dir: common_dir.clone(),
                fetches: AtomicUsize::new(0),
                fetch_failures: AtomicUsize::new(0),
                remote_status_calls: Mutex::new(BTreeMap::new()),
                remote_status_changed: Notify::new(),
                dirty: AtomicBool::new(false),
                common_dir_started: Some(started),
                release_common_dir: Some(release_resolution.clone()),
                fetch_started: None,
                fetch_cancelled: None,
                release_fetch: None,
            });
            let (interval, _) = watch::channel(Duration::ZERO);
            let broadcaster = StatusBroadcaster::with_automatic_remote_refresh_interval(
                Arc::new(GitRepository::with_runner_for_test(runner)),
                Duration::from_secs(3_600),
                interval,
                4,
            );
            install_epoch_repository_for_lifecycle(&broadcaster, &cwd, 7, "main", 0);
            {
                let mut state = broadcaster.lock_state();
                let entry = state.repositories.get_mut(&cwd).unwrap();
                entry.local = VcsStatusLocalResult::non_repository();
                entry.subscribers = HashMap::from([(7, RepositorySubscriber::GitManager)]);
            }
            Self {
                _sandbox: sandbox,
                cwd,
                common_dir,
                broadcaster,
                resolution_started,
                release_resolution,
            }
        }

        async fn publish_availability(&self, is_repo: bool, full: bool) {
            let mut local = VcsStatusLocalResult::non_repository();
            local.is_repo = is_repo;
            local.ref_name = is_repo.then(|| "main".to_owned());
            let fence = self
                .broadcaster
                .acquire_read_fence(&self.cwd, &CancellationToken::new())
                .await
                .expect("repair publication fence");
            if full {
                self.broadcaster.publish_status(
                    &self.cwd,
                    &VcsStatusResult {
                        local,
                        remote: VcsStatusRemoteResult {
                            has_upstream: false,
                            ahead_count: 0,
                            behind_count: 0,
                            ahead_of_default_count: None,
                            pr: None,
                        },
                    },
                    &fence,
                );
            } else {
                self.broadcaster.publish_local(&self.cwd, 7, &local, &fence);
            }
        }

        async fn wait_for_resolution(&mut self) {
            test_timeout(
                "repair common-directory resolution",
                self.resolution_started.recv(),
            )
            .await
            .expect("resolution observer stays open");
        }

        async fn wait_for_attachment(&self) {
            test_timeout(
                "repaired fetch ownership",
                self.broadcaster
                    .inner
                    .fetch_owner
                    .wait_for_worktree_count_for_test(1),
            )
            .await;
            assert_eq!(
                self.broadcaster.lock_state().repositories[&self.cwd].repository_key,
                Some(self.common_dir.clone())
            );
        }
    }

    #[tokio::test]
    async fn repair_fetch_attachment_coalesces_changes_and_attaches_current_subscribers() {
        let mut fixture = FetchRepairFixture::new();
        fixture.publish_availability(true, false).await;
        fixture.wait_for_resolution().await;
        for full in [false, true, false] {
            fixture.publish_availability(false, full).await;
            fixture.publish_availability(true, full).await;
        }
        assert!(
            matches!(
                fixture.resolution_started.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ),
            "repair observations must share one in-flight resolution"
        );
        fixture
            .broadcaster
            .lock_state()
            .repositories
            .get_mut(&fixture.cwd)
            .unwrap()
            .subscribers
            .insert(8, RepositorySubscriber::GitManager);
        fixture.broadcaster.release(&fixture.cwd, 7);
        fixture
            .broadcaster
            .lock_state()
            .repositories
            .get_mut(&fixture.cwd)
            .unwrap()
            .subscribers
            .insert(9, RepositorySubscriber::GitManager);

        fixture.release_resolution.add_permits(1);
        fixture.wait_for_resolution().await;
        assert_eq!(
            fixture
                .broadcaster
                .inner
                .fetch_owner
                .worktree_count_for_test(),
            0
        );
        fixture.release_resolution.add_permits(1);
        fixture.wait_for_attachment().await;
        assert!(
            !fixture
                .broadcaster
                .inner
                .fetch_owner
                .has_subscriber_for_test(&fixture.cwd, 7)
        );
        for subscriber in [8, 9] {
            assert!(
                fixture
                    .broadcaster
                    .inner
                    .fetch_owner
                    .has_subscriber_for_test(&fixture.cwd, subscriber)
            );
        }
        fixture.publish_availability(true, false).await;
        fixture.publish_availability(true, true).await;
        assert!(
            matches!(
                fixture.resolution_started.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ),
            "unchanged healthy status must not resolve or attach again"
        );
        assert_eq!(
            fixture
                .broadcaster
                .inner
                .fetch_owner
                .repository_count_for_test(),
            1
        );
        fixture.broadcaster.shutdown().await;
    }

    #[tokio::test]
    async fn repair_fetch_attachment_rejects_a_resolution_from_a_retired_mutation_epoch() {
        let mut fixture = FetchRepairFixture::new();
        fixture.publish_availability(true, true).await;
        fixture.wait_for_resolution().await;
        let mutation = fixture.broadcaster.begin_mutation(&fixture.cwd).await;
        fixture.release_resolution.add_permits(1);
        assert_eq!(
            fixture
                .broadcaster
                .inner
                .fetch_owner
                .worktree_count_for_test(),
            0
        );
        drop(mutation);
        fixture.wait_for_resolution().await;
        fixture.release_resolution.add_permits(1);
        fixture.wait_for_attachment().await;
        fixture.broadcaster.shutdown().await;
    }

    #[tokio::test]
    async fn repair_fetch_attachment_final_release_cancels_resolution_before_retirement_finishes() {
        let mut fixture = FetchRepairFixture::new();
        fixture.publish_availability(true, false).await;
        fixture.wait_for_resolution().await;
        let finished = fixture
            .broadcaster
            .inner
            .fetch_attachment_finished
            .notified();
        tokio::pin!(finished);
        finished.as_mut().enable();
        fixture.broadcaster.release(&fixture.cwd, 7);
        test_timeout("retired repair resolution cancellation", finished).await;
        fixture.release_resolution.add_permits(1);
        fixture
            .broadcaster
            .await_retired_lifecycle(&fixture.cwd)
            .await;
        assert_eq!(fixture.broadcaster.active_poller_count(), 0);
        assert_eq!(
            fixture
                .broadcaster
                .inner
                .fetch_owner
                .worktree_count_for_test(),
            0
        );
        fixture.broadcaster.shutdown().await;
    }

    #[tokio::test]
    async fn subscriber_setup_that_observes_repair_attaches_the_existing_lifecycle() {
        let mut fixture = FetchRepairFixture::new();
        let subscription = fixture
            .broadcaster
            .subscribe(fixture.cwd.clone(), CancellationToken::new())
            .await
            .expect("healthy subscriber setup observes repair");
        fixture.wait_for_resolution().await;
        fixture.release_resolution.add_permits(1);
        fixture.wait_for_attachment().await;
        for subscriber in [7, subscription.subscriber_id] {
            assert!(
                fixture
                    .broadcaster
                    .inner
                    .fetch_owner
                    .has_subscriber_for_test(&fixture.cwd, subscriber)
            );
        }
        drop(subscription);
        fixture.broadcaster.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn final_release_retires_the_old_epoch_before_reattachment_can_read() {
        let sandbox = TestSandbox::new("git-broadcaster-final-release-epoch-order");
        let cwd = sandbox.path("repository");
        fs::create_dir_all(&cwd).expect("repository fixture");
        let cwd = fs::canonicalize(cwd).expect("canonical repository fixture");
        let (ref_started, _) = mpsc::unbounded_channel();
        let (remote_started, _) = mpsc::unbounded_channel();
        let runner = Arc::new(EpochGitRunner {
            branch: Mutex::new("old".to_owned()),
            ref_calls: AtomicUsize::new(0),
            remote_calls: AtomicUsize::new(0),
            ref_started,
            remote_started,
            release_ref: Arc::new(Semaphore::new(16)),
            release_remote: Arc::new(Semaphore::new(16)),
        });
        let broadcaster = StatusBroadcaster::new(
            Arc::new(GitRepository::with_runner_for_test(runner)),
            Duration::from_secs(3_600),
            4,
        );
        let old_events = install_epoch_repository(&broadcaster, &cwd);
        let old_fence = broadcaster
            .acquire_read_fence(&cwd, &CancellationToken::new())
            .await
            .expect("old lifecycle fence");
        let reads_before_release = broadcaster.physical_local_read_count_for_test(&cwd).await;
        let gate = broadcaster.install_retirement_epoch_gate_for_test();
        let releasing = {
            let broadcaster = broadcaster.clone();
            let cwd = cwd.clone();
            tokio::task::spawn_blocking(move || broadcaster.release(&cwd, 1))
        };
        gate.wait_until_entered().await;

        let retirement_is_fenced = broadcaster.retiring_tracker_is_nonempty_for_test(&cwd);
        if !retirement_is_fenced {
            gate.release();
            releasing.await.expect("unfenced final release joins");
            panic!(
                "the published retirement tracker must fence reattachment before epoch retirement"
            );
        }
        let retirement_baseline = broadcaster.retirement_wait_count_for_test();
        let reattaching = {
            let broadcaster = broadcaster.clone();
            let cwd = cwd.clone();
            tokio::spawn(async move { broadcaster.subscribe(cwd, CancellationToken::new()).await })
        };
        tokio::time::timeout(
            Duration::from_secs(5),
            broadcaster.wait_for_retirement_wait_started_for_test(retirement_baseline),
        )
        .await
        .expect("reattachment reaches the old lifecycle retirement wait");
        assert!(
            !reattaching.is_finished(),
            "reattachment must wait until the old epoch has been retired"
        );
        assert_eq!(
            broadcaster.physical_local_read_count_for_test(&cwd).await,
            reads_before_release,
            "reattachment must not start a new physical read before old-epoch cancellation"
        );

        gate.release();
        releasing.await.expect("final release joins");
        let mut reattached = reattaching
            .await
            .expect("reattachment task joins")
            .expect("reattachment succeeds");
        assert!(matches!(
            reattached.recv().await,
            Some(VcsStatusStreamEvent::Snapshot { .. })
        ));
        assert!(
            broadcaster
                .publish_if_fence_current(
                    &broadcaster
                        .acquire_read_fence(&cwd, &CancellationToken::new())
                        .await
                        .expect("new lifecycle fence"),
                    || (),
                )
                .is_ok(),
            "old lifecycle retirement must not stale the new lifecycle"
        );
        assert!(
            broadcaster
                .publish_if_fence_current(&old_fence, || ())
                .is_err(),
            "the old lifecycle fence must be retired"
        );
        drop(old_events);
        drop(reattached);
        broadcaster.shutdown().await;
    }

    #[tokio::test]
    async fn blocked_post_fetch_remote_cannot_overwrite_the_post_switch_remote() {
        let sandbox = TestSandbox::new("git-broadcaster-remote-epoch");
        let cwd = sandbox.path("repository");
        fs::create_dir_all(&cwd).expect("repository fixture");
        let cwd = fs::canonicalize(cwd).expect("canonical repository fixture");
        let (ref_started, _) = mpsc::unbounded_channel();
        let (remote_started, mut remote_started_rx) = mpsc::unbounded_channel();
        let release_remote = Arc::new(Semaphore::new(0));
        let runner = Arc::new(EpochGitRunner {
            branch: Mutex::new("old".to_owned()),
            ref_calls: AtomicUsize::new(0),
            remote_calls: AtomicUsize::new(0),
            ref_started,
            remote_started,
            release_ref: Arc::new(Semaphore::new(0)),
            release_remote: Arc::clone(&release_remote),
        });
        let broadcaster = StatusBroadcaster::new(
            Arc::new(GitRepository::with_runner_for_test(runner.clone())),
            Duration::from_secs(3_600),
            4,
        );
        let mut events = install_epoch_repository(&broadcaster, &cwd);
        let old_refresh = {
            let broadcaster = broadcaster.clone();
            let cwd = cwd.clone();
            tokio::spawn(async move {
                broadcaster
                    .refresh_remote(&cwd, &CancellationToken::new())
                    .await
            })
        };
        remote_started_rx
            .recv()
            .await
            .expect("old remote read starts");

        let mutation = broadcaster.begin_mutation(&cwd).await;
        runner.set_branch("new");
        mutation.finish().await;
        {
            let mut state = broadcaster.lock_state();
            let entry = state.repositories.get_mut(&cwd).expect("repository entry");
            entry.local.ref_name = Some("new".to_owned());
            clear_remote_for_ref_change(entry);
        }
        broadcaster
            .refresh_remote(&cwd, &CancellationToken::new())
            .await
            .expect("post-switch remote refresh");
        assert!(matches!(
            events.recv().await,
            Some(StatusPublication {
                value: VcsStatusStreamEvent::RemoteUpdated { remote: Some(ref remote) },
                ..
            })
                if remote.ahead_count == 2
        ));

        release_remote.add_permits(1);
        let _ = old_refresh.await.expect("old remote task joins");
        assert_eq!(
            broadcaster
                .lock_state()
                .repositories
                .get(&cwd)
                .expect("repository remains registered")
                .remote
                .as_ref()
                .and_then(Option::as_ref)
                .expect("remote remains available")
                .ahead_count,
            2
        );
        assert!(
            events.try_recv().is_err(),
            "old remote event must be rejected"
        );
    }

    #[tokio::test]
    async fn final_release_rejects_a_blocked_remote_from_the_previous_lifecycle() {
        let sandbox = TestSandbox::new("git-broadcaster-final-release-remote");
        let cwd = sandbox.path("repository");
        fs::create_dir_all(&cwd).expect("repository fixture");
        let cwd = fs::canonicalize(cwd).expect("canonical repository fixture");
        let (ref_started, _) = mpsc::unbounded_channel();
        let (remote_started, mut remote_started_rx) = mpsc::unbounded_channel();
        let release_remote = Arc::new(Semaphore::new(0));
        let runner = Arc::new(EpochGitRunner {
            branch: Mutex::new("old".to_owned()),
            ref_calls: AtomicUsize::new(0),
            remote_calls: AtomicUsize::new(0),
            ref_started,
            remote_started,
            release_ref: Arc::new(Semaphore::new(0)),
            release_remote: Arc::clone(&release_remote),
        });
        let broadcaster = StatusBroadcaster::new(
            Arc::new(GitRepository::with_runner_for_test(runner.clone())),
            Duration::from_secs(3_600),
            4,
        );
        let old_events = install_epoch_repository_for_lifecycle(&broadcaster, &cwd, 1, "old", 1);
        let old_refresh = {
            let broadcaster = broadcaster.clone();
            let cwd = cwd.clone();
            tokio::spawn(async move {
                broadcaster
                    .refresh_remote(&cwd, &CancellationToken::new())
                    .await
            })
        };
        remote_started_rx
            .recv()
            .await
            .expect("old remote read starts");

        broadcaster.release(&cwd, 1);
        drop(old_events);
        let mut new_events =
            install_epoch_repository_for_lifecycle(&broadcaster, &cwd, 2, "old", 2);
        release_remote.add_permits(1);
        let _ = old_refresh.await.expect("old remote task joins");

        assert_eq!(
            broadcaster
                .lock_state()
                .repositories
                .get(&cwd)
                .expect("new lifecycle remains registered")
                .remote
                .as_ref()
                .and_then(Option::as_ref)
                .expect("new remote remains available")
                .ahead_count,
            2
        );
        assert!(
            new_events.try_recv().is_err(),
            "the old lifecycle must not publish into the reattached subscriber"
        );
    }

    async fn unavailable_repository_broadcaster(
        cwd: &Path,
        reason: VcsRepositoryUnavailableReason,
    ) -> (
        StatusBroadcaster,
        mpsc::Receiver<StatusPublication<VcsStatusStreamEvent>>,
        StatusReadFence,
    ) {
        let broadcaster = StatusBroadcaster::new(
            Arc::new(GitRepository::default()),
            Duration::from_secs(3_600),
            4,
        );
        let events = install_epoch_repository(&broadcaster, cwd);
        broadcaster
            .lock_state()
            .repositories
            .get_mut(cwd)
            .unwrap()
            .local = VcsStatusLocalResult::non_repository_with_reason(reason);
        let fence = broadcaster
            .acquire_read_fence(cwd, &CancellationToken::new())
            .await
            .expect("read fence");
        (broadcaster, events, fence)
    }

    #[tokio::test]
    async fn repository_unavailable_reason_change_is_republished() {
        let root = tempfile::tempdir().expect("repository folder");
        let cwd = fs::canonicalize(root.path()).expect("canonical folder");
        let (broadcaster, mut events, fence) =
            unavailable_repository_broadcaster(&cwd, VcsRepositoryUnavailableReason::Absent).await;
        let unreadable = VcsStatusLocalResult::non_repository_with_reason(
            VcsRepositoryUnavailableReason::Unreadable,
        );
        broadcaster.publish_local(&cwd, 1, &unreadable, &fence);
        assert_eq!(
            events.try_recv().expect("reason change is published").value,
            VcsStatusStreamEvent::LocalUpdated { local: unreadable }
        );
        broadcaster.shutdown().await;
    }

    #[tokio::test]
    async fn repository_unavailable_unchanged_unreadable_is_not_republished() {
        let root = tempfile::tempdir().expect("repository folder");
        let cwd = fs::canonicalize(root.path()).expect("canonical folder");
        let (broadcaster, mut events, fence) =
            unavailable_repository_broadcaster(&cwd, VcsRepositoryUnavailableReason::Unreadable)
                .await;
        let unreadable = VcsStatusLocalResult::non_repository_with_reason(
            VcsRepositoryUnavailableReason::Unreadable,
        );
        broadcaster.publish_local(&cwd, 1, &unreadable, &fence);
        assert!(matches!(
            events.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        broadcaster.shutdown().await;
    }

    fn record_availability_nudges(broadcaster: &StatusBroadcaster) -> Arc<Mutex<Vec<PathBuf>>> {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let observed_calls = Arc::clone(&calls);
        let inner = Arc::downgrade(&broadcaster.inner);
        broadcaster.set_local_change_observer(move |cwd| {
            let inner = inner.upgrade().expect("broadcaster remains alive");
            assert!(
                inner.state.try_lock().is_ok(),
                "availability observer must run outside the broadcaster state lock"
            );
            observed_calls.lock().unwrap().push(cwd.to_path_buf());
        });
        calls
    }

    async fn assert_repository_availability_publications_nudge(publish_full: bool) {
        let root = tempfile::tempdir().expect("repository folder");
        let cwd = fs::canonicalize(root.path()).expect("canonical folder");
        let (broadcaster, mut events, fence) =
            unavailable_repository_broadcaster(&cwd, VcsRepositoryUnavailableReason::Unreadable)
                .await;
        let calls = record_availability_nudges(&broadcaster);
        let mut healthy = VcsStatusLocalResult::non_repository();
        healthy.is_repo = true;
        healthy.ref_name = Some("main".to_owned());
        let mut branch_changed = healthy.clone();
        branch_changed.ref_name = Some("topic".to_owned());
        let mut dirty = branch_changed.clone();
        dirty.has_working_tree_changes = true;
        let unreadable = VcsStatusLocalResult::non_repository_with_reason(
            VcsRepositoryUnavailableReason::Unreadable,
        );
        let transitions = [
            ("repair", healthy.clone(), 1),
            ("branch change", branch_changed, 1),
            ("dirty change", dirty.clone(), 1),
            ("unchanged healthy", dirty, 1),
            ("break", unreadable.clone(), 2),
            ("unchanged unreadable", unreadable, 2),
            (
                "reason changes to absent",
                VcsStatusLocalResult::non_repository_with_reason(
                    VcsRepositoryUnavailableReason::Absent,
                ),
                3,
            ),
            (
                "reason changes to untrusted",
                VcsStatusLocalResult::non_repository_with_reason(
                    VcsRepositoryUnavailableReason::Untrusted,
                ),
                4,
            ),
            ("reason clears", VcsStatusLocalResult::non_repository(), 5),
            ("only is_repo changes", healthy, 6),
        ];
        for (description, local, expected_calls) in transitions {
            if publish_full {
                broadcaster.publish_status(
                    &cwd,
                    &VcsStatusResult {
                        local: local.clone(),
                        remote: VcsStatusRemoteResult {
                            has_upstream: false,
                            ahead_count: 0,
                            behind_count: 0,
                            ahead_of_default_count: None,
                            pr: None,
                        },
                    },
                    &fence,
                );
            } else {
                broadcaster.publish_local(&cwd, 1, &local, &fence);
            }
            assert_eq!(
                *calls.lock().unwrap(),
                vec![cwd.clone(); expected_calls],
                "{description}"
            );
            while events.try_recv().is_ok() {}
        }
        broadcaster.shutdown().await;
    }

    #[tokio::test]
    async fn repository_availability_local_publications_nudge_only_on_changes() {
        assert_repository_availability_publications_nudge(false).await;
    }

    #[tokio::test]
    async fn repository_availability_full_publications_nudge_only_on_changes() {
        assert_repository_availability_publications_nudge(true).await;
    }

    fn availability_broadcaster() -> StatusBroadcaster {
        let (ref_started, _) = mpsc::unbounded_channel();
        let (remote_started, _) = mpsc::unbounded_channel();
        let runner = Arc::new(EpochGitRunner {
            branch: Mutex::new("main".to_owned()),
            ref_calls: AtomicUsize::new(0),
            remote_calls: AtomicUsize::new(0),
            ref_started,
            remote_started,
            release_ref: Arc::new(Semaphore::new(16)),
            release_remote: Arc::new(Semaphore::new(16)),
        });
        StatusBroadcaster::new(
            Arc::new(GitRepository::with_runner_for_test(runner)),
            Duration::from_secs(3_600),
            4,
        )
    }

    async fn assert_repository_availability_recovery_nudges(subscribe: bool) {
        let root = tempfile::tempdir().expect("repository folder");
        let cwd = fs::canonicalize(root.path()).expect("canonical folder");
        let broadcaster = availability_broadcaster();
        let mut events = install_epoch_repository(&broadcaster, &cwd);
        broadcaster
            .lock_state()
            .repositories
            .get_mut(&cwd)
            .unwrap()
            .local = VcsStatusLocalResult::non_repository_with_reason(
            VcsRepositoryUnavailableReason::Unreadable,
        );
        let calls = record_availability_nudges(&broadcaster);
        // Both public entry points must notify the canonical path, not this alias.
        let alias = cwd.join(".");
        let mut subscription = if subscribe {
            Some(
                broadcaster
                    .subscribe(alias, CancellationToken::new())
                    .await
                    .expect("repaired repository subscription"),
            )
        } else {
            let local = broadcaster
                .refresh_local(&alias, &CancellationToken::new())
                .await
                .expect("repaired local status");
            assert!(local.is_repo);
            None
        };
        assert_eq!(*calls.lock().unwrap(), vec![cwd.clone()]);
        assert!(matches!(
            events.try_recv().expect("recovery is published").value,
            VcsStatusStreamEvent::LocalUpdated { local } if local.is_repo
        ));
        if let Some(subscription) = &mut subscription {
            assert!(matches!(
                subscription.recv().await,
                Some(VcsStatusStreamEvent::Snapshot { local, .. }) if local.is_repo
            ));
        }
        drop(subscription);
        broadcaster.shutdown().await;
    }

    #[tokio::test]
    async fn repository_availability_local_refresh_nudges_canonical_cwd() {
        assert_repository_availability_recovery_nudges(false).await;
    }

    #[tokio::test]
    async fn repository_availability_subscriber_read_nudges_canonical_cwd() {
        assert_repository_availability_recovery_nudges(true).await;
    }

    #[tokio::test]
    async fn repository_availability_first_and_unchanged_subscriptions_do_not_nudge() {
        let root = tempfile::tempdir().expect("repository folder");
        let broadcaster = availability_broadcaster();
        let calls = record_availability_nudges(&broadcaster);
        let first = broadcaster
            .subscribe(root.path().to_path_buf(), CancellationToken::new())
            .await
            .expect("first subscription");
        let second = broadcaster
            .subscribe(root.path().to_path_buf(), CancellationToken::new())
            .await
            .expect("unchanged subscription");
        assert!(calls.lock().unwrap().is_empty());
        drop((first, second));
        broadcaster.shutdown().await;
    }

    #[tokio::test]
    async fn unchanged_remote_cache_is_retied_to_the_current_epoch() {
        let sandbox = TestSandbox::new("git-broadcaster-remote-cache-epoch");
        let cwd = sandbox.path("repository");
        fs::create_dir_all(&cwd).expect("repository fixture");
        let cwd = fs::canonicalize(cwd).expect("canonical repository fixture");
        let (ref_started, _) = mpsc::unbounded_channel();
        let (remote_started, _) = mpsc::unbounded_channel();
        let release_remote = Arc::new(Semaphore::new(1));
        let runner = Arc::new(EpochGitRunner {
            branch: Mutex::new("old".to_owned()),
            ref_calls: AtomicUsize::new(0),
            remote_calls: AtomicUsize::new(0),
            ref_started,
            remote_started,
            release_ref: Arc::new(Semaphore::new(0)),
            release_remote,
        });
        let broadcaster = StatusBroadcaster::new(
            Arc::new(GitRepository::with_runner_for_test(runner)),
            Duration::from_secs(3_600),
            4,
        );
        let _events = install_epoch_repository(&broadcaster, &cwd);
        let old_fence = broadcaster
            .acquire_read_fence(&cwd, &CancellationToken::new())
            .await
            .expect("old fence");
        broadcaster
            .lock_state()
            .repositories
            .get_mut(&cwd)
            .expect("repository remains registered")
            .remote_fence = Some(old_fence);
        let mutation = broadcaster.begin_mutation(&cwd).await;
        mutation.finish().await;

        broadcaster
            .refresh_remote(&cwd, &CancellationToken::new())
            .await
            .expect("unchanged current remote refresh");
        let fence = broadcaster
            .lock_state()
            .repositories
            .get(&cwd)
            .and_then(|entry| entry.remote_fence.clone())
            .expect("remote retains a producing fence");
        broadcaster
            .publish_if_fence_current(&fence, || ())
            .expect("unchanged remote is current");
    }

    #[tokio::test]
    async fn same_epoch_external_switch_does_not_reuse_cached_remote_for_a_new_subscriber() {
        let sandbox = TestSandbox::new("git-broadcaster-subscriber-branch-provenance");
        let cwd = sandbox.path("repository");
        fs::create_dir_all(&cwd).expect("repository fixture");
        let cwd = fs::canonicalize(cwd).expect("canonical repository fixture");
        let (ref_started, _) = mpsc::unbounded_channel();
        let (remote_started, _) = mpsc::unbounded_channel();
        let runner = Arc::new(EpochGitRunner {
            branch: Mutex::new("old".to_owned()),
            ref_calls: AtomicUsize::new(0),
            remote_calls: AtomicUsize::new(0),
            ref_started,
            remote_started,
            release_ref: Arc::new(Semaphore::new(0)),
            release_remote: Arc::new(Semaphore::new(0)),
        });
        let broadcaster = StatusBroadcaster::new(
            Arc::new(GitRepository::with_runner_for_test(runner.clone())),
            Duration::from_secs(3_600),
            4,
        );
        let mut existing_events = install_epoch_repository(&broadcaster, &cwd);
        let fence = broadcaster
            .acquire_read_fence(&cwd, &CancellationToken::new())
            .await
            .expect("cached remote fence");
        {
            let mut state = broadcaster.lock_state();
            let entry = state
                .repositories
                .get_mut(&cwd)
                .expect("repository remains registered");
            entry.remote_fence = Some(fence);
            entry.remote_ref_name = Some(Some("old".to_owned()));
        }
        runner.set_branch("new");

        let mut new_subscription = broadcaster
            .subscribe(cwd.clone(), CancellationToken::new())
            .await
            .expect("new branch subscription");
        assert!(matches!(
            new_subscription.recv().await,
            Some(VcsStatusStreamEvent::Snapshot { ref local, remote: None })
                if local.ref_name.as_deref() == Some("new")
        ));
        assert!(matches!(
            existing_events.recv().await,
            Some(StatusPublication {
                value: VcsStatusStreamEvent::LocalUpdated { ref local },
                ..
            }) if local.ref_name.as_deref() == Some("new")
        ));
    }

    #[tokio::test]
    async fn external_local_observation_a_to_b_to_a_clears_remote_cache_and_reconciles() {
        let sandbox = TestSandbox::new("git-broadcaster-a-b-a-branch-provenance");
        let cwd = sandbox.path("repository");
        fs::create_dir_all(&cwd).expect("repository fixture");
        let cwd = fs::canonicalize(cwd).expect("canonical repository fixture");
        let (ref_started, _) = mpsc::unbounded_channel();
        let (remote_started, _) = mpsc::unbounded_channel();
        let runner = Arc::new(EpochGitRunner {
            branch: Mutex::new("new".to_owned()),
            ref_calls: AtomicUsize::new(0),
            remote_calls: AtomicUsize::new(0),
            ref_started,
            remote_started,
            release_ref: Arc::new(Semaphore::new(2)),
            release_remote: Arc::new(Semaphore::new(0)),
        });
        let broadcaster = StatusBroadcaster::new(
            Arc::new(GitRepository::with_runner_for_test(runner.clone())),
            Duration::from_secs(3_600),
            4,
        );
        let _events = install_epoch_repository(&broadcaster, &cwd);
        let fence = broadcaster
            .acquire_read_fence(&cwd, &CancellationToken::new())
            .await
            .expect("cached remote fence");
        {
            let mut state = broadcaster.lock_state();
            let entry = state.repositories.get_mut(&cwd).expect("repository entry");
            entry.remote_fence = Some(fence);
            entry.remote_ref_name = Some(Some("old".to_owned()));
        }

        broadcaster
            .refresh_local(&cwd, &CancellationToken::new())
            .await
            .expect("switch to B");
        runner.set_branch("old");
        broadcaster
            .refresh_local(&cwd, &CancellationToken::new())
            .await
            .expect("switch back to A");

        let state = broadcaster.lock_state();
        let entry = state.repositories.get(&cwd).expect("repository entry");
        assert_eq!(entry.local.ref_name.as_deref(), Some("old"));
        assert_eq!(entry.remote, None);
        assert_eq!(entry.remote_fence, None);
        assert_eq!(entry.remote_ref_name, None);
        assert_eq!(*entry.remote_refresh_requests.borrow(), 2);
    }

    #[tokio::test]
    async fn unchanged_local_ref_consumes_remote_mismatch_and_reconciles_exactly_once() {
        let sandbox = TestSandbox::new("git-broadcaster-unchanged-local-mismatch");
        let cwd = sandbox.path("repository");
        fs::create_dir_all(&cwd).expect("repository fixture");
        let cwd = fs::canonicalize(cwd).expect("canonical repository fixture");
        let runner = Arc::new(RemoteMismatchGitRunner::new(0, 1));
        let broadcaster = StatusBroadcaster::new(
            Arc::new(GitRepository::with_runner_for_test(runner.clone())),
            Duration::from_secs(3_600),
            4,
        );
        let mut events = install_epoch_repository_for_lifecycle(&broadcaster, &cwd, 1, "main", 9);
        {
            let mut state = broadcaster.lock_state();
            let local = &mut state
                .repositories
                .get_mut(&cwd)
                .expect("repository entry")
                .local;
            local.is_default_ref = true;
            local.default_ref_name = Some("main".to_owned());
        }
        let (cancellation, tasks) = spawn_status_workers_for_lifecycle(&broadcaster, &cwd, 1);

        runner.wait_for_calls(1, 2).await;

        let publication = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("reconciled remote event deadline");
        assert!(
            matches!(
                publication.as_ref(),
                Some(StatusPublication {
                    value: VcsStatusStreamEvent::RemoteUpdated { remote: Some(remote) },
                    ..
                }) if remote.ahead_count == 1
            ),
            "unexpected publication: {:?}",
            publication.map(|publication| publication.value)
        );
        tokio::task::yield_now().await;
        assert!(events.try_recv().is_err(), "one remote event is published");
        assert_eq!(runner.local_calls.load(Ordering::SeqCst), 1);
        assert_eq!(runner.remote_calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            broadcaster.local_refresh_generation_for_test(&cwd).await,
            1,
            "one mismatch requests one local refresh"
        );
        assert_eq!(
            *broadcaster
                .lock_state()
                .repositories
                .get(&cwd)
                .expect("repository entry")
                .remote_refresh_requests
                .borrow(),
            1,
            "unchanged authoritative local publication queues one reconcile"
        );

        cancellation.cancel();
        tasks.wait().await;
    }

    #[tokio::test]
    async fn local_error_keeps_one_pending_remote_reconcile_until_later_success() {
        let sandbox = TestSandbox::new("git-broadcaster-local-error-mismatch");
        let cwd = sandbox.path("repository");
        fs::create_dir_all(&cwd).expect("repository fixture");
        let cwd = fs::canonicalize(cwd).expect("canonical repository fixture");
        let runner = Arc::new(RemoteMismatchGitRunner::new(1, 2));
        let broadcaster = StatusBroadcaster::new(
            Arc::new(GitRepository::with_runner_for_test(runner.clone())),
            Duration::from_secs(3_600),
            4,
        );
        let mut events = install_epoch_repository_for_lifecycle(&broadcaster, &cwd, 1, "main", 9);
        {
            let mut state = broadcaster.lock_state();
            let local = &mut state
                .repositories
                .get_mut(&cwd)
                .expect("repository entry")
                .local;
            local.is_default_ref = true;
            local.default_ref_name = Some("main".to_owned());
        }
        let (cancellation, tasks) = spawn_status_workers_for_lifecycle(&broadcaster, &cwd, 1);

        runner.wait_for_calls(1, 1).await;
        assert_eq!(
            broadcaster.local_refresh_generation_for_test(&cwd).await,
            1,
            "first mismatch owns one local request"
        );
        assert!(
            events.try_recv().is_err(),
            "local failure publishes nothing"
        );

        broadcaster
            .lock_state()
            .repositories
            .get(&cwd)
            .expect("repository entry")
            .remote_refresh_requests
            .send_modify(|generation| *generation = generation.wrapping_add(1));
        runner.wait_for_calls(1, 2).await;
        tokio::task::yield_now().await;
        assert_eq!(
            broadcaster.local_refresh_generation_for_test(&cwd).await,
            1,
            "a second mismatch coalesces into the pending local request"
        );

        broadcaster.inner.status_owner.request_local_refresh(&cwd);
        runner.wait_for_calls(2, 3).await;
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(5), events.recv())
                .await
                .expect("event after later local success deadline"),
            Some(StatusPublication {
                value: VcsStatusStreamEvent::RemoteUpdated { remote: Some(ref remote) },
                ..
            }) if remote.ahead_count == 1
        ));
        tokio::task::yield_now().await;
        assert!(events.try_recv().is_err(), "one remote event is published");
        assert_eq!(runner.local_calls.load(Ordering::SeqCst), 2);
        assert_eq!(runner.remote_calls.load(Ordering::SeqCst), 3);
        assert_eq!(
            broadcaster.local_refresh_generation_for_test(&cwd).await,
            2,
            "only the explicit later trigger adds a second local generation"
        );
        assert_eq!(
            *broadcaster
                .lock_state()
                .repositories
                .get(&cwd)
                .expect("repository entry")
                .remote_refresh_requests
                .borrow(),
            2,
            "one external remote trigger and one consumed reconcile are queued"
        );

        cancellation.cancel();
        tasks.wait().await;
    }

    #[tokio::test]
    async fn remote_refresh_publishes_only_for_the_current_observed_ref() {
        let sandbox = TestSandbox::new("git-broadcaster-current-remote-branch");
        let cwd = sandbox.path("repository");
        fs::create_dir_all(&cwd).expect("repository fixture");
        let cwd = fs::canonicalize(cwd).expect("canonical repository fixture");
        let (ref_started, _) = mpsc::unbounded_channel();
        let (remote_started, _) = mpsc::unbounded_channel();
        let release_remote = Arc::new(Semaphore::new(1));
        let runner = Arc::new(EpochGitRunner {
            branch: Mutex::new("old".to_owned()),
            ref_calls: AtomicUsize::new(0),
            remote_calls: AtomicUsize::new(0),
            ref_started,
            remote_started,
            release_ref: Arc::new(Semaphore::new(0)),
            release_remote,
        });
        let broadcaster = StatusBroadcaster::new(
            Arc::new(GitRepository::with_runner_for_test(runner.clone())),
            Duration::from_secs(3_600),
            4,
        );
        let mut events = install_epoch_repository(&broadcaster, &cwd);
        broadcaster
            .lock_state()
            .repositories
            .get_mut(&cwd)
            .expect("repository entry")
            .local
            .ref_name = Some("new".to_owned());

        broadcaster
            .refresh_remote(&cwd, &CancellationToken::new())
            .await
            .expect("old ref remote is discarded");
        assert!(events.try_recv().is_err());
        runner.set_branch("new");
        broadcaster
            .refresh_remote(&cwd, &CancellationToken::new())
            .await
            .expect("current ref remote publishes");
        assert!(matches!(
            events.recv().await,
            Some(StatusPublication {
                value: VcsStatusStreamEvent::RemoteUpdated { remote: Some(ref remote) },
                ..
            }) if remote.ahead_count == 2
        ));
        let state = broadcaster.lock_state();
        let entry = state.repositories.get(&cwd).expect("repository entry");
        assert_eq!(entry.remote_ref_name, Some(Some("new".to_owned())));
        assert_eq!(
            entry
                .remote
                .as_ref()
                .and_then(Option::as_ref)
                .map(|remote| remote.ahead_count),
            Some(2)
        );
    }

    #[tokio::test]
    async fn mutation_seam_rejects_a_completed_pre_mutation_publication() {
        let sandbox = TestSandbox::new("git-broadcaster-stale-publication");
        let repository_path = sandbox.path("repository");
        fs::create_dir(&repository_path).expect("repository fixture directory");
        let canonical_cwd = tokio::fs::canonicalize(&repository_path)
            .await
            .expect("canonical repository fixture path");
        let broadcaster = StatusBroadcaster::new(
            Arc::new(GitRepository::default()),
            Duration::from_secs(30),
            4,
        );
        let read = broadcaster
            .inner
            .status_owner
            .read_full(
                StatusReadKey {
                    canonical_cwd: canonical_cwd.clone(),
                    output_kind: StatusOutputKind::Full,
                },
                &CancellationToken::new(),
                |_| async move {
                    Ok(VcsStatusResult {
                        local: VcsStatusLocalResult::non_repository(),
                        remote: VcsStatusRemoteResult {
                            has_upstream: false,
                            ahead_count: 0,
                            behind_count: 0,
                            ahead_of_default_count: Some(0),
                            pr: None,
                        },
                    })
                },
            )
            .await
            .expect("pre-mutation status read completes");
        let mutation = broadcaster.begin_mutation(&canonical_cwd).await;
        let mut published = false;

        let result = broadcaster
            .inner
            .status_owner
            .publish_if_current(read, |_| published = true);

        assert!(result.is_err());
        assert!(!published);
        mutation.finish().await;
    }

    fn install_epoch_repository(
        broadcaster: &StatusBroadcaster,
        cwd: &Path,
    ) -> mpsc::Receiver<StatusPublication<VcsStatusStreamEvent>> {
        install_epoch_repository_for_lifecycle(broadcaster, cwd, 1, "old", 1)
    }

    fn spawn_status_workers_for_lifecycle(
        broadcaster: &StatusBroadcaster,
        cwd: &Path,
        lifecycle_id: u64,
    ) -> (CancellationToken, TaskTracker) {
        let local_refresh_requests = broadcaster.inner.status_owner.subscribe_local_refresh(cwd);
        let remote_refresh_requests = broadcaster
            .lock_state()
            .repositories
            .get(cwd)
            .expect("repository entry")
            .remote_refresh_requests
            .subscribe();
        let cancellation = CancellationToken::new();
        let tasks = TaskTracker::new();
        broadcaster.spawn_local_status_poller(
            cwd.to_path_buf(),
            lifecycle_id,
            cancellation.clone(),
            local_refresh_requests,
            (
                WatcherSignalSource::healthy_source_for_test(),
                Duration::ZERO,
            ),
            &tasks,
        );
        broadcaster.spawn_remote_reconciliation(
            cwd.to_path_buf(),
            lifecycle_id,
            cancellation.clone(),
            remote_refresh_requests,
            &tasks,
        );
        tasks.close();
        (cancellation, tasks)
    }

    fn install_epoch_repository_for_lifecycle(
        broadcaster: &StatusBroadcaster,
        cwd: &Path,
        lifecycle_id: u64,
        ref_name: &str,
        ahead_count: u64,
    ) -> mpsc::Receiver<StatusPublication<VcsStatusStreamEvent>> {
        let (sender, receiver) = mpsc::channel(4);
        let (remote_refresh_requests, _) = watch::channel(0);
        let (git_manager_signal, _) = watch::channel(GitManagerSignal::default());
        let mut local = VcsStatusLocalResult::non_repository();
        local.is_repo = true;
        local.ref_name = Some(ref_name.to_owned());
        let tasks = TaskTracker::new();
        let retirement_cancellation = CancellationToken::new();
        let retirement_wait = retirement_cancellation.clone();
        tasks.spawn(async move {
            retirement_wait.cancelled().await;
        });
        tasks.close();
        broadcaster.lock_state().repositories.insert(
            cwd.to_path_buf(),
            RepositoryState {
                lifecycle_id,
                repository_key: None,
                fetch_attachment_in_flight: false,
                fetch_attachment_requested: false,
                local,
                remote: Some(Some(VcsStatusRemoteResult {
                    has_upstream: true,
                    ahead_count,
                    behind_count: 0,
                    ahead_of_default_count: Some(0),
                    pr: None,
                })),
                remote_fence: None,
                remote_ref_name: None,
                pending_local_reconcile: false,
                remote_refresh_requests,
                git_manager_signature: None,
                git_manager_signal,
                git_manager_read_lock: Arc::new(AsyncMutex::new(())),
                git_manager_common_dir: Some(cwd.join(".git")),
                subscribers: HashMap::from([(
                    lifecycle_id,
                    RepositorySubscriber::Status {
                        sender,
                        fell_behind: Arc::new(AtomicBool::new(false)),
                    },
                )]),
                poller_cancellation: CancellationToken::new(),
                retirement_cancellation,
                tasks,
            },
        );
        receiver
    }

    #[derive(Default)]
    struct RecordingSignalRunner {
        operations: Mutex<Vec<String>>,
    }

    impl GitProcessRunner for RecordingSignalRunner {
        fn run<'a>(
            &'a self,
            request: ProcessRequest,
            cancellation: &'a CancellationToken,
        ) -> BoxGitProcessFuture<'a> {
            self.operations
                .lock()
                .expect("recorded operations")
                .push(request.operation.clone());
            Box::pin(async move { ProcessRunner.run(request, cancellation).await })
        }
    }

    #[tokio::test]
    async fn external_git_add_does_not_read_signal_and_watcher_loss_only_changes_health() {
        let _native_watcher_permit = super::super::acquire_native_watcher_test_permit().await;
        let root = tempfile::tempdir().expect("signal index fixture");
        let cwd = fs::canonicalize(root.path()).expect("canonical fixture");
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .args(args)
                .current_dir(&cwd)
                .output()
                .expect("fixture Git");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.name", "Signal Test"]);
        git(&["config", "user.email", "signal@example.test"]);
        fs::write(cwd.join("tracked.txt"), "base\n").expect("base file");
        git(&["add", "tracked.txt"]);
        git(&["-c", "commit.gpgSign=false", "commit", "-q", "-m", "base"]);
        let runner = Arc::new(RecordingSignalRunner::default());
        let (fetch_interval, _) = watch::channel(Duration::from_secs(180));
        let broadcaster = StatusBroadcaster::with_automatic_remote_refresh_interval(
            Arc::new(GitRepository::with_runner_for_test(runner.clone())),
            Duration::from_secs(60),
            fetch_interval,
            4,
        );
        let mut subscription = test_timeout(
            "Git Manager signal subscription setup",
            broadcaster.subscribe_git_manager_signal(cwd.clone(), CancellationToken::new()),
        )
        .await
        .expect("signal subscription");
        let initial = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let signal = test_timeout("subscription event", subscription.recv())
                    .await
                    .expect("signal");
                if signal.generation > 0 {
                    break signal;
                }
            }
        })
        .await
        .expect("initial signature");
        assert!(!initial.watcher_degraded);
        let operations = || {
            runner
                .operations
                .lock()
                .expect("recorded operations")
                .clone()
        };
        let before = operations()
            .iter()
            .filter(|operation| operation.starts_with("GitManager.signal."))
            .count();
        let local_before = broadcaster.physical_local_read_count_for_test(&cwd).await;
        fs::write(cwd.join("tracked.txt"), "staged\n").expect("staged file");
        git(&["add", "tracked.txt"]);
        tokio::time::timeout(
            Duration::from_secs(5),
            broadcaster.wait_for_physical_local_read_after_for_test(&cwd, local_before),
        )
        .await
        .expect("index local invalidation");
        assert_eq!(
            operations()
                .iter()
                .filter(|operation| operation.starts_with("GitManager.signal."))
                .count(),
            before
        );
        test_timeout("watcher shutdown", broadcaster.shut_down_watcher_for_test()).await;
        let degraded = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let signal = test_timeout("subscription event", subscription.recv())
                    .await
                    .expect("degraded signal");
                if signal.watcher_degraded {
                    break signal;
                }
            }
        })
        .await
        .expect("health notification without waiting for a fetch tick");
        assert_eq!(degraded.generation, initial.generation);
        drop(subscription);
        test_timeout("broadcaster shutdown", broadcaster.shutdown()).await;
    }

    struct SignalReadRunner {
        reads: AtomicUsize,
        started: Notify,
        release: Semaphore,
    }

    impl GitProcessRunner for SignalReadRunner {
        fn run<'a>(
            &'a self,
            request: ProcessRequest,
            _cancellation: &'a CancellationToken,
        ) -> BoxGitProcessFuture<'a> {
            Box::pin(async move {
                if let Some(output) = signal_head_output(&request.operation) {
                    return Ok(output);
                }
                let stdout = match request.operation.as_str() {
                    "GitManager.signal.refs" => {
                        assert!(request.args.contains(&OsString::from("refs/stash")));
                        assert!(request.env.contains(&(
                            OsString::from("GIT_OPTIONAL_LOCKS"),
                            OsString::from("0")
                        )));
                        self.reads.fetch_add(1, Ordering::SeqCst);
                        self.started.notify_one();
                        // Deliberately ignore cancellation to exercise the publication fence.
                        self.release
                            .acquire()
                            .await
                            .expect("signal read gate")
                            .forget();
                        "deadbeef\trefs/heads/main\t/repository\n"
                    }
                    operation => {
                        panic!("lightweight signal read ran unexpected operation {operation}")
                    }
                };
                Ok(ProcessOutput {
                    exit_code: 0,
                    stdout: stdout.to_owned(),
                    stderr: String::new(),
                    stdout_truncated: false,
                    stderr_truncated: false,
                })
            })
        }
    }

    #[tokio::test]
    async fn lightweight_signal_read_deduplicates_and_fences_retired_repositories() {
        let root = tempfile::tempdir().expect("signal fixture");
        let cwd = fs::canonicalize(root.path()).expect("canonical fixture");
        let runner = Arc::new(SignalReadRunner {
            reads: AtomicUsize::new(0),
            started: Notify::new(),
            release: Semaphore::new(2),
        });
        let broadcaster = StatusBroadcaster::new(
            Arc::new(GitRepository::with_runner_for_test(runner.clone())),
            Duration::from_secs(180),
            4,
        );
        let _status = install_epoch_repository(&broadcaster, &cwd);
        let signal = broadcaster.lock_state().repositories[&cwd]
            .git_manager_signal
            .clone();
        for _ in 0..2 {
            broadcaster
                .refresh_git_manager_signal_for_lifecycle(&cwd, Some(1), &CancellationToken::new())
                .await
                .expect("signal read");
        }
        assert_eq!(runner.reads.load(Ordering::SeqCst), 2);
        assert_eq!(
            signal.borrow().generation,
            1,
            "unchanged refs must not bump"
        );
        // Drain the earlier read's notification before admitting the blocked read.
        runner.started.notified().await;
        let pending = {
            let broadcaster = broadcaster.clone();
            let cwd = cwd.clone();
            tokio::spawn(async move {
                broadcaster
                    .refresh_git_manager_signal_for_lifecycle(
                        &cwd,
                        Some(1),
                        &CancellationToken::new(),
                    )
                    .await
            })
        };
        runner.started.notified().await;
        broadcaster.release(&cwd, 1);
        // A fresh generation must not receive the retired read either.
        broadcaster.await_retired_lifecycle(&cwd).await;
        let _replacement = install_epoch_repository_for_lifecycle(&broadcaster, &cwd, 2, "main", 0);
        let replacement = broadcaster.lock_state().repositories[&cwd]
            .git_manager_signal
            .clone();
        runner.release.add_permits(1);
        assert!(pending.await.expect("signal task").is_err());
        assert_eq!(signal.borrow().generation, 1);
        assert_eq!(replacement.borrow().generation, 0);
        broadcaster.shutdown().await;
    }

    #[tokio::test]
    async fn signal_read_retries_after_an_in_app_mutation_fence_settles() {
        let root = tempfile::tempdir().expect("signal fixture");
        let cwd = fs::canonicalize(root.path()).expect("canonical fixture");
        let runner = Arc::new(SignalReadRunner {
            reads: AtomicUsize::new(0),
            started: Notify::new(),
            release: Semaphore::new(0),
        });
        let broadcaster = StatusBroadcaster::new(
            Arc::new(GitRepository::with_runner_for_test(runner.clone())),
            Duration::from_secs(180),
            4,
        );
        let _status = install_epoch_repository(&broadcaster, &cwd);
        let signal = broadcaster.lock_state().repositories[&cwd]
            .git_manager_signal
            .clone();
        let pending = {
            let broadcaster = broadcaster.clone();
            let cwd = cwd.clone();
            tokio::spawn(async move {
                broadcaster
                    .refresh_git_manager_signal_for_lifecycle(
                        &cwd,
                        Some(1),
                        &CancellationToken::new(),
                    )
                    .await
            })
        };
        runner.started.notified().await;
        let mutation = broadcaster.begin_mutation(&cwd).await;
        runner.release.add_permits(1);
        assert_eq!(signal.borrow().generation, 0);
        mutation.finish().await;
        tokio::time::timeout(Duration::from_secs(5), runner.started.notified())
            .await
            .expect("fenced read must retry after settlement");
        assert_eq!(runner.reads.load(Ordering::SeqCst), 2);
        runner.release.add_permits(1);
        pending
            .await
            .expect("signal task")
            .expect("retry publishes");
        assert_eq!(signal.borrow().generation, 1);
        broadcaster.shutdown().await;
    }

    #[test]
    fn identical_ref_ticks_bump_once_and_a_changed_ref_bumps_again() {
        let (signal, _) = watch::channel(GitManagerSignal::default());
        let mut signature = None;
        let first = hash_git_manager_signature(
            "aaaaaaaa\trefs/heads/main\t/repository\n",
            &GitManagerSignalHead {
                reference: Some("main".into()),
                oid: Some("aaaaaaaa".into()),
            },
            &[],
        );
        let unchanged = hash_git_manager_signature(
            "aaaaaaaa\trefs/heads/main\t/repository\n",
            &GitManagerSignalHead {
                reference: Some("main".into()),
                oid: Some("aaaaaaaa".into()),
            },
            &[],
        );
        let changed = hash_git_manager_signature(
            "bbbbbbbb\trefs/heads/main\t/repository\n",
            &GitManagerSignalHead {
                reference: Some("main".into()),
                oid: Some("bbbbbbbb".into()),
            },
            &[],
        );

        update_git_manager_signature(&mut signature, &signal, first);
        assert_eq!(signal.borrow().generation, 1);
        update_git_manager_signature(&mut signature, &signal, unchanged);
        assert_eq!(signal.borrow().generation, 1);
        update_git_manager_signature(&mut signature, &signal, changed);
        assert_eq!(signal.borrow().generation, 2);
    }

    #[tokio::test]
    async fn git_manager_signal_subscription_starts_pollers_without_a_status_subscriber() {
        let fixture = tempfile::tempdir().expect("temporary repository");
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.name", "Git Manager Test"][..],
            &["config", "user.email", "git-manager@example.test"][..],
        ] {
            let output = Command::new("git")
                .args(args)
                .current_dir(fixture.path())
                .output()
                .expect("git fixture starts");
            assert!(output.status.success());
        }
        fs::write(fixture.path().join("tracked.txt"), "base\n").expect("fixture file");
        for args in [
            &["add", "tracked.txt"][..],
            &["commit", "-q", "-m", "base"][..],
        ] {
            let output = Command::new("git")
                .args(args)
                .current_dir(fixture.path())
                .output()
                .expect("git fixture starts");
            assert!(output.status.success());
        }
        let broadcaster = StatusBroadcaster::new(
            Arc::new(GitRepository::default()),
            Duration::from_secs(3_600),
            4,
        );

        let mut signal = broadcaster
            .subscribe_git_manager_signal(fixture.path().to_path_buf(), CancellationToken::new())
            .await
            .expect("Git Manager signal subscription");

        assert_eq!(broadcaster.active_poller_count(), 1);
        assert_eq!(broadcaster.active_status_subscriber_count_for_test(), 0);
        assert!(signal.recv().await.is_some());
        drop(signal);
        broadcaster.shutdown().await;
    }
}
