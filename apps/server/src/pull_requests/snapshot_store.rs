//! SQLite-backed display snapshots for GitLab merge request reads.

#![allow(dead_code)]

use std::sync::atomic::{AtomicI64, Ordering};

use rusqlite::{Connection, OptionalExtension, Result, params};

pub(crate) const BYTE_CAP: i64 = 33_554_432;
pub(crate) const FILES_PAYLOAD_CAP: usize = 1_048_576;

static LAST_GENERATION: AtomicI64 = AtomicI64::new(0);

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Write order for `put`, taken before the host read starts: epoch
/// milliseconds, bumped past the previous stamp in this process. Rows left by
/// an earlier poller or an earlier server process carry older stamps, so a
/// later read can always replace them; a wall clock moved backwards across a
/// restart holds writes off only until it passes the stored stamps again.
pub(crate) fn next_generation() -> i64 {
    let now = now_ms();
    let previous = LAST_GENERATION
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |last| {
            Some(now.max(last + 1))
        })
        .unwrap_or_else(|last| last);
    now.max(previous + 1)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub host: String,
    pub project: String,
    pub kind: String,
    pub key: String,
    pub fingerprint: String,
    pub payload: Vec<u8>,
    pub observed_at_ms: i64,
    pub generation: i64,
}

pub struct SnapshotStore<'a> {
    connection: &'a Connection,
}

impl<'a> SnapshotStore<'a> {
    #[must_use]
    pub fn new(connection: &'a Connection) -> Self {
        Self { connection }
    }

    pub fn get(
        &self,
        host: &str,
        project: &str,
        kind: &str,
        key: &str,
    ) -> Result<Option<Snapshot>> {
        self.connection
            .query_row(
                "SELECT host, project, kind, key, fingerprint, payload, observed_at_ms, generation
                 FROM pull_request_snapshots
                 WHERE host = ?1 AND project = ?2 AND kind = ?3 AND key = ?4",
                params![host, project, kind, key],
                |row| {
                    Ok(Snapshot {
                        host: row.get(0)?,
                        project: row.get(1)?,
                        kind: row.get(2)?,
                        key: row.get(3)?,
                        fingerprint: row.get(4)?,
                        payload: row.get(5)?,
                        observed_at_ms: row.get(6)?,
                        generation: row.get(7)?,
                    })
                },
            )
            .optional()
    }

    pub fn put(&self, snapshot: Snapshot) -> Result<()> {
        if snapshot.kind == "files" && snapshot.payload.len() > FILES_PAYLOAD_CAP {
            return Ok(());
        }
        if self
            .connection
            .query_row(
                "SELECT generation
                 FROM pull_request_snapshots
                 WHERE host = ?1 AND project = ?2 AND kind = ?3 AND key = ?4",
                params![snapshot.host, snapshot.project, snapshot.kind, snapshot.key],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .is_some_and(|existing| existing >= snapshot.generation)
        {
            return Ok(());
        }

        self.connection.execute(
            "INSERT INTO pull_request_snapshots (
               host, project, kind, key, fingerprint, payload, observed_at_ms, generation
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(host, project, kind, key) DO UPDATE SET
               fingerprint = excluded.fingerprint,
               payload = excluded.payload,
               observed_at_ms = excluded.observed_at_ms,
               generation = excluded.generation",
            params![
                snapshot.host,
                snapshot.project,
                snapshot.kind,
                snapshot.key,
                snapshot.fingerprint,
                snapshot.payload,
                snapshot.observed_at_ms,
                snapshot.generation,
            ],
        )?;

        while self.byte_total()? > BYTE_CAP {
            let deleted = self.connection.execute(
                "DELETE FROM pull_request_snapshots
                 WHERE rowid = (
                   SELECT rowid
                   FROM pull_request_snapshots
                   ORDER BY observed_at_ms ASC
                   LIMIT 1
                 )",
                [],
            )?;
            if deleted == 0 {
                break;
            }
        }

        Ok(())
    }

    pub fn delete_host(&self, host: &str) -> Result<()> {
        self.connection.execute(
            "DELETE FROM pull_request_snapshots WHERE host = ?1",
            params![host],
        )?;
        Ok(())
    }

    /// Every row of one kind, for example each stored `context`.
    pub fn list_kind(&self, kind: &str) -> Result<Vec<Snapshot>> {
        let mut statement = self.connection.prepare(
            "SELECT host, project, kind, key, fingerprint, payload, observed_at_ms, generation
             FROM pull_request_snapshots
             WHERE kind = ?1",
        )?;
        statement
            .query_map(params![kind], |row| {
                Ok(Snapshot {
                    host: row.get(0)?,
                    project: row.get(1)?,
                    kind: row.get(2)?,
                    key: row.get(3)?,
                    fingerprint: row.get(4)?,
                    payload: row.get(5)?,
                    observed_at_ms: row.get(6)?,
                    generation: row.get(7)?,
                })
            })?
            .collect()
    }

    pub fn delete_number(&self, host: &str, project: &str, number: u64) -> Result<()> {
        self.connection.execute(
            "DELETE FROM pull_request_snapshots
             WHERE host = ?1 AND project = ?2 AND key = ?3",
            params![host, project, number.to_string()],
        )?;
        Ok(())
    }

    pub fn byte_total(&self) -> Result<i64> {
        self.connection.query_row(
            "SELECT COALESCE(SUM(length(payload)), 0) FROM pull_request_snapshots",
            [],
            |row| row.get(0),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_request_snapshots_generation_is_wall_clock_based_and_strictly_increasing() {
        let before = now_ms();
        let first = next_generation();
        let second = next_generation();
        assert!(
            first >= before,
            "a stamp is never older than the clock it read"
        );
        assert!(
            second > first,
            "two stamps in the same millisecond still order"
        );
    }
}
