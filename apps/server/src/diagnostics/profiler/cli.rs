//! `bibcode profiler`: findings, events and crash records from the profiler directory, read
//! through [`Reader`], their stacks symbolicated, and an MCP server over them for AI agents. It
//! never starts a server and works while one is stopped or running. Only `symbolicate
//! --download` writes, and only outside the profiler directory.

use std::{
    ffi::OsStr,
    io::{self, Write},
    path::PathBuf,
    time::Duration,
};

use clap::{Args, Parser, Subcommand};
use serde::Serialize as _;
use serde_json::{Map, Value, ser::Formatter};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::{
    mcp::run_stdio_mcp,
    profiler_dir,
    reader::{MAX_FOLLOW, MAX_LIMIT, Reader},
    symbolicate::{SymbolicateError, Target, symbolicate},
};

/// Envelope fields of an event line; the rest is its payload.
const ENVELOPE: [&str; 7] = ["v", "seq", "sid", "eventId", "ts", "kind", "source"];
const PAYLOAD_WIDTH: usize = 160;
const LINE_WIDTH: usize = 200;

#[derive(Clone, Debug, Args)]
pub struct ProfilerArgs {
    #[command(subcommand)]
    pub command: ProfilerSubcommand,
}

#[derive(Clone, Debug, Subcommand)]
pub enum ProfilerSubcommand {
    /// Show whether the profiler is on, its disk use, capabilities and newest findings.
    Status {
        /// Print one JSON document.
        #[arg(long)]
        json: bool,
    },
    /// List findings, newest first.
    Findings {
        #[command(flatten)]
        query: QueryArgs,
        /// The most findings to list.
        #[arg(long, default_value_t = 50, conflicts_with = "follow", value_parser = limit_parser())]
        limit: u16,
    },
    /// List recorded events, ending with the newest.
    Events {
        #[command(flatten)]
        query: QueryArgs,
        /// The most events to list; the newest are kept.
        #[arg(long, default_value_t = 200, conflicts_with = "follow", value_parser = limit_parser())]
        limit: u16,
    },
    /// List crash records, newest first.
    Crashes {
        /// The most crash records to list.
        #[arg(long, default_value_t = 20, value_parser = limit_parser())]
        limit: u16,
        /// Print one JSON document.
        #[arg(long)]
        json: bool,
    },
    /// Name the functions and source lines in a crash's stack or a panic's frames.
    Symbolicate {
        /// A crash id, as `crashes` lists it, or a panic event as <sid>:<seq>.
        #[arg(value_parser = Target::parse)]
        target: Target,
        /// A Breakpad symbol store (<module>/<debug id>/<module>.sym) to read first; repeatable.
        #[arg(long = "symbols", value_name = "DIR")]
        symbols: Vec<PathBuf>,
        /// When no local symbols match, download this release's symbols from GitHub.
        #[arg(long)]
        download: bool,
        /// Print one JSON document.
        #[arg(long)]
        json: bool,
    },
    /// Serve these records to an AI agent: a read-only MCP server on stdin and stdout.
    Mcp,
}

#[derive(Clone, Debug, Args)]
pub struct QueryArgs {
    /// Only records from this long ago (30s, 10m, 1h, 2d) or after this RFC3339 timestamp.
    #[arg(long, conflicts_with = "follow", value_parser = parse_since)]
    pub since: Option<OffsetDateTime>,
    /// Only this kind; a trailing `*` matches a prefix (`rpc.*`).
    #[arg(long)]
    pub kind: Option<String>,
    /// Print new records as they arrive, for `--duration`, instead of listing past ones.
    #[arg(long, requires = "duration")]
    pub follow: bool,
    /// How long to follow: 30s, 10m, at most 10m.
    #[arg(long, requires = "follow", value_parser = parse_follow_duration)]
    pub duration: Option<Duration>,
    /// Print one JSON document; with `--follow`, one JSON line per record.
    #[arg(long)]
    pub json: bool,
}

fn limit_parser() -> impl clap::builder::TypedValueParser<Value = u16> {
    clap::value_parser!(u16).range(1..=i64::from(MAX_LIMIT))
}

#[derive(Debug, Error)]
enum CliError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Symbolicate(#[from] SymbolicateError),
}

/// Runs a `profiler` command against `<state_dir>/profiler` and returns the exit code.
/// Blocks, and sleeps between polls under `--follow`.
pub fn run_profiler_cli(args: ProfilerArgs, state_dir: PathBuf) -> i32 {
    let reader = Reader::new(profiler_dir(&state_dir));
    let mut out = io::stdout().lock();
    match execute(args.command, &reader, &mut out) {
        Ok(()) => 0,
        // The reader of the pipe left (`| head`); that is not a failure.
        Err(CliError::Io(error)) if error.kind() == io::ErrorKind::BrokenPipe => 0,
        Err(error) => {
            let _ = writeln!(io::stderr(), "bibcode: {error}");
            1
        }
    }
}

