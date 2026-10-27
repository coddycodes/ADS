use super::*;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::{
    ffi::OsString,
    io::Read,
    os::unix::ffi::{OsStrExt, OsStringExt},
};

const PARENT_FIXTURE: &str = "watchdog_tests::child_watchdog_parent_fixture";
const READY_PATH: &str = "CODDY_STUDIO_WATCHDOG_PARENT_READY";

pub(super) struct OwnedPeer(std::process::Child);

impl OwnedPeer {
    pub(super) fn start() -> Self {
        Self(
            Command::new("sleep")
                .arg("30")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0)
                .spawn()
                .expect("owned independent peer"),
        )
    }

    pub(super) fn assert_alive(&mut self) {
        assert!(self.0.try_wait().expect("poll owned peer").is_none());
    }
}

impl Drop for OwnedPeer {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn exists(pid: libc::pid_t) -> bool {
    // SAFETY: signal zero observes only an exact PID published by our fixture.
    if unsafe { libc::kill(pid, 0) } == 0 {
        true
    } else {
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        false
    }
}

fn reap_adopted(pid: libc::pid_t) {
    #[cfg(target_os = "linux")]
    {
        let mut status = 0;
        // SAFETY: the isolated subreaper can adopt only these known fixture PIDs.
        let result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if result < 0 {
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ECHILD)
            );
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = pid;
}

// Failure cleanup is limited to the still-live, handshake-proven fixture group.
// The leader blocks in `wait` for 30 seconds, so a RED cannot leave test children.
struct FixtureCleanup {
    parent: std::process::Child,
    group: Option<libc::pid_t>,
    pids: Vec<libc::pid_t>,
}

impl Drop for FixtureCleanup {
    fn drop(&mut self) {
        if let Some(group) = self.group
            && exists(group)
        {
            // SAFETY: the handshake identifies this test's still-live leader.
            unsafe { libc::kill(-group, libc::SIGKILL) };
        }
        let _ = self.parent.kill();
        let _ = self.parent.wait();
        for _ in 0..100 {
            for &pid in &self.pids {
                reap_adopted(pid);
            }
            if self.pids.iter().all(|&pid| !exists(pid)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
#[ignore = "private parent-death fixture; selected only with its command-local marker"]
fn child_watchdog_parent_fixture() {
    let Some(ready) = std::env::var_os(READY_PATH) else {
        return;
    };
    assert!(std::env::args().any(|arg| arg == PARENT_FIXTURE));
    // SAFETY: this explicitly selected fixture owns its isolated signal policy.
    unsafe { libc::signal(libc::SIGINT, libc::SIG_DFL) };
    run_child_with_deadline(
        ChildCommandSpec::new("sh")
            .args([
                "-c",
                "sleep 30 & printf '%s %s\\n' \"$$\" \"$!\" > \"$1\"; wait",
                "watchdog-ready",
            ])
            .arg(ready),
        "parent-death-fixture",
        Duration::from_secs(20),
    );
}

fn parent_death(signal: libc::c_int) {
    let _guard = child_process_guard();
    let state = TempDir::new().expect("watchdog state");
    let ready = state.path().join("ready");
    let mut peer = OwnedPeer::start();
    let parent = Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--ignored",
            "--exact",
            PARENT_FIXTURE,
            "--nocapture",
            "--test-threads=1",
        ])
        .env(READY_PATH, &ready)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("owned parent fixture");
    let mut cleanup = FixtureCleanup {
        parent,
        group: None,
        pids: Vec::new(),
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    let pids = loop {
        if let Ok(text) = std::fs::read_to_string(&ready) {
            let pids: Vec<libc::pid_t> = text
                .split_whitespace()
                .map(|pid| pid.parse().expect("fixture numeric PID"))
                .collect();
            if pids.len() == 2 {
                break pids;
            }
        }
        assert!(
            Instant::now() < deadline,
            "parent/child/grandchild readiness"
        );
        assert!(
            cleanup
                .parent
                .try_wait()
                .expect("parent readiness poll")
                .is_none()
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    // SAFETY: these exact PIDs were published by our ready fixture.
    let group = unsafe { libc::getpgid(pids[0]) };
    assert!(group > 0);
    assert_ne!(
        group,
        libc::pid_t::try_from(cleanup.parent.id()).expect("parent PID")
    );
    cleanup.group = Some(group);
    cleanup.pids = vec![group, pids[0], pids[1]];
    assert!(cleanup.pids.iter().all(|&pid| exists(pid)));
    peer.assert_alive();
    // SAFETY: signal only our direct child, never its separate monitor group.
    assert_eq!(
        unsafe { libc::kill(cleanup.parent.id() as libc::pid_t, signal) },
        0
    );
    assert_eq!(
        cleanup.parent.wait().expect("reap parent").signal(),
        Some(signal)
    );
    let disappearance = Instant::now() + Duration::from_secs(2);
    loop {
        for &pid in &cleanup.pids {
            reap_adopted(pid);
        }
        if cleanup.pids.iter().all(|&pid| !exists(pid)) {
            break;
        }
        assert!(
            Instant::now() < disappearance,
            "owned child/grandchild/monitor survived parent death: {:?}",
            cleanup.pids
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    // SAFETY: signal zero checks the exact owned process group after disappearance.
    assert_eq!(unsafe { libc::kill(-group, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    cleanup.group = None;
    peer.assert_alive();
}

#[test]
fn child_watchdog_parent_sigkill_cleans_child_and_grandchild() {
    #[cfg(target_os = "linux")]
    {
        const TEST: &str =
            "watchdog_tests::child_watchdog_parent_sigkill_cleans_child_and_grandchild";
        let Some(phase) = reexec::enter(TEST, "subreaper") else {
            reexec::run(TEST, "subreaper", None, |_| {});
            return;
        };
        // SAFETY: subreaping is confined to this exactly selected fixture process.
        assert_eq!(unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) }, 0);
        parent_death(libc::SIGKILL);
        phase.complete();
    }
    #[cfg(not(target_os = "linux"))]
    parent_death(libc::SIGKILL);
}

#[test]
fn child_watchdog_parent_sigint_cleans_child_and_grandchild() {
    #[cfg(target_os = "linux")]
    {
        const TEST: &str =
            "watchdog_tests::child_watchdog_parent_sigint_cleans_child_and_grandchild";
        let Some(phase) = reexec::enter(TEST, "subreaper") else {
            reexec::run(TEST, "subreaper", None, |_| {});
            return;
        };
        // SAFETY: subreaping is confined to this exactly selected fixture process.
        assert_eq!(unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) }, 0);
        parent_death(libc::SIGINT);
        phase.complete();
    }
    #[cfg(not(target_os = "linux"))]
    parent_death(libc::SIGINT);
}

fn normal_exit_with_pipe_holder() {
    let _guard = child_process_guard();
    let mut peer = OwnedPeer::start();
    let started = Instant::now();
    let output = run_child_with_deadline(
        ChildCommandSpec::new("sh").args([
            "-c",
            "sleep 3 & printf 'monitor=%s\\ndescendant=%s\\n' \"$PPID\" \"$!\"; printf 'root stderr\\n' >&2; exit 0",
        ]),
        "normal-pipe-holder",
        Duration::from_secs(5),
    );
    let text = String::from_utf8(output.stdout).expect("fixture output");
    let monitor = text
        .lines()
        .next()
        .expect("monitor proof")
        .strip_prefix("monitor=")
        .expect("monitor field")
        .parse::<libc::pid_t>()
        .expect("monitor PID");
    let descendant = text
        .lines()
        .nth(1)
        .expect("descendant proof")
        .strip_prefix("descendant=")
        .expect("root output")
        .parse::<libc::pid_t>()
        .expect("descendant PID");
    assert!(output.status.success());
    assert_eq!(output.stderr, b"root stderr\n");
    assert!(
        !exists(monitor),
        "monitor must be absent before helper returns"
    );
    let mut status = 0;
    // SAFETY: query only the completed helper's monitor, which it already reaped.
    assert_eq!(
        unsafe { libc::waitpid(monitor, &mut status, libc::WNOHANG) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
    let deadline = Instant::now() + Duration::from_secs(1);
    while exists(descendant) {
        reap_adopted(descendant);
        assert!(Instant::now() < deadline, "normal exit descendant cleanup");
        std::thread::sleep(Duration::from_millis(5));
    }
    peer.assert_alive();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "normal child exit must not wait for descendant pipe EOF"
    );
}

#[test]
fn child_watchdog_normal_exit_cleans_a_descendant_holding_output() {
    #[cfg(target_os = "linux")]
    {
        const TEST: &str =
            "watchdog_tests::child_watchdog_normal_exit_cleans_a_descendant_holding_output";
        let Some(phase) = reexec::enter(TEST, "subreaper") else {
            reexec::run(TEST, "subreaper", None, |_| {});
            return;
        };
        // SAFETY: subreaping is confined to this exactly selected fixture process.
        assert_eq!(unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) }, 0);
        normal_exit_with_pipe_holder();
        phase.complete();
    }
    #[cfg(not(target_os = "linux"))]
    normal_exit_with_pipe_holder();
}

const CONFIG_TEST: &str = "watchdog_tests::child_watchdog_preserves_lossless_command_configuration";
const CONFIG_FIXTURE: &str = "watchdog_tests::child_watchdog_configuration_fixture";
const CONFIG_PARENT: &str = "CODDY_STUDIO_WATCHDOG_CONFIG_PARENT";
const CONFIG_MODE: &str = "CODDY_STUDIO_WATCHDOG_CONFIG_MODE";

#[test]
#[ignore = "private lossless command fixture"]
fn child_watchdog_configuration_fixture() {
    let Some(mode) = std::env::var_os(CONFIG_MODE) else {
        return;
    };
    #[cfg(target_os = "macos")]
    let cwd_name: &[u8] = b"cwd";
    #[cfg(not(target_os = "macos"))]
    let cwd_name: &[u8] = b"cwd\xfe";
    assert_eq!(
        std::env::current_dir()
            .expect("cwd")
            .file_name()
            .expect("cwd name")
            .as_bytes(),
        cwd_name
    );
    assert_eq!(
        std::env::var_os("CODDY_STUDIO_WATCHDOG_OVERRIDE")
            .expect("override")
            .as_bytes(),
        b"override\xfd"
    );
    assert_eq!(
        std::env::var_os(OsString::from_vec(b"CODDY_STUDIO_KEY\xff".to_vec()))
            .expect("binary key")
            .as_bytes(),
        b"value\xfe"
    );
    assert!(std::env::var_os("CODDY_STUDIO_WATCHDOG_REMOVE").is_none());
    assert!(std::env::var_os("CODDY_STUDIO_WATCHDOG_DISCARDED").is_none());
    match mode.to_str().expect("mode") {
        "inherit" => {
            assert_eq!(
                std::env::var("CODDY_STUDIO_WATCHDOG_INHERITED").as_deref(),
                Ok("inherited")
            );
            assert_eq!(
                std::env::var_os(child_watchdog::SELECTOR)
                    .expect("restored selector")
                    .as_bytes(),
                b"original\xff"
            );
        }
        "override" => {
            assert_eq!(
                std::env::var_os(child_watchdog::SELECTOR)
                    .expect("explicit selector")
                    .as_bytes(),
                b"explicit\xfe"
            );
        }
        "remove" | "clear" => {
            assert!(std::env::var_os(child_watchdog::SELECTOR).is_none());
            if mode == "clear" {
                assert!(std::env::var_os("CODDY_STUDIO_WATCHDOG_INHERITED").is_none());
            }
        }
        _ => panic!("unknown fixture mode"),
    }
    let mut byte = [0];
    assert_eq!(
        std::io::stdin().read(&mut byte).expect("null child stdin"),
        0
    );
}

#[test]
fn child_watchdog_preserves_lossless_command_configuration() {
    if std::env::var(CONFIG_PARENT).as_deref() != Ok("selected") {
        let output = Command::new(std::env::current_exe().expect("test executable"))
            .args(["--exact", CONFIG_TEST, "--nocapture", "--test-threads=1"])
            .env(CONFIG_PARENT, "selected")
            .env("CODDY_STUDIO_WATCHDOG_INHERITED", "inherited")
            .env("CODDY_STUDIO_WATCHDOG_REMOVE", "removed")
            .env(
                child_watchdog::SELECTOR,
                OsString::from_vec(b"original\xff".to_vec()),
            )
            .output()
            .expect("command-local configuration parent");
        assert!(
            output.status.success(),
            "configuration parent: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let _guard = child_process_guard();
    let state = TempDir::new().expect("configuration state");
    // The native macOS fixture filesystem rejects non-UTF-8 names (EILSEQ).
    // The lossless request codec
    // covers those path bytes on every Unix host; Linux also exercises exec/cwd.
    #[cfg(target_os = "macos")]
    let cwd_name = b"cwd".to_vec();
    #[cfg(not(target_os = "macos"))]
    let cwd_name = b"cwd\xfe".to_vec();
    let cwd = state.path().join(OsString::from_vec(cwd_name));
    std::fs::create_dir(&cwd).expect("binary cwd");
    for mode in ["inherit", "override", "remove", "clear"] {
        let mut spec = ChildCommandSpec::new(std::env::current_exe().expect("test executable"));
        spec.args([
            "--ignored",
            "--exact",
            CONFIG_FIXTURE,
            "--nocapture",
            "--test-threads=1",
        ]);
        spec.env("CODDY_STUDIO_WATCHDOG_DISCARDED", "discarded");
        if mode == "clear" {
            spec.env_clear();
        } else {
            spec.env_remove("CODDY_STUDIO_WATCHDOG_DISCARDED");
        }
        spec.current_dir(&cwd)
            .env(CONFIG_MODE, mode)
            .env("CODDY_STUDIO_WATCHDOG_OVERRIDE", "earlier")
            .env(
                "CODDY_STUDIO_WATCHDOG_OVERRIDE",
                OsString::from_vec(b"override\xfd".to_vec()),
            )
            .env(
                OsString::from_vec(b"CODDY_STUDIO_KEY\xff".to_vec()),
                OsString::from_vec(b"value\xfe".to_vec()),
            )
            .env_remove("CODDY_STUDIO_WATCHDOG_REMOVE");
        if mode == "override" {
            spec.env(
                child_watchdog::SELECTOR,
                OsString::from_vec(b"explicit\xfe".to_vec()),
            );
        }
        if mode == "remove" {
            spec.env_remove(child_watchdog::SELECTOR);
        }
        let output = run_child_with_deadline(&mut spec, mode, Duration::from_secs(3));
        assert!(
            output.status.success(),
            "{mode}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "fixture must actually execute"
        );
    }
    let output = run_child_with_deadline(
        ChildCommandSpec::new("sh").args([
            OsString::from("-c"),
            OsString::from("printf '%s' \"$1\""),
            OsString::from("binary-argument"),
            OsString::from_vec(b"arg\xff".to_vec()),
        ]),
        "binary-argument",
        Duration::from_secs(3),
    );
    assert!(output.status.success());
    assert_eq!(output.stdout, b"arg\xff");
}

#[test]
fn child_watchdog_preserves_binary_output_and_real_signal_status() {
    let _guard = child_process_guard();
    for (script, code, signal) in [
        (
            "printf '\\000\\377\\004\\004\\000\\000\\000\\000'; printf '\\376\\000' >&2; exit 0",
            Some(0),
            None,
        ),
        (
            "printf '\\000\\377\\004\\004\\000\\000\\000\\000'; printf '\\376\\000' >&2; exit 7",
            Some(7),
            None,
        ),
        (
            "printf '\\000\\377\\004\\004\\000\\000\\000\\000'; printf '\\376\\000' >&2; kill -ABRT \"$$\"",
            None,
            Some(libc::SIGABRT),
        ),
    ] {
        let output = run_child_with_deadline(
            ChildCommandSpec::new("sh").args(["-c", script]),
            "binary-result",
            Duration::from_secs(3),
        );
        assert_eq!(output.stdout, [0, 255, 4, 4, 0, 0, 0, 0]);
        assert_eq!(output.stderr, [254, 0]);
        assert_eq!(output.status.code(), code);
        assert_eq!(output.status.signal(), signal);
    }
}

const CONTINUOUS_WRITER_TEST: &str = "watchdog_tests::child_watchdog_continuous_writer_fixture";
const CONTINUOUS_WRITER_READY: &str = "CODDY_STUDIO_WATCHDOG_CONTINUOUS_WRITER_READY";

#[test]
#[ignore = "private captured-pipe backpressure fixture"]
fn child_watchdog_continuous_writer_fixture() {
    use std::os::fd::AsRawFd;

    let Some(ready) = std::env::var_os(CONTINUOUS_WRITER_READY).map(PathBuf::from) else {
        return;
    };
    let expected = [
        "--ignored",
        "--exact",
        CONTINUOUS_WRITER_TEST,
        "--nocapture",
        "--test-threads=1",
    ];
    if !std::env::args_os().skip(1).eq(expected.map(OsString::from)) {
        return;
    }
    let stdout = std::io::stdout();
    let fd = stdout.as_raw_fd();
    // SAFETY: this exact fixture retains its own captured stdout descriptor.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    assert!(flags >= 0, "read captured stdout flags");
    // SAFETY: only the fixture's captured pipe is made nonblocking, preserving
    // CLOEXEC. No host/parent descriptor or process-global environment changes.
    assert_eq!(
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) },
        0
    );
    let bytes = b"continuous descendant output\n".repeat(1024);
    let mut written = 0usize;
    let mut published = false;
    loop {
        // SAFETY: bytes lives through the write and fd remains this owned pipe.
        let count = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if count > 0 {
            written = written
                .checked_add(count as usize)
                .expect("written byte count");
            continue;
        }
        assert_eq!(
            count, -1,
            "captured stdout must accept data or report backpressure"
        );
        let error = std::io::Error::last_os_error();
        match error.kind() {
            std::io::ErrorKind::Interrupted => continue,
            std::io::ErrorKind::WouldBlock => {
                if !published && written >= 16 * 1024 {
                    // Root cannot exit until this writer itself proves actual
                    // successful output and a full captured pipe. Atomic rename
                    // publishes complete PID/count/backpressure evidence.
                    let pending = ready.with_extension("pending");
                    std::fs::write(
                        &pending,
                        format!("{} {written} backpressured\n", std::process::id()),
                    )
                    .expect("writer readiness");
                    std::fs::rename(&pending, &ready).expect("publish writer readiness");
                    published = true;
                }
            }
            _ => panic!("write continuous descendant output: {error}"),
        }
    }
}

fn continuous_descendant_output() {
    let _guard = child_process_guard();
    let state = TempDir::new().expect("continuous writer state");
    let ready = state.path().join("descendant");
    let mut peer = OwnedPeer::start();
    let started = Instant::now();
    let result = std::panic::catch_unwind(|| {
        run_child_with_deadline(
        ChildCommandSpec::new("sh").args([
            "-c", "printf 'root output\\n'; \"$1\" --ignored --exact \"$3\" --nocapture --test-threads=1 & while [ ! -f \"$2\" ]; do :; done; printf 'root released after writer backpressure\\n' >&2; exit 0", "continuous-writer",
        ]).arg(std::env::current_exe().expect("test executable")).arg(&ready).arg(CONTINUOUS_WRITER_TEST).env(CONTINUOUS_WRITER_READY, &ready), "continuous-output-fixture", Duration::from_millis(250),
    )
    });
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "continuous output must not evade the deadline"
    );
    let readiness = std::fs::read_to_string(&ready).expect("descendant readiness");
    let mut proof = readiness.split_whitespace();
    let descendant = proof
        .next()
        .expect("descendant PID")
        .parse::<libc::pid_t>()
        .expect("descendant PID");
    let deadline = Instant::now() + Duration::from_secs(1);
    while exists(descendant) {
        reap_adopted(descendant);
        assert!(Instant::now() < deadline, "continuous descendant cleanup");
        std::thread::sleep(Duration::from_millis(5));
    }
    let written = proof
        .next()
        .expect("writer must publish its actual output count")
        .parse::<usize>()
        .expect("written byte count");
    assert!(
        written >= 16 * 1024,
        "writer must establish a real output workload"
    );
    assert_eq!(
        proof.next(),
        Some("backpressured"),
        "writer must observe a full captured pipe before root exit"
    );
    match result {
        Ok(output) => {
            assert!(output.status.success());
            assert!(output.stdout.starts_with(b"root output\n"));
            assert!(
                output
                    .stdout
                    .windows(b"continuous descendant output\n".len())
                    .any(|bytes| bytes == b"continuous descendant output\n")
            );
            assert_eq!(output.stderr, b"root released after writer backpressure\n");
        }
        Err(panic) => {
            let report = panic.downcast_ref::<String>().expect("diagnostic panic");
            assert!(report.contains("continuous-output-fixture"), "{report}");
            assert!(report.contains("root output\n"), "{report}");
            assert!(
                report.contains("continuous descendant output\n"),
                "descendant output was not observed"
            );
            assert!(
                report.contains("root released after writer backpressure\n"),
                "root did not release after writer readiness"
            );
            assert!(
                (report.starts_with("continuous-output-fixture child (pid=")
                    && report.contains("timed out after")
                    && report.contains("deadline 250ms")
                    && report.contains("output incomplete: monitor stopped at the absolute deadline"))
                || report.starts_with("continuous-output-fixture monitor protocol failed: child monitor failed: child output drain reached deadline\n"),
                "unexpected helper failure: {report}"
            );
        }
    }
    peer.assert_alive();
    eprintln!(
        "continuous writer proved {written} bytes before observed backpressure; output, owned cleanup, and peer survival verified"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "continuous output and owned cleanup must finish within the original bound"
    );
}

#[test]
fn child_watchdog_continuous_descendant_output_obeys_deadline_and_preserves_peer() {
    #[cfg(target_os = "linux")]
    {
        const TEST: &str = "watchdog_tests::child_watchdog_continuous_descendant_output_obeys_deadline_and_preserves_peer";
        let Some(phase) = reexec::enter(TEST, "subreaper") else {
            reexec::run(TEST, "subreaper", None, |_| {});
            return;
        };
        // SAFETY: only the exact isolated fixture owns adopted descendants.
        assert_eq!(unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) }, 0);
        continuous_descendant_output();
        phase.complete();
    }
    #[cfg(not(target_os = "linux"))]
    continuous_descendant_output();
}
