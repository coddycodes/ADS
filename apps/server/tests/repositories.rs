use coddy_studio_server::orchestration::TurnDeliveryState;
use coddy_studio_server::persistence::{
    AuthPairingLink, AuthSessionClient, AuthSessionDeliveryState, CheckpointDiffBlob,
    CommandReceipt, Database, EVENT_PAGE_SIZE, NewAuthPairingOffer, NewAuthSession,
    NewOrchestrationEvent, ProjectionCheckpoint, ProjectionPendingApproval,
    ProjectionPendingTurnStart, ProjectionProject, ProjectionState, ProjectionThread,
    ProjectionThreadActivity, ProjectionThreadMessage, ProjectionThreadProposedPlan,
    ProjectionThreadSession, ProjectionTurnById, ProviderSessionRuntime, Repositories,
    WorktreeRemovalReceipt, WorktreeRepositoryPinOutcome, run_migrations,
};
use std::collections::HashMap;

use serde::Serialize;
use serde_json::json;
use tempfile::TempDir;

const T0: &str = "2026-07-10T10:00:00.000Z";
const T1: &str = "2026-07-10T10:01:00.000Z";
const T2: &str = "2026-07-10T10:02:00.000Z";
const TIME_3: &str = "2026-07-10T10:03:00.000Z";
const FUTURE: &str = "2027-07-10T10:00:00.000Z";

async fn migrated_repositories() -> Repositories {
    let database = Database::open_in_memory()
        .await
        .expect("temporary SQLite database opens");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("all migrations apply");
    database.quick_check().await.expect("database is healthy");
    Repositories::new(database)
}

async fn migrated_repository_pair() -> (TempDir, Repositories, Repositories) {
    let temp = TempDir::new().expect("temporary SQLite directory");
    let path = temp.path().join("state.sqlite");
    let first = Database::create_new(&path)
        .await
        .expect("first SQLite connection opens");
    first
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("all migrations apply");
    let second = Database::open_existing(&path)
        .await
        .expect("second SQLite connection opens");
    (temp, Repositories::new(first), Repositories::new(second))
}

fn assert_row_eq<T: Serialize>(actual: &T, expected: &T) {
    assert_eq!(
        serde_json::to_value(actual).expect("actual row serializes"),
        serde_json::to_value(expected).expect("expected row serializes")
    );
}

fn project(id: &str, created_at: &str) -> ProjectionProject {
    ProjectionProject {
        project_id: id.to_owned(),
        title: format!("Project {id}"),
        workspace_root: format!("C:/work/{id}"),
        default_model_selection: Some(json!({
            "provider": "codex",
            "model": "gpt-5",
            "nested": { "reasoning": "high" }
        })),
        scripts: json!({"verify": "vp check\nvp run typecheck"}),
        worktree_discovery: json!({
            "visibility": "shown",
            "initialPromptDismissedAt": "2026-08-09T00:00:00.000Z",
            "baselinePaths": ["/workspace/project-a", "/workspace/project-a-feature"]
        }),
        worktree_repository_key: None,
        created_at: created_at.to_owned(),
        updated_at: created_at.to_owned(),
        deleted_at: None,
        repository_identity: None,
    }
}

#[tokio::test]
async fn project_repository_identity_round_trips() {
    let repositories = migrated_repositories().await;
    let identity = json!({
        "canonicalKey": "github.com/acme/repo",
        "locator": {"source": "git-remote", "remoteName": "origin", "remoteUrl": "git@github.com:acme/repo.git"},
        "name": "repo"
    });
    let mut row = project("p-identity", T0);
    row.repository_identity = Some(identity.clone());
    repositories.upsert_project(row).await.expect("upsert");
    let stored = repositories
        .get_project("p-identity".to_owned())
        .await
        .expect("read")
        .expect("project exists");
    assert_eq!(stored.repository_identity, Some(identity));
    assert_eq!(
        repositories.list_projects().await.expect("list")[0].repository_identity,
        stored.repository_identity
    );
}

#[tokio::test]
async fn worktree_repository_identity_pin_is_compare_and_set_and_cannot_be_replaced() {
    let repositories = migrated_repositories().await;
    repositories
        .upsert_project(project("project-pin", T0))
        .await
        .expect("unpinned project");

    assert_eq!(
        repositories
            .pin_project_worktree_repository_key(
                "project-pin".to_owned(),
                "repository-key-a".to_owned(),
            )
            .await
            .expect("establish pin"),
        Some(WorktreeRepositoryPinOutcome::Established)
    );
    let mut unrelated_projection_update = project("project-pin", T1);
    unrelated_projection_update.title = "Updated without identity metadata".to_owned();
    repositories
        .upsert_project(unrelated_projection_update)
        .await
        .expect("ordinary projection update preserves pin");
    assert_eq!(
        repositories
            .pin_project_worktree_repository_key(
                "project-pin".to_owned(),
                "repository-key-a".to_owned(),
            )
            .await
            .expect("match pin"),
        Some(WorktreeRepositoryPinOutcome::Matched)
    );
    assert_eq!(
        repositories
            .pin_project_worktree_repository_key(
                "project-pin".to_owned(),
                "repository-key-b".to_owned(),
            )
            .await
            .expect("reject replacement pin"),
        Some(WorktreeRepositoryPinOutcome::Mismatch {
            pinned_repository_key: "repository-key-a".to_owned(),
        })
    );
    let project = repositories
        .get_project("project-pin".to_owned())
        .await
        .expect("read pinned project")
        .expect("project exists");
    assert_eq!(
        project.worktree_repository_key.as_deref(),
        Some("repository-key-a")
    );
}

#[tokio::test]
async fn generic_project_upsert_cannot_establish_or_replace_repository_identity() {
    let repositories = migrated_repositories().await;
    let mut arbitrary = project("project-exclusive-pin", T0);
    arbitrary.worktree_repository_key = Some("repository-key-arbitrary".to_owned());
    repositories
        .upsert_project(arbitrary)
        .await
        .expect("generic insert ignores arbitrary identity");
    assert_eq!(
        repositories
            .get_project("project-exclusive-pin".to_owned())
            .await
            .expect("read unpinned project")
            .expect("project exists")
            .worktree_repository_key,
        None
    );

    assert_eq!(
        repositories
            .pin_project_worktree_repository_key(
                "project-exclusive-pin".to_owned(),
                "repository-key-trusted".to_owned(),
            )
            .await
            .expect("trusted operation establishes pin"),
        Some(WorktreeRepositoryPinOutcome::Established)
    );
    let mut replacement = project("project-exclusive-pin", T1);
    replacement.worktree_repository_key = Some("repository-key-replacement".to_owned());
    repositories
        .upsert_project(replacement)
        .await
        .expect("generic update ignores replacement identity");
    assert_eq!(
        repositories
            .get_project("project-exclusive-pin".to_owned())
            .await
            .expect("read pinned project")
            .expect("project exists")
            .worktree_repository_key
            .as_deref(),
        Some("repository-key-trusted")
    );
}

#[tokio::test]
async fn worktree_repository_identity_pin_persists_across_database_restart() {
    let root = tempfile::tempdir().expect("database directory");
    let path = root.path().join("catalog.sqlite3");
    let database = Database::create_new(&path).await.expect("database opens");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations apply");
    let repositories = Repositories::new(database);
    repositories
        .upsert_project(project("project-restart-pin", T0))
        .await
        .expect("unpinned project");
    assert_eq!(
        repositories
            .pin_project_worktree_repository_key(
                "project-restart-pin".to_owned(),
                "repository-key-durable".to_owned(),
            )
            .await
            .expect("establish pin"),
        Some(WorktreeRepositoryPinOutcome::Established)
    );
    drop(repositories);

    let reopened = Repositories::new(
        Database::open_existing(&path)
            .await
            .expect("database reopens"),
    );
    let project = reopened
        .get_project("project-restart-pin".to_owned())
        .await
        .expect("read project after restart")
        .expect("project exists after restart");

    assert_eq!(
        project.worktree_repository_key.as_deref(),
        Some("repository-key-durable")
    );
}

fn thread(id: &str, project_id: &str, created_at: &str) -> ProjectionThread {
    ProjectionThread {
        thread_id: id.to_owned(),
        project_id: project_id.to_owned(),
        title: format!("Thread {id}"),
        kind: "coding".to_owned(),
        model_selection: json!({"provider": "codex", "options": ["fast", "safe"]}),
        runtime_mode: "full-access".to_owned(),
        interaction_mode: "default".to_owned(),
        branch: Some(format!("codex/{id}")),
        worktree_path: Some(format!("C:/worktrees/{id}")),
        latest_turn_id: None,
        created_at: created_at.to_owned(),
        updated_at: created_at.to_owned(),
        archived_at: None,
        latest_user_message_at: None,
        pending_approval_count: 0,
        pending_user_input_count: 0,
        has_actionable_proposed_plan: 0,
        unresolved_delivery_state: None,
        unresolved_delivery_detail: None,
        deleted_at: None,
        host_thread_id: None,
    }
}

fn turn(id: &str, checkpoint_turn_count: Option<i64>) -> ProjectionTurnById {
    ProjectionTurnById {
        thread_id: "thread-turns".to_owned(),
        turn_id: id.to_owned(),
        pending_message_id: Some(format!("message-{id}")),
        source_proposed_plan_thread_id: None,
        source_proposed_plan_id: None,
        assistant_message_id: Some(format!("assistant-{id}")),
        state: "completed".to_owned(),
        requested_at: T1.to_owned(),
        started_at: Some(T2.to_owned()),
        completed_at: Some(TIME_3.to_owned()),
        checkpoint_turn_count,
        checkpoint_ref: checkpoint_turn_count.map(|count| format!("checkpoint-{count}")),
        checkpoint_status: checkpoint_turn_count.map(|_| "ready".to_owned()),
        checkpoint_files: json!([{"path": "src/main.rs", "status": "modified"}]),
    }
}

fn auth_client(label: &str) -> AuthSessionClient {
    AuthSessionClient {
        label: Some(label.to_owned()),
        ip_address: Some("127.0.0.1".to_owned()),
        user_agent: Some("repository-test/1.0".to_owned()),
        device_type: "desktop".to_owned(),
        os: Some("windows".to_owned()),
        browser: Some("webview2".to_owned()),
    }
}

