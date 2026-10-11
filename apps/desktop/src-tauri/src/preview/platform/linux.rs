use std::sync::mpsc;
use std::thread::{self, ThreadId};
use std::time::Duration;

use cairo::ImageSurface;
use gtk::prelude::*;
use webkit2gtk::{
    SnapshotOptions, SnapshotRegion, WebViewExt, WebsiteDataManagerExtManual, WebsiteDataTypes,
};

use super::{ClearDataKinds, PlatformWebviewOps, PreviewPlatformError, json_envelope};

const PLATFORM_CALL_TIMEOUT: Duration = Duration::from_secs(10);

pub struct LinuxWebviewOps;

/// Name of the overlay layer that holds preview webviews above the window's
/// content box.
const PREVIEW_LAYER: &str = "bibcode-preview-layer";

/// Places a preview webview at `x`/`y` (relative to the main webview) with the
/// given size. Tauri packs Linux child webviews into the window's content box,
/// where `set_position`/`set_size` cannot move them (wry moves only children of
/// a `GtkFixed`; tauri#10420), so the first placement moves the webview into a
/// pass-through `GtkFixed` overlaid on the main webview. Runs on the GTK thread.
pub fn place_child(webview: &tauri::Webview, x: f64, y: f64, width: f64, height: f64) {
    let result = webview.with_webview(move |platform| {
        let child: gtk::Widget = platform.inner().upcast();
        if let Err(error) = place_in_preview_layer(&child, x, y, width, height) {
            tracing::warn!("failed to place the preview webview: {error}");
        }
    });
    if let Err(error) = result {
        tracing::warn!("failed to reach the preview webview to place it: {error}");
    }
}

pub(super) fn place_in_preview_layer(
    child: &gtk::Widget,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
) -> Result<(), String> {
    let parent = child
        .parent()
        .ok_or_else(|| "the preview webview has no parent".to_string())?;
    let layer = if parent.widget_name() == PREVIEW_LAYER {
        parent
            .downcast::<gtk::Fixed>()
            .map_err(|_| "the preview layer is not a GtkFixed".to_string())?
    } else {
        // First placement: Tauri packed the webview into the content box.
        let content = parent
            .downcast::<gtk::Box>()
            .map_err(|_| "the preview webview's parent is not the content box".to_string())?;
        let layer = preview_layer(&content, child)?;
        content.remove(child);
        layer.put(child, 0, 0);
        layer
    };
    // The layer covers exactly the main webview, so preview bounds (laid out
    // by the main webview's page) need no offset.
    layer.move_(child, x.round() as i32, y.round() as i32);
    child.set_size_request(
        (width.round() as i32).max(1),
        (height.round() as i32).max(1),
    );
    Ok(())
}

/// The preview layer over the window's main webview. The first time, the main
/// webview's slot in the content box becomes a `GtkOverlay` holding it, with a
/// layer that passes input through except over its children.
fn preview_layer(content: &gtk::Box, child: &gtk::Widget) -> Result<gtk::Fixed, String> {
    let existing = content
        .children()
        .into_iter()
        .filter_map(|widget| widget.downcast::<gtk::Overlay>().ok())
        .find_map(|overlay| {
            overlay
                .children()
                .into_iter()
                .find(|widget| widget.widget_name() == PREVIEW_LAYER)
                .and_then(|widget| widget.downcast::<gtk::Fixed>().ok())
        });
    if let Some(layer) = existing {
        return Ok(layer);
    }
    let main = content
        .children()
        .into_iter()
        .find(|widget| widget != child && widget.is::<webkit2gtk::WebView>())
        .ok_or_else(|| "the window has no main webview".to_string())?;
    let slot = content.child_position(&main);
    let (expand, fill, padding, pack_type) = content.query_child_packing(&main);
    let overlay = gtk::Overlay::new();
    content.remove(&main);
    overlay.add(&main);
    let layer = gtk::Fixed::new();
    layer.set_widget_name(PREVIEW_LAYER);
    overlay.add_overlay(&layer);
    overlay.set_overlay_pass_through(&layer, true);
    content.add(&overlay);
    content.set_child_packing(&overlay, expand, fill, padding, pack_type);
    content.reorder_child(&overlay, slot);
    overlay.show();
    layer.show();
    Ok(layer)
}

fn unavailable(context: &str, error: impl std::fmt::Display) -> PreviewPlatformError {
    PreviewPlatformError::Unavailable(format!("{context}: {error}"))
}

fn completion_wait_guard(
    caller_thread: ThreadId,
    webview_thread: ThreadId,
) -> Result<(), PreviewPlatformError> {
    if caller_thread == webview_thread {
        Err(PreviewPlatformError::Unavailable(
            "completion-based preview platform calls cannot wait on the WebKitGTK UI thread"
                .to_string(),
        ))
    } else {
        Ok(())
    }
}

