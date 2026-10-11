//! Windows Defender Firewall integration for grant-driven server exposure.
//!
//! The desktop backend port is picked dynamically, so the inbound allow rule is
//! program-scoped rather than port-scoped. Non-Windows platforms have no managed
//! firewall here and every call is a successful no-op.

#[cfg(windows)]
use bibcode_server::process::ProcessRunner;
#[cfg(any(windows, test))]
use bibcode_server::process::{OutputMode, ProcessRunInput};
#[cfg(windows)]
use std::sync::OnceLock;
#[cfg(any(windows, test))]
use std::sync::{Arc, Mutex};
#[cfg(any(windows, test))]
use std::time::Duration;
#[cfg(any(windows, test))]
use tokio::sync::{mpsc, oneshot};

#[cfg(any(windows, test))]
const FIREWALL_COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
#[cfg(any(windows, test))]
const FIREWALL_CALLER_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(any(windows, test))]
const FIREWALL_COMMAND_MAX_OUTPUT_BYTES: usize = 64 * 1024;
#[cfg(any(windows, test))]
const FIREWALL_WORKER_WAKE_CAPACITY: usize = 1;

#[cfg_attr(
    all(not(windows), not(test)),
    expect(dead_code, reason = "Windows-only firewall command")
)]
const REMOTE_ACCESS_RULE_NAME: &str = "BiBCode Remote Access";

#[must_use]
#[cfg_attr(
    all(not(windows), not(test)),
    expect(dead_code, reason = "Windows-only firewall command")
)]
pub(crate) fn remote_access_rule_add_args(program: &str) -> Vec<String> {
    vec![
        "advfirewall".to_owned(),
        "firewall".to_owned(),
        "add".to_owned(),
        "rule".to_owned(),
        format!("name={REMOTE_ACCESS_RULE_NAME}"),
        "dir=in".to_owned(),
        "action=allow".to_owned(),
        format!("program={program}"),
        "protocol=TCP".to_owned(),
        "profile=domain,private".to_owned(),
        "enable=yes".to_owned(),
    ]
}

#[cfg_attr(
    all(not(windows), not(test)),
    expect(dead_code, reason = "Windows-only firewall command")
)]
const FIREWALL_RULE_ABSENT_MARKER: &str = "No rules match the specified criteria.";

#[must_use]
#[cfg_attr(
    all(not(windows), not(test)),
    expect(dead_code, reason = "Windows-only firewall command")
)]
pub(crate) fn remote_access_rule_show_args() -> Vec<String> {
    vec![
        "advfirewall".to_owned(),
        "firewall".to_owned(),
        "show".to_owned(),
        "rule".to_owned(),
        format!("name={REMOTE_ACCESS_RULE_NAME}"),
    ]
}

#[must_use]
#[cfg_attr(
    all(not(windows), not(test)),
    expect(dead_code, reason = "Windows-only firewall command")
)]
pub(crate) fn remote_access_rule_delete_args() -> Vec<String> {
    vec![
        "advfirewall".to_owned(),
        "firewall".to_owned(),
        "delete".to_owned(),
        "rule".to_owned(),
        format!("name={REMOTE_ACCESS_RULE_NAME}"),
    ]
}

#[cfg(any(windows, test))]
#[derive(Clone, Debug, Eq, PartialEq)]
struct FirewallCommandOutput {
    success: bool,
    stdout: String,
    stderr: String,
}

#[cfg(any(windows, test))]
trait FirewallCommandRunner: Sync {
    fn run(
        &self,
        executable: String,
        args: Vec<String>,
    ) -> impl std::future::Future<Output = Result<FirewallCommandOutput, String>> + Send;
}

#[cfg(windows)]
struct ProcessFirewallCommandRunner;

#[cfg(any(windows, test))]
struct FirewallJob {
    enabled: bool,
    program: Option<String>,
    deadline: tokio::time::Instant,
    completion: oneshot::Sender<Result<(), String>>,
}

