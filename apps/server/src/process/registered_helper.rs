//! Long-lived helper processes the server spawns, owns, and attributes to itself.

use std::{process::ExitStatus, time::Instant};

use process_wrap::tokio::{ChildWrapper, CommandWrap};
use tokio::process::ChildStderr;

use crate::diagnostics::{
    AttributionKind, AttributionScope, NativeProcessSampler, ProcessAttributionRegistry,
    ProcessRegistration, ProcessRegistrationError, ProcessRegistrationMetadata, RegistrationSource,
    profiler::exits::{self, Exited, Watch},
};

/// A spawned helper registered with the process attribution registry. Its process group is
/// killed when it is dropped (`configure_supervised_background_command_wrap`); `terminate`
/// also waits for it.
pub(crate) struct RegisteredHelper {
    pub child: Box<dyn ChildWrapper>,
    pub pid: u32,
    pub exe: String,
    pub started: Instant,
    _registration: ProcessRegistration,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum HelperSpawnError {
    #[error("{0}")]
    Failed(String),
    /// The child exited before it could be registered; its stderr pipe, when piped, is
    /// handed back so the caller can say why.
    #[error("{label} exited before ownership admission")]
    ExitedEarly {
        label: String,
        status: Option<ExitStatus>,
        stderr: Option<ChildStderr>,
    },
}

/// Spawns `command` and registers it as a helper labelled `label`. A child that has no stable
/// identity, exits before registration, or is refused by the registry is killed and reaped.
pub(crate) async fn spawn_registered_helper(
    mut command: CommandWrap,
    exe: String,
    label: &str,
    registry: &ProcessAttributionRegistry,
) -> Result<RegisteredHelper, HelperSpawnError> {
    let mut child = command
        .spawn()
        .map_err(|error| HelperSpawnError::Failed(error.to_string()))?;
    let identity = child
        .id()
        .and_then(|pid| NativeProcessSampler::process_identity(pid).ok());
    let Some(identity) = identity else {
        // An exited child has no identity to read either.
        if let Some(status) = crate::process::exited_status(&*child) {
            return Err(exited_early(child, label, status).await);
        }
        terminate_unregistered(&mut *child).await;
        return Err(HelperSpawnError::Failed(format!(
            "spawned {label} has no stable process identity"
        )));
    };
    match helper_is_live(&*child) {
        Ok(true) => {}
        Ok(false) => {
            let status = crate::process::exited_status(&*child);
            return Err(match status {
                Some(status) => exited_early(child, label, status).await,
                None => {
                    terminate_unregistered(&mut *child).await;
                    HelperSpawnError::ExitedEarly {
                        label: label.to_owned(),
                        status: None,
                        stderr: None,
                    }
                }
            });
        }
        Err(error) => {
            terminate_unregistered(&mut *child).await;
            return Err(HelperSpawnError::Failed(error));
        }
    }
    let pid = identity.pid;
    let registration = match registry.register_identity(
        identity,
        ProcessRegistrationMetadata {
            scope: AttributionScope::External,
            kind: AttributionKind::Helper,
            label: label.to_owned(),
            source: RegistrationSource::Helper,
            driver: None,
        },
    ) {
        Ok(registration) => registration,
        Err(error @ (ProcessRegistrationError::Shutdown | ProcessRegistrationError::Capacity)) => {
            terminate_unregistered(&mut *child).await;
            return Err(HelperSpawnError::Failed(error.to_string()));
        }
    };
    Ok(RegisteredHelper {
        child,
        pid,
        exe,
        started: Instant::now(),
        _registration: registration,
    })
}

impl RegisteredHelper {
    /// The root's exit status once it has exited, read without reaping it.
    pub(crate) fn exited(&self) -> Option<ExitStatus> {
        crate::process::exited_status(&*self.child)
    }

    /// Kills the whole process group and waits for it.
    pub(crate) async fn terminate(mut self) {
        let report = crate::process::supervised::terminate_and_wait(&mut *self.child).await;
        crate::process::supervised::log_cleanup_failures("registered helper", &report);
    }

    /// An exit nobody asked for: logged, and with the Profiler on a `subprocess.exit` event and
    /// `subprocess.crash` finding.
    pub(crate) fn report_unrequested_exit(&self, label: &str, status: &ExitStatus) {
        tracing::warn!(%status, label, pid = self.pid, "helper process exited");
        if exits::watching() {
            exits::report_exit(
                Watch::Failures,
                &Exited {
                    kind: "command",
                    label,
                    exe: &self.exe,
                    pid: self.pid,
                    uptime: self.started.elapsed(),
                },
                status,
                false,
            );
        }
    }
}

/// Whether a spawned child still runs, observed without reaping it so its PID and process
/// group stay ours. Never `try_wait`: it consumes the exit, and an owner must still read the
/// status of a helper that died on its own to report it.
pub(crate) fn helper_is_live(child: &dyn ChildWrapper) -> Result<bool, String> {
    #[cfg(unix)]
    {
        let process_id = child
            .id()
            .ok_or_else(|| "helper PID is unavailable".to_owned())?;
        crate::process::child_exit_without_reaping(process_id, libc::WNOHANG)
            .map(|exit| exit.is_none())
            .map_err(|error| error.to_string())
    }
    #[cfg(not(unix))]
    Ok(crate::process::exited_status(child).is_none())
}

async fn exited_early(
    mut child: Box<dyn ChildWrapper>,
    label: &str,
    status: ExitStatus,
) -> HelperSpawnError {
    let stderr = child.stderr().take();
    terminate_unregistered(&mut *child).await;
    HelperSpawnError::ExitedEarly {
        label: label.to_owned(),
        status: Some(status),
        stderr,
    }
}

async fn terminate_unregistered(child: &mut dyn ChildWrapper) {
    let _ = child.start_kill();
    let _ = child.wait().await;
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn sleeper() -> CommandWrap {
        let mut command = CommandWrap::with_new("sleep", |command| {
            command.arg("600");
        });
        crate::process::configure_supervised_background_command_wrap(&mut command);
        command
    }

    #[tokio::test]
    async fn spawned_helper_is_registered_live_and_terminated() {
        let registry = ProcessAttributionRegistry::new();
        let helper = spawn_registered_helper(sleeper(), "sleep".into(), "test helper", &registry)
            .await
            .unwrap();
        let pid = helper.pid;
        assert!(helper_is_live(&*helper.child).unwrap());
        assert!(helper.exited().is_none());
        helper.terminate().await;
        assert!(!crate::device::testing::process_alive(pid));
    }

    #[tokio::test]
    async fn exit_is_observed_without_reaping() {
        let registry = ProcessAttributionRegistry::new();
        let mut command = CommandWrap::with_new("sh", |command| {
            command.args(["-c", "sleep 1; exit 3"]);
        });
        crate::process::configure_supervised_background_command_wrap(&mut command);
        let helper = spawn_registered_helper(command, "sh".into(), "test helper", &registry)
            .await
            .unwrap();
        crate::device::testing::wait_until(|| helper.exited().is_some()).await;
        // Observed twice: the first read did not consume the exit.
        assert_eq!(helper.exited().and_then(|status| status.code()), Some(3));
        assert!(!helper_is_live(&*helper.child).unwrap());
        helper.terminate().await;
    }
}
