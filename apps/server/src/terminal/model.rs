use std::{collections::BTreeMap, path::PathBuf};

use serde::{Deserialize, Deserializer, Serialize};

pub(crate) const WINDOWS_CONSOLE_THEME_ENV: &str = "CODDY_STUDIO_WINDOWS_CONSOLE_THEME";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalConsoleTheme {
    Light,
    Dark,
}

pub(crate) fn terminal_console_theme_from_env(
    env: &BTreeMap<String, String>,
) -> Option<TerminalConsoleTheme> {
    let value = environment_value(env, WINDOWS_CONSOLE_THEME_ENV);
    match value.map(String::as_str) {
        Some("light") => Some(TerminalConsoleTheme::Light),
        Some("dark") => Some(TerminalConsoleTheme::Dark),
        _ => None,
    }
}

fn environment_value<'a>(env: &'a BTreeMap<String, String>, name: &str) -> Option<&'a String> {
    env.get(name).or_else(|| {
        env.iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    })
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalStatus {
    Starting,
    Running,
    Exited,
    Error,
}

impl TerminalStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Exited => "exited",
            Self::Error => "error",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProviderTerminalActivityLaunch {
    pub driver_kind: String,
    pub provider_instance_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalLaunchCommand {
    pub executable: String,
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    pub activity: Option<ProviderTerminalActivityLaunch>,
    /// Provider defaults the client merges into the spawn env; kept so other
    /// clients that adopt the terminal relaunch it the same way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
}

fn deserialize_optional_non_null<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Clone, Debug)]
pub struct TerminalOpenInput {
    pub thread_id: String,
    pub terminal_id: String,
    pub cwd: PathBuf,
    pub worktree_path: Option<PathBuf>,
    pub cols: u16,
    pub rows: u16,
    pub env: BTreeMap<String, String>,
    pub command: Option<TerminalLaunchCommand>,
    /// Marks a host thread's center panel; sticky on the session once set.
    pub center_panel: bool,
}

impl TerminalOpenInput {
    pub fn new(
        thread_id: impl Into<String>,
        terminal_id: impl Into<String>,
        cwd: PathBuf,
        cols: u16,
        rows: u16,
    ) -> Self {
        Self {
            thread_id: thread_id.into(),
            terminal_id: terminal_id.into(),
            cwd,
            worktree_path: None,
            cols,
            rows,
            env: BTreeMap::new(),
            command: None,
            center_panel: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct TerminalAttachInput {
    pub thread_id: String,
    pub terminal_id: String,
    pub cwd: Option<PathBuf>,
    pub worktree_path: Option<PathBuf>,
    pub cols: Option<u16>,
    pub rows: Option<u16>,
    pub env: BTreeMap<String, String>,
    pub restart_if_not_running: bool,
    pub command: Option<TerminalLaunchCommand>,
    pub size_claim: Option<String>,
    pub center_panel: bool,
}

impl TerminalAttachInput {
    pub fn existing(thread_id: impl Into<String>, terminal_id: impl Into<String>) -> Self {
        Self {
            thread_id: thread_id.into(),
            terminal_id: terminal_id.into(),
            cwd: None,
            worktree_path: None,
            cols: None,
            rows: None,
            env: BTreeMap::new(),
            restart_if_not_running: false,
            command: None,
            size_claim: None,
            center_panel: false,
        }
    }
}

pub type TerminalRestartInput = TerminalOpenInput;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSize {
    pub cols: u16,
    pub rows: u16,
    pub size_claim: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSessionSnapshot {
    pub thread_id: String,
    pub terminal_id: String,
    pub cwd: String,
    pub worktree_path: Option<String>,
    pub status: TerminalStatus,
    pub pid: Option<u32>,
    pub history: String,
    pub exit_code: Option<i32>,
    pub exit_signal: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub console_theme: Option<TerminalConsoleTheme>,
    pub label: String,
    pub updated_at: String,
    pub sequence: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<TerminalSize>,
    #[serde(default)]
    pub osc_color_responder_active: bool,
    #[serde(default)]
    pub first_attachment_grant: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSummary {
    pub thread_id: String,
    pub terminal_id: String,
    pub cwd: String,
    pub worktree_path: Option<String>,
    pub status: TerminalStatus,
    pub pid: Option<u32>,
    pub exit_code: Option<i32>,
    pub exit_signal: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub console_theme: Option<TerminalConsoleTheme>,
    pub has_running_subprocess: bool,
    pub label: String,
    pub updated_at: String,
    #[serde(default)]
    pub center_panel: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<TerminalLaunchCommand>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum TerminalEvent {
    Started {
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "terminalId")]
        terminal_id: String,
        sequence: u64,
        snapshot: TerminalSessionSnapshot,
    },
    Output {
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "terminalId")]
        terminal_id: String,
        sequence: u64,
        data: String,
    },
    Resized {
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "terminalId")]
        terminal_id: String,
        sequence: u64,
        size: TerminalSize,
    },
    Exited {
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "terminalId")]
        terminal_id: String,
        sequence: u64,
        #[serde(rename = "exitCode")]
        exit_code: Option<i32>,
        #[serde(rename = "exitSignal")]
        exit_signal: Option<i32>,
    },
    Closed {
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "terminalId")]
        terminal_id: String,
        sequence: u64,
    },
    Error {
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "terminalId")]
        terminal_id: String,
        sequence: u64,
        message: String,
    },
    Cleared {
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "terminalId")]
        terminal_id: String,
        sequence: u64,
    },
    Restarted {
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "terminalId")]
        terminal_id: String,
        sequence: u64,
        snapshot: TerminalSessionSnapshot,
    },
    Activity {
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "terminalId")]
        terminal_id: String,
        sequence: u64,
        #[serde(rename = "hasRunningSubprocess")]
        has_running_subprocess: bool,
        label: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_attachment_uses_owned_identifiers_and_empty_restart_metadata() {
        let input = TerminalAttachInput::existing("thread".to_owned(), "terminal".to_owned());
        assert_eq!(input.thread_id, "thread");
        assert_eq!(input.terminal_id, "terminal");
        assert!(input.cwd.is_none());
        assert!(!input.restart_if_not_running);
    }
}

