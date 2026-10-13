//! The clean-shutdown sentinel. `session.json` records that a run is in progress, then that its
//! shutdown began; an orderly shutdown marks it clean, so the next start can tell how a run
//! that never did ended.

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
/// Shutdown began: a run left in this state was being shut down, not crashed, unless a dump
/// or an OS crash record says otherwise.
const STOPPING: &str = "stopping";
const CLEAN: &str = "clean";
/// Outside Linux the boot ID is the boot time in seconds, derived from the wall clock (Windows
/// subtracts the uptime from it), so a clock adjustment shifts it slightly.
const BOOT_TIME_SLACK_SECS: u64 = 60;
/// Frames that make up a native crash's fingerprint.
const FINGERPRINT_FRAMES: usize = 5;
/// A larger dump is not opened: its fingerprint falls back to the process kind alone.
const MAX_PARSED_DUMP_BYTES: u64 = 256 * 1024 * 1024;

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
    /// Shutdown began and never finished, and nothing says the run crashed.
    ShutdownIncomplete,
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

/// Where a dumped crash happened: the faulting instruction's module and its offset there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fault {
    /// The module's file name, without its directory.
    pub module: String,
    pub offset: u64,
}

/// A crash dump, its crash reason and its fault site, when the dump says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dump {
    pub sidecar: DumpSidecar,
    pub reason: Option<String>,
    pub fault: Option<Fault>,
}

impl Dump {
    /// Reads the crash reason and the fault site from the dump's exception stream and module
    /// list. The dump is mapped, not read whole, and nothing is symbolicated: that is the CLI's
    /// job. Blocks on file I/O, so it runs off the async workers, once per dump.
    pub fn read(dir: &Path, sidecar: DumpSidecar) -> Self {
        let path = dir
            .join(store::CRASHES_DIR)
            .join(format!("{}.dmp", sidecar.id));
        let (reason, fault) = read_dump(&path);
        Self {
            reason,
            fault,
            sidecar,
        }
    }

    /// `[processKind, "minidump", crash reason, fault module, fault offset]`, so faults at
    /// different places stay apart; `[processKind, "minidump", crash reason or "dump"]` when the
    /// dump names no fault site.
    fn fingerprint_fields(&self) -> Vec<String> {
        let mut fields = vec![
            self.sidecar.process_kind.clone(),
            "minidump".to_owned(),
            self.reason.clone().unwrap_or_else(|| "dump".to_owned()),
        ];
        if let Some(fault) = &self.fault {
            fields.extend([fault.module.clone(), format!("{:#x}", fault.offset)]);
        }
        fields
    }

