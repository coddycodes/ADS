//! Provider turn failures: a turn the provider ended as failed, recorded by provider, the
//! provider's own name for why it ended, and the error class BiBCode gave it. Repeats of the
//! same failure accumulate in one finding, so a thread that keeps hitting the same wall (such
//! as a full context window) is visible without reading the conversation.

use serde_json::json;

use super::{
    cite, emit,
    findings::{FindingInput, Severity},
    is_enabled, record_finding,
};

/// The longest provider-reported reason kept. Longer or non-identifier reasons are recorded as
/// `other`: the reason is part of the fingerprint and must stay a stable, content-free name.
const MAX_REASON_CHARS: usize = 64;

/// Records one failed provider turn as a `provider.turn-failed` event and finding. Never
/// records message text: only `provider`, the provider's `terminal_reason` when it gave one,
/// and the canonical `error_class`. Does nothing while the profiler is off.
pub fn record_turn_failure(
    provider: &'static str,
    terminal_reason: Option<&str>,
    error_class: &str,
) {
    if !is_enabled() {
        return;
    }
    let finding = turn_failure_finding(provider, terminal_reason, error_class);
    let event_id = emit("provider.turn-failed", finding.evidence.clone());
    record_finding(FindingInput {
        evidence: cite(finding.evidence, event_id),
        ..finding
    });
}

fn turn_failure_finding(
    provider: &'static str,
    terminal_reason: Option<&str>,
    error_class: &str,
) -> FindingInput {
    let reason = terminal_reason.map(identifier);
    let error_class = identifier(error_class);
    let severity = if error_class == "context_limit" {
        Severity::Warn
    } else {
        Severity::Info
    };
    let summary = match &reason {
        Some(reason) => format!("A {provider} turn failed with {error_class} ({reason})."),
        None => format!("A {provider} turn failed with {error_class}."),
    };
    FindingInput {
        kind: "provider.turn-failed",
        severity,
        source: "server".into(),
        fingerprint_fields: vec![
            provider.to_owned(),
            reason.clone().unwrap_or_default(),
            error_class.clone(),
        ],
        title: "Provider turn failed".into(),
        summary,
        evidence: json!({
            "provider": provider,
            "terminalReason": reason,
            "errorClass": error_class,
        }),
    }
}

/// `value` when it is a short snake_case or kebab-case name, else `other`.
fn identifier(value: &str) -> String {
    let value = value.trim();
    let is_identifier = !value.is_empty()
        && value.len() <= MAX_REASON_CHARS
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        });
    if is_identifier {
        value.to_owned()
    } else {
        "other".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_context_limit_stops_share_one_warning_finding() {
        let first = turn_failure_finding("claude", Some("blocking_limit"), "context_limit");
        let second = turn_failure_finding("claude", Some("blocking_limit"), "context_limit");
        assert_eq!(first.kind, "provider.turn-failed");
        assert_eq!(first.severity, Severity::Warn);
        assert_eq!(first.fingerprint_fields, second.fingerprint_fields);
        assert_eq!(
            first.evidence,
            json!({
                "provider": "claude",
                "terminalReason": "blocking_limit",
                "errorClass": "context_limit"
            })
        );
        let other = turn_failure_finding("claude", Some("max_turns"), "provider_error");
        assert_eq!(other.severity, Severity::Info);
        assert_ne!(other.fingerprint_fields, first.fingerprint_fields);
    }

    #[test]
    fn a_reason_that_is_not_a_plain_name_is_never_recorded() {
        let finding = turn_failure_finding(
            "claude",
            Some("Prompt is too long: /Users/someone/secret"),
            "context_limit",
        );
        assert_eq!(finding.evidence["terminalReason"], "other");
        assert!(!finding.summary.contains("secret"));
        assert_eq!(
            turn_failure_finding("claude", None, "provider_error").evidence["terminalReason"],
            serde_json::Value::Null
        );
    }
}
