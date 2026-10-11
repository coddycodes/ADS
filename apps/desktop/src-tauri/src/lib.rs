#[cfg(all(target_os = "macos", panic = "abort"))]
compile_error!("macOS desktop builds require panic=unwind for Objective-C exception recovery");

use tauri::Manager;

#[cfg(test)]
macro_rules! desktop_bridge_commands {
    ($with_commands:ident) => {
        $with_commands![
            desktop_bridge_get_bridge_metadata,
            desktop_bridge_get_app_branding,
            desktop_bridge_get_local_environment_bootstraps,
            desktop_bridge_get_project_data_statuses,
            desktop_bridge_restore_project_data,
            desktop_bridge_start_empty_project_data,
            desktop_bridge_retry_project_data,
            desktop_bridge_restart_app,
            desktop_bridge_open_project_data_path,
            desktop_bridge_export_project_data_diagnostics,
            desktop_bridge_get_client_settings,
            desktop_bridge_set_client_settings,
            desktop_bridge_get_connection_catalog,
            desktop_bridge_set_connection_catalog,
            desktop_bridge_compare_connection_catalog,
            desktop_bridge_compare_and_set_connection_catalog,
            desktop_bridge_clear_connection_catalog,
            desktop_bridge_discover_ssh_hosts,
            desktop_bridge_ensure_ssh_environment,
            desktop_bridge_disconnect_ssh_environment,
            desktop_bridge_ssh_forward,
            desktop_bridge_release_ssh_forward,
            desktop_bridge_fetch_environment_descriptor,
            desktop_bridge_bootstrap_ssh_bearer_session,
            desktop_bridge_fetch_ssh_session_state,
            desktop_bridge_issue_ssh_web_socket_ticket,
            desktop_bridge_resolve_ssh_password_prompt,
            desktop_bridge_get_server_exposure_state,
            desktop_bridge_apply_server_exposure,
            desktop_bridge_set_tailscale_serve_enabled,
            desktop_bridge_get_advertised_endpoints,
            desktop_bridge_get_wsl_state,
            desktop_bridge_set_wsl_backend_enabled,
            desktop_bridge_set_wsl_distro,
            desktop_bridge_set_wsl_only,
            desktop_bridge_set_theme,
            desktop_bridge_show_context_menu,
            desktop_bridge_get_update_state,
            desktop_bridge_check_for_update,
            desktop_bridge_download_update,
            desktop_bridge_install_update,
            desktop_bridge_pick_folder,
            desktop_bridge_save_diagnostic_logs,
            desktop_bridge_pick_files,
            desktop_bridge_download_to_folder,
            desktop_bridge_upload_file,
            desktop_bridge_confirm,
            desktop_bridge_open_external,
            desktop_bridge_open_in_file_manager,
        ]
    };
}

#[cfg(test)]
macro_rules! desktop_preview_commands {
    ($with_commands:ident) => {
        $with_commands![
            desktop_preview_create_tab,
            desktop_preview_close_tab,
            desktop_preview_set_bounds,
            desktop_preview_navigate,
            desktop_preview_go_back,
            desktop_preview_go_forward,
            desktop_preview_refresh,
            desktop_preview_hard_reload,
            desktop_preview_set_zoom,
            desktop_preview_open_devtools,
            desktop_preview_clear_data,
            desktop_preview_capture_screenshot,
            desktop_preview_reveal_artifact,
        ]
    };
}

