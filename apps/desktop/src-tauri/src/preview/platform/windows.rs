use std::slice;
use std::sync::mpsc;
use std::thread::{self, ThreadId};
use std::time::Duration;

use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_BROWSING_DATA_KINDS, COREWEBVIEW2_BROWSING_DATA_KINDS_ALL_DOM_STORAGE,
    COREWEBVIEW2_BROWSING_DATA_KINDS_COOKIES, COREWEBVIEW2_BROWSING_DATA_KINDS_DISK_CACHE,
    COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG, ICoreWebView2, ICoreWebView2_13,
    ICoreWebView2Profile2,
};
use webview2_com::{
    CapturePreviewCompletedHandler, ClearBrowsingDataCompletedHandler,
    ExecuteScriptCompletedHandler, take_pwstr,
};
use windows::Win32::Foundation::HGLOBAL;
use windows::Win32::System::Com::StructuredStorage::{CreateStreamOnHGlobal, GetHGlobalFromStream};
use windows::Win32::System::Com::{IStream, STATFLAG_NONAME, STATSTG};
use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};
use windows::core::{BOOL, Interface, PCWSTR, PWSTR};

use super::{ClearDataKinds, PlatformWebviewOps, PreviewPlatformError, json_envelope};

const PLATFORM_CALL_TIMEOUT: Duration = Duration::from_secs(10);

pub struct WindowsWebviewOps;

fn unavailable(context: &str, error: impl std::fmt::Display) -> PreviewPlatformError {
    PreviewPlatformError::Unavailable(format!("{context}: {error}"))
}

fn completion_wait_guard(
    caller_thread: ThreadId,
    webview_thread: ThreadId,
) -> Result<(), PreviewPlatformError> {
    if caller_thread == webview_thread {
        Err(PreviewPlatformError::Unavailable(
            "completion-based preview platform calls cannot wait on the WebView2 UI thread"
                .to_string(),
        ))
    } else {
        Ok(())
    }
}

/// Run `f` with the CoreWebView2 on the UI thread and post the result back.
fn with_wv2<T: Send + 'static>(
    webview: &tauri::Webview,
    f: impl FnOnce(&ICoreWebView2) -> Result<T, PreviewPlatformError> + Send + 'static,
) -> Result<T, PreviewPlatformError> {
    let (tx, rx) = mpsc::sync_channel::<Result<T, PreviewPlatformError>>(1);
    webview
        .with_webview(move |platform| {
            // SAFETY: Tauri provides the live WebView2 controller and invokes
            // this closure on its owning UI thread.
            let outcome = unsafe { platform.controller().CoreWebView2() }
                .map_err(|error| unavailable("failed to access CoreWebView2", error))
                .and_then(|core| f(&core));
            let _ = tx.send(outcome);
        })
        .map_err(|error| PreviewPlatformError::Unavailable(error.to_string()))?;
    rx.recv_timeout(PLATFORM_CALL_TIMEOUT)
        .map_err(|_| PreviewPlatformError::Timeout)?
}

struct GlobalUnlockGuard(HGLOBAL);

impl Drop for GlobalUnlockGuard {
    fn drop(&mut self) {
        // SAFETY: The guard is only constructed after GlobalLock succeeds and
        // does not outlive the IStream that owns this HGLOBAL.
        let _ = unsafe { GlobalUnlock(self.0) };
    }
}

fn read_png_stream(stream: &IStream) -> Result<Vec<u8>, PreviewPlatformError> {
    let mut stat = STATSTG::default();
    // SAFETY: `stat` is a valid out parameter. STATFLAG_NONAME ensures COM
    // does not allocate a name that the caller would need to free.
    unsafe { stream.Stat(&mut stat, STATFLAG_NONAME) }
        .map_err(|error| unavailable("failed to inspect screenshot stream", error))?;
    // SAFETY: `stream` was created by CreateStreamOnHGlobal and remains alive
    // for this entire extraction.
    let hglobal = unsafe { GetHGlobalFromStream(stream) }
        .map_err(|error| unavailable("failed to get screenshot memory", error))?;
    // SAFETY: `hglobal` belongs to the live stream above.
    let allocation_size = unsafe { GlobalSize(hglobal) };
    let logical_size = checked_screenshot_length(stat.cbSize, allocation_size)?;

    // SAFETY: `hglobal` is valid and owned by the live stream.
    let data = unsafe { GlobalLock(hglobal) };
    if data.is_null() {
        let error = windows::core::Error::from_win32();
        return Err(PreviewPlatformError::Unavailable(format!(
            "failed to lock screenshot memory: {error}"
        )));
    }
    let _unlock = GlobalUnlockGuard(hglobal);

    // SAFETY: GlobalLock returned a non-null pointer to an allocation at least
    // `logical_size` bytes long. The guard keeps it locked during the copy.
    Ok(unsafe { slice::from_raw_parts(data.cast::<u8>(), logical_size) }.to_vec())
}

