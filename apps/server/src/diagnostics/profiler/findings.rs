//! Findings: recurring problems the profiler has detected, deduplicated by fingerprint.
//!
//! `FindingsBook` is the in-memory index. It never touches the filesystem after `load`:
//! every change is queued through the store's non-blocking writer, which appends it to
//! `findings.ndjson`. `load` runs once at runtime start, before the writer exists, and
//! compacts that file.

use std::{
    collections::HashMap,
    fs::{self, File},
    io::{self, BufRead, BufReader, BufWriter, Write},
    path::Path,
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};

use super::{event::ProfilerEvent, store::EventSender};

// ponytail: mirrors the private `FINDINGS_FILE` in store.rs, which owns appending to it.
// The reload test in this module fails if the two names ever diverge.
const FINDINGS_FILE: &str = "findings.ndjson";
const FINDINGS_TMP_FILE: &str = "findings.ndjson.tmp";
/// Findings kept in memory and in the compacted file.
const MAX_FINDINGS: usize = 1000;
/// Resolved findings last seen longer ago than this are dropped when the file is compacted.
const RESOLVED_TTL_DAYS: i64 = 7;
const CRASH_KIND_PREFIX: &str = "crash.";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Warn,
    Error,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FindingState {
    Open,
    Resolved,
}

/// What a detector reports. The fingerprint fields must be stable across occurrences of the
/// same problem: no paths, timestamps or other per-occurrence values.
pub struct FindingInput {
    pub kind: &'static str,
    pub severity: Severity,
    pub source: String,
    pub fingerprint_fields: Vec<String>,
    pub title: String,
    pub summary: String,
    pub evidence: Value,
}

/// A finding as stored and as sent to clients. Timestamps are RFC3339 UTC with whole
/// seconds and a `Z` suffix, so they also sort chronologically as plain strings.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    pub id: String,
    pub fingerprint: String,
    pub kind: String,
    pub severity: Severity,
    pub first_seen: String,
    pub last_seen: String,
    pub count: u64,
    pub source: String,
    pub title: String,
    pub summary: String,
    pub state: FindingState,
    pub evidence: Value,
}

impl Finding {
    /// The `ProfilerFinding` shape of the status RPC: every field except `evidence`.
    pub fn to_wire(&self) -> Value {
        let mut wire = serde_json::to_value(self).unwrap_or(Value::Null);
        if let Value::Object(fields) = &mut wire {
            fields.remove("evidence");
        }
        wire
    }
}

/// First 12 hex digits of SHA-256 over the kind and the stable fields.
pub fn fingerprint(kind: &str, fields: &[String]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(kind.as_bytes());
    for field in fields {
        hasher.update([0u8]);
        hasher.update(field.as_bytes());
    }
    let digest = hasher.finalize();
    digest[..6].iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Default)]
pub struct FindingsBook {
    by_id: HashMap<String, Finding>,
}

impl FindingsBook {
    pub fn load(dir: &Path) -> Self {
        Self::load_at(dir, OffsetDateTime::now_utc())
    }

