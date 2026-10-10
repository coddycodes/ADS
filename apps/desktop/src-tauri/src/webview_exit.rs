//! Reports the app window's web content process dying, as the profiler's `crash.webview`
//! finding. WebKitGTK and WebView2 are hooked on the main window; on macOS, Tauri exposes
//! WebKit's termination callback only on the app builder.

use bibcode_server::diagnostics::profiler::{exits, is_enabled};

/// WebKitGTK's `web-process-terminated`.
#[cfg(target_os = "linux")]
pub(crate) fn watch(webview: &webkit2gtk::WebView) {
    use webkit2gtk::{WebProcessTerminationReason, WebViewExt};

    webview.connect_web_process_terminated(|_, reason| {
        // Ended through the API on request, not by itself.
        if reason != WebProcessTerminationReason::TerminatedByApi && is_enabled() {
            exits::report_webview_terminated(&format!("{reason:?}"));
        }
    });
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
            &format!("{}: {}", kind_name(kind), reason_name(reason)),
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

/// WebKit's `webViewWebContentProcessDidTerminate:`. Only the main window is reported:
/// preview tabs are pages the user opened.
#[cfg(target_os = "macos")]
pub(crate) fn watch<R: tauri::Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    use tauri::Manager;

    builder.on_web_content_process_terminate(|webview| {
        if webview.label() == crate::window::MAIN_WINDOW_LABEL && is_enabled() {
            exits::report_webview_terminated("web-content-terminated");
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
