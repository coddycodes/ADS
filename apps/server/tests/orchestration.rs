use coddy_studio_server::orchestration::engine::{
    EngineOptions, OrchestrationCommand, OrchestrationEngine, OrchestrationError, TestHooks,
    load_snapshot,
};
use coddy_studio_server::orchestration::{TurnDeliveryMode, TurnDeliveryState};
use coddy_studio_server::persistence::{
    Database, NewOrchestrationEvent, Repositories, run_migrations,
};
use serde_json::{Value, json};

const CREATED_AT: &str = "2026-07-10T10:00:00.000Z";

async fn migrated_repositories() -> Repositories {
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    Repositories::new(database)
}

fn command_values() -> Vec<Value> {
    vec![
        json!({"type":"project.create","commandId":"c01","projectId":"p1","title":"Project","workspaceRoot":"C:/repo","createWorkspaceRootIfMissing":true,"defaultModelSelection":null,"createdAt":CREATED_AT}),
        json!({"type":"project.meta.update","commandId":"c02","projectId":"p1","title":"Renamed","defaultModelSelection":{"instanceId":"codex","model":"gpt-5"},"scripts":[],"worktreeDiscovery":{"visibility":"shown","initialPromptDismissedAt":"2026-07-10T10:00:00.000Z","baselinePaths":["C:/repo-worktrees"]}}),
        json!({"type":"project.delete","commandId":"c23","projectId":"p1","force":true}),
        json!({"type":"thread.create","commandId":"c03","threadId":"t1","projectId":"p1","title":"Thread","modelSelection":{"instanceId":"codex","model":"gpt-5"},"runtimeMode":"full-access","branch":null,"worktreePath":null,"createdAt":CREATED_AT}),
        json!({"type":"thread.delete","commandId":"c22","threadId":"t1"}),
        json!({"type":"thread.archive","commandId":"c20","threadId":"t1"}),
        json!({"type":"thread.unarchive","commandId":"c21","threadId":"t1"}),
        json!({"type":"thread.meta.update","commandId":"c04","threadId":"t1","title":"Thread 2","branch":"main","worktreePath":null}),
        json!({"type":"thread.runtime-mode.set","commandId":"c05","threadId":"t1","runtimeMode":"approval-required","createdAt":CREATED_AT}),
        json!({"type":"thread.interaction-mode.set","commandId":"c06","threadId":"t1","interactionMode":"plan","createdAt":CREATED_AT}),
        json!({"type":"thread.turn.start","commandId":"c07","threadId":"t1","message":{"messageId":"m-user","role":"user","text":"hello","attachments":[]},"createdAt":CREATED_AT}),
        json!({"type":"thread.turn.interrupt","commandId":"c08","threadId":"t1","turnId":"turn-1","createdAt":CREATED_AT}),
        json!({"type":"thread.approval.respond","commandId":"c09","threadId":"t1","requestId":"r1","decision":"acceptForSession","createdAt":CREATED_AT}),
        json!({"type":"thread.user-input.respond","commandId":"c10","threadId":"t1","requestId":"r2","answers":{"question":"answer"},"createdAt":CREATED_AT}),
        json!({"type":"thread.checkpoint.revert","commandId":"c11","threadId":"t1","turnCount":0,"createdAt":CREATED_AT}),
        json!({"type":"thread.session.stop","commandId":"c12","threadId":"t1","createdAt":CREATED_AT}),
        json!({"type":"thread.session.set","commandId":"c13","threadId":"t1","session":{"threadId":"t1","status":"running","providerName":"Codex","activeTurnId":"turn-1","lastError":null,"updatedAt":CREATED_AT},"createdAt":CREATED_AT}),
        json!({"type":"thread.message.assistant.delta","commandId":"c14","threadId":"t1","messageId":"m-assistant","delta":"hel","turnId":"turn-1","createdAt":CREATED_AT}),
        json!({"type":"thread.message.assistant.complete","commandId":"c15","threadId":"t1","messageId":"m-assistant","turnId":"turn-1","createdAt":CREATED_AT}),
        json!({"type":"thread.proposed-plan.upsert","commandId":"c16","threadId":"t1","proposedPlan":{"id":"plan-1","turnId":"turn-1","planMarkdown":"Do it","createdAt":CREATED_AT,"updatedAt":CREATED_AT},"createdAt":CREATED_AT}),
        json!({"type":"thread.turn.diff.complete","commandId":"c17","threadId":"t1","turnId":"turn-1","completedAt":CREATED_AT,"checkpointRef":"ref-1","status":"ready","files":[{"path":"a.rs","kind":"modified","additions":2,"deletions":1}],"assistantMessageId":"m-assistant","checkpointTurnCount":1,"createdAt":CREATED_AT}),
        json!({"type":"thread.activity.append","commandId":"c18","threadId":"t1","activity":{"id":"activity-1","tone":"tool","kind":"command","summary":"ran","payload":{"requestId":"r3"},"turnId":"turn-1","createdAt":CREATED_AT},"createdAt":CREATED_AT}),
        json!({"type":"thread.revert.complete","commandId":"c19","threadId":"t1","turnCount":0,"createdAt":CREATED_AT}),
        json!({"type":"thread.turn.start","commandId":"c24","threadId":"t1","message":{"messageId":"m-queued","role":"user","text":"next task","attachments":[]},"runtimeMode":"full-access","interactionMode":"default","queued":true,"createdAt":CREATED_AT}),
        json!({"type":"thread.turn.steer","commandId":"c25","threadId":"t1","messageId":"m-queued","createdAt":CREATED_AT}),
        json!({"type":"thread.turn.promote","commandId":"c26","threadId":"t1","messageId":"m-queued","createdAt":CREATED_AT}),
        json!({"type":"thread.turn-delivery.resolve","commandId":"c27","threadId":"t1","messageId":"m-queued","action":"cancel","createdAt":CREATED_AT}),
    ]
}

fn decode(value: Value) -> OrchestrationCommand {
    serde_json::from_value(value).expect("contract command decodes")
}

#[test]
fn queued_delivery_state_and_modes_round_trip() {
    let queued: TurnDeliveryState = serde_json::from_value(json!("queued")).unwrap();
    assert_eq!(queued, TurnDeliveryState::Queued);
    assert_eq!(serde_json::to_value(queued).unwrap(), json!("queued"));
    for (value, mode) in [
        ("start", TurnDeliveryMode::Start),
        ("steer", TurnDeliveryMode::Steer),
    ] {
        assert_eq!(
            serde_json::from_value::<TurnDeliveryMode>(json!(value)).unwrap(),
            mode
        );
        assert_eq!(serde_json::to_value(mode).unwrap(), json!(value));
        assert_eq!(mode.as_str(), value);
    }
}