fn website_data_types(kinds: ClearDataKinds) -> WebsiteDataTypes {
    let mut types = WebsiteDataTypes::empty();
    if kinds.cookies {
        types |= WebsiteDataTypes::COOKIES;
    }
    if kinds.cache {
        types |= WebsiteDataTypes::MEMORY_CACHE | WebsiteDataTypes::DISK_CACHE;
    }
    if kinds.storage {
        types |= WebsiteDataTypes::SESSION_STORAGE
            | WebsiteDataTypes::LOCAL_STORAGE
            | WebsiteDataTypes::INDEXEDDB_DATABASES;
    }
    types
}

fn surface_to_png(surface: cairo::Surface) -> Result<Vec<u8>, PreviewPlatformError> {
    let image_surface = ImageSurface::try_from(surface).map_err(|surface| {
        PreviewPlatformError::Unavailable(format!(
            "snapshot did not return an image surface (got {:?})",
            surface.type_()
        ))
    })?;
    let mut png = Vec::new();
    image_surface
        .write_to_png(&mut png)
        .map_err(|error| unavailable("failed to encode preview snapshot as PNG", error))?;
    if png.is_empty() {
        return Err(PreviewPlatformError::Unavailable(
            "preview snapshot PNG was empty".to_string(),
        ));
    }
    Ok(png)
}

/// Run `f` with the WebKitGTK webview on the GTK UI thread and post the result back.
fn with_webkit<T: Send + 'static>(
    webview: &tauri::Webview,
    f: impl FnOnce(&webkit2gtk::WebView) -> Result<T, PreviewPlatformError> + Send + 'static,
) -> Result<T, PreviewPlatformError> {
    let (tx, rx) = mpsc::sync_channel::<Result<T, PreviewPlatformError>>(1);
    webview
        .with_webview(move |platform| {
            let webview = platform.inner();
            let _ = tx.send(f(&webview));
        })
        .map_err(|error| PreviewPlatformError::Unavailable(error.to_string()))?;
    rx.recv_timeout(PLATFORM_CALL_TIMEOUT)
        .map_err(|_| PreviewPlatformError::Timeout)?
}

impl PlatformWebviewOps for LinuxWebviewOps {
    fn eval_json(
        webview: &tauri::Webview,
        js: &str,
        timeout: Duration,
    ) -> Result<String, PreviewPlatformError> {
        let caller_thread = thread::current().id();
        let script = json_envelope(js);
        let (tx, rx) = mpsc::sync_channel::<Result<String, PreviewPlatformError>>(1);
        webview
            .with_webview(move |platform| {
                if let Err(error) = completion_wait_guard(caller_thread, thread::current().id()) {
                    let _ = tx.send(Err(error));
                    return;
                }

                let webview = platform.inner();
                webview.evaluate_javascript(
                    &script,
                    None,
                    None,
                    None::<&webkit2gtk::gio::Cancellable>,
                    move |result| {
                        let outcome = result
                            .map(|value| value.to_string())
                            .map_err(|error| PreviewPlatformError::Js(error.to_string()));
                        let _ = tx.send(outcome);
                    },
                );
            })
            .map_err(|error| PreviewPlatformError::Unavailable(error.to_string()))?;
        rx.recv_timeout(timeout)
            .map_err(|_| PreviewPlatformError::Timeout)?
    }

    fn title(webview: &tauri::Webview) -> Result<String, PreviewPlatformError> {
        with_webkit(webview, |webview| {
            Ok(webview
                .title()
                .map(|title| title.to_string())
                .unwrap_or_default())
        })
    }

    fn can_go_back(webview: &tauri::Webview) -> Result<bool, PreviewPlatformError> {
        with_webkit(webview, |webview| Ok(webview.can_go_back()))
    }

    fn can_go_forward(webview: &tauri::Webview) -> Result<bool, PreviewPlatformError> {
        with_webkit(webview, |webview| Ok(webview.can_go_forward()))
    }

    fn go_back(webview: &tauri::Webview) -> Result<(), PreviewPlatformError> {
        with_webkit(webview, |webview| {
            webview.go_back();
            Ok(())
        })
    }

    fn go_forward(webview: &tauri::Webview) -> Result<(), PreviewPlatformError> {
        with_webkit(webview, |webview| {
            webview.go_forward();
            Ok(())
        })
    }

