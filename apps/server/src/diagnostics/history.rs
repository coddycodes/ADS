use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::Duration,
};

use super::{
    AttributedProcessSnapshot, AttributionConfidence, AttributionKind, AttributionScope,
    ProcessAttributionTotals, ProcessIdentity, UiCoverage, process_tree_metadata,
};

const RETENTION: Duration = Duration::from_secs(60 * 60);
const MAX_RETAINED_SAMPLES: usize = 20_000;

/// One retained sample (#100). The sampler runs every 2 s while the status bar reads it, so an
/// hour holds about 1,800 of these: each keeps only what changes from tick to tick, and shares
/// what does not with the sample before it.
#[derive(Clone, Debug, PartialEq)]
pub struct AttributedProcessSample {
    pub sampled_at_ms: i128,
    processes: Vec<SampledProcess>,
    pub totals: ProcessAttributionTotals,
    pub ui_coverage: Arc<UiCoverage>,
}

#[derive(Clone, Debug, PartialEq)]
struct SampledProcess {
    cpu_percent: f64,
    rss_bytes: u64,
    details: Arc<ProcessDetails>,
}

/// What stays the same for a process from one sample to the next.
#[derive(Debug, PartialEq)]
struct ProcessDetails {
    identity: ProcessIdentity,
    process_key: String,
    scope: AttributionScope,
    kind: AttributionKind,
    label: String,
    confidence: AttributionConfidence,
    metadata: Option<ProcessHistoryMetadata>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ProcessHistoryMetadata {
    ppid: u32,
    command: String,
    depth: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SplitMetric<T> {
    pub combined: T,
    pub core: T,
    pub external: T,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BucketMetric<T> {
    pub average: SplitMetric<T>,
    pub peak: SplitMetric<T>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProcessResourceBucket {
    pub started_at_ms: i128,
    pub ended_at_ms: i128,
    pub cpu_percent: BucketMetric<f64>,
    pub rss_bytes: BucketMetric<u64>,
    pub max_process_count: SplitMetric<usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProcessResourceSummary {
    pub process_key: String,
    pub pid: u32,
    pub ppid: u32,
    pub command: String,
    pub depth: usize,
    pub scope: AttributionScope,
    pub kind: AttributionKind,
    pub label: String,
    pub confidence: AttributionConfidence,
    pub first_seen_at_ms: i128,
    pub last_seen_at_ms: i128,
    pub current_cpu_percent: f64,
    pub avg_cpu_percent: f64,
    pub max_cpu_percent: f64,
    pub cpu_seconds_approx: f64,
    pub current_rss_bytes: u64,
    pub max_rss_bytes: u64,
    pub sample_count: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProcessResourceHistory {
    pub read_at_ms: i128,
    pub window_ms: u64,
    pub bucket_ms: u64,
    pub sample_interval_ms: u64,
    pub retained_sample_count: usize,
    pub total_cpu_seconds_approx: SplitMetric<f64>,
    pub ui_coverage: UiCoverage,
    pub buckets: Vec<ProcessResourceBucket>,
    pub processes: Vec<ProcessResourceSummary>,
    pub error: Option<String>,
}

impl AttributedProcessSample {
    /// The sample of `snapshot`, sharing each process's details and the coverage with
    /// `previous` where they did not change.
    pub(crate) fn new(
        snapshot: &AttributedProcessSnapshot,
        previous: Option<&AttributedProcessSample>,
    ) -> Self {
        let mut metadata = process_tree_metadata(
            &snapshot.native_rows,
            snapshot.processes.iter().map(|process| process.identity),
        );
        let previous_details = previous
            .map(|previous| {
                previous
                    .processes
                    .iter()
                    .map(|process| (process.details.identity, &process.details))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        let processes = snapshot
            .processes
            .iter()
            .map(|process| {
                let details = ProcessDetails {
                    identity: process.identity,
                    process_key: process.process_key.clone(),
                    scope: process.scope,
                    kind: process.kind,
                    label: process.label.clone(),
                    confidence: process.confidence,
                    metadata: metadata.remove(&process.identity).map(|metadata| {
                        ProcessHistoryMetadata {
                            ppid: metadata.ppid,
                            command: metadata.command,
                            depth: metadata.depth,
                        }
                    }),
                };
                SampledProcess {
                    cpu_percent: process.cpu_percent,
                    rss_bytes: process.rss_bytes,
                    details: previous_details
                        .get(&process.identity)
                        .copied()
                        .filter(|shared| ***shared == details)
                        .map_or_else(|| Arc::new(details), Arc::clone),
                }
            })
            .collect();
        let ui_coverage = previous
            .map(|previous| &previous.ui_coverage)
            .filter(|shared| ***shared == snapshot.ui_coverage)
            .map_or_else(|| Arc::new(snapshot.ui_coverage.clone()), Arc::clone);
        Self {
            sampled_at_ms: snapshot.sampled_at_ms,
            processes,
            totals: snapshot.totals,
            ui_coverage,
        }
    }
}

pub(crate) fn trim_samples(samples: &mut VecDeque<Arc<AttributedProcessSample>>, now_ms: i128) {
    let minimum = now_ms - RETENTION.as_millis() as i128;
    while samples
        .front()
        .is_some_and(|sample| sample.sampled_at_ms < minimum)
        || samples.len() > MAX_RETAINED_SAMPLES
    {
        samples.pop_front();
    }
}

pub(crate) fn aggregate_history(
    retained: &[Arc<AttributedProcessSample>],
    error: Option<String>,
    read_at_ms: i128,
    window_ms: u64,
    bucket_ms: u64,
    interval: Duration,
) -> ProcessResourceHistory {
    let minimum = read_at_ms - i128::from(window_ms);
    let samples = retained
        .iter()
        .map(Arc::as_ref)
        .filter(|sample| sample.sampled_at_ms >= minimum)
        .collect::<Vec<_>>();
    let interval_seconds = interval.as_secs_f64();
    let core_cpu_seconds_approx = samples
        .iter()
        .map(|sample| sample.totals.core.cpu_percent / 100.0 * interval_seconds)
        .sum::<f64>();
    let external_cpu_seconds_approx = samples
        .iter()
        .map(|sample| sample.totals.external.cpu_percent / 100.0 * interval_seconds)
        .sum::<f64>();
    ProcessResourceHistory {
        read_at_ms,
        window_ms,
        bucket_ms,
        sample_interval_ms: interval.as_millis().try_into().unwrap_or(u64::MAX),
        retained_sample_count: retained.len(),
        total_cpu_seconds_approx: SplitMetric {
            combined: core_cpu_seconds_approx + external_cpu_seconds_approx,
            core: core_cpu_seconds_approx,
            external: external_cpu_seconds_approx,
        },
        ui_coverage: retained
            .last()
            .map(|sample| UiCoverage::clone(&sample.ui_coverage))
            .unwrap_or_default(),
        buckets: if retained.is_empty() {
            Vec::new()
        } else {
            build_buckets(&samples, read_at_ms, window_ms, bucket_ms)
        },
        processes: summarize_processes(&samples, interval_seconds),
        error,
    }
}

fn summarize_processes(
    samples: &[&AttributedProcessSample],
    interval_seconds: f64,
) -> Vec<ProcessResourceSummary> {
    struct ProcessGroup<'a> {
        process_key: &'a str,
        samples: Vec<(i128, &'a SampledProcess)>,
    }

    let mut group_indexes = HashMap::<&str, usize>::new();
    let mut groups = Vec::<ProcessGroup<'_>>::new();
    for sample in samples {
        for process in &sample.processes {
            let process_key = process.details.process_key.as_str();
            let index = *group_indexes.entry(process_key).or_insert_with(|| {
                groups.push(ProcessGroup {
                    process_key,
                    samples: Vec::new(),
                });
                groups.len() - 1
            });
            groups[index].samples.push((sample.sampled_at_ms, process));
        }
    }

    groups
        .into_iter()
        .filter_map(|group| {
            let (first_seen_at_ms, _) = group
                .samples
                .iter()
                .min_by_key(|(sampled_at_ms, _)| *sampled_at_ms)
                .copied()?;
            let (last_seen_at_ms, latest) = group
                .samples
                .iter()
                .max_by_key(|(sampled_at_ms, _)| *sampled_at_ms)
                .copied()?;
            let details = latest.details.as_ref();
            let metadata = details.metadata.as_ref();
            let cpu_total = group
                .samples
                .iter()
                .map(|(_, process)| process.cpu_percent)
                .sum::<f64>();
            Some(ProcessResourceSummary {
                process_key: group.process_key.to_owned(),
                pid: details.identity.pid,
                ppid: metadata.map_or(0, |metadata| metadata.ppid),
                command: metadata.map_or_else(
                    || details.label.clone(),
                    |metadata| metadata.command.clone(),
                ),
                depth: metadata.map_or(0, |metadata| metadata.depth),
                scope: details.scope,
                kind: details.kind,
                label: details.label.clone(),
                confidence: details.confidence,
                first_seen_at_ms,
                last_seen_at_ms,
                current_cpu_percent: latest.cpu_percent,
                avg_cpu_percent: cpu_total / group.samples.len() as f64,
                max_cpu_percent: group
                    .samples
                    .iter()
                    .map(|(_, process)| process.cpu_percent)
                    .fold(0.0, f64::max),
                cpu_seconds_approx: group
                    .samples
                    .iter()
                    .map(|(_, process)| process.cpu_percent / 100.0 * interval_seconds)
                    .sum(),
                current_rss_bytes: latest.rss_bytes,
                max_rss_bytes: group
                    .samples
                    .iter()
                    .map(|(_, process)| process.rss_bytes)
                    .max()
                    .unwrap_or(0),
                sample_count: group.samples.len(),
            })
        })
        .collect()
}

fn build_buckets(
    samples: &[&AttributedProcessSample],
    read_at_ms: i128,
    window_ms: u64,
    bucket_ms: u64,
) -> Vec<ProcessResourceBucket> {
    let mut buckets = Vec::new();
    let mut started_at_ms = read_at_ms - i128::from(window_ms);
    while started_at_ms < read_at_ms {
        let ended_at_ms = (started_at_ms + i128::from(bucket_ms)).min(read_at_ms);
        let reads = samples
            .iter()
            .copied()
            .filter(|sample| {
                sample.sampled_at_ms >= started_at_ms
                    && (sample.sampled_at_ms < ended_at_ms
                        || ended_at_ms == read_at_ms && sample.sampled_at_ms <= ended_at_ms)
            })
            .collect::<Vec<_>>();
        let cpu_average = average_cpu(&reads);
        let rss_average = average_rss(&reads);
        let cpu_peak = reads
            .iter()
            .copied()
            .reduce(|peak, sample| {
                if sample.totals.combined.cpu_percent > peak.totals.combined.cpu_percent {
                    sample
                } else {
                    peak
                }
            })
            .map(cpu_metric)
            .unwrap_or_default();
        let rss_peak = reads
            .iter()
            .copied()
            .reduce(|peak, sample| {
                if sample.totals.combined.rss_bytes > peak.totals.combined.rss_bytes {
                    sample
                } else {
                    peak
                }
            })
            .map(rss_metric)
            .unwrap_or_default();
        let max_process_count = reads
            .iter()
            .copied()
            .reduce(|peak, sample| {
                if sample.totals.combined.process_count > peak.totals.combined.process_count {
                    sample
                } else {
                    peak
                }
            })
            .map(process_count_metric)
            .unwrap_or_default();
        buckets.push(ProcessResourceBucket {
            started_at_ms,
            ended_at_ms,
            cpu_percent: BucketMetric {
                average: cpu_average,
                peak: cpu_peak,
            },
            rss_bytes: BucketMetric {
                average: rss_average,
                peak: rss_peak,
            },
            max_process_count,
        });
        started_at_ms = ended_at_ms;
    }
    buckets
}

fn average_cpu(samples: &[&AttributedProcessSample]) -> SplitMetric<f64> {
    if samples.is_empty() {
        return SplitMetric::default();
    }
    let divisor = samples.len() as f64;
    let core = samples
        .iter()
        .map(|sample| sample.totals.core.cpu_percent)
        .sum::<f64>()
        / divisor;
    let external = samples
        .iter()
        .map(|sample| sample.totals.external.cpu_percent)
        .sum::<f64>()
        / divisor;
    SplitMetric {
        combined: core + external,
        core,
        external,
    }
}

fn average_rss(samples: &[&AttributedProcessSample]) -> SplitMetric<u64> {
    let core = rounded_average(
        samples.iter().map(|sample| sample.totals.core.rss_bytes),
        samples.len(),
    );
    let external = rounded_average(
        samples
            .iter()
            .map(|sample| sample.totals.external.rss_bytes),
        samples.len(),
    );
    SplitMetric {
        combined: core.saturating_add(external),
        core,
        external,
    }
}

fn rounded_average(values: impl Iterator<Item = u64>, count: usize) -> u64 {
    if count == 0 {
        return 0;
    }
    let count = count as u128;
    let total = values.fold(0_u128, |total, value| total + u128::from(value));
    u64::try_from((total + count / 2) / count).unwrap_or(u64::MAX)
}

fn cpu_metric(sample: &AttributedProcessSample) -> SplitMetric<f64> {
    SplitMetric {
        combined: sample.totals.combined.cpu_percent,
        core: sample.totals.core.cpu_percent,
        external: sample.totals.external.cpu_percent,
    }
}

fn rss_metric(sample: &AttributedProcessSample) -> SplitMetric<u64> {
    SplitMetric {
        combined: sample.totals.combined.rss_bytes,
        core: sample.totals.core.rss_bytes,
        external: sample.totals.external.rss_bytes,
    }
}

fn process_count_metric(sample: &AttributedProcessSample) -> SplitMetric<usize> {
    SplitMetric {
        combined: sample.totals.combined.process_count,
        core: sample.totals.core.process_count,
        external: sample.totals.external.process_count,
    }
}

#[cfg(test)]
mod tests {
    use crate::diagnostics::{
        AttributedProcess, AttributionConfidence, AttributionKind, AttributionScope,
        ProcessAttributionTotals, ProcessIdentity, ProcessResourceTotals, ProcessRow, UiCoverage,
        UiCoverageStatus,
    };

    use super::*;

    fn attributed_process(
        identity: ProcessIdentity,
        scope: AttributionScope,
        kind: AttributionKind,
        label: &str,
        confidence: AttributionConfidence,
        cpu_percent: f64,
        rss_bytes: u64,
    ) -> AttributedProcess {
        AttributedProcess {
            identity,
            process_key: identity.key(),
            scope,
            kind,
            label: label.to_owned(),
            confidence,
            cpu_percent,
            rss_bytes,
        }
    }

    fn history_sample(
        sampled_at_ms: i128,
        core_cpu_percent: f64,
        core_rss_bytes: u64,
        external_cpu_percent: f64,
        external_rss_bytes: u64,
        coverage_status: UiCoverageStatus,
    ) -> Arc<AttributedProcessSample> {
        let core = attributed_process(
            ProcessIdentity {
                pid: 10,
                started_at: 100,
            },
            AttributionScope::Core,
            AttributionKind::Server,
            "core/server",
            AttributionConfidence::Exact,
            core_cpu_percent,
            core_rss_bytes,
        );
        let external = attributed_process(
            ProcessIdentity {
                pid: 20,
                started_at: 200,
            },
            AttributionScope::External,
            AttributionKind::Provider,
            "external/provider",
            AttributionConfidence::Inherited,
            external_cpu_percent,
            external_rss_bytes,
        );
        let snapshot = AttributedProcessSnapshot {
            sampled_at_ms,
            server_identity: core.identity,
            native_rows: Arc::from([]),
            processes: vec![core, external],
            totals: ProcessAttributionTotals {
                combined: ProcessResourceTotals {
                    cpu_percent: core_cpu_percent + external_cpu_percent,
                    rss_bytes: core_rss_bytes + external_rss_bytes,
                    process_count: 2,
                },
                core: ProcessResourceTotals {
                    cpu_percent: core_cpu_percent,
                    rss_bytes: core_rss_bytes,
                    process_count: 1,
                },
                external: ProcessResourceTotals {
                    cpu_percent: external_cpu_percent,
                    rss_bytes: external_rss_bytes,
                    process_count: 1,
                },
            },
            ui_coverage: UiCoverage {
                status: coverage_status,
                message: Some("fixture coverage".to_owned()),
            },
        };
        Arc::new(AttributedProcessSample::new(&snapshot, None))
    }

    fn split_history_samples() -> Vec<Arc<AttributedProcessSample>> {
        vec![
            history_sample(1_000, 80.0, 100, 10.0, 900, UiCoverageStatus::Partial),
            history_sample(2_000, 10.0, 2_000, 100.0, 100, UiCoverageStatus::Partial),
            history_sample(3_000, 30.0, 1_000, 20.0, 5_000, UiCoverageStatus::Partial),
        ]
    }

    #[test]
    fn attributed_history_retains_native_metadata_for_independent_roots() {
        let server_identity = ProcessIdentity {
            pid: 10,
            started_at: 100,
        };
        let provider_identity = ProcessIdentity {
            pid: 20,
            started_at: 200,
        };
        let child_identity = ProcessIdentity {
            pid: 21,
            started_at: 210,
        };
        let rows = [
            {
                let mut row = ProcessRow::fixture(10, 1, "coddy-studio");
                row.started_at = 100;
                row
            },
            {
                let mut row = ProcessRow::fixture(20, 1, "codex --model gpt");
                row.started_at = 200;
                row
            },
            {
                let mut row = ProcessRow::fixture(21, 20, "provider helper");
                row.started_at = 210;
                row
            },
        ];
        let process =
            |identity: ProcessIdentity, confidence: AttributionConfidence| -> AttributedProcess {
                AttributedProcess {
                    identity,
                    process_key: identity.key(),
                    scope: AttributionScope::External,
                    kind: AttributionKind::Provider,
                    label: "Codex".to_owned(),
                    confidence,
                    cpu_percent: 1.0,
                    rss_bytes: 10,
                }
            };
        let snapshot = AttributedProcessSnapshot {
            sampled_at_ms: 1_000,
            server_identity,
            native_rows: Arc::from(rows),
            processes: vec![
                process(provider_identity, AttributionConfidence::Exact),
                process(child_identity, AttributionConfidence::Inherited),
            ],
            totals: ProcessAttributionTotals {
                combined: ProcessResourceTotals {
                    cpu_percent: 2.0,
                    rss_bytes: 20,
                    process_count: 2,
                },
                core: ProcessResourceTotals::default(),
                external: ProcessResourceTotals {
                    cpu_percent: 2.0,
                    rss_bytes: 20,
                    process_count: 2,
                },
            },
            ui_coverage: UiCoverage::default(),
        };
        let retained = [Arc::new(AttributedProcessSample::new(&snapshot, None))];

        let history = aggregate_history(
            &retained,
            None,
            1_000,
            1_000,
            1_000,
            Duration::from_millis(500),
        );
        let provider = history
            .processes
            .iter()
            .find(|process| process.pid == 20)
            .expect("provider summary");
        let child = history
            .processes
            .iter()
            .find(|process| process.pid == 21)
            .expect("child summary");

        assert_eq!(provider.ppid, 1);
        assert_eq!(provider.command, "codex --model gpt");
        assert_eq!(provider.depth, 0);
        assert_eq!(child.ppid, 20);
        assert_eq!(child.command, "provider helper");
        assert_eq!(child.depth, 1);
    }

    #[test]
    fn retained_samples_do_not_copy_unattributed_native_rows() {
        let server_identity = ProcessIdentity {
            pid: 10,
            started_at: 100,
        };
        let mut native_row = ProcessRow::fixture(10, 1, "unattributed-secret-command");
        native_row.started_at = server_identity.started_at;
        let snapshot = AttributedProcessSnapshot {
            sampled_at_ms: 1_000,
            server_identity,
            native_rows: Arc::from([native_row]),
            processes: Vec::new(),
            totals: ProcessAttributionTotals::default(),
            ui_coverage: UiCoverage::default(),
        };

        let retained = AttributedProcessSample::new(&snapshot, None);

        assert!(
            !format!("{retained:?}").contains("unattributed-secret-command"),
            "retained history must not duplicate native rows outside attributed history"
        );
    }

    /// #100: an hour of samples must not repeat each process's strings 1,800 times.
    #[test]
    fn samples_share_what_did_not_change_and_keep_what_did() {
        let identity = ProcessIdentity {
            pid: 10,
            started_at: 100,
        };
        let snapshot = |sampled_at_ms: i128, label: &str| {
            let mut row = ProcessRow::fixture(10, 1, "coddy-studio serve");
            row.started_at = 100;
            AttributedProcessSnapshot {
                sampled_at_ms,
                server_identity: identity,
                native_rows: Arc::from([row]),
                processes: vec![attributed_process(
                    identity,
                    AttributionScope::Core,
                    AttributionKind::Server,
                    label,
                    AttributionConfidence::Exact,
                    1.0,
                    10,
                )],
                totals: ProcessAttributionTotals::default(),
                ui_coverage: UiCoverage::default(),
            }
        };
        let first = AttributedProcessSample::new(&snapshot(1_000, "server"), None);
        let same = AttributedProcessSample::new(&snapshot(2_000, "server"), Some(&first));
        let relabeled = AttributedProcessSample::new(&snapshot(3_000, "renamed"), Some(&same));

        assert!(Arc::ptr_eq(
            &first.processes[0].details,
            &same.processes[0].details
        ));
        assert!(Arc::ptr_eq(&first.ui_coverage, &same.ui_coverage));
        assert!(!Arc::ptr_eq(
            &same.processes[0].details,
            &relabeled.processes[0].details
        ));
        let history = aggregate_history(
            &[first, same, relabeled].map(Arc::new),
            None,
            3_000,
            3_000,
            3_000,
            Duration::from_secs(1),
        );
        assert_eq!(history.processes[0].label, "renamed");
        assert_eq!(history.processes[0].command, "coddy-studio serve");
        assert_eq!(history.processes[0].sample_count, 3);
    }

    #[test]
    fn retained_samples_keep_only_metadata_required_by_attributed_history() {
        let server_identity = ProcessIdentity {
            pid: 10,
            started_at: 100,
        };
        let mut native_row = ProcessRow::fixture(10, 1, "required-command");
        native_row.started_at = server_identity.started_at;
        native_row.status = "unretained-status".to_owned();
        native_row.elapsed = "unretained-elapsed".to_owned();
        let snapshot = AttributedProcessSnapshot {
            sampled_at_ms: 1_000,
            server_identity,
            native_rows: Arc::from([native_row]),
            processes: vec![attributed_process(
                server_identity,
                AttributionScope::Core,
                AttributionKind::Server,
                "core/server",
                AttributionConfidence::Exact,
                1.0,
                10,
            )],
            totals: ProcessAttributionTotals::default(),
            ui_coverage: UiCoverage::default(),
        };

        let retained = format!("{:?}", AttributedProcessSample::new(&snapshot, None));

        assert!(retained.contains("required-command"));
        assert!(!retained.contains("unretained-status"));
        assert!(!retained.contains("unretained-elapsed"));
    }

    fn assert_near(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected {expected}, got {actual}"
        );
    }

    fn empty_sample(sampled_at_ms: i128) -> AttributedProcessSample {
        AttributedProcessSample {
            sampled_at_ms,
            processes: Vec::new(),
            totals: ProcessAttributionTotals::default(),
            ui_coverage: Arc::default(),
        }
    }

    #[test]
    fn retained_samples_are_bounded_by_age_and_count() {
        let mut retained = (0..MAX_RETAINED_SAMPLES + 2)
            .map(|sampled_at_ms| Arc::new(empty_sample(sampled_at_ms as i128)))
            .collect::<VecDeque<_>>();

        trim_samples(&mut retained, (MAX_RETAINED_SAMPLES + 1) as i128);

        assert_eq!(retained.len(), MAX_RETAINED_SAMPLES);
        assert_eq!(retained.front().map(|sample| sample.sampled_at_ms), Some(2));

        retained.push_back(Arc::new(empty_sample(RETENTION.as_millis() as i128 + 3)));
        trim_samples(&mut retained, RETENTION.as_millis() as i128 + 3);

        assert_eq!(retained.front().map(|sample| sample.sampled_at_ms), Some(3));
        assert_eq!(retained.len(), MAX_RETAINED_SAMPLES);
    }

    #[test]
    fn split_bucket_averages_and_source_sample_peaks_reconcile() {
        let history = aggregate_history(
            &split_history_samples(),
            None,
            4_000,
            3_000,
            3_000,
            Duration::from_secs(2),
        );
        let bucket = &history.buckets[0];

        assert_eq!(
            bucket.cpu_percent.average.combined,
            bucket.cpu_percent.average.core + bucket.cpu_percent.average.external
        );
        assert_near(bucket.cpu_percent.average.core, 40.0);
        assert_near(bucket.cpu_percent.average.external, 130.0 / 3.0);
        assert_eq!(
            bucket.rss_bytes.average,
            SplitMetric {
                combined: 3_033,
                core: 1_033,
                external: 2_000,
            }
        );
        assert_eq!(
            bucket.cpu_percent.peak,
            SplitMetric {
                combined: 110.0,
                core: 10.0,
                external: 100.0,
            }
        );
        assert_eq!(
            bucket.rss_bytes.peak,
            SplitMetric {
                combined: 6_000,
                core: 1_000,
                external: 5_000,
            }
        );
        assert_eq!(
            bucket.max_process_count,
            SplitMetric {
                combined: 2,
                core: 1,
                external: 1,
            }
        );
        assert_eq!(
            history.total_cpu_seconds_approx.combined,
            history.total_cpu_seconds_approx.core + history.total_cpu_seconds_approx.external
        );
        assert_near(history.total_cpu_seconds_approx.core, 2.4);
        assert_near(history.total_cpu_seconds_approx.external, 2.6);
    }

    #[test]
    fn process_summaries_retain_attribution_and_first_seen_order() {
        let history = aggregate_history(
            &split_history_samples(),
            None,
            4_000,
            3_000,
            3_000,
            Duration::from_secs(2),
        );

        assert_eq!(
            history
                .processes
                .iter()
                .map(|process| process.process_key.as_str())
                .collect::<Vec<_>>(),
            ["10:100", "20:200"]
        );
        let provider = &history.processes[1];
        assert_eq!(provider.pid, 20);
        assert_eq!(provider.scope, AttributionScope::External);
        assert_eq!(provider.kind, AttributionKind::Provider);
        assert_eq!(provider.label, "external/provider");
        assert_eq!(provider.confidence, AttributionConfidence::Inherited);
        assert_eq!(provider.first_seen_at_ms, 1_000);
        assert_eq!(provider.last_seen_at_ms, 3_000);
        assert_eq!(provider.sample_count, 3);
    }

    #[test]
    fn every_ui_coverage_state_survives_history_aggregation() {
        for status in [
            UiCoverageStatus::Available,
            UiCoverageStatus::Partial,
            UiCoverageStatus::Unavailable,
            UiCoverageStatus::NotApplicable,
        ] {
            let retained = vec![history_sample(1_000, 1.0, 1, 2.0, 2, status)];
            let history =
                aggregate_history(&retained, None, 2_000, 1_000, 1_000, Duration::from_secs(2));

            assert_eq!(history.ui_coverage.status, status);
            assert_eq!(
                history.ui_coverage.message.as_deref(),
                Some("fixture coverage")
            );
        }
    }

    #[test]
    fn interior_bucket_boundary_sample_is_counted_once_in_the_following_bucket() {
        let retained = vec![history_sample(
            1_000,
            25.0,
            100,
            0.0,
            0,
            UiCoverageStatus::Partial,
        )];

        let history =
            aggregate_history(&retained, None, 3_000, 3_000, 1_000, Duration::from_secs(1));

        assert_eq!(history.buckets.len(), 3);
        assert_eq!(history.buckets[0].cpu_percent.average.combined, 0.0);
        assert_eq!(history.buckets[1].cpu_percent.average.combined, 25.0);
        assert_eq!(history.buckets[2].cpu_percent.average.combined, 0.0);
        assert_near(history.total_cpu_seconds_approx.combined, 0.25);
    }
}
