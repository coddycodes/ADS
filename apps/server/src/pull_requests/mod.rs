//! Repository-scoped hosted pull request reads and operations.

mod action;
mod cache;
pub mod checkout;
pub mod context;
pub mod error;
pub mod github;
pub mod gitlab;
pub mod host;
pub mod model;
pub mod permissions;
mod read;
pub(crate) mod snapshot_store;

use std::{
    collections::HashMap,
    future::Future,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

use tokio_util::sync::CancellationToken;

pub use context::ContextRead;
use context::{DiscoveredHosts, build_context, resolve_scope};
use error::PullRequestsOperationError;
use host::{HostCommandRunner, PullRequestHost};
use model::{
    ActionRequest, ActionResult, Checks, Commits, Context, CreateDefaults, Detail, Files, ListPage,
    ListQuery, Permission, Permissions, ReviewEvent, Snapshot, SnapshotPayload, SnapshotTab,
    SubscribeInput, SubscribeTab, Timeline, Vocabulary, VocabularyKind,
};

type ActionGateKey = (bool, String, String, u64);
type ActionGates = Mutex<HashMap<ActionGateKey, Weak<tokio::sync::Mutex<()>>>>;

/// Captures the admitted host as soon as a bounded read resolves scope, so a
/// later failure outside that read can still target the right host's
/// snapshots without re-resolving it.
#[derive(Clone, Default)]
struct HostCapture(Arc<Mutex<Option<String>>>);

impl HostCapture {
    fn set(&self, host: &str) {
        *self.0.lock().unwrap_or_else(|p| p.into_inner()) = Some(host.to_ascii_lowercase());
    }
    fn get(&self) -> Option<String> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

#[derive(Clone)]
pub struct PullRequestsService {
    runner: Arc<HostCommandRunner>,
    github: Arc<dyn PullRequestHost>,
    gitlab: Arc<dyn PullRequestHost>,
    gitlab_host: Arc<gitlab::GitLabHost>,
    action_gates: Arc<ActionGates>,
    database: Option<crate::persistence::Database>,
    pollers: Arc<gitlab::poll::Registry>,
    hosts_restored: Arc<tokio::sync::OnceCell<()>>,
    /// Per lowercased host, the generation of its last logout: a context read that
    /// started before it is not stored, even for a repository with no row yet.
    context_fences: Arc<Mutex<HashMap<String, i64>>>,
}

/// The stored context row's kind; its key is empty.
const CONTEXT_KIND: &str = "context";

impl PullRequestsService {
    /// A service with a private host observation (see `HostCommandRunner::new`);
    /// the server shares its own through `with_provider_hosts`.
    pub fn new(state_dir: PathBuf) -> Self {
        Self::with_runner(HostCommandRunner::new(state_dir))
    }

    /// Shares the server's host observation with scope resolution.
    #[must_use]
    pub fn with_provider_hosts(
        self,
        provider_hosts: Arc<crate::source_control::ProviderHosts>,
    ) -> Self {
        Self {
            database: self.database,
            pollers: self.pollers,
            ..Self::with_runner(
                self.runner
                    .as_ref()
                    .clone()
                    .with_provider_hosts(provider_hosts),
            )
        }
    }

    pub fn with_runner(runner: HostCommandRunner) -> Self {
        let runner = Arc::new(runner);
        let gitlab_host = Arc::new(gitlab::GitLabHost::new(runner.clone()));
        Self {
            github: Arc::new(github::GitHubHost::new(runner.clone())),
            gitlab: gitlab_host.clone(),
            gitlab_host,
            runner,
            action_gates: Arc::default(),
            database: None,
            pollers: Arc::default(),
            hosts_restored: Arc::default(),
            context_fences: Arc::default(),
        }
    }

    /// Snapshot reads and the poller are GitLab-only (Task 4); without a
    /// database, `readSnapshot` and `subscribe` answer `unavailable`.
    #[must_use]
    pub fn with_database(mut self, database: crate::persistence::Database) -> Self {
        self.database = Some(database);
        self
    }

    /// The admitted scope already names a supported provider.
    fn host(&self, scope: &host::HostScope) -> &dyn PullRequestHost {
        match scope.provider {
            model::PullRequestsProvider::Github => self.github.as_ref(),
            model::PullRequestsProvider::Gitlab => self.gitlab.as_ref(),
        }
    }

    /// Opening the panel or switching checkout reuses the bounded 30 s probe and
    /// host-context answers. Rescan and auth recovery pass `rescan`: never serve
    /// them from a prior account, CLI installation or repository-policy snapshot.
    pub async fn context(
        &self,
        cwd: &Path,
        read: ContextRead,
        c: &CancellationToken,
    ) -> Result<Context, PullRequestsOperationError> {
        self.read_context(cwd, read, false, c).await
    }

    /// The panel's first read: the stored context when one exists, marked
    /// `stale`, with no host call. The panel then reads live. Without a stored
    /// copy this is the live read.
    pub async fn context_allowing_stale(
        &self,
        cwd: &Path,
        c: &CancellationToken,
    ) -> Result<Context, PullRequestsOperationError> {
        self.read_context(cwd, ContextRead::Open, true, c).await
    }

    async fn read_context(
        &self,
        cwd: &Path,
        read: ContextRead,
        allow_stale: bool,
        c: &CancellationToken,
    ) -> Result<Context, PullRequestsOperationError> {
        if read == ContextRead::Rescan {
            self.invalidate_contexts();
        }
        let result = bounded_read(
            "pullRequests.getContext",
            Duration::from_secs(30),
            c,
            |c| async move {
                if allow_stale {
                    self.restore_recorded_hosts().await;
                }
                let pending = match context::resolve_pending_scope(
                    &self.runner,
                    cwd,
                    &DiscoveredHosts::default(),
                    read,
                    &c,
                )
                .await
                {
                    Ok(pending) => pending,
                    Err(unavailable) => return unavailable.into_context(),
                };
                let scope = &pending.scope;
                if allow_stale && let Some(context) = self.stored_context(scope).await {
                    return Ok(context);
                }
                let generation = snapshot_store::next_generation();
                let result = match pending
                    .read_authenticated(&self.runner, &c, |read_c| async move {
                        build_context(self.host(scope), &self.runner, scope, &read_c).await
                    })
                    .await
                {
                    Ok(context) => context,
                    Err(unavailable) => unavailable.into_context(),
                };
                self.remember_context(scope, &result, generation).await;
                result
            },
        )
        .await;
        // Availability is data on this RPC; these failures never reach the error branch.
        if let Ok(Context::Unavailable { code, host, .. }) = &result {
            if matches!(
                code,
                model::UnavailableCode::NotAuthenticated
                    | model::UnavailableCode::RepositoryUnreachable
                    | model::UnavailableCode::CliMissing
                    | model::UnavailableCode::CliTooOld
            ) {
                self.invalidate_contexts();
            }
            // Scope resolution already forgets on `unknown_host` and on its own
            // login check; a 401 from the host's context read arrives only here.
            // A Rescan that can no longer place the host at all also drops its
            // stored copies, so a restart cannot record the host from them again.
            let rejected = *code == model::UnavailableCode::NotAuthenticated
                || (*code == model::UnavailableCode::UnknownHost && read == ContextRead::Rescan);
            if rejected && let Some(host) = host {
                self.runner.provider_hosts().forget(host);
                self.delete_host_snapshots(host).await;
            }
        }
        if let Err(error) = &result {
            self.invalidate_failed_context(error, None).await;
        }
        result
    }

    async fn stored_context(&self, scope: &host::HostScope) -> Option<Context> {
        let database = self.database.clone()?;
        let host = scope.host.to_ascii_lowercase();
        let project = scope.repository.clone();
        let row = database
            .call(move |connection| {
                Ok(snapshot_store::SnapshotStore::new(connection).get(
                    &host,
                    &project,
                    CONTEXT_KIND,
                    "",
                )?)
            })
            .await
            .ok()??;
        // An empty payload is an invalidation tombstone.
        match serde_json::from_slice(&row.payload).ok()? {
            Context::Available {
                provider,
                host,
                host_version,
                repository,
                default_branch,
                account,
                repository_permission,
                merge_policy,
                capabilities,
                web_url,
                stale: _,
            } => Some(Context::Available {
                provider,
                host,
                host_version,
                repository,
                default_branch,
                account,
                repository_permission,
                merge_policy,
                capabilities,
                web_url,
                stale: true,
            }),
            Context::Unavailable { .. } => None,
        }
    }

    /// A live answer replaces the stored context; an answer saying the scope is
    /// unusable removes it. `not_authenticated` already removes every row for
    /// the host.
    async fn remember_context(
        &self,
        scope: &host::HostScope,
        result: &Result<Context, PullRequestsOperationError>,
        generation: i64,
    ) {
        let Some(database) = self.database.clone() else {
            return;
        };
        match result {
            Ok(context @ Context::Available { .. }) => {
                let Ok(payload) = serde_json::to_vec(context) else {
                    return;
                };
                let fences = self.context_fences.clone();
                let host = scope.host.to_ascii_lowercase();
                let project = scope.repository.clone();
                let _ = database
                    .call(move |connection| {
                        // Checked on the database worker: a logout's delete queued
                        // before this write has already raised the fence.
                        if fences
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .get(&host)
                            .is_some_and(|fence| generation <= *fence)
                        {
                            return Ok(());
                        }
                        Ok(snapshot_store::SnapshotStore::new(connection).put(
                            snapshot_store::Snapshot {
                                host,
                                project,
                                kind: CONTEXT_KIND.into(),
                                key: String::new(),
                                fingerprint: String::new(),
                                payload,
                                observed_at_ms: snapshot_store::now_ms(),
                                generation,
                            },
                        )?)
                    })
                    .await;
            }
            Ok(Context::Unavailable {
                code:
                    model::UnavailableCode::RepositoryUnreachable
                    | model::UnavailableCode::CliMissing
                    | model::UnavailableCode::CliTooOld,
                ..
            }) => {
                // A tombstone, not a delete: the store refuses older writes over it, and
                // this failure cannot replace a success that started later.
                gitlab::poll::store(
                    &database,
                    &scope.host,
                    &scope.repository,
                    CONTEXT_KIND,
                    "",
                    String::new(),
                    Vec::new(),
                    generation,
                )
                .await;
            }
            _ => {}
        }
    }

    /// After a restart the recorded hosts are gone, so a self-hosted origin
    /// would need CLI discovery before any stored copy could be read. A stored
    /// context exists only after an authenticated read, so its host is
    /// recorded again, as that authentication did.
    async fn restore_recorded_hosts(&self) {
        let Some(database) = self.database.clone() else {
            return;
        };
        self.hosts_restored
            .get_or_init(|| async {
                let rows = database
                    .call(|connection| {
                        Ok(snapshot_store::SnapshotStore::new(connection)
                            .list_kind(CONTEXT_KIND)?)
                    })
                    .await
                    .unwrap_or_default();
                for row in rows {
                    if let Ok(Context::Available { provider, .. }) =
                        serde_json::from_slice(&row.payload)
                    {
                        self.runner
                            .provider_hosts()
                            .record(&row.host, provider.kind());
                    }
                }
            })
            .await;
    }

    pub async fn vocabulary(
        &self,
        cwd: &Path,
        kind: VocabularyKind,
        query: Option<&str>,
        c: &CancellationToken,
    ) -> Result<Vocabulary, PullRequestsOperationError> {
        let host = HostCapture::default();
        let captured = host.clone();
        let result = bounded_read(
            "pullRequests.getVocabulary",
            Duration::from_secs(30),
            c,
            |c| async move {
                let scope = resolve_scope(&self.runner, cwd, &DiscoveredHosts::default(), &c)
                    .await
                    .map_err(|u| u.operation_error("pullRequests.getVocabulary"))?;
                captured.set(&scope.host);
                self.host(&scope).vocabulary(&scope, kind, query, &c).await
            },
        )
        .await;
        if let Err(error) = &result {
            self.invalidate_failed_context(error, host.get().as_deref())
                .await;
        }
        result
    }

    pub async fn create_defaults(
        &self,
        cwd: &Path,
        c: &CancellationToken,
    ) -> Result<CreateDefaults, PullRequestsOperationError> {
        let host = HostCapture::default();
        let captured = host.clone();
        let result = bounded_read(
            "pullRequests.getCreateDefaults",
            Duration::from_secs(30),
            c,
            |c| async move {
                let scope = resolve_scope(&self.runner, cwd, &DiscoveredHosts::default(), &c)
                    .await
                    .map_err(|u| u.operation_error("pullRequests.getCreateDefaults"))?;
                captured.set(&scope.host);
                self.host(&scope).create_defaults(&scope, &c).await
            },
        )
        .await;
        if let Err(error) = &result {
            self.invalidate_failed_context(error, host.get().as_deref())
                .await;
        }
        result
    }

    pub async fn read_snapshot(
        &self,
        input: SubscribeInput,
        c: &CancellationToken,
    ) -> Result<Snapshot, PullRequestsOperationError> {
        let operation = "pullRequests.readSnapshot";
        self.restore_recorded_hosts().await;
        bounded_read(operation, Duration::from_secs(30), c, |c| async move {
            // A database read needs the host's identity, not an authenticated
            // CLI: skip `pending.authenticate` so this stays glab-free.
            let pending = context::resolve_pending_scope(
                &self.runner,
                Path::new(&input.list.cwd),
                &DiscoveredHosts::default(),
                ContextRead::Open,
                &c,
            )
            .await
            .map_err(|u| u.operation_error(operation))?;
            let scope = pending.scope;
            let Some(database) = self.gitlab_snapshot_database(&scope) else {
                return Err(snapshot_unavailable(operation));
            };
            let host = scope.host.to_ascii_lowercase();
            let project = scope.repository.clone();
            let list_key = input.list.snapshot_key();
            let number = input.number;
            let tab = input.tab;
            let rows = database
                .call(move |connection| {
                    let store = snapshot_store::SnapshotStore::new(connection);
                    let list = store.get(&host, &project, "list", &list_key)?;
                    let detail = match number {
                        Some(number) => {
                            store.get(&host, &project, "detail", &number.to_string())?
                        }
                        None => None,
                    };
                    let tab_row = match (number, tab) {
                        (Some(number), Some(tab)) => {
                            store.get(&host, &project, tab_kind(tab), &number.to_string())?
                        }
                        _ => None,
                    };
                    Ok((list, detail, tab_row))
                })
                .await
                .map_err(|_| snapshot_unavailable(operation))?;
            Ok(build_snapshot(rows, tab))
        })
        .await
    }

    pub async fn subscribe(
        &self,
        input: SubscribeInput,
        emit: &(dyn Fn(model::Changed) + Send + Sync),
        c: &CancellationToken,
    ) -> Result<(), PullRequestsOperationError> {
        let operation = "pullRequests.subscribe";
        // The poll loop re-authenticates on every tick and backs off on failure;
        // admitting the scope here only needs the host's identity, not a CLI check.
        let pending = context::resolve_pending_scope(
            &self.runner,
            Path::new(&input.list.cwd),
            &DiscoveredHosts::default(),
            ContextRead::Open,
            c,
        )
        .await
        .map_err(|u| u.operation_error(operation))?;
        let scope = pending.scope;
        let Some(database) = self.gitlab_snapshot_database(&scope) else {
            return Err(snapshot_unavailable(operation));
        };
        let tab = gitlab::poll::ActiveTab {
            commits: input.tab == Some(SubscribeTab::Commits),
            files: input.tab == Some(SubscribeTab::Files),
        };
        let list_key = input.list.snapshot_key();
        gitlab::poll::run_subscriber(
            &self.pollers,
            &self.gitlab_host,
            &database,
            &scope,
            &input.list,
            &list_key,
            input.number,
            tab,
            emit,
            c,
        )
        .await;
        Ok(())
    }

    /// Snapshot reads and the poller are only wired for GitLab (Task 4).
    fn gitlab_snapshot_database(
        &self,
        scope: &host::HostScope,
    ) -> Option<crate::persistence::Database> {
        if scope.provider != model::PullRequestsProvider::Gitlab {
            return None;
        }
        self.database.clone()
    }

    /// Stores a successful unary read as the display copy the next
    /// `readSnapshot` paints. The poller only names what changed; these reads
    /// are what refill the rows. `generation` is taken before the host read,
    /// so a slower read that started earlier never replaces a newer row.
    async fn store_read<T: serde::Serialize>(
        &self,
        scope: &host::HostScope,
        kind: &'static str,
        key: &str,
        fingerprint: String,
        value: &T,
        generation: i64,
    ) {
        let Some(database) = self.gitlab_snapshot_database(scope) else {
            return;
        };
        let Ok(payload) = serde_json::to_vec(value) else {
            return;
        };
        gitlab::poll::store(
            &database,
            &scope.host,
            &scope.repository,
            kind,
            key,
            fingerprint,
            payload,
            generation,
        )
        .await;
    }

    pub async fn list(
        &self,
        cwd: &Path,
        query: ListQuery,
        c: &CancellationToken,
    ) -> Result<ListPage, PullRequestsOperationError> {
        let host = HostCapture::default();
        let captured = host.clone();
        let generation = snapshot_store::next_generation();
        // Only first pages are painted from snapshots.
        let snapshot_key = query.cursor.is_none().then(|| query.snapshot_key());
        let result = bounded_read(
            "pullRequests.list",
            Duration::from_secs(60),
            c,
            |c| async move {
                let pending = context::resolve_pending_scope(
                    &self.runner,
                    cwd,
                    &DiscoveredHosts::default(),
                    ContextRead::Open,
                    &c,
                )
                .await
                .map_err(|u| u.operation_error("pullRequests.list"))?;
                let scope = &pending.scope;
                captured.set(&scope.host);
                let page = pending
                    .read_authenticated(&self.runner, &c, |read_c| async move {
                        self.host(scope).list(scope, &query, &read_c).await
                    })
                    .await
                    .map_err(|u| u.operation_error("pullRequests.list"))??;
                if let Some(key) = &snapshot_key {
                    let fingerprint = gitlab::poll::list_fingerprint(&page);
                    self.store_read(scope, "list", key, fingerprint, &page, generation)
                        .await;
                }
                Ok(page)
            },
        )
        .await;
        if let Err(error) = &result {
            self.invalidate_failed_context(error, host.get().as_deref())
                .await;
        }
        result
    }
    pub async fn get(
        &self,
        cwd: &Path,
        number: u64,
        c: &CancellationToken,
    ) -> Result<Detail, PullRequestsOperationError> {
        let operation = "pullRequests.get";
        let host = HostCapture::default();
        let captured = host.clone();
        let generation = snapshot_store::next_generation();
        let result = bounded_read(operation, Duration::from_secs(60), c, |c| async move {
            let scope = resolve_scope(&self.runner, cwd, &DiscoveredHosts::default(), &c)
                .await
                .map_err(|u| u.operation_error(operation))?;
            captured.set(&scope.host);
            let host = self.host(&scope);
            let context = host.context(&scope, &c).await.map_err(|mut e| {
                e.operation = operation.into();
                e
            })?;
            let raw = host.detail(&scope, number, &context, &c).await?;
            let (permissions, readiness) = permissions::compute(&raw.inputs);
            let mut detail = raw.detail_without_permissions;
            for reviewer in &mut detail.reviewers {
                reviewer.can_rerequest = permissions::can_rerequest(&permissions, reviewer.state);
            }
            detail.permissions = permissions;
            detail.readiness = readiness;
            // The poller's probe row, not this one, holds the change fingerprint.
            self.store_read(
                &scope,
                "detail",
                &number.to_string(),
                String::new(),
                &detail,
                generation,
            )
            .await;
            Ok(detail)
        })
        .await;
        if let Err(error) = &result {
            self.invalidate_failed_context(error, host.get().as_deref())
                .await;
        }
        result
    }

    pub async fn timeline(
        &self,
        cwd: &Path,
        number: u64,
        c: &CancellationToken,
    ) -> Result<Timeline, PullRequestsOperationError> {
        let operation = "pullRequests.getTimeline";
        let host = HostCapture::default();
        let captured = host.clone();
        let generation = snapshot_store::next_generation();
        let result = bounded_read(operation, Duration::from_secs(60), c, |c| async move {
            let scope = resolve_scope(&self.runner, cwd, &DiscoveredHosts::default(), &c)
                .await
                .map_err(|u| u.operation_error(operation))?;
            captured.set(&scope.host);
            let value = self.host(&scope).timeline(&scope, number, &c).await?;
            self.store_read(
                &scope,
                "timeline",
                &number.to_string(),
                String::new(),
                &value,
                generation,
            )
            .await;
            Ok(value)
        })
        .await;
        if let Err(error) = &result {
            self.invalidate_failed_context(error, host.get().as_deref())
                .await;
        }
        result
    }

    pub async fn commits(
        &self,
        cwd: &Path,
        number: u64,
        c: &CancellationToken,
    ) -> Result<Commits, PullRequestsOperationError> {
        let operation = "pullRequests.getCommits";
        let host = HostCapture::default();
        let captured = host.clone();
        let generation = snapshot_store::next_generation();
        let result = bounded_read(operation, Duration::from_secs(60), c, |c| async move {
            let scope = resolve_scope(&self.runner, cwd, &DiscoveredHosts::default(), &c)
                .await
                .map_err(|u| u.operation_error(operation))?;
            captured.set(&scope.host);
            let value = self.host(&scope).commits(&scope, number, &c).await?;
            self.store_read(
                &scope,
                "commits",
                &number.to_string(),
                String::new(),
                &value,
                generation,
            )
            .await;
            Ok(value)
        })
        .await;
        if let Err(error) = &result {
            self.invalidate_failed_context(error, host.get().as_deref())
                .await;
        }
        result
    }

    pub async fn checks(
        &self,
        cwd: &Path,
        number: u64,
        c: &CancellationToken,
    ) -> Result<Checks, PullRequestsOperationError> {
        let operation = "pullRequests.getChecks";
        let host = HostCapture::default();
        let captured = host.clone();
        let generation = snapshot_store::next_generation();
        let result = bounded_read(operation, Duration::from_secs(60), c, |c| async move {
            let scope = resolve_scope(&self.runner, cwd, &DiscoveredHosts::default(), &c)
                .await
                .map_err(|u| u.operation_error(operation))?;
            captured.set(&scope.host);
            let value = self.host(&scope).checks(&scope, number, &c).await?;
            self.store_read(
                &scope,
                "checks",
                &number.to_string(),
                String::new(),
                &value,
                generation,
            )
            .await;
            Ok(value)
        })
        .await;
        if let Err(error) = &result {
            self.invalidate_failed_context(error, host.get().as_deref())
                .await;
        }
        result
    }

    pub async fn files(
        &self,
        cwd: &Path,
        number: u64,
        c: &CancellationToken,
    ) -> Result<Files, PullRequestsOperationError> {
        let operation = "pullRequests.getFiles";
        let host = HostCapture::default();
        let captured = host.clone();
        let generation = snapshot_store::next_generation();
        let result = bounded_read(operation, Duration::from_secs(60), c, |c| async move {
            let scope = resolve_scope(&self.runner, cwd, &DiscoveredHosts::default(), &c)
                .await
                .map_err(|u| u.operation_error(operation))?;
            captured.set(&scope.host);
            let value = self.host(&scope).files(&scope, number, &c).await?;
            self.store_read(
                &scope,
                "files",
                &number.to_string(),
                String::new(),
                &value,
                generation,
            )
            .await;
            Ok(value)
        })
        .await;
        if let Err(error) = &result {
            self.invalidate_failed_context(error, host.get().as_deref())
                .await;
        }
        result
    }

    pub async fn run_action(
        &self,
        action: &ActionRequest,
        c: &CancellationToken,
    ) -> Result<ActionResult, PullRequestsOperationError> {
        let operation = "pullRequests.runAction";
        action::validate(action)?;
        let host = HostCapture::default();
        let captured = host.clone();
        let result = bounded_read(operation, Duration::from_secs(60), c, |c| async move {
            let scope = resolve_scope(
                &self.runner,
                Path::new(&action.target().cwd),
                &DiscoveredHosts::default(),
                &c,
            )
            .await
            .map_err(|u| u.operation_error(operation))?;
            captured.set(&scope.host);
            let result = self.run_scoped_action(&scope, action, &c).await?;
            // Merges, state changes, reverts and deletions can change the tab totals.
            self.host(&scope).invalidate_totals(&scope);
            if scope.provider == model::PullRequestsProvider::Gitlab
                && let Some(database) = self.database.clone()
            {
                let host = scope.host.to_ascii_lowercase();
                let project = scope.repository.clone();
                let number = action.target().number;
                let _ = database
                    .call(move |connection| {
                        Ok(snapshot_store::SnapshotStore::new(connection)
                            .delete_number(&host, &project, number)?)
                    })
                    .await;
            }
            Ok(result)
        })
        .await;
        if let Err(error) = &result {
            // The code only: messages can carry host names and paths.
            tracing::warn!(operation, code = error.code, "pull request action failed");
            self.invalidate_failed_context(error, host.get().as_deref())
                .await;
        }
        result.map_err(|mut error| {
            error.operation = operation.into();
            error
        })
    }

    async fn acquire_action_gate(
        &self,
        scope: &host::HostScope,
        number: u64,
        operation: &str,
        c: &CancellationToken,
    ) -> Result<tokio::sync::OwnedMutexGuard<()>, PullRequestsOperationError> {
        let gate = {
            let mut gates = self.action_gates.lock().unwrap_or_else(|p| p.into_inner());
            gates.retain(|_, gate| gate.strong_count() > 0);
            let key = (
                scope.provider == model::PullRequestsProvider::Github,
                scope.host.to_ascii_lowercase(),
                scope.repository.to_ascii_lowercase(),
                number,
            );
            if let Some(gate) = gates.get(&key).and_then(Weak::upgrade) {
                gate
            } else {
                let gate = Arc::new(tokio::sync::Mutex::new(()));
                gates.insert(key, Arc::downgrade(&gate));
                gate
            }
        };
        tokio::select! {
            biased;
            () = c.cancelled() => Err(PullRequestsOperationError::new(operation, "timeout")),
            permit = gate.lock_owned() => Ok(permit),
        }
    }

    async fn run_scoped_action(
        &self,
        scope: &host::HostScope,
        action: &ActionRequest,
        c: &CancellationToken,
    ) -> Result<ActionResult, PullRequestsOperationError> {
        let operation = "pullRequests.runAction";
        // Serialize the entire read/modify/write sequence by remote identity,
        // including callers using a different checkout or a cloned service.
        let _permit = self
            .acquire_action_gate(scope, action.target().number, operation, c)
            .await?;
        let host = self.host(scope);
        let context = host.context(scope, c).await?;
        let raw = host
            .action_detail(scope, action.target().number, &context, c)
            .await?;
        if let ActionRequest::SubmitReview { head_sha, .. } | ActionRequest::Merge { head_sha, .. } =
            action
            && head_sha != &raw.inputs.head_sha
        {
            return Err(PullRequestsOperationError::new(operation, "stale_head"));
        }
        let (permissions, _) = permissions::compute(&raw.inputs);
        let caps = &raw.inputs.capabilities;
        let supported = match action {
            ActionRequest::MinimizeComment { .. } => caps.minimize_comment,
            ActionRequest::SubmitReview {
                event: ReviewEvent::RequestChanges,
                ..
            } => caps.request_changes,
            ActionRequest::RevokeApproval { .. } => caps.revoke_approval,
            ActionRequest::RemoveOwnChangeRequest { .. } => caps.remove_own_change_request,
            ActionRequest::DismissReview { .. } => caps.dismiss_review,
            ActionRequest::ApplySuggestions { .. } => caps.apply_suggestion,
            ActionRequest::Delete { .. } => caps.delete_pull_request,
            _ => true,
        };
        for permission in action_permissions(action, &permissions) {
            if !permission.allowed {
                let mut error = PullRequestsOperationError::new(
                    operation,
                    if supported {
                        "forbidden"
                    } else {
                        "unavailable"
                    },
                );
                error.message = permission
                    .reason
                    .clone()
                    .unwrap_or_else(|| permissions::reasons::HOST_DATA.into());
                return Err(error);
            }
        }
        let method_error = match action {
            ActionRequest::Merge { method, .. } if !permissions.merge.methods.contains(method) => {
                let label = match method {
                    model::MergeMethod::Merge => "Merge commit",
                    model::MergeMethod::Squash => "Squash",
                    model::MergeMethod::Rebase => "Rebase",
                };
                Some(format!("{label} merging is not allowed in this repository"))
            }
            ActionRequest::UpdateBranch { method, .. }
                if !permissions.update_branch.methods.contains(method) =>
            {
                Some("This branch update method is not allowed in this repository".into())
            }
            _ => None,
        };
        if let Some(message) = method_error {
            let mut error = PullRequestsOperationError::new(operation, "forbidden");
            error.message = message;
            return Err(error);
        }
        host.run_action(scope, action, &raw.context, c).await
    }

    fn invalidate_contexts(&self) {
        self.runner.clear_context_probes();
        self.github.invalidate_context();
        self.gitlab.invalidate_context();
    }

    /// Rescan uses `invalidate_contexts` directly and must leave snapshots in
    /// place; only a failure that discredits the account or its access to this
    /// repository deletes them here.
    async fn invalidate_failed_context(
        &self,
        error: &PullRequestsOperationError,
        host: Option<&str>,
    ) {
        if matches!(
            error.code,
            "not_authenticated" | "forbidden" | "not_found" | "cli_missing" | "cli_too_old"
        ) {
            self.invalidate_contexts();
        }
        if matches!(error.code, "not_authenticated" | "forbidden")
            && let Some(host) = host
        {
            self.delete_host_snapshots(host).await;
        }
    }

    /// GitLab-only snapshots key on the lowercased host; a GitHub host simply
    /// has no rows to delete.
    async fn delete_host_snapshots(&self, host: &str) {
        let Some(database) = self.database.clone() else {
            return;
        };
        let host = host.to_ascii_lowercase();
        // Fence first: a context read that started before this logout must not
        // write its context back once the rows are gone.
        self.context_fences
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(host.clone(), snapshot_store::next_generation());
        let _ = database
            .call(move |connection| {
                Ok(snapshot_store::SnapshotStore::new(connection).delete_host(&host)?)
            })
            .await;
    }
}

/// Told about a request created outside `runAction` (the Git Manager's and chat's
/// `git.runStackedAction`), so that repository's cached list totals read again.
pub trait CreatedRequestObserver: Send + Sync {
    fn request_created(&self, cwd: &Path, remote: &str);
}

impl CreatedRequestObserver for PullRequestsService {
    fn request_created(&self, cwd: &Path, remote: &str) {
        let identified = self.runner.provider_hosts().identify(remote);
        // A host only CLI discovery could place has no cached totals to clear.
        if let Ok(
            context::RemoteAdmission::Named(scope) | context::RemoteAdmission::Recorded(scope),
        ) = context::scope_from_remote(cwd, remote, &identified)
        {
            self.host(&scope).invalidate_totals(&scope);
        }
    }
}

fn action_permissions<'a>(action: &ActionRequest, p: &'a Permissions) -> Vec<&'a Permission> {
    use ActionRequest::*;
    match action {
        Comment { .. } | ReplyThread { .. } => vec![&p.comment],
        EditComment { .. } => vec![&p.edit_own_comment],
        DeleteComment { .. } => vec![&p.delete_own_comment],
        MinimizeComment { .. } => vec![&p.minimize_comment],
        React { .. } => vec![&p.react],
        ResolveThread { .. } => vec![&p.resolve_threads],
        SubmitReview { event, .. } => match event {
            ReviewEvent::Comment => vec![&p.review],
            ReviewEvent::Approve => vec![&p.review, &p.approve],
            ReviewEvent::RequestChanges => vec![&p.review, &p.request_changes],
        },
        RevokeApproval { .. } => vec![&p.revoke_approval],
        RemoveOwnChangeRequest { .. } => vec![&p.remove_own_change_request],
        DismissReview { .. } => vec![&p.dismiss_review],
        RerequestReview { .. } => vec![&p.rerequest_review],
        ApplySuggestions { .. } => vec![&p.apply_suggestion],
        EditPullRequest { .. } => vec![&p.edit_pull_request],
        SetReviewers { .. } => vec![&p.edit_reviewers],
        SetAssignees { .. } => vec![&p.edit_assignees],
        SetLabels { .. } => vec![&p.edit_labels],
        SetMilestone { .. } => vec![&p.edit_milestone],
        Lock { .. } => vec![&p.lock],
        Unlock { .. } => vec![&p.unlock],
        UpdateBranch { .. } => vec![&p.update_branch.permission],
        Merge { bypass, auto, .. } => {
            // These permissions include merge rights and their own readiness rules.
            // Requiring immediate merge readiness would make both paths unreachable.
            let mut required = Vec::new();
            if *bypass {
                required.push(&p.merge_bypass);
            }
            if *auto {
                required.push(&p.enable_auto_merge);
            }
            if required.is_empty() {
                required.push(&p.merge.permission);
            }
            required
        }
        DisableAutoMerge { .. } => vec![&p.disable_auto_merge],
        SetDraft { draft, .. } => vec![if *draft {
            &p.convert_to_draft
        } else {
            &p.mark_ready
        }],
        Close { .. } => vec![&p.close],
        Reopen { .. } => vec![&p.reopen],
        Delete { .. } => vec![&p.delete],
        Revert { .. } => vec![&p.revert],
    }
}

