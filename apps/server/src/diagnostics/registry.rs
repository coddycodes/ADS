use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex, Weak},
    time::{Duration, Instant},
};
use thiserror::Error;

use super::{
    AttributionKind, AttributionScope, PROCESS_CLAIM_LABEL_MAX_SCALARS, ProcessClaim,
    ProcessIdentity, ProcessRow,
};

const REGISTRY_CAPACITY: usize = 512;
const FIRST_OBSERVATION_TTL: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegistrationSource {
    Provider,
    Terminal,
    DesktopUi,
    Helper,
}

#[derive(Clone, Debug)]
pub struct ProcessRegistrationMetadata {
    pub scope: AttributionScope,
    pub kind: AttributionKind,
    pub label: String,
    pub source: RegistrationSource,
    /// A provider process's driver (`codex`, `claudeAgent`), which names its profiler kind
    /// `provider:<driver>`.
    pub driver: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ProcessAttributionRegistry {
    inner: Arc<Mutex<RegistryState>>,
}

#[derive(Debug)]
pub struct ProcessRegistration {
    registration_id: u64,
    registry: Weak<Mutex<RegistryState>>,
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub(crate) enum ProcessRegistrationError {
    #[error("process ownership registration is closed for shutdown")]
    Shutdown,
    #[error("process ownership registry capacity is exhausted")]
    Capacity,
}

#[derive(Debug)]
struct RegistryState {
    accepting_registrations: bool,
    next_registration_id: u64,
    entries: HashMap<u64, RegistryEntry>,
    registration_order: VecDeque<u64>,
}

#[derive(Debug)]
struct RegistryEntry {
    identity: ProcessIdentity,
    metadata: ProcessRegistrationMetadata,
    registered_at: Instant,
    observation: RegistrationObservation,
}

#[derive(Clone, Copy, Debug)]
enum RegistrationObservation {
    AwaitingFirstSample { deadline: Instant },
    Observed,
    Suppressed,
}

impl ProcessAttributionRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(RegistryState {
                accepting_registrations: true,
                next_registration_id: 0,
                entries: HashMap::new(),
                registration_order: VecDeque::new(),
            })),
        }
    }

    pub(crate) fn register_identity(
        &self,
        identity: ProcessIdentity,
        mut metadata: ProcessRegistrationMetadata,
    ) -> Result<ProcessRegistration, ProcessRegistrationError> {
        let now = Instant::now();
        let mut state = lock_state(&self.inner);
        if !state.accepting_registrations {
            return Err(ProcessRegistrationError::Shutdown);
        }
        if state.entries.len() >= REGISTRY_CAPACITY {
            tracing::warn!(
                capacity = REGISTRY_CAPACITY,
                "process attribution registry is full; rejecting new process root"
            );
            return Err(ProcessRegistrationError::Capacity);
        }

        metadata.label = normalize_label(&metadata.label);
        let registration_id = next_unused_registration_id(&mut state);
        state.entries.insert(
            registration_id,
            RegistryEntry {
                identity,
                metadata,
                registered_at: now,
                observation: RegistrationObservation::AwaitingFirstSample {
                    deadline: now + FIRST_OBSERVATION_TTL,
                },
            },
        );
        state.registration_order.push_back(registration_id);

        Ok(ProcessRegistration {
            registration_id,
            registry: Arc::downgrade(&self.inner),
        })
    }

    pub(crate) fn freeze_and_snapshot_identities(&self) -> Vec<ProcessIdentity> {
        let mut state = lock_state(&self.inner);
        state.accepting_registrations = false;
        state
            .registration_order
            .iter()
            .filter_map(|registration_id| {
                state
                    .entries
                    .get(registration_id)
                    .map(|entry| entry.identity)
            })
            .collect()
    }

    /// Every registered process and its source. Unlike `bind_and_snapshot` it leaves the
    /// observation state alone: that state belongs to the on-demand monitor, and a sampler
    /// that sees only part of the process table would suppress claims the monitor reports.
    pub(crate) fn registered_sources(&self) -> Vec<(ProcessIdentity, RegistrationSource)> {
        lock_state(&self.inner)
            .entries
            .values()
            .map(|entry| (entry.identity, entry.metadata.source))
            .collect()
    }

    /// [`registered_sources`](Self::registered_sources) with each provider's driver.
    pub(crate) fn registered_kinds(
        &self,
    ) -> Vec<(ProcessIdentity, RegistrationSource, Option<String>)> {
        lock_state(&self.inner)
            .entries
            .values()
            .map(|entry| {
                (
                    entry.identity,
                    entry.metadata.source,
                    entry.metadata.driver.clone(),
                )
            })
            .collect()
    }

    #[must_use]
    pub fn bind_and_snapshot(
        &self,
        rows: &[ProcessRow],
        sample_started_at: Instant,
    ) -> Vec<ProcessClaim> {
        let mut sampled_identities = HashSet::with_capacity(rows.len());
        let mut sampled_pids = HashSet::with_capacity(rows.len());
        for row in rows {
            let identity = ProcessIdentity {
                pid: row.pid,
                started_at: row.started_at,
            };
            sampled_identities.insert(identity);
            sampled_pids.insert(identity.pid);
        }

        let mut state = lock_state(&self.inner);
        let mut snapshot = Vec::with_capacity(state.entries.len());
        {
            let RegistryState {
                entries,
                registration_order,
                ..
            } = &mut *state;
            registration_order.retain(|registration_id| {
                let Some(entry) = entries.get_mut(registration_id) else {
                    return false;
                };
                let emit_claim = match entry.observation {
                    RegistrationObservation::AwaitingFirstSample { .. }
                        if sample_started_at < entry.registered_at
                            && sampled_identities.contains(&entry.identity) =>
                    {
                        entry.observation = RegistrationObservation::Observed;
                        true
                    }
                    RegistrationObservation::AwaitingFirstSample { .. }
                        if sample_started_at < entry.registered_at =>
                    {
                        false
                    }
                    RegistrationObservation::AwaitingFirstSample { deadline }
                        if deadline <= sample_started_at =>
                    {
                        entry.observation = RegistrationObservation::Suppressed;
                        false
                    }
                    RegistrationObservation::AwaitingFirstSample { .. }
                        if sampled_identities.contains(&entry.identity) =>
                    {
                        entry.observation = RegistrationObservation::Observed;
                        true
                    }
                    RegistrationObservation::AwaitingFirstSample { .. }
                        if sampled_pids.contains(&entry.identity.pid) =>
                    {
                        entry.observation = RegistrationObservation::Suppressed;
                        false
                    }
                    RegistrationObservation::AwaitingFirstSample { .. } => false,
                    RegistrationObservation::Observed
                        if sampled_identities.contains(&entry.identity) =>
                    {
                        true
                    }
                    RegistrationObservation::Observed
                        if sample_started_at < entry.registered_at =>
                    {
                        false
                    }
                    RegistrationObservation::Observed => {
                        entry.observation = RegistrationObservation::Suppressed;
                        false
                    }
                    RegistrationObservation::Suppressed => false,
                };
                if emit_claim {
                    snapshot.push(ProcessClaim {
                        identity: entry.identity,
                        scope: entry.metadata.scope,
                        kind: entry.metadata.kind,
                        label: entry.metadata.label.clone(),
                    });
                }
                true
            });
        }
        snapshot
    }
}

