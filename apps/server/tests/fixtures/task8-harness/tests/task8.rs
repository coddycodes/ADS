use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

use coddy_studio_task8_harness::{
    diagnostics::{
        AttributedProcessSnapshot, DiagnosticsMonitor, ProcessAttributionTotals, ProcessIdentity,
        ProcessRow, ResourceSampler, SamplingError, UiCoverage, build_descendant_entries,
    },
    process::{
        OutputMode, Platform, ProcessRunInput, ProcessRunner, TimeoutBehavior,
        resolve_shell_candidates,
    },
    terminal::{
        PortablePtyBackend, PtyBackend, PtyExit, PtyProcess, PtySpawnInput, SubprocessInspection,
        TerminalAttachInput, TerminalEvent, TerminalManager, TerminalManagerOptions,
        TerminalMetadataEvent, TerminalOpenInput, TerminalSubprocessInspector,
    },
};
use tokio::sync::{broadcast, watch};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn portable_pty_supports_input_output_resize_and_shutdown() {
    let (shell, args, command) = if cfg!(windows) {
        ("cmd.exe", Vec::new(), "echo CODDY_STUDIO_PTY_READY\r\n")
    } else {
        ("sh", Vec::new(), "echo CODDY_STUDIO_PTY_READY\n")
    };
    let input = PtySpawnInput {
        executable: shell.to_string(),
        args,
        cwd: std::env::current_dir().expect("cwd"),
        cols: 80,
        rows: 24,
        env: std::env::vars().collect(),
    };
    let backend = PortablePtyBackend;
    let process = backend.spawn(&input).expect("spawn portable PTY");
    let mut output = process.subscribe_output();
    let exit = process.subscribe_exit();
    process.resize(100, 32).expect("resize PTY");
    process.write(command).expect("write PTY command");

    let received = tokio::time::timeout(Duration::from_secs(10), async {
        let mut received = String::new();
        while !received.contains("CODDY_STUDIO_PTY_READY") {
            match output.recv().await {
                Ok(chunk) => received.push_str(&chunk),
                Err(error) => panic!(
                    "PTY output channel failed: {error}; exit={:?}; received={received:?}",
                    exit.borrow().clone()
                ),
            }
        }
        received
    })
    .await
    .unwrap_or_else(|error| {
        panic!(
            "PTY output timeout: {error}; pid={}; exit={:?}",
            process.pid(),
            exit.borrow().clone()
        )
    });
    assert!(received.contains("CODDY_STUDIO_PTY_READY"));
    process.kill().expect("kill PTY process tree");
}

#[test]
fn shell_resolution_is_deterministic_and_deduplicated() {
    let env = BTreeMap::from([("SHELL".to_string(), "/bin/fish -l".to_string())]);
    let candidates = resolve_shell_candidates(Platform::Unix, Some("/bin/fish -l"), &env);
    let commands = candidates
        .iter()
        .map(|candidate| candidate.command.as_str())
        .collect::<Vec<_>>();

    assert_eq!(commands[0], "/bin/fish");
    assert_eq!(
        commands.iter().filter(|item| **item == "/bin/fish").count(),
        1
    );
    assert!(commands.ends_with(&["zsh", "bash", "sh"]));
}

#[test]
fn windows_shell_resolution_prefers_powershell_then_absolute_fallbacks() {
    let env = BTreeMap::from([
        (
            "ComSpec".to_string(),
            "C:\\Windows\\System32\\cmd.exe".to_string(),
        ),
        ("SystemRoot".to_string(), "C:\\Windows".to_string()),
    ]);
    let candidates = resolve_shell_candidates(
        Platform::Windows,
        Some("C:\\missing\\custom-shell.exe"),
        &env,
    );
    let commands = candidates
        .iter()
        .map(|candidate| candidate.command.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        commands,
        [
            "C:\\missing\\custom-shell.exe",
            "pwsh.exe",
            "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
            "powershell.exe",
            "C:\\Windows\\System32\\cmd.exe",
            "cmd.exe",
        ]
    );
    for candidate in &candidates[1..=3] {
        assert_eq!(
            &candidate.args[..3],
            ["-NoLogo", "-NoExit", "-EncodedCommand"]
        );
        assert_eq!(candidate.args.len(), 4);
        assert!(!candidate.args[3].is_empty());
    }
}

#[tokio::test]
async fn process_runner_enforces_output_limit_and_truncation() {
    let runner = ProcessRunner;
    let strict = runner
        .run(ProcessRunInput::for_test_output(2_048).with_max_output_bytes(128))
        .await
        .expect_err("strict output mode must reject oversized output");
    assert_eq!(strict.output_limit(), Some(("stdout", 128)));

    let truncated = runner
        .run(
            ProcessRunInput::for_test_output(2_048)
                .with_max_output_bytes(128)
                .with_output_mode(OutputMode::Truncate),
        )
        .await
        .expect("truncate mode should complete");
    assert!(truncated.stdout.len() <= 128);
    assert!(truncated.stdout_truncated);
}

