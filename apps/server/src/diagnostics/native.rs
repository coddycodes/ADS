use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    sync::{Arc, Mutex},
};

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use thiserror::Error;

use crate::process::ProcessCleanupReport;

use super::{
    PROCESS_COMMAND_MAX_SCALARS, ProcessIdentity, ProcessRow, ProcessSampler, SamplingError,
    bound_diagnostic_string, build_descendant_entries,
};

#[derive(Debug)]
pub struct NativeProcessSampler {
    system: Arc<Mutex<System>>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct RuntimeProcessOwnership {
    roots: Vec<ProcessIdentity>,
    captured: Vec<ProcessIdentity>,
}

impl RuntimeProcessOwnership {
    pub(crate) fn roots_only(roots: Vec<ProcessIdentity>) -> Self {
        Self {
            roots,
            captured: Vec::new(),
        }
    }

    fn capture(rows: &[ProcessRow], roots: Vec<ProcessIdentity>) -> Self {
        let captured = runtime_owned_process_identities(rows, &roots);
        Self { roots, captured }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeProcessRecord {
    pub identity: ProcessIdentity,
    pub ppid: u32,
}

impl Default for NativeProcessSampler {
    fn default() -> Self {
        Self {
            system: Arc::new(Mutex::new(System::new())),
        }
    }
}

impl ProcessSampler for NativeProcessSampler {
    fn sample(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<ProcessRow>, SamplingError>> + Send + '_>,
    > {
        self.collect_rows()
    }
}

impl NativeProcessSampler {
    pub fn process_record(pid: u32) -> Result<NativeProcessRecord, SamplingError> {
        let record = platform_process_record(pid)
            .map_err(|error| SamplingError::Failed(error.to_string()))?;
        Ok(NativeProcessRecord {
            identity: ProcessIdentity {
                pid,
                started_at: record.started_at,
            },
            ppid: record.ppid,
        })
    }

    pub(crate) fn process_identity(pid: u32) -> Result<ProcessIdentity, SamplingError> {
        Self::process_record(pid).map(|record| record.identity)
    }

    pub(crate) fn collect_rows(
        &self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<ProcessRow>, SamplingError>> + Send + '_>,
    > {
        let system = self.system.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || collect_rows(&system))
                .await
                .map_err(|error| SamplingError::Failed(error.to_string()))?
        })
    }

    pub(super) fn signal_process(
        &self,
        expected_identity: ProcessIdentity,
        signal: ProcessSignal,
    ) -> Result<(), SignalError> {
        signal_process_identity(expected_identity, signal)
    }

    pub(crate) async fn capture_runtime_process_ownership(
        &self,
        roots: Vec<ProcessIdentity>,
    ) -> Result<RuntimeProcessOwnership, SamplingError> {
        if roots.is_empty() {
            return Ok(RuntimeProcessOwnership::roots_only(roots));
        }
        let rows = self.sample().await?;
        Ok(RuntimeProcessOwnership::capture(&rows, roots))
    }

    pub(crate) async fn cleanup_runtime_process_ownership(
        &self,
        ownership: RuntimeProcessOwnership,
    ) -> Result<ProcessCleanupReport, SignalError> {
        if ownership.roots.is_empty() && ownership.captured.is_empty() {
            return Ok(ProcessCleanupReport::default());
        }
        let rows = self
            .sample()
            .await
            .map_err(|error| SignalError::Read(error.to_string()))?;
        let identities = residual_runtime_owned_process_identities(&rows, &ownership);
        Ok(cleanup_runtime_process_identities(
            &identities,
            |identity| signal_process_identity(identity, ProcessSignal::Kill),
        ))
    }

    pub(crate) async fn cleanup_descendants(
        &self,
        root_pid: u32,
    ) -> Result<ProcessCleanupReport, SignalError> {
        let rows = self
            .sample()
            .await
            .map_err(|error| SignalError::Read(error.to_string()))?;
        let mut descendants = build_descendant_entries(&rows, root_pid);
        descendants.sort_by_key(|entry| std::cmp::Reverse(entry.depth));
        let identities = descendants
            .into_iter()
            .filter_map(|entry| {
                rows.iter()
                    .find(|row| row.pid == entry.pid)
                    .map(|row| ProcessIdentity {
                        pid: row.pid,
                        started_at: row.started_at,
                    })
            })
            .collect::<Vec<_>>();
        Ok(cleanup_runtime_process_identities(
            &identities,
            |identity| signal_process_identity(identity, ProcessSignal::Kill),
        ))
    }
}

fn runtime_owned_process_identities(
    rows: &[ProcessRow],
    roots: &[ProcessIdentity],
) -> Vec<ProcessIdentity> {
    let rows_by_pid = rows
        .iter()
        .map(|row| (row.pid, row))
        .collect::<HashMap<_, _>>();
    let mut children_by_pid = HashMap::<u32, Vec<&ProcessRow>>::new();
    for row in rows {
        children_by_pid.entry(row.ppid).or_default().push(row);
    }
    for children in children_by_pid.values_mut() {
        children.sort_by_key(|row| row.pid);
    }

    fn visit(
        identity: ProcessIdentity,
        rows_by_pid: &HashMap<u32, &ProcessRow>,
        children_by_pid: &HashMap<u32, Vec<&ProcessRow>>,
        visited: &mut HashSet<ProcessIdentity>,
        owned: &mut Vec<ProcessIdentity>,
    ) {
        let Some(row) = rows_by_pid.get(&identity.pid) else {
            return;
        };
        if row.started_at != identity.started_at || !visited.insert(identity) {
            return;
        }
        for child in children_by_pid
            .get(&identity.pid)
            .into_iter()
            .flatten()
            .filter(|child| child.started_at >= identity.started_at)
        {
            visit(
                ProcessIdentity {
                    pid: child.pid,
                    started_at: child.started_at,
                },
                rows_by_pid,
                children_by_pid,
                visited,
                owned,
            );
        }
        owned.push(identity);
    }

    let mut visited = HashSet::new();
    let mut owned = Vec::new();
    for root in roots {
        visit(
            *root,
            &rows_by_pid,
            &children_by_pid,
            &mut visited,
            &mut owned,
        );
    }
    owned
}

fn residual_runtime_owned_process_identities(
    rows: &[ProcessRow],
    ownership: &RuntimeProcessOwnership,
) -> Vec<ProcessIdentity> {
    let live = rows
        .iter()
        .map(|row| ProcessIdentity {
            pid: row.pid,
            started_at: row.started_at,
        })
        .collect::<HashSet<_>>();
    let mut residual = runtime_owned_process_identities(rows, &ownership.roots);
    let mut included = residual.iter().copied().collect::<HashSet<_>>();
    residual.extend(
        ownership
            .captured
            .iter()
            .copied()
            .filter(|identity| live.contains(identity) && included.insert(*identity)),
    );
    residual
}

fn cleanup_runtime_process_identities(
    identities: &[ProcessIdentity],
    mut signal: impl FnMut(ProcessIdentity) -> Result<(), SignalError>,
) -> ProcessCleanupReport {
    let mut report = ProcessCleanupReport::default();
    for identity in identities {
        match signal(*identity) {
            Ok(()) | Err(SignalError::NotFound(_) | SignalError::StaleIdentity(_)) => {
                report.record_success();
            }
            Err(error) => {
                report.record_failure(format!("process {}: {error}", identity.pid));
            }
        }
    }
    report
}

#[derive(Debug)]
struct ProcessPresentation {
    pid: u32,
    status: String,
    cpu_percent: f32,
    cpu_core_percent: Option<f32>,
    rss_bytes: u64,
    elapsed: String,
    command: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PlatformProcessRecord {
    started_at: u64,
    ppid: u32,
}

fn combine_process_observations(
    presentation: ProcessPresentation,
    platform: PlatformProcessRecord,
) -> ProcessRow {
    ProcessRow {
        pid: presentation.pid,
        started_at: platform.started_at,
        ppid: platform.ppid,
        pgid: None,
        status: presentation.status,
        cpu_percent: presentation.cpu_percent,
        cpu_core_percent: presentation.cpu_core_percent,
        rss_bytes: presentation.rss_bytes,
        elapsed: presentation.elapsed,
        command: presentation.command,
    }
}

fn collect_rows(system: &Arc<Mutex<System>>) -> Result<Vec<ProcessRow>, SamplingError> {
    let mut system = system
        .lock()
        .map_err(|error| SamplingError::Failed(error.to_string()))?;
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_cpu()
            .with_memory()
            .with_cmd(UpdateKind::OnlyIfNotSet)
            .with_exe(UpdateKind::OnlyIfNotSet),
    );
    let processors = std::thread::available_parallelism()
        .map(|count| count.get() as f32)
        .unwrap_or(1.0)
        .max(1.0);
    let mut rows = system
        .processes()
        .iter()
        .filter_map(|(pid, process)| {
            let pid = pid.as_u32();
            if pid == 0 || process.thread_kind().is_some() {
                return None;
            }
            let raw_cpu = process.cpu_usage().max(0.0);
            let presentation = ProcessPresentation {
                pid,
                status: format!("{:?}", process.status()),
                cpu_percent: if cfg!(windows) {
                    (raw_cpu / processors).clamp(0.0, 100.0)
                } else {
                    raw_cpu
                },
                cpu_core_percent: Some(raw_cpu),
                rss_bytes: process.memory(),
                elapsed: format_elapsed(process.run_time()),
                command: command_string(process.cmd(), process.name()),
            };
            let platform = platform_process_record(pid).ok()?;
            Some(combine_process_observations(presentation, platform))
        })
        .collect::<Vec<_>>();
    rows.sort_by_key(|row| row.pid);
    Ok(rows)
}

#[cfg(target_os = "linux")]
fn platform_process_record(pid: u32) -> std::io::Result<PlatformProcessRecord> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    parse_linux_process_record(&stat)
}

#[cfg(target_os = "linux")]
fn parse_linux_process_record(stat: &str) -> std::io::Result<PlatformProcessRecord> {
    let command_end = stat.rfind(')').ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Linux process stat has no command terminator",
        )
    })?;
    let fields = stat
        .get(command_end + 1..)
        .into_iter()
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>();
    let ppid = fields
        .get(1)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Linux process stat has no parent field",
            )
        })?
        .parse::<u32>()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let started_at = fields
        .get(19)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Linux process stat has no start-time field",
            )
        })?
        .parse::<u64>()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    Ok(PlatformProcessRecord { started_at, ppid })
}

