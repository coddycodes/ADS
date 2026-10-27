#![cfg(windows)]

use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    time::Duration,
};

use base64::Engine as _;
use coddy_studio_server::terminal::{PortablePtyBackend, PtyBackend, PtySpawnInput};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
        WAIT_OBJECT_0,
    },
    System::Console::{
        CONSOLE_SCREEN_BUFFER_INFOEX, GetConsoleScreenBufferInfoEx, GetStdHandle, STD_OUTPUT_HANDLE,
    },
    System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
};

const OUTPUT_MARKER: &str = "CODDY_STUDIO_NATIVE_PTY_OUTPUT";
const CONSOLE_PALETTE_MARKER: &str = "CODDY_STUDIO_CONSOLE_PALETTE";
const CONSOLE_PALETTE_CHILD_ENV: &str = "CODDY_STUDIO_TEST_CONSOLE_PALETTE_CHILD";
const WINDOWS_CONSOLE_THEME_ENV: &str = "CODDY_STUDIO_WINDOWS_CONSOLE_THEME";
const TRUSTED_CMD_FIXTURE_ENV: &str = "CODDY_STUDIO_TEST_TRUSTED_CMD_FIXTURE";
const NATIVE_ARGV_MARKER: &str = "CODDY_STUDIO_NATIVE_ARGV";
const TERMINAL_PROCESS_TREE_INTEGRATION_DEADLINE: Duration = Duration::from_secs(30);

struct OwnedHandle(HANDLE);

