use std::{
    borrow::Cow,
    collections::BTreeMap,
    env,
    ffi::OsStr,
    fmt,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, mpsc},
    thread,
    time::Duration,
};

#[cfg(windows)]
use portable_pty::SlavePty;
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
#[cfg(windows)]
use std::{ffi::OsString, os::windows::ffi::OsStringExt};
use tokio::sync::{broadcast, watch};

use crate::diagnostics::{NativeProcessSampler, ProcessIdentity};
use crate::process::{
    Platform, launch_executable_extensions, locate_executable, wrap_launch_program,
    wrap_windows_batch_command,
};
use crate::terminal::model::WINDOWS_CONSOLE_THEME_ENV;
#[cfg(any(windows, test))]
use crate::terminal::model::{
    TerminalConsoleTheme as WindowsConsoleTheme,
    terminal_console_theme_from_env as windows_console_theme_from_env,
};
use crate::terminal::osc::{
    OSC_BACKGROUND_ENV, OSC_CURSOR_ENV, OSC_FOREGROUND_ENV, OscColorResponder, colors_from_env,
    is_reserved_osc_env_key,
};

#[cfg(windows)]
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

type SharedPtyWriter = Arc<Mutex<Box<dyn Write + Send>>>;

#[cfg(windows)]
use crate::process::WindowsJob;

const DEFAULT_TERMINAL_TYPE: &str = "xterm-256color";

#[cfg(windows)]
fn windows_system_cmd_path() -> Result<PathBuf, String> {
    const MAX_WINDOWS_PATH_UNITS: usize = 32_768;
    let mut buffer = vec![0_u16; MAX_WINDOWS_PATH_UNITS];
    // SAFETY: `buffer` is writable for the exact capacity passed to the API.
    let length = unsafe {
        GetSystemDirectoryW(
            buffer.as_mut_ptr(),
            u32::try_from(buffer.len()).expect("Windows path buffer fits u32"),
        )
    };
    if length == 0 {
        return Err(format!(
            "failed to resolve the trusted Windows system directory: {}",
            std::io::Error::last_os_error()
        ));
    }
    let length = usize::try_from(length)
        .map_err(|_| "Windows system directory length exceeded usize".to_owned())?;
    if length >= buffer.len() {
        return Err(format!(
            "Windows system directory exceeded the supported path length: {length}"
        ));
    }
    if length == 0 || buffer[..length].contains(&0) {
        return Err("Windows system directory returned an invalid path".to_owned());
    }
    let directory = PathBuf::from(OsString::from_wide(&buffer[..length]));
    if !directory.is_absolute() || !directory.is_dir() {
        return Err(format!(
            "Windows system directory is not an absolute directory: {}",
            directory.display()
        ));
    }
    let command = directory.join("cmd.exe");
    if !command.is_file() {
        return Err(format!(
            "trusted Windows command interpreter was not found: {}",
            command.display()
        ));
    }
    Ok(command)
}

#[cfg(windows)]
fn build_windows_console_theme_initializer_command(
    theme: WindowsConsoleTheme,
) -> Result<CommandBuilder, String> {
    let color_code = match theme {
        WindowsConsoleTheme::Light => "F0",
        WindowsConsoleTheme::Dark => "0F",
    };
    let mut command = CommandBuilder::new(windows_system_cmd_path()?);
    command.raw_windows_args(format!("/d /c color {color_code}"));
    Ok(command)
}

#[cfg(windows)]
fn initialize_windows_console_theme(
    slave: &dyn SlavePty,
    theme: WindowsConsoleTheme,
) -> Result<(), String> {
    let command = build_windows_console_theme_initializer_command(theme)?;
    let child = slave
        .spawn_command(command)
        .map_err(|error| format!("failed to start Windows console theme initializer: {error}"))?;
    wait_for_windows_console_theme_initializer(child)
}

#[cfg(any(windows, test))]
fn wait_for_windows_console_theme_initializer(
    child: Box<dyn portable_pty::Child + Send + Sync>,
) -> Result<(), String> {
    let mut child = SpawnedChildGuard::new(child);
    let status = child.child_mut().wait().map_err(|error| {
        format!("failed to wait for Windows console theme initializer: {error}")
    })?;
    if !status.success() {
        return Err(format!(
            "Windows console theme initializer exited unsuccessfully: {status:?}"
        ));
    }
    child.disarm_child();
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PtyExit {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
}

#[derive(Clone, Debug)]
pub struct PtySpawnInput {
    pub executable: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub cols: u16,
    pub rows: u16,
    pub env: BTreeMap<String, String>,
}

pub trait PtyProcess: fmt::Debug + Send + Sync {
    fn pid(&self) -> u32;
    /// Returns the stable process identity captured by the backend at spawn.
    /// A live PTY must provide this identity before the manager can publish it.
    fn process_identity(&self) -> Option<ProcessIdentity>;
    fn osc_color_responder_active(&self) -> bool {
        false
    }
    fn write(&self, data: &str) -> Result<(), String>;
    fn resize(&self, cols: u16, rows: u16) -> Result<(), String>;
    fn kill(&self) -> Result<(), String>;
    fn wait_for_process_tree_exit(&self, _timeout: Duration) -> Result<Option<bool>, String> {
        Ok(None)
    }
    fn subscribe_output(&self) -> broadcast::Receiver<String>;
    fn subscribe_exit(&self) -> watch::Receiver<Option<PtyExit>>;
    /// The exit as `std` reports it, signal number included, once the root has exited. A
    /// published [`PtyExit`] keeps the backend's own code. `None` while it runs, or when the
    /// backend cannot tell.
    fn exit_status(&self) -> Option<std::process::ExitStatus> {
        None
    }
}

pub trait PtyBackend: fmt::Debug + Send + Sync {
    fn spawn(&self, input: &PtySpawnInput) -> Result<Arc<dyn PtyProcess>, String>;
}

#[derive(Debug, Default)]
pub struct PortablePtyBackend;

struct PreparedPtyCommand {
    command: CommandBuilder,
    #[cfg(windows)]
    job: WindowsJob,
}

struct SpawnedChildGuard {
    child: Option<Box<dyn portable_pty::Child + Send + Sync>>,
    root_killer: Option<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
    #[cfg(unix)]
    root_process_id: Option<i32>,
    #[cfg(unix)]
    process_group: Option<i32>,
    #[cfg(windows)]
    job: Option<Arc<WindowsJob>>,
    #[cfg(test)]
    tree_cleanup_observer: Option<Box<dyn FnOnce() + Send>>,
}

impl SpawnedChildGuard {
    #[cfg(any(not(unix), test))]
    fn new(child: Box<dyn portable_pty::Child + Send + Sync>) -> Self {
        Self {
            child: Some(child),
            root_killer: None,
            #[cfg(unix)]
            root_process_id: None,
            #[cfg(unix)]
            process_group: None,
            #[cfg(windows)]
            job: None,
            #[cfg(test)]
            tree_cleanup_observer: None,
        }
    }

    #[cfg(unix)]
    fn new_with_process_group(
        child: Box<dyn portable_pty::Child + Send + Sync>,
        process_group: Option<i32>,
    ) -> Self {
        Self {
            child: Some(child),
            root_killer: None,
            root_process_id: None,
            process_group,
            #[cfg(test)]
            tree_cleanup_observer: None,
        }
    }

    fn child(&self) -> &(dyn portable_pty::Child + Send + Sync) {
        self.child.as_deref().expect("spawned child guard")
    }

    fn child_mut(&mut self) -> &mut (dyn portable_pty::Child + Send + Sync) {
        self.child.as_deref_mut().expect("spawned child guard")
    }

    fn handoff_child_to_waiter(&mut self) -> Box<dyn portable_pty::Child + Send + Sync> {
        let root_killer = self.child().clone_killer();
        let child = self.child.take().expect("spawned child guard");
        self.root_killer = Some(root_killer);
        child
    }

    #[cfg(any(windows, test))]
    fn disarm_child(mut self) {
        self.child.take();
    }

    #[cfg(unix)]
    fn remember_root_process_id(&mut self, process_id: u32) -> Result<(), String> {
        let process_id = i32::try_from(process_id)
            .map_err(|_| "PTY child process id exceeded the Unix PID range".to_owned())?;
        self.root_process_id = Some(process_id);
        Ok(())
    }

    #[cfg(test)]
    fn observe_tree_cleanup(&mut self, observer: impl FnOnce() + Send + 'static) {
        self.tree_cleanup_observer = Some(Box::new(observer));
    }

    #[cfg(unix)]
    fn commit_process_group(mut self) -> Option<i32> {
        self.root_killer.take();
        self.root_process_id.take();
        self.process_group.take()
    }

    #[cfg(windows)]
    fn own_job(&mut self, job: WindowsJob) {
        debug_assert!(self.job.is_none(), "PTY child job already attached");
        self.job = Some(Arc::new(job));
    }

    #[cfg(windows)]
    fn clone_job_for_waiter(&self) -> Result<Arc<WindowsJob>, String> {
        self.job
            .as_ref()
            .cloned()
            .ok_or_else(|| "PTY child job was not retained during setup".to_owned())
    }

    #[cfg(windows)]
    fn commit_job(mut self) -> Result<Arc<WindowsJob>, String> {
        let job = self
            .job
            .take()
            .ok_or_else(|| "PTY child job was not retained during setup".to_owned())?;
        self.root_killer.take();
        Ok(job)
    }
}

type PtyThreadTask = Box<dyn FnOnce() + Send + 'static>;

fn spawn_pty_thread_with(
    name: String,
    task: PtyThreadTask,
    spawn: impl FnOnce(thread::Builder, PtyThreadTask) -> std::io::Result<thread::JoinHandle<()>>,
) -> std::io::Result<()> {
    spawn(thread::Builder::new().name(name), task).map(drop)
}

fn spawn_pty_thread(name: String, task: PtyThreadTask) -> std::io::Result<()> {
    spawn_pty_thread_with(name, task, |builder, task| builder.spawn(task))
}

impl Drop for SpawnedChildGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(process_group) = self.process_group
            && let Err(error) = kill_unix_process_group(process_group)
        {
            tracing::debug!(%error, process_group, "failed to kill PTY process group after spawn setup failure");
        }
        #[cfg(windows)]
        if let Some(job) = self.job.as_ref()
            && let Err(error) = job.terminate()
        {
            tracing::debug!(%error, "failed to kill PTY job after spawn setup failure");
        }
        #[cfg(test)]
        if let Some(observer) = self.tree_cleanup_observer.take() {
            observer();
        }
        if let Some(child) = self.child.as_mut() {
            if let Err(error) = child.kill() {
                tracing::debug!(%error, "failed to kill PTY child after spawn setup failure");
            }
            return;
        }
        #[cfg(unix)]
        let root_killed = self.root_process_id.is_some_and(|process_id| {
            if let Err(error) = kill_unix_process(process_id) {
                tracing::debug!(%error, process_id, "failed to SIGKILL PTY root after waiter spawn failure");
                false
            } else {
                true
            }
        });
        #[cfg(not(unix))]
        let root_killed = false;
        if !root_killed
            && let Some(root_killer) = self.root_killer.as_mut()
            && let Err(error) = root_killer.kill()
        {
            tracing::debug!(%error, "failed to kill PTY root after waiter spawn failure");
        }
    }
}

/// Collapses a burst of queued resizes down to the newest one.
///
/// A window drag emits many sizes while a single `master.resize` is still in
/// flight — on Windows that call is a cross-process `ResizePseudoConsole` and
/// takes milliseconds. Only the newest size describes the terminal the user is
/// actually looking at, so intermediate sizes are skipped rather than replayed.
/// Skipping them must never skip the last one: a PTY left at an intermediate
/// size strands every full-screen TUI that anchors to the reported row count.
fn coalesce_pending_resizes(first: PtySize, requests: &mpsc::Receiver<PtySize>) -> PtySize {
    let mut newest = first;
    while let Ok(size) = requests.try_recv() {
        newest = size;
    }
    newest
}

