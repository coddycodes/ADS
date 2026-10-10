//! Reports the app window's web content process dying, as the profiler's `crash.webview`
//! finding. WebKitGTK and WebView2 are hooked on the main window; on macOS, Tauri exposes
//! WebKit's termination callback only on the app builder.
//!
//! Neither WebKit says which process ended once it has, so on Linux and macOS the main
//! window's web content process is remembered after each page it finishes loading, and its
//! OS crash record is looked up by that pid.

use bibcode_server::diagnostics::profiler::{exits, is_enabled};

/// Our WebKitWebProcess children, with their executables, as of the main window's last
/// finished page load. WebKitGTK has reaped the one that ended by the time it says so.
#[cfg(target_os = "linux")]
static WEB_PROCESSES: std::sync::Mutex<Vec<(u32, String)>> = std::sync::Mutex::new(Vec::new());

/// WebKitGTK's `web-process-terminated`.
#[cfg(target_os = "linux")]
pub(crate) fn watch(webview: &webkit2gtk::WebView) {
    use webkit2gtk::{WebProcessTerminationReason, WebViewExt};

    webview.connect_web_process_terminated(|_, reason| {
        // Taken whatever the setting, so a later termination matches no stale pid.
        let ended = ended_web_process();
        // Ended through the API on request, not by itself.
        if reason != WebProcessTerminationReason::TerminatedByApi && is_enabled() {
            exits::report_webview_terminated(
                &format!("{reason:?}"),
                ended
                    .as_ref()
                    .map(|(pid, exe)| exits::WebProcess { pid: *pid, exe }),
            );
        }
    });
}

/// The main window finished loading a page, so its web process runs: remembers our web
/// processes, off the UI thread, while the profiler is on.
#[cfg(target_os = "linux")]
pub(crate) fn page_loaded<R: tauri::Runtime>(_app: &tauri::AppHandle<R>, _label: &str) {
    remember_web_processes(is_enabled());
}

/// From the in-process server's start on, scans for our web processes as its profiler turns
/// on, since the page loaded before then. Elsewhere than Linux nothing is scanned ahead.
pub(crate) fn follow_server(handle: &bibcode_server::ServerHandle) {
    #[cfg(target_os = "linux")]
    if let Some(setting) = handle.profiler_enabled_signal() {
        tauri::async_runtime::spawn(follow_setting(setting));
    }
    #[cfg(not(target_os = "linux"))]
    let _ = handle;
}

/// Applies each committed value of the setting until its sender is gone.
#[cfg(target_os = "linux")]
async fn follow_setting(mut setting: tokio::sync::watch::Receiver<bool>) {
    loop {
        remember_web_processes(*setting.borrow_and_update());
        if setting.changed().await.is_err() {
            break;
        }
    }
}

/// Scans for our web processes while the profiler is on. While it is off nothing runs, and
/// the list is cleared, so a later termination matches no pid from before.
#[cfg(target_os = "linux")]
fn remember_web_processes(enabled: bool) {
    let generation = next_scan(enabled);
    if !enabled {
        return;
    }
    let scan = std::thread::Builder::new()
        .name("bibcode-web-processes".into())
        .spawn(move || store_scan(generation, children_named("WebKitWebProcess")));
    if let Err(error) = scan {
        tracing::debug!(%error, "web process scan did not start");
    }
}

/// Counts scans and stops, so a scan that a later one superseded drops its result. Changed
/// only under the `WEB_PROCESSES` lock.
#[cfg(target_os = "linux")]
static SCAN_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Starts a scan, or with `enabled` false stops scanning and forgets what was found. Returns
/// the scan's generation.
#[cfg(target_os = "linux")]
fn next_scan(enabled: bool) -> u64 {
    let mut remembered = WEB_PROCESSES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !enabled {
        remembered.clear();
    }
    SCAN_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
}

/// Remembers a scan's result, unless a later scan or a stop superseded it.
#[cfg(target_os = "linux")]
fn store_scan(generation: u64, children: Vec<(u32, String)>) {
    let mut remembered = WEB_PROCESSES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if SCAN_GENERATION.load(std::sync::atomic::Ordering::Relaxed) == generation {
        *remembered = children;
    }
}

/// Takes the remembered web processes that are no longer running, and returns the one that
/// ended. `None` unless exactly one did: a crash record is attached only to the process it
/// belongs to.
#[cfg(target_os = "linux")]
fn ended_web_process() -> Option<(u32, String)> {
    let mut ended = Vec::new();
    WEB_PROCESSES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|process| {
            let running = is_running_child(process.0);
            if !running {
                ended.push(process.clone());
            }
            running
        });
    let mut ended = ended.into_iter();
    let first = ended.next()?;
    ended.next().is_none().then_some(first)
}

