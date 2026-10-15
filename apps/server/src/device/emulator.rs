//! Emulator process lifecycle: spawn, gRPC discovery, boot wait, and stop.

use std::{
    ffi::OsString,
    net::{Ipv4Addr, TcpListener},
    path::{Path, PathBuf},
    process::ExitStatus,
    time::Duration,
};

use process_wrap::tokio::CommandWrap;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{
    catalog::DeviceState,
    grpc::{EmulatorClient, proto::image_format::ImgFormat},
    sdk::{AndroidSdk, SdkEnv},
    stderr_tail::{STDERR_DRAIN_GRACE, StderrTail},
};
use crate::{
    diagnostics::{ProcessAttributionRegistry, profiler::exits},
    process::{
        HelperSpawnError, RegisteredHelper, configure_supervised_background_command_wrap,
        spawn_registered_helper,
        supervised::{SupervisedOverflow, SupervisedRunRequest, run_supervised},
    },
};

pub(crate) const BOOT_TIMEOUT: Duration = Duration::from_secs(180);
pub(crate) const STOP_TIMEOUT: Duration = Duration::from_secs(10);
/// Flags that make the emulator require, and publish, a gRPC bearer token (native
/// verification item 1).
pub(crate) const EMULATOR_GRPC_FLAGS: &[&str] = &["-grpc-use-token"];

pub(crate) const HELPER_LABEL: &str = "android-emulator";
const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Console ports the emulator accepts; each instance also takes the next (odd) port for adb.
const CONSOLE_PORTS: std::ops::RangeInclusive<u16> = 5554..=5682;
const ADB_TIMEOUT: Duration = Duration::from_secs(10);
const ADB_CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);
const ADB_MAX_OUTPUT_BYTES: usize = 4 * 1024;
const SIZE_ATTEMPTS: u32 = 10;
const SIZE_RETRY_DELAY: Duration = Duration::from_millis(500);

pub(crate) struct EmulatorPorts {
    pub console: u16,
    pub grpc: u16,
}

/// The emulator's arguments; the AVD name stays one argument and never meets a shell.
pub(crate) fn spawn_args(avd: &str, ports: &EmulatorPorts, software_gpu: bool) -> Vec<OsString> {
    let mut args: Vec<OsString> = vec!["-avd".into(), avd.into()];
    for arg in ["-no-window", "-no-audio", "-no-boot-anim"] {
        args.push(arg.into());
    }
    args.extend([
        "-port".into(),
        ports.console.to_string().into(),
        "-grpc".into(),
        ports.grpc.to_string().into(),
    ]);
    args.extend(EMULATOR_GRPC_FLAGS.iter().map(OsString::from));
    if software_gpu {
        args.extend(["-gpu".into(), "swiftshader_indirect".into()]);
    }
    args
}

/// `-gpu swiftshader_indirect` only on a Linux server without a display; elsewhere the
/// emulator's `auto` GPU mode works headless.
pub(crate) fn software_gpu_for(os: &str, var: impl Fn(&str) -> Option<OsString>) -> bool {
    os == "linux" && var("DISPLAY").is_none() && var("WAYLAND_DISPLAY").is_none()
}

/// The file a running emulator publishes its gRPC endpoint in (native verification item 1).
pub(crate) fn discovery_file_path(discovery_dir: &Path, pid: u32) -> PathBuf {
    discovery_dir.join(format!("pid_{pid}.ini"))
}

/// Where emulators publish discovery files, most specific first: the OS runtime directory
/// current emulators use, then the legacy `<emulator home>/avd/running` (native verification
/// item 1).
pub(crate) fn default_discovery_dirs(env: &SdkEnv) -> Vec<PathBuf> {
    let var = |key: &str| {
        env.vars
            .get(key)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    };
    let mut dirs = Vec::new();
    match env.os {
        "linux" => dirs.extend(var("XDG_RUNTIME_DIR").map(|dir| dir.join("avd/running"))),
        "macos" => dirs.extend(
            env.home
                .as_ref()
                .map(|home| home.join("Library/Caches/TemporaryItems/avd/running")),
        ),
        "windows" => {
            dirs.extend(var("LOCALAPPDATA").map(|dir| dir.join("Temp").join("avd").join("running")))
        }
        _ => {}
    }
    let emulator_home = var("ANDROID_EMULATOR_HOME")
        .or_else(|| env.home.as_ref().map(|home| home.join(".android")));
    dirs.extend(emulator_home.map(|home| home.join("avd").join("running")));
    dirs
}

