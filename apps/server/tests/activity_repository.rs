use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use coddy_studio_server::{
    activity::{
        ActivityActorControlState, ActivityActorSummary, ActivityCapabilities, ActivityChange,
        ActivityDelta, ActivityEntry, ActivityEntryKind, ActivityEntryTone,
        ActivityHistoryRecovery, ActivityLifecycle, ActivityObservationState, ActivityProjection,
        ActivityProjections, ActivityRecordKind, ActivityRepository, ActivityRepositoryError,
        ActivityRosterBucket, ActivityScopeRef, ActivityScopeSeed, ActivitySection,
        ActivitySectionHealth, ActivityWorkItemSummary, AgentActivityController,
        AgentActivitySource, ProviderActivityMutation,
    },
    persistence::{Database, run_migrations},
};
use serde_json::json;
use time::{Duration, UtcOffset};

#[path = "support/activity_time.rs"]
mod activity_time;
use activity_time::{activity_timestamp, recent_activity_anchor};

const MAX_ACTIVITY_MUTATIONS: usize = 256;

#[tokio::test]
async fn monitoring_disabled_mutations_are_noops_and_reads_fail_before_repository_access() {
    // Mutations caught: consulting the repository before the hard gate, or allowing disabled reads.
    let database = migrated_database().await;
    let controller = AgentActivityController::new(false);
    let projection =
        ActivityProjection::with_controller(ActivityRepository::new(database), controller);
    let scope = thread_scope("thread:monitoring-disabled", "monitoring-disabled");

    projection
        .ensure_scope(scope.clone())
        .await
        .expect("disabled ensure is a no-op");
    assert!(
        projection
            .apply(
                &scope.scope_id,
                "event:disabled".to_owned(),
                vec![
                    ProviderActivityMutation::upsert_actor(
                        "actor:disabled",
                        None,
                        "Disabled",
                        "running",
                    )
                    .expect("actor"),
                ],
                "2026-07-30T12:00:00Z".to_owned(),
            )
            .await
            .expect("disabled apply is a no-op")
            .is_empty()
    );

    assert!(matches!(
        projection.snapshot(&scope.scope).await,
        Err(ActivityRepositoryError::FeatureDisabled)
    ));
    assert!(matches!(
        projection
            .list_roster(
                &scope.scope,
                &scope.scope_id,
                ActivitySection::Subagents,
                ActivityRosterBucket::Active,
                None,
                10,
            )
            .await,
        Err(ActivityRepositoryError::FeatureDisabled)
    ));
    assert!(matches!(
        projection
            .list_detail(
                &scope.scope,
                &scope.scope_id,
                ActivityRecordKind::Actor,
                "actor:disabled",
                None,
                10,
            )
            .await,
        Err(ActivityRepositoryError::FeatureDisabled)
    ));
}