impl PortablePtyBackend {
    fn spawn_command(
        &self,
        input: &PtySpawnInput,
        prepared: PreparedPtyCommand,
    ) -> Result<Arc<dyn PtyProcess>, String> {
        self.spawn_command_with_waiter(
            input,
            prepared,
            spawn_pty_thread,
            #[cfg(all(test, unix))]
            |_| {},
            #[cfg(all(test, unix))]
            |_| {},
        )
    }

    fn spawn_command_with_waiter(
        &self,
        input: &PtySpawnInput,
        prepared: PreparedPtyCommand,
        spawn_waiter: impl FnOnce(String, PtyThreadTask) -> std::io::Result<()>,
        #[cfg(all(test, unix))] before_identity_probe: impl FnOnce(u32),
        #[cfg(all(test, unix))] observe_waiter_reservation: impl FnOnce(std::io::Result<()>)
        + Send
        + 'static,
    ) -> Result<Arc<dyn PtyProcess>, String> {
        let command = prepared.command;
        #[cfg(windows)]
        let job = prepared.job;
        let pair = match native_pty_system().openpty(PtySize {
            rows: input.rows,
            cols: input.cols,
            pixel_width: 0,
            pixel_height: 0,
        }) {
            Ok(pair) => pair,
            Err(error) => return Err(error.to_string()),
        };

        #[cfg(windows)]
        if let Some(theme) = windows_console_theme_from_env(&input.env) {
            initialize_windows_console_theme(pair.slave.as_ref(), theme)?;
        }

        let child = match pair.slave.spawn_command(command) {
            Ok(child) => child,
            Err(error) => return Err(error.to_string()),
        };
        #[cfg(unix)]
        let mut child =
            SpawnedChildGuard::new_with_process_group(child, pair.master.process_group_leader());
        #[cfg(not(unix))]
        let mut child = SpawnedChildGuard::new(child);
        #[cfg(windows)]
        child.own_job(job);
        drop(pair.slave);
        let pid = match child.child().process_id() {
            Some(pid) => pid,
            None => return Err("PTY child did not expose a process id".to_string()),
        };
        #[cfg(all(test, unix))]
        before_identity_probe(pid);
        let process_identity = retain_captured_identity_if_child_live(
            child.child_mut(),
            NativeProcessSampler::process_identity(pid).ok(),
        );
        #[cfg(unix)]
        child.remember_root_process_id(pid)?;
        let mut reader = match pair.master.try_clone_reader() {
            Ok(reader) => reader,
            Err(error) => return Err(error.to_string()),
        };
        let writer: SharedPtyWriter = match pair.master.take_writer() {
            Ok(writer) => Arc::new(Mutex::new(writer)),
            Err(error) => return Err(error.to_string()),
        };
        #[cfg(not(windows))]
        let killer = child.child().clone_killer();
        // Reads the manager has not published yet. A program printing thousands of short lines
        // writes faster than one read per publication; 256 slots overflowed within
        // milliseconds and dropped output from the terminal. The manager now publishes the
        // reads waiting together, and 1024 slots of at most 8 KiB bound a stalled backlog at
        // 8 MiB.
        let (output, initial_output) = broadcast::channel(1024);
        let (exit, _) = watch::channel(None);
        let (resize, resize_requests) = mpsc::channel();

        // Answer OSC color queries (e.g. OpenCode's OSC 11 light/dark probe) at
        // the PTY layer, using the app's resolved theme colors, so detection
        // does not depend on the client's slower round-trip reply.
        let osc_responder = {
            let colors = colors_from_env(&input.env);
            (!colors.is_empty()).then(|| (OscColorResponder::new(colors), Arc::clone(&writer)))
        };
        let osc_color_responder_active = osc_responder.is_some();

        let output_sender = output.clone();
        if let Err(error) = thread::Builder::new()
            .name(format!("coddy-studio-pty-output-{pid}"))
            .spawn(move || read_output(&mut reader, &output_sender, osc_responder))
        {
            return Err(error.to_string());
        }
        if let Err(error) = thread::Builder::new()
            .name(format!("coddy-studio-pty-resize-{pid}"))
            .spawn(move || {
                while let Ok(size) = resize_requests.recv() {
                    let size = coalesce_pending_resizes(size, &resize_requests);
                    if let Err(error) = pair.master.resize(size) {
                        // A dropped resize strands full-screen TUIs at a stale
                        // row count, so make the failure visible instead of
                        // leaving the PTY silently out of sync with the client.
                        tracing::warn!(
                            %error,
                            pid,
                            cols = size.cols,
                            rows = size.rows,
                            "failed to resize PTY"
                        );
                    }
                }
            })
        {
            return Err(error.to_string());
        }
        let exit_sender = exit.clone();
        #[cfg(windows)]
        let waiter_job = child.clone_job_for_waiter()?;
        #[cfg(unix)]
        let process_group = child.process_group;
        #[cfg(unix)]
        let process_group_ownership = Arc::new(Mutex::new(if process_group.is_some() {
            UnixProcessGroupOwnership::Reserved
        } else {
            UnixProcessGroupOwnership::Released
        }));
        #[cfg(unix)]
        let waiter_process_group_ownership = Arc::clone(&process_group_ownership);
        let wait_child = child.handoff_child_to_waiter();
        let exit_status = Arc::new(OnceLock::new());
        let waiter_exit_status = Arc::clone(&exit_status);
        spawn_waiter(
            format!("coddy-studio-pty-wait-{pid}"),
            Box::new(move || {
                let mut child = wait_child;
                // portable-pty names a signal but not its number, so the exit record is read
                // first, while reserving the root when it leads a process group.
                #[cfg(unix)]
                let mut exit_record = None;
                // A root that died before its group was read has none to reserve.
                #[cfg(unix)]
                if process_group.is_none() {
                    exit_record = wait_for_unix_child_exit_without_reaping(pid).ok().flatten();
                }
                #[cfg(unix)]
                if let Some(process_group) = process_group {
                    let reservation = wait_for_unix_child_exit_without_reaping(pid);
                    if let Err(error) = &reservation {
                        tracing::warn!(%error, pid, "failed to reserve PTY root identity through group cleanup");
                    }
                    exit_record = reservation.as_ref().ok().copied().flatten();
                    #[cfg(test)]
                    observe_waiter_reservation(reservation.map(|_| ()));
                    loop {
                        match kill_reserved_unix_process_group_after_root_exit(
                            process_group,
                            &waiter_process_group_ownership,
                        ) {
                            Ok(()) => break,
                            Err(error) => {
                                tracing::warn!(%error, process_group, "failed to terminate PTY process group after root exit; retaining ownership for retry");
                                std::thread::sleep(Duration::from_millis(10));
                            }
                        }
                    }
                }
                let event = match child.wait() {
                    Ok(status) => {
                        #[cfg(unix)]
                        let precise = exit_record;
                        #[cfg(windows)]
                        let precise = {
                            use std::os::windows::process::ExitStatusExt;
                            Some(std::process::ExitStatus::from_raw(status.exit_code()))
                        };
                        // Set before the exit is published, so whoever sees the exit can read it.
                        if let Some(precise) = precise {
                            let _ = waiter_exit_status.set(precise);
                        }
                        PtyExit {
                            exit_code: i32::try_from(status.exit_code()).ok(),
                            signal: None,
                        }
                    }
                    Err(_) => PtyExit {
                        exit_code: None,
                        signal: None,
                    },
                };
                #[cfg(windows)]
                {
                    if let Err(error) = waiter_job.terminate() {
                        tracing::warn!(%error, "failed to terminate PTY Job after root exit");
                    }
                    match waiter_job.wait_for_exit(Duration::from_secs(3)) {
                        Ok(true) => {}
                        Ok(false) => tracing::warn!("PTY Job did not exit before ownership release"),
                        Err(error) => tracing::warn!(%error, "failed waiting for PTY Job cleanup"),
                    }
                    // `portable_pty::WinChild` owns the root process handle. Release it
                    // before publishing the exit notification so shutdown callers cannot
                    // observe a completed Job while the exited PID is still queryable.
                    drop(child);
                    drop(waiter_job);
                }
                // Completion must survive both initial subscription latency and
                // the receiver gap when manager shutdown replaces its watcher.
                exit_sender.send_replace(Some(event));
            }),
        )
        .map_err(|error| error.to_string())?;
        #[cfg(unix)]
        let process_group = child.commit_process_group();
        #[cfg(windows)]
        let job = child.commit_job()?;
        Ok(Arc::new(PortablePtyProcess {
            pid,
            process_identity,
            osc_color_responder_active,
            resize,
            writer,
            #[cfg(not(windows))]
            killer: Mutex::new(killer),
            output,
            initial_output: Mutex::new(Some(initial_output)),
            exit,
            exit_status,
            #[cfg(unix)]
            process_group,
            #[cfg(unix)]
            process_group_ownership,
            #[cfg(windows)]
            job: Mutex::new(Some(job)),
        }))
    }
}

impl PtyBackend for PortablePtyBackend {
    fn spawn(&self, input: &PtySpawnInput) -> Result<Arc<dyn PtyProcess>, String> {
        let prepared = build_pty_command(input)?;
        self.spawn_command(input, prepared)
    }
}

fn build_pty_command(input: &PtySpawnInput) -> Result<PreparedPtyCommand, String> {
    let platform = Platform::current();
    let executable = resolve_pty_executable_for_launch(platform, input)?;
    let command = build_pty_command_from_launch(
        platform,
        input,
        prepare_pty_launch(platform, input, &executable)?,
    );
    #[cfg(windows)]
    {
        let mut command = command;
        let job = WindowsJob::new().map_err(|error| error.to_string())?;
        command.job_list(&[job.raw_handle()]);
        Ok(PreparedPtyCommand { command, job })
    }
    #[cfg(not(windows))]
    {
        Ok(PreparedPtyCommand { command })
    }
}

#[cfg(test)]
fn build_pty_command_on(
    platform: Platform,
    input: &PtySpawnInput,
) -> Result<CommandBuilder, String> {
    let executable = resolve_pty_executable_for_launch(platform, input)?;
    Ok(build_pty_command_from_launch(
        platform,
        input,
        prepare_pty_launch(platform, input, &executable)?,
    ))
}

fn resolve_pty_executable_for_launch(
    platform: Platform,
    input: &PtySpawnInput,
) -> Result<PathBuf, String> {
    match resolve_pty_executable_on(platform, &input.executable, &input.cwd, &input.env) {
        Some(executable) => Ok(executable),
        None => Err(format!(
            "terminal executable was not found: {}",
            input.executable
        )),
    }
}

fn prepare_pty_launch(
    platform: Platform,
    input: &PtySpawnInput,
    executable: &Path,
) -> Result<crate::process::PreparedLaunch, String> {
    let target = wrap_launch_program(platform, executable)?.prepare(&input.args);
    wrap_windows_batch_command(platform, target, &input.env)
}

fn build_pty_command_from_launch(
    platform: Platform,
    input: &PtySpawnInput,
    launch: crate::process::PreparedLaunch,
) -> CommandBuilder {
    let mut command = CommandBuilder::new(launch.program);
    command.args(launch.args);
    #[cfg(windows)]
    if let Some(raw_windows_args) = launch.raw_windows_args {
        command.raw_windows_args(raw_windows_args);
    }
    command.cwd(&input.cwd);
    // Interactive agent TUIs inherit the host process environment. Cursor and
    // other agent shells often export NO_COLOR=1 / FORCE_COLOR=0, which chalk
    // and Claude Code honor by emitting no ANSI colours at all. Strip those
    // and force colour support for PTY sessions unless the launch env
    // explicitly opts out.
    ensure_pty_color_environment(&mut command, &input.env);
    if !input.env.keys().any(|key| key.eq_ignore_ascii_case("TERM")) {
        command.env("TERM", DEFAULT_TERMINAL_TYPE);
    }
    for (key, value) in &input.env {
        // Internal terminal theme values configure PTY setup and query
        // responses. Windows environment keys are case-insensitive.
        if is_reserved_pty_env_key_on(platform, key) {
            continue;
        }
        command.env(key, value);
    }
    crate::process::isolate_appimage_environment(&mut command);
    command
}

