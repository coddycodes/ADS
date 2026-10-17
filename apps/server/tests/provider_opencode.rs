use bibcode_server::provider::{OptionRefusal, opencode};

use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{OriginalUri, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{delete, get, post},
};
use bibcode_server::activity::{
    ActivityCapabilities, ActivityEntryKind, ActivityHistoryRecovery, ActivityLifecycle,
    ActivityObservationState, ActivityProjection, ActivityRecordKind, ActivityRepository,
    ActivityRosterBucket, ActivityScopeSeed, ActivitySection, ActivitySectionObservationState,
    ProviderActivityMutation,
};
use bibcode_server::persistence::{Database, run_migrations};
use futures_util::{Stream, StreamExt, stream};
use opencode::{
    OpenCodeActivityFixtureAdapter, OpenCodeSessionRuntime, build_inventory_snapshot,
    merge_assistant_text, parse_model_slug,
};
use serde_json::{Value, json};
use tokio::{
    net::TcpListener,
    sync::{Mutex, Notify},
    task::JoinHandle,
    time::{sleep, timeout},
};

fn recent_fixture_timestamp_ms() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock follows the Unix epoch")
            .as_millis(),
    )
    .expect("current Unix timestamp fits in u64")
}

#[test]
fn opencode_helper_outputs_match_fixtures() {
    let inventory_fixture = fixture("inventory-snapshot.json");
    assert_eq!(
        serde_json::to_value(build_inventory_snapshot(
            &inventory_fixture["providerList"],
            inventory_fixture["agents"]
                .get("data")
                .unwrap_or(&inventory_fixture["agents"]),
            &inventory_fixture["commands"],
            &inventory_fixture["customModels"]
                .as_array()
                .expect("custom models")
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>(),
        ))
        .expect("inventory json"),
        inventory_fixture["expected"]
    );
    assert_eq!(
        parse_model_slug("openai/gpt-5.4"),
        Some(("openai".to_owned(), "gpt-5.4".to_owned()))
    );
    assert_eq!(
        merge_assistant_text(Some("Hello"), "Hello world"),
        ("Hello world".to_owned(), " world".to_owned())
    );
}

#[test]
fn activity_tracker_maps_only_verified_children_and_statuses_truthfully() {
    let mut tracker = OpenCodeActivityFixtureAdapter::new("ses-root-activity");
    let discovered = tracker.reconcile_children(
        "ses-root-activity",
        &json!([{
            "id": "ses-child-direct",
            "parentID": "ses-root-activity",
            "title": "Implement review",
            "time": { "created": 1721827200000_i64 }
        }]),
    );
    assert_eq!(discovered.mutations.len(), 1);
    assert!(matches!(
        &discovered.mutations[0],
        ProviderActivityMutation::UpsertActor(actor)
            if actor.status.as_str() == "waiting"
                && actor.id == "opencode:session:ses-child-direct"
    ));

    let busy = tracker.handle_event(&json!({
        "id": "busy-1", "type": "session.status",
        "properties": { "sessionID": "ses-child-direct", "status": { "type": "busy" } }
    }));
    assert!(matches!(
        &busy.mutations[0],
        ProviderActivityMutation::UpsertActor(actor) if actor.status.as_str() == "running"
    ));

    let retry = tracker.handle_event(&json!({
        "id": "retry-1", "type": "session.status",
        "properties": { "sessionID": "ses-child-direct", "status": { "type": "retry", "attempt": 1 } }
    }));
    assert!(matches!(
        &retry.mutations[0],
        ProviderActivityMutation::UpsertActor(actor) if actor.status.as_str() == "waiting"
    ));

    let idle = tracker.handle_event(&json!({
        "id": "idle-1", "type": "session.status",
        "properties": { "sessionID": "ses-child-direct", "status": { "type": "idle" } }
    }));
    assert!(
        idle.mutations.is_empty(),
        "idle preserves the already-waiting lifecycle"
    );

    let foreign = tracker.handle_event(&json!({
        "id": "foreign-1", "type": "session.status",
        "properties": { "sessionID": "ses-foreign", "status": { "type": "busy" } }
    }));
    assert!(foreign.mutations.is_empty());
}

#[test]
fn activity_tracker_keeps_child_graph_and_activity_entries_bounded_and_idempotent() {
    let mut tracker = OpenCodeActivityFixtureAdapter::new("root");
    assert!(
        tracker
            .reconcile_children("unknown", &json!([{"id":"foreign","parentID":"unknown"}]))
            .mutations
            .is_empty()
    );
    tracker.reconcile_children(
        "root",
        &json!([{"id":"child","parentID":"root","time":{"created":1000}}]),
    );
    tracker.reconcile_children(
        "child",
        &json!([{"id":"nested","parentID":"child","time":{"created":1001}}]),
    );
    tracker.handle_event(&json!({"id":"assistant-child","type":"message.updated","properties":{"sessionID":"child","info":{"id":"message","sessionID":"child","role":"assistant"}}}));
    tracker.handle_event(&json!({"id":"assistant-nested","type":"message.updated","properties":{"sessionID":"nested","info":{"id":"message-nested","sessionID":"nested","role":"assistant"}}}));
    assert_eq!(tracker.state_counts().children, 2);
    assert!(
        tracker
            .reconcile_children("child", &json!([{"id":"root","parentID":"child"}]))
            .mutations
            .is_empty()
    );

    let text = json!({"type":"message.part.updated","properties":{"sessionID":"child","part":{"id":"part-text","sessionID":"child","messageID":"message","type":"text","text":"alpha"},"time":10}});
    assert!(
        tracker.handle_event(&text).mutations.is_empty(),
        "text is coalesced before the timer elapses"
    );
    let suffix = json!({"type":"message.part.updated","properties":{"sessionID":"child","part":{"id":"part-text","sessionID":"child","messageID":"message","type":"text","text":"alpha beta"},"time":20}});
    assert!(tracker.handle_event(&suffix).mutations.is_empty());
    let commentary = tracker.flush_text();
    assert!(matches!(
        commentary.mutations.as_slice(),
        [ProviderActivityMutation::AppendEntry(entry)]
            if entry.id == "opencode:part:message:part-text:snapshot:0:10:1a989ea86150171c687b0727f218eedbb94c4665a7da9b0add1bf5de607f2bf1"
                && entry.detail.as_deref() == Some("alpha beta")
    ));
    assert!(
        tracker.handle_event(&text).mutations.is_empty(),
        "older cumulative text must not be replayed"
    );
    assert!(
        tracker.flush_text().mutations.is_empty(),
        "an older cumulative snapshot cannot enqueue a duplicate suffix"
    );

    let reasoning = json!({"type":"message.part.updated","properties":{"sessionID":"child","part":{"id":"reasoning","sessionID":"child","messageID":"message","type":"reasoning","text":"private chain"}}});
    assert!(tracker.handle_event(&reasoning).mutations.is_empty());

    let tool = json!({"type":"message.part.updated","properties":{"sessionID":"child","part":{"id":"tool-part","sessionID":"child","messageID":"message","type":"tool","callID":"call-1","tool":"bash","state":{"status":"completed"}}}});
    let tool_output = tracker.handle_event(&tool);
    assert!(
        matches!(tool_output.mutations.as_slice(), [ProviderActivityMutation::AppendEntry(entry)] if entry.kind == ActivityEntryKind::Tool && entry.id.starts_with("opencode:part:message:tool-part:completed:"))
    );
    assert!(tracker.handle_history("child", &json!([{"info":{"id":"message","sessionID":"child","role":"assistant","time":{"completed":1002}},"parts":[{"id":"tool-part","sessionID":"child","messageID":"message","type":"tool","callID":"call-1","tool":"bash","state":{"status":"completed"}}]}])).mutations.len() == 1, "history supplies terminal actor once but not a duplicate tool entry");
    assert!(tracker.handle_event(&json!({"type":"session.status","properties":{"sessionID":"child","status":{"type":"busy"}}})).mutations.is_empty(), "terminal child cannot reopen");

    let command = tracker.handle_event(&json!({"id":"command-1","type":"command.executed","properties":{"sessionID":"nested","messageID":"message-nested","name":"review","arguments":"--quick"}}));
    assert!(
        matches!(command.mutations.as_slice(), [ProviderActivityMutation::AppendEntry(entry)] if entry.kind == ActivityEntryKind::Command && entry.owner_id == "opencode:session:nested")
    );

    let documented_command = tracker.handle_event_at(
        &json!({
            "id": "command-2",
            "type": "command.executed",
            "properties": {
                "sessionID": "nested",
                "messageID": "message-nested",
                "name": "test",
                "arguments": "--focused",
                "time": 1_721_827_202_000_u64
            }
        }),
        1_721_827_209_000,
    );
    assert!(matches!(
        documented_command.mutations.as_slice(),
        [ProviderActivityMutation::AppendEntry(entry)]
            if entry.created_at == "2024-07-24T13:20:02.000000000Z"
    ));
}

#[test]
fn activity_tracker_rejects_overdeep_lineage_and_keeps_repeated_direct_batches_safe() {
    let mut tracker = OpenCodeActivityFixtureAdapter::new("root");
    let first = tracker.reconcile_children(
        "root",
        &json!([{"id":"one","parentID":"root","time":{"created":1}}]),
    );
    assert_eq!(first.mutations.len(), 1);
    assert!(
        tracker
            .reconcile_children(
                "root",
                &json!([{"id":"one","parentID":"root","time":{"created":1}}])
            )
            .mutations
            .is_empty()
    );

    let mut parent = "one".to_owned();
    for index in 2..=16 {
        let child = format!("child-{index}");
        assert_eq!(
            tracker
                .reconcile_children(
                    &parent,
                    &json!([{"id":child,"parentID":parent,"time":{"created":index}}])
                )
                .mutations
                .len(),
            1
        );
        parent = child;
    }
    assert!(
        tracker
            .reconcile_children(
                &parent,
                &json!([{"id":"too-deep","parentID":parent,"time":{"created":17}}])
            )
            .mutations
            .is_empty()
    );
    assert_eq!(tracker.state_counts().children, 16);
}

#[test]
fn activity_tracker_preserves_whitespace_deltas_and_flushes_at_the_coalesce_boundary() {
    let mut tracker = OpenCodeActivityFixtureAdapter::new("root");
    tracker.reconcile_children(
        "root",
        &json!([{"id":"child","parentID":"root","time":{"created":1}}]),
    );
    tracker.handle_event_at(
        &json!({"id":"assistant","type":"message.updated","properties":{"sessionID":"child","info":{"id":"message","sessionID":"child","role":"assistant"}}}),
        1,
    );
    let delta = json!({"id":"delta-1","type":"message.part.delta","properties":{"sessionID":"child","messageID":"message","partID":"part","field":"text","delta":" leading"}});
    assert!(tracker.handle_event_at(&delta, 10).mutations.is_empty());
    assert!(
        tracker.handle_event_at(&delta, 20).mutations.is_empty(),
        "exact repeated SSE id is idempotent"
    );
    let boundary = tracker.handle_event_at(
        &json!({"id":"delta-2","type":"message.part.delta","properties":{"sessionID":"child","messageID":"message","partID":"part","field":"text","delta":" text"}}),
        110,
    );
    assert!(matches!(
        boundary.mutations.as_slice(),
        [
            ProviderActivityMutation::AppendEntry(first),
            ProviderActivityMutation::AppendEntry(second),
        ] if first.detail.as_deref() == Some(" leading")
            && second.detail.as_deref() == Some(" text")
            && first.id != second.id
    ));
}

#[test]
fn activity_tracker_chooses_latest_terminal_evidence_independent_of_history_order() {
    let mut tracker = OpenCodeActivityFixtureAdapter::new("root");
    tracker.reconcile_children(
        "root",
        &json!([{"id":"child","parentID":"root","time":{"created":1}}]),
    );
    let output = tracker.handle_history("child", &json!([
        {"info":{"id":"complete","sessionID":"child","role":"assistant","time":{"completed":20},"finish":"stop"},"parts":[]},
        {"info":{"id":"failed","sessionID":"child","role":"assistant","time":{"completed":30},"error":{"name":"ProviderError"}},"parts":[]}
    ]));
    assert!(
        matches!(output.mutations.last(), Some(ProviderActivityMutation::UpsertActor(actor)) if actor.status.as_str() == "failed" && actor.terminal_at.as_deref() == Some("1970-01-01T00:00:00.030000000Z"))
    );

    let nullable = tracker.handle_event_at(&json!({"id":"nullable","type":"message.updated","properties":{"sessionID":"child","info":{"id":"null-error","sessionID":"child","role":"assistant","error":null}}}), 40);
    assert!(nullable.mutations.is_empty());
    let cancelled = tracker.handle_event_at(&json!({"id":"cancel","type":"session.error","properties":{"sessionID":"child","error":{"name":"MessageAbortedError"}}}), 50);
    assert!(
        matches!(cancelled.mutations.as_slice(), [ProviderActivityMutation::UpsertActor(actor)] if actor.status.as_str() == "cancelled")
    );
}

