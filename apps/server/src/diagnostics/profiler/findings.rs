//! Findings: recurring problems the profiler has detected, deduplicated by fingerprint.
//!
//! `FindingsBook` is the in-memory index and the source of truth while the process runs.
//! It never touches the filesystem after `load`: changes worth keeping are queued through
//! the store's non-blocking writer, which appends them to `findings.ndjson`. `load` runs
//! once at runtime start, before the writer exists, and compacts that file; afterwards the
//! runtime compacts it through the writer with `compacted_lines`.

use std::{
    collections::HashMap,
    fs::File,
    io::{self, BufRead, BufReader},
    path::Path,
};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use time::{Duration, OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};

use super::{
    event::ProfilerEvent,
    store::{EventSender, FINDINGS_FILE, replace_findings_file},
};

/// Findings kept in memory and in the compacted file.
const MAX_FINDINGS: usize = 1000;
/// Resolved findings last seen longer ago than this are dropped when the file is compacted.
const RESOLVED_TTL_DAYS: i64 = 7;
const CRASH_KIND_PREFIX: &str = "crash.";
/// A repeat is persisted at least this often, however many repeats arrive in between.
const PERSIST_INTERVAL: Duration = Duration::seconds(60);
/// The crash dumps a finding's `evidence.crashIds` keeps, newest first. A repeat's evidence
/// names only its own dump, so the earlier ones are carried over, up to this many.
const MAX_CRASH_IDS: usize = 64;

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

/// One recorded occurrence of a finding, for evidence a later lookup adds to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Occurrence {
    pub id: String,
    /// The finding's `count` once this occurrence was recorded.
    pub count: u64,
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
    /// When each finding's line was last queued for the writer (`now` as given to `record`;
    /// for a loaded or compacted finding, its `lastSeen`, as that is what the file holds).
    persisted_at: HashMap<String, OffsetDateTime>,
    /// `load` could not read the whole file. The book then never replaces it, because the
    /// file still holds findings the book never saw; the next start retries.
    partial: bool,
}

impl FindingsBook {
    pub fn load(dir: &Path) -> Self {
        Self::load_at(dir, OffsetDateTime::now_utc())
    }

    /// Replays `findings.ndjson` (the last line per id wins; unreadable lines are skipped),
    /// drops resolved findings older than 7 days, keeps the 1,000 most recent, and rewrites
    /// the file through a temp file and a rename. A failed rewrite leaves the old file in
    /// place for the next start, and so does a read error. Call it before the store's writer
    /// starts, so nothing appends while the file is replaced.
    pub fn load_at(dir: &Path, now: OffsetDateTime) -> Self {
        let mut book = Self::read(dir);
        if let Some(lines) = book.compacted_lines(now)
            && let Err(error) = replace_findings_file(dir, &lines)
        {
            tracing::warn!(%error, "profiler findings file was not compacted");
        }
        book
    }

    /// What `load` holds in memory, for a reader that must not write: the file is replayed and
    /// compacted in the book only, never rewritten, and a missing file is an empty book.
    pub fn read_only(dir: &Path) -> Self {
        let mut book = Self::read(dir);
        let _ = book.compacted_lines(OffsetDateTime::now_utc());
        book
    }

    /// Replays `findings.ndjson` without changing it: the last line per id wins, and
    /// unreadable lines are skipped.
    fn read(dir: &Path) -> Self {
        let path = dir.join(FINDINGS_FILE);
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Self::default(),
            Err(error) => {
                tracing::warn!(%error, "profiler findings file could not be read");
                return Self {
                    partial: true,
                    ..Self::default()
                };
            }
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
        // Everything loaded is what the file holds, so it counts as persisted at `lastSeen`.
        let persisted_at = by_id
            .iter()
            .filter_map(|(id, finding)| Some((id.clone(), parse_last_seen(finding)?)))
            .collect();
        Self {
            by_id,
            persisted_at,
            partial: !read_to_end,
        }
    }

