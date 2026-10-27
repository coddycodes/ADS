use coddy_studio_server::provider::claude;

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::PathBuf,
};

use claude::{
    ClaudeActivityFixtureAdapter, ClaudeActivityInputSource, ClaudeTranscriptFixtureAdapter,
    canonical::{CanonicalEvent, CanonicalEventTrace},
    protocol::{AssistantMessage, ClaudeMessage},
    runtime::{
        ClaudeControlRequest, ClaudeProviderRuntime, Decision, LaunchRequestInput,
        PermissionRequestInput, ReconnectSnapshot, RuntimeMode, TurnInput, UserInputRequestInput,
        claude_hook_native_event_id_for_test,
    },
};
use coddy_studio_server::activity::{
    ACTIVITY_DETAIL_MAX_LENGTH, ACTIVITY_SUMMARY_MAX_LENGTH, ActivityActorSummary,
    ActivityCapabilities, ActivityEntry, ActivityLifecycle, ActivityObservationState,
    ActivityRecordKind, ProviderActivityMutation,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("claude-provider")
}

fn load_fixture<T: DeserializeOwned>(name: &str) -> T {
    let path = fixture_dir().join(name);
    let text = fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!("failed to read fixture {}: {error}", path.display());
    });
    serde_json::from_str(&text).unwrap_or_else(|error| {
        panic!("failed to decode fixture {}: {error}", path.display());
    })
}