#[test]
fn all_contract_command_variants_round_trip_with_canonical_defaults() {
    let values = command_values();
    assert_eq!(values.len(), 27);
    for value in values {
        let expected_type = value["type"].clone();
        let command = decode(value.clone());
        assert_eq!(command.command_type(), expected_type.as_str().unwrap());
        let encoded = serde_json::to_value(command).expect("command encodes");
        assert_eq!(encoded["type"], expected_type);
        if value["queued"] == true
            || matches!(
                expected_type.as_str(),
                Some("thread.turn.steer" | "thread.turn.promote" | "thread.turn-delivery.resolve")
            )
        {
            assert_eq!(encoded, value);
        }
    }

    let thread_create = serde_json::to_value(decode(command_values()[3].clone())).unwrap();
    assert_eq!(thread_create["interactionMode"], "default");
    assert!(thread_create.get("kind").is_none());
    let turn_start = serde_json::to_value(decode(command_values()[10].clone())).unwrap();
    assert!(turn_start.get("queued").is_none());
    assert_eq!(turn_start["runtimeMode"], "full-access");
    assert_eq!(turn_start["interactionMode"], "default");
    let session_set = serde_json::to_value(decode(command_values()[16].clone())).unwrap();
    assert_eq!(session_set["session"]["runtimeMode"], "full-access");
    let plan = serde_json::to_value(decode(command_values()[19].clone())).unwrap();
    assert_eq!(plan["proposedPlan"]["implementedAt"], Value::Null);
    assert_eq!(plan["proposedPlan"]["implementationThreadId"], Value::Null);

    let project_create = serde_json::to_value(decode(command_values()[0].clone())).unwrap();
    assert_eq!(project_create["defaultModelSelection"], Value::Null);
    let missing_project_selection = serde_json::to_value(decode(json!({
        "type":"project.meta.update","commandId":"missing","projectId":"p1"
    })))
    .unwrap();
    assert!(
        missing_project_selection
            .get("defaultModelSelection")
            .is_none()
    );
    let null_project_selection = serde_json::to_value(decode(json!({
        "type":"project.meta.update","commandId":"null","projectId":"p1","defaultModelSelection":null
    }))).unwrap();
    assert_eq!(null_project_selection["defaultModelSelection"], Value::Null);
    let null_thread_paths = serde_json::to_value(decode(command_values()[7].clone())).unwrap();
    assert_eq!(null_thread_paths["worktreePath"], Value::Null);
    assert_eq!(null_thread_paths["branch"], "main");
}

#[tokio::test]
async fn all_contract_commands_persist_canonical_events_and_project_atomically() {
    let repositories = migrated_repositories().await;
    let engine =
        OrchestrationEngine::start(repositories.database().clone(), EngineOptions::default())
            .await
            .expect("engine starts");
    let values = command_values();
    let order = [
        0, 1, 3, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 5, 6, 4, 2,
    ];
    for index in order {
        if index == 22 {
            let before_revert = load_snapshot(&engine.repositories()).await.unwrap();
            assert_eq!(
                before_revert
                    .messages
                    .iter()
                    .find(|message| message.message_id == "m-assistant")
                    .unwrap()
                    .text,
                "hel"
            );
            let turn = before_revert
                .turns
                .iter()
                .find(|turn| turn.turn_id.as_deref() == Some("turn-1"))
                .unwrap();
            assert_eq!(turn.state, "running");
            assert_eq!(turn.assistant_message_id.as_deref(), Some("m-assistant"));
        }
        engine
            .dispatch(decode(values[index].clone()))
            .await
            .expect("command succeeds");
    }

    let events = engine.read_events(0).await.expect("events");
    let event_types: Vec<_> = events
        .iter()
        .map(|event| event.event.event_type.as_str())
        .collect();
    assert_eq!(
        event_types,
        vec![
            "project.created",
            "thread.created",
            "project.meta-updated",
            "thread.created",
            "thread.meta-updated",
            "thread.runtime-mode-set",
            "thread.interaction-mode-set",
            "thread.message-sent",
            "thread.turn-start-requested",
            "thread.turn-interrupt-requested",
            "thread.approval-response-requested",
            "thread.user-input-response-requested",
            "thread.checkpoint-revert-requested",
            "thread.session-stop-requested",
            "thread.session-set",
            "thread.message-sent",
            "thread.message-sent",
            "thread.proposed-plan-upserted",
            "thread.turn-diff-completed",
            "thread.activity-appended",
            "thread.reverted",
            "thread.archived",
            "thread.unarchived",
            "thread.deleted",
            "thread.deleted",
            "project.deleted",
        ]
    );
    assert_eq!(events[7].event.payload["attachments"], json!([]));
    assert_eq!(
        events[14].event.payload["session"]["runtimeMode"],
        "full-access"
    );
    assert_eq!(events[19].event.payload["activity"]["id"], "activity-1");
    assert_eq!(events[19].event.metadata["requestId"], "r3");
    assert_eq!(
        events[0].event.payload["worktreeDiscovery"],
        json!({"visibility":"hidden","initialPromptDismissedAt":null,"baselinePaths":[]})
    );
    assert_eq!(
        events[2].event.payload["worktreeDiscovery"],
        json!({"visibility":"shown","initialPromptDismissedAt":"2026-07-10T10:00:00.000Z","baselinePaths":["C:/repo-worktrees"]})
    );

    let snapshot = load_snapshot(&engine.repositories())
        .await
        .expect("snapshot");
    assert_eq!(snapshot.projects[0].title, "Renamed");
    assert_eq!(
        snapshot.projects[0].worktree_discovery,
        json!({"visibility":"shown","initialPromptDismissedAt":"2026-07-10T10:00:00.000Z","baselinePaths":["C:/repo-worktrees"]})
    );
    assert!(snapshot.projects[0].deleted_at.is_some());
    let thread = snapshot
        .threads
        .iter()
        .find(|thread| thread.thread_id == "t1")
        .unwrap();
    assert_eq!(thread.title, "Thread 2");
    assert_eq!(thread.runtime_mode, "approval-required");
    assert_eq!(thread.interaction_mode, "plan");
    assert!(thread.deleted_at.is_some());
    assert!(
        snapshot
            .messages
            .iter()
            .all(|message| message.message_id != "m-assistant")
    );
    assert!(
        snapshot.checkpoints.is_empty(),
        "revert to zero removes checkpoint projections"
    );
    assert!(
        snapshot.activities.is_empty(),
        "revert removes turn-bound activity projections"
    );
    assert!(
        snapshot.proposed_plans.is_empty(),
        "revert removes turn-bound plan projections"
    );

    let duplicate = engine
        .dispatch(decode(values[17].clone()))
        .await
        .expect("idempotent retry");
    assert_eq!(duplicate.sequence, 16);
    engine.shutdown().await;

    repositories
        .database()
        .call(|connection| {
            connection.execute("DELETE FROM projection_projects", [])?;
            connection.execute("DELETE FROM projection_thread_messages", [])?;
            connection.execute(
                "UPDATE projection_state SET last_applied_sequence = 0 WHERE projector IN ('projection.projects', 'projection.thread-messages')",
                [],
            )?;
            Ok(())
        })
        .await
        .expect("rewind selected projectors");

    let restarted =
        OrchestrationEngine::start(repositories.database().clone(), EngineOptions::default())
            .await
            .expect("restart");
    assert_eq!(restarted.read_events(0).await.unwrap().len(), 26);
    let replayed = load_snapshot(&restarted.repositories()).await.unwrap();
    assert_eq!(replayed.projects[0].title, "Renamed");
    assert_eq!(
        replayed.projects[0].worktree_discovery,
        json!({"visibility":"shown","initialPromptDismissedAt":"2026-07-10T10:00:00.000Z","baselinePaths":["C:/repo-worktrees"]})
    );
    assert!(replayed.projects[0].deleted_at.is_some());
    assert_eq!(replayed.messages.len(), 1);
    assert_eq!(replayed.messages[0].message_id, "m-user");
    restarted.shutdown().await;
}

