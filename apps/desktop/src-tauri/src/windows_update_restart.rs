//! Keeps a Windows update restart on the same data root and backend port.
//!
//! Passive NSIS relaunches the installed executable with `RunAsUser`. When the
//! installer is elevated, that helper uses `CreateProcessWithTokenW` and a null
//! environment block, so the new process does not receive `CODDY_STUDIO_HOME` or
//! `CODDY_STUDIO_PORT` from the process that started the installer. Remote clients
//! still dial the pre-update endpoint. The updating process publishes those two
//! variables, and the replacement applies whichever it does not already have.
//! A detached waiter starts the installed executable if the installer exits
//! without relaunching it.

#![cfg_attr(
    all(not(windows), not(test)),
    expect(
        dead_code,
        reason = "Windows update restart handoff is inactive on this platform"
    )
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const HANDOFF_FILE_NAME: &str = "coddy-studio-desktop-restart.json";
const HANDOFF_VERSION: u32 = 1;
/// The passive installer returns before NSIS finishes. The observed Windows
/// remote install replaced the executable about three minutes after the request.
const HANDOFF_TTL_MS: u64 = 10 * 60 * 1000;
const HANDOFF_FUTURE_SKEW_MS: u64 = 60 * 1000;
const HANDOFF_ENV_KEYS: &[&str] = &["CODDY_STUDIO_HOME", "CODDY_STUDIO_PORT"];
/// Parent stay-alive bound. The updater exits immediately after `ShellExecuteW`.
const WAITER_PARENT_SECONDS: u32 = 240;
/// Installer stay-alive bound. A slow NSIS run outlived a three-minute poll.
const WAITER_INSTALL_SECONDS: u32 = 360;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct RestartHandoff {
    version: u32,
    exe: String,
    created_at_ms: u64,
    env: BTreeMap<String, String>,
}

pub(crate) fn handoff_path(temp_dir: &Path) -> PathBuf {
    temp_dir.join(HANDOFF_FILE_NAME)
}

pub(crate) fn capture_handoff_env(
    value: impl Fn(&str) -> Option<String>,
) -> BTreeMap<String, String> {
    HANDOFF_ENV_KEYS
        .iter()
        .filter_map(|key| {
            let raw = value(key)?;
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.chars().any(|character| character.is_control()) {
                return None;
            }
            Some(((*key).to_owned(), trimmed.to_owned()))
        })
        .collect()
}

/// The variables a fresh handoff for `current_exe` sets, or `None` when the
/// handoff is not this executable's or not fresh. `Some` with no variables is
/// still this restart's handoff, already applied by inheritance.
pub(crate) fn assignments_from_handoff(
    bytes: &[u8],
    current_exe: &str,
    now_ms: u64,
    already_set: impl Fn(&str) -> bool,
) -> Option<Vec<(String, String)>> {
    let handoff = serde_json::from_slice::<RestartHandoff>(bytes).ok()?;
    if handoff.version != HANDOFF_VERSION || handoff.exe != current_exe {
        return None;
    }
    if now_ms.saturating_add(HANDOFF_FUTURE_SKEW_MS) < handoff.created_at_ms {
        return None;
    }
    if now_ms.saturating_sub(handoff.created_at_ms) > HANDOFF_TTL_MS {
        return None;
    }
    let assignments = handoff
        .env
        .into_iter()
        .filter(|(key, value)| {
            HANDOFF_ENV_KEYS.contains(&key.as_str())
                && !value.is_empty()
                && !value.chars().any(|character| character.is_control())
                && !already_set(key)
        })
        .collect();
    Some(assignments)
}

pub(crate) fn handoff_is_expired(bytes: &[u8], now_ms: u64) -> bool {
    let Ok(handoff) = serde_json::from_slice::<RestartHandoff>(bytes) else {
        return true;
    };
    now_ms.saturating_sub(handoff.created_at_ms) > HANDOFF_TTL_MS
        || now_ms.saturating_add(HANDOFF_FUTURE_SKEW_MS) < handoff.created_at_ms
}