fn snapshot_unavailable(operation: &str) -> PullRequestsOperationError {
    let mut error = PullRequestsOperationError::new(operation, "unavailable");
    error.message = "Merge request snapshots are not available for this host.".into();
    error
}

/// The stored snapshot kind for one subscribed tab; conversation shares the
/// `timeline` kind since that is what the panel renders there.
fn tab_kind(tab: SubscribeTab) -> &'static str {
    match tab {
        SubscribeTab::Conversation => "timeline",
        SubscribeTab::Commits => "commits",
        SubscribeTab::Checks => "checks",
        SubscribeTab::Files => "files",
    }
}

type RawSnapshotRow = Option<snapshot_store::Snapshot>;

fn build_snapshot(
    (list, detail, tab_row): (RawSnapshotRow, RawSnapshotRow, RawSnapshotRow),
    tab: Option<SubscribeTab>,
) -> Snapshot {
    Snapshot {
        list: list.and_then(|row| {
            serde_json::from_slice(&row.payload)
                .ok()
                .map(|payload| SnapshotPayload {
                    payload,
                    observed_at: row.observed_at_ms.max(0) as u64,
                })
        }),
        detail: detail.and_then(|row| {
            serde_json::from_slice(&row.payload)
                .ok()
                .map(|payload| SnapshotPayload {
                    payload,
                    observed_at: row.observed_at_ms.max(0) as u64,
                })
        }),
        tab: match (tab_row, tab) {
            (Some(row), Some(kind)) => serde_json::from_slice::<serde_json::Value>(&row.payload)
                .ok()
                .map(|payload| SnapshotTab {
                    kind,
                    payload,
                    observed_at: row.observed_at_ms.max(0) as u64,
                }),
            _ => None,
        },
    }
}

