//! systemd-coredump records and the user session's cgroup OOM-kill counter.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

use super::{MAX_FRAMES, OsCrashRecord, same_executable};

/// systemd-coredump's journal message ID.
#[cfg(target_os = "linux")]
const COREDUMP_MESSAGE_ID: &str = "MESSAGE_ID=fc2e22bc6ee647b6b90729ab34a250b1";

/// One entry of `coredumpctl --json=short list`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct CoredumpEntry {
    /// The journal record's `__REALTIME_TIMESTAMP`, in microseconds.
    pub time: u64,
    pub pid: u32,
    pub sig: i32,
    pub exe: String,
}

/// Unreadable output, or an entry missing a field, yields no entries for it.
pub fn parse_coredumpctl_list(json: &str) -> Vec<CoredumpEntry> {
    coredumpctl_list(json).unwrap_or_default()
}

/// The entries of `coredumpctl --json=short list` output, or `None` when it is not a JSON
/// list: a `coredumpctl` without `--json` (systemd before 250) prints nothing on stdout, and so
/// does one that found no coredump.
fn coredumpctl_list(json: &str) -> Option<Vec<CoredumpEntry>> {
    Some(
        serde_json::from_str::<Vec<Value>>(json)
            .ok()?
            .into_iter()
            .filter_map(|entry| serde_json::from_value(entry).ok())
            .collect(),
    )
}

/// The latest record in `journalctl -o json` coredump output (already narrowed to one pid)
/// whose executable has the same file name as `exe`: the record when `coredumpctl` cannot
/// list it. A process that reused the pid has another executable.
pub fn journal_record_for_exe(journal: &str, exe: &str) -> Option<OsCrashRecord> {
    journal
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .rfind(|record| {
            record
                .get("COREDUMP_EXE")
                .and_then(journal_text)
                .is_some_and(|recorded| same_executable(&recorded, exe))
        })
        .and_then(|record| parse_journal_message(&record))
}

/// The entries a `coredumpctl --json list` run answered, or `None` when it failed. It exits
/// 1 with "No coredumps found." when the window is empty, which is an answer; any other
/// failure, or output that is not a JSON list, is not.
pub fn coredumpctl_list_outcome(
    exit_code: Option<i32>,
    stdout: &str,
    stderr: &str,
) -> Option<Vec<CoredumpEntry>> {
    match exit_code {
        Some(0) => coredumpctl_list(stdout),
        Some(1) if stdout.trim().is_empty() && stderr.contains("No coredumps found") => {
            Some(Vec::new())
        }
        _ => None,
    }
}

/// The entries for processes running `exe`, this process's executable (see
/// [`runs_executable`]), other than `skip_pids`.
pub fn unrecorded_entries(
    entries: Vec<CoredumpEntry>,
    exe: &str,
    skip_pids: &[u32],
) -> Vec<super::UnrecordedCrash> {
    entries
        .into_iter()
        .filter(|entry| !skip_pids.contains(&entry.pid) && runs_executable(&entry.exe, exe))
        .map(|entry| super::UnrecordedCrash {
            pid: entry.pid,
            signal: entry.sig,
            time_micros: entry.time,
            exe: entry.exe,
        })
        .collect()
}

/// Whether a crashed process ran `exe`, both as `/proc/<pid>/exe` names them: the same path,
/// or, when `exe` is in an AppImage's mount (`<dir>/.mount_<id>/<rest>`, a new `<id>` every
/// run), `<rest>` in another mount in `<dir>` or at `/<rest>`, which is how the kernel names it
/// once that mount was detached, as stopping an updated app's unit does. A file of the same
/// name elsewhere, such as an extracted AppImage run by hand, is another program (#110); a
/// system install at exactly `/<rest>` cannot be told from a detached mount.
fn runs_executable(crashed: &str, exe: &str) -> bool {
    let (crashed, exe) = (
        crashed.trim_end_matches(" (deleted)"),
        exe.trim_end_matches(" (deleted)"),
    );
    crashed == exe
        || appimage_mount(exe).is_some_and(|(dir, rest)| {
            appimage_mount(crashed) == Some((dir, rest)) || crashed.strip_prefix('/') == Some(rest)
        })
}

