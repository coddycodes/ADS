use std::{
    ffi::OsStr,
    future::Future,
    io,
    path::Path,
    process::ExitStatus,
    time::{Duration, Instant},
};

use process_wrap::tokio::{ChildWrapper, CommandWrap};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
};
use tokio_util::sync::CancellationToken;

use super::{ProcessCleanupReport, configure_supervised_background_command_wrap};
use crate::diagnostics::profiler::exits::{self, Exited, Watch};

const PROCESS_CLEANUP_WAIT_TIMEOUT: Duration = Duration::from_secs(2);
const PROCESS_OUTPUT_READ_BUFFER_BYTES: usize = 8 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SupervisedOverflow {
    Error,
    Truncate,
}

#[derive(Debug)]
pub(crate) struct SupervisedRunRequest {
    pub(crate) command: Command,
    pub(crate) stdin: Option<Vec<u8>>,
    pub(crate) timeout: Duration,
    pub(crate) cleanup_timeout: Duration,
    pub(crate) max_output_bytes: usize,
    pub(crate) overflow: SupervisedOverflow,
}

#[derive(Debug)]
pub(crate) struct SupervisedStreamOutput {
    pub(crate) bytes: Vec<u8>,
    pub(crate) observed_bytes: usize,
}

impl SupervisedStreamOutput {
    pub(crate) fn truncated(&self) -> bool {
        self.observed_bytes > self.bytes.len()
    }
}

#[derive(Debug)]
pub(crate) struct SupervisedRunOutput {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: SupervisedStreamOutput,
    pub(crate) stderr: SupervisedStreamOutput,
}

#[derive(Debug)]
pub(crate) enum SupervisedRunError {
    Spawn(io::Error),
    Pipe {
        stream: &'static str,
    },
    Stdin(io::Error),
    Read {
        stream: &'static str,
        source: io::Error,
    },
    OutputLimit {
        stream: &'static str,
        max_bytes: usize,
        observed_bytes: usize,
    },
    Timeout,
    Cancelled,
    Wait(io::Error),
}

pub(crate) async fn run_supervised(
    request: SupervisedRunRequest,
    cancellation: &CancellationToken,
) -> Result<SupervisedRunOutput, SupervisedRunError> {
    run_supervised_with_observer(request, cancellation, None, |_| {}).await
}

pub(crate) async fn run_supervised_with_spawn_observer<F>(
    request: SupervisedRunRequest,
    cancellation: &CancellationToken,
    observer: F,
) -> Result<SupervisedRunOutput, SupervisedRunError>
where
    F: FnOnce(Option<u32>) + Send,
{
    run_supervised_with_observer(request, cancellation, None, observer).await
}

#[cfg(test)]
pub(crate) async fn run_supervised_with_spawn_observer_until<F>(
    request: SupervisedRunRequest,
    cancellation: &CancellationToken,
    deadline: tokio::time::Instant,
    observer: F,
) -> Result<SupervisedRunOutput, SupervisedRunError>
where
    F: FnOnce(Option<u32>) + Send,
{
    run_supervised_with_observer(request, cancellation, Some(deadline), observer).await
}

async fn run_supervised_with_observer<F>(
    request: SupervisedRunRequest,
    cancellation: &CancellationToken,
    deadline: Option<tokio::time::Instant>,
    observer: F,
) -> Result<SupervisedRunOutput, SupervisedRunError>
where
    F: FnOnce(Option<u32>) + Send,
{
    if cancellation.is_cancelled() {
        return Err(SupervisedRunError::Cancelled);
    }
    let SupervisedRunRequest {
        command,
        stdin,
        timeout,
        cleanup_timeout,
        max_output_bytes,
        overflow,
    } = request;
    let execution = SupervisedExecution {
        stdin,
        timeout,
        max_output_bytes,
        overflow,
    };
    // What a crash report names, read before the command is wrapped.
    let watched =
        exits::watching().then(|| (command.as_std().get_program().to_owned(), Instant::now()));
    let mut command = CommandWrap::from(command);
    configure_supervised_background_command_wrap(&mut command);
    // One absolute budget covers spawn and execution: a stalled process
    // creation (AV/EDR-wrapped CreateProcess) must not hold the caller — and
    // whatever lock the caller holds — for longer than the run itself may.
    let overall_deadline =
        deadline.unwrap_or_else(|| tokio::time::Instant::now() + execution.timeout);
    #[cfg(windows)]
    let child = {
        let mut spawn = tokio::task::spawn_blocking(move || spawn_wrapped(&mut command));
        match tokio::time::timeout_at(overall_deadline, &mut spawn).await {
            Ok(joined) => joined
                .map_err(|error| {
                    SupervisedRunError::Spawn(io::Error::other(format!(
                        "supervised process spawn task failed: {error}"
                    )))
                })?
                .map_err(SupervisedRunError::Spawn)?,
            Err(_) => {
                // The blocking spawn cannot be interrupted; reap whatever it
                // eventually produces so a late child never runs unsupervised.
                tokio::spawn(async move {
                    if let Ok(Ok(child)) = spawn.await {
                        terminate_and_wait_owned(child, cleanup_timeout, "late supervised spawn")
                            .await;
                    }
                });
                return Err(SupervisedRunError::Timeout);
            }
        }
    };
    #[cfg(not(windows))]
    let child = spawn_retrying_busy_executable(&mut command, overall_deadline)
        .await
        .map_err(SupervisedRunError::Spawn)?;
    let mut guard = SupervisedChildGuard::arm(child);
    let pid = guard.child().id();
    observer(pid);

    let outcome = execute_child(
        guard.child_mut(),
        &execution,
        cancellation,
        Some(overall_deadline),
    )
    .await;
    let child = guard.disarm();
    if outcome.is_err() {
        // A failed run may already have reaped its root while a descendant
        // still holds the output pipes. The group is still signalled then: a
        // descendant in the group keeps the group's id reserved, so the kill
        // reaches exactly the stragglers. Only a group with no members left
        // could have its id reused, which takes a PID wrap-around in between.
        terminate_and_wait_owned(child, cleanup_timeout, "supervised process").await;
    }
    // A run that timed out, was cancelled or failed was stopped on request.
    if let (Some((program, spawned)), Some(pid), Ok(output)) = (&watched, pid, &outcome) {
        report_command_exit(program, pid, &output.status, spawned.elapsed());
    }
    outcome
}