/// The desktop executable ships no separate `bibcode`, so `<desktop exe> profiler …` runs the
/// command here, before Tauri starts. Returns the exit code, or `None` when the process was
/// not started that way. `attach_console` runs first: a release build on Windows has no
/// console of its own to print to.
///
/// Only the profiler arguments are parsed, not the server's environment-backed options. The
/// data root is the desktop's: `BIBCODE_HOME` or `~/.bibcode`, and the state directory its
/// in-process server uses.
pub fn desktop_dispatch(attach_console: fn()) -> Option<i32> {
    if std::env::args_os().nth(1).as_deref() != Some(OsStr::new("profiler")) {
        return None;
    }
    attach_console();
    let cli = match DesktopCli::try_parse_from(std::env::args_os().skip(1)) {
        Ok(cli) => cli,
        Err(error) => {
            let _ = error.print();
            return Some(error.exit_code());
        }
    };
    Some(match crate::config::profiler_state_dir(None, None) {
        Ok(state_dir) => run_profiler_cli(cli.args, state_dir),
        Err(error) => {
            let _ = writeln!(io::stderr(), "bibcode: {error}");
            1
        }
    })
}

#[derive(Debug, Parser)]
#[command(
    name = "profiler",
    about = "Read BiBCode profiler data from disk. The data root is BIBCODE_HOME, or ~/.bibcode."
)]
struct DesktopCli {
    #[command(flatten)]
    args: ProfilerArgs,
}

fn execute(
    command: ProfilerSubcommand,
    reader: &Reader,
    out: &mut impl Write,
) -> Result<(), CliError> {
    match command {
        ProfilerSubcommand::Status { json } => {
            let status = reader.status();
            if json {
                write_json(out, &status)?;
            } else {
                write_status(out, &status)?;
            }
        }
        ProfilerSubcommand::Findings { query, limit } => {
            let kind = query.kind.as_deref();
            if let (true, Some(duration)) = (query.follow, query.duration) {
                reader.follow_findings(kind, duration, |finding| {
                    write_followed(out, &finding, query.json, finding_line)
                })?;
            } else {
                let findings = reader.findings(query.since, kind, usize::from(limit));
                write_list(out, findings, query.json, "No findings.", finding_line)?;
            }
        }
        ProfilerSubcommand::Events { query, limit } => {
            let kind = query.kind.as_deref();
            if let (true, Some(duration)) = (query.follow, query.duration) {
                reader.follow(kind, duration, |event| {
                    write_followed(out, &event, query.json, event_line)
                })?;
            } else {
                let events = reader.events(query.since, kind, usize::from(limit));
                write_list(out, events, query.json, "No events.", event_line)?;
            }
        }
        ProfilerSubcommand::Crashes { limit, json } => {
            let crashes = reader.crashes(usize::from(limit));
            write_list(out, crashes, json, "No crash records.", crash_line)?;
        }
        ProfilerSubcommand::Symbolicate {
            target,
            symbols,
            download,
            json,
        } => {
            let symbolicated = symbolicate(reader, &target, &symbols, download)?;
            if json {
                write_json(out, &symbolicated)?;
            } else {
                write_symbolicated(out, &symbolicated)?;
            }
        }
        ProfilerSubcommand::Mcp => run_stdio_mcp(reader.clone(), io::stdin().lock(), &mut *out)?,
    }
    out.flush()?;
    Ok(())
}

fn write_list(
    out: &mut impl Write,
    items: Vec<Value>,
    json: bool,
    empty: &str,
    line: fn(&Value) -> String,
) -> io::Result<()> {
    if json {
        write_json(out, &Value::Array(items))
    } else if items.is_empty() {
        writeln!(out, "{empty}")
    } else {
        items
            .iter()
            .try_for_each(|item| writeln!(out, "{}", line(item)))
    }
}

/// One record of a follow, flushed at once so a pipe sees it as it arrives.
fn write_followed(
    out: &mut impl Write,
    item: &Value,
    json: bool,
    line: fn(&Value) -> String,
) -> io::Result<()> {
    if json {
        write_json(out, item)?;
    } else {
        writeln!(out, "{}", line(item))?;
    }
    out.flush()
}

/// `value` as compact JSON and a newline: what `--json` prints.
pub(super) fn write_json(out: &mut impl Write, value: &Value) -> io::Result<()> {
    value.serialize(&mut serde_json::Serializer::with_formatter(
        &mut *out,
        EscapeControls,
    ))?;
    writeln!(out)
}

/// `value` as the same compact JSON as [`write_json`], without the newline.
pub(super) fn to_json(value: &Value) -> String {
    let mut text = Vec::new();
    write_json(&mut text, value).expect("writing to a Vec cannot fail");
    text.pop();
    String::from_utf8(text).expect("serde_json writes UTF-8")
}

/// Compact JSON, except that DEL and the C1 controls (CSI is U+009B, OSC is U+009D), the
/// Unicode format characters and the Tag characters are written as `\u007f`, `\u202e` and so on
/// (a surrogate pair above U+FFFF). serde_json escapes only U+0000 to U+001F, so these would
/// reach a terminal as they are, or a model as text a person never sees; a fragment it hands
/// over holds no other control character. They are ordinary JSON escapes: a parser reads the
/// same value. The CLI's `--json`, the MCP server's lines and its tool `text` (JSON inside a
/// JSON string, so the escapes survive the client's parse) write through this; an MCP client
/// still decodes `structuredContent` to the raw characters.
struct EscapeControls;

