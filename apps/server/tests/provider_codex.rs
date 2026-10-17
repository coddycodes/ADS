use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use bibcode_server::activity::{
    ActivityActorSummary, ActivityCapabilities, ActivityEntry, ActivityLifecycle,
    ActivityObservationState, ActivityRecordKind, ActivityRepository, ActivityScopeSeed,
    ActivitySection, ActivitySectionObservationState, ActivityWorkItemSummary,
    ProviderActivityMutation,
};
use bibcode_server::persistence::{Database, run_migrations};
use bibcode_server::provider::codex::{
    BuildTurnStartInput, CodexActivityFixtureAdapter, CodexRuntimeMode, CodexSessionOptions,
    CodexSessionRuntime, ConnectionConfig, IncomingEvent, JsonRpcConnection, RuntimeEvent,
    RuntimeEventStableView, build_initialize_params, build_turn_start_params,
    is_recoverable_thread_resume_error, parse_model_list_response, parse_skills_list_response,
    probe_provider,
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};

#[test]
fn missing_codex_rollout_is_a_recoverable_resume_failure() {
    assert!(is_recoverable_thread_resume_error(
        "no rollout is available for thread id 019f5662-6e5e-70d1-9074-06a0ba8761d0"
    ));
}

#[test]
fn automatic_codex_model_resolves_to_the_supported_default() {
    let payload = build_turn_start_params(&BuildTurnStartInput {
        thread_id: "provider-thread-1".to_owned(),
        runtime_mode: CodexRuntimeMode::FullAccess,
        client_user_message_id: None,
        prompt: Some("hello".to_owned()),
        attachments: vec![],
        model: Some("auto".to_owned()),
        service_tier: None,
        effort: None,
        interaction_mode: Some("default".to_owned()),
    });
    assert_eq!(payload["model"], "gpt-5.4");
    assert_eq!(payload["collaborationMode"]["settings"]["model"], "gpt-5.4");
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CodexFixtureManifest {
    fixtures: Vec<String>,
    activity_fixtures: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CodexActivityFixture {
    scenario: String,
    initial_actor_ids: Vec<String>,
    inbound_messages: Vec<Value>,
    expected_mutations: Vec<Value>,
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum ExpectedActivityMutation {
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
    UpsertWorkItem {
        work_item: ActivityWorkItemSummary,
    },
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct RequiredNullable<T>(Option<T>);

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ItemStartedNotificationDto {
    thread_id: String,
    turn_id: String,
    item: ThreadItemDto,
    started_at_ms: u64,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ItemCompletedNotificationDto {
    thread_id: String,
    turn_id: String,
    item: ThreadItemDto,
    completed_at_ms: u64,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct ItemStartedOrCompletedDto {
    item: ThreadItemDto,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentMessageDeltaNotificationDto {
    thread_id: String,
    turn_id: String,
    item_id: String,
    delta: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TurnCompletedNotificationDto {
    thread_id: String,
    turn: ThreadTurnDto,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum ThreadItemDto {
    #[serde(rename = "subAgentActivity")]
    SubAgentActivity {
        id: String,
        kind: SubAgentActivityKindDto,
        #[serde(rename = "agentThreadId")]
        agent_thread_id: String,
        #[serde(rename = "agentPath")]
        agent_path: String,
    },
    #[serde(rename = "collabAgentToolCall")]
    CollabAgentToolCall {
        id: String,
        tool: CollabAgentToolDto,
        status: CollabAgentToolCallStatusDto,
        #[serde(rename = "senderThreadId")]
        sender_thread_id: String,
        #[serde(rename = "receiverThreadIds")]
        receiver_thread_ids: Vec<String>,
        prompt: RequiredNullable<String>,
        model: RequiredNullable<String>,
        #[serde(rename = "reasoningEffort")]
        reasoning_effort: RequiredNullable<ReasoningEffortDto>,
        #[serde(rename = "agentsStates")]
        agents_states: HashMap<String, CollabAgentStateDto>,
    },
    #[serde(rename = "dynamicToolCall")]
    DynamicToolCall {
        id: String,
        namespace: RequiredNullable<String>,
        tool: String,
        arguments: Value,
        status: DynamicToolCallStatusDto,
        #[serde(rename = "contentItems")]
        content_items: RequiredNullable<Vec<Value>>,
        success: RequiredNullable<bool>,
        #[serde(rename = "durationMs")]
        duration_ms: RequiredNullable<u64>,
    },
    #[serde(rename = "commandExecution")]
    CommandExecution {
        id: String,
        command: String,
        cwd: String,
        #[serde(rename = "processId")]
        process_id: RequiredNullable<String>,
        source: CommandExecutionSourceDto,
        status: CommandExecutionStatusDto,
        #[serde(rename = "commandActions")]
        command_actions: Vec<CommandActionDto>,
        #[serde(rename = "aggregatedOutput")]
        aggregated_output: RequiredNullable<String>,
        #[serde(rename = "exitCode")]
        exit_code: RequiredNullable<i64>,
        #[serde(rename = "durationMs")]
        duration_ms: RequiredNullable<u64>,
    },
    #[serde(other)]
    Unknown,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum CommandActionDto {
    #[serde(rename = "read")]
    Read {
        command: String,
        name: String,
        path: String,
    },
    #[serde(rename = "listFiles")]
    ListFiles {
        command: String,
        path: RequiredNullable<String>,
    },
    #[serde(rename = "search")]
    Search {
        command: String,
        query: RequiredNullable<String>,
        path: RequiredNullable<String>,
    },
    #[serde(rename = "unknown")]
    Unknown { command: String },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum CollabAgentToolDto {
    SpawnAgent,
    SendInput,
    ResumeAgent,
    Wait,
    CloseAgent,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum SubAgentActivityKindDto {
    Started,
    Interacted,
    Interrupted,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum CollabAgentToolCallStatusDto {
    InProgress,
    Completed,
    Failed,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct CollabAgentStateDto {
    status: CollabAgentStatusDto,
    message: RequiredNullable<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum CollabAgentStatusDto {
    PendingInit,
    Running,
    Interrupted,
    Completed,
    Errored,
    Shutdown,
    NotFound,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum ReasoningEffortDto {
    None,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum DynamicToolCallStatusDto {
    InProgress,
    Completed,
    Failed,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum CommandExecutionSourceDto {
    Agent,
    UserShell,
    UnifiedExecStartup,
    UnifiedExecInteraction,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum CommandExecutionStatusDto {
    InProgress,
    Completed,
    Failed,
    Declined,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadListResponseDto {
    data: Vec<ThreadDto>,
    next_cursor: RequiredNullable<String>,
    backwards_cursor: RequiredNullable<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct ThreadReadResponseDto {
    thread: ThreadDto,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadDto {
    id: String,
    extra: RequiredNullable<HashMap<String, Value>>,
    session_id: String,
    forked_from_id: RequiredNullable<String>,
    parent_thread_id: RequiredNullable<String>,
    preview: String,
    ephemeral: bool,
    history_mode: ThreadHistoryModeDto,
    model_provider: String,
    created_at: u64,
    updated_at: u64,
    recency_at: RequiredNullable<u64>,
    status: ThreadStatusDto,
    path: RequiredNullable<String>,
    cwd: String,
    cli_version: String,
    source: SessionSourceDto,
    can_accept_direct_input: RequiredNullable<bool>,
    thread_source: RequiredNullable<String>,
    agent_nickname: RequiredNullable<String>,
    agent_role: RequiredNullable<String>,
    git_info: RequiredNullable<HashMap<String, Value>>,
    name: RequiredNullable<String>,
    turns: Vec<ThreadTurnDto>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum ThreadHistoryModeDto {
    Legacy,
    Paginated,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
enum ThreadStatusDto {
    NotLoaded,
    Idle,
    SystemError,
    Active {
        active_flags: Vec<ThreadActiveFlagDto>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum ThreadActiveFlagDto {
    WaitingOnApproval,
    WaitingOnUserInput,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum SessionSourceDto {
    Named(SessionSourceNameDto),
    Custom {
        custom: String,
    },
    SubAgent {
        #[serde(rename = "subAgent")]
        sub_agent: SubAgentSourceDto,
    },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum SessionSourceNameDto {
    Cli,
    Vscode,
    Exec,
    AppServer,
    Unknown,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum SubAgentSourceDto {
    Named(SubAgentSourceNameDto),
    ThreadSpawn { thread_spawn: ThreadSpawnSourceDto },
    Other { other: String },
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SubAgentSourceNameDto {
    Review,
    Compact,
    MemoryConsolidation,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct ThreadSpawnSourceDto {
    parent_thread_id: String,
    depth: u64,
    agent_path: RequiredNullable<String>,
    agent_nickname: RequiredNullable<String>,
    agent_role: RequiredNullable<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadTurnDto {
    id: String,
    items: Vec<ThreadItemDto>,
    items_view: TurnItemsViewDto,
    status: TurnStatusDto,
    error: RequiredNullable<HashMap<String, Value>>,
    started_at: RequiredNullable<u64>,
    completed_at: RequiredNullable<u64>,
    duration_ms: RequiredNullable<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum TurnItemsViewDto {
    NotLoaded,
    Summary,
    Full,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
enum TurnStatusDto {
    Completed,
    Interrupted,
    Failed,
    InProgress,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct AppServerErrorDto {
    code: i64,
    message: String,
    data: RequiredNullable<Value>,
}

#[test]
fn codex_collaboration_activity_fixtures_are_manifest_driven_and_projected() {
    let manifest: CodexFixtureManifest =
        serde_json::from_value(fixture("manifest.json")).expect("Codex fixture manifest");
    assert_eq!(
        manifest.activity_fixtures,
        [
            "trace-child-activity.json",
            "trace-collaboration.json",
            "trace-reconcile.json",
            "trace-schema-downgrade.json",
        ]
    );
    assert!(
        manifest.fixtures.windows(2).all(|pair| pair[0] < pair[1]),
        "the Codex fixture manifest must stay ordered"
    );

    let manifest_entries = manifest.fixtures.iter().collect::<HashSet<_>>();
    let mut fixtures = Vec::with_capacity(manifest.activity_fixtures.len());
    let mut scenarios = HashSet::new();
    for name in &manifest.activity_fixtures {
        assert!(
            manifest_entries.contains(name),
            "activity fixture {name} must also appear in the complete fixture manifest"
        );
        let activity_fixture: CodexActivityFixture =
            serde_json::from_value(fixture(name)).expect("valid Codex activity fixture");
        assert!(
            scenarios.insert(activity_fixture.scenario.clone()),
            "activity fixture scenarios must be unique"
        );
        validate_codex_activity_fixture(name, &activity_fixture);
        fixtures.push(activity_fixture);
    }

    let expected = fixtures
        .iter()
        .map(|activity_fixture| {
            json!({
                "scenario": activity_fixture.scenario,
                "mutations": activity_fixture.expected_mutations,
            })
        })
        .collect::<Vec<_>>();
    let actual = fixtures
        .iter()
        .map(|activity_fixture| {
            json!({
                "scenario": activity_fixture.scenario,
                "mutations": task_2_codex_activity_projection(activity_fixture),
            })
        })
        .collect::<Vec<_>>();

    assert_eq!(
        actual, expected,
        "Task 2 must replace the empty projection seam with Codex activity tracker output"
    );
}

#[test]
fn codex_activity_attributes_interrupted_child_turn_state() {
    let mut tracker = CodexActivityFixtureAdapter::new(Some("root-1"));
    tracker.seed_actor("child-1");
    let params = json!({
        "threadId": "child-1",
        "turn": {
            "id": "child-turn-1",
            "status": "interrupted",
            "error": null,
            "startedAt": 1784894400_u64,
            "completedAt": 1784894401_u64
        }
    });

    let output = tracker.handle_notification("turn/completed", &params, 1784894401000);
    let entry = output
        .mutations
        .iter()
        .find_map(|mutation| match mutation {
            bibcode_server::activity::ProviderActivityMutation::AppendEntry(entry) => Some(entry),
            _ => None,
        })
        .expect("interrupted child turn entry");

    assert_eq!(
        entry.kind,
        bibcode_server::activity::ActivityEntryKind::State
    );
    assert_eq!(entry.title, "Turn interrupted");
    assert_eq!(
        entry.tone,
        bibcode_server::activity::ActivityEntryTone::Warning
    );
    assert_eq!(entry.owner_id, "codex:thread:child-1");
}

#[test]
fn codex_activity_rejects_foreign_collaboration_and_root_receivers() {
    let mut tracker = CodexActivityFixtureAdapter::new(Some("root-1"));
    let foreign = json!({
        "threadId": "foreign-sender",
        "turnId": "turn-1",
        "item": {
            "id": "spawn-foreign",
            "type": "collabAgentToolCall",
            "tool": "spawnAgent",
            "status": "inProgress",
            "senderThreadId": "foreign-sender",
            "receiverThreadIds": ["foreign-child"],
            "agentsStates": {}
        },
        "startedAtMs": 1784894400000_u64
    });
    let root_receiver = json!({
        "threadId": "root-1",
        "turnId": "turn-1",
        "item": {
            "id": "spawn-root",
            "type": "collabAgentToolCall",
            "tool": "spawnAgent",
            "status": "inProgress",
            "senderThreadId": "root-1",
            "receiverThreadIds": ["root-1", "child-1"],
            "agentsStates": {}
        },
        "startedAtMs": 1784894401000_u64
    });

    let foreign_output = tracker.handle_notification("item/started", &foreign, 1784894400000);
    let root_output = tracker.handle_notification("item/started", &root_receiver, 1784894401000);

    assert!(foreign_output.mutations.is_empty());
    assert!(matches!(
        root_output.mutations.as_slice(),
        [bibcode_server::activity::ProviderActivityMutation::UpsertActor(actor)]
            if actor.id == "codex:thread:child-1"
    ));
    assert_eq!(tracker.state_counts().actors, 1);
}

#[test]
fn codex_activity_rejects_self_parent_edges_before_mutation() {
    let mut tracker = CodexActivityFixtureAdapter::new(Some("root-1"));
    let spawn_child = json!({
        "threadId": "root-1",
        "turnId": "turn-1",
        "item": {
            "id": "spawn-child",
            "type": "collabAgentToolCall",
            "tool": "spawnAgent",
            "status": "inProgress",
            "senderThreadId": "root-1",
            "receiverThreadIds": ["child-a"],
            "agentsStates": {}
        }
    });
    let self_receiver = json!({
        "threadId": "child-a",
        "turnId": "turn-2",
        "item": {
            "id": "self-receiver",
            "type": "collabAgentToolCall",
            "tool": "sendInput",
            "status": "completed",
            "senderThreadId": "child-a",
            "receiverThreadIds": ["child-a"],
            "agentsStates": {
                "child-a": { "status": "running", "message": "still working" }
            }
        }
    });

    assert_eq!(
        tracker
            .handle_notification("item/started", &spawn_child, 1_000)
            .mutations
            .len(),
        1
    );
    assert!(
        tracker
            .handle_notification("item/completed", &self_receiver, 1_001)
            .mutations
            .is_empty()
    );

    let status = tracker.handle_notification(
        "thread/status/changed",
        &json!({ "threadId": "child-a", "status": {"type": "idle"} }),
        1_002,
    );
    assert!(matches!(
        status.mutations.as_slice(),
        [bibcode_server::activity::ProviderActivityMutation::UpsertActor(actor)]
            if actor.parent_actor_id.is_none()
    ));
}

#[test]
fn codex_activity_rejects_ancestor_reparenting_cycles() {
    let mut tracker = CodexActivityFixtureAdapter::new(Some("root-1"));
    let collaboration = |sender: &str, receiver: &str, item_id: &str| {
        json!({
            "threadId": sender,
            "turnId": format!("turn-{item_id}"),
            "item": {
                "id": item_id,
                "type": "collabAgentToolCall",
                "tool": "spawnAgent",
                "status": "inProgress",
                "senderThreadId": sender,
                "receiverThreadIds": [receiver],
                "agentsStates": {}
            }
        })
    };

    assert_eq!(
        tracker
            .handle_notification(
                "item/started",
                &collaboration("root-1", "child-a", "spawn-a"),
                1_000,
            )
            .mutations
            .len(),
        1
    );
    assert_eq!(
        tracker
            .handle_notification(
                "item/started",
                &collaboration("child-a", "child-b", "spawn-b"),
                1_001,
            )
            .mutations
            .len(),
        1
    );

    let cycle = tracker.handle_notification(
        "item/completed",
        &collaboration("child-b", "child-a", "cycle-a-b"),
        1_002,
    );
    assert!(cycle.mutations.is_empty());

    let status = tracker.handle_notification(
        "thread/status/changed",
        &json!({ "threadId": "child-a", "status": {"type": "idle"} }),
        1_003,
    );
    assert!(matches!(
        status.mutations.as_slice(),
        [bibcode_server::activity::ProviderActivityMutation::UpsertActor(actor)]
            if actor.parent_actor_id.is_none()
    ));
}

#[test]
fn codex_activity_preserves_established_non_authoritative_parents() {
    let mut tracker = CodexActivityFixtureAdapter::new(Some("root-1"));
    let root_spawn = |receiver: &str, item_id: &str| {
        json!({
            "threadId": "root-1",
            "turnId": "turn-root",
            "item": {
                "id": item_id,
                "type": "collabAgentToolCall",
                "tool": "spawnAgent",
                "status": "inProgress",
                "senderThreadId": "root-1",
                "receiverThreadIds": [receiver],
                "agentsStates": {}
            }
        })
    };
    tracker.handle_notification("item/started", &root_spawn("child-a", "spawn-a"), 1_000);
    tracker.handle_notification("item/started", &root_spawn("child-c", "spawn-c"), 1_001);
    let sibling_update = json!({
        "threadId": "child-c",
        "turnId": "turn-c",
        "item": {
            "id": "update-a",
            "type": "collabAgentToolCall",
            "tool": "sendInput",
            "status": "completed",
            "senderThreadId": "child-c",
            "receiverThreadIds": ["child-a"],
            "agentsStates": {
                "child-a": { "status": "waiting", "message": "waiting" }
            }
        }
    });

    let output = tracker.handle_notification("item/completed", &sibling_update, 1_002);

    assert!(matches!(
        output.mutations.as_slice(),
        [bibcode_server::activity::ProviderActivityMutation::UpsertActor(actor)]
            if actor.id == "codex:thread:child-a" && actor.parent_actor_id.is_none()
    ));
}

#[test]
fn codex_activity_redacts_raw_command_output_before_display() {
    let mut tracker = CodexActivityFixtureAdapter::new(Some("root-1"));
    tracker.seed_actor("child-1");
    let params = json!({
        "threadId": "child-1",
        "turnId": "turn-1",
        "item": {
            "id": "command-1",
            "type": "commandExecution",
            "status": "completed",
            "aggregatedOutput": "API_TOKEN=secret-value\nAuthorization: Bearer abc123"
        },
        "completedAtMs": 1784894400000_u64
    });

    let output = tracker.handle_notification("item/completed", &params, 1784894400000);
    let entry = output
        .mutations
        .iter()
        .find_map(|mutation| match mutation {
            bibcode_server::activity::ProviderActivityMutation::AppendEntry(entry) => Some(entry),
            _ => None,
        })
        .expect("command entry");
    let encoded = serde_json::to_string(entry).expect("serialize command entry");

    assert_eq!(entry.detail.as_deref(), Some("[redacted command output]"));
    assert!(!encoded.contains("secret-value"));
    assert!(!encoded.contains("abc123"));
}

#[test]
fn codex_activity_prefers_complete_reasoning_summary_without_exposing_raw_reasoning() {
    let mut tracker = CodexActivityFixtureAdapter::new(Some("root-1"));
    tracker.seed_actor("child-1");
    let params = json!({
        "threadId": "child-1",
        "turnId": "child-turn-1",
        "itemId": "reasoning-1",
        "delta": "First official part.",
        "rawReasoning": "must not be mapped"
    });

    let output =
        tracker.handle_notification("item/reasoning/summaryTextDelta", &params, 1784894400000);
    assert!(
        output.mutations.is_empty(),
        "reasoning deltas must remain buffered until item completion"
    );

    let part_added = json!({
        "threadId": "child-1",
        "turnId": "child-turn-1",
        "itemId": "reasoning-2",
        "summaryIndex": 0
    });
    assert!(
        tracker
            .handle_notification(
                "item/reasoning/summaryPartAdded",
                &part_added,
                1784894400001,
            )
            .mutations
            .is_empty(),
        "summaryPartAdded has no documented display text"
    );

    let completed = json!({
        "threadId": "child-1",
        "turnId": "child-turn-1",
        "item": {
            "id": "reasoning-1",
            "type": "reasoning",
            "summary": ["First official part.", "Second official part."],
            "content": ["hidden chain of thought"]
        },
        "completedAtMs": 1784894401000_u64
    });
    let completed_output = tracker.handle_notification("item/completed", &completed, 1784894401000);
    let completed_entry = completed_output
        .mutations
        .iter()
        .find_map(|mutation| match mutation {
            bibcode_server::activity::ProviderActivityMutation::AppendEntry(entry) => Some(entry),
            _ => None,
        })
        .expect("official reasoning completion entry");
    assert_eq!(
        completed_entry.detail.as_deref(),
        Some("First official part.\nSecond official part.")
    );
    assert!(
        !serde_json::to_string(completed_entry)
            .expect("serialize reasoning completion")
            .contains("hidden chain of thought")
    );
}

#[test]
fn codex_activity_lifecycle_is_exhaustive_and_terminal_monotonic() {
    let mut tracker = CodexActivityFixtureAdapter::new(Some("root-1"));
    let started = json!({
        "threadId": "root-1",
        "turnId": "turn-1",
        "item": {
            "id": "spawn-1",
            "type": "collabAgentToolCall",
            "tool": "spawnAgent",
            "status": "inProgress",
            "senderThreadId": "root-1",
            "receiverThreadIds": ["child-1"],
            "agentsStates": {
                "child-1": { "status": "pendingInit", "message": null }
            }
        },
        "startedAtMs": 1784894400000_u64
    });
    let completed = json!({
        "threadId": "root-1",
        "turnId": "turn-1",
        "item": {
            "id": "wait-1",
            "type": "collabAgentToolCall",
            "tool": "wait",
            "status": "completed",
            "senderThreadId": "root-1",
            "receiverThreadIds": ["child-1"],
            "agentsStates": {
                "child-1": { "status": "completed", "message": "done" }
            }
        },
        "completedAtMs": 1784894401000_u64
    });
    let late_running = json!({
        "threadId": "root-1",
        "turnId": "turn-1",
        "item": {
            "id": "wait-late",
            "type": "collabAgentToolCall",
            "tool": "wait",
            "status": "completed",
            "senderThreadId": "root-1",
            "receiverThreadIds": ["child-1"],
            "agentsStates": {
                "child-1": { "status": "running", "message": null }
            }
        },
        "completedAtMs": 1784894402000_u64
    });

    let starting = tracker.handle_notification("item/started", &started, 1784894400000);
    let terminal = tracker.handle_notification("item/completed", &completed, 1784894401000);
    let late = tracker.handle_notification("item/completed", &late_running, 1784894402000);

    assert!(matches!(
        starting.mutations.as_slice(),
        [bibcode_server::activity::ProviderActivityMutation::UpsertActor(actor)]
            if actor.status == ActivityLifecycle::Starting
    ));
    assert!(matches!(
        terminal.mutations.as_slice(),
        [bibcode_server::activity::ProviderActivityMutation::UpsertActor(actor)]
            if actor.status == ActivityLifecycle::Completed
    ));
    assert!(
        late.mutations.is_empty(),
        "late progress must not reopen a terminal child"
    );

    for (native, expected) in [
        ("pending", ActivityLifecycle::Starting),
        ("starting", ActivityLifecycle::Starting),
        ("pendingInit", ActivityLifecycle::Starting),
        ("inProgress", ActivityLifecycle::Running),
        ("running", ActivityLifecycle::Running),
        ("waiting", ActivityLifecycle::Waiting),
        ("idle", ActivityLifecycle::Waiting),
        ("completed", ActivityLifecycle::Completed),
        ("failed", ActivityLifecycle::Failed),
        ("errored", ActivityLifecycle::Failed),
        ("cancelled", ActivityLifecycle::Cancelled),
        ("interrupted", ActivityLifecycle::Interrupted),
        ("shutdown", ActivityLifecycle::Interrupted),
        ("future", ActivityLifecycle::Unknown),
    ] {
        assert_eq!(
            CodexActivityFixtureAdapter::map_status(native),
            expected,
            "{native}"
        );
    }
}

#[test]
fn codex_activity_reopens_terminal_child_only_for_native_active_or_resume() {
    let mut tracker = CodexActivityFixtureAdapter::new(Some("root-1"));
    let started = json!({
        "threadId": "root-1",
        "turnId": "turn-1",
        "item": {
            "id": "spawn-1",
            "type": "collabAgentToolCall",
            "tool": "spawnAgent",
            "status": "inProgress",
            "senderThreadId": "root-1",
            "receiverThreadIds": ["child-1"],
            "agentsStates": {
                "child-1": { "status": "running", "message": null }
            }
        },
        "startedAtMs": 2_000_u64
    });
    let delayed_terminal = json!({
        "threadId": "root-1",
        "turnId": "turn-1",
        "item": {
            "id": "wait-1",
            "type": "collabAgentToolCall",
            "tool": "wait",
            "status": "completed",
            "senderThreadId": "root-1",
            "receiverThreadIds": ["child-1"],
            "agentsStates": {
                "child-1": { "status": "completed", "message": "done" }
            }
        },
        "completedAtMs": 1_000_u64
    });

    tracker.handle_notification("item/started", &started, 2_000);
    let terminal = tracker.handle_notification("item/completed", &delayed_terminal, 1_000);
    let actor = terminal
        .mutations
        .iter()
        .find_map(|mutation| match mutation {
            bibcode_server::activity::ProviderActivityMutation::UpsertActor(actor) => Some(actor),
            _ => None,
        })
        .expect("delayed terminal update");

    assert_eq!(actor.status, ActivityLifecycle::Completed);
    assert!(actor.started_at <= actor.updated_at);
    assert_eq!(actor.updated_at, actor.started_at);
    assert_eq!(
        actor.terminal_at.as_deref(),
        Some(actor.updated_at.as_str())
    );
    assert!(
        actor
            .terminal_at
            .as_ref()
            .is_some_and(|at| at <= &actor.updated_at)
    );

    let idle = tracker.handle_notification(
        "thread/status/changed",
        &json!({ "threadId": "child-1", "status": {"type": "idle"} }),
        3_000,
    );
    assert!(
        idle.mutations.is_empty(),
        "a later-received idle status must not reopen a terminal child"
    );
    let older_active = tracker.handle_notification(
        "thread/status/changed",
        &json!({
            "threadId": "child-1",
            "status": {"type": "active", "activeFlags": []}
        }),
        1_999,
    );
    assert!(
        older_active.mutations.is_empty(),
        "an older tagged Active status must not reopen a terminal child"
    );
    let equal_active = tracker.handle_notification(
        "thread/status/changed",
        &json!({
            "threadId": "child-1",
            "status": {"type": "active", "activeFlags": []}
        }),
        2_000,
    );
    assert!(matches!(
        equal_active.mutations.as_slice(),
        [bibcode_server::activity::ProviderActivityMutation::UpsertActor(actor)]
            if actor.status == ActivityLifecycle::Running
                && actor.terminal_at.is_none()
    ));

    let terminal_again = tracker.handle_notification(
        "item/completed",
        &json!({
            "threadId": "root-1",
            "turnId": "turn-2",
            "item": {
                "id": "wait-2",
                "type": "collabAgentToolCall",
                "tool": "wait",
                "status": "completed",
                "senderThreadId": "root-1",
                "receiverThreadIds": ["child-1"],
                "agentsStates": {
                    "child-1": {"status": "completed", "message": "done again"}
                }
            },
            "completedAtMs": 3_002_u64
        }),
        3_002,
    );
    assert!(matches!(
        terminal_again.mutations.as_slice(),
        [bibcode_server::activity::ProviderActivityMutation::UpsertActor(actor)]
            if actor.status == ActivityLifecycle::Completed
                && actor.terminal_at.is_some()
    ));

    let older_resume = tracker.handle_notification(
        "item/completed",
        &json!({
            "threadId": "root-1",
            "turnId": "turn-3",
            "item": {
                "id": "resume-older",
                "type": "collabAgentToolCall",
                "tool": "resumeAgent",
                "status": "completed",
                "senderThreadId": "root-1",
                "receiverThreadIds": ["child-1"],
                "agentsStates": {
                    "child-1": {"status": "running", "message": null}
                }
            },
            "completedAtMs": 3_001_u64
        }),
        3_001,
    );
    assert!(
        older_resume.mutations.is_empty(),
        "an older explicit resume must not reopen a terminal child"
    );
    let equal_resume = tracker.handle_notification(
        "item/completed",
        &json!({
            "threadId": "root-1",
            "turnId": "turn-3",
            "item": {
                "id": "resume-equal",
                "type": "collabAgentToolCall",
                "tool": "resumeAgent",
                "status": "completed",
                "senderThreadId": "root-1",
                "receiverThreadIds": ["child-1"],
                "agentsStates": {
                    "child-1": {"status": "running", "message": null}
                }
            },
            "completedAtMs": 3_002_u64
        }),
        3_002,
    );
    assert!(matches!(
        equal_resume.mutations.as_slice(),
        [bibcode_server::activity::ProviderActivityMutation::UpsertActor(actor)]
            if actor.status == ActivityLifecycle::Running
                && actor.terminal_at.is_none()
    ));
}

#[derive(Clone, Copy)]
enum ReopenPath {
    TaggedActive,
    ResumeAgent,
    RecoveryList,
}

fn timestamp_less_terminal_child_tracker() -> CodexActivityFixtureAdapter {
    let mut tracker = CodexActivityFixtureAdapter::new(Some("root-1"));
    let started = tracker.handle_envelope(&json!({
        "method": "item/started",
        "params": {
            "threadId": "root-1",
            "turnId": "turn-1",
            "item": {
                "id": "spawn-1",
                "type": "collabAgentToolCall",
                "tool": "spawnAgent",
                "status": "inProgress",
                "senderThreadId": "root-1",
                "receiverThreadIds": ["child-1"],
                "agentsStates": {
                    "child-1": {"status": "running", "message": null}
                }
            }
        }
    }));
    assert!(matches!(
        started.mutations.as_slice(),
        [bibcode_server::activity::ProviderActivityMutation::UpsertActor(actor)]
            if actor.status == ActivityLifecycle::Running
                && actor.updated_at.starts_with("1970-01-01T00:00:00")
    ));
    let terminal = tracker.handle_envelope(&json!({
        "method": "item/completed",
        "params": {
            "threadId": "root-1",
            "turnId": "turn-1",
            "item": {
                "id": "wait-1",
                "type": "collabAgentToolCall",
                "tool": "wait",
                "status": "completed",
                "senderThreadId": "root-1",
                "receiverThreadIds": ["child-1"],
                "agentsStates": {
                    "child-1": {"status": "completed", "message": "done"}
                }
            }
        }
    }));
    assert!(matches!(
        terminal.mutations.as_slice(),
        [bibcode_server::activity::ProviderActivityMutation::UpsertActor(actor)]
            if actor.status == ActivityLifecycle::Completed
                && actor.updated_at.starts_with("1970-01-01T00:00:00")
    ));
    tracker
}

fn reopen_without_valid_provider_timestamp(
    path: ReopenPath,
    provider_timestamp: Option<u64>,
) -> Vec<ProviderActivityMutation> {
    let mut tracker = timestamp_less_terminal_child_tracker();
    let mut envelope = match path {
        ReopenPath::TaggedActive => json!({
            "method": "thread/status/changed",
            "params": {
                "threadId": "child-1",
                "status": {"type": "active", "activeFlags": []}
            }
        }),
        ReopenPath::ResumeAgent => json!({
            "method": "item/completed",
            "params": {
                "threadId": "root-1",
                "turnId": "turn-2",
                "item": {
                    "id": "resume-1",
                    "type": "collabAgentToolCall",
                    "tool": "resumeAgent",
                    "status": "completed",
                    "senderThreadId": "root-1",
                    "receiverThreadIds": ["child-1"],
                    "agentsStates": {
                        "child-1": {"status": "running", "message": null}
                    }
                }
            }
        }),
        ReopenPath::RecoveryList => json!({
            "id": "recovery-list-timestamp-authority",
            "result": {
                "data": [{
                    "id": "child-1",
                    "parentThreadId": "root-1",
                    "status": {"type": "active", "activeFlags": []}
                }],
                "nextCursor": null,
                "backwardsCursor": null
            }
        }),
    };
    if let Some(provider_timestamp) = provider_timestamp {
        match path {
            ReopenPath::TaggedActive | ReopenPath::ResumeAgent => {
                envelope["emittedAtMs"] = json!(provider_timestamp);
            }
            ReopenPath::RecoveryList => {
                envelope["result"]["data"][0]["updatedAt"] = json!(provider_timestamp);
            }
        }
    }
    tracker.handle_envelope(&envelope).mutations
}

#[test]
fn tagged_active_without_provider_timestamp_cannot_reopen_epoch_terminal_actor() {
    assert!(reopen_without_valid_provider_timestamp(ReopenPath::TaggedActive, None).is_empty());
}

#[test]
fn tagged_active_with_out_of_range_provider_timestamp_cannot_reopen_epoch_terminal_actor() {
    assert!(
        reopen_without_valid_provider_timestamp(ReopenPath::TaggedActive, Some(u64::MAX))
            .is_empty()
    );
}

#[test]
fn resume_without_provider_timestamp_cannot_reopen_epoch_terminal_actor() {
    assert!(reopen_without_valid_provider_timestamp(ReopenPath::ResumeAgent, None).is_empty());
}

#[test]
fn resume_with_out_of_range_provider_timestamp_cannot_reopen_epoch_terminal_actor() {
    assert!(
        reopen_without_valid_provider_timestamp(ReopenPath::ResumeAgent, Some(u64::MAX)).is_empty()
    );
}

#[test]
fn active_recovery_without_provider_timestamp_cannot_reopen_epoch_terminal_actor() {
    assert!(reopen_without_valid_provider_timestamp(ReopenPath::RecoveryList, None).is_empty());
}

#[test]
fn active_recovery_with_out_of_range_provider_timestamp_cannot_reopen_epoch_terminal_actor() {
    assert!(
        reopen_without_valid_provider_timestamp(ReopenPath::RecoveryList, Some(u64::MAX))
            .is_empty()
    );
}

#[test]
fn codex_activity_decodes_legacy_string_status_without_granting_reopen_authority() {
    let mut tracker = CodexActivityFixtureAdapter::new(Some("root-1"));
    tracker.handle_notification(
        "item/started",
        &json!({
            "threadId": "root-1",
            "turnId": "turn-1",
            "item": {
                "id": "spawn-1",
                "type": "collabAgentToolCall",
                "tool": "spawnAgent",
                "status": "inProgress",
                "senderThreadId": "root-1",
                "receiverThreadIds": ["child-1"],
                "agentsStates": {
                    "child-1": {"status": "running", "message": null}
                }
            },
            "startedAtMs": 1_000_u64
        }),
        1_000,
    );

    let unknown_tagged = tracker.handle_notification(
        "thread/status/changed",
        &json!({
            "threadId": "child-1",
            "status": {"type": "futureStatus"}
        }),
        1_001,
    );
    assert!(unknown_tagged.mutations.is_empty());
    let unchanged_running = tracker.handle_notification(
        "thread/status/changed",
        &json!({"threadId": "child-1", "status": "running"}),
        1_002,
    );
    assert!(
        unchanged_running.mutations.is_empty(),
        "an unknown tagged status must not damage the existing running state"
    );

    let legacy_idle = tracker.handle_notification(
        "thread/status/changed",
        &json!({"threadId": "child-1", "status": "idle"}),
        1_003,
    );
    assert!(matches!(
        legacy_idle.mutations.as_slice(),
        [bibcode_server::activity::ProviderActivityMutation::UpsertActor(actor)]
            if actor.status == ActivityLifecycle::Waiting
    ));
    let legacy_terminal = tracker.handle_notification(
        "thread/status/changed",
        &json!({"threadId": "child-1", "status": "completed"}),
        1_004,
    );
    assert!(matches!(
        legacy_terminal.mutations.as_slice(),
        [bibcode_server::activity::ProviderActivityMutation::UpsertActor(actor)]
            if actor.status == ActivityLifecycle::Completed
                && actor.terminal_at.is_some()
    ));
    for (legacy_status, emitted_at_ms) in [("active", 1_005), ("running", 1_006)] {
        let legacy_reopen = tracker.handle_notification(
            "thread/status/changed",
            &json!({"threadId": "child-1", "status": legacy_status}),
            emitted_at_ms,
        );
        assert!(
            legacy_reopen.mutations.is_empty(),
            "legacy string {legacy_status} must never reopen a terminal child"
        );
    }
}

#[test]
fn codex_activity_tracks_nested_relationships_and_bounds_canonical_ids() {
    let mut tracker = CodexActivityFixtureAdapter::new(Some("root-1"));
    tracker.seed_actor("child parent");
    let long_receiver = format!("child {}", "🧪".repeat(400));
    let params = json!({
        "threadId": "child parent",
        "turnId": "nested-turn",
        "item": {
            "id": "spawn-nested",
            "type": "collabAgentToolCall",
            "tool": "spawnAgent",
            "status": "inProgress",
            "senderThreadId": "child parent",
            "receiverThreadIds": [long_receiver],
            "agentsStates": {}
        },
        "startedAtMs": 1784894400000_u64
    });

    let output = tracker.handle_notification("item/started", &params, 1784894400000);
    assert!(
        !output.mutations.is_empty(),
        "nested activity output: {output:?}"
    );
    let actor = output
        .mutations
        .iter()
        .find_map(|mutation| match mutation {
            bibcode_server::activity::ProviderActivityMutation::UpsertActor(actor) => Some(actor),
            _ => None,
        })
        .expect("nested actor mutation");

    let parent_id = actor
        .parent_actor_id
        .as_deref()
        .expect("bounded parent actor id");
    assert!(parent_id.starts_with("codex:thread:h"));
    assert_eq!(parent_id.len(), "codex:thread:h".len() + 64 + 1 + 64);
    assert!(actor.id.starts_with("codex:thread:h"));
    assert_eq!(actor.id.len(), "codex:thread:h".len() + 64 + 1 + 64);
    assert!(actor.id.chars().count() <= 256);
    assert!(!actor.id.chars().any(char::is_whitespace));
}

#[test]
fn codex_activity_event_keys_are_unambiguous_for_adversarial_delimiters() {
    let mut first_tracker = CodexActivityFixtureAdapter::new(Some("root-1"));
    first_tracker.seed_actor("child:a");
    let mut second_tracker = CodexActivityFixtureAdapter::new(Some("root-1"));
    second_tracker.seed_actor("child");
    let first = json!({
        "threadId": "child:a",
        "turnId": "b",
        "item": {
            "id": "c",
            "type": "dynamicToolCall",
            "tool": "inspect",
            "status": "inProgress"
        },
        "startedAtMs": 1784894400000_u64
    });
    let second = json!({
        "threadId": "child",
        "turnId": "a:b",
        "item": {
            "id": "c",
            "type": "dynamicToolCall",
            "tool": "inspect",
            "status": "inProgress"
        },
        "startedAtMs": 1784894400000_u64
    });

    let first_output = first_tracker.handle_notification("item/started", &first, 1784894400000);
    let second_output = second_tracker.handle_notification("item/started", &second, 1784894400000);
    let first_id = first_output
        .mutations
        .iter()
        .find_map(|mutation| match mutation {
            bibcode_server::activity::ProviderActivityMutation::AppendEntry(entry) => {
                Some(entry.id.as_str())
            }
            _ => None,
        })
        .expect("first tool entry");
    let second_id = second_output
        .mutations
        .iter()
        .find_map(|mutation| match mutation {
            bibcode_server::activity::ProviderActivityMutation::AppendEntry(entry) => {
                Some(entry.id.as_str())
            }
            _ => None,
        })
        .expect("second tool entry");

    assert_ne!(first_id, second_id);
}

#[test]
fn codex_activity_coalesces_and_utf8_clips_child_text() {
    let mut tracker = CodexActivityFixtureAdapter::new(Some("root-1"));
    tracker.seed_actor("child-1");
    let first = json!({
        "threadId": "child-1",
        "turnId": "child-turn",
        "itemId": "message-1",
        "delta": "a"
    });
    let second = json!({
        "threadId": "child-1",
        "turnId": "child-turn",
        "itemId": "message-1",
        "delta": "🧪".repeat(20_000)
    });
    let completed = json!({
        "threadId": "child-1",
        "turnId": "child-turn",
        "item": {
            "id": "message-1",
            "type": "agentMessage"
        },
        "completedAtMs": 1060_u64
    });

    let first_output = tracker.handle_notification("item/agentMessage/delta", &first, 1_000);
    let coalesced = tracker.handle_notification("item/agentMessage/delta", &second, 1_050);
    let flushed = tracker.handle_notification("item/completed", &completed, 1_060);

    assert!(first_output.mutations.is_empty());
    assert!(coalesced.mutations.is_empty());
    let detail = flushed
        .mutations
        .iter()
        .find_map(|mutation| match mutation {
            bibcode_server::activity::ProviderActivityMutation::AppendEntry(entry) => {
                entry.detail.as_deref()
            }
            _ => None,
        })
        .expect("completion flush entry");
    assert!(detail.len() <= bibcode_server::activity::ACTIVITY_DETAIL_MAX_LENGTH);
    assert!(std::str::from_utf8(detail.as_bytes()).is_ok());
    assert!(detail.starts_with('a'));
}

#[test]
fn codex_activity_text_deltas_are_replay_safe_across_completion() {
    let mut tracker = CodexActivityFixtureAdapter::new(Some("root-1"));
    tracker.seed_actor("child-1");
    let first = json!({
        "threadId": "child-1",
        "turnId": "child-turn",
        "itemId": "message-1",
        "delta": "same"
    });
    let equal_same_millisecond = first.clone();
    let completed = json!({
        "threadId": "child-1",
        "turnId": "child-turn",
        "item": {
            "id": "message-1",
            "type": "agentMessage",
            "text": "samesame"
        },
        "completedAtMs": 1_050_u64
    });

    let initial =
        tracker.handle_notification_with_sequence("item/agentMessage/delta", &first, 1_000, 10);
    let replay_before =
        tracker.handle_notification_with_sequence("item/agentMessage/delta", &first, 1_000, 10);
    let legitimate_equal = tracker.handle_notification_with_sequence(
        "item/agentMessage/delta",
        &equal_same_millisecond,
        1_000,
        11,
    );
    let completion =
        tracker.handle_notification_with_sequence("item/completed", &completed, 1_050, 12);
    let replay_at_completion =
        tracker.handle_notification_with_sequence("item/completed", &completed, 1_050, 12);
    let replay_after =
        tracker.handle_notification_with_sequence("item/agentMessage/delta", &first, 1_000, 10);

    assert!(initial.mutations.is_empty());
    assert!(replay_before.mutations.is_empty());
    assert!(legitimate_equal.mutations.is_empty());
    assert_eq!(
        completion
            .mutations
            .iter()
            .find_map(|mutation| match mutation {
                bibcode_server::activity::ProviderActivityMutation::AppendEntry(entry) =>
                    entry.detail.as_deref(),
                _ => None,
            }),
        Some("samesame"),
        "equal same-millisecond text with a distinct receive sequence is legitimate"
    );
    assert!(replay_at_completion.mutations.is_empty());
    assert!(replay_after.mutations.is_empty());
    assert_eq!(tracker.state_counts().pending_deltas, 0);
}

#[test]
fn codex_activity_explicit_sequences_advance_the_automatic_counter() {
    let mut tracker = CodexActivityFixtureAdapter::new(Some("root-1"));
    tracker.seed_actor("child-1");
    let first = json!({
        "threadId": "child-1",
        "turnId": "child-turn",
        "itemId": "message-1",
        "delta": "first"
    });
    let second = json!({
        "threadId": "child-1",
        "turnId": "child-turn",
        "itemId": "message-1",
        "delta": "second"
    });

    let explicit =
        tracker.handle_notification_with_sequence("item/agentMessage/delta", &first, 1_000, 0);
    let automatic = tracker.handle_notification("item/agentMessage/delta", &second, 1_100);
    let completion = tracker.handle_notification(
        "item/completed",
        &json!({
            "threadId": "child-1",
            "turnId": "child-turn",
            "item": {
                "id": "message-1",
                "type": "agentMessage",
                "text": "firstsecond"
            }
        }),
        1_200,
    );

    assert!(explicit.mutations.is_empty());
    assert!(automatic.mutations.is_empty());
    assert!(matches!(
        completion.mutations.as_slice(),
        [bibcode_server::activity::ProviderActivityMutation::AppendEntry(entry)]
            if entry.detail.as_deref() == Some("firstsecond")
    ));
}

#[test]
fn codex_activity_automatic_sequences_remain_unique_past_u64_max() {
    let mut tracker = CodexActivityFixtureAdapter::new(Some("root-1"));
    tracker.seed_actor("child-1");
    let delta = |text: &str| {
        json!({
            "threadId": "child-1",
            "turnId": "child-turn",
            "itemId": "message-1",
            "delta": text
        })
    };

    let explicit = tracker.handle_notification_with_sequence(
        "item/agentMessage/delta",
        &delta("explicit"),
        1_000,
        u64::MAX,
    );
    let first_automatic =
        tracker.handle_notification("item/agentMessage/delta", &delta("automatic-1"), 1_100);
    let second_automatic =
        tracker.handle_notification("item/agentMessage/delta", &delta("automatic-2"), 1_200);

    assert!(explicit.mutations.is_empty());
    assert!(first_automatic.mutations.is_empty());
    assert!(second_automatic.mutations.is_empty());
    let completion = tracker.handle_notification(
        "item/completed",
        &json!({
            "threadId": "child-1",
            "turnId": "child-turn",
            "item": {
                "id": "message-1",
                "type": "agentMessage",
                "text": "explicitautomatic-1automatic-2"
            }
        }),
        1_300,
    );
    assert!(matches!(
        completion.mutations.as_slice(),
        [bibcode_server::activity::ProviderActivityMutation::AppendEntry(entry)]
            if entry.detail.as_deref() == Some("explicitautomatic-1automatic-2")
    ));
}

#[test]
fn codex_activity_downgrades_recovery_only_for_a_missing_method() {
    let downgrades = |request_id: &str, error: Value| {
        CodexActivityFixtureAdapter::new(Some("root-1"))
            .handle_envelope(&json!({ "id": request_id, "error": error }))
            .mutations
            .iter()
            .any(|mutation| {
                matches!(
                    mutation,
                    ProviderActivityMutation::SetScope { capabilities, .. }
                        if capabilities.history_recovery
                            == bibcode_server::activity::ActivityHistoryRecovery::None
                )
            })
    };
    assert!(downgrades(
        "recovery-list-1",
        json!({"code": -32601, "message": "Method not found: thread/list"})
    ));
    // Codex's request decoding rejects a method it lacks with -32600.
    assert!(downgrades(
        "recovery-list-1",
        json!({
            "code": -32600,
            "message": "Invalid request: unknown variant `thread/list`, expected one of `initialize`, `thread/start`"
        })
    ));
    assert!(downgrades(
        "recovery-read-1",
        json!({
            "code": -32600,
            "message": "Invalid request: unknown variant `thread/read`, expected one of `initialize`, `thread/start`"
        })
    ));
    // Other invalid requests, including a bad enum parameter, are not a missing method.
    assert!(!downgrades(
        "recovery-list-1",
        json!({"code": -32600, "message": "Invalid request: missing field `ancestorThreadId`"})
    ));
    assert!(!downgrades(
        "recovery-list-1",
        json!({
            "code": -32600,
            "message": "Invalid request: unknown variant `sideways`, expected `asc` or `desc`"
        })
    ));
}

#[test]
fn codex_activity_ignores_unknown_items_and_bounds_internal_state() {
    let mut tracker = CodexActivityFixtureAdapter::new(Some("root-1"));
    let receivers = (0..600)
        .map(|index| format!("child-{index}"))
        .collect::<Vec<_>>();
    let states = receivers
        .iter()
        .map(|receiver| {
            (
                receiver.clone(),
                json!({ "status": "running", "message": null }),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let params = json!({
        "threadId": "root-1",
        "turnId": "turn-1",
        "item": {
            "id": "spawn-many",
            "type": "collabAgentToolCall",
            "tool": "spawnAgent",
            "status": "inProgress",
            "senderThreadId": "root-1",
            "receiverThreadIds": receivers,
            "agentsStates": states
        },
        "startedAtMs": 1784894400000_u64
    });
    let unknown = json!({
        "threadId": "root-1",
        "turnId": "turn-1",
        "item": {
            "id": "future-1",
            "type": "futureActivityItem",
            "rawParams": { "token": "must-not-leak" }
        },
        "startedAtMs": 1784894401000_u64
    });

    let output = tracker.handle_notification("item/started", &params, 1784894400000);
    let ignored = tracker.handle_notification("item/started", &unknown, 1784894401000);
    tracker.seed_actor("bounded-child");
    for index in 0..2_500 {
        let unsupported = json!({
            "threadId": "bounded-child",
            "turnId": "turn-1",
            "item": {
                "id": format!("future-{index}"),
                "type": "futureActivityItem"
            },
            "startedAtMs": 1784894401000_u64 + index
        });
        assert!(
            tracker
                .handle_notification("item/started", &unsupported, 1784894401000_u64 + index,)
                .mutations
                .is_empty()
        );
    }
    let counts = tracker.state_counts();

    assert!(output.mutations.len() <= 256);
    assert!(ignored.mutations.is_empty());
    assert!(counts.actors <= 256);
    assert!(counts.work_items <= 128);
    assert!(counts.seen_events <= 2_048);
    assert!(counts.pending_deltas <= 256);
}

fn validate_codex_activity_fixture(name: &str, fixture: &CodexActivityFixture) {
    assert!(!fixture.scenario.trim().is_empty(), "{name}: scenario");
    assert!(
        !fixture.inbound_messages.is_empty(),
        "{name}: inbound messages"
    );
    assert!(
        fixture.inbound_messages.len() <= 256,
        "{name}: inbound messages must be bounded"
    );
    assert!(
        !fixture.expected_mutations.is_empty(),
        "{name}: expected mutations"
    );
    assert!(
        fixture.expected_mutations.len() <= 256,
        "{name}: expected activity mutations must be bounded"
    );

    let mut ordered_envelopes = HashSet::new();
    for (index, message) in fixture.inbound_messages.iter().enumerate() {
        validate_app_server_envelope(name, index, message);
        validate_fixture_value(name, message, None);
        let encoded = serde_json::to_string(message).expect("fixture message json");
        assert!(
            ordered_envelopes.insert(encoded),
            "{name}: inbound message {index} duplicates an earlier envelope"
        );
    }
    for mutation in &fixture.expected_mutations {
        validate_fixture_value(name, mutation, None);
        validate_expected_mutation_shape(name, mutation);
        let expected: ExpectedActivityMutation = serde_json::from_value(mutation.clone())
            .unwrap_or_else(|error| panic!("{name}: invalid expected activity mutation: {error}"));
        validate_expected_activity_mutation(name, &expected);
    }
    for actor_id in &fixture.initial_actor_ids {
        assert_canonical_id(name, actor_id, "codex:thread:", "initial actor");
    }

    validate_codex_activity_scenario(name, fixture);
}

fn validate_expected_mutation_shape(name: &str, mutation: &Value) {
    let mutation_type = mutation["type"]
        .as_str()
        .unwrap_or_else(|| panic!("{name}: mutation type"));
    let (record_name, required_fields): (&str, &[&str]) = match mutation_type {
        "appendEntry" => (
            "entry",
            &[
                "id",
                "ownerKind",
                "ownerId",
                "kind",
                "title",
                "detail",
                "tone",
                "createdAt",
            ],
        ),
        "setScope" => {
            assert!(
                mutation.get("capabilities").is_some()
                    && mutation.get("observationState").is_some(),
                "{name}: setScope required fields"
            );
            return;
        }
        "upsertActor" => (
            "actor",
            &[
                "_tag",
                "id",
                "parentActorId",
                "name",
                "role",
                "providerType",
                "status",
                "summary",
                "startedAt",
                "updatedAt",
                "terminalAt",
            ],
        ),
        "upsertWorkItem" => (
            "workItem",
            &[
                "_tag",
                "id",
                "ownerActorId",
                "name",
                "workKind",
                "command",
                "cwd",
                "status",
                "summary",
                "startedAt",
                "updatedAt",
                "terminalAt",
            ],
        ),
        _ => panic!("{name}: unsupported fixture mutation {mutation_type}"),
    };
    let record = mutation[record_name]
        .as_object()
        .unwrap_or_else(|| panic!("{name}: {record_name} mutation payload"));
    for field in required_fields {
        assert!(
            record.contains_key(*field),
            "{name}: {record_name} is missing required field {field}"
        );
    }
}

fn validate_expected_activity_mutation(name: &str, mutation: &ExpectedActivityMutation) {
    match mutation {
        ExpectedActivityMutation::AppendEntry { entry } => {
            assert_canonical_id(name, &entry.id, "codex:event:", "entry");
            let owner_prefix = match entry.owner_kind {
                ActivityRecordKind::Actor => "codex:thread:",
                ActivityRecordKind::WorkItem => "codex:item:",
            };
            assert_canonical_id(name, &entry.owner_id, owner_prefix, "entry owner");
            ActivityEntry::try_new(
                entry.id.clone(),
                entry.owner_kind,
                entry.owner_id.clone(),
                entry.kind,
                entry.title.clone(),
                entry.detail.as_deref(),
                entry.tone,
                entry.created_at.clone(),
            )
            .unwrap_or_else(|error| panic!("{name}: invalid expected entry: {error}"));
        }
        ExpectedActivityMutation::SetScope {
            capabilities,
            observation_state,
        } => {
            let _ = (capabilities, observation_state);
        }
        ExpectedActivityMutation::UpsertActor { actor } => {
            assert_canonical_id(name, &actor.id, "codex:thread:", "actor");
            assert_eq!(
                actor.status.is_terminal(),
                actor.terminal_at.is_some(),
                "{name}: actor lifecycle and terminalAt must agree"
            );
            if let Some(parent_actor_id) = &actor.parent_actor_id {
                assert_canonical_id(name, parent_actor_id, "codex:thread:", "parent actor");
            }
            ActivityActorSummary::try_new(
                actor.id.clone(),
                actor.parent_actor_id.as_deref(),
                actor.name.clone(),
                actor.role.as_deref(),
                actor.provider_type.as_deref(),
                actor.status,
                actor.summary.as_deref(),
                actor.started_at.clone(),
                actor.updated_at.clone(),
                actor.terminal_at.as_deref(),
            )
            .unwrap_or_else(|error| panic!("{name}: invalid expected actor: {error}"));
        }
        ExpectedActivityMutation::UpsertWorkItem { work_item } => {
            assert_canonical_id(name, &work_item.id, "codex:item:", "work item");
            assert_eq!(
                work_item.status.is_terminal(),
                work_item.terminal_at.is_some(),
                "{name}: work item lifecycle and terminalAt must agree"
            );
            if let Some(owner_actor_id) = &work_item.owner_actor_id {
                assert_canonical_id(name, owner_actor_id, "codex:thread:", "work item owner");
            }
            ActivityWorkItemSummary::try_new(
                work_item.id.clone(),
                work_item.owner_actor_id.as_deref(),
                work_item.name.clone(),
                work_item.work_kind.clone(),
                work_item.command.as_deref(),
                work_item.cwd.as_deref(),
                work_item.status,
                work_item.summary.as_deref(),
                work_item.started_at.clone(),
                work_item.updated_at.clone(),
                work_item.terminal_at.as_deref(),
            )
            .unwrap_or_else(|error| panic!("{name}: invalid expected work item: {error}"));
        }
    }
}

fn assert_canonical_id(name: &str, value: &str, prefix: &str, kind: &str) {
    assert!(
        value.starts_with(prefix)
            && value.len() > prefix.len()
            && value.chars().count() <= 256
            && !value.chars().any(char::is_whitespace),
        "{name}: {kind} id {value:?} must use the {prefix} namespace"
    );
}

fn validate_app_server_envelope(name: &str, index: usize, message: &Value) {
    let envelope = message
        .as_object()
        .unwrap_or_else(|| panic!("{name}: inbound message {index} must be an object"));
    assert!(
        !envelope.contains_key("jsonrpc"),
        "{name}: inbound message {index} must match Codex 0.145.0 envelopes without jsonrpc"
    );
    let method = envelope.get("method");
    let id = envelope.get("id");
    let result = envelope.get("result");
    let error = envelope.get("error");
    match (method, id) {
        (Some(method), None) => {
            assert!(
                method.as_str().is_some_and(|method| !method.is_empty()),
                "{name}: notification method"
            );
            assert!(
                envelope.get("params").is_some(),
                "{name}: notification params"
            );
            assert!(
                envelope
                    .get("emittedAtMs")
                    .and_then(Value::as_u64)
                    .is_some(),
                "{name}: notification emittedAtMs"
            );
            assert!(
                result.is_none() && error.is_none(),
                "{name}: notification cannot contain a response"
            );
            validate_known_notification(
                name,
                method.as_str().expect("validated notification method"),
                envelope
                    .get("params")
                    .expect("validated notification params"),
            );
        }
        (None, Some(id)) => {
            assert!(
                id.is_string() || id.is_number(),
                "{name}: response id must be a string or number"
            );
            assert_ne!(
                result.is_some(),
                error.is_some(),
                "{name}: response must contain exactly one of result or error"
            );
            assert!(
                envelope.get("params").is_none() && envelope.get("emittedAtMs").is_none(),
                "{name}: response cannot contain notification fields"
            );
            validate_known_response(name, envelope);
        }
        _ => panic!("{name}: inbound message {index} is not a notification or response"),
    }
}

fn decode_fixture_payload<T: DeserializeOwned>(name: &str, kind: &str, value: &Value) {
    serde_json::from_value::<T>(value.clone())
        .unwrap_or_else(|error| panic!("{name}: invalid Codex 0.145.0 {kind}: {error}"));
}

fn validate_known_notification(name: &str, method: &str, params: &Value) {
    match method {
        "item/started" => {
            decode_fixture_payload::<ItemStartedNotificationDto>(name, method, params);
        }
        "item/completed" => {
            decode_fixture_payload::<ItemCompletedNotificationDto>(name, method, params);
        }
        "item/agentMessage/delta" => {
            decode_fixture_payload::<AgentMessageDeltaNotificationDto>(name, method, params);
        }
        "turn/completed" => {
            decode_fixture_payload::<TurnCompletedNotificationDto>(name, method, params);
        }
        _ => panic!("{name}: unvalidated notification method {method}"),
    }
}

fn validate_known_response(name: &str, envelope: &serde_json::Map<String, Value>) {
    let id = envelope["id"]
        .as_str()
        .unwrap_or_else(|| panic!("{name}: fixture response ids must be strings"));
    if let Some(error) = envelope.get("error") {
        assert!(
            id.starts_with("recovery-list-") || id.starts_with("recovery-read-"),
            "{name}: unexpected error response id {id}"
        );
        decode_fixture_payload::<AppServerErrorDto>(name, "error response", error);
        return;
    }

    let result = envelope.get("result").expect("validated result response");
    if id.starts_with("recovery-list-") {
        decode_fixture_payload::<ThreadListResponseDto>(name, "thread/list response", result);
    } else if id.starts_with("recovery-read-") {
        decode_fixture_payload::<ThreadReadResponseDto>(name, "thread/read response", result);
    } else {
        panic!("{name}: unvalidated success response id {id}");
    }
}

fn validate_fixture_value(name: &str, value: &Value, field: Option<&str>) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                validate_fixture_value(name, child, Some(key));
            }
        }
        Value::Array(array) => {
            for child in array {
                validate_fixture_value(name, child, field);
            }
        }
        Value::String(text) => {
            assert!(
                !text.starts_with('/') && !text.contains("\\Users\\") && !text.contains(":\\"),
                "{name}: fixture contains an unredacted absolute path"
            );
            let length = text.chars().count();
            match field {
                Some(
                    "id" | "itemId" | "ownerId" | "parentActorId" | "ownerActorId"
                    | "senderThreadId" | "threadId" | "turnId",
                ) => assert!(length <= 256, "{name}: activity identifier bound"),
                Some("name" | "title" | "role" | "providerType") => {
                    assert!(length <= 256, "{name}: activity label bound");
                }
                Some("summary") => {
                    assert!(length <= 2_048, "{name}: activity summary bound");
                }
                Some("detail") => {
                    assert!(length <= 16_384, "{name}: activity detail bound");
                }
                _ => assert!(length <= 16_384, "{name}: fixture string bound"),
            }
            if matches!(
                field,
                Some(
                    "aggregatedOutput"
                        | "command"
                        | "cwd"
                        | "delta"
                        | "detail"
                        | "path"
                        | "preview"
                        | "prompt"
                        | "summary"
                )
            ) || matches!(field, Some("agent_path"))
            {
                assert!(
                    text.starts_with("[redacted "),
                    "{name}: sensitive {field:?} field must be redacted"
                );
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn validate_codex_activity_scenario(name: &str, fixture: &CodexActivityFixture) {
    match name {
        "trace-collaboration.json" => {
            let states = fixture
                .inbound_messages
                .iter()
                .filter_map(|message| message.pointer("/params/item/agentsStates/child-1/status"))
                .filter_map(Value::as_str)
                .collect::<Vec<_>>();
            assert_eq!(states, ["pendingInit", "running", "completed"]);
            assert_eq!(
                fixture.inbound_messages[0]
                    .pointer("/params/item/receiverThreadIds")
                    .and_then(Value::as_array)
                    .map(Vec::len),
                Some(2)
            );
            assert!(
                fixture.inbound_messages.iter().any(|message| {
                    message.pointer("/params/item/type")
                        == Some(&Value::String("futureActivityItem".to_owned()))
                }),
                "the unknown item compatibility case must remain represented"
            );
            assert_eq!(
                fixture
                    .expected_mutations
                    .iter()
                    .filter(|mutation| mutation["type"] == "upsertActor")
                    .count(),
                6,
                "the known subAgentActivity item adds one provisional actor while the unknown item adds none"
            );
            let decoded_items = fixture
                .inbound_messages
                .iter()
                .map(|message| {
                    serde_json::from_value::<ItemStartedOrCompletedDto>(message["params"].clone())
                        .unwrap_or_else(|error| panic!("{name}: typed collaboration item: {error}"))
                        .item
                })
                .collect::<Vec<_>>();
            assert!(
                decoded_items[..3]
                    .iter()
                    .all(|item| matches!(item, ThreadItemDto::CollabAgentToolCall { .. }))
            );
            assert!(
                matches!(decoded_items[3], ThreadItemDto::SubAgentActivity { .. }),
                "the Codex 0.145 sub-agent lifecycle item must remain a known DTO"
            );
            assert!(
                matches!(decoded_items[4], ThreadItemDto::Unknown),
                "the explicit unknown item must decode only through the compatibility variant"
            );
            let spawn_item = fixture.inbound_messages[0]
                .pointer("/params/item")
                .expect("spawn item");
            let sender_thread_id = spawn_item["senderThreadId"].as_str().expect("spawn sender");
            let receiver_thread_ids = spawn_item["receiverThreadIds"]
                .as_array()
                .expect("spawn receivers")
                .iter()
                .map(|receiver| receiver.as_str().expect("receiver id"))
                .collect::<HashSet<_>>();
            for mutation in fixture
                .expected_mutations
                .iter()
                .filter(|mutation| mutation["type"] == "upsertActor")
            {
                let native_actor_id = mutation["actor"]["id"]
                    .as_str()
                    .expect("actor id")
                    .strip_prefix("codex:thread:")
                    .expect("validated canonical actor id");
                if receiver_thread_ids.contains(native_actor_id) {
                    let expected_parent = (sender_thread_id != "root-1")
                        .then(|| format!("codex:thread:{sender_thread_id}"));
                    assert_eq!(
                        mutation["actor"]["parentActorId"].as_str(),
                        expected_parent.as_deref(),
                        "root spawns have no parent actor; non-root spawns use the canonical sender"
                    );
                }
            }
        }
        "trace-child-activity.json" => {
            let methods = fixture
                .inbound_messages
                .iter()
                .filter_map(|message| message["method"].as_str())
                .collect::<Vec<_>>();
            assert_eq!(
                methods,
                [
                    "item/agentMessage/delta",
                    "item/completed",
                    "item/started",
                    "item/completed",
                    "item/completed",
                ]
            );
            assert!(
                fixture.inbound_messages.iter().any(|message| {
                    message.pointer("/params/item/type")
                        == Some(&Value::String("commandExecution".to_owned()))
                        && message.pointer("/params/item/status")
                            == Some(&Value::String("completed".to_owned()))
                }),
                "child command completion must remain represented"
            );
            let item_kinds = fixture
                .inbound_messages
                .iter()
                .skip(1)
                .map(|message| {
                    serde_json::from_value::<ItemStartedOrCompletedDto>(message["params"].clone())
                        .unwrap_or_else(|error| panic!("{name}: typed child item: {error}"))
                        .item
                })
                .collect::<Vec<_>>();
            assert!(matches!(item_kinds[0], ThreadItemDto::Unknown));
            assert!(matches!(
                item_kinds[1],
                ThreadItemDto::DynamicToolCall { .. }
            ));
            assert!(matches!(
                item_kinds[2],
                ThreadItemDto::DynamicToolCall { .. }
            ));
            assert!(matches!(
                item_kinds[3],
                ThreadItemDto::CommandExecution { .. }
            ));
            assert_eq!(
                fixture
                    .expected_mutations
                    .iter()
                    .map(|mutation| mutation["type"].as_str().expect("mutation type"))
                    .collect::<Vec<_>>(),
                ["appendEntry", "appendEntry", "appendEntry", "appendEntry"]
            );
            assert_eq!(
                fixture
                    .expected_mutations
                    .iter()
                    .filter_map(|mutation| {
                        mutation.pointer("/entry/kind").and_then(Value::as_str)
                    })
                    .collect::<Vec<_>>(),
                ["commentary", "tool", "tool", "command"]
            );
            assert!(fixture.expected_mutations.iter().all(|mutation| {
                mutation.pointer("/entry/ownerKind").and_then(Value::as_str) == Some("actor")
                    && mutation.pointer("/entry/ownerId").and_then(Value::as_str)
                        == Some("codex:thread:child-1")
            }));
            assert_eq!(
                fixture
                    .expected_mutations
                    .iter()
                    .filter_map(|mutation| mutation.pointer("/entry/id").and_then(Value::as_str))
                    .collect::<Vec<_>>(),
                [
                    "codex:event:commentary:4c9fe1e9943bf015237f3c34cf5d683b1de6a1cea00d44ddde25aff29e0c900e-c2200580d9ac8e8f636d634c17fdcdc4dfa057c83ad628c9beed86bd7b3d8d40",
                    "codex:event:item-started:child-1:child-turn-1:child-tool-1:running",
                    "codex:event:item-completed:child-1:child-turn-1:child-tool-1:completed",
                    "codex:event:item-completed:child-1:child-turn-1:child-command-1:completed",
                ]
            );
        }
        "trace-reconcile.json" => {
            assert_eq!(fixture.initial_actor_ids, ["codex:thread:child-live"]);
            assert_eq!(
                fixture
                    .expected_mutations
                    .iter()
                    .filter_map(|mutation| mutation.pointer("/actor/id"))
                    .collect::<Vec<_>>(),
                [&Value::String("codex:thread:child-missed".to_owned())],
                "reconciliation must add the missed child without duplicating the live child"
            );
            assert_eq!(
                fixture.expected_mutations[0]
                    .pointer("/actor/status")
                    .and_then(Value::as_str),
                Some("waiting")
            );
            assert_eq!(
                fixture.expected_mutations[0].pointer("/actor/terminalAt"),
                Some(&Value::Null)
            );
            assert_eq!(
                fixture.expected_mutations[0].pointer("/actor/parentActorId"),
                Some(&Value::Null)
            );
        }
        "trace-schema-downgrade.json" => {
            assert_eq!(
                fixture.inbound_messages[0].pointer("/error/code"),
                Some(&json!(-32601))
            );
            assert_eq!(
                fixture.inbound_messages[1].pointer("/params/turn/status"),
                Some(&Value::String("completed".to_owned())),
                "unsupported recovery must not fail the parent turn"
            );
            assert_eq!(
                fixture.expected_mutations[0].pointer("/capabilities/historyRecovery"),
                Some(&Value::String("none".to_owned()))
            );
        }
        _ => panic!("unexpected activity fixture {name}"),
    }
}

fn task_2_codex_activity_projection(fixture: &CodexActivityFixture) -> Vec<Value> {
    let root_thread_id = fixture.inbound_messages.iter().find_map(|message| {
        message
            .pointer("/result/data/0/parentThreadId")
            .and_then(Value::as_str)
    });
    let mut tracker = CodexActivityFixtureAdapter::new(root_thread_id);
    for actor_id in &fixture.initial_actor_ids {
        let native_actor_id = actor_id.trim_start_matches("codex:thread:");
        let listed_actor = fixture.inbound_messages.iter().find_map(|message| {
            message
                .pointer("/result/data")
                .and_then(Value::as_array)
                .and_then(|threads| {
                    threads
                        .iter()
                        .find(|thread| thread["id"] == native_actor_id)
                })
                .cloned()
        });
        if let Some(listed_actor) = listed_actor {
            let _ = tracker.handle_envelope(&json!({
                "id": "recovery-list-bootstrap",
                "result": {
                    "data": [listed_actor],
                    "nextCursor": null,
                    "backwardsCursor": null
                }
            }));
        } else {
            tracker.seed_actor(native_actor_id);
        }
    }
    fixture
        .inbound_messages
        .iter()
        .flat_map(|message| tracker.handle_envelope(message).mutations)
        .map(provider_activity_mutation_to_value)
        .collect()
}

fn provider_activity_mutation_to_value(
    mutation: bibcode_server::activity::ProviderActivityMutation,
) -> Value {
    let mut value = match mutation {
        bibcode_server::activity::ProviderActivityMutation::SetScope {
            capabilities,
            observation_state,
        } => json!({
            "type": "setScope",
            "capabilities": capabilities,
            "observationState": observation_state,
        }),
        bibcode_server::activity::ProviderActivityMutation::SetSectionHealth {
            section,
            health,
        } => {
            json!({
                "type": "setSectionHealth",
                "section": section,
                "health": health,
            })
        }
        bibcode_server::activity::ProviderActivityMutation::UpsertActor(actor) => {
            json!({ "type": "upsertActor", "actor": actor })
        }
        bibcode_server::activity::ProviderActivityMutation::RemoveActor { actor_id } => {
            json!({ "type": "removeActor", "actorId": actor_id })
        }
        bibcode_server::activity::ProviderActivityMutation::UpsertWorkItem(work_item) => {
            json!({ "type": "upsertWorkItem", "workItem": work_item })
        }
        bibcode_server::activity::ProviderActivityMutation::RemoveWorkItem { work_item_id } => {
            json!({ "type": "removeWorkItem", "workItemId": work_item_id })
        }
        bibcode_server::activity::ProviderActivityMutation::AppendEntry(entry) => {
            json!({ "type": "appendEntry", "entry": entry })
        }
    };
    normalize_fixture_timestamps(&mut value);
    value
}

fn normalize_fixture_timestamps(value: &mut Value) {
    match value {
        Value::Array(values) => {
            for value in values {
                normalize_fixture_timestamps(value);
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                if matches!(
                    key.as_str(),
                    "createdAt" | "startedAt" | "updatedAt" | "terminalAt"
                ) && let Some(timestamp) = value.as_str()
                    && let Some(seconds) = timestamp.strip_suffix(".000000000Z")
                {
                    *value = Value::String(format!("{seconds}Z"));
                    continue;
                }
                normalize_fixture_timestamps(value);
            }
        }
        _ => {}
    }
}

use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, BufReader, duplex},
    sync::{mpsc, oneshot},
    time::timeout,
};

#[tokio::test]
async fn helper_outputs_match_canonical_codex_fixtures() {
    let initialize_fixture = fixture("initialize-params.json");
    assert_eq!(build_initialize_params("0.1.1"), initialize_fixture);

    let default_turn_fixture = fixture("turn-start-default.json");
    assert_eq!(
        build_turn_start_params(&BuildTurnStartInput {
            thread_id: "provider-thread-1".to_owned(),
            runtime_mode: CodexRuntimeMode::AutoAcceptEdits,
            client_user_message_id: None,
            prompt: Some("Implement it".to_owned()),
            attachments: vec![json!({
                "type": "image",
                "url": "data:image/png;base64,abc",
            })],
            model: Some("gpt-5.3-codex".to_owned()),
            service_tier: None,
            effort: None,
            interaction_mode: Some("default".to_owned()),
        }),
        default_turn_fixture
    );

    let plan_turn_fixture = fixture("turn-start-plan.json");
    assert_eq!(
        build_turn_start_params(&BuildTurnStartInput {
            thread_id: "provider-thread-1".to_owned(),
            runtime_mode: CodexRuntimeMode::FullAccess,
            client_user_message_id: None,
            prompt: Some("Make a plan".to_owned()),
            attachments: vec![],
            model: Some("gpt-5.3-codex".to_owned()),
            service_tier: None,
            effort: Some("medium".to_owned()),
            interaction_mode: Some("plan".to_owned()),
        }),
        plan_turn_fixture
    );

    let model_fixture = fixture("model-discovery.json");
    let parsed_models = parse_model_list_response(
        &model_fixture["response"],
        &["custom-alpha".to_owned(), "gpt-5.3-codex".to_owned()],
    )
    .expect("models parse");
    assert_eq!(
        serde_json::to_value(parsed_models).expect("models json"),
        model_fixture["parsed"]
    );

    let skills_fixture = fixture("skill-discovery.json");
    let parsed_skills = parse_skills_list_response(
        &skills_fixture["response"],
        skills_fixture["cwd"].as_str().expect("fixture cwd"),
    )
    .expect("skills parse");
    assert_eq!(
        serde_json::to_value(parsed_skills).expect("skills json"),
        skills_fixture["parsed"]
    );
}

#[tokio::test]
async fn probe_matches_fixture_corpus() {
    let scenario = fixture("probe-scenario.json");
    let (connection, _incoming, mut peer) = scripted_peer();
    peer.expect_request("initialize", scenario["initializeRequest"].clone())
        .respond(scenario["initializeResponse"].clone());
    peer.expect_notification("initialized");
    peer.expect_request("account/read", json!({}))
        .respond(scenario["accountResponse"].clone());
    peer.expect_request("model/list", json!({}))
        .respond(scenario["modelListFirst"].clone());
    peer.expect_request("model/list", json!({ "cursor": "cursor-2" }))
        .respond(scenario["modelListSecond"].clone());
    peer.expect_request(
        "skills/list",
        json!({ "cwds": [scenario["cwd"].as_str().expect("cwd")] }),
    )
    .respond(scenario["skillsResponse"].clone());

    let peer_task = tokio::spawn(peer.run());
    let snapshot = probe_provider(
        &connection,
        "0.1.1",
        scenario["cwd"].as_str().expect("cwd"),
        &["custom-alpha".to_owned()],
    )
    .await
    .expect("probe succeeds");
    let expected = scenario["expectedSnapshot"].clone();
    assert_eq!(
        serde_json::to_value(snapshot).expect("snapshot json"),
        expected
    );
    peer_task.await.expect("peer task");
}

#[tokio::test]
async fn session_runtime_matches_text_tool_and_approval_traces() {
    let (connection, incoming, mut peer) = scripted_peer();
    let runtime = CodexSessionRuntime::new(
        CodexSessionOptions {
            version: "0.1.1".to_owned(),
            thread_id: "fixture-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: CodexRuntimeMode::FullAccess,
            model: Some("gpt-5.3-codex".to_owned()),
            service_tier: None,
            effort: None,
            resume_cursor: None,
        },
        connection.clone(),
        incoming,
    );

    peer.expect_request("initialize", fixture("initialize-params.json"))
        .respond(json!({
            "userAgent": "mock-codex-app-server",
            "codexHome": "/tmp/codex-home",
            "platformFamily": "unix",
            "platformOs": "linux",
        }));
    peer.expect_notification("initialized");
    peer.expect_request(
        "thread/start",
        json!({
            "cwd": "/tmp/project",
            "approvalPolicy": "never",
            "sandbox": "danger-full-access",
            "model": "gpt-5.3-codex",
            "serviceTier": null,
        }),
    )
    .respond(json!({
        "cwd": "/tmp/project",
        "model": "gpt-5.3-codex",
        "thread": { "id": "provider-thread-1" }
    }));
    peer.expect_request(
        "thread/goal/set",
        json!({
            "threadId": "provider-thread-1",
            "objective": "Finish the provider parity work",
            "status": "active",
        }),
    )
    .respond(json!({ "goal": { "status": "active" } }));
    peer.expect_request("turn/start", fixture("turn-start-text.json"))
        .respond(json!({
            "turn": { "id": "fixture-turn" }
        }))
        .emit_notification(json!({
            "method": "turn/started",
            "params": {
                "threadId": "provider-thread-1",
                "turn": { "id": "fixture-turn" }
            }
        }))
        .emit_notification(json!({
            "method": "item/agentMessage/delta",
            "params": {
                "threadId": "provider-thread-1",
                "turnId": "fixture-turn",
                "itemId": "commentary-1",
                "delta": "First."
            }
        }))
        .emit_notification(json!({
            "method": "item/completed",
            "params": {
                "threadId": "provider-thread-1",
                "turnId": "fixture-turn",
                "item": { "id": "commentary-1", "type": "agentMessage" }
            }
        }))
        .emit_notification(json!({
            "method": "item/agentMessage/delta",
            "params": {
                "threadId": "provider-thread-1",
                "turnId": "fixture-turn",
                "itemId": "final-1",
                "delta": "Second."
            }
        }))
        .emit_notification(json!({
            "method": "turn/completed",
            "params": {
                "threadId": "provider-thread-1",
                "turn": { "id": "fixture-turn", "status": "completed" }
            }
        }));
    peer.expect_request("turn/start", fixture("turn-start-tool.json"))
        .respond(json!({
            "turn": { "id": "fixture-turn" }
        }))
        .emit_notification(json!({
            "method": "turn/started",
            "params": {
                "threadId": "provider-thread-1",
                "turn": { "id": "fixture-turn" }
            }
        }))
        .emit_notification(json!({
            "method": "item/started",
            "params": {
                "threadId": "provider-thread-1",
                "turnId": "fixture-turn",
                "item": {
                    "type": "commandExecution",
                    "id": "cmd-1",
                    "command": "echo integration"
                }
            }
        }))
        .emit_notification(json!({
            "method": "item/completed",
            "params": {
                "threadId": "provider-thread-1",
                "turnId": "fixture-turn",
                "item": {
                    "type": "commandExecution",
                    "id": "cmd-1",
                    "command": "echo integration"
                }
            }
        }))
        .emit_notification(json!({
            "method": "item/agentMessage/delta",
            "params": {
                "threadId": "provider-thread-1",
                "turnId": "fixture-turn",
                "itemId": "item-2",
                "delta": "Applied the requested edit.\n"
            }
        }))
        .emit_notification(json!({
            "method": "turn/completed",
            "params": {
                "threadId": "provider-thread-1",
                "turn": { "id": "fixture-turn", "status": "completed" }
            }
        }));
    peer.expect_request("turn/start", fixture("turn-start-approval.json"))
        .respond(json!({
            "turn": { "id": "fixture-turn" }
        }))
        .emit_notification(json!({
            "method": "turn/started",
            "params": {
                "threadId": "provider-thread-1",
                "turn": { "id": "fixture-turn" }
            }
        }))
        .emit_request(json!({
            "id": 1001,
            "method": "item/commandExecution/requestApproval",
            "params": {
                "threadId": "provider-thread-1",
                "turnId": "fixture-turn",
                "itemId": "item-approval-1",
                "reason": "Please approve command"
            }
        }))
        .expect_response(json!({
            "id": 1001,
            "result": { "decision": "accept" }
        }))
        .emit_notification(json!({
            "method": "item/agentMessage/delta",
            "params": {
                "threadId": "provider-thread-1",
                "turnId": "fixture-turn",
                "itemId": "item-3",
                "delta": "Approval received and command executed.\n"
            }
        }))
        .emit_notification(json!({
            "method": "turn/completed",
            "params": {
                "threadId": "provider-thread-1",
                "turn": { "id": "fixture-turn", "status": "completed" }
            }
        }));

    let peer_task = tokio::spawn(peer.run());

    runtime.start().await.expect("runtime starts");
    let startup_events = runtime.collect_events(3).await;
    assert_eq!(startup_events[0].event_type, "session.connecting");
    assert_eq!(startup_events[1].event_type, "mcp.status.updated");
    assert_eq!(startup_events[2].event_type, "session.ready");
    runtime.set_agent_activity_enabled(false).await;

    runtime
        .set_goal("Finish the provider parity work")
        .await
        .expect("goal is set through app-server");

    runtime
        .send_turn(Some("Small text turn".to_owned()), vec![], None, None)
        .await
        .expect("text turn");
    let text_events = runtime.collect_events(5).await;
    assert_eq!(text_events, stable_fixture("trace-text.json"));
    assert_eq!(
        text_events
            .iter()
            .map(|event| (event.event_type.as_str(), event.item_id.as_deref()))
            .collect::<Vec<_>>(),
        vec![
            ("turn.started", None),
            ("content.delta", Some("commentary-1")),
            ("message.assistant.completed", Some("commentary-1")),
            ("content.delta", Some("final-1")),
            ("turn.completed", None),
        ]
    );

    runtime
        .send_turn(Some("Run a tool".to_owned()), vec![], None, None)
        .await
        .expect("tool turn");
    let tool_events = runtime.collect_events(5).await;
    assert_eq!(tool_events, stable_fixture("trace-tool.json"));

    runtime
        .send_turn(Some("Needs approval".to_owned()), vec![], None, None)
        .await
        .expect("approval turn");
    let mut approval_events = runtime.collect_events(2).await;
    assert_eq!(
        &approval_events[..2],
        &stable_fixture("trace-approval-prefix.json")[..2]
    );
    runtime
        .respond_to_request("approval:1001", "accept")
        .await
        .expect("approval response");
    approval_events.extend(runtime.collect_events(3).await);
    assert_eq!(approval_events, stable_fixture("trace-approval.json"));

    peer_task.await.expect("peer task");
}

#[tokio::test]
async fn root_token_usage_notifications_are_normalized_and_child_usage_is_ignored() {
    let (connection, incoming, mut peer) = scripted_peer();
    let runtime = CodexSessionRuntime::new(
        CodexSessionOptions {
            version: "0.1.1".to_owned(),
            thread_id: "fixture-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: CodexRuntimeMode::FullAccess,
            model: Some("gpt-5.3-codex".to_owned()),
            service_tier: None,
            effort: None,
            resume_cursor: None,
        },
        connection,
        incoming,
    );

    peer.expect_request("initialize", fixture("initialize-params.json"))
        .respond(json!({}));
    peer.expect_notification("initialized");
    peer.expect_request(
        "thread/start",
        json!({
            "cwd": "/tmp/project",
            "approvalPolicy": "never",
            "sandbox": "danger-full-access",
            "model": "gpt-5.3-codex",
            "serviceTier": null,
        }),
    )
    .respond(json!({
        "cwd": "/tmp/project",
        "model": "gpt-5.3-codex",
        "thread": { "id": "provider-thread-1" }
    }));
    peer.expect_request("turn/start", fixture("turn-start-text.json"))
        .respond(json!({ "turn": { "id": "fixture-turn" } }))
        .emit_notification(json!({
            "method": "turn/started",
            "params": {
                "threadId": "provider-thread-1",
                "turn": { "id": "fixture-turn" }
            }
        }))
        .emit_notification(json!({
            "method": "thread/tokenUsage/updated",
            "params": {
                "threadId": "provider-thread-1",
                "turnId": "fixture-turn",
                "tokenUsage": {
                    "last": { "totalTokens": 1_075 },
                    "total": { "totalTokens": 10_200 },
                    "modelContextWindow": 258_400
                }
            }
        }))
        .emit_notification(json!({
            "method": "thread/tokenUsage/updated",
            "params": {
                "threadId": "child-1",
                "turnId": "child-turn",
                "tokenUsage": {
                    "last": { "totalTokens": 999_999 },
                    "total": { "totalTokens": 999_999 },
                    "modelContextWindow": 1_000_000
                }
            }
        }))
        .emit_notification(json!({
            "method": "turn/completed",
            "params": {
                "threadId": "provider-thread-1",
                "turn": { "id": "fixture-turn", "status": "completed" }
            }
        }));

    let peer_task = tokio::spawn(peer.run());
    runtime.start().await.expect("runtime starts");
    runtime.collect_events(3).await;
    runtime
        .send_turn(Some("Small text turn".to_owned()), vec![], None, None)
        .await
        .expect("turn starts");
    let events = timeout(Duration::from_secs(2), runtime.collect_events(3))
        .await
        .expect("root usage and completion events");

    let usage_events = events
        .iter()
        .enumerate()
        .filter(|(_, event)| event.event_type == "thread.token-usage.updated")
        .collect::<Vec<_>>();
    assert_eq!(usage_events.len(), 1);
    let (usage_index, usage_event) = usage_events[0];
    assert_eq!(usage_event.turn_id.as_deref(), Some("fixture-turn"));
    assert_eq!(
        usage_event.payload,
        json!({
            "usage": {
                "usedTokens": 1_075,
                "totalProcessedTokens": 10_200,
                "maxTokens": 258_400,
                "lastUsedTokens": 1_075,
                "compactsAutomatically": true
            }
        })
    );
    let completion_index = events
        .iter()
        .position(|event| event.event_type == "turn.completed")
        .expect("turn completion event");
    assert!(usage_index < completion_index);
    assert!(
        !events
            .iter()
            .any(|event| event.payload.to_string().contains("999999")),
        "child usage must not reach canonical payloads: {events:?}"
    );

    peer_task.await.expect("peer task");
}

#[tokio::test]
async fn activity_runtime_routes_live_children_without_changing_root_events() {
    let (connection, incoming, mut peer) = scripted_peer();
    let runtime = CodexSessionRuntime::new(
        CodexSessionOptions {
            version: "0.1.1".to_owned(),
            thread_id: "fixture-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: CodexRuntimeMode::FullAccess,
            model: Some("gpt-5.3-codex".to_owned()),
            service_tier: None,
            effort: None,
            resume_cursor: None,
        },
        connection,
        incoming,
    );

    peer.expect_request("initialize", fixture("initialize-params.json"))
        .respond(json!({ "userAgent": "mock-codex-app-server" }));
    peer.expect_notification("initialized");
    peer.expect_request(
        "thread/start",
        json!({
            "cwd": "/tmp/project",
            "approvalPolicy": "never",
            "sandbox": "danger-full-access",
            "model": "gpt-5.3-codex",
            "serviceTier": null,
        }),
    )
    .respond(json!({
        "cwd": "/tmp/project",
        "model": "gpt-5.3-codex",
        "thread": { "id": "provider-thread-1" }
    }));
    peer.expect_request("turn/start", fixture("turn-start-text.json"))
        .respond(json!({ "turn": { "id": "fixture-turn" } }))
        .emit_notification(json!({
            "method": "turn/started",
            "emittedAtMs": 1_000,
            "params": {
                "threadId": "provider-thread-1",
                "turn": { "id": "fixture-turn" }
            }
        }))
        .emit_notification(json!({
            "method": "item/started",
            "emittedAtMs": 1_001,
            "params": {
                "threadId": "provider-thread-1",
                "turnId": "fixture-turn",
                "item": {
                    "id": "spawn-1",
                    "type": "collabAgentToolCall",
                    "tool": "spawnAgent",
                    "status": "inProgress",
                    "senderThreadId": "provider-thread-1",
                    "receiverThreadIds": ["child-1"],
                    "agentsStates": {
                        "child-1": { "status": "running", "message": null }
                    }
                },
                "startedAtMs": 1_001
            }
        }))
        .emit_notification(json!({
            "method": "item/agentMessage/delta",
            "emittedAtMs": 1_002,
            "params": {
                "threadId": "child-1",
                "turnId": "child-turn-1",
                "itemId": "child-message-1",
                "delta": "child-only"
            }
        }))
        .emit_notification(json!({
            "method": "item/completed",
            "emittedAtMs": 1_002,
            "params": {
                "threadId": "child-1",
                "turnId": "child-turn-1",
                "item": {
                    "id": "child-message-1",
                    "type": "agentMessage",
                    "text": "child-only"
                },
                "completedAtMs": 1_002
            }
        }))
        .emit_notification(json!({
            "method": "item/agentMessage/delta",
            "emittedAtMs": 1_003,
            "params": {
                "threadId": "foreign-thread",
                "turnId": "foreign-turn",
                "itemId": "foreign-message",
                "delta": "must-be-ignored"
            }
        }))
        .emit_notification(json!({
            "method": "item/agentMessage/delta",
            "emittedAtMs": 1_004,
            "params": {
                "threadId": "provider-thread-1",
                "turnId": "fixture-turn",
                "itemId": "root-message-1",
                "delta": "root-only"
            }
        }))
        .emit_notification(json!({
            "method": "item/started",
            "emittedAtMs": 1_005,
            "params": {
                "threadId": "provider-thread-1",
                "turnId": "fixture-turn",
                "item": {
                    "type": "commandExecution",
                    "id": "root-command-1",
                    "command": "echo root"
                }
            }
        }))
        .emit_notification(json!({
            "method": "item/completed",
            "emittedAtMs": 1_006,
            "params": {
                "threadId": "provider-thread-1",
                "turnId": "fixture-turn",
                "item": {
                    "type": "commandExecution",
                    "id": "root-command-1",
                    "command": "echo root"
                }
            }
        }));

    let peer_task = tokio::spawn(peer.run());
    runtime.start().await.expect("runtime starts");
    runtime.collect_events(3).await;
    runtime
        .send_turn(Some("Small text turn".to_owned()), vec![], None, None)
        .await
        .expect("turn starts");
    let events = timeout(Duration::from_secs(2), async {
        let mut events = Vec::new();
        while events.len() < 6 {
            events.push(runtime.next_event().await.expect("live runtime event"));
        }
        events
    })
    .await
    .expect("live events");

    let activity = events
        .iter()
        .filter(|event| !event.activity.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(activity.len(), 2);
    assert_eq!(
        activity
            .iter()
            .map(|event| event.native_event_id.as_deref())
            .collect::<Vec<_>>(),
        [Some("codex:activity:1"), Some("codex:activity:3")]
    );
    assert!(matches!(
        activity[0].activity.as_slice(),
        [ProviderActivityMutation::UpsertActor(actor)]
            if actor.id == "codex:thread:child-1"
    ));
    assert!(
        matches!(
            activity[1].activity.as_slice(),
            [ProviderActivityMutation::AppendEntry(entry)]
                if entry.owner_id == "codex:thread:child-1"
                    && entry.detail.as_deref() == Some("child-only")
                    && entry.created_at == "1970-01-01T00:00:01.002000000Z"
        ),
        "child activity: {:?}",
        activity[1].activity
    );

    let root_events = events
        .iter()
        .filter(|event| event.activity.is_empty())
        .map(RuntimeEvent::stable_view)
        .collect::<Vec<_>>();
    assert_eq!(
        root_events,
        vec![
            RuntimeEventStableView {
                event_type: "turn.started".to_owned(),
                thread_id: "fixture-thread".to_owned(),
                turn_id: Some("fixture-turn".to_owned()),
                item_id: None,
                request_id: None,
                payload: json!({}),
            },
            RuntimeEventStableView {
                event_type: "content.delta".to_owned(),
                thread_id: "fixture-thread".to_owned(),
                turn_id: Some("fixture-turn".to_owned()),
                item_id: Some("root-message-1".to_owned()),
                request_id: None,
                payload: json!({
                    "streamKind": "assistant_text",
                    "delta": "root-only",
                }),
            },
            RuntimeEventStableView {
                event_type: "item.started".to_owned(),
                thread_id: "fixture-thread".to_owned(),
                turn_id: Some("fixture-turn".to_owned()),
                item_id: None,
                request_id: None,
                payload: json!({
                    "itemType": "command_execution",
                    "title": "Ran command",
                    "detail": "echo root",
                }),
            },
            RuntimeEventStableView {
                event_type: "item.completed".to_owned(),
                thread_id: "fixture-thread".to_owned(),
                turn_id: Some("fixture-turn".to_owned()),
                item_id: None,
                request_id: None,
                payload: json!({
                    "itemType": "command_execution",
                    "status": "completed",
                    "title": "Ran command",
                    "detail": "echo root",
                }),
            },
        ]
    );
    peer_task.await.expect("peer task");
}

#[tokio::test]
async fn disabled_agent_activity_pauses_tracking_and_resumes_with_authoritative_reconciliation() {
    let (connection, _protocol_incoming, mut peer) = scripted_peer();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(
        CodexSessionOptions {
            version: "0.1.1".to_owned(),
            thread_id: "fixture-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: CodexRuntimeMode::FullAccess,
            model: Some("gpt-5.3-codex".to_owned()),
            service_tier: None,
            effort: None,
            resume_cursor: None,
        },
        connection,
        incoming_rx,
    );
    peer.expect_request("initialize", fixture("initialize-params.json"))
        .respond(json!({}));
    peer.expect_notification("initialized");
    peer.expect_request(
        "thread/start",
        json!({
            "cwd": "/tmp/project",
            "approvalPolicy": "never",
            "sandbox": "danger-full-access",
            "model": "gpt-5.3-codex",
            "serviceTier": null,
        }),
    )
    .respond(json!({
        "thread": {"id": "provider-root"},
        "cwd": "/tmp/project",
        "model": "gpt-5.3-codex"
    }));
    let (inflight_seen_tx, inflight_seen_rx) = tokio::sync::oneshot::channel();
    let (inflight_release_tx, inflight_release_rx) = tokio::sync::oneshot::channel();
    peer.expect_request(
        "thread/read",
        json!({"threadId": "provider-root", "includeTurns": false}),
    )
    .pause_response(inflight_seen_tx, inflight_release_rx)
    .respond_history(empty_root_thread_read_result());
    peer.expect_request(
        "thread/read",
        json!({"threadId": "provider-root", "includeTurns": false}),
    )
    .respond_history(empty_root_thread_read_result());
    peer.expect_request(
        "thread/list",
        json!({"ancestorThreadId": "provider-root", "limit": 50}),
    )
    .respond(json!({
        "data": [{
            "id": "child-disabled-history",
            "parentThreadId": "provider-root",
            "agentNickname": "Disabled interval child",
            "agentRole": "worker",
            "createdAt": 1,
            "updatedAt": 3,
            "status": {"type": "idle"}
        }],
        "nextCursor": null,
        "backwardsCursor": null
    }));
    peer.expect_request(
        "thread/read",
        json!({"threadId": "child-disabled-history", "includeTurns": false}),
    )
    .respond_history(json!({
        "thread": {
            "id": "child-disabled-history",
            "parentThreadId": "provider-root",
            "agentNickname": "Disabled interval child",
            "agentRole": "worker",
            "createdAt": 1,
            "updatedAt": 3,
            "status": {"type": "idle"},
            "turns": [
                {
                    "id": "turn-disabled-history",
                    "status": "completed",
                    "startedAt": 2,
                    "completedAt": 3,
                    "items": [{
                        "type": "agentMessage",
                        "id": "message-disabled-history",
                        "text": "disabled-history-backfill"
                    }]
                },
                {
                    "id": "turn-straddling",
                    "status": "inProgress",
                    "startedAt": 2,
                    "items": [{
                        "type": "commandExecution",
                        "id": "command-straddling",
                        "status": "inProgress",
                        "command": "echo disabled",
                        "aggregatedOutput": "disabled-straddling-command"
                    }]
                }
            ]
        }
    }));
    peer.expect_request(
        "thread/backgroundTerminals/list",
        json!({"threadId": "provider-root", "limit": 128}),
    )
    .respond(json!({"data": [], "nextCursor": null}));
    peer.expect_request(
        "thread/read",
        json!({"threadId": "provider-root", "includeTurns": false}),
    )
    .respond_history(empty_root_thread_read_result());
    peer.expect_request(
        "thread/list",
        json!({"ancestorThreadId": "provider-root", "limit": 50}),
    )
    .respond(json!({
        "data": [{
            "id": "child-disabled-history",
            "parentThreadId": "provider-root",
            "agentNickname": "Disabled interval child",
            "agentRole": "worker",
            "createdAt": 1,
            "updatedAt": 5,
            "status": {"type": "idle"}
        }],
        "nextCursor": null,
        "backwardsCursor": null
    }));
    peer.expect_request(
        "thread/read",
        json!({"threadId": "child-disabled-history", "includeTurns": false}),
    )
    .respond_history(json!({
        "thread": {
            "id": "child-disabled-history",
            "parentThreadId": "provider-root",
            "agentNickname": "Disabled interval child",
            "agentRole": "worker",
            "createdAt": 1,
            "updatedAt": 5,
            "status": {"type": "idle"},
            "turns": [
                {
                    "id": "turn-disabled-history",
                    "status": "completed",
                    "startedAt": 2,
                    "completedAt": 3,
                    "items": [{
                        "type": "agentMessage",
                        "id": "message-disabled-history",
                        "text": "disabled-history-backfill"
                    }]
                },
                {
                    "id": "turn-straddling",
                    "status": "completed",
                    "startedAt": 2,
                    "completedAt": 4_000_000_000_u64,
                    "items": [{
                        "type": "commandExecution",
                        "id": "command-straddling",
                        "status": "completed",
                        "command": "echo disabled",
                        "aggregatedOutput": "disabled-straddling-command"
                    }]
                },
                {
                    "id": "turn-after-baseline",
                    "status": "completed",
                    "startedAt": 4_000_000_001_u64,
                    "completedAt": 4_000_000_002_u64,
                    "items": [{
                        "type": "agentMessage",
                        "id": "message-after-baseline",
                        "text": "after-baseline-detail"
                    }]
                }
            ]
        }
    }));
    peer.expect_request(
        "thread/backgroundTerminals/list",
        json!({"threadId": "provider-root", "limit": 128}),
    )
    .respond(json!({"data": [], "nextCursor": null}));
    peer.expect_request("shutdown", Value::Null)
        .respond(Value::Null);
    let peer_task = tokio::spawn(peer.run());

    runtime.start().await.expect("runtime starts");
    runtime.collect_events(3).await;
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "thread/started".to_owned(),
            params: json!({"thread": {"id": "provider-root"}}),
            emitted_at_ms: 900,
        })
        .expect("root notification starts reconciliation");
    timeout(Duration::from_secs(2), inflight_seen_rx)
        .await
        .expect("in-flight reconciliation request")
        .expect("request observation");
    runtime.set_agent_activity_enabled(false).await;
    let _ = inflight_release_tx.send(());
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "item/started".to_owned(),
            params: json!({
                "threadId": "provider-root",
                "turnId": "turn-disabled",
                "item": {
                    "id": "spawn-disabled",
                    "type": "collabAgentToolCall",
                    "tool": "spawnAgent",
                    "status": "inProgress",
                    "senderThreadId": "provider-root",
                    "receiverThreadIds": ["child-disabled"],
                    "agentsStates": {
                        "child-disabled": {"status": "running", "message": null}
                    }
                },
                "startedAtMs": 1_000
            }),
            emitted_at_ms: 1_000,
        })
        .expect("disabled notification");
    assert!(
        timeout(Duration::from_millis(350), runtime.next_event())
            .await
            .is_err(),
        "disabled activity emits nothing and does not enqueue reconciliation"
    );

    runtime.set_agent_activity_enabled(true).await;
    let reconciliation = timeout(Duration::from_secs(2), runtime.next_event())
        .await
        .expect("resumed reconciliation timeout")
        .expect("resumed reconciliation event");
    assert!(!reconciliation.activity.is_empty());
    assert!(reconciliation.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::UpsertActor(actor)
            if actor.id == "codex:thread:child-disabled-history"
    )));
    assert!(
        reconciliation.activity.iter().all(|mutation| !matches!(
            mutation,
            ProviderActivityMutation::AppendEntry(entry)
                if entry.detail.as_deref() == Some("disabled-history-backfill")
        )),
        "re-enable preserves current topology without replaying disabled-period history"
    );
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "item/started".to_owned(),
            params: json!({
                "threadId": "provider-root",
                "turnId": "turn-reconcile",
                "item": {
                    "id": "reconcile-after-baseline",
                    "type": "subAgentActivity",
                    "agentThreadId": "child-disabled-history",
                    "agentPath": "/root/child-disabled-history",
                    "kind": "interacted"
                }
            }),
            emitted_at_ms: 2_000,
        })
        .expect("post-baseline reconciliation hint");
    let after_baseline = next_codex_event_matching(&runtime, |event| {
        event.activity.iter().any(|mutation| {
            matches!(
                mutation,
                ProviderActivityMutation::AppendEntry(entry)
                    if entry.detail.as_deref() == Some("after-baseline-detail")
            )
        })
    })
    .await;
    assert!(after_baseline.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::UpsertActor(actor)
            if actor.id == "codex:thread:child-disabled-history"
                && actor.status == ActivityLifecycle::Completed
    )));
    assert!(after_baseline.activity.iter().all(|mutation| !matches!(
        mutation,
        ProviderActivityMutation::AppendEntry(entry)
            if entry.title == "Command completed"
                || entry.detail.as_deref() == Some("disabled-history-backfill")
    )));

    runtime.shutdown().await.expect("runtime shuts down");
    peer_task.await.expect("peer");
}

#[tokio::test]
async fn reconciliation_first_root_notification_requests_exact_descendant_scope() {
    let (connection, _protocol_incoming, peer) = scripted_peer();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(
        CodexSessionOptions {
            version: "0.1.1".to_owned(),
            thread_id: "fixture-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: CodexRuntimeMode::FullAccess,
            model: Some("gpt-5.3-codex".to_owned()),
            service_tier: None,
            effort: None,
            resume_cursor: None,
        },
        connection,
        incoming_rx,
    );
    let (request_tx, request_rx) = oneshot::channel();
    let peer_task = tokio::spawn(async move {
        let mut reader = BufReader::new(peer.stdin);
        let mut writer = peer.stdout;

        let initialize = read_scripted_message(&mut reader, &mut writer).await;
        assert_eq!(initialize["method"], "initialize");
        write_json(
            &mut writer,
            json!({"id": initialize["id"].clone(), "result": {}}),
        )
        .await;
        assert_eq!(
            read_scripted_message(&mut reader, &mut writer).await["method"],
            "initialized"
        );
        let start = read_scripted_message(&mut reader, &mut writer).await;
        assert_eq!(start["method"], "thread/start");
        write_json(
            &mut writer,
            json!({
                "id": start["id"].clone(),
                "result": {
                    "thread": {"id": "provider-root"},
                    "cwd": "/tmp/project",
                    "model": "gpt-5.3-codex"
                }
            }),
        )
        .await;

        let reconciliation = read_scripted_message(&mut reader, &mut writer).await;
        let _ = request_tx.send(reconciliation);

        let shutdown = read_scripted_message(&mut reader, &mut writer).await;
        assert_eq!(shutdown["method"], "shutdown");
        write_json(
            &mut writer,
            json!({"id": shutdown["id"].clone(), "result": null}),
        )
        .await;
    });

    runtime.start().await.expect("runtime starts");
    runtime.collect_events(3).await;
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "thread/started".to_owned(),
            params: json!({"thread": {"id": "provider-root"}}),
            emitted_at_ms: 1_000,
        })
        .expect("root notification is delivered");

    let request = timeout(Duration::from_millis(500), request_rx)
        .await
        .expect("first root must schedule immediate reconciliation")
        .expect("reconciliation request");
    assert_eq!(request["method"], "thread/read");
    assert_eq!(
        request["params"],
        json!({"threadId": "provider-root", "includeTurns": false})
    );

    runtime.shutdown().await.expect("runtime shuts down");
    peer_task.await.expect("peer task");
    assert!(
        timeout(Duration::from_millis(100), runtime.next_event())
            .await
            .is_err(),
        "cancelling the in-flight reconciliation must not emit an activity error"
    );
}

#[tokio::test]
async fn sub_agent_activity_debounces_follow_up_reconciliation_and_repairs_nested_parentage() {
    let (connection, _protocol_incoming, peer) = scripted_peer();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(
        CodexSessionOptions {
            version: "0.1.1".to_owned(),
            thread_id: "fixture-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: CodexRuntimeMode::FullAccess,
            model: Some("gpt-5.3-codex".to_owned()),
            service_tier: None,
            effort: None,
            resume_cursor: None,
        },
        connection,
        incoming_rx,
    );
    let requests = Arc::new(StdMutex::new(Vec::<Value>::new()));
    let peer_requests = requests.clone();
    let peer_task = tokio::spawn(async move {
        let mut reader = BufReader::new(peer.stdin);
        let mut writer = peer.stdout;
        let mut history: Option<Value> = None;
        loop {
            let message = read_scripted_message(&mut reader, &mut writer).await;
            let Some(method) = message.get("method").and_then(Value::as_str) else {
                continue;
            };
            if message.get("id").is_none() {
                continue;
            }
            peer_requests
                .lock()
                .expect("request log mutex")
                .push(message.clone());
            let result = match method {
                "initialize" => json!({}),
                "thread/start" => json!({
                    "thread": {"id": "provider-root"},
                    "cwd": "/tmp/project",
                    "model": "gpt-5.3-codex"
                }),
                "thread/list" => json!({
                    "data": [],
                    "nextCursor": null,
                    "backwardsCursor": null
                }),
                "thread/read" => {
                    let full = match message["params"]["threadId"].as_str() {
                        Some("provider-root") => json!({
                            "thread": {
                                "id": "provider-root",
                                "createdAt": 1,
                                "updatedAt": 4,
                                "status": {"type": "idle"},
                                "turns": [{
                                    "id": "root-turn",
                                    "status": "completed",
                                    "startedAt": 1,
                                    "completedAt": 4,
                                    "items": [{
                                        "id": "spawn-direct",
                                        "type": "subAgentActivity",
                                        "agentThreadId": "child-direct",
                                        "agentPath": "/root/direct",
                                        "kind": "started"
                                    }]
                                }]
                            }
                        }),
                        Some("child-direct") => json!({
                            "thread": {
                                "id": "child-direct",
                                "parentThreadId": "provider-root",
                                "agentNickname": "Direct child",
                                "agentRole": "worker",
                                "createdAt": 2,
                                "updatedAt": 4,
                                "status": {"type": "notLoaded"},
                                "turns": [{
                                    "id": "direct-turn",
                                    "status": "completed",
                                    "startedAt": 2,
                                    "completedAt": 4,
                                    "items": [
                                        {
                                            "id": "spawn-nested",
                                            "type": "subAgentActivity",
                                            "agentThreadId": "child-nested",
                                            "agentPath": "/root/direct/nested",
                                            "kind": "started"
                                        },
                                        {
                                            "id": "direct-result",
                                            "type": "agentMessage",
                                            "text": "Direct result"
                                        }
                                    ]
                                }]
                            }
                        }),
                        Some("child-nested") => json!({
                            "thread": {
                                "id": "child-nested",
                                "parentThreadId": "child-direct",
                                "agentNickname": "Nested child",
                                "agentRole": "worker",
                                "createdAt": 3,
                                "updatedAt": 5,
                                "status": {"type": "notLoaded"},
                                "turns": [{
                                    "id": "nested-turn",
                                    "status": "completed",
                                    "startedAt": 3,
                                    "completedAt": 5,
                                    "items": [{
                                        "id": "nested-result",
                                        "type": "agentMessage",
                                        "text": "Nested result"
                                    }]
                                }]
                            }
                        }),
                        other => panic!("unexpected thread/read target {other:?}"),
                    };
                    let metadata = thread_history_metadata(&full);
                    history = Some(full);
                    metadata
                }
                "thread/turns/list" => thread_turns_page(
                    &message["params"],
                    history
                        .as_ref()
                        .expect("the history read precedes its pages"),
                ),
                "thread/backgroundTerminals/list" => {
                    json!({"data": [], "nextCursor": null})
                }
                "shutdown" => Value::Null,
                other => panic!("unexpected request in debounce test: {other}"),
            };
            write_json(
                &mut writer,
                json!({"id": message["id"].clone(), "result": result}),
            )
            .await;
            if method == "shutdown" {
                break;
            }
        }
    });

    runtime.start().await.expect("runtime starts");
    runtime.collect_events(2).await;

    for (method, kind, emitted_at_ms) in [
        ("item/started", "started", 1_001),
        ("item/completed", "interacted", 1_002),
    ] {
        incoming_tx
            .send(IncomingEvent::Notification {
                method: method.to_owned(),
                params: json!({
                    "threadId": "provider-root",
                    "turnId": "root-turn",
                    "item": {
                        "id": format!("sub-agent-{kind}"),
                        "type": "subAgentActivity",
                        "agentThreadId": "child-direct",
                        "agentPath": "/root/direct",
                        "kind": kind
                    }
                }),
                emitted_at_ms,
            })
            .expect("sub-agent activity notification");
    }

    let reconciliation = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:0")
    })
    .await;
    assert!(reconciliation.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::UpsertActor(actor)
            if actor.id == "codex:thread:child-direct"
                && actor.parent_actor_id.is_none()
                && actor.status == ActivityLifecycle::Completed
    )));
    assert!(reconciliation.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::UpsertActor(actor)
            if actor.id == "codex:thread:child-nested"
                && actor.parent_actor_id.as_deref() == Some("codex:thread:child-direct")
                && actor.status == ActivityLifecycle::Completed
    )));
    assert!(reconciliation.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::AppendEntry(entry)
            if entry.owner_id == "codex:thread:child-direct"
                && entry.detail.as_deref() == Some("Direct result")
    )));
    assert!(reconciliation.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::AppendEntry(entry)
            if entry.owner_id == "codex:thread:child-nested"
                && entry.detail.as_deref() == Some("Nested result")
    )));

    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        requests
            .lock()
            .expect("request log mutex")
            .iter()
            .filter(|request| request["method"] == "thread/list")
            .count(),
        1,
        "only one coalesced sub-agent reconciliation pass is expected"
    );

    runtime.shutdown().await.expect("runtime shuts down");
    peer_task.await.expect("peer task");
}

#[tokio::test]
async fn reconciliation_restart_recovers_child_from_root_history_when_lists_are_empty() {
    let (connection, _protocol_incoming, mut peer) = scripted_peer();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(
        codex_activity_options(Some("provider-root")),
        connection,
        incoming_rx,
    );
    peer.expect_request("initialize", fixture("initialize-params.json"))
        .respond(json!({}));
    peer.expect_notification("initialized");
    peer.expect_request(
        "thread/resume",
        json!({
            "threadId": "provider-root",
            "cwd": "/tmp/project",
            "approvalPolicy": "never",
            "sandbox": "danger-full-access",
            "model": "gpt-5.3-codex",
            "serviceTier": null,
        }),
    )
    .respond(json!({
        "thread": {"id": "provider-root"},
        "cwd": "/tmp/project",
        "model": "gpt-5.3-codex"
    }));
    peer.expect_request(
        "thread/read",
        json!({"threadId": "provider-root", "includeTurns": false}),
    )
    .respond_history(json!({
        "thread": {
            "id": "provider-root",
            "createdAt": 1,
            "updatedAt": 3,
            "status": {"type": "idle"},
            "turns": [{
                "id": "root-turn",
                "status": "completed",
                "startedAt": 1,
                "completedAt": 3,
                "items": [{
                    "id": "spawn-recovered",
                    "type": "subAgentActivity",
                    "agentThreadId": "recovered-child",
                    "agentPath": "/root/recovered",
                    "kind": "started"
                }]
            }]
        }
    }));
    peer.expect_request(
        "thread/list",
        json!({"ancestorThreadId": "provider-root", "limit": 50}),
    )
    .respond(empty_thread_list_result());
    peer.expect_request(
        "thread/read",
        json!({"threadId": "recovered-child", "includeTurns": false}),
    )
    .respond_history(json!({
        "thread": {
            "id": "recovered-child",
            "parentThreadId": "provider-root",
            "agentNickname": "Recovered child",
            "agentRole": "worker",
            "createdAt": 2,
            "updatedAt": 3,
            "status": {"type": "notLoaded"},
            "turns": [{
                "id": "recovered-turn",
                "status": "completed",
                "startedAt": 2,
                "completedAt": 3,
                "items": [{
                    "id": "recovered-result",
                    "type": "agentMessage",
                    "text": "Recovered result"
                }]
            }]
        }
    }));
    peer.expect_request(
        "thread/backgroundTerminals/list",
        json!({"threadId": "provider-root", "limit": 128}),
    )
    .respond(empty_background_terminal_list_result());
    peer.expect_request("shutdown", Value::Null)
        .respond(Value::Null);
    let peer_task = tokio::spawn(peer.run());

    runtime.start().await.expect("restarted runtime starts");
    runtime.collect_events(2).await;
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "thread/started".to_owned(),
            params: json!({"thread": {"id": "provider-root"}}),
            emitted_at_ms: 1_000,
        })
        .expect("restarted root notification");

    let reconciliation = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:0")
    })
    .await;
    assert!(reconciliation.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::UpsertActor(actor)
            if actor.id == "codex:thread:recovered-child"
                && actor.status == ActivityLifecycle::Completed
    )));
    assert!(reconciliation.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::AppendEntry(entry)
            if entry.owner_id == "codex:thread:recovered-child"
                && entry.detail.as_deref() == Some("Recovered result")
    )));

    runtime.shutdown().await.expect("runtime shuts down");
    peer_task.await.expect("peer task");
}

#[tokio::test]
async fn reconciliation_same_root_reconnect_refreshes_verified_child_with_empty_lists() {
    let (connection_a, _protocol_incoming_a, mut peer_a) = scripted_peer();
    let (incoming_tx_a, incoming_rx_a) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(
        codex_activity_options(Some("provider-root")),
        connection_a,
        incoming_rx_a,
    );
    peer_a
        .expect_request("initialize", fixture("initialize-params.json"))
        .respond(json!({}));
    peer_a.expect_notification("initialized");
    peer_a
        .expect_request(
            "thread/resume",
            json!({
                "threadId": "provider-root",
                "cwd": "/tmp/project",
                "approvalPolicy": "never",
                "sandbox": "danger-full-access",
                "model": "gpt-5.3-codex",
                "serviceTier": null,
            }),
        )
        .respond(json!({
            "thread": {"id": "provider-root"},
            "cwd": "/tmp/project",
            "model": "gpt-5.3-codex"
        }));
    peer_a
        .expect_request(
            "thread/read",
            json!({"threadId": "provider-root", "includeTurns": false}),
        )
        .respond_history(json!({
            "thread": {
                "id": "provider-root",
                "createdAt": 1,
                "updatedAt": 2,
                "status": {"type": "idle"},
                "turns": [{
                    "id": "root-turn",
                    "status": "completed",
                    "startedAt": 1,
                    "completedAt": 2,
                    "items": [{
                        "id": "spawn-refresh-child",
                        "type": "subAgentActivity",
                        "agentThreadId": "refresh-child",
                        "agentPath": "/root/refresh-child",
                        "kind": "started"
                    }]
                }]
            }
        }));
    peer_a
        .expect_request(
            "thread/list",
            json!({"ancestorThreadId": "provider-root", "limit": 50}),
        )
        .respond(empty_thread_list_result());
    peer_a
        .expect_request(
            "thread/read",
            json!({"threadId": "refresh-child", "includeTurns": false}),
        )
        .respond_history(json!({
            "thread": {
                "id": "refresh-child",
                "parentThreadId": "provider-root",
                "createdAt": 2,
                "updatedAt": 2,
                "status": {"type": "notLoaded"},
                "turns": [{
                    "id": "refresh-turn-initial",
                    "status": "completed",
                    "startedAt": 2,
                    "completedAt": 2,
                    "items": [{
                        "id": "refresh-result-initial",
                        "type": "agentMessage",
                        "text": "Initial refresh result"
                    }]
                }]
            }
        }));
    peer_a
        .expect_request(
            "thread/backgroundTerminals/list",
            json!({"threadId": "provider-root", "limit": 128}),
        )
        .respond(empty_background_terminal_list_result());
    let peer_a_task = tokio::spawn(peer_a.run());

    runtime.start().await.expect("runtime starts");
    runtime.collect_events(2).await;
    incoming_tx_a
        .send(IncomingEvent::Notification {
            method: "thread/started".to_owned(),
            params: json!({"thread": {"id": "provider-root"}}),
            emitted_at_ms: 1_000,
        })
        .expect("root notification");
    let initial = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:0")
    })
    .await;
    assert!(initial.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::AppendEntry(entry)
            if entry.detail.as_deref() == Some("Initial refresh result")
    )));
    peer_a_task.await.expect("initial peer task");

    let (connection_b, _protocol_incoming_b, mut peer_b) = scripted_peer();
    let (_incoming_tx_b, incoming_rx_b) = mpsc::unbounded_channel();
    peer_b
        .expect_request("initialize", fixture("initialize-params.json"))
        .respond(json!({}));
    peer_b.expect_notification("initialized");
    peer_b
        .expect_request(
            "thread/resume",
            json!({
                "threadId": "provider-root",
                "cwd": "/tmp/project",
                "approvalPolicy": "never",
                "sandbox": "danger-full-access",
                "model": "gpt-5.3-codex",
                "serviceTier": null,
            }),
        )
        .respond(json!({
            "thread": {"id": "provider-root"},
            "cwd": "/tmp/project",
            "model": "gpt-5.3-codex"
        }));
    peer_b
        .expect_request(
            "thread/read",
            json!({"threadId": "provider-root", "includeTurns": false}),
        )
        .respond_history(json!({
            "thread": {
                "id": "provider-root",
                "createdAt": 1,
                "updatedAt": 4,
                "status": {"type": "idle"},
                "turns": [{
                    "id": "root-turn",
                    "status": "completed",
                    "startedAt": 1,
                    "completedAt": 4,
                    "items": [{
                        "id": "spawn-refresh-child",
                        "type": "subAgentActivity",
                        "agentThreadId": "refresh-child",
                        "agentPath": "/root/refresh-child",
                        "kind": "interacted"
                    }]
                }]
            }
        }));
    peer_b
        .expect_request(
            "thread/list",
            json!({"ancestorThreadId": "provider-root", "limit": 50}),
        )
        .respond(empty_thread_list_result());
    peer_b
        .expect_request(
            "thread/read",
            json!({"threadId": "refresh-child", "includeTurns": false}),
        )
        .respond_history(json!({
            "thread": {
                "id": "refresh-child",
                "parentThreadId": "provider-root",
                "createdAt": 2,
                "updatedAt": 4,
                "status": {"type": "notLoaded"},
                "turns": [{
                    "id": "refresh-turn-reconnected",
                    "status": "completed",
                    "startedAt": 3,
                    "completedAt": 4,
                    "items": [{
                        "id": "refresh-result-reconnected",
                        "type": "agentMessage",
                        "text": "Reconnected refresh result"
                    }]
                }]
            }
        }));
    peer_b
        .expect_request(
            "thread/backgroundTerminals/list",
            json!({"threadId": "provider-root", "limit": 128}),
        )
        .respond(empty_background_terminal_list_result());
    peer_b
        .expect_request("shutdown", Value::Null)
        .respond(Value::Null);
    let peer_b_task = tokio::spawn(peer_b.run());

    runtime
        .reconnect(connection_b, incoming_rx_b)
        .await
        .expect("same-root runtime reconnects");
    runtime.collect_events(2).await;
    let refreshed = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:1")
    })
    .await;
    assert!(refreshed.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::UpsertActor(actor)
            if actor.id == "codex:thread:refresh-child"
                && actor.status == ActivityLifecycle::Completed
    )));
    assert!(refreshed.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::AppendEntry(entry)
            if entry.detail.as_deref() == Some("Reconnected refresh result")
    )));

    runtime.shutdown().await.expect("runtime shuts down");
    peer_b_task.await.expect("reconnected peer task");
}