    /// Replays `findings.ndjson` (the last line per id wins; unreadable lines are skipped),
    /// drops resolved findings older than 7 days, keeps the 1,000 most recent, and rewrites
    /// the file through a temp file and a rename. A failed rewrite leaves the old file in
    /// place for the next start, and so does a read error. Call it before the store's writer starts, so nothing appends
    /// while the file is replaced.
    pub fn load_at(dir: &Path, now: OffsetDateTime) -> Self {
        let path = dir.join(FINDINGS_FILE);
        let Ok(file) = File::open(&path) else {
            return Self::default();
        };
        let mut by_id = HashMap::new();
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        let mut read_to_end = true;
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) => break,
                Ok(_) => {}
                Err(_) => {
                    read_to_end = false;
                    break;
                }
            }
            if let Ok(finding) = serde_json::from_slice::<Finding>(&line) {
                by_id.insert(finding.id.clone(), finding);
            }
        }
        let cutoff = now - Duration::days(RESOLVED_TTL_DAYS);
        by_id.retain(|_, finding| {
            let Ok(last_seen) = OffsetDateTime::parse(&finding.last_seen, &Rfc3339) else {
                return false;
            };
            finding.state == FindingState::Open || last_seen >= cutoff
        });
        let mut book = Self { by_id };
        book.truncate_to(MAX_FINDINGS);
        // A read error mid-file leaves a partial book that must not replace the whole file.
        if read_to_end && let Err(error) = book.rewrite(dir) {
            tracing::warn!(%error, "profiler findings file was not compacted");
        }
        book
    }

    /// Records one occurrence and queues the finding for the writer; never touches the
    /// filesystem. A repeat of a known fingerprint increments `count`, advances `last_seen`,
    /// reopens the finding, and refreshes title, summary and evidence from this occurrence.
    /// Its severity only ever rises.
    pub fn record(
        &mut self,
        input: FindingInput,
        now: OffsetDateTime,
        events: &EventSender,
    ) -> Finding {
        let fingerprint = fingerprint(input.kind, &input.fingerprint_fields);
        let id = format!("{}-{fingerprint}", input.kind);
        let seen = timestamp(now);
        let event = ProfilerEvent {
            kind: "finding",
            source: input.source.clone(),
            payload: json!({ "id": id, "fingerprint": fingerprint, "kindOf": input.kind }),
        };
        let finding = if let Some(known) = self.by_id.get_mut(&id) {
            known.count += 1;
            known.severity = known.severity.max(input.severity);
            if seen > known.last_seen {
                known.last_seen = seen;
            }
            known.state = FindingState::Open;
            known.title = input.title;
            known.summary = input.summary;
            known.evidence = input.evidence;
            known.clone()
        } else {
            // Bounded like the file: a new finding past the cap pushes out the stalest one.
            self.truncate_to(MAX_FINDINGS - 1);
            let finding = Finding {
                id: id.clone(),
                fingerprint,
                kind: input.kind.to_owned(),
                severity: input.severity,
                first_seen: seen.clone(),
                last_seen: seen,
                count: 1,
                source: input.source,
                title: input.title,
                summary: input.summary,
                state: FindingState::Open,
                evidence: input.evidence,
            };
            self.by_id.insert(id, finding.clone());
            finding
        };
        if let Ok(line) = serde_json::to_value(&finding) {
            events.send_finding(line);
        }
        events.send(event);
        finding
    }

    /// The `n` most recently seen findings, newest first.
    pub fn newest(&self, n: usize) -> Vec<Finding> {
        let mut findings: Vec<&Finding> = self.by_id.values().collect();
        findings.sort_by(|a, b| (&b.last_seen, &a.id).cmp(&(&a.last_seen, &b.id)));
        findings.into_iter().take(n).cloned().collect()
    }

    /// When a `crash.*` finding was last seen.
    pub fn last_crash_at(&self) -> Option<String> {
        self.by_id
            .values()
            .filter(|f| f.kind.starts_with(CRASH_KIND_PREFIX))
            .map(|f| &f.last_seen)
            .max()
            .cloned()
    }

    /// Keeps the `keep` most recently seen findings.
    // ponytail: sorts all findings, but only once the book is at the cap; with 1,000 entries
    // that is microseconds. Switch to a min-heap on `last_seen` if the cap ever grows a lot.
    fn truncate_to(&mut self, keep: usize) {
        let excess = self.by_id.len().saturating_sub(keep);
        if excess == 0 {
            return;
        }
        let mut order: Vec<(&String, &String)> = self
            .by_id
            .iter()
            .map(|(id, f)| (&f.last_seen, id))
            .collect();
        order.sort();
        let stale: Vec<String> = order
            .into_iter()
            .take(excess)
            .map(|(_, id)| id.clone())
            .collect();
        for id in stale {
            self.by_id.remove(&id);
        }
    }

    /// Writes the book oldest first, so later appends keep the file in chronological order.
    fn rewrite(&self, dir: &Path) -> io::Result<()> {
        let tmp = dir.join(FINDINGS_TMP_FILE);
        let written = self.write_tmp(&tmp);
        let renamed = written.and_then(|()| fs::rename(&tmp, dir.join(FINDINGS_FILE)));
        if renamed.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        renamed
    }

    fn write_tmp(&self, tmp: &Path) -> io::Result<()> {
        let mut oldest_first: Vec<&Finding> = self.by_id.values().collect();
        oldest_first.sort_by(|a, b| (&a.last_seen, &a.id).cmp(&(&b.last_seen, &b.id)));
        let mut out = BufWriter::new(File::create(tmp)?);
        for finding in oldest_first {
            serde_json::to_writer(&mut out, finding)?;
            out.write_all(b"\n")?;
        }
        let file = out.into_inner().map_err(io::IntoInnerError::into_error)?;
        file.sync_data()
    }
}

