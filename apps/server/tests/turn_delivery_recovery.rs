#![allow(clippy::await_holding_lock)]
// These subprocess recovery tests intentionally hold a process-wide serialization guard
// across their async lifecycle so environment variables and child processes cannot overlap.

#[path = "support/hermetic_providers.rs"]
mod hermetic_providers;

#[path = "support/child_command.rs"]
mod child_command;
use child_command::ChildCommandSpec;

#[cfg(unix)]
#[path = "support/child_watchdog.rs"]
mod child_watchdog;

#[cfg(unix)]
#[path = "support/child_watchdog_tests.rs"]
mod watchdog_tests;

#[cfg(target_os = "linux")]
#[path = "support/reexec.rs"]
mod reexec;

use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

#[cfg(not(unix))]
use std::io::Read;

use bibcode_server::{
    RpcExit, RpcRegistry, RpcResult, ServerConfig, ServerMessage, ServerRuntime,
    activity::{ActivityProjection, ActivityRepository},
    cloud, diagnostics,
    git::GitRepository,
    orchestration::{EngineOptions, OrchestrationCommand, OrchestrationEngine, TurnDeliveryState},
    persistence::{Database, run_migrations},
    production::{
        orchestration_effects::{
            BoxEffectFuture, EffectsOptions, OrchestrationEffectCallbacks, OrchestrationEffects,
            ProviderRewind, SetupScriptLaunch,
        },
        orchestration_rpc::register_orchestration_rpc_with_delivery,
        provider_runtime::{
            BoxRuntimeFuture, ProviderDeliveryOutcome, ProviderDriver, ProviderDriverFactory,
            ProviderLaunchRequest, ProviderReconciliationOutcome, ProviderRuntimeError,
            ProviderRuntimeSupervisor, StartedSession, SupervisorOptions, freeze_delivery_route,
            reconcile_abandoned_provider_sessions,
        },
        server_terminal::{
            JsonFuture, JsonStream, ProductionServerControl, ServerTerminalServices,
        },
        turn_delivery::TurnDeliveryService,
    },
    provider::codex::resolve_codex_home_layout,
    provider_usage,
    terminal::{PortablePtyBackend, TerminalManager, TerminalManagerOptions},
};
use futures_util::SinkExt;
use serde_json::Value;
use tempfile::TempDir;
use tokio::sync::{Notify, mpsc};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tokio_util::sync::CancellationToken;

#[path = "support/websocket_frames.rs"]
mod websocket_frames;
use websocket_frames::next_frame_past_heartbeat;

const ATTACHMENT_ABORT_CHILD_STATE: &str = "BIBCODE_TURN_DELIVERY_ATTACHMENT_ABORT_CHILD_STATE";
const ATTACHMENT_ABORT_CHILD_READY: &str = "BIBCODE_TURN_DELIVERY_ATTACHMENT_ABORT_CHILD_READY";
const MISSING_ORIGIN_CHILD_TRACE: &str = "BIBCODE_TURN_DELIVERY_MISSING_ORIGIN_TRACE";
const CRASH_BOUNDARY_CHILD_STATE: &str = "BIBCODE_TURN_DELIVERY_CRASH_STATE";
const CRASH_BOUNDARY_CHILD_PROVIDER: &str = "BIBCODE_TURN_DELIVERY_CRASH_PROVIDER";
const CRASH_BOUNDARY_CHILD_MODE: &str = "BIBCODE_TURN_DELIVERY_CRASH_MODE";
const CRASH_BOUNDARY_CHILD_SENDS: &str = "BIBCODE_TURN_DELIVERY_CRASH_SENDS";
const BOOTSTRAP_RECOVERY_INTEGRATION_DEADLINE: Duration = Duration::from_secs(30);
const CHILD_PROCESS_DEADLINE: Duration = Duration::from_secs(30);
#[cfg(target_os = "linux")]
const DETACHED_PIPE_HOLDER_PID: &str = "BIBCODE_TURN_DELIVERY_DETACHED_PIPE_HOLDER_PID";

fn child_process_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn child_process_guard() -> std::sync::MutexGuard<'static, ()> {
    child_process_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn run_child_with_deadline(
    spec: &mut ChildCommandSpec,
    context: &str,
    timeout: Duration,
) -> Output {
    #[cfg(unix)]
    {
        child_watchdog::run(spec, context, timeout)
    }
    #[cfg(not(unix))]
    {
        run_direct_child_with_deadline(spec, context, timeout)
    }
}

#[cfg(not(unix))]
fn run_direct_child_with_deadline(
    spec: &ChildCommandSpec,
    context: &str,
    timeout: Duration,
) -> Output {
    let mut command = spec.command();
    fn drain(pipe: impl Read + Send + 'static) -> std::sync::mpsc::Receiver<Vec<u8>> {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut pipe = pipe;
            let mut bytes = Vec::new();
            pipe.read_to_end(&mut bytes).expect("drain child output");
            drop(pipe);
            let _ = sender.send(bytes);
        });
        receiver
    }

    let started = Instant::now();
    let deadline = started + timeout;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("spawn {context} child: {error}"));
    let pid = child.id();
    let stdout = drain(child.stdout.take().expect("piped child stdout"));
    let stderr = drain(child.stderr.take().expect("piped child stderr"));
    let (status, timed_out) = loop {
        if let Some(status) = child.try_wait().expect("poll child exit") {
            break (status, false);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            child.kill().expect("kill timed-out child");
            break (child.wait().expect("reap timed-out child"), true);
        }
        std::thread::sleep(remaining.min(Duration::from_millis(10)));
    };
    // Both readers share one cleanup budget. A descendant that escaped the
    // process group must not turn timeout diagnostics into an unbounded wait.
    let drain_deadline = timed_out.then(|| Instant::now() + Duration::from_secs(1));
    let collect = |reader: std::sync::mpsc::Receiver<Vec<u8>>| match drain_deadline {
        Some(deadline) => {
            match reader.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(bytes) => bytes,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    b"output unavailable: a descendant still holds the pipe".to_vec()
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    b"output unavailable: child output reader failed".to_vec()
                }
            }
        }
        None => reader.recv().expect("read complete child output"),
    };
    let output = Output {
        status,
        stdout: collect(stdout),
        stderr: collect(stderr),
    };
    let elapsed = started.elapsed();
    assert!(
        !timed_out,
        "{context} child (pid={pid}) timed out after {elapsed:?} (deadline {timeout:?})\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    eprintln!("{context} child finished after {elapsed:?}");
    output
}