#[tokio::test]
async fn reconciliation_deduplicates_root_and_live_hints_within_one_pass() {
    let (connection, _protocol_incoming, peer) = scripted_peer();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(codex_activity_options(None), connection, incoming_rx);
    let child_read_count = Arc::new(StdMutex::new(0_usize));
    let peer_child_read_count = child_read_count.clone();
    let (root_read_seen_tx, root_read_seen_rx) = oneshot::channel();
    let (release_root_read_tx, release_root_read_rx) = oneshot::channel();
    let peer_task = tokio::spawn(async move {
        let mut reader = BufReader::new(peer.stdin);
        let mut writer = peer.stdout;
        let mut root_read_seen_tx = Some(root_read_seen_tx);
        let mut release_root_read_rx = Some(release_root_read_rx);
        let mut history: Option<Value> = None;
        loop {
            let message = read_scripted_message(&mut reader, &mut writer).await;
            let Some(method) = message.get("method").and_then(Value::as_str) else {
                continue;
            };
            if message.get("id").is_none() {
                assert_eq!(method, "initialized");
                continue;
            }
            let result = match method {
                "initialize" => {
                    assert_eq!(message["params"], fixture("initialize-params.json"));
                    json!({})
                }
                "thread/start" => {
                    assert_eq!(
                        message["params"],
                        json!({
                            "cwd": "/tmp/project",
                            "approvalPolicy": "never",
                            "sandbox": "danger-full-access",
                            "model": "gpt-5.3-codex",
                            "serviceTier": null,
                        })
                    );
                    json!({
                        "thread": {"id": "provider-root"},
                        "cwd": "/tmp/project",
                        "model": "gpt-5.3-codex"
                    })
                }
                "thread/read" => {
                    let full = match message["params"]["threadId"].as_str() {
                        Some("provider-root") => {
                            assert_eq!(
                                message["params"],
                                json!({"threadId": "provider-root", "includeTurns": false})
                            );
                            if let Some(root_read_seen_tx) = root_read_seen_tx.take() {
                                let _ = root_read_seen_tx.send(());
                                release_root_read_rx
                                    .take()
                                    .expect("root read release receiver")
                                    .await
                                    .expect("root read release");
                            }
                            json!({
                                "thread": {
                                    "id": "provider-root",
                                    "createdAt": 1,
                                    "updatedAt": 2,
                                    "status": {"type": "idle"},
                                    "turns": [{
                                        "id": "root-turn",
                                        "status": "completed",
                                        "startedAt": 1,
                                        "completedAt": 2,
                                        "items": [{
                                            "id": "root-spawn",
                                            "type": "subAgentActivity",
                                            "agentThreadId": "duplicate-child",
                                            "agentPath": "/root/duplicate",
                                            "kind": "started"
                                        }]
                                    }]
                                }
                            })
                        }
                        Some("duplicate-child") => {
                            assert_eq!(
                                message["params"],
                                json!({"threadId": "duplicate-child", "includeTurns": false})
                            );
                            *peer_child_read_count
                                .lock()
                                .expect("child read count mutex") += 1;
                            json!({
                                "thread": {
                                    "id": "duplicate-child",
                                    "parentThreadId": "provider-root",
                                    "createdAt": 2,
                                    "updatedAt": 2,
                                    "status": {"type": "notLoaded"},
                                    "turns": []
                                }
                            })
                        }
                        other => panic!("unexpected thread/read target {other:?}"),
                    };
                    let metadata = thread_history_metadata(&full);
                    history = Some(full);
                    metadata
                }
                "thread/turns/list" => thread_turns_page(
                    &message["params"],
                    history
                        .as_ref()
                        .expect("the history read precedes its pages"),
                ),
                "thread/list" => {
                    assert_eq!(
                        message["params"],
                        json!({"ancestorThreadId": "provider-root", "limit": 50})
                    );
                    empty_thread_list_result()
                }
                "thread/backgroundTerminals/list" => {
                    assert_eq!(
                        message["params"],
                        json!({"threadId": "provider-root", "limit": 128})
                    );
                    empty_background_terminal_list_result()
                }
                "shutdown" => {
                    assert_eq!(message["params"], Value::Null);
                    write_json(
                        &mut writer,
                        json!({"id": message["id"].clone(), "result": null}),
                    )
                    .await;
                    break;
                }
                other => panic!("unexpected request in duplicate-hint test: {other}"),
            };
            write_json(
                &mut writer,
                json!({"id": message["id"].clone(), "result": result}),
            )
            .await;
        }
    });

    runtime.start().await.expect("runtime starts");
    runtime.collect_events(2).await;
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "thread/started".to_owned(),
            params: json!({"thread": {"id": "provider-root"}}),
            emitted_at_ms: 1_000,
        })
        .expect("root notification");
    timeout(Duration::from_secs(1), root_read_seen_rx)
        .await
        .expect("root read begins")
        .expect("root read signal");
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "item/started".to_owned(),
            params: json!({
                "threadId": "provider-root",
                "turnId": "root-turn",
                "item": {
                    "id": "live-spawn",
                    "type": "subAgentActivity",
                    "agentThreadId": "duplicate-child",
                    "agentPath": "/root/duplicate",
                    "kind": "started"
                }
            }),
            emitted_at_ms: 1_001,
        })
        .expect("duplicate live hint");
    release_root_read_tx.send(()).expect("release root read");

    next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:0")
    })
    .await;
    assert_eq!(*child_read_count.lock().expect("child read count mutex"), 1);

    runtime.shutdown().await.expect("runtime shuts down");
    peer_task.await.expect("peer task");
}

