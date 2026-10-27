use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
    time::Duration,
};

use coddy_studio_server::process::{
    OutputMode, ProcessError, ProcessRunInput, ProcessRunner, TimeoutBehavior,
};
#[cfg(unix)]
use serde_json::Value;
use tempfile::TempDir;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

#[cfg(windows)]
const WINDOWS_BROKEN_STDIN_FIXTURE: &str = "CODDY_STUDIO_PROCESS_RUNNER_BROKEN_STDIN_FIXTURE";
#[cfg(windows)]
const WINDOWS_BROKEN_STDIN_TREE: &str = "CODDY_STUDIO_PROCESS_RUNNER_BROKEN_STDIN_TREE";
#[cfg(windows)]
const WINDOWS_BROKEN_STDIN_ARGUMENT_PREFIX: &str = "CODDY_STUDIO_PROCESS_RUNNER_BROKEN_STDIN_ARG_";

#[test]
fn process_run_input_builders_preserve_defaults_and_apply_overrides() {
    let input = ProcessRunInput::new("demo-command", ["alpha", "beta"]);
    assert_eq!(input.command, "demo-command");
    assert_eq!(input.args, vec!["alpha".to_string(), "beta".to_string()]);
    assert_eq!(input.cwd, None);
    assert_eq!(input.spawn_cwd, None);
    assert_eq!(input.timeout, Duration::from_secs(60));
    assert_eq!(input.env, None);
    assert_eq!(input.stdin, None);
    assert_eq!(input.max_output_bytes, 8 * 1024 * 1024);
    assert_eq!(input.output_mode, OutputMode::Error);
    assert!(input.truncated_marker.is_empty());
    assert_eq!(input.timeout_behavior, TimeoutBehavior::Error);

    let overridden = ProcessRunInput::new("demo-command", ["alpha"])
        .with_max_output_bytes(12)
        .with_output_mode(OutputMode::Truncate)
        .with_timeout(Duration::from_millis(125))
        .with_timeout_behavior(TimeoutBehavior::TimedOutResult);
    assert_eq!(overridden.max_output_bytes, 12);
    assert_eq!(overridden.output_mode, OutputMode::Truncate);
    assert_eq!(overridden.timeout, Duration::from_millis(125));
    assert_eq!(overridden.timeout_behavior, TimeoutBehavior::TimedOutResult);

    let output_probe = ProcessRunInput::for_test_output(7);
    let sleep_probe = ProcessRunInput::for_test_sleep(Duration::from_millis(90));
    assert!(!output_probe.command.is_empty());
    assert!(!sleep_probe.command.is_empty());
}

#[tokio::test]
async fn process_runner_captures_stdout_stderr_stdin_env_and_cwd_precedence() {
    let temp = TempDir::new().expect("temporary directory");
    let capture_script = write_capture_script(temp.path());
    let cwd_only = temp.path().join("cwd-only");
    let spawn_dir = temp.path().join("spawn");
    fs::create_dir(&cwd_only).expect("cwd directory");
    fs::create_dir(&spawn_dir).expect("spawn directory");

    let mut env_vars = BTreeMap::new();
    env_vars.insert("PROCESS_RUNNER_TEST".to_string(), "env-visible".to_string());
    #[cfg(windows)]
    if let Some(system_root) = env::var_os("SystemRoot") {
        env_vars.insert(
            "SystemRoot".to_string(),
            system_root.to_string_lossy().into_owned(),
        );
    }

    let runner = ProcessRunner;
    let mut input = capture_script_input(&capture_script);
    input.cwd = Some(cwd_only.clone());
    input.env = Some(env_vars.clone());
    input.stdin = Some(capture_stdin_payload());
    let cwd_only_output = runner.run(input).await.expect("cwd-only run");
    assert_eq!(cwd_only_output.stderr.trim(), "stderr-marker");
    let cwd_only_capture = parse_capture_output(&cwd_only_output.stdout);
    assert_eq!(cwd_only_capture.env, "env-visible");
    assert_eq!(cwd_only_capture.stdin, "stdin-visible");
    assert_eq!(
        canonical_path(&cwd_only_capture.cwd),
        canonical_path(&cwd_only)
    );

    let mut spawn_override_input = capture_script_input(&capture_script);
    spawn_override_input.cwd = Some(cwd_only);
    spawn_override_input.spawn_cwd = Some(spawn_dir.clone());
    spawn_override_input.env = Some(env_vars);
    spawn_override_input.stdin = Some(capture_stdin_payload());
    let spawn_override_output = runner
        .run(spawn_override_input)
        .await
        .expect("spawn override run");
    let spawn_override_capture = parse_capture_output(&spawn_override_output.stdout);
    assert_eq!(
        canonical_path(&spawn_override_capture.cwd),
        canonical_path(&spawn_dir)
    );
}

