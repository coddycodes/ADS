//! Records every panic as a `panic` event, even while the profiler is off: a panic is rare
//! and cheap to record. Each frame is a `(module, debugId, offset)` triple, Breakpad's own
//! addressing, so the `.sym` files that symbolicate minidumps symbolicate panics too.
//!
//! Each panic also becomes a `crash.panic` finding, but never inside the hook: a thread can
//! panic while it holds the findings lock. The hook only `try_send`s a [`PanicRecord`] to the
//! runtime, whose task records the finding.

use std::{
    sync::{Once, OnceLock},
    time::Duration,
};

use serde_json::{Value, json};

use super::{
    cite,
    event::ProfilerEvent,
    findings::{FindingInput, Severity},
    global,
    redact::{MESSAGE_MAX_BYTES, redact_path},
};

const MAX_FRAMES: usize = 64;
const REDACTED: &str = "[redacted]";
/// How long a panicking thread waits for its record to reach the OS.
const FLUSH_TIMEOUT: Duration = Duration::from_millis(250);
/// How many records may wait for the runtime's task; more panics than that are events only.
pub(super) const PANIC_QUEUE: usize = 32;
/// Frames of this executable in a `crash.panic` fingerprint.
const FINGERPRINT_FRAMES: usize = 3;

/// What the hook hands the runtime to raise a `crash.panic` finding from.
#[derive(Debug)]
pub(super) struct PanicRecord {
    /// The thread's name class: digits dropped, so a pool's threads are one class.
    thread: String,
    /// `file:line` as compiled in: the same for every user of a build, so a fingerprint
    /// field. A dependency's path is the builder's, never this user's.
    site: Option<String>,
    /// `site` with this user's home directory as `~`, for the title.
    location: Option<String>,
    message: String,
    /// Offsets of the top frames in this executable.
    offsets: Vec<u64>,
    /// The `panic` event's `eventId`.
    event_id: Option<super::store::EventId>,
}

/// Installs the hook once per process. It records the panic, then runs the hook it replaced.
/// It never aborts, so `catch_unwind` and `JoinError::is_panic` recovery keep working. The
/// first call reads the executable's debug ID and resolves the home directory, so the hook
/// never waits on a slow home mount; call it off the async workers.
pub fn install_panic_hook() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = main_module();
        let _ = redact_path("");
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            record(info);
            previous(info);
        }));
    });
}

/// Sends straight to the store, not through `emit`, which drops events while the profiler is
/// off. Before a runtime starts, or while it is degraded, there is nowhere to record it.
fn record(info: &std::panic::PanicHookInfo<'_>) {
    if !records_panics() {
        return;
    }
    let Some(handle) = global() else {
        return;
    };
    let mut frames = Vec::with_capacity(MAX_FRAMES);
    let mut walked = 0;
    backtrace::trace(|frame| {
        walked += 1;
        frames.extend(module_offset(frame));
        walked < MAX_FRAMES
    });
    let message = panic_message(info.payload_as_str().unwrap_or("Box<dyn Any>"));
    let thread = std::thread::current().name().map(str::to_owned);
    let site = info
        .location()
        .map(|location| format!("{}:{}", location.file(), location.line()));
    // A dependency's location is a path under the builder's home directory.
    let location = site.as_deref().map(redact_path);
    let offsets = own_offsets(&frames);
    let event_id = handle.sender.send(ProfilerEvent {
        kind: "panic",
        source: "server".into(),
        payload: json!({
            "thread": thread.as_deref().unwrap_or("unnamed"),
            "message": message,
            "location": location,
            "frames": frames,
            // The build that made the frames, named as crash sidecars name it: symbolication
            // fetches that release's symbols, whatever build reads the event later.
            "version": env!("CARGO_PKG_VERSION"),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
        }),
    });
    // Never blocks: a full queue drops the record, and the event above still tells.
    let _ = handle.panics.try_send(PanicRecord {
        thread: thread_class(thread.as_deref()),
        site,
        location,
        message,
        offsets,
        event_id,
    });
    // A panic that cannot unwind aborts right after the hook, before the writer's next timed
    // flush. Panics are rare, so the wait costs nothing in practice.
    handle.sender.flush_within(FLUSH_TIMEOUT);
}