#[cfg(any(windows, test))]
#[derive(Default)]
struct FirewallWorkerState {
    pending: Option<FirewallJob>,
}

#[cfg(any(windows, test))]
#[derive(Clone)]
struct FirewallWorker {
    state: Arc<Mutex<FirewallWorkerState>>,
    wake: mpsc::Sender<()>,
}

#[cfg(any(windows, test))]
impl FirewallWorker {
    fn start<Runner>(runner: Runner) -> Self
    where
        Runner: FirewallCommandRunner + Send + 'static,
    {
        let state = Arc::new(Mutex::new(FirewallWorkerState::default()));
        let (wake, receiver) = mpsc::channel(FIREWALL_WORKER_WAKE_CAPACITY);
        tokio::spawn(run_firewall_worker(runner, Arc::clone(&state), receiver));
        Self { state, wake }
    }

    async fn sync(
        &self,
        enabled: bool,
        program: Option<String>,
        caller_timeout: Duration,
    ) -> Result<(), String> {
        let deadline = tokio::time::Instant::now() + caller_timeout;
        let (completion, receiver) = oneshot::channel();
        let (superseded, worker_unavailable) = {
            let mut state = self.state.lock().expect("firewall worker state");
            let superseded = state.pending.replace(FirewallJob {
                enabled,
                program,
                deadline,
                completion,
            });
            let worker_unavailable = match self.wake.try_send(()) {
                Ok(()) | Err(mpsc::error::TrySendError::Full(())) => false,
                Err(mpsc::error::TrySendError::Closed(())) => {
                    state.pending.take();
                    true
                }
            };
            (superseded, worker_unavailable)
        };
        if let Some(job) = superseded {
            let _ = job.completion.send(Err(
                "firewall worker is saturated; operation was superseded by a newer desired state before execution"
                    .to_owned(),
            ));
        }
        if worker_unavailable {
            return Err("firewall worker is unavailable".to_owned());
        }

        match tokio::time::timeout_at(deadline, receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("firewall worker stopped before reporting completion".to_owned()),
            Err(_) => Err(format!(
                "firewall operation timed out after {} seconds; fail-closed cleanup is queued",
                caller_timeout.as_secs()
            )),
        }
    }
}

#[cfg(any(windows, test))]
async fn run_firewall_worker<Runner>(
    runner: Runner,
    state: Arc<Mutex<FirewallWorkerState>>,
    mut wake: mpsc::Receiver<()>,
) where
    Runner: FirewallCommandRunner + Send + 'static,
{
    while wake.recv().await.is_some() {
        loop {
            let job = { state.lock().expect("firewall worker state").pending.take() };
            let Some(job) = job else {
                break;
            };
            let already_late = tokio::time::Instant::now() >= job.deadline;
            let mut result = if already_late {
                sync_remote_access_rule_with_runner(false, &runner, || {
                    Err("expired firewall job must not add a rule".to_owned())
                })
                .await
            } else {
                let program = job.program;
                sync_remote_access_rule_with_runner(job.enabled, &runner, move || {
                    program.ok_or_else(|| "desktop executable was not provided".to_owned())
                })
                .await
            };

            let completed_late = already_late || tokio::time::Instant::now() >= job.deadline;
            if job.enabled && !already_late && completed_late {
                let cleanup = sync_remote_access_rule_with_runner(false, &runner, || {
                    Err("firewall cleanup must not add a rule".to_owned())
                })
                .await;
                result = match (result, cleanup) {
                    (_, Ok(())) => Err(
                        "firewall enable completed after its caller deadline; the late rule was removed"
                            .to_owned(),
                    ),
                    (Ok(()), Err(cleanup_error)) => Err(format!(
                        "firewall enable completed after its caller deadline, and cleanup failed: {cleanup_error}"
                    )),
                    (Err(operation_error), Err(cleanup_error)) => Err(format!(
                        "late firewall operation failed: {operation_error}; cleanup also failed: {cleanup_error}"
                    )),
                };
            } else if completed_late && result.is_ok() {
                result = Err("firewall cleanup completed after its caller deadline".to_owned());
            }

            if job.completion.send(result.clone()).is_err() {
                tracing::warn!(
                    result = ?result,
                    "firewall worker completed an operation after its caller stopped waiting"
                );
            }
        }
    }
}

