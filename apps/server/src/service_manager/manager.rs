use std::ffi::OsString;
use std::io;
use std::path::PathBuf;

use serde::Serialize;
use thiserror::Error;

use super::definitions::{
    LAUNCHD_LABEL, SYSTEMD_UNIT_NAME, ServiceSpec, WINDOWS_TASK_NAME, render_launchd_plist,
    render_systemd_unit, windows_task_command,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ServicePlatform {
    Linux,
    MacOs,
    Windows,
}

impl ServicePlatform {
    pub(crate) fn current() -> Option<Self> {
        if cfg!(target_os = "linux") {
            Some(Self::Linux)
        } else if cfg!(target_os = "macos") {
            Some(Self::MacOs)
        } else if cfg!(windows) {
            Some(Self::Windows)
        } else {
            None
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Linux => "linux",
            Self::MacOs => "macos",
            Self::Windows => "windows",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ServiceLocations {
    pub(crate) definition_path: PathBuf,
    pub(crate) log_path: PathBuf,
    pub(crate) uid: Option<u32>,
}

impl ServiceLocations {
    pub(crate) fn detect(platform: ServicePlatform) -> Result<Self, ServiceError> {
        let home = dirs::home_dir().ok_or(ServiceError::HomeDirectoryUnavailable)?;
        Ok(match platform {
            ServicePlatform::Linux => Self {
                definition_path: dirs::config_dir()
                    .unwrap_or_else(|| home.join(".config"))
                    .join("systemd")
                    .join("user")
                    .join(SYSTEMD_UNIT_NAME),
                log_path: home.join(".coddy/studio").join("service.log"),
                uid: current_uid(),
            },
            ServicePlatform::MacOs => Self {
                definition_path: home
                    .join("Library")
                    .join("LaunchAgents")
                    .join(format!("{LAUNCHD_LABEL}.plist")),
                log_path: home
                    .join("Library")
                    .join("Logs")
                    .join("coddy-studio-server.log"),
                uid: current_uid(),
            },
            ServicePlatform::Windows => Self {
                definition_path: PathBuf::from(WINDOWS_TASK_NAME),
                log_path: home.join(".coddy/studio").join("service.log"),
                uid: None,
            },
        })
    }
}

#[cfg(unix)]
fn current_uid() -> Option<u32> {
    // SAFETY: getuid has no preconditions and cannot fail.
    Some(unsafe { libc::getuid() })
}

#[cfg(not(unix))]
fn current_uid() -> Option<u32> {
    None
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CommandOutput {
    pub(crate) success: bool,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

pub(crate) trait CommandRunner {
    fn run(&self, program: &str, arguments: &[OsString]) -> io::Result<CommandOutput>;
}

pub(crate) struct ProcessCommandRunner;

impl CommandRunner for ProcessCommandRunner {
    fn run(&self, program: &str, arguments: &[OsString]) -> io::Result<CommandOutput> {
        let output = std::process::Command::new(program)
            .args(arguments)
            .output()?;
        Ok(CommandOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ServiceState {
    Active,
    Inactive,
    NotInstalled,
    Removed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ServiceReport {
    pub(crate) platform: &'static str,
    pub(crate) definition: String,
    pub(crate) state: ServiceState,
    pub(crate) executed: Vec<String>,
    pub(crate) notes: Vec<String>,
}

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error("per-user services are not supported on this platform")]
    UnsupportedPlatform,
    #[error("the home directory could not be determined")]
    HomeDirectoryUnavailable,
    #[error("failed to write the service definition {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to remove the service definition {path}: {source}")]
    Remove {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to run {program}: {source}")]
    Spawn {
        program: String,
        #[source]
        source: io::Error,
    },
    #[error("{program} {arguments} failed: {stderr}")]
    Command {
        program: String,
        arguments: String,
        stderr: String,
    },
    #[error(
        "the service definition was written to {definition} but the user service manager is not reachable ({stderr}); run these commands once lingering is enabled:\n{}",
        steps.join("\n")
    )]
    ManualStepsRequired {
        definition: PathBuf,
        steps: Vec<String>,
        stderr: String,
    },
}

struct Session<'a> {
    runner: &'a dyn CommandRunner,
    executed: Vec<String>,
}

impl<'a> Session<'a> {
    fn new(runner: &'a dyn CommandRunner) -> Self {
        Self {
            runner,
            executed: Vec::new(),
        }
    }

    fn rendered(program: &str, arguments: &[OsString]) -> String {
        std::iter::once(program.to_owned())
            .chain(
                arguments
                    .iter()
                    .map(|argument| argument.to_string_lossy().into_owned()),
            )
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Runs a command and returns its output, recording it; only spawn
    /// failures are errors here so callers decide what a non-zero exit means.
    fn run(&mut self, program: &str, arguments: &[&str]) -> Result<CommandOutput, ServiceError> {
        let arguments: Vec<OsString> = arguments
            .iter()
            .map(|argument| OsString::from(*argument))
            .collect();
        self.executed.push(Self::rendered(program, &arguments));
        self.runner
            .run(program, &arguments)
            .map_err(|source| ServiceError::Spawn {
                program: program.to_owned(),
                source,
            })
    }

    fn require(
        &mut self,
        program: &str,
        arguments: &[&str],
    ) -> Result<CommandOutput, ServiceError> {
        let output = self.run(program, arguments)?;
        if output.success {
            Ok(output)
        } else {
            Err(ServiceError::Command {
                program: program.to_owned(),
                arguments: arguments.join(" "),
                stderr: output.stderr.trim().to_owned(),
            })
        }
    }
}

fn write_definition(path: &PathBuf, contents: &str) -> Result<(), ServiceError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| ServiceError::Write {
            path: path.clone(),
            source,
        })?;
    }
    std::fs::write(path, contents).map_err(|source| ServiceError::Write {
        path: path.clone(),
        source,
    })
}

fn remove_definition(path: &PathBuf) -> Result<(), ServiceError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(ServiceError::Remove {
            path: path.clone(),
            source,
        }),
    }
}

const LINUX_MANUAL_STEPS: [&str; 3] = [
    "loginctl enable-linger",
    "systemctl --user daemon-reload",
    "systemctl --user enable --now coddy-studio.service",
];

fn report(
    platform: ServicePlatform,
    locations: &ServiceLocations,
    state: ServiceState,
    session: Session<'_>,
    notes: Vec<String>,
) -> ServiceReport {
    ServiceReport {
        platform: platform.name(),
        definition: locations.definition_path.to_string_lossy().into_owned(),
        state,
        executed: session.executed,
        notes,
    }
}

fn gui_domain(locations: &ServiceLocations) -> String {
    format!("gui/{}", locations.uid.unwrap_or(0))
}

pub(crate) fn install(
    spec: &ServiceSpec,
    platform: ServicePlatform,
    locations: &ServiceLocations,
    runner: &dyn CommandRunner,
) -> Result<ServiceReport, ServiceError> {
    let mut session = Session::new(runner);
    let mut notes = Vec::new();
    match platform {
        ServicePlatform::Linux => {
            write_definition(&locations.definition_path, &render_systemd_unit(spec))?;
            let linger = session.run("loginctl", &["enable-linger"])?;
            if !linger.success {
                notes.push(format!(
                    "lingering could not be enabled ({}); run `loginctl enable-linger` yourself so the service starts at boot without a login",
                    linger.stderr.trim()
                ));
            }
            for arguments in [
                ["--user", "daemon-reload"].as_slice(),
                ["--user", "enable", "--now", SYSTEMD_UNIT_NAME].as_slice(),
            ] {
                let output = session.run("systemctl", arguments)?;
                if !output.success {
                    let stderr = output.stderr.trim().to_owned();
                    if stderr.contains("Failed to connect to bus") {
                        return Err(ServiceError::ManualStepsRequired {
                            definition: locations.definition_path.clone(),
                            steps: LINUX_MANUAL_STEPS
                                .iter()
                                .map(|step| (*step).to_owned())
                                .collect(),
                            stderr,
                        });
                    }
                    return Err(ServiceError::Command {
                        program: "systemctl".to_owned(),
                        arguments: arguments.join(" "),
                        stderr,
                    });
                }
            }
            Ok(report(
                platform,
                locations,
                ServiceState::Active,
                session,
                notes,
            ))
        }
        ServicePlatform::MacOs => {
            write_definition(
                &locations.definition_path,
                &render_launchd_plist(spec, &locations.log_path),
            )?;
            let domain = gui_domain(locations);
            let target = format!("{domain}/{LAUNCHD_LABEL}");
            // A previous agent may be loaded; unloading it is best-effort.
            session.run("launchctl", &["bootout", target.as_str()])?;
            let definition = locations.definition_path.to_string_lossy().into_owned();
            session.require(
                "launchctl",
                &["bootstrap", domain.as_str(), definition.as_str()],
            )?;
            notes.push("a LaunchAgent runs only inside a logged-in session; enable automatic login on a server Mac so it starts after a reboot".to_owned());
            Ok(report(
                platform,
                locations,
                ServiceState::Active,
                session,
                notes,
            ))
        }
        ServicePlatform::Windows => {
            let command = windows_task_command(spec);
            session.require(
                "schtasks",
                &[
                    "/Create",
                    "/F",
                    "/TN",
                    WINDOWS_TASK_NAME,
                    "/SC",
                    "ONLOGON",
                    "/RL",
                    "LIMITED",
                    "/TR",
                    command.as_str(),
                ],
            )?;
            session.require("schtasks", &["/Run", "/TN", WINDOWS_TASK_NAME])?;
            notes.push("the task starts at logon as the current user; running without a logged-on user is not configured".to_owned());
            Ok(report(
                platform,
                locations,
                ServiceState::Active,
                session,
                notes,
            ))
        }
    }
}

pub(crate) fn uninstall(
    platform: ServicePlatform,
    locations: &ServiceLocations,
    runner: &dyn CommandRunner,
) -> Result<ServiceReport, ServiceError> {
    let mut session = Session::new(runner);
    match platform {
        ServicePlatform::Linux => {
            session.run(
                "systemctl",
                &["--user", "disable", "--now", SYSTEMD_UNIT_NAME],
            )?;
            remove_definition(&locations.definition_path)?;
            session.run("systemctl", &["--user", "daemon-reload"])?;
        }
        ServicePlatform::MacOs => {
            let target = format!("{}/{LAUNCHD_LABEL}", gui_domain(locations));
            session.run("launchctl", &["bootout", target.as_str()])?;
            remove_definition(&locations.definition_path)?;
        }
        ServicePlatform::Windows => {
            session.run("schtasks", &["/Delete", "/F", "/TN", WINDOWS_TASK_NAME])?;
        }
    }
    Ok(report(
        platform,
        locations,
        ServiceState::Removed,
        session,
        Vec::new(),
    ))
}

pub(crate) fn status(
    platform: ServicePlatform,
    locations: &ServiceLocations,
    runner: &dyn CommandRunner,
) -> Result<ServiceReport, ServiceError> {
    let mut session = Session::new(runner);
    let state = match platform {
        ServicePlatform::Linux => {
            if !locations.definition_path.exists() {
                ServiceState::NotInstalled
            } else if session
                .run("systemctl", &["--user", "is-active", SYSTEMD_UNIT_NAME])?
                .success
            {
                ServiceState::Active
            } else {
                ServiceState::Inactive
            }
        }
        ServicePlatform::MacOs => {
            if !locations.definition_path.exists() {
                ServiceState::NotInstalled
            } else {
                let target = format!("{}/{LAUNCHD_LABEL}", gui_domain(locations));
                if session
                    .run("launchctl", &["print", target.as_str()])?
                    .success
                {
                    ServiceState::Active
                } else {
                    ServiceState::Inactive
                }
            }
        }
        ServicePlatform::Windows => {
            let output = session.run(
                "schtasks",
                &["/Query", "/TN", WINDOWS_TASK_NAME, "/FO", "LIST"],
            )?;
            if !output.success {
                ServiceState::NotInstalled
            } else if output.stdout.contains("Running") {
                ServiceState::Active
            } else {
                ServiceState::Inactive
            }
        }
    };
    Ok(report(platform, locations, state, session, Vec::new()))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::path::PathBuf;

    use super::*;

    struct FakeRunner {
        calls: RefCell<Vec<String>>,
        failures: HashMap<String, String>,
        stdout: HashMap<String, String>,
    }

    impl FakeRunner {
        fn new() -> Self {
            Self {
                calls: RefCell::new(Vec::new()),
                failures: HashMap::new(),
                stdout: HashMap::new(),
            }
        }

        fn failing(mut self, command: &str, stderr: &str) -> Self {
            self.failures.insert(command.to_owned(), stderr.to_owned());
            self
        }

        fn printing(mut self, command: &str, stdout: &str) -> Self {
            self.stdout.insert(command.to_owned(), stdout.to_owned());
            self
        }

        fn calls(&self) -> Vec<String> {
            self.calls.borrow().clone()
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, program: &str, arguments: &[OsString]) -> std::io::Result<CommandOutput> {
            let rendered = std::iter::once(program.to_owned())
                .chain(
                    arguments
                        .iter()
                        .map(|argument| argument.to_string_lossy().into_owned()),
                )
                .collect::<Vec<_>>()
                .join(" ");
            self.calls.borrow_mut().push(rendered.clone());
            if let Some(stderr) = self.failures.get(&rendered) {
                return Ok(CommandOutput {
                    success: false,
                    stdout: String::new(),
                    stderr: stderr.clone(),
                });
            }
            Ok(CommandOutput {
                success: true,
                stdout: self.stdout.get(&rendered).cloned().unwrap_or_default(),
                stderr: String::new(),
            })
        }
    }

    fn spec() -> ServiceSpec {
        ServiceSpec {
            executable: PathBuf::from("/usr/bin/coddy-studio"),
            host: "100.105.196.60".to_owned(),
            port: 3773,
            base_dir: None,
            static_dir: None,
            path_env: Some("/usr/bin".into()),
        }
    }

    fn locations(temp: &tempfile::TempDir, file: &str) -> ServiceLocations {
        ServiceLocations {
            definition_path: temp.path().join("nested").join(file),
            log_path: temp.path().join("coddy-studio-server.log"),
            uid: Some(1000),
        }
    }

    #[test]
    fn linux_install_writes_the_unit_then_lingers_reloads_and_enables() {
        let current = if cfg!(target_os = "linux") {
            Some(ServicePlatform::Linux)
        } else if cfg!(target_os = "macos") {
            Some(ServicePlatform::MacOs)
        } else if cfg!(windows) {
            Some(ServicePlatform::Windows)
        } else {
            None
        };
        assert_eq!(ServicePlatform::current(), current);
        let _detect: fn(ServicePlatform) -> Result<ServiceLocations, ServiceError> =
            ServiceLocations::detect;
        let _process_runner = ProcessCommandRunner;
        let _unsupported = ServiceError::UnsupportedPlatform;
        let temp = tempfile::tempdir().expect("temp");
        let locations = locations(&temp, "coddy-studio.service");
        let runner = FakeRunner::new();
        let report: ServiceReport =
            install(&spec(), ServicePlatform::Linux, &locations, &runner).expect("install");
        assert_eq!(
            runner.calls(),
            vec![
                "loginctl enable-linger",
                "systemctl --user daemon-reload",
                "systemctl --user enable --now coddy-studio.service",
            ]
        );
        let unit = std::fs::read_to_string(&locations.definition_path).expect("unit written");
        assert_eq!(unit, render_systemd_unit(&spec()));
        assert_eq!(report.state, ServiceState::Active);
        assert_eq!(report.platform, "linux");
        assert!(report.notes.is_empty(), "{:?}", report.notes);
    }

    #[test]
    fn linux_install_reports_manual_steps_when_the_user_bus_is_unreachable() {
        let temp = tempfile::tempdir().expect("temp");
        let locations = locations(&temp, "coddy-studio.service");
        let runner = FakeRunner::new().failing(
            "systemctl --user daemon-reload",
            "Failed to connect to bus: No medium found",
        );
        let error =
            install(&spec(), ServicePlatform::Linux, &locations, &runner).expect_err("no bus");
        let ServiceError::ManualStepsRequired {
            definition,
            steps,
            stderr,
        } = error
        else {
            panic!("expected manual steps, got {error:?}");
        };
        assert_eq!(definition, locations.definition_path);
        assert_eq!(
            steps,
            vec![
                "loginctl enable-linger",
                "systemctl --user daemon-reload",
                "systemctl --user enable --now coddy-studio.service",
            ]
        );
        assert!(stderr.contains("Failed to connect to bus"));
        assert!(
            locations.definition_path.exists(),
            "the unit stays written for the manual steps"
        );
    }

    #[test]
    fn linux_linger_failure_is_a_note_not_an_error() {
        let temp = tempfile::tempdir().expect("temp");
        let locations = locations(&temp, "coddy-studio.service");
        let runner = FakeRunner::new().failing(
            "loginctl enable-linger",
            "Interactive authentication required.",
        );
        let report =
            install(&spec(), ServicePlatform::Linux, &locations, &runner).expect("install");
        assert_eq!(report.state, ServiceState::Active);
        assert_eq!(report.notes.len(), 1);
        assert!(
            report.notes[0].contains("loginctl enable-linger"),
            "{:?}",
            report.notes
        );
    }

    #[test]
    fn linux_uninstall_disables_removes_and_reloads() {
        let temp = tempfile::tempdir().expect("temp");
        let locations = locations(&temp, "coddy-studio.service");
        std::fs::create_dir_all(locations.definition_path.parent().unwrap()).unwrap();
        std::fs::write(&locations.definition_path, "unit").unwrap();
        let runner = FakeRunner::new();
        let report = uninstall(ServicePlatform::Linux, &locations, &runner).expect("uninstall");
        assert_eq!(
            runner.calls(),
            vec![
                "systemctl --user disable --now coddy-studio.service",
                "systemctl --user daemon-reload",
            ]
        );
        assert!(!locations.definition_path.exists());
        assert_eq!(report.state, ServiceState::Removed);
    }

    #[test]
    fn linux_status_reads_is_active_and_reports_missing_definitions() {
        let temp = tempfile::tempdir().expect("temp");
        let locations = locations(&temp, "coddy-studio.service");
        let runner = FakeRunner::new().printing(
            "systemctl --user is-active coddy-studio.service",
            "active\n",
        );
        let report = status(ServicePlatform::Linux, &locations, &runner).expect("status");
        assert_eq!(report.state, ServiceState::NotInstalled);
        assert!(
            runner.calls().is_empty(),
            "no service manager call without a definition"
        );

        std::fs::create_dir_all(locations.definition_path.parent().unwrap()).unwrap();
        std::fs::write(&locations.definition_path, "unit").unwrap();
        let report = status(ServicePlatform::Linux, &locations, &runner).expect("status");
        assert_eq!(report.state, ServiceState::Active);
        let inactive = FakeRunner::new().failing(
            "systemctl --user is-active coddy-studio.service",
            "inactive",
        );
        let report = status(ServicePlatform::Linux, &locations, &inactive).expect("status");
        assert_eq!(report.state, ServiceState::Inactive);
    }

    #[test]
    fn macos_install_bootstraps_the_launch_agent_for_the_gui_domain() {
        let temp = tempfile::tempdir().expect("temp");
        let locations = locations(&temp, "com.coddy.studio.server.plist");
        let runner = FakeRunner::new().failing(
            "launchctl bootout gui/1000/com.coddy.studio.server",
            "No such process",
        );
        let report =
            install(&spec(), ServicePlatform::MacOs, &locations, &runner).expect("install");
        assert_eq!(
            runner.calls(),
            vec![
                "launchctl bootout gui/1000/com.coddy.studio.server".to_owned(),
                format!(
                    "launchctl bootstrap gui/1000 {}",
                    locations.definition_path.display()
                ),
            ]
        );
        let plist = std::fs::read_to_string(&locations.definition_path).expect("plist written");
        assert_eq!(plist, render_launchd_plist(&spec(), &locations.log_path));
        assert_eq!(report.state, ServiceState::Active);
        let report = uninstall(ServicePlatform::MacOs, &locations, &runner).expect("uninstall");
        assert_eq!(report.state, ServiceState::Removed);
        assert!(!locations.definition_path.exists());
    }

    #[test]
    fn windows_install_creates_and_runs_the_logon_task() {
        let temp = tempfile::tempdir().expect("temp");
        let locations = locations(&temp, "unused");
        let runner = FakeRunner::new();
        let report =
            install(&spec(), ServicePlatform::Windows, &locations, &runner).expect("install");
        let calls = runner.calls();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(
            calls[0].starts_with(
                "schtasks /Create /F /TN Coddy Studio Server /SC ONLOGON /RL LIMITED /TR "
            ),
            "{}",
            calls[0]
        );
        assert!(
            calls[0].ends_with(&windows_task_command(&spec())),
            "{}",
            calls[0]
        );
        assert_eq!(calls[1], "schtasks /Run /TN Coddy Studio Server");
        assert_eq!(report.state, ServiceState::Active);
        assert!(
            report.notes.iter().any(|note| note.contains("logon")),
            "{:?}",
            report.notes
        );
        let report = uninstall(ServicePlatform::Windows, &locations, &runner).expect("uninstall");
        assert_eq!(
            runner.calls()[2],
            "schtasks /Delete /F /TN Coddy Studio Server"
        );
        assert_eq!(report.state, ServiceState::Removed);
    }

    #[test]
    fn command_failures_other_than_the_bus_are_errors_with_context() {
        let temp = tempfile::tempdir().expect("temp");
        let locations = locations(&temp, "coddy-studio.service");
        let runner = FakeRunner::new().failing(
            "systemctl --user enable --now coddy-studio.service",
            "Failed to enable unit: Unit file coddy-studio.service does not exist.",
        );
        let error = install(&spec(), ServicePlatform::Linux, &locations, &runner)
            .expect_err("enable failed");
        assert!(matches!(error, ServiceError::Command { .. }), "{error:?}");
        assert!(error.to_string().contains("does not exist"), "{error}");
    }
}