#[cfg(test)]
macro_rules! bridge_command_names {
    ($($command:ident),+ $(,)?) => {
        &[$(stringify!($command)),+]
    };
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    windows_update_restart::adopt_restart_handoff();
    let shell_path_hydration = shell_environment::hydrate_process_path();
    // After the PATH hydration, which must run before any thread exists, and before Tauri.
    crash_monitor::start();
    let builder = tauri::Builder::<bridge::DesktopRuntime>::new()
        .plugin(tauri_plugin_single_instance::init(|_app, _argv, _cwd| {}))
        .manage(backend::BackendSupervisor::new())
        .manage(server_exposure::ServerExposureCoordinator::default())
        .manage(bridge::ConnectionCatalogCoordinator::new())
        .manage(context_menu::NativeContextMenuManager::new())
        .manage(ssh::SshEnvironmentManager::new())
        .manage(ssh::SshPasswordPromptManager::new())
        .manage(updates::DesktopUpdateManager::new())
        .manage(preview::PreviewHostState::new())
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        // The plugin's injected click script would swallow `_blank` links in
        // every webview (preview tabs included) and call an IPC command no
        // capability grants. New windows are routed by `on_new_window` instead.
        .plugin(
            tauri_plugin_opener::Builder::new()
                .open_js_links_on_click(false)
                .build(),
        )
        .plugin(tauri_plugin_updater::Builder::new().build());
    #[cfg(target_os = "linux")]
    let builder = builder.manage(linux_theme::LinuxThemeState::default());
    #[cfg(target_os = "macos")]
    let builder = webview_exit::watch(builder);
    #[cfg(feature = "desktop-e2e")]
    let builder = builder
        .plugin(desktop_e2e_logging_plugin())
        .plugin(tauri_plugin_wdio::init())
        .plugin(tauri_plugin_wdio_webdriver::init());
    let builder = builder.setup(move |app| {
        // Built here (not from config) so it can route new-window requests;
        // first, so everything below still runs after the main window exists.
        window::build_main_window(app.handle())?;
        #[cfg(target_os = "linux")]
        {
            linux_text_rendering::apply_webview_hinting_override();
            linux_theme::configure_theme(&app.state::<linux_theme::LinuxThemeState>());
        }
        shell_path_hydration.record();
        window::configure_application_menu(app.handle())?;
        window::restore_main_window_state(app.handle())?;

        #[cfg(any(windows, target_os = "linux"))]
        {
            use tauri_plugin_deep_link::DeepLinkExt;
            if let Err(error) = app.deep_link().register_all() {
                tracing::warn!("failed to register bibcode:// deep-link handler: {error}");
            }
        }

        let update_app = app.handle().clone();
        {
            use tauri_plugin_updater::UpdaterExt as _;
            let backend = app.state::<crate::backend::BackendSupervisor>();
            backend.install_remote_update_integration(
                crate::remote_update_delegate::DesktopRemoteUpdateDelegate::new(update_app.clone()),
                crate::remote_update_delegate::derive_remote_update_support(
                    update_app.updater().is_ok(),
                ),
            );
            let recovery_app = update_app.clone();
            backend.install_recovery_listener(std::sync::Arc::new(move || {
                recovery_app
                    .state::<updates::DesktopUpdateManager>()
                    .emit_current_state(&recovery_app);
            }));
        }
        tauri::async_runtime::spawn(updates::run_background_update_checks(update_app));

        let app_handle = app.handle().clone();
        let backend = app.state::<backend::BackendSupervisor>().inner().clone();
        let exposure_coordinator = app
            .state::<server_exposure::ServerExposureCoordinator>()
            .inner()
            .clone();
        #[cfg(unix)]
        backend::install_termination_signal_handler(app_handle.clone(), backend.clone());
        tauri::async_runtime::spawn(async move {
            match exposure_coordinator
                .run_exclusive(backend.start_default(app_handle))
                .await
            {
                Ok(_config) => {}
                Err(error) => {
                    tracing::error!("failed to start Tauri desktop backend: {error}");
                    backend.record_error(error);
                }
            }
        });
        Ok(())
    });
    #[cfg(not(test))]
    let builder = builder.invoke_handler(tauri::generate_handler![
        bridge::desktop_bridge_get_bridge_metadata,
        bridge::desktop_bridge_get_app_branding,
        bridge::desktop_bridge_get_local_environment_bootstraps,
        bridge::desktop_bridge_get_project_data_statuses,
        bridge::desktop_bridge_restore_project_data,
        bridge::desktop_bridge_start_empty_project_data,
        bridge::desktop_bridge_retry_project_data,
        bridge::desktop_bridge_restart_app,
        bridge::desktop_bridge_open_project_data_path,
        bridge::desktop_bridge_export_project_data_diagnostics,
        bridge::desktop_bridge_get_client_settings,
        bridge::desktop_bridge_set_client_settings,
        bridge::desktop_bridge_get_connection_catalog,
        bridge::desktop_bridge_set_connection_catalog,
        bridge::desktop_bridge_compare_connection_catalog,
        bridge::desktop_bridge_compare_and_set_connection_catalog,
        bridge::desktop_bridge_clear_connection_catalog,
        bridge::desktop_bridge_discover_ssh_hosts,
        bridge::desktop_bridge_ensure_ssh_environment,
        bridge::desktop_bridge_disconnect_ssh_environment,
        bridge::desktop_bridge_ssh_forward,
        bridge::desktop_bridge_release_ssh_forward,
        bridge::desktop_bridge_fetch_environment_descriptor,
        bridge::desktop_bridge_bootstrap_ssh_bearer_session,
        bridge::desktop_bridge_fetch_ssh_session_state,
        bridge::desktop_bridge_issue_ssh_web_socket_ticket,
        bridge::desktop_bridge_resolve_ssh_password_prompt,
        bridge::desktop_bridge_get_server_exposure_state,
        bridge::desktop_bridge_apply_server_exposure,
        bridge::desktop_bridge_set_tailscale_serve_enabled,
        bridge::desktop_bridge_get_advertised_endpoints,
        bridge::desktop_bridge_get_wsl_state,
        bridge::desktop_bridge_set_wsl_backend_enabled,
        bridge::desktop_bridge_set_wsl_distro,
        bridge::desktop_bridge_set_wsl_only,
        bridge::desktop_bridge_set_theme,
        bridge::desktop_bridge_show_context_menu,
        bridge::desktop_bridge_get_update_state,
        bridge::desktop_bridge_check_for_update,
        bridge::desktop_bridge_download_update,
        bridge::desktop_bridge_install_update,
        bridge::desktop_bridge_pick_folder,
        bridge::desktop_bridge_save_diagnostic_logs,
        bridge::desktop_bridge_pick_files,
        bridge::desktop_bridge_download_to_folder,
        bridge::desktop_bridge_upload_file,
        bridge::desktop_bridge_confirm,
        bridge::desktop_bridge_open_external,
        bridge::desktop_bridge_open_in_file_manager,
        preview::commands::desktop_preview_create_tab,
        preview::commands::desktop_preview_close_tab,
        preview::commands::desktop_preview_set_bounds,
        preview::commands::desktop_preview_navigate,
        preview::commands::desktop_preview_go_back,
        preview::commands::desktop_preview_go_forward,
        preview::commands::desktop_preview_refresh,
        preview::commands::desktop_preview_hard_reload,
        preview::commands::desktop_preview_set_zoom,
        preview::commands::desktop_preview_open_devtools,
        preview::commands::desktop_preview_clear_data,
        preview::commands::desktop_preview_capture_screenshot,
        preview::commands::desktop_preview_reveal_artifact,
        #[cfg(feature = "desktop-e2e")]
        desktop_e2e_prepare_for_exit,
    ]);
    builder
        .build(desktop_context())
        .expect("error while building BiBCode Tauri application")
        .run(|app_handle, event| {
            if matches!(event, tauri::RunEvent::ExitRequested { .. })
                && let Err(error) =
                    tauri::async_runtime::block_on(prepare_desktop_runtime_for_exit(app_handle))
            {
                tracing::warn!("failed to stop Tauri desktop runtime during exit: {error}");
            }
            if matches!(event, tauri::RunEvent::Exit) {
                // Mark after backend shutdown, at the last callback before Tauri's relaunch spawn.
                crate::relaunch::prepare_descriptors_for_relaunch();
            }
        });
}