#[test]
fn activity_tracker_resolves_history_timestamps_by_source_precedence_and_replays_deterministically()
{
    let history = json!([
        {
            "info": {
                "id": "part-fallback-message",
                "sessionID": "child",
                "role": "assistant",
                "time": {
                    "created": 1_700_000_018_000_u64,
                    "completed": 1_700_000_020_000_u64
                }
            },
            "parts": [{
                "id": "part-fallback",
                "sessionID": "child",
                "messageID": "part-fallback-message",
                "type": "tool",
                "callID": "part-fallback-call",
                "tool": "part-fallback",
                "state": {
                    "status": "completed",
                    "time": {
                        "start": 1_700_000_014_000_u64,
                        "end": u64::MAX
                    }
                }
            }]
        },
        {
            "info": {
                "id": "actor-fallback-message",
                "sessionID": "child",
                "role": "assistant"
            },
            "parts": [{
                "id": "actor-fallback",
                "sessionID": "child",
                "messageID": "actor-fallback-message",
                "type": "tool",
                "callID": "actor-fallback-call",
                "tool": "actor-fallback",
                "state": { "status": "completed" }
            }]
        },
        {
            "info": {
                "id": "message-fallback-message",
                "sessionID": "child",
                "role": "assistant",
                "time": {
                    "created": 1_700_000_010_000_u64,
                    "completed": 1_700_000_012_000_u64
                }
            },
            "parts": [{
                "id": "message-fallback",
                "sessionID": "child",
                "messageID": "message-fallback-message",
                "type": "text",
                "text": "message timestamp fallback",
                "time": {
                    "start": u64::MAX,
                    "end": 0
                }
            }]
        }
    ]);
    let mut tracker = OpenCodeActivityFixtureAdapter::new("root");
    tracker.reconcile_children(
        "root",
        &json!([{
            "id": "child",
            "parentID": "root",
            "time": { "created": 1_700_000_000_000_u64 }
        }]),
    );
    let mut first = tracker.handle_history("child", &history).mutations;
    first.extend(tracker.flush_text().mutations);
    let mut first_entries = first
        .into_iter()
        .filter_map(|mutation| match mutation {
            ProviderActivityMutation::AppendEntry(entry) => Some(entry),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(first_entries.len(), 3);
    let timestamps = first_entries
        .iter()
        .map(|entry| (entry.title.as_str(), entry.created_at.as_str()))
        .collect::<HashMap<_, _>>();
    assert_eq!(
        timestamps.get("part-fallback completed"),
        Some(&"2023-11-14T22:13:34.000000000Z"),
        "an invalid terminal end falls through to the valid tool start"
    );
    assert_eq!(
        timestamps.get("Commentary"),
        Some(&"2023-11-14T22:13:32.000000000Z"),
        "invalid text part timestamps fall through to message completion"
    );
    assert_eq!(
        timestamps.get("actor-fallback completed"),
        Some(&"2023-11-14T22:13:20.000000000Z"),
        "history without part or message time uses the immutable child start"
    );

    let mut replay = tracker.handle_history("child", &history).mutations;
    replay.extend(tracker.flush_text().mutations);
    assert!(
        replay
            .iter()
            .all(|mutation| !matches!(mutation, ProviderActivityMutation::AppendEntry(_))),
        "an identical history replay cannot append timestamp-only duplicates"
    );

    let mut reversed_messages = history.as_array().expect("history array").clone();
    reversed_messages.reverse();
    let mut reversed_tracker = OpenCodeActivityFixtureAdapter::new("root");
    reversed_tracker.reconcile_children(
        "root",
        &json!([{
            "id": "child",
            "parentID": "root",
            "time": { "created": 1_700_000_000_000_u64 }
        }]),
    );
    let mut reversed = reversed_tracker
        .handle_history("child", &Value::Array(reversed_messages))
        .mutations;
    reversed.extend(reversed_tracker.flush_text().mutations);
    let mut reversed_entries = reversed
        .into_iter()
        .filter_map(|mutation| match mutation {
            ProviderActivityMutation::AppendEntry(entry) => Some(entry),
            _ => None,
        })
        .collect::<Vec<_>>();
    first_entries.sort_by(|left, right| left.id.cmp(&right.id));
    reversed_entries.sort_by(|left, right| left.id.cmp(&right.id));
    assert_eq!(
        first_entries
            .iter()
            .map(|entry| (&entry.id, &entry.created_at))
            .collect::<Vec<_>>(),
        reversed_entries
            .iter()
            .map(|entry| (&entry.id, &entry.created_at))
            .collect::<Vec<_>>(),
        "history order cannot change stable entry identities or chronology"
    );
}

#[test]
fn activity_tracker_live_parts_prefer_documented_times_and_keep_legacy_event_time() {
    let mut tracker = OpenCodeActivityFixtureAdapter::new("root");
    tracker.reconcile_children(
        "root",
        &json!([{
            "id": "child",
            "parentID": "root",
            "time": { "created": 1_700_000_000_000_u64 }
        }]),
    );
    tracker.handle_event(&json!({
        "type": "message.updated",
        "properties": {
            "sessionID": "child",
            "info": {
                "id": "message",
                "sessionID": "child",
                "role": "assistant"
            }
        }
    }));

    let text = json!({
        "type": "message.part.updated",
        "properties": {
            "sessionID": "child",
            "time": 1_700_000_099_000_u64,
            "part": {
                "id": "text",
                "sessionID": "child",
                "messageID": "message",
                "type": "text",
                "text": "live timestamped text",
                "time": {
                    "start": 1_700_000_002_000_u64,
                    "end": 1_700_000_003_000_u64
                }
            }
        }
    });
    assert!(tracker.handle_event(&text).mutations.is_empty());
    let text_entry = match tracker.flush_text().mutations.as_slice() {
        [ProviderActivityMutation::AppendEntry(entry)] => entry.clone(),
        other => panic!("expected one timestamped text entry, got {other:?}"),
    };
    assert_eq!(text_entry.created_at, "2023-11-14T22:13:23.000000000Z");

    let tool = json!({
        "type": "message.part.updated",
        "properties": {
            "sessionID": "child",
            "time": 1_700_000_098_000_u64,
            "part": {
                "id": "tool",
                "sessionID": "child",
                "messageID": "message",
                "type": "tool",
                "callID": "tool-call",
                "tool": "bash",
                "state": {
                    "status": "completed",
                    "time": {
                        "start": 1_700_000_004_000_u64,
                        "end": 1_700_000_005_000_u64
                    }
                }
            }
        }
    });
    let tool_entry = match tracker.handle_event(&tool).mutations.as_slice() {
        [ProviderActivityMutation::AppendEntry(entry)] => entry.clone(),
        other => panic!("expected one timestamped tool entry, got {other:?}"),
    };
    assert_eq!(tool_entry.created_at, "2023-11-14T22:13:25.000000000Z");

    let legacy = json!({
        "type": "message.part.updated",
        "properties": {
            "sessionID": "child",
            "time": 1_700_000_040_000_u64,
            "part": {
                "id": "legacy-tool",
                "sessionID": "child",
                "messageID": "message",
                "type": "tool",
                "callID": "legacy-call",
                "tool": "legacy",
                "state": { "status": "completed" }
            }
        }
    });
    let legacy_entry = match tracker.handle_event(&legacy).mutations.as_slice() {
        [ProviderActivityMutation::AppendEntry(entry)] => entry.clone(),
        other => panic!("expected one legacy-timestamp tool entry, got {other:?}"),
    };
    assert_eq!(legacy_entry.created_at, "2023-11-14T22:14:00.000000000Z");

    let actor_fallback = json!({
        "type": "message.part.updated",
        "properties": {
            "sessionID": "child",
            "part": {
                "id": "actor-tool",
                "sessionID": "child",
                "messageID": "message",
                "type": "tool",
                "callID": "actor-call",
                "tool": "actor",
                "state": { "status": "completed" }
            }
        }
    });
    let actor_entry = match tracker.handle_event(&actor_fallback).mutations.as_slice() {
        [ProviderActivityMutation::AppendEntry(entry)] => entry.clone(),
        other => panic!("expected one actor-fallback tool entry, got {other:?}"),
    };
    assert_eq!(actor_entry.created_at, "2023-11-14T22:13:20.000000000Z");
    assert!(tracker.handle_event(&tool).mutations.is_empty());
    assert!(tracker.handle_event(&legacy).mutations.is_empty());
    assert!(tracker.handle_event(&actor_fallback).mutations.is_empty());
}

#[test]
fn activity_tracker_uses_created_time_for_the_fixture_shaped_assistant_error() {
    let mut tracker = OpenCodeActivityFixtureAdapter::new("root");
    tracker.reconcile_children(
        "root",
        &json!([{"id":"child","parentID":"root","time":{"created":1}}]),
    );
    let output = tracker.handle_history("child", &json!([
        {"info":{"id":"complete","sessionID":"child","role":"assistant","time":{"completed":20},"finish":"stop"},"parts":[]},
        {"info":{"id":"failed","sessionID":"child","role":"assistant","time":{"created":30},"error":{"name":"ProviderError","message":"fixture shape"}},"parts":[]}
    ]));
    assert!(
        matches!(output.mutations.last(), Some(ProviderActivityMutation::UpsertActor(actor)) if actor.status.as_str() == "failed" && actor.terminal_at.as_deref() == Some("1970-01-01T00:00:00.030000000Z"))
    );
}

#[test]
fn activity_tracker_advances_later_same_terminal_evidence_before_intermediate_completion() {
    let mut tracker = OpenCodeActivityFixtureAdapter::new("root");
    tracker.reconcile_children(
        "root",
        &json!([{"id":"child","parentID":"root","time":{"created":1}}]),
    );
    let output = tracker.handle_history("child", &json!([
        {"info":{"id":"failed-early","sessionID":"child","role":"assistant","time":{"completed":10},"error":{"name":"ProviderError"}},"parts":[]},
        {"info":{"id":"failed-late","sessionID":"child","role":"assistant","time":{"completed":20},"error":{"name":"ProviderError"}},"parts":[]},
        {"info":{"id":"complete-middle","sessionID":"child","role":"assistant","time":{"completed":15},"finish":"stop"},"parts":[]}
    ]));
    assert!(
        matches!(output.mutations.last(), Some(ProviderActivityMutation::UpsertActor(actor)) if actor.status.as_str() == "failed" && actor.terminal_at.as_deref() == Some("1970-01-01T00:00:00.020000000Z"))
    );
}

#[test]
fn activity_tracker_quarantines_child_before_parent_and_rejects_user_parts() {
    let mut tracker = OpenCodeActivityFixtureAdapter::new("root");
    assert!(
        tracker
            .reconcile_children(
                "parent",
                &json!([{"id":"nested","parentID":"parent","time":{"created":2}}])
            )
            .mutations
            .is_empty()
    );
    let admitted = tracker.reconcile_children(
        "root",
        &json!([{"id":"parent","parentID":"root","time":{"created":1}}]),
    );
    assert_eq!(
        admitted.mutations.len(),
        2,
        "parent admission promotes its verified quarantined child"
    );
    assert_eq!(tracker.state_counts().children, 2);
    let user_part = tracker.handle_event(&json!({"id":"user-part","type":"message.part.updated","properties":{"sessionID":"parent","part":{"id":"part","sessionID":"parent","messageID":"user-message","type":"text","text":"must not map"}}}));
    assert!(user_part.mutations.is_empty());
}

#[test]
fn activity_tracker_promotes_a_full_quarantined_lineage_to_a_fixed_point() {
    let mut tracker = OpenCodeActivityFixtureAdapter::new("root");
    tracker.reconcile_children(
        "middle",
        &json!([{"id":"leaf","parentID":"middle","time":{"created":3}}]),
    );
    tracker.reconcile_children(
        "direct",
        &json!([{"id":"middle","parentID":"direct","time":{"created":2}}]),
    );
    let output = tracker.reconcile_children(
        "root",
        &json!([{"id":"direct","parentID":"root","time":{"created":1}}]),
    );
    assert_eq!(output.mutations.len(), 3);
    assert_eq!(tracker.state_counts().children, 3);
}

#[test]
fn activity_tracker_chunks_oversized_text_without_exceeding_activity_bounds() {
    let mut tracker = OpenCodeActivityFixtureAdapter::new("root");
    tracker.reconcile_children(
        "root",
        &json!([{"id":"child","parentID":"root","time":{"created":1}}]),
    );
    tracker.handle_event(&json!({"id":"assistant","type":"message.updated","properties":{"sessionID":"child","info":{"id":"message","sessionID":"child","role":"assistant"}}}));
    let payload = "x".repeat(20_000);
    let output = tracker.handle_event_at(&json!({"id":"large","type":"message.part.delta","properties":{"sessionID":"child","messageID":"message","partID":"part","field":"text","delta":payload}}), 100);
    let flushed = tracker.flush_text();
    let details = output
        .mutations
        .iter()
        .chain(flushed.mutations.iter())
        .filter_map(|mutation| match mutation {
            ProviderActivityMutation::AppendEntry(entry) => entry.detail.as_deref(),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        details.as_slice(),
        [detail]
            if detail.len() == 16_384
                && detail.ends_with("[truncated; recover from history]")
    ));
}

#[test]
fn activity_tracker_bounds_history_before_stateful_handlers_and_leaves_the_tail_retryable() {
    let mut tracker = OpenCodeActivityFixtureAdapter::new("root");
    tracker.reconcile_children(
        "root",
        &json!([{"id":"child","parentID":"root","time":{"created":1}}]),
    );
    let messages = (0..10_000)
        .map(|index| {
            json!({
                "info": {
                    "id": format!("message-{index}"),
                    "sessionID": "child",
                    "role": "assistant"
                },
                "parts": [{
                    "id": format!("part-{index}"),
                    "sessionID": "child",
                    "messageID": format!("message-{index}"),
                    "type": "tool",
                    "callID": format!("call-{index}"),
                    "tool": "bash",
                    "state": { "status": "completed" }
                }]
            })
        })
        .collect::<Vec<_>>();

    let bounded = tracker.handle_history("child", &Value::Array(messages.clone()));
    assert_eq!(
        bounded.mutations.len(),
        200,
        "one history pass admits at most the documented 200 messages/parts",
    );
    assert!(matches!(
        bounded.mutations.last(),
        Some(ProviderActivityMutation::AppendEntry(entry))
            if entry.id.contains("message-199")
    ));
    assert_eq!(
        tracker.state_counts().seen_entries,
        200,
        "the ignored multi-megabyte tail must not consume seen-state capacity",
    );

    let retry = tracker.handle_history("child", &json!([messages[200].clone()]));
    assert!(matches!(
        retry.mutations.as_slice(),
        [ProviderActivityMutation::AppendEntry(entry)]
            if entry.id.contains("message-200")
    ));
}

#[test]
fn activity_tracker_defers_the_257th_pending_delta_without_seen_state_poisoning() {
    let mut tracker = OpenCodeActivityFixtureAdapter::new("root");
    tracker.reconcile_children(
        "root",
        &json!([{"id":"child","parentID":"root","time":{"created":1}}]),
    );
    tracker.handle_event(&json!({
        "id":"assistant",
        "type":"message.updated",
        "properties":{
            "sessionID":"child",
            "info":{"id":"message","sessionID":"child","role":"assistant"}
        }
    }));
    for index in 0..256 {
        let output = tracker.handle_event_at(
            &json!({
                "id":format!("delta-{index}"),
                "type":"message.part.delta",
                "properties":{
                    "sessionID":"child",
                    "messageID":"message",
                    "partID":"part",
                    "field":"text",
                    "delta":"x"
                }
            }),
            10,
        );
        assert!(output.mutations.is_empty());
    }
    let deferred = json!({
        "id":"delta-deferred",
        "type":"message.part.delta",
        "properties":{
            "sessionID":"child",
            "messageID":"message",
            "partID":"part",
            "field":"text",
            "delta":"tail"
        }
    });
    assert_eq!(
        tracker.handle_event_at(&deferred, 10).mutations.len(),
        256,
        "the full batch is accepted and the next event remains deferred",
    );
    assert!(
        tracker.handle_event_at(&deferred, 10).mutations.is_empty(),
        "the deferred event is still admissible on retry",
    );
    assert!(matches!(
        tracker.flush_text().mutations.as_slice(),
        [ProviderActivityMutation::AppendEntry(entry)]
            if entry.detail.as_deref() == Some("tail")
    ));
}

#[test]
fn activity_tracker_truncates_a_huge_delta_once_with_explicit_recovery_evidence() {
    let mut tracker = OpenCodeActivityFixtureAdapter::new("root");
    tracker.reconcile_children(
        "root",
        &json!([{"id":"child","parentID":"root","time":{"created":1}}]),
    );
    tracker.handle_event(&json!({
        "id":"assistant",
        "type":"message.updated",
        "properties":{
            "sessionID":"child",
            "info":{"id":"message","sessionID":"child","role":"assistant"}
        }
    }));
    let huge = "x".repeat(4 * 1024 * 1024);
    let immediate = tracker.handle_event_at(
        &json!({
            "id":"huge-delta",
            "type":"message.part.delta",
            "properties":{
                "sessionID":"child",
                "messageID":"message",
                "partID":"part",
                "field":"text",
                "delta":huge
            }
        }),
        10,
    );
    assert!(immediate.mutations.is_empty());

    let flushed = tracker.flush_text();
    assert!(matches!(
        flushed.mutations.as_slice(),
        [ProviderActivityMutation::AppendEntry(entry)]
            if entry.detail.as_ref().is_some_and(|detail| {
                detail.len() <= 16_384 && detail.ends_with("[truncated; recover from history]")
            })
    ));

    let huge_snapshot = "y".repeat(4 * 1024 * 1024);
    assert!(
        tracker
            .handle_history(
                "child",
                &json!([{
                    "info":{
                        "id":"message",
                        "sessionID":"child",
                        "role":"assistant"
                    },
                    "parts":[{
                        "id":"history-part",
                        "sessionID":"child",
                        "messageID":"message",
                        "type":"text",
                        "text":huge_snapshot
                    }]
                }]),
            )
            .mutations
            .is_empty()
    );
    assert!(matches!(
        tracker.flush_text().mutations.as_slice(),
        [ProviderActivityMutation::AppendEntry(entry)]
            if entry.detail.as_ref().is_some_and(|detail| {
                detail.len() <= 16_384 && detail.ends_with("[truncated; recover from history]")
            })
    ));
}

#[test]
fn activity_tracker_retains_same_stream_coverage_past_seen_cache_capacity() {
    let mut tracker = OpenCodeActivityFixtureAdapter::new("root");
    tracker.reconcile_children(
        "root",
        &json!([{"id":"child","parentID":"root","time":{"created":1}}]),
    );
    tracker.handle_event(&json!({
        "id":"assistant",
        "type":"message.updated",
        "properties":{
            "sessionID":"child",
            "info":{"id":"message","sessionID":"child","role":"assistant"}
        }
    }));
    let mut cumulative = String::new();
    let mut emitted_ids = Vec::new();

    for index in 0..2_050_u64 {
        let delta = char::from(b'a' + u8::try_from(index % 26).unwrap()).to_string();
        cumulative.push_str(&delta);
        let output = tracker.handle_event_at(
            &json!({
                "id": format!("same-stream-{index}"),
                "type": "message.part.delta",
                "properties": {
                    "sessionID": "child",
                    "messageID": "message",
                    "partID": "part",
                    "field": "text",
                    "delta": delta
                }
            }),
            index.saturating_mul(101),
        );
        for batch in [output, tracker.flush_text()] {
            emitted_ids.extend(
                batch
                    .mutations
                    .into_iter()
                    .filter_map(|mutation| match mutation {
                        ProviderActivityMutation::AppendEntry(entry) => Some(entry.id),
                        _ => None,
                    }),
            );
        }
    }

    assert_eq!(emitted_ids.len(), 2_050);
    let snapshot = tracker.handle_event_at(
        &json!({
            "id":"snapshot",
            "type":"message.part.updated",
            "properties":{
                "sessionID":"child",
                "part":{
                    "id":"part",
                    "sessionID":"child",
                    "messageID":"message",
                    "type":"text",
                    "text":cumulative
                }
            }
        }),
        300_000,
    );
    assert!(snapshot.mutations.is_empty());
    assert!(tracker.flush_text().mutations.is_empty());
}

#[test]
fn activity_tracker_marks_true_live_coverage_saturation_without_snapshot_echo() {
    let mut tracker = OpenCodeActivityFixtureAdapter::new("root");
    tracker.reconcile_children(
        "root",
        &json!([{"id":"child","parentID":"root","time":{"created":1}}]),
    );
    tracker.handle_event(&json!({
        "id":"assistant",
        "type":"message.updated",
        "properties":{
            "sessionID":"child",
            "info":{"id":"message","sessionID":"child","role":"assistant"}
        }
    }));
    let baseline = "b".repeat(16_384);
    assert!(
        tracker
            .handle_event_at(
                &json!({
                    "id":"baseline-snapshot",
                    "type":"message.part.updated",
                    "properties":{
                        "sessionID":"child",
                        "part":{
                            "id":"part",
                            "sessionID":"child",
                            "messageID":"message",
                            "type":"text",
                            "text":baseline
                        }
                    }
                }),
                1,
            )
            .mutations
            .is_empty()
    );
    assert_eq!(tracker.flush_text().mutations.len(), 1);

    let mut cumulative = baseline;
    let mut live_entry_count = 0;
    for index in 0..=16_384_u64 {
        cumulative.push('x');
        let output = tracker.handle_event_at(
            &json!({
                "id":format!("saturated-{index}"),
                "type":"message.part.delta",
                "properties":{
                    "sessionID":"child",
                    "messageID":"message",
                    "partID":"part",
                    "field":"text",
                    "delta":"x"
                }
            }),
            index.saturating_mul(101),
        );
        live_entry_count += output
            .mutations
            .iter()
            .filter(|mutation| matches!(mutation, ProviderActivityMutation::AppendEntry(_)))
            .count();
    }
    live_entry_count += tracker.flush_text().mutations.len();
    assert_eq!(live_entry_count, 16_385);

    let snapshot_event = json!({
        "id":"saturated-snapshot",
        "type":"message.part.updated",
        "properties":{
            "sessionID":"child",
            "part":{
                "id":"part",
                "sessionID":"child",
                "messageID":"message",
                "type":"text",
                "text":cumulative
            }
        }
    });
    let snapshot = tracker.handle_event_at(&snapshot_event, 1_700_000);
    let snapshot_and_flush = [snapshot, tracker.flush_text()];
    let marker_entries = snapshot_and_flush
        .into_iter()
        .flat_map(|output| output.mutations)
        .filter_map(|mutation| match mutation {
            ProviderActivityMutation::AppendEntry(entry) => Some(entry),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(marker_entries.len(), 1);
    assert_eq!(
        marker_entries[0].id,
        "opencode:part:message:part:coverage-saturated:32769:e33f24140499430b048f6600af4f41f3ccb0cb766d9f7661124cf8ba4b827523",
    );
    assert_eq!(
        marker_entries[0].detail.as_deref(),
        Some("[truncated; recover from history]"),
    );
    assert_ne!(
        marker_entries[0].detail.as_deref(),
        Some(cumulative.as_str())
    );

    assert!(
        tracker
            .handle_event_at(&snapshot_event, 1_700_001)
            .mutations
            .is_empty()
    );
    assert!(
        tracker.flush_text().mutations.is_empty(),
        "the same authoritative snapshot identity must not produce a distinct marker",
    );

    let recovery_text = " é";
    let recovery_delta = json!({
        "id":"after-saturation",
        "type":"message.part.delta",
        "properties":{
            "sessionID":"child",
            "messageID":"message",
            "partID":"part",
            "field":"text",
            "delta":recovery_text
        }
    });
    assert!(
        tracker
            .handle_event_at(&recovery_delta, 1_700_100)
            .mutations
            .is_empty()
    );
    assert!(matches!(
        tracker.flush_text().mutations.as_slice(),
        [ProviderActivityMutation::AppendEntry(entry)]
            if entry.detail.as_deref() == Some(recovery_text)
    ));
    cumulative.push_str(recovery_text);
    let recovered_snapshot = json!({
        "id":"after-saturation-snapshot",
        "type":"message.part.updated",
        "properties":{
            "sessionID":"child",
            "part":{
                "id":"part",
                "sessionID":"child",
                "messageID":"message",
                "type":"text",
                "text":cumulative
            }
        }
    });
    assert!(
        tracker
            .handle_event_at(&recovered_snapshot, 1_700_150)
            .mutations
            .is_empty()
            && tracker.flush_text().mutations.is_empty(),
        "the UTF-8 append is already covered by its live provider delta",
    );
    assert!(
        tracker
            .handle_event_at(&recovery_delta, 1_700_200)
            .mutations
            .is_empty()
            && tracker.flush_text().mutations.is_empty(),
        "the stable provider event remains idempotent after saturation recovery",
    );
}

#[test]
fn activity_tracker_replays_evicted_delta_with_stable_identity_without_corrupting_history() {
    let mut tracker = OpenCodeActivityFixtureAdapter::new("root");
    tracker.reconcile_children(
        "root",
        &json!([{"id":"child","parentID":"root","time":{"created":1}}]),
    );
    tracker.handle_event(&json!({
        "id":"assistant",
        "type":"message.updated",
        "properties":{
            "sessionID":"child",
            "info":{"id":"message","sessionID":"child","role":"assistant"}
        }
    }));

    let first_event = json!({
        "id":"delta-0",
        "type":"message.part.delta",
        "properties":{
            "sessionID":"child",
            "messageID":"message",
            "partID":"part",
            "field":"text",
            "delta":"x"
        }
    });
    assert!(
        tracker
            .handle_event_at(&first_event, 10)
            .mutations
            .is_empty()
    );
    let first = tracker.flush_text();
    let first_entry = match first.mutations.as_slice() {
        [ProviderActivityMutation::AppendEntry(entry)] => entry.clone(),
        other => panic!("expected one first delta entry, got {other:?}"),
    };

    let mut second_entry_id = None;
    for index in 1..=2_048 {
        let output = tracker.handle_event_at(
            &json!({
                "id":format!("delta-{index}"),
                "type":"message.part.delta",
                "properties":{
                    "sessionID":"child",
                    "messageID":"message",
                    "partID":"part",
                    "field":"text",
                    "delta":"x"
                }
            }),
            10,
        );
        assert!(output.mutations.is_empty());
        let flushed = tracker.flush_text();
        if index == 1 {
            second_entry_id = flushed
                .mutations
                .first()
                .and_then(|mutation| match mutation {
                    ProviderActivityMutation::AppendEntry(entry) => Some(entry.id.clone()),
                    _ => None,
                });
        }
    }
    assert_ne!(
        second_entry_id.as_deref(),
        Some(first_entry.id.as_str()),
        "distinct identical deltas keep distinct provider identities",
    );

    assert!(
        tracker
            .handle_event_at(&first_event, 10)
            .mutations
            .is_empty()
    );
    let replay = tracker.flush_text();
    assert!(matches!(
        replay.mutations.as_slice(),
        [ProviderActivityMutation::AppendEntry(entry)]
            if entry.id == first_entry.id && entry.detail == first_entry.detail
    ));

    let snapshot = "x".repeat(2_049);
    let history = tracker.handle_history(
        "child",
        &json!([{
            "info":{"id":"message","sessionID":"child","role":"assistant"},
            "parts":[{
                "id":"part",
                "sessionID":"child",
                "messageID":"message",
                "type":"text",
                "text":snapshot
            }]
        }]),
    );
    assert!(
        history.mutations.is_empty() && tracker.flush_text().mutations.is_empty(),
        "the authoritative cumulative snapshot is already covered by live deltas",
    );
}

#[test]
fn activity_tracker_matches_new_delta_after_stale_replay_and_unchanged_snapshot() {
    let mut tracker = OpenCodeActivityFixtureAdapter::new("root");
    tracker.reconcile_children(
        "root",
        &json!([{"id":"child","parentID":"root","time":{"created":1}}]),
    );
    tracker.handle_event(&json!({
        "id":"assistant",
        "type":"message.updated",
        "properties":{
            "sessionID":"child",
            "info":{"id":"message","sessionID":"child","role":"assistant"}
        }
    }));
    let snapshot = |text: &str| {
        json!({
            "id":"snapshot",
            "type":"message.part.updated",
            "properties":{
                "sessionID":"child",
                "part":{
                    "id":"part",
                    "sessionID":"child",
                    "messageID":"message",
                    "type":"text",
                    "text":text
                }
            }
        })
    };
    let delta = |id: &str, part_id: &str, text: &str| {
        json!({
            "id":id,
            "type":"message.part.delta",
            "properties":{
                "sessionID":"child",
                "messageID":"message",
                "partID":part_id,
                "field":"text",
                "delta":text
            }
        })
    };

    assert!(tracker.handle_event(&snapshot("base")).mutations.is_empty());
    assert_eq!(tracker.flush_text().mutations.len(), 1);
    let old = delta("delta-old", "part", " old");
    assert!(tracker.handle_event_at(&old, 10).mutations.is_empty());
    let old_entry = match tracker.flush_text().mutations.as_slice() {
        [ProviderActivityMutation::AppendEntry(entry)] => entry.clone(),
        other => panic!("expected the original old delta entry, got {other:?}"),
    };
    assert!(
        tracker
            .handle_event(&snapshot("base old"))
            .mutations
            .is_empty()
            && tracker.flush_text().mutations.is_empty(),
        "the first authoritative snapshot covers the original live delta",
    );

    for index in 0..2_048 {
        assert!(
            tracker
                .handle_event_at(&delta(&format!("evict-{index}"), "eviction-part", "z"), 10,)
                .mutations
                .is_empty()
        );
        assert_eq!(tracker.flush_text().mutations.len(), 1);
    }

    assert!(tracker.handle_event_at(&old, 10).mutations.is_empty());
    assert!(matches!(
        tracker.flush_text().mutations.as_slice(),
        [ProviderActivityMutation::AppendEntry(entry)]
            if entry.id == old_entry.id && entry.detail == old_entry.detail
    ));
    assert!(
        tracker
            .handle_event(&snapshot("base old"))
            .mutations
            .is_empty()
            && tracker.flush_text().mutations.is_empty(),
        "an unchanged authoritative snapshot must not move the cumulative baseline",
    );

    let new = delta("delta-new", "part", " new");
    assert!(tracker.handle_event_at(&new, 10).mutations.is_empty());
    let new_entry = match tracker.flush_text().mutations.as_slice() {
        [ProviderActivityMutation::AppendEntry(entry)] => entry.clone(),
        other => panic!("expected the new delta entry, got {other:?}"),
    };
    assert_ne!(new_entry.id, old_entry.id);
    assert_eq!(new_entry.detail.as_deref(), Some(" new"));
    assert!(
        tracker
            .handle_event(&snapshot("base old new"))
            .mutations
            .is_empty()
            && tracker.flush_text().mutations.is_empty(),
        "the updated snapshot must match the newest real delta around stale replay noise",
    );
}

#[test]
fn activity_tracker_matches_newest_equal_text_delta_without_collapsing_event_identity() {
    let mut tracker = OpenCodeActivityFixtureAdapter::new("root");
    tracker.reconcile_children(
        "root",
        &json!([{"id":"child","parentID":"root","time":{"created":1}}]),
    );
    tracker.handle_event(&json!({
        "id":"assistant",
        "type":"message.updated",
        "properties":{
            "sessionID":"child",
            "info":{"id":"message","sessionID":"child","role":"assistant"}
        }
    }));
    let snapshot = |text: &str| {
        json!({
            "id":"snapshot",
            "type":"message.part.updated",
            "properties":{
                "sessionID":"child",
                "part":{
                    "id":"part",
                    "sessionID":"child",
                    "messageID":"message",
                    "type":"text",
                    "text":text
                }
            }
        })
    };
    let delta = |id: &str, part_id: &str, text: &str| {
        json!({
            "id":id,
            "type":"message.part.delta",
            "properties":{
                "sessionID":"child",
                "messageID":"message",
                "partID":part_id,
                "field":"text",
                "delta":text
            }
        })
    };

    tracker.handle_event(&snapshot("base"));
    tracker.flush_text();
    let old = delta("delta-old-equal", "part", "x");
    tracker.handle_event_at(&old, 10);
    let old_entry = match tracker.flush_text().mutations.as_slice() {
        [ProviderActivityMutation::AppendEntry(entry)] => entry.clone(),
        other => panic!("expected the original equal-text delta, got {other:?}"),
    };
    tracker.handle_event(&snapshot("basex"));
    assert!(tracker.flush_text().mutations.is_empty());
    for index in 0..2_048 {
        tracker.handle_event_at(
            &delta(&format!("equal-evict-{index}"), "eviction-part", "z"),
            10,
        );
        tracker.flush_text();
    }

    tracker.handle_event_at(&old, 10);
    assert!(matches!(
        tracker.flush_text().mutations.as_slice(),
        [ProviderActivityMutation::AppendEntry(entry)]
            if entry.id == old_entry.id
    ));
    tracker.handle_event(&snapshot("basex"));
    assert!(tracker.flush_text().mutations.is_empty());

    let newest = delta("delta-new-equal", "part", "x");
    tracker.handle_event_at(&newest, 10);
    let newest_entry = match tracker.flush_text().mutations.as_slice() {
        [ProviderActivityMutation::AppendEntry(entry)] => entry.clone(),
        other => panic!("expected the newest equal-text delta, got {other:?}"),
    };
    assert_eq!(old_entry.detail, newest_entry.detail);
    assert_ne!(
        old_entry.id, newest_entry.id,
        "equal text from distinct SSE IDs must retain distinct repository identities",
    );
    let trailing = delta("delta-new-trailing", "part", "y");
    tracker.handle_event_at(&trailing, 10);
    let trailing_entry = match tracker.flush_text().mutations.as_slice() {
        [ProviderActivityMutation::AppendEntry(entry)] => entry.clone(),
        other => panic!("expected the trailing real delta, got {other:?}"),
    };
    assert_ne!(newest_entry.id, trailing_entry.id);
    assert!(
        tracker
            .handle_event(&snapshot("basexxy"))
            .mutations
            .is_empty()
            && tracker.flush_text().mutations.is_empty(),
        "the updated snapshot must cover the newest equal-text event and its trailing delta, not stale replay noise",
    );
}

#[test]
fn activity_tracker_maps_the_versioned_child_sse_fixture_after_registry_verification() {
    let sessions = activity_fixture("trace-child-sessions.json");
    let mut tracker = OpenCodeActivityFixtureAdapter::new("ses-root-activity");
    for response in sessions["childrenResponses"]
        .as_array()
        .expect("child batches")
    {
        tracker.reconcile_children(
            response["parentSessionID"].as_str().expect("parent"),
            &response["response"],
        );
    }
    let sse = activity_fixture("trace-child-sse.json");
    let mut mutations = Vec::new();
    for frame in raw_sse_frames(&sse) {
        mutations.extend(tracker.handle_event_at(&frame, 1_721_827_200_000).mutations);
    }
    mutations.extend(tracker.flush_text().mutations);
    assert!(mutations.iter().any(|mutation| matches!(mutation, ProviderActivityMutation::AppendEntry(entry) if entry.kind == ActivityEntryKind::Commentary && entry.detail.as_deref() == Some("[redacted child commentary]"))));
    assert!(mutations.iter().any(|mutation| matches!(mutation, ProviderActivityMutation::AppendEntry(entry) if entry.kind == ActivityEntryKind::Command && entry.owner_id == "opencode:session:ses-child-direct")));
}

#[test]
fn opencode_activity_fixture_manifest_rejects_missing_versioned_child_session_captures() {
    let fixture_names = fixture_names_from_manifest();
    assert_eq!(
        fixture_names,
        vec![
            "inventory-snapshot.json",
            "rollback.json",
            "trace-question-resolved.json",
            "trace-session.json",
            "trace-child-sessions.json",
            "trace-child-sse.json",
            "trace-child-history.json",
            "trace-foreign-session.json",
            "trace-reconnect.json",
        ],
        "the manifest is the fixture loader contract for every OpenCode child-activity capture",
    );

    for fixture_name in fixture_names.iter().filter(|name| {
        matches!(
            name.as_str(),
            "trace-child-sessions.json"
                | "trace-child-sse.json"
                | "trace-child-history.json"
                | "trace-foreign-session.json"
                | "trace-reconnect.json"
        )
    }) {
        assert_opencode_1184_metadata(&activity_fixture(fixture_name));
    }
}

#[test]
fn opencode_activity_fixture_rejects_non_recursive_or_unattributed_child_lineage() {
    let fixture = activity_fixture("trace-child-sessions.json");
    assert_opencode_1184_metadata(&fixture);
    assert_eq!(fixture["rootSessionID"], "ses-root-activity");

    let responses = fixture["childrenResponses"]
        .as_array()
        .expect("children responses");
    assert_eq!(responses.len(), 3, "root, direct child, then nested child");

    let root_children = children_response(responses, "ses-root-activity");
    assert_eq!(root_children.len(), 1, "children is a direct-only endpoint");
    assert_eq!(root_children[0]["id"], "ses-child-direct");
    assert_eq!(root_children[0]["parentID"], "ses-root-activity");

    let direct_children = children_response(responses, "ses-child-direct");
    assert_eq!(
        direct_children.len(),
        1,
        "nested discovery follows the direct child"
    );
    assert_eq!(direct_children[0]["id"], "ses-child-nested");
    assert_eq!(direct_children[0]["parentID"], "ses-child-direct");

    let nested_children = children_response(responses, "ses-child-nested");
    assert!(
        nested_children.is_empty(),
        "the recursive traversal terminates"
    );
}

#[test]
fn opencode_activity_fixture_rejects_sse_frames_that_lose_child_identity_or_treat_reasoning_as_commentary()
 {
    let fixture = activity_fixture("trace-child-sse.json");
    assert_opencode_1184_metadata(&fixture);
    assert_eq!(fixture["rootSessionID"], "ses-root-activity");

    let frames = raw_sse_frames(&fixture);
    let created = sse_frame(&frames, "oc-1184-child-created");
    assert_eq!(created["type"], "session.created");
    assert_eq!(created["properties"]["sessionID"], "ses-child-direct");
    assert_eq!(created["properties"]["info"]["id"], "ses-child-direct");
    assert_eq!(
        created["properties"]["info"]["parentID"],
        "ses-root-activity"
    );

    let statuses = frames
        .iter()
        .filter(|frame| frame["type"] == "session.status")
        .map(|frame| {
            (
                frame["properties"]["sessionID"]
                    .as_str()
                    .expect("status session ID"),
                frame["properties"]["status"]["type"]
                    .as_str()
                    .expect("documented status type"),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        statuses,
        vec![
            ("ses-child-direct", "busy"),
            ("ses-child-direct", "retry"),
            ("ses-child-direct", "idle"),
        ],
        "OpenCode 1.18.4 does not emit pending/running/completed/error session statuses",
    );

    let text = sse_frame(&frames, "oc-1184-child-text");
    assert_eq!(text["properties"]["sessionID"], "ses-child-direct");
    assert_eq!(text["properties"]["part"]["id"], "prt-child-text");
    assert_eq!(text["properties"]["part"]["sessionID"], "ses-child-direct");
    assert_eq!(
        text["properties"]["part"]["messageID"],
        "msg-child-assistant"
    );
    assert_eq!(text["properties"]["part"]["type"], "text");

    let message = sse_frame(&frames, "oc-1184-child-message");
    assert_eq!(message["properties"]["sessionID"], "ses-child-direct");
    assert_eq!(message["properties"]["info"]["id"], "msg-child-assistant");
    assert_eq!(
        message["properties"]["info"]["sessionID"],
        "ses-child-direct"
    );
    assert_eq!(message["properties"]["info"]["role"], "assistant");

    let reasoning = sse_frame(&frames, "oc-1184-child-reasoning");
    assert_eq!(reasoning["properties"]["sessionID"], "ses-child-direct");
    assert_eq!(reasoning["properties"]["part"]["id"], "prt-child-reasoning");
    assert_eq!(
        reasoning["properties"]["part"]["sessionID"],
        "ses-child-direct"
    );
    assert_eq!(
        reasoning["properties"]["part"]["messageID"],
        "msg-child-assistant"
    );
    assert_eq!(reasoning["properties"]["part"]["type"], "reasoning");
    assert_ne!(
        reasoning["properties"]["part"]["type"], "text",
        "raw reasoning is not documented commentary",
    );

    for (frame_id, expected_state) in [
        ("oc-1184-child-tool-pending", "pending"),
        ("oc-1184-child-tool-running", "running"),
        ("oc-1184-child-tool-completed", "completed"),
    ] {
        let tool = sse_frame(&frames, frame_id);
        assert_eq!(tool["properties"]["sessionID"], "ses-child-direct");
        assert_eq!(tool["properties"]["part"]["id"], "prt-child-tool");
        assert_eq!(tool["properties"]["part"]["sessionID"], "ses-child-direct");
        assert_eq!(
            tool["properties"]["part"]["messageID"],
            "msg-child-assistant"
        );
        assert_eq!(tool["properties"]["part"]["type"], "tool");
        assert_eq!(
            tool["properties"]["part"]["state"]["status"],
            expected_state
        );
    }

    let command = sse_frame(&frames, "oc-1184-child-command");
    assert_eq!(command["type"], "command.executed");
    assert_eq!(command["properties"]["sessionID"], "ses-child-direct");
    assert_eq!(command["properties"]["messageID"], "msg-child-assistant");
    assert_eq!(command["properties"]["name"], "review");
    assert_eq!(
        command["properties"]["arguments"],
        "[redacted command arguments]"
    );
}

#[test]
fn opencode_activity_fixture_rejects_history_without_terminal_evidence_or_sse_deduplication() {
    let fixture = activity_fixture("trace-child-history.json");
    assert_opencode_1184_metadata(&fixture);
    assert!(
        fixture.get("duplicateSseEventIDs").is_none(),
        "REST history cannot claim SSE event IDs it does not transport",
    );
    let responses = fixture["messageResponses"]
        .as_array()
        .expect("message responses");
    let completed = message_response(responses, "ses-child-direct");
    assert_eq!(completed[0]["info"]["id"], "msg-child-assistant");
    assert_eq!(completed[0]["info"]["sessionID"], "ses-child-direct");
    assert_eq!(completed[0]["info"]["role"], "assistant");
    assert_eq!(
        completed[0]["info"]["time"]["completed"],
        1_721_827_202_000_i64
    );
    assert_eq!(completed[0]["info"]["finish"], "stop");
    assert_eq!(completed[0]["parts"][0]["id"], "prt-child-text");
    assert_eq!(completed[0]["parts"][0]["sessionID"], "ses-child-direct");
    assert_eq!(completed[0]["parts"][0]["messageID"], "msg-child-assistant");

    let failed = message_response(responses, "ses-child-nested");
    assert_eq!(failed[0]["info"]["id"], "msg-nested-assistant");
    assert_eq!(failed[0]["info"]["sessionID"], "ses-child-nested");
    assert_eq!(failed[0]["info"]["role"], "assistant");
    assert_eq!(failed[0]["info"]["error"]["name"], "ProviderError");
    assert_eq!(
        failed[0]["info"]["error"]["message"],
        "[redacted provider failure]"
    );

    let child_sse = activity_fixture("trace-child-sse.json");
    let child_frames = raw_sse_frames(&child_sse);
    let live_identities = [
        part_identity(&sse_frame(&child_frames, "oc-1184-child-text")["properties"]["part"]),
        part_identity(
            &sse_frame(&child_frames, "oc-1184-child-tool-completed")["properties"]["part"],
        ),
    ];
    let history_identities = [
        part_identity(&completed[0]["parts"][0]),
        part_identity(&completed[0]["parts"][2]),
    ];
    assert_eq!(
        history_identities, live_identities,
        "REST history duplicates live delivery only through stable session/message/part identity",
    );
}

#[test]
fn opencode_activity_fixture_rejects_root_activity_that_erases_child_terminal_evidence() {
    let history_fixture = activity_fixture("trace-child-history.json");
    let completed = message_response(
        history_fixture["messageResponses"]
            .as_array()
            .expect("message responses"),
        "ses-child-direct",
    );
    let child_sse = activity_fixture("trace-child-sse.json");
    let child_frames = raw_sse_frames(&child_sse);
    let root_activity = sse_frame(&child_frames, "oc-1184-root-after-child-terminal");
    assert_eq!(
        root_activity["properties"]["sessionID"],
        "ses-root-activity"
    );
    assert!(
        root_activity["properties"]["info"]["time"]["created"]
            .as_i64()
            .expect("root activity time")
            > completed[0]["info"]["time"]["completed"]
                .as_i64()
                .expect("assistant terminal time"),
        "later root activity cannot replace documented child terminal evidence",
    );
}

#[test]
fn opencode_activity_fixture_rejects_foreign_workspace_events_that_claim_child_attribution() {
    let fixture = activity_fixture("trace-foreign-session.json");
    assert_opencode_1184_metadata(&fixture);
    assert_eq!(fixture["rootSessionID"], "ses-root-activity");
    assert_eq!(
        fixture["verifiedChildSessionIDs"],
        json!(["ses-child-direct", "ses-child-nested"])
    );
    let stream_directory = fixture["streamContext"]["directory"]
        .as_str()
        .expect("directory-scoped event stream");

    let frames = raw_sse_frames(&fixture);
    let foreign_session = sse_frame(&frames, "oc-1184-foreign-session");
    assert_eq!(foreign_session["type"], "session.updated");
    assert_eq!(
        foreign_session["properties"]["sessionID"],
        "ses-foreign-workspace"
    );
    assert_eq!(
        foreign_session["properties"]["info"]["id"],
        "ses-foreign-workspace"
    );
    assert_eq!(
        foreign_session["properties"]["info"]["directory"],
        stream_directory
    );
    assert_eq!(
        foreign_session["properties"]["info"]["parentID"],
        "ses-unrelated-parent"
    );
    assert_ne!(
        foreign_session["properties"]["info"]["parentID"],
        fixture["rootSessionID"]
    );
    assert!(
        !fixture["verifiedChildSessionIDs"]
            .as_array()
            .expect("verified child IDs")
            .contains(&foreign_session["properties"]["info"]["parentID"]),
        "same-directory foreign lineage must not terminate at a verified child",
    );

    let foreign = sse_frame(&frames, "oc-1184-foreign-text");
    assert_eq!(foreign["type"], "message.part.updated");
    assert_eq!(foreign["properties"]["sessionID"], "ses-foreign-workspace");
    assert_eq!(
        foreign["properties"]["part"]["sessionID"],
        "ses-foreign-workspace"
    );
    assert_eq!(
        foreign["properties"]["part"]["messageID"],
        "msg-foreign-assistant"
    );
    assert_eq!(
        foreign["properties"]["sessionID"],
        foreign_session["properties"]["info"]["id"]
    );
    assert_ne!(foreign["properties"]["sessionID"], fixture["rootSessionID"]);
    assert!(
        !fixture["verifiedChildSessionIDs"]
            .as_array()
            .expect("verified child IDs")
            .contains(&foreign["properties"]["sessionID"]),
        "directory-scoped SSE must not make a foreign session a child",
    );
}

#[test]
fn opencode_activity_fixture_rejects_reconnect_recovery_that_depends_on_sse_replay() {
    let fixture = activity_fixture("trace-reconnect.json");
    assert_opencode_1184_metadata(&fixture);
    let reconnect_frames = raw_sse_frames(&fixture);
    assert_eq!(
        reconnect_frames.len(),
        1,
        "reconnect has no SSE replay buffer"
    );
    assert_eq!(reconnect_frames[0]["type"], "server.connected");
    assert_eq!(reconnect_frames[0]["id"], "oc-1184-reconnected");

    let snapshots = fixture["restSnapshots"].as_array().expect("REST snapshots");
    let root_children = children_response(snapshots, "ses-root-activity");
    assert_eq!(root_children[0]["id"], "ses-child-missed-direct");
    assert_eq!(root_children[0]["parentID"], "ses-root-activity");
    let nested_children = children_response(snapshots, "ses-child-missed-direct");
    assert_eq!(nested_children[0]["id"], "ses-child-missed-nested");
    assert_eq!(nested_children[0]["parentID"], "ses-child-missed-direct");

    let status = snapshots
        .iter()
        .find(|snapshot| snapshot["path"] == "/session/status")
        .expect("global status snapshot")["response"]
        .as_object()
        .expect("status endpoint returns a bare map");
    assert_eq!(status["ses-child-missed-direct"]["type"], "idle");
    assert!(
        !status.contains_key("ses-child-missed-nested"),
        "absence remains waiting until message or tool terminal evidence is recovered",
    );

    let history = message_response(snapshots, "ses-child-missed-direct");
    assert_eq!(history[0]["info"]["sessionID"], "ses-child-missed-direct");
    assert_eq!(history[0]["info"]["role"], "assistant");
    assert_eq!(
        history[0]["info"]["time"]["completed"],
        1_721_827_260_000_i64
    );
    assert_eq!(history[0]["info"]["finish"], "stop");
}

#[tokio::test]
async fn opencode_runtime_authenticates_with_configured_server_password() {
    let state = Arc::new(TestServerState::default());
    let app = Router::new()
        .route("/session", post(create_authenticated_session))
        .route("/event", get(subscribe_events))
        .with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address: SocketAddr = listener.local_addr().expect("addr");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    let runtime = OpenCodeSessionRuntime::new_with_password(
        &format!("http://{address}"),
        "opencode-auth-thread",
        "/tmp/project",
        None,
        Some("secret"),
    )
    .expect("authenticated runtime");

    assert_eq!(
        runtime.start().await.expect("start"),
        "authenticated-session"
    );
    server.abort();
}

#[tokio::test]
async fn opencode_runtime_registers_the_bibcode_mcp_server() {
    let app = Router::new().route("/mcp", post(register_mcp));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address: SocketAddr = listener.local_addr().expect("addr");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    let runtime = OpenCodeSessionRuntime::new(
        &format!("http://{address}"),
        "opencode-mcp-thread",
        "C:/repo with spaces",
        None,
    );

    runtime
        .add_mcp_server("bibcode", "http://127.0.0.1:3773/mcp", "Bearer secret")
        .await
        .expect("register MCP");
    server.abort();
}

#[tokio::test]
async fn opencode_runtime_failure_boundaries_reject_invalid_sessions_and_http_statuses() {
    let state = Arc::new(TestServerState::default());
    let app = Router::new()
        .route("/session", post(invalid_session))
        .route("/session/{session_id}", get(resume_session))
        .route("/event", get(subscribe_permission_events))
        .route("/mcp", post(reject_request))
        .route("/session/{session_id}/prompt_async", post(reject_request))
        .route("/session/{session_id}/command", post(reject_request))
        .route("/session/{session_id}/abort", post(reject_request))
        .route("/permission/{request_id}/reply", post(reject_request))
        .with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address: SocketAddr = listener.local_addr().expect("addr");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    let runtime = OpenCodeSessionRuntime::new(
        &format!("http://{address}"),
        "opencode-failure-thread",
        "/tmp/project",
        Some("openai/gpt-5.4"),
    );

    assert!(runtime.start().await.is_err());
    assert!(runtime.resume(" ").await.is_err());
    assert!(runtime.resume("bad").await.is_err());
    assert_eq!(runtime.resume("session-1").await.unwrap(), "session-1");
    assert!(
        runtime
            .add_mcp_server("bibcode", "http://localhost/mcp", "Bearer token")
            .await
            .is_err()
    );
    assert!(runtime.set_model("missing-slash").await.is_err());
    assert!(runtime.send_command("/", "", None).await.is_err());
    assert!(
        runtime
            .send_turn(Some("hello"), Vec::new(), None)
            .await
            .is_err()
    );
    assert!(runtime.send_command("test", "args", None).await.is_err());
    assert!(runtime.rollback_thread(1, "rewind-key").await.is_err());
    assert!(
        runtime
            .respond_to_user_input("missing", json!({}))
            .await
            .is_err()
    );
    assert!(
        runtime
            .respond_to_permission("missing", "accept")
            .await
            .is_err()
    );

    let events = timeout(Duration::from_secs(2), runtime.collect_events(5))
        .await
        .expect("permission event");
    assert!(
        events
            .iter()
            .any(|event| event.request_id.as_deref() == Some("permission-1"))
    );
    assert!(
        runtime
            .respond_to_permission("permission-1", "accept")
            .await
            .is_err()
    );
    runtime
        .interrupt_turn()
        .await
        .expect("abort request writes");
    runtime.stop().await.expect("runtime stops");
    server.abort();
}

#[tokio::test]
async fn opencode_runtime_starts_a_new_session_only_when_the_saved_one_is_gone() {
    let state = Arc::new(TestServerState::default());
    let app = Router::new()
        .route("/session", post(create_session))
        .route("/session/{session_id}", get(resume_session))
        .route("/event", get(subscribe_permission_events))
        .with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address: SocketAddr = listener.local_addr().expect("addr");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    let runtime = OpenCodeSessionRuntime::new(
        &format!("http://{address}"),
        "opencode-lost-thread",
        "/tmp/project",
        None,
    );

    // Any other failure may be transient, so the saved session is kept for a retry.
    assert!(runtime.resume("bad").await.is_err());
    assert_eq!(runtime.resume("gone").await.unwrap(), "session-1");
    runtime.stop().await.expect("runtime stops");
    server.abort();
}

#[tokio::test]
async fn opencode_runtime_maps_transport_loss_across_every_live_operation() {
    let state = Arc::new(TestServerState::default());
    let app = Router::new()
        .route("/session", post(invalid_json_session))
        .route("/session/{session_id}", get(resume_session))
        .route("/event", get(subscribe_question_and_permission_events))
        .with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address: SocketAddr = listener.local_addr().expect("addr");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    let runtime = OpenCodeSessionRuntime::new_with_options(
        &format!("http://{address}"),
        "opencode-transport-thread",
        "/tmp/project",
        Some("openai/gpt-5.4"),
        None,
        Some("reviewer"),
    )
    .expect("runtime");

    assert!(
        runtime
            .start()
            .await
            .expect_err("invalid JSON must fail")
            .to_string()
            .contains("HTTP request failed")
    );
    assert_eq!(
        runtime.resume("transport-session").await.unwrap(),
        "transport-session"
    );

    let mut saw_question = false;
    let mut saw_permission = false;
    for _ in 0..6 {
        let event = timeout(Duration::from_secs(2), runtime.next_event())
            .await
            .expect("pending request event timeout")
            .expect("pending request event");
        saw_question |= event.request_id.as_deref() == Some("transport-question");
        saw_permission |= event.request_id.as_deref() == Some("transport-permission");
        if saw_question && saw_permission {
            break;
        }
    }
    assert!(saw_question && saw_permission);

    server.abort();
    sleep(Duration::from_millis(25)).await;

    assert!(runtime.start().await.is_err());
    assert!(runtime.resume("transport-session").await.is_err());
    assert!(
        runtime
            .add_mcp_server("bibcode", "http://localhost/mcp", "Bearer token")
            .await
            .is_err()
    );
    assert!(
        runtime
            .send_turn(Some("hello"), Vec::new(), None)
            .await
            .is_err()
    );
    assert!(runtime.send_command("review", "src", None).await.is_err());
    assert!(runtime.interrupt_turn().await.is_err());
    assert!(runtime.rollback_thread(1, "rewind-key").await.is_err());
    assert!(
        runtime
            .respond_to_permission("transport-permission", "decline")
            .await
            .is_err()
    );
    assert!(
        runtime
            .respond_to_user_input(
                "transport-question",
                json!({
                    "Scope": ["Workspace", "Session"],
                    "Notes": "all",
                    "Ignored": 42,
                }),
            )
            .await
            .is_err()
    );
    runtime.stop().await.expect("stop");
}

#[tokio::test]
async fn opencode_runtime_surfaces_and_resolves_permission_requests() {
    let state = Arc::new(TestServerState::default());
    let app = Router::new()
        .route("/session", post(create_session))
        .route("/event", get(subscribe_permission_events))
        .route("/permission/{request_id}/reply", post(reply_permission))
        .with_state(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address: SocketAddr = listener.local_addr().expect("addr");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    let runtime = OpenCodeSessionRuntime::new(
        &format!("http://{address}"),
        "opencode-permission-thread",
        "/tmp/project",
        None,
    );
    runtime.start().await.expect("start");
    let events = timeout(Duration::from_secs(2), runtime.collect_events(3))
        .await
        .expect("permission event");
    assert!(events.iter().any(|event| {
        event.event_type == "request.opened" && event.request_id.as_deref() == Some("permission-1")
    }));

    runtime
        .respond_to_permission("permission-1", "acceptForSession")
        .await
        .expect("permission reply");
    assert_eq!(
        state.permission_reply.lock().await.as_ref(),
        Some(&json!({ "reply": "always" }))
    );
    server.abort();
}

#[tokio::test]
async fn opencode_runtime_matches_session_and_rollback_traces() {
    let state = Arc::new(TestServerState::default());
    let app = Router::new()
        .route("/session", post(create_session))
        .route("/event", get(subscribe_events))
        .route("/question/{request_id}/reply", post(reply_question))
        .route("/session/{session_id}/prompt_async", post(prompt_async))
        .route("/session/{session_id}/abort", post(abort_session))
        .route("/session/{session_id}/message", get(list_messages))
        .route("/session/{session_id}/revert", post(revert_session))
        .with_state(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address: SocketAddr = listener.local_addr().expect("addr");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    let runtime = OpenCodeSessionRuntime::new(
        &format!("http://{address}"),
        "opencode-thread-1",
        "/tmp/project",
        Some("openai/gpt-5"),
    );
    runtime.start().await.expect("start");
    runtime
        .set_model("openai/gpt-5.4")
        .await
        .expect("switch model");
    runtime.set_variant(Some("fast".to_owned())).await;
    let send_runtime = runtime.clone();
    let send_turn = tokio::spawn(async move {
        send_runtime
            .send_turn(
                Some("hello"),
                vec![json!({
                    "type": "file",
                    "mime": "image/png",
                    "url": "file:///state/attachments/image-1",
                    "filename": "screen.png"
                })],
                None,
            )
            .await
    });
    let mut session_events = timeout(Duration::from_secs(2), runtime.collect_events(5))
        .await
        .expect("initial OpenCode events");
    let turn_id = session_events
        .iter()
        .find(|event| event.event_type == "turn.started")
        .and_then(|event| event.turn_id.clone())
        .expect("turn id");

    runtime
        .respond_to_user_input("question-1", json!({ "Scope": "Workspace" }))
        .await
        .expect("reply");
    let mut question_events = runtime.collect_events(1).await;
    normalize_turn_ids(&mut question_events, &turn_id);
    assert_eq!(
        question_events,
        stable_fixture("trace-question-resolved.json")
    );
    send_turn.await.expect("turn join").expect("turn");
    assert_eq!(
        state.prompt_body.lock().await.as_ref(),
        Some(&json!({
            "sessionID": "session-1",
            "model": { "providerID": "openai", "modelID": "gpt-5.4" },
            "variant": "fast",
            "parts": [
                { "type": "text", "text": "hello" },
                {
                    "type": "file",
                    "mime": "image/png",
                    "url": "file:///state/attachments/image-1",
                    "filename": "screen.png"
                }
            ],
        }))
    );
    session_events.extend(
        timeout(Duration::from_secs(2), runtime.collect_events(2))
            .await
            .expect("completed OpenCode events"),
    );
    normalize_turn_ids(&mut session_events, &turn_id);
    assert_eq!(session_events, stable_fixture("trace-session.json"));

    {
        let mut messages = state.messages.lock().await;
        *messages = vec![
            json!({ "info": { "id": "assistant-1", "role": "assistant" }, "parts": [] }),
            json!({ "info": { "id": "assistant-2", "role": "assistant" }, "parts": [] }),
        ];
    }
    let rollback = runtime
        .rollback_thread(2, "rewind-key")
        .await
        .expect("rollback");
    assert_eq!(
        serde_json::to_value(rollback).expect("rollback json"),
        fixture("rollback.json")
    );
    {
        let mut messages = state.messages.lock().await;
        *messages = vec![
            json!({ "info": { "id": "assistant-1", "role": "assistant" }, "parts": [] }),
            json!({ "info": { "id": "assistant-2", "role": "assistant" }, "parts": [] }),
            json!({ "info": { "id": "assistant-3", "role": "assistant" }, "parts": [] }),
        ];
    }
    assert_eq!(
        runtime
            .rollback_thread(1, "rewind-key")
            .await
            .expect("targeted rollback")
            .turns
            .len(),
        2
    );
    runtime.interrupt_turn().await.expect("interrupt");
    assert_eq!(*state.abort_count.lock().await, 1);

    runtime.stop().await.expect("stop");
    server.abort();
}

#[tokio::test]
async fn root_assistant_text_preserves_message_identity_and_completion() {
    let state = Arc::new(TestServerState::default());
    let app = Router::new()
        .route("/session", post(create_session))
        .route("/event", get(subscribe_root_assistant_identity_events))
        .route("/session/{session_id}/prompt_async", post(prompt_async))
        .with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address: SocketAddr = listener.local_addr().expect("addr");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    let runtime = OpenCodeSessionRuntime::new(
        &format!("http://{address}"),
        "opencode-identity-thread",
        "/tmp/project",
        None,
    );

    runtime.start().await.expect("start");
    let send_runtime = runtime.clone();
    let send_turn = tokio::spawn(async move {
        send_runtime
            .send_turn(Some("preserve identity"), Vec::new(), None)
            .await
    });
    let events = timeout(Duration::from_secs(2), async {
        let mut events = Vec::new();
        loop {
            let event = runtime.next_event().await.expect("root assistant event");
            let turn_completed = event.event_type == "turn.completed";
            events.push(event);
            if turn_completed {
                return events;
            }
        }
    })
    .await
    .expect("root assistant events");
    send_turn.await.expect("turn join").expect("turn");

    assert_eq!(
        events
            .iter()
            .filter(|event| {
                event.event_type == "content.delta"
                    || event.event_type == "message.assistant.completed"
            })
            .map(|event| (event.event_type.as_str(), event.item_id.as_deref()))
            .collect::<Vec<_>>(),
        vec![
            ("content.delta", Some("opencode-message-1")),
            ("message.assistant.completed", Some("opencode-message-1")),
            ("content.delta", Some("opencode-message-2")),
        ]
    );

    runtime.stop().await.expect("stop");
    server.abort();
}

#[tokio::test]
async fn opencode_runtime_dispatches_native_commands_with_agent_and_model() {
    let state = Arc::new(TestServerState::default());
    let app = Router::new()
        .route("/session", post(create_session))
        .route("/event", get(subscribe_permission_events))
        .route("/session/{session_id}/command", post(run_command))
        .with_state(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address: SocketAddr = listener.local_addr().expect("addr");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    let runtime = OpenCodeSessionRuntime::new_with_options(
        &format!("http://{address}"),
        "opencode-command-thread",
        "/tmp/project",
        Some("openai/gpt-5.4"),
        None,
        Some("reviewer"),
    )
    .expect("runtime");
    runtime.start().await.expect("start");
    runtime.set_variant(Some("high".to_owned())).await;
    runtime
        .send_command("review", "src/provider", None)
        .await
        .expect("native command");

    assert_eq!(
        state.command_body.lock().await.as_ref(),
        Some(&json!({
            "command": "review",
            "arguments": "src/provider",
            "agent": "reviewer",
            "model": "openai/gpt-5.4",
            "variant": "high",
        }))
    );
    server.abort();
}

#[tokio::test]
async fn opencode_options_use_exact_model_variants_and_reject_conflicts_before_prompt() {
    let state = Arc::new(TestServerState::default());
    let app = Router::new()
        .route("/session", post(create_session))
        .route("/event", get(subscribe_permission_events))
        .route(
            "/provider",
            get(|| async {
                Json(json!({
                    "connected": ["openai"],
                    "all": [{
                        "id": "openai",
                        "models": {
                            "gpt-5": { "variants": { "fast": {}, "high": {}, "low": {} } },
                            "other": { "variants": { "fast": {}, "medium": {} } },
                            "fast-only": { "variants": { "fast": {} } }
                        }
                    }]
                }))
            }),
        )
        .route("/session/{session_id}/prompt_async", post(prompt_async))
        .with_state(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address: SocketAddr = listener.local_addr().expect("addr");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    let runtime = OpenCodeSessionRuntime::new(
        &format!("http://{address}"),
        "opencode-options-thread",
        "/tmp/project",
        Some("openai/gpt-5"),
    );
    runtime.start().await.expect("start");

    let unknown = runtime
        .set_options(vec![json!({ "id": "madeUp", "value": true })])
        .await
        .expect_err("an unknown option is refused");
    assert_eq!(
        unknown.option_refusal(),
        Some("madeUp is not supported by the selected model.")
    );
    // The shape checks name each option by the label its descriptor shows.
    for (options, refusal) in [
        (
            vec![json!({ "value": true })],
            "The turn has an option without an id.",
        ),
        (
            vec![json!({ "id": "fastMode", "value": "yes" })],
            "Fast must be on or off.",
        ),
        (
            vec![json!({ "id": "variant", "value": "" })],
            "Variant needs a value.",
        ),
    ] {
        let refused = runtime
            .set_options(options)
            .await
            .expect_err("a malformed option is refused");
        assert_eq!(refused.option_refusal(), Some(refusal));
    }
    assert!(state.prompt_body.lock().await.is_none());
    let unadvertised = runtime
        .set_options(vec![json!({ "id": "variant", "value": "medium" })])
        .await
        .expect_err("a variant the model does not advertise is refused");
    assert_eq!(
        unadvertised.option_refusal(),
        Some("Variant medium is not supported by the selected model.")
    );
    assert!(state.prompt_body.lock().await.is_none());

    runtime
        .set_options(vec![json!({ "id": "fastMode", "value": false })])
        .await
        .expect("false selects the advertised non-fast default");
    let conflict = runtime
        .set_options(vec![
            json!({ "id": "fastMode", "value": true }),
            json!({ "id": "variant", "value": "high" }),
        ])
        .await
        .expect_err("Fast and a variant exclude each other");
    assert_eq!(
        conflict.option_refusal(),
        Some("Fast and Variant can't be used together.")
    );
    runtime
        .send_turn(Some("hello"), Vec::new(), None)
        .await
        .unwrap();
    assert_eq!(
        state.prompt_body.lock().await.as_ref().unwrap()["variant"],
        "high"
    );

    runtime.set_model("openai/other").await.unwrap();
    runtime
        .set_options(vec![json!({ "id": "fastMode", "value": true })])
        .await
        .expect("compatible target reapplies Fast");
    runtime
        .send_turn(Some("fast again"), Vec::new(), None)
        .await
        .unwrap();
    assert_eq!(
        state.prompt_body.lock().await.as_ref().unwrap()["variant"],
        "fast"
    );

    runtime.set_model("openai/fast-only").await.unwrap();
    runtime
        .set_options(vec![json!({ "id": "fastMode", "value": false })])
        .await
        .expect("false does not invent a fast-only variant");
    runtime
        .send_turn(Some("again"), Vec::new(), None)
        .await
        .unwrap();
    assert!(
        state
            .prompt_body
            .lock()
            .await
            .as_ref()
            .unwrap()
            .get("variant")
            .is_none()
    );

    server.abort();
}

/// A server that cannot be reached says nothing about the options, so the failure stays one a
/// retry may overcome instead of a refusal.
#[tokio::test]
async fn opencode_unreachable_server_is_not_an_option_refusal() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address: SocketAddr = listener.local_addr().expect("addr");
    drop(listener);
    let runtime = OpenCodeSessionRuntime::new(
        &format!("http://{address}"),
        "opencode-unreachable-thread",
        "/tmp/project",
        Some("openai/gpt-5"),
    );

    let error = runtime
        .set_options(vec![json!({ "id": "fastMode", "value": true })])
        .await
        .expect_err("the advertised variants cannot be read");
    assert_eq!(error.option_refusal(), None, "{error}");
}

#[tokio::test]
async fn opencode_runtime_surfaces_session_errors_and_removes_the_unanswered_prompt() {
    let state = Arc::new(TestServerState::default());
    let app = Router::new()
        .route("/session", post(create_session))
        .route("/event", get(subscribe_error_events))
        .route(
            "/session/{session_id}/prompt_async",
            post(error_prompt_async),
        )
        .route(
            "/session/{session_id}/message/{message_id}",
            delete(delete_message),
        )
        .with_state(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address: SocketAddr = listener.local_addr().expect("addr");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });

    let runtime = OpenCodeSessionRuntime::new(
        &format!("http://{address}"),
        "opencode-error-thread",
        "/tmp/project",
        Some("openai/gpt-5"),
    );
    runtime.start().await.expect("start");
    runtime
        .send_turn(Some("hello"), vec![], None)
        .await
        .expect("send turn");
    let events = timeout(Duration::from_secs(2), runtime.collect_events(4))
        .await
        .expect("failed OpenCode turn events");

    assert_eq!(events[3].event_type, "turn.completed");
    assert!(
        events[3]
            .turn_id
            .as_deref()
            .is_some_and(|turn_id| turn_id.starts_with("turn-"))
    );
    assert_eq!(
        events[3].payload,
        json!({
            "state": "failed",
            "stopReason": "error",
            "errorMessage": "Model not found: openai/gpt-5",
            "errorClass": "provider_error",
        })
    );
    assert_eq!(
        state.deleted_messages.lock().await.as_slice(),
        ["user-error-1"]
    );
    // The fixture closes its SSE stream at the end of the scenario, and the pump
    // reports that teardown as `session.exited`. That event is required, not
    // merely tolerated: without it a crashed opencode server goes back to
    // looking like a hung BiBCode. Nothing else may follow it, because idle
    // after an error must not emit a second successful completion.
    let mut saw_exit = false;
    while !saw_exit {
        let event = timeout(Duration::from_secs(2), runtime.next_event())
            .await
            .expect("terminal OpenCode event")
            .expect("session exit before stream termination");
        assert_eq!(
            event.event_type, "session.exited",
            "idle after an error must not emit a second successful completion"
        );
        saw_exit = true;
    }
    assert!(
        saw_exit,
        "a closed opencode event stream is reported as session.exited"
    );
    assert!(
        timeout(Duration::from_secs(1), runtime.next_event())
            .await
            .expect("closed OpenCode stream becomes terminal")
            .is_none(),
        "the supervisor must observe EOF after the final session.exited event"
    );

    runtime.stop().await.expect("stop");
    server.abort();
}

#[tokio::test]
async fn opencode_turn_ids_remain_unique_across_runtime_restarts() {
    let state = Arc::new(TestServerState::default());
    let app = Router::new()
        .route("/session", post(create_session))
        .route("/event", get(subscribe_pending_events))
        .route(
            "/session/{session_id}/prompt_async",
            post(prompt_async_immediate),
        )
        .with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address: SocketAddr = listener.local_addr().expect("addr");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    let endpoint = format!("http://{address}");

    let first =
        OpenCodeSessionRuntime::new(&endpoint, "opencode-restart-thread", "/tmp/project", None);
    first.start().await.expect("first start");
    let first_turn_id = first
        .send_turn(Some("first"), vec![], None)
        .await
        .expect("first turn");
    first.stop().await.expect("first stop");

    let second =
        OpenCodeSessionRuntime::new(&endpoint, "opencode-restart-thread", "/tmp/project", None);
    second.start().await.expect("second start");
    let second_turn_id = second
        .send_turn(Some("second"), vec![], None)
        .await
        .expect("second turn");
    second.stop().await.expect("second stop");

    assert_ne!(first_turn_id, second_turn_id);
    server.abort();
}

#[tokio::test]
async fn reconciliation_recurses_with_encoded_paths_and_recovers_a_missed_child_on_reconnect() {
    let state = Arc::new(ReconciliationServerState::new("root/with space"));
    state.children.lock().await.extend([
        (
            "root/with space".to_owned(),
            vec![json!({
                "id": "child/direct",
                "parentID": "root/with space",
                "title": "Direct child",
                "time": { "created": 10, "updated": 11 }
            })],
        ),
        (
            "child/direct".to_owned(),
            vec![json!({
                "id": "nested child",
                "parentID": "child/direct",
                "title": "Nested child",
                "time": { "created": 12, "updated": 13 }
            })],
        ),
        ("nested child".to_owned(), Vec::new()),
    ]);
    *state.statuses.lock().await = json!({
        "root/with space": { "type": "idle" },
        "child/direct": { "type": "busy" },
        "nested child": {
            "type": "retry",
            "attempt": 2,
            "message": "waiting",
            "next": 99
        },
        "foreign": { "type": "busy" }
    });
    state.histories.lock().await.extend([
        (
            "child/direct".to_owned(),
            json!([{
                "info": {
                    "id": "message-direct",
                    "sessionID": "child/direct",
                    "role": "assistant",
                    "time": { "created": 20 }
                },
                "parts": [{
                    "id": "part-direct",
                    "sessionID": "child/direct",
                    "messageID": "message-direct",
                    "type": "tool",
                    "callID": "call-direct",
                    "tool": "bash",
                    "state": { "status": "completed" }
                }]
            }]),
        ),
        (
            "nested child".to_owned(),
            json!([{
                "info": {
                    "id": "message-nested",
                    "sessionID": "nested child",
                    "role": "assistant",
                    "time": { "created": 21, "completed": 22 },
                    "finish": "stop"
                },
                "parts": [{
                    "id": "part-nested",
                    "sessionID": "nested child",
                    "messageID": "message-nested",
                    "type": "text",
                    "text": "recovered"
                }]
            }]),
        ),
    ]);
    let (endpoint, server) = spawn_reconciliation_server(state.clone()).await;
    let runtime = OpenCodeSessionRuntime::new(
        &endpoint,
        "opencode-reconciliation-thread",
        "/tmp/project",
        None,
    );

    assert_eq!(runtime.start().await.expect("start"), "root/with space");
    let first = next_reconciliation_activity(&runtime).await;
    assert!(first.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::SetScope {
            capabilities: ActivityCapabilities {
                actors: true,
                attributed_activity: true,
                background_work: false,
                history_recovery: ActivityHistoryRecovery::Full,
                terminal_observation: false,
                targeted_actor_cancellation: false,
            },
            observation_state: ActivityObservationState::Live,
        }
    )));
    assert!(first.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::UpsertActor(actor)
            if actor.id == "opencode:session:child/direct"
                && actor.status.as_str() == "running"
    )));
    assert!(first.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::UpsertActor(actor)
            if actor.id == "opencode:session:nested child"
    )));
    assert!(first.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::AppendEntry(entry)
            if entry.owner_id == "opencode:session:nested child"
                && entry.detail.as_deref() == Some("recovered")
    )));
    let children_requests = state.children_requests.lock().await.clone();
    assert_eq!(children_requests.len(), 3);
    assert!(
        children_requests
            .iter()
            .any(|uri| uri.starts_with("/session/root%2Fwith%20space/children?"))
    );
    assert!(
        children_requests
            .iter()
            .any(|uri| uri.starts_with("/session/child%2Fdirect/children?"))
    );
    assert_eq!(state.status_requests.load(Ordering::SeqCst), 1);
    assert_eq!(state.history_requests.lock().await.len(), 2);
    assert!(state.history_requests.lock().await.iter().all(|uri| {
        uri.contains("limit=200") && !uri.starts_with("http://") && !uri.starts_with("https://")
    }));

    state.children.lock().await.insert(
        "nested child".to_owned(),
        vec![json!({
            "id": "missed-child",
            "parentID": "nested child",
            "title": "Missed during disconnect",
            "time": { "created": 30, "updated": 31 }
        })],
    );
    state
        .children
        .lock()
        .await
        .insert("missed-child".to_owned(), Vec::new());
    state.histories.lock().await.insert(
        "missed-child".to_owned(),
        json!([{
            "info": {
                "id": "message-missed",
                "sessionID": "missed-child",
                "role": "assistant",
                "time": { "created": 32, "completed": 33 },
                "finish": "stop"
            },
            "parts": []
        }]),
    );
    state.statuses.lock().await["missed-child"] = json!({ "type": "idle" });
    state.reconnect_burst.notify_waiters();
    let recovered = next_reconciliation_activity(&runtime).await;
    assert!(recovered.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::UpsertActor(actor)
            if actor.id == "opencode:session:missed-child"
    )));
    sleep(Duration::from_millis(350)).await;
    assert_eq!(
        state.status_requests.load(Ordering::SeqCst),
        2,
        "three server.connected hints collapse into one 250ms pass"
    );
    assert_eq!(
        state.history_requests.lock().await.len(),
        5,
        "reconnect invalidates known history and also fetches the newly discovered child"
    );

    runtime.stop().await.expect("stop");
    server.abort();
}