    fn hard_reload(webview: &tauri::Webview) -> Result<(), PreviewPlatformError> {
        with_webkit(webview, |webview| {
            webview.reload_bypass_cache();
            Ok(())
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

                let webview = platform.inner();
                webview.snapshot(
                    SnapshotRegion::Visible,
                    SnapshotOptions::NONE,
                    None::<&webkit2gtk::gio::Cancellable>,
                    move |result| {
                        let outcome = result
                            .map_err(|error| unavailable("WebKitGTK snapshot failed", error))
                            .and_then(surface_to_png);
                        let _ = tx.send(outcome);
                    },
                );
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
        let data_types = website_data_types(kinds);
        let (tx, rx) = mpsc::sync_channel::<Result<(), PreviewPlatformError>>(1);
        webview
            .with_webview(move |platform| {
                if let Err(error) = completion_wait_guard(caller_thread, thread::current().id()) {
                    let _ = tx.send(Err(error));
                    return;
                }
                if data_types.is_empty() {
                    let _ = tx.send(Ok(()));
                    return;
                }

                let webview = platform.inner();
                let Some(manager) = webview.website_data_manager() else {
                    let _ = tx.send(Err(PreviewPlatformError::Unavailable(
                        "WebKitGTK website data manager is unavailable".to_string(),
                    )));
                    return;
                };
                manager.clear(
                    data_types,
                    webkit2gtk::glib::TimeSpan::from_seconds(0),
                    None::<&webkit2gtk::gio::Cancellable>,
                    move |result| {
                        let outcome = result.map_err(|error| {
                            unavailable("WebKitGTK website data clear failed", error)
                        });
                        let _ = tx.send(outcome);
                    },
                );
            })
            .map_err(|error| PreviewPlatformError::Unavailable(error.to_string()))?;
        rx.recv_timeout(PLATFORM_CALL_TIMEOUT)
            .map_err(|_| PreviewPlatformError::Timeout)?
    }
}

#[cfg(test)]
mod tests {
    /// Runs GTK on this test's thread against the session's display. Ignored by
    /// default: needs a display. Run on both backends:
    /// `cargo test -p bibcode-desktop --lib preview_child_follows -- --ignored --test-threads=1`
    /// with `GDK_BACKEND=wayland` and `GDK_BACKEND=x11`.
    #[test]
    #[ignore = "needs a Wayland or X11 display"]
    fn preview_child_follows_its_bounds_in_the_overlay_layer() {
        use gtk::prelude::*;

        gtk::init().expect("GTK display");
        let pump = || {
            for _ in 0..50 {
                while gtk::events_pending() {
                    gtk::main_iteration_do(false);
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        };
        // The layout tao and Tauri build: window -> vbox -> [menu bar, main
        // webview, child webviews packed after it].
        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_default_size(900, 700);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
        window.add(&content);
        let menu = gtk::MenuBar::new();
        menu.append(&gtk::MenuItem::with_label("File"));
        content.pack_start(&menu, false, false, 0);
        let main = webkit2gtk::WebView::new();
        content.pack_start(&main, true, true, 0);
        let child = webkit2gtk::WebView::new();
        content.pack_start(&child, true, true, 0);
        window.show_all();
        pump();

        let position = |child: &webkit2gtk::WebView| {
            child
                .translate_coordinates(&main, 0, 0)
                .expect("shared toplevel")
        };
        // Without placement the box lays the child out below the main webview,
        // which is where `desktop_preview_set_bounds` leaves it on Linux.
        assert_ne!(position(&child), (120, 80), "the bug this placement fixes");

        super::place_in_preview_layer(child.upcast_ref(), 120.0, 80.0, 300.0, 200.0)
            .expect("place");
        pump();
        assert_eq!(position(&child), (120, 80));
        let allocation = child.allocation();
        assert_eq!((allocation.width(), allocation.height()), (300, 200));

        // Later bounds move it within the layer; the main webview keeps the window.
        super::place_in_preview_layer(child.upcast_ref(), 10.0, 20.0, 50.0, 40.0).expect("move");
        pump();
        assert_eq!(position(&child), (10, 20));
        let allocation = child.allocation();
        assert_eq!((allocation.width(), allocation.height()), (50, 40));
        // The main webview still fills the content box below the menu bar.
        assert_eq!(
            main.allocation().height() + menu.allocation().height(),
            content.allocation().height()
        );
        assert_eq!(main.allocation().width(), content.allocation().width());
        window.close();
    }

    use std::sync::mpsc;
    use std::thread;

    use cairo::{Format, ImageSurface};

    use super::{completion_wait_guard, surface_to_png, website_data_types};
    use crate::preview::platform::{ClearDataKinds, PreviewPlatformError};

    #[test]
    fn completion_wait_guard_rejects_the_webview_thread() {
        let thread_id = thread::current().id();
        let error = completion_wait_guard(thread_id, thread_id).unwrap_err();
        assert!(matches!(
            error,
            PreviewPlatformError::Unavailable(message)
                if message.contains("WebKitGTK UI thread")
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
    fn website_data_kinds_map_to_the_expected_webkit_flags() {
        let types = website_data_types(ClearDataKinds {
            cookies: true,
            cache: true,
            storage: true,
        });

        assert!(types.contains(webkit2gtk::WebsiteDataTypes::COOKIES));
        assert!(types.contains(webkit2gtk::WebsiteDataTypes::MEMORY_CACHE));
        assert!(types.contains(webkit2gtk::WebsiteDataTypes::DISK_CACHE));
        assert!(types.contains(webkit2gtk::WebsiteDataTypes::SESSION_STORAGE));
        assert!(types.contains(webkit2gtk::WebsiteDataTypes::LOCAL_STORAGE));
        assert!(types.contains(webkit2gtk::WebsiteDataTypes::INDEXEDDB_DATABASES));
    }

    #[test]
    fn image_surface_encodes_as_png() {
        let image = ImageSurface::create(Format::ARgb32, 1, 1).unwrap();
        let png = surface_to_png(image.as_ref().clone()).unwrap();

        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    }
}