#[tokio::test]
async fn process_runner_times_out_and_observes_cancellation() {
    let runner = ProcessRunner;
    let timed_out = runner
        .run(
            ProcessRunInput::for_test_sleep(Duration::from_secs(30))
                .with_timeout(Duration::from_millis(40))
                .with_timeout_behavior(TimeoutBehavior::TimedOutResult),
        )
        .await
        .expect("synthetic timeout result should be returned");
    assert!(timed_out.timed_out);

    let cancellation = CancellationToken::new();
    let cancel_clone = cancellation.clone();
    let task = tokio::spawn(async move {
        ProcessRunner
            .run_with_cancellation(
                ProcessRunInput::for_test_sleep(Duration::from_secs(30)),
                cancel_clone,
            )
            .await
    });
    cancellation.cancel();
    let error = task
        .await
        .expect("runner task should join")
        .expect_err("cancelled command should fail");
    assert!(error.is_cancelled());
}

#[tokio::test]
async fn process_cancellation_removes_descendants() {
    use sysinfo::{Pid, ProcessesToUpdate, System};

    let directory = tempfile::tempdir().expect("temp directory");
    let pid_file = directory.path().join("child.pid");
    let input = if cfg!(windows) {
        let path = pid_file.to_string_lossy().replace('\'', "''");
        ProcessRunInput::new(
            "powershell.exe",
            [
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-Command".to_string(),
                format!(
                    "$p=Start-Process powershell.exe -ArgumentList '-NoProfile','-Command','Start-Sleep -Seconds 30' -PassThru; Set-Content -NoNewline -Path '{path}' -Value $p.Id; Start-Sleep -Seconds 30"
                ),
            ],
        )
    } else {
        ProcessRunInput::new(
            "sh",
            [
                "-c".to_string(),
                format!("sleep 30 & echo $! > '{}'; wait", pid_file.display()),
            ],
        )
    };
    let cancellation = CancellationToken::new();
    let task_cancellation = cancellation.clone();
    let task = tokio::spawn(async move {
        ProcessRunner
            .run_with_cancellation(input, task_cancellation)
            .await
    });
    for _ in 0..250 {
        if pid_file.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let child_pid = tokio::fs::read_to_string(&pid_file)
        .await
        .expect("child pid file")
        .trim()
        .parse::<u32>()
        .expect("child pid");
    cancellation.cancel();
    let error = task
        .await
        .expect("runner task join")
        .expect_err("runner should be cancelled");
    assert!(error.is_cancelled());

    let mut system = System::new_all();
    for _ in 0..100 {
        system.refresh_processes(ProcessesToUpdate::Some(&[Pid::from_u32(child_pid)]), true);
        if system.process(Pid::from_u32(child_pid)).is_none() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("descendant process {child_pid} survived cancellation");
}

#[derive(Debug)]
struct FakePty {
    pid: u32,
    identity: ProcessIdentity,
    writes: Mutex<Vec<String>>,
    sizes: Mutex<Vec<(u16, u16)>>,
    kill_count: Mutex<usize>,
    output: broadcast::Sender<String>,
    exit: watch::Sender<Option<PtyExit>>,
}

impl FakePty {
    fn emit(&self, output: &str) {
        let _ = self.output.send(output.to_string());
    }
}

impl PtyProcess for FakePty {
    fn pid(&self) -> u32 {
        self.pid
    }

    fn process_identity(&self) -> Option<ProcessIdentity> {
        Some(self.identity)
    }

    fn write(&self, data: &str) -> Result<(), String> {
        self.writes
            .lock()
            .expect("writes lock")
            .push(data.to_string());
        Ok(())
    }

    fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        self.sizes.lock().expect("sizes lock").push((cols, rows));
        Ok(())
    }

    fn kill(&self) -> Result<(), String> {
        *self.kill_count.lock().expect("kill lock") += 1;
        Ok(())
    }

    fn subscribe_output(&self) -> broadcast::Receiver<String> {
        self.output.subscribe()
    }

    fn subscribe_exit(&self) -> watch::Receiver<Option<PtyExit>> {
        self.exit.subscribe()
    }
}

#[derive(Debug, Default)]
struct FakeBackend {
    spawned: Mutex<Vec<Arc<FakePty>>>,
}

#[derive(Debug)]
struct FakeInspector {
    state: Mutex<SubprocessInspection>,
}

impl FakeInspector {
    fn new(state: SubprocessInspection) -> Self {
        Self {
            state: Mutex::new(state),
        }
    }

    fn set(&self, state: SubprocessInspection) {
        *self.state.lock().expect("inspector lock") = state;
    }
}

impl TerminalSubprocessInspector for FakeInspector {
    fn inspect(
        &self,
        _terminal_pid: u32,
    ) -> Pin<Box<dyn Future<Output = Result<SubprocessInspection, String>> + Send + '_>> {
        Box::pin(async move { Ok(self.state.lock().expect("inspector lock").clone()) })
    }
}