#[tokio::test]
async fn process_runner_clears_inherited_environment_before_applying_overrides() {
    assert!(
        env::var_os("PATH").is_some(),
        "test host must provide an inherited PATH"
    );

    let mut env_vars = BTreeMap::new();
    env_vars.insert(
        "CODDY_STUDIO_PROCESS_RUNNER_INJECTED".to_string(),
        "present".to_string(),
    );
    #[cfg(windows)]
    if let Some(system_root) = env::var_os("SystemRoot") {
        env_vars.insert(
            "SystemRoot".to_string(),
            system_root.to_string_lossy().into_owned(),
        );
    }

    let mut input = environment_probe_input();
    input.env = Some(env_vars);
    let output = ProcessRunner.run(input).await.expect("environment probe");
    let child_env = parse_environment(&output.stdout);

    assert_eq!(
        child_env
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("CODDY_STUDIO_PROCESS_RUNNER_INJECTED"))
            .map(|(_, value)| value.as_str()),
        Some("present")
    );
    assert!(
        child_env
            .keys()
            .all(|name| !name.eq_ignore_ascii_case("PATH")),
        "child unexpectedly inherited PATH: {child_env:?}"
    );
}

#[tokio::test]
async fn process_runner_returns_nonzero_exit_and_captures_output() {
    let temp = TempDir::new().expect("temporary directory");
    let script = write_nonzero_script(temp.path());

    let output = ProcessRunner
        .run(script_input(&script, &[]))
        .await
        .expect("nonzero process still returns output");

    assert_eq!(output.code, Some(7));
    assert_eq!(output.stdout, "stdout-nonzero");
    assert_eq!(output.stderr, "stderr-nonzero");
    assert!(!output.timed_out);
    assert!(!output.stdout_truncated);
    assert!(!output.stderr_truncated);
}

#[tokio::test]
async fn process_runner_reports_spawn_errors() {
    let error = ProcessRunner
        .run(ProcessRunInput::new(
            "definitely-not-a-real-process-runner-command",
            Vec::<String>::new(),
        ))
        .await
        .expect_err("spawn should fail");

    match &error {
        ProcessError::Spawn { command, message } => {
            assert_eq!(command, "definitely-not-a-real-process-runner-command");
            assert!(!message.is_empty());
        }
        other => panic!("expected spawn error, got {other:?}"),
    }
    assert!(!error.is_cancelled());
    assert_eq!(error.output_limit(), None);
}

#[tokio::test]
async fn process_runner_enforces_output_limit_in_error_mode() {
    let error = ProcessRunner
        .run(
            ProcessRunInput::for_test_output(9)
                .with_max_output_bytes(4)
                .with_output_mode(OutputMode::Error),
        )
        .await
        .expect_err("output limit should fail");

    assert_eq!(error.output_limit(), Some(("stdout", 4)));
    match error {
        ProcessError::OutputLimit {
            stream,
            max_bytes,
            observed_bytes,
        } => {
            assert_eq!(stream, "stdout");
            assert_eq!(max_bytes, 4);
            assert!(observed_bytes > max_bytes);
        }
        other => panic!("expected output limit error, got {other:?}"),
    }
}

#[tokio::test]
async fn process_runner_handles_stderr_limits_and_truncation() {
    let temp = TempDir::new().expect("temporary directory");
    let script = write_stderr_output_script(temp.path());

    let limit_error = ProcessRunner
        .run(
            script_input(&script, &[])
                .with_max_output_bytes(4)
                .with_output_mode(OutputMode::Error),
        )
        .await
        .expect_err("stderr output limit should fail");
    match limit_error {
        ProcessError::OutputLimit {
            stream,
            max_bytes,
            observed_bytes,
        } => {
            assert_eq!(stream, "stderr");
            assert_eq!(max_bytes, 4);
            assert!(observed_bytes > max_bytes);
        }
        other => panic!("expected stderr output limit error, got {other:?}"),
    }

    let mut truncate_input = script_input(&script, &[])
        .with_max_output_bytes(5)
        .with_output_mode(OutputMode::Truncate);
    truncate_input.truncated_marker = "++".to_string();
    let truncated = ProcessRunner
        .run(truncate_input)
        .await
        .expect("stderr truncation");
    assert_eq!(truncated.stdout, "");
    assert_eq!(truncated.stderr, "yyy++");
    assert!(!truncated.stdout_truncated);
    assert!(truncated.stderr_truncated);
}

#[tokio::test]
async fn process_runner_truncates_output_and_handles_marker_edge_cases() {
    let runner = ProcessRunner;

    let mut marker_input = ProcessRunInput::for_test_output(7)
        .with_max_output_bytes(5)
        .with_output_mode(OutputMode::Truncate);
    marker_input.truncated_marker = "++".to_string();
    let marker_output = runner.run(marker_input).await.expect("marker output");
    assert_eq!(marker_output.stdout, "xxx++");
    assert!(marker_output.stdout_truncated);

    let mut short_marker_input = ProcessRunInput::for_test_output(7)
        .with_max_output_bytes(1)
        .with_output_mode(OutputMode::Truncate);
    short_marker_input.truncated_marker = "XYZ".to_string();
    let short_marker_output = runner
        .run(short_marker_input)
        .await
        .expect("short marker output");
    assert_eq!(short_marker_output.stdout, "X");
    assert!(short_marker_output.stdout_truncated);

    let mut zero_limit_input = ProcessRunInput::for_test_output(3)
        .with_max_output_bytes(0)
        .with_output_mode(OutputMode::Truncate);
    zero_limit_input.truncated_marker = "XYZ".to_string();
    let zero_limit_output = runner.run(zero_limit_input).await.expect("zero limit");
    assert_eq!(zero_limit_output.stdout, "");
    assert!(zero_limit_output.stdout_truncated);
}