#[cfg(unix)]
#[test]
fn child_deadline_kills_and_reaps_a_stalled_child_and_reports_timeout() {
    let _guard = child_process_guard();
    #[cfg(target_os = "linux")]
    let phase = {
        const TEST: &str = "child_deadline_kills_and_reaps_a_stalled_child_and_reports_timeout";
        let Some(phase) = reexec::enter(TEST, "subreaper") else {
            reexec::run(TEST, "subreaper", None, |_| {});
            return;
        };
        // SAFETY: only this isolated test process adopts the orphaned descendant,
        // so the assertion does not depend on the host's init reaping promptly.
        assert_eq!(unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) }, 0);
        phase
    };
    let started = Instant::now();
    let result = std::panic::catch_unwind(|| {
        run_child_with_deadline(
            ChildCommandSpec::new("sh").args([
                "-c",
                "sleep 30 & printf 'requested-child=%s\\ndescendant=%s\\n' \"$$\" \"$!\"; printf 'stalled child\\n' >&2; wait",
            ]),
            "sleep-timeout-fixture",
            Duration::from_millis(100),
        )
    });
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the helper must report the timeout before the descendant exits naturally"
    );
    let panic = result.expect_err("a stalled child must time out instead of returning output");
    let report = panic.downcast_ref::<String>().expect("timeout diagnostic");
    assert!(report.contains("sleep-timeout-fixture"), "{report}");
    assert!(report.contains("timed out after"), "{report}");
    assert!(report.contains("stdout:\n"), "{report}");
    assert!(report.contains("stderr:\n"), "{report}");
    assert!(report.contains("stalled child\n"), "{report}");
    let descendant = report
        .split_once("descendant=")
        .and_then(|(_, rest)| rest.lines().next())
        .expect("timeout includes descendant PID")
        .parse::<libc::pid_t>()
        .expect("numeric descendant PID");
    let requested_child = report
        .split_once("requested-child=")
        .and_then(|(_, rest)| rest.lines().next())
        .expect("timeout includes requested child PID")
        .parse::<libc::pid_t>()
        .expect("numeric requested child PID");
    loop {
        #[cfg(target_os = "linux")]
        {
            let mut status = 0;
            // SAFETY: only reap the fixture descendant adopted by this subreaper;
            // WNOHANG exposes a surviving descendant instead of waiting for it.
            for owned in [requested_child, descendant] {
                let waited = unsafe { libc::waitpid(owned, &mut status, libc::WNOHANG) };
                if waited == owned {
                    assert!(libc::WIFSIGNALED(status));
                    assert_eq!(libc::WTERMSIG(status), libc::SIGKILL);
                } else {
                    assert!(
                        waited == 0
                            || std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD),
                        "wait for the owned fixture process"
                    );
                }
            }
        }
        // SAFETY: signal zero only checks the fixture descendant's existence.
        let descendant_absent = unsafe { libc::kill(descendant, 0) } == -1;
        if descendant_absent {
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
        }
        // SAFETY: the real child also belongs to this exact fixture.
        let child_absent = unsafe { libc::kill(requested_child, 0) } == -1;
        if child_absent {
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
        }
        if descendant_absent && child_absent {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "descendant still exists after the child timed out"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    // SAFETY: only observe the real requested child published in its stdout.
    assert_eq!(unsafe { libc::kill(requested_child, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    let pid = report
        .split_once("(pid=")
        .and_then(|(_, rest)| rest.split_once(')'))
        .expect("timeout includes child PID")
        .0
        .parse::<libc::pid_t>()
        .expect("numeric child PID");
    // SAFETY: signal zero only checks this test child's existence.
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "child still exists");
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    let mut status = 0;
    // SAFETY: status is writable and WNOHANG cannot block if the helper failed to reap.
    assert_eq!(
        unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
    #[cfg(target_os = "linux")]
    phase.complete();
}

#[cfg(target_os = "linux")]
#[test]
fn detached_pipe_holder_child() {
    let Some(pid_path) = std::env::var_os(DETACHED_PIPE_HOLDER_PID) else {
        return;
    };
    // SAFETY: this fixture runs only in an isolated descendant and deliberately
    // escapes the shell's process group while retaining both output pipes.
    assert!(unsafe { libc::setsid() } > 0);
    std::fs::write(pid_path, std::process::id().to_string()).expect("detached descendant PID");
    std::thread::sleep(Duration::from_secs(3));
}

#[cfg(target_os = "linux")]
#[test]
fn child_deadline_bounds_output_collection_when_a_descendant_escapes() {
    let _guard = child_process_guard();
    const TEST: &str = "child_deadline_bounds_output_collection_when_a_descendant_escapes";
    let Some(phase) = reexec::enter(TEST, "subreaper") else {
        reexec::run(TEST, "subreaper", None, |_| {});
        return;
    };
    // SAFETY: only this isolated test process adopts and reaps the fixture.
    assert_eq!(unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) }, 0);
    let state = TempDir::new().expect("detached descendant state");
    let pid_path = state.path().join("pid");
    let started = Instant::now();
    let result = std::panic::catch_unwind(|| {
        run_child_with_deadline(
            ChildCommandSpec::new("sh")
                .args([
                    "-c",
                    "\"$1\" --exact detached_pipe_holder_child --nocapture --test-threads=1 & wait",
                    "detached-pipe-holder",
                ])
                .arg(std::env::current_exe().expect("test executable"))
                .env(DETACHED_PIPE_HOLDER_PID, &pid_path),
            "escaped-descendant-fixture",
            Duration::from_millis(250),
        )
    });
    let elapsed = started.elapsed();
    let descendant = std::fs::read_to_string(pid_path)
        .expect("descendant escaped before the timeout")
        .parse::<libc::pid_t>()
        .expect("numeric descendant PID");
    // SAFETY: clean up only the known fixture process that escaped the group.
    assert_eq!(unsafe { libc::kill(descendant, libc::SIGKILL) }, 0);
    let reap_deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let mut status = 0;
        // SAFETY: status is writable and this subreaper owns the descendant.
        let waited = unsafe { libc::waitpid(descendant, &mut status, libc::WNOHANG) };
        if waited == descendant {
            break;
        }
        assert_eq!(waited, 0, "wait for escaped descendant");
        assert!(Instant::now() < reap_deadline, "reap escaped descendant");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(elapsed < Duration::from_secs(2), "timeout took {elapsed:?}");
    let panic = result.expect_err("the shell must time out");
    let report = panic.downcast_ref::<String>().expect("timeout diagnostic");
    assert!(report.contains("escaped-descendant-fixture"), "{report}");
    assert!(report.contains("timed out after"), "{report}");
    for stream in ["stdout", "stderr"] {
        assert!(report.contains(&format!("{stream}:\n")), "{report}");
    }
    assert!(
        report.contains("output incomplete: monitor stopped at the absolute deadline"),
        "{report}"
    );
    phase.complete();
}

#[cfg(unix)]
#[test]
fn child_deadline_returns_complete_output_on_normal_exit() {
    let _guard = child_process_guard();
    let output = run_child_with_deadline(
        ChildCommandSpec::new("sh").args([
            "-c",
            "i=0; while [ \"$i\" -lt 10000 ]; do printf 'stdout line\\n'; printf 'stderr line\\n' >&2; i=$((i + 1)); done; exit 7",
        ]),
        "complete-output-fixture",
        Duration::from_secs(5),
    );
    assert_eq!(output.status.code(), Some(7));
    assert_eq!(output.stdout, b"stdout line\n".repeat(10000));
    assert_eq!(output.stderr, b"stderr line\n".repeat(10000));
}

fn attachment_upload_stages_absent(attachments_dir: &Path) -> bool {
    match std::fs::read_dir(attachments_dir) {
        Ok(entries) => entries
            .map(|entry| entry.expect("attachment directory entry"))
            .all(|entry| !entry.file_name().to_string_lossy().ends_with(".upload")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => panic!("read attachment directory: {error}"),
    }
}

struct DatabasePhaseGate {
    entered: tokio::sync::oneshot::Receiver<()>,
    release: std::sync::mpsc::Sender<()>,
}

fn queue_database_phase_gate(database: Database) -> DatabasePhaseGate {
    let (entered_tx, entered) = tokio::sync::oneshot::channel();
    let (release, release_rx) = std::sync::mpsc::channel();
    tokio::spawn(async move {
        let _ = database
            .call(move |_connection| {
                let _ = entered_tx.send(());
                release_rx.recv().expect("database phase gate release");
                Ok(())
            })
            .await;
    });
    DatabasePhaseGate { entered, release }
}

async fn await_database_phase_gate(gate: &mut DatabasePhaseGate, context: &str) {
    tokio::time::timeout(Duration::from_secs(5), &mut gate.entered)
        .await
        .unwrap_or_else(|_| panic!("{context} database phase gate enters"))
        .unwrap_or_else(|_| panic!("{context} database phase gate signal"));
}

async fn advance_one_database_call(
    database: &Database,
    current: DatabasePhaseGate,
    context: &str,
) -> DatabasePhaseGate {
    tokio::time::timeout(Duration::from_secs(5), async {
        while database
            .queue_backpressure_snapshot_for_integration_test()
            .reserved_or_queued_jobs
            < 1
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{context} database call queues behind phase gate"));
    let mut next = queue_database_phase_gate(database.clone());
    current
        .release
        .send(())
        .unwrap_or_else(|_| panic!("release {context} database phase gate"));
    await_database_phase_gate(&mut next, context).await;
    next
}

#[tokio::test]
async fn attachment_abort_child() {
    let Some(state) = std::env::var_os(ATTACHMENT_ABORT_CHILD_STATE).map(PathBuf::from) else {
        return;
    };
    let attachment_config = ServerConfig::new(&state);
    let state_dir = attachment_config.state_dir();
    let database_path = attachment_config.database_path();
    let attachments_dir = state_dir.join("attachments");
    let final_path = attachments_dir.join("aborted-final");
    std::fs::create_dir_all(&state_dir).expect("attachment crash state directory");
    let database = Database::create_new(database_path.clone())
        .await
        .expect("attachment crash database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("attachment crash migrations");
    let engine = OrchestrationEngine::start(database.clone(), EngineOptions::default())
        .await
        .expect("attachment crash engine");
    seed_crash_delivery(&engine, &state, "codex").await;
    let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        Arc::new(NeverProvider::default()),
        ActivityProjection::new(ActivityRepository::new(database.clone())),
        SupervisorOptions::default(),
    ));
    let delivery = Arc::new(TurnDeliveryService::start(
        engine.clone(),
        supervisor.clone(),
        state_dir.clone(),
    ));
    delivery.shutdown().await;
    let mut registry = RpcRegistry::empty();
    register_orchestration_rpc_with_delivery(
        &mut registry,
        engine,
        supervisor,
        state_dir,
        delivery,
        attachment_upload_registry(),
    );
    let rpc_config = ServerConfig::new(state.join("rpc-runtime"))
        .with_bind("127.0.0.1", 0)
        .with_unsafe_no_auth();
    let runtime = ServerRuntime::start_with_registry(rpc_config, registry)
        .await
        .expect("attachment crash RPC runtime");
    let (mut socket, _) = connect_async(format!("ws://{}/ws", runtime.local_addr()))
        .await
        .expect("attachment crash RPC websocket");

    // Step the single SQLite worker through the receipt preflight and the three durable-reservation
    // calls. The final gate then blocks the post-publication identity read, proving the production
    // materializer publishes only after the command identity is durably owned and before the
    // command/outbox transaction commits.
    let observer = database
        .enable_queue_backpressure_observation_for_integration_test()
        .expect("attachment crash queue observer");
    let mut phase = queue_database_phase_gate(database.clone());
    await_database_phase_gate(&mut phase, "initial").await;
    socket
        .send(Message::Text(
            serde_json::json!({
                "_tag":"Request", "id":"1",
                "tag":"orchestration.dispatchCommand",
                "payload":{
                    "type":"thread.turn.start", "commandId":"attachment-crash-turn",
                    "threadId":"crash-thread",
                    "message":{
                        "messageId":"attachment-crash-message", "role":"user", "text":"notes",
                        "attachments":[{
                            "type":"file", "id":"aborted-final", "name":"notes.txt",
                            "mimeType":"text/plain", "sizeBytes":5,
                            "dataUrl":"data:text/plain;base64,bm90ZXM="
                        }]
                    },
                    "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                    "runtimeMode":"full-access", "interactionMode":"default",
                    "createdAt":"2026-08-01T00:00:01Z"
                },
                "headers":[]
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("attachment crash RPC request");
    phase = advance_one_database_call(&database, phase, "receipt preflight").await;
    phase = advance_one_database_call(&database, phase, "reservation timestamp").await;
    phase = advance_one_database_call(&database, phase, "reservation sequence").await;
    let _post_reservation_gate =
        advance_one_database_call(&database, phase, "durable receipt reservation").await;

    let receipt_status = rusqlite::Connection::open(database_path)
        .expect("reservation observer connection")
        .query_row(
            "SELECT status FROM orchestration_command_receipts WHERE command_id = ?",
            ["attachment-crash-turn"],
            |row| row.get::<_, String>(0),
        )
        .expect("durable attachment command reservation");
    assert_eq!(receipt_status, "reserved");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let post_publication_read_is_blocked = database
                .queue_backpressure_snapshot_for_integration_test()
                .reserved_or_queued_jobs
                >= 1;
            if final_path.exists()
                && attachment_upload_stages_absent(&attachments_dir)
                && post_publication_read_is_blocked
            {
                break;
            }
            tokio::select! {
                frame = next_frame_past_heartbeat(&mut socket) => {
                    panic!("attachment RPC completed before publication: {frame:?}");
                }
                _ = tokio::time::sleep(Duration::from_millis(5)) => {}
            }
        }
    })
    .await
    .expect("production materializer atomically publishes after reservation and before DB commit");
    assert!(
        database
            .queue_backpressure_snapshot_for_integration_test()
            .reserved_or_queued_jobs
            >= 1,
        "the post-publication admission read remains queued behind the database phase gate"
    );
    drop(observer);
    assert_eq!(
        std::fs::read(&final_path).expect("published attachment bytes"),
        b"notes"
    );
    assert!(
        attachment_upload_stages_absent(&attachments_dir),
        "production publication removes its stage before the crash boundary"
    );
    std::fs::write(
        std::env::var_os(ATTACHMENT_ABORT_CHILD_READY).expect("ready marker"),
        "production-materializer-published-after-reservation-before-db-commit",
    )
    .expect("ready marker write");
    std::process::abort();
}

#[tokio::test]
async fn attachment_startup_recovery_removes_finals_left_by_an_aborted_process() {
    let _process_guard = child_process_guard();
    let state = TempDir::new().expect("state directory");
    let config = ServerConfig::new(state.path()).with_bind("127.0.0.1", 0);
    hermetic_providers::write_hermetic_settings(&config.state_dir(), serde_json::json!({}));
    let attachments_dir = config.state_dir().join("attachments");
    let ready = state.path().join("published");
    let output = run_child_with_deadline(
        ChildCommandSpec::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "attachment_abort_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(ATTACHMENT_ABORT_CHILD_STATE, state.path())
            .env(ATTACHMENT_ABORT_CHILD_READY, &ready),
        "attachment-abort",
        CHILD_PROCESS_DEADLINE,
    );
    assert!(
        ready.exists(),
        "the child published its final\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.status.success(), "the child must abort");
    assert!(attachments_dir.join("aborted-final").exists());
    assert!(
        attachment_upload_stages_absent(&attachments_dir),
        "the production materializer removes its stage after final publication"
    );

    let database_path = config.database_path();
    let runtime = ServerRuntime::start(config)
        .await
        .expect("restarted runtime");

    assert!(!attachments_dir.join("aborted-final").exists());
    assert!(
        attachment_upload_stages_absent(&attachments_dir),
        "startup removes abandoned stages too"
    );
    runtime.shutdown();
    runtime.join().await.expect("runtime shutdown");
    let connection = rusqlite::Connection::open(database_path).expect("recovered database");
    let outbox_rows: i64 = connection
        .query_row("SELECT COUNT(*) FROM provider_turn_outbox", [], |row| {
            row.get(0)
        })
        .expect("outbox count");
    assert_eq!(
        outbox_rows, 0,
        "a published attachment without the DB commit cannot synthesize a delivery"
    );
}

fn crash_model(provider: &str) -> &'static str {
    match provider {
        "claudeAgent" => "claude-sonnet-4-5",
        "opencode" => "openai/gpt-5",
        "cursor" => "cursor-default",
        _ => "gpt-5",
    }
}

fn crash_binary_path(provider: &str, state: &Path) -> String {
    match provider {
        "cursor" => hermetic_providers::missing_provider_executable(state)
            .to_str()
            .expect("crash fixture path is UTF-8")
            .to_owned(),
        "claudeAgent" => "claude".to_owned(),
        other => other.to_owned(),
    }
}

fn crash_launch(provider: &str, state: &Path) -> ProviderLaunchRequest {
    ProviderLaunchRequest {
        thread_id: "crash-thread".to_owned(),
        activity_causal_revision: 0,
        provider: provider.to_owned(),
        provider_label: provider.to_owned(),
        provider_instance_id: Some(provider.to_owned()),
        binary_path: crash_binary_path(provider, state),
        cwd: state.to_path_buf(),
        runtime_mode: "full-access".to_owned(),
        interaction_mode: "default".to_owned(),
        model: Some(crash_model(provider).to_owned()),
        options: Vec::new(),
        custom_models: Vec::new(),
        service_tier: None,
        effort: None,
        agent: None,
        resume_cursor: None,
        environment: Default::default(),
        endpoint: None,
        server_password: None,
        mcp: None,
        codex_home: (provider == "codex").then(|| {
            resolve_codex_home_layout(
                None,
                None,
                dirs::home_dir()
                    .as_deref()
                    .unwrap_or_else(|| Path::new(".")),
            )
        }),
        open_url: None,
    }
}

fn configure_crash_provider(state: &Path, provider: &str) {
    if provider != "cursor" {
        return;
    }
    hermetic_providers::write_hermetic_settings(
        state,
        serde_json::json!({
            "providerInstances": {
                "cursor": {
                    "driver": "cursor",
                    "enabled": true,
                    "config": {"binaryPath": crash_binary_path(provider, state)}
                }
            }
        }),
    );
}

#[tokio::test]
async fn cursor_crash_launch_matches_the_persisted_provider_route() {
    let state = TempDir::new().expect("crash settings");
    configure_crash_provider(state.path(), "cursor");
    let settings = bibcode_server::server_settings::ProviderSettingsStore::new(state.path())
        .get()
        .await
        .expect("persisted crash settings");
    assert_eq!(
        crash_launch("cursor", state.path()).binary_path,
        settings.provider_instances["cursor"].config["binaryPath"]
            .as_str()
            .expect("cursor route binary"),
        "the pre-launched crash driver must use the admitted executable destination"
    );
}

async fn seed_crash_delivery(engine: &OrchestrationEngine, state: &Path, provider: &str) {
    for command in [
        serde_json::json!({
            "type":"project.create", "commandId":"crash-project-create",
            "projectId":"crash-project", "title":"Crash project",
            "workspaceRoot":state, "createdAt":"2026-08-01T00:00:00Z"
        }),
        serde_json::json!({
            "type":"thread.create", "commandId":"crash-thread-create",
            "threadId":"crash-thread", "projectId":"crash-project", "title":"Crash thread",
            "modelSelection":{"instanceId":provider,"model":crash_model(provider)},
            "runtimeMode":"full-access", "interactionMode":"default",
            "branch":null, "worktreePath":null, "createdAt":"2026-08-01T00:00:00Z"
        }),
    ] {
        engine
            .dispatch(serde_json::from_value(command).expect("crash command"))
            .await
            .expect("persist crash fixture");
    }
}

fn crash_turn(provider: &str) -> Value {
    serde_json::json!({
        "type":"thread.turn.start", "commandId":"crash-turn",
        "threadId":"crash-thread",
        "message":{"messageId":"crash-message","role":"user","text":"persist me","attachments":[]},
        "modelSelection":{"instanceId":provider,"model":crash_model(provider)},
        "runtimeMode":"full-access", "interactionMode":"default",
        "createdAt":"2026-08-01T00:00:01Z"
    })
}

#[tokio::test]
async fn durable_boundary_crash_child() {
    let Some(state) = std::env::var_os(CRASH_BOUNDARY_CHILD_STATE).map(PathBuf::from) else {
        return;
    };
    let provider = std::env::var(CRASH_BOUNDARY_CHILD_PROVIDER).expect("crash provider");
    let mode = std::env::var(CRASH_BOUNDARY_CHILD_MODE).expect("crash mode");
    let sends =
        PathBuf::from(std::env::var_os(CRASH_BOUNDARY_CHILD_SENDS).expect("crash send journal"));
    configure_crash_provider(&state, &provider);
    let database = Database::create_new(state.join("delivery.sqlite3"))
        .await
        .expect("crash database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("crash migrations");
    let engine = OrchestrationEngine::start(database.clone(), EngineOptions::default())
        .await
        .expect("crash engine");
    seed_crash_delivery(&engine, &state, &provider).await;

    let factory = Arc::new(CrashBoundaryFactory {
        provider: provider.clone(),
        sends,
        crash_on_delivery: true,
        reconciliation: ProviderReconciliationOutcome::Unavailable {
            detail: "child never reconciles".to_owned(),
        },
    });
    let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        ActivityProjection::new(ActivityRepository::new(database)),
        SupervisorOptions::default(),
    ));
    supervisor
        .launch(crash_launch(&provider, &state))
        .await
        .expect("crash provider launch");
    let delivery = Arc::new(TurnDeliveryService::start(
        engine.clone(),
        supervisor.clone(),
        state.clone(),
    ));
    if matches!(mode.as_str(), "after-db-commit" | "queued-after-db-commit") {
        delivery.shutdown().await;
    }
    if mode == "queued-after-db-commit" {
        engine.dispatch(serde_json::from_value(serde_json::json!({
            "type":"thread.session.set", "commandId":"queue-running", "threadId":"crash-thread",
            "session":{"threadId":"crash-thread", "status":"running", "providerName":provider, "activeTurnId":"previous-turn", "lastError":null, "updatedAt":"2026-08-01T00:00:00Z"},
            "createdAt":"2026-08-01T00:00:00Z"
        })).unwrap()).await.unwrap();
    }
    let mut turn = crash_turn(&provider);
    if mode == "queued-after-db-commit" {
        turn["queued"] = serde_json::json!(true);
    }
    let mut registry = RpcRegistry::empty();
    register_orchestration_rpc_with_delivery(
        &mut registry,
        engine,
        supervisor,
        state.clone(),
        delivery,
        attachment_upload_registry(),
    );
    let runtime = ServerRuntime::start_with_registry(
        ServerConfig::new(&state)
            .with_bind("127.0.0.1", 0)
            .with_unsafe_no_auth(),
        registry,
    )
    .await
    .expect("crash RPC runtime");
    let (mut socket, _) = connect_async(format!("ws://{}/ws", runtime.local_addr()))
        .await
        .expect("crash RPC websocket");
    socket
        .send(Message::Text(
            serde_json::json!({
                "_tag":"Request", "id":"1",
                "tag":"orchestration.dispatchCommand", "payload":turn,
                "headers":[]
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("crash RPC request");
    if matches!(mode.as_str(), "after-db-commit" | "queued-after-db-commit") {
        let frame = next_frame_past_heartbeat(&mut socket)
            .await
            .expect("crash RPC response")
            .expect("valid crash RPC response");
        let Message::Text(text) = frame else {
            panic!("expected crash RPC text response")
        };
        let message = serde_json::from_str::<ServerMessage>(&text).expect("crash server message");
        assert!(
            matches!(
                message,
                ServerMessage::Exit {
                    exit: RpcExit::Success { .. },
                    ..
                }
            ),
            "unexpected crash RPC response: {message:?}"
        );
        std::process::abort();
    }
    std::future::pending::<()>().await;
}

fn run_durable_boundary_child(state: &Path, provider: &str, mode: &str, sends: &Path) {
    let output = {
        let _guard = child_process_guard();
        run_child_with_deadline(
            ChildCommandSpec::new(std::env::current_exe().expect("test executable"))
                .args([
                    "--exact",
                    "durable_boundary_crash_child",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CRASH_BOUNDARY_CHILD_STATE, state)
                .env(CRASH_BOUNDARY_CHILD_PROVIDER, provider)
                .env(CRASH_BOUNDARY_CHILD_MODE, mode)
                .env(CRASH_BOUNDARY_CHILD_SENDS, sends),
            &format!("{provider} {mode}"),
            CHILD_PROCESS_DEADLINE,
        )
    };
    assert!(
        !output.status.success(),
        "{provider} {mode} child must abort\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            output.status.signal(),
            Some(libc::SIGABRT),
            "{provider} {mode} must reach the intentional crash boundary\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn recorded_sends(path: &Path) -> usize {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .count()
}

async fn wait_for_crash_delivery_state(engine: &OrchestrationEngine, expected: TurnDeliveryState) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let row = engine
                .repositories()
                .get_provider_turn_delivery("crash-turn".to_owned())
                .await
                .expect("crash delivery query")
                .expect("crash delivery row");
            if row.state == expected {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("delivery did not reach {expected:?}"));
}

async fn restart_crash_boundary(
    state: &Path,
    sends: &Path,
    provider: &str,
    expected_before: TurnDeliveryState,
    expected_after: TurnDeliveryState,
    reconciliation: ProviderReconciliationOutcome,
    launch_provider: bool,
) {
    let database = Database::open_existing(state.join("delivery.sqlite3"))
        .await
        .expect("restart database");
    let engine = OrchestrationEngine::start(database.clone(), EngineOptions::default())
        .await
        .expect("restart engine");
    let row = engine
        .repositories()
        .get_provider_turn_delivery("crash-turn".to_owned())
        .await
        .expect("pre-restart delivery")
        .expect("pre-restart row");
    assert_eq!(row.state, expected_before);
    assert_eq!(
        recorded_sends(sends),
        usize::from(expected_before == TurnDeliveryState::Sending)
    );
    reconcile_abandoned_provider_sessions(&engine)
        .await
        .expect("restart session reconciliation");

    let never = Arc::new(NeverProvider::default());
    let factory: Arc<dyn ProviderDriverFactory> = if expected_before == TurnDeliveryState::Queued {
        never.clone()
    } else {
        Arc::new(CrashBoundaryFactory {
            provider: provider.to_owned(),
            sends: sends.to_path_buf(),
            crash_on_delivery: false,
            reconciliation,
        })
    };
    let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        ActivityProjection::new(ActivityRepository::new(database)),
        SupervisorOptions::default(),
    ));
    if launch_provider {
        supervisor
            .launch(crash_launch(provider, state))
            .await
            .expect("restart provider launch");
    }
    let delivery =
        TurnDeliveryService::start(engine.clone(), supervisor.clone(), state.to_path_buf());
    if expected_before == TurnDeliveryState::Queued {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    wait_for_crash_delivery_state(&engine, expected_after).await;
    assert_eq!(
        recorded_sends(sends),
        usize::from(expected_before != TurnDeliveryState::Queued),
        "restart must not duplicate a send or send a queued row"
    );

    delivery.shutdown().await;
    assert_eq!(
        never.creates.load(Ordering::SeqCst),
        0,
        "queued recovery never launches a provider"
    );
    supervisor
        .shutdown()
        .await
        .expect("restart provider shutdown");
    engine.shutdown().await;
}

#[tokio::test]
async fn subprocess_crash_truth_table_preserves_exact_delivery_state_and_send_count() {
    for (provider, mode, before, after, reconciliation, launch_provider) in [
        (
            "codex",
            "queued-after-db-commit",
            TurnDeliveryState::Queued,
            TurnDeliveryState::Queued,
            ProviderReconciliationOutcome::Unavailable {
                detail: "queued rows never reconcile".to_owned(),
            },
            false,
        ),
        (
            "claudeAgent",
            "queued-after-db-commit",
            TurnDeliveryState::Queued,
            TurnDeliveryState::Queued,
            ProviderReconciliationOutcome::Unavailable {
                detail: "queued rows never reconcile".to_owned(),
            },
            false,
        ),
        (
            "codex",
            "after-db-commit",
            TurnDeliveryState::Pending,
            TurnDeliveryState::Delivered,
            ProviderReconciliationOutcome::Unavailable {
                detail: "pending rows send instead of reconciling".to_owned(),
            },
            true,
        ),
        (
            "codex",
            "after-provider-acceptance",
            TurnDeliveryState::Sending,
            TurnDeliveryState::Delivered,
            ProviderReconciliationOutcome::Found,
            true,
        ),
        (
            "opencode",
            "after-provider-acceptance",
            TurnDeliveryState::Sending,
            TurnDeliveryState::Delivered,
            ProviderReconciliationOutcome::Found,
            true,
        ),
        (
            "claudeAgent",
            "after-provider-write",
            TurnDeliveryState::Sending,
            TurnDeliveryState::Uncertain,
            ProviderReconciliationOutcome::Unavailable {
                detail: "Claude cannot reconcile exact delivery".to_owned(),
            },
            false,
        ),
        (
            "cursor",
            "after-provider-write",
            TurnDeliveryState::Sending,
            TurnDeliveryState::Uncertain,
            ProviderReconciliationOutcome::Unavailable {
                detail: "Cursor cannot reconcile exact delivery".to_owned(),
            },
            false,
        ),
    ] {
        let state = TempDir::new().expect("crash state");
        let sends = state.path().join("provider-sends");
        run_durable_boundary_child(state.path(), provider, mode, &sends);
        restart_crash_boundary(
            state.path(),
            &sends,
            provider,
            before,
            after,
            reconciliation,
            launch_provider,
        )
        .await;
    }
}

#[derive(Default)]
struct NeverProvider {
    creates: AtomicUsize,
}

impl ProviderDriverFactory for NeverProvider {
    fn create(
        &self,
        request: ProviderLaunchRequest,
    ) -> BoxRuntimeFuture<'_, Result<Arc<dyn ProviderDriver>, ProviderRuntimeError>> {
        self.creates.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Err(ProviderRuntimeError::UnsupportedProvider {
                provider: request.provider,
            })
        })
    }
}

struct CrashBoundaryFactory {
    provider: String,
    sends: PathBuf,
    crash_on_delivery: bool,
    reconciliation: ProviderReconciliationOutcome,
}

impl ProviderDriverFactory for CrashBoundaryFactory {
    fn create(
        &self,
        _request: ProviderLaunchRequest,
    ) -> BoxRuntimeFuture<'_, Result<Arc<dyn ProviderDriver>, ProviderRuntimeError>> {
        Box::pin(async move {
            let (event_sender, events) = mpsc::channel(1);
            Ok(Arc::new(CrashBoundaryDriver {
                provider: self.provider.clone(),
                sends: self.sends.clone(),
                crash_on_delivery: self.crash_on_delivery,
                reconciliation: self.reconciliation.clone(),
                _event_sender: event_sender,
                events: tokio::sync::Mutex::new(events),
            }) as Arc<dyn ProviderDriver>)
        })
    }
}

struct CrashBoundaryDriver {
    provider: String,
    sends: PathBuf,
    crash_on_delivery: bool,
    reconciliation: ProviderReconciliationOutcome,
    _event_sender: mpsc::Sender<bibcode_server::production::provider_runtime::ProviderEvent>,
    events: tokio::sync::Mutex<
        mpsc::Receiver<bibcode_server::production::provider_runtime::ProviderEvent>,
    >,
}

impl ProviderDriver for CrashBoundaryDriver {
    fn start(&self) -> BoxRuntimeFuture<'_, Result<StartedSession, ProviderRuntimeError>> {
        Box::pin(async move {
            Ok(StartedSession {
                resume_cursor: Some(if self.provider == "codex" {
                    serde_json::json!({"threadId":"crash-provider-session"})
                } else {
                    serde_json::json!({"sessionId":"crash-provider-session"})
                }),
                runtime_payload: None,
                activity_capabilities: bibcode_server::activity::ActivityCapabilities::none(),
            })
        })
    }

    fn send(
        &self,
        _text: String,
        _attachments: Vec<Value>,
        _interaction_mode: String,
    ) -> BoxRuntimeFuture<'_, Result<Option<String>, ProviderRuntimeError>> {
        Box::pin(async { unreachable!("durable delivery uses ProviderDriver::deliver") })
    }

    fn deliver(
        &self,
        _text: String,
        _attachments: Vec<Value>,
        _interaction_mode: String,
        _delivery_key: String,
    ) -> BoxRuntimeFuture<'_, ProviderDeliveryOutcome> {
        Box::pin(async move {
            let mut sends = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.sends)
                .expect("send journal");
            writeln!(sends, "{}", self.provider).expect("record provider boundary");
            sends.sync_all().expect("persist provider boundary");
            if self.crash_on_delivery {
                std::process::abort();
            }
            ProviderDeliveryOutcome::Accepted {
                turn_id: Some("crash-provider-turn".to_owned()),
            }
        })
    }

    fn reconcile(
        &self,
        _delivery_key: String,
    ) -> BoxRuntimeFuture<'_, ProviderReconciliationOutcome> {
        Box::pin(async move { self.reconciliation.clone() })
    }

    fn interrupt(
        &self,
        _turn_id: Option<String>,
    ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async { Ok(()) })
    }

    fn approve(
        &self,
        _request_id: String,
        _decision: String,
    ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async { Ok(()) })
    }

    fn answer(
        &self,
        _request_id: String,
        _answers: Value,
    ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async { Ok(()) })
    }

    fn set_mode(&self, _mode: String) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async { Ok(()) })
    }

    fn set_model(&self, _model: String) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async { Ok(()) })
    }

    fn set_options(
        &self,
        options: Vec<Value>,
    ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        let provider = self.provider.clone();
        Box::pin(async move {
            if options.is_empty() {
                Ok(())
            } else {
                Err(ProviderRuntimeError::Provider {
                    provider,
                    detail: "options are not supported by this recovery fixture".to_owned(),
                })
            }
        })
    }

    fn rollback(
        &self,
        _turn_count: i64,
        _rewind_key: String,
    ) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async { Ok(()) })
    }

    fn next_event(
        &self,
    ) -> BoxRuntimeFuture<'_, Option<bibcode_server::production::provider_runtime::ProviderEvent>>
    {
        Box::pin(async move { self.events.lock().await.recv().await })
    }

    fn shutdown(&self) -> BoxRuntimeFuture<'_, Result<(), ProviderRuntimeError>> {
        Box::pin(async { Ok(()) })
    }
}

