use std::sync::mpsc;
use std::time::Duration;

use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSBitmapImageRepPropertyKey, NSImage,
};
use objc2_foundation::{NSDate, NSDictionary, NSError, NSSet, NSString, NSThread, NSUUID};
use objc2_web_kit::{
    WKSnapshotConfiguration, WKWebView, WKWebsiteDataStore, WKWebsiteDataTypeCookies,
    WKWebsiteDataTypeDiskCache, WKWebsiteDataTypeIndexedDBDatabases, WKWebsiteDataTypeLocalStorage,
    WKWebsiteDataTypeMemoryCache, WKWebsiteDataTypeSessionStorage,
};

use super::{ClearDataKinds, PlatformWebviewOps, PreviewPlatformError, json_envelope};

const PLATFORM_CALL_TIMEOUT: Duration = Duration::from_secs(10);

pub struct MacosWebviewOps;

fn completion_wait_guard(is_main_thread: bool) -> Result<(), PreviewPlatformError> {
    if is_main_thread {
        Err(PreviewPlatformError::Unavailable(
            "completion-based preview platform calls cannot wait on the macOS main thread"
                .to_string(),
        ))
    } else {
        Ok(())
    }
}

fn ensure_completion_wait_allowed() -> Result<(), PreviewPlatformError> {
    completion_wait_guard(NSThread::isMainThread_class())
}

/// Run `f` with the WKWebView on the main thread and post the result back.
fn with_wk<T: Send + 'static>(
    webview: &tauri::Webview,
    f: impl FnOnce(&WKWebView) -> T + Send + 'static,
) -> Result<T, PreviewPlatformError> {
    let (tx, rx) = mpsc::sync_channel::<T>(1);
    webview
        .with_webview(move |platform| {
            // SAFETY: On macOS `PlatformWebview::inner()` is the live WKWebView
            // pointer, and Tauri invokes this closure on the main/UI thread.
            let wk: &WKWebView = unsafe { &*platform.inner().cast() };
            let _ = tx.send(f(wk));
        })
        .map_err(|error| PreviewPlatformError::Unavailable(error.to_string()))?;
    rx.recv_timeout(PLATFORM_CALL_TIMEOUT)
        .map_err(|_| PreviewPlatformError::Timeout)
}

impl PlatformWebviewOps for MacosWebviewOps {
    fn eval_json(
        webview: &tauri::Webview,
        js: &str,
        timeout: Duration,
    ) -> Result<String, PreviewPlatformError> {
        ensure_completion_wait_allowed()?;
        let script = json_envelope(js);
        let (tx, rx) = mpsc::sync_channel::<Result<String, PreviewPlatformError>>(1);
        webview
            .with_webview(move |platform| {
                // SAFETY: See `with_wk`; Tauri owns the WKWebView and runs the
                // closure on the main/UI thread.
                let wk: &WKWebView = unsafe { &*platform.inner().cast() };
                let ns_script = NSString::from_str(&script);
                let handler = RcBlock::new(move |result: *mut AnyObject, error: *mut NSError| {
                    let outcome = if !error.is_null() {
                        // SAFETY: WebKit provides a valid NSError for the
                        // duration of the completion handler when non-null.
                        let error = unsafe { &*error };
                        Err(PreviewPlatformError::Js(
                            error.localizedDescription().to_string(),
                        ))
                    } else if result.is_null() {
                        Ok("{\"ok\":null}".to_string())
                    } else {
                        // SAFETY: `json_envelope` always resolves to a JS
                        // string, represented here by NSString.
                        let ns: &NSString = unsafe { &*result.cast() };
                        Ok(ns.to_string())
                    };
                    let _ = tx.send(outcome);
                });
                // SAFETY: Both Objective-C objects and the copied completion
                // block remain valid for the duration required by WebKit.
                unsafe {
                    wk.evaluateJavaScript_completionHandler(&ns_script, Some(&handler));
                }
            })
            .map_err(|error| PreviewPlatformError::Unavailable(error.to_string()))?;
        rx.recv_timeout(timeout)
            .map_err(|_| PreviewPlatformError::Timeout)?
    }

    fn title(webview: &tauri::Webview) -> Result<String, PreviewPlatformError> {
        with_wk(webview, |wk| unsafe {
            wk.title()
                .map(|title| title.to_string())
                .unwrap_or_default()
        })
    }

    fn can_go_back(webview: &tauri::Webview) -> Result<bool, PreviewPlatformError> {
        with_wk(webview, |wk| unsafe { wk.canGoBack() })
    }

    fn can_go_forward(webview: &tauri::Webview) -> Result<bool, PreviewPlatformError> {
        with_wk(webview, |wk| unsafe { wk.canGoForward() })
    }

    fn go_back(webview: &tauri::Webview) -> Result<(), PreviewPlatformError> {
        with_wk(webview, |wk| {
            let _ = unsafe { wk.goBack() };
        })
    }

    fn go_forward(webview: &tauri::Webview) -> Result<(), PreviewPlatformError> {
        with_wk(webview, |wk| {
            let _ = unsafe { wk.goForward() };
        })
    }

    fn hard_reload(webview: &tauri::Webview) -> Result<(), PreviewPlatformError> {
        with_wk(webview, |wk| {
            let _ = unsafe { wk.reloadFromOrigin() };
        })
    }