/// One deadline includes discovery, authentication, metadata, pages and mutations.
/// On expiry/cancellation, cancel and await the owned operation so ProcessRunner can
/// finish its bounded process-tree cleanup before reporting completion.
async fn bounded_read<T, F: Future<Output = Result<T, PullRequestsOperationError>>>(
    operation: &str,
    duration: Duration,
    parent: &CancellationToken,
    read: impl FnOnce(CancellationToken) -> F,
) -> Result<T, PullRequestsOperationError> {
    let cancellation = parent.child_token();
    let _guard = cancellation.clone().drop_guard();
    let future = read(cancellation.clone());
    tokio::pin!(future);
    tokio::select! {
        biased;
        _ = parent.cancelled() => {
            cancellation.cancel();
            interrupted_result(operation, future.await)
        }
        _ = tokio::time::sleep(duration) => {
            cancellation.cancel();
            interrupted_result(operation, future.await)
        }
        result = &mut future => result,
    }
}

fn interrupted_result<T>(
    operation: &str,
    result: Result<T, PullRequestsOperationError>,
) -> Result<T, PullRequestsOperationError> {
    // A mutation may already have posted comments. Preserve its settled result
    // after cleanup instead of replacing that receipt with an ambiguous timeout.
    if operation == action::OP || operation == checkout::OP {
        result
    } else {
        Err(PullRequestsOperationError::new(operation, "timeout"))
    }
}

