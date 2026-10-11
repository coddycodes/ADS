//! Keeps a Windows update restart on the same data root and backend port.
//!
//! Passive NSIS relaunches the installed executable with `RunAsUser`. When the
//! installer is elevated, that helper uses `CreateProcessWithTokenW` and a null
//! environment block, so the new process does not receive `BIBCODE_HOME` or
//! `BIBCODE_PORT` from the process that started the installer. Remote clients
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

const HANDOFF_FILE_NAME: &str = "bibcode-desktop-restart.json";
const HANDOFF_VERSION: u32 = 1;
/// The passive installer returns before NSIS finishes. The observed Windows
/// remote install replaced the executable about three minutes after the request.
const HANDOFF_TTL_MS: u64 = 10 * 60 * 1000;
const HANDOFF_FUTURE_SKEW_MS: u64 = 60 * 1000;
const HANDOFF_ENV_KEYS: &[&str] = &["BIBCODE_HOME", "BIBCODE_PORT"];
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

pub(crate) fn assignments_from_handoff(
    bytes: &[u8],
    current_exe: &str,
    now_ms: u64,
    already_set: impl Fn(&str) -> bool,
) -> Vec<(String, String)> {
    let Ok(handoff) = serde_json::from_slice::<RestartHandoff>(bytes) else {
        return Vec::new();
    };
    if handoff.version != HANDOFF_VERSION || handoff.exe != current_exe {
        return Vec::new();
    }
    if now_ms.saturating_add(HANDOFF_FUTURE_SKEW_MS) < handoff.created_at_ms {
        return Vec::new();
    }
    if now_ms.saturating_sub(handoff.created_at_ms) > HANDOFF_TTL_MS {
        return Vec::new();
    }
    handoff
        .env
        .into_iter()
        .filter(|(key, value)| {
            HANDOFF_ENV_KEYS.contains(&key.as_str())
                && !value.is_empty()
                && !value.chars().any(|character| character.is_control())
                && !already_set(key)
        })
        .collect()
}

pub(crate) fn handoff_is_expired(bytes: &[u8], now_ms: u64) -> bool {
    let Ok(handoff) = serde_json::from_slice::<RestartHandoff>(bytes) else {
        return true;
    };
    now_ms.saturating_sub(handoff.created_at_ms) > HANDOFF_TTL_MS
        || now_ms.saturating_add(HANDOFF_FUTURE_SKEW_MS) < handoff.created_at_ms
}

/// Builds the waiter script. `None` when the executable path cannot be embedded safely.
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
         set \"APP={executable}\"\r\n\
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
         tasklist /FO CSV /NH 2>nul | findstr /I /C:\"-setup.exe\" >nul\r\n\
         if not errorlevel 1 (\r\n\
           set /a QUIET=0\r\n\
           set /a LEFT-=1\r\n\
           if %LEFT% LEQ 0 exit /b 0\r\n\
           ping -n 2 127.0.0.1 >nul\r\n\
           goto while_install\r\n\
         )\r\n\
         tasklist /FI \"IMAGENAME eq bibcode-desktop.exe\" /NH 2>nul | findstr /I /C:\"bibcode-desktop.exe\" >nul\r\n\
         if not errorlevel 1 exit /b 0\r\n\
         set /a QUIET+=1\r\n\
         if %QUIET% LSS 3 (\r\n\
           set /a LEFT-=1\r\n\
           if %LEFT% LEQ 0 exit /b 0\r\n\
           ping -n 2 127.0.0.1 >nul\r\n\
           goto while_install\r\n\
         )\r\n\
         start \"\" \"%APP%\"\r\n\
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

/// Applies a fresh handoff before any worker thread exists.
pub(crate) fn adopt_restart_handoff() {
    #[cfg(windows)]
    {
        let path = handoff_path(&std::env::temp_dir());
        let Ok(bytes) = std::fs::read(&path) else {
            return;
        };
        let now = now_ms();
        if handoff_is_expired(&bytes, now) {
            let _ = std::fs::remove_file(&path);
            return;
        }
        let Ok(current_exe) = std::env::current_exe() else {
            return;
        };
        let assignments =
            assignments_from_handoff(&bytes, &current_exe.to_string_lossy(), now, |key| {
                std::env::var_os(key).is_some()
            });
        if assignments.is_empty() {
            return;
        }
        // SAFETY: this runs at the start of `run`, before Tauri creates threads.
        // No other thread can read the environment concurrently.
        for (key, value) in assignments {
            unsafe { std::env::set_var(&key, value) };
        }
    }
}

/// Publishes the handoff and a detached waiter. Failures must not block install.
pub(crate) fn prepare_restart_handoff() {
    #[cfg(all(windows, not(test)))]
    {
        if let Err(error) = publish_restart_handoff() {
            tracing::warn!(%error, "windows update restart handoff was not published");
        }
    }
}

