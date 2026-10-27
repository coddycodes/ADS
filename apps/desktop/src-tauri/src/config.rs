use coddy_studio_server::{DataRootRequest, DataRootSource, ResolvedDataRoot, resolve_data_root};
use serde_json::{Value, json};
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Manager, Runtime};

const APP_BASE_NAME: &str = "Coddy Studio";

#[cfg(test)]
#[derive(Clone, Debug)]
pub(crate) struct IsolatedTestDataRoot(PathBuf);

#[cfg(test)]
impl IsolatedTestDataRoot {
    pub(crate) fn new(path: impl Into<PathBuf>) -> Self {
        Self(path.into())
    }
}

pub(crate) fn coddy_studio_env_var(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(name)
}

fn config_error(context: &str, error: impl std::fmt::Display) -> String {
    format!("{context}: {error}")
}

pub fn is_development_build() -> bool {
    cfg!(debug_assertions)
}

pub fn app_version<R: Runtime>(app: &AppHandle<R>) -> String {
    app.package_info().version.to_string()
}

fn is_nightly_version(version: &str) -> bool {
    let Some((_, suffix)) = version.rsplit_once("-nightly.") else {
        return false;
    };
    let mut parts = suffix.split('.');
    let Some(date) = parts.next() else {
        return false;
    };
    let Some(sequence) = parts.next() else {
        return false;
    };
    parts.next().is_none()
        && date.len() == 8
        && date.chars().all(|character| character.is_ascii_digit())
        && !sequence.is_empty()
        && sequence.chars().all(|character| character.is_ascii_digit())
}

fn resolve_app_stage_label(is_development: bool, app_version: &str) -> &'static str {
    if is_development {
        "Dev"
    } else if is_nightly_version(app_version) {
        "Nightly"
    } else {
        "Latest"
    }
}

fn app_branding_value(is_development: bool, app_version: &str) -> Value {
    let stage_label = resolve_app_stage_label(is_development, app_version);
    let display_name = if stage_label == "Latest" {
        APP_BASE_NAME.to_string()
    } else {
        format!("{APP_BASE_NAME} ({stage_label})")
    };
    json!({
        "baseName": APP_BASE_NAME,
        "stageLabel": stage_label,
        "displayName": display_name,
    })
}

pub fn app_branding<R: Runtime>(app: &AppHandle<R>) -> Value {
    app_branding_value(is_development_build(), &app_version(app))
}

pub fn runtime_arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        _ => "other",
    }
}

pub fn runtime_info() -> Value {
    let arch = runtime_arch();
    json!({
        "hostArch": arch,
        "appArch": arch,
        "runningUnderArm64Translation": false,
    })
}

fn resolve_default_path_from_home(
    home_directory: &Path,
    raw_options: Option<&Value>,
) -> Option<PathBuf> {
    let initial_path = raw_options?.get("initialPath")?.as_str()?.trim();
    if initial_path.is_empty() {
        return None;
    }
    if initial_path == "~" {
        return Some(home_directory.to_path_buf());
    }
    if let Some(path) = initial_path
        .strip_prefix("~/")
        .or_else(|| initial_path.strip_prefix("~\\"))
    {
        return Some(home_directory.join(path));
    }
    let path = PathBuf::from(initial_path);
    if path.is_absolute() {
        Some(path)
    } else {
        std::env::current_dir().ok().map(|cwd| cwd.join(path))
    }
}

pub fn resolve_pick_folder_default_path<R: Runtime>(
    app: &AppHandle<R>,
    raw_options: Option<&Value>,
) -> Option<PathBuf> {
    let home_directory = app.path().home_dir().ok()?;
    resolve_default_path_from_home(&home_directory, raw_options)
}

