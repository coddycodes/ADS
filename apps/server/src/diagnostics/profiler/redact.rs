use crate::diagnostics::redact_sensitive_text;

const MESSAGE_MAX_BYTES: usize = 2048;

/// Replaces the user's home directory with `~` and applies secret redaction.
pub fn redact_path(value: &str) -> String {
    let home = dirs::home_dir().map(|h| h.to_string_lossy().into_owned());
    let replaced = match home.as_deref() {
        Some(home) if !home.is_empty() => value.replace(home, "~"),
        _ => value.to_owned(),
    };
    redact_message(&replaced)
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