#[cfg(all(windows, not(test)))]
fn publish_restart_handoff() -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};

    let executable = std::env::current_exe()
        .map_err(|error| format!("could not resolve the installed executable: {error}"))?;
    let executable = executable.to_string_lossy().to_string();
    let env = capture_handoff_env(|key| std::env::var(key).ok());
    if !env.is_empty() {
        if let Err(error) = write_restart_handoff(&executable, env) {
            tracing::warn!(%error, "windows update restart handoff file was not published");
        }
    }
    let Some(script) = restart_waiter_script(std::process::id(), &executable) else {
        return Ok(());
    };
    let script_path = std::env::temp_dir().join("bibcode-desktop-restart-waiter.cmd");
    std::fs::write(&script_path, script)
        .map_err(|error| format!("could not write the restart waiter: {error}"))?;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
    let mut command = Command::new("cmd.exe");
    command
        .args(["/d", "/c", &script_path.to_string_lossy()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let spawned = command
        .creation_flags(
            DETACHED_PROCESS
                | CREATE_NEW_PROCESS_GROUP
                | CREATE_NO_WINDOW
                | CREATE_BREAKAWAY_FROM_JOB,
        )
        .spawn();
    if spawned.is_err() {
        command
            .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
            .spawn()
            .map_err(|error| format!("could not start the restart waiter: {error}"))?;
    }
    Ok(())
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
            ("BIBCODE_HOME".to_owned(), r"D:\lane\data".to_owned()),
            ("BIBCODE_PORT".to_owned(), "43123".to_owned()),
        ])
    }

    #[test]
    fn handoff_file_lives_in_the_temp_directory() {
        assert_eq!(
            handoff_path(Path::new(r"C:\Temp")),
            Path::new(r"C:\Temp").join("bibcode-desktop-restart.json")
        );
    }

    #[test]
    fn handoff_applies_only_missing_launch_variables_for_the_same_executable() {
        let bytes = encode_handoff(r"D:\Apps\bibcode-desktop.exe", 1_000, sample_env());
        let assignments =
            assignments_from_handoff(&bytes, r"D:\Apps\bibcode-desktop.exe", 1_500, |key| {
                key == "BIBCODE_HOME"
            });
        assert_eq!(
            assignments,
            vec![("BIBCODE_PORT".to_owned(), "43123".to_owned())]
        );
    }

    #[test]
    fn handoff_ignores_a_different_executable_an_expired_file_and_unknown_keys() {
        let mut env = sample_env();
        env.insert("OTHER".to_owned(), "secret".to_owned());
        let bytes = encode_handoff(r"D:\Apps\bibcode-desktop.exe", 1_000, env);
        assert!(
            assignments_from_handoff(&bytes, r"D:\Other\bibcode-desktop.exe", 1_500, |_| false)
                .is_empty()
        );
        assert!(
            assignments_from_handoff(
                &bytes,
                r"D:\Apps\bibcode-desktop.exe",
                1_000 + HANDOFF_TTL_MS + 1,
                |_| false,
            )
            .is_empty()
        );
        assert!(handoff_is_expired(&bytes, 1_000 + HANDOFF_TTL_MS + 1));
        let applied =
            assignments_from_handoff(&bytes, r"D:\Apps\bibcode-desktop.exe", 1_500, |_| false);
        assert!(applied.iter().all(|(key, _)| key != "OTHER"));
    }

    #[test]
    fn capture_drops_empty_and_control_bearing_values() {
        let env = capture_handoff_env(|key| match key {
            "BIBCODE_HOME" => Some("  D:\\lane\\data  ".to_owned()),
            "BIBCODE_PORT" => Some("43123\nbad".to_owned()),
            _ => None,
        });
        assert_eq!(
            env.get("BIBCODE_HOME").map(String::as_str),
            Some(r"D:\lane\data")
        );
        assert!(!env.contains_key("BIBCODE_PORT"));
    }

    #[test]
    fn waiter_script_names_the_parent_and_refuses_an_unsafe_path() {
        let script = restart_waiter_script(4242, r"D:\Apps\bibcode-desktop.exe").expect("script");
        assert!(script.contains("set \"PID=4242\""));
        assert!(script.contains("set \"APP=D:\\Apps\\bibcode-desktop.exe\""));
        assert!(script.contains("start \"\" \"%APP%\""));
        assert!(script.contains("-setup.exe"));
        assert!(script.contains("if %LEFT% LEQ 0 exit /b 0"));
        assert!(!script.contains("goto launch"));
        assert!(restart_waiter_script(1, r#"D:\Apps\bad"name.exe"#).is_none());
    }
}
