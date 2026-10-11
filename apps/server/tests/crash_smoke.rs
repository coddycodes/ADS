//! The crash smoke, also run by CI on every OS: a real native fault in the `crash-smoke`
//! binary must leave exactly one minidump with its sidecar that `bibcode profiler symbolicate`
//! reads, and the monitor child must never outlive the process it watches.

use std::{
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use bibcode_server::diagnostics::profiler::{
    crash::{self, MonitorDetector},
    runtime::{Detector, ProfilerRuntime},
    sentinel,
};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use tokio_util::sync::CancellationToken;

const SMOKE: &str = env!("CARGO_BIN_EXE_crash-smoke");

#[test]
#[cfg_attr(
    all(windows, target_arch = "aarch64"),
    ignore = "minidump-writer unimplemented on Windows arm64"
)]
fn native_crash_produces_a_minidump_and_a_sidecar() {
    // CI sets CRASH_SMOKE_OUT to a persistent directory so the dump survives the
    // test and can be symbolicated (and uploaded as an artifact) afterwards. It is a data
    // root: the dump goes to its profiler directory, where `bibcode profiler` finds it.
    let temp = tempfile::tempdir().unwrap();
    let out = std::env::var_os("CRASH_SMOKE_OUT").map(PathBuf::from);
    let root = out.as_deref().unwrap_or(temp.path());
    let dir = root.join("userdata").join("profiler");
    // A persistent directory may hold an earlier run's dump, session and findings.
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let crashes = dir.join("crashes");

    let started_at = time::OffsetDateTime::now_utc();
    let mut smoke = Command::new(SMOKE).arg(&dir).spawn().unwrap();
    let parent = smoke.id();
    let status = smoke.wait().unwrap();
    assert!(!status.success());
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        // macOS may report a fault in the protected zero page as SIGBUS.
        let faults: &[i32] = if cfg!(target_os = "macos") {
            &[libc::SIGSEGV, libc::SIGBUS]
        } else {
            &[libc::SIGSEGV]
        };
        assert!(
            status
                .signal()
                .is_some_and(|signal| faults.contains(&signal)),
            "the fault still ends the process: {status:?}"
        );
    }

    let dumps: Vec<_> = std::fs::read_dir(&crashes)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "dmp"))
        .collect();
    assert_eq!(dumps.len(), 1);
    let dump = std::fs::read(dumps[0].path()).unwrap();
    assert_eq!(&dump[..4], b"MDMP", "a minidump header");
    eprintln!("minidump: {} bytes", dump.len());

    let sidecar = dumps[0].path().with_extension("json");
    let meta: serde_json::Value = serde_json::from_slice(&std::fs::read(sidecar).unwrap()).unwrap();
    assert_eq!(meta["processKind"], "crash-smoke");
    let stem = dumps[0]
        .path()
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(meta["id"], stem.as_str());
    assert_eq!(meta["parentPid"], parent);
    assert_eq!(meta["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(meta["os"], std::env::consts::OS);
    assert_eq!(meta["arch"], std::env::consts::ARCH);
    assert!(meta["createdAt"].is_string());
    assert!(meta["lastEvents"].is_array());
    assert_eq!(meta["bootId"], serde_json::json!(sentinel::boot_id()));

    // The CLI walks the real dump; naming its frames needs symbols, the crash reason and the
    // crashing thread's stack do not.
    let symbolicated = Command::new(env!("CARGO_BIN_EXE_bibcode"))
        .args(["profiler", "symbolicate", &stem, "--json", "--base-dir"])
        .arg(root)
        // A developer shell must not move the state directory to `dev/`.
        .env_remove("VITE_DEV_SERVER_URL")
        .env_remove("BIBCODE_HOME")
        .output()
        .unwrap();
    assert!(
        symbolicated.status.success(),
        "{}",
        String::from_utf8_lossy(&symbolicated.stderr)
    );
    let symbolicated: serde_json::Value = serde_json::from_slice(&symbolicated.stdout).unwrap();
    assert!(symbolicated["crashReason"].is_string(), "{symbolicated}");
    assert!(
        symbolicated["crashingThread"]["frames"]
            .as_array()
            .is_some_and(|frames| !frames.is_empty()),
        "{symbolicated}"
    );

    // The next start of a profiler on this directory links the dump to the run that crashed:
    // a `crash.native` finding that names the dump and its crash reason.
    let reason = sentinel::crash_reason(&dumps[0].path()).expect("the dump's crash reason");
    std::fs::write(
        dir.join("session.json"),
        serde_json::json!({
            "state": "running",
            "pid": parent,
            "bootId": sentinel::boot_id(),
            "exe": "crash-smoke",
            "version": env!("CARGO_PKG_VERSION"),
            "startedAt": started_at
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap(),
            "oomKillBaseline": null,
        })
        .to_string(),
    )
    .unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let profiler = ProfilerRuntime::start(&root.join("userdata"), rx, vec![]);
        // The classification also reads the OS record, which can take its 5 s timeout.
        for _ in 0..200 {
            if profiler.status()["findings"]
                .as_array()
                .is_some_and(|findings| findings.iter().any(|f| f["kind"] == "crash.native"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        profiler.end_session().await;
    });
    let native = std::fs::read_to_string(dir.join("findings.ndjson"))
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .rfind(|finding| finding["kind"] == "crash.native")
        .expect("a crash.native finding for the dump");
    assert_eq!(native["evidence"]["crashId"], stem.as_str());
    assert_eq!(native["evidence"]["crashIds"], serde_json::json!([stem]));
    assert_eq!(native["evidence"]["crashReason"], reason.as_str());
    assert_eq!(native["evidence"]["pid"], parent);
    // The fault site comes from the dump's own exception context and module list: the crash
    // is in the smoke binary itself, at an offset into it.
    let module = native["evidence"]["faultModule"]
        .as_str()
        .unwrap_or_default();
    assert!(
        module.eq_ignore_ascii_case("crash-smoke")
            || module.eq_ignore_ascii_case("crash-smoke.exe"),
        "{native}"
    );
    assert!(
        native["evidence"]["faultOffset"]
            .as_str()
            .is_some_and(|offset| offset.starts_with("0x")),
        "{native}"
    );
}

/// The runbooks' release-build hook: with `BIBCODE_PROFILER_CRASH_TEST=1` an armed process
/// faults by itself 5 s later, here well before the 60 s it would otherwise sleep.
#[test]
#[cfg_attr(
    all(windows, target_arch = "aarch64"),
    ignore = "minidump-writer unimplemented on Windows arm64"
)]
fn the_crash_test_hook_faults_five_seconds_after_arming() {
    let dir = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let status = Command::new(SMOKE)
        .arg(dir.path())
        .env("CRASH_SMOKE_MODE", "arm-and-sleep")
        .env("BIBCODE_PROFILER_CRASH_TEST", "1")
        .stdout(Stdio::null())
        .status()
        .unwrap();
    let elapsed = started.elapsed();
    assert!(!status.success());
    assert!(
        elapsed >= Duration::from_secs(5) && elapsed < Duration::from_secs(30),
        "{elapsed:?}"
    );
    let dumps = std::fs::read_dir(dir.path().join("crashes"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "dmp"))
        .count();
    assert_eq!(dumps, 1);
}