/// Whole seconds in UTC with a `Z` suffix, e.g. `2026-10-09T12:00:00Z`.
fn timestamp(at: OffsetDateTime) -> String {
    let at = at.to_offset(UtcOffset::UTC);
    at.replace_nanosecond(0)
        .unwrap_or(at)
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use time::macros::datetime;

    fn input(method: &str) -> FindingInput {
        FindingInput {
            kind: "rpc.slow",
            severity: Severity::Warn,
            source: "server".into(),
            fingerprint_fields: vec![method.into(), "success".into(), "server".into()],
            title: format!("{method} took 12 s"),
            summary: "server 12 s".into(),
            evidence: json!({ "eventRefs": ["a1b2c3d4:1", "a1b2c3d4:2"] }),
        }
    }

    #[test]
    fn fingerprint_is_stable_and_ignores_paths_and_times() {
        let a = fingerprint("rpc.slow", &["m".into(), "success".into()]);
        assert_eq!(a, fingerprint("rpc.slow", &["m".into(), "success".into()]));
        assert_eq!(a.len(), 12);
        assert_ne!(a, fingerprint("rpc.slow", &["n".into(), "success".into()]));
    }

    #[test]
    fn load_compacts_duplicates_and_old_resolved_findings() {
        let dir = tempfile::tempdir().unwrap();
        let lines = [
            r#"{"id":"rpc.slow-aaaaaaaaaaaa","fingerprint":"aaaaaaaaaaaa","kind":"rpc.slow","severity":"warn","firstSeen":"2026-09-01T00:00:00Z","lastSeen":"2026-09-01T00:00:00Z","count":1,"source":"server","title":"t","summary":"s","state":"resolved","evidence":{}}"#,
            r#"{"id":"rpc.slow-bbbbbbbbbbbb","fingerprint":"bbbbbbbbbbbb","kind":"rpc.slow","severity":"warn","firstSeen":"2026-10-09T00:00:00Z","lastSeen":"2026-10-09T00:00:00Z","count":1,"source":"server","title":"t","summary":"s","state":"open","evidence":{}}"#,
            r#"{"id":"rpc.slow-bbbbbbbbbbbb","fingerprint":"bbbbbbbbbbbb","kind":"rpc.slow","severity":"warn","firstSeen":"2026-10-09T00:00:00Z","lastSeen":"2026-10-09T01:00:00Z","count":2,"source":"server","title":"t","summary":"s","state":"open","evidence":{}}"#,
        ];
        std::fs::write(dir.path().join("findings.ndjson"), lines.join("\n") + "\n").unwrap();
        let book = FindingsBook::load_at(dir.path(), datetime!(2026-10-09 12:00 UTC));
        assert_eq!(book.newest(20).len(), 1);
        assert_eq!(book.newest(20)[0].count, 2);
        let rewritten = std::fs::read_to_string(dir.path().join("findings.ndjson")).unwrap();
        assert_eq!(rewritten.lines().count(), 1);
    }

    #[test]
    fn repeats_increment_count_and_survive_reload() {
        let dir = tempfile::tempdir().unwrap();
        let store = super::super::store::ProfilerStore::start(
            super::super::store::StoreConfig::new(dir.path().to_path_buf()),
        );
        let mut book = FindingsBook::load(dir.path());
        book.record(input("a"), datetime!(2026-10-09 12:00 UTC), &store.sender());
        let second = book.record(input("a"), datetime!(2026-10-09 12:05 UTC), &store.sender());
        book.record(input("b"), datetime!(2026-10-09 12:06 UTC), &store.sender());
        store.shutdown(); // flushes the queued finding lines
        assert_eq!(second.count, 2);
        assert_eq!(second.first_seen, "2026-10-09T12:00:00Z");
        let reloaded = FindingsBook::load(dir.path());
        let newest = reloaded.newest(20);
        assert_eq!(newest.len(), 2);
        assert_eq!(
            newest[0].fingerprint,
            fingerprint("rpc.slow", &input("b").fingerprint_fields)
        );
    }

    fn stored_line(n: usize, kind: &str, last_seen: &str, state: &str) -> String {
        format!(
            r#"{{"id":"{kind}-{n:012}","fingerprint":"{n:012}","kind":"{kind}","severity":"warn","firstSeen":"{last_seen}","lastSeen":"{last_seen}","count":1,"source":"server","title":"t{n}","summary":"s","state":"{state}","evidence":{{}}}}"#
        )
    }

    fn scratch_sender() -> (tempfile::TempDir, super::super::store::ProfilerStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = super::super::store::ProfilerStore::start(
            super::super::store::StoreConfig::new(dir.path().to_path_buf()),
        );
        (dir, store)
    }

    #[test]
    fn wire_form_is_the_contract_shape_without_evidence() {
        let (_dir, store) = scratch_sender();
        let finding = FindingsBook::default().record(
            input("a"),
            datetime!(2026-10-09 12:00 UTC),
            &store.sender(),
        );
        let mut keys: Vec<String> = finding
            .to_wire()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "count",
                "fingerprint",
                "firstSeen",
                "id",
                "kind",
                "lastSeen",
                "severity",
                "source",
                "state",
                "summary",
                "title"
            ]
        );
        let wire = finding.to_wire();
        assert_eq!(wire["state"], "open");
        assert_eq!(wire["severity"], "warn");
        assert_eq!(wire["id"], format!("rpc.slow-{}", finding.fingerprint));
        assert!(
            serde_json::to_value(&finding)
                .unwrap()
                .get("evidence")
                .is_some()
        );
    }

    #[test]
    fn timestamps_are_whole_second_utc() {
        let (_dir, store) = scratch_sender();
        let finding = FindingsBook::default().record(
            input("a"),
            datetime!(2026-10-09 14:00:00.987 +2),
            &store.sender(),
        );
        assert_eq!(finding.first_seen, "2026-10-09T12:00:00Z");
        assert_eq!(finding.last_seen, "2026-10-09T12:00:00Z");
    }

    #[test]
    fn a_repeat_reopens_refreshes_and_never_lowers_severity_or_last_seen() {
        let dir = tempfile::tempdir().unwrap();
        let fp = fingerprint("rpc.slow", &input("a").fingerprint_fields);
        let stored = stored_line(1, "rpc.slow", "2026-10-09T11:00:00Z", "resolved")
            .replace("000000000001", &fp)
            .replace(r#""severity":"warn""#, r#""severity":"error""#);
        std::fs::write(dir.path().join("findings.ndjson"), stored + "\n").unwrap();
        let mut book = FindingsBook::load_at(dir.path(), datetime!(2026-10-09 12:00 UTC));
        let (_scratch, store) = scratch_sender();
        let again = book.record(input("a"), datetime!(2026-10-09 10:00 UTC), &store.sender());
        assert_eq!(again.id, format!("rpc.slow-{fp}"));
        assert_eq!(again.count, 2);
        assert_eq!(again.state, FindingState::Open);
        assert_eq!(again.severity, Severity::Error, "severity only rises");
        assert_eq!(
            again.last_seen, "2026-10-09T11:00:00Z",
            "a clock step back is ignored"
        );
        assert_eq!(
            again.title, "a took 12 s",
            "title comes from the latest occurrence"
        );
        assert_eq!(again.evidence, input("a").evidence);
        assert_eq!(book.newest(20).len(), 1);
    }

    #[test]
    fn last_crash_at_is_the_latest_crash_finding() {
        let dir = tempfile::tempdir().unwrap();
        let lines = [
            stored_line(1, "crash.panic", "2026-10-09T08:00:00Z", "open"),
            stored_line(2, "crash.native", "2026-10-09T09:00:00Z", "open"),
            stored_line(3, "rpc.slow", "2026-10-09T11:00:00Z", "open"),
        ];
        std::fs::write(dir.path().join("findings.ndjson"), lines.join("\n") + "\n").unwrap();
        let book = FindingsBook::load_at(dir.path(), datetime!(2026-10-09 12:00 UTC));
        assert_eq!(
            book.last_crash_at().as_deref(),
            Some("2026-10-09T09:00:00Z")
        );
        assert_eq!(FindingsBook::default().last_crash_at(), None);
    }

    #[test]
    fn load_keeps_the_1000_most_recent_and_skips_unreadable_lines() {
        let dir = tempfile::tempdir().unwrap();
        let mut lines: Vec<String> = (0..1005)
            .map(|n| {
                let stamp = format!(
                    "2026-10-09T{:02}:{:02}:{:02}Z",
                    n / 3600,
                    n % 3600 / 60,
                    n % 60
                );
                stored_line(n, "rpc.slow", &stamp, "open")
            })
            .collect();
        lines.insert(3, "not json".into());
        lines.push(r#"{"id":"torn-"#.into());
        std::fs::write(dir.path().join("findings.ndjson"), lines.join("\n")).unwrap();
        let book = FindingsBook::load_at(dir.path(), datetime!(2026-10-10 00:00 UTC));
        let newest = book.newest(5000);
        assert_eq!(newest.len(), 1000);
        assert_eq!(newest[0].title, "t1004");
        assert_eq!(newest[999].title, "t5");
        let rewritten = std::fs::read_to_string(dir.path().join("findings.ndjson")).unwrap();
        assert_eq!(rewritten.lines().count(), 1000);
        assert!(rewritten.ends_with('\n'));
        assert!(!dir.path().join("findings.ndjson.tmp").exists());
    }

    #[test]
    fn a_new_finding_past_the_cap_pushes_out_the_stalest() {
        let (_dir, store) = scratch_sender();
        let mut book = FindingsBook::default();
        let start = datetime!(2026-10-09 00:00 UTC);
        for n in 0..=MAX_FINDINGS {
            let mut next = input("m");
            next.fingerprint_fields = vec![n.to_string()];
            book.record(next, start + Duration::seconds(n as i64), &store.sender());
        }
        let newest = book.newest(5000);
        assert_eq!(newest.len(), MAX_FINDINGS);
        let stalest = fingerprint("rpc.slow", &["0".to_owned()]);
        assert!(newest.iter().all(|f| f.fingerprint != stalest));
    }
}