impl Formatter for EscapeControls {
    fn write_string_fragment<W>(&mut self, writer: &mut W, fragment: &str) -> io::Result<()>
    where
        W: ?Sized + Write,
    {
        let mut written = 0;
        for (at, c) in fragment.char_indices() {
            if c.is_control() || is_hidden_format(c) {
                writer.write_all(&fragment.as_bytes()[written..at])?;
                for unit in c.encode_utf16(&mut [0; 2]) {
                    write!(writer, "\\u{unit:04x}")?;
                }
                written = at + c.len_utf8();
            }
        }
        writer.write_all(&fragment.as_bytes()[written..])
    }
}

/// One line per frame, innermost first: `module!function [file:line]` where symbols named it,
/// `module+offset` where none did.
fn write_symbolicated(out: &mut impl Write, symbolicated: &Value) -> io::Result<()> {
    if let Some(reason) = symbolicated["crashReason"].as_str() {
        writeln!(out, "Crash: {}", plain(reason))?;
    }
    if symbolicated["message"].is_string() {
        writeln!(
            out,
            "Panic on thread {}: {} at {}",
            text(symbolicated, "thread", "?"),
            text(symbolicated, "message", ""),
            text(symbolicated, "location", "?"),
        )?;
    }
    let frames = symbolicated
        .pointer("/crashingThread/frames")
        .or_else(|| symbolicated.get("frames"))
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    for (index, frame) in frames.iter().enumerate() {
        let mut line = format!("{index:>3} {}", text(frame, "module", "?"));
        match frame["function"].as_str() {
            Some(function) => line.push_str(&format!("!{}", plain(function))),
            None => line.push_str(&format!("+{:#x}", frame["offset"].as_u64().unwrap_or(0))),
        }
        if let (Some(file), Some(number)) = (frame["file"].as_str(), frame["line"].as_u64()) {
            line.push_str(&format!(" [{}:{number}]", plain(file)));
        }
        writeln!(out, "{line}")?;
    }
    if !frames.iter().any(|frame| frame["function"].is_string()) {
        writeln!(
            out,
            "No symbols named these frames: pass --symbols with a symbol store, or --download for the release's symbols."
        )?;
    }
    Ok(())
}

fn write_status(out: &mut impl Write, status: &Value) -> io::Result<()> {
    let on = status["enabled"].as_bool().unwrap_or(false);
    writeln!(out, "Profiler setting: {}", if on { "on" } else { "off" })?;
    if let Some(since) = status["recordingSince"].as_str() {
        writeln!(out, "Recording since: {}", plain(since))?;
    }
    writeln!(
        out,
        "Disk: {}",
        human_bytes(status["diskBytes"].as_u64().unwrap_or(0))
    )?;
    writeln!(
        out,
        "Events: {} ({} dropped)",
        status["eventCount"].as_u64().unwrap_or(0),
        status["droppedEvents"].as_u64().unwrap_or(0)
    )?;
    writeln!(out, "Last crash: {}", text(status, "lastCrashAt", "none"))?;
    let capabilities = &status["capabilities"];
    writeln!(
        out,
        "Capabilities: native dumps {}, OS crash records {}, storage {}",
        text(capabilities, "nativeDumps", "unknown"),
        text(capabilities, "osCrashRecords", "unknown"),
        text(capabilities, "storage", "unknown"),
    )?;
    let findings = status["findings"].as_array().map_or(&[][..], Vec::as_slice);
    if findings.is_empty() {
        writeln!(out, "Findings: none")
    } else {
        writeln!(out, "Findings: {} newest", findings.len())?;
        findings
            .iter()
            .try_for_each(|finding| writeln!(out, "  {}", finding_line(finding)))
    }
}

fn finding_line(finding: &Value) -> String {
    clip(
        format!(
            "{} {} {} {} x{} {}",
            text(finding, "lastSeen", "-"),
            text(finding, "severity", "-"),
            text(finding, "state", "-"),
            text(finding, "kind", "-"),
            finding["count"].as_u64().unwrap_or(0),
            text(finding, "title", ""),
        ),
        LINE_WIDTH,
    )
}

fn event_line(event: &Value) -> String {
    let mut line = format!(
        "{} {} {}",
        text(event, "ts", "-"),
        text(event, "source", "-"),
        text(event, "kind", "-")
    );
    let payload: Map<String, Value> = event
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(key, _)| !ENVELOPE.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    if !payload.is_empty() {
        // serde_json escapes only U+0000 to U+001F; DEL and the C1 controls (CSI, OSC) are
        // written as they are, so the JSON text needs `plain` like every other field.
        line.push(' ');
        line.push_str(&clip(
            plain(&Value::Object(payload).to_string()),
            PAYLOAD_WIDTH,
        ));
    }
    line
}

