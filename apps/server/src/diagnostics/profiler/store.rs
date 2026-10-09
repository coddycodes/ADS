use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    },
    thread::JoinHandle,
    time::{Duration, Instant, SystemTime},
};

use serde_json::{Map, Value};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::event::ProfilerEvent;

pub const CHANNEL_CAPACITY: usize = 2048;

const FINDINGS_FILE: &str = "findings.ndjson";
const CRASHES_DIR: &str = "crashes";
const WRITE_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug)]
pub struct StoreConfig {
    pub dir: PathBuf,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
    pub max_age: Duration,
    pub flush_every: Duration,
}

impl StoreConfig {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            max_file_bytes: 10 * 1024 * 1024,
            max_total_bytes: 256 * 1024 * 1024,
            max_age: Duration::from_secs(7 * 24 * 3600),
            flush_every: Duration::from_secs(2),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StoreStats {
    pub written: u64,
    pub dropped: u64,
    pub failing: bool,
}

#[derive(Default)]
struct Counters {
    written: AtomicU64,
    dropped: AtomicU64,
    failing: AtomicBool,
}

enum Message {
    Event(ProfilerEvent),
    /// One finding line for `findings.ndjson`. Appended through its own short-lived
    /// handle; a failed append counts as a drop and sets `failing`.
    Finding(Value),
    Shutdown,
}

#[derive(Clone)]
pub struct EventSender {
    tx: SyncSender<Message>,
    counters: Arc<Counters>,
}

impl EventSender {
    /// Never blocks; a full channel drops the event and counts it.
    pub fn send(&self, event: ProfilerEvent) {
        self.enqueue(Message::Event(event));
    }

    /// Queues one finding for `findings.ndjson`. Never blocks; a full channel drops it.
    pub fn send_finding(&self, finding: Value) {
        self.enqueue(Message::Finding(finding));
    }

    fn enqueue(&self, message: Message) {
        if self.tx.try_send(message).is_err() {
            self.counters.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

pub struct ProfilerStore {
    sender: EventSender,
    thread: Option<JoinHandle<()>>,
    pause: Option<SyncSender<()>>,
}

impl ProfilerStore {
    pub fn start(config: StoreConfig) -> Self {
        Self::spawn(config, CHANNEL_CAPACITY, false)
    }

    #[cfg(test)]
    fn start_paused(config: StoreConfig, capacity: usize) -> Self {
        Self::spawn(config, capacity, true)
    }

    #[cfg(test)]
    fn resume(&self) {
        if let Some(pause) = &self.pause {
            let _ = pause.send(());
        }
    }

    fn spawn(config: StoreConfig, capacity: usize, paused: bool) -> Self {
        let (tx, rx) = mpsc::sync_channel(capacity);
        let counters = Arc::new(Counters::default());
        let (pause_tx, pause_rx) = mpsc::sync_channel::<()>(1);
        let writer_counters = counters.clone();
        let thread = std::thread::Builder::new()
            .name("bibcode-profiler-writer".into())
            .spawn(move || {
                if paused {
                    let _ = pause_rx.recv();
                }
                Writer::new(config, writer_counters).run(rx);
            })
            .ok();
        if thread.is_none() {
            // The closure (and its receiver) is gone, so every send counts as a drop.
            counters.failing.store(true, Ordering::Relaxed);
        }
        Self {
            sender: EventSender { tx, counters },
            thread,
            pause: paused.then_some(pause_tx),
        }
    }

    pub fn sender(&self) -> EventSender {
        self.sender.clone()
    }

    pub fn stats(&self) -> StoreStats {
        let c = &self.sender.counters;
        StoreStats {
            written: c.written.load(Ordering::Relaxed),
            dropped: c.dropped.load(Ordering::Relaxed),
            failing: c.failing.load(Ordering::Relaxed),
        }
    }

    /// Writes everything already queued, flushes, and joins the writer thread.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        if let Some(pause) = &self.pause {
            let _ = pause.try_send(());
        }
        // Shutdown is queued behind pending events, so they are all written first.
        let _ = self.sender.tx.send(Message::Shutdown);
        let _ = thread.join();
    }
}

impl Drop for ProfilerStore {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Writer {
    config: StoreConfig,
    counters: Arc<Counters>,
    /// The live events file and its length in bytes.
    out: Option<(BufWriter<File>, u64)>,
    next_file: u64,
    seq: u64,
    sid: String,
    /// Lines accepted into `out`'s buffer and not yet flushed to the OS.
    pending: u64,
    reported_drops: u64,
}

impl Writer {
    fn new(config: StoreConfig, counters: Arc<Counters>) -> Self {
        let next_file = existing_max_file_index(&config.dir).map_or(0, |n| n + 1);
        // `sid` makes event references unique across restarts: `seq` restarts at 0
        // for each writer session, so evidence is always `"<sid>:<seq>"`.
        let sid = uuid::Uuid::new_v4().simple().to_string()[..8].to_owned();
        Self {
            config,
            counters,
            out: None,
            next_file,
            seq: 0,
            sid,
            pending: 0,
            reported_drops: 0,
        }
    }

