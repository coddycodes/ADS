use std::{ffi::OsString, path::PathBuf, process::Stdio, time::Duration};

use thiserror::Error;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::process::supervised::{
    SupervisedOverflow, SupervisedRunError, SupervisedRunOutput, SupervisedRunRequest,
    SupervisedStreamOutput, run_supervised,
};
use crate::process::{ChildCommand, EnvironmentInheritance, SpawnPlan};

const TRUNCATION_MARKER: &str = "\n\n[truncated]";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputPolicy {
    Truncate,
    Error,
}

#[derive(Clone, Debug)]
pub struct ProcessRequest {
    pub operation: String,
    pub command: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub env: Vec<(OsString, OsString)>,
    pub stdin: Option<Vec<u8>>,
    pub timeout: Duration,
    pub max_output_bytes: usize,
    pub output_policy: OutputPolicy,
    pub append_truncation_marker: bool,
    pub allow_non_zero_exit: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessOutput {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessBytesOutput {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("failed to spawn {command} for {operation}")]
    Spawn {
        operation: String,
        command: String,
        source: std::io::Error,
    },
    #[error("failed to access {stream} for {operation}")]
    Pipe {
        operation: String,
        stream: &'static str,
    },
    #[error("failed to read {stream} for {operation}")]
    Read {
        operation: String,
        stream: &'static str,
        source: std::io::Error,
    },
    #[error("failed to write process stdin for {operation}")]
    Stdin {
        operation: String,
        source: std::io::Error,
    },
    #[error("{operation} timed out after {timeout_ms}ms")]
    Timeout { operation: String, timeout_ms: u128 },
    #[error("{operation} was cancelled")]
    Cancelled { operation: String },
    #[error("{operation} output exceeded {max_bytes} bytes on {stream}")]
    OutputLimit {
        operation: String,
        stream: &'static str,
        max_bytes: usize,
        observed_bytes: usize,
    },
    #[error("{operation} exited with code {exit_code}")]
    NonZeroExit {
        operation: String,
        exit_code: i32,
        stdout_length: usize,
        stderr_length: usize,
        stdout: Box<str>,
        stderr: Box<str>,
    },
    #[error("{operation} completed without an exit code")]
    MissingExitCode { operation: String },
    #[error("failed to wait for {operation}")]
    Wait {
        operation: String,
        source: std::io::Error,
    },
}

/// Safe facts for user-facing errors; never contains command paths, arguments or output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProcessFailureFacts {
    kind: ProcessFailureKind,
    os_error: Option<i32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProcessFailureKind {
    LaunchNotFound,
    LaunchDenied,
    LaunchNotDirectory,
    LaunchInvalidInput,
    LaunchRejected,
    TimedOut(u128),
    Cancelled,
    OutputLimit,
    Exited(i32),
    Pipe,
    Read,
    Stdin,
    Wait,
    MissingExitCode,
}

impl ProcessFailureFacts {
    pub(crate) fn exited(code: i32) -> Self {
        Self {
            kind: ProcessFailureKind::Exited(code),
            os_error: None,
        }
    }