fn assert_trace_eq(actual: &[CanonicalEvent], expected: &[CanonicalEventTrace]) {
    let actual_trace = actual
        .iter()
        .map(CanonicalEventTrace::from)
        .collect::<Vec<_>>();
    assert_eq!(actual_trace, expected);
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct LaunchFixture {
    thread_id: String,
    runtime_mode: RuntimeMode,
    cwd: Option<String>,
    claude_path: String,
    resume_session_id: Option<String>,
    new_session_id: Option<String>,
    expected: Value,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ControlFixture {
    interrupt: Value,
    set_permission_mode: Value,
    cancel_tool_call: Value,
    get_context_usage: Value,
    mcp_status: Value,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartupFixture {
    session_id: String,
    runtime_mode: RuntimeMode,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct TraceFixture {
    thread_id: String,
    turn_id: String,
    startup: StartupFixture,
    messages: Vec<ClaudeMessage>,
    expected_events: Vec<CanonicalEventTrace>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ContextUsageFixture {
    message_delta: Value,
    task_progress: Value,
    compact_boundary: Value,
    result: Value,
    query_success: Value,
    malformed: Value,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PermissionFixture {
    thread_id: String,
    turn_id: String,
    startup: StartupFixture,
    message: ClaudeMessage,
    request: PermissionRequestInput,
    resolution: PermissionResolutionFixture,
    expected_events: Vec<CanonicalEventTrace>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct PermissionResolutionFixture {
    decision: Decision,
    request_id: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserInputFixture {
    thread_id: String,
    turn_id: String,
    startup: StartupFixture,
    message: ClaudeMessage,
    request: UserInputRequestInput,
    resolution: UserInputResolutionFixture,
    expected_events: Vec<CanonicalEventTrace>,
    expected_updated_input: Value,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserInputResolutionFixture {
    request_id: String,
    answers: Value,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExitPlanFixture {
    thread_id: String,
    turn_id: String,
    startup: StartupFixture,
    message: AssistantMessage,
    expected_event: CanonicalEventTrace,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct StreamInterruptFixture {
    thread_id: String,
    turn_id: String,
    startup: StartupFixture,
    error: String,
    expected_events: Vec<CanonicalEventTrace>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReconnectFixture {
    thread_id: String,
    session_id: String,
    turn_id: String,
    runtime_mode: RuntimeMode,
    pending_approval: Value,
    pending_user_input: Value,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClaudeFixtureManifest {
    fixtures: Vec<String>,
    activity_fixtures: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClaudeActivityFixture {
    scenario: String,
    raw_input_lines: Vec<ClaudeRawInputLine>,
    expected_mutations: Vec<Value>,
    expected_mutation_input_indexes: Vec<usize>,
    #[serde(default)]
    no_mutation_input_indexes: Vec<usize>,
    expected_launch: Option<ClaudeActivityLaunchExpectation>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClaudeRawInputLine {
    source: ClaudeRawInputSource,
    line: String,
}

#[derive(Clone, Copy, Debug, serde::Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
enum ClaudeRawInputSource {
    BaseLaunch,
    CapabilityProbe,
    HookInput,
    Stream,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClaudeActivityLaunchExpectation {
    activity_flags_supported: bool,
    base_launch_functional: bool,
    required_base_arguments: Vec<String>,
    omitted_activity_arguments: Vec<String>,
    activity_capabilities: ActivityCapabilities,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClaudeTargetedCancellationFixture {
    session_id: String,
    orders: Vec<ClaudeTargetedCancellationOrder>,
    semantic_collision_facts: Vec<Value>,
    nested_facts: Vec<Value>,
    terminal_facts: Vec<Value>,
}

#[test]
fn targeted_task_correlation_fixture_requires_exact_nested_source_owner() {
    // Mutation caught: accepting nested Agent correlation without its exact parent tool owner.
    let fixture: ClaudeTargetedCancellationFixture =
        load_fixture("trace-targeted-task-cancellation.json");
    let mut runtime =
        ClaudeProviderRuntime::new("thread-targeted".to_owned(), fixture.session_id.clone());
    for (index, fact) in fixture.orders[0].facts.iter().enumerate() {
        let _ = apply_targeted_correlation_fact(&mut runtime, fact, index as u64);
    }
    let outputs = fixture
        .nested_facts
        .iter()
        .enumerate()
        .map(|(index, fact)| {
            apply_targeted_correlation_fact(&mut runtime, fact, 100 + index as u64)
        })
        .collect::<Vec<_>>();
    let statuses = outputs
        .iter()
        .flat_map(|output| &output.activity)
        .filter_map(|mutation| match mutation {
            ProviderActivityMutation::UpsertActor(actor)
                if actor.id == "claude:agent:agent-child" && actor.status.is_terminal() =>
            {
                Some(actor.status)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(statuses, [ActivityLifecycle::Failed]);
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClaudeTargetedCancellationOrder {
    name: String,
    facts: Vec<Value>,
}

fn apply_targeted_correlation_fact(
    runtime: &mut ClaudeProviderRuntime,
    fact: &Value,
    emitted_at_ms: u64,
) -> claude::runtime::ClaudeRuntimeOutput {
    if fact.get("hook_event_name").is_some() {
        runtime.handle_authenticated_hook_value(fact, emitted_at_ms)
    } else {
        runtime.handle_raw_value(fact, emitted_at_ms)
    }
}

#[test]
fn targeted_task_correlation_fixture_keeps_stopped_authoritative_after_subagent_stop() {
    // Mutation caught: ignoring a stopped task or allowing the later hook to rewrite it completed.
    let fixture: ClaudeTargetedCancellationFixture =
        load_fixture("trace-targeted-task-cancellation.json");
    assert_eq!(fixture.orders.len(), 2);
    assert!(fixture.orders.iter().all(|order| !order.name.is_empty()));

    for order in &fixture.orders {
        let mut runtime =
            ClaudeProviderRuntime::new("thread-targeted".to_owned(), fixture.session_id.clone());
        for (index, fact) in order.facts.iter().enumerate() {
            let _ = apply_targeted_correlation_fact(&mut runtime, fact, 1_000 + index as u64);
        }
        for (index, fact) in fixture.semantic_collision_facts.iter().enumerate() {
            let _ = apply_targeted_correlation_fact(&mut runtime, fact, 2_000 + index as u64);
        }

        let stopped =
            apply_targeted_correlation_fact(&mut runtime, &fixture.terminal_facts[0], 3_000);
        let after_stop =
            apply_targeted_correlation_fact(&mut runtime, &fixture.terminal_facts[1], 4_000);
        let stopped_statuses = stopped
            .activity
            .iter()
            .chain(&after_stop.activity)
            .filter_map(|mutation| match mutation {
                ProviderActivityMutation::UpsertActor(actor)
                    if actor.id == "claude:agent:agent-a" =>
                {
                    Some(actor.status)
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(
            stopped_statuses,
            [ActivityLifecycle::Cancelled],
            "{} must preserve stopped as the sole terminal lifecycle",
            order.name
        );
        assert!(
            stopped
                .activity
                .iter()
                .all(|mutation| !format!("{mutation:?}").contains("must not be retained")),
            "task output paths and summaries are not activity data"
        );
    }
}

#[test]
fn targeted_task_subtree_keeps_siblings_live_and_unmapped_agents_observable() {
    // Mutation caught: joining siblings by semantic fields or hiding an observable actor merely
    // because it lacks the exact Agent-tool/task identity chain required for control.
    let fixture: ClaudeTargetedCancellationFixture =
        load_fixture("trace-targeted-task-cancellation.json");
    let mut runtime =
        ClaudeProviderRuntime::new("thread-targeted".to_owned(), fixture.session_id.clone());
    let mut outputs = Vec::new();
    for (index, fact) in fixture.orders[0].facts.iter().enumerate() {
        outputs.push(apply_targeted_correlation_fact(
            &mut runtime,
            fact,
            1_000 + index as u64,
        ));
    }
    for (index, fact) in fixture.semantic_collision_facts.iter().enumerate() {
        outputs.push(apply_targeted_correlation_fact(
            &mut runtime,
            fact,
            2_000 + index as u64,
        ));
    }
    for (index, fact) in fixture.nested_facts[..4].iter().enumerate() {
        outputs.push(apply_targeted_correlation_fact(
            &mut runtime,
            fact,
            3_000 + index as u64,
        ));
    }
    outputs.push(runtime.handle_authenticated_hook_value(
        &json!({
            "hook_event_name":"SubagentStart",
            "session_id":fixture.session_id,
            "agent_id":"agent-unmapped",
            "agent_type":"same-role",
            "description":"same description",
            "prompt":"same prompt"
        }),
        4_000,
    ));

    let actors = outputs
        .iter()
        .flat_map(|output| &output.activity)
        .filter_map(|mutation| match mutation {
            ProviderActivityMutation::UpsertActor(actor) => Some(actor.clone()),
            _ => None,
        })
        .fold(std::collections::BTreeMap::new(), |mut latest, actor| {
            latest.insert(actor.id.clone(), actor);
            latest
        });
    assert_eq!(
        actors["claude:agent:agent-child"]
            .parent_actor_id
            .as_deref(),
        Some("claude:agent:agent-a")
    );
    assert!(actors["claude:agent:agent-a"].parent_actor_id.is_none());
    assert!(actors["claude:agent:agent-b"].parent_actor_id.is_none());
    assert!(
        actors["claude:agent:agent-unmapped"]
            .parent_actor_id
            .is_none()
    );
    assert!(actors.values().all(|actor| !actor.status.is_terminal()));

    let stopped = apply_targeted_correlation_fact(&mut runtime, &fixture.terminal_facts[0], 5_000);
    assert!(stopped.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::UpsertActor(actor)
            if actor.id == "claude:agent:agent-a"
                && actor.status == ActivityLifecycle::Cancelled
    )));
    assert!(stopped.activity.iter().all(|mutation| !matches!(
        mutation,
        ProviderActivityMutation::UpsertActor(actor)
            if actor.id == "claude:agent:agent-b"
                || actor.id == "claude:agent:agent-unmapped"
    )));
}

#[derive(Debug, serde::Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum ExpectedClaudeActivityMutation {
    AppendEntry {
        entry: ActivityEntry,
    },
    SetScope {
        capabilities: ActivityCapabilities,
        observation_state: ActivityObservationState,
    },
    UpsertActor {
        actor: ActivityActorSummary,
    },
}

#[test]
fn claude_subagent_activity_fixtures_are_manifest_driven_and_projected() {
    let manifest: ClaudeFixtureManifest = load_fixture("manifest.json");
    assert_eq!(
        manifest.activity_fixtures,
        [
            "trace-subagent-hooks.json",
            "trace-forwarded-subagent-text.json",
            "trace-subagent-tools.json",
            "trace-subagent-recovery.json",
            "trace-unsupported-hook-flags.json",
        ]
    );
    assert!(
        manifest.fixtures.windows(2).all(|pair| pair[0] < pair[1]),
        "the Claude fixture manifest must stay ordered"
    );

    let manifest_entries = manifest.fixtures.iter().collect::<HashSet<_>>();
    let mut scenarios = HashSet::new();
    let mut fixtures = Vec::with_capacity(manifest.activity_fixtures.len());
    for name in &manifest.activity_fixtures {
        assert!(
            manifest_entries.contains(name),
            "activity fixture {name} must also appear in the complete fixture manifest"
        );
        let fixture: ClaudeActivityFixture = load_fixture(name);
        assert!(
            scenarios.insert(fixture.scenario.clone()),
            "activity fixture scenarios must be unique"
        );
        validate_claude_activity_fixture(name, &fixture);
        fixtures.push(fixture);
    }
    validate_untrusted_boundary_coverage(&fixtures);

    let expected = fixtures
        .iter()
        .map(|fixture| {
            json!({
                "scenario": fixture.scenario,
                "mutations": fixture
                    .expected_mutation_input_indexes
                    .iter()
                    .zip(&fixture.expected_mutations)
                    .map(|(input_index, mutation)| {
                        json!({
                            "inputIndex": input_index,
                            "mutation": mutation,
                        })
                    })
                    .collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    let actual = fixtures
        .iter()
        .map(|fixture| {
            json!({
                "scenario": fixture.scenario,
                "mutations": task_2_claude_activity_projection(fixture),
            })
        })
        .collect::<Vec<_>>();

    assert_eq!(
        actual, expected,
        "Task 2 must replace the empty projection seam with Claude activity tracker output"
    );
}

#[test]
fn claude_activity_tracker_rejects_uncorrelated_foreign_sessions_and_accepts_known_actor() {
    let mut tracker = ClaudeActivityFixtureAdapter::new("session-root");
    let foreign_start = json!({
        "hook_event_name": "SubagentStart",
        "session_id": "foreign-session",
        "agent_id": "foreign-agent",
        "agent_type": "Explore",
        "transcript_path": "/tmp/foreign-session.jsonl",
        "cwd": "/workspace"
    });
    assert!(
        tracker
            .handle_value(ClaudeActivityInputSource::HookInput, &foreign_start, 1_000,)
            .mutations
            .is_empty()
    );

    let root_start = json!({
        "hook_event_name": "SubagentStart",
        "session_id": "session-root",
        "agent_id": "known-agent",
        "agent_type": "Explore",
        "transcript_path": "/tmp/session-root.jsonl",
        "cwd": "/workspace"
    });
    let started = tracker.handle_value(ClaudeActivityInputSource::HookInput, &root_start, 2_000);
    assert_eq!(started.mutations.len(), 2);

    let child_tool = json!({
        "hook_event_name": "PreToolUse",
        "session_id": "child-session",
        "agent_id": "known-agent",
        "agent_type": "Explore",
        "tool_name": "Read",
        "tool_use_id": "known-tool",
        "tool_input": {"file_path": "/redacted/file.rs"},
        "transcript_path": "/tmp/session-root.jsonl",
        "cwd": "/workspace"
    });
    assert_eq!(
        tracker
            .handle_value(ClaudeActivityInputSource::HookInput, &child_tool, 3_000,)
            .mutations
            .len(),
        1
    );

    let unknown_child_tool = json!({
        "hook_event_name": "PreToolUse",
        "session_id": "child-session",
        "agent_id": "unknown-agent",
        "agent_type": "Explore",
        "tool_name": "Read",
        "tool_use_id": "unknown-tool",
        "tool_input": {"file_path": "/redacted/file.rs"},
        "transcript_path": "/tmp/session-root.jsonl",
        "cwd": "/workspace"
    });
    assert!(
        tracker
            .handle_value(
                ClaudeActivityInputSource::HookInput,
                &unknown_child_tool,
                4_000,
            )
            .mutations
            .is_empty()
    );
}

#[test]
fn transcript_recovery_request_requires_authenticated_correlated_child_path() {
    let mut runtime =
        ClaudeProviderRuntime::new("thread-recovery".to_owned(), "session-root".to_owned());
    let child_transcript_path = std::env::temp_dir().join("child-agent.jsonl");
    let child_stop = json!({
        "hook_event_name": "SubagentStop",
        "session_id": "session-root",
        "agent_id": "agent-recovery",
        "agent_type": "Explore",
        "transcript_path": "/private/main-session.jsonl",
        "agent_transcript_path": child_transcript_path,
        "last_assistant_message": "done"
    });

    assert!(
        runtime
            .handle_raw_value(&child_stop, 1_000)
            .recovery_request_metadata()
            .is_none(),
        "unauthenticated stream-shaped hook input must never schedule transcript access"
    );

    runtime.handle_raw_value(
        &json!({
            "hook_event_name": "SubagentStart",
            "session_id": "session-root",
            "agent_id": "agent-recovery",
            "agent_type": "Explore"
        }),
        2_000,
    );
    let output = runtime.handle_authenticated_hook_value(&child_stop, 3_000);
    let request = output
        .recovery_request_metadata()
        .expect("authenticated correlated SubagentStop child path");
    assert_eq!(request.root_session_id, "session-root");
    assert_eq!(request.agent_id, "agent-recovery");
    assert_eq!(request.agent_type, "Explore");
    assert!(request.has_child_path);

    let main_only = json!({
        "hook_event_name": "SubagentStop",
        "session_id": "session-root",
        "agent_id": "agent-recovery",
        "agent_type": "Explore",
        "transcript_path": "/private/main-session.jsonl"
    });
    assert!(
        runtime
            .handle_authenticated_hook_value(&main_only, 4_000)
            .recovery_request_metadata()
            .is_none(),
        "the BaseHookInput main transcript path is not a child recovery target"
    );

    let mut relative_runtime =
        ClaudeProviderRuntime::new("thread-relative".to_owned(), "session-root".to_owned());
    relative_runtime.handle_raw_value(
        &json!({
            "hook_event_name":"SubagentStart",
            "session_id":"session-root",
            "agent_id":"agent-relative",
            "agent_type":"Explore"
        }),
        5_000,
    );
    assert!(
        relative_runtime
            .handle_authenticated_hook_value(
                &json!({
                    "hook_event_name":"SubagentStop",
                    "session_id":"session-root",
                    "agent_id":"agent-relative",
                    "agent_type":"Explore",
                    "agent_transcript_path":"relative-child.jsonl"
                }),
                6_000,
            )
            .recovery_request_metadata()
            .is_none(),
        "recovery paths must be absolute before canonicalization"
    );
}

#[test]
fn disabled_agent_activity_bypasses_hooks_and_restarts_correlation_on_enable() {
    let mut runtime =
        ClaudeProviderRuntime::new("thread-toggle".to_owned(), "session-root".to_owned());
    let agent_transcript_path = std::env::temp_dir().join("agent-toggle.jsonl");
    let hook = json!({
        "hook_event_name": "SubagentStart",
        "session_id": "session-root",
        "agent_id": "agent-toggle",
        "agent_type": "Explore"
    });
    let initial = runtime.handle_authenticated_hook_value(&hook, 1_000);
    assert!(
        !initial.activity.is_empty(),
        "initial hook establishes activity"
    );
    let stop_hook = json!({
        "hook_event_name": "SubagentStop",
        "session_id": "session-root",
        "agent_id": "agent-toggle",
        "agent_type": "Explore",
        "agent_transcript_path": agent_transcript_path
    });
    let stale_recovery = runtime
        .handle_authenticated_hook_value(&stop_hook, 1_500)
        .recovery_request_metadata()
        .expect("pre-disable recovery request");

    runtime.set_agent_activity_enabled(false);
    let disabled = runtime.handle_authenticated_hook_value(&hook, 2_000);
    assert!(disabled.activity.is_empty());
    assert!(disabled.recovery_request_metadata().is_none());
    let normal = runtime.handle_raw_value(
        &json!({
            "type": "stream_event",
            "parent_tool_use_id": null,
            "session_id": "session-root",
            "uuid": "normal-chat-continues",
            "event": {
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "text_delta", "text": "normal chat continues"}
            }
        }),
        2_100,
    );
    assert!(!normal.events.is_empty(), "normal Claude chat remains live");

    runtime.set_agent_activity_enabled(true);
    let resumed = runtime.handle_authenticated_hook_value(&hook, 3_000);
    assert!(
        !resumed.activity.is_empty(),
        "the same authenticated hook is accepted in the new correlation epoch"
    );
    let current_recovery = runtime
        .handle_authenticated_hook_value(&stop_hook, 3_100)
        .recovery_request_metadata()
        .expect("new-generation recovery request");
    assert_ne!(
        stale_recovery.generation, current_recovery.generation,
        "a recovery response started before disable cannot match the resumed epoch"
    );
    assert!(
        current_recovery.not_before_unix_nanos > stale_recovery.not_before_unix_nanos,
        "the resumed epoch carries a later provider-history cutoff"
    );

    let transcript = [
        r#"{"type":"assistant","sessionId":"session-root","agentId":"agent-toggle","isSidechain":true,"uuid":"disabled-message","timestamp":"2026-07-30T12:00:00Z","message":{"role":"assistant","content":[{"type":"text","text":"disabled-history-backfill"}]}}"#,
        r#"{"type":"assistant","sessionId":"session-root","agentId":"agent-toggle","isSidechain":true,"uuid":"resumed-message","timestamp":"2026-07-30T12:00:02Z","message":{"role":"assistant","content":[{"type":"text","text":"resumed-history-entry"}]}}"#,
    ]
    .join("\n");
    let recovered = ClaudeTranscriptFixtureAdapter::recover_since(
        "session-root",
        "agent-toggle",
        "Explore",
        transcript.as_bytes(),
        "2026-07-30T12:00:01Z",
    );
    assert!(recovered.mutations.iter().all(|mutation| !matches!(
        mutation,
        ProviderActivityMutation::AppendEntry(entry)
            if entry.detail.as_deref() == Some("disabled-history-backfill")
    )));
    assert!(recovered.mutations.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::AppendEntry(entry)
            if entry.detail.as_deref() == Some("resumed-history-entry")
    )));
}

#[test]
fn transcript_recovery_parser_accepts_only_correlated_sidechain_activity_records() {
    let transcript = [
        "{malformed}",
        r#"{"type":"assistant","sessionId":"session-root","agentId":"agent-recovery","isSidechain":true,"uuid":"message-1","timestamp":"2026-07-24T12:00:00Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"private chain"},{"type":"text","text":"Recovered commentary"},{"type":"tool_use","id":"tool-1","name":"Read","input":{"file_path":"/must-not-leak"}}]}}"#,
        r#"{"type":"assistant","sessionId":"wrong-root","agentId":"agent-recovery","isSidechain":true,"uuid":"wrong-session","timestamp":"2026-07-24T12:00:01Z","message":{"role":"assistant","content":[{"type":"text","text":"wrong session"}]}}"#,
        r#"{"type":"assistant","sessionId":"session-root","agentId":"wrong-agent","isSidechain":true,"uuid":"wrong-agent","timestamp":"2026-07-24T12:00:02Z","message":{"role":"assistant","content":[{"type":"text","text":"wrong actor"}]}}"#,
        r#"{"type":"assistant","sessionId":"session-root","agentId":"agent-recovery","isSidechain":false,"uuid":"not-sidechain","timestamp":"2026-07-24T12:00:03Z","message":{"role":"assistant","content":[{"type":"text","text":"root text"}]}}"#,
        r#"{"type":"user","sessionId":"session-root","agentId":"agent-recovery","isSidechain":true,"uuid":"message-2","timestamp":"2026-07-24T12:00:04Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool-1","content":"ok","is_error":false}]}}"#,
        r#"{"type":"assistant","sessionId":"session-root","agentId":"agent-recovery","isSidechain":true,"uuid":"truncated""#,
    ]
    .join("\n");

    let output = ClaudeTranscriptFixtureAdapter::recover(
        "session-root",
        "agent-recovery",
        "Explore",
        transcript.as_bytes(),
    );

    assert!(output.correlation_validated);
    assert_eq!(output.scanned_bytes, transcript.len());
    let entries = output
        .mutations
        .into_iter()
        .filter_map(|mutation| match mutation {
            ProviderActivityMutation::AppendEntry(entry) => Some(entry),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 3);
    assert_eq!(
        entries[0].kind,
        coddy_studio_server::activity::ActivityEntryKind::Commentary
    );
    assert_eq!(entries[0].detail.as_deref(), Some("Recovered commentary"));
    assert_eq!(
        entries[1].kind,
        coddy_studio_server::activity::ActivityEntryKind::Tool
    );
    assert_eq!(
        entries[2].tone,
        coddy_studio_server::activity::ActivityEntryTone::Success
    );
    assert!(
        entries
            .iter()
            .all(|entry| !format!("{entry:?}").contains("must-not-leak"))
    );
}

#[test]
fn transcript_recovery_tail_and_actor_entry_count_are_bounded() {
    let valid = r#"{"type":"assistant","sessionId":"session-root","agentId":"agent-bounded","isSidechain":true,"uuid":"tail-message","timestamp":"2026-07-24T12:00:00Z","message":{"role":"assistant","content":[{"type":"text","text":"tail commentary"}]}}"#;
    let mut oversized = vec![b'x'; 10 * 1024 * 1024 + 4_096];
    oversized.push(b'\n');
    oversized.extend_from_slice(valid.as_bytes());
    let tail = ClaudeTranscriptFixtureAdapter::recover(
        "session-root",
        "agent-bounded",
        "Explore",
        &oversized,
    );
    assert!(tail.correlation_validated);
    assert!(tail.scanned_bytes <= 10 * 1024 * 1024);
    assert_eq!(
        tail.mutations
            .iter()
            .filter(|mutation| matches!(mutation, ProviderActivityMutation::AppendEntry(_)))
            .count(),
        1
    );

    let content = (0..250)
        .map(|index| json!({"type":"text","text":format!("entry-{index}")}))
        .collect::<Vec<_>>();
    let transcript = json!({
        "type": "assistant",
        "sessionId": "session-root",
        "agentId": "agent-bounded",
        "isSidechain": true,
        "uuid": "many-entries",
        "timestamp": "2026-07-24T12:00:00Z",
        "message": {"role":"assistant","content":content}
    })
    .to_string();
    let bounded = ClaudeTranscriptFixtureAdapter::recover(
        "session-root",
        "agent-bounded",
        "Explore",
        transcript.as_bytes(),
    );
    assert_eq!(
        bounded
            .mutations
            .iter()
            .filter(|mutation| matches!(mutation, ProviderActivityMutation::AppendEntry(_)))
            .count(),
        200
    );
}

#[test]
fn transcript_recovery_tool_lifecycle_ids_deduplicate_in_both_delivery_orders() {
    let transcript = [
        r#"{"type":"assistant","sessionId":"session-root","agentId":"agent-dedup","isSidechain":true,"uuid":"message-tool","timestamp":"2026-07-24T12:00:00Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"tool-shared","name":"Read","input":{"file_path":"/redacted"}}]}}"#,
        r#"{"type":"user","sessionId":"session-root","agentId":"agent-dedup","isSidechain":true,"uuid":"message-result","timestamp":"2026-07-24T12:00:01Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool-shared","content":"ok","is_error":false}]}}"#,
    ]
    .join("\n");
    let start = json!({
        "hook_event_name":"SubagentStart",
        "session_id":"session-root",
        "agent_id":"agent-dedup",
        "agent_type":"Explore"
    });
    let pre = json!({
        "hook_event_name":"PreToolUse",
        "session_id":"session-root",
        "agent_id":"agent-dedup",
        "agent_type":"Explore",
        "tool_name":"Read",
        "tool_use_id":"tool-shared"
    });
    let post = json!({
        "hook_event_name":"PostToolUse",
        "session_id":"session-root",
        "agent_id":"agent-dedup",
        "agent_type":"Explore",
        "tool_name":"Read",
        "tool_use_id":"tool-shared"
    });

    let mut live_first = ClaudeTranscriptFixtureAdapter::new("session-root");
    live_first.handle_hook(&start, 1_000);
    let live_ids = [(&pre, 2_000), (&post, 3_000)]
        .into_iter()
        .flat_map(|(value, timestamp)| live_first.handle_hook(value, timestamp).mutations)
        .filter_map(|mutation| match mutation {
            ProviderActivityMutation::AppendEntry(entry) => Some(entry.id),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(live_ids.len(), 2);
    assert!(
        live_first
            .recover_bytes("agent-dedup", "Explore", transcript.as_bytes())
            .mutations
            .is_empty(),
        "recovery after live must be idempotent"
    );

    let mut recovery_first = ClaudeTranscriptFixtureAdapter::new("session-root");
    recovery_first.handle_hook(&start, 1_000);
    let recovered_ids = recovery_first
        .recover_bytes("agent-dedup", "Explore", transcript.as_bytes())
        .mutations
        .into_iter()
        .filter_map(|mutation| match mutation {
            ProviderActivityMutation::AppendEntry(entry) => Some(entry.id),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(recovered_ids, live_ids);
    assert!(
        recovery_first.handle_hook(&pre, 2_000).mutations.is_empty()
            && recovery_first
                .handle_hook(&post, 3_000)
                .mutations
                .is_empty(),
        "live after recovery must be idempotent"
    );
}

#[test]
fn transcript_recovery_partial_tool_start_completes_from_live_success_without_reopening_actor() {
    let partial_transcript = r#"{"type":"assistant","sessionId":"session-root","agentId":"agent-partial-success","isSidechain":true,"uuid":"message-tool","timestamp":"2026-07-24T12:00:00Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"tool-partial-success","name":"Read","input":{"file_path":"/redacted"}}]}}"#;
    let complete_transcript = [
        partial_transcript,
        r#"{"type":"user","sessionId":"session-root","agentId":"agent-partial-success","isSidechain":true,"uuid":"message-result","timestamp":"2026-07-24T12:00:01Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool-partial-success","content":"ok","is_error":false}]}}"#,
    ]
    .join("\n");
    let expected_ids = ClaudeTranscriptFixtureAdapter::recover(
        "session-root",
        "agent-partial-success",
        "Explore",
        complete_transcript.as_bytes(),
    )
    .mutations
    .into_iter()
    .filter_map(|mutation| match mutation {
        ProviderActivityMutation::AppendEntry(entry) => Some(entry.id),
        _ => None,
    })
    .collect::<Vec<_>>();
    assert_eq!(expected_ids.len(), 2);

    let mut adapter = ClaudeTranscriptFixtureAdapter::new("session-root");
    for hook in [
        json!({
            "hook_event_name":"SubagentStart",
            "session_id":"session-root",
            "agent_id":"agent-partial-success",
            "agent_type":"Explore"
        }),
        json!({
            "hook_event_name":"SubagentStop",
            "session_id":"session-root",
            "agent_id":"agent-partial-success",
            "agent_type":"Explore"
        }),
    ] {
        adapter.handle_hook(&hook, 1_000);
    }
    let recovered = adapter.recover_bytes(
        "agent-partial-success",
        "Explore",
        partial_transcript.as_bytes(),
    );
    assert!(
        recovered
            .mutations
            .iter()
            .all(|mutation| matches!(mutation, ProviderActivityMutation::AppendEntry(_)))
    );
    assert!(matches!(
        recovered.mutations.as_slice(),
        [ProviderActivityMutation::AppendEntry(entry)] if entry.id == expected_ids[0]
    ));

    let pre = json!({
        "hook_event_name":"PreToolUse",
        "session_id":"session-root",
        "agent_id":"agent-partial-success",
        "agent_type":"Explore",
        "tool_name":"Read",
        "tool_use_id":"tool-partial-success"
    });
    assert!(adapter.handle_hook(&pre, 2_000).mutations.is_empty());
    let post = json!({
        "hook_event_name":"PostToolUse",
        "session_id":"session-root",
        "agent_id":"agent-partial-success",
        "agent_type":"Explore",
        "tool_name":"Read",
        "tool_use_id":"tool-partial-success"
    });
    let completed = adapter.handle_hook(&post, 3_000);
    assert!(matches!(
        completed.mutations.as_slice(),
        [ProviderActivityMutation::AppendEntry(entry)]
            if entry.id == expected_ids[1] && entry.title == "Read completed"
    ));
    assert!(
        adapter.handle_hook(&post, 3_000).mutations.is_empty(),
        "the live terminal hook remains idempotent"
    );
}

#[test]
fn transcript_recovery_partial_tool_start_completes_from_live_failure_without_reopening_actor() {
    let partial_transcript = r#"{"type":"assistant","sessionId":"session-root","agentId":"agent-partial-failure","isSidechain":true,"uuid":"message-tool","timestamp":"2026-07-24T12:00:00Z","message":{"role":"assistant","content":[{"type":"tool_use","id":"tool-partial-failure","name":"Bash","input":{"command":"false"}}]}}"#;
    let complete_transcript = [
        partial_transcript,
        r#"{"type":"user","sessionId":"session-root","agentId":"agent-partial-failure","isSidechain":true,"uuid":"message-result","timestamp":"2026-07-24T12:00:01Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tool-partial-failure","content":"command failed","is_error":true}]}}"#,
    ]
    .join("\n");
    let expected_ids = ClaudeTranscriptFixtureAdapter::recover(
        "session-root",
        "agent-partial-failure",
        "Explore",
        complete_transcript.as_bytes(),
    )
    .mutations
    .into_iter()
    .filter_map(|mutation| match mutation {
        ProviderActivityMutation::AppendEntry(entry) => Some(entry.id),
        _ => None,
    })
    .collect::<Vec<_>>();
    assert_eq!(expected_ids.len(), 2);

    let mut adapter = ClaudeTranscriptFixtureAdapter::new("session-root");
    for hook in [
        json!({
            "hook_event_name":"SubagentStart",
            "session_id":"session-root",
            "agent_id":"agent-partial-failure",
            "agent_type":"Explore"
        }),
        json!({
            "hook_event_name":"SubagentStop",
            "session_id":"session-root",
            "agent_id":"agent-partial-failure",
            "agent_type":"Explore"
        }),
    ] {
        adapter.handle_hook(&hook, 1_000);
    }
    let recovered = adapter.recover_bytes(
        "agent-partial-failure",
        "Explore",
        partial_transcript.as_bytes(),
    );
    assert!(matches!(
        recovered.mutations.as_slice(),
        [ProviderActivityMutation::AppendEntry(entry)] if entry.id == expected_ids[0]
    ));

    let post = json!({
        "hook_event_name":"PostToolUseFailure",
        "session_id":"session-root",
        "agent_id":"agent-partial-failure",
        "agent_type":"Explore",
        "tool_name":"Bash",
        "tool_use_id":"tool-partial-failure",
        "error":"command failed"
    });
    let failed = adapter.handle_hook(&post, 3_000);
    assert!(matches!(
        failed.mutations.as_slice(),
        [ProviderActivityMutation::AppendEntry(entry)]
            if entry.id == expected_ids[1]
                && entry.title == "Bash failed"
                && entry.detail.as_deref() == Some("command failed")
    ));
    assert!(
        failed
            .mutations
            .iter()
            .all(|mutation| !matches!(mutation, ProviderActivityMutation::UpsertActor(_))),
        "late terminal delivery must not reopen the terminal actor"
    );
}

#[test]
fn transcript_recovery_saturation_never_orphans_the_513th_partial_tool_start() {
    let mut adapter = ClaudeTranscriptFixtureAdapter::new("session-root");
    let transcript = |agent_id: &str, first: usize, count: usize| {
        json!({
            "type":"assistant",
            "sessionId":"session-root",
            "agentId":agent_id,
            "isSidechain":true,
            "uuid":format!("message-{agent_id}"),
            "timestamp":"2026-07-24T12:00:00Z",
            "message":{
                "role":"assistant",
                "content":(first..first + count)
                    .map(|index| json!({
                        "type":"tool_use",
                        "id":format!("tool-{index}"),
                        "name":"Read",
                        "input":{"file_path":"/redacted"}
                    }))
                    .collect::<Vec<_>>()
            }
        })
        .to_string()
    };
    for (agent_id, first, count) in [
        ("agent-capacity-a", 0, 200),
        ("agent-capacity-b", 200, 200),
        ("agent-capacity-c", 400, 112),
    ] {
        let recovered = adapter.recover_bytes(
            agent_id,
            "Explore",
            transcript(agent_id, first, count).as_bytes(),
        );
        assert_eq!(
            recovered
                .mutations
                .iter()
                .filter(|mutation| matches!(mutation, ProviderActivityMutation::AppendEntry(_)))
                .count(),
            count
        );
    }

    let overflow_transcript = transcript("agent-capacity-overflow", 512, 1);
    assert!(
        adapter
            .recover_bytes(
                "agent-capacity-overflow",
                "Explore",
                overflow_transcript.as_bytes(),
            )
            .mutations
            .is_empty(),
        "an untracked recovered start must not be emitted or deduplicated"
    );

    let first_terminal = adapter.handle_hook(
        &json!({
            "hook_event_name":"PostToolUse",
            "session_id":"session-root",
            "agent_id":"agent-capacity-a",
            "agent_type":"Explore",
            "tool_name":"Read",
            "tool_use_id":"tool-0"
        }),
        2_000,
    );
    assert!(matches!(
        first_terminal.mutations.as_slice(),
        [ProviderActivityMutation::AppendEntry(entry)] if entry.title == "Read completed"
    ));

    let admitted = adapter.recover_bytes(
        "agent-capacity-overflow",
        "Explore",
        overflow_transcript.as_bytes(),
    );
    assert!(matches!(
        admitted.mutations.as_slice(),
        [ProviderActivityMutation::AppendEntry(entry)] if entry.title == "Read started"
    ));
    let overflow_terminal = adapter.handle_hook(
        &json!({
            "hook_event_name":"PostToolUseFailure",
            "session_id":"session-root",
            "agent_id":"agent-capacity-overflow",
            "agent_type":"Explore",
            "tool_name":"Read",
            "tool_use_id":"tool-512",
            "error":"not found"
        }),
        3_000,
    );
    assert!(matches!(
        overflow_terminal.mutations.as_slice(),
        [ProviderActivityMutation::AppendEntry(entry)]
            if entry.title == "Read failed" && entry.detail.as_deref() == Some("not found")
    ));
}

#[test]
fn transcript_recovery_parentage_uses_only_explicit_parent_agent_identity() {
    let mut tracker = ClaudeActivityFixtureAdapter::new("session-root");
    let explicit = tracker.handle_value(
        ClaudeActivityInputSource::HookInput,
        &json!({
            "hook_event_name":"SubagentStart",
            "session_id":"session-root",
            "agent_id":"nested-agent",
            "agent_type":"Explore",
            "parent_agent_id":"parent-agent"
        }),
        1_000,
    );
    assert!(matches!(
        explicit.mutations.first(),
        Some(ProviderActivityMutation::UpsertActor(actor))
            if actor.parent_actor_id.as_deref() == Some("claude:agent:parent-agent")
    ));

    let transcript = r#"{"type":"assistant","sessionId":"session-root","agentId":"lineage-agent","isSidechain":true,"uuid":"child-message","parentUuid":"parent-message-not-actor","timestamp":"2026-07-24T12:00:00Z","message":{"role":"assistant","content":[{"type":"text","text":"child commentary"}]}}"#;
    let recovered = ClaudeTranscriptFixtureAdapter::recover(
        "session-root",
        "lineage-agent",
        "Explore",
        transcript.as_bytes(),
    );
    assert!(
        recovered.mutations.iter().all(|mutation| !matches!(
            mutation,
            ProviderActivityMutation::UpsertActor(actor)
                if actor.parent_actor_id.as_deref()
                    == Some("claude:agent:parent-message-not-actor")
        )),
        "transcript parentUuid is message lineage, never actor lineage"
    );
}

#[test]
fn claude_activity_tracker_keeps_interleaved_same_type_tool_lifecycles_distinct() {
    let mut tracker = ClaudeActivityFixtureAdapter::new("session-root");
    for agent_id in ["agent-a", "agent-b"] {
        let start = json!({
            "hook_event_name": "SubagentStart",
            "session_id": "session-root",
            "agent_id": agent_id,
            "agent_type": "Explore",
            "transcript_path": "/tmp/session-root.jsonl",
            "cwd": "/workspace"
        });
        tracker.handle_value(ClaudeActivityInputSource::HookInput, &start, 1_000);
    }
    for (agent_id, tool_use_id) in [("agent-a", "tool-a"), ("agent-b", "tool-b")] {
        let pre = json!({
            "hook_event_name": "PreToolUse",
            "session_id": "session-root",
            "agent_id": agent_id,
            "agent_type": "Explore",
            "tool_name": "Read",
            "tool_use_id": tool_use_id,
            "tool_input": {"file_path": "/redacted/file.rs"},
            "transcript_path": "/tmp/session-root.jsonl",
            "cwd": "/workspace"
        });
        tracker.handle_value(ClaudeActivityInputSource::HookInput, &pre, 2_000);
    }

    let mismatched_post = json!({
        "hook_event_name": "PostToolUse",
        "session_id": "session-root",
        "agent_id": "agent-b",
        "agent_type": "Explore",
        "tool_name": "Read",
        "tool_use_id": "tool-a",
        "tool_input": {"file_path": "/redacted/file.rs"},
        "tool_response": {},
        "transcript_path": "/tmp/session-root.jsonl",
        "cwd": "/workspace"
    });
    assert!(
        tracker
            .handle_value(
                ClaudeActivityInputSource::HookInput,
                &mismatched_post,
                3_000,
            )
            .mutations
            .is_empty()
    );

    let matching_post = json!({
        "hook_event_name": "PostToolUse",
        "session_id": "session-root",
        "agent_id": "agent-a",
        "agent_type": "Explore",
        "tool_name": "Read",
        "tool_use_id": "tool-a",
        "tool_input": {"file_path": "/redacted/file.rs"},
        "tool_response": {},
        "transcript_path": "/tmp/session-root.jsonl",
        "cwd": "/workspace"
    });
    let output = tracker.handle_value(ClaudeActivityInputSource::HookInput, &matching_post, 4_000);
    assert!(matches!(
        output.mutations.as_slice(),
        [ProviderActivityMutation::AppendEntry(entry)]
            if entry.owner_id == "claude:agent:agent-a"
                && entry.id.contains("tool-a")
    ));
}

#[test]
fn claude_activity_tracker_bounds_state_and_uses_collision_safe_canonical_ids() {
    let mut tracker = ClaudeActivityFixtureAdapter::new("session-root");
    for index in 0..320 {
        let start = json!({
            "hook_event_name": "SubagentStart",
            "session_id": "session-root",
            "agent_id": format!("agent-{index}"),
            "agent_type": "Explore",
            "transcript_path": "/tmp/session-root.jsonl",
            "cwd": "/workspace"
        });
        tracker.handle_value(ClaudeActivityInputSource::HookInput, &start, 1_000 + index);
    }
    for index in 0..640 {
        let pre = json!({
            "hook_event_name": "PreToolUse",
            "session_id": "session-root",
            "agent_id": format!("agent-{}", index % 320),
            "agent_type": "Explore",
            "tool_name": "Read",
            "tool_use_id": format!("tool-{index}"),
            "tool_input": {"file_path": "/redacted/file.rs"},
            "transcript_path": "/tmp/session-root.jsonl",
            "cwd": "/workspace"
        });
        tracker.handle_value(ClaudeActivityInputSource::HookInput, &pre, 2_000 + index);
    }
    let counts = tracker.state_counts();
    assert!(counts.actors <= 256);
    assert!(counts.tool_lifecycles <= 512);
    assert!(counts.seen_events <= 2_048);

    let mut tracker = ClaudeActivityFixtureAdapter::new("session-root");
    let first = json!({
        "hook_event_name": "PreToolUse",
        "session_id": "session-root",
        "agent_id": "agent:delimiter",
        "agent_type": "Explore",
        "tool_name": "Read",
        "tool_use_id": "tool",
        "tool_input": {"file_path": "/redacted/file.rs"},
        "transcript_path": "/tmp/session-root.jsonl",
        "cwd": "/workspace"
    });
    let second = json!({
        "hook_event_name": "PreToolUse",
        "session_id": "session-root",
        "agent_id": "agent",
        "agent_type": "Explore",
        "tool_name": "Read",
        "tool_use_id": "delimiter:tool",
        "tool_input": {"file_path": "/redacted/file.rs"},
        "transcript_path": "/tmp/session-root.jsonl",
        "cwd": "/workspace"
    });
    for agent_id in ["agent:delimiter", "agent"] {
        let start = json!({
            "hook_event_name": "SubagentStart",
            "session_id": "session-root",
            "agent_id": agent_id,
            "agent_type": "Explore",
            "transcript_path": "/tmp/session-root.jsonl",
            "cwd": "/workspace"
        });
        tracker.handle_value(ClaudeActivityInputSource::HookInput, &start, 3_000);
    }
    let first_id = tracker
        .handle_value(ClaudeActivityInputSource::HookInput, &first, 3_001)
        .mutations
        .into_iter()
        .find_map(|mutation| match mutation {
            ProviderActivityMutation::AppendEntry(entry) => Some(entry.id),
            _ => None,
        })
        .expect("first entry");
    let second_id = tracker
        .handle_value(ClaudeActivityInputSource::HookInput, &second, 3_002)
        .mutations
        .into_iter()
        .find_map(|mutation| match mutation {
            ProviderActivityMutation::AppendEntry(entry) => Some(entry.id),
            _ => None,
        })
        .expect("second entry");
    assert_ne!(first_id, second_id);
    assert!(first_id.len() <= 256);
    assert!(second_id.len() <= 256);
}

#[test]
fn claude_activity_tracker_clips_safe_command_display_and_omits_sensitive_payloads() {
    let mut tracker = ClaudeActivityFixtureAdapter::new("session-root");
    let start = json!({
        "hook_event_name": "SubagentStart",
        "session_id": "session-root",
        "agent_id": "command-agent",
        "agent_type": "Bash",
        "transcript_path": "/tmp/session-root.jsonl",
        "cwd": "/workspace"
    });
    tracker.handle_value(ClaudeActivityInputSource::HookInput, &start, 1_000);

    let safe_command = format!(
        "  printf '{}'\n",
        "x".repeat(ACTIVITY_DETAIL_MAX_LENGTH * 2)
    );
    let safe = json!({
        "hook_event_name": "PreToolUse",
        "session_id": "session-root",
        "agent_id": "command-agent",
        "agent_type": "Bash",
        "tool_name": "Bash",
        "tool_use_id": "safe-command",
        "tool_input": {"command": safe_command},
        "transcript_path": "/tmp/session-root.jsonl",
        "cwd": "/workspace"
    });
    let detail = tracker
        .handle_value(ClaudeActivityInputSource::HookInput, &safe, 2_000)
        .mutations
        .into_iter()
        .find_map(|mutation| match mutation {
            ProviderActivityMutation::AppendEntry(entry) => entry.detail,
            _ => None,
        })
        .expect("safe command detail");
    assert!(detail.len() <= ACTIVITY_DETAIL_MAX_LENGTH);
    assert!(detail.starts_with("printf"));
    assert!(!detail.ends_with('\n'));
    assert!(std::str::from_utf8(detail.as_bytes()).is_ok());

    let sensitive = json!({
        "hook_event_name": "PreToolUse",
        "session_id": "session-root",
        "agent_id": "command-agent",
        "agent_type": "Bash",
        "tool_name": "Bash",
        "tool_use_id": "sensitive-command",
        "tool_input": {"command": "echo API_TOKEN=secret-value"},
        "transcript_path": "/tmp/session-root.jsonl",
        "cwd": "/workspace"
    });
    let sensitive_entry = tracker
        .handle_value(ClaudeActivityInputSource::HookInput, &sensitive, 3_000)
        .mutations
        .into_iter()
        .find_map(|mutation| match mutation {
            ProviderActivityMutation::AppendEntry(entry) => Some(entry),
            _ => None,
        })
        .expect("sensitive command entry");
    assert_eq!(sensitive_entry.detail, None);

    let sensitive_error_pre = json!({
        "hook_event_name": "PreToolUse",
        "session_id": "session-root",
        "agent_id": "command-agent",
        "agent_type": "Bash",
        "tool_name": "Read",
        "tool_use_id": "sensitive-error",
        "tool_input": {"file_path": "/redacted/file.rs"}
    });
    tracker.handle_value(
        ClaudeActivityInputSource::HookInput,
        &sensitive_error_pre,
        4_000,
    );
    let sensitive_error = json!({
        "hook_event_name": "PostToolUseFailure",
        "session_id": "session-root",
        "agent_id": "command-agent",
        "agent_type": "Bash",
        "tool_name": "Read",
        "tool_use_id": "sensitive-error",
        "tool_input": {"file_path": "/redacted/file.rs"},
        "error": "permission failed with API_TOKEN=secret-value"
    });
    let sensitive_error_entry = tracker
        .handle_value(
            ClaudeActivityInputSource::HookInput,
            &sensitive_error,
            5_000,
        )
        .mutations
        .into_iter()
        .find_map(|mutation| match mutation {
            ProviderActivityMutation::AppendEntry(entry) => Some(entry),
            _ => None,
        })
        .expect("sensitive error entry");
    assert_eq!(sensitive_error_entry.detail, None);

    for (index, command) in [
        "AWS_ACCESS_KEY_ID=AKIAEXAMPLE cargo test",
        "DATABASE_URL=postgres://user:pass@db.example/app migrate",
        "curl -u user:pass https://example.test",
        "GH_PAT=ghp_examplevalue gh auth login",
        "foo=abc123 deploy",
        "Mixed_Case=abc123 deploy",
    ]
    .into_iter()
    .enumerate()
    {
        let sensitive = json!({
            "hook_event_name": "PreToolUse",
            "session_id": "session-root",
            "agent_id": "command-agent",
            "agent_type": "Bash",
            "tool_name": "Bash",
            "tool_use_id": format!("adversarial-sensitive-command-{index}"),
            "tool_input": {"command": command}
        });
        let entry = tracker
            .handle_value(
                ClaudeActivityInputSource::HookInput,
                &sensitive,
                6_000 + index as u64,
            )
            .mutations
            .into_iter()
            .find_map(|mutation| match mutation {
                ProviderActivityMutation::AppendEntry(entry) => Some(entry),
                _ => None,
            })
            .expect("sensitive command entry");
        assert_eq!(entry.detail, None, "command leaked: {command}");
    }

    let mut summary_tracker = ClaudeActivityFixtureAdapter::new("session-root");
    summary_tracker.handle_value(
        ClaudeActivityInputSource::HookInput,
        &json!({
            "hook_event_name": "SubagentStart",
            "session_id": "session-root",
            "agent_id": "summary-agent",
            "agent_type": "Explore"
        }),
        1_000,
    );
    let stopped = summary_tracker.handle_value(
        ClaudeActivityInputSource::HookInput,
        &json!({
            "hook_event_name": "SubagentStop",
            "session_id": "session-root",
            "agent_id": "summary-agent",
            "agent_type": "Explore",
            "last_assistant_message": "Deployed with GH_PAT=ghp_examplevalue"
        }),
        2_000,
    );
    assert!(matches!(
        stopped.mutations.as_slice(),
        [ProviderActivityMutation::UpsertActor(actor)] if actor.summary.is_none()
    ));

    let mut assignment_tracker = ClaudeActivityFixtureAdapter::new("session-root");
    assignment_tracker.handle_value(
        ClaudeActivityInputSource::HookInput,
        &json!({
            "hook_event_name": "SubagentStart",
            "session_id": "session-root",
            "agent_id": "assignment-agent",
            "agent_type": "Explore"
        }),
        1_000,
    );
    assignment_tracker.handle_value(
        ClaudeActivityInputSource::HookInput,
        &json!({
            "hook_event_name": "PreToolUse",
            "session_id": "session-root",
            "agent_id": "assignment-agent",
            "tool_name": "Read",
            "tool_use_id": "assignment-error",
            "tool_input": {}
        }),
        2_000,
    );
    let failed = assignment_tracker.handle_value(
        ClaudeActivityInputSource::HookInput,
        &json!({
            "hook_event_name": "PostToolUseFailure",
            "session_id": "session-root",
            "agent_id": "assignment-agent",
            "tool_name": "Read",
            "tool_use_id": "assignment-error",
            "tool_input": {},
            "error": "foo=abc123 deploy"
        }),
        3_000,
    );
    assert!(matches!(
        failed.mutations.as_slice(),
        [ProviderActivityMutation::AppendEntry(entry)] if entry.detail.is_none()
    ));
    let stopped = assignment_tracker.handle_value(
        ClaudeActivityInputSource::HookInput,
        &json!({
            "hook_event_name": "SubagentStop",
            "session_id": "session-root",
            "agent_id": "assignment-agent",
            "agent_type": "Explore",
            "last_assistant_message": "mixedCase=abc123 deploy"
        }),
        4_000,
    );
    assert!(matches!(
        stopped.mutations.as_slice(),
        [ProviderActivityMutation::UpsertActor(actor)] if actor.summary.is_none()
    ));
}

#[test]
fn claude_activity_tracker_clips_tool_labels_without_breaking_lifecycle_correlation() {
    let mut tracker = ClaudeActivityFixtureAdapter::new("session-root");
    let start = json!({
        "hook_event_name": "SubagentStart",
        "session_id": "session-root",
        "agent_id": "long-tool-agent",
        "agent_type": "Explore"
    });
    tracker.handle_value(ClaudeActivityInputSource::HookInput, &start, 1_000);

    let tool_name = "x".repeat(512);
    let pre = json!({
        "hook_event_name": "PreToolUse",
        "session_id": "session-root",
        "agent_id": "long-tool-agent",
        "tool_name": tool_name,
        "tool_use_id": "long-tool-use",
        "tool_input": {}
    });
    assert_eq!(
        tracker
            .handle_value(ClaudeActivityInputSource::HookInput, &pre, 2_000)
            .mutations
            .len(),
        1
    );

    let post = json!({
        "hook_event_name": "PostToolUse",
        "session_id": "session-root",
        "agent_id": "long-tool-agent",
        "tool_name": tool_name,
        "tool_use_id": "long-tool-use",
        "tool_input": {},
        "tool_response": {}
    });
    let output = tracker.handle_value(ClaudeActivityInputSource::HookInput, &post, 3_000);
    assert!(matches!(
        output.mutations.as_slice(),
        [ProviderActivityMutation::AppendEntry(entry)]
            if entry.title.encode_utf16().count() <= 256
    ));
}

#[test]
fn claude_activity_tracker_retires_completed_tools_and_rejects_conflicting_terminals() {
    let mut tracker = ClaudeActivityFixtureAdapter::new("session-root");
    tracker.handle_value(
        ClaudeActivityInputSource::HookInput,
        &json!({
            "hook_event_name": "SubagentStart",
            "session_id": "session-root",
            "agent_id": "sequential-agent",
            "agent_type": "Explore"
        }),
        1_000,
    );

    for index in 0..513 {
        let tool_use_id = format!("sequential-tool-{index}");
        let pre = json!({
            "hook_event_name": "PreToolUse",
            "session_id": "session-root",
            "agent_id": "sequential-agent",
            "tool_name": "Read",
            "tool_use_id": tool_use_id,
            "tool_input": {}
        });
        assert_eq!(
            tracker
                .handle_value(ClaudeActivityInputSource::HookInput, &pre, 2_000 + index,)
                .mutations
                .len(),
            1
        );

        let post = json!({
            "hook_event_name": "PostToolUse",
            "session_id": "session-root",
            "agent_id": "sequential-agent",
            "tool_name": "Read",
            "tool_use_id": tool_use_id,
            "tool_input": {},
            "tool_response": {}
        });
        assert_eq!(
            tracker
                .handle_value(ClaudeActivityInputSource::HookInput, &post, 3_000 + index,)
                .mutations
                .len(),
            1
        );
        assert_eq!(tracker.state_counts().tool_lifecycles, 0);

        let conflicting_failure = json!({
            "hook_event_name": "PostToolUseFailure",
            "session_id": "session-root",
            "agent_id": "sequential-agent",
            "tool_name": "Read",
            "tool_use_id": tool_use_id,
            "tool_input": {},
            "error": "late conflicting failure"
        });
        assert!(
            tracker
                .handle_value(
                    ClaudeActivityInputSource::HookInput,
                    &conflicting_failure,
                    4_000 + index,
                )
                .mutations
                .is_empty()
        );
    }
}

#[test]
fn claude_activity_tracker_does_not_poison_a_valid_stop_with_invalid_chronology() {
    let mut tracker = ClaudeActivityFixtureAdapter::new("session-root");
    tracker.handle_value(
        ClaudeActivityInputSource::HookInput,
        &json!({
            "hook_event_name": "SubagentStart",
            "session_id": "session-root",
            "agent_id": "chronology-agent",
            "agent_type": "Explore"
        }),
        2_000,
    );
    let stop = json!({
        "hook_event_name": "SubagentStop",
        "session_id": "session-root",
        "agent_id": "chronology-agent",
        "agent_type": "Explore",
        "last_assistant_message": "Completed safely."
    });
    assert!(
        tracker
            .handle_value(ClaudeActivityInputSource::HookInput, &stop, 1_000)
            .mutations
            .is_empty()
    );
    assert!(matches!(
        tracker
            .handle_value(ClaudeActivityInputSource::HookInput, &stop, 3_000)
            .mutations
            .as_slice(),
        [ProviderActivityMutation::UpsertActor(actor)]
            if actor.status == ActivityLifecycle::Completed
                && actor.summary.as_deref() == Some("Completed safely.")
    ));
}

#[test]
fn claude_activity_tracker_retires_terminal_actors_without_allowing_reopen() {
    let mut tracker = ClaudeActivityFixtureAdapter::new("session-root");
    for index in 0..300 {
        let agent_id = format!("retired-agent-{index}");
        let start = json!({
            "hook_event_name": "SubagentStart",
            "session_id": "session-root",
            "agent_id": agent_id,
            "agent_type": "Explore"
        });
        assert_eq!(
            tracker
                .handle_value(ClaudeActivityInputSource::HookInput, &start, 1_000 + index,)
                .mutations
                .len(),
            2,
            "start {index}"
        );
        let stop = json!({
            "hook_event_name": "SubagentStop",
            "session_id": "session-root",
            "agent_id": agent_id,
            "agent_type": "Explore",
            "last_assistant_message": "Completed."
        });
        assert_eq!(
            tracker
                .handle_value(ClaudeActivityInputSource::HookInput, &stop, 2_000 + index,)
                .mutations
                .len(),
            1,
            "stop {index}"
        );
        assert_eq!(tracker.state_counts().actors, 0, "retire {index}");
        assert!(
            tracker
                .handle_value(ClaudeActivityInputSource::HookInput, &stop, 3_000 + index,)
                .mutations
                .is_empty(),
            "duplicate stop {index}"
        );
    }

    let reopen = json!({
        "hook_event_name": "SubagentStart",
        "session_id": "session-root",
        "agent_id": "retired-agent-299",
        "agent_type": "Explore"
    });
    assert!(
        tracker
            .handle_value(ClaudeActivityInputSource::HookInput, &reopen, 4_000)
            .mutations
            .is_empty()
    );
}

#[test]
fn claude_activity_tracker_stop_retires_open_tools_and_rejects_late_hooks() {
    let mut tracker = ClaudeActivityFixtureAdapter::new("session-root");
    tracker.handle_value(
        ClaudeActivityInputSource::HookInput,
        &json!({
            "hook_event_name": "SubagentStart",
            "session_id": "session-root",
            "agent_id": "stopped-with-tools",
            "agent_type": "Explore"
        }),
        1_000,
    );
    for index in 0..512 {
        let pre = json!({
            "hook_event_name": "PreToolUse",
            "session_id": "session-root",
            "agent_id": "stopped-with-tools",
            "tool_name": "Read",
            "tool_use_id": format!("orphan-candidate-{index}"),
            "tool_input": {}
        });
        assert_eq!(
            tracker
                .handle_value(ClaudeActivityInputSource::HookInput, &pre, 2_000 + index,)
                .mutations
                .len(),
            1
        );
    }
    assert_eq!(tracker.state_counts().tool_lifecycles, 512);

    let stop = json!({
        "hook_event_name": "SubagentStop",
        "session_id": "session-root",
        "agent_id": "stopped-with-tools",
        "agent_type": "Explore",
        "last_assistant_message": "Stopped."
    });
    assert_eq!(
        tracker
            .handle_value(ClaudeActivityInputSource::HookInput, &stop, 3_000)
            .mutations
            .len(),
        1
    );
    assert_eq!(tracker.state_counts().actors, 0);
    assert_eq!(tracker.state_counts().tool_lifecycles, 0);

    for event in ["PreToolUse", "PostToolUse", "PostToolUseFailure"] {
        let late = json!({
            "hook_event_name": event,
            "session_id": "session-root",
            "agent_id": "stopped-with-tools",
            "tool_name": "Read",
            "tool_use_id": "orphan-candidate-0",
            "tool_input": {},
            "tool_response": {},
            "error": "late failure"
        });
        assert!(
            tracker
                .handle_value(ClaudeActivityInputSource::HookInput, &late, 4_000)
                .mutations
                .is_empty(),
            "late {event}"
        );
    }

    tracker.handle_value(
        ClaudeActivityInputSource::HookInput,
        &json!({
            "hook_event_name": "SubagentStart",
            "session_id": "session-root",
            "agent_id": "replacement-agent",
            "agent_type": "Explore"
        }),
        5_000,
    );
    for index in 0..513 {
        let tool_use_id = format!("replacement-tool-{index}");
        let pre = json!({
            "hook_event_name": "PreToolUse",
            "session_id": "session-root",
            "agent_id": "replacement-agent",
            "tool_name": "Read",
            "tool_use_id": tool_use_id,
            "tool_input": {}
        });
        assert_eq!(
            tracker
                .handle_value(ClaudeActivityInputSource::HookInput, &pre, 6_000 + index,)
                .mutations
                .len(),
            1,
            "replacement pre {index}"
        );
        let post = json!({
            "hook_event_name": "PostToolUse",
            "session_id": "session-root",
            "agent_id": "replacement-agent",
            "tool_name": "Read",
            "tool_use_id": tool_use_id,
            "tool_input": {},
            "tool_response": {}
        });
        assert_eq!(
            tracker
                .handle_value(ClaudeActivityInputSource::HookInput, &post, 7_000 + index,)
                .mutations
                .len(),
            1,
            "replacement post {index}"
        );
    }
}

#[test]
fn launch_request_maps_runtime_modes_to_direct_cli_protocol_options() {
    let full_access: LaunchFixture = load_fixture("launch-full-access.json");
    let request = ClaudeProviderRuntime::build_launch_request(LaunchRequestInput {
        thread_id: full_access.thread_id,
        runtime_mode: full_access.runtime_mode,
        cwd: full_access.cwd,
        claude_path: full_access.claude_path,
        resume_session_id: full_access.resume_session_id,
        new_session_id: full_access.new_session_id,
    });
    assert_eq!(
        serde_json::to_value(request).expect("launch request json"),
        full_access.expected
    );

    let plan_mode: LaunchFixture = load_fixture("launch-plan-mode.json");
    let request = ClaudeProviderRuntime::build_launch_request(LaunchRequestInput {
        thread_id: plan_mode.thread_id,
        runtime_mode: plan_mode.runtime_mode,
        cwd: plan_mode.cwd,
        claude_path: plan_mode.claude_path,
        resume_session_id: plan_mode.resume_session_id,
        new_session_id: plan_mode.new_session_id,
    });
    assert_eq!(
        serde_json::to_value(request).expect("launch request json"),
        plan_mode.expected
    );
}

#[test]
fn control_requests_encode_official_correlated_frames() {
    let fixture: ControlFixture = load_fixture("control-requests.json");
    assert_eq!(
        serde_json::to_value(ClaudeControlRequest::interrupt(17)).expect("interrupt json"),
        fixture.interrupt
    );
    assert_eq!(
        serde_json::to_value(ClaudeControlRequest::set_permission_mode(
            18,
            RuntimeMode::AutoAcceptEdits.permission_mode()
        ))
        .expect("permission mode json"),
        fixture.set_permission_mode
    );
    assert_eq!(
        serde_json::to_value(ClaudeControlRequest::cancel_request(19, "approval:1001"))
            .expect("cancel json"),
        fixture.cancel_tool_call
    );
    assert_eq!(
        serde_json::to_value(ClaudeControlRequest::get_context_usage(20))
            .expect("context usage json"),
        fixture.get_context_usage
    );
    assert_eq!(
        serde_json::to_value(ClaudeControlRequest::mcp_status(21)).expect("MCP status json"),
        fixture.mcp_status
    );
}

#[test]
fn stop_task_control_request_encodes_the_exact_private_task_target() {
    // Mutation caught: using the root interrupt subtype or the wrong native task-id field.
    assert_eq!(
        serde_json::to_value(ClaudeControlRequest::stop_task(41, "task-a"))
            .expect("stop task json"),
        json!({
            "type": "control_request",
            "request_id": "coddy-studio-41",
            "request": { "subtype": "stop_task", "task_id": "task-a" }
        })
    );
}

#[test]
fn stop_task_control_request_debug_redacts_the_private_target() {
    let debug = format!("{:?}", ClaudeControlRequest::stop_task(41, "task-private"));

    assert_eq!(debug, "ClaudeControlRequest { .. }");
    assert!(!debug.contains("task-private"));
}

#[test]
fn claude_system_init_and_status_query_normalize_mcp_servers() {
    let mut runtime = ClaudeProviderRuntime::new("thread-1".to_owned(), "session-1".to_owned());
    let initial = runtime.handle_raw_value(
        &json!({
            "type": "system",
            "subtype": "init",
            "mcp_servers": [
                { "name": "connected", "status": "connected" },
                { "name": "pending", "status": "pending" },
                { "name": "auth", "status": "needs-auth" },
                { "name": "failed", "status": "failed", "error": "Connection failed" },
                { "name": "disabled", "status": "disabled" }
            ]
        }),
        1_000,
    );

    assert_eq!(initial.events.len(), 1);
    assert_eq!(initial.events[0].event_type, "mcp.status.updated");
    assert_eq!(
        initial.events[0].payload,
        json!({
            "servers": [
                { "name": "connected", "state": "connected" },
                { "name": "pending", "state": "starting" },
                { "name": "auth", "state": "needs-auth" },
                { "name": "failed", "state": "error", "detail": "Connection failed" },
                { "name": "disabled", "state": "disconnected" }
            ]
        })
    );

    let refreshed = runtime
        .apply_mcp_status_response_for_test(&json!({
            "mcpServers": [
                { "name": "connected", "status": "connected" },
                { "name": "pending", "status": "failed", "error": "Timed out" }
            ]
        }))
        .expect("valid MCP status response");
    assert_eq!(
        refreshed.payload,
        json!({
            "servers": [
                { "name": "connected", "state": "connected" },
                { "name": "pending", "state": "error", "detail": "Timed out" }
            ]
        })
    );
    assert!(
        runtime
            .apply_mcp_status_response_for_test(&json!({
                "mcpServers": [{ "name": "unknown", "status": "surprising" }]
            }))
            .is_none()
    );
}

#[test]
fn claude_stream_usage_preserves_active_context_and_accumulated_total() {
    let fixture: ContextUsageFixture = load_fixture("context-usage.json");
    let mut runtime = ClaudeProviderRuntime::new("thread-1".to_owned(), "session-1".to_owned());
    runtime.start_turn(TurnInput {
        turn_id: "turn-1".to_owned(),
        input: "measure context".to_owned(),
    });

    let message_delta = runtime.handle_raw_value(&fixture.message_delta, 1_000);
    assert_eq!(message_delta.events.len(), 1);
    assert_eq!(
        message_delta.events[0].event_type,
        "thread.token-usage.updated"
    );
    assert_eq!(
        message_delta.events[0].payload["usage"]["usedTokens"],
        1_550
    );

    let duplicate = runtime.handle_raw_value(&fixture.message_delta, 1_001);
    assert!(duplicate.events.is_empty());

    let mut child_delta = fixture.message_delta.clone();
    child_delta["parent_tool_use_id"] = json!("child-tool");
    child_delta["event"]["usage"]["input_tokens"] = json!(9_000);
    let child = runtime.handle_raw_value(&child_delta, 1_002);
    assert!(child.events.is_empty());

    let task_progress = runtime.handle_raw_value(&fixture.task_progress, 2_000);
    assert_eq!(task_progress.events.len(), 1);
    assert_eq!(
        task_progress.events[0].payload["usage"]["usedTokens"],
        1_800
    );
    assert_eq!(task_progress.events[0].payload["usage"]["toolUses"], 4);

    let compact = runtime.handle_raw_value(&fixture.compact_boundary, 3_000);
    assert_eq!(compact.events.len(), 1);
    assert_eq!(compact.events[0].payload["usage"]["usedTokens"], 24_000);
    assert_eq!(
        compact.events[0].payload["usage"]["lastUsedTokens"],
        190_000
    );

    let result = runtime.handle_raw_value(&fixture.result, 4_000);
    assert_eq!(result.events.len(), 2);
    assert_eq!(result.events[0].event_type, "thread.token-usage.updated");
    assert_eq!(result.events[0].payload["usage"]["usedTokens"], 24_000);
    assert_eq!(
        result.events[0].payload["usage"]["totalProcessedTokens"],
        42_000
    );
    assert_eq!(result.events[0].payload["usage"]["maxTokens"], 200_000);
    assert_eq!(
        result.events.last().expect("completion").event_type,
        "turn.completed"
    );
}

#[test]
fn malformed_claude_usage_cannot_clear_last_good_context() {
    let fixture: ContextUsageFixture = load_fixture("context-usage.json");
    let mut runtime = ClaudeProviderRuntime::new("thread-1".to_owned(), "session-1".to_owned());
    runtime.start_turn(TurnInput {
        turn_id: "turn-1".to_owned(),
        input: "measure context".to_owned(),
    });

    let initial = runtime.handle_raw_value(&fixture.message_delta, 1_000);
    assert_eq!(initial.events.len(), 1);

    let mut partially_malformed_stream = fixture.message_delta.clone();
    partially_malformed_stream["event"]["usage"] = json!({
        "input_tokens": -1,
        "output_tokens": 50,
    });
    let partially_malformed = runtime.handle_raw_value(&partially_malformed_stream, 1_500);
    assert!(partially_malformed.events.is_empty());

    let mut partially_malformed_result = fixture.result.clone();
    partially_malformed_result["usage"] = json!({
        "input_tokens": -1,
        "output_tokens": 50,
    });
    partially_malformed_result["modelUsage"] = Value::Null;
    let malformed_result = runtime.handle_raw_value(&partially_malformed_result, 1_750);
    assert_eq!(
        malformed_result
            .events
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>(),
        ["turn.completed"]
    );

    let mut malformed_stream = fixture.message_delta.clone();
    malformed_stream["event"]["usage"] = json!({
        "input_tokens": -1,
        "cache_creation_input_tokens": 9_007_199_254_740_992_u64,
        "output_tokens": "many",
    });
    let malformed = runtime.handle_raw_value(&malformed_stream, 2_000);
    assert!(malformed.events.is_empty());
    let malformed_response = runtime.handle_raw_value(&fixture.malformed, 2_500);
    assert!(malformed_response.events.is_empty());
    let empty = runtime.handle_raw_value(&json!({}), 3_000);
    assert!(empty.events.is_empty());

    let result = runtime.handle_raw_value(&fixture.result, 4_000);
    assert_eq!(result.events[0].event_type, "thread.token-usage.updated");
    assert_eq!(result.events[0].payload["usage"]["usedTokens"], 1_550);
    assert_eq!(
        result.events[0].payload["usage"]["totalProcessedTokens"],
        42_000
    );
    assert_eq!(
        result.events.last().expect("completion").event_type,
        "turn.completed"
    );
}

#[test]
fn claude_usage_emission_and_observation_are_scoped_to_turns() {
    let fixture: ContextUsageFixture = load_fixture("context-usage.json");
    let mut runtime = ClaudeProviderRuntime::new("thread-1".to_owned(), "session-1".to_owned());

    runtime.start_turn(TurnInput {
        turn_id: "turn-1".to_owned(),
        input: "first turn".to_owned(),
    });
    let first = runtime.handle_raw_value(&fixture.message_delta, 1_000);
    assert_eq!(first.events.len(), 1);
    assert_eq!(first.events[0].turn_id.as_deref(), Some("turn-1"));

    runtime.start_turn(TurnInput {
        turn_id: "turn-2".to_owned(),
        input: "second turn".to_owned(),
    });
    let identical_next_turn = runtime.handle_raw_value(&fixture.message_delta, 2_000);
    assert_eq!(identical_next_turn.events.len(), 1);
    assert_eq!(
        identical_next_turn.events[0].turn_id.as_deref(),
        Some("turn-2")
    );

    runtime.start_turn(TurnInput {
        turn_id: "turn-3".to_owned(),
        input: "third turn".to_owned(),
    });
    let lifetime_only = runtime.handle_raw_value(&fixture.result, 3_000);
    assert_eq!(
        lifetime_only
            .events
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>(),
        ["turn.completed"]
    );
}

#[test]
fn authoritative_context_query_is_deduplicated() {
    let fixture: ContextUsageFixture = load_fixture("context-usage.json");
    let mut runtime = ClaudeProviderRuntime::new("thread-1".to_owned(), "session-1".to_owned());

    runtime.start_turn(TurnInput {
        turn_id: "turn-1".to_owned(),
        input: "first turn".to_owned(),
    });
    let first = runtime
        .apply_context_usage_response_for_test("turn-1", &fixture.query_success)
        .expect("first query usage");
    assert_eq!(first.event_type, "thread.token-usage.updated");
    assert_eq!(first.turn_id.as_deref(), Some("turn-1"));
    assert_eq!(first.payload["usage"]["usedTokens"], 31_251);
    assert_eq!(first.payload["usage"]["maxTokens"], 200_000);
    assert_eq!(first.payload["usage"]["compactsAutomatically"], json!(true));
    assert!(
        runtime
            .apply_context_usage_response_for_test("turn-1", &fixture.query_success)
            .is_none()
    );
    assert!(
        runtime
            .apply_context_usage_response_for_test("turn-1", &fixture.malformed)
            .is_none()
    );

    runtime.start_turn(TurnInput {
        turn_id: "turn-2".to_owned(),
        input: "second turn".to_owned(),
    });
    let second = runtime
        .apply_context_usage_response_for_test("turn-2", &fixture.query_success)
        .expect("same query usage in a new turn");
    assert_eq!(second.turn_id.as_deref(), Some("turn-2"));
    assert_eq!(second.payload["usage"]["usedTokens"], 31_251);
}

#[test]
fn fixture_tool_streams_decode_to_canonical_events() {
    let fixture: TraceFixture = load_fixture("trace-tool-streams.json");
    let mut runtime = ClaudeProviderRuntime::new(fixture.thread_id, fixture.startup.session_id);
    let mut events = runtime.start_session(fixture.startup.runtime_mode, None);
    events.extend(runtime.start_turn(TurnInput {
        turn_id: fixture.turn_id,
        input: "search the repo".to_owned(),
    }));
    for message in fixture.messages {
        events.extend(runtime.handle_message(message));
    }
    assert_trace_eq(&events, &fixture.expected_events);
}

/// #167: Claude streams a tool's input as fragments of a JSON document. The fragments are one
/// update, with the complete input, when the tool's block stops, not one recorded activity
/// each; and the block index a later message reuses names that message's tool.
#[test]
fn fixture_tool_input_fragments_report_the_complete_input_once() {
    let fixture: TraceFixture = load_fixture("trace-tool-input-fragments.json");
    let mut runtime = ClaudeProviderRuntime::new(
        fixture.thread_id.clone(),
        fixture.startup.session_id.clone(),
    );
    let mut events = runtime.start_session(fixture.startup.runtime_mode, None);
    events.extend(runtime.start_turn(TurnInput {
        turn_id: fixture.turn_id.clone(),
        input: "run the tests".to_owned(),
    }));
    for message in &fixture.messages {
        events.extend(runtime.handle_message(message.clone()));
    }
    assert_trace_eq(&events, &fixture.expected_events);

    // The driver reads raw frames.
    let mut runtime = ClaudeProviderRuntime::new(fixture.thread_id, fixture.startup.session_id);
    let mut events = runtime.start_session(fixture.startup.runtime_mode, None);
    events.extend(runtime.start_turn(TurnInput {
        turn_id: fixture.turn_id,
        input: "run the tests".to_owned(),
    }));
    for message in fixture.messages {
        let value = serde_json::to_value(message).expect("a Claude frame");
        events.extend(runtime.handle_raw_value(&value, 1_000).events);
    }
    assert_trace_eq(&events, &fixture.expected_events);
}

#[test]
fn todo_write_streams_emit_plan_updates() {
    let fixture: TraceFixture = load_fixture("trace-todo-plan.json");
    let mut runtime = ClaudeProviderRuntime::new(fixture.thread_id, fixture.startup.session_id);
    let mut events = runtime.start_session(fixture.startup.runtime_mode, None);
    events.extend(runtime.start_turn(TurnInput {
        turn_id: fixture.turn_id,
        input: "make a plan".to_owned(),
    }));
    for message in fixture.messages {
        events.extend(runtime.handle_message(message));
    }
    assert_trace_eq(&events, &fixture.expected_events);
}

#[test]
fn task_tool_is_classified_as_collaboration_work() {
    let fixture: TraceFixture = load_fixture("trace-task-tool.json");
    let mut runtime = ClaudeProviderRuntime::new(fixture.thread_id, fixture.startup.session_id);
    let mut events = runtime.start_session(fixture.startup.runtime_mode, None);
    events.extend(runtime.start_turn(TurnInput {
        turn_id: fixture.turn_id,
        input: "delegate this".to_owned(),
    }));
    for message in fixture.messages {
        events.extend(runtime.handle_message(message));
    }
    assert_trace_eq(&events, &fixture.expected_events);
}

#[test]
fn aborted_results_map_to_interrupted_turn_completion() {
    let fixture: TraceFixture = load_fixture("trace-abort-result.json");
    let mut runtime = ClaudeProviderRuntime::new(fixture.thread_id, fixture.startup.session_id);
    let mut events = runtime.start_session(fixture.startup.runtime_mode, None);
    events.extend(runtime.start_turn(TurnInput {
        turn_id: fixture.turn_id,
        input: "hello".to_owned(),
    }));
    for message in fixture.messages {
        events.extend(runtime.handle_message(message));
    }
    assert_trace_eq(&events, &fixture.expected_events);
}

#[test]
fn a_failed_result_without_errors_reports_claude_terminal_reason() {
    // Claude names why a run ended in `terminal_reason`, snake_case like its
    // sibling `is_error` and `stop_reason`. On an error result that carries no
    // `errors`, that name is the only description of the failure, so decoding
    // it under the wrong key would silently fall back to the bare subtype.
    let message: ClaudeMessage = serde_json::from_value(json!({
        "type": "result",
        // Not `error_during_execution`: that subtype is Claude's user-abort
        // shape and maps to `interrupted`.
        "subtype": "error_max_turns",
        "is_error": true,
        "terminal_reason": "max_turns",
        "stop_reason": Value::Null,
        "session_id": "sdk-session-terminal",
        "uuid": "uuid-terminal",
    }))
    .expect("result frame decodes with a terminal reason");

    let mut runtime = ClaudeProviderRuntime::new(
        "provider-thread-1".to_owned(),
        "sdk-session-terminal".to_owned(),
    );
    runtime.start_session(RuntimeMode::FullAccess, None);
    runtime.start_turn(TurnInput {
        turn_id: "turn-1".to_owned(),
        input: "hello".to_owned(),
    });

    let events = runtime.handle_message(message);
    let completed = events
        .iter()
        .find(|event| event.event_type == "turn.completed")
        .expect("failing result completes the turn");
    assert_eq!(completed.payload["state"], "failed");
    assert_eq!(
        completed.payload["errorMessage"],
        "Claude turn failed (max_turns)."
    );
    assert_eq!(completed.payload["errorClass"], "provider_error");
}

fn claude_result(is_error: bool, uuid: &str) -> ClaudeMessage {
    serde_json::from_value(json!({
        "type": "result",
        "subtype": if is_error { "error_max_turns" } else { "success" },
        "is_error": is_error,
        "stop_reason": Value::Null,
        "session_id": "sdk-session-replay",
        "uuid": uuid,
    }))
    .expect("result frame decodes")
}

fn completes_turn(events: &[CanonicalEvent], turn_id: &str) -> bool {
    events.iter().any(|event| {
        event.event_type == "turn.completed" && event.turn_id.as_deref() == Some(turn_id)
    })
}

#[test]
fn a_result_before_the_turn_input_replay_does_not_end_the_turn() {
    let mut runtime = ClaudeProviderRuntime::new(
        "provider-thread-1".to_owned(),
        "sdk-session-replay".to_owned(),
    );
    runtime.start_session(RuntimeMode::FullAccess, None);
    runtime.start_turn(TurnInput {
        turn_id: "turn-1".to_owned(),
        input: "hello".to_owned(),
    });
    runtime.await_turn_input(7);

    assert!(!completes_turn(
        &runtime.handle_message(claude_result(false, "earlier-work")),
        "turn-1"
    ));
    // Another write's replay does not begin this turn.
    runtime.stop_awaiting_turn_input(6);
    assert!(!completes_turn(
        &runtime.handle_message(claude_result(false, "older-write")),
        "turn-1"
    ));
    runtime.stop_awaiting_turn_input(7);
    assert!(completes_turn(
        &runtime.handle_message(claude_result(false, "this-turn")),
        "turn-1"
    ));
}

#[test]
fn an_error_result_before_the_turn_input_replay_still_ends_the_turn() {
    let mut runtime = ClaudeProviderRuntime::new(
        "provider-thread-1".to_owned(),
        "sdk-session-replay".to_owned(),
    );
    runtime.start_session(RuntimeMode::FullAccess, None);
    runtime.start_turn(TurnInput {
        turn_id: "turn-1".to_owned(),
        input: "hello".to_owned(),
    });
    runtime.await_turn_input(7);

    assert!(completes_turn(
        &runtime.handle_message(claude_result(true, "failed-before-reading")),
        "turn-1"
    ));
}

#[test]
fn permission_requests_round_trip_through_open_and_resolved_events() {
    let fixture: PermissionFixture = load_fixture("permission-flow.json");
    let mut runtime = ClaudeProviderRuntime::new(fixture.thread_id, fixture.startup.session_id);
    runtime.start_session(fixture.startup.runtime_mode, None);
    runtime.start_turn(TurnInput {
        turn_id: fixture.turn_id.clone(),
        input: "approve a command".to_owned(),
    });

    let mut events = runtime.handle_message(fixture.message);
    let request_event =
        runtime.open_permission_request(fixture.request, &fixture.resolution.request_id);
    events.extend(request_event);
    events.extend(
        runtime.resolve_permission_request(
            &fixture.resolution.request_id,
            fixture.resolution.decision,
        ),
    );

    assert_trace_eq(&events, &fixture.expected_events);
}

#[test]
fn ask_user_question_round_trips_structured_answers() {
    let fixture: UserInputFixture = load_fixture("user-input-flow.json");
    let mut runtime = ClaudeProviderRuntime::new(fixture.thread_id, fixture.startup.session_id);
    runtime.start_session(fixture.startup.runtime_mode, None);
    runtime.start_turn(TurnInput {
        turn_id: fixture.turn_id.clone(),
        input: "question turn".to_owned(),
    });

    let mut events = runtime.handle_message(fixture.message);
    let opened = runtime.open_user_input_request(fixture.request, &fixture.resolution.request_id);
    events.extend(opened);
    let resolved = runtime.resolve_user_input_request(
        &fixture.resolution.request_id,
        fixture.resolution.answers.clone(),
    );
    assert_eq!(resolved.updated_input, fixture.expected_updated_input);
    events.extend(resolved.events);

    assert_trace_eq(&events, &fixture.expected_events);
}

#[test]
fn assistant_exit_plan_snapshots_emit_proposed_plan_completion() {
    let fixture: ExitPlanFixture = load_fixture("exit-plan-message.json");
    let mut runtime = ClaudeProviderRuntime::new(fixture.thread_id, fixture.startup.session_id);
    runtime.start_session(fixture.startup.runtime_mode, None);
    runtime.start_turn(TurnInput {
        turn_id: fixture.turn_id,
        input: "make a plan".to_owned(),
    });

    let event = runtime
        .handle_assistant_message(fixture.message)
        .into_iter()
        .find(|event| event.event_type == "turn.proposed.completed")
        .expect("turn.proposed.completed event");
    assert_eq!(CanonicalEventTrace::from(&event), fixture.expected_event);
}

#[test]
fn stream_interrupts_teardown_the_session_structurally() {
    let fixture: StreamInterruptFixture = load_fixture("stream-interrupt.json");
    let mut runtime = ClaudeProviderRuntime::new(fixture.thread_id, fixture.startup.session_id);
    let mut events = runtime.start_session(fixture.startup.runtime_mode, None);
    events.extend(runtime.start_turn(TurnInput {
        turn_id: fixture.turn_id,
        input: "hello".to_owned(),
    }));
    events.extend(runtime.handle_stream_failure(&fixture.error));
    assert_trace_eq(&events, &fixture.expected_events);
}

#[test]
fn reconnect_snapshots_preserve_pending_requests_and_user_dialogs() {
    let fixture: ReconnectFixture = load_fixture("reconnect-state.json");
    let mut runtime =
        ClaudeProviderRuntime::new(fixture.thread_id.clone(), fixture.session_id.clone());
    runtime.start_session(fixture.runtime_mode, None);
    runtime.start_turn(TurnInput {
        turn_id: fixture.turn_id.clone(),
        input: "resume this".to_owned(),
    });
    runtime.restore_from_snapshot(ReconnectSnapshot {
        session_id: fixture.session_id.clone(),
        thread_id: fixture.thread_id.clone(),
        turn_id: Some(fixture.turn_id.clone()),
        runtime_mode: fixture.runtime_mode,
        pending_approvals: vec![fixture.pending_approval.clone()],
        pending_user_inputs: vec![fixture.pending_user_input.clone()],
    });

    let snapshot = runtime.snapshot();
    assert_eq!(snapshot.session_id, fixture.session_id);
    assert_eq!(snapshot.thread_id, fixture.thread_id);
    assert_eq!(snapshot.turn_id, Some(fixture.turn_id));
    assert_eq!(snapshot.pending_approvals, vec![fixture.pending_approval]);
    assert_eq!(
        snapshot.pending_user_inputs,
        vec![fixture.pending_user_input]
    );
    assert_eq!(
        serde_json::to_value(snapshot.runtime_mode).expect("runtime mode"),
        json!("approval-required")
    );
}

#[test]
fn claude_runtime_routes_only_explicit_stable_hook_input_into_activity() {
    let mut runtime =
        ClaudeProviderRuntime::new("thread-root".to_owned(), "session-root".to_owned());

    let wrapper = runtime.handle_raw_value(
        &json!({
            "type": "system",
            "subtype": "hook_response",
            "session_id": "session-root",
            "hook_event": "SubagentStart",
            "hook_id": "hook-1",
            "uuid": "wrapper-1"
        }),
        1_000,
    );
    assert!(wrapper.events.is_empty());
    assert!(wrapper.activity.is_empty());
    assert!(wrapper.native_event_id.is_none());

    let forwarded = runtime.handle_raw_value(
        &json!({
            "type": "assistant",
            "session_id": "child-session",
            "uuid": "forwarded-1",
            "parent_tool_use_id": "task-tool-1",
            "message": {
                "id": "message-1",
                "content": [{"type": "text", "text": "forwarded child text"}]
            }
        }),
        2_000,
    );
    assert!(forwarded.events.is_empty());
    assert!(forwarded.activity.is_empty());
    assert!(forwarded.native_event_id.is_none());

    let hook_input = json!({
        "hook_event_name": "SubagentStart",
        "session_id": "session-root",
        "agent_id": "agent-stable-1",
        "agent_type": "Explore",
        "transcript_path": "/tmp/session-root.jsonl",
        "cwd": "/workspace"
    });
    let started = runtime.handle_raw_value(&hook_input, 3_000);
    assert!(started.events.is_empty());
    assert_eq!(started.activity.len(), 3);
    assert!(matches!(
        &started.activity[0],
        ProviderActivityMutation::SetScope {
            capabilities: ActivityCapabilities {
                actors: true,
                attributed_activity: true,
                background_work: false,
                terminal_observation: false,
                ..
            },
            observation_state: ActivityObservationState::Live,
        }
    ));
    let native_event_id = started
        .native_event_id
        .as_deref()
        .expect("stable hook input must receive a native event id");
    assert!(native_event_id.starts_with("claude:hook:"));

    let duplicate = runtime.handle_raw_value(&hook_input, 3_000);
    assert!(duplicate.events.is_empty());
    assert!(duplicate.activity.is_empty());
    assert!(duplicate.native_event_id.is_none());
}

#[test]
fn claude_text_deltas_follow_message_start_identity() {
    let mut runtime = ClaudeProviderRuntime::new("thread-1".to_owned(), "session-1".to_owned());
    runtime.start_turn(TurnInput {
        turn_id: "turn-1".to_owned(),
        input: "test boundaries".to_owned(),
    });

    for (message_id, text) in [
        ("claude-message-1", "First."),
        ("claude-message-2", "Second."),
    ] {
        runtime.handle_raw_value(
            &json!({
                "type": "stream_event",
                "session_id": "session-1",
                "uuid": format!("start-{message_id}"),
                "parent_tool_use_id": null,
                "event": { "type": "message_start", "message": { "id": message_id } }
            }),
            1_000,
        );
        let output = runtime.handle_raw_value(
            &json!({
                "type": "stream_event",
                "session_id": "session-1",
                "uuid": format!("delta-{message_id}"),
                "parent_tool_use_id": null,
                "event": {
                    "type": "content_block_delta",
                    "index": 0,
                    "delta": { "type": "text_delta", "text": text }
                }
            }),
            1_001,
        );
        assert_eq!(output.events[0].item_id.as_deref(), Some(message_id));
    }

    runtime.start_turn(TurnInput {
        turn_id: "turn-2".to_owned(),
        input: "new turn".to_owned(),
    });
    let output = runtime.handle_raw_value(
        &json!({
            "type": "stream_event",
            "session_id": "session-1",
            "uuid": "delta-without-message-start",
            "parent_tool_use_id": null,
            "event": {
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "text_delta", "text": "No stale identity." }
            }
        }),
        1_002,
    );
    assert_eq!(output.events[0].item_id, None);
}

#[test]
fn forwarded_text_is_suppressed_while_forwarded_task_lifecycle_stays_canonical() {
    let mut runtime =
        ClaudeProviderRuntime::new("thread-root".to_owned(), "session-root".to_owned());
    runtime.start_turn(TurnInput {
        turn_id: "turn-root".to_owned(),
        input: "delegate".to_owned(),
    });

    for value in [
        json!({
            "type": "assistant",
            "session_id": "child-session",
            "uuid": "child-assistant",
            "parent_tool_use_id": "task-parent",
            "message": {
                "id": "child-message",
                "content": [{"type": "text", "text": "must not become root assistant text"}]
            }
        }),
        json!({
            "type": "stream_event",
            "session_id": "child-session",
            "uuid": "child-text-delta",
            "parent_tool_use_id": "task-parent",
            "event": {
                "type": "content_block_delta",
                "index": 0,
                "delta": {
                    "type": "text_delta",
                    "text": "must not become a root content delta"
                }
            }
        }),
        json!({
            "type": "stream_event",
            "session_id": "child-session",
            "uuid": "child-reasoning-delta",
            "parent_tool_use_id": "task-parent",
            "event": {
                "type": "content_block_delta",
                "index": 0,
                "delta": {
                    "type": "thinking_delta",
                    "thinking": "must not become root reasoning"
                }
            }
        }),
    ] {
        let output = runtime.handle_raw_value(&value, 1_000);
        assert!(output.events.is_empty());
        assert!(output.activity.is_empty());
    }

    let root_text = runtime.handle_raw_value(
        &json!({
            "type": "stream_event",
            "session_id": "session-root",
            "uuid": "root-text-delta",
            "parent_tool_use_id": null,
            "event": {
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "text_delta", "text": "root response"}
            }
        }),
        1_100,
    );
    assert_eq!(root_text.events.len(), 1);
    assert_eq!(root_text.events[0].event_type, "content.delta");

    let started = runtime.handle_raw_value(
        &json!({
            "type": "stream_event",
            "session_id": "child-session",
            "uuid": "child-tool-start",
            "parent_tool_use_id": "task-parent",
            "event": {
                "type": "content_block_start",
                "index": 1,
                "content_block": {
                    "type": "tool_use",
                    "id": "child-tool-1",
                    "name": "Bash",
                    "input": {"command": "pwd"}
                }
            }
        }),
        1_200,
    );
    assert!(started.activity.is_empty());
    assert_eq!(started.events.len(), 1);
    assert_eq!(started.events[0].event_type, "item.started");
    assert_eq!(
        started.events[0]
            .provider_refs
            .as_ref()
            .and_then(|refs| refs.get("providerItemId")),
        Some(&json!("child-tool-1"))
    );
    assert_eq!(
        started.events[0]
            .provider_refs
            .as_ref()
            .and_then(|refs| refs.get("parentToolUseId")),
        Some(&json!("task-parent"))
    );

    let stopped = runtime.handle_raw_value(
        &json!({
            "type": "stream_event",
            "session_id": "child-session",
            "uuid": "child-tool-stop",
            "parent_tool_use_id": "task-parent",
            "event": {"type": "content_block_stop", "index": 1}
        }),
        1_300,
    );
    assert!(stopped.events.is_empty());
    assert!(stopped.activity.is_empty());

    let result = runtime.handle_raw_value(
        &json!({
            "type": "user",
            "session_id": "child-session",
            "uuid": "child-tool-result",
            "parent_tool_use_id": "task-parent",
            "message": {
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "child-tool-1",
                    "content": "workspace"
                }]
            }
        }),
        1_400,
    );
    assert!(result.activity.is_empty());
    assert_eq!(
        result
            .events
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>(),
        ["item.updated", "item.completed"]
    );
    assert!(result.events.iter().all(|event| {
        event
            .provider_refs
            .as_ref()
            .and_then(|refs| refs.get("parentToolUseId"))
            == Some(&json!("task-parent"))
    }));
}

#[test]
fn claude_hook_native_ids_are_length_framed_and_reject_ambiguous_control_fields() {
    let stable = json!({
        "hook_event_name": "SubagentStart",
        "session_id": "session-root",
        "agent_id": "agent-stable-1",
    });
    assert_eq!(
        claude_hook_native_event_id_for_test(&stable).as_deref(),
        Some("claude:hook:d522f68beee25576d97830b627590aa34a417885338384ec366469f8b0dda4b9")
    );

    let ambiguous_left = json!({
        "hook_event_name": "PreToolUse",
        "session_id": "session-root",
        "agent_id": "agent-stable-1",
        "tool_use_id": "tool\u{0000}tail",
    });
    let ambiguous_right = json!({
        "hook_event_name": "PreToolUse",
        "session_id": "session-root",
        "agent_id": "agent-stable-1\u{0000}tool",
        "tool_use_id": "tail",
    });
    assert_eq!(claude_hook_native_event_id_for_test(&ambiguous_left), None);
    assert_eq!(claude_hook_native_event_id_for_test(&ambiguous_right), None);

    let mut runtime =
        ClaudeProviderRuntime::new("thread-root".to_owned(), "session-root".to_owned());
    let started = runtime.handle_raw_value(
        &json!({
            "hook_event_name": "SubagentStart",
            "session_id": "session-root",
            "agent_id": "agent-stable-1",
            "agent_type": "Explore",
        }),
        1_000,
    );
    assert!(!started.activity.is_empty());
    let rejected = runtime.handle_raw_value(
        &json!({
            "hook_event_name": "PreToolUse",
            "session_id": "session-root",
            "agent_id": "agent-stable-1",
            "tool_use_id": "tool\u{0000}tail",
            "tool_name": "Bash",
            "tool_input": {"command": "pwd"},
        }),
        1_100,
    );
    assert!(rejected.events.is_empty());
    assert!(rejected.activity.is_empty());
    assert!(rejected.native_event_id.is_none());
}

fn validate_claude_activity_fixture(name: &str, fixture: &ClaudeActivityFixture) {
    assert!(!fixture.scenario.trim().is_empty(), "{name}: scenario");
    assert!(
        !fixture.raw_input_lines.is_empty(),
        "{name}: raw input lines"
    );
    assert!(
        fixture.raw_input_lines.len() <= 256,
        "{name}: raw input lines must stay bounded"
    );
    assert!(
        !fixture.expected_mutations.is_empty(),
        "{name}: expected mutations"
    );
    assert!(
        fixture.expected_mutations.len() <= 256,
        "{name}: expected mutations must stay bounded"
    );
    assert_eq!(
        fixture.expected_mutation_input_indexes.len(),
        fixture.expected_mutations.len(),
        "{name}: every expected mutation must identify its source input"
    );
    assert!(
        fixture
            .expected_mutation_input_indexes
            .windows(2)
            .all(|pair| pair[0] <= pair[1]),
        "{name}: mutation input indexes must preserve input order"
    );

    let decoded_lines = fixture
        .raw_input_lines
        .iter()
        .enumerate()
        .map(|(index, input)| {
            let value: Value = serde_json::from_str(&input.line)
                .unwrap_or_else(|error| panic!("{name}: raw input line {index}: {error}"));
            validate_claude_evidence_line(name, index, input.source, &value);
            value
        })
        .collect::<Vec<_>>();

    let no_mutation_indexes = fixture
        .no_mutation_input_indexes
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    assert_eq!(
        no_mutation_indexes.len(),
        fixture.no_mutation_input_indexes.len(),
        "{name}: no-op input indexes must be unique"
    );
    let mutation_indexes = fixture
        .expected_mutation_input_indexes
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    assert!(
        no_mutation_indexes.is_disjoint(&mutation_indexes),
        "{name}: a no-op input cannot own an expected mutation"
    );
    for index in no_mutation_indexes.union(&mutation_indexes) {
        assert!(
            *index < fixture.raw_input_lines.len(),
            "{name}: classified input index {index} is out of bounds"
        );
    }
    assert_eq!(
        no_mutation_indexes.len() + mutation_indexes.len(),
        fixture.raw_input_lines.len(),
        "{name}: every input must be classified as mutating or a semantic no-op"
    );

    validate_claude_identity_domains(name, fixture, &decoded_lines);
    for mutation in &fixture.expected_mutations {
        assert_untrusted_fields_omitted(name, mutation);
        let expected: ExpectedClaudeActivityMutation = serde_json::from_value(mutation.clone())
            .unwrap_or_else(|error| panic!("{name}: expected activity mutation: {error}"));
        match expected {
            ExpectedClaudeActivityMutation::AppendEntry { entry } => {
                assert!(
                    entry.id.starts_with("claude:event:"),
                    "{name}: canonical entry id"
                );
            }
            ExpectedClaudeActivityMutation::SetScope {
                capabilities,
                observation_state,
            } => {
                assert_eq!(
                    observation_state,
                    ActivityObservationState::Live,
                    "{name}: unsupported activity downgrade remains a live scope"
                );
                assert!(!capabilities.actors);
                assert!(!capabilities.attributed_activity);
                assert!(!capabilities.background_work);
                assert!(!capabilities.terminal_observation);
            }
            ExpectedClaudeActivityMutation::UpsertActor { actor } => {
                assert!(
                    actor.id.starts_with("claude:agent:"),
                    "{name}: canonical actor id"
                );
                assert_eq!(actor.provider_type.as_deref(), Some("claude"));
                assert!(
                    actor.parent_actor_id.is_none(),
                    "{name}: fixture evidence does not document parent lineage"
                );
            }
        }
    }

    validate_clipped_summary(name, fixture, &decoded_lines);

    if let Some(expected) = &fixture.expected_launch {
        assert!(!expected.activity_flags_supported);
        assert!(expected.base_launch_functional);
        assert_eq!(
            expected.required_base_arguments,
            [
                "--print",
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--include-partial-messages",
                "--verbose",
            ]
        );
        assert_eq!(
            expected.omitted_activity_arguments,
            ["--include-hook-events", "--forward-subagent-text"]
        );
        assert!(!expected.activity_capabilities.actors);
        assert!(!expected.activity_capabilities.attributed_activity);
    }
}

fn validate_claude_identity_domains(
    name: &str,
    fixture: &ClaudeActivityFixture,
    decoded_lines: &[Value],
) {
    let no_mutation_indexes = fixture
        .no_mutation_input_indexes
        .iter()
        .copied()
        .collect::<HashSet<_>>();
    let mut actor_ids = HashSet::new();
    let mut task_ids_by_tool_use_id = HashMap::new();
    let mut tool_owner_by_use_id = HashMap::new();
    let mut main_transcript_by_session_id = HashMap::new();
    for (input, value) in fixture.raw_input_lines.iter().zip(decoded_lines) {
        match input.source {
            ClaudeRawInputSource::HookInput => {
                let session_id = required_string(name, "hook input", value, "session_id");
                let transcript_path = required_string(name, "hook input", value, "transcript_path");
                if let Some(previous) =
                    main_transcript_by_session_id.insert(session_id, transcript_path.clone())
                {
                    assert_eq!(
                        previous, transcript_path,
                        "{name}: every hook input for one session must retain its main transcript"
                    );
                }
                if let Some(agent_id) = value.get("agent_id").and_then(Value::as_str) {
                    actor_ids.insert(format!("claude:agent:{agent_id}"));
                }
                if value["hook_event_name"] == "PreToolUse"
                    && let (Some(agent_id), Some(tool_use_id)) = (
                        value.get("agent_id").and_then(Value::as_str),
                        value.get("tool_use_id").and_then(Value::as_str),
                    )
                {
                    assert!(
                        tool_owner_by_use_id
                            .insert(tool_use_id.to_owned(), agent_id.to_owned())
                            .is_none(),
                        "{name}: attributed PreToolUse IDs must be unique"
                    );
                }
            }
            ClaudeRawInputSource::Stream
                if value["type"] == "system" && value["subtype"] == "task_started" =>
            {
                let task_id = required_string(name, "task_started", value, "task_id");
                let tool_use_id = required_string(name, "task_started", value, "tool_use_id");
                assert!(
                    task_ids_by_tool_use_id
                        .insert(tool_use_id, task_id.clone())
                        .is_none(),
                    "{name}: task tool-use IDs must be unique"
                );
            }
            _ => {}
        }
    }

    for (index, (input, value)) in fixture
        .raw_input_lines
        .iter()
        .zip(decoded_lines)
        .enumerate()
    {
        let lacks_stable_agent_identity = match input.source {
            ClaudeRawInputSource::Stream => {
                value["type"] == "assistant"
                    || value["type"] == "user"
                    || value["type"] == "stream_event"
                    || (value["type"] == "system"
                        && matches!(
                            value["subtype"].as_str(),
                            Some(
                                "hook_started"
                                    | "hook_response"
                                    | "task_started"
                                    | "task_updated"
                                    | "task_notification"
                            )
                        ))
            }
            ClaudeRawInputSource::HookInput => {
                value.get("agent_id").and_then(Value::as_str).is_none()
            }
            ClaudeRawInputSource::BaseLaunch | ClaudeRawInputSource::CapabilityProbe => false,
        };
        if lacks_stable_agent_identity {
            assert!(
                no_mutation_indexes.contains(&index),
                "{name}: input {index} without stable agent identity must be a semantic no-op"
            );
        }

        if input.source == ClaudeRawInputSource::HookInput
            && matches!(
                value["hook_event_name"].as_str(),
                Some("PostToolUse" | "PostToolUseFailure")
            )
            && let Some(agent_id) = value.get("agent_id").and_then(Value::as_str)
        {
            let tool_use_id = required_string(name, "tool completion", value, "tool_use_id");
            assert_eq!(
                tool_owner_by_use_id.get(&tool_use_id).map(String::as_str),
                Some(agent_id),
                "{name}: Pre/Post tool hooks must share tool_use_id and agent_id"
            );
        }

        if input.source == ClaudeRawInputSource::Stream
            && matches!(value["type"].as_str(), Some("assistant" | "user"))
        {
            let parent_tool_use_id =
                required_string(name, "forwarded message", value, "parent_tool_use_id");
            assert!(
                task_ids_by_tool_use_id.contains_key(&parent_tool_use_id),
                "{name}: forwarded text must correlate only through a task tool-use ID"
            );
        }
        if input.source == ClaudeRawInputSource::Stream
            && value["type"] == "system"
            && matches!(
                value["subtype"].as_str(),
                Some("task_updated" | "task_notification")
            )
        {
            let task_id = required_string(name, "task lifecycle", value, "task_id");
            assert!(
                task_ids_by_tool_use_id
                    .values()
                    .any(|started_task_id| started_task_id == &task_id),
                "{name}: task closure must match a prior task_started"
            );
        }
    }

    for (input_index, mutation) in fixture
        .expected_mutation_input_indexes
        .iter()
        .zip(&fixture.expected_mutations)
    {
        let expected: ExpectedClaudeActivityMutation =
            serde_json::from_value(mutation.clone()).expect("validated activity mutation");
        match expected {
            ExpectedClaudeActivityMutation::UpsertActor { actor } => {
                assert!(
                    actor_ids.contains(&actor.id),
                    "{name}: actor {} lacks a matching hook-input agent_id",
                    actor.id
                );
                let source_agent_id = decoded_lines[*input_index]
                    .get("agent_id")
                    .and_then(Value::as_str)
                    .map(|agent_id| format!("claude:agent:{agent_id}"));
                assert_eq!(
                    source_agent_id.as_deref(),
                    Some(actor.id.as_str()),
                    "{name}: actor mutation must be owned by the same input's agent_id"
                );
            }
            ExpectedClaudeActivityMutation::AppendEntry { entry } => match entry.owner_kind {
                ActivityRecordKind::Actor => {
                    assert!(
                        actor_ids.contains(&entry.owner_id),
                        "{name}: actor-owned entry lacks a matching hook-input agent_id"
                    );
                    let source_agent_id = decoded_lines[*input_index]
                        .get("agent_id")
                        .and_then(Value::as_str)
                        .map(|agent_id| format!("claude:agent:{agent_id}"));
                    assert_eq!(
                        source_agent_id.as_deref(),
                        Some(entry.owner_id.as_str()),
                        "{name}: actor entry must be owned by the same input's agent_id"
                    );
                    let source = &decoded_lines[*input_index];
                    let tool_use_id =
                        required_string(name, "actor-owned tool entry", source, "tool_use_id");
                    assert!(
                        entry.id.contains(&tool_use_id),
                        "{name}: tool entry ID must retain its source tool_use_id"
                    );
                    assert!(
                        matches!(
                            source["hook_event_name"].as_str(),
                            Some("PreToolUse" | "PostToolUse" | "PostToolUseFailure")
                        ),
                        "{name}: actor tool entry must originate from a tool hook input"
                    );
                }
                ActivityRecordKind::WorkItem => {
                    panic!("{name}: Claude Task/tool evidence is not background-work identity")
                }
            },
            ExpectedClaudeActivityMutation::SetScope { .. } => {}
        }
    }
}

fn validate_clipped_summary(name: &str, fixture: &ClaudeActivityFixture, decoded_lines: &[Value]) {
    let oversized_summary = fixture
        .raw_input_lines
        .iter()
        .zip(decoded_lines)
        .filter(|(input, value)| {
            input.source == ClaudeRawInputSource::HookInput
                && value["hook_event_name"] == "SubagentStop"
        })
        .filter_map(|(_, value)| value["last_assistant_message"].as_str())
        .find(|summary| summary.encode_utf16().count() > ACTIVITY_SUMMARY_MAX_LENGTH);
    let Some(oversized_summary) = oversized_summary else {
        return;
    };
    let clipped_summary = fixture.expected_mutations.iter().find_map(|mutation| {
        serde_json::from_value::<ExpectedClaudeActivityMutation>(mutation.clone())
            .ok()
            .and_then(|mutation| match mutation {
                ExpectedClaudeActivityMutation::UpsertActor { actor }
                    if actor.status == ActivityLifecycle::Completed =>
                {
                    actor.summary
                }
                _ => None,
            })
    });
    let clipped_summary =
        clipped_summary.unwrap_or_else(|| panic!("{name}: oversized stop needs a bounded summary"));
    assert_eq!(
        clipped_summary.encode_utf16().count(),
        ACTIVITY_SUMMARY_MAX_LENGTH,
        "{name}: safe summary must be clipped to the activity contract maximum"
    );
    assert!(
        oversized_summary.starts_with(&clipped_summary),
        "{name}: clipping must preserve complete UTF-8 scalar values"
    );
}

fn assert_untrusted_fields_omitted(name: &str, value: &Value) {
    const FORBIDDEN_KEYS: &[&str] = &[
        "apiKey",
        "api_key",
        "credentials",
        "env",
        "environment",
        "settings",
        "toolInput",
        "toolResponse",
        "tool_input",
        "tool_response",
    ];
    match value {
        Value::Array(values) => {
            for value in values {
                assert_untrusted_fields_omitted(name, value);
            }
        }
        Value::Object(object) => {
            for (key, value) in object {
                assert!(
                    !FORBIDDEN_KEYS.contains(&key.as_str()),
                    "{name}: untrusted raw field {key} leaked into expected activity"
                );
                assert_untrusted_fields_omitted(name, value);
            }
        }
        _ => {}
    }
}

fn validate_untrusted_boundary_coverage(fixtures: &[ClaudeActivityFixture]) {
    const SENTINELS: &[&str] = &[
        "CLAUDE_FIXTURE_SECRET_DO_NOT_LEAK",
        "CLAUDE_FIXTURE_ENV_DO_NOT_LEAK",
        "CLAUDE_FIXTURE_SETTINGS_DO_NOT_LEAK",
        "CLAUDE_FIXTURE_TOOL_PAYLOAD_DO_NOT_LEAK",
    ];
    let raw = fixtures
        .iter()
        .flat_map(|fixture| &fixture.raw_input_lines)
        .map(|input| input.line.as_str())
        .collect::<String>();
    let expected = fixtures
        .iter()
        .flat_map(|fixture| &fixture.expected_mutations)
        .map(Value::to_string)
        .collect::<String>();
    for sentinel in SENTINELS {
        assert!(
            raw.contains(sentinel),
            "raw fixtures must retain untrusted sentinel {sentinel}"
        );
        assert!(
            !expected.contains(sentinel),
            "expected activity must omit untrusted sentinel {sentinel}"
        );
    }

    let mut duplicate_noop_covered = false;
    let mut agent_ids_by_type = HashMap::<String, HashSet<String>>::new();
    for fixture in fixtures {
        let no_mutation_indexes = fixture
            .no_mutation_input_indexes
            .iter()
            .copied()
            .collect::<HashSet<_>>();
        for (index, input) in fixture.raw_input_lines.iter().enumerate() {
            if fixture.raw_input_lines[..index]
                .iter()
                .any(|prior| prior.source == input.source && prior.line == input.line)
            {
                assert!(
                    no_mutation_indexes.contains(&index),
                    "{}: duplicate delivery at input {index} must be a semantic no-op",
                    fixture.scenario
                );
                duplicate_noop_covered = true;
            }

            if input.source == ClaudeRawInputSource::HookInput {
                let value: Value =
                    serde_json::from_str(&input.line).expect("validated hook-input JSON");
                if value["hook_event_name"] == "SubagentStart"
                    && let (Some(agent_type), Some(agent_id)) =
                        (value["agent_type"].as_str(), value["agent_id"].as_str())
                {
                    agent_ids_by_type
                        .entry(agent_type.to_owned())
                        .or_default()
                        .insert(agent_id.to_owned());
                }
            }
        }
    }
    assert!(
        duplicate_noop_covered,
        "fixtures must cover exact duplicate hook delivery as a no-op"
    );
    assert!(
        agent_ids_by_type
            .values()
            .any(|agent_ids| agent_ids.len() >= 2),
        "fixtures must keep concurrent same-type agents unambiguous by agent_id"
    );
}

fn required_string(name: &str, context: &str, value: &Value, field: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| panic!("{name}: {context} requires string {field}"))
        .to_owned()
}

fn validate_claude_evidence_line(
    name: &str,
    index: usize,
    source: ClaudeRawInputSource,
    value: &Value,
) {
    match source {
        ClaudeRawInputSource::Stream => {
            let message_type = value["type"]
                .as_str()
                .unwrap_or_else(|| panic!("{name}: stream line {index} type"));
            if message_type == "system" {
                let subtype = value["subtype"]
                    .as_str()
                    .unwrap_or_else(|| panic!("{name}: system line {index} subtype"));
                match subtype {
                    "hook_started" => {
                        required_string(name, subtype, value, "hook_name");
                        required_string(name, subtype, value, "hook_event");
                        required_string(name, subtype, value, "hook_id");
                        required_string(name, subtype, value, "uuid");
                        required_string(name, subtype, value, "session_id");
                        assert!(
                            value.get("agent_id").is_none(),
                            "{name}: emitted hook line must not invent agent_id"
                        );
                        assert!(
                            value.get("tool_use_id").is_none(),
                            "{name}: emitted hook line must not invent tool_use_id"
                        );
                        assert!(value.get("hook_event_name").is_none());
                        assert!(value.get("outcome").is_none());
                        assert!(value.get("output").is_none());
                    }
                    "hook_response" => {
                        required_string(name, subtype, value, "hook_name");
                        required_string(name, subtype, value, "hook_event");
                        required_string(name, subtype, value, "hook_id");
                        required_string(name, subtype, value, "outcome");
                        required_string(name, subtype, value, "output");
                        assert!(value.get("stdout").and_then(Value::as_str).is_some());
                        assert!(value.get("stderr").and_then(Value::as_str).is_some());
                        assert!(value.get("exit_code").and_then(Value::as_i64).is_some());
                        required_string(name, subtype, value, "uuid");
                        required_string(name, subtype, value, "session_id");
                        assert!(
                            value.get("agent_id").is_none(),
                            "{name}: emitted hook line must not invent agent_id"
                        );
                        assert!(
                            value.get("tool_use_id").is_none(),
                            "{name}: emitted hook line must not invent tool_use_id"
                        );
                        assert!(value.get("hook_event_name").is_none());
                    }
                    "task_started" => {
                        required_string(name, subtype, value, "task_id");
                        required_string(name, subtype, value, "tool_use_id");
                        required_string(name, subtype, value, "description");
                        required_string(name, subtype, value, "uuid");
                        required_string(name, subtype, value, "session_id");
                        assert!(
                            value.get("subagent_type").is_none(),
                            "{name}: Claude 2.1.218 task_started uses description"
                        );
                        assert!(
                            value.get("agent_id").is_none(),
                            "{name}: Task lifecycle is not stable actor identity"
                        );
                    }
                    "task_updated" => {
                        required_string(name, subtype, value, "task_id");
                        required_string(name, subtype, value, "uuid");
                        required_string(name, subtype, value, "session_id");
                        assert!(
                            value
                                .get("patch")
                                .and_then(Value::as_object)
                                .and_then(|patch| patch.get("status"))
                                .and_then(Value::as_str)
                                .is_some(),
                            "{name}: task_updated requires patch.status"
                        );
                        assert!(
                            value.get("status").is_none(),
                            "{name}: task_updated status is nested under patch"
                        );
                        assert!(value.get("agent_id").is_none());
                        assert!(value.get("tool_use_id").is_none());
                    }
                    "task_notification" => {
                        required_string(name, subtype, value, "task_id");
                        required_string(name, subtype, value, "status");
                        required_string(name, subtype, value, "output_file");
                        required_string(name, subtype, value, "summary");
                        required_string(name, subtype, value, "uuid");
                        required_string(name, subtype, value, "session_id");
                        assert!(value.get("agent_id").is_none());
                        assert!(value.get("tool_use_id").is_none());
                    }
                    _ => panic!("{name}: unsupported system subtype {subtype}"),
                }
            } else if message_type == "assistant" || message_type == "user" {
                required_string(name, message_type, value, "session_id");
                required_string(name, message_type, value, "uuid");
                required_string(name, message_type, value, "parent_tool_use_id");
                assert!(
                    value.get("agent_id").is_none(),
                    "{name}: forwarded {message_type} does not carry stable actor identity"
                );
                let message = value
                    .get("message")
                    .and_then(Value::as_object)
                    .unwrap_or_else(|| panic!("{name}: forwarded {message_type} message"));
                assert!(message.get("content").and_then(Value::as_array).is_some());
                if message_type == "assistant" {
                    assert!(message.get("id").and_then(Value::as_str).is_some());
                } else {
                    assert_eq!(message.get("role"), Some(&json!("user")));
                }
            } else if message_type == "stream_event" {
                required_string(name, message_type, value, "session_id");
                required_string(name, message_type, value, "uuid");
                assert!(value.get("parent_tool_use_id").is_some());
                let content_block = value
                    .pointer("/event/content_block")
                    .and_then(Value::as_object)
                    .unwrap_or_else(|| panic!("{name}: stream_event content block"));
                assert_eq!(content_block.get("name"), Some(&json!("Task")));
                assert!(
                    value.get("agent_id").is_none(),
                    "{name}: Task tool rows do not establish actor identity"
                );
            } else {
                panic!("{name}: unsupported stream message type {message_type}");
            }
        }
        ClaudeRawInputSource::HookInput => {
            let session_id = required_string(name, "hook input", value, "session_id");
            let transcript_path = required_string(name, "hook input", value, "transcript_path");
            required_string(name, "hook input", value, "cwd");
            let event = required_string(name, "hook input", value, "hook_event_name");
            assert!(
                value.get("parent_agent_id").is_none(),
                "{name}: documented hook input does not prove parent lineage"
            );
            if let Some(agent_id) = value.get("agent_id").and_then(Value::as_str) {
                required_string(name, "subagent hook input", value, "agent_type");
                assert!(
                    !transcript_path.contains(agent_id),
                    "{name}: BaseHookInput transcript_path is the main session transcript, not the agent transcript"
                );
            }
            assert!(
                transcript_path.contains(&session_id),
                "{name}: fixture main transcript sentinel must identify its session"
            );
            assert!(
                event == "SubagentStop" || value.get("agent_transcript_path").is_none(),
                "{name}: only SubagentStop documents agent_transcript_path"
            );
            match event.as_str() {
                "SubagentStart" => {
                    required_string(name, "SubagentStart", value, "agent_id");
                    required_string(name, "SubagentStart", value, "agent_type");
                }
                "SubagentStop" => {
                    required_string(name, "SubagentStop", value, "agent_id");
                    required_string(name, "SubagentStop", value, "agent_type");
                    let agent_transcript_path =
                        required_string(name, "SubagentStop", value, "agent_transcript_path");
                    assert_ne!(
                        transcript_path, agent_transcript_path,
                        "{name}: SubagentStop main and agent transcript paths must remain distinct"
                    );
                    assert!(
                        value
                            .get("stop_hook_active")
                            .and_then(Value::as_bool)
                            .is_some(),
                        "{name}: SubagentStop requires stop_hook_active"
                    );
                    assert!(
                        value
                            .get("last_assistant_message")
                            .and_then(Value::as_str)
                            .is_some(),
                        "{name}: SubagentStop requires last_assistant_message"
                    );
                }
                "PreToolUse" => {
                    required_string(name, "PreToolUse", value, "tool_use_id");
                    required_string(name, "PreToolUse", value, "tool_name");
                    assert!(value.get("tool_input").and_then(Value::as_object).is_some());
                    assert!(value.get("tool_response").is_none());
                    assert!(value.get("error").is_none());
                }
                "PostToolUse" => {
                    required_string(name, "PostToolUse", value, "tool_use_id");
                    let tool_name = required_string(name, "PostToolUse", value, "tool_name");
                    assert!(value.get("tool_input").and_then(Value::as_object).is_some());
                    let tool_response = value
                        .get("tool_response")
                        .and_then(Value::as_object)
                        .unwrap_or_else(|| panic!("{name}: PostToolUse requires tool_response"));
                    assert!(value.get("error").is_none());
                    if tool_name == "Bash" {
                        assert!(
                            tool_response
                                .get("stdout")
                                .and_then(Value::as_str)
                                .is_some()
                        );
                        assert!(
                            tool_response
                                .get("stderr")
                                .and_then(Value::as_str)
                                .is_some()
                        );
                        assert!(
                            tool_response
                                .get("interrupted")
                                .and_then(Value::as_bool)
                                .is_some()
                        );
                        assert!(
                            tool_response
                                .get("isImage")
                                .and_then(Value::as_bool)
                                .is_some()
                        );
                        assert!(tool_response.get("exit_code").is_none());
                        assert!(tool_response.get("output").is_none());
                    }
                }
                "PostToolUseFailure" => {
                    required_string(name, "PostToolUseFailure", value, "tool_use_id");
                    required_string(name, "PostToolUseFailure", value, "tool_name");
                    required_string(name, "PostToolUseFailure", value, "error");
                    assert!(value.get("tool_input").and_then(Value::as_object).is_some());
                    assert!(value.get("tool_response").is_none());
                }
                _ => panic!("{name}: unsupported documented hook input {event}"),
            }
        }
        ClaudeRawInputSource::CapabilityProbe => {
            assert!(value.get("version").is_some());
            assert!(value.get("help_flags").and_then(Value::as_array).is_some());
        }
        ClaudeRawInputSource::BaseLaunch => {
            assert_eq!(value["functional"], true);
            assert!(value.get("arguments").and_then(Value::as_array).is_some());
        }
    }
}

fn task_2_claude_activity_projection(fixture: &ClaudeActivityFixture) -> Vec<Value> {
    let mut tracker = ClaudeActivityFixtureAdapter::new("session-root");
    let mut projected = Vec::new();
    for (input_index, input) in fixture.raw_input_lines.iter().enumerate() {
        let value: Value = serde_json::from_str(&input.line).expect("validated fixture input");
        let source = match input.source {
            ClaudeRawInputSource::BaseLaunch => ClaudeActivityInputSource::BaseLaunch,
            ClaudeRawInputSource::CapabilityProbe => ClaudeActivityInputSource::CapabilityProbe,
            ClaudeRawInputSource::HookInput => ClaudeActivityInputSource::HookInput,
            ClaudeRawInputSource::Stream => ClaudeActivityInputSource::Stream,
        };
        let emitted_at_ms = fixture_timestamp_ms(fixture, input_index);
        for mutation in tracker
            .handle_value(source, &value, emitted_at_ms)
            .mutations
        {
            projected.push(json!({
                "inputIndex": input_index,
                "mutation": activity_mutation_json(mutation),
            }));
        }
    }
    projected
}

fn fixture_timestamp_ms(fixture: &ClaudeActivityFixture, input_index: usize) -> u64 {
    let timestamp = fixture
        .expected_mutation_input_indexes
        .iter()
        .zip(&fixture.expected_mutations)
        .find_map(|(expected_input_index, mutation)| {
            (*expected_input_index == input_index).then(|| {
                mutation
                    .pointer("/actor/updatedAt")
                    .or_else(|| mutation.pointer("/entry/createdAt"))
                    .and_then(Value::as_str)
            })
        })
        .flatten();
    let Some(timestamp) = timestamp else {
        return 0;
    };
    let parsed =
        OffsetDateTime::parse(timestamp, &Rfc3339).expect("fixture timestamp is already validated");
    u64::try_from(parsed.unix_timestamp_nanos() / 1_000_000)
        .expect("fixture timestamp is after the Unix epoch")
}

fn activity_mutation_json(mutation: ProviderActivityMutation) -> Value {
    let mut value = match mutation {
        ProviderActivityMutation::SetScope {
            capabilities,
            observation_state,
        } => json!({
            "type": "setScope",
            "capabilities": capabilities,
            "observationState": observation_state,
        }),
        ProviderActivityMutation::UpsertActor(actor) => {
            json!({"type": "upsertActor", "actor": actor})
        }
        ProviderActivityMutation::AppendEntry(entry) => {
            json!({"type": "appendEntry", "entry": entry})
        }
        ProviderActivityMutation::SetSectionHealth { .. }
        | ProviderActivityMutation::RemoveActor { .. }
        | ProviderActivityMutation::UpsertWorkItem(_)
        | ProviderActivityMutation::RemoveWorkItem { .. } => {
            panic!("Claude Task 2 must not project unsupported activity mutations")
        }
    };
    compact_fixture_timestamps(&mut value);
    value
}

fn compact_fixture_timestamps(value: &mut Value) {
    match value {
        Value::Array(values) => {
            for value in values {
                compact_fixture_timestamps(value);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                compact_fixture_timestamps(value);
            }
        }
        Value::String(text) => {
            if let Some(prefix) = text.strip_suffix(".000000000Z") {
                *text = format!("{prefix}Z");
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn claude_system(subtype: &str, fields: Value) -> Value {
    let mut value = json!({
        "type": "system",
        "subtype": subtype,
        "session_id": "session-root",
        "uuid": format!("{subtype}-uuid"),
    });
    value
        .as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    value
}

fn claude_result_frame(origin: Option<&str>) -> Value {
    let mut value = json!({
        "type": "result",
        "subtype": "success",
        "is_error": false,
        "stop_reason": "end_turn",
        "session_id": "session-root",
        "uuid": "result-uuid",
    });
    if let Some(kind) = origin {
        value["origin"] = json!({ "kind": kind });
    }
    value
}

fn claude_task_notification_turn() -> Value {
    json!({
        "type": "user",
        "message": {"role": "user", "content": "<task-notification>done</task-notification>"},
        "origin": {"kind": "task-notification"},
        "parent_tool_use_id": null,
        "session_id": "session-root",
        "uuid": "follow-up-user",
    })
}

fn claude_task_started(task_id: &str, task_type: &str, fields: Value) -> Value {
    let mut started = claude_system(
        "task_started",
        json!({"task_id": task_id, "task_type": task_type, "description": task_id}),
    );
    started
        .as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    started
}

/// Idle suspension keeps a Claude session while it reports background work, read from Claude's
/// own `system` task messages: they arrive without hook events and with Chat Activity off.
#[test]
fn claude_runtime_counts_background_work_from_its_stream() {
    let mut runtime = ClaudeProviderRuntime::new("thread-1".to_owned(), "session-root".to_owned());
    runtime.set_agent_activity_enabled(false);
    let mut live = |value: Value| runtime.handle_raw_value(&value, 1_000).background_tasks;

    assert_eq!(
        live(claude_task_started(
            "bash",
            "local_bash",
            json!({"is_backgrounded": true})
        )),
        Some(1),
        "a run_in_background Bash command"
    );
    assert_eq!(
        live(claude_task_started(
            "agent",
            "local_agent",
            json!({"is_backgrounded": true})
        )),
        Some(2)
    );
    assert_eq!(
        live(claude_task_started(
            "foreground",
            "local_agent",
            json!({"is_backgrounded": false})
        )),
        Some(2),
        "a foreground task holds its turn open instead"
    );
    assert_eq!(
        live(claude_task_started(
            "watch",
            "local_bash",
            json!({"ambient": true})
        )),
        Some(2),
        "Claude's own ambient tasks are not the session's work"
    );
    assert_eq!(
        live(claude_system(
            "task_progress",
            json!({"task_id": "agent", "description": "agent", "usage": {"total_tokens": 1, "tool_uses": 1, "duration_ms": 1}})
        )),
        Some(2),
        "progress confirms the count"
    );
    assert_eq!(
        live(claude_system(
            "task_updated",
            json!({"task_id": "foreground", "patch": {"is_backgrounded": true}})
        )),
        Some(3),
        "a foreground task moved to the background"
    );
    assert_eq!(
        live(claude_system(
            "task_updated",
            json!({"task_id": "bash", "patch": {"status": "killed"}})
        )),
        Some(3),
        "a finished background task leaves the turn that answers it owed"
    );
    assert_eq!(
        live(claude_system(
            "background_tasks_changed",
            json!({"tasks": [
                {"task_id": "agent", "task_type": "local_agent", "description": "agent"},
                {"task_id": "watch", "task_type": "local_bash", "description": "watch", "ambient": true}
            ]})
        )),
        Some(2),
        "the complete live set replaces the tasks"
    );
    assert_eq!(
        live(claude_system(
            "task_notification",
            json!({"task_id": "agent", "status": "completed", "output_file": "/tmp/agent", "summary": "Done."})
        )),
        Some(2),
        "the turns that answer the two finished tasks are still owed"
    );
    assert_eq!(
        live(claude_system(
            "background_tasks_changed",
            json!({"tasks": []})
        )),
        Some(2)
    );
    assert_eq!(live(claude_task_notification_turn()), None);
    assert_eq!(
        live(claude_result_frame(None)),
        None,
        "an ordinary turn's result does not settle the owed answer"
    );
    assert_eq!(
        live(claude_result_frame(Some("task-notification"))),
        Some(1),
        "each finished task gets its own answer"
    );
    assert_eq!(
        live(claude_result_frame(Some("task-notification"))),
        Some(0)
    );
    assert_eq!(
        live(claude_task_started(
            "inline",
            "local_agent",
            json!({"is_backgrounded": false})
        )),
        Some(0)
    );
    assert_eq!(
        live(claude_system(
            "task_notification",
            json!({"task_id": "inline", "status": "completed", "output_file": "/tmp/inline", "summary": "Done."})
        )),
        Some(0),
        "a foreground task is answered in its own turn"
    );
    assert_eq!(live(claude_system("init", json!({}))), None);
}

/// Claude answers a finished background task with a turn it starts itself; it gets a turn of its
/// own. Not while a delivered message awaits its replay: on resume Claude can run that turn before
/// it reads the message, and its result is ignored (#93).
#[test]
fn claude_runtime_gives_its_own_follow_up_turn_a_turn() {
    let mut runtime = ClaudeProviderRuntime::new("thread-1".to_owned(), "session-root".to_owned());
    runtime.start_turn(TurnInput {
        turn_id: "delivered".to_owned(),
        input: "start".to_owned(),
    });
    let delivered = runtime.handle_raw_value(&claude_result_frame(None), 1_000);
    assert!(delivered.events.iter().any(|event| {
        event.event_type == "turn.completed" && event.turn_id.as_deref() == Some("delivered")
    }));

    let follow_up = runtime.handle_raw_value(&claude_task_notification_turn(), 2_000);
    let started = follow_up
        .events
        .iter()
        .find(|event| event.event_type == "turn.started")
        .expect("Claude's own turn starts a turn");
    let follow_up_turn = started.turn_id.clone().expect("a turn id");
    assert_ne!(follow_up_turn, "delivered");
    let ended = runtime.handle_raw_value(&claude_result_frame(Some("task-notification")), 3_000);
    assert!(ended.events.iter().any(|event| {
        event.event_type == "turn.completed" && event.turn_id.as_deref() == Some(&follow_up_turn)
    }));

    runtime.start_turn(TurnInput {
        turn_id: "resumed".to_owned(),
        input: "next".to_owned(),
    });
    runtime.await_turn_input(7);
    let early = runtime.handle_raw_value(&claude_task_notification_turn(), 4_000);
    assert!(
        early
            .events
            .iter()
            .all(|event| event.event_type != "turn.started"),
        "the delivered message's turn stays current"
    );
    assert!(
        runtime
            .handle_raw_value(&claude_result_frame(Some("task-notification")), 5_000)
            .events
            .is_empty(),
        "the result before the replay is ignored"
    );
}

/// Past the bound a task is not tracked, but the count never reaches zero until Claude's complete
/// live set shows nothing runs.
#[test]
fn claude_runtime_keeps_reporting_work_past_its_task_bound() {
    let mut runtime = ClaudeProviderRuntime::new("thread-1".to_owned(), "session-root".to_owned());
    let mut last = None;
    for index in 0..201 {
        last = runtime
            .handle_raw_value(
                &claude_task_started(
                    &format!("task-{index}"),
                    "local_bash",
                    json!({"is_backgrounded": true}),
                ),
                1_000,
            )
            .background_tasks;
    }
    assert_eq!(last, Some(201));
    for index in 0..200 {
        runtime.handle_raw_value(
            &claude_system(
                "task_notification",
                json!({"task_id": format!("task-{index}"), "status": "completed", "output_file": "/tmp/task", "summary": "Done."}),
            ),
            2_000,
        );
        last = runtime
            .handle_raw_value(&claude_result_frame(Some("task-notification")), 2_000)
            .background_tasks;
    }
    assert_eq!(last, Some(1), "the untracked task still counts");
    let settled = runtime
        .handle_raw_value(
            &claude_system("background_tasks_changed", json!({"tasks": []})),
            3_000,
        )
        .background_tasks;
    assert_eq!(settled, Some(0));
}

/// A task that finishes during a turn needs no bridge: that turn's completion arms a fresh idle
/// deadline, and a separate answer starts its own turn right after it.
#[test]
fn claude_runtime_owes_no_answer_for_a_task_finished_during_a_turn() {
    let mut runtime = ClaudeProviderRuntime::new("thread-1".to_owned(), "session-root".to_owned());
    runtime.start_turn(TurnInput {
        turn_id: "delivered".to_owned(),
        input: "start".to_owned(),
    });
    let started = runtime.handle_raw_value(
        &claude_task_started("bash", "local_bash", json!({"is_backgrounded": true})),
        1_000,
    );
    assert_eq!(started.background_tasks, Some(1));
    let finished = runtime.handle_raw_value(
        &claude_system(
            "task_notification",
            json!({"task_id": "bash", "status": "completed", "output_file": "/tmp/bash", "summary": "Done."}),
        ),
        2_000,
    );
    assert_eq!(finished.background_tasks, Some(0));
}

/// #165: a background task finishes while a slow turn (`/compact`) runs, and Claude replays its
/// notification just before that turn's `result`. The answer Claude then runs by itself gets a
/// turn of its own, opened with that `result`, so the idle deadline the `result` arms cannot
/// stop Claude mid-answer. Its output and `result` belong to the new turn.
#[test]
fn claude_runtime_opens_a_follow_up_replayed_before_the_previous_result() {
    let mut runtime = ClaudeProviderRuntime::new("thread-1".to_owned(), "session-root".to_owned());
    runtime.start_turn(TurnInput {
        turn_id: "compact".to_owned(),
        input: "/compact".to_owned(),
    });
    for frame in [
        claude_task_started("bash", "local_bash", json!({"is_backgrounded": true})),
        claude_system(
            "task_notification",
            json!({"task_id": "bash", "status": "completed", "output_file": "/tmp/bash", "summary": "Done."}),
        ),
    ] {
        runtime.handle_raw_value(&frame, 1_000);
    }
    let replay = runtime.handle_raw_value(&claude_task_notification_turn(), 2_000);
    assert!(
        replay
            .events
            .iter()
            .all(|event| event.event_type != "turn.started"),
        "the open turn stays current until its result"
    );

    let closed = runtime.handle_raw_value(&claude_result_frame(None), 3_000);
    let kinds = closed
        .events
        .iter()
        .map(|event| (event.event_type.as_str(), event.turn_id.as_deref()))
        .collect::<Vec<_>>();
    assert_eq!(kinds.len(), 2, "{kinds:?}");
    assert_eq!(kinds[0], ("turn.completed", Some("compact")));
    assert_eq!(kinds[1].0, "turn.started", "{kinds:?}");
    let follow_up_turn = kinds[1].1.expect("a turn id").to_owned();
    assert_ne!(follow_up_turn, "compact");

    let answer = runtime.handle_raw_value(
        &json!({
            "type": "stream_event",
            "session_id": "session-root",
            "uuid": "follow-up-delta",
            "parent_tool_use_id": null,
            "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Done."}}
        }),
        4_000,
    );
    assert!(
        answer
            .events
            .iter()
            .all(|event| event.turn_id.as_deref() == Some(&follow_up_turn)),
        "the answer streams under its own turn"
    );
    let ended = runtime.handle_raw_value(&claude_result_frame(Some("task-notification")), 5_000);
    let kinds = ended
        .events
        .iter()
        .map(|event| (event.event_type.as_str(), event.turn_id.as_deref()))
        .collect::<Vec<_>>();
    assert_eq!(
        kinds,
        vec![("turn.completed", Some(follow_up_turn.as_str()))],
        "the answer's result ends its turn and opens no other"
    );
}

/// A delivered `/compact` is never replayed, so its write still awaits a replay when a task's
/// notification arrives. The driver acknowledges the command from its `result`; the notification
/// must still open its own turn then.
#[test]
fn claude_runtime_keeps_a_follow_up_replayed_while_a_local_command_awaits_its_replay() {
    let mut runtime = ClaudeProviderRuntime::new("thread-1".to_owned(), "session-root".to_owned());
    runtime.start_turn(TurnInput {
        turn_id: "compact".to_owned(),
        input: "/compact".to_owned(),
    });
    runtime.await_turn_input(7);
    let replay = runtime.handle_raw_value(&claude_task_notification_turn(), 1_000);
    assert!(
        replay
            .events
            .iter()
            .all(|event| event.event_type != "turn.started"),
        "the open turn stays current until its result"
    );

    runtime.stop_awaiting_turn_input(7);
    let closed = runtime.handle_raw_value(&claude_result_frame(None), 2_000);
    let kinds = closed
        .events
        .iter()
        .map(|event| (event.event_type.as_str(), event.turn_id.as_deref()))
        .collect::<Vec<_>>();
    assert_eq!(kinds.len(), 2, "{kinds:?}");
    assert_eq!(kinds[0], ("turn.completed", Some("compact")));
    assert_eq!(kinds[1].0, "turn.started", "{kinds:?}");
}

/// After a resume Claude can answer a finished task before it reads the new message. That answer's
/// result is ignored while the message awaits its replay, so the notification it answered must not
/// open another turn once the message's own turn ends.
#[test]
fn claude_runtime_opens_no_follow_up_for_one_answered_before_the_message_replay() {
    let mut runtime = ClaudeProviderRuntime::new("thread-1".to_owned(), "session-root".to_owned());
    runtime.start_turn(TurnInput {
        turn_id: "message".to_owned(),
        input: "next step".to_owned(),
    });
    runtime.await_turn_input(7);
    runtime.handle_raw_value(&claude_task_notification_turn(), 1_000);
    let stale = runtime.handle_raw_value(&claude_result_frame(Some("task-notification")), 2_000);
    assert!(stale.events.is_empty(), "{:?}", stale.events);

    runtime.stop_awaiting_turn_input(7);
    let closed = runtime.handle_raw_value(&claude_result_frame(None), 3_000);
    let kinds = closed
        .events
        .iter()
        .map(|event| (event.event_type.as_str(), event.turn_id.as_deref()))
        .collect::<Vec<_>>();
    assert_eq!(kinds, vec![("turn.completed", Some("message"))]);
}

/// Two tasks finish during one turn: each answer opens as the turn before it ends. An interrupted
/// turn opens none, since the user stopped Claude.
#[test]
fn claude_runtime_opens_each_follow_up_in_order_and_none_after_an_interrupt() {
    let mut runtime = ClaudeProviderRuntime::new("thread-1".to_owned(), "session-root".to_owned());
    runtime.start_turn(TurnInput {
        turn_id: "delivered".to_owned(),
        input: "start".to_owned(),
    });
    runtime.handle_raw_value(&claude_task_notification_turn(), 1_000);
    runtime.handle_raw_value(&claude_task_notification_turn(), 1_000);
    let started = |output: &claude::runtime::ClaudeRuntimeOutput| {
        output
            .events
            .iter()
            .filter(|event| event.event_type == "turn.started")
            .count()
    };
    let first = runtime.handle_raw_value(&claude_result_frame(None), 2_000);
    assert_eq!(started(&first), 1);
    let second = runtime.handle_raw_value(&claude_result_frame(Some("task-notification")), 3_000);
    assert_eq!(started(&second), 1, "the second answer follows the first");
    let last = runtime.handle_raw_value(&claude_result_frame(Some("task-notification")), 4_000);
    assert_eq!(started(&last), 0);

    runtime.start_turn(TurnInput {
        turn_id: "stopped".to_owned(),
        input: "work".to_owned(),
    });
    runtime.handle_raw_value(&claude_task_notification_turn(), 5_000);
    let mut interrupted = claude_result_frame(None);
    interrupted["subtype"] = json!("error_during_execution");
    interrupted["is_error"] = json!(true);
    interrupted["stop_reason"] = json!("interrupted");
    let stopped = runtime.handle_raw_value(&interrupted, 6_000);
    assert_eq!(started(&stopped), 0, "an interrupted turn opens no answer");
}

/// #167: two subagents stream tool blocks at the same index at once. Each block's fragments
/// build its own input.
#[test]
fn concurrent_subagent_tool_inputs_stream_into_their_own_blocks() {
    let mut runtime = ClaudeProviderRuntime::new("thread-1".to_owned(), "session-root".to_owned());
    runtime.start_turn(TurnInput {
        turn_id: "turn-1".to_owned(),
        input: "delegate".to_owned(),
    });
    let frame = |parent: &str, event: Value| -> ClaudeMessage {
        serde_json::from_value(json!({
            "type": "stream_event",
            "session_id": "session-root",
            "uuid": "stream",
            "parent_tool_use_id": parent,
            "event": event,
        }))
        .expect("a stream frame")
    };
    let start = |id: &str, name: &str| json!({"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}}});
    let delta = |fragment: &str| json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": fragment}});
    let stop = json!({"type": "content_block_stop", "index": 1});
    let mut events = Vec::new();
    for message in [
        frame("agent-a", start("tool-a", "Bash")),
        frame("agent-b", start("tool-b", "Read")),
        frame("agent-a", delta("{\"command\":")),
        frame("agent-b", delta("{\"file_path\":")),
        frame("agent-a", delta("\"ls\"}")),
        frame("agent-b", delta("\"README.md\"}")),
        frame("agent-b", stop.clone()),
        frame("agent-a", stop),
    ] {
        events.extend(runtime.handle_message(message));
    }
    let updates = events
        .iter()
        .filter(|event| event.event_type == "item.updated")
        .map(|event| {
            (
                event.provider_refs.as_ref().unwrap()["providerItemId"].clone(),
                event.payload["data"]["input"].clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        updates,
        vec![
            (json!("tool-b"), json!({"file_path": "README.md"})),
            (json!("tool-a"), json!({"command": "ls"})),
        ]
    );
}