#[tokio::test]
async fn process_runner_times_out_or_returns_timed_out_result() {
    let timeout_error = ProcessRunner
        .run(
            ProcessRunInput::for_test_sleep(Duration::from_millis(250))
                .with_timeout(Duration::from_millis(50)),
        )
        .await
        .expect_err("timeout error");
    match &timeout_error {
        ProcessError::Timeout { timeout_ms } => assert_eq!(*timeout_ms, 50),
        other => panic!("expected timeout error, got {other:?}"),
    }
    assert!(!timeout_error.is_cancelled());
    assert_eq!(timeout_error.output_limit(), None);

    let timed_out_result = ProcessRunner
        .run(
            ProcessRunInput::for_test_sleep(Duration::from_millis(250))
                .with_timeout(Duration::from_millis(50))
                .with_timeout_behavior(TimeoutBehavior::TimedOutResult),
        )
        .await
        .expect("timed-out result");
    assert!(timed_out_result.timed_out);
    assert_eq!(timed_out_result.code, None);
    assert_eq!(timed_out_result.stdout, "");
    assert_eq!(timed_out_result.stderr, "");
    assert!(!timed_out_result.stdout_truncated);
    assert!(!timed_out_result.stderr_truncated);
}

#[tokio::test]
async fn process_runner_returns_cancelled_for_pre_cancelled_tokens() {
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let error = ProcessRunner
        .run_with_cancellation(
            ProcessRunInput::for_test_sleep(Duration::from_millis(250)),
            cancellation,
        )
        .await
        .expect_err("pre-cancelled run should fail");

    assert!(error.is_cancelled());
    assert!(matches!(error, ProcessError::Cancelled));
}

#[tokio::test]
async fn process_runner_cancels_in_flight_requests_and_cleans_up_children() {
    let temp = TempDir::new().expect("temporary directory");
    let script = write_process_tree_script(temp.path());
    let parent_ready_path = temp.path().join("parent.ready");
    let child_ready_path = temp.path().join("child.ready");
    let grandchild_ready_path = temp.path().join("grandchild.ready");
    let parent_survived_path = temp.path().join("parent.survived");
    let child_survived_path = temp.path().join("child.survived");
    let grandchild_survived_path = temp.path().join("grandchild.survived");
    let release_path = temp.path().join("release");

    let cancellation = CancellationToken::new();
    let task = tokio::spawn({
        let cancellation = cancellation.clone();
        let input = script_input(
            &script,
            &[
                parent_ready_path.to_string_lossy().as_ref(),
                child_ready_path.to_string_lossy().as_ref(),
                grandchild_ready_path.to_string_lossy().as_ref(),
                parent_survived_path.to_string_lossy().as_ref(),
                child_survived_path.to_string_lossy().as_ref(),
                grandchild_survived_path.to_string_lossy().as_ref(),
                release_path.to_string_lossy().as_ref(),
            ],
        );
        async move {
            ProcessRunner
                .run_with_cancellation(input, cancellation)
                .await
        }
    });

    wait_for_file(&parent_ready_path).await;
    wait_for_file(&child_ready_path).await;
    wait_for_file(&grandchild_ready_path).await;

    cancellation.cancel();

    let error = task
        .await
        .expect("join cancellation task")
        .expect_err("cancelled");
    assert!(error.is_cancelled());
    assert_cleanup_sentinels_remain_absent(
        &release_path,
        &[
            &parent_survived_path,
            &child_survived_path,
            &grandchild_survived_path,
        ],
    )
    .await;
}

#[tokio::test]
async fn process_runner_cleans_up_children_when_an_in_flight_run_is_dropped() {
    let temp = TempDir::new().expect("temporary directory");
    let script = write_process_tree_script(temp.path());
    let tree = ProcessTreePaths::new(temp.path(), "dropped");
    let mut run = Box::pin(
        ProcessRunner.run_with_cancellation(tree.input(&script), CancellationToken::new()),
    );
    tokio::select! {
        () = tree.wait_until_ready() => {}
        result = &mut run => panic!("the process tree must still be running, got {result:?}"),
    }

    // An interrupted inline RPC drops its handler future this way: the token is
    // never cancelled, so the runner's cancellation branch never runs.
    drop(run);

    tree.assert_stopped().await;
}

#[tokio::test]
async fn process_runner_cleans_up_children_when_its_task_is_aborted() {
    let temp = TempDir::new().expect("temporary directory");
    let script = write_process_tree_script(temp.path());
    let tree = ProcessTreePaths::new(temp.path(), "aborted");
    let input = tree.input(&script);
    let task = tokio::spawn(async move {
        ProcessRunner
            .run_with_cancellation(input, CancellationToken::new())
            .await
    });
    tree.wait_until_ready().await;

    // Session teardown aborts a request task that outlives its join bound.
    task.abort();
    let aborted = task.await.expect_err("the aborted run must not complete");
    assert!(aborted.is_cancelled());

    tree.assert_stopped().await;
}

