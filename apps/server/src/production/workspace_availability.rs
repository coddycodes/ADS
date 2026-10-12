use std::path::{Path, PathBuf};

use crate::{
    persistence::Repositories,
    worktree_catalog::{
        WorkspaceAdmissionError as RegistryWorkspaceAdmissionError, WorkspaceAdmissionLease,
        WorkspaceAvailabilityRegistry, WorkspaceIdentityError, WorkspaceUnavailable,
    },
};

#[derive(Clone)]
pub(crate) struct WorkspaceAdmissionController {
    registry: WorkspaceAvailabilityRegistry,
    repositories: Option<Repositories>,
}

pub(crate) enum WorkspaceAdmissionError {
    Unavailable(WorkspaceUnavailable),
    Identity(WorkspaceIdentityError),
    Resolution(String),
}

impl From<RegistryWorkspaceAdmissionError> for WorkspaceAdmissionError {
    fn from(error: RegistryWorkspaceAdmissionError) -> Self {
        match error {
            RegistryWorkspaceAdmissionError::Unavailable(error) => Self::Unavailable(error),
            RegistryWorkspaceAdmissionError::Identity(error) => Self::Identity(error),
        }
    }
}

impl WorkspaceAdmissionController {
    pub(crate) fn new(registry: WorkspaceAvailabilityRegistry, repositories: Repositories) -> Self {
        Self {
            registry,
            repositories: Some(repositories),
        }
    }

    pub(crate) fn registry_only(registry: WorkspaceAvailabilityRegistry) -> Self {
        Self {
            registry,
            repositories: None,
        }
    }

    pub(crate) async fn acquire_thread<'a>(
        &self,
        thread_id: &str,
        requested_paths: impl IntoIterator<Item = &'a Path>,
    ) -> Result<WorkspaceAdmissionLease, WorkspaceAdmissionError> {
        self.acquire_thread_with_origin(thread_id, requested_paths)
            .await
            .map(|(admission, _)| admission)
    }

    /// Like [`Self::acquire_thread`], and also returns the project whose root is the thread's
    /// persisted workspace, when the thread has no worktree of its own.
    pub(crate) async fn acquire_thread_with_origin<'a>(
        &self,
        thread_id: &str,
        requested_paths: impl IntoIterator<Item = &'a Path>,
    ) -> Result<(WorkspaceAdmissionLease, Option<String>), WorkspaceAdmissionError> {
        let mut paths = requested_paths
            .into_iter()
            .map(Path::to_path_buf)
            .collect::<Vec<_>>();
        let mut inherited_root_of = None;
        if let Some(repositories) = &self.repositories
            && let Some(workspace) = resolve_persisted_workspace(repositories, thread_id).await?
        {
            paths.push(workspace.path);
            inherited_root_of = workspace.inherited_root_of;
        }
        self.registry
            .acquire_admission(thread_id, paths.iter().map(PathBuf::as_path))
            .await
            .map(|admission| (admission, inherited_root_of))
            .map_err(WorkspaceAdmissionError::from)
    }
}

/// A thread's persisted workspace: its own worktree, or the root of its project.
struct PersistedWorkspace {
    path: PathBuf,
    /// The project whose root `path` is, when the thread has no worktree of its own.
    inherited_root_of: Option<String>,
}

async fn resolve_persisted_workspace(
    repositories: &Repositories,
    thread_id: &str,
) -> Result<Option<PersistedWorkspace>, WorkspaceAdmissionError> {
    let Some(thread) = repositories
        .get_thread(thread_id.to_owned())
        .await
        .map_err(|error| WorkspaceAdmissionError::Resolution(error.to_string()))?
    else {
        return Ok(None);
    };
    if let Some(path) = thread.worktree_path {
        return Ok(Some(PersistedWorkspace {
            path: PathBuf::from(path),
            inherited_root_of: None,
        }));
    }
    repositories
        .get_project(thread.project_id.clone())
        .await
        .map(|project| {
            project.map(|project| PersistedWorkspace {
                path: PathBuf::from(project.workspace_root),
                inherited_root_of: Some(thread.project_id),
            })
        })
        .map_err(|error| WorkspaceAdmissionError::Resolution(error.to_string()))
}
