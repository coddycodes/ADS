use std::{
    collections::{BTreeSet, HashMap, HashSet},
    future::pending,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, Weak},
    time::Duration,
};

#[cfg(test)]
use tokio::sync::Notify;
use tokio::{sync::watch, time::Instant};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use super::{GitCommandError, GitRepository};

const FAILURE_BACKOFF_INITIAL: Duration = Duration::from_secs(30);
const FAILURE_BACKOFF_MAX: Duration = Duration::from_secs(15 * 60);

#[derive(Clone)]
pub(super) struct RepositoryFetchOwner {
    inner: Arc<Inner>,
}

struct Inner {
    repository: Arc<GitRepository>,
    interval: watch::Sender<Duration>,
    state: Mutex<State>,
    tasks: TaskTracker,
    #[cfg(test)]
    attachments_changed: Notify,
    #[cfg(test)]
    interval_observed: watch::Sender<Duration>,
    #[cfg(test)]
    workers_started: watch::Sender<u64>,
}

#[derive(Default)]
struct State {
    closed: bool,
    next_generation: u64,
    repositories: HashMap<PathBuf, RepositoryState>,
    worktree_keys: HashMap<PathBuf, PathBuf>,
}

struct RepositoryState {
    generation: u64,
    cancellation: CancellationToken,
    worktrees: HashMap<PathBuf, WorktreeState>,
}

#[derive(Clone)]
struct WorktreeState {
    subscribers: HashSet<u64>,
    ref_name: Option<String>,
    reconcile: watch::Sender<u64>,
}

struct FetchInputs {
    ref_names: BTreeSet<String>,
}

impl RepositoryFetchOwner {
    pub(super) fn new(repository: Arc<GitRepository>, interval: watch::Sender<Duration>) -> Self {
        #[cfg(test)]
        let (interval_observed, _) = watch::channel(*interval.borrow());
        #[cfg(test)]
        let (workers_started, _) = watch::channel(0);
        Self {
            inner: Arc::new(Inner {
                repository,
                interval,
                state: Mutex::new(State::default()),
                tasks: TaskTracker::new(),
                #[cfg(test)]
                attachments_changed: Notify::new(),
                #[cfg(test)]
                interval_observed,
                #[cfg(test)]
                workers_started,
            }),
        }
    }

    pub(super) fn attach(
        &self,
        repository_key: PathBuf,
        cwd: PathBuf,
        subscriber_id: u64,
        ref_name: Option<String>,
        reconcile: watch::Sender<u64>,
    ) {
        let mut cancelled = None;
        let mut spawn = None;
        {
            let mut state = self.lock_state();
            if state.closed {
                return;
            }
            if let Some(previous_key) = state.worktree_keys.get(&cwd).cloned()
                && previous_key != repository_key
            {
                cancelled = remove_worktree(&mut state, &previous_key, &cwd);
            }
            state
                .worktree_keys
                .insert(cwd.clone(), repository_key.clone());
            if !state.repositories.contains_key(&repository_key) {
                let generation = state.next_generation;
                state.next_generation = state.next_generation.wrapping_add(1);
                let cancellation = CancellationToken::new();
                state.repositories.insert(
                    repository_key.clone(),
                    RepositoryState {
                        generation,
                        cancellation: cancellation.clone(),
                        worktrees: HashMap::new(),
                    },
                );
                spawn = Some((generation, cancellation));
            }
            let repository = state
                .repositories
                .get_mut(&repository_key)
                .expect("repository owner was inserted");
            let worktree = repository
                .worktrees
                .entry(cwd)
                .or_insert_with(|| WorktreeState {
                    subscribers: HashSet::new(),
                    ref_name: ref_name.clone(),
                    reconcile: reconcile.clone(),
                });
            worktree.subscribers.insert(subscriber_id);
            worktree.ref_name = ref_name;
            worktree.reconcile = reconcile;
            if let Some((generation, cancellation)) = spawn.take() {
                self.spawn(repository_key.clone(), generation, cancellation);
            }
        }
        if let Some(cancellation) = cancelled {
            cancellation.cancel();
        }
        #[cfg(test)]
        self.inner.attachments_changed.notify_waiters();
    }