#[test]
fn process_runner_cleans_up_children_when_the_runtime_shuts_down_mid_run() {
    let temp = TempDir::new().expect("temporary directory");
    let script = write_process_tree_script(temp.path());
    let tree = ProcessTreePaths::new(temp.path(), "shutdown");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("server-like runtime");
    let input = tree.input(&script);
    let _run = runtime.spawn(async move {
        ProcessRunner
            .run_with_cancellation(input, CancellationToken::new())
            .await
    });
    wait_for_files_blocking(&tree.ready);

    // Shutdown drops every task, including one that is running a process.
    runtime.shutdown_timeout(Duration::from_secs(5));

    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("assertion runtime")
        .block_on(tree.assert_stopped());
}

/// A failed run still signals its process group after the root was reaped. A
/// descendant in the group that keeps the output pipes open also keeps the
/// group's id reserved, so the kill is exact; skipping it would leave the
/// descendant running after the run was cancelled.
#[cfg(unix)]
#[tokio::test]
async fn process_runner_cancellation_kills_a_descendant_that_outlives_the_root() {
    let temp = TempDir::new().expect("temporary directory");
    let script = write_outliving_descendant_script(temp.path());
    let root_pid_path = temp.path().join("root.pid");
    let ready_path = temp.path().join("descendant.ready");
    let survived_path = temp.path().join("descendant.survived");
    let release_path = temp.path().join("descendant.release");
    let cancellation = CancellationToken::new();
    let task = tokio::spawn({
        let cancellation = cancellation.clone();
        let input = script_input(
            &script,
            &[
                root_pid_path.to_string_lossy().as_ref(),
                ready_path.to_string_lossy().as_ref(),
                survived_path.to_string_lossy().as_ref(),
                release_path.to_string_lossy().as_ref(),
            ],
        );
        async move {
            ProcessRunner
                .run_with_cancellation(input, cancellation)
                .await
        }
    });
    wait_for_file(&root_pid_path).await;
    let root_pid = fs::read_to_string(&root_pid_path).expect("root PID");
    wait_until_process_is_reaped(root_pid.trim()).await;
    assert!(
        !task.is_finished(),
        "the descendant still holds the output pipes"
    );

    cancellation.cancel();

    let error = task
        .await
        .expect("join cancellation task")
        .expect_err("cancelled");
    assert!(error.is_cancelled());
    assert_cleanup_sentinels_remain_absent(&release_path, &[&survived_path]).await;
}

#[tokio::test(start_paused = true)]
async fn process_runner_times_out_and_cleans_up_children() {
    let temp = TempDir::new().expect("temporary directory");
    let script = write_process_tree_script(temp.path());
    let parent_ready_path = temp.path().join("parent-timeout.ready");
    let child_ready_path = temp.path().join("child-timeout.ready");
    let grandchild_ready_path = temp.path().join("grandchild-timeout.ready");
    let parent_survived_path = temp.path().join("parent-timeout.survived");
    let child_survived_path = temp.path().join("child-timeout.survived");
    let grandchild_survived_path = temp.path().join("grandchild-timeout.survived");
    let release_path = temp.path().join("timeout.release");

    let mut input = script_input(
        &script,
        &[
            parent_ready_path.to_string_lossy().as_ref(),
            child_ready_path.to_string_lossy().as_ref(),
            grandchild_ready_path.to_string_lossy().as_ref(),
            parent_survived_path.to_string_lossy().as_ref(),
            child_survived_path.to_string_lossy().as_ref(),
            grandchild_survived_path.to_string_lossy().as_ref(),
            release_path.to_string_lossy().as_ref(),
        ],
    );
    input.timeout = Duration::from_secs(5);

    let keep_time_paused = CancellationToken::new();
    let keepalive = tokio::spawn({
        let keep_time_paused = keep_time_paused.clone();
        async move {
            while !keep_time_paused.is_cancelled() {
                tokio::task::yield_now().await;
            }
        }
    });
    let task = tokio::spawn(async move { ProcessRunner.run(input).await });
    tokio::task::spawn_blocking(move || {
        wait_for_files_blocking(&[parent_ready_path, child_ready_path, grandchild_ready_path]);
    })
    .await
    .expect("join process-tree readiness observer");
    keep_time_paused.cancel();
    keepalive.await.expect("join paused-time keepalive");
    tokio::time::advance(Duration::from_secs(5)).await;
    tokio::time::resume();

    let error = task
        .await
        .expect("join timeout task")
        .expect_err("timed out");
    match error {
        ProcessError::Timeout { timeout_ms } => assert_eq!(timeout_ms, 5_000),
        other => panic!("expected timeout error, got {other:?}"),
    }
    assert_cleanup_sentinels_remain_absent(
        &release_path,
        &[
            &parent_survived_path,
            &child_survived_path,
            &grandchild_survived_path,
        ],
    )
    .await;
}