#[tokio::test]
async fn monitoring_disabled_drain_waits_for_an_admitted_apply_and_fences_its_publication() {
    // Mutations caught: dropping admission before persistence completes, or publishing an old generation.
    let database = migrated_database().await;
    let controller = AgentActivityController::new(true);
    let projection = ActivityProjection::with_controller(
        ActivityRepository::new(database.clone()),
        controller.clone(),
    );
    let scope = thread_scope("thread:monitoring-drain", "monitoring-drain");
    projection.ensure_scope(scope.clone()).await.expect("scope");
    let mut events = projection.subscribe();
    let observer = database
        .enable_queue_backpressure_observation_for_integration_test()
        .expect("exclusive queue observer");

    let (worker_entered_sender, worker_entered_receiver) = tokio::sync::oneshot::channel();
    let (worker_release_sender, worker_release_receiver) = std::sync::mpsc::sync_channel(1);
    let blocking_database = database.clone();
    let blocker = tokio::spawn(async move {
        blocking_database
            .call(move |_| {
                worker_entered_sender.send(()).expect("worker entered");
                worker_release_receiver.recv().expect("worker released");
                Ok(())
            })
            .await
            .expect("blocking database job");
    });
    worker_entered_receiver
        .await
        .expect("database worker entered");

    let applying = tokio::spawn({
        let projection = projection.clone();
        let scope_id = scope.scope_id.clone();
        async move {
            projection
                .apply(
                    &scope_id,
                    "event:admitted-before-disable".to_owned(),
                    vec![
                        ProviderActivityMutation::upsert_actor(
                            "actor:admitted",
                            None,
                            "Admitted",
                            "running",
                        )
                        .expect("actor"),
                    ],
                    "2026-07-30T12:00:00Z".to_owned(),
                )
                .await
        }
    });
    for _ in 0..100 {
        if database
            .queue_backpressure_snapshot_for_integration_test()
            .reserved_or_queued_jobs
            == 1
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(
        database
            .queue_backpressure_snapshot_for_integration_test()
            .reserved_or_queued_jobs,
        1,
        "the apply must be admitted and queued before disable starts"
    );

    let disabling = tokio::spawn({
        let controller = controller.clone();
        async move { controller.disable().await }
    });
    tokio::task::yield_now().await;
    assert!(!disabling.is_finished());
    worker_release_sender.send(()).expect("release worker");

    let deltas = applying
        .await
        .expect("apply task")
        .expect("admitted apply completes");
    assert!(!deltas.is_empty());
    disabling.await.expect("disable task");
    blocker.await.expect("blocker task");
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    drop(observer);
}

#[tokio::test]
async fn monitoring_disabled_waits_for_a_blocked_retention_worker_and_defers_reenable() {
    // Mutations caught: untracked retention work, early admission reopening, or leaked worker keys.
    let database = migrated_database().await;
    let controller = AgentActivityController::new(true);
    let projection = ActivityProjection::with_controller(
        ActivityRepository::new(database.clone()),
        controller.clone(),
    );
    let scope = thread_scope("thread:monitoring-retention", "monitoring-retention");
    projection.ensure_scope(scope.clone()).await.expect("scope");
    projection
        .apply(
            &scope.scope_id,
            "event:retention-owner".to_owned(),
            vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:retention-owner",
                    None,
                    "Retention owner",
                    "running",
                )
                .expect("owner"),
            ],
            "2099-01-01T00:00:00Z".to_owned(),
        )
        .await
        .expect("owner");

    let entries = (0..400)
        .map(|index| {
            let created_at = format!("2099-01-01T00:{:02}:{:02}Z", index / 60, index % 60);
            let entry = entry(
                &format!("entry:monitoring-retention:{index:03}"),
                ActivityRecordKind::Actor,
                "actor:retention-owner",
                &created_at,
            )
            .expect("entry");
            (
                entry.id.clone(),
                serde_json::to_string(&entry).expect("entry JSON"),
                created_at,
            )
        })
        .collect::<Vec<_>>();
    database
        .call({
            let scope_id = scope.scope_id.clone();
            move |connection| {
                let transaction = connection.transaction()?;
                for (entry_id, entry_json, created_at) in entries {
                    transaction.execute(
                        "INSERT INTO activity_entries (
                           scope_id, entry_id, owner_kind, owner_id, native_sort_key,
                           entry_json, created_at
                         ) VALUES (?, ?, 'actor', 'actor:retention-owner', ?, ?, ?)",
                        rusqlite::params![scope_id, entry_id, created_at, entry_json, created_at],
                    )?;
                }
                transaction.execute(
                    "INSERT INTO activity_entry_owners (
                       scope_id, owner_kind, owner_id, entry_count
                     ) VALUES (?, 'actor', 'actor:retention-owner', 400)
                     ON CONFLICT(scope_id, owner_kind, owner_id)
                     DO UPDATE SET entry_count = 400",
                    [&scope_id],
                )?;
                transaction.commit()?;
                Ok(())
            }
        })
        .await
        .expect("oversized entry fixture");

    projection
        .apply(
            &scope.scope_id,
            "event:retention-trigger".to_owned(),
            vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:retention-owner",
                    None,
                    "Retention owner updated",
                    "running",
                )
                .expect("updated owner"),
            ],
            "2099-01-01T01:00:00Z".to_owned(),
        )
        .await
        .expect("retention trigger");
    assert_eq!(projection.registry_counts_for_integration_test().1, 1);

    let observer = database
        .enable_queue_backpressure_observation_for_integration_test()
        .expect("exclusive queue observer");
    let (worker_entered_sender, worker_entered_receiver) = std::sync::mpsc::sync_channel(1);
    let (worker_release_sender, worker_release_receiver) = std::sync::mpsc::sync_channel(1);
    let blocking_database = database.clone();
    let blocker = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("blocker runtime")
            .block_on(async move {
                blocking_database
                    .call(move |_| {
                        worker_entered_sender.send(()).expect("worker entered");
                        worker_release_receiver.recv().expect("worker released");
                        Ok(())
                    })
                    .await
                    .expect("blocking database job");
            });
    });
    worker_entered_receiver
        .recv()
        .expect("database worker entered");

    for _ in 0..100 {
        if database
            .queue_backpressure_snapshot_for_integration_test()
            .reserved_or_queued_jobs
            == 1
        {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert_eq!(
        database
            .queue_backpressure_snapshot_for_integration_test()
            .reserved_or_queued_jobs,
        1,
        "the real retention worker must be queued behind the blocked SQLite worker"
    );

    let mut finalizing = tokio::spawn({
        let projection = projection.clone();
        async move {
            projection
                .interrupt_for_monitoring_disabled(AgentActivitySource::Chat)
                .await
        }
    });
    tokio::task::yield_now().await;
    assert!(!finalizing.is_finished());
    assert!(
        !controller.enable().enabled,
        "enable is deferred while finalization owns the lifecycle"
    );
    assert!(controller.admit().is_none());

    worker_release_sender
        .send(())
        .expect("release database worker");
    blocker.join().expect("blocker thread");
    let interrupted = tokio::time::timeout(std::time::Duration::from_secs(5), &mut finalizing)
        .await
        .expect("finalization drains")
        .expect("finalization task")
        .expect("finalization result");
    assert_eq!(interrupted, 1);
    assert!(controller.snapshot().enabled);
    assert!(controller.admit().is_some());
    assert_eq!(projection.registry_counts_for_integration_test(), (0, 0));
    drop(observer);
}

#[tokio::test]
async fn monitoring_disabled_finalization_interrupts_once_and_preserves_completed_history() {
    // Mutations caught: terminal-only interruption, duplicate state entries, or deleting completed history.
    let database = migrated_database().await;
    let controller = AgentActivityController::new(true);
    let projection =
        ActivityProjection::with_controller(ActivityRepository::new(database), controller.clone());
    let scope = thread_scope("thread:monitoring-finalize", "monitoring-finalize");
    projection.ensure_scope(scope.clone()).await.expect("scope");
    projection
        .apply(
            &scope.scope_id,
            "event:monitoring-fixture".to_owned(),
            vec![
                ProviderActivityMutation::upsert_actor("actor:running", None, "Running", "running")
                    .expect("running actor"),
                ProviderActivityMutation::upsert_actor(
                    "actor:completed",
                    None,
                    "Completed",
                    "completed",
                )
                .expect("completed actor"),
                ProviderActivityMutation::UpsertWorkItem(
                    work_item_summary(
                        "work:running",
                        Some("actor:running"),
                        ActivityLifecycle::Running,
                        "2099-07-30T12:00:00Z",
                        "2099-07-30T12:00:00Z",
                        None,
                    )
                    .expect("running work item"),
                ),
            ],
            "2099-07-30T12:00:00Z".to_owned(),
        )
        .await
        .expect("fixture");

    assert_eq!(
        projection
            .interrupt_for_monitoring_disabled(AgentActivitySource::Chat)
            .await
            .expect("first finalization"),
        2
    );
    assert_eq!(
        projection
            .interrupt_for_monitoring_disabled(AgentActivitySource::Chat)
            .await
            .expect("idempotent finalization"),
        0
    );
    assert!(
        !controller.snapshot().enabled,
        "finalization closes and drains projection admission"
    );
    assert_eq!(
        projection.registry_counts_for_integration_test(),
        (0, 0),
        "disable finalization clears both projection registries"
    );

    controller.enable();
    let snapshot = projection.snapshot(&scope.scope).await.expect("history");
    assert_eq!(
        snapshot
            .actors
            .iter()
            .find(|actor| actor.id == "actor:running")
            .expect("running actor retained")
            .status,
        ActivityLifecycle::Interrupted
    );
    assert_eq!(
        snapshot
            .actors
            .iter()
            .find(|actor| actor.id == "actor:completed")
            .expect("completed actor retained")
            .status,
        ActivityLifecycle::Completed
    );
    assert_eq!(
        snapshot
            .work_items
            .iter()
            .find(|work_item| work_item.id == "work:running")
            .expect("running work item retained")
            .status,
        ActivityLifecycle::Interrupted
    );
    for (record_kind, record_id) in [
        (ActivityRecordKind::Actor, "actor:running"),
        (ActivityRecordKind::WorkItem, "work:running"),
    ] {
        let detail = projection
            .list_detail(
                &scope.scope,
                &scope.scope_id,
                record_kind,
                record_id,
                None,
                10,
            )
            .await
            .expect("interruption detail");
        assert_eq!(detail.entries.len(), 1);
        assert_eq!(detail.entries[0].kind, ActivityEntryKind::State);
        assert_eq!(
            detail.entries[0].title,
            "Agent activity monitoring disabled"
        );
    }
}

#[tokio::test]
async fn source_specific_chat_finalization_does_not_interrupt_terminal_activity() {
    // Mutations caught: omitting the source filter or sharing one controller across projections.
    let database = migrated_database().await;
    let thread_scope = thread_scope("thread:source-specific-chat", "source-specific-chat");
    let terminal_scope = ActivityScopeSeed::terminal(
        "terminal:source-specific-chat",
        "generation:source-specific-chat",
        "source-specific-chat",
        "terminal:source-specific-chat",
        "codex",
        Some("codex"),
        ActivityCapabilities::structured_full(true),
    )
    .expect("terminal scope");
    let projections = ActivityProjections::new(
        ActivityRepository::new(database),
        AgentActivityController::new(true),
        AgentActivityController::new(true),
    );
    seed_running_actor(&projections.chat(), &thread_scope, "actor:chat").await;
    seed_running_actor(&projections.terminal(), &terminal_scope, "actor:terminal").await;

    assert_eq!(
        projections
            .chat()
            .interrupt_for_monitoring_disabled(AgentActivitySource::Chat)
            .await
            .expect("disable Chat"),
        1,
    );
    assert!(
        projections
            .terminal()
            .agent_activity_controller_for_integration_test()
            .snapshot()
            .enabled
    );
    projections
        .chat()
        .agent_activity_controller_for_integration_test()
        .enable();
    let thread_snapshot = projections
        .chat()
        .snapshot(&thread_scope.scope)
        .await
        .expect("thread snapshot");
    let terminal_snapshot = projections
        .terminal()
        .snapshot(&terminal_scope.scope)
        .await
        .expect("terminal snapshot");
    assert_eq!(
        thread_snapshot.actors[0].status,
        ActivityLifecycle::Interrupted
    );
    assert_eq!(
        terminal_snapshot.actors[0].status,
        ActivityLifecycle::Running
    );
}

#[tokio::test]
async fn source_specific_terminal_finalization_does_not_interrupt_chat_activity() {
    // Mutations caught: routing Terminal to Chat or clearing the other source's controller state.
    let database = migrated_database().await;
    let thread_scope = thread_scope(
        "thread:source-specific-terminal",
        "source-specific-terminal",
    );
    let terminal_scope = ActivityScopeSeed::terminal(
        "terminal:source-specific-terminal",
        "generation:source-specific-terminal",
        "source-specific-terminal",
        "terminal:source-specific-terminal",
        "codex",
        Some("codex"),
        ActivityCapabilities::structured_full(true),
    )
    .expect("terminal scope");
    let projections = ActivityProjections::new(
        ActivityRepository::new(database),
        AgentActivityController::new(true),
        AgentActivityController::new(true),
    );
    seed_running_actor(&projections.chat(), &thread_scope, "actor:chat").await;
    seed_running_actor(&projections.terminal(), &terminal_scope, "actor:terminal").await;

    assert_eq!(
        projections
            .terminal()
            .interrupt_for_monitoring_disabled(AgentActivitySource::Terminal)
            .await
            .expect("disable Terminal"),
        1,
    );
    assert!(
        projections
            .chat()
            .agent_activity_controller_for_integration_test()
            .snapshot()
            .enabled
    );
    projections
        .terminal()
        .agent_activity_controller_for_integration_test()
        .enable();
    let thread_snapshot = projections
        .chat()
        .snapshot(&thread_scope.scope)
        .await
        .expect("thread snapshot");
    let terminal_snapshot = projections
        .terminal()
        .snapshot(&terminal_scope.scope)
        .await
        .expect("terminal snapshot");
    assert_eq!(thread_snapshot.actors[0].status, ActivityLifecycle::Running);
    assert_eq!(
        terminal_snapshot.actors[0].status,
        ActivityLifecycle::Interrupted
    );
}

#[tokio::test]
async fn source_specific_mismatched_finalization_leaves_both_sources_unchanged() {
    // Mutation caught: trusting the call-site source instead of the routed projection identity.
    let database = migrated_database().await;
    let thread_scope = thread_scope(
        "thread:source-specific-mismatch",
        "source-specific-mismatch",
    );
    let terminal_scope = ActivityScopeSeed::terminal(
        "terminal:source-specific-mismatch",
        "generation:source-specific-mismatch",
        "source-specific-mismatch",
        "terminal:source-specific-mismatch",
        "codex",
        Some("codex"),
        ActivityCapabilities::structured_full(true),
    )
    .expect("terminal scope");
    let projections = ActivityProjections::new(
        ActivityRepository::new(database),
        AgentActivityController::new(true),
        AgentActivityController::new(true),
    );
    seed_running_actor(&projections.chat(), &thread_scope, "actor:chat").await;
    seed_running_actor(&projections.terminal(), &terminal_scope, "actor:terminal").await;
    let chat_registry_counts = projections.chat().registry_counts_for_integration_test();
    let terminal_registry_counts = projections
        .terminal()
        .registry_counts_for_integration_test();
    assert_ne!(chat_registry_counts, (0, 0));
    assert_ne!(terminal_registry_counts, (0, 0));
    let chat_controller_state = projections
        .chat()
        .agent_activity_controller_for_integration_test()
        .snapshot();
    let terminal_controller_state = projections
        .terminal()
        .agent_activity_controller_for_integration_test()
        .snapshot();
    let thread_snapshot = projections
        .chat()
        .snapshot(&thread_scope.scope)
        .await
        .expect("thread snapshot before mismatch");
    let terminal_snapshot = projections
        .terminal()
        .snapshot(&terminal_scope.scope)
        .await
        .expect("terminal snapshot before mismatch");

    assert!(matches!(
        projections
            .chat()
            .interrupt_for_monitoring_disabled(AgentActivitySource::Terminal)
            .await,
        Err(ActivityRepositoryError::InvalidScope(_))
    ));
    assert!(matches!(
        projections
            .terminal()
            .interrupt_for_monitoring_disabled(AgentActivitySource::Chat)
            .await,
        Err(ActivityRepositoryError::InvalidScope(_))
    ));
    assert_eq!(
        projections
            .chat()
            .agent_activity_controller_for_integration_test()
            .snapshot(),
        chat_controller_state
    );
    assert_eq!(
        projections
            .terminal()
            .agent_activity_controller_for_integration_test()
            .snapshot(),
        terminal_controller_state
    );
    assert_eq!(
        projections.chat().registry_counts_for_integration_test(),
        chat_registry_counts
    );
    assert_eq!(
        projections
            .terminal()
            .registry_counts_for_integration_test(),
        terminal_registry_counts
    );
    assert_eq!(
        projections
            .chat()
            .snapshot(&thread_scope.scope)
            .await
            .expect("thread snapshot after mismatch"),
        thread_snapshot
    );
    assert_eq!(
        projections
            .terminal()
            .snapshot(&terminal_scope.scope)
            .await
            .expect("terminal snapshot after mismatch"),
        terminal_snapshot
    );
}

#[tokio::test]
async fn monitoring_disabled_reactivation_records_each_disable_generation_once() {
    // Mutation caught: reusing one interruption-entry identity across distinct disable generations.
    let database = migrated_database().await;
    let controller = AgentActivityController::new(true);
    let projection =
        ActivityProjection::with_controller(ActivityRepository::new(database), controller.clone());
    let scope = thread_scope("thread:monitoring-reactivation", "monitoring-reactivation");
    projection.ensure_scope(scope.clone()).await.expect("scope");
    projection
        .apply(
            &scope.scope_id,
            "event:monitoring-reactivation-start".to_owned(),
            vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:reactivated",
                    None,
                    "Reactivated",
                    "running",
                )
                .expect("actor"),
            ],
            "2026-07-30T12:00:00Z".to_owned(),
        )
        .await
        .expect("running actor");

    assert_eq!(
        projection
            .interrupt_for_monitoring_disabled(AgentActivitySource::Chat)
            .await
            .expect("first disable"),
        1
    );
    assert_eq!(
        projection
            .interrupt_for_monitoring_disabled(AgentActivitySource::Chat)
            .await
            .expect("repeat first disable"),
        0
    );

    controller.enable();
    projection
        .apply(
            &scope.scope_id,
            "event:monitoring-reactivation-running".to_owned(),
            vec![
                ProviderActivityMutation::set_actor_status("actor:reactivated", "running")
                    .expect("reactivation"),
            ],
            "2099-07-30T12:00:00Z".to_owned(),
        )
        .await
        .expect("reactivated actor");
    assert_eq!(
        projection
            .interrupt_for_monitoring_disabled(AgentActivitySource::Chat)
            .await
            .expect("second disable"),
        1
    );
    assert_eq!(
        projection
            .interrupt_for_monitoring_disabled(AgentActivitySource::Chat)
            .await
            .expect("repeat second disable"),
        0
    );

    controller.enable();
    let detail = projection
        .list_detail(
            &scope.scope,
            &scope.scope_id,
            ActivityRecordKind::Actor,
            "actor:reactivated",
            None,
            10,
        )
        .await
        .expect("interruption history");
    assert_eq!(detail.entries.len(), 2);
    assert_ne!(detail.entries[0].id, detail.entries[1].id);
    assert!(
        detail
            .entries
            .iter()
            .all(|entry| entry.title == "Agent activity monitoring disabled")
    );
}

#[tokio::test]
async fn activity_batches_are_durable_idempotent_and_terminal_monotonic() {
    let timeline_start = recent_activity_anchor();
    let started_at = activity_timestamp(timeline_start, Duration::ZERO);
    let duplicate_at = activity_timestamp(timeline_start, Duration::seconds(1));
    let completed_at = activity_timestamp(timeline_start, Duration::seconds(2));
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
        "thread:thread-1",
        "thread-1",
        "codex",
        Some("codex"),
        ActivityCapabilities::structured_full(true),
    )
    .expect("valid scope");

    repository.ensure_scope(scope.clone()).await.expect("scope");
    let first = repository
        .apply_batch(
            &scope.scope_id,
            "codex:item:1",
            vec![
                ProviderActivityMutation::upsert_actor("actor:child-1", None, "Explore", "running")
                    .expect("valid actor"),
            ],
            &started_at,
        )
        .await
        .expect("batch")
        .into_iter()
        .next()
        .expect("new delta");
    assert_eq!((first.previous_revision, first.revision), (0, 1));

    assert!(
        repository
            .apply_batch(
                &scope.scope_id,
                "codex:item:1",
                vec![
                    ProviderActivityMutation::remove_actor("actor:child-1")
                        .expect("valid actor id"),
                ],
                &duplicate_at,
            )
            .await
            .expect("duplicate")
            .is_empty()
    );

    repository
        .apply_batch(
            &scope.scope_id,
            "codex:item:2",
            vec![
                ProviderActivityMutation::set_actor_status("actor:child-1", "completed")
                    .expect("valid status"),
            ],
            &completed_at,
        )
        .await
        .expect("complete");
    assert!(
        repository
            .apply_batch(
                &scope.scope_id,
                "codex:item:late",
                vec![
                    ProviderActivityMutation::set_actor_status("actor:child-1", "running",)
                        .expect("valid status")
                ],
                &duplicate_at,
            )
            .await
            .expect("late")
            .is_empty()
    );

    let snapshot = repository
        .snapshot(&ActivityScopeRef::Thread {
            thread_id: "thread-1".to_owned(),
        })
        .await
        .expect("snapshot");
    assert_eq!(snapshot.revision, 2);
    assert_eq!(snapshot.actors[0].status, ActivityLifecycle::Completed);
}