/// `<dir>` and `<rest>` of an executable at `<dir>/.mount_<id>/<rest>`.
fn appimage_mount(exe: &str) -> Option<(&str, &str)> {
    let (dir, mounted) = exe.split_once("/.mount_")?;
    Some((dir, mounted.split_once('/')?.1))
}

#[cfg(target_os = "linux")]
pub(super) async fn unrecorded_crashes(
    since: time::OffsetDateTime,
    exe: &str,
    skip_pids: &[u32],
) -> Option<Vec<super::UnrecordedCrash>> {
    let since = format!("--since=@{}", since.unix_timestamp());
    let output = super::query_output(
        "coredumpctl",
        &["--json=short", "--no-pager", "list", &since],
    )
    .await?;
    let list = coredumpctl_list_outcome(
        output.status.code(),
        &String::from_utf8_lossy(&output.stdout),
        &String::from_utf8_lossy(&output.stderr),
    )?;
    Some(unrecorded_entries(list, exe, skip_pids))
}

/// The latest entry for `pid` whose executable has the same file name as `exe`.
pub fn matching_entry(entries: Vec<CoredumpEntry>, pid: u32, exe: &str) -> Option<CoredumpEntry> {
    entries
        .into_iter()
        .rev()
        .find(|entry| entry.pid == pid && same_executable(&entry.exe, exe))
}

/// One `journalctl -o json` coredump entry. `MESSAGE` may be a string or, when it is not
/// UTF-8, an array of bytes; without `journalctl -a` a long one is `null`.
/// The record in `journalctl -o json` output that `entry` lists: coredumpctl's `time` is the
/// journal record's `__REALTIME_TIMESTAMP`. A process that reused the pid has its own record.
pub fn journal_record(journal: &str, entry: &CoredumpEntry) -> Option<OsCrashRecord> {
    let time = entry.time.to_string();
    journal
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|record| record.get("__REALTIME_TIMESTAMP").and_then(Value::as_str) == Some(&time))
        .and_then(|record| parse_journal_message(&record))
}

pub fn parse_journal_message(entry: &Value) -> Option<OsCrashRecord> {
    let field = |name: &str| entry.get(name).and_then(journal_text);
    let message = field("MESSAGE")?;
    let signal_or_code = field("COREDUMP_SIGNAL_NAME")
        .or_else(|| field("COREDUMP_SIGNAL").map(|signal| format!("signal {signal}")))?;
    // The crashing thread's stack comes first; it ends at the blank line before the next one.
    let frames: Vec<String> = message
        .lines()
        .skip_while(|line| !line.starts_with("Stack trace of thread"))
        .skip(1)
        .map_while(frame)
        .take(MAX_FRAMES)
        .map(str::to_owned)
        .collect();
    let module = frames
        .first()
        .and_then(|first| super::frame_module(first))
        .map(str::to_owned);
    let pid = field("COREDUMP_PID").unwrap_or_default();
    let timestamp = field("COREDUMP_TIMESTAMP").unwrap_or_default();
    Some(OsCrashRecord {
        source: "coredump",
        signal_or_code,
        module,
        frames,
        raw_ref: format!("coredumpctl info COREDUMP_PID={pid} COREDUMP_TIMESTAMP={timestamp}"),
    })
}

fn journal_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(bytes) => {
            let bytes = bytes
                .iter()
                .map(|byte| byte.as_u64().and_then(|byte| u8::try_from(byte).ok()))
                .collect::<Option<Vec<u8>>>()?;
            Some(String::from_utf8_lossy(&bytes).into_owned())
        }
        _ => None,
    }
}

/// `#3  0x000063e1cb1d4a3a symbol (module + 0x98dda3a)` without the number and the
/// randomized address: `symbol (module + 0x98dda3a)`.
fn frame(line: &str) -> Option<&str> {
    let numbered = line.trim_start().strip_prefix('#')?;
    let (_, rest) = numbered.split_once(char::is_whitespace)?;
    let (_, rest) = rest.trim_start().split_once(' ')?;
    Some(rest)
}