#[tokio::test]
async fn disabled_agent_activity_skips_reconciliation_and_resumes_authoritatively() {
    let state = Arc::new(ReconciliationServerState::new("root-toggle"));
    state.children.lock().await.insert(
        "root-toggle".to_owned(),
        vec![json!({
            "id": "child-toggle",
            "parentID": "root-toggle",
            "title": "Toggle child",
            "time": {"created": 10, "updated": 11}
        })],
    );
    state
        .children
        .lock()
        .await
        .insert("child-toggle".to_owned(), Vec::new());
    *state.statuses.lock().await = json!({"child-toggle": {"type": "busy"}});
    state.histories.lock().await.insert(
        "child-toggle".to_owned(),
        json!([{
            "info": {
                "id": "message-disabled-history",
                "sessionID": "child-toggle",
                "role": "assistant",
                "time": {"created": 12}
            },
            "parts": [{
                "id": "part-disabled-history",
                "sessionID": "child-toggle",
                "messageID": "message-disabled-history",
                "type": "text",
                "text": "disabled-straddling-start",
                "time": {"start": 12}
            }]
        }]),
    );
    let (endpoint, server) = spawn_reconciliation_server(state.clone()).await;
    let runtime =
        OpenCodeSessionRuntime::new(&endpoint, "opencode-toggle-thread", "/tmp/project", None);

    runtime.set_agent_activity_enabled(false).await;
    runtime.start().await.expect("runtime starts");
    let normal = runtime.collect_events(2).await;
    assert_eq!(normal.len(), 2, "normal session readiness continues");
    sleep(Duration::from_millis(350)).await;
    assert!(
        state.children_requests.lock().await.is_empty(),
        "disabled runtime performs no reconciliation work"
    );
    assert!(
        timeout(Duration::from_millis(100), runtime.next_event())
            .await
            .is_err(),
        "disabled runtime emits no activity-only batch"
    );

    runtime.set_agent_activity_enabled(true).await;
    let resumed = next_reconciliation_activity(&runtime).await;
    assert!(resumed.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::UpsertActor(actor)
            if actor.id == "opencode:session:child-toggle"
                && actor.status == ActivityLifecycle::Running
    )));
    assert!(resumed.iter().all(|mutation| !matches!(
        mutation,
        ProviderActivityMutation::AppendEntry(entry)
            if entry.detail.as_deref() == Some("disabled-straddling-start")
    )));
    assert_eq!(
        state.children_requests.lock().await.as_slice(),
        [
            "/session/root-toggle/children?directory=%2Ftmp%2Fproject",
            "/session/child-toggle/children?directory=%2Ftmp%2Fproject",
        ],
        "enable requests exactly one authoritative reconciliation pass"
    );

    let resumed_history_at_ms = recent_fixture_timestamp_ms().saturating_add(1_000);
    state.histories.lock().await.insert(
        "child-toggle".to_owned(),
        json!([
            {
                "info": {
                    "id": "message-disabled-history",
                    "sessionID": "child-toggle",
                    "role": "assistant",
                    "time": {"created": 12, "completed": resumed_history_at_ms},
                    "finish": "stop"
                },
                "parts": [{
                    "id": "part-disabled-history",
                    "sessionID": "child-toggle",
                    "messageID": "message-disabled-history",
                    "type": "text",
                    "text": "disabled-straddling-completed",
                    "time": {"start": 12, "end": resumed_history_at_ms}
                }]
            },
            {
                "info": {
                    "id": "message-resumed-history",
                    "sessionID": "child-toggle",
                    "role": "assistant",
                    "time": {
                        "created": resumed_history_at_ms,
                        "completed": resumed_history_at_ms + 1
                    },
                    "finish": "stop"
                },
                "parts": [{
                    "id": "part-resumed-history",
                    "sessionID": "child-toggle",
                    "messageID": "message-resumed-history",
                    "type": "text",
                    "text": "resumed-history-entry",
                    "time": {
                        "start": resumed_history_at_ms,
                        "end": resumed_history_at_ms + 1
                    }
                }]
            }
        ]),
    );
    state.reconnect_burst.notify_waiters();
    let new_generation = next_reconciliation_activity(&runtime).await;
    assert!(new_generation.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::AppendEntry(entry)
            if entry.detail.as_deref() == Some("resumed-history-entry")
    )));
    assert!(new_generation.iter().all(|mutation| !matches!(
        mutation,
        ProviderActivityMutation::AppendEntry(entry)
            if entry.detail.as_deref() == Some("disabled-straddling-start")
                || entry.detail.as_deref() == Some("disabled-straddling-completed")
    )));

    runtime.stop().await.expect("stop");
    server.abort();
}

