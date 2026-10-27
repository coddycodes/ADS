//! Unix-only, private recovery-test child lifetime ownership.
use super::child_command::ChildCommandSpec;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, RawFd},
        unix::{
            ffi::{OsStrExt, OsStringExt},
            process::{CommandExt, ExitStatusExt},
        },
    },
    path::PathBuf,
    process::{Child, ChildStdin, Command, ExitStatus, Output, Stdio},
    time::{Duration, Instant},
};

pub(super) const MONITOR_TEST: &str = "child_watchdog::child_watchdog_monitor_fixture";
pub(super) const SELECTOR: &str = "CODDY_STUDIO_TEST_CHILD_MONITOR";
const VERSION: &str = "lease-v1";
const BLOCKED_CONFIGURATION: &str = "lease-v1:blocked-configuration-fixture";
const MAX_REQUEST: usize = 8 * 1024 * 1024;
const CHUNK: usize = 16 * 1024;
const READY: u8 = 1;
const STDOUT: u8 = 2;
const STDERR: u8 = 3;
const COMPLETE: u8 = 4;
const FAILURE: u8 = 5;
const CLEANUP_BUDGET: Duration = Duration::from_secs(1);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    program: Vec<u8>,
    args: Vec<Vec<u8>>,
    current_dir: Option<Vec<u8>>,
    inherit_environment: bool,
    env: Vec<(Vec<u8>, Option<Vec<u8>>)>,
    original_selector: Option<Vec<u8>>,
    remaining_nanos: u64,
}

impl Request {
    fn from_spec(spec: &ChildCommandSpec, remaining: Duration) -> io::Result<Self> {
        Ok(Self {
            program: spec.program.as_bytes().to_vec(),
            args: spec
                .args
                .iter()
                .map(|arg| arg.as_bytes().to_vec())
                .collect(),
            current_dir: spec
                .current_dir
                .as_ref()
                .map(|path| path.as_os_str().as_bytes().to_vec()),
            inherit_environment: spec.inherit_environment,
            env: spec
                .env
                .iter()
                .map(|(key, value)| {
                    (
                        key.as_bytes().to_vec(),
                        value.as_ref().map(|value| value.as_bytes().to_vec()),
                    )
                })
                .collect(),
            original_selector: std::env::var_os(SELECTOR).map(|value| value.as_bytes().to_vec()),
            remaining_nanos: u64::try_from(remaining.as_nanos())
                .map_err(|_| invalid("child deadline is too large"))?,
        })
    }

