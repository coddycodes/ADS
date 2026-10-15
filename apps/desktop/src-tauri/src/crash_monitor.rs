//! The host's crash monitor. The host owns the process's one crash hook, so its in-process
//! server runs no monitor of its own; this one follows the local environment's
//! `enableProfiler` instead.

use std::{
    sync::{Mutex, MutexGuard, PoisonError},
    time::Duration,
};

use bibcode_server::diagnostics::profiler::crash;
use tokio::sync::watch;

/// The local environment's setting as last seen. The monitor follows it until [`stop`] drops
/// it.
static SETTING: Mutex<Option<watch::Sender<bool>>> = Mutex::new(None);
/// Ends once the monitor is stopped and reaped.
static FOLLOWER: Mutex<Option<tauri::async_runtime::JoinHandle<()>>> = Mutex::new(None);
/// Stopping kills and reaps one process; anything slower is abandoned.
const STOP_WAIT: Duration = Duration::from_secs(2);

fn lock<T>(slot: &Mutex<T>) -> MutexGuard<'_, T> {
    slot.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Before Tauri starts: arms from the local settings file, so a crash during startup is
/// captured once the profiler is on.
pub(crate) fn start() {
    let Some((dir, enabled)) = crash::desktop_local_profiler() else {
        return;
    };
    let mut setting = lock(&SETTING);
    if setting.is_some() {
        return;
    }
    let (sender, follower) = watch::channel(enabled);
    *setting = Some(sender);
    *lock(&FOLLOWER) = Some(tauri::async_runtime::spawn(crash::follow(
        crash::MonitorDetector::new(dir, "desktop"),
        follower,
    )));
}

/// From then on the in-process server's committed setting drives the monitor, for as long as
/// that server runs.
pub(crate) fn follow_server(handle: &bibcode_server::ServerHandle) {
    let Some(mut committed) = handle.profiler_enabled_signal() else {
        return;
    };
    tauri::async_runtime::spawn(async move {
        loop {
            let enabled = *committed.borrow_and_update();
            match lock(&SETTING).as_ref() {
                Some(setting) => setting.send_replace(enabled),
                None => break,
            };
            if committed.changed().await.is_err() {
                break;
            }
        }
    });
}

/// Stops the monitor and waits until it is reaped, for an exit it must not outlive. The
/// monitor ignores SIGTERM so that it covers a graceful shutdown, so it otherwise leaves only
/// once it sees this process gone.
pub(crate) fn stop() {
    drop(lock(&SETTING).take());
    let follower = lock(&FOLLOWER).take();
    // The timer needs the runtime: this runs on the event loop's thread.
    if let Some(follower) = follower
        && tauri::async_runtime::block_on(async { tokio::time::timeout(STOP_WAIT, follower).await })
            .is_err()
    {
        tracing::warn!("the crash monitor did not stop before exit");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    use super::*;

    /// Tauri's last exit callback runs on the event loop's thread, outside any runtime.
    #[test]
    fn stop_waits_for_the_follower_from_outside_the_runtime() {
        let (sender, mut setting) = watch::channel(true);
        let ended = Arc::new(AtomicBool::new(false));
        let follower_ended = ended.clone();
        *lock(&SETTING) = Some(sender);
        *lock(&FOLLOWER) = Some(tauri::async_runtime::spawn(async move {
            while setting.changed().await.is_ok() {}
            follower_ended.store(true, Ordering::SeqCst);
        }));
        stop();
        assert!(
            ended.load(Ordering::SeqCst),
            "stop returned before the follower ended"
        );
        assert!(lock(&SETTING).is_none() && lock(&FOLLOWER).is_none());
    }
}