/// systemd-coredump's journal records matching `pid` since `since`, on `boot` when given.
#[cfg(target_os = "linux")]
async fn coredump_journal(since: &str, pid: &str, boot: Option<&str>) -> Option<String> {
    // `-a`: a backtrace longer than 4096 bytes is `null` without it.
    let mut args = vec![
        "-a",
        "-o",
        "json",
        "--no-pager",
        "--output-fields=MESSAGE,COREDUMP_PID,COREDUMP_SIGNAL,COREDUMP_SIGNAL_NAME,COREDUMP_TIMESTAMP,COREDUMP_EXE",
        since,
        COREDUMP_MESSAGE_ID,
        pid,
    ];
    args.extend(boot);
    super::query("journalctl", &args).await
}

/// The journal match for the boot a run recorded: `/proc/sys/kernel/random/boot_id` without
/// its dashes. After a reboot the pid may belong to another process that crashed since.
pub fn boot_match(boot_id: &str) -> Option<String> {
    let id: String = boot_id.chars().filter(|c| *c != '-').collect();
    (id.len() == 32 && id.chars().all(|c| c.is_ascii_hexdigit())).then(|| format!("_BOOT_ID={id}"))
}

/// `oom_kill N` from a cgroup v2 `memory.events` file.
pub fn read_oom_kill(user_cgroup_events: &Path) -> Option<u64> {
    std::fs::read_to_string(user_cgroup_events)
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("oom_kill ")?.trim().parse().ok())
}

/// The `memory.events` of the `user@<uid>.service` cgroup that holds this process, given
/// `/proc/self/cgroup`. That cgroup outlives the process, and its counters are hierarchical.
pub fn user_service_events(proc_self_cgroup: &str) -> Option<PathBuf> {
    let path = proc_self_cgroup
        .lines()
        .find_map(|line| line.strip_prefix("0::"))?;
    let mut cgroup = PathBuf::from("/sys/fs/cgroup");
    for part in path.split('/').filter(|part| !part.is_empty()) {
        cgroup.push(part);
        if part.starts_with("user@") && part.ends_with(".service") {
            return Some(cgroup.join("memory.events"));
        }
    }
    None
}

/// The systemd user unit a process runs in, from its `/proc/<pid>/cgroup`: the innermost
/// `.service` or `.scope` below the user manager, such as `coddy-studio-desktop.service` for a run
/// started with `systemd-run --user --unit=coddy-studio-desktop`. `None` outside a user unit.
pub fn unit_of(proc_self_cgroup: &str) -> Option<String> {
    let path = proc_self_cgroup
        .lines()
        .find_map(|line| line.strip_prefix("0::"))?;
    let mut parts = path.split('/').filter(|part| !part.is_empty());
    parts
        .by_ref()
        .find(|part| part.starts_with("user@") && part.ends_with(".service"))?;
    parts
        .rfind(|part| part.ends_with(".service") || part.ends_with(".scope"))
        .filter(|unit| *unit != "init.scope")
        .map(ToOwned::to_owned)
}

/// systemd's own messages about a unit ending, from `journalctl -o cat` output: how its main
/// process exited, the result it failed with, and what it consumed. The last five, oldest
/// first.
pub fn unit_exit_lines_of(journal: &str) -> Vec<String> {
    const ENDINGS: [&str; 4] = [
        "Main process exited",
        "Failed with result",
        "Killing process",
        "Consumed ",
    ];
    let lines: Vec<String> = journal
        .lines()
        .map(str::trim)
        .filter(|line| ENDINGS.iter().any(|ending| line.contains(ending)))
        .map(ToOwned::to_owned)
        .collect();
    lines[lines.len().saturating_sub(5)..].to_vec()
}

/// [`unit_exit_lines_of`] for `unit` since `since`, from the user journal. Empty when the
/// journal could not be read.
#[cfg(target_os = "linux")]
pub(super) async fn unit_exit_lines(unit: &str, since: time::OffsetDateTime) -> Vec<String> {
    let unit = format!("--unit={unit}");
    let since = format!("--since=@{}", since.unix_timestamp());
    let args = [
        "--user",
        &unit,
        "--identifier=systemd",
        &since,
        "--output=cat",
        "--no-pager",
        "--quiet",
    ];
    super::query("journalctl", &args)
        .await
        .map(|journal| unit_exit_lines_of(&journal))
        .unwrap_or_default()
}