#[cfg(target_os = "macos")]
fn platform_process_record(pid: u32) -> std::io::Result<PlatformProcessRecord> {
    use std::ffi::{c_int, c_void};

    const PROC_PIDTBSDINFO: c_int = 3;

    #[repr(C)]
    struct ProcBsdInfo {
        pbi_flags: u32,
        pbi_status: u32,
        pbi_xstatus: u32,
        pbi_pid: u32,
        pbi_ppid: u32,
        pbi_uid: u32,
        pbi_gid: u32,
        pbi_ruid: u32,
        pbi_rgid: u32,
        pbi_svuid: u32,
        pbi_svgid: u32,
        rfu_1: u32,
        pbi_comm: [i8; 16],
        pbi_name: [i8; 32],
        pbi_nfiles: u32,
        pbi_pgid: u32,
        pbi_pjobc: u32,
        e_tdev: u32,
        e_tpgid: u32,
        pbi_nice: i32,
        pbi_start_tvsec: u64,
        pbi_start_tvusec: u64,
    }

    unsafe extern "C" {
        fn proc_pidinfo(
            pid: c_int,
            flavor: c_int,
            arg: u64,
            buffer: *mut c_void,
            buffersize: c_int,
        ) -> c_int;
    }

    // SAFETY: `ProcBsdInfo` is a C-compatible plain-data buffer matching
    // `proc_bsdinfo`; `proc_pidinfo` receives its exact address and size.
    let mut info = unsafe { std::mem::zeroed::<ProcBsdInfo>() };
    let size = std::mem::size_of::<ProcBsdInfo>();
    let read = unsafe {
        proc_pidinfo(
            i32::try_from(pid)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?,
            PROC_PIDTBSDINFO,
            0,
            std::ptr::from_mut(&mut info).cast(),
            i32::try_from(size).expect("proc_bsdinfo size fits c_int"),
        )
    };
    if read != i32::try_from(size).expect("proc_bsdinfo size fits c_int") {
        return Err(std::io::Error::last_os_error());
    }
    Ok(PlatformProcessRecord {
        started_at: macos_process_creation_identity(info.pbi_start_tvsec, info.pbi_start_tvusec)?,
        ppid: info.pbi_ppid,
    })
}