fn checked_screenshot_length(
    logical_size: u64,
    allocation_size: usize,
) -> Result<usize, PreviewPlatformError> {
    if logical_size == 0 {
        return Err(PreviewPlatformError::Unavailable(
            "screenshot stream was empty".to_string(),
        ));
    }
    let logical_size = usize::try_from(logical_size).map_err(|_| {
        PreviewPlatformError::Unavailable("screenshot stream was too large".to_string())
    })?;
    if logical_size > isize::MAX as usize {
        return Err(PreviewPlatformError::Unavailable(
            "screenshot stream was too large".to_string(),
        ));
    }
    if logical_size > allocation_size {
        return Err(PreviewPlatformError::Unavailable(format!(
            "screenshot stream length {logical_size} exceeds allocation {allocation_size}"
        )));
    }
    Ok(logical_size)
}

fn browsing_data_kinds(kinds: ClearDataKinds) -> COREWEBVIEW2_BROWSING_DATA_KINDS {
    let mut mapped = COREWEBVIEW2_BROWSING_DATA_KINDS(0);
    if kinds.cookies {
        mapped |= COREWEBVIEW2_BROWSING_DATA_KINDS_COOKIES;
    }
    if kinds.cache {
        mapped |= COREWEBVIEW2_BROWSING_DATA_KINDS_DISK_CACHE;
    }
    if kinds.storage {
        mapped |= COREWEBVIEW2_BROWSING_DATA_KINDS_ALL_DOM_STORAGE;
    }
    mapped
}

impl PlatformWebviewOps for WindowsWebviewOps {
    fn eval_json(
        webview: &tauri::Webview,
        js: &str,
        timeout: Duration,
    ) -> Result<String, PreviewPlatformError> {
        let caller_thread = thread::current().id();
        let script_utf16: Vec<u16> = json_envelope(js)
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let (tx, rx) = mpsc::sync_channel::<Result<String, PreviewPlatformError>>(1);
        webview
            .with_webview(move |platform| {
                if let Err(error) = completion_wait_guard(caller_thread, thread::current().id()) {
                    let _ = tx.send(Err(error));
                    return;
                }
                let schedule = (|| {
                    // SAFETY: Tauri provides the live controller on its owning
                    // UI thread for the duration of this closure.
                    let core = unsafe { platform.controller().CoreWebView2() }
                        .map_err(|error| unavailable("failed to access CoreWebView2", error))?;
                    let completion_tx = tx.clone();
                    let handler =
                        ExecuteScriptCompletedHandler::create(Box::new(move |result, raw| {
                            let outcome = result
                                .map_err(|error| {
                                    PreviewPlatformError::Js(format!(
                                        "WebView2 ExecuteScript failed: {error}"
                                    ))
                                })
                                .and_then(|()| {
                                    serde_json::from_str::<String>(&raw).map_err(|error| {
                                        PreviewPlatformError::Js(format!(
                                            "invalid WebView2 ExecuteScript result: {error}"
                                        ))
                                    })
                                });
                            let _ = completion_tx.send(outcome);
                            Ok(())
                        }));
                    // SAFETY: The script buffer is NUL-terminated and remains
                    // live through this call; WebView2 retains the COM handler.
                    unsafe { core.ExecuteScript(PCWSTR(script_utf16.as_ptr()), &handler) }
                        .map_err(|error| unavailable("failed to schedule ExecuteScript", error))
                })();

                if let Err(error) = schedule {
                    let _ = tx.send(Err(error));
                }
            })
            .map_err(|error| PreviewPlatformError::Unavailable(error.to_string()))?;
        rx.recv_timeout(timeout)
            .map_err(|_| PreviewPlatformError::Timeout)?
    }