    fn command(self) -> io::Result<Command> {
        if self.remaining_nanos == 0 {
            return Err(invalid("expired child deadline"));
        }
        if self.program.is_empty()
            || self.program.contains(&0)
            || self.args.iter().any(|arg| arg.contains(&0))
            || self
                .current_dir
                .as_ref()
                .is_some_and(|path| path.contains(&0))
            || self
                .original_selector
                .as_ref()
                .is_some_and(|value| value.contains(&0))
        {
            return Err(invalid("invalid child program, argument, cwd, or selector"));
        }
        let mut env = BTreeMap::new();
        for (key, value) in self.env {
            if key.is_empty()
                || key.contains(&0)
                || key.contains(&b'=')
                || value.as_ref().is_some_and(|value| value.contains(&0))
            {
                return Err(invalid("invalid child environment key or value"));
            }
            if env
                .insert(OsString::from_vec(key), value.map(OsString::from_vec))
                .is_some()
            {
                return Err(invalid("duplicate child environment key"));
            }
        }
        let spec = ChildCommandSpec {
            program: OsString::from_vec(self.program),
            args: self.args.into_iter().map(OsString::from_vec).collect(),
            current_dir: self
                .current_dir
                .map(|path| PathBuf::from(OsString::from_vec(path))),
            inherit_environment: self.inherit_environment,
            env,
        };
        // Restore monitor-only control state before explicit overrides/removals.
        let mut command = spec.command();
        if spec.inherit_environment && !spec.env.contains_key(std::ffi::OsStr::new(SELECTOR)) {
            if let Some(value) = self.original_selector {
                command.env(SELECTOR, OsString::from_vec(value));
            } else {
                command.env_remove(SELECTOR);
            }
        }
        Ok(command)
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn nonblocking(fd: RawFd) -> io::Result<()> {
    // SAFETY: callers retain these owned descriptors through both fcntl calls.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// At most one bounded batch per poll, so a continuous writer cannot starve
/// the deadline, the other stream, configuration transmission, or child wait.
fn read_available(
    pipe: &mut impl Read,
    mut consume: impl FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<bool> {
    let mut buffer = [0; CHUNK];
    for _ in 0..16 {
        match pipe.read(&mut buffer) {
            Ok(0) => return Ok(true),
            Ok(count) => consume(&buffer[..count])?,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

fn append(bytes: &mut Vec<u8>, incoming: &[u8]) -> io::Result<()> {
    bytes
        .try_reserve(incoming.len())
        .map_err(|_| invalid("child output allocation failed"))?;
    bytes.extend_from_slice(incoming);
    Ok(())
}

fn frame(writer: &mut impl Write, kind: u8, bytes: &[u8]) -> io::Result<()> {
    if bytes.len() > CHUNK {
        return Err(invalid("oversized child result frame"));
    }
    writer.write_all(&[kind])?;
    writer.write_all(&(bytes.len() as u32).to_le_bytes())?;
    writer.write_all(bytes)
}

#[derive(Default)]
struct ResultFrames {
    pending: Vec<u8>,
    ready: bool,
    status: Option<ExitStatus>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl ResultFrames {
    fn receive(&mut self, bytes: &[u8], monitor_pid: u32) -> io::Result<()> {
        append(&mut self.pending, bytes)?;
        let mut consumed = 0;
        while self.pending.len() - consumed >= 5 {
            let kind = self.pending[consumed];
            let length = u32::from_le_bytes(
                self.pending[consumed + 1..consumed + 5]
                    .try_into()
                    .expect("four-byte length"),
            ) as usize;
            if length > CHUNK {
                return Err(invalid("oversized child result frame"));
            }
            let end = consumed + 5 + length;
            if end > self.pending.len() {
                break;
            }
            let payload = &self.pending[consumed + 5..end];
            if self.status.is_some() {
                return Err(invalid("child result after completion"));
            }
            match kind {
                READY if !self.ready && payload == monitor_pid.to_le_bytes() => self.ready = true,
                STDOUT if self.ready => append(&mut self.stdout, payload)?,
                STDERR if self.ready => append(&mut self.stderr, payload)?,
                COMPLETE if self.ready && length == 4 => {
                    let raw = i32::from_le_bytes(payload.try_into().expect("four-byte status"));
                    if !libc::WIFEXITED(raw) && !libc::WIFSIGNALED(raw) {
                        return Err(invalid("child completion is not a terminal wait status"));
                    }
                    self.status = Some(ExitStatus::from_raw(raw));
                }
                FAILURE if self.ready => {
                    return Err(io::Error::other(format!(
                        "child monitor failed: {}",
                        String::from_utf8_lossy(payload)
                    )));
                }
                _ => return Err(invalid("unexpected child result frame")),
            }
            consumed = end;
        }
        self.pending.drain(..consumed);
        Ok(())
    }

    fn finish(self) -> io::Result<Output> {
        if !self.pending.is_empty() {
            return Err(invalid("truncated child result frame"));
        }
        Ok(Output {
            status: self
                .status
                .ok_or_else(|| invalid("missing child completion"))?,
            stdout: self.stdout,
            stderr: self.stderr,
        })
    }
}

/// Never polls/reaps the monitor until its owned group has been signaled.
/// An exited leader remains our zombie, retaining PID/PGID identity until wait.
struct MonitorOwner {
    child: Child,
    lease: Option<ChildStdin>,
    reaped: bool,
}

impl MonitorOwner {
    fn spawn() -> io::Result<Self> {
        Self::spawn_marker(VERSION)
    }

    fn spawn_marker(marker: &str) -> io::Result<Self> {
        let mut child = Command::new(std::env::current_exe()?)
            .args([
                "--ignored",
                "--exact",
                MONITOR_TEST,
                "--nocapture",
                "--test-threads=1",
            ])
            .env(SELECTOR, marker)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()?;
        let lease = child.stdin.take();
        Ok(Self {
            child,
            lease,
            reaped: false,
        })
    }

    fn cleanup(&mut self) -> io::Result<ExitStatus> {
        if self.reaped {
            return Err(invalid("monitor was already reaped"));
        }
        let group = libc::pid_t::try_from(self.child.id())
            .map_err(|_| invalid("monitor PID does not fit pid_t"))?;
        // SAFETY: this direct child was spawned as group leader and is unreaped.
        // Retaining its zombie prevents both PID and process-group identity reuse.
        let killed = unsafe { libc::kill(-group, libc::SIGKILL) };
        let kill_error = (killed < 0).then(io::Error::last_os_error);
        self.lease.take();
        let status = self.child.wait()?;
        self.reaped = true;
        if let Some(error) = kill_error {
            // Darwin's killpg filters SZOMB members, then returns EPERM when
            // the retained group contains no signalable live member. Require
            // positive proof that our leader died by SIGKILL before accepting
            // that platform-specific dead-group result. Other errors fail.
            // https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/kern_sig.c
            #[cfg(target_os = "macos")]
            let dead_group =
                error.raw_os_error() == Some(libc::EPERM) && status.signal() == Some(libc::SIGKILL);
            #[cfg(not(target_os = "macos"))]
            let dead_group = false;
            if error.raw_os_error() != Some(libc::ESRCH) && !dead_group {
                return Err(io::Error::other(format!(
                    "signal owned monitor group: {error}; monitor exit: {status:?}"
                )));
            }
        }
        Ok(status)
    }
}

impl Drop for MonitorOwner {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.cleanup();
        }
    }
}

pub(super) fn run(spec: &ChildCommandSpec, context: &str, timeout: Duration) -> Output {
    run_marker(spec, context, timeout, VERSION)
}

fn run_marker(spec: &ChildCommandSpec, context: &str, timeout: Duration, marker: &str) -> Output {
    let started = Instant::now();
    let deadline = started + timeout;
    let request = Request::from_spec(spec, deadline.saturating_duration_since(Instant::now()))
        .and_then(|request| serde_json::to_vec(&request).map_err(io::Error::other))
        .unwrap_or_else(|error| panic!("configure {context} child: {error}"));
    assert!(
        request.len() <= MAX_REQUEST,
        "{context} child request exceeds framing limit"
    );
    let mut configuration = (request.len() as u32).to_le_bytes().to_vec();
    configuration.extend_from_slice(&request);
    let mut owner = MonitorOwner::spawn_marker(marker)
        .unwrap_or_else(|error| panic!("spawn {context} monitor: {error}"));
    let pid = owner.child.id();
    let mut diagnostic_pipe = owner.child.stdout.take().expect("monitor stdout");
    let mut result_pipe = owner.child.stderr.take().expect("monitor result pipe");
    for fd in [
        owner.lease.as_ref().expect("monitor lease").as_raw_fd(),
        diagnostic_pipe.as_raw_fd(),
        result_pipe.as_raw_fd(),
    ] {
        nonblocking(fd).unwrap_or_else(|error| panic!("configure {context} monitor pipe: {error}"));
    }
    let mut sent = 0;
    let mut frames = ResultFrames::default();
    let mut diagnostics = Vec::new();
    let mut timed_out = false;
    let result = loop {
        if Instant::now() >= deadline {
            timed_out = true;
            break Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "absolute child deadline expired",
            ));
        }
        if sent < configuration.len() {
            match owner
                .lease
                .as_mut()
                .expect("retained monitor lease")
                .write(&configuration[sent..])
            {
                Ok(0) => {
                    break Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "monitor configuration write",
                    ));
                }
                Ok(count) => sent += count,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(error) => break Err(error),
            }
        }
        if let Err(error) = read_available(&mut diagnostic_pipe, |bytes| {
            append(&mut diagnostics, bytes)
        }) {
            break Err(error);
        }
        match read_available(&mut result_pipe, |bytes| frames.receive(bytes, pid)) {
            Ok(true) => break Ok(()),
            Ok(false) => {}
            Err(error) => break Err(error),
        }
        std::thread::sleep(
            deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(1)),
        );
    };
    let monitor_status = owner
        .cleanup()
        .unwrap_or_else(|error| panic!("clean/reap {context} monitor: {error}"));
    // Fresh result pipes are not inherited by the real child. Collect available
    // frames after kill under the same bounded cleanup budget, retaining early
    // timeout stdout/stderr rather than waiting on an escaped child's pipes.
    let drain_deadline = Instant::now() + CLEANUP_BUDGET;
    let mut drain_result = Ok(());
    loop {
        match read_available(&mut result_pipe, |bytes| frames.receive(bytes, pid)) {
            Ok(true) => break,
            Ok(false) if Instant::now() < drain_deadline => {}
            Ok(false) => {
                drain_result = Err(invalid("monitor result drain exceeded cleanup budget"));
                break;
            }
            Err(error) => {
                drain_result = Err(error);
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let elapsed = started.elapsed();
    assert!(
        !timed_out,
        "{context} child (pid={pid}) timed out after {elapsed:?} (deadline {timeout:?})\nstdout:\n{}\nstderr:\n{}\noutput incomplete: monitor stopped at the absolute deadline\nmonitor ready: {}; configuration sent: {sent}/{}",
        String::from_utf8_lossy(&frames.stdout),
        String::from_utf8_lossy(&frames.stderr),
        frames.ready,
        configuration.len()
    );
    result.and(drain_result).unwrap_or_else(|error| panic!("{context} monitor protocol failed: {error}\nmonitor stdout:\n{}\nchild stdout:\n{}\nchild stderr:\n{}", String::from_utf8_lossy(&diagnostics), String::from_utf8_lossy(&frames.stdout), String::from_utf8_lossy(&frames.stderr)));
    assert_eq!(
        monitor_status.signal(),
        Some(libc::SIGKILL),
        "{context} monitor did not finish owned-group cleanup"
    );
    let output = frames
        .finish()
        .unwrap_or_else(|error| panic!("{context} monitor protocol failed: {error}"));
    eprintln!("{context} child finished after {elapsed:?}");
    output
}

struct KillOwnGroup(libc::pid_t);

impl Drop for KillOwnGroup {
    fn drop(&mut self) {
        // SAFETY: created only after verifying this process is its group leader.
        // It remains alive through this final signal, so the PGID is retained.
        if unsafe { libc::kill(-self.0, libc::SIGKILL) } < 0 {
            // Fail closed if the kill itself failed. No libtest panic catcher
            // can turn an armed monitor into a natural exit with descendants.
            unsafe { libc::_exit(125) };
        }
        // Darwin may return from kill before delivering our own pending SIGKILL.
        // A successful group signal must finish by that signal, not race _exit.
        loop {
            // SAFETY: pause only waits in this doomed, private monitor process.
            unsafe { libc::pause() };
        }
    }
}

fn monitor(mut protocol: impl Write, blocked_configuration_fixture: bool) -> io::Result<()> {
    // SAFETY: these identity queries do not mutate process state.
    let pid = unsafe { libc::getpid() };
    if pid <= 0 || unsafe { libc::getpgrp() } != pid {
        return Err(invalid("monitor is not its own process-group leader"));
    }
    let _cleanup = KillOwnGroup(pid);
    frame(&mut protocol, READY, &(pid as u32).to_le_bytes())?;
    protocol.flush()?;
    if blocked_configuration_fixture {
        // A deliberately non-reading receiver proves parent request writes are
        // bounded. Even this injection keeps the lease EOF backstop: poll HUP
        // without consuming buffered configuration, then kill the owned group.
        std::thread::Builder::new()
            .name("blocked-configuration-lease".into())
            .spawn(move || {
                let mut lease = libc::pollfd {
                    fd: libc::STDIN_FILENO,
                    events: 0,
                    revents: 0,
                };
                loop {
                    // SAFETY: stdin remains this monitor's owned lease read end.
                    let result = unsafe { libc::poll(&mut lease, 1, -1) };
                    if result < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted
                    {
                        continue;
                    }
                    break;
                }
                drop(KillOwnGroup(pid));
            })?;
        loop {
            std::thread::park();
        }
    }
    let mut lease = io::stdin();
    let result = (|| {
        let mut length = [0; 4];
        lease.read_exact(&mut length)?;
        let length = u32::from_le_bytes(length) as usize;
        if length > MAX_REQUEST {
            return Err(invalid("oversized child request"));
        }
        let mut bytes = vec![0; length];
        lease.read_exact(&mut bytes)?;
        let request: Request = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        let deadline = Instant::now() + Duration::from_nanos(request.remaining_nanos);
        // Only the read end exists in this process; the real child receives null
        // stdin. Unexpected data after the one request fails closed just like EOF.
        std::thread::Builder::new()
            .name("child-lifetime-lease".into())
            .spawn(move || {
                let mut byte = [0];
                loop {
                    match lease.read(&mut byte) {
                        Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                        _ => break,
                    }
                }
                drop(KillOwnGroup(pid));
            })?;
        let mut child = request
            .command()?
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        // No process_group call here: the real child must inherit our group.
        let mut stdout = child.stdout.take().expect("child stdout");
        let mut stderr = child.stderr.take().expect("child stderr");
        nonblocking(stdout.as_raw_fd())?;
        nonblocking(stderr.as_raw_fd())?;
        let mut stdout_done = false;
        let mut stderr_done = false;
        let status = loop {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "monitor child deadline expired",
                ));
            }
            if !stdout_done {
                stdout_done =
                    read_available(&mut stdout, |bytes| frame(&mut protocol, STDOUT, bytes))?;
            }
            if !stderr_done {
                stderr_done =
                    read_available(&mut stderr, |bytes| frame(&mut protocol, STDERR, bytes))?;
            }
            if let Some(status) = child.try_wait()? {
                break status;
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        // Root writes precede wait completion. Drain available bytes without
        // awaiting inherited writers; bounded batches preserve the deadline even
        // when a descendant continuously writes instead of going idle.
        for (kind, pipe, done) in [
            (STDOUT, &mut stdout as &mut dyn Read, stdout_done),
            (STDERR, &mut stderr as &mut dyn Read, stderr_done),
        ] {
            if done {
                continue;
            }
            let mut pipe = pipe;
            while Instant::now() < deadline {
                let mut available = false;
                let eof = read_available(&mut pipe, |bytes| {
                    available = true;
                    frame(&mut protocol, kind, bytes)
                })?;
                if eof || !available {
                    break;
                }
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "child output drain reached deadline",
                ));
            }
        }
        frame(&mut protocol, COMPLETE, &status.into_raw().to_le_bytes())?;
        protocol.flush()
    })();
    if let Err(error) = &result {
        let diagnostic = error.to_string();
        let _ = frame(
            &mut protocol,
            FAILURE,
            &diagnostic.as_bytes()[..diagnostic.len().min(CHUNK)],
        );
        let _ = protocol.flush();
    }
    result
}

#[test]
#[ignore = "private lease monitor; exact selection and command-local marker required"]
fn child_watchdog_monitor_fixture() {
    let expected = [
        "--ignored",
        "--exact",
        MONITOR_TEST,
        "--nocapture",
        "--test-threads=1",
    ];
    let marker = std::env::var(SELECTOR);
    if !matches!(marker.as_deref(), Ok(VERSION | BLOCKED_CONFIGURATION))
        || !std::env::args_os().skip(1).eq(expected.map(OsString::from))
    {
        return;
    }
    let _ = monitor(
        io::stderr().lock(),
        marker.as_deref() == Ok(BLOCKED_CONFIGURATION),
    );
}

#[test]
fn child_watchdog_codec_preserves_binary_frames_and_raw_wait_status() {
    // These literal packets are independent of the writer under test. They
    // carry monitor PID 42, binary stdout/stderr, and real child exit code 7.
    let bytes = [
        1, 4, 0, 0, 0, 42, 0, 0, 0, 2, 3, 0, 0, 0, 0, 255, 4, 3, 2, 0, 0, 0, 254, 0, 4, 4, 0, 0, 0,
        0, 7, 0, 0,
    ];
    for width in 1..=bytes.len() {
        let mut frames = ResultFrames::default();
        for part in bytes.chunks(width) {
            frames.receive(part, 42).expect("incremental framed result");
        }
        let output = frames.finish().expect("complete result");
        assert_eq!(output.status.into_raw(), 7 << 8);
        assert_eq!(output.stdout, [0, 255, 4]);
        assert_eq!(output.stderr, [254, 0]);
    }
    let mut writer = Vec::new();
    frame(&mut writer, STDERR, &[254, 0]).expect("binary frame");
    assert_eq!(writer, [3, 2, 0, 0, 0, 254, 0]);
    for (bytes, raw) in [
        ([1, 4, 0, 0, 0, 42, 0, 0, 0, 4, 4, 0, 0, 0, 6, 0, 0, 0], 6),
        (
            [1, 4, 0, 0, 0, 42, 0, 0, 0, 4, 4, 0, 0, 0, 134, 0, 0, 0],
            134,
        ),
    ] {
        let mut frames = ResultFrames::default();
        frames
            .receive(&bytes, 42)
            .expect("signal wait-status frame");
        assert_eq!(
            frames
                .finish()
                .expect("signal completion")
                .status
                .into_raw(),
            raw
        );
    }
}

#[test]
fn child_watchdog_codec_rejects_invalid_truncated_and_duplicate_results() {
    for bytes in [
        vec![],
        vec![1, 4, 0],
        vec![2, 1, 0, 0, 0, 7],
        vec![1, 4, 0, 0, 0, 43, 0, 0, 0],
        vec![9, 0, 0, 0, 0],
        vec![1, 0, 0, 1, 0],
        vec![1, 4, 0, 0, 0, 42, 0, 0, 0],
        vec![1, 4, 0, 0, 0, 42, 0, 0, 0, 4, 4, 0, 0, 0, 127, 0, 0, 0],
        vec![
            1, 4, 0, 0, 0, 42, 0, 0, 0, 4, 4, 0, 0, 0, 0, 0, 0, 0, 4, 4, 0, 0, 0, 0, 0, 0, 0,
        ],
        vec![
            1, 4, 0, 0, 0, 42, 0, 0, 0, 4, 4, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0, 0,
        ],
    ] {
        let mut frames = ResultFrames::default();
        assert!(
            frames
                .receive(&bytes, 42)
                .and_then(|()| frames.finish())
                .is_err(),
            "invalid packet accepted: {bytes:?}"
        );
    }
}

#[test]
fn child_watchdog_request_preserves_non_utf8_program_arguments_environment_and_cwd() {
    let mut spec = ChildCommandSpec::new(OsString::from_vec(b"program\xff".to_vec()));
    spec.arg(OsString::from_vec(b"arg\xfe".to_vec()))
        .current_dir(PathBuf::from(OsString::from_vec(b"cwd\xfd".to_vec())))
        .env("discard", "earlier")
        .env_clear()
        .env(
            OsString::from_vec(b"key\xfc".to_vec()),
            OsString::from_vec(b"value\xfb".to_vec()),
        )
        .env_remove(OsString::from_vec(b"removed\xfa".to_vec()));
    let request = Request::from_spec(&spec, Duration::from_secs(1)).expect("explicit request");
    let bytes = serde_json::to_vec(&request).expect("request encoding");
    let decoded: Request = serde_json::from_slice(&bytes).expect("request decoding");
    assert_eq!(decoded.program, b"program\xff");
    assert_eq!(decoded.args, [b"arg\xfe".to_vec()]);
    assert_eq!(decoded.current_dir.as_deref(), Some(b"cwd\xfd".as_slice()));
    assert!(!decoded.inherit_environment);
    assert_eq!(
        decoded.env,
        [
            (b"key\xfc".to_vec(), Some(b"value\xfb".to_vec())),
            (b"removed\xfa".to_vec(), None)
        ]
    );
    let command = decoded.command().expect("supported command");
    assert_eq!(command.get_program().as_bytes(), b"program\xff");
    assert_eq!(
        command.get_args().next().expect("argument").as_bytes(),
        b"arg\xfe"
    );
    assert_eq!(
        command
            .get_current_dir()
            .expect("cwd")
            .as_os_str()
            .as_bytes(),
        b"cwd\xfd"
    );
}

fn assert_monitor_reaped(pid: u32) {
    let pid = libc::pid_t::try_from(pid).expect("monitor PID");
    // SAFETY: signal zero observes only the exact monitor launched by this test.
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
    let mut status = 0;
    // SAFETY: only query this already-reaped direct child; WNOHANG cannot block.
    assert_eq!(
        unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
        -1
    );
    assert_eq!(
        io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}

fn failing_request(request: &[u8], truncate: bool) {
    let mut owner = MonitorOwner::spawn().expect("owned failure monitor");
    let pid = owner.child.id();
    let mut results = owner.child.stderr.take().expect("result transport");
    nonblocking(results.as_raw_fd()).expect("nonblocking result transport");
    owner
        .lease
        .as_mut()
        .expect("lease")
        .write_all(request)
        .expect("small fixture request");
    if truncate {
        owner.lease.take();
    }
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut frames = ResultFrames::default();
    let result: io::Result<Output> = loop {
        match read_available(&mut results, |bytes| frames.receive(bytes, pid)) {
            Ok(true) => break frames.finish(),
            Ok(false) => {}
            Err(error) => break Err(error),
        }
        assert!(
            Instant::now() < deadline,
            "failed request must terminate monitor"
        );
        std::thread::sleep(Duration::from_millis(1));
    };
    assert!(result.is_err());
    owner.cleanup().expect("failed request cleanup");
    assert_monitor_reaped(pid);
}

#[test]
fn child_watchdog_malformed_and_truncated_requests_cleanup_and_reap_monitor() {
    let _guard = super::child_process_guard();
    failing_request(&[1, 0, 0, 0, b'{'], false);
    failing_request(&[10, 0, 0, 0, b'{'], true);
    failing_request(&u32::MAX.to_le_bytes(), false);
}

#[test]
fn child_watchdog_spawn_failures_cleanup_and_reap_monitor() {
    let _guard = super::child_process_guard();
    let state = tempfile::tempdir().expect("spawn failure state");
    for spec in [
        ChildCommandSpec::new(state.path().join("missing-executable")),
        {
            let mut spec = ChildCommandSpec::new("sh");
            spec.current_dir(state.path().join("missing-cwd"));
            spec
        },
        {
            let mut spec = ChildCommandSpec::new("sh");
            spec.arg(OsString::from_vec(b"nul\0arg".to_vec()));
            spec
        },
        {
            let mut spec = ChildCommandSpec::new("sh");
            spec.env("bad=key", "value");
            spec
        },
    ] {
        let request =
            Request::from_spec(&spec, Duration::from_secs(3)).expect("supported explicit request");
        let bytes = serde_json::to_vec(&request).expect("request");
        let mut framed = (bytes.len() as u32).to_le_bytes().to_vec();
        framed.extend(bytes);
        failing_request(&framed, false);
        // Public helper reports the actual failure instead of fabricated output.
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(
                &spec,
                "startup-failure",
                Duration::from_secs(3)
            )))
            .is_err()
        );
    }
}