#[tokio::test]
async fn reconciliation_defers_nested_hint_beyond_per_pass_descendant_limit() {
    let (connection, _protocol_incoming, mut peer) = scripted_peer();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(
        codex_activity_options(Some("provider-root")),
        connection,
        incoming_rx,
    );
    peer.expect_request("initialize", fixture("initialize-params.json"))
        .respond(json!({}));
    peer.expect_notification("initialized");
    peer.expect_request(
        "thread/resume",
        json!({
            "threadId": "provider-root",
            "cwd": "/tmp/project",
            "approvalPolicy": "never",
            "sandbox": "danger-full-access",
            "model": "gpt-5.3-codex",
            "serviceTier": null,
        }),
    )
    .respond(json!({
        "thread": {"id": "provider-root"},
        "cwd": "/tmp/project",
        "model": "gpt-5.3-codex"
    }));
    let root_items = (0..50)
        .map(|index| {
            json!({
                "id": format!("spawn-{index}"),
                "type": "subAgentActivity",
                "agentThreadId": format!("limited-child-{index}"),
                "agentPath": format!("/root/limited-{index}"),
                "kind": "started"
            })
        })
        .collect::<Vec<_>>();
    let root_read = json!({
        "thread": {
            "id": "provider-root",
            "createdAt": 1,
            "updatedAt": 3,
            "status": {"type": "idle"},
            "turns": [{
                "id": "root-turn",
                "status": "completed",
                "startedAt": 1,
                "completedAt": 3,
                "items": root_items
            }]
        }
    });
    peer.expect_request(
        "thread/read",
        json!({"threadId": "provider-root", "includeTurns": false}),
    )
    .respond_history(root_read.clone());
    peer.expect_request(
        "thread/list",
        json!({"ancestorThreadId": "provider-root", "limit": 50}),
    )
    .respond(empty_thread_list_result());
    for index in 0..50 {
        let thread_id = format!("limited-child-{index}");
        let turns = if index == 0 {
            vec![json!({
                "id": "limited-child-turn",
                "status": "completed",
                "startedAt": 2,
                "completedAt": 3,
                "items": [{
                    "id": "spawn-deferred-nested",
                    "type": "subAgentActivity",
                    "agentThreadId": "deferred-nested",
                    "agentPath": "/root/limited-0/deferred-nested",
                    "kind": "started"
                }]
            })]
        } else {
            Vec::new()
        };
        peer.expect_request(
            "thread/read",
            json!({"threadId": thread_id, "includeTurns": false}),
        )
        .respond_history(json!({
            "thread": {
                "id": thread_id,
                "parentThreadId": "provider-root",
                "createdAt": 2,
                "updatedAt": 3,
                "status": {"type": "notLoaded"},
                "turns": turns
            }
        }));
    }
    peer.expect_request(
        "thread/backgroundTerminals/list",
        json!({"threadId": "provider-root", "limit": 128}),
    )
    .respond(empty_background_terminal_list_result());
    peer.expect_request(
        "thread/read",
        json!({"threadId": "provider-root", "includeTurns": false}),
    )
    .respond_history(root_read);
    peer.expect_request(
        "thread/list",
        json!({"ancestorThreadId": "provider-root", "limit": 50}),
    )
    .respond(empty_thread_list_result());
    peer.expect_request(
        "thread/read",
        json!({"threadId": "deferred-nested", "includeTurns": false}),
    )
    .respond_history(json!({
        "thread": {
            "id": "deferred-nested",
            "parentThreadId": "limited-child-0",
            "createdAt": 3,
            "updatedAt": 4,
            "status": {"type": "notLoaded"},
            "turns": [{
                "id": "deferred-nested-turn",
                "status": "completed",
                "startedAt": 3,
                "completedAt": 4,
                "items": [{
                    "id": "deferred-nested-result",
                    "type": "agentMessage",
                    "text": "Deferred nested result"
                }]
            }]
        }
    }));
    peer.expect_request(
        "thread/backgroundTerminals/list",
        json!({"threadId": "provider-root", "limit": 128}),
    )
    .respond(empty_background_terminal_list_result());
    peer.expect_request("shutdown", Value::Null)
        .respond(Value::Null);
    let peer_task = tokio::spawn(peer.run());

    runtime.start().await.expect("runtime starts");
    runtime.collect_events(2).await;
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "thread/started".to_owned(),
            params: json!({"thread": {"id": "provider-root"}}),
            emitted_at_ms: 1_000,
        })
        .expect("root notification");
    let first_reconciliation = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:0")
    })
    .await;
    assert!(
        !first_reconciliation
            .activity
            .iter()
            .any(|mutation| matches!(
                mutation,
                ProviderActivityMutation::AppendEntry(entry)
                    if entry.detail.as_deref() == Some("Deferred nested result")
            ))
    );
    let second_reconciliation = timeout(Duration::from_secs(2), async {
        next_codex_event_matching(&runtime, |event| {
            event.native_event_id.as_deref() == Some("codex:reconciliation:1")
        })
        .await
    })
    .await
    .expect("deferred nested reconciliation");
    assert!(
        second_reconciliation
            .activity
            .iter()
            .any(|mutation| matches!(
                mutation,
                ProviderActivityMutation::AppendEntry(entry)
                    if entry.detail.as_deref() == Some("Deferred nested result")
            ))
    );

    runtime.shutdown().await.expect("runtime shuts down");
    peer_task.await.expect("peer task");
}

