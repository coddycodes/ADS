use std::{
    collections::{HashMap, HashSet},
    io,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use serde::Serialize;
use tokio::sync::{Mutex as AsyncMutex, Notify, OwnedMutexGuard};
use tokio_util::sync::CancellationToken;

use crate::{
    git::{canonical_worktree_path_key, host_path_platform, normalize_worktree_path_key},
    persistence::{CommitFence, CommitPermit},
};

use super::{AdoptedWorktreeAvailability, WorktreeCatalogSnapshot, bounded_message};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkspaceGuardState {
    MissingRegistered,
    MissingUnregistered,
    Removing,
}

impl WorkspaceGuardState {
    fn availability(self) -> AdoptedWorktreeAvailability {
        match self {
            Self::MissingRegistered => AdoptedWorktreeAvailability::MissingRegistered,
            Self::MissingUnregistered => AdoptedWorktreeAvailability::MissingUnregistered,
            Self::Removing => AdoptedWorktreeAvailability::Removing,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceUnavailable {
    #[serde(rename = "_tag")]
    pub tag: &'static str,
    pub reason: &'static str,
    pub message: String,
    pub thread_id: String,
    pub path: String,
    pub availability: AdoptedWorktreeAvailability,
    #[serde(skip)]
    pub state: WorkspaceGuardState,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceIdentityError {
    #[serde(rename = "_tag")]
    pub tag: &'static str,
    pub reason: &'static str,
    pub message: String,
    pub path: String,
}

impl WorkspaceIdentityError {
    fn new(path: &Path, error: &io::Error) -> Self {
        Self {
            tag: "WorkspaceIdentityError",
            reason: "workspace-identity-unavailable",
            message: bounded_message(format!(
                "The physical workspace identity for '{}' could not be verified: {error}",
                path.display()
            )),
            path: path.to_string_lossy().into_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum WorkspaceAdmissionError {
    Unavailable(WorkspaceUnavailable),
    Identity(WorkspaceIdentityError),
}

impl WorkspaceAdmissionError {
    #[must_use]
    pub fn identity(&self) -> Option<&WorkspaceIdentityError> {
        match self {
            Self::Identity(error) => Some(error),
            Self::Unavailable(_) => None,
        }
    }

    #[must_use]
    pub fn unavailable(&self) -> Option<&WorkspaceUnavailable> {
        match self {
            Self::Unavailable(error) => Some(error),
            Self::Identity(_) => None,
        }
    }
}

impl From<WorkspaceUnavailable> for WorkspaceAdmissionError {
    fn from(error: WorkspaceUnavailable) -> Self {
        Self::Unavailable(error)
    }
}

impl From<WorkspaceIdentityError> for WorkspaceAdmissionError {
    fn from(error: WorkspaceIdentityError) -> Self {
        Self::Identity(error)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceLossTransition {
    pub thread_id: String,
    pub repository_key: String,
    pub generation: u64,
    pub path: PathBuf,
    pub availability: AdoptedWorktreeAvailability,
}

impl WorkspaceLossTransition {
    fn guard_state(&self) -> Option<WorkspaceGuardState> {
        match self.availability {
            AdoptedWorktreeAvailability::MissingRegistered => {
                Some(WorkspaceGuardState::MissingRegistered)
            }
            AdoptedWorktreeAvailability::MissingUnregistered => {
                Some(WorkspaceGuardState::MissingUnregistered)
            }
            _ => None,
        }
    }
}

#[derive(Clone, Default)]
pub struct WorkspaceAvailabilityRegistry {
    inner: Arc<Mutex<RegistryState>>,
    transition_lock: Arc<AsyncMutex<()>>,
    admission_changed: Arc<Notify>,
    #[cfg(test)]
    finalization_rejection_pause: Arc<Mutex<Option<FinalizationRejectionPause>>>,
    #[cfg(test)]
    terminal_signal_before_permit_pause: Arc<Mutex<Option<TerminalSignalPermitPause>>>,
    #[cfg(test)]
    terminal_signal_after_permit_pause: Arc<Mutex<Option<TerminalSignalPermitPause>>>,
    #[cfg(test)]
    identity_resolution_error: Arc<Mutex<Option<io::ErrorKind>>>,
}

#[derive(Default)]
struct RegistryState {
    entries: HashMap<String, GuardEntry>,
    latest_loss: HashMap<String, LossWatermark>,
    orphan_cleanups: HashMap<u64, CleanupRecord>,
    next_cleanup_id: u64,
    active_admissions: HashMap<u64, ActiveAdmission>,
    next_admission_id: u64,
    next_removal_id: u64,
    reconciliation_active: bool,
    /// Ages added to guards by `backdate_for_test`, so a test needs no clock arithmetic that a
    /// freshly booted host could not do.
    #[cfg(test)]
    age_offsets: HashMap<String, Duration>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum AdmissionScope {
    Thread(String),
    Path(String),
}

#[derive(Clone)]
struct GuardEntry {
    thread_id: String,
    repository_key: String,
    path_key: String,
    path: String,
    state: WorkspaceGuardState,
    /// When the guard entered `state`.
    entered_at: Instant,
    terminal_signal: WorkspaceTerminalSignalGate,
    pending_missing: Option<PendingMissing>,
    removal_ids: HashSet<u64>,
}

#[derive(Clone)]
struct PendingMissing {
    repository_key: String,
    path_key: String,
    path: String,
    state: WorkspaceGuardState,
}

#[derive(Clone, Copy)]
struct LossWatermark {
    generation: u64,
    missing_registered: bool,
    missing_unregistered: bool,
}

impl LossWatermark {
    fn new(generation: u64) -> Self {
        Self {
            generation,
            missing_registered: false,
            missing_unregistered: false,
        }
    }

    fn admit(&mut self, generation: u64, state: WorkspaceGuardState) -> bool {
        if generation < self.generation {
            return false;
        }
        if generation > self.generation {
            *self = Self::new(generation);
        }
        let admitted = match state {
            WorkspaceGuardState::MissingRegistered => &mut self.missing_registered,
            WorkspaceGuardState::MissingUnregistered => &mut self.missing_unregistered,
            WorkspaceGuardState::Removing => return false,
        };
        if *admitted {
            return false;
        }
        *admitted = true;
        true
    }
}

pub struct RemovalGuard {
    registry: WorkspaceAvailabilityRegistry,
    thread_id: String,
    removal_id: u64,
    released: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceRemovalIdentity {
    thread_id: String,
    path_key: String,
    removal_id: u64,
}

impl WorkspaceRemovalIdentity {
    #[must_use]
    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    #[must_use]
    pub fn path_key(&self) -> &str {
        &self.path_key
    }
}

pub struct WorkspaceAdmissionLease {
    registry: WorkspaceAvailabilityRegistry,
    admission_id: u64,
    loss_cancellation: WorkspaceAdmissionCancellation,
    finalization: WorkspaceFinalizationGate,
}

#[derive(Clone)]
pub struct WorkspaceAdmissionCancellation {
    token: CancellationToken,
    unavailable: Arc<Mutex<Option<WorkspaceUnavailable>>>,
}

struct ActiveAdmission {
    scopes: Vec<AdmissionScope>,
    loss_cancellation: WorkspaceAdmissionCancellation,
    finalization: WorkspaceFinalizationGate,
}

#[derive(Clone)]
struct WorkspaceFinalizationGate {
    inner: Arc<(Mutex<WorkspaceFinalizationState>, Condvar)>,
    loss_cancellation: WorkspaceAdmissionCancellation,
}

#[derive(Default)]
enum WorkspaceFinalizationState {
    #[default]
    Open,
    Finalizing,
    Finalized,
    Rejected,
}

struct WorkspaceFinalizationPermit {
    gate: WorkspaceFinalizationGate,
}

#[derive(Clone, Default)]
struct WorkspaceTerminalSignalGate {
    inner: Arc<Mutex<WorkspaceTerminalSignalState>>,
    drained: Arc<Notify>,
    #[cfg(test)]
    invalidation_started: Arc<Notify>,
}

#[derive(Default)]
struct WorkspaceTerminalSignalState {
    active_permits: usize,
    invalidated: bool,
    reopen_when_drained: bool,
}

pub(crate) struct WorkspaceTerminalSignalPermit {
    gate: WorkspaceTerminalSignalGate,
}

#[derive(Clone)]
struct WorkspaceTerminalSignalDrain {
    gate: WorkspaceTerminalSignalGate,
}

enum WorkspaceAvailabilityAction {
    MarkUnavailable {
        transition: WorkspaceLossTransition,
        skip_if_guarded: bool,
    },
    ClearRecovered {
        thread_id: String,
        path_key: String,
        repository_key: Option<String>,
    },
    MarkRemoving {
        thread_id: String,
        path_key: String,
        removal_id: u64,
    },
}

struct ClosedTerminalSignalGate {
    thread_id: String,
    gate: WorkspaceTerminalSignalGate,
}

pub(crate) struct WorkspaceAvailabilityReconciliation {
    registry: WorkspaceAvailabilityRegistry,
    _transition_guard: Option<OwnedMutexGuard<()>>,
    actions: Vec<WorkspaceAvailabilityAction>,
    closed_gates: Vec<ClosedTerminalSignalGate>,
    drains: Vec<WorkspaceTerminalSignalDrain>,
    active: bool,
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct FinalizationRejectionPause {
    entered: Arc<Notify>,
    release: Arc<(Mutex<bool>, Condvar)>,
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct TerminalSignalPermitPause {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CleanupTransitionKey {
    thread_id: String,
    repository_key: String,
    generation: u64,
    path_key: String,
    state: WorkspaceGuardState,
}

struct CleanupRecord {
    transition: CleanupTransitionKey,
    cancellation: CancellationToken,
}

#[derive(Clone)]
pub struct WorkspaceCleanupOwnership {
    id: u64,
    transition: CleanupTransitionKey,
    cancellation: CancellationToken,
}

impl WorkspaceAvailabilityRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    fn with_identity_resolution_error_for_test(kind: io::ErrorKind) -> Self {
        let registry = Self::new();
        *lock(&registry.identity_resolution_error) = Some(kind);
        registry
    }

    #[cfg(test)]
    pub(crate) fn fail_identity_resolution_for_test(&self, kind: io::ErrorKind) {
        *lock(&self.identity_resolution_error) = Some(kind);
    }

    async fn physical_path_key(&self, path: &Path) -> Result<String, WorkspaceIdentityError> {
        #[cfg(test)]
        if let Some(kind) = *lock(&self.identity_resolution_error) {
            return Err(WorkspaceIdentityError::new(
                path,
                &io::Error::new(kind, "injected physical identity failure"),
            ));
        }
        canonical_worktree_path_key(path)
            .await
            .map_err(|error| WorkspaceIdentityError::new(path, &error))
    }

    /// Every guarded workspace and how long it has held its state. Changes nothing.
    pub(crate) fn guarded(&self) -> Vec<(WorkspaceUnavailable, Duration)> {
        let state = lock(&self.inner);
        state
            .entries
            .values()
            .map(|entry| {
                let held = entry.entered_at.elapsed();
                #[cfg(test)]
                let held = held
                    + state
                        .age_offsets
                        .get(&entry.thread_id)
                        .copied()
                        .unwrap_or_default();
                (unavailable(entry), held)
            })
            .collect()
    }

    /// Ages a guard by `by`, so a test can make it look long held.
    #[cfg(test)]
    pub(crate) fn backdate_for_test(&self, thread_id: &str, by: Duration) {
        lock(&self.inner)
            .age_offsets
            .insert(thread_id.to_owned(), by);
    }

    pub async fn guard_thread(&self, thread_id: &str) -> Result<(), WorkspaceUnavailable> {
        let state = lock(&self.inner);
        state
            .entries
            .get(thread_id)
            .map_or(Ok(()), |entry| Err(unavailable(entry)))
    }

    pub async fn guard_path(&self, path: &Path) -> Result<(), WorkspaceAdmissionError> {
        let physical = self.physical_path_key(path).await?;
        let state = lock(&self.inner);
        state
            .entries
            .values()
            .find(|entry| path_is_within(&physical, &entry.path_key))
            .map_or(Ok(()), |entry| Err(unavailable(entry).into()))
    }

    pub async fn acquire_admission<'a>(
        &self,
        thread_id: &str,
        paths: impl IntoIterator<Item = &'a Path>,
    ) -> Result<WorkspaceAdmissionLease, WorkspaceAdmissionError> {
        self.acquire_admission_inner(Some(thread_id), paths).await
    }

    pub async fn acquire_path_admission<'a>(
        &self,
        paths: impl IntoIterator<Item = &'a Path>,
    ) -> Result<WorkspaceAdmissionLease, WorkspaceAdmissionError> {
        self.acquire_admission_inner(None, paths).await
    }

    async fn acquire_admission_inner<'a>(
        &self,
        thread_id: Option<&str>,
        paths: impl IntoIterator<Item = &'a Path>,
    ) -> Result<WorkspaceAdmissionLease, WorkspaceAdmissionError> {
        let mut path_keys = HashSet::new();
        for path in paths {
            path_keys.insert(self.physical_path_key(path).await?);
        }
        let mut state = lock(&self.inner);
        if let Some(thread_id) = thread_id
            && let Some(entry) = state.entries.get(thread_id)
        {
            return Err(unavailable(entry).into());
        }
        if let Some(entry) = state.entries.values().find(|entry| {
            path_keys
                .iter()
                .any(|path| path_is_within(path, &entry.path_key))
        }) {
            return Err(unavailable(entry).into());
        }

        let mut scopes = Vec::with_capacity(path_keys.len() + usize::from(thread_id.is_some()));
        scopes.extend(thread_id.map(|thread_id| AdmissionScope::Thread(thread_id.to_owned())));
        scopes.extend(path_keys.into_iter().map(AdmissionScope::Path));
        state.next_admission_id = state.next_admission_id.wrapping_add(1).max(1);
        let admission_id = state.next_admission_id;
        let loss_cancellation = WorkspaceAdmissionCancellation {
            token: CancellationToken::new(),
            unavailable: Arc::new(Mutex::new(None)),
        };
        let finalization = WorkspaceFinalizationGate::new(loss_cancellation.clone());
        state.active_admissions.insert(
            admission_id,
            ActiveAdmission {
                scopes: scopes.clone(),
                loss_cancellation: loss_cancellation.clone(),
                finalization: finalization.clone(),
            },
        );
        drop(state);
        Ok(WorkspaceAdmissionLease {
            registry: self.clone(),
            admission_id,
            loss_cancellation,
            finalization,
        })
    }

    pub async fn wait_for_transition_admissions(&self, transition: &WorkspaceLossTransition) {
        loop {
            let notified = self.admission_changed.notified();
            if !self.has_transition_admissions(transition) {
                return;
            }
            notified.await;
        }
    }

    #[must_use]
    pub fn has_transition_admissions(&self, transition: &WorkspaceLossTransition) -> bool {
        let thread = AdmissionScope::Thread(transition.thread_id.clone());
        let path = normalized_path(&transition.path);
        let state = lock(&self.inner);
        state.active_admissions.values().any(|admission| {
            admission.scopes.iter().any(|scope| {
                scope == &thread
                    || matches!(scope, AdmissionScope::Path(candidate) if path_is_within(candidate, &path))
            })
        })
    }

    pub async fn mark_unavailable(
        &self,
        mut transition: WorkspaceLossTransition,
    ) -> Result<bool, WorkspaceIdentityError> {
        transition.path = PathBuf::from(self.physical_path_key(&transition.path).await?);
        let expected = transition.clone();
        let reconciliation = self
            .prepare_reconciliation(vec![WorkspaceAvailabilityAction::MarkUnavailable {
                transition,
                skip_if_guarded: false,
            }])
            .await;
        reconciliation.wait_for_drains().await;
        Ok(reconciliation
            .finalize()
            .iter()
            .any(|transition| transition == &expected))
    }

    pub async fn mark_removing(
        &self,
        thread_id: &str,
        path: &Path,
    ) -> Result<RemovalGuard, WorkspaceIdentityError> {
        let path_key = self.physical_path_key(path).await?;
        let removal_id = {
            let mut state = lock(&self.inner);
            state.next_removal_id = state.next_removal_id.wrapping_add(1).max(1);
            state.next_removal_id
        };
        let reconciliation = self
            .prepare_reconciliation(vec![WorkspaceAvailabilityAction::MarkRemoving {
                thread_id: thread_id.to_owned(),
                path_key,
                removal_id,
            }])
            .await;
        reconciliation.wait_for_drains().await;
        reconciliation.finalize();
        Ok(RemovalGuard {
            registry: self.clone(),
            thread_id: thread_id.to_owned(),
            removal_id,
            released: false,
        })
    }

    pub async fn clear_recovered(
        &self,
        thread_id: &str,
        path: &Path,
    ) -> Result<(), WorkspaceIdentityError> {
        self.clear_matching(thread_id, path, None).await
    }

    pub async fn clear_recovered_in_repository(
        &self,
        thread_id: &str,
        path: &Path,
        repository_key: &str,
    ) -> Result<(), WorkspaceIdentityError> {
        self.clear_matching(thread_id, path, Some(repository_key))
            .await
    }

    async fn clear_matching(
        &self,
        thread_id: &str,
        path: &Path,
        repository_key: Option<&str>,
    ) -> Result<(), WorkspaceIdentityError> {
        let path_key = self.physical_path_key(path).await?;
        let reconciliation = self
            .prepare_reconciliation(vec![WorkspaceAvailabilityAction::ClearRecovered {
                thread_id: thread_id.to_owned(),
                path_key,
                repository_key: repository_key.map(str::to_owned),
            }])
            .await;
        reconciliation.wait_for_drains().await;
        reconciliation.finalize();
        Ok(())
    }

    pub async fn reconcile_snapshot(
        &self,
        snapshot: &WorktreeCatalogSnapshot,
    ) -> Result<Vec<WorkspaceLossTransition>, WorkspaceIdentityError> {
        let reconciliation = self.prepare_snapshot_reconciliation(snapshot).await?;
        reconciliation.wait_for_drains().await;
        Ok(reconciliation.finalize())
    }

    pub(crate) async fn prepare_snapshot_reconciliation(
        &self,
        snapshot: &WorktreeCatalogSnapshot,
    ) -> Result<WorkspaceAvailabilityReconciliation, WorkspaceIdentityError> {
        if !snapshot.authoritative {
            return Ok(self.prepare_reconciliation(Vec::new()).await);
        }
        let mut actions = Vec::new();
        for workspace in &snapshot.adopted_workspaces {
            match workspace.availability {
                AdoptedWorktreeAvailability::MissingRegistered
                | AdoptedWorktreeAvailability::MissingUnregistered => {
                    let path = self.physical_path_key(Path::new(&workspace.path)).await?;
                    let transition = WorkspaceLossTransition {
                        thread_id: workspace.thread_id.clone(),
                        repository_key: snapshot.repository_key.clone(),
                        generation: snapshot.generation,
                        path: PathBuf::from(path),
                        availability: workspace.availability,
                    };
                    actions.push(WorkspaceAvailabilityAction::MarkUnavailable {
                        transition,
                        skip_if_guarded: true,
                    });
                }
                AdoptedWorktreeAvailability::Present => {
                    let path_key = self.physical_path_key(Path::new(&workspace.path)).await?;
                    actions.push(WorkspaceAvailabilityAction::ClearRecovered {
                        thread_id: workspace.thread_id.clone(),
                        path_key,
                        repository_key: Some(snapshot.repository_key.clone()),
                    });
                }
                AdoptedWorktreeAvailability::VerificationUnavailable
                | AdoptedWorktreeAvailability::Removing => {}
            }
        }
        Ok(self.prepare_reconciliation(actions).await)
    }

    async fn prepare_reconciliation(
        &self,
        requested: Vec<WorkspaceAvailabilityAction>,
    ) -> WorkspaceAvailabilityReconciliation {
        let transition_guard = self.transition_lock.clone().lock_owned().await;
        let mut state = lock(&self.inner);
        debug_assert!(!state.reconciliation_active);
        state.reconciliation_active = true;
        let mut actions = Vec::with_capacity(requested.len());
        let mut closed_gates = Vec::new();
        let mut drains = Vec::new();

        for action in requested {
            match &action {
                WorkspaceAvailabilityAction::MarkUnavailable {
                    transition,
                    skip_if_guarded,
                } => {
                    let Some(guard_state) = transition.guard_state() else {
                        continue;
                    };
                    let path_key = normalized_path(&transition.path);
                    if *skip_if_guarded
                        && state
                            .entries
                            .get(&transition.thread_id)
                            .is_some_and(|entry| {
                                entry.repository_key == transition.repository_key
                                    && entry.path_key == path_key
                                    && (entry.state == guard_state
                                        || entry.state == WorkspaceGuardState::Removing)
                            })
                    {
                        continue;
                    }
                    let mut watermark = state
                        .latest_loss
                        .get(&transition.thread_id)
                        .copied()
                        .unwrap_or_else(|| LossWatermark::new(transition.generation));
                    if !watermark.admit(transition.generation, guard_state) {
                        continue;
                    }
                    if let Some(entry) = state.entries.get(&transition.thread_id)
                        && entry.state != WorkspaceGuardState::Removing
                    {
                        close_terminal_signal_gate(
                            &transition.thread_id,
                            &entry.terminal_signal,
                            &mut closed_gates,
                            &mut drains,
                        );
                    }
                    actions.push(action);
                }
                WorkspaceAvailabilityAction::ClearRecovered {
                    thread_id,
                    path_key,
                    repository_key,
                } => {
                    let Some(entry) = state.entries.get(thread_id) else {
                        continue;
                    };
                    if entry.path_key != *path_key
                        || repository_key
                            .as_deref()
                            .is_some_and(|repository_key| entry.repository_key != repository_key)
                    {
                        continue;
                    }
                    if entry.state != WorkspaceGuardState::Removing {
                        close_terminal_signal_gate(
                            thread_id,
                            &entry.terminal_signal,
                            &mut closed_gates,
                            &mut drains,
                        );
                    }
                    actions.push(action);
                }
                WorkspaceAvailabilityAction::MarkRemoving { thread_id, .. } => {
                    if let Some(entry) = state.entries.get(thread_id)
                        && entry.state != WorkspaceGuardState::Removing
                    {
                        close_terminal_signal_gate(
                            thread_id,
                            &entry.terminal_signal,
                            &mut closed_gates,
                            &mut drains,
                        );
                    }
                    actions.push(action);
                }
            }
        }
        drop(state);
        WorkspaceAvailabilityReconciliation {
            registry: self.clone(),
            _transition_guard: Some(transition_guard),
            actions,
            closed_gates,
            drains,
            active: true,
        }
    }

    pub fn begin_orphan_cleanup(
        &self,
        transition: &WorkspaceLossTransition,
    ) -> Option<WorkspaceCleanupOwnership> {
        let state_key = transition.guard_state()?;
        let transition_key = CleanupTransitionKey {
            thread_id: transition.thread_id.clone(),
            repository_key: transition.repository_key.clone(),
            generation: transition.generation,
            path_key: normalized_path(&transition.path),
            state: state_key,
        };
        let mut state = lock(&self.inner);
        let current = state.entries.get(&transition.thread_id)?;
        let watermark = state.latest_loss.get(&transition.thread_id)?;
        if current.repository_key != transition.repository_key
            || current.path_key != transition_key.path_key
            || current.state != state_key
            || watermark.generation != transition.generation
        {
            return None;
        }
        state.next_cleanup_id = state.next_cleanup_id.wrapping_add(1).max(1);
        let id = state.next_cleanup_id;
        let cancellation = CancellationToken::new();
        state.orphan_cleanups.insert(
            id,
            CleanupRecord {
                transition: transition_key.clone(),
                cancellation: cancellation.clone(),
            },
        );
        Some(WorkspaceCleanupOwnership {
            id,
            transition: transition_key,
            cancellation,
        })
    }

    #[must_use]
    pub fn transition_is_current(&self, transition: &WorkspaceLossTransition) -> bool {
        let Some(guard_state) = transition.guard_state() else {
            return false;
        };
        let path_key = normalized_path(&transition.path);
        let state = lock(&self.inner);
        state
            .entries
            .get(&transition.thread_id)
            .is_some_and(|entry| {
                entry.repository_key == transition.repository_key
                    && entry.path_key == path_key
                    && entry.state == guard_state
            })
            && state
                .latest_loss
                .get(&transition.thread_id)
                .is_some_and(|watermark| watermark.generation == transition.generation)
    }

    #[must_use]
    pub fn removal_is_current(&self, identity: &WorkspaceRemovalIdentity) -> bool {
        let state = lock(&self.inner);
        state.entries.get(&identity.thread_id).is_some_and(|entry| {
            entry.state == WorkspaceGuardState::Removing
                && entry.path_key == identity.path_key
                && entry.removal_ids.contains(&identity.removal_id)
        })
    }

    pub(crate) fn retain_removal(
        &self,
        identity: &WorkspaceRemovalIdentity,
    ) -> Option<RemovalGuard> {
        let mut state = lock(&self.inner);
        let current = state.entries.get(&identity.thread_id)?;
        if current.state != WorkspaceGuardState::Removing
            || current.path_key != identity.path_key
            || !current.removal_ids.contains(&identity.removal_id)
        {
            return None;
        }
        state.next_removal_id = state.next_removal_id.wrapping_add(1).max(1);
        let removal_id = state.next_removal_id;
        state
            .entries
            .get_mut(&identity.thread_id)?
            .removal_ids
            .insert(removal_id);
        Some(RemovalGuard {
            registry: self.clone(),
            thread_id: identity.thread_id.clone(),
            removal_id,
            released: false,
        })
    }

    #[must_use]
    pub fn has_removal_admissions(&self, identity: &WorkspaceRemovalIdentity) -> bool {
        if !self.removal_is_current(identity) {
            return false;
        }
        let thread = AdmissionScope::Thread(identity.thread_id.clone());
        let state = lock(&self.inner);
        state.active_admissions.values().any(|admission| {
            admission.scopes.iter().any(|scope| {
                scope == &thread
                    || matches!(scope, AdmissionScope::Path(candidate) if path_is_within(candidate, &identity.path_key))
            })
        })
    }

    pub async fn wait_for_removal_admissions(&self, identity: &WorkspaceRemovalIdentity) {
        loop {
            let notified = self.admission_changed.notified();
            if !self.has_removal_admissions(identity) {
                return;
            }
            notified.await;
        }
    }

    pub(crate) async fn begin_removal_terminal_signal(
        &self,
        identity: &WorkspaceRemovalIdentity,
    ) -> Option<WorkspaceTerminalSignalPermit> {
        let terminal_signal = {
            let state = lock(&self.inner);
            let entry = state.entries.get(&identity.thread_id)?;
            if entry.state != WorkspaceGuardState::Removing
                || entry.path_key != identity.path_key
                || !entry.removal_ids.contains(&identity.removal_id)
            {
                return None;
            }
            entry.terminal_signal.clone()
        };
        terminal_signal.begin()
    }

    pub(crate) async fn begin_terminal_signal(
        &self,
        transition: &WorkspaceLossTransition,
    ) -> Option<WorkspaceTerminalSignalPermit> {
        #[cfg(test)]
        self.maybe_pause_before_terminal_signal_permit().await;
        let guard_state = transition.guard_state()?;
        let path_key = normalized_path(&transition.path);
        let terminal_signal = {
            let state = lock(&self.inner);
            let entry = state.entries.get(&transition.thread_id)?;
            if entry.repository_key != transition.repository_key
                || entry.path_key != path_key
                || entry.state != guard_state
                || !state
                    .latest_loss
                    .get(&transition.thread_id)
                    .is_some_and(|watermark| watermark.generation == transition.generation)
            {
                return None;
            }
            entry.terminal_signal.clone()
        };
        let permit = terminal_signal.begin()?;
        #[cfg(test)]
        self.maybe_pause_after_terminal_signal_permit().await;
        Some(permit)
    }

    #[cfg(test)]
    pub(crate) fn pause_after_next_finalization_rejection(&self) -> FinalizationRejectionPause {
        let pause = FinalizationRejectionPause {
            entered: Arc::new(Notify::new()),
            release: Arc::new((Mutex::new(false), Condvar::new())),
        };
        *lock(&self.finalization_rejection_pause) = Some(pause.clone());
        pause
    }

    #[cfg(test)]
    fn maybe_pause_after_finalization_rejection(&self) {
        let pause = lock(&self.finalization_rejection_pause).take();
        if let Some(pause) = pause {
            pause.block_until_released();
        }
    }

    #[cfg(test)]
    pub(crate) fn pause_before_next_terminal_signal_permit(&self) -> TerminalSignalPermitPause {
        let pause = TerminalSignalPermitPause::new();
        *lock(&self.terminal_signal_before_permit_pause) = Some(pause.clone());
        pause
    }

    #[cfg(test)]
    pub(crate) fn pause_after_next_terminal_signal_permit(&self) -> TerminalSignalPermitPause {
        let pause = TerminalSignalPermitPause::new();
        *lock(&self.terminal_signal_after_permit_pause) = Some(pause.clone());
        pause
    }

    #[cfg(test)]
    pub(crate) async fn terminal_signal_invalidation_notification(
        &self,
        transition: &WorkspaceLossTransition,
    ) -> Option<Arc<Notify>> {
        let guard_state = transition.guard_state()?;
        let path_key = self.physical_path_key(&transition.path).await.ok()?;
        let state = lock(&self.inner);
        let entry = state.entries.get(&transition.thread_id)?;
        (entry.repository_key == transition.repository_key
            && entry.path_key == path_key
            && entry.state == guard_state
            && state
                .latest_loss
                .get(&transition.thread_id)
                .is_some_and(|watermark| watermark.generation == transition.generation))
        .then(|| entry.terminal_signal.invalidation_started.clone())
    }

    #[cfg(test)]
    async fn maybe_pause_before_terminal_signal_permit(&self) {
        let pause = lock(&self.terminal_signal_before_permit_pause).take();
        if let Some(pause) = pause {
            pause.pause().await;
        }
    }

    #[cfg(test)]
    async fn maybe_pause_after_terminal_signal_permit(&self) {
        let pause = lock(&self.terminal_signal_after_permit_pause).take();
        if let Some(pause) = pause {
            pause.pause().await;
        }
    }

    pub fn complete_orphan_cleanup(&self, ownership: &WorkspaceCleanupOwnership) {
        let mut state = lock(&self.inner);
        if state
            .orphan_cleanups
            .get(&ownership.id)
            .is_some_and(|record| record.transition == ownership.transition)
        {
            state.orphan_cleanups.remove(&ownership.id);
        }
    }

    #[must_use]
    pub fn cleanup_is_current(&self, ownership: &WorkspaceCleanupOwnership) -> bool {
        let state = lock(&self.inner);
        !ownership.cancellation.is_cancelled()
            && state
                .orphan_cleanups
                .get(&ownership.id)
                .is_some_and(|record| record.transition == ownership.transition)
    }

    #[must_use]
    pub fn orphan_cleanup_pending(&self, thread_id: &str) -> bool {
        let state = lock(&self.inner);
        state.entries.contains_key(thread_id)
            && state
                .orphan_cleanups
                .values()
                .any(|record| record.transition.thread_id == thread_id)
    }

    fn release_removal(&self, thread_id: &str, removal_id: u64) {
        let mut state = lock(&self.inner);
        let Some(entry) = state.entries.get_mut(thread_id) else {
            return;
        };
        if entry.state != WorkspaceGuardState::Removing
            || !entry.removal_ids.remove(&removal_id)
            || !entry.removal_ids.is_empty()
        {
            return;
        }
        let _drain = entry.terminal_signal.invalidate();
        if !state.reconciliation_active {
            complete_released_removal(&mut state, thread_id);
        }
    }

    fn release_admission(&self, admission_id: u64) {
        let mut state = lock(&self.inner);
        state.active_admissions.remove(&admission_id);
        drop(state);
        self.admission_changed.notify_waiters();
    }
}

impl WorkspaceAdmissionCancellation {
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    pub async fn cancelled(&self) {
        self.token.cancelled().await;
    }

    #[must_use]
    pub fn cancellation_token(&self) -> CancellationToken {
        self.token.clone()
    }

    #[must_use]
    pub fn unavailable(&self) -> Option<WorkspaceUnavailable> {
        lock(&self.unavailable).clone()
    }

    fn publish_unavailable(&self, unavailable: WorkspaceUnavailable) {
        *lock(&self.unavailable) = Some(unavailable);
    }

    fn cancel(&self) {
        debug_assert!(lock(&self.unavailable).is_some());
        self.token.cancel();
    }
}

#[cfg(test)]
impl FinalizationRejectionPause {
    pub(crate) async fn wait_until_entered(&self) {
        self.entered.notified().await;
    }

    pub(crate) fn release(&self) {
        let (released, changed) = self.release.as_ref();
        *lock(released) = true;
        changed.notify_one();
    }

    fn block_until_released(&self) {
        self.entered.notify_one();
        let (released, changed) = self.release.as_ref();
        let mut released = lock(released);
        while !*released {
            released = changed
                .wait(released)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

#[cfg(test)]
impl TerminalSignalPermitPause {
    fn new() -> Self {
        Self {
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        }
    }

    pub(crate) async fn wait_until_entered(&self) {
        tokio::time::timeout(std::time::Duration::from_secs(1), self.entered.notified())
            .await
            .expect("terminal signal permit pause is entered");
    }

    pub(crate) fn release(&self) {
        self.release.notify_one();
    }

    async fn pause(&self) {
        self.entered.notify_one();
        self.release.notified().await;
    }
}

impl WorkspaceAdmissionLease {
    #[must_use]
    pub fn loss_cancellation(&self) -> WorkspaceAdmissionCancellation {
        self.loss_cancellation.clone()
    }

    #[must_use]
    pub(crate) fn commit_fence(&self) -> CommitFence {
        self.finalization.commit_fence()
    }

    pub(crate) fn begin_finalization(&self) -> Result<CommitPermit, WorkspaceUnavailable> {
        self.commit_fence().acquire().map_err(|_| {
            self.loss_cancellation
                .unavailable()
                .unwrap_or_else(|| WorkspaceUnavailable {
                    tag: "WorkspaceUnavailableError",
                    reason: "workspace_transition",
                    message: "The workspace became unavailable before the mutation finalized."
                        .to_owned(),
                    thread_id: String::new(),
                    path: String::new(),
                    availability: AdoptedWorktreeAvailability::Removing,
                    state: WorkspaceGuardState::Removing,
                })
        })
    }
}

impl WorkspaceFinalizationGate {
    fn new(loss_cancellation: WorkspaceAdmissionCancellation) -> Self {
        Self {
            inner: Arc::new((Mutex::new(WorkspaceFinalizationState::Open), Condvar::new())),
            loss_cancellation,
        }
    }

    fn commit_fence(&self) -> CommitFence {
        let gate = self.clone();
        CommitFence::new(move || {
            gate.begin_finalization()
                .map(|permit| Box::new(permit) as Box<dyn Send + 'static>)
        })
    }

    fn begin_finalization(&self) -> Result<WorkspaceFinalizationPermit, ()> {
        let (state, changed) = self.inner.as_ref();
        let mut state = lock(state);
        while matches!(*state, WorkspaceFinalizationState::Finalizing) {
            state = changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        if !matches!(*state, WorkspaceFinalizationState::Open) {
            return Err(());
        }
        *state = WorkspaceFinalizationState::Finalizing;
        Ok(WorkspaceFinalizationPermit { gate: self.clone() })
    }

    fn reject(&self, unavailable: WorkspaceUnavailable) {
        let (state, changed) = self.inner.as_ref();
        let mut state = lock(state);
        while matches!(*state, WorkspaceFinalizationState::Finalizing) {
            state = changed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        self.loss_cancellation.publish_unavailable(unavailable);
        *state = WorkspaceFinalizationState::Rejected;
    }

    fn finish_finalization(&self) {
        let (state, changed) = self.inner.as_ref();
        let mut state = lock(state);
        if matches!(*state, WorkspaceFinalizationState::Finalizing) {
            *state = WorkspaceFinalizationState::Finalized;
        }
        drop(state);
        changed.notify_all();
    }
}

impl WorkspaceTerminalSignalGate {
    fn begin(&self) -> Option<WorkspaceTerminalSignalPermit> {
        let mut state = lock(&self.inner);
        if state.invalidated {
            return None;
        }
        state.active_permits = state
            .active_permits
            .checked_add(1)
            .expect("workspace terminal signal permit count overflow");
        Some(WorkspaceTerminalSignalPermit { gate: self.clone() })
    }

    fn invalidate(&self) -> WorkspaceTerminalSignalDrain {
        let mut state = lock(&self.inner);
        state.invalidated = true;
        state.reopen_when_drained = false;
        #[cfg(test)]
        self.invalidation_started.notify_one();
        WorkspaceTerminalSignalDrain { gate: self.clone() }
    }

    fn is_same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    fn cancel_invalidation(&self) {
        let mut state = lock(&self.inner);
        if state.active_permits == 0 {
            state.invalidated = false;
        } else {
            state.reopen_when_drained = true;
        }
    }

    fn finish_signal(&self) {
        let mut state = lock(&self.inner);
        state.active_permits = state
            .active_permits
            .checked_sub(1)
            .expect("workspace terminal signal permit underflow");
        let signal_drained = state.active_permits == 0;
        if signal_drained && state.reopen_when_drained {
            state.invalidated = false;
            state.reopen_when_drained = false;
        }
        drop(state);
        if signal_drained {
            self.drained.notify_waiters();
        }
    }
}

impl WorkspaceTerminalSignalDrain {
    async fn wait(&self) {
        loop {
            let notified = self.gate.drained.clone().notified_owned();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if lock(&self.gate.inner).active_permits == 0 {
                return;
            }
            notified.await;
        }
    }
}

impl WorkspaceAvailabilityReconciliation {
    pub(crate) async fn wait_for_drains(&self) {
        for drain in &self.drains {
            drain.wait().await;
        }
    }

    pub(crate) fn finalize(mut self) -> Vec<WorkspaceLossTransition> {
        let mut admitted = Vec::new();
        let mut cancellations = Vec::new();
        {
            let mut state = lock(&self.registry.inner);
            for action in self.actions.drain(..) {
                match action {
                    WorkspaceAvailabilityAction::MarkUnavailable { transition, .. } => {
                        let Some(guard_state) = transition.guard_state() else {
                            continue;
                        };
                        let watermark = state
                            .latest_loss
                            .entry(transition.thread_id.clone())
                            .or_insert_with(|| LossWatermark::new(transition.generation));
                        if !watermark.admit(transition.generation, guard_state) {
                            continue;
                        }
                        let path_key = normalized_path(&transition.path);
                        let path = path_key.clone();
                        invalidate_thread_cleanups(&mut state, &transition.thread_id);
                        if let Some(entry) = state.entries.get_mut(&transition.thread_id)
                            && entry.state == WorkspaceGuardState::Removing
                        {
                            if (entry.repository_key.is_empty()
                                || entry.repository_key == transition.repository_key)
                                && entry.path_key == path_key
                            {
                                entry.repository_key = transition.repository_key.clone();
                                entry.pending_missing = Some(PendingMissing {
                                    repository_key: transition.repository_key,
                                    path_key,
                                    path,
                                    state: guard_state,
                                });
                            }
                            continue;
                        }
                        state.entries.insert(
                            transition.thread_id.clone(),
                            GuardEntry {
                                thread_id: transition.thread_id.clone(),
                                repository_key: transition.repository_key.clone(),
                                path_key: path_key.clone(),
                                path: path.clone(),
                                state: guard_state,
                                entered_at: Instant::now(),
                                terminal_signal: WorkspaceTerminalSignalGate::default(),
                                pending_missing: None,
                                removal_ids: HashSet::new(),
                            },
                        );
                        let unavailable = state
                            .entries
                            .get(&transition.thread_id)
                            .map(unavailable)
                            .expect("inserted workspace guard");
                        cancellations.extend(reject_matching_admissions(
                            &state,
                            &transition.thread_id,
                            &path_key,
                            unavailable,
                        ));
                        admitted.push(transition);
                    }
                    WorkspaceAvailabilityAction::ClearRecovered {
                        thread_id,
                        path_key,
                        repository_key,
                    } => {
                        let Some(entry) = state.entries.get_mut(&thread_id) else {
                            continue;
                        };
                        if entry.path_key != path_key
                            || repository_key.as_deref().is_some_and(|repository_key| {
                                entry.repository_key != repository_key
                            })
                        {
                            continue;
                        }
                        if entry.state == WorkspaceGuardState::Removing {
                            entry.pending_missing = None;
                        } else {
                            state.entries.remove(&thread_id);
                        }
                        invalidate_thread_cleanups(&mut state, &thread_id);
                    }
                    WorkspaceAvailabilityAction::MarkRemoving {
                        thread_id,
                        path_key,
                        removal_id,
                    } => {
                        if let Some(entry) = state.entries.get_mut(&thread_id)
                            && entry.state == WorkspaceGuardState::Removing
                        {
                            entry.removal_ids.insert(removal_id);
                            continue;
                        }
                        let previous = state.entries.remove(&thread_id);
                        let pending_missing = previous.as_ref().and_then(|entry| {
                            (entry.state != WorkspaceGuardState::Removing).then(|| PendingMissing {
                                repository_key: entry.repository_key.clone(),
                                path_key: entry.path_key.clone(),
                                path: entry.path.clone(),
                                state: entry.state,
                            })
                        });
                        let repository_key = previous
                            .as_ref()
                            .map(|entry| entry.repository_key.clone())
                            .unwrap_or_default();
                        state.entries.insert(
                            thread_id.clone(),
                            GuardEntry {
                                thread_id: thread_id.clone(),
                                repository_key,
                                path: path_key.clone(),
                                path_key: path_key.clone(),
                                state: WorkspaceGuardState::Removing,
                                entered_at: Instant::now(),
                                terminal_signal: WorkspaceTerminalSignalGate::default(),
                                pending_missing,
                                removal_ids: HashSet::from([removal_id]),
                            },
                        );
                        let unavailable = state
                            .entries
                            .get(&thread_id)
                            .map(unavailable)
                            .expect("inserted removing guard");
                        cancellations.extend(reject_matching_admissions(
                            &state,
                            &thread_id,
                            &path_key,
                            unavailable,
                        ));
                    }
                }
            }
            finish_registry_reconciliation(&mut state);
        }
        self.active = false;
        #[cfg(test)]
        if !cancellations.is_empty() {
            self.registry.maybe_pause_after_finalization_rejection();
        }
        for loss_cancellation in cancellations {
            loss_cancellation.cancel();
        }
        self._transition_guard.take();
        admitted
    }

    fn abort(&mut self) {
        if !self.active {
            return;
        }
        let mut state = lock(&self.registry.inner);
        for closed in &self.closed_gates {
            if let Some(entry) = state.entries.get(&closed.thread_id)
                && entry.terminal_signal.is_same(&closed.gate)
            {
                closed.gate.cancel_invalidation();
            }
        }
        finish_registry_reconciliation(&mut state);
        self.active = false;
    }
}

impl Drop for WorkspaceAvailabilityReconciliation {
    fn drop(&mut self) {
        self.abort();
    }
}

impl Drop for WorkspaceFinalizationPermit {
    fn drop(&mut self) {
        self.gate.finish_finalization();
    }
}

impl Drop for WorkspaceTerminalSignalPermit {
    fn drop(&mut self) {
        self.gate.finish_signal();
    }
}

impl WorkspaceCleanupOwnership {
    pub async fn cancelled(&self) {
        self.cancellation.cancelled().await;
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }
}

impl RemovalGuard {
    #[must_use]
    pub fn identity(&self) -> WorkspaceRemovalIdentity {
        let path_key = {
            let state = lock(&self.registry.inner);
            state
                .entries
                .get(&self.thread_id)
                .filter(|entry| entry.removal_ids.contains(&self.removal_id))
                .map(|entry| entry.path_key.clone())
                .unwrap_or_default()
        };
        WorkspaceRemovalIdentity {
            thread_id: self.thread_id.clone(),
            path_key,
            removal_id: self.removal_id,
        }
    }

    pub fn release(mut self) {
        self.registry
            .release_removal(&self.thread_id, self.removal_id);
        self.released = true;
    }
}

impl Drop for RemovalGuard {
    fn drop(&mut self) {
        if !self.released {
            self.registry
                .release_removal(&self.thread_id, self.removal_id);
        }
    }
}

impl Drop for WorkspaceAdmissionLease {
    fn drop(&mut self) {
        self.registry.release_admission(self.admission_id);
    }
}

fn close_terminal_signal_gate(
    thread_id: &str,
    gate: &WorkspaceTerminalSignalGate,
    closed_gates: &mut Vec<ClosedTerminalSignalGate>,
    drains: &mut Vec<WorkspaceTerminalSignalDrain>,
) {
    if closed_gates.iter().any(|closed| closed.gate.is_same(gate)) {
        return;
    }
    drains.push(gate.invalidate());
    closed_gates.push(ClosedTerminalSignalGate {
        thread_id: thread_id.to_owned(),
        gate: gate.clone(),
    });
}

fn reject_matching_admissions(
    state: &RegistryState,
    thread_id: &str,
    path_key: &str,
    unavailable: WorkspaceUnavailable,
) -> Vec<WorkspaceAdmissionCancellation> {
    let thread_scope = AdmissionScope::Thread(thread_id.to_owned());
    let matching_admissions = state
        .active_admissions
        .values()
        .filter(|admission| {
            admission.scopes.iter().any(|scope| {
                scope == &thread_scope
                    || matches!(scope, AdmissionScope::Path(candidate) if path_is_within(candidate, path_key))
            })
        })
        .map(|admission| {
            (
                admission.finalization.clone(),
                admission.loss_cancellation.clone(),
            )
        })
        .collect::<Vec<_>>();
    for (finalization, _) in &matching_admissions {
        finalization.reject(unavailable.clone());
    }
    matching_admissions
        .into_iter()
        .map(|(_, cancellation)| cancellation)
        .collect()
}

fn finish_registry_reconciliation(state: &mut RegistryState) {
    state.reconciliation_active = false;
    let released = state
        .entries
        .iter()
        .filter(|(_, entry)| {
            entry.state == WorkspaceGuardState::Removing && entry.removal_ids.is_empty()
        })
        .map(|(thread_id, _)| thread_id.clone())
        .collect::<Vec<_>>();
    for thread_id in released {
        complete_released_removal(state, &thread_id);
    }
}

fn complete_released_removal(state: &mut RegistryState, thread_id: &str) {
    let Some(mut entry) = state.entries.remove(thread_id) else {
        return;
    };
    if let Some(pending) = entry.pending_missing.take() {
        entry.repository_key = pending.repository_key;
        entry.path_key = pending.path_key;
        entry.path = pending.path;
        entry.state = pending.state;
        entry.entered_at = Instant::now();
        entry.terminal_signal = WorkspaceTerminalSignalGate::default();
        entry.removal_ids.clear();
        state.entries.insert(thread_id.to_owned(), entry);
    }
}

fn unavailable(entry: &GuardEntry) -> WorkspaceUnavailable {
    WorkspaceUnavailable {
        tag: "WorkspaceUnavailableError",
        reason: "workspace-unavailable",
        message: match entry.state {
            WorkspaceGuardState::MissingRegistered => {
                "The workspace directory is missing while Git still registers the worktree."
                    .to_owned()
            }
            WorkspaceGuardState::MissingUnregistered => {
                "The workspace is no longer registered as a Git worktree.".to_owned()
            }
            WorkspaceGuardState::Removing => {
                "The workspace is being removed from BiBCode.".to_owned()
            }
        },
        thread_id: entry.thread_id.clone(),
        path: entry.path.clone(),
        availability: entry.state.availability(),
        state: entry.state,
    }
}

fn invalidate_thread_cleanups(state: &mut RegistryState, thread_id: &str) {
    let ids = state
        .orphan_cleanups
        .iter()
        .filter_map(|(id, record)| (record.transition.thread_id == thread_id).then_some(*id))
        .collect::<Vec<_>>();
    for id in ids {
        if let Some(record) = state.orphan_cleanups.remove(&id) {
            record.cancellation.cancel();
        }
    }
}

fn normalized_path(path: &Path) -> String {
    normalize_worktree_path_key(path, host_path_platform())
}

fn path_is_within(candidate: &str, root: &str) -> bool {
    candidate == root
        || candidate
            .strip_prefix(root)
            .is_some_and(|suffix| root.ends_with('/') || suffix.starts_with('/'))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use std::{
        path::{Path, PathBuf},
        sync::Arc,
    };

    use tokio::sync::Notify;

    use crate::worktree_catalog::{
        AdoptedWorktreeAvailability, AdoptedWorktreeStatus, CatalogScanStatus,
        WorktreeCatalogSnapshot,
    };

    use super::{WorkspaceAvailabilityRegistry, WorkspaceGuardState, WorkspaceLossTransition};

    fn transition(
        repository_key: &str,
        generation: u64,
        availability: AdoptedWorktreeAvailability,
    ) -> WorkspaceLossTransition {
        WorkspaceLossTransition {
            thread_id: "thread-1".to_owned(),
            repository_key: repository_key.to_owned(),
            generation,
            path: PathBuf::from("/repo/worktrees/missing"),
            availability,
        }
    }

    fn snapshot(
        authoritative: bool,
        repository_key: &str,
        generation: u64,
        availability: AdoptedWorktreeAvailability,
    ) -> WorktreeCatalogSnapshot {
        WorktreeCatalogSnapshot {
            repository_key: repository_key.to_owned(),
            generation,
            authoritative,
            observed_at: "2026-08-10T00:00:00Z".to_owned(),
            scan_status: if authoritative {
                CatalogScanStatus::Ready
            } else {
                CatalogScanStatus::Degraded {
                    reason: super::super::CatalogDegradedReason::GitFailed,
                    message: "offline".to_owned(),
                    failed_at: "2026-08-10T00:00:00Z".to_owned(),
                    last_authoritative_at: Some("2026-08-09T23:59:00Z".to_owned()),
                }
            },
            worktrees: Vec::new(),
            adopted_workspaces: vec![AdoptedWorktreeStatus {
                thread_id: "thread-1".to_owned(),
                worktree_key: None,
                path: "/repo/worktrees/missing".to_owned(),
                branch: Some("feature/missing".to_owned()),
                availability,
                registration_state: None,
                locked: false,
                lock_reason: None,
            }],
        }
    }

    #[tokio::test]
    async fn loss_token_is_admitted_once_and_guards_thread_root_and_descendants() {
        let registry = WorkspaceAvailabilityRegistry::new();
        let missing = transition(
            "repository-a",
            7,
            AdoptedWorktreeAvailability::MissingRegistered,
        );

        assert!(
            registry
                .mark_unavailable(missing.clone())
                .await
                .expect("physical identity resolves")
        );
        assert!(
            !registry
                .mark_unavailable(missing)
                .await
                .expect("physical identity resolves")
        );
        let changed = transition(
            "repository-a",
            7,
            AdoptedWorktreeAvailability::MissingUnregistered,
        );
        assert!(
            registry
                .mark_unavailable(changed.clone())
                .await
                .expect("physical identity resolves")
        );
        assert!(
            !registry
                .mark_unavailable(changed)
                .await
                .expect("physical identity resolves")
        );

        let thread_error = registry
            .guard_thread("thread-1")
            .await
            .expect_err("missing thread is guarded");
        assert_eq!(thread_error.state, WorkspaceGuardState::MissingUnregistered);
        assert_eq!(thread_error.thread_id, "thread-1");
        assert_eq!(thread_error.path, "/repo/worktrees/missing");
        assert_eq!(
            registry
                .guard_path(Path::new("/repo/worktrees/missing/src/lib.rs"))
                .await
                .expect_err("a nested cwd cannot bypass the workspace guard")
                .unavailable()
                .expect("guard failure is workspace unavailable"),
            &thread_error
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn missing_workspace_aliases_share_one_physical_removal_guard() {
        let root = tempfile::tempdir().expect("workspace parent");
        let physical_parent = root.path().join("physical");
        std::fs::create_dir(&physical_parent).expect("physical parent");
        let alias_parent = root.path().join("alias");
        std::os::unix::fs::symlink(&physical_parent, &alias_parent).expect("parent alias");
        let alias_missing = alias_parent.join("removed-worktree");
        let physical_descendant = physical_parent.join("removed-worktree/src/lib.rs");
        let registry = WorkspaceAvailabilityRegistry::new();

        let _removing = registry
            .mark_removing("thread-1", &alias_missing)
            .await
            .expect("physical identity resolves");

        let unavailable = registry
            .guard_path(&physical_descendant)
            .await
            .expect_err("a missing checkout alias cannot bypass its physical removal guard");
        let unavailable = unavailable
            .unavailable()
            .expect("alias resolves to the removal guard");
        assert_eq!(unavailable.thread_id, "thread-1");
        assert_eq!(unavailable.state, WorkspaceGuardState::Removing);
    }

    #[tokio::test]
    async fn non_missing_identity_failures_abort_every_authority_changing_transition() {
        let path = Path::new("/repo/worktrees/identity-error");

        let unavailable_registry =
            WorkspaceAvailabilityRegistry::with_identity_resolution_error_for_test(
                std::io::ErrorKind::PermissionDenied,
            );
        let unavailable_error = unavailable_registry
            .mark_unavailable(WorkspaceLossTransition {
                thread_id: "thread-identity".to_owned(),
                repository_key: "repository-a".to_owned(),
                generation: 1,
                path: path.to_path_buf(),
                availability: AdoptedWorktreeAvailability::MissingRegistered,
            })
            .await
            .expect_err("permission errors cannot become lexical guard authority");
        assert_eq!(unavailable_error.tag, "WorkspaceIdentityError");
        assert_eq!(
            unavailable_registry.guard_thread("thread-identity").await,
            Ok(())
        );

        let removal_registry =
            WorkspaceAvailabilityRegistry::with_identity_resolution_error_for_test(
                std::io::ErrorKind::Other,
            );
        let Err(removal_error) = removal_registry
            .mark_removing("thread-identity", path)
            .await
        else {
            panic!("symlink loops cannot create a lexical removal guard");
        };
        assert_eq!(removal_error.tag, "WorkspaceIdentityError");
        assert_eq!(
            removal_registry.guard_thread("thread-identity").await,
            Ok(())
        );

        let admission_registry =
            WorkspaceAvailabilityRegistry::with_identity_resolution_error_for_test(
                std::io::ErrorKind::PermissionDenied,
            );
        let Err(admission_error) = admission_registry.acquire_path_admission([path]).await else {
            panic!("identity failures cannot admit an unverified filesystem operation");
        };
        assert_eq!(
            admission_error
                .identity()
                .expect("typed identity error")
                .tag,
            "WorkspaceIdentityError"
        );

        let recovery_registry = WorkspaceAvailabilityRegistry::new();
        recovery_registry
            .mark_unavailable(WorkspaceLossTransition {
                thread_id: "thread-recovery".to_owned(),
                repository_key: "repository-a".to_owned(),
                generation: 1,
                path: path.to_path_buf(),
                availability: AdoptedWorktreeAvailability::MissingRegistered,
            })
            .await
            .expect("initial physical identity resolves");
        recovery_registry.fail_identity_resolution_for_test(std::io::ErrorKind::PermissionDenied);
        let recovery_error = recovery_registry
            .clear_recovered("thread-recovery", path)
            .await
            .expect_err("identity failure aborts recovery");
        assert_eq!(recovery_error.tag, "WorkspaceIdentityError");
        assert!(
            recovery_registry
                .guard_thread("thread-recovery")
                .await
                .is_err(),
            "failed recovery cannot clear the existing guard"
        );
    }

    #[test]
    fn path_ancestry_handles_filesystem_roots_without_prefix_false_positives() {
        assert!(super::path_is_within("/repo/file", "/"));
        assert!(super::path_is_within("c:/repo/file", "c:/"));
        assert!(!super::path_is_within("/repository/file", "/repo"));
    }

    #[tokio::test]
    async fn recovery_requires_the_same_thread_path_and_repository() {
        let registry = WorkspaceAvailabilityRegistry::new();
        assert!(
            registry
                .mark_unavailable(transition(
                    "repository-a",
                    8,
                    AdoptedWorktreeAvailability::MissingUnregistered,
                ))
                .await
                .expect("physical identity resolves")
        );

        registry
            .clear_recovered_in_repository(
                "thread-1",
                Path::new("/repo/worktrees/other"),
                "repository-a",
            )
            .await
            .expect("physical identity resolves");
        registry
            .clear_recovered_in_repository(
                "thread-1",
                Path::new("/repo/worktrees/missing"),
                "repository-b",
            )
            .await
            .expect("physical identity resolves");
        assert!(registry.guard_thread("thread-1").await.is_err());

        registry
            .clear_recovered_in_repository(
                "thread-1",
                Path::new("/repo/worktrees/missing"),
                "repository-a",
            )
            .await
            .expect("physical identity resolves");
        assert_eq!(registry.guard_thread("thread-1").await, Ok(()));
    }

    #[tokio::test]
    async fn guarded_lists_each_guard_with_how_long_it_has_held_its_state() {
        let registry = WorkspaceAvailabilityRegistry::new();
        assert!(registry.guarded().is_empty());
        let removal = registry
            .mark_removing("thread-1", Path::new("/repo/worktrees/a"))
            .await
            .expect("physical identity resolves");
        let guarded = registry.guarded();
        let [(unavailable, held)] = guarded.as_slice() else {
            panic!("one guard: {guarded:?}");
        };
        assert_eq!(unavailable.thread_id, "thread-1");
        assert_eq!(
            unavailable.availability,
            AdoptedWorktreeAvailability::Removing
        );
        assert!(*held < std::time::Duration::from_secs(60));
        removal.release();
        assert!(registry.guarded().is_empty());
    }

    #[tokio::test]
    async fn removing_precedes_concurrent_loss_and_restores_latest_missing_state_on_release() {
        let registry = WorkspaceAvailabilityRegistry::new();
        assert!(
            registry
                .mark_unavailable(transition(
                    "repository-a",
                    9,
                    AdoptedWorktreeAvailability::MissingRegistered,
                ))
                .await
                .expect("physical identity resolves")
        );
        let removal = registry
            .mark_removing("thread-1", Path::new("/repo/worktrees/missing"))
            .await
            .expect("physical identity resolves");
        let removing = registry
            .guard_thread("thread-1")
            .await
            .expect_err("removing workspace remains guarded");
        assert_eq!(removing.state, WorkspaceGuardState::Removing);

        assert!(
            !registry
                .mark_unavailable(transition(
                    "repository-a",
                    10,
                    AdoptedWorktreeAvailability::MissingUnregistered,
                ))
                .await
                .expect("physical identity resolves")
        );
        assert_eq!(
            registry
                .guard_thread("thread-1")
                .await
                .expect_err("loss cannot overwrite removing")
                .state,
            WorkspaceGuardState::Removing
        );

        drop(removal);
        assert_eq!(
            registry
                .guard_thread("thread-1")
                .await
                .expect_err("release restores the newest proven missing state")
                .state,
            WorkspaceGuardState::MissingUnregistered
        );
    }

    #[tokio::test]
    async fn nested_removal_guards_preserve_missing_state_until_the_last_token_drops() {
        let registry = WorkspaceAvailabilityRegistry::new();
        assert!(
            registry
                .mark_unavailable(transition(
                    "repository-a",
                    10,
                    AdoptedWorktreeAvailability::MissingRegistered,
                ))
                .await
                .expect("physical identity resolves")
        );
        let outer = registry
            .mark_removing("thread-1", Path::new("/repo/worktrees/missing"))
            .await
            .expect("physical identity resolves");
        let middle = registry
            .mark_removing("thread-1", Path::new("/repo/worktrees/missing"))
            .await
            .expect("physical identity resolves");
        let inner = registry
            .mark_removing("thread-1", Path::new("/repo/worktrees/missing"))
            .await
            .expect("physical identity resolves");

        drop(middle);
        drop(outer);
        assert_eq!(
            registry
                .guard_thread("thread-1")
                .await
                .expect_err("one live removal token retains removing precedence")
                .state,
            WorkspaceGuardState::Removing,
        );

        drop(inner);
        assert_eq!(
            registry
                .guard_thread("thread-1")
                .await
                .expect_err("the final removal token restores the pending loss")
                .state,
            WorkspaceGuardState::MissingRegistered,
        );
    }

    #[tokio::test]
    async fn removal_identity_is_exact_and_terminal_signals_close_on_final_release() {
        let registry = WorkspaceAvailabilityRegistry::new();
        let first = registry
            .mark_removing("thread-1", Path::new("/repo/worktrees/missing"))
            .await
            .expect("physical identity resolves");
        let identity = first.identity();
        assert!(registry.removal_is_current(&identity));
        let signal = registry
            .begin_removal_terminal_signal(&identity)
            .await
            .expect("exact removal terminal signal");

        drop(first);
        assert!(!registry.removal_is_current(&identity));
        assert!(
            registry
                .begin_removal_terminal_signal(&identity)
                .await
                .is_none()
        );
        drop(signal);
        assert_eq!(registry.guard_thread("thread-1").await, Ok(()));
    }

    #[tokio::test]
    async fn removing_synchronously_cancels_an_existing_path_admission() {
        let registry = WorkspaceAvailabilityRegistry::new();
        let lease = registry
            .acquire_admission("panel-1", [Path::new("/repo/worktrees/missing/src")])
            .await
            .expect("panel admission");
        let cancellation = lease.loss_cancellation();

        let _removal = registry
            .mark_removing("thread-1", Path::new("/repo/worktrees/missing"))
            .await
            .expect("physical identity resolves");

        assert!(cancellation.is_cancelled());
        assert_eq!(
            cancellation.unavailable().expect("removing error").state,
            WorkspaceGuardState::Removing
        );
    }

    #[tokio::test]
    async fn degraded_snapshot_is_a_no_op_and_authoritative_recovery_clears_exact_guard() {
        let registry = WorkspaceAvailabilityRegistry::new();

        let degraded = snapshot(
            false,
            "repository-a",
            11,
            AdoptedWorktreeAvailability::MissingRegistered,
        );
        assert!(
            registry
                .reconcile_snapshot(&degraded)
                .await
                .expect("physical identity resolves")
                .is_empty()
        );
        assert_eq!(registry.guard_thread("thread-1").await, Ok(()));

        let missing = snapshot(
            true,
            "repository-a",
            12,
            AdoptedWorktreeAvailability::MissingRegistered,
        );
        assert_eq!(
            registry
                .reconcile_snapshot(&missing)
                .await
                .expect("physical identity resolves")
                .len(),
            1
        );
        assert!(registry.guard_thread("thread-1").await.is_err());

        let recovered = snapshot(
            true,
            "repository-a",
            13,
            AdoptedWorktreeAvailability::Present,
        );
        assert!(
            registry
                .reconcile_snapshot(&recovered)
                .await
                .expect("physical identity resolves")
                .is_empty()
        );
        assert_eq!(registry.guard_thread("thread-1").await, Ok(()));
    }

    #[tokio::test]
    async fn completing_one_reaper_job_does_not_clear_another_pending_cleanup() {
        let registry = WorkspaceAvailabilityRegistry::new();
        assert!(
            registry
                .mark_unavailable(transition(
                    "repository-a",
                    14,
                    AdoptedWorktreeAvailability::MissingRegistered,
                ))
                .await
                .expect("physical identity resolves")
        );
        let transition = transition(
            "repository-a",
            14,
            AdoptedWorktreeAvailability::MissingRegistered,
        );
        let first = registry
            .begin_orphan_cleanup(&transition)
            .expect("first cleanup ownership");
        let second = registry
            .begin_orphan_cleanup(&transition)
            .expect("second cleanup ownership");

        registry.complete_orphan_cleanup(&first);
        assert!(registry.orphan_cleanup_pending("thread-1"));

        registry.complete_orphan_cleanup(&second);
        assert!(!registry.orphan_cleanup_pending("thread-1"));
    }

    #[tokio::test]
    async fn newer_loss_invalidates_older_active_or_saturated_cleanup_ownership() {
        let registry = WorkspaceAvailabilityRegistry::new();
        let older = transition(
            "repository-a",
            15,
            AdoptedWorktreeAvailability::MissingRegistered,
        );
        assert!(
            registry
                .mark_unavailable(older.clone())
                .await
                .expect("physical identity resolves")
        );
        let active = registry
            .begin_orphan_cleanup(&older)
            .expect("active ownership");
        let saturated = registry
            .begin_orphan_cleanup(&older)
            .expect("saturated ownership remains registered without a job");

        let newer = transition(
            "repository-a",
            16,
            AdoptedWorktreeAvailability::MissingUnregistered,
        );
        assert!(
            registry
                .mark_unavailable(newer)
                .await
                .expect("physical identity resolves")
        );

        assert!(active.is_cancelled());
        assert!(saturated.is_cancelled());
        assert!(!registry.cleanup_is_current(&active));
        assert!(!registry.cleanup_is_current(&saturated));
        assert!(!registry.orphan_cleanup_pending("thread-1"));
    }

    #[tokio::test]
    async fn newer_loss_drains_all_terminal_signal_permits_without_blocking_the_runtime() {
        let registry = WorkspaceAvailabilityRegistry::new();
        let older = transition(
            "repository-a",
            16,
            AdoptedWorktreeAvailability::MissingRegistered,
        );
        assert!(
            registry
                .mark_unavailable(older.clone())
                .await
                .expect("physical identity resolves")
        );
        let first = registry
            .begin_terminal_signal(&older)
            .await
            .expect("first terminal signal permit");
        let second = registry
            .begin_terminal_signal(&older)
            .await
            .expect("second terminal signal permit");
        let invalidation_started = registry
            .terminal_signal_invalidation_notification(&older)
            .await
            .expect("current terminal signal gate");

        let newer = transition(
            "repository-a",
            17,
            AdoptedWorktreeAvailability::MissingUnregistered,
        );
        let newer_registry = registry.clone();
        let newer_transition = newer.clone();
        let newer_loss =
            tokio::spawn(async move { newer_registry.mark_unavailable(newer_transition).await });

        let first_release = Arc::new(Notify::new());
        let first_cleanup_release = first_release.clone();
        let first_cleanup = tokio::spawn(async move {
            first_cleanup_release.notified().await;
            tokio::task::yield_now().await;
            drop(first);
        });
        let second_release = Arc::new(Notify::new());
        let second_cleanup_release = second_release.clone();
        let second_cleanup = tokio::spawn(async move {
            second_cleanup_release.notified().await;
            tokio::task::yield_now().await;
            drop(second);
        });

        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            invalidation_started.notified(),
        )
        .await
        .expect("the newer transition invalidates the old signal gate");
        first_release.notify_one();
        first_cleanup.await.expect("first cleanup task");
        assert!(
            !newer_loss.is_finished(),
            "the newer transition waits for every current signal permit"
        );
        second_release.notify_one();
        second_cleanup.await.expect("second cleanup task");
        assert!(
            newer_loss
                .await
                .expect("newer loss task")
                .expect("physical identity resolves"),
            "the newer transition installs after terminal signaling finishes"
        );
        assert!(registry.begin_terminal_signal(&older).await.is_none());
        drop(
            registry
                .begin_terminal_signal(&newer)
                .await
                .expect("new transition signal permit"),
        );
    }

    #[tokio::test]
    async fn recovery_drain_allows_cleanup_timeout_to_release_on_current_thread() {
        let registry = WorkspaceAvailabilityRegistry::new();
        let loss = transition(
            "repository-a",
            18,
            AdoptedWorktreeAvailability::MissingRegistered,
        );
        assert!(
            registry
                .mark_unavailable(loss.clone())
                .await
                .expect("physical identity resolves")
        );
        let permit = registry
            .begin_terminal_signal(&loss)
            .await
            .expect("terminal cleanup owns the missing transition");
        let invalidation_started = registry
            .terminal_signal_invalidation_notification(&loss)
            .await
            .expect("current terminal signal gate");
        let cleanup = tokio::spawn(async move {
            let _permit = permit;
            tokio::time::timeout(
                std::time::Duration::from_millis(10),
                std::future::pending::<()>(),
            )
            .await
            .expect_err("cleanup reaches its deadline");
            tokio::task::yield_now().await;
        });
        let recovery_registry = registry.clone();
        let recovery = tokio::spawn(async move {
            recovery_registry
                .clear_recovered_in_repository(
                    "thread-1",
                    Path::new("/repo/worktrees/missing"),
                    "repository-a",
                )
                .await
                .expect("physical identity resolves");
        });
        invalidation_started.notified().await;

        cleanup.await.expect("cleanup task");
        tokio::time::timeout(std::time::Duration::from_secs(1), recovery)
            .await
            .expect("cleanup timeout can run while recovery drains")
            .expect("recovery task");
        assert_eq!(registry.guard_thread("thread-1").await, Ok(()));
    }

    #[tokio::test]
    async fn cancelled_recovery_reopens_signaling_only_after_the_owned_permit_drains() {
        let registry = WorkspaceAvailabilityRegistry::new();
        let loss = transition(
            "repository-a",
            19,
            AdoptedWorktreeAvailability::MissingRegistered,
        );
        assert!(
            registry
                .mark_unavailable(loss.clone())
                .await
                .expect("physical identity resolves")
        );
        let permit = registry
            .begin_terminal_signal(&loss)
            .await
            .expect("terminal cleanup owns the missing transition");
        let invalidation_started = registry
            .terminal_signal_invalidation_notification(&loss)
            .await
            .expect("current terminal signal gate");
        let recovery_registry = registry.clone();
        let recovery = tokio::spawn(async move {
            recovery_registry
                .clear_recovered_in_repository(
                    "thread-1",
                    Path::new("/repo/worktrees/missing"),
                    "repository-a",
                )
                .await
                .expect("physical identity resolves");
        });
        invalidation_started.notified().await;
        recovery.abort();
        assert!(
            recovery
                .await
                .expect_err("recovery task was cancelled")
                .is_cancelled()
        );

        assert!(
            registry.begin_terminal_signal(&loss).await.is_none(),
            "a cancelled transition stays closed until the old permit drains"
        );
        drop(permit);
        let retry_permit = registry
            .begin_terminal_signal(&loss)
            .await
            .expect("the retained missing transition reopens after drain");
        drop(retry_permit);
        registry
            .clear_recovered_in_repository(
                "thread-1",
                Path::new("/repo/worktrees/missing"),
                "repository-a",
            )
            .await
            .expect("physical identity resolves");
        assert_eq!(registry.guard_thread("thread-1").await, Ok(()));
    }

    #[tokio::test]
    async fn removal_drains_terminal_signal_without_blocking_the_runtime() {
        let registry = WorkspaceAvailabilityRegistry::new();
        let loss = transition(
            "repository-a",
            20,
            AdoptedWorktreeAvailability::MissingRegistered,
        );
        assert!(
            registry
                .mark_unavailable(loss.clone())
                .await
                .expect("physical identity resolves")
        );
        let permit = registry
            .begin_terminal_signal(&loss)
            .await
            .expect("terminal cleanup owns the missing transition");
        let invalidation_started = registry
            .terminal_signal_invalidation_notification(&loss)
            .await
            .expect("current terminal signal gate");
        let cleanup_release = Arc::new(Notify::new());
        let task_cleanup_release = cleanup_release.clone();
        let cleanup = tokio::spawn(async move {
            task_cleanup_release.notified().await;
            tokio::task::yield_now().await;
            drop(permit);
        });
        let removal_registry = registry.clone();
        let removal = tokio::spawn(async move {
            removal_registry
                .mark_removing("thread-1", Path::new("/repo/worktrees/missing"))
                .await
        });
        invalidation_started.notified().await;

        assert!(!removal.is_finished());
        cleanup_release.notify_one();
        cleanup.await.expect("cleanup task");
        let guard = removal
            .await
            .expect("removal task")
            .expect("physical identity resolves");
        assert_eq!(
            registry
                .guard_thread("thread-1")
                .await
                .expect_err("removal remains guarded")
                .availability,
            AdoptedWorktreeAvailability::Removing
        );
        guard.release();
        assert_eq!(
            registry
                .guard_thread("thread-1")
                .await
                .expect_err("the prior missing state is restored")
                .availability,
            AdoptedWorktreeAvailability::MissingRegistered
        );
    }

    #[tokio::test]
    async fn loss_waits_for_an_earlier_path_admission_and_rejects_later_admission() {
        let registry = WorkspaceAvailabilityRegistry::new();
        let lease = registry
            .acquire_admission("panel-1", [Path::new("/repo/worktrees/missing/./src")])
            .await
            .expect("an available panel path is admitted");
        let loss = transition(
            "repository-a",
            15,
            AdoptedWorktreeAvailability::MissingRegistered,
        );
        assert!(
            registry
                .mark_unavailable(loss.clone())
                .await
                .expect("physical identity resolves")
        );

        let waiter_registry = registry.clone();
        let waiter_loss = loss.clone();
        let waiter = tokio::spawn(async move {
            waiter_registry
                .wait_for_transition_admissions(&waiter_loss)
                .await;
        });
        tokio::task::yield_now().await;
        assert!(
            !waiter.is_finished(),
            "loss cleanup cannot run before the earlier admission publishes"
        );

        drop(lease);
        waiter.await.expect("admission drain finishes");
        assert!(
            registry
                .acquire_admission("panel-1", [Path::new("/repo/worktrees/missing/src")],)
                .await
                .is_err(),
            "the synchronously installed path guard rejects later panel admission",
        );
    }

    #[tokio::test]
    async fn loss_synchronously_cancels_an_earlier_admission() {
        let registry = WorkspaceAvailabilityRegistry::new();
        let lease = registry
            .acquire_admission("panel-1", [Path::new("/repo/worktrees/missing/src")])
            .await
            .expect("an available panel path is admitted");
        let cancellation = lease.loss_cancellation();

        assert!(
            registry
                .mark_unavailable(transition(
                    "repository-a",
                    16,
                    AdoptedWorktreeAvailability::MissingRegistered,
                ))
                .await
                .expect("physical identity resolves")
        );

        assert!(
            cancellation.is_cancelled(),
            "guard installation must cancel the admitted operation before returning"
        );
        assert_eq!(
            cancellation
                .unavailable()
                .expect("loss cancellation retains the structured boundary error")
                .thread_id,
            "thread-1"
        );
    }

    #[tokio::test]
    async fn cancelled_admission_owner_releases_every_thread_and_path_scope() {
        let registry = WorkspaceAvailabilityRegistry::new();
        let task_registry = registry.clone();
        let (acquired_tx, acquired_rx) = tokio::sync::oneshot::channel();
        let owner = tokio::spawn(async move {
            let _lease = task_registry
                .acquire_admission("panel-1", [Path::new("/repo/worktrees/missing/src")])
                .await
                .expect("admission");
            acquired_tx.send(()).expect("announce admission");
            std::future::pending::<()>().await;
        });
        acquired_rx.await.expect("admission acquired");
        let loss = transition(
            "repository-a",
            16,
            AdoptedWorktreeAvailability::MissingRegistered,
        );
        assert!(
            registry
                .mark_unavailable(loss.clone())
                .await
                .expect("physical identity resolves")
        );
        let waiter_registry = registry.clone();
        let waiter = tokio::spawn(async move {
            waiter_registry.wait_for_transition_admissions(&loss).await;
        });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());

        owner.abort();
        assert!(owner.await.expect_err("owner was cancelled").is_cancelled());
        waiter
            .await
            .expect("cancellation drops the admission lease");
    }

    #[tokio::test]
    async fn dropping_a_finalization_permit_unblocks_loss_and_rejects_reuse() {
        let registry = WorkspaceAvailabilityRegistry::new();
        let lease = registry
            .acquire_admission("thread-1", [Path::new("/repo/worktrees/missing")])
            .await
            .expect("workspace admission");
        let fence = lease.commit_fence();
        let permit = fence.acquire().expect("finalization permit");
        let loss = transition(
            "repository-a",
            17,
            AdoptedWorktreeAvailability::MissingRegistered,
        );
        let loss_registry = registry.clone();
        let runtime = tokio::runtime::Handle::current();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let loss_task = tokio::task::spawn_blocking(move || {
            let _ = started_tx.send(());
            runtime.block_on(loss_registry.mark_unavailable(loss))
        });
        started_rx.await.expect("loss task starts");
        tokio::task::yield_now().await;
        assert!(
            !loss_task.is_finished(),
            "loss waits while finalization owns the gate"
        );

        drop(permit);
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(5), loss_task)
                .await
                .expect("loss unblocks after permit drop")
                .expect("loss task joins")
                .expect("physical identity resolves")
        );
        assert!(
            fence.acquire().is_err(),
            "a dropped/error finalization cannot be retried after loss"
        );
        assert!(lease.loss_cancellation().is_cancelled());
    }
}
