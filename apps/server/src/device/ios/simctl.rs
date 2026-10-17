//! `xcrun simctl`: listing, booting, shutting down, and screenshotting iOS simulators. Every
//! call is a supervised child with a timeout, and a failure carries its stderr tail.

use std::{
    path::PathBuf,
    process::ExitStatus,
    time::{Duration, Instant},
};

use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::{
    device::stderr_tail,
    diagnostics::{
        AttributionKind, AttributionScope, NativeProcessSampler, ProcessAttributionRegistry,
        ProcessRegistration, ProcessRegistrationMetadata, RegistrationSource,
    },
    process::supervised::{
        SupervisedOverflow, SupervisedRunError, SupervisedRunOutput, SupervisedRunRequest,
        run_supervised,
    },
};

const LIST_TIMEOUT: Duration = Duration::from_secs(15);
const SCREENSHOT_TIMEOUT: Duration = Duration::from_secs(15);
const PBCOPY_TIMEOUT: Duration = Duration::from_secs(15);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);
/// A listing of every simulator of every runtime; a Mac with many is still far below this.
const LIST_MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
/// One full-resolution JPEG screenshot.
const SCREENSHOT_MAX_OUTPUT_BYTES: usize = 32 * 1024 * 1024;
/// Commands whose stdout nobody reads; their stderr explains a failure.
const SMALL_OUTPUT_BYTES: usize = 64 * 1024;
const RUNTIME_PREFIX: &str = "com.apple.CoreSimulator.SimRuntime.";
/// The identity the profiler's sampler attributes a booted simulator's `launchd_sim` to.
pub(crate) const SIMULATOR_LABEL: &str = "ios-simulator";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum SimctlError {
    #[error("could not run {command}: {message}")]
    Spawn { command: String, message: String },
    #[error("{command} timed out")]
    Timeout { command: String },
    #[error("{command} was cancelled")]
    Cancelled { command: String },
    #[error("{command} failed: {}", .stderr.as_deref().unwrap_or(.status.as_str()))]
    Failed {
        command: String,
        status: String,
        /// The last lines `simctl` wrote to stderr.
        stderr: Option<String>,
    },
    #[error("{0}")]
    Parse(String),
}