/// This process's running children whose executable is named `name`, with its path.
#[cfg(target_os = "linux")]
fn children_named(name: &str) -> Vec<(u32, String)> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| is_running_child(*pid))
        .filter_map(|pid| {
            let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
            (exe.file_name()? == name).then(|| (pid, exe.to_string_lossy().into_owned()))
        })
        .collect()
}

/// Whether `pid` is a child of this process that has not ended: not gone, and not a zombie.
#[cfg(target_os = "linux")]
fn is_running_child(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| state_and_parent(&stat))
        .is_some_and(|(state, parent)| state != 'Z' && parent == std::process::id())
}

/// The state and the parent pid in a `/proc/<pid>/stat` line, whose command name may hold
/// spaces and parentheses.
#[cfg(target_os = "linux")]
fn state_and_parent(stat: &str) -> Option<(char, u32)> {
    let mut fields = stat.get(stat.rfind(')')? + 1..)?.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let parent = fields.next()?.parse().ok()?;
    Some((state, parent))
}

/// WebView2's `ProcessFailed`, with the failure's kind, reason and exit code.
#[cfg(target_os = "windows")]
pub(crate) fn watch(
    webview: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2,
) -> windows::core::Result<()> {
    use webview2_com::{Microsoft::Web::WebView2::Win32::*, ProcessFailedEventHandler};
    use windows::core::Interface;

    let handler = ProcessFailedEventHandler::create(Box::new(|webview, args| {
        let Some(args) = args else {
            return Ok(());
        };
        if !is_enabled() {
            return Ok(());
        }
        let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
        let mut reason = COREWEBVIEW2_PROCESS_FAILED_REASON_UNEXPECTED;
        let mut exit_code = 0;
        // SAFETY: COM calls on the event's live arguments, on the UI thread that raised it.
        unsafe {
            args.ProcessFailedKind(&mut kind)?;
            if let Ok(details) = args.cast::<ICoreWebView2ProcessFailedEventArgs2>() {
                details.Reason(&mut reason)?;
                details.ExitCode(&mut exit_code)?;
            }
        }
        // Someone ended it on purpose, for example from Task Manager.
        if reason == COREWEBVIEW2_PROCESS_FAILED_REASON_TERMINATED {
            return Ok(());
        }
        exits::report_webview_failed(
            &kind_name(kind),
            &reason_name(reason),
            exit_code,
            webview.as_ref().and_then(failure_report_folder),
        );
        Ok(())
    }));
    let mut token = 0;
    // SAFETY: `with_webview` runs this on the UI thread that owns `webview`.
    unsafe { webview.add_ProcessFailed(&handler, &mut token) }
}

/// Where WebView2 writes the minidumps of its failed processes.
#[cfg(target_os = "windows")]
fn failure_report_folder(
    webview: &webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2,
) -> Option<std::path::PathBuf> {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2_2, ICoreWebView2Environment11,
    };
    use windows::core::{Interface, PWSTR};

    // SAFETY: COM calls on a live webview, on its UI thread.
    let environment = unsafe { webview.cast::<ICoreWebView2_2>().ok()?.Environment().ok()? };
    let environment = environment.cast::<ICoreWebView2Environment11>().ok()?;
    let mut folder = PWSTR::null();
    // SAFETY: `folder` receives a CoTaskMem string, which `take_pwstr` frees.
    unsafe { environment.FailureReportFolderPath(&mut folder) }.ok()?;
    let folder = webview2_com::take_pwstr(folder);
    (!folder.is_empty()).then(|| folder.into())
}

#[cfg(target_os = "windows")]
fn kind_name(
    kind: webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_PROCESS_FAILED_KIND,
) -> String {
    use webview2_com::Microsoft::Web::WebView2::Win32::*;

    match kind {
        COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED => "browser-process-exited",
        COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED => "render-process-exited",
        COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE => {
            "render-process-unresponsive"
        }
        COREWEBVIEW2_PROCESS_FAILED_KIND_FRAME_RENDER_PROCESS_EXITED => {
            "frame-render-process-exited"
        }
        COREWEBVIEW2_PROCESS_FAILED_KIND_GPU_PROCESS_EXITED => "gpu-process-exited",
        COREWEBVIEW2_PROCESS_FAILED_KIND_UTILITY_PROCESS_EXITED => "utility-process-exited",
        other => return format!("process-kind-{}", other.0),
    }
    .to_owned()
}

