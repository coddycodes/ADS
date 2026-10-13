use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};

use super::Platform;

pub(crate) const WINDOWS_LAUNCH_EXECUTABLE_EXTENSIONS: &[&str] =
    &["exe", "com", "cmd", "bat", "ps1"];

pub(crate) fn launch_executable_extensions(
    platform: Platform,
    windows_path_extensions: Option<&str>,
) -> Vec<String> {
    if platform == Platform::Unix {
        return vec![String::new()];
    }

    let configured = windows_path_extensions
        .into_iter()
        .flat_map(|extensions| extensions.split(';'))
        .map(str::trim)
        .filter(|extension| !extension.is_empty())
        .map(|extension| {
            if extension.starts_with('.') {
                extension.to_ascii_lowercase()
            } else {
                format!(".{}", extension.to_ascii_lowercase())
            }
        });
    let fallback = WINDOWS_LAUNCH_EXECUTABLE_EXTENSIONS
        .iter()
        .map(|extension| format!(".{extension}"));
    // Bare Windows commands must follow PATHEXT semantics. Checking the exact
    // name first can select npm's extensionless POSIX shim before its .cmd
    // launcher, which CreateProcess rejects with ERROR_BAD_EXE_FORMAT.
    let mut extensions: Vec<String> = Vec::new();
    for extension in configured.chain(fallback) {
        if !extensions
            .iter()
            .any(|candidate| candidate.eq_ignore_ascii_case(&extension))
        {
            extensions.push(extension);
        }
    }
    extensions
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LaunchProgram {
    program: PathBuf,
    prefix_args: Vec<OsString>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PreparedLaunch {
    pub(crate) program: PathBuf,
    pub(crate) args: Vec<OsString>,
    pub(crate) raw_windows_args: Option<OsString>,
}

impl LaunchProgram {
    pub(crate) fn prepare<I, S>(self, arguments: I) -> PreparedLaunch
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        PreparedLaunch {
            program: self.program,
            args: self
                .prefix_args
                .into_iter()
                .chain(
                    arguments
                        .into_iter()
                        .map(|argument| argument.as_ref().to_owned()),
                )
                .collect(),
            raw_windows_args: None,
        }
    }
}

pub(crate) fn wrap_launch_program(
    platform: Platform,
    executable: &Path,
) -> Result<LaunchProgram, String> {
    let extension = executable.extension().and_then(OsStr::to_str);
    if platform == Platform::Windows
        && extension.is_some_and(|extension| extension.eq_ignore_ascii_case("ps1"))
    {
        return Ok(LaunchProgram {
            program: PathBuf::from("powershell.exe"),
            prefix_args: [
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
            ]
            .into_iter()
            .map(OsString::from)
            .chain(std::iter::once(executable.as_os_str().to_owned()))
            .collect(),
        });
    }
    if is_windows_batch_executable(platform, executable) {
        if executable
            .as_os_str()
            .to_string_lossy()
            .chars()
            .any(char::is_control)
        {
            return Err(
                "Windows batch executable paths cannot contain control characters".to_owned(),
            );
        }
        return Ok(LaunchProgram {
            program: executable.to_path_buf(),
            prefix_args: Vec::new(),
        });
    }
    Ok(LaunchProgram {
        program: executable.to_path_buf(),
        prefix_args: Vec::new(),
    })
}

fn is_windows_batch_executable(platform: Platform, executable: &Path) -> bool {
    platform == Platform::Windows
        && executable
            .extension()
            .and_then(OsStr::to_str)
            .is_some_and(|extension| {
                extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
            })
}

pub(crate) fn wrap_windows_batch_command(
    platform: Platform,
    target: PreparedLaunch,
    environment: &BTreeMap<String, String>,
) -> Result<PreparedLaunch, String> {
    if !is_windows_batch_executable(platform, &target.program) {
        return Ok(target);
    }

    let command_processor = environment
        .iter()
        .find(|(key, value)| key.eq_ignore_ascii_case("ComSpec") && !value.trim().is_empty())
        .map(|(_, value)| value.as_str())
        .unwrap_or("cmd.exe");
    let raw_windows_args = make_windows_batch_command_line(&target.program, &target.args)?;

    Ok(PreparedLaunch {
        program: PathBuf::from(command_processor),
        args: vec![raw_windows_args.clone()],
        raw_windows_args: Some(raw_windows_args),
    })
}

fn make_windows_batch_command_line(
    script: &Path,
    arguments: &[OsString],
) -> Result<OsString, String> {
    let script = script.to_string_lossy();
    if script.contains('"') || script.ends_with('\\') {
        return Err(
            "Windows batch executable paths cannot contain quotes or end with a backslash"
                .to_owned(),
        );
    }

    let mut command = String::from("/e:ON /v:OFF /d /c \"\"");
    command.push_str(&script);
    command.push('"');
    for argument in arguments {
        command.push(' ');
        append_windows_batch_argument(&mut command, &argument.to_string_lossy())?;
    }
    command.push('"');
    Ok(command.into())
}

fn append_windows_batch_argument(command: &mut String, argument: &str) -> Result<(), String> {
    if argument
        .chars()
        .any(|character| matches!(character, '\r' | '\n'))
    {
        return Err("Windows batch executable arguments cannot contain line breaks".to_owned());
    }

    const SAFE_UNQUOTED: &str = r"#$*+-./:?@\_";
    let quote = argument.is_empty()
        || argument.ends_with('\\')
        || argument.chars().any(|character| {
            (character.is_ascii()
                && !(character.is_ascii_alphanumeric() || SAFE_UNQUOTED.contains(character)))
                || character.is_control()
        });
    if quote {
        command.push('"');
    }

    let mut backslashes = 0;
    for character in argument.chars() {
        if character == '\\' {
            backslashes += 1;
        } else {
            if character == '"' {
                command.extend(std::iter::repeat_n('\\', backslashes));
                command.push('"');
            } else if character == '%' {
                // Prevent `%VAR%` expansion with the same zero-length `%cd%`
                // substring used by Rust's hardened batch launcher.
                command.push_str("%%cd:~,%");
            }
            backslashes = 0;
        }
        command.push(character);
    }
    if quote {
        command.extend(std::iter::repeat_n('\\', backslashes));
        command.push('"');
    }
    Ok(())
}

pub(crate) fn locate_executable<E>(
    command: &str,
    cwd: Option<&Path>,
    search_path: Option<&OsStr>,
    extensions: &[E],
) -> Option<PathBuf>
where
    E: AsRef<str>,
{
    let command_path = Path::new(command);
    if command_path.is_absolute() {
        return command_path.is_file().then(|| command_path.to_path_buf());
    }
    if command_path.components().count() > 1 {
        let resolved = cwd?.join(command_path);
        return resolved.is_file().then_some(resolved);
    }

    search_path
        .into_iter()
        .flat_map(std::env::split_paths)
        .find_map(|directory| {
            let directory = if directory.is_absolute() {
                directory
            } else {
                cwd?.join(directory)
            };
            if command_path.extension().is_some() {
                let candidate = directory.join(command);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
            extensions.iter().find_map(|extension| {
                let extension = extension.as_ref().trim();
                let candidate = if extension.is_empty() {
                    directory.join(command)
                } else if extension.starts_with('.') {
                    directory.join(format!("{command}{extension}"))
                } else {
                    directory.join(format!("{command}.{extension}"))
                };
                candidate.is_file().then_some(candidate)
            })
        })
}

/// The path to name instead of a bare program, so that Rust can start the command with
/// `posix_spawn`.
///
/// Rust forks whenever a command names a program without a directory and also sets `PATH`
/// for its child, because `posix_spawnp` would search this process's `PATH` instead. A
/// fork copies the page tables of the whole server: about 140 ms per child from a 6 GB
/// host, paid by every Git command (#85). AppImage isolation sets that `PATH` for every
/// user-facing child. This searches the child's `PATH` the way `execvp` would: relative
/// entries against the child's directory, past candidates that fail the way `execvp`
/// moves past. It returns an absolute path, or `None` when there is nothing to resolve,
/// nothing is found, or a candidate fails the way `execvp` stops at; the bare name then
/// spawns as before and reports the same error.
#[cfg(unix)]
pub(crate) fn spawn_program(command: &std::process::Command) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;

    let program = Path::new(command.get_program());
    if program.as_os_str().is_empty() || program.components().count() != 1 || program.is_absolute()
    {
        return None;
    }
    let (_, search_path) = command.get_envs().find(|(name, _)| *name == "PATH")?;
    // The child changes directory before it executes, so a relative result would resolve twice.
    let directory = std::env::current_dir()
        .ok()?
        .join(command.get_current_dir().unwrap_or(Path::new("")));
    for entry in std::env::split_paths(search_path?) {
        let candidate = directory.join(entry).join(program);
        let path = std::ffi::CString::new(candidate.as_os_str().as_bytes()).ok()?;
        // SAFETY: `path` is a NUL-terminated string that outlives the call.
        if unsafe { libc::faccessat(libc::AT_FDCWD, path.as_ptr(), libc::X_OK, libc::AT_EACCESS) }
            == 0
        {
            // execve refuses a directory with EACCES, which execvp moves past.
            if candidate.is_file() {
                return Some(candidate);
            }
        } else if !execvp_moves_past(&std::io::Error::last_os_error()) {
            return None;
        }
    }
    None
}

#[cfg(not(unix))]
pub(crate) fn spawn_program(_command: &std::process::Command) -> Option<PathBuf> {
    None
}

/// Whether `execvp` would go on after this launch error: to the next `PATH` entry, or for
/// `ENOEXEC` to running the file through `/bin/sh`. Any other error ends its search.
#[cfg(unix)]
pub(crate) fn execvp_moves_past(error: &std::io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(
            libc::ENOENT
                | libc::EACCES
                | libc::ENOTDIR
                | libc::ESTALE
                | libc::ENODEV
                | libc::ETIMEDOUT
                | libc::ENOEXEC
        )
    )
}

