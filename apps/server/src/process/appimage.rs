/// Whether a standard/Tokio command inherits the parent environment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EnvironmentInheritance {
    Inherit,
    Cleared,
}

/// Adapts the supported command builders without owning or spawning a process.
/// PTYs already hold their complete environment, so inheritance only applies to
/// standard/Tokio commands.
pub enum ChildCommand<'a> {
    Process {
        command: &'a mut std::process::Command,
        inheritance: EnvironmentInheritance,
    },
    Pty(&'a mut portable_pty::CommandBuilder),
}

/// Uses `Inherit`; see the warning on [`isolate_appimage_environment`].
impl<'a> From<&'a mut std::process::Command> for ChildCommand<'a> {
    fn from(command: &'a mut std::process::Command) -> Self {
        Self::Process {
            command,
            inheritance: EnvironmentInheritance::Inherit,
        }
    }
}

/// Uses `Inherit`; see the warning on [`isolate_appimage_environment`].
impl<'a> From<&'a mut tokio::process::Command> for ChildCommand<'a> {
    fn from(command: &'a mut tokio::process::Command) -> Self {
        Self::Process {
            command: command.as_std_mut(),
            inheritance: EnvironmentInheritance::Inherit,
        }
    }
}

impl<'a> From<&'a mut portable_pty::CommandBuilder> for ChildCommand<'a> {
    fn from(command: &'a mut portable_pty::CommandBuilder) -> Self {
        Self::Pty(command)
    }
}