#[tokio::test]
async fn project_worktree_discovery_policy_survives_metadata_updates_and_replay() {
    let repositories = migrated_repositories().await;
    let engine =
        OrchestrationEngine::start(repositories.database().clone(), EngineOptions::default())
            .await
            .expect("engine starts");
    let policy = json!({
        "visibility": "shown",
        "initialPromptDismissedAt": "2026-07-10T10:00:00.000Z",
        "baselinePaths": ["C:/policy-worktree"]
    });
    let expected_policy = json!({
        "visibility": "shown",
        "initialPromptDismissedAt": "2026-07-10T10:00:00.000Z",
        "baselinePaths": ["C:/policy-worktree"]
    });
    engine
        .dispatch(decode(json!({
            "type": "project.create",
            "commandId": "policy-create",
            "projectId": "policy-project",
            "title": "Policy project",
            "workspaceRoot": "C:/policy-project",
            "defaultModelSelection": null,
            "createdAt": CREATED_AT,
        })))
        .await
        .expect("project creates");
    engine
        .dispatch(decode(json!({
            "type": "project.meta.update",
            "commandId": "policy-update",
            "projectId": "policy-project",
            "worktreeDiscovery": policy,
        })))
        .await
        .expect("policy update persists");
    engine
        .dispatch(decode(json!({
            "type": "project.meta.update",
            "commandId": "policy-title-update",
            "projectId": "policy-project",
            "title": "Renamed policy project",
        })))
        .await
        .expect("unrelated metadata update persists");

    let events = engine.read_events(0).await.expect("events read");
    assert_eq!(
        events[0].event.payload["worktreeDiscovery"],
        json!({"visibility":"hidden","initialPromptDismissedAt":null,"baselinePaths":[]})
    );
    assert_eq!(
        events[2].event.payload["worktreeDiscovery"],
        expected_policy
    );
    assert!(events[3].event.payload.get("worktreeDiscovery").is_none());
    assert_eq!(
        load_snapshot(&engine.repositories())
            .await
            .expect("snapshot")
            .projects[0]
            .worktree_discovery,
        expected_policy
    );
    engine.shutdown().await;

    repositories
        .database()
        .call(|connection| {
            connection.execute("DELETE FROM projection_projects", [])?;
            connection.execute(
                "UPDATE projection_state SET last_applied_sequence = 0 WHERE projector = 'projection.projects'",
                [],
            )?;
            Ok(())
        })
        .await
        .expect("rewind project projection");
    let restarted =
        OrchestrationEngine::start(repositories.database().clone(), EngineOptions::default())
            .await
            .expect("engine restarts");
    assert_eq!(
        load_snapshot(&restarted.repositories())
            .await
            .expect("replayed snapshot")
            .projects[0]
            .worktree_discovery,
        expected_policy
    );
    restarted.shutdown().await;
}

#[tokio::test]
async fn project_repository_identity_pin_survives_projection_rewind_and_replay() {
    let repositories = migrated_repositories().await;
    let engine =
        OrchestrationEngine::start(repositories.database().clone(), EngineOptions::default())
            .await
            .expect("engine starts");
    engine
        .dispatch(decode(json!({
            "type": "project.create",
            "commandId": "pin-replay-create",
            "projectId": "pin-replay-project",
            "title": "Pinned replay project",
            "workspaceRoot": "C:/pin-replay-project",
            "defaultModelSelection": null,
            "createdAt": CREATED_AT,
        })))
        .await
        .expect("project creates");
    assert_eq!(
        engine
            .repositories()
            .pin_project_worktree_repository_key(
                "pin-replay-project".to_owned(),
                "repository-key-durable".to_owned(),
            )
            .await
            .expect("pin persists"),
        Some(coddy_studio_server::persistence::WorktreeRepositoryPinOutcome::Established)
    );
    engine.shutdown().await;

    repositories
        .database()
        .call(|connection| {
            connection.execute("DELETE FROM projection_projects", [])?;
            connection.execute(
                "UPDATE projection_state SET last_applied_sequence = 0 WHERE projector = 'projection.projects'",
                [],
            )?;
            Ok(())
        })
        .await
        .expect("rewind project projection");
    let restarted =
        OrchestrationEngine::start(repositories.database().clone(), EngineOptions::default())
            .await
            .expect("engine restarts");
    let project = restarted
        .repositories()
        .get_project("pin-replay-project".to_owned())
        .await
        .expect("read replayed project")
        .expect("project replayed");

    assert_eq!(
        project.worktree_repository_key.as_deref(),
        Some("repository-key-durable")
    );
    restarted.shutdown().await;
}