#[tokio::test]
async fn reconciliation_shared_descendant_namespace_preserves_exact_structural_budget() {
    let (connection, _protocol_incoming, peer) = scripted_peer();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(
        codex_activity_options(Some("provider-root")),
        connection,
        incoming_rx,
    );
    let peer_task = tokio::spawn(async move {
        let mut reader = BufReader::new(peer.stdin);
        let mut writer = peer.stdout;
        let mut history: Option<Value> = None;
        loop {
            let request = read_scripted_message(&mut reader, &mut writer).await;
            let method = request["method"].as_str().expect("request method");
            let result = match method {
                "initialize" => json!({}),
                "thread/resume" => {
                    assert_eq!(
                        request["params"],
                        json!({
                            "threadId": "provider-root",
                            "cwd": "/tmp/project",
                            "approvalPolicy": "never",
                            "sandbox": "danger-full-access",
                            "model": "gpt-5.3-codex",
                            "serviceTier": null,
                        })
                    );
                    json!({
                        "thread": {"id": "provider-root"},
                        "cwd": "/tmp/project",
                        "model": "gpt-5.3-codex"
                    })
                }
                "thread/read" => {
                    let full = {
                        assert_eq!(request["params"]["includeTurns"], false);
                        let thread_id = request["params"]["threadId"]
                            .as_str()
                            .expect("thread/read target");
                        if thread_id == "provider-root" {
                            json!({
                                "thread": {
                                    "id": "provider-root",
                                    "createdAt": 1,
                                    "updatedAt": 3,
                                    "status": {"type": "idle"},
                                    "turns": [{
                                        "id": "root-turn",
                                        "status": "completed",
                                        "startedAt": 1,
                                        "completedAt": 3,
                                        "items": (0..50)
                                            .map(|index| json!({
                                                "id": format!("spawn-budget-root-{index}"),
                                                "type": "subAgentActivity",
                                                "agentThreadId": format!("budget-root-child-{index}"),
                                                "agentPath": format!("/root/budget-root-child-{index}"),
                                                "kind": "started"
                                            }))
                                            .collect::<Vec<_>>()
                                    }]
                                }
                            })
                        } else if let Some(index) = thread_id.strip_prefix("budget-root-child-") {
                            let turns = if index == "0" {
                                vec![json!({
                                    "id": "budget-nested-source-turn",
                                    "status": "completed",
                                    "startedAt": 2,
                                    "completedAt": 3,
                                    "items": (0..50)
                                        .map(|nested_index| json!({
                                            "id": format!("spawn-budget-nested-{nested_index}"),
                                            "type": "subAgentActivity",
                                            "agentThreadId": format!("budget-nested-{nested_index}"),
                                            "agentPath": format!(
                                                "/root/budget-root-child-0/budget-nested-{nested_index}"
                                            ),
                                            "kind": "started"
                                        }))
                                        .collect::<Vec<_>>()
                                })]
                            } else {
                                Vec::new()
                            };
                            json!({
                                "thread": {
                                    "id": thread_id,
                                    "parentThreadId": "provider-root",
                                    "createdAt": 2,
                                    "updatedAt": 3,
                                    "status": {"type": "notLoaded"},
                                    "turns": turns
                                }
                            })
                        } else if thread_id.starts_with("budget-nested-") {
                            json!({
                                "thread": {
                                    "id": thread_id,
                                    "parentThreadId": "budget-root-child-0",
                                    "createdAt": 3,
                                    "updatedAt": 4,
                                    "status": {"type": "notLoaded"},
                                    "turns": []
                                }
                            })
                        } else {
                            panic!("unexpected structural-budget read target {thread_id}");
                        }
                    };
                    let metadata = thread_history_metadata(&full);
                    history = Some(full);
                    metadata
                }
                "thread/turns/list" => thread_turns_page(
                    &request["params"],
                    history
                        .as_ref()
                        .expect("the history read precedes its pages"),
                ),
                "thread/list" => {
                    assert_eq!(
                        request["params"],
                        json!({"ancestorThreadId": "provider-root", "limit": 50})
                    );
                    empty_thread_list_result()
                }
                "thread/backgroundTerminals/list" => {
                    assert_eq!(
                        request["params"],
                        json!({"threadId": "provider-root", "limit": 128})
                    );
                    json!({
                        "data": (0..128)
                            .map(|index| json!({
                                "itemId": format!("budget-background-{index}"),
                                "processId": format!("budget-process-{index}"),
                                "command": format!("budget command {index}"),
                                "cwd": "/tmp/project"
                            }))
                            .collect::<Vec<_>>(),
                        "nextCursor": null
                    })
                }
                "shutdown" => {
                    write_json(
                        &mut writer,
                        json!({"id": request["id"].clone(), "result": null}),
                    )
                    .await;
                    break;
                }
                "initialized" => continue,
                other => panic!("unexpected structural-budget method {other}"),
            };
            write_json(
                &mut writer,
                json!({"id": request["id"].clone(), "result": result}),
            )
            .await;
        }
    });

    runtime.start().await.expect("runtime starts");
    runtime.collect_events(2).await;
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "thread/started".to_owned(),
            params: json!({"thread": {"id": "provider-root"}}),
            emitted_at_ms: 1_000,
        })
        .expect("root notification");
    let reconciliation = timeout(Duration::from_secs(2), async {
        next_codex_event_matching(&runtime, |event| {
            event.native_event_id.as_deref() == Some("codex:reconciliation:0")
        })
        .await
    })
    .await
    .expect("bounded reconciliation survives nested hint pressure");
    let structural_count = reconciliation
        .activity
        .iter()
        .filter(|mutation| !matches!(mutation, ProviderActivityMutation::AppendEntry(_)))
        .count();
    assert_eq!(structural_count, 230);
    assert!(reconciliation.activity.len() <= 256);
    assert!(!reconciliation.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::UpsertActor(actor)
            if actor.id.starts_with("codex:thread:budget-nested-")
    )));

    runtime.shutdown().await.expect("runtime shuts down");
    peer_task.await.expect("peer task");
}

#[tokio::test]
async fn reconciliation_rejects_mismatched_and_foreign_parent_direct_reads() {
    let (connection, _protocol_incoming, mut peer) = scripted_peer();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(codex_activity_options(None), connection, incoming_rx);
    peer.expect_request("initialize", fixture("initialize-params.json"))
        .respond(json!({}));
    peer.expect_notification("initialized");
    peer.expect_request(
        "thread/start",
        json!({
            "cwd": "/tmp/project",
            "approvalPolicy": "never",
            "sandbox": "danger-full-access",
            "model": "gpt-5.3-codex",
            "serviceTier": null,
        }),
    )
    .respond(json!({
        "thread": {"id": "provider-root"},
        "cwd": "/tmp/project",
        "model": "gpt-5.3-codex"
    }));
    peer.expect_request(
        "thread/read",
        json!({"threadId": "provider-root", "includeTurns": false}),
    )
    .respond_history(json!({
        "thread": {
            "id": "provider-root",
            "createdAt": 1,
            "updatedAt": 3,
            "status": {"type": "idle"},
            "turns": [{
                "id": "root-turn",
                "status": "completed",
                "startedAt": 1,
                "completedAt": 3,
                "items": [
                    {
                        "id": "spawn-mismatch",
                        "type": "subAgentActivity",
                        "agentThreadId": "mismatched-child",
                        "agentPath": "/root/mismatch",
                        "kind": "started"
                    },
                    {
                        "id": "spawn-foreign",
                        "type": "subAgentActivity",
                        "agentThreadId": "foreign-parent-child",
                        "agentPath": "/root/foreign",
                        "kind": "started"
                    }
                ]
            }]
        }
    }));
    peer.expect_request(
        "thread/list",
        json!({"ancestorThreadId": "provider-root", "limit": 50}),
    )
    .respond(empty_thread_list_result());
    peer.expect_request(
        "thread/read",
        json!({"threadId": "mismatched-child", "includeTurns": false}),
    )
    .respond_history(json!({
        "thread": {
            "id": "different-child",
            "parentThreadId": "provider-root",
            "createdAt": 2,
            "updatedAt": 3,
            "status": {"type": "notLoaded"},
            "turns": [{
                "id": "mismatched-turn",
                "status": "completed",
                "startedAt": 2,
                "completedAt": 3,
                "items": [{
                    "id": "mismatched-result",
                    "type": "agentMessage",
                    "text": "foreign result"
                }]
            }]
        }
    }));
    peer.expect_request(
        "thread/read",
        json!({"threadId": "foreign-parent-child", "includeTurns": false}),
    )
    .respond_history(json!({
        "thread": {
            "id": "foreign-parent-child",
            "parentThreadId": "unrelated-root",
            "createdAt": 2,
            "updatedAt": 3,
            "status": {"type": "notLoaded"},
            "turns": [{
                "id": "foreign-turn",
                "status": "completed",
                "startedAt": 2,
                "completedAt": 3,
                "items": [{
                    "id": "foreign-result",
                    "type": "agentMessage",
                    "text": "foreign result"
                }]
            }]
        }
    }));
    peer.expect_request(
        "thread/backgroundTerminals/list",
        json!({"threadId": "provider-root", "limit": 128}),
    )
    .respond(empty_background_terminal_list_result());
    peer.expect_request("shutdown", Value::Null)
        .respond(Value::Null);
    let peer_task = tokio::spawn(peer.run());

    runtime.start().await.expect("runtime starts");
    runtime.collect_events(2).await;
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "thread/started".to_owned(),
            params: json!({"thread": {"id": "provider-root"}}),
            emitted_at_ms: 1_000,
        })
        .expect("root notification");
    let reconciliation = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:0")
    })
    .await;
    assert!(!reconciliation.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::AppendEntry(entry)
            if entry.detail.as_deref() == Some("foreign result")
    )));

    runtime.shutdown().await.expect("runtime shuts down");
    peer_task.await.expect("peer task");
}

#[tokio::test]
async fn reconciliation_reconnect_fences_late_child_read_and_retries_pending_hint() {
    let (connection_a, _protocol_incoming_a, mut peer_a) = scripted_peer();
    let (incoming_tx_a, incoming_rx_a) = mpsc::unbounded_channel();
    let runtime =
        CodexSessionRuntime::new(codex_activity_options(None), connection_a, incoming_rx_a);
    peer_a
        .expect_request("initialize", fixture("initialize-params.json"))
        .respond(json!({}));
    peer_a.expect_notification("initialized");
    peer_a
        .expect_request(
            "thread/start",
            json!({
                "cwd": "/tmp/project",
                "approvalPolicy": "never",
                "sandbox": "danger-full-access",
                "model": "gpt-5.3-codex",
                "serviceTier": null,
            }),
        )
        .respond(json!({
            "thread": {"id": "provider-root"},
            "cwd": "/tmp/project",
            "model": "gpt-5.3-codex"
        }));
    peer_a
        .expect_request(
            "thread/read",
            json!({"threadId": "provider-root", "includeTurns": false}),
        )
        .respond_history(json!({
            "thread": {
                "id": "provider-root",
                "createdAt": 1,
                "updatedAt": 2,
                "status": {"type": "idle"},
                "turns": [{
                    "id": "root-turn",
                    "status": "completed",
                    "startedAt": 1,
                    "completedAt": 2,
                    "items": [{
                        "id": "spawn-late",
                        "type": "subAgentActivity",
                        "agentThreadId": "late-child",
                        "agentPath": "/root/late",
                        "kind": "started"
                    }]
                }]
            }
        }));
    peer_a
        .expect_request(
            "thread/list",
            json!({"ancestorThreadId": "provider-root", "limit": 50}),
        )
        .respond(empty_thread_list_result());
    let (late_read_seen_tx, late_read_seen_rx) = oneshot::channel();
    let (release_late_read_tx, release_late_read_rx) = oneshot::channel();
    peer_a
        .expect_request(
            "thread/read",
            json!({"threadId": "late-child", "includeTurns": false}),
        )
        .pause_response(late_read_seen_tx, release_late_read_rx)
        .respond_history(json!({
            "thread": {
                "id": "late-child",
                "parentThreadId": "provider-root",
                "createdAt": 2,
                "updatedAt": 3,
                "status": {"type": "notLoaded"},
                "turns": [{
                    "id": "late-turn",
                    "status": "completed",
                    "startedAt": 2,
                    "completedAt": 3,
                    "items": [{
                        "id": "late-result",
                        "type": "agentMessage",
                        "text": "late stale result"
                    }]
                }]
            }
        }));
    let old_peer_task = tokio::spawn(peer_a.run());

    runtime.start().await.expect("initial runtime starts");
    runtime.collect_events(2).await;
    incoming_tx_a
        .send(IncomingEvent::Notification {
            method: "thread/started".to_owned(),
            params: json!({"thread": {"id": "provider-root"}}),
            emitted_at_ms: 1_000,
        })
        .expect("initial root notification");
    timeout(Duration::from_secs(1), late_read_seen_rx)
        .await
        .expect("late child read begins")
        .expect("late child read signal");

    let (connection_b, _protocol_incoming_b, mut peer_b) = scripted_peer();
    let (_incoming_tx_b, incoming_rx_b) = mpsc::unbounded_channel();
    peer_b
        .expect_request("initialize", fixture("initialize-params.json"))
        .respond(json!({}));
    peer_b.expect_notification("initialized");
    peer_b
        .expect_request(
            "thread/resume",
            json!({
                "threadId": "provider-root",
                "cwd": "/tmp/project",
                "approvalPolicy": "never",
                "sandbox": "danger-full-access",
                "model": "gpt-5.3-codex",
                "serviceTier": null,
            }),
        )
        .respond(json!({
            "thread": {"id": "provider-root"},
            "cwd": "/tmp/project",
            "model": "gpt-5.3-codex"
        }));
    peer_b
        .expect_request(
            "thread/read",
            json!({"threadId": "provider-root", "includeTurns": false}),
        )
        .respond_history(empty_root_thread_read_result());
    peer_b
        .expect_request(
            "thread/list",
            json!({"ancestorThreadId": "provider-root", "limit": 50}),
        )
        .respond(empty_thread_list_result());
    peer_b
        .expect_request(
            "thread/read",
            json!({"threadId": "late-child", "includeTurns": false}),
        )
        .respond_history(json!({
            "thread": {
                "id": "late-child",
                "parentThreadId": "provider-root",
                "createdAt": 2,
                "updatedAt": 4,
                "status": {"type": "notLoaded"},
                "turns": [{
                    "id": "current-turn",
                    "status": "completed",
                    "startedAt": 2,
                    "completedAt": 4,
                    "items": [{
                        "id": "current-result",
                        "type": "agentMessage",
                        "text": "current result"
                    }]
                }]
            }
        }));
    peer_b
        .expect_request(
            "thread/backgroundTerminals/list",
            json!({"threadId": "provider-root", "limit": 128}),
        )
        .respond(empty_background_terminal_list_result());
    peer_b
        .expect_request("shutdown", Value::Null)
        .respond(Value::Null);
    let new_peer_task = tokio::spawn(peer_b.run());

    runtime
        .reconnect(connection_b, incoming_rx_b)
        .await
        .expect("runtime reconnects");
    runtime.collect_events(2).await;
    release_late_read_tx
        .send(())
        .expect("release stale child response after epoch replacement");
    let reconciliation = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:0")
    })
    .await;
    assert!(reconciliation.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::AppendEntry(entry)
            if entry.detail.as_deref() == Some("current result")
    )));
    assert!(!reconciliation.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::AppendEntry(entry)
            if entry.detail.as_deref() == Some("late stale result")
    )));

    runtime.shutdown().await.expect("runtime shuts down");
    old_peer_task.await.expect("old peer task");
    new_peer_task.await.expect("new peer task");
}

#[tokio::test]
async fn root_turn_completion_dedupes_recovery_and_recovers_eventual_nested_descendants() {
    let (connection, _protocol_incoming, peer) = scripted_peer();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(
        CodexSessionOptions {
            version: "0.1.1".to_owned(),
            thread_id: "fixture-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: CodexRuntimeMode::FullAccess,
            model: Some("gpt-5.3-codex".to_owned()),
            service_tier: None,
            effort: None,
            resume_cursor: None,
        },
        connection,
        incoming_rx,
    );
    let requests = Arc::new(StdMutex::new(Vec::<Value>::new()));
    let peer_requests = requests.clone();
    let peer_task = tokio::spawn(async move {
        let mut reader = BufReader::new(peer.stdin);
        let mut writer = peer.stdout;
        let mut thread_list_count = 0_u8;
        let mut direct_read_count = 0_u8;
        let mut history: Option<Value> = None;
        loop {
            let message = read_scripted_message(&mut reader, &mut writer).await;
            let Some(method) = message.get("method").and_then(Value::as_str) else {
                continue;
            };
            if message.get("id").is_none() {
                continue;
            }
            peer_requests
                .lock()
                .expect("request log mutex")
                .push(message.clone());
            let result = match method {
                "initialize" => json!({}),
                "thread/start" => json!({
                    "thread": {"id": "provider-root"},
                    "cwd": "/tmp/project",
                    "model": "gpt-5.3-codex"
                }),
                "thread/list" => {
                    thread_list_count += 1;
                    match thread_list_count {
                        1 => json!({
                            "data": [{
                                "id": "child-direct",
                                "parentThreadId": "provider-root",
                                "agentNickname": "Direct child",
                                "agentRole": "worker",
                                "createdAt": 2,
                                "updatedAt": 2,
                                "status": {"type": "notLoaded"}
                            }],
                            "nextCursor": null,
                            "backwardsCursor": null
                        }),
                        2 | 3 => json!({
                            "data": [
                                {
                                    "id": "child-direct",
                                    "parentThreadId": "provider-root",
                                    "agentNickname": "Direct child",
                                    "agentRole": "worker",
                                    "createdAt": 2,
                                    "updatedAt": 4,
                                    "status": {"type": "notLoaded"}
                                },
                                {
                                    "id": "child-nested",
                                    "parentThreadId": "child-direct",
                                    "agentNickname": "Nested child",
                                    "agentRole": "worker",
                                    "createdAt": 3,
                                    "updatedAt": 5,
                                    "status": {"type": "notLoaded"}
                                }
                            ],
                            "nextCursor": null,
                            "backwardsCursor": null
                        }),
                        other => panic!("unexpected thread/list pass {other}"),
                    }
                }
                "thread/read" => {
                    let full = match message["params"]["threadId"].as_str() {
                        Some("provider-root") => {
                            assert_eq!(
                                message["params"],
                                json!({"threadId": "provider-root", "includeTurns": false})
                            );
                            empty_root_thread_read_result()
                        }
                        Some("child-direct") => {
                            direct_read_count += 1;
                            match direct_read_count {
                                1 => json!({
                                    "thread": {
                                        "id": "child-direct",
                                        "parentThreadId": "provider-root",
                                        "agentNickname": "Direct child",
                                        "agentRole": "worker",
                                        "createdAt": 2,
                                        "updatedAt": 2,
                                        "status": {"type": "notLoaded"},
                                        "turns": []
                                    }
                                }),
                                2 => json!({
                                    "thread": {
                                        "id": "child-direct",
                                        "parentThreadId": "provider-root",
                                        "agentNickname": "Direct child",
                                        "agentRole": "worker",
                                        "createdAt": 2,
                                        "updatedAt": 4,
                                        "status": {"type": "notLoaded"},
                                        "turns": [{
                                            "id": "direct-turn",
                                            "status": "completed",
                                            "startedAt": 2,
                                            "completedAt": 4,
                                            "items": [{
                                                "id": "direct-result",
                                                "type": "agentMessage",
                                                "text": "Direct complete final message"
                                            }]
                                        }]
                                    }
                                }),
                                other => panic!("unexpected direct thread/read pass {other}"),
                            }
                        }
                        Some("child-nested") => json!({
                            "thread": {
                                "id": "child-nested",
                                "parentThreadId": "child-direct",
                                "agentNickname": "Nested child",
                                "agentRole": "worker",
                                "createdAt": 3,
                                "updatedAt": 5,
                                "status": {"type": "notLoaded"},
                                "turns": [{
                                    "id": "nested-turn",
                                    "status": "completed",
                                    "startedAt": 3,
                                    "completedAt": 5,
                                    "items": [{
                                        "id": "nested-result",
                                        "type": "agentMessage",
                                        "text": "Nested complete final message"
                                    }]
                                }]
                            }
                        }),
                        other => panic!("unexpected thread/read target {other:?}"),
                    };
                    let metadata = thread_history_metadata(&full);
                    history = Some(full);
                    metadata
                }
                "thread/turns/list" => thread_turns_page(
                    &message["params"],
                    history
                        .as_ref()
                        .expect("the history read precedes its pages"),
                ),
                "thread/backgroundTerminals/list" => {
                    json!({"data": [], "nextCursor": null})
                }
                "shutdown" => Value::Null,
                other => panic!("unexpected request in completion recovery test: {other}"),
            };
            write_json(
                &mut writer,
                json!({"id": message["id"].clone(), "result": result}),
            )
            .await;
            if method == "shutdown" {
                break;
            }
        }
    });

    runtime.start().await.expect("runtime starts");
    runtime.collect_events(2).await;
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "thread/started".to_owned(),
            params: json!({"thread": {"id": "provider-root"}}),
            emitted_at_ms: 1_000,
        })
        .expect("root notification");
    let initial = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:0")
    })
    .await;
    assert!(initial.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::UpsertActor(actor)
            if actor.id == "codex:thread:child-direct"
                && actor.status == ActivityLifecycle::Unknown
    )));

    let root_completion_params = json!({
        "threadId": "provider-root",
        "turn": {
            "id": "root-turn",
            "status": "completed",
            "completedAt": 6
        }
    });
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "turn/completed".to_owned(),
            params: root_completion_params.clone(),
            emitted_at_ms: 6_000,
        })
        .expect("root completion notification");

    let root_completion =
        next_codex_event_matching(&runtime, |event| event.event_type == "turn.completed").await;
    assert!(
        root_completion.activity.is_empty(),
        "the normal root conversation completion must remain free of activity projection"
    );
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "turn/completed".to_owned(),
            params: root_completion_params.clone(),
            emitted_at_ms: 6_001,
        })
        .expect("duplicate root completion inside debounce");
    let duplicate_conversation =
        next_codex_event_matching(&runtime, |event| event.event_type == "turn.completed").await;
    assert!(duplicate_conversation.activity.is_empty());

    let reconciliation = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:1")
    })
    .await;
    for (actor_id, parent_actor_id) in [
        ("codex:thread:child-direct", None),
        (
            "codex:thread:child-nested",
            Some("codex:thread:child-direct"),
        ),
    ] {
        assert!(reconciliation.activity.iter().any(|mutation| matches!(
            mutation,
            ProviderActivityMutation::UpsertActor(actor)
                if actor.id == actor_id
                    && actor.parent_actor_id.as_deref() == parent_actor_id
                    && actor.status == ActivityLifecycle::Completed
        )));
    }
    for (owner_id, detail) in [
        ("codex:thread:child-direct", "Direct complete final message"),
        ("codex:thread:child-nested", "Nested complete final message"),
    ] {
        assert_eq!(
            reconciliation
                .activity
                .iter()
                .filter(|mutation| matches!(
                    mutation,
                    ProviderActivityMutation::AppendEntry(entry)
                        if entry.owner_id == owner_id
                            && entry.detail.as_deref() == Some(detail)
                ))
                .count(),
            1,
            "reconciliation must recover each complete provider message exactly once"
        );
    }
    assert_eq!(
        requests
            .lock()
            .expect("request log mutex")
            .iter()
            .filter(|request| request["method"] == "thread/list")
            .count(),
        2,
        "root completion must schedule exactly one debounced follow-up pass"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "turn/completed".to_owned(),
            params: root_completion_params,
            emitted_at_ms: 7_000,
        })
        .expect("duplicate root completion after debounce");
    let late_duplicate_conversation =
        next_codex_event_matching(&runtime, |event| event.event_type == "turn.completed").await;
    assert!(late_duplicate_conversation.activity.is_empty());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        requests
            .lock()
            .expect("request log mutex")
            .iter()
            .filter(|request| request["method"] == "thread/list")
            .count(),
        2,
        "a stable duplicate after the debounce window must not run another pass"
    );
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "turn/completed".to_owned(),
            params: json!({
                "threadId": "provider-root",
                "turn": {
                    "id": "later-root-turn",
                    "status": "completed",
                    "completedAt": 8
                }
            }),
            emitted_at_ms: 8_000,
        })
        .expect("distinct later root completion");
    let later_conversation =
        next_codex_event_matching(&runtime, |event| event.event_type == "turn.completed").await;
    assert!(later_conversation.activity.is_empty());
    next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:2")
    })
    .await;
    assert_eq!(
        requests
            .lock()
            .expect("request log mutex")
            .iter()
            .filter(|request| request["method"] == "thread/list")
            .count(),
        3,
        "a distinct later root turn must schedule one follow-up pass"
    );

    runtime.shutdown().await.expect("runtime shuts down");
    peer_task.await.expect("peer task");
}