/// Looks up a systemd-coredump entry for the previous run's pid and executable, then its
/// journal record with the backtrace. When `coredumpctl --json` is missing (systemd before
/// 250) or fails, the journal is queried for the pid directly. Neither tool reads as no record.
#[cfg(target_os = "linux")]
pub(super) async fn harvest(previous: &super::SessionRecord) -> Option<OsCrashRecord> {
    let since = format!("--since=@{}", super::started_at(previous)?.unix_timestamp());
    let mut list_args = vec!["--json=short", "--no-pager", "list", &since];
    let boot = previous.boot_id.as_deref().and_then(boot_match);
    list_args.extend(boot.as_deref());
    let list = super::query("coredumpctl", &list_args)
        .await
        .and_then(|list| coredumpctl_list(&list));
    let Some(list) = list else {
        let pid = format!("COREDUMP_PID={}", previous.pid);
        let journal = coredump_journal(&since, &pid, boot.as_deref()).await?;
        return journal_record_for_exe(&journal, &previous.exe);
    };
    let entry = matching_entry(list, previous.pid, &previous.exe)?;
    let pid = format!("COREDUMP_PID={}", entry.pid);
    let journal = coredump_journal(&since, &pid, None)
        .await
        .unwrap_or_default();
    journal_record(&journal, &entry).or_else(|| {
        Some(OsCrashRecord {
            source: "coredump",
            signal_or_code: format!("signal {}", entry.sig),
            module: None,
            frames: Vec::new(),
            raw_ref: format!("coredumpctl info {}", entry.pid),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::{unit_exit_lines_of, unit_of};

    #[test]
    fn the_unit_is_the_innermost_user_service_or_scope() {
        let cgroup = |path: &str| format!("0::{path}\n");
        assert_eq!(
            unit_of(&cgroup(
                "/user.slice/user-1000.slice/user@1000.service/app.slice/coddy-studio-desktop.service"
            ))
            .as_deref(),
            Some("coddy-studio-desktop.service")
        );
        assert_eq!(
            unit_of(&cgroup(
                "/user.slice/user-1000.slice/user@1000.service/app.slice/app-x.scope"
            ))
            .as_deref(),
            Some("app-x.scope")
        );
        assert_eq!(
            unit_of(&cgroup(
                "/user.slice/user-1000.slice/user@1000.service/init.scope"
            )),
            None
        );
        assert_eq!(unit_of(&cgroup("/system.slice/coddy-studio.service")), None);
        assert_eq!(unit_of("garbage"), None);
    }

    #[test]
    fn unit_exit_lines_keep_systemds_account_of_the_ending() {
        let journal = "Started [systemd-run] /opt/coddy-studio.appimage.\n\
            coddy-studio-desktop.service: Main process exited, code=killed, status=9/KILL\n\
            coddy-studio-desktop.service: Failed with result 'signal'.\n\
            coddy-studio-desktop.service: Consumed 1h CPU time, 8G memory peak.\n\
            Started [systemd-run] /opt/coddy-studio.appimage.\n";
        assert_eq!(
            unit_exit_lines_of(journal),
            vec![
                "coddy-studio-desktop.service: Main process exited, code=killed, status=9/KILL",
                "coddy-studio-desktop.service: Failed with result 'signal'.",
                "coddy-studio-desktop.service: Consumed 1h CPU time, 8G memory peak.",
            ]
        );
        assert!(unit_exit_lines_of("").is_empty());
    }

    use super::*;

    const LIST: &str = include_str!("../../../../tests/fixtures/profiler/coredumpctl-list.json");
    const JOURNAL: &str = include_str!("../../../../tests/fixtures/profiler/journal-coredump.json");

    #[test]
    fn a_failed_coredump_listing_is_not_an_empty_one() {
        assert_eq!(
            coredumpctl_list_outcome(Some(1), "", "No coredumps found.\n"),
            Some(Vec::new())
        );
        assert_eq!(
            coredumpctl_list_outcome(Some(1), "", "Failed to access journal: Permission denied"),
            None
        );
        assert_eq!(coredumpctl_list_outcome(Some(0), "not json", ""), None);
        assert_eq!(coredumpctl_list_outcome(None, "", ""), None);
    }

    /// Only this app's executable counts, not another binary of the same name, such as an
    /// extracted AppImage run by hand (#110). An AppImage mounts at a new `.mount_<id>` every
    /// run, and a crash recorded after its mount was detached names `/usr/bin/…`.
    #[test]
    fn unrecorded_entries_keep_this_executable_other_than_skipped_pids() {
        let entry = |pid, exe: &str| CoredumpEntry {
            time: 1_000 + u64::from(pid),
            pid,
            sig: 7,
            exe: exe.to_owned(),
        };
        let pids = |exe: &str, entries| {
            unrecorded_entries(entries, exe, &[13])
                .iter()
                .map(|crash| crash.pid)
                .collect::<Vec<_>>()
        };
        let appimage = "/tmp/.mount_bibcodcDBndf/usr/bin/coddy-studio-desktop";
        assert_eq!(
            pids(
                appimage,
                vec![
                    entry(10, "/tmp/.mount_bibcodEBCJgP/usr/bin/coddy-studio-desktop"),
                    entry(11, "/usr/bin/coddy-studio-desktop"),
                    entry(
                        12,
                        "/home/u/.cache/leak/appimage/squashfs-root/usr/bin/coddy-studio-desktop"
                    ),
                    entry(13, "/tmp/.mount_bibcodEBCJgP/usr/bin/coddy-studio-desktop"),
                    entry(14, "/usr/bin/firefox"),
                    entry(15, "/var/tmp/.mount_bibcodX/usr/bin/coddy-studio-desktop"),
                    entry(16, appimage),
                ]
            ),
            [10, 11, 16]
        );
        let built = "/home/u/src/coddy-studio/target/release/coddy-studio-desktop";
        assert_eq!(
            pids(
                built,
                vec![
                    entry(20, &format!("{built} (deleted)")),
                    entry(21, "/usr/bin/coddy-studio-desktop"),
                    entry(22, "/tmp/.mount_bibcodEBCJgP/usr/bin/coddy-studio-desktop"),
                    entry(23, built),
                ]
            ),
            [20, 23]
        );
        let crashes = unrecorded_entries(vec![entry(30, built)], built, &[]);
        assert_eq!((crashes[0].signal, crashes[0].exe.as_str()), (7, built));
    }

    #[test]
    fn coredumpctl_list_picks_the_entry_with_matching_pid_and_exe() {
        let entries = parse_coredumpctl_list(LIST);
        assert_eq!(entries.len(), 2);
        let entry = matching_entry(
            entries.clone(),
            1_732_134,
            "~/bin/coddy_studio_server-fe771ab9e6f4b1bc",
        )
        .expect("matching entry");
        assert_eq!((entry.pid, entry.sig), (1_732_134, 6));
        assert!(matching_entry(entries.clone(), 1_732_134, "coddy-studio").is_none());
        assert!(matching_entry(entries, 1966, "coddy_studio_server-fe771ab9e6f4b1bc").is_none());
        assert!(parse_coredumpctl_list("").is_empty());
    }

    #[test]
    fn journal_message_yields_the_signal_and_the_crashing_thread_frames() {
        let entry: Value = serde_json::from_str(JOURNAL).unwrap();
        let record = parse_journal_message(&entry).expect("record");
        assert_eq!(record.source, "coredump");
        assert_eq!(record.signal_or_code, "SIGABRT");
        assert_eq!(record.module.as_deref(), Some("libc.so.6"));
        assert_eq!(record.frames.len(), 8, "only the first thread's frames");
        assert_eq!(record.frames[1], "raise (libc.so.6 + 0x3e5d0)");
        assert!(record.raw_ref.contains("1732134"));
    }

    /// A process that reused the pid and crashed later has its own journal record.
    #[test]
    fn journal_record_is_the_one_the_list_entry_points_at() {
        let entry = matching_entry(
            parse_coredumpctl_list(LIST),
            1_732_134,
            "coddy_studio_server-fe771ab9e6f4b1bc",
        )
        .unwrap();
        let ours: Value = serde_json::from_str(JOURNAL).unwrap();
        let mut reused = ours.clone();
        reused["__REALTIME_TIMESTAMP"] = "1790707099000000".into();
        reused["COREDUMP_SIGNAL_NAME"] = "SIGSEGV".into();
        let journal = format!("{ours}\n{reused}\n");
        let record = journal_record(&journal, &entry).expect("record");
        assert_eq!(record.signal_or_code, "SIGABRT");
        assert!(journal_record(&format!("{reused}\n"), &entry).is_none());
    }

    /// Without `coredumpctl --json` (systemd 249) the journal, queried for the pid, is the
    /// record: the latest one for the same executable.
    #[test]
    fn the_journal_alone_finds_the_record_when_coredumpctl_cannot_list_it() {
        assert_eq!(
            coredumpctl_list(""),
            None,
            "an unsupported --json prints nothing"
        );
        assert_eq!(coredumpctl_list("No coredumps found."), None);
        assert_eq!(coredumpctl_list("[]"), Some(Vec::new()));

        let ours: Value = serde_json::from_str(JOURNAL).unwrap();
        let mut reused = ours.clone();
        reused["COREDUMP_EXE"] = "/usr/bin/other".into();
        reused["COREDUMP_SIGNAL_NAME"] = "SIGSEGV".into();
        let journal = format!("{ours}\n{reused}\n");
        let record = journal_record_for_exe(&journal, "~/bin/coddy_studio_server-fe771ab9e6f4b1bc")
            .expect("our executable's record");
        assert_eq!(record.signal_or_code, "SIGABRT");
        assert_eq!(record.frames.len(), 8);
        assert!(journal_record_for_exe(&journal, "coddy-studio").is_none());
        assert!(journal_record_for_exe("", "coddy_studio_server-fe771ab9e6f4b1bc").is_none());
    }

    #[test]
    fn journal_message_given_as_bytes_decodes() {
        let mut entry: Value = serde_json::from_str(JOURNAL).unwrap();
        let message = entry["MESSAGE"].as_str().unwrap().as_bytes().to_vec();
        entry["MESSAGE"] = serde_json::json!(message);
        entry
            .as_object_mut()
            .unwrap()
            .remove("COREDUMP_SIGNAL_NAME");
        let record = parse_journal_message(&entry).expect("record");
        assert_eq!(record.frames.len(), 8);
        assert_eq!(record.signal_or_code, "signal 6");
        entry["MESSAGE"] = Value::Null;
        assert!(parse_journal_message(&entry).is_none());
    }

    #[test]
    fn the_previous_boot_becomes_a_journal_match() {
        assert_eq!(
            boot_match("c941105e-2424-424d-aac3-e8070bb2bb52").as_deref(),
            Some("_BOOT_ID=c941105e2424424daac3e8070bb2bb52")
        );
        assert_eq!(
            boot_match("1791540000"),
            None,
            "a boot time is not a journal boot ID"
        );
        assert_eq!(boot_match("another-boot"), None);
    }

    #[test]
    fn oom_kill_comes_from_the_user_service_cgroup() {
        assert_eq!(
            user_service_events(
                "0::/user.slice/user-1000.slice/user@1000.service/app.slice/x.scope\n"
            ),
            Some(PathBuf::from(
                "/sys/fs/cgroup/user.slice/user-1000.slice/user@1000.service/memory.events"
            ))
        );
        assert_eq!(
            user_service_events("0::/system.slice/coddy-studio.service\n"),
            None
        );
        let dir = tempfile::tempdir().unwrap();
        let events = dir.path().join("memory.events");
        std::fs::write(
            &events,
            "low 0\nhigh 0\nmax 12\noom 1\noom_kill 3\noom_group_kill 0\n",
        )
        .unwrap();
        assert_eq!(read_oom_kill(&events), Some(3));
        assert_eq!(read_oom_kill(&dir.path().join("missing")), None);
    }
}
