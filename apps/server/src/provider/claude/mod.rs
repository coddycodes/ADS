pub(crate) mod activity;
pub mod canonical;
pub(crate) mod failure;
pub(crate) mod hook_sink;
pub mod model;
pub mod protocol;
pub mod runtime;
pub(crate) mod transcript;
mod usage;

#[doc(hidden)]
pub use activity::{
    ClaudeActivityFixtureAdapter, ClaudeActivityInputSource, ClaudeActivityOutput,
    ClaudeActivityStateCounts,
};
pub use canonical::{CanonicalEvent, CanonicalEventTrace};
pub(crate) use protocol::ClaudeControlResponseFrame;
pub use protocol::{AssistantMessage, ClaudeMessage};
pub use runtime::{
    ClaudeControlRequest, ClaudePermissionMode, ClaudeProviderRuntime, Decision, LaunchRequest,
    LaunchRequestInput, PermissionRequestInput, ReconnectSnapshot, ResolvedUserInput, RuntimeMode,
    TurnInput, UserInputRequestInput,
};
#[doc(hidden)]
pub use transcript::{ClaudeTranscriptFixtureAdapter, ClaudeTranscriptFixtureOutput};
#[doc(hidden)]
pub use transcript::{ClaudeTranscriptReadFixtureOutput, ClaudeTranscriptReaderFixture};