#[cfg(target_os = "macos")]
fn macos_process_creation_identity(seconds: u64, microseconds: u64) -> std::io::Result<u64> {
    seconds
        .checked_mul(1_000_000)
        .and_then(|seconds| seconds.checked_add(microseconds))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "macOS process creation identity overflowed",
            )
        })
}

#[cfg(windows)]
fn platform_process_record(pid: u32) -> std::io::Result<PlatformProcessRecord> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, STILL_ACTIVE},
        System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_INFORMATION},
    };

    // SAFETY: the requested access is query-only and the returned owned handle
    // is checked for null and closed exactly once below.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_INFORMATION, 0, pid) };
    if handle.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    let record = (|| {
        let mut exit_code = 0_u32;
        // SAFETY: `handle` is a live query handle and `exit_code` is valid
        // writable storage for the duration of the call.
        if unsafe { GetExitCodeProcess(handle, &mut exit_code) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        if exit_code != u32::try_from(STILL_ACTIVE).expect("STILL_ACTIVE fits u32") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("process {pid} has exited"),
            ));
        }
        windows_process_record(handle)
    })();
    // SAFETY: `handle` was opened successfully above and is closed once.
    unsafe { CloseHandle(handle) };
    record
}

#[cfg(windows)]
fn windows_process_record(
    handle: windows_sys::Win32::Foundation::HANDLE,
) -> std::io::Result<PlatformProcessRecord> {
    use windows_sys::{
        Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation},
        Win32::System::Threading::PROCESS_BASIC_INFORMATION,
    };

    let started_at = windows_process_creation_identity(handle)?;
    let mut basic = PROCESS_BASIC_INFORMATION::default();
    // SAFETY: `basic` is an initialized output buffer of the declared size and
    // `handle` remains owned and live for the complete record query.
    let status = unsafe {
        NtQueryInformationProcess(
            handle,
            ProcessBasicInformation,
            std::ptr::from_mut(&mut basic).cast(),
            u32::try_from(std::mem::size_of::<PROCESS_BASIC_INFORMATION>())
                .expect("PROCESS_BASIC_INFORMATION size fits u32"),
            std::ptr::null_mut(),
        )
    };
    if status < 0 {
        return Err(std::io::Error::other(format!(
            "NtQueryInformationProcess failed with NTSTATUS {status:#x}"
        )));
    }
    Ok(PlatformProcessRecord {
        started_at,
        ppid: u32::try_from(basic.InheritedFromUniqueProcessId)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?,
    })
}

#[cfg(windows)]
fn windows_process_creation_identity(
    handle: windows_sys::Win32::Foundation::HANDLE,
) -> std::io::Result<u64> {
    use windows_sys::Win32::{Foundation::FILETIME, System::Threading::GetProcessTimes};

    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: all pointers reference initialized FILETIME storage and `handle`
    // remains owned and live for the duration of the call.
    let read = unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) };
    let error = (read == 0).then(std::io::Error::last_os_error);
    if let Some(error) = error {
        return Err(error);
    }
    Ok(windows_filetime_identity(creation))
}