#[tokio::test]
async fn process_runner_cleans_up_tree_after_broken_stdin() {
    let temp = TempDir::new().expect("temporary directory");
    let parent_ready_path = temp.path().join("stdin-parent.ready");
    let child_ready_path = temp.path().join("stdin-child.ready");
    let grandchild_ready_path = temp.path().join("stdin-grandchild.ready");
    let parent_survived_path = temp.path().join("stdin-parent.survived");
    let child_survived_path = temp.path().join("stdin-child.survived");
    let grandchild_survived_path = temp.path().join("stdin-grandchild.survived");
    let release_path = temp.path().join("stdin.release");

    let mut input = broken_stdin_tree_input(
        temp.path(),
        &[
            parent_ready_path.to_string_lossy().as_ref(),
            child_ready_path.to_string_lossy().as_ref(),
            grandchild_ready_path.to_string_lossy().as_ref(),
            parent_survived_path.to_string_lossy().as_ref(),
            child_survived_path.to_string_lossy().as_ref(),
            grandchild_survived_path.to_string_lossy().as_ref(),
            release_path.to_string_lossy().as_ref(),
        ],
    );
    input.stdin = Some("x".repeat(8 * 1024 * 1024));

    let task = tokio::spawn(async move { ProcessRunner.run(input).await });
    wait_for_file(&parent_ready_path).await;
    wait_for_file(&child_ready_path).await;
    wait_for_file(&grandchild_ready_path).await;

    let error = task
        .await
        .expect("join broken-stdin task")
        .expect_err("closed stdin should fail");
    assert!(matches!(error, ProcessError::Stdin(_)));
    assert_cleanup_sentinels_remain_absent(
        &release_path,
        &[
            &parent_survived_path,
            &child_survived_path,
            &grandchild_survived_path,
        ],
    )
    .await;
}