#[tokio::test]
async fn disabled_agent_activity_cancels_inflight_reconciliation_before_resuming() {
    let state = Arc::new(ReconciliationServerState::new("root-toggle-inflight"));
    state.children.lock().await.insert(
        "root-toggle-inflight".to_owned(),
        vec![json!({
            "id": "child-toggle-inflight",
            "parentID": "root-toggle-inflight",
            "title": "Inflight child",
            "time": {"created": 10, "updated": 11}
        })],
    );
    state
        .children
        .lock()
        .await
        .insert("child-toggle-inflight".to_owned(), Vec::new());
    *state.statuses.lock().await = json!({"child-toggle-inflight": {"type": "busy"}});
    state
        .histories
        .lock()
        .await
        .insert("child-toggle-inflight".to_owned(), json!([]));
    state.hold_children.store(true, Ordering::SeqCst);
    let (endpoint, server) = spawn_reconciliation_server(state.clone()).await;
    let runtime = OpenCodeSessionRuntime::new(
        &endpoint,
        "opencode-toggle-inflight-thread",
        "/tmp/project",
        None,
    );

    runtime.start().await.expect("runtime starts");
    runtime.collect_events(2).await;
    state.children_entered.notified().await;
    runtime.set_agent_activity_enabled(false).await;
    state.hold_children.store(false, Ordering::SeqCst);
    state.children_continue.notify_waiters();
    assert!(
        timeout(Duration::from_millis(150), runtime.next_event())
            .await
            .is_err(),
        "the cancelled reconciliation cannot publish after disable"
    );

    runtime.set_agent_activity_enabled(true).await;
    let resumed = next_reconciliation_activity(&runtime).await;
    assert!(resumed.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::UpsertActor(actor)
            if actor.id == "opencode:session:child-toggle-inflight"
    )));

    runtime.stop().await.expect("stop");
    server.abort();
}

#[tokio::test]
async fn opencode_runtime_routes_root_verified_child_and_foreign_sse_without_cross_talk() {
    let observed_at_ms = recent_fixture_timestamp_ms();
    let state = Arc::new(ReconciliationServerState::new("root"));
    state.children.lock().await.insert(
        "root".to_owned(),
        vec![
            json!({
                "id": "child",
                "parentID": "root",
                "title": "Verified child",
                "time": { "created": observed_at_ms, "updated": observed_at_ms }
            }),
            json!({
                "id": "failed-child",
                "parentID": "root",
                "title": "Failed child",
                "time": { "created": observed_at_ms + 1, "updated": observed_at_ms + 1 }
            }),
        ],
    );
    state
        .children
        .lock()
        .await
        .insert("child".to_owned(), Vec::new());
    state
        .children
        .lock()
        .await
        .insert("failed-child".to_owned(), Vec::new());
    *state.statuses.lock().await = json!({
        "root": { "type": "idle" },
        "child": { "type": "idle" },
        "failed-child": { "type": "idle" }
    });
    state.histories.lock().await.insert(
        "child".to_owned(),
        json!([{
            "info": {
                "id": "shared-message",
                "sessionID": "child",
                "role": "assistant"
            },
            "parts": []
        }]),
    );
    state.histories.lock().await.insert(
        "failed-child".to_owned(),
        json!([{
            "info": {
                "id": "failed-message",
                "sessionID": "failed-child",
                "role": "assistant"
            },
            "parts": []
        }]),
    );
    let (endpoint, server) = spawn_reconciliation_server(state.clone()).await;
    let runtime = OpenCodeSessionRuntime::new(
        &endpoint,
        "opencode-live-routing-thread",
        "/tmp/project",
        None,
    );

    assert_eq!(runtime.start().await.expect("start"), "root");
    let initial = next_reconciliation_event(&runtime).await;
    assert!(initial.activity.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::UpsertActor(actor)
            if actor.id == "opencode:session:child"
                && actor.status.as_str() == "waiting"
    )));
    runtime
        .send_turn(Some("root prompt"), Vec::new(), None)
        .await
        .expect("root turn");

    *state.reconnect_events.lock().await = Some(vec![
        json!({
            "id": "child-message",
            "type": "message.updated",
            "properties": {
                "sessionID": "child",
                "info": {
                    "id": "shared-message",
                    "sessionID": "child",
                    "role": "assistant"
                }
            }
        }),
        json!({
            "id": "child-busy",
            "type": "session.status",
            "properties": {
                "sessionID": "child",
                "status": { "type": "busy" }
            }
        }),
        json!({
            "id": "child-snapshot",
            "type": "message.part.updated",
            "properties": {
                "sessionID": "child",
                "time": observed_at_ms + 5,
                "part": {
                    "id": "child-text",
                    "sessionID": "child",
                    "messageID": "shared-message",
                    "type": "text",
                    "text": "snapshot "
                }
            }
        }),
        json!({
            "id": "child-delta-1",
            "type": "message.part.delta",
            "properties": {
                "sessionID": "child",
                "messageID": "shared-message",
                "partID": "child-text",
                "field": "text",
                "delta": "live ",
                "time": observed_at_ms + 10
            }
        }),
        json!({
            "id": "child-delta-1",
            "type": "message.part.delta",
            "properties": {
                "sessionID": "child",
                "messageID": "shared-message",
                "partID": "child-text",
                "field": "text",
                "delta": "live ",
                "time": observed_at_ms + 20
            }
        }),
        json!({
            "id": "child-delta-2",
            "type": "message.part.delta",
            "properties": {
                "sessionID": "child",
                "messageID": "shared-message",
                "partID": "child-text",
                "field": "text",
                "delta": "text",
                "time": observed_at_ms + 110
            }
        }),
        json!({
            "id": "child-tool",
            "type": "message.part.updated",
            "properties": {
                "sessionID": "child",
                "part": {
                    "id": "child-tool",
                    "sessionID": "child",
                    "messageID": "shared-message",
                    "type": "tool",
                    "callID": "call-child",
                    "tool": "task",
                    "state": { "status": "completed" }
                }
            }
        }),
        json!({
            "id": "child-command",
            "type": "command.executed",
            "properties": {
                "sessionID": "child",
                "messageID": "shared-message",
                "name": "review",
                "arguments": "--quick"
            }
        }),
        json!({
            "id": "child-command",
            "type": "command.executed",
            "properties": {
                "sessionID": "child",
                "messageID": "shared-message",
                "name": "review",
                "arguments": "--quick"
            }
        }),
        json!({
            "id": "child-complete",
            "type": "message.updated",
            "properties": {
                "sessionID": "child",
                "info": {
                    "id": "shared-message",
                    "sessionID": "child",
                    "role": "assistant",
                    "time": { "completed": observed_at_ms + 200 },
                    "finish": "stop"
                }
            }
        }),
        json!({
            "id": "child-cancelled",
            "type": "session.error",
            "properties": {
                "sessionID": "child",
                "time": observed_at_ms + 300,
                "error": { "name": "MessageAbortedError" }
            }
        }),
        json!({
            "id": "failed-child-message",
            "type": "message.updated",
            "properties": {
                "sessionID": "failed-child",
                "info": {
                    "id": "failed-message",
                    "sessionID": "failed-child",
                    "role": "assistant",
                    "time": { "completed": observed_at_ms + 250 },
                    "error": { "name": "ProviderError", "message": "child failed" }
                }
            }
        }),
        json!({
            "id": "foreign-busy",
            "type": "session.status",
            "properties": {
                "sessionID": "foreign",
                "status": { "type": "busy" }
            }
        }),
        json!({
            "id": "foreign-message",
            "type": "message.updated",
            "properties": {
                "sessionID": "foreign",
                "info": {
                    "id": "foreign-message",
                    "sessionID": "foreign",
                    "role": "assistant"
                }
            }
        }),
        json!({
            "id": "foreign-part",
            "type": "message.part.updated",
            "properties": {
                "sessionID": "foreign",
                "part": {
                    "id": "foreign-part",
                    "sessionID": "foreign",
                    "messageID": "foreign-message",
                    "type": "text",
                    "text": "must not render"
                }
            }
        }),
        json!({
            "id": "root-shared-part",
            "type": "message.part.updated",
            "properties": {
                "sessionID": "root",
                "part": {
                    "id": "root-shared-part",
                    "sessionID": "root",
                    "messageID": "shared-message",
                    "type": "text",
                    "text": "child ownership must not authorize this root part"
                }
            }
        }),
        json!({
            "id": "root-message",
            "type": "message.updated",
            "properties": {
                "sessionID": "root",
                "info": {
                    "id": "root-message",
                    "sessionID": "root",
                    "role": "assistant"
                }
            }
        }),
        json!({
            "id": "root-part",
            "type": "message.part.updated",
            "properties": {
                "sessionID": "root",
                "part": {
                    "id": "root-part",
                    "sessionID": "root",
                    "messageID": "root-message",
                    "type": "text",
                    "text": "root reply"
                }
            }
        }),
        json!({
            "id": "root-idle",
            "type": "session.status",
            "properties": {
                "sessionID": "root",
                "status": { "type": "idle" }
            }
        }),
    ]);
    state.reconnect_burst.notify_waiters();

    let events = timeout(Duration::from_secs(4), async {
        let mut events = Vec::new();
        loop {
            let event = runtime.next_event().await.expect("routed SSE event");
            let completed = event.event_type == "turn.completed";
            events.push(event);
            if completed {
                break events;
            }
        }
    })
    .await
    .expect("routed SSE timeout");
    let activity_events = events
        .iter()
        .filter(|event| !event.activity.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(
        activity_events.len(),
        7,
        "empty, duplicate, root, and foreign frames must not emit activity events"
    );
    assert!(
        activity_events.iter().all(|event| {
            event.event_type == "activity.native" && event.native_event_id.is_some()
        })
    );
    assert_eq!(
        activity_events
            .iter()
            .flat_map(|event| event.activity.iter())
            .filter(|mutation| matches!(
                mutation,
                ProviderActivityMutation::AppendEntry(entry)
                    if entry.kind == ActivityEntryKind::Command
            ))
            .count(),
        1,
        "retrying the same native command frame must be idempotent"
    );
    assert!(activity_events.iter().any(|event| matches!(
        event.activity.as_slice(),
        [
            ProviderActivityMutation::AppendEntry(snapshot),
            ProviderActivityMutation::AppendEntry(first),
            ProviderActivityMutation::AppendEntry(second),
        ] if snapshot.kind == ActivityEntryKind::Commentary
            && first.kind == ActivityEntryKind::Commentary
            && second.kind == ActivityEntryKind::Commentary
            && snapshot.detail.as_deref() == Some("snapshot ")
            && first.detail.as_deref() == Some("live ")
            && second.detail.as_deref() == Some("text")
    )));
    assert!(activity_events.iter().any(|event| matches!(
        event.activity.as_slice(),
        [ProviderActivityMutation::AppendEntry(entry)]
            if entry.kind == ActivityEntryKind::Tool
                && entry.owner_id == "opencode:session:child"
    )));
    assert!(activity_events.iter().any(|event| matches!(
        event.activity.as_slice(),
        [ProviderActivityMutation::UpsertActor(actor)]
            if actor.id == "opencode:session:failed-child"
                && actor.status.as_str() == "failed"
    )));
    assert!(activity_events.iter().any(|event| matches!(
        event.activity.as_slice(),
        [ProviderActivityMutation::UpsertActor(actor)]
            if actor.id == "opencode:session:child"
                && actor.status.as_str() == "cancelled"
    )));
    assert!(activity_events.iter().all(|event| {
        event.activity.iter().all(|mutation| match mutation {
            ProviderActivityMutation::UpsertActor(actor) => actor.id != "opencode:session:foreign",
            ProviderActivityMutation::AppendEntry(entry) => {
                entry.owner_id != "opencode:session:foreign"
                    && entry.detail.as_deref() != Some("must not render")
            }
            _ => true,
        })
    }));
    let root_deltas = events
        .iter()
        .filter(|event| event.event_type == "content.delta")
        .map(|event| event.payload["delta"].as_str().unwrap_or_default())
        .collect::<Vec<_>>();
    assert_eq!(root_deltas, ["root reply"]);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.event_type == "turn.completed")
            .count(),
        1,
        "child terminal/error lifecycle must not complete the root turn"
    );

    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let projection = ActivityProjection::new(ActivityRepository::new(database));
    let scope = ActivityScopeSeed::thread(
        "thread:opencode-live-routing-thread",
        "opencode-live-routing-thread",
        "opencode",
        Some("opencode"),
        ActivityCapabilities::none(),
    )
    .expect("valid scope");
    projection
        .ensure_scope(scope.clone())
        .await
        .expect("initial none scope");
    projection
        .apply(
            &scope.scope_id,
            initial.native_event_id.expect("initial native ID"),
            initial.activity,
            "2026-07-25T12:10:00Z".to_owned(),
        )
        .await
        .expect("initial runtime batch reaches the durable projection");
    for (index, event) in activity_events.iter().enumerate() {
        projection
            .apply(
                &scope.scope_id,
                event.native_event_id.clone().expect("live child native ID"),
                event.activity.clone(),
                format!("2026-07-25T12:10:{:02}Z", index + 1),
            )
            .await
            .expect("live runtime batch reaches the durable projection");
    }
    let persisted = projection
        .list_detail(
            &scope.scope,
            &scope.scope_id,
            ActivityRecordKind::Actor,
            "opencode:session:child",
            None,
            20,
        )
        .await
        .expect("persisted live child detail");
    assert!(persisted.entries.iter().any(|entry| {
        entry.kind == ActivityEntryKind::Commentary && entry.detail.as_deref() == Some("snapshot ")
    }));
    assert!(
        persisted
            .entries
            .iter()
            .any(|entry| { entry.kind == ActivityEntryKind::Tool })
    );

    runtime.stop().await.expect("stop");
    server.abort();
}

