use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, Position, Runtime, Size,
    WebviewWindow,
    menu::{Menu, MenuBuilder, MenuItemBuilder, SubmenuBuilder},
};

use crate::config::{read_json_file, state_dir, write_json_file};
use crate::context_menu::NativeContextMenuManager;

pub const MENU_ACTION_EVENT: &str = "desktop:menu-action";
pub const MENU_ACTION_OPEN_SETTINGS: &str = "open-settings";
pub const MENU_ACTION_CHECK_FOR_UPDATES: &str = "check-for-updates";
pub const MENU_ACTION_CLOSE_SHORTCUT: &str = "close-shortcut";

pub(crate) const MAIN_WINDOW_LABEL: &str = "main";
const MENU_ID_OPEN_SETTINGS: &str = "bibcode:open-settings";
const MENU_ID_CHECK_FOR_UPDATES: &str = "bibcode:check-for-updates";
const MENU_ID_CLOSE_SHORTCUT: &str = "bibcode:close-shortcut";
const MENU_ID_CLOSE_WINDOW: &str = "bibcode:close-window";
const MIN_RESTORED_WINDOW_HEIGHT: u32 = 480;
const MIN_RESTORED_WINDOW_WIDTH: u32 = 640;
const MAX_RESTORED_WINDOW_HEIGHT: u32 = 10_000;
const MAX_RESTORED_WINDOW_WIDTH: u32 = 10_000;
const WINDOW_STATE_FILE_NAME: &str = "window-state.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MainWindowState {
    width: u32,
    height: u32,
    x: Option<i32>,
    y: Option<i32>,
    maximized: bool,
    fullscreen: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApplicationMenuAction {
    Emit(&'static str),
    CloseWindow,
}

fn menu_action_for_id(id: &str) -> Option<ApplicationMenuAction> {
    match id {
        MENU_ID_OPEN_SETTINGS => Some(ApplicationMenuAction::Emit(MENU_ACTION_OPEN_SETTINGS)),
        MENU_ID_CHECK_FOR_UPDATES => {
            Some(ApplicationMenuAction::Emit(MENU_ACTION_CHECK_FOR_UPDATES))
        }
        MENU_ID_CLOSE_SHORTCUT => Some(ApplicationMenuAction::Emit(MENU_ACTION_CLOSE_SHORTCUT)),
        MENU_ID_CLOSE_WINDOW => Some(ApplicationMenuAction::CloseWindow),
        _ => None,
    }
}

pub fn configure_application_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let menu = build_application_menu(app)?;
    app.set_menu(menu)?;
    app.on_menu_event(|app, event| {
        if app
            .state::<NativeContextMenuManager>()
            .complete_if_context_menu_event(event.id().0.as_str())
        {
            return;
        }
        match menu_action_for_id(event.id().0.as_str()) {
            Some(ApplicationMenuAction::Emit(action)) => {
                if action == MENU_ACTION_OPEN_SETTINGS
                    && let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL)
                {
                    let _ = window.set_focus();
                }
                let _ = app.emit(MENU_ACTION_EVENT, action);
            }
            Some(ApplicationMenuAction::CloseWindow) => {
                if let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) {
                    let _ = window.close();
                }
            }
            None => {}
        }
    });
    Ok(())
}

fn build_application_menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let settings = MenuItemBuilder::with_id(MENU_ID_OPEN_SETTINGS, "Settings...")
        .accelerator("CmdOrCtrl+,")
        .build(app)?;
    let check_for_updates =
        MenuItemBuilder::with_id(MENU_ID_CHECK_FOR_UPDATES, "Check for Updates...").build(app)?;
    let close_shortcut = MenuItemBuilder::with_id(MENU_ID_CLOSE_SHORTCUT, "Close Active Surface")
        .accelerator("CmdOrCtrl+W")
        .build(app)?;
    let close_window = MenuItemBuilder::with_id(MENU_ID_CLOSE_WINDOW, "Close Window").build(app)?;

    let file = SubmenuBuilder::new(app, "File")
        .item(&settings)
        .separator()
        .item(&close_shortcut)
        .separator()
        .quit()
        .build()?;
    let edit = SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;
    let view = SubmenuBuilder::new(app, "View").fullscreen().build()?;
    let window = SubmenuBuilder::new(app, "Window")
        .minimize()
        .maximize()
        .separator()
        .item(&close_window)
        .build()?;
    let help = SubmenuBuilder::new(app, "Help")
        .item(&check_for_updates)
        .build()?;

    MenuBuilder::new(app)
        .items(&[&file, &edit, &view, &window, &help])
        .build()
}

