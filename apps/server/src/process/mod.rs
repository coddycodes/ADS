mod appimage;
mod background;
mod cleanup;
mod executable;
mod registered_helper;
mod runner;
mod shell;
pub(crate) mod supervised;
#[cfg(windows)]
mod windows_job;

pub use appimage::{ChildCommand, EnvironmentInheritance, isolate_appimage_environment};

#[cfg(windows)]
pub(crate) use windows_job::WindowsJob;

pub(crate) use background::exited_status;
#[cfg(unix)]
pub(crate) use background::{child_exit_without_reaping, exit_status};
pub use background::{
    configure_background_command, configure_background_std_command,
    configure_supervised_background_command_wrap,
};
pub(crate) use cleanup::ProcessCleanupReport;
pub(crate) use executable::{
    PreparedLaunch, SpawnPlan, launch_executable_extensions, locate_executable,
    wrap_launch_program, wrap_windows_batch_command,
};
pub(crate) use registered_helper::{RegisteredHelper, helper_is_live, spawn_registered_helper};
pub use runner::{
    OutputMode, ProcessError, ProcessRunInput, ProcessRunOutput, ProcessRunner, TimeoutBehavior,
};
pub use shell::{Platform, ShellCandidate, resolve_shell_candidates};