#[tokio::test]
async fn count_changes_emit_one_authoritative_scope_update_without_scope_field_changes() {
    let timeline_start = recent_activity_anchor();
    let started_at = activity_timestamp(timeline_start, Duration::ZERO);
    let completed_at = activity_timestamp(timeline_start, Duration::seconds(1));
    let database = migrated_database().await;
    let repository = ActivityRepository::new(database);
    let scope = thread_scope("thread:count-deltas", "count-deltas");
    repository.ensure_scope(scope.clone()).await.expect("scope");

    let started = repository
        .apply_batch(
            &scope.scope_id,
            "event:actor-started",
            vec![
                ProviderActivityMutation::upsert_actor("actor:counted", None, "Counted", "running")
                    .expect("actor"),
            ],
            &started_at,
        )
        .await
        .expect("started batch")
        .into_iter()
        .next()
        .expect("started delta");
    assert_authoritative_subagent_counts(&started, 1, 0);

    let completed = repository
        .apply_batch(
            &scope.scope_id,
            "event:actor-completed",
            vec![
                ProviderActivityMutation::SetScope {
                    capabilities: scope.capabilities.clone(),
                    observation_state: ActivityObservationState::Live,
                },
                ProviderActivityMutation::set_actor_status("actor:counted", "completed")
                    .expect("completed actor"),
            ],
            &completed_at,
        )
        .await
        .expect("completed batch")
        .into_iter()
        .next()
        .expect("completed delta");
    assert_authoritative_subagent_counts(&completed, 0, 1);
}

#[tokio::test]
async fn maximum_mutation_batch_persists_ordered_bounded_deltas_and_replays_idempotently() {
    let database = migrated_database().await;
    let repository = ActivityRepository::new(database.clone());
    let scope = thread_scope("thread:maximum-batch", "maximum-batch");
    repository.ensure_scope(scope.clone()).await.expect("scope");

    repository
        .apply_batch(
            &scope.scope_id,
            "event:maximum-batch",
            actor_mutations(MAX_ACTIVITY_MUTATIONS),
            "2026-07-22T12:00:00Z",
        )
        .await
        .expect("maximum supported batch");

    let encoded_deltas = database
        .call(|connection| {
            let mut statement = connection.prepare(
                "SELECT delta_json FROM activity_journal
                 WHERE scope_id = 'thread:maximum-batch'
                 ORDER BY revision",
            )?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            let mut deltas = Vec::new();
            for row in rows {
                deltas.push(row?);
            }
            Ok(deltas)
        })
        .await
        .expect("journal");
    let deltas = encoded_deltas
        .iter()
        .map(|encoded| serde_json::from_str::<ActivityDelta>(encoded).expect("activity delta"))
        .collect::<Vec<_>>();
    assert_eq!(deltas.len(), 2);
    assert_eq!(
        deltas
            .iter()
            .map(|delta| (delta.previous_revision, delta.revision))
            .collect::<Vec<_>>(),
        [(0, 1), (1, 2)]
    );
    assert!(deltas.iter().all(|delta| delta.changes.len() <= 256));
    assert!(matches!(
        deltas[0].changes.first(),
        Some(ActivityChange::ScopeUpdated { .. })
    ));
    assert_eq!(
        deltas
            .iter()
            .flat_map(|delta| &delta.changes)
            .filter(|change| matches!(change, ActivityChange::ScopeUpdated { .. }))
            .count(),
        1
    );
    assert_eq!(
        deltas
            .iter()
            .flat_map(|delta| &delta.changes)
            .filter(|change| matches!(change, ActivityChange::ActorUpserted { .. }))
            .count(),
        MAX_ACTIVITY_MUTATIONS
    );

    repository
        .apply_batch(
            &scope.scope_id,
            "event:maximum-batch",
            actor_mutations(MAX_ACTIVITY_MUTATIONS),
            "2026-07-22T12:00:01Z",
        )
        .await
        .expect("duplicate replay");
    let snapshot = repository
        .snapshot(&ActivityScopeRef::Thread {
            thread_id: "maximum-batch".to_owned(),
        })
        .await
        .expect("snapshot");
    assert_eq!(snapshot.revision, 2);
    assert!(snapshot.actors_has_more);
    assert_eq!(
        snapshot.counts.subagents.active,
        MAX_ACTIVITY_MUTATIONS as u64
    );
    let (record_count, journal_count) = database
        .call(|connection| {
            Ok((
                connection.query_row(
                    "SELECT COUNT(*) FROM activity_records
                     WHERE scope_id = 'thread:maximum-batch'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?,
                connection.query_row(
                    "SELECT COUNT(*) FROM activity_journal
                     WHERE scope_id = 'thread:maximum-batch'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?,
            ))
        })
        .await
        .expect("persisted counts");
    assert_eq!(record_count, MAX_ACTIVITY_MUTATIONS as i64);
    assert_eq!(journal_count, 2);
    let canonical_keys = database
        .call(|connection| {
            let mut statement = connection.prepare(
                "SELECT native_event_key FROM activity_journal
                 WHERE scope_id = 'thread:maximum-batch'
                   AND event_key_namespace = 'canonical'
                 ORDER BY revision",
            )?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            let mut keys = Vec::new();
            for row in rows {
                keys.push(row?);
            }
            Ok(keys)
        })
        .await
        .expect("canonical journal keys");
    assert_eq!(canonical_keys.len(), 2);
    assert!(canonical_keys.iter().all(|key| key.len() == 76));
    assert_ne!(canonical_keys[0], canonical_keys[1]);
}

#[tokio::test]
async fn mutation_batch_overflow_is_rejected_without_partial_projection() {
    let database = migrated_database().await;
    let repository = ActivityRepository::new(database.clone());
    let scope = thread_scope("thread:overflow-batch", "overflow-batch");
    repository.ensure_scope(scope.clone()).await.expect("scope");

    assert!(matches!(
        repository
            .apply_batch(
                &scope.scope_id,
                "event:overflow-batch",
                actor_mutations(MAX_ACTIVITY_MUTATIONS + 1),
                "2026-07-22T12:00:00Z",
            )
            .await,
        Err(ActivityRepositoryError::TooManyMutations)
    ));
    let snapshot = repository
        .snapshot(&ActivityScopeRef::Thread {
            thread_id: "overflow-batch".to_owned(),
        })
        .await
        .expect("snapshot");
    assert_eq!(snapshot.revision, 0);
    assert!(snapshot.actors.is_empty());
    let journal_count = database
        .call(|connection| {
            Ok(connection.query_row(
                "SELECT COUNT(*) FROM activity_journal
                 WHERE scope_id = 'thread:overflow-batch'",
                [],
                |row| row.get::<_, i64>(0),
            )?)
        })
        .await
        .expect("journal count");
    assert_eq!(journal_count, 0);
}

#[tokio::test]
async fn canonical_journal_key_text_cannot_alias_a_distinct_raw_event_key() {
    let database = migrated_database().await;
    let repository = ActivityRepository::new(database.clone());
    let scope = thread_scope("thread:journal-alias", "journal-alias");
    repository.ensure_scope(scope.clone()).await.expect("scope");

    repository
        .apply_batch(
            &scope.scope_id,
            "event:first",
            actor_mutations(1),
            "2026-07-22T12:00:00Z",
        )
        .await
        .expect("first event");
    let first_canonical_key = database
        .call(|connection| {
            Ok(connection.query_row(
                "SELECT native_event_key FROM activity_journal
                 WHERE scope_id = 'thread:journal-alias' AND revision = 1",
                [],
                |row| row.get::<_, String>(0),
            )?)
        })
        .await
        .expect("first canonical key");

    let second = repository
        .apply_batch(
            &scope.scope_id,
            &first_canonical_key,
            vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:distinct",
                    None,
                    "Distinct",
                    "running",
                )
                .expect("actor"),
            ],
            "2026-07-22T12:00:01Z",
        )
        .await
        .expect("distinct raw event");

    assert!(!second.is_empty());
    let snapshot = repository
        .snapshot(&ActivityScopeRef::Thread {
            thread_id: "journal-alias".to_owned(),
        })
        .await
        .expect("snapshot");
    assert_eq!(snapshot.revision, 2);
    assert_eq!(snapshot.actors.len(), 2);
}