struct RecoveryCallbacks {
    terminal_home: PathBuf,
    terminals: ServerTerminalServices,
    setup_launches: AtomicUsize,
    setup_entered: Notify,
}

impl OrchestrationEffectCallbacks for RecoveryCallbacks {
    fn workspace_for_thread<'a>(
        &'a self,
        _thread_id: &'a str,
    ) -> BoxEffectFuture<'a, Option<PathBuf>> {
        Box::pin(async { Ok(None) })
    }

    fn rollback_provider<'a>(
        &'a self,
        _thread_id: &'a str,
        _turns: i64,
        _rewind_key: &'a str,
    ) -> BoxEffectFuture<'a, ProviderRewind> {
        Box::pin(async { Ok(ProviderRewind::Rewound) })
    }

    fn stop_provider<'a>(&'a self, _thread_id: &'a str) -> BoxEffectFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }

    fn close_terminals<'a>(&'a self, thread_id: &'a str) -> BoxEffectFuture<'a, ()> {
        Box::pin(async move {
            self.terminals.close_thread_terminals(thread_id).await;
            Ok(())
        })
    }

    fn refresh_workspace<'a>(&'a self, _cwd: &'a Path) -> BoxEffectFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }

    fn setup_script_is_running<'a>(
        &'a self,
        thread_id: &'a str,
        terminal_id: &'a str,
    ) -> BoxEffectFuture<'a, bool> {
        Box::pin(async move { Ok(self.terminals.terminal_exists(thread_id, terminal_id).await) })
    }

    fn launch_setup_script<'a>(&'a self, mut input: SetupScriptLaunch) -> BoxEffectFuture<'a, ()> {
        Box::pin(async move {
            input.env.insert(
                "HOME".to_owned(),
                self.terminal_home.to_string_lossy().into_owned(),
            );
            input.env.insert(
                "USERPROFILE".to_owned(),
                self.terminal_home.to_string_lossy().into_owned(),
            );
            self.terminals.launch_setup_script(input).await?;
            if self.setup_launches.fetch_add(1, Ordering::SeqCst) == 0 {
                self.setup_entered.notify_one();
                std::future::pending::<()>().await;
            }
            Ok(())
        })
    }
}

