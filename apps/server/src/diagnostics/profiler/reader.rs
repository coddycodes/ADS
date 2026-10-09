//! `Reader`: the read-only query layer over the profiler directory.
//!
//! It works while the server is stopped, which is when crash data matters, and beside a
//! running server. It never takes the directory lock, never creates or writes a file, and
//! tolerates the writer rotating, cutting back or deleting files underneath it. Memory stays
//! bounded: events stream line by line and only the newest `limit` matches are kept.
//!
//! This module also owns the `ProfilerStatus` shape ([`status_json`]), so the runtime and the
//! reader cannot drift apart.

use std::{
    collections::{HashMap, VecDeque},
    fs::{self, File},
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::{
    findings::{Finding, FindingsBook},
    store::{self, CRASHES_DIR, FINDINGS_FILE, event_file_index},
};

/// The most records one query returns, whatever the caller asks for.
pub const MAX_LIMIT: u16 = 10_000;
/// The longest a follow runs; a longer request is cut to this.
pub const MAX_FOLLOW: Duration = Duration::from_secs(600);
const FOLLOW_POLL: Duration = Duration::from_millis(500);
const STATUS_FINDINGS: usize = 20;
const CAPABILITIES_FILE: &str = "capabilities.json";
const SETTINGS_FILE: &str = "settings.json";
const DROPPED_KIND: &str = "telemetry.dropped";
/// A crash sidecar is a small summary; anything bigger is not read.
const MAX_SIDECAR_BYTES: u64 = 1024 * 1024;
/// How far back from the end of a file a follow looks for its last complete line. A record
/// longer than this that is half written when a follow starts is missed.
const TAIL_WINDOW: u64 = 1024 * 1024;
/// How many of the last consumed bytes a follow remembers to check the file still holds them.
const ECHO_BYTES: usize = 256;
/// Slack for clock steps when a file's modification time is used to skip it.
const MTIME_SKEW: Duration = Duration::from_secs(60);

/// What the `ProfilerStatus` contract reports besides the files themselves. The runtime fills
/// it from its live counters, the reader from the files.
#[derive(Default)]
pub(super) struct StatusLive {
    pub enabled: bool,
    pub recording_since: Option<String>,
    pub event_count: u64,
    pub dropped_events: u64,
    /// Failing writes, a degraded runtime, or findings on disk that could not be loaded.
    pub storage_failing: bool,
    pub last_crash_at: Option<String>,
    /// Newest first.
    pub findings: Vec<Finding>,
}

impl StatusLive {
    /// Takes the book's part. It does no I/O, so a caller may hold the book's lock meanwhile.
    pub(super) fn with_findings(mut self, book: &FindingsBook) -> Self {
        self.storage_failing |= book.is_partial();
        self.last_crash_at = book.last_crash_at();
        self.findings = book.newest(STATUS_FINDINGS);
        self
    }
}

/// The `ProfilerStatus` contract shape. Reads the directory, so call it off the async
/// workers and without holding the findings lock.
pub(super) fn status_json(dir: &Path, live: StatusLive) -> Value {
    json!({
        "enabled": live.enabled,
        "recordingSince": live.recording_since,
        "diskBytes": store::disk_bytes(dir),
        "eventCount": live.event_count,
        "droppedEvents": live.dropped_events,
        "lastCrashAt": live.last_crash_at,
        "capabilities": capabilities(dir, live.storage_failing),
        "findings": live.findings.iter().map(Finding::to_wire).collect::<Vec<_>>(),
    })
}

/// Native capture states come from `capabilities.json` when it holds contract values.
fn capabilities(dir: &Path, storage_failing: bool) -> Value {
    let recorded: Value = fs::read(dir.join(CAPABILITIES_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null);
    let state = |key: &str| match recorded.get(key).and_then(Value::as_str) {
        Some(value @ ("available" | "unavailable" | "disabled")) => value.to_owned(),
        _ => "unavailable".to_owned(),
    };
    json!({
        "nativeDumps": state("nativeDumps"),
        "osCrashRecords": state("osCrashRecords"),
        "storage": if storage_failing { "failing" } else { "ok" },
    })
}

pub struct Reader {
    dir: PathBuf,
}

impl Reader {
    /// `dir` is the profiler directory (`<state dir>/profiler`).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The same shape as `ProfilerRuntime::status`, computed from the files. With no live
    /// writer to ask, `enabled` is the persisted setting, `recordingSince` is null, and the
    /// event counts are what the event files hold (best effort).
    pub fn status(&self) -> Value {
        let (event_count, dropped_events) = self.count_events();
        let book = FindingsBook::read_only(&self.dir);
        status_json(
            &self.dir,
            StatusLive {
                enabled: self.enabled(),
                event_count,
                dropped_events,
                ..StatusLive::default()
            }
            .with_findings(&book),
        )
    }

    /// Findings seen at or after `since` whose kind matches, newest first. A `kind` ending in
    /// `*` matches by prefix.
    pub fn findings(
        &self,
        since: Option<OffsetDateTime>,
        kind: Option<&str>,
        limit: usize,
    ) -> Vec<Value> {
        FindingsBook::read_only(&self.dir)
            .newest(usize::MAX)
            .iter()
            .filter_map(|finding| serde_json::to_value(finding).ok())
            .filter(|finding| matches(finding, "lastSeen", since, kind))
            .take(limit.min(max_limit()))
            .collect()
    }

    /// The newest `limit` events at or after `since` whose kind matches, oldest first: file
    /// order (the file index, then line order), because `seq` restarts with each writer
    /// session. Torn and invalid lines are skipped.
    pub fn events(
        &self,
        since: Option<OffsetDateTime>,
        kind: Option<&str>,
        limit: usize,
    ) -> Vec<Value> {
        let limit = limit.min(max_limit());
        let mut newest = VecDeque::new();
        if limit == 0 {
            return Vec::new();
        }
        for (_, path) in self.event_files() {
            if since.is_some_and(|since| written_before(&path, since)) {
                continue;
            }
            // A file retention removed since the listing is simply gone.
            let Ok(file) = File::open(&path) else {
                continue;
            };
            for_each_line(BufReader::new(file), |line| {
                if let Some(event) = parse_object(line)
                    && matches(&event, "ts", since, kind)
                {
                    if newest.len() == limit {
                        newest.pop_front();
                    }
                    newest.push_back(event);
                }
            });
        }
        newest.into()
    }

    /// The newest `limit` crash sidecars (`crashes/*.json`), newest first. Their shape belongs
    /// to the crash writer, so each is returned as found, with `id` (the file stem) added when
    /// it has none.
    pub fn crashes(&self, limit: usize) -> Vec<Value> {
        let Ok(entries) = fs::read_dir(self.dir.join(CRASHES_DIR)) else {
            return Vec::new();
        };
        let mut sidecars: Vec<(SystemTime, PathBuf)> = entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
            .collect();
        sidecars.sort_unstable_by(|a, b| b.cmp(a));
        sidecars
            .into_iter()
            .filter_map(|(_, path)| read_sidecar(&path))
            .take(limit.min(max_limit()))
            .collect()
    }

    /// Calls `emit` with every matching event written from now on, for `duration` (at most
    /// [`MAX_FOLLOW`]). Polls every 500 ms, tracking the file and the byte offset. A line is
    /// reported once its newline has arrived. An `emit` error ends the follow with that error.
    pub fn follow(
        &self,
        kind: Option<&str>,
        duration: Duration,
        mut emit: impl FnMut(Value) -> io::Result<()>,
    ) -> io::Result<()> {
        // Lines already complete in the newest file are history, not news.
        let mut cursor = self
            .event_files()
            .pop()
            .map(|(index, path)| Cursor::at_end_of_complete_lines(index, &path));
        poll_until(duration, || {
            for (index, path) in self.event_files() {
                let current = cursor.get_or_insert_with(|| Cursor::new(index));
                if index < current.index {
                    continue;
                }
                if index > current.index {
                    // A file that appeared after the one being followed.
                    *current = Cursor::new(index);
                }
                drain(&path, current, kind, &mut emit)?;
            }
            Ok(())
        })
    }

    /// Calls `emit` with every finding that appears or changes from now on, for `duration`.
    /// `findings.ndjson` is replaced when compacted, so it is replayed whenever it changes
    /// rather than followed by offset.
    pub fn follow_findings(
        &self,
        kind: Option<&str>,
        duration: Duration,
        mut emit: impl FnMut(Value) -> io::Result<()>,
    ) -> io::Result<()> {
        let path = self.dir.join(FINDINGS_FILE);
        let mut seen: HashMap<String, (u64, String)> = HashMap::new();
        let mut file_state = None;
        let mut baseline = true;
        poll_until(duration, || {
            let now = fs::metadata(&path)
                .ok()
                .map(|meta| (meta.len(), meta.modified().ok()));
            if !baseline && now == file_state {
                return Ok(());
            }
            file_state = now;
            // Oldest first, so findings arrive in the order they happened.
            for finding in FindingsBook::read_only(&self.dir)
                .newest(usize::MAX)
                .into_iter()
                .rev()
            {
                let state = (finding.count, finding.last_seen.clone());
                if seen.get(&finding.id) == Some(&state) {
                    continue;
                }
                seen.insert(finding.id.clone(), state);
                // The first pass only records what was already there.
                if !baseline
                    && let Ok(value) = serde_json::to_value(&finding)
                    && matches(&value, "lastSeen", None, kind)
                {
                    emit(value)?;
                }
            }
            baseline = false;
            Ok(())
        })
    }

    /// The persisted `enableProfiler` setting, which sits beside the profiler directory.
    fn enabled(&self) -> bool {
        self.dir
            .parent()
            .and_then(|state| fs::read(state.join(SETTINGS_FILE)).ok())
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|settings| settings.get("enableProfiler")?.as_bool())
            .unwrap_or(false)
    }

    /// `(events, dropped)` from the lines the event files hold: the sum of `telemetry.dropped`
    /// counts, and every other complete line.
    fn count_events(&self) -> (u64, u64) {
        let (mut events, mut dropped) = (0, 0);
        for (_, path) in self.event_files() {
            let Ok(file) = File::open(path) else {
                continue;
            };
            for_each_line(BufReader::new(file), |line| {
                match dropped_count(line) {
                    Some(count) => dropped += count,
                    None => events += 1,
                };
            });
        }
        (events, dropped)
    }

    /// The `events-*.ndjson` files in write order.
    fn event_files(&self) -> Vec<(u64, PathBuf)> {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut files: Vec<(u64, PathBuf)> = entries
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .filter_map(|entry| {
                let index = event_file_index(&entry.file_name().to_string_lossy())?;
                Some((index, entry.path()))
            })
            .collect();
        files.sort_unstable();
        files
    }
}

fn max_limit() -> usize {
    usize::from(MAX_LIMIT)
}

/// Runs `poll`, then again every 500 ms, with a last run at the deadline.
fn poll_until(duration: Duration, mut poll: impl FnMut() -> io::Result<()>) -> io::Result<()> {
    let deadline = Instant::now() + duration.min(MAX_FOLLOW);
    loop {
        poll()?;
        let now = Instant::now();
        if now >= deadline {
            return Ok(());
        }
        std::thread::sleep(FOLLOW_POLL.min(deadline - now));
    }
}

/// Where a follow stands in an events file: the byte offset after the last line it consumed,
/// that line's `(sid, seq)`, which lets it skip what it already reported when the store cuts
/// the file back, and the end of that line, to notice a file rewritten under the offset.
struct Cursor {
    index: u64,
    offset: u64,
    last: Option<(String, u64)>,
    /// The last (up to) `ECHO_BYTES` bytes before `offset`; empty when unknown.
    echo: Vec<u8>,
}

impl Cursor {
    fn new(index: u64) -> Self {
        Self {
            index,
            offset: 0,
            last: None,
            echo: Vec::new(),
        }
    }

    /// Whether the file still holds the bytes this cursor consumed last.
    fn echo_intact(&self, file: &mut File) -> bool {
        let Some(start) = self.offset.checked_sub(self.echo.len() as u64) else {
            return false;
        };
        let mut found = vec![0; self.echo.len()];
        file.seek(SeekFrom::Start(start))
            .and_then(|_| file.read_exact(&mut found))
            .is_ok()
            && found == self.echo
    }

    /// Stands after the last complete line of `path`, so a record the writer is part-way
    /// through is reported once it completes.
    fn at_end_of_complete_lines(index: u64, path: &Path) -> Self {
        let mut cursor = Self::new(index);
        let Ok(mut file) = File::open(path) else {
            return cursor;
        };
        let Ok(len) = file.metadata().map(|meta| meta.len()) else {
            return cursor;
        };
        let start = len.saturating_sub(TAIL_WINDOW);
        let mut tail = Vec::new();
        if file.seek(SeekFrom::Start(start)).is_err()
            || file.take(TAIL_WINDOW).read_to_end(&mut tail).is_err()
        {
            cursor.offset = len;
            return cursor;
        }
        match tail.iter().rposition(|&byte| byte == b'\n') {
            Some(newline) => {
                cursor.offset = start + newline as u64 + 1;
                cursor.set_echo(&tail[..=newline]);
                cursor.last = tail[..newline]
                    .rsplit(|&byte| byte == b'\n')
                    .next()
                    .and_then(parse_object)
                    .and_then(|event| stamp(&event));
            }
            // No complete line anywhere: the file is empty or one record being written.
            None if start == 0 => {}
            // Inside a record longer than the window.
            None => cursor.offset = len,
        }
        cursor
    }
}

impl Cursor {
    fn set_echo(&mut self, consumed: &[u8]) {
        self.echo.clear();
        self.echo
            .extend_from_slice(&consumed[consumed.len().saturating_sub(ECHO_BYTES)..]);
    }
}

fn stamp(event: &Value) -> Option<(String, u64)> {
    Some((
        event.get("sid")?.as_str()?.to_owned(),
        event.get("seq")?.as_u64()?,
    ))
}

/// Emits the matching events in the complete lines of `path` after the cursor, and moves the
/// cursor past them. A line without its newline is left for the next call.
fn drain(
    path: &Path,
    cursor: &mut Cursor,
    kind: Option<&str>,
    emit: &mut impl FnMut(Value) -> io::Result<()>,
) -> io::Result<()> {
    // A file deleted or unreadable since the listing has nothing new.
    let Ok(mut file) = File::open(path) else {
        return Ok(());
    };
    let Ok(len) = file.metadata().map(|meta| meta.len()) else {
        return Ok(());
    };
    // Shorter than before, or no longer holding the last bytes consumed: after a failed write
    // the store cuts the file back to its last flush and appends again, however far it has
    // regrown by now. Read it afresh, skipping what was already reported (`seq` only rises
    // within a writer session).
    let rescan = len < cursor.offset || !cursor.echo_intact(&mut file);
    if rescan {
        cursor.offset = 0;
        cursor.echo.clear();
    }
    if len <= cursor.offset || file.seek(SeekFrom::Start(cursor.offset)).is_err() {
        return Ok(());
    }
    let reported = if rescan { cursor.last.clone() } else { None };
    // Bounded by the length seen above, so a file that keeps growing cannot hold this call.
    let mut reader = BufReader::new(file.take(len - cursor.offset));
    let mut line = Vec::new();
    loop {
        line.clear();
        if !matches!(reader.read_until(b'\n', &mut line), Ok(read) if read > 0) {
            return Ok(());
        }
        if line.last() != Some(&b'\n') {
            return Ok(());
        }
        cursor.offset += line.len() as u64;
        cursor.set_echo(&line);
        let Some(event) = parse_object(&line) else {
            continue;
        };
        let line_stamp = stamp(&event);
        if let (Some((sid, seq)), Some((last_sid, last_seq))) = (&line_stamp, &reported)
            && sid == last_sid
            && seq <= last_seq
        {
            continue;
        }
        if line_stamp.is_some() {
            cursor.last = line_stamp;
        }
        if matches(&event, "ts", None, kind) {
            emit(event)?;
        }
    }
}

/// Calls `each` with every complete line, newline removed. A last line with no newline is
/// torn (or still being written) and is skipped; a read error ends the scan.
fn for_each_line(mut reader: impl BufRead, mut each: impl FnMut(&[u8])) {
    let mut line = Vec::new();
    loop {
        line.clear();
        if !matches!(reader.read_until(b'\n', &mut line), Ok(read) if read > 0) {
            return;
        }
        if line.pop() != Some(b'\n') {
            return;
        }
        each(&line);
    }
}

fn parse_object(line: &[u8]) -> Option<Value> {
    serde_json::from_slice(line)
        .ok()
        .filter(|value: &Value| value.is_object())
}

/// Whether `record` has a matching `kind` (a trailing `*` matches by prefix) and a `time_key`
/// timestamp at or after `since`. A record with no readable timestamp never matches a `since`.
fn matches(
    record: &Value,
    time_key: &str,
    since: Option<OffsetDateTime>,
    kind: Option<&str>,
) -> bool {
    if let Some(wanted) = kind {
        let actual = record.get("kind").and_then(Value::as_str).unwrap_or("");
        let hit = match wanted.strip_suffix('*') {
            Some(prefix) => actual.starts_with(prefix),
            None => actual == wanted,
        };
        if !hit {
            return false;
        }
    }
    since.is_none_or(|since| {
        record
            .get(time_key)
            .and_then(Value::as_str)
            .and_then(|at| OffsetDateTime::parse(at, &Rfc3339).ok())
            .is_some_and(|at| at >= since)
    })
}

/// A file last written before `since` (with slack for clock steps) holds no line from after it.
fn written_before(path: &Path, since: OffsetDateTime) -> bool {
    fs::metadata(path)
        .and_then(|meta| meta.modified())
        .is_ok_and(|modified| OffsetDateTime::from(modified) + MTIME_SKEW < since)
}

/// The count carried by a `telemetry.dropped` line, `None` for any other line. The substring
/// test keeps the common line from being parsed.
fn dropped_count(line: &[u8]) -> Option<u64> {
    if !std::str::from_utf8(line).is_ok_and(|text| text.contains(DROPPED_KIND)) {
        return None;
    }
    let event = parse_object(line)?;
    if event.get("kind").and_then(Value::as_str) != Some(DROPPED_KIND) {
        return None;
    }
    event.get("count").and_then(Value::as_u64)
}

fn read_sidecar(path: &Path) -> Option<Value> {
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take(MAX_SIDECAR_BYTES)
        .read_to_end(&mut bytes)
        .ok()?;
    let mut sidecar = parse_object(&bytes)?;
    if let Some(fields) = sidecar.as_object_mut() {
        let id = path.file_stem()?.to_string_lossy().into_owned();
        fields.entry("id").or_insert(Value::String(id));
    }
    Some(sidecar)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn event_line(sid: &str, seq: u64, kind: &str, ts: &str) -> String {
        format!(
            r#"{{"v":1,"seq":{seq},"sid":"{sid}","ts":"{ts}","kind":"{kind}","source":"server"}}"#
        )
    }

    fn write_lines(path: &Path, lines: &[String]) {
        let mut text = lines.join("\n");
        text.push('\n');
        fs::write(path, text).unwrap();
    }

    fn finding_line(kind: &str, fingerprint: &str, count: u64, last_seen: &str) -> String {
        format!(
            r#"{{"id":"{kind}-{fingerprint}","fingerprint":"{fingerprint}","kind":"{kind}","severity":"warn","firstSeen":"2026-10-09T10:00:00Z","lastSeen":"{last_seen}","count":{count},"source":"server","title":"t","summary":"s","state":"open","evidence":{{}}}}"#
        )
    }

    fn profiler_dir(state: &Path) -> PathBuf {
        let dir = state.join("profiler");
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn ids(events: &[Value]) -> Vec<String> {
        events
            .iter()
            .map(|event| format!("{}:{}", event["sid"].as_str().unwrap(), event["seq"]))
            .collect()
    }

    fn listing(dir: &Path) -> Vec<(String, u64)> {
        let mut entries: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|entry| {
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    entry.metadata().unwrap().len(),
                )
            })
            .collect();
        entries.sort();
        entries
    }

    #[test]
    fn events_return_complete_lines_in_file_order_and_skip_a_torn_one() {
        let state = tempfile::tempdir().unwrap();
        let dir = profiler_dir(state.path());
        let at = "2026-10-09T10:00:00.123456789Z";
        // `seq` restarts with each writer session, so only the file order is meaningful.
        write_lines(
            &dir.join("events-000000.ndjson"),
            &[
                event_line("aaaa", 0, "a", at),
                event_line("aaaa", 1, "b", at),
                "not json".to_owned(),
            ],
        );
        let mut second = event_line("bbbb", 0, "c", at);
        second.push('\n');
        second.push_str(r#"{"v":1,"seq":1,"sid":"bbbb","ts":"2026-10-09T10:0"#);
        fs::write(dir.join("events-000001.ndjson"), second).unwrap();

        let events = Reader::new(&dir).events(None, None, 100);

        assert_eq!(ids(&events), ["aaaa:0", "aaaa:1", "bbbb:0"]);
    }

    #[test]
    fn events_keep_the_newest_matches_within_the_limit_and_filter_by_kind_and_time() {
        let state = tempfile::tempdir().unwrap();
        let dir = profiler_dir(state.path());
        write_lines(
            &dir.join("events-000000.ndjson"),
            &[
                event_line("s", 0, "rpc.slow", "2026-10-09T10:00:00Z"),
                event_line("s", 1, "process.sample", "2026-10-09T10:01:00Z"),
                event_line("s", 2, "rpc.slow", "2026-10-09T10:02:00Z"),
                event_line("s", 3, "rpc.error", "2026-10-09T10:03:00.5Z"),
                event_line("s", 4, "rpc.slow", "2026-10-09T10:04:00Z"),
            ],
        );
        let reader = Reader::new(&dir);

        assert_eq!(ids(&reader.events(None, None, 2)), ["s:3", "s:4"]);
        assert_eq!(
            ids(&reader.events(None, Some("rpc.slow"), 2)),
            ["s:2", "s:4"]
        );
        assert_eq!(
            ids(&reader.events(None, Some("rpc.*"), 10)),
            ["s:0", "s:2", "s:3", "s:4"]
        );
        let since = OffsetDateTime::parse("2026-10-09T10:02:30Z", &Rfc3339).unwrap();
        assert_eq!(ids(&reader.events(Some(since), None, 10)), ["s:3", "s:4"]);
        assert!(reader.events(None, None, 0).is_empty());
    }

    #[test]
    fn a_missing_directory_reads_as_empty_and_is_not_created() {
        let state = tempfile::tempdir().unwrap();
        let dir = state.path().join("profiler");
        let reader = Reader::new(&dir);

        assert!(reader.events(None, None, 10).is_empty());
        assert!(reader.findings(None, None, 10).is_empty());
        assert!(reader.crashes(10).is_empty());
        let status = reader.status();
        assert_eq!(status["enabled"], false);
        assert_eq!(status["diskBytes"], 0);
        assert_eq!(status["eventCount"], 0);
        assert_eq!(status["findings"], json!([]));
        assert_eq!(status["capabilities"]["storage"], "ok");
        assert!(!dir.exists());
    }

    #[test]
    fn findings_filter_by_kind_and_time_newest_first_and_last_line_per_id_wins() {
        let state = tempfile::tempdir().unwrap();
        let dir = profiler_dir(state.path());
        write_lines(
            &dir.join("findings.ndjson"),
            &[
                finding_line("rpc.slow", "aaaaaaaaaaaa", 1, "2026-10-09T10:00:00Z"),
                finding_line("leak.suspect", "bbbbbbbbbbbb", 1, "2026-10-09T11:00:00Z"),
                finding_line("rpc.slow", "aaaaaaaaaaaa", 4, "2026-10-09T12:00:00Z"),
                finding_line("rpc.slow", "cccccccccccc", 1, "2026-10-09T09:00:00Z"),
            ],
        );
        let before = listing(&dir);
        let reader = Reader::new(&dir);

        let slow = reader.findings(None, Some("rpc.slow"), 10);
        assert_eq!(slow.len(), 2);
        assert_eq!(slow[0]["id"], "rpc.slow-aaaaaaaaaaaa");
        assert_eq!(slow[0]["count"], 4);
        assert!(slow[0]["evidence"].is_object());
        assert_eq!(reader.findings(None, None, 10).len(), 3);
        assert_eq!(reader.findings(None, None, 1)[0]["kind"], "rpc.slow");
        let since = OffsetDateTime::parse("2026-10-09T09:30:00Z", &Rfc3339).unwrap();
        assert_eq!(reader.findings(Some(since), None, 10).len(), 2);
        // Reading never rewrites the file, compacts it or adds a file.
        assert_eq!(listing(&dir), before);
        assert_eq!(
            fs::read_to_string(dir.join("findings.ndjson"))
                .unwrap()
                .lines()
                .count(),
            4
        );
    }

    #[test]
    fn status_is_built_from_the_files_and_the_persisted_setting() {
        let state = tempfile::tempdir().unwrap();
        let dir = profiler_dir(state.path());
        fs::write(
            state.path().join("settings.json"),
            r#"{"enableProfiler":true}"#,
        )
        .unwrap();
        fs::write(
            dir.join("capabilities.json"),
            r#"{"nativeDumps":"available","osCrashRecords":"bogus"}"#,
        )
        .unwrap();
        let at = "2026-10-09T10:00:00Z";
        write_lines(
            &dir.join("events-000000.ndjson"),
            &[
                event_line("s", 0, "a", at),
                format!(
                    r#"{{"v":1,"seq":1,"sid":"s","ts":"{at}","kind":"telemetry.dropped","source":"server","count":7}}"#
                ),
                event_line("s", 2, "b", at),
            ],
        );
        write_lines(
            &dir.join("findings.ndjson"),
            &[finding_line("crash.native", "aaaaaaaaaaaa", 1, at)],
        );

        let status = Reader::new(&dir).status();

        assert_eq!(status["enabled"], true);
        assert!(status["recordingSince"].is_null());
        assert_eq!(status["eventCount"], 2);
        assert_eq!(status["droppedEvents"], 7);
        assert_eq!(status["lastCrashAt"], at);
        assert_eq!(status["capabilities"]["nativeDumps"], "available");
        assert_eq!(status["capabilities"]["osCrashRecords"], "unavailable");
        assert_eq!(status["findings"][0]["kind"], "crash.native");
        assert!(status["findings"][0].get("evidence").is_none());
        assert!(status["diskBytes"].as_u64().unwrap() > 0);

        fs::write(state.path().join("settings.json"), "not json").unwrap();
        assert_eq!(Reader::new(&dir).status()["enabled"], false);
    }

    #[test]
    fn crashes_list_sidecars_newest_first_and_skip_unreadable_ones() {
        let state = tempfile::tempdir().unwrap();
        let crashes = profiler_dir(state.path()).join("crashes");
        fs::create_dir_all(&crashes).unwrap();
        for (name, body, age_secs) in [
            ("old.json", r#"{"id":"custom-old","os":"linux"}"#, 300),
            ("new.json", r#"{"os":"linux"}"#, 10),
            ("broken.json", "{", 5),
            ("dump.dmp", "MDMP", 1),
        ] {
            let path = crashes.join(name);
            fs::write(&path, body).unwrap();
            fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(SystemTime::now() - Duration::from_secs(age_secs))
                .unwrap();
        }
        let reader = Reader::new(state.path().join("profiler"));

        let all = reader.crashes(10);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0]["id"], "new");
        assert_eq!(all[1]["id"], "custom-old");
        assert_eq!(reader.crashes(1).len(), 1);
    }

    fn append(path: &Path, text: &str) {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        file.write_all(text.as_bytes()).unwrap();
    }

    /// Runs `writer` on another thread after `delay`, and follows the directory meanwhile.
    fn follow_while(
        reader: &Reader,
        kind: Option<&str>,
        duration: Duration,
        delay: Duration,
        writer: impl FnOnce() + Send + 'static,
    ) -> Vec<Value> {
        let writer = std::thread::spawn(move || {
            std::thread::sleep(delay);
            writer();
        });
        let mut seen = Vec::new();
        reader
            .follow(kind, duration, |event| {
                seen.push(event);
                Ok(())
            })
            .unwrap();
        writer.join().unwrap();
        seen
    }

    #[test]
    fn follow_picks_up_a_line_appended_after_it_started() {
        let state = tempfile::tempdir().unwrap();
        let dir = profiler_dir(state.path());
        let path = dir.join("events-000000.ndjson");
        write_lines(&path, &[event_line("s", 0, "old", "2026-10-09T10:00:00Z")]);
        let new_line = event_line("s", 1, "new", "2026-10-09T10:00:01Z");

        let seen = follow_while(
            &Reader::new(&dir),
            None,
            Duration::from_millis(300),
            Duration::from_millis(100),
            move || append(&path, &format!("{new_line}\n")),
        );

        assert_eq!(ids(&seen), ["s:1"]);
    }

    #[test]
    fn follow_waits_for_the_newline_follows_rotation_and_filters_by_kind() {
        let state = tempfile::tempdir().unwrap();
        let dir = profiler_dir(state.path());
        let first = dir.join("events-000000.ndjson");
        write_lines(&first, &[event_line("s", 0, "old", "2026-10-09T10:00:00Z")]);
        let torn = event_line("s", 1, "keep", "2026-10-09T10:00:01Z");
        let (head, tail) = torn.split_at(torn.len() / 2);
        let (head, tail) = (head.to_owned(), format!("{tail}\n"));
        let second = dir.join("events-000001.ndjson");
        let rotated = format!(
            "{}\n{}\n",
            event_line("s", 2, "skip", "2026-10-09T10:00:02Z"),
            event_line("s", 3, "keep", "2026-10-09T10:00:03Z"),
        );

        let seen = follow_while(
            &Reader::new(&dir),
            Some("keep"),
            Duration::from_millis(1300),
            Duration::from_millis(100),
            move || {
                // The first half is on disk at the poll at 500 ms and must not be reported
                // or lost; the second half and the next file arrive before the poll at 1000 ms.
                append(&first, &head);
                std::thread::sleep(Duration::from_millis(650));
                append(&first, &tail);
                append(&second, &rotated);
            },
        );

        assert_eq!(ids(&seen), ["s:1", "s:3"]);
    }

    #[test]
    fn follow_reports_a_record_that_was_part_written_when_it_started() {
        let state = tempfile::tempdir().unwrap();
        let dir = profiler_dir(state.path());
        let path = dir.join("events-000000.ndjson");
        let torn = event_line("s", 1, "new", "2026-10-09T10:00:01Z");
        let (head, tail) = torn.split_at(torn.len() / 2);
        let tail = format!("{tail}\n");
        fs::write(
            &path,
            format!(
                "{}\n{head}",
                event_line("s", 0, "old", "2026-10-09T10:00:00Z")
            ),
        )
        .unwrap();

        let seen = follow_while(
            &Reader::new(&dir),
            None,
            Duration::from_millis(300),
            Duration::from_millis(100),
            move || append(&path, &tail),
        );

        assert_eq!(ids(&seen), ["s:1"]);
    }

    #[test]
    fn follow_reads_a_file_the_store_cut_back_without_repeating_records() {
        let state = tempfile::tempdir().unwrap();
        let dir = profiler_dir(state.path());
        let path = dir.join("events-000000.ndjson");
        let at = "2026-10-09T10:00:00Z";
        let first = format!("{}\n", event_line("s", 0, "a", at));
        let second = format!("{}\n", event_line("s", 1, "b", at));
        let third = format!(
            "{}\n",
            event_line("s", 2, "a-longer-kind-than-the-next", at)
        );
        let replacement = format!("{}\n", event_line("s", 3, "c", at));
        fs::write(&path, &first).unwrap();
        let kept = (first.len() + second.len()) as u64;

        let seen = follow_while(
            &Reader::new(&dir),
            None,
            Duration::from_millis(1300),
            Duration::from_millis(100),
            move || {
                append(&path, &format!("{second}{third}"));
                // Reported by the poll at 500 ms. Then a failed write: the store cuts the
                // file back to its last flush and appends, leaving it shorter than before.
                std::thread::sleep(Duration::from_millis(650));
                fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_len(kept)
                    .unwrap();
                append(&path, &replacement);
            },
        );

        assert_eq!(ids(&seen), ["s:1", "s:2", "s:3"]);
    }

    #[test]
    fn follow_notices_a_file_refilled_to_the_length_it_had() {
        let state = tempfile::tempdir().unwrap();
        let dir = profiler_dir(state.path());
        let path = dir.join("events-000000.ndjson");
        let at = "2026-10-09T10:00:00Z";
        let line = |seq, kind| format!("{}\n", event_line("s", seq, kind, at));
        let (first, second, third) = (line(0, "a"), line(1, "b"), line(2, "c"));
        fs::write(&path, &first).unwrap();
        let kept = (first.len() + second.len()) as u64;
        // As long as the line it replaces, and then one longer.
        let (same_length, longer) = (line(3, "d"), line(4, "eee"));
        assert_eq!(same_length.len(), third.len());

        let seen = follow_while(
            &Reader::new(&dir),
            None,
            Duration::from_millis(1800),
            Duration::from_millis(100),
            move || {
                append(&path, &format!("{second}{third}"));
                // Each replacement lands between two polls (at 500 ms intervals).
                for (wait, replacement) in [(650, same_length), (500, longer)] {
                    std::thread::sleep(Duration::from_millis(wait));
                    // The store cuts the file back and it regrows to at least its old length.
                    fs::OpenOptions::new()
                        .write(true)
                        .open(&path)
                        .unwrap()
                        .set_len(kept)
                        .unwrap();
                    append(&path, &replacement);
                }
            },
        );

        assert_eq!(ids(&seen), ["s:1", "s:2", "s:3", "s:4"]);
    }

    #[test]
    fn follow_stops_with_the_error_of_a_failing_emit() {
        let state = tempfile::tempdir().unwrap();
        let dir = profiler_dir(state.path());
        let path = dir.join("events-000000.ndjson");
        write_lines(&path, &[event_line("s", 0, "old", "2026-10-09T10:00:00Z")]);
        let line = event_line("s", 1, "new", "2026-10-09T10:00:01Z");
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            append(&path, &format!("{line}\n"));
        });
        let started = Instant::now();

        let result = Reader::new(&dir).follow(None, Duration::from_secs(30), |_| {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        });

        writer.join().unwrap();
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn follow_findings_reports_new_and_changed_findings_only() {
        let state = tempfile::tempdir().unwrap();
        let dir = profiler_dir(state.path());
        let path = dir.join("findings.ndjson");
        write_lines(
            &path,
            &[finding_line(
                "rpc.slow",
                "aaaaaaaaaaaa",
                1,
                "2026-10-09T10:00:00Z",
            )],
        );
        let update = format!(
            "{}\n{}\n",
            finding_line("rpc.slow", "aaaaaaaaaaaa", 2, "2026-10-09T10:05:00Z"),
            finding_line("leak.suspect", "bbbbbbbbbbbb", 1, "2026-10-09T10:06:00Z"),
        );
        let writer = std::thread::spawn({
            let path = path.clone();
            move || {
                std::thread::sleep(Duration::from_millis(100));
                append(&path, &update);
            }
        });
        let mut seen = Vec::new();

        Reader::new(&dir)
            .follow_findings(None, Duration::from_millis(300), |finding| {
                seen.push(finding);
                Ok(())
            })
            .unwrap();
        writer.join().unwrap();

        let reported: Vec<_> = seen
            .iter()
            .map(|finding| {
                (
                    finding["kind"].as_str().unwrap(),
                    finding["count"].as_u64().unwrap(),
                )
            })
            .collect();
        assert_eq!(reported, [("rpc.slow", 2), ("leak.suspect", 1)]);
    }
}
