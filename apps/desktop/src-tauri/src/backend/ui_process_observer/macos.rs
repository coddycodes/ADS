use std::{
    collections::{BTreeSet, HashMap, HashSet},
    ffi::OsStr,
    fmt::{self, Debug, Formatter},
    future::Future,
    mem::size_of,
    os::raw::c_int,
    path::Path,
    pin::Pin,
    ptr::from_mut,
    sync::Arc,
    time::Duration,
};

use coddy_studio_server::diagnostics::{
    DesktopUiObservation, DesktopUiProcessObserver, ProcessIdentity, ProcessRow, UiCoverage,
    UiCoverageStatus,
};
use objc2::{msg_send, runtime::NSObjectProtocol, sel};
use objc2_web_kit::WKWebView;
use tauri::{AppHandle, Manager, Runtime};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum WebKitProcessRole {
    WebContent,
    Gpu,
    Networking,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct WebKitProcessCandidate {
    pid: u32,
    role: WebKitProcessRole,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CoalitionIds {
    resource: u64,
    jetsam: u64,
}

const PROC_PIDCOALITIONINFO: c_int = 20;
const WEBVIEW_PID_COLLECTION_TIMEOUT: Duration = Duration::from_millis(175);

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct ProcPidCoalitionInfo {
    coalition_id: [u64; 2],
    reserved: [u64; 3],
}

const _: () = assert!(size_of::<ProcPidCoalitionInfo>() == 40);

fn coalition_ids(pid: u32) -> Result<CoalitionIds, ()> {
    let mut info = ProcPidCoalitionInfo::default();
    let size = size_of::<ProcPidCoalitionInfo>();
    let size = c_int::try_from(size).map_err(|_| ())?;
    let pid = c_int::try_from(pid).map_err(|_| ())?;

    // SAFETY: the C-compatible buffer has the exact private coalition-info
    // layout and remains valid for the complete query.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            PROC_PIDCOALITIONINFO,
            0,
            from_mut(&mut info).cast(),
            size,
        )
    };
    if read != size {
        return Err(());
    }

    let ids = CoalitionIds {
        resource: info.coalition_id[0],
        jetsam: info.coalition_id[1],
    };
    (ids.resource != 0 && ids.jetsam != 0)
        .then_some(ids)
        .ok_or(())
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum MacosObservationIssue {
    WebviewDispatch,
    WebviewDeadline,
    PrivateSelector,
    ServerSnapshot,
    ServerCoalition,
    CandidateSnapshot,
    CandidateExecutable,
    CandidateCoalition,
}

#[derive(Debug, Default)]
struct WebKitPidCollection {
    candidates: Vec<WebKitProcessCandidate>,
    issues: BTreeSet<MacosObservationIssue>,
}

#[derive(Debug, Default)]
struct WebViewPidResult {
    candidates: Vec<WebKitProcessCandidate>,
    missing_selectors: usize,
}

fn reduce_webview_results(
    expected_callbacks: usize,
    dispatch_failures: usize,
    timed_out: bool,
    results: Vec<WebViewPidResult>,
) -> WebKitPidCollection {
    let completed_callbacks = results.len();
    let mut collection = WebKitPidCollection::default();

    if dispatch_failures > 0 {
        collection
            .issues
            .insert(MacosObservationIssue::WebviewDispatch);
    }
    if timed_out || completed_callbacks < expected_callbacks {
        collection
            .issues
            .insert(MacosObservationIssue::WebviewDeadline);
    }

    for result in results {
        if result.missing_selectors > 0 {
            collection
                .issues
                .insert(MacosObservationIssue::PrivateSelector);
        }
        collection.candidates.extend(
            result
                .candidates
                .into_iter()
                .filter(|candidate| candidate.pid > 0),
        );
    }

    collection
}

fn push_candidate(result: &mut WebViewPidResult, pid: libc::pid_t, role: WebKitProcessRole) {
    if let Ok(pid) = u32::try_from(pid)
        && pid > 0
    {
        result.candidates.push(WebKitProcessCandidate { pid, role });
    }
}

fn read_webview_pids(wk: &WKWebView) -> WebViewPidResult {
    let mut result = WebViewPidResult::default();

    if wk.respondsToSelector(sel!(_webProcessIdentifier)) {
        // SAFETY: the runtime capability check proves the private pid_t getter is
        // implemented by this WKWebView instance.
        let pid: libc::pid_t = unsafe { msg_send![wk, _webProcessIdentifier] };
        push_candidate(&mut result, pid, WebKitProcessRole::WebContent);
    } else {
        result.missing_selectors += 1;
    }

    if wk.respondsToSelector(sel!(_provisionalWebProcessIdentifier)) {
        // SAFETY: the runtime capability check proves the private pid_t getter is
        // implemented by this WKWebView instance.
        let pid: libc::pid_t = unsafe { msg_send![wk, _provisionalWebProcessIdentifier] };
        push_candidate(&mut result, pid, WebKitProcessRole::WebContent);
    } else {
        result.missing_selectors += 1;
    }

    if wk.respondsToSelector(sel!(_gpuProcessIdentifier)) {
        // SAFETY: the runtime capability check proves the private pid_t getter is
        // implemented by this WKWebView instance.
        let pid: libc::pid_t = unsafe { msg_send![wk, _gpuProcessIdentifier] };
        push_candidate(&mut result, pid, WebKitProcessRole::Gpu);
    } else {
        result.missing_selectors += 1;
    }

    // SAFETY: generated public getters retain their returned Objective-C
    // objects for the duration of these local values.
    let data_store = unsafe { wk.configuration().websiteDataStore() };
    if data_store.respondsToSelector(sel!(_networkProcessIdentifier)) {
        // SAFETY: the runtime capability check proves the private pid_t getter is
        // implemented by this WKWebsiteDataStore instance.
        let pid: libc::pid_t = unsafe { msg_send![&data_store, _networkProcessIdentifier] };
        push_candidate(&mut result, pid, WebKitProcessRole::Networking);
    } else {
        result.missing_selectors += 1;
    }

    result
}

async fn collect_webview_pids<R: Runtime>(app: &AppHandle<R>) -> WebKitPidCollection {
    let webviews = app.webviews();
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut expected_callbacks = 0;
    let mut dispatch_failures = 0;

    for webview in webviews.into_values() {
        let sender = sender.clone();
        let dispatch = webview.with_webview(move |platform| {
            // SAFETY: on macOS Tauri provides its live WKWebView and executes this
            // closure on the main/UI thread.
            let wk: &WKWebView = unsafe { &*platform.inner().cast() };
            let _ = sender.send(read_webview_pids(wk));
        });
        if dispatch.is_ok() {
            expected_callbacks += 1;
        } else {
            dispatch_failures += 1;
        }
    }
    drop(sender);

    let deadline = tokio::time::Instant::now() + WEBVIEW_PID_COLLECTION_TIMEOUT;
    let mut results = Vec::with_capacity(expected_callbacks);
    let mut timed_out = false;
    while results.len() < expected_callbacks {
        match tokio::time::timeout_at(deadline, receiver.recv()).await {
            Ok(Some(result)) => results.push(result),
            Ok(None) => break,
            Err(_) => {
                timed_out = true;
                break;
            }
        }
    }

    reduce_webview_results(expected_callbacks, dispatch_failures, timed_out, results)
}

pub(super) struct MacosDesktopUiProcessObserver<R: Runtime> {
    app: AppHandle<R>,
}

impl<R: Runtime> MacosDesktopUiProcessObserver<R> {
    pub(super) fn new(app: AppHandle<R>) -> Self {
        Self { app }
    }
}

impl<R: Runtime> Debug for MacosDesktopUiProcessObserver<R> {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MacosDesktopUiProcessObserver")
            .finish_non_exhaustive()
    }
}