#[test]
fn public_repository_api_inventory_is_explicit() {
    let source = include_str!("../src/persistence/repositories.rs");
    let mut methods = source
        .lines()
        .filter_map(|line| {
            let line = line.trim_start();
            line.strip_prefix("pub async fn ")
                .or_else(|| line.strip_prefix("pub fn "))
                .or_else(|| line.strip_prefix("pub const fn "))
                .and_then(|suffix| suffix.split('(').next())
                .map(str::to_owned)
        })
        .collect::<Vec<_>>();
    methods.sort();

    let mut expected = vec![
        "append_event",
        "event_pages",
        "auth_authority_revision",
        "can_promote_queued_provider_turn",
        "claim_provider_turn",
        "clear_checkpoint_turn_conflict",
        "consume_auth_pairing_link",
        "complete_auth_pairing_offer",
        "confirm_pending_auth_session",
        "count_active_work",
        "create_auth_pairing_link",
        "create_auth_pairing_link_with_offer",
        "create_auth_session",
        "database",
        "delete_activities_by_thread",
        "delete_checkpoints_by_thread",
        "delete_messages_by_thread",
        "delete_pending_approval",
        "delete_pending_turn_start",
        "delete_project",
        "delete_proposed_plans_by_thread",
        "delete_provider_session_runtime",
        "delete_thread",
        "delete_thread_session",
        "delete_turns_by_thread",
        "finalize_command_receipt",
        "freeze_provider_turn_session",
        "get_auth_pairing_link_by_credential",
        "get_auth_session",
        "get_checkpoint",
        "get_command_receipt",
        "get_message",
        "get_pending_approval",
        "get_pending_turn_start",
        "get_project",
        "get_projection_state",
        "get_provider_session_runtime",
        "get_provider_turn_delivery",
        "get_provider_turn_delivery_for_message",
        "get_thread",
        "get_thread_session",
        "get_turn_by_id",
        "get_worktree_removal_receipt",
        "list_active_auth_pairing_links",
        "list_active_auth_sessions",
        "list_activities_by_thread",
        "list_checkpoint_diff_blobs_by_thread",
        "list_checkpoints_by_thread",
        "list_messages_by_thread",
        "list_pending_approvals_by_thread",
        "list_projects",
        "list_projection_states",
        "list_proposed_plans_by_thread",
        "list_provider_session_runtimes",
        "list_provider_turn_deliveries",
        "list_queued_provider_turn_heads",
        "list_referenced_attachment_ids",
        "thread_attachment_digests",
        "list_thread_sessions_by_status",
        "list_threads_by_project",
        "load_shell_snapshot_rows",
        "load_worktree_catalog_projection",
        "list_turns_by_thread",
        "load_auth_authority_snapshot",
        "max_event_sequence",
        "next_page",
        "min_last_applied_sequence",
        "pin_project_worktree_repository_key",
        "prepare_reserved_command_receipt",
        "prune_and_list_active_auth_pairing_offers",
        "prune_and_get_active_auth_pairing_offer",
        "recover_pending_auth_pairing_offer",
        "new",
        "read_events_from_sequence",
        "release_reserved_command_receipt",
        "reserve_command_receipt",
        "replace_pending_provider_turn_payload",
        "requeue_provider_turn",
        "replace_pending_turn_start",
        "prepare_worktree_removal_receipt",
        "revoke_auth_pairing_link",
        "cancel_auth_pairing_offer",
        "revoke_auth_session",
        "revoke_pending_auth_session",
        "revoke_pending_auth_sessions",
        "revoke_other_auth_sessions",
        "set_auth_session_last_connected_at",
        "upsert_activity",
        "upsert_checkpoint",
        "upsert_checkpoint_diff_blob",
        "upsert_command_receipt",
        "upsert_message",
        "upsert_pending_approval",
        "upsert_project",
        "upsert_projection_state",
        "upsert_proposed_plan",
        "upsert_provider_session_runtime",
        "upsert_thread",
        "upsert_thread_session",
        "upsert_turn_by_id",
        "verify_prepared_command_receipt",
        "complete_worktree_removal_receipt",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect::<Vec<_>>();
    expected.sort();

    assert_eq!(methods, expected, "update repository execution coverage");
}

#[tokio::test]
async fn worktree_catalog_projection_read_is_consistent_filtered_and_bounded() {
    let repositories = migrated_repositories().await;
    repositories
        .upsert_project(project("project-catalog", T0))
        .await
        .expect("catalog project");
    let mut first = thread("workspace-1", "project-catalog", T0);
    first.kind = "workspace".to_owned();
    let mut second = thread("workspace-2", "project-catalog", T1);
    second.kind = "workspace".to_owned();
    let mut third = thread("workspace-3", "project-catalog", T2);
    third.kind = "workspace".to_owned();
    let mut panel = thread("panel", "project-catalog", T2);
    panel.kind = "panel".to_owned();
    let mut deleted = thread("deleted", "project-catalog", T2);
    deleted.kind = "workspace".to_owned();
    deleted.deleted_at = Some(T2.to_owned());
    for row in [first, second, third, panel, deleted] {
        repositories
            .upsert_thread(row)
            .await
            .expect("catalog thread");
    }

    let projection = repositories
        .load_worktree_catalog_projection("project-catalog".to_owned(), 2)
        .await
        .expect("catalog projection read")
        .expect("catalog project exists");

    assert_eq!(projection.project.project_id, "project-catalog");
    assert_eq!(
        projection
            .threads
            .iter()
            .map(|thread| thread.thread_id.as_str())
            .collect::<Vec<_>>(),
        ["workspace-1", "workspace-2"]
    );
    assert!(projection.truncated);
}

#[tokio::test]
async fn worktree_removal_receipts_are_insert_only_until_marked_removed() {
    let repositories = migrated_repositories().await;
    let receipt = WorktreeRemovalReceipt {
        owner_thread_id: "workspace-1".to_owned(),
        project_cwd: "C:/repo".to_owned(),
        worktree_path: "C:/worktrees/feature".to_owned(),
        identity_nonce: "identity-1".to_owned(),
        state: "prepared".to_owned(),
        created_at: String::new(),
        updated_at: String::new(),
    };
    let inserted = repositories
        .prepare_worktree_removal_receipt(receipt.clone())
        .await
        .expect("receipt insert");
    assert_eq!(inserted.owner_thread_id, receipt.owner_thread_id);
    assert_eq!(inserted.identity_nonce, receipt.identity_nonce);
    assert_eq!(inserted.state, "prepared");

    let conflicting = repositories
        .prepare_worktree_removal_receipt(WorktreeRemovalReceipt {
            identity_nonce: "identity-2".to_owned(),
            ..receipt.clone()
        })
        .await
        .expect("duplicate receipt read");
    assert_eq!(conflicting.identity_nonce, "identity-1");

    repositories
        .complete_worktree_removal_receipt(
            receipt.owner_thread_id.clone(),
            receipt.identity_nonce.clone(),
        )
        .await
        .expect("receipt completion");
    assert_eq!(
        repositories
            .get_worktree_removal_receipt(receipt.owner_thread_id)
            .await
            .expect("receipt lookup")
            .expect("receipt exists")
            .state,
        "removed"
    );
}

#[tokio::test]
async fn orchestration_event_writer_round_trips_json() {
    let repositories = migrated_repositories().await;
    repositories
        .database()
        .quick_check()
        .await
        .expect("database accessor returns the live database");

    let first = NewOrchestrationEvent {
        event_id: "event-1".to_owned(),
        event_type: "thread.created".to_owned(),
        aggregate_kind: "thread".to_owned(),
        aggregate_id: "thread-1".to_owned(),
        occurred_at: T0.to_owned(),
        command_id: Some("client:create".to_owned()),
        causation_event_id: None,
        correlation_id: Some("correlation-1".to_owned()),
        payload: json!({"text": "line one\nline two", "items": [1, true, null]}),
        metadata: json!({"nested": {"source": "test"}}),
    };
    let inserted_first = repositories
        .append_event(first.clone())
        .await
        .expect("event 1");
    assert!(inserted_first.sequence > 0);
    assert_row_eq(&inserted_first.event, &first);
}

#[tokio::test]
async fn event_pages_walk_the_log_after_a_cursor_one_bounded_page_at_a_time() {
    let repositories = migrated_repositories().await;
    let mut sequences = Vec::new();
    for index in 0..EVENT_PAGE_SIZE + 2 {
        let event = repositories
            .append_event(NewOrchestrationEvent {
                event_id: format!("paged-{index}"),
                event_type: "thread.updated".to_owned(),
                aggregate_kind: "thread".to_owned(),
                aggregate_id: "thread-1".to_owned(),
                occurred_at: T0.to_owned(),
                command_id: None,
                causation_event_id: None,
                correlation_id: None,
                payload: json!({}),
                metadata: json!({}),
            })
            .await
            .expect("event");
        sequences.push(event.sequence);
    }

    let mut pages = repositories.event_pages(sequences[0]);
    let first = pages
        .next_page()
        .await
        .expect("first page")
        .expect("events");
    assert_eq!(first.len(), EVENT_PAGE_SIZE);
    assert_eq!(first[0].sequence, sequences[1]);
    let second = pages
        .next_page()
        .await
        .expect("second page")
        .expect("events");
    assert_eq!(
        second
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        sequences[EVENT_PAGE_SIZE + 1..]
    );
    assert!(pages.next_page().await.expect("end").is_none());
    assert!(
        repositories
            .event_pages(*sequences.last().unwrap())
            .next_page()
            .await
            .expect("empty tail")
            .is_none()
    );
}

#[tokio::test]
async fn orchestration_event_reader_pages_seeded_json_rows_in_sequence_order() {
    let repositories = migrated_repositories().await;
    repositories
        .database()
        .call(|connection| {
            let rows = [
                (
                    "event-1",
                    "thread.created",
                    "thread",
                    "thread-1",
                    0_i64,
                    T0,
                    Some("client:create"),
                    None,
                    Some("correlation-1"),
                    "client",
                    json!({"text": "line one\nline two", "items": [1, true, null]}),
                    json!({"nested": {"source": "test"}}),
                ),
                (
                    "event-2",
                    "thread.updated",
                    "thread",
                    "thread-1",
                    1_i64,
                    T1,
                    Some("provider:resume"),
                    Some("event-1"),
                    Some("correlation-1"),
                    "provider",
                    json!({"state": "running"}),
                    json!({"adapterKey": "codex"}),
                ),
                (
                    "event-3",
                    "project.created",
                    "project",
                    "project-1",
                    0_i64,
                    T2,
                    None,
                    None,
                    None,
                    "server",
                    json!({"object": {"b": 2, "a": 1}}),
                    json!({}),
                ),
            ];
            for row in rows {
                connection.execute(
                    "INSERT INTO orchestration_events (event_id, event_type, aggregate_kind, stream_id, stream_version, occurred_at, command_id, causation_event_id, correlation_id, actor_kind, payload_json, metadata_json) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    rusqlite::params![
                        row.0,
                        row.1,
                        row.2,
                        row.3,
                        row.4,
                        row.5,
                        row.6,
                        row.7,
                        row.8,
                        row.9,
                        serde_json::to_string(&row.10).expect("payload JSON"),
                        serde_json::to_string(&row.11).expect("metadata JSON"),
                    ],
                )?;
            }
            Ok(())
        })
        .await
        .expect("seed orchestration events");

    let first_page = repositories
        .read_events_from_sequence(0, 2)
        .await
        .expect("first page");
    assert_eq!(
        first_page
            .iter()
            .map(|event| event.event.event_id.as_str())
            .collect::<Vec<_>>(),
        ["event-1", "event-2"]
    );
    let second_page = repositories
        .read_events_from_sequence(first_page[1].sequence, 10)
        .await
        .expect("second page");
    assert_eq!(second_page.len(), 1);
    assert_eq!(second_page[0].event.event_id, "event-3");
    assert_eq!(
        second_page[0].event.payload,
        json!({"object": {"b": 2, "a": 1}})
    );
    assert!(
        repositories
            .read_events_from_sequence(-100, 0)
            .await
            .expect("zero limit")
            .is_empty()
    );

    let storage_types = repositories
        .database()
        .call(|connection| {
            Ok(connection.query_row(
                "SELECT typeof(payload_json), typeof(metadata_json) FROM orchestration_events WHERE event_id = 'event-1'",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )?)
        })
        .await
        .expect("JSON storage types");
    assert_eq!(storage_types, ("text".to_owned(), "text".to_owned()));
}

