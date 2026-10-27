use std::sync::Arc;

use coddy_studio_server::diagnostics::DesktopUiProcessObserver;
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
use coddy_studio_server::diagnostics::UnavailableDesktopUiProcessObserver;
#[cfg(windows)]
use coddy_studio_server::diagnostics::WebView2DesktopUiProcessObserver;
use tauri::{AppHandle, Runtime};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
use self::linux::LinuxDesktopUiProcessObserver;
#[cfg(target_os = "macos")]
use self::macos::MacosDesktopUiProcessObserver;

pub(super) fn for_app<R: Runtime>(app: &AppHandle<R>) -> Arc<dyn DesktopUiProcessObserver> {
    #[cfg(target_os = "macos")]
    return Arc::new(MacosDesktopUiProcessObserver::new(app.clone()));

    #[cfg(target_os = "linux")]
    {
        let _ = app;
        Arc::new(LinuxDesktopUiProcessObserver::new())
    }

    #[cfg(windows)]
    if let Some(executable_name) = std::env::current_exe().ok().and_then(|path| {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
    }) {
        return Arc::new(WebView2DesktopUiProcessObserver::new(executable_name));
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let _ = app;

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    Arc::new(UnavailableDesktopUiProcessObserver)
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use tauri::test::{mock_builder, mock_context, noop_assets};

    #[test]
    fn linux_ui_factory_installs_the_linux_observer() {
        let app = mock_builder()
            .build(mock_context(noop_assets()))
            .expect("mock app");
        let observer = super::for_app(app.handle());

        assert_eq!(format!("{observer:?}"), "LinuxDesktopUiProcessObserver");
    }
}