#[tokio::test]
async fn opencode_runtime_resynchronizes_after_a_malformed_sse_frame() {
    let state = Arc::new(ReconciliationServerState::new("root"));
    let (endpoint, server) = spawn_reconciliation_server(state.clone()).await;
    let runtime = OpenCodeSessionRuntime::new(
        &endpoint,
        "opencode-malformed-sse-thread",
        "/tmp/project",
        None,
    );

    assert_eq!(runtime.start().await.expect("start"), "root");
    let _ = next_reconciliation_activity(&runtime).await;
    *state.reconnect_payloads.lock().await = Some(vec![
        "not-json".to_owned(),
        json!({
            "id": "question-after-malformed-frame",
            "type": "question.asked",
            "properties": {
                "sessionID": "root",
                "requestID": "question-after-malformed-frame",
                "questions": [{
                    "header": "Continue",
                    "question": "Did the stream recover?",
                    "options": []
                }]
            }
        })
        .to_string(),
    ]);
    state.reconnect_burst.notify_waiters();

    let event_types = timeout(Duration::from_secs(4), async {
        let mut event_types = Vec::new();
        while !event_types
            .iter()
            .any(|event_type| event_type == "user-input.requested")
        {
            event_types.push(
                runtime
                    .next_event()
                    .await
                    .expect("event pump remains connected")
                    .event_type,
            );
        }
        event_types
    })
    .await
    .expect("valid frame after malformed SSE reaches the runtime");
    assert!(
        event_types
            .iter()
            .any(|event_type| event_type == "runtime.error")
    );
    assert!(
        event_types
            .iter()
            .any(|event_type| event_type == "user-input.requested"),
        "the event pump parses the valid successor frame instead of terminating"
    );

    runtime.stop().await.expect("stop");
    server.abort();
}

#[tokio::test]
async fn opencode_runtime_discards_a_split_oversized_sse_frame_before_valid_activity() {
    let state = Arc::new(ReconciliationServerState::new("root"));
    let mut oversized = vec![b'x'; 256 * 1024 + 1];
    let discarded = json!({
        "id": "discarded-oversized-question",
        "type": "question.asked",
        "properties": {
            "sessionID": "root",
            "requestID": "discarded-oversized-question",
            "questions": [{
                "header": "Discarded",
                "question": "This suffix belongs to the oversized frame.",
                "options": []
            }]
        }
    });
    let valid = json!({
        "id": "valid-question-after-oversized-frame",
        "type": "question.asked",
        "properties": {
            "sessionID": "root",
            "requestID": "valid-question-after-oversized-frame",
            "questions": [{
                "header": "Continue",
                "question": "Did the stream recover?",
                "options": []
            }]
        }
    });
    let successor_chunk = format!("data: {discarded}\n\ndata: {valid}\n\n").into_bytes();
    *state.reconnect_raw_chunks.lock().await =
        Some(vec![std::mem::take(&mut oversized), successor_chunk]);
    let (endpoint, server) = spawn_reconciliation_server(state.clone()).await;
    let runtime = OpenCodeSessionRuntime::new(
        &endpoint,
        "opencode-oversized-sse-thread",
        "/tmp/project",
        None,
    );

    assert_eq!(runtime.start().await.expect("start"), "root");
    let _ = next_reconciliation_activity(&runtime).await;
    state.reconnect_burst.notify_waiters();

    let events = timeout(Duration::from_secs(4), async {
        let mut events = Vec::new();
        loop {
            let event = runtime
                .next_event()
                .await
                .expect("event pump remains connected");
            let recovered =
                event.request_id.as_deref() == Some("valid-question-after-oversized-frame");
            events.push(event);
            if recovered {
                break;
            }
        }
        events
    })
    .await
    .expect("valid frame after the oversized frame reaches the runtime");
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "runtime.error"),
        "the oversized frame remains observable as a bounded runtime error"
    );
    assert!(
        events
            .iter()
            .all(|event| event.request_id.as_deref() != Some("discarded-oversized-question")),
        "a suffix of the oversized frame must never be reparsed as a standalone event"
    );

    runtime.stop().await.expect("stop");
    server.abort();
}

#[tokio::test]
async fn opencode_runtime_recovers_a_child_frame_that_arrives_before_lineage_verification() {
    let state = Arc::new(ReconciliationServerState::new("root"));
    let (endpoint, server) = spawn_reconciliation_server(state.clone()).await;
    let runtime = OpenCodeSessionRuntime::new(
        &endpoint,
        "opencode-preverification-thread",
        "/tmp/project",
        None,
    );

    assert_eq!(runtime.start().await.expect("start"), "root");
    let _ = next_reconciliation_activity(&runtime).await;
    state.children.lock().await.insert(
        "root".to_owned(),
        vec![json!({
            "id": "late-child",
            "parentID": "root",
            "title": "Late child",
            "time": { "created": 10, "updated": 10 }
        })],
    );
    state
        .children
        .lock()
        .await
        .insert("late-child".to_owned(), Vec::new());
    state.histories.lock().await.insert(
        "late-child".to_owned(),
        json!([{
            "info": {
                "id": "late-message",
                "sessionID": "late-child",
                "role": "assistant",
                "time": { "created": 11, "completed": 12 },
                "finish": "stop"
            },
            "parts": [{
                "id": "late-text",
                "sessionID": "late-child",
                "messageID": "late-message",
                "type": "text",
                "text": "recovered after verification"
            }]
        }]),
    );
    *state.statuses.lock().await = json!({
        "root": { "type": "idle" },
        "late-child": { "type": "idle" }
    });
    *state.reconnect_events.lock().await = Some(vec![json!({
        "id": "late-child-created",
        "type": "session.created",
        "properties": {
            "sessionID": "late-child",
            "info": {
                "id": "late-child",
                "parentID": "root",
                "title": "Late child",
                "time": { "created": 10, "updated": 10 }
            }
        }
    })]);
    state.reconnect_burst.notify_waiters();

    // The fixture closes its SSE stream once the reconnect burst is delivered,
    // and the pump reports that teardown as `session.exited`. That is the only
    // event permitted here: an unverified child SSE frame is a reconciliation
    // hint, never renderable activity.
    while let Ok(Some(event)) = timeout(Duration::from_millis(100), runtime.next_event()).await {
        assert_eq!(
            event.event_type, "session.exited",
            "an unverified child SSE frame is a reconciliation hint, never renderable activity"
        );
        assert!(
            event.activity.is_empty(),
            "stream teardown carries no renderable activity"
        );
    }
    let recovered = next_reconciliation_activity(&runtime).await;
    assert!(recovered.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::UpsertActor(actor)
            if actor.id == "opencode:session:late-child"
                && actor.status.as_str() == "completed"
    )));
    assert!(recovered.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::AppendEntry(entry)
            if entry.owner_id == "opencode:session:late-child"
                && entry.detail.as_deref() == Some("recovered after verification")
    )));

    runtime.stop().await.expect("stop");
    server.abort();
}

#[tokio::test]
async fn live_timestamp_less_child_deltas_persist_observation_time_in_provider_order() {
    let state = Arc::new(ReconciliationServerState::new("root"));
    state.children.lock().await.extend([
        (
            "root".to_owned(),
            vec![json!({
                "id": "child",
                "parentID": "root",
                "title": "Live timestamp child",
                "time": { "created": 1_700_000_000_000_u64 }
            })],
        ),
        ("child".to_owned(), Vec::new()),
    ]);
    state
        .histories
        .lock()
        .await
        .insert("child".to_owned(), json!([]));
    let (endpoint, server) = spawn_reconciliation_server(state.clone()).await;
    let thread_id = "opencode-live-observation-time";
    let runtime = OpenCodeSessionRuntime::new(&endpoint, thread_id, "/tmp/project", None);
    let (projection, scope) =
        durable_activity_projection(thread_id, ActivityCapabilities::none()).await;

    runtime.start().await.expect("start");
    let initial = next_reconciliation_event(&runtime).await;
    projection
        .apply(
            &scope.scope_id,
            initial
                .native_event_id
                .expect("initial reconciliation native ID"),
            initial.activity,
            "2026-07-25T12:00:00Z".to_owned(),
        )
        .await
        .expect("initial reconciliation applies");

    *state.reconnect_events.lock().await = Some(vec![
        json!({
            "id": "child-message",
            "type": "message.updated",
            "properties": {
                "sessionID": "child",
                "info": {
                    "id": "child-assistant",
                    "sessionID": "child",
                    "role": "assistant"
                }
            }
        }),
        json!({
            "id": "delta-first",
            "type": "message.part.delta",
            "properties": {
                "sessionID": "child",
                "messageID": "child-assistant",
                "partID": "child-text",
                "field": "text",
                "delta": "first"
            }
        }),
        json!({
            "id": "delta-second",
            "type": "message.part.delta",
            "properties": {
                "sessionID": "child",
                "messageID": "child-assistant",
                "partID": "child-text",
                "field": "text",
                "delta": "second"
            }
        }),
        json!({
            "id": "delta-third",
            "type": "message.part.delta",
            "properties": {
                "sessionID": "child",
                "messageID": "child-assistant",
                "partID": "child-text",
                "field": "text",
                "delta": "third"
            }
        }),
    ]);
    state.reconnect_burst.notify_waiters();

    let (detail, live_entry_batches) = timeout(Duration::from_secs(4), async {
        let mut live_entry_batches = 0;
        loop {
            let event = runtime.next_event().await.expect("live runtime event");
            assert_ne!(
                event.event_type, "content.delta",
                "verified child text must not enter the root transcript"
            );
            if event.activity.is_empty() {
                continue;
            }
            let entry_count = event
                .activity
                .iter()
                .filter(|mutation| matches!(mutation, ProviderActivityMutation::AppendEntry(_)))
                .count();
            live_entry_batches += usize::from(entry_count > 0);
            projection
                .apply(
                    &scope.scope_id,
                    event.native_event_id.expect("live native ID"),
                    event.activity,
                    event.created_at,
                )
                .await
                .expect("live activity applies");
            let detail = projection
                .list_detail(
                    &scope.scope,
                    &scope.scope_id,
                    ActivityRecordKind::Actor,
                    "opencode:session:child",
                    None,
                    10,
                )
                .await
                .expect("child detail");
            if detail.entries.len() == 3 {
                break (detail, live_entry_batches);
            }
        }
    })
    .await
    .expect("timestamp-less live deltas reach durable detail");

    assert_eq!(
        live_entry_batches, 1,
        "one coalescing flush produces one native activity event"
    );
    assert_eq!(
        detail
            .entries
            .iter()
            .map(|entry| entry.detail.as_deref())
            .collect::<Vec<_>>(),
        vec![Some("third"), Some("second"), Some("first")],
        "descending durable chronology preserves provider emission order"
    );
    assert!(
        detail
            .entries
            .iter()
            .all(|entry| !entry.created_at.starts_with("1970-01-01")),
        "the production SSE boundary supplies truthful observation time"
    );
    assert!(
        detail
            .entries
            .windows(2)
            .all(|entries| entries[0].created_at > entries[1].created_at),
        "rapid deltas retain a stable order without entry-ID tie breaking"
    );

    runtime.stop().await.expect("stop");
    server.abort();
}

#[tokio::test]
async fn captured_timestamp_less_child_command_persists_observation_time_once() {
    let state = Arc::new(ReconciliationServerState::new("ses-root-activity"));
    state.children.lock().await.extend([
        (
            "ses-root-activity".to_owned(),
            vec![json!({
                "id": "ses-child-direct",
                "parentID": "ses-root-activity",
                "title": "Captured child",
                "time": { "created": 1_721_827_200_000_u64 }
            })],
        ),
        ("ses-child-direct".to_owned(), Vec::new()),
    ]);
    state
        .histories
        .lock()
        .await
        .insert("ses-child-direct".to_owned(), json!([]));
    let fixture = activity_fixture("trace-child-sse.json");
    let frames = raw_sse_frames(&fixture);
    let assistant = sse_frame(&frames, "oc-1184-child-message");
    let command = sse_frame(&frames, "oc-1184-child-command");
    assert!(
        command.pointer("/properties/time").is_none(),
        "the authoritative OpenCode 1.18.4 command capture has no provider timestamp"
    );
    let (endpoint, server) = spawn_reconciliation_server(state.clone()).await;
    let thread_id = "opencode-live-command-observation-time";
    let runtime = OpenCodeSessionRuntime::new(&endpoint, thread_id, "/tmp/project", None);
    let (projection, scope) =
        durable_activity_projection(thread_id, ActivityCapabilities::none()).await;

    runtime.start().await.expect("start");
    let initial = next_reconciliation_event(&runtime).await;
    projection
        .apply(
            &scope.scope_id,
            initial
                .native_event_id
                .expect("initial reconciliation native ID"),
            initial.activity,
            "2026-07-25T12:00:00Z".to_owned(),
        )
        .await
        .expect("initial reconciliation applies");

    *state.reconnect_events.lock().await = Some(vec![
        (*assistant).clone(),
        (*command).clone(),
        (*command).clone(),
    ]);
    state.reconnect_burst.notify_waiters();

    let command_event = timeout(Duration::from_secs(4), async {
        loop {
            let event = runtime.next_event().await.expect("live runtime event");
            if event.activity.iter().any(|mutation| {
                matches!(
                    mutation,
                    ProviderActivityMutation::AppendEntry(entry)
                        if entry.kind == ActivityEntryKind::Command
                )
            }) {
                break event;
            }
        }
    })
    .await
    .expect("captured command reaches the runtime activity boundary");
    let native_event_id = command_event
        .native_event_id
        .clone()
        .expect("captured command native ID");
    projection
        .apply(
            &scope.scope_id,
            native_event_id.clone(),
            command_event.activity.clone(),
            command_event.created_at.clone(),
        )
        .await
        .expect("captured command reaches the durable projection");
    let replay = projection
        .apply(
            &scope.scope_id,
            native_event_id,
            command_event.activity,
            command_event.created_at,
        )
        .await
        .expect("captured command replay is accepted");
    assert!(
        replay.is_empty(),
        "the stable native identity deduplicates an exact replay"
    );

    let detail = projection
        .list_detail(
            &scope.scope,
            &scope.scope_id,
            ActivityRecordKind::Actor,
            "opencode:session:ses-child-direct",
            None,
            10,
        )
        .await
        .expect("captured child detail");
    let commands = detail
        .entries
        .iter()
        .filter(|entry| entry.kind == ActivityEntryKind::Command)
        .collect::<Vec<_>>();
    assert_eq!(
        commands.len(),
        1,
        "the duplicate captured SSE frame is stable"
    );
    assert!(
        !commands[0].created_at.starts_with("1970-01-01"),
        "timestamp-less live commands use production SSE observation time"
    );

    runtime.stop().await.expect("stop");
    server.abort();
}

#[tokio::test]
async fn live_maximum_provider_timestamp_keeps_repeated_successors_in_durable_provider_order() {
    let state = Arc::new(ReconciliationServerState::new("root"));
    state.children.lock().await.extend([
        (
            "root".to_owned(),
            vec![json!({
                "id": "child",
                "parentID": "root",
                "title": "Maximum timestamp child",
                "time": { "created": 1_700_000_000_000_u64 }
            })],
        ),
        ("child".to_owned(), Vec::new()),
    ]);
    state
        .histories
        .lock()
        .await
        .insert("child".to_owned(), json!([]));
    let (endpoint, server) = spawn_reconciliation_server(state.clone()).await;
    let thread_id = "opencode-live-maximum-provider-time";
    let runtime = OpenCodeSessionRuntime::new(&endpoint, thread_id, "/tmp/project", None);
    let remaining_owner = runtime.clone();
    let (projection, scope) =
        durable_activity_projection(thread_id, ActivityCapabilities::none()).await;

    runtime.start().await.expect("start");
    let initial = next_reconciliation_event(&runtime).await;
    projection
        .apply(
            &scope.scope_id,
            initial
                .native_event_id
                .expect("initial reconciliation native ID"),
            initial.activity,
            "2026-07-25T12:00:00Z".to_owned(),
        )
        .await
        .expect("initial reconciliation applies");

    *state.reconnect_events.lock().await = Some(vec![
        json!({
            "id": "maximum-message",
            "type": "message.updated",
            "properties": {
                "sessionID": "child",
                "info": {
                    "id": "maximum-assistant",
                    "sessionID": "child",
                    "role": "assistant"
                }
            }
        }),
        json!({
            "id": "maximum-delta",
            "type": "message.part.delta",
            "properties": {
                "sessionID": "child",
                "messageID": "maximum-assistant",
                "partID": "maximum-text",
                "field": "text",
                "delta": "maximum",
                "time": 253_402_300_799_999_u64
            }
        }),
        json!({
            "id": "post-maximum-one",
            "type": "message.part.delta",
            "properties": {
                "sessionID": "child",
                "messageID": "maximum-assistant",
                "partID": "maximum-text",
                "field": "text",
                "delta": "successor-one"
            }
        }),
        json!({
            "id": "post-maximum-two",
            "type": "message.part.delta",
            "properties": {
                "sessionID": "child",
                "messageID": "maximum-assistant",
                "partID": "maximum-text",
                "field": "text",
                "delta": "successor-two"
            }
        }),
    ]);
    state.reconnect_burst.notify_waiters();

    let detail = timeout(Duration::from_secs(4), async {
        loop {
            let event = remaining_owner.next_event().await.expect("runtime event");
            if event.activity.is_empty() {
                continue;
            }
            projection
                .apply(
                    &scope.scope_id,
                    event.native_event_id.expect("native event ID"),
                    event.activity,
                    event.created_at,
                )
                .await
                .expect("live activity applies");
            let detail = projection
                .list_detail(
                    &scope.scope,
                    &scope.scope_id,
                    ActivityRecordKind::Actor,
                    "opencode:session:child",
                    None,
                    10,
                )
                .await
                .expect("child detail");
            if detail.entries.len() == 3 {
                break detail;
            }
        }
    })
    .await
    .expect("maximum-boundary entries reach durable detail");

    assert_eq!(
        detail
            .entries
            .iter()
            .map(|entry| entry.detail.as_deref())
            .collect::<Vec<_>>(),
        vec![
            Some("successor-two"),
            Some("successor-one"),
            Some("maximum")
        ],
        "durable chronology preserves repeated provider order at the formatter ceiling"
    );
    assert!(
        detail
            .entries
            .iter()
            .all(|entry| !entry.created_at.starts_with("1970-01-01")),
        "the formatter ceiling must never poison later observations into epoch"
    );
    assert!(
        detail
            .entries
            .windows(2)
            .all(|entries| entries[0].created_at > entries[1].created_at),
        "representable successors must retain strict durable timestamp order"
    );

    remaining_owner.stop().await.expect("stop");
    drop(runtime);
    server.abort();
}

#[tokio::test]
async fn reconciliation_transactionally_drains_pending_live_text_within_slice_budget() {
    let state = Arc::new(ReconciliationServerState::new("root"));
    state.children.lock().await.extend([
        (
            "root".to_owned(),
            vec![json!({
                "id": "child",
                "parentID": "root",
                "title": "Bounded drain child",
                "time": { "created": 1_700_000_000_000_u64 }
            })],
        ),
        ("child".to_owned(), Vec::new()),
    ]);
    state
        .histories
        .lock()
        .await
        .insert("child".to_owned(), json!([]));
    let (endpoint, server) = spawn_reconciliation_server(state.clone()).await;
    let thread_id = "opencode-bounded-live-history-drain";
    let runtime = OpenCodeSessionRuntime::new(&endpoint, thread_id, "/tmp/project", None);
    let (projection, scope) =
        durable_activity_projection(thread_id, ActivityCapabilities::none()).await;

    runtime.start().await.expect("start");
    let initial = next_reconciliation_event(&runtime).await;
    projection
        .apply(
            &scope.scope_id,
            initial
                .native_event_id
                .expect("initial reconciliation native ID"),
            initial.activity,
            "2026-07-25T12:00:00Z".to_owned(),
        )
        .await
        .expect("initial reconciliation applies");
    state.histories.lock().await.insert(
        "child".to_owned(),
        json!([{
            "info": {
                "id": "child-assistant",
                "sessionID": "child",
                "role": "assistant"
            },
            "parts": []
        }]),
    );

    let mut frames = vec![
        json!({
            "id": "reconciliation-hint",
            "type": "server.connected",
            "properties": {}
        }),
        json!({
            "id": "child-message",
            "type": "message.updated",
            "properties": {
                "sessionID": "child",
                "info": {
                    "id": "child-assistant",
                    "sessionID": "child",
                    "role": "assistant"
                }
            }
        }),
    ];
    frames.extend((0..6).map(|index| {
        json!({
            "id": format!("bounded-delta-{index}"),
            "type": "message.part.delta",
            "properties": {
                "sessionID": "child",
                "messageID": "child-assistant",
                "partID": "child-text",
                "field": "text",
                "delta": format!("fragment-{index}")
            }
        })
    }));
    frames.push(json!({
        "id": "pending-drain-ready",
        "type": "session.status",
        "properties": {
            "sessionID": "child",
            "status": { "type": "busy" }
        }
    }));
    state.reconnect_pause_after.store(1, Ordering::SeqCst);
    state.hold_children.store(true, Ordering::SeqCst);
    *state.reconnect_events.lock().await = Some(frames);
    let children_entered = state.children_entered.notified();
    state.reconnect_burst.notify_waiters();
    children_entered.await;
    state.reconnect_continue.notify_waiters();
    timeout(Duration::from_millis(80), async {
        loop {
            let event = runtime.next_event().await.expect("live setup event");
            if event.activity.iter().any(|mutation| {
                matches!(
                    mutation,
                    ProviderActivityMutation::UpsertActor(actor)
                        if actor.id == "opencode:session:child"
                            && actor.status.as_str() == "running"
                )
            }) {
                break;
            }
        }
    })
    .await
    .expect("all pending live deltas reach the tracker before reconciliation resumes");
    state.hold_children.store(false, Ordering::SeqCst);
    state.children_continue.notify_waiters();

    let (detail, replay_events, reconciliation_entry_batches) =
        timeout(Duration::from_secs(4), async {
            let mut replay_events = Vec::new();
            let mut reconciliation_entry_batches = 0;
            loop {
                let event = runtime.next_event().await.expect("bounded runtime event");
                if event.activity.is_empty() {
                    continue;
                }
                let entry_count = event
                    .activity
                    .iter()
                    .filter(|mutation| matches!(mutation, ProviderActivityMutation::AppendEntry(_)))
                    .count();
                let is_reconciliation = event
                    .activity
                    .iter()
                    .any(|mutation| matches!(mutation, ProviderActivityMutation::SetScope { .. }));
                assert!(event.activity.len() <= 256, "provider batch bound");
                if is_reconciliation && entry_count > 0 {
                    assert!(
                        entry_count <= 4,
                        "one reconciliation history slice may drain at most four text mutations"
                    );
                    reconciliation_entry_batches += 1;
                }
                let native_event_id = event.native_event_id.expect("bounded native ID");
                projection
                    .apply(
                        &scope.scope_id,
                        native_event_id.clone(),
                        event.activity.clone(),
                        event.created_at.clone(),
                    )
                    .await
                    .expect("bounded activity applies");
                replay_events.push((native_event_id, event.activity, event.created_at));
                let detail = projection
                    .list_detail(
                        &scope.scope,
                        &scope.scope_id,
                        ActivityRecordKind::Actor,
                        "opencode:session:child",
                        None,
                        10,
                    )
                    .await
                    .expect("bounded child detail");
                if detail.entries.len() == 6 {
                    break (detail, replay_events, reconciliation_entry_batches);
                }
            }
        })
        .await
        .expect("bounded continuation drains every pending fragment");

    assert!(
        reconciliation_entry_batches >= 2,
        "retained pending text drains through deterministic continuation passes"
    );
    assert_eq!(
        detail
            .entries
            .iter()
            .map(|entry| entry.detail.as_deref())
            .collect::<HashSet<_>>()
            .len(),
        6,
        "every fragment is retained exactly once"
    );
    for (native_event_id, activity, created_at) in replay_events {
        let deltas = projection
            .apply(&scope.scope_id, native_event_id, activity, created_at)
            .await
            .expect("replay applies");
        assert!(
            deltas.is_empty(),
            "native replay produces no duplicate deltas"
        );
    }
    let replayed_detail = projection
        .list_detail(
            &scope.scope,
            &scope.scope_id,
            ActivityRecordKind::Actor,
            "opencode:session:child",
            None,
            10,
        )
        .await
        .expect("replayed detail");
    assert_eq!(replayed_detail.entries.len(), 6);

    runtime.stop().await.expect("panic-free stop");
    drop(runtime);
    server.abort();
}

#[tokio::test]
async fn reconciliation_bounds_discovery_to_fifty_children_and_history_to_two_hundred_items() {
    let state = Arc::new(ReconciliationServerState::new("root"));
    let children = (0..51)
        .map(|index| {
            json!({
                "id": format!("child-{index:02}"),
                "parentID": "root",
                "title": format!("Child {index:02}"),
                "time": { "created": index + 1, "updated": index + 1 }
            })
        })
        .collect::<Vec<_>>();
    state
        .children
        .lock()
        .await
        .insert("root".to_owned(), children);
    let history = (0..205)
        .map(|index| {
            json!({
                "info": {
                    "id": format!("message-{index}"),
                    "sessionID": "child-00",
                    "role": "assistant",
                    "time": { "created": index + 100 }
                },
                "parts": [{
                    "id": format!("part-{index}"),
                    "sessionID": "child-00",
                    "messageID": format!("message-{index}"),
                    "type": "tool",
                    "callID": format!("call-{index}"),
                    "tool": "bash",
                    "state": { "status": "completed" }
                }]
            })
        })
        .collect::<Vec<_>>();
    for index in 0..50 {
        let child = format!("child-{index:02}");
        state
            .children
            .lock()
            .await
            .insert(child.clone(), Vec::new());
        state.histories.lock().await.insert(
            child,
            if index == 0 {
                Value::Array(history.clone())
            } else {
                Value::Array(Vec::new())
            },
        );
    }
    let (endpoint, server) = spawn_reconciliation_server(state.clone()).await;
    let runtime = OpenCodeSessionRuntime::new(
        &endpoint,
        "opencode-reconciliation-bounds",
        "/tmp/project",
        None,
    );

    runtime.start().await.expect("start");
    let mutations = next_reconciliation_activity(&runtime).await;
    let actor_ids = mutations
        .iter()
        .filter_map(|mutation| match mutation {
            ProviderActivityMutation::UpsertActor(actor) => Some(actor.id.clone()),
            _ => None,
        })
        .collect::<HashSet<_>>();
    assert_eq!(actor_ids.len(), 50, "unique actors: {actor_ids:?}");
    assert!(!actor_ids.contains("opencode:session:child-50"));
    assert_eq!(
        mutations
            .iter()
            .filter(|mutation| matches!(mutation, ProviderActivityMutation::AppendEntry(_)))
            .count(),
        200
    );
    assert!(mutations.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::AppendEntry(entry) if entry.id.contains("message-199")
    )));
    assert!(!mutations.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::AppendEntry(entry) if entry.id.contains("message-200")
    )));
    assert_eq!(state.children_requests.lock().await.len(), 51);
    assert_eq!(state.history_requests.lock().await.len(), 50);

    runtime.stop().await.expect("stop");
    server.abort();
}