#[tokio::test]
async fn command_receipt_and_checkpoint_diff_repositories_upsert_and_order() {
    let repositories = migrated_repositories().await;
    let mut receipt = CommandReceipt {
        command_id: "command-1".to_owned(),
        aggregate_kind: "thread".to_owned(),
        aggregate_id: "thread-1".to_owned(),
        accepted_at: T0.to_owned(),
        result_sequence: 1,
        status: "accepted".to_owned(),
        error: None,
        payload_digest: Some("digest-1".to_owned()),
    };
    repositories
        .upsert_command_receipt(receipt.clone())
        .await
        .expect("receipt insert");
    receipt.result_sequence = 3;
    receipt.status = "failed".to_owned();
    receipt.error = Some("first line\nsecond line".to_owned());
    repositories
        .upsert_command_receipt(receipt.clone())
        .await
        .expect("receipt upsert");
    assert_row_eq(
        &repositories
            .get_command_receipt(receipt.command_id.clone())
            .await
            .expect("receipt lookup")
            .expect("receipt exists"),
        &receipt,
    );
    assert!(
        repositories
            .get_command_receipt("missing".to_owned())
            .await
            .expect("missing receipt lookup")
            .is_none()
    );

    let early_diff = CheckpointDiffBlob {
        thread_id: "thread-1".to_owned(),
        from_turn_count: 0,
        to_turn_count: 1,
        diff: "@@ -1 +1 @@\n-old\n+new".to_owned(),
        created_at: T1.to_owned(),
    };
    let mut later_diff = CheckpointDiffBlob {
        thread_id: "thread-1".to_owned(),
        from_turn_count: 1,
        to_turn_count: 3,
        diff: "initial".to_owned(),
        created_at: T2.to_owned(),
    };
    repositories
        .upsert_checkpoint_diff_blob(later_diff.clone())
        .await
        .expect("later diff insert");
    repositories
        .upsert_checkpoint_diff_blob(early_diff.clone())
        .await
        .expect("early diff insert");
    later_diff.diff = "replacement\nwith multiple lines".to_owned();
    later_diff.created_at = TIME_3.to_owned();
    repositories
        .upsert_checkpoint_diff_blob(later_diff.clone())
        .await
        .expect("diff upsert");
    let diffs = repositories
        .list_checkpoint_diff_blobs_by_thread("thread-1".to_owned())
        .await
        .expect("diff listing");
    assert_eq!(
        diffs
            .iter()
            .map(|diff| diff.to_turn_count)
            .collect::<Vec<_>>(),
        [1, 3]
    );
    assert_row_eq(&diffs[0], &early_diff);
    assert_row_eq(&diffs[1], &later_diff);
}

#[tokio::test]
async fn provider_turn_delivery_repositories_fetch_filter_reference_and_claim() {
    let repositories = migrated_repositories().await;
    repositories
        .database()
        .call(|connection| {
            connection.execute_batch(
                "INSERT INTO orchestration_command_receipts (command_id, aggregate_kind, aggregate_id, accepted_at, result_sequence, status) VALUES ('command-a', 'thread', 'thread-1', '2026-08-01T00:00:00Z', 1, 'accepted'), ('command-b', 'thread', 'thread-1', '2026-08-01T00:00:01Z', 2, 'accepted'), ('command-c', 'thread', 'thread-1', '2026-08-01T00:00:02Z', 3, 'accepted');
                 INSERT INTO provider_turn_outbox (command_id, thread_id, message_id, provider_instance_id, provider_kind, provider_session_id, delivery_key, payload_json, state, attempts, last_error, created_at, updated_at) VALUES
                   ('command-b', 'thread-1', 'message-b', 'claudeAgent', 'claudeAgent', NULL, 'key-b', '{\"text\":\"later\"}', 'uncertain', 2, 'lost', '2026-08-01T00:00:02Z', '2026-08-01T00:00:03Z'),
                   ('command-c', 'thread-1', 'message-c', 'codex', 'codex', NULL, 'key-c', '{\"text\":\"excluded\"}', 'delivered', 1, NULL, '2026-08-01T00:00:04Z', '2026-08-01T00:00:05Z'),
                   ('command-a', 'thread-1', 'message-a', 'codex', 'codex', NULL, 'key-a', '{\"text\":\"first\"}', 'pending', 0, NULL, '2026-08-01T00:00:00Z', '2026-08-01T00:00:01Z');
                 INSERT INTO orchestration_attachment_refs (command_id, attachment_id, content_digest, size_bytes) VALUES ('command-a', 'attachment-z', 'digest-z', 3), ('command-b', 'attachment-a', NULL, 4);",
            )?;
            Ok(())
        })
        .await
        .expect("delivery fixtures insert");

    let fetched = repositories
        .get_provider_turn_delivery("command-a".to_owned())
        .await
        .expect("delivery fetch")
        .expect("delivery exists");
    assert_eq!(fetched.payload, json!({"text": "first"}));
    assert_eq!(fetched.provider_session_id, None);
    assert_eq!(
        repositories
            .list_provider_turn_deliveries(vec![TurnDeliveryState::Pending])
            .await
            .expect("delivery list")
            .into_iter()
            .map(|delivery| delivery.command_id)
            .collect::<Vec<_>>(),
        vec!["command-a"]
    );
    assert_eq!(
        repositories
            .list_referenced_attachment_ids()
            .await
            .expect("reference list"),
        vec!["attachment-a", "attachment-z"]
    );
    assert_eq!(
        repositories
            .thread_attachment_digests(
                "thread-1".to_owned(),
                vec![
                    "attachment-z".to_owned(),
                    "attachment-a".to_owned(),
                    "attachment-unreferenced".to_owned(),
                ],
            )
            .await
            .expect("thread attachment digests"),
        HashMap::from([
            ("attachment-z".to_owned(), Some("digest-z".to_owned())),
            ("attachment-a".to_owned(), None),
        ])
    );
    assert!(
        repositories
            .thread_attachment_digests("thread-2".to_owned(), vec!["attachment-z".to_owned()])
            .await
            .expect("another thread's digests")
            .is_empty(),
        "an attachment belongs only to the thread whose command attached it"
    );
    let claimed = repositories
        .claim_provider_turn("command-a".to_owned(), "2026-08-01T00:00:04Z".to_owned())
        .await
        .expect("first claim")
        .expect("pending turn claims");
    assert_eq!(claimed.state, TurnDeliveryState::Sending);
    assert_eq!(claimed.attempts, 1);
    let frozen = repositories
        .freeze_provider_turn_session(
            "command-a".to_owned(),
            1,
            "codex".to_owned(),
            "codex".to_owned(),
            "session-a".to_owned(),
            "2026-08-01T00:00:05Z".to_owned(),
        )
        .await
        .expect("session freeze")
        .expect("sending turn freezes");
    assert_eq!(frozen.provider_session_id.as_deref(), Some("session-a"));
    assert!(
        repositories
            .freeze_provider_turn_session(
                "command-a".to_owned(),
                1,
                "codex".to_owned(),
                "codex".to_owned(),
                "session-a".to_owned(),
                "2026-08-01T00:00:06Z".to_owned(),
            )
            .await
            .expect("idempotent session freeze")
            .is_some()
    );
    assert!(
        repositories
            .freeze_provider_turn_session(
                "command-a".to_owned(),
                1,
                "codex".to_owned(),
                "codex".to_owned(),
                "session-drift".to_owned(),
                "2026-08-01T00:00:07Z".to_owned(),
            )
            .await
            .expect("session drift is a conditional miss")
            .is_none()
    );
    assert!(
        repositories
            .freeze_provider_turn_session(
                "command-a".to_owned(),
                2,
                "codex".to_owned(),
                "codex".to_owned(),
                "session-a".to_owned(),
                "2026-08-01T00:00:08Z".to_owned(),
            )
            .await
            .expect("stale attempt is a conditional miss")
            .is_none()
    );
    let frozen = repositories
        .get_provider_turn_delivery("command-a".to_owned())
        .await
        .expect("frozen delivery fetch")
        .expect("frozen delivery exists");
    assert_eq!(frozen.provider_session_id.as_deref(), Some("session-a"));
    assert_eq!(frozen.attempts, 1);
    assert!(
        repositories
            .claim_provider_turn("command-a".to_owned(), "2026-08-01T00:00:05Z".to_owned())
            .await
            .expect("second claim")
            .is_none()
    );
}