#[cfg(windows)]
fn windows_filetime_identity(filetime: windows_sys::Win32::Foundation::FILETIME) -> u64 {
    (u64::from(filetime.dwHighDateTime) << 32) | u64::from(filetime.dwLowDateTime)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn platform_process_record(_pid: u32) -> std::io::Result<PlatformProcessRecord> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "high-fidelity process identity is unavailable on this platform",
    ))
}

#[cfg(target_os = "linux")]
fn platform_process_creation_identity(pid: u32) -> std::io::Result<u64> {
    platform_process_record(pid).map(|record| record.started_at)
}

#[cfg(any(windows, test))]
const WINDOWS_ERROR_ACCESS_DENIED: i32 = 5;
#[cfg(any(windows, test))]
const WINDOWS_ERROR_INVALID_HANDLE: i32 = 6;
#[cfg(any(windows, test))]
const WINDOWS_ERROR_INVALID_PARAMETER: i32 = 87;
#[cfg(any(windows, test))]
const WINDOWS_ERROR_NOT_FOUND: i32 = 1_168;

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WindowsProcessOperation {
    Open,
    QueryCreationTime,
}

#[cfg(any(windows, test))]
fn classify_windows_process_error(
    pid: u32,
    operation: WindowsProcessOperation,
    error_code: i32,
) -> SignalError {
    if error_code == WINDOWS_ERROR_ACCESS_DENIED {
        return SignalError::Rejected(pid);
    }
    if operation == WindowsProcessOperation::Open
        && matches!(
            error_code,
            WINDOWS_ERROR_INVALID_PARAMETER | WINDOWS_ERROR_NOT_FOUND
        )
    {
        return SignalError::NotFound(pid);
    }
    if operation == WindowsProcessOperation::QueryCreationTime
        && error_code == WINDOWS_ERROR_INVALID_HANDLE
    {
        return SignalError::Read(format!(
            "GetProcessTimes failed with Win32 error {error_code}"
        ));
    }
    let operation = match operation {
        WindowsProcessOperation::Open => "OpenProcess",
        WindowsProcessOperation::QueryCreationTime => "GetProcessTimes",
    };
    SignalError::Read(format!("{operation} failed with Win32 error {error_code}"))
}

trait IdentityBoundProcess: std::fmt::Debug {
    fn creation_identity(&self) -> Result<u64, SignalError>;

    fn signal(&self, signal: ProcessSignal) -> Result<(), SignalError>;
}

fn signal_process_identity(
    expected_identity: ProcessIdentity,
    signal: ProcessSignal,
) -> Result<(), SignalError> {
    let process = open_identity_bound_process(expected_identity.pid)?;
    signal_identity_bound_process(process.as_ref(), expected_identity, signal)
}

fn signal_identity_bound_process(
    process: &dyn IdentityBoundProcess,
    expected_identity: ProcessIdentity,
    signal: ProcessSignal,
) -> Result<(), SignalError> {
    if process.creation_identity()? != expected_identity.started_at {
        return Err(SignalError::StaleIdentity(expected_identity.pid));
    }
    // The user stops it on purpose: its owner must not report the exit as a crash.
    match signal {
        ProcessSignal::Kill => super::profiler::exits::note_stop_requested(expected_identity.pid),
        ProcessSignal::Interrupt => {
            super::profiler::exits::note_interrupt_requested(expected_identity.pid);
        }
    }
    let signalled = process.signal(signal);
    if signalled.is_err() {
        super::profiler::exits::withdraw_stop_request(expected_identity.pid);
    }
    signalled
}

#[cfg(target_os = "linux")]
#[derive(Debug)]
struct LinuxIdentityBoundProcess {
    pid: u32,
    pidfd: std::os::fd::OwnedFd,
}

#[cfg(target_os = "linux")]
impl IdentityBoundProcess for LinuxIdentityBoundProcess {
    fn creation_identity(&self) -> Result<u64, SignalError> {
        platform_process_creation_identity(self.pid).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                SignalError::NotFound(self.pid)
            } else {
                SignalError::Read(error.to_string())
            }
        })
    }

    fn signal(&self, signal: ProcessSignal) -> Result<(), SignalError> {
        use std::os::fd::AsRawFd;

        let signal = match signal {
            ProcessSignal::Interrupt => libc::SIGINT,
            ProcessSignal::Kill => libc::SIGKILL,
        };
        // SAFETY: `pidfd` is an owned pidfd, the signal is a valid platform
        // constant, and null siginfo asks the kernel to synthesize it.
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.pidfd.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0_u32,
            )
        };
        if result == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::ESRCH) => Err(SignalError::NotFound(self.pid)),
            Some(libc::ENOSYS) => Err(SignalError::Unsupported),
            _ => Err(SignalError::Rejected(self.pid)),
        }
    }
}

