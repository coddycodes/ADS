#[cfg(windows)]
use std::{
    env,
    fs::OpenOptions,
    io::Write,
    process::{self, Command},
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(windows)]
use serde_json::json;
#[cfg(windows)]
use windows_sys::{
    Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation},
    Win32::{
        Foundation::{CloseHandle, FILETIME, WAIT_ABANDONED_0, WAIT_OBJECT_0},
        System::Threading::{
            CreateMutexW, GetProcessTimes, INFINITE, OpenProcess, PROCESS_BASIC_INFORMATION,
            PROCESS_QUERY_INFORMATION, ReleaseMutex, WaitForSingleObject,
        },
    },
};

#[cfg(windows)]
fn process_identity(pid: u32) -> std::io::Result<(u32, u64)> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_INFORMATION, 0, pid);
        if handle.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let result = (|| {
            let mut basic = PROCESS_BASIC_INFORMATION::default();
            let status = NtQueryInformationProcess(
                handle,
                ProcessBasicInformation,
                std::ptr::from_mut(&mut basic).cast(),
                u32::try_from(std::mem::size_of::<PROCESS_BASIC_INFORMATION>()).unwrap(),
                std::ptr::null_mut(),
            );
            if status < 0 {
                return Err(std::io::Error::other(format!(
                    "NtQueryInformationProcess failed with NTSTATUS {status:#x}"
                )));
            }
            let mut creation = FILETIME::default();
            let mut exit = FILETIME::default();
            let mut kernel = FILETIME::default();
            let mut user = FILETIME::default();
            if GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) == 0 {
                return Err(std::io::Error::last_os_error());
            }
            let started_at =
                (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
            Ok((
                u32::try_from(basic.InheritedFromUniqueProcessId)
                    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?,
                started_at,
            ))
        })();
        CloseHandle(handle);
        result
    }
}

#[cfg(windows)]
fn main() {
    let real_git = env::var_os("CODDY_STUDIO_VCS_MEASURE_REAL_GIT").expect("missing real Git path");
    let log_path = env::var_os("CODDY_STUDIO_VCS_MEASURE_GIT_LOG").expect("missing Git log path");
    let args = env::args_os().skip(1).collect::<Vec<_>>();
    let pid = process::id();
    let (parent_pid, started_at) = process_identity(pid).expect("query shim identity");
    let (_, parent_started_at) = process_identity(parent_pid).expect("query parent identity");
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after epoch")
        .as_millis();
    let record = json!({
        "timestampMs": timestamp_ms,
        "pid": pid,
        "startedAt": started_at.to_string(),
        "parentPid": parent_pid,
        "parentStartedAt": parent_started_at.to_string(),
        "args": args.iter().map(|value| value.to_string_lossy()).collect::<Vec<_>>()
    });
    let mutex_name = "Local\\CoddyStudioVcsMeasureGitLogMutex"
        .encode_utf16()
        .chain(Some(0))
        .collect::<Vec<_>>();
    unsafe {
        let mutex = CreateMutexW(std::ptr::null(), 0, mutex_name.as_ptr());
        assert!(!mutex.is_null(), "create Git log mutex");
        let waited = WaitForSingleObject(mutex, INFINITE);
        assert!(
            matches!(waited, WAIT_OBJECT_0 | WAIT_ABANDONED_0),
            "acquire Git log mutex"
        );
        let mut log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log_path)
            .expect("open Git launch log");
        let line = format!("{record}\n");
        log.write_all(line.as_bytes())
            .expect("append Git launch record");
        drop(log);
        assert_ne!(ReleaseMutex(mutex), 0, "release Git log mutex");
        CloseHandle(mutex);
    }

    let status = Command::new(real_git)
        .args(args)
        .status()
        .expect("launch real Git");
    process::exit(status.code().unwrap_or(1));
}

#[cfg(not(windows))]
fn main() {
    eprintln!("measure_vcs_git_shim is available only on Windows");
    std::process::exit(1);
}