#[cfg(not(unix))]
pub(crate) fn execvp_moves_past(_error: &std::io::Error) -> bool {
    false
}

/// A command named so that `posix_spawn` can start it (#85), with the bare-name command
/// to fall back to.
pub(crate) struct SpawnPlan {
    command: tokio::process::Command,
    fallback: Option<tokio::process::Command>,
}

impl SpawnPlan {
    /// Builds the command for `program` with `build`, which sets everything else: arguments,
    /// directory, environment and AppImage isolation. When that environment sets `PATH` for
    /// a bare program, the plan names the program's path in that `PATH` instead
    /// ([`spawn_program`]) and keeps the bare-name command as its fallback.
    pub(crate) fn new(program: &Path, build: impl Fn(&Path) -> tokio::process::Command) -> Self {
        let bare = build(program);
        match spawn_program(bare.as_std()) {
            Some(resolved) => Self {
                command: build(&resolved),
                fallback: Some(bare),
            },
            None => Self {
                command: bare,
                fallback: None,
            },
        }
    }

    /// Starts the plan with `start`: the command first, then the bare-name fallback when
    /// `start` failed to spawn the command the way `execvp` moves past, so the search and
    /// its error stay `execvp`'s. Those failures are immediate, so the fallback keeps the
    /// caller's time budget. `spawn_error` picks the spawn failure out of `start`'s error.
    pub(crate) async fn start<T, E, F>(
        self,
        mut start: impl FnMut(tokio::process::Command) -> F,
        spawn_error: impl Fn(&E) -> Option<&std::io::Error>,
    ) -> Result<T, E>
    where
        F: std::future::Future<Output = Result<T, E>>,
    {
        let result = start(self.command).await;
        match (result, self.fallback) {
            (Err(error), Some(fallback)) if spawn_error(&error).is_some_and(execvp_moves_past) => {
                start(fallback).await
            }
            (result, _) => result,
        }
    }
}