#[cfg(target_os = "linux")]
fn open_identity_bound_process(pid: u32) -> Result<Box<dyn IdentityBoundProcess>, SignalError> {
    use std::os::fd::FromRawFd;

    let platform_pid = i32::try_from(pid).map_err(|_| SignalError::NotFound(pid))?;
    // SAFETY: `pidfd_open` takes an integer PID and zero flags. A nonnegative
    // return value is a newly owned file descriptor.
    let descriptor = unsafe { libc::syscall(libc::SYS_pidfd_open, platform_pid, 0_u32) };
    if descriptor < 0 {
        let error = std::io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(libc::ESRCH) => Err(SignalError::NotFound(pid)),
            Some(libc::ENOSYS) | Some(libc::EINVAL) => Err(SignalError::Unsupported),
            _ => Err(SignalError::Read(error.to_string())),
        };
    }
    let descriptor = i32::try_from(descriptor)
        .map_err(|error| SignalError::Read(format!("invalid pidfd: {error}")))?;
    // SAFETY: the successful syscall returned this newly owned descriptor.
    let pidfd = unsafe { std::os::fd::OwnedFd::from_raw_fd(descriptor) };
    Ok(Box::new(LinuxIdentityBoundProcess { pid, pidfd }))
}

#[cfg(windows)]
#[derive(Debug)]
struct WindowsIdentityBoundProcess {
    pid: u32,
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl Drop for WindowsIdentityBoundProcess {
    fn drop(&mut self) {
        // SAFETY: this type owns the non-null process handle and closes it once.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.handle) };
    }
}

#[cfg(windows)]
impl IdentityBoundProcess for WindowsIdentityBoundProcess {
    fn creation_identity(&self) -> Result<u64, SignalError> {
        windows_process_creation_identity(self.handle).map_err(|error| {
            classify_windows_process_error(
                self.pid,
                WindowsProcessOperation::QueryCreationTime,
                error.raw_os_error().unwrap_or_default(),
            )
        })
    }

    fn signal(&self, signal: ProcessSignal) -> Result<(), SignalError> {
        if signal == ProcessSignal::Interrupt {
            return Err(SignalError::Unsupported);
        }
        // SAFETY: the owned handle remains live and was opened with
        // PROCESS_TERMINATE access.
        if unsafe { windows_sys::Win32::System::Threading::TerminateProcess(self.handle, 1) } == 0 {
            Err(SignalError::Rejected(self.pid))
        } else {
            Ok(())
        }
    }
}

#[cfg(windows)]
fn open_identity_bound_process(pid: u32) -> Result<Box<dyn IdentityBoundProcess>, SignalError> {
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    };

    // SAFETY: the access mask requests a queryable/terminable process object
    // and the returned handle is owned by `WindowsIdentityBoundProcess`.
    let handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
            0,
            pid,
        )
    };
    if handle.is_null() {
        let error = std::io::Error::last_os_error();
        return Err(classify_windows_process_error(
            pid,
            WindowsProcessOperation::Open,
            error.raw_os_error().unwrap_or_default(),
        ));
    }
    Ok(Box::new(WindowsIdentityBoundProcess { pid, handle }))
}

#[cfg(any(
    target_os = "macos",
    not(any(target_os = "linux", target_os = "macos", windows))
))]
fn open_identity_bound_process(_pid: u32) -> Result<Box<dyn IdentityBoundProcess>, SignalError> {
    Err(SignalError::Unsupported)
}

fn command_string(parts: &[impl AsRef<OsStr>], fallback: &OsStr) -> String {
    let joined = parts
        .iter()
        .map(|part| part.as_ref().to_string_lossy())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let command = if joined.trim().is_empty() {
        fallback.to_string_lossy().into_owned()
    } else {
        joined
    };
    bound_diagnostic_string(&command, PROCESS_COMMAND_MAX_SCALARS)
}

