use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::remote_update::RemoteUpdateInstallKind;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StaticDirSource {
    Explicit,
    Packaged,
}

impl StaticDirSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Packaged => "packaged",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedStaticDir {
    pub path: PathBuf,
    pub source: StaticDirSource,
    pub install_kind: RemoteUpdateInstallKind,
}

#[derive(Debug, Error)]
pub enum StaticDirError {
    #[error("explicit static directory does not contain index.html: {path}")]
    ExplicitInvalid { path: PathBuf },
}

fn is_valid_static_dir(path: &Path) -> bool {
    path.is_dir() && path.join("index.html").is_file()
}

pub fn resolve_static_dir(
    explicit: Option<&Path>,
    executable: &Path,
) -> Result<Option<ResolvedStaticDir>, StaticDirError> {
    if let Some(path) = explicit {
        if !is_valid_static_dir(path) {
            return Err(StaticDirError::ExplicitInvalid {
                path: path.to_path_buf(),
            });
        }
        return Ok(Some(ResolvedStaticDir {
            path: path.to_path_buf(),
            source: StaticDirSource::Explicit,
            install_kind: RemoteUpdateInstallKind::Unknown,
        }));
    }

    let Some(executable_directory) = executable.parent() else {
        return Ok(None);
    };
    let sibling = executable_directory.join("web");
    if is_valid_static_dir(&sibling) {
        return Ok(Some(ResolvedStaticDir {
            path: sibling,
            source: StaticDirSource::Packaged,
            install_kind: RemoteUpdateInstallKind::Archive,
        }));
    }

    let Some(prefix) = executable_directory.parent() else {
        return Ok(None);
    };
    let installed = prefix.join("share/coddy-studio/web");
    Ok(
        is_valid_static_dir(&installed).then_some(ResolvedStaticDir {
            path: installed,
            source: StaticDirSource::Packaged,
            install_kind: RemoteUpdateInstallKind::SystemPackage,
        }),
    )
}
