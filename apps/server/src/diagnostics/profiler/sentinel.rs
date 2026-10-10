//! The clean-shutdown sentinel. `session.json` records that a run is in progress; an orderly
//! shutdown marks it clean, so the next start can tell how a run that never did ended.

use std::{collections::HashSet, fs, io, path::Path};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::{
    findings::{FindingInput, Severity},
    os_records::{self, OsCrashRecord},
    reader::Reader,
    redact::redact_path,
    store,
};

const SESSION_FILE: &str = "session.json";
const SESSION_TMP_FILE: &str = "session.json.tmp";
const RUNNING: &str = "running";
const CLEAN: &str = "clean";
/// Outside Linux the boot ID is the boot time in seconds, derived from the wall clock (Windows
/// subtracts the uptime from it), so a clock adjustment shifts it slightly.
const BOOT_TIME_SLACK_SECS: u64 = 60;
/// Frames that make up a native crash's fingerprint.
const FINGERPRINT_FRAMES: usize = 5;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRecord {
    pub state: String,
    pub pid: u32,
    pub boot_id: Option<String>,
    pub exe: String,
    pub version: String,
    pub started_at: String,
    pub oom_kill_baseline: Option<u64>,
}

/// How the previous run ended.
#[derive(Debug)]
pub enum PreviousRun {
    Clean,
    /// The crash monitor dumped it. An OS record of the same crash adds its frames.
    Dump(Dump, Option<OsCrashRecord>),
    Crash(OsCrashRecord),
    Oom,
    UncleanExit {
        facility: &'static str,
    },
    SystemCrash,
}

/// A crash dump's sidecar, as far as linking it to a finding needs.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DumpSidecar {
    pub id: String,
    pub created_at: String,
    pub parent_pid: u32,
    pub process_kind: String,
    #[serde(default)]
    pub version: Option<String>,
    /// Absent from sidecars written before it was recorded.
    #[serde(default)]
    pub boot_id: Option<String>,
}

/// A crash dump and its crash reason, when the dump says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dump {
    pub sidecar: DumpSidecar,
    pub reason: Option<String>,
}

impl Dump {
    /// Reads the crash reason from the dump's exception stream. The dump is mapped, not read
    /// whole, and nothing is symbolicated: that is the CLI's job.
    pub fn read(dir: &Path, sidecar: DumpSidecar) -> Self {
        let path = dir
            .join(store::CRASHES_DIR)
            .join(format!("{}.dmp", sidecar.id));
        Self {
            reason: crash_reason(&path),
            sidecar,
        }
    }

    /// `[processKind, "minidump", crash reason or "dump"]`.
    fn fingerprint_fields(&self) -> Vec<String> {
        vec![
            self.sidecar.process_kind.clone(),
            "minidump".to_owned(),
            self.reason.clone().unwrap_or_else(|| "dump".to_owned()),
        ]
    }

    fn evidence(&self) -> Map<String, Value> {
        let mut evidence = Map::from_iter([
            ("source".to_owned(), "minidump".into()),
            ("crashId".to_owned(), self.sidecar.id.clone().into()),
            (
                "processKind".to_owned(),
                self.sidecar.process_kind.clone().into(),
            ),
            (
                "createdAt".to_owned(),
                self.sidecar.created_at.clone().into(),
            ),
        ]);
        if let Some(reason) = &self.reason {
            evidence.insert("crashReason".to_owned(), reason.clone().into());
        }
        evidence
    }

    /// The `crash.native` finding of a dump written while BiBCode kept running, such as one a
    /// Windows invalid-parameter handler asked for.
    pub fn running_finding(&self) -> FindingInput {
        let reason = self.reason.as_deref().unwrap_or("a native fault");
        let mut evidence = self.evidence();
        if let Some(version) = &self.sidecar.version {
            evidence.insert("version".to_owned(), version.clone().into());
        }
        FindingInput {
            kind: "crash.native",
            severity: Severity::Error,
            source: "server".into(),
            fingerprint_fields: self.fingerprint_fields(),
            title: format!("Native crash dumped: {reason}"),
            summary: format!(
                "The crash monitor wrote minidump {} for the {} process ({reason}), which kept \
                 running.",
                self.sidecar.id, self.sidecar.process_kind
            ),
            evidence: Value::Object(evidence),
        }
    }
}

