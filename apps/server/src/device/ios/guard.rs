//! Whether this Mac's Xcode can run simulators, the private-API guard that decides whether
//! they run live or view-only, and the iOS platform's availability.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime},
};

use super::{
    coresim::CoreSim,
    lock,
    simctl::{SimDevice, SimctlError, SimctlRunner},
};
use crate::device::catalog::{Availability, AvailabilityKind};

pub(crate) const XCODE_MISSING_MESSAGE: &str = "Install Xcode, then open it once to finish setup.";
pub(crate) const XCODE_SETUP_MESSAGE: &str =
    "Finish Xcode setup: run `sudo xcodebuild -runFirstLaunch`.";
pub(crate) const NO_SIMULATORS_MESSAGE: &str =
    "Create a simulator in Xcode → Window → Devices and Simulators.";
/// Spec §6. Unreachable for now: spike S1 found no GUI-session requirement it could test.
pub(crate) const HEADLESS_MESSAGE: &str =
    "Live input needs someone logged in on this Mac. Simulators open as screenshots.";

/// `GuardResult::ViewOnly::missing` when another Xcode's frameworks are already loaded.
pub(crate) const RESTART_NEEDED: &str = "a server restart (another Xcode is loaded)";
pub(crate) const XCODE_CHANGED_MESSAGE: &str = "Xcode changed while the server was running. Restart the server to use live simulators; until then they open as screenshots without touch input.";

/// Why simulators of this Xcode open view-only.
pub(crate) fn unsupported_message(version: &str) -> String {
    format!(
        "Xcode {version} changed the simulator interfaces BiBCode uses. Simulators open as screenshots without touch input."
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum XcodeStatus {
    Missing,
    SetupIncomplete,
    Ready {
        developer_dir: PathBuf,
        version: String,
    },
}

/// Remembers the last `Ready` status, so a refresh re-runs the `xcrun` probes only when the
/// active developer directory or its Xcode's `version.plist` changed. A refresh still runs
/// `xcode-select -p`; a status that is not `Ready` is never kept, so finishing Xcode setup
/// (which leaves `version.plist` alone) shows on the next refresh. A pending first launch is
/// found by `xcodebuild -checkFirstLaunchStatus`, which spawns no installer.
#[derive(Default)]
pub(crate) struct XcodeCache {
    last: Mutex<Option<(PathBuf, Option<SystemTime>, XcodeStatus)>>,
}

impl XcodeCache {
    pub(crate) async fn status(&self, runner: &SimctlRunner) -> XcodeStatus {
        let Ok(dir) = runner.developer_dir().await else {
            lock(&self.last).take();
            return xcode_status(runner).await;
        };
        let stamp = version_stamp(&dir).await;
        if let Some((cached_dir, cached_stamp, status)) = lock(&self.last).as_ref()
            && *cached_dir == dir
            && *cached_stamp == stamp
        {
            return status.clone();
        }
        let status = xcode_status(runner).await;
        *lock(&self.last) =
            matches!(status, XcodeStatus::Ready { .. }).then(|| (dir, stamp, status.clone()));
        status
    }
}

/// When the Xcode that owns `developer_dir` (`…/Xcode.app/Contents/Developer`) last changed
/// its version file; `None` when it has none.
async fn version_stamp(developer_dir: &Path) -> Option<SystemTime> {
    let plist = developer_dir.parent()?.join("version.plist");
    tokio::fs::metadata(plist).await.ok()?.modified().ok()
}

/// What the private-API guard found for one Xcode.
#[derive(Clone)]
pub(crate) enum GuardResult {
    /// Both frameworks loaded and every class, selector and symbol BiBCode calls is present
    /// with the signature spec §0 recorded.
    Live(Arc<CoreSim>),
    /// Simulators open view-only; `missing` names the first name that failed, for the log.
    ViewOnly {
        kind: AvailabilityKind,
        missing: String,
    },
}

impl std::fmt::Debug for GuardResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Live(_) => f.write_str("Live"),
            Self::ViewOnly { kind, missing } => f
                .debug_struct("ViewOnly")
                .field("kind", kind)
                .field("missing", missing)
                .finish(),
        }
    }
}