#[test]
#[cfg_attr(
    all(windows, target_arch = "aarch64"),
    ignore = "minidump-writer unimplemented on Windows arm64"
)]
fn the_monitor_exits_when_its_parent_is_killed() {
    let dir = tempfile::tempdir().unwrap();
    let mut smoke = spawn_armed(dir.path(), &[]);
    let parent = smoke.id();
    assert_eq!(
        monitors_of(parent).len(),
        1,
        "the armed process runs one monitor"
    );
    // Its socket is in a fresh directory of its own, which only this user can enter.
    let socket_dir = monitor_socket(parent)
        .parent()
        .expect("the socket has a directory")
        .to_path_buf();
    let name = socket_dir
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(name.starts_with("bibcode-"), "{}", socket_dir.display());
    #[cfg(unix)]
    assert_eq!(mode(&socket_dir), 0o700);

    smoke.kill().unwrap();
    smoke.wait().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !monitors_of(parent).is_empty() {
        assert!(
            Instant::now() < deadline,
            "the monitor outlived its parent by 5 s"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        !socket_dir.exists(),
        "the monitor removes its socket directory"
    );
}

/// The monitor's socket lives in a fresh directory that only this user can enter (0700, the
/// socket 0600): no other local user can connect to the monitor or plant the path first. A
/// directory that already exists is refused rather than used, and stopping removes the socket
/// with its directory. One test, because the detector arms this process's global hook.
#[test]
#[cfg_attr(
    all(windows, target_arch = "aarch64"),
    ignore = "minidump-writer unimplemented on Windows arm64"
)]
fn the_monitor_socket_is_private_and_never_reused() {
    // The monitor serves the crash hook: without one attached it never starts.
    crash::attach();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let sockets = tempfile::tempdir().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let fresh = sockets.path().join("fresh");
        let (cancel, task) = start_monitor(dir.path(), &fresh);
        wait_for_native_dumps(dir.path(), "available").await;
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;
            assert_eq!(mode(&fresh), 0o700);
            let socket = fresh.join("monitor.sock");
            assert!(
                std::fs::symlink_metadata(&socket)
                    .unwrap()
                    .file_type()
                    .is_socket()
            );
            assert_eq!(mode(&socket), 0o600);
        }
        cancel.cancel();
        task.await.unwrap();
        assert!(
            !fresh.exists(),
            "stopping removes the socket and its directory"
        );

        let dir = tempfile::tempdir().unwrap();
        let taken = sockets.path().join("taken");
        std::fs::create_dir(&taken).unwrap();
        let (cancel, task) = start_monitor(dir.path(), &taken);
        wait_for_native_dumps(dir.path(), "unavailable").await;
        assert!(
            monitors_using(&taken).is_empty(),
            "no monitor was started on a path that already existed"
        );
        cancel.cancel();
        task.await.unwrap();
        assert!(
            taken.exists(),
            "a directory it did not create is left alone"
        );
    });
}