/// A completed command that died of a signal. Its exit code is not watched: non-zero is
/// normal for git.
fn report_command_exit(program: &OsStr, pid: u32, status: &ExitStatus, uptime: Duration) {
    let exe = program.to_string_lossy();
    let name = Path::new(program)
        .file_name()
        .unwrap_or(program)
        .to_string_lossy();
    exits::report_exit(
        Watch::Signals,
        &Exited {
            kind: "command",
            label: &name,
            exe: &exe,
            pid,
            uptime,
        },
        status,
        false,
    );
}

/// Owns a spawned child until its supervised run settles.
///
/// A run's future can be dropped before it settles: the session drops an
/// inline RPC handler on interrupt or socket teardown, aborts a request task
/// that outlives teardown, and runtime shutdown and panics drop whatever is
/// running. None of these reach the cancellation branch, and dropping the child
/// alone kills only its root (`kill_on_drop`), so descendants such as Git's
/// transport helpers would keep running with their connections open.
///
/// Dropping an armed guard therefore requests termination of the whole
/// ownership unit (`killpg(SIGKILL)` on Unix, job termination on Windows), but
/// only while the root is unreaped: `id()` stays `Some` until tokio reaps the
/// root, and until then its PID, and so the process-group id, cannot be
/// reused. A run dropped after the root was reaped (while a descendant still
/// holds the output pipes) is therefore not signalled.
///
/// `Drop` never waits, spawns, or blocks. The dropped child hands its root to
/// tokio's orphan reaper, and whichever process adopts the killed descendants
/// reaps them. Callers that must wait for cleanup to finish cancel the run's
/// token instead.
struct SupervisedChildGuard {
    child: Option<Box<dyn ChildWrapper>>,
}

impl SupervisedChildGuard {
    fn arm(child: Box<dyn ChildWrapper>) -> Self {
        Self { child: Some(child) }
    }

    fn child(&self) -> &dyn ChildWrapper {
        self.child
            .as_deref()
            .expect("an armed supervised child guard owns its child")
    }

    fn child_mut(&mut self) -> &mut dyn ChildWrapper {
        self.child
            .as_deref_mut()
            .expect("an armed supervised child guard owns its child")
    }

    /// Hands the child to its next owner once the run has settled.
    fn disarm(mut self) -> Box<dyn ChildWrapper> {
        self.child
            .take()
            .expect("an armed supervised child guard owns its child")
    }
}

impl Drop for SupervisedChildGuard {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_deref_mut() {
            terminate_dropped_child(child);
        }
    }
}

fn terminate_dropped_child(child: &mut dyn ChildWrapper) {
    if child.id().is_none() {
        return;
    }
    let mut report = ProcessCleanupReport::default();
    request_termination(child, &mut report);
    log_cleanup_failures("dropped supervised process", &report);
}

struct SupervisedExecution {
    stdin: Option<Vec<u8>>,
    timeout: Duration,
    max_output_bytes: usize,
    overflow: SupervisedOverflow,
}

enum ExecutionOutcome<T> {
    Completed(T),
    TimedOut,
    Cancelled,
}

async fn select_execution_outcome<F>(
    mut execution: std::pin::Pin<&mut F>,
    timeout: Duration,
    cancellation: &CancellationToken,
) -> ExecutionOutcome<F::Output>
where
    F: Future + ?Sized,
{
    select_execution_outcome_until(
        execution.as_mut(),
        tokio::time::Instant::now() + timeout,
        cancellation,
    )
    .await
}

async fn select_execution_outcome_until<F>(
    mut execution: std::pin::Pin<&mut F>,
    deadline: tokio::time::Instant,
    cancellation: &CancellationToken,
) -> ExecutionOutcome<F::Output>
where
    F: Future + ?Sized,
{
    tokio::select! {
        biased;
        () = cancellation.cancelled() => ExecutionOutcome::Cancelled,
        () = tokio::time::sleep_until(deadline) => {
            let completed = std::future::poll_fn(|context| {
                std::task::Poll::Ready(match execution.as_mut().poll(context) {
                    std::task::Poll::Ready(result) => Some(result),
                    std::task::Poll::Pending => None,
                })
            })
            .await;
            match completed {
                Some(result) => ExecutionOutcome::Completed(result),
                None => ExecutionOutcome::TimedOut,
            }
        },
        result = execution.as_mut() => ExecutionOutcome::Completed(result),
    }
}

async fn execute_child(
    child: &mut dyn ChildWrapper,
    request: &SupervisedExecution,
    cancellation: &CancellationToken,
    deadline: Option<tokio::time::Instant>,
) -> Result<SupervisedRunOutput, SupervisedRunError> {
    let stdout = child
        .stdout()
        .take()
        .ok_or(SupervisedRunError::Pipe { stream: "stdout" })?;
    let stderr = child
        .stderr()
        .take()
        .ok_or(SupervisedRunError::Pipe { stream: "stderr" })?;
    let stdin = match request.stdin.as_ref() {
        Some(_) => Some(
            child
                .stdin()
                .take()
                .ok_or(SupervisedRunError::Pipe { stream: "stdin" })?,
        ),
        None => {
            drop(child.stdin().take());
            None
        }
    };

    let outcome = {
        let execution = async {
            let stdin = write_stdin(stdin, request.stdin.as_deref());
            let stdout =
                collect_output(stdout, "stdout", request.max_output_bytes, request.overflow);
            let stderr =
                collect_output(stderr, "stderr", request.max_output_bytes, request.overflow);
            let wait = async { child.wait().await.map_err(SupervisedRunError::Wait) };
            let ((), stdout, stderr, status) = tokio::try_join!(stdin, stdout, stderr, wait)?;
            Ok(SupervisedRunOutput {
                status,
                stdout,
                stderr,
            })
        };
        tokio::pin!(execution);
        match deadline {
            Some(deadline) => {
                select_execution_outcome_until(execution.as_mut(), deadline, cancellation).await
            }
            None => {
                select_execution_outcome(execution.as_mut(), request.timeout, cancellation).await
            }
        }
    };

    match outcome {
        ExecutionOutcome::Completed(result) => result,
        ExecutionOutcome::TimedOut => Err(SupervisedRunError::Timeout),
        ExecutionOutcome::Cancelled => Err(SupervisedRunError::Cancelled),
    }
}