async fn prepare_desktop_runtime_for_exit<R: tauri::Runtime>(
    app_handle: &tauri::AppHandle<R>,
) -> Result<(), String> {
    if let Err(error) = window::persist_main_window_state(app_handle) {
        tracing::warn!("failed to persist Tauri main window state during exit: {error}");
    }
    let backend = app_handle
        .state::<backend::BackendSupervisor>()
        .inner()
        .clone();
    let backend_result = backend
        .stop_for_exit(backend::BackendShutdownConfig::default())
        .await;
    let ssh = app_handle.state::<ssh::SshEnvironmentManager>();
    ssh.shutdown().await;
    backend_result
}

#[cfg(all(feature = "desktop-e2e", not(test)))]
#[tauri::command]
async fn desktop_e2e_prepare_for_exit(
    app_handle: tauri::AppHandle<bridge::DesktopRuntime>,
) -> Result<(), String> {
    prepare_desktop_runtime_for_exit(&app_handle).await
}

fn desktop_context<R: tauri::Runtime>() -> tauri::Context<R> {
    tauri::generate_context!()
}

#[cfg(feature = "desktop-e2e")]
fn desktop_e2e_logging_plugin<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    let mut page_loads = desktop_e2e_page_load::PageLoadTracker::new();
    tauri::plugin::Builder::new("desktop-e2e-logging")
        .setup(|app, _api| {
            let server_log = config::state_dir(app)
                .map_err(std::io::Error::other)?
                .join("logs")
                .join("server.log");
            bibcode_server::logging::initialize(&server_log)?;
            Ok(())
        })
        .on_page_load(move |webview, payload| {
            if let Some(message) = page_loads.message(webview.label(), payload.event()) {
                tracing::info!(target: "bibcode::desktop_e2e", "{message}");
            }
        })
        .build()
}