    fn screenshot_png(
        webview: &tauri::Webview,
        timeout: Duration,
    ) -> Result<Vec<u8>, PreviewPlatformError> {
        ensure_completion_wait_allowed()?;
        let (tx, rx) = mpsc::sync_channel::<Result<Vec<u8>, PreviewPlatformError>>(1);
        webview
            .with_webview(move |platform| {
                // SAFETY: See `with_wk`; Tauri owns the WKWebView and runs the
                // closure on the main/UI thread.
                let wk: &WKWebView = unsafe { &*platform.inner().cast() };
                let Some(main_thread) = MainThreadMarker::new() else {
                    let _ = tx.send(Err(PreviewPlatformError::Unavailable(
                        "snapshot was not scheduled on the main thread".to_string(),
                    )));
                    return;
                };
                // SAFETY: The marker proves this is the main thread.
                let config = unsafe { WKSnapshotConfiguration::new(main_thread) };
                let handler = RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
                    let outcome = if !error.is_null() {
                        // SAFETY: WebKit provides a valid NSError for the
                        // duration of the completion handler when non-null.
                        let error = unsafe { &*error };
                        Err(PreviewPlatformError::Js(
                            error.localizedDescription().to_string(),
                        ))
                    } else if image.is_null() {
                        Err(PreviewPlatformError::Unavailable(
                            "snapshot returned nil".to_string(),
                        ))
                    } else {
                        // SAFETY: WebKit provides a valid NSImage for the
                        // duration of the completion handler when non-null.
                        let image = unsafe { &*image };
                        image
                            .TIFFRepresentation()
                            .and_then(|tiff| NSBitmapImageRep::imageRepWithData(&tiff))
                            .and_then(|rep| {
                                let properties =
                                    NSDictionary::<NSBitmapImageRepPropertyKey, AnyObject>::new();
                                // SAFETY: The empty dictionary contains no
                                // values that could violate the API's generic
                                // property type requirement.
                                unsafe {
                                    rep.representationUsingType_properties(
                                        NSBitmapImageFileType::PNG,
                                        &properties,
                                    )
                                }
                            })
                            .map(|png| png.to_vec())
                            .ok_or_else(|| {
                                PreviewPlatformError::Unavailable("png encode failed".to_string())
                            })
                    };
                    let _ = tx.send(outcome);
                });
                // SAFETY: The configuration, web view, and copied completion
                // block meet WebKit's generated Objective-C contract.
                unsafe {
                    wk.takeSnapshotWithConfiguration_completionHandler(Some(&config), &handler);
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
        ensure_completion_wait_allowed()?;
        let (tx, rx) = mpsc::sync_channel::<()>(1);
        webview
            .with_webview(move |platform| {
                // SAFETY: See `with_wk`; all WKWebView and WebKit data-store
                // access remains inside Tauri's main/UI-thread closure.
                unsafe {
                    let wk: &WKWebView = &*platform.inner().cast();
                    let store = wk.configuration().websiteDataStore();
                    let mut types: Vec<&NSString> = Vec::new();
                    if kinds.cookies {
                        types.push(WKWebsiteDataTypeCookies);
                    }
                    if kinds.cache {
                        types.extend([WKWebsiteDataTypeDiskCache, WKWebsiteDataTypeMemoryCache]);
                    }
                    if kinds.storage {
                        types.extend([
                            WKWebsiteDataTypeLocalStorage,
                            WKWebsiteDataTypeSessionStorage,
                            WKWebsiteDataTypeIndexedDBDatabases,
                        ]);
                    }
                    let ns_types = NSSet::from_slice(&types);
                    let done = RcBlock::new(move || {
                        let _ = tx.send(());
                    });
                    store.removeDataOfTypes_modifiedSince_completionHandler(
                        &ns_types,
                        &NSDate::distantPast(),
                        &done,
                    );
                }
            })
            .map_err(|error| PreviewPlatformError::Unavailable(error.to_string()))?;
        rx.recv_timeout(PLATFORM_CALL_TIMEOUT)
            .map_err(|_| PreviewPlatformError::Timeout)
    }
}

/// Deletes a persistent website data store by its identifier (an environment's
/// preview storage). No live WKWebView may use it; callers keep a profile a
/// preview used this session.
pub fn remove_data_store(app: &tauri::AppHandle, identifier: [u8; 16]) -> Result<(), String> {
    ensure_completion_wait_allowed().map_err(super::super::host::platform_err)?;
    let (tx, rx) = mpsc::sync_channel::<Result<(), String>>(1);
    app.run_on_main_thread(move || {
        let Some(main_thread) = MainThreadMarker::new() else {
            let _ = tx.send(Err(
                "preview data store removal left the main thread".to_string()
            ));
            return;
        };
        let uuid = NSUUID::from_bytes(identifier);
        let done = RcBlock::new(move |error: *mut NSError| {
            let result = if error.is_null() {
                Ok(())
            } else {
                // SAFETY: WebKit passes a valid NSError when the pointer is not null.
                Err(unsafe { &*error }.localizedDescription().to_string())
            };
            let _ = tx.send(result);
        });
        // SAFETY: Called on the main thread with a retained identifier and block.
        unsafe {
            WKWebsiteDataStore::removeDataStoreForIdentifier_completionHandler(
                &uuid,
                &done,
                main_thread,
            );
        }
    })
    .map_err(|error| error.to_string())?;
    match rx.recv_timeout(PLATFORM_CALL_TIMEOUT) {
        Ok(result) => result,
        Err(_) => Err("timed out removing the preview data store".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{PreviewPlatformError, completion_wait_guard};

    #[test]
    fn completion_wait_guard_rejects_the_main_thread() {
        let error = completion_wait_guard(true).unwrap_err();
        assert!(matches!(
            error,
            PreviewPlatformError::Unavailable(message)
                if message.contains("macOS main thread")
        ));
    }

    #[test]
    fn completion_wait_guard_allows_worker_threads() {
        assert!(completion_wait_guard(false).is_ok());
    }
}