#[tokio::test]
async fn reconciliation_bursty_collaboration_hints_coalesce_into_one_debounced_pass() {
    let (connection, _protocol_incoming, peer) = scripted_peer();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(
        CodexSessionOptions {
            version: "0.1.1".to_owned(),
            thread_id: "fixture-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: CodexRuntimeMode::FullAccess,
            model: Some("gpt-5.3-codex".to_owned()),
            service_tier: None,
            effort: None,
            resume_cursor: None,
        },
        connection,
        incoming_rx,
    );
    let requests = Arc::new(StdMutex::new(Vec::<Value>::new()));
    let peer_requests = requests.clone();
    let peer_task = tokio::spawn(async move {
        let mut reader = BufReader::new(peer.stdin);
        let mut writer = peer.stdout;
        let mut history: Option<Value> = None;
        loop {
            let message = read_scripted_message(&mut reader, &mut writer).await;
            let Some(method) = message.get("method").and_then(Value::as_str) else {
                continue;
            };
            if message.get("id").is_none() {
                continue;
            }
            peer_requests
                .lock()
                .expect("request log mutex")
                .push(message.clone());
            let result = match method {
                "initialize" => json!({}),
                "thread/start" => json!({
                    "thread": {"id": "provider-root"},
                    "cwd": "/tmp/project",
                    "model": "gpt-5.3-codex"
                }),
                "thread/list" => json!({
                    "data": [],
                    "nextCursor": null,
                    "backwardsCursor": null
                }),
                "thread/read" => {
                    let full = {
                        assert_eq!(
                            message["params"],
                            json!({"threadId": "provider-root", "includeTurns": false})
                        );
                        empty_root_thread_read_result()
                    };
                    let metadata = thread_history_metadata(&full);
                    history = Some(full);
                    metadata
                }
                "thread/turns/list" => thread_turns_page(
                    &message["params"],
                    history
                        .as_ref()
                        .expect("the history read precedes its pages"),
                ),
                "thread/backgroundTerminals/list" => {
                    json!({"data": [], "nextCursor": null})
                }
                "shutdown" => Value::Null,
                other => panic!("unexpected request in debounce test: {other}"),
            };
            write_json(
                &mut writer,
                json!({"id": message["id"].clone(), "result": result}),
            )
            .await;
            if method == "shutdown" {
                break;
            }
        }
    });

    runtime.start().await.expect("runtime starts");
    runtime.collect_events(2).await;
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "thread/started".to_owned(),
            params: json!({"thread": {"id": "provider-root"}}),
            emitted_at_ms: 1_000,
        })
        .expect("root notification");
    timeout(Duration::from_secs(1), async {
        loop {
            let list_count = requests
                .lock()
                .expect("request log mutex")
                .iter()
                .filter(|request| request["method"] == "thread/list")
                .count();
            if list_count == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("initial reconciliation");

    incoming_tx
        .send(IncomingEvent::Notification {
            method: "item/started".to_owned(),
            params: json!({
                "threadId": "provider-root",
                "turnId": "root-turn",
                "item": {
                    "id": "spawn-0",
                    "type": "collabAgentToolCall",
                    "tool": "spawnAgent",
                    "status": "inProgress",
                    "senderThreadId": "provider-root",
                    "receiverThreadIds": ["child-0"],
                    "agentsStates": {}
                }
            }),
            emitted_at_ms: 1_001,
        })
        .expect("first collaboration notification");
    tokio::time::sleep(Duration::from_millis(50)).await;
    for index in 1..20 {
        incoming_tx
            .send(IncomingEvent::Notification {
                method: "item/started".to_owned(),
                params: json!({
                    "threadId": "provider-root",
                    "turnId": "root-turn",
                    "item": {
                        "id": format!("spawn-{index}"),
                        "type": "collabAgentToolCall",
                        "tool": "spawnAgent",
                        "status": "inProgress",
                        "senderThreadId": "provider-root",
                        "receiverThreadIds": [format!("child-{index}")],
                        "agentsStates": {}
                    }
                }),
                emitted_at_ms: 1_001 + index,
            })
            .expect("collaboration notification");
    }

    tokio::time::sleep(Duration::from_millis(650)).await;
    assert_eq!(
        requests
            .lock()
            .expect("request log mutex")
            .iter()
            .filter(|request| request["method"] == "thread/list")
            .count(),
        2,
        "the initial pass plus one coalesced burst pass are expected"
    );

    runtime.shutdown().await.expect("runtime shuts down");
    peer_task.await.expect("peer task");
}

#[tokio::test]
async fn reconciliation_repairs_missed_history_and_official_background_terminals_in_one_batch() {
    let (connection, _protocol_incoming, mut peer) = scripted_peer();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(
        CodexSessionOptions {
            version: "0.1.1".to_owned(),
            thread_id: "fixture-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: CodexRuntimeMode::FullAccess,
            model: Some("gpt-5.3-codex".to_owned()),
            service_tier: None,
            effort: None,
            resume_cursor: None,
        },
        connection,
        incoming_rx,
    );
    peer.expect_request("initialize", fixture("initialize-params.json"))
        .respond(json!({}));
    peer.expect_notification("initialized");
    peer.expect_request(
        "thread/start",
        json!({
            "cwd": "/tmp/project",
            "approvalPolicy": "never",
            "sandbox": "danger-full-access",
            "model": "gpt-5.3-codex",
            "serviceTier": null,
        }),
    )
    .respond(json!({
        "thread": {"id": "provider-root"},
        "cwd": "/tmp/project",
        "model": "gpt-5.3-codex"
    }));
    peer.expect_request(
        "thread/read",
        json!({"threadId": "provider-root", "includeTurns": false}),
    )
    .respond_history(empty_root_thread_read_result());
    peer.expect_request(
        "thread/list",
        json!({"ancestorThreadId": "provider-root", "limit": 50}),
    )
    .respond(json!({
        "data": [
            {
                "id": "child-missed",
                "parentThreadId": "provider-root",
                "agentNickname": "Missed child",
                "agentRole": "worker",
                "createdAt": 1,
                "updatedAt": 2,
                "status": {"type": "idle"}
            },
            {
                "id": "foreign-child",
                "parentThreadId": "unrelated-root",
                "createdAt": 1,
                "updatedAt": 2,
                "status": {"type": "active", "activeFlags": []}
            },
            {
                "parentThreadId": "provider-root",
                "createdAt": 1,
                "updatedAt": 2,
                "status": {"type": "idle"}
            }
        ],
        "nextCursor": null,
        "backwardsCursor": null
    }));
    peer.expect_request(
        "thread/read",
        json!({"threadId": "child-missed", "includeTurns": false}),
    )
    .respond_history(json!({
        "thread": {
            "id": "child-missed",
            "parentThreadId": "provider-root",
            "agentNickname": "Missed child",
            "agentRole": "worker",
            "createdAt": 1,
            "updatedAt": 3,
            "status": {"type": "idle"},
            "turns": [{
                "id": "child-turn-1",
                "status": "completed",
                "startedAt": 2,
                "completedAt": 3,
                "items": [{
                    "type": "agentMessage",
                    "id": "child-message-1",
                    "text": "Recovered child result"
                }]
            }]
        }
    }));
    peer.expect_request(
        "thread/backgroundTerminals/list",
        json!({"threadId": "provider-root", "limit": 128}),
    )
    .respond(json!({
        "data": [
            {
                "itemId": "background-item-1",
                "processId": "process-1",
                "command": "cargo test",
                "cwd": "/tmp/project",
                "osPid": 42,
                "cpuPercent": 1.5,
                "rssKb": 1024
            },
            {
                "processId": "missing-item-id",
                "command": "must be ignored",
                "cwd": "/tmp/project"
            }
        ],
        "nextCursor": null
    }));
    peer.expect_request("shutdown", Value::Null)
        .respond(Value::Null);
    let peer_task = tokio::spawn(peer.run());

    runtime.start().await.expect("runtime starts");
    runtime.collect_events(2).await;
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "thread/started".to_owned(),
            params: json!({"thread": {"id": "provider-root"}}),
            emitted_at_ms: 1_000,
        })
        .expect("root notification");

    let event = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:0")
    })
    .await;
    assert_eq!(event.event_type, "activity.native");
    assert!(event.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::UpsertActor(actor)
            if actor.id == "codex:thread:child-missed"
                && actor.parent_actor_id.is_none()
                && actor.status == ActivityLifecycle::Waiting
    )));
    assert!(!event.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::UpsertActor(actor)
            if actor.id == "codex:thread:foreign-child"
    )));
    assert!(event.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::AppendEntry(entry)
            if entry.id.starts_with("codex:event:commentary:")
                && entry.detail.as_deref() == Some("Recovered child result")
    )));
    assert!(event.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::UpsertWorkItem(work_item)
            if work_item.id == "codex:item:background-item-1"
                && work_item.name == "cargo test"
                && work_item.status == ActivityLifecycle::Running
    )));
    assert!(event.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::SetScope {
            capabilities,
            observation_state: ActivityObservationState::Live,
        } if capabilities.actors
            && capabilities.attributed_activity
            && capabilities.background_work
            && capabilities.history_recovery
                == bibcode_server::activity::ActivityHistoryRecovery::Full
            && !capabilities.terminal_observation
    )));

    runtime.shutdown().await.expect("runtime shuts down");
    peer_task.await.expect("peer task");
}

#[tokio::test]
async fn reconciliation_background_method_downgrade_isolated_and_warned_once() {
    assert_background_list_downgrades_once(json!({
        "code": -32601,
        "message": "Method not found",
        "data": null
    }))
    .await;
}

/// Codex rejects a method its request enum lacks while decoding the request, with -32600
/// (`deserialize_client_request` in `codex-rs/app-server/src/message_processor.rs`; the same
/// shape `thread/rollback` gets from a live 0.162.1).
#[tokio::test]
async fn reconciliation_downgrades_a_method_codex_rejects_as_an_unknown_variant() {
    assert_background_list_downgrades_once(json!({
        "code": -32600,
        "message": "Invalid request: unknown variant `thread/backgroundTerminals/list`, expected one of `initialize`, `thread/start`, `thread/resume`"
    }))
    .await;
}

/// A -32600 that is not about the method name is an invalid request, not a missing method:
/// the pass is stale and the next pass asks again.
#[tokio::test]
async fn reconciliation_retries_an_invalid_request_instead_of_downgrading() {
    let (connection, _protocol_incoming, mut peer) = scripted_peer();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let runtime = project_runtime(connection, incoming_rx);
    peer.expect_request("initialize", fixture("initialize-params.json"))
        .respond(json!({}));
    peer.expect_notification("initialized");
    peer.expect_request(
        "thread/start",
        fixture("reconnect-scenario.json")["initialThreadStartRequest"].clone(),
    )
    .respond(json!({
        "thread": {"id": "provider-root"},
        "cwd": "/tmp/project",
        "model": "gpt-5.3-codex"
    }));
    for background_list in [
        Err(json!({
            "code": -32600,
            "message": "Invalid request: missing field `threadId`"
        })),
        Ok(empty_background_terminal_list_result()),
    ] {
        peer.expect_request(
            "thread/read",
            json!({"threadId": "provider-root", "includeTurns": false}),
        )
        .respond_history(empty_root_thread_read_result());
        peer.expect_request(
            "thread/list",
            json!({"ancestorThreadId": "provider-root", "limit": 50}),
        )
        .respond(empty_thread_list_result());
        let step = peer.expect_request(
            "thread/backgroundTerminals/list",
            json!({"threadId": "provider-root", "limit": 128}),
        );
        match background_list {
            Ok(result) => step.respond(result),
            Err(error) => step.respond_error(error),
        };
    }
    peer.expect_request("shutdown", Value::Null)
        .respond(Value::Null);
    let peer_task = tokio::spawn(peer.run());

    runtime.start().await.expect("runtime starts");
    runtime.collect_events(2).await;
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "thread/started".to_owned(),
            params: json!({"thread": {"id": "provider-root"}}),
            emitted_at_ms: 1_000,
        })
        .expect("root notification");
    let stale = timeout(Duration::from_secs(2), async {
        loop {
            let event = runtime.next_event().await.expect("reconciliation event");
            assert_ne!(
                event.event_type, "runtime.warning",
                "an invalid request must not be reported as an unsupported method"
            );
            if event.native_event_id.as_deref() == Some("codex:reconciliation:0") {
                return event;
            }
        }
    })
    .await
    .expect("stale reconciliation");
    assert!(stale.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::SetScope {
            observation_state: ActivityObservationState::Stale,
            ..
        }
    )));

    incoming_tx
        .send(IncomingEvent::Notification {
            method: "item/started".to_owned(),
            params: json!({
                "threadId": "provider-root",
                "turnId": "root-turn",
                "item": {
                    "id": "spawn-after-invalid-request",
                    "type": "collabAgentToolCall",
                    "tool": "spawnAgent",
                    "status": "inProgress",
                    "senderThreadId": "provider-root",
                    "receiverThreadIds": ["child-after-invalid-request"],
                    "agentsStates": {}
                }
            }),
            emitted_at_ms: 1_001,
        })
        .expect("follow-up collaboration hint");
    let retried = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:1")
    })
    .await;
    assert!(retried.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::SetScope {
            observation_state: ActivityObservationState::Live,
            ..
        }
    )));

    runtime.shutdown().await.expect("runtime shuts down");
    peer_task.await.expect("peer task");
}

async fn assert_background_list_downgrades_once(missing_method_error: Value) {
    let (connection, _protocol_incoming, mut peer) = scripted_peer();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(
        CodexSessionOptions {
            version: "0.1.1".to_owned(),
            thread_id: "fixture-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: CodexRuntimeMode::FullAccess,
            model: Some("gpt-5.3-codex".to_owned()),
            service_tier: None,
            effort: None,
            resume_cursor: None,
        },
        connection,
        incoming_rx,
    );
    peer.expect_request("initialize", fixture("initialize-params.json"))
        .respond(json!({}));
    peer.expect_notification("initialized");
    peer.expect_request(
        "thread/start",
        json!({
            "cwd": "/tmp/project",
            "approvalPolicy": "never",
            "sandbox": "danger-full-access",
            "model": "gpt-5.3-codex",
            "serviceTier": null,
        }),
    )
    .respond(json!({
        "thread": {"id": "provider-root"},
        "cwd": "/tmp/project",
        "model": "gpt-5.3-codex"
    }));
    peer.expect_request(
        "thread/read",
        json!({"threadId": "provider-root", "includeTurns": false}),
    )
    .respond_history(empty_root_thread_read_result());
    peer.expect_request(
        "thread/list",
        json!({"ancestorThreadId": "provider-root", "limit": 50}),
    )
    .respond(json!({
        "data": [],
        "nextCursor": null,
        "backwardsCursor": null
    }));
    peer.expect_request(
        "thread/backgroundTerminals/list",
        json!({"threadId": "provider-root", "limit": 128}),
    )
    .respond_error(missing_method_error);
    peer.expect_request(
        "thread/read",
        json!({"threadId": "provider-root", "includeTurns": false}),
    )
    .respond_history(empty_root_thread_read_result());
    peer.expect_request(
        "thread/list",
        json!({"ancestorThreadId": "provider-root", "limit": 50}),
    )
    .respond(json!({
        "data": [],
        "nextCursor": null,
        "backwardsCursor": null
    }));
    peer.expect_request("shutdown", Value::Null)
        .respond(Value::Null);
    let peer_task = tokio::spawn(peer.run());

    runtime.start().await.expect("runtime starts");
    runtime.collect_events(2).await;
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "thread/started".to_owned(),
            params: json!({"thread": {"id": "provider-root"}}),
            emitted_at_ms: 1_000,
        })
        .expect("root notification");

    let (warning, first_reconciliation) = timeout(Duration::from_secs(2), async {
        let mut warning = None;
        let mut reconciliation = None;
        while warning.is_none() || reconciliation.is_none() {
            let event = runtime.next_event().await.expect("downgrade event");
            if event.event_type == "runtime.warning" {
                warning = Some(event);
            } else if event.native_event_id.as_deref() == Some("codex:reconciliation:0") {
                reconciliation = Some(event);
            }
        }
        (warning.unwrap(), reconciliation.unwrap())
    })
    .await
    .expect("background downgrade events");
    assert!(
        warning.payload["message"]
            .as_str()
            .is_some_and(|message| message.contains("thread/backgroundTerminals/list"))
    );
    assert!(
        first_reconciliation
            .activity
            .iter()
            .any(|mutation| matches!(
                mutation,
                ProviderActivityMutation::SetScope {
                    capabilities,
                    observation_state: ActivityObservationState::Live,
                } if capabilities.actors
                    && capabilities.attributed_activity
                    && !capabilities.background_work
                    && capabilities.history_recovery
                        == bibcode_server::activity::ActivityHistoryRecovery::Full
            ))
    );
    assert!(
        first_reconciliation
            .activity
            .iter()
            .any(|mutation| matches!(
                mutation,
                ProviderActivityMutation::SetSectionHealth {
                    section: ActivitySection::BackgroundTasks,
                    health,
                } if health.state == ActivitySectionObservationState::Error
                    && !health.retryable
            ))
    );
    assert!(
        !first_reconciliation
            .activity
            .iter()
            .any(|mutation| matches!(
                mutation,
                ProviderActivityMutation::SetSectionHealth {
                    section: ActivitySection::Subagents,
                    ..
                }
            ))
    );

    incoming_tx
        .send(IncomingEvent::Notification {
            method: "item/started".to_owned(),
            params: json!({
                "threadId": "provider-root",
                "turnId": "root-turn",
                "item": {
                    "id": "spawn-after-downgrade",
                    "type": "collabAgentToolCall",
                    "tool": "spawnAgent",
                    "status": "inProgress",
                    "senderThreadId": "provider-root",
                    "receiverThreadIds": ["child-after-downgrade"],
                    "agentsStates": {}
                }
            }),
            emitted_at_ms: 1_001,
        })
        .expect("post-downgrade collaboration hint");
    let second_reconciliation = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:1")
    })
    .await;
    assert!(
        second_reconciliation
            .activity
            .iter()
            .any(|mutation| matches!(
                mutation,
                ProviderActivityMutation::SetScope { capabilities, .. }
                    if !capabilities.background_work
            ))
    );
    assert!(
        timeout(Duration::from_millis(150), runtime.next_event())
            .await
            .is_err(),
        "an unsupported method emits only one operational warning"
    );

    runtime.shutdown().await.expect("runtime shuts down");
    peer_task.await.expect("peer task");
}

#[tokio::test]
async fn reconciliation_never_advertises_full_before_thread_read_is_proven() {
    let (connection, _protocol_incoming, mut peer) = scripted_peer();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(
        CodexSessionOptions {
            version: "0.1.1".to_owned(),
            thread_id: "fixture-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: CodexRuntimeMode::FullAccess,
            model: Some("gpt-5.3-codex".to_owned()),
            service_tier: None,
            effort: None,
            resume_cursor: None,
        },
        connection,
        incoming_rx,
    );
    peer.expect_request("initialize", fixture("initialize-params.json"))
        .respond(json!({}));
    peer.expect_notification("initialized");
    peer.expect_request(
        "thread/start",
        json!({
            "cwd": "/tmp/project",
            "approvalPolicy": "never",
            "sandbox": "danger-full-access",
            "model": "gpt-5.3-codex",
            "serviceTier": null,
        }),
    )
    .respond(json!({
        "thread": {"id": "provider-root"},
        "cwd": "/tmp/project",
        "model": "gpt-5.3-codex"
    }));
    peer.expect_request(
        "thread/read",
        json!({"threadId": "provider-root", "includeTurns": false}),
    )
    .respond_history(json!({
        "thread": {
            "id": "wrong-root",
            "createdAt": 1,
            "updatedAt": 1,
            "status": {"type": "idle"},
            "turns": []
        }
    }));
    peer.expect_request(
        "thread/list",
        json!({"ancestorThreadId": "provider-root", "limit": 50}),
    )
    .respond(json!({
        "data": [],
        "nextCursor": null,
        "backwardsCursor": null
    }));
    peer.expect_request(
        "thread/backgroundTerminals/list",
        json!({"threadId": "provider-root", "limit": 128}),
    )
    .respond(json!({"data": [], "nextCursor": null}));
    peer.expect_request(
        "thread/read",
        json!({"threadId": "provider-root", "includeTurns": false}),
    )
    .respond_error(json!({
        "code": -32601,
        "message": "Method not found",
        "data": null
    }));
    peer.expect_request(
        "thread/list",
        json!({"ancestorThreadId": "provider-root", "limit": 50}),
    )
    .respond(json!({
        "data": [{
            "id": "child-read-unsupported",
            "parentThreadId": "provider-root",
            "createdAt": 1,
            "updatedAt": 2,
            "status": {"type": "idle"}
        }],
        "nextCursor": null,
        "backwardsCursor": null
    }));
    peer.expect_request(
        "thread/backgroundTerminals/list",
        json!({"threadId": "provider-root", "limit": 128}),
    )
    .respond(json!({"data": [], "nextCursor": null}));
    peer.expect_request("shutdown", Value::Null)
        .respond(Value::Null);
    let peer_task = tokio::spawn(peer.run());

    runtime.start().await.expect("runtime starts");
    runtime.collect_events(2).await;
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "thread/started".to_owned(),
            params: json!({"thread": {"id": "provider-root"}}),
            emitted_at_ms: 1_000,
        })
        .expect("root notification");
    let first_reconciliation = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:0")
    })
    .await;

    incoming_tx
        .send(IncomingEvent::Notification {
            method: "item/started".to_owned(),
            params: json!({
                "threadId": "provider-root",
                "turnId": "root-turn",
                "item": {
                    "id": "spawn-read-unsupported",
                    "type": "collabAgentToolCall",
                    "tool": "spawnAgent",
                    "status": "inProgress",
                    "senderThreadId": "provider-root",
                    "receiverThreadIds": ["child-read-unsupported"],
                    "agentsStates": {}
                }
            }),
            emitted_at_ms: 1_001,
        })
        .expect("second reconciliation hint");
    let (warning, second_reconciliation) = timeout(Duration::from_secs(2), async {
        let mut warning = None;
        let mut reconciliation = None;
        while warning.is_none() || reconciliation.is_none() {
            let event = runtime.next_event().await.expect("capability event");
            if event.event_type == "runtime.warning" {
                warning = Some(event);
            } else if event.native_event_id.as_deref() == Some("codex:reconciliation:1") {
                reconciliation = Some(event);
            }
        }
        (warning.unwrap(), reconciliation.unwrap())
    })
    .await
    .expect("read unsupported events");

    runtime.shutdown().await.expect("runtime shuts down");
    peer_task.await.expect("peer task");

    assert!(
        warning.payload["message"]
            .as_str()
            .is_some_and(|message| message.contains("thread/read"))
    );
    for reconciliation in [&first_reconciliation, &second_reconciliation] {
        assert!(
            reconciliation.activity.iter().any(|mutation| matches!(
                mutation,
                ProviderActivityMutation::SetScope {
                    capabilities,
                    observation_state: ActivityObservationState::Live,
                } if capabilities.history_recovery
                    == bibcode_server::activity::ActivityHistoryRecovery::Bounded
            )),
            "history remains bounded until both list and read support are proven"
        );
        assert!(
            !reconciliation.activity.iter().any(|mutation| matches!(
                mutation,
                ProviderActivityMutation::SetScope { capabilities, .. }
                    if capabilities.history_recovery
                        == bibcode_server::activity::ActivityHistoryRecovery::Full
            )),
            "an empty list or an unsupported read must never advertise full history"
        );
    }
}

#[tokio::test]
async fn reconciliation_wrong_thread_read_does_not_prove_support_or_apply_history() {
    let (connection, _protocol_incoming, mut peer) = scripted_peer();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(
        CodexSessionOptions {
            version: "0.1.1".to_owned(),
            thread_id: "fixture-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: CodexRuntimeMode::FullAccess,
            model: Some("gpt-5.3-codex".to_owned()),
            service_tier: None,
            effort: None,
            resume_cursor: None,
        },
        connection,
        incoming_rx,
    );
    peer.expect_request("initialize", fixture("initialize-params.json"))
        .respond(json!({}));
    peer.expect_notification("initialized");
    peer.expect_request(
        "thread/start",
        json!({
            "cwd": "/tmp/project",
            "approvalPolicy": "never",
            "sandbox": "danger-full-access",
            "model": "gpt-5.3-codex",
            "serviceTier": null,
        }),
    )
    .respond(json!({
        "thread": {"id": "provider-root"},
        "cwd": "/tmp/project",
        "model": "gpt-5.3-codex"
    }));
    for (response_thread_id, turn_id, message_id, message) in [
        (
            "different-child",
            "wrong-turn",
            "wrong-message",
            "Wrong thread history",
        ),
        (
            "requested-child",
            "correct-turn",
            "correct-message",
            "Correct thread history",
        ),
    ] {
        let root_response_thread_id = if response_thread_id == "different-child" {
            "wrong-root"
        } else {
            "provider-root"
        };
        peer.expect_request(
            "thread/read",
            json!({"threadId": "provider-root", "includeTurns": false}),
        )
        .respond_history(json!({
            "thread": {
                "id": root_response_thread_id,
                "createdAt": 1,
                "updatedAt": 1,
                "status": {"type": "idle"},
                "turns": []
            }
        }));
        peer.expect_request(
            "thread/list",
            json!({"ancestorThreadId": "provider-root", "limit": 50}),
        )
        .respond(json!({
            "data": [{
                "id": "requested-child",
                "parentThreadId": "provider-root",
                "createdAt": 1,
                "updatedAt": 2,
                "status": {"type": "idle"}
            }],
            "nextCursor": null,
            "backwardsCursor": null
        }));
        peer.expect_request(
            "thread/read",
            json!({"threadId": "requested-child", "includeTurns": false}),
        )
        .respond_history(json!({
            "thread": {
                "id": response_thread_id,
                "parentThreadId": "provider-root",
                "createdAt": 1,
                "updatedAt": 2,
                "status": {"type": "idle"},
                "turns": [{
                    "id": turn_id,
                    "status": "completed",
                    "startedAt": 1,
                    "completedAt": 2,
                    "items": [{
                        "type": "agentMessage",
                        "id": message_id,
                        "text": message
                    }]
                }]
            }
        }));
        peer.expect_request(
            "thread/backgroundTerminals/list",
            json!({"threadId": "provider-root", "limit": 128}),
        )
        .respond(json!({"data": [], "nextCursor": null}));
    }
    peer.expect_request("shutdown", Value::Null)
        .respond(Value::Null);
    let peer_task = tokio::spawn(peer.run());

    runtime.start().await.expect("runtime starts");
    runtime.collect_events(2).await;
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "thread/started".to_owned(),
            params: json!({"thread": {"id": "provider-root"}}),
            emitted_at_ms: 1_000,
        })
        .expect("root notification");
    let first_reconciliation = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:0")
    })
    .await;

    assert!(
        first_reconciliation
            .activity
            .iter()
            .any(|mutation| matches!(
                mutation,
                ProviderActivityMutation::SetScope {
                    capabilities,
                    observation_state: ActivityObservationState::Live,
                } if capabilities.history_recovery
                    == bibcode_server::activity::ActivityHistoryRecovery::Bounded
            ))
    );
    assert!(
        !first_reconciliation
            .activity
            .iter()
            .any(|mutation| matches!(
                mutation,
                ProviderActivityMutation::SetScope { capabilities, .. }
                    if capabilities.history_recovery
                        == bibcode_server::activity::ActivityHistoryRecovery::Full
            ))
    );
    assert!(
        !first_reconciliation
            .activity
            .iter()
            .any(|mutation| matches!(
                mutation,
                ProviderActivityMutation::AppendEntry(entry)
                    if entry.detail.as_deref() == Some("Wrong thread history")
            ))
    );

    incoming_tx
        .send(IncomingEvent::Notification {
            method: "item/started".to_owned(),
            params: json!({
                "threadId": "provider-root",
                "turnId": "root-turn",
                "item": {
                    "id": "wait-for-correct-read",
                    "type": "collabAgentToolCall",
                    "tool": "wait",
                    "status": "completed",
                    "senderThreadId": "provider-root",
                    "receiverThreadIds": ["requested-child"],
                    "agentsStates": {}
                }
            }),
            emitted_at_ms: 1_001,
        })
        .expect("second reconciliation hint");
    let second_reconciliation = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:1")
    })
    .await;

    assert!(
        second_reconciliation
            .activity
            .iter()
            .any(|mutation| matches!(
                mutation,
                ProviderActivityMutation::SetScope {
                    capabilities,
                    observation_state: ActivityObservationState::Live,
                } if capabilities.history_recovery
                    == bibcode_server::activity::ActivityHistoryRecovery::Full
            ))
    );
    assert!(
        second_reconciliation
            .activity
            .iter()
            .any(|mutation| matches!(
                mutation,
                ProviderActivityMutation::AppendEntry(entry)
                    if entry.detail.as_deref() == Some("Correct thread history")
            ))
    );
    assert!(
        !second_reconciliation
            .activity
            .iter()
            .any(|mutation| matches!(
                mutation,
                ProviderActivityMutation::AppendEntry(entry)
                    if entry.detail.as_deref() == Some("Wrong thread history")
            ))
    );

    runtime.shutdown().await.expect("runtime shuts down");
    peer_task.await.expect("peer task");
}

#[tokio::test]
async fn reconciliation_read_method_not_found_degrades_history_to_bounded() {
    let (connection, _protocol_incoming, mut peer) = scripted_peer();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(
        CodexSessionOptions {
            version: "0.1.1".to_owned(),
            thread_id: "fixture-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: CodexRuntimeMode::FullAccess,
            model: Some("gpt-5.3-codex".to_owned()),
            service_tier: None,
            effort: None,
            resume_cursor: None,
        },
        connection,
        incoming_rx,
    );
    peer.expect_request("initialize", fixture("initialize-params.json"))
        .respond(json!({}));
    peer.expect_notification("initialized");
    peer.expect_request(
        "thread/start",
        json!({
            "cwd": "/tmp/project",
            "approvalPolicy": "never",
            "sandbox": "danger-full-access",
            "model": "gpt-5.3-codex",
            "serviceTier": null,
        }),
    )
    .respond(json!({
        "thread": {"id": "provider-root"},
        "cwd": "/tmp/project",
        "model": "gpt-5.3-codex"
    }));
    for pass in 0..2 {
        if pass == 0 {
            peer.expect_request(
                "thread/read",
                json!({"threadId": "provider-root", "includeTurns": false}),
            )
            .respond_error(json!({
                "code": -32601,
                "message": "Method not found",
                "data": null
            }));
        }
        peer.expect_request(
            "thread/list",
            json!({"ancestorThreadId": "provider-root", "limit": 50}),
        )
        .respond(json!({
            "data": [{
                "id": "child-schema",
                "parentThreadId": "provider-root",
                "createdAt": 1,
                "updatedAt": 2,
                "status": {"type": "idle"}
            }],
            "nextCursor": null,
            "backwardsCursor": null
        }));
        peer.expect_request(
            "thread/backgroundTerminals/list",
            json!({"threadId": "provider-root", "limit": 128}),
        )
        .respond(json!({"data": [], "nextCursor": null}));
    }
    peer.expect_request("shutdown", Value::Null)
        .respond(Value::Null);
    let peer_task = tokio::spawn(peer.run());

    runtime.start().await.expect("runtime starts");
    runtime.collect_events(2).await;
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "thread/started".to_owned(),
            params: json!({"thread": {"id": "provider-root"}}),
            emitted_at_ms: 1_000,
        })
        .expect("root notification");

    let (warning, first_reconciliation) = timeout(Duration::from_secs(2), async {
        let mut warning = None;
        let mut reconciliation = None;
        while warning.is_none() || reconciliation.is_none() {
            let event = runtime.next_event().await.expect("read downgrade event");
            if event.event_type == "runtime.warning" {
                warning = Some(event);
            } else if event.native_event_id.as_deref() == Some("codex:reconciliation:0") {
                reconciliation = Some(event);
            }
        }
        (warning.unwrap(), reconciliation.unwrap())
    })
    .await
    .expect("read downgrade events");
    assert_eq!(
        warning.payload["message"],
        "Codex activity method thread/read is unsupported"
    );
    assert!(
        first_reconciliation
            .activity
            .iter()
            .any(|mutation| matches!(
                mutation,
                ProviderActivityMutation::SetScope {
                    capabilities,
                    observation_state: ActivityObservationState::Live,
                } if capabilities.background_work
                    && capabilities.history_recovery
                        == bibcode_server::activity::ActivityHistoryRecovery::Bounded
            ))
    );
    assert!(
        first_reconciliation
            .activity
            .iter()
            .any(|mutation| matches!(
                mutation,
                ProviderActivityMutation::UpsertActor(actor)
                    if actor.id == "codex:thread:child-schema"
            ))
    );

    incoming_tx
        .send(IncomingEvent::Notification {
            method: "item/started".to_owned(),
            params: json!({
                "threadId": "provider-root",
                "turnId": "root-turn",
                "item": {
                    "id": "wait-after-read-downgrade",
                    "type": "collabAgentToolCall",
                    "tool": "wait",
                    "status": "completed",
                    "senderThreadId": "provider-root",
                    "receiverThreadIds": ["child-schema"],
                    "agentsStates": {}
                }
            }),
            emitted_at_ms: 1_001,
        })
        .expect("post-downgrade hint");
    let second_reconciliation = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:1")
    })
    .await;
    assert!(
        second_reconciliation
            .activity
            .iter()
            .any(|mutation| matches!(
                mutation,
                ProviderActivityMutation::SetScope { capabilities, .. }
                    if capabilities.history_recovery
                        == bibcode_server::activity::ActivityHistoryRecovery::Bounded
            ))
    );
    assert!(
        timeout(Duration::from_millis(150), runtime.next_event())
            .await
            .is_err(),
        "a missing thread/read warns once and skips future reads"
    );

    runtime.shutdown().await.expect("runtime shuts down");
    peer_task.await.expect("peer task");
}

/// #120: a successful `thread/read` one byte over the 512 KiB decoder bound, inflated by command
/// output the activity log never shows, is a large thread, not a missing method.
#[tokio::test]
async fn reconciliation_oversized_thread_read_keeps_the_method_supported_and_recovers() {
    let child_read = json!({
        "thread": {
            "id": "diagnostic-child",
            "parentThreadId": "provider-root",
            "createdAt": 1,
            "updatedAt": 2,
            "status": {"type": "idle"},
            "turns": [{
                "id": "fixture-turn",
                "status": "completed",
                "startedAt": 1,
                "completedAt": 2,
                "items": [{
                    "type": "commandExecution",
                    "id": "fixture-command",
                    "command": "echo fixture",
                    "status": "completed",
                    "aggregatedOutput": "x".repeat(523_955)
                }]
            }]
        }
    });
    assert_eq!(
        serde_json::to_vec(&child_read).expect("child read").len(),
        512 * 1024 + 1,
        "the reply must be one byte over the decoder bound"
    );
    let (connection, _protocol_incoming, mut peer) = scripted_peer();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(codex_activity_options(None), connection, incoming_rx);
    peer.expect_request("initialize", fixture("initialize-params.json"))
        .respond(json!({}));
    peer.expect_notification("initialized");
    peer.expect_request(
        "thread/start",
        json!({
            "cwd": "/tmp/project",
            "approvalPolicy": "never",
            "sandbox": "danger-full-access",
            "model": "gpt-5.3-codex",
            "serviceTier": null,
        }),
    )
    .respond(json!({
        "thread": {"id": "provider-root"},
        "cwd": "/tmp/project",
        "model": "gpt-5.3-codex"
    }));
    peer.expect_request(
        "thread/read",
        json!({"threadId": "provider-root", "includeTurns": false}),
    )
    .respond_history(empty_root_thread_read_result());
    peer.expect_request(
        "thread/list",
        json!({"ancestorThreadId": "provider-root", "limit": 50}),
    )
    .respond(json!({
        "data": [{
            "id": "diagnostic-child",
            "parentThreadId": "provider-root",
            "createdAt": 1,
            "updatedAt": 2,
            "status": {"type": "idle"}
        }],
        "nextCursor": null,
        "backwardsCursor": null
    }));
    peer.expect_request(
        "thread/read",
        json!({"threadId": "diagnostic-child", "includeTurns": false}),
    )
    .respond_history(child_read);
    peer.expect_request(
        "thread/backgroundTerminals/list",
        json!({"threadId": "provider-root", "limit": 128}),
    )
    .respond(json!({"data": [], "nextCursor": null}));
    peer.expect_request("shutdown", Value::Null)
        .respond(Value::Null);
    let peer_task = tokio::spawn(peer.run());

    runtime.start().await.expect("runtime starts");
    runtime.collect_events(2).await;
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "thread/started".to_owned(),
            params: json!({"thread": {"id": "provider-root"}}),
            emitted_at_ms: 1_000,
        })
        .expect("root notification");

    let mut warnings = Vec::new();
    let reconciliation = timeout(Duration::from_secs(5), async {
        loop {
            let event = runtime.next_event().await.expect("reconciliation event");
            if event.event_type == "runtime.warning" {
                warnings.push(event.payload["message"].clone());
            } else if event.native_event_id.as_deref() == Some("codex:reconciliation:0") {
                return event;
            }
        }
    })
    .await
    .expect("first reconciliation");
    assert!(
        warnings.is_empty(),
        "a successful thread/read must not be reported unsupported: {warnings:?}"
    );
    assert!(reconciliation.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::SetScope { capabilities, .. }
            if capabilities.history_recovery
                == bibcode_server::activity::ActivityHistoryRecovery::Full
    )));
    assert!(
        reconciliation.activity.iter().any(|mutation| matches!(
            mutation,
            ProviderActivityMutation::AppendEntry(entry)
                if entry.detail.as_deref() == Some("[redacted command output]")
        )),
        "the large thread's history is recovered"
    );

    runtime.shutdown().await.expect("runtime shuts down");
    peer_task.await.expect("peer task");
}

/// Answers a `thread/turns/list` request for `full_read_result`'s thread: a result or an error.
type TurnsPageServer = Box<dyn FnMut(&Value, &Value) -> Result<Value, Value> + Send>;

#[derive(Default)]
struct PagingPeerLog {
    requests: Vec<Value>,
    largest_reply_bytes: usize,
}

/// A Codex peer with one root and one listed child, `paging-child`, whose history is `child`.
/// Turn pages come from `serve_turns`; everything else is answered empty. It logs every request
/// and the largest reply it wrote.
fn spawn_paging_peer(
    peer: ScriptedPeer,
    child: Value,
    mut serve_turns: TurnsPageServer,
) -> (tokio::task::JoinHandle<()>, Arc<StdMutex<PagingPeerLog>>) {
    let log = Arc::new(StdMutex::new(PagingPeerLog::default()));
    let peer_log = log.clone();
    let task = tokio::spawn(async move {
        let mut reader = BufReader::new(peer.stdin);
        let mut writer = peer.stdout;
        let root = empty_root_thread_read_result();
        loop {
            let request = read_scripted_message(&mut reader, &mut writer).await;
            if request.get("id").is_none() {
                continue;
            }
            peer_log
                .lock()
                .expect("paging peer log")
                .requests
                .push(request.clone());
            let history_for = |request: &Value| match request["params"]["threadId"].as_str() {
                Some("provider-root") => &root,
                Some("paging-child") => &child,
                other => panic!("unexpected history target {other:?}"),
            };
            let reply = match request["method"].as_str().expect("request method") {
                "initialize" => Ok(json!({})),
                "thread/start" => Ok(json!({
                    "thread": {"id": "provider-root"},
                    "cwd": "/tmp/project",
                    "model": "gpt-5.3-codex"
                })),
                "thread/read" => {
                    assert_eq!(
                        request["params"]["includeTurns"], false,
                        "history never asks for full turns"
                    );
                    Ok(thread_history_metadata(history_for(&request)))
                }
                "thread/turns/list" => serve_turns(&request["params"], history_for(&request)),
                "thread/list" => Ok(json!({
                    "data": [{
                        "id": "paging-child",
                        "parentThreadId": "provider-root",
                        "createdAt": 1,
                        "updatedAt": 2,
                        "status": {"type": "idle"}
                    }],
                    "nextCursor": null,
                    "backwardsCursor": null
                })),
                "thread/backgroundTerminals/list" => Ok(json!({"data": [], "nextCursor": null})),
                "shutdown" => Ok(Value::Null),
                other => panic!("unexpected paging request {other}"),
            };
            let reply = match reply {
                Ok(result) => json!({"id": request["id"].clone(), "result": result}),
                Err(error) => json!({"id": request["id"].clone(), "error": error}),
            };
            let reply_bytes = serde_json::to_vec(&reply).expect("reply").len();
            {
                let mut log = peer_log.lock().expect("paging peer log");
                log.largest_reply_bytes = log.largest_reply_bytes.max(reply_bytes);
            }
            write_json(&mut writer, reply).await;
            if request["method"] == "shutdown" {
                break;
            }
        }
    });
    (task, log)
}