/// The discovery file of the emulator serving gRPC on `grpc_port`, with its token. In each
/// directory in order, `pid_<pid>.ini` is tried before any other `pid_*.ini` (qemu may run as
/// a child of the launched process); only a file naming `grpc_port` is accepted, which also
/// skips files a dead emulator left behind under a reused PID.
pub(crate) fn find_discovery(
    discovery_dirs: &[PathBuf],
    pid: u32,
    grpc_port: u16,
) -> Option<(PathBuf, Option<String>)> {
    let matching = |path: &Path| {
        let (port, token) = parse_discovery(&std::fs::read_to_string(path).ok()?)?;
        (port == grpc_port).then_some(token)
    };
    for dir in discovery_dirs {
        let own = discovery_file_path(dir, pid);
        if let Some(token) = matching(&own) {
            return Some((own, token));
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for path in entries.filter_map(|entry| Some(entry.ok()?.path())) {
            let is_discovery = path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("pid_") && name.ends_with(".ini"));
            if is_discovery
                && path != own
                && let Some(token) = matching(&path)
            {
                return Some((path, token));
            }
        }
    }
    None
}

/// `grpc.port` and `grpc.token` from a discovery file; `None` without a port.
pub(crate) fn parse_discovery(contents: &str) -> Option<(u16, Option<String>)> {
    let mut port = None;
    let mut token = None;
    for (key, value) in contents.lines().filter_map(|line| line.split_once('=')) {
        match key.trim() {
            "grpc.port" => port = value.trim().parse().ok(),
            "grpc.token" => token = Some(value.trim().to_owned()).filter(|token| !token.is_empty()),
            _ => {}
        }
    }
    port.map(|port| (port, token))
}

pub(crate) struct RunningEmulator {
    pub helper: RegisteredHelper,
    /// The adb serial, `emulator-<console port>`.
    pub serial: String,
    pub client: EmulatorClient,
    pub width: u32,
    pub height: u32,
    /// The discovery file that was found; removed on stop, since a killed emulator leaves
    /// it behind.
    pub discovery_file: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum LaunchError {
    #[error("The emulator exited while starting: {0}")]
    Exited(String),
    #[error("The emulator did not finish booting within the time allowed.")]
    BootTimeout,
    #[error("Could not start the emulator: {0}")]
    Spawn(String),
    /// The emulator booted but its gRPC endpoint never reported a display.
    #[error("The emulator booted but did not report its display: {0}")]
    Unresponsive(String),
    #[error("The emulator start was cancelled.")]
    Cancelled,
}

pub(crate) struct EmulatorLauncher {
    pub sdk: AndroidSdk,
    /// Directories searched, in order, for the emulator's discovery file
    /// (`default_discovery_dirs`). The emulator creates them; the launcher only reads.
    pub discovery_dirs: Vec<PathBuf>,
    pub registry: ProcessAttributionRegistry,
    pub software_gpu: bool,
    pub boot_timeout: Duration,
    /// A fixed gRPC port; `None` picks a free one. Tests pin the fake controller's port.
    pub grpc_port: Option<u16>,
}

/// Why waiting for a boot ended without a running emulator.
enum BootFailure {
    Exited(ExitStatus),
    Unresponsive(String),
}

impl EmulatorLauncher {
    pub(crate) fn new(
        sdk: AndroidSdk,
        discovery_dirs: Vec<PathBuf>,
        registry: ProcessAttributionRegistry,
        software_gpu: bool,
    ) -> Self {
        Self {
            sdk,
            discovery_dirs,
            registry,
            software_gpu,
            boot_timeout: BOOT_TIMEOUT,
            grpc_port: None,
        }
    }

