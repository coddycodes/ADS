//! SQLite repositories for the application tables created by `migrations`.
//!
//! Timestamps and identifiers deliberately remain strings: SQLite stores the
//! TypeScript implementation's ISO timestamps as `TEXT`, and these APIs must
//! not normalize or regenerate them while reading an existing database.

use std::cmp::min;
use std::collections::HashMap;
#[cfg(test)]
use std::sync::{
    Arc, Mutex as StdMutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    auth::limits::{MAX_ACTIVE_PAIRING_OFFERS, MAX_ACTIVE_PAIRING_OFFERS_PER_PRINCIPAL},
    orchestration::{ProviderTurnDelivery, TurnDeliveryMode, TurnDeliveryState},
};

use super::{Database, PersistenceError, Result};

pub type Timestamp = String;

/// How many events one page holds when a caller walks the event log.
pub const EVENT_PAGE_SIZE: usize = 128;

/// Walks the event log after an exclusive sequence cursor, one bounded read at a time, so a
/// caller never holds more than [`EVENT_PAGE_SIZE`] events.
pub struct EventPages<'a> {
    repositories: &'a Repositories,
    cursor: i64,
}

impl EventPages<'_> {
    /// Returns the next page after the cursor, or `None` once a read finds no newer event.
    pub async fn next_page(&mut self) -> Result<Option<Vec<OrchestrationEvent>>> {
        let page = self
            .repositories
            .read_events_from_sequence(self.cursor, EVENT_PAGE_SIZE)
            .await?;
        let Some(last) = page.last() else {
            return Ok(None);
        };
        self.cursor = last.sequence;
        Ok(Some(page))
    }
}

#[derive(Clone, Debug)]
pub struct Repositories {
    database: Database,
    #[cfg(test)]
    delivery_test_hooks: Arc<DeliveryRepositoryTestHooks>,
}

#[cfg(test)]
#[derive(Debug, Default)]
struct DeliveryRepositoryTestHooks {
    fail_reads: AtomicBool,
    claim_calls: AtomicUsize,
    pause_after_next_read: StdMutex<Option<ProviderTurnReadPause>>,
    pause_after_next_claim: StdMutex<Option<ProviderTurnReadPause>>,
}

#[cfg(test)]
#[derive(Clone, Debug)]
pub(crate) struct ProviderTurnReadPause {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[cfg(test)]
impl ProviderTurnReadPause {
    pub(crate) async fn wait_until_entered(&self) {
        self.entered.notified().await;
    }

    pub(crate) fn release(&self) {
        self.release.notify_one();
    }
}

impl Repositories {
    pub fn new(database: Database) -> Self {
        Self {
            database,
            #[cfg(test)]
            delivery_test_hooks: Arc::new(DeliveryRepositoryTestHooks::default()),
        }
    }

    pub fn database(&self) -> &Database {
        &self.database
    }

    pub async fn append_event(&self, event: NewOrchestrationEvent) -> Result<OrchestrationEvent> {
        self.database
            .call(move |connection| {
                connection.query_row(
                    "INSERT INTO orchestration_events ( \
                       event_id, aggregate_kind, stream_id, stream_version, event_type, occurred_at, \
                       command_id, causation_event_id, correlation_id, actor_kind, payload_json, metadata_json \
                     ) VALUES (?, ?, ?, COALESCE(( \
                       SELECT stream_version + 1 FROM orchestration_events \
                       WHERE aggregate_kind = ? AND stream_id = ? \
                       ORDER BY stream_version DESC LIMIT 1 \
                     ), 0), ?, ?, ?, ?, ?, ?, ?, ?) \
                     RETURNING sequence, event_id, event_type, aggregate_kind, stream_id, occurred_at, \
                       command_id, causation_event_id, correlation_id, payload_json, metadata_json",
                    params![
                        event.event_id,
                        event.aggregate_kind,
                        event.aggregate_id,
                        event.aggregate_kind,
                        event.aggregate_id,
                        event.event_type,
                        event.occurred_at,
                        event.command_id,
                        event.causation_event_id,
                        event.correlation_id,
                        infer_actor_kind(&event),
                        encode_json(&event.payload)?,
                        encode_json(&event.metadata)?,
                    ],
                    decode_event,
                ).map_err(Into::into)
            })
            .await
    }

    /// Reads at most `limit` events after an exclusive sequence cursor.  Callers
    /// stream large replays by advancing the cursor to the last returned event.
    /// [`Repositories::event_pages`] walks the log that way in [`EVENT_PAGE_SIZE`] pages.
    pub async fn read_events_from_sequence(
        &self,
        sequence_exclusive: i64,
        limit: usize,
    ) -> Result<Vec<OrchestrationEvent>> {
        let limit = min(limit, i64::MAX as usize) as i64;
        self.database
            .call(move |connection| {
                let mut statement = connection.prepare(
                    "SELECT sequence, event_id, event_type, aggregate_kind, stream_id, occurred_at, \
                       command_id, causation_event_id, correlation_id, payload_json, metadata_json \
                     FROM orchestration_events \
                     WHERE sequence > ? \
                     ORDER BY sequence ASC LIMIT ?",
                )?;
                statement
                    .query_map(params![sequence_exclusive.max(0), limit], decode_event)?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(Into::into)
            })
            .await
    }

    /// Starts walking the event log after `sequence_exclusive`; see [`EventPages`].
    pub fn event_pages(&self, sequence_exclusive: i64) -> EventPages<'_> {
        EventPages {
            repositories: self,
            cursor: sequence_exclusive,
        }
    }