/// A child thread of `turn_count` completed turns; turn `n` says `message-n` and ran one
/// command printing `output_bytes` bytes.
fn paging_child(turn_count: usize, output_bytes: usize) -> Value {
    let output = "x".repeat(output_bytes);
    let turns = (0..turn_count)
        .map(|index| {
            json!({
                "id": format!("turn-{index}"),
                "status": "completed",
                "startedAt": 1,
                "completedAt": 2,
                "items": [
                    {
                        "type": "commandExecution",
                        "id": format!("command-{index}"),
                        "command": "make test",
                        "status": "completed",
                        "aggregatedOutput": output
                    },
                    {
                        "type": "agentMessage",
                        "id": format!("message-{index}"),
                        "text": format!("message-{index}")
                    }
                ]
            })
        })
        .collect::<Vec<_>>();
    json!({
        "thread": {
            "id": "paging-child",
            "parentThreadId": "provider-root",
            "createdAt": 1,
            "updatedAt": 2,
            "status": {"type": "idle"},
            "turns": turns
        }
    })
}

/// Starts the root, runs `passes` reconciliation passes (each later one hinted by a wait on the
/// child), and returns their events with every runtime warning.
async fn run_paging_passes(
    runtime: &CodexSessionRuntime,
    incoming_tx: &mpsc::UnboundedSender<IncomingEvent>,
    passes: usize,
) -> (Vec<RuntimeEvent>, Vec<Value>) {
    runtime.start().await.expect("runtime starts");
    runtime.collect_events(2).await;
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "thread/started".to_owned(),
            params: json!({"thread": {"id": "provider-root"}}),
            emitted_at_ms: 1_000,
        })
        .expect("root notification");
    let mut warnings = Vec::new();
    let mut reconciliations = Vec::new();
    for sequence in 0..passes {
        if sequence > 0 {
            incoming_tx
                .send(IncomingEvent::Notification {
                    method: "item/started".to_owned(),
                    params: json!({
                        "threadId": "provider-root",
                        "turnId": "root-turn",
                        "item": {
                            "id": format!("wait-{sequence}"),
                            "type": "collabAgentToolCall",
                            "tool": "wait",
                            "status": "completed",
                            "senderThreadId": "provider-root",
                            "receiverThreadIds": ["paging-child"],
                            "agentsStates": {}
                        }
                    }),
                    emitted_at_ms: 1_000 + sequence as u64,
                })
                .expect("reconciliation hint");
        }
        let native_event_id = format!("codex:reconciliation:{sequence}");
        let event = timeout(Duration::from_secs(10), async {
            loop {
                let event = runtime.next_event().await.expect("reconciliation event");
                if event.event_type == "runtime.warning" {
                    warnings.push(event.payload["message"].clone());
                } else if event.native_event_id.as_deref() == Some(native_event_id.as_str()) {
                    return event;
                }
            }
        })
        .await
        .expect("reconciliation");
        reconciliations.push(event);
    }
    (reconciliations, warnings)
}

fn appended_details(event: &RuntimeEvent) -> Vec<String> {
    event
        .activity
        .iter()
        .filter_map(|mutation| match mutation {
            ProviderActivityMutation::AppendEntry(entry) => entry.detail.clone(),
            _ => None,
        })
        .collect()
}

fn history_recovery(
    event: &RuntimeEvent,
) -> Option<bibcode_server::activity::ActivityHistoryRecovery> {
    event.activity.iter().find_map(|mutation| match mutation {
        ProviderActivityMutation::SetScope { capabilities, .. } => {
            Some(capabilities.history_recovery)
        }
        _ => None,
    })
}

fn turns_page_requests(log: &PagingPeerLog, thread_id: &str) -> Vec<Value> {
    log.requests
        .iter()
        .filter(|request| {
            request["method"] == "thread/turns/list" && request["params"]["threadId"] == thread_id
        })
        .map(|request| request["params"].clone())
        .collect()
}

/// A thread whose full history is far past the 8 MiB reply-line limit is recovered from one page
/// of its newest turns: no reply exceeds the 512 KiB decoder bound, and no request asks for the
/// whole history.
#[tokio::test]
async fn reconciliation_recovers_a_huge_thread_from_its_newest_page() {
    let child = paging_child(2_000, 10 * 1024);
    assert!(
        serde_json::to_vec(&child).expect("child").len() > 16 * 1024 * 1024,
        "the full history must exceed the transport's 8 MiB line limit"
    );
    let (connection, _protocol_incoming, peer) = scripted_peer();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(codex_activity_options(None), connection, incoming_rx);
    let (peer_task, log) = spawn_paging_peer(
        peer,
        child,
        Box::new(|params, history| Ok(thread_turns_page(params, history))),
    );

    let (reconciliations, warnings) = run_paging_passes(&runtime, &incoming_tx, 1).await;
    runtime.shutdown().await.expect("runtime shuts down");
    peer_task.await.expect("peer task");

    assert!(warnings.is_empty(), "{warnings:?}");
    let log = log.lock().expect("paging peer log");
    assert!(
        log.largest_reply_bytes < 512 * 1024,
        "largest reply was {} bytes",
        log.largest_reply_bytes
    );
    assert_eq!(
        turns_page_requests(&log, "paging-child"),
        [json!({
            "threadId": "paging-child",
            "limit": 20,
            "sortDirection": "desc",
            "itemsView": "full"
        })],
        "one page holds every turn activity keeps"
    );
    let details = appended_details(&reconciliations[0]);
    assert!(details.iter().any(|detail| detail == "message-1999"));
    assert!(details.iter().any(|detail| detail == "message-1980"));
    assert!(!details.iter().any(|detail| detail == "message-1979"));
    assert_eq!(
        history_recovery(&reconciliations[0]),
        Some(bibcode_server::activity::ActivityHistoryRecovery::Full)
    );
}

/// Codex may return fewer turns than asked for; paging continues until activity has its 20
/// turns and asks for nothing more.
#[tokio::test]
async fn reconciliation_pages_short_turn_pages_until_it_has_twenty_turns() {
    let (connection, _protocol_incoming, peer) = scripted_peer();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(codex_activity_options(None), connection, incoming_rx);
    let (peer_task, log) = spawn_paging_peer(
        peer,
        paging_child(30, 16),
        Box::new(|params, history| {
            let mut clamped = params.clone();
            clamped["limit"] = json!(7);
            Ok(thread_turns_page(&clamped, history))
        }),
    );

    let (reconciliations, warnings) = run_paging_passes(&runtime, &incoming_tx, 1).await;
    runtime.shutdown().await.expect("runtime shuts down");
    peer_task.await.expect("peer task");

    assert!(warnings.is_empty(), "{warnings:?}");
    let cursors = turns_page_requests(&log.lock().expect("paging peer log"), "paging-child")
        .into_iter()
        .map(|params| params["cursor"].clone())
        .collect::<Vec<_>>();
    assert_eq!(cursors, [Value::Null, json!("turn-23"), json!("turn-16")]);
    let details = appended_details(&reconciliations[0]);
    assert!(details.iter().any(|detail| detail == "message-10"));
    assert!(!details.iter().any(|detail| detail == "message-9"));
}

/// A cursor Codex already returned ends paging instead of looping.
#[tokio::test]
async fn reconciliation_stops_paging_at_a_repeated_cursor() {
    let (connection, _protocol_incoming, peer) = scripted_peer();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(codex_activity_options(None), connection, incoming_rx);
    let (peer_task, log) = spawn_paging_peer(
        peer,
        paging_child(30, 16),
        Box::new(|params, history| {
            if params["threadId"] == "provider-root" {
                return Ok(thread_turns_page(params, history));
            }
            let mut clamped = params.clone();
            clamped["limit"] = json!(5);
            clamped.as_object_mut().expect("params").remove("cursor");
            let mut page = thread_turns_page(&clamped, history);
            page["nextCursor"] = json!("same-cursor");
            Ok(page)
        }),
    );

    let (_, warnings) = run_paging_passes(&runtime, &incoming_tx, 1).await;
    runtime.shutdown().await.expect("runtime shuts down");
    peer_task.await.expect("peer task");

    assert!(warnings.is_empty(), "{warnings:?}");
    let cursors = turns_page_requests(&log.lock().expect("paging peer log"), "paging-child")
        .into_iter()
        .map(|params| params["cursor"].clone())
        .collect::<Vec<_>>();
    assert_eq!(cursors, [Value::Null, json!("same-cursor")]);
}

/// A Codex without `thread/turns/list` cannot recover history: the read downgrades once, with a
/// warning naming that method, and later passes read no history.
#[tokio::test]
async fn reconciliation_missing_turns_list_downgrades_history_reads() {
    let (connection, _protocol_incoming, peer) = scripted_peer();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(codex_activity_options(None), connection, incoming_rx);
    let (peer_task, log) = spawn_paging_peer(
        peer,
        paging_child(3, 16),
        Box::new(|_, _| {
            Err(json!({
                "code": -32600,
                "message": "Invalid request: unknown variant `thread/turns/list`, expected one of `initialize`"
            }))
        }),
    );

    let (reconciliations, warnings) = run_paging_passes(&runtime, &incoming_tx, 2).await;
    runtime.shutdown().await.expect("runtime shuts down");
    peer_task.await.expect("peer task");

    assert_eq!(
        warnings,
        [json!(
            "Codex activity method thread/turns/list is unsupported"
        )]
    );
    assert_eq!(
        history_recovery(&reconciliations[1]),
        Some(bibcode_server::activity::ActivityHistoryRecovery::Bounded)
    );
    let history_requests = log
        .lock()
        .expect("paging peer log")
        .requests
        .iter()
        .filter(|request| {
            request["method"] == "thread/read" || request["method"] == "thread/turns/list"
        })
        .count();
    assert_eq!(history_requests, 2, "only the first root read is attempted");
}

/// A turns page that does not decode is a failed read of that thread: no downgrade, no warning,
/// and the next pass recovers the thread.
#[tokio::test]
async fn reconciliation_undecodable_turns_page_is_retried() {
    let (connection, _protocol_incoming, peer) = scripted_peer();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(codex_activity_options(None), connection, incoming_rx);
    let mut child_pages = 0;
    let (peer_task, _log) = spawn_paging_peer(
        peer,
        paging_child(3, 16),
        Box::new(move |params, history| {
            if params["threadId"] == "paging-child" {
                child_pages += 1;
                if child_pages == 1 {
                    return Ok(json!({"data": "not turns", "nextCursor": null}));
                }
            }
            Ok(thread_turns_page(params, history))
        }),
    );

    let (reconciliations, warnings) = run_paging_passes(&runtime, &incoming_tx, 2).await;
    runtime.shutdown().await.expect("runtime shuts down");
    peer_task.await.expect("peer task");

    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(!appended_details(&reconciliations[0]).contains(&"message-2".to_owned()));
    assert!(appended_details(&reconciliations[1]).contains(&"message-2".to_owned()));
}

/// A `thread/read` reply that fails to decode is a failed read of that thread, not a missing
/// method: the pass reads the remaining threads, warns nobody, and the next pass reads again.
#[tokio::test]
async fn reconciliation_undecodable_thread_read_is_retried_instead_of_downgrading() {
    let (connection, _protocol_incoming, mut peer) = scripted_peer();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(codex_activity_options(None), connection, incoming_rx);
    let child_read = |thread_id: &str, message: &str| {
        json!({
            "thread": {
                "id": thread_id,
                "parentThreadId": "provider-root",
                "createdAt": 1,
                "updatedAt": 2,
                "status": {"type": "idle"},
                "turns": [{
                    "id": format!("{thread_id}-turn"),
                    "status": "completed",
                    "startedAt": 1,
                    "completedAt": 2,
                    "items": [{
                        "type": "agentMessage",
                        "id": format!("{thread_id}-message"),
                        "text": message
                    }]
                }]
            }
        })
    };
    let children = ["child-a", "child-b"].map(|id| {
        json!({
            "id": id,
            "parentThreadId": "provider-root",
            "createdAt": 1,
            "updatedAt": 2,
            "status": {"type": "idle"}
        })
    });
    let listed_children = json!({
        "data": children,
        "nextCursor": null,
        "backwardsCursor": null
    });
    peer.expect_request("initialize", fixture("initialize-params.json"))
        .respond(json!({}));
    peer.expect_notification("initialized");
    peer.expect_request(
        "thread/start",
        json!({
            "cwd": "/tmp/project",
            "approvalPolicy": "never",
            "sandbox": "danger-full-access",
            "model": "gpt-5.3-codex",
            "serviceTier": null,
        }),
    )
    .respond(json!({
        "thread": {"id": "provider-root"},
        "cwd": "/tmp/project",
        "model": "gpt-5.3-codex"
    }));
    peer.expect_request(
        "thread/read",
        json!({"threadId": "provider-root", "includeTurns": false}),
    )
    .respond_history(json!({"unexpected": "schema"}));
    peer.expect_request(
        "thread/list",
        json!({"ancestorThreadId": "provider-root", "limit": 50}),
    )
    .respond(listed_children.clone());
    peer.expect_request(
        "thread/read",
        json!({"threadId": "child-a", "includeTurns": false}),
    )
    .respond_history(json!({"unexpected": "schema"}));
    peer.expect_request(
        "thread/read",
        json!({"threadId": "child-b", "includeTurns": false}),
    )
    .respond_history(child_read("child-b", "Child B history"));
    peer.expect_request(
        "thread/backgroundTerminals/list",
        json!({"threadId": "provider-root", "limit": 128}),
    )
    .respond(json!({"data": [], "nextCursor": null}));
    peer.expect_request(
        "thread/read",
        json!({"threadId": "provider-root", "includeTurns": false}),
    )
    .respond_history(empty_root_thread_read_result());
    peer.expect_request(
        "thread/list",
        json!({"ancestorThreadId": "provider-root", "limit": 50}),
    )
    .respond(listed_children);
    peer.expect_request(
        "thread/read",
        json!({"threadId": "child-a", "includeTurns": false}),
    )
    .respond_history(child_read("child-a", "Child A history"));
    peer.expect_request(
        "thread/backgroundTerminals/list",
        json!({"threadId": "provider-root", "limit": 128}),
    )
    .respond(json!({"data": [], "nextCursor": null}));
    peer.expect_request("shutdown", Value::Null)
        .respond(Value::Null);
    let peer_task = tokio::spawn(peer.run());

    runtime.start().await.expect("runtime starts");
    runtime.collect_events(2).await;
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "thread/started".to_owned(),
            params: json!({"thread": {"id": "provider-root"}}),
            emitted_at_ms: 1_000,
        })
        .expect("root notification");
    let mut warnings = Vec::new();
    let mut reconciliation_events = Vec::new();
    for sequence in 0..2 {
        if sequence == 1 {
            incoming_tx
                .send(IncomingEvent::Notification {
                    method: "item/started".to_owned(),
                    params: json!({
                        "threadId": "provider-root",
                        "turnId": "root-turn",
                        "item": {
                            "id": "wait-for-child-a",
                            "type": "collabAgentToolCall",
                            "tool": "wait",
                            "status": "completed",
                            "senderThreadId": "provider-root",
                            "receiverThreadIds": ["child-a"],
                            "agentsStates": {}
                        }
                    }),
                    emitted_at_ms: 1_001,
                })
                .expect("second reconciliation hint");
        }
        let native_event_id = format!("codex:reconciliation:{sequence}");
        let event = timeout(Duration::from_secs(5), async {
            loop {
                let event = runtime.next_event().await.expect("reconciliation event");
                if event.event_type == "runtime.warning" {
                    warnings.push(event.payload["message"].clone());
                } else if event.native_event_id.as_deref() == Some(native_event_id.as_str()) {
                    return event;
                }
            }
        })
        .await
        .expect("reconciliation");
        reconciliation_events.push(event);
    }
    assert!(warnings.is_empty(), "no method downgrade: {warnings:?}");
    let has_entry = |event: &RuntimeEvent, detail: &str| {
        event.activity.iter().any(|mutation| {
            matches!(
                mutation,
                ProviderActivityMutation::AppendEntry(entry)
                    if entry.detail.as_deref() == Some(detail)
            )
        })
    };
    assert!(
        has_entry(&reconciliation_events[0], "Child B history"),
        "an undecodable thread does not stop the pass reading the next one"
    );
    assert!(
        has_entry(&reconciliation_events[1], "Child A history"),
        "the next pass reads the thread that failed to decode"
    );

    runtime.shutdown().await.expect("runtime shuts down");
    peer_task.await.expect("peer task");
}

#[tokio::test]
async fn reconciliation_list_method_not_found_keeps_bounded_root_read_recovery() {
    let (connection, _protocol_incoming, mut peer) = scripted_peer();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(
        CodexSessionOptions {
            version: "0.1.1".to_owned(),
            thread_id: "fixture-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: CodexRuntimeMode::FullAccess,
            model: Some("gpt-5.3-codex".to_owned()),
            service_tier: None,
            effort: None,
            resume_cursor: None,
        },
        connection,
        incoming_rx,
    );
    peer.expect_request("initialize", fixture("initialize-params.json"))
        .respond(json!({}));
    peer.expect_notification("initialized");
    peer.expect_request(
        "thread/start",
        json!({
            "cwd": "/tmp/project",
            "approvalPolicy": "never",
            "sandbox": "danger-full-access",
            "model": "gpt-5.3-codex",
            "serviceTier": null,
        }),
    )
    .respond(json!({
        "thread": {"id": "provider-root"},
        "cwd": "/tmp/project",
        "model": "gpt-5.3-codex"
    }));
    peer.expect_request(
        "thread/read",
        json!({"threadId": "provider-root", "includeTurns": false}),
    )
    .respond_history(empty_root_thread_read_result());
    peer.expect_request(
        "thread/list",
        json!({"ancestorThreadId": "provider-root", "limit": 50}),
    )
    .respond_error(json!({
        "code": -32601,
        "message": "Method not found",
        "data": null
    }));
    peer.expect_request(
        "thread/backgroundTerminals/list",
        json!({"threadId": "provider-root", "limit": 128}),
    )
    .respond(json!({"data": [], "nextCursor": null}));
    peer.expect_request(
        "thread/read",
        json!({"threadId": "provider-root", "includeTurns": false}),
    )
    .respond_history(empty_root_thread_read_result());
    peer.expect_request(
        "thread/backgroundTerminals/list",
        json!({"threadId": "provider-root", "limit": 128}),
    )
    .respond(json!({"data": [], "nextCursor": null}));
    peer.expect_request("shutdown", Value::Null)
        .respond(Value::Null);
    let peer_task = tokio::spawn(peer.run());

    runtime.start().await.expect("runtime starts");
    runtime.collect_events(2).await;
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "thread/started".to_owned(),
            params: json!({"thread": {"id": "provider-root"}}),
            emitted_at_ms: 1_000,
        })
        .expect("root notification");

    let (warning, first_reconciliation) = timeout(Duration::from_secs(2), async {
        let mut warning = None;
        let mut reconciliation = None;
        while warning.is_none() || reconciliation.is_none() {
            let event = runtime.next_event().await.expect("list downgrade event");
            if event.event_type == "runtime.warning" {
                warning = Some(event);
            } else if event.native_event_id.as_deref() == Some("codex:reconciliation:0") {
                reconciliation = Some(event);
            }
        }
        (warning.unwrap(), reconciliation.unwrap())
    })
    .await
    .expect("list downgrade events");
    assert!(
        warning.payload["message"]
            .as_str()
            .is_some_and(|message| message.contains("thread/list"))
    );
    assert!(
        first_reconciliation
            .activity
            .iter()
            .any(|mutation| matches!(
                mutation,
                ProviderActivityMutation::SetScope {
                    capabilities,
                    observation_state: ActivityObservationState::Live,
                } if capabilities.actors
                    && capabilities.attributed_activity
                    && capabilities.background_work
                    && capabilities.history_recovery
                        == bibcode_server::activity::ActivityHistoryRecovery::Bounded
            ))
    );

    incoming_tx
        .send(IncomingEvent::Notification {
            method: "item/started".to_owned(),
            params: json!({
                "threadId": "provider-root",
                "turnId": "root-turn",
                "item": {
                    "id": "spawn-after-list-downgrade",
                    "type": "collabAgentToolCall",
                    "tool": "spawnAgent",
                    "status": "inProgress",
                    "senderThreadId": "provider-root",
                    "receiverThreadIds": ["child-after-list-downgrade"],
                    "agentsStates": {}
                }
            }),
            emitted_at_ms: 1_001,
        })
        .expect("post-downgrade hint");
    let second_reconciliation = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:1")
    })
    .await;
    assert!(
        second_reconciliation
            .activity
            .iter()
            .any(|mutation| matches!(
                mutation,
                ProviderActivityMutation::SetScope { capabilities, .. }
                    if capabilities.history_recovery
                        == bibcode_server::activity::ActivityHistoryRecovery::Bounded
            ))
    );
    assert!(
        timeout(Duration::from_millis(150), runtime.next_event())
            .await
            .is_err(),
        "list incompatibility warns only once"
    );

    runtime.shutdown().await.expect("runtime shuts down");
    peer_task.await.expect("peer task");
}

#[tokio::test]
async fn reconciliation_reconnect_cancels_old_pass_and_runs_one_immediate_repair() {
    let (connection_a, _protocol_incoming_a, peer_a) = scripted_peer();
    let (incoming_tx_a, incoming_rx_a) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(
        CodexSessionOptions {
            version: "0.1.1".to_owned(),
            thread_id: "fixture-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: CodexRuntimeMode::FullAccess,
            model: Some("gpt-5.3-codex".to_owned()),
            service_tier: None,
            effort: None,
            resume_cursor: None,
        },
        connection_a,
        incoming_rx_a,
    );
    let (old_list_tx, old_list_rx) = oneshot::channel();
    let old_peer_task = tokio::spawn(async move {
        let mut reader = BufReader::new(peer_a.stdin);
        let mut writer = peer_a.stdout;
        let initialize = read_scripted_message(&mut reader, &mut writer).await;
        write_json(
            &mut writer,
            json!({"id": initialize["id"].clone(), "result": {}}),
        )
        .await;
        assert_eq!(
            read_scripted_message(&mut reader, &mut writer).await["method"],
            "initialized"
        );
        let start = read_scripted_message(&mut reader, &mut writer).await;
        write_json(
            &mut writer,
            json!({
                "id": start["id"].clone(),
                "result": {
                    "thread": {"id": "provider-root"},
                    "cwd": "/tmp/project",
                    "model": "gpt-5.3-codex"
                }
            }),
        )
        .await;
        let root_read = read_scripted_message(&mut reader, &mut writer).await;
        let _ = old_list_tx.send(root_read);
        std::future::pending::<()>().await;
    });

    runtime.start().await.expect("initial runtime starts");
    runtime.collect_events(2).await;
    incoming_tx_a
        .send(IncomingEvent::Notification {
            method: "thread/started".to_owned(),
            params: json!({"thread": {"id": "provider-root"}}),
            emitted_at_ms: 1_000,
        })
        .expect("initial root notification");
    let old_list = timeout(Duration::from_secs(1), old_list_rx)
        .await
        .expect("old pass starts")
        .expect("old list request");
    assert_eq!(old_list["method"], "thread/read");
    assert_eq!(
        old_list["params"],
        json!({"threadId": "provider-root", "includeTurns": false})
    );

    let (connection_b, _protocol_incoming_b, mut peer_b) = scripted_peer();
    let (_incoming_tx_b, incoming_rx_b) = mpsc::unbounded_channel();
    peer_b
        .expect_request("initialize", fixture("initialize-params.json"))
        .respond(json!({}));
    peer_b.expect_notification("initialized");
    peer_b
        .expect_request(
            "thread/resume",
            json!({
                "threadId": "provider-root",
                "cwd": "/tmp/project",
                "approvalPolicy": "never",
                "sandbox": "danger-full-access",
                "model": "gpt-5.3-codex",
                "serviceTier": null,
            }),
        )
        .respond(json!({
            "thread": {"id": "provider-root"},
            "cwd": "/tmp/project",
            "model": "gpt-5.3-codex"
        }));
    peer_b
        .expect_request(
            "thread/read",
            json!({"threadId": "provider-root", "includeTurns": false}),
        )
        .respond_history(empty_root_thread_read_result());
    peer_b
        .expect_request(
            "thread/list",
            json!({"ancestorThreadId": "provider-root", "limit": 50}),
        )
        .respond(json!({
            "data": [],
            "nextCursor": null,
            "backwardsCursor": null
        }));
    peer_b
        .expect_request(
            "thread/backgroundTerminals/list",
            json!({"threadId": "provider-root", "limit": 128}),
        )
        .respond(json!({"data": [], "nextCursor": null}));
    peer_b
        .expect_request("shutdown", Value::Null)
        .respond(Value::Null);
    let new_peer_task = tokio::spawn(peer_b.run());

    runtime
        .reconnect(connection_b, incoming_rx_b)
        .await
        .expect("runtime reconnects");
    runtime.collect_events(2).await;
    let reconciliation = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:0")
    })
    .await;
    assert!(reconciliation.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::SetScope {
            capabilities,
            observation_state: ActivityObservationState::Live,
        } if capabilities.history_recovery
                == bibcode_server::activity::ActivityHistoryRecovery::Full
            && capabilities.background_work
    )));

    runtime.shutdown().await.expect("runtime shuts down");
    new_peer_task.await.expect("new peer task");
    old_peer_task.abort();
    let _ = old_peer_task.await;
}

#[tokio::test]
async fn reconciliation_initial_transport_failure_publishes_bounded_history() {
    let (connection, _protocol_incoming, peer) = scripted_peer();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(
        codex_activity_options(Some("provider-root")),
        connection,
        incoming_rx,
    );
    let peer_task = tokio::spawn(async move {
        let mut reader = BufReader::new(peer.stdin);
        let mut writer = peer.stdout;
        let initialize = read_scripted_message(&mut reader, &mut writer).await;
        assert_eq!(initialize["method"], "initialize");
        write_json(
            &mut writer,
            json!({"id": initialize["id"].clone(), "result": {}}),
        )
        .await;
        assert_eq!(
            read_scripted_message(&mut reader, &mut writer).await["method"],
            "initialized"
        );
        let resume = read_scripted_message(&mut reader, &mut writer).await;
        assert_eq!(resume["method"], "thread/resume");
        write_json(
            &mut writer,
            json!({
                "id": resume["id"].clone(),
                "result": {
                    "thread": {"id": "provider-root"},
                    "cwd": "/tmp/project",
                    "model": "gpt-5.3-codex"
                }
            }),
        )
        .await;
        let root_read = read_scripted_message(&mut reader, &mut writer).await;
        assert_eq!(root_read["method"], "thread/read");
        assert_eq!(
            root_read["params"],
            json!({"threadId": "provider-root", "includeTurns": false})
        );
        drop(writer);
    });

    runtime.start().await.expect("runtime starts");
    runtime.collect_events(2).await;
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "thread/started".to_owned(),
            params: json!({"thread": {"id": "provider-root"}}),
            emitted_at_ms: 1_000,
        })
        .expect("root notification");
    let stale = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:0")
    })
    .await;
    assert!(stale.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::SetScope {
            capabilities,
            observation_state: ActivityObservationState::Stale,
        } if capabilities.history_recovery
            == bibcode_server::activity::ActivityHistoryRecovery::Bounded
    )));

    peer_task.await.expect("peer task");
    let _ = runtime.shutdown().await;
}

#[tokio::test]
async fn reconciliation_read_downgrade_before_transport_failure_publishes_bounded_history() {
    let (connection, _protocol_incoming, peer) = scripted_peer();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(
        codex_activity_options(Some("provider-root")),
        connection,
        incoming_rx,
    );
    let peer_task = tokio::spawn(async move {
        let mut reader = BufReader::new(peer.stdin);
        let mut writer = peer.stdout;
        let initialize = read_scripted_message(&mut reader, &mut writer).await;
        write_json(
            &mut writer,
            json!({"id": initialize["id"].clone(), "result": {}}),
        )
        .await;
        assert_eq!(
            read_scripted_message(&mut reader, &mut writer).await["method"],
            "initialized"
        );
        let resume = read_scripted_message(&mut reader, &mut writer).await;
        write_json(
            &mut writer,
            json!({
                "id": resume["id"].clone(),
                "result": {
                    "thread": {"id": "provider-root"},
                    "cwd": "/tmp/project",
                    "model": "gpt-5.3-codex"
                }
            }),
        )
        .await;
        let root_read = read_scripted_message(&mut reader, &mut writer).await;
        assert_eq!(root_read["method"], "thread/read");
        write_json(
            &mut writer,
            json!({
                "id": root_read["id"].clone(),
                "result": empty_root_thread_read_result()
            }),
        )
        .await;
        serve_thread_turns_pages(&mut reader, &mut writer, &empty_root_thread_read_result()).await;
        let list = read_scripted_message(&mut reader, &mut writer).await;
        assert_eq!(list["method"], "thread/list");
        write_json(
            &mut writer,
            json!({"id": list["id"].clone(), "result": empty_thread_list_result()}),
        )
        .await;
        let background = read_scripted_message(&mut reader, &mut writer).await;
        assert_eq!(background["method"], "thread/backgroundTerminals/list");
        write_json(
            &mut writer,
            json!({
                "id": background["id"].clone(),
                "result": empty_background_terminal_list_result()
            }),
        )
        .await;

        let incompatible_read = read_scripted_message(&mut reader, &mut writer).await;
        assert_eq!(incompatible_read["method"], "thread/read");
        write_json(
            &mut writer,
            json!({
                "id": incompatible_read["id"].clone(),
                "error": {"code": -32601, "message": "Method not found", "data": null}
            }),
        )
        .await;
        let retry_list = read_scripted_message(&mut reader, &mut writer).await;
        assert_eq!(retry_list["method"], "thread/list");
        write_json(
            &mut writer,
            json!({"id": retry_list["id"].clone(), "result": empty_thread_list_result()}),
        )
        .await;
        let failed_background = read_scripted_message(&mut reader, &mut writer).await;
        assert_eq!(
            failed_background["method"],
            "thread/backgroundTerminals/list"
        );
        drop(writer);
    });

    runtime.start().await.expect("runtime starts");
    runtime.collect_events(2).await;
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "thread/started".to_owned(),
            params: json!({"thread": {"id": "provider-root"}}),
            emitted_at_ms: 1_000,
        })
        .expect("root notification");
    let initial = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:0")
    })
    .await;
    assert!(initial.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::SetScope { capabilities, .. }
            if capabilities.history_recovery
                == bibcode_server::activity::ActivityHistoryRecovery::Full
    )));

    incoming_tx
        .send(IncomingEvent::Notification {
            method: "item/started".to_owned(),
            params: json!({
                "threadId": "provider-root",
                "turnId": "root-turn",
                "item": {
                    "id": "capability-retry-hint",
                    "type": "collabAgentToolCall",
                    "tool": "wait",
                    "status": "completed",
                    "senderThreadId": "provider-root",
                    "receiverThreadIds": ["capability-child"],
                    "agentsStates": {}
                }
            }),
            emitted_at_ms: 1_001,
        })
        .expect("follow-up reconciliation hint");
    let stale = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:1")
    })
    .await;
    assert!(stale.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::SetScope {
            capabilities,
            observation_state: ActivityObservationState::Stale,
        } if capabilities.history_recovery
            == bibcode_server::activity::ActivityHistoryRecovery::Bounded
    )));

    peer_task.await.expect("peer task");
    let _ = runtime.shutdown().await;
}

#[tokio::test]
async fn reconciliation_transport_loss_marks_stale_and_preserves_capabilities_until_reconnect() {
    let (connection_a, _protocol_incoming_a, peer_a) = scripted_peer();
    let (incoming_tx_a, incoming_rx_a) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(
        CodexSessionOptions {
            version: "0.1.1".to_owned(),
            thread_id: "fixture-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: CodexRuntimeMode::FullAccess,
            model: Some("gpt-5.3-codex".to_owned()),
            service_tier: None,
            effort: None,
            resume_cursor: None,
        },
        connection_a,
        incoming_rx_a,
    );
    let old_peer_task = tokio::spawn(async move {
        let mut reader = BufReader::new(peer_a.stdin);
        let mut writer = peer_a.stdout;
        let initialize = read_scripted_message(&mut reader, &mut writer).await;
        write_json(
            &mut writer,
            json!({"id": initialize["id"].clone(), "result": {}}),
        )
        .await;
        assert_eq!(
            read_scripted_message(&mut reader, &mut writer).await["method"],
            "initialized"
        );
        let start = read_scripted_message(&mut reader, &mut writer).await;
        write_json(
            &mut writer,
            json!({
                "id": start["id"].clone(),
                "result": {
                    "thread": {"id": "provider-root"},
                    "cwd": "/tmp/project",
                    "model": "gpt-5.3-codex"
                }
            }),
        )
        .await;
        let root_read = read_scripted_message(&mut reader, &mut writer).await;
        assert_eq!(root_read["method"], "thread/read");
        assert_eq!(
            root_read["params"],
            json!({"threadId": "provider-root", "includeTurns": false})
        );
        write_json(
            &mut writer,
            json!({
                "id": root_read["id"].clone(),
                "result": empty_root_thread_read_result()
            }),
        )
        .await;
        serve_thread_turns_pages(&mut reader, &mut writer, &empty_root_thread_read_result()).await;
        let first_list = read_scripted_message(&mut reader, &mut writer).await;
        assert_eq!(first_list["method"], "thread/list");
        write_json(
            &mut writer,
            json!({
                "id": first_list["id"].clone(),
                "result": {
                    "data": [],
                    "nextCursor": null,
                    "backwardsCursor": null
                }
            }),
        )
        .await;
        let background = read_scripted_message(&mut reader, &mut writer).await;
        assert_eq!(background["method"], "thread/backgroundTerminals/list");
        write_json(
            &mut writer,
            json!({
                "id": background["id"].clone(),
                "result": {"data": [], "nextCursor": null}
            }),
        )
        .await;
        let retry_root_read = read_scripted_message(&mut reader, &mut writer).await;
        assert_eq!(retry_root_read["method"], "thread/read");
        assert_eq!(
            retry_root_read["params"],
            json!({"threadId": "provider-root", "includeTurns": false})
        );
        write_json(
            &mut writer,
            json!({
                "id": retry_root_read["id"].clone(),
                "result": empty_root_thread_read_result()
            }),
        )
        .await;
        serve_thread_turns_pages(&mut reader, &mut writer, &empty_root_thread_read_result()).await;
        let retry_list = read_scripted_message(&mut reader, &mut writer).await;
        assert_eq!(retry_list["method"], "thread/list");
        drop(writer);
    });

    runtime.start().await.expect("initial runtime starts");
    runtime.collect_events(2).await;
    incoming_tx_a
        .send(IncomingEvent::Notification {
            method: "thread/started".to_owned(),
            params: json!({"thread": {"id": "provider-root"}}),
            emitted_at_ms: 1_000,
        })
        .expect("initial root notification");
    let initial_reconciliation = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:0")
    })
    .await;
    assert!(
        initial_reconciliation
            .activity
            .iter()
            .any(|mutation| matches!(
                mutation,
                ProviderActivityMutation::SetScope { capabilities, .. }
                    if capabilities.background_work
                        && capabilities.history_recovery
                            == bibcode_server::activity::ActivityHistoryRecovery::Full
            ))
    );

    incoming_tx_a
        .send(IncomingEvent::Notification {
            method: "item/started".to_owned(),
            params: json!({
                "threadId": "provider-root",
                "turnId": "root-turn",
                "item": {
                    "id": "transport-retry-hint",
                    "type": "collabAgentToolCall",
                    "tool": "wait",
                    "status": "completed",
                    "senderThreadId": "provider-root",
                    "receiverThreadIds": ["transport-child"],
                    "agentsStates": {}
                }
            }),
            emitted_at_ms: 1_001,
        })
        .expect("transport retry hint");
    let stale = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:1")
    })
    .await;
    assert!(stale.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::SetScope {
            capabilities,
            observation_state: ActivityObservationState::Stale,
        } if capabilities.background_work
            && capabilities.history_recovery
                == bibcode_server::activity::ActivityHistoryRecovery::Full
    )));
    assert!(!stale.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::AppendEntry(entry)
            if entry.kind == bibcode_server::activity::ActivityEntryKind::Error
    )));
    old_peer_task.await.expect("old peer task");

    let (connection_b, _protocol_incoming_b, mut peer_b) = scripted_peer();
    let (_incoming_tx_b, incoming_rx_b) = mpsc::unbounded_channel();
    peer_b
        .expect_request("initialize", fixture("initialize-params.json"))
        .respond(json!({}));
    peer_b.expect_notification("initialized");
    peer_b
        .expect_request(
            "thread/resume",
            json!({
                "threadId": "provider-root",
                "cwd": "/tmp/project",
                "approvalPolicy": "never",
                "sandbox": "danger-full-access",
                "model": "gpt-5.3-codex",
                "serviceTier": null,
            }),
        )
        .respond(json!({
            "thread": {"id": "provider-root"},
            "cwd": "/tmp/project",
            "model": "gpt-5.3-codex"
        }));
    peer_b
        .expect_request(
            "thread/read",
            json!({"threadId": "provider-root", "includeTurns": false}),
        )
        .respond_history(empty_root_thread_read_result());
    peer_b
        .expect_request(
            "thread/list",
            json!({"ancestorThreadId": "provider-root", "limit": 50}),
        )
        .respond(json!({
            "data": [],
            "nextCursor": null,
            "backwardsCursor": null
        }));
    peer_b
        .expect_request(
            "thread/backgroundTerminals/list",
            json!({"threadId": "provider-root", "limit": 128}),
        )
        .respond(json!({"data": [], "nextCursor": null}));
    peer_b
        .expect_request("shutdown", Value::Null)
        .respond(Value::Null);
    let new_peer_task = tokio::spawn(peer_b.run());
    runtime
        .reconnect(connection_b, incoming_rx_b)
        .await
        .expect("runtime reconnects");
    runtime.collect_events(2).await;
    let repaired = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:2")
    })
    .await;
    assert!(repaired.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::SetScope {
            observation_state: ActivityObservationState::Live,
            ..
        }
    )));

    runtime.shutdown().await.expect("runtime shuts down");
    new_peer_task.await.expect("new peer task");
}

