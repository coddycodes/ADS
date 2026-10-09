mod attribution;
pub(crate) mod history;
mod model;
mod monitor;
mod native;
pub mod profiler;
mod registry;
mod resource_sampler;
mod trace;

pub use attribution::{
    AttributedProcess, AttributionConfidence, AttributionKind, AttributionScope,
    ProcessAttribution, ProcessAttributionTotals, ProcessClaim, ProcessResourceTotals,
    ResourceAttributor,
};
pub use history::{
    AttributedProcessSample, BucketMetric, ProcessResourceBucket, ProcessResourceHistory,
    ProcessResourceSummary, SplitMetric,
};
pub use model::{
    DescendantEntry, PROCESS_CLAIM_LABEL_MAX_SCALARS, PROCESS_COMMAND_MAX_SCALARS,
    ProcessDiagnosticsResult, ProcessIdentity, ProcessRow, ProcessTreeMetadata,
    bound_diagnostic_string, build_descendant_entries, build_process_tree_entries,
    process_tree_metadata,
};
pub use monitor::{CurrentProcessDiagnostics, DiagnosticsMonitor, ProcessSampler, SamplingError};
pub(crate) use native::RuntimeProcessOwnership;
pub use native::{NativeProcessRecord, NativeProcessSampler, ProcessSignal, SignalError};
pub(crate) use registry::ProcessRegistrationError;
pub use registry::{
    ProcessAttributionRegistry, ProcessRegistration, ProcessRegistrationMetadata,
    RegistrationSource,
};
pub use resource_sampler::{
    AttributedProcessSnapshot, DesktopUiObservation, DesktopUiProcessObserver,
    NativeResourceSampler, NotApplicableUiProcessObserver, ResourceSampler, UiCoverage,
    UiCoverageStatus, UnavailableDesktopUiProcessObserver, WebView2DesktopUiProcessObserver,
};
pub use trace::TraceDiagnosticsStore;
pub(crate) use trace::redact_sensitive_text;