#[cfg(test)]
mod service_tests {
    use super::*;
    use crate::pull_requests::{error::PullRequestsOperationError, model::*};
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::Duration,
    };
    use tokio_util::sync::CancellationToken;

    struct ActionHost {
        inputs: PermissionInputs,
        calls: Arc<AtomicUsize>,
        context_calls: Option<Arc<AtomicUsize>>,
        block_first: Option<Arc<tokio::sync::Notify>>,
    }

    impl PullRequestHost for ActionHost {
        fn kind(&self) -> crate::source_control::ProviderKind {
            crate::source_control::ProviderKind::Github
        }
        fn capabilities(&self, _: Option<&host::HostVersion>) -> HostCapabilities {
            self.inputs.capabilities.clone()
        }
        fn context<'a>(
            &'a self,
            _: &'a host::HostScope,
            _: &'a CancellationToken,
        ) -> host::HostFuture<'a, host::HostContext> {
            Box::pin(async move {
                if let Some(calls) = &self.context_calls {
                    calls.fetch_add(1, Ordering::SeqCst);
                }
                Ok(host::HostContext {
                    provider: PullRequestsProvider::Github,
                    host: "github.com".into(),
                    repository: "owner/repo".into(),
                    host_version: None,
                    default_branch: "main".into(),
                    account: Actor {
                        login: self.inputs.viewer_login.clone(),
                        name: None,
                        is_bot: false,
                    },
                    repository_permission: self.inputs.repository_permission,
                    merge_policy: self.inputs.merge_policy.clone(),
                    web_url: "https://github.com/owner/repo".into(),
                    raw_host_version: None,
                    gitlab_access_level: None,
                })
            })
        }
        fn vocabulary<'a>(
            &'a self,
            _: &'a host::HostScope,
            _: VocabularyKind,
            _: Option<&'a str>,
            _: &'a CancellationToken,
        ) -> host::HostFuture<'a, Vocabulary> {
            unreachable!()
        }
        fn list<'a>(
            &'a self,
            _: &'a host::HostScope,
            _: &'a ListQuery,
            _: &'a CancellationToken,
        ) -> host::HostFuture<'a, ListPage> {
            unreachable!()
        }
        fn detail<'a>(
            &'a self,
            _: &'a host::HostScope,
            _: u64,
            _: &'a host::HostContext,
            _: &'a CancellationToken,
        ) -> host::HostFuture<'a, DetailRaw> {
            Box::pin(async move {
                let (permissions, readiness) = permissions::compute(&self.inputs);
                let value = serde_json::json!({
                    "number":14,"title":"Review","body":"","state":"open","isDraft":false,"locked":false,"lockReason":null,
                    "author":{"login":"author","name":null,"isBot":false},"createdAt":"now","updatedAt":"now","mergedAt":null,"closedAt":null,"mergedBy":null,
                    "headBranch":"feature","baseBranch":"main","headSha":self.inputs.head_sha,"baseSha":"base","isCrossRepository":false,"headRepository":null,"maintainerCanModify":true,
                    "commitCount":1,"changedFiles":1,"additions":1,"deletions":1,"labels":[],"milestone":null,"assignees":[],"reviewers":[],"approvalRules":[],"linkedIssues":[],"reactions":[],
                    "readiness":readiness,"permissions":permissions,"url":"https://github.com/owner/repo/pull/14","tabCounts":{"conversation":0,"commits":1,"checks":0,"files":1}
                });
                Ok(DetailRaw {
                    merge_commit_sha: None,
                    detail_without_permissions: serde_json::from_value(value).unwrap(),
                    inputs: self.inputs.clone(),
                })
            })
        }
        fn run_action<'a>(
            &'a self,
            _: &'a host::HostScope,
            _: &'a ActionRequest,
            _: &'a ActionContext,
            _: &'a CancellationToken,
        ) -> host::HostFuture<'a, ActionResult> {
            Box::pin(async move {
                let index = self.calls.fetch_add(1, Ordering::SeqCst);
                if index == 0
                    && let Some(release) = &self.block_first
                {
                    release.notified().await;
                }
                Ok(ActionResult::Done)
            })
        }
        fn head_ref_spec(&self, _: u64) -> String {
            unreachable!()
        }
    }

    async fn action_precheck(
        inputs: PermissionInputs,
        action: ActionRequest,
    ) -> (Result<ActionResult, PullRequestsOperationError>, usize) {
        let calls = Arc::new(AtomicUsize::new(0));
        let fake = Arc::new(ActionHost {
            inputs,
            calls: calls.clone(),
            context_calls: None,
            block_first: None,
        });
        let service = PullRequestsService {
            runner: Arc::new(HostCommandRunner::new(PathBuf::new())),
            github: fake.clone(),
            gitlab: fake,
            gitlab_host: Arc::new(gitlab::GitLabHost::new(Arc::new(HostCommandRunner::new(
                PathBuf::new(),
            )))),
            action_gates: Arc::default(),
            database: None,
            pollers: Arc::default(),
            hosts_restored: Arc::default(),
            context_fences: Arc::default(),
        };
        let scope = host::HostScope {
            cwd: PathBuf::from("/repo"),
            host: "github.com".into(),
            repository: "owner/repo".into(),
            provider: model::PullRequestsProvider::Github,
        };
        let result = service
            .run_scoped_action(&scope, &action, &CancellationToken::new())
            .await;
        (result, calls.load(Ordering::SeqCst))
    }

    fn review_action(event: ReviewEvent, head: &str) -> ActionRequest {
        ActionRequest::SubmitReview {
            target: ActionTarget {
                cwd: "/repo".into(),
                number: 14,
            },
            event,
            head_sha: head.into(),
            body: Some("review".into()),
            comments: vec![],
        }
    }

    /// Admission is the only way to a scope, and so to an adapter: an identified
    /// provider other than GitHub or GitLab is refused there, and an unidentified
    /// host waits for CLI discovery. The refusal names the provider and host on the
    /// wire, and keeps the unsupported answer ahead of a missing repository path.
    #[test]
    fn pull_requests_admit_only_supported_providers() {
        use crate::source_control::{ProviderHosts, ProviderKind};
        use context::RemoteAdmission;
        let hosts = ProviderHosts::default();
        let admit = |remote: &str| {
            context::scope_from_remote(Path::new("/checkout"), remote, &hosts.identify(remote))
        };
        for (remote, code, provider, host) in [
            (
                "https://dev.azure.com/org/project/_git/repo",
                "unsupported_provider",
                ProviderKind::AzureDevops,
                "dev.azure.com",
            ),
            (
                "git@bitbucket.org:team/repo.git",
                "unsupported_provider",
                ProviderKind::Bitbucket,
                "bitbucket.org",
            ),
            (
                "https://dev.azure.com",
                "unsupported_provider",
                ProviderKind::AzureDevops,
                "dev.azure.com",
            ),
            (
                "https://github.com",
                "no_remote",
                ProviderKind::Github,
                "github.com",
            ),
            (
                "https://git.acme.example",
                "no_remote",
                ProviderKind::Unknown,
                "git.acme.example",
            ),
        ] {
            let Err(unavailable) = admit(remote) else {
                panic!("{remote} must not be admitted");
            };
            assert_eq!(unavailable.code, code, "{remote}");
            assert_eq!(unavailable.provider, Some(provider), "{remote}");
            assert_eq!(unavailable.host.as_deref(), Some(host), "{remote}");
        }

        let custom = "https://git.acme.example/team/repo.git";
        assert!(matches!(
            admit(custom),
            Ok(RemoteAdmission::Unidentified(_))
        ));
        hosts.record("git.acme.example", ProviderKind::Gitlab);
        assert!(matches!(
            admit(custom),
            Ok(RemoteAdmission::Recorded(host::HostScope {
                provider: model::PullRequestsProvider::Gitlab,
                ..
            }))
        ));
        assert!(matches!(
            admit("git@github.com:owner/repo.git"),
            Ok(RemoteAdmission::Named(host::HostScope {
                provider: model::PullRequestsProvider::Github,
                ..
            }))
        ));
    }

    #[tokio::test]
    async fn pull_requests_actions_serialize_fresh_reads_across_checkouts_and_cancel_queued_work() {
        let calls = Arc::new(AtomicUsize::new(0));
        let contexts = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::Notify::new());
        let fake = Arc::new(ActionHost {
            inputs: permissions::tests::github_inputs(),
            calls: calls.clone(),
            context_calls: Some(contexts.clone()),
            block_first: Some(release.clone()),
        });
        let service = PullRequestsService {
            runner: Arc::new(HostCommandRunner::new(PathBuf::new())),
            github: fake.clone(),
            gitlab: fake,
            gitlab_host: Arc::new(gitlab::GitLabHost::new(Arc::new(HostCommandRunner::new(
                PathBuf::new(),
            )))),
            action_gates: Arc::default(),
            database: None,
            pollers: Arc::default(),
            hosts_restored: Arc::default(),
            context_fences: Arc::default(),
        };
        let scope = host::HostScope {
            cwd: "/checkout-a".into(),
            host: "github.com".into(),
            repository: "owner/repo".into(),
            provider: model::PullRequestsProvider::Github,
        };
        let mut other_checkout = scope.clone();
        other_checkout.cwd = "/checkout-b".into();
        other_checkout.repository = "Owner/Repo".into();
        let action =
            edit_action(serde_json::json!({"action":"setAssignees","add":["new"],"remove":[]}));
        let c = CancellationToken::new();
        let mut first = Box::pin(service.run_scoped_action(&scope, &action, &c));
        assert!(futures_util::poll!(first.as_mut()).is_pending());
        let cloned = service.clone();
        let queued_c = CancellationToken::new();
        let mut second = Box::pin(cloned.run_scoped_action(&other_checkout, &action, &queued_c));
        assert!(
            futures_util::poll!(second.as_mut()).is_pending(),
            "a second edit must wait before reading a snapshot that could lose the first edit"
        );
        assert_eq!(contexts.load(Ordering::SeqCst), 1);
        queued_c.cancel();
        assert_eq!(second.await.unwrap_err().code, "timeout");
        assert_eq!(contexts.load(Ordering::SeqCst), 1);
        let mut other_action = action.clone();
        if let ActionRequest::SetAssignees { target, .. } = &mut other_action {
            target.number = 15;
        }
        assert_eq!(
            service
                .run_scoped_action(&scope, &other_action, &c)
                .await
                .unwrap(),
            ActionResult::Done
        );
        assert_eq!(
            contexts.load(Ordering::SeqCst),
            2,
            "a different PR stays independent"
        );
        release.notify_one();
        assert_eq!(first.await.unwrap(), ActionResult::Done);
        assert_eq!(
            cloned
                .run_scoped_action(&other_checkout, &action, &c)
                .await
                .unwrap(),
            ActionResult::Done
        );
        assert_eq!(
            contexts.load(Ordering::SeqCst),
            3,
            "the next edit gets a fresh snapshot after settlement"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    fn edit_action(mut value: serde_json::Value) -> ActionRequest {
        value["cwd"] = serde_json::json!("/repo");
        value["number"] = serde_json::json!(14);
        serde_json::from_value(value).unwrap()
    }

    fn merge_action() -> serde_json::Value {
        serde_json::json!({"action":"merge","method":"squash","deleteBranch":false,
            "auto":false,"bypass":false,"headSha":"head-sha","subject":null,"body":null})
    }

    #[tokio::test]
    async fn pull_requests_merge_bad_method_never_dispatches() {
        let mut inputs = permissions::tests::github_inputs();
        inputs.merge_policy.methods = vec![MergeMethod::Merge];
        let (result, calls) = action_precheck(inputs, edit_action(merge_action())).await;
        let error = result.unwrap_err();
        assert_eq!(error.code, "forbidden");
        assert_eq!(
            error.message,
            "Squash merging is not allowed in this repository"
        );
        assert_eq!(calls, 0);
    }

    #[tokio::test]
    async fn pull_requests_merge_stale_head_never_dispatches() {
        let mut action = merge_action();
        action["headSha"] = serde_json::json!("old-head");
        for inputs in [
            permissions::tests::github_inputs(),
            permissions::tests::gitlab_inputs(),
        ] {
            let (result, calls) = action_precheck(inputs, edit_action(action.clone())).await;
            assert_eq!(result.unwrap_err().code, "stale_head");
            assert_eq!(calls, 0);
        }
    }

    #[tokio::test]
    async fn pull_requests_merge_bypass_requires_its_permission() {
        let mut action = merge_action();
        action["bypass"] = serde_json::json!(true);
        let (result, calls) =
            action_precheck(permissions::tests::github_inputs(), edit_action(action)).await;
        assert_eq!(result.unwrap_err().code, "forbidden");
        assert_eq!(calls, 0);
    }

    #[tokio::test]
    async fn pull_requests_merge_bypass_and_auto_use_their_own_readiness_permissions() {
        for gitlab in [false, true] {
            for bypass in [false, true] {
                let mut inputs = if gitlab {
                    permissions::tests::gitlab_inputs()
                } else {
                    permissions::tests::github_inputs()
                };
                if let Some(gh) = &mut inputs.gh {
                    gh.merge_state_status = "BLOCKED".into();
                    gh.viewer_can_merge_as_admin = bypass;
                }
                if let Some(gl) = &mut inputs.gl {
                    gl.detailed_merge_status = if bypass {
                        "requested_changes"
                    } else {
                        "ci_still_running"
                    }
                    .into();
                }
                assert!(!permissions::compute(&inputs).0.merge.permission.allowed);
                let mut action = merge_action();
                action["bypass"] = serde_json::json!(bypass);
                action["auto"] = serde_json::json!(!bypass);
                let (result, calls) = action_precheck(inputs, edit_action(action)).await;
                assert_eq!(result.unwrap(), ActionResult::Done);
                assert_eq!(calls, 1);
            }
        }
    }

    #[tokio::test]
    async fn pull_requests_update_branch_rejects_disallowed_method() {
        let mut inputs = permissions::tests::gitlab_inputs();
        inputs.gl.as_mut().unwrap().detailed_merge_status = "need_rebase".into();
        let (result, calls) = action_precheck(
            inputs,
            edit_action(
                serde_json::json!({"action":"updateBranch","method":"merge","skipCi":false}),
            ),
        )
        .await;
        assert_eq!(result.unwrap_err().code, "forbidden");
        assert_eq!(calls, 0);
    }

    #[tokio::test]
    async fn pull_requests_edit_state_actions_enforce_permission_rows() {
        use serde_json::json;
        for value in [
            json!({"action":"editPullRequest","title":"updated"}),
            json!({"action":"setReviewers","add":["a"],"remove":[]}),
            json!({"action":"setAssignees","add":["a"],"remove":[]}),
            json!({"action":"setLabels","add":["a"],"remove":[]}),
            json!({"action":"setMilestone","milestoneId":"1"}),
            json!({"action":"lock","reason":null}),
            json!({"action":"unlock"}),
            json!({"action":"updateBranch","method":"merge","skipCi":false}),
            merge_action(),
            json!({"action":"disableAutoMerge"}),
            json!({"action":"setDraft","draft":true}),
            json!({"action":"setDraft","draft":false}),
            json!({"action":"close"}),
            json!({"action":"reopen"}),
            json!({"action":"revert"}),
        ] {
            let mut inputs = permissions::tests::github_inputs();
            inputs.repository_permission = RepositoryPermission::Read;
            inputs.gh.as_mut().unwrap().viewer_can_update = false;
            let (result, calls) = action_precheck(inputs, edit_action(value.clone())).await;
            assert_eq!(result.unwrap_err().code, "forbidden", "{value}");
            assert_eq!(calls, 0, "{value}");
        }
        let (result, calls) = action_precheck(
            permissions::tests::github_inputs(),
            edit_action(json!({"action":"delete"})),
        )
        .await;
        assert_eq!(result.unwrap_err().code, "unavailable");
        assert_eq!(calls, 0);
        let (result, calls) = action_precheck(
            permissions::tests::gitlab_inputs(),
            edit_action(json!({"action":"delete"})),
        )
        .await;
        assert_eq!(result.unwrap_err().code, "forbidden");
        assert_eq!(calls, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn pull_requests_action_deadline_retains_partial_failure_after_cleanup() {
        let cleaned = Arc::new(AtomicUsize::new(0));
        let observed = cleaned.clone();
        let result: Result<ActionResult, PullRequestsOperationError> = bounded_read(
            "pullRequests.runAction",
            Duration::from_secs(1),
            &CancellationToken::new(),
            |c| async move {
                c.cancelled().await;
                observed.fetch_add(1, Ordering::SeqCst);
                let mut error =
                    PullRequestsOperationError::new("pullRequests.runAction", "host_rejected");
                error.message = "2 comments were posted; approval failed.".into();
                Err(error)
            },
        )
        .await;
        let error = result.unwrap_err();
        assert_eq!(error.code, "host_rejected");
        assert!(error.message.contains("2 comments were posted"));
        assert_eq!(cleaned.load(Ordering::SeqCst), 1);
    }

    /// List and action calls on an unsupported origin answer the generic wire code:
    /// `unsupported_provider` is a context-only code (not in `error::CODES`), so
    /// `operation_error` sends `unavailable`. No provider CLI runs.
    #[cfg(unix)]
    #[tokio::test]
    async fn pull_requests_unsupported_origin_operations_answer_unavailable() {
        let s = crate::test_support::TestSandbox::new("pr-unsupported-origin");
        let git = s.executable_script(
            "git",
            "printf '%s' 'https://dev.azure.com/org/project/_git/repo'",
            "",
        );
        let provider =
            s.executable_script("provider", "echo called >> provider-calls\nexit 64", "");
        let service = PullRequestsService::with_runner(
            HostCommandRunner::new(s.path("state")).with_commands(&provider, &provider, &git),
        );
        let c = CancellationToken::new();
        let cwd = s.root().to_string_lossy().into_owned();
        let query: ListQuery = serde_json::from_value(serde_json::json!({"cwd":cwd,"state":"open","search":null,"author":null,"assignee":null,"reviewer":null,"reviewStatus":null,"draft":null,"labels":[],"milestone":null,"targetBranch":null,"sort":"newest","cursor":null})).unwrap();
        let list = service.list(s.root(), query, &c).await.unwrap_err();
        assert_eq!(
            (list.operation.as_str(), list.code),
            ("pullRequests.list", "unavailable")
        );
        assert!(
            list.message.contains("GitHub and GitLab"),
            "{}",
            list.message
        );
        let comment: ActionRequest = serde_json::from_value(
            serde_json::json!({"action":"comment","cwd":cwd,"number":14,"body":"hello"}),
        )
        .unwrap();
        let action = service.run_action(&comment, &c).await.unwrap_err();
        assert_eq!(
            (action.operation.as_str(), action.code),
            ("pullRequests.runAction", "unavailable")
        );
        assert!(!s.path("provider-calls").exists(), "no provider CLI ran");
    }

    /// A request `git.runStackedAction` created re-reads its repository's GitLab
    /// totals on the next list; other repositories keep their cached totals.
    #[cfg(unix)]
    #[tokio::test]
    async fn pull_requests_created_request_rereads_only_its_repository_totals() {
        let s = crate::test_support::TestSandbox::new("pr-created-request-totals");
        let script = s.executable_script(
            "glab",
            r#"printf '%s\n' "$*" >> calls
case "$1 $2" in
  'mr list') echo '[]' ;;
  'api -i') printf 'HTTP/2 200 OK\r\nx-total: 3\r\n\r\n[]' ;;
  *) exit 64 ;;
esac"#,
            "",
        );
        let service = PullRequestsService::with_runner(
            HostCommandRunner::new(s.path("state")).with_commands(&script, &script, &script),
        );
        service.runner.provider_hosts().record(
            "git.acme.example",
            crate::source_control::ProviderKind::Gitlab,
        );
        let scope = |repository: &str| host::HostScope {
            cwd: s.root().into(),
            host: "git.acme.example".into(),
            repository: repository.into(),
            provider: model::PullRequestsProvider::Gitlab,
        };
        let query: ListQuery = serde_json::from_value(serde_json::json!({"cwd":s.root(),"state":"open","search":null,"author":null,"assignee":null,"reviewer":null,"reviewStatus":null,"draft":null,"labels":[],"milestone":null,"targetBranch":null,"sort":"newest","cursor":null})).unwrap();
        let totals_reads = || {
            std::fs::read_to_string(s.path("calls"))
                .unwrap()
                .lines()
                .filter(|line| line.starts_with("api -i"))
                .count()
        };
        let c = CancellationToken::new();
        let list_both = || async {
            for repository in ["team/sub/repo", "team/other"] {
                service
                    .gitlab
                    .list(&scope(repository), &query, &c)
                    .await
                    .unwrap();
            }
        };
        list_both().await;
        list_both().await;
        assert_eq!(totals_reads(), 6, "both repositories reuse their totals");

        service.runner.provider_hosts().forget("git.acme.example");
        service.request_created(s.root(), "git@git.acme.example:team/sub/repo.git");
        list_both().await;
        assert_eq!(
            totals_reads(),
            6,
            "an unknown provider must not reach the GitLab adapter"
        );
        service.runner.provider_hosts().record(
            "git.acme.example",
            crate::source_control::ProviderKind::Gitlab,
        );

        service.request_created(s.root(), "git@git.acme.example:team/sub/repo.git");
        list_both().await;
        assert_eq!(
            totals_reads(),
            9,
            "only the created request's repository reads its totals again"
        );
    }

    #[tokio::test]
    async fn pull_requests_action_precheck_denies_own_approval_without_host_action() {
        let mut inputs = permissions::tests::github_inputs();
        inputs.viewer_is_author = true;
        let (result, calls) =
            action_precheck(inputs, review_action(ReviewEvent::Approve, "head-sha")).await;
        let error = result.unwrap_err();
        assert_eq!(error.code, "forbidden");
        assert_eq!(error.message, "You cannot approve your own pull request.");
        assert_eq!(calls, 0);
    }

    #[tokio::test]
    async fn pull_requests_action_stale_head_denies_before_host_action() {
        let (result, calls) = action_precheck(
            permissions::tests::github_inputs(),
            review_action(ReviewEvent::Approve, "old-head"),
        )
        .await;
        assert_eq!(result.unwrap_err().code, "stale_head");
        assert_eq!(calls, 0);
    }

    #[tokio::test]
    async fn pull_requests_action_precheck_allows_review_once_and_checks_general_review_permission()
    {
        let inputs = permissions::tests::github_inputs();
        let (result, calls) = action_precheck(
            inputs.clone(),
            review_action(ReviewEvent::Approve, "head-sha"),
        )
        .await;
        assert_eq!(result.unwrap(), ActionResult::Done);
        assert_eq!(calls, 1);
        let mut inputs = inputs;
        inputs.state = PullRequestState::Closed;
        let (result, calls) =
            action_precheck(inputs, review_action(ReviewEvent::Comment, "head-sha")).await;
        assert_eq!(result.unwrap_err().message, permissions::reasons::NOT_OPEN);
        assert_eq!(calls, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn pull_requests_overall_deadline_covers_all_child_reads_and_awaits_cancellation() {
        let completed = Arc::new(AtomicUsize::new(0));
        let observed = completed.clone();
        let start = tokio::time::Instant::now();
        let result = bounded_read("pullRequests.list", Duration::from_secs(30), &CancellationToken::new(), |c| async move {
            for _ in 0..2 {
                tokio::select! {
                    _ = c.cancelled() => return Err(PullRequestsOperationError::new("pullRequests.list", "timeout")),
                    _ = tokio::time::sleep(Duration::from_secs(20)) => { observed.fetch_add(1, Ordering::SeqCst); }
                }
            }
            Ok(())
        }).await;
        assert_eq!(result.unwrap_err().code, "timeout");
        assert_eq!(completed.load(Ordering::SeqCst), 1);
        assert_eq!(start.elapsed(), Duration::from_secs(30));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pull_requests_service_scope_failure_is_context_data_and_other_reads_fail() {
        use crate::pull_requests::host::HostCommandRunner;
        use crate::test_support::TestSandbox;
        let s = TestSandbox::new("pr-service");
        let git = s.executable_script(
            "git",
            "echo \"error: No such remote 'origin'\" >&2; exit 2",
            "",
        );
        let service = PullRequestsService::with_runner(
            HostCommandRunner::new(s.path("state")).with_commands(
                "missing-gh",
                "missing-glab",
                git,
            ),
        );
        let c = CancellationToken::new();
        assert!(matches!(
            service
                .context(s.root(), ContextRead::Rescan, &c)
                .await
                .unwrap(),
            Context::Unavailable {
                code: UnavailableCode::NoRemote,
                ..
            }
        ));
        let error = service
            .vocabulary(s.root(), VocabularyKind::Labels, None, &c)
            .await
            .unwrap_err();
        assert_eq!(error.code, "unavailable");
        assert_eq!(error.operation, "pullRequests.getVocabulary");
        c.cancel();
        assert_eq!(
            service
                .context(s.root(), ContextRead::Rescan, &c)
                .await
                .unwrap_err()
                .code,
            "timeout"
        );
    }

    async fn new_database() -> crate::persistence::Database {
        let database = crate::persistence::Database::open_in_memory()
            .await
            .expect("database");
        database
            .call(|connection| Ok(crate::persistence::run_migrations(connection, None)?))
            .await
            .expect("migrations");
        database
    }

    /// A self-hosted GitLab whose host name does not say "gitlab", so only a
    /// recorded host or CLI discovery identifies it. It answers context reads
    /// until `offline` exists; every glab call is logged to `calls`.
    #[cfg(unix)]
    fn stored_context_fixture(
        name: &str,
    ) -> (
        crate::test_support::TestSandbox,
        impl Fn(&crate::persistence::Database) -> PullRequestsService,
    ) {
        let s = crate::test_support::TestSandbox::new(name);
        std::fs::write(
            s.path("project.json"),
            include_str!("../../tests/fixtures/pull_requests/gitlab_project.json"),
        )
        .unwrap();
        let glab = s.executable_script(
            "glab",
            r#"printf '%s\n' "$*" >> calls
if [ -e offline ]; then exit 64; fi
case "$1 $2" in
  '--version ') echo 'glab version 1.114.0' ;;
  'auth status') if [ -e logged-out ]; then echo 'No GitLab instances have been authenticated with glab.' >&2; exit 1; fi
    printf 'code.acme.example\n  ✓ Logged in to code.acme.example as alice\n' ;;
  'api user') echo '{"id":8,"username":"viewer","name":null}' ;;
  'api version') echo '{"version":"17.9.1"}' ;;
  'api projects/team%2Frepo') if [ -e project-404 ]; then echo 'HTTP 404' >&2; exit 1; fi; cat project.json ;;
  *) exit 64 ;;