#[tokio::test]
async fn runtime_project_and_thread_repositories_upsert_order_and_delete() {
    let repositories = migrated_repositories().await;

    let runtime_early = ProviderSessionRuntime {
        thread_id: "runtime-a".to_owned(),
        provider_name: "codex".to_owned(),
        provider_instance_id: Some("instance-a".to_owned()),
        adapter_key: "codex-app-server".to_owned(),
        runtime_mode: "full-access".to_owned(),
        status: "idle".to_owned(),
        last_seen_at: T1.to_owned(),
        resume_cursor: Some(json!({"sequence": 7, "tokens": ["a", "b"]})),
        runtime_payload: Some(json!({"pid": 1234, "state": {"healthy": true}})),
    };
    let mut runtime_late = ProviderSessionRuntime {
        thread_id: "runtime-b".to_owned(),
        last_seen_at: T2.to_owned(),
        ..runtime_early.clone()
    };
    repositories
        .upsert_provider_session_runtime(runtime_late.clone())
        .await
        .expect("late runtime insert");
    repositories
        .upsert_provider_session_runtime(runtime_early.clone())
        .await
        .expect("early runtime insert");
    runtime_late.status = "running".to_owned();
    runtime_late.resume_cursor = None;
    runtime_late.runtime_payload = Some(json!({"pid": 5678, "lines": "one\ntwo"}));
    repositories
        .upsert_provider_session_runtime(runtime_late.clone())
        .await
        .expect("runtime upsert");
    assert_row_eq(
        &repositories
            .get_provider_session_runtime("runtime-b".to_owned())
            .await
            .expect("runtime lookup")
            .expect("runtime exists"),
        &runtime_late,
    );
    assert_eq!(
        repositories
            .list_provider_session_runtimes()
            .await
            .expect("runtime listing")
            .iter()
            .map(|runtime| runtime.thread_id.as_str())
            .collect::<Vec<_>>(),
        ["runtime-a", "runtime-b"]
    );
    repositories
        .delete_provider_session_runtime("runtime-a".to_owned())
        .await
        .expect("runtime deletion");
    assert!(
        repositories
            .get_provider_session_runtime("runtime-a".to_owned())
            .await
            .expect("deleted runtime lookup")
            .is_none()
    );

    let project_early = project("project-a", T0);
    let mut project_late = project("project-b", T1);
    repositories
        .upsert_project(project_late.clone())
        .await
        .expect("late project insert");
    repositories
        .upsert_project(project_early.clone())
        .await
        .expect("early project insert");
    project_late.title = "Updated title".to_owned();
    project_late.scripts = json!({"test": ["vp", "test"], "env": {"CI": true}});
    project_late.worktree_discovery = json!({
        "visibility": "hidden",
        "initialPromptDismissedAt": null,
        "baselinePaths": ["/workspace/project-b"]
    });
    project_late.updated_at = TIME_3.to_owned();
    repositories
        .upsert_project(project_late.clone())
        .await
        .expect("project upsert");
    assert_row_eq(
        &repositories
            .get_project("project-b".to_owned())
            .await
            .expect("project lookup")
            .expect("project exists"),
        &project_late,
    );
    assert_eq!(
        repositories
            .list_projects()
            .await
            .expect("project listing")
            .iter()
            .map(|project| project.project_id.as_str())
            .collect::<Vec<_>>(),
        ["project-a", "project-b"]
    );

    repositories
        .database()
        .call(|connection| {
            connection.execute(
                "UPDATE projection_projects SET worktree_discovery_json = '{\"visibility\":\"not-a-visibility\",\"initialPromptDismissedAt\":null,\"baselinePaths\":[]}' WHERE project_id = 'project-b'",
                [],
            )?;
            Ok(())
        })
        .await
        .expect("malformed policy fixture persists without a repository fallback");
    assert_eq!(
        repositories
            .get_project("project-b".to_owned())
            .await
            .expect("malformed policy remains readable JSON")
            .expect("project exists")
            .worktree_discovery,
        json!({
            "visibility": "not-a-visibility",
            "initialPromptDismissedAt": null,
            "baselinePaths": []
        })
    );

    let thread_early = thread("thread-a", "project-b", T1);
    let mut thread_late = thread("thread-b", "project-b", T2);
    repositories
        .upsert_thread(thread_late.clone())
        .await
        .expect("late thread insert");
    repositories
        .upsert_thread(thread_early.clone())
        .await
        .expect("early thread insert");
    thread_late.title = "Updated thread".to_owned();
    thread_late.pending_approval_count = 2;
    thread_late.has_actionable_proposed_plan = 1;
    thread_late.updated_at = TIME_3.to_owned();
    repositories
        .upsert_thread(thread_late.clone())
        .await
        .expect("thread upsert");
    assert_row_eq(
        &repositories
            .get_thread("thread-b".to_owned())
            .await
            .expect("thread lookup")
            .expect("thread exists"),
        &thread_late,
    );
    assert_eq!(
        repositories
            .list_threads_by_project("project-b".to_owned())
            .await
            .expect("thread listing")
            .iter()
            .map(|thread| thread.thread_id.as_str())
            .collect::<Vec<_>>(),
        ["thread-a", "thread-b"]
    );

    repositories
        .delete_thread("thread-a".to_owned())
        .await
        .expect("thread deletion");
    assert!(
        repositories
            .get_thread("thread-a".to_owned())
            .await
            .expect("deleted thread lookup")
            .is_none()
    );
    repositories
        .delete_project("project-a".to_owned())
        .await
        .expect("project deletion");
    assert!(
        repositories
            .get_project("project-a".to_owned())
            .await
            .expect("deleted project lookup")
            .is_none()
    );
}