#[derive(Debug)]
struct RecoveryControl;

impl ProductionServerControl for RecoveryControl {
    fn call(
        &self,
        _method: &'static str,
        _payload: Value,
        _cancellation: CancellationToken,
    ) -> JsonFuture {
        Box::pin(async { Ok(Value::Null) as RpcResult })
    }

    fn subscribe(&self, _method: &'static str, _cancellation: CancellationToken) -> JsonStream {
        let (_sender, receiver) = mpsc::channel(1);
        receiver
    }
}

fn terminal_services(terminal: TerminalManager) -> ServerTerminalServices {
    let usage = provider_usage::ProviderUsageService::new(
        Vec::new(),
        Arc::new(time::OffsetDateTime::now_utc),
    );
    let sampler = Arc::new(diagnostics::NativeProcessSampler::default());
    let resource_sampler = Arc::new(diagnostics::NativeResourceSampler::new(
        sampler.clone(),
        diagnostics::ProcessAttributionRegistry::new(),
        Arc::new(diagnostics::NotApplicableUiProcessObserver),
    ));
    let monitor = Arc::new(diagnostics::DiagnosticsMonitor::new(
        resource_sampler.clone(),
        Duration::from_secs(60),
    ));
    let relay = cloud::RelayClientService::new(
        || async {
            cloud::RelayClientStatus::Missing {
                version: "1.0.0".into(),
            }
        },
        |_report| async {
            Ok(cloud::RelayClientStatus::Missing {
                version: "1.0.0".into(),
            })
        },
    );
    ServerTerminalServices::new(
        terminal,
        sampler,
        resource_sampler,
        monitor,
        usage,
        relay,
        Arc::new(RecoveryControl),
    )
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "BiBCode Test")
        .env("GIT_AUTHOR_EMAIL", "bibcode@example.invalid")
        .env("GIT_COMMITTER_NAME", "BiBCode Test")
        .env("GIT_COMMITTER_EMAIL", "bibcode@example.invalid")
        .output()
        .expect("git starts");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

