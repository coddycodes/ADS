//! Git stacked actions (`git.runStackedAction`: commit, push, pull request): every terminal
//! outcome as a `git.stacked-action` event, and a `git.stacked-action-failed` finding when the
//! same step keeps failing in one worktree (#158). The stream reports a failure as an
//! `action_failed` item and then ends successfully, so the RPC latency book never sees it.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Mutex, PoisonError},
    time::{Duration, Instant},
};

use serde_json::json;

use super::{
    cite, emit,
    findings::{FindingInput, Severity},
    is_enabled, record_finding,
    redact::{redact_message, redact_path},
};

/// Failures of one step in one worktree within this window count as repeats.
const REPEAT_WINDOW: Duration = Duration::from_secs(30 * 60);
/// The failure in the window that raises the finding; each later one adds an occurrence.
const REPEAT_THRESHOLD: usize = 3;
/// Worktree and step pairs remembered at once; the least recently failed one is forgotten.
const MAX_TRACKED: usize = 128;
/// Failure instants kept per pair; the count it reports stops growing there.
const MAX_TIMES_PER_KEY: usize = 64;

/// Why a stacked action failed, from the `_tag` of its wire error. Only the step itself
/// failing (`git-failed`, `provider-failed`, `internal`) can raise the finding: a request
/// BiBCode refused, a workspace that went away, or a client that cancelled is the protocol
/// working as designed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureReason {
    GitFailed,
    ProviderFailed,
    Rejected,
    WorkspaceUnavailable,
    Cancelled,
    Internal,
}

impl FailureReason {
    /// The reason of a wire error `_tag`.
    pub fn from_tag(tag: Option<&str>) -> Self {
        match tag {
            Some("GitCommandError") => Self::GitFailed,
            Some("SourceControlProviderError") => Self::ProviderFailed,
            Some("RpcRequestInvalid") => Self::Rejected,
            Some("WorkspaceUnavailableError") => Self::WorkspaceUnavailable,
            _ => Self::Internal,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::GitFailed => "git-failed",
            Self::ProviderFailed => "provider-failed",
            Self::Rejected => "rejected",
            Self::WorkspaceUnavailable => "workspace-unavailable",
            Self::Cancelled => "cancelled",
            Self::Internal => "internal",
        }
    }

    fn raises_finding(self) -> bool {
        matches!(
            self,
            Self::GitFailed | Self::ProviderFailed | Self::Internal
        )
    }
}

/// What failed. `operation` is the failing Git or provider operation's static tag, such as
/// `GitVcsDriver.pushCurrentBranch`; `message` is the safe failure description (the exit
/// status, never Git's output, which can quote a commit message or hook output).
pub struct StackedActionFailure<'a> {
    pub phase: Option<&'static str>,
    pub reason: FailureReason,
    pub operation: Option<&'a str>,
    pub message: &'a str,
}

/// Records one `git.runStackedAction` outcome for the worktree `cwd`. Does nothing while the
/// profiler is off.
pub fn record_stacked_action_outcome(
    action: &str,
    feature_branch: bool,
    cwd: &str,
    outcome: Result<(), StackedActionFailure<'_>>,
    duration: Duration,
) {
    if !is_enabled() {
        return;
    }
    let duration_ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
    let failure = outcome.err();
    let phase = failure.as_ref().and_then(|failure| failure.phase);
    let reason = failure.as_ref().map(|failure| failure.reason.as_str());
    let operation = failure
        .as_ref()
        .and_then(|failure| failure.operation)
        .map(redact_message);
    let event_id = emit(
        "git.stacked-action",
        json!({
            // An unsupported action is the client's text until validation refuses it.
            "action": redact_message(action),
            "featureBranch": feature_branch,
            "ok": failure.is_none(),
            "phase": phase,
            "reason": reason,
            "operation": operation,
            "durationMs": duration_ms,
        }),
    );
    let Some(failure) = failure else { return };
    if !failure.reason.raises_finding() {
        return;
    }
    let phase = phase.unwrap_or("preflight");
    let operation = operation.unwrap_or_default();
    let key = [cwd, action, phase, failure.reason.as_str(), &operation].join("\0");
    let Some(repeats) = REPEATS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .note_failure(key, Instant::now())
    else {
        return;
    };
    record_finding(FindingInput {
        kind: "git.stacked-action-failed",
        severity: Severity::Warn,
        source: "server".into(),
        fingerprint_fields: vec![
            action.to_owned(),
            phase.to_owned(),
            failure.reason.as_str().to_owned(),
            operation.clone(),
        ],
        title: "A Git action keeps failing".into(),
        summary: format!(
            "{action} failed at {phase} ({}) {repeats} times in one worktree within {} minutes.",
            failure.reason.as_str(),
            REPEAT_WINDOW.as_secs() / 60,
        ),
        evidence: cite(
            json!({
                "action": action,
                "featureBranch": feature_branch,
                "phase": phase,
                "reason": failure.reason.as_str(),
                "operation": operation,
                "message": redact_message(failure.message),
                "worktree": redact_path(cwd),
                "repeatsInWindow": repeats,
                "windowMinutes": REPEAT_WINDOW.as_secs() / 60,
                "durationMs": duration_ms,
            }),
            event_id,
        ),
    });
}

