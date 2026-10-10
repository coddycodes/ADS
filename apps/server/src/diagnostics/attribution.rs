use std::collections::{HashMap, HashSet};

use super::{
    PROCESS_CLAIM_LABEL_MAX_SCALARS, ProcessIdentity, ProcessRow, UiCoverage,
    bound_diagnostic_string,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttributionScope {
    Core,
    External,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttributionKind {
    Server,
    Ui,
    Provider,
    Terminal,
    Helper,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttributionConfidence {
    Exact,
    Inherited,
    Fallback,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProcessClaim {
    pub identity: ProcessIdentity,
    pub scope: AttributionScope,
    pub kind: AttributionKind,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AttributedProcess {
    pub identity: ProcessIdentity,
    pub process_key: String,
    pub scope: AttributionScope,
    pub kind: AttributionKind,
    pub label: String,
    pub confidence: AttributionConfidence,
    pub cpu_percent: f64,
    pub rss_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ProcessResourceTotals {
    pub cpu_percent: f64,
    pub rss_bytes: u64,
    pub process_count: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ProcessAttributionTotals {
    pub combined: ProcessResourceTotals,
    pub core: ProcessResourceTotals,
    pub external: ProcessResourceTotals,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProcessAttribution {
    pub processes: Vec<AttributedProcess>,
    pub totals: ProcessAttributionTotals,
    pub ui_coverage: UiCoverage,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ResourceAttributor;

impl ResourceAttributor {
    #[must_use]
    pub fn attribute(
        rows: &[ProcessRow],
        server_identity: ProcessIdentity,
        claims: &[ProcessClaim],
        ui_coverage: UiCoverage,
    ) -> ProcessAttribution {
        let rows_by_identity = unique_rows(rows);
        let rows_by_pid = rows
            .iter()
            .map(|row| {
                let identity = ProcessIdentity {
                    pid: row.pid,
                    started_at: row.started_at,
                };
                (identity.pid, identity)
            })
            .collect::<HashMap<_, _>>();
        let children_by_pid = child_index(rows, &rows_by_identity);
        let claims_by_identity = claims
            .iter()
            .filter(|claim| rows_by_identity.contains_key(&claim.identity))
            .map(|claim| (claim.identity, claim))
            .collect::<HashMap<_, _>>();

        let mut roots = claims_by_identity.keys().copied().collect::<Vec<_>>();
        if rows_by_identity.contains_key(&server_identity) {
            roots.push(server_identity);
        }
        let included = included_identities(&roots, &children_by_pid, &rows_by_identity);
        let mut owner_memo = HashMap::new();
        let mut emitted = HashSet::new();
        let processes = rows
            .iter()
            .filter_map(|sampled_row| {
                let identity = ProcessIdentity {
                    pid: sampled_row.pid,
                    started_at: sampled_row.started_at,
                };
                if !emitted.insert(identity) || !included.contains(&identity) {
                    return None;
                }
                let row = rows_by_identity.get(&identity)?;
                let (scope, kind, label, confidence) = if identity == server_identity {
                    (
                        AttributionScope::Core,
                        AttributionKind::Server,
                        "core/server".to_owned(),
                        AttributionConfidence::Exact,
                    )
                } else if let Some(claim) = claims_by_identity.get(&identity) {
                    (
                        claim.scope,
                        claim.kind,
                        bound_diagnostic_string(&claim.label, PROCESS_CLAIM_LABEL_MAX_SCALARS),
                        AttributionConfidence::Exact,
                    )
                } else if let Some(owner) = nearest_claim(
                    identity,
                    &rows_by_identity,
                    &rows_by_pid,
                    &claims_by_identity,
                    &mut owner_memo,
                    &mut HashSet::new(),
                ) {
                    let claim = claims_by_identity.get(&owner)?;
                    (
                        claim.scope,
                        claim.kind,
                        bound_diagnostic_string(&claim.label, PROCESS_CLAIM_LABEL_MAX_SCALARS),
                        AttributionConfidence::Inherited,
                    )
                } else {
                    (
                        AttributionScope::External,
                        AttributionKind::Unknown,
                        "external/unknown/fallback".to_owned(),
                        AttributionConfidence::Fallback,
                    )
                };

                Some(AttributedProcess {
                    identity,
                    process_key: identity.key(),
                    scope,
                    kind,
                    label,
                    confidence,
                    cpu_percent: f64::from(row.cpu_percent),
                    rss_bytes: row.rss_bytes,
                })
            })
            .collect::<Vec<_>>();
        let totals = aggregate_totals(&processes);

        ProcessAttribution {
            processes,
            totals,
            ui_coverage,
        }
    }
}

fn unique_rows(rows: &[ProcessRow]) -> HashMap<ProcessIdentity, &ProcessRow> {
    rows.iter()
        .map(|row| {
            (
                ProcessIdentity {
                    pid: row.pid,
                    started_at: row.started_at,
                },
                row,
            )
        })
        .collect()
}

fn child_index(
    rows: &[ProcessRow],
    rows_by_identity: &HashMap<ProcessIdentity, &ProcessRow>,
) -> HashMap<u32, Vec<ProcessIdentity>> {
    let mut children = HashMap::<u32, Vec<ProcessIdentity>>::new();
    let mut indexed = HashSet::new();
    for sampled_row in rows {
        let identity = ProcessIdentity {
            pid: sampled_row.pid,
            started_at: sampled_row.started_at,
        };
        if !indexed.insert(identity) {
            continue;
        }
        let Some(row) = rows_by_identity.get(&identity) else {
            continue;
        };
        children.entry(row.ppid).or_default().push(identity);
    }
    children
}

fn included_identities(
    roots: &[ProcessIdentity],
    children_by_pid: &HashMap<u32, Vec<ProcessIdentity>>,
    rows_by_identity: &HashMap<ProcessIdentity, &ProcessRow>,
) -> HashSet<ProcessIdentity> {
    let mut included = HashSet::new();
    let mut stack = roots.to_vec();
    while let Some(identity) = stack.pop() {
        if !included.insert(identity) {
            continue;
        }
        if let Some(children) = children_by_pid.get(&identity.pid) {
            stack.extend(
                children
                    .iter()
                    .copied()
                    .filter(|child| parent_edge_is_current(identity, *child, rows_by_identity)),
            );
        }
    }
    included
}

fn nearest_claim(
    identity: ProcessIdentity,
    rows_by_identity: &HashMap<ProcessIdentity, &ProcessRow>,
    rows_by_pid: &HashMap<u32, ProcessIdentity>,
    claims_by_identity: &HashMap<ProcessIdentity, &ProcessClaim>,
    memo: &mut HashMap<ProcessIdentity, Option<ProcessIdentity>>,
    visiting: &mut HashSet<ProcessIdentity>,
) -> Option<ProcessIdentity> {
    if let Some(owner) = memo.get(&identity) {
        return *owner;
    }
    if !visiting.insert(identity) {
        return None;
    }
    let owner = if claims_by_identity.contains_key(&identity) {
        Some(identity)
    } else {
        rows_by_identity
            .get(&identity)
            .and_then(|row| rows_by_pid.get(&row.ppid))
            .copied()
            .filter(|parent| parent_edge_is_current(*parent, identity, rows_by_identity))
            .and_then(|parent| {
                nearest_claim(
                    parent,
                    rows_by_identity,
                    rows_by_pid,
                    claims_by_identity,
                    memo,
                    visiting,
                )
            })
    };
    visiting.remove(&identity);
    memo.insert(identity, owner);
    owner
}

fn parent_edge_is_current(
    parent: ProcessIdentity,
    child: ProcessIdentity,
    rows_by_identity: &HashMap<ProcessIdentity, &ProcessRow>,
) -> bool {
    rows_by_identity
        .get(&child)
        .is_some_and(|row| row.ppid == parent.pid && parent.started_at <= child.started_at)
}

fn aggregate_totals(processes: &[AttributedProcess]) -> ProcessAttributionTotals {
    let mut totals = ProcessAttributionTotals::default();
    for process in processes {
        let group = match process.scope {
            AttributionScope::Core => &mut totals.core,
            AttributionScope::External => &mut totals.external,
        };
        group.cpu_percent += process.cpu_percent;
        group.rss_bytes = group.rss_bytes.saturating_add(process.rss_bytes);
        group.process_count += 1;
    }
    totals.combined = add_totals(totals.core, totals.external);
    totals
}

fn add_totals(left: ProcessResourceTotals, right: ProcessResourceTotals) -> ProcessResourceTotals {
    ProcessResourceTotals {
        cpu_percent: left.cpu_percent + right.cpu_percent,
        rss_bytes: left.rss_bytes.saturating_add(right.rss_bytes),
        process_count: left.process_count.saturating_add(right.process_count),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::diagnostics::{
        ProcessAttributionRegistry, ProcessIdentity, ProcessRegistrationMetadata, ProcessRow,
        RegistrationSource,
    };

    fn row(pid: u32, ppid: u32, started_at: u64, cpu: f32, rss: u64) -> ProcessRow {
        ProcessRow {
            pid,
            started_at,
            ppid,
            pgid: None,
            status: "Run".to_owned(),
            cpu_percent: cpu,
            cpu_core_percent: Some(cpu),
            rss_bytes: rss,
            elapsed: "00:00:00".to_owned(),
            command: format!("process-{pid}"),
        }
    }

    fn identity(pid: u32, started_at: u64) -> ProcessIdentity {
        ProcessIdentity { pid, started_at }
    }

    fn claim(
        pid: u32,
        started_at: u64,
        scope: AttributionScope,
        kind: AttributionKind,
        label: &str,
    ) -> ProcessClaim {
        ProcessClaim {
            identity: identity(pid, started_at),
            scope,
            kind,
            label: label.to_owned(),
        }
    }

    #[test]
    fn attributes_registered_roots_and_server_descendants() {
        let rows = [
            row(1, 0, 10, 1.0, 100),
            row(2, 1, 20, 2.0, 200),
            row(3, 1, 30, 3.0, 300),
            row(4, 2, 40, 4.0, 400),
            row(5, 1, 50, 5.0, 500),
            row(6, 0, 60, 6.0, 600),
        ];
        let attribution = ResourceAttributor::attribute(
            &rows,
            identity(1, 10),
            &[
                claim(
                    2,
                    20,
                    AttributionScope::Core,
                    AttributionKind::Ui,
                    "core/ui",
                ),
                claim(
                    3,
                    30,
                    AttributionScope::External,
                    AttributionKind::Provider,
                    "external/provider",
                ),
            ],
            UiCoverage::default(),
        );

        let cases = [
            (
                identity(1, 10),
                AttributionScope::Core,
                AttributionKind::Server,
                "core/server",
                AttributionConfidence::Exact,
            ),
            (
                identity(2, 20),
                AttributionScope::Core,
                AttributionKind::Ui,
                "core/ui",
                AttributionConfidence::Exact,
            ),
            (
                identity(3, 30),
                AttributionScope::External,
                AttributionKind::Provider,
                "external/provider",
                AttributionConfidence::Exact,
            ),
            (
                identity(4, 40),
                AttributionScope::Core,
                AttributionKind::Ui,
                "core/ui",
                AttributionConfidence::Inherited,
            ),
            (
                identity(5, 50),
                AttributionScope::External,
                AttributionKind::Unknown,
                "external/unknown/fallback",
                AttributionConfidence::Fallback,
            ),
        ];

        for (identity, scope, kind, label, confidence) in cases {
            let process = attribution
                .processes
                .iter()
                .find(|process| process.identity == identity)
                .expect("included process");
            assert_eq!(process.scope, scope);
            assert_eq!(process.kind, kind);
            assert_eq!(process.label, label);
            assert_eq!(process.confidence, confidence);
            assert_eq!(process.process_key, identity.key());
        }
        assert!(
            attribution
                .processes
                .iter()
                .all(|process| process.identity != identity(6, 60))
        );
    }

    #[test]
    fn nearest_exact_root_wins_and_identities_are_deduplicated() {
        let rows = [
            row(1, 0, 10, 1.0, 100),
            row(2, 1, 20, 2.0, 200),
            row(3, 2, 30, 3.0, 300),
            row(3, 2, 30, 30.0, 3_000),
            row(4, 3, 40, 4.0, 400),
        ];
        let attribution = ResourceAttributor::attribute(
            &rows,
            identity(1, 10),
            &[
                claim(
                    2,
                    20,
                    AttributionScope::Core,
                    AttributionKind::Ui,
                    "core/ui",
                ),
                claim(
                    3,
                    30,
                    AttributionScope::External,
                    AttributionKind::Provider,
                    "external/provider",
                ),
            ],
            UiCoverage::default(),
        );

        assert_eq!(attribution.processes.len(), 4);
        let provider = attribution
            .processes
            .iter()
            .find(|process| process.identity == identity(3, 30))
            .expect("registered provider");
        assert_eq!(provider.kind, AttributionKind::Provider);
        assert_eq!(provider.confidence, AttributionConfidence::Exact);
        let provider_descendant = attribution
            .processes
            .iter()
            .find(|process| process.identity == identity(4, 40))
            .expect("provider descendant");
        assert_eq!(provider_descendant.kind, AttributionKind::Provider);
        assert_eq!(
            provider_descendant.confidence,
            AttributionConfidence::Inherited
        );
    }

    #[test]
    fn ignores_reused_parent_pid_with_a_later_start_time() {
        let rows = [
            row(1, 0, 10, 1.0, 100),
            row(42, 1, 30, 2.0, 200),
            row(9, 42, 20, 3.0, 300),
        ];
        let attribution = ResourceAttributor::attribute(
            &rows,
            identity(1, 10),
            &[claim(
                42,
                30,
                AttributionScope::Core,
                AttributionKind::Ui,
                "core/ui",
            )],
            UiCoverage::default(),
        );

        assert!(
            attribution
                .processes
                .iter()
                .any(|process| process.identity == identity(42, 30))
        );
        assert!(
            attribution
                .processes
                .iter()
                .all(|process| process.identity != identity(9, 20))
        );
    }

    #[test]
    fn bounds_claim_labels_by_unicode_scalar_value() {
        let label = "é".repeat(81);
        let rows = [row(1, 0, 10, 1.0, 100), row(2, 1, 20, 2.0, 200)];
        let attribution = ResourceAttributor::attribute(
            &rows,
            identity(1, 10),
            &[claim(
                2,
                20,
                AttributionScope::Core,
                AttributionKind::Ui,
                &label,
            )],
            UiCoverage::default(),
        );
        let process = attribution
            .processes
            .iter()
            .find(|process| process.identity == identity(2, 20))
            .unwrap();
        assert_eq!(process.label.chars().count(), 80);
    }

    #[test]
    fn retains_reparented_exact_roots_and_rejects_stale_pid_claims() {
        let rows = [
            row(1, 0, 10, 1.0, 100),
            row(8, 0, 80, 8.0, 800),
            row(9, 8, 90, 9.0, 900),
            row(10, 0, 100, 10.0, 1_000),
        ];
        let attribution = ResourceAttributor::attribute(
            &rows,
            identity(1, 10),
            &[
                claim(
                    8,
                    80,
                    AttributionScope::Core,
                    AttributionKind::Ui,
                    "core/ui",
                ),
                claim(
                    10,
                    99,
                    AttributionScope::External,
                    AttributionKind::Provider,
                    "external/provider",
                ),
            ],
            UiCoverage::default(),
        );

        assert!(
            attribution
                .processes
                .iter()
                .any(|process| process.identity == identity(8, 80))
        );
        let inherited = attribution
            .processes
            .iter()
            .find(|process| process.identity == identity(9, 90))
            .expect("reparented root descendant");
        assert_eq!(inherited.confidence, AttributionConfidence::Inherited);
        assert!(
            attribution
                .processes
                .iter()
                .all(|process| process.identity != identity(10, 100))
        );
    }

    #[test]
    fn stale_registry_claim_is_omitted_and_server_descendant_falls_back() {
        let registry = ProcessAttributionRegistry::new();
        let _registration = registry
            .register_identity(
                identity(42, 20),
                ProcessRegistrationMetadata {
                    scope: AttributionScope::External,
                    kind: AttributionKind::Provider,
                    label: "external/provider".to_owned(),
                    source: RegistrationSource::Provider,
                    driver: None,
                },
            )
            .expect("registration should fit");
        let now = Instant::now();
        let _ = registry.bind_and_snapshot(&[row(42, 1, 20, 0.0, 0)], now);
        let rows = [row(1, 0, 10, 1.0, 100), row(42, 1, 30, 2.0, 200)];

        let attribution = ResourceAttributor::attribute(
            &rows,
            identity(1, 10),
            &registry.bind_and_snapshot(&rows, now),
            UiCoverage::default(),
        );

        let process = attribution
            .processes
            .iter()
            .find(|process| process.identity == identity(42, 30))
            .expect("server descendant should be included");
        assert_eq!(process.scope, AttributionScope::External);
        assert_eq!(process.kind, AttributionKind::Unknown);
        assert_eq!(process.label, "external/unknown/fallback");
        assert_eq!(process.confidence, AttributionConfidence::Fallback);
    }

    #[test]
    fn totals_reconcile_across_core_and_external_processes() {
        let rows = [
            row(1, 0, 10, 1.0, 100),
            row(2, 1, 20, 2.0, 200),
            row(3, 1, 30, 3.0, 300),
            row(4, 2, 40, 4.0, 400),
            row(5, 1, 50, 5.0, 500),
        ];
        let attribution = ResourceAttributor::attribute(
            &rows,
            identity(1, 10),
            &[
                claim(
                    2,
                    20,
                    AttributionScope::Core,
                    AttributionKind::Ui,
                    "core/ui",
                ),
                claim(
                    3,
                    30,
                    AttributionScope::External,
                    AttributionKind::Provider,
                    "external/provider",
                ),
            ],
            UiCoverage::default(),
        );

        assert_eq!(
            attribution.totals.core,
            ProcessResourceTotals {
                cpu_percent: 7.0,
                rss_bytes: 700,
                process_count: 3,
            }
        );
        assert_eq!(
            attribution.totals.external,
            ProcessResourceTotals {
                cpu_percent: 8.0,
                rss_bytes: 800,
                process_count: 2,
            }
        );
        assert_eq!(
            attribution.totals.combined,
            ProcessResourceTotals {
                cpu_percent: 15.0,
                rss_bytes: 1_500,
                process_count: 5,
            }
        );
    }

    #[test]
    fn attributed_processes_preserve_input_row_order_without_comparison_sorting() {
        let rows = [
            row(90, 70, 900, 9.0, 900),
            row(5, 0, 50, 5.0, 500),
            row(50, 0, 500, 1.0, 100),
            row(7, 50, 70, 2.0, 200),
            row(70, 50, 700, 3.0, 300),
            row(8, 7, 80, 4.0, 400),
        ];
        let attribution = ResourceAttributor::attribute(
            &rows,
            identity(50, 500),
            &[
                claim(
                    7,
                    70,
                    AttributionScope::Core,
                    AttributionKind::Ui,
                    "core/ui",
                ),
                claim(
                    70,
                    700,
                    AttributionScope::External,
                    AttributionKind::Provider,
                    "external/provider",
                ),
            ],
            UiCoverage::default(),
        );

        assert_eq!(
            attribution
                .processes
                .iter()
                .map(|process| process.identity)
                .collect::<Vec<_>>(),
            [
                identity(90, 900),
                identity(50, 500),
                identity(7, 70),
                identity(70, 700),
                identity(8, 80),
            ]
        );
    }
}