#[cfg(all(test, unix))]
mod spawn_program_tests {
    use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

    use super::spawn_program;

    fn file(path: &Path, mode: u32) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "#!/bin/sh\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn resolves_a_bare_name_against_the_childs_path_like_execvp() {
        let root = tempfile::tempdir().unwrap();
        file(&root.path().join("plain/tool"), 0o644);
        file(&root.path().join("relative/tool"), 0o755);
        file(&root.path().join("later/tool"), 0o755);
        let mut command = Command::new("tool");
        command
            .current_dir(root.path())
            .env("PATH", "plain:relative:later");

        // A file without an execute bit is skipped; a relative entry follows the child's cwd.
        assert_eq!(
            spawn_program(&command),
            Some(root.path().join("relative/tool"))
        );
    }

    #[test]
    fn skips_a_candidate_this_process_may_not_execute() {
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            return; // root may execute any file with an execute bit
        }
        let root = tempfile::tempdir().unwrap();
        // Executable by group and others, but not by its owner: this process.
        file(&root.path().join("denied/tool"), 0o011);
        file(&root.path().join("allowed/tool"), 0o755);
        let mut command = Command::new("tool");
        command
            .current_dir(root.path())
            .env("PATH", "denied:allowed");

        assert_eq!(
            spawn_program(&command),
            Some(root.path().join("allowed/tool"))
        );
    }

    #[test]
    fn leaves_the_search_to_execvp_after_an_error_it_would_stop_at() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("looping")).unwrap();
        std::os::unix::fs::symlink("tool", root.path().join("looping/tool")).unwrap();
        file(&root.path().join("later/tool"), 0o755);
        let mut command = Command::new("tool");
        command
            .current_dir(root.path())
            .env("PATH", "looping:later");

        // execvp stops at ELOOP instead of running the later tool.
        assert_eq!(spawn_program(&command), None);
    }

    #[test]
    fn returns_an_absolute_path_for_a_relative_child_directory() {
        let root = tempfile::tempdir().unwrap();
        file(&root.path().join("repo/bin/tool"), 0o755);
        // The same directory, spelled relative to this process's working directory.
        let current = std::env::current_dir().unwrap();
        let mut relative = std::path::PathBuf::new();
        for _ in current.components().skip(1) {
            relative.push("..");
        }
        relative.push(root.path().strip_prefix("/").unwrap().join("repo"));
        let mut command = Command::new("tool");
        command.current_dir(&relative).env("PATH", "bin");

        let resolved = spawn_program(&command).expect("resolved tool");
        assert!(resolved.is_absolute(), "{resolved:?}");
        assert_eq!(
            fs::canonicalize(resolved).unwrap(),
            fs::canonicalize(root.path().join("repo/bin/tool")).unwrap()
        );
    }

    #[test]
    fn leaves_commands_that_need_no_resolution_or_resolve_to_nothing() {
        let root = tempfile::tempdir().unwrap();
        file(&root.path().join("bin/tool"), 0o755);
        let path = root.path().join("bin");

        let inherited = Command::new("tool");
        let mut with_directory = Command::new("bin/tool");
        with_directory.current_dir(root.path()).env("PATH", &path);
        let mut removed = Command::new("tool");
        removed.env_remove("PATH");
        let mut missing = Command::new("absent-tool");
        missing.env("PATH", &path);

        for command in [inherited, with_directory, removed, missing] {
            assert_eq!(spawn_program(&command), None, "{command:?}");
        }
    }
}