async fn write_stdin(
    stdin: Option<tokio::process::ChildStdin>,
    input: Option<&[u8]>,
) -> Result<(), SupervisedRunError> {
    let (Some(mut stdin), Some(input)) = (stdin, input) else {
        return Ok(());
    };
    stdin
        .write_all(input)
        .await
        .map_err(SupervisedRunError::Stdin)?;
    stdin.shutdown().await.map_err(SupervisedRunError::Stdin)
}

async fn collect_output(
    mut reader: impl AsyncRead + Unpin,
    stream: &'static str,
    max_bytes: usize,
    overflow: SupervisedOverflow,
) -> Result<SupervisedStreamOutput, SupervisedRunError> {
    let mut bytes = Vec::with_capacity(max_bytes.min(64 * 1024));
    let mut observed_bytes = 0usize;
    let mut buffer = vec![0u8; PROCESS_OUTPUT_READ_BUFFER_BYTES];
    loop {
        let read = reader
            .read(&mut buffer)
            .await
            .map_err(|source| SupervisedRunError::Read { stream, source })?;
        if read == 0 {
            break;
        }
        observed_bytes = observed_bytes.saturating_add(read);
        let remaining = max_bytes.saturating_sub(bytes.len());
        bytes.extend_from_slice(&buffer[..read.min(remaining)]);
        if observed_bytes > max_bytes && overflow == SupervisedOverflow::Error {
            return Err(SupervisedRunError::OutputLimit {
                stream,
                max_bytes,
                observed_bytes,
            });
        }
    }
    Ok(SupervisedStreamOutput {
        bytes,
        observed_bytes,
    })
}

pub(crate) async fn terminate_and_wait(child: &mut dyn ChildWrapper) -> ProcessCleanupReport {
    terminate_and_wait_with_timeout(child, PROCESS_CLEANUP_WAIT_TIMEOUT).await
}

async fn terminate_and_wait_with_timeout(
    child: &mut dyn ChildWrapper,
    wait_timeout: Duration,
) -> ProcessCleanupReport {
    let mut report = ProcessCleanupReport::default();
    request_termination(child, &mut report);
    match tokio::time::timeout(wait_timeout, child.wait()).await {
        Ok(Ok(_)) => report.record_success(),
        Ok(Err(error)) => report.record_failure(format!("wait: {error}")),
        Err(_) => report.record_failure(format!(
            "wait timed out after {} ms",
            wait_timeout.as_millis()
        )),
    }
    report
}

async fn terminate_and_wait_owned(
    mut child: Box<dyn ChildWrapper>,
    wait_timeout: Duration,
    operation: &'static str,
) {
    let mut report = ProcessCleanupReport::default();
    request_termination(&mut *child, &mut report);
    match tokio::time::timeout(wait_timeout, child.wait()).await {
        Ok(Ok(_)) => report.record_success(),
        Ok(Err(error)) => report.record_failure(format!("wait: {error}")),
        Err(_) => {
            report.record_failure(format!(
                "wait timed out after {} ms",
                wait_timeout.as_millis()
            ));
            log_cleanup_failures(operation, &report);
            tokio::spawn(async move {
                let mut final_report = ProcessCleanupReport::default();
                match child.wait().await {
                    Ok(_) => final_report.record_success(),
                    Err(error) => final_report.record_failure(format!("wait: {error}")),
                }
                if final_report.failure_count > 0 {
                    log_cleanup_failures(operation, &final_report);
                } else {
                    tracing::debug!(
                        operation,
                        attempted = final_report.attempted,
                        succeeded = final_report.succeeded,
                        "background supervised process reap completed"
                    );
                }
            });
            return;
        }
    }
    log_cleanup_failures(operation, &report);
}

/// Kills the ownership unit, falling back to the root alone if that fails.
///
/// A process group that no longer exists (`ESRCH`) has nothing left to stop,
/// so it counts as stopped, not as a failure. Its leader, the root, has
/// normally exited with it and may already be reaped, so the root is killed
/// only if it still runs, having left its group: a kill by the PID of a
/// reaped root could reach an unrelated process. Windows needs no
/// counterpart: terminating an empty job succeeds.
fn request_termination(child: &mut dyn ChildWrapper, report: &mut ProcessCleanupReport) {
    match child.start_kill() {
        Ok(()) => {
            report.record_success();
            return;
        }
        Err(error) if kill_target_is_gone(&error) => {
            report.record_success();
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
        }
        Err(error) => report.record_failure(format!("kill ownership unit: {error}")),
    }
    match child.inner_mut().start_kill() {
        Ok(()) => report.record_success(),
        Err(error) => report.record_failure(format!("kill root child: {error}")),
    }
}

fn kill_target_is_gone(error: &io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::ESRCH)
    }
    #[cfg(not(unix))]
    {
        let _ = error;
        false
    }
}

pub(crate) fn log_cleanup_failures(operation: &'static str, report: &ProcessCleanupReport) {
    if report.failure_count > 0 {
        tracing::warn!(
            operation,
            attempted = report.attempted,
            succeeded = report.succeeded,
            failure_count = report.failure_count,
            failures = ?report.failures,
            "supervised process cleanup was incomplete"
        );
    }
}

#[cfg(not(windows))]
fn spawn_wrapped(command: &mut CommandWrap) -> io::Result<Box<dyn ChildWrapper>> {
    command.spawn()
}

#[cfg(not(windows))]
const SPAWN_BUSY_RETRY_DELAY: Duration = Duration::from_millis(25);
#[cfg(not(windows))]
const SPAWN_BUSY_RETRY_BUDGET: Duration = Duration::from_secs(1);

/// Spawns the command, retrying briefly while the executable is busy.
///
/// Linux refuses to execute a file that any process still holds open for
/// writing (`ETXTBSY`). A freshly installed or rewritten executable hits this
/// when the writer is still closing it, or when another thread of this
/// process forked between the writer's open and close and the child has not
/// exec'd yet. The condition clears within milliseconds, so the spawn retries
/// for at most one second inside the caller's deadline; every other error,
/// and a deadline that would be crossed by the next retry, returns
/// immediately.
///
/// Shared by the supervised runner and provider-terminal helper launchers.
#[cfg(not(windows))]
pub(crate) async fn spawn_retrying_busy_executable(
    command: &mut CommandWrap,
    deadline: tokio::time::Instant,
) -> io::Result<Box<dyn ChildWrapper>> {
    let retry_until = deadline.min(tokio::time::Instant::now() + SPAWN_BUSY_RETRY_BUDGET);
    loop {
        match spawn_wrapped(command) {
            Err(error)
                if error.kind() == io::ErrorKind::ExecutableFileBusy
                    && tokio::time::Instant::now() + SPAWN_BUSY_RETRY_DELAY < retry_until =>
            {
                tokio::time::sleep(SPAWN_BUSY_RETRY_DELAY).await;
            }
            result => return result,
        }
    }
}

