use std::collections::{BTreeMap, BTreeSet};
use std::time::{SystemTime, UNIX_EPOCH};

use coddy_studio_server::{
    activity::{
        ACTIVITY_DETAIL_MAX_LENGTH, ACTIVITY_ID_MAX_LENGTH, ACTIVITY_LABEL_MAX_LENGTH,
        ACTIVITY_PAGE_MAX_LENGTH, ACTIVITY_SUMMARY_MAX_LENGTH, ActivityActorSummary,
        ActivityCapabilities, ActivityEntry, ActivityEntryKind, ActivityEntryTone,
        ActivityLifecycle, ActivityRecordKind, ActivityRepository, ActivityRepositoryError,
        ActivityRosterBucket, ActivityScopeSeed, ActivitySection, ActivityWorkItemSummary,
        ProviderActivityMutation,
    },
    persistence::{Database, run_migrations},
    provider::{
        claude::{
            ClaudeActivityFixtureAdapter, ClaudeActivityInputSource, ClaudeTranscriptFixtureAdapter,
        },
        codex::CodexActivityFixtureAdapter,
        opencode::OpenCodeActivityFixtureAdapter,
    },
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use time::{Duration as TimeDuration, OffsetDateTime, format_description::well_known::Rfc3339};

const CANONICAL_SCENARIOS: &str =
    include_str!("fixtures/activity-conformance/canonical-scenarios.json");

#[derive(Clone, Copy, Debug)]
enum Provider {
    Codex,
    Claude,
    OpenCode,
}

impl Provider {
    fn name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::OpenCode => "opencode",
        }
    }
}

fn recent_fixture_timestamp_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock follows the Unix epoch")
            .as_millis(),
    )
    .expect("current Unix timestamp fits in u64")
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CanonicalScenario {
    name: String,
    group: String,
    revision_semantics: RevisionSemantics,
    #[serde(default)]
    phases: Vec<String>,
    #[serde(default)]
    allowed_lifecycle: Vec<String>,
    expected: Option<CanonicalExpected>,
    #[serde(default)]
    details: Value,
}

impl CanonicalScenario {
    fn expected(&self) -> &CanonicalExpected {
        self.expected
            .as_ref()
            .unwrap_or_else(|| panic!("{} expected semantic result", self.name))
    }