static REPEATS: Mutex<RepeatBook> = Mutex::new(RepeatBook { failures: None });

/// Recent failure instants per worktree and step, in memory only: the key holds the path.
struct RepeatBook {
    failures: Option<HashMap<String, VecDeque<Instant>>>,
}

impl RepeatBook {
    /// Notes a failure at `now` and returns how many failures of `key` fall in the window,
    /// once that reaches the threshold.
    fn note_failure(&mut self, key: String, now: Instant) -> Option<usize> {
        let failures = self.failures.get_or_insert_with(HashMap::new);
        let within = |at: &Instant| now.saturating_duration_since(*at) < REPEAT_WINDOW;
        failures.retain(|_, times| times.back().is_some_and(within));
        if !failures.contains_key(&key) && failures.len() >= MAX_TRACKED {
            let oldest = failures
                .iter()
                .min_by_key(|(_, times)| times.back().copied())
                .map(|(key, _)| key.clone());
            if let Some(oldest) = oldest {
                failures.remove(&oldest);
            }
        }
        let times = failures.entry(key).or_default();
        while times.front().is_some_and(|at| !within(at)) {
            times.pop_front();
        }
        times.push_back(now);
        // A hot repeat cannot grow the window without bound.
        while times.len() > MAX_TIMES_PER_KEY {
            times.pop_front();
        }
        (times.len() >= REPEAT_THRESHOLD).then_some(times.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn book() -> RepeatBook {
        RepeatBook { failures: None }
    }

    #[test]
    fn the_third_failure_in_the_window_raises_and_later_ones_repeat() {
        let mut book = book();
        let start = Instant::now();
        let at = |minutes: u64| start + Duration::from_secs(minutes * 60);
        assert_eq!(book.note_failure("a".into(), at(0)), None);
        assert_eq!(book.note_failure("a".into(), at(5)), None);
        assert_eq!(book.note_failure("a".into(), at(10)), Some(3));
        assert_eq!(book.note_failure("a".into(), at(15)), Some(4));
        // Another worktree or step counts on its own.
        assert_eq!(book.note_failure("b".into(), at(15)), None);
    }

    #[test]
    fn failures_older_than_the_window_do_not_count() {
        let mut book = book();
        let start = Instant::now();
        let at = |minutes: u64| start + Duration::from_secs(minutes * 60);
        book.note_failure("a".into(), at(0));
        book.note_failure("a".into(), at(10));
        // The first failure left the 30-minute window.
        assert_eq!(book.note_failure("a".into(), at(31)), None);
        assert_eq!(book.note_failure("a".into(), at(32)), Some(3));
    }

    #[test]
    fn the_book_forgets_the_least_recent_pair_when_full() {
        let mut book = book();
        let start = Instant::now();
        for index in 0..MAX_TRACKED {
            book.note_failure(
                format!("k{index}"),
                start + Duration::from_secs(index as u64),
            );
        }
        book.note_failure("new".into(), start + Duration::from_secs(1_000));
        let failures = book.failures.as_ref().unwrap();
        assert_eq!(failures.len(), MAX_TRACKED);
        assert!(!failures.contains_key("k0"));
        assert!(failures.contains_key("new"));
    }

    #[test]
    fn only_a_failing_step_can_raise_the_finding() {
        assert_eq!(
            FailureReason::from_tag(Some("GitCommandError")),
            FailureReason::GitFailed
        );
        assert_eq!(
            FailureReason::from_tag(Some("RpcRequestInvalid")),
            FailureReason::Rejected
        );
        assert_eq!(FailureReason::from_tag(None), FailureReason::Internal);
        assert!(FailureReason::GitFailed.raises_finding());
        assert!(FailureReason::ProviderFailed.raises_finding());
        assert!(!FailureReason::Rejected.raises_finding());
        assert!(!FailureReason::WorkspaceUnavailable.raises_finding());
        assert!(!FailureReason::Cancelled.raises_finding());
    }
}
