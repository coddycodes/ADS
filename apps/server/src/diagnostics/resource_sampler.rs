use std::{
    collections::{HashMap, HashSet},
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use time::OffsetDateTime;

use super::{
    AttributedProcess, AttributionKind, AttributionScope, NativeProcessSampler,
    ProcessAttributionRegistry, ProcessAttributionTotals, ProcessClaim, ProcessIdentity,
    ProcessRow, ProcessSignal, ResourceAttributor, SamplingError, SignalError,
    bound_diagnostic_string,
};

const UI_OBSERVATION_TIMEOUT: Duration = Duration::from_millis(250);
const SLOW_SAMPLE: Duration = Duration::from_millis(500);
const UI_IDENTITY_LIMIT: usize = 64;
const UI_COVERAGE_MESSAGE_MAX_SCALARS: usize = 160;
const UI_UNAVAILABLE_MESSAGE: &str =
    "Native server usage is included, but local UI/WebView usage could not be associated reliably.";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UiCoverageStatus {
    Available,
    Partial,
    Unavailable,
    NotApplicable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UiCoverage {
    pub status: UiCoverageStatus,
    pub message: Option<String>,
}

impl Default for UiCoverage {
    fn default() -> Self {
        Self {
            status: UiCoverageStatus::Unavailable,
            message: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct DesktopUiObservation {
    pub identities: Vec<ProcessIdentity>,
    pub coverage: UiCoverage,
}

pub trait DesktopUiProcessObserver: std::fmt::Debug + Send + Sync + 'static {
    fn observe(
        &self,
        rows: Arc<[ProcessRow]>,
        server_identity: ProcessIdentity,
    ) -> Pin<Box<dyn Future<Output = DesktopUiObservation> + Send + '_>>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct UnavailableDesktopUiProcessObserver;

impl DesktopUiProcessObserver for UnavailableDesktopUiProcessObserver {
    fn observe(
        &self,
        _rows: Arc<[ProcessRow]>,
        _server_identity: ProcessIdentity,
    ) -> Pin<Box<dyn Future<Output = DesktopUiObservation> + Send + '_>> {
        Box::pin(async {
            DesktopUiObservation {
                identities: Vec::new(),
                coverage: unavailable_coverage(UI_UNAVAILABLE_MESSAGE),
            }
        })
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NotApplicableUiProcessObserver;

impl DesktopUiProcessObserver for NotApplicableUiProcessObserver {
    fn observe(
        &self,
        _rows: Arc<[ProcessRow]>,
        _server_identity: ProcessIdentity,
    ) -> Pin<Box<dyn Future<Output = DesktopUiObservation> + Send + '_>> {
        Box::pin(async {
            DesktopUiObservation {
                identities: Vec::new(),
                coverage: UiCoverage {
                    status: UiCoverageStatus::NotApplicable,
                    message: None,
                },
            }
        })
    }
}

#[derive(Clone, Debug)]
pub struct WebView2DesktopUiProcessObserver {
    host_executable_argument: String,
}

impl WebView2DesktopUiProcessObserver {
    #[must_use]
    pub fn new(host_executable_name: impl AsRef<str>) -> Self {
        Self {
            host_executable_argument: format!(
                "--webview-exe-name={}",
                host_executable_name.as_ref()
            ),
        }
    }
}

impl DesktopUiProcessObserver for WebView2DesktopUiProcessObserver {
    fn observe(
        &self,
        rows: Arc<[ProcessRow]>,
        server_identity: ProcessIdentity,
    ) -> Pin<Box<dyn Future<Output = DesktopUiObservation> + Send + '_>> {
        Box::pin(async move {
            let identities = rows
                .iter()
                .filter(|row| {
                    has_current_server_ancestry(
                        &rows,
                        server_identity,
                        ProcessIdentity {
                            pid: row.pid,
                            started_at: row.started_at,
                        },
                    ) && webview2_command_matches_host(&row.command, &self.host_executable_argument)
                })
                .map(|row| ProcessIdentity {
                    pid: row.pid,
                    started_at: row.started_at,
                })
                .collect::<Vec<_>>();
            let coverage = if identities.is_empty() {
                unavailable_coverage(UI_UNAVAILABLE_MESSAGE)
            } else {
                UiCoverage {
                    status: UiCoverageStatus::Available,
                    message: None,
                }
            };

            DesktopUiObservation {
                identities,
                coverage,
            }
        })
    }
}

fn webview2_command_matches_host(command: &str, host_executable_argument: &str) -> bool {
    let mut has_embedded_webview = false;
    let mut has_matching_host = false;
    for argument in command.split_ascii_whitespace() {
        has_embedded_webview |= argument.eq_ignore_ascii_case("--embedded-browser-webview=1");
        has_matching_host |= argument.eq_ignore_ascii_case(host_executable_argument);
    }
    has_embedded_webview && has_matching_host
}

#[derive(Clone, Debug, PartialEq)]
pub struct AttributedProcessSnapshot {
    pub sampled_at_ms: i128,
    pub server_identity: ProcessIdentity,
    pub native_rows: Arc<[ProcessRow]>,
    pub processes: Vec<AttributedProcess>,
    pub totals: ProcessAttributionTotals,
    pub ui_coverage: UiCoverage,
}

pub trait ResourceSampler: std::fmt::Debug + Send + Sync + 'static {
    fn sample(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<AttributedProcessSnapshot, SamplingError>> + Send + '_>>;
}

pub(crate) trait NativeProcessRowSource: std::fmt::Debug + Send + Sync + 'static {
    fn collect_rows(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ProcessRow>, SamplingError>> + Send + '_>>;

    fn signal_process(
        &self,
        expected_identity: ProcessIdentity,
        signal: ProcessSignal,
    ) -> Result<(), SignalError>;
}

impl NativeProcessRowSource for NativeProcessSampler {
    fn collect_rows(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ProcessRow>, SamplingError>> + Send + '_>> {
        NativeProcessSampler::collect_rows(self)
    }

    fn signal_process(
        &self,
        expected_identity: ProcessIdentity,
        signal: ProcessSignal,
    ) -> Result<(), SignalError> {
        NativeProcessSampler::signal_process(self, expected_identity, signal)
    }
}

#[derive(Debug)]
pub struct NativeResourceSampler {
    native: Arc<dyn NativeProcessRowSource>,
    registry: ProcessAttributionRegistry,
    ui_observer: Arc<dyn DesktopUiProcessObserver>,
}

impl NativeResourceSampler {
    #[must_use]
    pub fn new(
        native: Arc<NativeProcessSampler>,
        registry: ProcessAttributionRegistry,
        ui_observer: Arc<dyn DesktopUiProcessObserver>,
    ) -> Self {
        Self::with_native_process_source(native, registry, ui_observer)
    }

    fn with_native_process_source(
        native: Arc<dyn NativeProcessRowSource>,
        registry: ProcessAttributionRegistry,
        ui_observer: Arc<dyn DesktopUiProcessObserver>,
    ) -> Self {
        Self {
            native,
            registry,
            ui_observer,
        }
    }

    pub async fn signal_external_descendant(
        &self,
        expected_identity: ProcessIdentity,
        signal: ProcessSignal,
    ) -> Result<(), SignalError> {
        let sample_started_at = Instant::now();
        let rows: Arc<[ProcessRow]> = self
            .native
            .collect_rows()
            .await
            .map_err(|error| SignalError::Read(error.to_string()))?
            .into();
        let server_identity = rows
            .iter()
            .find(|row| row.pid == std::process::id())
            .map(|row| ProcessIdentity {
                pid: row.pid,
                started_at: row.started_at,
            })
            .ok_or_else(|| {
                SignalError::Read("current server process is absent from native rows".to_owned())
            })?;
        let observation =
            observe_ui_processes(self.ui_observer.clone(), rows.clone(), server_identity).await;
        let mut claims = self.registry.bind_and_snapshot(&rows, sample_started_at);
        append_ui_claims(&mut claims, &rows, &observation.identities);
        let attribution =
            ResourceAttributor::attribute(&rows, server_identity, &claims, observation.coverage);
        let Some(target_row) = rows.iter().find(|row| row.pid == expected_identity.pid) else {
            return Err(SignalError::NotFound(expected_identity.pid));
        };
        if target_row.started_at != expected_identity.started_at {
            return Err(SignalError::StaleIdentity(expected_identity.pid));
        }
        if expected_identity == server_identity {
            return Err(SignalError::NotEligible(expected_identity.pid));
        }
        if !has_current_server_ancestry(&rows, server_identity, expected_identity) {
            return Err(SignalError::NotDescendant(expected_identity.pid));
        }
        let eligible = attribution.processes.iter().any(|process| {
            process.identity == expected_identity && process.scope == AttributionScope::External
        });
        if !eligible {
            return Err(SignalError::NotEligible(expected_identity.pid));
        }

        self.native.signal_process(expected_identity, signal)
    }
}

impl ResourceSampler for NativeResourceSampler {
    fn sample(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<AttributedProcessSnapshot, SamplingError>> + Send + '_>>
    {
        Box::pin(async move {
            let sample_started_at = Instant::now();
            let rows: Arc<[ProcessRow]> = self.native.collect_rows().await?.into();
            let enumerated_at = Instant::now();
            let server_identity = rows
                .iter()
                .find(|row| row.pid == std::process::id())
                .map(|row| ProcessIdentity {
                    pid: row.pid,
                    started_at: row.started_at,
                })
                .ok_or_else(|| {
                    SamplingError::Failed(
                        "current server process is absent from native rows".into(),
                    )
                })?;
            let observation =
                observe_ui_processes(self.ui_observer.clone(), rows.clone(), server_identity).await;
            let observed_at = Instant::now();
            let mut claims = self.registry.bind_and_snapshot(&rows, sample_started_at);
            append_ui_claims(&mut claims, &rows, &observation.identities);
            let attribution = ResourceAttributor::attribute(
                &rows,
                server_identity,
                &claims,
                observation.coverage,
            );
            log_sample_stages(rows.len(), sample_started_at, enumerated_at, observed_at);

            Ok(AttributedProcessSnapshot {
                sampled_at_ms: OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000,
                server_identity,
                native_rows: rows,
                processes: attribution.processes,
                totals: attribution.totals,
                ui_coverage: attribution.ui_coverage,
            })
        })
    }
}

/// Names the slow stage of a sample: enumeration includes waiting for the blocking pool and the
/// native process-table lock, and the UI observer is capped at `UI_OBSERVATION_TIMEOUT`.
fn log_sample_stages(
    rows: usize,
    started_at: Instant,
    enumerated_at: Instant,
    observed_at: Instant,
) {
    let total = started_at.elapsed();
    let ms = |duration: Duration| duration.as_secs_f64() * 1_000.0;
    let enumerate_ms = ms(enumerated_at.duration_since(started_at));
    let ui_observer_ms = ms(observed_at.duration_since(enumerated_at));
    let attribution_ms = ms(observed_at.elapsed());
    let total_ms = ms(total);
    if total >= SLOW_SAMPLE {
        tracing::warn!(
            rows,
            total_ms,
            enumerate_ms,
            ui_observer_ms,
            attribution_ms,
            "slow process diagnostics sample"
        );
    } else {
        tracing::debug!(
            rows,
            total_ms,
            enumerate_ms,
            ui_observer_ms,
            attribution_ms,
            "process diagnostics sample"
        );
    }
}

async fn observe_ui_processes(
    observer: Arc<dyn DesktopUiProcessObserver>,
    rows: Arc<[ProcessRow]>,
    server_identity: ProcessIdentity,
) -> DesktopUiObservation {
    let mut task = tokio::spawn(async move { observer.observe(rows, server_identity).await });
    let observation = match tokio::time::timeout(UI_OBSERVATION_TIMEOUT, &mut task).await {
        Ok(Ok(observation)) => observation,
        Ok(Err(_)) => DesktopUiObservation {
            identities: Vec::new(),
            coverage: unavailable_coverage(&format!(
                "UI process observation failed. {UI_UNAVAILABLE_MESSAGE}"
            )),
        },
        Err(_) => {
            task.abort();
            DesktopUiObservation {
                identities: Vec::new(),
                coverage: unavailable_coverage(&format!(
                    "UI process observation timed out. {UI_UNAVAILABLE_MESSAGE}"
                )),
            }
        }
    };

    DesktopUiObservation {
        identities: observation
            .identities
            .into_iter()
            .take(UI_IDENTITY_LIMIT)
            .filter(|identity| identity.pid != 0 && identity.started_at != 0)
            .collect(),
        coverage: UiCoverage {
            status: observation.coverage.status,
            message: observation
                .coverage
                .message
                .map(|message| bound_diagnostic_string(&message, UI_COVERAGE_MESSAGE_MAX_SCALARS)),
        },
    }
}

fn append_ui_claims(
    claims: &mut Vec<ProcessClaim>,
    rows: &[ProcessRow],
    identities: &[ProcessIdentity],
) {
    let sampled_identities = rows
        .iter()
        .map(|row| ProcessIdentity {
            pid: row.pid,
            started_at: row.started_at,
        })
        .collect::<HashSet<_>>();
    let mut claimed_identities = claims
        .iter()
        .map(|claim| claim.identity)
        .collect::<HashSet<_>>();
    claims.extend(
        identities
            .iter()
            .copied()
            .filter(|identity| sampled_identities.contains(identity))
            .filter(|identity| claimed_identities.insert(*identity))
            .map(|identity| ProcessClaim {
                identity,
                scope: AttributionScope::Core,
                kind: AttributionKind::Ui,
                label: "core/ui".to_owned(),
            }),
    );
}

fn has_current_server_ancestry(
    rows: &[ProcessRow],
    server_identity: ProcessIdentity,
    target_identity: ProcessIdentity,
) -> bool {
    let rows_by_pid = rows
        .iter()
        .map(|row| (row.pid, row))
        .collect::<HashMap<_, _>>();
    let mut current = target_identity;
    let mut visited = HashSet::new();
    while visited.insert(current) {
        let Some(row) = rows_by_pid.get(&current.pid) else {
            return false;
        };
        if row.started_at != current.started_at {
            return false;
        }
        let Some(parent) = rows_by_pid.get(&row.ppid) else {
            return false;
        };
        let parent_identity = ProcessIdentity {
            pid: parent.pid,
            started_at: parent.started_at,
        };
        if parent_identity.started_at > current.started_at {
            return false;
        }
        if parent_identity == server_identity {
            return true;
        }
        current = parent_identity;
    }
    false
}

fn unavailable_coverage(message: &str) -> UiCoverage {
    UiCoverage {
        status: UiCoverageStatus::Unavailable,
        message: Some(bound_diagnostic_string(
            message,
            UI_COVERAGE_MESSAGE_MAX_SCALARS,
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future::Future,
        pin::Pin,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        time::{Duration, Instant},
    };

    use super::{
        DesktopUiObservation, DesktopUiProcessObserver, NativeProcessRowSource,
        NativeResourceSampler, ResourceSampler, UiCoverage, UiCoverageStatus,
        WebView2DesktopUiProcessObserver,
    };
    use crate::diagnostics::{
        AttributionConfidence, AttributionKind, AttributionScope, ProcessAttributionRegistry,
        ProcessIdentity, ProcessRegistrationMetadata, ProcessRow, ProcessSignal,
        RegistrationSource, SamplingError, SignalError,
    };

    #[derive(Debug)]
    struct FakeNativeProcessRowSource {
        rows: Vec<ProcessRow>,
        samples: AtomicUsize,
        signals: Mutex<Vec<(ProcessIdentity, ProcessSignal)>>,
        signal_supported: AtomicBool,
        replacement_before_signal: Mutex<Option<ProcessIdentity>>,
    }

    impl FakeNativeProcessRowSource {
        fn new(rows: Vec<ProcessRow>) -> Self {
            Self {
                rows,
                samples: AtomicUsize::new(0),
                signals: Mutex::new(Vec::new()),
                signal_supported: AtomicBool::new(true),
                replacement_before_signal: Mutex::new(None),
            }
        }

        fn replace_identity_before_signal(&self, replacement: ProcessIdentity) {
            *self
                .replacement_before_signal
                .lock()
                .expect("replacement identity") = Some(replacement);
        }
    }

    impl NativeProcessRowSource for FakeNativeProcessRowSource {
        fn collect_rows(
            &self,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<ProcessRow>, SamplingError>> + Send + '_>>
        {
            Box::pin(async move {
                self.samples.fetch_add(1, Ordering::SeqCst);
                Ok(self.rows.clone())
            })
        }

        fn signal_process(
            &self,
            expected_identity: ProcessIdentity,
            signal: ProcessSignal,
        ) -> Result<(), SignalError> {
            if !self.signal_supported.load(Ordering::SeqCst) {
                return Err(SignalError::Unsupported);
            }
            if let Some(replacement) = self
                .replacement_before_signal
                .lock()
                .expect("replacement identity")
                .take()
                && replacement != expected_identity
            {
                return Err(SignalError::StaleIdentity(expected_identity.pid));
            }
            self.signals
                .lock()
                .expect("signals")
                .push((expected_identity, signal));
            Ok(())
        }
    }

    #[derive(Clone, Debug)]
    enum FakeObservation {
        Return(DesktopUiObservation),
        Panic,
        Slow(Duration),
    }

    #[derive(Debug)]
    struct FakeDesktopUiProcessObserver {
        observation: FakeObservation,
    }

    impl DesktopUiProcessObserver for FakeDesktopUiProcessObserver {
        fn observe(
            &self,
            _rows: Arc<[ProcessRow]>,
            _server_identity: ProcessIdentity,
        ) -> Pin<Box<dyn Future<Output = DesktopUiObservation> + Send + '_>> {
            Box::pin(async move {
                match &self.observation {
                    FakeObservation::Return(observation) => observation.clone(),
                    FakeObservation::Panic => panic!("fake UI observer failure"),
                    FakeObservation::Slow(duration) => {
                        tokio::time::sleep(*duration).await;
                        DesktopUiObservation {
                            identities: Vec::new(),
                            coverage: UiCoverage {
                                status: UiCoverageStatus::Available,
                                message: None,
                            },
                        }
                    }
                }
            })
        }
    }

    #[tokio::test]
    async fn webview2_observer_attributes_only_this_desktop_process_tree_as_core_ui() {
        let server_pid = std::process::id();
        let webview_pid = server_pid + 1;
        let renderer_pid = server_pid + 2;
        let unrelated_pid = server_pid + 3;
        let mut webview = row(webview_pid, server_pid, 200);
        webview.command = "msedgewebview2.exe --embedded-browser-webview=1 --webview-exe-name=BiBCode-Desktop.EXE".to_owned();
        let mut unrelated = row(unrelated_pid, 1, 400);
        unrelated.command = "msedgewebview2.exe --embedded-browser-webview=1 --webview-exe-name=bibcode-desktop.exe".to_owned();
        let native = Arc::new(FakeNativeProcessRowSource::new(vec![
            row(server_pid, 1, 100),
            webview,
            row(renderer_pid, webview_pid, 300),
            unrelated,
        ]));
        let sampler = NativeResourceSampler::with_native_process_source(
            native,
            ProcessAttributionRegistry::new(),
            Arc::new(WebView2DesktopUiProcessObserver::new("bibcode-desktop.exe")),
        );

        let snapshot = sampler.sample().await.expect("sample should succeed");

        assert_eq!(snapshot.ui_coverage.status, UiCoverageStatus::Available);
        for pid in [webview_pid, renderer_pid] {
            let process = snapshot
                .processes
                .iter()
                .find(|process| process.identity.pid == pid)
                .expect("BiBCode WebView2 process");
            assert_eq!(process.scope, AttributionScope::Core);
            assert_eq!(process.kind, AttributionKind::Ui);
        }
        assert!(
            snapshot
                .processes
                .iter()
                .all(|process| process.identity.pid != unrelated_pid)
        );
    }

    fn row(pid: u32, ppid: u32, started_at: u64) -> ProcessRow {
        let mut row = ProcessRow::fixture(pid, ppid, format!("process-{pid}"));
        row.started_at = started_at;
        row.cpu_percent = 1.0;
        row.rss_bytes = 10;
        row
    }

    fn identity(pid: u32, started_at: u64) -> ProcessIdentity {
        ProcessIdentity { pid, started_at }
    }

    fn observer(observation: FakeObservation) -> Arc<dyn DesktopUiProcessObserver> {
        Arc::new(FakeDesktopUiProcessObserver { observation })
    }

    fn sampler(
        rows: Vec<ProcessRow>,
        observation: FakeObservation,
    ) -> (NativeResourceSampler, Arc<FakeNativeProcessRowSource>) {
        sampler_with_registry(rows, observation, ProcessAttributionRegistry::new())
    }

    fn sampler_with_registry(
        rows: Vec<ProcessRow>,
        observation: FakeObservation,
        registry: ProcessAttributionRegistry,
    ) -> (NativeResourceSampler, Arc<FakeNativeProcessRowSource>) {
        let native = Arc::new(FakeNativeProcessRowSource::new(rows));
        let sampler = NativeResourceSampler::with_native_process_source(
            native.clone(),
            registry,
            observer(observation),
        );
        (sampler, native)
    }

    #[tokio::test]
    async fn exact_ui_identities_become_core_ui_and_are_bounded_to_sixty_four() {
        let server_pid = std::process::id();
        let ui_identities = (1..=65)
            .map(|offset| identity(server_pid + offset, 100 + u64::from(offset)))
            .collect::<Vec<_>>();
        let mut rows = vec![row(server_pid, 1, 100)];
        rows.extend(
            ui_identities
                .iter()
                .map(|identity| row(identity.pid, 1, identity.started_at)),
        );
        let (sampler, native) = sampler(
            rows,
            FakeObservation::Return(DesktopUiObservation {
                identities: ui_identities,
                coverage: UiCoverage {
                    status: UiCoverageStatus::Available,
                    message: None,
                },
            }),
        );

        let snapshot = sampler.sample().await.expect("sample should succeed");
        let ui_processes = snapshot
            .processes
            .iter()
            .filter(|process| process.kind == AttributionKind::Ui)
            .collect::<Vec<_>>();

        assert_eq!(ui_processes.len(), 64);
        assert!(ui_processes.iter().all(|process| {
            process.scope == AttributionScope::Core
                && process.label == "core/ui"
                && process.confidence == AttributionConfidence::Exact
        }));
        assert_eq!(native.samples.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn exact_external_claim_wins_over_a_conflicting_ui_observation() {
        let server_pid = std::process::id();
        let candidate = identity(server_pid + 1, 200);
        let registry = ProcessAttributionRegistry::new();
        let _registration = registry
            .register_identity(
                candidate,
                ProcessRegistrationMetadata {
                    scope: AttributionScope::External,
                    kind: AttributionKind::Provider,
                    label: "external/provider/codex".to_owned(),
                    source: RegistrationSource::Provider,
                    driver: None,
                },
            )
            .expect("registration should fit");
        let (sampler, _) = sampler_with_registry(
            vec![
                row(server_pid, 1, 100),
                row(candidate.pid, server_pid, candidate.started_at),
            ],
            FakeObservation::Return(DesktopUiObservation {
                identities: vec![candidate, candidate],
                coverage: UiCoverage {
                    status: UiCoverageStatus::Available,
                    message: None,
                },
            }),
            registry,
        );

        let snapshot = sampler.sample().await.expect("sample should succeed");
        let process = snapshot
            .processes
            .iter()
            .find(|process| process.identity == candidate)
            .expect("registered candidate");

        assert_eq!(process.scope, AttributionScope::External);
        assert_eq!(process.kind, AttributionKind::Provider);
        assert_eq!(process.label, "external/provider/codex");
        assert_eq!(process.confidence, AttributionConfidence::Exact);
    }

    #[tokio::test]
    async fn every_ui_coverage_status_survives_the_sample() {
        for status in [
            UiCoverageStatus::Available,
            UiCoverageStatus::Partial,
            UiCoverageStatus::Unavailable,
            UiCoverageStatus::NotApplicable,
        ] {
            let server_pid = std::process::id();
            let coverage = UiCoverage {
                status,
                message: Some("coverage detail".to_owned()),
            };
            let (sampler, _) = sampler(
                vec![row(server_pid, 1, 100)],
                FakeObservation::Return(DesktopUiObservation {
                    identities: Vec::new(),
                    coverage: coverage.clone(),
                }),
            );

            let snapshot = sampler.sample().await.expect("sample should succeed");

            assert_eq!(snapshot.ui_coverage, coverage);
        }
    }

    #[tokio::test]
    async fn partial_observations_keep_exact_rows_and_bound_utf8_messages() {
        let server_pid = std::process::id();
        let ui_identity = identity(server_pid + 1, 200);
        let (sampler, _) = sampler(
            vec![
                row(server_pid, 1, 100),
                row(ui_identity.pid, 1, ui_identity.started_at),
            ],
            FakeObservation::Return(DesktopUiObservation {
                identities: vec![ui_identity],
                coverage: UiCoverage {
                    status: UiCoverageStatus::Partial,
                    message: Some("é".repeat(200)),
                },
            }),
        );

        let snapshot = sampler.sample().await.expect("sample should succeed");

        assert_eq!(
            snapshot
                .processes
                .iter()
                .find(|process| process.identity == ui_identity)
                .map(|process| process.kind),
            Some(AttributionKind::Ui)
        );
        assert_eq!(
            snapshot
                .ui_coverage
                .message
                .as_deref()
                .expect("partial message")
                .chars()
                .count(),
            160
        );
    }

    #[tokio::test]
    async fn observer_panics_become_unavailable_without_losing_the_server_sample() {
        let server_pid = std::process::id();
        let (sampler, _) = sampler(vec![row(server_pid, 1, 100)], FakeObservation::Panic);

        let snapshot = sampler.sample().await.expect("sample should succeed");

        assert_eq!(snapshot.ui_coverage.status, UiCoverageStatus::Unavailable);
        assert!(snapshot.processes.iter().any(|process| {
            process.identity == identity(server_pid, 100) && process.kind == AttributionKind::Server
        }));
    }

    #[tokio::test]
    async fn slow_observers_time_out_without_blocking_native_diagnostics() {
        let server_pid = std::process::id();
        let (sampler, native) = sampler(
            vec![row(server_pid, 1, 100)],
            FakeObservation::Slow(Duration::from_secs(5)),
        );
        let started = Instant::now();

        let snapshot = sampler.sample().await.expect("sample should succeed");

        assert_eq!(snapshot.ui_coverage.status, UiCoverageStatus::Unavailable);
        assert!(started.elapsed() >= Duration::from_millis(200));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(native.samples.load(Ordering::SeqCst), 1);
        assert!(snapshot.processes.iter().any(|process| {
            process.identity == identity(server_pid, 100) && process.kind == AttributionKind::Server
        }));
    }

    #[tokio::test]
    async fn malformed_and_unknown_ui_identities_are_ignored_without_guessing() {
        let server_pid = std::process::id();
        let ui_pid = server_pid + 1;
        let (sampler, _) = sampler(
            vec![row(server_pid, 1, 100), row(ui_pid, server_pid, 200)],
            FakeObservation::Return(DesktopUiObservation {
                identities: vec![
                    identity(0, 0),
                    identity(ui_pid, 201),
                    identity(u32::MAX, 999),
                ],
                coverage: UiCoverage {
                    status: UiCoverageStatus::Partial,
                    message: Some("Some helpers could not be observed.".to_owned()),
                },
            }),
        );

        let snapshot = sampler.sample().await.expect("sample should succeed");

        assert!(
            snapshot
                .processes
                .iter()
                .all(|process| process.kind != AttributionKind::Ui)
        );
        assert_eq!(
            snapshot
                .processes
                .iter()
                .find(|process| process.identity.pid == ui_pid)
                .map(|process| process.kind),
            Some(AttributionKind::Unknown)
        );
    }

    #[tokio::test]
    async fn each_resource_sample_collects_native_rows_exactly_once() {
        let server_pid = std::process::id();
        let (sampler, native) = sampler(
            vec![row(server_pid, 1, 100)],
            FakeObservation::Return(DesktopUiObservation {
                identities: Vec::new(),
                coverage: UiCoverage {
                    status: UiCoverageStatus::NotApplicable,
                    message: None,
                },
            }),
        );

        sampler.sample().await.expect("first sample should succeed");
        sampler
            .sample()
            .await
            .expect("second sample should succeed");

        assert_eq!(native.samples.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn signal_revalidation_requires_current_external_identity_and_server_ancestry() {
        let server_pid = std::process::id();
        let target_pid = server_pid + 1;
        let server_identity = identity(server_pid, 100);
        let target_identity = identity(target_pid, 200);
        let external_registry = ProcessAttributionRegistry::new();
        let _registration = external_registry
            .register_identity(
                target_identity,
                ProcessRegistrationMetadata {
                    scope: AttributionScope::External,
                    kind: AttributionKind::Provider,
                    label: "Codex".to_owned(),
                    source: RegistrationSource::Provider,
                    driver: None,
                },
            )
            .expect("external registration");
        let (external_sampler, external_native) = sampler_with_registry(
            vec![
                row(server_pid, 1, server_identity.started_at),
                row(target_pid, server_pid, target_identity.started_at),
            ],
            FakeObservation::Return(DesktopUiObservation {
                identities: vec![target_identity],
                coverage: UiCoverage {
                    status: UiCoverageStatus::Available,
                    message: None,
                },
            }),
            external_registry,
        );

        external_sampler
            .signal_external_descendant(target_identity, ProcessSignal::Interrupt)
            .await
            .expect("exact external descendant should be signaled");

        assert_eq!(external_native.samples.load(Ordering::SeqCst), 1);
        assert_eq!(
            *external_native.signals.lock().expect("signals"),
            [(target_identity, ProcessSignal::Interrupt)]
        );

        let (core_root_sampler, _) = sampler(
            vec![row(server_pid, 1, server_identity.started_at)],
            FakeObservation::Return(DesktopUiObservation {
                identities: Vec::new(),
                coverage: UiCoverage {
                    status: UiCoverageStatus::NotApplicable,
                    message: None,
                },
            }),
        );
        assert!(matches!(
            core_root_sampler
                .signal_external_descendant(server_identity, ProcessSignal::Kill)
                .await,
            Err(SignalError::NotEligible(pid)) if pid == server_pid
        ));

        let (core_ui_sampler, _) = sampler(
            vec![
                row(server_pid, 1, server_identity.started_at),
                row(target_pid, server_pid, target_identity.started_at),
            ],
            FakeObservation::Return(DesktopUiObservation {
                identities: vec![target_identity],
                coverage: UiCoverage {
                    status: UiCoverageStatus::Available,
                    message: None,
                },
            }),
        );
        assert!(matches!(
            core_ui_sampler
                .signal_external_descendant(target_identity, ProcessSignal::Kill)
                .await,
            Err(SignalError::NotEligible(pid)) if pid == target_pid
        ));

        let stale_registry = ProcessAttributionRegistry::new();
        let _stale_registration = stale_registry
            .register_identity(
                target_identity,
                ProcessRegistrationMetadata {
                    scope: AttributionScope::External,
                    kind: AttributionKind::Provider,
                    label: "Codex".to_owned(),
                    source: RegistrationSource::Provider,
                    driver: None,
                },
            )
            .expect("stale registration");
        let (stale_sampler, stale_native) = sampler_with_registry(
            vec![
                row(server_pid, 1, server_identity.started_at),
                row(target_pid, server_pid, target_identity.started_at + 1),
            ],
            FakeObservation::Return(DesktopUiObservation {
                identities: Vec::new(),
                coverage: UiCoverage {
                    status: UiCoverageStatus::NotApplicable,
                    message: None,
                },
            }),
            stale_registry,
        );
        assert!(matches!(
            stale_sampler
                .signal_external_descendant(target_identity, ProcessSignal::Kill)
                .await,
            Err(SignalError::StaleIdentity(pid)) if pid == target_pid
        ));
        assert_eq!(stale_native.samples.load(Ordering::SeqCst), 1);

        let reparented_registry = ProcessAttributionRegistry::new();
        let _reparented_registration = reparented_registry
            .register_identity(
                target_identity,
                ProcessRegistrationMetadata {
                    scope: AttributionScope::External,
                    kind: AttributionKind::Provider,
                    label: "Codex".to_owned(),
                    source: RegistrationSource::Provider,
                    driver: None,
                },
            )
            .expect("reparented registration");
        let (reparented_sampler, _) = sampler_with_registry(
            vec![
                row(server_pid, 1, server_identity.started_at),
                row(target_pid, 1, target_identity.started_at),
            ],
            FakeObservation::Return(DesktopUiObservation {
                identities: Vec::new(),
                coverage: UiCoverage {
                    status: UiCoverageStatus::NotApplicable,
                    message: None,
                },
            }),
            reparented_registry,
        );
        assert!(matches!(
            reparented_sampler
                .signal_external_descendant(target_identity, ProcessSignal::Kill)
                .await,
            Err(SignalError::NotDescendant(pid)) if pid == target_pid
        ));

        assert!(matches!(
            external_sampler
                .signal_external_descendant(identity(u32::MAX, 999), ProcessSignal::Kill)
                .await,
            Err(SignalError::NotFound(pid)) if pid == u32::MAX
        ));

        external_native
            .signal_supported
            .store(false, Ordering::SeqCst);
        assert!(matches!(
            external_sampler
                .signal_external_descendant(target_identity, ProcessSignal::Kill)
                .await,
            Err(SignalError::Unsupported)
        ));
    }

    #[tokio::test]
    async fn signal_revalidation_rejects_replacement_after_attribution() {
        let server_pid = std::process::id();
        let target_pid = server_pid + 1;
        let target_identity = identity(target_pid, 200);
        let replacement_identity = identity(target_pid, 201);
        let registry = ProcessAttributionRegistry::new();
        let _registration = registry
            .register_identity(
                target_identity,
                ProcessRegistrationMetadata {
                    scope: AttributionScope::External,
                    kind: AttributionKind::Provider,
                    label: "Codex".to_owned(),
                    source: RegistrationSource::Provider,
                    driver: None,
                },
            )
            .expect("external registration");
        let (sampler, native) = sampler_with_registry(
            vec![
                row(server_pid, 1, 100),
                row(target_pid, server_pid, target_identity.started_at),
            ],
            FakeObservation::Return(DesktopUiObservation {
                identities: Vec::new(),
                coverage: UiCoverage {
                    status: UiCoverageStatus::NotApplicable,
                    message: None,
                },
            }),
            registry,
        );
        native.replace_identity_before_signal(replacement_identity);

        assert!(matches!(
            sampler
                .signal_external_descendant(target_identity, ProcessSignal::Kill)
                .await,
            Err(SignalError::StaleIdentity(pid)) if pid == target_pid
        ));
        assert_eq!(native.samples.load(Ordering::SeqCst), 1);
        assert!(native.signals.lock().expect("signals").is_empty());
    }
}