#[tokio::test]
async fn omitted_worktree_discovery_metadata_update_preserves_custom_policy_through_replay() {
    let repositories = migrated_repositories().await;
    let engine =
        OrchestrationEngine::start(repositories.database().clone(), EngineOptions::default())
            .await
            .expect("engine starts");
    let custom_policy = json!({
        "visibility": "shown",
        "initialPromptDismissedAt": "2026-08-09T00:00:00.000Z",
        "baselinePaths": ["C:/custom-policy-worktree"]
    });
    let expected_custom_policy = json!({
        "visibility": "shown",
        "initialPromptDismissedAt": "2026-08-09T00:00:00.000Z",
        "baselinePaths": ["C:/custom-policy-worktree"]
    });
    engine
        .dispatch(decode(json!({
            "type": "project.create",
            "commandId": "custom-policy-create",
            "projectId": "custom-policy-project",
            "title": "Custom policy project",
            "workspaceRoot": "C:/custom-policy-project",
            "defaultModelSelection": null,
            "createdAt": CREATED_AT,
        })))
        .await
        .expect("project creates");
    engine
        .dispatch(decode(json!({
            "type": "project.meta.update",
            "commandId": "custom-policy-update",
            "projectId": "custom-policy-project",
            "worktreeDiscovery": custom_policy,
        })))
        .await
        .expect("custom policy persists");

    let omitted_policy_update = decode(json!({
        "type": "project.meta.update",
        "commandId": "omitted-policy-update",
        "projectId": "custom-policy-project",
        "title": "Renamed custom policy project",
    }));
    engine
        .dispatch(omitted_policy_update)
        .await
        .expect("omitted policy update persists");
    assert_eq!(
        load_snapshot(&engine.repositories())
            .await
            .expect("project projection")
            .projects[0]
            .worktree_discovery,
        expected_custom_policy
    );
    engine.shutdown().await;

    repositories
        .database()
        .call(|connection| {
            connection.execute("DELETE FROM projection_projects", [])?;
            connection.execute(
                "UPDATE projection_state SET last_applied_sequence = 0 WHERE projector = 'projection.projects'",
                [],
            )?;
            Ok(())
        })
        .await
        .expect("rewind project projection");
    let restarted =
        OrchestrationEngine::start(repositories.database().clone(), EngineOptions::default())
            .await
            .expect("engine restarts");
    assert_eq!(
        load_snapshot(&restarted.repositories())
            .await
            .expect("replayed project projection")
            .projects[0]
            .worktree_discovery,
        expected_custom_policy
    );
    restarted.shutdown().await;
}

#[tokio::test]
async fn default_thread_cannot_be_archived_or_deleted_directly() {
    let repositories = migrated_repositories().await;
    let engine =
        OrchestrationEngine::start(repositories.database().clone(), EngineOptions::default())
            .await
            .expect("engine starts");
    engine
        .dispatch(decode(json!({
            "type": "project.create",
            "commandId": "create-project",
            "projectId": "project-default-guard",
            "title": "Guarded Project",
            "workspaceRoot": "C:/guarded",
            "defaultModelSelection": null,
            "createdAt": CREATED_AT,
        })))
        .await
        .expect("project created");

    let snapshot = load_snapshot(&engine.repositories())
        .await
        .expect("snapshot");
    let default_id = snapshot
        .threads
        .iter()
        .find(|thread| thread.project_id == "project-default-guard" && thread.kind == "default")
        .expect("default thread")
        .thread_id
        .clone();

    let duplicate_error = engine
        .dispatch(decode(json!({
            "type": "thread.create",
            "commandId": "replace-default",
            "threadId": "replacement-default",
            "projectId": "project-default-guard",
            "title": "Replacement",
            "kind": "default",
            "modelSelection": {"instanceId": "codex", "model": "gpt-5"},
            "runtimeMode": "full-access",
            "interactionMode": "default",
            "branch": null,
            "worktreePath": null,
            "createdAt": CREATED_AT,
        })))
        .await
        .expect_err("second default thread is rejected");
    assert!(duplicate_error.to_string().contains("default thread"));

    for (command_id, command_type) in [
        ("archive-default", "thread.archive"),
        ("delete-default", "thread.delete"),
    ] {
        let error = engine
            .dispatch(decode(json!({
                "type": command_type,
                "commandId": command_id,
                "threadId": default_id,
            })))
            .await
            .expect_err("default thread mutation is rejected");
        assert!(error.to_string().contains("Default thread"));
    }

    let snapshot = load_snapshot(&engine.repositories())
        .await
        .expect("snapshot");
    let default_thread = snapshot
        .threads
        .iter()
        .find(|thread| thread.thread_id == default_id)
        .expect("default thread remains");
    assert!(default_thread.archived_at.is_none());
    assert!(default_thread.deleted_at.is_none());
    assert_eq!(
        snapshot
            .threads
            .iter()
            .filter(|thread| {
                thread.project_id == "project-default-guard"
                    && thread.kind == "default"
                    && thread.deleted_at.is_none()
            })
            .count(),
        1
    );
    engine.shutdown().await;
}

#[tokio::test]
async fn bootstrap_turn_creates_thread_before_dispatching_final_turn() {
    let repositories = migrated_repositories().await;
    let engine =
        OrchestrationEngine::start(repositories.database().clone(), EngineOptions::default())
            .await
            .expect("engine starts");
    engine
        .dispatch(decode(command_values()[0].clone()))
        .await
        .expect("project is created");

    engine
        .dispatch(decode(json!({
            "type":"thread.turn.start",
            "commandId":"bootstrap-turn",
            "threadId":"bootstrap-thread",
            "message":{"messageId":"bootstrap-message","role":"user","text":"build it","attachments":[]},
            "bootstrap":{"createThread":{
                "projectId":"p1",
                "title":"Bootstrap thread",
                "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access",
                "interactionMode":"default",
                "branch":null,
                "worktreePath":null,
                "createdAt":CREATED_AT
            }},
            "createdAt":CREATED_AT
        })))
        .await
        .expect("bootstrap turn succeeds");

    let thread = engine
        .repositories()
        .get_thread("bootstrap-thread".to_owned())
        .await
        .expect("thread query")
        .expect("thread was created");
    assert_eq!(thread.title, "Bootstrap thread");

    let events = engine.read_events(0).await.expect("events");
    let bootstrap_events: Vec<_> = events
        .iter()
        .filter(|event| event.event.aggregate_id == "bootstrap-thread")
        .map(|event| event.event.event_type.as_str())
        .collect();
    assert_eq!(
        bootstrap_events,
        vec![
            "thread.created",
            "thread.message-sent",
            "thread.turn-start-requested"
        ]
    );

    engine.shutdown().await;
}