esac"#,
            "",
        );
        let git = s.executable_script(
            "git",
            "printf '%s' 'git@code.acme.example:team/repo.git'",
            "",
        );
        let state = s.path("state");
        // Each call is a fresh process: a new runner has no recorded hosts.
        let service =
            move |database: &crate::persistence::Database| {
                PullRequestsService::with_runner(
                    HostCommandRunner::new(state.clone()).with_commands("missing-gh", &glab, &git),
                )
                .with_database(database.clone())
            };
        (s, service)
    }

    #[cfg(unix)]
    fn without_stale_marker(context: &Context) -> serde_json::Value {
        let mut value = serde_json::to_value(context).unwrap();
        value.as_object_mut().unwrap().remove("stale");
        value
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn allow_stale_answers_from_the_stored_context_without_cli_calls_even_after_restart() {
        let (s, service) = stored_context_fixture("pr-stored-context");
        let database = new_database().await;
        let first = service(&database);
        first.runner.provider_hosts().record(
            "code.acme.example",
            crate::source_control::ProviderKind::Gitlab,
        );
        let c = CancellationToken::new();
        let live = first
            .context(s.root(), ContextRead::Open, &c)
            .await
            .unwrap();
        assert!(
            matches!(live, Context::Available { stale: false, .. }),
            "{live:?}"
        );

        std::fs::write(s.path("offline"), "").unwrap();
        let calls = || {
            std::fs::read_to_string(s.path("calls"))
                .unwrap()
                .lines()
                .count()
        };
        let before = calls();
        let stale = first.context_allowing_stale(s.root(), &c).await.unwrap();
        assert!(matches!(stale, Context::Available { stale: true, .. }));
        assert_eq!(without_stale_marker(&stale), without_stale_marker(&live));

        // A restarted server has no recorded hosts: the stored row identifies
        // this self-hosted origin, so no CLI discovery runs.
        let restarted = service(&database);
        let after_restart = restarted
            .context_allowing_stale(s.root(), &c)
            .await
            .unwrap();
        assert!(matches!(
            after_restart,
            Context::Available { stale: true, .. }
        ));
        // The same holds when a restarted server's first read is a snapshot.
        let input: SubscribeInput = serde_json::from_value(serde_json::json!({"cwd":s.root(),"state":"open","search":null,"author":null,"assignee":null,"reviewer":null,"reviewStatus":null,"draft":null,"labels":[],"milestone":null,"targetBranch":null,"sort":"newest","cursor":null,"refreshTotals":false,"number":null,"tab":null})).unwrap();
        service(&database)
            .read_snapshot(input, &c)
            .await
            .expect("a snapshot read resolves the stored host");
        assert_eq!(calls(), before, "stored answers start no CLI");
    }

    /// Invalidation is fenced by read order: a read that started earlier cannot
    /// write back an invalidated context, and an older failure cannot erase a
    /// newer success.
    #[cfg(unix)]
    #[tokio::test]
    async fn stored_context_writes_and_invalidations_respect_read_order() {
        let (s, service) = stored_context_fixture("pr-stored-context-order");
        let database = new_database().await;
        let service = service(&database);
        service.runner.provider_hosts().record(
            "code.acme.example",
            crate::source_control::ProviderKind::Gitlab,
        );
        let c = CancellationToken::new();
        let live = Ok(service
            .context(s.root(), ContextRead::Open, &c)
            .await
            .unwrap());
        let unreachable = Ok(Context::Unavailable {
            code: UnavailableCode::RepositoryUnreachable,
            message: "unreachable".into(),
            provider: None,
            host: None,
            install_hint: None,
            auth_command: None,
        });
        let scope = host::HostScope {
            cwd: s.root().into(),
            host: "code.acme.example".into(),
            repository: "team/repo".into(),
            provider: model::PullRequestsProvider::Gitlab,
        };
        std::fs::write(s.path("offline"), "").unwrap();
        let stored = || async {
            matches!(
                service.context_allowing_stale(s.root(), &c).await,
                Ok(Context::Available { stale: true, .. })
            )
        };

        // A success that started before the invalidation does not restore it.
        let older = snapshot_store::next_generation();
        service
            .remember_context(&scope, &unreachable, snapshot_store::next_generation())
            .await;
        service.remember_context(&scope, &live, older).await;
        assert!(!stored().await, "an invalidated context stays invalidated");

        // A failure that started before a newer success does not erase it.
        let older = snapshot_store::next_generation();
        service
            .remember_context(&scope, &live, snapshot_store::next_generation())
            .await;
        service.remember_context(&scope, &unreachable, older).await;
        assert!(stored().await, "a newer success survives an older failure");

        // A logout fences reads that started before it, too.
        let older = snapshot_store::next_generation();
        service.delete_host_snapshots("code.acme.example").await;
        service.remember_context(&scope, &live, older).await;
        assert!(!stored().await, "a logout is not undone by an earlier read");

        // Including for a repository whose first read was still running.
        let other = host::HostScope {
            repository: "team/other".into(),
            ..scope.clone()
        };
        let older = snapshot_store::next_generation();
        service.delete_host_snapshots("code.acme.example").await;
        service.remember_context(&other, &live, older).await;
        let row = database
            .call(|connection| {
                Ok(snapshot_store::SnapshotStore::new(connection).get(
                    "code.acme.example",
                    "team/other",
                    CONTEXT_KIND,
                    "",
                )?)
            })
            .await
            .unwrap();
        assert!(
            row.is_none(),
            "an earlier first read stores nothing after a logout"
        );
    }

    /// After a full logout, a Rescan cannot place a self-hosted host at all
    /// (`unknown_host`). Its stored copies go too, so a restart cannot record the
    /// host again from them.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_rescan_that_rejects_the_host_removes_its_stored_copies() {
        let (s, service) = stored_context_fixture("pr-stored-context-rejected");
        let database = new_database().await;
        let first = service(&database);
        first.runner.provider_hosts().record(
            "code.acme.example",
            crate::source_control::ProviderKind::Gitlab,
        );
        let c = CancellationToken::new();
        first
            .context(s.root(), ContextRead::Open, &c)
            .await
            .unwrap();
        std::fs::write(s.path("logged-out"), "").unwrap();

        let rejected = first
            .context(s.root(), ContextRead::Rescan, &c)
            .await
            .unwrap();
        assert!(
            matches!(
                rejected,
                Context::Unavailable {
                    code: UnavailableCode::UnknownHost,
                    ..
                }
            ),
            "{rejected:?}"
        );
        let restarted = service(&database)
            .context_allowing_stale(s.root(), &c)
            .await;
        assert!(
            !matches!(restarted, Ok(Context::Available { .. })),
            "{restarted:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_unreachable_repository_removes_the_stored_context() {
        let (s, service) = stored_context_fixture("pr-stored-context-unreachable");
        let database = new_database().await;
        let service = service(&database);
        service.runner.provider_hosts().record(
            "code.acme.example",
            crate::source_control::ProviderKind::Gitlab,
        );
        let c = CancellationToken::new();
        service
            .context(s.root(), ContextRead::Open, &c)
            .await
            .unwrap();
        std::fs::write(s.path("project-404"), "").unwrap();

        let unavailable = service
            .context(s.root(), ContextRead::Rescan, &c)
            .await
            .unwrap();
        assert!(
            matches!(
                unavailable,
                Context::Unavailable {
                    code: UnavailableCode::RepositoryUnreachable,
                    ..
                }
            ),
            "{unavailable:?}"
        );
        let row = database
            .call(|connection| {
                Ok(snapshot_store::SnapshotStore::new(connection).get(
                    "code.acme.example",
                    "team/repo",
                    CONTEXT_KIND,
                    "",
                )?)
            })
            .await
            .unwrap();
        assert!(
            row.is_none_or(|row| row.payload.is_empty()),
            "an unusable scope keeps no stored context"
        );
        let next = service.context_allowing_stale(s.root(), &c).await.unwrap();
        assert!(matches!(next, Context::Unavailable { .. }), "{next:?}");
    }

    #[tokio::test]
    async fn gitlab_poller_read_snapshot_with_a_populated_row_performs_zero_glab_calls() {
        let s = crate::test_support::TestSandbox::new("pr-read-snapshot");
        let glab = s.executable_script("glab", "printf '%s\\n' \"$*\" >> calls\nexit 64", "");
        let git = s.executable_script(
            "git",
            "printf '%s' 'git@gitlab.acme.example:team/repo.git'",
            "",
        );
        let database = new_database().await;
        let service = PullRequestsService::with_runner(
            HostCommandRunner::new(s.path("state")).with_commands("missing-gh", &glab, &git),
        )
        .with_database(database.clone());
        service.runner.provider_hosts().record(
            "gitlab.acme.example",
            crate::source_control::ProviderKind::Gitlab,
        );
        let list_query: ListQuery = serde_json::from_value(serde_json::json!({"cwd":s.root(),"state":"open","search":null,"author":null,"assignee":null,"reviewer":null,"reviewStatus":null,"draft":null,"labels":[],"milestone":null,"targetBranch":null,"sort":"newest","cursor":null,"refreshTotals":true})).unwrap();
        let list_key = list_query.snapshot_key();
        database
            .call(move |connection| {
                Ok(snapshot_store::SnapshotStore::new(connection).put(
                    snapshot_store::Snapshot {
                        host: "gitlab.acme.example".into(),
                        project: "team/repo".into(),
                        kind: "list".into(),
                        key: list_key,
                        fingerprint: "fp".into(),
                        payload: serde_json::to_vec(&ListPage {
                            rows: vec![],
                            next_cursor: None,
                            total_count: None,
                            counts: None,
                        })
                        .unwrap(),
                        observed_at_ms: 1,
                        generation: 1,
                    },
                )?)
            })
            .await
            .unwrap();
        let input: SubscribeInput = serde_json::from_value(serde_json::json!({"cwd":s.root(),"state":"open","search":null,"author":null,"assignee":null,"reviewer":null,"reviewStatus":null,"draft":null,"labels":[],"milestone":null,"targetBranch":null,"sort":"newest","cursor":null,"refreshTotals":true,"number":null,"tab":null})).unwrap();
        let snapshot = service
            .read_snapshot(input, &CancellationToken::new())
            .await
            .unwrap();
        assert!(snapshot.list.is_some());
        assert!(!s.path("calls").exists(), "zero glab calls");
    }

    #[derive(Clone, Default)]
    struct CapturedLog(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CapturedLog {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A failed action leaves its code in the server log, never its message:
    /// messages can carry checkout paths and host names.
    #[tokio::test]
    async fn run_action_failure_logs_the_code_without_the_message() {
        let log = CapturedLog::default();
        let _logging = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .with_ansi(false)
                .with_writer({
                    let log = log.clone();
                    move || log.clone()
                })
                .finish(),
        );
        let s = crate::test_support::TestSandbox::new("pr-run-action-failure-log");
        let service = PullRequestsService::new(s.path("state"));
        let cwd = s.path("missing-checkout").to_string_lossy().into_owned();
        let action: ActionRequest = serde_json::from_value(
            serde_json::json!({"action":"comment","cwd":cwd,"number":7,"body":"hi"}),
        )
        .unwrap();

        let error = service
            .run_action(&action, &CancellationToken::new())
            .await
            .expect_err("a missing checkout cannot run an action");

        let log = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
        assert!(log.contains("pull request action failed"), "{log}");
        assert!(log.contains(&format!("code=\"{}\"", error.code)), "{log}");
        assert!(error.message.contains("missing-checkout"));
        assert!(!log.contains("missing-checkout"), "{log}");
    }

    #[tokio::test]
    async fn gitlab_poller_run_action_success_deletes_the_acted_numbers_rows_and_leaves_others() {
        let s = crate::test_support::TestSandbox::new("pr-run-action-snapshots");
        let git = s.executable_script(
            "git",
            "printf '%s' 'git@gitlab.acme.example:team/repo.git'",
            "",
        );
        let database = new_database().await;
        let glab = s.executable_script(
            "glab",
            r#"case "$1 $2" in
  '--version ') echo 'glab 1.114.0' ;;
  'auth status') printf 'gitlab.acme.example\n  ✓ Logged in to gitlab.acme.example as alice\n' ;;
  *) exit 64 ;;
esac"#,
            "",
        );
        let runner = Arc::new(HostCommandRunner::new(s.path("state")).with_commands(
            "missing-gh",
            &glab,
            &git,
        ));
        runner.provider_hosts().record(
            "gitlab.acme.example",
            crate::source_control::ProviderKind::Gitlab,
        );
        let fake = Arc::new(ActionHost {
            inputs: permissions::tests::gitlab_inputs(),
            calls: Arc::new(AtomicUsize::new(0)),
            context_calls: None,
            block_first: None,
        });
        let service = PullRequestsService {
            runner: runner.clone(),
            github: fake.clone(),
            gitlab: fake,
            gitlab_host: Arc::new(gitlab::GitLabHost::new(Arc::new(HostCommandRunner::new(
                PathBuf::new(),
            )))),
            action_gates: Arc::default(),
            database: Some(database.clone()),
            pollers: Arc::default(),
            hosts_restored: Arc::default(),
            context_fences: Arc::default(),
        };
        for (number, kind) in [
            (3941u64, "detail"),
            (3941, "timeline"),
            (3941, "commits"),
            (3941, "checks"),
            (3941, "files"),
            (42, "detail"),
        ] {
            database
                .call(move |connection| {
                    Ok(snapshot_store::SnapshotStore::new(connection).put(
                        snapshot_store::Snapshot {
                            host: "gitlab.acme.example".into(),
                            project: "team/repo".into(),
                            kind: kind.into(),
                            key: number.to_string(),
                            fingerprint: "fp".into(),
                            payload: b"x".to_vec(),
                            observed_at_ms: 1,
                            generation: 1,
                        },
                    )?)
                })
                .await
                .unwrap();
        }
        let cwd = s.root().to_string_lossy().into_owned();
        let action: ActionRequest = serde_json::from_value(
            serde_json::json!({"action":"comment","cwd":cwd,"number":3941,"body":"hi"}),
        )
        .unwrap();
        let result = service
            .run_action(&action, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result, ActionResult::Done);
        for kind in ["detail", "timeline", "commits", "checks", "files"] {
            let row = database
                .call(move |connection| {
                    Ok(snapshot_store::SnapshotStore::new(connection).get(
                        "gitlab.acme.example",
                        "team/repo",
                        kind,
                        "3941",
                    )?)
                })
                .await
                .unwrap();
            assert!(row.is_none(), "{kind} for the acted number must be gone");
        }
        let other = database
            .call(|connection| {
                Ok(snapshot_store::SnapshotStore::new(connection).get(
                    "gitlab.acme.example",
                    "team/repo",
                    "detail",
                    "42",
                )?)
            })
            .await
            .unwrap();
        assert!(other.is_some(), "other numbers remain");
    }

    #[tokio::test]
    async fn gitlab_unary_get_stores_the_detail_snapshot_read_snapshot_paints() {
        let s = crate::test_support::TestSandbox::new("pr-get-stores-snapshot");
        let git = s.executable_script(
            "git",
            "printf '%s' 'git@gitlab.acme.example:team/repo.git'",
            "",
        );
        let database = new_database().await;
        let glab = s.executable_script(
            "glab",
            r#"case "$1 $2" in
  '--version ') echo 'glab 1.114.0' ;;
  'auth status') printf 'gitlab.acme.example\n  ✓ Logged in to gitlab.acme.example as alice\n' ;;
  *) exit 64 ;;