#[tokio::test]
async fn migrated_legacy_raw_journal_key_remains_replay_safe() {
    let database = Database::open_in_memory().await.expect("database");
    database
        .call(|connection| {
            run_migrations(connection, Some(34))?;
            Ok(())
        })
        .await
        .expect("legacy migrations");
    let repository = ActivityRepository::new(database.clone());
    let scope = thread_scope("thread:legacy-replay", "legacy-replay");
    repository.ensure_scope(scope.clone()).await.expect("scope");
    let legacy_scope_id = scope.scope_id.clone();
    database
        .call(move |connection| {
            connection.execute(
                "INSERT INTO activity_journal (
                   scope_id, revision, native_event_key, delta_json, created_at
                 ) VALUES (?, 1, 'event:legacy', '{}', '2026-07-22T12:00:00Z')",
                [&legacy_scope_id],
            )?;
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("journal namespace migration");

    let replay = repository
        .apply_batch(
            &scope.scope_id,
            "event:legacy",
            actor_mutations(1),
            "2026-07-22T12:00:01Z",
        )
        .await
        .expect("legacy replay");
    assert!(replay.is_empty());
    let namespace = database
        .call(|connection| {
            Ok(connection.query_row(
                "SELECT event_key_namespace FROM activity_journal
                 WHERE scope_id = 'thread:legacy-replay' AND revision = 1",
                [],
                |row| row.get::<_, String>(0),
            )?)
        })
        .await
        .expect("legacy namespace");
    assert_eq!(namespace, "legacy");
}

#[test]
fn scope_constructors_reject_invalid_capability_combinations() {
    let invalid = ActivityCapabilities {
        actors: false,
        attributed_activity: true,
        background_work: false,
        history_recovery: ActivityHistoryRecovery::None,
        terminal_observation: false,
        targeted_actor_cancellation: false,
    };

    assert!(
        ActivityScopeSeed::thread(
            "thread:invalid-capabilities",
            "invalid-capabilities",
            "codex",
            Some("codex"),
            invalid,
        )
        .is_err()
    );
}

#[tokio::test]
async fn invalid_references_roll_back_the_entire_batch_and_leave_the_event_key_reusable() {
    let database = migrated_database().await;
    let repository = ActivityRepository::new(database);
    let scope = thread_scope("thread:rollback", "rollback");
    repository.ensure_scope(scope.clone()).await.expect("scope");

    let error = repository
        .apply_batch(
            &scope.scope_id,
            "event:rollback",
            vec![
                ProviderActivityMutation::upsert_actor("actor:valid", None, "Valid", "running")
                    .expect("valid actor"),
                ProviderActivityMutation::AppendEntry(
                    ActivityEntry::try_new(
                        "entry:orphan",
                        ActivityRecordKind::Actor,
                        "actor:missing",
                        ActivityEntryKind::Commentary,
                        "Orphan",
                        None,
                        ActivityEntryTone::Info,
                        "2026-07-22T12:00:00Z",
                    )
                    .expect("valid entry"),
                ),
            ],
            "2026-07-22T12:00:00Z",
        )
        .await
        .expect_err("orphan owner must reject the batch");
    assert!(matches!(
        error,
        ActivityRepositoryError::InvalidReference(ref id) if id == "actor:missing"
    ));

    let empty = repository
        .snapshot(&ActivityScopeRef::Thread {
            thread_id: "rollback".to_owned(),
        })
        .await
        .expect("snapshot");
    assert_eq!(empty.revision, 0);
    assert!(empty.actors.is_empty());

    let retry = repository
        .apply_batch(
            &scope.scope_id,
            "event:rollback",
            vec![
                ProviderActivityMutation::upsert_actor("actor:valid", None, "Valid", "running")
                    .expect("valid actor"),
            ],
            "2026-07-22T12:00:00Z",
        )
        .await
        .expect("retry")
        .into_iter()
        .next()
        .expect("rolled-back key is reusable");
    assert_eq!(retry.revision, 1);
}

#[tokio::test]
async fn terminal_generation_replacement_interrupts_prior_active_records_atomically() {
    let database = migrated_database().await;
    let repository = ActivityRepository::new(database.clone());
    let first_scope = ActivityScopeSeed::terminal(
        "terminal:scope-1",
        "generation-1",
        "thread-terminal",
        "terminal-1",
        "codex",
        Some("codex"),
        ActivityCapabilities::structured_full(true),
    )
    .expect("first terminal scope");
    repository
        .ensure_scope(first_scope.clone())
        .await
        .expect("first scope");
    repository
        .apply_batch(
            &first_scope.scope_id,
            "terminal:event-1",
            vec![
                ProviderActivityMutation::upsert_actor("actor:old", None, "Old worker", "running")
                    .expect("valid actor"),
            ],
            "2026-07-22T12:00:00Z",
        )
        .await
        .expect("old activity");

    let second_scope = ActivityScopeSeed::terminal(
        "terminal:scope-2",
        "generation-2",
        "thread-terminal",
        "terminal-1",
        "codex",
        Some("codex"),
        ActivityCapabilities::structured_full(true),
    )
    .expect("second terminal scope");
    repository
        .ensure_scope(second_scope)
        .await
        .expect("replacement scope");

    let (old_is_current, old_status, old_json_status, current_count) = database
        .call(|connection| {
            let old = connection.query_row(
                "SELECT s.is_current, r.status, json_extract(r.summary_json, '$.status')
                 FROM activity_scopes s
                 JOIN activity_records r ON r.scope_id = s.scope_id
                 WHERE s.scope_id = 'terminal:scope-1' AND r.record_id = 'actor:old'",
                [],
                |row| {
                    Ok((
                        row.get::<_, bool>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )?;
            let current_count = connection.query_row(
                "SELECT COUNT(*) FROM activity_scopes
                 WHERE thread_id = 'thread-terminal' AND terminal_id = 'terminal-1'
                   AND is_current = 1",
                [],
                |row| row.get::<_, i64>(0),
            )?;
            Ok((old.0, old.1, old.2, current_count))
        })
        .await
        .expect("generation rows");
    assert!(!old_is_current);
    assert_eq!(old_status, "interrupted");
    assert_eq!(old_json_status, "interrupted");
    assert_eq!(current_count, 1);
}

#[tokio::test]
async fn roster_and_detail_use_bounded_keyset_cursors() {
    let timeline_start = recent_activity_anchor();
    let actors_at = activity_timestamp(timeline_start, Duration::ZERO);
    let database = migrated_database().await;
    let repository = ActivityRepository::new(database);
    let scope = thread_scope("thread:paging", "paging");
    repository.ensure_scope(scope.clone()).await.expect("scope");
    repository
        .apply_batch(
            &scope.scope_id,
            "event:actors",
            (1..=3)
                .map(|index| {
                    ProviderActivityMutation::upsert_actor(
                        format!("actor:{index}"),
                        None,
                        format!("Actor {index}"),
                        "completed",
                    )
                    .expect("valid actor")
                })
                .collect(),
            &actors_at,
        )
        .await
        .expect("actors");
    repository
        .apply_batch(
            &scope.scope_id,
            "event:entries",
            (1..=3)
                .map(|index| {
                    ProviderActivityMutation::AppendEntry(
                        ActivityEntry::try_new(
                            format!("entry:{index}"),
                            ActivityRecordKind::Actor,
                            "actor:3",
                            ActivityEntryKind::Commentary,
                            format!("Entry {index}"),
                            None,
                            ActivityEntryTone::Info,
                            activity_timestamp(timeline_start, Duration::seconds(index)),
                        )
                        .expect("valid entry"),
                    )
                })
                .collect(),
            &activity_timestamp(timeline_start, Duration::seconds(4)),
        )
        .await
        .expect("entries");

    let first = repository
        .list_roster(
            &scope.scope,
            &scope.scope_id,
            ActivitySection::Subagents,
            ActivityRosterBucket::Done,
            None,
            2,
        )
        .await
        .expect("first roster page");
    assert_eq!(first.records.len(), 2);
    let roster_cursor = first.next_cursor.expect("roster cursor");
    let second = repository
        .list_roster(
            &scope.scope,
            &scope.scope_id,
            ActivitySection::Subagents,
            ActivityRosterBucket::Done,
            Some(&roster_cursor),
            2,
        )
        .await
        .expect("second roster page");
    assert_eq!(second.records.len(), 1);
    assert!(second.next_cursor.is_none());

    let first_detail = repository
        .list_detail(
            &scope.scope,
            &scope.scope_id,
            ActivityRecordKind::Actor,
            "actor:3",
            None,
            2,
        )
        .await
        .expect("first detail page");
    assert_eq!(
        first_detail
            .entries
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        vec!["entry:3", "entry:2"]
    );
    let detail_cursor = first_detail.next_cursor.expect("detail cursor");
    let second_detail = repository
        .list_detail(
            &scope.scope,
            &scope.scope_id,
            ActivityRecordKind::Actor,
            "actor:3",
            Some(&detail_cursor),
            500,
        )
        .await
        .expect("second detail page");
    assert_eq!(second_detail.entries[0].id, "entry:1");
    assert!(second_detail.next_cursor.is_none());

    assert!(matches!(
        repository
            .list_roster(
                &scope.scope,
                &scope.scope_id,
                ActivitySection::Subagents,
                ActivityRosterBucket::Done,
                Some("not-base64!"),
                10,
            )
            .await,
        Err(ActivityRepositoryError::InvalidCursor)
    ));
}

#[test]
fn timestamps_are_canonical_and_record_lifecycle_chronology_is_validated() {
    let canonical = actor_summary(
        "actor:canonical",
        None,
        ActivityLifecycle::Running,
        "2026-07-22T10:00:00-02:00",
        "2026-07-22T14:00:00+02:00",
        None,
    )
    .expect("offset-equivalent timestamps");
    assert_eq!(canonical.started_at, "2026-07-22T12:00:00.000000000Z");
    assert_eq!(canonical.updated_at, "2026-07-22T12:00:00.000000000Z");

    assert!(
        actor_summary(
            "actor:backward",
            None,
            ActivityLifecycle::Running,
            "2026-07-22T12:00:01Z",
            "2026-07-22T12:00:00Z",
            None,
        )
        .is_err()
    );
    assert!(
        work_item_summary(
            "work:backward",
            None,
            ActivityLifecycle::Completed,
            "2026-07-22T12:00:00Z",
            "2026-07-22T12:00:02Z",
            Some("2026-07-22T12:00:01Z"),
        )
        .is_err()
    );
}

#[tokio::test]
async fn offset_late_events_do_not_regress_terminal_records() {
    let timeline_start = recent_activity_anchor();
    let completed_at = activity_timestamp(timeline_start, Duration::ZERO);
    let late_offset =
        timeline_start.to_offset(UtcOffset::from_hms(2, 0, 0).expect("valid UTC offset"));
    let late_at = activity_timestamp(late_offset, -Duration::hours(1));
    let database = migrated_database().await;
    let repository = ActivityRepository::new(database);
    let scope = thread_scope("thread:offset-late", "offset-late");
    repository.ensure_scope(scope.clone()).await.expect("scope");
    repository
        .apply_batch(
            &scope.scope_id,
            "event:complete",
            vec![
                ProviderActivityMutation::upsert_actor("actor:offset", None, "Offset", "completed")
                    .expect("actor"),
            ],
            &completed_at,
        )
        .await
        .expect("complete");

    assert!(
        repository
            .apply_batch(
                &scope.scope_id,
                "event:late-offset",
                vec![
                    ProviderActivityMutation::set_actor_status("actor:offset", "running")
                        .expect("status"),
                ],
                &late_at,
            )
            .await
            .expect("late offset")
            .is_empty()
    );
    let snapshot = repository
        .snapshot(&ActivityScopeRef::Thread {
            thread_id: "offset-late".to_owned(),
        })
        .await
        .expect("snapshot");
    assert_eq!(snapshot.revision, 1);
    assert_eq!(snapshot.actors[0].status, ActivityLifecycle::Completed);
}

#[tokio::test]
async fn canonical_fractional_timestamps_sort_by_instant() {
    let timeline_start = recent_activity_anchor();
    let whole_at = activity_timestamp(timeline_start, Duration::ZERO);
    let fractional_at = activity_timestamp(timeline_start, Duration::milliseconds(100));
    let batch_at = activity_timestamp(timeline_start, Duration::seconds(1));
    let database = migrated_database().await;
    let repository = ActivityRepository::new(database);
    let scope = thread_scope("thread:fractional-order", "fractional-order");
    repository.ensure_scope(scope.clone()).await.expect("scope");
    repository
        .apply_batch(
            &scope.scope_id,
            "event:fractional-order",
            vec![
                ProviderActivityMutation::UpsertActor(
                    actor_summary(
                        "actor:whole",
                        None,
                        ActivityLifecycle::Completed,
                        &whole_at,
                        &whole_at,
                        Some(&whole_at),
                    )
                    .expect("whole second"),
                ),
                ProviderActivityMutation::UpsertActor(
                    actor_summary(
                        "actor:fractional",
                        None,
                        ActivityLifecycle::Completed,
                        &whole_at,
                        &fractional_at,
                        Some(&fractional_at),
                    )
                    .expect("fractional second"),
                ),
            ],
            &batch_at,
        )
        .await
        .expect("actors");
    let page = repository
        .list_roster(
            &scope.scope,
            &scope.scope_id,
            ActivitySection::Subagents,
            ActivityRosterBucket::Done,
            None,
            10,
        )
        .await
        .expect("roster");
    assert!(matches!(
        &page.records[0],
        coddy_studio_server::activity::ActivityRecordSummary::Actor(actor)
            if actor.id == "actor:fractional"
    ));
}

#[tokio::test]
async fn generation_replacement_never_moves_record_timestamps_backward() {
    let database = migrated_database().await;
    let repository = ActivityRepository::new(database.clone());
    let first = terminal_scope(
        "terminal:future-1",
        "future-generation-1",
        "future-terminal",
    );
    repository.ensure_scope(first.clone()).await.expect("scope");
    repository
        .apply_batch(
            &first.scope_id,
            "event:future",
            vec![
                ProviderActivityMutation::upsert_actor("actor:future", None, "Future", "running")
                    .expect("actor"),
            ],
            "2099-01-01T00:00:00+00:00",
        )
        .await
        .expect("future activity");
    repository
        .ensure_scope(terminal_scope(
            "terminal:future-2",
            "future-generation-2",
            "future-terminal",
        ))
        .await
        .expect("replacement");

    let (updated_at, terminal_at, json_updated_at, json_terminal_at) = database
        .call(|connection| {
            connection
                .query_row(
                    "SELECT updated_at, terminal_at,
                            json_extract(summary_json, '$.updatedAt'),
                            json_extract(summary_json, '$.terminalAt')
                     FROM activity_records
                     WHERE scope_id = 'terminal:future-1' AND record_id = 'actor:future'",
                    [],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                        ))
                    },
                )
                .map_err(Into::into)
        })
        .await
        .expect("record timestamps");
    assert_eq!(updated_at, "2099-01-01T00:00:00.000000000Z");
    assert_eq!(terminal_at, updated_at);
    assert_eq!(json_updated_at, updated_at);
    assert_eq!(json_terminal_at, updated_at);
}

#[tokio::test]
async fn ordered_relationship_validation_is_scope_local_and_deletion_safe() {
    let database = migrated_database().await;
    let repository = ActivityRepository::new(database);
    let first = thread_scope("thread:relationships-a", "relationships-a");
    let second = thread_scope("thread:relationships-b", "relationships-b");
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
            &first.scope_id,
            "event:root",
            vec![ProviderActivityMutation::UpsertActor(
                actor_summary(
                    "actor:root",
                    None,
                    ActivityLifecycle::Running,
                    "2026-07-22T12:00:00Z",
                    "2026-07-22T12:00:00Z",
                    None,
                )
                .expect("root"),
            )],
            "2026-07-22T12:00:00Z",
        )
        .await
        .expect("root");

    for (event_key, mutation) in [
        (
            "event:cross-actor",
            ProviderActivityMutation::UpsertActor(
                actor_summary(
                    "actor:cross-scope-child",
                    Some("actor:root"),
                    ActivityLifecycle::Running,
                    "2026-07-22T12:00:00Z",
                    "2026-07-22T12:00:00Z",
                    None,
                )
                .expect("child"),
            ),
        ),
        (
            "event:cross-work",
            ProviderActivityMutation::UpsertWorkItem(
                work_item_summary(
                    "work:cross-scope",
                    Some("actor:root"),
                    ActivityLifecycle::Running,
                    "2026-07-22T12:00:00Z",
                    "2026-07-22T12:00:00Z",
                    None,
                )
                .expect("work"),
            ),
        ),
    ] {
        assert!(matches!(
            repository
                .apply_batch(
                    &second.scope_id,
                    event_key,
                    vec![mutation],
                    "2026-07-22T12:00:00Z",
                )
                .await,
            Err(ActivityRepositoryError::InvalidReference(_))
        ));
    }

    repository
        .apply_batch(
            &first.scope_id,
            "event:child",
            vec![ProviderActivityMutation::UpsertActor(
                actor_summary(
                    "actor:child",
                    Some("actor:root"),
                    ActivityLifecycle::Running,
                    "2026-07-22T12:00:00Z",
                    "2026-07-22T12:00:00Z",
                    None,
                )
                .expect("child"),
            )],
            "2026-07-22T12:00:00Z",
        )
        .await
        .expect("same-scope child");
    assert!(matches!(
        repository
            .apply_batch(
                &first.scope_id,
                "event:delete-with-child",
                vec![ProviderActivityMutation::remove_actor("actor:root").expect("root id"),],
                "2026-07-22T12:00:01Z",
            )
            .await,
        Err(ActivityRepositoryError::InvalidReference(_))
    ));

    repository
        .apply_batch(
            &first.scope_id,
            "event:work",
            vec![ProviderActivityMutation::UpsertWorkItem(
                work_item_summary(
                    "work:owned",
                    Some("actor:root"),
                    ActivityLifecycle::Running,
                    "2026-07-22T12:00:00Z",
                    "2026-07-22T12:00:00Z",
                    None,
                )
                .expect("work"),
            )],
            "2026-07-22T12:00:00Z",
        )
        .await
        .expect("same-scope work");

    let revision_before = repository
        .snapshot(&ActivityScopeRef::Thread {
            thread_id: "relationships-a".to_owned(),
        })
        .await
        .expect("snapshot")
        .revision;
    assert!(matches!(
        repository
            .apply_batch(
                &first.scope_id,
                "event:ordered-rollback",
                vec![
                    ProviderActivityMutation::remove_actor("actor:child").expect("child id"),
                    ProviderActivityMutation::remove_actor("actor:root").expect("root id"),
                ],
                "2026-07-22T12:00:01Z",
            )
            .await,
        Err(ActivityRepositoryError::InvalidReference(_))
    ));
    let after_rollback = repository
        .snapshot(&ActivityScopeRef::Thread {
            thread_id: "relationships-a".to_owned(),
        })
        .await
        .expect("snapshot");
    assert_eq!(after_rollback.revision, revision_before);
    assert_eq!(after_rollback.actors.len(), 2);

    repository
        .apply_batch(
            &first.scope_id,
            "event:ordered-rollback",
            vec![
                ProviderActivityMutation::remove_actor("actor:child").expect("child id"),
                ProviderActivityMutation::remove_work_item("work:owned").expect("work id"),
                ProviderActivityMutation::remove_actor("actor:root").expect("root id"),
            ],
            "2026-07-22T12:00:02Z",
        )
        .await
        .expect("ordered deletion")
        .into_iter()
        .next()
        .expect("effective deletion");
}