    /// Starts `avd` and waits until Android has booted. `on_phase` hears `Booting` once the
    /// emulator's gRPC endpoint answers. On every failure the emulator is stopped.
    pub(crate) async fn launch(
        &self,
        avd: &str,
        on_phase: impl Fn(DeviceState) + Send,
        cancel: &CancellationToken,
    ) -> Result<RunningEmulator, LaunchError> {
        let ports = pick_ports(self.grpc_port).map_err(LaunchError::Spawn)?;
        let mut command = CommandWrap::with_new(&self.sdk.emulator, |command| {
            command
                .args(spawn_args(avd, &ports, self.software_gpu))
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped());
        });
        configure_supervised_background_command_wrap(&mut command);
        let exe = self.sdk.emulator.to_string_lossy().into_owned();
        let mut helper =
            match spawn_registered_helper(command, exe, HELPER_LABEL, &self.registry).await {
                Ok(helper) => helper,
                Err(error) => {
                    tracing::warn!(%error, "android emulator failed to start");
                    return Err(match error {
                        HelperSpawnError::Failed(message) => LaunchError::Spawn(message),
                        HelperSpawnError::ExitedEarly { status, stderr, .. } => {
                            let tail = StderrTail::default();
                            if let Some(pipe) = stderr {
                                let _ = tokio::time::timeout(STDERR_DRAIN_GRACE, tail.drain(pipe))
                                    .await;
                            }
                            LaunchError::Exited(tail.text().unwrap_or_else(|| {
                                status.map_or_else(
                                    || "exited immediately".to_owned(),
                                    |status| status.to_string(),
                                )
                            }))
                        }
                    });
                }
            };
        let stderr = StderrTail::default();
        let drain = helper.child.stderr().take().map(|pipe| stderr.drain(pipe));
        let serial = format!("emulator-{}", ports.console);
        let mut discovery_file = None;

        let boot = self.wait_for_boot(
            &helper,
            &serial,
            ports.grpc,
            &mut discovery_file,
            on_phase,
            cancel,
        );
        let outcome = tokio::select! {
            biased;
            () = cancel.cancelled() => Err(None),
            result = tokio::time::timeout(self.boot_timeout, boot) => match result {
                Ok(Ok(booted)) => Ok(booted),
                Ok(Err(failure)) => Err(Some(failure)),
                Err(_) => Err(None),
            },
        };
        let error = match outcome {
            Ok((client, width, height)) => {
                return Ok(RunningEmulator {
                    helper,
                    serial,
                    client,
                    width,
                    height,
                    discovery_file: discovery_file.expect("a booted emulator was discovered"),
                });
            }
            Err(Some(BootFailure::Exited(status))) => {
                helper.report_unrequested_exit(HELPER_LABEL, &status);
                if let Some(drain) = drain {
                    let _ = tokio::time::timeout(STDERR_DRAIN_GRACE, drain).await;
                }
                LaunchError::Exited(stderr.text().unwrap_or_else(|| status.to_string()))
            }
            Err(Some(BootFailure::Unresponsive(message))) => LaunchError::Unresponsive(message),
            Err(None) if cancel.is_cancelled() => LaunchError::Cancelled,
            Err(None) => LaunchError::BootTimeout,
        };
        if !matches!(error, LaunchError::Exited(_)) {
            exits::note_stop_requested(helper.pid);
        }
        tracing::warn!(%error, "android emulator launch failed");
        helper.terminate().await;
        if let Some(discovery_file) = &discovery_file {
            remove_discovery_file(discovery_file);
        }
        Err(error)
    }

