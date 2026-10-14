//! Android SDK location and AVD discovery.

use std::{
    collections::HashMap,
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};

use tokio_util::sync::CancellationToken;

use super::catalog::{Availability, AvailabilityKind, Avd};
use crate::process::supervised::{SupervisedOverflow, SupervisedRunRequest, run_supervised};

const LIST_TIMEOUT: Duration = Duration::from_secs(15);
const LIST_CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);
const LIST_MAX_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct AndroidSdk {
    pub emulator: PathBuf,
    pub adb: PathBuf,
}

/// The slice of the process environment that SDK discovery reads.
#[derive(Clone, Debug)]
pub(crate) struct SdkEnv {
    pub vars: HashMap<String, OsString>,
    pub home: Option<PathBuf>,
    pub os: &'static str,
}

impl SdkEnv {
    pub(crate) fn from_process() -> Self {
        Self {
            vars: std::env::vars_os()
                .filter_map(|(key, value)| Some((key.into_string().ok()?, value)))
                .collect(),
            home: std::env::home_dir(),
            os: std::env::consts::OS,
        }
    }
}

struct ToolOutput {
    success: bool,
    stdout: String,
}

fn availability(kind: AvailabilityKind, message: String) -> Availability {
    Availability { kind, message }
}

pub(crate) fn locate_sdk(env: &SdkEnv) -> Result<AndroidSdk, Availability> {
    let exe = if env.os == "windows" { ".exe" } else { "" };
    let candidates = ["ANDROID_HOME", "ANDROID_SDK_ROOT"]
        .iter()
        .filter_map(|key| env.vars.get(*key).map(PathBuf::from))
        .chain(default_sdk_dirs(env));
    for root in candidates {
        let emulator = root.join("emulator").join(format!("emulator{exe}"));
        if !emulator.is_file() {
            continue;
        }
        let adb = root.join("platform-tools").join(format!("adb{exe}"));
        if !adb.is_file() {
            return Err(availability(
                AvailabilityKind::AdbMissing,
                format!(
                    "Android platform-tools are missing from {}. Install them with the Android SDK Manager, then restart the BiBCode server.",
                    root.display()
                ),
            ));
        }
        return Ok(AndroidSdk { emulator, adb });
    }
    Err(availability(
        AvailabilityKind::SdkMissing,
        "No Android SDK found on this server. Set ANDROID_HOME for the BiBCode server to your SDK folder and restart it.".into(),
    ))
}

fn default_sdk_dirs(env: &SdkEnv) -> Vec<PathBuf> {
    match (env.os, &env.home) {
        ("macos", Some(home)) => vec![home.join("Library/Android/sdk")],
        ("windows", _) => env
            .vars
            .get("LOCALAPPDATA")
            .map(|dir| PathBuf::from(dir).join("Android/Sdk"))
            .into_iter()
            .collect(),
        (_, Some(home)) => vec![home.join("Android/Sdk")],
        _ => Vec::new(),
    }
}

pub(crate) async fn discover(env: &SdkEnv) -> (Availability, Vec<Avd>, Option<AndroidSdk>) {
    let sdk = match locate_sdk(env) {
        Ok(sdk) => sdk,
        Err(availability) => return (availability, Vec::new(), None),
    };
    let avds = match run_tool(&sdk.emulator, &["-list-avds"]).await {
        Ok(output) => parse_avds(&output.stdout),
        Err(message) => {
            return (
                availability(AvailabilityKind::SdkMissing, message),
                Vec::new(),
                Some(sdk),
            );
        }
    };
    if avds.is_empty() {
        return (
            availability(
                AvailabilityKind::NoAvds,
                "This server's Android SDK has no virtual devices. Create one in Android Studio's Device Manager.".into(),
            ),
            avds,
            Some(sdk),
        );
    }
    match run_tool(&sdk.emulator, &["-accel-check"]).await {
        Ok(output) if output.success => (
            availability(AvailabilityKind::Ready, String::new()),
            avds,
            Some(sdk),
        ),
        _ => (
            availability(
                AvailabilityKind::AccelerationUnavailable,
                acceleration_message(env.os),
            ),
            avds,
            Some(sdk),
        ),
    }
}