impl TerminalEvent {
    pub const fn sequence(&self) -> u64 {
        match self {
            Self::Started { sequence, .. }
            | Self::Output { sequence, .. }
            | Self::Resized { sequence, .. }
            | Self::Exited { sequence, .. }
            | Self::Closed { sequence, .. }
            | Self::Error { sequence, .. }
            | Self::Cleared { sequence, .. }
            | Self::Restarted { sequence, .. }
            | Self::Activity { sequence, .. } => *sequence,
        }
    }

    pub fn belongs_to(&self, thread_id: &str, terminal_id: &str) -> bool {
        let (event_thread_id, event_terminal_id) = match self {
            Self::Started {
                thread_id,
                terminal_id,
                ..
            }
            | Self::Output {
                thread_id,
                terminal_id,
                ..
            }
            | Self::Resized {
                thread_id,
                terminal_id,
                ..
            }
            | Self::Exited {
                thread_id,
                terminal_id,
                ..
            }
            | Self::Closed {
                thread_id,
                terminal_id,
                ..
            }
            | Self::Error {
                thread_id,
                terminal_id,
                ..
            }
            | Self::Cleared {
                thread_id,
                terminal_id,
                ..
            }
            | Self::Restarted {
                thread_id,
                terminal_id,
                ..
            }
            | Self::Activity {
                thread_id,
                terminal_id,
                ..
            } => (thread_id, terminal_id),
        };
        event_thread_id == thread_id && event_terminal_id == terminal_id
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum TerminalMetadataEvent {
    Snapshot {
        terminals: Vec<TerminalSummary>,
    },
    Upsert {
        terminal: Box<TerminalSummary>,
    },
    Remove {
        #[serde(rename = "threadId")]
        thread_id: String,
        #[serde(rename = "terminalId")]
        terminal_id: String,
        reason: TerminalRemovalReason,
    },
}

/// Why a session left the metadata list. Only `Closed` means a client closed
/// it; clients keep center-panel tabs for the others.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TerminalRemovalReason {
    Closed,
    Restarted,
    Shutdown,
}
