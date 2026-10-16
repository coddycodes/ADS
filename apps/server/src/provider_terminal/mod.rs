mod claude;
mod codex;
mod model;
mod opencode;
mod supervisor;

#[allow(unused_imports)]
pub(crate) use model::{
    TerminalAgentActivityAdmission, TerminalAgentActivityControl, TerminalAgentActivityObservation,
    TerminalAgentActivityObservationKind, TerminalAgentActivityState,
};

pub use claude::{
    CachedClaudeCapabilityProbe, ClaudeAdditiveHookAttestor, ClaudeCapabilities,
    ClaudeCapabilityProbeRunner, ClaudeExecutablePinner, ClaudeProbeOutput,
    ClaudeTerminalObserverFactory,
};
pub use codex::{
    CachedCodexCapabilityProbe, CodexCapabilities, CodexCapabilityProbeRunner, CodexHelperLaunch,
    CodexHelperLauncher, CodexHelperProcess, CodexProbeOutput, CodexRemoteClient,
    CodexRemoteClientFactory, CodexTerminalObserverFactory,
};
pub use model::{
    NativeTerminalCommand, PreparedTerminalLaunch, PreparedTerminalObserver,
    TerminalAgentActivityProviderEpochs, TerminalAgentActivityTransition,
    TerminalGenerationActivityPublisher, TerminalLaunchPreparation, TerminalLaunchPreparationInput,
    TerminalLaunchPreparer, TerminalObserverCancellationReason, TerminalObserverGeneration,
    TerminalObserverGenerationLease, TerminalObserverWorkerContext,
    TerminalObserverWorkerSpawnError,
};
pub use opencode::{
    CachedOpenCodeCapabilityProbe, OpenCodeCapabilities, OpenCodeCapabilityProbeRunner,
    OpenCodeEventStream, OpenCodeHelperLaunch, OpenCodeHelperLauncher, OpenCodeHelperProcess,
    OpenCodeHelperReady, OpenCodeProbeOutput, OpenCodeRemoteClient, OpenCodeRemoteClientFactory,
    OpenCodeTerminalObserverFactory,
};
pub use supervisor::{
    ProviderSettingsInventoryAuthority, ProviderTerminalActivitySupervisor,
    ProviderTerminalInventory, ProviderTerminalInventoryAuthority, ProviderTerminalInventoryEntry,
    ProviderTerminalObserverFactories, ProviderTerminalObserverFactory,
    ProviderTerminalObserverFactoryInput,
};
