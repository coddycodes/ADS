//! Whether this Mac's Xcode can run simulators, and the iOS platform's availability.

use std::path::PathBuf;

use super::simctl::{SimDevice, SimctlRunner};
use crate::device::catalog::{Availability, AvailabilityKind};

pub(crate) const XCODE_MISSING_MESSAGE: &str = "Install Xcode, then open it once to finish setup.";
pub(crate) const XCODE_SETUP_MESSAGE: &str =
    "Finish Xcode setup: run `sudo xcodebuild -runFirstLaunch`.";
pub(crate) const NO_SIMULATORS_MESSAGE: &str =
    "Create a simulator in Xcode → Window → Devices and Simulators.";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum XcodeStatus {
    Missing,
    SetupIncomplete,
    Ready {
        #[allow(dead_code)] // The private-API guard loads its frameworks from here.
        developer_dir: PathBuf,
        #[allow(dead_code)] // The private-API guard names it in its message.
        version: String,
    },
}

/// Whether `stderr` is Xcode asking for its license or first-launch setup.
fn asks_for_setup(stderr: &[u8]) -> bool {
    let stderr = String::from_utf8_lossy(stderr).to_lowercase();
    [
        "license",
        "runfirstlaunch",
        "first launch",
        "additional required components",
    ]
    .iter()
    .any(|needle| stderr.contains(needle))
}

/// Runs `xcode-select -p`, `xcrun simctl help`, and `xcrun xcodebuild -version`.
pub(crate) async fn xcode_status(runner: &SimctlRunner) -> XcodeStatus {
    let developer_dir = match runner.developer_dir().await {
        Ok(dir) => dir,
        Err(error) => {
            tracing::debug!(%error, "xcode-select found no developer directory");
            return XcodeStatus::Missing;
        }
    };
    match runner.xcrun_output(&["simctl", "help"]).await {
        Ok(output) if output.status.success() => {}
        Ok(output) if asks_for_setup(&output.stderr.bytes) => return XcodeStatus::SetupIncomplete,
        Ok(output) => {
            tracing::debug!(status = %output.status, "simctl is unavailable");
            return XcodeStatus::Missing;
        }
        Err(error) => {
            tracing::debug!(%error, "simctl is unavailable");
            return XcodeStatus::Missing;
        }
    }
    let version = match runner.xcrun_output(&["xcodebuild", "-version"]).await {
        Ok(output) if output.status.success() => parse_version(&output.stdout.bytes),
        Ok(output) if asks_for_setup(&output.stderr.bytes) => return XcodeStatus::SetupIncomplete,
        _ => None,
    };
    XcodeStatus::Ready {
        developer_dir,
        version: version.unwrap_or_else(|| "unknown".to_owned()),
    }
}

/// "Xcode 27.0\nBuild version 27A266a" → "27.0".
fn parse_version(stdout: &[u8]) -> Option<String> {
    String::from_utf8_lossy(stdout)
        .lines()
        .find_map(|line| line.trim().strip_prefix("Xcode "))
        .map(|version| version.trim().to_owned())
        .filter(|version| !version.is_empty())
}

/// The iOS platform's availability for this Xcode and its simulators.
pub(crate) fn availability_for(status: &XcodeStatus, devices: &[SimDevice]) -> Availability {
    let (kind, message) = match status {
        XcodeStatus::Missing => (AvailabilityKind::XcodeMissing, XCODE_MISSING_MESSAGE),
        XcodeStatus::SetupIncomplete => {
            (AvailabilityKind::XcodeSetupIncomplete, XCODE_SETUP_MESSAGE)
        }
        XcodeStatus::Ready { .. } if devices.is_empty() => {
            (AvailabilityKind::NoSimulators, NO_SIMULATORS_MESSAGE)
        }
        XcodeStatus::Ready { .. } => (AvailabilityKind::Ready, ""),
    };
    Availability {
        kind,
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::ios::simctl::SimState;

    #[test]
    fn maps_xcode_failures_to_availability() {
        assert_eq!(
            availability_for(&XcodeStatus::Missing, &[]).kind,
            AvailabilityKind::XcodeMissing
        );
        assert_eq!(
            availability_for(&XcodeStatus::SetupIncomplete, &[]).kind,
            AvailabilityKind::XcodeSetupIncomplete
        );
        let ready = XcodeStatus::Ready {
            developer_dir: "/x".into(),
            version: "27.0".into(),
        };
        assert_eq!(
            availability_for(&ready, &[]).kind,
            AvailabilityKind::NoSimulators
        );
        assert_eq!(
            availability_for(&ready, &[]).message,
            "Create a simulator in Xcode → Window → Devices and Simulators."
        );
        let device = SimDevice {
            udid: "U".into(),
            name: "iPhone".into(),
            runtime: "iOS 27.0".into(),
            state: SimState::Shutdown,
        };
        assert_eq!(
            availability_for(&ready, &[device]).kind,
            AvailabilityKind::Ready
        );
    }

    #[test]
    fn recognizes_license_and_first_launch_prompts() {
        assert!(asks_for_setup(
            b"You have not agreed to the Xcode license agreements."
        ));
        assert!(asks_for_setup(
            b"Run 'xcodebuild -runFirstLaunch' to install additional required components."
        ));
        assert!(!asks_for_setup(
            b"xcrun: error: unable to find utility \"simctl\""
        ));
        assert_eq!(
            parse_version(b"Xcode 27.0\nBuild version 27A266a\n").as_deref(),
            Some("27.0")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reads_xcode_status_through_the_fake_tools() {
        use crate::device::ios::testing::FakeXcrun;
        let ready = FakeXcrun::new().write();
        assert!(matches!(
            xcode_status(&ready.runner()).await,
            XcodeStatus::Ready { version, .. } if version == "27.0"
        ));
        let license = FakeXcrun::new()
            .simctl_help_fails("You have not agreed to the Xcode license agreements.")
            .write();
        assert_eq!(
            xcode_status(&license.runner()).await,
            XcodeStatus::SetupIncomplete
        );
        let tools_only = FakeXcrun::new()
            .simctl_help_fails("xcrun: error: unable to find utility \"simctl\"")
            .write();
        assert_eq!(
            xcode_status(&tools_only.runner()).await,
            XcodeStatus::Missing
        );
        let mut missing = FakeXcrun::new().write().runner();
        missing.xcode_select = "/nonexistent/xcode-select".into();
        assert_eq!(xcode_status(&missing).await, XcodeStatus::Missing);
    }
}