#[cfg(target_os = "windows")]
fn reason_name(
    reason: webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_PROCESS_FAILED_REASON,
) -> String {
    use webview2_com::Microsoft::Web::WebView2::Win32::*;

    match reason {
        COREWEBVIEW2_PROCESS_FAILED_REASON_UNEXPECTED => "unexpected",
        COREWEBVIEW2_PROCESS_FAILED_REASON_UNRESPONSIVE => "unresponsive",
        COREWEBVIEW2_PROCESS_FAILED_REASON_CRASHED => "crashed",
        COREWEBVIEW2_PROCESS_FAILED_REASON_LAUNCH_FAILED => "launch-failed",
        COREWEBVIEW2_PROCESS_FAILED_REASON_OUT_OF_MEMORY => "out-of-memory",
        COREWEBVIEW2_PROCESS_FAILED_REASON_PROFILE_DELETED => "profile-deleted",
        other => return format!("reason-{}", other.0),
    }
    .to_owned()
}

/// The main window's web content pid as of its last finished page load. WebKit no longer
/// tells it once the process has ended.
#[cfg(target_os = "macos")]
static WEB_CONTENT_PID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The main window finished loading a page: remembers its web content pid. From another
/// thread `with_webview` is queued to the event loop rather than run inside WebKit's
/// navigation delegate.
#[cfg(target_os = "macos")]
pub(crate) fn page_loaded<R: tauri::Runtime>(app: &tauri::AppHandle<R>, label: &str) {
    use tauri::Manager;

    let (app, label) = (app.clone(), label.to_owned());
    tauri::async_runtime::spawn(async move {
        let Some(webview) = app.get_webview(&label) else {
            return;
        };
        let read = webview.with_webview(|platform| {
            use objc2::{msg_send, runtime::NSObjectProtocol, sel};

            // SAFETY: on macOS Tauri passes its live WKWebView and runs this on the main
            // thread.
            let webview: &objc2_web_kit::WKWebView = unsafe { &*platform.inner().cast() };
            if webview.respondsToSelector(sel!(_webProcessIdentifier)) {
                // SAFETY: the check above proves this WKWebView implements the private getter.
                let pid: libc::pid_t = unsafe { msg_send![webview, _webProcessIdentifier] };
                if let Ok(pid) = u32::try_from(pid)
                    && pid > 0
                {
                    WEB_CONTENT_PID.store(pid, std::sync::atomic::Ordering::Relaxed);
                }
            }
        });
        if let Err(error) = read {
            tracing::debug!(%error, "web content pid was not read");
        }
    });
}