/// The waiter's environment variable that names the executable to start.
/// `cmd.exe` reads a batch file in the console code page, so a path written
/// into the script would lose non-ASCII characters; an environment variable
/// stays UTF-16.
const WAITER_APP_VAR: &str = "CODDY_STUDIO_RESTART_APP";

/// Builds the waiter script, ASCII only. `None` when the executable path cannot
/// be passed to `start` safely.
///
/// The app counts as relaunched only with a window: the profiler's `profiler
/// mcp` server and crash monitor run the same executable without one.
///
/// The published NSIS basename ends in `-setup.exe`, but `tauri-plugin-updater`
/// writes those bytes to `{package}-{version}-installer.exe` before
/// `ShellExecuteW`. The running image is therefore
/// `coddy-studio-desktop-<version>-installer.exe`. Waiting only for `-setup.exe`
/// treats that install as finished and starts the app while NSIS is replacing it.
pub(crate) fn restart_waiter_script(parent_pid: u32, executable: &str) -> Option<String> {
    if executable.is_empty()
        || executable
            .chars()
            .any(|character| matches!(character, '"' | '\n' | '\r' | '%' | '&' | '|' | '<' | '>'))
    {
        return None;
    }
    Some(format!(
        "@echo off\r\n\
         setlocal EnableExtensions\r\n\
         set \"PID={parent_pid}\"\r\n\
         set /a LEFT={WAITER_PARENT_SECONDS}\r\n\
         :while_parent\r\n\
         tasklist /FI \"PID eq %PID%\" /NH 2>nul | findstr /C:\"%PID%\" >nul\r\n\
         if errorlevel 1 goto parent_gone\r\n\
         set /a LEFT-=1\r\n\
         if %LEFT% LEQ 0 exit /b 0\r\n\
         ping -n 2 127.0.0.1 >nul\r\n\
         goto while_parent\r\n\
         :parent_gone\r\n\
         set /a QUIET=0\r\n\
         set /a LEFT={WAITER_INSTALL_SECONDS}\r\n\
         :while_install\r\n\
         tasklist /FO CSV /NH 2>nul | findstr /I /C:\"-setup.exe\" /C:\"-installer.exe\" >nul\r\n\
         if not errorlevel 1 (\r\n\
           set /a QUIET=0\r\n\
           set /a LEFT-=1\r\n\
           if %LEFT% LEQ 0 exit /b 0\r\n\
           ping -n 2 127.0.0.1 >nul\r\n\
           goto while_install\r\n\
         )\r\n\
         tasklist /FI \"IMAGENAME eq coddy-studio-desktop.exe\" /FI \"WINDOWTITLE ne N/A\" /NH 2>nul | findstr /I /C:\"coddy-studio-desktop.exe\" >nul\r\n\
         if not errorlevel 1 exit /b 0\r\n\
         set /a QUIET+=1\r\n\
         if %QUIET% LSS 3 (\r\n\
           set /a LEFT-=1\r\n\
           if %LEFT% LEQ 0 exit /b 0\r\n\
           ping -n 2 127.0.0.1 >nul\r\n\
           goto while_install\r\n\
         )\r\n\
         start \"\" \"%{WAITER_APP_VAR}%\"\r\n\
         exit /b 0\r\n"
    ))
}

pub(crate) fn encode_handoff(
    executable: &str,
    now_ms: u64,
    env: BTreeMap<String, String>,
) -> Vec<u8> {
    let handoff = RestartHandoff {
        version: HANDOFF_VERSION,
        exe: executable.to_owned(),
        created_at_ms: now_ms,
        env,
    };
    serde_json::to_vec(&handoff).unwrap_or_default()
}