impl SimctlError {
    /// What `simctl` said, or the error itself when it said nothing.
    pub(crate) fn reason(&self) -> String {
        match self {
            Self::Failed {
                stderr: Some(stderr),
                ..
            } => stderr.clone(),
            other => other.to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SimDevice {
    pub udid: String,
    pub name: String,
    /// "iOS 27.0".
    pub runtime: String,
    pub state: SimState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SimState {
    Shutdown,
    Booted,
    Booting,
    ShuttingDown,
    Other,
}

impl SimState {
    fn parse(state: &str) -> Self {
        match state {
            "Shutdown" => Self::Shutdown,
            "Booted" => Self::Booted,
            "Booting" => Self::Booting,
            "Shutting Down" => Self::ShuttingDown,
            _ => Self::Other,
        }
    }
}

#[derive(Deserialize)]
struct ListOutput {
    devices: std::collections::BTreeMap<String, Vec<ListedDevice>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListedDevice {
    udid: String,
    name: String,
    state: String,
    #[serde(default = "available_by_default")]
    is_available: bool,
}

fn available_by_default() -> bool {
    true
}

/// "com.apple.CoreSimulator.SimRuntime.iOS-27-0" → "iOS 27.0"; `None` for other platforms.
fn ios_runtime(identifier: &str) -> Option<String> {
    let (platform, version) = identifier.strip_prefix(RUNTIME_PREFIX)?.split_once('-')?;
    (platform == "iOS").then(|| format!("iOS {}", version.replace('-', ".")))
}

/// The version numbers of "iOS 27.0", for ordering newest first.
fn runtime_version(runtime: &str) -> Vec<u32> {
    runtime
        .trim_start_matches("iOS ")
        .split('.')
        .filter_map(|part| part.parse().ok())
        .collect()
}

/// The available simulators of iOS runtimes in `simctl list devices available -j`, newest
/// runtime first, in `simctl`'s order within one.
pub(crate) fn parse_list(json: &[u8]) -> Result<Vec<SimDevice>, SimctlError> {
    let output: ListOutput = serde_json::from_slice(json)
        .map_err(|error| SimctlError::Parse(format!("unexpected simctl list output: {error}")))?;
    let mut devices: Vec<SimDevice> = output
        .devices
        .into_iter()
        .filter_map(|(runtime, devices)| Some((ios_runtime(&runtime)?, devices)))
        .flat_map(|(runtime, devices)| {
            devices
                .into_iter()
                .filter(|device| device.is_available)
                .map(move |device| SimDevice {
                    udid: device.udid,
                    name: device.name,
                    runtime: runtime.clone(),
                    state: SimState::parse(&device.state),
                })
        })
        .collect();
    devices.sort_by_key(|device| std::cmp::Reverse(runtime_version(&device.runtime)));
    Ok(devices)
}

/// "iPhone 17 Pro · iOS 27.0".
pub(crate) fn display_name(device: &SimDevice) -> String {
    format!("{} · {}", device.name, device.runtime)
}

/// The width and height in a JPEG's start-of-frame header; `None` if it has none.
pub(crate) fn jpeg_size(jpeg: &[u8]) -> Option<(u32, u32)> {
    if jpeg.get(..2)? != [0xFF, 0xD8] {
        return None;
    }
    let mut at = 2;
    loop {
        if *jpeg.get(at)? != 0xFF {
            return None;
        }
        // Fill bytes may precede a marker.
        while *jpeg.get(at + 1)? == 0xFF {
            at += 1;
        }
        let marker = *jpeg.get(at + 1)?;
        at += 2;
        // Markers without a length.
        if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            continue;
        }
        let length = usize::from(u16::from_be_bytes([*jpeg.get(at)?, *jpeg.get(at + 1)?]));
        // Every start-of-frame marker except DHT (C4), JPG (C8), and DAC (CC).
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            let height = u16::from_be_bytes([*jpeg.get(at + 3)?, *jpeg.get(at + 4)?]);
            let width = u16::from_be_bytes([*jpeg.get(at + 5)?, *jpeg.get(at + 6)?]);
            return (width > 0 && height > 0).then_some((u32::from(width), u32::from(height)));
        }
        if marker == 0xDA || length < 2 {
            return None;
        }
        at += length;
    }
}

/// A temporary file, removed when dropped: also when a screenshot's future is dropped
/// mid-await.
struct RemovedOnDrop(PathBuf);

impl Drop for RemovedOnDrop {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.0)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%error, "could not remove a simulator screenshot");
        }
    }
}

/// The screenshot `simctl` wrote to `path`, refused when it is larger than one full-size
/// JPEG should be.
async fn read_screenshot(path: &std::path::Path) -> Result<Vec<u8>, SimctlError> {
    let unreadable = |error: std::io::Error| {
        SimctlError::Parse(format!("could not read the simulator screenshot: {error}"))
    };
    let size = tokio::fs::metadata(path).await.map_err(unreadable)?.len();
    if size > SCREENSHOT_MAX_OUTPUT_BYTES as u64 {
        return Err(SimctlError::Parse(format!(
            "the simulator screenshot is larger than {SCREENSHOT_MAX_OUTPUT_BYTES} bytes"
        )));
    }
    tokio::fs::read(path).await.map_err(unreadable)
}

/// Runs `xcrun simctl` and `xcode-select` for the iOS backend.
#[derive(Clone)]
pub(crate) struct SimctlRunner {
    pub(crate) xcrun: PathBuf,
    pub(crate) xcode_select: PathBuf,
    pub(crate) registry: ProcessAttributionRegistry,
}

impl SimctlRunner {
    pub(crate) async fn list(&self) -> Result<Vec<SimDevice>, SimctlError> {
        let output = self
            .simctl(
                &["list", "devices", "available", "-j"],
                None,
                LIST_TIMEOUT,
                LIST_MAX_OUTPUT_BYTES,
            )
            .await?;
        parse_list(&output)
    }

