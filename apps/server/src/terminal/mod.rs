mod history;
mod input;
mod manager;
mod model;
mod osc;
mod pty;

pub use input::TerminalInputError;
pub(crate) use input::{InputReadmissionScope, TerminalInputAdmission};
pub(crate) use manager::TerminalSessionIdentity;
pub use manager::{
    AttachmentItem, SubprocessInspection, TerminalAttachment, TerminalError, TerminalEventReceiver,
    TerminalManager, TerminalManagerOptions, TerminalMetadataAttachment,
    TerminalSubprocessInspector, WorktreeRemovalGuard,
};
pub use model::{
    ProviderTerminalActivityLaunch, TerminalAttachInput, TerminalConsoleTheme, TerminalEvent,
    TerminalLaunchCommand, TerminalMetadataEvent, TerminalOpenInput, TerminalRemovalReason,
    TerminalRestartInput, TerminalSessionSnapshot, TerminalSize, TerminalStatus, TerminalSummary,
};
pub use pty::{PortablePtyBackend, PtyBackend, PtyExit, PtyProcess, PtySpawnInput};