#[tokio::test]
async fn projector_failure_rolls_back_event_projection_and_receipt() {
    let repositories = migrated_repositories().await;
    let hooks = TestHooks::default();
    let engine = OrchestrationEngine::start(
        repositories.database().clone(),
        EngineOptions {
            queue_capacity: 1,
            test_hooks: hooks.clone(),
        },
    )
    .await
    .expect("engine starts");
    let values = command_values();
    for index in [0, 3] {
        engine
            .dispatch(decode(values[index].clone()))
            .await
            .expect("setup");
    }
    hooks.fail_next_projector(
        "projection.thread-activities",
        Some("thread.activity-appended"),
    );
    let failed = engine
        .dispatch(decode(values[21].clone()))
        .await
        .expect_err("injected failure");
    assert!(matches!(
        failed,
        OrchestrationError::InjectedProjectorFailure { .. }
    ));
    assert_eq!(engine.read_events(0).await.unwrap().len(), 3);
    assert!(
        repositories
            .get_command_receipt("c18".to_owned())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        load_snapshot(&repositories)
            .await
            .unwrap()
            .activities
            .is_empty()
    );
    engine
        .dispatch(decode(values[21].clone()))
        .await
        .expect("retry succeeds");
    assert_eq!(engine.read_events(0).await.unwrap().len(), 4);
    engine.shutdown().await;
}

#[tokio::test]
async fn assistant_completion_updates_only_matching_streaming_assistant_rows() {
    let repositories = migrated_repositories().await;
    let engine =
        OrchestrationEngine::start(repositories.database().clone(), EngineOptions::default())
            .await
            .expect("engine starts");
    for command in [
        json!({"type":"project.create","commandId":"completion-project","projectId":"p1","title":"Project","workspaceRoot":"C:/repo","createdAt":CREATED_AT}),
        json!({"type":"thread.create","commandId":"completion-thread-1","threadId":"t1","projectId":"p1","title":"Thread 1","modelSelection":{"instanceId":"codex","model":"gpt-5"},"runtimeMode":"full-access","createdAt":CREATED_AT}),
        json!({"type":"thread.create","commandId":"completion-thread-2","threadId":"t2","projectId":"p1","title":"Thread 2","modelSelection":{"instanceId":"codex","model":"gpt-5"},"runtimeMode":"full-access","createdAt":CREATED_AT}),
        json!({"type":"thread.message.assistant.delta","commandId":"removed-delta","threadId":"t1","messageId":"assistant-removed","delta":"removed","turnId":"turn-1","createdAt":CREATED_AT}),
    ] {
        engine
            .dispatch(decode(command))
            .await
            .expect("setup command");
    }
    repositories
        .delete_messages_by_thread("t1".to_owned())
        .await
        .expect("simulate checkpoint message removal");
    engine
        .dispatch(decode(json!({
            "type":"thread.message.assistant.complete",
            "commandId":"removed-complete",
            "threadId":"t1",
            "messageId":"assistant-removed",
            "turnId":"turn-1",
            "createdAt":CREATED_AT
        })))
        .await
        .expect("completion of a removed row is an accepted no-op");
    for command in [
        json!({"type":"thread.message.assistant.delta","commandId":"wrong-turn-delta","threadId":"t1","messageId":"assistant-wrong-turn","delta":"wrong turn","turnId":"turn-other","createdAt":CREATED_AT}),
        json!({"type":"thread.message.assistant.delta","commandId":"wrong-thread-delta","threadId":"t2","messageId":"assistant-wrong-thread","delta":"wrong thread","turnId":"turn-2","createdAt":CREATED_AT}),
        json!({"type":"thread.turn.start","commandId":"user-start","threadId":"t1","message":{"messageId":"user-message","role":"user","text":"keep user","attachments":[]},"createdAt":CREATED_AT}),
        json!({"type":"thread.message.assistant.delta","commandId":"settled-delta","threadId":"t1","messageId":"assistant-settled","delta":"settled","turnId":"turn-1","createdAt":CREATED_AT}),
        json!({"type":"thread.message.assistant.complete","commandId":"settled-complete","threadId":"t1","messageId":"assistant-settled","turnId":"turn-1","createdAt":CREATED_AT}),
    ] {
        engine
            .dispatch(decode(command))
            .await
            .expect("setup command");
    }
    for command in [
        json!({"type":"thread.message.assistant.complete","commandId":"wrong-turn-complete","threadId":"t1","messageId":"assistant-wrong-turn","turnId":"turn-1","createdAt":"2026-07-10T10:01:00.000Z"}),
        json!({"type":"thread.message.assistant.complete","commandId":"wrong-thread-complete","threadId":"t1","messageId":"assistant-wrong-thread","turnId":"turn-2","createdAt":"2026-07-10T10:01:00.000Z"}),
        json!({"type":"thread.message.assistant.complete","commandId":"wrong-role-complete","threadId":"t1","messageId":"user-message","turnId":null,"createdAt":"2026-07-10T10:01:00.000Z"}),
        json!({"type":"thread.message.assistant.complete","commandId":"already-settled-complete","threadId":"t1","messageId":"assistant-settled","turnId":"turn-1","createdAt":"2026-07-10T10:01:00.000Z"}),
    ] {
        engine
            .dispatch(decode(command))
            .await
            .expect("mismatched completion is an accepted no-op");
    }

    let wrong_turn = repositories
        .get_message("assistant-wrong-turn".to_owned())
        .await
        .unwrap()
        .unwrap();
    let wrong_thread = repositories
        .get_message("assistant-wrong-thread".to_owned())
        .await
        .unwrap()
        .unwrap();
    let user = repositories
        .get_message("user-message".to_owned())
        .await
        .unwrap()
        .unwrap();
    let settled = repositories
        .get_message("assistant-settled".to_owned())
        .await
        .unwrap()
        .unwrap();
    let removed_missing = repositories
        .get_message("assistant-removed".to_owned())
        .await
        .expect("removed message query")
        .is_none();
    assert_eq!(
        (
            removed_missing,
            (
                wrong_turn.turn_id.as_deref(),
                wrong_turn.text.as_str(),
                wrong_turn.is_streaming,
            ),
            (
                wrong_thread.thread_id.as_str(),
                wrong_thread.text.as_str(),
                wrong_thread.is_streaming,
            ),
            (user.role.as_str(), user.text.as_str()),
            (
                settled.text.as_str(),
                settled.is_streaming,
                settled.updated_at.as_str(),
            ),
        ),
        (
            true,
            (Some("turn-other"), "wrong turn", true),
            ("t2", "wrong thread", true),
            ("user", "keep user"),
            ("settled", false, CREATED_AT),
        )
    );

    engine.shutdown().await;
}