#[test]
fn raw_conpty_captures_native_windows_terminal_output() {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut command = CommandBuilder::new("cmd.exe");
    command.args(["/d", "/s", "/c", &format!("echo {OUTPUT_MARKER}")]);
    let mut child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let output = std::thread::spawn(move || {
        let mut output = String::new();
        reader.read_to_string(&mut output).unwrap();
        output
    });

    assert!(child.wait().unwrap().success());
    drop(pair.master);
    let output = output.join().unwrap();
    assert!(
        output.contains(OUTPUT_MARKER),
        "captured output: {output:?}"
    );
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: this type owns the handle and closes it exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

async fn wait_for_pid_file(path: &Path) -> u32 {
    tokio::time::timeout(TERMINAL_PROCESS_TREE_INTEGRATION_DEADLINE, async {
        loop {
            if let Ok(contents) = std::fs::read_to_string(path)
                && let Ok(pid) = contents.trim().parse::<u32>()
            {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap()
}

fn powershell_encoded_command(command: &str) -> String {
    let utf16 = command
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    base64::engine::general_purpose::STANDARD.encode(utf16)
}

fn descendant_spawning_powershell_command() -> String {
    let child_command = powershell_encoded_command(
        "[IO.File]::WriteAllText($env:CODDY_STUDIO_TEST_CHILD_PID, [string]$PID); Start-Sleep -Seconds 30",
    );
    format!(
        "$child = Start-Process -FilePath powershell.exe -ArgumentList '-NoLogo','-NoProfile','-NonInteractive','-EncodedCommand','{child_command}' -PassThru; Wait-Process -Id $child.Id"
    )
}

async fn assert_terminal_kills_descendant(input: PtySpawnInput, child_pid_file: &Path) {
    let process = PortablePtyBackend.spawn(&input).unwrap();
    let child_pid = wait_for_pid_file(child_pid_file).await;

    let mut exit = process.subscribe_exit();
    process.kill().unwrap();
    let already_exited = exit.borrow().is_some();
    if !already_exited {
        tokio::time::timeout(Duration::from_secs(10), exit.changed())
            .await
            .unwrap()
            .unwrap();
    }

    // SAFETY: OpenProcess only inspects the process identified by the fixture PID.
    let child = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, child_pid) };
    if child.is_null() {
        // A process that has already been reaped is also a successful outcome.
        assert_eq!(unsafe { GetLastError() }, ERROR_INVALID_PARAMETER);
        return;
    }
    let child = OwnedHandle(child);
    // SAFETY: `child` owns a live synchronization handle.
    assert_eq!(
        unsafe { WaitForSingleObject(child.0, 10_000) },
        WAIT_OBJECT_0,
        "PTY descendant survived terminal kill"
    );
}

async fn assert_shim_output(
    executable: &Path,
    arguments: &[String],
    environment: BTreeMap<String, String>,
    marker: &str,
) {
    let process = PortablePtyBackend
        .spawn(&PtySpawnInput {
            executable: executable.to_string_lossy().into_owned(),
            args: arguments.to_vec(),
            cwd: executable.parent().unwrap().to_path_buf(),
            cols: 80,
            rows: 24,
            env: environment,
        })
        .unwrap();
    let mut output = process.subscribe_output();
    let mut exit = process.subscribe_exit();
    let text = tokio::time::timeout(Duration::from_secs(10), async {
        let mut text = String::new();
        while !text.contains(marker) {
            if exit.borrow().is_some() {
                break;
            }
            tokio::select! {
                received = output.recv() => {
                    text.push_str(&received.expect("terminal output channel should stay open"));
                }
                changed = exit.changed() => {
                    changed.expect("terminal exit channel should stay open");
                    if exit.borrow().is_some() {
                        break;
                    }
                }
            }
        }
        text
    })
    .await
    .unwrap();
    assert!(
        text.contains(marker),
        "terminal exited without the expected marker; output={text:?}, exit={:?}",
        exit.borrow().clone()
    );
    tokio::time::timeout(Duration::from_secs(10), exit.changed())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        exit.borrow().as_ref().and_then(|event| event.exit_code),
        Some(0)
    );
}

#[tokio::test]
async fn portable_backend_captures_native_windows_terminal_output() {
    let directory = tempfile::tempdir().unwrap();
    let process = PortablePtyBackend
        .spawn(&PtySpawnInput {
            executable: "cmd.exe".to_owned(),
            args: vec![
                "/d".to_owned(),
                "/s".to_owned(),
                "/c".to_owned(),
                format!("echo {OUTPUT_MARKER}"),
            ],
            cwd: directory.path().to_path_buf(),
            cols: 80,
            rows: 24,
            env: BTreeMap::new(),
        })
        .unwrap();
    let mut output = process.subscribe_output();
    let mut exit = process.subscribe_exit();
    let text = tokio::time::timeout(Duration::from_secs(10), async {
        let mut text = String::new();
        while !text.contains(OUTPUT_MARKER) {
            if exit.borrow().is_some() {
                break;
            }
            tokio::select! {
                received = output.recv() => text.push_str(&received.unwrap()),
                changed = exit.changed() => {
                    changed.unwrap();
                    if exit.borrow().is_some() {
                        break;
                    }
                }
            }
        }
        text
    })
    .await
    .unwrap();
    assert!(
        text.contains(OUTPUT_MARKER),
        "captured output: {text:?}; exit={:?}",
        exit.borrow().clone()
    );
}

fn decode_color_ref(color_ref: u32) -> (u8, u8, u8) {
    (
        (color_ref & 0xff) as u8,
        ((color_ref >> 8) & 0xff) as u8,
        ((color_ref >> 16) & 0xff) as u8,
    )
}

fn is_light((red, green, blue): (u8, u8, u8)) -> bool {
    299 * u32::from(red) + 587 * u32::from(green) + 114 * u32::from(blue) >= 128_000
}

#[test]
fn console_palette_fallback_child_fixture() {
    let Ok(expected) = std::env::var(CONSOLE_PALETTE_CHILD_ENV) else {
        return;
    };

    // Match Codex v0.144's OSC timeout before it falls back to the console buffer.
    std::thread::sleep(Duration::from_millis(100));
    let output = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
    if output.is_null() || output == INVALID_HANDLE_VALUE {
        std::process::exit(91);
    }
    let mut info = unsafe { std::mem::zeroed::<CONSOLE_SCREEN_BUFFER_INFOEX>() };
    info.cbSize = std::mem::size_of::<CONSOLE_SCREEN_BUFFER_INFOEX>() as u32;
    if unsafe { GetConsoleScreenBufferInfoEx(output, &mut info) } == 0 {
        std::process::exit(91);
    }

    let foreground_index = usize::from(info.wAttributes & 0x0f);
    let background_index = usize::from((info.wAttributes >> 4) & 0x0f);
    let foreground = decode_color_ref(info.ColorTable[foreground_index]);
    let background = decode_color_ref(info.ColorTable[background_index]);
    let palette_matches = match expected.as_str() {
        "light" => !is_light(foreground) && is_light(background),
        "dark" => is_light(foreground) && !is_light(background),
        "report" => true,
        _ => false,
    };
    let cwd_matches = std::env::var("CODDY_STUDIO_TEST_EXPECTED_CWD").map_or(true, |cwd| {
        Path::new(&cwd) == std::env::current_dir().unwrap()
    });
    let env_matches =
        std::env::var("CODDY_STUDIO_TEST_SENTINEL_ENV").map_or(true, |value| value == "env value");
    let marker_stripped = std::env::var_os(WINDOWS_CONSOLE_THEME_ENV).is_none();
    if !(palette_matches && cwd_matches && env_matches && marker_stripped) {
        std::process::exit(92);
    }

    println!(
        "{CONSOLE_PALETTE_MARKER}:{expected}:fg-index={foreground_index}:bg-index={background_index}:fg={foreground:?}:bg={background:?}"
    );
}

#[test]
fn trusted_cmd_path_child_fixture() {
    let Ok(expected) = std::env::var(TRUSTED_CMD_FIXTURE_ENV) else {
        return;
    };
    let executable = std::env::current_exe().unwrap();
    let process = PortablePtyBackend
        .spawn(&PtySpawnInput {
            executable: executable.to_string_lossy().into_owned(),
            args: vec![
                "console_palette_fallback_child_fixture".to_owned(),
                "--exact".to_owned(),
                "--nocapture".to_owned(),
            ],
            cwd: executable.parent().unwrap().to_path_buf(),
            cols: 80,
            rows: 24,
            env: BTreeMap::from([
                (CONSOLE_PALETTE_CHILD_ENV.to_owned(), expected.clone()),
                (WINDOWS_CONSOLE_THEME_ENV.to_owned(), expected.clone()),
            ]),
        })
        .unwrap();
    let mut output = process.subscribe_output();
    let mut exit = process.subscribe_exit();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let text = runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(10), async {
            let mut text = String::new();
            while !text.contains(CONSOLE_PALETTE_MARKER) && exit.borrow().is_none() {
                tokio::select! {
                    received = output.recv() => text.push_str(&received.unwrap()),
                    changed = exit.changed() => { changed.unwrap(); }
                }
            }
            text
        })
        .await
        .unwrap()
    });
    assert!(
        text.contains(&format!("{CONSOLE_PALETTE_MARKER}:{expected}:")),
        "output={text:?}, exit={:?}",
        exit.borrow().clone()
    );
}