mod backend;
mod bridge;
mod config;
mod context_menu;
mod crash_monitor;
mod data_safety;
#[cfg(any(feature = "desktop-e2e", test))]
mod desktop_e2e_page_load;
mod firewall;
#[cfg(target_os = "linux")]
mod linux_text_rendering;
#[cfg(target_os = "linux")]
mod linux_theme;
mod network_interfaces;
mod preview;
mod relaunch;
mod remote_update_delegate;
mod security;
mod server_exposure;
mod shell_environment;
pub mod ssh;
mod tailscale;
#[cfg(test)]
mod test_support;
mod updates;
mod webview_exit;
mod window;
mod windows_update_restart;

pub use bridge::{
    desktop_bridge_bootstrap_ssh_bearer_session, desktop_bridge_fetch_environment_descriptor,
    desktop_bridge_fetch_ssh_session_state, desktop_bridge_get_bridge_metadata,
    desktop_bridge_issue_ssh_web_socket_ticket,
};

#[cfg(test)]
const DESKTOP_BRIDGE_COMMAND_NAMES: &[&str] = desktop_bridge_commands!(bridge_command_names);

#[cfg(test)]
const DESKTOP_PREVIEW_COMMAND_NAMES: &[&str] = desktop_preview_commands!(bridge_command_names);

#[cfg(test)]
mod tests {
    use serde::Deserialize;
    use serde_json::Value;

    use super::{DESKTOP_BRIDGE_COMMAND_NAMES, DESKTOP_PREVIEW_COMMAND_NAMES};