#[tokio::test]
async fn zero_row_completion_does_not_rewind_the_turn_assistant_message() {
    let repositories = migrated_repositories().await;
    let engine =
        OrchestrationEngine::start(repositories.database().clone(), EngineOptions::default())
            .await
            .expect("engine starts");
    for command in [
        json!({"type":"project.create","commandId":"rewind-project","projectId":"p1","title":"Project","workspaceRoot":"C:/repo","createdAt":CREATED_AT}),
        json!({"type":"thread.create","commandId":"rewind-thread","threadId":"t1","projectId":"p1","title":"Thread","modelSelection":{"instanceId":"codex","model":"gpt-5"},"runtimeMode":"full-access","createdAt":CREATED_AT}),
        json!({"type":"thread.message.assistant.delta","commandId":"rewind-a-delta","threadId":"t1","messageId":"assistant-a","delta":"A","turnId":"turn-1","createdAt":CREATED_AT}),
        json!({"type":"thread.message.assistant.complete","commandId":"rewind-a-complete","threadId":"t1","messageId":"assistant-a","turnId":"turn-1","createdAt":CREATED_AT}),
        json!({"type":"thread.message.assistant.delta","commandId":"rewind-b-delta","threadId":"t1","messageId":"assistant-b","delta":"B","turnId":"turn-1","createdAt":CREATED_AT}),
        json!({"type":"thread.message.assistant.complete","commandId":"rewind-a-duplicate","threadId":"t1","messageId":"assistant-a","turnId":"turn-1","createdAt":CREATED_AT}),
    ] {
        engine
            .dispatch(decode(command))
            .await
            .expect("command dispatches");
    }

    let turns = repositories
        .list_turns_by_thread("t1".to_owned())
        .await
        .expect("turns query");
    let messages = repositories
        .list_messages_by_thread("t1".to_owned())
        .await
        .expect("messages query");
    assert_eq!(turns.len(), 1);
    assert_eq!(
        (
            turns[0].assistant_message_id.as_deref(),
            messages
                .iter()
                .find(|message| message.message_id == "assistant-a")
                .expect("assistant A")
                .is_streaming,
            messages
                .iter()
                .find(|message| message.message_id == "assistant-b")
                .expect("assistant B")
                .is_streaming,
        ),
        (Some("assistant-b"), false, true)
    );

    engine.shutdown().await;
}

#[tokio::test]
async fn assistant_pointer_uses_message_id_to_break_equal_timestamp_ties() {
    let repositories = migrated_repositories().await;
    let engine =
        OrchestrationEngine::start(repositories.database().clone(), EngineOptions::default())
            .await
            .expect("engine starts");
    for command in [
        json!({"type":"project.create","commandId":"tie-project","projectId":"p1","title":"Project","workspaceRoot":"C:/repo","createdAt":CREATED_AT}),
        json!({"type":"thread.create","commandId":"tie-thread","threadId":"t1","projectId":"p1","title":"Thread","modelSelection":{"instanceId":"codex","model":"gpt-5"},"runtimeMode":"full-access","createdAt":CREATED_AT}),
        json!({"type":"thread.message.assistant.delta","commandId":"tie-a-delta","threadId":"t1","messageId":"assistant-a","delta":"A","turnId":"turn-1","createdAt":CREATED_AT}),
        json!({"type":"thread.message.assistant.delta","commandId":"tie-z-delta","threadId":"t1","messageId":"assistant-z","delta":"Z","turnId":"turn-1","createdAt":CREATED_AT}),
        json!({"type":"thread.message.assistant.complete","commandId":"tie-z-complete","threadId":"t1","messageId":"assistant-z","turnId":"turn-1","createdAt":CREATED_AT}),
        json!({"type":"thread.message.assistant.complete","commandId":"tie-a-late-complete","threadId":"t1","messageId":"assistant-a","turnId":"turn-1","createdAt":CREATED_AT}),
    ] {
        engine
            .dispatch(decode(command))
            .await
            .expect("command dispatches");
    }

    let turns = repositories
        .list_turns_by_thread("t1".to_owned())
        .await
        .expect("turns query");
    assert_eq!(turns.len(), 1);
    assert_eq!(
        turns[0].assistant_message_id.as_deref(),
        Some("assistant-z")
    );

    engine.shutdown().await;
}

#[tokio::test]
async fn live_noop_assistant_completion_is_not_replayed_after_projector_rewind() {
    let repositories = migrated_repositories().await;
    let engine =
        OrchestrationEngine::start(repositories.database().clone(), EngineOptions::default())
            .await
            .expect("engine starts");
    for command in [
        json!({"type":"project.create","commandId":"noop-replay-project","projectId":"p1","title":"Project","workspaceRoot":"C:/repo","createdAt":CREATED_AT}),
        json!({"type":"thread.create","commandId":"noop-replay-thread","threadId":"t1","projectId":"p1","title":"Thread","modelSelection":{"instanceId":"codex","model":"gpt-5"},"runtimeMode":"full-access","createdAt":CREATED_AT}),
        json!({"type":"thread.message.assistant.delta","commandId":"noop-replay-delta","threadId":"t1","messageId":"assistant-removed","delta":"removed","turnId":"turn-1","createdAt":CREATED_AT}),
    ] {
        engine
            .dispatch(decode(command))
            .await
            .expect("setup command");
    }
    repositories
        .delete_messages_by_thread("t1".to_owned())
        .await
        .expect("simulate checkpoint message removal");
    let sequence_before_completion = engine
        .read_events(0)
        .await
        .expect("durable setup event read")
        .last()
        .expect("setup event")
        .sequence;
    engine
        .dispatch(decode(json!({
            "type":"thread.message.assistant.complete",
            "commandId":"noop-replay-complete",
            "threadId":"t1",
            "messageId":"assistant-removed",
            "turnId":"turn-1",
            "createdAt":"2026-07-10T10:01:00.000Z"
        })))
        .await
        .expect("completion of a removed row is an accepted no-op");
    engine.shutdown().await;

    repositories
        .database()
        .call(move |connection| {
            connection.execute(
                "UPDATE projection_state SET last_applied_sequence = ? WHERE projector = 'projection.thread-messages'",
                [sequence_before_completion],
            )?;
            Ok(())
        })
        .await
        .expect("rewind the message projector to before the no-op completion");

    let restarted =
        OrchestrationEngine::start(repositories.database().clone(), EngineOptions::default())
            .await
            .expect("projector restart");
    let message = repositories
        .get_message("assistant-removed".to_owned())
        .await
        .expect("replayed message query");
    let completion_events = restarted
        .read_events(0)
        .await
        .expect("durable event read")
        .into_iter()
        .filter(|event| event.event.command_id.as_deref() == Some("noop-replay-complete"))
        .count();
    assert_eq!((message.is_none(), completion_events), (true, 0));

    restarted.shutdown().await;
}