#[tokio::test]
async fn actor_reparenting_cycles_are_rejected_with_transactional_rollback() {
    let database = migrated_database().await;
    let repository = ActivityRepository::new(database);
    let scope = thread_scope("thread:actor-cycle", "actor-cycle");
    repository.ensure_scope(scope.clone()).await.expect("scope");
    repository
        .apply_batch(
            &scope.scope_id,
            "event:cycle-seed",
            vec![
                ProviderActivityMutation::upsert_actor("actor:cycle:a", None, "Actor A", "running")
                    .expect("actor a"),
                ProviderActivityMutation::upsert_actor("actor:cycle:b", None, "Actor B", "running")
                    .expect("actor b"),
            ],
            "2026-07-22T12:00:00Z",
        )
        .await
        .expect("cycle seed");
    let before = repository
        .snapshot(&scope.scope)
        .await
        .expect("before cycle");

    let cycle = repository
        .apply_batch(
            &scope.scope_id,
            "event:two-node-cycle",
            vec![
                ProviderActivityMutation::UpsertActor(
                    actor_summary(
                        "actor:cycle:a",
                        Some("actor:cycle:b"),
                        ActivityLifecycle::Running,
                        "2026-07-22T12:00:00Z",
                        "2026-07-22T12:01:00Z",
                        None,
                    )
                    .expect("actor a reparent"),
                ),
                ProviderActivityMutation::UpsertActor(
                    actor_summary(
                        "actor:cycle:b",
                        Some("actor:cycle:a"),
                        ActivityLifecycle::Running,
                        "2026-07-22T12:00:00Z",
                        "2026-07-22T12:01:00Z",
                        None,
                    )
                    .expect("actor b reparent"),
                ),
            ],
            "2026-07-22T12:01:00Z",
        )
        .await;
    assert!(matches!(
        cycle,
        Err(ActivityRepositoryError::InvalidReference(_))
    ));

    let after = repository
        .snapshot(&scope.scope)
        .await
        .expect("after cycle");
    assert_eq!(after.revision, before.revision);
    assert_eq!(after.actors, before.actors);

    repository
        .apply_batch(
            &scope.scope_id,
            "event:two-node-cycle",
            vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:cycle:c",
                    Some("actor:cycle:a"),
                    "Actor C",
                    "running",
                )
                .expect("valid child"),
            ],
            "2026-07-22T12:02:00Z",
        )
        .await
        .expect("cycle rejection leaves the event key reusable");
    let after_reuse = repository
        .snapshot(&scope.scope)
        .await
        .expect("after event-key reuse");
    assert_eq!(after_reuse.revision, before.revision + 1);
    assert!(
        after_reuse
            .actors
            .iter()
            .any(|actor| actor.id == "actor:cycle:c")
    );
}

#[tokio::test]
async fn actor_lineage_v1_depth_budget_rejects_the_sixty_fifth_ancestor() {
    let database = migrated_database().await;
    let repository = ActivityRepository::new(database);
    let scope = thread_scope("thread:actor-depth-budget", "actor-depth-budget");
    repository.ensure_scope(scope.clone()).await.expect("scope");
    let mutations = (0..=64)
        .map(|index| {
            ProviderActivityMutation::upsert_actor(
                format!("actor:depth:{index}"),
                (index > 0)
                    .then(|| format!("actor:depth:{}", index - 1))
                    .as_deref(),
                format!("Actor {index}"),
                "running",
            )
            .expect("valid depth actor")
        })
        .collect();
    repository
        .apply_batch(
            &scope.scope_id,
            "event:depth-at-limit",
            mutations,
            "2026-07-22T12:00:00Z",
        )
        .await
        .expect("lineage at v1 limit");
    let at_limit = repository.snapshot(&scope.scope).await.expect("at limit");

    let beyond_limit = repository
        .apply_batch(
            &scope.scope_id,
            "event:depth-beyond-limit",
            vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:depth:65",
                    Some("actor:depth:64"),
                    "Actor 65",
                    "running",
                )
                .expect("valid actor shape"),
            ],
            "2026-07-22T12:01:00Z",
        )
        .await;
    assert!(matches!(
        beyond_limit,
        Err(ActivityRepositoryError::InvalidReference(_))
    ));
    assert_eq!(
        repository
            .snapshot(&scope.scope)
            .await
            .expect("after rejection")
            .revision,
        at_limit.revision
    );

    repository
        .apply_batch(
            &scope.scope_id,
            "event:depth-beyond-limit",
            vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:depth:sibling",
                    Some("actor:depth:0"),
                    "Sibling",
                    "running",
                )
                .expect("valid sibling"),
            ],
            "2026-07-22T12:02:00Z",
        )
        .await
        .expect("rejected event key remains reusable");
    assert_eq!(
        repository
            .snapshot(&scope.scope)
            .await
            .expect("after reuse")
            .revision,
        at_limit.revision + 1
    );
}

#[tokio::test]
async fn actor_and_work_item_entries_block_owner_deletion() {
    let database = migrated_database().await;
    let repository = ActivityRepository::new(database);
    let scope = thread_scope("thread:entry-owners", "entry-owners");
    repository.ensure_scope(scope.clone()).await.expect("scope");
    repository
        .apply_batch(
            &scope.scope_id,
            "event:entry-owners",
            vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:entry-owner",
                    None,
                    "Entry owner",
                    "running",
                )
                .expect("actor"),
                ProviderActivityMutation::upsert_actor(
                    "actor:entry-only",
                    None,
                    "Entry only",
                    "running",
                )
                .expect("actor"),
                ProviderActivityMutation::UpsertWorkItem(
                    work_item_summary(
                        "work:entry-owner",
                        Some("actor:entry-owner"),
                        ActivityLifecycle::Running,
                        "2026-07-22T12:00:00Z",
                        "2026-07-22T12:00:00Z",
                        None,
                    )
                    .expect("work"),
                ),
                ProviderActivityMutation::AppendEntry(
                    entry(
                        "entry:actor",
                        ActivityRecordKind::Actor,
                        "actor:entry-only",
                        "2026-07-22T12:00:01Z",
                    )
                    .expect("actor entry"),
                ),
                ProviderActivityMutation::AppendEntry(
                    entry(
                        "entry:work",
                        ActivityRecordKind::WorkItem,
                        "work:entry-owner",
                        "2026-07-22T12:00:01Z",
                    )
                    .expect("work entry"),
                ),
            ],
            "2026-07-22T12:00:01Z",
        )
        .await
        .expect("records");

    for (event_key, mutation) in [
        (
            "event:delete-actor-with-entry",
            ProviderActivityMutation::remove_actor("actor:entry-only").expect("actor id"),
        ),
        (
            "event:delete-work-with-entry",
            ProviderActivityMutation::remove_work_item("work:entry-owner").expect("work id"),
        ),
    ] {
        assert!(matches!(
            repository
                .apply_batch(
                    &scope.scope_id,
                    event_key,
                    vec![mutation],
                    "2026-07-22T12:00:02Z",
                )
                .await,
            Err(ActivityRepositoryError::InvalidReference(_))
        ));
    }
}

