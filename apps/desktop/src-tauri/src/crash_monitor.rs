//! The host's crash monitor. The host owns the process's one crash hook, so its in-process
//! server runs no monitor of its own; this one follows the local environment's
//! `enableProfiler` instead.

use std::sync::OnceLock;

use bibcode_server::diagnostics::profiler::crash;
use tokio::sync::watch;

/// The local environment's setting as last seen. The monitor follows it.
static SETTING: OnceLock<watch::Sender<bool>> = OnceLock::new();

/// Before Tauri starts: arms from the local settings file, so a crash during startup is
/// captured once the profiler is on.
pub(crate) fn start() {
    let Some((dir, enabled)) = crash::desktop_local_profiler() else {
        return;
    };
    let (setting, follower) = watch::channel(enabled);
    if SETTING.set(setting).is_ok() {
        tauri::async_runtime::spawn(crash::follow(
            crash::MonitorDetector::new(dir, "desktop"),
            follower,
        ));
    }
}

/// From then on the in-process server's committed setting drives the monitor, for as long as
/// that server runs.
pub(crate) fn follow_server(handle: &bibcode_server::ServerHandle) {
    let (Some(setting), Some(mut committed)) = (SETTING.get(), handle.profiler_enabled_signal())
    else {
        return;
    };
    tauri::async_runtime::spawn(async move {
        loop {
            setting.send_replace(*committed.borrow_and_update());
            if committed.changed().await.is_err() {
                break;
            }
        }
    });
}