#[tokio::test]
async fn thread_sessions_by_status_filter_and_order_complete_rows() {
    let repositories = migrated_repositories().await;
    let mut expected = Vec::new();
    for (thread_id, status, updated_at) in [
        ("later", "running", T2),
        ("b", "connecting", T1),
        ("ready", "ready", T0),
        ("error", "error", T0),
        ("idle", "idle", T0),
        ("stopped", "stopped", T0),
        ("a", "starting", T1),
    ] {
        let session = ProjectionThreadSession {
            thread_id: thread_id.into(),
            status: status.into(),
            provider_name: Some("codex".into()),
            provider_instance_id: Some("custom-codex".into()),
            runtime_mode: "approval-required".into(),
            active_turn_id: Some(format!("turn-{thread_id}")),
            last_error: Some("previous failure".into()),
            last_error_class: Some("transport_error".into()),
            updated_at: updated_at.into(),
        };
        repositories
            .upsert_thread_session(session.clone())
            .await
            .unwrap();
        if matches!(status, "starting" | "connecting" | "running") {
            expected.push(session);
        }
    }
    let sessions = repositories
        .list_thread_sessions_by_status(vec![
            "starting".into(),
            "connecting".into(),
            "running".into(),
        ])
        .await
        .unwrap();
    assert_eq!(
        sessions
            .iter()
            .map(|session| session.thread_id.as_str())
            .collect::<Vec<_>>(),
        ["a", "b", "later"]
    );
    for (actual, expected) in sessions.iter().zip(expected.iter().rev()) {
        assert_row_eq(actual, expected);
    }
    assert!(
        repositories
            .list_thread_sessions_by_status(vec![])
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        repositories
            .list_thread_sessions_by_status(vec!["running' OR 1=1 --".into()])
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn conversation_projection_repositories_round_trip_order_and_delete() {
    let repositories = migrated_repositories().await;

    let message_a = ProjectionThreadMessage {
        delivery_reason: None,
        delivery_provider_instance_id: None,
        delivery_mode: None,
        delivery_held: None,
        message_id: "message-a".to_owned(),
        thread_id: "thread-conversation".to_owned(),
        turn_id: Some("turn-a".to_owned()),
        role: "user".to_owned(),
        text: "first\nmessage".to_owned(),
        attachments: Some(json!([{
            "kind": "image",
            "path": "C:/tmp/screenshot.png",
            "metadata": {"width": 800, "height": 600}
        }])),
        is_streaming: false,
        delivery_state: Some("uncertain".to_owned()),
        delivery_provider: Some("claudeAgent".to_owned()),
        delivery_detail: Some("connection lost after write".to_owned()),
        created_at: T1.to_owned(),
        updated_at: T1.to_owned(),
    };
    let message_b = ProjectionThreadMessage {
        delivery_reason: None,
        delivery_provider_instance_id: None,
        message_id: "message-b".to_owned(),
        created_at: T2.to_owned(),
        updated_at: T2.to_owned(),
        attachments: None,
        ..message_a.clone()
    };
    repositories
        .upsert_message(message_b.clone())
        .await
        .expect("later message insert");
    repositories
        .upsert_message(message_a.clone())
        .await
        .expect("earlier message insert");
    let mut message_update = message_a.clone();
    message_update.text = "updated while preserving attachments".to_owned();
    message_update.attachments = None;
    message_update.is_streaming = true;
    message_update.updated_at = TIME_3.to_owned();
    repositories
        .upsert_message(message_update.clone())
        .await
        .expect("message upsert");
    let stored_message = repositories
        .get_message("message-a".to_owned())
        .await
        .expect("message lookup")
        .expect("message exists");
    assert_eq!(stored_message.text, message_update.text);
    assert_eq!(stored_message.is_streaming, message_update.is_streaming);
    assert_eq!(stored_message.attachments, message_a.attachments);
    assert_eq!(
        repositories
            .list_messages_by_thread("thread-conversation".to_owned())
            .await
            .expect("message listing")
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>(),
        ["message-a", "message-b"]
    );
    repositories
        .delete_messages_by_thread("thread-conversation".to_owned())
        .await
        .expect("message deletion");
    assert!(
        repositories
            .list_messages_by_thread("thread-conversation".to_owned())
            .await
            .expect("empty message listing")
            .is_empty()
    );

    let activity_none = ProjectionThreadActivity {
        activity_id: "activity-none".to_owned(),
        thread_id: "thread-conversation".to_owned(),
        turn_id: None,
        tone: "neutral".to_owned(),
        kind: "status".to_owned(),
        summary: "No provider sequence".to_owned(),
        payload: json!({"details": ["a", {"b": true}]}),
        sequence: None,
        created_at: TIME_3.to_owned(),
    };
    let activity_one = ProjectionThreadActivity {
        activity_id: "activity-one".to_owned(),
        sequence: Some(1),
        created_at: T2.to_owned(),
        ..activity_none.clone()
    };
    let activity_two = ProjectionThreadActivity {
        activity_id: "activity-two".to_owned(),
        sequence: Some(2),
        created_at: T1.to_owned(),
        ..activity_none.clone()
    };
    for activity in [&activity_two, &activity_none, &activity_one] {
        repositories
            .upsert_activity(activity.clone())
            .await
            .expect("activity upsert");
    }
    let mut activity_one_update = activity_one.clone();
    activity_one_update.summary = "Updated".to_owned();
    activity_one_update.payload = json!({"multiline": "one\ntwo"});
    repositories
        .upsert_activity(activity_one_update.clone())
        .await
        .expect("activity idempotent update");
    let activities = repositories
        .list_activities_by_thread("thread-conversation".to_owned())
        .await
        .expect("activity listing");
    assert_eq!(
        activities
            .iter()
            .map(|activity| activity.activity_id.as_str())
            .collect::<Vec<_>>(),
        ["activity-none", "activity-one", "activity-two"]
    );
    assert_row_eq(&activities[1], &activity_one_update);
    repositories
        .delete_activities_by_thread("thread-conversation".to_owned())
        .await
        .expect("activity deletion");

    let mut session = ProjectionThreadSession {
        thread_id: "thread-conversation".to_owned(),
        status: "idle".to_owned(),
        provider_name: Some("codex".to_owned()),
        provider_instance_id: Some("instance-1".to_owned()),
        runtime_mode: "full-access".to_owned(),
        active_turn_id: None,
        last_error: None,
        last_error_class: None,
        updated_at: T1.to_owned(),
    };
    repositories
        .upsert_thread_session(session.clone())
        .await
        .expect("session insert");
    session.status = "failed".to_owned();
    session.last_error = Some("provider exited\nwith code 1".to_owned());
    session.updated_at = T2.to_owned();
    repositories
        .upsert_thread_session(session.clone())
        .await
        .expect("session upsert");
    assert_row_eq(
        &repositories
            .get_thread_session("thread-conversation".to_owned())
            .await
            .expect("session lookup")
            .expect("session exists"),
        &session,
    );
    repositories
        .delete_thread_session("thread-conversation".to_owned())
        .await
        .expect("session deletion");
    assert!(
        repositories
            .get_thread_session("thread-conversation".to_owned())
            .await
            .expect("deleted session lookup")
            .is_none()
    );

    let approval_a = ProjectionPendingApproval {
        request_id: "approval-a".to_owned(),
        thread_id: "thread-conversation".to_owned(),
        turn_id: Some("turn-a".to_owned()),
        status: "pending".to_owned(),
        decision: None,
        created_at: T1.to_owned(),
        resolved_at: None,
    };
    let mut approval_b = ProjectionPendingApproval {
        request_id: "approval-b".to_owned(),
        created_at: T2.to_owned(),
        ..approval_a.clone()
    };
    repositories
        .upsert_pending_approval(approval_b.clone())
        .await
        .expect("later approval insert");
    repositories
        .upsert_pending_approval(approval_a.clone())
        .await
        .expect("earlier approval insert");
    approval_b.status = "resolved".to_owned();
    approval_b.decision = Some("approved".to_owned());
    approval_b.resolved_at = Some(TIME_3.to_owned());
    repositories
        .upsert_pending_approval(approval_b.clone())
        .await
        .expect("approval upsert");
    assert_row_eq(
        &repositories
            .get_pending_approval("approval-b".to_owned())
            .await
            .expect("approval lookup")
            .expect("approval exists"),
        &approval_b,
    );
    assert_eq!(
        repositories
            .list_pending_approvals_by_thread("thread-conversation".to_owned())
            .await
            .expect("approval listing")
            .iter()
            .map(|approval| approval.request_id.as_str())
            .collect::<Vec<_>>(),
        ["approval-a", "approval-b"]
    );
    repositories
        .delete_pending_approval("approval-a".to_owned())
        .await
        .expect("approval deletion");
    assert!(
        repositories
            .get_pending_approval("approval-a".to_owned())
            .await
            .expect("deleted approval lookup")
            .is_none()
    );

    let plan_a = ProjectionThreadProposedPlan {
        plan_id: "plan-a".to_owned(),
        thread_id: "thread-conversation".to_owned(),
        turn_id: Some("turn-a".to_owned()),
        plan_markdown: "# Plan\n\n1. First".to_owned(),
        implemented_at: None,
        implementation_thread_id: None,
        created_at: T1.to_owned(),
        updated_at: T1.to_owned(),
    };
    let mut plan_b = ProjectionThreadProposedPlan {
        plan_id: "plan-b".to_owned(),
        created_at: T2.to_owned(),
        updated_at: T2.to_owned(),
        ..plan_a.clone()
    };
    repositories
        .upsert_proposed_plan(plan_b.clone())
        .await
        .expect("later plan insert");
    repositories
        .upsert_proposed_plan(plan_a.clone())
        .await
        .expect("earlier plan insert");
    plan_b.implemented_at = Some(TIME_3.to_owned());
    plan_b.implementation_thread_id = Some("thread-implementation".to_owned());
    repositories
        .upsert_proposed_plan(plan_b.clone())
        .await
        .expect("plan upsert");
    let plans = repositories
        .list_proposed_plans_by_thread("thread-conversation".to_owned())
        .await
        .expect("plan listing");
    assert_eq!(
        plans
            .iter()
            .map(|plan| plan.plan_id.as_str())
            .collect::<Vec<_>>(),
        ["plan-a", "plan-b"]
    );
    assert_row_eq(&plans[1], &plan_b);
    repositories
        .delete_proposed_plans_by_thread("thread-conversation".to_owned())
        .await
        .expect("plan deletion");
    assert!(
        repositories
            .list_proposed_plans_by_thread("thread-conversation".to_owned())
            .await
            .expect("empty plan listing")
            .is_empty()
    );

    let state_b = ProjectionState {
        projector: "threads".to_owned(),
        last_applied_sequence: 12,
        updated_at: T2.to_owned(),
    };
    let mut state_a = ProjectionState {
        projector: "messages".to_owned(),
        last_applied_sequence: 7,
        updated_at: T1.to_owned(),
    };
    repositories
        .upsert_projection_state(state_b.clone())
        .await
        .expect("state b insert");
    repositories
        .upsert_projection_state(state_a.clone())
        .await
        .expect("state a insert");
    state_a.last_applied_sequence = 9;
    state_a.updated_at = TIME_3.to_owned();
    repositories
        .upsert_projection_state(state_a.clone())
        .await
        .expect("state upsert");
    assert_row_eq(
        &repositories
            .get_projection_state("messages".to_owned())
            .await
            .expect("state lookup")
            .expect("state exists"),
        &state_a,
    );
    assert_eq!(
        repositories
            .list_projection_states()
            .await
            .expect("state listing")
            .iter()
            .map(|state| state.projector.as_str())
            .collect::<Vec<_>>(),
        ["messages", "threads"]
    );
    assert_eq!(
        repositories
            .min_last_applied_sequence()
            .await
            .expect("minimum state sequence"),
        Some(9)
    );
}

#[tokio::test]
async fn turn_and_checkpoint_repositories_preserve_conflicts_and_roll_back_transactions() {
    let repositories = migrated_repositories().await;

    let pending_a = ProjectionPendingTurnStart {
        thread_id: "thread-turns".to_owned(),
        message_id: "pending-a".to_owned(),
        source_proposed_plan_thread_id: Some("source-thread".to_owned()),
        source_proposed_plan_id: Some("source-plan".to_owned()),
        requested_at: T1.to_owned(),
    };
    let pending_b = ProjectionPendingTurnStart {
        message_id: "pending-b".to_owned(),
        requested_at: T2.to_owned(),
        ..pending_a.clone()
    };
    repositories
        .replace_pending_turn_start(pending_a.clone())
        .await
        .expect("first pending turn");
    repositories
        .replace_pending_turn_start(pending_b.clone())
        .await
        .expect("pending turn replacement");
    assert_row_eq(
        &repositories
            .get_pending_turn_start("thread-turns".to_owned())
            .await
            .expect("pending turn lookup")
            .expect("pending turn exists"),
        &pending_b,
    );

    repositories
        .database()
        .call(|connection| {
            connection.execute_batch(
                "CREATE TRIGGER reject_pending_turn BEFORE INSERT ON projection_turns \
                 WHEN NEW.pending_message_id = 'reject-pending' \
                 BEGIN SELECT RAISE(ABORT, 'reject pending turn'); END;",
            )?;
            Ok(())
        })
        .await
        .expect("pending rollback trigger");
    let rejected_pending = ProjectionPendingTurnStart {
        message_id: "reject-pending".to_owned(),
        requested_at: TIME_3.to_owned(),
        ..pending_b.clone()
    };
    assert!(
        repositories
            .replace_pending_turn_start(rejected_pending)
            .await
            .is_err()
    );
    assert_row_eq(
        &repositories
            .get_pending_turn_start("thread-turns".to_owned())
            .await
            .expect("pending turn after rollback")
            .expect("original pending turn survives"),
        &pending_b,
    );
    repositories
        .delete_pending_turn_start("thread-turns".to_owned())
        .await
        .expect("pending turn deletion");
    assert!(
        repositories
            .get_pending_turn_start("thread-turns".to_owned())
            .await
            .expect("deleted pending turn lookup")
            .is_none()
    );

    let mut turn_a = turn("turn-a", Some(5));
    let turn_b = turn("turn-b", None);
    repositories
        .upsert_turn_by_id(turn_a.clone())
        .await
        .expect("turn a insert");
    repositories
        .upsert_turn_by_id(turn_b.clone())
        .await
        .expect("turn b insert");
    turn_a.state = "error".to_owned();
    turn_a.checkpoint_files = json!(["a.rs", "b.rs"]);
    repositories
        .upsert_turn_by_id(turn_a.clone())
        .await
        .expect("turn upsert");
    assert_row_eq(
        &repositories
            .get_turn_by_id("thread-turns".to_owned(), "turn-a".to_owned())
            .await
            .expect("turn lookup")
            .expect("turn exists"),
        &turn_a,
    );
    assert!(
        repositories
            .get_turn_by_id("thread-turns".to_owned(), "missing".to_owned())
            .await
            .expect("missing turn lookup")
            .is_none()
    );
    assert_eq!(
        repositories
            .list_turns_by_thread("thread-turns".to_owned())
            .await
            .expect("turn listing")
            .len(),
        2
    );

    repositories
        .clear_checkpoint_turn_conflict("thread-turns".to_owned(), "turn-b".to_owned(), 5)
        .await
        .expect("checkpoint conflict clear");
    let cleared_turn = repositories
        .get_turn_by_id("thread-turns".to_owned(), "turn-a".to_owned())
        .await
        .expect("cleared turn lookup")
        .expect("turn remains");
    assert_eq!(cleared_turn.checkpoint_turn_count, None);
    assert_eq!(cleared_turn.checkpoint_files, json!([]));

    let old_checkpoint_turn = turn("old-checkpoint", Some(8));
    repositories
        .upsert_turn_by_id(old_checkpoint_turn)
        .await
        .expect("old checkpoint turn");
    let checkpoint = ProjectionCheckpoint {
        thread_id: "thread-turns".to_owned(),
        turn_id: "new-checkpoint".to_owned(),
        checkpoint_turn_count: 8,
        checkpoint_ref: "checkpoint-new".to_owned(),
        status: "ready".to_owned(),
        files: json!([{"path": "src/lib.rs", "sha": "abc123"}]),
        assistant_message_id: Some("assistant-checkpoint".to_owned()),
        completed_at: TIME_3.to_owned(),
    };
    repositories
        .upsert_checkpoint(checkpoint.clone())
        .await
        .expect("checkpoint conflict replacement");
    assert_row_eq(
        &repositories
            .get_checkpoint("thread-turns".to_owned(), 8)
            .await
            .expect("checkpoint lookup")
            .expect("checkpoint exists"),
        &checkpoint,
    );
    assert_eq!(
        repositories
            .list_checkpoints_by_thread("thread-turns".to_owned())
            .await
            .expect("checkpoint listing")
            .iter()
            .map(|checkpoint| checkpoint.checkpoint_turn_count)
            .collect::<Vec<_>>(),
        [8]
    );

    repositories
        .database()
        .call(|connection| {
            connection.execute_batch(
                "CREATE TRIGGER reject_checkpoint BEFORE INSERT ON projection_turns \
                 WHEN NEW.turn_id = 'reject-checkpoint' \
                 BEGIN SELECT RAISE(ABORT, 'reject checkpoint'); END;",
            )?;
            Ok(())
        })
        .await
        .expect("checkpoint rollback trigger");
    let rejected_checkpoint = ProjectionCheckpoint {
        turn_id: "reject-checkpoint".to_owned(),
        checkpoint_ref: "must-not-replace".to_owned(),
        ..checkpoint.clone()
    };
    assert!(
        repositories
            .upsert_checkpoint(rejected_checkpoint)
            .await
            .is_err()
    );
    assert_row_eq(
        &repositories
            .get_checkpoint("thread-turns".to_owned(), 8)
            .await
            .expect("checkpoint after rollback")
            .expect("original checkpoint survives"),
        &checkpoint,
    );

    repositories
        .delete_checkpoints_by_thread("thread-turns".to_owned())
        .await
        .expect("checkpoint deletion");
    assert!(
        repositories
            .list_checkpoints_by_thread("thread-turns".to_owned())
            .await
            .expect("empty checkpoint listing")
            .is_empty()
    );
    assert!(
        !repositories
            .list_turns_by_thread("thread-turns".to_owned())
            .await
            .expect("turns survive checkpoint clearing")
            .is_empty()
    );
    repositories
        .delete_turns_by_thread("thread-turns".to_owned())
        .await
        .expect("turn deletion");
    assert!(
        repositories
            .list_turns_by_thread("thread-turns".to_owned())
            .await
            .expect("empty turn listing")
            .is_empty()
    );
}

#[tokio::test]
async fn auth_pairing_links_consume_and_revoke_atomically() {
    let repositories = migrated_repositories().await;
    let pairing = AuthPairingLink {
        id: "pairing-a".to_owned(),
        credential: "credential-a".to_owned(),
        method: "pairing".to_owned(),
        scopes: json!(["rpc:read", {"delegated": ["rpc:write"]}]),
        subject: "user-a".to_owned(),
        label: Some("Laptop".to_owned()),
        proof_key_thumbprint: Some("thumbprint-a".to_owned()),
        created_at: T1.to_owned(),
        expires_at: FUTURE.to_owned(),
        consumed_at: None,
        revoked_at: None,
        reach: None,
        off_host: None,
    };
    let later_pairing = AuthPairingLink {
        id: "pairing-b".to_owned(),
        credential: "credential-b".to_owned(),
        proof_key_thumbprint: None,
        created_at: T2.to_owned(),
        ..pairing.clone()
    };
    repositories
        .create_auth_pairing_link(pairing.clone())
        .await
        .expect("pairing insert");
    repositories
        .create_auth_pairing_link(later_pairing.clone())
        .await
        .expect("later pairing insert");
    assert_row_eq(
        &repositories
            .get_auth_pairing_link_by_credential("credential-a".to_owned())
            .await
            .expect("pairing lookup")
            .expect("pairing exists"),
        &pairing,
    );
    assert_eq!(
        repositories
            .list_active_auth_pairing_links(T0.to_owned())
            .await
            .expect("active pairing listing")
            .iter()
            .map(|pairing| pairing.id.as_str())
            .collect::<Vec<_>>(),
        ["pairing-b", "pairing-a"]
    );
    assert!(
        repositories
            .consume_auth_pairing_link(
                "credential-a".to_owned(),
                Some("wrong-thumbprint".to_owned()),
                T2.to_owned(),
                T2.to_owned(),
            )
            .await
            .expect("wrong proof is handled")
            .is_none()
    );

    let first_consumer = repositories.clone();
    let second_consumer = repositories.clone();
    let (first_result, second_result) = tokio::join!(
        first_consumer.consume_auth_pairing_link(
            "credential-a".to_owned(),
            Some("thumbprint-a".to_owned()),
            T2.to_owned(),
            T2.to_owned(),
        ),
        second_consumer.consume_auth_pairing_link(
            "credential-a".to_owned(),
            Some("thumbprint-a".to_owned()),
            TIME_3.to_owned(),
            T2.to_owned(),
        )
    );
    let consumed = [
        first_result.expect("first atomic consume"),
        second_result.expect("second atomic consume"),
    ];
    assert_eq!(consumed.iter().filter(|row| row.is_some()).count(), 1);
    let consumed_row = consumed
        .into_iter()
        .flatten()
        .next()
        .expect("one consumer wins");
    assert_eq!(consumed_row.id, "pairing-a");
    assert!(consumed_row.consumed_at.is_some());
    assert!(
        !repositories
            .revoke_auth_pairing_link("pairing-a".to_owned(), TIME_3.to_owned())
            .await
            .expect("consumed pairing cannot be revoked")
    );
    assert!(
        repositories
            .revoke_auth_pairing_link("pairing-b".to_owned(), TIME_3.to_owned())
            .await
            .expect("active pairing revoked")
    );
    assert!(
        !repositories
            .revoke_auth_pairing_link("pairing-b".to_owned(), TIME_3.to_owned())
            .await
            .expect("pairing revocation is idempotent")
    );
    assert!(
        repositories
            .list_active_auth_pairing_links(T0.to_owned())
            .await
            .expect("no active pairings remain")
            .is_empty()
    );
}

#[tokio::test]
async fn auth_authority_revision_changes_only_with_authority_transactions() {
    let repositories = migrated_repositories().await;
    assert_eq!(
        repositories
            .auth_authority_revision()
            .await
            .expect("initial auth revision"),
        0
    );
    repositories
        .upsert_project(project("unrelated", T1))
        .await
        .expect("unrelated durable mutation");
    assert_eq!(
        repositories
            .auth_authority_revision()
            .await
            .expect("unchanged auth revision"),
        0
    );
    repositories
        .create_auth_pairing_link(pairing_offer_link(9_000))
        .await
        .expect("auth authority mutation");
    assert_eq!(
        repositories
            .auth_authority_revision()
            .await
            .expect("created auth revision"),
        1
    );
    assert!(
        repositories
            .revoke_auth_pairing_link("pairing-quota-9000".to_owned(), TIME_3.to_owned())
            .await
            .expect("auth revocation")
    );
    assert_eq!(
        repositories
            .auth_authority_revision()
            .await
            .expect("revoked auth revision"),
        2
    );
}

#[tokio::test]
async fn pairing_offer_reservation_completion_and_cancellation_are_durable() {
    let repositories = migrated_repositories().await;
    let pairing = AuthPairingLink {
        id: "pairing-offer".to_owned(),
        credential: "credential-offer".to_owned(),
        method: "one-time-token".to_owned(),
        scopes: json!(["orchestration:read"]),
        subject: "one-time-token".to_owned(),
        label: Some("Tablet".to_owned()),
        proof_key_thumbprint: None,
        created_at: T1.to_owned(),
        expires_at: FUTURE.to_owned(),
        consumed_at: None,
        revoked_at: None,
        reach: Some("another-device".to_owned()),
        off_host: Some(true),
    };
    repositories
        .create_auth_pairing_link_with_offer(
            pairing.clone(),
            NewAuthPairingOffer {
                principal_id: "principal".to_owned(),
                idempotency_key: "request-key".to_owned(),
                input_fingerprint: "fingerprint".to_owned(),
                expires_at: FUTURE.to_owned(),
            },
        )
        .await
        .expect("pairing and reservation commit together");
    let conflicting_pairing = AuthPairingLink {
        id: "pairing-conflict".to_owned(),
        credential: "credential-conflict".to_owned(),
        ..pairing
    };
    let existing = repositories
        .create_auth_pairing_link_with_offer(
            conflicting_pairing,
            NewAuthPairingOffer {
                principal_id: "principal".to_owned(),
                idempotency_key: "request-key".to_owned(),
                input_fingerprint: "other fingerprint".to_owned(),
                expires_at: FUTURE.to_owned(),
            },
        )
        .await
        .expect("existing keyed authority");
    assert!(!existing.reserved);
    assert_eq!(existing.offer.input_fingerprint, "fingerprint");
    assert!(
        repositories
            .get_auth_pairing_link_by_credential("credential-conflict".to_owned())
            .await
            .expect("rolled-back link lookup")
            .is_none(),
        "the losing keyed candidate must not insert a pairing link"
    );
    let pending = repositories
        .prune_and_list_active_auth_pairing_offers(T2.to_owned())
        .await
        .expect("pending reservation");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].pairing_id.as_deref(), Some("pairing-offer"));
    assert!(pending[0].result.is_none());

    let result = json!({
        "id": "pairing-offer",
        "code": "encoded-offer",
        "reach": "another-device",
        "endpoint": "http://192.168.1.20:3773",
        "name": "Tablet",
        "expiresAt": FUTURE,
    });
    assert!(
        repositories
            .complete_auth_pairing_offer(
                "principal".to_owned(),
                "request-key".to_owned(),
                result.clone(),
            )
            .await
            .expect("offer completion")
    );
    assert_eq!(
        repositories
            .prune_and_list_active_auth_pairing_offers(T2.to_owned())
            .await
            .expect("completed reservation")[0]
            .result
            .as_ref(),
        Some(&result)
    );

    let cancellation = repositories
        .cancel_auth_pairing_offer(
            "principal".to_owned(),
            "request-key".to_owned(),
            TIME_3.to_owned(),
            FUTURE.to_owned(),
        )
        .await
        .expect("offer cancellation");
    assert!(cancellation.revoked);
    assert_eq!(cancellation.pairing_id.as_deref(), Some("pairing-offer"));
    assert!(
        repositories
            .list_active_auth_pairing_links(T2.to_owned())
            .await
            .expect("revoked grant is absent")
            .is_empty()
    );
    let tombstone = repositories
        .prune_and_list_active_auth_pairing_offers(TIME_3.to_owned())
        .await
        .expect("durable tombstone");
    assert_eq!(tombstone.len(), 1);
    assert!(tombstone[0].pairing_id.is_none());
    assert!(tombstone[0].result.is_none());
    assert_eq!(tombstone[0].cancelled_at.as_deref(), Some(TIME_3));
}