#[tokio::test]
async fn net_noop_scope_batches_do_not_consume_revision_or_event_key() {
    let database = migrated_database().await;
    let repository = ActivityRepository::new(database.clone());
    let scope = thread_scope("thread:scope-noop", "scope-noop");
    repository.ensure_scope(scope.clone()).await.expect("scope");
    let stale = ActivitySectionHealth::try_stale("temporary", true).expect("stale");

    assert!(
        repository
            .apply_batch(
                &scope.scope_id,
                "event:scope-noop",
                vec![
                    ProviderActivityMutation::SetSectionHealth {
                        section: ActivitySection::Subagents,
                        health: stale.clone(),
                    },
                    ProviderActivityMutation::SetSectionHealth {
                        section: ActivitySection::Subagents,
                        health: ActivitySectionHealth::live(),
                    },
                ],
                "2026-07-22T12:00:00Z",
            )
            .await
            .expect("net no-op")
            .is_empty()
    );
    let (revision, journal_count) = database
        .call(|connection| {
            Ok((
                connection.query_row(
                    "SELECT revision FROM activity_scopes WHERE scope_id = 'thread:scope-noop'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?,
                connection.query_row(
                    "SELECT COUNT(*) FROM activity_journal
                     WHERE scope_id = 'thread:scope-noop'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?,
            ))
        })
        .await
        .expect("persisted state");
    assert_eq!((revision, journal_count), (0, 0));

    let effective = repository
        .apply_batch(
            &scope.scope_id,
            "event:scope-noop",
            vec![
                ProviderActivityMutation::SetSectionHealth {
                    section: ActivitySection::Subagents,
                    health: stale,
                },
                ProviderActivityMutation::SetScope {
                    capabilities: scope.capabilities.clone(),
                    observation_state: ActivityObservationState::Reconnecting,
                },
            ],
            "2026-07-22T12:00:01Z",
        )
        .await
        .expect("reused key")
        .into_iter()
        .next()
        .expect("effective scope delta");
    assert_eq!(effective.changes.len(), 1);
}

#[tokio::test]
async fn cursor_payload_structure_is_strict_for_roster_and_detail() {
    let timeline_start = recent_activity_anchor();
    let fixture_at = activity_timestamp(timeline_start, Duration::seconds(1));
    let database = migrated_database().await;
    let repository = ActivityRepository::new(database);
    let scope = thread_scope("thread:strict-cursor", "strict-cursor");
    repository.ensure_scope(scope.clone()).await.expect("scope");
    repository
        .apply_batch(
            &scope.scope_id,
            "event:strict-cursor",
            vec![
                ProviderActivityMutation::upsert_actor("actor:cursor", None, "Cursor", "completed")
                    .expect("actor"),
                ProviderActivityMutation::AppendEntry(
                    entry(
                        "entry:cursor",
                        ActivityRecordKind::Actor,
                        "actor:cursor",
                        &fixture_at,
                    )
                    .expect("entry"),
                ),
            ],
            &fixture_at,
        )
        .await
        .expect("fixture");

    for payload in [
        json!({ "updatedAt": "2026-07-22T12:00:01Z", "recordId": " actor:cursor " }),
        json!({ "updatedAt": "not-a-time", "recordId": "actor:cursor" }),
        json!({ "updatedAt": "2026-07-22T12:00:01Z" }),
        json!({
            "updatedAt": "2026-07-22T12:00:01Z",
            "recordId": "actor:cursor",
            "extra": true
        }),
    ] {
        let cursor = encode_test_cursor(payload);
        assert!(matches!(
            repository
                .list_roster(
                    &scope.scope,
                    &scope.scope_id,
                    ActivitySection::Subagents,
                    ActivityRosterBucket::Done,
                    Some(&cursor),
                    10,
                )
                .await,
            Err(ActivityRepositoryError::InvalidCursor)
        ));
    }

    for payload in [
        json!({ "createdAt": "2026-07-22T12:00:01Z", "entryId": " entry:cursor " }),
        json!({ "createdAt": "not-a-time", "entryId": "entry:cursor" }),
        json!({ "createdAt": "2026-07-22T12:00:01Z" }),
        json!({
            "createdAt": "2026-07-22T12:00:01Z",
            "entryId": "entry:cursor",
            "extra": true
        }),
    ] {
        let cursor = encode_test_cursor(payload);
        assert!(matches!(
            repository
                .list_detail(
                    &scope.scope,
                    &scope.scope_id,
                    ActivityRecordKind::Actor,
                    "actor:cursor",
                    Some(&cursor),
                    10,
                )
                .await,
            Err(ActivityRepositoryError::InvalidCursor)
        ));
    }
}

#[tokio::test]
async fn roster_pages_enforce_the_two_hundred_row_cap() {
    let timeline_start = recent_activity_anchor();
    let completed_at = activity_timestamp(timeline_start, Duration::ZERO);
    let database = migrated_database().await;
    let repository = ActivityRepository::new(database);
    let scope = thread_scope("thread:page-cap", "page-cap");
    repository.ensure_scope(scope.clone()).await.expect("scope");
    repository
        .apply_batch(
            &scope.scope_id,
            "event:page-cap",
            (0..205)
                .map(|index| {
                    ProviderActivityMutation::upsert_actor(
                        format!("actor:{index:03}"),
                        None,
                        format!("Actor {index}"),
                        "completed",
                    )
                    .expect("actor")
                })
                .collect(),
            &completed_at,
        )
        .await
        .expect("actors");

    let first = repository
        .list_roster(
            &scope.scope,
            &scope.scope_id,
            ActivitySection::Subagents,
            ActivityRosterBucket::Done,
            None,
            500,
        )
        .await
        .expect("first page");
    assert_eq!(first.records.len(), 200);
    let cursor = first.next_cursor.expect("next cursor");
    let second = repository
        .list_roster(
            &scope.scope,
            &scope.scope_id,
            ActivitySection::Subagents,
            ActivityRosterBucket::Done,
            Some(&cursor),
            500,
        )
        .await
        .expect("second page");
    assert_eq!(second.records.len(), 5);
    assert!(second.next_cursor.is_none());
}

#[tokio::test]
async fn retention_prunes_old_completed_records_and_entries_but_preserves_live_lineage() {
    let database = migrated_database().await;
    let repository = ActivityRepository::new(database.clone());
    let projection = ActivityProjection::new(repository);
    let scope = thread_scope("thread:retention", "retention");
    projection.ensure_scope(scope.clone()).await.expect("scope");

    projection
        .apply(
            &scope.scope_id,
            "event:retention-parent".to_owned(),
            vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:completed-parent",
                    None,
                    "Completed parent",
                    "completed",
                )
                .expect("completed parent"),
                ProviderActivityMutation::upsert_actor(
                    "actor:active-child",
                    Some("actor:completed-parent"),
                    "Active child",
                    "running",
                )
                .expect("active child"),
            ],
            "2099-01-01T00:00:00Z".to_owned(),
        )
        .await
        .expect("lineage");

    projection
        .apply(
            &scope.scope_id,
            "event:retention-expired".to_owned(),
            vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:expired",
                    None,
                    "Expired actor",
                    "completed",
                )
                .expect("expired actor"),
            ],
            "2020-01-01T00:00:00Z".to_owned(),
        )
        .await
        .expect("expired record");

    for (batch, mutations) in (0..2_000)
        .map(|index| {
            ProviderActivityMutation::upsert_actor(
                format!("actor:completed:{index:04}"),
                None,
                format!("Completed {index}"),
                "completed",
            )
            .expect("completed actor")
        })
        .collect::<Vec<_>>()
        .chunks(MAX_ACTIVITY_MUTATIONS)
        .enumerate()
    {
        projection
            .apply(
                &scope.scope_id,
                format!("event:retention-records:{batch}"),
                mutations.to_vec(),
                "2099-01-01T00:00:00Z".to_owned(),
            )
            .await
            .expect("completed records");
    }

    projection
        .apply(
            &scope.scope_id,
            "event:retention-entries".to_owned(),
            (0..201)
                .map(|index| {
                    entry(
                        &format!("entry:retention:{index:03}"),
                        ActivityRecordKind::Actor,
                        "actor:active-child",
                        &format!("2020-01-01T00:{:02}:{:02}Z", index / 60, index % 60),
                    )
                    .map(ProviderActivityMutation::AppendEntry)
                    .expect("entry")
                })
                .collect(),
            "2020-01-01T00:03:20Z".to_owned(),
        )
        .await
        .expect("entries");

    wait_for_retention_caps(&database, &scope.scope_id).await;

    let (record_count, parent_retained, active_child_retained, oldest_completed_retained, expired_record_retained, entry_count, oldest_entry_retained) = database
        .call(move |connection| {
            Ok((
                connection.query_row(
                    "SELECT COUNT(*) FROM activity_records WHERE scope_id = ?",
                    [&scope.scope_id],
                    |row| row.get::<_, i64>(0),
                )?,
                connection.query_row(
                    "SELECT EXISTS(
                       SELECT 1 FROM activity_records
                       WHERE scope_id = ? AND record_kind = 'actor' AND record_id = 'actor:completed-parent'
                     )",
                    [&scope.scope_id],
                    |row| row.get::<_, bool>(0),
                )?,
                connection.query_row(
                    "SELECT EXISTS(
                       SELECT 1 FROM activity_records
                       WHERE scope_id = ? AND record_kind = 'actor' AND record_id = 'actor:active-child'
                     )",
                    [&scope.scope_id],
                    |row| row.get::<_, bool>(0),
                )?,
                connection.query_row(
                    "SELECT EXISTS(
                       SELECT 1 FROM activity_records
                       WHERE scope_id = ? AND record_kind = 'actor' AND record_id = 'actor:completed:0000'
                     )",
                    [&scope.scope_id],
                    |row| row.get::<_, bool>(0),
                )?,
                connection.query_row(
                    "SELECT EXISTS(
                       SELECT 1 FROM activity_records
                       WHERE scope_id = ? AND record_kind = 'actor' AND record_id = 'actor:expired'
                     )",
                    [&scope.scope_id],
                    |row| row.get::<_, bool>(0),
                )?,
                connection.query_row(
                    "SELECT COUNT(*) FROM activity_entries
                     WHERE scope_id = ? AND owner_kind = 'actor' AND owner_id = 'actor:active-child'",
                    [&scope.scope_id],
                    |row| row.get::<_, i64>(0),
                )?,
                connection.query_row(
                    "SELECT EXISTS(
                       SELECT 1 FROM activity_entries
                       WHERE scope_id = ? AND entry_id = 'entry:retention:000'
                     )",
                    [&scope.scope_id],
                    |row| row.get::<_, bool>(0),
                )?,
            ))
        })
        .await
        .expect("retention inspection");

    assert_eq!(record_count, 2_000);
    assert!(
        parent_retained,
        "the active child still references its completed parent"
    );
    assert!(
        active_child_retained,
        "active records are never eligible for retention"
    );
    assert!(
        !oldest_completed_retained,
        "the oldest eligible completed record is pruned first"
    );
    assert!(
        !expired_record_retained,
        "completed records older than thirty days are pruned"
    );
    assert_eq!(entry_count, 200);
    assert!(!oldest_entry_retained, "the oldest entry is pruned first");
}