    fn run(mut self, rx: Receiver<Message>) {
        let mut last_flush = Instant::now();
        loop {
            match rx.recv_timeout(self.config.flush_every) {
                Ok(Message::Event(event)) => self.write(event),
                Ok(Message::Finding(finding)) => self.append_finding(&finding),
                Ok(Message::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
            if last_flush.elapsed() >= self.config.flush_every {
                self.report_drops();
                self.flush();
                last_flush = Instant::now();
            }
        }
        self.report_drops();
        self.close();
    }

    fn report_drops(&mut self) {
        let dropped = self.counters.dropped.load(Ordering::Relaxed);
        let count = dropped.saturating_sub(self.reported_drops);
        if count > 0 {
            self.reported_drops = dropped;
            let mut line = Map::new();
            line.insert("kind".into(), "telemetry.dropped".into());
            line.insert("source".into(), "server".into());
            line.insert("count".into(), count.into());
            self.write_line(line);
        }
    }

    fn write(&mut self, event: ProfilerEvent) {
        let mut line = match event.payload {
            Value::Object(fields) => fields,
            Value::Null => Map::new(),
            other => Map::from_iter([("payload".to_owned(), other)]),
        };
        // Envelope fields are inserted last so a payload can never overwrite them.
        line.insert("kind".into(), event.kind.into());
        line.insert("source".into(), event.source.into());
        self.write_line(line);
    }

    fn write_line(&mut self, mut line: Map<String, Value>) {
        line.insert("v".into(), 1.into());
        line.insert("seq".into(), self.seq.into());
        line.insert("sid".into(), self.sid.clone().into());
        line.insert("ts".into(), now_rfc3339().into());
        self.seq += 1;
        let Ok(mut bytes) = serde_json::to_vec(&line) else {
            return;
        };
        bytes.push(b'\n');
        if bytes.len() as u64 > self.config.max_file_bytes {
            // One record bigger than a whole file would defeat rotation and retention.
            // Its `seq` is skipped, and the drop is reported like any other.
            self.counters.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        match self.append(&bytes) {
            Ok(()) => self.pending += 1,
            Err(_) => self.fail_events(1),
        }
    }

    fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        let max = self.config.max_file_bytes;
        if self
            .out
            .as_ref()
            .is_some_and(|(_, len)| *len > 0 && *len + bytes.len() as u64 > max)
        {
            self.close();
        }
        if self.out.is_none() {
            self.out = Some(self.open_next()?);
        }
        if let Some((out, len)) = self.out.as_mut() {
            out.write_all(bytes)?;
            *len += bytes.len() as u64;
        }
        Ok(())
    }

    /// Runs retention first so the space for the new live file is already free.
    fn open_next(&mut self) -> io::Result<(BufWriter<File>, u64)> {
        fs::create_dir_all(&self.config.dir)?;
        self.enforce_retention();
        let path = self
            .config
            .dir
            .join(format!("events-{:06}.ndjson", self.next_file));
        self.next_file += 1;
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok((BufWriter::with_capacity(WRITE_BUFFER_BYTES, file), 0))
    }

    fn append_finding(&mut self, finding: &Value) {
        let appended = serde_json::to_vec(finding)
            .map_err(io::Error::from)
            .and_then(|mut bytes| {
                bytes.push(b'\n');
                fs::create_dir_all(&self.config.dir)?;
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(self.config.dir.join(FINDINGS_FILE))?
                    .write_all(&bytes)
            });
        if appended.is_err() {
            self.count_lost(1);
        } else {
            // The directory accepted a write, so an earlier failure has recovered. An events
            // failure sets the flag again on its next failed write.
            self.counters.failing.store(false, Ordering::Relaxed);
        }
    }

    fn flush(&mut self) {
        let Some((out, _)) = self.out.as_mut() else {
            return;
        };
        match out.flush() {
            Ok(()) => self.commit(),
            Err(_) => self.fail_events(0),
        }
    }

    /// Flushes and syncs the live file; used on rotation and shutdown only.
    fn close(&mut self) {
        let Some((mut out, _)) = self.out.take() else {
            return;
        };
        match out.flush() {
            Ok(()) => {
                let _ = out.get_ref().sync_data();
                self.commit();
            }
            Err(_) => self.fail_events(0),
        }
    }

    fn commit(&mut self) {
        if self.pending > 0 {
            self.counters
                .written
                .fetch_add(self.pending, Ordering::Relaxed);
            self.pending = 0;
            self.counters.failing.store(false, Ordering::Relaxed);
        }
    }

    /// An I/O error loses the buffered lines (and `extra` more). Count them as drops and
    /// abandon the handle so the next event reopens a fresh file.
    fn fail_events(&mut self, extra: u64) {
        let lost = self.pending + extra;
        self.pending = 0;
        self.out = None;
        self.count_lost(lost);
    }

    fn count_lost(&mut self, lost: u64) {
        self.counters.failing.store(true, Ordering::Relaxed);
        self.counters.dropped.fetch_add(lost, Ordering::Relaxed);
        // These drops are already known here; do not report them as `telemetry.dropped`.
        self.reported_drops += lost;
    }

    /// Retention for the whole profiler directory, run before every new events file:
    /// 1. rotated `events-*` files older than `max_age` are deleted;
    /// 2. while the total (events + `crashes/` + `findings.ndjson`) plus room for the new
    ///    live file exceeds `max_total_bytes`, delete the oldest rotated events file;
    /// 3. if no rotated events file is left and the total is still over budget, delete the
    ///    oldest crash dump and its `.json` sidecar, repeating until under budget.
    ///
    /// `findings.ndjson` counts toward the total but is not trimmed here; whoever appends
    /// findings owns compacting that file.
    fn enforce_retention(&self) {
        let dir = &self.config.dir;
        let crashes = dir.join(CRASHES_DIR);
        let mut files = event_files(dir);
        files.sort();
        let mut total = files.iter().map(|f| f.1).sum::<u64>()
            + dir_size(&crashes)
            + file_len(&dir.join(FINDINGS_FILE));
        // The live file about to be opened may grow to `max_file_bytes`.
        let budget = self
            .config
            .max_total_bytes
            .saturating_sub(self.config.max_file_bytes);
        let now = SystemTime::now();
        for (path, len, modified) in files {
            let expired = now
                .duration_since(modified)
                .is_ok_and(|age| age > self.config.max_age);
            if (expired || total > budget) && fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(len);
            }
        }
        for (dump, len) in crash_dumps_oldest_first(&crashes) {
            if total <= budget {
                break;
            }
            let sidecar = dump.with_extension("json");
            let sidecar_len = file_len(&sidecar);
            if fs::remove_file(&sidecar).is_ok() {
                total = total.saturating_sub(sidecar_len);
            }
            if fs::remove_file(&dump).is_ok() {
                total = total.saturating_sub(len);
            }
        }
    }
}

fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

fn file_len(path: &Path) -> u64 {
    fs::metadata(path).map_or(0, |m| m.len())
}

/// Sum of the regular files directly inside `dir`.
fn dir_size(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

fn event_file_index(name: &str) -> Option<u64> {
    name.strip_prefix("events-")?
        .strip_suffix(".ndjson")?
        .parse()
        .ok()
}

fn existing_max_file_index(dir: &Path) -> Option<u64> {
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| event_file_index(&e.file_name().to_string_lossy()))
        .max()
}

/// `(path, length, modified)` of every `events-*.ndjson` file in `dir`.
fn event_files(dir: &Path) -> Vec<(PathBuf, u64, SystemTime)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| event_file_index(&e.file_name().to_string_lossy()).is_some())
        .filter_map(|e| {
            let meta = e.metadata().ok().filter(fs::Metadata::is_file)?;
            Some((e.path(), meta.len(), meta.modified().ok()?))
        })
        .collect()
}

fn crash_dumps_oldest_first(crashes: &Path) -> Vec<(PathBuf, u64)> {
    let Ok(entries) = fs::read_dir(crashes) else {
        return Vec::new();
    };
    let mut dumps: Vec<_> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "dmp"))
        .filter_map(|e| {
            let meta = e.metadata().ok().filter(fs::Metadata::is_file)?;
            Some((meta.modified().ok()?, e.path(), meta.len()))
        })
        .collect();
    dumps.sort();
    dumps
        .into_iter()
        .map(|(_, path, len)| (path, len))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config(dir: &std::path::Path) -> StoreConfig {
        StoreConfig {
            max_file_bytes: 300,
            flush_every: std::time::Duration::from_millis(10),
            ..StoreConfig::new(dir.to_path_buf())
        }
    }

