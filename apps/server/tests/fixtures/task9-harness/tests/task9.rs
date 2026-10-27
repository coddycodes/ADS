use serde_json::{Value, json};
use coddy_studio_task9_harness::{
    orchestration::engine::{
        EngineOptions, OrchestrationCommand, OrchestrationEngine, OrchestrationError, TestHooks,
        load_snapshot,
    },
    persistence::{Database, Repositories, run_migrations},
};

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

fn fixture_commands() -> Vec<OrchestrationCommand> {
    let fixture: Value = serde_json::from_str(include_str!("../../orchestration-task9-trace.json"))
        .expect("valid task 9 fixture");
    fixture["commands"]
        .as_array()
        .expect("command array")
        .iter()
        .cloned()
        .map(|command| serde_json::from_value(command).expect("fixture command decodes"))
        .collect()
}

#[tokio::test]
async fn trace_replays_across_all_projectors_and_restart_boundaries() {
    let repositories = migrated_repositories().await;
    let engine =
        OrchestrationEngine::start(repositories.database().clone(), EngineOptions::default())
            .await
            .expect("engine starts");
    let mut stream = engine.subscribe_events();
    let commands = fixture_commands();
    let mut pushed_types = Vec::new();

    for command in commands.iter().cloned() {
        engine.dispatch(command).await.expect("command succeeds");
    }
    for _ in 0..10 {
        pushed_types.push(
            stream
                .recv()
                .await
                .expect("published event")
                .event
                .event_type,
        );
    }

    assert_eq!(
        pushed_types,
        vec![
            "project.created",
            "thread.created",
            "thread.created",
            "thread.message.added",
            "thread.turn.started",
            "thread.activity.appended",
            "thread.session.set",
            "thread.approval.upserted",
            "thread.plan.upserted",
            "thread.checkpoint.saved",
        ]
    );

    let snapshot = load_snapshot(&engine.repositories())
        .await
        .expect("snapshot loads");
    assert_eq!(snapshot.projects.len(), 1);
    assert_eq!(snapshot.threads.len(), 2);
    assert_eq!(snapshot.messages.len(), 1);
    assert_eq!(snapshot.activities.len(), 1);
    assert_eq!(snapshot.sessions.len(), 1);
    assert_eq!(snapshot.approvals.len(), 1);
    assert_eq!(snapshot.proposed_plans.len(), 1);
    assert_eq!(snapshot.checkpoints.len(), 1);
    assert_eq!(snapshot.diffs.len(), 1);
    assert_eq!(snapshot.states.len(), 9);
    assert!(
        snapshot
            .states
            .iter()
            .all(|state| state.last_applied_sequence == 10)
    );
    assert_eq!(
        engine
            .read_events(0)
            .await
            .expect("read persisted events")
            .len(),
        10
    );

    engine.shutdown().await;

    repositories
        .database()
        .call(|connection| {
            connection.execute(
                "DELETE FROM projection_pending_approvals WHERE request_id = 'approval-1'",
                [],
            )?;
            connection.execute(
                "UPDATE projection_state SET last_applied_sequence = 0 WHERE projector = 'projection.pending-approvals'",
                [],
            )?;
            Ok(())
        })
        .await
        .expect("rewind pending approvals");

    let restarted =
        OrchestrationEngine::start(repositories.database().clone(), EngineOptions::default())
            .await
            .expect("engine restarts");
    let restarted_snapshot = load_snapshot(&restarted.repositories())
        .await
        .expect("restarted snapshot");
    assert_eq!(restarted_snapshot.approvals.len(), 1);
    assert_eq!(
        restarted_snapshot
            .states
            .iter()
            .find(|state| state.projector == "projection.pending-approvals")
            .expect("pending approvals state")
            .last_applied_sequence,
        10
    );
    restarted.shutdown().await;
}

#[tokio::test]
async fn persists_rejected_receipts_and_accepted_idempotency() {
    let repositories = migrated_repositories().await;
    let engine =
        OrchestrationEngine::start(repositories.database().clone(), EngineOptions::default())
            .await
            .expect("engine starts");
    let commands = fixture_commands();
    let project_create = commands[0].clone();
    let thread_create = commands[1].clone();
    let turn_start = commands[2].clone();

    engine
        .dispatch(project_create)
        .await
        .expect("project create");
    engine
        .dispatch(thread_create.clone())
        .await
        .expect("thread create");
    let first_turn = engine
        .dispatch(turn_start.clone())
        .await
        .expect("turn start");
    let duplicate_turn = engine.dispatch(turn_start).await.expect("duplicate turn");
    assert_eq!(duplicate_turn, first_turn);

    let duplicate_thread: OrchestrationCommand = serde_json::from_value(json!({
      "type": "thread.create",
      "commandId": "client:thread-create-duplicate",
      "threadId": "thread-1",
      "projectId": "project-1",
      "title": "dup",
      "kind": "coding",
      "modelSelection": { "provider": "codex", "model": "gpt-5" },
      "runtimeMode": "full-access",
      "interactionMode": "default",
      "branch": "codex/thread-1",
      "worktreePath": "C:/worktrees/thread-1",
      "createdAt": "2026-07-10T10:09:00.000Z"
    }))
    .expect("duplicate command decodes");

    let error = engine
        .dispatch(duplicate_thread.clone())
        .await
        .expect_err("duplicate thread rejected");
    match error {
        OrchestrationError::Invariant { .. } => {}
        other => panic!("unexpected duplicate error: {other}"),
    }

    let rejected_receipt = repositories
        .get_command_receipt("client:thread-create-duplicate".to_owned())
        .await
        .expect("receipt lookup")
        .expect("rejected receipt exists");
    assert_eq!(rejected_receipt.status, "rejected");

    let retry_error = engine
        .dispatch(duplicate_thread)
        .await
        .expect_err("retry stays rejected");
    match retry_error {
        OrchestrationError::PreviouslyRejected { command_id, .. } => {
            assert_eq!(command_id, "client:thread-create-duplicate");
        }
        other => panic!("unexpected retry error: {other}"),
    }

    engine.shutdown().await;
}

#[tokio::test]
async fn rolls_back_failed_projectors_and_keeps_queue_alive() {
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
    let commands = fixture_commands();

    for command in commands.iter().take(3).cloned() {
        engine.dispatch(command).await.expect("setup command");
    }

    hooks.fail_next_projector(
        "projection.thread-activities",
        Some("thread.activity.appended"),
    );
    let activity = commands[3].clone();
    let failed = engine
        .dispatch(activity.clone())
        .await
        .expect_err("activity failure injected");
    match failed {
        OrchestrationError::InjectedProjectorFailure { projector, .. } => {
            assert_eq!(projector, "projection.thread-activities");
        }
        other => panic!("unexpected projector failure: {other}"),
    }

    let after_failure = load_snapshot(&repositories)
        .await
        .expect("snapshot after failure");
    assert!(after_failure.activities.is_empty());
    assert!(
        repositories
            .get_command_receipt("client:activity-1".to_owned())
            .await
            .expect("receipt lookup")
            .is_none()
    );

    let retried = engine.dispatch(activity).await.expect("retry succeeds");
    assert_eq!(retried.sequence, 6);

    let next = engine
        .dispatch(commands[4].clone())
        .await
        .expect("queue continues after failure");
    assert_eq!(next.sequence, 7);

    engine.shutdown().await;
    let cancelled = engine
        .dispatch(commands[5].clone())
        .await
        .expect_err("shutdown");
    match cancelled {
        OrchestrationError::Cancelled | OrchestrationError::WorkerClosed => {}
        other => panic!("unexpected shutdown error: {other}"),
    }
}