#[tokio::test]
async fn large_activity_projection_keeps_pages_and_published_deltas_bounded() {
    const ACTOR_AND_WORK_ITEM_COUNT: usize = 10_000;
    const ENTRY_COUNT: usize = 1_000;

    let database = migrated_database().await;
    let projection =
        ActivityProjection::with_capacity(ActivityRepository::new(database.clone()), 512);
    let scope = thread_scope("thread:large-projection", "large-projection");
    projection.ensure_scope(scope.clone()).await.expect("scope");
    let mut emitted_deltas = Vec::new();

    for (batch, mutations) in (0..ACTOR_AND_WORK_ITEM_COUNT)
        .map(|index| {
            ProviderActivityMutation::upsert_actor(
                format!("actor:load:{index:05}"),
                None,
                format!("Actor {index}"),
                "running",
            )
            .expect("actor")
        })
        .collect::<Vec<_>>()
        .chunks(MAX_ACTIVITY_MUTATIONS)
        .enumerate()
    {
        emitted_deltas.extend(
            projection
                .apply(
                    &scope.scope_id,
                    format!("event:large-actors:{batch}"),
                    mutations.to_vec(),
                    "2026-07-22T12:00:00Z".to_owned(),
                )
                .await
                .expect("actor batch"),
        );
    }

    for (batch, mutations) in (0..ACTOR_AND_WORK_ITEM_COUNT)
        .map(|index| {
            ProviderActivityMutation::UpsertWorkItem(
                work_item_summary(
                    &format!("work:load:{index:05}"),
                    Some(&format!("actor:load:{index:05}")),
                    ActivityLifecycle::Running,
                    "2026-07-22T12:00:00Z",
                    "2026-07-22T12:00:00Z",
                    None,
                )
                .expect("work item"),
            )
        })
        .collect::<Vec<_>>()
        .chunks(MAX_ACTIVITY_MUTATIONS)
        .enumerate()
    {
        emitted_deltas.extend(
            projection
                .apply(
                    &scope.scope_id,
                    format!("event:large-work-items:{batch}"),
                    mutations.to_vec(),
                    "2026-07-22T12:00:01Z".to_owned(),
                )
                .await
                .expect("work-item batch"),
        );
    }

    for (batch, mutations) in (0..ENTRY_COUNT)
        .map(|index| {
            entry(
                &format!("entry:load:{index:05}"),
                ActivityRecordKind::Actor,
                "actor:load:00000",
                "2026-07-22T12:00:02Z",
            )
            .map(ProviderActivityMutation::AppendEntry)
            .expect("entry")
        })
        .collect::<Vec<_>>()
        .chunks(MAX_ACTIVITY_MUTATIONS)
        .enumerate()
    {
        emitted_deltas.extend(
            projection
                .apply(
                    &scope.scope_id,
                    format!("event:large-entries:{batch}"),
                    mutations.to_vec(),
                    "2026-07-22T12:00:02Z".to_owned(),
                )
                .await
                .expect("entry batch"),
        );
    }
    wait_for_entry_retention_cap(&database, &scope.scope_id).await;

    assert!(
        emitted_deltas
            .iter()
            .all(|delta| delta.changes.len() <= MAX_ACTIVITY_MUTATIONS),
        "every emitted delta remains within the wire mutation bound"
    );
    assert!(
        emitted_deltas
            .iter()
            .flat_map(|delta| &delta.changes)
            .any(|change| matches!(change, ActivityChange::EntriesReplaced { .. })),
        "entry retention must publish replacement semantics instead of stale appends"
    );

    let snapshot = projection
        .snapshot(&ActivityScopeRef::Thread {
            thread_id: "large-projection".to_owned(),
        })
        .await
        .expect("snapshot");
    assert_eq!(snapshot.actors.len(), 200);
    assert!(snapshot.actors_has_more);
    assert_eq!(snapshot.control.actors.len(), 200);
    assert!(
        snapshot
            .control
            .actors
            .iter()
            .all(|control| control.state == ActivityActorControlState::Unsupported)
    );
    assert_eq!(snapshot.work_items.len(), 200);
    assert!(snapshot.work_items_has_more);

    let roster = projection
        .list_roster(
            &scope.scope,
            &scope.scope_id,
            ActivitySection::BackgroundTasks,
            ActivityRosterBucket::Active,
            None,
            500,
        )
        .await
        .expect("bounded roster");
    assert_eq!(roster.records.len(), 200);
    assert!(roster.next_cursor.is_some());
    let detail = projection
        .list_detail(
            &scope.scope,
            &scope.scope_id,
            ActivityRecordKind::Actor,
            "actor:load:00000",
            None,
            500,
        )
        .await
        .expect("bounded detail");
    assert_eq!(detail.entries.len(), 200);
    assert_eq!(
        detail
            .actor_control
            .expect("historical actor control")
            .state,
        ActivityActorControlState::Unsupported
    );
    assert!(detail.next_cursor.is_none());
}

#[tokio::test]
async fn retention_keeps_only_the_latest_five_thousand_journal_rows_per_scope() {
    let database = migrated_database().await;
    let repository = ActivityRepository::new(database.clone());
    let scope = thread_scope("thread:retention-journal", "retention-journal");
    repository.ensure_scope(scope.clone()).await.expect("scope");

    let scope_id = scope.scope_id.clone();
    database
        .call(move |connection| {
            let transaction = connection.transaction()?;
            for revision in 1..=5_000 {
                transaction.execute(
                    "INSERT INTO activity_journal (
                       scope_id, revision, event_key_namespace, native_event_key, delta_json, created_at
                     ) VALUES (?, ?, 'canonical', ?, '{}', '2020-01-01T00:00:00Z')",
                    rusqlite::params![
                        scope_id,
                        revision,
                        format!("event:retention-journal:{revision}"),
                    ],
                )?;
            }
            transaction.execute(
                "UPDATE activity_scopes SET revision = 5000 WHERE scope_id = ?",
                [&scope_id],
            )?;
            transaction.commit()?;
            Ok(())
        })
        .await
        .expect("journal fixture");

    repository
        .apply_batch(
            &scope.scope_id,
            "event:retention-journal-current",
            vec![
                ProviderActivityMutation::upsert_actor("actor:journal", None, "Journal", "running")
                    .expect("actor"),
            ],
            "2099-01-01T00:00:00Z",
        )
        .await
        .expect("current batch");

    let scope_id = scope.scope_id.clone();
    let (journal_count, oldest_revision) = database
        .call(move |connection| {
            Ok((
                connection.query_row(
                    "SELECT COUNT(*) FROM activity_journal WHERE scope_id = ?",
                    [&scope_id],
                    |row| row.get::<_, i64>(0),
                )?,
                connection.query_row(
                    "SELECT MIN(revision) FROM activity_journal WHERE scope_id = ?",
                    [&scope_id],
                    |row| row.get::<_, i64>(0),
                )?,
            ))
        })
        .await
        .expect("journal inspection");

    assert_eq!(journal_count, 5_000);
    assert_eq!(oldest_revision, 2);
}

#[tokio::test]
async fn entry_retention_replaces_detail_state_instead_of_emitting_evicted_appends() {
    let database = migrated_database().await;
    let repository = ActivityRepository::new(database.clone());
    let scope = thread_scope("thread:entry-reset", "entry-reset");
    repository.ensure_scope(scope.clone()).await.expect("scope");
    repository
        .apply_batch(
            &scope.scope_id,
            "event:entry-owner",
            vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:entry-owner",
                    None,
                    "Entry owner",
                    "running",
                )
                .expect("owner"),
            ],
            "2026-07-22T12:00:00Z",
        )
        .await
        .expect("owner mutation");

    let deltas = repository
        .apply_batch(
            &scope.scope_id,
            "event:entry-overflow",
            (0..201)
                .map(|index| {
                    entry(
                        &format!("entry:reset:{index:03}"),
                        ActivityRecordKind::Actor,
                        "actor:entry-owner",
                        &format!("2026-07-22T12:{:02}:{:02}Z", index / 60, index % 60),
                    )
                    .map(ProviderActivityMutation::AppendEntry)
                    .expect("entry")
                })
                .collect(),
            "2026-07-22T12:03:20Z",
        )
        .await
        .expect("entry overflow");
    let serialized = serde_json::to_value(&deltas).expect("serialized deltas");
    let changes = serialized
        .as_array()
        .expect("deltas")
        .iter()
        .flat_map(|delta| delta["changes"].as_array().expect("changes"))
        .collect::<Vec<_>>();
    assert!(
        changes
            .iter()
            .any(|change| change["kind"] == "entries-replaced"),
        "clients must receive an authoritative detail reset when retention evicts entries"
    );
    assert!(
        changes
            .iter()
            .all(|change| change["kind"] != "entry-appended"),
        "an entry append delta cannot survive when its detail page was reset"
    );
    let page = repository
        .list_detail(
            &scope.scope,
            &scope.scope_id,
            ActivityRecordKind::Actor,
            "actor:entry-owner",
            None,
            201,
        )
        .await
        .expect("retained detail");
    assert_eq!(page.entries.len(), 200);
    assert_eq!(page.entries[0].id, "entry:reset:200");
}

#[tokio::test]
async fn entry_retention_progresses_at_128_200_201_and_converges_after_multiple_passes() {
    let database = migrated_database().await;
    let projection = ActivityProjection::new(ActivityRepository::new(database.clone()));
    let scope = thread_scope(
        "thread:entry-retention-boundaries",
        "entry-retention-boundaries",
    );
    projection.ensure_scope(scope.clone()).await.expect("scope");
    projection
        .apply(
            &scope.scope_id,
            "event:entry-owner".to_owned(),
            vec![
                ProviderActivityMutation::upsert_actor(
                    "actor:entry-boundary",
                    None,
                    "Entry boundary owner",
                    "running",
                )
                .expect("owner"),
            ],
            "2026-07-22T12:00:00Z".to_owned(),
        )
        .await
        .expect("owner");

    for (event, start, end) in [("event:entry-128", 0, 128), ("event:entry-200", 128, 200)] {
        let deltas = projection
            .apply(
                &scope.scope_id,
                event.to_owned(),
                (start..end)
                    .map(|index| {
                        entry(
                            &format!("entry:boundary:{index:03}"),
                            ActivityRecordKind::Actor,
                            "actor:entry-boundary",
                            &format!("2026-07-22T12:{:02}:{:02}Z", index / 60, index % 60),
                        )
                        .map(ProviderActivityMutation::AppendEntry)
                        .expect("entry")
                    })
                    .collect(),
                "2026-07-22T12:00:00Z".to_owned(),
            )
            .await
            .expect("bounded entries");
        assert!(
            deltas
                .iter()
                .flat_map(|delta| &delta.changes)
                .all(|change| !matches!(change, ActivityChange::EntriesReplaced { .. }))
        );
    }

    let overflow = projection
        .apply(
            &scope.scope_id,
            "event:entry-201".to_owned(),
            vec![
                entry(
                    "entry:boundary:200",
                    ActivityRecordKind::Actor,
                    "actor:entry-boundary",
                    "2026-07-22T12:03:20Z",
                )
                .map(ProviderActivityMutation::AppendEntry)
                .expect("overflow entry"),
            ],
            "2026-07-22T12:03:20Z".to_owned(),
        )
        .await
        .expect("201st entry");
    assert!(overflow.iter().flat_map(|delta| &delta.changes).any(
        |change| matches!(change, ActivityChange::EntriesReplaced { owner_id, .. } if owner_id == "actor:entry-boundary"),
    ));

    let capped = projection
        .list_detail(
            &scope.scope,
            &scope.scope_id,
            ActivityRecordKind::Actor,
            "actor:entry-boundary",
            None,
            200,
        )
        .await
        .expect("capped detail");
    assert_eq!(capped.entries.len(), 200);

    projection
        .apply(
            &scope.scope_id,
            "event:entry-multipass".to_owned(),
            (201..457)
                .map(|index| {
                    entry(
                        &format!("entry:boundary:{index:03}"),
                        ActivityRecordKind::Actor,
                        "actor:entry-boundary",
                        &format!("2026-07-22T12:{:02}:{:02}Z", index / 60, index % 60),
                    )
                    .map(ProviderActivityMutation::AppendEntry)
                    .expect("multi-pass entry")
                })
                .collect(),
            "2026-07-22T12:10:00Z".to_owned(),
        )
        .await
        .expect("multi-pass entry batch");
    wait_for_entry_retention_cap(&database, &scope.scope_id).await;
    let converged = projection
        .list_detail(
            &scope.scope,
            &scope.scope_id,
            ActivityRecordKind::Actor,
            "actor:entry-boundary",
            None,
            200,
        )
        .await
        .expect("converged detail");
    assert_eq!(converged.entries.len(), 200);
}

#[tokio::test]
async fn journal_retention_keeps_multichunk_event_replay_idempotent_across_the_cutoff() {
    let database = migrated_database().await;
    let repository = ActivityRepository::new(database.clone());
    let scope = thread_scope("thread:journal-multichunk", "journal-multichunk");
    repository.ensure_scope(scope.clone()).await.expect("scope");
    let split_mutations = actor_mutations(MAX_ACTIVITY_MUTATIONS);
    let split_deltas = repository
        .apply_batch(
            &scope.scope_id,
            "event:journal-split",
            split_mutations.clone(),
            "2026-07-22T12:00:00Z",
        )
        .await
        .expect("split event");
    assert_eq!(split_deltas.len(), 2, "fixture must have two chunks");

    let scope_id = scope.scope_id.clone();
    database
        .call(move |connection| {
            let transaction = connection.transaction()?;
            for revision in 3..=4_999 {
                transaction.execute(
                    "INSERT INTO activity_journal (
                       scope_id, revision, event_key_namespace, native_event_key, delta_json, created_at
                     ) VALUES (?, ?, 'legacy', ?, '{}', '2020-01-01T00:00:00Z')",
                    rusqlite::params![
                        scope_id,
                        revision,
                        format!("event:journal-filler:{revision}"),
                    ],
                )?;
            }
            transaction.execute(
                "UPDATE activity_scopes SET revision = 4999 WHERE scope_id = ?",
                [&scope_id],
            )?;
            transaction.commit()?;
            Ok(())
        })
        .await
        .expect("journal cutoff fixture");

    repository
        .apply_batch(
            &scope.scope_id,
            "event:journal-after-cutoff",
            (0..MAX_ACTIVITY_MUTATIONS)
                .map(|index| {
                    ProviderActivityMutation::upsert_actor(
                        format!("actor:after-cutoff:{index}"),
                        None,
                        format!("After cutoff {index}"),
                        "running",
                    )
                    .expect("actor")
                })
                .collect(),
            "2026-07-22T12:01:00Z",
        )
        .await
        .expect("cutoff event");

    assert!(
        repository
            .apply_batch(
                &scope.scope_id,
                "event:journal-split",
                vec![
                    ProviderActivityMutation::remove_actor("actor:after-cutoff:0")
                        .expect("duplicate payload"),
                ],
                "2026-07-22T12:00:00Z",
            )
            .await
            .expect("replay must remain idempotent")
            .is_empty(),
        "retention must not retain a trailing chunk without its event identity"
    );
}