#[tokio::test]
async fn pending_pairing_offer_recovery_revokes_the_grant_and_releases_the_key() {
    let repositories = migrated_repositories().await;
    repositories
        .create_auth_pairing_link_with_offer(
            pairing_offer_link(17),
            NewAuthPairingOffer {
                principal_id: "principal".to_owned(),
                idempotency_key: "interrupted-request".to_owned(),
                input_fingerprint: "same-input".to_owned(),
                expires_at: FUTURE.to_owned(),
            },
        )
        .await
        .expect("pending pairing offer");

    assert!(
        repositories
            .recover_pending_auth_pairing_offer(
                "principal".to_owned(),
                "interrupted-request".to_owned(),
                "different-input".to_owned(),
                T2.to_owned(),
                T2.to_owned(),
            )
            .await
            .expect("conflicting recovery lookup")
            .is_none(),
        "a conflicting request must not abandon another request's grant"
    );
    assert_eq!(
        repositories
            .list_active_auth_pairing_links(T2.to_owned())
            .await
            .expect("conflicting recovery preserves the grant")
            .len(),
        1
    );

    assert!(
        repositories
            .recover_pending_auth_pairing_offer(
                "principal".to_owned(),
                "interrupted-request".to_owned(),
                "same-input".to_owned(),
                T2.to_owned(),
                T0.to_owned(),
            )
            .await
            .expect("young pending offer lookup")
            .is_none(),
        "a young reservation may still belong to another live server"
    );

    let recovered = repositories
        .recover_pending_auth_pairing_offer(
            "principal".to_owned(),
            "interrupted-request".to_owned(),
            "same-input".to_owned(),
            T2.to_owned(),
            T2.to_owned(),
        )
        .await
        .expect("atomic pending-offer recovery");
    assert_eq!(recovered.as_deref(), Some("pairing-quota-17"));
    assert!(
        repositories
            .list_active_auth_pairing_links(T2.to_owned())
            .await
            .expect("abandoned grant lookup")
            .is_empty()
    );
    assert!(
        repositories
            .prune_and_get_active_auth_pairing_offer(
                "principal".to_owned(),
                "interrupted-request".to_owned(),
                T2.to_owned(),
            )
            .await
            .expect("released idempotency key lookup")
            .is_none()
    );
}

