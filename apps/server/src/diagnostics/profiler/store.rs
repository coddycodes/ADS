use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Read, Seek, SeekFrom, Write},
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
/// The event that names the host, OS and architecture of a recording; see `SessionHeader`.
pub const SESSION_START: &str = "session.start";

pub(super) const FINDINGS_FILE: &str = "findings.ndjson";
const FINDINGS_TMP_FILE: &str = "findings.ndjson.tmp";
pub(super) const CRASHES_DIR: &str = "crashes";
const WRITE_BUFFER_BYTES: usize = 64 * 1024;

/// Everything the profiler directory may hold: events, crash dumps and findings.
pub const MAX_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
/// The share of `MAX_TOTAL_BYTES` crash dumps may take. Events never push dumps out, so a busy
/// client cannot evict the crash evidence; dumps leave by age, or beyond this share.
pub const MAX_DUMP_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct StoreConfig {
    pub dir: PathBuf,
    pub max_file_bytes: u64,
    pub max_total_bytes: u64,
    pub max_dump_bytes: u64,
    pub max_age: Duration,
    pub flush_every: Duration,
}

impl StoreConfig {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            max_file_bytes: 10 * 1024 * 1024,
            max_total_bytes: MAX_TOTAL_BYTES,
            max_dump_bytes: MAX_DUMP_BYTES,
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

struct Counters {
    written: AtomicU64,
    dropped: AtomicU64,
    failing: AtomicBool,
    /// Names one store session (one process run); the envelope's `sid` is its 8 hex digits.
    sid: u32,
    /// The next `eventId` number. Senders take theirs before queueing, so a caller knows its
    /// event's id without waiting for the writer, which assigns `seq`.
    next_event: AtomicU64,
}

impl Default for Counters {
    fn default() -> Self {
        Self {
            written: AtomicU64::default(),
            dropped: AtomicU64::default(),
            failing: AtomicBool::default(),
            sid: uuid::Uuid::new_v4().as_u128() as u32,
            next_event: AtomicU64::default(),
        }
    }
}

/// A queued event's `eventId`. It is formatted only where a finding cites it, as
/// `"<sid>-<n>"`, so a sender that ignores it pays for no string.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EventId {
    sid: u32,
    number: u64,
}

impl EventId {
    #[cfg(test)]
    pub(crate) fn new(sid: u32, number: u64) -> Self {
        Self { sid, number }
    }
}

impl std::fmt::Display for EventId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:08x}-{}", self.sid, self.number)
    }
}

enum Message {
    /// An event and the number of its `eventId`.
    Event(u64, ProfilerEvent),
    /// One finding line for `findings.ndjson`. Appended through its own short-lived
    /// handle; a failed append counts as a drop and sets `failing`.
    Finding(Value),
    /// Runs retention outside rotation, never touching the live file.
    Maintain,
    /// Atomically replaces `findings.ndjson` with exactly these lines. Handled in queue order,
    /// so findings queued after it are appended to the new file.
    ReplaceFindings(Vec<Value>),
    /// Flushes the live file now, then answers.
    Flush(SyncSender<()>),
    Shutdown,
}

#[derive(Clone)]
pub struct EventSender {
    tx: SyncSender<Message>,
    counters: Arc<Counters>,
}

impl EventSender {
    /// Queues `event` and returns its `eventId`, which a finding can cite before the writer
    /// has written it. Never blocks; a full channel drops the event, counts it and returns
    /// `None`.
    pub fn send(&self, event: ProfilerEvent) -> Option<EventId> {
        let id = EventId {
            sid: self.counters.sid,
            number: self.counters.next_event.fetch_add(1, Ordering::Relaxed),
        };
        self.enqueue(Message::Event(id.number, event)).then_some(id)
    }

    /// Queues one finding for `findings.ndjson`. Never blocks; a full channel drops it.
    pub fn send_finding(&self, finding: Value) {
        self.enqueue(Message::Finding(finding));
    }

    /// Queues a retention pass on the writer thread. Never blocks.
    pub fn maintain(&self) {
        self.enqueue(Message::Maintain);
    }

    /// Hands everything queued so far to the OS before returning, or gives up after
    /// `timeout` (a full channel, or a writer that is stuck or gone). It blocks, so only the
    /// panic hook uses it: a panic may abort the process right after.
    pub fn flush_within(&self, timeout: Duration) -> bool {
        let (ack, flushed) = mpsc::sync_channel(1);
        self.tx.try_send(Message::Flush(ack)).is_ok() && flushed.recv_timeout(timeout).is_ok()
    }

    /// Queues a rewrite of `findings.ndjson` with exactly `lines`. Never blocks.
    pub fn replace_findings(&self, lines: Vec<Value>) {
        self.enqueue(Message::ReplaceFindings(lines));
    }

