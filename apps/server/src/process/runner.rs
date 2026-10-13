use std::{collections::BTreeMap, path::PathBuf, process::Stdio, time::Duration};

use thiserror::Error;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use super::SpawnPlan;
use super::supervised::{
    SupervisedOverflow, SupervisedRunError, SupervisedRunRequest, SupervisedStreamOutput,
    run_supervised_with_spawn_observer,
};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OutputMode {
    #[default]
    Error,
    Truncate,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum TimeoutBehavior {
    #[default]
    Error,
    TimedOutResult,
}

#[derive(Clone, Debug)]
pub struct ProcessRunInput {
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub spawn_cwd: Option<PathBuf>,
    pub timeout: Duration,
    pub env: Option<BTreeMap<String, String>>,
    pub stdin: Option<String>,
    pub max_output_bytes: usize,
    pub output_mode: OutputMode,
    pub truncated_marker: String,
    pub timeout_behavior: TimeoutBehavior,
}

impl ProcessRunInput {
    pub fn new(
        command: impl Into<String>,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            command: command.into(),
            args: args.into_iter().map(Into::into).collect(),
            cwd: None,
            spawn_cwd: None,
            timeout: DEFAULT_TIMEOUT,
            env: None,
            stdin: None,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            output_mode: OutputMode::Error,
            truncated_marker: String::new(),
            timeout_behavior: TimeoutBehavior::Error,
        }
    }

    pub fn with_max_output_bytes(mut self, max_output_bytes: usize) -> Self {
        self.max_output_bytes = max_output_bytes;
        self
    }

    pub fn with_output_mode(mut self, output_mode: OutputMode) -> Self {
        self.output_mode = output_mode;
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_timeout_behavior(mut self, timeout_behavior: TimeoutBehavior) -> Self {
        self.timeout_behavior = timeout_behavior;
        self
    }

    #[doc(hidden)]
    pub fn for_test_output(bytes: usize) -> Self {
        if cfg!(windows) {
            Self::new(
                "powershell.exe",
                [
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-Command".to_string(),
                    format!("[Console]::Out.Write('x' * {bytes})"),
                ],
            )
        } else {
            Self::new(
                "sh",
                [
                    "-c".to_string(),
                    format!("head -c {bytes} /dev/zero | tr '\\0' x"),
                ],
            )
        }
    }

    #[doc(hidden)]
    pub fn for_test_sleep(duration: Duration) -> Self {
        if cfg!(windows) {
            Self::new(
                "powershell.exe",
                [
                    "-NoProfile".to_string(),
                    "-NonInteractive".to_string(),
                    "-Command".to_string(),
                    format!("Start-Sleep -Milliseconds {}", duration.as_millis()),
                ],
            )
        } else {
            Self::new("sleep", [duration.as_secs_f64().to_string()])
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessRunOutput {
    pub stdout: String,
    pub stderr: String,
    pub code: Option<i32>,
    pub timed_out: bool,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
}

#[derive(Debug, Error)]
pub enum ProcessError {
    #[error("failed to spawn process '{command}': {message}")]
    Spawn { command: String, message: String },
    #[error("failed to write process stdin: {0}")]
    Stdin(String),
    #[error("failed to read process {stream}: {message}")]
    Read {
        stream: &'static str,
        message: String,
    },
    #[error(
        "process {stream} exceeded the {max_bytes} byte output limit after {observed_bytes} bytes"
    )]
    OutputLimit {
        stream: &'static str,
        max_bytes: usize,
        observed_bytes: usize,
    },
    #[error("process timed out after {timeout_ms}ms")]
    Timeout { timeout_ms: u128 },
    #[error("process was cancelled")]
    Cancelled,
    #[error("failed while waiting for process: {0}")]
    Wait(String),
}

impl ProcessError {
    pub const fn output_limit(&self) -> Option<(&'static str, usize)> {
        match self {
            Self::OutputLimit {
                stream, max_bytes, ..
            } => Some((*stream, *max_bytes)),
            _ => None,
        }
    }

    pub const fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

#[derive(Clone, Debug, Default)]
pub struct ProcessRunner;

impl ProcessRunner {
    pub async fn run(&self, input: ProcessRunInput) -> Result<ProcessRunOutput, ProcessError> {
        self.run_with_cancellation(input, CancellationToken::new())
            .await
    }

    pub async fn run_with_cancellation(
        &self,
        input: ProcessRunInput,
        cancellation: CancellationToken,
    ) -> Result<ProcessRunOutput, ProcessError> {
        self.run_with_cancellation_and_spawn_observer(input, cancellation, |_| {})
            .await
    }

    async fn run_with_cancellation_and_spawn_observer<F>(
        &self,
        input: ProcessRunInput,
        cancellation: CancellationToken,
        observer: F,
    ) -> Result<ProcessRunOutput, ProcessError>
    where
        F: FnOnce(Option<u32>) + Send,
    {
        let resolved = resolve_command(&input);
        // This generic runner may launch BiBCode's own binaries, which need the
        // AppImage environment. Isolation belongs at user-facing spawn sites.
        let build = |program: &std::path::Path| {
            let mut command = Command::new(program);
            command
                .args(&resolved.args)
                .stdin(if input.stdin.is_some() {
                    Stdio::piped()
                } else {
                    Stdio::null()
                })
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if let Some(cwd) = input.spawn_cwd.as_ref().or(input.cwd.as_ref()) {
                command.current_dir(cwd);
            }
            if let Some(env) = &input.env {
                command.env_clear().envs(env);
            }
            command
        };
        // A bare program with the input's own PATH would fork the whole server (#85).
        // Only a spawned child reaches the observer, so one attempt at most takes it.
        let observer = std::sync::Mutex::new(Some(observer));
        let observe = |pid| {
            let observer = observer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(observer) = observer {
                observer(pid);
            }
        };
        let result = SpawnPlan::new(std::path::Path::new(&resolved.command), build)
            .start(
                |command| {
                    let request = SupervisedRunRequest {
                        command,
                        stdin: input.stdin.as_ref().map(|stdin| stdin.as_bytes().to_vec()),
                        timeout: input.timeout,
                        cleanup_timeout: Duration::from_secs(2),
                        max_output_bytes: input.max_output_bytes,
                        overflow: match input.output_mode {
                            OutputMode::Error => SupervisedOverflow::Error,
                            OutputMode::Truncate => SupervisedOverflow::Truncate,
                        },
                    };
                    // Boxed, as in the Git runner: two inline attempts make large futures.
                    Box::pin(run_supervised_with_spawn_observer(
                        request,
                        &cancellation,
                        &observe,
                    ))
                },
                |error| match error {
                    SupervisedRunError::Spawn(error) => Some(error),
                    _ => None,
                },
            )
            .await;

        match result {
            Ok(output) => {
                let (stdout, stdout_truncated) = render_output(
                    output.stdout,
                    input.max_output_bytes,
                    &input.truncated_marker,
                );
                let (stderr, stderr_truncated) = render_output(
                    output.stderr,
                    input.max_output_bytes,
                    &input.truncated_marker,
                );
                Ok(ProcessRunOutput {
                    stdout,
                    stderr,
                    code: output.status.code(),
                    timed_out: false,
                    stdout_truncated,
                    stderr_truncated,
                })
            }
            Err(SupervisedRunError::Timeout)
                if input.timeout_behavior == TimeoutBehavior::TimedOutResult =>
            {
                Ok(ProcessRunOutput {
                    stdout: String::new(),
                    stderr: String::new(),
                    code: None,
                    timed_out: true,
                    stdout_truncated: false,
                    stderr_truncated: false,
                })
            }
            Err(error) => Err(map_supervised_error(error, &input)),
        }
    }
}

fn render_output(
    mut output: SupervisedStreamOutput,
    max_bytes: usize,
    marker: &str,
) -> (String, bool) {
    let truncated = output.truncated();
    if truncated && !marker.is_empty() && max_bytes > 0 {
        let marker = marker.as_bytes();
        let marker_len = marker.len().min(max_bytes);
        output.bytes.truncate(max_bytes - marker_len);
        output.bytes.extend_from_slice(&marker[..marker_len]);
    }
    (
        String::from_utf8_lossy(&output.bytes).into_owned(),
        truncated,
    )
}

fn map_supervised_error(error: SupervisedRunError, input: &ProcessRunInput) -> ProcessError {
    match error {
        SupervisedRunError::Spawn(error) => ProcessError::Spawn {
            command: input.command.clone(),
            message: error.to_string(),
        },
        SupervisedRunError::Pipe { stream: "stdin" } => {
            ProcessError::Stdin("spawned process did not expose stdin".to_string())
        }
        SupervisedRunError::Pipe { stream } => ProcessError::Read {
            stream,
            message: format!("spawned process did not expose {stream}"),
        },
        SupervisedRunError::Stdin(error) => ProcessError::Stdin(error.to_string()),
        SupervisedRunError::Read { stream, source } => ProcessError::Read {
            stream,
            message: source.to_string(),
        },
        SupervisedRunError::OutputLimit {
            stream,
            max_bytes,
            observed_bytes,
        } => ProcessError::OutputLimit {
            stream,
            max_bytes,
            observed_bytes,
        },
        SupervisedRunError::Timeout => ProcessError::Timeout {
            timeout_ms: input.timeout.as_millis(),
        },
        SupervisedRunError::Cancelled => ProcessError::Cancelled,
        SupervisedRunError::Wait(error) => ProcessError::Wait(error.to_string()),
    }
}

#[derive(Debug)]
struct ResolvedCommand {
    command: String,
    args: Vec<String>,
}

fn resolve_command(input: &ProcessRunInput) -> ResolvedCommand {
    #[cfg(windows)]
    if input.command.to_ascii_lowercase().ends_with(".cmd")
        || input.command.to_ascii_lowercase().ends_with(".bat")
    {
        let mut args = vec![
            "/d".to_string(),
            "/s".to_string(),
            "/v:off".to_string(),
            "/c".to_string(),
            "call".to_string(),
            input.command.clone(),
        ];
        args.extend(input.args.iter().cloned());
        return ResolvedCommand {
            command: "cmd.exe".to_string(),
            args,
        };
    }
    ResolvedCommand {
        command: input.command.clone(),
        args: input.args.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestSandbox;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn runner_uses_command_local_environment_for_parallel_children() {
        async fn run(label: &str) -> ProcessRunOutput {
            let sandbox = TestSandbox::new(label);
            let script = sandbox.executable_script(
                "print-label",
                "printf '%s' \"$FIXTURE_LABEL\"",
                "@echo off\r\n<nul set /p =%FIXTURE_LABEL%\r\nexit /b 0",
            );
            let mut input = sandbox.process_input(script, Vec::<String>::new());
            input.env = Some(sandbox.environment([("FIXTURE_LABEL", label)]));
            ProcessRunner
                .run(input)
                .await
                .expect("parallel fixture process")
        }

        let (left, right) = tokio::join!(run("left"), run("right"));
        assert_eq!(left.stdout, "left");
        assert_eq!(right.stdout, "right");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn runner_bounds_output_for_parallel_fixture_children() {
        async fn run(label: &str) -> ProcessRunOutput {
            let sandbox = TestSandbox::new(label);
            let script = sandbox.executable_script(
                "print-output",
                "printf 1234567",
                "@echo off\r\n<nul set /p =1234567\r\nexit /b 0",
            );
            ProcessRunner
                .run(
                    sandbox
                        .process_input(script, Vec::<String>::new())
                        .with_max_output_bytes(5)
                        .with_output_mode(OutputMode::Truncate),
                )
                .await
                .expect("bounded parallel fixture process")
        }

        let (left, right) = tokio::join!(run("bounded-left"), run("bounded-right"));
        assert_eq!(left.stdout, "12345");
        assert!(left.stdout_truncated);
        assert_eq!(right.stdout, "12345");
        assert!(right.stdout_truncated);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn runner_cancels_parallel_fixture_children_after_spawn() {
        async fn run(label: &str) -> ProcessError {
            let sandbox = TestSandbox::new(label);
            let script = sandbox.executable_script(
                "cancellation-child",
                "while :; do sleep 1; done",
                "@echo off\r\n:wait\r\nping -n 2 127.0.0.1 >nul\r\ngoto wait",
            );
            let cancellation = CancellationToken::new();
            let input = sandbox.process_input(script, Vec::<String>::new());
            let (spawned, spawned_rx) = tokio::sync::oneshot::channel();
            let running = tokio::spawn(ProcessRunner.run_with_cancellation_and_spawn_observer(
                input,
                cancellation.clone(),
                move |_| {
                    let _ = spawned.send(());
                },
            ));

            spawned_rx
                .await
                .expect("fixture child must finish spawning");

            cancellation.cancel();
            running
                .await
                .expect("runner task should join")
                .expect_err("cancellation must terminate the ready fixture child")
        }

        let (left, right) = tokio::join!(run("cancel-left"), run("cancel-right"));
        assert!(left.is_cancelled());
        assert!(right.is_cancelled());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn runner_returns_timeout_results_after_reaping_parallel_fixture_children() {
        async fn run(label: &str) -> ProcessRunOutput {
            let sandbox = TestSandbox::new(label);
            let script = sandbox.executable_script(
                "wait-for-timeout",
                "sleep 30",
                "@echo off\r\nping -n 31 127.0.0.1 >nul",
            );
            ProcessRunner
                .run(
                    sandbox
                        .process_input(script, Vec::<String>::new())
                        .with_timeout(Duration::ZERO)
                        .with_timeout_behavior(TimeoutBehavior::TimedOutResult),
                )
                .await
                .expect("timed out fixture child should return a timeout result")
        }

        let (left, right) = tokio::join!(run("timeout-left"), run("timeout-right"));
        assert!(left.timed_out);
        assert!(right.timed_out);
    }

    #[tokio::test]
    async fn runner_reports_spawn_error_for_a_missing_sandbox_executable() {
        let sandbox = TestSandbox::new("missing-executable");
        let error = ProcessRunner
            .run(sandbox.process_input(sandbox.path("missing-command"), Vec::<String>::new()))
            .await
            .expect_err("missing explicit executable should fail to spawn");
        assert!(matches!(error, ProcessError::Spawn { .. }));
    }

    #[test]
    fn output_rendering_covers_markers_and_command_resolution() {
        let (rendered, truncated) = render_output(
            SupervisedStreamOutput {
                bytes: b"abcd".to_vec(),
                observed_bytes: 6,
            },
            4,
            "++",
        );
        assert_eq!(rendered, "ab++");
        assert!(truncated);

        let input = ProcessRunInput::new("command", ["one", "two"]);
        let resolved = resolve_command(&input);
        assert_eq!(resolved.command, "command");
        assert_eq!(resolved.args, vec!["one", "two"]);
    }
}