impl Default for ProcessAttributionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessRegistration {
    pub fn unregister(&self) {
        if let Some(registry) = self.registry.upgrade() {
            let mut state = lock_state(&registry);
            if state.entries.remove(&self.registration_id).is_some() {
                state
                    .registration_order
                    .retain(|registration_id| *registration_id != self.registration_id);
            }
        }
    }
}

impl Drop for ProcessRegistration {
    fn drop(&mut self) {
        self.unregister();
    }
}

fn lock_state(registry: &Mutex<RegistryState>) -> std::sync::MutexGuard<'_, RegistryState> {
    registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn next_unused_registration_id(state: &mut RegistryState) -> u64 {
    let mut registration_id = state.next_registration_id;
    for _ in 0..REGISTRY_CAPACITY {
        if !state.entries.contains_key(&registration_id) {
            state.next_registration_id = registration_id.wrapping_add(1);
            return registration_id;
        }
        registration_id = registration_id.wrapping_add(1);
    }
    unreachable!("registry capacity was checked before allocating an ID")
}

fn normalize_label(label: &str) -> String {
    let mut normalized = String::with_capacity(PROCESS_CLAIM_LABEL_MAX_SCALARS);
    let mut pending_separator = false;
    let mut output_scalars = 0_usize;
    for character in label.chars() {
        if character.is_whitespace() {
            pending_separator = !normalized.is_empty();
            continue;
        }
        let required_scalars = usize::from(pending_separator) + 1;
        if output_scalars.saturating_add(required_scalars) > PROCESS_CLAIM_LABEL_MAX_SCALARS {
            break;
        }
        if pending_separator {
            normalized.push(' ');
            output_scalars += 1;
            pending_separator = false;
        }
        normalized.push(character);
        output_scalars += 1;
        if output_scalars == PROCESS_CLAIM_LABEL_MAX_SCALARS {
            break;
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::diagnostics::{
        AttributionKind, AttributionScope, ProcessIdentity, ProcessRow, ResourceAttributor,
        UiCoverage,
    };

    fn metadata(label: &str) -> ProcessRegistrationMetadata {
        ProcessRegistrationMetadata {
            scope: AttributionScope::External,
            kind: AttributionKind::Provider,
            label: label.to_owned(),
            source: RegistrationSource::Provider,
            driver: None,
        }
    }

    fn row(pid: u32, ppid: u32, started_at: u64) -> ProcessRow {
        ProcessRow {
            pid,
            started_at,
            ppid,
            pgid: None,
            status: "Run".to_owned(),
            cpu_percent: 0.0,
            cpu_core_percent: Some(0.0),
            rss_bytes: 0,
            elapsed: "00:00:00".to_owned(),
            command: format!("process-{pid}"),
        }
    }

    fn identity(pid: u32, started_at: u64) -> ProcessIdentity {
        ProcessIdentity { pid, started_at }
    }

    fn register(
        registry: &ProcessAttributionRegistry,
        pid: u32,
        started_at: u64,
        label: &str,
    ) -> ProcessRegistration {
        registry
            .register_identity(identity(pid, started_at), metadata(label))
            .expect("registration should fit")
    }

    #[test]
    fn captured_identity_refuses_pid_reuse_before_initial_sampling() {
        let registry = ProcessAttributionRegistry::new();
        let _registration = register(&registry, 42, 100, "external/provider");

        let claims = registry.bind_and_snapshot(&[row(42, 1, 200)], Instant::now());

        assert!(claims.is_empty());
        assert!(
            registry
                .bind_and_snapshot(&[row(42, 1, 100)], Instant::now())
                .is_empty()
        );
    }

    #[test]
    fn captured_identity_survives_a_sample_that_started_before_registration() {
        let registry = ProcessAttributionRegistry::new();
        let sample_started_at = Instant::now();
        let _registration = register(&registry, 42, 100, "external/provider");

        assert!(
            registry
                .bind_and_snapshot(&[row(42, 1, 50)], sample_started_at)
                .is_empty()
        );
        let claims = registry.bind_and_snapshot(&[row(42, 1, 100)], Instant::now());

        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].identity, identity(42, 100));
    }

    #[test]
    fn expired_diagnostic_visibility_preserves_shutdown_ownership_until_token_drop() {
        let registry = ProcessAttributionRegistry::new();
        let registration = register(&registry, 42, 100, "external/provider");
        let registered_at = Instant::now();

        let claims =
            registry.bind_and_snapshot(&[row(42, 1, 100)], registered_at + FIRST_OBSERVATION_TTL);

        assert!(claims.is_empty());
        assert_eq!(
            registry.freeze_and_snapshot_identities(),
            [identity(42, 100)],
            "diagnostic observation expiry must not evict a live process owner"
        );
        drop(registration);
        assert!(registry.freeze_and_snapshot_identities().is_empty());
    }

    #[test]
    fn captured_identity_appears_in_a_matching_snapshot() {
        let registry = ProcessAttributionRegistry::new();
        let _registration = register(&registry, 42, 100, "external/provider");

        let claims = registry.bind_and_snapshot(&[row(42, 1, 100)], Instant::now());

        assert_eq!(claims.len(), 1);
        assert_eq!(
            claims[0].identity,
            ProcessIdentity {
                pid: 42,
                started_at: 100
            }
        );
    }

    #[test]
    fn matching_identity_remains_registered_across_snapshots() {
        let registry = ProcessAttributionRegistry::new();
        let _registration = register(&registry, 42, 100, "external/provider");

        let first = registry.bind_and_snapshot(&[row(42, 1, 100)], Instant::now());
        let second = registry.bind_and_snapshot(&[row(42, 1, 100)], Instant::now());

        assert_eq!(
            first[0].identity,
            ProcessIdentity {
                pid: 42,
                started_at: 100
            }
        );
        assert_eq!(
            second[0].identity,
            ProcessIdentity {
                pid: 42,
                started_at: 100
            }
        );
    }

    #[test]
    fn captured_identity_refuses_pid_reuse_after_a_matching_snapshot() {
        let registry = ProcessAttributionRegistry::new();
        let _registration = register(&registry, 42, 100, "external/provider");
        let now = Instant::now();

        let _ = registry.bind_and_snapshot(&[row(42, 1, 100)], now);
        let claims = registry.bind_and_snapshot(&[row(42, 1, 200)], now);

        assert!(claims.is_empty());
    }

    #[test]
    fn dropping_registration_unregisters_immediately() {
        let registry = ProcessAttributionRegistry::new();
        let registration = register(&registry, 42, 100, "external/provider");
        drop(registration);

        assert!(
            registry
                .bind_and_snapshot(&[row(42, 1, 100)], Instant::now())
                .is_empty()
        );
    }

    #[test]
    fn explicit_unregister_is_idempotent() {
        let registry = ProcessAttributionRegistry::new();
        let registration = register(&registry, 42, 100, "external/provider");

        registration.unregister();
        registration.unregister();

        assert!(
            registry
                .bind_and_snapshot(&[row(42, 1, 100)], Instant::now())
                .is_empty()
        );
    }

    #[test]
    fn missing_captured_identity_is_suppressed_until_its_owner_drops() {
        let registry = ProcessAttributionRegistry::new();
        let _registration = register(&registry, 42, 100, "external/provider");
        let now = Instant::now();
        let _ = registry.bind_and_snapshot(&[row(42, 1, 100)], now);

        assert!(registry.bind_and_snapshot(&[], now).is_empty());
        assert!(
            registry
                .bind_and_snapshot(&[row(42, 1, 100)], now)
                .is_empty()
        );
        assert_eq!(
            registry.freeze_and_snapshot_identities(),
            [identity(42, 100)]
        );
    }

    #[test]
    fn capacity_does_not_evict_a_still_owned_registration() {
        let registry = ProcessAttributionRegistry::new();
        let registrations = (0..512)
            .map(|pid| register(&registry, pid, 100, "external/provider"))
            .collect::<Vec<_>>();

        assert!(matches!(
            registry.register_identity(identity(999, 100), metadata("overflow")),
            Err(ProcessRegistrationError::Capacity)
        ));
        assert_eq!(
            registry.bind_and_snapshot(&[row(0, 1, 100)], Instant::now()),
            vec![crate::diagnostics::ProcessClaim {
                identity: ProcessIdentity {
                    pid: 0,
                    started_at: 100
                },
                scope: AttributionScope::External,
                kind: AttributionKind::Provider,
                label: "external/provider".to_owned(),
            }]
        );
        assert_eq!(registrations.len(), 512);
    }

    #[test]
    fn labels_are_normalized_and_bounded_to_eighty_utf8_characters() {
        let registry = ProcessAttributionRegistry::new();
        let _registration = register(
            &registry,
            42,
            100,
            &format!("  provider\n{}  ", "é".repeat(100)),
        );

        let claims = registry.bind_and_snapshot(&[row(42, 1, 100)], Instant::now());

        assert_eq!(claims[0].label, format!("provider {}", "é".repeat(71)));
        assert_eq!(claims[0].label.chars().count(), 80);
    }

    #[test]
    fn oversized_labels_stop_before_allocating_or_emitting_a_trailing_separator() {
        let registry = ProcessAttributionRegistry::new();
        let oversized_label = format!("{}{}", "x ".repeat(500_000), "é".repeat(500_000));
        let _registration = register(&registry, 42, 100, &oversized_label);

        let claims = registry.bind_and_snapshot(&[row(42, 1, 100)], Instant::now());

        assert_eq!(claims[0].label, format!("{}x", "x ".repeat(39)));
        assert_eq!(claims[0].label.chars().count(), 79);
    }

    #[test]
    fn wraparound_allocates_unused_ids_without_replacing_entries_or_reordering_snapshot() {
        let registry = ProcessAttributionRegistry::new();
        lock_state(registry.inner.as_ref()).next_registration_id = u64::MAX;
        let _first = register(&registry, 1, 10, "first");
        let _second = register(&registry, 2, 20, "second");
        lock_state(registry.inner.as_ref()).next_registration_id = u64::MAX;
        let _third = register(&registry, 3, 30, "third");

        let claims = registry.bind_and_snapshot(
            &[row(1, 0, 10), row(2, 0, 20), row(3, 0, 30)],
            Instant::now(),
        );

        assert_eq!(
            claims
                .iter()
                .map(|claim| claim.label.as_str())
                .collect::<Vec<_>>(),
            ["first", "second", "third"]
        );
    }

    #[test]
    fn snapshots_match_exact_identity_and_preserve_registration_order() {
        let registry = ProcessAttributionRegistry::new();
        let _first = register(&registry, 2, 20, "first");
        let _second = register(&registry, 1, 10, "second");

        let claims = registry.bind_and_snapshot(
            &[row(2, 0, 20), row(1, 0, 10), row(2, 0, 30)],
            Instant::now(),
        );

        assert_eq!(
            claims
                .iter()
                .map(|claim| (claim.label.as_str(), claim.identity))
                .collect::<Vec<_>>(),
            [
                (
                    "first",
                    ProcessIdentity {
                        pid: 2,
                        started_at: 20
                    }
                ),
                (
                    "second",
                    ProcessIdentity {
                        pid: 1,
                        started_at: 10
                    }
                ),
            ]
        );
    }

    #[test]
    fn shutdown_freeze_is_idempotent_and_rejects_late_roots() {
        let registry = ProcessAttributionRegistry::new();
        let _first = register(&registry, 2, 20, "first");
        let _second = register(&registry, 1, 10, "second");

        let first_snapshot = registry.freeze_and_snapshot_identities();
        let second_snapshot = registry.freeze_and_snapshot_identities();
        let late = registry.register_identity(identity(3, 30), metadata("late"));

        assert_eq!(
            first_snapshot,
            [identity(2, 20), identity(1, 10)],
            "freeze must preserve exact registration order"
        );
        assert_eq!(second_snapshot, first_snapshot);
        assert!(
            matches!(late, Err(ProcessRegistrationError::Shutdown)),
            "a frozen runtime cannot publish a late root"
        );
        assert_eq!(registry.freeze_and_snapshot_identities(), first_snapshot);
    }

    #[test]
    fn snapshots_are_owned_before_attribution_runs() {
        let registry = ProcessAttributionRegistry::new();
        let _registration = register(&registry, 42, 20, "external/provider");
        let rows = [row(1, 0, 10), row(42, 1, 20)];
        let claims = registry.bind_and_snapshot(&rows, Instant::now());

        let state_guard = lock_state(registry.inner.as_ref());
        let attribution = ResourceAttributor::attribute(
            &rows,
            ProcessIdentity {
                pid: 1,
                started_at: 10,
            },
            &claims,
            UiCoverage::default(),
        );
        drop(state_guard);

        assert_eq!(attribution.processes.len(), 2);
    }
}