#[tokio::test]
async fn reconciliation_defers_history_that_cannot_fit_without_poisoning_its_signature() {
    let state = Arc::new(ReconciliationServerState::new("root"));
    state.children.lock().await.extend([
        (
            "root".to_owned(),
            vec![
                json!({
                    "id": "child-a",
                    "parentID": "root",
                    "title": "Child A",
                    "time": { "created": 1, "updated": 2 }
                }),
                json!({
                    "id": "child-b",
                    "parentID": "root",
                    "title": "Child B",
                    "time": { "created": 3, "updated": 4 }
                }),
            ],
        ),
        ("child-a".to_owned(), Vec::new()),
        ("child-b".to_owned(), Vec::new()),
    ]);
    for child in ["child-a", "child-b"] {
        let history = (0..200)
            .map(|index| {
                json!({
                    "info": {
                        "id": format!("{child}-message-{index}"),
                        "sessionID": child,
                        "role": "assistant",
                        "time": { "created": index + 10 }
                    },
                    "parts": [{
                        "id": format!("{child}-part-{index}"),
                        "sessionID": child,
                        "messageID": format!("{child}-message-{index}"),
                        "type": "tool",
                        "callID": format!("{child}-call-{index}"),
                        "tool": "bash",
                        "state": { "status": "completed" }
                    }]
                })
            })
            .collect::<Vec<_>>();
        state
            .histories
            .lock()
            .await
            .insert(child.to_owned(), Value::Array(history));
    }
    let (endpoint, server) = spawn_reconciliation_server(state.clone()).await;
    let runtime = OpenCodeSessionRuntime::new(
        &endpoint,
        "opencode-reconciliation-deferred-history",
        "/tmp/project",
        None,
    );

    runtime.start().await.expect("start");
    let first = next_reconciliation_activity(&runtime).await;
    assert!(
        first.len() <= 256 && first.len() > 200,
        "one reconciliation batch stays bounded while making progress"
    );
    state.reconnect_burst.notify_waiters();
    let second = next_reconciliation_activity(&runtime).await;
    assert!(second.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::AppendEntry(entry)
            if entry.id.contains("child-b-message-199")
    )));
    sleep(Duration::from_millis(350)).await;
    let history_requests = state.history_requests.lock().await;
    let child_a_requests = history_requests
        .iter()
        .filter(|request| request.contains("/session/child-a/message"))
        .count();
    let child_b_requests = history_requests
        .iter()
        .filter(|request| request.contains("/session/child-b/message"))
        .count();
    assert_eq!(
        child_a_requests, 2,
        "the reconnect force revalidates child-a"
    );
    assert!(
        (2..=3).contains(&child_b_requests),
        "child-b is revalidated, with at most one unfinished-cursor fetch"
    );

    runtime.stop().await.expect("stop");
    server.abort();
}

#[tokio::test]
async fn reconciliation_advances_through_one_message_with_two_hundred_parts() {
    let state = Arc::new(ReconciliationServerState::new("root"));
    state.children.lock().await.extend([
        (
            "root".to_owned(),
            vec![json!({
                "id": "child",
                "parentID": "root",
                "title": "Child",
                "time": { "created": 1, "updated": 2 }
            })],
        ),
        ("child".to_owned(), Vec::new()),
    ]);
    let parts = (0..200)
        .map(|index| {
            json!({
                "id": format!("part-{index}"),
                "sessionID": "child",
                "messageID": "message",
                "type": "tool",
                "callID": format!("call-{index}"),
                "tool": "bash",
                "state": { "status": "completed" }
            })
        })
        .collect::<Vec<_>>();
    state.histories.lock().await.insert(
        "child".to_owned(),
        json!([{
            "info": {
                "id": "message",
                "sessionID": "child",
                "role": "assistant",
                "time": { "created": 10 }
            },
            "parts": parts
        }]),
    );
    let (endpoint, server) = spawn_reconciliation_server(state.clone()).await;
    let runtime = OpenCodeSessionRuntime::new(
        &endpoint,
        "opencode-reconciliation-one-large-message",
        "/tmp/project",
        None,
    );

    runtime.start().await.expect("start");
    let mut recovered_tail = false;
    for pass in 0..4 {
        let mutations = next_reconciliation_activity(&runtime).await;
        recovered_tail |= mutations.iter().any(|mutation| {
            matches!(
                mutation,
                ProviderActivityMutation::AppendEntry(entry) if entry.id.contains("part-199")
            )
        });
        if recovered_tail {
            break;
        }
        state.reconnect_burst.notify_waiters();
        assert!(pass < 3, "history cursor never reached the final part");
    }
    assert!(
        recovered_tail,
        "the final part must be recovered across bounded passes"
    );

    runtime.stop().await.expect("stop");
    server.abort();
}

#[tokio::test]
async fn reconciliation_autonomously_drains_deferred_history_without_duplicate_entries() {
    let state = Arc::new(ReconciliationServerState::new("root"));
    state.children.lock().await.extend([
        (
            "root".to_owned(),
            vec![
                json!({
                    "id": "child-a",
                    "parentID": "root",
                    "title": "Child A",
                    "time": { "created": 1, "updated": 2 }
                }),
                json!({
                    "id": "child-b",
                    "parentID": "root",
                    "title": "Child B",
                    "time": { "created": 3, "updated": 4 }
                }),
            ],
        ),
        ("child-a".to_owned(), Vec::new()),
        ("child-b".to_owned(), Vec::new()),
    ]);
    for child in ["child-a", "child-b"] {
        state
            .histories
            .lock()
            .await
            .insert(child.to_owned(), tool_history(child, 200));
    }
    let (endpoint, server) = spawn_reconciliation_server(state).await;
    let runtime = OpenCodeSessionRuntime::new(
        &endpoint,
        "opencode-reconciliation-autonomous-cursor",
        "/tmp/project",
        None,
    );

    runtime.start().await.expect("start");
    let (batch_count, entry_count) = timeout(Duration::from_secs(4), async {
        let mut batch_count = 0;
        let mut entry_ids = HashSet::new();
        loop {
            let event = runtime.next_event().await.expect("runtime event");
            if event.activity.is_empty() {
                continue;
            }
            batch_count += 1;
            let mut reached_tail = false;
            for mutation in event.activity {
                if let ProviderActivityMutation::AppendEntry(entry) = mutation {
                    reached_tail |= entry.id.contains("child-b-message-199");
                    assert!(
                        entry_ids.insert(entry.id),
                        "autonomous continuation must not duplicate an activity entry"
                    );
                }
            }
            if reached_tail {
                break (batch_count, entry_ids.len());
            }
        }
    })
    .await
    .expect("deferred cursor must continue without another provider hint");
    assert!(batch_count >= 2);
    assert_eq!(entry_count, 400);

    runtime.stop().await.expect("stop");
    server.abort();
}

#[tokio::test]
async fn force_history_is_consumed_before_large_continuations_can_replay_evicted_entries() {
    let state = Arc::new(ReconciliationServerState::new("root"));
    let children = (0..13)
        .map(|index| {
            json!({
                "id": format!("child-{index:02}"),
                "parentID": "root",
                "title": format!("Child {index:02}"),
                "time": { "created": index + 1, "updated": index + 1 }
            })
        })
        .collect::<Vec<_>>();
    state
        .children
        .lock()
        .await
        .insert("root".to_owned(), children);
    for index in 0..13 {
        let child = format!("child-{index:02}");
        state
            .children
            .lock()
            .await
            .insert(child.clone(), Vec::new());
        state
            .histories
            .lock()
            .await
            .insert(child.clone(), tool_history(&child, 200));
    }
    let (endpoint, server) = spawn_reconciliation_server(state.clone()).await;
    let runtime = OpenCodeSessionRuntime::new(
        &endpoint,
        "opencode-force-history-one-shot",
        "/tmp/project",
        None,
    );

    runtime.start().await.expect("start");
    let entry_count = timeout(Duration::from_secs(8), async {
        let mut entry_ids = HashSet::new();
        loop {
            let event = runtime.next_event().await.expect("runtime event");
            assert!(event.activity.len() <= 256);
            let mut reached_tail = false;
            for mutation in event.activity {
                if let ProviderActivityMutation::AppendEntry(entry) = mutation {
                    reached_tail |= entry.id.contains("child-12-message-199");
                    assert!(
                        entry_ids.insert(entry.id),
                        "one accepted force-history snapshot must not be replayed by its continuation"
                    );
                }
            }
            if reached_tail {
                break entry_ids.len();
            }
        }
    })
    .await
    .expect("the final child cursor must recover after more than 2,400 history parts");
    assert_eq!(
        entry_count, 2_600,
        "all thirteen bounded histories recover exactly once"
    );
    assert_eq!(
        state
            .history_requests
            .lock()
            .await
            .iter()
            .filter(|uri| uri.contains("/session/child-00/message"))
            .count(),
        1,
        "an accepted force refresh is one-shot for a completed child"
    );

    runtime.stop().await.expect("stop");
    server.abort();
}

#[tokio::test]
async fn reconciliation_reconnect_refetches_equal_summary_and_status_history() {
    let state = Arc::new(ReconciliationServerState::new("root"));
    state.children.lock().await.extend([
        (
            "root".to_owned(),
            vec![json!({
                "id": "child",
                "parentID": "root",
                "title": "Child",
                "time": { "created": 1, "updated": 2 }
            })],
        ),
        ("child".to_owned(), Vec::new()),
    ]);
    *state.statuses.lock().await = json!({ "child": { "type": "busy" } });
    state
        .histories
        .lock()
        .await
        .insert("child".to_owned(), json!([]));
    let (endpoint, server) = spawn_reconciliation_server(state.clone()).await;
    let runtime = OpenCodeSessionRuntime::new(
        &endpoint,
        "opencode-reconciliation-dirty-history",
        "/tmp/project",
        None,
    );

    runtime.start().await.expect("start");
    let _ = next_reconciliation_activity(&runtime).await;
    state.histories.lock().await.insert(
        "child".to_owned(),
        json!([{
            "info": {
                "id": "late-message",
                "sessionID": "child",
                "role": "assistant",
                "time": { "created": 10 }
            },
            "parts": [{
                "id": "late-part",
                "sessionID": "child",
                "messageID": "late-message",
                "type": "text",
                "text": "same summary, new history"
            }]
        }]),
    );
    state.reconnect_burst.notify_waiters();
    let recovered = next_reconciliation_activity(&runtime).await;
    assert!(recovered.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::AppendEntry(entry)
            if entry.detail.as_deref() == Some("same summary, new history")
    )));
    assert_eq!(state.history_requests.lock().await.len(), 2);

    runtime.stop().await.expect("stop");
    server.abort();
}

#[tokio::test]
async fn reconciliation_root_plus_fifty_dirty_hints_never_drop_the_last_child() {
    let state = Arc::new(ReconciliationServerState::new("root"));
    let children = (0..50)
        .map(|index| {
            json!({
                "id": format!("child-{index:02}"),
                "parentID": "root",
                "title": format!("Child {index:02}"),
                "time": { "created": index + 1, "updated": index + 1 }
            })
        })
        .collect::<Vec<_>>();
    state
        .children
        .lock()
        .await
        .insert("root".to_owned(), children);
    for index in 0..50 {
        let child = format!("child-{index:02}");
        state
            .children
            .lock()
            .await
            .insert(child.clone(), Vec::new());
        state.histories.lock().await.insert(child, json!([]));
    }
    let (endpoint, server) = spawn_reconciliation_server(state.clone()).await;
    let runtime = OpenCodeSessionRuntime::new(
        &endpoint,
        "opencode-reconciliation-dirty-overflow",
        "/tmp/project",
        None,
    );

    runtime.start().await.expect("start");
    let _ = next_reconciliation_activity(&runtime).await;
    state.histories.lock().await.insert(
        "child-49".to_owned(),
        json!([{
            "info": {
                "id": "overflow-message",
                "sessionID": "child-49",
                "role": "assistant",
                "time": { "created": 100 }
            },
            "parts": [{
                "id": "overflow-part",
                "sessionID": "child-49",
                "messageID": "overflow-message",
                "type": "text",
                "text": "last dirty child retained"
            }]
        }]),
    );
    let mut events = vec![json!({
        "id": "root-hint",
        "type": "session.status",
        "properties": {
            "sessionID": "root",
            "status": { "type": "busy" }
        }
    })];
    events.extend((0..50).map(|index| {
        json!({
            "id": format!("child-hint-{index:02}"),
            "type": "message.updated",
            "properties": {
                "info": {
                    "id": format!("hint-message-{index:02}"),
                    "sessionID": format!("child-{index:02}"),
                    "role": "assistant"
                }
            }
        })
    }));
    *state.reconnect_events.lock().await = Some(events);
    state.reconnect_burst.notify_waiters();
    let recovered = next_reconciliation_activity(&runtime).await;
    assert!(recovered.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::AppendEntry(entry)
            if entry.detail.as_deref() == Some("last dirty child retained")
    )));
    assert_eq!(
        state.history_requests.lock().await.len(),
        100,
        "all fifty equal-signature histories are refreshed after the bounded burst"
    );

    runtime.stop().await.expect("stop");
    server.abort();
}

#[tokio::test]
async fn reconciliation_history_persists_documented_part_timestamps_in_durable_chronology() {
    const TIMESTAMP_FIXTURE_BASE_MS: u64 = 4_070_908_800_000;
    let state = Arc::new(ReconciliationServerState::new("root"));
    state.children.lock().await.extend([
        (
            "root".to_owned(),
            vec![json!({
                "id": "child",
                "parentID": "root",
                "title": "Timestamped child",
                "time": { "created": TIMESTAMP_FIXTURE_BASE_MS }
            })],
        ),
        ("child".to_owned(), Vec::new()),
    ]);
    state.histories.lock().await.insert(
        "child".to_owned(),
        json!([
            {
                "info": {
                    "id": "text-message",
                    "sessionID": "child",
                    "role": "assistant",
                    "time": {
                        "created": TIMESTAMP_FIXTURE_BASE_MS + 1_000,
                        "completed": TIMESTAMP_FIXTURE_BASE_MS + 5_000
                    },
                    "finish": "stop"
                },
                "parts": [{
                    "id": "text-part",
                    "sessionID": "child",
                    "messageID": "text-message",
                    "type": "text",
                    "text": "timestamped commentary",
                    "time": {
                        "start": TIMESTAMP_FIXTURE_BASE_MS + 2_000,
                        "end": TIMESTAMP_FIXTURE_BASE_MS + 3_000
                    }
                }]
            },
            {
                "info": {
                    "id": "fallback-message",
                    "sessionID": "child",
                    "role": "assistant",
                    "time": {
                        "created": TIMESTAMP_FIXTURE_BASE_MS + 7_000,
                        "completed": "malformed"
                    },
                    "finish": "stop"
                },
                "parts": [{
                    "id": "fallback-text",
                    "sessionID": "child",
                    "messageID": "fallback-message",
                    "type": "text",
                    "text": "message timestamp fallback",
                    "time": {
                        "start": u64::MAX,
                        "end": "malformed"
                    }
                }]
            },
            {
                "info": {
                    "id": "tool-message",
                    "sessionID": "child",
                    "role": "assistant",
                    "time": {
                        "created": TIMESTAMP_FIXTURE_BASE_MS + 4_000,
                        "completed": TIMESTAMP_FIXTURE_BASE_MS + 9_000
                    },
                    "finish": "stop"
                },
                "parts": [{
                    "id": "tool-part",
                    "sessionID": "child",
                    "messageID": "tool-message",
                    "type": "tool",
                    "callID": "tool-call",
                    "tool": "bash",
                    "state": {
                        "status": "completed",
                        "time": {
                            "start": TIMESTAMP_FIXTURE_BASE_MS + 6_000,
                            "end": TIMESTAMP_FIXTURE_BASE_MS + 8_000
                        }
                    }
                }]
            }
        ]),
    );
    let (endpoint, server) = spawn_reconciliation_server(state).await;
    let thread_id = "opencode-reconciliation-history-timestamps";
    let runtime = OpenCodeSessionRuntime::new(&endpoint, thread_id, "/tmp/project", None);
    let (projection, scope) =
        durable_activity_projection(thread_id, ActivityCapabilities::none()).await;

    runtime.start().await.expect("start");
    let reconciliation = next_reconciliation_event(&runtime).await;
    projection
        .apply(
            &scope.scope_id,
            reconciliation
                .native_event_id
                .expect("history reconciliation native ID"),
            reconciliation.activity,
            "2026-07-25T12:00:00Z".to_owned(),
        )
        .await
        .expect("history reconciliation applies");

    let detail = projection
        .list_detail(
            &scope.scope,
            &scope.scope_id,
            ActivityRecordKind::Actor,
            "opencode:session:child",
            None,
            10,
        )
        .await
        .expect("child detail");
    assert_eq!(detail.entries.len(), 3);
    assert_eq!(detail.entries[0].kind, ActivityEntryKind::Tool);
    assert_eq!(
        detail.entries[0].created_at, "2099-01-01T00:00:08.000000000Z",
        "terminal tool history uses state.time.end"
    );
    assert_eq!(
        detail.entries[1].detail.as_deref(),
        Some("message timestamp fallback")
    );
    assert_eq!(
        detail.entries[1].created_at, "2099-01-01T00:00:07.000000000Z",
        "invalid part/message completion sources fall through to message creation"
    );
    assert_eq!(
        detail.entries[2].detail.as_deref(),
        Some("timestamped commentary")
    );
    assert_eq!(
        detail.entries[2].created_at, "2099-01-01T00:00:03.000000000Z",
        "recovered text history uses part.time.end"
    );
    assert!(
        detail
            .entries
            .iter()
            .all(|entry| !entry.created_at.starts_with("1970-01-01"))
    );

    runtime.stop().await.expect("stop");
    server.abort();
}

#[tokio::test]
async fn reconciliation_batches_apply_through_production_projection_from_none_scope() {
    let state = Arc::new(ReconciliationServerState::new("root"));
    let (endpoint, server) = spawn_reconciliation_server(state.clone()).await;
    let runtime = OpenCodeSessionRuntime::new(
        &endpoint,
        "opencode-reconciliation-repository-boundary",
        "/tmp/project",
        None,
    );
    let (projection, scope) = durable_activity_projection(
        "opencode-reconciliation-repository-boundary",
        ActivityCapabilities::none(),
    )
    .await;

    runtime.start().await.expect("start");
    let empty_reconciliation = next_reconciliation_event(&runtime).await;
    let initial_live_native_event_id = empty_reconciliation
        .native_event_id
        .clone()
        .expect("empty reconciliation native ID");
    let initial_live_activity = empty_reconciliation.activity.clone();
    projection
        .apply(
            &scope.scope_id,
            initial_live_native_event_id.clone(),
            empty_reconciliation.activity,
            "2026-07-25T12:00:00Z".to_owned(),
        )
        .await
        .expect("empty reconciliation applies atomically");
    let negotiated = projection
        .snapshot(&scope.scope)
        .await
        .expect("negotiated snapshot");
    assert!(negotiated.capabilities.actors);
    assert_eq!(
        negotiated.sections.subagents.state,
        ActivitySectionObservationState::Live
    );
    assert_eq!(
        negotiated.sections.background_tasks.state,
        ActivitySectionObservationState::Unsupported
    );

    timeout(Duration::from_secs(1), async {
        while state.event_connections.load(Ordering::SeqCst) == 0 {
            state.event_connection_opened.notified().await;
        }
    })
    .await
    .expect("SSE connection opened");
    state.children_failures.store(1, Ordering::SeqCst);
    state.reconnect_burst.notify_waiters();

    let stale = next_reconciliation_event(&runtime).await;
    projection
        .apply(
            &scope.scope_id,
            stale.native_event_id.expect("stale native ID"),
            stale.activity,
            "2026-07-25T12:00:01Z".to_owned(),
        )
        .await
        .expect("stale transition applies atomically");
    let stale_snapshot = projection
        .snapshot(&scope.scope)
        .await
        .expect("stale snapshot");
    assert_eq!(
        stale_snapshot.observation_state,
        ActivityObservationState::Stale
    );
    assert_eq!(
        stale_snapshot.sections.subagents.state,
        ActivitySectionObservationState::Stale
    );

    let recovered = next_reconciliation_event(&runtime).await;
    let recovered_native_event_id = recovered
        .native_event_id
        .clone()
        .expect("recovered native ID");
    assert_eq!(
        initial_live_activity, recovered.activity,
        "recovery projects the identical provider snapshot after stale"
    );
    assert_ne!(
        initial_live_native_event_id, recovered_native_event_id,
        "post-stale recovery is a distinct causal reconciliation occurrence"
    );
    projection
        .apply(
            &scope.scope_id,
            recovered_native_event_id,
            recovered.activity,
            "2026-07-25T12:00:02Z".to_owned(),
        )
        .await
        .expect("recovery applies atomically");
    let recovered_snapshot = projection
        .snapshot(&scope.scope)
        .await
        .expect("recovered snapshot");
    assert_eq!(
        recovered_snapshot.observation_state,
        ActivityObservationState::Live
    );
    assert_eq!(
        recovered_snapshot.sections.subagents.state,
        ActivitySectionObservationState::Live
    );

    runtime.stop().await.expect("stop recovery runtime");
    server.abort();

    let child_observed_at_ms = recent_fixture_timestamp_ms();
    let child_state = Arc::new(ReconciliationServerState::new("root"));
    child_state.children.lock().await.extend([
        (
            "root".to_owned(),
            vec![json!({
                "id": "child",
                "parentID": "root",
                "title": "Repository child",
                "time": {
                    "created": child_observed_at_ms,
                    "updated": child_observed_at_ms + 1
                }
            })],
        ),
        ("child".to_owned(), Vec::new()),
    ]);
    *child_state.statuses.lock().await = json!({ "child": { "type": "busy" } });
    child_state.histories.lock().await.insert(
        "child".to_owned(),
        json!([{
            "info": {
                "id": "message",
                "sessionID": "child",
                "role": "assistant",
                "time": { "created": child_observed_at_ms + 2 }
            },
            "parts": [{
                "id": "text",
                "sessionID": "child",
                "messageID": "message",
                "type": "text",
                "text": "persisted child commentary"
            }]
        }]),
    );
    let (child_endpoint, child_server) = spawn_reconciliation_server(child_state).await;
    let child_runtime = OpenCodeSessionRuntime::new(
        &child_endpoint,
        "opencode-reconciliation-repository-boundary-child",
        "/tmp/project",
        None,
    );
    child_runtime.start().await.expect("child start");
    let child_reconciliation = next_reconciliation_event(&child_runtime).await;
    let child_mutations = child_reconciliation.activity.clone();
    assert!(matches!(
        child_mutations.first(),
        Some(ProviderActivityMutation::SetScope { .. })
    ));
    assert!(matches!(
        child_mutations.get(1),
        Some(ProviderActivityMutation::SetSectionHealth {
            section: ActivitySection::Subagents,
            ..
        })
    ));
    assert!(matches!(
        child_mutations.get(2),
        Some(ProviderActivityMutation::SetSectionHealth {
            section: ActivitySection::BackgroundTasks,
            ..
        })
    ));
    assert!(child_mutations.iter().skip(3).all(|mutation| matches!(
        mutation,
        ProviderActivityMutation::UpsertActor(_)
            | ProviderActivityMutation::AppendEntry(_)
            | ProviderActivityMutation::RemoveActor { .. }
    )));
    projection
        .apply(
            &scope.scope_id,
            child_reconciliation
                .native_event_id
                .expect("child reconciliation native ID"),
            child_reconciliation.activity,
            "2026-07-25T12:00:03Z".to_owned(),
        )
        .await
        .expect("child reconciliation applies atomically");

    // The child is busy and its latest message has no completion time, so it is active. (The
    // history DTO carries that missing time as `null`, which was once misread as completed.)
    let roster = projection
        .list_roster(
            &scope.scope,
            &scope.scope_id,
            ActivitySection::Subagents,
            ActivityRosterBucket::Active,
            None,
            10,
        )
        .await
        .expect("subagent roster");
    assert!(
        roster.records.iter().any(|record| {
            matches!(
                record,
                bibcode_server::activity::ActivityRecordSummary::Actor(actor)
                    if actor.id == "opencode:session:child"
            )
        }),
        "child mutations: {child_mutations:#?}; roster: {roster:#?}"
    );
    let detail = projection
        .list_detail(
            &scope.scope,
            &scope.scope_id,
            ActivityRecordKind::Actor,
            "opencode:session:child",
            None,
            10,
        )
        .await
        .expect("child detail");
    assert!(
        detail
            .entries
            .iter()
            .any(|entry| { entry.detail.as_deref() == Some("persisted child commentary") })
    );

    child_runtime.stop().await.expect("stop child runtime");
    child_server.abort();

    let bounded_state = Arc::new(ReconciliationServerState::new("root"));
    bounded_state
        .history_unsupported
        .store(true, Ordering::SeqCst);
    let (bounded_endpoint, bounded_server) = spawn_reconciliation_server(bounded_state).await;
    let bounded_runtime = OpenCodeSessionRuntime::new(
        &bounded_endpoint,
        "opencode-reconciliation-repository-boundary-bounded",
        "/tmp/project",
        None,
    );
    bounded_runtime.start().await.expect("bounded start");
    let bounded = next_reconciliation_event(&bounded_runtime).await;
    projection
        .apply(
            &scope.scope_id,
            bounded.native_event_id.expect("bounded native ID"),
            bounded.activity,
            "2026-07-25T12:00:04Z".to_owned(),
        )
        .await
        .expect("bounded capability transition applies atomically");
    let bounded_snapshot = projection
        .snapshot(&scope.scope)
        .await
        .expect("bounded snapshot");
    assert_eq!(
        bounded_snapshot.capabilities.history_recovery,
        ActivityHistoryRecovery::Bounded
    );
    assert_eq!(
        bounded_snapshot.sections.subagents.state,
        ActivitySectionObservationState::Live
    );
    bounded_runtime.stop().await.expect("stop bounded runtime");
    bounded_server.abort();

    let none_state = Arc::new(ReconciliationServerState::new("root"));
    none_state
        .children_unsupported
        .store(true, Ordering::SeqCst);
    let (none_endpoint, none_server) = spawn_reconciliation_server(none_state).await;
    let none_runtime = OpenCodeSessionRuntime::new(
        &none_endpoint,
        "opencode-reconciliation-repository-boundary-none",
        "/tmp/project",
        None,
    );
    none_runtime.start().await.expect("none start");
    let none = next_reconciliation_event(&none_runtime).await;
    projection
        .apply(
            &scope.scope_id,
            none.native_event_id.expect("none native ID"),
            none.activity,
            "2026-07-25T12:00:05Z".to_owned(),
        )
        .await
        .expect("none capability transition retains durable records");
    let none_snapshot = projection
        .snapshot(&scope.scope)
        .await
        .expect("none snapshot");
    assert_eq!(none_snapshot.capabilities, ActivityCapabilities::none());
    assert_eq!(
        none_snapshot.sections.subagents.state,
        ActivitySectionObservationState::Stale
    );
    assert_eq!(none_snapshot.actors.len(), 1);

    none_runtime.stop().await.expect("stop none runtime");
    none_server.abort();
}