#[cfg(windows)]
#[test]
#[allow(clippy::zombie_processes)]
fn process_runner_windows_broken_stdin_fixture() {
    if std::env::var_os(WINDOWS_BROKEN_STDIN_FIXTURE).is_none() {
        return;
    }
    // SAFETY: this reads the disposable fixture process's standard-input
    // handle without transferring ownership.
    let stdin = unsafe {
        windows_sys::Win32::System::Console::GetStdHandle(
            windows_sys::Win32::System::Console::STD_INPUT_HANDLE,
        )
    };
    assert!(!stdin.is_null(), "fixture stdin handle should exist");
    // SAFETY: the handle is valid for this fixture process. Clearing its
    // inheritance prevents the process-tree descendants from retaining extra
    // read ends that would mask the injected broken pipe.
    assert_ne!(
        unsafe {
            windows_sys::Win32::Foundation::SetHandleInformation(
                stdin,
                windows_sys::Win32::Foundation::HANDLE_FLAG_INHERIT,
                0,
            )
        },
        0,
        "fixture stdin handle should become non-inheritable"
    );
    let process_tree = PathBuf::from(
        std::env::var_os(WINDOWS_BROKEN_STDIN_TREE)
            .expect("Windows broken-stdin process-tree path"),
    );
    let arguments = (0..7)
        .map(|index| {
            std::env::var_os(format!("{WINDOWS_BROKEN_STDIN_ARGUMENT_PREFIX}{index}"))
                .unwrap_or_else(|| panic!("missing Windows broken-stdin argument {index}"))
        })
        .collect::<Vec<_>>();
    let mut command = std::process::Command::new(platform_interpreter());
    command
        .args(platform_script_prefix(&process_tree))
        .args(&arguments)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let _tree = command
        .spawn()
        .expect("spawn Windows broken-stdin process tree");
    for argument in arguments.iter().take(3) {
        let path = PathBuf::from(argument);
        for _ in 0..1_000 {
            if path.is_file() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(path.is_file(), "timed out waiting for {}", path.display());
    }

    // SAFETY: the fixture closes its standard-input handle exactly once.
    assert_ne!(
        unsafe { windows_sys::Win32::Foundation::CloseHandle(stdin) },
        0,
        "fixture stdin handle should close"
    );
    loop {
        std::thread::park();
    }
}

#[cfg(windows)]
#[tokio::test]
async fn process_runner_resolves_cmd_and_bat_scripts() {
    let temp = TempDir::new().expect("temporary directory");
    let cmd_script = temp.path().join("echo-script.cmd");
    fs::write(
        &cmd_script,
        "@echo off\r\necho cmd:%1:%2\r\necho cmd-stderr 1>&2\r\n",
    )
    .expect("write cmd script");

    let bat_script = temp.path().join("echo-script.bat");
    fs::write(
        &bat_script,
        "@echo off\r\necho bat:%1:%2\r\necho bat-stderr 1>&2\r\n",
    )
    .expect("write bat script");

    let cmd_output = ProcessRunner
        .run(ProcessRunInput::new(
            cmd_script.to_string_lossy().into_owned(),
            ["alpha", "beta"],
        ))
        .await
        .expect("cmd output");
    assert_eq!(cmd_output.stdout.trim(), "cmd:alpha:beta");
    assert_eq!(cmd_output.stderr.trim(), "cmd-stderr");

    let bat_output = ProcessRunner
        .run(ProcessRunInput::new(
            bat_script.to_string_lossy().into_owned(),
            ["left", "right"],
        ))
        .await
        .expect("bat output");
    assert_eq!(bat_output.stdout.trim(), "bat:left:right");
    assert_eq!(bat_output.stderr.trim(), "bat-stderr");
}

fn script_input(script: &Path, extra_args: &[&str]) -> ProcessRunInput {
    let mut args = platform_script_prefix(script);
    args.extend(extra_args.iter().map(|arg| (*arg).to_string()));
    ProcessRunInput::new(platform_interpreter().to_string_lossy().into_owned(), args)
}

#[cfg(windows)]
fn broken_stdin_tree_input(directory: &Path, extra_args: &[&str]) -> ProcessRunInput {
    let process_tree = write_process_tree_script(directory);
    let mut environment = std::env::vars().collect::<BTreeMap<_, _>>();
    environment.insert(WINDOWS_BROKEN_STDIN_FIXTURE.to_string(), "1".to_string());
    environment.insert(
        WINDOWS_BROKEN_STDIN_TREE.to_string(),
        process_tree.to_string_lossy().into_owned(),
    );
    for (index, argument) in extra_args.iter().enumerate() {
        environment.insert(
            format!("{WINDOWS_BROKEN_STDIN_ARGUMENT_PREFIX}{index}"),
            (*argument).to_string(),
        );
    }
    let mut input = ProcessRunInput::new(
        std::env::current_exe()
            .expect("current test executable")
            .to_string_lossy()
            .into_owned(),
        [
            "--exact",
            "process_runner_windows_broken_stdin_fixture",
            "--nocapture",
            "--test-threads=1",
        ],
    );
    input.env = Some(environment);
    input
}

#[cfg(unix)]
fn broken_stdin_tree_input(directory: &Path, extra_args: &[&str]) -> ProcessRunInput {
    let script = write_broken_stdin_tree_script(directory);
    script_input(&script, extra_args)
}

#[cfg(windows)]
fn environment_probe_input() -> ProcessRunInput {
    ProcessRunInput::new(
        Path::new(&env::var_os("SystemRoot").expect("SystemRoot"))
            .join("System32")
            .join("cmd.exe")
            .to_string_lossy()
            .into_owned(),
        ["/D", "/C", "set"],
    )
}

#[cfg(unix)]
fn environment_probe_input() -> ProcessRunInput {
    ProcessRunInput::new("/usr/bin/env", Vec::<String>::new())
}

#[cfg(windows)]
fn capture_script_input(script: &Path) -> ProcessRunInput {
    ProcessRunInput::new(script.to_string_lossy().into_owned(), Vec::<String>::new())
}

#[cfg(unix)]
fn capture_script_input(script: &Path) -> ProcessRunInput {
    script_input(script, &[])
}

fn platform_script_prefix(script: &Path) -> Vec<String> {
    #[cfg(windows)]
    {
        vec![
            "-NoProfile".to_string(),
            "-NonInteractive".to_string(),
            "-ExecutionPolicy".to_string(),
            "Bypass".to_string(),
            "-File".to_string(),
            script.to_string_lossy().into_owned(),
        ]
    }
    #[cfg(unix)]
    {
        vec![script.to_string_lossy().into_owned()]
    }
}

#[cfg(windows)]
fn platform_interpreter() -> PathBuf {
    Path::new(&env::var_os("SystemRoot").expect("SystemRoot"))
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe")
}

#[cfg(unix)]
fn platform_interpreter() -> PathBuf {
    PathBuf::from("/bin/sh")
}

#[cfg(windows)]
fn write_capture_script(directory: &Path) -> PathBuf {
    let path = directory.join("capture.cmd");
    fs::write(
        &path,
        r#"@echo off
setlocal EnableExtensions DisableDelayedExpansion
set /p STDIN_CONTENT=
echo cwd=%CD%
echo env=%PROCESS_RUNNER_TEST%
echo stdin=%STDIN_CONTENT%
<nul set /p =stderr-marker 1>&2
"#,
    )
    .expect("write capture script");
    path
}

#[cfg(unix)]
fn write_capture_script(directory: &Path) -> PathBuf {
    write_script(
        directory,
        "capture",
        "",
        r#"stdin_content=$(cat)
printf '{"cwd":"%s","env":"%s","stdin":"%s"}' "$PWD" "$PROCESS_RUNNER_TEST" "$stdin_content"
printf 'stderr-marker' >&2
"#,
    )
}

fn write_nonzero_script(directory: &Path) -> PathBuf {
    write_script(
        directory,
        "nonzero",
        r#"[Console]::Out.Write('stdout-nonzero')
[Console]::Error.Write('stderr-nonzero')
exit 7
"#,
        r#"printf 'stdout-nonzero'
printf 'stderr-nonzero' >&2
exit 7
"#,
    )
}

fn write_stderr_output_script(directory: &Path) -> PathBuf {
    write_script(
        directory,
        "stderr-output",
        r#"[Console]::Error.Write('yyyyyyy')
"#,
        r#"printf 'yyyyyyy' >&2
"#,
    )
}

#[cfg(windows)]
fn write_process_tree_script(directory: &Path) -> PathBuf {
    let windows = format!(
        r#"
param(
    [string]$parentReadyPath,
    [string]$childReadyPath,
    [string]$grandchildReadyPath,
    [string]$parentSurvivedPath,
    [string]$childSurvivedPath,
    [string]$grandchildSurvivedPath,
    [string]$releasePath
)
$env:CODDY_STUDIO_PROCESS_RUNNER_CHILD_READY = $childReadyPath
$env:CODDY_STUDIO_PROCESS_RUNNER_GRANDCHILD_READY = $grandchildReadyPath
$env:CODDY_STUDIO_PROCESS_RUNNER_CHILD_SURVIVED = $childSurvivedPath
$env:CODDY_STUDIO_PROCESS_RUNNER_GRANDCHILD_SURVIVED = $grandchildSurvivedPath
$env:CODDY_STUDIO_PROCESS_RUNNER_RELEASE = $releasePath
$grandchildCommand = 'Set-Content -LiteralPath $env:CODDY_STUDIO_PROCESS_RUNNER_GRANDCHILD_READY -Value ready -NoNewline; while (-not (Test-Path -LiteralPath $env:CODDY_STUDIO_PROCESS_RUNNER_RELEASE)) {{ Start-Sleep -Milliseconds 25 }}; Set-Content -LiteralPath $env:CODDY_STUDIO_PROCESS_RUNNER_GRANDCHILD_SURVIVED -Value survived -NoNewline; Start-Sleep -Seconds 30'
$childCommand = '$grandchild = Start-Process -FilePath ''{}'' -ArgumentList @(''-NoProfile'', ''-NonInteractive'', ''-Command'', $env:CODDY_STUDIO_PROCESS_RUNNER_GRANDCHILD_COMMAND) -PassThru -WindowStyle Hidden; Set-Content -LiteralPath $env:CODDY_STUDIO_PROCESS_RUNNER_CHILD_READY -Value ready -NoNewline; while (-not (Test-Path -LiteralPath $env:CODDY_STUDIO_PROCESS_RUNNER_RELEASE)) {{ Start-Sleep -Milliseconds 25 }}; Set-Content -LiteralPath $env:CODDY_STUDIO_PROCESS_RUNNER_CHILD_SURVIVED -Value survived -NoNewline; Start-Sleep -Seconds 30'
$env:CODDY_STUDIO_PROCESS_RUNNER_GRANDCHILD_COMMAND = $grandchildCommand
$child = Start-Process -FilePath '{}' -ArgumentList @('-NoProfile', '-NonInteractive', '-Command', $childCommand) -PassThru -WindowStyle Hidden
Set-Content -LiteralPath $parentReadyPath -Value ready -NoNewline
while (-not (Test-Path -LiteralPath $releasePath)) {{
    Start-Sleep -Milliseconds 25
}}
Set-Content -LiteralPath $parentSurvivedPath -Value survived -NoNewline
while ($true) {{
    Start-Sleep -Milliseconds 100
}}
"#,
        escape_powershell_single_quoted(platform_interpreter().to_string_lossy().as_ref()),
        escape_powershell_single_quoted(platform_interpreter().to_string_lossy().as_ref())
    );
    write_script(directory, "process-tree", &windows, "")
}

#[cfg(unix)]
fn write_process_tree_script(directory: &Path) -> PathBuf {
    let grandchild = write_script(
        directory,
        "process-tree-grandchild",
        "",
        r#"printf ready > "$1"
while [ ! -f "$3" ]; do
    sleep 0.05
done
printf survived > "$2"
sleep 30
"#,
    );
    let child = write_script(
        directory,
        "process-tree-child",
        "",
        &format!(
            r#"sh '{}' "$3" "$4" "$5" </dev/null &
printf ready > "$1"
while [ ! -f "$5" ]; do
    sleep 0.05
done
printf survived > "$2"
sleep 30
"#,
            grandchild.display()
        ),
    );
    let unix = format!(
        r#"parent_ready_path=$1
child_ready_path=$2
grandchild_ready_path=$3
parent_survived_path=$4
child_survived_path=$5
grandchild_survived_path=$6
release_path=$7
sh '{}' "$child_ready_path" "$child_survived_path" "$grandchild_ready_path" "$grandchild_survived_path" "$release_path" </dev/null &
printf ready > "$parent_ready_path"
while [ ! -f "$release_path" ]; do
    sleep 0.05
done
printf survived > "$parent_survived_path"
while true; do
    sleep 0.1
done
"#,
        child.display()
    );
    write_script(directory, "process-tree", "", &unix)
}

/// The root starts a descendant that inherits the output pipes, records its own
/// PID once the descendant is ready, and exits.
#[cfg(unix)]
fn write_outliving_descendant_script(directory: &Path) -> PathBuf {
    let unix = r#"root_pid_path=$1
ready_path=$2
survived_path=$3
release_path=$4
sh -c 'printf ready > "$1"
while [ ! -f "$3" ]; do
    sleep 0.05
done
printf survived > "$2"
sleep 30' descendant "$ready_path" "$survived_path" "$release_path" </dev/null &
while [ ! -f "$ready_path" ]; do
    sleep 0.05
done
printf '%s' "$$" > "$root_pid_path.tmp"
mv "$root_pid_path.tmp" "$root_pid_path"
"#;
    write_script(directory, "outliving-descendant", "", unix)
}

/// Waits until `pid` is reaped. `kill -0` still succeeds for a zombie.
#[cfg(unix)]
async fn wait_until_process_is_reaped(pid: &str) {
    for _ in 0..400 {
        let alive = std::process::Command::new("kill")
            .args(["-0", pid])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("probe the root process")
            .success();
        if !alive {
            return;
        }
        sleep(Duration::from_millis(25)).await;
    }
    panic!("the runner never reaped root process {pid}");
}

#[cfg(unix)]
fn write_broken_stdin_tree_script(directory: &Path) -> PathBuf {
    let process_tree = write_process_tree_script(directory);
    let unix = format!(
        r#"sh '{}' "$@" </dev/null &
tree_pid=$!
while [ ! -f "$1" ] || [ ! -f "$2" ] || [ ! -f "$3" ]; do
    sleep 0.01
done
exec 0<&-
wait "$tree_pid"
"#,
        process_tree.display()
    );
    write_script(directory, "broken-stdin-tree", "", &unix)
}

#[cfg(windows)]
fn write_script(directory: &Path, stem: &str, windows: &str, _unix: &str) -> PathBuf {
    let path = directory.join(format!("{stem}.ps1"));
    fs::write(&path, windows.trim_start()).expect("write script");
    path
}

#[cfg(unix)]
fn write_script(directory: &Path, stem: &str, _windows: &str, unix: &str) -> PathBuf {
    let path = directory.join(format!("{stem}.sh"));
    fs::write(&path, unix.trim_start()).expect("write script");
    path
}

#[derive(Debug)]
struct CaptureOutput {
    cwd: String,
    env: String,
    stdin: String,
}

#[cfg(windows)]
fn parse_capture_output(stdout: &str) -> CaptureOutput {
    let mut cwd = None;
    let mut env = None;
    let mut stdin = None;
    for line in stdout.lines() {
        if let Some(value) = line.strip_prefix("cwd=") {
            cwd = Some(value.to_string());
        } else if let Some(value) = line.strip_prefix("env=") {
            env = Some(value.to_string());
        } else if let Some(value) = line.strip_prefix("stdin=") {
            stdin = Some(value.to_string());
        }
    }
    CaptureOutput {
        cwd: cwd.expect("cwd line"),
        env: env.expect("env line"),
        stdin: stdin.expect("stdin line"),
    }
}

#[cfg(unix)]
fn parse_capture_output(stdout: &str) -> CaptureOutput {
    let value: Value = serde_json::from_str(stdout).expect("capture JSON output");
    CaptureOutput {
        cwd: value["cwd"].as_str().expect("cwd string").to_string(),
        env: value["env"].as_str().expect("env string").to_string(),
        stdin: value["stdin"].as_str().expect("stdin string").to_string(),
    }
}

#[cfg(windows)]
fn capture_stdin_payload() -> String {
    "stdin-visible\r\n".to_string()
}

#[cfg(unix)]
fn capture_stdin_payload() -> String {
    "stdin-visible".to_string()
}

async fn wait_for_file(path: &Path) {
    for _ in 0..160 {
        if path.is_file() {
            return;
        }
        sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for {}", path.display());
}

fn wait_for_files_blocking(paths: &[PathBuf]) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    for path in paths {
        while !path.is_file() {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {}",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

async fn assert_cleanup_sentinels_remain_absent(release: &Path, sentinels: &[&Path]) {
    fs::write(release, "release").expect("write survivor release file");
    sleep(Duration::from_secs(1)).await;
    for sentinel in sentinels {
        assert!(
            !sentinel.exists(),
            "process remained alive long enough to write {}",
            sentinel.display()
        );
    }
}

/// The paths `write_process_tree_script` takes, in its argument order. Each
/// process marks itself ready, and writes its `survived` sentinel only if it is
/// still alive once `release` appears.
struct ProcessTreePaths {
    ready: [PathBuf; 3],
    survived: [PathBuf; 3],
    release: PathBuf,
}

impl ProcessTreePaths {
    fn new(directory: &Path, label: &str) -> Self {
        let path = |name: &str| directory.join(format!("{label}-{name}"));
        Self {
            ready: [
                path("parent.ready"),
                path("child.ready"),
                path("grandchild.ready"),
            ],
            survived: [
                path("parent.survived"),
                path("child.survived"),
                path("grandchild.survived"),
            ],
            release: path("release"),
        }
    }

    fn input(&self, script: &Path) -> ProcessRunInput {
        let arguments = self
            .ready
            .iter()
            .chain(&self.survived)
            .chain([&self.release])
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        let arguments = arguments.iter().map(String::as_str).collect::<Vec<_>>();
        script_input(script, &arguments)
    }

    async fn wait_until_ready(&self) {
        for path in &self.ready {
            wait_for_file(path).await;
        }
    }

    async fn assert_stopped(&self) {
        let survived = self
            .survived
            .iter()
            .map(PathBuf::as_path)
            .collect::<Vec<_>>();
        assert_cleanup_sentinels_remain_absent(&self.release, &survived).await;
    }
}

fn parse_environment(stdout: &str) -> BTreeMap<String, String> {
    stdout
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| (name.to_string(), value.trim_end_matches('\r').to_string()))
        .collect()
}

#[cfg(windows)]
fn escape_powershell_single_quoted(value: &str) -> String {
    value.replace('\'', "''")
}

fn canonical_path(path: impl AsRef<Path>) -> PathBuf {
    fs::canonicalize(path).expect("canonical test path")
}
