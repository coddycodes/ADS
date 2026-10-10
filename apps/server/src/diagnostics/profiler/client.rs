//! Client responsiveness reports (Decisions 7 and 8).
//!
//! `server.reportProfilerClientBatch` crosses a trust boundary. The whole batch is bounded and
//! validated before anything is derived from it, and an invalid one is refused with a typed
//! error while nothing is written. Slow requests are joined to the server's own timing by
//! correlation ID only: a record without a join entry has an unknown slow part, never a
//! guessed one, because matching by method and time can blame the wrong request.

use std::io;

use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    cite, emit_from,
    findings::{FindingInput, Severity},
    is_enabled,
    latency::{
        BUCKET_GROWTH, BUCKETS, JoinEntry, Tier, global_latency, is_background, is_long_running,
        judge,
    },
    record_finding,
    store::EventId,
};
use crate::rpc::ACTIVE_RPC_METHODS;

const MAX_PAYLOAD_BYTES: usize = 64 * 1024;
const MAX_RECORDS: usize = 64;
const MAX_HISTOGRAMS: usize = 64;
const MAX_METHOD_BYTES: usize = 128;
const MAX_ID_BYTES: usize = 64;
const MAX_CAPABILITIES: usize = 32;
const MAX_CAPABILITY_BYTES: usize = 64;
/// A longer sample spans a sleep; the collector drops those itself.
const MAX_SAMPLE_MS: f64 = 3_600_000.0;
const MAX_WINDOW_MS: f64 = 600_000.0;
const MAX_COUNT: u32 = 1_000_000;
/// `performance.now()` of a page open for 30 years.
const MAX_TIMESTAMP_MS: f64 = 1e12;
const MAX_HEAP_BYTES: f64 = (1u64 << 40) as f64;
const STALL_FINDING_MS: f64 = 1_000.0;
const INTERACTION_FINDING_MS: f64 = 500.0;
/// The source of client findings. Events name their client: `client:<clientId>`.
const SOURCE: &str = "client";

/// Handles one batch: `{ "accepted": true }` once it is recorded, `{ "accepted": false }`
/// while the profiler is off, and a `ProfilerClientBatchTooLarge` or
/// `ProfilerClientBatchInvalid` error, with nothing written, for a batch over its limits or
/// with an invalid field. Validation comes first, so a bad batch is refused whatever the toggle.
pub fn ingest_client_batch(batch: Value, env_id: &str) -> Result<Value, Value> {
    let batch = validate(&batch)?;
    if !is_enabled() {
        return Ok(json!({ "accepted": false }));
    }
    let book = global_latency();
    let analysis = analyze(&batch, env_id, |id| book.find_join(id));
    let source = format!("{SOURCE}:{}", batch.client_id);
    let event_ids: Vec<Option<EventId>> = analysis
        .events
        .into_iter()
        .map(|(kind, payload)| emit_from(source.clone(), kind, payload))
        .collect();
    for (finding, event) in analysis.findings {
        let event_id = event_ids.get(event).copied().flatten();
        record_finding(FindingInput {
            evidence: cite(finding.evidence, event_id),
            ..finding
        });
    }
    Ok(json!({ "accepted": true }))
}

/// `ProfilerClientBatch` from `packages/contracts/src/profiler.ts`. Unknown fields are
/// ignored, so a newer client still reports to an older server.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClientBatch {
    client_id: String,
    capabilities: Vec<String>,
    window_ms: f64,
    records: Vec<ClientRecord>,
    histograms: Vec<ClientHistogram>,
    stalls: Stalls,
    interactions: Interactions,
    js_heap_bytes: Option<f64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClientRecord {
    correlation_id: Option<String>,
    method: String,
    rtt_ms: f64,
    render_ms: Option<f64>,
    started_at_ms: f64,
}