#[tokio::test]
async fn bootstrap_preserves_legacy_unmarked_message_sent_projection() {
    let repositories = migrated_repositories().await;
    let engine =
        OrchestrationEngine::start(repositories.database().clone(), EngineOptions::default())
            .await
            .expect("engine starts");
    for command in [
        json!({"type":"project.create","commandId":"legacy-project","projectId":"p1","title":"Project","workspaceRoot":"C:/repo","createdAt":CREATED_AT}),
        json!({"type":"thread.create","commandId":"legacy-thread","threadId":"t1","projectId":"p1","title":"Thread","modelSelection":{"instanceId":"codex","model":"gpt-5"},"runtimeMode":"full-access","createdAt":CREATED_AT}),
    ] {
        engine
            .dispatch(decode(command))
            .await
            .expect("setup command");
    }
    engine.shutdown().await;

    repositories
        .append_event(NewOrchestrationEvent {
            event_id: "legacy-unmarked-message-event".to_owned(),
            event_type: "thread.message-sent".to_owned(),
            aggregate_kind: "thread".to_owned(),
            aggregate_id: "t1".to_owned(),
            occurred_at: CREATED_AT.to_owned(),
            command_id: Some("legacy-unmarked-message-command".to_owned()),
            causation_event_id: None,
            correlation_id: Some("legacy-unmarked-message-command".to_owned()),
            payload: json!({
                "threadId": "t1",
                "messageId": "legacy-assistant",
                "role": "assistant",
                "text": "legacy text",
                "turnId": "turn-legacy",
                "streaming": false,
                "createdAt": CREATED_AT,
                "updatedAt": CREATED_AT,
            }),
            metadata: json!({}),
        })
        .await
        .expect("legacy event append");

    let restarted =
        OrchestrationEngine::start(repositories.database().clone(), EngineOptions::default())
            .await
            .expect("legacy projector restart");
    let message = repositories
        .get_message("legacy-assistant".to_owned())
        .await
        .expect("legacy message query")
        .expect("legacy message projected");
    assert_eq!(
        (
            message.thread_id.as_str(),
            message.turn_id.as_deref(),
            message.role.as_str(),
            message.text.as_str(),
            message.is_streaming,
        ),
        ("t1", Some("turn-legacy"), "assistant", "legacy text", false,)
    );
    restarted.shutdown().await;
}

#[path = "support/turn_queue.rs"]
mod turn_queue;
use turn_queue::{queue_engine, seed_queued_message};

#[tokio::test]
async fn promote_replays_the_stored_turn_start() {
    let engine = queue_engine(TestHooks::default()).await;
    seed_queued_message(&engine, "saved-queue").await;
    engine
        .dispatch(decode(command_values()[8].clone()))
        .await
        .unwrap();
    engine
        .dispatch(decode(command_values()[9].clone()))
        .await
        .unwrap();
    let before = engine
        .read_events(0)
        .await
        .unwrap()
        .last()
        .unwrap()
        .sequence;
    let promote = json!({"type":"thread.turn.promote", "commandId":"promote-first", "threadId":"t1", "messageId":"saved-queue", "createdAt":"2026-07-10T10:01:00.000Z"});
    let result = engine
        .dispatch(decode(promote.clone()))
        .await
        .expect("queued row promotes");
    let events = engine.read_events(before).await.unwrap();
    assert_eq!(
        events
            .iter()
            .map(|event| event.event.event_type.as_str())
            .collect::<Vec<_>>(),
        [
            "thread.turn-start-requested",
            "thread.turn-delivery-updated"
        ]
    );
    assert_eq!(
        events[0].event.payload,
        json!({
            "threadId":"t1", "messageId":"saved-queue", "modelSelection":{"instanceId":"codex", "model":"saved-model", "options":{"reasoningEffort":"high"}},
            "runtimeMode":"full-access", "interactionMode":"default", "titleSeed":"Saved title", "createdAt":"2026-07-10T10:01:00.000Z"
        })
    );
    assert_eq!(
        events[1].event.payload["delivery"],
        json!({"state":"pending", "provider":"codex", "providerInstanceId":"codex", "mode":"start", "held":false})
    );
    let row = engine
        .repositories()
        .get_provider_turn_delivery("saved-queue".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, TurnDeliveryState::Pending);
    assert!(!row.held);
    assert_eq!(
        engine
            .dispatch(decode(promote.clone()))
            .await
            .unwrap()
            .sequence,
        result.sequence
    );
    let mut second = promote;
    second["commandId"] = json!("promote-second");
    assert!(matches!(
        engine.dispatch(decode(second)).await,
        Err(OrchestrationError::Invariant { .. })
    ));
    assert_eq!(engine.read_events(before).await.unwrap().len(), 2);
    engine.shutdown().await;
}