    /// Compacts the book as `load` does (resolved findings older than 7 days dropped, the
    /// 1,000 most recent kept) and returns its lines for `findings.ndjson`, oldest first so
    /// later appends keep the file in chronological order. The lines carry the current state,
    /// so each kept finding then counts as persisted at its `lastSeen`. `None` after a
    /// partial `load`: the book still compacts in memory but must not replace the file.
    pub fn compacted_lines(&mut self, now: OffsetDateTime) -> Option<Vec<Value>> {
        let cutoff = now - Duration::days(RESOLVED_TTL_DAYS);
        self.by_id.retain(|_, finding| {
            parse_last_seen(finding)
                .is_some_and(|last_seen| finding.state == FindingState::Open || last_seen >= cutoff)
        });
        self.truncate_to(MAX_FINDINGS);
        let by_id = &self.by_id;
        self.persisted_at.retain(|id, _| by_id.contains_key(id));
        if self.partial {
            return None;
        }
        let mut oldest_first: Vec<&Finding> = self.by_id.values().collect();
        oldest_first.sort_by(|a, b| (&a.last_seen, &a.id).cmp(&(&b.last_seen, &b.id)));
        for finding in &oldest_first {
            if let Some(last_seen) = parse_last_seen(finding) {
                self.persisted_at.insert(finding.id.clone(), last_seen);
            }
        }
        Some(
            oldest_first
                .into_iter()
                .filter_map(|finding| serde_json::to_value(finding).ok())
                .collect(),
        )
    }

    /// Whether `load` could not read the whole file, so findings on disk are missing here.
    pub fn is_partial(&self) -> bool {
        self.partial
    }

    /// Records one occurrence; never touches the filesystem. A repeat of a known fingerprint
    /// increments `count`, advances `last_seen`, reopens the finding, and refreshes title,
    /// summary and evidence from this occurrence. Its severity only ever rises.
    ///
    /// The in-memory state is authoritative. Persisting every repeat would grow
    /// `findings.ndjson` without bound while the process runs, so the finding line (and its
    /// cross-reference event) is queued only when the finding is new, its severity rose, it
    /// was reopened, its new `count` is a power of two, or `PERSIST_INTERVAL` has passed
    /// since it was last queued. Trade-off: a crash can lose up to a minute of `count` and
    /// `lastSeen` updates, and a restart reloads the last persisted values.
    pub fn record(
        &mut self,
        input: FindingInput,
        now: OffsetDateTime,
        events: &EventSender,
    ) -> Finding {
        self.record_with(input, now, events, false)
    }

    /// [`record`](Self::record), persisting this occurrence whatever the repeat throttle says.
    /// For rare occurrences whose evidence must survive a crash, such as a crash dump's id.
    pub fn record_now(
        &mut self,
        input: FindingInput,
        now: OffsetDateTime,
        events: &EventSender,
    ) -> Finding {
        self.record_with(input, now, events, true)
    }

