//! Short-lived, coalesced open pull-request lookups for VCS status enrichment.
//!
//! `vcs.refreshStatus` and every `subscribeVcsStatus` publication ask the
//! hosting service for the current branch's open pull request. That lookup is a
//! forge CLI run (`gh pr list`, `glab mr list`, `az repos pr list`) or a
//! Bitbucket request, taking hundreds of milliseconds to seconds, while the
//! local status takes tens. One refresh with an open status stream used to run
//! it twice, and every refresh ran it again.
//!
//! The rules:
//!
//! - Entries are keyed by (worktree path as the caller gave it, provider,
//!   branch), so a branch change or another repository never reads them.
//! - A successful answer, including "no open pull request", is reused for
//!   [`OPEN_PULL_REQUEST_LOOKUP_TTL`]. A failed or cancelled lookup is never
//!   stored; the next caller asks the forge again.
//! - Concurrent callers for one key share one in-flight lookup. It runs in its
//!   own task and holds one lease per waiting caller, so one caller leaving (a
//!   stream replacing its enrichment, a cancelled RPC) does not cancel the
//!   others; the last caller leaving cancels it.
//! - [`OpenPullRequestLookups::invalidate`] drops every entry under a path,
//!   in-flight ones included, so a lookup started before the invalidation can
//!   still answer the callers already waiting on it but is never stored.
//! - At most [`OPEN_PULL_REQUEST_LOOKUP_CAPACITY`] entries are held. When full,
//!   expired and abandoned entries go first, then the entry closest to
//!   expiring; a lookup with no room still runs, uncoalesced and unstored.

use std::{
    collections::HashMap,
    future::Future,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError, Weak},
    time::Duration,
};

use tokio::{sync::watch, time::Instant};
use tokio_util::sync::CancellationToken;

use super::{ProviderKind, ResolvedPullRequest, SourceControlProviderError};

/// How long a successful lookup answers later status reads. Short, because
/// pull requests also change outside BiBCode (merged or closed on the web).
pub(crate) const OPEN_PULL_REQUEST_LOOKUP_TTL: Duration = Duration::from_secs(10);
/// Upper bound on remembered and in-flight lookups across all worktrees.
pub(crate) const OPEN_PULL_REQUEST_LOOKUP_CAPACITY: usize = 64;

pub(crate) type LookupResult = Result<Option<ResolvedPullRequest>, SourceControlProviderError>;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct LookupKey {
    pub(crate) cwd: PathBuf,
    pub(crate) provider: ProviderKind,
    pub(crate) reference: String,
}

pub(crate) struct OpenPullRequestLookups {
    state: Mutex<State>,
}

struct State {
    ttl: Duration,
    capacity: usize,
    next_flight: u64,
    entries: HashMap<LookupKey, Entry>,
}

struct Entry {
    /// Lets an invalidation that names the canonical worktree (the status
    /// owner's form) match a key holding the caller's spelling of the path.
    canonical_cwd: PathBuf,
    slot: Slot,
}

enum Slot {
    Fresh {
        value: Option<ResolvedPullRequest>,
        expires_at: Instant,
    },
    InFlight {
        id: u64,
        lease: Weak<FlightLease>,
        result: watch::Receiver<Option<LookupResult>>,
    },
}

/// One per waiting caller; the last one dropped cancels the shared lookup.
struct FlightLease {
    cancellation: CancellationToken,
}

impl Drop for FlightLease {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

enum Admission {
    Cached(Option<ResolvedPullRequest>),
    Wait(Arc<FlightLease>, watch::Receiver<Option<LookupResult>>),
}

impl Default for OpenPullRequestLookups {
    fn default() -> Self {
        Self::with_limits(
            OPEN_PULL_REQUEST_LOOKUP_TTL,
            OPEN_PULL_REQUEST_LOOKUP_CAPACITY,
        )
    }
}

impl std::fmt::Debug for OpenPullRequestLookups {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.lock();
        formatter
            .debug_struct("OpenPullRequestLookups")
            .field("ttl", &state.ttl)
            .field("entries", &state.entries.len())
            .finish()
    }
}