fn window_state_path<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    Ok(state_dir(app)?.join(WINDOW_STATE_FILE_NAME))
}

fn normalize_main_window_state(state: MainWindowState) -> Option<MainWindowState> {
    if !(MIN_RESTORED_WINDOW_WIDTH..=MAX_RESTORED_WINDOW_WIDTH).contains(&state.width) {
        return None;
    }
    if !(MIN_RESTORED_WINDOW_HEIGHT..=MAX_RESTORED_WINDOW_HEIGHT).contains(&state.height) {
        return None;
    }
    match (state.x, state.y) {
        (Some(_), Some(_)) | (None, None) => Some(state),
        _ => None,
    }
}

fn main_window_state_to_value(state: &MainWindowState) -> Result<Value, String> {
    serde_json::to_value(state)
        .map_err(|error| format!("Could not encode Tauri main window state: {error}"))
}

fn decode_main_window_state(value: Value) -> Result<MainWindowState, String> {
    serde_json::from_value::<MainWindowState>(value)
        .map_err(|error| format!("Could not decode Tauri main window state: {error}"))
}

fn apply_main_window_state<R: Runtime>(window: &WebviewWindow<R>, state: &MainWindowState) {
    if let Err(error) = window.set_size(Size::Physical(PhysicalSize {
        width: state.width,
        height: state.height,
    })) {
        tracing::warn!("failed to restore Tauri main window size: {error}");
    }
    if let (Some(x), Some(y)) = (state.x, state.y)
        && let Err(error) = window.set_position(Position::Physical(PhysicalPosition { x, y }))
    {
        tracing::warn!("failed to restore Tauri main window position: {error}");
    }
    if state.fullscreen {
        if let Err(error) = window.set_fullscreen(true) {
            tracing::warn!("failed to restore Tauri main window fullscreen state: {error}");
        }
    } else if state.maximized
        && let Err(error) = window.maximize()
    {
        tracing::warn!("failed to restore Tauri main window maximized state: {error}");
    }
}

fn capture_main_window_state<R: Runtime>(
    window: &WebviewWindow<R>,
) -> Result<Option<MainWindowState>, String> {
    let size = window
        .outer_size()
        .or_else(|_| window.inner_size())
        .map_err(|error| format!("Could not read the Tauri main window size: {error}"))?;
    let position = window.outer_position().ok();
    let maximized = window.is_maximized().unwrap_or(false);
    let fullscreen = window.is_fullscreen().unwrap_or(false);
    Ok(normalize_main_window_state(MainWindowState {
        width: size.width,
        height: size.height,
        x: position.as_ref().map(|position| position.x),
        y: position.as_ref().map(|position| position.y),
        maximized,
        fullscreen,
    }))
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MainNewWindowAction {
    OpenExternal,
    Ignore,
}

pub(crate) fn main_window_new_window_action(url: &tauri::Url) -> MainNewWindowAction {
    match url.scheme() {
        "http" | "https" | "mailto" => MainNewWindowAction::OpenExternal,
        _ => MainNewWindowAction::Ignore,
    }
}

/// Builds the main window from `tauri.conf.json` (`"create": false`) so it can
/// route `window.open` / `target=_blank` requests to the system browser.
pub(crate) fn build_main_window<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    use tauri_plugin_opener::OpenerExt;
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|window| window.label == MAIN_WINDOW_LABEL)
        .cloned()
        .expect("tauri.conf.json defines the main window");
    let handle = app.clone();
    let builder = tauri::WebviewWindowBuilder::from_config(app, &config)?.on_new_window(
        move |url, _features| {
            if main_window_new_window_action(&url) == MainNewWindowAction::OpenExternal
                && let Err(error) = handle.opener().open_url(url.as_str(), None::<&str>)
            {
                tracing::warn!("failed to open link externally: {error}");
            }
            tauri::webview::NewWindowResponse::Deny
        },
    );
    // A loaded page runs in a web content process the profiler remembers for crash reports.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let builder = builder.on_page_load(|window, payload| {
        if payload.event() == tauri::webview::PageLoadEvent::Finished {
            crate::webview_exit::page_loaded(window.app_handle(), window.label());
        }
        // A reload starts a renderer that knows none of the old one's preview tunnels (#69).
        #[cfg(target_os = "linux")]
        if payload.event() == tauri::webview::PageLoadEvent::Started
            && let Some(tunnels) = window
                .app_handle()
                .try_state::<crate::preview::socks::PreviewTunnels>()
        {
            let tunnels = tunnels.inner().clone();
            tauri::async_runtime::spawn(async move { tunnels.close_all().await });
        }
    });
    let window = builder.build()?;
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    watch_web_content_process(&window);
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    let _ = window;
    Ok(())
}