/// Offsets of the top frames in this executable.
// ponytail: the top frames are the hook's and std's panic machinery, the same for every panic
// of a build, so they pin the build and the site tells panics apart. Skipping them needs
// symbols, which shipped builds do not have.
fn own_offsets(frames: &[Value]) -> Vec<u64> {
    let Some(main) = main_module() else {
        return Vec::new();
    };
    frames
        .iter()
        .filter(|frame| frame["module"] == main.name.as_str())
        .filter_map(|frame| frame["offset"].as_u64())
        .take(FINGERPRINT_FRAMES)
        .collect()
}

/// A thread's name without digits, so a pool's threads are one class; Tokio's workers are
/// `tokio-worker`.
fn thread_class(name: Option<&str>) -> String {
    match name {
        None => "unnamed".to_owned(),
        Some("tokio-runtime-worker") => "tokio-worker".to_owned(),
        Some(name) => name.chars().filter(|c| !c.is_ascii_digit()).collect(),
    }
}

/// The `crash.panic` finding for `record`.
pub(super) fn panic_finding(record: PanicRecord) -> FindingInput {
    let mut fingerprint_fields = vec![
        record.thread.clone(),
        record.site.unwrap_or_else(|| "unknown".to_owned()),
    ];
    fingerprint_fields.extend(record.offsets.iter().map(|offset| format!("{offset:#x}")));
    let location = record.location.as_deref().unwrap_or("an unknown location");
    FindingInput {
        kind: "crash.panic",
        severity: Severity::Error,
        source: "server".into(),
        fingerprint_fields,
        title: format!("Panic at {location}"),
        summary: format!(
            "A thread of class {} panicked at {location}: {}",
            record.thread, record.message
        ),
        evidence: cite(
            json!({
                "thread": record.thread,
                "location": record.location,
                "message": record.message,
            }),
            record.event_id,
        ),
    }
}

/// Lib tests share this process-wide hook, and many panic on purpose, so none of their
/// panics is recorded: it would land in whichever test's store is installed. Integration
/// tests, built without `cfg(test)`, record every panic.
fn records_panics() -> bool {
    !cfg!(test)
}

/// Where std puts the value a panic was about, up to the end of the line.
const VALUE_MARKERS: [&str; 6] = [
    "Err` value:",
    "Err value:",
    "Ok` value:",
    "Ok value:",
    "left:",
    "right:",
];

/// The panic's first line, at most 2 KiB, without the data std and `expect` embed in it:
/// strings between backticks or double quotes, `Err` and `Ok` values, assertion operands,
/// and a `Debug` rendering after `: `. That data can be prompt or terminal text. Plain prose
/// such as `failed: connection refused` stays. Truncating first also bounds the work.
fn panic_message(raw: &str) -> String {
    let first = raw.lines().next().unwrap_or("");
    let mut end = first.len().min(MESSAGE_MAX_BYTES);
    while !first.is_char_boundary(end) {
        end -= 1;
    }
    let mut line = &first[..end];
    // A URL cut inside its authority may have lost the `@` that marks its credentials, so
    // credential redaction would no longer see them: the partial URL goes whole.
    let mut url_cut = false;
    if end < first.len()
        && let Some(scheme) = line.rfind("://")
        && !line[scheme + 3..].contains(['/', ' ', '\t'])
    {
        line = &line[..line[..scheme].rfind(char::is_whitespace).unwrap_or(0)];
        url_cut = true;
    }
    let value_at = VALUE_MARKERS
        .iter()
        .filter_map(|marker| line.find(marker).map(|at| at + marker.len()))
        .chain(debug_value_at(line))
        .min();
    let mut rest = value_at.map_or(line, |at| &line[..at]);
    let mut kept = String::with_capacity(line.len());
    while let Some(open) = rest.find(['`', '"']) {
        kept.push_str(&rest[..open]);
        kept.push_str(REDACTED);
        let quoted = &rest[open + 1..];
        rest =
            closing_quote(quoted, rest.as_bytes()[open]).map_or("", |close| &quoted[close + 1..]);
    }
    kept.push_str(rest);
    if value_at.is_some() || url_cut {
        if !kept.is_empty() {
            kept.push(' ');
        }
        kept.push_str(REDACTED);
    }
    redact_path(&kept)
}