#[tokio::test]
async fn bootstrap_restart_after_setup_launch_reuses_worktree_and_terminal_for_persisted_thread() {
    let _process_guard = child_process_guard();
    let repository_root = TempDir::new().expect("repository");
    git(repository_root.path(), &["init", "-b", "main"]);
    std::fs::write(repository_root.path().join("README.md"), "base\n").expect("fixture");
    git(repository_root.path(), &["add", "README.md"]);
    git(repository_root.path(), &["commit", "-m", "initial"]);

    let state = TempDir::new().expect("state");
    let database_path = state.path().join("delivery.sqlite3");
    let terminal = TerminalManager::new(
        Arc::new(PortablePtyBackend),
        TerminalManagerOptions::default(),
    );
    let callbacks = Arc::new(RecoveryCallbacks {
        terminal_home: hermetic_providers::isolated_terminal_home(state.path()),
        terminals: terminal_services(terminal.clone()),
        setup_launches: AtomicUsize::new(0),
        setup_entered: Notify::new(),
    });
    let repository = Arc::new(GitRepository::default());
    let provider_factory = Arc::new(NeverProvider::default());
    let worktree_path = {
        let database = Database::create_new(&database_path)
            .await
            .expect("database");
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .expect("migrations");
        let engine = OrchestrationEngine::start(database.clone(), EngineOptions::default())
            .await
            .expect("engine");
        for value in [
            serde_json::json!({
                "type":"project.create", "commandId":"project", "projectId":"project",
                "title":"Project", "workspaceRoot":repository_root.path(),
                "createdAt":"2026-08-01T00:00:00Z"
            }),
            serde_json::json!({
                "type":"project.meta.update", "commandId":"project-scripts",
                "projectId":"project", "scripts":[{
                    "id":"install", "name":"Install", "command":"echo setup",
                    "runOnWorktreeCreate":true
                }]
            }),
        ] {
            engine
                .dispatch(serde_json::from_value::<OrchestrationCommand>(value).expect("command"))
                .await
                .expect("fixture dispatch");
        }
        let mut payload = serde_json::json!({
            "type":"thread.turn.start", "commandId":"bootstrap-restart",
            "threadId":"persisted-thread",
            "message":{"messageId":"message","role":"user","text":"build","attachments":[]},
            "bootstrap":{
                "createThread":{
                    "projectId":"project", "title":"Thread",
                    "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                    "runtimeMode":"full-access", "interactionMode":"default",
                    "branch":null, "worktreePath":null, "createdAt":"2026-08-01T00:00:01Z"
                },
                "prepareWorktree":{
                    "projectCwd":repository_root.path(), "baseBranch":"main",
                    "branch":"bibcode/restart"
                },
                "runSetupScript":true
            },
            "createdAt":"2026-08-01T00:00:01Z"
        });
        let command = serde_json::from_value::<OrchestrationCommand>(payload.clone())
            .expect("bootstrap command");
        freeze_delivery_route(&engine, &state.path().to_path_buf(), &command, &mut payload)
            .await
            .expect("freeze admission route before worktree setup");
        engine
            .dispatch(
                serde_json::from_value(serde_json::json!({
                    "type":"thread.create", "commandId":"thread", "threadId":"persisted-thread",
                    "projectId":"project", "title":"Thread",
                    "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                    "runtimeMode":"full-access", "interactionMode":"default",
                    "branch":null, "worktreePath":null, "createdAt":"2026-08-01T00:00:00Z"
                }))
                .expect("thread command"),
            )
            .await
            .expect("fixture dispatch");
        let payload = payload.to_string();
        database
            .call(move |connection| {
                connection.execute(
                    "INSERT INTO orchestration_command_receipts (command_id, aggregate_kind, aggregate_id, accepted_at, result_sequence, status, error, payload_digest) VALUES ('bootstrap-restart', 'thread', 'persisted-thread', '2026-08-01T00:00:01Z', 0, 'accepted', NULL, 'digest')",
                    [],
                )?;
                connection.execute(
                    "INSERT INTO provider_turn_outbox (command_id, thread_id, message_id, provider_instance_id, provider_kind, provider_session_id, delivery_key, payload_json, state, attempts, last_error, created_at, updated_at) VALUES ('bootstrap-restart', 'persisted-thread', 'message', 'codex', 'codex', NULL, 'key', ?, 'pending', 0, NULL, '2026-08-01T00:00:01Z', '2026-08-01T00:00:01Z')",
                    [payload],
                )?;
                Ok(())
            })
            .await
            .expect("outbox");
        let effects = OrchestrationEffects::start(
            engine.clone(),
            repository.clone(),
            callbacks.clone(),
            EffectsOptions::default(),
        )
        .await
        .expect("effects");
        let provider = Arc::new(ProviderRuntimeSupervisor::start(
            engine.clone(),
            provider_factory.clone(),
            ActivityProjection::new(ActivityRepository::new(database.clone())),
            SupervisorOptions::default(),
        ));
        let delivery = TurnDeliveryService::start(
            engine.clone(),
            provider.clone(),
            state.path().to_path_buf(),
        );
        tokio::time::timeout(
            BOOTSTRAP_RECOVERY_INTEGRATION_DEADLINE,
            callbacks.setup_entered.notified(),
        )
        .await
        .expect("setup submitted before cancellation");
        delivery.shutdown().await;
        let row = engine
            .repositories()
            .get_provider_turn_delivery("bootstrap-restart".to_owned())
            .await
            .expect("delivery")
            .expect("row");
        assert_eq!(row.state, TurnDeliveryState::Pending);
        assert_eq!(row.attempts, 0);
        assert!(
            terminal
                .subscribe_metadata()
                .await
                .initial
                .iter()
                .any(|entry| entry.thread_id == "persisted-thread"
                    && entry.terminal_id == "setup-install")
        );
        let path = engine
            .repositories()
            .get_thread("persisted-thread".to_owned())
            .await
            .expect("thread")
            .expect("persisted thread")
            .worktree_path
            .expect("worktree path");
        provider.shutdown().await.expect("provider shutdown");
        effects.shutdown().await;
        engine.shutdown().await;
        path
    };

    {
        let database = Database::open_existing(&database_path)
            .await
            .expect("reopened database");
        let engine = OrchestrationEngine::start(database.clone(), EngineOptions::default())
            .await
            .expect("fresh engine");
        let effects = OrchestrationEffects::start(
            engine.clone(),
            repository,
            callbacks.clone(),
            EffectsOptions::default(),
        )
        .await
        .expect("restarted effects");
        let provider = Arc::new(ProviderRuntimeSupervisor::start(
            engine.clone(),
            provider_factory.clone(),
            ActivityProjection::new(ActivityRepository::new(database)),
            SupervisorOptions::default(),
        ));
        let delivery = TurnDeliveryService::start(
            engine.clone(),
            provider.clone(),
            state.path().to_path_buf(),
        );
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let row = engine
                    .repositories()
                    .get_provider_turn_delivery("bootstrap-restart".to_owned())
                    .await
                    .expect("delivery")
                    .expect("row");
                if row.state == TurnDeliveryState::Pending
                    && row.attempts == 1
                    && row.last_error.as_deref().is_some_and(|detail| {
                        detail.starts_with("Codex is turned off or not set up.")
                    })
                {
                    break;
                }
                assert!(
                    row.attempts <= 1,
                    "recovery retried more than once: {row:?}"
                );
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("launch failure is consumed and durably returned to pending");
        let thread = engine
            .repositories()
            .get_thread("persisted-thread".to_owned())
            .await
            .expect("thread")
            .expect("persisted thread");
        assert_eq!(thread.branch.as_deref(), Some("bibcode/restart"));
        assert_eq!(
            thread.worktree_path.as_deref(),
            Some(worktree_path.as_str())
        );
        delivery.shutdown().await;
        provider.shutdown().await.expect("provider shutdown");
        effects.shutdown().await;
        engine.shutdown().await;
    }

    assert_eq!(callbacks.setup_launches.load(Ordering::SeqCst), 1);
    assert_eq!(provider_factory.creates.load(Ordering::SeqCst), 1);
    let worktrees = git(repository_root.path(), &["worktree", "list", "--porcelain"]);
    assert_eq!(
        worktrees
            .matches("branch refs/heads/bibcode/restart")
            .count(),
        1
    );
    assert!(worktrees.replace('\\', "/").contains(&worktree_path));
    terminal.shutdown().await;
    git(
        repository_root.path(),
        &["worktree", "remove", "--force", &worktree_path],
    );
    git(repository_root.path(), &["branch", "-D", "bibcode/restart"]);
}