impl<R: Runtime> DesktopUiProcessObserver for MacosDesktopUiProcessObserver<R> {
    fn observe(
        &self,
        rows: Arc<[ProcessRow]>,
        server_identity: ProcessIdentity,
    ) -> Pin<Box<dyn Future<Output = DesktopUiObservation> + Send + '_>> {
        Box::pin(async move {
            let collection = collect_webview_pids(&self.app).await;
            build_observation_with(&rows, server_identity, collection, coalition_ids)
        })
    }
}

const UI_UNAVAILABLE_MESSAGE: &str =
    "Native server usage is included, but local UI/WebView usage could not be associated reliably.";

fn build_observation_with(
    rows: &[ProcessRow],
    server_identity: ProcessIdentity,
    collection: WebKitPidCollection,
    mut coalition_for: impl FnMut(u32) -> Result<CoalitionIds, ()>,
) -> DesktopUiObservation {
    let WebKitPidCollection {
        candidates,
        mut issues,
    } = collection;
    let Some(server) = rows
        .iter()
        .find(|row| row.pid == server_identity.pid && row.started_at == server_identity.started_at)
    else {
        issues.insert(MacosObservationIssue::ServerSnapshot);
        return DesktopUiObservation {
            identities: Vec::new(),
            coverage: coverage_for(false, &issues),
        };
    };
    let Ok(server_coalition) = coalition_for(server.pid) else {
        issues.insert(MacosObservationIssue::ServerCoalition);
        return DesktopUiObservation {
            identities: Vec::new(),
            coverage: coverage_for(false, &issues),
        };
    };
    if server_coalition.resource == 0 || server_coalition.jetsam == 0 {
        issues.insert(MacosObservationIssue::ServerCoalition);
        return DesktopUiObservation {
            identities: Vec::new(),
            coverage: coverage_for(false, &issues),
        };
    }

    let rows_by_pid = rows
        .iter()
        .map(|row| (row.pid, row))
        .collect::<HashMap<_, _>>();
    let mut seen_candidates = HashSet::new();
    let mut coalition_cache = HashMap::<u32, Result<CoalitionIds, ()>>::new();
    coalition_cache.insert(server.pid, Ok(server_coalition));
    let mut accepted = HashSet::new();

    for candidate in candidates {
        if candidate.pid == 0 || !seen_candidates.insert(candidate) {
            continue;
        }
        let Some(row) = rows_by_pid.get(&candidate.pid) else {
            issues.insert(MacosObservationIssue::CandidateSnapshot);
            continue;
        };
        if !command_matches_role(&row.command, candidate.role) {
            issues.insert(MacosObservationIssue::CandidateExecutable);
            continue;
        }

        let candidate_coalition = if let Some(cached) = coalition_cache.get(&candidate.pid) {
            *cached
        } else {
            let queried = coalition_for(candidate.pid);
            coalition_cache.insert(candidate.pid, queried);
            queried
        };
        let Ok(candidate_coalition) = candidate_coalition else {
            issues.insert(MacosObservationIssue::CandidateCoalition);
            continue;
        };
        if candidate_coalition.resource != server_coalition.resource
            || candidate_coalition.jetsam != server_coalition.jetsam
        {
            issues.insert(MacosObservationIssue::CandidateCoalition);
            continue;
        }

        accepted.insert(ProcessIdentity {
            pid: row.pid,
            started_at: row.started_at,
        });
    }

    let mut identities = accepted.into_iter().collect::<Vec<_>>();
    identities.sort_unstable_by_key(|identity| identity.pid);

    let coverage = coverage_for(!identities.is_empty(), &issues);
    DesktopUiObservation {
        identities,
        coverage,
    }
}