/// Just past the `:` of the first `: ` that a `Debug` rendering follows: a string, list or
/// map, or a struct, tuple struct or variant (`Name {`, `Name(`). `expect` puts the error's
/// `Debug` there.
fn debug_value_at(line: &str) -> Option<usize> {
    line.match_indices(": ").find_map(|(at, _)| {
        let tail = &line[at + 2..];
        let name = tail
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(tail.len());
        let debug = tail.starts_with(['"', '[', '{'])
            || (tail.starts_with(|c: char| c.is_ascii_uppercase())
                && (tail[name..].starts_with(" {") || tail[name..].starts_with('(')));
        debug.then_some(at + 1)
    })
}

/// Where the quoted text starting `text` ends. Inside double quotes a backslash escapes the
/// next character, as `Debug` writes it.
fn closing_quote(text: &str, quote: u8) -> Option<usize> {
    let mut escaped = false;
    text.bytes().position(|byte| {
        let close = byte == quote && !escaped;
        escaped = quote == b'"' && byte == b'\\' && !escaped;
        close
    })
}

/// The executable that holds this code: its load address, its name as symbol files name it,
/// and its Breakpad debug ID.
struct MainModule {
    base: usize,
    name: String,
    debug_id: Option<String>,
}

fn main_module() -> Option<&'static MainModule> {
    static MAIN: OnceLock<Option<MainModule>> = OnceLock::new();
    MAIN.get_or_init(load_main_module).as_ref()
}

fn load_main_module() -> Option<MainModule> {
    let exe = std::env::current_exe().ok()?;
    let mut name = file_name(&exe.to_string_lossy()).to_owned();
    let mut debug_id = None;
    // `ReadCache` reads only the headers and the note or debug directory, not the whole file.
    if let Ok(file) = std::fs::File::open(&exe) {
        let cache = object::ReadCache::new(file);
        if let Ok(parsed) = object::File::parse(&cache) {
            use object::Object;
            if let Ok(Some(build_id)) = parsed.build_id() {
                let mut guid = [0; 16];
                let len = build_id.len().min(16);
                guid[..len].copy_from_slice(&build_id[..len]);
                debug_id = Some(format!("{}0", breakpad_guid(guid)));
            } else if let Ok(Some(uuid)) = parsed.mach_uuid() {
                debug_id = Some(format!("{}0", upper_hex(&uuid)));
            } else if let Ok(Some(pdb)) = parsed.pdb_info() {
                // Windows symbol files are named after the PDB.
                name = file_name(&String::from_utf8_lossy(pdb.path())).to_owned();
                debug_id = Some(format!("{}{:x}", breakpad_guid(pdb.guid()), pdb.age()));
            }
        }
    }
    Some(MainModule {
        base: main_base()?,
        name,
        debug_id,
    })
}

/// Breakpad reads an identifier as a GUID whose first three fields are little-endian.
fn breakpad_guid(mut guid: [u8; 16]) -> String {
    guid[..4].reverse();
    guid[4..6].reverse();
    guid[6..8].reverse();
    upper_hex(&guid)
}

fn upper_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02X}")).collect()
}

fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn frame_json(base: usize, ip: usize, other_module: impl FnOnce() -> String) -> Option<Value> {
    let offset = ip.checked_sub(base)?;
    let (module, debug_id) = match main_module() {
        Some(main) if main.base == base => (main.name.clone(), main.debug_id.clone()),
        _ => (other_module(), None),
    };
    Some(json!({ "module": module, "debugId": debug_id, "offset": offset }))
}