/// The crash reason a minidump's exception stream holds, such as `SIGSEGV / SEGV_MAPERR` or
/// `EXCEPTION_ACCESS_VIOLATION_WRITE`.
pub fn crash_reason(dump: &Path) -> Option<String> {
    let dump = minidump::Minidump::read_path(dump).ok()?;
    let system = dump.get_stream::<minidump::MinidumpSystemInfo>().ok()?;
    let exception = dump.get_stream::<minidump::MinidumpException>().ok()?;
    Some(
        exception
            .get_crash_reason(system.os, system.cpu)
            .to_string(),
    )
}

/// Every readable crash sidecar in the profiler directory `dir`, newest first.
pub fn dump_sidecars(dir: &Path) -> Vec<DumpSidecar> {
    Reader::new(dir)
        .crashes(usize::MAX)
        .into_iter()
        .filter_map(|sidecar| serde_json::from_value(sidecar).ok())
        .collect()
}

/// Whether a dump in `sidecar` is of the process `pid` that started at `started_at`: written
/// for that pid, at or after the start, and on the same boot when both boots are known.
pub fn dump_is_of(
    sidecar: &DumpSidecar,
    pid: u32,
    started_at: OffsetDateTime,
    boot: Option<&str>,
) -> bool {
    sidecar.parent_pid == pid
        && OffsetDateTime::parse(&sidecar.created_at, &Rfc3339)
            .is_ok_and(|created| created >= started_at)
        && match (sidecar.boot_id.as_deref(), boot) {
            (Some(dumped), Some(boot)) => same_boot(dumped, boot),
            _ => true,
        }
}

/// The previous run's crash dump: its newest dump, unless a finding cites that one already.
/// A dump a finding cites is one the run survived and reported while it ran, and so was every
/// older one, so the run ended some other way. `sidecars` is newest first.
// ponytail: a dump the run survived in its last minute, before the scan found it, is not
// reported when a fatal dump follows it.
pub fn dump_of_run<'a>(
    sidecars: &'a [DumpSidecar],
    previous: &SessionRecord,
    linked: &HashSet<String>,
) -> Option<&'a DumpSidecar> {
    let started_at = OffsetDateTime::parse(&previous.started_at, &Rfc3339).ok()?;
    let boot = previous.boot_id.as_deref();
    sidecars
        .iter()
        .find(|sidecar| dump_is_of(sidecar, previous.pid, started_at, boot))
        .filter(|sidecar| !linked.contains(&sidecar.id))
}

/// Returns the previous record if it was left `running`, then records this run as running
/// through a synced temp file and a rename. Blocks on file I/O.
pub fn begin_session(dir: &Path) -> Option<SessionRecord> {
    let previous = read(dir).filter(|record| record.state == RUNNING);
    let current = SessionRecord {
        state: RUNNING.into(),
        pid: std::process::id(),
        boot_id: boot_id(),
        exe: std::env::current_exe()
            .map(|exe| redact_path(&exe.to_string_lossy()))
            .unwrap_or_default(),
        version: env!("CARGO_PKG_VERSION").into(),
        started_at: store::now_rfc3339(),
        oom_kill_baseline: oom_kill_count(),
    };
    write(dir, &current);
    previous
}

/// Marks this process's session clean. A record another process wrote after this one
/// released the directory is left alone. Blocks on file I/O.
pub fn end_session(dir: &Path) {
    if let Some(mut record) = read(dir).filter(|record| record.pid == std::process::id()) {
        record.state = CLEAN.into();
        write(dir, &record);
    }
}

/// The run's own crash dump is the most specific evidence, then an OS crash record; a
/// different boot means the system went down with the run; a risen `oom_kill` count points at
/// the OOM killer.
pub fn classify(
    previous: &SessionRecord,
    current_boot: Option<&str>,
    oom_now: Option<u64>,
    record: Option<OsCrashRecord>,
    dump: Option<Dump>,
) -> PreviousRun {
    if previous.state != RUNNING {
        return PreviousRun::Clean;
    }
    if let Some(dump) = dump {
        return PreviousRun::Dump(dump, record);
    }
    if let Some(record) = record {
        return PreviousRun::Crash(record);
    }
    if let (Some(before), Some(now)) = (previous.boot_id.as_deref(), current_boot)
        && !same_boot(before, now)
    {
        return PreviousRun::SystemCrash;
    }
    if let (Some(before), Some(now)) = (previous.oom_kill_baseline, oom_now)
        && now > before
    {
        return PreviousRun::Oom;
    }
    PreviousRun::UncleanExit {
        facility: os_records::facility(),
    }
}