#[cfg(windows)]
fn spawn_wrapped(command: &mut CommandWrap) -> io::Result<Box<dyn ChildWrapper>> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE},
        System::Threading::GetCurrentProcess,
    };

    let mut duplicated: HANDLE = std::ptr::null_mut();
    let result = command.spawn_with(|command| {
        let child = command.spawn()?;
        let raw_handle = child
            .raw_handle()
            .ok_or_else(|| io::Error::other("spawned process handle is unavailable"))?;
        let process = raw_handle.cast();
        // SAFETY: the source process handle belongs to the newly spawned child,
        // both process pseudo-handles refer to the current process, and
        // `duplicated` is valid writable storage for the new owned handle.
        if unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                process,
                GetCurrentProcess(),
                &mut duplicated,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        } == 0
        {
            let error = io::Error::last_os_error();
            cleanup_failed_windows_spawn_handle(process, "duplicate process handle");
            return Err(error);
        }
        Ok(child)
    });

    if result.is_err() && !duplicated.is_null() {
        cleanup_failed_windows_spawn_handle(duplicated, "process-wrap hook");
    }
    if !duplicated.is_null() {
        // SAFETY: this closes the duplicate created above exactly once. The
        // wrapped child retains its original process handle on success.
        unsafe { CloseHandle(duplicated) };
    }
    result
}