#[cfg(windows)]
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// A handoff this launch adopted. [`ClaimedHandoff::release`] deletes it once
/// this launch owns the single instance, so a later ordinary launch keeps its
/// own defaults while a duplicate launch rejected meanwhile leaves it for the
/// installer's replacement.
pub(crate) struct ClaimedHandoff(Option<PathBuf>);

impl ClaimedHandoff {
    pub(crate) fn release(self) {
        if let Some(path) = self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Applies a fresh handoff before any worker thread exists.
pub(crate) fn adopt_restart_handoff() -> ClaimedHandoff {
    ClaimedHandoff(claim_handoff())
}

#[cfg(windows)]
fn claim_handoff() -> Option<PathBuf> {
    let path = handoff_path(&std::env::temp_dir());
    let bytes = std::fs::read(&path).ok()?;
    let now = now_ms();
    if handoff_is_expired(&bytes, now) {
        let _ = std::fs::remove_file(&path);
        return None;
    }
    let current_exe = std::env::current_exe().ok()?;
    let assignments =
        assignments_from_handoff(&bytes, &current_exe.to_string_lossy(), now, |key| {
            std::env::var_os(key).is_some()
        })?;
    // SAFETY: this runs at the start of `run`, before Tauri creates threads.
    // No other thread can read the environment concurrently.
    for (key, value) in assignments {
        unsafe { std::env::set_var(&key, value) };
    }
    Some(path)
}

#[cfg(not(windows))]
fn claim_handoff() -> Option<PathBuf> {
    None
}

/// The detached waiter [`prepare_restart_handoff`] started, if any.
pub(crate) struct RestartWaiter {
    #[cfg(all(windows, not(test)))]
    process: Option<std::os::windows::io::OwnedHandle>,
}

impl RestartWaiter {
    /// The installer did not start: stop the waiter, which would otherwise
    /// start this version again once the user quits, and drop the handoff.
    pub(crate) fn cancel(self) {
        #[cfg(all(windows, not(test)))]
        {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::System::Threading::{TerminateProcess, WaitForSingleObject};

            if let Some(process) = self.process {
                // SAFETY: `process` is the waiter's live process handle, owned here.
                unsafe {
                    TerminateProcess(process.as_raw_handle(), 1);
                    WaitForSingleObject(process.as_raw_handle(), 5_000);
                }
            }
            let _ = std::fs::remove_file(handoff_path(&std::env::temp_dir()));
        }
    }
}

/// Publishes the handoff and a detached waiter. Failures must not block install.
pub(crate) fn prepare_restart_handoff() -> RestartWaiter {
    #[cfg(all(windows, not(test)))]
    let process = publish_restart_handoff().unwrap_or_else(|error| {
        tracing::warn!(%error, "windows update restart handoff was not published");
        None
    });
    RestartWaiter {
        #[cfg(all(windows, not(test)))]
        process,
    }
}

#[cfg(all(windows, not(test)))]
fn publish_restart_handoff() -> Result<Option<std::os::windows::io::OwnedHandle>, String> {
    let executable_path = std::env::current_exe()
        .map_err(|error| format!("could not resolve the installed executable: {error}"))?;
    let executable = executable_path.to_string_lossy().to_string();
    let env = capture_handoff_env(|key| std::env::var(key).ok());
    if !env.is_empty()
        && let Err(error) = write_restart_handoff(&executable, env)
    {
        tracing::warn!(%error, "windows update restart handoff file was not published");
    }
    let Some(script) = restart_waiter_script(std::process::id(), &executable) else {
        return Ok(None);
    };
    let script_path = std::env::temp_dir().join("coddy-studio-desktop-restart-waiter.cmd");
    std::fs::write(&script_path, script)
        .map_err(|error| format!("could not write the restart waiter: {error}"))?;
    spawn_waiter(&script_path, &executable_path)
        .map(Some)
        .map_err(|error| format!("could not start the restart waiter: {error}"))
}

/// Starts `cmd.exe` on the waiter script without inheriting any handle.
///
/// `std::process::Command` always lets the child inherit every inheritable
/// handle. One of those can be the write end of a pipe some ancestor reads, such
/// as a launching script or test harness. The waiter, and the copy it starts,
/// would then keep that reader waiting until they exit. The script redirects
/// all its output, so it needs no standard handles.
#[cfg(all(windows, not(test)))]
fn spawn_waiter(
    script_path: &Path,
    executable_path: &Path,
) -> std::io::Result<std::os::windows::io::OwnedHandle> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
    use windows_sys::Win32::System::Threading::{
        CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW,
        CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DETACHED_PROCESS, PROCESS_INFORMATION,
        STARTUPINFOW,
    };

    let mut system = vec![0u16; 260];
    // SAFETY: the buffer holds `system.len()` UTF-16 units.
    let length = unsafe { GetSystemDirectoryW(system.as_mut_ptr(), system.len() as u32) } as usize;
    if length == 0 || length >= system.len() {
        return Err(std::io::Error::last_os_error());
    }
    system.truncate(length);
    system.extend("\\cmd.exe".encode_utf16());
    let script: Vec<u16> = script_path.as_os_str().encode_wide().collect();
    let mut application = system.clone();
    application.push(0);
    let environment = waiter_environment(
        std::env::vars_os()
            .map(|(name, value)| (name.encode_wide().collect(), value.encode_wide().collect())),
        &WAITER_APP_VAR.encode_utf16().collect::<Vec<_>>(),
        executable_path.as_os_str().encode_wide().collect(),
    );
    let base = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW;
    let mut failure = None;
    // A job that forbids breakaway refuses the first attempt.
    for flags in [base | CREATE_BREAKAWAY_FROM_JOB, base] {
        let mut command_line = waiter_command_line(&system, &script);
        // SAFETY: an all-zero STARTUPINFOW with `cb` set asks for no standard handles.
        let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
        startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        // SAFETY: an all-zero PROCESS_INFORMATION is the expected output buffer.
        let mut information: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: every string is NUL-terminated UTF-16, the command line is
        // writable as CreateProcessW requires, and the environment block ends
        // with two NULs.
        let created = unsafe {
            CreateProcessW(
                application.as_ptr(),
                command_line.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                flags | CREATE_UNICODE_ENVIRONMENT,
                environment.as_ptr().cast(),
                std::ptr::null(),
                &startup,
                &mut information,
            )
        };
        if created == 0 {
            failure = Some(std::io::Error::last_os_error());
            continue;
        }
        // SAFETY: CreateProcessW returned both handles, and nothing else owns them.
        return Ok(unsafe {
            drop(OwnedHandle::from_raw_handle(information.hThread));
            OwnedHandle::from_raw_handle(information.hProcess)
        });
    }
    Err(failure.unwrap_or_else(std::io::Error::last_os_error))
}

/// `"<cmd>" /d /c "<script>"`, NUL-terminated. Windows paths cannot contain `"`.
fn waiter_command_line(cmd: &[u16], script: &[u16]) -> Vec<u16> {
    let quote = u16::from(b'"');
    let mut line = vec![quote];
    line.extend_from_slice(cmd);
    line.push(quote);
    line.extend(" /d /c ".encode_utf16());
    line.push(quote);
    line.extend_from_slice(script);
    line.push(quote);
    line.push(0);
    line
}

/// A `CREATE_UNICODE_ENVIRONMENT` block: the inherited variables with `name`
/// set to `value`, sorted by name ignoring case as Windows keeps them, each
/// `name=value` NUL-terminated, and one more NUL at the end.
fn waiter_environment(
    inherited: impl IntoIterator<Item = (Vec<u16>, Vec<u16>)>,
    name: &[u16],
    value: Vec<u16>,
) -> Vec<u16> {
    let folded = |text: &[u16]| -> Vec<u16> {
        text.iter()
            .map(|&unit| match u8::try_from(unit) {
                Ok(byte) => u16::from(byte.to_ascii_uppercase()),
                Err(_) => unit,
            })
            .collect()
    };
    let mut variables: Vec<(Vec<u16>, Vec<u16>)> = inherited
        .into_iter()
        .filter(|(inherited_name, _)| folded(inherited_name) != folded(name))
        .collect();
    variables.push((name.to_vec(), value));
    variables.sort_by_cached_key(|(variable, _)| folded(variable));
    let mut block = Vec::new();
    for (variable, value) in variables {
        block.extend(variable);
        block.push(u16::from(b'='));
        block.extend(value);
        block.push(0);
    }
    block.push(0);
    block
}

#[cfg(all(windows, not(test)))]
fn write_restart_handoff(executable: &str, env: BTreeMap<String, String>) -> Result<(), String> {
    use std::io::Write;

    let bytes = encode_handoff(executable, now_ms(), env);
    let path = handoff_path(&std::env::temp_dir());
    let temporary = path.with_extension("json.tmp");
    {
        let mut file = std::fs::File::create(&temporary)
            .map_err(|error| format!("could not create the restart handoff: {error}"))?;
        file.write_all(&bytes)
            .map_err(|error| format!("could not write the restart handoff: {error}"))?;
    }
    std::fs::rename(&temporary, &path)
        .map_err(|error| format!("could not publish the restart handoff: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_env() -> BTreeMap<String, String> {
        BTreeMap::from([
            ("CODDY_STUDIO_HOME".to_owned(), r"D:\lane\data".to_owned()),
            ("CODDY_STUDIO_PORT".to_owned(), "43123".to_owned()),
        ])
    }

    #[test]
    fn handoff_file_lives_in_the_temp_directory() {
        assert_eq!(
            handoff_path(Path::new(r"C:\Temp")),
            Path::new(r"C:\Temp").join("coddy-studio-desktop-restart.json")
        );
    }

    #[test]
    fn handoff_applies_only_missing_launch_variables_for_the_same_executable() {
        let bytes = encode_handoff(r"D:\Apps\coddy-studio-desktop.exe", 1_000, sample_env());
        let assignments =
            assignments_from_handoff(&bytes, r"D:\Apps\coddy-studio-desktop.exe", 1_500, |key| {
                key == "CODDY_STUDIO_HOME"
            });
        assert_eq!(
            assignments,
            Some(vec![("CODDY_STUDIO_PORT".to_owned(), "43123".to_owned())])
        );
    }

    /// A restart that inherited both variables still claims the handoff, so
    /// `adopt_restart_handoff` deletes it before a later ordinary launch.
    #[test]
    fn a_matching_handoff_is_claimed_even_when_nothing_is_left_to_apply() {
        let bytes = encode_handoff(r"D:\Apps\coddy-studio-desktop.exe", 1_000, sample_env());
        assert_eq!(
            assignments_from_handoff(&bytes, r"D:\Apps\coddy-studio-desktop.exe", 1_500, |_| true),
            Some(Vec::new())
        );
    }

    #[test]
    fn handoff_ignores_a_different_executable_an_expired_file_and_unknown_keys() {
        let mut env = sample_env();
        env.insert("OTHER".to_owned(), "secret".to_owned());
        let bytes = encode_handoff(r"D:\Apps\coddy-studio-desktop.exe", 1_000, env);
        assert!(
            assignments_from_handoff(&bytes, r"D:\Other\coddy-studio-desktop.exe", 1_500, |_| {
                false
            })
            .is_none()
        );
        assert!(
            assignments_from_handoff(
                &bytes,
                r"D:\Apps\coddy-studio-desktop.exe",
                1_000 + HANDOFF_TTL_MS + 1,
                |_| false,
            )
            .is_none()
        );
        assert!(handoff_is_expired(&bytes, 1_000 + HANDOFF_TTL_MS + 1));
        let applied =
            assignments_from_handoff(&bytes, r"D:\Apps\coddy-studio-desktop.exe", 1_500, |_| {
                false
            })
            .expect("a matching handoff");
        assert!(applied.iter().all(|(key, _)| key != "OTHER"));
    }

    #[test]
    fn capture_drops_empty_and_control_bearing_values() {
        let env = capture_handoff_env(|key| match key {
            "CODDY_STUDIO_HOME" => Some("  D:\\lane\\data  ".to_owned()),
            "CODDY_STUDIO_PORT" => Some("43123\nbad".to_owned()),
            _ => None,
        });
        assert_eq!(
            env.get("CODDY_STUDIO_HOME").map(String::as_str),
            Some(r"D:\lane\data")
        );
        assert!(!env.contains_key("CODDY_STUDIO_PORT"));
    }

    #[test]
    fn waiter_script_names_the_parent_and_refuses_an_unsafe_path() {
        let script =
            restart_waiter_script(4242, r"D:\Apps\coddy-studio-desktop.exe").expect("script");
        assert!(script.contains("set \"PID=4242\""));
        // cmd.exe reads the script in the console code page: the path is not in it.
        assert!(script.is_ascii());
        assert!(!script.contains(r"D:\Apps"));
        let unicode = restart_waiter_script(1, r"C:\Users\José\coddy-studio-desktop.exe");
        assert!(unicode.is_some_and(|script| script.is_ascii()));
        assert!(script.contains("start \"\" \"%CODDY_STUDIO_RESTART_APP%\""));
        assert!(script.contains("/C:\"-setup.exe\" /C:\"-installer.exe\""));
        assert!(script.contains("if %LEFT% LEQ 0 exit /b 0"));
        assert!(
            script.contains(
                "/FI \"IMAGENAME eq coddy-studio-desktop.exe\" /FI \"WINDOWTITLE ne N/A\""
            )
        );
        assert!(!script.contains("goto launch"));
        assert!(restart_waiter_script(1, r#"D:\Apps\bad"name.exe"#).is_none());
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    #[test]
    fn waiter_command_line_quotes_both_paths_and_ends_with_nul() {
        let line = waiter_command_line(
            &wide(r"C:\Windows\system32\cmd.exe"),
            &wide(r"C:\Users\José Á\AppData\Local\Temp\coddy-studio-desktop-restart-waiter.cmd"),
        );
        assert_eq!(line.last(), Some(&0));
        assert_eq!(
            String::from_utf16(&line[..line.len() - 1]).expect("UTF-16"),
            r#""C:\Windows\system32\cmd.exe" /d /c "C:\Users\José Á\AppData\Local\Temp\coddy-studio-desktop-restart-waiter.cmd""#
        );
    }

    #[test]
    fn waiter_environment_sets_the_app_path_once_in_a_sorted_block() {
        let block = waiter_environment(
            [
                (wide("Path"), wide(r"C:\Windows")),
                (wide("coddy_studio_restart_app"), wide("stale")),
                (wide("=C:"), wide(r"C:\")),
                (wide("CODDY_STUDIO_HOME"), wide(r"D:\lane\data")),
            ],
            &wide(WAITER_APP_VAR),
            wide(r"C:\Users\José\coddy-studio-desktop.exe"),
        );
        assert_eq!(&block[block.len() - 2..], &[0, 0]);
        let entries: Vec<String> = block[..block.len() - 2]
            .split(|&unit| unit == 0)
            .map(|entry| String::from_utf16(entry).expect("UTF-16"))
            .collect();
        assert_eq!(
            entries,
            [
                r"=C:=C:\",
                r"CODDY_STUDIO_HOME=D:\lane\data",
                r"CODDY_STUDIO_RESTART_APP=C:\Users\José\coddy-studio-desktop.exe",
                r"Path=C:\Windows",
            ]
        );
    }
}
