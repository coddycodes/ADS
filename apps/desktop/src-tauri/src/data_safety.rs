use coddy_studio_server::persistence::{
    BackupTrigger, RecoveryAction, RecoveryResult, StoreInspection, StoreInspectionStatus,
};
use coddy_studio_server::{ResolvedDataRoot, persistence};
use serde::{Deserialize, Serialize};
use std::{process::Stdio, time::Duration};
use tokio::{io::AsyncReadExt, process::Command};
use uuid::Uuid;

use crate::backend::{
    BackendLaunchPlan, BackendLaunchTarget, BackendProjectDataOperation, BackendProjectDataTarget,
    BackendStartFailure, BackendSupervisor,
};

const WSL_PROGRAM: &str = "wsl.exe";
const WSL_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const WSL_OUTPUT_LIMIT: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
struct WslStorageInvocation {
    program: String,
    args: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WslStorageAction<'a> {
    Inspect,
    Restore { backup_id: &'a str },
    StartEmpty,
}

fn wsl_storage_invocation(
    distro: &str,
    binary: &str,
    root: &str,
    action: WslStorageAction<'_>,
) -> WslStorageInvocation {
    let mut args = vec![
        "--distribution".to_owned(),
        distro.to_owned(),
        "--".to_owned(),
        binary.to_owned(),
        "storage".to_owned(),
    ];
    match action {
        WslStorageAction::Inspect => args.push("inspect".to_owned()),
        WslStorageAction::Restore { backup_id } => {
            args.extend([
                "restore".to_owned(),
                "--backup-id".to_owned(),
                backup_id.to_owned(),
            ]);
        }
        WslStorageAction::StartEmpty => args.push("start-empty".to_owned()),
    }
    args.extend([
        "--base-dir".to_owned(),
        root.to_owned(),
        "--json".to_owned(),
    ]);
    WslStorageInvocation {
        program: WSL_PROGRAM.to_owned(),
        args,
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum ProjectDataError {
    #[error("the selected project-data environment or backup is no longer available")]
    InvalidSelection,
    #[error("project-data inspection failed: {0}")]
    Inspection(String),
    #[error("the selected backend could not be stopped: {0}")]
    Stop(String),
    #[error("project-data recovery failed: {0}")]
    Recovery(String),
    #[error("Coddy Studio's local server couldn't restart: {0}")]
    Restart(BackendStartFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectDataRecoveryCommit {
    action: RecoveryAction,
    preserved_directory: String,
    storage_instance_id: Option<String>,
}

impl From<RecoveryResult> for ProjectDataRecoveryCommit {
    fn from(result: RecoveryResult) -> Self {
        Self {
            action: result.action,
            preserved_directory: result.preserved_directory.to_string_lossy().into_owned(),
            storage_instance_id: result.storage_instance_id.map(|value| value.to_string()),
        }
    }
}

#[cfg(test)]
impl ProjectDataRecoveryCommit {
    fn preserved_for_test() -> Self {
        Self {
            action: RecoveryAction::Restore,
            preserved_directory: "preserved".to_owned(),
            storage_instance_id: None,
        }
    }
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProjectDataRecoveryOutcome {
    commit: ProjectDataRecoveryCommit,
    restart_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopProjectDataRecoveryResult {
    environment_id: String,
    action: &'static str,
    committed: bool,
    preserved_directory: String,
    storage_instance_id: Option<String>,
    restart_error: Option<String>,
}

#[cfg(test)]
async fn execute_recovery_workflow<Validate, Stop, Recover, Restart>(
    validate: Validate,
    stop: Stop,
    recover: Recover,
    restart: Restart,
) -> Result<ProjectDataRecoveryOutcome, ProjectDataError>
where
    Validate: FnOnce() -> Result<(), ProjectDataError>,
    Stop: FnOnce() -> Result<(), ProjectDataError>,
    Recover: FnOnce() -> Result<ProjectDataRecoveryCommit, ProjectDataError>,
    Restart: FnOnce() -> Result<(), String>,
{
    validate()?;
    stop()?;
    let commit = recover()?;
    let restart_error = restart().err();
    Ok(ProjectDataRecoveryOutcome {
        commit,
        restart_error,
    })
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopProjectDataBackup {
    backup_id: String,
    created_at: String,
    trigger: BackupTrigger,
    app_version: String,
    schema_version: i64,
    size_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopProjectDataEnvironmentStatus {
    environment_id: String,
    label: String,
    running_distro: Option<String>,
    status: &'static str,
    requested_root: String,
    effective_root: String,
    is_filesystem_alias: bool,
    storage_instance_id: Option<String>,
    issue: Option<String>,
    backups: Vec<DesktopProjectDataBackup>,
}

fn desktop_status(
    environment_id: String,
    label: String,
    running_distro: Option<String>,
    inspection: StoreInspection,
) -> DesktopProjectDataEnvironmentStatus {
    let status = match inspection.classification {
        StoreInspectionStatus::FirstRun
        | StoreInspectionStatus::ExistingUnmarked
        | StoreInspectionStatus::Existing => "healthy",
        StoreInspectionStatus::DatabaseMissing
        | StoreInspectionStatus::MarkerMalformed
        | StoreInspectionStatus::CorruptDatabase
        | StoreInspectionStatus::UnrecognizedStore
        | StoreInspectionStatus::UnsafeDatabaseState
        | StoreInspectionStatus::RecoveryIncomplete => "recovery-required",
    };
    DesktopProjectDataEnvironmentStatus {
        environment_id,
        label,
        running_distro,
        status,
        requested_root: inspection.requested_root.to_string_lossy().into_owned(),
        effective_root: inspection.effective_root.to_string_lossy().into_owned(),
        is_filesystem_alias: inspection.is_filesystem_alias,
        storage_instance_id: inspection
            .storage_instance_id
            .map(|value| value.to_string()),
        issue: inspection.issue,
        backups: inspection
            .backups
            .into_iter()
            .map(|backup| DesktopProjectDataBackup {
                backup_id: backup.manifest.backup_id.to_string(),
                created_at: backup.manifest.created_at,
                trigger: backup.manifest.trigger,
                app_version: backup.manifest.app_version,
                schema_version: backup.manifest.schema_version,
                size_bytes: backup.manifest.database_size_bytes,
            })
            .collect(),
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WslInspectionDocument {
    classification: String,
    storage_instance_id: Option<String>,
    backups: Vec<WslBackupDocument>,
    requested_root: String,
    effective_root: String,
    is_filesystem_alias: bool,
    issue: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WslBackupDocument {
    backup_id: String,
    created_at: String,
    trigger: BackupTrigger,
    app_version: String,
    schema_version: i64,
    database_size_bytes: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WslRecoveryDocument {
    action: String,
    preserved_directory: String,
    storage_instance_id: Option<String>,
}

#[derive(Debug, Clone)]
enum ProjectDataTarget {
    Native(ResolvedDataRoot),
    Wsl {
        distro: String,
        binary: String,
        root: String,
    },
}

impl ProjectDataTarget {
    fn root(&self) -> String {
        match self {
            Self::Native(root) => root.effective.to_string_lossy().into_owned(),
            Self::Wsl { root, .. } => root.clone(),
        }
    }
}

fn wsl_plan_parts(plan: &BackendLaunchPlan) -> Result<(String, String, String), ProjectDataError> {
    let BackendLaunchTarget::ExternalProcess {
        program,
        args,
        data_root,
        ..
    } = &plan.target
    else {
        return Err(ProjectDataError::Inspection(
            "The selected backend is not a WSL process.".to_owned(),
        ));
    };
    if !program.eq_ignore_ascii_case(WSL_PROGRAM) {
        return Err(ProjectDataError::Inspection(
            "The selected external backend is not owned by WSL.".to_owned(),
        ));
    }
    let distro_index = args
        .iter()
        .position(|arg| arg == "-d" || arg == "--distribution")
        .and_then(|index| index.checked_add(1))
        .filter(|index| *index < args.len())
        .ok_or_else(|| ProjectDataError::Inspection("The WSL distro is unavailable.".to_owned()))?;
    let serve_index = args.iter().position(|arg| arg == "serve").ok_or_else(|| {
        ProjectDataError::Inspection("The bundled WSL binary is unavailable.".to_owned())
    })?;
    let binary_index = serve_index.checked_sub(1).ok_or_else(|| {
        ProjectDataError::Inspection("The bundled WSL binary is unavailable.".to_owned())
    })?;
    let data_root = data_root.clone().ok_or_else(|| {
        ProjectDataError::Inspection("The WSL project data root is unavailable.".to_owned())
    })?;
    Ok((
        args[distro_index].clone(),
        args[binary_index].clone(),
        data_root,
    ))
}

async fn bounded_command(invocation: WslStorageInvocation) -> Result<Vec<u8>, ProjectDataError> {
    let mut command = Command::new(&invocation.program);
    command
        .args(&invocation.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(|error| {
        ProjectDataError::Inspection(format!("Could not launch WSL recovery: {error}"))
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        ProjectDataError::Inspection("WSL recovery did not expose standard output.".to_owned())
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        ProjectDataError::Inspection("WSL recovery did not expose standard error.".to_owned())
    })?;
    let read_stdout = async move {
        let mut bytes = Vec::new();
        stdout
            .take(WSL_OUTPUT_LIMIT + 1)
            .read_to_end(&mut bytes)
            .await
            .map(|_| bytes)
    };
    let read_stderr = async move {
        let mut bytes = Vec::new();
        stderr
            .take(WSL_OUTPUT_LIMIT + 1)
            .read_to_end(&mut bytes)
            .await
            .map(|_| bytes)
    };
    let (status, stdout, stderr) = tokio::time::timeout(WSL_COMMAND_TIMEOUT, async {
        let (stdout, stderr, status) = tokio::join!(read_stdout, read_stderr, child.wait());
        Ok::<_, std::io::Error>((status?, stdout?, stderr?))
    })
    .await
    .map_err(|_| ProjectDataError::Inspection("WSL recovery timed out.".to_owned()))?
    .map_err(|error| {
        ProjectDataError::Inspection(format!("WSL recovery could not finish: {error}"))
    })?;
    if stdout.len() as u64 > WSL_OUTPUT_LIMIT || stderr.len() as u64 > WSL_OUTPUT_LIMIT {
        return Err(ProjectDataError::Inspection(
            "WSL recovery output exceeded its safety limit.".to_owned(),
        ));
    }
    if !status.success() {
        let detail = String::from_utf8_lossy(&stderr);
        return Err(ProjectDataError::Recovery(format!(
            "The WSL recovery command failed: {}",
            detail.trim()
        )));
    }
    Ok(stdout)
}

async fn resolve_project_data_target(
    plan: &BackendLaunchPlan,
) -> Result<ProjectDataTarget, ProjectDataError> {
    match &plan.target {
        BackendLaunchTarget::InProcess { data_root, .. } => {
            Ok(ProjectDataTarget::Native(data_root.clone()))
        }
        BackendLaunchTarget::ExternalProcess { .. } => {
            let (distro, binary, root) = wsl_plan_parts(plan)?;
            if !root.starts_with('/') || root.contains('\n') || root.contains('\r') {
                return Err(ProjectDataError::Inspection(
                    "The WSL project data root is not an absolute Linux path.".to_owned(),
                ));
            }
            Ok(ProjectDataTarget::Wsl {
                distro,
                binary,
                root,
            })
        }
    }
}

fn wsl_desktop_status(
    target: &BackendProjectDataTarget,
    document: WslInspectionDocument,
) -> DesktopProjectDataEnvironmentStatus {
    let healthy = matches!(
        document.classification.as_str(),
        "first-run" | "existing-unmarked" | "existing"
    );
    DesktopProjectDataEnvironmentStatus {
        environment_id: target.environment_id.clone(),
        label: target.label.clone(),
        running_distro: target.running_distro.clone(),
        status: if healthy {
            "healthy"
        } else {
            "recovery-required"
        },
        requested_root: document.requested_root,
        effective_root: document.effective_root,
        is_filesystem_alias: document.is_filesystem_alias,
        storage_instance_id: document.storage_instance_id,
        issue: document.issue,
        backups: document
            .backups
            .into_iter()
            .map(|backup| DesktopProjectDataBackup {
                backup_id: backup.backup_id,
                created_at: backup.created_at,
                trigger: backup.trigger,
                app_version: backup.app_version,
                schema_version: backup.schema_version,
                size_bytes: backup.database_size_bytes,
            })
            .collect(),
    }
}

fn unavailable_desktop_status(
    target: &BackendProjectDataTarget,
    issue: String,
) -> DesktopProjectDataEnvironmentStatus {
    let (requested_root, effective_root) = match &target.launch_plan.target {
        BackendLaunchTarget::InProcess { data_root, .. } => (
            data_root.requested.to_string_lossy().into_owned(),
            data_root.effective.to_string_lossy().into_owned(),
        ),
        BackendLaunchTarget::ExternalProcess { .. } => {
            ("Unavailable".to_owned(), "Unavailable".to_owned())
        }
    };
    DesktopProjectDataEnvironmentStatus {
        environment_id: target.environment_id.clone(),
        label: target.label.clone(),
        running_distro: target.running_distro.clone(),
        status: "unavailable",
        requested_root,
        effective_root,
        is_filesystem_alias: false,
        storage_instance_id: None,
        issue: Some(issue),
        backups: Vec::new(),
    }
}

async fn inspect_target(
    target: &BackendProjectDataTarget,
    resolved: &ProjectDataTarget,
) -> Result<DesktopProjectDataEnvironmentStatus, ProjectDataError> {
    match resolved {
        ProjectDataTarget::Native(root) => {
            let inspection = persistence::inspect_store(root)
                .await
                .map_err(|error| ProjectDataError::Inspection(error.to_string()))?;
            Ok(desktop_status(
                target.environment_id.clone(),
                target.label.clone(),
                target.running_distro.clone(),
                inspection,
            ))
        }
        ProjectDataTarget::Wsl {
            distro,
            binary,
            root,
        } => {
            let bytes = bounded_command(wsl_storage_invocation(
                distro,
                binary,
                root,
                WslStorageAction::Inspect,
            ))
            .await?;
            let document = serde_json::from_slice(&bytes).map_err(|error| {
                ProjectDataError::Inspection(format!("WSL inspection output was invalid: {error}"))
            })?;
            Ok(wsl_desktop_status(target, document))
        }
    }
}

pub(crate) async fn get_project_data_statuses(
    backend: &BackendSupervisor,
) -> Result<Vec<DesktopProjectDataEnvironmentStatus>, String> {
    let mut statuses = Vec::new();
    for target in backend.project_data_targets() {
        let status = match resolve_project_data_target(&target.launch_plan).await {
            Ok(resolved) => match inspect_target(&target, &resolved).await {
                Ok(status) => status,
                Err(error) => unavailable_desktop_status(&target, error.to_string()),
            },
            Err(error) => unavailable_desktop_status(&target, error.to_string()),
        };
        statuses.push(status);
    }
    Ok(statuses)
}

async fn recover_native(
    root: &ResolvedDataRoot,
    action: WslStorageAction<'_>,
) -> Result<ProjectDataRecoveryCommit, ProjectDataError> {
    let result = match action {
        WslStorageAction::Restore { backup_id } => {
            let backup_id =
                Uuid::parse_str(backup_id).map_err(|_| ProjectDataError::InvalidSelection)?;
            persistence::restore_backup(root, backup_id).await
        }
        WslStorageAction::StartEmpty => persistence::preserve_and_start_empty(root).await,
        WslStorageAction::Inspect => return Err(ProjectDataError::InvalidSelection),
    }
    .map_err(|error| ProjectDataError::Recovery(error.to_string()))?;
    Ok(result.into())
}

async fn recover_wsl(
    distro: &str,
    binary: &str,
    root: &str,
    action: WslStorageAction<'_>,
) -> Result<ProjectDataRecoveryCommit, ProjectDataError> {
    let bytes = bounded_command(wsl_storage_invocation(distro, binary, root, action)).await?;
    let result: WslRecoveryDocument = serde_json::from_slice(&bytes).map_err(|error| {
        ProjectDataError::Recovery(format!("WSL recovery output was invalid: {error}"))
    })?;
    let action = match result.action.as_str() {
        "restore" => RecoveryAction::Restore,
        "start-empty" => RecoveryAction::StartEmpty,
        _ => {
            return Err(ProjectDataError::Recovery(
                "WSL recovery returned an unknown action.".to_owned(),
            ));
        }
    };
    Ok(ProjectDataRecoveryCommit {
        action,
        preserved_directory: result.preserved_directory,
        storage_instance_id: result.storage_instance_id,
    })
}

async fn run_recovery(
    backend: &BackendSupervisor,
    environment_id: &str,
    action: WslStorageAction<'_>,
) -> Result<DesktopProjectDataRecoveryResult, String> {
    let mut operation: BackendProjectDataOperation = backend
        .begin_project_data_operation(environment_id)
        .await
        .map_err(|error| ProjectDataError::Inspection(error).to_string())?;
    let target = operation.target().clone();
    let resolved = resolve_project_data_target(&target.launch_plan)
        .await
        .map_err(|error| error.to_string())?;
    let inspection = inspect_target(&target, &resolved)
        .await
        .map_err(|error| error.to_string())?;
    if let WslStorageAction::Restore { backup_id } = action
        && !inspection
            .backups
            .iter()
            .any(|backup| backup.backup_id == backup_id)
    {
        return Err(ProjectDataError::InvalidSelection.to_string());
    }
    operation
        .stop_selected()
        .await
        .map_err(|error| ProjectDataError::Stop(error).to_string())?;
    let commit = match &resolved {
        ProjectDataTarget::Native(root) => recover_native(root, action).await,
        ProjectDataTarget::Wsl {
            distro,
            binary,
            root,
        } => recover_wsl(distro, binary, root, action).await,
    }
    .map_err(|error| error.to_string())?;
    let restart_error = operation
        .restart_after_commit()
        .await
        .err()
        .map(String::from);
    Ok(DesktopProjectDataRecoveryResult {
        environment_id: environment_id.to_owned(),
        action: match commit.action {
            RecoveryAction::Restore => "restore",
            RecoveryAction::StartEmpty => "start-empty",
        },
        committed: true,
        preserved_directory: commit.preserved_directory,
        storage_instance_id: commit.storage_instance_id,
        restart_error,
    })
}

pub(crate) async fn restore_project_data(
    backend: &BackendSupervisor,
    environment_id: &str,
    backup_id: &str,
) -> Result<DesktopProjectDataRecoveryResult, String> {
    Uuid::parse_str(backup_id).map_err(|_| ProjectDataError::InvalidSelection.to_string())?;
    run_recovery(
        backend,
        environment_id,
        WslStorageAction::Restore { backup_id },
    )
    .await
}

pub(crate) async fn start_empty_project_data(
    backend: &BackendSupervisor,
    environment_id: &str,
) -> Result<DesktopProjectDataRecoveryResult, String> {
    run_recovery(backend, environment_id, WslStorageAction::StartEmpty).await
}

pub(crate) async fn retry_project_data(
    backend: &BackendSupervisor,
    environment_id: &str,
) -> Result<(), ProjectDataError> {
    let operation = backend
        .begin_project_data_operation(environment_id)
        .await
        .map_err(ProjectDataError::Inspection)?;
    if operation.target().running {
        return Ok(());
    }
    operation
        .restart_after_commit()
        .await
        .map_err(ProjectDataError::Restart)
}

pub(crate) async fn project_data_root(
    backend: &BackendSupervisor,
    environment_id: &str,
) -> Result<String, String> {
    let target = backend
        .project_data_targets()
        .into_iter()
        .find(|target| target.environment_id == environment_id)
        .ok_or_else(|| ProjectDataError::InvalidSelection.to_string())?;
    resolve_project_data_target(&target.launch_plan)
        .await
        .map(|target| target.root())
        .map_err(|error| error.to_string())
}

pub(crate) async fn project_data_diagnostics(
    backend: &BackendSupervisor,
    environment_id: &str,
) -> Result<serde_json::Value, String> {
    let statuses = get_project_data_statuses(backend).await?;
    let status = statuses
        .into_iter()
        .find(|status| status.environment_id == environment_id)
        .ok_or_else(|| ProjectDataError::InvalidSelection.to_string())?;
    serde_json::to_value(status).map_err(|error| format!("Could not encode diagnostics: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendRunConfig, BackendShutdownConfig, WslBackendLaunchPlanInput};
    use coddy_studio_server::{DataRootRequest, DataRootSource, resolve_data_root};
    use std::fs;

    fn local_config() -> BackendRunConfig {
        BackendRunConfig {
            environment_id: "primary".to_owned(),
            label: "Local".to_owned(),
            running_distro: None,
            port: 0,
            bind_host: "127.0.0.1".to_owned(),
            local_host: "127.0.0.1".to_owned(),
            desktop_bootstrap_token: "project-data-test-token".to_owned(),
            server_exposure_mode: "local-only".to_owned(),
            endpoint_url: None,
            advertised_host: None,
            tailscale_serve_enabled: false,
            tailscale_serve_port: 443,
        }
    }

    fn local_plan_for(
        path: &std::path::Path,
        environment_id: &str,
        label: &str,
    ) -> BackendLaunchPlan {
        let data_root = resolve_data_root(DataRootRequest::explicit(
            DataRootSource::Cli,
            path.to_path_buf(),
            std::path::PathBuf::new(),
        ))
        .expect("test root should resolve");
        BackendLaunchPlan {
            target: BackendLaunchTarget::InProcess {
                base_dir: data_root.effective.clone(),
                data_root,
            },
            log_path: None,
            config: BackendRunConfig {
                environment_id: environment_id.to_owned(),
                label: label.to_owned(),
                ..local_config()
            },
        }
    }

    fn local_plan(path: &std::path::Path) -> BackendLaunchPlan {
        local_plan_for(path, "primary", "Local")
    }

    #[test]
    fn retry_error_preserves_classification_and_generalizes_restart_wording() {
        let error = ProjectDataError::Restart(BackendStartFailure::PortInUse {
            port: 43117,
            detail: "listener is occupied".to_owned(),
        });
        assert_eq!(
            error.to_string(),
            "Coddy Studio's local server couldn't restart: listener is occupied"
        );
        assert!(matches!(
            error,
            ProjectDataError::Restart(BackendStartFailure::PortInUse { port: 43117, .. })
        ));
        assert_eq!(
            ProjectDataError::Inspection("busy".to_owned()).to_string(),
            "project-data inspection failed: busy"
        );
    }

    #[test]
    fn wsl_storage_invocation_is_an_argument_vector_without_shell_interpretation() {
        let invocation = wsl_storage_invocation(
            "Ubuntu; touch /tmp/owned",
            "/opt/coddy-studio/bin/coddy-studio; echo owned",
            "/home/user/.coddy/studio; touch /tmp/store-owned",
            WslStorageAction::Restore {
                backup_id: "26b6ca53-27d3-401a-b51f-d7bdf534081f",
            },
        );

        assert_eq!(invocation.program, "wsl.exe");
        assert_eq!(
            invocation.args,
            vec![
                "--distribution",
                "Ubuntu; touch /tmp/owned",
                "--",
                "/opt/coddy-studio/bin/coddy-studio; echo owned",
                "storage",
                "restore",
                "--backup-id",
                "26b6ca53-27d3-401a-b51f-d7bdf534081f",
                "--base-dir",
                "/home/user/.coddy/studio; touch /tmp/store-owned",
                "--json",
            ]
        );
    }

    #[tokio::test]
    async fn wsl_recovery_uses_the_exact_root_pinned_in_the_server_bootstrap() {
        let plan = BackendLaunchPlan::wsl(WslBackendLaunchPlanInput {
            environment_id: "wsl:Ubuntu".to_owned(),
            label: "WSL (Ubuntu)".to_owned(),
            running_distro: "Ubuntu".to_owned(),
            port: 4_301,
            renderer_host: "172.20.0.2".to_owned(),
            desktop_bootstrap_token: "wsl-token".to_owned(),
            binary_path: "/opt/coddy-studio/bin/coddy-studio".to_owned(),
            data_root: "/srv/coddy-studio projects".to_owned(),
        });

        let resolved = resolve_project_data_target(&plan)
            .await
            .expect("the launch plan should own the recovery root");
        assert!(matches!(
            resolved,
            ProjectDataTarget::Wsl { ref root, .. } if root == "/srv/coddy-studio projects"
        ));
        let BackendLaunchTarget::ExternalProcess { bootstrap_line, .. } = &plan.target else {
            panic!("WSL plan should launch an external process");
        };
        let bootstrap: serde_json::Value =
            serde_json::from_str(bootstrap_line).expect("bootstrap should be JSON");
        assert_eq!(bootstrap["coddyStudioHome"], "/srv/coddy-studio projects");
    }

    #[tokio::test]
    async fn validation_failure_does_not_stop_or_restart_the_target() {
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let result = execute_recovery_workflow(
            {
                let events = events.clone();
                move || {
                    events.lock().expect("events lock").push("validate");
                    Err(ProjectDataError::InvalidSelection)
                }
            },
            {
                let events = events.clone();
                move || {
                    events.lock().expect("events lock").push("stop");
                    Ok(())
                }
            },
            {
                let events = events.clone();
                move || {
                    events.lock().expect("events lock").push("recover");
                    Ok(ProjectDataRecoveryCommit::preserved_for_test())
                }
            },
            {
                let events = events.clone();
                move || {
                    events.lock().expect("events lock").push("restart");
                    Ok(())
                }
            },
        )
        .await;

        assert!(matches!(result, Err(ProjectDataError::InvalidSelection)));
        assert_eq!(*events.lock().expect("events lock"), vec!["validate"]);
    }

    #[tokio::test]
    async fn committed_recovery_restarts_only_after_the_selected_target_stops() {
        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let result = execute_recovery_workflow(
            {
                let events = events.clone();
                move || {
                    events.lock().expect("events lock").push("validate");
                    Ok(())
                }
            },
            {
                let events = events.clone();
                move || {
                    events.lock().expect("events lock").push("stop");
                    Ok(())
                }
            },
            {
                let events = events.clone();
                move || {
                    events.lock().expect("events lock").push("recover");
                    Ok(ProjectDataRecoveryCommit::preserved_for_test())
                }
            },
            {
                let events = events.clone();
                move || {
                    events.lock().expect("events lock").push("restart");
                    Ok(())
                }
            },
        )
        .await
        .expect("recovery should commit");

        assert!(result.restart_error.is_none());
        assert_eq!(
            *events.lock().expect("events lock"),
            vec!["validate", "stop", "recover", "restart"]
        );
    }

    #[tokio::test]
    async fn native_status_inspects_the_current_launch_plan_without_renderer_paths() {
        let root = tempfile::tempdir().expect("native project-data root");
        let supervisor = BackendSupervisor::new();
        supervisor
            .start(local_plan(root.path()))
            .await
            .expect("native backend should start");

        let statuses = get_project_data_statuses(&supervisor)
            .await
            .expect("native status should inspect");
        assert_eq!(statuses.len(), 1);
        assert_eq!(statuses[0].environment_id, "primary");
        assert_eq!(statuses[0].status, "healthy");
        assert_eq!(
            statuses[0].effective_root,
            fs::canonicalize(root.path())
                .expect("canonical root")
                .to_string_lossy()
                .into_owned()
        );

        supervisor
            .stop(BackendShutdownConfig::default())
            .await
            .expect("native backend should stop");
    }

    #[tokio::test]
    async fn native_start_empty_stops_preserves_commits_and_restarts_the_same_target() {
        // Restarts a backend on the port it has just released.
        let Some(isolated) = crate::test_support::isolated_scenario(
            "data_safety::tests::native_start_empty_stops_preserves_commits_and_restarts_the_same_target",
        ) else {
            return;
        };
        let root = tempfile::tempdir().expect("native project-data root");
        let supervisor = BackendSupervisor::new();
        supervisor
            .start(local_plan(root.path()))
            .await
            .expect("native backend should start");
        let marker = fs::canonicalize(root.path())
            .expect("canonical root")
            .join("userdata")
            .join("environment-id");
        let original_marker = fs::read_to_string(&marker).expect("original marker");

        let result = start_empty_project_data(&supervisor, "primary")
            .await
            .expect("start-empty should commit");
        assert!(result.committed);
        assert_eq!(result.action, "start-empty");
        assert!(result.restart_error.is_none());
        assert_ne!(
            fs::read_to_string(&marker).expect("replacement marker"),
            original_marker
        );
        assert!(std::path::Path::new(&result.preserved_directory).is_dir());
        assert!(
            supervisor
                .project_data_targets()
                .into_iter()
                .find(|target| target.environment_id == "primary")
                .is_some_and(|target| target.running)
        );

        supervisor
            .stop(BackendShutdownConfig::default())
            .await
            .expect("native backend should stop");
        isolated.complete();
    }

    #[tokio::test]
    async fn unknown_backup_validation_keeps_the_native_target_running() {
        let root = tempfile::tempdir().expect("native project-data root");
        let supervisor = BackendSupervisor::new();
        supervisor
            .start(local_plan(root.path()))
            .await
            .expect("native backend should start");

        let error = restore_project_data(
            &supervisor,
            "primary",
            "26b6ca53-27d3-401a-b51f-d7bdf534081f",
        )
        .await
        .expect_err("unknown backup must fail validation");
        assert!(error.contains("no longer available"));
        assert!(
            supervisor
                .project_data_targets()
                .into_iter()
                .find(|target| target.environment_id == "primary")
                .is_some_and(|target| target.running)
        );

        supervisor
            .stop(BackendShutdownConfig::default())
            .await
            .expect("native backend should stop");
    }

    #[tokio::test]
    async fn committed_recovery_restarts_a_previously_failed_registered_target() {
        // Restarts a backend on the port it has just released.
        let Some(isolated) = crate::test_support::isolated_scenario(
            "data_safety::tests::committed_recovery_restarts_a_previously_failed_registered_target",
        ) else {
            return;
        };
        let root = tempfile::tempdir().expect("native project-data root");
        let supervisor = BackendSupervisor::new();
        supervisor
            .start(local_plan(root.path()))
            .await
            .expect("native backend should start");
        let mut stopped = supervisor
            .begin_project_data_operation("primary")
            .await
            .expect("exclusive operation should begin");
        stopped
            .stop_selected()
            .await
            .expect("selected target should stop");
        drop(stopped);
        assert!(
            supervisor
                .project_data_targets()
                .into_iter()
                .find(|target| target.environment_id == "primary")
                .is_some_and(|target| !target.running)
        );

        let result = start_empty_project_data(&supervisor, "primary")
            .await
            .expect("committed recovery should restart the registered plan");
        assert!(result.restart_error.is_none());
        assert!(
            supervisor
                .project_data_targets()
                .into_iter()
                .find(|target| target.environment_id == "primary")
                .is_some_and(|target| target.running)
        );

        supervisor
            .stop(BackendShutdownConfig::default())
            .await
            .expect("native backend should stop");
        isolated.complete();
    }

    #[tokio::test]
    async fn retry_starts_the_exact_registered_target_only_when_it_is_stopped() {
        // Restarts a backend on the port it has just released.
        let Some(isolated) = crate::test_support::isolated_scenario(
            "data_safety::tests::retry_starts_the_exact_registered_target_only_when_it_is_stopped",
        ) else {
            return;
        };
        let root = tempfile::tempdir().expect("native project-data root");
        let supervisor = BackendSupervisor::new();
        supervisor
            .start(local_plan(root.path()))
            .await
            .expect("native backend should start");
        let mut operation = supervisor
            .begin_project_data_operation("primary")
            .await
            .expect("exclusive operation should begin");
        operation
            .stop_selected()
            .await
            .expect("selected target should stop");
        drop(operation);

        retry_project_data(&supervisor, "primary")
            .await
            .expect("retry should restart the registered target");
        assert!(
            supervisor
                .project_data_targets()
                .into_iter()
                .find(|target| target.environment_id == "primary")
                .is_some_and(|target| target.running)
        );
        retry_project_data(&supervisor, "primary")
            .await
            .expect("retrying a running target should be inert");

        supervisor
            .stop(BackendShutdownConfig::default())
            .await
            .expect("native backend should stop");
        isolated.complete();
    }

    #[tokio::test]
    async fn recovery_stops_only_the_selected_environment() {
        // Restarts a backend on the port it has just released.
        let Some(isolated) = crate::test_support::isolated_scenario(
            "data_safety::tests::recovery_stops_only_the_selected_environment",
        ) else {
            return;
        };
        let primary_root = tempfile::tempdir().expect("primary project-data root");
        let secondary_root = tempfile::tempdir().expect("secondary project-data root");
        let supervisor = BackendSupervisor::new();
        supervisor
            .start(local_plan(primary_root.path()))
            .await
            .expect("primary backend should start");
        supervisor
            .start(local_plan_for(
                secondary_root.path(),
                "wsl:test",
                "WSL (test)",
            ))
            .await
            .expect("secondary backend should start");
        let secondary_marker = fs::canonicalize(secondary_root.path())
            .expect("canonical secondary root")
            .join("userdata")
            .join("environment-id");
        let secondary_marker_bytes = fs::read(&secondary_marker).expect("secondary marker");

        let recovery = start_empty_project_data(&supervisor, "primary")
            .await
            .expect("primary start-empty should commit");
        let targets = supervisor.project_data_targets();
        assert!(
            targets
                .iter()
                .find(|target| target.environment_id == "primary")
                .is_some_and(|target| target.running),
            "{recovery:?}\n{targets:?}"
        );
        assert!(
            targets
                .iter()
                .find(|target| target.environment_id == "wsl:test")
                .is_some_and(|target| target.running),
            "{recovery:?}\n{targets:?}"
        );
        assert_eq!(
            fs::read(&secondary_marker).expect("secondary marker after recovery"),
            secondary_marker_bytes
        );

        supervisor
            .stop(BackendShutdownConfig::default())
            .await
            .expect("backends should stop");
        isolated.complete();
    }
}
