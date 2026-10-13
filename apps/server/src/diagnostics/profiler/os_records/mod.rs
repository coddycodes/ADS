//! Crash records the operating system kept for a previous run, read on the next start.

pub mod linux;
pub mod macos;
pub mod windows;

use std::time::Duration;

use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::sentinel::SessionRecord;

const HARVEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Frames kept from one OS crash record.
const MAX_FRAMES: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OsCrashRecord {
    pub source: &'static str,
    pub signal_or_code: String,
    pub module: Option<String>,
    pub frames: Vec<String>,
    pub raw_ref: String,
}

/// Finds the OS's record of `previous` crashing. Gives up after 5 s; a query process still
/// running then is killed. Every failure reads as "no record".
pub async fn harvest_previous_crash(previous: &SessionRecord) -> Option<OsCrashRecord> {
    match tokio::time::timeout(HARVEST_TIMEOUT, harvest(previous)).await {
        Ok(record) => record,
        Err(_) => {
            tracing::warn!("reading the OS crash records timed out");
            None
        }
    }
}

/// A crash of this executable the OS recorded for a process no profiler session covered,
/// such as an app relaunched by an update that died before its profiler started (#94).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnrecordedCrash {
    pub pid: u32,
    pub signal: i32,
    /// When the OS recorded it, in microseconds since the Unix epoch.
    pub time_micros: u64,
    pub exe: String,
}

/// The OS crash records since `since` of processes running `exe`, other than `skip_pids`,
/// or `None` when they could not be read. Only systemd-coredump keeps such a list;
/// elsewhere there are none.
pub async fn harvest_unrecorded_crashes(
    since: OffsetDateTime,
    exe: &str,
    skip_pids: &[u32],
) -> Option<Vec<UnrecordedCrash>> {
    #[cfg(target_os = "linux")]
    {
        match tokio::time::timeout(
            HARVEST_TIMEOUT,
            linux::unrecorded_crashes(since, exe, skip_pids),
        )
        .await
        {
            Ok(crashes) => crashes,
            Err(_) => {
                tracing::warn!("listing the OS crash records timed out");
                None
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (since, exe, skip_pids);
        Some(Vec::new())
    }
}

#[cfg(target_os = "linux")]
use linux::harvest;
#[cfg(target_os = "macos")]
use macos::harvest;
#[cfg(windows)]
use windows::harvest;

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
async fn harvest(_previous: &SessionRecord) -> Option<OsCrashRecord> {
    None
}

/// The facility that keeps crash records on this host, as an unclean-exit finding names it.
pub fn facility() -> &'static str {
    if cfg!(target_os = "linux") {
        let pattern = std::fs::read_to_string("/proc/sys/kernel/core_pattern").unwrap_or_default();
        if pattern.contains("systemd-coredump") {
            "systemd-coredump"
        } else {
            "no systemd-coredump"
        }
    } else if cfg!(windows) {
        "Windows Error Reporting"
    } else if cfg!(target_os = "macos") {
        "DiagnosticReports"
    } else {
        "none"
    }
}

/// Runs a read-only query tool and returns its standard output, or `None` if it could not
/// run. The child is killed if the caller stops waiting.
#[cfg(any(target_os = "linux", windows))]
async fn query(program: &str, args: &[&str]) -> Option<String> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    crate::process::configure_background_command(&mut command);
    crate::process::isolate_appimage_environment(&mut command);
    match command.output().await {
        Ok(output) => Some(String::from_utf8_lossy(&output.stdout).into_owned()),
        Err(error) => {
            tracing::debug!(program, %error, "OS crash record query did not run");
            None
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
fn started_at(previous: &SessionRecord) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(&previous.started_at, &Rfc3339).ok()
}

/// The module a frame names, in every format above: `symbol (module + 0x…)` or
/// `module + 0x…`.
pub(super) fn frame_module(frame: &str) -> Option<&str> {
    let location = frame
        .strip_suffix(')')
        .and_then(|frame| frame.rsplit_once(" ("))
        .map_or(frame, |(_, location)| location);
    location.split(" + ").next()
}

/// The executable's file name, as findings name it.
pub fn executable_name(path: &str) -> &str {
    file_name(path).trim_end_matches(" (deleted)")
}

/// The file name of a path from any OS, as the previous run recorded its executable.
pub(super) fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Whether two executable paths name the same file. A Linux executable replaced on disk
/// reads as `<path> (deleted)`.
fn same_executable(a: &str, b: &str) -> bool {
    let name = |path| file_name(path).trim_end_matches(" (deleted)");
    name(a).eq_ignore_ascii_case(name(b))
}