    fn title(webview: &tauri::Webview) -> Result<String, PreviewPlatformError> {
        with_wv2(webview, |core| {
            let mut title = PWSTR::null();
            // SAFETY: WebView2 writes a CoTaskMem-allocated PWSTR to the valid
            // out pointer. `take_pwstr` copies and frees that allocation.
            unsafe { core.DocumentTitle(&mut title) }
                .map_err(|error| unavailable("failed to read document title", error))?;
            Ok(take_pwstr(title))
        })
    }

    fn can_go_back(webview: &tauri::Webview) -> Result<bool, PreviewPlatformError> {
        with_wv2(webview, |core| {
            let mut can_go_back = BOOL::default();
            // SAFETY: The BOOL out pointer is valid for the call.
            unsafe { core.CanGoBack(&mut can_go_back) }
                .map_err(|error| unavailable("failed to read back-navigation state", error))?;
            Ok(can_go_back.as_bool())
        })
    }

    fn can_go_forward(webview: &tauri::Webview) -> Result<bool, PreviewPlatformError> {
        with_wv2(webview, |core| {
            let mut can_go_forward = BOOL::default();
            // SAFETY: The BOOL out pointer is valid for the call.
            unsafe { core.CanGoForward(&mut can_go_forward) }
                .map_err(|error| unavailable("failed to read forward-navigation state", error))?;
            Ok(can_go_forward.as_bool())
        })
    }

    fn go_back(webview: &tauri::Webview) -> Result<(), PreviewPlatformError> {
        with_wv2(webview, |core| {
            // SAFETY: The call stays on the WebView2-owning UI thread.
            unsafe { core.GoBack() }.map_err(|error| unavailable("failed to navigate back", error))
        })
    }

    fn go_forward(webview: &tauri::Webview) -> Result<(), PreviewPlatformError> {
        with_wv2(webview, |core| {
            // SAFETY: The call stays on the WebView2-owning UI thread.
            unsafe { core.GoForward() }
                .map_err(|error| unavailable("failed to navigate forward", error))
        })
    }

    fn hard_reload(webview: &tauri::Webview) -> Result<(), PreviewPlatformError> {
        with_wv2(webview, |core| {
            // SAFETY: The call stays on the WebView2-owning UI thread.
            unsafe { core.Reload() }.map_err(|error| unavailable("failed to reload", error))
        })
    }

    fn screenshot_png(
        webview: &tauri::Webview,
        timeout: Duration,
    ) -> Result<Vec<u8>, PreviewPlatformError> {
        let caller_thread = thread::current().id();
        let (tx, rx) = mpsc::sync_channel::<Result<Vec<u8>, PreviewPlatformError>>(1);
        webview
            .with_webview(move |platform| {
                if let Err(error) = completion_wait_guard(caller_thread, thread::current().id()) {
                    let _ = tx.send(Err(error));
                    return;
                }
                let schedule = (|| {
                    // SAFETY: Tauri provides the live controller on its owning
                    // UI thread for the duration of this closure.
                    let core = unsafe { platform.controller().CoreWebView2() }
                        .map_err(|error| unavailable("failed to access CoreWebView2", error))?;
                    // SAFETY: A null HGLOBAL asks COM to allocate movable
                    // memory; true transfers cleanup to the returned IStream.
                    let stream = unsafe { CreateStreamOnHGlobal(HGLOBAL::default(), true) }
                        .map_err(|error| {
                            unavailable("failed to create screenshot stream", error)
                        })?;
                    let completion_stream = stream.clone();
                    let completion_tx = tx.clone();
                    let handler = CapturePreviewCompletedHandler::create(Box::new(move |result| {
                        let outcome = result
                            .map_err(|error| unavailable("WebView2 CapturePreview failed", error))
                            .and_then(|()| read_png_stream(&completion_stream));
                        let _ = completion_tx.send(outcome);
                        Ok(())
                    }));
                    // SAFETY: The stream and typed handler are valid COM
                    // objects; the handler retains its stream clone.
                    unsafe {
                        core.CapturePreview(
                            COREWEBVIEW2_CAPTURE_PREVIEW_IMAGE_FORMAT_PNG,
                            &stream,
                            &handler,
                        )
                    }
                    .map_err(|error| unavailable("failed to schedule CapturePreview", error))
                })();

                if let Err(error) = schedule {
                    let _ = tx.send(Err(error));
                }
            })
            .map_err(|error| PreviewPlatformError::Unavailable(error.to_string()))?;
        rx.recv_timeout(timeout)
            .map_err(|_| PreviewPlatformError::Timeout)?
    }