    pub async fn max_event_sequence(&self) -> Result<i64> {
        self.database
            .call(|connection| {
                connection
                    .query_row(
                        "SELECT COALESCE(MAX(sequence), 0) FROM orchestration_events",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(Into::into)
            })
            .await
    }

    pub async fn upsert_command_receipt(&self, row: CommandReceipt) -> Result<()> {
        self.database.call(move |connection| {
            connection.execute(
                "INSERT INTO orchestration_command_receipts ( \
                   command_id, aggregate_kind, aggregate_id, accepted_at, result_sequence, status, error, payload_digest \
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT (command_id) DO UPDATE SET \
                   aggregate_kind = excluded.aggregate_kind, aggregate_id = excluded.aggregate_id, \
                   accepted_at = excluded.accepted_at, result_sequence = excluded.result_sequence, \
                   status = excluded.status, error = excluded.error, payload_digest = excluded.payload_digest",
                params![row.command_id, row.aggregate_kind, row.aggregate_id, row.accepted_at, row.result_sequence, row.status, row.error, row.payload_digest],
            )?;
            Ok(())
        }).await
    }

    /// Finalizes a command only when its identity is still absent or held by the exact matching
    /// reservation. An accepted/rejected or differently owned receipt is never overwritten.
    pub async fn finalize_command_receipt(&self, row: CommandReceipt) -> Result<()> {
        self.database
            .call(move |connection| finalize_command_receipt_on(connection, row))
            .await
    }

    pub async fn get_command_receipt(&self, command_id: String) -> Result<Option<CommandReceipt>> {
        self.database.call(move |connection| connection.query_row(
            "SELECT command_id, aggregate_kind, aggregate_id, accepted_at, result_sequence, status, error, payload_digest \
             FROM orchestration_command_receipts WHERE command_id = ?",
            [command_id], decode_command_receipt).optional().map_err(Into::into)).await
    }

    pub async fn prepare_worktree_removal_receipt(
        &self,
        row: WorktreeRemovalReceipt,
    ) -> Result<WorktreeRemovalReceipt> {
        self.database
            .call(move |connection| {
                connection.execute(
                    "INSERT OR IGNORE INTO worktree_removal_receipts (\
                       owner_thread_id, project_cwd, worktree_path, identity_nonce, state, created_at, updated_at\
                     ) VALUES (?, ?, ?, ?, 'prepared', \
                       strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
                    params![
                        &row.owner_thread_id,
                        &row.project_cwd,
                        &row.worktree_path,
                        &row.identity_nonce,
                    ],
                )?;
                connection
                    .query_row(
                        "SELECT owner_thread_id, project_cwd, worktree_path, identity_nonce, state, created_at, updated_at \
                         FROM worktree_removal_receipts WHERE owner_thread_id = ?",
                        [row.owner_thread_id],
                        decode_worktree_removal_receipt,
                    )
                    .map_err(Into::into)
            })
            .await
    }

    pub async fn get_worktree_removal_receipt(
        &self,
        owner_thread_id: String,
    ) -> Result<Option<WorktreeRemovalReceipt>> {
        self.database
            .call(move |connection| {
                connection
                    .query_row(
                        "SELECT owner_thread_id, project_cwd, worktree_path, identity_nonce, state, created_at, updated_at \
                         FROM worktree_removal_receipts WHERE owner_thread_id = ?",
                        [owner_thread_id],
                        decode_worktree_removal_receipt,
                    )
                    .optional()
                    .map_err(Into::into)
            })
            .await
    }

    pub async fn complete_worktree_removal_receipt(
        &self,
        owner_thread_id: String,
        identity_nonce: String,
    ) -> Result<()> {
        self.database
            .call(move |connection| {
                let updated = connection.execute(
                    "UPDATE worktree_removal_receipts SET state = 'removed', \
                       updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
                     WHERE owner_thread_id = ? AND identity_nonce = ? AND state = 'prepared'",
                    params![owner_thread_id, identity_nonce],
                )?;
                if updated != 1 {
                    return Err(PersistenceError::Corrupt(
                        "worktree removal receipt changed before completion".to_owned(),
                    ));
                }
                Ok(())
            })
            .await
    }

    pub async fn upsert_checkpoint_diff_blob(&self, row: CheckpointDiffBlob) -> Result<()> {
        self.database.call(move |connection| {
            connection.execute(
                "INSERT INTO checkpoint_diff_blobs (thread_id, from_turn_count, to_turn_count, diff, created_at) \
                 VALUES (?, ?, ?, ?, ?) \
                 ON CONFLICT (thread_id, from_turn_count, to_turn_count) DO UPDATE SET \
                   diff = excluded.diff, created_at = excluded.created_at",
                params![row.thread_id, row.from_turn_count, row.to_turn_count, row.diff, row.created_at],
            )?;
            Ok(())
        }).await
    }

    pub async fn list_checkpoint_diff_blobs_by_thread(
        &self,
        thread_id: String,
    ) -> Result<Vec<CheckpointDiffBlob>> {
        self.database.call(move |connection| collect(
            connection,
            "SELECT thread_id, from_turn_count, to_turn_count, diff, created_at FROM checkpoint_diff_blobs WHERE thread_id = ? ORDER BY to_turn_count ASC",
            [thread_id],
            decode_checkpoint_diff_blob,
        )).await
    }

    pub async fn upsert_provider_session_runtime(&self, row: ProviderSessionRuntime) -> Result<()> {
        self.database.call(move |connection| {
            connection.execute(
                "INSERT INTO provider_session_runtime ( \
                   thread_id, provider_name, provider_instance_id, adapter_key, runtime_mode, status, \
                   last_seen_at, resume_cursor_json, runtime_payload_json \
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT (thread_id) DO UPDATE SET \
                   provider_name = excluded.provider_name, provider_instance_id = excluded.provider_instance_id, \
                   adapter_key = excluded.adapter_key, runtime_mode = excluded.runtime_mode, \
                   status = excluded.status, last_seen_at = excluded.last_seen_at, \
                   resume_cursor_json = excluded.resume_cursor_json, runtime_payload_json = excluded.runtime_payload_json",
                params![row.thread_id, row.provider_name, row.provider_instance_id, row.adapter_key, row.runtime_mode, row.status, row.last_seen_at, optional_json(&row.resume_cursor)?, optional_json(&row.runtime_payload)?],
            )?;
            Ok(())
        }).await
    }

    /// Preserve resumable state only while its thread still exists and is not deleted.
    /// The existence check covers both insertion and conflict updates in one statement.
    pub(crate) async fn upsert_provider_session_runtime_if_thread_live(
        &self,
        row: ProviderSessionRuntime,
    ) -> Result<bool> {
        self.database.call(move |connection| {
            let written = connection.execute(
                "INSERT INTO provider_session_runtime ( \
                   thread_id, provider_name, provider_instance_id, adapter_key, runtime_mode, status, \
                   last_seen_at, resume_cursor_json, runtime_payload_json \
                 ) SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9 \
                   WHERE EXISTS (SELECT 1 FROM projection_threads WHERE thread_id = ?1 AND deleted_at IS NULL) \
                 ON CONFLICT (thread_id) DO UPDATE SET \
                   provider_name = excluded.provider_name, provider_instance_id = excluded.provider_instance_id, \
                   adapter_key = excluded.adapter_key, runtime_mode = excluded.runtime_mode, \
                   status = excluded.status, last_seen_at = excluded.last_seen_at, \
                   resume_cursor_json = excluded.resume_cursor_json, runtime_payload_json = excluded.runtime_payload_json",
                params![row.thread_id, row.provider_name, row.provider_instance_id, row.adapter_key, row.runtime_mode, row.status, row.last_seen_at, optional_json(&row.resume_cursor)?, optional_json(&row.runtime_payload)?],
            )?;
            Ok(written != 0)
        }).await
    }

    /// Links resumable state to a live thread only when the thread has none yet and has not
    /// admitted a turn or user message, so a conversation the thread started is never replaced.
    pub(crate) async fn insert_provider_session_runtime_if_absent_for_live_thread(
        &self,
        row: ProviderSessionRuntime,
    ) -> Result<bool> {
        self.database.call(move |connection| {
            let written = connection.execute(
                "INSERT INTO provider_session_runtime ( \
                   thread_id, provider_name, provider_instance_id, adapter_key, runtime_mode, status, \
                   last_seen_at, resume_cursor_json, runtime_payload_json \
                 ) SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9 \
                   WHERE EXISTS (SELECT 1 FROM projection_threads WHERE thread_id = ?1 AND deleted_at IS NULL \
                   AND latest_turn_id IS NULL AND latest_user_message_at IS NULL) \
                 ON CONFLICT (thread_id) DO NOTHING",
                params![row.thread_id, row.provider_name, row.provider_instance_id, row.adapter_key, row.runtime_mode, row.status, row.last_seen_at, optional_json(&row.resume_cursor)?, optional_json(&row.runtime_payload)?],
            )?;
            Ok(written != 0)
        }).await
    }

    /// Native provider session IDs mapped to a thread that delivered turns to them or can
    /// resume them.
    pub(crate) async fn list_provider_session_owners(&self) -> Result<Vec<(String, String)>> {
        self.database
            .call(|connection| {
                collect(
                    connection,
                    "SELECT provider_session_id, thread_id FROM provider_turn_outbox \
                       WHERE provider_session_id IS NOT NULL \
                     UNION SELECT json_extract(resume_cursor_json, '$.sessionId'), thread_id \
                       FROM provider_session_runtime \
                       WHERE json_type(resume_cursor_json, '$.sessionId') = 'text' \
                     UNION SELECT json_extract(resume_cursor_json, '$.threadId'), thread_id \
                       FROM provider_session_runtime \
                       WHERE json_type(resume_cursor_json, '$.threadId') = 'text'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
            })
            .await
    }

    pub async fn get_provider_session_runtime(
        &self,
        thread_id: String,
    ) -> Result<Option<ProviderSessionRuntime>> {
        self.database.call(move |connection| connection.query_row(
            "SELECT thread_id, provider_name, provider_instance_id, adapter_key, runtime_mode, status, \
             last_seen_at, resume_cursor_json, runtime_payload_json FROM provider_session_runtime \
             WHERE thread_id = ?", [thread_id], decode_provider_runtime).optional().map_err(Into::into)).await
    }

    pub async fn list_provider_session_runtimes(&self) -> Result<Vec<ProviderSessionRuntime>> {
        self.database.call(|connection| collect(connection, "SELECT thread_id, provider_name, provider_instance_id, adapter_key, runtime_mode, status, last_seen_at, resume_cursor_json, runtime_payload_json FROM provider_session_runtime ORDER BY last_seen_at ASC, thread_id ASC", [], decode_provider_runtime)).await
    }

    pub async fn delete_provider_session_runtime(&self, thread_id: String) -> Result<()> {
        self.delete(
            "DELETE FROM provider_session_runtime WHERE thread_id = ?",
            thread_id,
        )
        .await
    }

    pub async fn upsert_project(&self, row: ProjectionProject) -> Result<()> {
        self.database.call(move |connection| {
            connection.execute(
                "INSERT INTO projection_projects (project_id, title, workspace_root, default_model_selection_json, scripts_json, worktree_discovery_json, created_at, updated_at, deleted_at, repository_identity_json) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT (project_id) DO UPDATE SET \
                   title=excluded.title, workspace_root=excluded.workspace_root, \
                   default_model_selection_json=excluded.default_model_selection_json, scripts_json=excluded.scripts_json, worktree_discovery_json=excluded.worktree_discovery_json, \
                   created_at=excluded.created_at, updated_at=excluded.updated_at, deleted_at=excluded.deleted_at, repository_identity_json=excluded.repository_identity_json",
                params![row.project_id, row.title, row.workspace_root, optional_json(&row.default_model_selection)?, encode_json(&row.scripts)?, encode_json(&row.worktree_discovery)?, row.created_at, row.updated_at, row.deleted_at, optional_json(&row.repository_identity)?],
            )?; Ok(())
        }).await
    }

    pub async fn get_project(&self, project_id: String) -> Result<Option<ProjectionProject>> {
        self.database
            .call(move |connection| {
                connection
                    .query_row(
                        &format!("{PROJECT_SELECT} WHERE project_id = ?"),
                        [project_id],
                        decode_project,
                    )
                    .optional()
                    .map_err(Into::into)
            })
            .await
    }

    pub async fn list_projects(&self) -> Result<Vec<ProjectionProject>> {
        self.database
            .call(|connection| {
                collect(
                    connection,
                    &format!("{PROJECT_SELECT}{PROJECT_LIST_ORDER}"),
                    [],
                    decode_project,
                )
            })
            .await
    }

    /// Reads, in one SQLite job, the rows a live (`archived == false`) or archived shell
    /// snapshot shows. Threads must be undeleted and have a project row. Previews need only
    /// the newest rows, so for each thread this reads every message tied for the newest
    /// `user` and `assistant` message, and, when the latest turn is running, every `tool`
    /// activity tied for its newest; ties keep the per-thread list order.
    pub async fn load_shell_snapshot_rows(&self, archived: bool) -> Result<ShellSnapshotRows> {
        self.database
            .call(move |connection| {
                let archive_filter = if archived {
                    "archived_at IS NOT NULL"
                } else {
                    "archived_at IS NULL"
                };
                let mut rows = ShellSnapshotRows {
                    projects: collect(
                        connection,
                        &format!("{PROJECT_SELECT}{PROJECT_LIST_ORDER}"),
                        [],
                        decode_project,
                    )?,
                    threads: collect(
                        connection,
                        &format!(
                            "{THREAD_SELECT} WHERE deleted_at IS NULL AND {archive_filter} \
                             AND project_id IN (SELECT project_id FROM projection_projects) \
                             ORDER BY thread_id ASC"
                        ),
                        [],
                        decode_thread,
                    )?,
                    sessions: Vec::new(),
                    latest_turns: Vec::new(),
                    preview_messages: Vec::new(),
                    preview_activities: Vec::new(),
                    states: collect(
                        connection,
                        PROJECTION_STATE_LIST,
                        [],
                        decode_projection_state,
                    )?,
                };
                let mut session =
                    connection.prepare(&format!("{THREAD_SESSION_SELECT} WHERE thread_id = ?"))?;
                let mut newest_messages = connection.prepare(&format!(
                    "{MESSAGE_SELECT} WHERE thread_id = ?1 AND role = ?2 AND created_at = \
                     (SELECT created_at FROM projection_thread_messages WHERE thread_id = ?1 \
                      AND role = ?2 ORDER BY created_at DESC LIMIT 1){MESSAGE_LIST_ORDER}"
                ))?;
                // (thread_id, turn_id) is unique, so this is at most one row.
                let mut latest_turn = connection.prepare(&format!(
                    "{TURN_SELECT} WHERE thread_id = ? AND turn_id = ?"
                ))?;
                let mut newest_tools = connection.prepare(&format!(
                    "{ACTIVITY_SELECT} WHERE thread_id = ?1 AND turn_id = ?2 AND tone = 'tool' \
                     AND created_at = (SELECT created_at FROM projection_thread_activities \
                      WHERE thread_id = ?1 AND turn_id = ?2 AND tone = 'tool' \
                      ORDER BY created_at DESC LIMIT 1){ACTIVITY_LIST_ORDER}"
                ))?;
                for thread in &rows.threads {
                    rows.sessions.extend(
                        session
                            .query_row([&thread.thread_id], decode_thread_session)
                            .optional()?,
                    );
                    for role in ["user", "assistant"] {
                        for message in newest_messages
                            .query_map(params![thread.thread_id, role], decode_message)?
                        {
                            rows.preview_messages.push(message?);
                        }
                    }
                    let Some(turn_id) = &thread.latest_turn_id else {
                        continue;
                    };
                    let Some(turn) = latest_turn
                        .query_row(params![thread.thread_id, turn_id], decode_turn)
                        .optional()?
                    else {
                        continue;
                    };
                    if turn.state == "running" {
                        for activity in newest_tools
                            .query_map(params![thread.thread_id, turn_id], decode_activity)?
                        {
                            rows.preview_activities.push(activity?);
                        }
                    }
                    rows.latest_turns.push(turn);
                }
                Ok(rows)
            })
            .await
    }

    /// Establishes the durable repository identity exactly once and then compares only.
    pub async fn pin_project_worktree_repository_key(
        &self,
        project_id: String,
        repository_key: String,
    ) -> Result<Option<WorktreeRepositoryPinOutcome>> {
        self.database
            .call(move |connection| {
                let established = connection.execute(
                    "INSERT INTO project_worktree_repository_pins (project_id, repository_key) \
                     SELECT ?, ? WHERE EXISTS (SELECT 1 FROM projection_projects WHERE project_id = ?) \
                     ON CONFLICT (project_id) DO NOTHING",
                    params![project_id, repository_key, project_id],
                )?;
                let pinned = connection
                    .query_row(
                        "SELECT pins.repository_key FROM projection_projects AS projects \
                         LEFT JOIN project_worktree_repository_pins AS pins USING (project_id) \
                         WHERE projects.project_id = ?",
                        [&project_id],
                        |row| row.get::<_, Option<String>>(0),
                    )
                    .optional()?;
                let Some(pinned) = pinned else {
                    return Ok(None);
                };
                let Some(pinned_repository_key) = pinned else {
                    return Ok(None);
                };
                if established == 1 {
                    Ok(Some(WorktreeRepositoryPinOutcome::Established))
                } else if pinned_repository_key == repository_key {
                    Ok(Some(WorktreeRepositoryPinOutcome::Matched))
                } else {
                    Ok(Some(WorktreeRepositoryPinOutcome::Mismatch {
                        pinned_repository_key,
                    }))
                }
            })
            .await
    }

    /// Atomically establishes an immutable command identity without overwriting an existing
    /// receipt. A matching reserved receipt is intentionally resumable after interruption.
    pub async fn reserve_command_receipt(
        &self,
        row: CommandReceipt,
    ) -> Result<(CommandReceipt, bool)> {
        self.database
            .call(move |connection| {
                let inserted = connection.execute(
                    "INSERT OR IGNORE INTO orchestration_command_receipts (\
                       command_id, aggregate_kind, aggregate_id, accepted_at, result_sequence, status, error, payload_digest\
                     ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                    params![
                        row.command_id,
                        row.aggregate_kind,
                        row.aggregate_id,
                        row.accepted_at,
                        row.result_sequence,
                        row.status,
                        row.error,
                        row.payload_digest
                    ],
                )?;
                let receipt = connection
                    .query_row(
                        "SELECT command_id, aggregate_kind, aggregate_id, accepted_at, result_sequence, status, error, payload_digest \
                         FROM orchestration_command_receipts WHERE command_id = ?",
                        [&row.command_id],
                        decode_command_receipt,
                    )
                    .map_err(crate::persistence::PersistenceError::from)?;
                Ok((receipt, inserted == 1))
            })
            .await
    }

    pub async fn release_reserved_command_receipt(
        &self,
        command_id: String,
        aggregate_kind: String,
        aggregate_id: String,
        payload_digest: String,
    ) -> Result<bool> {
        self.database
            .call(move |connection| {
                connection
                    .execute(
                        "DELETE FROM orchestration_command_receipts \
                         WHERE command_id = ? AND aggregate_kind = ? AND aggregate_id = ? \
                           AND payload_digest = ? AND status = 'reserved'",
                        params![command_id, aggregate_kind, aggregate_id, payload_digest],
                    )
                    .map(|deleted| deleted == 1)
                    .map_err(Into::into)
            })
            .await
    }

    pub async fn prepare_reserved_command_receipt(
        &self,
        command_id: String,
        payload_digest: String,
    ) -> Result<Option<CommandReceipt>> {
        self.database
            .call(move |connection| {
                connection.execute(
                    "UPDATE orchestration_command_receipts SET status = 'prepared' \
                     WHERE command_id = ? AND payload_digest = ? AND status = 'reserved'",
                    params![command_id, payload_digest],
                )?;
                connection
                    .query_row(
                        "SELECT command_id, aggregate_kind, aggregate_id, accepted_at, result_sequence, status, error, payload_digest \
                         FROM orchestration_command_receipts WHERE command_id = ?",
                        [&command_id],
                        decode_command_receipt,
                    )
                    .optional()
                    .map_err(Into::into)
            })
            .await
    }

    pub async fn verify_prepared_command_receipt(
        &self,
        command_id: String,
        aggregate_kind: String,
        aggregate_id: String,
        payload_digest: String,
    ) -> Result<bool> {
        self.database
            .call(move |connection| {
                connection
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM orchestration_command_receipts \
                         WHERE command_id = ? AND aggregate_kind = ? AND aggregate_id = ? \
                           AND payload_digest = ? AND status = 'prepared')",
                        params![command_id, aggregate_kind, aggregate_id, payload_digest],
                        |row| row.get(0),
                    )
                    .map_err(Into::into)
            })
            .await
    }

    pub async fn delete_project(&self, project_id: String) -> Result<()> {
        self.delete(
            "DELETE FROM projection_projects WHERE project_id = ?",
            project_id,
        )
        .await
    }

    pub async fn upsert_thread(&self, row: ProjectionThread) -> Result<()> {
        self.database.call(move |connection| {
            connection.execute(
                "INSERT INTO projection_threads (thread_id, project_id, title, kind, model_selection_json, runtime_mode, interaction_mode, branch, worktree_path, latest_turn_id, created_at, updated_at, archived_at, latest_user_message_at, pending_approval_count, pending_user_input_count, has_actionable_proposed_plan, deleted_at, host_thread_id) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT (thread_id) DO UPDATE SET \
                   project_id=excluded.project_id, title=excluded.title, kind=excluded.kind, model_selection_json=excluded.model_selection_json, runtime_mode=excluded.runtime_mode, interaction_mode=excluded.interaction_mode, branch=excluded.branch, worktree_path=excluded.worktree_path, latest_turn_id=excluded.latest_turn_id, created_at=excluded.created_at, updated_at=excluded.updated_at, archived_at=excluded.archived_at, latest_user_message_at=excluded.latest_user_message_at, pending_approval_count=excluded.pending_approval_count, pending_user_input_count=excluded.pending_user_input_count, has_actionable_proposed_plan=excluded.has_actionable_proposed_plan, deleted_at=excluded.deleted_at, host_thread_id=excluded.host_thread_id",
                params![row.thread_id,row.project_id,row.title,row.kind,encode_json(&row.model_selection)?,row.runtime_mode,row.interaction_mode,row.branch,row.worktree_path,row.latest_turn_id,row.created_at,row.updated_at,row.archived_at,row.latest_user_message_at,row.pending_approval_count,row.pending_user_input_count,row.has_actionable_proposed_plan,row.deleted_at,row.host_thread_id],
            )?; Ok(())
        }).await
    }

    pub async fn get_thread(&self, thread_id: String) -> Result<Option<ProjectionThread>> {
        self.database
            .call(move |connection| {
                connection
                    .query_row(
                        &(THREAD_SELECT.to_owned() + " WHERE thread_id = ?"),
                        [thread_id],
                        decode_thread,
                    )
                    .optional()
                    .map_err(Into::into)
            })
            .await
    }

    pub async fn list_threads_by_project(
        &self,
        project_id: String,
    ) -> Result<Vec<ProjectionThread>> {
        self.database
            .call(move |connection| {
                collect(
                    connection,
                    &(THREAD_SELECT.to_owned()
                        + " WHERE project_id = ? ORDER BY created_at ASC, thread_id ASC"),
                    [project_id],
                    decode_thread,
                )
            })
            .await
    }

    /// Reads the project and its canonical worktree-owning threads in one SQLite job.
    ///
    /// Panel and deleted threads cannot claim worktrees. The extra sentinel row reports
    /// truncation without allowing the returned collection to exceed `limit`.
    pub async fn load_worktree_catalog_projection(
        &self,
        project_id: String,
        limit: usize,
    ) -> Result<Option<WorktreeCatalogProjection>> {
        let limit = min(limit, 512);
        self.database
            .call(move |connection| {
                let Some(project) = connection
                    .query_row(
                        &format!("{PROJECT_SELECT} WHERE project_id = ?"),
                        [&project_id],
                        decode_project,
                    )
                    .optional()?
                else {
                    return Ok(None);
                };
                let sentinel_limit = i64::try_from(limit.saturating_add(1)).unwrap_or(i64::MAX);
                let mut threads = collect(
                    connection,
                    &(THREAD_SELECT.to_owned()
                        + " WHERE project_id = ? AND kind <> 'panel' AND deleted_at IS NULL ORDER BY created_at ASC, thread_id ASC LIMIT ?"),
                    params![project_id, sentinel_limit],
                    decode_thread,
                )?;
                let truncated = threads.len() > limit;
                threads.truncate(limit);
                Ok(Some(WorktreeCatalogProjection {
                    project,
                    threads,
                    truncated,
                }))
            })
            .await
    }

    pub async fn delete_thread(&self, thread_id: String) -> Result<()> {
        self.delete(
            "DELETE FROM projection_threads WHERE thread_id = ?",
            thread_id,
        )
        .await
    }

    pub async fn upsert_message(&self, row: ProjectionThreadMessage) -> Result<()> {
        self.database.call(move |connection| {
            let attachments = row.attachments.as_ref().map(encode_json).transpose()?;
            connection.execute(
                "INSERT INTO projection_thread_messages (message_id, thread_id, turn_id, role, text, attachments_json, is_streaming, delivery_state, delivery_provider, delivery_detail, created_at, updated_at, delivery_mode, delivery_held, delivery_reason, delivery_provider_instance_id) \
                 VALUES (?, ?, ?, ?, ?, COALESCE(?, (SELECT attachments_json FROM projection_thread_messages WHERE message_id = ?)), ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT (message_id) DO UPDATE SET \
                   thread_id=excluded.thread_id, turn_id=excluded.turn_id, role=excluded.role, text=excluded.text, \
                   attachments_json=COALESCE(excluded.attachments_json, projection_thread_messages.attachments_json), \
                   is_streaming=excluded.is_streaming, delivery_state=excluded.delivery_state, delivery_provider=excluded.delivery_provider, delivery_detail=excluded.delivery_detail, delivery_mode=excluded.delivery_mode, delivery_held=excluded.delivery_held, delivery_reason=excluded.delivery_reason, delivery_provider_instance_id=excluded.delivery_provider_instance_id, created_at=excluded.created_at, updated_at=excluded.updated_at",
                params![row.message_id,row.thread_id,row.turn_id,row.role,row.text,attachments,row.message_id,i64::from(row.is_streaming),row.delivery_state,row.delivery_provider,row.delivery_detail,row.created_at,row.updated_at,row.delivery_mode,row.delivery_held,row.delivery_reason,row.delivery_provider_instance_id],
            )?; Ok(())
        }).await
    }

    pub async fn get_provider_turn_delivery(
        &self,
        command_id: String,
    ) -> Result<Option<ProviderTurnDelivery>> {
        self.database
            .call(move |connection| {
                connection
                    .query_row(
                        &(PROVIDER_TURN_DELIVERY_SELECT.to_owned() + " WHERE command_id = ?"),
                        [command_id],
                        decode_provider_turn_delivery,
                    )
                    .optional()
                    .map_err(Into::into)
            })
            .await
    }

    pub async fn list_provider_turn_deliveries(
        &self,
        states: Vec<TurnDeliveryState>,
    ) -> Result<Vec<ProviderTurnDelivery>> {
        #[cfg(test)]
        if self.delivery_test_hooks.fail_reads.load(Ordering::SeqCst) {
            return Err(PersistenceError::WorkerUnavailable);
        }
        if states.is_empty() {
            return Ok(Vec::new());
        }
        #[cfg(test)]
        let pause_after_read = states.contains(&TurnDeliveryState::Pending);
        let result = self
            .database
            .call(move |connection| {
                let values = states
                    .iter()
                    .map(|state| state.as_str())
                    .collect::<Vec<_>>();
                let placeholders = std::iter::repeat_n("?", values.len())
                    .collect::<Vec<_>>()
                    .join(", ");
                collect(
                    connection,
                    &(PROVIDER_TURN_DELIVERY_SELECT.to_owned()
                        + " WHERE state IN ("
                        + &placeholders
                        + ") ORDER BY created_at ASC, command_id ASC"),
                    rusqlite::params_from_iter(values),
                    decode_provider_turn_delivery,
                )
            })
            .await;
        #[cfg(test)]
        if pause_after_read {
            let pause = self
                .delivery_test_hooks
                .pause_after_next_read
                .lock()
                .expect("provider turn read pause mutex")
                .take();
            if let Some(pause) = pause {
                pause.entered.notify_one();
                pause.release.notified().await;
            }
        }
        result
    }

    pub async fn get_provider_turn_delivery_for_message(
        &self,
        thread_id: String,
        message_id: String,
    ) -> Result<Option<ProviderTurnDelivery>> {
        self.database
            .call(move |connection| {
                connection
                    .query_row(
                        &(PROVIDER_TURN_DELIVERY_SELECT.to_owned()
                            + " WHERE thread_id = ? AND message_id = ?"),
                        params![thread_id, message_id],
                        decode_provider_turn_delivery,
                    )
                    .optional()
                    .map_err(Into::into)
            })
            .await
    }

    pub(crate) async fn get_queued_provider_turn_head(
        &self,
        thread_id: String,
    ) -> Result<Option<ProviderTurnDelivery>> {
        self.database
            .call(move |connection| queued_provider_turn_head_on(connection, &thread_id))
            .await
    }

    pub async fn can_promote_queued_provider_turn(
        &self,
        thread_id: String,
        message_id: String,
        automatic: bool,
    ) -> Result<bool> {
        self.database
            .call(move |connection| {
                can_promote_queued_provider_turn_on(connection, &thread_id, &message_id, automatic)
            })
            .await
    }

    /// OpenCode deliveries dismissed while uncertain whose pending turn marker still holds the
    /// queue: they are released once OpenCode shows the session idle.
    pub(crate) async fn list_dismissed_uncertain_opencode_markers(
        &self,
    ) -> Result<Vec<ProviderTurnDelivery>> {
        self.database
            .call(|connection| {
                let query = PROVIDER_TURN_DELIVERY_SELECT.to_owned()
                    + " AS dismissed WHERE dismissed.state = 'dismissed' AND dismissed.provider_kind = 'opencode'
                       AND EXISTS(SELECT 1 FROM projection_threads AS thread
                         WHERE thread.thread_id = dismissed.thread_id AND thread.deleted_at IS NULL
                           -- A closed chat panel starts no provider until it is reopened.
                           AND NOT (thread.kind = 'panel' AND thread.archived_at IS NOT NULL))
                       AND EXISTS(SELECT 1 FROM projection_turns AS marker
                         WHERE marker.thread_id = dismissed.thread_id AND marker.pending_message_id = dismissed.message_id
                           AND marker.turn_id IS NULL AND marker.state = 'running')
                       AND EXISTS(SELECT 1 FROM orchestration_events AS resolution
                         WHERE resolution.aggregate_kind = 'thread' AND resolution.stream_id = dismissed.thread_id
                           AND resolution.event_type = 'thread.turn-delivery-updated'
                           AND json_extract(resolution.metadata_json, '$.deliveryCommandId') = dismissed.command_id
                           AND json_extract(resolution.payload_json, '$.state') = 'dismissed'
                           AND json_extract(resolution.metadata_json, '$.deliveryPreviousState') = 'uncertain')
                       ORDER BY dismissed.created_at, dismissed.command_id";
                collect(connection, &query, [], decode_provider_turn_delivery)
            })
            .await
    }

    pub async fn list_queued_provider_turn_heads(&self) -> Result<Vec<ProviderTurnDelivery>> {
        self.database.call(|connection| {
            // Rank identifiers once, then load only each thread's head payload.
            let query = "WITH queued_order AS (
                SELECT queued.command_id,
                  ROW_NUMBER() OVER (PARTITION BY queued.thread_id ORDER BY receipt.result_sequence, queued.created_at, queued.command_id) AS position
                FROM provider_turn_outbox AS queued
                JOIN orchestration_command_receipts AS receipt ON receipt.command_id = queued.command_id
                WHERE queued.state = 'queued'
            ) ".to_owned() + PROVIDER_TURN_DELIVERY_SELECT
                + " AS queued WHERE command_id IN (SELECT command_id FROM queued_order WHERE position = 1) ORDER BY (SELECT result_sequence FROM orchestration_command_receipts WHERE command_id = queued.command_id), created_at, command_id";
            collect(connection, &query, [], decode_provider_turn_delivery)
        }).await
    }

    /// Return an unaccepted steer to the durable queue without changing its identity or payload.
    pub async fn requeue_provider_turn(
        &self,
        command_id: String,
        expected_attempt: i64,
        updated_at: String,
    ) -> Result<Option<ProviderTurnDelivery>> {
        self.database
            .call(move |connection| {
                requeue_provider_turn_on(connection, &command_id, expected_attempt, &updated_at)
            })
            .await
    }

    pub(crate) async fn get_running_provider_steer_target(
        &self,
        thread_id: String,
        message_id: String,
    ) -> Result<Option<String>> {
        self.database.call(move |connection| {
            let target = provider_turn_steer_target_on(connection, &thread_id, &message_id)?;
            let active: Option<String> = connection.query_row(
                "SELECT session.active_turn_id FROM projection_thread_sessions AS session
                 JOIN provider_turn_outbox AS delivery ON delivery.thread_id = session.thread_id
                 WHERE session.thread_id = ? AND session.status = 'running' AND delivery.message_id = ?
                   AND delivery.mode = 'steer' AND delivery.state IN ('pending', 'sending') AND delivery.held = 0",
                params![thread_id, message_id], |row| row.get::<_, Option<String>>(0),
            ).optional()?.flatten();
            Ok(target.filter(|id| Some(id) == active.as_ref()))
        }).await
    }

    pub async fn list_referenced_attachment_ids(&self) -> Result<Vec<String>> {
        self.database.call(|connection| collect(connection, "SELECT DISTINCT attachment_id FROM orchestration_attachment_refs ORDER BY attachment_id ASC", [], |row| row.get(0))).await
    }

    /// Returns which of `attachment_ids` an accepted command on `thread_id` already attached, each
    /// with its recorded content digest (none for references backfilled from legacy events).
    pub async fn thread_attachment_digests(
        &self,
        thread_id: String,
        attachment_ids: Vec<String>,
    ) -> Result<HashMap<String, Option<String>>> {
        self.database
            .call(move |connection| {
                // One digest per attachment: MAX ignores NULL, so a recorded digest wins over a
                // legacy reference without one. Several different digests for one id, which the
                // immutable attachment files rule out in practice, resolve to the
                // lexicographically greatest, and the stored file must then match that one.
                let mut statement = connection.prepare(
                    "SELECT MAX(refs.content_digest) FROM orchestration_attachment_refs AS refs \
                     JOIN orchestration_command_receipts AS receipts \
                       ON receipts.command_id = refs.command_id \
                     WHERE refs.attachment_id = ? AND receipts.aggregate_kind = 'thread' \
                       AND receipts.aggregate_id = ? AND receipts.status = 'accepted' \
                     GROUP BY refs.attachment_id",
                )?;
                let mut digests = HashMap::new();
                for attachment_id in attachment_ids {
                    let digest = statement
                        .query_row(params![attachment_id, thread_id], |row| {
                            row.get::<_, Option<String>>(0)
                        })
                        .optional()?;
                    if let Some(digest) = digest {
                        digests.insert(attachment_id, digest);
                    }
                }
                Ok(digests)
            })
            .await
    }

    pub async fn claim_provider_turn(
        &self,
        command_id: String,
        updated_at: String,
    ) -> Result<Option<ProviderTurnDelivery>> {
        #[cfg(test)]
        self.delivery_test_hooks
            .claim_calls
            .fetch_add(1, Ordering::SeqCst);
        // A closed (archived) chat panel starts nothing until it is reopened.
        let result = self.database.call(move |connection| connection.query_row(
            "UPDATE provider_turn_outbox SET state = 'sending', attempts = attempts + 1, updated_at = ? WHERE command_id = ? AND state = 'pending' AND (mode <> 'start' OR NOT EXISTS(SELECT 1 FROM projection_thread_sessions WHERE thread_id = provider_turn_outbox.thread_id AND status IN ('running', 'starting'))) AND NOT EXISTS(SELECT 1 FROM projection_threads WHERE thread_id = provider_turn_outbox.thread_id AND kind = 'panel' AND archived_at IS NOT NULL) RETURNING command_id, thread_id, message_id, provider_instance_id, provider_kind, provider_session_id, delivery_key, payload_json, state, attempts, last_error, created_at, updated_at, mode, held",
            params![updated_at, command_id], decode_provider_turn_delivery).optional().map_err(Into::into)).await;
        #[cfg(test)]
        if result.as_ref().is_ok_and(Option::is_some) {
            let pause = self
                .delivery_test_hooks
                .pause_after_next_claim
                .lock()
                .expect("provider turn claim pause mutex")
                .take();
            if let Some(pause) = pause {
                pause.entered.notify_one();
                pause.release.notified().await;
            }
        }
        result
    }

    pub async fn freeze_provider_turn_session(
        &self,
        command_id: String,
        expected_attempt: i64,
        provider_instance_id: String,
        provider_kind: String,
        provider_session_id: String,
        updated_at: String,
    ) -> Result<Option<ProviderTurnDelivery>> {
        self.database
            .call(move |connection| {
                connection
                    .query_row(
                        "UPDATE provider_turn_outbox SET provider_session_id = ?, updated_at = ? \
                         WHERE command_id = ? AND state = 'sending' AND attempts = ? \
                           AND provider_instance_id = ? AND provider_kind = ? \
                           AND (provider_session_id IS NULL OR provider_session_id = ?) \
                         RETURNING command_id, thread_id, message_id, provider_instance_id, provider_kind, provider_session_id, delivery_key, payload_json, state, attempts, last_error, created_at, updated_at, mode, held",
                        params![
                            provider_session_id,
                            updated_at,
                            command_id,
                            expected_attempt,
                            provider_instance_id,
                            provider_kind,
                            provider_session_id,
                        ],
                        decode_provider_turn_delivery,
                    )
                    .optional()
                    .map_err(Into::into)
            })
            .await
    }

    /// Clears only a claimed start's lost native identity, preserving its payload and FIFO receipt.
    pub(crate) async fn unfreeze_provider_turn_session(
        &self,
        command_id: String,
        expected_attempt: i64,
        provider_instance_id: String,
        provider_kind: String,
        provider_session_id: String,
        updated_at: String,
    ) -> Result<Option<ProviderTurnDelivery>> {
        self.database.call(move |connection| {
            connection.query_row(
                "UPDATE provider_turn_outbox SET provider_session_id = NULL, updated_at = ? \
                 WHERE command_id = ? AND state = 'sending' AND mode = 'start' AND attempts = ? \
                   AND provider_instance_id = ? AND provider_kind = ? AND provider_session_id = ? \
                   AND NOT EXISTS (SELECT 1 FROM provider_session_runtime AS runtime \
                     WHERE runtime.thread_id = provider_turn_outbox.thread_id \
                       AND (runtime.provider_name <> provider_turn_outbox.provider_kind \
                         OR runtime.provider_instance_id IS NOT provider_turn_outbox.provider_instance_id \
                         OR runtime.resume_cursor_json IS NOT NULL)) \
                 RETURNING command_id, thread_id, message_id, provider_instance_id, provider_kind, provider_session_id, delivery_key, payload_json, state, attempts, last_error, created_at, updated_at, mode, held",
                params![updated_at, command_id, expected_attempt, provider_instance_id, provider_kind, provider_session_id],
                decode_provider_turn_delivery,
            ).optional().map_err(Into::into)
        }).await
    }

    pub async fn replace_pending_provider_turn_payload(
        &self,
        command_id: String,
        expected_attempt: i64,
        payload: Value,
        updated_at: String,
    ) -> Result<Option<ProviderTurnDelivery>> {
        self.database
            .call(move |connection| {
                connection
                    .query_row(
                        "UPDATE provider_turn_outbox SET payload_json = ?, updated_at = ? \
                         WHERE command_id = ? AND state = 'pending' AND attempts = ? \
                         RETURNING command_id, thread_id, message_id, provider_instance_id, provider_kind, provider_session_id, delivery_key, payload_json, state, attempts, last_error, created_at, updated_at, mode, held",
                        params![
                            encode_json(&payload)?,
                            updated_at,
                            command_id,
                            expected_attempt,
                        ],
                        decode_provider_turn_delivery,
                    )
                    .optional()
                    .map_err(Into::into)
            })
            .await
    }

    #[cfg(test)]
    pub(crate) fn fail_provider_turn_reads_for_test(&self, fail: bool) {
        self.delivery_test_hooks
            .fail_reads
            .store(fail, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub(crate) fn provider_turn_claims_for_test(&self) -> usize {
        self.delivery_test_hooks.claim_calls.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(crate) fn pause_after_next_provider_turn_read_for_test(&self) -> ProviderTurnReadPause {
        let pause = ProviderTurnReadPause {
            entered: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new(tokio::sync::Notify::new()),
        };
        *self
            .delivery_test_hooks
            .pause_after_next_read
            .lock()
            .expect("provider turn read pause mutex") = Some(pause.clone());
        pause
    }

    #[cfg(test)]
    pub(crate) fn pause_after_next_provider_turn_claim_for_test(&self) -> ProviderTurnReadPause {
        let pause = ProviderTurnReadPause {
            entered: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new(tokio::sync::Notify::new()),
        };
        *self
            .delivery_test_hooks
            .pause_after_next_claim
            .lock()
            .expect("provider turn claim pause mutex") = Some(pause.clone());
        pause
    }

    pub async fn get_message(&self, message_id: String) -> Result<Option<ProjectionThreadMessage>> {
        self.database
            .call(move |connection| {
                connection
                    .query_row(
                        &(MESSAGE_SELECT.to_owned() + " WHERE message_id = ? LIMIT 1"),
                        [message_id],
                        decode_message,
                    )
                    .optional()
                    .map_err(Into::into)
            })
            .await
    }
    pub async fn list_messages_by_thread(
        &self,
        thread_id: String,
    ) -> Result<Vec<ProjectionThreadMessage>> {
        self.database
            .call(move |connection| {
                collect(
                    connection,
                    &format!("{MESSAGE_SELECT} WHERE thread_id = ?{MESSAGE_LIST_ORDER}"),
                    [thread_id],
                    decode_message,
                )
            })
            .await
    }
    pub async fn delete_messages_by_thread(&self, thread_id: String) -> Result<()> {
        self.delete(
            "DELETE FROM projection_thread_messages WHERE thread_id = ?",
            thread_id,
        )
        .await
    }

    pub async fn upsert_activity(&self, row: ProjectionThreadActivity) -> Result<()> {
        self.database.call(move |connection| { connection.execute(
            "INSERT INTO projection_thread_activities (activity_id, thread_id, turn_id, tone, kind, summary, payload_json, sequence, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (activity_id) DO UPDATE SET thread_id=excluded.thread_id, turn_id=excluded.turn_id, tone=excluded.tone, kind=excluded.kind, summary=excluded.summary, payload_json=excluded.payload_json, sequence=excluded.sequence, created_at=excluded.created_at",
            params![row.activity_id,row.thread_id,row.turn_id,row.tone,row.kind,row.summary,encode_json(&row.payload)?,row.sequence,row.created_at])?; Ok(()) }).await
    }
    pub async fn list_activities_by_thread(
        &self,
        thread_id: String,
    ) -> Result<Vec<ProjectionThreadActivity>> {
        self.database
            .call(move |connection| {
                collect(
                    connection,
                    &format!("{ACTIVITY_SELECT} WHERE thread_id = ?{ACTIVITY_LIST_ORDER}"),
                    [thread_id],
                    decode_activity,
                )
            })
            .await
    }
    pub async fn delete_activities_by_thread(&self, thread_id: String) -> Result<()> {
        self.delete(
            "DELETE FROM projection_thread_activities WHERE thread_id = ?",
            thread_id,
        )
        .await
    }

    pub async fn upsert_thread_session(&self, row: ProjectionThreadSession) -> Result<()> {
        self.database.call(move |connection| { connection.execute(
            "INSERT INTO projection_thread_sessions (thread_id, status, provider_name, provider_instance_id, runtime_mode, active_turn_id, last_error, last_error_class, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (thread_id) DO UPDATE SET status=excluded.status, provider_name=excluded.provider_name, provider_instance_id=excluded.provider_instance_id, runtime_mode=excluded.runtime_mode, active_turn_id=excluded.active_turn_id, last_error=excluded.last_error, last_error_class=excluded.last_error_class, updated_at=excluded.updated_at",
            params![row.thread_id,row.status,row.provider_name,row.provider_instance_id,row.runtime_mode,row.active_turn_id,row.last_error,row.last_error_class,row.updated_at])?; Ok(()) }).await
    }
    pub async fn get_thread_session(
        &self,
        thread_id: String,
    ) -> Result<Option<ProjectionThreadSession>> {
        self.database
            .call(move |connection| {
                connection
                    .query_row(
                        &format!("{THREAD_SESSION_SELECT} WHERE thread_id = ?"),
                        [thread_id],
                        decode_thread_session,
                    )
                    .optional()
                    .map_err(Into::into)
            })
            .await
    }

    /// Sessions that are running or starting, and queued messages, for the update confirmation.
    pub async fn count_active_work(&self) -> Result<(u64, u64)> {
        self.database
            .call(|connection| {
                let running: i64 = connection.query_row(
                    "SELECT COUNT(*) FROM projection_thread_sessions WHERE status IN ('running', 'starting')",
                    [],
                    |row| row.get(0),
                )?;
                let queued: i64 = connection.query_row(
                    "SELECT COUNT(*) FROM provider_turn_outbox WHERE state = 'queued'",
                    [],
                    |row| row.get(0),
                )?;
                // COUNT(*) is never negative.
                Ok((
                    u64::try_from(running).unwrap_or(0),
                    u64::try_from(queued).unwrap_or(0),
                ))
            })
            .await
    }

    pub async fn list_thread_sessions_by_status(
        &self,
        statuses: Vec<String>,
    ) -> Result<Vec<ProjectionThreadSession>> {
        if statuses.is_empty() {
            return Ok(Vec::new());
        }
        self.database.call(move |connection| {
            let placeholders = std::iter::repeat_n("?", statuses.len())
                .collect::<Vec<_>>()
                .join(", ");
            collect(
                connection,
                &format!("SELECT thread_id, status, provider_name, provider_instance_id, runtime_mode, active_turn_id, last_error, last_error_class, updated_at FROM projection_thread_sessions WHERE status IN ({placeholders}) ORDER BY updated_at ASC, thread_id ASC"),
                rusqlite::params_from_iter(statuses),
                decode_thread_session,
            )
        }).await
    }
    pub async fn delete_thread_session(&self, thread_id: String) -> Result<()> {
        self.delete(
            "DELETE FROM projection_thread_sessions WHERE thread_id = ?",
            thread_id,
        )
        .await
    }

    pub async fn upsert_pending_approval(&self, row: ProjectionPendingApproval) -> Result<()> {
        self.database.call(move |connection| { connection.execute(
            "INSERT INTO projection_pending_approvals (request_id, thread_id, turn_id, status, decision, created_at, resolved_at) VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT (request_id) DO UPDATE SET thread_id=excluded.thread_id, turn_id=excluded.turn_id, status=excluded.status, decision=excluded.decision, created_at=excluded.created_at, resolved_at=excluded.resolved_at",
            params![row.request_id,row.thread_id,row.turn_id,row.status,row.decision,row.created_at,row.resolved_at])?; Ok(()) }).await
    }
    pub async fn list_pending_approvals_by_thread(
        &self,
        thread_id: String,
    ) -> Result<Vec<ProjectionPendingApproval>> {
        self.database.call(move |connection| collect(connection, "SELECT request_id, thread_id, turn_id, status, decision, created_at, resolved_at FROM projection_pending_approvals WHERE thread_id = ? ORDER BY created_at ASC, request_id ASC", [thread_id], decode_pending_approval)).await
    }
    pub async fn get_pending_approval(
        &self,
        request_id: String,
    ) -> Result<Option<ProjectionPendingApproval>> {
        self.database.call(move |connection| connection.query_row("SELECT request_id, thread_id, turn_id, status, decision, created_at, resolved_at FROM projection_pending_approvals WHERE request_id = ?", [request_id], decode_pending_approval).optional().map_err(Into::into)).await
    }
    pub async fn delete_pending_approval(&self, request_id: String) -> Result<()> {
        self.delete(
            "DELETE FROM projection_pending_approvals WHERE request_id = ?",
            request_id,
        )
        .await
    }

    pub async fn upsert_projection_state(&self, row: ProjectionState) -> Result<()> {
        self.database.call(move |connection| { connection.execute("INSERT INTO projection_state (projector, last_applied_sequence, updated_at) VALUES (?, ?, ?) ON CONFLICT (projector) DO UPDATE SET last_applied_sequence=excluded.last_applied_sequence, updated_at=excluded.updated_at", params![row.projector,row.last_applied_sequence,row.updated_at])?; Ok(()) }).await
    }
    pub async fn get_projection_state(&self, projector: String) -> Result<Option<ProjectionState>> {
        self.database.call(move |connection| connection.query_row("SELECT projector, last_applied_sequence, updated_at FROM projection_state WHERE projector = ?", [projector], decode_projection_state).optional().map_err(Into::into)).await
    }
    pub async fn list_projection_states(&self) -> Result<Vec<ProjectionState>> {
        self.database
            .call(|connection| {
                collect(
                    connection,
                    PROJECTION_STATE_LIST,
                    [],
                    decode_projection_state,
                )
            })
            .await
    }
    pub async fn min_last_applied_sequence(&self) -> Result<Option<i64>> {
        self.database
            .call(|connection| {
                connection
                    .query_row(
                        "SELECT MIN(last_applied_sequence) FROM projection_state",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(Into::into)
            })
            .await
    }

    pub async fn upsert_proposed_plan(&self, row: ProjectionThreadProposedPlan) -> Result<()> {
        self.database.call(move |connection| { connection.execute("INSERT INTO projection_thread_proposed_plans (plan_id, thread_id, turn_id, plan_markdown, implemented_at, implementation_thread_id, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT (plan_id) DO UPDATE SET thread_id=excluded.thread_id, turn_id=excluded.turn_id, plan_markdown=excluded.plan_markdown, implemented_at=excluded.implemented_at, implementation_thread_id=excluded.implementation_thread_id, created_at=excluded.created_at, updated_at=excluded.updated_at", params![row.plan_id,row.thread_id,row.turn_id,row.plan_markdown,row.implemented_at,row.implementation_thread_id,row.created_at,row.updated_at])?; Ok(()) }).await
    }
    pub async fn list_proposed_plans_by_thread(
        &self,
        thread_id: String,
    ) -> Result<Vec<ProjectionThreadProposedPlan>> {
        self.database.call(move |connection| collect(connection, "SELECT plan_id, thread_id, turn_id, plan_markdown, implemented_at, implementation_thread_id, created_at, updated_at FROM projection_thread_proposed_plans WHERE thread_id = ? ORDER BY created_at ASC, plan_id ASC", [thread_id], decode_proposed_plan)).await
    }
    pub async fn delete_proposed_plans_by_thread(&self, thread_id: String) -> Result<()> {
        self.delete(
            "DELETE FROM projection_thread_proposed_plans WHERE thread_id = ?",
            thread_id,
        )
        .await
    }

    pub async fn replace_pending_turn_start(&self, row: ProjectionPendingTurnStart) -> Result<()> {
        self.database.call(move |connection| { let transaction = connection.transaction()?; transaction.execute("DELETE FROM projection_turns WHERE thread_id = ? AND turn_id IS NULL AND state = 'pending' AND checkpoint_turn_count IS NULL", [&row.thread_id])?; transaction.execute("INSERT INTO projection_turns (thread_id, turn_id, pending_message_id, source_proposed_plan_thread_id, source_proposed_plan_id, assistant_message_id, state, requested_at, started_at, completed_at, checkpoint_turn_count, checkpoint_ref, checkpoint_status, checkpoint_files_json) VALUES (?, NULL, ?, ?, ?, NULL, 'pending', ?, NULL, NULL, NULL, NULL, NULL, '[]')", params![row.thread_id,row.message_id,row.source_proposed_plan_thread_id,row.source_proposed_plan_id,row.requested_at])?; transaction.commit()?; Ok(()) }).await
    }

    pub async fn upsert_turn_by_id(&self, row: ProjectionTurnById) -> Result<()> {
        self.database
            .call(move |connection| {
                connection.execute(
                    TURN_UPSERT_SQL,
                    params![
                        row.thread_id,
                        row.turn_id,
                        row.pending_message_id,
                        row.source_proposed_plan_thread_id,
                        row.source_proposed_plan_id,
                        row.assistant_message_id,
                        row.state,
                        row.requested_at,
                        row.started_at,
                        row.completed_at,
                        row.checkpoint_turn_count,
                        row.checkpoint_ref,
                        row.checkpoint_status,
                        encode_json(&row.checkpoint_files)?
                    ],
                )?;
                Ok(())
            })
            .await
    }
    pub async fn get_pending_turn_start(
        &self,
        thread_id: String,
    ) -> Result<Option<ProjectionPendingTurnStart>> {
        self.database.call(move |connection| connection.query_row("SELECT thread_id, pending_message_id, source_proposed_plan_thread_id, source_proposed_plan_id, requested_at FROM projection_turns WHERE thread_id = ? AND turn_id IS NULL AND state = 'pending' AND pending_message_id IS NOT NULL AND checkpoint_turn_count IS NULL ORDER BY requested_at DESC LIMIT 1", [thread_id], decode_pending_turn).optional().map_err(Into::into)).await
    }
    pub async fn delete_pending_turn_start(&self, thread_id: String) -> Result<()> {
        self.database.call(move |connection| { connection.execute("DELETE FROM projection_turns WHERE thread_id = ? AND turn_id IS NULL AND state = 'pending' AND checkpoint_turn_count IS NULL", [thread_id])?; Ok(()) }).await
    }
    pub async fn list_turns_by_thread(&self, thread_id: String) -> Result<Vec<ProjectionTurn>> {
        self.database.call(move |connection| collect(connection, &(TURN_SELECT.to_owned() + " WHERE thread_id = ? ORDER BY CASE WHEN checkpoint_turn_count IS NULL THEN 1 ELSE 0 END ASC, checkpoint_turn_count ASC, requested_at ASC, turn_id ASC"), [thread_id], decode_turn)).await
    }
    pub async fn get_turn_by_id(
        &self,
        thread_id: String,
        turn_id: String,
    ) -> Result<Option<ProjectionTurnById>> {
        self.database
            .call(move |connection| {
                connection
                    .query_row(
                        &(TURN_SELECT.to_owned() + " WHERE thread_id = ? AND turn_id = ? LIMIT 1"),
                        params![thread_id, turn_id],
                        decode_turn_by_id,
                    )
                    .optional()
                    .map_err(Into::into)
            })
            .await
    }
    pub async fn clear_checkpoint_turn_conflict(
        &self,
        thread_id: String,
        turn_id: String,
        checkpoint_turn_count: i64,
    ) -> Result<()> {
        self.database.call(move |connection| { connection.execute("UPDATE projection_turns SET checkpoint_turn_count=NULL, checkpoint_ref=NULL, checkpoint_status=NULL, checkpoint_files_json='[]' WHERE thread_id = ? AND checkpoint_turn_count = ? AND (turn_id IS NULL OR turn_id <> ?)", params![thread_id,checkpoint_turn_count,turn_id])?; Ok(()) }).await
    }
    pub async fn delete_turns_by_thread(&self, thread_id: String) -> Result<()> {
        self.delete(
            "DELETE FROM projection_turns WHERE thread_id = ?",
            thread_id,
        )
        .await
    }

    /// Checkpoints are projection-turn rows.  This transaction clears a
    /// conflicting checkpoint key before inserting/updating the canonical turn.
    pub async fn upsert_checkpoint(&self, row: ProjectionCheckpoint) -> Result<()> {
        self.database.call(move |connection| { let transaction = connection.transaction()?; transaction.execute("UPDATE projection_turns SET checkpoint_turn_count=NULL, checkpoint_ref=NULL, checkpoint_status=NULL, checkpoint_files_json='[]' WHERE thread_id = ? AND checkpoint_turn_count = ?", params![row.thread_id,row.checkpoint_turn_count])?; transaction.execute(TURN_UPSERT_SQL, params![row.thread_id,row.turn_id,Option::<String>::None,Option::<String>::None,Option::<String>::None,row.assistant_message_id,if row.status == "error" { "error" } else { "completed" },row.completed_at,row.completed_at,row.completed_at,row.checkpoint_turn_count,row.checkpoint_ref,row.status,encode_json(&row.files)?])?; transaction.commit()?; Ok(()) }).await
    }
    pub async fn list_checkpoints_by_thread(
        &self,
        thread_id: String,
    ) -> Result<Vec<ProjectionCheckpoint>> {
        self.database.call(move |connection| collect(connection, "SELECT thread_id, turn_id, checkpoint_turn_count, checkpoint_ref, checkpoint_status, checkpoint_files_json, assistant_message_id, completed_at FROM projection_turns WHERE thread_id = ? AND checkpoint_turn_count IS NOT NULL ORDER BY checkpoint_turn_count ASC", [thread_id], decode_checkpoint)).await
    }
    pub async fn get_checkpoint(
        &self,
        thread_id: String,
        checkpoint_turn_count: i64,
    ) -> Result<Option<ProjectionCheckpoint>> {
        self.database.call(move |connection| connection.query_row("SELECT thread_id, turn_id, checkpoint_turn_count, checkpoint_ref, checkpoint_status, checkpoint_files_json, assistant_message_id, completed_at FROM projection_turns WHERE thread_id = ? AND checkpoint_turn_count = ?", params![thread_id,checkpoint_turn_count], decode_checkpoint).optional().map_err(Into::into)).await
    }
    pub async fn delete_checkpoints_by_thread(&self, thread_id: String) -> Result<()> {
        self.database.call(move |connection| { connection.execute("UPDATE projection_turns SET checkpoint_turn_count=NULL, checkpoint_ref=NULL, checkpoint_status=NULL, checkpoint_files_json='[]' WHERE thread_id = ? AND checkpoint_turn_count IS NOT NULL", [thread_id])?; Ok(()) }).await
    }

    pub async fn create_auth_pairing_link(&self, row: AuthPairingLink) -> Result<()> {
        self.database
            .call(move |connection| {
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                insert_auth_pairing_link_on(&transaction, &row)?;
                bump_auth_authority_revision_on(&transaction)?;
                transaction.commit()?;
                Ok(())
            })
            .await
    }
    pub async fn create_auth_pairing_link_with_offer(
        &self,
        row: AuthPairingLink,
        offer: NewAuthPairingOffer,
    ) -> Result<AuthPairingOfferReservation> {
        self.database
            .call(move |connection| {
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let pruned = transaction.execute(
                    "DELETE FROM auth_pairing_offer_idempotency WHERE expires_at <= ?",
                    [&row.created_at],
                )?;
                let existing = transaction
                    .query_row(
                        PAIRING_OFFER_BY_KEY_SQL,
                        params![row.created_at, offer.principal_id, offer.idempotency_key],
                        decode_pairing_offer,
                    )
                    .optional()?;
                if let Some(offer) = existing {
                    let pairing = match &offer.pairing_id {
                        Some(pairing_id) => transaction
                            .query_row(
                                &(PAIRING_SELECT.to_owned()
                                    + " WHERE id = ? AND revoked_at IS NULL
                                         AND consumed_at IS NULL AND expires_at > ?"),
                                params![pairing_id, row.created_at],
                                decode_pairing_link,
                            )
                            .optional()?,
                        None => None,
                    };
                    if pruned > 0 {
                        bump_auth_authority_revision_on(&transaction)?;
                    }
                    transaction.commit()?;
                    return Ok(AuthPairingOfferReservation {
                        offer,
                        pairing,
                        reserved: false,
                    });
                }
                ensure_auth_pairing_offer_capacity_on(&transaction, &offer.principal_id)?;
                insert_auth_pairing_link_on(&transaction, &row)?;
                transaction.execute(
                    "INSERT INTO auth_pairing_offer_idempotency (
                        principal_id, idempotency_key, input_fingerprint, pairing_id,
                        result_json, expires_at, cancelled_at
                     ) VALUES (?, ?, ?, ?, NULL, ?, NULL)",
                    params![
                        offer.principal_id,
                        offer.idempotency_key,
                        offer.input_fingerprint,
                        row.id,
                        offer.expires_at,
                    ],
                )?;
                let persisted = AuthPairingOffer {
                    principal_id: offer.principal_id,
                    idempotency_key: offer.idempotency_key,
                    input_fingerprint: offer.input_fingerprint,
                    pairing_id: Some(row.id.clone()),
                    result: None,
                    expires_at: offer.expires_at,
                    cancelled_at: None,
                };
                bump_auth_authority_revision_on(&transaction)?;
                transaction.commit()?;
                Ok(AuthPairingOfferReservation {
                    offer: persisted,
                    pairing: Some(row),
                    reserved: true,
                })
            })
            .await
    }
    pub async fn complete_auth_pairing_offer(
        &self,
        principal_id: String,
        idempotency_key: String,
        result: Value,
    ) -> Result<bool> {
        self.database
            .call(move |connection| {
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let completed = transaction
                    .query_row(
                        "UPDATE auth_pairing_offer_idempotency SET result_json = ?
                         WHERE principal_id = ? AND idempotency_key = ?
                           AND cancelled_at IS NULL AND pairing_id IS NOT NULL
                         RETURNING idempotency_key",
                        params![encode_json(&result)?, principal_id, idempotency_key],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?
                    .is_some();
                if completed {
                    bump_auth_authority_revision_on(&transaction)?;
                }
                transaction.commit()?;
                Ok(completed)
            })
            .await
    }
    pub async fn prune_and_list_active_auth_pairing_offers(
        &self,
        now: Timestamp,
    ) -> Result<Vec<AuthPairingOffer>> {
        self.database
            .call(move |connection| {
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let pruned = transaction.execute(
                    "DELETE FROM auth_pairing_offer_idempotency WHERE expires_at <= ?",
                    [&now],
                )?;
                let offers = collect(
                    &transaction,
                    PAIRING_OFFER_SELECT,
                    [&now],
                    decode_pairing_offer,
                )?;
                if pruned > 0 {
                    bump_auth_authority_revision_on(&transaction)?;
                }
                transaction.commit()?;
                Ok(offers)
            })
            .await
    }
    pub async fn load_auth_authority_snapshot(
        &self,
        now: Timestamp,
    ) -> Result<AuthAuthoritySnapshot> {
        self.database
            .call(move |connection| {
                let transaction = connection.transaction()?;
                let revision = auth_authority_revision_on(&transaction)?;
                let pairings = collect(
                    &transaction,
                    &(PAIRING_SELECT.to_owned()
                        + " WHERE revoked_at IS NULL AND consumed_at IS NULL
                             AND expires_at > ? ORDER BY created_at DESC, id DESC"),
                    [&now],
                    decode_pairing_link,
                )?;
                let offers = collect(
                    &transaction,
                    PAIRING_OFFER_SELECT,
                    [&now],
                    decode_pairing_offer,
                )?;
                let sessions = collect(
                    &transaction,
                    &(AUTH_SESSION_SELECT.to_owned()
                        + " WHERE revoked_at IS NULL AND expires_at > ?
                             ORDER BY issued_at DESC, session_id DESC"),
                    [&now],
                    decode_auth_session,
                )?;
                let next_expiry_at = transaction.query_row(
                    "SELECT MIN(expires_at) FROM (
                        SELECT MIN(expires_at) AS expires_at FROM auth_pairing_links
                         WHERE revoked_at IS NULL AND consumed_at IS NULL AND expires_at > ?
                        UNION ALL
                        SELECT MIN(expires_at) AS expires_at FROM auth_pairing_offer_idempotency
                         WHERE expires_at > ?
                        UNION ALL
                        SELECT MIN(expires_at) AS expires_at FROM auth_sessions
                         WHERE revoked_at IS NULL AND expires_at > ?
                     )",
                    params![now, now, now],
                    |row| row.get::<_, Option<String>>(0),
                )?;
                transaction.commit()?;
                Ok(AuthAuthoritySnapshot {
                    revision,
                    next_expiry_at,
                    pairings,
                    offers,
                    sessions,
                })
            })
            .await
    }
    pub async fn auth_authority_revision(&self) -> Result<u64> {
        self.database
            .call(|connection| auth_authority_revision_on(connection))
            .await
    }
    pub async fn prune_and_get_active_auth_pairing_offer(
        &self,
        principal_id: String,
        idempotency_key: String,
        now: Timestamp,
    ) -> Result<Option<AuthPairingOfferReservation>> {
        self.database
            .call(move |connection| {
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let pruned = transaction.execute(
                    "DELETE FROM auth_pairing_offer_idempotency WHERE expires_at <= ?",
                    [&now],
                )?;
                let offer = transaction
                    .query_row(
                        PAIRING_OFFER_BY_KEY_SQL,
                        params![now, principal_id, idempotency_key],
                        decode_pairing_offer,
                    )
                    .optional()?;
                let authority = match offer {
                    Some(offer) => {
                        let pairing = match &offer.pairing_id {
                            Some(pairing_id) => transaction
                                .query_row(
                                    &(PAIRING_SELECT.to_owned()
                                        + " WHERE id = ? AND revoked_at IS NULL
                                             AND consumed_at IS NULL AND expires_at > ?"),
                                    params![pairing_id, now],
                                    decode_pairing_link,
                                )
                                .optional()?,
                            None => None,
                        };
                        Some(AuthPairingOfferReservation {
                            offer,
                            pairing,
                            reserved: false,
                        })
                    }
                    None => None,
                };
                if pruned > 0 {
                    bump_auth_authority_revision_on(&transaction)?;
                }
                transaction.commit()?;
                Ok(authority)
            })
            .await
    }
    /// Atomically abandons a reservation whose pairing grant was committed but
    /// whose encoded offer was never recorded. Completed offers, cancellation
    /// tombstones, and conflicting request fingerprints are left untouched.
    pub async fn recover_pending_auth_pairing_offer(
        &self,
        principal_id: String,
        idempotency_key: String,
        input_fingerprint: String,
        now: Timestamp,
        stale_before: Timestamp,
    ) -> Result<Option<String>> {
        self.database
            .call(move |connection| {
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let pruned = transaction.execute(
                    "DELETE FROM auth_pairing_offer_idempotency WHERE expires_at <= ?",
                    [&now],
                )?;
                let pending_pairing_id = transaction
                    .query_row(
                        "SELECT offer.pairing_id
                         FROM auth_pairing_offer_idempotency AS offer
                         JOIN auth_pairing_links AS pairing ON pairing.id = offer.pairing_id
                         WHERE offer.principal_id = ? AND offer.idempotency_key = ?
                           AND offer.input_fingerprint = ? AND offer.pairing_id IS NOT NULL
                           AND offer.result_json IS NULL AND offer.cancelled_at IS NULL
                           AND offer.expires_at > ? AND pairing.created_at <= ?",
                        params![
                            principal_id,
                            idempotency_key,
                            input_fingerprint,
                            now,
                            stale_before,
                        ],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?;
                if let Some(pairing_id) = &pending_pairing_id {
                    transaction.execute(
                        "UPDATE auth_pairing_links SET revoked_at = ?
                         WHERE id = ? AND revoked_at IS NULL AND consumed_at IS NULL",
                        params![now, pairing_id],
                    )?;
                    transaction.execute(
                        "DELETE FROM auth_pairing_offer_idempotency
                         WHERE principal_id = ? AND idempotency_key = ?
                           AND input_fingerprint = ? AND pairing_id = ?
                           AND result_json IS NULL AND cancelled_at IS NULL",
                        params![principal_id, idempotency_key, input_fingerprint, pairing_id],
                    )?;
                }
                if pruned > 0 || pending_pairing_id.is_some() {
                    bump_auth_authority_revision_on(&transaction)?;
                }
                transaction.commit()?;
                Ok(pending_pairing_id)
            })
            .await
    }
    pub async fn cancel_auth_pairing_offer(
        &self,
        principal_id: String,
        idempotency_key: String,
        cancelled_at: Timestamp,
        expires_at: Timestamp,
    ) -> Result<AuthPairingOfferCancellation> {
        self.database
            .call(move |connection| {
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                transaction.execute(
                    "DELETE FROM auth_pairing_offer_idempotency WHERE expires_at <= ?",
                    [&cancelled_at],
                )?;
                let existing_pairing_id = transaction
                    .query_row(
                        "SELECT pairing_id FROM auth_pairing_offer_idempotency
                         WHERE principal_id = ? AND idempotency_key = ?",
                        params![principal_id, idempotency_key],
                        |row| row.get::<_, Option<String>>(0),
                    )
                    .optional()?;
                if existing_pairing_id.is_none() {
                    ensure_auth_pairing_offer_capacity_on(&transaction, &principal_id)?;
                }
                let pairing_id = existing_pairing_id.flatten();
                let revoked = if let Some(pairing_id) = &pairing_id {
                    transaction
                        .query_row(
                            "UPDATE auth_pairing_links SET revoked_at = ?
                             WHERE id = ? AND revoked_at IS NULL AND consumed_at IS NULL
                             RETURNING id",
                            params![cancelled_at, pairing_id],
                            |row| row.get::<_, String>(0),
                        )
                        .optional()?
                        .is_some()
                } else {
                    false
                };
                transaction.execute(
                    "INSERT INTO auth_pairing_offer_idempotency (
                        principal_id, idempotency_key, input_fingerprint, pairing_id,
                        result_json, expires_at, cancelled_at
                     ) VALUES (?, ?, '', NULL, NULL, ?, ?)
                     ON CONFLICT (principal_id, idempotency_key) DO UPDATE SET
                        input_fingerprint = '', pairing_id = NULL, result_json = NULL,
                        expires_at = excluded.expires_at, cancelled_at = excluded.cancelled_at",
                    params![principal_id, idempotency_key, expires_at, cancelled_at],
                )?;
                bump_auth_authority_revision_on(&transaction)?;
                transaction.commit()?;
                Ok(AuthPairingOfferCancellation {
                    pairing_id,
                    revoked,
                })
            })
            .await
    }
    pub async fn consume_auth_pairing_link(
        &self,
        credential: String,
        proof_key_thumbprint: Option<String>,
        consumed_at: Timestamp,
        now: Timestamp,
    ) -> Result<Option<AuthPairingLink>> {
        self.database
            .call(move |connection| {
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let pairing = transaction
                    .query_row(
                        PAIRING_RETURNING_SQL,
                        params![consumed_at, credential, now, proof_key_thumbprint],
                        decode_pairing_link,
                    )
                    .optional()?;
                if pairing.is_some() {
                    bump_auth_authority_revision_on(&transaction)?;
                }
                transaction.commit()?;
                Ok(pairing)
            })
            .await
    }
    pub async fn list_active_auth_pairing_links(
        &self,
        now: Timestamp,
    ) -> Result<Vec<AuthPairingLink>> {
        self.database.call(move |connection| collect(connection, &(PAIRING_SELECT.to_owned() + " WHERE revoked_at IS NULL AND consumed_at IS NULL AND expires_at > ? ORDER BY created_at DESC, id DESC"), [now], decode_pairing_link)).await
    }
    pub async fn revoke_auth_pairing_link(
        &self,
        id: String,
        revoked_at: Timestamp,
    ) -> Result<bool> {
        self.database
            .call(move |connection| {
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let revoked = transaction
                    .query_row(
                        "UPDATE auth_pairing_links SET revoked_at = ? \
                 WHERE id = ? AND revoked_at IS NULL AND consumed_at IS NULL \
                 RETURNING id",
                        params![revoked_at, id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?
                    .is_some();
                if revoked {
                    bump_auth_authority_revision_on(&transaction)?;
                }
                transaction.commit()?;
                Ok(revoked)
            })
            .await
    }
    pub async fn get_auth_pairing_link_by_credential(
        &self,
        credential: String,
    ) -> Result<Option<AuthPairingLink>> {
        self.database
            .call(move |connection| {
                connection
                    .query_row(
                        &(PAIRING_SELECT.to_owned() + " WHERE credential = ?"),
                        [credential],
                        decode_pairing_link,
                    )
                    .optional()
                    .map_err(Into::into)
            })
            .await
    }

    pub async fn create_auth_session(&self, row: NewAuthSession) -> Result<()> {
        self.database.call(move |connection| {
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            transaction.execute("INSERT INTO auth_sessions (session_id, subject, scopes, method, client_label, client_ip_address, client_user_agent, client_device_type, client_os, client_browser, issued_at, expires_at, revoked_at, reach, off_host, delivery_state) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?)", params![row.session_id,row.subject,encode_json(&row.scopes)?,row.method,row.client.label,row.client.ip_address,row.client.user_agent,row.client.device_type,row.client.os,row.client.browser,row.issued_at,row.expires_at,row.reach,row.off_host,row.delivery_state.as_str()])?;
            bump_auth_authority_revision_on(&transaction)?;
            transaction.commit()?;
            Ok(())
        }).await
    }
    pub async fn get_auth_session(&self, session_id: String) -> Result<Option<AuthSession>> {
        self.database
            .call(move |connection| {
                connection
                    .query_row(
                        &(AUTH_SESSION_SELECT.to_owned() + " WHERE session_id = ?"),
                        [session_id],
                        decode_auth_session,
                    )
                    .optional()
                    .map_err(Into::into)
            })
            .await
    }
    pub async fn list_active_auth_sessions(&self, now: Timestamp) -> Result<Vec<AuthSession>> {
        self.database.call(move |connection| collect(connection, &(AUTH_SESSION_SELECT.to_owned() + " WHERE revoked_at IS NULL AND expires_at > ? ORDER BY issued_at DESC, session_id DESC"), [now], decode_auth_session)).await
    }
    pub async fn revoke_auth_session(
        &self,
        session_id: String,
        revoked_at: Timestamp,
    ) -> Result<bool> {
        self.database
            .call(move |connection| {
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let revoked = transaction
                    .query_row(
                        "UPDATE auth_sessions SET revoked_at = ? \
                 WHERE session_id = ? AND revoked_at IS NULL \
                 RETURNING session_id",
                        params![revoked_at, session_id],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?
                    .is_some();
                if revoked {
                    bump_auth_authority_revision_on(&transaction)?;
                }
                transaction.commit()?;
                Ok(revoked)
            })
            .await
    }
    pub async fn confirm_pending_auth_session(
        &self,
        session_id: String,
        now: Timestamp,
    ) -> Result<bool> {
        self.database
            .call(move |connection| {
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let confirmed = transaction
                    .query_row(
                        "UPDATE auth_sessions SET delivery_state = ?
                         WHERE session_id = ? AND revoked_at IS NULL AND expires_at > ?
                           AND delivery_state = ?
                         RETURNING session_id",
                        params![
                            AuthSessionDeliveryState::Active.as_str(),
                            session_id,
                            now,
                            AuthSessionDeliveryState::PendingPairing.as_str()
                        ],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?
                    .is_some();
                let available = if confirmed {
                    bump_auth_authority_revision_on(&transaction)?;
                    true
                } else {
                    transaction
                        .query_row(
                            "SELECT 1 FROM auth_sessions
                             WHERE session_id = ? AND revoked_at IS NULL AND expires_at > ?
                               AND delivery_state = ?",
                            params![session_id, now, AuthSessionDeliveryState::Active.as_str()],
                            |_| Ok(()),
                        )
                        .optional()?
                        .is_some()
                };
                transaction.commit()?;
                Ok(available)
            })
            .await
    }
    pub async fn revoke_pending_auth_session(
        &self,
        session_id: String,
        revoked_at: Timestamp,
    ) -> Result<bool> {
        self.database
            .call(move |connection| {
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let revoked = transaction
                    .query_row(
                        "UPDATE auth_sessions SET revoked_at = ?
                         WHERE session_id = ? AND revoked_at IS NULL
                           AND delivery_state = ?
                         RETURNING session_id",
                        params![
                            revoked_at,
                            session_id,
                            AuthSessionDeliveryState::PendingPairing.as_str()
                        ],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()?
                    .is_some();
                if revoked {
                    bump_auth_authority_revision_on(&transaction)?;
                }
                transaction.commit()?;
                Ok(revoked)
            })
            .await
    }
    /// Revokes unconfirmed pairing sessions issued at or before the cutoff.
    /// The age gate keeps a sweep from racing a confirmation that is still in
    /// flight on a live connection.
    pub async fn revoke_pending_auth_sessions(
        &self,
        revoked_at: Timestamp,
        issued_at_or_before: Timestamp,
    ) -> Result<Vec<String>> {
        self.database
            .call(move |connection| {
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let revoked = {
                    let mut statement = transaction.prepare(
                        "UPDATE auth_sessions SET revoked_at = ?
                         WHERE revoked_at IS NULL AND delivery_state = ?
                           AND issued_at <= ?
                         RETURNING session_id",
                    )?;
                    statement
                        .query_map(
                            params![
                                revoked_at,
                                AuthSessionDeliveryState::PendingPairing.as_str(),
                                issued_at_or_before
                            ],
                            |row| row.get(0),
                        )?
                        .collect::<rusqlite::Result<Vec<_>>>()?
                };
                if !revoked.is_empty() {
                    bump_auth_authority_revision_on(&transaction)?;
                }
                transaction.commit()?;
                Ok(revoked)
            })
            .await
    }
    pub async fn revoke_other_auth_sessions(
        &self,
        current_session_id: String,
        revoked_at: Timestamp,
    ) -> Result<Vec<String>> {
        self.database.call(move |connection| {
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let revoked = {
                let mut statement = transaction.prepare("UPDATE auth_sessions SET revoked_at = ? WHERE session_id <> ? AND revoked_at IS NULL RETURNING session_id")?;
                statement.query_map(params![revoked_at,current_session_id], |row| row.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?
            };
            if !revoked.is_empty() {
                bump_auth_authority_revision_on(&transaction)?;
            }
            transaction.commit()?;
            Ok(revoked)
        }).await
    }
    pub async fn set_auth_session_last_connected_at(
        &self,
        session_id: String,
        last_connected_at: Timestamp,
    ) -> Result<()> {
        self.database.call(move |connection| {
            let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let changed = transaction.execute("UPDATE auth_sessions SET last_connected_at = ? WHERE session_id = ? AND revoked_at IS NULL", params![last_connected_at,session_id])?;
            if changed > 0 {
                bump_auth_authority_revision_on(&transaction)?;
            }
            transaction.commit()?;
            Ok(())
        }).await
    }

    async fn delete(&self, sql: &'static str, id: String) -> Result<()> {
        self.database
            .call(move |connection| {
                connection.execute(sql, [id])?;
                Ok(())
            })
            .await
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewOrchestrationEvent {
    pub event_id: String,
    pub event_type: String,
    pub aggregate_kind: String,
    pub aggregate_id: String,
    pub occurred_at: Timestamp,
    pub command_id: Option<String>,
    pub causation_event_id: Option<String>,
    pub correlation_id: Option<String>,
    pub payload: Value,
    pub metadata: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OrchestrationEvent {
    pub sequence: i64,
    #[serde(flatten)]
    pub event: NewOrchestrationEvent,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommandReceipt {
    pub command_id: String,
    pub aggregate_kind: String,
    pub aggregate_id: String,
    pub accepted_at: Timestamp,
    pub result_sequence: i64,
    pub status: String,
    pub error: Option<String>,
    pub payload_digest: Option<String>,
}

pub(crate) fn finalize_command_receipt_on(
    connection: &Connection,
    receipt: CommandReceipt,
) -> Result<()> {
    let command_id = receipt.command_id.clone();
    let changed = connection.execute(
        "INSERT INTO orchestration_command_receipts (command_id, aggregate_kind, aggregate_id, accepted_at, result_sequence, status, error, payload_digest) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT (command_id) DO UPDATE SET \
           aggregate_kind = excluded.aggregate_kind, aggregate_id = excluded.aggregate_id, accepted_at = excluded.accepted_at, \
           result_sequence = excluded.result_sequence, status = excluded.status, error = excluded.error, payload_digest = excluded.payload_digest \
         WHERE orchestration_command_receipts.status IN ('reserved', 'prepared') \
           AND orchestration_command_receipts.aggregate_kind = excluded.aggregate_kind \
           AND orchestration_command_receipts.aggregate_id = excluded.aggregate_id \
           AND orchestration_command_receipts.payload_digest IS excluded.payload_digest",
        params![
            receipt.command_id,
            receipt.aggregate_kind,
            receipt.aggregate_id,
            receipt.accepted_at,
            receipt.result_sequence,
            receipt.status,
            receipt.error,
            receipt.payload_digest
        ],
    )?;
    if changed == 1 {
        Ok(())
    } else {
        Err(PersistenceError::CommandReceiptConflict(command_id))
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorktreeRemovalReceipt {
    pub owner_thread_id: String,
    pub project_cwd: String,
    pub worktree_path: String,
    pub identity_nonce: String,
    pub state: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckpointDiffBlob {
    pub thread_id: String,
    pub from_turn_count: i64,
    pub to_turn_count: i64,
    pub diff: String,
    pub created_at: Timestamp,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderSessionRuntime {
    pub thread_id: String,
    pub provider_name: String,
    pub provider_instance_id: Option<String>,
    pub adapter_key: String,
    pub runtime_mode: String,
    pub status: String,
    pub last_seen_at: Timestamp,
    pub resume_cursor: Option<Value>,
    pub runtime_payload: Option<Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectionProject {
    pub project_id: String,
    pub title: String,
    pub workspace_root: String,
    pub default_model_selection: Option<Value>,
    pub scripts: Value,
    pub worktree_discovery: Value,
    pub worktree_repository_key: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub deleted_at: Option<Timestamp>,
    pub repository_identity: Option<Value>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorktreeRepositoryPinOutcome {
    Established,
    Matched,
    Mismatch { pinned_repository_key: String },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectionThread {
    pub thread_id: String,
    pub project_id: String,
    pub title: String,
    pub kind: String,
    pub model_selection: Value,
    pub runtime_mode: String,
    pub interaction_mode: String,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub latest_turn_id: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub archived_at: Option<Timestamp>,
    pub latest_user_message_at: Option<Timestamp>,
    pub pending_approval_count: i64,
    pub pending_user_input_count: i64,
    pub has_actionable_proposed_plan: i64,
    /// The state of the newest delivery on this thread that the user still has
    /// to resolve (`failed` or `uncertain`), or `None` when there is none.
    /// Derived from the outbox on every delivery transition, so it clears itself.
    pub unresolved_delivery_state: Option<String>,
    pub unresolved_delivery_detail: Option<String>,
    pub deleted_at: Option<Timestamp>,
    /// Host thread of a `panel` thread, recorded by `worktree.createPanel`.
    #[serde(default)]
    pub host_thread_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorktreeCatalogProjection {
    pub project: ProjectionProject,
    pub threads: Vec<ProjectionThread>,
    pub truncated: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectionThreadMessage {
    pub message_id: String,
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub role: String,
    pub text: String,
    pub attachments: Option<Value>,
    pub is_streaming: bool,
    pub delivery_state: Option<String>,
    pub delivery_provider: Option<String>,
    pub delivery_provider_instance_id: Option<String>,
    pub delivery_detail: Option<String>,
    pub delivery_reason: Option<String>,
    pub delivery_mode: Option<String>,
    pub delivery_held: Option<bool>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}
/// The rows a shell snapshot shows; see [`Repositories::load_shell_snapshot_rows`].
#[derive(Clone, Debug)]
pub struct ShellSnapshotRows {
    pub projects: Vec<ProjectionProject>,
    pub threads: Vec<ProjectionThread>,
    pub sessions: Vec<ProjectionThreadSession>,
    pub latest_turns: Vec<ProjectionTurn>,
    pub preview_messages: Vec<ProjectionThreadMessage>,
    pub preview_activities: Vec<ProjectionThreadActivity>,
    pub states: Vec<ProjectionState>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectionThreadActivity {
    pub activity_id: String,
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub tone: String,
    pub kind: String,
    pub summary: String,
    pub payload: Value,
    pub sequence: Option<i64>,
    pub created_at: Timestamp,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectionThreadSession {
    pub thread_id: String,
    pub status: String,
    pub provider_name: Option<String>,
    pub provider_instance_id: Option<String>,
    pub runtime_mode: String,
    pub active_turn_id: Option<String>,
    pub last_error: Option<String>,
    pub last_error_class: Option<String>,
    pub updated_at: Timestamp,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectionPendingApproval {
    pub request_id: String,
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub status: String,
    pub decision: Option<String>,
    pub created_at: Timestamp,
    pub resolved_at: Option<Timestamp>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectionState {
    pub projector: String,
    pub last_applied_sequence: i64,
    pub updated_at: Timestamp,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectionThreadProposedPlan {
    pub plan_id: String,
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub plan_markdown: String,
    pub implemented_at: Option<Timestamp>,
    pub implementation_thread_id: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectionTurn {
    pub thread_id: String,
    pub turn_id: Option<String>,
    pub pending_message_id: Option<String>,
    pub source_proposed_plan_thread_id: Option<String>,
    pub source_proposed_plan_id: Option<String>,
    pub assistant_message_id: Option<String>,
    pub state: String,
    pub requested_at: Timestamp,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
    pub checkpoint_turn_count: Option<i64>,
    pub checkpoint_ref: Option<String>,
    pub checkpoint_status: Option<String>,
    pub checkpoint_files: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectionTurnById {
    pub thread_id: String,
    pub turn_id: String,
    pub pending_message_id: Option<String>,
    pub source_proposed_plan_thread_id: Option<String>,
    pub source_proposed_plan_id: Option<String>,
    pub assistant_message_id: Option<String>,
    pub state: String,
    pub requested_at: Timestamp,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
    pub checkpoint_turn_count: Option<i64>,
    pub checkpoint_ref: Option<String>,
    pub checkpoint_status: Option<String>,
    pub checkpoint_files: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectionPendingTurnStart {
    pub thread_id: String,
    pub message_id: String,
    pub source_proposed_plan_thread_id: Option<String>,
    pub source_proposed_plan_id: Option<String>,
    pub requested_at: Timestamp,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectionCheckpoint {
    pub thread_id: String,
    pub turn_id: String,
    pub checkpoint_turn_count: i64,
    pub checkpoint_ref: String,
    pub status: String,
    pub files: Value,
    pub assistant_message_id: Option<String>,
    pub completed_at: Timestamp,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthPairingLink {
    pub id: String,
    pub credential: String,
    pub method: String,
    pub scopes: Value,
    pub subject: String,
    pub label: Option<String>,
    pub proof_key_thumbprint: Option<String>,
    pub created_at: Timestamp,
    pub expires_at: Timestamp,
    pub consumed_at: Option<Timestamp>,
    pub revoked_at: Option<Timestamp>,
    pub reach: Option<String>,
    pub off_host: Option<bool>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewAuthPairingOffer {
    pub principal_id: String,
    pub idempotency_key: String,
    pub input_fingerprint: String,
    pub expires_at: Timestamp,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthPairingOffer {
    pub principal_id: String,
    pub idempotency_key: String,
    pub input_fingerprint: String,
    pub pairing_id: Option<String>,
    pub result: Option<Value>,
    pub expires_at: Timestamp,
    pub cancelled_at: Option<Timestamp>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthPairingOfferReservation {
    pub offer: AuthPairingOffer,
    pub pairing: Option<AuthPairingLink>,
    pub reserved: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthAuthoritySnapshot {
    pub revision: u64,
    pub next_expiry_at: Option<Timestamp>,
    pub pairings: Vec<AuthPairingLink>,
    pub offers: Vec<AuthPairingOffer>,
    pub sessions: Vec<AuthSession>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthPairingOfferCancellation {
    pub pairing_id: Option<String>,
    pub revoked: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthSessionClient {
    pub label: Option<String>,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
    pub device_type: String,
    pub os: Option<String>,
    pub browser: Option<String>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthSessionDeliveryState {
    Active,
    PendingPairing,
}

impl AuthSessionDeliveryState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::PendingPairing => "pending-pairing",
        }
    }
}

impl rusqlite::types::FromSql for AuthSessionDeliveryState {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        match value.as_str()? {
            "active" => Ok(Self::Active),
            "pending-pairing" => Ok(Self::PendingPairing),
            value => Err(rusqlite::types::FromSqlError::Other(
                format!("invalid auth session delivery state: {value}").into(),
            )),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewAuthSession {
    pub session_id: String,
    pub subject: String,
    pub scopes: Value,
    pub method: String,
    pub client: AuthSessionClient,
    pub issued_at: Timestamp,
    pub expires_at: Timestamp,
    pub reach: Option<String>,
    pub off_host: Option<bool>,
    pub delivery_state: AuthSessionDeliveryState,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuthSession {
    pub session_id: String,
    pub subject: String,
    pub scopes: Value,
    pub method: String,
    pub client: AuthSessionClient,
    pub issued_at: Timestamp,
    pub expires_at: Timestamp,
    pub last_connected_at: Option<Timestamp>,
    pub revoked_at: Option<Timestamp>,
    pub reach: Option<String>,
    pub off_host: Option<bool>,
    pub delivery_state: AuthSessionDeliveryState,
}

const PROJECT_SELECT: &str = "SELECT project_id, title, workspace_root, default_model_selection_json, scripts_json, worktree_discovery_json, (SELECT repository_key FROM project_worktree_repository_pins WHERE project_id = projection_projects.project_id), created_at, updated_at, deleted_at, repository_identity_json FROM projection_projects";
const THREAD_SELECT: &str = "SELECT thread_id, project_id, title, kind, model_selection_json, runtime_mode, interaction_mode, branch, worktree_path, latest_turn_id, created_at, updated_at, archived_at, latest_user_message_at, pending_approval_count, pending_user_input_count, has_actionable_proposed_plan, unresolved_delivery_state, unresolved_delivery_detail, deleted_at, host_thread_id FROM projection_threads";
const PROJECT_LIST_ORDER: &str = " ORDER BY created_at ASC, project_id ASC";
const MESSAGE_SELECT: &str = "SELECT message_id, thread_id, turn_id, role, text, attachments_json, is_streaming, delivery_state, delivery_provider, delivery_detail, created_at, updated_at, delivery_mode, delivery_held, delivery_reason, delivery_provider_instance_id FROM projection_thread_messages";
const MESSAGE_LIST_ORDER: &str = " ORDER BY CASE WHEN delivery_state = 'queued' THEN (SELECT receipt.result_sequence FROM provider_turn_outbox AS queued JOIN orchestration_command_receipts AS receipt ON receipt.command_id = queued.command_id WHERE queued.message_id = projection_thread_messages.message_id) END ASC, created_at ASC, message_id ASC";
const ACTIVITY_SELECT: &str = "SELECT activity_id, thread_id, turn_id, tone, kind, summary, payload_json, sequence, created_at FROM projection_thread_activities";
/// Activities without a provider sequence come first: SQLite sorts NULL before any value in
/// ascending order. Spelled this way, the order is that of
/// `idx_projection_thread_activities_thread_sequence_created_id`, so a thread's activities
/// stream from the index instead of going through a temporary sort, which spills their
/// payloads to a temporary file once they outgrow the page cache.
const ACTIVITY_LIST_ORDER: &str = " ORDER BY sequence ASC, created_at ASC, activity_id ASC";
const THREAD_SESSION_SELECT: &str = "SELECT thread_id, status, provider_name, provider_instance_id, runtime_mode, active_turn_id, last_error, last_error_class, updated_at FROM projection_thread_sessions";
const PROJECTION_STATE_LIST: &str = "SELECT projector, last_applied_sequence, updated_at FROM projection_state ORDER BY projector ASC";
const PROVIDER_TURN_DELIVERY_SELECT: &str = "SELECT command_id, thread_id, message_id, provider_instance_id, provider_kind, provider_session_id, delivery_key, payload_json, state, attempts, last_error, created_at, updated_at, mode, held FROM provider_turn_outbox";
const TURN_SELECT: &str = "SELECT thread_id, turn_id, pending_message_id, source_proposed_plan_thread_id, source_proposed_plan_id, assistant_message_id, state, requested_at, started_at, completed_at, checkpoint_turn_count, checkpoint_ref, checkpoint_status, checkpoint_files_json FROM projection_turns";
const TURN_UPSERT_SQL: &str = "INSERT INTO projection_turns (thread_id, turn_id, pending_message_id, source_proposed_plan_thread_id, source_proposed_plan_id, assistant_message_id, state, requested_at, started_at, completed_at, checkpoint_turn_count, checkpoint_ref, checkpoint_status, checkpoint_files_json) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT (thread_id, turn_id) DO UPDATE SET pending_message_id=excluded.pending_message_id, source_proposed_plan_thread_id=excluded.source_proposed_plan_thread_id, source_proposed_plan_id=excluded.source_proposed_plan_id, assistant_message_id=excluded.assistant_message_id, state=excluded.state, requested_at=excluded.requested_at, started_at=excluded.started_at, completed_at=excluded.completed_at, checkpoint_turn_count=excluded.checkpoint_turn_count, checkpoint_ref=excluded.checkpoint_ref, checkpoint_status=excluded.checkpoint_status, checkpoint_files_json=excluded.checkpoint_files_json";
const PAIRING_SELECT: &str = "SELECT id, credential, method, scopes, subject, label, proof_key_thumbprint, created_at, expires_at, consumed_at, revoked_at, reach, off_host FROM auth_pairing_links";
const PAIRING_OFFER_SELECT: &str = "SELECT principal_id, idempotency_key, input_fingerprint, pairing_id, result_json, expires_at, cancelled_at FROM auth_pairing_offer_idempotency WHERE expires_at > ? ORDER BY expires_at, principal_id, idempotency_key";
const PAIRING_OFFER_BY_KEY_SQL: &str = "SELECT principal_id, idempotency_key, input_fingerprint, pairing_id, result_json, expires_at, cancelled_at FROM auth_pairing_offer_idempotency WHERE expires_at > ? AND principal_id = ? AND idempotency_key = ?";
const PAIRING_RETURNING_SQL: &str = "UPDATE auth_pairing_links SET consumed_at = ? WHERE credential = ? AND revoked_at IS NULL AND consumed_at IS NULL AND expires_at > ? AND (proof_key_thumbprint IS NULL OR proof_key_thumbprint = ?) RETURNING id, credential, method, scopes, subject, label, proof_key_thumbprint, created_at, expires_at, consumed_at, revoked_at, reach, off_host";
const AUTH_SESSION_SELECT: &str = "SELECT session_id, subject, scopes, method, client_label, client_ip_address, client_user_agent, client_device_type, client_os, client_browser, issued_at, expires_at, last_connected_at, revoked_at, reach, off_host, delivery_state FROM auth_sessions";

fn collect<T, P>(
    connection: &rusqlite::Connection,
    sql: &str,
    params: P,
    decode: fn(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>>
where
    P: rusqlite::Params,
{
    let mut statement = connection.prepare(sql)?;
    Ok(statement
        .query_map(params, decode)?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}
fn insert_auth_pairing_link_on(connection: &Connection, row: &AuthPairingLink) -> Result<()> {
    connection.execute(
        "INSERT INTO auth_pairing_links (
            id, credential, method, scopes, subject, label, proof_key_thumbprint,
            created_at, expires_at, consumed_at, revoked_at, reach, off_host
         ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, ?, ?)",
        params![
            row.id,
            row.credential,
            row.method,
            encode_json(&row.scopes)?,
            row.subject,
            row.label,
            row.proof_key_thumbprint,
            row.created_at,
            row.expires_at,
            row.reach,
            row.off_host,
        ],
    )?;
    Ok(())
}
fn auth_authority_revision_on(connection: &Connection) -> Result<u64> {
    let revision = connection.query_row(
        "SELECT revision FROM auth_authority_state WHERE singleton = 1",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    u64::try_from(revision)
        .map_err(|_| PersistenceError::Corrupt("auth authority revision is negative".to_owned()))
}
fn bump_auth_authority_revision_on(connection: &Connection) -> Result<()> {
    let changed = connection.execute(
        "UPDATE auth_authority_state SET revision = revision + 1
         WHERE singleton = 1 AND revision < 9223372036854775807",
        [],
    )?;
    if changed == 1 {
        Ok(())
    } else {
        Err(PersistenceError::Corrupt(
            "auth authority revision is unavailable or exhausted".to_owned(),
        ))
    }
}
fn ensure_auth_pairing_offer_capacity_on(
    connection: &Connection,
    principal_id: &str,
) -> Result<()> {
    let principal_count = connection.query_row(
        "SELECT COUNT(*) FROM auth_pairing_offer_idempotency WHERE principal_id = ?",
        [principal_id],
        |row| row.get::<_, i64>(0),
    )?;
    if principal_count
        >= i64::try_from(MAX_ACTIVE_PAIRING_OFFERS_PER_PRINCIPAL)
            .expect("pairing offer principal quota fits i64")
    {
        return Err(PersistenceError::PairingOfferPrincipalCapacityExceeded);
    }
    let global_count = connection.query_row(
        "SELECT COUNT(*) FROM auth_pairing_offer_idempotency",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    if global_count
        >= i64::try_from(MAX_ACTIVE_PAIRING_OFFERS).expect("pairing offer global quota fits i64")
    {
        return Err(PersistenceError::PairingOfferGlobalCapacityExceeded);
    }
    Ok(())
}
fn encode_json(value: &Value) -> Result<String> {
    serde_json::to_string(value).map_err(|error| {
        PersistenceError::Corrupt(format!("could not encode JSON for SQLite TEXT: {error}"))
    })
}
fn optional_json(value: &Option<Value>) -> Result<Option<String>> {
    value.as_ref().map(encode_json).transpose()
}
fn decode_json(value: String, _column: &str) -> rusqlite::Result<Value> {
    serde_json::from_str(&value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}
fn decode_optional_json(value: Option<String>, column: &str) -> rusqlite::Result<Option<Value>> {
    value.map(|value| decode_json(value, column)).transpose()
}

fn infer_actor_kind(event: &NewOrchestrationEvent) -> &'static str {
    match event.command_id.as_deref() {
        Some(command_id) if command_id.starts_with("provider:") => "provider",
        Some(command_id) if command_id.starts_with("server:") => "server",
        Some(_) => "client",
        None if event.metadata.get("providerTurnId").is_some()
            || event.metadata.get("providerItemId").is_some()
            || event.metadata.get("adapterKey").is_some() =>
        {
            "provider"
        }
        None => "server",
    }
}

fn decode_event(row: &Row<'_>) -> rusqlite::Result<OrchestrationEvent> {
    Ok(OrchestrationEvent {
        sequence: row.get(0)?,
        event: NewOrchestrationEvent {
            event_id: row.get(1)?,
            event_type: row.get(2)?,
            aggregate_kind: row.get(3)?,
            aggregate_id: row.get(4)?,
            occurred_at: row.get(5)?,
            command_id: row.get(6)?,
            causation_event_id: row.get(7)?,
            correlation_id: row.get(8)?,
            payload: decode_json(row.get(9)?, "payload_json")?,
            metadata: decode_json(row.get(10)?, "metadata_json")?,
        },
    })
}
fn decode_command_receipt(row: &Row<'_>) -> rusqlite::Result<CommandReceipt> {
    Ok(CommandReceipt {
        command_id: row.get(0)?,
        aggregate_kind: row.get(1)?,
        aggregate_id: row.get(2)?,
        accepted_at: row.get(3)?,
        result_sequence: row.get(4)?,
        status: row.get(5)?,
        error: row.get(6)?,
        payload_digest: row.get(7)?,
    })
}
fn decode_worktree_removal_receipt(row: &Row<'_>) -> rusqlite::Result<WorktreeRemovalReceipt> {
    Ok(WorktreeRemovalReceipt {
        owner_thread_id: row.get(0)?,
        project_cwd: row.get(1)?,
        worktree_path: row.get(2)?,
        identity_nonce: row.get(3)?,
        state: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}
fn decode_checkpoint_diff_blob(row: &Row<'_>) -> rusqlite::Result<CheckpointDiffBlob> {
    Ok(CheckpointDiffBlob {
        thread_id: row.get(0)?,
        from_turn_count: row.get(1)?,
        to_turn_count: row.get(2)?,
        diff: row.get(3)?,
        created_at: row.get(4)?,
    })
}
fn decode_provider_runtime(row: &Row<'_>) -> rusqlite::Result<ProviderSessionRuntime> {
    Ok(ProviderSessionRuntime {
        thread_id: row.get(0)?,
        provider_name: row.get(1)?,
        provider_instance_id: row.get(2)?,
        adapter_key: row.get(3)?,
        runtime_mode: row.get(4)?,
        status: row.get(5)?,
        last_seen_at: row.get(6)?,
        resume_cursor: decode_optional_json(row.get(7)?, "resume_cursor_json")?,
        runtime_payload: decode_optional_json(row.get(8)?, "runtime_payload_json")?,
    })
}
fn decode_project(row: &Row<'_>) -> rusqlite::Result<ProjectionProject> {
    Ok(ProjectionProject {
        project_id: row.get(0)?,
        title: row.get(1)?,
        workspace_root: row.get(2)?,
        default_model_selection: decode_optional_json(row.get(3)?, "default_model_selection_json")?,
        scripts: decode_json(row.get(4)?, "scripts_json")?,
        worktree_discovery: decode_json(row.get(5)?, "worktree_discovery_json")?,
        worktree_repository_key: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        deleted_at: row.get(9)?,
        repository_identity: decode_optional_json(row.get(10)?, "repository_identity_json")?,
    })
}
fn decode_thread(row: &Row<'_>) -> rusqlite::Result<ProjectionThread> {
    Ok(ProjectionThread {
        thread_id: row.get(0)?,
        project_id: row.get(1)?,
        title: row.get(2)?,
        kind: row.get(3)?,
        model_selection: decode_json(row.get(4)?, "model_selection_json")?,
        runtime_mode: row.get(5)?,
        interaction_mode: row.get(6)?,
        branch: row.get(7)?,
        worktree_path: row.get(8)?,
        latest_turn_id: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
        archived_at: row.get(12)?,
        latest_user_message_at: row.get(13)?,
        pending_approval_count: row.get(14)?,
        pending_user_input_count: row.get(15)?,
        has_actionable_proposed_plan: row.get(16)?,
        unresolved_delivery_state: row.get(17)?,
        unresolved_delivery_detail: row.get(18)?,
        deleted_at: row.get(19)?,
        host_thread_id: row.get(20)?,
    })
}
fn decode_message(row: &Row<'_>) -> rusqlite::Result<ProjectionThreadMessage> {
    Ok(ProjectionThreadMessage {
        message_id: row.get(0)?,
        thread_id: row.get(1)?,
        turn_id: row.get(2)?,
        role: row.get(3)?,
        text: row.get(4)?,
        attachments: decode_optional_json(row.get(5)?, "attachments_json")?,
        is_streaming: row.get::<_, i64>(6)? == 1,
        delivery_state: row.get(7)?,
        delivery_provider: row.get(8)?,
        delivery_provider_instance_id: row.get(15)?,
        delivery_detail: row.get(9)?,
        delivery_reason: row.get(14)?,
        delivery_mode: row.get(12)?,
        delivery_held: row.get(13)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

/// The caller owns the transaction that also appends and projects the hold events.
pub(crate) fn hold_queued_provider_turns(
    connection: &Connection,
    thread_id: &str,
    updated_at: &str,
) -> Result<Vec<String>> {
    let command_ids = collect(
        connection,
        "SELECT command_id FROM provider_turn_outbox WHERE thread_id = ? AND (state = 'queued' OR (mode = 'steer' AND state IN ('pending', 'sending'))) ORDER BY created_at, command_id",
        [thread_id],
        |row| row.get(0),
    )?;
    connection.execute("UPDATE provider_turn_outbox SET held = 1, updated_at = ? WHERE thread_id = ? AND (state = 'queued' OR (mode = 'steer' AND state IN ('pending', 'sending')))", params![updated_at, thread_id])?;
    Ok(command_ids)
}

pub(crate) fn requeue_provider_turn_on(
    connection: &Connection,
    command_id: &str,
    expected_attempt: i64,
    updated_at: &str,
) -> Result<Option<ProviderTurnDelivery>> {
    connection.query_row(
        "UPDATE provider_turn_outbox SET state = 'queued', mode = 'start', attempts = 0, last_error = NULL, updated_at = ? WHERE command_id = ? AND mode = 'steer' AND state IN ('pending', 'sending') AND attempts = ? RETURNING command_id, thread_id, message_id, provider_instance_id, provider_kind, provider_session_id, delivery_key, payload_json, state, attempts, last_error, created_at, updated_at, mode, held",
        params![updated_at, command_id, expected_attempt], decode_provider_turn_delivery,
    ).optional().map_err(Into::into)
}

pub(crate) fn provider_turn_steer_target_on(
    connection: &Connection,
    thread_id: &str,
    message_id: &str,
) -> Result<Option<String>> {
    // The accepted steer event owns the target; never retarget a later running turn.
    Ok(connection
        .query_row(
            "SELECT json_extract(payload_json, '$.turnId') FROM orchestration_events
         WHERE stream_id = ? AND event_type = 'thread.turn-steer-requested'
           AND json_extract(payload_json, '$.messageId') = ? ORDER BY sequence DESC LIMIT 1",
            params![thread_id, message_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten()
        .filter(|id| !id.trim().is_empty()))
}

pub(crate) fn queued_provider_turn_head_on(
    connection: &Connection,
    thread_id: &str,
) -> Result<Option<ProviderTurnDelivery>> {
    connection
        .query_row(
            &(PROVIDER_TURN_DELIVERY_SELECT.to_owned()
                + " WHERE command_id = (
            SELECT queued.command_id FROM provider_turn_outbox AS queued
            JOIN orchestration_command_receipts AS receipt ON receipt.command_id = queued.command_id
            WHERE queued.thread_id = ? AND queued.state = 'queued'
            ORDER BY receipt.result_sequence, queued.created_at, queued.command_id LIMIT 1
        )"),
            [thread_id],
            decode_provider_turn_delivery,
        )
        .optional()
        .map_err(Into::into)
}

pub(crate) fn can_promote_queued_provider_turn_on(
    connection: &Connection,
    thread_id: &str,
    message_id: &str,
    automatic: bool,
) -> Result<bool> {
    connection.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM provider_turn_outbox AS queued
           JOIN projection_threads AS thread ON thread.thread_id = queued.thread_id AND thread.deleted_at IS NULL
           LEFT JOIN projection_thread_sessions AS session ON session.thread_id = queued.thread_id
           WHERE queued.thread_id = ? AND queued.message_id = ? AND queued.state = 'queued'
             AND COALESCE(session.status, 'idle') NOT IN ('running', 'starting')
             AND (? = 0 OR (
               session.status = 'ready' AND queued.held = 0
               -- A closed (archived) chat panel keeps its queue until it is reopened.
               AND NOT (thread.kind = 'panel' AND thread.archived_at IS NOT NULL)
               AND NOT EXISTS(SELECT 1 FROM projection_turns AS turn WHERE turn.thread_id = queued.thread_id AND turn.state = 'running'
                 AND NOT (turn.turn_id IS NULL AND EXISTS(SELECT 1 FROM provider_turn_outbox AS dismissed
                   WHERE dismissed.thread_id = turn.thread_id AND dismissed.message_id = turn.pending_message_id AND dismissed.state = 'dismissed'
                     AND EXISTS(SELECT 1 FROM orchestration_events AS resolution
                       WHERE resolution.aggregate_kind = 'thread' AND resolution.stream_id = dismissed.thread_id
                         AND resolution.event_type = 'thread.turn-delivery-updated'
                         AND json_extract(resolution.metadata_json, '$.deliveryCommandId') = dismissed.command_id
                         AND json_extract(resolution.payload_json, '$.state') = 'dismissed'
                         AND json_extract(resolution.metadata_json, '$.deliveryPreviousState') IN ('pending', 'failed')))))
               AND NOT EXISTS(SELECT 1 FROM projection_pending_approvals WHERE thread_id = queued.thread_id AND status = 'pending')
               AND NOT EXISTS(
                 SELECT 1 FROM (
                   SELECT kind, lower(COALESCE(json_extract(payload_json, '$.detail'), '')) AS detail,
                     ROW_NUMBER() OVER (PARTITION BY json_extract(payload_json, '$.requestId') ORDER BY COALESCE(sequence, 0) DESC, created_at DESC, activity_id DESC) AS position
                   FROM projection_thread_activities WHERE thread_id = queued.thread_id
                     AND json_extract(payload_json, '$.requestId') IS NOT NULL
                     AND kind IN ('user-input.requested', 'user-input.resolved', 'provider.user-input.respond.failed')
                 ) WHERE position = 1 AND (kind = 'user-input.requested' OR (kind = 'provider.user-input.respond.failed'
                   AND detail NOT LIKE '%stale pending user-input request%' AND detail NOT LIKE '%unknown pending user-input request%'))
               )
             ))
             AND NOT EXISTS(SELECT 1 FROM provider_turn_outbox AS active
               WHERE active.thread_id = queued.thread_id AND active.state IN ('pending', 'sending'))
             AND queued.command_id = (
               SELECT next.command_id FROM provider_turn_outbox AS next
               JOIN orchestration_command_receipts AS receipt ON receipt.command_id = next.command_id
               WHERE next.thread_id = queued.thread_id AND next.state = 'queued'
               ORDER BY receipt.result_sequence, next.created_at, next.command_id LIMIT 1
             )
         )",
        params![thread_id, message_id, automatic], |row| row.get(0),
    ).map_err(Into::into)
}

fn decode_provider_turn_delivery(row: &Row<'_>) -> rusqlite::Result<ProviderTurnDelivery> {
    let state = match row.get::<_, String>(8)?.as_str() {
        "queued" => TurnDeliveryState::Queued,
        "pending" => TurnDeliveryState::Pending,
        "sending" => TurnDeliveryState::Sending,
        "delivered" => TurnDeliveryState::Delivered,
        "uncertain" => TurnDeliveryState::Uncertain,
        "dismissed" => TurnDeliveryState::Dismissed,
        "failed" => TurnDeliveryState::Failed,
        state => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                8,
                rusqlite::types::Type::Text,
                format!("unknown turn delivery state: {state}").into(),
            ));
        }
    };
    Ok(ProviderTurnDelivery {
        command_id: row.get(0)?,
        thread_id: row.get(1)?,
        message_id: row.get(2)?,
        provider_instance_id: row.get(3)?,
        provider_kind: row.get(4)?,
        provider_session_id: row.get(5)?,
        delivery_key: row.get(6)?,
        payload: decode_json(row.get(7)?, "payload_json")?,
        state,
        mode: match row.get::<_, String>(13)?.as_str() {
            "start" => TurnDeliveryMode::Start,
            "steer" => TurnDeliveryMode::Steer,
            mode => {
                return Err(rusqlite::Error::FromSqlConversionFailure(
                    13,
                    rusqlite::types::Type::Text,
                    format!("unknown turn delivery mode: {mode}").into(),
                ));
            }
        },
        held: row.get(14)?,
        attempts: row.get(9)?,
        last_error: row.get(10)?,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

impl TurnDeliveryState {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Pending => "pending",
            Self::Sending => "sending",
            Self::Delivered => "delivered",
            Self::Uncertain => "uncertain",
            Self::Dismissed => "dismissed",
            Self::Failed => "failed",
        }
    }
}
fn decode_activity(row: &Row<'_>) -> rusqlite::Result<ProjectionThreadActivity> {
    Ok(ProjectionThreadActivity {
        activity_id: row.get(0)?,
        thread_id: row.get(1)?,
        turn_id: row.get(2)?,
        tone: row.get(3)?,
        kind: row.get(4)?,
        summary: row.get(5)?,
        payload: decode_json(row.get(6)?, "payload_json")?,
        sequence: row.get(7)?,
        created_at: row.get(8)?,
    })
}
fn decode_thread_session(row: &Row<'_>) -> rusqlite::Result<ProjectionThreadSession> {
    Ok(ProjectionThreadSession {
        thread_id: row.get(0)?,
        status: row.get(1)?,
        provider_name: row.get(2)?,
        provider_instance_id: row.get(3)?,
        runtime_mode: row.get(4)?,
        active_turn_id: row.get(5)?,
        last_error: row.get(6)?,
        last_error_class: row.get(7)?,
        updated_at: row.get(8)?,
    })
}
fn decode_pending_approval(row: &Row<'_>) -> rusqlite::Result<ProjectionPendingApproval> {
    Ok(ProjectionPendingApproval {
        request_id: row.get(0)?,
        thread_id: row.get(1)?,
        turn_id: row.get(2)?,
        status: row.get(3)?,
        decision: row.get(4)?,
        created_at: row.get(5)?,
        resolved_at: row.get(6)?,
    })
}
fn decode_projection_state(row: &Row<'_>) -> rusqlite::Result<ProjectionState> {
    Ok(ProjectionState {
        projector: row.get(0)?,
        last_applied_sequence: row.get(1)?,
        updated_at: row.get(2)?,
    })
}
fn decode_proposed_plan(row: &Row<'_>) -> rusqlite::Result<ProjectionThreadProposedPlan> {
    Ok(ProjectionThreadProposedPlan {
        plan_id: row.get(0)?,
        thread_id: row.get(1)?,
        turn_id: row.get(2)?,
        plan_markdown: row.get(3)?,
        implemented_at: row.get(4)?,
        implementation_thread_id: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
    })
}
fn decode_turn(row: &Row<'_>) -> rusqlite::Result<ProjectionTurn> {
    Ok(ProjectionTurn {
        thread_id: row.get(0)?,
        turn_id: row.get(1)?,
        pending_message_id: row.get(2)?,
        source_proposed_plan_thread_id: row.get(3)?,
        source_proposed_plan_id: row.get(4)?,
        assistant_message_id: row.get(5)?,
        state: row.get(6)?,
        requested_at: row.get(7)?,
        started_at: row.get(8)?,
        completed_at: row.get(9)?,
        checkpoint_turn_count: row.get(10)?,
        checkpoint_ref: row.get(11)?,
        checkpoint_status: row.get(12)?,
        checkpoint_files: decode_json(row.get(13)?, "checkpoint_files_json")?,
    })
}
fn decode_turn_by_id(row: &Row<'_>) -> rusqlite::Result<ProjectionTurnById> {
    let turn = decode_turn(row)?;
    Ok(ProjectionTurnById {
        thread_id: turn.thread_id,
        turn_id: turn.turn_id.ok_or(rusqlite::Error::InvalidQuery)?,
        pending_message_id: turn.pending_message_id,
        source_proposed_plan_thread_id: turn.source_proposed_plan_thread_id,
        source_proposed_plan_id: turn.source_proposed_plan_id,
        assistant_message_id: turn.assistant_message_id,
        state: turn.state,
        requested_at: turn.requested_at,
        started_at: turn.started_at,
        completed_at: turn.completed_at,
        checkpoint_turn_count: turn.checkpoint_turn_count,
        checkpoint_ref: turn.checkpoint_ref,
        checkpoint_status: turn.checkpoint_status,
        checkpoint_files: turn.checkpoint_files,
    })
}
fn decode_pending_turn(row: &Row<'_>) -> rusqlite::Result<ProjectionPendingTurnStart> {
    Ok(ProjectionPendingTurnStart {
        thread_id: row.get(0)?,
        message_id: row.get(1)?,
        source_proposed_plan_thread_id: row.get(2)?,
        source_proposed_plan_id: row.get(3)?,
        requested_at: row.get(4)?,
    })
}
fn decode_checkpoint(row: &Row<'_>) -> rusqlite::Result<ProjectionCheckpoint> {
    Ok(ProjectionCheckpoint {
        thread_id: row.get(0)?,
        turn_id: row.get(1)?,
        checkpoint_turn_count: row.get(2)?,
        checkpoint_ref: row.get(3)?,
        status: row.get(4)?,
        files: decode_json(row.get(5)?, "checkpoint_files_json")?,
        assistant_message_id: row.get(6)?,
        completed_at: row.get(7)?,
    })
}
fn decode_pairing_link(row: &Row<'_>) -> rusqlite::Result<AuthPairingLink> {
    Ok(AuthPairingLink {
        id: row.get(0)?,
        credential: row.get(1)?,
        method: row.get(2)?,
        scopes: decode_json(row.get(3)?, "scopes")?,
        subject: row.get(4)?,
        label: row.get(5)?,
        proof_key_thumbprint: row.get(6)?,
        created_at: row.get(7)?,
        expires_at: row.get(8)?,
        consumed_at: row.get(9)?,
        revoked_at: row.get(10)?,
        reach: row.get(11)?,
        off_host: row.get(12)?,
    })
}
fn decode_pairing_offer(row: &Row<'_>) -> rusqlite::Result<AuthPairingOffer> {
    Ok(AuthPairingOffer {
        principal_id: row.get(0)?,
        idempotency_key: row.get(1)?,
        input_fingerprint: row.get(2)?,
        pairing_id: row.get(3)?,
        result: row
            .get::<_, Option<String>>(4)?
            .map(|value| decode_json(value, "result_json"))
            .transpose()?,
        expires_at: row.get(5)?,
        cancelled_at: row.get(6)?,
    })
}
fn decode_auth_session(row: &Row<'_>) -> rusqlite::Result<AuthSession> {
    Ok(AuthSession {
        session_id: row.get(0)?,
        subject: row.get(1)?,
        scopes: decode_json(row.get(2)?, "scopes")?,
        method: row.get(3)?,
        client: AuthSessionClient {
            label: row.get(4)?,
            ip_address: row.get(5)?,
            user_agent: row.get(6)?,
            device_type: row.get(7)?,
            os: row.get(8)?,
            browser: row.get(9)?,
        },
        issued_at: row.get(10)?,
        expires_at: row.get(11)?,
        last_connected_at: row.get(12)?,
        revoked_at: row.get(13)?,
        reach: row.get(14)?,
        off_host: row.get(15)?,
        delivery_state: row.get(16)?,
    })
}

#[cfg(test)]
mod activity_order_tests {
    use super::*;
    use crate::persistence::run_migrations;

    #[tokio::test]
    async fn thread_activities_stream_from_the_sequence_index_without_a_temporary_sort() {
        let database = Database::open_in_memory().await.unwrap();
        let plan = database
            .call(|connection| {
                run_migrations(connection, None)?;
                let mut statement = connection.prepare(&format!(
                    "EXPLAIN QUERY PLAN {ACTIVITY_SELECT} WHERE thread_id = ?{ACTIVITY_LIST_ORDER}"
                ))?;
                let details = statement
                    .query_map(["thread"], |row| row.get::<_, String>(3))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(details)
            })
            .await
            .unwrap();
        assert!(
            plan.iter().any(|detail| detail
                .contains("idx_projection_thread_activities_thread_sequence_created_id")),
            "{plan:?}"
        );
        assert!(
            !plan.iter().any(|detail| detail.contains("TEMP B-TREE")),
            "{plan:?}"
        );
    }
}

#[cfg(test)]
mod frozen_session_unfreeze_tests {
    use super::*;
    use crate::persistence::{ProviderSessionRuntime, run_migrations};
    use serde_json::json;

    async fn fixture() -> Repositories {
        let database = Database::open_in_memory().await.unwrap();
        database.call(|connection| {
            run_migrations(connection, None)?;
            connection.execute(
                "INSERT INTO orchestration_command_receipts (command_id, aggregate_kind, aggregate_id, accepted_at, result_sequence, status, payload_digest) VALUES ('frozen', 'thread', 't1', 'created', 123, 'accepted', 'digest')", [],
            )?;
            connection.execute(
                "INSERT INTO provider_turn_outbox (command_id, thread_id, message_id, provider_instance_id, provider_kind, provider_session_id, delivery_key, payload_json, state, attempts, last_error, created_at, updated_at) VALUES ('frozen', 't1', 'm1', 'cursor-instance', 'cursor', 'old-session', 'same-key', '{\"text\":\"preserve\"}', 'sending', 3, 'previous error', 'created', 'before')", [],
            )?;
            Ok(())
        }).await.unwrap();
        Repositories::new(database)
    }

    async fn unfreeze(repositories: &Repositories) -> Option<ProviderTurnDelivery> {
        repositories
            .unfreeze_provider_turn_session(
                "frozen".to_owned(),
                3,
                "cursor-instance".to_owned(),
                "cursor".to_owned(),
                "old-session".to_owned(),
                "after".to_owned(),
            )
            .await
            .unwrap()
    }

    async fn runtime(
        repositories: &Repositories,
        provider: &str,
        instance: Option<&str>,
        cursor: Option<Value>,
    ) {
        repositories
            .upsert_provider_session_runtime(ProviderSessionRuntime {
                thread_id: "t1".to_owned(),
                provider_name: provider.to_owned(),
                provider_instance_id: instance.map(str::to_owned),
                adapter_key: "cursor-acp".to_owned(),
                runtime_mode: "full-access".to_owned(),
                status: "suspended".to_owned(),
                last_seen_at: "seen".to_owned(),
                resume_cursor: cursor,
                runtime_payload: None,
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn unfreeze_preserves_payload_fifo_receipt_and_attempt_for_missing_or_cursorless_runtime()
    {
        for cursorless_row in [false, true] {
            let repositories = fixture().await;
            if cursorless_row {
                runtime(&repositories, "cursor", Some("cursor-instance"), None).await;
            }
            let row = unfreeze(&repositories)
                .await
                .expect("expected identity unfreezes");
            assert_eq!(row.provider_session_id, None);
            assert_eq!(row.state, TurnDeliveryState::Sending);
            assert_eq!(row.attempts, 3);
            assert_eq!(row.payload, json!({"text":"preserve"}));
            assert_eq!(row.delivery_key, "same-key");
            assert_eq!(row.last_error.as_deref(), Some("previous error"));
            assert_eq!(row.created_at, "created");
            let sequence: i64 = repositories.database.call(|connection| {
                Ok(connection.query_row("SELECT result_sequence FROM orchestration_command_receipts WHERE command_id = 'frozen'", [], |row| row.get(0))?)
            }).await.unwrap();
            assert_eq!(sequence, 123);
            assert!(
                unfreeze(&repositories).await.is_none(),
                "the old freeze cannot clear twice"
            );
        }
    }

    #[tokio::test]
    async fn unfreeze_rejects_wrong_command_attempt_instance_provider_native_session_state_or_mode()
    {
        for (command, attempt, instance, provider, session, state, mode) in [
            (
                "other",
                3,
                "cursor-instance",
                "cursor",
                "old-session",
                "sending",
                "start",
            ),
            (
                "frozen",
                2,
                "cursor-instance",
                "cursor",
                "old-session",
                "sending",
                "start",
            ),
            (
                "frozen",
                3,
                "other",
                "cursor",
                "old-session",
                "sending",
                "start",
            ),
            (
                "frozen",
                3,
                "cursor-instance",
                "claudeAgent",
                "old-session",
                "sending",
                "start",
            ),
            (
                "frozen",
                3,
                "cursor-instance",
                "cursor",
                "other",
                "sending",
                "start",
            ),
            (
                "frozen",
                3,
                "cursor-instance",
                "cursor",
                "old-session",
                "pending",
                "start",
            ),
            (
                "frozen",
                3,
                "cursor-instance",
                "cursor",
                "old-session",
                "delivered",
                "start",
            ),
            (
                "frozen",
                3,
                "cursor-instance",
                "cursor",
                "old-session",
                "uncertain",
                "start",
            ),
            (
                "frozen",
                3,
                "cursor-instance",
                "cursor",
                "old-session",
                "dismissed",
                "start",
            ),
            (
                "frozen",
                3,
                "cursor-instance",
                "cursor",
                "old-session",
                "sending",
                "steer",
            ),
        ] {
            let repositories = fixture().await;
            repositories
                .database
                .call(move |connection| {
                    connection.execute(
                        "UPDATE provider_turn_outbox SET state = ?, mode = ?",
                        params![state, mode],
                    )?;
                    Ok(())
                })
                .await
                .unwrap();
            assert!(
                repositories
                    .unfreeze_provider_turn_session(
                        command.to_owned(),
                        attempt,
                        instance.to_owned(),
                        provider.to_owned(),
                        session.to_owned(),
                        "after".to_owned(),
                    )
                    .await
                    .unwrap()
                    .is_none()
            );
            let row = repositories
                .get_provider_turn_delivery("frozen".to_owned())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(row.provider_session_id.as_deref(), Some("old-session"));
            assert_eq!(row.updated_at, "before");
        }
    }

    #[tokio::test]
    async fn unfreeze_cas_refuses_runtime_resume_or_identity_restored_after_loss_was_observed() {
        for (provider, instance, cursor) in [
            (
                "cursor",
                Some("cursor-instance"),
                Some(json!({"sessionId":"old-session"})),
            ),
            (
                "cursor",
                Some("cursor-instance"),
                Some(json!({"sessionId":"replacement"})),
            ),
            ("cursor", Some("other"), None),
            ("codex", Some("cursor-instance"), None),
            ("cursor", None, None),
        ] {
            let repositories = fixture().await;
            runtime(&repositories, provider, instance, cursor.clone()).await;
            assert!(unfreeze(&repositories).await.is_none());
            assert_eq!(
                repositories
                    .get_provider_turn_delivery("frozen".to_owned())
                    .await
                    .unwrap()
                    .unwrap()
                    .provider_session_id
                    .as_deref(),
                Some("old-session")
            );
            let retained = repositories
                .get_provider_session_runtime("t1".to_owned())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(retained.resume_cursor, cursor);
            assert_eq!(retained.provider_name, provider);
            assert_eq!(retained.provider_instance_id.as_deref(), instance);
        }
    }
}