/// Reports the main window's web content process dying to the profiler. macOS registers its
/// hook on the app builder instead.
#[cfg(any(target_os = "linux", target_os = "windows"))]
fn watch_web_content_process<R: Runtime>(window: &WebviewWindow<R>) {
    let installed = window.with_webview(|webview| {
        #[cfg(target_os = "linux")]
        crate::webview_exit::watch(&webview.inner());
        #[cfg(target_os = "windows")]
        // SAFETY: Tauri runs this closure on the UI thread that owns the controller.
        if let Err(error) = unsafe { webview.controller().CoreWebView2() }
            .and_then(|core| crate::webview_exit::watch(&core))
        {
            tracing::warn!(%error, "webview process failures are not watched");
        }
    });
    if let Err(error) = installed {
        tracing::warn!(%error, "webview process exits are not watched");
    }
}

pub fn restore_main_window_state<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let path = window_state_path(app)?;
    let Some(value) = read_json_file(&path)? else {
        return Ok(());
    };
    let state = match decode_main_window_state(value).and_then(|state| {
        normalize_main_window_state(state)
            .ok_or_else(|| "Ignored invalid Tauri main window state bounds".to_string())
    }) {
        Ok(state) => state,
        Err(error) => {
            tracing::warn!("{error}");
            return Ok(());
        }
    };
    let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) else {
        return Ok(());
    };
    apply_main_window_state(&window, &state);
    Ok(())
}