    fn clear_data(
        webview: &tauri::Webview,
        kinds: ClearDataKinds,
    ) -> Result<(), PreviewPlatformError> {
        let caller_thread = thread::current().id();
        let data_kinds = browsing_data_kinds(kinds);
        let (tx, rx) = mpsc::sync_channel::<Result<(), PreviewPlatformError>>(1);
        webview
            .with_webview(move |platform| {
                if let Err(error) = completion_wait_guard(caller_thread, thread::current().id()) {
                    let _ = tx.send(Err(error));
                    return;
                }
                let schedule = (|| {
                    if data_kinds.0 == 0 {
                        return tx.send(Ok(())).map_err(|error| {
                            unavailable("failed to report clear-data result", error)
                        });
                    }

                    // SAFETY: Tauri provides the live controller on its owning
                    // UI thread for the duration of this closure.
                    let core = unsafe { platform.controller().CoreWebView2() }
                        .map_err(|error| unavailable("failed to access CoreWebView2", error))?;
                    let core_13 = core.cast::<ICoreWebView2_13>().map_err(|error| {
                        unavailable("WebView2 profile API is unavailable", error)
                    })?;
                    // SAFETY: The versioned interface cast succeeded.
                    let profile = unsafe { core_13.Profile() }
                        .map_err(|error| unavailable("failed to access WebView2 profile", error))?;
                    let profile_2 = profile.cast::<ICoreWebView2Profile2>().map_err(|error| {
                        unavailable("WebView2 clear-data API is unavailable", error)
                    })?;
                    let completion_tx = tx.clone();
                    let handler =
                        ClearBrowsingDataCompletedHandler::create(Box::new(move |result| {
                            let outcome = result.map_err(|error| {
                                unavailable("WebView2 ClearBrowsingData failed", error)
                            });
                            let _ = completion_tx.send(outcome);
                            Ok(())
                        }));
                    // SAFETY: The versioned profile interface and typed
                    // completion handler satisfy the generated COM contract.
                    unsafe { profile_2.ClearBrowsingData(data_kinds, &handler) }
                        .map_err(|error| unavailable("failed to schedule ClearBrowsingData", error))
                })();

                if let Err(error) = schedule {
                    let _ = tx.send(Err(error));
                }
            })
            .map_err(|error| PreviewPlatformError::Unavailable(error.to_string()))?;
        rx.recv_timeout(PLATFORM_CALL_TIMEOUT)
            .map_err(|_| PreviewPlatformError::Timeout)?
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;

    use super::{checked_screenshot_length, completion_wait_guard};
    use crate::preview::platform::PreviewPlatformError;

    #[test]
    fn completion_wait_guard_rejects_the_webview_thread() {
        let thread_id = thread::current().id();
        let error = completion_wait_guard(thread_id, thread_id).unwrap_err();
        assert!(matches!(
            error,
            PreviewPlatformError::Unavailable(message)
                if message.contains("WebView2 UI thread")
        ));
    }

    #[test]
    fn completion_wait_guard_allows_a_worker_thread() {
        let caller_thread = thread::current().id();
        let (tx, rx) = mpsc::sync_channel(1);
        thread::spawn(move || {
            tx.send(thread::current().id()).unwrap();
        })
        .join()
        .unwrap();

        assert!(completion_wait_guard(caller_thread, rx.recv().unwrap()).is_ok());
    }

    #[test]
    fn screenshot_length_uses_stream_length_not_allocation_capacity() {
        assert_eq!(checked_screenshot_length(8, 32).unwrap(), 8);
    }

    #[test]
    fn screenshot_length_rejects_empty_streams() {
        let error = checked_screenshot_length(0, 32).unwrap_err();
        assert!(matches!(
            error,
            PreviewPlatformError::Unavailable(message)
                if message.contains("empty")
        ));
    }

    #[test]
    fn screenshot_length_rejects_values_larger_than_the_allocation() {
        let error = checked_screenshot_length(33, 32).unwrap_err();
        assert!(matches!(
            error,
            PreviewPlatformError::Unavailable(message)
                if message.contains("allocation")
        ));
    }

    #[test]
    fn screenshot_length_rejects_values_larger_than_a_slice() {
        let logical_size = (isize::MAX as u64) + 1;
        let error = checked_screenshot_length(logical_size, usize::MAX).unwrap_err();
        assert!(matches!(
            error,
            PreviewPlatformError::Unavailable(message)
                if message.contains("too large")
        ));
    }
}