#[tokio::test]
async fn concurrent_pairing_offer_reservations_return_the_persisted_keyed_authority() {
    let (_temp, first, second) = migrated_repository_pair().await;
    let reserve = |repositories: Repositories, index| async move {
        repositories
            .create_auth_pairing_link_with_offer(
                pairing_offer_link(index),
                NewAuthPairingOffer {
                    principal_id: "principal".to_owned(),
                    idempotency_key: "shared-key".to_owned(),
                    input_fingerprint: "fingerprint".to_owned(),
                    expires_at: FUTURE.to_owned(),
                },
            )
            .await
    };

    let (first_result, second_result) = tokio::join!(reserve(first.clone(), 1), reserve(second, 2));
    let first_result = first_result.expect("first reservation outcome");
    let second_result = second_result.expect("second reservation outcome");
    assert_ne!(first_result.reserved, second_result.reserved);
    assert_eq!(
        first_result.offer.pairing_id,
        second_result.offer.pairing_id
    );
    assert_eq!(
        first
            .list_active_auth_pairing_links(T2.to_owned())
            .await
            .expect("active keyed pairing")
            .len(),
        1
    );
}

#[tokio::test]
async fn pairing_offer_writes_prune_expired_ledger_rows() {
    let repositories = migrated_repositories().await;
    for index in 0..3 {
        repositories
            .cancel_auth_pairing_offer(
                "principal".to_owned(),
                format!("expired-{index}"),
                T1.to_owned(),
                T1.to_owned(),
            )
            .await
            .expect("expired tombstone insert");
    }
    repositories
        .cancel_auth_pairing_offer(
            "principal".to_owned(),
            "live".to_owned(),
            T2.to_owned(),
            FUTURE.to_owned(),
        )
        .await
        .expect("live tombstone insert prunes expired rows");

    let rows = repositories
        .prune_and_list_active_auth_pairing_offers(T2.to_owned())
        .await
        .expect("bounded ledger");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].idempotency_key, "live");
}

#[tokio::test]
async fn pairing_offer_reservations_enforce_the_shared_principal_quota() {
    let (_temp, first, second) = migrated_repository_pair().await;
    for index in 0..128 {
        let repositories = if index % 2 == 0 { &first } else { &second };
        repositories
            .create_auth_pairing_link_with_offer(
                pairing_offer_link(index),
                NewAuthPairingOffer {
                    principal_id: "principal".to_owned(),
                    idempotency_key: format!("key-{index}"),
                    input_fingerprint: format!("fingerprint-{index}"),
                    expires_at: FUTURE.to_owned(),
                },
            )
            .await
            .expect("reservation within principal quota");
    }

    assert!(
        first
            .create_auth_pairing_link_with_offer(
                pairing_offer_link(128),
                NewAuthPairingOffer {
                    principal_id: "principal".to_owned(),
                    idempotency_key: "key-128".to_owned(),
                    input_fingerprint: "fingerprint-128".to_owned(),
                    expires_at: FUTURE.to_owned(),
                },
            )
            .await
            .is_err(),
        "the durable principal quota must reject reservation 129"
    );
    assert!(
        second
            .cancel_auth_pairing_offer(
                "principal".to_owned(),
                "cancelled-overflow".to_owned(),
                T2.to_owned(),
                FUTURE.to_owned(),
            )
            .await
            .is_err(),
        "a new cancellation tombstone must obey the durable principal quota"
    );
}

#[tokio::test]
async fn pairing_offer_reservations_enforce_the_shared_global_quota() {
    let (_temp, first, second) = migrated_repository_pair().await;
    for index in 0..4_096 {
        let repositories = if index % 2 == 0 { &first } else { &second };
        repositories
            .create_auth_pairing_link_with_offer(
                pairing_offer_link(index),
                NewAuthPairingOffer {
                    principal_id: format!("principal-{}", index / 128),
                    idempotency_key: format!("key-{index}"),
                    input_fingerprint: format!("fingerprint-{index}"),
                    expires_at: FUTURE.to_owned(),
                },
            )
            .await
            .expect("reservation within global quota");
    }

    assert!(
        first
            .create_auth_pairing_link_with_offer(
                pairing_offer_link(4_096),
                NewAuthPairingOffer {
                    principal_id: "overflow-principal".to_owned(),
                    idempotency_key: "overflow-key".to_owned(),
                    input_fingerprint: "overflow-fingerprint".to_owned(),
                    expires_at: FUTURE.to_owned(),
                },
            )
            .await
            .is_err(),
        "the durable global quota must reject reservation 4,097"
    );
    assert!(
        second
            .cancel_auth_pairing_offer(
                "overflow-principal".to_owned(),
                "cancelled-global-overflow".to_owned(),
                T2.to_owned(),
                FUTURE.to_owned(),
            )
            .await
            .is_err(),
        "a new cancellation tombstone must obey the durable global quota"
    );
}

fn pairing_offer_link(index: usize) -> AuthPairingLink {
    AuthPairingLink {
        id: format!("pairing-quota-{index}"),
        credential: format!("credential-quota-{index}"),
        method: "one-time-token".to_owned(),
        scopes: json!(["orchestration:read"]),
        subject: "one-time-token".to_owned(),
        label: Some(format!("Client {index}")),
        proof_key_thumbprint: None,
        created_at: T1.to_owned(),
        expires_at: FUTURE.to_owned(),
        consumed_at: None,
        revoked_at: None,
        reach: Some("another-device".to_owned()),
        off_host: Some(true),
    }
}

#[tokio::test]
async fn auth_pairing_reach_round_trips_through_persistence() {
    let repositories = migrated_repositories().await;
    repositories
        .create_auth_pairing_link(AuthPairingLink {
            id: "pairing-reach".to_owned(),
            credential: "credential-reach".to_owned(),
            method: "one-time-token".to_owned(),
            scopes: json!(["orchestration:read"]),
            subject: "one-time-token".to_owned(),
            label: Some("Tablet".to_owned()),
            proof_key_thumbprint: None,
            created_at: T1.to_owned(),
            expires_at: FUTURE.to_owned(),
            consumed_at: None,
            revoked_at: None,
            reach: Some("another-device".to_owned()),
            off_host: Some(true),
        })
        .await
        .expect("pairing insert");
    let links = repositories
        .list_active_auth_pairing_links(T2.to_owned())
        .await
        .expect("active pairing listing");
    assert_eq!(links[0].reach.as_deref(), Some("another-device"));
    assert_eq!(links[0].off_host, Some(true));

    repositories
        .create_auth_session(NewAuthSession {
            session_id: "session-reach".to_owned(),
            subject: "one-time-token".to_owned(),
            scopes: json!(["orchestration:read"]),
            method: "bearer-access-token".to_owned(),
            client: auth_client("Tablet"),
            issued_at: T1.to_owned(),
            expires_at: FUTURE.to_owned(),
            reach: Some("this-computer".to_owned()),
            off_host: Some(false),
            delivery_state: AuthSessionDeliveryState::Active,
        })
        .await
        .expect("session insert");
    let sessions = repositories
        .list_active_auth_sessions(T2.to_owned())
        .await
        .expect("active session listing");
    assert_eq!(sessions[0].reach.as_deref(), Some("this-computer"));
    assert_eq!(sessions[0].off_host, Some(false));
}