impl OpenPullRequestLookups {
    pub(crate) fn with_limits(ttl: Duration, capacity: usize) -> Self {
        Self {
            state: Mutex::new(State {
                ttl,
                capacity,
                next_flight: 0,
                entries: HashMap::new(),
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn set_ttl_for_test(&self, ttl: Duration) {
        self.lock().ttl = ttl;
    }

    /// The remembered answer for `key`, or the result of the shared lookup
    /// `load` starts. `load` receives the shared lookup's own cancellation.
    /// `None` means this caller was cancelled, or the shared lookup ended
    /// without an answer (its task was aborted).
    pub(crate) async fn resolve<F, Fut>(
        self: &Arc<Self>,
        key: LookupKey,
        canonical_cwd: PathBuf,
        cancellation: &CancellationToken,
        load: F,
    ) -> Option<LookupResult>
    where
        F: FnOnce(CancellationToken) -> Fut,
        Fut: Future<Output = LookupResult> + Send + 'static,
    {
        let (_lease, mut result) = match self.admit(key, canonical_cwd, load) {
            Admission::Cached(value) => return Some(Ok(value)),
            Admission::Wait(lease, result) => (lease, result),
        };
        tokio::select! {
            biased;
            () = cancellation.cancelled() => None,
            outcome = result.wait_for(Option::is_some) => {
                outcome.ok().and_then(|outcome| outcome.clone())
            }
        }
    }

    /// Forgets every lookup for a worktree at or below `path`, matching either
    /// the caller's spelling of the path or its canonical form.
    pub(crate) fn invalidate(&self, path: &Path) {
        self.lock().entries.retain(|key, entry| {
            !(key.cwd.starts_with(path) || entry.canonical_cwd.starts_with(path))
        });
    }

    fn admit<F, Fut>(self: &Arc<Self>, key: LookupKey, canonical_cwd: PathBuf, load: F) -> Admission
    where
        F: FnOnce(CancellationToken) -> Fut,
        Fut: Future<Output = LookupResult> + Send + 'static,
    {
        let mut state = self.lock();
        let now = Instant::now();
        if let Some(entry) = state.entries.get(&key) {
            match &entry.slot {
                Slot::Fresh { value, expires_at } if *expires_at > now => {
                    return Admission::Cached(value.clone());
                }
                Slot::InFlight { lease, result, .. } => {
                    if let Some(lease) = lease.upgrade() {
                        return Admission::Wait(lease, result.clone());
                    }
                }
                Slot::Fresh { .. } => {}
            }
        }
        state.entries.remove(&key);
        state.make_room(now);

        let id = state.next_flight;
        state.next_flight = state.next_flight.wrapping_add(1);
        let flight_cancellation = CancellationToken::new();
        let lease = Arc::new(FlightLease {
            cancellation: flight_cancellation.clone(),
        });
        let (sender, result) = watch::channel(None);
        if state.entries.len() < state.capacity {
            state.entries.insert(
                key.clone(),
                Entry {
                    canonical_cwd,
                    slot: Slot::InFlight {
                        id,
                        lease: Arc::downgrade(&lease),
                        result: result.clone(),
                    },
                },
            );
        }
        let lookup = load(flight_cancellation);
        let lookups = Arc::clone(self);
        tokio::spawn(async move {
            let outcome = lookup.await;
            lookups.settle(&key, id, &outcome);
            let _ = sender.send(Some(outcome));
        });
        Admission::Wait(lease, result)
    }

    /// Stores a successful answer only while this flight still owns the key;
    /// an invalidation or a newer flight since it started means its answer may
    /// predate a change.
    fn settle(&self, key: &LookupKey, id: u64, outcome: &LookupResult) {
        let mut state = self.lock();
        let ttl = state.ttl;
        let Some(entry) = state.entries.get_mut(key) else {
            return;
        };
        if !matches!(entry.slot, Slot::InFlight { id: current, .. } if current == id) {
            return;
        }
        match outcome {
            Ok(value) => {
                entry.slot = Slot::Fresh {
                    value: value.clone(),
                    expires_at: Instant::now() + ttl,
                };
            }
            Err(_) => {
                state.entries.remove(key);
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl State {
    fn make_room(&mut self, now: Instant) {
        if self.entries.len() < self.capacity {
            return;
        }
        self.entries.retain(|_, entry| match &entry.slot {
            Slot::Fresh { expires_at, .. } => *expires_at > now,
            Slot::InFlight { lease, .. } => lease.strong_count() > 0,
        });
        while self.entries.len() >= self.capacity {
            let Some(oldest) = self
                .entries
                .iter()
                .filter_map(|(key, entry)| match entry.slot {
                    Slot::Fresh { expires_at, .. } => Some((expires_at, key)),
                    Slot::InFlight { .. } => None,
                })
                .min_by_key(|(expires_at, _)| *expires_at)
                .map(|(_, key)| key.clone())
            else {
                return;
            };
            self.entries.remove(&oldest);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::sync::Notify;

    use super::*;
    use crate::source_control::ChangeRequestState;

    fn key(cwd: &str, reference: &str) -> LookupKey {
        LookupKey {
            cwd: PathBuf::from(cwd),
            provider: ProviderKind::Github,
            reference: reference.to_owned(),
        }
    }

    fn pull_request(reference: &str) -> ResolvedPullRequest {
        ResolvedPullRequest {
            number: 7,
            title: format!("PR for {reference}"),
            url: "https://github.com/acme/repo/pull/7".to_owned(),
            base_branch: "main".to_owned(),
            head_branch: reference.to_owned(),
            state: ChangeRequestState::Open,
        }
    }

    fn failure() -> SourceControlProviderError {
        SourceControlProviderError {
            tag: "SourceControlProviderError",
            provider: ProviderKind::Github,
            operation: "resolveCurrentPullRequest".into(),
            cwd: "/repo".into(),
            command: Some("gh".into()),
            reference: Some("main".into()),
            detail: "forge unavailable".into(),
            command_failure: None,
        }
    }

    /// A loader that counts its runs and answers `answer` once released.
    #[derive(Clone)]
    struct Loader {
        runs: Arc<AtomicUsize>,
        cancelled: Arc<AtomicUsize>,
        release: Arc<Notify>,
        gated: bool,
        answer: LookupResult,
    }

    impl Loader {
        fn answering(answer: LookupResult) -> Self {
            Self {
                runs: Arc::default(),
                cancelled: Arc::default(),
                release: Arc::new(Notify::new()),
                gated: false,
                answer,
            }
        }

        fn gated(answer: LookupResult) -> Self {
            Self {
                gated: true,
                ..Self::answering(answer)
            }
        }

        fn runs(&self) -> usize {
            self.runs.load(Ordering::SeqCst)
        }

        fn load(&self) -> impl FnOnce(CancellationToken) -> BoxedLookup + use<> {
            let loader = self.clone();
            move |flight: CancellationToken| -> BoxedLookup {
                loader.runs.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    if loader.gated {
                        tokio::select! {
                            () = flight.cancelled() => {
                                loader.cancelled.fetch_add(1, Ordering::SeqCst);
                                return Err(failure());
                            }
                            () = loader.release.notified() => {}
                        }
                    }
                    loader.answer.clone()
                })
            }
        }
    }

    type BoxedLookup = std::pin::Pin<Box<dyn Future<Output = LookupResult> + Send>>;

    async fn resolve(
        lookups: &Arc<OpenPullRequestLookups>,
        key: LookupKey,
        loader: &Loader,
    ) -> Option<LookupResult> {
        let canonical = key.cwd.clone();
        lookups
            .resolve(key, canonical, &CancellationToken::new(), loader.load())
            .await
    }

    async fn wait_for_runs(loader: &Loader, expected: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while loader.runs() < expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("loader run deadline");
    }

    #[tokio::test]
    async fn answers_including_no_pull_request_are_reused_until_the_ttl_ends() {
        let lookups = Arc::new(OpenPullRequestLookups::with_limits(
            Duration::from_millis(200),
            8,
        ));
        let loader = Loader::answering(Ok(None));
        for _ in 0..3 {
            assert_eq!(
                resolve(&lookups, key("/repo", "main"), &loader).await,
                Some(Ok(None))
            );
        }
        assert_eq!(loader.runs(), 1, "absence is a successful answer");

        tokio::time::sleep(Duration::from_millis(300)).await;
        resolve(&lookups, key("/repo", "main"), &loader).await;
        assert_eq!(loader.runs(), 2, "an expired answer asks the forge again");
    }

    #[tokio::test]
    async fn keys_never_share_across_worktrees_branches_or_providers() {
        let lookups = Arc::new(OpenPullRequestLookups::default());
        let loader = Loader::answering(Ok(Some(pull_request("main"))));
        resolve(&lookups, key("/repo", "main"), &loader).await;
        resolve(&lookups, key("/repo", "feature"), &loader).await;
        resolve(&lookups, key("/other", "main"), &loader).await;
        let mut gitlab = key("/repo", "main");
        gitlab.provider = ProviderKind::Gitlab;
        resolve(&lookups, gitlab, &loader).await;
        assert_eq!(loader.runs(), 4);
    }

    #[tokio::test]
    async fn failures_are_shared_with_waiters_but_never_stored() {
        let lookups = Arc::new(OpenPullRequestLookups::default());
        let loader = Loader::gated(Err(failure()));
        let first = tokio::spawn({
            let lookups = Arc::clone(&lookups);
            let loader = loader.clone();
            async move { resolve(&lookups, key("/repo", "main"), &loader).await }
        });
        wait_for_runs(&loader, 1).await;
        let second = tokio::spawn({
            let lookups = Arc::clone(&lookups);
            let loader = loader.clone();
            async move { resolve(&lookups, key("/repo", "main"), &loader).await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        loader.release.notify_one();
        assert_eq!(first.await.unwrap(), Some(Err(failure())));
        assert_eq!(second.await.unwrap(), Some(Err(failure())));
        assert_eq!(loader.runs(), 1, "the waiter joined the running lookup");

        let recovered = Loader::answering(Ok(None));
        resolve(&lookups, key("/repo", "main"), &recovered).await;
        assert_eq!(recovered.runs(), 1, "a failure must not be remembered");
    }

    #[tokio::test]
    async fn one_waiter_leaving_does_not_cancel_the_shared_lookup_but_the_last_one_does() {
        let lookups = Arc::new(OpenPullRequestLookups::default());
        let loader = Loader::gated(Ok(Some(pull_request("main"))));
        let leaving = CancellationToken::new();
        let first = tokio::spawn({
            let lookups = Arc::clone(&lookups);
            let loader = loader.clone();
            let leaving = leaving.clone();
            async move {
                lookups
                    .resolve(
                        key("/repo", "main"),
                        PathBuf::from("/repo"),
                        &leaving,
                        loader.load(),
                    )
                    .await
            }
        });
        wait_for_runs(&loader, 1).await;
        let staying = tokio::spawn({
            let lookups = Arc::clone(&lookups);
            let loader = loader.clone();
            async move { resolve(&lookups, key("/repo", "main"), &loader).await }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        leaving.cancel();
        assert_eq!(
            first.await.unwrap(),
            None,
            "the leaving caller gets no answer"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(loader.cancelled.load(Ordering::SeqCst), 0);
        loader.release.notify_one();
        assert_eq!(
            staying.await.unwrap(),
            Some(Ok(Some(pull_request("main")))),
            "the remaining caller still receives the shared answer"
        );
        assert_eq!(loader.runs(), 1);

        // With every caller gone, the lookup itself is cancelled.
        let abandoned = Loader::gated(Ok(None));
        let only = CancellationToken::new();
        let task = tokio::spawn({
            let lookups = Arc::clone(&lookups);
            let abandoned = abandoned.clone();
            let only = only.clone();
            async move {
                lookups
                    .resolve(
                        key("/repo", "feature"),
                        PathBuf::from("/repo"),
                        &only,
                        abandoned.load(),
                    )
                    .await
            }
        });
        wait_for_runs(&abandoned, 1).await;
        only.cancel();
        assert_eq!(task.await.unwrap(), None);
        tokio::time::timeout(Duration::from_secs(5), async {
            while abandoned.cancelled.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the last caller leaving cancels the shared lookup");
    }

    #[tokio::test]
    async fn invalidation_drops_answers_and_keeps_an_earlier_lookup_from_being_stored() {
        let lookups = Arc::new(OpenPullRequestLookups::default());
        let loader = Loader::answering(Ok(None));
        resolve(&lookups, key("/private/var/repo", "main"), &loader).await;
        lookups.invalidate(Path::new("/private/var/repo"));
        resolve(&lookups, key("/private/var/repo", "main"), &loader).await;
        assert_eq!(loader.runs(), 2, "invalidation drops the stored answer");

        // The status owner names the canonical worktree; the key keeps the
        // caller's spelling.
        let canonical = Loader::answering(Ok(None));
        lookups
            .resolve(
                key("/var/repo/sub", "main"),
                PathBuf::from("/private/var/repo/sub"),
                &CancellationToken::new(),
                canonical.load(),
            )
            .await;
        lookups.invalidate(Path::new("/private/var/repo"));
        lookups
            .resolve(
                key("/var/repo/sub", "main"),
                PathBuf::from("/private/var/repo/sub"),
                &CancellationToken::new(),
                canonical.load(),
            )
            .await;
        assert_eq!(canonical.runs(), 2, "the canonical worktree matches too");

        // A lookup in flight across an invalidation answers its waiter but is
        // not remembered: it may predate the change.
        let stale = Loader::gated(Ok(None));
        let waiter = tokio::spawn({
            let lookups = Arc::clone(&lookups);
            let stale = stale.clone();
            async move { resolve(&lookups, key("/repo", "main"), &stale).await }
        });
        wait_for_runs(&stale, 1).await;
        lookups.invalidate(Path::new("/repo"));
        stale.release.notify_one();
        assert_eq!(waiter.await.unwrap(), Some(Ok(None)));
        let fresh = Loader::answering(Ok(Some(pull_request("main"))));
        assert_eq!(
            resolve(&lookups, key("/repo", "main"), &fresh).await,
            Some(Ok(Some(pull_request("main"))))
        );
        assert_eq!(fresh.runs(), 1);
    }

    #[tokio::test]
    async fn the_number_of_entries_stays_within_capacity() {
        let lookups = Arc::new(OpenPullRequestLookups::with_limits(
            OPEN_PULL_REQUEST_LOOKUP_TTL,
            2,
        ));
        let loader = Loader::answering(Ok(None));
        for reference in ["one", "two", "three", "four"] {
            resolve(&lookups, key("/repo", reference), &loader).await;
        }
        assert_eq!(lookups.lock().entries.len(), 2);
        // The newest answers survive eviction.
        resolve(&lookups, key("/repo", "four"), &loader).await;
        assert_eq!(loader.runs(), 4);
    }
}
