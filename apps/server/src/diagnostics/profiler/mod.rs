pub mod client;
pub mod crash;
pub mod event;
pub mod findings;
pub mod latency;
pub mod leak;
pub mod os_records;
pub mod panic_hook;
pub mod redact;
pub mod runtime;
pub mod sampler;
pub mod sentinel;
pub mod store;

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError, RwLock,
        atomic::{AtomicBool, Ordering},
    },
};

use serde_json::Value;

pub fn profiler_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("profiler")
}

/// The live profiler state that hot paths reach through [`global`].
pub struct ProfilerHandle {
    pub(crate) enabled: AtomicBool,
    pub(crate) sender: store::EventSender,
    pub(crate) findings: Mutex<findings::FindingsBook>,
    pub(crate) recording_since: Mutex<Option<String>>,
}

/// Replaceable rather than a `OnceLock`: the desktop host restarts the server in-process,
/// and the new runtime must take over from the one that shut down.
static HANDLE: RwLock<Option<Arc<ProfilerHandle>>> = RwLock::new(None);

/// Installs `handle` unless one is installed already, and returns the installed handle.
pub(crate) fn install(handle: Arc<ProfilerHandle>) -> Arc<ProfilerHandle> {
    HANDLE
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .get_or_insert(handle)
        .clone()
}

/// Clears the global handle if it is `handle`.
pub(crate) fn uninstall(handle: &Arc<ProfilerHandle>) {
    let mut slot = HANDLE.write().unwrap_or_else(PoisonError::into_inner);
    if slot
        .as_ref()
        .is_some_and(|installed| Arc::ptr_eq(installed, handle))
    {
        *slot = None;
    }
}

pub fn global() -> Option<Arc<ProfilerHandle>> {
    HANDLE
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

fn enabled_handle() -> Option<Arc<ProfilerHandle>> {
    global().filter(|handle| handle.enabled.load(Ordering::Relaxed))
}

/// Every RPC checks this, so it borrows the handle rather than cloning the `Arc`.
pub fn is_enabled() -> bool {
    HANDLE
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .is_some_and(|handle| handle.enabled.load(Ordering::Relaxed))
}

/// Queues one server event. Does nothing while the profiler is off; never blocks.
pub fn emit(kind: &'static str, payload: Value) {
    if let Some(handle) = enabled_handle() {
        handle.sender.send(event::ProfilerEvent {
            kind,
            source: "server".into(),
            payload,
        });
    }
}

pub fn emit_from(source: String, kind: &'static str, payload: Value) {
    if let Some(handle) = enabled_handle() {
        handle.sender.send(event::ProfilerEvent {
            kind,
            source,
            payload,
        });
    }
}

/// Records one finding occurrence. Does nothing while the profiler is off.
pub fn record_finding(input: findings::FindingInput) {
    if let Some(handle) = enabled_handle() {
        handle
            .findings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .record(input, time::OffsetDateTime::now_utc(), &handle.sender);
    }
}
