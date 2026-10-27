use coddy_studio_server::diagnostics::TraceDiagnosticsStore;
use serde_json::json;

#[test]
fn measured_failures_keep_legacy_and_untimed_records_out_of_latency_statistics() {
    let directory = tempfile::tempdir().expect("temporary diagnostics directory");
    let path = directory.path().join("server.trace.ndjson");
    let legacy = |id: &str, duration: f64| {
        json!({
            "type": "native-span", "name": "same.operation", "traceId": id, "spanId": id,
            "startTimeUnixNano": "1000000000", "endTimeUnixNano": "1012000000",
            "durationMs": duration, "exit": {"_tag": "Failure", "cause": id},
        })
    };
    let mut unknown = legacy("legacy-unmeasured", 0.0);
    unknown["endTimeUnixNano"] = json!("1000000000");
    std::fs::write(
        &path,
        format!("{}\n{}\n", unknown, legacy("legacy-measured", 12.0)),
    )
    .expect("legacy records");
    let store = TraceDiagnosticsStore::new(path);
    store
        .record_failure("same.operation", &json!({"detail":"untimed"}))
        .unwrap();
    store
        .record_timed_failure(
            "same.operation",
            &json!({"detail":"measured-sub-ms"}),
            std::time::Duration::from_micros(500),
        )
        .unwrap();
    store
        .record_timed_failure(
            "same.operation",
            &json!({"detail":"measured-zero"}),
            std::time::Duration::ZERO,
        )
        .unwrap();
    store.record_event("untimed.event", json!({})).unwrap();

    let result = store.read();
    assert_eq!(result["recordCount"], 6);
    assert_eq!(result["failureCount"], 5);
    assert_eq!(result["slowestSpans"].as_array().unwrap().len(), 3);
    let summary = &result["topSpansByCount"][0];
    assert_eq!(summary["count"], 5);
    assert_eq!(summary["measuredCount"], 3);
    assert_eq!(summary["totalDurationMs"], 12.5);
    assert_eq!(summary["averageDurationMs"], 12.5 / 3.0);
    assert_eq!(summary["maxDurationMs"], 12.0);
    let event = &result["topSpansByCount"][1];
    assert_eq!(event["measuredCount"], 0);
    assert_eq!(event["averageDurationMs"], 0.0);
    for failure in result["latestFailures"].as_array().unwrap() {
        let cause = failure["cause"].as_str().unwrap();
        assert_eq!(
            failure["durationMeasured"],
            !matches!(cause, "legacy-unmeasured" | "untimed")
        );
    }
}

#[test]
fn explicit_unknown_timing_is_excluded_and_invalid_timing_is_rejected() {
    let directory = tempfile::tempdir().expect("temporary diagnostics directory");
    let path = directory.path().join("server.trace.ndjson");
    let record = json!({
        "type":"native-span", "name":"same.operation", "traceId":"fixture", "spanId":"fixture",
        "startTimeUnixNano":"1000000000", "endTimeUnixNano":"2000000000",
        "durationMs":1000.0, "durationMeasured":false,
        "exit":{"_tag":"Failure", "cause":"owned fixture"},
    });
    let mut rows = vec![record.clone()];
    for (key, value) in [
        ("durationMs", json!(-1)),
        ("durationMs", json!(null)),
        ("durationMeasured", json!("true")),
        ("durationMeasured", json!(null)),
    ] {
        let mut invalid = record.clone();
        invalid[key] = value;
        rows.push(invalid);
    }
    std::fs::write(
        &path,
        rows.into_iter()
            .map(|row| format!("{row}\n"))
            .collect::<String>(),
    )
    .unwrap();
    let result = TraceDiagnosticsStore::new(path).read();
    assert_eq!(result["recordCount"], 1);
    assert_eq!(result["failureCount"], 1);
    assert_eq!(result["parseErrorCount"], 4);
    assert_eq!(result["slowSpanCount"], 0);
    assert!(result["slowestSpans"].as_array().unwrap().is_empty());
    assert_eq!(result["topSpansByCount"][0]["measuredCount"], 0);
    assert_eq!(result["topSpansByCount"][0]["totalDurationMs"], 0.0);
    assert_eq!(result["topSpansByCount"][0]["averageDurationMs"], 0.0);
    assert_eq!(result["topSpansByCount"][0]["maxDurationMs"], 0.0);
    assert_eq!(result["latestFailures"][0]["durationMeasured"], false);
}

