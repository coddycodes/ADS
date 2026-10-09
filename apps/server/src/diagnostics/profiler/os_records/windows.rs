//! Windows Error Reporting's Application log Event 1000.

use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::{OsCrashRecord, same_executable};

/// One Event 1000 ("Application Error") as `wevtutil qe /f:xml` prints it.
pub fn parse_event_xml(xml: &str) -> Option<OsCrashRecord> {
    let code = hex(&data(xml, "ExceptionCode")?);
    let module = data(xml, "ModuleName").filter(|module| !module.is_empty());
    let frames = match (&module, data(xml, "FaultingOffset")) {
        (Some(module), Some(offset)) => {
            let offset = u64::from_str_radix(offset.trim_start_matches("0x"), 16).ok()?;
            vec![format!("{module} + {offset:#x}")]
        }
        _ => Vec::new(),
    };
    let pid = data(xml, "ProcessId").unwrap_or_default();
    Some(OsCrashRecord {
        source: "wer",
        signal_or_code: code,
        module,
        frames,
        raw_ref: format!("Application log, Event 1000, process {pid}"),
    })
}

/// Whether the event reports a crash of `pid` running `exe`, after the run began.
pub fn event_matches(xml: &str, pid: u32, exe: &str, started: OffsetDateTime) -> bool {
    let event_pid = data(xml, "ProcessId").and_then(|value| match value.strip_prefix("0x") {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => value.parse().ok(),
    });
    let created =
        attribute(xml, "SystemTime").and_then(|time| OffsetDateTime::parse(&time, &Rfc3339).ok());
    event_pid == Some(pid)
        && data(xml, "AppName").is_some_and(|app| same_executable(&app, exe))
        && created.is_some_and(|created| created >= started)
}

/// The `wevtutil /q:` filter for `exe`'s Event 1000 records in the last `session_age_ms`. It
/// narrows the log before `/c:` caps the count, so other applications' crashes cannot crowd
/// ours out. `event_matches` still checks the pid, the time and the name afterwards.
pub fn event_query(exe: &str, session_age_ms: u64) -> String {
    let system = format!(
        "System[Provider[@Name='Application Error'] and (EventID=1000) and \
         TimeCreated[timediff(@SystemTime) <= {session_age_ms}]]"
    );
    let name = super::file_name(exe);
    // ponytail: XPath compares `AppName` exactly, so a launch path cased differently from
    // the event's name misses the record; match without the name (and page) if that shows up.
    if name.contains(['\'', '"']) {
        format!("*[{system}]")
    } else {
        format!("*[{system} and EventData[Data[@Name='AppName']='{name}']]")
    }
}

/// `c0000005` → `0xc0000005`.
fn hex(code: &str) -> String {
    let code = code.trim().to_ascii_lowercase();
    if code.starts_with("0x") {
        code
    } else {
        format!("0x{code}")
    }
}

/// The text of `<Data Name='name'>…</Data>`, with either quote style.
fn data(xml: &str, name: &str) -> Option<String> {
    ['\'', '"'].into_iter().find_map(|quote| {
        let open = format!("Name={quote}{name}{quote}>");
        let value = &xml[xml.find(&open)? + open.len()..];
        Some(unescape(&value[..value.find('<')?]))
    })
}

/// The value of the first `name='…'` attribute, with either quote style.
fn attribute(xml: &str, name: &str) -> Option<String> {
    ['\'', '"'].into_iter().find_map(|quote| {
        let open = format!("{name}={quote}");
        let value = &xml[xml.find(&open)? + open.len()..];
        Some(unescape(&value[..value.find(quote)?]))
    })
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The newest Event 1000 records for the previous run's pid and executable.
#[cfg(windows)]
pub(super) async fn harvest(previous: &super::SessionRecord) -> Option<OsCrashRecord> {
    let started = super::started_at(previous)?;
    // A minute of slack for clock adjustments since the run began.
    let age_ms = (OffsetDateTime::now_utc() - started).whole_milliseconds() + 60_000;
    let query = format!(
        "/q:{}",
        event_query(&previous.exe, u64::try_from(age_ms).unwrap_or(60_000))
    );
    let events = super::query(
        "wevtutil",
        &["qe", "Application", &query, "/f:xml", "/c:20", "/rd:true"],
    )
    .await?;
    events
        .split_inclusive("</Event>")
        .find(|event| event_matches(event, previous.pid, &previous.exe, started))
        .and_then(parse_event_xml)
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    const EVENT: &str = include_str!("../../../../tests/fixtures/profiler/wer-event-1000.xml");

    #[test]
    fn event_1000_yields_the_module_and_exception_code() {
        let record = parse_event_xml(EVENT).expect("record");
        assert_eq!(record.source, "wer");
        assert_eq!(record.signal_or_code, "0xc0000005");
        assert_eq!(record.module.as_deref(), Some("ntdll.dll"));
        assert_eq!(record.frames, vec!["ntdll.dll + 0x2c5a4".to_owned()]);
        let double_quoted = EVENT.replace('\'', "\"");
        assert_eq!(parse_event_xml(&double_quoted), Some(record));
        assert!(parse_event_xml("<Event></Event>").is_none());
    }

    #[test]
    fn the_query_filters_by_provider_event_and_session_window_before_the_count_limit() {
        assert_eq!(
            event_query("~/AppData/Local/BiBCode/bibcode.exe", 90_000),
            "*[System[Provider[@Name='Application Error'] and (EventID=1000) and \
             TimeCreated[timediff(@SystemTime) <= 90000]] and \
             EventData[Data[@Name='AppName']='bibcode.exe']]"
        );
        // A quote cannot appear inside the XPath literal; the name is then matched locally.
        assert!(!event_query("it's.exe", 1).contains("AppName"));
    }

    #[test]
    fn event_matches_the_pid_executable_and_session_start() {
        let started = datetime!(2026-10-09 10:00 UTC);
        assert!(event_matches(
            EVENT,
            0x2a2c,
            "~/AppData/Local/BiBCode/bibcode.exe",
            started
        ));
        assert!(event_matches(EVENT, 0x2a2c, "BIBCODE.EXE", started));
        assert!(!event_matches(EVENT, 0x2a2d, "bibcode.exe", started));
        assert!(!event_matches(EVENT, 0x2a2c, "other.exe", started));
        assert!(!event_matches(
            EVENT,
            0x2a2c,
            "bibcode.exe",
            datetime!(2026-10-09 10:06 UTC)
        ));
    }
}
