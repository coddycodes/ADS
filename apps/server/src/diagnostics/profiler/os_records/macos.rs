//! macOS crash reports: `~/Library/Logs/DiagnosticReports/*.ips`.

use serde_json::Value;

use super::{MAX_FRAMES, OsCrashRecord};

/// Larger reports are skipped rather than read.
#[cfg(target_os = "macos")]
const MAX_REPORT_BYTES: u64 = 8 * 1024 * 1024;

/// A crash report (`bug_type` 309) made of a one-line metadata header and the report body.
/// The header is sometimes missing.
pub fn parse_ips(text: &str) -> Option<OsCrashRecord> {
    record(&ips_body(text)?)
}

/// Crash reports are named `<process>-<date>-<time>.ips`; only those of `process` are read.
pub fn is_candidate(file_name: &str, process: &str) -> bool {
    let Some(stem) = file_name.strip_suffix(".ips") else {
        return false;
    };
    stem.get(..process.len())
        .is_some_and(|name| name.eq_ignore_ascii_case(process))
        && stem
            .get(process.len()..)
            .and_then(|rest| rest.strip_prefix('-'))
            .is_some_and(|date| date.starts_with(|c: char| c.is_ascii_digit()))
}

fn ips_body(text: &str) -> Option<Value> {
    let mut documents = serde_json::Deserializer::from_str(text).into_iter::<Value>();
    let first = documents.next()?.ok()?;
    let Some(body) = documents.next() else {
        return Some(first);
    };
    if first
        .get("bug_type")
        .is_some_and(|bug_type| bug_type.as_str() != Some("309"))
    {
        return None;
    }
    body.ok()
}

fn record(body: &Value) -> Option<OsCrashRecord> {
    let exception = body.get("exception")?;
    let signal_or_code = exception
        .get("signal")
        .or_else(|| exception.get("type"))?
        .as_str()?
        .to_owned();
    let images = body.get("usedImages").and_then(Value::as_array);
    let image_name = |frame: &Value| {
        let index = usize::try_from(frame.get("imageIndex")?.as_u64()?).ok()?;
        images?.get(index)?.get("name")?.as_str()
    };
    let faulting = usize::try_from(body.get("faultingThread")?.as_u64()?).ok()?;
    let frames = body
        .get("threads")?
        .get(faulting)?
        .get("frames")?
        .as_array()?;
    let module = frames.first().and_then(image_name).map(str::to_owned);
    let frames = frames
        .iter()
        .take(MAX_FRAMES)
        .map(|frame| {
            let image = image_name(frame).unwrap_or("???");
            let offset = frame
                .get("imageOffset")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            match frame.get("symbol").and_then(Value::as_str) {
                Some(symbol) => format!("{symbol} ({image} + {offset:#x})"),
                None => format!("{image} + {offset:#x}"),
            }
        })
        .collect();
    let process = body.get("procName").and_then(Value::as_str).unwrap_or("");
    let pid = body.get("pid").and_then(Value::as_u64).unwrap_or(0);
    Some(OsCrashRecord {
        source: "ips",
        signal_or_code,
        module,
        frames,
        raw_ref: format!("{process} pid {pid}"),
    })
}

/// The newest crash report for the previous run's pid and process name, written after the
/// run began.
#[cfg(target_os = "macos")]
pub(super) async fn harvest(previous: &super::SessionRecord) -> Option<OsCrashRecord> {
    let previous = previous.clone();
    // The harvest timeout cannot cancel a blocking scan, so the scan keeps its own deadline.
    let deadline = std::time::Instant::now() + super::HARVEST_TIMEOUT;
    tokio::task::spawn_blocking(move || find_report(&previous, deadline))
        .await
        .ok()
        .flatten()
}

#[cfg(target_os = "macos")]
fn find_report(
    previous: &super::SessionRecord,
    deadline: std::time::Instant,
) -> Option<OsCrashRecord> {
    let started = std::time::SystemTime::from(super::started_at(previous)?);
    let name = super::file_name(&previous.exe);
    let dir = dirs::home_dir()?.join("Library/Logs/DiagnosticReports");
    let mut reports: Vec<(std::time::SystemTime, std::path::PathBuf)> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .take_while(|_| std::time::Instant::now() < deadline)
        .filter(|entry| is_candidate(&entry.file_name().to_string_lossy(), name))
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            (metadata.len() <= MAX_REPORT_BYTES).then_some(())?;
            Some((metadata.modified().ok()?, entry.path()))
        })
        .filter(|(modified, _)| *modified >= started)
        .collect();
    reports.sort();
    reports
        .into_iter()
        .rev()
        .take_while(|_| std::time::Instant::now() < deadline)
        .find_map(|(_, path)| {
            let body = ips_body(&std::fs::read_to_string(&path).ok()?)?;
            let matches = body.get("pid").and_then(Value::as_u64) == Some(u64::from(previous.pid))
                && body.get("procName").and_then(Value::as_str) == Some(name);
            let mut record = record(&body).filter(|_| matches)?;
            record.raw_ref = super::super::redact::redact_path(&path.to_string_lossy());
            Some(record)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const IPS: &str = include_str!("../../../../tests/fixtures/profiler/crash.ips");

    #[test]
    fn ips_with_header_yields_the_faulting_thread() {
        let record = parse_ips(IPS).expect("record");
        assert_eq!(record.source, "ips");
        assert_eq!(record.signal_or_code, "SIGSEGV");
        assert_eq!(record.module.as_deref(), Some("bibcode"));
        assert_eq!(
            record.frames,
            vec![
                "bibcode_server::crash_here (bibcode + 0x12d687)".to_owned(),
                "libsystem_kernel.dylib + 0x929".to_owned(),
            ]
        );
    }

    #[test]
    fn only_reports_named_after_the_process_are_read() {
        assert!(is_candidate("bibcode-2026-10-09-100500.ips", "bibcode"));
        assert!(is_candidate("BiBCode-2026-10-09-100500.ips", "bibcode"));
        assert!(!is_candidate(
            "bibcode-helper-2026-10-09-100500.ips",
            "bibcode"
        ));
        assert!(!is_candidate("Safari-2026-10-09-100500.ips", "bibcode"));
        assert!(!is_candidate("bibcode-2026-10-09-100500.diag", "bibcode"));
    }

    #[test]
    fn ips_without_header_parses_the_body() {
        let body = IPS.split_once('\n').unwrap().1;
        assert_eq!(parse_ips(body), parse_ips(IPS));
        assert!(parse_ips(body).is_some());
        let stackshot = IPS.replacen("\"bug_type\":\"309\"", "\"bug_type\":\"288\"", 1);
        assert!(parse_ips(&stackshot).is_none());
        assert!(
            parse_ips(IPS.lines().next().unwrap()).is_none(),
            "header only"
        );
    }
}