    /// Boots `udid` and waits until it has finished booting, both within `timeout`.
    pub(crate) async fn boot(&self, udid: &str, timeout: Duration) -> Result<(), SimctlError> {
        let deadline = Instant::now() + timeout;
        self.simctl(&["boot", udid], None, timeout, SMALL_OUTPUT_BYTES)
            .await?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(SimctlError::Timeout {
                command: "simctl bootstatus".to_owned(),
            });
        }
        self.simctl(
            &["bootstatus", udid, "-b"],
            None,
            remaining,
            SMALL_OUTPUT_BYTES,
        )
        .await?;
        Ok(())
    }

    pub(crate) async fn shutdown(&self, udid: &str) -> Result<(), SimctlError> {
        self.simctl(
            &["shutdown", udid],
            None,
            SHUTDOWN_TIMEOUT,
            SMALL_OUTPUT_BYTES,
        )
        .await?;
        Ok(())
    }

    /// A JPEG screenshot. Xcode 27's `simctl io screenshot` no longer writes to stdout (it
    /// takes `-` as a file name), so it writes to a temporary file that is read and removed.
    pub(crate) async fn screenshot_jpeg(&self, udid: &str) -> Result<Vec<u8>, SimctlError> {
        let path = RemovedOnDrop(std::env::temp_dir().join(format!(
            "bibcode-simulator-{}.jpg",
            uuid::Uuid::new_v4().simple()
        )));
        let target = path.0.to_string_lossy().into_owned();
        let taken = self
            .simctl(
                &["io", udid, "screenshot", "--type=jpeg", &target],
                None,
                SCREENSHOT_TIMEOUT,
                SMALL_OUTPUT_BYTES,
            )
            .await;
        match taken {
            Ok(_) => read_screenshot(&path.0).await,
            Err(error) => Err(error),
        }
    }

    /// Puts `text` on the simulator's pasteboard.
    #[cfg_attr(not(test), allow(dead_code))] // Live mode's text input pastes through it.
    pub(crate) async fn pbcopy(&self, udid: &str, text: &str) -> Result<(), SimctlError> {
        self.simctl(
            &["pbcopy", udid],
            Some(text.as_bytes().to_vec()),
            PBCOPY_TIMEOUT,
            SMALL_OUTPUT_BYTES,
        )
        .await?;
        Ok(())
    }

    /// Runs `xcrun simctl <args>` and returns its stdout once it succeeds.
    async fn simctl(
        &self,
        args: &[&str],
        stdin: Option<Vec<u8>>,
        timeout: Duration,
        max_output_bytes: usize,
    ) -> Result<Vec<u8>, SimctlError> {
        let command = format!("simctl {}", args.first().copied().unwrap_or_default());
        let mut full = vec!["simctl"];
        full.extend_from_slice(args);
        let output = run(&self.xcrun, &full, stdin, timeout, max_output_bytes)
            .await
            .map_err(|error| run_error(&command, error))?;
        succeeded(&command, output)
    }

    /// Runs `xcrun <args>` within `timeout` and returns its output, failed or not.
    pub(crate) async fn xcrun_output(
        &self,
        args: &[&str],
        timeout: Duration,
    ) -> Result<SupervisedRunOutput, SimctlError> {
        let command = format!("xcrun {}", args.join(" "));
        run(&self.xcrun, args, None, timeout, SMALL_OUTPUT_BYTES)
            .await
            .map_err(|error| run_error(&command, error))
    }

    /// The active developer directory `xcode-select -p` prints.
    pub(crate) async fn developer_dir(&self) -> Result<PathBuf, SimctlError> {
        let command = "xcode-select -p";
        let output = run(
            &self.xcode_select,
            &["-p"],
            None,
            LIST_TIMEOUT,
            SMALL_OUTPUT_BYTES,
        )
        .await
        .map_err(|error| run_error(command, error))?;
        let stdout = succeeded(command, output)?;
        let dir = String::from_utf8_lossy(&stdout).trim().to_owned();
        if dir.is_empty() {
            return Err(SimctlError::Parse(
                "xcode-select printed no directory".into(),
            ));
        }
        Ok(PathBuf::from(dir))
    }

    /// Registers the booted simulator's `launchd_sim` with the profiler's sampler, so its
    /// memory and CPU are attributed to BiBCode. Best effort: `None` when it is not found.
    pub(crate) async fn register_simulator(&self, udid: &str) -> Option<SimulatorProcess> {
        let owned = udid.to_owned();
        let pid = tokio::task::spawn_blocking(move || find_launchd_sim(&owned))
            .await
            .ok()
            .flatten();
        let Some(pid) = pid else {
            tracing::debug!("no launchd_sim found for the booted simulator");
            return None;
        };
        let identity = match NativeProcessSampler::process_identity(pid) {
            Ok(identity) => identity,
            Err(error) => {
                tracing::debug!(?error, pid, "launchd_sim has no stable identity");
                return None;
            }
        };
        let registration = self
            .registry
            .register_identity(
                identity,
                ProcessRegistrationMetadata {
                    scope: AttributionScope::External,
                    kind: AttributionKind::Helper,
                    label: SIMULATOR_LABEL.to_owned(),
                    source: RegistrationSource::Helper,
                    driver: None,
                },
            )
            .inspect_err(|error| {
                tracing::warn!(%error, "could not register the simulator process");
            })
            .ok();
        Some(SimulatorProcess {
            pid,
            started: Instant::now(),
            registration,
        })
    }
}