    fn record_with(
        &mut self,
        input: FindingInput,
        now: OffsetDateTime,
        events: &EventSender,
        force: bool,
    ) -> Finding {
        let fingerprint = fingerprint(input.kind, &input.fingerprint_fields);
        let id = format!("{}-{fingerprint}", input.kind);
        let seen = timestamp(now);
        let kind = input.kind;
        let (finding, persist) = if let Some(known) = self.by_id.get_mut(&id) {
            let reopened = known.state == FindingState::Resolved;
            let severity_rose = input.severity > known.severity;
            known.count += 1;
            known.severity = known.severity.max(input.severity);
            if seen > known.last_seen {
                known.last_seen = seen;
            }
            known.state = FindingState::Open;
            known.title = input.title;
            known.summary = input.summary;
            let mut evidence = input.evidence;
            carry_crash_ids(&mut evidence, &known.evidence);
            known.evidence = evidence;
            let interval_passed = self.persisted_at.get(&id).is_none_or(|at| {
                let elapsed = now - *at;
                // A negative gap is a clock step back; persist rather than wait it out.
                elapsed >= PERSIST_INTERVAL || elapsed.is_negative()
            });
            let persist = force
                || reopened
                || severity_rose
                || known.count.is_power_of_two()
                || interval_passed;
            (known.clone(), persist)
        } else {
            // Bounded like the file: a new finding past the cap pushes out the stalest one.
            self.truncate_to(MAX_FINDINGS - 1);
            let finding = Finding {
                id: id.clone(),
                fingerprint,
                kind: kind.to_owned(),
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
            (finding, true)
        };
        if persist {
            self.persisted_at.insert(finding.id.clone(), now);
            if let Ok(line) = serde_json::to_value(&finding) {
                events.send_finding(line);
            }
            events.send(ProfilerEvent {
                kind: "finding",
                source: finding.source.clone(),
                payload: json!({ "id": finding.id, "fingerprint": finding.fingerprint, "kindOf": kind }),
            });
        }
        finding
    }

    /// Adds `extra` to the evidence of `occurrence`, for what a lookup finds after it was
    /// recorded. It is not an occurrence: `count` and `lastSeen` stay. A later occurrence has
    /// replaced the evidence, so it is left alone; so is a finding no longer in the book.
    /// Returns whether the evidence was amended.
    pub fn amend(
        &mut self,
        occurrence: &Occurrence,
        extra: Map<String, Value>,
        now: OffsetDateTime,
        events: &EventSender,
    ) -> bool {
        let Some(known) = self
            .by_id
            .get_mut(&occurrence.id)
            .filter(|known| known.count == occurrence.count)
        else {
            return false;
        };
        if let Value::Object(evidence) = &mut known.evidence {
            evidence.extend(extra);
        }
        if let Ok(line) = serde_json::to_value(&*known) {
            events.send_finding(line);
        }
        self.persisted_at.insert(occurrence.id.clone(), now);
        true
    }

    /// The `n` most recently seen findings, newest first.
    pub fn newest(&self, n: usize) -> Vec<Finding> {
        let mut findings: Vec<&Finding> = self.by_id.values().collect();
        findings.sort_by(|a, b| (&b.last_seen, &a.id).cmp(&(&a.last_seen, &b.id)));
        findings.into_iter().take(n).cloned().collect()
    }

    /// The crash dumps findings cite: every id in `evidence.crashIds` (each finding's newest
    /// 64), and `evidence.crashId`, which findings written before `crashIds` carry alone.
    pub fn crash_ids(&self) -> impl Iterator<Item = String> + '_ {
        self.by_id.values().flat_map(|finding| {
            let evidence = &finding.evidence;
            evidence
                .get("crashIds")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .chain(evidence.get("crashId"))
                .filter_map(|id| id.as_str().map(str::to_owned))
        })
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
            self.persisted_at.remove(&id);
        }
    }
}

/// Appends the dumps `previous` counted to the `crashIds` of a repeat's `evidence`, newest
/// first and each once, keeping `MAX_CRASH_IDS`. A repeat without a dump of its own (the same
/// crash seen only in an OS record) keeps them too, so a later start never raises them again.
fn carry_crash_ids(evidence: &mut Value, previous: &Value) {
    let earlier: Vec<&Value> = previous
        .get("crashIds")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .chain(previous.get("crashId"))
        .filter(|id| id.is_string())
        .collect();
    if earlier.is_empty() {
        return;
    }
    let Value::Object(fields) = evidence else {
        return;
    };
    let Value::Array(ids) = fields
        .entry("crashIds")
        .or_insert_with(|| Value::Array(Vec::new()))
    else {
        return;
    };
    for id in earlier {
        if ids.len() >= MAX_CRASH_IDS {
            break;
        }
        if !ids.contains(id) {
            ids.push(id.clone());
        }
    }
    ids.truncate(MAX_CRASH_IDS);
}