fn coverage_for(has_identities: bool, issues: &BTreeSet<MacosObservationIssue>) -> UiCoverage {
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

fn bounded_issue_message(
    status: UiCoverageStatus,
    issues: &BTreeSet<MacosObservationIssue>,
) -> String {
    let boundary = match issues.iter().next() {
        Some(MacosObservationIssue::WebviewDispatch) => {
            "WebView process discovery could not run on the main thread."
        }
        Some(MacosObservationIssue::WebviewDeadline) => {
            "WebView process discovery exceeded its deadline."
        }
        Some(MacosObservationIssue::PrivateSelector) => {
            "WebView process identifiers were unavailable from WebKit."
        }
        Some(MacosObservationIssue::ServerSnapshot) => {
            "The native server process was absent from the process snapshot."
        }
        Some(MacosObservationIssue::ServerCoalition) => {
            "The native server coalition could not be validated."
        }
        Some(MacosObservationIssue::CandidateSnapshot) => {
            "A WebKit process was absent from the process snapshot."
        }
        Some(MacosObservationIssue::CandidateExecutable) => {
            "A WebKit process executable did not match its reported role."
        }
        Some(MacosObservationIssue::CandidateCoalition) => {
            "A WebKit process coalition could not be validated."
        }
        None => "No WebKit UI process identity was available.",
    };

    if status == UiCoverageStatus::Unavailable {
        format!("{boundary} {UI_UNAVAILABLE_MESSAGE}")
    } else {
        boundary.to_string()
    }
}

fn command_matches_role(command: &str, role: WebKitProcessRole) -> bool {
    let Some(executable) = command.split_ascii_whitespace().next() else {
        return false;
    };
    let Some(name) = Path::new(executable).file_name().and_then(OsStr::to_str) else {
        return false;
    };

    match role {
        WebKitProcessRole::WebContent => matches!(
            name,
            "com.apple.WebKit.WebContent" | "com.apple.WebKit.WebContent.EnhancedSecurity"
        ),
        WebKitProcessRole::Gpu => name == "com.apple.WebKit.GPU",
        WebKitProcessRole::Networking => name == "com.apple.WebKit.Networking",
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, collections::HashMap};

    use coddy_studio_server::diagnostics::{ProcessIdentity, ProcessRow, UiCoverageStatus};

    use super::{
        CoalitionIds, MacosObservationIssue, WebKitPidCollection, WebKitProcessCandidate,
        WebKitProcessRole, WebViewPidResult, build_observation_with, coalition_ids,
        reduce_webview_results,
    };

    const SERVER_PID: u32 = 410;
    const SERVER_STARTED_AT: u64 = 1_000;
    const HOST_COALITION: CoalitionIds = CoalitionIds {
        resource: 71,
        jetsam: 72,
    };

    fn row(pid: u32, started_at: u64, command: &str) -> ProcessRow {
        let mut row = ProcessRow::fixture(pid, SERVER_PID, command);
        row.started_at = started_at;
        row
    }

    fn server_row() -> ProcessRow {
        row(
            SERVER_PID,
            SERVER_STARTED_AT,
            "/Applications/Coddy Studio.app/Contents/MacOS/Coddy Studio",
        )
    }

    fn web_content_row(pid: u32, started_at: u64) -> ProcessRow {
        row(
            pid,
            started_at,
            "/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/\
com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent --type=webcontent",
        )
    }

    fn enhanced_security_row(pid: u32, started_at: u64) -> ProcessRow {
        row(
            pid,
            started_at,
            "/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/\
com.apple.WebKit.WebContent.EnhancedSecurity.xpc/Contents/MacOS/\
com.apple.WebKit.WebContent.EnhancedSecurity --type=webcontent",
        )
    }

    fn gpu_row(pid: u32, started_at: u64) -> ProcessRow {
        row(
            pid,
            started_at,
            "/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/\
com.apple.WebKit.GPU.xpc/Contents/MacOS/com.apple.WebKit.GPU --type=gpu",
        )
    }

    fn networking_row(pid: u32, started_at: u64) -> ProcessRow {
        row(
            pid,
            started_at,
            "/System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/\
com.apple.WebKit.Networking.xpc/Contents/MacOS/com.apple.WebKit.Networking --type=networking",
        )
    }

    fn unrelated_webkit_row(pid: u32, started_at: u64) -> ProcessRow {
        web_content_row(pid, started_at)
    }

    fn ordinary_row(pid: u32, started_at: u64) -> ProcessRow {
        row(
            pid,
            started_at,
            "/usr/bin/helper --child=/System/Library/Frameworks/WebKit.framework/Versions/A/\
XPCServices/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent",
        )
    }

    fn server_identity() -> ProcessIdentity {
        ProcessIdentity {
            pid: SERVER_PID,
            started_at: SERVER_STARTED_AT,
        }
    }

    fn collection(candidates: &[(u32, WebKitProcessRole)]) -> WebKitPidCollection {
        WebKitPidCollection {
            candidates: candidates
                .iter()
                .map(|&(pid, role)| WebKitProcessCandidate { pid, role })
                .collect(),
            ..WebKitPidCollection::default()
        }
    }

    fn observe(
        rows: &[ProcessRow],
        collection: WebKitPidCollection,
        coalitions: HashMap<u32, Result<CoalitionIds, ()>>,
    ) -> coddy_studio_server::diagnostics::DesktopUiObservation {
        build_observation_with(rows, server_identity(), collection, |pid| {
            coalitions.get(&pid).copied().unwrap_or(Err(()))
        })
    }

    #[test]
    fn macos_ui_collection_retains_candidates_from_every_webview() {
        // Mutation caught: replacing rather than extending the candidate vector loses the
        // first WebView's WebContent process when a later WebView reports its GPU process.
        let collection = reduce_webview_results(
            2,
            0,
            false,
            vec![
                WebViewPidResult {
                    candidates: vec![WebKitProcessCandidate {
                        pid: 501,
                        role: WebKitProcessRole::WebContent,
                    }],
                    missing_selectors: 0,
                },
                WebViewPidResult {
                    candidates: vec![WebKitProcessCandidate {
                        pid: 503,
                        role: WebKitProcessRole::Gpu,
                    }],
                    missing_selectors: 0,
                },
            ],
        );

        assert_eq!(
            collection.candidates,
            vec![
                WebKitProcessCandidate {
                    pid: 501,
                    role: WebKitProcessRole::WebContent,
                },
                WebKitProcessCandidate {
                    pid: 503,
                    role: WebKitProcessRole::Gpu,
                },
            ]
        );
        assert!(collection.issues.is_empty());
    }

    #[test]
    fn macos_ui_collection_records_dispatch_failures() {
        // Mutation caught: ignoring an immediate with_webview failure incorrectly reports
        // complete collection coverage from the callbacks that did dispatch.
        let collection = reduce_webview_results(1, 1, false, vec![WebViewPidResult::default()]);

        assert_eq!(
            collection.issues,
            [MacosObservationIssue::WebviewDispatch]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn macos_ui_collection_records_an_incomplete_timed_out_fanout() {
        // Mutation caught: checking only the timeout flag or only callback count misses a
        // scheduled callback that did not return before the deadline.
        let collection = reduce_webview_results(2, 0, true, vec![WebViewPidResult::default()]);

        assert_eq!(
            collection.issues,
            [MacosObservationIssue::WebviewDeadline]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn macos_ui_collection_early_channel_close_preserves_a_candidate_as_partial() {
        // Mutation caught: requiring a timeout before recording incomplete callback coverage
        // lets an early closed channel report Available from one completed WebView.
        let rows = vec![server_row(), web_content_row(501, 2_001)];
        let coalitions = [(SERVER_PID, Ok(HOST_COALITION)), (501, Ok(HOST_COALITION))]
            .into_iter()
            .collect();
        let collection = reduce_webview_results(
            2,
            0,
            false,
            vec![WebViewPidResult {
                candidates: vec![WebKitProcessCandidate {
                    pid: 501,
                    role: WebKitProcessRole::WebContent,
                }],
                missing_selectors: 0,
            }],
        );

        assert_eq!(
            collection.issues,
            [MacosObservationIssue::WebviewDeadline]
                .into_iter()
                .collect()
        );
        let observation = observe(&rows, collection, coalitions);
        assert_eq!(
            observation.identities,
            vec![ProcessIdentity {
                pid: 501,
                started_at: 2_001,
            }]
        );
        assert_eq!(observation.coverage.status, UiCoverageStatus::Partial);
        assert_eq!(
            observation.coverage.message.as_deref(),
            Some("WebView process discovery exceeded its deadline.")
        );
    }

    #[test]
    fn macos_ui_collection_records_any_missing_private_selector() {
        // Mutation caught: treating selector capability loss as healthy because another
        // selector produced a candidate suppresses the WebKit capability boundary.
        let collection = reduce_webview_results(
            1,
            0,
            false,
            vec![WebViewPidResult {
                candidates: vec![WebKitProcessCandidate {
                    pid: 501,
                    role: WebKitProcessRole::WebContent,
                }],
                missing_selectors: 1,
            }],
        );

        assert_eq!(
            collection.issues,
            [MacosObservationIssue::PrivateSelector]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn macos_ui_collection_without_callbacks_or_candidates_is_unavailable() {
        // Mutation caught: deriving availability from an issue-free collection rather than
        // from validated identities reports healthy coverage when no WebView exists.
        let rows = vec![server_row()];
        let coalitions = [(SERVER_PID, Ok(HOST_COALITION))].into_iter().collect();
        let collection = reduce_webview_results(0, 0, false, Vec::new());

        let observation = observe(&rows, collection, coalitions);

        assert!(observation.identities.is_empty());
        assert_eq!(observation.coverage.status, UiCoverageStatus::Unavailable);
    }

    #[test]
    fn macos_ui_collection_drops_zero_pids_before_validation() {
        // Mutation caught: forwarding WebKit's no-process sentinel into validation retains
        // a candidate that was never a live process identifier.
        let collection = reduce_webview_results(
            1,
            0,
            false,
            vec![WebViewPidResult {
                candidates: vec![
                    WebKitProcessCandidate {
                        pid: 0,
                        role: WebKitProcessRole::WebContent,
                    },
                    WebKitProcessCandidate {
                        pid: 503,
                        role: WebKitProcessRole::Gpu,
                    },
                ],
                missing_selectors: 0,
            }],
        );

        assert_eq!(
            collection.candidates,
            vec![WebKitProcessCandidate {
                pid: 503,
                role: WebKitProcessRole::Gpu,
            }]
        );
    }

    #[test]
    fn macos_ui_accepts_all_role_matched_candidates_in_the_host_coalition() {
        // Mutation caught: dropping any supported role/executable mapping, or using
        // a candidate PID instead of its snapshot start identity, loses an expected identity.
        let rows = vec![
            server_row(),
            web_content_row(501, 2_001),
            web_content_row(502, 2_002),
            gpu_row(503, 2_003),
            networking_row(504, 2_004),
            enhanced_security_row(505, 2_005),
        ];
        let collection = collection(&[
            (501, WebKitProcessRole::WebContent),
            (502, WebKitProcessRole::WebContent),
            (503, WebKitProcessRole::Gpu),
            (504, WebKitProcessRole::Networking),
            (505, WebKitProcessRole::WebContent),
        ]);
        let coalitions = [
            (SERVER_PID, Ok(HOST_COALITION)),
            (501, Ok(HOST_COALITION)),
            (502, Ok(HOST_COALITION)),
            (503, Ok(HOST_COALITION)),
            (504, Ok(HOST_COALITION)),
            (505, Ok(HOST_COALITION)),
        ]
        .into_iter()
        .collect();

        let observation = observe(&rows, collection, coalitions);

        assert_eq!(
            observation.identities,
            vec![
                ProcessIdentity {
                    pid: 501,
                    started_at: 2_001,
                },
                ProcessIdentity {
                    pid: 502,
                    started_at: 2_002,
                },
                ProcessIdentity {
                    pid: 503,
                    started_at: 2_003,
                },
                ProcessIdentity {
                    pid: 504,
                    started_at: 2_004,
                },
                ProcessIdentity {
                    pid: 505,
                    started_at: 2_005,
                },
            ]
        );
        assert_eq!(observation.coverage.status, UiCoverageStatus::Available);
        assert_eq!(observation.coverage.message, None);
    }

    #[test]
    fn macos_ui_deduplicates_candidates_from_multiple_webviews_and_ignores_zero() {
        // Mutations caught: removing typed-candidate deduplication, PID-level coalition
        // caching, or the zero-PID guard adds a native lookup beyond server + PID 501.
        let rows = vec![server_row(), web_content_row(501, 2_001)];
        let collection = collection(&[
            (501, WebKitProcessRole::WebContent),
            (501, WebKitProcessRole::WebContent),
            (0, WebKitProcessRole::WebContent),
            (0, WebKitProcessRole::Gpu),
        ]);
        let queried = RefCell::new(Vec::new());

        let observation = build_observation_with(&rows, server_identity(), collection, |pid| {
            queried.borrow_mut().push(pid);
            match pid {
                SERVER_PID | 501 => Ok(HOST_COALITION),
                _ => Err(()),
            }
        });

        assert_eq!(
            observation.identities,
            vec![ProcessIdentity {
                pid: 501,
                started_at: 2_001,
            }]
        );
        assert_eq!(observation.coverage.status, UiCoverageStatus::Available);
        assert_eq!(*queried.borrow(), vec![SERVER_PID, 501]);
    }

    #[test]
    fn macos_ui_marks_missing_snapshot_candidate_partial_when_another_is_valid() {
        // Mutation caught: silently dropping a nonzero candidate absent from the immutable
        // snapshot incorrectly reports complete coverage.
        let rows = vec![server_row(), web_content_row(501, 2_001)];
        let collection = collection(&[
            (501, WebKitProcessRole::WebContent),
            (599, WebKitProcessRole::WebContent),
        ]);
        let coalitions = [(SERVER_PID, Ok(HOST_COALITION)), (501, Ok(HOST_COALITION))]
            .into_iter()
            .collect();

        let observation = observe(&rows, collection, coalitions);

        assert_eq!(
            observation.identities,
            vec![ProcessIdentity {
                pid: 501,
                started_at: 2_001,
            }]
        );
        assert_eq!(observation.coverage.status, UiCoverageStatus::Partial);
        assert_eq!(
            observation.coverage.message.as_deref(),
            Some("A WebKit process was absent from the process snapshot.")
        );
    }

    #[test]
    fn macos_ui_rejects_a_different_coalition() {
        // Mutation caught: accepting an executable-role match without comparing both host
        // coalition fields claims an unrelated WebKit process.
        let rows = vec![server_row(), unrelated_webkit_row(601, 3_001)];
        let collection = collection(&[(601, WebKitProcessRole::WebContent)]);
        let coalitions = [
            (SERVER_PID, Ok(HOST_COALITION)),
            (
                601,
                Ok(CoalitionIds {
                    resource: 71,
                    jetsam: 99,
                }),
            ),
        ]
        .into_iter()
        .collect();

        let observation = observe(&rows, collection, coalitions);

        assert_eq!(observation.identities, Vec::<ProcessIdentity>::new());
        assert_eq!(observation.coverage.status, UiCoverageStatus::Unavailable);
        assert_eq!(
            observation.coverage.message.as_deref(),
            Some(
                "A WebKit process coalition could not be validated. Native server usage is \
included, but local UI/WebView usage could not be associated reliably."
            )
        );
    }

    #[test]
    fn macos_ui_rejects_a_non_webkit_process_in_the_host_coalition() {
        // Mutation caught: substring matching or inspecting later arguments accepts an
        // ordinary helper merely because a WebKit executable path appears in its arguments.
        let rows = vec![server_row(), ordinary_row(602, 3_002)];
        let collection = collection(&[(602, WebKitProcessRole::WebContent)]);
        let coalitions = [(SERVER_PID, Ok(HOST_COALITION)), (602, Ok(HOST_COALITION))]
            .into_iter()
            .collect();

        let observation = observe(&rows, collection, coalitions);

        assert_eq!(observation.identities, Vec::<ProcessIdentity>::new());
        assert_eq!(observation.coverage.status, UiCoverageStatus::Unavailable);
        assert_eq!(
            observation.coverage.message.as_deref(),
            Some(
                "A WebKit process executable did not match its reported role. Native server \
usage is included, but local UI/WebView usage could not be associated reliably."
            )
        );
    }

    #[test]
    fn macos_ui_coalition_failure_is_partial_with_a_valid_peer() {
        // Mutation caught: aborting the whole observation on one candidate coalition query
        // failure removes an already validated peer identity.
        let rows = vec![
            server_row(),
            web_content_row(501, 2_001),
            gpu_row(503, 2_003),
        ];
        let collection = collection(&[
            (501, WebKitProcessRole::WebContent),
            (503, WebKitProcessRole::Gpu),
        ]);
        let coalitions = [
            (SERVER_PID, Ok(HOST_COALITION)),
            (501, Ok(HOST_COALITION)),
            (503, Err(())),
        ]
        .into_iter()
        .collect();

        let observation = observe(&rows, collection, coalitions);

        assert_eq!(
            observation.identities,
            vec![ProcessIdentity {
                pid: 501,
                started_at: 2_001,
            }]
        );
        assert_eq!(observation.coverage.status, UiCoverageStatus::Partial);
        assert_eq!(
            observation.coverage.message.as_deref(),
            Some("A WebKit process coalition could not be validated.")
        );
    }

    #[test]
    fn macos_ui_coalition_failure_is_unavailable_without_a_valid_identity() {
        // Mutation caught: deriving status from issue presence instead of accepted identity
        // count reports Partial despite there being no attributable WebKit process.
        let rows = vec![server_row(), gpu_row(503, 2_003)];
        let collection = collection(&[(503, WebKitProcessRole::Gpu)]);
        let coalitions = [(SERVER_PID, Ok(HOST_COALITION)), (503, Err(()))]
            .into_iter()
            .collect();

        let observation = observe(&rows, collection, coalitions);

        assert_eq!(observation.identities, Vec::<ProcessIdentity>::new());
        assert_eq!(observation.coverage.status, UiCoverageStatus::Unavailable);
        assert_eq!(
            observation.coverage.message.as_deref(),
            Some(
                "A WebKit process coalition could not be validated. Native server usage is \
included, but local UI/WebView usage could not be associated reliably."
            )
        );
    }

    fn assert_collection_issue_preserves_validated_row(
        issue: MacosObservationIssue,
        expected_message: &str,
    ) {
        let rows = vec![server_row(), web_content_row(501, 2_001)];
        let mut collection = collection(&[(501, WebKitProcessRole::WebContent)]);
        collection.issues.insert(issue);
        let coalitions = [(SERVER_PID, Ok(HOST_COALITION)), (501, Ok(HOST_COALITION))]
            .into_iter()
            .collect();

        let observation = observe(&rows, collection, coalitions);

        assert_eq!(
            observation.identities,
            vec![ProcessIdentity {
                pid: 501,
                started_at: 2_001,
            }]
        );
        assert_eq!(observation.coverage.status, UiCoverageStatus::Partial);
        assert_eq!(
            observation.coverage.message.as_deref(),
            Some(expected_message)
        );
    }

    #[test]
    fn macos_ui_collection_dispatch_failure_preserves_validated_rows_as_partial() {
        // Mutation caught: deleting dispatch issue preservation or mapping it to another
        // collection boundary loses the exact dispatch message while retaining a valid peer.
        assert_collection_issue_preserves_validated_row(
            MacosObservationIssue::WebviewDispatch,
            "WebView process discovery could not run on the main thread.",
        );
    }

    #[test]
    fn macos_ui_collection_deadline_failure_preserves_validated_rows_as_partial() {
        // Mutation caught: deleting deadline issue preservation or letting dispatch shadow
        // deadline handling loses the exact deadline message while retaining a valid peer.
        assert_collection_issue_preserves_validated_row(
            MacosObservationIssue::WebviewDeadline,
            "WebView process discovery exceeded its deadline.",
        );
    }

    #[test]
    fn macos_ui_collection_private_selector_failure_preserves_validated_rows_as_partial() {
        // Mutation caught: deleting private-selector issue preservation or mapping it to a
        // generic collection failure loses its exact message while retaining a valid peer.
        assert_collection_issue_preserves_validated_row(
            MacosObservationIssue::PrivateSelector,
            "WebView process identifiers were unavailable from WebKit.",
        );
    }

    #[test]
    fn macos_ui_requires_the_exact_server_start_identity() {
        // Mutation caught: finding the server by PID alone allows a reused process ID to
        // authorize candidates from a different server generation.
        let mut reused_server = server_row();
        reused_server.started_at = SERVER_STARTED_AT + 1;
        let rows = vec![reused_server, web_content_row(501, 2_001)];
        let collection = collection(&[(501, WebKitProcessRole::WebContent)]);
        let queried = RefCell::new(Vec::new());

        let observation = build_observation_with(&rows, server_identity(), collection, |pid| {
            queried.borrow_mut().push(pid);
            Ok(HOST_COALITION)
        });

        assert_eq!(observation.identities, Vec::<ProcessIdentity>::new());
        assert_eq!(observation.coverage.status, UiCoverageStatus::Unavailable);
        assert_eq!(*queried.borrow(), Vec::<u32>::new());
        assert_eq!(
            observation.coverage.message.as_deref(),
            Some(
                "The native server process was absent from the process snapshot. Native server \
usage is included, but local UI/WebView usage could not be associated reliably."
            )
        );
    }

    #[test]
    fn macos_ui_requires_nonzero_resource_and_jetsam_coalitions() {
        // Mutations caught: validating only one host coalition field accepts an ambiguous
        // zero-valued resource or jetsam identity.
        let rows = vec![server_row(), web_content_row(501, 2_001)];
        let collection_for = || collection(&[(501, WebKitProcessRole::WebContent)]);

        for invalid_host in [
            CoalitionIds {
                resource: 0,
                jetsam: 72,
            },
            CoalitionIds {
                resource: 71,
                jetsam: 0,
            },
        ] {
            let coalitions = [(SERVER_PID, Ok(invalid_host)), (501, Ok(invalid_host))]
                .into_iter()
                .collect();

            let observation = observe(&rows, collection_for(), coalitions);

            assert_eq!(observation.identities, Vec::<ProcessIdentity>::new());
            assert_eq!(observation.coverage.status, UiCoverageStatus::Unavailable);
            assert_eq!(
                observation.coverage.message.as_deref(),
                Some(
                    "The native server coalition could not be validated. Native server usage is \
included, but local UI/WebView usage could not be associated reliably."
                )
            );
        }
    }

    #[test]
    fn macos_ui_rejects_role_swaps() {
        // Mutation caught: accepting any WebKit executable irrespective of the selector's
        // typed role lets a GPU candidate claim a WebContent process.
        let rows = vec![server_row(), web_content_row(501, 2_001)];
        let collection = collection(&[(501, WebKitProcessRole::Gpu)]);
        let coalitions = [(SERVER_PID, Ok(HOST_COALITION)), (501, Ok(HOST_COALITION))]
            .into_iter()
            .collect();

        let observation = observe(&rows, collection, coalitions);

        assert_eq!(observation.identities, Vec::<ProcessIdentity>::new());
        assert_eq!(observation.coverage.status, UiCoverageStatus::Unavailable);
        assert_eq!(
            observation.coverage.message.as_deref(),
            Some(
                "A WebKit process executable did not match its reported role. Native server \
usage is included, but local UI/WebView usage could not be associated reliably."
            )
        );
    }

    #[test]
    fn macos_ui_rejects_pid_values_outside_the_native_query_range() {
        // Mutation caught: unchecked u32-to-c_int conversion can wrap an invalid candidate
        // process identifier before entering the native coalition API.
        assert_eq!(coalition_ids(u32::MAX), Err(()));
    }

    #[test]
    fn macos_ui_uses_a_generic_bounded_message_when_no_candidate_is_running() {
        // Mutation caught: indexing the empty issue set panics, while reusing candidate
        // details can disclose a PID, command, path, or operating-system error.
        let rows = vec![server_row()];
        let coalitions = [(SERVER_PID, Ok(HOST_COALITION))].into_iter().collect();

        let observation = observe(&rows, WebKitPidCollection::default(), coalitions);

        assert_eq!(observation.identities, Vec::<ProcessIdentity>::new());
        assert_eq!(observation.coverage.status, UiCoverageStatus::Unavailable);
        assert_eq!(
            observation.coverage.message.as_deref(),
            Some(
                "No WebKit UI process identity was available. Native server usage is included, \
but local UI/WebView usage could not be associated reliably."
            )
        );
    }
}