#[test]
fn child_watchdog_cancel_during_configuration_kills_and_reaps_monitor() {
    let _guard = super::child_process_guard();
    let mut owner = MonitorOwner::spawn().expect("owned cancellation monitor");
    let pid = owner.child.id();
    owner
        .lease
        .as_mut()
        .expect("lease")
        .write_all(&[10, 0, 0, 0, b'{'])
        .expect("partial setup");
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _owner = owner;
        panic!("cancel incomplete configuration");
    }));
    assert!(result.is_err());
    assert_monitor_reaped(pid);
}

#[test]
fn child_watchdog_listing_and_unmarked_fixture_selection_are_harmless() {
    let _guard = super::child_process_guard();
    for args in [
        vec!["--list"],
        vec![
            "--ignored",
            "--exact",
            MONITOR_TEST,
            "--nocapture",
            "--test-threads=1",
        ],
    ] {
        let output = Command::new(std::env::current_exe().expect("test executable"))
            .args(&args)
            .env_remove(SELECTOR)
            .stdin(Stdio::null())
            .output()
            .expect("harmless enumeration");
        assert!(output.status.success());
        assert!(
            output.stderr.is_empty(),
            "must not enter framed monitor mode"
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains(MONITOR_TEST));
    }
    let output = Command::new(std::env::current_exe().expect("test executable"))
        .args(["--list"])
        .env(SELECTOR, VERSION)
        .stdin(Stdio::null())
        .output()
        .expect("marker alone enumeration");
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
}