    /// Returns whether `message` was queued.
    fn enqueue(&self, message: Message) -> bool {
        let queued = self.tx.try_send(message).is_ok();
        if !queued {
            self.counters.dropped.fetch_add(1, Ordering::Relaxed);
        }
        queued
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
            .name("coddy-studio-profiler-writer".into())
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
    /// The open handle for the live events file and its length in bytes.
    out: Option<(BufWriter<File>, u64)>,
    /// The live events file. It survives I/O failures so the handle is reopened on the same
    /// path, and is cleared only by rotation, so a failing disk never multiplies files.
    live_path: Option<PathBuf>,
    /// Length of `live_path` as of the last successful flush; bytes beyond it are uncommitted.
    committed: u64,
    next_file: u64,
    seq: u64,
    /// Event lines accepted into `out`'s buffer and not yet flushed to the OS.
    pending: u64,
    /// Drops already confirmed in a flushed `telemetry.dropped` line.
    reported_drops: u64,
    /// Drops carried by `telemetry.dropped` lines that are buffered but not yet flushed.
    in_flight: u64,
}

impl Writer {
    fn new(config: StoreConfig, counters: Arc<Counters>) -> Self {
        let next_file = existing_max_file_index(&config.dir).map_or(0, |n| n + 1);
        Self {
            config,
            counters,
            out: None,
            live_path: None,
            committed: 0,
            next_file,
            seq: 0,
            pending: 0,
            reported_drops: 0,
            in_flight: 0,
        }
    }