/// WebKit's `webViewWebContentProcessDidTerminate:`. Only the main window is reported:
/// preview tabs are pages the user opened.
#[cfg(target_os = "macos")]
pub(crate) fn watch<R: tauri::Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    use tauri::Manager;

    builder.on_web_content_process_terminate(|webview| {
        if webview.label() == crate::window::MAIN_WINDOW_LABEL {
            // Taken, so a later termination before the next page load matches no stale pid.
            let pid = WEB_CONTENT_PID.swap(0, std::sync::atomic::Ordering::Relaxed);
            if is_enabled() {
                exits::report_webview_terminated(
                    "web-content-terminated",
                    (pid > 0).then_some(exits::WebProcess {
                        pid,
                        exe: "com.apple.WebKit.WebContent",
                    }),
                );
            }
        }
        // This hook replaces Tauri's own, which reloads the webview, so every webview still
        // reloads. From another thread the reload is queued to the event loop instead of running
        // inside this delegate, where Tauri's own handler only try-borrows the window map.
        let (app, label) = (webview.app_handle().clone(), webview.label().to_owned());
        tauri::async_runtime::spawn(async move {
            if let Some(webview) = app.get_webview(&label)
                && let Err(error) = webview.reload()
            {
                tracing::warn!(%error, "webview did not reload after its content process ended");
            }
        });
    })
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn stat_lines_give_the_state_and_the_parent() {
        assert_eq!(
            state_and_parent("4242 (WebKitWebProces) S 17 4242 4242 0 -1 4194560"),
            Some(('S', 17))
        );
        assert_eq!(
            state_and_parent("7 (odd) name (x)) Z 1 7 7 0"),
            Some(('Z', 1)),
            "the command name may hold spaces and parentheses"
        );
        assert_eq!(state_and_parent("garbage"), None);
    }

    #[test]
    fn running_children_are_found_by_executable_until_they_end() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
        assert!(is_running_child(pid));
        assert!(
            children_named("sleep")
                .iter()
                .any(|(found, exe)| *found == pid && exe.ends_with("/sleep"))
        );
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(!is_running_child(pid));
        assert!(!is_running_child(1), "init is nobody's child here");
    }

    /// The tests that share `WEB_PROCESSES` run one at a time.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn remembered() -> Vec<(u32, String)> {
        WEB_PROCESSES.lock().unwrap().clone()
    }

    /// Spec Decision 1: with the profiler off, nothing runs but the crash hook and the
    /// sentinel, so a page load scans nothing, and forgets what an earlier scan found.
    /// A child the scan recognises: a copy of `sleep` named as WebKit names its process, with
    /// the directory that holds it.
    fn stand_in_web_process() -> (std::process::Child, tempfile::TempDir) {
        let sleep = std::env::split_paths(&std::env::var_os("PATH").unwrap())
            .map(|dir| dir.join("sleep"))
            .find(|path| path.is_file())
            .expect("sleep on PATH");
        let dir = tempfile::tempdir().unwrap();
        let web_process = dir.path().join("WebKitWebProcess");
        std::fs::copy(sleep, &web_process).unwrap();
        // A fork on another test thread can still hold the copy open for writing for a moment,
        // which fails the exec with ETXTBSY.
        let mut attempts = 0;
        loop {
            match std::process::Command::new(&web_process).arg("30").spawn() {
                Ok(child) => return (child, dir),
                Err(_) if attempts < 50 => {
                    attempts += 1;
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(error) => panic!("spawn the stand-in web process: {error}"),
            }
        }
    }

    fn wait_until_remembered(pid: Option<u32>) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let found = remembered();
            let done = match pid {
                Some(pid) => found.iter().any(|(remembered, _)| *remembered == pid),
                None => found.is_empty(),
            };
            if done {
                return;
            }
            assert!(std::time::Instant::now() < deadline, "{found:?}");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    fn page_loads_scan_for_web_processes_only_while_the_profiler_is_on() {
        let _serial = serial();
        let (mut child, _dir) = stand_in_web_process();

        *WEB_PROCESSES.lock().unwrap() = vec![(1, "stale".to_owned())];
        remember_web_processes(false);
        assert!(remembered().is_empty(), "the list is cleared while off");
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(remembered().is_empty(), "and nothing scanned");

        remember_web_processes(true);
        wait_until_remembered(Some(child.id()));
        child.kill().unwrap();
        child.wait().unwrap();
        WEB_PROCESSES.lock().unwrap().clear();
    }

    /// A scan still running when the profiler turns off, or when a newer scan starts, must not
    /// write its result over what came after it.
    #[test]
    fn a_superseded_scan_is_dropped() {
        let _serial = serial();
        let process = |pid| vec![(pid, "WebKitWebProcess".to_owned())];
        let stopped = next_scan(true);
        next_scan(false);
        store_scan(stopped, process(7));
        assert!(remembered().is_empty(), "the stop came after it");

        let older = next_scan(true);
        let newer = next_scan(true);
        store_scan(newer, process(8));
        store_scan(older, process(7));
        assert_eq!(remembered(), process(8), "the newer scan came after it");
        WEB_PROCESSES.lock().unwrap().clear();
    }

    /// Turning the profiler on loads no page, so the setting itself starts a scan; turning it
    /// off forgets what was found.
    #[test]
    fn turning_the_profiler_on_scans_without_a_page_load() {
        let _serial = serial();
        let (mut child, _dir) = stand_in_web_process();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (setting, follower) = tokio::sync::watch::channel(false);
            let following = tokio::spawn(follow_setting(follower));
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            assert!(remembered().is_empty());
            setting.send(true).unwrap();
            let pid = child.id();
            tokio::task::spawn_blocking(move || wait_until_remembered(Some(pid)))
                .await
                .unwrap();
            setting.send(false).unwrap();
            tokio::task::spawn_blocking(|| wait_until_remembered(None))
                .await
                .unwrap();
            drop(setting);
            following.await.unwrap();
        });
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn an_ended_web_process_is_named_once() {
        let _serial = serial();
        let exe = "/usr/lib/webkit2gtk-4.1/WebKitWebProcess".to_owned();
        let mut ended = std::process::Command::new("true")
            .spawn()
            .expect("spawn true");
        ended.wait().unwrap();
        let mut running = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        *WEB_PROCESSES.lock().unwrap() = vec![(ended.id(), exe.clone()), (running.id(), exe)];
        assert_eq!(ended_web_process().map(|(pid, _)| pid), Some(ended.id()));
        assert_eq!(ended_web_process(), None, "taken by the first termination");
        running.kill().unwrap();
        running.wait().unwrap();
        assert_eq!(ended_web_process().map(|(pid, _)| pid), Some(running.id()));
    }
}