/// A booted simulator's `launchd_sim`, registered for attribution until dropped.
pub(crate) struct SimulatorProcess {
    pub pid: u32,
    /// When BiBCode found it, which bounds its uptime from below.
    pub started: Instant,
    registration: Option<ProcessRegistration>,
}

impl SimulatorProcess {
    pub(crate) fn unregister(&mut self) {
        if let Some(registration) = self.registration.take() {
            registration.unregister();
        }
    }
}

/// Whether a process named `name` running `cmd` is the `launchd_sim` of simulator `udid`; its
/// command line names the simulator's data directory, which contains the UDID.
fn is_launchd_sim_of(name: &std::ffi::OsStr, cmd: &[std::ffi::OsString], udid: &str) -> bool {
    name == "launchd_sim" && cmd.iter().any(|arg| arg.to_string_lossy().contains(udid))
}

fn find_launchd_sim(udid: &str) -> Option<u32> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::Always)
            .without_tasks(),
    );
    system
        .processes()
        .iter()
        .find(|(_, process)| is_launchd_sim_of(process.name(), process.cmd(), udid))
        .map(|(pid, _)| pid.as_u32())
}

async fn run(
    program: &std::path::Path,
    args: &[&str],
    stdin: Option<Vec<u8>>,
    timeout: Duration,
    max_output_bytes: usize,
) -> Result<SupervisedRunOutput, SupervisedRunError> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .stdin(if stdin.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    run_supervised(
        SupervisedRunRequest {
            command,
            stdin,
            timeout,
            cleanup_timeout: CLEANUP_TIMEOUT,
            max_output_bytes,
            overflow: SupervisedOverflow::Error,
        },
        &CancellationToken::new(),
    )
    .await
}

fn run_error(command: &str, error: SupervisedRunError) -> SimctlError {
    let command = command.to_owned();
    match error {
        SupervisedRunError::Timeout => SimctlError::Timeout { command },
        SupervisedRunError::Cancelled => SimctlError::Cancelled { command },
        SupervisedRunError::Spawn(error) => SimctlError::Spawn {
            command,
            message: error.to_string(),
        },
        other => SimctlError::Spawn {
            command,
            message: run_failure(&other),
        },
    }
}