#[cfg(unix)]
fn main_base() -> Option<usize> {
    dladdr(main_module as fn() -> Option<&'static MainModule> as *const libc::c_void)
        .map(|info| info.dli_fbase as usize)
}

#[cfg(unix)]
fn dladdr(address: *const libc::c_void) -> Option<libc::Dl_info> {
    // SAFETY: `Dl_info` is plain data, and `dladdr` only reads the loader's tables to fill it.
    let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
    let found = unsafe { libc::dladdr(address, &mut info) } != 0;
    (found && !info.dli_fbase.is_null()).then_some(info)
}

#[cfg(unix)]
fn module_offset(frame: &backtrace::Frame) -> Option<Value> {
    let info = dladdr(frame.ip())?;
    frame_json(info.dli_fbase as usize, frame.ip() as usize, || {
        if info.dli_fname.is_null() {
            return "unknown".to_owned();
        }
        // SAFETY: a non-null `dli_fname` is a NUL-terminated string owned by the loader.
        let path = unsafe { std::ffi::CStr::from_ptr(info.dli_fname) }.to_string_lossy();
        file_name(&path).to_owned()
    })
}

#[cfg(windows)]
fn main_base() -> Option<usize> {
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    // SAFETY: a null name asks for the executable's own module handle.
    let base = unsafe { GetModuleHandleW(std::ptr::null()) };
    (!base.is_null()).then_some(base as usize)
}

#[cfg(windows)]
fn module_offset(frame: &backtrace::Frame) -> Option<Value> {
    let base = frame.module_base_address()?;
    frame_json(base as usize, frame.ip() as usize, || {
        use windows_sys::Win32::System::LibraryLoader::GetModuleFileNameW;
        let mut path = [0u16; 1024];
        // SAFETY: `base` is a loaded module's handle and the buffer's length is passed.
        let len = unsafe { GetModuleFileNameW(base, path.as_mut_ptr(), path.len() as u32) };
        let path = String::from_utf16_lossy(&path[..(len as usize).min(path.len())]);
        file_name(&path).to_owned()
    })
}

#[cfg(not(any(unix, windows)))]
fn main_base() -> Option<usize> {
    None
}

#[cfg(not(any(unix, windows)))]
fn module_offset(_frame: &backtrace::Frame) -> Option<Value> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The message std builds for a panic in `f`, as the hook receives it.
    fn recorded_message(f: impl FnOnce() + std::panic::UnwindSafe) -> String {
        let payload = std::panic::catch_unwind(f).unwrap_err();
        let raw = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied())
            .unwrap();
        panic_message(raw)
    }

    #[test]
    fn slicing_panics_record_no_part_of_the_text() {
        // Index 17 falls inside the two-byte `é`.
        let text = String::from("my secret prompté");
        let end = std::hint::black_box(17);
        let message = recorded_message(|| {
            let _ = &text[..end];
        });
        assert!(
            !message.contains("secret") && !message.contains("prompt"),
            "{message}"
        );
        // Older std quoted up to 256 bytes of the string between backticks.
        assert_eq!(
            panic_message(
                "byte index 17 is not a char boundary; it is inside 'é' (bytes 16..18) of \
                 `my secret prompté`"
            ),
            "byte index 17 is not a char boundary; it is inside 'é' (bytes 16..18) of [redacted]"
        );
    }

    #[test]
    fn assertions_and_unwraps_record_no_values() {
        let (left, right) = (String::from("my secret"), String::from("my prompt"));
        let message = recorded_message(|| assert_eq!(left, right));
        assert!(
            !message.contains("secret") && !message.contains("prompt"),
            "{message}"
        );
        let message = recorded_message(|| {
            std::hint::black_box(Err::<(), _>("my secret prompt")).unwrap();
        });
        assert!(!message.contains("secret"), "{message}");
        assert!(message.ends_with("[redacted]"), "{message}");
    }

    #[test]
    fn only_the_first_line_is_kept_and_capped() {
        let message = panic_message(&format!("{}\nsecond line", "a".repeat(5000)));
        assert_eq!(message, "a".repeat(2048));
        assert_eq!(
            panic_message("unterminated `my secret"),
            "unterminated [redacted]"
        );
    }

    /// `Debug` renderings after `: ` and quoted strings are values; plain prose stays.
    #[test]
    fn debug_values_and_quoted_strings_are_redacted() {
        let secret = "my secret prompt";
        let ok_value = recorded_message(|| {
            std::hint::black_box(Ok::<_, ()>(secret)).unwrap_err();
        });
        let expect_string = recorded_message(|| {
            std::hint::black_box(Err::<(), _>(secret.to_owned())).expect("send");
        });
        let expect_struct = recorded_message(|| {
            String::from_utf8(std::hint::black_box(b"my secret prompt\xff".to_vec()))
                .expect("decode");
        });
        let serde_display =
            panic_message(&format!("invalid type: string \"{secret}\", expected u32"));
        for message in [&ok_value, &expect_string, &expect_struct, &serde_display] {
            assert!(!message.contains(secret), "{message}");
            assert!(!message.contains("109, 121"), "{message}");
            assert!(message.contains(REDACTED), "{message}");
        }
        assert_eq!(expect_string, "send: [redacted]");
        assert_eq!(expect_struct, "decode: [redacted]");
        assert_eq!(
            serde_display,
            "invalid type: string [redacted], expected u32"
        );
        // An escaped quote does not end the string early.
        assert_eq!(
            panic_message(r#"bad "a \"my secret\" b" here"#),
            "bad [redacted] here"
        );
        assert_eq!(
            panic_message("failed: connection refused"),
            "failed: connection refused"
        );
        assert_eq!(panic_message("state: Ready"), "state: Ready");
    }

    /// The cap must not cut a URL between its credentials and the `@` that marks them.
    #[test]
    fn a_url_cut_by_the_cap_is_redacted_whole() {
        let raw = format!(
            "{} https://alice:supersecret@example.test/path",
            "a".repeat(2025)
        );
        let message = panic_message(&raw);
        assert!(
            !message.contains("alice") && !message.contains("supersec"),
            "{message}"
        );
        assert!(message.ends_with("[redacted]"), "{message}");
        let short = panic_message("fetch https://alice:supersecret@example.test/path failed");
        assert!(!short.contains("supersecret"), "{short}");
    }

    fn panic_record(thread: &str, site: &str, offsets: &[u64]) -> PanicRecord {
        PanicRecord {
            thread: thread_class(Some(thread)),
            site: Some(site.to_owned()),
            location: Some(site.to_owned()),
            message: "boom".to_owned(),
            offsets: offsets.to_vec(),
            event_id: Some(super::super::store::EventId::new(0xa1b2_c3d4, 7)),
        }
    }

    #[test]
    fn a_panic_finding_is_keyed_by_thread_class_site_and_own_frames() {
        let fingerprint = |input: &FindingInput| {
            super::super::findings::fingerprint(input.kind, &input.fingerprint_fields)
        };
        let first = panic_finding(panic_record("tokio-runtime-worker", "src/a.rs:7", &[1, 2]));
        assert_eq!(first.kind, "crash.panic");
        assert_eq!(first.severity, Severity::Error);
        assert_eq!(first.title, "Panic at src/a.rs:7");
        assert_eq!(
            first.fingerprint_fields,
            ["tokio-worker", "src/a.rs:7", "0x1", "0x2"]
        );
        assert_eq!(first.evidence["eventIds"], json!(["a1b2c3d4-7"]));
        // Another worker of the same pool is the same panic.
        let same = panic_finding(panic_record("pool-worker-12", "src/a.rs:7", &[1, 2]));
        let also = panic_finding(panic_record("pool-worker-3", "src/a.rs:7", &[1, 2]));
        assert_eq!(fingerprint(&same), fingerprint(&also));
        assert_ne!(
            fingerprint(&first),
            fingerprint(&panic_finding(panic_record(
                "tokio-runtime-worker",
                "src/a.rs:8",
                &[1, 2]
            )))
        );
        assert_eq!(thread_class(None), "unnamed");
    }

    /// The hook redacts paths, and resolving the home directory touches the filesystem: a
    /// hung home mount must stall the install, never a panicking thread. Runs in a fresh
    /// process, where nothing has resolved it yet.
    #[test]
    fn installing_the_hook_resolves_the_home_directory() {
        const CASE: &str = "panic-hook-resolves-home";
        const TEST: &str = "diagnostics::profiler::panic_hook::tests::installing_the_hook_resolves_the_home_directory";
        if crate::test_support::TestSandbox::is_isolated_case(CASE, TEST) {
            assert!(!super::super::redact::homes_resolved());
            install_panic_hook();
            assert!(super::super::redact::homes_resolved());
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
            .env("CODDY_STUDIO_TEST_ISOLATED_CASE", CASE)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{stdout}");
        // A filter that no longer names this test runs nothing, and that passes too.
        assert!(
            stdout.contains("test result: ok. 1 passed;"),
            "the child ran exactly this test: {stdout}"
        );
    }

    #[test]
    fn debug_ids_use_breakpad_byte_order() {
        let build_id: [u8; 16] = std::array::from_fn(|i| i as u8);
        assert_eq!(breakpad_guid(build_id), "030201000504070608090A0B0C0D0E0F");
    }

    #[test]
    fn the_main_module_is_this_executable_with_its_debug_id() {
        let main = main_module().expect("main module");
        let exe = std::env::current_exe().unwrap();
        let exe = exe.file_name().unwrap().to_string_lossy();
        // Windows names a module after its PDB, as its symbol files are.
        assert!(
            main.name == exe || (cfg!(windows) && main.name.ends_with(".pdb")),
            "{}",
            main.name
        );
        if cfg!(target_os = "linux") {
            let id = main.debug_id.as_deref().expect("a GNU build-id");
            assert_eq!(id.len(), 33);
            assert!(id.ends_with('0'));
        }
    }
}