    pub(super) fn detach(&self, cwd: &Path, subscriber_id: u64) {
        let cancellation = {
            let mut state = self.lock_state();
            let Some(repository_key) = state.worktree_keys.get(cwd).cloned() else {
                return;
            };
            let Some(repository) = state.repositories.get_mut(&repository_key) else {
                return;
            };
            let Some(worktree) = repository.worktrees.get_mut(cwd) else {
                return;
            };
            worktree.subscribers.remove(&subscriber_id);
            if !worktree.subscribers.is_empty() {
                return;
            }
            remove_worktree(&mut state, &repository_key, cwd)
        };
        if let Some(cancellation) = cancellation {
            cancellation.cancel();
        }
    }

    pub(super) fn update_worktree_ref(&self, cwd: &Path, ref_name: Option<String>) {
        let mut state = self.lock_state();
        let Some(repository_key) = state.worktree_keys.get(cwd).cloned() else {
            return;
        };
        let Some(worktree) = state
            .repositories
            .get_mut(&repository_key)
            .and_then(|repository| repository.worktrees.get_mut(cwd))
        else {
            return;
        };
        worktree.ref_name = ref_name;
    }

    pub(super) fn repository_key_for_worktree(&self, cwd: &Path) -> Option<PathBuf> {
        self.lock_state().worktree_keys.get(cwd).cloned()
    }

    pub(super) fn invalidate_after_catalog_mutation(&self, repository_keys: &[PathBuf]) {
        let keys = repository_keys.iter().collect::<HashSet<_>>();
        let mut replacements = Vec::new();
        let mut cancelled = Vec::new();
        {
            let mut state = self.lock_state();
            if state.closed {
                return;
            }
            for repository_key in keys {
                let Some(previous) = state.repositories.remove(repository_key) else {
                    continue;
                };
                cancelled.push(previous.cancellation);
                let generation = state.next_generation;
                state.next_generation = state.next_generation.wrapping_add(1);
                let cancellation = CancellationToken::new();
                state.repositories.insert(
                    repository_key.clone(),
                    RepositoryState {
                        generation,
                        cancellation: cancellation.clone(),
                        worktrees: previous.worktrees,
                    },
                );
                replacements.push((repository_key.clone(), generation, cancellation));
            }
            for (repository_key, generation, cancellation) in replacements.drain(..) {
                self.spawn(repository_key, generation, cancellation);
            }
        }
        for cancellation in cancelled {
            cancellation.cancel();
        }
    }

    pub(super) async fn shutdown(&self) {
        let cancellations = {
            let mut state = self.lock_state();
            state.closed = true;
            state.worktree_keys.clear();
            let cancellations = state
                .repositories
                .drain()
                .map(|(_, repository)| repository.cancellation)
                .collect::<Vec<_>>();
            self.inner.tasks.close();
            cancellations
        };
        for cancellation in cancellations {
            cancellation.cancel();
        }
        self.inner.tasks.wait().await;
    }