#[test]
fn child_watchdog_blocked_configuration_write_obeys_absolute_deadline_and_reaps_monitor() {
    let _guard = super::child_process_guard();
    let mut peer = super::watchdog_tests::OwnedPeer::start();
    peer.assert_alive();
    let mut spec = ChildCommandSpec::new("sh");
    spec.arg(OsString::from_vec(vec![b'x'; 200_000]));
    let started = Instant::now();
    let panic = std::panic::catch_unwind(|| {
        run_marker(
            &spec,
            "blocked-configuration",
            Duration::from_millis(100),
            BLOCKED_CONFIGURATION,
        )
    })
    .expect_err("blocked request must time out");
    let report = panic.downcast_ref::<String>().expect("timeout diagnostic");
    assert!(report.contains("timed out after"), "{report}");
    assert!(report.contains("monitor ready: true"), "{report}");
    let sent = report
        .split_once("configuration sent: ")
        .expect("configuration progress")
        .1;
    let (sent, total) = sent.split_once('/').expect("sent/total");
    let sent: usize = sent.parse().expect("sent bytes");
    let total: usize = total.parse().expect("configuration bytes");
    assert!(
        sent > 0 && sent < total,
        "configuration must be partially transmitted"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    let pid: u32 = report
        .split_once("(pid=")
        .expect("monitor PID")
        .1
        .split_once(')')
        .expect("PID end")
        .0
        .parse()
        .expect("numeric monitor PID");
    assert_monitor_reaped(pid);
    peer.assert_alive();
}