#[test]
fn windows_theme_initializer_ignores_poisoned_path_and_working_directory() {
    let directory = tempfile::tempdir().unwrap();
    let fake_cmd = directory.path().join("cmd.exe");
    let hostile_executable = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32")
        .join("where.exe");
    std::fs::copy(hostile_executable, &fake_cmd).unwrap();
    let fake_output = std::process::Command::new(&fake_cmd)
        .args(["/d", "/c", "color", "F0"])
        .output()
        .unwrap();
    assert!(
        !fake_output.status.success(),
        "the poisoned cmd fixture must fail if it is ever selected"
    );

    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["trusted_cmd_path_child_fixture", "--exact", "--nocapture"])
        .current_dir(directory.path())
        .env("PATH", directory.path())
        .env("PATHEXT", ".EXE")
        .env(TRUSTED_CMD_FIXTURE_ENV, "light")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "fixture selected the fake cmd.exe; status={:?}, stdout={:?}, stderr={:?}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn palette_observation(output: &str) -> &str {
    let start = output
        .find(CONSOLE_PALETTE_MARKER)
        .unwrap_or_else(|| panic!("palette marker missing from {output:?}"));
    let observation = &output[start..];
    let end = observation.find(['\r', '\n']).unwrap_or(observation.len());
    &observation[..end]
}