    /// Polls until the emulator answers gRPC, Android reports `sys.boot_completed`, and the
    /// display size is known; ends early when the emulator exits.
    async fn wait_for_boot(
        &self,
        helper: &RegisteredHelper,
        serial: &str,
        grpc_port: u16,
        discovery_file: &mut Option<PathBuf>,
        on_phase: impl Fn(DeviceState) + Send,
        cancel: &CancellationToken,
    ) -> Result<(EmulatorClient, u32, u32), BootFailure> {
        let mut client: Option<EmulatorClient> = None;
        let mut booting = false;
        loop {
            if let Some(status) = helper.exited() {
                return Err(BootFailure::Exited(status));
            }
            if client.is_none()
                && let Some((path, token)) = self.discover(helper.pid, grpc_port).await
            {
                *discovery_file = Some(path);
                client = EmulatorClient::connect(grpc_port, token)
                    .await
                    .inspect_err(|error| tracing::debug!(%error, "emulator gRPC not ready"))
                    .ok();
            }
            if let Some(connected) = &client {
                if !booting {
                    match connected.status().await {
                        Ok(_) => {
                            booting = true;
                            on_phase(DeviceState::Booting);
                        }
                        Err(error) => {
                            // Re-read the discovery file: it may have been incomplete.
                            tracing::debug!(%error, "emulator gRPC status failed");
                            client = None;
                        }
                    }
                }
                if booting && self.boot_completed(serial, cancel).await {
                    let connected = client.take().expect("a booted emulator has a client");
                    let (width, height) = display_size(helper, &connected).await?;
                    return Ok((connected, width, height));
                }
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    async fn discover(&self, pid: u32, grpc_port: u16) -> Option<(PathBuf, Option<String>)> {
        let dirs = self.discovery_dirs.clone();
        tokio::task::spawn_blocking(move || find_discovery(&dirs, pid, grpc_port))
            .await
            .ok()
            .flatten()
    }

    async fn boot_completed(&self, serial: &str, cancel: &CancellationToken) -> bool {
        let mut command = tokio::process::Command::new(&self.sdk.adb);
        command
            .args(["-s", serial, "shell", "getprop", "sys.boot_completed"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let request = SupervisedRunRequest {
            command,
            stdin: None,
            timeout: ADB_TIMEOUT,
            cleanup_timeout: ADB_CLEANUP_TIMEOUT,
            max_output_bytes: ADB_MAX_OUTPUT_BYTES,
            overflow: SupervisedOverflow::Truncate,
        };
        match run_supervised(request, cancel).await {
            Ok(output) => String::from_utf8_lossy(&output.stdout.bytes).trim() == "1",
            Err(error) => {
                tracing::debug!(?error, serial, "adb boot check failed");
                false
            }
        }
    }
}

/// Stops an emulator: asks the VM to shut down, waits up to [`STOP_TIMEOUT`] for it to
/// exit, then kills its process group.
pub(crate) async fn stop(running: RunningEmulator) {
    let RunningEmulator {
        helper,
        client,
        discovery_file,
        ..
    } = running;
    exits::note_stop_requested(helper.pid);
    let deadline = Instant::now() + STOP_TIMEOUT;
    match tokio::time::timeout_at(deadline, client.shutdown_vm()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => tracing::debug!(%error, "emulator VM shutdown request failed"),
        Err(_) => tracing::debug!("emulator VM shutdown request timed out"),
    }
    while helper.exited().is_none() && Instant::now() < deadline {
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    helper.terminate().await;
    remove_discovery_file(&discovery_file);
}

/// The first even console port whose adb neighbour is also free on loopback, and `grpc`
/// or else an OS-assigned gRPC port.
fn pick_ports(grpc: Option<u16>) -> Result<EmulatorPorts, String> {
    let free = |port: u16| TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok();
    let console = CONSOLE_PORTS
        .step_by(2)
        .find(|port| free(*port) && free(port + 1))
        .ok_or_else(|| "every emulator console port is in use".to_owned())?;
    let grpc = match grpc {
        Some(port) => port,
        None => TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .and_then(|listener| listener.local_addr())
            .map_err(|error| format!("no free gRPC port: {error}"))?
            .port(),
    };
    Ok(EmulatorPorts { console, grpc })
}

/// The native display size. PNG avoids the `FAILED_PRECONDITION` other formats return
/// before the guest posts its first frame; any error is still retried while the emulator
/// runs.
async fn display_size(
    helper: &RegisteredHelper,
    client: &EmulatorClient,
) -> Result<(u32, u32), BootFailure> {
    let mut last_error = String::new();
    for attempt in 0..SIZE_ATTEMPTS {
        if attempt > 0 {
            tokio::time::sleep(SIZE_RETRY_DELAY).await;
            if let Some(status) = helper.exited() {
                return Err(BootFailure::Exited(status));
            }
        }
        match client.screenshot(ImgFormat::Png, 0).await {
            Ok(image) => match image.format {
                Some(format) if format.width > 0 && format.height > 0 => {
                    return Ok((format.width, format.height));
                }
                _ => last_error = "screenshot without a size".to_owned(),
            },
            Err(error) => last_error = error.to_string(),
        }
    }
    Err(BootFailure::Unresponsive(last_error))
}

fn remove_discovery_file(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "could not remove emulator discovery file");
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::{
        sync::{Arc, Mutex},
        time::Instant,
    };

    use super::*;
    use crate::device::{
        grpc::proto::vm_run_state::RunState,
        testing::{FakeController, FakeSdk, LaunchStub, process_alive, wait_until},
    };

    #[test]
    fn spawn_args_keep_avd_name_as_one_argument() {
        let args = spawn_args(
            "Tablet API 34; rm -rf /",
            &EmulatorPorts {
                console: 5554,
                grpc: 8554,
            },
            true,
        );
        let avd_index = args.iter().position(|arg| arg == "-avd").unwrap();
        assert_eq!(
            args[avd_index + 1],
            OsString::from("Tablet API 34; rm -rf /")
        );
        for expected in [
            "-no-window",
            "-no-audio",
            "-no-boot-anim",
            "-gpu",
            "swiftshader_indirect",
            "-port",
            "5554",
            "-grpc",
            "8554",
        ] {
            assert!(args.iter().any(|arg| arg == expected), "missing {expected}");
        }
    }

    #[test]
    fn parses_discovery_file() {
        assert_eq!(
            parse_discovery("grpc.port=8554\ngrpc.token=abc\nother=1\n"),
            Some((8554, Some("abc".into())))
        );
        assert_eq!(parse_discovery("grpc.port=8554\n"), Some((8554, None)));
        assert_eq!(parse_discovery("port.serial=5554\n"), None);
    }

    #[tokio::test]
    async fn launches_boots_and_stops_a_fake_emulator() {
        let controller = FakeController::spawn().await;
        let sdk = FakeSdk::new()
            .avds(&["Pixel"])
            .launch_stub(LaunchStub::Runs {
                grpc_port: controller.port,
                token: "test-token",
            })
            .boot_completed_after(2)
            .write();
        let launcher = sdk.launcher();
        let phases = Arc::new(Mutex::new(Vec::new()));
        let seen = phases.clone();
        let running = launcher
            .launch(
                "Pixel",
                move |phase| seen.lock().unwrap().push(phase),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!((running.width, running.height), (1080, 2400));
        assert_eq!(*phases.lock().unwrap(), [DeviceState::Booting]);
        let pid = running.helper.pid;
        let discovery_file = running.discovery_file.clone();
        assert!(discovery_file.is_file());
        // The VM shutdown ends the stub, so stop never needs its kill.
        controller.exit_on_shutdown(pid);
        let started = Instant::now();
        stop(running).await;
        assert!(started.elapsed() < STOP_TIMEOUT);
        assert_eq!(
            *controller.vm_state.lock().unwrap(),
            Some(RunState::Shutdown as i32)
        );
        assert!(!process_alive(pid));
        assert!(!discovery_file.exists());
    }

    #[tokio::test]
    async fn boot_reports_early_exit_without_waiting_for_timeout() {
        let sdk = FakeSdk::new()
            .avds(&["Pixel"])
            .launch_stub(LaunchStub::ExitsWith(1, "PANIC: Missing emulator engine"))
            .write();
        let started = Instant::now();
        let error = sdk
            .launcher()
            .launch("Pixel", |_| {}, &CancellationToken::new())
            .await
            .err()
            .expect("the launch fails");
        assert!(
            matches!(&error, LaunchError::Exited(message) if message.contains("Missing emulator engine")),
            "{error:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn boot_times_out_and_kills_the_process() {
        let controller = FakeController::spawn().await;
        let sdk = FakeSdk::new()
            .avds(&["Pixel"])
            .launch_stub(LaunchStub::Runs {
                grpc_port: controller.port,
                token: "test-token",
            })
            .never_boots()
            .write();
        let mut launcher = sdk.launcher();
        launcher.boot_timeout = Duration::from_secs(2);
        let error = launcher
            .launch("Pixel", |_| {}, &CancellationToken::new())
            .await
            .err()
            .expect("the launch fails");
        assert!(matches!(error, LaunchError::BootTimeout), "{error:?}");
        let launched = sdk.launched_pids();
        assert_eq!(launched.len(), 1);
        assert!(launched.iter().all(|pid| !process_alive(*pid)));
    }

    #[tokio::test]
    async fn cancelled_launch_kills_the_process() {
        let controller = FakeController::spawn().await;
        let sdk = FakeSdk::new()
            .avds(&["Pixel"])
            .launch_stub(LaunchStub::Runs {
                grpc_port: controller.port,
                token: "test-token",
            })
            .never_boots()
            .write();
        let cancel = CancellationToken::new();
        let trigger = cancel.clone();
        let error = sdk
            .launcher()
            .launch(
                "Pixel",
                move |phase| {
                    if phase == DeviceState::Booting {
                        trigger.cancel();
                    }
                },
                &cancel,
            )
            .await
            .err()
            .expect("the launch fails");
        assert!(matches!(error, LaunchError::Cancelled), "{error:?}");
        assert!(sdk.launched_pids().iter().all(|pid| !process_alive(*pid)));
    }

    #[tokio::test]
    async fn stop_escalates_to_kill_when_vm_ignores_shutdown() {
        let controller = FakeController::spawn().await;
        let sdk = FakeSdk::new()
            .avds(&["Pixel"])
            .launch_stub(LaunchStub::IgnoresShutdown {
                grpc_port: controller.port,
                token: "test-token",
            })
            .write();
        let running = sdk
            .launcher()
            .launch("Pixel", |_| {}, &CancellationToken::new())
            .await
            .unwrap();
        let pid = running.helper.pid;
        controller.exit_on_shutdown(pid);
        tokio::time::timeout(STOP_TIMEOUT + Duration::from_secs(5), stop(running))
            .await
            .unwrap();
        assert!(!process_alive(pid));
    }

    #[tokio::test]
    async fn stop_kills_grandchildren_of_the_emulator_launcher() {
        // The real `emulator` launcher may run qemu as a child rather than exec it.
        let controller = FakeController::spawn().await;
        let sdk = FakeSdk::new()
            .avds(&["Pixel"])
            .launch_stub(LaunchStub::ForksChild {
                grpc_port: controller.port,
                token: "test-token",
            })
            .write();
        let running = sdk
            .launcher()
            .launch("Pixel", |_| {}, &CancellationToken::new())
            .await
            .unwrap();
        let child_pid = sdk.wait_forked_child_pid().await;
        // Published under the child's PID, not the launched process's.
        assert_eq!(
            running.discovery_file,
            discovery_file_path(&sdk.discovery_dirs()[0], child_pid)
        );
        // The launcher exits on the VM shutdown and leaves its child behind.
        controller.exit_on_shutdown(running.helper.pid);
        stop(running).await;
        wait_until(|| !process_alive(child_pid)).await;
    }

    #[tokio::test]
    async fn launch_and_stop_futures_are_send() {
        fn require_send<T: Send>(_: &T) {}
        let controller = FakeController::spawn().await;
        let sdk = FakeSdk::new()
            .avds(&["Pixel"])
            .launch_stub(LaunchStub::Runs {
                grpc_port: controller.port,
                token: "test-token",
            })
            .write();
        let launcher = sdk.launcher();
        let cancel = CancellationToken::new();
        let launch = launcher.launch("Pixel", |_| {}, &cancel);
        require_send(&launch);
        let running = launch.await.unwrap();
        controller.exit_on_shutdown(running.helper.pid);
        let stopping = stop(running);
        require_send(&stopping);
        stopping.await;
    }

    fn env(os: &'static str, vars: &[(&str, &str)]) -> SdkEnv {
        SdkEnv {
            vars: vars
                .iter()
                .map(|(key, value)| ((*key).to_owned(), OsString::from(value)))
                .collect(),
            home: Some(PathBuf::from("/home/me")),
            os,
        }
    }

    #[test]
    fn default_discovery_dirs_put_the_runtime_directory_first() {
        assert_eq!(
            default_discovery_dirs(&env("linux", &[("XDG_RUNTIME_DIR", "/run/user/1000")])),
            [
                PathBuf::from("/run/user/1000/avd/running"),
                PathBuf::from("/home/me/.android/avd/running"),
            ]
        );
        assert_eq!(
            default_discovery_dirs(&env(
                "linux",
                &[("XDG_RUNTIME_DIR", ""), ("ANDROID_EMULATOR_HOME", "/emu")]
            )),
            [PathBuf::from("/emu/avd/running")]
        );
        assert_eq!(
            default_discovery_dirs(&env("macos", &[])),
            [
                PathBuf::from("/home/me/Library/Caches/TemporaryItems/avd/running"),
                PathBuf::from("/home/me/.android/avd/running"),
            ]
        );
        assert_eq!(
            default_discovery_dirs(&env(
                "windows",
                &[("LOCALAPPDATA", "/appdata"), ("ANDROID_EMULATOR_HOME", "")]
            )),
            [
                PathBuf::from("/appdata")
                    .join("Temp")
                    .join("avd")
                    .join("running"),
                PathBuf::from("/home/me/.android")
                    .join("avd")
                    .join("running"),
            ]
        );
    }

    fn write_discovery(dir: &Path, name: &str, port: u16) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join(name),
            format!("grpc.port={port}\ngrpc.token=t-{port}\n"),
        )
        .unwrap();
    }

    #[test]
    fn discovery_accepts_another_pids_file_naming_our_port() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("running");
        write_discovery(&dir, "pid_999.ini", 8554);
        assert_eq!(
            find_discovery(std::slice::from_ref(&dir), 42, 8554),
            Some((dir.join("pid_999.ini"), Some("t-8554".into())))
        );
    }

    #[test]
    fn discovery_ignores_files_naming_another_port() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("running");
        // Our own PID, but a stale file from a previous emulator on another port.
        write_discovery(&dir, "pid_42.ini", 8600);
        write_discovery(&dir, "pid_7.ini", 8601);
        assert_eq!(find_discovery(std::slice::from_ref(&dir), 42, 8554), None);
        write_discovery(&dir, "pid_8.ini", 8554);
        assert_eq!(
            find_discovery(std::slice::from_ref(&dir), 42, 8554).map(|found| found.0),
            Some(dir.join("pid_8.ini"))
        );
    }

    #[test]
    fn discovery_searches_later_directories() {
        let root = tempfile::tempdir().unwrap();
        let dirs = [root.path().join("missing"), root.path().join("second")];
        write_discovery(&dirs[1], "pid_42.ini", 8554);
        assert_eq!(
            find_discovery(&dirs, 42, 8554).map(|found| found.0),
            Some(dirs[1].join("pid_42.ini"))
        );
    }

    #[tokio::test]
    async fn launch_finds_the_discovery_file_in_a_later_directory() {
        let controller = FakeController::spawn().await;
        let sdk = FakeSdk::new()
            .avds(&["Pixel"])
            .launch_stub(LaunchStub::Runs {
                grpc_port: controller.port,
                token: "test-token",
            })
            .discovery_in(1)
            .write();
        let running = sdk
            .launcher()
            .launch("Pixel", |_| {}, &CancellationToken::new())
            .await
            .unwrap();
        assert!(running.discovery_file.starts_with(&sdk.discovery_dirs()[1]));
        let pid = running.helper.pid;
        controller.exit_on_shutdown(pid);
        stop(running).await;
        assert!(!process_alive(pid));
    }

    #[test]
    fn software_gpu_only_on_headless_linux() {
        assert!(software_gpu_for("linux", |_| None));
        assert!(!software_gpu_for("linux", |key| (key == "DISPLAY").then(|| ":0".into())));
        assert!(!software_gpu_for("linux", |key| (key == "WAYLAND_DISPLAY")
            .then(|| "wayland-0".into())));
        assert!(!software_gpu_for("macos", |_| None));
    }
}
