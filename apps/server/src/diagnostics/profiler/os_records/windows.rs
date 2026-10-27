//! Windows Error Reporting's Application log Event 1000.

use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::{OsCrashRecord, same_executable};

/// One Event 1000 ("Application Error") as `wevtutil qe /f:xml` prints it.
pub fn parse_event_xml(xml: &str) -> Option<OsCrashRecord> {
    let code = hex(&data(xml, "ExceptionCode")?);
    let module = data(xml, "ModuleName").filter(|module| !module.is_empty());
    // An unreadable offset loses the frame, not the record.
    let offset = data(xml, "FaultingOffset")
        .and_then(|offset| u64::from_str_radix(offset.trim_start_matches("0x"), 16).ok());
    let frames = match (&module, offset) {
        (Some(module), Some(offset)) => vec![format!("{module} + {offset:#x}")],
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

/// The `wevtutil /q:` filter for `exe`'s Event 1000 records in the last `session_age_ms`,
/// applied before `/c:` caps the count, so other applications' crashes cannot crowd ours
/// out. `Data='…'` without a `@Name` predicate matches any `Data` child, so it covers the
/// named and the positional event forms; `event_matches` still checks `AppName` itself, as
/// the name could match another field. XPath cannot escape a quote inside a literal, so a
/// name holding one is matched only after parsing.
pub fn event_query(exe: &str, session_age_ms: u64) -> String {
    let system = format!(
        "System[Provider[@Name='Application Error'] and (EventID=1000) and \
         TimeCreated[timediff(@SystemTime) <= {session_age_ms}]]"
    );
    let name = super::file_name(exe);
    if name.contains(['\'', '"']) {
        format!("*[{system}]")
    } else {
        format!("*[{system} and EventData[Data='{name}']]")
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

/// Where Event 1000 puts each value when its `<Data>` elements carry no names.
const POSITIONS: [(&str, usize); 5] = [
    ("AppName", 0),
    ("ModuleName", 3),
    ("ExceptionCode", 6),
    ("FaultingOffset", 7),
    ("ProcessId", 8),
];

/// The text of `<Data Name='name'>…</Data>`, with either quote style. Windows 10 and Server
/// builds log the same values as unnamed `<Data>` elements, which are read by position.
fn data(xml: &str, name: &str) -> Option<String> {
    ['\'', '"']
        .into_iter()
        .find_map(|quote| {
            let open = format!("Name={quote}{name}{quote}>");
            let value = &xml[xml.find(&open)? + open.len()..];
            Some(unescape(&value[..value.find('<')?]))
        })
        .or_else(|| {
            let (_, index) = POSITIONS.iter().find(|(known, _)| *known == name)?;
            positional_data(xml, *index)
        })
}

/// The `index`th unnamed `<Data>` value in `<EventData>`; `None` once a named one appears.
fn positional_data(xml: &str, index: usize) -> Option<String> {
    let mut rest = &xml[xml.find("<EventData>")?..];
    for position in 0.. {
        rest = &rest[rest.find("<Data")? + "<Data".len()..];
        let (value, after) = match rest.strip_prefix("/>") {
            Some(after) => ("", after),
            None => {
                let content = rest.strip_prefix('>')?;
                let end = content.find("</Data>")?;
                (&content[..end], &content[end + "</Data>".len()..])
            }
        };
        if position == index {
            return Some(unescape(value));
        }
        rest = after;
    }
    None
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
        &["qe", "Application", &query, "/f:xml", "/c:50", "/rd:true"],
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
    /// Windows 10 and Server builds log the same values as unnamed, positional `<Data>`.
    const POSITIONAL: &str =
        include_str!("../../../../tests/fixtures/profiler/wer-event-1000-positional.xml");

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
    fn positional_event_data_parses_to_the_same_record() {
        assert_eq!(parse_event_xml(POSITIONAL), parse_event_xml(EVENT));
        assert!(parse_event_xml(POSITIONAL).is_some());
        let started = datetime!(2026-10-09 10:00 UTC);
        assert!(event_matches(
            POSITIONAL,
            0x2a2c,
            "coddy-studio.exe",
            started
        ));
        assert!(!event_matches(POSITIONAL, 0x2a2c, "other.exe", started));
    }

    #[test]
    fn an_unreadable_offset_drops_only_the_frame() {
        let event = EVENT.replace("000000000002c5a4", "not-hex");
        let record = parse_event_xml(&event).expect("record");
        assert_eq!(record.signal_or_code, "0xc0000005");
        assert_eq!(record.module.as_deref(), Some("ntdll.dll"));
        assert!(record.frames.is_empty());
    }

    #[test]
    fn the_query_filters_by_app_name_in_any_data_element() {
        let query = event_query("~/AppData/Local/Coddy Studio/coddy-studio.exe", 90_000);
        assert_eq!(
            query,
            "*[System[Provider[@Name='Application Error'] and (EventID=1000) and \
             TimeCreated[timediff(@SystemTime) <= 90000]] and EventData[Data='coddy-studio.exe']]"
        );
        // Any `Data` child: the named and the positional forms both match.
        assert!(!query.contains("Data[@Name"), "{query}");
        // A quote cannot be escaped in the XPath literal: the name is then matched only
        // after parsing.
        let quoted = event_query("it's.exe", 1);
        assert!(
            !quoted.contains("EventData") && !quoted.contains("it's"),
            "{quoted}"
        );
    }

    #[test]
    fn event_matches_the_pid_executable_and_session_start() {
        let started = datetime!(2026-10-09 10:00 UTC);
        assert!(event_matches(
            EVENT,
            0x2a2c,
            "~/AppData/Local/Coddy Studio/coddy-studio.exe",
            started
        ));
        assert!(event_matches(EVENT, 0x2a2c, "CODDY_STUDIO.EXE", started));
        assert!(!event_matches(EVENT, 0x2a2d, "coddy-studio.exe", started));
        assert!(!event_matches(EVENT, 0x2a2c, "other.exe", started));
        assert!(!event_matches(
            EVENT,
            0x2a2c,
            "coddy-studio.exe",
            datetime!(2026-10-09 10:06 UTC)
        ));
    }
}