fn format_elapsed(seconds: u64) -> String {
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;
    format!("{hours:02}:{minutes:02}:{seconds:02}")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessSignal {
    Interrupt,
    Kill,
}

#[derive(Debug, Error)]
pub enum SignalError {
    #[error("refusing to signal the server process")]
    ServerProcess,
    #[error("process {0} is not a live descendant of the server")]
    NotDescendant(u32),
    #[error("process {0} is not eligible for signaling")]
    NotEligible(u32),
    #[error("process {0} no longer has the expected identity")]
    StaleIdentity(u32),
    #[error("process {0} no longer exists")]
    NotFound(u32),
    #[error("the operating system does not support this signal")]
    Unsupported,
    #[error("the operating system rejected the signal for process {0}")]
    Rejected(u32),
    #[error("failed to read process state: {0}")]
    Read(String),
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    };

    use super::*;
    use crate::diagnostics::ProcessIdentity;

    #[derive(Debug)]
    struct FakeIdentityBoundProcess {
        bound_identity: u64,
        numeric_pid_identity: AtomicU64,
        signaled_identity: AtomicU64,
    }

    impl IdentityBoundProcess for FakeIdentityBoundProcess {
        fn creation_identity(&self) -> Result<u64, SignalError> {
            Ok(self.bound_identity)
        }

        fn signal(&self, _signal: ProcessSignal) -> Result<(), SignalError> {
            self.numeric_pid_identity
                .store(self.bound_identity + 1, Ordering::SeqCst);
            self.signaled_identity
                .store(self.bound_identity, Ordering::SeqCst);
            Ok(())
        }
    }

    fn locked_native_process_sampler() -> (
        NativeProcessSampler,
        mpsc::Sender<()>,
        std::thread::JoinHandle<()>,
    ) {
        let system = Arc::new(Mutex::new(System::new()));
        let system_to_lock = system.clone();
        let (locked_tx, locked_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::channel();
        let blocker = std::thread::spawn(move || {
            let _guard = system_to_lock
                .lock()
                .expect("fresh process sampler mutex should lock");
            locked_tx
                .send(())
                .expect("sampler lock publication should succeed");
            release_rx
                .recv()
                .expect("sampler lock release should arrive");
        });
        locked_rx
            .recv()
            .expect("sampler lock publication should arrive");
        (NativeProcessSampler { system }, release_tx, blocker)
    }

    #[tokio::test]
    async fn empty_runtime_ownership_capture_does_not_require_native_sampling() {
        let (sampler, release, blocker) = locked_native_process_sampler();
        let ownership = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            sampler.capture_runtime_process_ownership(Vec::new()),
        )
        .await;
        release.send(()).expect("sampler lock release should send");
        blocker.join().expect("sampler lock holder should join");
        let ownership = ownership
            .expect("empty ownership capture should not wait for native sampling")
            .expect("empty ownership should not sample the native process table");

        assert!(ownership.roots.is_empty());
        assert!(ownership.captured.is_empty());
    }

    #[tokio::test]
    async fn empty_runtime_ownership_cleanup_does_not_require_native_sampling() {
        let (sampler, release, blocker) = locked_native_process_sampler();
        let report = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            sampler
                .cleanup_runtime_process_ownership(RuntimeProcessOwnership::roots_only(Vec::new())),
        )
        .await;
        release.send(()).expect("sampler lock release should send");
        blocker.join().expect("sampler lock holder should join");
        let report = report
            .expect("empty ownership cleanup should not wait for native sampling")
            .expect("empty ownership should not sample the native process table");

        assert_eq!(report, ProcessCleanupReport::default());
    }

    #[tokio::test]
    async fn native_sampler_defers_host_enumeration_until_the_first_requested_sample() {
        let sampler = NativeProcessSampler::default();
        assert!(
            sampler
                .system
                .lock()
                .expect("fresh process sampler should lock")
                .processes()
                .is_empty(),
            "constructing the boot sampler must not enumerate host processes"
        );

        let rows = sampler.sample().await.expect("first requested sample");
        let current = rows
            .iter()
            .find(|row| row.pid == std::process::id())
            .expect("first sample discovers the current process without a boot snapshot");
        assert!(!current.command.is_empty());
        assert!(current.rss_bytes > 0);
        assert!(current.cpu_percent.is_finite());
        assert!(current.started_at > 0);
        assert_eq!(
            current.ppid,
            platform_process_record(std::process::id())
                .expect("current process parent")
                .ppid
        );
    }

    #[tokio::test]
    async fn native_sampler_refreshes_repeatedly_and_keeps_the_current_process() {
        let sampler = NativeProcessSampler::default();

        for _ in 0..3 {
            let rows = sampler.sample().await.expect("processes should sample");
            let current = rows
                .iter()
                .find(|row| row.pid == std::process::id())
                .expect("current test process should remain visible");
            assert!(current.cpu_percent.is_finite());
            assert!(current.cpu_core_percent.is_some_and(f32::is_finite));
            assert!(current.rss_bytes > 0);
            assert!(current.started_at > 0);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn first_requested_sample_discovers_descendants_created_after_sampler_construction() {
        let sampler = NativeProcessSampler::default();
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn process sampling fixture");
        let child_pid = child.id();
        let sample = sampler.sample().await;
        let _ = child.kill();
        child.wait().expect("reap process sampling fixture");

        let rows = sample.expect("first requested process sample");
        let descendant = rows
            .iter()
            .find(|row| row.pid == child_pid)
            .expect("full requested sample discovers a previously unknown descendant");
        assert_eq!(descendant.ppid, std::process::id());
        assert!(descendant.command.contains("sleep"));
        assert!(descendant.started_at > 0);
        let ownership = runtime_owned_process_identities(
            &rows,
            &[NativeProcessSampler::process_identity(std::process::id())
                .expect("current process identity")],
        );
        assert!(ownership.contains(&ProcessIdentity {
            pid: child_pid,
            started_at: descendant.started_at,
        }));
    }

    #[tokio::test]
    async fn native_sampler_uses_the_platform_creation_identity() {
        let sampler = NativeProcessSampler::default();
        let rows = sampler.sample().await.expect("processes should sample");
        let current = rows
            .iter()
            .find(|row| row.pid == std::process::id())
            .expect("current test process should remain visible");
        let platform =
            platform_process_record(std::process::id()).expect("current platform process record");

        assert_eq!(current.started_at, platform.started_at);
        assert_eq!(current.ppid, platform.ppid);
    }

    #[test]
    fn process_identity_is_captured_without_waiting_for_a_full_sample() {
        let identity = NativeProcessSampler::process_identity(std::process::id())
            .expect("current process identity");
        let platform =
            platform_process_record(std::process::id()).expect("current platform process record");

        assert_eq!(identity.pid, std::process::id());
        assert_eq!(identity.started_at, platform.started_at);
    }

    #[cfg(windows)]
    #[test]
    fn process_identity_rejects_an_exited_process_with_a_retained_handle() {
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "exit", "0"])
            .spawn()
            .expect("spawn exited-process identity fixture");
        let pid = child.id();
        assert!(
            child
                .wait()
                .expect("wait for exited-process identity fixture")
                .success()
        );

        assert!(
            NativeProcessSampler::process_identity(pid).is_err(),
            "an exited Windows process must not be reported as a live identity"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn native_process_record_exposes_the_current_linux_identity_and_parent() {
        let record = NativeProcessSampler::process_record(std::process::id())
            .expect("current process record");
        let expected =
            platform_process_record(std::process::id()).expect("current platform record");

        assert_eq!(record.identity.pid, std::process::id());
        assert_eq!(record.identity.started_at, expected.started_at);
        assert_eq!(record.ppid, expected.ppid);
        assert_ne!(record.identity.started_at, 0);
    }

    #[test]
    fn collection_window_replacement_cannot_inherit_cached_ancestry() {
        let cached_process_a = ProcessPresentation {
            pid: 42,
            status: "cached-a".to_owned(),
            cpu_percent: 1.0,
            cpu_core_percent: Some(1.0),
            rss_bytes: 100,
            elapsed: "00:00:01".to_owned(),
            command: "cached-a".to_owned(),
        };
        let coherent_process_b = PlatformProcessRecord {
            started_at: 200,
            ppid: 1,
        };

        let row = combine_process_observations(cached_process_a, coherent_process_b);

        assert_eq!(row.started_at, 200);
        assert_eq!(row.ppid, 1);
        assert_eq!(row.command, "cached-a");
    }

    #[test]
    fn windows_process_errors_distinguish_missing_denied_and_unexpected_failures() {
        assert!(matches!(
            classify_windows_process_error(
                42,
                WindowsProcessOperation::Open,
                WINDOWS_ERROR_INVALID_PARAMETER,
            ),
            SignalError::NotFound(42)
        ));
        assert!(matches!(
            classify_windows_process_error(
                42,
                WindowsProcessOperation::Open,
                WINDOWS_ERROR_NOT_FOUND,
            ),
            SignalError::NotFound(42)
        ));
        assert!(matches!(
            classify_windows_process_error(
                42,
                WindowsProcessOperation::Open,
                WINDOWS_ERROR_ACCESS_DENIED,
            ),
            SignalError::Rejected(42)
        ));
        assert!(matches!(
            classify_windows_process_error(
                42,
                WindowsProcessOperation::QueryCreationTime,
                WINDOWS_ERROR_ACCESS_DENIED,
            ),
            SignalError::Rejected(42)
        ));
        assert!(matches!(
            classify_windows_process_error(
                42,
                WindowsProcessOperation::QueryCreationTime,
                WINDOWS_ERROR_INVALID_HANDLE,
            ),
            SignalError::Read(message) if message.contains("GetProcessTimes")
        ));
    }

    #[test]
    fn cleanup_fallback_continues_after_failures_and_bounds_its_report() {
        let identities = (1..=20)
            .map(|pid| ProcessIdentity {
                pid,
                started_at: u64::from(pid) * 100,
            })
            .collect::<Vec<_>>();
        let mut attempted = Vec::new();

        let report = cleanup_runtime_process_identities(&identities, |identity| {
            attempted.push(identity.pid);
            if identity.pid == 2 {
                Ok(())
            } else {
                Err(SignalError::Read("界".repeat(500)))
            }
        });

        assert_eq!(attempted, (1..=20).collect::<Vec<_>>());
        assert_eq!(report.attempted, 20);
        assert_eq!(report.succeeded, 1);
        assert_eq!(report.failure_count, 19);
        assert!(report.failures.len() < report.failure_count);
        assert!(
            report
                .failures
                .iter()
                .all(|failure| failure.chars().count() <= 160)
        );
    }

    #[test]
    fn owned_closure_excludes_a_peer_runtime_under_the_same_server_pid() {
        let mut server = ProcessRow::fixture(10, 1, "server");
        server.started_at = 10;
        let mut runtime_a = ProcessRow::fixture(20, 10, "runtime-a");
        runtime_a.started_at = 20;
        let mut runtime_a_child = ProcessRow::fixture(21, 20, "runtime-a-child");
        runtime_a_child.started_at = 21;
        let mut runtime_b = ProcessRow::fixture(30, 10, "runtime-b");
        runtime_b.started_at = 30;
        let mut runtime_b_child = ProcessRow::fixture(31, 30, "runtime-b-child");
        runtime_b_child.started_at = 31;

        let owned = runtime_owned_process_identities(
            &[
                server,
                runtime_a,
                runtime_a_child,
                runtime_b,
                runtime_b_child,
            ],
            &[ProcessIdentity {
                pid: 20,
                started_at: 20,
            }],
        );

        assert_eq!(
            owned,
            [
                ProcessIdentity {
                    pid: 21,
                    started_at: 21,
                },
                ProcessIdentity {
                    pid: 20,
                    started_at: 20,
                },
            ]
        );
    }

    #[test]
    fn owned_closure_rejects_pid_reuse_and_creation_time_inversion() {
        let mut replacement = ProcessRow::fixture(20, 10, "replacement");
        replacement.started_at = 200;
        let mut older_child = ProcessRow::fixture(21, 20, "older-child");
        older_child.started_at = 150;

        let owned = runtime_owned_process_identities(
            &[replacement, older_child],
            &[ProcessIdentity {
                pid: 20,
                started_at: 100,
            }],
        );

        assert!(owned.is_empty(), "a reused PID cannot inherit ownership");

        let matching_root = ProcessIdentity {
            pid: 20,
            started_at: 200,
        };
        let owned = runtime_owned_process_identities(
            &[
                ProcessRow {
                    started_at: 200,
                    ..ProcessRow::fixture(20, 10, "matching-root")
                },
                ProcessRow {
                    started_at: 150,
                    ..ProcessRow::fixture(21, 20, "older-child")
                },
            ],
            &[matching_root],
        );
        assert_eq!(
            owned,
            [matching_root],
            "a child older than its matching parent identity cannot inherit ownership"
        );
    }

    #[test]
    fn residual_selection_includes_descendants_forked_after_initial_capture() {
        let root_a = ProcessIdentity {
            pid: 20,
            started_at: 20,
        };
        let initial_rows = [ProcessRow {
            started_at: 20,
            ..ProcessRow::fixture(20, 10, "runtime-a")
        }];
        let ownership = RuntimeProcessOwnership::capture(&initial_rows, vec![root_a]);
        let residual_rows = [
            ProcessRow {
                started_at: 20,
                ..ProcessRow::fixture(20, 10, "runtime-a")
            },
            ProcessRow {
                started_at: 21,
                ..ProcessRow::fixture(21, 20, "late-a-child")
            },
            ProcessRow {
                started_at: 30,
                ..ProcessRow::fixture(30, 10, "runtime-b")
            },
        ];

        let residual = residual_runtime_owned_process_identities(&residual_rows, &ownership);

        assert_eq!(
            residual,
            [
                ProcessIdentity {
                    pid: 21,
                    started_at: 21,
                },
                root_a,
            ]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn same_second_process_replacements_have_distinct_creation_identities() {
        let first = macos_process_creation_identity(1_000, 1).expect("first identity");
        let replacement = macos_process_creation_identity(1_000, 2).expect("replacement identity");

        assert_ne!(first, replacement);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_start_ticks_distinguish_close_process_replacements() {
        let first = parse_linux_process_record(
            "42 (command with spaces) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 100",
        )
        .expect("first start ticks");
        let replacement = parse_linux_process_record(
            "42 (command with spaces) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 101",
        )
        .expect("replacement start ticks");

        assert_eq!(
            first,
            PlatformProcessRecord {
                started_at: 100,
                ppid: 1,
            }
        );
        assert_eq!(
            replacement,
            PlatformProcessRecord {
                started_at: 101,
                ppid: 1,
            }
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_filetime_distinguishes_close_process_replacements() {
        use windows_sys::Win32::Foundation::FILETIME;

        let first = windows_filetime_identity(FILETIME {
            dwLowDateTime: 100,
            dwHighDateTime: 1,
        });
        let replacement = windows_filetime_identity(FILETIME {
            dwLowDateTime: 101,
            dwHighDateTime: 1,
        });

        assert_ne!(first, replacement);
    }

    #[test]
    fn final_signal_stays_bound_to_the_verified_process_after_pid_replacement() {
        let target = FakeIdentityBoundProcess {
            bound_identity: 100,
            numeric_pid_identity: AtomicU64::new(100),
            signaled_identity: AtomicU64::new(0),
        };

        signal_identity_bound_process(
            &target,
            ProcessIdentity {
                pid: 42,
                started_at: 100,
            },
            ProcessSignal::Kill,
        )
        .expect("identity-bound signal");

        assert_eq!(target.numeric_pid_identity.load(Ordering::SeqCst), 101);
        assert_eq!(target.signaled_identity.load(Ordering::SeqCst), 100);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_signal_is_unsupported_without_an_identity_bound_primitive() {
        assert!(matches!(
            signal_process_identity(
                NativeProcessSampler::process_identity(std::process::id())
                    .expect("current identity"),
                ProcessSignal::Kill,
            ),
            Err(SignalError::Unsupported)
        ));
    }

    #[test]
    fn process_identity_distinguishes_reused_pids() {
        let first = ProcessRow::fixture(42, 1, "first");
        let mut second = ProcessRow::fixture(42, 1, "second");
        second.started_at = 1;

        let first_identity = ProcessIdentity {
            pid: first.pid,
            started_at: first.started_at,
        };
        let second_identity = ProcessIdentity {
            pid: second.pid,
            started_at: second.started_at,
        };

        assert_ne!(first_identity.key(), second_identity.key());
    }

    #[test]
    fn command_strings_are_bounded_without_breaking_utf8() {
        let command = command_string(&["é".repeat(513)], OsStr::new("fallback"));
        assert_eq!(command.chars().count(), 512);
    }

    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn identity_bound_signal_reports_missing_processes() {
        assert!(matches!(
            signal_process_identity(
                ProcessIdentity {
                    pid: u32::MAX,
                    started_at: 1,
                },
                ProcessSignal::Kill,
            ),
            Err(SignalError::NotFound(u32::MAX))
        ));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn native_sampler_excludes_linux_threads_from_process_rows() {
        let sampler = NativeProcessSampler::default();
        let rows = sampler.sample().await.expect("processes should sample");
        let thread_pids = sampler
            .system
            .lock()
            .expect("system should lock")
            .processes()
            .iter()
            .filter_map(|(pid, process)| process.thread_kind().map(|_| pid.as_u32()))
            .collect::<Vec<_>>();

        assert!(
            !thread_pids.is_empty(),
            "test process should expose threads"
        );
        assert!(
            rows.iter().all(|row| !thread_pids.contains(&row.pid)),
            "sampled process rows must not contain Linux threads"
        );
    }
}