fn raw_conpty_palette_observation() -> String {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut reader = pair.master.try_clone_reader().unwrap();
    let output = std::thread::spawn(move || {
        let mut output = String::new();
        reader.read_to_string(&mut output).unwrap();
        output
    });
    let mut probe = CommandBuilder::new(std::env::current_exe().unwrap());
    probe.args([
        "console_palette_fallback_child_fixture",
        "--exact",
        "--nocapture",
    ]);
    probe.env(CONSOLE_PALETTE_CHILD_ENV, "report");
    let mut child = pair.slave.spawn_command(probe).unwrap();
    drop(pair.slave);
    assert!(child.wait().unwrap().success());
    drop(pair.master);
    let output = output.join().unwrap();
    palette_observation(&output).to_owned()
}

#[tokio::test]
async fn portable_backend_without_theme_marker_keeps_the_fresh_conpty_palette() {
    let baseline = raw_conpty_palette_observation();
    let executable = std::env::current_exe().unwrap();
    let process = PortablePtyBackend
        .spawn(&PtySpawnInput {
            executable: executable.to_string_lossy().into_owned(),
            args: vec![
                "console_palette_fallback_child_fixture".to_owned(),
                "--exact".to_owned(),
                "--nocapture".to_owned(),
            ],
            cwd: executable.parent().unwrap().to_path_buf(),
            cols: 80,
            rows: 24,
            env: BTreeMap::from([(CONSOLE_PALETTE_CHILD_ENV.to_owned(), "report".to_owned())]),
        })
        .unwrap();
    let mut output = process.subscribe_output();
    let mut exit = process.subscribe_exit();
    let text = tokio::time::timeout(Duration::from_secs(10), async {
        let mut text = String::new();
        while !text.contains(CONSOLE_PALETTE_MARKER) && exit.borrow().is_none() {
            tokio::select! {
                received = output.recv() => text.push_str(&received.unwrap()),
                changed = exit.changed() => { changed.unwrap(); }
            }
        }
        text
    })
    .await
    .unwrap();

    assert_eq!(palette_observation(&text), baseline);
}

#[tokio::test]
async fn marked_backend_preserves_native_root_argv_metacharacters() {
    let directory = tempfile::tempdir().unwrap();
    let script = directory.path().join("inspect argv with spaces !.ps1");
    std::fs::write(
        &script,
        format!(
            "Write-Output (\"{NATIVE_ARGV_MARKER}:\" + (($args | ForEach-Object {{ \"[$_]\" }}) -join \"|\"))\n"
        ),
    )
    .unwrap();
    let expected = format!("{NATIVE_ARGV_MARKER}:[value with spaces]|[%PATH%]|[!literal!]");
    let powershell = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    assert!(powershell.is_file(), "powershell={powershell:?}");

    assert_shim_output(
        &powershell,
        &[
            "-NoLogo".to_owned(),
            "-NoProfile".to_owned(),
            "-NonInteractive".to_owned(),
            "-ExecutionPolicy".to_owned(),
            "Bypass".to_owned(),
            "-File".to_owned(),
            script.to_string_lossy().into_owned(),
            "value with spaces".to_owned(),
            "%PATH%".to_owned(),
            "!literal!".to_owned(),
        ],
        BTreeMap::from([(WINDOWS_CONSOLE_THEME_ENV.to_owned(), "dark".to_owned())]),
        &expected,
    )
    .await;
}