/// Ensures PTY children can emit colours even when the Coddy Studio host was
/// launched from a NO_COLOR / FORCE_COLOR=0 environment.
fn ensure_pty_color_environment(
    command: &mut CommandBuilder,
    launch_env: &BTreeMap<String, String>,
) {
    let launch_disables_color = launch_env.iter().any(|(key, value)| {
        key.eq_ignore_ascii_case("NO_COLOR") && !value.is_empty()
            || key.eq_ignore_ascii_case("FORCE_COLOR") && value == "0"
    });
    if launch_disables_color {
        return;
    }
    command.env_remove("NO_COLOR");
    if !launch_env
        .keys()
        .any(|key| key.eq_ignore_ascii_case("FORCE_COLOR"))
    {
        command.env("FORCE_COLOR", "3");
    }
    if !launch_env
        .keys()
        .any(|key| key.eq_ignore_ascii_case("COLORTERM"))
    {
        command.env("COLORTERM", "truecolor");
    }
}

fn is_reserved_pty_env_key_on(platform: Platform, key: &str) -> bool {
    if platform == Platform::Windows {
        return [
            WINDOWS_CONSOLE_THEME_ENV,
            OSC_BACKGROUND_ENV,
            OSC_FOREGROUND_ENV,
            OSC_CURSOR_ENV,
        ]
        .into_iter()
        .any(|reserved| key.eq_ignore_ascii_case(reserved));
    }
    key == WINDOWS_CONSOLE_THEME_ENV || is_reserved_osc_env_key(key)
}

fn resolve_pty_executable_on(
    platform: Platform,
    command: &str,
    cwd: &Path,
    overrides: &BTreeMap<String, String>,
) -> Option<PathBuf> {
    let path = overrides
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("PATH"))
        .map(|(_, value)| value.clone())
        .or_else(|| env::var("PATH").ok());

    let path_extensions = if platform == Platform::Windows {
        overrides
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("PATHEXT"))
            .map(|(_, value)| value.clone())
            .or_else(|| env::var("PATHEXT").ok())
    } else {
        None
    };
    let extensions = launch_executable_extensions(platform, path_extensions.as_deref());

    locate_executable(
        command,
        Some(cwd),
        path.as_deref().map(OsStr::new),
        &extensions,
    )
}

fn decode_pty_output(pending: &mut Vec<u8>, bytes: &[u8], end_of_stream: bool) -> String {
    pending.extend_from_slice(bytes);
    let mut output = String::with_capacity(pending.len());
    let mut consumed = 0;

    while consumed < pending.len() {
        let undecoded = &pending[consumed..];
        match std::str::from_utf8(undecoded) {
            Ok(text) => {
                output.push_str(text);
                consumed = pending.len();
            }
            Err(error) => {
                let valid_len = error.valid_up_to();
                if valid_len > 0 {
                    let valid = std::str::from_utf8(&undecoded[..valid_len])
                        .expect("Utf8Error::valid_up_to must identify valid UTF-8");
                    output.push_str(valid);
                    consumed += valid_len;
                }

                match error.error_len() {
                    Some(invalid_len) => {
                        output.push('\u{fffd}');
                        consumed += invalid_len;
                    }
                    None if end_of_stream => {
                        output.push('\u{fffd}');
                        consumed = pending.len();
                    }
                    None => break,
                }
            }
        }
    }

    if consumed > 0 {
        pending.drain(..consumed);
    }
    output
}

fn read_output(
    reader: &mut dyn Read,
    sender: &broadcast::Sender<String>,
    mut osc_responder: Option<(OscColorResponder, SharedPtyWriter)>,
) {
    let mut buffer = [0u8; 8 * 1024];
    let mut pending = Vec::with_capacity(4);
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => {
                let text = decode_pty_output(&mut pending, &[], true);
                if !text.is_empty() {
                    let _ = sender.send(text);
                }
                return;
            }
            Ok(read) => {
                answer_osc_color_queries(osc_responder.as_mut(), &buffer[..read]);
                let text = decode_pty_output(&mut pending, &buffer[..read], false);
                if !text.is_empty() {
                    let _ = sender.send(text);
                }
            }
        }
    }
}

/// Feeds raw output bytes to the OSC responder and writes any reply back to the
/// PTY input. Runs on the raw byte stream so split queries are tracked before
/// UTF-8 decoding; write failures are ignored because a lost reply only leaves
/// the provider on its default theme.
fn answer_osc_color_queries(
    responder: Option<&mut (OscColorResponder, SharedPtyWriter)>,
    bytes: &[u8],
) {
    let Some((responder, writer)) = responder else {
        return;
    };
    let reply = responder.process(bytes);
    if reply.is_empty() {
        return;
    }
    if let Ok(mut writer) = writer.lock() {
        let _ = writer.write_all(&reply);
        let _ = writer.flush();
    }
}

/// Removes DEC private mode 1004 focus reports (`ESC [ I` focus-in, `ESC [ O`
/// focus-out) from terminal input.
///
/// On Windows, ConPTY unconditionally advertises focus tracking to the host
/// terminal at pseudoconsole startup, so xterm enables focus reporting and emits
/// one of these on every panel focus change. ConPTY's console-input translation
/// does not round-trip them back to the child faithfully, so provider TUIs (for
/// example Codex) insert them as literal `[I` / `[O` text in their input line.
/// They carry no value for the embedded terminals, so the Windows write path
/// drops them. Real PTYs on other platforms are left untouched, so a TUI that
/// explicitly requested focus events there still receives them.
///
/// Only the bare sequences are removed: a parameterised CSI such as `ESC [ 2 I`
/// (cursor horizontal tab) is not a focus report and is preserved.
#[cfg_attr(not(windows), allow(dead_code))]
fn strip_focus_reports(data: &str) -> Cow<'_, str> {
    // Fast path: both focus reports begin with ESC, so input without an ESC
    // (the common keystroke case) needs no scanning or allocation.
    if !data.as_bytes().contains(&0x1b) {
        return Cow::Borrowed(data);
    }
    if !data.contains("\u{1b}[I") && !data.contains("\u{1b}[O") {
        return Cow::Borrowed(data);
    }
    Cow::Owned(data.replace("\u{1b}[I", "").replace("\u{1b}[O", ""))
}

fn retain_captured_identity_if_child_live(
    child: &mut (dyn portable_pty::Child + Send + Sync),
    captured: Option<ProcessIdentity>,
) -> Option<ProcessIdentity> {
    #[cfg(unix)]
    {
        // try_wait reaps an exited Unix child, releasing its PID/PGID before
        // the waiter can terminate the owned group. Keep that root waitable.
        captured.filter(|_| {
            child
                .process_id()
                .is_some_and(|pid| matches!(probe_unix_child_exit_without_reaping(pid), Ok(false)))
        })
    }
    #[cfg(not(unix))]
    captured.filter(|_| matches!(child.try_wait(), Ok(None)))
}

struct PortablePtyProcess {
    pid: u32,
    process_identity: Option<ProcessIdentity>,
    osc_color_responder_active: bool,
    resize: mpsc::Sender<PtySize>,
    writer: SharedPtyWriter,
    #[cfg(not(windows))]
    killer: Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
    output: broadcast::Sender<String>,
    initial_output: Mutex<Option<broadcast::Receiver<String>>>,
    exit: watch::Sender<Option<PtyExit>>,
    exit_status: Arc<OnceLock<std::process::ExitStatus>>,
    #[cfg(unix)]
    process_group: Option<i32>,
    #[cfg(unix)]
    process_group_ownership: Arc<Mutex<UnixProcessGroupOwnership>>,
    #[cfg(windows)]
    job: Mutex<Option<Arc<WindowsJob>>>,
}

impl fmt::Debug for PortablePtyProcess {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PortablePtyProcess")
            .field("pid", &self.pid)
            .finish()
    }
}

impl PtyProcess for PortablePtyProcess {
    fn pid(&self) -> u32 {
        self.pid
    }

    fn process_identity(&self) -> Option<ProcessIdentity> {
        self.process_identity
    }

    fn osc_color_responder_active(&self) -> bool {
        self.osc_color_responder_active
    }

    fn write(&self, data: &str) -> Result<(), String> {
        // On Windows, ConPTY makes xterm emit focus reports that provider TUIs
        // render as literal `[I` / `[O` text; drop them before they reach the
        // pseudoconsole. See strip_focus_reports.
        #[cfg(windows)]
        let owned = strip_focus_reports(data);
        #[cfg(windows)]
        let data: &str = owned.as_ref();
        #[cfg(windows)]
        if data.is_empty() {
            return Ok(());
        }

        let mut writer = match self.writer.lock() {
            Ok(writer) => writer,
            Err(error) => return Err(error.to_string()),
        };
        if let Err(error) = writer.write_all(data.as_bytes()) {
            return Err(error.to_string());
        }
        match writer.flush() {
            Ok(()) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }

    fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        // Never drop a request: callers and `TerminalSession` both record the
        // requested size as applied, so a discarded resize desyncs the PTY from
        // the client permanently. The worker coalesces any backlog instead.
        match self.resize.send(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        }) {
            Ok(()) => Ok(()),
            Err(mpsc::SendError(_)) => Err("PTY resize worker is not available".to_string()),
        }
    }

    fn kill(&self) -> Result<(), String> {
        #[cfg(unix)]
        if let Some(process_group) = self.process_group {
            return kill_reserved_unix_process_group(process_group, &self.process_group_ownership)
                .map_err(|error| error.to_string());
        }
        #[cfg(windows)]
        {
            let job = self
                .job
                .lock()
                .map_err(|error| error.to_string())?
                .as_ref()
                .cloned();
            let Some(job) = job else {
                return Ok(());
            };
            if let Err(error) = job.terminate() {
                return Err(error.to_string());
            }
            Ok(())
        }
        #[cfg(not(windows))]
        {
            self.killer
                .lock()
                .map_err(|error| error.to_string())?
                .kill()
                .map_err(|error| error.to_string())
        }
    }

    #[cfg(windows)]
    fn wait_for_process_tree_exit(&self, timeout: Duration) -> Result<Option<bool>, String> {
        let job = self
            .job
            .lock()
            .map_err(|error| error.to_string())?
            .as_ref()
            .cloned();
        let Some(job) = job else {
            return Ok(Some(true));
        };
        let exited = job
            .wait_for_exit(timeout)
            .map_err(|error| error.to_string())?;
        if exited {
            self.job.lock().map_err(|error| error.to_string())?.take();
        }
        Ok(Some(exited))
    }

    fn subscribe_output(&self) -> broadcast::Receiver<String> {
        self.initial_output
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
            .unwrap_or_else(|| self.output.subscribe())
    }

    fn subscribe_exit(&self) -> watch::Receiver<Option<PtyExit>> {
        self.exit.subscribe()
    }

    fn exit_status(&self) -> Option<std::process::ExitStatus> {
        self.exit_status.get().copied()
    }
}

#[cfg(unix)]
unsafe extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}

#[cfg(all(test, unix))]
unsafe extern "C" {
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
}