impl FakeBackend {
    fn latest(&self) -> Arc<FakePty> {
        self.spawned
            .lock()
            .expect("spawned lock")
            .last()
            .unwrap()
            .clone()
    }
}

impl PtyBackend for FakeBackend {
    fn spawn(&self, _input: &PtySpawnInput) -> Result<Arc<dyn PtyProcess>, String> {
        let (output, _) = broadcast::channel(32);
        let (exit, _) = watch::channel(None);
        let mut spawned = self.spawned.lock().expect("spawned lock");
        let generation = spawned.len() as u64 + 1;
        let pid = 4_242 + u32::try_from(generation - 1).expect("synthetic PID");
        let process = Arc::new(FakePty {
            pid,
            identity: ProcessIdentity {
                pid,
                started_at: generation,
            },
            writes: Mutex::new(Vec::new()),
            sizes: Mutex::new(Vec::new()),
            kill_count: Mutex::new(0),
            output,
            exit,
        });
        spawned.push(process.clone());
        Ok(process)
    }
}

#[tokio::test]
async fn terminal_attach_reconnect_resize_input_metadata_and_cleanup() {
    let backend = Arc::new(FakeBackend::default());
    let manager = TerminalManager::new(
        backend.clone(),
        TerminalManagerOptions {
            history_line_limit: 100,
            ..TerminalManagerOptions::default()
        },
    );
    let cwd = std::env::current_dir().expect("cwd");
    let snapshot = manager
        .open(TerminalOpenInput::new(
            "thread-1",
            "term-1",
            cwd.clone(),
            80,
            24,
        ))
        .await
        .expect("open terminal");
    assert_eq!(snapshot.status.as_str(), "running");

    let metadata = manager.subscribe_metadata().await;
    assert_eq!(metadata.initial.len(), 1);

    let process = backend.latest();
    process.emit("hello\r\n");
    let mut first = manager
        .attach(TerminalAttachInput::existing("thread-1", "term-1"))
        .await
        .expect("attach terminal");
    assert!(first.initial.history.contains("hello"));

    manager
        .write("thread-1", "term-1", "pwd\r")
        .await
        .expect("write");
    manager
        .resize("thread-1", "term-1", 120, 40)
        .await
        .expect("resize");
    assert_eq!(
        process.writes.lock().expect("writes lock").as_slice(),
        ["pwd\r"]
    );
    assert_eq!(
        process.sizes.lock().expect("sizes lock").as_slice(),
        [(120, 40)]
    );

    process.emit("later\r\n");
    let event = tokio::time::timeout(Duration::from_secs(1), first.recv())
        .await
        .expect("live event timeout")
        .expect("live event");
    assert!(matches!(event, TerminalEvent::Output { data, .. } if data.contains("later")));

    let second = manager
        .attach(TerminalAttachInput::existing("thread-1", "term-1"))
        .await
        .expect("reconnect terminal");
    assert!(second.initial.history.contains("later"));

    manager.shutdown().await;
    assert_eq!(*process.kill_count.lock().expect("kill lock"), 1);
}

#[tokio::test]
async fn terminal_attach_restart_if_not_running_uses_fresh_session_snapshot() {
    let backend = Arc::new(FakeBackend::default());
    let manager = TerminalManager::new(backend.clone(), TerminalManagerOptions::default());
    let cwd = std::env::current_dir().expect("cwd");
    manager
        .open(TerminalOpenInput::new(
            "thread-1",
            "term-1",
            cwd.clone(),
            80,
            24,
        ))
        .await
        .expect("open terminal");

    let first = backend.latest();
    let _ = first.exit.send(Some(PtyExit {
        exit_code: Some(0),
        signal: Some(0),
    }));
    tokio::time::sleep(Duration::from_millis(20)).await;

    let attachment = manager
        .attach(TerminalAttachInput {
            size_claim: None,
            thread_id: "thread-1".to_string(),
            terminal_id: "term-1".to_string(),
            cwd: Some(cwd),
            worktree_path: None,
            cols: Some(80),
            rows: Some(24),
            env: BTreeMap::new(),
            restart_if_not_running: true,
            command: None,
        })
        .await
        .expect("attach should restart stopped terminal");

    assert_eq!(backend.spawned.lock().expect("spawned lock").len(), 2);
    assert_eq!(attachment.initial.status.as_str(), "running");
    assert_eq!(attachment.initial.pid, Some(4_243));
}