#[test]
fn missing_origin_keeps_durable_delivery_pending_without_provider_route() {
    let state = TempDir::new().expect("child state");
    let trace_path = state.path().join("git-trace.json");
    let output = {
        let _guard = child_process_guard();
        run_child_with_deadline(
            ChildCommandSpec::new(std::env::current_exe().expect("test executable"))
                .args([
                    "--exact",
                    "missing_origin_delivery_child",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(MISSING_ORIGIN_CHILD_TRACE, &trace_path)
                .env("GIT_TRACE2_EVENT", &trace_path),
            "missing-origin",
            CHILD_PROCESS_DEADLINE,
        )
    };
    let trace = std::fs::read_to_string(&trace_path).unwrap_or_else(|error| error.to_string());
    assert!(
        output.status.success(),
        "missing-origin child failed with {}\nstdout:\n{}\nstderr:\n{}\ntrace:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        trace
    );
    assert!(
        trace_has_failed_fetch(&trace),
        "child must record a nonzero `git fetch origin` exit"
    );
}

fn trace_has_failed_fetch(trace: &str) -> bool {
    let mut fetch_sessions = std::collections::HashSet::new();
    for event in trace
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
    {
        let Some(session) = event.get("sid").and_then(Value::as_str) else {
            continue;
        };
        if event.get("event").and_then(Value::as_str) == Some("start")
            && event
                .get("argv")
                .and_then(Value::as_array)
                .is_some_and(|argv| {
                    argv.windows(2).any(|args| {
                        args[0].as_str() == Some("fetch") && args[1].as_str() == Some("origin")
                    })
                })
        {
            fetch_sessions.insert(session.to_owned());
        }
        if event.get("event").and_then(Value::as_str) == Some("exit")
            && event
                .get("code")
                .and_then(Value::as_i64)
                .is_some_and(|code| code != 0)
            && fetch_sessions.contains(session)
        {
            return true;
        }
    }
    false
}

#[tokio::test]
async fn missing_origin_delivery_child() {
    let Some(trace_path) = std::env::var_os(MISSING_ORIGIN_CHILD_TRACE).map(PathBuf::from) else {
        return;
    };
    let repository_root = TempDir::new().expect("repository");
    git(repository_root.path(), &["init", "-b", "main"]);
    std::fs::write(repository_root.path().join("README.md"), "base\n").expect("fixture");
    git(repository_root.path(), &["add", "README.md"]);
    git(repository_root.path(), &["commit", "-m", "initial"]);
    assert!(
        git(repository_root.path(), &["remote"]).is_empty(),
        "the real repository must not have an origin"
    );

    let state = TempDir::new().expect("state");
    let database = Database::create_new(state.path().join("delivery.sqlite3"))
        .await
        .expect("database");
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .expect("migrations");
    let engine = OrchestrationEngine::start(database.clone(), EngineOptions::default())
        .await
        .expect("engine");
    for value in [
        serde_json::json!({
            "type":"project.create", "commandId":"missing-origin-project",
            "projectId":"missing-origin-project", "title":"Project",
            "workspaceRoot":repository_root.path(), "createdAt":"2026-08-01T00:00:00Z"
        }),
        serde_json::json!({
            "type":"project.meta.update", "commandId":"missing-origin-scripts",
            "projectId":"missing-origin-project", "scripts":[{
                "id":"install", "name":"Install", "command":"echo setup",
                "runOnWorktreeCreate":true
            }]
        }),
        serde_json::json!({
            "type":"thread.create", "commandId":"missing-origin-thread-create",
            "threadId":"missing-origin-thread", "projectId":"missing-origin-project",
            "title":"Thread", "modelSelection":{"instanceId":"codex","model":"gpt-5"},
            "runtimeMode":"full-access", "interactionMode":"default",
            "branch":null, "worktreePath":null, "createdAt":"2026-08-01T00:00:00Z"
        }),
    ] {
        engine
            .dispatch(serde_json::from_value::<OrchestrationCommand>(value).expect("command"))
            .await
            .expect("persist durable fixture");
    }
    let payload = serde_json::json!({
        "type":"thread.turn.start", "commandId":"missing-origin",
        "threadId":"missing-origin-thread",
        "message":{
            "messageId":"missing-origin-message", "role":"user",
            "text":"build", "attachments":[]
        },
        "bootstrap":{
            "createThread":{
                "projectId":"missing-origin-project", "title":"Thread",
                "modelSelection":{"instanceId":"codex","model":"gpt-5"},
                "runtimeMode":"full-access", "interactionMode":"default",
                "branch":null, "worktreePath":null,
                "createdAt":"2026-08-01T00:00:01Z"
            },
            "prepareWorktree":{
                "projectCwd":repository_root.path(), "baseBranch":"main",
                "branch":"bibcode/missing-origin", "startFromOrigin":true
            },
            "runSetupScript":true
        },
        "createdAt":"2026-08-01T00:00:01Z"
    })
    .to_string();
    database
        .call(move |connection| {
            connection.execute(
                "INSERT INTO orchestration_command_receipts (command_id, aggregate_kind, aggregate_id, accepted_at, result_sequence, status, error, payload_digest) VALUES ('missing-origin', 'thread', 'missing-origin-thread', '2026-08-01T00:00:01Z', 0, 'accepted', NULL, 'digest')",
                [],
            )?;
            connection.execute(
                "INSERT INTO provider_turn_outbox (command_id, thread_id, message_id, provider_instance_id, provider_kind, provider_session_id, delivery_key, payload_json, state, attempts, last_error, created_at, updated_at) VALUES ('missing-origin', 'missing-origin-thread', 'missing-origin-message', 'codex', 'codex', NULL, 'missing-origin-key', ?, 'pending', 0, NULL, '2026-08-01T00:00:01Z', '2026-08-01T00:00:01Z')",
                [payload],
            )?;
            Ok(())
        })
        .await
        .expect("persist composite bootstrap outbox");
    assert!(
        engine
            .repositories()
            .get_project("missing-origin-project".to_owned())
            .await
            .expect("project projection")
            .is_some()
    );
    let persisted_thread = engine
        .repositories()
        .get_thread("missing-origin-thread".to_owned())
        .await
        .expect("thread projection")
        .expect("persisted thread");
    assert!(persisted_thread.branch.is_none());
    assert!(persisted_thread.worktree_path.is_none());
    let pending = engine
        .repositories()
        .get_provider_turn_delivery("missing-origin".to_owned())
        .await
        .expect("delivery projection")
        .expect("durable composite outbox row");
    assert_eq!(pending.state, TurnDeliveryState::Pending);
    assert!(pending.payload.get("bootstrap").is_some());
    let terminal = TerminalManager::new(
        Arc::new(PortablePtyBackend),
        TerminalManagerOptions::default(),
    );
    let callbacks = Arc::new(RecoveryCallbacks {
        terminal_home: hermetic_providers::isolated_terminal_home(state.path()),
        terminals: terminal_services(terminal.clone()),
        setup_launches: AtomicUsize::new(0),
        setup_entered: Notify::new(),
    });
    let effects = OrchestrationEffects::start(
        engine.clone(),
        Arc::new(GitRepository::default()),
        callbacks.clone(),
        EffectsOptions::default(),
    )
    .await
    .expect("effects");
    let provider_factory = Arc::new(NeverProvider::default());
    let provider = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        provider_factory.clone(),
        ActivityProjection::new(ActivityRepository::new(database)),
        SupervisorOptions::default(),
    ));
    assert!(
        trace_path.exists(),
        "fixture Git commands must emit Trace2 evidence"
    );
    if trace_path.exists() {
        std::fs::remove_file(&trace_path).expect("clear setup Git trace");
    }
    let delivery =
        TurnDeliveryService::start(engine.clone(), provider.clone(), state.path().to_path_buf());

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if std::fs::read_to_string(&trace_path)
                .ok()
                .is_some_and(|trace| trace_has_failed_fetch(&trace))
            {
                break;
            }
            let delivery_state = engine
                .repositories()
                .get_provider_turn_delivery("missing-origin".to_owned())
                .await
                .expect("delivery during fetch wait")
                .expect("durable delivery during fetch wait")
                .state;
            assert_eq!(
                delivery_state,
                TurnDeliveryState::Pending,
                "delivery left pending before the expected fetch evidence"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("real `git fetch origin` failure trace");
    let row = engine
        .repositories()
        .get_provider_turn_delivery("missing-origin".to_owned())
        .await
        .expect("delivery")
        .expect("row");
    assert_eq!(row.state, TurnDeliveryState::Pending);
    assert_eq!(row.attempts, 0);
    assert_eq!(provider_factory.creates.load(Ordering::SeqCst), 0);
    assert_eq!(callbacks.setup_launches.load(Ordering::SeqCst), 0);
    let thread = engine
        .repositories()
        .get_thread("missing-origin-thread".to_owned())
        .await
        .expect("thread")
        .expect("persisted thread");
    assert!(thread.branch.is_none());
    assert!(thread.worktree_path.is_none());
    assert!(
        git(
            repository_root.path(),
            &["branch", "--list", "bibcode/missing-origin"]
        )
        .is_empty()
    );

    delivery.shutdown().await;
    provider.shutdown().await.expect("provider shutdown");
    effects.shutdown().await;
    engine.shutdown().await;
    terminal.shutdown().await;
}

#[tokio::test]
async fn pre_39_migration_is_restart_idempotent_without_synthesizing_historical_deliveries() {
    let _process_guard = child_process_guard();
    let state = TempDir::new().expect("migration state");
    let config = ServerConfig::new(state.path())
        .with_bind("127.0.0.1", 0)
        .with_unsafe_no_auth();
    hermetic_providers::write_hermetic_settings(&config.state_dir(), serde_json::json!({}));
    let database_path = config.database_path();
    let legacy_attachment_path = config.state_dir().join("attachments/legacy-notes");
    let legacy_attachments = r#"[{"type":"file","id":"legacy-notes","name":"notes.txt","mimeType":"text/plain","sizeBytes":5}]"#;
    std::fs::create_dir_all(
        legacy_attachment_path
            .parent()
            .expect("legacy attachment parent"),
    )
    .expect("legacy attachment directory");
    std::fs::write(&legacy_attachment_path, b"notes").expect("legacy attachment file");
    {
        let mut connection = rusqlite::Connection::open(&database_path).expect("pre-39 database");
        run_migrations(&mut connection, Some(38)).expect("pre-39 migrations");
        connection
            .execute(
                "INSERT INTO orchestration_command_receipts (command_id, aggregate_kind, aggregate_id, accepted_at, result_sequence, status, error) VALUES ('legacy-turn', 'thread', 'legacy-thread', '2026-08-01T00:00:01Z', 1, 'accepted', NULL)",
                [],
            )
            .expect("legacy receipt");
        connection
            .execute(
                "INSERT INTO orchestration_events (event_id, aggregate_kind, stream_id, stream_version, event_type, occurred_at, command_id, causation_event_id, correlation_id, actor_kind, payload_json, metadata_json) VALUES ('legacy-event', 'thread', 'legacy-thread', 1, 'thread.message-sent', '2026-08-01T00:00:01Z', 'legacy-turn', NULL, NULL, 'user', ?, '{}')",
                [serde_json::json!({
                    "threadId":"legacy-thread", "messageId":"legacy-message",
                    "role":"user", "text":"legacy text",
                    "attachments":serde_json::from_str::<Value>(legacy_attachments).expect("legacy attachments"),
                    "turnId":null, "streaming":false,
                    "createdAt":"2026-08-01T00:00:01Z",
                    "updatedAt":"2026-08-01T00:00:01Z"
                }).to_string()],
            )
            .expect("legacy event");
        connection
            .execute(
                "INSERT INTO projection_thread_messages (message_id, thread_id, turn_id, role, text, is_streaming, created_at, updated_at, attachments_json) VALUES ('legacy-message', 'legacy-thread', NULL, 'user', 'legacy text', 0, '2026-08-01T00:00:01Z', '2026-08-01T00:00:01Z', ?)",
                [legacy_attachments],
            )
            .expect("legacy message projection");
    }

    for restart in 1..=2 {
        let runtime = ServerRuntime::start(config.clone())
            .await
            .unwrap_or_else(|error| panic!("restart {restart} runtime startup: {error}"));
        runtime.shutdown();
        runtime
            .join()
            .await
            .unwrap_or_else(|error| panic!("restart {restart} runtime shutdown: {error}"));
        assert_eq!(
            std::fs::read(&legacy_attachment_path).expect("legacy attachment file survives"),
            b"notes",
            "restart {restart} preserves the backfilled attachment"
        );
        let connection = rusqlite::Connection::open(&database_path).expect("restart database");
        let migration_rows: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM effect_sql_migrations WHERE migration_id = 39 AND name = 'DurableProviderTurnDelivery'",
                [],
                |row| row.get(0),
            )
            .expect("migration 39 ledger count");
        assert_eq!(
            migration_rows, 1,
            "restart {restart} records migration 39 exactly once"
        );
        let (text, attachments): (String, String) = connection
            .query_row(
                "SELECT text, attachments_json FROM projection_thread_messages WHERE message_id = 'legacy-message'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("legacy message survives");
        assert_eq!(text, "legacy text");
        assert_eq!(
            serde_json::from_str::<Value>(&attachments).expect("persisted legacy attachments"),
            serde_json::from_str::<Value>(legacy_attachments).expect("expected legacy attachments")
        );
        let attachment_refs: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM orchestration_attachment_refs WHERE command_id = 'legacy-turn' AND attachment_id = 'legacy-notes' AND content_digest IS NULL AND size_bytes = 5",
                [],
                |row| row.get(0),
            )
            .expect("legacy attachment reference");
        assert_eq!(attachment_refs, 1);
        let outbox_rows: i64 = connection
            .query_row("SELECT COUNT(*) FROM provider_turn_outbox", [], |row| {
                row.get(0)
            })
            .expect("historical outbox count");
        assert_eq!(outbox_rows, 0);
        let payload_digest: Option<String> = connection
            .query_row(
                "SELECT payload_digest FROM orchestration_command_receipts WHERE command_id = 'legacy-turn'",
                [],
                |row| row.get(0),
            )
            .expect("legacy receipt");
        assert_eq!(payload_digest, None);
    }
}