#[cfg(unix)]
fn kill_unix_process_group(process_group: i32) -> std::io::Result<()> {
    // Negative PIDs target the complete process group created by the PTY.
    kill_unix_target(-process_group)
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UnixProcessGroupOwnership {
    Reserved,
    Released,
}

#[cfg(unix)]
fn signal_reserved_unix_process_group_with(
    process_group: i32,
    ownership: &Mutex<UnixProcessGroupOwnership>,
    signal: impl FnOnce(i32) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let mut ownership = ownership
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if *ownership == UnixProcessGroupOwnership::Released {
        return Ok(());
    }
    signal(process_group)?;
    *ownership = UnixProcessGroupOwnership::Released;
    Ok(())
}

#[cfg(unix)]
fn kill_reserved_unix_process_group(
    process_group: i32,
    ownership: &Mutex<UnixProcessGroupOwnership>,
) -> std::io::Result<()> {
    signal_reserved_unix_process_group_with(process_group, ownership, kill_unix_process_group)
}

#[cfg(unix)]
fn kill_reserved_unix_process_group_after_root_exit(
    process_group: i32,
    ownership: &Mutex<UnixProcessGroupOwnership>,
) -> std::io::Result<()> {
    signal_reserved_unix_process_group_with(process_group, ownership, |process_group| {
        match kill_unix_process_group(process_group) {
            Ok(()) => Ok(()),
            // Once the owned root is observed exited but kept waitable,
            // EPERM means the group contains no signalable process. Any live
            // descendant of this same-user ownership unit would make the
            // group signal succeed, so there is no owned live member left.
            Err(error) if error.raw_os_error() == Some(libc::EPERM) => Ok(()),
            Err(error) => Err(error),
        }
    })
}

/// Waits for the child to exit without reaping it, and returns its exit status.
#[cfg(unix)]
fn wait_for_unix_child_exit_without_reaping(
    process_id: u32,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    crate::process::child_exit_without_reaping(process_id, 0)
        .map(|exit| exit.as_ref().and_then(crate::process::exit_status))
}

#[cfg(unix)]
fn probe_unix_child_exit_without_reaping(process_id: u32) -> std::io::Result<bool> {
    crate::process::child_exit_without_reaping(process_id, libc::WNOHANG).map(|exit| exit.is_some())
}

#[cfg(unix)]
fn kill_unix_process(process_id: i32) -> std::io::Result<()> {
    kill_unix_target(process_id)
}

#[cfg(unix)]
fn kill_unix_target(target: i32) -> std::io::Result<()> {
    let result = unsafe { kill(target, 9) };
    if result == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(3) {
        Ok(())
    } else {
        Err(error)
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    use std::ffi::OsString;
    #[cfg(not(windows))]
    use std::io::{Error, ErrorKind};
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::time::Duration;

    struct OneByteReader {
        bytes: std::vec::IntoIter<u8>,
    }

    impl Read for OneByteReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let Some(byte) = self.bytes.next() else {
                return Ok(0);
            };
            buffer[0] = byte;
            Ok(1)
        }
    }

    #[test]
    fn pty_output_preserves_utf8_split_across_reads() {
        let expected = "é◆😀─";
        let (sender, mut receiver) = broadcast::channel(32);
        let mut reader = OneByteReader {
            bytes: expected.as_bytes().to_vec().into_iter(),
        };

        read_output(&mut reader, &sender, None);

        let output: String = std::iter::from_fn(|| receiver.try_recv().ok()).collect();
        assert_eq!(output, expected);
    }

    #[test]
    fn strip_focus_reports_drops_standalone_focus_events() {
        // xterm delivers each DEC 1004 focus report as its own write.
        assert_eq!(strip_focus_reports("\u{1b}[I").as_ref(), "");
        assert_eq!(strip_focus_reports("\u{1b}[O").as_ref(), "");
    }

    #[test]
    fn strip_focus_reports_preserves_ordinary_and_other_escape_input() {
        assert_eq!(strip_focus_reports("hello").as_ref(), "hello");
        // Cursor keys and other CSI sequences must pass through untouched.
        assert_eq!(strip_focus_reports("\u{1b}[A").as_ref(), "\u{1b}[A");
        assert_eq!(strip_focus_reports("\u{1b}[D").as_ref(), "\u{1b}[D");
        // CHT with an explicit parameter (ESC [ 2 I) is not a focus report.
        assert_eq!(strip_focus_reports("\u{1b}[2I").as_ref(), "\u{1b}[2I");
        // A bracketed-paste payload that merely contains the bytes is preserved
        // as-is because it is not a bare focus report.
        assert_eq!(
            strip_focus_reports("\u{1b}[200~\u{1b}[201~").as_ref(),
            "\u{1b}[200~\u{1b}[201~"
        );
    }

    #[test]
    fn strip_focus_reports_drops_embedded_and_repeated_focus_events() {
        // The reported symptom: bursts of focus in/out while switching panels.
        assert_eq!(
            strip_focus_reports("\u{1b}[O\u{1b}[O\u{1b}[I\u{1b}[O\u{1b}[I").as_ref(),
            ""
        );
        // Focus reports interleaved with real typed input keep the input.
        assert_eq!(strip_focus_reports("a\u{1b}[Ob\u{1b}[Ic").as_ref(), "abc");
    }

    #[test]
    fn pty_output_replaces_invalid_and_truncated_utf8() {
        let (sender, mut receiver) = broadcast::channel(8);
        let mut reader = OneByteReader {
            bytes: vec![b'a', 0x80, b'b', 0xe2, 0x97].into_iter(),
        };

        read_output(&mut reader, &sender, None);

        let output: String = std::iter::from_fn(|| receiver.try_recv().ok()).collect();
        assert_eq!(output, "a\u{fffd}b\u{fffd}");
    }

    #[cfg(not(unix))]
    #[derive(Clone, Debug)]
    struct FinishedPortableChild;

    #[cfg(not(unix))]
    impl portable_pty::ChildKiller for FinishedPortableChild {
        fn kill(&mut self) -> std::io::Result<()> {
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
            Box::new(self.clone())
        }
    }

    #[cfg(not(unix))]
    impl portable_pty::Child for FinishedPortableChild {
        fn try_wait(&mut self) -> std::io::Result<Option<portable_pty::ExitStatus>> {
            Ok(Some(portable_pty::ExitStatus::with_exit_code(17)))
        }

        fn wait(&mut self) -> std::io::Result<portable_pty::ExitStatus> {
            Ok(portable_pty::ExitStatus::with_exit_code(17))
        }

        fn process_id(&self) -> Option<u32> {
            Some(42)
        }

        #[cfg(windows)]
        fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
            None
        }
    }

    #[cfg(not(unix))]
    #[test]
    fn finished_pty_child_does_not_expose_a_captured_process_identity() {
        let mut child = FinishedPortableChild;
        let captured = ProcessIdentity {
            pid: 42,
            started_at: 100,
        };

        assert_eq!(
            retain_captured_identity_if_child_live(&mut child, Some(captured)),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn exited_unix_child_identity_probe_leaves_status_waitable() {
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 17"])
            .spawn()
            .expect("spawn immediately exiting child");
        let pid = child.id();
        let captured = ProcessIdentity {
            pid,
            started_at: 100,
        };
        wait_for_unix_child_exit_without_reaping(pid).expect("observe exit before probing");

        let exited = probe_unix_child_exit_without_reaping(pid);
        let retained = retain_captured_identity_if_child_live(&mut child, Some(captured));
        let still_waitable = wait_for_unix_child_exit_without_reaping(pid);
        let mut status = 0;
        // SAFETY: this is the test's direct child, observed exited above, and
        // status is writable. Reap before asserting so a failure cannot leak it.
        let reaped = unsafe { libc::waitpid(pid as libc::pid_t, &raw mut status, libc::WNOHANG) };

        assert!(exited.expect("probe exited child"));
        assert_eq!(retained, None, "an exited root must not expose an identity");
        assert!(
            still_waitable.is_ok(),
            "identity probe reaped the root before waiter handoff: {still_waitable:?}"
        );
        assert_eq!(reaped, pid as libc::pid_t);
        assert!(libc::WIFEXITED(status));
        assert_eq!(libc::WEXITSTATUS(status), 17);
    }

    #[cfg(unix)]
    #[test]
    fn live_unix_child_identity_probe_retains_identity_without_reaping() {
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "read -r line; exit 23"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .expect("spawn child gated by stdin");
        let pid = child.id();
        let captured = ProcessIdentity {
            pid,
            started_at: 100,
        };

        let exited = probe_unix_child_exit_without_reaping(pid);
        let retained = retain_captured_identity_if_child_live(&mut child, Some(captured));
        let mut status = 0;
        // SAFETY: this test owns the direct child and writable status buffer.
        let running = unsafe { libc::waitpid(pid as libc::pid_t, &raw mut status, libc::WNOHANG) };
        drop(child.stdin.take());
        let status = child.wait().expect("reap child after releasing stdin");

        assert!(!exited.expect("probe live child"));
        assert_eq!(retained, Some(captured));
        assert_eq!(
            running, 0,
            "probe must leave the live child owned and running"
        );
        assert_eq!(status.code(), Some(23));
    }

    #[cfg(unix)]
    #[test]
    fn portable_backend_immediate_exit_reserves_root_until_waiter_reaps() {
        let input = PtySpawnInput {
            executable: "/bin/sh".to_owned(),
            // Stop only to let spawn capture the process group deterministically.
            // Once released, the command exits immediately before the probe.
            args: vec!["-c".to_owned(), "kill -STOP $$; exit 17".to_owned()],
            cwd: std::env::temp_dir(),
            cols: 80,
            rows: 24,
            env: BTreeMap::new(),
        };
        let prepared = build_pty_command(&input).unwrap();
        let (reservation, reserved) = mpsc::channel();
        let process = PortablePtyBackend
            .spawn_command_with_waiter(
                &input,
                prepared,
                |_name, task| {
                    task();
                    Ok(())
                },
                |pid| {
                    let mut information = std::mem::MaybeUninit::<libc::siginfo_t>::zeroed();
                    // SAFETY: pid names this fixture's child and information is
                    // writable. WNOWAIT leaves the stopped child owned by spawn.
                    let stopped = unsafe {
                        libc::waitid(
                            libc::P_PID,
                            pid as libc::id_t,
                            information.as_mut_ptr(),
                            libc::WSTOPPED | libc::WNOWAIT,
                        )
                    };
                    assert_eq!(stopped, 0, "observe fixture stop");
                    // SAFETY: the stopped, unreaped fixture still owns this PID.
                    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, libc::SIGCONT) }, 0);
                    wait_for_unix_child_exit_without_reaping(pid)
                        .expect("fixture exits before the spawn identity probe");
                },
                move |result| reservation.send(result).expect("record waiter reservation"),
            )
            .expect("spawn immediately exiting PTY");

        assert_eq!(
            *process.subscribe_exit().borrow(),
            Some(PtyExit {
                exit_code: Some(17),
                signal: None,
            })
        );
        assert_eq!(process.process_identity(), None);
        reserved
            .try_recv()
            .expect("waiter must reserve the captured process group")
            .expect("waiter must not take the failed-to-reserve warning path");
    }

    #[cfg(not(windows))]
    #[derive(Debug)]
    enum TestWriter {
        WriteError,
        FlushError,
    }

    #[cfg(not(windows))]
    impl Write for TestWriter {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            match self {
                Self::WriteError => Err(Error::new(ErrorKind::BrokenPipe, "write failed")),
                Self::FlushError => Ok(buffer.len()),
            }
        }

        fn flush(&mut self) -> std::io::Result<()> {
            match self {
                Self::WriteError => Ok(()),
                Self::FlushError => Err(Error::new(ErrorKind::BrokenPipe, "flush failed")),
            }
        }
    }

    #[cfg(not(windows))]
    #[derive(Clone, Debug)]
    struct TestKiller {
        fail: bool,
    }

    #[cfg(not(windows))]
    impl portable_pty::ChildKiller for TestKiller {
        fn kill(&mut self) -> std::io::Result<()> {
            if self.fail {
                Err(Error::new(ErrorKind::PermissionDenied, "kill failed"))
            } else {
                Ok(())
            }
        }

        fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
            Box::new(self.clone())
        }
    }

    #[derive(Clone, Debug)]
    struct SetupTestChild {
        killed: Arc<std::sync::atomic::AtomicBool>,
        panic_on_clone: bool,
    }

    impl portable_pty::ChildKiller for SetupTestChild {
        fn kill(&mut self) -> std::io::Result<()> {
            self.killed
                .store(true, std::sync::atomic::Ordering::Release);
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
            assert!(!self.panic_on_clone, "injected child killer clone failure");
            Box::new(self.clone())
        }
    }

    impl portable_pty::Child for SetupTestChild {
        fn try_wait(&mut self) -> std::io::Result<Option<portable_pty::ExitStatus>> {
            Ok(None)
        }

        fn wait(&mut self) -> std::io::Result<portable_pty::ExitStatus> {
            Ok(portable_pty::ExitStatus::with_exit_code(0))
        }

        fn process_id(&self) -> Option<u32> {
            Some(41)
        }

        #[cfg(windows)]
        fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
            None
        }
    }

    #[derive(Clone, Debug)]
    struct WaitErrorSetupTestChild {
        killed: Arc<std::sync::atomic::AtomicBool>,
    }

    impl portable_pty::ChildKiller for WaitErrorSetupTestChild {
        fn kill(&mut self) -> std::io::Result<()> {
            self.killed
                .store(true, std::sync::atomic::Ordering::Release);
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
            Box::new(self.clone())
        }
    }

    impl portable_pty::Child for WaitErrorSetupTestChild {
        fn try_wait(&mut self) -> std::io::Result<Option<portable_pty::ExitStatus>> {
            Ok(None)
        }

        fn wait(&mut self) -> std::io::Result<portable_pty::ExitStatus> {
            Err(std::io::Error::other("injected initializer wait failure"))
        }

        fn process_id(&self) -> Option<u32> {
            Some(43)
        }

        #[cfg(windows)]
        fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
            None
        }
    }

    #[derive(Clone, Debug)]
    struct OrderedSetupTestChild {
        cleanup_order: Arc<Mutex<Vec<&'static str>>>,
    }

    impl portable_pty::ChildKiller for OrderedSetupTestChild {
        fn kill(&mut self) -> std::io::Result<()> {
            self.cleanup_order.lock().unwrap().push("root");
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
            Box::new(self.clone())
        }
    }

    impl portable_pty::Child for OrderedSetupTestChild {
        fn try_wait(&mut self) -> std::io::Result<Option<portable_pty::ExitStatus>> {
            Ok(None)
        }

        fn wait(&mut self) -> std::io::Result<portable_pty::ExitStatus> {
            Ok(portable_pty::ExitStatus::with_exit_code(0))
        }

        fn process_id(&self) -> Option<u32> {
            Some(42)
        }

        #[cfg(windows)]
        fn as_raw_handle(&self) -> Option<std::os::windows::io::RawHandle> {
            None
        }
    }

    #[test]
    fn spawned_child_guard_kills_on_setup_failure_until_process_ownership_commits() {
        let failed_setup_killed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let guard = SpawnedChildGuard::new(Box::new(SetupTestChild {
            killed: failed_setup_killed.clone(),
            panic_on_clone: false,
        }));
        drop(guard);
        assert!(failed_setup_killed.load(std::sync::atomic::Ordering::Acquire));

        #[cfg(unix)]
        {
            let committed_killed = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let mut guard = SpawnedChildGuard::new(Box::new(SetupTestChild {
                killed: committed_killed.clone(),
                panic_on_clone: false,
            }));
            let child = guard.handoff_child_to_waiter();
            let process_group = guard.commit_process_group();
            drop(child);
            assert!(process_group.is_none());
            assert!(!committed_killed.load(std::sync::atomic::Ordering::Acquire));
        }

        #[cfg(windows)]
        {
            let committed_killed = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let mut guard = SpawnedChildGuard::new(Box::new(SetupTestChild {
                killed: committed_killed.clone(),
                panic_on_clone: false,
            }));
            guard.own_job(WindowsJob::new().expect("create committed PTY test job"));
            let waiter_job = guard
                .clone_job_for_waiter()
                .expect("clone waiter PTY job ownership");
            let child = guard.handoff_child_to_waiter();
            let job = guard.commit_job().expect("commit PTY job ownership");

            assert!(Arc::ptr_eq(&job, &waiter_job));
            drop(child);
            assert!(!committed_killed.load(std::sync::atomic::Ordering::Acquire));
        }
    }

    #[test]
    fn initializer_wait_failure_kills_the_initializer_child() {
        let killed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let error = wait_for_windows_console_theme_initializer(Box::new(WaitErrorSetupTestChild {
            killed: killed.clone(),
        }))
        .unwrap_err();

        assert!(error.contains("injected initializer wait failure"));
        assert!(killed.load(std::sync::atomic::Ordering::Acquire));
    }

    #[test]
    fn waiter_spawn_failure_keeps_the_root_child_fallback() {
        let killed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut guard = SpawnedChildGuard::new(Box::new(SetupTestChild {
            killed: killed.clone(),
            panic_on_clone: false,
        }));
        let handoff = guard.handoff_child_to_waiter();

        let error = spawn_pty_thread_with(
            "injected-waiter-spawn-failure".to_owned(),
            Box::new(move || drop(handoff)),
            |_builder, _task| Err(std::io::Error::other("injected waiter spawn failure")),
        )
        .unwrap_err();

        assert_eq!(error.to_string(), "injected waiter spawn failure");
        assert!(
            !killed.load(std::sync::atomic::Ordering::Acquire),
            "root cleanup must remain with the tree-owning setup guard"
        );
        drop(guard);
        assert!(killed.load(std::sync::atomic::Ordering::Acquire));
    }

    #[test]
    fn child_killer_clone_panic_keeps_the_root_in_the_setup_guard() {
        let killed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let result = catch_unwind(AssertUnwindSafe({
            let killed = killed.clone();
            move || {
                let mut guard = SpawnedChildGuard::new(Box::new(SetupTestChild {
                    killed,
                    panic_on_clone: true,
                }));
                let _child = guard.handoff_child_to_waiter();
            }
        }));

        assert!(result.is_err());
        assert!(killed.load(std::sync::atomic::Ordering::Acquire));
    }

    #[cfg(unix)]
    #[test]
    fn failed_group_signal_keeps_authority_until_waiter_retry_succeeds() {
        let ownership = Arc::new(Mutex::new(UnixProcessGroupOwnership::Reserved));
        let first_entered = Arc::new(std::sync::Barrier::new(2));
        let first_release = Arc::new(std::sync::Barrier::new(2));
        let first_ownership = ownership.clone();
        let entered = first_entered.clone();
        let release = first_release.clone();
        let first = std::thread::spawn(move || {
            signal_reserved_unix_process_group_with(42, &first_ownership, |_| {
                entered.wait();
                release.wait();
                Err(std::io::Error::from_raw_os_error(libc::EPERM))
            })
        });
        first_entered.wait();

        let second_ownership = ownership.clone();
        let (second_entered, second_entry) = std::sync::mpsc::channel();
        let second = std::thread::spawn(move || {
            signal_reserved_unix_process_group_with(42, &second_ownership, |_| {
                second_entered.send(()).expect("waiter signal observer");
                Ok(())
            })
        });
        assert!(
            second_entry
                .recv_timeout(Duration::from_millis(20))
                .is_err(),
            "waiter signal must serialize behind the in-progress manager attempt"
        );
        first_release.wait();
        assert_eq!(
            first
                .join()
                .expect("manager signal task")
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EPERM)
        );
        second_entry
            .recv_timeout(Duration::from_secs(1))
            .expect("waiter retries after manager failure");
        second.join().expect("waiter signal task").unwrap();
        assert_eq!(
            *ownership
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            UnixProcessGroupOwnership::Released
        );
    }

    #[cfg(unix)]
    #[test]
    fn waiter_spawn_failure_sigkills_a_hup_resistant_root_without_a_process_group() {
        let directory = tempfile::tempdir().unwrap();
        let ready_file = directory.path().join("root-ready.txt");
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut command = CommandBuilder::new("/bin/sh");
        command.args([
            "-c",
            "trap '' HUP; echo ready > \"$CODDY_STUDIO_ROOT_READY\"; exec sleep 30",
        ]);
        command.env("CODDY_STUDIO_ROOT_READY", ready_file.as_os_str());
        let child = pair.slave.spawn_command(command).unwrap();
        let root_pid = child.process_id().unwrap();
        let root_pid_i32 = i32::try_from(root_pid).unwrap();
        let mut guard = SpawnedChildGuard::new_with_process_group(child, None);
        guard.remember_root_process_id(root_pid).unwrap();
        drop(pair.slave);

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !ready_file.is_file() {
            assert!(
                std::time::Instant::now() < deadline,
                "fixture root did not publish readiness"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let child = guard.handoff_child_to_waiter();
        spawn_pty_thread_with(
            "injected-hup-resistant-waiter-spawn-failure".to_owned(),
            Box::new(move || drop(child)),
            |_builder, _task| Err(std::io::Error::other("injected waiter spawn failure")),
        )
        .unwrap_err();

        // SAFETY: signal zero only checks whether the fixture root still exists.
        assert_eq!(unsafe { kill(root_pid_i32, 0) }, 0);
        drop(guard);

        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut status = 0;
        loop {
            // SAFETY: the fixture root is a direct child of this test process;
            // WNOHANG observes and reaps it only after termination.
            let result = unsafe { waitpid(root_pid_i32, &raw mut status, 1) };
            if result == root_pid_i32 {
                break;
            }
            if std::time::Instant::now() >= deadline {
                // SAFETY: the PID came from this test's fixture root.
                unsafe { kill(root_pid_i32, 9) };
                panic!("HUP-resistant PTY root survived waiter spawn failure");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn waiter_spawn_failure_terminates_the_tree_before_the_root() {
        let cleanup_order = Arc::new(Mutex::new(Vec::new()));
        let mut guard = SpawnedChildGuard::new(Box::new(OrderedSetupTestChild {
            cleanup_order: cleanup_order.clone(),
        }));
        guard.observe_tree_cleanup({
            let cleanup_order = cleanup_order.clone();
            move || cleanup_order.lock().unwrap().push("tree")
        });
        let child = guard.handoff_child_to_waiter();

        spawn_pty_thread_with(
            "injected-ordered-waiter-spawn-failure".to_owned(),
            Box::new(move || drop(child)),
            |_builder, _task| Err(std::io::Error::other("injected waiter spawn failure")),
        )
        .unwrap_err();

        assert!(cleanup_order.lock().unwrap().is_empty());
        drop(guard);
        assert_eq!(*cleanup_order.lock().unwrap(), ["tree", "root"]);
    }

    #[cfg(unix)]
    #[test]
    fn setup_failure_kills_the_unix_process_group() {
        let directory = tempfile::tempdir().unwrap();
        let child_pid_file = directory.path().join("descendant-pid.txt");
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut command = CommandBuilder::new("/bin/sh");
        command.args([
            "-c",
            "trap '' HUP; /bin/sh -c 'trap \"\" HUP; echo $$ > \"$CODDY_STUDIO_CHILD_PID\"; exec sleep 30' & wait",
        ]);
        command.env("CODDY_STUDIO_CHILD_PID", child_pid_file.as_os_str());
        let child = pair.slave.spawn_command(command).unwrap();
        let process_group = pair
            .master
            .process_group_leader()
            .expect("fixture PTY should expose its process group");
        let guard = SpawnedChildGuard::new_with_process_group(child, Some(process_group));

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !child_pid_file.is_file() {
            assert!(
                std::time::Instant::now() < deadline,
                "fixture descendant did not publish its process id"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let descendant_pid = std::fs::read_to_string(&child_pid_file)
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();
        assert_ne!(descendant_pid, process_group);

        drop(guard);

        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            // SAFETY: signal zero only checks whether the fixture PID still exists.
            let result = unsafe { kill(descendant_pid, 0) };
            if result != 0 && std::io::Error::last_os_error().raw_os_error() == Some(3) {
                break;
            }
            if std::time::Instant::now() >= deadline {
                // SAFETY: the PID came from this test's fixture descendant.
                unsafe { kill(descendant_pid, 9) };
                panic!("PTY descendant survived post-spawn setup failure");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn executable_discovery_handles_absolute_relative_and_overridden_paths() {
        let executable = std::env::current_exe().expect("current test executable");
        let command = executable
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .expect("test executable file name");
        let directory = executable.parent().expect("test executable directory");
        let overrides = BTreeMap::new();
        assert_eq!(
            resolve_pty_executable_on(
                Platform::current(),
                executable.to_str().expect("test executable path"),
                directory,
                &overrides,
            ),
            Some(executable.clone())
        );
        assert_eq!(
            resolve_pty_executable_on(
                Platform::current(),
                executable
                    .with_file_name("definitely-missing-coddy-studio-shell")
                    .to_str()
                    .expect("missing executable path"),
                directory,
                &overrides,
            ),
            None
        );

        let mut isolated = BTreeMap::new();
        isolated.insert(
            "Path".to_owned(),
            std::env::join_paths([directory])
                .expect("test executable search path")
                .to_string_lossy()
                .into_owned(),
        );
        assert_eq!(
            resolve_pty_executable_on(Platform::current(), command, directory, &isolated),
            Some(executable.clone())
        );
        isolated.insert(
            "PATH".to_owned(),
            executable
                .parent()
                .expect("test executable directory")
                .join("definitely-missing-coddy-studio-bin")
                .to_string_lossy()
                .into_owned(),
        );
        assert_eq!(
            resolve_pty_executable_on(
                Platform::current(),
                "coddy-studio-shell",
                directory,
                &isolated,
            ),
            None
        );
    }

    #[test]
    fn executable_discovery_honors_windows_pathext_for_bare_commands() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("claude.exe");
        std::fs::write(&executable, b"fixture").unwrap();
        let mut environment = BTreeMap::new();
        environment.insert(
            "Path".to_owned(),
            std::env::join_paths([directory.path()])
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        );
        environment.insert("PATHEXT".to_owned(), ".COM;.EXE;.BAT;.CMD".to_owned());

        assert_eq!(
            resolve_pty_executable_on(Platform::Windows, "claude", directory.path(), &environment,),
            Some(executable)
        );
        assert_eq!(
            resolve_pty_executable_on(Platform::Unix, "claude", directory.path(), &environment,),
            None
        );
    }

    #[test]
    fn windows_executable_discovery_includes_powershell_shims_when_pathext_omits_them() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("claude.ps1");
        std::fs::write(&executable, b"fixture").unwrap();
        let environment = BTreeMap::from([
            (
                "Path".to_owned(),
                std::env::join_paths([directory.path()])
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
            ),
            ("PATHEXT".to_owned(), ".COM;.EXE;.BAT;.CMD".to_owned()),
        ]);

        assert_eq!(
            resolve_pty_executable_on(Platform::Windows, "claude", directory.path(), &environment),
            Some(executable)
        );
    }

    #[test]
    fn windows_pty_launch_wraps_cmd_and_bat_case_insensitively() {
        let directory = tempfile::tempdir().unwrap();
        let shim_directory = directory.path().join("provider ! shims");
        std::fs::create_dir(&shim_directory).unwrap();
        let arguments = vec![
            "--flag".to_owned(),
            "value with spaces".to_owned(),
            "&literal".to_owned(),
            "%PATH%".to_owned(),
            "!literal!".to_owned(),
            String::new(),
        ];

        for extension in ["CmD", "bAt"] {
            let executable = shim_directory.join(format!("provider.{extension}"));
            std::fs::write(&executable, b"fixture").unwrap();
            let input = PtySpawnInput {
                executable: executable.to_string_lossy().into_owned(),
                args: arguments.clone(),
                cwd: directory.path().to_path_buf(),
                cols: 80,
                rows: 24,
                env: BTreeMap::from([
                    ("ComSpec".to_owned(), "user-command-processor".to_owned()),
                    (
                        "CODDY_STUDIO_INTERNAL_BATCH_SCRIPT".to_owned(),
                        "user-controlled-value".to_owned(),
                    ),
                    (
                        "CODDY_STUDIO_INTERNAL_BATCH_ARG_2".to_owned(),
                        "user-controlled-value".to_owned(),
                    ),
                ]),
            };

            let command = build_pty_command_on(Platform::Windows, &input).unwrap();
            let raw_arguments = format!(
                "/e:ON /v:OFF /d /c \"\"{}\" --flag \"value with spaces\" \"&literal\" \"%%cd:~,%%PATH%%cd:~,%%\" \"!literal!\" \"\"\"",
                executable.display()
            );
            let expected = vec![
                OsString::from("user-command-processor"),
                OsString::from(raw_arguments),
            ];
            assert_eq!(command.get_argv(), &expected);
            assert_eq!(
                command.get_env("CODDY_STUDIO_INTERNAL_BATCH_SCRIPT"),
                Some(std::ffi::OsStr::new("user-controlled-value"))
            );
            assert_eq!(
                command.get_env("CODDY_STUDIO_INTERNAL_BATCH_ARG_2"),
                Some(std::ffi::OsStr::new("user-controlled-value"))
            );
            assert_eq!(
                command.get_env("ComSpec"),
                Some(std::ffi::OsStr::new("user-command-processor"))
            );
        }
    }

    #[test]
    fn windows_batch_launch_rejects_control_characters_in_the_script_path() {
        // Windows rejects this filename at the filesystem boundary, so test
        // the launch validation directly instead of trying to create it.
        let error =
            wrap_launch_program(Platform::Windows, Path::new("provider\nshim.cmd")).unwrap_err();

        assert_eq!(
            error,
            "Windows batch executable paths cannot contain control characters"
        );
    }

    #[test]
    fn windows_pty_launch_discovers_and_wraps_powershell_shims() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("provider.ps1");
        std::fs::write(&executable, b"fixture").unwrap();
        let arguments = vec![
            "--flag".to_owned(),
            "value with spaces".to_owned(),
            "$literal".to_owned(),
            String::new(),
        ];
        let input = PtySpawnInput {
            executable: "provider".to_owned(),
            args: arguments.clone(),
            cwd: directory.path().to_path_buf(),
            cols: 80,
            rows: 24,
            env: BTreeMap::from([
                (
                    "Path".to_owned(),
                    std::env::join_paths([directory.path()])
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                ),
                ("PATHEXT".to_owned(), ".COM;.EXE;.BAT;.CMD".to_owned()),
            ]),
        };

        let command = build_pty_command_on(Platform::Windows, &input).unwrap();
        let expected = std::iter::once(OsString::from("powershell.exe"))
            .chain(
                [
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-File",
                ]
                .map(OsString::from),
            )
            .chain(std::iter::once(executable.into_os_string()))
            .chain(arguments.iter().map(OsString::from))
            .collect::<Vec<_>>();
        assert_eq!(command.get_argv(), &expected);
    }

    #[test]
    fn windows_pty_launches_native_executables_directly() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("provider.exe");
        std::fs::write(&executable, b"fixture").unwrap();
        let arguments = vec!["--flag".to_owned(), "value with spaces".to_owned()];
        let input = PtySpawnInput {
            executable: executable.to_string_lossy().into_owned(),
            args: arguments.clone(),
            cwd: directory.path().to_path_buf(),
            cols: 80,
            rows: 24,
            env: BTreeMap::new(),
        };

        let command = build_pty_command_on(Platform::Windows, &input).unwrap();
        let expected = std::iter::once(executable.into_os_string())
            .chain(arguments.iter().map(OsString::from))
            .collect::<Vec<_>>();
        assert_eq!(command.get_argv(), &expected);
    }

    #[test]
    fn pty_launch_resolves_a_multi_component_executable_to_the_exact_cwd_path() {
        let cwd = tempfile::tempdir().unwrap();
        let bin = cwd.path().join("tools");
        std::fs::create_dir(&bin).unwrap();
        let executable = bin.join("provider-fixture");
        std::fs::write(&executable, b"fixture").unwrap();
        let input = PtySpawnInput {
            executable: "tools/provider-fixture".to_owned(),
            args: vec!["--direct".to_owned()],
            cwd: cwd.path().to_path_buf(),
            cols: 80,
            rows: 24,
            env: BTreeMap::new(),
        };

        let command = build_pty_command_on(Platform::Unix, &input).unwrap();
        let argv = command.get_argv();
        let resolved = Path::new(&argv[0]).components().collect::<PathBuf>();
        let expected = executable.components().collect::<PathBuf>();
        assert_eq!(resolved, expected);
        assert_eq!(argv[1], std::ffi::OsString::from("--direct"));
    }

    #[test]
    fn pty_launch_defaults_terminal_capabilities_for_direct_commands() {
        let executable = std::env::current_exe().expect("current test executable");
        let input = PtySpawnInput {
            executable: executable.to_string_lossy().into_owned(),
            args: Vec::new(),
            cwd: executable
                .parent()
                .expect("test executable directory")
                .to_path_buf(),
            cols: 80,
            rows: 24,
            env: BTreeMap::new(),
        };

        let command = build_pty_command_on(Platform::Unix, &input).unwrap();
        assert_eq!(
            command
                .iter_extra_env_as_str()
                .find(|(key, _)| *key == "TERM"),
            Some(("TERM", "xterm-256color"))
        );
    }

    #[test]
    fn pty_launch_preserves_an_explicit_terminal_type() {
        let executable = std::env::current_exe().expect("current test executable");
        let input = PtySpawnInput {
            executable: executable.to_string_lossy().into_owned(),
            args: Vec::new(),
            cwd: executable
                .parent()
                .expect("test executable directory")
                .to_path_buf(),
            cols: 80,
            rows: 24,
            env: BTreeMap::from([("TERM".to_owned(), "dumb".to_owned())]),
        };

        let command = build_pty_command_on(Platform::Unix, &input).unwrap();
        assert_eq!(command.get_env("TERM"), Some(OsStr::new("dumb")));
    }

    #[test]
    fn prepared_command_clears_host_no_color_and_forces_truecolor() {
        let executable = std::env::current_exe().expect("current test executable");
        let input = PtySpawnInput {
            executable: executable.to_string_lossy().into_owned(),
            args: Vec::new(),
            cwd: executable
                .parent()
                .expect("test executable directory")
                .to_path_buf(),
            cols: 80,
            rows: 24,
            env: BTreeMap::new(),
        };

        let command = build_pty_command_on(Platform::Unix, &input).unwrap();
        assert_eq!(command.get_env("NO_COLOR"), None);
        assert_eq!(command.get_env("FORCE_COLOR"), Some(OsStr::new("3")));
        assert_eq!(command.get_env("COLORTERM"), Some(OsStr::new("truecolor")));
    }

    #[test]
    fn prepared_command_preserves_explicit_launch_no_color() {
        let executable = std::env::current_exe().expect("current test executable");
        let input = PtySpawnInput {
            executable: executable.to_string_lossy().into_owned(),
            args: Vec::new(),
            cwd: executable
                .parent()
                .expect("test executable directory")
                .to_path_buf(),
            cols: 80,
            rows: 24,
            env: BTreeMap::from([("NO_COLOR".to_owned(), "1".to_owned())]),
        };

        let command = build_pty_command_on(Platform::Unix, &input).unwrap();
        // ensure_pty_color_environment opts out; launch env then applies NO_COLOR.
        assert_eq!(command.get_env("NO_COLOR"), Some(OsStr::new("1")));
    }

    #[test]
    fn reserved_pty_env_keys_follow_platform_case_rules() {
        for key in [
            crate::terminal::osc::OSC_FOREGROUND_ENV,
            crate::terminal::osc::OSC_BACKGROUND_ENV,
            crate::terminal::osc::OSC_CURSOR_ENV,
            WINDOWS_CONSOLE_THEME_ENV,
        ] {
            assert!(is_reserved_pty_env_key_on(Platform::Unix, key));
            assert!(is_reserved_pty_env_key_on(Platform::Windows, key));
            assert!(!is_reserved_pty_env_key_on(
                Platform::Unix,
                &key.to_ascii_lowercase()
            ));
            assert!(is_reserved_pty_env_key_on(
                Platform::Windows,
                &key.to_ascii_lowercase()
            ));
        }
        assert!(!is_reserved_pty_env_key_on(Platform::Windows, "ORDINARY"));
    }

    #[test]
    fn prepared_command_strips_reserved_palette_and_theme_environment() {
        let executable = std::env::current_exe().expect("current test executable");
        let input = PtySpawnInput {
            executable: executable.to_string_lossy().into_owned(),
            args: Vec::new(),
            cwd: executable
                .parent()
                .expect("test executable directory")
                .to_path_buf(),
            cols: 80,
            rows: 24,
            env: BTreeMap::from([
                (WINDOWS_CONSOLE_THEME_ENV.to_owned(), "light".to_owned()),
                (
                    crate::terminal::osc::OSC_FOREGROUND_ENV.to_owned(),
                    "28,33,41".to_owned(),
                ),
                (
                    crate::terminal::osc::OSC_BACKGROUND_ENV.to_owned(),
                    "255,255,255".to_owned(),
                ),
                (
                    crate::terminal::osc::OSC_CURSOR_ENV.to_owned(),
                    "38,56,78".to_owned(),
                ),
                ("ORDINARY".to_owned(), "value".to_owned()),
            ]),
        };

        let command = build_pty_command_on(Platform::Unix, &input).unwrap();
        assert_eq!(command.get_env(WINDOWS_CONSOLE_THEME_ENV), None);
        assert_eq!(
            command.get_env(crate::terminal::osc::OSC_FOREGROUND_ENV),
            None
        );
        assert_eq!(
            command.get_env(crate::terminal::osc::OSC_BACKGROUND_ENV),
            None
        );
        assert_eq!(command.get_env(crate::terminal::osc::OSC_CURSOR_ENV), None);
        assert_eq!(command.get_env("ORDINARY"), Some(OsStr::new("value")));
    }

    #[test]
    fn windows_prepared_command_strips_mixed_case_coddy_studio_environment() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("provider.exe");
        std::fs::write(&executable, b"fixture").unwrap();
        let input = PtySpawnInput {
            executable: executable.to_string_lossy().into_owned(),
            args: Vec::new(),
            cwd: directory.path().to_path_buf(),
            cols: 80,
            rows: 24,
            env: BTreeMap::from([
                (
                    "coddy_studio_osc_foreground".to_owned(),
                    "reserved".to_owned(),
                ),
                (
                    "CoddyStudio_Osc_Background".to_owned(),
                    "reserved".to_owned(),
                ),
                ("coddy_studio_osc_cursor".to_owned(), "reserved".to_owned()),
                (
                    "CoddyStudio_Windows_Console_Theme".to_owned(),
                    "light".to_owned(),
                ),
                ("ORDINARY".to_owned(), "value".to_owned()),
            ]),
        };

        let command = build_pty_command_on(Platform::Windows, &input).unwrap();
        assert_eq!(command.get_env("coddy_studio_osc_foreground"), None);
        assert_eq!(command.get_env("CoddyStudio_Osc_Background"), None);
        assert_eq!(command.get_env("coddy_studio_osc_cursor"), None);
        assert_eq!(command.get_env("CoddyStudio_Windows_Console_Theme"), None);
        assert_eq!(command.get_env("ORDINARY"), Some(OsStr::new("value")));
    }

    #[test]
    fn windows_console_theme_marker_requires_an_exact_supported_value() {
        for (value, expected) in [
            ("light", Some(WindowsConsoleTheme::Light)),
            ("dark", Some(WindowsConsoleTheme::Dark)),
            ("", None),
            ("LIGHT", None),
            ("true", None),
            ("1", None),
        ] {
            let env = BTreeMap::from([(WINDOWS_CONSOLE_THEME_ENV.to_owned(), value.to_owned())]);
            assert_eq!(
                windows_console_theme_from_env(&env),
                expected,
                "value={value:?}"
            );
        }
        assert_eq!(windows_console_theme_from_env(&BTreeMap::new()), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_theme_initializer_uses_the_absolute_system_cmd() {
        let command =
            build_windows_console_theme_initializer_command(WindowsConsoleTheme::Light).unwrap();
        let program = Path::new(&command.get_argv()[0]);
        assert!(program.is_absolute(), "program={program:?}");
        assert_eq!(program.file_name().and_then(OsStr::to_str), Some("cmd.exe"));
        assert!(program.is_file(), "program={program:?}");
    }

    #[test]
    fn executable_resolution_anchors_relative_path_entries_to_the_terminal_cwd() {
        let cwd = tempfile::tempdir().unwrap();
        let bin = cwd.path().join("relative-bin");
        std::fs::create_dir(&bin).unwrap();
        let executable = bin.join("provider-fixture");
        std::fs::write(&executable, b"fixture").unwrap();
        let mut environment = BTreeMap::new();
        environment.insert("PATH".to_owned(), "relative-bin".to_owned());

        assert_eq!(
            resolve_pty_executable_on(Platform::Unix, "provider-fixture", cwd.path(), &environment,),
            Some(executable)
        );
    }

    #[test]
    fn platform_resolution_rejects_missing_candidates() {
        let directory = tempfile::tempdir().unwrap();
        let environment = BTreeMap::from([(
            "PATH".to_owned(),
            directory.path().to_string_lossy().into_owned(),
        )]);

        assert_eq!(
            resolve_pty_executable_on(
                Platform::Windows,
                "missing-provider",
                directory.path(),
                &environment,
            ),
            None
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn portable_backend_discovers_a_relative_executable_from_the_terminal_cwd() {
        let cwd = tempfile::tempdir().unwrap();
        let executable = cwd.path().join("provider-fixture");
        crate::test_support::executable_fixture::write_executable(
            &executable,
            "#!/bin/sh\nexit 0\n",
        );

        let process = PortablePtyBackend
            .spawn(&PtySpawnInput {
                executable: "./provider-fixture".to_owned(),
                args: Vec::new(),
                cwd: cwd.path().to_path_buf(),
                cols: 80,
                rows: 24,
                env: BTreeMap::new(),
            })
            .unwrap();
        assert!(process.pid() > 0);
        let mut exit = process.subscribe_exit();
        tokio::time::timeout(Duration::from_secs(3), async {
            while exit.borrow().is_none() {
                exit.changed().await.unwrap();
            }
        })
        .await
        .expect("relative script exits before its fixture is removed");
        assert_eq!(exit.borrow().as_ref().unwrap().exit_code, Some(0));
    }

    #[cfg(unix)]
    #[test]
    fn portable_backend_reports_a_missing_script_interpreter_as_spawn_failure() {
        let cwd = tempfile::tempdir().unwrap();
        let executable = cwd.path().join("provider-fixture");
        crate::test_support::executable_fixture::write_executable(
            &executable,
            "#!/coddy-studio-missing-interpreter\n",
        );
        let result = PortablePtyBackend.spawn(&PtySpawnInput {
            executable: executable.to_string_lossy().into_owned(),
            args: Vec::new(),
            cwd: cwd.path().to_path_buf(),
            cols: 80,
            rows: 24,
            env: BTreeMap::new(),
        });
        assert!(
            result.is_err(),
            "failed exec must not publish a PTY process"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn portable_backend_closes_inherited_non_stdio_descriptors_on_exec() {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

        let file = std::fs::File::open("/dev/null").unwrap();
        // F_DUPFD deliberately creates an inheritable descriptor above the
        // range the shell uses for its own script and redirection descriptors.
        let descriptor = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_DUPFD, 200) };
        assert!(descriptor >= 200);
        // SAFETY: F_DUPFD returned a new descriptor owned by this fixture.
        let descriptor = unsafe { OwnedFd::from_raw_fd(descriptor) };
        let process = PortablePtyBackend
            .spawn(&PtySpawnInput {
                executable: "/bin/sh".to_owned(),
                args: vec![
                    "-c".to_owned(),
                    format!("test ! -e /dev/fd/{}", descriptor.as_raw_fd()),
                ],
                cwd: std::env::temp_dir(),
                cols: 80,
                rows: 24,
                env: BTreeMap::new(),
            })
            .unwrap();
        let mut exit = process.subscribe_exit();
        tokio::time::timeout(Duration::from_secs(3), async {
            while exit.borrow().is_none() {
                exit.changed().await.unwrap();
            }
        })
        .await
        .expect("descriptor probe exits");
        assert_eq!(exit.borrow().as_ref().unwrap().exit_code, Some(0));
    }

    #[tokio::test]
    async fn portable_backend_retains_exit_for_a_late_subscriber() {
        let input = PtySpawnInput {
            executable: if cfg!(windows) { "cmd.exe" } else { "/bin/sh" }.to_owned(),
            args: if cfg!(windows) {
                vec!["/D".to_owned(), "/C".to_owned(), "exit 7".to_owned()]
            } else {
                vec!["-c".to_owned(), "exit 7".to_owned()]
            },
            cwd: std::env::temp_dir(),
            cols: 80,
            rows: 24,
            env: BTreeMap::new(),
        };
        let prepared = build_pty_command(&input).unwrap();
        let (completed, waiter_completed) = tokio::sync::oneshot::channel();
        let process = PortablePtyBackend
            .spawn_command_with_waiter(
                &input,
                prepared,
                |name, task| {
                    spawn_pty_thread_with(name, task, |builder, task| {
                        builder.spawn(move || {
                            task();
                            let _ = completed.send(());
                        })
                    })
                },
                #[cfg(all(test, unix))]
                |_| {},
                #[cfg(all(test, unix))]
                |_| {},
            )
            .unwrap();

        // Wait for native reaping and exit publication, without ever creating
        // an exit receiver. A PID disappearance alone would race publication.
        let event_timeout = Duration::from_secs(if cfg!(windows) { 10 } else { 3 });
        tokio::time::timeout(event_timeout, waiter_completed)
            .await
            .expect("native PTY waiter completes")
            .expect("native PTY waiter reports completion");

        let exit = process.subscribe_exit();
        assert_eq!(
            *exit.borrow(),
            Some(PtyExit {
                exit_code: Some(7),
                signal: None,
            }),
            "late subscribers must observe the already reaped native child"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_root_killed_by_a_signal_keeps_the_signal_in_its_exit_status() {
        let process = PortablePtyBackend
            .spawn(&PtySpawnInput {
                executable: "/bin/sh".to_owned(),
                args: vec!["-c".to_owned(), "kill -USR1 $$".to_owned()],
                cwd: std::env::temp_dir(),
                cols: 80,
                rows: 24,
                env: BTreeMap::new(),
            })
            .unwrap();
        let mut exit = process.subscribe_exit();
        let observed = tokio::time::timeout(Duration::from_secs(3), exit.wait_for(Option::is_some))
            .await
            .expect("exit before the deadline")
            .expect("exit published")
            .clone();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            process.exit_status().and_then(|status| status.signal()),
            Some(libc::SIGUSR1)
        );
        assert_eq!(
            observed,
            Some(PtyExit {
                exit_code: Some(1),
                signal: None,
            }),
            "the published exit keeps portable-pty's code"
        );
    }

    #[tokio::test]
    async fn portable_backend_streams_input_output_resize_and_exit() {
        let (executable, args, input, output_marker) = if cfg!(windows) {
            (
                "cmd.exe".to_owned(),
                vec!["/D".to_owned(), "/Q".to_owned()],
                "echo ready\r\necho got:hello from test\r\nexit /b 7\r\n",
                "got:hello from test",
            )
        } else {
            (
                "/bin/sh".to_owned(),
                vec![
                    "-c".to_owned(),
                    "printf 'ready\\n'; IFS= read -r line; printf 'got:%s\\n' \"$line\"; exit 7"
                        .to_owned(),
                ],
                "hello from test\n",
                "got:hello from test",
            )
        };
        let event_timeout = Duration::from_secs(if cfg!(windows) { 10 } else { 3 });
        let backend = PortablePtyBackend;
        let process = backend
            .spawn(&PtySpawnInput {
                executable,
                args,
                cwd: std::env::temp_dir(),
                cols: 80,
                rows: 24,
                env: BTreeMap::new(),
            })
            .unwrap();
        assert!(process.pid() > 0);
        assert!(format!("{process:?}").contains("PortablePtyProcess"));

        let mut output = process.subscribe_output();
        let mut exit = process.subscribe_exit();
        process.resize(100, 40).unwrap();
        process.resize(120, 50).unwrap();
        if cfg!(windows) {
            process.write("\u{1b}[1;1R").unwrap();
        }
        process.write(input).unwrap();

        let text = tokio::time::timeout(event_timeout, async {
            let mut text = String::new();
            while !text.contains(output_marker) {
                text.push_str(&output.recv().await.unwrap());
            }
            text
        })
        .await
        .unwrap();
        assert!(text.contains("ready"));

        tokio::time::timeout(event_timeout, exit.changed())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            *exit.borrow(),
            Some(PtyExit {
                exit_code: Some(7),
                signal: None,
            })
        );
    }

    #[tokio::test]
    async fn portable_backend_kills_a_live_process_group() {
        let (executable, args) = if cfg!(windows) {
            (
                "powershell.exe".to_owned(),
                vec![
                    "-NoLogo".to_owned(),
                    "-NoProfile".to_owned(),
                    "-Command".to_owned(),
                    "Start-Sleep -Seconds 30".to_owned(),
                ],
            )
        } else {
            (
                "/bin/sh".to_owned(),
                vec!["-c".to_owned(), "sleep 30".to_owned()],
            )
        };
        let process = PortablePtyBackend
            .spawn(&PtySpawnInput {
                executable,
                args,
                cwd: std::env::temp_dir(),
                cols: 80,
                rows: 24,
                env: BTreeMap::new(),
            })
            .unwrap();
        let mut exit = process.subscribe_exit();

        process.kill().unwrap();
        let tree_exited = process
            .wait_for_process_tree_exit(Duration::from_secs(3))
            .expect("process-tree wait");
        if cfg!(windows) {
            assert_eq!(tree_exited, Some(true));
        } else {
            assert_eq!(tree_exited, None);
        }
        tokio::time::timeout(Duration::from_secs(3), exit.changed())
            .await
            .unwrap()
            .unwrap();
        assert!(exit.borrow().is_some());
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn natural_root_exit_kills_late_hup_resistant_descendant_before_exit_publication() {
        let directory = tempfile::tempdir().expect("late descendant fixture");
        let descendant_release = directory.path().join("release-descendant");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&descendant_release)
                .status()
                .expect("mkfifo starts")
                .success(),
            "late-descendant release FIFO is created"
        );
        let process = PortablePtyBackend
            .spawn(&PtySpawnInput {
                executable: "/bin/sh".to_owned(),
                args: vec![
                    "-c".to_owned(),
                    "trap 'exit 0' USR1; CODDY_STUDIO_ROOT_PID=$$; export CODDY_STUDIO_ROOT_PID; /bin/sh -c 'trap \"\" HUP; printf \"\\nCODDY_STUDIO_DESCENDANT_PID=%s\\n\" \"$$\"; IFS= read -r _ < \"$CODDY_STUDIO_RELEASE_FIFO\"; kill -USR1 \"$CODDY_STUDIO_ROOT_PID\"; exec sleep 30' & wait"
                        .to_owned(),
                ],
                cwd: directory.path().to_path_buf(),
                cols: 80,
                rows: 24,
                env: BTreeMap::from([(
                    "CODDY_STUDIO_RELEASE_FIFO".to_owned(),
                    descendant_release.to_string_lossy().into_owned(),
                )]),
            })
            .expect("spawn natural-exit PTY root");
        let descendant_identity_marker = "CODDY_STUDIO_DESCENDANT_PID=";
        let mut output = process.subscribe_output();
        let mut exit = process.subscribe_exit();
        let descendant_pid = tokio::time::timeout(Duration::from_secs(3), async {
            let mut observed = String::new();
            loop {
                observed.push_str(&output.recv().await.expect("PTY output remains open"));
                if let Some(pid) = observed
                    .lines()
                    .find_map(|line| line.trim().strip_prefix(descendant_identity_marker))
                {
                    return pid.parse::<u32>().expect("numeric late descendant PID");
                }
            }
        })
        .await
        .expect("late descendant publishes its framed PID");
        let descendant_identity = NativeProcessSampler::process_identity(descendant_pid)
            .expect("capture late descendant identity");
        std::fs::OpenOptions::new()
            .write(true)
            .open(&descendant_release)
            .expect("open late-descendant release FIFO")
            .write_all(b"release\n")
            .expect("release late descendant after identity capture");

        tokio::time::timeout(Duration::from_secs(3), exit.changed())
            .await
            .expect("root exit publication outer bound")
            .expect("root exit publisher remains live");
        assert!(exit.borrow().is_some());
        // Exit publication is the ownership-release boundary. A zombie may
        // remain briefly visible after SIGKILL, but it is no longer a live
        // escape; distinguish that state through the native process status.
        let sampler = NativeProcessSampler::default();
        let descendant_is_live = sampler
            .collect_rows()
            .await
            .expect("sample descendant status")
            .into_iter()
            .find(|row| {
                row.pid == descendant_identity.pid
                    && row.started_at == descendant_identity.started_at
            })
            .is_some_and(|row| !row.status.to_ascii_lowercase().contains("zomb"));
        assert!(
            !descendant_is_live,
            "late same-group descendant survived root exit publication"
        );
    }

    #[test]
    fn portable_backend_rejects_a_missing_executable_before_opening_a_pty() {
        let error = PortablePtyBackend
            .spawn(&PtySpawnInput {
                executable: "/definitely/missing/coddy-studio-shell".to_owned(),
                args: Vec::new(),
                cwd: std::env::temp_dir(),
                cols: 80,
                rows: 24,
                env: BTreeMap::new(),
            })
            .unwrap_err();
        assert!(error.contains("terminal executable was not found"));
    }

    #[test]
    fn coalescing_pending_resizes_keeps_the_newest_size() {
        let (resize, requests) = mpsc::channel();
        let size = |cols, rows| PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };

        // A window drag queues a burst while one resize is still in flight.
        resize.send(size(186, 57)).unwrap();
        resize.send(size(196, 63)).unwrap();
        resize.send(size(206, 69)).unwrap();

        let first = requests.recv().unwrap();
        assert_eq!(coalesce_pending_resizes(first, &requests), size(206, 69));
        assert!(requests.try_recv().is_err(), "backlog must be drained");
    }

    #[test]
    fn coalescing_pending_resizes_passes_through_a_lone_size() {
        let (resize, requests) = mpsc::channel::<PtySize>();
        let only = PtySize {
            rows: 30,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        };
        resize.send(only).unwrap();

        let first = requests.recv().unwrap();
        assert_eq!(coalesce_pending_resizes(first, &requests), only);
    }

    #[cfg(not(windows))]
    #[test]
    fn portable_process_queues_every_resize_including_the_final_size() {
        // Regression: a bounded queue silently discarded requests once full and
        // still reported success, leaving the PTY at an intermediate size while
        // the session recorded the requested one. A full-screen TUI then anchors
        // its bottom rows to the stale row count and never repaints.
        let (resize, resize_requests) = mpsc::channel();
        let (output, initial_output) = broadcast::channel(1);
        let (exit, _) = watch::channel(None);
        let process = PortablePtyProcess {
            osc_color_responder_active: false,
            pid: 44,
            process_identity: None,
            resize,
            writer: Arc::new(Mutex::new(Box::new(TestWriter::WriteError))),
            killer: Mutex::new(Box::new(TestKiller { fail: false })),
            output,
            initial_output: Mutex::new(Some(initial_output)),
            exit,
            exit_status: Arc::default(),
            #[cfg(unix)]
            process_group: None,
            #[cfg(unix)]
            process_group_ownership: Arc::new(Mutex::new(UnixProcessGroupOwnership::Released)),
        };

        // Nothing drains while these are issued, exactly as when the worker is
        // blocked inside a slow ConPTY resize.
        process.resize(186, 57).unwrap();
        process.resize(196, 63).unwrap();
        process.resize(206, 69).unwrap();

        let first = resize_requests.recv().unwrap();
        let applied = coalesce_pending_resizes(first, &resize_requests);
        assert_eq!((applied.cols, applied.rows), (206, 69));
    }

    #[cfg(not(windows))]
    #[test]
    fn portable_process_reports_writer_resize_and_killer_failures() {
        let (resize, resize_requests) = mpsc::channel();
        let (output, initial_output) = broadcast::channel(1);
        let (exit, _) = watch::channel(None);
        let process = PortablePtyProcess {
            osc_color_responder_active: false,
            pid: 42,
            process_identity: None,
            resize,
            writer: Arc::new(Mutex::new(Box::new(TestWriter::WriteError))),
            killer: Mutex::new(Box::new(TestKiller { fail: true })),
            output,
            initial_output: Mutex::new(Some(initial_output)),
            exit,
            exit_status: Arc::default(),
            #[cfg(unix)]
            process_group: None,
            #[cfg(unix)]
            process_group_ownership: Arc::new(Mutex::new(UnixProcessGroupOwnership::Released)),
        };

        assert!(process.write("data").unwrap_err().contains("write failed"));
        *process.writer.lock().unwrap() = Box::new(TestWriter::FlushError);
        assert!(process.write("data").unwrap_err().contains("flush failed"));

        process.resize(80, 24).unwrap();
        process.resize(100, 40).unwrap();
        drop(resize_requests);
        assert_eq!(
            process.resize(120, 50).unwrap_err(),
            "PTY resize worker is not available"
        );

        assert!(process.kill().unwrap_err().contains("kill failed"));
        let mut cloned_killer = process.killer.lock().unwrap().clone_killer();
        assert!(
            cloned_killer
                .kill()
                .unwrap_err()
                .to_string()
                .contains("kill failed")
        );
        *process.killer.lock().unwrap() = Box::new(TestKiller { fail: false });
        process.kill().unwrap();
    }

    #[cfg(not(windows))]
    #[test]
    fn portable_process_reports_poisoned_writer_and_killer_locks() {
        let (resize, _resize_requests) = mpsc::channel();
        let (output, initial_output) = broadcast::channel(1);
        let (exit, _) = watch::channel(None);
        let process = PortablePtyProcess {
            osc_color_responder_active: false,
            pid: 43,
            process_identity: None,
            resize,
            writer: Arc::new(Mutex::new(Box::new(TestWriter::FlushError))),
            killer: Mutex::new(Box::new(TestKiller { fail: false })),
            output,
            initial_output: Mutex::new(Some(initial_output)),
            exit,
            exit_status: Arc::default(),
            #[cfg(unix)]
            process_group: None,
            #[cfg(unix)]
            process_group_ownership: Arc::new(Mutex::new(UnixProcessGroupOwnership::Released)),
        };

        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _guard = process.writer.lock().unwrap();
            panic!("poison writer");
        }));
        assert!(process.write("data").unwrap_err().contains("poisoned lock"));

        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _guard = process.killer.lock().unwrap();
            panic!("poison killer");
        }));
        assert!(process.kill().unwrap_err().contains("poisoned lock"));
    }
}