#[tokio::test]
async fn reconciliation_bounds_final_batch_and_retains_newest_multi_child_history() {
    let (connection, _protocol_incoming, mut peer) = scripted_peer();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(
        CodexSessionOptions {
            version: "0.1.1".to_owned(),
            thread_id: "fixture-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: CodexRuntimeMode::FullAccess,
            model: Some("gpt-5.3-codex".to_owned()),
            service_tier: None,
            effort: None,
            resume_cursor: None,
        },
        connection,
        incoming_rx,
    );
    peer.expect_request("initialize", fixture("initialize-params.json"))
        .respond(json!({}));
    peer.expect_notification("initialized");
    peer.expect_request(
        "thread/start",
        json!({
            "cwd": "/tmp/project",
            "approvalPolicy": "never",
            "sandbox": "danger-full-access",
            "model": "gpt-5.3-codex",
            "serviceTier": null,
        }),
    )
    .respond(json!({
        "thread": {"id": "provider-root"},
        "cwd": "/tmp/project",
        "model": "gpt-5.3-codex"
    }));
    peer.expect_request(
        "thread/read",
        json!({"threadId": "provider-root", "includeTurns": false}),
    )
    .respond_history(empty_root_thread_read_result());
    peer.expect_request(
        "thread/list",
        json!({"ancestorThreadId": "provider-root", "limit": 50}),
    )
    .respond(json!({
        "data": [],
        "nextCursor": "page-2",
        "backwardsCursor": null
    }));
    let listed_threads = (0..51)
        .map(|index| {
            json!({
                "id": format!("budget-child-{index}"),
                "parentThreadId": "provider-root",
                "createdAt": 1,
                "updatedAt": if index == 1 { 100 } else { 2 },
                "status": {"type": "idle"}
            })
        })
        .collect::<Vec<_>>();
    peer.expect_request(
        "thread/list",
        json!({
            "ancestorThreadId": "provider-root",
            "limit": 50,
            "cursor": "page-2"
        }),
    )
    .respond(json!({
        "data": listed_threads,
        "nextCursor": null,
        "backwardsCursor": null
    }));
    let turns = (0..21)
        .map(|turn_index| {
            json!({
                "id": format!("budget-turn-{turn_index}"),
                "status": "completed",
                "startedAt": turn_index + 1,
                "completedAt": turn_index + 1,
                "items": (0..11)
                    .map(|item_index| {
                        json!({
                            "type": "agentMessage",
                            "id": format!("budget-message-{turn_index}-{item_index}"),
                            "text": format!("turn {turn_index} item {item_index}")
                        })
                    })
                    .collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    for index in 0..50 {
        let thread_id = format!("budget-child-{index}");
        let thread_turns = if index == 0 {
            turns.clone()
        } else if index == 1 {
            vec![json!({
                "id": "newest-secondary-turn",
                "status": "completed",
                "startedAt": 99,
                "completedAt": 100,
                "items": [{
                    "type": "agentMessage",
                    "id": "newest-secondary-message",
                    "text": "newest secondary history"
                }]
            })]
        } else {
            Vec::new()
        };
        peer.expect_request(
            "thread/read",
            json!({"threadId": thread_id, "includeTurns": false}),
        )
        .respond_history(json!({
            "thread": {
                "id": thread_id,
                "parentThreadId": "provider-root",
                "createdAt": 1,
                "updatedAt": if index == 1 { 100 } else { 2 },
                "status": {"type": "idle"},
                "turns": thread_turns
            }
        }));
    }
    peer.expect_request(
        "thread/backgroundTerminals/list",
        json!({"threadId": "provider-root", "limit": 128}),
    )
    .respond(json!({
        "data": (0..4)
            .map(|index| json!({
                "itemId": format!("budget-background-{index}"),
                "processId": format!("budget-process-{index}"),
                "command": format!("budget command {index}"),
                "cwd": "/tmp/project"
            }))
            .collect::<Vec<_>>(),
        "nextCursor": null
    }));
    peer.expect_request("shutdown", Value::Null)
        .respond(Value::Null);
    let peer_task = tokio::spawn(peer.run());

    runtime.start().await.expect("runtime starts");
    runtime.collect_events(2).await;
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "thread/started".to_owned(),
            params: json!({"thread": {"id": "provider-root"}}),
            emitted_at_ms: 1_000,
        })
        .expect("root notification");
    let reconciliation = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:0")
    })
    .await;
    assert_eq!(
        reconciliation
            .activity
            .iter()
            .filter_map(|mutation| match mutation {
                ProviderActivityMutation::UpsertActor(actor) => Some(actor.id.as_str()),
                _ => None,
            })
            .collect::<HashSet<_>>()
            .len(),
        50
    );
    assert!(!reconciliation.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::UpsertActor(actor)
            if actor.id == "codex:thread:budget-child-50"
    )));
    let all_entries = reconciliation
        .activity
        .iter()
        .filter_map(|mutation| match mutation {
            ProviderActivityMutation::AppendEntry(entry) => Some(entry),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        all_entries
            .windows(2)
            .all(|pair| pair[0].created_at <= pair[1].created_at),
        "globally retained newest history must be emitted chronologically"
    );
    let first_entry_index = reconciliation
        .activity
        .iter()
        .position(|mutation| matches!(mutation, ProviderActivityMutation::AppendEntry(_)))
        .expect("retained history");
    assert!(
        reconciliation.activity[..first_entry_index]
            .iter()
            .any(|mutation| matches!(
                mutation,
                ProviderActivityMutation::UpsertActor(actor)
                    if actor.id == "codex:thread:budget-child-0"
                        && actor.status == ActivityLifecycle::Completed
            ))
    );
    let entries = reconciliation
        .activity
        .iter()
        .filter_map(|mutation| match mutation {
            ProviderActivityMutation::AppendEntry(entry)
                if entry.owner_id == "codex:thread:budget-child-0" =>
            {
                Some(entry)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(entries.len(), 196);
    assert!(
        entries
            .iter()
            .all(|entry| !entry.id.contains("budget-turn-0")),
        "the oldest turn beyond the 20-turn read budget is ignored"
    );
    assert!(
        entries
            .windows(2)
            .all(|pair| pair[0].created_at <= pair[1].created_at),
        "the newest retained entries must still be emitted chronologically"
    );
    assert!(entries.iter().any(|entry| {
        entry.id
            == "codex:event:turn-completed:budget-child-0:budget-turn-20:budget-turn-20:completed"
    }));
    assert_eq!(
        reconciliation
            .activity
            .iter()
            .filter(|mutation| matches!(
                mutation,
                ProviderActivityMutation::AppendEntry(entry)
                    if entry.owner_id == "codex:thread:budget-child-1"
                        && (entry.id.contains("newest-secondary-turn")
                            || entry.detail.as_deref() == Some("newest secondary history"))
            ))
            .count(),
        2,
        "newest history from another descendant must survive global truncation"
    );
    for child_id in ["budget-child-0", "budget-child-1"] {
        assert!(reconciliation.activity.iter().any(|mutation| matches!(
            mutation,
            ProviderActivityMutation::UpsertActor(actor)
                if actor.id == format!("codex:thread:{child_id}")
                    && actor.status == ActivityLifecycle::Completed
        )));
    }
    assert_eq!(
        reconciliation
            .activity
            .iter()
            .filter(|mutation| matches!(mutation, ProviderActivityMutation::UpsertWorkItem(_)))
            .count(),
        4
    );
    assert_eq!(
        reconciliation.activity.len(),
        256,
        "the final reconciliation mutation batch must match the repository limit"
    );
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let repository = ActivityRepository::new(database);
    let scope = ActivityScopeSeed::thread(
        "thread:fixture-thread",
        "fixture-thread",
        "codex",
        Some("codex"),
        ActivityCapabilities::structured_full(true),
    )
    .expect("valid scope");
    repository.ensure_scope(scope.clone()).await.expect("scope");
    let deltas = repository
        .apply_batch(
            &scope.scope_id,
            "codex:reconciliation:boundary",
            reconciliation.activity.clone(),
            "2026-07-24T12:00:00Z",
        )
        .await
        .expect("repository accepts bounded reconciliation batch");
    assert!(!deltas.is_empty());
    assert!(deltas.iter().all(|delta| delta.changes.len() <= 256));

    runtime.shutdown().await.expect("runtime shuts down");
    peer_task.await.expect("peer task");
}

#[tokio::test]
async fn reconciliation_page_ceiling_keeps_omitted_background_work_running() {
    const EXPECTED_PAGE_CEILING: usize = 8;

    let (connection, _protocol_incoming, peer) = scripted_peer();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(
        CodexSessionOptions {
            version: "0.1.1".to_owned(),
            thread_id: "fixture-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: CodexRuntimeMode::FullAccess,
            model: Some("gpt-5.3-codex".to_owned()),
            service_tier: None,
            effort: None,
            resume_cursor: None,
        },
        connection,
        incoming_rx,
    );
    let request_counts = Arc::new(StdMutex::new((0_usize, 0_usize)));
    let peer_request_counts = request_counts.clone();
    let peer_task = tokio::spawn(async move {
        let mut reader = BufReader::new(peer.stdin);
        let mut writer = peer.stdout;
        let mut history: Option<Value> = None;
        loop {
            let request = read_scripted_message(&mut reader, &mut writer).await;
            let method = request["method"].as_str().expect("request method");
            if request.get("id").is_none() {
                assert_eq!(method, "initialized");
                continue;
            }
            let result = match method {
                "initialize" => json!({}),
                "thread/start" => json!({
                    "thread": {"id": "provider-root"},
                    "cwd": "/tmp/project",
                    "model": "gpt-5.3-codex"
                }),
                "thread/read" => {
                    let full = {
                        assert_eq!(
                            request["params"],
                            json!({"threadId": "provider-root", "includeTurns": false})
                        );
                        empty_root_thread_read_result()
                    };
                    let metadata = thread_history_metadata(&full);
                    history = Some(full);
                    metadata
                }
                "thread/turns/list" => thread_turns_page(
                    &request["params"],
                    history
                        .as_ref()
                        .expect("the history read precedes its pages"),
                ),
                "thread/list" => {
                    let request_count = {
                        let mut counts = peer_request_counts.lock().expect("request counts");
                        counts.0 += 1;
                        counts.0
                    };
                    if request_count == 1 || request_count >= EXPECTED_PAGE_CEILING + 2 {
                        json!({
                            "data": [],
                            "nextCursor": null,
                            "backwardsCursor": null
                        })
                    } else {
                        json!({
                            "data": [],
                            "nextCursor": format!("thread-page-{}", request_count - 1),
                            "backwardsCursor": null
                        })
                    }
                }
                "thread/backgroundTerminals/list" => {
                    let request_count = {
                        let mut counts = peer_request_counts.lock().expect("request counts");
                        counts.1 += 1;
                        counts.1
                    };
                    if request_count == 1 {
                        json!({
                            "data": [{
                                "itemId": "background-prior",
                                "processId": "process-prior",
                                "command": "prior command"
                            }],
                            "nextCursor": null
                        })
                    } else if request_count >= EXPECTED_PAGE_CEILING + 2 {
                        json!({
                            "data": [],
                            "nextCursor": null
                        })
                    } else {
                        json!({
                            "data": if request_count == 2 {
                                vec![json!({
                                    "itemId": "background-prefix",
                                    "processId": "process-prefix",
                                    "command": "prefix command"
                                })]
                            } else {
                                Vec::new()
                            },
                            "nextCursor": format!("background-page-{}", request_count - 1)
                        })
                    }
                }
                "shutdown" => Value::Null,
                other => panic!("unexpected request in page-ceiling test: {other}"),
            };
            write_json(
                &mut writer,
                json!({"id": request["id"].clone(), "result": result}),
            )
            .await;
            if method == "shutdown" {
                break;
            }
        }
    });

    runtime.start().await.expect("runtime starts");
    runtime.collect_events(2).await;
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "thread/started".to_owned(),
            params: json!({"thread": {"id": "provider-root"}}),
            emitted_at_ms: 1_000,
        })
        .expect("root notification");
    let initial = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:0")
    })
    .await;
    assert!(initial.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::UpsertWorkItem(work_item)
            if work_item.id == "codex:item:background-prior"
                && work_item.status == ActivityLifecycle::Running
    )));

    incoming_tx
        .send(IncomingEvent::Notification {
            method: "item/started".to_owned(),
            params: json!({
                "threadId": "provider-root",
                "turnId": "root-turn",
                "item": {
                    "id": "spawn-page-ceiling",
                    "type": "collabAgentToolCall",
                    "tool": "spawnAgent",
                    "status": "inProgress",
                    "senderThreadId": "provider-root",
                    "receiverThreadIds": ["child-page-ceiling"],
                    "agentsStates": {}
                }
            }),
            emitted_at_ms: 1_001,
        })
        .expect("collaboration notification");
    let reconciliation = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:1")
    })
    .await;
    assert!(
        reconciliation.activity.iter().any(|mutation| matches!(
            mutation,
            ProviderActivityMutation::UpsertWorkItem(work_item)
                if work_item.id == "codex:item:background-prefix"
                    && work_item.status == ActivityLifecycle::Running
        )),
        "records from a bounded prefix remain useful"
    );
    assert!(
        !reconciliation.activity.iter().any(|mutation| matches!(
            mutation,
            ProviderActivityMutation::UpsertWorkItem(work_item)
                if work_item.id == "codex:item:background-prior"
                    && work_item.status == ActivityLifecycle::Interrupted
        )),
        "a page-ceiling prefix cannot prove that prior running work disappeared"
    );

    for (suffix, sequence, expect_interruption) in
        [("natural-empty", 2_u64, true), ("repeat-empty", 3, false)]
    {
        incoming_tx
            .send(IncomingEvent::Notification {
                method: "item/started".to_owned(),
                params: json!({
                    "threadId": "provider-root",
                    "turnId": "root-turn",
                    "item": {
                        "id": format!("spawn-{suffix}"),
                        "type": "collabAgentToolCall",
                        "tool": "spawnAgent",
                        "status": "inProgress",
                        "senderThreadId": "provider-root",
                        "receiverThreadIds": [format!("child-{suffix}")],
                        "agentsStates": {}
                    }
                }),
                emitted_at_ms: 1_002 + sequence,
            })
            .expect("collaboration notification");
        let expected_native_id = format!("codex:reconciliation:{sequence}");
        let natural_exhaustion = next_codex_event_matching(&runtime, |event| {
            event.native_event_id.as_deref() == Some(expected_native_id.as_str())
        })
        .await;
        assert_eq!(
            natural_exhaustion
                .activity
                .iter()
                .filter(|mutation| matches!(
                    mutation,
                    ProviderActivityMutation::UpsertWorkItem(work_item)
                        if work_item.id == "codex:item:background-prior"
                            && work_item.status == ActivityLifecycle::Interrupted
                ))
                .count(),
            usize::from(expect_interruption),
            "a naturally exhausted empty snapshot interrupts disappeared work exactly once"
        );
    }

    runtime.shutdown().await.expect("runtime shuts down");
    peer_task.await.expect("peer task");
    assert_eq!(
        *request_counts.lock().expect("request counts"),
        (EXPECTED_PAGE_CEILING + 3, EXPECTED_PAGE_CEILING + 3)
    );
}

#[derive(Clone, Copy, Debug)]
enum IncompleteBackgroundPage {
    CursorCycle,
    MethodIncompatible,
    SchemaIncompatible,
    TransportError,
}

async fn assert_partial_background_pages_preserve_running_work(
    incomplete_page: IncompleteBackgroundPage,
) {
    let (connection, _protocol_incoming, peer) = scripted_peer();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(
        CodexSessionOptions {
            version: "0.1.1".to_owned(),
            thread_id: "fixture-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: CodexRuntimeMode::FullAccess,
            model: Some("gpt-5.3-codex".to_owned()),
            service_tier: None,
            effort: None,
            resume_cursor: None,
        },
        connection,
        incoming_rx,
    );
    let peer_task = tokio::spawn(async move {
        let mut reader = BufReader::new(peer.stdin);
        let mut writer = peer.stdout;
        let mut background_request_count = 0_usize;
        loop {
            let request = read_scripted_message(&mut reader, &mut writer).await;
            let method = request["method"].as_str().expect("request method");
            if request.get("id").is_none() {
                assert_eq!(method, "initialized");
                continue;
            }
            let response = match method {
                "initialize" => json!({"id": request["id"].clone(), "result": {}}),
                "thread/start" => json!({
                    "id": request["id"].clone(),
                    "result": {
                        "thread": {"id": "provider-root"},
                        "cwd": "/tmp/project",
                        "model": "gpt-5.3-codex"
                    }
                }),
                "thread/read" => {
                    assert_eq!(
                        request["params"],
                        json!({"threadId": "provider-root", "includeTurns": false})
                    );
                    json!({
                        "id": request["id"].clone(),
                        "result": empty_root_thread_read_result()
                    })
                }
                "thread/turns/list" => json!({
                    "id": request["id"].clone(),
                    "result": thread_turns_page(&request["params"], &empty_root_thread_read_result())
                }),
                "thread/list" => json!({
                    "id": request["id"].clone(),
                    "result": {
                        "data": [],
                        "nextCursor": null,
                        "backwardsCursor": null
                    }
                }),
                "thread/backgroundTerminals/list" => {
                    background_request_count += 1;
                    match background_request_count {
                        1 => json!({
                            "id": request["id"].clone(),
                            "result": {
                                "data": [{
                                    "itemId": "background-prior",
                                    "processId": "process-prior",
                                    "command": "prior command"
                                }],
                                "nextCursor": null
                            }
                        }),
                        2 => json!({
                            "id": request["id"].clone(),
                            "result": {
                                "data": [{
                                    "itemId": "background-prefix",
                                    "processId": "process-prefix",
                                    "command": "prefix command"
                                }],
                                "nextCursor": "background-page-2"
                            }
                        }),
                        3 => match incomplete_page {
                            IncompleteBackgroundPage::CursorCycle => json!({
                                "id": request["id"].clone(),
                                "result": {
                                    "data": [],
                                    "nextCursor": "background-page-2"
                                }
                            }),
                            IncompleteBackgroundPage::MethodIncompatible => json!({
                                "id": request["id"].clone(),
                                "error": {
                                    "code": -32601,
                                    "message": "method not found"
                                }
                            }),
                            IncompleteBackgroundPage::SchemaIncompatible => json!({
                                "id": request["id"].clone(),
                                "result": {
                                    "data": "invalid",
                                    "nextCursor": null
                                }
                            }),
                            IncompleteBackgroundPage::TransportError => json!({
                                "id": request["id"].clone(),
                                "error": {
                                    "code": -32000,
                                    "message": "transport unavailable"
                                }
                            }),
                        },
                        other => {
                            panic!("unexpected background request {other} for {incomplete_page:?}")
                        }
                    }
                }
                "shutdown" => json!({"id": request["id"].clone(), "result": null}),
                other => panic!(
                    "unexpected request {other} for partial background case {incomplete_page:?}"
                ),
            };
            write_json(&mut writer, response).await;
            if method == "shutdown" {
                break;
            }
        }
    });

    runtime.start().await.expect("runtime starts");
    runtime.collect_events(2).await;
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "thread/started".to_owned(),
            params: json!({"thread": {"id": "provider-root"}}),
            emitted_at_ms: 1_000,
        })
        .expect("root notification");
    let initial = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:0")
    })
    .await;
    assert!(initial.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::UpsertWorkItem(work_item)
            if work_item.id == "codex:item:background-prior"
                && work_item.status == ActivityLifecycle::Running
    )));

    incoming_tx
        .send(IncomingEvent::Notification {
            method: "item/started".to_owned(),
            params: json!({
                "threadId": "provider-root",
                "turnId": "root-turn",
                "item": {
                    "id": format!("spawn-{incomplete_page:?}"),
                    "type": "collabAgentToolCall",
                    "tool": "spawnAgent",
                    "status": "inProgress",
                    "senderThreadId": "provider-root",
                    "receiverThreadIds": [format!("child-{incomplete_page:?}")],
                    "agentsStates": {}
                }
            }),
            emitted_at_ms: 1_001,
        })
        .expect("collaboration notification");
    let reconciliation = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:1")
    })
    .await;

    runtime.shutdown().await.expect("runtime shuts down");
    peer_task.await.expect("peer task");
    assert!(
        !reconciliation.activity.iter().any(|mutation| matches!(
            mutation,
            ProviderActivityMutation::UpsertWorkItem(work_item)
                if work_item.id == "codex:item:background-prior"
                    && work_item.status == ActivityLifecycle::Interrupted
        )),
        "partial background pages ending in {incomplete_page:?} cannot prove disappearance"
    );
}

#[tokio::test]
async fn reconciliation_background_cursor_cycle_is_not_authoritative() {
    assert_partial_background_pages_preserve_running_work(IncompleteBackgroundPage::CursorCycle)
        .await;
}

#[tokio::test]
async fn reconciliation_background_method_incompatibility_is_not_authoritative() {
    assert_partial_background_pages_preserve_running_work(
        IncompleteBackgroundPage::MethodIncompatible,
    )
    .await;
}

#[tokio::test]
async fn reconciliation_background_schema_incompatibility_is_not_authoritative() {
    assert_partial_background_pages_preserve_running_work(
        IncompleteBackgroundPage::SchemaIncompatible,
    )
    .await;
}

#[tokio::test]
async fn reconciliation_background_transport_error_is_not_authoritative() {
    assert_partial_background_pages_preserve_running_work(IncompleteBackgroundPage::TransportError)
        .await;
}

#[tokio::test]
async fn reconciliation_invalid_rows_do_not_consume_accepted_record_budgets() {
    let (connection, _protocol_incoming, peer) = scripted_peer();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(
        CodexSessionOptions {
            version: "0.1.1".to_owned(),
            thread_id: "fixture-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: CodexRuntimeMode::FullAccess,
            model: Some("gpt-5.3-codex".to_owned()),
            service_tier: None,
            effort: None,
            resume_cursor: None,
        },
        connection,
        incoming_rx,
    );
    let request_counts = Arc::new(StdMutex::new((0_usize, 0_usize)));
    let peer_request_counts = request_counts.clone();
    let peer_task = tokio::spawn(async move {
        let mut reader = BufReader::new(peer.stdin);
        let mut writer = peer.stdout;
        let mut history: Option<Value> = None;
        loop {
            let request = read_scripted_message(&mut reader, &mut writer).await;
            let method = request["method"].as_str().expect("request method");
            if request.get("id").is_none() {
                assert_eq!(method, "initialized");
                continue;
            }
            let result = match method {
                "initialize" => json!({}),
                "thread/start" => json!({
                    "thread": {"id": "provider-root"},
                    "cwd": "/tmp/project",
                    "model": "gpt-5.3-codex"
                }),
                "thread/list" => {
                    let request_count = {
                        let mut counts = peer_request_counts.lock().expect("request counts");
                        counts.0 += 1;
                        counts.0
                    };
                    if request_count == 1 {
                        let invalid_threads = (0..50)
                            .map(|index| {
                                if index % 2 == 0 {
                                    json!({
                                        "parentThreadId": "provider-root",
                                        "updatedAt": 1,
                                        "status": {"type": "idle"}
                                    })
                                } else {
                                    json!({
                                        "id": format!("foreign-{index}"),
                                        "parentThreadId": "foreign-root",
                                        "updatedAt": 1,
                                        "status": {"type": "idle"}
                                    })
                                }
                            })
                            .collect::<Vec<_>>();
                        json!({
                            "data": invalid_threads,
                            "nextCursor": "valid-thread-page",
                            "backwardsCursor": null
                        })
                    } else {
                        assert_eq!(request["params"]["cursor"], "valid-thread-page");
                        json!({
                            "data": [{
                                "id": "accepted-child",
                                "parentThreadId": "provider-root",
                                "createdAt": 1,
                                "updatedAt": 2,
                                "status": {"type": "idle"}
                            }],
                            "nextCursor": null,
                            "backwardsCursor": null
                        })
                    }
                }
                "thread/read" => {
                    let full = match request["params"]["threadId"].as_str() {
                        Some("provider-root") => {
                            assert_eq!(
                                request["params"],
                                json!({"threadId": "provider-root", "includeTurns": false})
                            );
                            empty_root_thread_read_result()
                        }
                        Some("accepted-child") => json!({
                            "thread": {
                                "id": "accepted-child",
                                "parentThreadId": "provider-root",
                                "createdAt": 1,
                                "updatedAt": 2,
                                "status": {"type": "idle"},
                                "turns": []
                            }
                        }),
                        other => panic!("unexpected accepted-budget read target {other:?}"),
                    };
                    let metadata = thread_history_metadata(&full);
                    history = Some(full);
                    metadata
                }
                "thread/turns/list" => thread_turns_page(
                    &request["params"],
                    history
                        .as_ref()
                        .expect("the history read precedes its pages"),
                ),
                "thread/backgroundTerminals/list" => {
                    let request_count = {
                        let mut counts = peer_request_counts.lock().expect("request counts");
                        counts.1 += 1;
                        counts.1
                    };
                    if request_count == 1 {
                        let invalid_terminals = (0..128)
                            .map(|index| {
                                json!({
                                    "processId": format!("missing-item-{index}"),
                                    "command": "ignored"
                                })
                            })
                            .collect::<Vec<_>>();
                        json!({
                            "data": invalid_terminals,
                            "nextCursor": "valid-background-page"
                        })
                    } else {
                        assert_eq!(request["params"]["cursor"], "valid-background-page");
                        json!({
                            "data": [{
                                "itemId": "accepted-background",
                                "processId": "accepted-process",
                                "command": format!("safe-command-{}", "x".repeat(400)),
                                "cwd": "/private/must-not-project",
                                "aggregatedOutput": "private-command-output"
                            }],
                            "nextCursor": null
                        })
                    }
                }
                "shutdown" => Value::Null,
                other => panic!("unexpected request in accepted-budget test: {other}"),
            };
            write_json(
                &mut writer,
                json!({"id": request["id"].clone(), "result": result}),
            )
            .await;
            if method == "shutdown" {
                break;
            }
        }
    });

    runtime.start().await.expect("runtime starts");
    runtime.collect_events(2).await;
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "thread/started".to_owned(),
            params: json!({"thread": {"id": "provider-root"}}),
            emitted_at_ms: 1_000,
        })
        .expect("root notification");
    let reconciliation = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:0")
    })
    .await;

    assert!(reconciliation.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::UpsertActor(actor)
            if actor.id == "codex:thread:accepted-child"
    )));
    assert!(reconciliation.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::UpsertWorkItem(work_item)
            if work_item.id == "codex:item:accepted-background"
                && work_item.name.starts_with("safe-command-")
                && work_item.name.encode_utf16().count() <= 256
                && work_item.summary.as_deref() == Some("accepted-process")
                && !work_item.name.contains("must-not-project")
    )));
    let projected_activity = format!("{:?}", reconciliation.activity);
    assert!(!projected_activity.contains("/private/must-not-project"));
    assert!(!projected_activity.contains("private-command-output"));
    assert_eq!(
        *request_counts.lock().expect("request counts"),
        (2, 2),
        "invalid rows must not exhaust either accepted-record budget"
    );

    runtime.shutdown().await.expect("runtime shuts down");
    peer_task.await.expect("peer task");
}

#[tokio::test]
async fn reconciliation_background_pagination_skips_empty_pages_and_stops_at_128() {
    let (connection, _protocol_incoming, mut peer) = scripted_peer();
    let (incoming_tx, incoming_rx) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(
        CodexSessionOptions {
            version: "0.1.1".to_owned(),
            thread_id: "fixture-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: CodexRuntimeMode::FullAccess,
            model: Some("gpt-5.3-codex".to_owned()),
            service_tier: None,
            effort: None,
            resume_cursor: None,
        },
        connection,
        incoming_rx,
    );
    peer.expect_request("initialize", fixture("initialize-params.json"))
        .respond(json!({}));
    peer.expect_notification("initialized");
    peer.expect_request(
        "thread/start",
        json!({
            "cwd": "/tmp/project",
            "approvalPolicy": "never",
            "sandbox": "danger-full-access",
            "model": "gpt-5.3-codex",
            "serviceTier": null,
        }),
    )
    .respond(json!({
        "thread": {"id": "provider-root"},
        "cwd": "/tmp/project",
        "model": "gpt-5.3-codex"
    }));
    peer.expect_request(
        "thread/read",
        json!({"threadId": "provider-root", "includeTurns": false}),
    )
    .respond_history(empty_root_thread_read_result());
    peer.expect_request(
        "thread/list",
        json!({"ancestorThreadId": "provider-root", "limit": 50}),
    )
    .respond(json!({
        "data": [],
        "nextCursor": null,
        "backwardsCursor": null
    }));
    peer.expect_request(
        "thread/backgroundTerminals/list",
        json!({"threadId": "provider-root", "limit": 128}),
    )
    .respond(json!({"data": [], "nextCursor": "background-page-2"}));
    let terminals = (0..129)
        .map(|index| {
            json!({
                "itemId": format!("background-budget-{index}"),
                "processId": format!("process-{index}"),
                "command": format!("command-{index}"),
                "cwd": "/tmp/project"
            })
        })
        .collect::<Vec<_>>();
    peer.expect_request(
        "thread/backgroundTerminals/list",
        json!({
            "threadId": "provider-root",
            "limit": 128,
            "cursor": "background-page-2"
        }),
    )
    .respond(json!({
        "data": terminals,
        "nextCursor": null
    }));
    peer.expect_request("shutdown", Value::Null)
        .respond(Value::Null);
    let peer_task = tokio::spawn(peer.run());

    runtime.start().await.expect("runtime starts");
    runtime.collect_events(2).await;
    incoming_tx
        .send(IncomingEvent::Notification {
            method: "thread/started".to_owned(),
            params: json!({"thread": {"id": "provider-root"}}),
            emitted_at_ms: 1_000,
        })
        .expect("root notification");
    let reconciliation = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:0")
    })
    .await;
    assert_eq!(
        reconciliation
            .activity
            .iter()
            .filter(|mutation| matches!(mutation, ProviderActivityMutation::UpsertWorkItem(_)))
            .count(),
        128
    );
    assert!(!reconciliation.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::UpsertWorkItem(work_item)
            if work_item.id == "codex:item:background-budget-128"
    )));

    runtime.shutdown().await.expect("runtime shuts down");
    peer_task.await.expect("peer task");
}

#[tokio::test]
async fn activity_runtime_reconnect_and_shutdown_drop_stale_incoming_work() {
    let (connection_a, _protocol_incoming_a, mut peer_a) = scripted_peer();
    let (old_incoming_tx, old_incoming_rx) = mpsc::unbounded_channel();
    let runtime = CodexSessionRuntime::new(
        CodexSessionOptions {
            version: "0.1.1".to_owned(),
            thread_id: "fixture-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: CodexRuntimeMode::FullAccess,
            model: Some("gpt-5.3-codex".to_owned()),
            service_tier: None,
            effort: None,
            resume_cursor: None,
        },
        connection_a,
        old_incoming_rx,
    );
    peer_a
        .expect_request("initialize", fixture("initialize-params.json"))
        .respond(json!({}));
    peer_a.expect_notification("initialized");
    peer_a
        .expect_request(
            "thread/start",
            json!({
                "cwd": "/tmp/project",
                "approvalPolicy": "never",
                "sandbox": "danger-full-access",
                "model": "gpt-5.3-codex",
                "serviceTier": null,
            }),
        )
        .respond(json!({
            "thread": { "id": "provider-thread-1" },
            "cwd": "/tmp/project",
            "model": "gpt-5.3-codex",
        }));
    let peer_a_task = tokio::spawn(peer_a.run());
    runtime.start().await.expect("initial runtime start");
    let startup_events = runtime.collect_events(4).await;
    assert_eq!(
        startup_events.last().map(|event| event.event_type.as_str()),
        Some("session.ready")
    );
    assert!(
        startup_events
            .iter()
            .any(|event| event.event_type == "mcp.status.updated")
    );
    peer_a_task.await.expect("initial peer");

    old_incoming_tx
        .send(IncomingEvent::Notification {
            method: "item/started".to_owned(),
            params: json!({
                "threadId": "provider-thread-1",
                "turnId": "turn-1",
                "item": {
                    "id": "spawn-1",
                    "type": "collabAgentToolCall",
                    "tool": "spawnAgent",
                    "status": "inProgress",
                    "senderThreadId": "provider-thread-1",
                    "receiverThreadIds": ["child-1"],
                    "agentsStates": {
                        "child-1": { "status": "running", "message": null }
                    }
                },
                "startedAtMs": 1_000,
            }),
            emitted_at_ms: 1_000,
        })
        .expect("initial incoming task is live");
    let initial_activity = timeout(Duration::from_secs(2), runtime.next_event())
        .await
        .expect("initial activity timeout")
        .expect("initial activity");
    assert_eq!(
        initial_activity.native_event_id.as_deref(),
        Some("codex:activity:0"),
        "unexpected event after startup: {initial_activity:?}"
    );

    let (connection_b, _protocol_incoming_b, mut peer_b) = scripted_peer();
    let (new_incoming_tx, new_incoming_rx) = mpsc::unbounded_channel();
    peer_b
        .expect_request("initialize", fixture("initialize-params.json"))
        .respond(json!({}));
    peer_b.expect_notification("initialized");
    peer_b
        .expect_request(
            "thread/resume",
            json!({
                "threadId": "provider-thread-1",
                "cwd": "/tmp/project",
                "approvalPolicy": "never",
                "sandbox": "danger-full-access",
                "model": "gpt-5.3-codex",
                "serviceTier": null,
            }),
        )
        .respond(json!({
            "thread": { "id": "provider-thread-1" },
            "cwd": "/tmp/project",
            "model": "gpt-5.3-codex",
        }));
    peer_b
        .expect_request(
            "thread/read",
            json!({"threadId": "provider-thread-1", "includeTurns": false}),
        )
        .respond_history(empty_thread_read_result("provider-thread-1"));
    peer_b
        .expect_request(
            "thread/list",
            json!({"ancestorThreadId": "provider-thread-1", "limit": 50}),
        )
        .respond(json!({
            "data": [],
            "nextCursor": null,
            "backwardsCursor": null
        }));
    peer_b
        .expect_request(
            "thread/backgroundTerminals/list",
            json!({"threadId": "provider-thread-1", "limit": 128}),
        )
        .respond(json!({"data": [], "nextCursor": null}));
    peer_b
        .expect_request("shutdown", Value::Null)
        .respond(Value::Null);
    let peer_b_task = tokio::spawn(peer_b.run());
    runtime
        .reconnect(connection_b, new_incoming_rx)
        .await
        .expect("runtime reconnects");
    runtime.collect_events(2).await;
    tokio::task::yield_now().await;
    assert!(
        old_incoming_tx
            .send(IncomingEvent::Notification {
                method: "item/agentMessage/delta".to_owned(),
                params: json!({
                    "threadId": "child-1",
                    "turnId": "old-turn",
                    "itemId": "old-message",
                    "delta": "stale",
                }),
                emitted_at_ms: 1_001,
            })
            .is_err(),
        "reconnect must drop the prior incoming receiver"
    );

    new_incoming_tx
        .send(IncomingEvent::Notification {
            method: "item/agentMessage/delta".to_owned(),
            params: json!({
                "threadId": "child-1",
                "turnId": "new-turn",
                "itemId": "new-message",
                "delta": "current",
            }),
            emitted_at_ms: 1_002,
        })
        .expect("replacement incoming task is live");
    new_incoming_tx
        .send(IncomingEvent::Notification {
            method: "item/completed".to_owned(),
            params: json!({
                "threadId": "child-1",
                "turnId": "new-turn",
                "item": {
                    "id": "new-message",
                    "type": "agentMessage",
                    "text": "current",
                },
                "completedAtMs": 1_002,
            }),
            emitted_at_ms: 1_002,
        })
        .expect("replacement completion is live");
    let current_activity = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:activity:2")
    })
    .await;
    assert_eq!(
        current_activity.native_event_id.as_deref(),
        Some("codex:activity:2")
    );
    assert!(matches!(
        current_activity.activity.as_slice(),
        [ProviderActivityMutation::AppendEntry(entry)]
            if entry.detail.as_deref() == Some("current")
    ));

    runtime.shutdown().await.expect("runtime shuts down");
    tokio::task::yield_now().await;
    assert!(
        new_incoming_tx
            .send(IncomingEvent::Notification {
                method: "item/agentMessage/delta".to_owned(),
                params: json!({
                    "threadId": "child-1",
                    "turnId": "shutdown-turn",
                    "itemId": "shutdown-message",
                    "delta": "must-not-leak",
                }),
                emitted_at_ms: 1_003,
            })
            .is_err(),
        "shutdown must drop the replacement incoming receiver"
    );
    assert!(
        timeout(Duration::from_millis(100), runtime.next_event())
            .await
            .is_err(),
        "shutdown must not leak stale activity"
    );
    peer_b_task.await.expect("replacement peer");
}

#[tokio::test]
async fn reconnect_resume_fallback_and_shutdown_stay_correlated() {
    let reconnect_fixture = fixture("reconnect-scenario.json");

    let (connection_a, incoming_a, mut peer_a) = scripted_peer();
    let runtime = CodexSessionRuntime::new(
        CodexSessionOptions {
            version: "0.1.1".to_owned(),
            thread_id: "fixture-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: CodexRuntimeMode::FullAccess,
            model: Some("gpt-5.3-codex".to_owned()),
            service_tier: None,
            effort: None,
            resume_cursor: None,
        },
        connection_a.clone(),
        incoming_a,
    );
    peer_a
        .expect_request("initialize", fixture("initialize-params.json"))
        .respond(json!({ "userAgent": "mock-a" }));
    peer_a.expect_notification("initialized");
    peer_a
        .expect_request(
            "thread/start",
            reconnect_fixture["initialThreadStartRequest"].clone(),
        )
        .respond(reconnect_fixture["initialThreadStartResponse"].clone());
    let peer_a_task = tokio::spawn(peer_a.run());
    runtime.start().await.expect("initial start");
    peer_a_task.await.expect("peer a");

    let (connection_b, incoming_b, mut peer_b) = scripted_peer();
    peer_b
        .expect_request("initialize", fixture("initialize-params.json"))
        .respond(json!({ "userAgent": "mock-b" }));
    peer_b.expect_notification("initialized");
    peer_b
        .expect_request("thread/resume", reconnect_fixture["resumeRequest"].clone())
        .respond_error(json!({
            "code": -32603,
            "message": "Thread does not exist"
        }));
    peer_b
        .expect_request(
            "thread/start",
            reconnect_fixture["fallbackThreadStartRequest"].clone(),
        )
        .respond(reconnect_fixture["fallbackThreadStartResponse"].clone());
    peer_b
        .expect_request(
            "thread/read",
            json!({"threadId": "provider-thread-2", "includeTurns": false}),
        )
        .respond_history(empty_thread_read_result("provider-thread-2"));
    peer_b
        .expect_request(
            "thread/list",
            json!({"ancestorThreadId": "provider-thread-2", "limit": 50}),
        )
        .respond(json!({
            "data": [],
            "nextCursor": null,
            "backwardsCursor": null
        }));
    peer_b
        .expect_request(
            "thread/backgroundTerminals/list",
            json!({"threadId": "provider-thread-2", "limit": 128}),
        )
        .respond(json!({"data": [], "nextCursor": null}));
    peer_b
        .expect_request("shutdown", Value::Null)
        .respond(Value::Null);
    let peer_b_task = tokio::spawn(peer_b.run());

    runtime
        .reconnect(connection_b.clone(), incoming_b)
        .await
        .expect("reconnect");
    let reconciliation = next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:0")
    })
    .await;
    assert!(reconciliation.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::SetScope {
            observation_state: ActivityObservationState::Live,
            ..
        }
    )));
    runtime.shutdown().await.expect("shutdown");
    peer_b_task.await.expect("peer b");
}

fn project_runtime(
    connection: JsonRpcConnection,
    incoming: mpsc::UnboundedReceiver<IncomingEvent>,
) -> CodexSessionRuntime {
    CodexSessionRuntime::new(
        CodexSessionOptions {
            version: "0.1.1".to_owned(),
            thread_id: "fixture-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: CodexRuntimeMode::FullAccess,
            model: Some("gpt-5.3-codex".to_owned()),
            service_tier: None,
            effort: None,
            resume_cursor: None,
        },
        connection,
        incoming,
    )
}

/// Codex 0.151+ starts every new thread with paginated history and rewinds it through
/// `thread/revert` before a turn ID. Older Codex omits `historyMode` (`None`).
fn expect_rewind_session_start(peer: &mut ScriptedPeer, history_mode: Option<&str>) {
    let reconnect_fixture = fixture("reconnect-scenario.json");
    let mut thread = json!({ "id": "provider-thread-1", "turns": [] });
    if let Some(history_mode) = history_mode {
        thread["historyMode"] = json!(history_mode);
    }
    peer.expect_request("initialize", fixture("initialize-params.json"))
        .respond(json!({ "userAgent": "bibcode_desktop/0.162.1" }));
    peer.expect_notification("initialized");
    peer.expect_request(
        "thread/start",
        reconnect_fixture["initialThreadStartRequest"].clone(),
    )
    .respond(json!({
        "cwd": "/tmp/project",
        "model": "gpt-5.3-codex",
        "thread": thread
    }));
}

