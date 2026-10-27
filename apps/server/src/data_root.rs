use std::{
    error::Error as StdError,
    ffi::OsStr,
    fmt, fs,
    path::{Component, Path, PathBuf},
};

use serde::Serialize;
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DataRootSource {
    Default,
    Environment,
    Cli,
}

impl fmt::Display for DataRootSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl StdError for DataRootSource {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DataRootRequest {
    pub source: DataRootSource,
    pub requested: Option<PathBuf>,
    pub home_dir: PathBuf,
}

impl DataRootRequest {
    #[must_use]
    pub fn default(home_dir: PathBuf) -> Self {
        Self {
            source: DataRootSource::Default,
            requested: None,
            home_dir,
        }
    }

    #[must_use]
    pub fn explicit(source: DataRootSource, requested: PathBuf, home_dir: PathBuf) -> Self {
        Self {
            source,
            requested: Some(requested),
            home_dir,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedDataRoot {
    pub source: DataRootSource,
    pub requested: PathBuf,
    pub effective: PathBuf,
    pub is_filesystem_alias: bool,
}

#[derive(Debug, Error)]
pub enum DataRootError {
    #[error("{source:?} data root must be absolute: {path}")]
    RelativeExplicit {
        source: DataRootSource,
        path: PathBuf,
    },
    #[error("the current user's home directory is unavailable")]
    HomeDirectoryUnavailable,
    #[error("failed to resolve data root {path}")]
    Canonicalize {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub fn resolve_data_root(request: DataRootRequest) -> Result<ResolvedDataRoot, DataRootError> {
    let requested = match request.requested {
        Some(path) => expand_home(path, &request.home_dir)?,
        None => {
            if request.home_dir.as_os_str().is_empty() {
                return Err(DataRootError::HomeDirectoryUnavailable);
            }
            request.home_dir.join(".coddy").join("studio")
        }
    };

    if !requested.is_absolute() {
        return Err(DataRootError::RelativeExplicit {
            source: request.source,
            path: requested,
        });
    }

    let requested = lexical_normalize(&requested);
    let effective = canonicalize_with_missing_leaves(&requested)?;
    Ok(ResolvedDataRoot {
        source: request.source,
        is_filesystem_alias: requested != effective,
        requested,
        effective,
    })
}

fn expand_home(path: PathBuf, home_dir: &Path) -> Result<PathBuf, DataRootError> {
    match path.strip_prefix("~") {
        Ok(relative) => {
            if home_dir.as_os_str().is_empty() {
                Err(DataRootError::HomeDirectoryUnavailable)
            } else {
                Ok(home_dir.join(relative))
            }
        }
        Err(_) => Ok(path),
    }
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    normalized
}

fn canonicalize_with_missing_leaves(path: &Path) -> Result<PathBuf, DataRootError> {
    let mut existing_ancestor = path;
    let mut missing_leaves = Vec::new();

    while !existing_ancestor.exists() {
        let Some(leaf) = existing_ancestor.file_name() else {
            break;
        };
        missing_leaves.push(leaf.to_os_string());
        let Some(parent) = existing_ancestor.parent() else {
            break;
        };
        existing_ancestor = parent;
    }

    let mut effective =
        fs::canonicalize(existing_ancestor).map_err(|source| DataRootError::Canonicalize {
            path: existing_ancestor.to_path_buf(),
            source,
        })?;
    for leaf in missing_leaves.iter().rev() {
        effective.push(OsStr::new(leaf));
    }
    Ok(effective)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{DataRootError, DataRootRequest, DataRootSource, resolve_data_root};

    #[test]
    fn resolves_default_root_below_home_directory() {
        let temp = tempfile::tempdir().expect("temp root");
        let resolved = resolve_data_root(DataRootRequest {
            source: DataRootSource::Default,
            requested: None,
            home_dir: temp.path().to_path_buf(),
        })
        .expect("resolve default root");

        assert_eq!(
            resolved.requested,
            temp.path().join(".coddy").join("studio")
        );
        assert_eq!(
            resolved.effective,
            temp.path()
                .canonicalize()
                .expect("canonical home")
                .join(".coddy")
                .join("studio")
        );
    }

    #[test]
    fn expands_a_leading_tilde_from_the_supplied_home_directory() {
        let temp = tempfile::tempdir().expect("temp home");
        let resolved = resolve_data_root(DataRootRequest {
            source: DataRootSource::Environment,
            requested: Some(PathBuf::from("~/.coddy/studio")),
            home_dir: temp.path().to_path_buf(),
        })
        .expect("resolve home-relative root");

        assert_eq!(resolved.requested, temp.path().join(".coddy/studio"));
    }

    #[test]
    fn rejects_relative_explicit_roots() {
        let error = resolve_data_root(DataRootRequest {
            source: DataRootSource::Environment,
            requested: Some(PathBuf::from("relative/.coddy/studio")),
            home_dir: PathBuf::from("/home/alice"),
        })
        .expect_err("relative environment root must fail");
        assert!(matches!(error, DataRootError::RelativeExplicit { .. }));
    }

    #[test]
    fn preserves_missing_final_components_after_canonicalizing_the_existing_ancestor() {
        let temp = tempfile::tempdir().expect("temp root");
        let existing = temp.path().join("existing");
        std::fs::create_dir(&existing).expect("existing directory");
        let requested = existing.join("missing/leaf");

        let resolved = resolve_data_root(DataRootRequest {
            source: DataRootSource::Cli,
            requested: Some(requested.clone()),
            home_dir: temp.path().to_path_buf(),
        })
        .expect("resolve missing leaf");

        assert_eq!(resolved.requested, requested);
        assert_eq!(
            resolved.effective,
            existing
                .canonicalize()
                .expect("canonical existing")
                .join("missing/leaf")
        );
    }

    #[cfg(unix)]
    #[test]
    fn reports_symlink_requested_and_effective_roots() {
        let temp = tempfile::tempdir().expect("temp root");
        let target = temp.path().join("target");
        std::fs::create_dir(&target).expect("target");
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&target, &alias).expect("symlink");
        let resolved = resolve_data_root(DataRootRequest {
            source: DataRootSource::Cli,
            requested: Some(alias.clone()),
            home_dir: temp.path().to_path_buf(),
        })
        .expect("resolve alias");
        assert_eq!(resolved.requested, alias);
        assert_eq!(
            resolved.effective,
            target.canonicalize().expect("canonical target")
        );
        assert!(resolved.is_filesystem_alias);
    }

    #[cfg(windows)]
    #[test]
    fn reports_junction_requested_and_effective_roots() {
        let temp = tempfile::tempdir().expect("temp root");
        let target = temp.path().join("target");
        std::fs::create_dir(&target).expect("target");
        let alias = temp.path().join("alias");
        junction::create(&target, &alias).expect("junction");
        let resolved = resolve_data_root(DataRootRequest {
            source: DataRootSource::Cli,
            requested: Some(alias.clone()),
            home_dir: temp.path().to_path_buf(),
        })
        .expect("resolve alias");
        assert_eq!(resolved.requested, alias);
        assert_eq!(
            resolved.effective,
            target.canonicalize().expect("canonical target")
        );
        assert!(resolved.is_filesystem_alias);
    }
}
