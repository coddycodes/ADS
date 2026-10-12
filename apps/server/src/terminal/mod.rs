mod history;
mod input;
mod manager;
mod model;
mod osc;
mod pty;

pub(crate) use input::TerminalInputAdmission;
pub use input::TerminalInputError;
pub(crate) use manager::TerminalSessionIdentity;
pub use manager::{
    SubprocessInspection, TerminalAttachment, TerminalError, TerminalEventReceiver,
    TerminalManager, TerminalManagerOptions, TerminalMetadataAttachment,
    TerminalSubprocessInspector, WorktreeRemovalGuard,
};
pub use model::{
    ProviderTerminalActivityLaunch, TerminalAttachInput, TerminalConsoleTheme, TerminalEvent,
    TerminalLaunchCommand, TerminalMetadataEvent, TerminalOpenInput, TerminalRemovalReason,
    TerminalRestartInput, TerminalSessionSnapshot, TerminalSize, TerminalStatus, TerminalSummary,
};
pub use pty::{PortablePtyBackend, PtyBackend, PtyExit, PtyProcess, PtySpawnInput};