    fn evidence(&self) -> Map<String, Value> {
        let mut evidence = Map::from_iter([
            ("source".to_owned(), "minidump".into()),
            ("crashId".to_owned(), self.sidecar.id.clone().into()),
            // Every dump this finding counted, newest first; `FindingsBook` merges repeats.
            ("crashIds".to_owned(), vec![self.sidecar.id.clone()].into()),
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
        if let Some(fault) = &self.fault {
            evidence.insert("faultModule".to_owned(), fault.module.clone().into());
            evidence.insert(
                "faultOffset".to_owned(),
                format!("{:#x}", fault.offset).into(),
            );
        }
        evidence
    }

    /// The `crash.native` finding of a dump written while BiBCode kept running, such as one a
    /// Windows invalid-parameter handler asked for.
    pub fn running_finding(&self) -> FindingInput {
        self.finding(format!(
            "The crash monitor wrote minidump {} for the {} process ({}), which kept running.",
            self.sidecar.id,
            self.sidecar.process_kind,
            self.reason.as_deref().unwrap_or("a native fault"),
        ))
    }

    /// The `crash.native` finding of a dump no finding linked to its run: written for a process
    /// that no profiler session classified, such as a desktop host that crashed during startup.
    pub fn unlinked_finding(&self) -> FindingInput {
        self.finding(format!(
            "The crash monitor wrote minidump {} for the {} process (pid {}, {}), and no \
             profiler session reported it.",
            self.sidecar.id,
            self.sidecar.process_kind,
            self.sidecar.parent_pid,
            self.reason.as_deref().unwrap_or("a native fault"),
        ))
    }

    fn finding(&self, summary: String) -> FindingInput {
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
            summary,
            evidence: Value::Object(evidence),
        }
    }
}

/// The crash reason a minidump's exception stream holds, such as `SIGSEGV / SEGV_MAPERR` or
/// `EXCEPTION_ACCESS_VIOLATION_WRITE`.
pub fn crash_reason(dump: &Path) -> Option<String> {
    read_dump(dump).0
}

/// The crash reason and the fault site of a minidump: the module that holds the crashing
/// thread's instruction pointer, or the exception address when the dump has no context. A
/// file over 256 MiB, or one that is not a minidump, yields neither.
fn read_dump(path: &Path) -> (Option<String>, Option<Fault>) {
    if fs::metadata(path).map_or(true, |meta| meta.len() > MAX_PARSED_DUMP_BYTES) {
        return (None, None);
    }
    let Ok(dump) = minidump::Minidump::read_path(path) else {
        return (None, None);
    };
    let (Ok(system), Ok(exception)) = (
        dump.get_stream::<minidump::MinidumpSystemInfo>(),
        dump.get_stream::<minidump::MinidumpException>(),
    ) else {
        return (None, None);
    };
    let reason = exception
        .get_crash_reason(system.os, system.cpu)
        .to_string();
    let misc = dump.get_stream::<minidump::MinidumpMiscInfo>().ok();
    let address = exception.context(&system, misc.as_ref()).map_or(
        exception.raw.exception_record.exception_address,
        |context| context.get_instruction_pointer(),
    );
    let fault = dump
        .get_stream::<minidump::MinidumpModuleList>()
        .ok()
        .and_then(|modules| {
            use minidump::Module;
            let module = modules.module_at_address(address)?;
            Some(Fault {
                module: os_records::file_name(&module.code_file()).to_owned(),
                offset: address.checked_sub(module.base_address())?,
            })
        });
    (Some(reason), fault)
}

/// Every readable crash sidecar in the profiler directory `dir`, newest `createdAt` first;
/// one whose time cannot be read comes last. File times are not used: copying or restoring
/// the directory changes them.
pub fn dump_sidecars(dir: &Path) -> Vec<DumpSidecar> {
    let mut sidecars: Vec<DumpSidecar> = Reader::new(dir)
        .crashes(usize::MAX)
        .into_iter()
        .filter_map(|sidecar| serde_json::from_value(sidecar).ok())
        .collect();
    sidecars.sort_by_cached_key(|sidecar| {
        std::cmp::Reverse(OffsetDateTime::parse(&sidecar.created_at, &Rfc3339).ok())
    });
    sidecars
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

/// Returns the previous record if it was left `running` or `stopping`, then records this run as
/// running through a synced temp file and a rename. Blocks on file I/O.
pub fn begin_session(dir: &Path) -> Option<SessionRecord> {
    let previous = read(dir).filter(|record| record.state == RUNNING || record.state == STOPPING);
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

/// Marks this process's running session as stopping: its shutdown has begun. A clean record,
/// or one another process wrote, is left alone. Blocks on file I/O.
pub fn mark_stopping(dir: &Path) {
    if let Some(mut record) =
        read(dir).filter(|record| record.pid == std::process::id() && record.state == RUNNING)
    {
        record.state = STOPPING.into();
        write(dir, &record);
    }
}

/// Marks this process's session clean. A record another process wrote after this one
/// released the directory is left alone. Blocks on file I/O.
pub fn end_session(dir: &Path) {
    if let Some(mut record) = read(dir).filter(|record| record.pid == std::process::id()) {
        record.state = CLEAN.into();
        write(dir, &record);
    }
}

/// The run's own crash dump is the most specific evidence, then an OS crash record; a run
/// whose shutdown had begun otherwise just never finished it; a different boot means the
/// system went down with the run; a risen `oom_kill` count points at the OOM killer.
pub fn classify(
    previous: &SessionRecord,
    current_boot: Option<&str>,
    oom_now: Option<u64>,
    record: Option<OsCrashRecord>,
    dump: Option<Dump>,
) -> PreviousRun {
    if previous.state != RUNNING && previous.state != STOPPING {
        return PreviousRun::Clean;
    }
    if let Some(dump) = dump {
        return PreviousRun::Dump(dump, record);
    }
    if let Some(record) = record {
        return PreviousRun::Crash(record);
    }
    if previous.state == STOPPING {
        return PreviousRun::ShutdownIncomplete;
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
            // The OS record of the same crash, when there is one, adds its frames, and its own
            // module's frames tell crashes apart as they do without a dump.
            let fingerprint_fields = match &record {
                Some(record) => {
                    let site = RecordSite::of(previous, record);
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
                        ("frames".to_owned(), site.frames.clone().into()),
                        ("rawRef".to_owned(), redact_path(&record.raw_ref).into()),
                    ]);
                    site.fingerprint_fields
                }
                None => dump.fingerprint_fields(),
            };
            (
                "crash.native",
                Severity::Error,
                fingerprint_fields,
                format!("Previous run crashed with {reason}"),
                format!(
                    "{reason} in the {} process; the crash monitor wrote minidump {}.",
                    dump.sidecar.process_kind, dump.sidecar.id
                ),
                Value::Object(extra),
            )
        }
        PreviousRun::Crash(record) => {
            let RecordSite {
                fingerprint_fields,
                module,
                frames,
            } = RecordSite::of(previous, &record);
            let shown_module = module.as_deref().unwrap_or("an unknown module");
            (
                "crash.native",
                Severity::Error,
                fingerprint_fields,
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
        // Not a `crash.*` kind: it never sets the last crash.
        PreviousRun::ShutdownIncomplete => (
            "shutdown.incomplete",
            Severity::Info,
            Vec::new(),
            "BiBCode did not finish shutting down".to_owned(),
            "The previous run began shutting down and stopped before it finished, and no crash \
             record matched it. Quitting while work was still stopping, or a forced quit, can \
             cause this."
                .to_owned(),
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

/// Where an OS crash record says a crash happened, redacted.
struct RecordSite {
    fingerprint_fields: Vec<String>,
    /// The module the fingerprint names: our own executable when a frame is in it.
    module: Option<String>,
    frames: Vec<String>,
}

impl RecordSite {
    fn of(previous: &SessionRecord, record: &OsCrashRecord) -> Self {
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
        let mut fingerprint_fields = vec![
            record.source.to_owned(),
            record.signal_or_code.clone(),
            module.as_deref().unwrap_or("an unknown module").to_owned(),
        ];
        // The fault site too: faults at different places behind one caller stay apart.
        fingerprint_fields.extend(frames.first().cloned());
        fingerprint_fields.extend(key_frames.into_iter().take(FINGERPRINT_FRAMES).cloned());
        Self {
            fingerprint_fields,
            module,
            frames,
        }
    }
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

/// The last session record, whatever its state. Blocks on file I/O.
pub fn last_session(dir: &Path) -> Option<SessionRecord> {
    read(dir)
}

/// A crash of this executable the OS kept that no session reported. `crash_id` links it,
/// so the next start does not raise it again.
pub fn unrecorded_crash_finding(
    crash: &super::os_records::UnrecordedCrash,
    crash_id: String,
) -> FindingInput {
    let name = super::os_records::executable_name(&crash.exe).to_owned();
    let crashed_at = i128::from(crash.time_micros)
        .checked_mul(1_000)
        .and_then(|nanos| OffsetDateTime::from_unix_timestamp_nanos(nanos).ok())
        .and_then(|time| time.format(&Rfc3339).ok());
    FindingInput {
        kind: "crash.native",
        severity: Severity::Error,
        source: "server".into(),
        fingerprint_fields: vec![
            "unrecorded".to_owned(),
            name.clone(),
            format!("signal {}", crash.signal),
        ],
        title: format!(
            "{name} crashed with signal {} outside a profiler session",
            crash.signal
        ),
        summary: format!(
            "systemd-coredump recorded signal {} for {name} (pid {}), a process no profiler \
             session covered, such as an app relaunched by an update or a crash monitor.",
            crash.signal, crash.pid
        ),
        evidence: json!({
            "source": "coredump",
            "pid": crash.pid,
            "signal": crash.signal,
            "exe": redact_path(&crash.exe),
            "crashedAt": crashed_at,
            "crashId": crash_id,
            "crashIds": [crash_id],
            "rawRef": format!("coredumpctl info {}", crash.pid),
        }),
    }
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

    #[test]
    fn an_unrecorded_crash_is_linked_by_its_coredump_and_grouped_by_signal() {
        let crash = |pid| crate::diagnostics::profiler::os_records::UnrecordedCrash {
            pid,
            signal: 7,
            time_micros: 1_791_000_000_000_000,
            exe: "/tmp/.mount_abc/usr/bin/bibcode-desktop".to_owned(),
        };
        let first = unrecorded_crash_finding(&crash(10), "coredump-10-1".to_owned());
        let second = unrecorded_crash_finding(&crash(11), "coredump-11-1".to_owned());
        assert_eq!(first.kind, "crash.native");
        assert_eq!(first.fingerprint_fields, second.fingerprint_fields);
        assert_eq!(first.evidence["crashIds"], json!(["coredump-10-1"]));
        assert_eq!(first.evidence["signal"], 7);
        assert!(
            first
                .title
                .starts_with("bibcode-desktop crashed with signal 7")
        );
        assert!(first.evidence["crashedAt"].as_str().is_some());
    }

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
            fault: None,
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
            Some(record.clone()),
            Some(dump(Some("SIGSEGV / SEGV_MAPERR"))),
        );
        let input = finding_for(&prev, run).unwrap();
        assert_eq!(input.kind, "crash.native");
        assert_eq!(input.severity, Severity::Error);
        // The record's own-module frames tell crashes apart, as without a dump.
        let recorded = finding_for(&prev, PreviousRun::Crash(record)).unwrap();
        assert_eq!(input.fingerprint_fields, recorded.fingerprint_fields);
        assert_eq!(input.fingerprint_fields[2], "bibcode");
        assert_eq!(input.evidence["crashId"], "6f1c");
        assert_eq!(input.evidence["crashIds"], json!(["6f1c"]));
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

    /// Without an OS record, a dump's fault site keeps different crashes apart, and the same
    /// site again is a repeat of one finding.
    #[test]
    fn dumps_are_told_apart_by_their_fault_site() {
        let dump = |id: &str, offset: u64| Dump {
            sidecar: sidecar(id, 42, "2026-10-09T10:02:00Z", Some("b1")),
            reason: Some("SIGSEGV / SEGV_MAPERR".into()),
            fault: Some(Fault {
                module: "bibcode".into(),
                offset,
            }),
        };
        assert_eq!(
            dump("a", 0x1234).running_finding().fingerprint_fields,
            [
                "server",
                "minidump",
                "SIGSEGV / SEGV_MAPERR",
                "bibcode",
                "0x1234"
            ]
        );
        let dir = tempfile::tempdir().unwrap();
        let store = store::ProfilerStore::start(store::StoreConfig::new(dir.path().into()));
        let mut book = super::super::findings::FindingsBook::default();
        let now = OffsetDateTime::now_utc();
        for (id, offset) in [("a", 0x1234), ("b", 0x5678), ("c", 0x1234)] {
            book.record_now(dump(id, offset).running_finding(), now, &store.sender());
        }
        // The previous run's dump without a record follows the same rule.
        let prev = previous();
        let previous_run = finding_for(&prev, PreviousRun::Dump(dump("d", 0x5678), None)).unwrap();
        assert_eq!(
            previous_run.fingerprint_fields,
            dump("x", 0x5678).running_finding().fingerprint_fields
        );
        store.shutdown();
        let mut counts: Vec<u64> = book.newest(10).iter().map(|f| f.count).collect();
        counts.sort_unstable();
        assert_eq!(counts, [1, 2], "two sites, one of them twice");
    }

    #[test]
    fn a_dump_over_256_mib_is_not_opened() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.dmp");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_PARSED_DUMP_BYTES + 1).unwrap();
        assert_eq!(read_dump(&path), (None, None));
    }

    /// The newest-dump rule goes by when the monitor wrote each dump, not by file times, which
    /// a copy or a restore of the directory can change.
    #[test]
    fn sidecars_are_ordered_by_creation_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let crashes = dir.path().join("crashes");
        std::fs::create_dir_all(&crashes).unwrap();
        // `later` was created after `earlier`, but its file is the older one.
        for (id, created_at, age_secs) in [
            ("earlier", "2026-10-09T10:00:00Z", 0),
            ("later", "2026-10-09T10:05:00Z", 3600),
            ("unreadable-time", "yesterday", 7200),
        ] {
            let path = crashes.join(format!("{id}.json"));
            let sidecar = json!({
                "id": id, "createdAt": created_at, "parentPid": 42, "processKind": "server",
            });
            std::fs::write(&path, sidecar.to_string()).unwrap();
            std::fs::File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_modified(
                    std::time::SystemTime::now() - std::time::Duration::from_secs(age_secs),
                )
                .unwrap();
        }
        let ids: Vec<String> = dump_sidecars(dir.path())
            .into_iter()
            .map(|sidecar| sidecar.id)
            .collect();
        assert_eq!(ids, ["later", "earlier", "unreadable-time"]);
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

    /// A run that began shutting down and never finished is not a crash, unless a dump or an
    /// OS record says it crashed on the way out.
    #[test]
    fn a_shutdown_that_never_finished_is_info_unless_it_crashed() {
        let prev = SessionRecord {
            state: "stopping".into(),
            ..previous()
        };
        // Another boot and a risen OOM count do not make it a crash either.
        let run = classify(&prev, Some("b2"), Some(4), None, None);
        assert!(matches!(run, PreviousRun::ShutdownIncomplete), "{run:?}");
        let input = finding_for(&prev, run).unwrap();
        assert_eq!(input.kind, "shutdown.incomplete");
        assert_eq!(input.severity, Severity::Info);
        assert_eq!(input.title, "BiBCode did not finish shutting down");
        assert_eq!(input.evidence["pid"], 42);

        let dump = Dump {
            sidecar: sidecar("d", 42, "2026-10-09T10:02:00Z", Some("b1")),
            reason: Some("SIGSEGV / SEGV_MAPERR".into()),
            fault: None,
        };
        assert!(matches!(
            classify(&prev, Some("b1"), Some(3), None, Some(dump)),
            PreviousRun::Dump(..)
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

    #[test]
    fn mark_stopping_records_a_shutdown_in_progress_for_the_next_start() {
        let dir = tempfile::tempdir().unwrap();
        assert!(begin_session(dir.path()).is_none());
        mark_stopping(dir.path());
        let prev = begin_session(dir.path()).expect("a shutdown that never finished");
        assert_eq!(prev.state, "stopping");
        // An orderly end after it is clean; a clean record is never turned back.
        mark_stopping(dir.path());
        end_session(dir.path());
        mark_stopping(dir.path());
        assert!(begin_session(dir.path()).is_none());
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