/// Isolate a user-facing child from AppImage launcher state, after configuring
/// its environment. Never call for Coddy Studio binaries or the desktop/WebKitGTK
/// process itself. A no-op off Linux; does not change process-global state.
///
/// # Warning
///
/// After `env_clear()`, pass `ChildCommand::Process` with
/// `inheritance: EnvironmentInheritance::Cleared` and the standard command builder
/// (`command.as_std_mut()` for Tokio). The default `Inherit` conversion would
/// evaluate the parent environment.
pub fn isolate_appimage_environment<'a>(command: impl Into<ChildCommand<'a>>) {
    #[cfg(target_os = "linux")]
    linux::isolate(command.into());
    #[cfg(not(target_os = "linux"))]
    let _ = command;
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{ChildCommand, EnvironmentInheritance};
    use std::{
        collections::BTreeMap,
        ffi::{OsStr, OsString},
        os::unix::ffi::OsStrExt,
        path::{Path, PathBuf},
    };

    /// Launcher markers and forced settings removed from user-facing children.
    ///
    /// These children do not consume AppImage markers: Git disables askpass, and
    /// desktop SSH askpass is a temporary script, not the AppImage binary. The gate
    /// reads the pre-isolation environment, so child marker removal leaves the
    /// parent environment and future spawn decisions unchanged.
    ///
    /// linuxdeploy's AppRun.wrapped C runtime forces PYTHONDONTWRITEBYTECODE; its
    /// packaged GTK hook (apprun-hooks/linuxdeploy-plugin-gtk.sh) overwrites
    /// GDK_BACKEND, but exports GTK_THEME only for a nonempty user-supplied
    /// APPIMAGE_GTK_THEME. An inherited GTK_THEME otherwise passes through.
    /// GTK_PATH is also overwritten with bundled and host paths, not
    /// appended to the user value: even its surviving host directories must be removed.
    /// Remove these settings, including inherited or overridden GTK_THEME, from
    /// user-facing children so shells can reapply their rc settings as in SSH.
    const LAUNCHER_VARIABLES: [&str; 8] = [
        "APPDIR",
        "APPIMAGE",
        "ARGV0",
        "OWD",
        "GTK_PATH",
        "GTK_THEME",
        "GDK_BACKEND",
        "PYTHONDONTWRITEBYTECODE",
    ];

    /// Child-only overrides for user-facing commands, evaluated from their effective
    /// environment. Never mutate std::env: the desktop and its WebKitGTK children,
    /// and Coddy Studio's own binaries, need the bundled environment.
    pub(crate) fn child_environment_overrides(
        environment: &BTreeMap<OsString, OsString>,
        appdir: &Path,
    ) -> Vec<(OsString, Option<OsString>)> {
        // Both launcher layers keep adding paths (Python, Qt, GStreamer, GTK), so
        // inspect every value instead of maintaining a name list.
        let mut overrides = Vec::new();
        for (name, value) in environment {
            if name
                .to_str()
                .is_some_and(|name| LAUNCHER_VARIABLES.contains(&name))
            {
                overrides.push((name.clone(), None));
                continue;
            }
            let mut removed = false;
            let host_paths: Vec<_> = value
                .as_bytes()
                .split(|byte| {
                    // glibc accepts semicolons in library paths, and spaces in
                    // preloads. Other path lists use colons, preserving spaces.
                    *byte == b':'
                        || (name == "LD_LIBRARY_PATH" && *byte == b';')
                        || (name == "LD_PRELOAD" && *byte == b' ')
                })
                .map(|entry| Path::new(OsStr::from_bytes(entry)))
                .filter(|path| {
                    // Include extracted AppDirs and stale mounts from prior versions,
                    // but keep host paths that only share the AppDir's name prefix.
                    let bundled = path.starts_with(appdir)
                        || (path.is_absolute()
                            && path.components().any(|component| {
                                component.as_os_str().as_bytes().starts_with(b".mount_")
                            }));
                    removed |= bundled;
                    !bundled
                })
                .collect();
            if !removed {
                continue;
            }
            // AppRun appends an empty original as a trailing separator. Remove
            // empty-only survivors so children neither search cwd for libraries
            // nor disable system plugins with an empty GStreamer search path.
            let value = if host_paths.iter().all(|path| path.as_os_str().is_empty()) {
                None
            } else {
                Some(std::env::join_paths(host_paths).expect("split Unix paths can be rejoined"))
            };
            overrides.push((name.clone(), value));
        }
        overrides
    }

    fn appimage_root(
        variable: impl Fn(&str) -> Option<OsString>,
        current_executable: impl FnOnce() -> Option<PathBuf>,
    ) -> Option<PathBuf> {
        let appdir = PathBuf::from(variable("APPDIR")?);
        if !appdir.is_absolute() || appdir.parent().is_none() {
            return None;
        }
        // Extracted AppRun launches have no APPIMAGE. Resolve the executable
        // only for this fallback, and inspect AppRun exactly once per command.
        if variable("APPIMAGE").as_deref().is_none_or(OsStr::is_empty)
            && (!current_executable().is_some_and(|path| path.starts_with(&appdir))
                || !appdir.join("AppRun").exists())
        {
            return None;
        }
        Some(appdir)
    }

    pub(super) fn isolate(mut command: ChildCommand<'_>) {
        // The single gate does cheap lookups before copying an environment.
        let Some(appdir) = appimage_root(
            |name| command.variable(name),
            || std::env::current_exe().ok(),
        ) else {
            return;
        };
        let environment = command.environment();
        for (name, value) in child_environment_overrides(&environment, &appdir) {
            command.apply_override(name, value);
        }
    }

    impl ChildCommand<'_> {
        fn apply_override(&mut self, name: OsString, value: Option<OsString>) {
            match self {
                Self::Process { command, .. } => {
                    match value {
                        Some(value) => command.env(name, value),
                        None => command.env_remove(name),
                    };
                }
                Self::Pty(command) => match value {
                    Some(value) => command.env(name, value),
                    None => command.env_remove(name),
                },
            }
        }

        fn variable(&self, name: &str) -> Option<OsString> {
            match self {
                Self::Process {
                    command,
                    inheritance,
                } => match command.get_envs().find(|(key, _)| *key == name) {
                    Some((_, value)) => value.map(OsStr::to_owned),
                    None if *inheritance == EnvironmentInheritance::Inherit => {
                        std::env::var_os(name)
                    }
                    None => None,
                },
                Self::Pty(command) => command.get_env(name).map(OsStr::to_owned),
            }
        }

        fn environment(&self) -> BTreeMap<OsString, OsString> {
            match self {
                Self::Process {
                    command,
                    inheritance,
                } => {
                    let mut environment = if *inheritance == EnvironmentInheritance::Inherit {
                        std::env::vars_os().collect::<BTreeMap<_, _>>()
                    } else {
                        BTreeMap::new()
                    };
                    for (name, value) in command.get_envs() {
                        match value {
                            Some(value) => environment.insert(name.to_owned(), value.to_owned()),
                            None => environment.remove(name),
                        };
                    }
                    environment
                }
                Self::Pty(command) => command
                    .iter_full_env()
                    .map(|(name, value)| (name.to_owned(), value.to_owned()))
                    .collect(),
            }
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn pty_isolation_includes_non_utf8_command_overrides() {
            use std::os::unix::ffi::OsStringExt;

            let mut command = portable_pty::CommandBuilder::new("/usr/bin/env");
            command.env_clear();
            command.env("APPDIR", "/tmp/coddy-studio-raw-appdir");
            command.env("APPIMAGE", "/opt/coddy-studio.AppImage");
            let name = OsString::from_vec(b"CODDY_STUDIO_RAW_\xff".to_vec());
            command.env(
                &name,
                OsString::from_vec(b"/tmp/coddy-studio-raw-appdir/usr/lib:/host/\xff".to_vec()),
            );
            super::super::isolate_appimage_environment(&mut command);
            assert_eq!(
                command.get_env(&name),
                Some(OsStr::from_bytes(b"/host/\xff"))
            );
        }

        fn gated_overrides(
            environment: &BTreeMap<OsString, OsString>,
            executable: Option<&Path>,
        ) -> Vec<(OsString, Option<OsString>)> {
            appimage_root(
                |name| environment.get(OsStr::new(name)).cloned(),
                || executable.map(Path::to_path_buf),
            )
            .map(|root| child_environment_overrides(environment, &root))
            .unwrap_or_default()
        }

        fn environment(appdir: &Path, appimage: Option<&str>) -> BTreeMap<OsString, OsString> {
            let mut environment = BTreeMap::from([
                ("APPDIR".into(), appdir.as_os_str().to_owned()),
                ("PYTHONHOME".into(), appdir.join("usr").into_os_string()),
            ]);
            if let Some(appimage) = appimage {
                environment.insert("APPIMAGE".into(), appimage.into());
            }
            environment
        }

        #[test]
        fn extracted_appdir_isolation_requires_launcher_and_executable_identity() {
            let fixture = tempfile::tempdir().unwrap();
            let appdir = fixture.path().join("squashfs-root");
            std::fs::create_dir(&appdir).unwrap();
            std::fs::write(appdir.join("AppRun"), "launcher").unwrap();
            let executable = appdir.join("usr/bin/coddy-studio-desktop");
            for marker in [None, Some("")] {
                let environment = environment(&appdir, marker);
                let overrides = gated_overrides(&environment, Some(&executable));
                assert!(
                    overrides
                        .iter()
                        .any(|(name, value)| name == "PYTHONHOME" && value.is_none()),
                    "extracted AppRun must isolate Python without APPIMAGE"
                );
                for outside in [
                    fixture.path().join("coddy-studio"),
                    fixture.path().join("squashfs-root-host/bin/coddy-studio"),
                ] {
                    assert!(gated_overrides(&environment, Some(&outside)).is_empty());
                }
                assert!(gated_overrides(&environment, None).is_empty());
            }
            std::fs::remove_file(appdir.join("AppRun")).unwrap();
            assert!(
                gated_overrides(&environment(&appdir, None), Some(&executable)).is_empty(),
                "stray APPDIR without AppRun must not strip an installed server's paths"
            );
        }

        #[test]
        fn appimage_marker_keeps_existing_gate_independent_of_executable_location() {
            let fixture = tempfile::tempdir().unwrap();
            let environment = environment(fixture.path(), Some("/opt/coddy-studio.AppImage"));
            assert!(!gated_overrides(&environment, None).is_empty());
            assert!(
                !gated_overrides(&environment, Some(Path::new("/usr/bin/coddy-studio"))).is_empty()
            );
            for invalid in ["", "/", "relative/AppDir"] {
                let mut environment = environment.clone();
                environment.insert("APPDIR".into(), invalid.into());
                assert!(
                    gated_overrides(&environment, Some(Path::new("/usr/bin/coddy-studio")))
                        .is_empty()
                );
            }
        }

        #[test]
        fn cleared_command_does_not_reintroduce_ambient_variables() {
            use crate::test_support::appimage_environment::check_inherited_environment;
            const TEST: &str = "process::appimage::linux::tests::cleared_command_does_not_reintroduce_ambient_variables";
            check_inherited_environment(TEST, &["mixed"], |_| {
                let mut command = std::process::Command::new("/usr/bin/env");
                command.env_clear().arg("-0");
                // Keep the gate ON from explicit values, while every other
                // inherited fixture variable must stay out of this command.
                command.env("APPDIR", std::env::var_os("APPDIR").unwrap());
                command.env("APPIMAGE", std::env::var_os("APPIMAGE").unwrap());
                command.env("CODDY_STUDIO_EXPLICIT_ONLY", "yes");
                super::super::isolate_appimage_environment(ChildCommand::Process {
                    command: &mut command,
                    inheritance: EnvironmentInheritance::Cleared,
                });
                let output = command.output().unwrap();
                assert!(output.status.success());
                assert_eq!(output.stdout, b"CODDY_STUDIO_EXPLICIT_ONLY=yes\0");
            });
        }
    }
}