    fn event(n: u64) -> ProfilerEvent {
        ProfilerEvent {
            kind: "test.event",
            source: "server".into(),
            payload: json!({ "n": n }),
        }
    }

    #[test]
    fn writes_ndjson_envelopes_and_rotates_by_size() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProfilerStore::start(config(dir.path()));
        for n in 0..20 {
            store.sender().send(event(n));
        }
        store.shutdown();
        let mut files: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.starts_with("events-"))
            .collect();
        files.sort();
        assert!(files.len() > 1, "300-byte cap must rotate: {files:?}");
        let first = std::fs::read_to_string(dir.path().join(&files[0])).unwrap();
        let line: serde_json::Value = serde_json::from_str(first.lines().next().unwrap()).unwrap();
        assert_eq!(line["v"], 1);
        assert_eq!(line["kind"], "test.event");
        assert_eq!(line["seq"], 0);
        assert_eq!(
            line["sid"].as_str().unwrap().len(),
            8,
            "every writer start has a session id"
        );
        assert!(line["ts"].as_str().unwrap().ends_with('Z'));
    }

    #[test]
    fn full_channel_drops_and_reports_a_dropped_event() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProfilerStore::start_paused(config(dir.path()), 4);
        for n in 0..10 {
            store.sender().send(event(n));
        }
        assert_eq!(store.stats().dropped, 6);
        store.resume();
        store.shutdown();
        let all = read_all(dir.path());
        assert!(
            all.iter()
                .any(|l| l["kind"] == "telemetry.dropped" && l["count"] == 6)
        );
    }

    #[test]
    fn unwritable_directory_marks_failing_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("profiler");
        std::fs::write(&blocker, b"not a dir").unwrap();
        let store = ProfilerStore::start(config(&blocker));
        store.sender().send(event(1));
        wait_until(|| store.stats().failing);
        store.shutdown();
    }

    #[test]
    fn retention_deletes_crash_dumps_when_events_alone_cannot_fit() {
        let dir = tempfile::tempdir().unwrap();
        let crashes = dir.path().join("crashes");
        std::fs::create_dir_all(&crashes).unwrap();
        std::fs::write(crashes.join("old.dmp"), vec![0u8; 800]).unwrap();
        std::fs::write(crashes.join("old.json"), b"{}").unwrap();
        let store = ProfilerStore::start(StoreConfig {
            max_total_bytes: 900,
            ..config(dir.path())
        });
        for n in 0..40 {
            store.sender().send(event(n));
        }
        store.shutdown();
        assert!(!crashes.join("old.dmp").exists());
        assert!(!crashes.join("old.json").exists());
    }

    #[test]
    fn retention_deletes_oldest_files_over_total_cap() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProfilerStore::start(StoreConfig {
            max_total_bytes: 900,
            ..config(dir.path())
        });
        for n in 0..200 {
            store.sender().send(event(n));
        }
        store.shutdown();
        let total: u64 = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().metadata().unwrap().len())
            .sum();
        assert!(
            total <= 900 + 300,
            "total {total} exceeds cap plus one live file"
        );
    }

    #[test]
    fn queued_findings_append_one_line_each_to_findings_ndjson() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProfilerStore::start(config(dir.path()));
        let sender = store.sender();
        sender.send_finding(json!({ "id": "a" }));
        sender.send_finding(json!({ "id": "b" }));
        store.shutdown();
        let text = std::fs::read_to_string(dir.path().join("findings.ndjson")).unwrap();
        let ids: Vec<_> = text
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["id"].clone())
            .collect();
        assert_eq!(ids, [json!("a"), json!("b")]);
    }

    #[test]
    fn payload_cannot_overwrite_envelope_fields() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProfilerStore::start(config(dir.path()));
        store.sender().send(ProfilerEvent {
            kind: "test.event",
            source: "server".into(),
            payload: json!({ "v": 7, "seq": 99, "kind": "forged", "source": "forged", "n": 1 }),
        });
        store.shutdown();
        let line = &read_all(dir.path())[0];
        assert_eq!(
            (line["v"].clone(), line["seq"].clone()),
            (json!(1), json!(0))
        );
        assert_eq!(
            (line["kind"].clone(), line["source"].clone()),
            (json!("test.event"), json!("server"))
        );
        assert_eq!(line["n"], 1);
    }

    #[test]
    fn failed_writes_count_as_drops_and_the_store_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("profiler");
        std::fs::write(&blocker, b"not a dir").unwrap();
        let store = ProfilerStore::start(config(&blocker));
        store.sender().send(event(1));
        wait_until(|| store.stats().failing);
        assert_eq!(store.stats().dropped, 1);

        std::fs::remove_file(&blocker).unwrap();
        store.sender().send(event(2));
        wait_until(|| !store.stats().failing);
        let stats = store.stats();
        assert_eq!((stats.written, stats.dropped), (1, 1));
        store.shutdown();
        let all = read_all(&blocker);
        assert!(all.iter().any(|l| l["n"] == 2));
        assert!(
            !all.iter().any(|l| l["kind"] == "telemetry.dropped"),
            "writer-side failures are not reported twice"
        );
    }

    #[test]
    fn retention_deletes_rotated_files_older_than_max_age() {
        let dir = tempfile::tempdir().unwrap();
        let stale = dir.path().join("events-000000.ndjson");
        std::fs::write(&stale, b"{}\n").unwrap();
        let ten_days = std::time::Duration::from_secs(10 * 24 * 3600);
        std::fs::OpenOptions::new()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - ten_days)
            .unwrap();
        let store = ProfilerStore::start(config(dir.path()));
        store.sender().send(event(1));
        store.shutdown();
        assert!(!stale.exists());
        assert!(
            dir.path().join("events-000001.ndjson").exists(),
            "numbering continues after restart"
        );
    }

    #[test]
    fn record_larger_than_a_file_is_dropped_and_reported() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProfilerStore::start(config(dir.path()));
        store.sender().send(ProfilerEvent {
            kind: "test.event",
            source: "server".into(),
            payload: json!({ "blob": "x".repeat(1000) }),
        });
        store.sender().send(event(1));
        store.shutdown();
        let all = read_all(dir.path());
        assert!(all.iter().all(|l| l.get("blob").is_none()));
        assert!(all.iter().any(|l| l["n"] == 1));
        assert!(
            all.iter()
                .any(|l| l["kind"] == "telemetry.dropped" && l["count"] == 1)
        );
    }

    #[test]
    fn finding_write_failures_count_as_drops_and_recover() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("profiler");
        std::fs::write(&blocker, b"not a dir").unwrap();
        let store = ProfilerStore::start(config(&blocker));
        store.sender().send_finding(json!({ "id": "lost" }));
        wait_until(|| store.stats().failing);
        assert_eq!(store.stats().dropped, 1);

        std::fs::remove_file(&blocker).unwrap();
        store.sender().send_finding(json!({ "id": "kept" }));
        wait_until(|| !store.stats().failing);
        store.shutdown();
        let text = std::fs::read_to_string(blocker.join("findings.ndjson")).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert!(text.contains("kept"));
    }

    fn wait_until(condition: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !condition() {
            assert!(
                std::time::Instant::now() < deadline,
                "condition not reached"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn read_all(dir: &std::path::Path) -> Vec<serde_json::Value> {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        names.sort();
        names
            .iter()
            .flat_map(|p| {
                std::fs::read_to_string(p)
                    .unwrap()
                    .lines()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .filter_map(|l| serde_json::from_str(&l).ok())
            .collect()
    }
}