#[tokio::test]
async fn cancel_withdraws_a_queued_message() {
    let engine = queue_engine(TestHooks::default()).await;
    seed_queued_message(&engine, "cancel-queue").await;
    let before = engine
        .read_events(0)
        .await
        .unwrap()
        .last()
        .unwrap()
        .sequence;
    let cancel = json!({"type":"thread.turn-delivery.resolve", "commandId":"cancel-first", "threadId":"t1", "messageId":"cancel-queue", "action":"cancel", "createdAt":CREATED_AT});
    engine
        .dispatch(decode(cancel.clone()))
        .await
        .expect("queued cancellation");
    let row = engine
        .repositories()
        .get_provider_turn_delivery("cancel-queue".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, TurnDeliveryState::Dismissed);
    let events = engine.read_events(before).await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event.event_type, "thread.turn-delivery-updated");
    assert_eq!(events[0].event.payload["withdrawn"], true);
    assert_eq!(
        events[0].event.payload["delivery"],
        json!({"state":"dismissed", "provider":"codex", "providerInstanceId":"codex", "mode":"start", "held":true})
    );
    assert!(
        !load_snapshot(&engine.repositories())
            .await
            .unwrap()
            .messages
            .iter()
            .any(|message| message.message_id == "cancel-queue")
    );
    engine
        .dispatch(decode(cancel))
        .await
        .expect("same cancellation is idempotent");
    assert_eq!(engine.read_events(before).await.unwrap().len(), 1);
    seed_queued_message(&engine, "cancel-pending").await;
    engine.repositories().database().call(|connection| {
        connection.execute("UPDATE provider_turn_outbox SET state = 'pending' WHERE command_id = 'cancel-pending'", [])?;
        Ok(())
    }).await.unwrap();
    assert!(matches!(engine.dispatch(decode(json!({"type":"thread.turn-delivery.resolve", "commandId":"cancel-refused", "threadId":"t1", "messageId":"cancel-pending", "action":"cancel", "createdAt":CREATED_AT}))).await,
        Err(OrchestrationError::Invariant { .. })));
    assert_eq!(
        engine
            .repositories()
            .get_provider_turn_delivery("cancel-pending".into())
            .await
            .unwrap()
            .unwrap()
            .state,
        TurnDeliveryState::Pending
    );
    assert!(
        engine
            .repositories()
            .get_message("cancel-pending".into())
            .await
            .unwrap()
            .is_some()
    );
    engine.shutdown().await;
}

async fn steer_session(engine: &OrchestrationEngine, status: &str, active: Option<&str>, id: &str) {
    engine.dispatch(serde_json::from_value(json!({
        "type":"thread.session.set", "commandId":id, "threadId":"t1", "createdAt":CREATED_AT,
        "session":{"threadId":"t1", "status":status, "providerName":"codex", "activeTurnId":active,
          "lastError":null, "updatedAt":CREATED_AT}
    })).unwrap()).await.unwrap();
}
fn steer_command(id: &str, message: &str) -> OrchestrationCommand {
    serde_json::from_value(json!({"type":"thread.turn.steer", "commandId":id, "threadId":"t1", "messageId":message, "createdAt":CREATED_AT})).unwrap()
}

#[tokio::test]
async fn steer_requires_head_running_and_capability() {
    let engine = turn_queue::queue_engine(TestHooks::default()).await;
    for id in ["head", "tail"] {
        turn_queue::seed_queued_message(&engine, id).await;
    }
    steer_session(&engine, "running", Some("active"), "running").await;
    let error = engine
        .dispatch(steer_command("non-head", "tail"))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("queued head"), "{error}");
    for (status, active) in [("ready", None), ("starting", None), ("running", None)] {
        steer_session(&engine, status, active, &format!("session-{status}")).await;
        let error = engine
            .dispatch(steer_command(&format!("steer-{status}"), "head"))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("running turn"), "{error}");
    }
    steer_session(&engine, "running", Some("active"), "running-again").await;
    engine.repositories().database().call(|connection| {
        connection.execute("UPDATE provider_turn_outbox SET provider_kind = 'cursor' WHERE command_id = 'head'", [])?;
        Ok(())
    }).await.unwrap();
    let error = engine
        .dispatch(steer_command("unsupported", "head"))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("does not support steering"),
        "{error}"
    );
    for id in ["head", "tail"] {
        assert_eq!(
            engine
                .repositories()
                .get_provider_turn_delivery(id.into())
                .await
                .unwrap()
                .unwrap()
                .state,
            TurnDeliveryState::Queued
        );
    }
    engine.shutdown().await;
}

#[tokio::test]
async fn steer_emits_turn_steer_requested_and_flips_row() {
    let engine = turn_queue::queue_engine(TestHooks::default()).await;
    turn_queue::seed_queued_message(&engine, "head").await;
    steer_session(&engine, "running", Some("active"), "running").await;
    let before = engine
        .read_events(0)
        .await
        .unwrap()
        .last()
        .unwrap()
        .sequence;
    let command = steer_command("steer", "head");
    engine.dispatch(command.clone()).await.unwrap();
    engine.dispatch(command).await.unwrap();
    let events = engine.read_events(before).await.unwrap();
    assert_eq!(events.len(), 2, "duplicate steer must not emit twice");
    assert_eq!(events[0].event.event_type, "thread.turn-steer-requested");
    assert_eq!(
        events[0].event.payload,
        json!({"threadId":"t1","messageId":"head","turnId":"active","createdAt":CREATED_AT})
    );
    assert_eq!(events[1].event.event_type, "thread.turn-delivery-updated");
    assert_eq!(
        events[1].event.payload["delivery"],
        json!({"state":"pending","provider":"codex","providerInstanceId":"codex","mode":"steer","held":false})
    );
    let row = engine
        .repositories()
        .get_provider_turn_delivery("head".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, TurnDeliveryState::Pending);
    assert_eq!(row.mode, TurnDeliveryMode::Steer);
    assert!(!row.held);
    let message = engine
        .repositories()
        .get_message("head".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(message.delivery_state.as_deref(), Some("pending"));
    assert_eq!(message.delivery_mode.as_deref(), Some("steer"));
    assert_eq!(message.turn_id, None, "bind only on provider acceptance");
    let session = engine
        .repositories()
        .get_thread_session("t1".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.status, "running");
    assert_eq!(session.active_turn_id.as_deref(), Some("active"));
    engine.shutdown().await;
}

#[tokio::test]
async fn steer_rolls_back_with_its_delivery_projection() {
    let hooks = TestHooks::default();
    let engine = turn_queue::queue_engine(hooks.clone()).await;
    turn_queue::seed_queued_message(&engine, "head").await;
    steer_session(&engine, "running", Some("active"), "running").await;
    let before = engine.read_events(0).await.unwrap().len();
    hooks.fail_next_projector(
        "projection.thread-messages",
        Some("thread.turn-delivery-updated"),
    );
    let error = engine
        .dispatch(steer_command("steer", "head"))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("projection.thread-messages"),
        "{error}"
    );
    let row = engine
        .repositories()
        .get_provider_turn_delivery("head".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, TurnDeliveryState::Queued);
    assert_eq!(row.mode, TurnDeliveryMode::Start);
    assert!(row.held);
    assert_eq!(engine.read_events(0).await.unwrap().len(), before);
    engine.shutdown().await;
}