/// What went wrong with a child that did start, in words.
fn run_failure(error: &SupervisedRunError) -> String {
    match error {
        SupervisedRunError::Spawn(error) => error.to_string(),
        SupervisedRunError::Pipe { stream } => format!("its {stream} was not available"),
        SupervisedRunError::Stdin(error) => format!("could not write its input: {error}"),
        SupervisedRunError::Read { stream, source } => {
            format!("could not read its {stream}: {source}")
        }
        SupervisedRunError::OutputLimit {
            stream, max_bytes, ..
        } => format!("its {stream} was larger than {max_bytes} bytes"),
        SupervisedRunError::Timeout => "it timed out".to_owned(),
        SupervisedRunError::Cancelled => "it was cancelled".to_owned(),
        SupervisedRunError::Wait(error) => format!("could not wait for it: {error}"),
    }
}

fn succeeded(command: &str, output: SupervisedRunOutput) -> Result<Vec<u8>, SimctlError> {
    if output.status.success() {
        return Ok(output.stdout.bytes);
    }
    Err(failed(command, &output.status, &output.stderr.bytes))
}

pub(crate) fn failed(command: &str, status: &ExitStatus, stderr: &[u8]) -> SimctlError {
    SimctlError::Failed {
        command: command.to_owned(),
        status: status.to_string(),
        stderr: stderr_tail::tail_of(stderr),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ios_devices_and_skips_other_platforms() {
        let devices = parse_list(include_bytes!("fixtures/simctl-list.json")).unwrap();
        assert_eq!(devices.len(), 3);
        assert!(devices.iter().all(|d| d.runtime.starts_with("iOS ")));
        // The recording has this simulator on the iOS 26.5 runtime.
        let pro = devices
            .iter()
            .find(|d| d.udid == "308FD3CD-F46A-44B9-9B68-9A849F337C80")
            .unwrap();
        assert_eq!(display_name(pro), "iPhone 17 Pro · iOS 26.5");
        assert_eq!(pro.state, SimState::Shutdown);
        let newest = devices
            .iter()
            .find(|d| d.udid == "9AFB380D-3F4E-4688-B7AF-E0EADE5D4E81")
            .unwrap();
        assert_eq!(display_name(newest), "iPhone 18 Pro · iOS 27.0");
        assert_eq!(devices[0].runtime, "iOS 27.0", "newest runtime first");
        assert_eq!(devices[2].runtime, "iOS 26.5");
    }

    #[test]
    fn reads_states_and_rejects_malformed_listings() {
        let json = br#"{"devices":{"com.apple.CoreSimulator.SimRuntime.iOS-27-0":[
            {"udid":"A","name":"a","state":"Booted","isAvailable":true},
            {"udid":"B","name":"b","state":"Shutting Down","isAvailable":true},
            {"udid":"C","name":"c","state":"Creating","isAvailable":true},
            {"udid":"D","name":"d","state":"Shutdown","isAvailable":false}]}}"#;
        let states: Vec<_> = parse_list(json)
            .unwrap()
            .into_iter()
            .map(|d| (d.udid, d.state))
            .collect();
        assert_eq!(
            states,
            [
                ("A".to_owned(), SimState::Booted),
                ("B".to_owned(), SimState::ShuttingDown),
                ("C".to_owned(), SimState::Other),
            ]
        );
        assert!(matches!(parse_list(b"nope"), Err(SimctlError::Parse(_))));
    }

    #[test]
    fn reads_jpeg_dimensions_from_sof0() {
        let rgb = vec![0x80u8; 1206 * 2622 * 3];
        let jpeg = crate::device::frames::encode_rgb888(&rgb, 1206, 2622).unwrap();
        assert_eq!(jpeg_size(&jpeg), Some((1206, 2622)));
        assert_eq!(jpeg_size(b"not a jpeg"), None);
        assert_eq!(
            jpeg_size(&jpeg[..20]),
            None,
            "truncated before the frame header"
        );
    }

    #[test]
    fn matches_the_launchd_sim_of_one_simulator() {
        let cmd = [
            "launchd_sim".into(),
            "/Users/u/Library/Developer/CoreSimulator/Devices/UDID-1/data/var/run/launchd_bootstrap.plist".into(),
        ];
        assert!(is_launchd_sim_of("launchd_sim".as_ref(), &cmd, "UDID-1"));
        assert!(!is_launchd_sim_of("launchd_sim".as_ref(), &cmd, "UDID-2"));
        assert!(!is_launchd_sim_of("launchd".as_ref(), &cmd, "UDID-1"));
    }

    #[tokio::test]
    async fn runs_simctl_through_the_fake_xcrun() {
        use crate::device::ios::testing::{FakeXcrun, SCREEN};
        let sim = FakeXcrun::new()
            .device("UDID-1", "iPhone 17 Pro", "iOS 27.0")
            .write();
        let runner = sim.runner();
        runner.boot("UDID-1", Duration::from_secs(5)).await.unwrap();
        assert_eq!(runner.list().await.unwrap()[0].state, SimState::Booted);
        runner.pbcopy("UDID-1", "hello").await.unwrap();
        assert_eq!(sim.pasteboard().as_deref(), Some("hello"));
        let jpeg = runner.screenshot_jpeg("UDID-1").await.unwrap();
        assert_eq!(jpeg_size(&jpeg), Some(SCREEN));
        runner.shutdown("UDID-1").await.unwrap();
        let calls = sim.calls();
        assert_eq!(
            calls[..4],
            [
                "boot UDID-1",
                "bootstatus UDID-1 -b",
                "list devices available -j",
                "pbcopy UDID-1",
            ]
        );
        // A file, not stdout, which Xcode 27 no longer writes to; it is removed after.
        let screenshot = calls[4]
            .strip_prefix("io UDID-1 screenshot --type=jpeg ")
            .expect("a screenshot call");
        assert!(screenshot.ends_with(".jpg"));
        assert!(!std::path::Path::new(screenshot).exists());
        assert_eq!(calls[5], "shutdown UDID-1");
    }

    #[tokio::test]
    async fn a_failed_boot_carries_its_stderr() {
        use crate::device::ios::testing::FakeXcrun;
        let sim = FakeXcrun::new()
            .device("UDID-1", "iPhone 17 Pro", "iOS 27.0")
            .boot_fails("Invalid device: UDID-1")
            .write();
        let error = sim
            .runner()
            .boot("UDID-1", Duration::from_secs(5))
            .await
            .unwrap_err();
        assert_eq!(
            error.reason(),
            "An error was encountered processing the command\nInvalid device: UDID-1"
        );
        assert!(
            sim.calls()
                .iter()
                .all(|call| !call.starts_with("bootstatus"))
        );
    }

    #[test]
    fn a_temporary_screenshot_is_removed_on_drop() {
        let path = std::env::temp_dir().join(format!(
            "bibcode-simulator-test-{}.jpg",
            uuid::Uuid::new_v4().simple()
        ));
        std::fs::write(&path, b"jpeg").unwrap();
        drop(RemovedOnDrop(path.clone()));
        assert!(!path.exists());
        // A file simctl never wrote is no error.
        drop(RemovedOnDrop(path));
    }

    #[test]
    fn run_errors_read_as_words() {
        let error = run_error(
            "simctl io",
            SupervisedRunError::OutputLimit {
                stream: "stdout",
                max_bytes: 10,
                observed_bytes: 11,
            },
        );
        assert_eq!(
            error.to_string(),
            "could not run simctl io: its stdout was larger than 10 bytes"
        );
        let error = run_error(
            "simctl list",
            SupervisedRunError::Read {
                stream: "stderr",
                source: std::io::Error::other("broken pipe"),
            },
        );
        assert_eq!(
            error.to_string(),
            "could not run simctl list: could not read its stderr: broken pipe"
        );
    }

    #[test]
    fn a_failure_names_the_command_and_its_stderr_tail() {
        use std::os::unix::process::ExitStatusExt as _;
        let error = failed(
            "simctl boot",
            &ExitStatus::from_raw(149 << 8),
            b"An error was encountered processing the command\nUnable to boot device in current state: Booted\n",
        );
        assert_eq!(
            error.to_string(),
            "simctl boot failed: An error was encountered processing the command\nUnable to boot device in current state: Booted"
        );
        assert!(error.reason().starts_with("An error"));
    }
}