#[tokio::test]
async fn auth_sessions_round_trip_order_connect_and_revoke() {
    let repositories = migrated_repositories().await;
    let session_a = NewAuthSession {
        session_id: "session-a".to_owned(),
        subject: "user-a".to_owned(),
        scopes: json!(["rpc:read", "rpc:write", {"admin": true}]),
        method: "pairing".to_owned(),
        client: auth_client("Laptop"),
        issued_at: T1.to_owned(),
        expires_at: FUTURE.to_owned(),
        reach: None,
        off_host: None,
        delivery_state: AuthSessionDeliveryState::Active,
    };
    let session_b = NewAuthSession {
        session_id: "session-b".to_owned(),
        client: auth_client("Desktop"),
        issued_at: T2.to_owned(),
        ..session_a.clone()
    };
    let session_c = NewAuthSession {
        session_id: "session-c".to_owned(),
        client: auth_client("Browser"),
        issued_at: TIME_3.to_owned(),
        ..session_a.clone()
    };
    for session in [&session_a, &session_b, &session_c] {
        repositories
            .create_auth_session(session.clone())
            .await
            .expect("session insert");
    }
    let stored_a = repositories
        .get_auth_session("session-a".to_owned())
        .await
        .expect("session lookup")
        .expect("session exists");
    assert_eq!(stored_a.session_id, session_a.session_id);
    assert_eq!(stored_a.subject, session_a.subject);
    assert_eq!(stored_a.scopes, session_a.scopes);
    assert_row_eq(&stored_a.client, &session_a.client);
    assert_eq!(stored_a.last_connected_at, None);
    assert_eq!(stored_a.revoked_at, None);
    assert_eq!(
        repositories
            .list_active_auth_sessions(T0.to_owned())
            .await
            .expect("active session listing")
            .iter()
            .map(|session| session.session_id.as_str())
            .collect::<Vec<_>>(),
        ["session-c", "session-b", "session-a"]
    );

    repositories
        .set_auth_session_last_connected_at("session-a".to_owned(), TIME_3.to_owned())
        .await
        .expect("last connected update");
    assert_eq!(
        repositories
            .get_auth_session("session-a".to_owned())
            .await
            .expect("connected session lookup")
            .expect("connected session exists")
            .last_connected_at
            .as_deref(),
        Some(TIME_3)
    );
    assert!(
        repositories
            .revoke_auth_session("session-b".to_owned(), TIME_3.to_owned())
            .await
            .expect("session revocation")
    );
    assert!(
        !repositories
            .revoke_auth_session("session-b".to_owned(), TIME_3.to_owned())
            .await
            .expect("session revocation is idempotent")
    );
    repositories
        .set_auth_session_last_connected_at("session-b".to_owned(), FUTURE.to_owned())
        .await
        .expect("revoked session connection update is ignored");
    let revoked_b = repositories
        .get_auth_session("session-b".to_owned())
        .await
        .expect("revoked session lookup")
        .expect("revoked session exists");
    assert_eq!(revoked_b.last_connected_at, None);
    assert_eq!(revoked_b.revoked_at.as_deref(), Some(TIME_3));

    let mut revoked_others = repositories
        .revoke_other_auth_sessions("session-a".to_owned(), FUTURE.to_owned())
        .await
        .expect("other sessions revoked");
    revoked_others.sort();
    assert_eq!(revoked_others, ["session-c"]);
    assert_eq!(
        repositories
            .list_active_auth_sessions(T0.to_owned())
            .await
            .expect("only current session active")
            .iter()
            .map(|session| session.session_id.as_str())
            .collect::<Vec<_>>(),
        ["session-a"]
    );
    assert!(
        repositories
            .revoke_other_auth_sessions("session-a".to_owned(), FUTURE.to_owned())
            .await
            .expect("revoke others is idempotent")
            .is_empty()
    );
    assert!(
        repositories
            .get_auth_session("missing".to_owned())
            .await
            .expect("missing session lookup")
            .is_none()
    );

    let storage_type = repositories
        .database()
        .call(|connection| {
            Ok(connection.query_row(
                "SELECT typeof(scopes) FROM auth_sessions WHERE session_id = 'session-a'",
                [],
                |row| row.get::<_, String>(0),
            )?)
        })
        .await
        .expect("auth scope storage type");
    assert_eq!(storage_type, "text");
}

#[tokio::test]
async fn pending_auth_sessions_confirm_by_id_and_startup_cleanup_is_selective() {
    let repositories = migrated_repositories().await;
    let session = |session_id: &str, delivery_state| NewAuthSession {
        session_id: session_id.to_owned(),
        subject: "paired-device".to_owned(),
        scopes: json!(["orchestration:read"]),
        method: "bearer-access-token".to_owned(),
        client: auth_client(session_id),
        issued_at: T1.to_owned(),
        expires_at: FUTURE.to_owned(),
        reach: Some("another-device".to_owned()),
        off_host: Some(true),
        delivery_state,
    };
    repositories
        .create_auth_session(session(
            "pending-confirmed",
            AuthSessionDeliveryState::PendingPairing,
        ))
        .await
        .expect("pending session insert");
    repositories
        .create_auth_session(session(
            "pending-abandoned",
            AuthSessionDeliveryState::PendingPairing,
        ))
        .await
        .expect("second pending session insert");
    repositories
        .create_auth_session(session("active-session", AuthSessionDeliveryState::Active))
        .await
        .expect("active session insert");
    repositories
        .create_auth_session(NewAuthSession {
            expires_at: T2.to_owned(),
            ..session("pending-expired", AuthSessionDeliveryState::PendingPairing)
        })
        .await
        .expect("expired pending session insert");

    let active_ids = repositories
        .list_active_auth_sessions(T2.to_owned())
        .await
        .expect("active session listing")
        .into_iter()
        .map(|session| session.session_id)
        .collect::<Vec<_>>();
    assert!(active_ids.contains(&"pending-confirmed".to_owned()));
    assert!(active_ids.contains(&"pending-abandoned".to_owned()));
    assert!(active_ids.contains(&"active-session".to_owned()));
    assert!(!active_ids.contains(&"pending-expired".to_owned()));

    assert_eq!(
        repositories
            .get_auth_session("pending-confirmed".to_owned())
            .await
            .expect("pending lookup")
            .expect("pending exists")
            .delivery_state,
        AuthSessionDeliveryState::PendingPairing,
    );
    assert!(
        repositories
            .confirm_pending_auth_session("pending-confirmed".to_owned(), T2.to_owned())
            .await
            .expect("pending confirmation")
    );
    assert!(
        repositories
            .confirm_pending_auth_session("pending-confirmed".to_owned(), T2.to_owned())
            .await
            .expect("confirmation is idempotent")
    );
    assert!(
        !repositories
            .confirm_pending_auth_session("missing".to_owned(), T2.to_owned())
            .await
            .expect("missing confirmation is a miss")
    );

    let mut revoked = repositories
        .revoke_pending_auth_sessions(TIME_3.to_owned(), TIME_3.to_owned())
        .await
        .expect("startup pending cleanup");
    revoked.sort();
    assert_eq!(revoked, ["pending-abandoned", "pending-expired"]);
    assert_eq!(
        repositories
            .get_auth_session("pending-confirmed".to_owned())
            .await
            .expect("confirmed lookup")
            .expect("confirmed exists")
            .delivery_state,
        AuthSessionDeliveryState::Active,
    );
    assert_eq!(
        repositories
            .get_auth_session("active-session".to_owned())
            .await
            .expect("active lookup")
            .expect("active exists")
            .revoked_at,
        None,
    );
    assert_eq!(
        repositories
            .get_auth_session("pending-abandoned".to_owned())
            .await
            .expect("abandoned lookup")
            .expect("abandoned exists")
            .revoked_at
            .as_deref(),
        Some(TIME_3),
    );
}

#[tokio::test]
async fn active_work_counts_running_sessions_and_every_queued_message() {
    let repositories = migrated_repositories().await;
    assert_eq!(repositories.count_active_work().await.unwrap(), (0, 0));
    for (thread_id, status) in [
        ("busy", "running"),
        ("booting", "starting"),
        ("waiting", "ready"),
    ] {
        repositories
            .upsert_thread_session(ProjectionThreadSession {
                thread_id: thread_id.into(),
                status: status.into(),
                provider_name: Some("codex".into()),
                provider_instance_id: Some("codex".into()),
                runtime_mode: "approval-required".into(),
                active_turn_id: None,
                last_error: None,
                last_error_class: None,
                updated_at: T0.into(),
            })
            .await
            .unwrap();
    }
    repositories.database().call(|connection| {
        for (command, state) in [("q1", "queued"), ("q2", "queued"), ("sent", "delivered")] {
            connection.execute("INSERT INTO orchestration_command_receipts (command_id, aggregate_kind, aggregate_id, accepted_at, result_sequence, status) VALUES (?, 'thread', 'busy', ?, 0, 'accepted')", [command, T0])?;
            connection.execute("INSERT INTO provider_turn_outbox (command_id, thread_id, message_id, provider_instance_id, provider_kind, delivery_key, payload_json, state, mode, held, attempts, last_error, created_at, updated_at) VALUES (?, 'busy', ?, 'codex', 'codex', ?, '{}', ?, 'start', 0, 0, NULL, ?, ?)", [command, command, command, state, T0, T1])?;
        }
        Ok(())
    }).await.unwrap();
    assert_eq!(repositories.count_active_work().await.unwrap(), (2, 2));
}

#[tokio::test]
async fn requeue_provider_turn_is_attempt_conditioned_and_preserves_payload() {
    let repositories = migrated_repositories().await;
    repositories.database().call(|connection| {
        connection.execute("INSERT INTO orchestration_command_receipts (command_id, aggregate_kind, aggregate_id, accepted_at, result_sequence, status) VALUES ('steer', 'thread', 'thread', ?, 0, 'accepted')", [T0])?;
        connection.execute("INSERT INTO provider_turn_outbox (command_id, thread_id, message_id, provider_instance_id, provider_kind, delivery_key, payload_json, state, mode, held, attempts, last_error, created_at, updated_at) VALUES ('steer', 'thread', 'message', 'codex', 'codex', 'key', '{\"saved\":true}', 'sending', 'steer', 0, 2, 'no active turn', ?, ?)", [T0, T1])?;
        Ok(())
    }).await.unwrap();
    assert!(
        repositories
            .requeue_provider_turn("steer".into(), 1, T2.into())
            .await
            .unwrap()
            .is_none()
    );
    let row = repositories
        .requeue_provider_turn("steer".into(), 2, T2.into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, TurnDeliveryState::Queued);
    assert_eq!(
        row.mode,
        coddy_studio_server::orchestration::TurnDeliveryMode::Start
    );
    assert_eq!(row.payload, json!({"saved":true}));
    assert_eq!(row.message_id, "message");
    assert_eq!(row.delivery_key, "key");
    assert_eq!(row.created_at, T0);
    assert_eq!(row.attempts, 0);
    assert_eq!(row.last_error, None);
    assert!(
        repositories
            .requeue_provider_turn("steer".into(), 0, T2.into())
            .await
            .unwrap()
            .is_none()
    );
}
