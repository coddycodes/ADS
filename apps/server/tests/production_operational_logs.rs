use std::path::PathBuf;

use coddy_studio_server::production::{
    operational_logs::{OperationalLogOptions, ProviderOperationalLog},
    provider_runtime::ProviderEvent,
};
use coddy_studio_server::{
    production::operational_logs::TerminalOperationalLog, terminal::TerminalEvent,
};
use serde_json::json;
use tempfile::TempDir;

#[tokio::test]
async fn activity_provider_events_without_a_lifecycle_suffix_are_marked_observed() {
    let temp = TempDir::new().expect("temporary log directory");
    let path = temp.path().join("events.log");
    let log = ProviderOperationalLog::start(path.clone(), OperationalLogOptions::default())
        .await
        .expect("provider log starts");

    assert!(log.record(&ProviderEvent {
        native_event_id: None,
        event_type: "assistant.message.delta".to_owned(),
        thread_id: "thread-1".to_owned(),
        turn_id: Some("turn-1".to_owned()),
        item_id: None,
        request_id: None,
        payload: json!({
            "text": "Bearer PRIVATE_BEARER_TOKEN",
            "arguments": { "apiKey": "PRIVATE_API_KEY", "cookie": "PRIVATE_COOKIE" },
            "environment": { "SECRET": "PRIVATE_ENVIRONMENT" },
            "raw": "https://hooks.example/activity?token=PRIVATE_HOOK_TOKEN"
        }),
        activity: Vec::new(),
        activity_controls: Default::default(),
    }));
    log.shutdown().await.expect("provider log shuts down");

    let contents = std::fs::read_to_string(path).expect("read provider log");
    let record: serde_json::Value = serde_json::from_str(contents.trim()).expect("provider record");
    assert_eq!(record["status"], "observed");
    for private_value in [
        "PRIVATE_BEARER_TOKEN",
        "PRIVATE_API_KEY",
        "PRIVATE_COOKIE",
        "PRIVATE_ENVIRONMENT",
        "PRIVATE_HOOK_TOKEN",
    ] {
        assert!(!contents.contains(private_value));
    }
}

#[tokio::test]
async fn provider_logs_reject_malformed_item_ids() {
    let temp = TempDir::new().expect("temporary log directory");
    let path = temp.path().join("events.log");
    let log = ProviderOperationalLog::start(path.clone(), OperationalLogOptions::default())
        .await
        .expect("provider log starts");

    for item_id in [
        format!("SENSITIVE_OVERSIZED_ITEM_ID{}", "x".repeat(5_000)),
        "SENSITIVE_CONTROL_ITEM_ID\u{0007}".to_owned(),
        " SENSITIVE_LEADING_SPACE_ITEM_ID".to_owned(),
        "SENSITIVE_TRAILING_SPACE_ITEM_ID ".to_owned(),
        "SENSITIVE_TRAILING_NEWLINE_ITEM_ID\n".to_owned(),
    ] {
        assert!(log.record(&ProviderEvent {
            native_event_id: None,
            event_type: "assistant.message.delta".to_owned(),
            thread_id: "thread-1".to_owned(),
            turn_id: Some("turn-1".to_owned()),
            item_id: Some(item_id),
            request_id: None,
            payload: json!({}),
            activity: Vec::new(),
            activity_controls: Default::default(),
        }));
    }
    log.shutdown().await.expect("provider log shuts down");

    let contents = std::fs::read_to_string(path).expect("read provider log");
    assert!(!contents.contains("SENSITIVE_OVERSIZED_ITEM_ID"));
    assert!(!contents.contains("SENSITIVE_CONTROL_ITEM_ID"));
    assert!(!contents.contains("SENSITIVE_LEADING_SPACE_ITEM_ID"));
    assert!(!contents.contains("SENSITIVE_TRAILING_SPACE_ITEM_ID"));
    assert!(!contents.contains("SENSITIVE_TRAILING_NEWLINE_ITEM_ID"));
    for line in contents.lines() {
        let record: serde_json::Value = serde_json::from_str(line).expect("provider record");
        assert!(record.get("itemId").is_none());
    }
}

#[tokio::test]
async fn terminal_output_is_persisted_as_metadata_without_data() {
    let temp = TempDir::new().expect("temporary log directory");
    let path = temp.path().join("terminal-events.log");
    let log = TerminalOperationalLog::start(path.clone(), OperationalLogOptions::default())
        .await
        .expect("terminal log starts");

    assert!(log.record(&TerminalEvent::Output {
        thread_id: "thread-1".to_owned(),
        terminal_id: "terminal-1".to_owned(),
        sequence: 7,
        data: "PRIVATE_TERMINAL_OUTPUT".to_owned(),
    }));
    log.shutdown().await.expect("terminal log shuts down");

    let contents = std::fs::read_to_string(path).expect("read terminal log");
    let record: serde_json::Value = serde_json::from_str(contents.trim()).expect("terminal record");
    assert_eq!(record["eventType"], "activity");
    assert_eq!(record["activityType"], "output");
    assert_eq!(record["byteCount"], 23);
    assert!(record.get("data").is_none());
    assert!(!contents.contains("PRIVATE_TERMINAL_OUTPUT"));
}

#[tokio::test]
async fn startup_removes_provider_log_files_that_exceed_the_configured_bound() {
    let temp = TempDir::new().expect("temporary log directory");
    let path = temp.path().join("events.log");
    std::fs::write(&path, vec![b'x'; 256]).expect("oversized existing log");
    let log = ProviderOperationalLog::start(
        path.clone(),
        OperationalLogOptions {
            max_file_bytes: 100,
            retained_files: 2,
            queue_capacity: 2,
        },
    )
    .await
    .expect("provider log starts");

    assert!(log.record(&ProviderEvent {
        native_event_id: None,
        event_type: "turn.completed".to_owned(),
        thread_id: "thread-1".to_owned(),
        turn_id: None,
        item_id: None,
        request_id: None,
        payload: json!({ "text": "must not be persisted" }),
        activity: Vec::new(),
        activity_controls: Default::default(),
    }));
    log.shutdown().await.expect("provider log shuts down");

    assert!(std::fs::metadata(&path).expect("active log").len() <= 100);
    let rotated = PathBuf::from(format!("{}.1", path.display()));
    assert!(!rotated.exists() || std::fs::metadata(rotated).expect("rotated log").len() <= 100);
}