#[tokio::test]
async fn activity_projection_normalizes_terminal_text_and_redacts_native_secrets() {
    let database = migrated_database().await;
    let repository = ActivityRepository::new(database);
    let scope = thread_scope("thread:unsafe-text", "unsafe-text");
    repository.ensure_scope(scope.clone()).await.expect("scope");

    repository
        .apply_batch(
            &scope.scope_id,
            "event:unsafe-text",
            vec![
                ProviderActivityMutation::UpsertActor(
                    ActivityActorSummary::try_new(
                        "actor:unsafe-text",
                        None,
                        "\u{1b}[31m<script>untrusted</script>",
                        Some("Cookie: cookie-private"),
                        None,
                        ActivityLifecycle::Running,
                        Some("Bearer bearer-private\nAPI_KEY=api-private"),
                        "2099-01-01T00:00:00Z",
                        "2099-01-01T00:00:00Z",
                        None,
                    )
                    .expect("unsafe actor"),
                ),
                ProviderActivityMutation::AppendEntry(
                    ActivityEntry::try_new(
                        "entry:unsafe-text",
                        ActivityRecordKind::Actor,
                        "actor:unsafe-text",
                        ActivityEntryKind::Commentary,
                        "hook https://hooks.example/activity?token=hook-private",
                        Some(
                            "ENV_SECRET=environment-private\n{\"apiKey\":\"json-api-private\"}\n\u{1b}]8;;https://example.invalid\u{7}unsafe",
                        ),
                        ActivityEntryTone::Info,
                        "2099-01-01T00:00:00Z",
                    )
                    .expect("unsafe entry"),
                ),
                ProviderActivityMutation::AppendEntry(
                    ActivityEntry::try_new(
                        "entry:unsafe-separators",
                        ActivityRecordKind::Actor,
                        "actor:unsafe-text",
                        ActivityEntryKind::Commentary,
                        "separator redaction coverage",
                        Some(&format!(
                            "Authorization \t : \t Bearer whitespace-private\nCookie \t : \t cookie-spacing-private\nAWS_ACCESS_KEY_ID = aws-key-private\n{{\"apiKey\" : \"camel-json-private\"}}\n{{\\\"apiKey\\\"\t:\t\\\"escaped-json-private\\\"}}\n{}\"apiKey\":\"deep-json-private\"{}\n{}AWS_ACCESS_KEY_ID=boundary-private",
                            "[".repeat(1_000),
                            "]".repeat(1_000),
                            "🦀".repeat(7_000),
                        )),
                        ActivityEntryTone::Info,
                        "2099-01-01T00:00:01Z",
                    )
                    .expect("separator entry"),
                ),
                ProviderActivityMutation::AppendEntry(
                    ActivityEntry::try_new(
                        "entry:unsafe-false-positive",
                        ActivityRecordKind::Actor,
                        "actor:unsafe-text",
                        ActivityEntryKind::Commentary,
                        "monkey: harmless activity text",
                        Some("CACHE_KEY=public-cache-value"),
                        ActivityEntryTone::Info,
                        "2099-01-01T00:00:02Z",
                    )
                    .expect("non-secret entry"),
                ),
            ],
            "2099-01-01T00:00:00Z",
        )
        .await
        .expect("unsafe mutation");

    let snapshot = repository
        .snapshot(&ActivityScopeRef::Thread {
            thread_id: "unsafe-text".to_owned(),
        })
        .await
        .expect("snapshot");
    let detail = repository
        .list_detail(
            &scope.scope,
            &scope.scope_id,
            ActivityRecordKind::Actor,
            "actor:unsafe-text",
            None,
            10,
        )
        .await
        .expect("detail");
    let wire = serde_json::to_string(&(snapshot, detail)).expect("activity wire");

    for secret in [
        "bearer-private",
        "api-private",
        "cookie-private",
        "hook-private",
        "environment-private",
        "json-api-private",
        "whitespace-private",
        "cookie-spacing-private",
        "aws-key-private",
        "camel-json-private",
        "escaped-json-private",
        "deep-json-private",
        "boundary-private",
    ] {
        assert!(!wire.contains(secret), "activity wire leaked {secret}");
    }
    assert!(
        !wire.contains('\u{1b}'),
        "terminal controls remain active in activity text"
    );
    assert!(wire.contains("[REDACTED]"));
    assert!(wire.contains("monkey: harmless activity text"));
    assert!(wire.contains("CACHE_KEY=public-cache-value"));
}

#[tokio::test]
async fn activity_text_utf8_utf16_boundaries_remain_valid_and_bounded_in_storage_and_deltas() {
    let database = migrated_database().await;
    let repository = ActivityRepository::new(database);
    let scope = thread_scope("thread:text-boundaries", "text-boundaries");
    repository.ensure_scope(scope.clone()).await.expect("scope");
    let cases = vec![
        ("ascii-before", "a".repeat(2_047), "a".repeat(2_047)),
        ("ascii-at", "a".repeat(2_048), "a".repeat(2_048)),
        ("ascii-after", "a".repeat(2_049), "a".repeat(2_048)),
        ("two-byte-before", "é".repeat(2_047), "é".repeat(2_047)),
        ("two-byte-at", "é".repeat(2_048), "é".repeat(2_048)),
        ("two-byte-after", "é".repeat(2_049), "é".repeat(2_048)),
        ("three-byte-before", "€".repeat(2_047), "€".repeat(2_047)),
        ("three-byte-at", "€".repeat(2_048), "€".repeat(2_048)),
        ("three-byte-after", "€".repeat(2_049), "€".repeat(2_048)),
        ("pair-before", "🦀".repeat(1_023), "🦀".repeat(1_023)),
        ("pair-at", "🦀".repeat(1_024), "🦀".repeat(1_024)),
        ("pair-after", "🦀".repeat(1_025), "🦀".repeat(1_024)),
        (
            "mixed-before",
            format!("{}é€", "a".repeat(2_045)),
            format!("{}é€", "a".repeat(2_045)),
        ),
        (
            "mixed-at-pair",
            format!("{}🦀", "a".repeat(2_046)),
            format!("{}🦀", "a".repeat(2_046)),
        ),
        (
            "mixed-after-pair",
            format!("{}🦀", "a".repeat(2_047)),
            "a".repeat(2_047),
        ),
        (
            "redacted-before-store",
            format!(
                "Authorization: Bearer boundary-secret {}",
                "🦀".repeat(1_025)
            ),
            "Authorization: [REDACTED]".to_owned(),
        ),
    ];
    let deltas = repository
        .apply_batch(
            &scope.scope_id,
            "event:text-boundaries",
            cases
                .iter()
                .map(|(id, summary, _)| {
                    let mut actor = ActivityActorSummary::try_new(
                        format!("actor:{id}"),
                        None,
                        *id,
                        None,
                        None,
                        ActivityLifecycle::Running,
                        None,
                        "2026-07-22T12:00:00Z",
                        "2026-07-22T12:00:00Z",
                        None,
                    )
                    .expect("boundary actor");
                    actor.summary = Some(summary.clone());
                    ProviderActivityMutation::UpsertActor(actor)
                })
                .collect(),
            "2026-07-22T12:00:00Z",
        )
        .await
        .expect("boundary batch");
    let snapshot = repository.snapshot(&scope.scope).await.expect("snapshot");
    let emitted = deltas
        .iter()
        .flat_map(|delta| &delta.changes)
        .filter_map(|change| match change {
            ActivityChange::ActorUpserted { actor } => actor
                .summary
                .as_ref()
                .map(|summary| (actor.id.clone(), summary.clone())),
            _ => None,
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let stored = snapshot
        .actors
        .iter()
        .filter_map(|actor| {
            actor
                .summary
                .as_ref()
                .map(|summary| (actor.id.clone(), summary.clone()))
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let expected = cases
        .iter()
        .map(|(id, _, expected)| (format!("actor:{id}"), expected.clone()))
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(emitted.len(), cases.len());
    assert_eq!(stored.len(), cases.len());
    assert_eq!(emitted, expected);
    assert_eq!(stored, expected);
    for value in expected.values() {
        assert!(value.is_char_boundary(value.len()));
        assert!(value.encode_utf16().count() <= 2_048);
        assert!(std::str::from_utf8(value.as_bytes()).is_ok());
    }
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

async fn wait_for_retention_caps(database: &Database, scope_id: &str) {
    for _ in 0..200 {
        let scope_id = scope_id.to_owned();
        let retention = database
            .call(move |connection| {
                Ok((
                    connection.query_row(
                        "SELECT COUNT(*) FROM activity_records WHERE scope_id = ?",
                        [&scope_id],
                        |row| row.get::<_, i64>(0),
                    )?,
                    connection.query_row(
                        "SELECT COUNT(*) <= 2000 FROM activity_records WHERE scope_id = ?",
                        [&scope_id],
                        |row| row.get::<_, bool>(0),
                    )?,
                    connection.query_row(
                        "SELECT COALESCE(MAX(entry_count), 0) <= 200
                         FROM (
                           SELECT COUNT(*) AS entry_count
                           FROM activity_entries
                           WHERE scope_id = ?
                           GROUP BY owner_kind, owner_id
                         )",
                        [&scope_id],
                        |row| row.get::<_, bool>(0),
                    )?,
                ))
            })
            .await
            .expect("retention convergence inspection");
        if retention.1 && retention.2 {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("retention worker did not converge within two seconds");
}

async fn wait_for_entry_retention_cap(database: &Database, scope_id: &str) {
    for _ in 0..200 {
        let scope_id = scope_id.to_owned();
        let converged = database
            .call(move |connection| {
                Ok(connection.query_row(
                    "SELECT COALESCE(MAX(entry_count), 0) <= 200
                     FROM (
                       SELECT COUNT(*) AS entry_count
                       FROM activity_entries
                       WHERE scope_id = ?
                       GROUP BY owner_kind, owner_id
                     )",
                    [&scope_id],
                    |row| row.get::<_, bool>(0),
                )?)
            })
            .await
            .expect("entry retention convergence inspection");
        if converged {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("entry retention worker did not converge within two seconds");
}

async fn seed_running_actor(
    projection: &ActivityProjection,
    scope: &ActivityScopeSeed,
    actor_id: &str,
) {
    projection.ensure_scope(scope.clone()).await.expect("scope");
    projection
        .apply(
            &scope.scope_id,
            format!("event:{actor_id}"),
            vec![
                ProviderActivityMutation::upsert_actor(actor_id, None, actor_id, "running")
                    .expect("actor"),
            ],
            "2026-08-04T12:00:00Z".to_owned(),
        )
        .await
        .expect("running actor");
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
        "thread-terminal-boundaries",
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

fn assert_authoritative_subagent_counts(delta: &ActivityDelta, active: u64, done: u64) {
    let scope_updates = delta
        .changes
        .iter()
        .filter_map(|change| match change {
            ActivityChange::ScopeUpdated { counts, .. } => Some(counts),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(scope_updates.len(), 1);
    assert!(matches!(
        delta.changes.first(),
        Some(ActivityChange::ScopeUpdated { .. })
    ));
    assert_eq!(scope_updates[0].subagents.active, active);
    assert_eq!(scope_updates[0].subagents.done, done);
}

fn actor_mutations(count: usize) -> Vec<ProviderActivityMutation> {
    (0..count)
        .map(|index| {
            ProviderActivityMutation::upsert_actor(
                format!("actor:boundary:{index}"),
                None,
                format!("Boundary {index}"),
                "running",
            )
            .expect("actor")
        })
        .collect()
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
        id,
        None,
        ActivityEntryTone::Info,
        created_at,
    )
}

fn encode_test_cursor(payload: serde_json::Value) -> String {
    URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).expect("cursor JSON"))
}