/// The id and the scalar fields. The rest of a crash record is long (the events before it)
/// and belongs to `--json`.
fn crash_line(crash: &Value) -> String {
    let mut line = text(crash, "id", "-");
    for (key, value) in crash.as_object().into_iter().flatten() {
        let shown = match value {
            Value::String(string) => plain(string),
            Value::Number(number) => number.to_string(),
            Value::Bool(flag) => flag.to_string(),
            _ => continue,
        };
        if key != "id" {
            line.push_str(&format!(" {}={shown}", plain(key)));
        }
    }
    clip(line, LINE_WIDTH)
}

/// A string field made safe to print by `plain`: values come from monitored processes and
/// must not drive the terminal or mislead the reader.
fn text(value: &Value, key: &str, missing: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .map_or_else(|| missing.to_owned(), plain)
}

pub(super) fn plain(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() || is_hidden_format(c) {
                '?'
            } else {
                c
            }
        })
        .collect()
}

/// Unicode format (Cf) characters and the Tag characters: they print as nothing, or reorder
/// the text around them (the "Trojan Source" set), and Tags can carry text a person never
/// sees. U+00AD soft hyphen, U+0600 to U+0605, U+061C Arabic letter mark, U+06DD, U+070F,
/// U+0890 and U+0891, U+08E2, U+180E Mongolian vowel separator, U+200B to U+200F zero-width
/// characters and direction marks, U+202A to U+202E embeddings and overrides, U+2060 to U+206F
/// word joiner, invisible operators, isolates and deprecated shaping controls, U+FEFF byte
/// order mark, U+FFF9 to U+FFFB interlinear annotations, U+110BD, U+110CD, U+13430 to
/// U+1343F, U+1BCA0 to U+1BCA3, U+1D173 to U+1D17A, and U+E0000 to U+E007F, the Tags.
fn is_hidden_format(c: char) -> bool {
    matches!(
        c,
        '\u{ad}'
            | '\u{600}'..='\u{605}'
            | '\u{61c}'
            | '\u{6dd}'
            | '\u{70f}'
            | '\u{890}'..='\u{891}'
            | '\u{8e2}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
            | '\u{110bd}'
            | '\u{110cd}'
            | '\u{13430}'..='\u{1343f}'
            | '\u{1bca0}'..='\u{1bca3}'
            | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0000}'..='\u{e007f}'
    )
}

