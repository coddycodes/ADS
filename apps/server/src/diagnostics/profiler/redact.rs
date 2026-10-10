use std::{path::Path, sync::OnceLock};

use crate::{
    diagnostics::redact_sensitive_text,
    git::{HostPathPlatform, host_path_platform, normalize_worktree_path_key},
};

pub(super) const MESSAGE_MAX_BYTES: usize = 2048;

/// Replaces the user's home directory with `~` and applies secret redaction. The home
/// directory is replaced as `$HOME` spells it and as the filesystem resolves it, so a
/// symlinked home (`/home/u` to `/data/u`) cannot leak the user name through resolved paths.
pub fn redact_path(value: &str) -> String {
    static HOMES: OnceLock<Vec<String>> = OnceLock::new();
    let homes = HOMES.get_or_init(|| {
        dirs::home_dir().map_or_else(Vec::new, |home| spellings(&home, host_path_platform()))
    });
    redact_message(&replace_homes(value, homes))
}

/// `home` as given and as resolved, each also as a worktree identity key (on Windows,
/// uppercase with forward slashes: how the availability registry spells its paths), longest
/// first, so a shorter spelling never cuts into a longer one that contains it.
fn spellings(home: &Path, platform: HostPathPlatform) -> Vec<String> {
    let mut paths = vec![home.to_path_buf()];
    if let Ok(resolved) = std::fs::canonicalize(home) {
        paths.push(resolved);
    }
    let mut spellings: Vec<String> = paths
        .iter()
        .flat_map(|path| {
            [
                path.to_string_lossy().into_owned(),
                normalize_worktree_path_key(path, platform),
            ]
        })
        .filter(|spelling| !spelling.is_empty())
        .collect();
    spellings.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    spellings.dedup();
    spellings
}

fn replace_homes(value: &str, homes: &[String]) -> String {
    homes.iter().fold(value.to_owned(), |value, home| {
        value.replace(home.as_str(), "~")
    })
}

/// Keeps only the executable's file name; arguments may carry prompts or secrets.
pub fn redact_command(command: &str) -> String {
    let command = command.trim_start();
    let first = match command.strip_prefix(['"', '\'']) {
        // A quoted executable may contain spaces: take everything up to the closing quote.
        Some(quoted) => quoted.split(['"', '\'']).next().unwrap_or(""),
        None => command.split_whitespace().next().unwrap_or(""),
    };
    let name = std::path::Path::new(first)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    redact_message(&name)
}

/// Secret redaction, control-character stripping, and a 2 KiB cap.
pub fn redact_message(value: &str) -> String {
    let cleaned: String = redact_sensitive_text(value)
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect();
    let mut end = cleaned.len().min(MESSAGE_MAX_BYTES);
    while !cleaned.is_char_boundary(end) {
        end -= 1;
    }
    cleaned[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_directory_becomes_tilde() {
        let home = dirs::home_dir()
            .expect("home dir")
            .to_string_lossy()
            .into_owned();
        assert_eq!(redact_path(&format!("{home}/work/repo")), "~/work/repo");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_home_is_redacted_in_its_resolved_spelling_too() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("data/u");
        std::fs::create_dir_all(&real).unwrap();
        let home = dir.path().join("home-u");
        std::os::unix::fs::symlink(&real, &home).unwrap();
        let homes = spellings(&home, HostPathPlatform::Posix);
        let resolved = std::fs::canonicalize(&real).unwrap();
        for path in [home.join("work/repo"), resolved.join("work/repo")] {
            assert_eq!(
                replace_homes(&path.to_string_lossy(), &homes),
                "~/work/repo",
                "{homes:?}"
            );
        }
    }

    #[test]
    fn a_windows_home_is_redacted_in_its_identity_key_spelling_too() {
        let homes = spellings(Path::new(r"C:\Users\alice"), HostPathPlatform::Windows);
        for path in [
            r"C:\Users\alice\work\missing",
            "C:/USERS/ALICE/work/missing",
        ] {
            assert_eq!(
                replace_homes(path, &homes).replace('\\', "/"),
                "~/work/missing",
                "{homes:?}"
            );
        }
    }

    #[test]
    fn command_keeps_only_the_executable_name() {
        assert_eq!(
            redact_command("/usr/bin/claude --api-key sk-x prompt"),
            "claude"
        );
        assert_eq!(
            redact_command("\"/opt/My Tools/claude\" --api-key sk-x"),
            "claude"
        );
    }

    #[test]
    fn message_strips_ansi_escapes_and_caps_length() {
        assert!(!redact_message("\u{1b}[31mred\u{1b}[0m").contains('\u{1b}'));
        assert_eq!(redact_message(&"a".repeat(5000)).len(), 2048);
        let multibyte = redact_message(&"é".repeat(2000));
        assert!(multibyte.len() <= 2048 && multibyte.chars().all(|c| c == 'é'));
    }
}