pub fn persist_main_window_state<R: Runtime>(app: &AppHandle<R>) -> Result<(), String> {
    let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) else {
        return Ok(());
    };
    let Some(state) = capture_main_window_state(&window)? else {
        return Ok(());
    };
    let path = window_state_path(app)?;
    write_json_file(&path, &main_window_state_to_value(&state)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn main_window_new_windows_open_externally_for_web_and_mail_links() {
        use super::{MainNewWindowAction, main_window_new_window_action};
        for url in [
            "https://github.com/x/y/pull/1",
            "http://localhost:3000/",
            "mailto:a@b.c",
        ] {
            let url = tauri::Url::parse(url).unwrap();
            assert_eq!(
                main_window_new_window_action(&url),
                MainNewWindowAction::OpenExternal
            );
        }
        let file = tauri::Url::parse("file:///etc/passwd").unwrap();
        assert_eq!(
            main_window_new_window_action(&file),
            MainNewWindowAction::Ignore
        );
    }

    #[test]
    fn maps_known_application_menu_ids_to_actions() {
        assert_eq!(
            menu_action_for_id(MENU_ID_OPEN_SETTINGS),
            Some(ApplicationMenuAction::Emit(MENU_ACTION_OPEN_SETTINGS))
        );
        assert_eq!(
            menu_action_for_id(MENU_ID_CHECK_FOR_UPDATES),
            Some(ApplicationMenuAction::Emit(MENU_ACTION_CHECK_FOR_UPDATES))
        );
        assert_eq!(
            menu_action_for_id(MENU_ID_CLOSE_SHORTCUT),
            Some(ApplicationMenuAction::Emit(MENU_ACTION_CLOSE_SHORTCUT))
        );
        assert_eq!(
            menu_action_for_id(MENU_ID_CLOSE_WINDOW),
            Some(ApplicationMenuAction::CloseWindow)
        );
        assert_eq!(menu_action_for_id("copy"), None);
    }

    #[test]
    fn normalizes_main_window_state_bounds() {
        let valid = MainWindowState {
            width: 1280,
            height: 900,
            x: Some(100),
            y: Some(120),
            maximized: false,
            fullscreen: false,
        };

        assert_eq!(normalize_main_window_state(valid.clone()), Some(valid));
        assert_eq!(
            normalize_main_window_state(MainWindowState {
                width: MIN_RESTORED_WINDOW_WIDTH - 1,
                height: 900,
                x: Some(100),
                y: Some(120),
                maximized: false,
                fullscreen: false,
            }),
            None
        );
        assert_eq!(
            normalize_main_window_state(MainWindowState {
                width: 1280,
                height: MAX_RESTORED_WINDOW_HEIGHT + 1,
                x: Some(100),
                y: Some(120),
                maximized: false,
                fullscreen: false,
            }),
            None
        );
        assert_eq!(
            normalize_main_window_state(MainWindowState {
                width: 1280,
                height: 900,
                x: None,
                y: Some(120),
                maximized: false,
                fullscreen: false,
            }),
            None
        );
        assert_eq!(
            normalize_main_window_state(MainWindowState {
                width: 1280,
                height: 900,
                x: Some(100),
                y: None,
                maximized: false,
                fullscreen: false,
            }),
            None
        );
    }

    #[test]
    fn encodes_and_decodes_main_window_state_documents() {
        let state = MainWindowState {
            width: 1280,
            height: 900,
            x: None,
            y: None,
            maximized: true,
            fullscreen: false,
        };

        assert_eq!(
            decode_main_window_state(main_window_state_to_value(&state).expect("state encodes"))
                .expect("state decodes"),
            state
        );
        assert_eq!(
            main_window_state_to_value(&state).expect("state encodes"),
            json!({
                "width": 1280,
                "height": 900,
                "x": null,
                "y": null,
                "maximized": true,
                "fullscreen": false,
            })
        );
        assert!(decode_main_window_state(json!({ "width": "wide" })).is_err());
    }

    #[test]
    fn mock_window_exercises_menu_capture_and_restore_helpers() {
        use crate::config::IsolatedTestDataRoot;
        use tauri::test::{mock_builder, mock_context, noop_assets};

        let temp = tempfile::tempdir().expect("isolated desktop data root");
        let mut context = mock_context(noop_assets());
        context.config_mut().identifier =
            format!("com.bibcode.window-tests-{}", std::process::id());
        let app = mock_builder()
            .manage(IsolatedTestDataRoot::new(temp.path().join("data-root")))
            .build(context)
            .expect("mock Tauri app");
        let handle = app.handle();
        let window = tauri::WebviewWindowBuilder::new(&app, MAIN_WINDOW_LABEL, Default::default())
            .build()
            .expect("mock webview");
        let state_path = window_state_path(handle).expect("window state path should resolve");

        persist_main_window_state(handle).expect("empty mock state should be ignored");
        restore_main_window_state(handle).expect("missing state should be ignored");
        write_json_file(
            &state_path,
            &json!({
                "width": 10,
                "height": 10,
                "x": null,
                "y": null,
                "maximized": false,
                "fullscreen": false,
            }),
        )
        .expect("invalid state fixture should write");
        restore_main_window_state(handle).expect("invalid state should be ignored");

        assert!(
            window_state_path(handle)
                .expect("window state path should resolve")
                .ends_with(WINDOW_STATE_FILE_NAME)
        );

        apply_main_window_state(
            &window,
            &MainWindowState {
                width: 900,
                height: 700,
                x: Some(25),
                y: Some(35),
                maximized: false,
                fullscreen: false,
            },
        );
        assert_eq!(
            capture_main_window_state(&window).expect("window state should read"),
            None
        );

        apply_main_window_state(
            &window,
            &MainWindowState {
                width: 1000,
                height: 800,
                x: None,
                y: None,
                maximized: true,
                fullscreen: false,
            },
        );

        apply_main_window_state(
            &window,
            &MainWindowState {
                width: 1000,
                height: 800,
                x: None,
                y: None,
                maximized: false,
                fullscreen: true,
            },
        );
        let _ = std::fs::remove_file(state_path);
    }
}