/// Who answers for a selector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Owner {
    /// Instances of the class.
    Instance(&'static str),
    /// The class itself (a `+` method).
    Class(&'static str),
    /// Objects conforming to the protocol: the remote proxies CoreSimulator hands out.
    Protocol(&'static str),
}

/// One private name BiBCode depends on.
enum Need {
    Class(&'static str),
    Protocol(&'static str),
    /// `types` is the Objective-C type encoding from spec §0, compared without its offsets.
    Method {
        owner: Owner,
        selector: &'static str,
        types: Option<&'static str>,
    },
    Function(&'static str),
}

const SCREEN_CALLBACKS: &str = "registerScreenCallbacksWithUUID:callbackQueue:frameCallback:surfacesChangedCallback:propertiesChangedCallback:";

/// Every private class, protocol, selector and C symbol the live path calls (spec §0). The
/// HID entries are for Task 8's input; a simulator without them must not go live either.
const REQUIRED: &[Need] = &[
    Need::Class("SimServiceContext"),
    Need::Class("SimDeviceSet"),
    Need::Class("SimDevice"),
    Need::Class("SimDeviceIOClient"),
    Need::Class("SimulatorKit.SimDeviceLegacyHIDClient"),
    Need::Protocol("SimDeviceIOPortInterface"),
    Need::Protocol("SimDeviceIOPortDescriptorInterface"),
    Need::Protocol("SimScreen"),
    Need::Protocol("SimDisplayRenderable"),
    Need::Protocol("SimDisplayIOSurfaceRenderable"),
    Need::Protocol("SimDisplayDescriptorState"),
    Need::Protocol("SimScreenProperties"),
    Need::Method {
        owner: Owner::Class("SimServiceContext"),
        selector: "sharedServiceContextForDeveloperDir:error:",
        types: Some("@32@0:8@16^@24"),
    },
    Need::Method {
        owner: Owner::Instance("SimServiceContext"),
        selector: "defaultDeviceSetWithError:",
        types: Some("@24@0:8^@16"),
    },
    Need::Method {
        owner: Owner::Instance("SimDeviceSet"),
        selector: "devices",
        types: Some("@16@0:8"),
    },
    Need::Method {
        owner: Owner::Instance("SimDevice"),
        selector: "UDID",
        types: Some("@16@0:8"),
    },
    Need::Method {
        owner: Owner::Instance("SimDevice"),
        selector: "io",
        types: Some("@16@0:8"),
    },
    Need::Method {
        owner: Owner::Instance("SimDevice"),
        selector: "lookup:error:",
        types: Some("I32@0:8@16^@24"),
    },
    Need::Method {
        owner: Owner::Instance("SimDeviceIOClient"),
        selector: "ioPorts",
        types: Some("@16@0:8"),
    },
    Need::Method {
        owner: Owner::Protocol("SimDeviceIOPortInterface"),
        selector: "descriptor",
        types: Some("@16@0:8"),
    },
    Need::Method {
        owner: Owner::Protocol("SimDeviceIOPortDescriptorInterface"),
        selector: "state",
        types: Some("@16@0:8"),
    },
    Need::Method {
        owner: Owner::Protocol("SimDisplayDescriptorState"),
        selector: "displayClass",
        types: Some("S16@0:8"),
    },
    Need::Method {
        owner: Owner::Protocol("SimScreen"),
        selector: SCREEN_CALLBACKS,
        types: Some("v56@0:8@16@24@?32@?40@?48"),
    },
    Need::Method {
        owner: Owner::Protocol("SimScreen"),
        selector: "unregisterScreenCallbacksWithUUID:",
        types: Some("v24@0:8@16"),
    },
    Need::Method {
        owner: Owner::Protocol("SimScreen"),
        selector: "screenProperties",
        types: Some("@16@0:8"),
    },
    Need::Method {
        owner: Owner::Protocol("SimDisplayIOSurfaceRenderable"),
        selector: "framebufferSurface",
        types: Some("@16@0:8"),
    },
    Need::Method {
        owner: Owner::Protocol("SimScreenProperties"),
        selector: "uiOrientation",
        types: Some("I16@0:8"),
    },
    Need::Method {
        owner: Owner::Instance("SimulatorKit.SimDeviceLegacyHIDClient"),
        selector: "initWithDevice:error:",
        types: Some("@32@0:8@16^@24"),
    },
    Need::Method {
        owner: Owner::Instance("SimulatorKit.SimDeviceLegacyHIDClient"),
        selector: "sendWithMessage:freeWhenDone:completionQueue:completion:",
        // The message struct's encoding is long and private; only the presence is checked.
        types: None,
    },
    Need::Function("IndigoHIDMessageForMouseNSEvent"),
    Need::Function("IndigoHIDMessageForButton"),
    Need::Function("IndigoHIDMessageForHIDArbitrary"),
    Need::Function("xpc_endpoint_create_mach_port_4sim"),
    Need::Function("xpc_connection_create_from_endpoint"),
    Need::Function("xpc_connection_enable_sim2host_4sim"),
];

/// Looks private names up, in the loaded frameworks or in a test's table.
pub(crate) trait SymbolResolver {
    fn class(&self, name: &str) -> bool;
    fn protocol(&self, name: &str) -> bool;
    /// The method's type encoding when `owner` has `selector`.
    fn responds(&self, owner: Owner, selector: &str) -> Option<String>;
    fn function(&self, name: &str) -> bool;
    /// The loaded frameworks, once every name was found.
    fn load(&self) -> Result<Arc<CoreSim>, String>;
}

/// A type encoding without its frame offsets: `v56@0:8@16` → `v@:@`.
fn without_offsets(types: &str) -> String {
    types.chars().filter(|c| !c.is_ascii_digit()).collect()
}

/// The first required name `resolver` lacks, or whose signature changed.
fn first_missing(resolver: &dyn SymbolResolver) -> Option<String> {
    REQUIRED.iter().find_map(|need| match need {
        Need::Class(name) => (!resolver.class(name)).then(|| (*name).to_owned()),
        Need::Protocol(name) => (!resolver.protocol(name)).then(|| (*name).to_owned()),
        Need::Function(name) => (!resolver.function(name)).then(|| (*name).to_owned()),
        Need::Method {
            owner,
            selector,
            types,
        } => match (resolver.responds(*owner, selector), types) {
            (None, _) => Some((*selector).to_owned()),
            (Some(found), Some(expected))
                if without_offsets(&found) != without_offsets(expected) =>
            {
                Some(format!("{selector} (signature {found})"))
            }
            (Some(_), _) => None,
        },
    })
}

/// Checks every required name through `resolver`; never panics.
pub(crate) fn probe_with(resolver: &dyn SymbolResolver) -> GuardResult {
    let missing = match first_missing(resolver) {
        Some(missing) => missing,
        None => match resolver.load() {
            Ok(sim) => return GuardResult::Live(sim),
            Err(missing) => missing,
        },
    };
    tracing::warn!(
        missing = %missing,
        "the simulator frameworks lack a private name BiBCode uses; simulators run view-only"
    );
    GuardResult::ViewOnly {
        kind: AvailabilityKind::XcodeUnsupported,
        missing,
    }
}

type Prober = Box<dyn Fn(&Path) -> GuardResult + Send + Sync>;

/// The guard's result for the last Xcode it probed; it probes again only when the developer
/// directory or Xcode version changes.
pub(crate) struct GuardCache {
    prober: Prober,
    last: Mutex<Option<(PathBuf, String, GuardResult)>>,
    /// The developer directory and version whose frameworks are loaded into this process.
    /// They cannot be unloaded, and loading a second SimulatorKit would duplicate its
    /// classes, so no other Xcode goes live until the server restarts.
    loaded: Mutex<Option<(PathBuf, String)>>,
    probes: AtomicUsize,
}

impl GuardCache {
    /// Probes this Mac's frameworks.
    pub(crate) fn new() -> Self {
        Self::with_prober(Box::new(super::coresim::probe))
    }

    pub(crate) fn with_prober(prober: Prober) -> Self {
        Self {
            prober,
            last: Mutex::new(None),
            loaded: Mutex::new(None),
            probes: AtomicUsize::new(0),
        }
    }

    /// The guard's result for `status`, which must be `Ready`; any other status has no
    /// frameworks to load.
    pub(crate) fn get(&self, status: &XcodeStatus) -> GuardResult {
        let XcodeStatus::Ready {
            developer_dir,
            version,
        } = status
        else {
            return GuardResult::ViewOnly {
                kind: AvailabilityKind::XcodeMissing,
                missing: "Xcode".to_owned(),
            };
        };
        let mut last = lock(&self.last);
        if let Some((dir, cached_version, result)) = last.as_ref()
            && dir == developer_dir
            && cached_version == version
        {
            return result.clone();
        }
        let mut loaded = lock(&self.loaded);
        let result = match loaded.as_ref() {
            Some((dir, loaded_version)) if dir != developer_dir || loaded_version != version => {
                tracing::warn!(
                    loaded = %dir.display(),
                    %loaded_version,
                    developer_dir = %developer_dir.display(),
                    %version,
                    "Xcode changed while the server ran; restart the server to use live simulators with it"
                );
                GuardResult::ViewOnly {
                    kind: AvailabilityKind::XcodeUnsupported,
                    missing: RESTART_NEEDED.to_owned(),
                }
            }
            _ => {
                self.probes.fetch_add(1, Ordering::Relaxed);
                let result = (self.prober)(developer_dir);
                if matches!(result, GuardResult::Live(_)) {
                    *loaded = Some((developer_dir.clone(), version.clone()));
                }
                result
            }
        };
        *last = Some((developer_dir.clone(), version.clone(), result.clone()));
        result
    }

    #[cfg(test)]
    pub(crate) fn probes(&self) -> usize {
        self.probes.load(Ordering::Relaxed)
    }
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

/// How long `xcodebuild -checkFirstLaunchStatus` may take; it only reads what is installed.
const FIRST_LAUNCH_CHECK_TIMEOUT: Duration = Duration::from_secs(5);
/// How long `simctl help` and `xcodebuild -version` may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// Runs `xcode-select -p`, `xcrun xcodebuild -checkFirstLaunchStatus`, `xcrun simctl help`,
/// and `xcrun xcodebuild -version`.
pub(crate) async fn xcode_status(runner: &SimctlRunner) -> XcodeStatus {
    xcode_status_within(runner, PROBE_TIMEOUT).await
}

async fn xcode_status_within(runner: &SimctlRunner, probe_timeout: Duration) -> XcodeStatus {
    let developer_dir = match runner.developer_dir().await {
        Ok(dir) => dir,
        Err(error) => {
            tracing::debug!(%error, "xcode-select found no developer directory");
            return XcodeStatus::Missing;
        }
    };
    // With the first launch pending (an Xcode update whose CoreSimulator is not installed
    // yet), Xcode's `simctl` wrapper starts `xcodebuild -runFirstLaunch`, which waits for an
    // administrator. This check spawns nothing and answers at once.
    match runner
        .xcrun_output(
            &["xcodebuild", "-checkFirstLaunchStatus"],
            FIRST_LAUNCH_CHECK_TIMEOUT,
        )
        .await
    {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            tracing::debug!(status = %output.status, "Xcode's first launch is pending");
            return XcodeStatus::SetupIncomplete;
        }
        Err(error) => tracing::debug!(%error, "could not check Xcode's first launch"),
    }
    match runner
        .xcrun_output(&["simctl", "help"], probe_timeout)
        .await
    {
        Ok(output) if output.status.success() => {}
        Ok(output) if asks_for_setup(&output.stderr.bytes) => return XcodeStatus::SetupIncomplete,
        Ok(output) => {
            tracing::debug!(status = %output.status, "simctl is unavailable");
            return XcodeStatus::Missing;
        }
        // A `simctl` that never answers is waiting on Xcode's first-launch installer.
        Err(SimctlError::Timeout { .. }) => {
            tracing::debug!("simctl help timed out");
            return XcodeStatus::SetupIncomplete;
        }
        Err(error) => {
            tracing::debug!(%error, "simctl is unavailable");
            return XcodeStatus::Missing;
        }
    }
    let version = match runner
        .xcrun_output(&["xcodebuild", "-version"], probe_timeout)
        .await
    {
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

/// The iOS platform's availability for this Xcode, its simulators, and the guard's result.
pub(crate) fn availability_for(
    status: &XcodeStatus,
    devices: &[SimDevice],
    guard: Option<&GuardResult>,
) -> Availability {
    let (kind, message) = match (status, guard) {
        (XcodeStatus::Missing, _) => (
            AvailabilityKind::XcodeMissing,
            XCODE_MISSING_MESSAGE.to_owned(),
        ),
        (XcodeStatus::SetupIncomplete, _) => (
            AvailabilityKind::XcodeSetupIncomplete,
            XCODE_SETUP_MESSAGE.to_owned(),
        ),
        (XcodeStatus::Ready { .. }, _) if devices.is_empty() => (
            AvailabilityKind::NoSimulators,
            NO_SIMULATORS_MESSAGE.to_owned(),
        ),
        (
            XcodeStatus::Ready { .. },
            Some(GuardResult::ViewOnly {
                kind: AvailabilityKind::ViewOnlyHeadless,
                ..
            }),
        ) => (
            AvailabilityKind::ViewOnlyHeadless,
            HEADLESS_MESSAGE.to_owned(),
        ),
        (XcodeStatus::Ready { .. }, Some(GuardResult::ViewOnly { missing, .. }))
            if missing == RESTART_NEEDED =>
        {
            (
                AvailabilityKind::XcodeUnsupported,
                XCODE_CHANGED_MESSAGE.to_owned(),
            )
        }
        (XcodeStatus::Ready { version, .. }, Some(GuardResult::ViewOnly { .. })) => (
            AvailabilityKind::XcodeUnsupported,
            unsupported_message(version),
        ),
        (XcodeStatus::Ready { .. }, _) => (AvailabilityKind::Ready, String::new()),
    };
    Availability { kind, message }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::ios::simctl::SimState;

    #[test]
    fn maps_xcode_failures_to_availability() {
        assert_eq!(
            availability_for(&XcodeStatus::Missing, &[], None).kind,
            AvailabilityKind::XcodeMissing
        );
        assert_eq!(
            availability_for(&XcodeStatus::SetupIncomplete, &[], None).kind,
            AvailabilityKind::XcodeSetupIncomplete
        );
        let ready = XcodeStatus::Ready {
            developer_dir: "/x".into(),
            version: "27.0".into(),
        };
        assert_eq!(
            availability_for(&ready, &[], None).kind,
            AvailabilityKind::NoSimulators
        );
        assert_eq!(
            availability_for(&ready, &[], None).message,
            "Create a simulator in Xcode → Window → Devices and Simulators."
        );
        let device = SimDevice {
            udid: "U".into(),
            name: "iPhone".into(),
            runtime: "iOS 27.0".into(),
            state: SimState::Shutdown,
        };
        assert_eq!(
            availability_for(&ready, std::slice::from_ref(&device), None).kind,
            AvailabilityKind::Ready
        );
        let live_unsupported = GuardResult::ViewOnly {
            kind: AvailabilityKind::XcodeUnsupported,
            missing: "SimScreen".into(),
        };
        let unsupported = availability_for(
            &ready,
            std::slice::from_ref(&device),
            Some(&live_unsupported),
        );
        assert_eq!(unsupported.kind, AvailabilityKind::XcodeUnsupported);
        assert_eq!(
            unsupported.message,
            "Xcode 27.0 changed the simulator interfaces BiBCode uses. Simulators open as screenshots without touch input."
        );
        assert_eq!(
            availability_for(&ready, &[], Some(&live_unsupported)).kind,
            AvailabilityKind::NoSimulators,
            "no simulators comes before the guard"
        );
        let headless = GuardResult::ViewOnly {
            kind: AvailabilityKind::ViewOnlyHeadless,
            missing: "GUI session".into(),
        };
        let headless = availability_for(&ready, &[device], Some(&headless));
        assert_eq!(headless.kind, AvailabilityKind::ViewOnlyHeadless);
        assert_eq!(
            headless.message,
            "Live input needs someone logged in on this Mac. Simulators open as screenshots."
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

    #[cfg(unix)]
    #[tokio::test]
    async fn a_pending_first_launch_is_setup_incomplete_without_running_simctl() {
        use crate::device::ios::testing::FakeXcrun;
        let sim = FakeXcrun::new()
            .first_launch_pending()
            .device("UDID-1", "iPhone 17 Pro", "iOS 27.0")
            .write();
        let cache = XcodeCache::default();
        for _ in 0..2 {
            let status = cache.status(&sim.runner()).await;
            assert_eq!(status, XcodeStatus::SetupIncomplete);
            let availability = availability_for(&status, &[], None);
            assert_eq!(availability.kind, AvailabilityKind::XcodeSetupIncomplete);
            assert_eq!(availability.message, XCODE_SETUP_MESSAGE);
        }
        // `simctl help` would start `xcodebuild -runFirstLaunch` through Xcode's wrapper.
        assert!(sim.calls().is_empty(), "simctl ran: {:?}", sim.calls());
        assert_eq!(
            sim.first_launch_checks(),
            2,
            "SetupIncomplete is not cached"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_simctl_help_that_never_answers_is_setup_incomplete() {
        use crate::device::ios::testing::FakeXcrun;
        let sim = FakeXcrun::new().simctl_help_hangs().write();
        let started = std::time::Instant::now();
        assert_eq!(
            xcode_status_within(&sim.runner(), std::time::Duration::from_millis(500)).await,
            XcodeStatus::SetupIncomplete
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
        assert_eq!(sim.calls(), ["help"]);
    }

    #[test]
    fn a_missing_selector_gives_view_only_with_its_name() {
        let resolver = FakeResolver::complete().without_selector(
            "registerScreenCallbacksWithUUID:callbackQueue:frameCallback:surfacesChangedCallback:propertiesChangedCallback:",
        );
        match probe_with(&resolver) {
            GuardResult::ViewOnly { kind, missing } => {
                assert_eq!(kind, AvailabilityKind::XcodeUnsupported);
                assert_eq!(
                    missing,
                    "registerScreenCallbacksWithUUID:callbackQueue:frameCallback:surfacesChangedCallback:propertiesChangedCallback:"
                );
            }
            GuardResult::Live(_) => panic!("must not go live"),
        }
    }

    #[test]
    fn guard_reprobes_when_xcode_changes() {
        let cache = GuardCache::with_prober(counting_prober());
        let a = XcodeStatus::Ready {
            developer_dir: "/X1".into(),
            version: "27.0".into(),
        };
        let b = XcodeStatus::Ready {
            developer_dir: "/X1".into(),
            version: "27.1".into(),
        };
        cache.get(&a);
        cache.get(&a);
        cache.get(&b);
        assert_eq!(cache.probes(), 2);
    }

    #[test]
    fn unsupported_message_names_the_xcode_version() {
        assert_eq!(
            unsupported_message("27.1"),
            "Xcode 27.1 changed the simulator interfaces BiBCode uses. Simulators open as screenshots without touch input."
        );
    }

    /// Answers from `REQUIRED`, less what a test removed or changed; it never loads.
    struct FakeResolver {
        removed: Vec<String>,
        changed: Vec<(String, String)>,
    }

    impl FakeResolver {
        fn complete() -> Self {
            Self {
                removed: Vec::new(),
                changed: Vec::new(),
            }
        }

        fn without_selector(mut self, selector: &str) -> Self {
            self.removed.push(selector.to_owned());
            self
        }

        fn without(self, name: &str) -> Self {
            self.without_selector(name)
        }

        fn with_types(mut self, selector: &str, types: &str) -> Self {
            self.changed.push((selector.to_owned(), types.to_owned()));
            self
        }

        fn has(&self, name: &str) -> bool {
            !self.removed.iter().any(|removed| removed == name)
        }
    }

    impl SymbolResolver for FakeResolver {
        fn class(&self, name: &str) -> bool {
            self.has(name)
        }

        fn protocol(&self, name: &str) -> bool {
            self.has(name)
        }

        fn responds(&self, owner: Owner, selector: &str) -> Option<String> {
            if !self.has(selector) {
                return None;
            }
            if let Some((_, types)) = self.changed.iter().find(|(s, _)| s == selector) {
                return Some(types.clone());
            }
            REQUIRED.iter().find_map(|need| match need {
                Need::Method {
                    owner: o,
                    selector: s,
                    types,
                } if *o == owner && *s == selector => Some(types.unwrap_or("?").to_owned()),
                _ => None,
            })
        }

        fn function(&self, name: &str) -> bool {
            self.has(name)
        }

        fn load(&self) -> Result<Arc<CoreSim>, String> {
            Err("the fake loads no frameworks".to_owned())
        }
    }

    fn counting_prober() -> Prober {
        Box::new(|_| GuardResult::ViewOnly {
            kind: AvailabilityKind::XcodeUnsupported,
            missing: "fake".into(),
        })
    }

    fn missing(resolver: &FakeResolver) -> String {
        match probe_with(resolver) {
            GuardResult::ViewOnly { kind, missing } => {
                assert_eq!(kind, AvailabilityKind::XcodeUnsupported);
                missing
            }
            GuardResult::Live(_) => panic!("must not go live"),
        }
    }

    #[test]
    fn a_complete_resolver_passes_every_check() {
        assert_eq!(first_missing(&FakeResolver::complete()), None);
        // Every check passed, so the guard tried to load the frameworks.
        assert_eq!(
            missing(&FakeResolver::complete()),
            "the fake loads no frameworks"
        );
    }

    #[test]
    fn a_failed_gating_check_gives_view_only() {
        // `conformsToProtocol:` cannot pass for a protocol the runtime does not know.
        assert_eq!(
            missing(&FakeResolver::complete().without("SimScreen")),
            "SimScreen"
        );
        assert_eq!(
            missing(&FakeResolver::complete().without("SimulatorKit.SimDeviceLegacyHIDClient")),
            "SimulatorKit.SimDeviceLegacyHIDClient"
        );
        assert_eq!(
            missing(&FakeResolver::complete().without("xpc_connection_enable_sim2host_4sim")),
            "xpc_connection_enable_sim2host_4sim"
        );
        assert_eq!(
            missing(&FakeResolver::complete().without_selector("uiOrientation")),
            "uiOrientation"
        );
    }

    #[test]
    fn a_changed_signature_gives_view_only() {
        let resolver = FakeResolver::complete().with_types("framebufferSurface", "^v16@0:8");
        assert_eq!(
            missing(&resolver),
            "framebufferSurface (signature ^v16@0:8)"
        );
        // Offsets alone may differ.
        let resolver = FakeResolver::complete().with_types("framebufferSurface", "@24@0:16");
        assert_eq!(first_missing(&resolver), None);
    }

    #[test]
    fn the_guard_is_cached_per_developer_dir_and_version() {
        let cache = GuardCache::with_prober(counting_prober());
        let ready = |dir: &str| XcodeStatus::Ready {
            developer_dir: dir.into(),
            version: "27.0".into(),
        };
        cache.get(&ready("/X1"));
        cache.get(&ready("/X2"));
        cache.get(&ready("/X2"));
        assert_eq!(cache.probes(), 2);
        assert!(matches!(
            cache.get(&XcodeStatus::Missing),
            GuardResult::ViewOnly { .. }
        ));
        assert_eq!(cache.probes(), 2);
    }

    #[test]
    fn a_second_xcode_never_loads_while_one_is_live() {
        let cache = GuardCache::with_prober(Box::new(|_| GuardResult::Live(CoreSim::fake())));
        let ready = |dir: &str, version: &str| XcodeStatus::Ready {
            developer_dir: dir.into(),
            version: version.into(),
        };
        assert!(matches!(
            cache.get(&ready("/X1", "27.0")),
            GuardResult::Live(_)
        ));
        for other in [ready("/X2", "27.0"), ready("/X1", "27.1")] {
            match cache.get(&other) {
                GuardResult::ViewOnly { kind, missing } => {
                    assert_eq!(kind, AvailabilityKind::XcodeUnsupported);
                    assert_eq!(missing, RESTART_NEEDED);
                }
                GuardResult::Live(_) => panic!("a second Xcode must not load"),
            }
            let device = SimDevice {
                udid: "U".into(),
                name: "iPhone".into(),
                runtime: "iOS 27.0".into(),
                state: crate::device::ios::simctl::SimState::Shutdown,
            };
            assert_eq!(
                availability_for(&other, &[device], Some(&cache.get(&other))).message,
                XCODE_CHANGED_MESSAGE
            );
        }
        assert_eq!(cache.probes(), 1, "only the first Xcode was probed");
        // Switching back to the loaded Xcode is live again.
        assert!(matches!(
            cache.get(&ready("/X1", "27.0")),
            GuardResult::Live(_)
        ));
    }

    #[tokio::test]
    async fn a_cached_xcode_status_runs_one_process_per_refresh() {
        use crate::device::ios::testing::FakeXcrun;
        let sim = FakeXcrun::new().write();
        let runner = sim.runner();
        let cache = XcodeCache::default();
        for _ in 0..3 {
            assert!(matches!(
                cache.status(&runner).await,
                XcodeStatus::Ready { .. }
            ));
        }
        // `simctl help` (and `xcodebuild -version` with it) ran once; later refreshes ran
        // only `xcode-select -p`.
        assert_eq!(sim.calls(), ["help"]);
        let license = FakeXcrun::new()
            .simctl_help_fails("You have not agreed to the Xcode license agreements.")
            .write();
        let runner = license.runner();
        let cache = XcodeCache::default();
        cache.status(&runner).await;
        cache.status(&runner).await;
        assert_eq!(license.calls(), ["help", "help"], "only Ready is cached");
    }
}