fn resolve_data_root_from_home(
    home_dir: PathBuf,
    requested: Option<PathBuf>,
) -> Result<ResolvedDataRoot, String> {
    let request = match requested {
        Some(path) => DataRootRequest::explicit(DataRootSource::Environment, path, home_dir),
        None => DataRootRequest::default(home_dir),
    };
    resolve_data_root(request)
        .map_err(|error| config_error("Could not resolve the Coddy Studio data root", error))
}

/// The environment-provided data-root override. A Studio-specific name wins
/// over the legacy prefix, and `CODDY_HOME` selects its `studio` child so the
/// rest of `~/.coddy` keeps its own owners.
#[cfg(not(test))]
fn studio_data_root_override() -> Option<PathBuf> {
    for name in ["CODDY_STUDIO_HOME", "BIBCODE_HOME"] {
        if let Some(value) = coddy_studio_env_var(name) {
            return Some(PathBuf::from(value));
        }
    }
    coddy_studio_env_var("CODDY_HOME").map(|home| PathBuf::from(home).join("studio"))
}

pub(crate) fn data_root<R: Runtime>(app: &AppHandle<R>) -> Result<ResolvedDataRoot, String> {
    let home = app
        .path()
        .home_dir()
        .map_err(|error| config_error("Could not resolve the home directory", error))?;
    #[cfg(test)]
    let requested = app
        .try_state::<IsolatedTestDataRoot>()
        .map(|root| root.0.clone())
        .ok_or_else(|| {
            "mock Tauri apps must install an explicit isolated test data root".to_owned()
        })?;
    #[cfg(test)]
    return resolve_data_root_from_home(home, Some(requested));

    #[cfg(not(test))]
    resolve_data_root_from_home(home, studio_data_root_override())
}

pub(crate) fn base_dir<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    data_root(app).map(|resolved| resolved.effective)
}

pub fn state_dir<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    Ok(base_dir(app)?.join(if cfg!(debug_assertions) {
        "dev"
    } else {
        "userdata"
    }))
}

pub fn read_json_file(path: &Path) -> Result<Option<Value>, String> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(config_error(
                &format!("Could not read {}", path.display()),
                error,
            ));
        }
    };
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|error| config_error(&format!("Could not decode {}", path.display()), error))
}