#[tokio::test]
async fn terminal_activity_events_update_metadata_and_labels() {
    let backend = Arc::new(FakeBackend::default());
    let inspector = Arc::new(FakeInspector::new(SubprocessInspection::default()));
    let manager = TerminalManager::new(
        backend,
        TerminalManagerOptions {
            subprocess_poll_interval: Duration::from_millis(20),
            subprocess_inspector: Some(inspector.clone()),
            ..TerminalManagerOptions::default()
        },
    );
    let cwd = std::env::current_dir().expect("cwd");
    let mut events = manager.subscribe_events();
    manager
        .open(TerminalOpenInput::new("thread-1", "term-1", cwd, 80, 24))
        .await
        .expect("open terminal");
    let mut metadata = manager.subscribe_metadata().await;

    inspector.set(SubprocessInspection {
        has_running_subprocess: true,
        child_command_label: Some("vim".to_string()),
        process_ids: vec![4_242, 9_001],
    });

    let active = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match events.recv().await {
                Ok(TerminalEvent::Activity {
                    has_running_subprocess,
                    label,
                    ..
                }) if has_running_subprocess && label == "vim" => return,
                Ok(_) => continue,
                Err(error) => panic!("activity stream closed unexpectedly: {error}"),
            }
        }
    })
    .await;
    assert!(active.is_ok(), "expected running activity event");

    let active_metadata = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match metadata.recv().await {
                Some(TerminalMetadataEvent::Upsert { terminal })
                    if terminal.has_running_subprocess && terminal.label == "vim" =>
                {
                    return terminal;
                }
                Some(_) => continue,
                None => panic!("metadata stream closed unexpectedly"),
            }
        }
    })
    .await
    .expect("active metadata timeout");
    assert!(active_metadata.has_running_subprocess);

    inspector.set(SubprocessInspection::default());
    let idle = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            match events.recv().await {
                Ok(TerminalEvent::Activity {
                    has_running_subprocess,
                    label,
                    ..
                }) if !has_running_subprocess && label == "Terminal 1" => return,
                Ok(_) => continue,
                Err(error) => panic!("activity stream closed unexpectedly: {error}"),
            }
        }
    })
    .await;
    assert!(idle.is_ok(), "expected idle activity event");
}

#[test]
fn descendant_graph_is_depth_first_and_excludes_unrelated_processes() {
    let rows = vec![
        ProcessRow::fixture(10, 0, "server"),
        ProcessRow::fixture(11, 10, "provider"),
        ProcessRow::fixture(12, 11, "tool"),
        ProcessRow::fixture(99, 1, "unrelated"),
    ];
    let descendants = build_descendant_entries(&rows, 10);
    assert_eq!(
        descendants.iter().map(|row| row.pid).collect::<Vec<_>>(),
        [11, 12]
    );
    assert_eq!(descendants[1].depth, 1);
}

#[derive(Debug, Default)]
struct CountingSampler {
    calls: Mutex<usize>,
}

impl ResourceSampler for CountingSampler {
    fn sample(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<AttributedProcessSnapshot, SamplingError>> + Send + '_>>
    {
        Box::pin(async move {
            *self.calls.lock().expect("calls lock") += 1;
            Ok(AttributedProcessSnapshot {
                sampled_at_ms: 1_000,
                server_identity: ProcessIdentity {
                    pid: std::process::id(),
                    started_at: 1,
                },
                native_rows: Arc::from([]),
                processes: Vec::new(),
                totals: ProcessAttributionTotals::default(),
                ui_coverage: UiCoverage::default(),
            })
        })
    }
}

#[tokio::test(start_paused = true)]
async fn diagnostics_sampling_is_on_demand_and_has_no_background_timer() {
    let sampler = Arc::new(CountingSampler::default());
    let monitor = DiagnosticsMonitor::new(sampler.clone(), Duration::from_secs(5));
    tokio::time::advance(Duration::from_secs(20)).await;
    assert_eq!(*sampler.calls.lock().expect("calls lock"), 0);

    let current = monitor.sample_current().await;
    assert!(current.snapshot.is_some());
    assert_eq!(*sampler.calls.lock().expect("calls lock"), 1);
    let history = monitor.read_history(60_000, 1_000).await;
    assert_eq!(history.retained_sample_count, 1);
    assert_eq!(*sampler.calls.lock().expect("calls lock"), 1);

    tokio::time::advance(Duration::from_secs(5)).await;
    tokio::task::yield_now().await;
    assert_eq!(*sampler.calls.lock().expect("calls lock"), 1);
}