esac"#,
            "",
        );
        let runner = Arc::new(HostCommandRunner::new(s.path("state")).with_commands(
            "missing-gh",
            &glab,
            &git,
        ));
        runner.provider_hosts().record(
            "gitlab.acme.example",
            crate::source_control::ProviderKind::Gitlab,
        );
        let fake = Arc::new(ActionHost {
            inputs: permissions::tests::gitlab_inputs(),
            calls: Arc::new(AtomicUsize::new(0)),
            context_calls: None,
            block_first: None,
        });
        let service = PullRequestsService {
            runner,
            github: fake.clone(),
            gitlab: fake,
            gitlab_host: Arc::new(gitlab::GitLabHost::new(Arc::new(HostCommandRunner::new(
                PathBuf::new(),
            )))),
            action_gates: Arc::default(),
            database: Some(database.clone()),
            pollers: Arc::default(),
            hosts_restored: Arc::default(),
            context_fences: Arc::default(),
        };
        let detail = service
            .get(s.root(), 14, &CancellationToken::new())
            .await
            .unwrap();
        let row = database
            .call(|connection| {
                Ok(snapshot_store::SnapshotStore::new(connection).get(
                    "gitlab.acme.example",
                    "team/repo",
                    "detail",
                    "14",
                )?)
            })
            .await
            .unwrap()
            .expect("a successful unary get stores the display copy");
        assert_eq!(
            serde_json::from_slice::<Detail>(&row.payload).unwrap(),
            detail,
            "the stored payload is exactly what the unary read answered"
        );

        let input: SubscribeInput = serde_json::from_value(serde_json::json!({"cwd":s.root(),"state":"open","search":null,"author":null,"assignee":null,"reviewer":null,"reviewStatus":null,"draft":null,"labels":[],"milestone":null,"targetBranch":null,"sort":"newest","cursor":null,"refreshTotals":false,"number":14,"tab":null})).unwrap();
        let snapshot = service
            .read_snapshot(input, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(snapshot.detail.map(|row| row.payload), Some(detail));
    }

    #[tokio::test]
    async fn gitlab_poller_rescan_leaves_snapshot_rows_in_place() {
        let s = crate::test_support::TestSandbox::new("pr-rescan-snapshots");
        let git = s.executable_script(
            "git",
            "printf '%s' 'git@gitlab.acme.example:team/repo.git'",
            "",
        );
        let database = new_database().await;
        let service = PullRequestsService::with_runner(
            HostCommandRunner::new(s.path("state")).with_commands(
                "missing-gh",
                "missing-glab",
                &git,
            ),
        )
        .with_database(database.clone());
        service.runner.provider_hosts().record(
            "gitlab.acme.example",
            crate::source_control::ProviderKind::Gitlab,
        );
        database
            .call(|connection| {
                Ok(snapshot_store::SnapshotStore::new(connection).put(
                    snapshot_store::Snapshot {
                        host: "gitlab.acme.example".into(),
                        project: "team/repo".into(),
                        kind: "detail".into(),
                        key: "3941".into(),
                        fingerprint: "fp".into(),
                        payload: b"x".to_vec(),
                        observed_at_ms: 1,
                        generation: 1,
                    },
                )?)
            })
            .await
            .unwrap();
        let _ = service
            .context(s.root(), ContextRead::Rescan, &CancellationToken::new())
            .await;
        let row = database
            .call(|connection| {
                Ok(snapshot_store::SnapshotStore::new(connection).get(
                    "gitlab.acme.example",
                    "team/repo",
                    "detail",
                    "3941",
                )?)
            })
            .await
            .unwrap();
        assert!(row.is_some(), "rescan must not delete snapshots");
    }
}