#[tokio::test]
async fn reconciliation_native_ids_do_not_collide_after_same_thread_runtime_restart() {
    let state = Arc::new(ReconciliationServerState::new("root"));
    state.children.lock().await.extend([
        (
            "root".to_owned(),
            vec![json!({
                "id": "child-a",
                "parentID": "root",
                "title": "Child A",
                "time": { "created": 1, "updated": 2 }
            })],
        ),
        ("child-a".to_owned(), Vec::new()),
        ("child-b".to_owned(), Vec::new()),
    ]);
    let (endpoint, server) = spawn_reconciliation_server(state.clone()).await;

    let first = OpenCodeSessionRuntime::new(
        &endpoint,
        "opencode-reconciliation-restart-id",
        "/tmp/project",
        None,
    );
    first.start().await.expect("first start");
    let first_event = next_reconciliation_event(&first).await;
    first.stop().await.expect("first stop");

    state.children.lock().await.insert(
        "root".to_owned(),
        vec![json!({
            "id": "child-b",
            "parentID": "root",
            "title": "Child B",
            "time": { "created": 3, "updated": 4 }
        })],
    );
    let second = OpenCodeSessionRuntime::new(
        &endpoint,
        "opencode-reconciliation-restart-id",
        "/tmp/project",
        None,
    );
    second.start().await.expect("second start");
    let second_event = next_reconciliation_event(&second).await;
    assert_ne!(
        first_event.native_event_id, second_event.native_event_id,
        "different provider snapshots in one durable thread need distinct dedup identities"
    );
    let (projection, scope) = durable_activity_projection(
        "opencode-reconciliation-restart-id",
        ActivityCapabilities {
            actors: true,
            attributed_activity: true,
            background_work: false,
            history_recovery: ActivityHistoryRecovery::Full,
            terminal_observation: false,
            targeted_actor_cancellation: false,
        },
    )
    .await;
    let first_deltas = projection
        .apply(
            &scope.scope_id,
            first_event
                .native_event_id
                .clone()
                .expect("first native event id"),
            first_event.activity,
            "2026-07-25T12:00:00Z".to_owned(),
        )
        .await
        .expect("first restart batch");
    let second_deltas = projection
        .apply(
            &scope.scope_id,
            second_event
                .native_event_id
                .clone()
                .expect("second native event id"),
            second_event.activity,
            "2026-07-25T12:00:01Z".to_owned(),
        )
        .await
        .expect("second restart batch");
    assert!(!first_deltas.is_empty());
    assert!(
        !second_deltas.is_empty(),
        "the permanent repository dedup must admit the post-restart batch"
    );

    second.stop().await.expect("second stop");
    server.abort();
}

#[tokio::test]
async fn reconciliation_native_ids_reuse_retry_occurrence_through_durable_projection() {
    let state = Arc::new(ReconciliationServerState::new("root"));
    state.children_failures.store(2, Ordering::SeqCst);
    let (endpoint, server) = spawn_reconciliation_server(state).await;
    let runtime = OpenCodeSessionRuntime::new(
        &endpoint,
        "opencode-reconciliation-retry-id",
        "/tmp/project",
        None,
    );
    let (projection, scope) = durable_activity_projection(
        "opencode-reconciliation-retry-id",
        ActivityCapabilities::none(),
    )
    .await;

    runtime.start().await.expect("start");
    let first_stale = next_reconciliation_event(&runtime).await;
    let retried_stale = next_reconciliation_event(&runtime).await;
    assert_eq!(
        first_stale.native_event_id, retried_stale.native_event_id,
        "the bounded transport retry must retain its causal occurrence identity"
    );

    let first_deltas = projection
        .apply(
            &scope.scope_id,
            first_stale
                .native_event_id
                .clone()
                .expect("first stale native ID"),
            first_stale.activity,
            "2026-07-25T12:20:00Z".to_owned(),
        )
        .await
        .expect("first stale transition");
    let retry_deltas = projection
        .apply(
            &scope.scope_id,
            retried_stale
                .native_event_id
                .expect("retried stale native ID"),
            retried_stale.activity,
            "2026-07-25T12:20:01Z".to_owned(),
        )
        .await
        .expect("retried stale transition");
    assert!(!first_deltas.is_empty());
    assert!(
        retry_deltas.is_empty(),
        "the durable journal must deduplicate a genuine retry"
    );

    let recovered = next_reconciliation_event(&runtime).await;
    projection
        .apply(
            &scope.scope_id,
            recovered.native_event_id.expect("recovery native ID"),
            recovered.activity,
            "2026-07-25T12:20:02Z".to_owned(),
        )
        .await
        .expect("retry recovery");
    let recovered_snapshot = projection
        .snapshot(&scope.scope)
        .await
        .expect("recovered snapshot");
    assert_eq!(
        recovered_snapshot.observation_state,
        ActivityObservationState::Live
    );
    assert_eq!(
        recovered_snapshot.sections.subagents.state,
        ActivitySectionObservationState::Live
    );

    runtime.stop().await.expect("stop");
    server.abort();
}

#[tokio::test]
async fn reconciliation_native_ids_deduplicate_identical_restart_replay() {
    let state = Arc::new(ReconciliationServerState::new("root"));
    let (endpoint, server) = spawn_reconciliation_server(state).await;
    let first_runtime = OpenCodeSessionRuntime::new(
        &endpoint,
        "opencode-reconciliation-restart-replay",
        "/tmp/project",
        None,
    );
    first_runtime.start().await.expect("first start");
    let first = next_reconciliation_event(&first_runtime).await;
    first_runtime.stop().await.expect("first stop");

    let second_runtime = OpenCodeSessionRuntime::new(
        &endpoint,
        "opencode-reconciliation-restart-replay",
        "/tmp/project",
        None,
    );
    second_runtime.start().await.expect("second start");
    let replay = next_reconciliation_event(&second_runtime).await;
    assert_eq!(
        first.native_event_id, replay.native_event_id,
        "an identical initial snapshot after restart is a deterministic replay"
    );

    let (projection, scope) = durable_activity_projection(
        "opencode-reconciliation-restart-replay",
        ActivityCapabilities::none(),
    )
    .await;
    let first_deltas = projection
        .apply(
            &scope.scope_id,
            first.native_event_id.expect("first native ID"),
            first.activity,
            "2026-07-25T12:30:00Z".to_owned(),
        )
        .await
        .expect("first reconciliation");
    let replay_deltas = projection
        .apply(
            &scope.scope_id,
            replay.native_event_id.expect("replay native ID"),
            replay.activity,
            "2026-07-25T12:30:01Z".to_owned(),
        )
        .await
        .expect("restart replay");
    assert!(!first_deltas.is_empty());
    assert!(
        replay_deltas.is_empty(),
        "the durable journal must deduplicate an identical restart replay"
    );

    second_runtime.stop().await.expect("second stop");
    server.abort();
}

#[tokio::test]
async fn reconciliation_restart_after_stale_recovers_identical_durable_snapshot() {
    let state = Arc::new(ReconciliationServerState::new("root"));
    let (endpoint, server) = spawn_reconciliation_server(state.clone()).await;
    let first_runtime = OpenCodeSessionRuntime::new(
        &endpoint,
        "opencode-reconciliation-restart-after-stale",
        "/tmp/project",
        None,
    );
    let (projection, scope) = durable_activity_projection(
        "opencode-reconciliation-restart-after-stale",
        ActivityCapabilities::none(),
    )
    .await;

    first_runtime.start().await.expect("first start");
    let initial_live = next_reconciliation_event(&first_runtime).await;
    let initial_activity = initial_live.activity.clone();
    let initial_native_event_id = initial_live
        .native_event_id
        .clone()
        .expect("initial live native ID");
    projection
        .apply(
            &scope.scope_id,
            initial_native_event_id.clone(),
            initial_live.activity,
            "2026-07-25T12:40:00Z".to_owned(),
        )
        .await
        .expect("initial live reconciliation");
    timeout(Duration::from_secs(1), async {
        while state.event_connections.load(Ordering::SeqCst) == 0 {
            state.event_connection_opened.notified().await;
        }
    })
    .await
    .expect("first SSE connection opened");
    state.children_failures.store(1, Ordering::SeqCst);
    state.reconnect_burst.notify_waiters();

    let stale = next_reconciliation_event(&first_runtime).await;
    projection
        .apply(
            &scope.scope_id,
            stale.native_event_id.expect("stale native ID"),
            stale.activity,
            "2026-07-25T12:40:01Z".to_owned(),
        )
        .await
        .expect("stale reconciliation");
    let stale_snapshot = projection
        .snapshot(&scope.scope)
        .await
        .expect("stale snapshot");
    assert_eq!(
        stale_snapshot.observation_state,
        ActivityObservationState::Stale
    );
    assert_eq!(
        stale_snapshot.sections.subagents.state,
        ActivitySectionObservationState::Stale
    );
    first_runtime.stop().await.expect("first stop");

    let second_runtime = OpenCodeSessionRuntime::new_with_reconciliation_revision(
        &endpoint,
        "opencode-reconciliation-restart-after-stale",
        "/tmp/project",
        None,
        stale_snapshot.revision,
    );
    second_runtime.start().await.expect("second start");
    let recovered = next_reconciliation_event(&second_runtime).await;
    assert_eq!(
        initial_activity, recovered.activity,
        "the provider snapshot is unchanged across the stale restart"
    );
    let recovered_native_event_id = recovered
        .native_event_id
        .clone()
        .expect("recovered native ID");
    assert_ne!(
        initial_native_event_id, recovered_native_event_id,
        "the durable revision must advance post-stale recovery identity"
    );
    projection
        .apply(
            &scope.scope_id,
            recovered_native_event_id,
            recovered.activity,
            "2026-07-25T12:40:02Z".to_owned(),
        )
        .await
        .expect("post-restart recovery");
    let recovered_snapshot = projection
        .snapshot(&scope.scope)
        .await
        .expect("recovered snapshot");
    assert_eq!(
        recovered_snapshot.observation_state,
        ActivityObservationState::Live
    );
    assert_eq!(
        recovered_snapshot.sections.subagents.state,
        ActivitySectionObservationState::Live
    );

    second_runtime.stop().await.expect("second stop");
    server.abort();
}

#[tokio::test]
async fn reconciliation_downgrades_unsupported_history_without_failing_chat() {
    let state = Arc::new(ReconciliationServerState::new("root"));
    state.children.lock().await.extend([
        (
            "root".to_owned(),
            vec![json!({
                "id": "child",
                "parentID": "root",
                "title": "Child",
                "time": { "created": 1, "updated": 2 }
            })],
        ),
        ("child".to_owned(), Vec::new()),
    ]);
    *state.statuses.lock().await = json!({ "child": { "type": "idle" } });
    state.history_unsupported.store(true, Ordering::SeqCst);
    let (endpoint, server) = spawn_reconciliation_server(state.clone()).await;
    let runtime = OpenCodeSessionRuntime::new(
        &endpoint,
        "opencode-reconciliation-downgrade",
        "/tmp/project",
        None,
    );

    assert_eq!(runtime.start().await.expect("chat still starts"), "root");
    let mutations = next_reconciliation_activity(&runtime).await;
    assert!(mutations.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::SetScope {
            capabilities: ActivityCapabilities {
                actors: true,
                attributed_activity: true,
                background_work: false,
                history_recovery: ActivityHistoryRecovery::Bounded,
                terminal_observation: false,
                targeted_actor_cancellation: false,
            },
            observation_state: ActivityObservationState::Live,
        }
    )));
    assert!(
        timeout(Duration::from_millis(100), runtime.next_event())
            .await
            .is_err(),
        "unsupported activity endpoint does not emit a root runtime error"
    );
    state.reconnect_burst.notify_waiters();
    timeout(Duration::from_secs(2), async {
        while state.status_requests.load(Ordering::SeqCst) < 2 {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("follow-up reconciliation");
    assert_eq!(
        state.history_requests.lock().await.len(),
        2,
        "one child 404 plus one root probe establishes a stable endpoint downgrade"
    );

    runtime.stop().await.expect("stop");
    server.abort();
}

#[tokio::test]
async fn reconciliation_child_history_404_does_not_downgrade_the_endpoint() {
    let state = Arc::new(ReconciliationServerState::new("root"));
    state.children.lock().await.extend([
        (
            "root".to_owned(),
            vec![
                json!({
                    "id": "child-a",
                    "parentID": "root",
                    "title": "Deleted child",
                    "time": { "created": 1, "updated": 2 }
                }),
                json!({
                    "id": "child-b",
                    "parentID": "root",
                    "title": "Live child",
                    "time": { "created": 3, "updated": 4 }
                }),
            ],
        ),
        ("child-a".to_owned(), Vec::new()),
        ("child-b".to_owned(), Vec::new()),
    ]);
    state
        .missing_history_sessions
        .lock()
        .await
        .insert("child-a".to_owned());
    state.histories.lock().await.insert(
        "child-b".to_owned(),
        json!([{
            "info": {
                "id": "message-b",
                "sessionID": "child-b",
                "role": "assistant",
                "time": { "created": 10 }
            },
            "parts": [{
                "id": "part-b",
                "sessionID": "child-b",
                "messageID": "message-b",
                "type": "text",
                "text": "survives child deletion"
            }]
        }]),
    );
    let (endpoint, server) = spawn_reconciliation_server(state.clone()).await;
    let runtime = OpenCodeSessionRuntime::new(
        &endpoint,
        "opencode-reconciliation-child-race",
        "/tmp/project",
        None,
    );

    runtime.start().await.expect("start");
    let mutations = next_reconciliation_activity(&runtime).await;
    assert!(mutations.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::SetScope {
            capabilities: ActivityCapabilities {
                history_recovery: ActivityHistoryRecovery::Full,
                ..
            },
            observation_state: ActivityObservationState::Live,
        }
    )));
    assert!(mutations.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::AppendEntry(entry)
            if entry.detail.as_deref() == Some("survives child deletion")
    )));

    runtime.stop().await.expect("stop");
    server.abort();
}

#[tokio::test]
async fn reconciliation_malformed_success_is_transient_and_recovers() {
    let state = Arc::new(ReconciliationServerState::new("root"));
    state.children.lock().await.extend([
        (
            "root".to_owned(),
            vec![json!({
                "id": "child",
                "parentID": "root",
                "title": "Child",
                "time": { "created": 1, "updated": 2 }
            })],
        ),
        ("child".to_owned(), Vec::new()),
    ]);
    state.malformed_history_responses.store(1, Ordering::SeqCst);
    state.histories.lock().await.insert(
        "child".to_owned(),
        json!([{
            "info": {
                "id": "message",
                "sessionID": "child",
                "role": "assistant",
                "time": { "created": 10 }
            },
            "parts": [{
                "id": "part",
                "sessionID": "child",
                "messageID": "message",
                "type": "text",
                "text": "recovered after malformed response"
            }]
        }]),
    );
    let (endpoint, server) = spawn_reconciliation_server(state).await;
    let runtime = OpenCodeSessionRuntime::new(
        &endpoint,
        "opencode-reconciliation-malformed",
        "/tmp/project",
        None,
    );

    runtime.start().await.expect("start");
    let stale = next_reconciliation_activity(&runtime).await;
    assert!(stale.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::SetScope {
            observation_state: ActivityObservationState::Stale,
            ..
        }
    )));
    let recovered = next_reconciliation_activity(&runtime).await;
    assert!(recovered.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::AppendEntry(entry)
            if entry.detail.as_deref() == Some("recovered after malformed response")
    )));

    runtime.stop().await.expect("stop");
    server.abort();
}

#[tokio::test]
async fn force_history_survives_transport_failure_until_an_authoritative_snapshot_is_accepted() {
    let state = Arc::new(ReconciliationServerState::new("root"));
    state.children.lock().await.extend([
        (
            "root".to_owned(),
            vec![json!({
                "id": "child",
                "parentID": "root",
                "title": "Retry child",
                "time": { "created": 1, "updated": 2 }
            })],
        ),
        ("child".to_owned(), Vec::new()),
    ]);
    state
        .histories
        .lock()
        .await
        .insert("child".to_owned(), json!([]));
    let (endpoint, server) = spawn_reconciliation_server(state.clone()).await;
    let runtime = OpenCodeSessionRuntime::new(
        &endpoint,
        "opencode-force-history-transport-retry",
        "/tmp/project",
        None,
    );

    runtime.start().await.expect("start");
    let _ = next_reconciliation_activity(&runtime).await;
    state.histories.lock().await.insert(
        "child".to_owned(),
        json!([{
            "info": {
                "id": "retry-message",
                "sessionID": "child",
                "role": "assistant",
                "time": { "created": 10 }
            },
            "parts": [{
                "id": "retry-part",
                "sessionID": "child",
                "messageID": "retry-message",
                "type": "text",
                "text": "force retained across transport failure"
            }]
        }]),
    );
    state.malformed_history_responses.store(1, Ordering::SeqCst);
    *state.reconnect_events.lock().await = Some(vec![json!({
        "id": "force-after-complete",
        "type": "server.connected",
        "properties": {}
    })]);
    state.reconnect_burst.notify_waiters();

    let stale = next_reconciliation_activity(&runtime).await;
    assert!(stale.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::SetScope {
            observation_state: ActivityObservationState::Stale,
            ..
        }
    )));
    let recovered = timeout(Duration::from_secs(4), async {
        loop {
            let mutations = next_reconciliation_activity(&runtime).await;
            if mutations.iter().any(|mutation| matches!(
                mutation,
                ProviderActivityMutation::AppendEntry(entry)
                    if entry.detail.as_deref() == Some("force retained across transport failure")
            )) {
                break mutations;
            }
        }
    })
    .await
    .expect("the failed force-history pass must retry the equal-signature child history");
    assert!(recovered.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::SetScope {
            observation_state: ActivityObservationState::Live,
            ..
        }
    )));
    assert_eq!(
        state.history_requests.lock().await.len(),
        3,
        "initial history, failed force refresh, then forced retry"
    );

    runtime.stop().await.expect("stop");
    server.abort();
}

#[tokio::test]
async fn reconciliation_rejects_control_ids_and_stops_discovery_at_depth_sixteen() {
    let state = Arc::new(ReconciliationServerState::new("root"));
    let mut parent = "root".to_owned();
    for depth in 1..=17 {
        let child = format!("depth-{depth}");
        state.children.lock().await.insert(
            parent.clone(),
            vec![json!({
                "id": child,
                "parentID": parent,
                "title": format!("Depth {depth}"),
                "time": { "created": depth, "updated": depth }
            })],
        );
        parent = child;
    }
    state.children.lock().await.insert(parent, Vec::new());
    state
        .children
        .lock()
        .await
        .get_mut("root")
        .expect("root")
        .push(json!({
            "id": "bad\nchild",
            "parentID": "root",
            "title": "Invalid",
            "time": { "created": 30, "updated": 30 }
        }));
    let (endpoint, server) = spawn_reconciliation_server(state.clone()).await;
    let runtime = OpenCodeSessionRuntime::new(
        &endpoint,
        "opencode-reconciliation-validation",
        "/tmp/project",
        None,
    );

    runtime.start().await.expect("start");
    let mutations = next_reconciliation_activity(&runtime).await;
    let requests = state.children_requests.lock().await.clone();
    assert!(!requests.iter().any(|uri| uri.contains("bad%0Achild")));
    assert!(!requests.iter().any(|uri| uri.contains("depth-16/children")));
    assert!(!mutations.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::UpsertActor(actor) if actor.id.contains("depth-17")
    )));
    assert!(
        !state
            .history_requests
            .lock()
            .await
            .iter()
            .any(|uri| { uri.contains("bad%0Achild") || uri.contains("depth-17") })
    );

    runtime.stop().await.expect("stop");
    server.abort();
}

#[tokio::test]
async fn reconciliation_rejects_an_oversized_stream_before_waiting_for_eof() {
    let state = Arc::new(ReconciliationServerState::new("root"));
    state.children.lock().await.extend([
        (
            "root".to_owned(),
            vec![json!({
                "id": "child",
                "parentID": "root",
                "title": "Child",
                "time": { "created": 1, "updated": 2 }
            })],
        ),
        ("child".to_owned(), Vec::new()),
    ]);
    state.oversized_stream.store(true, Ordering::SeqCst);
    let (endpoint, server) = spawn_reconciliation_server(state).await;
    let runtime = OpenCodeSessionRuntime::new(
        &endpoint,
        "opencode-reconciliation-stream-bound",
        "/tmp/project",
        None,
    );

    runtime.start().await.expect("start");
    let stale = timeout(
        Duration::from_secs(1),
        next_reconciliation_activity(&runtime),
    )
    .await
    .expect("oversized first chunk must be rejected before the five-second pass timeout");
    assert!(stale.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::SetScope {
            observation_state: ActivityObservationState::Stale,
            ..
        }
    )));

    runtime.stop().await.expect("stop");
    server.abort();
}

#[tokio::test]
async fn dropping_runtime_ends_reconciliation_retry_worker() {
    let state = Arc::new(ReconciliationServerState::new("root"));
    state.children_failures.store(100, Ordering::SeqCst);
    let (endpoint, server) = spawn_reconciliation_server(state.clone()).await;
    let runtime = OpenCodeSessionRuntime::new(
        &endpoint,
        "opencode-reconciliation-drop",
        "/tmp/project",
        None,
    );

    runtime.start().await.expect("start");
    let _ = next_reconciliation_activity(&runtime).await;
    drop(runtime);
    sleep(Duration::from_millis(50)).await;
    let requests_after_drop = state.children_requests.lock().await.len();
    sleep(Duration::from_millis(500)).await;
    assert_eq!(
        state.children_requests.lock().await.len(),
        requests_after_drop,
        "the retry worker must not retain the runtime after its owner is dropped"
    );

    server.abort();
}

#[tokio::test]
async fn invalid_root_identity_disables_activity_without_breaking_chat_start() {
    for root_id in ["root\ncontrol".to_owned(), "r".repeat(65)] {
        let state = Arc::new(ReconciliationServerState::new(&root_id));
        let (endpoint, server) = spawn_reconciliation_server(state.clone()).await;
        let runtime = OpenCodeSessionRuntime::new(
            &endpoint,
            "opencode-reconciliation-invalid-root",
            "/tmp/project",
            None,
        );

        assert_eq!(
            runtime
                .start()
                .await
                .expect("normal chat start remains available"),
            root_id
        );
        assert_eq!(runtime.collect_events(2).await.len(), 2);
        sleep(Duration::from_millis(300)).await;
        assert!(state.children_requests.lock().await.is_empty());
        assert_eq!(state.status_requests.load(Ordering::SeqCst), 0);
        assert!(state.history_requests.lock().await.is_empty());
        assert!(
            timeout(Duration::from_millis(100), runtime.next_event())
                .await
                .is_err(),
            "invalid root identity must not create an activity tracker or event"
        );

        runtime.stop().await.expect("stop");
        server.abort();
    }
}

#[tokio::test]
async fn dropping_runtime_cancels_a_never_yielding_sse_connection() {
    let state = Arc::new(ReconciliationServerState::new("root"));
    let (endpoint, server) = spawn_reconciliation_server(state.clone()).await;
    let runtime =
        OpenCodeSessionRuntime::new(&endpoint, "opencode-event-pump-drop", "/tmp/project", None);

    runtime.start().await.expect("start");
    timeout(Duration::from_secs(1), async {
        while state.event_connections.load(Ordering::SeqCst) == 0 {
            state.event_connection_opened.notified().await;
        }
    })
    .await
    .expect("SSE connection opened");
    drop(runtime);
    timeout(Duration::from_millis(500), async {
        while state.event_disconnects.load(Ordering::SeqCst) == 0 {
            state.event_connection_closed.notified().await;
        }
    })
    .await
    .expect("owner drop must cancel the blocked SSE response");

    server.abort();
}

#[tokio::test]
async fn reconciliation_marks_transient_failure_stale_then_recovers_with_bounded_retry() {
    let state = Arc::new(ReconciliationServerState::new("root"));
    state.children.lock().await.extend([
        (
            "root".to_owned(),
            vec![json!({
                "id": "child",
                "parentID": "root",
                "title": "Child",
                "time": { "created": 1, "updated": 2 }
            })],
        ),
        ("child".to_owned(), Vec::new()),
    ]);
    state
        .histories
        .lock()
        .await
        .insert("child".to_owned(), json!([]));
    state.children_failures.store(1, Ordering::SeqCst);
    let (endpoint, server) = spawn_reconciliation_server(state.clone()).await;
    let runtime = OpenCodeSessionRuntime::new(
        &endpoint,
        "opencode-reconciliation-retry",
        "/tmp/project",
        None,
    );

    runtime.start().await.expect("start");
    let stale = next_reconciliation_activity(&runtime).await;
    assert!(stale.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::SetScope {
            observation_state: ActivityObservationState::Stale,
            ..
        }
    )));
    let recovered = next_reconciliation_activity(&runtime).await;
    assert!(recovered.iter().any(|mutation| matches!(
        mutation,
        ProviderActivityMutation::SetScope {
            capabilities: ActivityCapabilities {
                actors: true,
                attributed_activity: true,
                background_work: false,
                history_recovery: ActivityHistoryRecovery::Full,
                terminal_observation: false,
                targeted_actor_cancellation: false,
            },
            observation_state: ActivityObservationState::Live,
        }
    )));
    assert_eq!(state.status_requests.load(Ordering::SeqCst), 1);
    assert_eq!(state.children_requests.lock().await.len(), 3);

    runtime.stop().await.expect("stop");
    server.abort();
}

#[tokio::test]
async fn reconciliation_shutdown_cancels_an_inflight_pass_without_waiting_for_http_timeout() {
    let state = Arc::new(ReconciliationServerState::new("root"));
    state.slow_children.store(true, Ordering::SeqCst);
    let (endpoint, server) = spawn_reconciliation_server(state.clone()).await;
    let runtime = OpenCodeSessionRuntime::new(
        &endpoint,
        "opencode-reconciliation-cancel",
        "/tmp/project",
        None,
    );

    runtime.start().await.expect("start");
    timeout(Duration::from_secs(2), state.children_entered.notified())
        .await
        .expect("reconciliation request entered");
    timeout(Duration::from_millis(500), runtime.stop())
        .await
        .expect("shutdown cancels reconciliation")
        .expect("stop");

    server.abort();
}