    fn spawn(&self, repository_key: PathBuf, generation: u64, cancellation: CancellationToken) {
        let inner = Arc::downgrade(&self.inner);
        let repository = Arc::clone(&self.inner.repository);
        let mut interval = self.inner.interval.subscribe();
        #[cfg(test)]
        let interval_observed = self.inner.interval_observed.clone();
        #[cfg(test)]
        let workers_started = self.inner.workers_started.clone();
        self.inner.tasks.spawn(async move {
            let configured_interval = *interval.borrow_and_update();
            #[cfg(test)]
            workers_started.send_modify(|count| *count = count.wrapping_add(1));
            #[cfg(test)]
            interval_observed.send_replace(configured_interval);
            let mut next_fetch =
                (!configured_interval.is_zero()).then(|| Instant::now() + configured_interval);
            let mut failure_backoff = Duration::ZERO;
            loop {
                tokio::select! {
                    _ = cancellation.cancelled() => break,
                    changed = interval.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        failure_backoff = Duration::ZERO;
                        let configured_interval = *interval.borrow_and_update();
                        #[cfg(test)]
                        interval_observed.send_replace(configured_interval);
                        next_fetch = (!configured_interval.is_zero())
                            .then(|| Instant::now() + configured_interval);
                    }
                    _ = wait_for_deadline(next_fetch) => {
                        let result = run_fetch(
                            &inner,
                            &repository,
                            &repository_key,
                            generation,
                            &cancellation,
                        ).await;
                        if result.is_ok() {
                            failure_backoff = Duration::ZERO;
                        } else {
                            failure_backoff = next_failure_backoff(failure_backoff);
                        }
                        let configured_interval = *interval.borrow();
                        next_fetch = (!configured_interval.is_zero()).then(|| {
                            Instant::now() + configured_interval.max(failure_backoff)
                        });
                    }
                }
            }
        });
    }

    fn lock_state(&self) -> MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[cfg(test)]
    async fn fetch_now_for_test(&self, repository_key: &Path) -> Result<(), GitCommandError> {
        let generation = self
            .lock_state()
            .repositories
            .get(repository_key)
            .expect("repository fetch owner exists")
            .generation;
        run_fetch(
            &Arc::downgrade(&self.inner),
            &self.inner.repository,
            repository_key,
            generation,
            &CancellationToken::new(),
        )
        .await
    }

    #[cfg(test)]
    pub(crate) fn repository_count_for_test(&self) -> usize {
        self.lock_state().repositories.len()
    }

    #[cfg(test)]
    pub(crate) fn worktree_count_for_test(&self) -> usize {
        self.lock_state()
            .repositories
            .values()
            .map(|repository| repository.worktrees.len())
            .sum()
    }

    #[cfg(test)]
    pub(super) fn has_subscriber_for_test(&self, cwd: &Path, subscriber_id: u64) -> bool {
        let state = self.lock_state();
        state
            .worktree_keys
            .get(cwd)
            .and_then(|key| state.repositories.get(key))
            .and_then(|repository| repository.worktrees.get(cwd))
            .is_some_and(|worktree| worktree.subscribers.contains(&subscriber_id))
    }

    #[cfg(test)]
    pub(crate) async fn wait_for_worktree_count_for_test(&self, expected: usize) {
        loop {
            let notified = self.inner.attachments_changed.notified();
            if self.worktree_count_for_test() == expected {
                return;
            }
            notified.await;
        }
    }

    #[cfg(test)]
    pub(crate) async fn wait_for_interval_for_test(&self, expected: Duration) {
        let mut observed = self.inner.interval_observed.subscribe();
        loop {
            if *observed.borrow_and_update() == expected {
                return;
            }
            observed
                .changed()
                .await
                .expect("repository fetch owner remains alive");
        }
    }

    #[cfg(test)]
    async fn wait_for_worker_count_for_test(&self, expected: u64) {
        let mut workers_started = self.inner.workers_started.subscribe();
        loop {
            if *workers_started.borrow_and_update() >= expected {
                return;
            }
            workers_started
                .changed()
                .await
                .expect("repository fetch owner remains alive");
        }
    }

    #[cfg(test)]
    fn subscriber_count_for_test(&self, repository_key: &Path, cwd: &Path) -> usize {
        self.lock_state()
            .repositories
            .get(repository_key)
            .and_then(|repository| repository.worktrees.get(cwd))
            .map_or(0, |worktree| worktree.subscribers.len())
    }
}

impl Inner {
    fn fetch_inputs(&self, repository_key: &Path, generation: u64) -> Option<FetchInputs> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let repository = state.repositories.get(repository_key)?;
        if repository.generation != generation {
            return None;
        }
        Some(FetchInputs {
            ref_names: repository
                .worktrees
                .values()
                .filter_map(|worktree| worktree.ref_name.clone())
                .collect(),
        })
    }

    fn fan_out(&self, repository_key: &Path, generation: u64) {
        let reconciliations = {
            let state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let Some(repository) = state.repositories.get(repository_key) else {
                return;
            };
            if repository.generation != generation {
                return;
            }
            repository
                .worktrees
                .values()
                .map(|worktree| worktree.reconcile.clone())
                .collect::<Vec<_>>()
        };
        for reconcile in reconciliations {
            reconcile.send_modify(|generation| *generation = generation.wrapping_add(1));
        }
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for repository in state.repositories.values() {
            repository.cancellation.cancel();
        }
        self.tasks.close();
    }
}