    pub(crate) fn message(self, program: &'static str) -> String {
        let (reason, guidance) = match self.kind {
            ProcessFailureKind::LaunchNotFound => (
                "a required file or directory was not found",
                "Check that the executable is available to this environment and that the repository folder is accessible.",
            ),
            ProcessFailureKind::LaunchDenied => (
                "permission was denied",
                "Check executable permissions and access to the repository folder.",
            ),
            ProcessFailureKind::LaunchNotDirectory => (
                "a required path is not a directory",
                "Check that the repository folder and executable location are accessible directories.",
            ),
            ProcessFailureKind::LaunchInvalidInput => (
                "the launch input was invalid",
                "Check the executable configuration and the requested input.",
            ),
            ProcessFailureKind::LaunchRejected => (
                "the operating system rejected the launch",
                "Check the executable and access to the repository folder.",
            ),
            ProcessFailureKind::TimedOut(milliseconds) => {
                return format!(
                    "{program} timed out after {milliseconds} ms. Check the operation before retrying."
                );
            }
            ProcessFailureKind::Cancelled => return format!("{program} was cancelled."),
            ProcessFailureKind::OutputLimit => {
                return format!("{program} output exceeded its limit.");
            }
            ProcessFailureKind::Exited(code) => {
                return format!(
                    "{program} exited with status {code}. Check the repository and provider state before retrying."
                );
            }
            ProcessFailureKind::Pipe => {
                return format!("Could not access {program} process input or output.");
            }
            ProcessFailureKind::Read => {
                return format!("{program} process output could not be read.");
            }
            ProcessFailureKind::Stdin => return format!("Could not send input to {program}."),
            ProcessFailureKind::Wait => {
                return format!("Could not observe {program} process completion.");
            }
            ProcessFailureKind::MissingExitCode => {
                return format!("{program} completed without an exit status.");
            }
        };
        let os_error = self
            .os_error
            .map_or_else(String::new, |code| format!(" (OS error {code})"));
        format!("Could not start {program}: {reason}{os_error}. {guidance}")
    }
}

impl ProcessError {
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled { .. })
    }

    pub(crate) fn safe_facts(&self) -> ProcessFailureFacts {
        match self {
            Self::Spawn { source, .. } => ProcessFailureFacts {
                kind: match source.kind() {
                    std::io::ErrorKind::NotFound => ProcessFailureKind::LaunchNotFound,
                    std::io::ErrorKind::PermissionDenied => ProcessFailureKind::LaunchDenied,
                    std::io::ErrorKind::NotADirectory => ProcessFailureKind::LaunchNotDirectory,
                    std::io::ErrorKind::InvalidInput => ProcessFailureKind::LaunchInvalidInput,
                    _ => ProcessFailureKind::LaunchRejected,
                },
                os_error: source.raw_os_error(),
            },
            error => ProcessFailureFacts {
                kind: match error {
                    Self::Timeout { timeout_ms, .. } => ProcessFailureKind::TimedOut(*timeout_ms),
                    Self::Cancelled { .. } => ProcessFailureKind::Cancelled,
                    Self::OutputLimit { .. } => ProcessFailureKind::OutputLimit,
                    Self::NonZeroExit { exit_code, .. } => ProcessFailureKind::Exited(*exit_code),
                    Self::Pipe { .. } => ProcessFailureKind::Pipe,
                    Self::Read { .. } => ProcessFailureKind::Read,
                    Self::Stdin { .. } => ProcessFailureKind::Stdin,
                    Self::Wait { .. } => ProcessFailureKind::Wait,
                    Self::MissingExitCode { .. } => ProcessFailureKind::MissingExitCode,
                    Self::Spawn { .. } => unreachable!("spawn handled above"),
                },
                os_error: None,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessRunner;

impl ProcessRunner {
    pub async fn run(
        &self,
        request: ProcessRequest,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, ProcessError> {
        self.run_with_environment(request, cancellation, EnvironmentInheritance::Inherit)
            .await
    }

    pub async fn run_bytes(
        &self,
        request: ProcessRequest,
        cancellation: &CancellationToken,
    ) -> Result<ProcessBytesOutput, ProcessError> {
        #[cfg(feature = "hermetic-test-guard")]
        let request = checked_guarded_request(request, EnvironmentInheritance::Inherit)?;
        let command_label = request.command.to_string_lossy().into_owned();
        let output = run_request(&request, EnvironmentInheritance::Inherit, cancellation)
            .await
            .map_err(|error| map_supervised_error(error, &request, command_label))?;

        let exit_code = output
            .status
            .code()
            .ok_or_else(|| ProcessError::MissingExitCode {
                operation: request.operation.clone(),
            })?;
        let stdout_length = output.stdout.observed_bytes;
        let stderr_length = output.stderr.observed_bytes;
        let stdout_truncated = output.stdout.truncated();
        let stderr_truncated = output.stderr.truncated();
        let stdout = render_bytes(output.stdout, request.append_truncation_marker);
        let stderr = render_bytes(output.stderr, request.append_truncation_marker);
        if exit_code != 0 && !request.allow_non_zero_exit {
            return Err(ProcessError::NonZeroExit {
                operation: request.operation,
                exit_code,
                stdout_length,
                stderr_length,
                stdout: String::from_utf8_lossy(&stdout).into_owned().into(),
                stderr: String::from_utf8_lossy(&stderr).into_owned().into(),
            });
        }
        Ok(ProcessBytesOutput {
            exit_code,
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
        })
    }

    #[cfg(test)]
    pub(crate) async fn run_with_clean_environment_for_test(
        &self,
        request: ProcessRequest,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, ProcessError> {
        self.run_with_environment(request, cancellation, EnvironmentInheritance::Cleared)
            .await
    }

    async fn run_with_environment(
        &self,
        request: ProcessRequest,
        cancellation: &CancellationToken,
        inheritance: EnvironmentInheritance,
    ) -> Result<ProcessOutput, ProcessError> {
        #[cfg(feature = "hermetic-test-guard")]
        let request = checked_guarded_request(request, inheritance)?;
        let command_label = request.command.to_string_lossy().into_owned();
        let output = run_request(&request, inheritance, cancellation)
            .await
            .map_err(|error| map_supervised_error(error, &request, command_label))?;

        let exit_code = match output.status.code() {
            Some(exit_code) => exit_code,
            None => {
                return Err(ProcessError::MissingExitCode {
                    operation: request.operation.clone(),
                });
            }
        };
        let stdout_length = output.stdout.observed_bytes;
        let stderr_length = output.stderr.observed_bytes;
        let stdout_truncated = output.stdout.truncated();
        let stderr_truncated = output.stderr.truncated();
        let stdout = render(output.stdout, request.append_truncation_marker);
        let stderr = render(output.stderr, request.append_truncation_marker);
        if exit_code != 0 && !request.allow_non_zero_exit {
            return Err(ProcessError::NonZeroExit {
                operation: request.operation,
                exit_code,
                stdout_length,
                stderr_length,
                stdout: stdout.into(),
                stderr: stderr.into(),
            });
        }
        Ok(ProcessOutput {
            exit_code,
            stdout,
            stderr,
            stdout_truncated,
            stderr_truncated,
        })
    }
}

#[cfg(feature = "hermetic-test-guard")]
fn checked_guarded_request(
    mut request: ProcessRequest,
    inheritance: EnvironmentInheritance,
) -> Result<ProcessRequest, ProcessError> {
    if !crate::hermetic_guard::is_guarded_program(&request.command) {
        return Ok(request);
    }
    let search_path = request
        .env
        .iter()
        .rev()
        .find(|(name, _)| {
            if cfg!(windows) {
                name.to_string_lossy().eq_ignore_ascii_case("PATH")
            } else {
                name == "PATH"
            }
        })
        .map(|(_, value)| value.clone())
        .or_else(|| {
            (inheritance == EnvironmentInheritance::Inherit)
                .then(|| std::env::var_os("PATH"))
                .flatten()
        });
    let path = crate::hermetic_guard::checked_launch_executable(
        &request.command,
        Some(&request.cwd),
        search_path.as_deref(),
    )
    .ok_or_else(|| ProcessError::Spawn {
        operation: request.operation.clone(),
        command: request.command.display().to_string(),
        source: std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "guarded executable was not found or was refused by the hermetic test guard",
        ),
    })?;
    request.command = path;
    Ok(request)
}

/// Runs the request with a bare `git` named by its path in the child's isolated `PATH`,
/// so it starts with `posix_spawn` instead of forking the whole server (#85). The run is
/// boxed: inline, the plan's two attempts make every Git caller's future large enough to
/// overflow a test thread's stack.
async fn run_request(
    request: &ProcessRequest,
    inheritance: EnvironmentInheritance,
    cancellation: &CancellationToken,
) -> Result<SupervisedRunOutput, SupervisedRunError> {
    SpawnPlan::new(&request.command, |program| {
        process_command(request, inheritance, program)
    })
    .start(
        |command| {
            Box::pin(run_supervised(
                supervised_request(request, command),
                cancellation,
            ))
        },
        |error| match error {
            SupervisedRunError::Spawn(error) => Some(error),
            _ => None,
        },
    )
    .await
}

fn process_command(
    request: &ProcessRequest,
    inheritance: EnvironmentInheritance,
    program: &std::path::Path,
) -> Command {
    let mut command = Command::new(program);
    if inheritance == EnvironmentInheritance::Cleared {
        command.env_clear();
    }
    command
        .args(&request.args)
        .current_dir(&request.cwd)
        .envs(request.env.iter().cloned())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::process::isolate_appimage_environment(ChildCommand::Process {
        command: command.as_std_mut(),
        inheritance,
    });
    command
}

fn supervised_request(request: &ProcessRequest, command: Command) -> SupervisedRunRequest {
    SupervisedRunRequest {
        command,
        stdin: request.stdin.clone(),
        timeout: request.timeout,
        cleanup_timeout: Duration::from_secs(2),
        max_output_bytes: request.max_output_bytes,
        overflow: match request.output_policy {
            OutputPolicy::Truncate => SupervisedOverflow::Truncate,
            OutputPolicy::Error => SupervisedOverflow::Error,
        },
    }
}

fn map_supervised_error(
    error: SupervisedRunError,
    request: &ProcessRequest,
    command: String,
) -> ProcessError {
    let operation = request.operation.clone();
    match error {
        SupervisedRunError::Spawn(source) => ProcessError::Spawn {
            operation,
            command,
            source,
        },
        SupervisedRunError::Pipe { stream } => ProcessError::Pipe { operation, stream },
        SupervisedRunError::Stdin(source) => ProcessError::Stdin { operation, source },
        SupervisedRunError::Read { stream, source } => ProcessError::Read {
            operation,
            stream,
            source,
        },
        SupervisedRunError::OutputLimit {
            stream,
            max_bytes,
            observed_bytes,
        } => ProcessError::OutputLimit {
            operation,
            stream,
            max_bytes,
            observed_bytes,
        },
        SupervisedRunError::Timeout => ProcessError::Timeout {
            operation,
            timeout_ms: request.timeout.as_millis(),
        },
        SupervisedRunError::Cancelled => ProcessError::Cancelled { operation },
        SupervisedRunError::Wait(source) => ProcessError::Wait { operation, source },
    }
}

fn render(output: SupervisedStreamOutput, append_marker: bool) -> String {
    let truncated = output.truncated();
    let mut rendered = String::from_utf8_lossy(&output.bytes).into_owned();
    if truncated && append_marker {
        rendered.push_str(TRUNCATION_MARKER);
    }
    rendered
}

fn render_bytes(output: SupervisedStreamOutput, append_marker: bool) -> Vec<u8> {
    let truncated = output.truncated();
    let mut bytes = output.bytes;
    if truncated && append_marker {
        bytes.extend_from_slice(TRUNCATION_MARKER.as_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestSandbox;

    #[test]
    fn safe_spawn_failure_preserves_category_without_private_launch_data() {
        for (kind, expected) in [
            (
                std::io::ErrorKind::NotFound,
                "required file or directory was not found",
            ),
            (
                std::io::ErrorKind::PermissionDenied,
                "permission was denied",
            ),
            (
                std::io::ErrorKind::NotADirectory,
                "a required path is not a directory",
            ),
            (
                std::io::ErrorKind::InvalidInput,
                "the launch input was invalid",
            ),
            (
                std::io::ErrorKind::Other,
                "the operating system rejected the launch",
            ),
        ] {
            let error = ProcessError::Spawn {
                operation: "private-operation-sentinel".to_owned(),
                command: "/private/executable-sentinel".to_owned(),
                source: std::io::Error::new(kind, "Bearer credential-sentinel stdout-sentinel"),
            };
            let message = error.safe_facts().message("glab");
            assert!(message.contains(expected), "{message}");
            assert!(message.contains("glab"));
            assert!(!message.contains("sentinel"));
            assert!(!message.contains("/private"));
            assert!(!message.contains("not installed"));
        }
        let error = ProcessError::Spawn {
            operation: "fixture".into(),
            command: "private".into(),
            source: std::io::Error::from_raw_os_error(2),
        };
        assert!(error.safe_facts().message("glab").contains("OS error 2"));
    }

    #[test]
    fn safe_process_failure_distinguishes_completion_without_forwarding_output() {
        for (error, expected) in [
            (
                ProcessError::NonZeroExit {
                    operation: "private-operation-sentinel".into(),
                    exit_code: 23,
                    stdout_length: 1,
                    stderr_length: 1,
                    stdout: "stdout-credential-sentinel".into(),
                    stderr: "stderr-credential-sentinel".into(),
                },
                "status 23",
            ),
            (
                ProcessError::Timeout {
                    operation: "private-operation-sentinel".into(),
                    timeout_ms: 60000,
                },
                "timed out after 60000 ms",
            ),
            (
                ProcessError::Cancelled {
                    operation: "private-operation-sentinel".into(),
                },
                "was cancelled",
            ),
            (
                ProcessError::OutputLimit {
                    operation: "private-operation-sentinel".into(),
                    stream: "stdout",
                    max_bytes: 10,
                    observed_bytes: 20,
                },
                "output exceeded its limit",
            ),
            (
                ProcessError::Read {
                    operation: "private-operation-sentinel".into(),
                    stream: "stdout",
                    source: std::io::Error::other("credential-sentinel"),
                },
                "output could not be read",
            ),
            (
                ProcessError::Wait {
                    operation: "private-operation-sentinel".into(),
                    source: std::io::Error::other("credential-sentinel"),
                },
                "process completion",
            ),
            (
                ProcessError::Pipe {
                    operation: "private-operation-sentinel".into(),
                    stream: "stdout",
                },
                "process input or output",
            ),
            (
                ProcessError::Stdin {
                    operation: "private-operation-sentinel".into(),
                    source: std::io::Error::other("credential-sentinel"),
                },
                "send input",
            ),
            (
                ProcessError::MissingExitCode {
                    operation: "private-operation-sentinel".into(),
                },
                "without an exit status",
            ),
        ] {
            let message = error.safe_facts().message("git");
            assert!(message.contains(expected), "{message}");
            assert!(!message.contains("sentinel"));
        }
    }

    fn fixture_request(sandbox: &TestSandbox, operation: &str, command: PathBuf) -> ProcessRequest {
        ProcessRequest {
            operation: operation.to_owned(),
            command,
            args: Vec::new(),
            cwd: sandbox.root().to_path_buf(),
            env: sandbox
                .environment(std::iter::empty::<(String, String)>())
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
            stdin: None,
            timeout: Duration::from_secs(30),
            max_output_bytes: 1024,
            output_policy: OutputPolicy::Truncate,
            append_truncation_marker: false,
            allow_non_zero_exit: false,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn runner_uses_command_local_environment_for_parallel_children() {
        async fn run(label: &str) -> ProcessOutput {
            let sandbox = TestSandbox::new(label);
            let script = sandbox.executable_script(
                "print-label",
                "printf '%s' \"$FIXTURE_LABEL\"",
                "@echo off\r\n<nul set /p =%FIXTURE_LABEL%\r\nexit /b 0",
            );
            let mut request = fixture_request(&sandbox, "fixture-label", script);
            request.env = sandbox
                .environment([("FIXTURE_LABEL", label)])
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect();
            ProcessRunner
                .run(request, &CancellationToken::new())
                .await
                .expect("parallel git fixture process")
        }

        let (left, right) = tokio::join!(run("left"), run("right"));
        assert_eq!(left.stdout, "left");
        assert_eq!(right.stdout, "right");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_program_that_fails_to_start_still_falls_through_the_path_search() {
        let sandbox = TestSandbox::new("git-spawn-fallback");
        let broken = sandbox.path("broken");
        let working = sandbox.path("working");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::create_dir_all(&working).unwrap();
        // Executable, but its interpreter is missing: execve fails with ENOENT, and execvp
        // goes on to the next PATH entry, as a bare name always did.
        TestSandbox::write_executable(&broken.join("fixture-tool"), "#!/nonexistent/interpreter\n");
        TestSandbox::write_executable(&working.join("fixture-tool"), "#!/bin/sh\nprintf found\n");
        let mut request =
            fixture_request(&sandbox, "fixture-fallback", PathBuf::from("fixture-tool"));
        request.env.push((
            "PATH".into(),
            std::env::join_paths([&broken, &working]).unwrap(),
        ));

        for output in [
            ProcessRunner
                .run(request.clone(), &CancellationToken::new())
                .await
                .map(|output| output.stdout.into_bytes()),
            ProcessRunner
                .run_bytes(request, &CancellationToken::new())
                .await
                .map(|output| output.stdout),
        ] {
            assert_eq!(
                output.expect("the PATH search finds the working tool"),
                b"found"
            );
        }
    }
}