#[tokio::test]
async fn reconciliation_whole_pass_times_out_after_five_seconds_and_marks_observation_stale() {
    let state = Arc::new(ReconciliationServerState::new("root"));
    state.slow_children.store(true, Ordering::SeqCst);
    let (endpoint, server) = spawn_reconciliation_server(state).await;
    let runtime = OpenCodeSessionRuntime::new(
        &endpoint,
        "opencode-reconciliation-timeout",
        "/tmp/project",
        None,
    );

    let started_at = tokio::time::Instant::now();
    runtime.start().await.expect("start");
    let stale = timeout(Duration::from_millis(5_750), async {
        loop {
            let event = runtime.next_event().await.expect("runtime event");
            if event.activity.iter().any(|mutation| {
                matches!(
                    mutation,
                    ProviderActivityMutation::SetScope {
                        observation_state: ActivityObservationState::Stale,
                        ..
                    }
                )
            }) {
                break event;
            }
        }
    })
    .await
    .expect("five-second reconciliation timeout");
    assert_eq!(stale.event_type, "activity.native");
    assert!(
        started_at.elapsed() >= Duration::from_millis(4_900),
        "the whole-pass deadline must not fire substantially before five seconds"
    );

    runtime.stop().await.expect("stop");
    server.abort();
}

async fn next_reconciliation_activity(
    runtime: &OpenCodeSessionRuntime,
) -> Vec<ProviderActivityMutation> {
    next_reconciliation_event(runtime).await.activity
}

async fn next_reconciliation_event(
    runtime: &OpenCodeSessionRuntime,
) -> opencode::OpenCodeRuntimeEvent {
    timeout(Duration::from_secs(4), async {
        loop {
            let event = runtime
                .next_event()
                .await
                .expect("OpenCode reconciliation event");
            if !event.activity.is_empty() {
                assert_eq!(event.event_type, "activity.native");
                assert!(event.native_event_id.is_some());
                break event;
            }
        }
    })
    .await
    .expect("reconciliation activity timeout")
}

async fn durable_activity_projection(
    thread_id: &str,
    capabilities: ActivityCapabilities,
) -> (ActivityProjection, ActivityScopeSeed) {
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let projection = ActivityProjection::new(ActivityRepository::new(database));
    let scope = ActivityScopeSeed::thread(
        format!("thread:{thread_id}"),
        thread_id,
        "opencode",
        Some("opencode"),
        capabilities,
    )
    .expect("valid scope");
    projection
        .ensure_scope(scope.clone())
        .await
        .expect("activity scope");
    (projection, scope)
}

struct ReconciliationServerState {
    root_id: String,
    children: Mutex<HashMap<String, Vec<Value>>>,
    statuses: Mutex<Value>,
    histories: Mutex<HashMap<String, Value>>,
    children_requests: Mutex<Vec<String>>,
    status_requests: AtomicUsize,
    history_requests: Mutex<Vec<String>>,
    reconnect_burst: Notify,
    reconnect_events: Mutex<Option<Vec<Value>>>,
    reconnect_payloads: Mutex<Option<Vec<String>>>,
    reconnect_raw_chunks: Mutex<Option<Vec<Vec<u8>>>>,
    reconnect_pause_after: AtomicUsize,
    reconnect_continue: Notify,
    event_connections: AtomicUsize,
    event_disconnects: AtomicUsize,
    event_connection_opened: Notify,
    event_connection_closed: Notify,
    children_failures: AtomicUsize,
    children_unsupported: AtomicBool,
    history_unsupported: AtomicBool,
    missing_history_sessions: Mutex<HashSet<String>>,
    malformed_history_responses: AtomicUsize,
    oversized_stream: AtomicBool,
    slow_children: AtomicBool,
    hold_children: AtomicBool,
    children_entered: Notify,
    children_continue: Notify,
}

impl ReconciliationServerState {
    fn new(root_id: &str) -> Self {
        Self {
            root_id: root_id.to_owned(),
            children: Mutex::new(HashMap::new()),
            statuses: Mutex::new(json!({})),
            histories: Mutex::new(HashMap::new()),
            children_requests: Mutex::new(Vec::new()),
            status_requests: AtomicUsize::new(0),
            history_requests: Mutex::new(Vec::new()),
            reconnect_burst: Notify::new(),
            reconnect_events: Mutex::new(None),
            reconnect_payloads: Mutex::new(None),
            reconnect_raw_chunks: Mutex::new(None),
            reconnect_pause_after: AtomicUsize::new(usize::MAX),
            reconnect_continue: Notify::new(),
            event_connections: AtomicUsize::new(0),
            event_disconnects: AtomicUsize::new(0),
            event_connection_opened: Notify::new(),
            event_connection_closed: Notify::new(),
            children_failures: AtomicUsize::new(0),
            children_unsupported: AtomicBool::new(false),
            history_unsupported: AtomicBool::new(false),
            missing_history_sessions: Mutex::new(HashSet::new()),
            malformed_history_responses: AtomicUsize::new(0),
            oversized_stream: AtomicBool::new(false),
            slow_children: AtomicBool::new(false),
            hold_children: AtomicBool::new(false),
            children_entered: Notify::new(),
            children_continue: Notify::new(),
        }
    }
}

async fn spawn_reconciliation_server(
    state: Arc<ReconciliationServerState>,
) -> (String, JoinHandle<()>) {
    let app = Router::new()
        .route("/session", post(create_reconciliation_session))
        .route("/event", get(subscribe_reconciliation_events))
        .route(
            "/session/{session_id}/prompt_async",
            post(|| async { Json(json!({})) }),
        )
        .route(
            "/session/{session_id}/children",
            get(list_reconciliation_children),
        )
        .route("/session/status", get(list_reconciliation_statuses))
        .route(
            "/session/{session_id}/message",
            get(list_reconciliation_messages),
        )
        .with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("addr");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    (format!("http://{address}"), server)
}

async fn create_reconciliation_session(
    State(state): State<Arc<ReconciliationServerState>>,
) -> Json<Value> {
    Json(json!({ "id": state.root_id }))
}

async fn subscribe_reconciliation_events(
    State(state): State<Arc<ReconciliationServerState>>,
) -> Response {
    state.event_connections.fetch_add(1, Ordering::SeqCst);
    state.event_connection_opened.notify_waiters();
    let guard = ReconciliationEventConnectionGuard {
        state: state.clone(),
    };
    let raw_chunks = state.reconnect_raw_chunks.lock().await.take();
    if let Some(chunks) = raw_chunks {
        let stream = stream::once(async move {
            let _guard = guard;
            state.reconnect_burst.notified().await;
            chunks
        })
        .flat_map(|chunks| {
            stream::iter(
                chunks
                    .into_iter()
                    .map(|chunk| Ok::<Bytes, std::convert::Infallible>(Bytes::from(chunk))),
            )
        })
        // This endpoint models a live provider subscription. The reconnect
        // burst is finite, but the SSE connection itself must remain open
        // until the client cancels it.
        .chain(stream::pending());
        return Response::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/event-stream")
            .body(Body::from_stream(stream))
            .expect("raw SSE response");
    }
    let stream = stream::once(async move {
        let _guard = guard;
        state.reconnect_burst.notified().await;
        let payloads = match state.reconnect_payloads.lock().await.take() {
            Some(payloads) => payloads,
            None => state
                .reconnect_events
                .lock()
                .await
                .take()
                .unwrap_or_else(|| {
                    vec![
                        json!({ "id": "connected-1", "type": "server.connected", "properties": {} }),
                        json!({ "id": "connected-2", "type": "server.connected", "properties": {} }),
                        json!({ "id": "connected-3", "type": "server.connected", "properties": {} }),
                    ]
                })
                .into_iter()
                .map(|event| event.to_string())
                .collect(),
        };
        (state, payloads)
    })
    .flat_map(|(state, payloads)| {
        stream::iter(payloads.into_iter().enumerate()).then(move |(index, payload)| {
            let state = state.clone();
            async move {
                if index == state.reconnect_pause_after.load(Ordering::SeqCst) {
                    state.reconnect_continue.notified().await;
                }
                Ok::<Event, std::convert::Infallible>(Event::default().data(payload))
            }
        })
    })
    // A finite test burst is not provider EOF. Keep the subscription alive so
    // tests exercise reconnect reconciliation without racing stream teardown.
    .chain(stream::pending());
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

struct ReconciliationEventConnectionGuard {
    state: Arc<ReconciliationServerState>,
}

impl Drop for ReconciliationEventConnectionGuard {
    fn drop(&mut self) {
        self.state.event_disconnects.fetch_add(1, Ordering::SeqCst);
        self.state.event_connection_closed.notify_waiters();
    }
}

fn tool_history(session_id: &str, count: usize) -> Value {
    Value::Array(
        (0..count)
            .map(|index| {
                json!({
                    "info": {
                        "id": format!("{session_id}-message-{index}"),
                        "sessionID": session_id,
                        "role": "assistant",
                        "time": { "created": index + 10 }
                    },
                    "parts": [{
                        "id": format!("{session_id}-part-{index}"),
                        "sessionID": session_id,
                        "messageID": format!("{session_id}-message-{index}"),
                        "type": "tool",
                        "callID": format!("{session_id}-call-{index}"),
                        "tool": "bash",
                        "state": { "status": "completed" }
                    }]
                })
            })
            .collect(),
    )
}

async fn list_reconciliation_children(
    Path(session_id): Path<String>,
    OriginalUri(uri): OriginalUri,
    State(state): State<Arc<ReconciliationServerState>>,
) -> Response {
    state.children_requests.lock().await.push(uri.to_string());
    state.children_entered.notify_waiters();
    if state.slow_children.load(Ordering::SeqCst) {
        std::future::pending::<()>().await;
    }
    if state.hold_children.load(Ordering::SeqCst) {
        state.children_continue.notified().await;
    }
    if state.children_unsupported.load(Ordering::SeqCst) {
        return StatusCode::NOT_FOUND.into_response();
    }
    if state
        .children_failures
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
            remaining.checked_sub(1)
        })
        .is_ok()
    {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    Json(
        state
            .children
            .lock()
            .await
            .get(&session_id)
            .cloned()
            .unwrap_or_default(),
    )
    .into_response()
}

async fn list_reconciliation_statuses(
    State(state): State<Arc<ReconciliationServerState>>,
) -> Json<Value> {
    state.status_requests.fetch_add(1, Ordering::SeqCst);
    Json(state.statuses.lock().await.clone())
}

async fn list_reconciliation_messages(
    Path(session_id): Path<String>,
    OriginalUri(uri): OriginalUri,
    State(state): State<Arc<ReconciliationServerState>>,
) -> Response {
    state.history_requests.lock().await.push(uri.to_string());
    if state.history_unsupported.load(Ordering::SeqCst) {
        return StatusCode::NOT_FOUND.into_response();
    }
    if state
        .missing_history_sessions
        .lock()
        .await
        .contains(&session_id)
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    if state
        .malformed_history_responses
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
            remaining.checked_sub(1)
        })
        .is_ok()
    {
        return (StatusCode::OK, "not-json").into_response();
    }
    if state.oversized_stream.load(Ordering::SeqCst) {
        let first = stream::once(async {
            Ok::<Bytes, std::convert::Infallible>(Bytes::from(vec![b'x'; 4 * 1024 * 1024 + 1]))
        });
        let tail = stream::pending::<Result<Bytes, std::convert::Infallible>>();
        return Response::new(Body::from_stream(first.chain(tail)));
    }
    Json(
        state
            .histories
            .lock()
            .await
            .get(&session_id)
            .cloned()
            .unwrap_or_else(|| json!([])),
    )
    .into_response()
}

#[derive(Default)]
struct TestServerState {
    prompt_received: Notify,
    question_replied: Notify,
    prompt_body: Mutex<Option<Value>>,
    deleted_messages: Mutex<Vec<String>>,
    abort_count: Mutex<usize>,
    messages: Mutex<Vec<Value>>,
    permission_reply: Mutex<Option<Value>>,
    command_body: Mutex<Option<Value>>,
}

async fn create_session(State(_state): State<Arc<TestServerState>>) -> Json<Value> {
    Json(json!({ "id": "session-1" }))
}

async fn invalid_session() -> Json<Value> {
    Json(json!({}))
}

async fn invalid_json_session() -> &'static str {
    "not-json"
}

async fn resume_session(Path(session_id): Path<String>) -> StatusCode {
    if session_id == "bad" {
        StatusCode::BAD_GATEWAY
    } else if session_id == "gone" {
        StatusCode::NOT_FOUND
    } else {
        StatusCode::OK
    }
}

async fn reject_request() -> StatusCode {
    StatusCode::BAD_GATEWAY
}

async fn register_mcp(
    Query(query): Query<std::collections::HashMap<String, String>>,
    Json(body): Json<Value>,
) -> Json<Value> {
    assert_eq!(
        query.get("directory").map(String::as_str),
        Some("C:/repo with spaces")
    );
    assert_eq!(
        body,
        json!({
            "name": "bibcode",
            "config": {
                "type": "remote",
                "url": "http://127.0.0.1:3773/mcp",
                "headers": { "Authorization": "Bearer secret" },
                "oauth": false,
            }
        })
    );
    Json(json!({ "bibcode": { "status": "connected" } }))
}

async fn create_authenticated_session(
    State(_state): State<Arc<TestServerState>>,
    headers: HeaderMap,
) -> Json<Value> {
    assert_eq!(
        headers
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
        Some("Basic b3BlbmNvZGU6c2VjcmV0")
    );
    Json(json!({ "id": "authenticated-session" }))
}

async fn subscribe_events(
    State(state): State<Arc<TestServerState>>,
    Query(_query): Query<std::collections::HashMap<String, String>>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let initial_events = vec![
        json!({
            "type": "message.updated",
            "properties": {
                "info": {
                    "id": "user-1",
                    "role": "user",
                    "sessionID": "session-1"
                }
            }
        }),
        json!({
            "type": "message.part.updated",
            "properties": {
                "part": {
                    "id": "part-user-1",
                    "messageID": "user-1",
                    "sessionID": "session-1",
                    "type": "text",
                    "text": "hello"
                }
            }
        }),
        json!({
            "type": "message.updated",
            "properties": {
                "info": {
                    "id": "assistant-1",
                    "role": "assistant",
                    "sessionID": "session-1"
                }
            }
        }),
        json!({
            "type": "message.part.updated",
            "properties": {
                "part": {
                    "id": "part-assistant-1",
                    "messageID": "assistant-1",
                    "sessionID": "session-1",
                    "type": "text",
                    "text": "Hello"
                }
            }
        }),
        json!({
            "type": "question.asked",
            "properties": {
                "sessionID": "session-1",
                "requestID": "question-1",
                "questions": [
                    {
                        "header": "Scope",
                        "question": "Scope",
                        "options": [{ "label": "Workspace" }, { "label": "Session" }]
                    }
                ]
            }
        }),
    ];
    let initial_state = state.clone();
    let initial = stream::once(async move {
        initial_state.prompt_received.notified().await;
        initial_events
    })
    .flat_map(|events| {
        stream::iter(
            events
                .into_iter()
                .map(|event| Ok(Event::default().data(event.to_string()))),
        )
    });
    let tail_state = state.clone();
    let tail = stream::once(async move {
        tail_state.question_replied.notified().await;
        [
            json!({
                "type": "message.part.updated",
                "properties": {
                    "part": {
                        "id": "part-assistant-1",
                        "messageID": "assistant-1",
                        "sessionID": "session-1",
                        "type": "text",
                        "text": "Hello world"
                    }
                }
            }),
            json!({
                "type": "session.status",
                "properties": {
                    "sessionID": "session-1",
                    "status": { "type": "idle" }
                }
            }),
        ]
    })
    .flat_map(|events| {
        stream::iter(
            events
                .into_iter()
                .map(|event| Ok(Event::default().data(event.to_string()))),
        )
    });
    let stream = initial.chain(tail);
    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn subscribe_root_assistant_identity_events(
    State(state): State<Arc<TestServerState>>,
    Query(_query): Query<std::collections::HashMap<String, String>>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let events = [
        json!({
            "type": "message.updated",
            "properties": {
                "info": {
                    "id": "opencode-message-1",
                    "sessionID": "session-1",
                    "role": "assistant"
                }
            }
        }),
        json!({
            "type": "message.part.updated",
            "properties": {
                "part": {
                    "id": "opencode-part-1",
                    "messageID": "opencode-message-1",
                    "sessionID": "session-1",
                    "type": "text",
                    "text": "First."
                }
            }
        }),
        json!({
            "type": "message.updated",
            "properties": {
                "sessionID": "session-1",
                "info": {
                    "id": "opencode-message-1",
                    "sessionID": "session-1",
                    "role": "assistant",
                    "time": { "completed": 20 },
                    "finish": "stop"
                }
            }
        }),
        json!({
            "type": "message.updated",
            "properties": {
                "info": {
                    "id": "opencode-message-2",
                    "sessionID": "session-1",
                    "role": "assistant"
                }
            }
        }),
        json!({
            "type": "message.part.updated",
            "properties": {
                "part": {
                    "id": "opencode-part-2",
                    "messageID": "opencode-message-2",
                    "sessionID": "session-1",
                    "type": "text",
                    "text": "Second."
                }
            }
        }),
        json!({
            "type": "session.status",
            "properties": {
                "sessionID": "session-1",
                "status": { "type": "idle" }
            }
        }),
    ];
    let stream = stream::once(async move {
        state.prompt_received.notified().await;
        events
    })
    .flat_map(|events| {
        stream::iter(
            events
                .into_iter()
                .map(|event| Ok(Event::default().data(event.to_string()))),
        )
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn subscribe_permission_events(
    State(_state): State<Arc<TestServerState>>,
    Query(_query): Query<std::collections::HashMap<String, String>>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let event = json!({
        "type": "permission.asked",
        "properties": {
            "sessionID": "session-1",
            "id": "permission-1",
            "permission": "bash",
            "patterns": ["git status"]
        }
    });
    Sse::new(stream::iter(vec![Ok(
        Event::default().data(event.to_string())
    )]))
    .keep_alive(KeepAlive::default())
}

async fn subscribe_question_and_permission_events(
    State(_state): State<Arc<TestServerState>>,
    Query(_query): Query<std::collections::HashMap<String, String>>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let events = [
        json!({
            "type": "question.asked",
            "properties": {
                "sessionID": "transport-session",
                "requestID": "transport-question",
                "questions": [
                    { "header": "Scope", "question": "Scope?", "options": [] },
                    { "header": "Notes", "question": "Notes?", "options": [] },
                    { "header": "Ignored", "question": "Ignored?", "options": [] }
                ]
            }
        }),
        json!({
            "type": "permission.asked",
            "properties": {
                "sessionID": "transport-session",
                "requestID": "transport-permission",
                "permission": "bash"
            }
        }),
    ];
    Sse::new(stream::iter(
        events
            .into_iter()
            .map(|event| Ok(Event::default().data(event.to_string()))),
    ))
    .keep_alive(KeepAlive::default())
}

async fn reply_permission(
    Path(request_id): Path<String>,
    State(state): State<Arc<TestServerState>>,
    Json(body): Json<Value>,
) -> Json<Value> {
    assert_eq!(request_id, "permission-1");
    *state.permission_reply.lock().await = Some(body);
    Json(json!({ "ok": true }))
}

async fn reply_question(
    Path(_request_id): Path<String>,
    State(state): State<Arc<TestServerState>>,
    Json(_body): Json<Value>,
) -> Json<Value> {
    let notify_state = state.clone();
    tokio::spawn(async move {
        sleep(Duration::from_millis(10)).await;
        notify_state.question_replied.notify_waiters();
    });
    Json(json!({ "ok": true }))
}

async fn prompt_async(
    Path(_session_id): Path<String>,
    State(state): State<Arc<TestServerState>>,
    Json(body): Json<Value>,
) -> StatusCode {
    *state.prompt_body.lock().await = Some(body);
    state.prompt_received.notify_one();
    StatusCode::NO_CONTENT
}

async fn run_command(
    Path(session_id): Path<String>,
    State(state): State<Arc<TestServerState>>,
    Json(body): Json<Value>,
) -> StatusCode {
    assert_eq!(session_id, "session-1");
    *state.command_body.lock().await = Some(body);
    StatusCode::OK
}

async fn subscribe_error_events(
    State(state): State<Arc<TestServerState>>,
    Query(_query): Query<std::collections::HashMap<String, String>>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    let events = vec![
        json!({
            "type": "message.updated",
            "properties": {
                "sessionID": "session-1",
                "info": {
                    "id": "user-error-1",
                    "role": "user",
                    "sessionID": "session-1"
                }
            }
        }),
        json!({
            "type": "message.part.updated",
            "properties": {
                "sessionID": "session-1",
                "part": {
                    "id": "part-user-error-1",
                    "messageID": "user-error-1",
                    "sessionID": "session-1",
                    "type": "text",
                    "text": "hello"
                }
            }
        }),
        json!({
            "type": "session.status",
            "properties": {
                "sessionID": "session-1",
                "status": { "type": "busy" }
            }
        }),
        json!({
            "type": "session.error",
            "properties": {
                "sessionID": "session-1",
                "error": {
                    "name": "UnknownError",
                    "data": { "message": "Model not found: openai/gpt-5" }
                }
            }
        }),
        json!({
            "type": "session.status",
            "properties": {
                "sessionID": "session-1",
                "status": { "type": "idle" }
            }
        }),
    ];
    let stream = stream::once(async move {
        state.prompt_received.notified().await;
        events
    })
    .flat_map(|events| {
        stream::iter(
            events
                .into_iter()
                .map(|event| Ok(Event::default().data(event.to_string()))),
        )
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn error_prompt_async(
    Path(_session_id): Path<String>,
    State(state): State<Arc<TestServerState>>,
    Json(body): Json<Value>,
) -> StatusCode {
    *state.prompt_body.lock().await = Some(body);
    state.prompt_received.notify_one();
    StatusCode::NO_CONTENT
}

async fn subscribe_pending_events(
    State(_state): State<Arc<TestServerState>>,
    Query(_query): Query<std::collections::HashMap<String, String>>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    Sse::new(stream::pending())
}

async fn prompt_async_immediate(
    Path(_session_id): Path<String>,
    State(_state): State<Arc<TestServerState>>,
    Json(_body): Json<Value>,
) -> StatusCode {
    StatusCode::NO_CONTENT
}

async fn delete_message(
    Path((_session_id, message_id)): Path<(String, String)>,
    State(state): State<Arc<TestServerState>>,
) -> StatusCode {
    state.deleted_messages.lock().await.push(message_id);
    StatusCode::NO_CONTENT
}

async fn abort_session(
    Path(_session_id): Path<String>,
    State(state): State<Arc<TestServerState>>,
) -> Json<Value> {
    *state.abort_count.lock().await += 1;
    Json(json!({ "ok": true }))
}

async fn list_messages(
    Path(_session_id): Path<String>,
    State(state): State<Arc<TestServerState>>,
) -> Json<Value> {
    Json(json!({ "data": state.messages.lock().await.clone() }))
}

async fn revert_session(
    Path(_session_id): Path<String>,
    State(state): State<Arc<TestServerState>>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let message_id = body
        .get("messageID")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut messages = state.messages.lock().await;
    if let Some(message_id) = message_id {
        let target_index = messages.iter().position(|entry| {
            entry
                .get("info")
                .and_then(|info| info.get("id"))
                .and_then(Value::as_str)
                == Some(message_id.as_str())
        });
        if let Some(target_index) = target_index {
            messages.truncate(target_index + 1);
        }
    } else {
        messages.clear();
    }
    Json(json!({ "ok": true }))
}

fn fixture(name: &str) -> Value {
    serde_json::from_str(
        &std::fs::read_to_string(fixture_directory().join(name)).expect("fixture file"),
    )
    .expect("valid fixture")
}

fn fixture_names_from_manifest() -> Vec<String> {
    fixture("manifest.json")["fixtures"]
        .as_array()
        .expect("fixture manifest list")
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .expect("fixture manifest file name")
                .to_owned()
        })
        .collect()
}

fn activity_fixture(name: &str) -> Value {
    assert!(
        fixture_names_from_manifest()
            .iter()
            .any(|entry| entry == name),
        "fixture manifest must include {name}",
    );
    fixture(name)
}

fn assert_opencode_1184_metadata(fixture: &Value) {
    let metadata = fixture["metadata"]
        .as_object()
        .expect("versioned fixture metadata");
    assert_eq!(metadata["provider"], "opencode");
    assert_eq!(metadata["producerVersion"], "1.18.4");
    assert_eq!(metadata["schemaSource"], "GET /doc");
    assert_eq!(metadata["capturedAt"], "2026-07-24T00:00:00Z");
    assert_eq!(
        metadata["redactions"],
        json!([
            "[redacted workspace]",
            "[redacted command arguments]",
            "[redacted provider failure]"
        ])
    );
}

fn raw_sse_frames(fixture: &Value) -> Vec<Value> {
    fixture["rawSseFrames"]
        .as_array()
        .expect("raw SSE frame list")
        .iter()
        .map(|frame| parse_raw_sse_frame(frame.as_str().expect("raw SSE frame")))
        .collect()
}

fn parse_raw_sse_frame(raw: &str) -> Value {
    assert!(raw.ends_with("\n\n"), "SSE frame terminator");
    let mut lines = raw.lines();
    let header_id = lines
        .next()
        .and_then(|line| line.strip_prefix("id: "))
        .expect("SSE id header");
    assert_eq!(lines.next(), Some("event: message"), "SSE event header");
    let data = lines
        .next()
        .and_then(|line| line.strip_prefix("data: "))
        .expect("SSE data header");
    assert!(
        lines.all(str::is_empty),
        "one JSON payload per captured frame"
    );
    let payload: Value = serde_json::from_str(data).expect("SSE JSON payload");
    assert_eq!(
        payload["id"], header_id,
        "top-level event ID matches SSE header"
    );
    payload
}

fn sse_frame<'a>(frames: &'a [Value], id: &str) -> &'a Value {
    frames
        .iter()
        .find(|frame| frame["id"] == id)
        .unwrap_or_else(|| panic!("SSE frame {id}"))
}

fn children_response<'a>(responses: &'a [Value], parent_session_id: &str) -> &'a Vec<Value> {
    responses
        .iter()
        .find(|response| response["parentSessionID"] == parent_session_id)
        .unwrap_or_else(|| panic!("children response for {parent_session_id}"))["response"]
        .as_array()
        .expect("children endpoint returns a bare array")
}

fn message_response<'a>(responses: &'a [Value], session_id: &str) -> &'a Vec<Value> {
    responses
        .iter()
        .find(|response| response["sessionID"] == session_id)
        .unwrap_or_else(|| panic!("message response for {session_id}"))["response"]
        .as_array()
        .expect("message endpoint returns a bare array")
}

fn part_identity(part: &Value) -> (String, String, String) {
    (
        part["sessionID"]
            .as_str()
            .expect("part session ID")
            .to_owned(),
        part["messageID"]
            .as_str()
            .expect("part message ID")
            .to_owned(),
        part["id"].as_str().expect("part ID").to_owned(),
    )
}

fn stable_fixture(name: &str) -> Vec<opencode::OpenCodeRuntimeEventStableView> {
    serde_json::from_value(fixture(name)).expect("stable fixture")
}

fn normalize_turn_ids(
    events: &mut [opencode::OpenCodeRuntimeEventStableView],
    actual_turn_id: &str,
) {
    for event in events {
        if event.turn_id.as_deref() == Some(actual_turn_id) {
            event.turn_id = Some("turn-3".to_owned());
        }
    }
}

fn fixture_directory() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../packages/contracts/fixtures/opencode-provider")
}