    fn run(mut self, rx: Receiver<Message>) {
        let mut last_flush = Instant::now();
        loop {
            // Idle, it sleeps until a message arrives: the periodic flush runs only while there
            // is something to flush.
            let received = if self.has_pending() {
                rx.recv_timeout(self.config.flush_every)
            } else {
                rx.recv().map_err(|_| RecvTimeoutError::Disconnected)
            };
            match received {
                Ok(Message::Event(number, event)) => self.write(number, event),
                Ok(Message::Finding(finding)) => self.append_finding(&finding),
                Ok(Message::Maintain) => self.enforce_retention(),
                Ok(Message::ReplaceFindings(lines)) => self.replace_findings(&lines),
                Ok(Message::Flush(flushed)) => {
                    self.flush();
                    let _ = flushed.send(());
                }
                Ok(Message::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
            if last_flush.elapsed() >= self.config.flush_every {
                self.report_drops();
                self.flush();
                last_flush = Instant::now();
            }
        }
        self.finish();
    }

    /// Whether the periodic flush has work: lines not yet committed (still buffered, or a line
    /// larger than the buffer written past it), or drops not yet reported.
    fn has_pending(&self) -> bool {
        self.out
            .as_ref()
            .is_some_and(|(out, len)| !out.buffer().is_empty() || *len > self.committed)
            || self.counters.dropped.load(Ordering::Relaxed) > self.reported_drops + self.in_flight
    }

    /// Final report and close. Closing can itself lose lines (a failed rotation or last
    /// flush), so whatever it added is reported once more before the writer exits.
    fn finish(&mut self) {
        for _ in 0..2 {
            self.report_drops();
            self.close();
        }
    }

    /// Writes the drops not yet in the file as one `telemetry.dropped` line. They count as
    /// reported only once that line is flushed, so a line lost to an I/O error is sent again
    /// and the file's counts add up to `stats().dropped` after an outage.
    fn report_drops(&mut self) {
        let dropped = self.counters.dropped.load(Ordering::Relaxed);
        let count = dropped.saturating_sub(self.reported_drops + self.in_flight);
        if count > 0 {
            let mut line = Map::new();
            line.insert("kind".into(), "telemetry.dropped".into());
            line.insert("source".into(), "server".into());
            line.insert("count".into(), count.into());
            let number = self.counters.next_event.fetch_add(1, Ordering::Relaxed);
            if self.write_line(number, line, false) {
                self.in_flight += count;
            }
        }
    }

    fn write(&mut self, number: u64, event: ProfilerEvent) {
        let mut line = match event.payload {
            Value::Object(fields) => fields,
            Value::Null => Map::new(),
            other => Map::from_iter([("payload".to_owned(), other)]),
        };
        // Envelope fields are inserted last so a payload can never overwrite them.
        line.insert("kind".into(), event.kind.into());
        line.insert("source".into(), event.source.into());
        self.write_line(number, line, true);
    }

    /// Returns whether the line reached the buffer. Only `is_event` lines (records from
    /// senders) count as drops when lost; the writer's own `telemetry.dropped` lines do not.
    /// `sid` makes `"<sid>:<seq>"` unique across restarts, as `seq` restarts at 0 with each
    /// writer session. `eventId` (`"<sid>-<number>"`) is unique too, and known to the sender.
    fn write_line(&mut self, number: u64, mut line: Map<String, Value>, is_event: bool) -> bool {
        line.insert("v".into(), 1.into());
        // Every line names its build, so no retention or failed write can part events from it
        // (#98).
        line.insert("coddy-studio".into(), env!("CARGO_PKG_VERSION").into());
        line.insert("seq".into(), self.seq.into());
        let id = EventId {
            sid: self.counters.sid,
            number,
        };
        line.insert("sid".into(), format!("{:08x}", id.sid).into());
        line.insert("eventId".into(), id.to_string().into());
        line.insert("ts".into(), now_rfc3339().into());
        self.seq += 1;
        let Ok(mut bytes) = serde_json::to_vec(&line) else {
            return false;
        };
        bytes.push(b'\n');
        if bytes.len() as u64 > self.config.max_file_bytes {
            // One record bigger than a whole file would defeat rotation and retention.
            // Its `seq` is skipped, and the drop is reported like any other.
            if is_event {
                self.counters.dropped.fetch_add(1, Ordering::Relaxed);
            }
            return false;
        }
        match self.append(&bytes) {
            Ok(()) => {
                self.pending += u64::from(is_event);
                true
            }
            Err(_) => {
                self.fail_events(u64::from(is_event));
                false
            }
        }
    }

    fn append(&mut self, bytes: &[u8]) -> io::Result<()> {
        let max = self.config.max_file_bytes;
        // Normally one pass. A file is checked after any reopen, so one a failed flush left
        // near the cap still rotates. If rotation's final flush fails, `close` keeps the file,
        // the next pass reopens it (cut back to the last flush, empty buffer) and closes it
        // cleanly. The third pass opens the new file, which is empty and so never rotates.
        for _ in 0..3 {
            if self.out.is_none() {
                self.out = Some(self.open_live()?);
            }
            let full = self
                .out
                .as_ref()
                .is_some_and(|(_, len)| *len > 0 && *len + bytes.len() as u64 > max);
            if full {
                self.close();
            } else if let Some((out, len)) = self.out.as_mut() {
                out.write_all(bytes)?;
                *len += bytes.len() as u64;
                return Ok(());
            }
        }
        Err(io::Error::other("could not rotate the live events file"))
    }

    /// Opens the live events file. A new file (first start or after rotation) runs retention
    /// first so its space is already free. After an I/O failure the same file is reopened in
    /// append mode, with no directory scan and no new file, once the bytes beyond the last
    /// successful flush are cut off: those lines were counted as dropped, and a torn tail
    /// would corrupt the next record or double-count a persisted `telemetry.dropped`. If the
    /// cut-off fails, the reopen fails too and is retried with the next record.
    fn open_live(&mut self) -> io::Result<(BufWriter<File>, u64)> {
        fs::create_dir_all(&self.config.dir)?;
        let path = match &self.live_path {
            Some(path) => {
                let path = path.clone();
                self.discard_uncommitted(&path)?;
                path
            }
            None => {
                self.enforce_retention();
                let path = self
                    .config
                    .dir
                    .join(format!("events-{:06}.ndjson", self.next_file));
                self.next_file += 1;
                self.live_path = Some(path.clone());
                self.committed = 0;
                path
            }
        };
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let len = file.metadata().map_or(0, |m| m.len());
        Ok((BufWriter::with_capacity(WRITE_BUFFER_BYTES, file), len))
    }

    /// Cuts the live file back to its last successfully flushed length. A file that was
    /// deleted meanwhile is simply recreated empty.
    fn discard_uncommitted(&mut self, path: &Path) -> io::Result<()> {
        match OpenOptions::new().write(true).open(path) {
            Ok(file) if file.metadata()?.len() > self.committed => file.set_len(self.committed),
            Ok(_) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.committed = 0;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    /// Appends one line. A file whose last line is torn (a full disk cut a write short, or a
    /// crash did) gets a newline first, so the fragment stays a line of its own that readers
    /// skip instead of corrupting this one; a write that fails is cut back off the file.
    fn append_finding(&mut self, finding: &Value) {
        let appended = serde_json::to_vec(finding)
            .map_err(io::Error::from)
            .and_then(|mut bytes| {
                bytes.push(b'\n');
                fs::create_dir_all(&self.config.dir)?;
                let mut file = OpenOptions::new()
                    .create(true)
                    .read(true)
                    .append(true)
                    .open(self.config.dir.join(FINDINGS_FILE))?;
                let committed = file.metadata()?.len();
                if committed > 0 {
                    let mut last = [0u8];
                    file.seek(SeekFrom::End(-1))?;
                    file.read_exact(&mut last)?;
                    if last[0] != b'\n' {
                        bytes.insert(0, b'\n');
                    }
                }
                file.write_all(&bytes).inspect_err(|_| {
                    let _ = file.set_len(committed);
                })
            });
        if appended.is_err() {
            self.count_lost(1);
        } else {
            // The directory accepted a write, so an earlier failure has recovered. An events
            // failure sets the flag again on its next failed write.
            self.counters.failing.store(false, Ordering::Relaxed);
        }
    }

    fn replace_findings(&mut self, lines: &[Value]) {
        if replace_findings_file(&self.config.dir, lines).is_ok() {
            self.counters.failing.store(false, Ordering::Relaxed);
        } else {
            // The old file stays in place, so nothing is lost; storage is still failing.
            self.count_lost(0);
        }
    }

    fn flush(&mut self) {
        let Some((out, len)) = self.out.as_mut() else {
            return;
        };
        match out.flush() {
            Ok(()) => {
                self.committed = *len;
                self.commit();
            }
            Err(_) => self.fail_events(0),
        }
    }

    /// Flushes and syncs the live file, then retires it so the next record opens a new one.
    /// Used on rotation and shutdown only. If the flush fails the file stays live: it is cut
    /// back to its last good length and reused, not left torn behind a new file.
    fn close(&mut self) {
        let Some((mut out, len)) = self.out.take() else {
            self.live_path = None;
            return;
        };
        match out.flush() {
            Ok(()) => {
                let _ = out.get_ref().sync_data();
                self.live_path = None;
                self.commit();
            }
            Err(_) => {
                self.out = Some((out, len));
                self.fail_events(0);
            }
        }
    }

    fn commit(&mut self) {
        if self.pending > 0 || self.in_flight > 0 {
            self.counters
                .written
                .fetch_add(self.pending, Ordering::Relaxed);
            self.pending = 0;
            self.reported_drops += self.in_flight;
            self.in_flight = 0;
            self.counters.failing.store(false, Ordering::Relaxed);
        }
    }

    /// An I/O error loses the buffered lines (and `extra` more). Event lines count as drops;
    /// a lost `telemetry.dropped` line is simply sent again. The handle is abandoned without
    /// a second flush, and the file is cut back to its last good length now (best effort) and
    /// again when the next record reopens it, so no part of a lost line stays behind.
    fn fail_events(&mut self, extra: u64) {
        let lost = self.pending + extra;
        self.pending = 0;
        self.in_flight = 0;
        if let Some((out, _)) = self.out.take() {
            let _ = out.into_parts();
        }
        if let Some(path) = self.live_path.clone() {
            let _ = self.discard_uncommitted(&path);
        }
        self.count_lost(lost);
    }

    fn count_lost(&mut self, lost: u64) {
        self.counters.failing.store(true, Ordering::Relaxed);
        self.counters.dropped.fetch_add(lost, Ordering::Relaxed);
    }

    /// Retention for the whole profiler directory, run before every new events file and on
    /// `Message::Maintain`. Rotated `events-*` files and crash dumps (with their `.json`
    /// sidecars, or a sidecar alone once its dump is gone), each oldest modification time
    /// first:
    /// 1. anything older than `max_age` is deleted;
    /// 2. while the dumps alone exceed `max_dump_bytes`, the oldest dump is deleted;
    /// 3. while the total (events + `crashes/` + `findings.ndjson`) plus room for the new live
    ///    file exceeds `max_total_bytes`, the oldest events file is deleted. Dumps are never
    ///    deleted to make room for events, so a busy client cannot evict crash evidence.
    ///
    /// `findings.ndjson` counts toward the total but is not trimmed here; whoever appends
    /// findings owns compacting that file. An open live file counts toward the total but is
    /// never deleted.
    fn enforce_retention(&self) {
        let dir = &self.config.dir;
        let crashes = dir.join(CRASHES_DIR);
        let events = event_files(dir);
        let mut total = events.iter().map(|f| f.len).sum::<u64>()
            + dir_size(&crashes)
            + file_len(&dir.join(FINDINGS_FILE));
        let mut events: Vec<Retained> = events
            .into_iter()
            .filter(|file| self.live_path.as_ref() != Some(&file.path))
            .collect();
        let mut dumps = crash_dumps(&crashes);
        let now = SystemTime::now();
        for files in [&mut events, &mut dumps] {
            files.sort_by(|a, b| (a.modified, &a.path).cmp(&(b.modified, &b.path)));
            files.retain(|file| {
                let expired = now
                    .duration_since(file.modified)
                    .is_ok_and(|age| age > self.config.max_age);
                if expired {
                    total = total.saturating_sub(file.remove());
                }
                !expired
            });
        }
        let mut dump_total: u64 = dumps.iter().map(Retained::size).sum();
        for dump in &dumps {
            if dump_total <= self.config.max_dump_bytes {
                break;
            }
            let freed = dump.remove();
            dump_total = dump_total.saturating_sub(freed);
            total = total.saturating_sub(freed);
        }
        // The live file about to be opened may grow to `max_file_bytes`.
        let budget = self
            .config
            .max_total_bytes
            .saturating_sub(self.config.max_file_bytes);
        for file in &events {
            if total <= budget {
                break;
            }
            total = total.saturating_sub(file.remove());
        }
    }
}

/// A file retention may delete: a rotated events file, or a crash dump with its sidecar.
struct Retained {
    modified: SystemTime,
    path: PathBuf,
    len: u64,
    sidecar: Option<(PathBuf, u64)>,
}

impl Retained {
    /// The bytes it holds, its sidecar included.
    fn size(&self) -> u64 {
        self.len + self.sidecar.as_ref().map_or(0, |(_, len)| *len)
    }

    /// Deletes the file, then its sidecar, so a failed delete never leaves a dump without its
    /// sidecar. Returns the bytes freed.
    fn remove(&self) -> u64 {
        if fs::remove_file(&self.path).is_err() {
            return 0;
        }
        let sidecar = match &self.sidecar {
            Some((path, len)) if fs::remove_file(path).is_ok() => *len,
            _ => 0,
        };
        self.len + sidecar
    }
}

/// Replaces `findings.ndjson` with `lines` through a synced temp file and a rename. On
/// failure the old file stays in place and the temp file is removed.
pub(super) fn replace_findings_file(dir: &Path, lines: &[Value]) -> io::Result<()> {
    let tmp = dir.join(FINDINGS_TMP_FILE);
    let renamed = write_lines(&tmp, lines).and_then(|()| fs::rename(&tmp, dir.join(FINDINGS_FILE)));
    if renamed.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    renamed
}

/// Writes `lines` as NDJSON and syncs the file.
pub(super) fn write_lines(path: &Path, lines: &[Value]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut out = BufWriter::new(File::create(path)?);
    for line in lines {
        serde_json::to_writer(&mut out, line)?;
        out.write_all(b"\n")?;
    }
    out.into_inner()
        .map_err(io::IntoInnerError::into_error)?
        .sync_data()
}

/// Bytes the profiler uses on disk: the directory's own files plus crash dumps.
pub(super) fn disk_bytes(dir: &Path) -> u64 {
    dir_size(dir) + dir_size(&dir.join(CRASHES_DIR))
}

pub(super) fn now_rfc3339() -> String {
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

pub(super) fn event_file_index(name: &str) -> Option<u64> {
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

/// Every rotated `events-*.ndjson` file in `dir`.
fn event_files(dir: &Path) -> Vec<Retained> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| event_file_index(&e.file_name().to_string_lossy()).is_some())
        .filter_map(|e| {
            let meta = e.metadata().ok().filter(fs::Metadata::is_file)?;
            Some(Retained {
                modified: meta.modified().ok()?,
                path: e.path(),
                len: meta.len(),
                sidecar: None,
            })
        })
        .collect()
}

/// Every `*.dmp` in `crashes` with its `.json` sidecar when one exists, plus any `*.json`
/// whose dump is gone, which would otherwise count toward the total and never be deleted.
fn crash_dumps(crashes: &Path) -> Vec<Retained> {
    let Ok(entries) = fs::read_dir(crashes) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let meta = e.metadata().ok().filter(fs::Metadata::is_file)?;
            let modified = meta.modified().ok()?;
            match path.extension()?.to_str()? {
                "dmp" => {
                    let sidecar = path.with_extension("json");
                    let sidecar_len = fs::metadata(&sidecar)
                        .ok()
                        .filter(fs::Metadata::is_file)
                        .map(|m| m.len());
                    Some(Retained {
                        modified,
                        len: meta.len(),
                        sidecar: sidecar_len.map(|len| (sidecar, len)),
                        path,
                    })
                }
                "json" if !path.with_extension("dmp").exists() => Some(Retained {
                    modified,
                    len: meta.len(),
                    sidecar: None,
                    path,
                }),
                _ => None,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn config(dir: &std::path::Path) -> StoreConfig {
        StoreConfig {
            max_file_bytes: 340,
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
        assert!(files.len() > 1, "340-byte cap must rotate: {files:?}");
        let first = std::fs::read_to_string(dir.path().join(&files[0])).unwrap();
        let line: serde_json::Value = serde_json::from_str(first.lines().next().unwrap()).unwrap();
        assert_eq!(line["v"], 1);
        assert_eq!(line["coddy-studio"], env!("CARGO_PKG_VERSION"));
        assert_eq!(line["kind"], "test.event");
        assert_eq!(line["seq"], 0);
        assert_eq!(
            line["sid"].as_str().unwrap().len(),
            8,
            "every writer start has a session id"
        );
        assert!(line["ts"].as_str().unwrap().ends_with('Z'));
    }

    /// A panic that aborts the process right after the hook must already be on disk.
    #[test]
    fn flush_within_writes_queued_events_before_returning() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProfilerStore::start(StoreConfig::new(dir.path().to_path_buf()));
        store.sender().send(event(7));
        assert!(store.sender().flush_within(Duration::from_secs(5)));
        let live = std::fs::read_to_string(dir.path().join("events-000000.ndjson")).unwrap();
        assert!(live.contains("test.event"), "{live}");
        store.shutdown();
        // A stopped writer never answers: the caller gives up instead of hanging.
        let (tx, _rx) = mpsc::sync_channel(1);
        let orphan = EventSender {
            tx,
            counters: Arc::default(),
        };
        assert!(!orphan.flush_within(Duration::from_millis(10)));
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

    /// Events, a client's included, never push a dump out: dumps leave by age or by their
    /// own share only.
    #[test]
    fn retention_never_deletes_a_dump_to_make_room_for_events() {
        let dir = tempfile::tempdir().unwrap();
        let crashes = dir.path().join("crashes");
        std::fs::create_dir_all(&crashes).unwrap();
        std::fs::write(crashes.join("old.dmp"), vec![0u8; 800]).unwrap();
        std::fs::write(crashes.join("old.json"), b"{}").unwrap();
        age(&crashes.join("old.dmp"), 3600);
        let store = ProfilerStore::start(StoreConfig {
            max_total_bytes: 900,
            max_dump_bytes: 850,
            ..config(dir.path())
        });
        for n in 0..40 {
            store.sender().send(event(n));
        }
        store.shutdown();
        assert!(crashes.join("old.dmp").exists());
        assert!(crashes.join("old.json").exists());
        let rotated = std::fs::read_dir(dir.path())
            .unwrap()
            .filter(|e| {
                let name = e.as_ref().unwrap().file_name();
                event_file_index(&name.to_string_lossy()).is_some()
            })
            .count();
        assert_eq!(rotated, 1, "only the live events file is left");
    }

    #[test]
    fn retention_deletes_the_oldest_dumps_over_their_share() {
        let dir = tempfile::tempdir().unwrap();
        let crashes = dir.path().join("crashes");
        std::fs::create_dir_all(&crashes).unwrap();
        for (name, hours) in [("a", 3), ("b", 2), ("c", 1)] {
            std::fs::write(crashes.join(format!("{name}.dmp")), vec![0u8; 300]).unwrap();
            std::fs::write(crashes.join(format!("{name}.json")), b"{}").unwrap();
            age(&crashes.join(format!("{name}.dmp")), hours * 3600);
        }
        // 906 bytes of dumps against a 700-byte share: the oldest goes, sidecar after dump.
        let store = ProfilerStore::start(StoreConfig {
            max_total_bytes: 1 << 20,
            max_dump_bytes: 700,
            ..config(dir.path())
        });
        store.sender().send(event(1));
        store.shutdown();
        assert!(!crashes.join("a.dmp").exists());
        assert!(!crashes.join("a.json").exists());
        for name in ["b.dmp", "b.json", "c.dmp", "c.json"] {
            assert!(crashes.join(name).exists(), "{name}");
        }
    }

    /// A finding line torn by a full disk (or a crash) must not merge with the next one: the
    /// next line starts on a line of its own, and readers skip the fragment.
    #[test]
    fn a_torn_findings_line_never_merges_with_the_next_one() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(FINDINGS_FILE), b"{\"id\":\"torn").unwrap();
        let store = ProfilerStore::start(config(dir.path()));
        store.sender().send_finding(json!({ "id": "whole" }));
        store.shutdown();
        let text = std::fs::read_to_string(dir.path().join(FINDINGS_FILE)).unwrap();
        assert_eq!(text, "{\"id\":\"torn\n{\"id\":\"whole\"}\n");
    }

    /// With nothing buffered the writer has no reason to wake every 2 s.
    #[test]
    fn the_writer_waits_for_work_only_while_something_is_buffered() {
        let dir = tempfile::tempdir().unwrap();
        let counters = Arc::new(Counters::default());
        let mut writer = Writer::new(config(dir.path()), counters.clone());
        assert!(!writer.has_pending(), "idle");
        writer.write(0, event(0));
        assert!(writer.has_pending(), "a line in the buffer");
        writer.flush();
        assert!(!writer.has_pending(), "flushed");
        counters.dropped.fetch_add(1, Ordering::Relaxed);
        assert!(writer.has_pending(), "a drop to report");
        writer.report_drops();
        writer.flush();
        assert!(!writer.has_pending(), "reported");
        // A line larger than the buffer goes past it to the file, still uncommitted.
        let mut writer = Writer::new(
            StoreConfig {
                max_file_bytes: 1 << 20,
                ..config(dir.path())
            },
            Arc::new(Counters::default()),
        );
        writer.write(
            1,
            ProfilerEvent {
                kind: "test.event",
                source: "server".into(),
                payload: json!({ "big": "x".repeat(WRITE_BUFFER_BYTES + 1) }),
            },
        );
        assert!(writer.has_pending(), "an uncommitted line past the buffer");
        writer.flush();
        assert!(!writer.has_pending());
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
        let id = store.sender().send(ProfilerEvent {
            kind: "test.event",
            source: "server".into(),
            payload: json!({
                "v": 7, "seq": 99, "sid": "forged", "eventId": "forged",
                "kind": "forged", "source": "forged", "n": 1
            }),
        });
        store.shutdown();
        let line = &read_all(dir.path())[0];
        assert_eq!(
            (line["v"].clone(), line["seq"].clone()),
            (json!(1), json!(0))
        );
        assert_ne!(line["sid"], "forged");
        assert_eq!(
            line["eventId"].as_str(),
            id.map(|id| id.to_string()).as_deref()
        );
        assert_eq!(
            (line["kind"].clone(), line["source"].clone()),
            (json!("test.event"), json!("server"))
        );
        assert_eq!(line["n"], 1);
    }

    /// A finding cites its event by the id `send` returns, before the writer has assigned
    /// the event's `seq`.
    #[test]
    fn send_returns_the_event_id_the_line_carries() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProfilerStore::start_paused(config(dir.path()), 2);
        let sender = store.sender();
        let ids: Vec<Option<String>> = (0..3)
            .map(|n| sender.send(event(n)).map(|id| id.to_string()))
            .collect();
        assert!(ids[0].is_some() && ids[1].is_some() && ids[0] != ids[1]);
        assert_eq!(ids[2], None, "a dropped event has no id to cite");
        store.resume();
        store.shutdown();
        let lines = read_all(dir.path());
        let sid = lines[0]["sid"].as_str().unwrap();
        for (n, id) in ids.iter().take(2).enumerate() {
            let line = lines.iter().find(|line| line["n"] == n).unwrap();
            assert_eq!(line["eventId"].as_str(), id.as_deref());
            assert!(id.as_deref().unwrap().starts_with(&format!("{sid}-")));
        }
        assert!(
            lines.iter().all(|line| line["eventId"].is_string()),
            "the writer's own telemetry.dropped line has one too"
        );
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
        // The first good append clears `failing`; `written` only counts at the next flush.
        wait_until(|| {
            let stats = store.stats();
            !stats.failing && stats.written == 1
        });
        let stats = store.stats();
        assert_eq!((stats.written, stats.dropped), (1, 1));
        store.shutdown();
        assert!(read_all(&blocker).iter().any(|l| l["n"] == 2));
        assert_eq!(
            reported_drops(&blocker),
            stats.dropped,
            "drops caused by the outage reach the file once storage recovers"
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
        assert_eq!(reported_drops(&blocker), 1, "the lost finding is reported");
    }

    #[test]
    fn retention_deletes_expired_crash_dumps_even_under_the_size_budget() {
        let dir = tempfile::tempdir().unwrap();
        let crashes = dir.path().join("crashes");
        std::fs::create_dir_all(&crashes).unwrap();
        for name in ["old.dmp", "old.json", "new.dmp", "new.json"] {
            std::fs::write(crashes.join(name), b"x").unwrap();
        }
        for name in ["old.dmp", "old.json"] {
            age(&crashes.join(name), 10 * 24 * 3600);
        }
        let store = ProfilerStore::start(StoreConfig {
            max_total_bytes: 1 << 20,
            ..config(dir.path())
        });
        store.sender().send(event(1));
        store.shutdown();
        assert!(!crashes.join("old.dmp").exists());
        assert!(!crashes.join("old.json").exists());
        assert!(crashes.join("new.dmp").exists());
        assert!(crashes.join("new.json").exists());
    }

    #[test]
    fn retention_deletes_events_before_an_older_dump() {
        let dir = tempfile::tempdir().unwrap();
        let crashes = dir.path().join("crashes");
        std::fs::create_dir_all(&crashes).unwrap();
        std::fs::write(crashes.join("a.dmp"), vec![0u8; 300]).unwrap();
        std::fs::write(crashes.join("b.dmp"), vec![0u8; 150]).unwrap();
        std::fs::write(dir.path().join("events-000000.ndjson"), vec![b'\n'; 200]).unwrap();
        age(&crashes.join("a.dmp"), 3 * 3600);
        age(&dir.path().join("events-000000.ndjson"), 2 * 3600);
        age(&crashes.join("b.dmp"), 3600);
        // Budget is 700 - 300 = 400 and the directory holds 650: the events file goes, though
        // a dump is older.
        let store = ProfilerStore::start(StoreConfig {
            max_total_bytes: 700,
            ..config(dir.path())
        });
        store.sender().send(event(1));
        store.shutdown();
        assert!(!dir.path().join("events-000000.ndjson").exists());
        assert!(crashes.join("a.dmp").exists(), "an older dump stays");
        assert!(crashes.join("b.dmp").exists());
    }

    #[cfg(target_os = "linux")]
    fn full_disk_handle() -> (BufWriter<File>, u64) {
        let file = OpenOptions::new().write(true).open("/dev/full").unwrap();
        (BufWriter::new(file), 0)
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn repeated_flush_failures_keep_one_events_file() {
        let dir = tempfile::tempdir().unwrap();
        let counters = Arc::new(Counters::default());
        let mut writer = Writer::new(config(dir.path()), counters.clone());
        for n in 0..5 {
            writer.write(0, event(n)); // reopens the live file after the previous failure
            writer.out = Some(full_disk_handle()); // the next flush hits ENOSPC
            writer.write(0, event(n));
            writer.flush();
            assert!(writer.out.is_none(), "a failed flush abandons the handle");
        }
        assert!(counters.failing.load(Ordering::Relaxed));
        assert!(counters.dropped.load(Ordering::Relaxed) >= 5);
        let files = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(
            files, 1,
            "failures must not create a new events file each time"
        );
        let text = std::fs::read_to_string(dir.path().join("events-000000.ndjson")).unwrap();
        assert!(
            text.lines()
                .filter(|l| !l.is_empty())
                .all(|l| serde_json::from_str::<serde_json::Value>(l).is_ok())
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn telemetry_report_lost_in_a_failed_flush_is_sent_again() {
        let dir = tempfile::tempdir().unwrap();
        let counters = Arc::new(Counters::default());
        let mut writer = Writer::new(config(dir.path()), counters.clone());
        counters.dropped.store(3, Ordering::Relaxed);
        writer.out = Some(full_disk_handle());
        writer.report_drops();
        writer.flush();
        assert!(writer.out.is_none() && counters.failing.load(Ordering::Relaxed));
        assert_eq!(
            counters.dropped.load(Ordering::Relaxed),
            3,
            "a lost report is not a drop"
        );

        writer.report_drops();
        writer.flush();
        assert_eq!(reported_drops(dir.path()), 3);
        assert!(!counters.failing.load(Ordering::Relaxed));
    }

    #[test]
    fn reopened_file_that_is_already_full_rotates_before_the_next_record() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = Writer::new(config(dir.path()), Arc::new(Counters::default()));
        writer.write(0, event(0));
        writer.write(0, event(1));
        writer.flush();
        let live = dir.path().join("events-000000.ndjson");
        let before = std::fs::metadata(&live).unwrap().len();
        assert!(before > 200, "two events nearly fill the 340-byte cap");
        writer.out = None; // as after a failed flush: the handle is gone, the file is not

        writer.write(0, event(2));
        writer.flush();
        assert_eq!(std::fs::metadata(&live).unwrap().len(), before);
        assert!(dir.path().join("events-000001.ndjson").exists());
    }

    #[test]
    fn reopening_discards_bytes_a_failed_flush_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = Writer::new(config(dir.path()), Arc::new(Counters::default()));
        writer.write(0, event(0));
        writer.flush();
        let live = dir.path().join("events-000000.ndjson");
        // A complete JSON object with no newline: the failed flush died on the last byte.
        let mut file = OpenOptions::new().append(true).open(&live).unwrap();
        file.write_all(br#"{"kind":"telemetry.dropped","count":9}"#)
            .unwrap();
        writer.out = None; // the handle was abandoned without cutting the file back

        writer.write(0, event(1));
        writer.flush();
        let text = std::fs::read_to_string(&live).unwrap();
        assert!(
            !text.contains("telemetry.dropped"),
            "uncommitted bytes were kept: {text}"
        );
        assert_eq!(text.lines().count(), 2);
        assert!(text.ends_with('\n') && !text.contains("\n\n"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn final_report_includes_lines_lost_while_closing() {
        let dir = tempfile::tempdir().unwrap();
        let counters = Arc::new(Counters::default());
        let mut writer = Writer::new(config(dir.path()), counters.clone());
        writer.out = Some(full_disk_handle());
        writer.write(0, event(0)); // buffered for /dev/full, so closing the file will fail
        if let Some((_, len)) = writer.out.as_mut() {
            *len = 290; // the report line below then rotates, which closes this file
        }
        counters.dropped.fetch_add(1, Ordering::Relaxed);

        writer.finish();
        assert_eq!(counters.dropped.load(Ordering::Relaxed), 2);
        assert_eq!(
            reported_drops(dir.path()),
            2,
            "the line lost during rotation is reported too"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn failed_close_keeps_the_live_file_and_cuts_it_back() {
        let dir = tempfile::tempdir().unwrap();
        let counters = Arc::new(Counters::default());
        let mut writer = Writer::new(config(dir.path()), counters.clone());
        writer.write(0, event(0));
        writer.flush();
        let live = dir.path().join("events-000000.ndjson");
        let committed = std::fs::metadata(&live).unwrap().len();
        // A flush that ran out of disk halfway left a whole report line behind.
        let mut file = OpenOptions::new().append(true).open(&live).unwrap();
        file.write_all(b"{\"kind\":\"telemetry.dropped\",\"count\":9}\n")
            .unwrap();
        writer.out = Some(full_disk_handle());
        writer.write(0, event(1)); // buffered for /dev/full, so closing fails

        writer.close();
        assert_eq!(writer.live_path.as_deref(), Some(live.as_path()));
        assert_eq!(std::fs::metadata(&live).unwrap().len(), committed);
        assert_eq!(counters.dropped.load(Ordering::Relaxed), 1);

        writer.write(0, event(2));
        writer.flush();
        let text = std::fs::read_to_string(&live).unwrap();
        assert_eq!(text.lines().count(), 2, "{text}");
        assert!(!text.contains("telemetry.dropped"));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn retention_deletes_crash_sidecars_whose_dump_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let crashes = dir.path().join("crashes");
        std::fs::create_dir_all(&crashes).unwrap();
        for name in ["orphan.json", "pair.dmp", "pair.json", "fresh-orphan.json"] {
            std::fs::write(crashes.join(name), b"x").unwrap();
        }
        age(&crashes.join("orphan.json"), 10 * 24 * 3600);
        let store = ProfilerStore::start(StoreConfig {
            max_total_bytes: 1 << 20,
            ..config(dir.path())
        });
        store.sender().send(event(1));
        store.shutdown();
        assert!(!crashes.join("orphan.json").exists());
        assert!(crashes.join("pair.dmp").exists());
        assert!(crashes.join("pair.json").exists());
        assert!(crashes.join("fresh-orphan.json").exists());
    }

    #[test]
    fn maintain_runs_retention_but_never_deletes_the_live_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProfilerStore::start(config(dir.path()));
        store.sender().send(event(1));
        let live = dir.path().join("events-000000.ndjson");
        wait_until(|| file_len(&live) > 0);
        let stale = dir.path().join("events-000050.ndjson");
        std::fs::write(&stale, b"{}\n").unwrap();
        age(&stale, 10 * 24 * 3600);
        age(&live, 10 * 24 * 3600);

        store.sender().maintain();
        wait_until(|| !stale.exists());
        store.sender().send(event(2));
        store.shutdown();
        assert!(
            live.exists(),
            "the open live file is never a retention candidate"
        );
        assert!(read_all(dir.path()).iter().any(|l| l["n"] == 2));
    }

    #[test]
    fn replace_findings_rewrites_the_file_in_queue_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = ProfilerStore::start(config(dir.path()));
        let sender = store.sender();
        sender.send_finding(json!({ "id": "a" }));
        sender.send_finding(json!({ "id": "a" }));
        sender.replace_findings(vec![json!({ "id": "a" }), json!({ "id": "b" })]);
        sender.send_finding(json!({ "id": "c" }));
        store.shutdown();
        let text = std::fs::read_to_string(dir.path().join(FINDINGS_FILE)).unwrap();
        let ids: Vec<_> = text
            .lines()
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap()["id"].clone())
            .collect();
        assert_eq!(ids, [json!("a"), json!("b"), json!("c")]);
        assert!(!dir.path().join(FINDINGS_TMP_FILE).exists());
    }

    /// Sum of the `count` of every `telemetry.dropped` line under `dir`.
    fn reported_drops(dir: &std::path::Path) -> u64 {
        read_all(dir)
            .iter()
            .filter(|l| l["kind"] == "telemetry.dropped")
            .map(|l| l["count"].as_u64().unwrap())
            .sum()
    }

    fn age(path: &std::path::Path, seconds: u64) {
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(seconds))
            .unwrap();
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