fn acceleration_message(os: &str) -> String {
    match os {
        "linux" => "Hardware acceleration is unavailable. Enable KVM on this server (bare metal or a VM with nested virtualization) and give the BiBCode user access to /dev/kvm.",
        "macos" => "Hardware acceleration (Hypervisor.framework) is unavailable on this server.",
        "windows" => "Hardware acceleration is unavailable. Enable the Windows Hypervisor Platform (WHPX) on this server.",
        _ => "Hardware acceleration is unavailable on this server.",
    }
    .to_owned()
}

fn parse_avds(stdout: &str) -> Vec<Avd> {
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty() && !line.starts_with("INFO") && !line.starts_with("WARNING")
        })
        .map(|name| Avd {
            name: name.to_owned(),
            display_name: name.replace('_', " "),
        })
        .collect()
}

async fn run_tool(program: &Path, args: &[&str]) -> Result<ToolOutput, String> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let output = run_supervised(
        SupervisedRunRequest {
            command,
            stdin: None,
            timeout: LIST_TIMEOUT,
            cleanup_timeout: LIST_CLEANUP_TIMEOUT,
            max_output_bytes: LIST_MAX_OUTPUT_BYTES,
            overflow: SupervisedOverflow::Truncate,
        },
        &CancellationToken::new(),
    )
    .await
    .map_err(|error| {
        tracing::warn!(program = %program.display(), ?error, "android sdk tool failed");
        format!(
            "Could not run {} {}. Check that the Android SDK installation is intact.",
            program.display(),
            args.join(" ")
        )
    })?;
    Ok(ToolOutput {
        success: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout.bytes).into_owned(),
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::device::testing::FakeSdk;

    #[tokio::test]
    async fn reports_sdk_missing_with_actionable_message() {
        let env = SdkEnv {
            vars: HashMap::new(),
            home: Some(tempfile::tempdir().unwrap().keep()),
            os: "linux",
        };
        let (availability, avds, sdk) = discover(&env).await;
        assert_eq!(availability.kind, AvailabilityKind::SdkMissing);
        assert!(availability.message.contains("ANDROID_HOME"));
        assert!(avds.is_empty() && sdk.is_none());
    }

    #[tokio::test]
    async fn lists_avds_from_android_home() {
        let sdk = FakeSdk::new()
            .avds(&["Pixel_8_API_35", "Tablet API 34"])
            .accel_ok(true)
            .write();
        let (availability, avds, _) = discover(&sdk.env()).await;
        assert_eq!(availability.kind, AvailabilityKind::Ready);
        assert_eq!(
            avds.iter().map(|avd| avd.name.as_str()).collect::<Vec<_>>(),
            ["Pixel_8_API_35", "Tablet API 34"]
        );
        assert_eq!(avds[0].display_name, "Pixel 8 API 35");
    }

    #[tokio::test]
    async fn classifies_no_avds_adb_missing_and_no_acceleration() {
        assert_eq!(
            discover(&FakeSdk::new().write().env()).await.0.kind,
            AvailabilityKind::NoAvds
        );
        assert_eq!(
            discover(&FakeSdk::new().avds(&["a"]).without_adb().write().env())
                .await
                .0
                .kind,
            AvailabilityKind::AdbMissing
        );
        let no_accel = discover(&FakeSdk::new().avds(&["a"]).accel_ok(false).write().env())
            .await
            .0;
        assert_eq!(no_accel.kind, AvailabilityKind::AccelerationUnavailable);
        assert!(no_accel.message.contains("KVM"));
    }

    #[test]
    fn prefers_android_home_over_sdk_root_and_defaults() {
        let first = FakeSdk::new().write();
        let second = FakeSdk::new().write();
        let mut env = second.env();
        env.vars
            .insert("ANDROID_SDK_ROOT".into(), second.root().into());
        env.vars.insert("ANDROID_HOME".into(), first.root().into());
        assert_eq!(
            locate_sdk(&env).unwrap().emulator,
            first.root().join("emulator/emulator")
        );
    }
}