fn same_boot(before: &str, now: &str) -> bool {
    before == now
        || matches!(
            (before.parse::<u64>(), now.parse::<u64>()),
            (Ok(before), Ok(now)) if before.abs_diff(now) <= BOOT_TIME_SLACK_SECS
        )
}

/// The finding that reports `run`, or `None` for a clean run.
pub fn finding_for(previous: &SessionRecord, run: PreviousRun) -> Option<FindingInput> {
    let mut evidence = json!({
        "pid": previous.pid,
        "version": previous.version,
        "startedAt": previous.started_at,
    });
    let (kind, severity, fingerprint_fields, title, summary, extra) = match run {
        PreviousRun::Clean => return None,
        PreviousRun::Dump(dump, record) => {
            let reason = dump.reason.as_deref().unwrap_or("a native fault");
            let mut extra = dump.evidence();
            // The OS record of the same crash, when there is one, adds its frames.
            if let Some(record) = &record {
                let frames: Vec<String> = record.frames.iter().map(|f| redact_path(f)).collect();
                extra.extend([
                    ("recordSource".to_owned(), record.source.into()),
                    (
                        "signalOrCode".to_owned(),
                        record.signal_or_code.clone().into(),
                    ),
                    (
                        "module".to_owned(),
                        record.module.as_deref().map(redact_path).into(),
                    ),
                    ("frames".to_owned(), frames.into()),
                    ("rawRef".to_owned(), redact_path(&record.raw_ref).into()),
                ]);
            }
            (
                "crash.native",
                Severity::Error,
                dump.fingerprint_fields(),
                format!("Previous run crashed with {reason}"),
                format!(
                    "{reason} in the {} process; the crash monitor wrote minidump {}.",
                    dump.sidecar.process_kind, dump.sidecar.id
                ),
                Value::Object(extra),
            )
        }
        PreviousRun::Crash(record) => {
            // OS records hold paths and symbols; nothing from them is kept unredacted.
            let frames: Vec<String> = record.frames.iter().map(|f| redact_path(f)).collect();
            // Every abort shares libc's and std's top frames, so our own executable's frames
            // tell crashes apart; other modules' top frames stand in when it has none.
            let own = os_records::file_name(&previous.exe);
            let own_frames: Vec<&String> = frames
                .iter()
                .filter(|frame| {
                    !own.is_empty()
                        && os_records::frame_module(frame)
                            .is_some_and(|module| module.eq_ignore_ascii_case(own))
                })
                .collect();
            let (module, key_frames) = if own_frames.is_empty() {
                (
                    record.module.as_deref().map(redact_path),
                    frames.iter().collect::<Vec<_>>(),
                )
            } else {
                (Some(own.to_owned()), own_frames)
            };
            let shown_module = module.as_deref().unwrap_or("an unknown module");
            let mut fields = vec![
                record.source.to_owned(),
                record.signal_or_code.clone(),
                shown_module.to_owned(),
            ];
            // The fault site too: faults at different places behind one caller stay apart.
            fields.extend(frames.first().cloned());
            fields.extend(key_frames.into_iter().take(FINGERPRINT_FRAMES).cloned());
            (
                "crash.native",
                Severity::Error,
                fields,
                format!("Previous run crashed with {}", record.signal_or_code),
                format!(
                    "{} in {shown_module}, from the OS crash record ({})",
                    record.signal_or_code, record.source
                ),
                json!({
                    "source": record.source,
                    "signalOrCode": record.signal_or_code,
                    "module": module,
                    "frames": frames,
                    "rawRef": redact_path(&record.raw_ref),
                }),
            )
        }
        PreviousRun::Oom => (
            "crash.oom",
            Severity::Error,
            Vec::new(),
            "Previous run was likely killed for using too much memory".to_owned(),
            "The user session's oom_kill count rose while BiBCode was running, and no crash \
             record matched it."
                .to_owned(),
            json!({}),
        ),
        PreviousRun::UncleanExit { facility } => (
            "crash.unclean-exit",
            Severity::Warn,
            Vec::new(),
            "Previous run did not exit cleanly".to_owned(),
            format!(
                "BiBCode stopped without shutting down and no OS crash record matched \
                 (crash records: {facility}). A forced kill, a hang or a missed crash dump \
                 can cause this."
            ),
            json!({ "facility": facility }),
        ),
        PreviousRun::SystemCrash => (
            "crash.system",
            Severity::Info,
            Vec::new(),
            "Previous run ended with the system".to_owned(),
            "The computer restarted, crashed or lost power while BiBCode was running.".to_owned(),
            json!({}),
        ),
    };
    if let (Value::Object(evidence), Value::Object(extra)) = (&mut evidence, extra) {
        evidence.extend(extra);
    }
    Some(FindingInput {
        kind,
        severity,
        source: "server".into(),
        fingerprint_fields,
        title,
        summary,
        evidence,
    })
}

