use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

/// The value a provider process sees for `name`: the instance environment first (matched
/// case-insensitively, as Windows does), then the server's own environment.
pub(crate) fn effective_environment_value(
    environment: &[(OsString, OsString)],
    name: &str,
) -> Option<OsString> {
    environment
        .iter()
        .find(|(candidate, _)| candidate.to_string_lossy().eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
        .or_else(|| std::env::var_os(name))
}

/// The configuration directory Claude Code uses under `environment`: `CLAUDE_CONFIG_DIR`, else
/// `.claude` in the home directory.
pub(crate) fn claude_config_directory(environment: &[(OsString, OsString)]) -> Option<PathBuf> {
    effective_environment_value(environment, "CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| home_directory(environment).map(|home| home.join(".claude")))
}

/// The home Codex uses under `environment` when Coddy Studio sets no `CODEX_HOME` for it:
/// `CODEX_HOME`, else `.codex` in the home directory.
pub(crate) fn codex_home_directory(environment: &[(OsString, OsString)]) -> Option<PathBuf> {
    effective_environment_value(environment, "CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| home_directory(environment).map(|home| home.join(".codex")))
}

fn home_directory(environment: &[(OsString, OsString)]) -> Option<PathBuf> {
    effective_environment_value(environment, "HOME")
        .or_else(|| effective_environment_value(environment, "USERPROFILE"))
        .map(PathBuf::from)
}

/// Claude Code shortens longer project directory names with a hash this cannot reproduce.
const CLAUDE_PROJECT_DIRECTORY_NAME_MAX: usize = 200;

/// The directory under `<config dir>/projects` where Claude Code keeps the transcripts of
/// conversations started in `cwd`: the path with every character other than an ASCII letter or
/// digit replaced by `-`, one per UTF-16 unit as Claude's JavaScript does. `None` for a name
/// Claude would shorten.
pub(crate) fn claude_project_directory_name(cwd: &Path) -> Option<String> {
    let name = cwd
        .to_string_lossy()
        .chars()
        .flat_map(|character| {
            let replaced = if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            };
            std::iter::repeat_n(replaced, character.len_utf16())
        })
        .collect::<String>();
    (name.len() <= CLAUDE_PROJECT_DIRECTORY_NAME_MAX).then_some(name)
}

/// Whether Claude Code can resume `session_id` in `cwd`, which needs its transcript
/// `<config dir>/projects/<project>/<session_id>.jsonl`. Claude looks in the project of `cwd`
/// (also checked resolved, because Claude records the real path), then in every project, as for
/// a conversation started in an earlier working directory. `None` when that cannot be told: no
/// config directory, or no readable `projects` directory in it.
pub(crate) fn claude_session_transcript_exists(
    environment: &[(OsString, OsString)],
    cwd: &Path,
    session_id: &str,
) -> Option<bool> {
    let projects = claude_config_directory(environment)?.join("projects");
    if !projects.is_dir() {
        return None;
    }
    let file_name = format!("{session_id}.jsonl");
    let mut candidates = vec![cwd.to_path_buf()];
    candidates.extend(std::fs::canonicalize(cwd).ok().filter(|real| real != cwd));
    for candidate in candidates {
        if let Some(name) = claude_project_directory_name(&candidate)
            && projects.join(name).join(&file_name).is_file()
        {
            return Some(true);
        }
    }
    let entries = std::fs::read_dir(&projects).ok()?;
    Some(
        entries
            .flatten()
            .any(|entry| entry.path().join(&file_name).is_file()),
    )
}

/// Isolate providers from host diagnostics and AppImage launcher paths.
pub(crate) fn sanitize_provider_subprocess_environment(command: &mut tokio::process::Command) {
    // Host diagnostics must not turn provider stderr into a high-volume event stream.
    command.env_remove("RUST_LOG");
    crate::process::isolate_appimage_environment(command);
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    use std::time::Duration;
    use std::{ffi::OsString, path::Path};

    use super::{claude_project_directory_name, claude_session_transcript_exists};

    #[test]
    fn claude_project_directories_replace_every_non_alphanumeric_character() {
        // Names observed under `~/.claude/projects` for these working directories.
        for (cwd, name) in [
            (
                "/work/workspaces/coddy-studio/CoddyStudio/main-3",
                "-work-workspaces-coddy-studio-CoddyStudio-main-3",
            ),
            (
                "/work/github/CoddyStudio/.claude/worktrees/agent-a6485f3e336f43353",
                "-work-github-CoddyStudio--claude-worktrees-agent-a6485f3e336f43353",
            ),
            (r"C:\Users\me\my_repo", "C--Users-me-my-repo"),
            // One `-` per UTF-16 unit, as Claude's JavaScript replaces them.
            ("/tmp/caf\u{e9}/\u{1f600}", "-tmp-caf----"),
        ] {
            assert_eq!(
                claude_project_directory_name(Path::new(cwd)).as_deref(),
                Some(name)
            );
        }
        let long = format!("/{}", "a".repeat(200));
        assert_eq!(claude_project_directory_name(Path::new(&long)), None);
    }

    #[test]
    fn claude_transcripts_are_found_under_the_working_directory_project() {
        let config = tempfile::tempdir().expect("claude config");
        let cwd = config.path().join("work tree");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let environment = [(
            OsString::from("CLAUDE_CONFIG_DIR"),
            config.path().as_os_str().to_owned(),
        )];
        // Without a projects directory Coddy Studio cannot tell, so it keeps resuming.
        assert_eq!(
            claude_session_transcript_exists(&environment, &cwd, "session"),
            None
        );
        let project = config
            .path()
            .join("projects")
            .join(claude_project_directory_name(&cwd).expect("short cwd"));
        std::fs::create_dir_all(&project).expect("project");
        assert_eq!(
            claude_session_transcript_exists(&environment, &cwd, "session"),
            Some(false)
        );
        std::fs::write(project.join("session.jsonl"), "{}\n").expect("transcript");
        assert_eq!(
            claude_session_transcript_exists(&environment, &cwd, "session"),
            Some(true)
        );
        assert_eq!(
            claude_session_transcript_exists(&environment, &cwd, "other"),
            Some(false)
        );
        // Claude also resumes a transcript recorded under another project, such as an earlier
        // working directory or another spelling of it.
        let elsewhere = config.path().join("projects").join("-earlier-cwd");
        std::fs::create_dir_all(&elsewhere).expect("other project");
        std::fs::write(elsewhere.join("other.jsonl"), "{}\n").expect("other transcript");
        assert_eq!(
            claude_session_transcript_exists(&environment, &cwd, "other"),
            Some(true)
        );
    }

    #[test]
    fn provider_commands_do_not_inherit_host_rust_logging() {
        let mut command = tokio::process::Command::new("provider-fixture");
        command.env("RUST_LOG", "info");

        super::sanitize_provider_subprocess_environment(&mut command);

        assert!(
            command
                .as_std()
                .get_envs()
                .any(|(name, value)| { name == "RUST_LOG" && value.is_none() })
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn provider_commands_ignore_appimage_environment() {
        use crate::test_support::appimage_environment::{
            ENVIRONMENT_CASES, assert_child_environment, check_inherited_environment,
        };

        check_inherited_environment(
            "provider::environment::tests::provider_commands_ignore_appimage_environment",
            ENVIRONMENT_CASES,
            |expected| {
                tokio::runtime::Runtime::new().unwrap().block_on(async {
                    let mut command = tokio::process::Command::new("/usr/bin/env");
                    command.arg("-0").kill_on_drop(true);
                    super::sanitize_provider_subprocess_environment(&mut command);
                    let output = tokio::time::timeout(Duration::from_secs(10), command.output())
                        .await
                        .expect("provider probe completed")
                        .expect("provider probe spawned");
                    assert!(output.status.success());
                    assert_child_environment(&output.stdout, expected);

                    // An effective environment must honor explicit removals and
                    // overrides instead of restoring the polluted parent values.
                    let mut command = tokio::process::Command::new("/usr/bin/env");
                    command.arg("-0").kill_on_drop(true);
                    command.env_remove("CODDY_STUDIO_TEST_FUTURE_PLUGIN_PATH");
                    command.env("PYTHONHOME", "/host/explicit-python");
                    super::sanitize_provider_subprocess_environment(&mut command);
                    let output = tokio::time::timeout(Duration::from_secs(10), command.output())
                        .await
                        .expect("provider override probe completed")
                        .expect("provider override probe spawned");
                    assert!(output.status.success());
                    let mut overridden = expected.clone();
                    overridden.insert("CODDY_STUDIO_TEST_FUTURE_PLUGIN_PATH", None);
                    overridden.insert("PYTHONHOME", Some("/host/explicit-python".into()));
                    assert_child_environment(&output.stdout, &overridden);
                });
            },
        );
    }
}
