use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime},
};

use rusqlite::{Connection, ErrorCode, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use time::{OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{
    Database, MIGRATIONS, PersistenceError, PreparedStore, StateKind, StatePaths,
    StorageInstanceId, StoreStartupError,
};
use crate::{
    ServerConfig,
    data_root::{DataRootRequest, ResolvedDataRoot, resolve_data_root},
};

const BACKUP_FILE_NAME: &str = "state.sqlite";
const MANIFEST_FILE_NAME: &str = "manifest.json";
const RETAINED_GENERATIONS: usize = 3;
const BACKUP_TIMEOUT: Duration = Duration::from_secs(30);
const LOCK_WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(5);
const SQLITE_PROGRESS_OPS: i32 = 1_000;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_PUBLICATION_TIME_SKEW: Duration = Duration::from_secs(60);
const RECOVERY_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackupTrigger {
    PreMigration,
    PreUpdate,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupManifest {
    #[serde(with = "uuid_string")]
    pub backup_id: Uuid,
    pub storage_instance_id: StorageInstanceId,
    pub created_at: String,
    pub state_kind: StateKind,
    pub trigger: BackupTrigger,
    pub app_version: String,
    pub schema_version: i64,
    pub database_size_bytes: u64,
    pub sha256: String,
}

mod uuid_string {
    use serde::{Deserialize, Deserializer, Serializer, de};
    use uuid::Uuid;

    pub fn serialize<S>(value: &Uuid, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&value.to_string())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Uuid, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Uuid::parse_str(&value).map_err(|_| de::Error::custom("backup ID must be a UUID"))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    volume: u64,
    file: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PlainPathSnapshot {
    identity: FileIdentity,
    links: u64,
    size: u64,
    modified: SystemTime,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct VerifiedGenerationIdentity {
    directory: FileIdentity,
    database: FileIdentity,
    manifest: FileIdentity,
    publication_time: SystemTime,
}

#[derive(Clone, Debug)]
struct BoundDirectory {
    path: PathBuf,
    canonical: PathBuf,
    identity: FileIdentity,
    volume: u64,
}

#[derive(Clone, Debug)]
struct BackupStoreBoundary {
    state_kind_value: StateKind,
    root: BoundDirectory,
    backups: BoundDirectory,
    state_kind: BoundDirectory,
    store: BoundDirectory,
}

#[derive(Clone, Debug)]
pub struct VerifiedBackup {
    pub directory: PathBuf,
    pub database: PathBuf,
    pub manifest_path: PathBuf,
    pub manifest: BackupManifest,
    identity: VerifiedGenerationIdentity,
}

impl VerifiedBackup {
    pub async fn manifest_matches_file(&self) -> Result<bool, BackupError> {
        let backup = self.clone();
        let cancellation = CancellationToken::new();
        let _cancel_on_drop = CancelOnDrop(cancellation.clone());
        let deadline = Instant::now()
            .checked_add(BACKUP_TIMEOUT)
            .ok_or(BackupError::DeadlineElapsed)?;
        tokio::task::spawn_blocking(move || {
            backup.manifest_matches_file_blocking(Some(&cancellation), Some(deadline))
        })
        .await
        .map_err(BackupError::Worker)?
    }

    pub async fn quick_check(&self) -> Result<String, BackupError> {
        let database = self.database.clone();
        let cancellation = CancellationToken::new();
        let _cancel_on_drop = CancelOnDrop(cancellation.clone());
        let deadline = Instant::now()
            .checked_add(BACKUP_TIMEOUT)
            .ok_or(BackupError::DeadlineElapsed)?;
        tokio::task::spawn_blocking(move || {
            quick_check_database(&database, Some(&cancellation), Some(deadline))
        })
        .await
        .map_err(BackupError::Worker)?
    }

    fn manifest_matches_file_blocking(
        &self,
        cancellation: Option<&CancellationToken>,
        deadline: Option<Instant>,
    ) -> Result<bool, BackupError> {
        ensure_optional_active(cancellation, deadline)?;
        let bytes = read_manifest_bytes(&self.manifest_path)?;
        let manifest = serde_json::from_slice::<BackupManifest>(&bytes).map_err(|source| {
            BackupError::ManifestDecode {
                path: self.manifest_path.clone(),
                source,
            }
        })?;
        if manifest != self.manifest {
            return Ok(false);
        }
        let (size, sha256) = database_size_and_hash(&self.database, cancellation, deadline)?;
        Ok(size == manifest.database_size_bytes && sha256 == manifest.sha256)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupInventoryIssue {
    pub entry_name: String,
    pub message: String,
}

#[derive(Clone, Debug, Default)]
pub struct BackupInventory {
    pub verified: Vec<VerifiedBackup>,
    pub issues: Vec<BackupInventoryIssue>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StoreInspectionStatus {
    FirstRun,
    ExistingUnmarked,
    Existing,
    DatabaseMissing,
    MarkerMalformed,
    CorruptDatabase,
    UnrecognizedStore,
    UnsafeDatabaseState,
    RecoveryIncomplete,
}

#[derive(Clone, Debug)]
pub struct StoreInspection {
    pub classification: StoreInspectionStatus,
    pub storage_instance_id: Option<StorageInstanceId>,
    pub backups: Vec<VerifiedBackup>,
    pub backup_issues: Vec<BackupInventoryIssue>,
    pub requested_root: PathBuf,
    pub effective_root: PathBuf,
    pub is_filesystem_alias: bool,
    pub issue: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecoveryAction {
    Restore,
    StartEmpty,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryResult {
    #[serde(with = "uuid_string")]
    pub operation_id: Uuid,
    pub action: RecoveryAction,
    pub preserved_directory: PathBuf,
    pub storage_instance_id: Option<StorageInstanceId>,
}

#[derive(Debug, Error)]
pub enum RecoveryError {
    #[error("the effective project-data root changed before recovery")]
    RootChanged,
    #[error("the selected verified backup does not exist")]
    BackupNotFound,
    #[error("the selected backup belongs to a different storage instance")]
    StorageIdentityMismatch,
    #[error("the project-data store is currently owned by a running server")]
    StoreRunning,
    #[error("the store recovery journal already exists at {path}")]
    RecoveryInProgress { path: PathBuf },
    #[error("the storage instance marker at {path} is malformed")]
    MarkerMalformed { path: PathBuf },
    #[error("project-data recovery failed")]
    Backup(#[from] BackupError),
    #[error("project-data recovery worker failed")]
    Worker(#[source] tokio::task::JoinError),
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RecoveryJournal {
    #[serde(with = "uuid_string")]
    operation_id: Uuid,
    action: RecoveryAction,
    state_kind: StateKind,
    backup_id: Option<String>,
    phase: &'static str,
}

#[derive(Debug, Error)]
pub enum BackupError {
    #[error("failed to access backup path {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("SQLite backup failed")]
    Persistence(#[from] PersistenceError),
    #[error("backup worker failed")]
    Worker(#[source] tokio::task::JoinError),
    #[error("failed to encode backup manifest")]
    ManifestEncode(#[source] serde_json::Error),
    #[error("failed to decode backup manifest {path}")]
    ManifestDecode {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("backup at {path} failed SQLite quick_check: {detail}")]
    QuickCheck { path: PathBuf, detail: String },
    #[error("backup verification failed: {0}")]
    Verification(String),
    #[error("backup operation was cancelled")]
    Cancelled,
    #[error("backup operation deadline elapsed")]
    DeadlineElapsed,
    #[error("timed out waiting for the persistent-store operation lock {path}")]
    LockTimeout { path: PathBuf },
}

#[derive(Debug)]
pub struct StoreOperationGuard {
    lock_file: File,
}

#[derive(Debug)]
pub struct StoreRuntimeGuard {
    lock_file: File,
}

impl StoreRuntimeGuard {
    pub async fn acquire(effective_root: &Path) -> Result<Self, BackupError> {
        let lock_path = effective_root.join(".coddy-studio-runtime.lock");
        let cancellation = CancellationToken::new();
        let _cancel_on_drop = CancelOnDrop(cancellation.clone());
        let deadline = Instant::now()
            .checked_add(LOCK_WAIT_TIMEOUT)
            .ok_or_else(|| BackupError::LockTimeout {
                path: lock_path.clone(),
            })?;
        tokio::task::spawn_blocking(move || {
            let lock_file = open_private_lock_file(&lock_path)?;
            loop {
                ensure_lock_wait_active(
                    &lock_path,
                    &cancellation,
                    &CancellationToken::new(),
                    deadline,
                )?;
                match File::try_lock_shared(&lock_file) {
                    Ok(()) => return Ok(Self { lock_file }),
                    Err(std::fs::TryLockError::WouldBlock) => {
                        let remaining = deadline.saturating_duration_since(Instant::now());
                        thread::sleep(LOCK_RETRY_DELAY.min(remaining));
                    }
                    Err(std::fs::TryLockError::Error(source)) => {
                        return Err(BackupError::Io {
                            path: lock_path,
                            source,
                        });
                    }
                }
            }
        })
        .await
        .map_err(BackupError::Worker)?
    }
}

impl Drop for StoreRuntimeGuard {
    fn drop(&mut self) {
        let _ = self.lock_file.unlock();
    }
}

struct OfflineRecoveryGuard {
    lock_file: File,
}

impl OfflineRecoveryGuard {
    fn acquire(paths: &StatePaths) -> Result<Self, RecoveryError> {
        let path = paths.runtime_lock();
        let lock_file = open_private_lock_file(&path)?;
        match lock_file.try_lock() {
            Ok(()) => Ok(Self { lock_file }),
            Err(std::fs::TryLockError::WouldBlock) => Err(RecoveryError::StoreRunning),
            Err(std::fs::TryLockError::Error(source)) => {
                Err(BackupError::Io { path, source }.into())
            }
        }
    }
}

impl Drop for OfflineRecoveryGuard {
    fn drop(&mut self) {
        let _ = self.lock_file.unlock();
    }
}

impl StoreOperationGuard {
    pub async fn acquire(
        effective_root: &Path,
        cancellation: CancellationToken,
        timeout: Duration,
    ) -> Result<Self, BackupError> {
        let lock_path = effective_root.join(".coddy-studio-storage.lock");
        let deadline =
            Instant::now()
                .checked_add(timeout)
                .ok_or_else(|| BackupError::LockTimeout {
                    path: lock_path.clone(),
                })?;
        let abort = CancellationToken::new();
        let _cancel_on_drop = CancelOnDrop(abort.clone());
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let worker = tokio::task::spawn_blocking(move || {
            let result =
                acquire_operation_lock_blocking(lock_path, &cancellation, &abort, deadline);
            let _ = sender.send(result);
        });
        let result = receiver.await;
        worker.await.map_err(BackupError::Worker)?;
        result.map_err(|_| {
            BackupError::Verification("operation-lock worker returned no result".to_owned())
        })?
    }

    pub(crate) async fn acquire_for_startup(paths: &StatePaths) -> Result<Self, BackupError> {
        Self::acquire(&paths.base_dir, CancellationToken::new(), LOCK_WAIT_TIMEOUT).await
    }
}

fn acquire_operation_lock_blocking(
    lock_path: PathBuf,
    cancellation: &CancellationToken,
    abort: &CancellationToken,
    deadline: Instant,
) -> Result<StoreOperationGuard, BackupError> {
    let lock_file = open_private_lock_file(&lock_path)?;
    loop {
        ensure_lock_wait_active(&lock_path, cancellation, abort, deadline)?;
        match lock_file.try_lock() {
            Ok(()) => {
                if let Err(error) =
                    ensure_lock_wait_active(&lock_path, cancellation, abort, deadline)
                {
                    let _ = lock_file.unlock();
                    return Err(error);
                }
                return Ok(StoreOperationGuard { lock_file });
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                thread::sleep(LOCK_RETRY_DELAY.min(remaining));
            }
            Err(std::fs::TryLockError::Error(source)) => {
                return Err(BackupError::Io {
                    path: lock_path,
                    source,
                });
            }
        }
    }
}

fn ensure_lock_wait_active(
    lock_path: &Path,
    cancellation: &CancellationToken,
    abort: &CancellationToken,
    deadline: Instant,
) -> Result<(), BackupError> {
    if cancellation.is_cancelled() || abort.is_cancelled() {
        Err(BackupError::Cancelled)
    } else if Instant::now() >= deadline {
        Err(BackupError::LockTimeout {
            path: lock_path.to_path_buf(),
        })
    } else {
        Ok(())
    }
}

impl Drop for StoreOperationGuard {
    fn drop(&mut self) {
        let _ = self.lock_file.unlock();
    }
}

struct CancelOnDrop(CancellationToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BackupFault {
    None,
    BeforeBackupsDirectorySync,
    BeforeBackupsParentSync,
    BeforeStateKindDirectorySync,
    BeforeStateKindParentSync,
    BeforeStoreDirectorySync,
    BeforeStoreParentSync,
    BeforeQuickCheck,
    BeforeHash,
    BeforeDatabaseSync,
    BeforeManifestWrite,
    BeforeStagingSync,
    BeforePublish,
    BeforeParentSync,
    BeforeReloadVerification,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecoveryFault {
    None,
    AfterPreserve,
}

impl RecoveryFault {
    fn inject(self, phase: Self) -> Result<(), RecoveryError> {
        if self == phase {
            Err(BackupError::Verification(format!("injected recovery failure at {phase:?}")).into())
        } else {
            Ok(())
        }
    }
}

impl BackupFault {
    fn inject(self, phase: Self) -> Result<(), BackupError> {
        if self == phase {
            Err(BackupError::Verification(format!(
                "injected backup failure at {phase:?}"
            )))
        } else {
            Ok(())
        }
    }
}

pub async fn create_verified_backup(
    database: &Database,
    prepared: &PreparedStore,
    trigger: BackupTrigger,
    app_version: &str,
) -> Result<VerifiedBackup, BackupError> {
    let cancellation = CancellationToken::new();
    let _cancel_on_drop = CancelOnDrop(cancellation.clone());
    let deadline = Instant::now()
        .checked_add(BACKUP_TIMEOUT)
        .ok_or(BackupError::DeadlineElapsed)?;
    let backup_id = Uuid::new_v4();
    let created_at = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|error| BackupError::Verification(format!("UTC timestamp failed: {error}")))?;
    let paths = prepared.paths.clone();
    let storage_instance_id = prepared.storage_instance_id;
    let app_version = app_version.to_owned();
    let staging = tokio::task::spawn_blocking({
        let paths = paths.clone();
        let cancellation = cancellation.clone();
        move || {
            prepare_staging_directory(
                &paths,
                storage_instance_id,
                backup_id,
                &cancellation,
                deadline,
                BackupFault::None,
            )
        }
    })
    .await
    .map_err(BackupError::Worker)??;

    async {
        database
            .backup_to_cancellable(&staging.database, cancellation.clone(), deadline)
            .await?;
        let verified = tokio::task::spawn_blocking({
            let paths = paths.clone();
            let cancellation = cancellation.clone();
            move || {
                finish_and_publish_backup(
                    &paths,
                    storage_instance_id,
                    trigger,
                    &app_version,
                    backup_id,
                    &created_at,
                    staging,
                    &cancellation,
                    deadline,
                    BackupFault::None,
                )
            }
        })
        .await
        .map_err(BackupError::Worker)??;
        Ok(verified)
    }
    .await
}

pub async fn inventory_verified_backups(
    paths: &StatePaths,
    storage_instance_id: StorageInstanceId,
) -> Result<BackupInventory, BackupError> {
    let paths = paths.clone();
    let cancellation = CancellationToken::new();
    let _cancel_on_drop = CancelOnDrop(cancellation.clone());
    let deadline = Instant::now()
        .checked_add(BACKUP_TIMEOUT)
        .ok_or(BackupError::DeadlineElapsed)?;
    tokio::task::spawn_blocking(move || {
        inventory_blocking(
            &paths,
            storage_instance_id,
            Some(&cancellation),
            Some(deadline),
        )
    })
    .await
    .map_err(BackupError::Worker)?
}

pub async fn inspect_store(root: &ResolvedDataRoot) -> Result<StoreInspection, RecoveryError> {
    let current = resolve_data_root(DataRootRequest {
        source: root.source,
        requested: Some(root.requested.clone()),
        home_dir: PathBuf::new(),
    })
    .map_err(|_| RecoveryError::RootChanged)?;
    if current.effective != root.effective {
        return Err(RecoveryError::RootChanged);
    }
    let mut config = ServerConfig::new(&current.effective);
    config.base_dir.clone_from(&current.effective);
    config.resolved_data_root = Some(current.clone());
    let paths = StatePaths::from_config(&config);
    if !path_entry_exists(&paths.base_dir)? {
        return Ok(StoreInspection {
            classification: StoreInspectionStatus::FirstRun,
            storage_instance_id: None,
            backups: Vec::new(),
            backup_issues: Vec::new(),
            requested_root: current.requested,
            effective_root: current.effective,
            is_filesystem_alias: current.is_filesystem_alias,
            issue: None,
        });
    }
    let journal_exists = path_entry_exists(&paths.recovery_journal())?;
    let staging = find_recovery_staging_entry(&paths)?;
    let database_exists = path_entry_exists(&paths.database)?;
    let marker_exists = path_entry_exists(&paths.environment_id)?;
    let marker = if marker_exists {
        let bytes = fs::read(&paths.environment_id).map_err(|source| BackupError::Io {
            path: paths.environment_id.clone(),
            source,
        })?;
        std::str::from_utf8(&bytes)
            .ok()
            .and_then(|value| Uuid::parse_str(value.trim()).ok())
            .map(StorageInstanceId::from_uuid)
    } else {
        None
    };
    let (classification, issue) = if journal_exists || staging.is_some() {
        (
            StoreInspectionStatus::RecoveryIncomplete,
            Some("An incomplete project-data recovery operation requires attention.".to_owned()),
        )
    } else if marker_exists && marker.is_none() {
        (
            StoreInspectionStatus::MarkerMalformed,
            Some("The storage instance marker is malformed.".to_owned()),
        )
    } else {
        match (database_exists, marker) {
            (false, None) => (StoreInspectionStatus::FirstRun, None),
            (false, Some(_)) => (
                StoreInspectionStatus::DatabaseMissing,
                Some("The database is missing while its storage marker remains.".to_owned()),
            ),
            (true, marker) => {
                match super::store::validate_existing_store_for_inspection(&paths.database).await {
                    Ok(()) if marker.is_some() => (StoreInspectionStatus::Existing, None),
                    Ok(()) => (StoreInspectionStatus::ExistingUnmarked, None),
                    Err(error) => {
                        let status = match &error {
                            StoreStartupError::CorruptDatabase { .. } => {
                                StoreInspectionStatus::CorruptDatabase
                            }
                            StoreStartupError::UnrecognizedStore { .. } => {
                                StoreInspectionStatus::UnrecognizedStore
                            }
                            _ => StoreInspectionStatus::UnsafeDatabaseState,
                        };
                        (status, Some(error.to_string()))
                    }
                }
            }
        }
    };
    let inventory = if let Some(storage_instance_id) = marker {
        inventory_verified_backups(&paths, storage_instance_id).await?
    } else {
        inventory_all_verified_backups(&paths).await?
    };
    Ok(StoreInspection {
        classification,
        storage_instance_id: marker,
        backups: inventory.verified,
        backup_issues: inventory.issues,
        requested_root: current.requested,
        effective_root: current.effective,
        is_filesystem_alias: current.is_filesystem_alias,
        issue,
    })
}

async fn inventory_all_verified_backups(
    paths: &StatePaths,
) -> Result<BackupInventory, BackupError> {
    let paths = paths.clone();
    let cancellation = CancellationToken::new();
    let _cancel_on_drop = CancelOnDrop(cancellation.clone());
    let deadline = Instant::now()
        .checked_add(BACKUP_TIMEOUT)
        .ok_or(BackupError::DeadlineElapsed)?;
    tokio::task::spawn_blocking(move || {
        let state_kind_directory = paths.backups_dir.join(state_kind_name(paths.state_kind));
        let entries = match fs::read_dir(&state_kind_directory) {
            Ok(entries) => entries,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(BackupInventory::default());
            }
            Err(source) => {
                return Err(BackupError::Io {
                    path: state_kind_directory,
                    source,
                });
            }
        };
        let mut combined = BackupInventory::default();
        for entry in entries {
            ensure_active(&cancellation, Some(deadline))?;
            let entry = entry.map_err(|source| BackupError::Io {
                path: state_kind_directory.clone(),
                source,
            })?;
            let Some(storage_instance_id) = entry
                .file_name()
                .to_str()
                .and_then(|name| Uuid::parse_str(name).ok())
                .map(StorageInstanceId::from_uuid)
            else {
                combined.issues.push(BackupInventoryIssue {
                    entry_name: entry.file_name().to_string_lossy().into_owned(),
                    message: "backup store directory name is not a storage UUID".to_owned(),
                });
                continue;
            };
            match inventory_blocking(
                &paths,
                storage_instance_id,
                Some(&cancellation),
                Some(deadline),
            ) {
                Ok(mut inventory) => {
                    combined.verified.append(&mut inventory.verified);
                    combined.issues.append(&mut inventory.issues);
                }
                Err(error) => combined.issues.push(BackupInventoryIssue {
                    entry_name: entry.file_name().to_string_lossy().into_owned(),
                    message: error.to_string(),
                }),
            }
        }
        combined
            .verified
            .sort_by_key(|backup| backup.identity.publication_time);
        Ok(combined)
    })
    .await
    .map_err(BackupError::Worker)?
}

fn path_entry_exists(path: &Path) -> Result<bool, BackupError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(BackupError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn find_recovery_staging_entry(paths: &StatePaths) -> Result<Option<PathBuf>, BackupError> {
    for entry in fs::read_dir(&paths.base_dir).map_err(|source| BackupError::Io {
        path: paths.base_dir.clone(),
        source,
    })? {
        let entry = entry.map_err(|source| BackupError::Io {
            path: paths.base_dir.clone(),
            source,
        })?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Some(operation_id) = name
            .strip_prefix('.')
            .and_then(|name| name.strip_suffix(".recovery-staging"))
        else {
            continue;
        };
        if Uuid::parse_str(operation_id).is_ok() {
            return Ok(Some(entry.path()));
        }
    }
    Ok(None)
}

pub async fn restore_backup(
    root: &ResolvedDataRoot,
    backup_id: Uuid,
) -> Result<RecoveryResult, RecoveryError> {
    restore_backup_inner(root, backup_id, RecoveryFault::None).await
}

#[cfg(test)]
async fn restore_backup_with_fault(
    root: &ResolvedDataRoot,
    backup_id: Uuid,
    fault: RecoveryFault,
) -> Result<RecoveryResult, RecoveryError> {
    restore_backup_inner(root, backup_id, fault).await
}

async fn restore_backup_inner(
    root: &ResolvedDataRoot,
    backup_id: Uuid,
    fault: RecoveryFault,
) -> Result<RecoveryResult, RecoveryError> {
    let current = resolve_data_root(DataRootRequest {
        source: root.source,
        requested: Some(root.requested.clone()),
        home_dir: PathBuf::new(),
    })
    .map_err(|_| RecoveryError::RootChanged)?;
    if current.effective != root.effective {
        return Err(RecoveryError::RootChanged);
    }
    let mut config = ServerConfig::new(&current.effective);
    config.base_dir.clone_from(&current.effective);
    config.resolved_data_root = Some(current.clone());
    let paths = StatePaths::from_config(&config);
    let _offline = OfflineRecoveryGuard::acquire(&paths)?;
    let cancellation = CancellationToken::new();
    let _cancel_on_drop = CancelOnDrop(cancellation.clone());
    let _guard =
        StoreOperationGuard::acquire(&paths.base_dir, cancellation.clone(), RECOVERY_TIMEOUT)
            .await?;
    let deadline = Instant::now()
        .checked_add(RECOVERY_TIMEOUT)
        .ok_or(BackupError::DeadlineElapsed)?;
    tokio::task::spawn_blocking(move || {
        restore_backup_blocking(paths, current, backup_id, &cancellation, deadline, fault)
    })
    .await
    .map_err(RecoveryError::Worker)?
}

fn restore_backup_blocking(
    paths: StatePaths,
    expected_root: ResolvedDataRoot,
    backup_id: Uuid,
    cancellation: &CancellationToken,
    deadline: Instant,
    fault: RecoveryFault,
) -> Result<RecoveryResult, RecoveryError> {
    ensure_active(cancellation, Some(deadline))?;
    let current = resolve_data_root(DataRootRequest {
        source: expected_root.source,
        requested: Some(expected_root.requested.clone()),
        home_dir: PathBuf::new(),
    })
    .map_err(|_| RecoveryError::RootChanged)?;
    if current.effective != expected_root.effective || current.effective != paths.base_dir {
        return Err(RecoveryError::RootChanged);
    }
    ensure_no_recovery_in_progress(&paths)?;
    let marker_exists = path_entry_exists(&paths.environment_id)?;
    let marker_id = if marker_exists {
        let marker_bytes = fs::read(&paths.environment_id).map_err(|source| BackupError::Io {
            path: paths.environment_id.clone(),
            source,
        })?;
        std::str::from_utf8(&marker_bytes)
            .ok()
            .and_then(|value| Uuid::parse_str(value.trim()).ok())
            .map(StorageInstanceId::from_uuid)
    } else {
        None
    };
    let selected = find_verified_backup_for_recovery(&paths, backup_id, cancellation, deadline)?
        .ok_or(RecoveryError::BackupNotFound)?;
    if marker_id.is_some_and(|marker_id| selected.manifest.storage_instance_id != marker_id) {
        return Err(RecoveryError::StorageIdentityMismatch);
    }
    let restored_storage_id = selected.manifest.storage_instance_id;

    let root_boundary = inspect_root_directory(&paths.base_dir)?;
    let state_boundary = inspect_child_directory(&root_boundary, &paths.state_dir)?;
    let recovery_root = paths.base_dir.join("recovery");
    let recovery_root_boundary = ensure_backup_component(
        &root_boundary,
        &recovery_root,
        true,
        BackupFault::None,
        BackupFault::BeforeBackupsDirectorySync,
        BackupFault::BeforeBackupsParentSync,
    )?
    .expect("recovery root was created");
    let recovery_kind = paths.recovery_dir();
    let recovery_kind_boundary = ensure_backup_component(
        &recovery_root_boundary,
        &recovery_kind,
        true,
        BackupFault::None,
        BackupFault::BeforeStateKindDirectorySync,
        BackupFault::BeforeStateKindParentSync,
    )?
    .expect("recovery state-kind directory was created");
    let operation_id = Uuid::new_v4();
    let preserved_directory = recovery_kind.join(format!(
        "{}-{operation_id}",
        OffsetDateTime::now_utc().unix_timestamp()
    ));
    create_private_directory(&preserved_directory)?;
    sync_directory(&preserved_directory)?;
    sync_directory(&recovery_kind)?;
    let preserved_boundary =
        inspect_child_directory(&recovery_kind_boundary, &preserved_directory)?;

    let staging_directory = paths.recovery_staging_dir(operation_id);
    create_private_directory(&staging_directory)?;
    let staging_boundary = inspect_child_directory(&root_boundary, &staging_directory)?;
    let staged_database = staging_directory.join(BACKUP_FILE_NAME);
    copy_verified_database(&selected, &staged_database, cancellation, deadline)?;
    let staged_marker = staging_directory.join("environment-id");
    let mut marker = private_create_new(&staged_marker)?;
    marker
        .write_all(format!("{restored_storage_id}\n").as_bytes())
        .and_then(|()| marker.sync_all())
        .map_err(|source| BackupError::Io {
            path: staged_marker.clone(),
            source,
        })?;
    drop(marker);
    sync_directory(&staging_directory)?;
    ensure_active(cancellation, Some(deadline))?;

    let journal = RecoveryJournal {
        operation_id,
        action: RecoveryAction::Restore,
        state_kind: paths.state_kind,
        backup_id: Some(backup_id.to_string()),
        phase: "preserving-live-store",
    };
    let mut journal_bytes =
        serde_json::to_vec_pretty(&journal).map_err(BackupError::ManifestEncode)?;
    journal_bytes.push(b'\n');
    let journal_path = paths.recovery_journal();
    let mut journal_file = private_create_new(&journal_path)?;
    journal_file
        .write_all(&journal_bytes)
        .and_then(|()| journal_file.sync_all())
        .map_err(|source| BackupError::Io {
            path: journal_path.clone(),
            source,
        })?;
    drop(journal_file);
    sync_directory(&paths.base_dir)?;

    preserve_live_file(&state_boundary, &paths.database, &preserved_boundary)?;
    for suffix in ["-wal", "-shm"] {
        let sidecar = sqlite_sidecar_path(&paths.database, suffix);
        if fs::symlink_metadata(&sidecar).is_ok() {
            preserve_live_file(&state_boundary, &sidecar, &preserved_boundary)?;
        }
    }
    if marker_exists {
        preserve_live_file(&state_boundary, &paths.environment_id, &preserved_boundary)?;
    }
    sync_directory(&preserved_directory)?;
    sync_directory(&paths.state_dir)?;
    fault.inject(RecoveryFault::AfterPreserve)?;

    move_staged_file(&staging_boundary, &staged_database, &paths.database)?;
    move_staged_file(&staging_boundary, &staged_marker, &paths.environment_id)?;
    sync_directory(&paths.state_dir)?;
    fs::remove_dir(&staging_directory).map_err(|source| BackupError::Io {
        path: staging_directory.clone(),
        source,
    })?;
    fs::remove_file(&journal_path).map_err(|source| BackupError::Io {
        path: journal_path.clone(),
        source,
    })?;
    sync_directory(&paths.base_dir)?;
    Ok(RecoveryResult {
        operation_id,
        action: RecoveryAction::Restore,
        preserved_directory,
        storage_instance_id: Some(restored_storage_id),
    })
}

pub async fn preserve_and_start_empty(
    root: &ResolvedDataRoot,
) -> Result<RecoveryResult, RecoveryError> {
    let current = resolve_data_root(DataRootRequest {
        source: root.source,
        requested: Some(root.requested.clone()),
        home_dir: PathBuf::new(),
    })
    .map_err(|_| RecoveryError::RootChanged)?;
    if current.effective != root.effective {
        return Err(RecoveryError::RootChanged);
    }
    let mut config = ServerConfig::new(&current.effective);
    config.base_dir.clone_from(&current.effective);
    config.resolved_data_root = Some(current.clone());
    let paths = StatePaths::from_config(&config);
    let _offline = OfflineRecoveryGuard::acquire(&paths)?;
    let cancellation = CancellationToken::new();
    let _cancel_on_drop = CancelOnDrop(cancellation.clone());
    let _guard =
        StoreOperationGuard::acquire(&paths.base_dir, cancellation.clone(), RECOVERY_TIMEOUT)
            .await?;
    let deadline = Instant::now()
        .checked_add(RECOVERY_TIMEOUT)
        .ok_or(BackupError::DeadlineElapsed)?;
    tokio::task::spawn_blocking(move || {
        preserve_and_start_empty_blocking(paths, current, &cancellation, deadline)
    })
    .await
    .map_err(RecoveryError::Worker)?
}

fn preserve_and_start_empty_blocking(
    paths: StatePaths,
    expected_root: ResolvedDataRoot,
    cancellation: &CancellationToken,
    deadline: Instant,
) -> Result<RecoveryResult, RecoveryError> {
    ensure_active(cancellation, Some(deadline))?;
    let current = resolve_data_root(DataRootRequest {
        source: expected_root.source,
        requested: Some(expected_root.requested.clone()),
        home_dir: PathBuf::new(),
    })
    .map_err(|_| RecoveryError::RootChanged)?;
    if current.effective != expected_root.effective || current.effective != paths.base_dir {
        return Err(RecoveryError::RootChanged);
    }
    ensure_no_recovery_in_progress(&paths)?;
    let root_boundary = inspect_root_directory(&paths.base_dir)?;
    let state_boundary = inspect_child_directory(&root_boundary, &paths.state_dir)?;
    let recovery_root = paths.base_dir.join("recovery");
    let recovery_root_boundary = ensure_backup_component(
        &root_boundary,
        &recovery_root,
        true,
        BackupFault::None,
        BackupFault::BeforeBackupsDirectorySync,
        BackupFault::BeforeBackupsParentSync,
    )?
    .expect("recovery root was created");
    let recovery_kind = paths.recovery_dir();
    let recovery_kind_boundary = ensure_backup_component(
        &recovery_root_boundary,
        &recovery_kind,
        true,
        BackupFault::None,
        BackupFault::BeforeStateKindDirectorySync,
        BackupFault::BeforeStateKindParentSync,
    )?
    .expect("recovery state-kind directory was created");
    let operation_id = Uuid::new_v4();
    let preserved_directory = recovery_kind.join(format!(
        "{}-{operation_id}",
        OffsetDateTime::now_utc().unix_timestamp()
    ));
    create_private_directory(&preserved_directory)?;
    sync_directory(&preserved_directory)?;
    sync_directory(&recovery_kind)?;
    let preserved_boundary =
        inspect_child_directory(&recovery_kind_boundary, &preserved_directory)?;
    let journal = RecoveryJournal {
        operation_id,
        action: RecoveryAction::StartEmpty,
        state_kind: paths.state_kind,
        backup_id: None,
        phase: "preserving-live-store",
    };
    let mut journal_bytes =
        serde_json::to_vec_pretty(&journal).map_err(BackupError::ManifestEncode)?;
    journal_bytes.push(b'\n');
    let journal_path = paths.recovery_journal();
    let mut journal_file = private_create_new(&journal_path)?;
    journal_file
        .write_all(&journal_bytes)
        .and_then(|()| journal_file.sync_all())
        .map_err(|source| BackupError::Io {
            path: journal_path.clone(),
            source,
        })?;
    drop(journal_file);
    sync_directory(&paths.base_dir)?;

    for source in [
        paths.database.clone(),
        sqlite_sidecar_path(&paths.database, "-wal"),
        sqlite_sidecar_path(&paths.database, "-shm"),
        paths.environment_id.clone(),
    ] {
        match fs::symlink_metadata(&source) {
            Ok(_) => preserve_live_file(&state_boundary, &source, &preserved_boundary)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source_error) => {
                return Err(BackupError::Io {
                    path: source,
                    source: source_error,
                }
                .into());
            }
        }
    }
    sync_directory(&preserved_directory)?;
    sync_directory(&paths.state_dir)?;
    fs::remove_file(&journal_path).map_err(|source| BackupError::Io {
        path: journal_path.clone(),
        source,
    })?;
    sync_directory(&paths.base_dir)?;
    Ok(RecoveryResult {
        operation_id,
        action: RecoveryAction::StartEmpty,
        preserved_directory,
        storage_instance_id: None,
    })
}

fn ensure_no_recovery_in_progress(paths: &StatePaths) -> Result<(), RecoveryError> {
    if path_entry_exists(&paths.recovery_journal())? {
        return Err(RecoveryError::RecoveryInProgress {
            path: paths.recovery_journal(),
        });
    }
    if let Some(path) = find_recovery_staging_entry(paths)? {
        return Err(RecoveryError::RecoveryInProgress { path });
    }
    Ok(())
}

fn find_verified_backup_for_recovery(
    paths: &StatePaths,
    backup_id: Uuid,
    cancellation: &CancellationToken,
    deadline: Instant,
) -> Result<Option<VerifiedBackup>, BackupError> {
    ensure_active(cancellation, Some(deadline))?;
    let state_kind_directory = paths.backups_dir.join(state_kind_name(paths.state_kind));
    let entries = match fs::read_dir(&state_kind_directory) {
        Ok(entries) => entries,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(BackupError::Io {
                path: state_kind_directory,
                source,
            });
        }
    };
    let mut selected_entry_failed_verification = false;
    for entry in entries {
        ensure_active(cancellation, Some(deadline))?;
        let entry = entry.map_err(|source| BackupError::Io {
            path: state_kind_directory.clone(),
            source,
        })?;
        let Some(storage_id) = entry
            .file_name()
            .to_str()
            .and_then(|name| Uuid::parse_str(name).ok())
            .map(StorageInstanceId::from_uuid)
        else {
            continue;
        };
        let selected_path = paths
            .backup_store_dir(storage_id)
            .join(backup_id.to_string());
        let selected_entry_exists = match fs::symlink_metadata(&selected_path) {
            Ok(_) => true,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => false,
            Err(source) => {
                return Err(BackupError::Io {
                    path: selected_path,
                    source,
                });
            }
        };
        let Ok(inventory) =
            inventory_blocking(paths, storage_id, Some(cancellation), Some(deadline))
        else {
            selected_entry_failed_verification |= selected_entry_exists;
            continue;
        };
        if let Some(backup) = inventory
            .verified
            .into_iter()
            .find(|backup| backup.manifest.backup_id == backup_id)
        {
            return Ok(Some(backup));
        }
        selected_entry_failed_verification |= selected_entry_exists;
    }
    if selected_entry_failed_verification {
        return Err(BackupError::Verification(
            "the selected backup exists but failed verification".to_owned(),
        ));
    }
    Ok(None)
}

fn copy_verified_database(
    selected: &VerifiedBackup,
    destination: &Path,
    cancellation: &CancellationToken,
    deadline: Instant,
) -> Result<(), BackupError> {
    let source_snapshot = plain_path_snapshot(&selected.database, PlainPathKind::File)?;
    if source_snapshot.identity != selected.identity.database {
        return Err(BackupError::Verification(
            "selected backup changed before restore staging".to_owned(),
        ));
    }
    let mut source =
        open_path_without_following(&selected.database, PlainPathKind::File).map_err(|source| {
            BackupError::Io {
                path: selected.database.clone(),
                source,
            }
        })?;
    let opened = source.metadata().map_err(|source| BackupError::Io {
        path: selected.database.clone(),
        source,
    })?;
    let opened_snapshot = file_snapshot(&source, &opened).map_err(|source| BackupError::Io {
        path: selected.database.clone(),
        source,
    })?;
    if opened_snapshot.identity != selected.identity.database {
        return Err(BackupError::Verification(
            "selected backup changed while opening restore source".to_owned(),
        ));
    }
    let mut target = private_create_new(destination)?;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        ensure_active(cancellation, Some(deadline))?;
        let count = source.read(&mut buffer).map_err(|source| BackupError::Io {
            path: selected.database.clone(),
            source,
        })?;
        if count == 0 {
            break;
        }
        target
            .write_all(&buffer[..count])
            .map_err(|source| BackupError::Io {
                path: destination.to_path_buf(),
                source,
            })?;
    }
    target.sync_all().map_err(|source| BackupError::Io {
        path: destination.to_path_buf(),
        source,
    })?;
    drop(target);
    let (size, sha256) = database_size_and_hash(destination, Some(cancellation), Some(deadline))?;
    if size != selected.manifest.database_size_bytes || sha256 != selected.manifest.sha256 {
        return Err(BackupError::Verification(
            "staged restore database does not match its verified manifest".to_owned(),
        ));
    }
    let integrity = quick_check_database(destination, Some(cancellation), Some(deadline))?;
    if integrity != "ok" {
        return Err(BackupError::QuickCheck {
            path: destination.to_path_buf(),
            detail: integrity,
        });
    }
    Ok(())
}

fn preserve_live_file(
    state: &BoundDirectory,
    source: &Path,
    preserved: &BoundDirectory,
) -> Result<(), BackupError> {
    let snapshot = inspect_plain_child_file(state, source)?;
    let destination = preserved.path.join(
        source
            .file_name()
            .ok_or_else(|| BackupError::Verification("live store file has no name".to_owned()))?,
    );
    if fs::symlink_metadata(&destination).is_ok() {
        return Err(BackupError::Verification(
            "recovery preservation destination already exists".to_owned(),
        ));
    }
    fs::rename(source, &destination).map_err(|source_error| BackupError::Io {
        path: source.to_path_buf(),
        source: source_error,
    })?;
    let preserved_snapshot = inspect_plain_child_file(preserved, &destination)?;
    if preserved_snapshot.identity != snapshot.identity {
        return Err(BackupError::Verification(
            "preserved live store file identity changed".to_owned(),
        ));
    }
    Ok(())
}

fn move_staged_file(
    staging: &BoundDirectory,
    source: &Path,
    destination: &Path,
) -> Result<(), BackupError> {
    let snapshot = inspect_plain_child_file(staging, source)?;
    if fs::symlink_metadata(destination).is_ok() {
        return Err(BackupError::Verification(
            "live restore destination unexpectedly exists".to_owned(),
        ));
    }
    fs::rename(source, destination).map_err(|source_error| BackupError::Io {
        path: destination.to_path_buf(),
        source: source_error,
    })?;
    let confirmed = plain_path_snapshot(destination, PlainPathKind::File)?;
    if confirmed.identity != snapshot.identity {
        return Err(BackupError::Verification(
            "restored live file identity changed during publication".to_owned(),
        ));
    }
    Ok(())
}

fn sqlite_sidecar_path(database: &Path, suffix: &str) -> PathBuf {
    let mut path = database.as_os_str().to_os_string();
    path.push(suffix);
    PathBuf::from(path)
}

impl BackupStoreBoundary {
    fn open_or_create(
        paths: &StatePaths,
        storage_instance_id: StorageInstanceId,
        fault: BackupFault,
    ) -> Result<Self, BackupError> {
        let root = inspect_root_directory(&paths.base_dir)?;
        let backups = ensure_backup_component(
            &root,
            &paths.backups_dir,
            true,
            fault,
            BackupFault::BeforeBackupsDirectorySync,
            BackupFault::BeforeBackupsParentSync,
        )?
        .expect("creating the backups directory returns a boundary");
        let state_kind_path = paths.backups_dir.join(state_kind_name(paths.state_kind));
        let state_kind = ensure_backup_component(
            &backups,
            &state_kind_path,
            true,
            fault,
            BackupFault::BeforeStateKindDirectorySync,
            BackupFault::BeforeStateKindParentSync,
        )?
        .expect("creating the state-kind directory returns a boundary");
        let store_path = paths.backup_store_dir(storage_instance_id);
        let store = ensure_backup_component(
            &state_kind,
            &store_path,
            true,
            fault,
            BackupFault::BeforeStoreDirectorySync,
            BackupFault::BeforeStoreParentSync,
        )?
        .expect("creating the backup-store directory returns a boundary");
        let boundary = Self {
            state_kind_value: paths.state_kind,
            root,
            backups,
            state_kind,
            store,
        };
        boundary.revalidate()?;
        Ok(boundary)
    }

    fn open_existing(
        paths: &StatePaths,
        storage_instance_id: StorageInstanceId,
    ) -> Result<Option<Self>, BackupError> {
        let root = inspect_root_directory(&paths.base_dir)?;
        let Some(backups) = ensure_backup_component(
            &root,
            &paths.backups_dir,
            false,
            BackupFault::None,
            BackupFault::None,
            BackupFault::None,
        )?
        else {
            return Ok(None);
        };
        let state_kind_path = paths.backups_dir.join(state_kind_name(paths.state_kind));
        let Some(state_kind) = ensure_backup_component(
            &backups,
            &state_kind_path,
            false,
            BackupFault::None,
            BackupFault::None,
            BackupFault::None,
        )?
        else {
            return Ok(None);
        };
        let store_path = paths.backup_store_dir(storage_instance_id);
        let Some(store) = ensure_backup_component(
            &state_kind,
            &store_path,
            false,
            BackupFault::None,
            BackupFault::None,
            BackupFault::None,
        )?
        else {
            return Ok(None);
        };
        let boundary = Self {
            state_kind_value: paths.state_kind,
            root,
            backups,
            state_kind,
            store,
        };
        boundary.revalidate()?;
        Ok(Some(boundary))
    }

    fn revalidate(&self) -> Result<(), BackupError> {
        self.root.revalidate(None)?;
        self.backups.revalidate(Some(&self.root))?;
        self.state_kind.revalidate(Some(&self.backups))?;
        self.store.revalidate(Some(&self.state_kind))
    }
}

impl BoundDirectory {
    fn revalidate(&self, parent: Option<&Self>) -> Result<(), BackupError> {
        let current = match parent {
            Some(parent) => inspect_child_directory(parent, &self.path)?,
            None => inspect_root_directory(&self.path)?,
        };
        if current.identity != self.identity
            || current.canonical != self.canonical
            || current.volume != self.volume
        {
            return Err(BackupError::Verification(format!(
                "backup directory identity changed: {}",
                self.path.display()
            )));
        }
        Ok(())
    }
}

fn state_kind_name(state_kind: StateKind) -> &'static str {
    match state_kind {
        StateKind::Userdata => "userdata",
        StateKind::Dev => "dev",
    }
}

fn ensure_backup_component(
    parent: &BoundDirectory,
    path: &Path,
    create: bool,
    fault: BackupFault,
    before_directory_sync: BackupFault,
    before_parent_sync: BackupFault,
) -> Result<Option<BoundDirectory>, BackupError> {
    parent.revalidate(None)?;
    let created = match fs::symlink_metadata(path) {
        Ok(_) => false,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound && create => {
            create_private_directory(path)?;
            true
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(BackupError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let mut directory = inspect_child_directory(parent, path)?;
    if create {
        set_private_directory_permissions(path)?;
        directory = inspect_child_directory(parent, path)?;
    }
    if created {
        fault.inject(before_directory_sync)?;
        sync_directory(path)?;
        fault.inject(before_parent_sync)?;
        parent.revalidate(None)?;
        sync_directory(&parent.path)?;
        directory = inspect_child_directory(parent, path)?;
    }
    Ok(Some(directory))
}

fn path_parent(path: &Path) -> Result<&Path, BackupError> {
    path.parent().ok_or_else(|| {
        BackupError::Verification("backup component has no parent directory".to_owned())
    })
}

fn inspect_root_directory(path: &Path) -> Result<BoundDirectory, BackupError> {
    let snapshot = plain_path_snapshot(path, PlainPathKind::Directory)?;
    let canonical = fs::canonicalize(path).map_err(|source| BackupError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let confirmed = plain_path_snapshot(path, PlainPathKind::Directory)?;
    if confirmed.identity != snapshot.identity {
        return Err(BackupError::Verification(
            "backup directory identity changed during inspection".to_owned(),
        ));
    }
    Ok(BoundDirectory {
        path: path.to_path_buf(),
        canonical,
        identity: snapshot.identity,
        volume: snapshot.identity.volume,
    })
}

fn inspect_child_directory(
    parent: &BoundDirectory,
    path: &Path,
) -> Result<BoundDirectory, BackupError> {
    let named_parent = path_parent(path)?;
    if named_parent != parent.path {
        return Err(BackupError::Verification(
            "backup directory is not the expected direct child".to_owned(),
        ));
    }
    let snapshot = plain_path_snapshot(path, PlainPathKind::Directory)?;
    if snapshot.identity.volume != parent.volume {
        return Err(BackupError::Verification(
            "backup directory crossed a filesystem boundary".to_owned(),
        ));
    }
    let canonical = fs::canonicalize(path).map_err(|source| BackupError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let expected = parent.canonical.join(
        path.file_name()
            .ok_or_else(|| BackupError::Verification("backup child has no name".to_owned()))?,
    );
    if canonical != expected || !canonical.starts_with(&parent.canonical) {
        return Err(BackupError::Verification(
            "backup directory escapes its trusted parent".to_owned(),
        ));
    }
    let confirmed = plain_path_snapshot(path, PlainPathKind::Directory)?;
    if confirmed.identity != snapshot.identity {
        return Err(BackupError::Verification(
            "backup directory identity changed during inspection".to_owned(),
        ));
    }
    Ok(BoundDirectory {
        path: path.to_path_buf(),
        canonical,
        identity: snapshot.identity,
        volume: parent.volume,
    })
}

#[derive(Clone, Copy)]
enum PlainPathKind {
    Directory,
    File,
}

fn plain_path_snapshot(path: &Path, kind: PlainPathKind) -> Result<PlainPathSnapshot, BackupError> {
    let named = fs::symlink_metadata(path).map_err(|source| BackupError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let correct_named_type = match kind {
        PlainPathKind::Directory => named.is_dir(),
        PlainPathKind::File => named.is_file(),
    };
    if named.file_type().is_symlink() || is_reparse_point(&named) || !correct_named_type {
        return Err(BackupError::Verification(format!(
            "{} is not a plain {}",
            path.display(),
            match kind {
                PlainPathKind::Directory => "directory",
                PlainPathKind::File => "file",
            }
        )));
    }
    let file = open_path_without_following(path, kind).map_err(|source| BackupError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let metadata = file.metadata().map_err(|source| BackupError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let correct_opened_type = match kind {
        PlainPathKind::Directory => metadata.is_dir(),
        PlainPathKind::File => metadata.is_file(),
    };
    if is_reparse_point(&metadata) || !correct_opened_type {
        return Err(BackupError::Verification(
            "backup path changed type while it was opened".to_owned(),
        ));
    }
    let snapshot = file_snapshot(&file, &metadata).map_err(|source| BackupError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if matches!(kind, PlainPathKind::File) && snapshot.links != 1 {
        return Err(BackupError::Verification(
            "backup file has an untrusted hard-link count".to_owned(),
        ));
    }
    Ok(snapshot)
}

#[cfg(unix)]
fn open_path_without_following(path: &Path, kind: PlainPathKind) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    let directory_flag = if matches!(kind, PlainPathKind::Directory) {
        libc::O_DIRECTORY
    } else {
        0
    };
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | directory_flag)
        .open(path)
}

#[cfg(windows)]
fn open_path_without_following(path: &Path, kind: PlainPathKind) -> std::io::Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ,
        FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    let (access_mode, directory_flag) = if matches!(kind, PlainPathKind::Directory) {
        (FILE_READ_ATTRIBUTES, FILE_FLAG_BACKUP_SEMANTICS)
    } else {
        (FILE_GENERIC_READ, 0)
    };
    OpenOptions::new()
        .access_mode(access_mode)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | directory_flag)
        .open(path)
}

#[cfg(not(any(unix, windows)))]
fn open_path_without_following(path: &Path, _kind: PlainPathKind) -> std::io::Result<File> {
    File::open(path)
}

#[cfg(unix)]
fn file_snapshot(_file: &File, metadata: &fs::Metadata) -> std::io::Result<PlainPathSnapshot> {
    use std::os::unix::fs::MetadataExt;

    Ok(PlainPathSnapshot {
        identity: FileIdentity {
            volume: metadata.dev(),
            file: metadata.ino(),
        },
        links: metadata.nlink(),
        size: metadata.len(),
        modified: metadata.modified()?,
    })
}

#[cfg(windows)]
fn file_snapshot(file: &File, metadata: &fs::Metadata) -> std::io::Result<PlainPathSnapshot> {
    use std::mem::MaybeUninit;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    let succeeded =
        unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) };
    if succeeded == 0 {
        return Err(std::io::Error::last_os_error());
    }
    let information = unsafe { information.assume_init() };
    Ok(PlainPathSnapshot {
        identity: FileIdentity {
            volume: u64::from(information.dwVolumeSerialNumber),
            file: (u64::from(information.nFileIndexHigh) << 32)
                | u64::from(information.nFileIndexLow),
        },
        links: u64::from(information.nNumberOfLinks),
        size: metadata.len(),
        modified: metadata.modified()?,
    })
}

#[cfg(not(any(unix, windows)))]
fn file_snapshot(_file: &File, metadata: &fs::Metadata) -> std::io::Result<PlainPathSnapshot> {
    Ok(PlainPathSnapshot {
        identity: FileIdentity { volume: 0, file: 0 },
        links: 1,
        size: metadata.len(),
        modified: metadata.modified()?,
    })
}

#[cfg(windows)]
fn is_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

#[derive(Debug)]
struct StagedBackup {
    boundary: BackupStoreBoundary,
    directory: PathBuf,
    directory_identity: FileIdentity,
    database: PathBuf,
    database_identity: FileIdentity,
    database_reservation: File,
    manifest: PathBuf,
    final_directory: PathBuf,
}

fn prepare_staging_directory(
    paths: &StatePaths,
    storage_instance_id: StorageInstanceId,
    backup_id: Uuid,
    cancellation: &CancellationToken,
    deadline: Instant,
    fault: BackupFault,
) -> Result<StagedBackup, BackupError> {
    ensure_active(cancellation, Some(deadline))?;
    let boundary = BackupStoreBoundary::open_or_create(paths, storage_instance_id, fault)?;
    boundary.revalidate()?;
    let directory = boundary.store.path.join(format!(".{backup_id}.staging"));
    create_private_directory(&directory)?;
    let staged_directory = inspect_child_directory(&boundary.store, &directory)?;
    sync_directory(&directory)?;
    sync_directory(&boundary.store.path)?;
    let database = directory.join(BACKUP_FILE_NAME);
    let database_reservation = private_create_new(&database)?;
    let database_identity = plain_path_snapshot(&database, PlainPathKind::File)?.identity;
    Ok(StagedBackup {
        boundary,
        database,
        database_identity,
        database_reservation,
        manifest: directory.join(MANIFEST_FILE_NAME),
        final_directory: staged_directory
            .path
            .parent()
            .expect("staging directory has a store parent")
            .join(backup_id.to_string()),
        directory_identity: staged_directory.identity,
        directory,
    })
}

#[allow(clippy::too_many_arguments)]
fn finish_and_publish_backup(
    paths: &StatePaths,
    storage_instance_id: StorageInstanceId,
    trigger: BackupTrigger,
    app_version: &str,
    backup_id: Uuid,
    created_at: &str,
    staging: StagedBackup,
    cancellation: &CancellationToken,
    deadline: Instant,
    fault: BackupFault,
) -> Result<VerifiedBackup, BackupError> {
    ensure_active(cancellation, Some(deadline))?;
    let named_database = plain_path_snapshot(&staging.database, PlainPathKind::File)?;
    if named_database.identity != staging.database_identity {
        return Err(BackupError::Verification(
            "staged database identity changed during online backup".to_owned(),
        ));
    }
    set_private_open_file_permissions(&staging.database_reservation, &staging.database)?;
    drop(staging.database_reservation);
    fault.inject(BackupFault::BeforeQuickCheck)?;
    let integrity = quick_check_database(&staging.database, Some(cancellation), Some(deadline))?;
    if integrity != "ok" {
        return Err(BackupError::QuickCheck {
            path: staging.database,
            detail: integrity,
        });
    }
    let schema_version =
        backup_schema_version(&staging.database, Some(cancellation), Some(deadline))?;
    fault.inject(BackupFault::BeforeHash)?;
    let (database_size_bytes, sha256) =
        database_size_and_hash(&staging.database, Some(cancellation), Some(deadline))?;
    fault.inject(BackupFault::BeforeDatabaseSync)?;
    sync_file(&staging.database)?;
    let manifest = BackupManifest {
        backup_id,
        storage_instance_id,
        created_at: created_at.to_owned(),
        state_kind: paths.state_kind,
        trigger,
        app_version: app_version.to_owned(),
        schema_version,
        database_size_bytes,
        sha256,
    };
    fault.inject(BackupFault::BeforeManifestWrite)?;
    write_manifest(&staging.manifest, &manifest)?;
    fault.inject(BackupFault::BeforeStagingSync)?;
    sync_directory(&staging.directory)?;
    ensure_active(cancellation, Some(deadline))?;
    staging.boundary.revalidate()?;
    let staged_directory = inspect_child_directory(&staging.boundary.store, &staging.directory)?;
    if staged_directory.identity != staging.directory_identity {
        return Err(BackupError::Verification(
            "backup staging directory identity changed before publication".to_owned(),
        ));
    }
    match fs::symlink_metadata(&staging.final_directory) {
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => {
            return Err(BackupError::Verification(
                "backup generation destination already exists".to_owned(),
            ));
        }
        Err(source) => {
            return Err(BackupError::Io {
                path: staging.final_directory.clone(),
                source,
            });
        }
    }
    fault.inject(BackupFault::BeforePublish)?;
    fs::rename(&staging.directory, &staging.final_directory).map_err(|source| BackupError::Io {
        path: staging.final_directory.clone(),
        source,
    })?;
    staging.boundary.revalidate()?;
    let published_directory =
        inspect_child_directory(&staging.boundary.store, &staging.final_directory)?;
    if published_directory.identity != staging.directory_identity {
        return Err(BackupError::Verification(
            "published backup directory identity differs from its stage".to_owned(),
        ));
    }
    fault.inject(BackupFault::BeforeParentSync)?;
    sync_directory(&staging.boundary.store.path)?;

    fault.inject(BackupFault::BeforeReloadVerification)?;
    let verified = verify_generation(
        &staging.boundary,
        &staging.final_directory,
        backup_id,
        storage_instance_id,
        Some(cancellation),
        Some(deadline),
    )?;
    apply_retention(
        paths,
        storage_instance_id,
        backup_id,
        cancellation,
        deadline,
    )?;
    Ok(verified)
}

fn inventory_blocking(
    paths: &StatePaths,
    storage_instance_id: StorageInstanceId,
    cancellation: Option<&CancellationToken>,
    deadline: Option<Instant>,
) -> Result<BackupInventory, BackupError> {
    ensure_optional_active(cancellation, deadline)?;
    let Some(boundary) = BackupStoreBoundary::open_existing(paths, storage_instance_id)? else {
        return Ok(BackupInventory::default());
    };
    boundary.revalidate()?;
    let store_directory = boundary.store.path.clone();
    let entries = fs::read_dir(&store_directory).map_err(|source| BackupError::Io {
        path: store_directory.clone(),
        source,
    })?;
    let mut inventory = BackupInventory::default();
    for entry in entries {
        ensure_optional_active(cancellation, deadline)?;
        let entry = entry.map_err(|source| BackupError::Io {
            path: store_directory.clone(),
            source,
        })?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') && name.ends_with(".staging") {
            continue;
        }
        let backup_id = match Uuid::parse_str(&name) {
            Ok(value) => value,
            Err(_) => {
                inventory.issues.push(BackupInventoryIssue {
                    entry_name: name,
                    message: "backup generation name is not a UUID".to_owned(),
                });
                continue;
            }
        };
        let file_type = entry.file_type().map_err(|source| BackupError::Io {
            path: entry.path(),
            source,
        })?;
        if !file_type.is_dir() || file_type.is_symlink() {
            inventory.issues.push(BackupInventoryIssue {
                entry_name: name,
                message: "backup generation is not a plain directory".to_owned(),
            });
            continue;
        }
        match verify_generation(
            &boundary,
            &entry.path(),
            backup_id,
            storage_instance_id,
            cancellation,
            deadline,
        ) {
            Ok(backup) => inventory.verified.push(backup),
            Err(error) => inventory.issues.push(BackupInventoryIssue {
                entry_name: name,
                message: error.to_string(),
            }),
        }
    }
    boundary.revalidate()?;
    inventory.verified.sort_by(|left, right| {
        left.identity
            .publication_time
            .cmp(&right.identity.publication_time)
            .then_with(|| left.manifest.backup_id.cmp(&right.manifest.backup_id))
    });
    Ok(inventory)
}

fn verify_generation(
    boundary: &BackupStoreBoundary,
    directory: &Path,
    expected_backup_id: Uuid,
    expected_storage_instance_id: StorageInstanceId,
    cancellation: Option<&CancellationToken>,
    deadline: Option<Instant>,
) -> Result<VerifiedBackup, BackupError> {
    ensure_optional_active(cancellation, deadline)?;
    boundary.revalidate()?;
    let generation = inspect_child_directory(&boundary.store, directory)?;
    let directory_snapshot = plain_path_snapshot(directory, PlainPathKind::Directory)?;
    ensure_exact_generation_entries(directory)?;
    let manifest_path = directory.join(MANIFEST_FILE_NAME);
    let database = directory.join(BACKUP_FILE_NAME);
    let manifest_snapshot = inspect_plain_child_file(&generation, &manifest_path)?;
    let database_snapshot = inspect_plain_child_file(&generation, &database)?;
    let manifest = serde_json::from_slice::<BackupManifest>(&read_manifest_bytes(&manifest_path)?)
        .map_err(|source| BackupError::ManifestDecode {
            path: manifest_path.clone(),
            source,
        })?;
    if manifest.backup_id != expected_backup_id
        || manifest.storage_instance_id != expected_storage_instance_id
        || manifest.state_kind != boundary.state_kind_value
    {
        return Err(BackupError::Verification(
            "manifest identity does not match its trusted inventory location".to_owned(),
        ));
    }
    let created_at = OffsetDateTime::parse(&manifest.created_at, &Rfc3339).map_err(|_| {
        BackupError::Verification("manifest creation time is not RFC 3339 UTC".to_owned())
    })?;
    if created_at.offset() != UtcOffset::UTC {
        return Err(BackupError::Verification(
            "manifest creation time is not RFC 3339 UTC".to_owned(),
        ));
    }
    let canonical_created_at = created_at.format(&Rfc3339).map_err(|error| {
        BackupError::Verification(format!(
            "manifest UTC time could not be canonicalized: {error}"
        ))
    })?;
    if canonical_created_at != manifest.created_at {
        return Err(BackupError::Verification(
            "manifest creation time is not canonical RFC 3339 UTC".to_owned(),
        ));
    }
    let publication_time = OffsetDateTime::from(directory_snapshot.modified);
    let time_skew = created_at
        .unix_timestamp_nanos()
        .abs_diff(publication_time.unix_timestamp_nanos());
    if time_skew > MAX_PUBLICATION_TIME_SKEW.as_nanos() {
        return Err(BackupError::Verification(
            "manifest creation time does not match trusted publication time".to_owned(),
        ));
    }
    if manifest.sha256.len() != 64
        || !manifest
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(BackupError::Verification(
            "manifest checksum is not lowercase SHA-256".to_owned(),
        ));
    }
    let (size, sha256) = database_size_and_hash(&database, cancellation, deadline)?;
    if size != manifest.database_size_bytes || sha256 != manifest.sha256 {
        return Err(BackupError::Verification(
            "manifest size or checksum does not match database".to_owned(),
        ));
    }
    let schema_version = backup_schema_version(&database, cancellation, deadline)?;
    if schema_version != manifest.schema_version {
        return Err(BackupError::Verification(
            "manifest schema version does not match the backup ledger".to_owned(),
        ));
    }
    let integrity = quick_check_database(&database, cancellation, deadline)?;
    if integrity != "ok" {
        return Err(BackupError::QuickCheck {
            path: database,
            detail: integrity,
        });
    }
    boundary.revalidate()?;
    ensure_exact_generation_entries(directory)?;
    let confirmed_generation = inspect_child_directory(&boundary.store, directory)?;
    let confirmed_directory = plain_path_snapshot(directory, PlainPathKind::Directory)?;
    let confirmed_manifest = inspect_plain_child_file(&confirmed_generation, &manifest_path)?;
    let confirmed_database = inspect_plain_child_file(&confirmed_generation, &database)?;
    if confirmed_generation.identity != generation.identity
        || confirmed_directory != directory_snapshot
        || confirmed_manifest != manifest_snapshot
        || confirmed_database != database_snapshot
    {
        return Err(BackupError::Verification(
            "backup generation changed during verification".to_owned(),
        ));
    }
    Ok(VerifiedBackup {
        directory: directory.to_path_buf(),
        database,
        manifest_path,
        manifest,
        identity: VerifiedGenerationIdentity {
            directory: generation.identity,
            database: database_snapshot.identity,
            manifest: manifest_snapshot.identity,
            publication_time: directory_snapshot.modified,
        },
    })
}

fn ensure_exact_generation_entries(directory: &Path) -> Result<(), BackupError> {
    let mut database = false;
    let mut manifest = false;
    let mut count = 0_usize;
    for entry in fs::read_dir(directory).map_err(|source| BackupError::Io {
        path: directory.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| BackupError::Io {
            path: directory.to_path_buf(),
            source,
        })?;
        count += 1;
        if entry.file_name() == OsStr::new(BACKUP_FILE_NAME) {
            database = true;
        } else if entry.file_name() == OsStr::new(MANIFEST_FILE_NAME) {
            manifest = true;
        } else {
            return Err(BackupError::Verification(
                "backup generation contains an unexpected entry".to_owned(),
            ));
        }
    }
    if count != 2 || !database || !manifest {
        return Err(BackupError::Verification(
            "backup generation must contain exactly state.sqlite and manifest.json".to_owned(),
        ));
    }
    Ok(())
}

fn inspect_plain_child_file(
    parent: &BoundDirectory,
    path: &Path,
) -> Result<PlainPathSnapshot, BackupError> {
    if path_parent(path)? != parent.path {
        return Err(BackupError::Verification(
            "backup file is not the expected direct child".to_owned(),
        ));
    }
    let snapshot = plain_path_snapshot(path, PlainPathKind::File)?;
    if snapshot.identity.volume != parent.volume {
        return Err(BackupError::Verification(
            "backup file crossed a filesystem boundary".to_owned(),
        ));
    }
    let canonical = fs::canonicalize(path).map_err(|source| BackupError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let expected = parent.canonical.join(
        path.file_name()
            .ok_or_else(|| BackupError::Verification("backup file has no name".to_owned()))?,
    );
    if canonical != expected {
        return Err(BackupError::Verification(
            "backup file escapes its generation directory".to_owned(),
        ));
    }
    let confirmed = plain_path_snapshot(path, PlainPathKind::File)?;
    if confirmed != snapshot {
        return Err(BackupError::Verification(
            "backup file identity changed during inspection".to_owned(),
        ));
    }
    Ok(snapshot)
}

fn apply_retention(
    paths: &StatePaths,
    storage_instance_id: StorageInstanceId,
    published_backup_id: Uuid,
    cancellation: &CancellationToken,
    deadline: Instant,
) -> Result<(), BackupError> {
    let inventory = inventory_blocking(
        paths,
        storage_instance_id,
        Some(cancellation),
        Some(deadline),
    )?;
    for issue in &inventory.issues {
        tracing::warn!(
            entry = %issue.entry_name,
            detail = %issue.message,
            "ignored untrusted backup inventory entry"
        );
    }
    let removal_count = inventory
        .verified
        .len()
        .saturating_sub(RETAINED_GENERATIONS);
    for backup in inventory.verified.iter().take(removal_count) {
        ensure_active(cancellation, Some(deadline))?;
        if backup.manifest.backup_id == published_backup_id {
            continue;
        }
        if let Err(error) = delete_verified_generation_inner(
            paths,
            storage_instance_id,
            backup,
            Some(cancellation),
            Some(deadline),
            || {},
        ) {
            tracing::warn!(
                backup_id = %backup.manifest.backup_id,
                detail = %error,
                "failed to delete an expired verified backup"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
fn delete_verified_generation_with_hook<F>(
    paths: &StatePaths,
    storage_instance_id: StorageInstanceId,
    expected: &VerifiedBackup,
    hook: F,
) -> Result<(), BackupError>
where
    F: FnOnce(),
{
    delete_verified_generation_inner(paths, storage_instance_id, expected, None, None, hook)
}

fn delete_verified_generation_inner<F>(
    paths: &StatePaths,
    storage_instance_id: StorageInstanceId,
    expected: &VerifiedBackup,
    cancellation: Option<&CancellationToken>,
    deadline: Option<Instant>,
    hook: F,
) -> Result<(), BackupError>
where
    F: FnOnce(),
{
    ensure_optional_active(cancellation, deadline)?;
    let boundary = BackupStoreBoundary::open_existing(paths, storage_instance_id)?
        .ok_or_else(|| BackupError::Verification("backup store disappeared".to_owned()))?;
    boundary.revalidate()?;
    let current = verify_generation(
        &boundary,
        &expected.directory,
        expected.manifest.backup_id,
        storage_instance_id,
        cancellation,
        deadline,
    )?;
    ensure_same_generation_identity(expected, &current)?;
    hook();
    ensure_optional_active(cancellation, deadline)?;
    boundary.revalidate()?;
    let current = verify_generation(
        &boundary,
        &expected.directory,
        expected.manifest.backup_id,
        storage_instance_id,
        cancellation,
        deadline,
    )?;
    ensure_same_generation_identity(expected, &current)?;

    remove_verified_file(&current.manifest_path, current.identity.manifest)?;
    boundary.revalidate()?;
    remove_verified_file(&current.database, current.identity.database)?;
    boundary.revalidate()?;
    let directory = inspect_child_directory(&boundary.store, &current.directory)?;
    if directory.identity != current.identity.directory {
        return Err(BackupError::Verification(
            "backup generation directory changed before deletion".to_owned(),
        ));
    }
    fs::remove_dir(&current.directory).map_err(|source| BackupError::Io {
        path: current.directory.clone(),
        source,
    })?;
    sync_directory(&boundary.store.path)
}

fn ensure_same_generation_identity(
    expected: &VerifiedBackup,
    current: &VerifiedBackup,
) -> Result<(), BackupError> {
    if expected.identity != current.identity {
        return Err(BackupError::Verification(
            "backup generation identity changed before retention".to_owned(),
        ));
    }
    Ok(())
}

fn remove_verified_file(path: &Path, expected: FileIdentity) -> Result<(), BackupError> {
    let snapshot = plain_path_snapshot(path, PlainPathKind::File)?;
    if snapshot.identity != expected {
        return Err(BackupError::Verification(
            "backup file identity changed before deletion".to_owned(),
        ));
    }
    let confirmed = plain_path_snapshot(path, PlainPathKind::File)?;
    if confirmed.identity != expected {
        return Err(BackupError::Verification(
            "backup file identity changed during deletion".to_owned(),
        ));
    }
    fs::remove_file(path).map_err(|source| BackupError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn backup_schema_version(
    database: &Path,
    cancellation: Option<&CancellationToken>,
    deadline: Option<Instant>,
) -> Result<i64, BackupError> {
    ensure_optional_active(cancellation, deadline)?;
    let connection = open_read_only_database(database)?;
    if cancellation.is_some() || deadline.is_some() {
        let cancellation = cancellation.cloned();
        connection
            .progress_handler(
                SQLITE_PROGRESS_OPS,
                Some(move || {
                    cancellation
                        .as_ref()
                        .is_some_and(CancellationToken::is_cancelled)
                        || deadline.is_some_and(|deadline| Instant::now() >= deadline)
                }),
            )
            .map_err(|source| BackupError::Persistence(PersistenceError::Sql(source)))?;
    }
    let mut statement = connection
        .prepare(
            "SELECT migration_id, name \
             FROM effect_sql_migrations \
             ORDER BY migration_id ASC \
             LIMIT ?1",
        )
        .map_err(|source| BackupError::Persistence(PersistenceError::Sql(source)))?;
    let row_limit = i64::try_from(MIGRATIONS.len() + 1)
        .map_err(|_| BackupError::Verification("migration count overflowed i64".to_owned()))?;
    let recorded = statement
        .query_map([row_limit], |row| {
            Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?))
        })
        .and_then(Iterator::collect::<rusqlite::Result<Vec<_>>>)
        .map_err(|source| {
            if source.sqlite_error_code() == Some(ErrorCode::OperationInterrupted) {
                ensure_optional_active(cancellation, deadline)
                    .err()
                    .unwrap_or_else(|| BackupError::Persistence(PersistenceError::Sql(source)))
            } else {
                BackupError::Persistence(PersistenceError::Sql(source))
            }
        })?;
    ensure_optional_active(cancellation, deadline)?;
    if recorded.is_empty()
        || recorded.len() > MIGRATIONS.len()
        || recorded
            .iter()
            .zip(MIGRATIONS)
            .any(|((id, name), expected)| *id != expected.id || name != expected.name)
    {
        return Err(BackupError::Verification(
            "backup migration ledger is not an exact prefix of this binary".to_owned(),
        ));
    }
    Ok(i64::from(
        recorded
            .last()
            .expect("non-empty recognized migration ledger")
            .0,
    ))
}

fn quick_check_database(
    database: &Path,
    cancellation: Option<&CancellationToken>,
    deadline: Option<Instant>,
) -> Result<String, BackupError> {
    if let Some(cancellation) = cancellation {
        ensure_active(cancellation, deadline)?;
    }
    let connection = open_read_only_database(database)?;
    if let (Some(cancellation), Some(deadline)) = (cancellation, deadline) {
        let cancellation = cancellation.clone();
        connection
            .progress_handler(
                SQLITE_PROGRESS_OPS,
                Some(move || cancellation.is_cancelled() || Instant::now() >= deadline),
            )
            .map_err(|source| BackupError::Persistence(PersistenceError::Sql(source)))?;
    }
    connection
        .query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
        .map_err(|source| {
            if source.sqlite_error_code() == Some(ErrorCode::OperationInterrupted) {
                cancellation.map_or_else(
                    || BackupError::QuickCheck {
                        path: database.to_path_buf(),
                        detail: source.to_string(),
                    },
                    |token| match ensure_active(token, deadline) {
                        Err(error) => error,
                        Ok(()) => BackupError::QuickCheck {
                            path: database.to_path_buf(),
                            detail: "SQLite quick_check was interrupted unexpectedly".to_owned(),
                        },
                    },
                )
            } else {
                BackupError::QuickCheck {
                    path: database.to_path_buf(),
                    detail: source.to_string(),
                }
            }
        })
}

fn open_read_only_database(database: &Path) -> Result<Connection, BackupError> {
    Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|source| {
        BackupError::Persistence(PersistenceError::Open {
            path: database.to_path_buf(),
            source,
        })
    })
}

fn database_size_and_hash(
    database: &Path,
    cancellation: Option<&CancellationToken>,
    deadline: Option<Instant>,
) -> Result<(u64, String), BackupError> {
    ensure_plain_file(database)?;
    let mut file = File::open(database).map_err(|source| BackupError::Io {
        path: database.to_path_buf(),
        source,
    })?;
    let mut digest = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        if let Some(cancellation) = cancellation {
            ensure_active(cancellation, deadline)?;
        }
        let read = file.read(&mut buffer).map_err(|source| BackupError::Io {
            path: database.to_path_buf(),
            source,
        })?;
        if read == 0 {
            break;
        }
        size = size
            .checked_add(read as u64)
            .ok_or_else(|| BackupError::Verification("backup size overflowed u64".to_owned()))?;
        digest.update(&buffer[..read]);
    }
    let sha256 = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok((size, sha256))
}

fn read_manifest_bytes(path: &Path) -> Result<Vec<u8>, BackupError> {
    ensure_plain_file(path)?;
    let metadata = fs::metadata(path).map_err(|source| BackupError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.len() > MAX_MANIFEST_BYTES {
        return Err(BackupError::Verification(
            "manifest exceeds the bounded inventory size".to_owned(),
        ));
    }
    fs::read(path).map_err(|source| BackupError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn write_manifest(path: &Path, manifest: &BackupManifest) -> Result<(), BackupError> {
    let mut contents = serde_json::to_vec_pretty(manifest).map_err(BackupError::ManifestEncode)?;
    contents.push(b'\n');
    let mut file = private_create_new(path)?;
    file.write_all(&contents)
        .and_then(|()| file.sync_all())
        .map_err(|source| BackupError::Io {
            path: path.to_path_buf(),
            source,
        })
}

fn sync_file(path: &Path) -> Result<(), BackupError> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .and_then(|file| file.sync_all())
        .map_err(|source| BackupError::Io {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<(), BackupError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| BackupError::Io {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> Result<(), BackupError> {
    Ok(())
}

fn ensure_plain_file(path: &Path) -> Result<(), BackupError> {
    plain_path_snapshot(path, PlainPathKind::File).map(|_| ())
}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<(), BackupError> {
    use std::os::unix::fs::PermissionsExt;
    let directory =
        open_path_without_following(path, PlainPathKind::Directory).map_err(|source| {
            BackupError::Io {
                path: path.to_path_buf(),
                source,
            }
        })?;
    directory
        .set_permissions(fs::Permissions::from_mode(0o700))
        .map_err(|source| BackupError::Io {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<(), BackupError> {
    Ok(())
}

fn create_private_directory(path: &Path) -> Result<(), BackupError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(path).map_err(|source| BackupError::Io {
            path: path.to_path_buf(),
            source,
        })
    }
    #[cfg(not(unix))]
    {
        fs::create_dir(path).map_err(|source| BackupError::Io {
            path: path.to_path_buf(),
            source,
        })
    }
}

fn open_private_lock_file(path: &Path) -> Result<File, BackupError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)
            .map_err(|source| BackupError::Io {
                path: path.to_path_buf(),
                source,
            })
    }
    #[cfg(not(unix))]
    {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|source| BackupError::Io {
                path: path.to_path_buf(),
                source,
            })
    }
}

fn private_create_new(path: &Path) -> Result<File, BackupError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(|source| BackupError::Io {
                path: path.to_path_buf(),
                source,
            })
    }
    #[cfg(not(unix))]
    {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|source| BackupError::Io {
                path: path.to_path_buf(),
                source,
            })
    }
}

#[cfg(unix)]
fn set_private_open_file_permissions(file: &File, path: &Path) -> Result<(), BackupError> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|source| BackupError::Io {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(not(unix))]
fn set_private_open_file_permissions(_file: &File, _path: &Path) -> Result<(), BackupError> {
    Ok(())
}

fn ensure_active(
    cancellation: &CancellationToken,
    deadline: Option<Instant>,
) -> Result<(), BackupError> {
    if cancellation.is_cancelled() {
        Err(BackupError::Cancelled)
    } else if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        Err(BackupError::DeadlineElapsed)
    } else {
        Ok(())
    }
}

fn ensure_optional_active(
    cancellation: Option<&CancellationToken>,
    deadline: Option<Instant>,
) -> Result<(), BackupError> {
    if let Some(cancellation) = cancellation {
        ensure_active(cancellation, deadline)
    } else if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        Err(BackupError::DeadlineElapsed)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ServerConfig, persistence::run_migrations};
    use tempfile::TempDir;

    fn schema_version(database: &Path) -> i64 {
        Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("schema reader")
            .query_row(
                "SELECT MAX(migration_id) FROM effect_sql_migrations",
                [],
                |row| row.get(0),
            )
            .expect("schema version")
    }

    #[test]
    fn staged_database_sync_reopens_the_file_with_write_access() {
        let root = TempDir::new().expect("sync fixture root");
        let path = root.path().join(BACKUP_FILE_NAME);
        let mut file = private_create_new(&path).expect("sync fixture should create");
        file.write_all(b"durable backup fixture")
            .expect("sync fixture should write");
        drop(file);

        sync_file(&path).expect("staged database should flush on every supported platform");
    }

    #[tokio::test]
    async fn restore_failure_after_preservation_keeps_the_journal_and_recoverable_live_files() {
        let root = TempDir::new().expect("recovery seam root");
        let mut config = ServerConfig::new(root.path());
        let resolved = resolve_data_root(config.data_root_request.clone()).expect("resolve root");
        config.base_dir.clone_from(&resolved.effective);
        config.resolved_data_root = Some(resolved.clone());
        let paths = StatePaths::from_config(&config);
        fs::create_dir_all(&paths.state_dir).expect("state directory");
        let mut setup = Connection::open(&paths.database).expect("source database");
        run_migrations(&mut setup, None).expect("source schema");
        setup
            .execute_batch(
                "INSERT INTO projection_projects (
                   project_id, title, workspace_root, default_model_selection_json,
                   scripts_json, created_at, updated_at, deleted_at
                 ) VALUES ('recovery-seam', 'Preserved', '/tmp/recovery-seam', NULL, '{}',
                           '2026-08-10T00:00:00Z', '2026-08-10T00:00:00Z', NULL)",
            )
            .expect("source row");
        drop(setup);
        let storage_instance_id = StorageInstanceId::from_uuid(Uuid::new_v4());
        fs::write(&paths.environment_id, format!("{storage_instance_id}\n"))
            .expect("source marker");
        let database = Database::open_existing(&paths.database)
            .await
            .expect("source worker");
        let prepared = PreparedStore {
            database: database.clone(),
            storage_instance_id,
            classification: crate::persistence::StoreClassification::Existing,
            paths: paths.clone(),
        };
        let backup = create_verified_backup(
            &database,
            &prepared,
            BackupTrigger::PreUpdate,
            "recovery-seam-test",
        )
        .await
        .expect("verified backup");
        drop(prepared);
        database.close().await;

        let error = restore_backup_with_fault(
            &resolved,
            backup.manifest.backup_id,
            RecoveryFault::AfterPreserve,
        )
        .await
        .expect_err("injected post-preservation failure");

        assert!(
            matches!(&error, RecoveryError::Backup(BackupError::Verification(_))),
            "unexpected recovery error: {error:?}"
        );
        assert!(paths.recovery_journal().is_file());
        assert!(!paths.database.exists());
        assert!(!paths.environment_id.exists());
        let recovery_entries = fs::read_dir(paths.recovery_dir())
            .expect("recovery directory")
            .map(|entry| entry.expect("recovery entry").path())
            .collect::<Vec<_>>();
        assert_eq!(recovery_entries.len(), 1);
        assert!(recovery_entries[0].join("state.sqlite").is_file());
        assert!(recovery_entries[0].join("environment-id").is_file());
        let startup = crate::persistence::prepare_store(&config)
            .await
            .expect_err("journal must block startup after interrupted restore");
        assert!(matches!(
            startup,
            crate::persistence::StoreStartupError::RecoveryIncomplete { .. }
        ));
    }

    #[tokio::test]
    async fn every_post_backup_failure_seam_preserves_source_bytes_and_schema() {
        let root = TempDir::new().expect("backup seam root");
        let paths = StatePaths::from_config(&ServerConfig::new(root.path()));
        fs::create_dir_all(&paths.state_dir).expect("state directory");
        let mut setup = Connection::open(&paths.database).expect("source database");
        run_migrations(&mut setup, Some(38)).expect("source schema");
        setup
            .execute_batch(
                "INSERT INTO projection_projects (
                   project_id, title, workspace_root, default_model_selection_json,
                   scripts_json, created_at, updated_at, deleted_at
                 ) VALUES ('protected', 'Protected', '/tmp/protected', NULL, '{}',
                           '2026-08-09T00:00:00Z', '2026-08-09T00:00:00Z', NULL)",
            )
            .expect("source row");
        drop(setup);
        let source_bytes = fs::read(&paths.database).expect("source bytes");
        let source_schema = schema_version(&paths.database);
        let database = Database::open_existing_read_only(&paths.database)
            .await
            .expect("read-only source");
        let storage_instance_id = StorageInstanceId::from_uuid(Uuid::new_v4());
        let cancellation = CancellationToken::new();

        for fault in [
            BackupFault::BeforeQuickCheck,
            BackupFault::BeforeHash,
            BackupFault::BeforeDatabaseSync,
            BackupFault::BeforeManifestWrite,
            BackupFault::BeforeStagingSync,
            BackupFault::BeforePublish,
            BackupFault::BeforeParentSync,
            BackupFault::BeforeReloadVerification,
        ] {
            let backup_id = Uuid::new_v4();
            let deadline = Instant::now() + Duration::from_secs(5);
            let staging = prepare_staging_directory(
                &paths,
                storage_instance_id,
                backup_id,
                &cancellation,
                deadline,
                BackupFault::None,
            )
            .expect("staging directory");
            database
                .backup_to_cancellable(&staging.database, cancellation.clone(), deadline)
                .await
                .expect("online backup completes before injected seam");
            let error = finish_and_publish_backup(
                &paths,
                storage_instance_id,
                BackupTrigger::PreMigration,
                "seam-test",
                backup_id,
                "2026-08-09T00:00:00Z",
                staging,
                &cancellation,
                deadline,
                fault,
            )
            .expect_err("seam must fail");
            assert!(
                matches!(error, BackupError::Verification(_)),
                "typed seam failure: {error}"
            );
            assert_eq!(
                fs::read(&paths.database).expect("source remains"),
                source_bytes,
                "source bytes changed at {fault:?}"
            );
            assert_eq!(
                schema_version(&paths.database),
                source_schema,
                "source schema changed at {fault:?}"
            );
            let store = paths.backup_store_dir(storage_instance_id);
            let _ = fs::remove_dir_all(store.join(format!(".{backup_id}.staging")));
            let _ = fs::remove_dir_all(store.join(backup_id.to_string()));
        }
        database.close().await;
    }

    #[tokio::test]
    async fn cancellation_before_the_online_backup_worker_runs_creates_no_destination() {
        let root = TempDir::new().expect("cancelled backup root");
        let source = root.path().join("source.sqlite");
        let mut setup = Connection::open(&source).expect("source database");
        run_migrations(&mut setup, Some(38)).expect("source schema");
        drop(setup);
        let before = fs::read(&source).expect("source bytes");
        let database = Database::open_existing_read_only(&source)
            .await
            .expect("read-only source");
        let destination = root.path().join("cancelled.sqlite");
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let error = database
            .backup_to_cancellable(
                &destination,
                cancellation,
                Instant::now() + Duration::from_secs(1),
            )
            .await
            .expect_err("cancelled backup");

        assert!(matches!(error, PersistenceError::BackupStopped(_)));
        assert!(!destination.exists());
        assert_eq!(fs::read(&source).expect("source remains"), before);
        database.close().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn published_backup_files_and_directories_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let root = TempDir::new().expect("permission backup root");
        let paths = StatePaths::from_config(&ServerConfig::new(root.path()));
        fs::create_dir_all(&paths.state_dir).expect("state directory");
        let mut setup = Connection::open(&paths.database).expect("source database");
        run_migrations(&mut setup, None).expect("source schema");
        drop(setup);
        let database = Database::open_existing_read_only(&paths.database)
            .await
            .expect("read-only source");
        let storage_instance_id = StorageInstanceId::from_uuid(Uuid::new_v4());
        let prepared = PreparedStore {
            database: database.clone(),
            storage_instance_id,
            classification: crate::persistence::StoreClassification::Existing,
            paths: paths.clone(),
        };

        let backup = create_verified_backup(
            &database,
            &prepared,
            BackupTrigger::PreUpdate,
            "permission-test",
        )
        .await
        .expect("verified backup");

        for directory in [
            paths.backup_store_dir(storage_instance_id),
            backup.directory,
        ] {
            let mode = fs::metadata(directory)
                .expect("directory metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o700);
        }
        for file in [backup.database, backup.manifest_path] {
            let mode = fs::metadata(file)
                .expect("file metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
        drop(prepared);
        database.close().await;
    }

    #[tokio::test]
    async fn every_new_backup_ancestor_sync_failure_preserves_source_bytes_and_schema() {
        for fault in [
            BackupFault::BeforeBackupsDirectorySync,
            BackupFault::BeforeBackupsParentSync,
            BackupFault::BeforeStateKindDirectorySync,
            BackupFault::BeforeStateKindParentSync,
            BackupFault::BeforeStoreDirectorySync,
            BackupFault::BeforeStoreParentSync,
        ] {
            let root = TempDir::new().expect("ancestor sync root");
            let paths = StatePaths::from_config(&ServerConfig::new(root.path()));
            fs::create_dir_all(&paths.state_dir).expect("state directory");
            let mut setup = Connection::open(&paths.database).expect("source database");
            run_migrations(&mut setup, Some(38)).expect("source schema");
            drop(setup);
            let source_bytes = fs::read(&paths.database).expect("source bytes");
            let source_schema = schema_version(&paths.database);
            let storage_instance_id = StorageInstanceId::from_uuid(Uuid::new_v4());
            let cancellation = CancellationToken::new();

            let error = prepare_staging_directory(
                &paths,
                storage_instance_id,
                Uuid::new_v4(),
                &cancellation,
                Instant::now() + Duration::from_secs(5),
                fault,
            )
            .expect_err("ancestor durability seam must fail");

            assert!(matches!(error, BackupError::Verification(_)));
            assert_eq!(
                fs::read(&paths.database).expect("source remains"),
                source_bytes,
                "source bytes changed at {fault:?}"
            );
            assert_eq!(
                schema_version(&paths.database),
                source_schema,
                "source schema changed at {fault:?}"
            );
        }
    }

    #[tokio::test]
    async fn retention_race_never_deletes_a_foreign_replacement_or_extra() {
        let root = TempDir::new().expect("retention race root");
        let paths = StatePaths::from_config(&ServerConfig::new(root.path()));
        fs::create_dir_all(&paths.state_dir).expect("state directory");
        let mut setup = Connection::open(&paths.database).expect("source database");
        run_migrations(&mut setup, None).expect("source schema");
        drop(setup);
        let database = Database::open_existing_read_only(&paths.database)
            .await
            .expect("read-only source");
        let storage_instance_id = StorageInstanceId::from_uuid(Uuid::new_v4());
        let prepared = PreparedStore {
            database: database.clone(),
            storage_instance_id,
            classification: crate::persistence::StoreClassification::Existing,
            paths: paths.clone(),
        };
        let backup = create_verified_backup(
            &database,
            &prepared,
            BackupTrigger::PreUpdate,
            "retention-race-test",
        )
        .await
        .expect("verified backup");
        let foreign_manifest = b"foreign replacement";
        let foreign_extra = backup.directory.join("foreign-extra");

        let error =
            delete_verified_generation_with_hook(&paths, storage_instance_id, &backup, || {
                fs::remove_file(&backup.manifest_path).expect("remove owned manifest");
                fs::write(&backup.manifest_path, foreign_manifest).expect("foreign manifest");
                fs::write(&foreign_extra, b"foreign extra").expect("foreign extra");
            })
            .expect_err("identity replacement must stop deletion");

        assert!(matches!(error, BackupError::Verification(_)));
        assert_eq!(
            fs::read(&backup.manifest_path).expect("foreign manifest remains"),
            foreign_manifest
        );
        assert_eq!(
            fs::read(&foreign_extra).expect("foreign extra remains"),
            b"foreign extra"
        );
        assert!(backup.database.is_file(), "owned database is not deleted");
        drop(prepared);
        database.close().await;
    }
}
