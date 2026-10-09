//! The clean-shutdown sentinel. `session.json` records that a run is in progress; an orderly
//! shutdown marks it clean, so the next start can tell how a run that never did ended.

use std::{fs, io, path::Path};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{
    findings::{FindingInput, Severity},
    os_records::{self, OsCrashRecord},
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
    Crash(OsCrashRecord),
    Oom,
    UncleanExit { facility: &'static str },
    SystemCrash,
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

/// An OS crash record is the most specific evidence; a different boot means the system went
/// down with the run; a risen `oom_kill` count points at the OOM killer.
pub fn classify(
    previous: &SessionRecord,
    current_boot: Option<&str>,
    oom_now: Option<u64>,
    record: Option<OsCrashRecord>,
) -> PreviousRun {
    if previous.state != RUNNING {
        return PreviousRun::Clean;
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
            classify(&prev, Some("b2"), Some(3), None),
            PreviousRun::SystemCrash
        ));
        assert!(matches!(
            classify(&prev, Some("b1"), Some(4), None),
            PreviousRun::Oom
        ));
        assert!(matches!(
            classify(&prev, Some("b1"), Some(3), None),
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
            classify(&prev, Some("b1"), Some(3), Some(record)),
            PreviousRun::Crash(_)
        ));
    }

    #[test]
    fn boot_times_within_a_minute_are_the_same_boot() {
        let mut prev = previous();
        prev.boot_id = Some("1791540000".into());
        assert!(matches!(
            classify(&prev, Some("1791540001"), None, None),
            PreviousRun::UncleanExit { .. }
        ));
        assert!(matches!(
            classify(&prev, Some("1791550000"), None, None),
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