    fn decode_details<T: DeserializeOwned>(&self) -> T {
        serde_json::from_value(self.details.clone())
            .unwrap_or_else(|error| panic!("{} details: {error}", self.name))
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RevisionSemantics {
    initial_revision: u64,
    final_revision: String,
    effective_mutation_batches_only: bool,
    duplicate_or_rejected_events_advance: bool,
    assertion_stage: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CanonicalExpected {
    actors: Vec<SemanticActor>,
    work_items: Vec<SemanticWorkItem>,
    entries: Vec<SemanticEntry>,
    counts: SemanticCounts,
    required_lifecycle: Vec<String>,
    observed_lifecycle: ProviderLifecycle,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
struct SemanticActor {
    alias: String,
    parent_alias: Option<String>,
    status: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
struct SemanticWorkItem {
    alias: String,
    owner_alias: Option<String>,
    status: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
struct SemanticEntry {
    owner_alias: String,
    kind: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
struct SemanticCounts {
    active: usize,
    done: usize,
}

#[derive(Debug, Deserialize)]
struct ProviderLifecycle {
    codex: Vec<String>,
    claude: Vec<String>,
    opencode: Vec<String>,
}

impl ProviderLifecycle {
    fn for_provider(&self, provider: Provider) -> &[String] {
        match provider {
            Provider::Codex => &self.codex,
            Provider::Claude => &self.claude,
            Provider::OpenCode => &self.opencode,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
struct SemanticTopologyActor {
    alias: String,
    parent_alias: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NestedDetails {
    actors: Vec<SemanticTopologyActor>,
    status_by_provider: ProviderString,
}

#[derive(Debug, Deserialize)]
struct ProviderString {
    codex: String,
    claude: String,
    opencode: String,
}

impl ProviderString {
    fn for_provider(&self, provider: Provider) -> &str {
        match provider {
            Provider::Codex => &self.codex,
            Provider::Claude => &self.claude,
            Provider::OpenCode => &self.opencode,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StateDetails {
    states: Vec<String>,
    expected_by_provider: ProviderStateMap,
}

#[derive(Debug, Deserialize)]
struct ProviderStateMap {
    codex: BTreeMap<String, Option<String>>,
    claude: BTreeMap<String, Option<String>>,
    opencode: BTreeMap<String, Option<String>>,
}

impl ProviderStateMap {
    fn for_provider(&self, provider: Provider) -> &BTreeMap<String, Option<String>> {
        match provider {
            Provider::Codex => &self.codex,
            Provider::Claude => &self.claude,
            Provider::OpenCode => &self.opencode,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct EntryDetails {
    canonical_kinds: Vec<String>,
    expected_by_provider: ProviderEntryCapabilities,
}

#[derive(Debug, Deserialize)]
struct ProviderEntryCapabilities {
    codex: EntryCapabilities,
    claude: EntryCapabilities,
    opencode: EntryCapabilities,
}

impl ProviderEntryCapabilities {
    fn for_provider(&self, provider: Provider) -> &EntryCapabilities {
        match provider {
            Provider::Codex => &self.codex,
            Provider::Claude => &self.claude,
            Provider::OpenCode => &self.opencode,
        }
    }
}

#[derive(Debug, Deserialize)]
struct EntryCapabilities {
    supported: Vec<String>,
    unsupported: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
struct SuppressionOutcome {
    duplicate_suppressed: bool,
    late_progress_suppressed: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SuppressionDetails {
    expected_by_provider: ProviderSuppression,
}

#[derive(Debug, Deserialize)]
struct ProviderSuppression {
    codex: SuppressionOutcome,
    claude: SuppressionOutcome,
    opencode: SuppressionOutcome,
}

impl ProviderSuppression {
    fn for_provider(&self, provider: Provider) -> SuppressionOutcome {
        match provider {
            Provider::Codex => self.codex,
            Provider::Claude => self.claude,
            Provider::OpenCode => self.opencode,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
struct RecoveryOutcome {
    actor_status_repair_supported: bool,
    history_recovery: String,
    final_status: Option<String>,
    recovered_entry_kinds: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecoveryDetails {
    expected_by_provider: ProviderRecovery,
}

#[derive(Debug, Deserialize)]
struct ProviderRecovery {
    codex: RecoveryOutcome,
    claude: RecoveryOutcome,
    opencode: RecoveryOutcome,
}

impl ProviderRecovery {
    fn for_provider(&self, provider: Provider) -> &RecoveryOutcome {
        match provider {
            Provider::Codex => &self.codex,
            Provider::Claude => &self.claude,
            Provider::OpenCode => &self.opencode,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
enum OversizedIdentityOutcome {
    Rejected,
    BoundedNormalized,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
struct NativeInputOutcome {
    valid_control_emitted: bool,
    malformed_rejected: bool,
    oversized_identity: OversizedIdentityOutcome,
    oversized_display_fields_bounded: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct NativeInputDetails {
    expected_by_provider: ProviderNativeInput,
}

#[derive(Debug, Deserialize)]
struct ProviderNativeInput {
    codex: NativeInputOutcome,
    claude: NativeInputOutcome,
    opencode: NativeInputOutcome,
}

impl ProviderNativeInput {
    fn for_provider(&self, provider: Provider) -> NativeInputOutcome {
        match provider {
            Provider::Codex => self.codex,
            Provider::Claude => self.claude,
            Provider::OpenCode => self.opencode,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
struct ExpectedSectionCounts {
    active: u64,
    done: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
struct ExpectedSummaryCounts {
    subagents: ExpectedSectionCounts,
    background_tasks: ExpectedSectionCounts,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecoveryLossDetails {
    expected_interrupted_records: usize,
    actor_status: String,
    work_item_status: String,
    counts: ExpectedSummaryCounts,
}

#[derive(Debug)]
struct NativeMutationBatch {
    event_key: String,
    updated_at: String,
    mutations: Vec<ProviderActivityMutation>,
}

#[derive(Debug, Default)]
struct ProviderTrace {
    aliases: BTreeMap<String, String>,
    observed: ObservedActivity,
    batches: Vec<NativeMutationBatch>,
}

impl ProviderTrace {
    fn new(aliases: BTreeMap<String, String>) -> Self {
        Self {
            aliases,
            ..Self::default()
        }
    }

    fn record(
        &mut self,
        event_key: impl Into<String>,
        updated_at: impl Into<String>,
        mutations: Vec<ProviderActivityMutation>,
    ) {
        self.observed.apply(&self.aliases, mutations.clone());
        self.batches.push(NativeMutationBatch {
            event_key: event_key.into(),
            updated_at: updated_at.into(),
            mutations,
        });
    }

    fn record_without_semantics(
        &mut self,
        event_key: impl Into<String>,
        updated_at: impl Into<String>,
        mutations: Vec<ProviderActivityMutation>,
    ) {
        self.batches.push(NativeMutationBatch {
            event_key: event_key.into(),
            updated_at: updated_at.into(),
            mutations,
        });
    }
}

#[derive(Debug)]
struct Driven<T> {
    value: T,
    trace: ProviderTrace,
}

#[derive(Debug, Default)]
struct ObservedActivity {
    actors: BTreeMap<String, SemanticActor>,
    work_items: Vec<SemanticWorkItem>,
    entries: Vec<SemanticEntry>,
    lifecycle: Vec<String>,
}

impl ObservedActivity {
    fn apply(
        &mut self,
        aliases: &BTreeMap<String, String>,
        mutations: Vec<ProviderActivityMutation>,
    ) {
        for mutation in mutations {
            match mutation {
                ProviderActivityMutation::UpsertActor(actor) => {
                    let alias = aliases
                        .get(&actor.id)
                        .unwrap_or_else(|| panic!("unmapped actor id {}", actor.id))
                        .clone();
                    let parent_alias = actor.parent_actor_id.as_ref().map(|parent_id| {
                        aliases
                            .get(parent_id)
                            .unwrap_or_else(|| panic!("unmapped parent actor id {parent_id}"))
                            .clone()
                    });
                    let status = actor.status.as_str().to_owned();
                    self.lifecycle.push(status.clone());
                    self.actors.insert(
                        alias.clone(),
                        SemanticActor {
                            alias,
                            parent_alias,
                            status,
                        },
                    );
                }
                ProviderActivityMutation::UpsertWorkItem(work_item) => {
                    self.work_items.push(SemanticWorkItem {
                        alias: work_item.id,
                        owner_alias: work_item
                            .owner_actor_id
                            .as_ref()
                            .and_then(|owner_id| aliases.get(owner_id))
                            .cloned(),
                        status: work_item.status.as_str().to_owned(),
                    });
                }
                ProviderActivityMutation::AppendEntry(entry) => {
                    self.entries.push(SemanticEntry {
                        owner_alias: aliases
                            .get(&entry.owner_id)
                            .cloned()
                            .unwrap_or(entry.owner_id),
                        kind: format!("{:?}", entry.kind).to_ascii_lowercase(),
                    });
                }
                _ => {}
            }
        }
    }

    fn counts(&self) -> SemanticCounts {
        let done = self
            .actors
            .values()
            .filter(|actor| {
                matches!(
                    actor.status.as_str(),
                    "completed" | "failed" | "cancelled" | "interrupted"
                )
            })
            .count();
        SemanticCounts {
            active: self.actors.len() - done,
            done,
        }
    }
}

#[tokio::test]
async fn direct_child_lifecycle_is_semantically_conformant_across_providers() {
    let scenarios: Vec<CanonicalScenario> =
        serde_json::from_str(CANONICAL_SCENARIOS).expect("canonical activity scenarios");
    let scenario = scenario(&scenarios, "direct-child-lifecycle");
    let expected = scenario.expected();

    for provider in [Provider::Codex, Provider::Claude, Provider::OpenCode] {
        let trace = drive_scenario(provider, scenario);
        assert_trace_repository_semantics(provider, scenario, &trace).await;
        let observed = &trace.observed;
        let mut expected_actors = expected.actors.clone();
        expected_actors.sort_by(|left, right| left.alias.cmp(&right.alias));
        assert_eq!(
            observed.actors.values().cloned().collect::<Vec<_>>(),
            expected_actors,
            "{} final actor graph",
            provider.name()
        );
        assert_eq!(
            observed.work_items,
            expected.work_items,
            "{} work items",
            provider.name()
        );
        assert_eq!(
            observed.entries,
            expected.entries,
            "{} entries",
            provider.name()
        );
        assert_eq!(
            observed.counts(),
            expected.counts,
            "{} counts",
            provider.name()
        );
        assert_eq!(
            observed.lifecycle,
            expected.observed_lifecycle.for_provider(provider),
            "{} truthful lifecycle trace",
            provider.name()
        );
        let allowed = scenario
            .allowed_lifecycle
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>();
        assert!(
            observed
                .lifecycle
                .iter()
                .all(|status| allowed.contains(status.as_str())),
            "{} emitted a lifecycle outside the canonical set",
            provider.name()
        );
        for required in &expected.required_lifecycle {
            assert!(
                observed.lifecycle.contains(required),
                "{} omitted required {required} checkpoint",
                provider.name()
            );
        }
    }
}

#[test]
fn checkpoint_two_canonical_scenario_groups_are_present() {
    let scenarios = canonical_scenarios();
    let names = scenarios
        .iter()
        .map(|scenario| scenario.name.as_str())
        .collect::<Vec<_>>();
    assert!(names.len() >= 6);
    assert_eq!(
        &names[..6],
        [
            "direct-child-lifecycle",
            "nested-child-parent",
            "terminal-state-truthfulness",
            "entry-kind-capabilities",
            "duplicate-and-late-terminal",
            "authoritative-history-repair",
        ]
    );
}

#[test]
fn checkpoint_three_canonical_scenario_groups_are_present() {
    let scenarios = canonical_scenarios();
    assert_eq!(
        scenarios
            .iter()
            .map(|scenario| scenario.name.as_str())
            .collect::<Vec<_>>(),
        [
            "direct-child-lifecycle",
            "nested-child-parent",
            "terminal-state-truthfulness",
            "entry-kind-capabilities",
            "duplicate-and-late-terminal",
            "authoritative-history-repair",
            "malformed-and-oversized-native-input",
            "recovery-loss-interrupts-active",
        ]
    );
}

#[tokio::test]
async fn every_canonical_scenario_has_a_repository_checkpoint() {
    let scenarios = canonical_scenarios();
    let mut implemented = Vec::new();
    for scenario in &scenarios {
        match scenario.name.as_str() {
            "direct-child-lifecycle" => {
                for provider in [Provider::Codex, Provider::Claude, Provider::OpenCode] {
                    let trace = drive_scenario(provider, scenario);
                    assert_trace_repository_semantics(provider, scenario, &trace).await;
                }
            }
            "nested-child-parent" => {
                for provider in [Provider::Codex, Provider::Claude, Provider::OpenCode] {
                    let trace = drive_nested(provider);
                    assert_trace_repository_semantics(provider, scenario, &trace).await;
                }
            }
            "terminal-state-truthfulness" => {
                let details: StateDetails = scenario.decode_details();
                for provider in [Provider::Codex, Provider::Claude, Provider::OpenCode] {
                    let driven = drive_terminal_states(provider, &details.states);
                    assert_trace_repository_semantics(provider, scenario, &driven.trace).await;
                }
            }
            "entry-kind-capabilities" => {
                for provider in [Provider::Codex, Provider::Claude, Provider::OpenCode] {
                    let driven = drive_entry_kinds(provider);
                    assert_trace_repository_semantics(provider, scenario, &driven.trace).await;
                }
            }
            "duplicate-and-late-terminal" => {
                for provider in [Provider::Codex, Provider::Claude, Provider::OpenCode] {
                    let driven = drive_duplicate_and_late(provider);
                    assert_trace_repository_semantics(provider, scenario, &driven.trace).await;
                }
            }
            "authoritative-history-repair" => {
                for provider in [Provider::Codex, Provider::Claude, Provider::OpenCode] {
                    let driven = drive_history_repair(provider);
                    assert_trace_repository_semantics(provider, scenario, &driven.trace).await;
                }
            }
            "malformed-and-oversized-native-input" => {
                for provider in [Provider::Codex, Provider::Claude, Provider::OpenCode] {
                    let driven = drive_native_input_bounds(provider);
                    assert_trace_repository_semantics(provider, scenario, &driven.trace).await;
                }
            }
            "recovery-loss-interrupts-active" => {
                assert_recovery_loss_repository_semantics(scenario).await;
            }
            other => panic!("canonical scenario lacks repository checkpoint: {other}"),
        }
        implemented.push(scenario.name.as_str());
    }
    assert_eq!(
        scenarios
            .iter()
            .map(|scenario| scenario.name.as_str())
            .collect::<Vec<_>>(),
        implemented
    );
}

#[test]
fn every_scenario_declares_deferred_effective_mutation_revision_semantics() {
    for scenario in canonical_scenarios() {
        assert_eq!(
            scenario.revision_semantics.initial_revision, 0,
            "{}",
            scenario.name
        );
        assert_eq!(
            scenario.revision_semantics.final_revision, "initialPlusEffectiveMutationBatches",
            "{}",
            scenario.name
        );
        assert!(
            scenario.revision_semantics.effective_mutation_batches_only,
            "{}",
            scenario.name
        );
        assert!(
            !scenario
                .revision_semantics
                .duplicate_or_rejected_events_advance,
            "{}",
            scenario.name
        );
        assert_eq!(
            scenario.revision_semantics.assertion_stage, "repository-checkpoint-3",
            "{}",
            scenario.name
        );
        assert!(!scenario.group.is_empty(), "{}", scenario.name);
    }
}

#[test]
fn nested_children_preserve_the_same_normalized_parent_graph() {
    let scenarios = canonical_scenarios();
    let scenario = scenario(&scenarios, "nested-child-parent");
    let details: NestedDetails = scenario.decode_details();
    for provider in [Provider::Codex, Provider::Claude, Provider::OpenCode] {
        let trace = drive_nested(provider);
        let topology = trace
            .observed
            .actors
            .values()
            .map(|actor| SemanticTopologyActor {
                alias: actor.alias.clone(),
                parent_alias: actor.parent_alias.clone(),
            })
            .collect::<Vec<_>>();
        let mut expected_topology = details.actors.clone();
        expected_topology.sort_by(|left, right| left.alias.cmp(&right.alias));
        assert_eq!(topology, expected_topology, "{} topology", provider.name());
        assert!(
            trace
                .observed
                .actors
                .values()
                .all(|actor| actor.status == details.status_by_provider.for_provider(provider)),
            "{} truthful nested status",
            provider.name()
        );
    }
}

#[test]
fn terminal_state_capabilities_are_truthful_without_fabricated_mappings() {
    let scenarios = canonical_scenarios();
    let scenario = scenario(&scenarios, "terminal-state-truthfulness");
    let details: StateDetails = scenario.decode_details();
    for provider in [Provider::Codex, Provider::Claude, Provider::OpenCode] {
        assert_eq!(
            drive_terminal_states(provider, &details.states).value,
            *details.expected_by_provider.for_provider(provider),
            "{} terminal state truthfulness",
            provider.name()
        );
    }
}

#[test]
fn entry_kind_capabilities_are_exhaustive_and_provider_truthful() {
    let scenarios = canonical_scenarios();
    let scenario = scenario(&scenarios, "entry-kind-capabilities");
    let details: EntryDetails = scenario.decode_details();
    let canonical = details
        .canonical_kinds
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    for provider in [Provider::Codex, Provider::Claude, Provider::OpenCode] {
        let expected = details.expected_by_provider.for_provider(provider);
        let supported = expected.supported.iter().cloned().collect::<BTreeSet<_>>();
        let unsupported = expected
            .unsupported
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            supported
                .union(&unsupported)
                .cloned()
                .collect::<BTreeSet<_>>(),
            canonical,
            "{} capability partition",
            provider.name()
        );
        assert!(
            supported.is_disjoint(&unsupported),
            "{} capability overlap",
            provider.name()
        );
        assert_eq!(
            drive_entry_kinds(provider).value,
            supported,
            "{} emitted entry kinds",
            provider.name()
        );
    }
}

#[test]
fn duplicate_delivery_and_late_progress_do_not_mutate_terminal_children() {
    let scenarios = canonical_scenarios();
    let scenario = scenario(&scenarios, "duplicate-and-late-terminal");
    let details: SuppressionDetails = scenario.decode_details();
    for provider in [Provider::Codex, Provider::Claude, Provider::OpenCode] {
        assert_eq!(
            drive_duplicate_and_late(provider).value,
            details.expected_by_provider.for_provider(provider),
            "{} suppression",
            provider.name()
        );
    }
}

#[test]
fn authoritative_history_recovery_is_truthful_about_actor_and_entry_repair() {
    let scenarios = canonical_scenarios();
    let scenario = scenario(&scenarios, "authoritative-history-repair");
    let details: RecoveryDetails = scenario.decode_details();
    for provider in [Provider::Codex, Provider::Claude, Provider::OpenCode] {
        assert_eq!(
            drive_history_repair(provider).value,
            *details.expected_by_provider.for_provider(provider),
            "{} history recovery",
            provider.name()
        );
    }
}

#[test]
fn malformed_native_inputs_are_rejected_and_oversized_inputs_are_handled_safely() {
    let scenarios = canonical_scenarios();
    let scenario = scenario(&scenarios, "malformed-and-oversized-native-input");
    let details: NativeInputDetails = scenario.decode_details();
    for provider in [Provider::Codex, Provider::Claude, Provider::OpenCode] {
        assert_eq!(
            drive_native_input_bounds(provider).value,
            details.expected_by_provider.for_provider(provider),
            "{} native input bounds",
            provider.name()
        );
    }
}

#[tokio::test]
async fn repository_graph_invariants_hold_for_bounded_deterministic_permutations() {
    let timeline_start = OffsetDateTime::now_utc() - TimeDuration::minutes(10);
    for seed in [0xA11CE_u64, 0xC0DE_u64, 0x5EED_u64] {
        let database = Database::open_in_memory()
            .await
            .unwrap_or_else(|error| panic!("seed={seed:#x} database: {error}"));
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .unwrap_or_else(|error| panic!("seed={seed:#x} migrations: {error}"));
        let repository = ActivityRepository::new(database);
        let mut ids_by_scope = Vec::new();
        let mut scoped_identities = BTreeSet::new();

        for scope_index in 0..2 {
            let scope_id = format!("thread:permutation:{seed:x}:{scope_index}");
            let thread_id = format!("permutation-{seed:x}-{scope_index}");
            let scope = ActivityScopeSeed::thread(
                &scope_id,
                &thread_id,
                "codex",
                Some("codex"),
                ActivityCapabilities::structured_full(false),
            )
            .unwrap_or_else(|error| {
                panic!("seed={seed:#x} construct scope {scope_index}: {error}")
            });
            repository
                .ensure_scope(scope.clone())
                .await
                .unwrap_or_else(|error| panic!("seed={seed:#x} ensure scope: {error}"));

            let mut pending = deterministic_permutation(8, seed ^ scope_index);
            let mut completed = BTreeSet::new();
            let mut effective_batches = 0_u64;
            while !pending.is_empty() {
                let position = pending
                    .iter()
                    .position(|operation| {
                        permutation_dependencies(*operation, seed)
                            .iter()
                            .all(|dependency| completed.contains(dependency))
                    })
                    .unwrap_or_else(|| panic!("seed={seed:#x} dependency scheduler deadlocked"));
                let operation = pending.remove(position);
                let (event_key, updated_at, mutations, outcome) =
                    permutation_operation(operation, seed, timeline_start);
                let before_revision = repository
                    .snapshot(&scope.scope)
                    .await
                    .unwrap_or_else(|error| {
                        panic!("seed={seed:#x} snapshot before operation {operation}: {error}")
                    })
                    .revision;
                let result = repository
                    .apply_batch(&scope_id, event_key, mutations, &updated_at)
                    .await;
                match outcome {
                    PermutationOutcome::Effective => {
                        let deltas = result.unwrap_or_else(|error| {
                            panic!("seed={seed:#x} effective operation {operation}: {error}")
                        });
                        assert!(
                            !deltas.is_empty(),
                            "seed={seed:#x} operation {operation} was unexpectedly a no-op"
                        );
                        effective_batches += 1;
                        assert_eq!(
                            repository
                                .snapshot(&scope.scope)
                                .await
                                .unwrap_or_else(|error| {
                                    panic!(
                                        "seed={seed:#x} effective snapshot after operation \
                                         {operation}: {error}"
                                    )
                                })
                                .revision,
                            before_revision + 1,
                            "seed={seed:#x} operation {operation} revision"
                        );
                    }
                    PermutationOutcome::Suppressed => {
                        assert!(
                            result
                                .unwrap_or_else(|error| {
                                    panic!(
                                        "seed={seed:#x} suppressed operation {operation}: {error}"
                                    )
                                })
                                .is_empty(),
                            "seed={seed:#x} operation {operation} was not suppressed"
                        );
                        assert_eq!(
                            repository
                                .snapshot(&scope.scope)
                                .await
                                .unwrap_or_else(|error| {
                                    panic!(
                                        "seed={seed:#x} suppressed snapshot after operation \
                                         {operation}: {error}"
                                    )
                                })
                                .revision,
                            before_revision,
                            "seed={seed:#x} suppressed operation {operation} advanced revision"
                        );
                    }
                    PermutationOutcome::Rejected => {
                        assert!(
                            matches!(result, Err(ActivityRepositoryError::InvalidReference(_))),
                            "seed={seed:#x} operation {operation} was not rejected: {result:?}"
                        );
                        assert_eq!(
                            repository
                                .snapshot(&scope.scope)
                                .await
                                .unwrap_or_else(|error| {
                                    panic!(
                                        "seed={seed:#x} rejected snapshot after operation \
                                         {operation}: {error}"
                                    )
                                })
                                .revision,
                            before_revision,
                            "seed={seed:#x} rejected operation {operation} advanced revision"
                        );
                    }
                }
                completed.insert(operation);
            }

            let snapshot = repository
                .snapshot(&scope.scope)
                .await
                .unwrap_or_else(|error| panic!("seed={seed:#x} final snapshot: {error}"));
            assert_eq!(effective_batches, 5, "seed={seed:#x}");
            assert_eq!(snapshot.revision, effective_batches, "seed={seed:#x}");
            assert_eq!(
                snapshot
                    .actors
                    .iter()
                    .find(|actor| actor.id == "actor:shared:child")
                    .unwrap_or_else(|| panic!("seed={seed:#x} missing terminal actor"))
                    .status,
                ActivityLifecycle::Completed,
                "seed={seed:#x}"
            );
            assert_eq!(
                snapshot
                    .actors
                    .iter()
                    .find(|actor| actor.id == "actor:shared:child")
                    .unwrap_or_else(|| panic!("seed={seed:#x} missing nested child"))
                    .parent_actor_id
                    .as_deref(),
                Some("actor:shared:parent"),
                "seed={seed:#x}"
            );
            assert_eq!(
                snapshot
                    .actors
                    .iter()
                    .find(|actor| actor.id == "actor:shared:parent")
                    .unwrap_or_else(|| panic!("seed={seed:#x} missing parent"))
                    .parent_actor_id
                    .as_deref(),
                Some("actor:shared:root"),
                "seed={seed:#x}"
            );
            assert_eq!(snapshot.work_items.len(), 1, "seed={seed:#x}");
            assert_eq!(
                snapshot.work_items[0].owner_actor_id.as_deref(),
                Some("actor:shared:child"),
                "seed={seed:#x}"
            );
            assert_eq!(snapshot.counts.subagents.active, 2, "seed={seed:#x}");
            assert_eq!(snapshot.counts.subagents.done, 1, "seed={seed:#x}");
            assert_eq!(snapshot.counts.background_tasks.active, 1, "seed={seed:#x}");
            let ids = snapshot
                .actors
                .iter()
                .map(|actor| actor.id.clone())
                .collect::<BTreeSet<_>>();
            assert_eq!(ids.len(), 3, "seed={seed:#x} duplicate actor ids");
            for id in &ids {
                assert!(
                    scoped_identities.insert((scope_id.clone(), id.clone())),
                    "seed={seed:#x} duplicate scoped identity {scope_id}/{id}"
                );
            }
            ids_by_scope.push(ids);
        }

        assert_eq!(
            ids_by_scope[0], ids_by_scope[1],
            "seed={seed:#x} same provider-native ids were not isolated by scope"
        );
        assert_eq!(
            scoped_identities.len(),
            6,
            "seed={seed:#x} scoped identity namespace"
        );
    }
}

#[tokio::test]
async fn repository_rejects_cross_scope_parents_cycles_and_missing_owners_transactionally() {
    let database = migrated_database().await;
    let repository = ActivityRepository::new(database);
    let first = thread_scope("thread:graph:first", "graph-first");
    let second = thread_scope("thread:graph:second", "graph-second");
    repository
        .ensure_scope(first.clone())
        .await
        .expect("first scope");
    repository
        .ensure_scope(second.clone())
        .await
        .expect("second scope");

    repository
        .apply_batch(
            &second.scope_id,
            "event:foreign-parent",
            vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:graph:second:parent",
                    None,
                    "Foreign parent",
                    "running",
                )
                .expect("foreign parent"),
            ],
            "2026-07-22T12:00:00Z",
        )
        .await
        .expect("foreign parent insert");
    repository
        .apply_batch(
            &first.scope_id,
            "event:cycle-seed",
            vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:graph:first:a",
                    None,
                    "Actor A",
                    "running",
                )
                .expect("actor a"),
                ProviderActivityMutation::upsert_actor(
                    "actor:graph:first:b",
                    None,
                    "Actor B",
                    "running",
                )
                .expect("actor b"),
            ],
            "2026-07-22T12:00:00Z",
        )
        .await
        .expect("cycle seed");

    let revision_before = repository
        .snapshot(&first.scope)
        .await
        .expect("before invalid batches")
        .revision;
    let cross_scope = repository
        .apply_batch(
            &first.scope_id,
            "event:cross-scope-parent",
            vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:graph:first:child",
                    Some("actor:graph:second:parent"),
                    "Cross-scope child",
                    "running",
                )
                .expect("cross-scope child"),
            ],
            "2026-07-22T12:01:00Z",
        )
        .await;
    assert!(matches!(
        cross_scope,
        Err(ActivityRepositoryError::InvalidReference(_))
    ));

    let cycle = repository
        .apply_batch(
            &first.scope_id,
            "event:cycle",
            vec![
                ProviderActivityMutation::UpsertActor(
                    actor_summary(
                        "actor:graph:first:a",
                        Some("actor:graph:first:b"),
                        ActivityLifecycle::Running,
                        "2026-07-22T12:00:00Z",
                        "2026-07-22T12:02:00Z",
                        None,
                    )
                    .expect("cycle actor a"),
                ),
                ProviderActivityMutation::UpsertActor(
                    actor_summary(
                        "actor:graph:first:b",
                        Some("actor:graph:first:a"),
                        ActivityLifecycle::Running,
                        "2026-07-22T12:00:00Z",
                        "2026-07-22T12:02:00Z",
                        None,
                    )
                    .expect("cycle actor b"),
                ),
            ],
            "2026-07-22T12:02:00Z",
        )
        .await;
    assert!(matches!(
        cycle,
        Err(ActivityRepositoryError::InvalidReference(_))
    ));

    let missing_work_owner = repository
        .apply_batch(
            &first.scope_id,
            "event:missing-work-owner",
            vec![ProviderActivityMutation::UpsertWorkItem(
                work_item_summary(
                    "work:graph:first:missing",
                    Some("actor:graph:first:missing"),
                    ActivityLifecycle::Running,
                    "2026-07-22T12:00:00Z",
                    "2026-07-22T12:03:00Z",
                    None,
                )
                .expect("missing-owner work item"),
            )],
            "2026-07-22T12:03:00Z",
        )
        .await;
    assert!(matches!(
        missing_work_owner,
        Err(ActivityRepositoryError::InvalidReference(_))
    ));

    let missing_entry_owner = repository
        .apply_batch(
            &first.scope_id,
            "event:missing-entry-owner",
            vec![ProviderActivityMutation::AppendEntry(
                entry(
                    "entry:graph:first:missing",
                    ActivityRecordKind::Actor,
                    "actor:graph:first:missing",
                    "2026-07-22T12:04:00Z",
                )
                .expect("missing-owner entry"),
            )],
            "2026-07-22T12:04:00Z",
        )
        .await;
    assert!(matches!(
        missing_entry_owner,
        Err(ActivityRepositoryError::InvalidReference(_))
    ));

    let after = repository
        .snapshot(&first.scope)
        .await
        .expect("after invalid batches");
    assert_eq!(after.revision, revision_before);
    assert_eq!(after.actors.len(), 2);
    assert!(
        after
            .actors
            .iter()
            .all(|actor| actor.parent_actor_id.is_none())
    );
    assert!(after.work_items.is_empty());
}

#[tokio::test]
async fn repository_counts_are_exact_when_summary_and_roster_pages_are_capped() {
    let database = migrated_database().await;
    let repository = ActivityRepository::new(database);
    let scope = thread_scope("thread:capped-counts", "capped-counts");
    repository.ensure_scope(scope.clone()).await.expect("scope");
    let expected_count = ACTIVITY_PAGE_MAX_LENGTH + 5;
    let mutations = (0..expected_count)
        .map(|index| {
            ProviderActivityMutation::upsert_actor(
                format!("actor:capped:{index:03}"),
                None,
                format!("Actor {index:03}"),
                "running",
            )
            .expect("capped actor")
        })
        .collect();
    repository
        .apply_batch(
            &scope.scope_id,
            "event:capped-actors",
            mutations,
            "2026-07-22T12:00:00Z",
        )
        .await
        .expect("capped actors");

    let snapshot = repository.snapshot(&scope.scope).await.expect("snapshot");
    assert_eq!(snapshot.actors.len(), ACTIVITY_PAGE_MAX_LENGTH);
    assert!(snapshot.actors_has_more);
    assert_eq!(snapshot.counts.subagents.active, expected_count as u64);
    assert_eq!(snapshot.counts.subagents.done, 0);

    let roster = repository
        .list_roster(
            &scope.scope,
            &scope.scope_id,
            ActivitySection::Subagents,
            ActivityRosterBucket::Active,
            None,
            17,
        )
        .await
        .expect("roster");
    assert_eq!(roster.records.len(), 17);
    assert!(roster.next_cursor.is_some());
    assert_eq!(snapshot.counts.subagents.active, expected_count as u64);
}

#[tokio::test]
async fn recovery_loss_interrupts_active_repository_records() {
    let scenarios = canonical_scenarios();
    let scenario = scenario(&scenarios, "recovery-loss-interrupts-active");
    assert_recovery_loss_repository_semantics(scenario).await;
}

async fn assert_recovery_loss_repository_semantics(scenario: &CanonicalScenario) {
    let details: RecoveryLossDetails = scenario.decode_details();
    let database = migrated_database().await;
    let repository = ActivityRepository::new(database);
    let scope = terminal_scope(
        "terminal:recovery-loss",
        "generation:recovery-loss",
        "terminal:recovery-loss",
    );
    repository.ensure_scope(scope.clone()).await.expect("scope");
    let active_mutations = vec![
        ProviderActivityMutation::upsert_actor(
            "actor:recovery-loss",
            None,
            "Active actor",
            "running",
        )
        .expect("active actor"),
        ProviderActivityMutation::UpsertWorkItem(
            work_item_summary(
                "work:recovery-loss",
                Some("actor:recovery-loss"),
                ActivityLifecycle::Waiting,
                "2026-07-22T12:00:00Z",
                "2026-07-22T12:00:00Z",
                None,
            )
            .expect("active work item"),
        ),
    ];
    let seeded = repository
        .apply_batch(
            &scope.scope_id,
            "event:active-records",
            active_mutations.clone(),
            "2026-07-22T12:00:00Z",
        )
        .await
        .expect("active records");
    assert!(!seeded.is_empty());
    let seeded_snapshot = repository
        .snapshot(&scope.scope)
        .await
        .expect("seeded snapshot");
    assert_eq!(seeded_snapshot.revision, 1);

    let duplicate = repository
        .apply_batch(
            &scope.scope_id,
            "event:active-records",
            active_mutations,
            "2026-07-22T12:00:00Z",
        )
        .await
        .expect("duplicate active records");
    assert!(duplicate.is_empty());
    assert_eq!(
        repository
            .snapshot(&scope.scope)
            .await
            .expect("duplicate seed snapshot"),
        seeded_snapshot
    );

    let interrupted = repository
        .interrupt_unresolved_terminal_scopes()
        .await
        .expect("interrupt unresolved terminal scope");
    assert_eq!(interrupted, details.expected_interrupted_records);
    let snapshot = repository.snapshot(&scope.scope).await.expect("snapshot");
    assert_eq!(snapshot.revision, 2);
    assert_eq!(
        snapshot.actors[0].status.as_str(),
        details.actor_status.as_str()
    );
    assert_eq!(
        snapshot.work_items[0].status.as_str(),
        details.work_item_status.as_str()
    );
    assert_eq!(
        snapshot.counts.subagents.active,
        details.counts.subagents.active
    );
    assert_eq!(
        snapshot.counts.subagents.done,
        details.counts.subagents.done
    );
    assert_eq!(
        snapshot.counts.background_tasks.active,
        details.counts.background_tasks.active
    );
    assert_eq!(
        snapshot.counts.background_tasks.done,
        details.counts.background_tasks.done
    );

    let repeated = repository
        .interrupt_unresolved_terminal_scopes()
        .await
        .expect("repeat interrupt unresolved terminal scope");
    assert_eq!(repeated, 0);
    assert_eq!(
        repository
            .snapshot(&scope.scope)
            .await
            .expect("repeated interruption snapshot"),
        snapshot
    );
}

async fn assert_trace_repository_semantics(
    provider: Provider,
    scenario: &CanonicalScenario,
    trace: &ProviderTrace,
) {
    let database = migrated_database().await;
    let repository = ActivityRepository::new(database);
    let scope_id = format!("thread:trace:{}:{}", scenario.name, provider.name());
    let scope = ActivityScopeSeed::thread(
        &scope_id,
        format!("trace-{}", provider.name()),
        provider.name(),
        Some(provider.name()),
        ActivityCapabilities::structured_full(false),
    )
    .expect("valid provider trace scope");
    repository.ensure_scope(scope.clone()).await.expect("scope");
    let initial = repository.snapshot(&scope.scope).await.expect("initial");
    assert_eq!(
        initial.revision,
        scenario.revision_semantics.initial_revision,
        "{} initial repository revision",
        provider.name()
    );

    let mut expected_revision = scenario.revision_semantics.initial_revision;
    for batch in &trace.batches {
        if batch.mutations.is_empty() {
            assert_eq!(
                repository
                    .snapshot(&scope.scope)
                    .await
                    .expect("adapter-rejected snapshot")
                    .revision,
                expected_revision,
                "{} adapter-rejected batch {} changed revision",
                provider.name(),
                batch.event_key
            );
            continue;
        }
        let deltas = repository
            .apply_batch(
                &scope_id,
                &batch.event_key,
                batch.mutations.clone(),
                &batch.updated_at,
            )
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "{} repository batch {}: {error}",
                    provider.name(),
                    batch.event_key
                )
            });
        if deltas.is_empty() {
            assert_eq!(
                repository
                    .snapshot(&scope.scope)
                    .await
                    .expect("repository no-op snapshot")
                    .revision,
                expected_revision,
                "{} repository no-op batch {} advanced revision",
                provider.name(),
                batch.event_key
            );
        } else {
            expected_revision += 1;
            assert_eq!(
                deltas.first().expect("first delta").previous_revision,
                expected_revision - 1,
                "{} previous revision for {}",
                provider.name(),
                batch.event_key
            );
            assert_eq!(
                deltas.last().expect("last delta").revision,
                expected_revision,
                "{} effective revision for {}",
                provider.name(),
                batch.event_key
            );
        }
        assert_eq!(
            repository
                .snapshot(&scope.scope)
                .await
                .expect("effective snapshot")
                .revision,
            expected_revision,
            "{} snapshot revision for {}",
            provider.name(),
            batch.event_key
        );

        let duplicate = repository
            .apply_batch(
                &scope_id,
                &batch.event_key,
                batch.mutations.clone(),
                &batch.updated_at,
            )
            .await
            .expect("duplicate trace batch");
        assert!(
            duplicate.is_empty(),
            "{} duplicate batch {} emitted a delta",
            provider.name(),
            batch.event_key
        );
        assert_eq!(
            repository
                .snapshot(&scope.scope)
                .await
                .expect("duplicate snapshot")
                .revision,
            expected_revision,
            "{} duplicate batch {} advanced revision",
            provider.name(),
            batch.event_key
        );
    }

    let final_snapshot = repository.snapshot(&scope.scope).await.expect("final");
    assert_eq!(
        final_snapshot.revision,
        expected_revision,
        "{} final effective-batch revision",
        provider.name()
    );
    if !trace.aliases.is_empty() {
        let mut repository_actors = final_snapshot
            .actors
            .iter()
            .map(|actor| {
                let alias = trace
                    .aliases
                    .get(&actor.id)
                    .unwrap_or_else(|| {
                        panic!("{} unmapped persisted actor {}", provider.name(), actor.id)
                    })
                    .clone();
                SemanticActor {
                    alias,
                    parent_alias: actor.parent_actor_id.as_ref().map(|parent_id| {
                        trace
                            .aliases
                            .get(parent_id)
                            .unwrap_or_else(|| {
                                panic!("{} unmapped persisted parent {parent_id}", provider.name())
                            })
                            .clone()
                    }),
                    status: actor.status.as_str().to_owned(),
                }
            })
            .collect::<Vec<_>>();
        repository_actors.sort_by(|left, right| left.alias.cmp(&right.alias));
        assert_eq!(
            repository_actors,
            trace.observed.actors.values().cloned().collect::<Vec<_>>(),
            "{} persisted semantic graph",
            provider.name()
        );
        assert_eq!(
            final_snapshot.counts.subagents.active,
            trace.observed.counts().active as u64,
            "{} persisted active count",
            provider.name()
        );
        assert_eq!(
            final_snapshot.counts.subagents.done,
            trace.observed.counts().done as u64,
            "{} persisted done count",
            provider.name()
        );
        assert_eq!(
            final_snapshot
                .work_items
                .iter()
                .map(|work_item| SemanticWorkItem {
                    alias: work_item.id.clone(),
                    owner_alias: work_item
                        .owner_actor_id
                        .as_ref()
                        .and_then(|owner_id| trace.aliases.get(owner_id))
                        .cloned(),
                    status: work_item.status.as_str().to_owned(),
                })
                .collect::<Vec<_>>(),
            trace.observed.work_items,
            "{} persisted work items",
            provider.name()
        );
        for (actor_id, alias) in &trace.aliases {
            let expected_kinds = trace
                .observed
                .entries
                .iter()
                .filter(|entry| entry.owner_alias == *alias)
                .map(|entry| entry.kind.clone())
                .collect::<BTreeSet<_>>();
            if expected_kinds.is_empty() {
                continue;
            }
            let detail = repository
                .list_detail(
                    &scope.scope,
                    &scope_id,
                    ActivityRecordKind::Actor,
                    actor_id,
                    None,
                    200,
                )
                .await
                .unwrap_or_else(|error| {
                    panic!("{} persisted detail for {alias}: {error}", provider.name())
                });
            let persisted_kinds = detail
                .entries
                .iter()
                .map(|entry| format!("{:?}", entry.kind).to_ascii_lowercase())
                .collect::<BTreeSet<_>>();
            assert_eq!(
                persisted_kinds,
                expected_kinds,
                "{} persisted entry kinds for {alias}",
                provider.name()
            );
        }
    }
}

fn drive_scenario(provider: Provider, scenario: &CanonicalScenario) -> ProviderTrace {
    let child_alias = scenario
        .expected()
        .actors
        .first()
        .expect("direct child actor")
        .alias
        .as_str();
    match provider {
        Provider::Codex => drive_codex(&scenario.phases, child_alias),
        Provider::Claude => drive_claude(&scenario.phases, child_alias),
        Provider::OpenCode => drive_opencode(&scenario.phases, child_alias),
    }
}

fn drive_codex(phases: &[String], child_alias: &str) -> ProviderTrace {
    let observed_at_ms = recent_fixture_timestamp_ms();
    let root_id = "conformance-codex-root";
    let child_id = format!("conformance-codex-{child_alias}");
    let canonical_child_id = format!("codex:thread:{child_id}");
    let aliases = BTreeMap::from([(canonical_child_id, child_alias.to_owned())]);
    let mut adapter = CodexActivityFixtureAdapter::new(Some(root_id));
    let mut trace = ProviderTrace::new(aliases);
    for (index, phase) in phases.iter().enumerate() {
        let status = match phase.as_str() {
            "start" => "pendingInit",
            "running" => "running",
            "waiting" => "waiting",
            "completed" => "completed",
            other => panic!("unknown canonical phase {other}"),
        };
        let method = if phase == "start" {
            "item/started"
        } else {
            "item/completed"
        };
        let tool = if phase == "start" {
            "spawnAgent"
        } else {
            "wait"
        };
        let params = json!({
            "threadId": root_id,
            "turnId": "conformance-turn",
            "item": {
                "id": format!("conformance-{phase}"),
                "type": "collabAgentToolCall",
                "tool": tool,
                "status": if phase == "start" { "inProgress" } else { "completed" },
                "senderThreadId": root_id,
                "receiverThreadIds": [&child_id],
                "agentsStates": {
                    child_id.clone(): {
                        "status": status,
                        "message": if phase == "completed" { Value::String("done".to_owned()) } else { Value::Null }
                    }
                }
            },
            "completedAtMs": observed_at_ms + index as u64 * 1_000
        });
        trace.record(
            format!("codex:lifecycle:{index}:{phase}"),
            format!("2026-07-24T12:00:{index:02}Z"),
            adapter
                .handle_notification(method, &params, observed_at_ms + index as u64 * 1_000)
                .mutations,
        );
    }
    trace
}

fn drive_claude(phases: &[String], child_alias: &str) -> ProviderTrace {
    let observed_at_ms = recent_fixture_timestamp_ms();
    let root_id = "conformance-claude-root";
    let child_id = format!("conformance-claude-{child_alias}");
    let canonical_child_id = format!("claude:agent:{child_id}");
    let aliases = BTreeMap::from([(canonical_child_id, child_alias.to_owned())]);
    let mut adapter = ClaudeActivityFixtureAdapter::new(root_id);
    let mut trace = ProviderTrace::new(aliases);
    for (index, phase) in phases.iter().enumerate() {
        let event = match phase.as_str() {
            "start" => Some(json!({
                "hook_event_name": "SubagentStart",
                "session_id": root_id,
                "agent_id": child_id,
                "agent_type": "Explore"
            })),
            "running" | "waiting" => None,
            "completed" => Some(json!({
                "hook_event_name": "SubagentStop",
                "session_id": root_id,
                "agent_id": child_id,
                "agent_type": "Explore",
                "last_assistant_message": "done"
            })),
            other => panic!("unknown canonical phase {other}"),
        };
        if let Some(event) = event {
            trace.record(
                format!("claude:lifecycle:{index}:{phase}"),
                format!("2026-07-24T12:00:{index:02}Z"),
                adapter
                    .handle_value(
                        ClaudeActivityInputSource::HookInput,
                        &event,
                        observed_at_ms + index as u64 * 1_000,
                    )
                    .mutations,
            );
        }
    }
    trace
}

fn drive_opencode(phases: &[String], child_alias: &str) -> ProviderTrace {
    let observed_at_ms = recent_fixture_timestamp_ms();
    let root_id = "conformance-opencode-root";
    let child_id = format!("conformance-opencode-{child_alias}");
    let canonical_child_id = format!("opencode:session:{child_id}");
    let aliases = BTreeMap::from([(canonical_child_id, child_alias.to_owned())]);
    let mut adapter = OpenCodeActivityFixtureAdapter::new(root_id);
    let mut trace = ProviderTrace::new(aliases);
    for (index, phase) in phases.iter().enumerate() {
        let output = match phase.as_str() {
            "start" => adapter.reconcile_children(
                root_id,
                &json!([{
                    "id": child_id,
                    "parentID": root_id,
                    "title": "Direct child",
                    "time": { "created": observed_at_ms }
                }]),
            ),
            "running" => adapter.handle_event_at(
                &json!({
                    "id": "conformance-busy",
                    "type": "session.status",
                    "properties": { "sessionID": child_id, "status": { "type": "busy" } }
                }),
                observed_at_ms + index as u64 * 1_000,
            ),
            "waiting" => adapter.handle_event_at(
                &json!({
                    "id": "conformance-idle",
                    "type": "session.status",
                    "properties": { "sessionID": child_id, "status": { "type": "idle" } }
                }),
                observed_at_ms + index as u64 * 1_000,
            ),
            "completed" => adapter.handle_history(
                &child_id,
                &json!([{
                    "info": {
                        "id": "conformance-complete",
                        "sessionID": child_id,
                        "role": "assistant",
                        "time": { "completed": observed_at_ms + 3_000 },
                        "finish": "stop"
                    },
                    "parts": []
                }]),
            ),
            other => panic!("unknown canonical phase {other}"),
        };
        trace.record(
            format!("opencode:lifecycle:{index}:{phase}"),
            format!("2026-07-24T12:00:{index:02}Z"),
            output.mutations,
        );
    }
    trace
}

fn canonical_scenarios() -> Vec<CanonicalScenario> {
    serde_json::from_str(CANONICAL_SCENARIOS).expect("canonical activity scenarios")
}

fn scenario<'a>(scenarios: &'a [CanonicalScenario], name: &str) -> &'a CanonicalScenario {
    scenarios
        .iter()
        .find(|scenario| scenario.name == name)
        .unwrap_or_else(|| panic!("missing canonical scenario {name}"))
}

fn drive_nested(provider: Provider) -> ProviderTrace {
    match provider {
        Provider::Codex => {
            let root = "nested-codex-root";
            let parent = "nested-codex-parent";
            let child = "nested-codex-child";
            let aliases = BTreeMap::from([
                (format!("codex:thread:{parent}"), "parent".to_owned()),
                (format!("codex:thread:{child}"), "nested-child".to_owned()),
            ]);
            let mut adapter = CodexActivityFixtureAdapter::new(Some(root));
            let mut trace = ProviderTrace::new(aliases);
            for (index, (sender, receiver, item_id)) in [
                (root, parent, "spawn-parent"),
                (parent, child, "spawn-child"),
            ]
            .into_iter()
            .enumerate()
            {
                let output = adapter.handle_notification(
                    "item/started",
                    &json!({
                        "threadId": sender,
                        "turnId": format!("turn-{item_id}"),
                        "item": {
                            "id": item_id,
                            "type": "collabAgentToolCall",
                            "tool": "spawnAgent",
                            "status": "inProgress",
                            "senderThreadId": sender,
                            "receiverThreadIds": [receiver],
                            "agentsStates": {
                                receiver: { "status": "running", "message": null }
                            }
                        }
                    }),
                    1_000,
                );
                trace.record(
                    format!("codex:nested:{index}:{item_id}"),
                    format!("2026-07-24T12:01:{index:02}Z"),
                    output.mutations,
                );
            }
            trace
        }
        Provider::Claude => {
            let root = "nested-claude-root";
            let parent = "nested-claude-parent";
            let child = "nested-claude-child";
            let aliases = BTreeMap::from([
                (format!("claude:agent:{parent}"), "parent".to_owned()),
                (format!("claude:agent:{child}"), "nested-child".to_owned()),
            ]);
            let mut adapter = ClaudeActivityFixtureAdapter::new(root);
            let mut trace = ProviderTrace::new(aliases);
            for (index, event) in [
                json!({
                    "hook_event_name": "SubagentStart",
                    "session_id": root,
                    "agent_id": parent,
                    "agent_type": "Explore"
                }),
                json!({
                    "hook_event_name": "SubagentStart",
                    "session_id": root,
                    "agent_id": child,
                    "agent_type": "Explore",
                    "parent_agent_id": parent
                }),
            ]
            .into_iter()
            .enumerate()
            {
                trace.record(
                    format!("claude:nested:{index}"),
                    format!("2026-07-24T12:01:{index:02}Z"),
                    adapter
                        .handle_value(ClaudeActivityInputSource::HookInput, &event, 1_000)
                        .mutations,
                );
            }
            trace
        }
        Provider::OpenCode => {
            let root = "nested-opencode-root";
            let parent = "nested-opencode-parent";
            let child = "nested-opencode-child";
            let aliases = BTreeMap::from([
                (format!("opencode:session:{parent}"), "parent".to_owned()),
                (
                    format!("opencode:session:{child}"),
                    "nested-child".to_owned(),
                ),
            ]);
            let mut adapter = OpenCodeActivityFixtureAdapter::new(root);
            let mut trace = ProviderTrace::new(aliases);
            trace.record(
                "opencode:nested:parent",
                "2026-07-24T12:01:00Z",
                adapter
                    .reconcile_children(
                        root,
                        &json!([{
                            "id": parent,
                            "parentID": root,
                            "time": { "created": 1_000_u64 }
                        }]),
                    )
                    .mutations,
            );
            trace.record(
                "opencode:nested:child",
                "2026-07-24T12:01:01Z",
                adapter
                    .reconcile_children(
                        parent,
                        &json!([{
                            "id": child,
                            "parentID": parent,
                            "time": { "created": 2_000_u64 }
                        }]),
                    )
                    .mutations,
            );
            trace
        }
    }
}

fn drive_terminal_states(
    provider: Provider,
    states: &[String],
) -> Driven<BTreeMap<String, Option<String>>> {
    let base_observed_at_ms = recent_fixture_timestamp_ms();
    let aliases = states
        .iter()
        .map(|state| {
            let canonical_id = match provider {
                Provider::Codex => format!("codex:thread:state-codex-child-{state}"),
                Provider::Claude => format!("claude:agent:state-claude-{state}"),
                Provider::OpenCode => format!("opencode:session:state-opencode-child-{state}"),
            };
            (canonical_id, state.clone())
        })
        .collect();
    let mut trace = ProviderTrace::new(aliases);
    let mut statuses = BTreeMap::new();
    for (index, state) in states.iter().enumerate() {
        let observed_at_ms = base_observed_at_ms
            + u64::try_from(index).expect("terminal-state fixture index fits in u64");
        let status = match provider {
            Provider::Codex => {
                let root = format!("state-codex-root-{state}");
                let child = format!("state-codex-child-{state}");
                let mut adapter = CodexActivityFixtureAdapter::new(Some(&root));
                let output = adapter.handle_notification(
                    "item/started",
                    &json!({
                        "threadId": root,
                        "turnId": "state-turn",
                        "item": {
                            "id": "state-item",
                            "type": "collabAgentToolCall",
                            "tool": "spawnAgent",
                            "status": "inProgress",
                            "senderThreadId": root,
                            "receiverThreadIds": [child],
                            "agentsStates": {
                                child: { "status": state, "message": null }
                            }
                        }
                    }),
                    observed_at_ms,
                );
                let status = actor_status(output.mutations.clone());
                trace.record(
                    format!("codex:terminal:{index}:{state}"),
                    format!("2026-07-24T12:02:{index:02}Z"),
                    output.mutations,
                );
                status
            }
            Provider::Claude => {
                let mut adapter = ClaudeActivityFixtureAdapter::new("state-claude-root");
                let output = adapter.handle_value(
                    ClaudeActivityInputSource::HookInput,
                    &json!({
                        "hook_event_name": "SubagentState",
                        "session_id": "state-claude-root",
                        "agent_id": format!("state-claude-{state}"),
                        "agent_type": "Explore",
                        "status": state
                    }),
                    observed_at_ms,
                );
                let status = actor_status(output.mutations.clone());
                trace.record(
                    format!("claude:terminal:{index}:{state}"),
                    format!("2026-07-24T12:02:{index:02}Z"),
                    output.mutations,
                );
                status
            }
            Provider::OpenCode => {
                let root = format!("state-opencode-root-{state}");
                let child = format!("state-opencode-child-{state}");
                let mut adapter = OpenCodeActivityFixtureAdapter::new(&root);
                let seed = adapter.reconcile_children(
                    &root,
                    &json!([{
                        "id": child,
                        "parentID": root,
                        "time": { "created": observed_at_ms }
                    }]),
                );
                trace.record(
                    format!("opencode:terminal:{index}:{state}:seed"),
                    format!("2026-07-24T12:02:{:02}Z", index * 2),
                    seed.mutations,
                );
                let output = match state.as_str() {
                    "failed" => adapter.handle_history(
                        &child,
                        &json!([{
                            "info": {
                                "id": "failed-message",
                                "sessionID": child,
                                "role": "assistant",
                                "time": { "completed": observed_at_ms + 1 },
                                "error": { "name": "ProviderError" }
                            },
                            "parts": []
                        }]),
                    ),
                    "cancelled" => adapter.handle_event_at(
                        &json!({
                            "id": "cancelled-event",
                            "type": "session.error",
                            "properties": {
                                "sessionID": child,
                                "error": { "name": "MessageAbortedError" }
                            }
                        }),
                        observed_at_ms + 1,
                    ),
                    "interrupted" | "unknown" => adapter.handle_event_at(
                        &json!({
                            "id": format!("{state}-event"),
                            "type": "session.status",
                            "properties": {
                                "sessionID": child,
                                "status": { "type": state }
                            }
                        }),
                        observed_at_ms + 1,
                    ),
                    other => panic!("unknown canonical terminal state {other}"),
                };
                let status = actor_status(output.mutations.clone());
                trace.record(
                    format!("opencode:terminal:{index}:{state}:outcome"),
                    format!("2026-07-24T12:02:{:02}Z", index * 2 + 1),
                    output.mutations,
                );
                status
            }
        };
        statuses.insert(state.clone(), status);
    }
    Driven {
        value: statuses,
        trace,
    }
}

fn drive_entry_kinds(provider: Provider) -> Driven<BTreeSet<String>> {
    let observed_at_ms = recent_fixture_timestamp_ms();
    let mut kinds = BTreeSet::new();
    let aliases = BTreeMap::from([(
        match provider {
            Provider::Codex => "codex:thread:entry-codex-child",
            Provider::Claude => "claude:agent:entry-claude-child",
            Provider::OpenCode => "opencode:session:entry-opencode-child",
        }
        .to_owned(),
        "entry-child".to_owned(),
    )]);
    let mut trace = ProviderTrace::new(aliases);
    match provider {
        Provider::Codex => {
            let mut adapter = CodexActivityFixtureAdapter::new(Some("entry-codex-root"));
            let seed = adapter.handle_notification(
                "item/started",
                &json!({
                    "threadId": "entry-codex-root",
                    "turnId": "entry-seed-turn",
                    "item": {
                        "id": "entry-seed",
                        "type": "collabAgentToolCall",
                        "tool": "spawnAgent",
                        "status": "inProgress",
                        "senderThreadId": "entry-codex-root",
                        "receiverThreadIds": ["entry-codex-child"],
                        "agentsStates": {
                            "entry-codex-child": { "status": "running", "message": null }
                        }
                    }
                }),
                observed_at_ms,
            );
            trace.record("codex:entries:seed", "2026-07-24T12:03:00Z", seed.mutations);
            for (index, params) in [
                json!({
                    "threadId": "entry-codex-child",
                    "turnId": "commentary-turn",
                    "item": {
                        "id": "commentary-item",
                        "type": "agentMessage",
                        "text": "visible commentary"
                    }
                }),
                json!({
                    "threadId": "entry-codex-child",
                    "turnId": "tool-turn",
                    "item": {
                        "id": "tool-item",
                        "type": "mcpToolCall",
                        "tool": "Read",
                        "status": "completed"
                    }
                }),
                json!({
                    "threadId": "entry-codex-child",
                    "turnId": "command-turn",
                    "item": {
                        "id": "command-item",
                        "type": "commandExecution",
                        "status": "completed"
                    }
                }),
            ]
            .into_iter()
            .enumerate()
            {
                let output =
                    adapter.handle_notification("item/completed", &params, observed_at_ms + 1);
                collect_entry_kinds(&mut kinds, output.mutations.clone());
                trace.record(
                    format!("codex:entries:item:{index}"),
                    format!("2026-07-24T12:03:{:02}Z", index + 1),
                    output.mutations,
                );
            }
            for (index, (turn_id, status)) in
                [("failed-turn", "failed"), ("state-turn", "interrupted")]
                    .into_iter()
                    .enumerate()
            {
                let output = adapter.handle_notification(
                    "turn/completed",
                    &json!({
                        "threadId": "entry-codex-child",
                        "turn": {
                            "id": turn_id,
                            "status": status,
                            "startedAt": observed_at_ms + 1,
                            "completedAt": observed_at_ms + 2
                        }
                    }),
                    observed_at_ms + 2,
                );
                collect_entry_kinds(&mut kinds, output.mutations.clone());
                trace.record(
                    format!("codex:entries:turn:{index}"),
                    format!("2026-07-24T12:03:{:02}Z", index + 4),
                    output.mutations,
                );
            }
        }
        Provider::Claude => {
            let mut adapter = ClaudeActivityFixtureAdapter::new("entry-claude-root");
            let seed = adapter.handle_value(
                ClaudeActivityInputSource::HookInput,
                &json!({
                    "hook_event_name": "SubagentStart",
                    "session_id": "entry-claude-root",
                    "agent_id": "entry-claude-child",
                    "agent_type": "Explore"
                }),
                observed_at_ms,
            );
            trace.record(
                "claude:entries:seed",
                "2026-07-24T12:03:00Z",
                seed.mutations,
            );
            for (index, event) in [
                json!({
                    "hook_event_name": "PreToolUse",
                    "session_id": "entry-claude-root",
                    "agent_id": "entry-claude-child",
                    "tool_name": "Read",
                    "tool_use_id": "tool-entry",
                    "tool_input": {}
                }),
                json!({
                    "hook_event_name": "PreToolUse",
                    "session_id": "entry-claude-root",
                    "agent_id": "entry-claude-child",
                    "tool_name": "Bash",
                    "tool_use_id": "command-entry",
                    "tool_input": { "command": "pwd" }
                }),
                json!({
                    "hook_event_name": "PreToolUse",
                    "session_id": "entry-claude-root",
                    "agent_id": "entry-claude-child",
                    "tool_name": "Read",
                    "tool_use_id": "error-entry",
                    "tool_input": {}
                }),
                json!({
                    "hook_event_name": "PostToolUseFailure",
                    "session_id": "entry-claude-root",
                    "agent_id": "entry-claude-child",
                    "tool_name": "Read",
                    "tool_use_id": "error-entry",
                    "tool_input": {},
                    "error": "not found"
                }),
            ]
            .into_iter()
            .enumerate()
            {
                let output = adapter.handle_value(
                    ClaudeActivityInputSource::HookInput,
                    &event,
                    observed_at_ms + 1,
                );
                collect_entry_kinds(&mut kinds, output.mutations.clone());
                trace.record(
                    format!("claude:entries:hook:{index}"),
                    format!("2026-07-24T12:03:{:02}Z", index + 1),
                    output.mutations,
                );
            }
            let transcript = r#"{"type":"assistant","sessionId":"entry-claude-root","agentId":"entry-claude-child","isSidechain":true,"uuid":"commentary-message","timestamp":"2026-07-24T12:00:00Z","message":{"role":"assistant","content":[{"type":"text","text":"Recovered commentary"}]}}"#;
            let output = ClaudeTranscriptFixtureAdapter::recover(
                "entry-claude-root",
                "entry-claude-child",
                "Explore",
                transcript.as_bytes(),
            );
            collect_entry_kinds(&mut kinds, output.mutations.clone());
            trace.record(
                "claude:entries:transcript",
                "2026-07-24T12:03:05Z",
                output.mutations,
            );
        }
        Provider::OpenCode => {
            let root = "entry-opencode-root";
            let child = "entry-opencode-child";
            let mut adapter = OpenCodeActivityFixtureAdapter::new(root);
            let seed = adapter.reconcile_children(
                root,
                &json!([{
                    "id": child,
                    "parentID": root,
                    "time": { "created": observed_at_ms }
                }]),
            );
            trace.record(
                "opencode:entries:seed",
                "2026-07-24T12:03:00Z",
                seed.mutations,
            );
            adapter.handle_event_at(
                &json!({
                    "id": "assistant-message",
                    "type": "message.updated",
                    "properties": {
                        "sessionID": child,
                        "info": {
                            "id": "assistant",
                            "sessionID": child,
                            "role": "assistant"
                        }
                    }
                }),
                observed_at_ms,
            );
            adapter.handle_event_at(
                &json!({
                    "id": "commentary-event",
                    "type": "message.part.updated",
                    "properties": {
                        "sessionID": child,
                        "part": {
                            "id": "commentary-part",
                            "sessionID": child,
                            "messageID": "assistant",
                            "type": "text",
                            "text": "visible commentary"
                        }
                    }
                }),
                observed_at_ms + 1,
            );
            let commentary = adapter.flush_text();
            collect_entry_kinds(&mut kinds, commentary.mutations.clone());
            trace.record(
                "opencode:entries:commentary",
                "2026-07-24T12:03:01Z",
                commentary.mutations,
            );
            for (index, event) in [
                json!({
                    "id": "tool-event",
                    "type": "message.part.updated",
                    "properties": {
                        "sessionID": child,
                        "part": {
                            "id": "tool-part",
                            "sessionID": child,
                            "messageID": "assistant",
                            "type": "tool",
                            "callID": "tool-call",
                            "tool": "Read",
                            "state": { "status": "completed" }
                        }
                    }
                }),
                json!({
                    "id": "command-event",
                    "type": "command.executed",
                    "properties": {
                        "sessionID": child,
                        "messageID": "assistant",
                        "name": "test",
                        "arguments": "--focused"
                    }
                }),
                json!({
                    "id": "error-event",
                    "type": "message.part.updated",
                    "properties": {
                        "sessionID": child,
                        "part": {
                            "id": "error-part",
                            "sessionID": child,
                            "messageID": "assistant",
                            "type": "tool",
                            "callID": "error-call",
                            "tool": "Read",
                            "state": { "status": "error" }
                        }
                    }
                }),
            ]
            .into_iter()
            .enumerate()
            {
                let output = adapter.handle_event_at(&event, observed_at_ms + 2);
                collect_entry_kinds(&mut kinds, output.mutations.clone());
                trace.record(
                    format!("opencode:entries:event:{index}"),
                    format!("2026-07-24T12:03:{:02}Z", index + 2),
                    output.mutations,
                );
            }
        }
    }
    Driven {
        value: kinds,
        trace,
    }
}

fn drive_duplicate_and_late(provider: Provider) -> Driven<SuppressionOutcome> {
    let observed_at_ms = recent_fixture_timestamp_ms();
    let aliases = BTreeMap::from([(
        match provider {
            Provider::Codex => "codex:thread:suppress-codex-child",
            Provider::Claude => "claude:agent:suppress-claude-child",
            Provider::OpenCode => "opencode:session:suppress-opencode-child",
        }
        .to_owned(),
        "suppressed-child".to_owned(),
    )]);
    let mut trace = ProviderTrace::new(aliases);
    match provider {
        Provider::Codex => {
            let mut adapter = CodexActivityFixtureAdapter::new(Some("suppress-codex-root"));
            let event = |item_id: &str, status: &str| {
                json!({
                    "threadId": "suppress-codex-root",
                    "turnId": "suppress-turn",
                    "item": {
                        "id": item_id,
                        "type": "collabAgentToolCall",
                        "tool": if status == "running" { "spawnAgent" } else { "wait" },
                        "status": if status == "running" { "inProgress" } else { "completed" },
                        "senderThreadId": "suppress-codex-root",
                        "receiverThreadIds": ["suppress-codex-child"],
                        "agentsStates": {
                            "suppress-codex-child": { "status": status, "message": null }
                        }
                    }
                })
            };
            let start = adapter.handle_notification(
                "item/started",
                &event("start", "running"),
                observed_at_ms,
            );
            trace.record(
                "codex:suppression:start",
                "2026-07-24T12:04:00Z",
                start.mutations,
            );
            let terminal = event("terminal", "completed");
            let completed =
                adapter.handle_notification("item/completed", &terminal, observed_at_ms + 1);
            trace.record(
                "codex:suppression:terminal",
                "2026-07-24T12:04:01Z",
                completed.mutations,
            );
            let duplicate =
                adapter.handle_notification("item/completed", &terminal, observed_at_ms + 1);
            let duplicate_suppressed = duplicate.mutations.is_empty();
            trace.record(
                "codex:suppression:duplicate",
                "2026-07-24T12:04:02Z",
                duplicate.mutations,
            );
            let late = adapter.handle_notification(
                "item/completed",
                &event("late", "running"),
                observed_at_ms + 2,
            );
            let late_progress_suppressed = late.mutations.is_empty();
            trace.record(
                "codex:suppression:late",
                "2026-07-24T12:04:03Z",
                late.mutations,
            );
            Driven {
                value: SuppressionOutcome {
                    duplicate_suppressed,
                    late_progress_suppressed,
                },
                trace,
            }
        }
        Provider::Claude => {
            let mut adapter = ClaudeActivityFixtureAdapter::new("suppress-claude-root");
            let start = adapter.handle_value(
                ClaudeActivityInputSource::HookInput,
                &json!({
                    "hook_event_name": "SubagentStart",
                    "session_id": "suppress-claude-root",
                    "agent_id": "suppress-claude-child",
                    "agent_type": "Explore"
                }),
                observed_at_ms,
            );
            trace.record(
                "claude:suppression:start",
                "2026-07-24T12:04:00Z",
                start.mutations,
            );
            let terminal = json!({
                "hook_event_name": "SubagentStop",
                "session_id": "suppress-claude-root",
                "agent_id": "suppress-claude-child",
                "agent_type": "Explore",
                "last_assistant_message": "done"
            });
            let completed = adapter.handle_value(
                ClaudeActivityInputSource::HookInput,
                &terminal,
                observed_at_ms + 1,
            );
            trace.record(
                "claude:suppression:terminal",
                "2026-07-24T12:04:01Z",
                completed.mutations,
            );
            let duplicate = adapter.handle_value(
                ClaudeActivityInputSource::HookInput,
                &terminal,
                observed_at_ms + 1,
            );
            let duplicate_suppressed = duplicate.mutations.is_empty();
            trace.record(
                "claude:suppression:duplicate",
                "2026-07-24T12:04:02Z",
                duplicate.mutations,
            );
            let late = adapter.handle_value(
                ClaudeActivityInputSource::HookInput,
                &json!({
                    "hook_event_name": "PreToolUse",
                    "session_id": "suppress-claude-root",
                    "agent_id": "suppress-claude-child",
                    "tool_name": "Read",
                    "tool_use_id": "late-tool",
                    "tool_input": {}
                }),
                observed_at_ms + 2,
            );
            let late_progress_suppressed = late.mutations.is_empty();
            trace.record(
                "claude:suppression:late",
                "2026-07-24T12:04:03Z",
                late.mutations,
            );
            Driven {
                value: SuppressionOutcome {
                    duplicate_suppressed,
                    late_progress_suppressed,
                },
                trace,
            }
        }
        Provider::OpenCode => {
            let root = "suppress-opencode-root";
            let child = "suppress-opencode-child";
            let mut adapter = OpenCodeActivityFixtureAdapter::new(root);
            let start = adapter.reconcile_children(
                root,
                &json!([{
                    "id": child,
                    "parentID": root,
                    "time": { "created": observed_at_ms }
                }]),
            );
            trace.record(
                "opencode:suppression:start",
                "2026-07-24T12:04:00Z",
                start.mutations,
            );
            let history = json!([{
                "info": {
                    "id": "complete-message",
                    "sessionID": child,
                    "role": "assistant",
                    "time": { "completed": observed_at_ms + 1 },
                    "finish": "stop"
                },
                "parts": []
            }]);
            let completed = adapter.handle_history(child, &history);
            trace.record(
                "opencode:suppression:terminal",
                "2026-07-24T12:04:01Z",
                completed.mutations,
            );
            let duplicate = adapter.handle_history(child, &history);
            let duplicate_suppressed = duplicate.mutations.is_empty();
            trace.record(
                "opencode:suppression:duplicate",
                "2026-07-24T12:04:02Z",
                duplicate.mutations,
            );
            let late = adapter.handle_event_at(
                &json!({
                    "id": "late-busy",
                    "type": "session.status",
                    "properties": {
                        "sessionID": child,
                        "status": { "type": "busy" }
                    }
                }),
                observed_at_ms + 2,
            );
            let late_progress_suppressed = late.mutations.is_empty();
            trace.record(
                "opencode:suppression:late",
                "2026-07-24T12:04:03Z",
                late.mutations,
            );
            Driven {
                value: SuppressionOutcome {
                    duplicate_suppressed,
                    late_progress_suppressed,
                },
                trace,
            }
        }
    }
}

fn drive_history_repair(provider: Provider) -> Driven<RecoveryOutcome> {
    let observed_at_ms = recent_fixture_timestamp_ms();
    let observed_at_seconds = observed_at_ms / 1_000;
    let aliases = BTreeMap::from([(
        match provider {
            Provider::Codex => "codex:thread:recovery-codex-child",
            Provider::Claude => "claude:agent:recovery-claude-child",
            Provider::OpenCode => "opencode:session:recovery-opencode-child",
        }
        .to_owned(),
        "recovered-child".to_owned(),
    )]);
    let mut trace = ProviderTrace::new(aliases);
    match provider {
        Provider::Codex => {
            let root = "recovery-codex-root";
            let child = "recovery-codex-child";
            let mut adapter = CodexActivityFixtureAdapter::new(Some(root));
            let discovered = adapter.handle_envelope(&json!({
                "id": "recovery-list-conformance",
                "result": {
                    "data": [{
                        "id": child,
                        "parentThreadId": root,
                        "createdAt": observed_at_seconds,
                        "updatedAt": observed_at_seconds + 1,
                        "status": { "type": "idle" }
                    }],
                    "nextCursor": null,
                    "backwardsCursor": null
                }
            }));
            trace.record(
                "codex:recovery:discover",
                "2026-07-24T12:05:00Z",
                discovered.mutations,
            );
            let output = adapter.handle_envelope(&json!({
                "id": "recovery-read-conformance",
                "result": {
                    "thread": {
                        "id": child,
                        "parentThreadId": root,
                        "createdAt": observed_at_seconds,
                        "updatedAt": observed_at_seconds + 3,
                        "status": { "type": "idle" },
                        "turns": [{
                            "id": "missed-turn",
                            "status": "completed",
                            "startedAt": observed_at_seconds,
                            "completedAt": observed_at_seconds + 3,
                            "items": []
                        }]
                    }
                }
            }));
            let final_status = actor_status(output.mutations.clone());
            trace.record(
                "codex:recovery:read",
                "2026-07-24T12:05:01Z",
                output.mutations,
            );
            Driven {
                value: RecoveryOutcome {
                    actor_status_repair_supported: true,
                    history_recovery: "full".to_owned(),
                    final_status,
                    recovered_entry_kinds: Vec::new(),
                },
                trace,
            }
        }
        Provider::Claude => {
            let transcript = [
                r#"{"type":"assistant","sessionId":"recovery-claude-root","agentId":"recovery-claude-child","isSidechain":true,"uuid":"message-1","timestamp":"2026-07-24T12:00:00Z","message":{"role":"assistant","content":[{"type":"text","text":"Recovered commentary"},{"type":"tool_use","id":"tool-1","name":"Read","input":{"file_path":"/redacted"}}]}}"#,
                r#"{"type":"user","sessionId":"recovery-claude-root","agentId":"recovery-claude-child","isSidechain":true,"uuid":"message-2","timestamp":"2026-07-24T12:00:01Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool-1","content":"ok","is_error":false}]}}"#,
            ]
            .join("\n");
            let mut adapter = ClaudeTranscriptFixtureAdapter::new("recovery-claude-root");
            let correlated_start = adapter.handle_hook(
                &json!({
                    "hook_event_name": "SubagentStart",
                    "session_id": "recovery-claude-root",
                    "agent_id": "recovery-claude-child",
                    "agent_type": "Explore"
                }),
                observed_at_ms,
            );
            assert_eq!(
                actor_status(correlated_start.mutations.clone()),
                Some("running".to_owned())
            );
            trace.record(
                "claude:recovery:start",
                "2026-07-24T12:05:00Z",
                correlated_start.mutations,
            );
            let output =
                adapter.recover_bytes("recovery-claude-child", "Explore", transcript.as_bytes());
            assert!(output.correlation_validated);
            let final_status = actor_status(output.mutations.clone());
            let mut recovered_entry_kinds = BTreeSet::new();
            collect_entry_kinds(&mut recovered_entry_kinds, output.mutations.clone());
            trace.record(
                "claude:recovery:transcript",
                "2026-07-24T12:05:01Z",
                output.mutations,
            );
            let mut oversized_transcript = vec![b'x'; 10 * 1_024 * 1_024 + 4_096];
            oversized_transcript.push(b'\n');
            oversized_transcript.extend_from_slice(transcript.as_bytes());
            let bounded_probe = ClaudeTranscriptFixtureAdapter::recover(
                "recovery-claude-root",
                "recovery-claude-child",
                "Explore",
                &oversized_transcript,
            );
            let history_recovery = if bounded_probe.correlation_validated
                && bounded_probe.scanned_bytes < oversized_transcript.len()
                && bounded_probe
                    .mutations
                    .iter()
                    .any(|mutation| matches!(mutation, ProviderActivityMutation::AppendEntry(_)))
            {
                "bounded"
            } else {
                "none"
            };
            Driven {
                value: RecoveryOutcome {
                    actor_status_repair_supported: false,
                    history_recovery: history_recovery.to_owned(),
                    final_status,
                    recovered_entry_kinds: recovered_entry_kinds.into_iter().collect(),
                },
                trace,
            }
        }
        Provider::OpenCode => {
            let root = "recovery-opencode-root";
            let child = "recovery-opencode-child";
            let mut adapter = OpenCodeActivityFixtureAdapter::new(root);
            let discovered = adapter.reconcile_children(
                root,
                &json!([{
                    "id": child,
                    "parentID": root,
                    "time": { "created": observed_at_ms }
                }]),
            );
            trace.record(
                "opencode:recovery:discover",
                "2026-07-24T12:05:00Z",
                discovered.mutations,
            );
            let output = adapter.handle_history(
                child,
                &json!([{
                    "info": {
                        "id": "missed-message",
                        "sessionID": child,
                        "role": "assistant",
                        "time": { "completed": observed_at_ms + 1 },
                        "finish": "stop"
                    },
                    "parts": []
                }]),
            );
            let final_status = actor_status(output.mutations.clone());
            trace.record(
                "opencode:recovery:history",
                "2026-07-24T12:05:01Z",
                output.mutations,
            );
            Driven {
                value: RecoveryOutcome {
                    actor_status_repair_supported: true,
                    history_recovery: "full".to_owned(),
                    final_status,
                    recovered_entry_kinds: Vec::new(),
                },
                trace,
            }
        }
    }
}

fn drive_native_input_bounds(provider: Provider) -> Driven<NativeInputOutcome> {
    let oversized_id = "x".repeat(1_024);
    let distinct_oversized_id = "y".repeat(1_024);
    let oversized_display = "display".repeat(2_048);
    let mut trace = ProviderTrace::default();
    match provider {
        Provider::Codex => {
            let mut malformed_adapter = CodexActivityFixtureAdapter::new(Some("bounds-codex-root"));
            let malformed = malformed_adapter.handle_notification(
                "item/started",
                &json!({
                    "threadId": "bounds-codex-root",
                    "turnId": "malformed-turn",
                    "item": {
                        "id": "malformed-item",
                        "type": "collabAgentToolCall",
                        "tool": "spawnAgent",
                        "status": "inProgress",
                        "senderThreadId": "bounds-codex-root",
                        "receiverThreadIds": [""],
                        "agentsStates": {}
                    }
                }),
                1_000,
            );
            let malformed_rejected = malformed.mutations.is_empty();
            trace.record_without_semantics(
                "codex:bounds:malformed",
                "2026-07-24T12:06:00Z",
                malformed.mutations,
            );
            let mut oversized_adapter = CodexActivityFixtureAdapter::new(Some("bounds-codex-root"));
            let oversized_mutations = oversized_adapter
                .handle_notification(
                    "item/started",
                    &json!({
                        "threadId": "bounds-codex-root",
                        "turnId": "oversized-turn",
                        "item": {
                            "id": "oversized-item",
                            "type": "collabAgentToolCall",
                            "tool": "spawnAgent",
                            "status": "inProgress",
                            "senderThreadId": "bounds-codex-root",
                            "receiverThreadIds": [oversized_id],
                            "agentsStates": {}
                        }
                    }),
                    1_000,
                )
                .mutations;
            let oversized_replay = CodexActivityFixtureAdapter::new(Some("bounds-codex-root"))
                .handle_notification(
                    "item/started",
                    &json!({
                        "threadId": "bounds-codex-root",
                        "turnId": "oversized-replay-turn",
                        "item": {
                            "id": "oversized-replay-item",
                            "type": "collabAgentToolCall",
                            "tool": "spawnAgent",
                            "status": "inProgress",
                            "senderThreadId": "bounds-codex-root",
                            "receiverThreadIds": [oversized_id],
                            "agentsStates": {}
                        }
                    }),
                    1_000,
                )
                .mutations;
            let distinct_oversized = CodexActivityFixtureAdapter::new(Some("bounds-codex-root"))
                .handle_notification(
                    "item/started",
                    &json!({
                        "threadId": "bounds-codex-root",
                        "turnId": "oversized-distinct-turn",
                        "item": {
                            "id": "oversized-distinct-item",
                            "type": "collabAgentToolCall",
                            "tool": "spawnAgent",
                            "status": "inProgress",
                            "senderThreadId": "bounds-codex-root",
                            "receiverThreadIds": [distinct_oversized_id],
                            "agentsStates": {}
                        }
                    }),
                    1_000,
                )
                .mutations;
            let oversized_identity = assert_oversized_identity_semantics(
                provider,
                &oversized_id,
                &oversized_mutations,
                &oversized_replay,
                &distinct_oversized,
            );
            trace.record_without_semantics(
                "codex:bounds:identity",
                "2026-07-24T12:06:01Z",
                oversized_mutations,
            );
            let mut display_adapter = CodexActivityFixtureAdapter::new(Some("bounds-codex-root"));
            let display_mutations = display_adapter
                .handle_notification(
                    "item/started",
                    &json!({
                        "threadId": "bounds-codex-root",
                        "turnId": "display-turn",
                        "item": {
                            "id": "display-item",
                            "type": "collabAgentToolCall",
                            "tool": "spawnAgent",
                            "status": "inProgress",
                            "senderThreadId": "bounds-codex-root",
                            "receiverThreadIds": ["bounds-codex-child"],
                            "agentsStates": {
                                "bounds-codex-child": {
                                    "status": "running",
                                    "message": oversized_display
                                }
                            }
                        }
                    }),
                    1_000,
                )
                .mutations;
            let oversized_display_fields_bounded = !display_mutations.is_empty()
                && oversized_mutations_are_safe(&display_mutations, &oversized_display);
            let valid_control_emitted = !display_mutations.is_empty();
            trace.record_without_semantics(
                "codex:bounds:display",
                "2026-07-24T12:06:02Z",
                display_mutations,
            );
            Driven {
                value: NativeInputOutcome {
                    valid_control_emitted,
                    malformed_rejected,
                    oversized_identity,
                    oversized_display_fields_bounded,
                },
                trace,
            }
        }
        Provider::Claude => {
            let mut malformed_adapter = ClaudeActivityFixtureAdapter::new("bounds-claude-root");
            let malformed = malformed_adapter.handle_value(
                ClaudeActivityInputSource::HookInput,
                &json!({
                    "hook_event_name": "SubagentStart",
                    "session_id": "bounds-claude-root",
                    "agent_id": "",
                    "agent_type": "Explore"
                }),
                1_000,
            );
            let malformed_rejected = malformed.mutations.is_empty();
            trace.record_without_semantics(
                "claude:bounds:malformed",
                "2026-07-24T12:06:00Z",
                malformed.mutations,
            );
            let mut oversized_adapter = ClaudeActivityFixtureAdapter::new("bounds-claude-root");
            let oversized_mutations = oversized_adapter
                .handle_value(
                    ClaudeActivityInputSource::HookInput,
                    &json!({
                        "hook_event_name": "SubagentStart",
                        "session_id": "bounds-claude-root",
                        "agent_id": oversized_id,
                        "agent_type": "Explore"
                    }),
                    1_000,
                )
                .mutations;
            let oversized_replay = ClaudeActivityFixtureAdapter::new("bounds-claude-root")
                .handle_value(
                    ClaudeActivityInputSource::HookInput,
                    &json!({
                        "hook_event_name": "SubagentStart",
                        "session_id": "bounds-claude-root",
                        "agent_id": oversized_id,
                        "agent_type": "Explore"
                    }),
                    1_000,
                )
                .mutations;
            let distinct_oversized = ClaudeActivityFixtureAdapter::new("bounds-claude-root")
                .handle_value(
                    ClaudeActivityInputSource::HookInput,
                    &json!({
                        "hook_event_name": "SubagentStart",
                        "session_id": "bounds-claude-root",
                        "agent_id": distinct_oversized_id,
                        "agent_type": "Explore"
                    }),
                    1_000,
                )
                .mutations;
            let oversized_identity = assert_oversized_identity_semantics(
                provider,
                &oversized_id,
                &oversized_mutations,
                &oversized_replay,
                &distinct_oversized,
            );
            trace.record_without_semantics(
                "claude:bounds:identity",
                "2026-07-24T12:06:01Z",
                oversized_mutations,
            );
            let mut display_adapter = ClaudeActivityFixtureAdapter::new("bounds-claude-root");
            let display_mutations = display_adapter
                .handle_value(
                    ClaudeActivityInputSource::HookInput,
                    &json!({
                        "hook_event_name": "SubagentStart",
                        "session_id": "bounds-claude-root",
                        "agent_id": "bounds-claude-child",
                        "agent_type": oversized_display
                    }),
                    1_000,
                )
                .mutations;
            let oversized_display_fields_bounded = !display_mutations.is_empty()
                && oversized_mutations_are_safe(&display_mutations, &oversized_display);
            let valid_control_emitted = !display_mutations.is_empty();
            trace.record_without_semantics(
                "claude:bounds:display",
                "2026-07-24T12:06:02Z",
                display_mutations,
            );
            Driven {
                value: NativeInputOutcome {
                    valid_control_emitted,
                    malformed_rejected,
                    oversized_identity,
                    oversized_display_fields_bounded,
                },
                trace,
            }
        }
        Provider::OpenCode => {
            let mut malformed_adapter = OpenCodeActivityFixtureAdapter::new("bounds-opencode-root");
            let malformed = malformed_adapter.reconcile_children(
                "bounds-opencode-root",
                &json!([{
                    "id": "",
                    "parentID": "bounds-opencode-root",
                    "time": { "created": 1_000_u64 }
                }]),
            );
            let malformed_rejected = malformed.mutations.is_empty();
            trace.record_without_semantics(
                "opencode:bounds:malformed",
                "2026-07-24T12:06:00Z",
                malformed.mutations,
            );
            let mut oversized_adapter = OpenCodeActivityFixtureAdapter::new("bounds-opencode-root");
            let oversized_mutations = oversized_adapter
                .reconcile_children(
                    "bounds-opencode-root",
                    &json!([{
                        "id": oversized_id,
                        "parentID": "bounds-opencode-root",
                        "time": { "created": 1_000_u64 }
                    }]),
                )
                .mutations;
            let oversized_replay = OpenCodeActivityFixtureAdapter::new("bounds-opencode-root")
                .reconcile_children(
                    "bounds-opencode-root",
                    &json!([{
                        "id": oversized_id,
                        "parentID": "bounds-opencode-root",
                        "time": { "created": 1_000_u64 }
                    }]),
                )
                .mutations;
            let distinct_oversized = OpenCodeActivityFixtureAdapter::new("bounds-opencode-root")
                .reconcile_children(
                    "bounds-opencode-root",
                    &json!([{
                        "id": distinct_oversized_id,
                        "parentID": "bounds-opencode-root",
                        "time": { "created": 1_000_u64 }
                    }]),
                )
                .mutations;
            let oversized_identity = assert_oversized_identity_semantics(
                provider,
                &oversized_id,
                &oversized_mutations,
                &oversized_replay,
                &distinct_oversized,
            );
            trace.record_without_semantics(
                "opencode:bounds:identity",
                "2026-07-24T12:06:01Z",
                oversized_mutations,
            );
            let mut display_adapter = OpenCodeActivityFixtureAdapter::new("bounds-opencode-root");
            let display_mutations = display_adapter
                .reconcile_children(
                    "bounds-opencode-root",
                    &json!([{
                        "id": "bounds-opencode-child",
                        "parentID": "bounds-opencode-root",
                        "title": oversized_display,
                        "time": { "created": 1_000_u64 }
                    }]),
                )
                .mutations;
            let oversized_display_fields_bounded = !display_mutations.is_empty()
                && oversized_mutations_are_safe(&display_mutations, &oversized_display);
            let valid_control_emitted = !display_mutations.is_empty();
            trace.record_without_semantics(
                "opencode:bounds:display",
                "2026-07-24T12:06:02Z",
                display_mutations,
            );
            Driven {
                value: NativeInputOutcome {
                    valid_control_emitted,
                    malformed_rejected,
                    oversized_identity,
                    oversized_display_fields_bounded,
                },
                trace,
            }
        }
    }
}

fn assert_oversized_identity_semantics(
    provider: Provider,
    oversized_id: &str,
    mutations: &[ProviderActivityMutation],
    replay: &[ProviderActivityMutation],
    distinct: &[ProviderActivityMutation],
) -> OversizedIdentityOutcome {
    if mutations.is_empty() {
        assert!(replay.is_empty(), "{} oversized replay", provider.name());
        assert!(
            distinct.is_empty(),
            "{} distinct oversized id",
            provider.name()
        );
        OversizedIdentityOutcome::Rejected
    } else {
        assert!(
            oversized_mutations_are_safe(mutations, oversized_id),
            "{} accepted oversized identity was not bounded",
            provider.name()
        );
        let actor_id = single_actor_id(mutations, provider, "primary oversized identity");
        let replay_id = single_actor_id(replay, provider, "replayed oversized identity");
        let distinct_id = single_actor_id(distinct, provider, "distinct oversized identity");
        assert_eq!(
            actor_id,
            replay_id,
            "{} deterministic identity",
            provider.name()
        );
        assert_ne!(
            actor_id,
            distinct_id,
            "{} identity collision",
            provider.name()
        );
        assert!(
            actor_id.starts_with(&format!("{}:", provider.name())),
            "{} canonical actor namespace: {actor_id}",
            provider.name()
        );
        assert!(
            actor_id.encode_utf16().count() <= ACTIVITY_ID_MAX_LENGTH,
            "{} canonical actor id bound",
            provider.name()
        );
        OversizedIdentityOutcome::BoundedNormalized
    }
}

fn single_actor_id<'a>(
    mutations: &'a [ProviderActivityMutation],
    provider: Provider,
    case: &str,
) -> &'a str {
    let actor_ids = mutations
        .iter()
        .filter_map(|mutation| match mutation {
            ProviderActivityMutation::UpsertActor(actor) => Some(actor.id.as_str()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let actor_id = actor_ids
        .first()
        .copied()
        .unwrap_or_else(|| panic!("{} {case} emitted no actor", provider.name()));
    assert!(
        actor_ids.len() == 1,
        "{} {case} emitted multiple unique actors: {actor_ids:?}",
        provider.name()
    );
    actor_id
}

fn oversized_mutations_are_safe(
    mutations: &[ProviderActivityMutation],
    oversized_value: &str,
) -> bool {
    mutations.iter().all(|mutation| {
        if format!("{mutation:?}").contains(oversized_value) {
            return false;
        }
        match mutation {
            ProviderActivityMutation::UpsertActor(actor) => {
                actor.id.encode_utf16().count() <= ACTIVITY_ID_MAX_LENGTH
                    && actor
                        .parent_actor_id
                        .as_ref()
                        .is_none_or(|id| id.encode_utf16().count() <= ACTIVITY_ID_MAX_LENGTH)
                    && actor.name.encode_utf16().count() <= ACTIVITY_LABEL_MAX_LENGTH
                    && actor
                        .role
                        .as_ref()
                        .is_none_or(|role| role.encode_utf16().count() <= ACTIVITY_LABEL_MAX_LENGTH)
                    && actor.provider_type.as_ref().is_none_or(|provider_type| {
                        provider_type.encode_utf16().count() <= ACTIVITY_LABEL_MAX_LENGTH
                    })
                    && actor.summary.as_ref().is_none_or(|summary| {
                        summary.encode_utf16().count() <= ACTIVITY_SUMMARY_MAX_LENGTH
                    })
            }
            ProviderActivityMutation::RemoveActor { actor_id } => {
                actor_id.encode_utf16().count() <= ACTIVITY_ID_MAX_LENGTH
            }
            ProviderActivityMutation::UpsertWorkItem(work_item) => {
                work_item.id.encode_utf16().count() <= ACTIVITY_ID_MAX_LENGTH
                    && work_item
                        .owner_actor_id
                        .as_ref()
                        .is_none_or(|id| id.encode_utf16().count() <= ACTIVITY_ID_MAX_LENGTH)
                    && work_item.name.encode_utf16().count() <= ACTIVITY_LABEL_MAX_LENGTH
                    && work_item.work_kind.encode_utf16().count() <= ACTIVITY_LABEL_MAX_LENGTH
                    && work_item.command.as_ref().is_none_or(|command| {
                        command.encode_utf16().count() <= ACTIVITY_DETAIL_MAX_LENGTH
                    })
                    && work_item
                        .cwd
                        .as_ref()
                        .is_none_or(|cwd| cwd.encode_utf16().count() <= ACTIVITY_DETAIL_MAX_LENGTH)
                    && work_item.summary.as_ref().is_none_or(|summary| {
                        summary.encode_utf16().count() <= ACTIVITY_SUMMARY_MAX_LENGTH
                    })
            }
            ProviderActivityMutation::RemoveWorkItem { work_item_id } => {
                work_item_id.encode_utf16().count() <= ACTIVITY_ID_MAX_LENGTH
            }
            ProviderActivityMutation::AppendEntry(entry) => {
                entry.id.encode_utf16().count() <= ACTIVITY_ID_MAX_LENGTH
                    && entry.owner_id.encode_utf16().count() <= ACTIVITY_ID_MAX_LENGTH
                    && entry.title.encode_utf16().count() <= ACTIVITY_LABEL_MAX_LENGTH
                    && entry.detail.as_ref().is_none_or(|detail| {
                        detail.encode_utf16().count() <= ACTIVITY_DETAIL_MAX_LENGTH
                    })
            }
            ProviderActivityMutation::SetScope { .. }
            | ProviderActivityMutation::SetSectionHealth { .. } => true,
        }
    })
}

fn actor_status(mutations: Vec<ProviderActivityMutation>) -> Option<String> {
    mutations
        .into_iter()
        .rev()
        .find_map(|mutation| match mutation {
            ProviderActivityMutation::UpsertActor(actor) => Some(actor.status.as_str().to_owned()),
            _ => None,
        })
}

fn collect_entry_kinds(kinds: &mut BTreeSet<String>, mutations: Vec<ProviderActivityMutation>) {
    kinds.extend(mutations.into_iter().filter_map(|mutation| match mutation {
        ProviderActivityMutation::AppendEntry(entry) => {
            Some(format!("{:?}", entry.kind).to_ascii_lowercase())
        }
        _ => None,
    }));
}

async fn migrated_database() -> Database {
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    database
}

fn thread_scope(scope_id: &str, thread_id: &str) -> ActivityScopeSeed {
    ActivityScopeSeed::thread(
        scope_id,
        thread_id,
        "codex",
        Some("codex"),
        ActivityCapabilities::structured_full(false),
    )
    .expect("valid thread scope")
}

fn terminal_scope(scope_id: &str, generation_id: &str, terminal_id: &str) -> ActivityScopeSeed {
    ActivityScopeSeed::terminal(
        scope_id,
        generation_id,
        "activity-conformance",
        terminal_id,
        "codex",
        Some("codex"),
        ActivityCapabilities::structured_full(true),
    )
    .expect("valid terminal scope")
}

fn actor_summary(
    id: &str,
    parent_actor_id: Option<&str>,
    status: ActivityLifecycle,
    started_at: &str,
    updated_at: &str,
    terminal_at: Option<&str>,
) -> Result<ActivityActorSummary, coddy_studio_server::activity::ActivityModelError> {
    ActivityActorSummary::try_new(
        id,
        parent_actor_id,
        id,
        None,
        None,
        status,
        None,
        started_at,
        updated_at,
        terminal_at,
    )
}

fn work_item_summary(
    id: &str,
    owner_actor_id: Option<&str>,
    status: ActivityLifecycle,
    started_at: &str,
    updated_at: &str,
    terminal_at: Option<&str>,
) -> Result<ActivityWorkItemSummary, coddy_studio_server::activity::ActivityModelError> {
    ActivityWorkItemSummary::try_new(
        id,
        owner_actor_id,
        id,
        "background",
        None,
        None,
        status,
        None,
        started_at,
        updated_at,
        terminal_at,
    )
}

fn entry(
    id: &str,
    owner_kind: ActivityRecordKind,
    owner_id: &str,
    created_at: &str,
) -> Result<ActivityEntry, coddy_studio_server::activity::ActivityModelError> {
    ActivityEntry::try_new(
        id,
        owner_kind,
        owner_id,
        ActivityEntryKind::Commentary,
        "Commentary",
        Some("Conformance entry"),
        ActivityEntryTone::Info,
        created_at,
    )
}

fn deterministic_permutation(length: usize, seed: u64) -> Vec<usize> {
    let mut values = (0..length).collect::<Vec<_>>();
    let mut state = seed;
    for index in (1..length).rev() {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        values.swap(index, state as usize % (index + 1));
    }
    values
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PermutationOutcome {
    Effective,
    Suppressed,
    Rejected,
}

fn permutation_dependencies(operation: usize, seed: u64) -> &'static [usize] {
    match operation {
        0 | 7 => &[],
        1 | 6 => &[0],
        2 => &[1],
        3 | 4 => &[2],
        5 => &[4],
        other => panic!("seed={seed:#x} unknown permutation dependency {other}"),
    }
}

fn permutation_operation(
    operation: usize,
    seed: u64,
    timeline_start: OffsetDateTime,
) -> (
    &'static str,
    String,
    Vec<ProviderActivityMutation>,
    PermutationOutcome,
) {
    let minute_offset = match operation {
        0 | 6 => 0,
        1 => 1,
        2 => 2,
        3 => 3,
        4 => 10,
        5 => 5,
        7 => 4,
        other => panic!("seed={seed:#x} unknown permutation operation {other}"),
    };
    let updated_at = (timeline_start + TimeDuration::minutes(minute_offset))
        .format(&Rfc3339)
        .unwrap_or_else(|error| panic!("seed={seed:#x} format operation {operation}: {error}"));
    match operation {
        0 => (
            "event:shared:root",
            updated_at,
            vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:shared:root",
                    None,
                    "Root",
                    "running",
                )
                .unwrap_or_else(|error| panic!("seed={seed:#x} construct root operation: {error}")),
            ],
            PermutationOutcome::Effective,
        ),
        1 => (
            "event:shared:parent",
            updated_at,
            vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:shared:parent",
                    Some("actor:shared:root"),
                    "Parent",
                    "running",
                )
                .unwrap_or_else(|error| {
                    panic!("seed={seed:#x} construct parent operation: {error}")
                }),
            ],
            PermutationOutcome::Effective,
        ),
        2 => (
            "event:shared:child",
            updated_at,
            vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:shared:child",
                    Some("actor:shared:parent"),
                    "Child",
                    "running",
                )
                .unwrap_or_else(|error| {
                    panic!("seed={seed:#x} construct child operation: {error}")
                }),
            ],
            PermutationOutcome::Effective,
        ),
        3 => (
            "event:shared:work",
            updated_at.clone(),
            vec![ProviderActivityMutation::UpsertWorkItem(
                work_item_summary(
                    "work:shared:child",
                    Some("actor:shared:child"),
                    ActivityLifecycle::Running,
                    &updated_at,
                    &updated_at,
                    None,
                )
                .unwrap_or_else(|error| {
                    panic!("seed={seed:#x} construct owned work operation: {error}")
                }),
            )],
            PermutationOutcome::Effective,
        ),
        4 => (
            "event:shared:complete",
            updated_at,
            vec![
                ProviderActivityMutation::set_actor_status("actor:shared:child", "completed")
                    .unwrap_or_else(|error| {
                        panic!("seed={seed:#x} construct completion operation: {error}")
                    }),
            ],
            PermutationOutcome::Effective,
        ),
        5 => (
            "event:shared:late-running",
            updated_at,
            vec![
                ProviderActivityMutation::set_actor_status("actor:shared:child", "running")
                    .unwrap_or_else(|error| {
                        panic!("seed={seed:#x} construct late-running operation: {error}")
                    }),
            ],
            PermutationOutcome::Suppressed,
        ),
        6 => (
            "event:shared:root",
            updated_at,
            vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:shared:root",
                    None,
                    "Root",
                    "running",
                )
                .unwrap_or_else(|error| {
                    panic!("seed={seed:#x} construct duplicate-root operation: {error}")
                }),
            ],
            PermutationOutcome::Suppressed,
        ),
        7 => (
            "event:shared:missing-owner",
            updated_at.clone(),
            vec![ProviderActivityMutation::UpsertWorkItem(
                work_item_summary(
                    "work:shared:missing",
                    Some("actor:shared:missing"),
                    ActivityLifecycle::Running,
                    &updated_at,
                    &updated_at,
                    None,
                )
                .unwrap_or_else(|error| {
                    panic!("seed={seed:#x} construct missing-owner operation: {error}")
                }),
            )],
            PermutationOutcome::Rejected,
        ),
        other => panic!("seed={seed:#x} unknown permutation operation {other}"),
    }
}