#[tokio::test]
async fn queued_deliveries_survive_restart_without_provider_launch() {
    for (held, session_status, runtime_status) in [
        (false, None, None),
        (true, None, None),
        (false, Some("running"), None),
        (false, Some("starting"), None),
        (false, Some("connecting"), None),
        (false, Some("running"), Some("error")),
        (false, Some("running"), Some("suspended")),
    ] {
        let state = TempDir::new().unwrap();
        let path = state.path().join("queued.sqlite3");
        let database = Database::create_new(&path).await.unwrap();
        database
            .call(|connection| {
                run_migrations(connection, None)?;
                Ok(())
            })
            .await
            .unwrap();
        let engine = OrchestrationEngine::start(database.clone(), EngineOptions::default())
            .await
            .unwrap();
        seed_crash_delivery(&engine, state.path(), "codex").await;
        if let Some(status) = session_status {
            for command in [
                serde_json::json!({
                    "type":"thread.session.set", "commandId":"abandoned-session", "threadId":"crash-thread",
                    "session":{"threadId":"crash-thread","status":status,"providerName":"codex","providerInstanceId":"codex","runtimeMode":"full-access","activeTurnId":"abandoned-turn","lastError":null,"updatedAt":"2026-08-01T00:00:00Z"},
                    "createdAt":"2026-08-01T00:00:00Z"
                }),
                serde_json::json!({
                    "type":"thread.message.assistant.delta", "commandId":"abandoned-partial", "threadId":"crash-thread",
                    "messageId":"abandoned-assistant", "delta":"Partial response", "turnId":"abandoned-turn",
                    "createdAt":"2026-08-01T00:00:00Z"
                }),
            ] {
                engine
                    .dispatch(serde_json::from_value(command).unwrap())
                    .await
                    .unwrap();
            }
        }
        if let Some(status) = runtime_status {
            engine
                .repositories()
                .upsert_provider_session_runtime(
                    bibcode_server::persistence::ProviderSessionRuntime {
                        thread_id: "crash-thread".into(),
                        provider_name: "codex".into(),
                        provider_instance_id: Some("codex".into()),
                        adapter_key: "codex-app-server".into(),
                        runtime_mode: "full-access".into(),
                        status: status.into(),
                        last_seen_at: "2026-08-01T00:00:00Z".into(),
                        resume_cursor: Some(serde_json::json!({"threadId":"saved-native-thread"})),
                        runtime_payload: None,
                    },
                )
                .await
                .unwrap();
        }
        let mut payload = crash_turn("codex");
        payload["queued"] = serde_json::json!(true);
        engine
            .dispatch(serde_json::from_value(payload.clone()).unwrap())
            .await
            .unwrap();
        let stored = payload.clone();
        database.call(move |connection| {
            connection.execute("INSERT INTO provider_turn_outbox (command_id, thread_id, message_id, provider_instance_id, provider_kind, delivery_key, payload_json, state, mode, held, created_at, updated_at) VALUES ('crash-turn', 'crash-thread', 'crash-message', 'codex', 'codex', 'saved-key', ?, 'queued', 'start', ?, '2026-08-01T00:00:01Z', '2026-08-01T00:00:01Z')", rusqlite::params![stored.to_string(), held])?;
            connection.execute("UPDATE projection_thread_messages SET delivery_state = 'queued', delivery_provider = 'codex', delivery_mode = 'start', delivery_held = ? WHERE message_id = 'crash-message'", [held])?;
            Ok(())
        }).await.unwrap();
        engine.shutdown().await;
        drop(engine);
        drop(database);
        let database = Database::open_existing(&path).await.unwrap();
        let engine = OrchestrationEngine::start(database.clone(), EngineOptions::default())
            .await
            .unwrap();
        reconcile_abandoned_provider_sessions(&engine)
            .await
            .unwrap();
        let event_count = engine.read_events(0).await.unwrap().len();
        reconcile_abandoned_provider_sessions(&engine)
            .await
            .unwrap();
        assert_eq!(engine.read_events(0).await.unwrap().len(), event_count);
        let never = Arc::new(NeverProvider::default());
        let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
            engine.clone(),
            never.clone(),
            ActivityProjection::new(ActivityRepository::new(database)),
            SupervisorOptions::default(),
        ));
        let delivery = TurnDeliveryService::start(
            engine.clone(),
            supervisor.clone(),
            state.path().to_path_buf(),
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        delivery.shutdown().await;
        let row = engine
            .repositories()
            .get_provider_turn_delivery("crash-turn".into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, TurnDeliveryState::Queued);
        let expected_held = held || session_status.is_some();
        assert_eq!(
            row.held, expected_held,
            "session {session_status:?}, runtime {runtime_status:?}"
        );
        assert_eq!(
            row.mode,
            bibcode_server::orchestration::TurnDeliveryMode::Start
        );
        assert_eq!(row.payload, payload);
        assert_eq!(row.attempts, 0);
        assert_eq!(row.delivery_key, "saved-key");
        assert_eq!(never.creates.load(Ordering::SeqCst), 0);
        let message = engine
            .repositories()
            .get_message("crash-message".into())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(message.delivery_state.as_deref(), Some("queued"));
        assert_eq!(message.delivery_held, Some(expected_held));
        assert!(
            !engine
                .read_events(0)
                .await
                .unwrap()
                .iter()
                .any(|event| event.event.event_type == "thread.turn-start-requested")
        );
        if session_status.is_some() {
            let session = engine
                .repositories()
                .get_thread_session("crash-thread".into())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(session.status, "error");
            assert_eq!(session.active_turn_id, None);
            assert_eq!(session.provider_name.as_deref(), Some("codex"));
            assert_eq!(session.provider_instance_id.as_deref(), Some("codex"));
            assert_eq!(session.runtime_mode, "full-access");
            assert_eq!(
                session.last_error.as_deref(),
                Some(
                    "Provider session ended when BiBCode stopped. Review delivery status before continuing."
                )
            );
            assert_eq!(session.last_error_class.as_deref(), Some("session_stopped"));
            let assistant = engine
                .repositories()
                .get_message("abandoned-assistant".into())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(assistant.text, "Partial response");
            assert!(!assistant.is_streaming);
            if runtime_status.is_some() {
                assert_eq!(
                    engine
                        .repositories()
                        .get_provider_session_runtime("crash-thread".into())
                        .await
                        .unwrap()
                        .unwrap()
                        .resume_cursor,
                    Some(serde_json::json!({"threadId":"saved-native-thread"}))
                );
            }
            engine.dispatch(serde_json::from_value(serde_json::json!({
                "type":"thread.turn.promote", "commandId":"send-now-after-restart", "threadId":"crash-thread",
                "messageId":"crash-message", "createdAt":"2026-08-01T00:00:02Z"
            })).unwrap()).await.unwrap();
            let claimed = engine
                .repositories()
                .claim_provider_turn("crash-turn".into(), "2026-08-01T00:00:03Z".into())
                .await
                .unwrap()
                .expect("Send now is claimable after restart reconciliation");
            assert_eq!(claimed.state, TurnDeliveryState::Sending);
            assert!(!claimed.held);
        }
        supervisor.shutdown().await.unwrap();
        engine.shutdown().await;
    }
}