#[test]
fn actionable_failures_are_redacted_and_survive_restart() {
    let directory = tempfile::tempdir().expect("temporary diagnostics directory");
    let trace_path = directory.path().join("logs/server.trace.ndjson");

    {
        let store = TraceDiagnosticsStore::new(trace_path.clone());
        store
            .record_failure(
                "git.createWorktree",
                &json!({
                    "_tag": "GitCommandError",
                    "detail": "fatal: bad config line 3 in .gitmodules\nAuthorization: Bearer super-secret"
                }),
            )
            .expect("failure is persisted");
    }

    let restarted = TraceDiagnosticsStore::new(trace_path.clone());
    let diagnostics = restarted.read();

    assert_eq!(
        diagnostics["traceFilePath"],
        trace_path.to_string_lossy().as_ref()
    );
    assert_eq!(diagnostics["recordCount"], 1);
    assert_eq!(diagnostics["failureCount"], 1);
    assert_eq!(
        diagnostics["latestFailures"][0]["name"],
        "git.createWorktree"
    );
    let cause = diagnostics["latestFailures"][0]["cause"]
        .as_str()
        .expect("failure cause");
    assert!(cause.contains("bad config line 3 in .gitmodules"));
    assert!(!cause.contains("super-secret"));
    assert!(cause.contains("[REDACTED]"));
}

#[test]
fn structured_and_common_inline_credentials_are_redacted_before_persistence() {
    let directory = tempfile::tempdir().expect("temporary diagnostics directory");
    let trace_path = directory.path().join("logs/server.trace.ndjson");
    let store = TraceDiagnosticsStore::new(trace_path.clone());

    store
        .record_failure(
            "provider.probe",
            &json!({
                "context": {
                    "token": "nested-token-value",
                    "client_secret": "nested-client-secret",
                    "reason": "Bearer inline-bearer-value"
                }
            }),
        )
        .expect("failure is persisted");

    let persisted = std::fs::read_to_string(trace_path).expect("trace file is readable");
    assert!(!persisted.contains("nested-token-value"));
    assert!(!persisted.contains("nested-client-secret"));
    assert!(!persisted.contains("inline-bearer-value"));
    assert!(persisted.contains("[REDACTED]"));
}

#[test]
fn credentials_from_legacy_trace_files_are_redacted_when_read() {
    let directory = tempfile::tempdir().expect("temporary diagnostics directory");
    let trace_path = directory.path().join("logs/server.trace.ndjson");
    std::fs::create_dir_all(trace_path.parent().unwrap()).expect("trace directory");
    std::fs::write(
        &trace_path,
        serde_json::to_string(&json!({
            "type": "native-span",
            "name": "provider.legacyFailure",
            "traceId": "legacy-trace",
            "spanId": "legacy-span",
            "startTimeUnixNano": "1000000000",
            "endTimeUnixNano": "1000000000",
            "durationMs": 0.0,
            "events": [{
                "name": "Authorization: Bearer legacy-super-secret",
                "timeUnixNano": "1000000000",
                "attributes": { "effect.logLevel": "Error" }
            }],
            "exit": {
                "_tag": "Failure",
                "cause": "token=legacy-super-secret"
            }
        }))
        .unwrap()
            + "\n",
    )
    .expect("legacy trace fixture");

    let diagnostics = TraceDiagnosticsStore::new(trace_path).read();
    let encoded = diagnostics.to_string();
    assert!(!encoded.contains("legacy-super-secret"));
    assert!(encoded.contains("[REDACTED]"));
}

#[test]
fn malformed_persisted_lines_are_counted_without_hiding_valid_failures() {
    let directory = tempfile::tempdir().expect("temporary diagnostics directory");
    let trace_path = directory.path().join("server.trace.ndjson");
    std::fs::write(&trace_path, "not-json\n").expect("malformed fixture");
    let store = TraceDiagnosticsStore::new(trace_path);
    store
        .record_failure("vcs.createWorktree", &json!({ "message": "failed" }))
        .expect("valid failure");

    let diagnostics = store.read();
    assert_eq!(diagnostics["parseErrorCount"], 1);
    assert_eq!(diagnostics["recordCount"], 1);
    assert_eq!(diagnostics["failureCount"], 1);
}