    #[test]
    fn main_window_is_created_by_setup_so_it_can_handle_new_windows() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(config["app"]["windows"][0]["label"], "main");
        assert_eq!(config["app"]["windows"][0]["create"], false);
    }

    #[derive(Debug, Deserialize)]
    struct PermissionsFile {
        permission: Vec<PermissionEntry>,
    }

    #[derive(Debug, Deserialize)]
    struct PermissionEntry {
        identifier: String,
        commands: PermissionCommands,
    }

    #[derive(Debug, Deserialize)]
    struct PermissionCommands {
        allow: Vec<String>,
    }

    fn assert_main_webview_only(capability: &Value) {
        assert_eq!(
            capability.get("webviews"),
            Some(&serde_json::json!(["main"])),
            "capability must target only the trusted main webview",
        );
        assert!(
            capability.get("windows").is_none(),
            "window-wide capability scope would also authorize preview child webviews",
        );
    }

    #[test]
    fn production_capability_is_scoped_to_the_main_webview() {
        let capability: Value = serde_json::from_str(include_str!("../capabilities/default.json"))
            .expect("default capability JSON should parse");

        assert_main_webview_only(&capability);
    }

    #[test]
    fn desktop_e2e_capability_is_scoped_to_the_main_webview() {
        let config: Value = serde_json::from_str(include_str!("../tauri.e2e.conf.json"))
            .expect("desktop E2E config JSON should parse");
        let capability = &config["app"]["security"]["capabilities"][0];

        assert_main_webview_only(capability);
        assert_eq!(
            capability.get("permissions"),
            Some(&serde_json::json!([
                "allow-desktop-bridge",
                "core:default",
                "wdio:default",
                "allow-desktop-e2e-lifecycle"
            ]))
        );
    }

    #[test]
    fn desktop_e2e_lifecycle_permission_only_allows_runtime_shutdown() {
        let permissions: PermissionsFile =
            toml::from_str(include_str!("../permissions/desktop-e2e-lifecycle.toml"))
                .expect("desktop E2E lifecycle permission TOML should parse");

        let lifecycle_permission = permissions
            .permission
            .iter()
            .find(|permission| permission.identifier == "allow-desktop-e2e-lifecycle")
            .expect("desktop E2E lifecycle permission should exist");

        assert_eq!(
            lifecycle_permission.commands.allow,
            ["desktop_e2e_prepare_for_exit"]
        );
    }

    #[test]
    fn desktop_bridge_permission_allows_registered_commands() {
        let permissions: PermissionsFile =
            toml::from_str(include_str!("../permissions/desktop-bridge.toml"))
                .expect("desktop bridge permission TOML should parse");

        let bridge_permission = permissions
            .permission
            .iter()
            .find(|permission| permission.identifier == "allow-desktop-bridge")
            .expect("desktop bridge permission should exist");

        let mut allowed = bridge_permission.commands.allow.clone();
        allowed.sort();

        let mut registered = DESKTOP_BRIDGE_COMMAND_NAMES
            .iter()
            .map(|command| (*command).to_string())
            .collect::<Vec<_>>();
        registered.sort();

        assert_eq!(allowed, registered);
    }

    #[test]
    fn desktop_preview_permission_allows_registered_commands() {
        let permissions: PermissionsFile =
            toml::from_str(include_str!("../permissions/preview.toml"))
                .expect("desktop preview permission TOML should parse");

        let preview_permission = permissions
            .permission
            .iter()
            .find(|permission| permission.identifier == "allow-desktop-preview")
            .expect("desktop preview permission should exist");

        let mut allowed = preview_permission.commands.allow.clone();
        allowed.sort();

        let mut registered = DESKTOP_PREVIEW_COMMAND_NAMES
            .iter()
            .map(|command| (*command).to_string())
            .collect::<Vec<_>>();
        registered.sort();

        assert_eq!(allowed, registered);
    }

    #[test]
    fn generated_handler_registers_the_audited_command_lists() {
        let source = include_str!("lib.rs");
        let handler = source
            .split_once(".invoke_handler(tauri::generate_handler![")
            .and_then(|(_, remainder)| remainder.split_once("]);"))
            .map(|(handler, _)| handler)
            .expect("explicit generate_handler invocation should exist");

        let registered_with_prefix = |prefix: &str| {
            let mut commands = handler
                .lines()
                .filter_map(|line| {
                    line.trim()
                        .strip_prefix(prefix)
                        .and_then(|line| line.strip_suffix(','))
                        .map(str::to_string)
                })
                .collect::<Vec<_>>();
            commands.sort();
            commands
        };
        let audited = |commands: &[&str]| {
            let mut commands = commands
                .iter()
                .map(|command| (*command).to_string())
                .collect::<Vec<_>>();
            commands.sort();
            commands
        };

        assert_eq!(
            registered_with_prefix("bridge::"),
            audited(DESKTOP_BRIDGE_COMMAND_NAMES)
        );
        assert_eq!(
            registered_with_prefix("preview::commands::"),
            audited(DESKTOP_PREVIEW_COMMAND_NAMES)
        );
    }

    #[test]
    fn desktop_bridge_permission_toml_round_trips_without_schema_drift() {
        let source = include_str!("../permissions/desktop-bridge.toml");
        let parsed: toml::Value = toml::from_str(source).expect("permission TOML should parse");
        let encoded = toml::to_string(&parsed).expect("permission TOML should encode");
        let reparsed: toml::Value =
            toml::from_str(&encoded).expect("encoded permission TOML should parse");

        assert_eq!(reparsed, parsed);
    }
}