fn start_monitor(
    dir: &Path,
    socket_dir: &Path,
) -> (CancellationToken, tokio::task::JoinHandle<()>) {
    let cancel = CancellationToken::new();
    let detector = MonitorDetector::new(dir.to_path_buf(), "crash-smoke")
        .with_executable(SMOKE.into())
        .with_socket_dir(socket_dir.to_path_buf());
    let task = detector.start(cancel.clone());
    (cancel, task)
}

async fn wait_for_native_dumps(dir: &Path, expected: &str) {
    for _ in 0..100 {
        let recorded = std::fs::read(dir.join("capabilities.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        if recorded.is_some_and(|capabilities| capabilities["nativeDumps"] == expected) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("nativeDumps never became {expected}");
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777
}

/// The 10 s ping replaces a monitor that died, and one that stopped answering (after the 5 s
/// ping timeout), so a monitor failure never leaves the process uncovered for long.
#[test]
#[cfg_attr(
    all(windows, target_arch = "aarch64"),
    ignore = "minidump-writer unimplemented on Windows arm64"
)]
fn a_dead_or_hung_monitor_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let mut smoke = spawn_armed(dir.path(), &[]);
    let parent = smoke.id();
    let first = monitors_of(parent);
    assert_eq!(first.len(), 1);

    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::Some(&[first[0]]), true);
    assert!(system.process(first[0]).unwrap().kill());
    let second = replacement(parent, first[0], Duration::from_secs(20));
    assert_ne!(second, first[0]);

    #[cfg(unix)]
    {
        // Alive but deaf.
        // SAFETY: a plain signal to a process this test owns.
        assert_eq!(
            unsafe { libc::kill(second.as_u32() as libc::pid_t, libc::SIGSTOP) },
            0
        );
        replacement(parent, second, Duration::from_secs(25));
    }

    smoke.kill().unwrap();
    smoke.wait().unwrap();
}

