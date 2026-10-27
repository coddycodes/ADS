use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    ffi::OsStr,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};

use coddy_studio_server::diagnostics::{
    DesktopUiObservation, DesktopUiProcessObserver, NativeProcessRecord, NativeProcessSampler,
    ProcessIdentity, ProcessRow, UiCoverage, UiCoverageStatus,
};

fn read_process_executable(pid: u32) -> Result<PathBuf, ()> {
    let executable = std::fs::read_link(format!("/proc/{pid}/exe")).map_err(|_| ())?;
    validate_process_executable_path(executable)
}

fn validate_process_executable_path(executable: PathBuf) -> Result<PathBuf, ()> {
    let metadata = std::fs::metadata(&executable).map_err(|_| ())?;
    metadata.is_file().then_some(executable).ok_or(())
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct LinuxDesktopUiProcessObserver;

impl LinuxDesktopUiProcessObserver {
    pub(super) fn new() -> Self {
        Self
    }
}

impl DesktopUiProcessObserver for LinuxDesktopUiProcessObserver {
    fn observe(
        &self,
        rows: Arc<[ProcessRow]>,
        server_identity: ProcessIdentity,
    ) -> Pin<Box<dyn Future<Output = DesktopUiObservation> + Send + '_>> {
        Box::pin(async move {
            build_observation_with(
                &rows,
                server_identity,
                |pid| NativeProcessSampler::process_record(pid).map_err(|_| ()),
                read_process_executable,
            )
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum WebKitGtkProcessRole {
    Web,
    Network,
    Gpu,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum LinuxObservationIssue {
    ServerSnapshot,
    ParentEdge,
    ProcessRecord,
    Executable,
    RoleMismatch,
    UnsupportedRole,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WebKitGtkCandidateHint {
    Supported(WebKitGtkProcessRole),
    Unsupported,
    Conflicting,
}

impl WebKitGtkCandidateHint {
    fn merge(self, other: Self) -> Self {
        match (self, other) {
            (Self::Supported(left), Self::Supported(right)) if left == right => self,
            (Self::Unsupported, Self::Unsupported) => self,
            _ => Self::Conflicting,
        }
    }
}

impl WebKitGtkProcessRole {
    fn from_executable_name(name: &str) -> Option<Self> {
        match name {
            "WebKitWebProcess" => Some(Self::Web),
            "WebKitNetworkProcess" => Some(Self::Network),
            "WebKitGPUProcess" => Some(Self::Gpu),
            _ => None,
        }
    }

    fn executable_name(self) -> &'static str {
        match self {
            Self::Web => "WebKitWebProcess",
            Self::Network => "WebKitNetworkProcess",
            Self::Gpu => "WebKitGPUProcess",
        }
    }
}

fn command_executable_name(command: &str) -> Option<&str> {
    Path::new(command.split_ascii_whitespace().next()?)
        .file_name()
        .and_then(OsStr::to_str)
}

fn is_webkit_like_process_name(name: &str) -> bool {
    name.starts_with("WebKit") && name.ends_with("Process")
}

fn candidate_hint(command: &str) -> Option<WebKitGtkCandidateHint> {
    let executable_name = command_executable_name(command)?;
    if !is_webkit_like_process_name(executable_name) {
        return None;
    }
    Some(
        WebKitGtkProcessRole::from_executable_name(executable_name).map_or(
            WebKitGtkCandidateHint::Unsupported,
            WebKitGtkCandidateHint::Supported,
        ),
    )
}

const LINUX_UI_CANDIDATE_LIMIT: usize = 64;

fn build_observation_with(
    rows: &[ProcessRow],
    server_identity: ProcessIdentity,
    mut record_for: impl FnMut(u32) -> Result<NativeProcessRecord, ()>,
    mut executable_for: impl FnMut(u32) -> Result<PathBuf, ()>,
) -> DesktopUiObservation {
    let mut issues = BTreeSet::new();
    if !rows
        .iter()
        .any(|row| row.pid == server_identity.pid && row.started_at == server_identity.started_at)
    {
        issues.insert(LinuxObservationIssue::ServerSnapshot);
        return DesktopUiObservation {
            identities: Vec::new(),
            coverage: coverage_for(false, &issues),
        };
    }

    let mut candidates = BTreeMap::<(u32, u64), (&ProcessRow, WebKitGtkCandidateHint)>::new();
    for row in rows.iter().filter(|row| row.ppid == server_identity.pid) {
        let Some(hint) = candidate_hint(&row.command) else {
            continue;
        };
        let key = (row.pid, row.started_at);
        if let Some((_, existing_hint)) = candidates.get_mut(&key) {
            *existing_hint = existing_hint.merge(hint);
            continue;
        }

        if candidates.len() == LINUX_UI_CANDIDATE_LIMIT {
            let largest_key = *candidates
                .last_key_value()
                .expect("a full candidate map has a largest key")
                .0;
            issues.insert(LinuxObservationIssue::ProcessRecord);
            if key >= largest_key {
                continue;
            }
            candidates.pop_last();
        }
        candidates.insert(key, (row, hint));
    }

    let mut accepted = HashSet::new();
    for (_, (row, hint)) in candidates {
        let role = match hint {
            WebKitGtkCandidateHint::Supported(role) => role,
            WebKitGtkCandidateHint::Unsupported => {
                issues.insert(LinuxObservationIssue::UnsupportedRole);
                continue;
            }
            WebKitGtkCandidateHint::Conflicting => {
                issues.insert(LinuxObservationIssue::RoleMismatch);
                continue;
            }
        };

        match validate_candidate(
            row,
            server_identity,
            role,
            &mut record_for,
            &mut executable_for,
        ) {
            Ok(identity) => {
                accepted.insert(identity);
            }
            Err(issue) => {
                issues.insert(issue);
            }
        }
    }

    let mut identities = accepted.into_iter().collect::<Vec<_>>();
    identities.sort_unstable_by_key(|identity| (identity.pid, identity.started_at));
    DesktopUiObservation {
        coverage: coverage_for(!identities.is_empty(), &issues),
        identities,
    }
}

fn validate_candidate(
    row: &ProcessRow,
    server_identity: ProcessIdentity,
    role: WebKitGtkProcessRole,
    record_for: &mut impl FnMut(u32) -> Result<NativeProcessRecord, ()>,
    executable_for: &mut impl FnMut(u32) -> Result<PathBuf, ()>,
) -> Result<ProcessIdentity, LinuxObservationIssue> {
    if server_identity.started_at > row.started_at {
        return Err(LinuxObservationIssue::ParentEdge);
    }
    let expected = NativeProcessRecord {
        identity: ProcessIdentity {
            pid: row.pid,
            started_at: row.started_at,
        },
        ppid: server_identity.pid,
    };
    let before = record_for(row.pid).map_err(|()| LinuxObservationIssue::ProcessRecord)?;
    if before != expected {
        return Err(LinuxObservationIssue::ProcessRecord);
    }
    let executable = executable_for(row.pid).map_err(|()| LinuxObservationIssue::Executable)?;
    if executable.file_name().and_then(OsStr::to_str) != Some(role.executable_name()) {
        return Err(LinuxObservationIssue::RoleMismatch);
    }
    let after = record_for(row.pid).map_err(|()| LinuxObservationIssue::ProcessRecord)?;
    if after != before {
        return Err(LinuxObservationIssue::ProcessRecord);
    }
    Ok(expected.identity)
}

fn coverage_for(has_identities: bool, issues: &BTreeSet<LinuxObservationIssue>) -> UiCoverage {
    let status = if !has_identities {
        UiCoverageStatus::Unavailable
    } else if issues.is_empty() {
        UiCoverageStatus::Available
    } else {
        UiCoverageStatus::Partial
    };
    let message =
        (status != UiCoverageStatus::Available).then(|| bounded_issue_message(status, issues));
    UiCoverage { status, message }
}

const UI_UNAVAILABLE_MESSAGE: &str =
    "Native server usage is included, but local UI/WebView usage could not be associated reliably.";

fn bounded_issue_message(
    status: UiCoverageStatus,
    issues: &BTreeSet<LinuxObservationIssue>,
) -> String {
    let boundary = match issues.iter().next() {
        Some(LinuxObservationIssue::ServerSnapshot) => {
            "The native server process was absent from the process snapshot."
        }
        Some(LinuxObservationIssue::ParentEdge) => {
            "A WebKitGTK process parent edge could not be validated."
        }
        Some(LinuxObservationIssue::ProcessRecord) => {
            "A WebKitGTK process identity could not be validated."
        }
        Some(LinuxObservationIssue::Executable) => {
            "A WebKitGTK process executable could not be resolved."
        }
        Some(LinuxObservationIssue::RoleMismatch) => {
            "A WebKitGTK process executable did not match its reported role."
        }
        Some(LinuxObservationIssue::UnsupportedRole) => {
            "An unsupported WebKitGTK process role was observed."
        }
        None => "No supported WebKitGTK UI process identity was available.",
    };

    if status == UiCoverageStatus::Unavailable {
        format!("{boundary} {UI_UNAVAILABLE_MESSAGE}")
    } else {
        boundary.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::RefCell,
        collections::{BTreeSet, HashMap, VecDeque},
        path::PathBuf,
        sync::Arc,
    };

    use coddy_studio_server::diagnostics::{
        NativeProcessRecord, NativeProcessSampler, ProcessIdentity, ProcessRow, UiCoverageStatus,
    };

    use super::{
        LinuxObservationIssue, build_observation_with, coverage_for, read_process_executable,
        validate_process_executable_path,
    };

    const SERVER_PID: u32 = 410;
    const SERVER_STARTED_AT: u64 = 100;

    fn row(pid: u32, ppid: u32, started_at: u64, command: &str) -> ProcessRow {
        let mut row = ProcessRow::fixture(pid, ppid, command);
        row.started_at = started_at;
        row
    }

    fn server_identity() -> ProcessIdentity {
        ProcessIdentity {
            pid: SERVER_PID,
            started_at: SERVER_STARTED_AT,
        }
    }

    fn record(pid: u32, ppid: u32, started_at: u64) -> NativeProcessRecord {
        NativeProcessRecord {
            identity: ProcessIdentity { pid, started_at },
            ppid,
        }
    }

    fn stable_records(rows: &[ProcessRow]) -> HashMap<u32, NativeProcessRecord> {
        rows.iter()
            .map(|row| (row.pid, record(row.pid, row.ppid, row.started_at)))
            .collect()
    }

    fn executable_paths(rows: &[ProcessRow]) -> HashMap<u32, PathBuf> {
        rows.iter()
            .filter_map(|row| {
                let executable = row.command.split_ascii_whitespace().next()?;
                Some((row.pid, PathBuf::from(executable)))
            })
            .collect()
    }

    fn observe(
        rows: Arc<[ProcessRow]>,
        records: HashMap<u32, NativeProcessRecord>,
        executables: HashMap<u32, PathBuf>,
    ) -> coddy_studio_server::diagnostics::DesktopUiObservation {
        build_observation_with(
            &rows,
            server_identity(),
            |pid| records.get(&pid).copied().ok_or(()),
            |pid| executables.get(&pid).cloned().ok_or(()),
        )
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_ui_native_record_and_executable_reader_validate_the_current_process() {
        let record = NativeProcessSampler::process_record(std::process::id())
            .expect("current process record");
        let executable = read_process_executable(std::process::id()).expect("current executable");

        assert_eq!(record.identity.pid, std::process::id());
        assert_ne!(record.identity.started_at, 0);
        assert!(executable.is_absolute());
        assert!(executable.is_file());
    }

    #[test]
    fn linux_ui_rejects_exact_role_directory_as_executable() {
        // Mutation caught: omitting the regular-file check accepts a non-executable directory
        // whose basename exactly matches an allowed WebKitGTK process role.
        let temporary = tempfile::tempdir().expect("temporary executable fixture");
        let directory = temporary.path().join("WebKitWebProcess");
        std::fs::create_dir(&directory).expect("exact-role directory fixture");

        assert_eq!(validate_process_executable_path(directory), Err(()));
    }

    #[test]
    fn linux_ui_accepts_exact_web_network_and_gpu_children() {
        // Mutation caught: accepting only one role, traversing input order, or returning
        // non-snapshot identities loses or misorders a validated WebKitGTK helper.
        let rows: Arc<[ProcessRow]> = Arc::from([
            row(SERVER_PID, 1, 100, "/app/bin/coddy-studio-desktop"),
            row(
                501,
                SERVER_PID,
                200,
                "/usr/libexec/webkit2gtk-4.1/WebKitWebProcess --web-process",
            ),
            row(
                502,
                SERVER_PID,
                201,
                "/usr/libexec/webkit2gtk-4.1/WebKitNetworkProcess",
            ),
            row(
                503,
                SERVER_PID,
                202,
                "/usr/libexec/webkit2gtk-4.1/WebKitGPUProcess",
            ),
        ]);

        let observation = observe(rows.clone(), stable_records(&rows), executable_paths(&rows));

        assert_eq!(
            observation.identities,
            vec![
                ProcessIdentity {
                    pid: 501,
                    started_at: 200,
                },
                ProcessIdentity {
                    pid: 502,
                    started_at: 201,
                },
                ProcessIdentity {
                    pid: 503,
                    started_at: 202,
                },
            ]
        );
        assert_eq!(observation.coverage.status, UiCoverageStatus::Available);
        assert_eq!(observation.coverage.message, None);
    }

    #[test]
    fn linux_ui_deduplicates_shared_helpers() {
        // Mutation caught: validating duplicate snapshot hints repeatedly or collecting into a
        // Vec can perform unbounded duplicate work and return the same stable identity twice.
        let rows: Arc<[ProcessRow]> = Arc::from([
            row(SERVER_PID, 1, 100, "/app/bin/coddy-studio-desktop"),
            row(503, SERVER_PID, 203, "/usr/lib/WebKitGPUProcess"),
            row(501, SERVER_PID, 201, "/usr/lib/WebKitWebProcess"),
            row(503, SERVER_PID, 203, "/usr/lib/WebKitGPUProcess --shared"),
            row(501, SERVER_PID, 201, "/usr/lib/WebKitWebProcess --shared"),
        ]);
        let records = stable_records(&rows);
        let executables = executable_paths(&rows);
        let record_queries = RefCell::new(Vec::new());
        let executable_queries = RefCell::new(Vec::new());

        let observation = build_observation_with(
            &rows,
            server_identity(),
            |pid| {
                record_queries.borrow_mut().push(pid);
                records.get(&pid).copied().ok_or(())
            },
            |pid| {
                executable_queries.borrow_mut().push(pid);
                executables.get(&pid).cloned().ok_or(())
            },
        );

        assert_eq!(
            observation.identities,
            vec![
                ProcessIdentity {
                    pid: 501,
                    started_at: 201,
                },
                ProcessIdentity {
                    pid: 503,
                    started_at: 203,
                },
            ]
        );
        assert_eq!(&*record_queries.borrow(), &[501, 501, 503, 503]);
        assert_eq!(&*executable_queries.borrow(), &[501, 503]);
        assert_eq!(observation.coverage.status, UiCoverageStatus::Available);
    }

    #[test]
    fn linux_ui_bounds_candidate_validation_before_native_reads() {
        // Mutation caught: applying the 64-identity limit only after validation performs native
        // work for every hinted child and can incorrectly report complete coverage.
        let mut rows = vec![row(
            SERVER_PID,
            1,
            SERVER_STARTED_AT,
            "/app/bin/coddy-studio-desktop",
        )];
        for pid in (500..=565).rev() {
            rows.push(row(
                pid,
                SERVER_PID,
                1_000 + u64::from(pid),
                "/usr/lib/WebKitWebProcess",
            ));
        }
        let rows: Arc<[ProcessRow]> = Arc::from(rows.into_boxed_slice());
        let records = stable_records(&rows);
        let record_queries = RefCell::new(Vec::new());
        let executable_queries = RefCell::new(Vec::new());

        let observation = build_observation_with(
            &rows,
            server_identity(),
            |pid| {
                record_queries.borrow_mut().push(pid);
                records.get(&pid).copied().ok_or(())
            },
            |pid| {
                executable_queries.borrow_mut().push(pid);
                Ok(PathBuf::from("/usr/lib/WebKitWebProcess"))
            },
        );

        let expected_record_queries = (500..=563).flat_map(|pid| [pid, pid]).collect::<Vec<_>>();
        let expected_executable_queries = (500..=563).collect::<Vec<_>>();
        assert_eq!(&*record_queries.borrow(), &expected_record_queries);
        assert_eq!(&*executable_queries.borrow(), &expected_executable_queries);
        assert_eq!(
            observation
                .identities
                .iter()
                .map(|identity| identity.pid)
                .collect::<Vec<_>>(),
            expected_executable_queries
        );
        assert_eq!(observation.identities.len(), 64);
        assert_eq!(observation.coverage.status, UiCoverageStatus::Partial);
        assert_eq!(
            observation.coverage.message.as_deref(),
            Some("A WebKitGTK process identity could not be validated.")
        );
    }

    #[test]
    fn linux_ui_orders_reused_pid_candidates_by_start_identity() {
        // Mutation caught: sorting accepted identities only by PID leaves equal-PID rows in
        // hash-iteration order rather than the deterministic candidate identity order.
        let mut rows = vec![row(
            SERVER_PID,
            1,
            SERVER_STARTED_AT,
            "/app/bin/coddy-studio-desktop",
        )];
        for started_at in (200..=207).rev() {
            rows.push(row(
                501,
                SERVER_PID,
                started_at,
                "/usr/lib/WebKitWebProcess",
            ));
        }
        let rows: Arc<[ProcessRow]> = Arc::from(rows.into_boxed_slice());
        let mut records = (200..=207)
            .flat_map(|started_at| {
                let current = Ok(record(501, SERVER_PID, started_at));
                [current, current]
            })
            .collect::<VecDeque<_>>();

        let observation = build_observation_with(
            &rows,
            server_identity(),
            |_| records.pop_front().expect("bounded record query"),
            |_| Ok(PathBuf::from("/usr/lib/WebKitWebProcess")),
        );

        assert_eq!(
            observation
                .identities
                .iter()
                .map(|identity| identity.started_at)
                .collect::<Vec<_>>(),
            vec![200, 201, 202, 203, 204, 205, 206, 207]
        );
        assert!(records.is_empty());
        assert_eq!(observation.coverage.status, UiCoverageStatus::Available);
    }

    #[test]
    fn linux_ui_classifies_duplicate_hints_independently_of_row_order() {
        // Mutation caught: deduplicating before hint classification lets an ordinary duplicate
        // suppress a later valid WebKit hint for the same snapshot identity.
        let server = row(
            SERVER_PID,
            1,
            SERVER_STARTED_AT,
            "/app/bin/coddy-studio-desktop",
        );
        let ordinary = row(501, SERVER_PID, 200, "/usr/bin/ordinary-helper");
        let web = row(501, SERVER_PID, 200, "/usr/lib/WebKitWebProcess");

        for duplicate_rows in [
            [ordinary.clone(), web.clone()],
            [web.clone(), ordinary.clone()],
        ] {
            let rows: Arc<[ProcessRow]> = Arc::from([
                server.clone(),
                duplicate_rows[0].clone(),
                duplicate_rows[1].clone(),
            ]);
            let expected = record(501, SERVER_PID, 200);
            let record_queries = RefCell::new(Vec::new());
            let executable_queries = RefCell::new(Vec::new());

            let observation = build_observation_with(
                &rows,
                server_identity(),
                |pid| {
                    record_queries.borrow_mut().push(pid);
                    Ok(expected)
                },
                |pid| {
                    executable_queries.borrow_mut().push(pid);
                    Ok(PathBuf::from("/usr/lib/WebKitWebProcess"))
                },
            );

            assert_eq!(
                observation.identities,
                vec![ProcessIdentity {
                    pid: 501,
                    started_at: 200,
                }]
            );
            assert_eq!(&*record_queries.borrow(), &[501, 501]);
            assert_eq!(&*executable_queries.borrow(), &[501]);
            assert_eq!(observation.coverage.status, UiCoverageStatus::Available);
        }
    }

    #[test]
    fn linux_ui_rejects_conflicting_duplicate_hints_in_either_order() {
        // Mutation caught: first-row-wins deduplication validates one of two conflicting role
        // hints, making coverage depend on equal-identity snapshot row order.
        let server = row(
            SERVER_PID,
            1,
            SERVER_STARTED_AT,
            "/app/bin/coddy-studio-desktop",
        );
        let web = row(501, SERVER_PID, 200, "/usr/lib/WebKitWebProcess");
        let gpu = row(501, SERVER_PID, 200, "/usr/lib/WebKitGPUProcess");

        for duplicate_rows in [[web.clone(), gpu.clone()], [gpu.clone(), web.clone()]] {
            let rows: Arc<[ProcessRow]> = Arc::from([
                server.clone(),
                duplicate_rows[0].clone(),
                duplicate_rows[1].clone(),
            ]);
            let record_queries = RefCell::new(Vec::new());
            let executable_queries = RefCell::new(Vec::new());

            let observation = build_observation_with(
                &rows,
                server_identity(),
                |pid| {
                    record_queries.borrow_mut().push(pid);
                    Ok(record(501, SERVER_PID, 200))
                },
                |pid| {
                    executable_queries.borrow_mut().push(pid);
                    Ok(PathBuf::from("/usr/lib/WebKitWebProcess"))
                },
            );

            assert!(observation.identities.is_empty());
            assert!(record_queries.borrow().is_empty());
            assert!(executable_queries.borrow().is_empty());
            assert_eq!(observation.coverage.status, UiCoverageStatus::Unavailable);
            assert!(
                observation
                    .coverage
                    .message
                    .as_deref()
                    .is_some_and(|message| message.starts_with(
                        "A WebKitGTK process executable did not match its reported role."
                    ))
            );
        }
    }

    #[test]
    fn linux_ui_rejects_supported_and_unsupported_duplicate_hints_in_either_order() {
        // Mutation caught: merging a supported and unsupported WebKit-like hint by first-row
        // precedence can validate an ambiguous identity or vary with snapshot order.
        let server = row(
            SERVER_PID,
            1,
            SERVER_STARTED_AT,
            "/app/bin/coddy-studio-desktop",
        );
        let web = row(501, SERVER_PID, 200, "/usr/lib/WebKitWebProcess");
        let model = row(501, SERVER_PID, 200, "/usr/lib/WebKitModelProcess");

        for duplicate_rows in [[web.clone(), model.clone()], [model.clone(), web.clone()]] {
            let rows: Arc<[ProcessRow]> = Arc::from([
                server.clone(),
                duplicate_rows[0].clone(),
                duplicate_rows[1].clone(),
            ]);
            let record_queries = RefCell::new(Vec::new());
            let executable_queries = RefCell::new(Vec::new());

            let observation = build_observation_with(
                &rows,
                server_identity(),
                |pid| {
                    record_queries.borrow_mut().push(pid);
                    Ok(record(501, SERVER_PID, 200))
                },
                |pid| {
                    executable_queries.borrow_mut().push(pid);
                    Ok(PathBuf::from("/usr/lib/WebKitWebProcess"))
                },
            );

            assert!(observation.identities.is_empty());
            assert!(record_queries.borrow().is_empty());
            assert!(executable_queries.borrow().is_empty());
            assert_eq!(observation.coverage.status, UiCoverageStatus::Unavailable);
            assert!(
                observation
                    .coverage
                    .message
                    .as_deref()
                    .is_some_and(|message| message.starts_with(
                        "A WebKitGTK process executable did not match its reported role."
                    ))
            );
        }
    }

    #[test]
    fn linux_ui_ignores_same_name_processes_owned_by_another_parent() {
        // Mutation caught: machine-wide executable-name discovery claims another application's
        // exact WebKitGTK helper even though it is not an immediate server child.
        let rows: Arc<[ProcessRow]> = Arc::from([
            row(SERVER_PID, 1, 100, "/app/bin/coddy-studio-desktop"),
            row(700, 1, 120, "/opt/another-app"),
            row(701, 700, 200, "/usr/lib/WebKitWebProcess"),
        ]);
        let record_queries = RefCell::new(Vec::new());
        let executable_queries = RefCell::new(Vec::new());

        let observation = build_observation_with(
            &rows,
            server_identity(),
            |pid| {
                record_queries.borrow_mut().push(pid);
                Err(())
            },
            |pid| {
                executable_queries.borrow_mut().push(pid);
                Err(())
            },
        );

        assert!(observation.identities.is_empty());
        assert_eq!(observation.coverage.status, UiCoverageStatus::Unavailable);
        assert!(record_queries.borrow().is_empty());
        assert!(executable_queries.borrow().is_empty());
    }

    #[test]
    fn linux_ui_ignores_provider_and_terminal_webkit_descendants() {
        // Mutation caught: arbitrary descendant traversal crosses provider and terminal roots
        // and misattributes their WebKit helpers to Core UI.
        let rows: Arc<[ProcessRow]> = Arc::from([
            row(SERVER_PID, 1, 100, "/app/bin/coddy-studio-desktop"),
            row(600, SERVER_PID, 150, "/opt/provider"),
            row(601, 600, 201, "/usr/lib/WebKitNetworkProcess"),
            row(700, SERVER_PID, 151, "/usr/bin/terminal"),
            row(701, 700, 202, "/usr/lib/WebKitGPUProcess"),
        ]);
        let record_queries = RefCell::new(Vec::new());
        let executable_queries = RefCell::new(Vec::new());

        let observation = build_observation_with(
            &rows,
            server_identity(),
            |pid| {
                record_queries.borrow_mut().push(pid);
                Err(())
            },
            |pid| {
                executable_queries.borrow_mut().push(pid);
                Err(())
            },
        );

        assert!(observation.identities.is_empty());
        assert_eq!(observation.coverage.status, UiCoverageStatus::Unavailable);
        assert!(record_queries.borrow().is_empty());
        assert!(executable_queries.borrow().is_empty());
    }

    #[test]
    fn linux_ui_requires_the_exact_server_identity() {
        // Mutation caught: matching only the server PID allows a reused process snapshot to
        // authorize child inspection for a different server lifetime.
        let rows: Arc<[ProcessRow]> = Arc::from([
            row(SERVER_PID, 1, 99, "/app/bin/coddy-studio-desktop"),
            row(501, SERVER_PID, 200, "/usr/lib/WebKitWebProcess"),
        ]);
        let record_queries = RefCell::new(Vec::new());
        let executable_queries = RefCell::new(Vec::new());

        let observation = build_observation_with(
            &rows,
            server_identity(),
            |pid| {
                record_queries.borrow_mut().push(pid);
                Err(())
            },
            |pid| {
                executable_queries.borrow_mut().push(pid);
                Err(())
            },
        );

        assert!(observation.identities.is_empty());
        assert_eq!(observation.coverage.status, UiCoverageStatus::Unavailable);
        assert_eq!(
            observation.coverage.message.as_deref(),
            Some(
                "The native server process was absent from the process snapshot. Native server \
usage is included, but local UI/WebView usage could not be associated reliably."
            )
        );
        assert!(record_queries.borrow().is_empty());
        assert!(executable_queries.borrow().is_empty());
    }

    #[test]
    fn linux_ui_rejects_invalid_parent_start_order() {
        // Mutation caught: trusting PPID alone accepts a child snapshot older than its current
        // parent, which is impossible without identity reuse or stale snapshot data.
        let rows: Arc<[ProcessRow]> = Arc::from([
            row(SERVER_PID, 1, 100, "/app/bin/coddy-studio-desktop"),
            row(501, SERVER_PID, 99, "/usr/lib/WebKitWebProcess"),
        ]);
        let record_queries = RefCell::new(Vec::new());
        let executable_queries = RefCell::new(Vec::new());

        let observation = build_observation_with(
            &rows,
            server_identity(),
            |pid| {
                record_queries.borrow_mut().push(pid);
                Err(())
            },
            |pid| {
                executable_queries.borrow_mut().push(pid);
                Err(())
            },
        );

        assert!(observation.identities.is_empty());
        assert_eq!(observation.coverage.status, UiCoverageStatus::Unavailable);
        assert!(record_queries.borrow().is_empty());
        assert!(executable_queries.borrow().is_empty());
        assert!(
            observation
                .coverage
                .message
                .as_deref()
                .is_some_and(|message| message.starts_with("A WebKitGTK process parent edge"))
        );
    }

    #[test]
    fn linux_ui_rejects_snapshot_and_procfs_identity_mismatch() {
        // Mutation caught: checking only PID accepts stale start ticks or a changed parent from
        // the current process record.
        for (label, current) in [
            ("changed parent", record(501, SERVER_PID + 1, 200)),
            ("changed start", record(501, SERVER_PID, 201)),
        ] {
            let rows: Arc<[ProcessRow]> = Arc::from([
                row(SERVER_PID, 1, 100, "/app/bin/coddy-studio-desktop"),
                row(501, SERVER_PID, 200, "/usr/lib/WebKitWebProcess"),
            ]);
            let executable_queries = RefCell::new(Vec::new());

            let observation = build_observation_with(
                &rows,
                server_identity(),
                |_| Ok(current),
                |pid| {
                    executable_queries.borrow_mut().push(pid);
                    Ok(PathBuf::from("/usr/lib/WebKitWebProcess"))
                },
            );

            assert!(observation.identities.is_empty(), "{label}");
            assert_eq!(
                observation.coverage.status,
                UiCoverageStatus::Unavailable,
                "{label}"
            );
            assert!(executable_queries.borrow().is_empty(), "{label}");
        }
    }

    #[test]
    fn linux_ui_rejects_pid_reuse_around_executable_resolution() {
        // Mutation caught: omitting the second process-record read accepts an executable after
        // the candidate PID changes identity during path resolution.
        let rows: Arc<[ProcessRow]> = Arc::from([
            row(SERVER_PID, 1, 100, "/app/bin/coddy-studio-desktop"),
            row(501, SERVER_PID, 200, "/usr/lib/WebKitWebProcess"),
        ]);
        let mut records = VecDeque::from([
            Ok(record(501, SERVER_PID, 200)),
            Ok(record(501, SERVER_PID, 201)),
        ]);

        let observation = build_observation_with(
            &rows,
            server_identity(),
            |_| records.pop_front().expect("bounded record query"),
            |_| Ok(PathBuf::from("/usr/lib/WebKitWebProcess")),
        );

        assert!(observation.identities.is_empty());
        assert_eq!(observation.coverage.status, UiCoverageStatus::Unavailable);
        assert!(records.is_empty());
    }

    #[test]
    fn linux_ui_rejects_executable_role_mismatch() {
        // Mutation caught: accepting any supported executable allows a Web command hint to
        // claim a GPU process after PID reuse or malformed snapshot data.
        let rows: Arc<[ProcessRow]> = Arc::from([
            row(SERVER_PID, 1, 100, "/app/bin/coddy-studio-desktop"),
            row(
                501,
                SERVER_PID,
                200,
                "/usr/lib/WebKitWebProcess --web-process",
            ),
        ]);
        let records = stable_records(&rows);

        let observation = build_observation_with(
            &rows,
            server_identity(),
            |pid| records.get(&pid).copied().ok_or(()),
            |_| Ok(PathBuf::from("/usr/lib/WebKitGPUProcess")),
        );

        assert!(observation.identities.is_empty());
        assert_eq!(observation.coverage.status, UiCoverageStatus::Unavailable);
        assert!(
            observation
                .coverage
                .message
                .as_deref()
                .is_some_and(|message| message.starts_with(
                    "A WebKitGTK process executable did not match its reported role."
                ))
        );
    }

    #[test]
    fn linux_ui_rejects_process_record_and_executable_read_failures() {
        // Mutation caught: treating any failed validation boundary as success leaks a stale PID;
        // retaining injected detail can disclose process or filesystem information.
        enum Failure {
            ProcessExit,
            MalformedStat,
            PermissionDenial,
        }

        for (label, failure) in [
            ("process-exit", Failure::ProcessExit),
            ("malformed-stat", Failure::MalformedStat),
            ("permission-denial", Failure::PermissionDenial),
        ] {
            let rows: Arc<[ProcessRow]> = Arc::from([
                row(SERVER_PID, 1, 100, "/app/bin/coddy-studio-desktop"),
                row(501, SERVER_PID, 200, "/usr/lib/WebKitWebProcess"),
            ]);
            let expected = record(501, SERVER_PID, 200);
            let mut records = match failure {
                Failure::ProcessExit => VecDeque::from([Err(())]),
                Failure::MalformedStat => VecDeque::from([Ok(expected), Err(())]),
                Failure::PermissionDenial => VecDeque::from([Ok(expected), Ok(expected)]),
            };

            let observation = build_observation_with(
                &rows,
                server_identity(),
                |_| records.pop_front().expect("bounded record query"),
                |_| match failure {
                    Failure::PermissionDenial => Err(()),
                    Failure::ProcessExit | Failure::MalformedStat => {
                        Ok(PathBuf::from("/usr/lib/WebKitWebProcess"))
                    }
                },
            );

            assert!(observation.identities.is_empty(), "{label}");
            assert_eq!(
                observation.coverage.status,
                UiCoverageStatus::Unavailable,
                "{label}"
            );
            let message = observation
                .coverage
                .message
                .expect("bounded failure message");
            assert!(message.chars().count() <= 160, "{label}: {message}");
            assert!(!message.contains(label), "{label}: {message}");
        }
    }

    #[test]
    fn linux_ui_keeps_valid_rows_when_a_peer_fails() {
        // Mutation caught: one candidate failure must not discard a separately validated helper
        // or incorrectly report complete coverage.
        let rows: Arc<[ProcessRow]> = Arc::from([
            row(SERVER_PID, 1, 100, "/app/bin/coddy-studio-desktop"),
            row(501, SERVER_PID, 200, "/usr/lib/WebKitWebProcess"),
            row(502, SERVER_PID, 201, "/usr/lib/WebKitNetworkProcess"),
        ]);
        let valid = record(501, SERVER_PID, 200);

        let observation = build_observation_with(
            &rows,
            server_identity(),
            |pid| (pid == 501).then_some(valid).ok_or(()),
            |pid| {
                (pid == 501)
                    .then(|| PathBuf::from("/usr/lib/WebKitWebProcess"))
                    .ok_or(())
            },
        );

        assert_eq!(
            observation.identities,
            vec![ProcessIdentity {
                pid: 501,
                started_at: 200,
            }]
        );
        assert_eq!(observation.coverage.status, UiCoverageStatus::Partial);
        assert_eq!(
            observation.coverage.message.as_deref(),
            Some("A WebKitGTK process identity could not be validated.")
        );
    }

    #[test]
    fn linux_ui_rejects_unknown_direct_webkit_roles() {
        // Mutation caught: prefix/suffix recognition without the strict role allowlist claims a
        // newly introduced WebKit role whose ownership contract has not been approved.
        let rows_with_peer: Arc<[ProcessRow]> = Arc::from([
            row(SERVER_PID, 1, 100, "/app/bin/coddy-studio-desktop"),
            row(501, SERVER_PID, 200, "/usr/lib/WebKitWebProcess"),
            row(504, SERVER_PID, 204, "/usr/lib/WebKitModelProcess"),
        ]);
        let records = stable_records(&rows_with_peer);
        let executables = executable_paths(&rows_with_peer);
        let queried = RefCell::new(Vec::new());
        let observation = build_observation_with(
            &rows_with_peer,
            server_identity(),
            |pid| {
                queried.borrow_mut().push(pid);
                records.get(&pid).copied().ok_or(())
            },
            |pid| executables.get(&pid).cloned().ok_or(()),
        );

        assert_eq!(
            observation.identities,
            vec![ProcessIdentity {
                pid: 501,
                started_at: 200,
            }]
        );
        assert_eq!(observation.coverage.status, UiCoverageStatus::Partial);
        assert_eq!(&*queried.borrow(), &[501, 501]);
        assert_eq!(
            observation.coverage.message.as_deref(),
            Some("An unsupported WebKitGTK process role was observed.")
        );

        let unknown_only: Arc<[ProcessRow]> = Arc::from([
            row(SERVER_PID, 1, 100, "/app/bin/coddy-studio-desktop"),
            row(504, SERVER_PID, 204, "/usr/lib/WebKitModelProcess"),
        ]);
        let observation = build_observation_with(
            &unknown_only,
            server_identity(),
            |_| panic!("unknown roles must not query process records"),
            |_| panic!("unknown roles must not resolve executables"),
        );

        assert!(observation.identities.is_empty());
        assert_eq!(observation.coverage.status, UiCoverageStatus::Unavailable);
        assert!(
            observation
                .coverage
                .message
                .as_deref()
                .is_some_and(|message| message
                    .starts_with("An unsupported WebKitGTK process role was observed."))
        );
    }

    #[test]
    fn linux_ui_no_helpers_is_unavailable() {
        // Mutation caught: treating an empty candidate set as complete produces a healthy-looking
        // zero instead of explicitly unavailable local UI coverage.
        let rows: Arc<[ProcessRow]> = Arc::from([
            row(SERVER_PID, 1, 100, "/app/bin/coddy-studio-desktop"),
            row(
                501,
                SERVER_PID,
                200,
                "/usr/bin/helper --child=/usr/lib/WebKitWebProcess",
            ),
        ]);

        let observation = build_observation_with(
            &rows,
            server_identity(),
            |_| panic!("ordinary children must not query process records"),
            |_| panic!("ordinary children must not resolve executables"),
        );

        assert!(observation.identities.is_empty());
        assert_eq!(observation.coverage.status, UiCoverageStatus::Unavailable);
        assert_eq!(
            observation.coverage.message.as_deref(),
            Some(
                "No supported WebKitGTK UI process identity was available. Native server usage \
is included, but local UI/WebView usage could not be associated reliably."
            )
        );
    }

    #[test]
    fn linux_ui_messages_are_bounded() {
        // Mutation caught: interpolating an unbounded PID, command, path, or operating-system
        // error can exceed the observer's direct 160-scalar message budget.
        for issue in [
            None,
            Some(LinuxObservationIssue::ServerSnapshot),
            Some(LinuxObservationIssue::ParentEdge),
            Some(LinuxObservationIssue::ProcessRecord),
            Some(LinuxObservationIssue::Executable),
            Some(LinuxObservationIssue::RoleMismatch),
            Some(LinuxObservationIssue::UnsupportedRole),
        ] {
            let issues = issue.into_iter().collect::<BTreeSet<_>>();
            for has_identities in [false, true] {
                let coverage = coverage_for(has_identities, &issues);
                if let Some(message) = coverage.message {
                    assert!(
                        message.chars().count() <= 160,
                        "message exceeded bound: {message}"
                    );
                }
            }
        }
    }
}