async fn run_fetch(
    inner: &Weak<Inner>,
    repository: &GitRepository,
    repository_key: &Path,
    generation: u64,
    cancellation: &CancellationToken,
) -> Result<(), GitCommandError> {
    let Some(inputs) = inner
        .upgrade()
        .and_then(|inner| inner.fetch_inputs(repository_key, generation))
    else {
        return Ok(());
    };
    repository
        .automatic_fetch_upstream_remotes(repository_key, &inputs.ref_names, cancellation)
        .await?;
    if let Some(inner) = inner.upgrade() {
        inner.fan_out(repository_key, generation);
    }
    Ok(())
}

fn remove_worktree(
    state: &mut State,
    repository_key: &Path,
    cwd: &Path,
) -> Option<CancellationToken> {
    let repository = state.repositories.get_mut(repository_key)?;
    repository.worktrees.remove(cwd);
    if state
        .worktree_keys
        .get(cwd)
        .is_some_and(|key| key == repository_key)
    {
        state.worktree_keys.remove(cwd);
    }
    if !repository.worktrees.is_empty() {
        return None;
    }
    state
        .repositories
        .remove(repository_key)
        .map(|repository| repository.cancellation)
}

async fn wait_for_deadline(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => pending().await,
    }
}

fn next_failure_backoff(current: Duration) -> Duration {
    if current.is_zero() {
        FAILURE_BACKOFF_INITIAL
    } else {
        current.saturating_mul(2).min(FAILURE_BACKOFF_MAX)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        ffi::OsString,
        fs,
        path::{Path, PathBuf},
        process::Command,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };

    use tokio::sync::{Notify, Semaphore, watch};
    use tokio_util::sync::CancellationToken;

    use super::RepositoryFetchOwner;
    use crate::git::{
        BoxGitProcessFuture, GitProcessRunner, GitRepository, ProcessError, ProcessOutput,
        ProcessRequest,
    };
    use crate::test_support::TestSandbox;

    struct RecordingFetchRunner {
        upstreams: HashMap<String, String>,
        requests: Mutex<Vec<ProcessRequest>>,
        fetches: AtomicUsize,
        fetch_started: Notify,
        failures_remaining: AtomicUsize,
        blocked_fetches_remaining: AtomicUsize,
        release_fetch: Semaphore,
    }

    impl RecordingFetchRunner {
        fn new(upstreams: impl IntoIterator<Item = (&'static str, &'static str)>) -> Self {
            Self {
                upstreams: upstreams
                    .into_iter()
                    .map(|(branch, remote)| (branch.to_owned(), remote.to_owned()))
                    .collect(),
                requests: Mutex::new(Vec::new()),
                fetches: AtomicUsize::new(0),
                fetch_started: Notify::new(),
                failures_remaining: AtomicUsize::new(0),
                blocked_fetches_remaining: AtomicUsize::new(0),
                release_fetch: Semaphore::new(0),
            }
        }

        fn with_failures(mut self, failures: usize) -> Self {
            self.failures_remaining = AtomicUsize::new(failures);
            self
        }

        fn with_blocked_fetches(mut self, blocked: usize) -> Self {
            self.blocked_fetches_remaining = AtomicUsize::new(blocked);
            self
        }

        fn fetch_count(&self) -> usize {
            self.fetches.load(Ordering::SeqCst)
        }

        async fn wait_for_fetches(&self, expected: usize) {
            loop {
                let notified = self.fetch_started.notified();
                if self.fetch_count() >= expected {
                    return;
                }
                notified.await;
            }
        }

        fn operation_count(&self, operation: &str) -> usize {
            self.requests
                .lock()
                .expect("request log lock")
                .iter()
                .filter(|request| request.operation == operation)
                .count()
        }

        fn fetch_args(&self) -> Vec<OsString> {
            self.requests
                .lock()
                .expect("request log lock")
                .iter()
                .find(|request| request.operation == "GitVcsDriver.automaticFetch.fetch")
                .expect("automatic fetch request")
                .args
                .clone()
        }

        fn request_environment(&self, operation: &str) -> Vec<(OsString, OsString)> {
            self.requests
                .lock()
                .expect("request log lock")
                .iter()
                .find(|request| request.operation == operation)
                .expect("recorded Git request")
                .env
                .clone()
        }

        fn request_cwd(&self, operation: &str) -> PathBuf {
            self.requests
                .lock()
                .expect("request log lock")
                .iter()
                .find(|request| request.operation == operation)
                .expect("recorded Git request")
                .cwd
                .clone()
        }

        fn request_timeout(&self, operation: &str) -> Duration {
            self.requests
                .lock()
                .expect("request log lock")
                .iter()
                .find(|request| request.operation == operation)
                .expect("recorded Git request")
                .timeout
        }
    }

    impl GitProcessRunner for RecordingFetchRunner {
        fn run<'a>(
            &'a self,
            request: ProcessRequest,
            _cancellation: &'a CancellationToken,
        ) -> BoxGitProcessFuture<'a> {
            self.requests
                .lock()
                .expect("request log lock")
                .push(request.clone());
            Box::pin(async move {
                match request.operation.as_str() {
                    "GitVcsDriver.automaticFetch.upstreams" => {
                        let mut upstreams = self.upstreams.iter().collect::<Vec<_>>();
                        upstreams.sort_unstable_by_key(|(branch, _)| *branch);
                        Ok(output(
                            0,
                            upstreams
                                .into_iter()
                                .map(|(branch, remote)| format!("{branch}\0{remote}\n"))
                                .collect(),
                        ))
                    }
                    "GitVcsDriver.automaticFetch.fetch" => {
                        self.fetches.fetch_add(1, Ordering::SeqCst);
                        self.fetch_started.notify_waiters();
                        if self
                            .blocked_fetches_remaining
                            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                                (remaining > 0).then(|| remaining - 1)
                            })
                            .is_ok()
                        {
                            self.release_fetch
                                .acquire()
                                .await
                                .expect("fetch release remains open")
                                .forget();
                        }
                        if self
                            .failures_remaining
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
                        Ok(output(0, String::new()))
                    }
                    operation => panic!("unexpected Git operation {operation}"),
                }
            })
        }
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

    fn run_git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("fixture Git command starts");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git_succeeds(cwd: &Path, args: &[&str]) -> bool {
        Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .expect("fixture Git probe starts")
            .success()
    }

    struct FetchOwnerHarness {
        owner: RepositoryFetchOwner,
        interval: watch::Sender<Duration>,
        runner: Arc<RecordingFetchRunner>,
        repository_key: PathBuf,
        reconciliations: Vec<(PathBuf, watch::Receiver<u64>)>,
    }

    impl FetchOwnerHarness {
        fn new(runner: RecordingFetchRunner, interval: Duration) -> Self {
            let runner = Arc::new(runner);
            let repository = Arc::new(GitRepository::with_runner_for_test(runner.clone()));
            let (interval_sender, _) = watch::channel(interval);
            Self {
                owner: RepositoryFetchOwner::new(repository, interval_sender.clone()),
                interval: interval_sender,
                runner,
                repository_key: PathBuf::from("/physical/repository/.git"),
                reconciliations: Vec::new(),
            }
        }

        fn attach(&mut self, subscriber_id: u64, cwd: &str, branch: Option<&str>) {
            let cwd = PathBuf::from(cwd);
            let (reconcile, receiver) = watch::channel(0);
            self.owner.attach(
                self.repository_key.clone(),
                cwd.clone(),
                subscriber_id,
                branch.map(str::to_owned),
                reconcile,
            );
            self.reconciliations.push((cwd, receiver));
        }

        async fn fetch_now(&self) {
            self.owner
                .fetch_now_for_test(&self.repository_key)
                .await
                .expect("controlled automatic fetch");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn five_worktrees_share_one_fetch_per_interval() {
        let mut harness = FetchOwnerHarness::new(
            RecordingFetchRunner::new([("main", "origin")]),
            Duration::from_secs(30),
        );
        for index in 0..5 {
            harness.attach(index, &format!("/repo/worktree-{index}"), Some("main"));
        }
        harness.owner.wait_for_worker_count_for_test(1).await;

        tokio::time::advance(Duration::from_secs(30)).await;
        harness.runner.wait_for_fetches(1).await;

        assert_eq!(harness.runner.fetch_count(), 1);
        assert_eq!(harness.owner.repository_count_for_test(), 1);
        assert_eq!(
            harness
                .runner
                .operation_count("GitVcsDriver.automaticFetch.upstreams"),
            1,
            "upstream discovery is per physical repository, not per worktree"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn zero_disables_and_live_interval_change_rearms_without_restart() {
        let mut harness = FetchOwnerHarness::new(
            RecordingFetchRunner::new([("main", "origin")]),
            Duration::ZERO,
        );
        harness.attach(1, "/repo/main", Some("main"));
        harness.owner.wait_for_worker_count_for_test(1).await;

        tokio::time::advance(Duration::from_secs(60)).await;
        assert_eq!(harness.runner.fetch_count(), 0);

        harness.interval.send_replace(Duration::from_secs(30));
        harness
            .owner
            .wait_for_interval_for_test(Duration::from_secs(30))
            .await;
        tokio::time::advance(Duration::from_secs(30)).await;
        harness.runner.wait_for_fetches(1).await;
        assert_eq!(harness.runner.fetch_count(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn failed_fetch_uses_the_existing_bounded_backoff() {
        let mut harness = FetchOwnerHarness::new(
            RecordingFetchRunner::new([("main", "origin")]).with_failures(1),
            Duration::from_secs(5),
        );
        harness.attach(1, "/repo/main", Some("main"));
        harness.owner.wait_for_worker_count_for_test(1).await;

        tokio::time::advance(Duration::from_secs(5)).await;
        harness.runner.wait_for_fetches(1).await;
        tokio::time::advance(Duration::from_secs(29)).await;
        assert_eq!(harness.runner.fetch_count(), 1);
        tokio::time::advance(Duration::from_secs(1)).await;
        harness.runner.wait_for_fetches(2).await;
        assert_eq!(harness.runner.fetch_count(), 2);
    }

    #[tokio::test]
    async fn shutdown_waits_for_a_cancellation_ignoring_fetch_to_finish() {
        let mut harness = FetchOwnerHarness::new(
            RecordingFetchRunner::new([("main", "origin")]).with_blocked_fetches(1),
            Duration::from_millis(1),
        );
        harness.attach(1, "/repo/main", Some("main"));
        harness.owner.wait_for_worker_count_for_test(1).await;
        harness.runner.wait_for_fetches(1).await;

        let owner = harness.owner.clone();
        let shutdown = tokio::spawn(async move { owner.shutdown().await });
        tokio::task::yield_now().await;

        assert!(
            !shutdown.is_finished(),
            "shutdown must retain and await a fetch that ignores cancellation"
        );
        harness.runner.release_fetch.add_permits(1);
        tokio::time::timeout(Duration::from_secs(1), shutdown)
            .await
            .expect("fetch-owner shutdown completes after the fetch exits")
            .expect("fetch-owner shutdown task joins");
        assert_eq!(harness.owner.repository_count_for_test(), 0);
        assert_eq!(harness.owner.worktree_count_for_test(), 0);
    }

    #[tokio::test]
    async fn duplicate_attach_and_detach_are_idempotent() {
        let mut harness = FetchOwnerHarness::new(
            RecordingFetchRunner::new([("main", "origin")]),
            Duration::ZERO,
        );
        harness.attach(7, "/repo/main", Some("main"));
        harness.attach(7, "/repo/main", Some("main"));

        assert_eq!(harness.owner.repository_count_for_test(), 1);
        assert_eq!(
            harness
                .owner
                .subscriber_count_for_test(&harness.repository_key, Path::new("/repo/main")),
            1
        );

        harness.owner.detach(Path::new("/repo/main"), 7);
        harness.owner.detach(Path::new("/repo/main"), 7);
        assert_eq!(harness.owner.repository_count_for_test(), 0);
    }

    #[tokio::test]
    async fn replaced_generation_cannot_fan_out_its_completed_fetch() {
        let mut harness = FetchOwnerHarness::new(
            RecordingFetchRunner::new([("main", "origin")]).with_blocked_fetches(1),
            Duration::ZERO,
        );
        harness.attach(1, "/repo/main", Some("main"));
        let stale_reconciliation = harness.reconciliations[0].1.clone();
        let owner = harness.owner.clone();
        let key = harness.repository_key.clone();
        let stale_fetch = tokio::spawn(async move { owner.fetch_now_for_test(&key).await });
        harness.runner.wait_for_fetches(1).await;

        harness
            .owner
            .invalidate_after_catalog_mutation(std::slice::from_ref(&harness.repository_key));
        let replacement_reconciliation = harness.reconciliations[0].1.clone();
        harness.runner.release_fetch.add_permits(1);
        stale_fetch
            .await
            .expect("stale fetch task joins")
            .expect("stale fetch dependency completed");

        assert!(
            !stale_reconciliation
                .has_changed()
                .expect("stale signal open")
        );
        assert!(
            !replacement_reconciliation
                .has_changed()
                .expect("replacement signal open"),
            "a stale owner generation must not signal its replacement"
        );

        harness.fetch_now().await;
        assert!(
            replacement_reconciliation
                .has_changed()
                .expect("replacement signal open")
        );
    }

    #[tokio::test]
    async fn one_fetch_covers_distinct_upstream_remotes_and_signals_each_worktree() {
        let mut harness = FetchOwnerHarness::new(
            RecordingFetchRunner::new([("main", "origin"), ("feature/test", "backup")]),
            Duration::ZERO,
        );
        harness.attach(1, "/repo/main", Some("main"));
        harness.attach(2, "/repo/feature", None);
        harness
            .owner
            .update_worktree_ref(Path::new("/repo/feature"), Some("feature/test".to_owned()));
        let main_reconciliation = harness.reconciliations[0].1.clone();
        let feature_reconciliation = harness.reconciliations[1].1.clone();

        harness.fetch_now().await;

        assert_eq!(harness.runner.fetch_count(), 1);
        assert_eq!(
            harness.runner.fetch_args(),
            [
                "-c",
                "http.lowSpeedLimit=1000",
                "-c",
                "http.lowSpeedTime=60",
                "fetch",
                "--quiet",
                "--multiple",
                "--",
                "backup",
                "origin"
            ]
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
        );
        assert_eq!(
            harness
                .runner
                .request_timeout("GitVcsDriver.automaticFetch.fetch"),
            crate::git::BOUNDED_TRANSFER_TIMEOUT,
            "the automatic fetch has no Cancel and keeps the bounded deadline"
        );
        assert_eq!(
            crate::git::BOUNDED_TRANSFER_TIMEOUT,
            Duration::from_secs(600)
        );
        assert!(crate::git::BOUNDED_TRANSFER_TIMEOUT < crate::git::NETWORK_TRANSFER_TIMEOUT);
        assert_eq!(
            harness
                .runner
                .request_timeout("GitVcsDriver.automaticFetch.upstreams"),
            Duration::from_secs(30),
            "the upstream discovery read keeps the ordinary deadline"
        );
        assert_eq!(
            harness
                .runner
                .request_cwd("GitVcsDriver.automaticFetch.upstreams"),
            harness.repository_key
        );
        assert_eq!(
            harness
                .runner
                .request_cwd("GitVcsDriver.automaticFetch.fetch"),
            harness.repository_key
        );
        assert!(main_reconciliation.has_changed().expect("main signal open"));
        assert!(
            feature_reconciliation
                .has_changed()
                .expect("feature signal open")
        );
        assert!(
            harness
                .runner
                .request_environment("GitVcsDriver.automaticFetch.upstreams")
                .iter()
                .any(|(key, value)| key == "GIT_OPTIONAL_LOCKS" && value == "0")
        );
        assert!(
            harness
                .runner
                .request_environment("GitVcsDriver.automaticFetch.fetch")
                .iter()
                .all(|(key, _)| key != "GIT_OPTIONAL_LOCKS"),
            "fetch must retain the normal mutation environment"
        );
    }

    #[tokio::test]
    async fn option_shaped_remote_name_is_an_exact_fetch_operand() {
        let mut harness = FetchOwnerHarness::new(
            RecordingFetchRunner::new([("main", "--all")]),
            Duration::ZERO,
        );
        harness.attach(1, "/repo/main", Some("main"));

        harness.fetch_now().await;

        assert_eq!(
            harness.runner.fetch_args(),
            [
                "-c",
                "http.lowSpeedLimit=1000",
                "-c",
                "http.lowSpeedTime=60",
                "fetch",
                "--quiet",
                "--",
                "--all"
            ]
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn real_common_dir_fetch_ignores_removed_attachment_and_does_not_broaden_option_remote() {
        let sandbox = TestSandbox::new("git-fetch-owner-real-common-dir");
        let repository_path = sandbox.path("repository");
        let remote_path = sandbox.path("remote.git");
        let other_remote_path = sandbox.path("other.git");
        let stale_path = sandbox.path("stale-worktree");
        let healthy_path = sandbox.path("healthy-worktree");
        for path in [&repository_path, &remote_path, &other_remote_path] {
            fs::create_dir_all(path).expect("Git fixture directory");
        }
        run_git(&remote_path, &["init", "--bare", "--initial-branch=main"]);
        run_git(
            &other_remote_path,
            &["init", "--bare", "--initial-branch=main"],
        );
        run_git(&repository_path, &["init", "--initial-branch=main"]);
        run_git(
            &repository_path,
            &["config", "user.name", "Coddy Studio Test"],
        );
        run_git(
            &repository_path,
            &["config", "user.email", "coddy-studio@example.invalid"],
        );
        run_git(&repository_path, &["config", "commit.gpgSign", "false"]);
        fs::write(repository_path.join("tracked.txt"), "base\n").expect("tracked fixture");
        run_git(&repository_path, &["add", "--", "tracked.txt"]);
        run_git(&repository_path, &["commit", "-m", "initial"]);
        let remote = remote_path.to_string_lossy().into_owned();
        let other_remote = other_remote_path.to_string_lossy().into_owned();
        run_git(&repository_path, &["remote", "add", "origin", &remote]);
        run_git(&repository_path, &["push", "-u", "origin", "main"]);
        run_git(&repository_path, &["branch", "stale", "main"]);
        run_git(&repository_path, &["branch", "healthy", "main"]);
        run_git(&repository_path, &["push", "-u", "origin", "stale"]);
        run_git(&repository_path, &["push", "-u", "origin", "healthy"]);
        run_git(&repository_path, &["push", &other_remote, "main"]);
        let stale = stale_path.to_string_lossy().into_owned();
        let healthy = healthy_path.to_string_lossy().into_owned();
        run_git(&repository_path, &["worktree", "add", &stale, "stale"]);
        run_git(&repository_path, &["worktree", "add", &healthy, "healthy"]);

        let config_path = repository_path.join(".git/config");
        let config = fs::read_to_string(&config_path)
            .expect("repository config")
            .replace("[remote \"origin\"]", "[remote \"--all\"]")
            .replace("refs/remotes/origin/", "refs/remotes/--all/")
            .replace("remote = origin", "remote = --all");
        fs::write(&config_path, config).expect("option-shaped remote config");
        run_git(&repository_path, &["remote", "add", "other", &other_remote]);

        let repository = Arc::new(GitRepository::default());
        let common_dir = repository
            .resolve_common_dir(&healthy_path, &CancellationToken::new())
            .await
            .expect("linked worktree common directory");
        let (interval, _) = watch::channel(Duration::ZERO);
        let owner = RepositoryFetchOwner::new(repository, interval);
        let (stale_reconcile, stale_signal) = watch::channel(0);
        let (healthy_reconcile, healthy_signal) = watch::channel(0);
        owner.attach(
            common_dir.clone(),
            stale_path.clone(),
            1,
            Some("stale".to_owned()),
            stale_reconcile,
        );
        owner.attach(
            common_dir.clone(),
            healthy_path.clone(),
            2,
            Some("healthy".to_owned()),
            healthy_reconcile,
        );
        fs::remove_dir_all(&stale_path).expect("remove stale attached worktree directory");

        owner
            .fetch_now_for_test(&common_dir)
            .await
            .expect("common-dir fetch succeeds despite stale attachment");

        assert!(
            stale_signal
                .has_changed()
                .expect("stale signal remains open")
        );
        assert!(
            healthy_signal
                .has_changed()
                .expect("healthy signal remains open"),
            "healthy sibling receives its own post-fetch reconciliation signal"
        );
        assert!(
            !git_succeeds(
                &common_dir,
                &["show-ref", "--verify", "--quiet", "refs/remotes/other/main"]
            ),
            "the option-shaped --all remote must not broaden fetch to the other remote"
        );
    }
}