#[cfg(test)]
mod tripwires {
    fn violations(source: &str) -> Vec<&'static str> {
        let mut result = Vec::new();
        if [
            concat!("std::process", "::Command"),
            concat!("tokio::process", "::Command"),
        ]
        .iter()
        .any(|needle| source.contains(needle))
        {
            result.push("direct-process");
        }
        if [
            concat!("tokio::time", "::interval"),
            concat!("spawn_blocking(", "loop"),
            concat!("set", "Interval"),
        ]
        .iter()
        .any(|needle| source.contains(needle))
        {
            result.push("poller");
        }
        for (needle, violation) in [
            (concat!("reqwest", "::"), "direct-http"),
            (concat!("tokio", "::spawn("), "async-task"),
            (concat!("std::thread", "::spawn"), "thread-task"),
        ] {
            if source.contains(needle) {
                result.push(violation);
            }
        }
        for call in source.split(concat!("tracing", "::")).skip(1) {
            let call = call.split_once(");").map_or(call, |(call, _)| call);
            if [
                "host",
                "repository",
                "branch",
                "title",
                "body",
                "login",
                "url",
                "stderr",
            ]
            .iter()
            .any(|field| call.contains(field))
            {
                result.push("log-content");
            }
        }
        result
    }

    const SOURCES: &[(&str, &str)] = &[
        ("mod.rs", include_str!("mod.rs")),
        ("model.rs", include_str!("model.rs")),
        ("host.rs", include_str!("host.rs")),
        ("context.rs", include_str!("context.rs")),
        ("error.rs", include_str!("error.rs")),
        ("permissions.rs", include_str!("permissions.rs")),
        ("read.rs", include_str!("read.rs")),
        ("action.rs", include_str!("action.rs")),
        ("cache.rs", include_str!("cache.rs")),
        ("github/actions.rs", include_str!("github/actions.rs")),
        (
            "github/detail_tests.rs",
            include_str!("github/detail_tests.rs"),
        ),
        ("github/timeline.rs", include_str!("github/timeline.rs")),
        ("github/files.rs", include_str!("github/files.rs")),
        ("checkout.rs", include_str!("checkout.rs")),
        ("github/mod.rs", include_str!("github/mod.rs")),
        ("github/graphql.rs", include_str!("github/graphql.rs")),
        ("github/parse.rs", include_str!("github/parse.rs")),
        ("gitlab/mod.rs", include_str!("gitlab/mod.rs")),
        ("gitlab/actions.rs", include_str!("gitlab/actions.rs")),
        ("gitlab/poll.rs", include_str!("gitlab/poll.rs")),
        (
            "gitlab/detail_tests.rs",
            include_str!("gitlab/detail_tests.rs"),
        ),
        (
            "gitlab/review_positions.rs",
            include_str!("gitlab/review_positions.rs"),
        ),
        ("gitlab/parse.rs", include_str!("gitlab/parse.rs")),
        ("gitlab/graphql.rs", include_str!("gitlab/graphql.rs")),
        ("gitlab/timeline.rs", include_str!("gitlab/timeline.rs")),
        ("gitlab/files.rs", include_str!("gitlab/files.rs")),
        ("gitlab/refresh.rs", include_str!("gitlab/refresh.rs")),
        ("snapshot_store.rs", include_str!("snapshot_store.rs")),
        (
            "production/pull_requests_rpc.rs",
            include_str!("../production/pull_requests_rpc.rs"),
        ),
    ];

    #[test]
    fn pull_requests_tripwire_inventory_covers_every_module_file() {
        use std::{collections::BTreeSet, path::Path};

        fn visit(root: &Path, directory: &Path, files: &mut BTreeSet<String>) {
            for entry in std::fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    visit(root, &path, files);
                } else if path.extension().is_some_and(|extension| extension == "rs") {
                    files.insert(
                        path.strip_prefix(root)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/"),
                    );
                }
            }
        }

        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/pull_requests");
        let mut files = BTreeSet::new();
        visit(&root, &root, &mut files);
        let scanned: BTreeSet<_> = SOURCES
            .iter()
            .filter(|(name, _)| !name.starts_with("production/"))
            .map(|(name, _)| (*name).to_owned())
            .collect();
        assert_eq!(files, scanned, "new modules must join the source tripwires");
    }

    #[test]
    fn pull_requests_default_runner_only_configures_gh_glab_and_git() {
        use super::host::{HostCommandRunner, command_specs_for_test};
        let runner = HostCommandRunner::new(std::path::PathBuf::from("unused-state"));
        let specs = command_specs_for_test(&runner);
        for (spec, expected) in specs.into_iter().zip(["gh", "glab", "git"]) {
            assert_eq!(spec.executable, std::path::Path::new(expected));
            assert!(
                spec.args(std::iter::empty()).is_empty(),
                "no wrapper commands"
            );
        }
    }

    #[test]
    fn pull_requests_log_hygiene_has_no_host_or_user_content() {
        for (name, source) in SOURCES {
            assert!(!violations(source).contains(&"log-content"), "{name}");
        }
    }

    #[test]
    fn pull_requests_process_surface_uses_only_the_existing_runner() {
        for (name, source) in SOURCES {
            assert!(!violations(source).contains(&"direct-process"), "{name}");
        }
    }

    #[test]
    fn pull_requests_module_has_no_background_pollers() {
        for (name, source) in SOURCES {
            assert!(!violations(source).contains(&"poller"), "{name}");
        }
    }

    #[test]
    fn pull_requests_tripwires_reject_http_and_background_tasks() {
        for (source, violation) in [
            (concat!("reqwest", "::Client::new()"), "direct-http"),
            (concat!("tokio", "::spawn(async {})"), "async-task"),
            (concat!("std::thread", "::spawn(|| {})"), "thread-task"),
        ] {
            assert!(violations(source).contains(&violation), "{violation}");
        }
    }

    #[test]
    fn pull_requests_module_has_no_http_or_detached_tasks() {
        for (name, source) in SOURCES {
            let found = violations(source);
            assert!(!found.contains(&"direct-http"), "{name}");
            assert!(!found.contains(&"thread-task"), "{name}");
            if *name != "production/pull_requests_rpc.rs" {
                assert!(!found.contains(&"async-task"), "{name}");
            }
        }
    }

    #[test]
    fn pull_requests_tripwires_allow_bounded_request_and_checkout_deadlines() {
        // These are one-shot safety deadlines, never background polling. Checkout's
        // write token is independent after handoff so a dropped caller cannot kill Git.
        for source in [
            "tokio::time::sleep(duration)",
            "tokio::time::timeout(deadline, request)",
            "Budget::CheckoutWrite => crate::git::CHECKOUT_WRITE_TIMEOUT",
            "Duration::from_secs(24 * 60 * 60)",
            "CancellationToken::new()",
        ] {
            assert!(violations(source).is_empty(), "{source}");
        }
    }

    #[test]
    fn pull_requests_tripwires_reject_sensitive_logs_direct_commands_and_pollers() {
        for prefix in [
            concat!("std::process", "::Command"),
            concat!("tokio::process", "::Command"),
        ] {
            assert!(violations(&format!("{prefix}::new(binary)")).contains(&"direct-process"));
        }
        for field in [
            "host",
            "repository",
            "branch",
            "title",
            "body",
            "login",
            "url",
            "stderr",
        ] {
            let source = format!(
                "{}info!(\n {field} = %{field},\n \"event\"\n);",
                concat!("tracing", "::")
            );
            assert!(violations(&source).contains(&"log-content"));
        }
        for poller in [
            concat!("tokio::time", "::interval"),
            concat!("spawn_blocking(", "loop"),
            concat!("set", "Interval"),
        ] {
            assert!(violations(poller).contains(&"poller"));
        }
        assert!(violations(concat!("tracing", "::info!(code, length);")).is_empty());
    }
}