#[test]
fn same_conpty_slave_retains_console_theme_for_codex_fallback() {
    for (expected, color_code) in [("light", "F0"), ("dark", "0F")] {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut reader = pair.master.try_clone_reader().unwrap();
        let output = std::thread::spawn(move || {
            let mut output = String::new();
            reader.read_to_string(&mut output).unwrap();
            output
        });

        let mut initializer = CommandBuilder::new("cmd.exe");
        initializer.raw_windows_args(format!("/d /c color {color_code}"));
        let mut initializer = pair.slave.spawn_command(initializer).unwrap();
        assert!(initializer.wait().unwrap().success());

        let directory = tempfile::tempdir().unwrap();
        let mut probe = CommandBuilder::new(std::env::current_exe().unwrap());
        probe.args([
            "console_palette_fallback_child_fixture",
            "--exact",
            "--nocapture",
        ]);
        probe.cwd(directory.path());
        probe.env(CONSOLE_PALETTE_CHILD_ENV, expected);
        let mut child = pair.slave.spawn_command(probe).unwrap();
        drop(pair.slave);
        let status = child.wait().unwrap();
        drop(pair.master);
        let output = output.join().unwrap();
        assert!(
            status.success() && output.contains(&format!("{CONSOLE_PALETTE_MARKER}:{expected}:")),
            "expected={expected}, status={status:?}, output={output:?}"
        );
    }
}

#[tokio::test]
async fn portable_backend_initializes_codex_console_palette_before_root_spawn() {
    for expected in ["light", "dark"] {
        let directory = tempfile::tempdir().unwrap();
        let cwd = directory.path().join("codex cwd with spaces ! literal");
        std::fs::create_dir(&cwd).unwrap();
        let executable = std::env::current_exe().unwrap();
        let process = PortablePtyBackend
            .spawn(&PtySpawnInput {
                executable: executable.to_string_lossy().into_owned(),
                args: vec![
                    "console_palette_fallback_child_fixture".to_owned(),
                    "--exact".to_owned(),
                    "--nocapture".to_owned(),
                ],
                cwd: cwd.clone(),
                cols: 80,
                rows: 24,
                env: BTreeMap::from([
                    (CONSOLE_PALETTE_CHILD_ENV.to_owned(), expected.to_owned()),
                    (WINDOWS_CONSOLE_THEME_ENV.to_owned(), expected.to_owned()),
                    (
                        "CODDY_STUDIO_TEST_EXPECTED_CWD".to_owned(),
                        cwd.to_string_lossy().into_owned(),
                    ),
                    (
                        "CODDY_STUDIO_TEST_SENTINEL_ENV".to_owned(),
                        "env value".to_owned(),
                    ),
                ]),
            })
            .unwrap();
        let mut output = process.subscribe_output();
        let mut exit = process.subscribe_exit();
        let text = tokio::time::timeout(Duration::from_secs(10), async {
            let mut text = String::new();
            while !text.contains(CONSOLE_PALETTE_MARKER) && exit.borrow().is_none() {
                tokio::select! {
                    received = output.recv() => text.push_str(&received.unwrap()),
                    changed = exit.changed() => { changed.unwrap(); }
                }
            }
            text
        })
        .await
        .unwrap();
        assert!(
            text.contains(&format!("{CONSOLE_PALETTE_MARKER}:{expected}:")),
            "expected={expected}, output={text:?}, exit={:?}",
            exit.borrow().clone()
        );
    }
}