async fn sending_steer_restart(provider: &str, expected: TurnDeliveryState) {
    use bibcode_server::orchestration::TurnDeliveryMode;
    use serde_json::json;

    let state = TempDir::new().unwrap();
    let path = state.path().join("steer.sqlite3");
    let sends = state.path().join("provider-sends");
    let database = Database::create_new(&path).await.unwrap();
    database
        .call(|connection| {
            run_migrations(connection, None)?;
            Ok(())
        })
        .await
        .unwrap();
    let engine = OrchestrationEngine::start(database.clone(), EngineOptions::default())
        .await
        .unwrap();
    seed_crash_delivery(&engine, state.path(), provider).await;
    let mut payload = crash_turn(provider);
    payload["queued"] = json!(true);
    let command: OrchestrationCommand = serde_json::from_value(payload.clone()).unwrap();
    freeze_delivery_route(&engine, &state.path().to_path_buf(), &command, &mut payload)
        .await
        .unwrap();
    engine.dispatch(command).await.unwrap();
    let provider_kind = provider.to_owned();
    database.call(move |connection| {
        connection.execute(
            "INSERT INTO provider_turn_outbox (command_id, thread_id, message_id, provider_instance_id, provider_kind, provider_session_id, delivery_key, payload_json, state, mode, held, created_at, updated_at)
             VALUES ('crash-turn', 'crash-thread', 'crash-message', ?, ?, 'crash-provider-session', 'steer-key', ?, 'queued', 'start', 0, '2026-08-01T00:00:01Z', '2026-08-01T00:00:01Z')",
            rusqlite::params![provider_kind, provider_kind, payload.to_string()],
        )?;
        Ok(())
    }).await.unwrap();
    engine.dispatch(serde_json::from_value(json!({
        "type":"thread.session.set","commandId":"steer-running","threadId":"crash-thread","createdAt":"2026-08-01T00:00:02Z",
        "session":{"threadId":"crash-thread","status":"running","providerName":provider,"activeTurnId":"steer-active","lastError":null,"updatedAt":"2026-08-01T00:00:02Z"}
    })).unwrap()).await.unwrap();
    engine.dispatch(serde_json::from_value(json!({"type":"thread.turn.steer","commandId":"steer-request","threadId":"crash-thread","messageId":"crash-message","createdAt":"2026-08-01T00:00:03Z"})).unwrap()).await.unwrap();
    let sending = engine
        .repositories()
        .claim_provider_turn("crash-turn".into(), "2026-08-01T00:00:04Z".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sending.mode, TurnDeliveryMode::Steer);
    assert_eq!(sending.state, TurnDeliveryState::Sending);
    engine.shutdown().await;
    drop(engine);
    drop(database);

    let database = Database::open_existing(&path).await.unwrap();
    let engine = OrchestrationEngine::start(database.clone(), EngineOptions::default())
        .await
        .unwrap();
    reconcile_abandoned_provider_sessions(&engine)
        .await
        .unwrap();
    let never = Arc::new(NeverProvider::default());
    let factory: Arc<dyn ProviderDriverFactory> = if provider == "codex" {
        Arc::new(CrashBoundaryFactory {
            provider: provider.into(),
            sends: sends.clone(),
            crash_on_delivery: false,
            reconciliation: ProviderReconciliationOutcome::Found,
        })
    } else {
        never.clone()
    };
    let supervisor = Arc::new(ProviderRuntimeSupervisor::start(
        engine.clone(),
        factory,
        ActivityProjection::new(ActivityRepository::new(database)),
        SupervisorOptions::default(),
    ));
    if provider == "codex" {
        supervisor
            .launch(crash_launch(provider, state.path()))
            .await
            .unwrap();
    }
    let delivery = TurnDeliveryService::start(
        engine.clone(),
        supervisor.clone(),
        state.path().to_path_buf(),
    );
    wait_for_crash_delivery_state(&engine, expected).await;
    delivery.shutdown().await;
    let row = engine
        .repositories()
        .get_provider_turn_delivery("crash-turn".into())
        .await
        .unwrap()
        .unwrap();
    let message = engine
        .repositories()
        .get_message("crash-message".into())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.state, expected);
    assert_eq!(row.mode, TurnDeliveryMode::Steer);
    assert_eq!(row.delivery_key, "steer-key");
    assert_eq!(row.attempts, 1);
    assert_eq!(
        message.delivery_state.as_deref(),
        Some(if provider == "codex" {
            "delivered"
        } else {
            "uncertain"
        })
    );
    assert_eq!(message.delivery_mode.as_deref(), Some("steer"));
    assert_eq!(
        message.turn_id.as_deref(),
        if provider == "codex" {
            Some("steer-active")
        } else {
            None
        }
    );
    assert_eq!(
        recorded_sends(&sends),
        0,
        "recovery must not resend steering"
    );
    assert_eq!(
        never.creates.load(Ordering::SeqCst),
        0,
        "Claude ambiguity needs no runtime launch"
    );
    supervisor.shutdown().await.unwrap();
    engine.shutdown().await;
}

#[tokio::test]
async fn codex_sending_steer_recovers_as_delivered_with_turn_attribution() {
    sending_steer_restart("codex", TurnDeliveryState::Delivered).await;
}

#[tokio::test]
async fn claude_sending_steer_recovers_as_uncertain_without_resend() {
    sending_steer_restart("claudeAgent", TurnDeliveryState::Uncertain).await;
}

fn attachment_upload_registry() -> bibcode_server::transfer::staging::UploadRegistry {
    bibcode_server::transfer::staging::UploadRegistry::new(
        std::env::temp_dir().join(format!("bibcode-test-uploads-{}", uuid::Uuid::new_v4())),
        bibcode_server::transfer::staging::UploadLimits::default(),
        std::sync::Arc::new(tokio::time::Instant::now),
    )
}
