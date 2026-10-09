//! `bibcode profiler`: findings, events and crash records from the profiler directory, read
//! through [`Reader`]. It never starts a server and works while one is stopped or running.

use std::{
    ffi::OsStr,
    io::{self, Write},
    path::PathBuf,
    time::Duration,
};

use clap::{Args, Parser, Subcommand};
use serde_json::{Map, Value};
use thiserror::Error;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::{
    profiler_dir,
    reader::{MAX_FOLLOW, MAX_LIMIT, Reader},
};

/// Envelope fields of an event line; the rest is its payload.
const ENVELOPE: [&str; 6] = ["v", "seq", "sid", "ts", "kind", "source"];
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
    /// Hidden until symbolication ships.
    #[command(hide = true)]
    Symbolicate {
        target: String,
        #[arg(long)]
        symbols: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Hidden until the MCP server ships.
    #[command(hide = true)]
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
    #[error("`profiler {0}` is not available yet")]
    NotAvailable(&'static str),
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
                writeln!(out, "{status}")?;
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
        ProfilerSubcommand::Symbolicate { .. } => {
            return Err(CliError::NotAvailable("symbolicate"));
        }
        ProfilerSubcommand::Mcp => return Err(CliError::NotAvailable("mcp")),
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
        writeln!(out, "{}", Value::Array(items))
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
        writeln!(out, "{item}")?;
    } else {
        writeln!(out, "{}", line(item))?;
    }
    out.flush()
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
        // Compact JSON escapes control characters, so it is safe to print as it is.
        line.push(' ');
        line.push_str(&clip(Value::Object(payload).to_string(), PAYLOAD_WIDTH));
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

/// A string field with control characters replaced: values come from monitored processes and
/// must not drive the terminal.
fn text(value: &Value, key: &str, missing: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .map_or_else(|| missing.to_owned(), plain)
}

fn plain(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
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

fn parse_since(input: &str) -> Result<OffsetDateTime, String> {
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
    fn symbolicate_and_mcp_parse_but_are_hidden_and_unavailable() {
        use clap::CommandFactory;
        let help = DesktopCli::command().render_help().to_string();
        assert!(
            !help.contains("symbolicate") && !help.contains("mcp"),
            "{help}"
        );
        for args in [&["symbolicate", "abc"][..], &["mcp"][..]] {
            let error = execute(
                parse(args).unwrap(),
                &Reader::new("/nonexistent"),
                &mut Vec::new(),
            )
            .unwrap_err();
            assert!(error.to_string().contains("not available yet"));
        }
    }

    #[test]
    fn output_is_one_json_document_or_short_readable_lines() {
        let state = tempfile::tempdir().unwrap();
        let dir = state.path().join("profiler");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("events-000000.ndjson"),
            concat!(
                r#"{"v":1,"seq":0,"sid":"s","ts":"2026-10-09T10:00:00Z","kind":"rpc.slow","source":"server","method":"x\u001b[31m","ms":1200}"#,
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
        let findings = run(&["findings"], &dir);
        assert_eq!(
            findings,
            "2026-10-09T10:00:00Z warn open rpc.slow x3 Slow ?[2Jrequest\n"
        );
        assert!(run(&["status"], &dir).contains("Profiler setting: off"));
        assert!(run(&["crashes"], &dir).starts_with("No crash records."));
        assert_eq!(run(&["crashes", "--json"], &dir), "[]\n");
    }

    #[test]
    fn follow_prints_each_new_record_as_a_json_line() {
        let state = tempfile::tempdir().unwrap();
        let dir = state.path().join("profiler");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("events-000000.ndjson");
        fs::write(&path, "{\"kind\":\"old\"}\n").unwrap();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            let mut file = fs::OpenOptions::new().append(true).open(path).unwrap();
            file.write_all(b"{\"kind\":\"a\"}\n{\"kind\":\"b\"}\n")
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

        let lines: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines, [json!({"kind": "a"}), json!({"kind": "b"})]);
    }

    #[test]
    fn bytes_read_in_the_largest_whole_unit() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(256 * 1024 * 1024), "256.0 MiB");
    }
}