#[tokio::test]
async fn portable_backend_runs_windows_command_and_powershell_shims() {
    let directory = tempfile::tempdir().unwrap();
    let shim_directory = directory.path().join("provider ! shims");
    std::fs::create_dir(&shim_directory).unwrap();
    let arguments = [
        "value with spaces".to_owned(),
        "literal-value".to_owned(),
        "percent-value".to_owned(),
        "!literal!".to_owned(),
    ];
    for extension in ["cmd", "bat"] {
        let executable = shim_directory.join(format!("provider.{extension}"));
        std::fs::write(
            &executable,
            format!(
                "@echo off\r\nping -n 2 127.0.0.1 >nul\r\necho {extension}:%~1:%~2:%~3:%~4:%CODDY_STUDIO_INTERNAL_BATCH_SCRIPT%\r\n"
            ),
        )
        .unwrap();
        assert_shim_output(
            &executable,
            &arguments,
            BTreeMap::from([(
                "CODDY_STUDIO_INTERNAL_BATCH_SCRIPT".to_owned(),
                "user-controlled-value".to_owned(),
            )]),
            &format!(
                "{extension}:value with spaces:literal-value:percent-value:!literal!:user-controlled-value"
            ),
        )
        .await;
    }

    let powershell = shim_directory.join("provider.ps1");
    std::fs::write(
        &powershell,
        "Start-Sleep -Milliseconds 250\nWrite-Output \"ps1:$($args[0]):$($args[1]):$($args[2]):$($args[3])\"\n",
    )
    .unwrap();
    assert_shim_output(
        &powershell,
        &arguments,
        BTreeMap::new(),
        "ps1:value with spaces:literal-value:percent-value:!literal!",
    )
    .await;
}

#[tokio::test]
async fn killing_a_batch_terminal_terminates_its_descendant_process() {
    let directory = tempfile::tempdir().unwrap();
    let child_pid_file = directory.path().join("child-pid.txt");
    let script = directory.path().join("long running provider.cmd");
    std::fs::write(
        &script,
        "@echo off\r\npowershell.exe -NoLogo -NoProfile -NonInteractive -Command \"[IO.File]::WriteAllText($env:CODDY_STUDIO_TEST_CHILD_PID, [string]$PID); Start-Sleep -Seconds 30\"\r\n",
    )
    .unwrap();
    assert_terminal_kills_descendant(
        PtySpawnInput {
            executable: script.to_string_lossy().into_owned(),
            args: Vec::new(),
            cwd: directory.path().to_path_buf(),
            cols: 80,
            rows: 24,
            env: BTreeMap::from([(
                "CODDY_STUDIO_TEST_CHILD_PID".to_owned(),
                child_pid_file.to_string_lossy().into_owned(),
            )]),
        },
        &child_pid_file,
    )
    .await;
}

#[tokio::test]
async fn killing_a_powershell_shim_terminal_terminates_its_descendant_process() {
    let directory = tempfile::tempdir().unwrap();
    let child_pid_file = directory.path().join("powershell-shim-child-pid.txt");
    let script = directory.path().join("long running provider.ps1");
    std::fs::write(&script, descendant_spawning_powershell_command()).unwrap();

    assert_terminal_kills_descendant(
        PtySpawnInput {
            executable: script.to_string_lossy().into_owned(),
            args: Vec::new(),
            cwd: directory.path().to_path_buf(),
            cols: 80,
            rows: 24,
            env: BTreeMap::from([(
                "CODDY_STUDIO_TEST_CHILD_PID".to_owned(),
                child_pid_file.to_string_lossy().into_owned(),
            )]),
        },
        &child_pid_file,
    )
    .await;
}

#[tokio::test]
async fn killing_a_native_windows_terminal_terminates_its_descendant_process() {
    let directory = tempfile::tempdir().unwrap();
    let child_pid_file = directory.path().join("native-child-pid.txt");

    assert_terminal_kills_descendant(
        PtySpawnInput {
            executable: "powershell.exe".to_owned(),
            args: vec![
                "-NoLogo".to_owned(),
                "-NoProfile".to_owned(),
                "-NonInteractive".to_owned(),
                "-Command".to_owned(),
                descendant_spawning_powershell_command(),
            ],
            cwd: directory.path().to_path_buf(),
            cols: 80,
            rows: 24,
            env: BTreeMap::from([(
                "CODDY_STUDIO_TEST_CHILD_PID".to_owned(),
                child_pid_file.to_string_lossy().into_owned(),
            )]),
        },
        &child_pid_file,
    )
    .await;
}