/// A crash while the monitor hangs must still end the process: the hook may wait on the
/// monitor, but the detector replaces a monitor that stops answering, which releases it.
#[cfg(target_os = "linux")]
#[test]
fn a_crash_while_the_monitor_hangs_still_ends_the_process() {
    use std::os::unix::process::ExitStatusExt;

    let dir = tempfile::tempdir().unwrap();
    // A real fault, 5 s after arming: a signal sent with `kill` would not fault again once
    // the hook declines it, so it could not end the process.
    let mut smoke = spawn_armed(dir.path(), &[("BIBCODE_PROFILER_CRASH_TEST", "1")]);
    let parent = smoke.id();
    let monitors = monitors_of(parent);
    assert_eq!(monitors.len(), 1);
    let monitor = monitors[0].as_u32() as libc::pid_t;
    // SAFETY: a plain signal to a process this test owns.
    assert_eq!(unsafe { libc::kill(monitor, libc::SIGSTOP) }, 0);
    let deadline = Instant::now() + Duration::from_secs(40);
    let status = loop {
        if let Some(status) = smoke.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = smoke.kill();
            let _ = smoke.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    // A stopped monitor cannot notice its parent is gone.
    // SAFETY: as above.
    unsafe { libc::kill(monitor, libc::SIGKILL) };
    let status = status.expect("the crashed process hung on its stopped monitor");
    assert_eq!(status.signal(), Some(libc::SIGSEGV));
}

/// Starts `crash-smoke` armed and asleep, once its monitor is connected.
fn spawn_armed(dir: &std::path::Path, env: &[(&str, &str)]) -> std::process::Child {
    let mut smoke = Command::new(SMOKE)
        .arg(dir)
        .env("CRASH_SMOKE_MODE", "arm-and-sleep")
        .envs(env.iter().copied())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(smoke.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(line.trim(), "armed");
    smoke
}

/// Waits until `parent`'s only monitor is another process than `old`.
fn replacement(parent: u32, old: Pid, within: Duration) -> Pid {
    let deadline = Instant::now() + within;
    loop {
        if let [monitor] = monitors_of(parent)[..]
            && monitor != old
        {
            return monitor;
        }
        assert!(Instant::now() < deadline, "no replacement monitor");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The socket path in the argv of `parent`'s monitor.
fn monitor_socket(parent: u32) -> PathBuf {
    let parent = parent.to_string();
    monitor_commands()
        .into_iter()
        .find(|cmd| cmd.last().is_some_and(|arg| *arg == *parent))
        .and_then(|cmd| {
            let flag = cmd.iter().position(|arg| arg == "--crash-monitor")?;
            cmd.get(flag + 1).map(PathBuf::from)
        })
        .expect("a monitor with its socket in its argv")
}

/// Live monitors whose argv names a path inside `dir`.
fn monitors_using(dir: &Path) -> Vec<Vec<std::ffi::OsString>> {
    monitor_commands()
        .into_iter()
        .filter(|cmd| cmd.iter().any(|arg| Path::new(arg).starts_with(dir)))
        .collect()
}

fn monitor_commands() -> Vec<Vec<std::ffi::OsString>> {
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .without_tasks()
            .with_cmd(UpdateKind::Always),
    );
    system
        .processes()
        .values()
        .map(|process| process.cmd().to_vec())
        .filter(|cmd| cmd.iter().any(|arg| arg == "--crash-monitor"))
        .collect()
}

/// Live `--crash-monitor` processes whose argv names `parent` as the process they watch. Once
/// the parent dies the monitor is reparented, so its OS parent no longer identifies it.
fn monitors_of(parent: u32) -> Vec<Pid> {
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .without_tasks()
            .with_cmd(UpdateKind::Always),
    );
    let parent = parent.to_string();
    system
        .processes()
        .values()
        .filter(|process| {
            let cmd = process.cmd();
            cmd.iter().any(|arg| arg == "--crash-monitor")
                && cmd.last().is_some_and(|arg| *arg == *parent)
        })
        .map(sysinfo::Process::pid)
        .collect()
}