#[cfg(windows)]
fn cleanup_failed_windows_spawn_handle(
    process: windows_sys::Win32::Foundation::HANDLE,
    stage: &'static str,
) {
    use windows_sys::Win32::{
        Foundation::WAIT_OBJECT_0,
        System::Threading::{TerminateProcess, WaitForSingleObject},
    };

    const SPAWN_FAILURE_WAIT_MS: u32 = 5_000;

    // SAFETY: the handle names the newly spawned process and remains valid
    // through this bounded termination and wait sequence.
    let terminated = unsafe { TerminateProcess(process, 1) };
    // SAFETY: the same live process handle may be synchronously waited.
    let waited = unsafe { WaitForSingleObject(process, SPAWN_FAILURE_WAIT_MS) };
    if terminated == 0 || waited != WAIT_OBJECT_0 {
        let error = io::Error::last_os_error();
        tracing::warn!(
            stage,
            error = %super::cleanup::bound_process_cleanup_failure(error),
            "failed to fully clean up process-wrap spawn failure"
        );
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future::Future,
        pin::Pin,
        process::{ExitStatus, Stdio},
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        task::{Context, Poll},
    };

    use tokio::{process::Child, sync::watch};

    use super::*;

    #[derive(Debug)]
    struct TrackingChild {
        child: Child,
        kill_calls: Arc<AtomicUsize>,
        wait_calls: Arc<AtomicUsize>,
        kill_error: Option<fn() -> io::Error>,
    }

    #[derive(Debug)]
    struct ControlledExecution<T> {
        ready: Arc<AtomicBool>,
        output: Option<T>,
        polls: Arc<AtomicUsize>,
    }

    impl<T: Unpin> Future for ControlledExecution<T> {
        type Output = T;

        fn poll(mut self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Self::Output> {
            self.polls.fetch_add(1, Ordering::SeqCst);
            if self.ready.load(Ordering::SeqCst) {
                Poll::Ready(
                    self.output
                        .take()
                        .expect("controlled execution must complete only once"),
                )
            } else {
                Poll::Pending
            }
        }
    }

    impl ChildWrapper for TrackingChild {
        fn inner(&self) -> &dyn ChildWrapper {
            &self.child
        }

        fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
            &mut self.child
        }

        fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
            Box::new(self.child)
        }

        fn start_kill(&mut self) -> io::Result<()> {
            self.kill_calls.fetch_add(1, Ordering::SeqCst);
            match self.kill_error {
                Some(error) => Err(error()),
                None => self.child.start_kill(),
            }
        }

        fn wait(&mut self) -> Pin<Box<dyn Future<Output = io::Result<ExitStatus>> + Send + '_>> {
            self.wait_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(self.child.wait())
        }
    }

    /// A process ID above every supported `pid_max`, so even a misused fake ID
    /// names no process.
    const NONEXISTENT_PROCESS_ID: u32 = 2_147_483_647;

    #[derive(Debug)]
    struct PendingChild {
        id: Option<u32>,
        kill_calls: Arc<AtomicUsize>,
        wait_calls: Arc<AtomicUsize>,
    }

    impl ChildWrapper for PendingChild {
        fn inner(&self) -> &dyn ChildWrapper {
            self
        }

        fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
            self
        }

        fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
            self
        }

        fn id(&self) -> Option<u32> {
            self.id
        }

        fn start_kill(&mut self) -> io::Result<()> {
            self.kill_calls.fetch_add(1, Ordering::SeqCst);
            Err(io::Error::other("injected pending-child kill failure"))
        }

        fn wait(&mut self) -> Pin<Box<dyn Future<Output = io::Result<ExitStatus>> + Send + '_>> {
            self.wait_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(std::future::pending())
        }
    }

    #[derive(Debug)]
    struct DelayedReapChild {
        release: watch::Receiver<bool>,
        wait_calls: Arc<AtomicUsize>,
        drop_calls: Arc<AtomicUsize>,
    }

    impl Drop for DelayedReapChild {
        fn drop(&mut self) {
            self.drop_calls.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl ChildWrapper for DelayedReapChild {
        fn inner(&self) -> &dyn ChildWrapper {
            self
        }

        fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
            self
        }

        fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
            self
        }

        fn start_kill(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn wait(&mut self) -> Pin<Box<dyn Future<Output = io::Result<ExitStatus>> + Send + '_>> {
            self.wait_calls.fetch_add(1, Ordering::SeqCst);
            let mut release = self.release.clone();
            Box::pin(async move {
                while !*release.borrow() {
                    release
                        .changed()
                        .await
                        .map_err(|_| io::Error::other("reap release sender dropped"))?;
                }
                Ok(successful_exit_status())
            })
        }
    }

    #[cfg(unix)]
    fn successful_exit_status() -> ExitStatus {
        use std::os::unix::process::ExitStatusExt;

        ExitStatus::from_raw(0)
    }

    #[cfg(windows)]
    fn successful_exit_status() -> ExitStatus {
        use std::os::windows::process::ExitStatusExt;

        ExitStatus::from_raw(0)
    }

    fn pending_child(id: Option<u32>) -> (Box<PendingChild>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let kill_calls = Arc::new(AtomicUsize::new(0));
        let wait_calls = Arc::new(AtomicUsize::new(0));
        (
            Box::new(PendingChild {
                id,
                kill_calls: Arc::clone(&kill_calls),
                wait_calls: Arc::clone(&wait_calls),
            }),
            kill_calls,
            wait_calls,
        )
    }

    fn completed_command() -> Command {
        #[cfg(windows)]
        {
            let mut command = Command::new("cmd.exe");
            command.args(["/d", "/s", "/c", "exit /b 0"]);
            command
        }
        #[cfg(not(windows))]
        {
            let mut command = Command::new("sh");
            command.args(["-c", "exit 0"]);
            command
        }
    }

    fn tracking_child(fail_kill: bool) -> (Box<TrackingChild>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let child = completed_command()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("tracking child should spawn");
        let kill_calls = Arc::new(AtomicUsize::new(0));
        let wait_calls = Arc::new(AtomicUsize::new(0));
        (
            Box::new(TrackingChild {
                child,
                kill_calls: Arc::clone(&kill_calls),
                wait_calls: Arc::clone(&wait_calls),
                kill_error: if fail_kill {
                    Some(|| io::Error::other("injected kill failure"))
                } else {
                    None
                },
            }),
            kill_calls,
            wait_calls,
        )
    }

    fn execution() -> SupervisedExecution {
        SupervisedExecution {
            stdin: None,
            timeout: Duration::from_secs(5),
            max_output_bytes: 1024,
            overflow: SupervisedOverflow::Error,
        }
    }

    fn controlled_execution<T>(
        output: T,
    ) -> (ControlledExecution<T>, Arc<AtomicBool>, Arc<AtomicUsize>) {
        let ready = Arc::new(AtomicBool::new(false));
        let polls = Arc::new(AtomicUsize::new(0));
        (
            ControlledExecution {
                ready: Arc::clone(&ready),
                output: Some(output),
                polls: Arc::clone(&polls),
            },
            ready,
            polls,
        )
    }

    #[cfg(target_os = "linux")]
    fn busy_executable_fixture(directory: &std::path::Path) -> (std::path::PathBuf, std::fs::File) {
        use std::os::unix::fs::PermissionsExt;

        let path = directory.join("busy.sh");
        std::fs::write(&path, "#!/bin/sh\nprintf ok\n").expect("write fixture script");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .expect("fixture script permissions");
        let writer = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("hold the fixture script open for writing");
        (path, writer)
    }

    #[cfg(target_os = "linux")]
    fn busy_executable_request(path: &std::path::Path, timeout: Duration) -> SupervisedRunRequest {
        let mut command = Command::new(path);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        SupervisedRunRequest {
            command,
            stdin: None,
            timeout,
            cleanup_timeout: PROCESS_CLEANUP_WAIT_TIMEOUT,
            max_output_bytes: 1024,
            overflow: SupervisedOverflow::Error,
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn spawn_retries_while_the_executable_is_still_open_for_writing() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let (path, writer) = busy_executable_fixture(directory.path());
        let cancellation = CancellationToken::new();
        let run = tokio::spawn({
            let request = busy_executable_request(&path, Duration::from_secs(5));
            let cancellation = cancellation.clone();
            async move { run_supervised(request, &cancellation).await }
        });
        // The first attempts must fail with ETXTBSY while the writer is open.
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert!(
            !run.is_finished(),
            "spawn must keep retrying while the script is busy"
        );
        drop(writer);

        let output = run
            .await
            .expect("supervised run task")
            .expect("spawn must succeed once the writer closes");
        assert!(output.status.success());
        assert_eq!(output.stdout.bytes, b"ok");
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn spawn_stops_retrying_a_busy_executable_at_the_run_deadline() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let (path, _writer) = busy_executable_fixture(directory.path());
        let cancellation = CancellationToken::new();
        let started = std::time::Instant::now();
        let error = run_supervised(
            busy_executable_request(&path, Duration::from_millis(100)),
            &cancellation,
        )
        .await
        .expect_err("a permanently busy executable must not spawn");

        assert!(
            matches!(&error, SupervisedRunError::Spawn(source) if source.kind() == io::ErrorKind::ExecutableFileBusy),
            "unexpected error: {error:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "retries must stop at the run deadline, not the retry budget"
        );
    }

    #[tokio::test]
    async fn pre_cancelled_run_rejects_before_spawning_the_command() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let error = run_supervised(
            SupervisedRunRequest {
                command: Command::new("bibcode-command-that-must-not-exist"),
                stdin: None,
                timeout: Duration::from_secs(5),
                cleanup_timeout: PROCESS_CLEANUP_WAIT_TIMEOUT,
                max_output_bytes: 1024,
                overflow: SupervisedOverflow::Error,
            },
            &cancellation,
        )
        .await
        .expect_err("pre-cancelled execution must not attempt to spawn");

        assert!(matches!(error, SupervisedRunError::Cancelled));
    }

    #[tokio::test(start_paused = true)]
    async fn ready_execution_wins_the_timeout_wake_without_a_separate_waker() {
        let expected_status = successful_exit_status();
        let completed: Result<SupervisedRunOutput, SupervisedRunError> = Ok(SupervisedRunOutput {
            status: expected_status,
            stdout: SupervisedStreamOutput {
                bytes: b"stdout complete".to_vec(),
                observed_bytes: 15,
            },
            stderr: SupervisedStreamOutput {
                bytes: b"stderr complete".to_vec(),
                observed_bytes: 15,
            },
        });
        let (execution, ready, polls) = controlled_execution(completed);
        tokio::pin!(execution);
        let cancellation = CancellationToken::new();
        let outcome =
            select_execution_outcome(execution.as_mut(), Duration::from_secs(5), &cancellation);
        tokio::pin!(outcome);

        std::future::poll_fn(|context| {
            assert!(
                outcome.as_mut().poll(context).is_pending(),
                "the controlled complete execution must start pending"
            );
            std::task::Poll::Ready(())
        })
        .await;
        assert_eq!(polls.load(Ordering::SeqCst), 1);

        ready.store(true, Ordering::SeqCst);
        tokio::time::advance(Duration::from_secs(5)).await;

        let ExecutionOutcome::Completed(output) = outcome.await else {
            panic!("completed execution must win over the simultaneous timeout wake");
        };
        let output = output.expect("controlled execution output");
        assert_eq!(output.status, expected_status);
        assert_eq!(output.stdout.bytes, b"stdout complete");
        assert_eq!(output.stderr.bytes, b"stderr complete");
        assert_eq!(polls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn pending_execution_still_times_out_at_the_boundary() {
        let (execution, _ready, polls) = controlled_execution(37usize);
        tokio::pin!(execution);
        let cancellation = CancellationToken::new();
        let outcome =
            select_execution_outcome(execution.as_mut(), Duration::from_secs(5), &cancellation);
        tokio::pin!(outcome);

        std::future::poll_fn(|context| {
            assert!(outcome.as_mut().poll(context).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        tokio::time::advance(Duration::from_secs(5)).await;

        assert!(matches!(outcome.await, ExecutionOutcome::TimedOut));
        assert_eq!(polls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn pending_execution_does_not_reanchor_a_precomputed_absolute_deadline() {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        tokio::time::advance(Duration::from_secs(4)).await;
        let (execution, _ready, polls) = controlled_execution(37usize);
        tokio::pin!(execution);
        let cancellation = CancellationToken::new();
        let outcome = select_execution_outcome_until(execution.as_mut(), deadline, &cancellation);
        tokio::pin!(outcome);

        std::future::poll_fn(|context| {
            assert!(outcome.as_mut().poll(context).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        tokio::time::advance(Duration::from_secs(1)).await;

        assert!(matches!(outcome.await, ExecutionOutcome::TimedOut));
        assert_eq!(polls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn cancellation_wins_over_simultaneous_completion_and_timeout() {
        let (execution, ready, polls) = controlled_execution(37usize);
        tokio::pin!(execution);
        let cancellation = CancellationToken::new();
        let outcome =
            select_execution_outcome(execution.as_mut(), Duration::from_secs(5), &cancellation);
        tokio::pin!(outcome);

        std::future::poll_fn(|context| {
            assert!(outcome.as_mut().poll(context).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        ready.store(true, Ordering::SeqCst);
        cancellation.cancel();
        tokio::time::advance(Duration::from_secs(5)).await;

        assert!(matches!(outcome.await, ExecutionOutcome::Cancelled));
        assert_eq!(polls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn missing_required_stream_still_kills_and_waits() {
        let (mut child, kill_calls, wait_calls) = tracking_child(false);
        let error = execute_child(&mut *child, &execution(), &CancellationToken::new(), None)
            .await
            .expect_err("missing stdout should fail");
        assert!(matches!(
            error,
            SupervisedRunError::Pipe { stream: "stdout" }
        ));

        let report = terminate_and_wait(&mut *child).await;
        assert_eq!(kill_calls.load(Ordering::SeqCst), 1);
        assert_eq!(wait_calls.load(Ordering::SeqCst), 1);
        assert_eq!(report.attempted, 2);
    }

    #[tokio::test]
    async fn failed_kill_still_waits_and_bounds_cleanup_failures() {
        let (mut child, kill_calls, wait_calls) = tracking_child(true);
        let report = terminate_and_wait(&mut *child).await;
        assert_eq!(kill_calls.load(Ordering::SeqCst), 1);
        assert_eq!(wait_calls.load(Ordering::SeqCst), 1);
        assert_eq!(report.attempted, 3);
        assert_eq!(report.failure_count, 1);
        assert!(report.failures[0].chars().count() <= 160);

        let mut bounded = ProcessCleanupReport::default();
        for index in 0..32 {
            bounded.record_failure(format!("{index}:{}", "x".repeat(1_000)));
        }
        assert_eq!(bounded.failure_count, 32);
        assert_eq!(bounded.failures.len(), 8);
        assert!(
            bounded
                .failures
                .iter()
                .all(|failure| failure.chars().count() <= 160)
        );
    }

    /// A real child spawned the way the supervised runner and the provider
    /// runtime spawn theirs, running a command that exits at once.
    #[cfg(unix)]
    fn spawn_completed_supervised_child() -> Box<dyn ChildWrapper> {
        let mut command = completed_command();
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut command = CommandWrap::from(command);
        configure_supervised_background_command_wrap(&mut command);
        command.spawn().expect("supervised child should spawn")
    }

    /// Cleanup that finds the process group already gone has nothing left to
    /// stop (#83). `wait` reaps the root and the rest of its group, as when a
    /// run fails after its command exited; `try_wait` reaps the root behind
    /// tokio's back, as provider admission does for a provider that exited
    /// before it was attributed.
    #[cfg(unix)]
    #[tokio::test]
    async fn cleanup_of_an_already_reaped_process_group_reports_no_failure() {
        let mut waited = spawn_completed_supervised_child();
        waited
            .wait()
            .await
            .expect("reap the root and its process group");
        let mut polled = spawn_completed_supervised_child();
        tokio::time::timeout(Duration::from_secs(5), async {
            while polled.try_wait().expect("poll the root").is_none() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the completed command exits");

        for (shape, mut child) in [("polled", polled), ("waited", waited)] {
            let report = terminate_and_wait(&mut *child).await;

            assert_eq!(report.failure_count, 0, "{shape}: {:?}", report.failures);
            assert_eq!(report.attempted, report.succeeded, "{shape}");
        }
    }

    /// A root that moves itself into another process group leaves its own
    /// group empty, so the group kill finds nothing (`ESRCH`) while the root
    /// still runs: cleanup must still kill it directly.
    #[cfg(unix)]
    #[tokio::test]
    async fn cleanup_still_kills_a_root_that_left_its_process_group() {
        let mut command = Command::new("sleep");
        command
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: getpgrp reads this process's group. The hook only calls the
        // async-signal-safe setpgid, after the wrapper has made the child the
        // leader of its own group, to join this test process's group.
        let group = unsafe { libc::getpgrp() };
        unsafe {
            command.pre_exec(move || {
                if libc::setpgid(0, group) == 0 {
                    Ok(())
                } else {
                    Err(io::Error::last_os_error())
                }
            });
        }
        let mut command = CommandWrap::from(command);
        configure_supervised_background_command_wrap(&mut command);
        let mut child = command.spawn().expect("supervised child should spawn");

        let report = terminate_and_wait(&mut *child).await;

        assert_eq!(report.failure_count, 0, "{:?}", report.failures);
        assert_eq!(report.attempted, report.succeeded);
    }

    #[cfg(unix)]
    #[derive(Clone, Default)]
    struct CapturedLog(Arc<std::sync::Mutex<Vec<u8>>>);

    #[cfg(unix)]
    impl io::Write for CapturedLog {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .expect("captured log")
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// The path that logged "supervised process cleanup was incomplete" for an
    /// already-exited group: a failed run hands its child to owned cleanup.
    #[cfg(unix)]
    #[tokio::test]
    async fn owned_cleanup_of_an_already_reaped_process_group_logs_no_warning() {
        let log = CapturedLog::default();
        let _logging = tracing::subscriber::set_default(
            tracing_subscriber::fmt()
                .with_ansi(false)
                .with_writer({
                    let log = log.clone();
                    move || log.clone()
                })
                .finish(),
        );
        let mut child = spawn_completed_supervised_child();
        child
            .wait()
            .await
            .expect("reap the root and its process group");

        terminate_and_wait_owned(child, PROCESS_CLEANUP_WAIT_TIMEOUT, "supervised process").await;

        let log =
            String::from_utf8(log.0.lock().expect("captured log").clone()).expect("UTF-8 log");
        assert!(!log.contains("cleanup was incomplete"), "{log}");
    }

    /// Only a target that no longer exists is exempt: any other kill error is
    /// still a cleanup failure, and the root is still killed directly.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_refused_ownership_unit_kill_is_still_a_cleanup_failure() {
        let (mut child, kill_calls, _wait_calls) = tracking_child(false);
        child.kill_error = Some(|| io::Error::from_raw_os_error(libc::EPERM));

        let report = terminate_and_wait(&mut *child).await;

        assert_eq!(kill_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            report.failures,
            [format!(
                "kill ownership unit: {}",
                io::Error::from_raw_os_error(libc::EPERM)
            )]
        );
        // The ownership unit failed; the root kill and the wait succeeded.
        assert_eq!((report.attempted, report.succeeded), (3, 2));
    }

    #[tokio::test]
    async fn pending_child_with_failed_owner_and_root_kills_returns_bounded_report() {
        let (mut child, kill_calls, wait_calls) = pending_child(Some(NONEXISTENT_PROCESS_ID));
        let cleanup =
            tokio::time::timeout(Duration::from_secs(3), terminate_and_wait(&mut *child)).await;
        let report = cleanup.expect("cleanup must return within its bounded wait deadline");

        assert_eq!(kill_calls.load(Ordering::SeqCst), 2);
        assert_eq!(wait_calls.load(Ordering::SeqCst), 1);
        assert_eq!(report.attempted, 3);
        assert_eq!(report.failure_count, 3);
        assert!(
            report
                .failures
                .iter()
                .all(|failure| failure.chars().count() <= 160)
        );
    }

    #[tokio::test]
    async fn owned_cleanup_retains_reaper_after_initial_wait_timeout() {
        let (release, release_receiver) = watch::channel(false);
        let wait_calls = Arc::new(AtomicUsize::new(0));
        let drop_calls = Arc::new(AtomicUsize::new(0));
        let child: Box<dyn ChildWrapper> = Box::new(DelayedReapChild {
            release: release_receiver,
            wait_calls: Arc::clone(&wait_calls),
            drop_calls: Arc::clone(&drop_calls),
        });

        terminate_and_wait_owned(
            child,
            Duration::from_millis(10),
            "delayed-reap test process",
        )
        .await;

        tokio::time::timeout(Duration::from_secs(1), async {
            while wait_calls.load(Ordering::SeqCst) < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("background reaper must take ownership after the initial wait expires");
        assert_eq!(
            drop_calls.load(Ordering::SeqCst),
            0,
            "child owner must remain live while the background wait is pending"
        );

        release.send_replace(true);
        tokio::time::timeout(Duration::from_secs(1), async {
            while drop_calls.load(Ordering::SeqCst) == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("background reaper must release its owner after wait completes");
        assert_eq!(wait_calls.load(Ordering::SeqCst), 2);
        assert_eq!(drop_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn dropping_an_armed_guard_terminates_the_child_once_without_waiting() {
        let (child, kill_calls, wait_calls) = tracking_child(false);

        drop(SupervisedChildGuard::arm(child));

        assert_eq!(kill_calls.load(Ordering::SeqCst), 1);
        assert_eq!(wait_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_disarmed_guard_hands_the_child_over_without_signalling_it() {
        let (child, kill_calls, wait_calls) = tracking_child(false);

        let mut child = SupervisedChildGuard::arm(child).disarm();
        child
            .wait()
            .await
            .expect("the new owner can still reap the child");
        drop(child);

        assert_eq!(kill_calls.load(Ordering::SeqCst), 0);
        assert_eq!(wait_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_guard_whose_root_was_reaped_does_not_signal_its_id() {
        let (mut child, kill_calls, _wait_calls) = tracking_child(false);
        child.wait().await.expect("reap the completed root");
        assert_eq!(child.id(), None, "tokio retires the id once it reaps");

        drop(SupervisedChildGuard::arm(child));

        assert_eq!(kill_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_guard_without_a_root_id_is_not_signalled() {
        let (child, kill_calls, wait_calls) = pending_child(None);

        drop(SupervisedChildGuard::arm(child));

        assert_eq!(kill_calls.load(Ordering::SeqCst), 0);
        assert_eq!(wait_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_dropped_guard_falls_back_to_the_root_and_never_waits() {
        let (child, kill_calls, wait_calls) = pending_child(Some(NONEXISTENT_PROCESS_ID));

        drop(SupervisedChildGuard::arm(child));

        // The ownership-unit kill fails, so the root is killed directly; the
        // child never exits, so any wait would hang this test.
        assert_eq!(kill_calls.load(Ordering::SeqCst), 2);
        assert_eq!(wait_calls.load(Ordering::SeqCst), 0);
    }

    /// This isolated subreaper adopts the double-forked helper as soon as its
    /// intermediate shell exits, so only this process can reap it. A parent
    /// shell can reap a killed helper while waiting for any child, even `mv`
    /// after publishing the PIDs, leaving the adopter nothing to observe.
    /// Killing the adopted helper proves the guard signals the whole process
    /// group; tokio must also reap the root.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_dropped_run_kills_its_process_group_and_leaves_no_zombie_root() {
        use crate::test_support::{reexec, run_on_current_thread};

        const TEST: &str = "process::supervised::tests::a_dropped_run_kills_its_process_group_and_leaves_no_zombie_root";
        let Some(phase) = reexec::enter(TEST, "subreaper") else {
            reexec::run(TEST, "subreaper", None, |_| {});
            return;
        };
        // SAFETY: prctl has no pointer arguments here and changes only this
        // isolated test process.
        assert_eq!(unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) }, 0);
        let directory = tempfile::tempdir().expect("subreaper fixture");
        let pids_path = directory.path().join("pids");

        run_on_current_thread(async {
            let mut command = Command::new("sh");
            command
                .args([
                    "-c",
                    "helper=$(sleep 30 >/dev/null 2>&1 & echo $!); printf '%s %s' \"$$\" \"$helper\" > \"$1.tmp\" && mv \"$1.tmp\" \"$1\"; exec sleep 30",
                    "tree",
                ])
                .arg(&pids_path)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let request = SupervisedRunRequest {
                command,
                stdin: None,
                timeout: Duration::from_secs(30),
                cleanup_timeout: PROCESS_CLEANUP_WAIT_TIMEOUT,
                max_output_bytes: 1024,
                overflow: SupervisedOverflow::Error,
            };
            let cancellation = CancellationToken::new();
            let mut run = Box::pin(run_supervised(request, &cancellation));
            tokio::select! {
                () = async {
                    while !pids_path.exists() {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                } => {}
                result = &mut run => panic!("the tree must still be running, got {result:?}"),
            }
            let pids = std::fs::read_to_string(&pids_path).expect("fixture PIDs");
            let (root, helper) = pids.split_once(' ').expect("root and helper PIDs");
            let root = root.parse::<libc::pid_t>().expect("numeric root PID");
            let helper = helper.parse::<libc::pid_t>().expect("numeric helper PID");

            drop(run);

            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let helper_status = loop {
                let mut status = 0;
                // SAFETY: waitpid targets only the fixture helper and writes a
                // valid status. The helper becomes this process's child once its
                // intermediate shell finishes exiting; until then waitpid may
                // report ECHILD.
                let waited = unsafe { libc::waitpid(helper, &mut status, libc::WNOHANG) };
                if waited == helper {
                    break status;
                }
                let error = io::Error::last_os_error();
                assert!(
                    waited == 0 || error.raw_os_error() == Some(libc::ECHILD),
                    "waitpid for the helper failed: {error}"
                );
                assert!(
                    std::time::Instant::now() < deadline,
                    "the helper outlived the dropped run"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            };
            assert!(
                libc::WIFSIGNALED(helper_status) && libc::WTERMSIG(helper_status) == libc::SIGKILL,
                "the process-group kill must end the helper, got status {helper_status}"
            );

            let root_process = std::path::PathBuf::from(format!("/proc/{root}"));
            while root_process.exists() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "tokio never reaped the dropped run's root"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        phase.complete();
    }

    #[tokio::test]
    async fn a_panic_while_the_guard_is_armed_terminates_the_child() {
        let (child, kill_calls, _wait_calls) = tracking_child(false);

        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _guard = SupervisedChildGuard::arm(child);
            panic!("supervised caller panicked");
        }));

        assert!(unwound.is_err());
        assert_eq!(kill_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn output_collection_enforces_or_records_overflow() {
        let error = collect_output(&b"abcdef"[..], "stdout", 3, SupervisedOverflow::Error)
            .await
            .expect_err("strict output should fail");
        assert!(matches!(
            error,
            SupervisedRunError::OutputLimit {
                stream: "stdout",
                max_bytes: 3,
                observed_bytes: 6,
            }
        ));

        let truncated = collect_output(&b"abcdef"[..], "stderr", 4, SupervisedOverflow::Truncate)
            .await
            .expect("truncate output should complete");
        assert_eq!(truncated.bytes, b"abcd");
        assert_eq!(truncated.observed_bytes, 6);
        assert!(truncated.truncated());
    }

    #[test]
    fn output_collection_future_keeps_read_buffer_off_stack() {
        let future = collect_output(&b""[..], "stdout", 1, SupervisedOverflow::Error);
        let future_size = std::mem::size_of_val(&future);

        assert!(
            future_size < PROCESS_OUTPUT_READ_BUFFER_BYTES,
            "output collection future retained an inline 8 KiB read buffer ({future_size} bytes)"
        );
    }

    #[cfg(unix)]
    fn shell_request(script: &str, timeout: Duration) -> SupervisedRunRequest {
        let mut command = Command::new("sh");
        command
            .args(["-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        SupervisedRunRequest {
            command,
            stdin: None,
            timeout,
            cleanup_timeout: PROCESS_CLEANUP_WAIT_TIMEOUT,
            max_output_bytes: 1024,
            overflow: SupervisedOverflow::Truncate,
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn only_a_signal_bibcode_did_not_send_reports_a_command_crash() {
        let captured = crate::diagnostics::profiler::exits::CapturedExits::start();
        let cancellation = CancellationToken::new();
        let killed = run_supervised(
            shell_request("kill -USR1 $$", Duration::from_secs(10)),
            &cancellation,
        )
        .await
        .expect("signalled run completes");
        assert!(!killed.status.success());
        let failed = run_supervised(
            shell_request("exit 3", Duration::from_secs(10)),
            &cancellation,
        )
        .await
        .expect("failed run completes");
        assert_eq!(failed.status.code(), Some(3));
        let timed_out = run_supervised(
            shell_request("sleep 30", Duration::from_millis(200)),
            &cancellation,
        )
        .await;
        assert!(matches!(timed_out, Err(SupervisedRunError::Timeout)));

        assert_eq!(captured.take(), [["command", "sh", "SIGUSR1"]]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stdin_failure_cleans_up_before_returning() {
        let mut command = Command::new("sh");
        command
            .args(["-c", "exec 0<&-; sleep 30"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let cancellation = CancellationToken::new();
        let error = run_supervised(
            SupervisedRunRequest {
                command,
                stdin: Some(vec![b'x'; 1024 * 1024]),
                timeout: Duration::from_secs(5),
                cleanup_timeout: PROCESS_CLEANUP_WAIT_TIMEOUT,
                max_output_bytes: 1024,
                overflow: SupervisedOverflow::Error,
            },
            &cancellation,
        )
        .await
        .expect_err("closed stdin should fail");
        assert!(matches!(error, SupervisedRunError::Stdin(_)));
    }
}