#[tokio::test]
async fn paginated_thread_rollback_reverts_before_the_oldest_rolled_back_turn() {
    let (connection, incoming, mut peer) = scripted_peer();
    let runtime = project_runtime(connection, incoming);
    expect_rewind_session_start(&mut peer, Some("paginated"));
    // Three turns back, across two newest-first pages.
    peer.expect_request(
        "thread/turns/list",
        json!({
            "threadId": "provider-thread-1",
            "limit": 3,
            "sortDirection": "desc",
            "itemsView": "notLoaded"
        }),
    )
    .respond(json!({
        "data": [{ "id": "turn-5", "items": [] }, { "id": "turn-4", "items": [] }],
        "nextCursor": "page-2",
        "backwardsCursor": "turn-5"
    }));
    peer.expect_request(
        "thread/turns/list",
        json!({
            "threadId": "provider-thread-1",
            "limit": 1,
            "sortDirection": "desc",
            "itemsView": "notLoaded",
            "cursor": "page-2"
        }),
    )
    .respond(json!({
        "data": [{ "id": "turn-3", "items": [] }],
        "nextCursor": "page-3",
        "backwardsCursor": "turn-3"
    }));
    let revert_response = json!({
        "thread": {
            "id": "provider-thread-1",
            "historyMode": "paginated",
            "turns": []
        },
        "turnsBackwardsCursor": null,
        "itemsBackwardsCursor": null
    });
    peer.expect_request(
        "thread/revert",
        json!({ "threadId": "provider-thread-1", "beforeTurnId": "turn-3" }),
    )
    .respond(revert_response.clone());
    // Codex holds fewer turns than requested: rewind everything it still has.
    peer.expect_request(
        "thread/turns/list",
        json!({
            "threadId": "provider-thread-1",
            "limit": 5,
            "sortDirection": "desc",
            "itemsView": "notLoaded"
        }),
    )
    .respond(json!({
        "data": [{ "id": "turn-2", "items": [] }, { "id": "turn-1", "items": [] }],
        "nextCursor": null,
        "backwardsCursor": "turn-2"
    }));
    peer.expect_request(
        "thread/revert",
        json!({ "threadId": "provider-thread-1", "beforeTurnId": "turn-1" }),
    )
    .respond(revert_response);
    peer.expect_request("shutdown", Value::Null)
        .respond(Value::Null);
    let peer_task = tokio::spawn(peer.run());

    runtime.start().await.expect("start");
    runtime
        .rollback_thread(0)
        .await
        .expect("rolling back no turns sends nothing");
    runtime
        .rollback_thread(3)
        .await
        .expect("paginated rollback reverts three turns");
    runtime
        .rollback_thread(5)
        .await
        .expect("paginated rollback reverts every remaining turn");
    runtime.shutdown().await.expect("shutdown");
    peer_task.await.expect("peer");
}

/// Only a paginated thread can be rewound. A Codex that reports no `historyMode` predates
/// `thread/revert`; a `legacy` thread is rejected by it. Both fail without a request.
#[tokio::test]
async fn rewind_without_paginated_history_fails_before_contacting_codex() {
    for (history_mode, next_step) in [
        (None, "Update Codex to 0.151.0 or later"),
        (
            Some("legacy"),
            "Start a new thread on Codex 0.151.0 or later",
        ),
    ] {
        let (connection, incoming, mut peer) = scripted_peer();
        let runtime = project_runtime(connection, incoming);
        expect_rewind_session_start(&mut peer, history_mode);
        // The next request the peer accepts is shutdown: no rewind request may come first.
        peer.expect_request("shutdown", Value::Null)
            .respond(Value::Null);
        let peer_task = tokio::spawn(peer.run());

        runtime.start().await.expect("start");
        let error = runtime
            .rollback_thread(1)
            .await
            .expect_err("only a paginated thread can be rewound")
            .to_string();
        assert!(
            error.contains(next_step),
            "{history_mode:?}: the error must say what to do next: {error}"
        );
        runtime.shutdown().await.expect("shutdown");
        peer_task.await.expect("peer");
    }
}

/// A Codex without `thread/revert` rejects it while decoding the request (-32600).
#[tokio::test]
async fn rewind_rejected_as_a_missing_method_asks_for_a_newer_codex() {
    let (connection, incoming, mut peer) = scripted_peer();
    let runtime = project_runtime(connection, incoming);
    expect_rewind_session_start(&mut peer, Some("paginated"));
    peer.expect_request(
        "thread/turns/list",
        json!({
            "threadId": "provider-thread-1",
            "limit": 1,
            "sortDirection": "desc",
            "itemsView": "notLoaded"
        }),
    )
    .respond(json!({ "data": [{ "id": "turn-1", "items": [] }], "nextCursor": null }));
    peer.expect_request(
        "thread/revert",
        json!({ "threadId": "provider-thread-1", "beforeTurnId": "turn-1" }),
    )
    .respond_error(json!({
        "code": -32600,
        "message": "Invalid request: unknown variant `thread/revert`, expected one of `initialize`, `thread/start`"
    }));
    peer.expect_request("shutdown", Value::Null)
        .respond(Value::Null);
    let peer_task = tokio::spawn(peer.run());

    runtime.start().await.expect("start");
    let error = runtime
        .rollback_thread(1)
        .await
        .expect_err("a missing thread/revert cannot rewind")
        .to_string();
    assert!(
        error.contains("Update Codex to 0.151.0 or later") && !error.contains("unknown variant"),
        "{error}"
    );
    runtime.shutdown().await.expect("shutdown");
    peer_task.await.expect("peer");
}

#[tokio::test]
async fn codex_runtime_covers_auto_edit_resume_requests_and_stream_edges() {
    let (connection, incoming, mut peer) = scripted_peer();
    let runtime = CodexSessionRuntime::new(
        CodexSessionOptions {
            version: "0.1.1".to_owned(),
            thread_id: "codex-edge-thread".to_owned(),
            cwd: "/tmp/codex-edge".to_owned(),
            runtime_mode: CodexRuntimeMode::AutoAcceptEdits,
            model: None,
            service_tier: Some("fast".to_owned()),
            effort: Some("high".to_owned()),
            resume_cursor: Some("resume-edge".to_owned()),
        },
        connection,
        incoming,
    );
    assert!(
        runtime
            .set_goal("  ")
            .await
            .expect_err("empty goals must fail")
            .to_string()
            .contains("between 1 and 4000")
    );
    assert!(
        runtime
            .set_goal(&"x".repeat(4_001))
            .await
            .expect_err("oversized goals must fail")
            .to_string()
            .contains("between 1 and 4000")
    );
    assert!(
        runtime
            .respond_to_request("missing", "accept")
            .await
            .expect_err("unknown approvals must fail")
            .to_string()
            .contains("Unknown pending request id missing")
    );
    assert!(
        runtime
            .respond_to_user_input("missing-input", json!({}))
            .await
            .expect_err("unknown prompts must fail")
            .to_string()
            .contains("Unknown pending request id missing-input")
    );

    peer.expect_request("initialize", build_initialize_params("0.1.1"))
        .respond(json!({}));
    peer.expect_notification("initialized");
    peer.expect_request(
        "thread/resume",
        json!({
            "threadId": "resume-edge",
            "cwd": "/tmp/codex-edge",
            "approvalPolicy": "on-request",
            "sandbox": "workspace-write",
            "model": null,
            "serviceTier": "fast",
        }),
    )
    .respond(json!({ "thread": { "id": "provider-edge" } }));

    peer.expect_request(
        "turn/start",
        codex_edge_turn_params("provider-edge", "invalid turn"),
    )
    .respond(json!({}));

    peer.expect_request(
        "turn/start",
        codex_edge_turn_params("provider-edge", "file approval"),
    )
    .respond(json!({ "turn": { "id": "file-turn" } }))
    .emit_notification(json!({
        "method": "thread/started",
        "params": { "thread": { "id": "provider-updated" } }
    }));
    peer.expect_request(
        "thread/read",
        json!({"threadId": "provider-updated", "includeTurns": false}),
    )
    .respond_history(empty_thread_read_result("provider-updated"));
    peer.expect_request(
        "thread/list",
        json!({"ancestorThreadId": "provider-updated", "limit": 50}),
    )
    .respond(json!({
        "data": [],
        "nextCursor": null,
        "backwardsCursor": null
    }));
    peer.expect_request(
        "thread/backgroundTerminals/list",
        json!({"threadId": "provider-updated", "limit": 128}),
    )
    .respond(json!({"data": [], "nextCursor": null}))
    .emit_notification(json!({
        "method": "turn/started",
        "params": { "turn": {} }
    }))
    .emit_notification(json!({
        "method": "turn/started",
        "params": { "turn": { "id": "file-turn" } }
    }))
    .emit_notification(json!({
        "method": "item/started",
        "params": {
            "turnId": "file-turn",
            "item": { "type": "fileChange", "id": "not-command" }
        }
    }))
    .emit_notification(json!({
        "method": "item/started",
        "params": {
            "turnId": "file-turn",
            "item": { "type": "commandExecution", "id": "command-without-detail" }
        }
    }))
    .emit_request(json!({
        "id": 6001,
        "method": "item/fileChange/requestApproval",
        "params": { "turnId": "file-turn" }
    }))
    .expect_response(json!({
        "id": 6001,
        "result": { "decision": "decline" }
    }))
    .emit_notification(json!({
        "method": "turn/completed",
        "params": {
            "turn": {
                "id": "file-turn",
                "status": "failed",
                "error": { "message": "provider failed" }
            }
        }
    }));

    peer.expect_request(
        "turn/start",
        codex_edge_turn_params("provider-updated", "user input"),
    )
    .respond(json!({ "turn": { "id": "input-turn" } }))
    .emit_request(json!({
        "id": 6002,
        "method": "item/tool/requestUserInput",
        "params": {
            "turnId": "input-turn",
            "questions": [
                {
                    "id": "single",
                    "header": "Single",
                    "question": "Choose one",
                    "options": [{ "label": "yes" }]
                },
                { "id": "invalid" }
            ]
        }
    }))
    .expect_response(json!({
        "id": 6002,
        "result": {
            "answers": {
                "single": { "answers": ["yes"] },
                "many": { "answers": ["a", "b"] },
                "ignored": { "answers": [] }
            }
        }
    }))
    .emit_notification(json!({
        "method": "turn/completed",
        "params": { "turn": { "id": "input-turn" } }
    }));

    peer.expect_request(
        "turn/start",
        codex_edge_turn_params("provider-updated", "generic cancellation"),
    )
    .respond(json!({ "turn": { "id": "cancel-input-turn" } }))
    .emit_request(json!({
        "id": 6003,
        "method": "item/tool/requestUserInput",
        "params": { "turnId": "cancel-input-turn", "questions": null }
    }))
    .expect_response(json!({
        "id": 6003,
        "result": { "decision": "cancel" }
    }))
    .emit_notification(json!({
        "method": "turn/completed",
        "params": { "turn": { "id": "cancel-input-turn", "status": "completed" } }
    }));

    peer.expect_request(
        "turn/start",
        codex_edge_turn_params("provider-updated", "unknown request"),
    )
    .respond(json!({ "turn": { "id": "unknown-turn" } }))
    .emit_request(json!({
        "id": 6004,
        "method": "unsupported/request",
        "params": {}
    }))
    .expect_response(json!({
        "id": 6004,
        "error": {
            "code": -32601,
            "message": "Method not found: unsupported/request",
            "data": null
        }
    }))
    .emit_notification(json!({
        "method": "ignored/notification",
        "params": {}
    }))
    .emit_notification(json!({
        "method": "turn/completed",
        "params": { "turn": { "id": "unknown-turn", "status": "completed" } }
    }));

    peer.expect_request(
        "turn/start",
        codex_edge_turn_params("provider-updated", "interrupt"),
    )
    .emit_stderr("ordinary provider warning")
    .emit_stderr("FAILED TO CONNECT TO WEBSOCKET while streaming")
    .respond(json!({ "turn": { "id": "interrupt-turn" } }));
    peer.expect_request(
        "turn/interrupt",
        json!({ "threadId": "provider-updated", "turnId": "explicit-turn" }),
    )
    .respond(json!({}));

    let peer_task = tokio::spawn(peer.run());
    let session = runtime.start().await.expect("resumed session");
    assert_eq!(session.resume_cursor.as_deref(), Some("provider-edge"));
    assert_eq!(session.cwd, "/tmp/codex-edge");
    runtime.collect_events(2).await;
    runtime
        .interrupt_turn(None)
        .await
        .expect("no active turn is a no-op");

    assert!(
        runtime
            .send_turn(Some("invalid turn".to_owned()), Vec::new(), None, None)
            .await
            .expect_err("missing turn ids must fail")
            .to_string()
            .contains("missing turn.id")
    );

    runtime
        .send_turn(Some("file approval".to_owned()), Vec::new(), None, None)
        .await
        .expect("file turn");
    let file_request = next_codex_event_matching(&runtime, |event| {
        event.request_id.as_deref() == Some("approval:6001")
    })
    .await;
    assert_eq!(
        file_request.payload["requestType"],
        json!("file_change_approval")
    );
    assert_eq!(file_request.payload["detail"], "");
    runtime
        .respond_to_request("approval:6001", "decline")
        .await
        .expect("file decision");
    let failed = next_codex_event_matching(&runtime, |event| {
        event.event_type == "turn.completed" && event.turn_id.as_deref() == Some("file-turn")
    })
    .await;
    assert_eq!(failed.payload["state"], "failed");
    assert_eq!(failed.payload["errorMessage"], "provider failed");
    assert_eq!(failed.payload["errorClass"], "provider_error");

    runtime
        .send_turn(Some("user input".to_owned()), Vec::new(), None, None)
        .await
        .expect("input turn");
    let input_request = next_codex_event_matching(&runtime, |event| {
        event.request_id.as_deref() == Some("user-input:6002")
    })
    .await;
    assert_eq!(
        input_request.payload["questions"].as_array().unwrap().len(),
        1
    );
    runtime
        .respond_to_user_input(
            "user-input:6002",
            json!({
                "single": "yes",
                "many": ["a", "b"],
                "ignored": 42,
            }),
        )
        .await
        .expect("input response");
    let resolved = next_codex_event_matching(&runtime, |event| {
        event.request_id.as_deref() == Some("user-input:6002")
            && event.event_type == "user-input.resolved"
    })
    .await;
    assert_eq!(resolved.payload["answers"]["single"], "yes");
    assert_eq!(resolved.payload["answers"]["many"], json!(["a", "b"]));

    runtime
        .send_turn(
            Some("generic cancellation".to_owned()),
            Vec::new(),
            None,
            None,
        )
        .await
        .expect("generic cancellation turn");
    next_codex_event_matching(&runtime, |event| {
        event.request_id.as_deref() == Some("user-input:6003")
    })
    .await;
    runtime
        .respond_to_request("user-input:6003", "cancel")
        .await
        .expect("generic cancellation");

    runtime
        .send_turn(Some("unknown request".to_owned()), Vec::new(), None, None)
        .await
        .expect("unknown request turn");
    next_codex_event_matching(&runtime, |event| {
        event.event_type == "turn.completed" && event.turn_id.as_deref() == Some("unknown-turn")
    })
    .await;

    runtime
        .send_turn(Some("interrupt".to_owned()), Vec::new(), None, None)
        .await
        .expect("interrupt turn");
    runtime
        .interrupt_turn(Some("explicit-turn".to_owned()))
        .await
        .expect("explicit interrupt");
    peer_task.await.expect("peer");

    let mut saw_warning = false;
    let mut saw_fatal = false;
    let mut saw_exit = false;
    for _ in 0..12 {
        let event = timeout(Duration::from_secs(2), runtime.next_event())
            .await
            .expect("edge event timeout")
            .expect("edge event");
        saw_warning |= event.event_type == "runtime.warning";
        saw_fatal |= event.event_type == "runtime.error"
            && event.payload["class"] == json!("provider_error");
        saw_exit |= event.event_type == "session.exited";
        if saw_warning && saw_fatal && saw_exit {
            break;
        }
    }
    assert!(saw_warning && saw_fatal && saw_exit);
}

#[tokio::test]
async fn codex_runtime_and_probe_reject_invalid_provider_payloads() {
    let (probe_connection, _probe_incoming, mut probe_peer) = scripted_peer();
    probe_peer
        .expect_request("initialize", build_initialize_params("0.1.1"))
        .respond(json!({}));
    probe_peer.expect_notification("initialized");
    probe_peer
        .expect_request("account/read", json!({}))
        .respond(json!({}));
    probe_peer
        .expect_request("model/list", json!({}))
        .respond(json!({ "nextCursor": null }));
    let probe_task = tokio::spawn(probe_peer.run());
    assert!(
        probe_provider(&probe_connection, "0.1.1", "/tmp", &[])
            .await
            .expect_err("model data is required")
            .to_string()
            .contains("missing data array")
    );
    probe_task.await.expect("probe peer");

    let (missing_connection, missing_incoming, mut missing_peer) = scripted_peer();
    let missing_runtime = CodexSessionRuntime::new(
        codex_invalid_options(None),
        missing_connection,
        missing_incoming,
    );
    assert!(
        missing_runtime
            .send_turn(Some("before start".to_owned()), Vec::new(), None, None)
            .await
            .expect_err("turns require a provider thread")
            .to_string()
            .contains("missing a provider thread id")
    );
    missing_peer
        .expect_request("initialize", build_initialize_params("0.1.1"))
        .respond(json!({}));
    missing_peer.expect_notification("initialized");
    missing_peer
        .expect_request(
            "thread/start",
            json!({
                "cwd": "/tmp/codex-invalid",
                "approvalPolicy": "untrusted",
                "sandbox": "read-only",
                "model": null,
                "serviceTier": null,
            }),
        )
        .respond(json!({}));
    let missing_task = tokio::spawn(missing_peer.run());
    assert!(
        missing_runtime
            .start()
            .await
            .expect_err("thread identifiers are required")
            .to_string()
            .contains("missing thread.id")
    );
    missing_task.await.expect("missing peer");

    let (resume_connection, resume_incoming, mut resume_peer) = scripted_peer();
    let resume_runtime = CodexSessionRuntime::new(
        codex_invalid_options(Some("unavailable-thread")),
        resume_connection,
        resume_incoming,
    );
    resume_peer
        .expect_request("initialize", build_initialize_params("0.1.1"))
        .respond(json!({}));
    resume_peer.expect_notification("initialized");
    resume_peer
        .expect_request(
            "thread/resume",
            json!({
                "threadId": "unavailable-thread",
                "cwd": "/tmp/codex-invalid",
                "approvalPolicy": "untrusted",
                "sandbox": "read-only",
                "model": null,
                "serviceTier": null,
            }),
        )
        .respond_error(json!({ "code": -32000, "message": "permission denied" }));
    let resume_task = tokio::spawn(resume_peer.run());
    assert!(
        resume_runtime
            .start()
            .await
            .expect_err("non-recoverable resume errors must propagate")
            .to_string()
            .contains("permission denied")
    );
    resume_task.await.expect("resume peer");
}

#[tokio::test]
async fn targeted_cancel_interrupts_only_the_verified_active_child_turn() {
    // Mutation caught: reusing the root composer interrupt or dispatching stale child handles.
    let (connection, _protocol_incoming, mut peer) = scripted_peer();
    let runtime = CodexSessionRuntime::new(
        CodexSessionOptions {
            version: "0.1.1".to_owned(),
            thread_id: "fixture-thread".to_owned(),
            cwd: "/tmp/project".to_owned(),
            runtime_mode: CodexRuntimeMode::FullAccess,
            model: Some("gpt-5.3-codex".to_owned()),
            service_tier: None,
            effort: None,
            resume_cursor: None,
        },
        connection,
        _protocol_incoming,
    );
    peer.expect_request("initialize", fixture("initialize-params.json"))
        .respond(json!({}));
    peer.expect_notification("initialized");
    peer.expect_request(
        "thread/start",
        json!({
            "cwd": "/tmp/project",
            "approvalPolicy": "never",
            "sandbox": "danger-full-access",
            "model": "gpt-5.3-codex",
            "serviceTier": null,
        }),
    )
    .respond(json!({
        "thread": {"id": "provider-root"},
        "cwd": "/tmp/project",
        "model": "gpt-5.3-codex"
    }))
    .emit_notification(json!({
        "method": "thread/started",
        "emittedAtMs": 1_000,
        "params": {"thread": {"id": "provider-root"}}
    }));
    peer.expect_request(
        "thread/read",
        json!({"threadId": "provider-root", "includeTurns": false}),
    )
    .respond_history(empty_root_thread_read_result());
    peer.expect_request(
        "thread/list",
        json!({"ancestorThreadId": "provider-root", "limit": 50}),
    )
    .respond(json!({
        "data": [
            {
                "id": "child-thread-1",
                "parentThreadId": "provider-root",
                "createdAt": 2,
                "updatedAt": 3,
                "status": {"type": "active", "activeFlags": []}
            },
            {
                "id": "child-thread-2",
                "parentThreadId": "child-thread-1",
                "createdAt": 3,
                "updatedAt": 4,
                "status": {"type": "active", "activeFlags": []}
            }
        ],
        "nextCursor": null,
        "backwardsCursor": null
    }));
    peer.expect_request(
        "thread/read",
        json!({"threadId": "child-thread-1", "includeTurns": false}),
    )
    .respond_history(json!({
        "thread": {
            "id": "child-thread-1",
            "parentThreadId": "provider-root",
            "createdAt": 2,
            "updatedAt": 3,
            "status": {"type": "active", "activeFlags": []},
            "turns": [{"id": "child-turn-3", "status": "inProgress", "startedAt": 3, "items": []}]
        }
    }));
    peer.expect_request(
        "thread/read",
        json!({"threadId": "child-thread-2", "includeTurns": false}),
    )
    .respond_history(json!({
        "thread": {
            "id": "child-thread-2",
            "parentThreadId": "child-thread-1",
            "createdAt": 3,
            "updatedAt": 4,
            "status": {"type": "active", "activeFlags": []},
            "turns": [{"id": "child-turn-7", "status": "inProgress", "startedAt": 4, "items": []}]
        }
    }));
    peer.expect_request(
        "thread/backgroundTerminals/list",
        json!({"threadId": "provider-root", "limit": 128}),
    )
    .respond(empty_background_terminal_list_result());
    peer.expect_request(
        "turn/interrupt",
        json!({
            "threadId": "child-thread-2",
            "turnId": "child-turn-7"
        }),
    )
    .respond(json!({}))
    .emit_notification(json!({
        "method": "turn/completed",
        "emittedAtMs": 5_000,
        "params": {
            "threadId": "child-thread-2",
            "turn": {"id": "child-turn-7", "status": "completed", "completedAt": 5}
        }
    }));
    peer.expect_request(
        "thread/read",
        json!({"threadId": "provider-root", "includeTurns": false}),
    )
    .respond_history(empty_root_thread_read_result());
    peer.expect_request(
        "thread/list",
        json!({"ancestorThreadId": "provider-root", "limit": 50}),
    )
    .respond(json!({
        "data": [
            {
                "id": "child-thread-1",
                "parentThreadId": "provider-root",
                "createdAt": 2,
                "updatedAt": 3,
                "status": {"type": "active", "activeFlags": []}
            },
            {
                "id": "child-thread-2",
                "parentThreadId": "child-thread-1",
                "createdAt": 3,
                "updatedAt": 4,
                "status": {"type": "idle"}
            }
        ],
        "nextCursor": null,
        "backwardsCursor": null
    }));
    peer.expect_request(
        "thread/backgroundTerminals/list",
        json!({"threadId": "provider-root", "limit": 128}),
    )
    .respond(empty_background_terminal_list_result())
    .emit_notification(json!({
        "method": "turn/started",
        "emittedAtMs": 6_000,
        "params": {
            "threadId": "child-thread-2",
            "turn": {"id": "child-turn-8", "status": "inProgress", "startedAt": 6}
        }
    }));
    peer.expect_request(
        "turn/interrupt",
        json!({
            "threadId": "child-thread-2",
            "turnId": "child-turn-8"
        }),
    )
    .respond_error(json!({
        "code": -32000,
        "message": "provider refused exact child interrupt",
        "data": null
    }));
    peer.expect_request("shutdown", Value::Null)
        .respond(Value::Null);
    let peer_task = tokio::spawn(peer.run());

    runtime.start().await.expect("runtime starts");
    next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:0")
    })
    .await;

    let root_error = runtime
        .interrupt_targeted_turn("provider-root", "root-turn")
        .await
        .expect_err("root thread must never be a targeted child");
    assert!(root_error.to_string().contains("root"));
    let oversized_error = runtime
        .interrupt_targeted_turn(&"x".repeat(257), "child-turn-7")
        .await
        .expect_err("oversized native id must fail closed");
    assert!(oversized_error.to_string().contains("bound"));

    runtime
        .interrupt_targeted_turn("child-thread-2", "child-turn-7")
        .await
        .expect("exact nested child interrupt");
    next_codex_event_matching(&runtime, |event| {
        event.native_event_id.as_deref() == Some("codex:reconciliation:1")
    })
    .await;

    next_codex_event_matching(&runtime, |event| {
        event.event_type == "activity.native"
            && event.native_event_id.as_deref() == Some("codex:activity:2")
    })
    .await;
    let stale_error = runtime
        .interrupt_targeted_turn("child-thread-2", "child-turn-7")
        .await
        .expect_err("naturally completed target must be stale before provider I/O");
    assert!(stale_error.to_string().contains("active"));

    let provider_error = runtime
        .interrupt_targeted_turn("child-thread-2", "child-turn-8")
        .await
        .expect_err("provider error must propagate");
    assert!(
        provider_error
            .to_string()
            .contains("provider refused exact child interrupt")
    );

    runtime.shutdown().await.expect("runtime shuts down");
    peer_task.await.expect("peer");
}

fn codex_invalid_options(resume_cursor: Option<&str>) -> CodexSessionOptions {
    CodexSessionOptions {
        version: "0.1.1".to_owned(),
        thread_id: "codex-invalid-thread".to_owned(),
        cwd: "/tmp/codex-invalid".to_owned(),
        runtime_mode: CodexRuntimeMode::ApprovalRequired,
        model: None,
        service_tier: None,
        effort: None,
        resume_cursor: resume_cursor.map(str::to_owned),
    }
}

fn codex_activity_options(resume_cursor: Option<&str>) -> CodexSessionOptions {
    CodexSessionOptions {
        version: "0.1.1".to_owned(),
        thread_id: "fixture-thread".to_owned(),
        cwd: "/tmp/project".to_owned(),
        runtime_mode: CodexRuntimeMode::FullAccess,
        model: Some("gpt-5.3-codex".to_owned()),
        service_tier: None,
        effort: None,
        resume_cursor: resume_cursor.map(str::to_owned),
    }
}

fn empty_root_thread_read_result() -> Value {
    empty_thread_read_result("provider-root")
}

fn empty_thread_read_result(thread_id: &str) -> Value {
    json!({
        "thread": {
            "id": thread_id,
            "createdAt": 1,
            "updatedAt": 1,
            "status": {"type": "idle"},
            "turns": []
        }
    })
}

fn empty_thread_list_result() -> Value {
    json!({
        "data": [],
        "nextCursor": null,
        "backwardsCursor": null
    })
}

fn empty_background_terminal_list_result() -> Value {
    json!({"data": [], "nextCursor": null})
}

fn codex_edge_turn_params(provider_thread_id: &str, prompt: &str) -> Value {
    build_turn_start_params(&BuildTurnStartInput {
        thread_id: provider_thread_id.to_owned(),
        runtime_mode: CodexRuntimeMode::AutoAcceptEdits,
        client_user_message_id: None,
        prompt: Some(prompt.to_owned()),
        attachments: Vec::new(),
        model: None,
        service_tier: Some("fast".to_owned()),
        effort: Some("high".to_owned()),
        interaction_mode: None,
    })
}

async fn next_codex_event_matching(
    runtime: &CodexSessionRuntime,
    predicate: impl Fn(&RuntimeEvent) -> bool,
) -> RuntimeEvent {
    loop {
        let event = timeout(Duration::from_secs(5), runtime.next_event())
            .await
            .expect("Codex event timeout")
            .expect("Codex event");
        if predicate(&event) {
            return event;
        }
    }
}

fn fixture(name: &str) -> Value {
    serde_json::from_str(
        &std::fs::read_to_string(fixture_directory().join(name)).expect("fixture file"),
    )
    .expect("valid fixture json")
}

fn stable_fixture(name: &str) -> Vec<RuntimeEventStableView> {
    serde_json::from_value(fixture(name)).expect("stable trace fixture")
}

fn fixture_directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/contracts/fixtures/codex-provider")
}

fn scripted_peer() -> (
    JsonRpcConnection,
    mpsc::UnboundedReceiver<IncomingEvent>,
    ScriptedPeer,
) {
    let (runtime_stdout, peer_stdout) = duplex(16 * 1024);
    let (peer_stdin, runtime_stdin) = duplex(16 * 1024);
    let (peer_stderr, runtime_stderr) = duplex(16 * 1024);
    let (connection, incoming) = JsonRpcConnection::spawn(
        runtime_stdout,
        runtime_stdin,
        runtime_stderr,
        ConnectionConfig::default(),
    );
    (
        connection,
        incoming,
        ScriptedPeer::new(peer_stdout, peer_stdin, peer_stderr),
    )
}

struct ScriptedPeer {
    stdout: tokio::io::DuplexStream,
    stdin: tokio::io::DuplexStream,
    stderr: tokio::io::DuplexStream,
    steps: Vec<PeerStep>,
}

impl ScriptedPeer {
    fn new(
        stdout: tokio::io::DuplexStream,
        stdin: tokio::io::DuplexStream,
        stderr: tokio::io::DuplexStream,
    ) -> Self {
        Self {
            stdout,
            stdin,
            stderr,
            steps: Vec::new(),
        }
    }

    fn expect_request(&mut self, method: &str, params: Value) -> &mut PeerStep {
        self.steps.push(PeerStep::ExpectRequest {
            method: method.to_owned(),
            params,
            response: None,
            response_error: None,
            request_seen: None,
            response_gate: None,
            emits: Vec::new(),
            expected_follow_up_response: None,
            stderr_messages: Vec::new(),
            history: None,
        });
        self.steps.last_mut().expect("request step")
    }

    fn expect_notification(&mut self, method: &str) {
        self.steps.push(PeerStep::ExpectNotification {
            method: method.to_owned(),
        });
    }

    async fn run(self) {
        let mut reader = BufReader::new(self.stdin);
        let mut writer = self.stdout;
        let mut stderr = self.stderr;
        let mut pending = None;
        for step in self.steps {
            match step {
                PeerStep::ExpectRequest {
                    method,
                    params,
                    response,
                    response_error,
                    request_seen,
                    response_gate,
                    emits,
                    expected_follow_up_response,
                    stderr_messages,
                    history,
                } => {
                    let message = match pending.take() {
                        Some(message) => message,
                        None => read_scripted_message(&mut reader, &mut writer).await,
                    };
                    assert_eq!(message["method"], method);
                    assert_eq!(message["params"], params);
                    if let Some(request_seen) = request_seen {
                        let _ = request_seen.send(());
                    }
                    if let Some(response_gate) = response_gate {
                        let _ = response_gate.await;
                    }
                    for message in stderr_messages {
                        write_line(&mut stderr, &message).await;
                    }
                    if let Some(result) = response {
                        write_json(
                            &mut writer,
                            json!({
                                "id": message["id"].clone(),
                                "result": result,
                            }),
                        )
                        .await;
                    } else if let Some(error) = response_error {
                        write_json(
                            &mut writer,
                            json!({
                                "id": message["id"].clone(),
                                "error": error,
                            }),
                        )
                        .await;
                    }
                    let mut expected_follow_up_response = expected_follow_up_response;
                    for emit in emits {
                        let requires_response =
                            emit.get("id").is_some() && emit.get("method").is_some();
                        write_json(&mut writer, emit).await;
                        if requires_response
                            && let Some(expected_response) = expected_follow_up_response.take()
                        {
                            let follow_up = read_scripted_message(&mut reader, &mut writer).await;
                            assert_eq!(follow_up, expected_response);
                        }
                    }
                    if let Some(expected_response) = expected_follow_up_response {
                        let follow_up = read_scripted_message(&mut reader, &mut writer).await;
                        assert_eq!(follow_up, expected_response);
                    }
                    if let Some(history) = history
                        && history.pointer("/thread/id") == message["params"].get("threadId")
                    {
                        // Codex serves the thread's turns for as long as the runtime pages them;
                        // the first other message belongs to the next step. A cancelled pass
                        // asks for no pages, so silence ends the history.
                        loop {
                            let Ok(next) = timeout(
                                Duration::from_millis(500),
                                read_scripted_message(&mut reader, &mut writer),
                            )
                            .await
                            else {
                                break;
                            };
                            if next["method"] != "thread/turns/list"
                                || next["params"]["threadId"] != message["params"]["threadId"]
                            {
                                pending = Some(next);
                                break;
                            }
                            write_json(
                                &mut writer,
                                json!({
                                    "id": next["id"].clone(),
                                    "result": thread_turns_page(&next["params"], &history),
                                }),
                            )
                            .await;
                        }
                    }
                }
                PeerStep::ExpectNotification { method } => {
                    let message = match pending.take() {
                        Some(message) => message,
                        None => read_scripted_message(&mut reader, &mut writer).await,
                    };
                    assert_eq!(message["method"], method);
                    assert!(message.get("id").is_none());
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// A metadata-only `thread/read` result: the full result without its turns.
fn thread_history_metadata(full_read_result: &Value) -> Value {
    let mut metadata = full_read_result.clone();
    if let Some(thread) = metadata.get_mut("thread").and_then(Value::as_object_mut) {
        thread.insert("turns".to_owned(), json!([]));
    }
    metadata
}

/// Codex's `thread/turns/list` over a full-history result: newest first, `limit` turns, and a
/// cursor naming the last turn served when more remain.
fn thread_turns_page(params: &Value, full_read_result: &Value) -> Value {
    let keys = params
        .as_object()
        .expect("thread/turns/list params")
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    assert!(
        keys.is_subset(&std::collections::BTreeSet::from([
            "threadId",
            "limit",
            "sortDirection",
            "itemsView",
            "cursor",
        ])),
        "unexpected thread/turns/list params {params}"
    );
    assert_eq!(params["sortDirection"], "desc");
    assert_eq!(params["itemsView"], "full");
    let limit = params["limit"]
        .as_u64()
        .map_or(25, |limit| limit.clamp(1, 100)) as usize;
    let mut turns = full_read_result
        .pointer("/thread/turns")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    turns.reverse();
    if let Some(cursor) = params.get("cursor").and_then(Value::as_str) {
        let anchor = turns
            .iter()
            .position(|turn| turn["id"] == cursor)
            .expect("the cursor names a served turn");
        turns.drain(..=anchor);
    }
    let more = turns.len() > limit;
    turns.truncate(limit);
    let next_cursor = if more {
        turns.last().map(|turn| turn["id"].clone())
    } else {
        None
    };
    json!({"data": turns, "nextCursor": next_cursor, "backwardsCursor": null})
}

/// Serves the `thread/turns/list` pages a sequential peer owes after answering a metadata read of
/// `full_read_result`'s thread.
async fn serve_thread_turns_pages<R, W>(
    reader: &mut BufReader<R>,
    writer: &mut W,
    full_read_result: &Value,
) where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    loop {
        let request = read_scripted_message(reader, writer).await;
        assert_eq!(request["method"], "thread/turns/list");
        assert_eq!(
            request["params"]["threadId"],
            full_read_result["thread"]["id"]
        );
        let page = thread_turns_page(&request["params"], full_read_result);
        let last_page = page["nextCursor"].is_null();
        write_json(writer, json!({"id": request["id"].clone(), "result": page})).await;
        if last_page {
            break;
        }
    }
}

async fn read_scripted_message<R, W>(reader: &mut BufReader<R>, writer: &mut W) -> Value
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    loop {
        let message = read_json_message(reader).await;
        if message["method"] != "mcpServerStatus/list" {
            return message;
        }
        write_json(
            writer,
            json!({
                "id": message["id"].clone(),
                "result": { "data": [], "nextCursor": null },
            }),
        )
        .await;
    }
}

#[allow(clippy::large_enum_variant)]
enum PeerStep {
    ExpectRequest {
        method: String,
        params: Value,
        response: Option<Value>,
        response_error: Option<Value>,
        request_seen: Option<tokio::sync::oneshot::Sender<()>>,
        response_gate: Option<tokio::sync::oneshot::Receiver<()>>,
        emits: Vec<Value>,
        expected_follow_up_response: Option<Value>,
        stderr_messages: Vec<String>,
        /// A full-history `thread/read` result served as Codex pages it: metadata for the
        /// metadata read, then `thread/turns/list` pages.
        history: Option<Value>,
    },
    ExpectNotification {
        method: String,
    },
}

impl PeerStep {
    fn pause_response(
        &mut self,
        request_seen: tokio::sync::oneshot::Sender<()>,
        response_gate: tokio::sync::oneshot::Receiver<()>,
    ) -> &mut Self {
        if let PeerStep::ExpectRequest {
            request_seen: seen,
            response_gate: gate,
            ..
        } = self
        {
            *seen = Some(request_seen);
            *gate = Some(response_gate);
        }
        self
    }

    fn respond(&mut self, result: Value) -> &mut Self {
        if let PeerStep::ExpectRequest { response, .. } = self {
            *response = Some(result);
        }
        self
    }

    /// Answers a metadata `thread/read` from a full-history result and serves the
    /// `thread/turns/list` pages that follow from the same result.
    fn respond_history(&mut self, full_read_result: Value) -> &mut Self {
        if let PeerStep::ExpectRequest {
            response, history, ..
        } = self
        {
            *response = Some(thread_history_metadata(&full_read_result));
            *history = Some(full_read_result);
        }
        self
    }

    fn respond_error(&mut self, error: Value) -> &mut Self {
        if let PeerStep::ExpectRequest { response_error, .. } = self {
            *response_error = Some(error);
        }
        self
    }

    fn emit_notification(&mut self, notification: Value) -> &mut Self {
        if let PeerStep::ExpectRequest { emits, .. } = self {
            emits.push(notification);
        }
        self
    }

    fn emit_request(&mut self, request: Value) -> &mut Self {
        if let PeerStep::ExpectRequest { emits, .. } = self {
            emits.push(request);
        }
        self
    }

    fn expect_response(&mut self, response: Value) -> &mut Self {
        if let PeerStep::ExpectRequest {
            expected_follow_up_response,
            ..
        } = self
        {
            *expected_follow_up_response = Some(response);
        }
        self
    }

    fn emit_stderr(&mut self, message: &str) -> &mut Self {
        if let PeerStep::ExpectRequest {
            stderr_messages, ..
        } = self
        {
            stderr_messages.push(message.to_owned());
        }
        self
    }
}

async fn read_json_message<R>(reader: &mut BufReader<R>) -> Value
where
    R: AsyncRead + Unpin,
{
    let line = timeout(Duration::from_secs(2), async {
        let mut buffer = String::new();
        reader.read_line(&mut buffer).await.expect("read line");
        buffer
    })
    .await
    .expect("message timeout");
    serde_json::from_str(line.trim_end()).expect("valid json line")
}

async fn write_json<W>(writer: &mut W, value: Value)
where
    W: AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;

    writer
        .write_all(format!("{value}\n").as_bytes())
        .await
        .expect("write json");
    writer.flush().await.expect("flush json");
}

async fn write_line<W>(writer: &mut W, value: &str)
where
    W: AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;

    writer
        .write_all(format!("{value}\n").as_bytes())
        .await
        .expect("write line");
    writer.flush().await.expect("flush line");
}
