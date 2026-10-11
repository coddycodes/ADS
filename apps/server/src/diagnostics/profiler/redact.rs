use std::{path::Path, sync::OnceLock};

use crate::{
    diagnostics::redact_sensitive_text,
    git::{HostPathPlatform, host_path_platform, normalize_worktree_path_key},
};

pub(super) const MESSAGE_MAX_BYTES: usize = 2048;

/// The home directory's spellings, resolved once: resolving touches the filesystem.
static HOMES: OnceLock<Vec<String>> = OnceLock::new();

/// Replaces the user's home directory with `~` and applies secret redaction. The home
/// directory is replaced as `$HOME` spells it and as the filesystem resolves it, so a
/// symlinked home (`/home/u` to `/data/u`) cannot leak the user name through resolved paths.
/// The first call resolves it.
pub fn redact_path(value: &str) -> String {
    let homes = HOMES.get_or_init(|| {
        dirs::home_dir().map_or_else(Vec::new, |home| spellings(&home, host_path_platform()))
    });
    redact_message(&replace_homes(value, homes))
}

/// Whether the home directory's spellings were resolved already.
#[cfg(test)]
pub(super) fn homes_resolved() -> bool {
    HOMES.get().is_some()
}

/// `home` as given and as resolved, each also as a worktree identity key (on Windows,
/// uppercase with forward slashes: how the availability registry spells its paths), without
/// trailing separators, longest first, so a shorter spelling never cuts into a longer one
/// that contains it.
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
        .map(|spelling| spelling.trim_end_matches(['/', '\\']).to_owned())
        .filter(|spelling| !spelling.is_empty())
        .collect();
    spellings.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    spellings.dedup();
    spellings
}

/// Replaces each spelling where it names the home itself or a path inside it: no file name
/// may continue on either side of the match. `/home/u` therefore never matches inside
/// `/home/user2`, `/home/u.bak` or `/mnt/home/u`, while `"/home/u"`, `PATH=…:/home/u/bin`,
/// `/home/u/x:12` and the end of a sentence, `Cannot access /home/u.`, still do.
fn replace_homes(value: &str, homes: &[String]) -> String {
    homes.iter().fold(value.to_owned(), |value, home| {
        replace_whole_path(&value, home)
    })
}

fn replace_whole_path(value: &str, path: &str) -> String {
    let mut replaced = String::with_capacity(value.len());
    let mut copied = 0;
    for (start, _) in value.match_indices(path) {
        let end = start + path.len();
        let before = value[..start].chars().next_back();
        if before.is_some_and(|c| c.is_alphanumeric() || is_name_punctuation(c))
            || name_continues(&value[end..])
        {
            continue;
        }
        replaced.push_str(&value[copied..start]);
        replaced.push('~');
        copied = end;
    }
    replaced.push_str(&value[copied..]);
    replaced
}

/// Whether `rest`, the text right after a match, continues the file name the match ends in:
/// a letter or digit, possibly after name punctuation (`/home/u.bak`, `/home/u-old`).
/// Punctuation that ends the text or a sentence (`Cannot access /home/u.`) does not.
fn name_continues(rest: &str) -> bool {
    rest.chars()
        .find(|&c| !is_name_punctuation(c))
        .is_some_and(char::is_alphanumeric)
}

/// Punctuation that file names commonly hold. Separators, whitespace and quotes are not.
fn is_name_punctuation(c: char) -> bool {
    matches!(c, '.' | '-' | '_' | '+' | '@' | '%' | '~')
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
        // Another user's directory beside the resolved home.
        let sibling = resolved.with_file_name("user2").join("repo");
        let sibling = sibling.to_string_lossy();
        assert_eq!(replace_homes(&sibling, &homes), sibling, "{homes:?}");
    }

    #[test]
    fn a_home_matches_only_as_a_whole_path() {
        let homes = spellings(Path::new("/home/u"), HostPathPlatform::Posix);
        for (value, redacted) in [
            ("/home/u", "~"),
            ("/home/u/work/repo", "~/work/repo"),
            ("\"/home/u\" is missing", "\"~\" is missing"),
            ("at /home/u/src/main.rs:12", "at ~/src/main.rs:12"),
            ("PATH=/usr/bin:/home/u/bin", "PATH=/usr/bin:~/bin"),
            ("cwd=/home/u", "cwd=~"),
            // Punctuation ending a sentence or a parenthesis is not part of the name.
            ("Cannot access /home/u.", "Cannot access ~."),
            ("(see /home/u).", "(see ~)."),
            ("/home/u... and more", "~... and more"),
            // Another user's directory, a sibling, and the home's spelling inside a longer path.
            ("/home/user2/repo", "/home/user2/repo"),
            ("/home/u.bak/repo", "/home/u.bak/repo"),
            ("/home/u-old", "/home/u-old"),
            ("/mnt/home/u/repo", "/mnt/home/u/repo"),
        ] {
            assert_eq!(replace_homes(value, &homes), redacted, "{value}");
        }
    }

    #[test]
    fn a_trailing_separator_in_home_does_not_widen_the_match() {
        let homes = spellings(Path::new("/home/u/"), HostPathPlatform::Posix);
        assert_eq!(replace_homes("/home/u", &homes), "~");
        assert_eq!(replace_homes("/home/u/x", &homes), "~/x");
        assert_eq!(replace_homes("/home/user2/x", &homes), "/home/user2/x");
    }

    #[test]
    fn a_windows_home_is_redacted_in_its_identity_key_spelling_too() {
        let homes = spellings(Path::new(r"C:\Users\al"), HostPathPlatform::Windows);
        for (value, redacted) in [
            (r"C:\Users\al\work\missing", r"~\work\missing"),
            ("C:/USERS/AL/work/missing", "~/work/missing"),
            (r"\\?\C:\Users\al\work", r"\\?\~\work"),
            // Another user whose name starts with this one's.
            (r"C:\Users\alice\work", r"C:\Users\alice\work"),
            ("C:/USERS/ALICE/work", "C:/USERS/ALICE/work"),
        ] {
            assert_eq!(replace_homes(value, &homes), redacted, "{homes:?}");
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
