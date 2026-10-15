//! A fake `xcrun` and `xcode-select` for hermetic iOS tests: shell scripts that answer
//! `simctl` from files in a temporary directory and log every call.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use tokio::sync::watch;

use super::{IosBackend, IosBackendConfig, simctl::SimctlRunner};
use crate::device::{DeviceManager, DeviceManagerConfig};

/// The size of the JPEG every fake screenshot returns.
pub(crate) const SCREEN: (u32, u32) = (60, 130);

/// Builder for a fake Xcode whose simulators live in files.
pub(crate) struct FakeXcrun {
    dir: tempfile::TempDir,
    /// `(udid, name, runtime)` in listing order.
    devices: Vec<(String, String, String)>,
    states: Vec<(String, String)>,
    boot_failure: Option<String>,
    help_failure: Option<String>,
}

impl FakeXcrun {
    pub(crate) fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("fake xcrun dir"),
            devices: Vec::new(),
            states: Vec::new(),
            boot_failure: None,
            help_failure: None,
        }
    }

    /// A `Shutdown` simulator of `runtime` ("iOS 27.0").
    pub(crate) fn device(mut self, udid: &str, name: &str, runtime: &str) -> Self {
        self.devices
            .push((udid.to_owned(), name.to_owned(), runtime.to_owned()));
        self
    }

    /// Starts `udid` in `state` ("Booted").
    pub(crate) fn state(mut self, udid: &str, state: &str) -> Self {
        self.states.push((udid.to_owned(), state.to_owned()));
        self
    }

    /// `simctl boot` prints `message` to stderr and exits 149.
    pub(crate) fn boot_fails(mut self, message: &str) -> Self {
        self.boot_failure = Some(message.to_owned());
        self
    }

    /// `simctl help` prints `message` to stderr and exits 1.
    pub(crate) fn simctl_help_fails(mut self, message: &str) -> Self {
        self.help_failure = Some(message.to_owned());
        self
    }

    pub(crate) fn write(self) -> Self {
        use std::{fs, os::unix::fs::PermissionsExt};
        let root = self.dir.path();
        fs::create_dir_all(root.join("state")).expect("state dir");
        for (udid, _, _) in &self.devices {
            self.set_state(udid, "Shutdown");
        }
        for (udid, state) in &self.states {
            self.set_state(udid, state);
        }
        let rgb = vec![0x80u8; (SCREEN.0 * SCREEN.1 * 3) as usize];
        let jpeg = crate::device::frames::encode_rgb888(&rgb, SCREEN.0, SCREEN.1).expect("jpeg");
        fs::write(root.join("screenshot.jpg"), jpeg).expect("write screenshot");
        let write_script = |path: &Path, body: &str| {
            fs::write(path, body).expect("write script");
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod script");
        };
        write_script(&root.join("xcrun"), &self.xcrun_script());
        write_script(
            &root.join("xcode-select"),
            "#!/bin/sh\n[ \"$1\" = \"-p\" ] && echo /Applications/Xcode.app/Contents/Developer\n",
        );
        self
    }

    fn xcrun_script(&self) -> String {
        let dir = self.dir.path().display();
        let help = self.help_failure.as_ref().map_or_else(
            || "exit 0".to_owned(),
            |message| format!("echo '{message}' >&2; exit 1"),
        );
        let boot = self.boot_failure.as_ref().map_or_else(
            || format!("echo Booted > \"{dir}/state/$2\""),
            |message| {
                format!("echo 'An error was encountered processing the command' >&2; echo '{message}' >&2; exit 149")
            },
        );
        format!(
            "#!/bin/sh\ndir=\"{dir}\"\nif [ \"$1\" = xcodebuild ]; then echo 'Xcode 27.0'; echo 'Build version 27A266a'; exit 0; fi\n[ \"$1\" = simctl ] && shift\necho \"$*\" >> \"$dir/calls\"\ncase \"$1\" in\n  help) {help} ;;\n  list) {list} ;;\n  boot) {boot} ;;\n  bootstatus) exit 0 ;;\n  shutdown) echo Shutdown > \"$dir/state/$2\" ;;\n  io) cat \"$dir/screenshot.jpg\" ;;\n  pbcopy) cat > \"$dir/pasteboard\" ;;\n  *) echo \"unknown simctl command $1\" >&2; exit 1 ;;\nesac\n",
            list = self.list_script()
        )
    }

    /// Prints `simctl list devices available -j` with each device's current state file.
    fn list_script(&self) -> String {
        let mut runtimes: Vec<&str> = Vec::new();
        for (_, _, runtime) in &self.devices {
            if !runtimes.contains(&runtime.as_str()) {
                runtimes.push(runtime);
            }
        }
        let mut script = String::from("printf '{\"devices\":{'; ");
        for (index, runtime) in runtimes.iter().enumerate() {
            if index > 0 {
                script.push_str("printf ','; ");
            }
            let key = format!(
                "com.apple.CoreSimulator.SimRuntime.{}",
                runtime.replace([' ', '.'], "-")
            );
            script.push_str(&format!("printf '\"{key}\":['; "));
            let devices = self
                .devices
                .iter()
                .filter(|(_, _, device_runtime)| device_runtime == runtime);
            for (position, (udid, name, _)) in devices.enumerate() {
                if position > 0 {
                    script.push_str("printf ','; ");
                }
                script.push_str(&format!(
                    "printf '{{\"udid\":\"{udid}\",\"name\":\"{name}\",\"isAvailable\":true,\"state\":\"%s\"}}' \"$(cat \"$dir/state/{udid}\")\"; "
                ));
            }
            script.push_str("printf ']'; ");
        }
        script.push_str("printf '}}'");
        script
    }

    /// Sets `udid`'s state, as `simctl` would report it next.
    pub(crate) fn set_state(&self, udid: &str, state: &str) {
        let states = self.dir.path().join("state");
        let temp = states.join(format!("{udid}.tmp"));
        std::fs::write(&temp, state).expect("write state");
        std::fs::rename(temp, states.join(udid)).expect("publish state");
    }

    /// Every `simctl` call so far, its arguments after `simctl`.
    pub(crate) fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// What `simctl pbcopy` last received.
    pub(crate) fn pasteboard(&self) -> Option<String> {
        std::fs::read_to_string(self.dir.path().join("pasteboard")).ok()
    }

    pub(crate) fn runner(&self) -> SimctlRunner {
        SimctlRunner {
            xcrun: self.path("xcrun"),
            xcode_select: self.path("xcode-select"),
            registry: crate::diagnostics::ProcessAttributionRegistry::new(),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
}

/// A manager whose only backend is iOS on `sim`, with device previews on.
pub(crate) fn ios_manager(sim: &FakeXcrun) -> (DeviceManager, watch::Sender<bool>) {
    manager_for(sim.runner())
}

/// A manager whose only backend is iOS through `runner`, with device previews on.
pub(crate) fn manager_for(runner: SimctlRunner) -> (DeviceManager, watch::Sender<bool>) {
    let (enabled, signal) = watch::channel(true);
    let backend = IosBackend::new(IosBackendConfig {
        runner,
        boot_timeout: Duration::from_secs(10),
    });
    let manager = DeviceManager::start(DeviceManagerConfig {
        backends: vec![Arc::new(backend)],
        enabled: signal,
    });
    (manager, enabled)
}