fn clip(text: String, width: usize) -> String {
    if text.chars().count() <= width {
        return text;
    }
    let mut clipped: String = text.chars().take(width).collect();
    clipped.push_str("...");
    clipped
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// `30s`, `10m`, `1h` or `2d`.
fn parse_span(input: &str) -> Option<Duration> {
    let unit = input.chars().last()?;
    let digits = &input[..input.len() - unit.len_utf8()];
    let seconds_per_unit: u64 = match unit {
        's' => 1,
        'm' => 60,
        'h' => 3600,
        'd' => 86_400,
        _ => return None,
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let count: u32 = digits.parse().ok()?;
    Some(Duration::from_secs(u64::from(count) * seconds_per_unit))
}

pub(super) fn parse_since(input: &str) -> Result<OffsetDateTime, String> {
    parse_since_at(input, OffsetDateTime::now_utc())
}

/// A span back from `now`, or an RFC3339 timestamp.
fn parse_since_at(input: &str, now: OffsetDateTime) -> Result<OffsetDateTime, String> {
    if let Ok(at) = OffsetDateTime::parse(input, &Rfc3339) {
        return Ok(at);
    }
    let span = parse_span(input).ok_or_else(|| {
        format!(
            "invalid time \"{input}\": use a span such as 30s, 10m, 1h or 2d, or an RFC3339 timestamp such as 2026-10-09T12:00:00Z"
        )
    })?;
    time::Duration::try_from(span)
        .ok()
        .and_then(|span| now.checked_sub(span))
        .ok_or_else(|| format!("time \"{input}\" is out of range"))
}

fn parse_follow_duration(input: &str) -> Result<Duration, String> {
    let duration = parse_span(input).ok_or_else(|| {
        format!("invalid duration \"{input}\": use a number and a unit, such as 30s or 10m")
    })?;
    if duration.is_zero() || duration > MAX_FOLLOW {
        return Err(format!(
            "duration \"{input}\" must be longer than zero and at most {}m",
            MAX_FOLLOW.as_secs() / 60
        ));
    }
    Ok(duration)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;

    fn parse(args: &[&str]) -> Result<ProfilerSubcommand, clap::Error> {
        DesktopCli::try_parse_from(std::iter::once("profiler").chain(args.iter().copied()))
            .map(|cli| cli.args.command)
    }

    fn run(args: &[&str], dir: &std::path::Path) -> String {
        let mut out = Vec::new();
        execute(parse(args).unwrap(), &Reader::new(dir), &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    /// The records as the writer stores them: one `Value::to_string` per line, which leaves DEL
    /// and the C1 controls unescaped.
    fn ndjson(records: &[Value]) -> String {
        records.iter().map(|record| format!("{record}\n")).collect()
    }

    fn event_record(seq: u64, kind: &str, source: &str, payload: Value) -> Value {
        let mut event = json!({
            "v": 1, "seq": seq, "sid": "s", "ts": "2026-10-09T10:00:00Z",
            "kind": kind, "source": source,
        });
        event
            .as_object_mut()
            .unwrap()
            .extend(payload.as_object().unwrap().clone());
        event
    }

    fn finding_record(kind: &str, title: &str, summary: &str) -> Value {
        json!({
            "id": format!("{kind}-aaaaaaaaaaaa"), "fingerprint": "aaaaaaaaaaaa", "kind": kind,
            "severity": "warn", "firstSeen": "2026-10-09T10:00:00Z",
            "lastSeen": "2026-10-09T10:00:00Z", "count": 3, "source": "server",
            "title": title, "summary": summary, "state": "open", "evidence": {},
        })
    }

    /// A profiler directory holding these records.
    fn seeded_dir(
        events: &[Value],
        findings: &[Value],
        crashes: &[Value],
    ) -> (tempfile::TempDir, std::path::PathBuf) {
        let state = tempfile::tempdir().unwrap();
        let dir = state.path().join("profiler");
        fs::create_dir_all(dir.join("crashes")).unwrap();
        fs::write(dir.join("events-000000.ndjson"), ndjson(events)).unwrap();
        fs::write(dir.join("findings.ndjson"), ndjson(findings)).unwrap();
        for (index, crash) in crashes.iter().enumerate() {
            fs::write(
                dir.join(format!("crashes/crash-{index}.json")),
                crash.to_string(),
            )
            .unwrap();
        }
        (state, dir)
    }

    /// DEL is the byte 0x7F, and a C1 control (U+0080 to U+009F) is the UTF-8 pair 0xC2 0x80
    /// to 0xC2 0x9F.
    fn assert_no_raw_del_or_c1(bytes: &[u8], context: &str) {
        let shown = String::from_utf8_lossy(bytes);
        assert!(!bytes.contains(&0x7f), "{context}: raw DEL in {shown:?}");
        assert!(
            !bytes
                .windows(2)
                .any(|pair| pair[0] == 0xc2 && (0x80..=0x9f).contains(&pair[1])),
            "{context}: raw C1 control in {shown:?}"
        );
    }

    #[test]
    fn since_accepts_spans_and_timestamps() {
        let now = OffsetDateTime::parse("2026-10-09T12:00:00Z", &Rfc3339).unwrap();
        for (input, expected) in [
            ("30s", "2026-10-09T11:59:30Z"),
            ("10m", "2026-10-09T11:50:00Z"),
            ("1h", "2026-10-09T11:00:00Z"),
            ("2d", "2026-10-07T12:00:00Z"),
            ("2026-10-09T08:30:00Z", "2026-10-09T08:30:00Z"),
            ("2026-10-09T10:30:00+02:00", "2026-10-09T08:30:00Z"),
        ] {
            let at = parse_since_at(input, now).unwrap_or_else(|error| panic!("{input}: {error}"));
            assert_eq!(
                at,
                OffsetDateTime::parse(expected, &Rfc3339).unwrap(),
                "{input}"
            );
        }
    }

    #[test]
    fn since_rejects_everything_else_with_a_message_naming_the_accepted_forms() {
        let now = OffsetDateTime::parse("2026-10-09T12:00:00Z", &Rfc3339).unwrap();
        for input in [
            "",
            "10",
            "m",
            "1w",
            "-5m",
            "1.5h",
            "5 m",
            "5M",
            "yesterday",
            "2026-10-09",
            "+5m",
            "4294967295d",
            "99999999999d",
            "5\u{e9}",
        ] {
            let error = parse_since_at(input, now).unwrap_err();
            assert!(
                error.contains("30s, 10m, 1h or 2d") || error.contains("out of range"),
                "{input}: {error}"
            );
        }
    }

    #[test]
    fn follow_needs_a_bounded_duration_and_excludes_history_options() {
        assert!(parse(&["events", "--follow"]).is_err());
        assert!(parse(&["events", "--duration", "60s"]).is_err());
        assert!(parse(&["events", "--follow", "--duration", "0s"]).is_err());
        assert!(parse(&["events", "--follow", "--duration", "11m"]).is_err());
        assert!(parse(&["events", "--follow", "--duration", "90"]).is_err());
        assert!(parse(&["events", "--follow", "--duration", "60s", "--since", "1h"]).is_err());
        assert!(parse(&["events", "--follow", "--duration", "60s", "--limit", "5"]).is_err());
        assert!(parse(&["findings", "--follow", "--duration", "60s", "--limit", "5"]).is_err());
        assert!(parse(&["events", "--limit", "0"]).is_err());
        assert!(parse(&["events", "--limit", "10001"]).is_err());
        let ProfilerSubcommand::Events { query, .. } =
            parse(&["events", "--follow", "--duration", "10m", "--kind", "rpc.*"]).unwrap()
        else {
            panic!("events");
        };
        assert_eq!(query.duration, Some(Duration::from_secs(600)));
        assert_eq!(query.kind.as_deref(), Some("rpc.*"));
    }

    #[test]
    fn help_lists_symbolicate_and_mcp_and_their_arguments_parse() {
        use clap::CommandFactory;
        let help = DesktopCli::command().render_help().to_string();
        assert!(
            help.contains("symbolicate") && help.contains("mcp"),
            "{help}"
        );
        let ProfilerSubcommand::Symbolicate {
            target,
            symbols,
            download,
            json,
        } = parse(&[
            "symbolicate",
            "aaaa1111:7",
            "--symbols",
            "/a",
            "--symbols",
            "/b",
            "--json",
        ])
        .unwrap()
        else {
            panic!("symbolicate");
        };
        assert_eq!(
            target,
            Target::Panic {
                sid: "aaaa1111".into(),
                seq: 7
            }
        );
        assert_eq!(symbols, [PathBuf::from("/a"), PathBuf::from("/b")]);
        assert!(json && !download, "--download is off unless asked");
        assert!(parse(&["symbolicate", "aaaa1111:x"]).is_err());
        assert!(matches!(parse(&["mcp"]), Ok(ProfilerSubcommand::Mcp)));
        let error = execute(
            parse(&["symbolicate", "no-such-crash"]).unwrap(),
            &Reader::new("/nonexistent/profiler"),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(
            error.to_string().contains("no crash record no-such-crash"),
            "{error}"
        );
    }

    /// Function and file names come from symbol files and module names from event files:
    /// human output passes them through `plain` like every other field.
    #[test]
    fn symbolicated_frames_print_one_plain_line_each() {
        let symbolicated = json!({
            "thread": "main", "message": "boom\u{1b}[2J", "location": "src/lib.rs:42",
            "frames": [
                { "module": "bibcode", "offset": 0x1010, "function": "my_fn\u{9b}", "file": "src/lib.rs", "line": 42 },
                { "module": "libc.so.6\u{202e}", "offset": 0x29d90, "function": null, "file": null, "line": null },
            ],
        });
        let mut out = Vec::new();

        write_symbolicated(&mut out, &symbolicated).unwrap();

        assert_eq!(
            String::from_utf8(out).unwrap(),
            "Panic on thread main: boom?[2J at src/lib.rs:42\n  0 bibcode!my_fn? [src/lib.rs:42]\n  1 libc.so.6?+0x29d90\n"
        );
        let mut out = Vec::new();
        write_symbolicated(&mut out, &json!({ "crashReason": "SIGSEGV / SEGV_MAPERR", "crashingThread": { "frames": [{ "module": "crash-smoke", "offset": 16 }] } })).unwrap();
        let printed = String::from_utf8(out).unwrap();
        assert!(
            printed.starts_with("Crash: SIGSEGV / SEGV_MAPERR\n  0 crash-smoke+0x10\n"),
            "{printed}"
        );
        assert!(
            printed.contains("No symbols named these frames"),
            "{printed}"
        );
    }

    #[test]
    fn output_is_one_json_document_or_short_readable_lines() {
        let state = tempfile::tempdir().unwrap();
        let dir = state.path().join("profiler");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("events-000000.ndjson"),
            concat!(
                r#"{"v":1,"seq":0,"sid":"s","eventId":"0000000a-0","ts":"2026-10-09T10:00:00Z","kind":"rpc.slow","source":"server","method":"x\u001b[31m","ms":1200}"#,
                "\n",
                r#"{"v":1,"seq":1,"sid":"s","ts":"2026-10-09T10:00:01Z","kind":"process.sample","source":"server"}"#,
                "\n",
            ),
        )
        .unwrap();
        fs::write(
            dir.join("findings.ndjson"),
            concat!(
                r#"{"id":"rpc.slow-aaaaaaaaaaaa","fingerprint":"aaaaaaaaaaaa","kind":"rpc.slow","severity":"warn","firstSeen":"2026-10-09T10:00:00Z","lastSeen":"2026-10-09T10:00:00Z","count":3,"source":"server","title":"Slow \u001b[2Jrequest","summary":"s","state":"open","evidence":{}}"#,
                "\n"
            ),
        )
        .unwrap();

        let events: Value = serde_json::from_str(&run(&["events", "--json"], &dir)).unwrap();
        assert_eq!(events.as_array().unwrap().len(), 2);
        let status: Value = serde_json::from_str(&run(&["status", "--json"], &dir)).unwrap();
        assert_eq!(status["findings"][0]["kind"], "rpc.slow");

        let human = run(&["events", "--kind", "rpc.slow"], &dir);
        assert_eq!(human.lines().count(), 1);
        assert!(
            human.starts_with("2026-10-09T10:00:00Z server rpc.slow {"),
            "{human}"
        );
        assert!(
            human.contains("\"ms\":1200") && !human.contains('\u{1b}'),
            "{human}"
        );
        assert!(!human.contains("eventId"), "an envelope field: {human}");
        let findings = run(&["findings"], &dir);
        assert_eq!(
            findings,
            "2026-10-09T10:00:00Z warn open rpc.slow x3 Slow ?[2Jrequest\n"
        );
        assert!(run(&["status"], &dir).contains("Profiler setting: off"));
        assert!(run(&["crashes"], &dir).starts_with("No crash records."));
        assert_eq!(run(&["crashes", "--json"], &dir), "[]\n");
    }

    /// serde_json escapes only U+0000 to U+001F, so DEL and the C1 controls (CSI is U+009B, OSC
    /// is U+009D) reach the files raw. Human output must still carry none of them.
    #[test]
    fn human_output_replaces_every_control_character_the_files_carry() {
        // The ones a terminal acts on, and then every control character there is.
        const NAMED: &str = "a\u{1b}b\u{7f}c\u{9b}d\u{9d}e";
        let everything: String = ('\u{0}'..='\u{9f}').filter(|c| c.is_control()).collect();
        assert!(everything.contains('\u{7f}') && everything.contains('\u{9d}'));
        let (_state, dir) = seeded_dir(
            &[
                event_record(
                    0,
                    "rpc.slow",
                    "server",
                    json!({"method": NAMED, "ms": 1200}),
                ),
                event_record(
                    1,
                    &everything,
                    &everything,
                    json!({(everything.clone()): everything.clone()}),
                ),
            ],
            &[
                finding_record("rpc.slow", &format!("Slow {NAMED}"), NAMED),
                finding_record(&everything, &everything, &everything),
            ],
            &[json!({"id": everything, "os": NAMED, (everything.clone()): "v"})],
        );

        let reader = Reader::new(&dir);
        let mut followed = Vec::new();
        for event in reader.events(None, None, 10) {
            write_followed(&mut followed, &event, false, event_line).unwrap();
        }
        for finding in reader.findings(None, None, 10) {
            write_followed(&mut followed, &finding, false, finding_line).unwrap();
        }
        let outputs = [
            run(&["events"], &dir),
            run(&["findings"], &dir),
            run(&["status"], &dir),
            run(&["crashes"], &dir),
            String::from_utf8(followed).unwrap(),
        ];

        for output in &outputs {
            for raw in ['\u{1b}', '\u{7f}', '\u{9b}', '\u{9d}'] {
                assert!(
                    !output.contains(raw),
                    "{raw:?} reached the terminal: {output:?}"
                );
            }
            assert!(
                output.chars().all(|c| c == '\n' || !c.is_control()),
                "a control character reached the terminal: {output:?}"
            );
        }
        let [events, findings, status, crashes, _] = &outputs;
        // serde_json already wrote ESC as the six characters `\u001b`, which do nothing; DEL, CSI
        // and OSC are what `plain` replaced.
        assert!(
            events.contains(
                "2026-10-09T10:00:00Z server rpc.slow {\"method\":\"a\\u001bb?c?d?e\",\"ms\":1200}\n"
            ),
            "{events}"
        );
        assert!(
            findings.contains(" rpc.slow x3 Slow a?b?c?d?e\n"),
            "{findings}"
        );
        assert!(status.contains("  2026-10-09T10:00:00Z warn open rpc.slow x3 Slow a?b?c?d?e\n"));
        assert!(crashes.contains(" os=a?b?c?d?e"), "{crashes}");
        // Machine output keeps the data as it is.
        let events: Value = serde_json::from_str(&run(&["events", "--json"], &dir)).unwrap();
        assert_eq!(events[0]["method"], NAMED);
        assert_eq!(events[1]["kind"], everything.as_str());
        let findings: Value = serde_json::from_str(&run(&["findings", "--json"], &dir)).unwrap();
        assert!(
            findings
                .as_array()
                .unwrap()
                .iter()
                .any(|f| f["summary"] == NAMED)
        );
    }

    /// Invisible characters that reorder or hide the text around them print as `?` too, and
    /// ordinary non-ASCII text is left alone.
    #[test]
    fn human_output_replaces_format_and_bidi_characters() {
        let hidden: String = [
            '\u{ad}'..='\u{ad}',
            '\u{200b}'..='\u{200f}',
            '\u{202a}'..='\u{202e}',
            '\u{2066}'..='\u{2069}',
            '\u{feff}'..='\u{feff}',
        ]
        .into_iter()
        .flatten()
        .collect();
        const TEXT: &str = "Cafe\u{301} \u{65e5}\u{672c}\u{8a9e} \u{1f642}";
        let marks = "?".repeat(hidden.chars().count());
        let (_state, dir) = seeded_dir(
            &[event_record(
                0,
                "rpc.slow",
                "server",
                json!({"note": format!("a{hidden}b"), "text": TEXT, (hidden.clone()): "v"}),
            )],
            &[finding_record(
                "rpc.slow",
                &format!("Slow \u{202e}request\u{2066} {TEXT}"),
                "s",
            )],
            &[json!({"id": "crash-0", "os": format!("x{hidden}y")})],
        );

        let outputs =
            ["events", "findings", "status", "crashes"].map(|command| run(&[command], &dir));

        for output in &outputs {
            for c in hidden.chars() {
                assert!(
                    !output.contains(c),
                    "U+{:04X} reached the output: {output:?}",
                    u32::from(c)
                );
            }
        }
        let [events, findings, status, crashes] = &outputs;
        let title = format!("rpc.slow x3 Slow ?request? {TEXT}\n");
        assert!(findings.contains(&title), "{findings}");
        assert!(status.contains(&title), "{status}");
        assert!(
            events.contains(&format!("\"note\":\"a{marks}b\"")),
            "{events}"
        );
        assert!(events.contains(&format!("\"text\":\"{TEXT}\"")), "{events}");
        assert!(crashes.contains(&format!(" os=x{marks}y")), "{crashes}");
    }

    /// serde_json escapes only U+0000 to U+001F, so DEL and the C1 controls would reach a
    /// terminal in `--json` output as they are. They are written as `\u007f`, `\u009b` and so
    /// on instead, and a parser reads back the same values.
    #[test]
    fn json_output_escapes_del_and_c1_controls_and_reads_back_identically() {
        const NAMED: &str = "a\u{1b}b\u{7f}c\u{9b}d\u{9d}e";
        let everything: String = ('\u{0}'..='\u{9f}').filter(|c| c.is_control()).collect();
        let event = event_record(
            0,
            "rpc.slow",
            "server",
            json!({"method": NAMED, (everything.clone()): everything.clone()}),
        );
        let finding = finding_record("rpc.slow", NAMED, &everything);
        let crash = json!({"id": "crash-0", "os": NAMED, (everything.clone()): NAMED});
        let (_state, dir) = seeded_dir(
            std::slice::from_ref(&event),
            std::slice::from_ref(&finding),
            std::slice::from_ref(&crash),
        );
        let on_disk = fs::read_to_string(dir.join("events-000000.ndjson")).unwrap();
        assert!(
            on_disk.contains('\u{7f}') && on_disk.contains('\u{9b}') && on_disk.contains('\u{9d}'),
            "the files must carry the raw characters for this test to mean anything"
        );
        let reader = Reader::new(&dir);

        for (args, expected) in [
            (&["events", "--json"][..], json!([event])),
            (&["findings", "--json"][..], json!([finding])),
            (&["crashes", "--json"][..], json!([crash])),
            (&["status", "--json"][..], reader.status()),
        ] {
            let mut out = Vec::new();
            execute(parse(args).unwrap(), &reader, &mut out).unwrap();

            assert_no_raw_del_or_c1(&out, &format!("{args:?}"));
            let text = String::from_utf8(out).unwrap();
            for escape in ["\\u007f", "\\u009b", "\\u009d"] {
                assert!(text.contains(escape), "{args:?}: no {escape} in {text:?}");
            }
            assert_eq!(
                serde_json::from_str::<Value>(&text).unwrap(),
                expected,
                "{args:?}"
            );
        }
    }

    /// Unicode Tag characters and the format (Cf) characters are invisible to a person but read
    /// by a model, so every JSON reader gets them escaped; a parser reads back the same values.
    #[test]
    fn json_output_escapes_tag_and_format_characters() {
        let hidden = "a\u{e0041}\u{e007f}b\u{202e}c\u{200b}d\u{2066}e\u{feff}f\u{ad}g";
        let mut out = Vec::new();
        write_json(&mut out, &json!({ "title": hidden, (hidden): 1 })).unwrap();
        let text = String::from_utf8(out).unwrap();
        for escape in [
            "\\udb40\\udc41",
            "\\udb40\\udc7f",
            "\\u202e",
            "\\u200b",
            "\\u2066",
            "\\ufeff",
            "\\u00ad",
        ] {
            assert!(text.contains(escape), "no {escape} in {text}");
        }
        assert!(
            text.chars()
                .all(|c| !is_hidden_format(c) && !c.is_control() || c == '\n'),
            "{text:?}"
        );
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap(),
            json!({ "title": hidden, (hidden): 1 })
        );
    }

    #[test]
    fn follow_prints_each_new_record_as_a_json_line() {
        let state = tempfile::tempdir().unwrap();
        let dir = state.path().join("profiler");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("events-000000.ndjson");
        fs::write(&path, "{\"kind\":\"old\"}\n").unwrap();
        // The first new record carries DEL, CSI and OSC as the writer would store them.
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
            file.write_all(
                "{\"kind\":\"a\",\"note\":\"x\u{7f}\u{9b}\u{9d}y\"}\n{\"kind\":\"b\"}\n".as_bytes(),
            )
            .unwrap();
        });
        let mut out = Vec::new();

        execute(
            ProfilerSubcommand::Events {
                query: QueryArgs {
                    since: None,
                    kind: None,
                    follow: true,
                    duration: Some(Duration::from_millis(300)),
                    json: true,
                },
                limit: 200,
            },
            &Reader::new(&dir),
            &mut out,
        )
        .unwrap();
        writer.join().unwrap();

        assert_no_raw_del_or_c1(&out, "events --follow --json");
        let lines: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            lines,
            [
                json!({"kind": "a", "note": "x\u{7f}\u{9b}\u{9d}y"}),
                json!({"kind": "b"})
            ]
        );
    }

    #[test]
    fn bytes_read_in_the_largest_whole_unit() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(256 * 1024 * 1024), "256.0 MiB");
    }
}
