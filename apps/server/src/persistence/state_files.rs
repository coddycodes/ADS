use std::{path::PathBuf, result::Result as StdResult};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;
use tokio::{
    fs::{self, OpenOptions},
    io::AsyncWriteExt,
};
use uuid::Uuid;

use crate::ServerConfig;

use super::StorageInstanceId;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StateKind {
    Userdata,
    Dev,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatePaths {
    pub base_dir: PathBuf,
    pub state_kind: StateKind,
    pub backups_dir: PathBuf,
    pub operation_lock: PathBuf,
    pub state_dir: PathBuf,
    pub database: PathBuf,
    pub keybindings: PathBuf,
    pub settings: PathBuf,
    pub provider_status_cache_dir: PathBuf,
    pub worktrees_dir: PathBuf,
    pub attachments_dir: PathBuf,
    pub attachment_uploads_dir: PathBuf,
    pub terminal_pastes_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub server_log: PathBuf,
    pub server_trace: PathBuf,
    pub provider_logs_dir: PathBuf,
    pub provider_event_log: PathBuf,
    pub terminal_logs_dir: PathBuf,
    pub anonymous_id: PathBuf,
    pub environment_id: PathBuf,
    pub server_runtime_state: PathBuf,
    pub secrets_dir: PathBuf,
    /// Holds the `coddy-studio-open-url` shims that agents and terminals find on `PATH`. Created by
    /// `open_url::write_shims`, not by startup: the hook is optional and must not stop the server.
    pub open_url_shim_dir: PathBuf,
}

impl StatePaths {
    #[must_use]
    pub fn from_config(config: &ServerConfig) -> Self {
        let state_dir = config.state_dir();
        let state_kind = if config.dev_url.is_some() {
            StateKind::Dev
        } else {
            StateKind::Userdata
        };
        let logs_dir = state_dir.join("logs");
        let provider_logs_dir = logs_dir.join("provider");
        Self {
            base_dir: config.base_dir.clone(),
            state_kind,
            backups_dir: config.base_dir.join("backups"),
            operation_lock: config.base_dir.join(".coddy-studio-storage.lock"),
            database: state_dir.join("state.sqlite"),
            keybindings: state_dir.join("keybindings.json"),
            settings: state_dir.join("settings.json"),
            provider_status_cache_dir: config.base_dir.join("caches"),
            worktrees_dir: config.base_dir.join("worktrees"),
            attachments_dir: state_dir.join("attachments"),
            attachment_uploads_dir: state_dir.join("attachment-uploads"),
            terminal_pastes_dir: state_dir.join("terminal-pastes"),
            server_log: logs_dir.join("server.log"),
            server_trace: logs_dir.join("server.trace.ndjson"),
            provider_event_log: provider_logs_dir.join("events.log"),
            terminal_logs_dir: logs_dir.join("terminals"),
            anonymous_id: state_dir.join("anonymous-id"),
            environment_id: state_dir.join("environment-id"),
            server_runtime_state: state_dir.join("server-runtime.json"),
            secrets_dir: state_dir.join("secrets"),
            open_url_shim_dir: state_dir.join("runtime/open-url"),
            state_dir,
            logs_dir,
            provider_logs_dir,
        }
    }

    #[must_use]
    pub fn backup_store_dir(&self, storage_instance_id: StorageInstanceId) -> PathBuf {
        self.backups_dir
            .join(match self.state_kind {
                StateKind::Userdata => "userdata",
                StateKind::Dev => "dev",
            })
            .join(storage_instance_id.to_string())
    }

    #[must_use]
    pub fn recovery_dir(&self) -> PathBuf {
        self.base_dir.join("recovery").join(match self.state_kind {
            StateKind::Userdata => "userdata",
            StateKind::Dev => "dev",
        })
    }

    #[must_use]
    pub fn recovery_journal(&self) -> PathBuf {
        self.base_dir.join(match self.state_kind {
            StateKind::Userdata => ".coddy-studio-recovery-userdata.json",
            StateKind::Dev => ".coddy-studio-recovery-dev.json",
        })
    }

    #[must_use]
    pub fn recovery_staging_dir(&self, operation_id: Uuid) -> PathBuf {
        self.base_dir
            .join(format!(".{operation_id}.recovery-staging"))
    }

    #[must_use]
    pub fn runtime_lock(&self) -> PathBuf {
        self.base_dir.join(".coddy-studio-runtime.lock")
    }

    pub async fn ensure_directories_without_database_side_effects(&self) -> Result<()> {
        for directory in [
            &self.state_dir,
            &self.provider_status_cache_dir,
            &self.worktrees_dir,
            &self.attachments_dir,
            &self.attachment_uploads_dir,
            &self.terminal_pastes_dir,
            &self.logs_dir,
            &self.provider_logs_dir,
            &self.terminal_logs_dir,
            &self.secrets_dir,
        ] {
            fs::create_dir_all(directory).await.map_err(|source| {
                StateFileError::CreateDirectory {
                    path: directory.clone(),
                    source,
                }
            })?;
        }
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.server_log)
            .await
            .map_err(|source| StateFileError::Persist {
                path: self.server_log.clone(),
                source,
            })?;
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum StateFileError {
    #[error("failed to create state directory {path}")]
    CreateDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read state file {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to decode JSON state file {path}")]
    Decode {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to encode JSON state file {path}")]
    Encode {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to persist state file {path}")]
    Persist {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub type Result<T> = StdResult<T, StateFileError>;

pub async fn read_json<T>(path: impl Into<PathBuf>) -> Result<Option<T>>
where
    T: DeserializeOwned,
{
    let path = path.into();
    let bytes = match fs::read(&path).await {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(StateFileError::Read { path, source }),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|source| StateFileError::Decode { path, source })
}

pub async fn write_json_atomically<T>(path: impl Into<PathBuf>, value: &T) -> Result<()>
where
    T: Serialize + ?Sized,
{
    let path = path.into();
    let mut contents =
        serde_json::to_vec_pretty(value).map_err(|source| StateFileError::Encode {
            path: path.clone(),
            source,
        })?;
    contents.push(b'\n');
    write_bytes_atomically(path, &contents).await
}

pub async fn write_bytes_atomically(path: impl Into<PathBuf>, contents: &[u8]) -> Result<()> {
    let path = path.into();
    let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    fs::create_dir_all(parent)
        .await
        .map_err(|source| StateFileError::CreateDirectory {
            path: parent.to_path_buf(),
            source,
        })?;
    let temporary_directory = parent.join(format!(
        "{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("state"),
        Uuid::new_v4()
    ));
    fs::create_dir(&temporary_directory)
        .await
        .map_err(|source| StateFileError::CreateDirectory {
            path: temporary_directory.clone(),
            source,
        })?;
    let temporary_path = temporary_directory.join("contents.tmp");
    let result = async {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary_path)
            .await
            .map_err(|source| StateFileError::Persist {
                path: path.clone(),
                source,
            })?;
        file.write_all(contents)
            .await
            .map_err(|source| StateFileError::Persist {
                path: path.clone(),
                source,
            })?;
        file.sync_all()
            .await
            .map_err(|source| StateFileError::Persist {
                path: path.clone(),
                source,
            })?;
        drop(file);

        match fs::rename(&temporary_path, &path).await {
            Ok(()) => Ok(()),
            Err(source) if should_replace_with_backup(&source) => {
                replace_existing_atomically(&temporary_path, &path).await
            }
            Err(source) => Err(StateFileError::Persist {
                path: path.clone(),
                source,
            }),
        }
    }
    .await;
    let _ = fs::remove_dir_all(&temporary_directory).await;
    result
}

fn should_replace_with_backup(error: &std::io::Error) -> bool {
    cfg!(windows)
        && matches!(
            error.kind(),
            std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::PermissionDenied
        )
}

#[cfg(windows)]
async fn replace_existing_atomically(
    temporary_path: &std::path::Path,
    target_path: &std::path::Path,
) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW, REPLACEFILE_WRITE_THROUGH,
        ReplaceFileW,
    };

    let replacement = temporary_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let target = target_path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let target_for_error = target_path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        // SAFETY: Both paths are valid NUL-terminated UTF-16 strings.
        if unsafe {
            ReplaceFileW(
                target.as_ptr(),
                replacement.as_ptr(),
                std::ptr::null(),
                REPLACEFILE_WRITE_THROUGH,
                std::ptr::null(),
                std::ptr::null(),
            )
        } != 0
        {
            return Ok(());
        }
        // The target may have disappeared between the original rename and replacement attempt.
        // MoveFileExW still publishes the fully synced temporary file atomically.
        if unsafe {
            MoveFileExW(
                replacement.as_ptr(),
                target.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } != 0
        {
            Ok(())
        } else {
            Err(StateFileError::Persist {
                path: target_for_error,
                source: std::io::Error::last_os_error(),
            })
        }
    })
    .await
    .map_err(|error| StateFileError::Persist {
        path: target_path.to_path_buf(),
        source: std::io::Error::other(error),
    })?
}

#[cfg(not(windows))]
async fn replace_existing_atomically(
    _temporary_path: &std::path::Path,
    target_path: &std::path::Path,
) -> Result<()> {
    Err(StateFileError::Persist {
        path: target_path.to_path_buf(),
        source: std::io::Error::other("platform does not require replacement fallback"),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use tempfile::TempDir;
    use url::Url;

    use super::*;

    #[test]
    fn derives_the_existing_userdata_and_dev_paths() {
        let base = PathBuf::from("workspace-state");
        let production = StatePaths::from_config(&ServerConfig::new(&base));
        assert_eq!(production.base_dir, base);
        assert_eq!(production.state_kind, StateKind::Userdata);
        assert_eq!(production.backups_dir, base.join("backups"));
        assert_eq!(
            production.operation_lock,
            base.join(".coddy-studio-storage.lock")
        );
        assert_eq!(production.database, base.join("userdata/state.sqlite"));
        assert_eq!(
            production.keybindings,
            base.join("userdata/keybindings.json")
        );
        assert_eq!(production.secrets_dir, base.join("userdata/secrets"));

        let development = StatePaths::from_config(
            &ServerConfig::new(&base)
                .with_dev_url(Url::parse("http://127.0.0.1:5173").expect("development URL")),
        );
        assert_eq!(development.database, base.join("dev/state.sqlite"));
        assert_eq!(development.state_kind, StateKind::Dev);
    }

    #[tokio::test]
    async fn atomic_json_writes_replace_existing_values_and_clean_up() {
        let temp = TempDir::new().expect("temporary state directory");
        let path = temp.path().join("settings.json");
        write_json_atomically(&path, &json!({ "version": 1 }))
            .await
            .expect("first write");
        write_json_atomically(&path, &json!({ "version": 2 }))
            .await
            .expect("replacement write");

        assert_eq!(
            read_json::<Value>(&path).await.unwrap(),
            Some(json!({ "version": 2 }))
        );
        assert_eq!(
            std::fs::read_dir(temp.path())
                .expect("state directory")
                .count(),
            1
        );

        let directory_source = temp.path().join("directory-source");
        fs::create_dir(&directory_source).await.unwrap();
        assert!(matches!(
            read_json::<Value>(&directory_source).await,
            Err(StateFileError::Read { .. })
        ));

        let parent_file = temp.path().join("parent-file");
        fs::write(&parent_file, "blocker").await.unwrap();
        assert!(matches!(
            write_bytes_atomically(parent_file.join("state.json"), b"value").await,
            Err(StateFileError::CreateDirectory { .. })
        ));

        let directory_target = temp.path().join("directory-target");
        fs::create_dir(&directory_target).await.unwrap();
        assert!(matches!(
            write_bytes_atomically(&directory_target, b"value").await,
            Err(StateFileError::Persist { .. })
        ));
        let replace_error = std::io::Error::new(std::io::ErrorKind::AlreadyExists, "fixture");
        #[cfg(windows)]
        assert!(should_replace_with_backup(&replace_error));
        #[cfg(not(windows))]
        assert!(!should_replace_with_backup(&replace_error));
        #[cfg(not(windows))]
        assert!(
            replace_existing_atomically(
                std::path::Path::new("temporary"),
                std::path::Path::new("target"),
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn ensuring_state_directories_creates_the_server_log_file() {
        let temp = TempDir::new().expect("temporary state directory");
        let paths = StatePaths::from_config(&ServerConfig::new(temp.path()));

        paths
            .ensure_directories_without_database_side_effects()
            .await
            .expect("state directories are created");

        assert!(paths.logs_dir.is_dir());
        assert!(paths.server_log.is_file());
        assert!(!paths.database.exists());

        fs::remove_file(&paths.server_log).await.unwrap();
        fs::create_dir(&paths.server_log).await.unwrap();
        assert!(matches!(
            paths
                .ensure_directories_without_database_side_effects()
                .await,
            Err(StateFileError::Persist { .. })
        ));
    }

    #[tokio::test]
    async fn malformed_json_is_reported_without_overwriting_the_source() {
        let temp = TempDir::new().expect("temporary state directory");
        let path = temp.path().join("keybindings.json");
        std::fs::write(&path, b"{ malformed").expect("malformed fixture");

        let error = read_json::<Value>(&path)
            .await
            .expect_err("malformed keybindings must fail");

        assert!(matches!(error, StateFileError::Decode { .. }));
        assert_eq!(
            std::fs::read(&path).expect("malformed source remains"),
            b"{ malformed"
        );
    }
}