pub fn write_json_file(path: &Path, value: &Value) -> Result<(), String> {
    let directory = path.parent().ok_or_else(|| {
        format!(
            "Could not resolve the parent directory for {}",
            path.display()
        )
    })?;
    fs::create_dir_all(directory).map_err(|error| {
        config_error(&format!("Could not create {}", directory.display()), error)
    })?;

    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let temp_path = path.with_extension(format!("tmp.{}.{}", std::process::id(), suffix));
    let encoded = serde_json::to_string_pretty(value)
        .map_err(|error| config_error("Could not encode JSON document", error))?;

    fs::write(&temp_path, format!("{encoded}\n")).map_err(|error| {
        config_error(&format!("Could not write {}", temp_path.display()), error)
    })?;

    match fs::rename(&temp_path, path) {
        Ok(()) => Ok(()),
        Err(first_error) if path.exists() => {
            fs::remove_file(path).map_err(|remove_error| {
                let _ = fs::remove_file(&temp_path);
                config_error(
                    &format!(
                        "Could not replace {} after rename failed with {first_error}",
                        path.display()
                    ),
                    remove_error,
                )
            })?;
            fs::rename(&temp_path, path).map_err(|rename_error| {
                let _ = fs::remove_file(&temp_path);
                config_error(
                    &format!("Could not replace {}", path.display()),
                    rename_error,
                )
            })
        }
        Err(error) => {
            let _ = fs::remove_file(&temp_path);
            Err(config_error(
                &format!("Could not move {} into place", temp_path.display()),
                error,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_tauri_apps_refuse_to_resolve_the_real_user_data_root() {
        use tauri::test::{mock_builder, mock_context, noop_assets};

        let app = mock_builder()
            .build(mock_context(noop_assets()))
            .expect("mock Tauri app");

        let error = data_root(app.handle())
            .expect_err("mock apps must require an explicit isolated data root");
        assert!(error.contains("isolated test data root"), "{error}");
    }

    #[test]
    fn rejects_relative_environment_data_root_before_desktop_uses_it() {
        let temp = tempfile::tempdir().expect("temporary home directory");

        let error = resolve_data_root_from_home(
            temp.path().to_path_buf(),
            Some(PathBuf::from("relative/.coddy/studio")),
        )
        .expect_err("relative environment root must fail");

        assert!(error.contains("must be absolute"));
    }

    #[test]
    fn detects_nightly_versions() {
        assert!(is_nightly_version("0.2.0-nightly.20260708.1"));
        assert!(!is_nightly_version("0.2.0-nightly.20260708"));
        assert!(!is_nightly_version("0.2.0-nightly.2026-07-08.1"));
        assert!(!is_nightly_version("0.2.0"));
    }

    #[test]
    fn resolves_app_stage_labels() {
        assert_eq!(resolve_app_stage_label(true, "0.2.0"), "Dev");
        assert_eq!(
            resolve_app_stage_label(false, "0.2.0-nightly.20260708.1"),
            "Nightly"
        );
        assert_eq!(resolve_app_stage_label(false, "0.2.0"), "Latest");
    }

    #[test]
    fn formats_app_branding_for_release_channels() {
        assert_eq!(
            app_branding_value(false, "0.2.0"),
            json!({
                "baseName": "Coddy Studio",
                "stageLabel": "Latest",
                "displayName": "Coddy Studio",
            })
        );
        assert_eq!(
            app_branding_value(true, "0.2.0"),
            json!({
                "baseName": "Coddy Studio",
                "stageLabel": "Dev",
                "displayName": "Coddy Studio (Dev)",
            })
        );
    }

    #[test]
    fn reports_runtime_info_shape() {
        let runtime = runtime_info();

        assert!(matches!(
            runtime["hostArch"].as_str(),
            Some("arm64" | "x64" | "other")
        ));
        assert_eq!(runtime["hostArch"], runtime["appArch"]);
        assert_eq!(runtime["runningUnderArm64Translation"], false);
    }

    #[test]
    fn file_and_path_failures_cover_configuration_boundaries() {
        assert!(is_development_build());
        let temp = tempfile::tempdir().expect("temporary directory");
        let invalid_json = temp.path().join("invalid.json");
        fs::write(&invalid_json, "{").expect("invalid JSON fixture");
        assert!(
            read_json_file(&invalid_json)
                .unwrap_err()
                .contains("decode")
        );

        let directory = temp.path().join("directory");
        fs::create_dir(&directory).expect("directory fixture");
        assert!(read_json_file(&directory).unwrap_err().contains("read"));

        let parent_file = temp.path().join("parent-file");
        fs::write(&parent_file, "blocker").expect("parent fixture");
        assert!(write_json_file(&parent_file.join("settings.json"), &json!({})).is_err());

        assert_eq!(resolve_default_path_from_home(temp.path(), None), None);
        assert_eq!(
            resolve_default_path_from_home(temp.path(), Some(&json!({"initialPath":" "}))),
            None
        );
        assert_eq!(
            resolve_default_path_from_home(temp.path(), Some(&json!({"initialPath":"~\\nested"}))),
            Some(temp.path().join("nested"))
        );
    }

    #[test]
    fn resolves_pick_folder_default_paths() {
        let home = PathBuf::from("C:/Users/example");

        assert_eq!(
            resolve_default_path_from_home(&home, Some(&json!({ "initialPath": "~" }))),
            Some(home.clone())
        );
        assert_eq!(
            resolve_default_path_from_home(&home, Some(&json!({ "initialPath": "~/code" }))),
            Some(PathBuf::from("C:/Users/example/code"))
        );
        assert_eq!(
            resolve_default_path_from_home(&home, Some(&json!({ "initialPath": "   " }))),
            None
        );
    }
}