fn parse_last_seen(finding: &Finding) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(&finding.last_seen, &Rfc3339).ok()
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
    fn compacted_lines_keep_exactly_what_a_reload_keeps() {
        let dir = tempfile::tempdir().unwrap();
        let lines = [
            stored_line(1, "rpc.slow", "2026-10-07T00:00:00Z", "resolved"),
            stored_line(2, "rpc.slow", "2026-10-08T00:00:00Z", "open"),
            stored_line(3, "rpc.slow", "2026-10-01T00:00:00Z", "open"),
        ];
        std::fs::write(dir.path().join("findings.ndjson"), lines.join("\n") + "\n").unwrap();
        let mut book = FindingsBook::load_at(dir.path(), datetime!(2026-10-09 00:00 UTC));
        assert_eq!(book.newest(20).len(), 3);

        let later = datetime!(2026-10-16 00:00 UTC);
        let ids: Vec<String> = book
            .compacted_lines(later)
            .unwrap()
            .iter()
            .map(|line| line["id"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            ids,
            ["rpc.slow-000000000003", "rpc.slow-000000000002"],
            "oldest first, the expired resolved finding dropped"
        );
        assert_eq!(
            book.newest(20).len(),
            2,
            "memory matches the compacted file"
        );
        let mut reloaded: Vec<String> = FindingsBook::load_at(dir.path(), later)
            .newest(20)
            .into_iter()
            .map(|finding| finding.id)
            .collect();
        reloaded.reverse();
        assert_eq!(reloaded, ids);
    }

    #[test]
    fn a_partial_load_never_replaces_the_file() {
        let dir = tempfile::tempdir().unwrap();
        // Opening a directory succeeds on Unix and reading it fails, as a mid-file read error.
        let unreadable = dir.path().join("findings.ndjson");
        std::fs::create_dir(&unreadable).unwrap();
        let mut book = FindingsBook::load_at(dir.path(), datetime!(2026-10-09 00:00 UTC));
        assert!(
            book.compacted_lines(datetime!(2026-10-09 01:00 UTC))
                .is_none()
        );
        assert!(unreadable.is_dir(), "the unread file is left in place");
    }

    #[test]
    fn compaction_persists_held_back_state_and_restarts_the_throttle_from_it() {
        let start = datetime!(2026-10-09 12:00 UTC);
        let (dir, store) = scratch_sender();
        let sender = store.sender();
        let mut book = FindingsBook::default();
        // Counts 1, 2 and 4 are queued (powers of two); 3 and 5 are held back.
        for n in 0..5 {
            book.record(input("a"), start + Duration::seconds(n), &sender);
        }
        // As the runtime's hourly maintenance does.
        let lines = book.compacted_lines(start + Duration::seconds(10)).unwrap();
        assert_eq!(counts(&lines), [5], "compaction writes the held-back state");
        sender.replace_findings(lines);
        assert_eq!(book.persisted_at.len(), book.by_id.len());
        // The throttle restarts from the compacted lastSeen (4 s), not the last queued line
        // (3 s): count 6 at 63 s is held, count 7 at 64 s is queued.
        book.record(input("a"), start + Duration::seconds(63), &sender);
        book.record(input("a"), start + Duration::seconds(64), &sender);
        store.shutdown();
        assert_eq!(counts(&persisted_lines(dir.path())), [5, 7]);
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

    /// Records `input` at each time against a real store, shuts it down, and returns the
    /// directory plus the persisted finding lines.
    fn persisted_after(
        input: impl Fn() -> FindingInput,
        times: impl IntoIterator<Item = OffsetDateTime>,
    ) -> (tempfile::TempDir, Vec<Value>) {
        let (dir, store) = scratch_sender();
        let mut book = FindingsBook::default();
        for at in times {
            book.record(input(), at, &store.sender());
        }
        store.shutdown();
        let lines = persisted_lines(dir.path());
        (dir, lines)
    }

    fn persisted_lines(dir: &std::path::Path) -> Vec<Value> {
        std::fs::read_to_string(dir.join("findings.ndjson"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn counts(lines: &[Value]) -> Vec<u64> {
        lines.iter().map(|l| l["count"].as_u64().unwrap()).collect()
    }

    #[test]
    fn amended_evidence_is_kept_and_persisted_without_counting_an_occurrence() {
        let (dir, store) = scratch_sender();
        let mut book = FindingsBook::default();
        let recorded = book.record(input("a"), datetime!(2026-10-09 12:00 UTC), &store.sender());
        let first = Occurrence {
            id: recorded.id.clone(),
            count: 1,
        };
        let extra = Map::from_iter([("frames".to_owned(), json!(["f (libc.so.6 + 0x10)"]))]);
        let at = datetime!(2026-10-09 12:00:04 UTC);

        assert!(book.amend(&first, extra.clone(), at, &store.sender()));
        let unknown = Occurrence {
            id: "rpc.slow-000000000000".to_owned(),
            count: 1,
        };
        assert!(!book.amend(&unknown, extra.clone(), at, &store.sender()));
        store.shutdown();

        let amended = &book.newest(1)[0];
        assert_eq!(
            (amended.count, amended.last_seen.as_str()),
            (1, "2026-10-09T12:00:00Z")
        );
        assert_eq!(amended.evidence["frames"][0], "f (libc.so.6 + 0x10)");
        assert_eq!(
            amended.evidence["eventRefs"], recorded.evidence["eventRefs"],
            "the recorded evidence stays"
        );
        let reloaded = FindingsBook::load(dir.path()).newest(1);
        assert_eq!(reloaded[0].evidence, amended.evidence);

        // A later occurrence replaced the evidence: the first one's lookup must not mix in.
        let (_scratch, store) = scratch_sender();
        let mut book = FindingsBook::default();
        book.record(input("a"), datetime!(2026-10-09 12:00 UTC), &store.sender());
        book.record(
            input("a"),
            datetime!(2026-10-09 12:00:01 UTC),
            &store.sender(),
        );
        assert!(!book.amend(&first, extra, at, &store.sender()));
        assert_eq!(book.newest(1)[0].evidence, input("a").evidence);
    }

    #[test]
    fn a_hot_repeat_persists_only_at_powers_of_two() {
        let start = datetime!(2026-10-09 12:00 UTC);
        let (dir, lines) = persisted_after(
            || input("a"),
            (0..1000).map(|n| start + Duration::milliseconds(n * 10)),
        );
        assert_eq!(counts(&lines), [1, 2, 4, 8, 16, 32, 64, 128, 256, 512]);
        // The cross-reference events follow the same throttle.
        let events: usize = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("events-")
            })
            .map(|path| {
                std::fs::read_to_string(path)
                    .unwrap()
                    .lines()
                    .filter(|line| line.contains(r#""kind":"finding""#))
                    .count()
            })
            .sum();
        assert_eq!(events, 10);
        // Reload sees the last persisted count, not the in-memory 1,000.
        let reloaded = FindingsBook::load(dir.path()).newest(20);
        assert_eq!((reloaded.len(), reloaded[0].count), (1, 512));
    }

    #[test]
    fn a_repeat_a_minute_after_the_last_persisted_line_persists() {
        let start = datetime!(2026-10-09 12:00 UTC);
        let (dir, lines) = persisted_after(
            || input("a"),
            (0..1000)
                .map(|n| start + Duration::milliseconds(n * 10))
                .chain([start + Duration::seconds(10 + 61)]),
        );
        assert_eq!(counts(&lines).last(), Some(&1001));
        assert_eq!(lines.len(), 11);
        assert_eq!(lines[10]["lastSeen"], "2026-10-09T12:01:11Z");
        assert_eq!(FindingsBook::load(dir.path()).newest(1)[0].count, 1001);
    }

    #[test]
    fn the_time_rule_is_a_minute_since_the_last_persisted_line() {
        let start = datetime!(2026-10-09 12:00 UTC);
        let third = |after: i64| {
            persisted_after(
                || input("a"),
                [start, start, start + Duration::seconds(after)],
            )
            .1
        };
        assert_eq!(counts(&third(59)), [1, 2], "count 3 at 59 s is held back");
        assert_eq!(counts(&third(60)), [1, 2, 3], "count 3 at 60 s persists");
    }

    /// A dump's finding is how the next start knows the run survived that dump, so every
    /// occurrence reaches the file with its `crashId`, whatever the repeat throttle says.
    #[test]
    fn an_occurrence_recorded_now_persists_even_between_powers_of_two() {
        let start = datetime!(2026-10-09 12:00 UTC);
        let (dir, store) = scratch_sender();
        let mut book = FindingsBook::default();
        for n in 0..3 {
            book.record_now(input("a"), start + Duration::seconds(n), &store.sender());
        }
        store.shutdown();
        assert_eq!(counts(&persisted_lines(dir.path())), [1, 2, 3]);
    }

    fn dump_input(crash_id: &str) -> FindingInput {
        FindingInput {
            kind: "crash.native",
            severity: Severity::Error,
            source: "server".into(),
            fingerprint_fields: vec!["server".into(), "minidump".into(), "SIGSEGV".into()],
            title: "Native crash dumped: SIGSEGV".into(),
            summary: "s".into(),
            evidence: json!({ "crashId": crash_id, "crashIds": [crash_id] }),
        }
    }

    /// Each repeat names only its own dump; the finding keeps every dump it counted (newest
    /// 64), so a later start never raises one of them again.
    #[test]
    fn a_dump_finding_keeps_the_dumps_it_counted() {
        let start = datetime!(2026-10-09 12:00 UTC);
        let (_dir, store) = scratch_sender();
        let mut book = FindingsBook::default();
        // A finding written before `crashIds` existed names one dump.
        let mut legacy = dump_input("legacy");
        legacy.evidence = json!({ "crashId": "legacy" });
        book.record_now(legacy, start, &store.sender());
        for n in 0..70 {
            book.record_now(dump_input(&format!("d{n}")), start, &store.sender());
        }
        let finding = &book.newest(1)[0];
        assert_eq!(finding.count, 71);
        let ids = finding.evidence["crashIds"].as_array().unwrap();
        assert_eq!(ids.len(), 64);
        assert_eq!(ids[0], "d69", "newest first");
        assert_eq!(ids[63], "d6");
        assert_eq!(finding.evidence["crashId"], "d69");
        let linked: std::collections::HashSet<String> = book.crash_ids().collect();
        assert!(linked.contains("d69") && linked.contains("d6"));
        assert!(!linked.contains("d5"), "beyond the newest 64");

        // A repeat of the same dump is not listed twice, and the legacy id is carried over.
        let mut book = FindingsBook::default();
        let mut legacy = dump_input("legacy");
        legacy.evidence = json!({ "crashId": "legacy" });
        book.record_now(legacy, start, &store.sender());
        book.record_now(dump_input("a"), start, &store.sender());
        book.record_now(dump_input("a"), start, &store.sender());
        assert_eq!(
            book.newest(1)[0].evidence["crashIds"],
            json!(["a", "legacy"])
        );
        // The same crash again, seen only in an OS record: the dumps stay linked.
        let mut record_only = dump_input("unused");
        record_only.evidence = json!({ "source": "coredump" });
        book.record_now(record_only, start, &store.sender());
        assert_eq!(
            book.newest(1)[0].evidence["crashIds"],
            json!(["a", "legacy"])
        );
        assert!(book.crash_ids().any(|id| id == "a"));
    }

    #[test]
    fn a_rising_severity_persists_even_between_powers_of_two() {
        let start = datetime!(2026-10-09 12:00 UTC);
        let (dir, store) = scratch_sender();
        let mut book = FindingsBook::default();
        for severity in [Severity::Warn, Severity::Warn, Severity::Error] {
            let mut next = input("a");
            next.severity = severity;
            book.record(next, start, &store.sender());
        }
        store.shutdown();
        assert_eq!(counts(&persisted_lines(dir.path())), [1, 2, 3]);
    }

    #[test]
    fn reopening_persists_and_a_loaded_finding_counts_as_persisted_at_last_seen() {
        let dir = tempfile::tempdir().unwrap();
        let fp = fingerprint("rpc.slow", &input("a").fingerprint_fields);
        let stored = |state: &str| {
            stored_line(1, "rpc.slow", "2026-10-09T11:00:00Z", state)
                .replace("000000000001", &fp)
                .replace(r#""count":1"#, r#""count":4"#)
        };
        let record_once = |state: &str, at: OffsetDateTime| {
            std::fs::write(dir.path().join("findings.ndjson"), stored(state) + "\n").unwrap();
            let mut book = FindingsBook::load_at(dir.path(), at);
            let store = super::super::store::ProfilerStore::start(
                super::super::store::StoreConfig::new(dir.path().to_path_buf()),
            );
            book.record(input("a"), at, &store.sender());
            store.shutdown();
            persisted_lines(dir.path())
        };
        let soon = datetime!(2026-10-09 11:00:30 UTC);
        assert_eq!(
            counts(&record_once("open", soon)),
            [4],
            "count 5 within a minute is held"
        );
        assert_eq!(
            counts(&record_once("resolved", soon)),
            [4, 5],
            "reopening persists"
        );
        let later = datetime!(2026-10-09 11:01:00 UTC);
        assert_eq!(
            counts(&record_once("open", later)),
            [4, 5],
            "a minute after lastSeen persists"
        );
    }
}