#[derive(Deserialize)]
struct ClientHistogram {
    method: String,
    /// `(bucket, count)` pairs; see `latency::bucket_of`.
    buckets: Vec<(u32, u32)>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Stalls {
    count: u32,
    total_ms: f64,
    max_ms: f64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Interactions {
    count: u32,
    max_ms: f64,
}

fn validate(batch: &Value) -> Result<ClientBatch, Value> {
    if serde_json::to_writer(ByteBudget(MAX_PAYLOAD_BYTES), batch).is_err() {
        return Err(too_large("The batch is over 64 KiB."));
    }
    let length = |field: &str| {
        batch
            .get(field)
            .and_then(Value::as_array)
            .map_or(0, Vec::len)
    };
    if length("records") > MAX_RECORDS || length("histograms") > MAX_HISTOGRAMS {
        return Err(too_large(
            "The batch has more than 64 records or 64 histograms.",
        ));
    }
    let batch = ClientBatch::deserialize(batch).map_err(|error| invalid(&error.to_string()))?;
    batch.check().map_err(invalid)?;
    Ok(batch)
}

/// Counts serialized bytes and fails at the cap, so an oversized payload is never copied.
struct ByteBudget(usize);

impl io::Write for ByteBudget {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_sub(bytes.len())
            .ok_or_else(|| io::Error::other("over the byte budget"))?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn too_large(message: &str) -> Value {
    json!({ "_tag": "ProfilerClientBatchTooLarge", "message": message })
}

fn invalid(message: &str) -> Value {
    json!({ "_tag": "ProfilerClientBatchInvalid", "message": message })
}

/// Whether `value` is a number in `0..=max`; NaN and infinities are not.
fn bounded(value: f64, max: f64) -> bool {
    (0.0..=max).contains(&value)
}

fn ensure(ok: bool, message: &'static str) -> Result<(), &'static str> {
    if ok { Ok(()) } else { Err(message) }
}

impl ClientBatch {
    /// What the contract's schema leaves open: string lengths, and number ranges.
    fn check(&self) -> Result<(), &'static str> {
        ensure(
            self.client_id.len() <= MAX_ID_BYTES,
            "clientId is over 64 bytes.",
        )?;
        ensure(
            self.capabilities.len() <= MAX_CAPABILITIES
                && self
                    .capabilities
                    .iter()
                    .all(|capability| capability.len() <= MAX_CAPABILITY_BYTES),
            "capabilities holds over 32 entries or an entry over 64 bytes.",
        )?;
        ensure(
            bounded(self.window_ms, MAX_WINDOW_MS),
            "windowMs is outside 0 to 10 minutes.",
        )?;
        for record in &self.records {
            ensure(
                record.method.len() <= MAX_METHOD_BYTES,
                "A record's method is over 128 bytes.",
            )?;
            ensure(
                record
                    .correlation_id
                    .as_ref()
                    .is_none_or(|id| id.len() <= MAX_ID_BYTES),
                "A record's correlationId is over 64 bytes.",
            )?;
            ensure(
                bounded(record.rtt_ms, MAX_SAMPLE_MS)
                    && record.render_ms.is_none_or(|ms| bounded(ms, MAX_SAMPLE_MS))
                    && bounded(record.started_at_ms, MAX_TIMESTAMP_MS),
                "A record's duration is outside 0 to 1 hour, or its start is invalid.",
            )?;
        }
        for histogram in &self.histograms {
            ensure(
                histogram.method.len() <= MAX_METHOD_BYTES,
                "A histogram's method is over 128 bytes.",
            )?;
            ensure(
                histogram.buckets.len() <= BUCKETS
                    && histogram
                        .buckets
                        .iter()
                        .all(|&(bucket, count)| (bucket as usize) < BUCKETS && count <= MAX_COUNT),
                "A histogram bucket is out of range.",
            )?;
        }
        ensure(
            self.stalls.count <= MAX_COUNT
                && bounded(self.stalls.total_ms, MAX_SAMPLE_MS)
                && bounded(self.stalls.max_ms, MAX_SAMPLE_MS)
                && self.interactions.count <= MAX_COUNT
                && bounded(self.interactions.max_ms, MAX_SAMPLE_MS),
            "stalls or interactions are out of range.",
        )?;
        ensure(
            self.js_heap_bytes
                .is_none_or(|bytes| bounded(bytes, MAX_HEAP_BYTES)),
            "jsHeapBytes is out of range.",
        )
    }
}

struct Analysis {
    events: Vec<(&'static str, Value)>,
    /// Each finding with the index in `events` of the event that explains it.
    findings: Vec<(FindingInput, usize)>,
}

fn is_known(method: &str) -> bool {
    ACTIVE_RPC_METHODS.iter().any(|spec| spec.name == method)
}

/// The events and findings for a validated batch. Records and histograms of methods the
/// server does not serve are dropped and counted. A request record becomes an `rpc.slow`
/// finding over the absolute tiers, unless its method waits on external work.
fn analyze(
    batch: &ClientBatch,
    env_id: &str,
    join: impl Fn(&str) -> Option<JoinEntry>,
) -> Analysis {
    let mut events = Vec::new();
    let mut findings = Vec::new();
    let mut dropped = 0usize;
    for record in &batch.records {
        if !is_known(&record.method) {
            dropped += 1;
            continue;
        }
        // An ID joined to another method was reused, so it is not this request.
        let joined = record
            .correlation_id
            .as_deref()
            .and_then(&join)
            .filter(|entry| entry.method == record.method);
        let request = RequestTiming::of(record, joined.as_ref());
        let payload = request.payload(batch, record, env_id);
        let tier = judge(request.total_ms, None, 0, !is_background(&record.method));
        if tier >= Tier::Warn && !is_long_running(&record.method) {
            let finding = FindingInput {
                kind: "rpc.slow",
                severity: if tier == Tier::Severe {
                    Severity::Error
                } else {
                    Severity::Warn
                },
                source: SOURCE.to_owned(),
                fingerprint_fields: vec![record.method.clone(), request.slow_part.to_owned()],
                title: format!("{} took {:.1} s", record.method, request.total_ms / 1000.0),
                summary: request.summary(record),
                evidence: payload.clone(),
            };
            findings.push((finding, events.len()));
        }
        events.push(("client.request", payload));
    }
    let mut histograms = Vec::new();
    for histogram in &batch.histograms {
        if is_known(&histogram.method) {
            histograms.push(json!({ "method": histogram.method, "buckets": histogram.buckets }));
        } else {
            dropped += 1;
        }
    }
    let stalls = json!({
        "count": batch.stalls.count,
        "totalMs": batch.stalls.total_ms,
        "maxMs": batch.stalls.max_ms,
    });
    let interactions = json!({
        "count": batch.interactions.count,
        "maxMs": batch.interactions.max_ms,
    });
    events.push((
        "client.batch",
        json!({
            "envId": env_id,
            "clientId": batch.client_id,
            "capabilities": batch.capabilities,
            "windowMs": batch.window_ms,
            "records": batch.records.len(),
            "droppedUnknownMethods": dropped,
            "bucketGrowth": BUCKET_GROWTH,
            "histograms": histograms,
            "stalls": stalls,
            "interactions": interactions,
            "jsHeapBytes": batch.js_heap_bytes,
        }),
    ));
    // Client-wide findings cite the batch event, which carries their figures.
    let batch_event = events.len() - 1;
    let window_secs = batch.window_ms / 1000.0;
    if batch.stalls.total_ms > STALL_FINDING_MS {
        let finding = FindingInput {
            kind: "client.stall",
            severity: Severity::Warn,
            source: SOURCE.to_owned(),
            fingerprint_fields: vec![SOURCE.to_owned()],
            title: format!(
                "The interface stalled for {:.1} s",
                batch.stalls.total_ms / 1000.0
            ),
            summary: format!(
                "{count} main-thread stalls added up to {total:.0} ms within {window_secs:.0} s; \
                 the longest lasted {max:.0} ms.",
                count = batch.stalls.count,
                total = batch.stalls.total_ms,
                max = batch.stalls.max_ms,
            ),
            evidence: json!({
                "envId": env_id,
                "clientId": batch.client_id,
                "capabilities": batch.capabilities,
                "windowMs": batch.window_ms,
                "stalls": stalls,
            }),
        };
        findings.push((finding, batch_event));
    }
    if batch.interactions.max_ms > INTERACTION_FINDING_MS {
        let finding = FindingInput {
            kind: "client.interaction",
            severity: Severity::Warn,
            source: SOURCE.to_owned(),
            fingerprint_fields: vec![SOURCE.to_owned()],
            title: format!(
                "An interaction took {:.0} ms to paint",
                batch.interactions.max_ms
            ),
            summary: format!(
                "The slowest of {count} slow interactions within {window_secs:.0} s took \
                 {max:.0} ms from input to the next paint.",
                count = batch.interactions.count,
                max = batch.interactions.max_ms,
            ),
            evidence: json!({
                "envId": env_id,
                "clientId": batch.client_id,
                "capabilities": batch.capabilities,
                "windowMs": batch.window_ms,
                "interactions": interactions,
            }),
        };
        findings.push((finding, batch_event));
    }
    Analysis { events, findings }
}

/// Where one request's time went, as the client saw it until the response was painted.
struct RequestTiming {
    server_ms: Option<f64>,
    /// The round trip minus the server's handler time. The join log has no queue time
    /// (`JoinEntry::queued_ms` is always 0, meaning unknown), so queueing counts here.
    network_ms: Option<f64>,
    total_ms: f64,
    slow_part: &'static str,
}

impl RequestTiming {
    fn of(record: &ClientRecord, joined: Option<&JoinEntry>) -> Self {
        let server_ms = joined.map(|entry| entry.server_ms);
        let network_ms = server_ms.map(|server| (record.rtt_ms - server).max(0.0));
        let slow_part = match (server_ms, network_ms) {
            (Some(server), Some(network)) => slowest([
                ("server", server),
                ("network", network),
                ("render", record.render_ms.unwrap_or(0.0)),
            ]),
            _ => "unknown",
        };
        Self {
            server_ms,
            network_ms,
            total_ms: record.rtt_ms + record.render_ms.unwrap_or(0.0),
            slow_part,
        }
    }

    fn payload(&self, batch: &ClientBatch, record: &ClientRecord, env_id: &str) -> Value {
        json!({
            "envId": env_id,
            "clientId": batch.client_id,
            "method": record.method,
            "correlationId": record.correlation_id,
            "joinedBy": if self.server_ms.is_some() { "correlationId" } else { "none" },
            "rttMs": record.rtt_ms,
            "renderMs": record.render_ms,
            "serverMs": self.server_ms,
            "networkMs": self.network_ms,
            "queuedMs": null,
            "totalMs": self.total_ms,
            "slowPart": self.slow_part,
        })
    }

    fn summary(&self, record: &ClientRecord) -> String {
        let render = record
            .render_ms
            .map_or_else(|| "not measured".to_owned(), |ms| format!("{ms:.0} ms"));
        let total = self.total_ms;
        match (self.server_ms, self.network_ms) {
            (Some(server), Some(network)) => format!(
                "The client waited {total:.0} ms: server {server:.0} ms · network {network:.0} ms \
                 · render {render}. Network includes any time queued on the server."
            ),
            _ => format!(
                "The client waited {total:.0} ms: round trip {rtt:.0} ms · render {render}. The \
                 server's timing for this request was not found, so the slow part is unknown.",
                rtt = record.rtt_ms,
            ),
        }
    }
}

/// The name of the largest part; the first one wins a tie.
fn slowest(parts: [(&'static str, f64); 3]) -> &'static str {
    parts
        .into_iter()
        .fold(
            parts[0],
            |slowest, part| {
                if part.1 > slowest.1 { part } else { slowest }
            },
        )
        .0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::profiler::latency::LatencyBook;

    const SLOW: &str = "orchestration.dispatchCommand";

    fn record(correlation_id: Option<&str>, method: &str, rtt_ms: f64, render_ms: f64) -> Value {
        json!({
            "correlationId": correlation_id,
            "method": method,
            "rttMs": rtt_ms,
            "renderMs": render_ms,
            "startedAtMs": 1_000.0,
        })
    }

    fn batch(records: Vec<Value>) -> Value {
        json!({
            "clientId": "c1",
            "capabilities": ["heartbeat"],
            "windowMs": 10_000,
            "records": records,
            "histograms": [{ "method": "server.getConfig", "buckets": [[3, 7]] }],
            "stalls": { "count": 0, "totalMs": 0, "maxMs": 0 },
            "interactions": { "count": 0, "maxMs": 0 },
            "jsHeapBytes": null,
        })
    }

    fn analyze_with(book: &LatencyBook, batch: &Value) -> Analysis {
        let batch = validate(batch).expect("a valid batch");
        analyze(&batch, "env-1", |id| book.find_join(id))
    }

    fn tag(error: &Value) -> &str {
        error["_tag"].as_str().unwrap_or_default()
    }

    fn events<'a>(analysis: &'a Analysis, kind: &str) -> Vec<&'a Value> {
        analysis
            .events
            .iter()
            .filter(|(event, _)| *event == kind)
            .map(|(_, payload)| payload)
            .collect()
    }

    #[test]
    fn a_batch_over_any_limit_is_rejected_as_too_large_before_anything_is_read() {
        let too_many_records = batch(vec![record(None, SLOW, 300.0, 10.0); 65]);
        let error = validate(&too_many_records).err().expect("rejected");
        assert_eq!(tag(&error), "ProfilerClientBatchTooLarge");
        // The handler returns before the enabled check, so nothing is emitted or recorded.
        let error = ingest_client_batch(too_many_records, "env-1").expect_err("rejected");
        assert_eq!(tag(&error), "ProfilerClientBatchTooLarge");

        let mut too_many_histograms = batch(Vec::new());
        too_many_histograms["histograms"] = json!(vec![
            json!({ "method": "server.getConfig", "buckets": [] });
            65
        ]);
        assert_eq!(
            tag(&validate(&too_many_histograms).err().expect("rejected")),
            "ProfilerClientBatchTooLarge"
        );

        let mut oversized = batch(Vec::new());
        oversized["capabilities"] = json!(["x".repeat(64 * 1024)]);
        assert_eq!(
            tag(&validate(&oversized).err().expect("rejected")),
            "ProfilerClientBatchTooLarge",
            "the byte cap is checked before any field"
        );
    }

    #[test]
    fn a_joined_record_names_its_largest_part() {
        let book = LatencyBook::default();
        book.joins
            .lock()
            .expect("join log")
            .push("c1:7".into(), SLOW.into(), 900.0, 0.0);
        let analysis = analyze_with(
            &book,
            &batch(vec![record(Some("c1:7"), SLOW, 1_500.0, 50.0)]),
        );

        assert_eq!(analysis.findings.len(), 1);
        let (finding, event) = &analysis.findings[0];
        assert_eq!(finding.kind, "rpc.slow");
        assert_eq!(
            analysis.events[*event].0, "client.request",
            "the finding cites its request's event"
        );
        assert_eq!(finding.source, "client");
        assert_eq!(finding.severity, Severity::Warn);
        assert!(finding.title.starts_with(SLOW), "{}", finding.title);
        assert!(
            finding
                .summary
                .contains("server 900 ms · network 600 ms · render 50 ms"),
            "{}",
            finding.summary
        );
        assert_eq!(finding.fingerprint_fields, [SLOW, "server"]);
        assert_eq!(finding.evidence["joinedBy"], "correlationId");
        assert_eq!(
            finding.evidence["queuedMs"],
            Value::Null,
            "queue time is unknown"
        );

        let requests = events(&analysis, "client.request");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["serverMs"], 900.0);
        assert_eq!(requests[0]["networkMs"], 600.0);
        assert_eq!(requests[0]["slowPart"], "server");
        assert_eq!(requests[0]["envId"], "env-1");
    }

    #[test]
    fn a_record_without_a_join_entry_is_never_guessed() {
        let book = LatencyBook::default();
        // Same method, but another request: a time-based guess could pick it.
        book.joins
            .lock()
            .expect("join log")
            .push("c1:6".into(), SLOW.into(), 1_400.0, 0.0);
        // Same ID, but another method: a reused ID, not this request.
        book.joins.lock().expect("join log").push(
            "c1:9".into(),
            "server.getConfig".into(),
            1_400.0,
            0.0,
        );
        let analysis = analyze_with(
            &book,
            &batch(vec![
                record(Some("c1:8"), SLOW, 1_500.0, 50.0),
                record(Some("c1:9"), SLOW, 1_500.0, 50.0),
                record(None, SLOW, 1_500.0, 50.0),
            ]),
        );

        let requests = events(&analysis, "client.request");
        assert_eq!(requests.len(), 3);
        for request in requests {
            assert_eq!(request["joinedBy"], "none");
            assert_eq!(request["slowPart"], "unknown");
            assert_eq!(request["serverMs"], Value::Null);
        }
        assert_eq!(analysis.findings.len(), 3);
        assert!(
            analysis
                .findings
                .iter()
                .all(|(finding, _)| finding.fingerprint_fields == [SLOW, "unknown"])
        );
    }

    #[test]
    fn only_absolute_tiers_of_non_external_methods_raise_findings() {
        let book = LatencyBook::default();
        let analysis = analyze_with(
            &book,
            &batch(vec![
                record(None, SLOW, 300.0, 10.0),
                record(None, "vcs.clone", 12_000.0, 10.0),
                record(None, "server.getConfig", 10_500.0, 10.0),
            ]),
        );
        assert_eq!(events(&analysis, "client.request").len(), 3, "all logged");
        assert_eq!(analysis.findings.len(), 1);
        assert_eq!(
            analysis.findings[0].0.fingerprint_fields[0],
            "server.getConfig"
        );
        assert_eq!(analysis.findings[0].0.severity, Severity::Error);
    }

    #[test]
    fn stalls_and_slow_interactions_raise_client_findings_over_their_thresholds() {
        let book = LatencyBook::default();
        let mut quiet = batch(Vec::new());
        quiet["stalls"] = json!({ "count": 4, "totalMs": 1_000, "maxMs": 400 });
        quiet["interactions"] = json!({ "count": 2, "maxMs": 500 });
        assert!(analyze_with(&book, &quiet).findings.is_empty());

        let mut janky = batch(Vec::new());
        janky["stalls"] = json!({ "count": 5, "totalMs": 1_200, "maxMs": 400 });
        janky["interactions"] = json!({ "count": 2, "maxMs": 620 });
        let analysis = analyze_with(&book, &janky);
        let kinds: Vec<&str> = analysis
            .findings
            .iter()
            .map(|(finding, _)| finding.kind)
            .collect();
        assert_eq!(kinds, ["client.stall", "client.interaction"]);
        assert!(
            analysis.findings.iter().all(|(finding, event)| {
                finding.source == "client" && analysis.events[*event].0 == "client.batch"
            }),
            "client-wide findings cite the batch event"
        );
    }

    #[test]
    fn unknown_methods_are_dropped_and_counted() {
        let book = LatencyBook::default();
        let mut report = batch(vec![
            record(None, "not.aMethod", 1_500.0, 10.0),
            record(None, SLOW, 300.0, 10.0),
        ]);
        report["histograms"] = json!([
            { "method": "not.aMethod", "buckets": [[3, 1]] },
            { "method": SLOW, "buckets": [[50, 1]] },
        ]);
        let analysis = analyze_with(&book, &report);
        assert!(analysis.findings.is_empty());
        let requests = events(&analysis, "client.request");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["method"], SLOW);
        let summary = events(&analysis, "client.batch");
        assert_eq!(summary.len(), 1);
        assert_eq!(summary[0]["droppedUnknownMethods"], 2);
        assert_eq!(summary[0]["histograms"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn invalid_fields_are_rejected() {
        type Mutation = Box<dyn Fn(&mut Value)>;
        let cases: Vec<(&str, Mutation)> = vec![
            (
                "negative rtt",
                Box::new(|b| b["records"][0]["rttMs"] = json!(-1)),
            ),
            (
                "rtt over an hour",
                Box::new(|b| b["records"][0]["rttMs"] = json!(3_600_001)),
            ),
            (
                "render over an hour",
                Box::new(|b| b["records"][0]["renderMs"] = json!(3_600_001)),
            ),
            (
                "window over 10 min",
                Box::new(|b| b["windowMs"] = json!(600_001)),
            ),
            (
                "bucket index 100",
                Box::new(|b| b["histograms"][0]["buckets"] = json!([[100, 1]])),
            ),
            (
                "bucket count over 1e6",
                Box::new(|b| b["histograms"][0]["buckets"] = json!([[3, 1_000_001]])),
            ),
            (
                "stall count over 1e6",
                Box::new(|b| b["stalls"]["count"] = json!(1_000_001)),
            ),
            ("negative heap", Box::new(|b| b["jsHeapBytes"] = json!(-5))),
            (
                "long method",
                Box::new(|b| b["records"][0]["method"] = json!("m".repeat(129))),
            ),
            (
                "long client id",
                Box::new(|b| b["clientId"] = json!("c".repeat(65))),
            ),
            (
                "long correlation id",
                Box::new(|b| b["records"][0]["correlationId"] = json!("r".repeat(65))),
            ),
            (
                "33 capabilities",
                Box::new(|b| b["capabilities"] = json!(vec!["event"; 33])),
            ),
            (
                "long capability",
                Box::new(|b| b["capabilities"] = json!(["e".repeat(65)])),
            ),
            (
                "missing field",
                Box::new(|b| {
                    b.as_object_mut().map(|o| o.remove("stalls"));
                }),
            ),
            ("wrong type", Box::new(|b| b["windowMs"] = json!("10s"))),
        ];
        for (name, mutate) in cases {
            let mut report = batch(vec![record(None, SLOW, 300.0, 10.0)]);
            mutate(&mut report);
            let error = validate(&report)
                .err()
                .unwrap_or_else(|| panic!("{name} accepted"));
            assert_eq!(tag(&error), "ProfilerClientBatchInvalid", "{name}");
        }
        assert!(
            validate(&batch(vec![record(
                Some(&"r".repeat(64)),
                SLOW,
                3_600_000.0,
                0.0
            )]))
            .is_ok()
        );
    }

    #[test]
    fn rejections_have_the_shape_of_the_contract_errors() {
        let fixtures = [
            include_str!(
                "../../../../../packages/contracts/fixtures/rpc-wire/typed-failures/server__reportProfilerClientBatch-00.json"
            ),
            include_str!(
                "../../../../../packages/contracts/fixtures/rpc-wire/typed-failures/server__reportProfilerClientBatch-01.json"
            ),
        ];
        for (fixture, error) in fixtures.into_iter().zip([too_large("m"), invalid("m")]) {
            let fixture: Value = serde_json::from_str(fixture).expect("fixture JSON");
            let expected = &fixture["exit"]["cause"][0]["error"];
            assert_eq!(expected["_tag"], error["_tag"]);
            let keys = |value: &Value| {
                let mut keys: Vec<String> = value
                    .as_object()
                    .expect("an error object")
                    .keys()
                    .cloned()
                    .collect();
                keys.sort();
                keys
            };
            assert_eq!(keys(expected), keys(&error));
        }
    }
}
