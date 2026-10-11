use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Url};
use uuid::Uuid;

use super::PendingBounds;
use super::host;
use super::platform::{ClearDataKinds, Platform, PlatformWebviewOps};

const SCREENSHOT_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoundsInput {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScreenshotArtifact {
    pub id: String,
    pub tab_id: String,
    pub path: String,
    pub mime_type: &'static str,
    pub size_bytes: u64,
    pub created_at: String,
}

async fn run_on_worker<T, Operation>(operation: Operation) -> Result<T, String>
where
    T: Send + 'static,
    Operation: FnOnce() -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(operation)
        .await
        .map_err(|error| format!("preview worker task failed: {error}"))?
}

fn parse_preview_url(url: &str) -> Result<Url, String> {
    let parsed = Url::parse(url).map_err(|error| format!("invalid url: {error}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("only http(s) urls can be previewed".to_string());
    }
    Ok(parsed)
}

fn new_screenshot_artifact_id() -> String {
    format!("shot-{}", Uuid::new_v4().simple())
}

fn validate_artifact_path(directory: &Path, requested: &Path) -> Result<PathBuf, String> {
    let directory = directory
        .canonicalize()
        .map_err(|error| format!("failed to resolve preview artifact directory: {error}"))?;
    let requested = requested
        .canonicalize()
        .map_err(|error| format!("failed to resolve preview artifact: {error}"))?;
    if !requested.starts_with(&directory) || !requested.is_file() {
        return Err("preview artifact must be a file inside the artifact directory".to_string());
    }
    Ok(requested)
}

#[tauri::command]
pub async fn desktop_preview_create_tab(
    app: AppHandle,
    tab_id: String,
    environment_id: Option<String>,
) -> Result<(), String> {
    run_on_worker(move || host::create_tab(&app, &tab_id, environment_id.as_deref())).await
}

#[tauri::command]
pub fn desktop_preview_close_tab(app: AppHandle, tab_id: String) -> Result<(), String> {
    host::close_tab(&app, &tab_id)
}

#[tauri::command]
pub fn desktop_preview_set_bounds(
    app: AppHandle,
    tab_id: String,
    bounds: BoundsInput,
    visible: bool,
) -> Result<(), String> {
    host::set_bounds(
        &app,
        &tab_id,
        PendingBounds {
            x: bounds.x,
            y: bounds.y,
            width: bounds.width,
            height: bounds.height,
        },
        visible,
    )
}

#[tauri::command]
pub fn desktop_preview_navigate(app: AppHandle, tab_id: String, url: String) -> Result<(), String> {
    let parsed = parse_preview_url(&url)?;
    host::with_tab_webview(&app, &tab_id, |webview| {
        webview.navigate(parsed).map_err(|error| error.to_string())
    })
}

#[tauri::command]
pub fn desktop_preview_go_back(app: AppHandle, tab_id: String) -> Result<(), String> {
    host::with_tab_webview(&app, &tab_id, |webview| {
        Platform::go_back(webview).map_err(host::platform_err)
    })
}

#[tauri::command]
pub fn desktop_preview_go_forward(app: AppHandle, tab_id: String) -> Result<(), String> {
    host::with_tab_webview(&app, &tab_id, |webview| {
        Platform::go_forward(webview).map_err(host::platform_err)
    })
}

#[tauri::command]
pub fn desktop_preview_refresh(app: AppHandle, tab_id: String) -> Result<(), String> {
    host::with_tab_webview(&app, &tab_id, |webview| {
        webview.reload().map_err(|error| error.to_string())
    })
}

#[tauri::command]
pub fn desktop_preview_hard_reload(app: AppHandle, tab_id: String) -> Result<(), String> {
    host::with_tab_webview(&app, &tab_id, |webview| {
        Platform::hard_reload(webview).map_err(host::platform_err)
    })
}

#[tauri::command]
pub fn desktop_preview_set_zoom(app: AppHandle, tab_id: String, factor: f64) -> Result<(), String> {
    host::set_zoom(&app, &tab_id, factor)
}

#[tauri::command]
pub fn desktop_preview_open_devtools(app: AppHandle, tab_id: String) -> Result<(), String> {
    host::with_tab_webview(&app, &tab_id, |webview| {
        webview.open_devtools();
        Ok(())
    })
}

/// Deletes a removed environment's preview storage unless one of its previews
/// was opened this session.
#[tauri::command]
pub async fn desktop_preview_forget_environment(
    app: AppHandle,
    environment_id: String,
) -> Result<(), String> {
    run_on_worker(move || host::forget_environment(&app, &environment_id)).await
}

#[tauri::command]
pub async fn desktop_preview_clear_data(
    app: AppHandle,
    tab_id: String,
    cookies: bool,
    cache: bool,
    storage: bool,
) -> Result<(), String> {
    // Each environment's previews keep their own profile, so clearing through
    // this tab's webview clears that environment's preview storage.
    let webview = host::with_tab_webview(&app, &tab_id, |webview| Ok(webview.clone()))?;

    run_on_worker(move || {
        Platform::clear_data(
            &webview,
            ClearDataKinds {
                cookies,
                cache,
                storage,
            },
        )
        .map_err(host::platform_err)
    })
    .await
}

#[tauri::command]
pub async fn desktop_preview_capture_screenshot(
    app: AppHandle,
    tab_id: String,
) -> Result<ScreenshotArtifact, String> {
    let webview = host::with_tab_webview(&app, &tab_id, |webview| Ok(webview.clone()))?;
    let id = new_screenshot_artifact_id();
    let directory = crate::config::state_dir(&app)?.join("preview-artifacts");
    let path = directory.join(format!("{id}.png"));
    let artifact_path = path.to_string_lossy().into_owned();
    let artifact_id = id.clone();
    let artifact_tab_id = tab_id.clone();

    run_on_worker(move || {
        let png =
            Platform::screenshot_png(&webview, SCREENSHOT_TIMEOUT).map_err(host::platform_err)?;
        std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        std::fs::write(&path, &png).map_err(|error| error.to_string())?;
        Ok(ScreenshotArtifact {
            id: artifact_id,
            tab_id: artifact_tab_id,
            path: artifact_path,
            mime_type: "image/png",
            size_bytes: png.len() as u64,
            created_at: host::now_iso(),
        })
    })
    .await
}

#[tauri::command]
pub fn desktop_preview_reveal_artifact(app: AppHandle, path: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;

    let directory = crate::config::state_dir(&app)?.join("preview-artifacts");
    let artifact = validate_artifact_path(&directory, Path::new(&path))?;
    app.opener()
        .reveal_item_in_dir(artifact)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::future::Future;
    use std::thread;

    use super::{
        desktop_preview_create_tab, new_screenshot_artifact_id, parse_preview_url, run_on_worker,
        validate_artifact_path,
    };

    #[test]
    fn screenshot_artifact_ids_are_unique() {
        assert_ne!(new_screenshot_artifact_id(), new_screenshot_artifact_id());
    }

    #[test]
    fn reveal_path_must_be_a_file_inside_the_artifact_directory() {
        let directory = tempfile::tempdir().expect("artifact directory");
        let artifact_directory = directory.path().join("preview-artifacts");
        fs::create_dir(&artifact_directory).expect("create artifact directory");
        let artifact = artifact_directory.join("shot.png");
        fs::write(&artifact, b"png").expect("write artifact");
        let outside = directory.path().join("outside.png");
        fs::write(&outside, b"png").expect("write outside file");

        assert_eq!(
            validate_artifact_path(&artifact_directory, &artifact).expect("valid artifact"),
            artifact.canonicalize().expect("canonical artifact")
        );
        assert!(validate_artifact_path(&artifact_directory, &outside).is_err());
        assert!(validate_artifact_path(&artifact_directory, &artifact_directory).is_err());
    }

    #[test]
    fn create_tab_command_is_async() {
        fn assert_async_command<Command, CommandFuture>(_: Command)
        where
            Command: Fn(tauri::AppHandle, String, Option<String>) -> CommandFuture,
            CommandFuture: Future<Output = Result<(), String>>,
        {
        }

        assert_async_command(desktop_preview_create_tab);
    }

    #[tokio::test]
    async fn worker_dispatch_runs_on_a_different_thread() {
        let caller = thread::current().id();
        let worker = run_on_worker(|| Ok(thread::current().id()))
            .await
            .expect("worker result");

        assert_ne!(worker, caller);
    }

    #[tokio::test]
    async fn worker_dispatch_propagates_results_and_operation_errors() {
        let result = run_on_worker(|| Ok::<_, String>(42)).await;
        assert_eq!(result, Ok(42));

        let error = run_on_worker(|| Err::<(), _>("platform failure".to_string())).await;
        assert_eq!(error, Err("platform failure".to_string()));
    }

    #[tokio::test]
    async fn worker_dispatch_propagates_join_errors() {
        let error = run_on_worker(|| -> Result<(), String> { panic!("worker panic") })
            .await
            .expect_err("panic should surface as a join error");

        assert!(error.contains("preview worker task failed"));
    }

    #[test]
    fn preview_urls_are_restricted_to_http_and_https() {
        assert!(parse_preview_url("https://example.com/path").is_ok());
        assert!(parse_preview_url("http://127.0.0.1:3000").is_ok());
        assert_eq!(
            parse_preview_url("file:///tmp/index.html").unwrap_err(),
            "only http(s) urls can be previewed"
        );
        assert_eq!(
            parse_preview_url("data:text/html,hello").unwrap_err(),
            "only http(s) urls can be previewed"
        );
    }
}