#[cfg(any(windows, test))]
fn firewall_process_input(executable: String, args: Vec<String>) -> ProcessRunInput {
    ProcessRunInput::new(executable, args)
        .with_timeout(FIREWALL_COMMAND_TIMEOUT)
        .with_max_output_bytes(FIREWALL_COMMAND_MAX_OUTPUT_BYTES)
        .with_output_mode(OutputMode::Truncate)
}

#[cfg(windows)]
impl FirewallCommandRunner for ProcessFirewallCommandRunner {
    async fn run(
        &self,
        executable: String,
        args: Vec<String>,
    ) -> Result<FirewallCommandOutput, String> {
        let output = ProcessRunner
            .run(firewall_process_input(executable.clone(), args))
            .await
            .map_err(|error| format!("failed to run {executable}: {error}"))?;
        Ok(FirewallCommandOutput {
            success: output.code == Some(0),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    }
}

#[cfg(any(windows, test))]
fn firewall_command_text(output: &FirewallCommandOutput) -> String {
    format!("{}\n{}", output.stdout, output.stderr)
}

/// `Ok(true)` when the named rule is confirmed absent. A query that neither
/// reports absence nor succeeds is an error, so a missing rule is accepted
/// only after the store was actually read.
#[cfg(any(windows, test))]
fn rule_listing_is_absent(output: &FirewallCommandOutput) -> Result<bool, String> {
    let text = firewall_command_text(output);
    if text.contains(FIREWALL_RULE_ABSENT_MARKER) {
        return Ok(true);
    }
    if !output.success {
        let details = text.trim();
        let details = if details.is_empty() {
            "command exited unsuccessfully without output"
        } else {
            details
        };
        return Err(format!(
            "failed to delete and verify the remote access firewall rule: {details}"
        ));
    }
    Ok(!text.contains(REMOTE_ACCESS_RULE_NAME))
}

#[cfg(any(windows, test))]
async fn delete_and_verify_remote_access_rule<Runner>(runner: &Runner) -> Result<(), String>
where
    Runner: FirewallCommandRunner,
{
    let listing = runner
        .run("netsh".to_owned(), remote_access_rule_show_args())
        .await?;
    if rule_listing_is_absent(&listing)? {
        return Ok(());
    }
    let deletion = runner
        .run("netsh".to_owned(), remote_access_rule_delete_args())
        .await?;
    require_firewall_command_success("delete the remote access firewall rule", deletion)?;
    let confirmed = runner
        .run("netsh".to_owned(), remote_access_rule_show_args())
        .await?;
    if rule_listing_is_absent(&confirmed)? {
        Ok(())
    } else {
        Err(
            "failed to delete and verify the remote access firewall rule: remote access firewall rule is still present after deletion"
                .to_owned(),
        )
    }
}

#[cfg(any(windows, test))]
async fn sync_remote_access_rule_with_runner<Runner, ResolveProgram>(
    enabled: bool,
    runner: &Runner,
    resolve_program: ResolveProgram,
) -> Result<(), String>
where
    Runner: FirewallCommandRunner,
    ResolveProgram: FnOnce() -> Result<String, String>,
{
    delete_and_verify_remote_access_rule(runner).await?;
    if !enabled {
        return Ok(());
    }

    let program = resolve_program()?;
    let addition = runner
        .run("netsh".to_owned(), remote_access_rule_add_args(&program))
        .await?;
    require_firewall_command_success("add the remote access firewall rule", addition)
}

#[cfg(any(windows, test))]
fn require_firewall_command_success(
    operation: &str,
    output: FirewallCommandOutput,
) -> Result<(), String> {
    if output.success {
        return Ok(());
    }
    let stderr = output.stderr.trim();
    let stdout = output.stdout.trim();
    let details = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        "command exited unsuccessfully without output"
    };
    Err(format!("failed to {operation}: {details}"))
}

#[cfg(windows)]
pub(crate) async fn sync_remote_access_rule(enabled: bool) -> Result<(), String> {
    static WORKER: OnceLock<FirewallWorker> = OnceLock::new();

    let program = enabled
        .then(|| {
            std::env::current_exe()
                .map_err(|error| format!("failed to resolve desktop executable: {error}"))
                .map(|path| path.to_string_lossy().into_owned())
        })
        .transpose()?;
    WORKER
        .get_or_init(|| FirewallWorker::start(ProcessFirewallCommandRunner))
        .sync(enabled, program, FIREWALL_CALLER_TIMEOUT)
        .await
}

#[cfg(not(windows))]
pub(crate) async fn sync_remote_access_rule(_enabled: bool) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::VecDeque,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
    };
    use tokio::sync::Semaphore;

    type FirewallCall = (String, Vec<String>);

    #[derive(Clone, Default)]
    struct FakeFirewallCommandRunner {
        calls: Arc<Mutex<Vec<FirewallCall>>>,
        results: Arc<Mutex<VecDeque<Result<FirewallCommandOutput, String>>>>,
    }

    impl FakeFirewallCommandRunner {
        fn with_results(results: Vec<Result<FirewallCommandOutput, String>>) -> Self {
            Self {
                calls: Arc::default(),
                results: Arc::new(Mutex::new(results.into())),
            }
        }

        fn calls(&self) -> Vec<FirewallCall> {
            self.calls.lock().expect("firewall calls").clone()
        }
    }

    impl FirewallCommandRunner for FakeFirewallCommandRunner {
        fn run(
            &self,
            executable: String,
            args: Vec<String>,
        ) -> impl std::future::Future<Output = Result<FirewallCommandOutput, String>> + Send
        {
            self.calls
                .lock()
                .expect("firewall calls")
                .push((executable, args));
            let result = self
                .results
                .lock()
                .expect("firewall results")
                .pop_front()
                .expect("configured firewall result");
            async move { result }
        }
    }

    fn success() -> FirewallCommandOutput {
        FirewallCommandOutput {
            success: true,
            stdout: String::new(),
            stderr: String::new(),
        }
    }

    fn failure(stderr: &str) -> FirewallCommandOutput {
        FirewallCommandOutput {
            success: false,
            stdout: String::new(),
            stderr: stderr.to_owned(),
        }
    }

    #[test]
    fn add_rule_arguments_are_program_scoped() {
        let args = remote_access_rule_add_args(r"C:\Apps\BiBCode\bibcode-desktop.exe");
        assert_eq!(
            args,
            vec![
                "advfirewall".to_string(),
                "firewall".to_string(),
                "add".to_string(),
                "rule".to_string(),
                "name=BiBCode Remote Access".to_string(),
                "dir=in".to_string(),
                "action=allow".to_string(),
                r"program=C:\Apps\BiBCode\bibcode-desktop.exe".to_string(),
                "protocol=TCP".to_string(),
                "profile=domain,private".to_string(),
                "enable=yes".to_string(),
            ]
        );
    }

    #[test]
    fn firewall_processes_use_one_bounded_supervised_run() {
        let input = firewall_process_input("netsh".to_owned(), vec!["advfirewall".to_owned()]);

        assert_eq!(input.timeout, FIREWALL_COMMAND_TIMEOUT);
        assert_eq!(input.max_output_bytes, FIREWALL_COMMAND_MAX_OUTPUT_BYTES);
        assert_eq!(input.output_mode, OutputMode::Truncate);
        assert_eq!(input.command, "netsh");
        assert_eq!(input.args, ["advfirewall"]);
    }

    #[test]
    fn show_and_delete_arguments_name_the_persistent_rule() {
        assert_eq!(
            remote_access_rule_show_args(),
            vec![
                "advfirewall".to_string(),
                "firewall".to_string(),
                "show".to_string(),
                "rule".to_string(),
                "name=BiBCode Remote Access".to_string(),
            ]
        );
        assert_eq!(
            remote_access_rule_delete_args(),
            vec![
                "advfirewall".to_string(),
                "firewall".to_string(),
                "delete".to_string(),
                "rule".to_string(),
                "name=BiBCode Remote Access".to_string(),
            ]
        );
    }

    #[test]
    fn an_absent_listing_is_accepted_only_when_the_store_was_read() {
        assert!(super::rule_listing_is_absent(&success()).expect("empty success is absent"));
        assert!(
            super::rule_listing_is_absent(&FirewallCommandOutput {
                success: false,
                stdout: super::FIREWALL_RULE_ABSENT_MARKER.to_owned(),
                stderr: String::new(),
            })
            .expect("explicit absence")
        );
        assert!(
            !super::rule_listing_is_absent(&FirewallCommandOutput {
                success: true,
                stdout: "Rule Name: BiBCode Remote Access\n".to_owned(),
                stderr: String::new(),
            })
            .expect("present rule")
        );
        let error = super::rule_listing_is_absent(&failure("Access is denied."))
            .expect_err("query failure");
        assert!(error.contains("delete and verify"));
        assert!(error.contains("Access is denied."));
    }

    #[tokio::test]
    async fn a_present_rule_is_deleted_and_absence_is_confirmed_before_add() {
        let runner = FakeFirewallCommandRunner::with_results(vec![
            Ok(FirewallCommandOutput {
                success: true,
                stdout: "Rule Name: BiBCode Remote Access\n".to_owned(),
                stderr: String::new(),
            }),
            Ok(success()),
            Ok(FirewallCommandOutput {
                success: false,
                stdout: super::FIREWALL_RULE_ABSENT_MARKER.to_owned(),
                stderr: String::new(),
            }),
            Ok(success()),
        ]);

        sync_remote_access_rule_with_runner(true, &runner, || {
            Ok(r"C:\Apps\BiBCode\bibcode-desktop.exe".to_owned())
        })
        .await
        .expect("replace a present rule");

        let calls = runner.calls();
        assert_eq!(calls.len(), 4);
        assert_eq!(calls[0].1, remote_access_rule_show_args());
        assert_eq!(calls[1].1, remote_access_rule_delete_args());
        assert_eq!(calls[2].1, remote_access_rule_show_args());
        assert_eq!(
            calls[3].1,
            remote_access_rule_add_args(r"C:\Apps\BiBCode\bibcode-desktop.exe")
        );
    }

    #[tokio::test]
    async fn disabling_reports_firewall_process_launch_failure() {
        let runner = FakeFirewallCommandRunner::with_results(vec![Err("launch denied".to_owned())]);

        let error = sync_remote_access_rule_with_runner(false, &runner, || {
            panic!("disabled cleanup must not resolve the executable")
        })
        .await
        .expect_err("spawn failure must be reported");

        assert!(error.contains("launch denied"));
        assert_eq!(runner.calls().len(), 1);
        assert_eq!(runner.calls()[0].0, "netsh");
    }

    #[tokio::test]
    async fn disabling_reports_policy_denial_instead_of_claiming_cleanup() {
        let runner =
            FakeFirewallCommandRunner::with_results(vec![Ok(failure("Access is denied."))]);

        let error = sync_remote_access_rule_with_runner(false, &runner, || {
            panic!("disabled cleanup must not resolve the executable")
        })
        .await
        .expect_err("policy denial must be reported");

        assert!(error.contains("delete and verify"));
        assert!(error.contains("Access is denied."));
        assert_eq!(runner.calls().len(), 1);
    }

    #[tokio::test]
    async fn enabling_verifies_deletion_before_adding_the_program_rule() {
        let runner = FakeFirewallCommandRunner::with_results(vec![Ok(success()), Ok(success())]);

        sync_remote_access_rule_with_runner(true, &runner, || {
            Ok(r"C:\Apps\BiBCode\bibcode-desktop.exe".to_owned())
        })
        .await
        .expect("verified replacement");

        let calls = runner.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].1, remote_access_rule_show_args());
        assert_eq!(calls[1].0, "netsh");
        assert_eq!(
            calls[1].1,
            remote_access_rule_add_args(r"C:\Apps\BiBCode\bibcode-desktop.exe")
        );
    }

    #[tokio::test]
    async fn worker_replaces_idempotent_enables_and_verifies_disable() {
        let runner = FakeFirewallCommandRunner::with_results(vec![
            Ok(success()),
            Ok(success()),
            Ok(success()),
            Ok(success()),
            Ok(success()),
        ]);
        let worker = FirewallWorker::start(runner.clone());
        let program = Some(r"C:\Apps\BiBCode\bibcode-desktop.exe".to_owned());

        worker
            .sync(true, program.clone(), FIREWALL_CALLER_TIMEOUT)
            .await
            .expect("first enable");
        worker
            .sync(true, program, FIREWALL_CALLER_TIMEOUT)
            .await
            .expect("idempotent replacement");
        worker
            .sync(false, None, FIREWALL_CALLER_TIMEOUT)
            .await
            .expect("verified disable");

        assert_eq!(
            runner
                .calls()
                .iter()
                .map(|(executable, _)| executable.as_str())
                .collect::<Vec<_>>(),
            ["netsh", "netsh", "netsh", "netsh", "netsh"]
        );
    }

    #[tokio::test]
    async fn worker_does_not_add_after_failed_delete_verification() {
        let runner =
            FakeFirewallCommandRunner::with_results(vec![Ok(failure("rule is still present"))]);
        let worker = FirewallWorker::start(runner.clone());

        let error = worker
            .sync(
                true,
                Some(r"C:\Apps\BiBCode\bibcode-desktop.exe".to_owned()),
                FIREWALL_CALLER_TIMEOUT,
            )
            .await
            .expect_err("failed deletion must fail closed");

        assert!(error.contains("delete and verify"));
        assert!(error.contains("rule is still present"));
        assert_eq!(runner.calls().len(), 1);
        assert_eq!(runner.calls()[0].0, "netsh");
    }

    #[derive(Clone)]
    struct LateSpawnFirewallCommandRunner {
        calls: Arc<Mutex<Vec<FirewallCall>>>,
        block_next_add: Arc<AtomicBool>,
        release_add: Arc<Semaphore>,
    }

    impl Default for LateSpawnFirewallCommandRunner {
        fn default() -> Self {
            Self {
                calls: Arc::default(),
                block_next_add: Arc::default(),
                release_add: Arc::new(Semaphore::new(0)),
            }
        }
    }

    impl LateSpawnFirewallCommandRunner {
        fn block_next_add(&self) {
            self.block_next_add.store(true, Ordering::SeqCst);
        }

        fn calls(&self) -> Vec<FirewallCall> {
            self.calls.lock().expect("firewall calls").clone()
        }

        fn release_add(&self) {
            self.release_add.add_permits(1);
        }
    }

    impl FirewallCommandRunner for LateSpawnFirewallCommandRunner {
        fn run(
            &self,
            executable: String,
            args: Vec<String>,
        ) -> impl std::future::Future<Output = Result<FirewallCommandOutput, String>> + Send
        {
            let should_block = args.iter().any(|arg| arg == "add")
                && self.block_next_add.swap(false, Ordering::SeqCst);
            self.calls
                .lock()
                .expect("firewall calls")
                .push((executable.clone(), args));
            let release_add = self.release_add.clone();
            async move {
                if should_block {
                    release_add
                        .acquire()
                        .await
                        .expect("release semaphore")
                        .forget();
                }
                Ok(success())
            }
        }
    }

    async fn wait_for_firewall_calls(runner: &LateSpawnFirewallCommandRunner, expected: usize) {
        for _ in 0..1_000 {
            if runner.calls().len() >= expected {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!(
            "expected at least {expected} firewall calls, got {:?}",
            runner.calls()
        );
    }

    async fn wait_for_pending_firewall_job(worker: &FirewallWorker) {
        for _ in 0..1_000 {
            let has_pending = worker
                .state
                .lock()
                .expect("firewall worker state")
                .pending
                .is_some();
            if has_pending {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("expected a pending firewall job");
    }

    #[tokio::test(start_paused = true)]
    async fn caller_deadline_covers_late_spawn_and_worker_cleans_up_before_later_enable() {
        let runner = LateSpawnFirewallCommandRunner::default();
        runner.block_next_add();
        let worker = FirewallWorker::start(runner.clone());
        let first_worker = worker.clone();
        let first = tokio::spawn(async move {
            first_worker
                .sync(
                    true,
                    Some(r"C:\Apps\BiBCode\bibcode-desktop.exe".to_owned()),
                    FIREWALL_CALLER_TIMEOUT,
                )
                .await
        });
        wait_for_firewall_calls(&runner, 2).await;

        tokio::time::advance(FIREWALL_CALLER_TIMEOUT).await;
        let error = first
            .await
            .expect("caller task")
            .expect_err("caller deadline should expire while command spawn is outstanding");
        assert!(error.contains("timed out"));

        let later_worker = worker.clone();
        let later = tokio::spawn(async move {
            later_worker
                .sync(
                    true,
                    Some(r"C:\Apps\BiBCode\bibcode-desktop.exe".to_owned()),
                    FIREWALL_CALLER_TIMEOUT,
                )
                .await
        });
        tokio::task::yield_now().await;
        assert_eq!(runner.calls().len(), 2, "later enable must remain queued");

        runner.release_add();
        wait_for_firewall_calls(&runner, 5).await;
        assert_eq!(
            runner
                .calls()
                .iter()
                .map(|(executable, _)| executable.as_str())
                .collect::<Vec<_>>(),
            ["netsh", "netsh", "netsh", "netsh", "netsh"]
        );
        later
            .await
            .expect("later caller task")
            .expect("later enable should succeed after cleanup");
    }

    #[tokio::test]
    async fn worker_coalesces_queued_jobs_to_the_latest_desired_state() {
        let runner = LateSpawnFirewallCommandRunner::default();
        runner.block_next_add();
        let worker = FirewallWorker::start(runner.clone());
        let program = Some(r"C:\Apps\BiBCode\bibcode-desktop.exe".to_owned());

        let active_worker = worker.clone();
        let active_program = program.clone();
        let active = tokio::spawn(async move {
            active_worker
                .sync(true, active_program, Duration::from_secs(30))
                .await
        });
        wait_for_firewall_calls(&runner, 2).await;

        let superseded_worker = worker.clone();
        let superseded = tokio::spawn(async move {
            superseded_worker
                .sync(true, program, Duration::from_secs(30))
                .await
        });
        wait_for_pending_firewall_job(&worker).await;

        let final_worker = worker.clone();
        let final_job = tokio::spawn(async move {
            final_worker
                .sync(false, None, Duration::from_secs(30))
                .await
        });

        let superseded_error = tokio::time::timeout(Duration::from_secs(1), superseded)
            .await
            .expect("a replaced queued caller must be released promptly")
            .expect("superseded caller task")
            .expect_err("the replaced job must report explicit overload");
        assert!(superseded_error.contains("superseded"));

        runner.release_add();
        active
            .await
            .expect("active caller task")
            .expect("active enable");
        final_job
            .await
            .expect("final caller task")
            .expect("final disable");

        assert_eq!(
            runner
                .calls()
                .iter()
                .map(|(executable, _)| executable.as_str())
                .collect::<Vec<_>>(),
            ["netsh", "netsh", "netsh"],
            "only the in-flight operation and latest queued state should execute"
        );
    }
}