/// Linux's boot ID; elsewhere the boot time in seconds.
pub fn boot_id() -> Option<String> {
    if cfg!(target_os = "linux") {
        fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .ok()
            .map(|id| id.trim().to_owned())
    } else {
        Some(sysinfo::System::boot_time().to_string())
    }
}

/// The OOM killer's kill count in the user session's cgroup (Linux only).
pub fn oom_kill_count() -> Option<u64> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let cgroup = fs::read_to_string("/proc/self/cgroup").ok()?;
    os_records::linux::read_oom_kill(&os_records::linux::user_service_events(&cgroup)?)
}

fn read(dir: &Path) -> Option<SessionRecord> {
    serde_json::from_slice(&fs::read(dir.join(SESSION_FILE)).ok()?).ok()
}

fn write(dir: &Path, record: &SessionRecord) {
    let tmp = dir.join(SESSION_TMP_FILE);
    let written = serde_json::to_value(record)
        .map_err(io::Error::from)
        .and_then(|line| store::write_lines(&tmp, &[line]))
        .and_then(|()| fs::rename(&tmp, dir.join(SESSION_FILE)));
    if let Err(error) = written {
        let _ = fs::remove_file(&tmp);
        tracing::warn!(%error, "profiler session sentinel was not written");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn previous() -> SessionRecord {
        SessionRecord {
            state: "running".into(),
            pid: 42,
            boot_id: Some("b1".into()),
            exe: "bibcode".into(),
            version: "0.9.0".into(),
            started_at: "2026-10-09T10:00:00Z".into(),
            oom_kill_baseline: Some(3),
        }
    }

    #[test]
    fn classifies_previous_runs() {
        let prev = previous();
        assert!(matches!(
            classify(&prev, Some("b2"), Some(3), None, None),
            PreviousRun::SystemCrash
        ));
        assert!(matches!(
            classify(&prev, Some("b1"), Some(4), None, None),
            PreviousRun::Oom
        ));
        assert!(matches!(
            classify(&prev, Some("b1"), Some(3), None, None),
            PreviousRun::UncleanExit { .. }
        ));
        let record = OsCrashRecord {
            source: "coredump",
            signal_or_code: "SIGSEGV".into(),
            module: None,
            frames: vec![],
            raw_ref: "pid 42".into(),
        };
        assert!(matches!(
            classify(&prev, Some("b1"), Some(3), Some(record), None),
            PreviousRun::Crash(_)
        ));
    }

    fn sidecar(id: &str, pid: u32, created_at: &str, boot: Option<&str>) -> DumpSidecar {
        DumpSidecar {
            id: id.into(),
            created_at: created_at.into(),
            parent_pid: pid,
            process_kind: "server".into(),
            version: Some("0.9.0".into()),
            boot_id: boot.map(Into::into),
        }
    }

    /// Only a dump of the previous run itself counts: its pid, after its start, on its boot.
    #[test]
    fn the_previous_runs_newest_unlinked_dump_is_its_crash() {
        let prev = previous();
        let sidecars = [
            sidecar("other-pid", 43, "2026-10-09T10:05:00Z", Some("b1")),
            sidecar("other-boot", 42, "2026-10-09T10:04:00Z", Some("b0")),
            sidecar("newest", 42, "2026-10-09T10:02:00.5Z", None),
            sidecar("older", 42, "2026-10-09T10:01:00Z", Some("b1")),
            sidecar("before-start", 42, "2026-10-09T09:59:59.9Z", Some("b1")),
        ];
        let mut linked = HashSet::new();
        let found = |linked: &HashSet<String>| {
            dump_of_run(&sidecars, &prev, linked).map(|sidecar| sidecar.id.clone())
        };
        assert_eq!(found(&linked).as_deref(), Some("newest"));
        // A finding cites the newest dump already: the run survived it, as it did every older
        // one, so the run ended some other way. A repeat replaces a finding's `crashId`, so an
        // older dump may look unlinked after a restart; it must not count again.
        linked.insert("newest".to_owned());
        assert_eq!(found(&linked), None, "the run survived its newest dump");
    }

    /// The run's own dump outranks everything else on the next start, and keeps the OS
    /// record's frames.
    #[test]
    fn a_dump_of_the_previous_run_is_its_native_crash() {
        let prev = previous();
        let dump = |reason: Option<&str>| Dump {
            sidecar: sidecar("6f1c", 42, "2026-10-09T10:02:00Z", Some("b1")),
            reason: reason.map(Into::into),
        };
        let record = OsCrashRecord {
            source: "coredump",
            signal_or_code: "SIGSEGV".into(),
            module: Some("bibcode".into()),
            frames: vec!["main (bibcode + 0x10)".into()],
            raw_ref: "coredumpctl info 42".into(),
        };
        // Another boot and a risen OOM count since: the dump still says what happened.
        let run = classify(
            &prev,
            Some("b2"),
            Some(4),
            Some(record),
            Some(dump(Some("SIGSEGV / SEGV_MAPERR"))),
        );
        let input = finding_for(&prev, run).unwrap();
        assert_eq!(input.kind, "crash.native");
        assert_eq!(input.severity, Severity::Error);
        assert_eq!(
            input.fingerprint_fields,
            ["server", "minidump", "SIGSEGV / SEGV_MAPERR"]
        );
        assert_eq!(input.evidence["crashId"], "6f1c");
        assert_eq!(input.evidence["source"], "minidump");
        assert_eq!(input.evidence["recordSource"], "coredump");
        assert_eq!(input.evidence["frames"][0], "main (bibcode + 0x10)");
        assert_eq!(input.evidence["pid"], 42);
        assert!(input.title.contains("SIGSEGV"), "{}", input.title);

        let unread = finding_for(&prev, classify(&prev, None, None, None, Some(dump(None))));
        assert_eq!(
            unread.unwrap().fingerprint_fields,
            ["server", "minidump", "dump"]
        );
        let running = dump(None).running_finding();
        assert_eq!(running.kind, "crash.native");
        assert_eq!(running.fingerprint_fields, ["server", "minidump", "dump"]);
        assert_eq!(running.evidence["crashId"], "6f1c");
    }

    #[test]
    fn a_file_that_is_not_a_minidump_has_no_crash_reason() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.dmp");
        std::fs::write(&path, "not a minidump").unwrap();
        assert_eq!(crash_reason(&path), None);
        assert_eq!(crash_reason(&dir.path().join("missing.dmp")), None);
    }

    #[test]
    fn boot_times_within_a_minute_are_the_same_boot() {
        let mut prev = previous();
        prev.boot_id = Some("1791540000".into());
        assert!(matches!(
            classify(&prev, Some("1791540001"), None, None, None),
            PreviousRun::UncleanExit { .. }
        ));
        assert!(matches!(
            classify(&prev, Some("1791550000"), None, None, None),
            PreviousRun::SystemCrash
        ));
    }

    #[test]
    fn begin_session_returns_previous_running_record_and_end_marks_clean() {
        let dir = tempfile::tempdir().unwrap();
        assert!(begin_session(dir.path()).is_none());
        let prev = begin_session(dir.path()).expect("first session was left running");
        assert_eq!(prev.state, "running");
        assert_eq!(prev.pid, std::process::id());
        end_session(dir.path());
        assert!(begin_session(dir.path()).is_none());
    }

    #[test]
    fn end_session_leaves_another_process_record_running() {
        let dir = tempfile::tempdir().unwrap();
        let other = SessionRecord {
            pid: std::process::id().wrapping_add(1),
            ..previous()
        };
        std::fs::write(
            dir.path().join("session.json"),
            serde_json::to_vec(&other).unwrap(),
        )
        .unwrap();
        end_session(dir.path());
        assert_eq!(begin_session(dir.path()), Some(other));
    }

    fn abort_in(own_frame: &str) -> OsCrashRecord {
        let mut frames: Vec<String> = (0..5)
            .map(|n| format!("n/a (libc.so.6 + 0x{n}a17c)"))
            .collect();
        frames.push(format!("{own_frame} (bibcode + 0x1234)"));
        OsCrashRecord {
            source: "coredump",
            signal_or_code: "SIGABRT".into(),
            module: Some("libc.so.6".into()),
            frames,
            raw_ref: "coredumpctl info 42".into(),
        }
    }

    /// Aborts all share libc's top frames; our own frames tell them apart.
    #[test]
    fn native_crashes_are_told_apart_by_our_own_frames() {
        let prev = previous();
        let first = finding_for(&prev, PreviousRun::Crash(abort_in("parse_a"))).unwrap();
        let second = finding_for(&prev, PreviousRun::Crash(abort_in("parse_b"))).unwrap();
        let fingerprint = |input: &FindingInput| {
            super::super::findings::fingerprint(input.kind, &input.fingerprint_fields)
        };
        assert_ne!(fingerprint(&first), fingerprint(&second));
        assert_eq!(first.evidence["module"], "bibcode");
        assert!(first.summary.contains("bibcode"), "{}", first.summary);
        // No frame in our module: the top frames and the record's module stand.
        let mut foreign = abort_in("parse_a");
        foreign.frames.pop();
        let foreign = finding_for(&prev, PreviousRun::Crash(foreign)).unwrap();
        assert_eq!(foreign.evidence["module"], "libc.so.6");
    }

    /// Faults at different places in a library reached from the same caller stay apart.
    #[test]
    fn the_fault_site_stays_in_the_fingerprint() {
        let segv = |fault: &str| OsCrashRecord {
            source: "ips",
            signal_or_code: "SIGSEGV".into(),
            module: Some("libx.dylib".into()),
            frames: vec![fault.to_owned(), "main (bibcode + 0x1234)".into()],
            raw_ref: "bibcode pid 42".into(),
        };
        let fingerprint = |record| {
            let input = finding_for(&previous(), PreviousRun::Crash(record)).unwrap();
            super::super::findings::fingerprint(input.kind, &input.fingerprint_fields)
        };
        assert_ne!(
            fingerprint(segv("fault_a (libx.dylib + 0x10)")),
            fingerprint(segv("fault_b (libx.dylib + 0x20)"))
        );
    }

    #[test]
    fn os_record_text_is_redacted() {
        let home = dirs::home_dir().unwrap().to_string_lossy().into_owned();
        let record = OsCrashRecord {
            source: "ips",
            signal_or_code: "SIGSEGV".into(),
            module: Some(format!("{home}/lib/libx.dylib")),
            frames: vec![format!("{home}/src/x.rs (libx.dylib + 0x10)")],
            raw_ref: format!("{home}/Library/Logs/DiagnosticReports/x.ips"),
        };
        let input = finding_for(&previous(), PreviousRun::Crash(record)).unwrap();
        let text = input.evidence.to_string() + &input.summary + &input.title;
        assert!(!text.contains(&home), "{text}");
        assert!(
            input.evidence["frames"][0]
                .as_str()
                .unwrap()
                .starts_with("~/src/x.rs")
        );
    }

    #[test]
    fn findings_name_the_crash_kind() {
        let prev = previous();
        let kind = |run| finding_for(&prev, run).map(|input| input.kind);
        assert_eq!(kind(PreviousRun::Clean), None);
        assert_eq!(kind(PreviousRun::SystemCrash), Some("crash.system"));
        assert_eq!(kind(PreviousRun::Oom), Some("crash.oom"));
        let unclean = finding_for(
            &prev,
            PreviousRun::UncleanExit {
                facility: "no systemd-coredump",
            },
        )
        .unwrap();
        assert_eq!(unclean.kind, "crash.unclean-exit");
        assert!(unclean.summary.contains("no systemd-coredump"));
        let record = OsCrashRecord {
            source: "coredump",
            signal_or_code: "SIGSEGV".into(),
            module: Some("libc.so.6".into()),
            frames: vec!["n/a (libc.so.6 + 0x9a17c)".into()],
            raw_ref: "coredumpctl info 42".into(),
        };
        let native = finding_for(&prev, PreviousRun::Crash(record)).unwrap();
        assert_eq!(native.kind, "crash.native");
        assert!(native.title.contains("SIGSEGV"));
        assert_eq!(native.evidence["frames"][0], "n/a (libc.so.6 + 0x9a17c)");
    }
}
