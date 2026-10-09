use std::{
    collections::BTreeMap,
    io::{Cursor, Read, Write},
    ops::Add,
    process::Command,
    time::{Duration, SystemTime},
};

use bibcode_server::diagnostic_bundle::{DiagnosticBundleService, MAX_DIAGNOSTIC_ARCHIVE_BYTES};
use tempfile::TempDir;
use time::OffsetDateTime;
use zip::{CompressionMethod, ZipArchive, ZipWriter, write::SimpleFileOptions};

fn generated_at() -> OffsetDateTime {
    OffsetDateTime::parse(
        "2026-07-15T12:34:56Z",
        &time::format_description::well_known::Rfc3339,
    )
    .expect("fixed generated-at timestamp")
}

fn validated_zip_entries(bytes: &[u8]) -> Result<(Vec<String>, BTreeMap<String, String>), String> {
    let mut archive =
        ZipArchive::new(Cursor::new(bytes)).map_err(|error| format!("readable zip: {error}"))?;
    let mut names = Vec::new();
    let mut entries = BTreeMap::new();
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("zip entry {index}: {error}"))?;
        let name = entry.name().to_owned();
        let enclosed = entry
            .enclosed_name()
            .ok_or_else(|| format!("unsafe diagnostic archive entry: {name}"))?;
        if enclosed.components().count() != 1 {
            return Err(format!(
                "diagnostic archive entry is not at the root: {name}"
            ));
        }
        let mut contents = String::new();
        entry
            .read_to_string(&mut contents)
            .map_err(|error| format!("UTF-8 log entry {name}: {error}"))?;
        names.push(name.clone());
        entries.insert(name, contents);
    }
    Ok((names, entries))
}

fn zip_entries(bytes: &[u8]) -> BTreeMap<String, String> {
    validated_zip_entries(bytes).expect("safe diagnostic zip").1
}

#[tokio::test]
async fn bundle_contains_exact_root_entries_and_oldest_first_server_rotations() {
    let temp = TempDir::new().expect("temporary logs directory");
    for (name, contents) in [
        ("server.log.3", "oldest\n"),
        ("server.log.2", "older\n"),
        ("server.log.1", "recent\n"),
        ("server.log", "active\n"),
        (
            "server.trace.ndjson",
            "{\"name\":\"filesystem.browse\",\"exit\":{\"_tag\":\"Failure\"}}\n",
        ),
    ] {
        std::fs::write(temp.path().join(name), contents).expect("write retained log");
    }
    let service = DiagnosticBundleService::new(temp.path());

    let bundle = service
        .build("frontend warning\n".to_owned(), false, generated_at())
        .await
        .expect("diagnostic bundle");

    assert_eq!(bundle.filename, "bibcode-diagnostics-20260715T123456Z.zip");
    let (names, entries) = validated_zip_entries(&bundle.bytes).expect("safe diagnostic zip");
    assert_eq!(
        names,
        vec!["server.log", "server.trace.ndjson", "frontend.log"]
    );
    let server = entries.get("server.log").expect("server log entry");
    let positions = ["oldest", "older", "recent", "active"].map(|text| {
        server
            .find(text)
            .unwrap_or_else(|| panic!("missing {text}"))
    });
    assert!(positions.windows(2).all(|window| window[0] < window[1]));
    assert!(server.contains("===== server.log.3 ====="));
    assert!(
        entries
            .get("server.trace.ndjson")
            .unwrap()
            .contains("filesystem.browse")
    );
    assert_eq!(entries.get("frontend.log").unwrap(), "frontend warning\n");
}

#[tokio::test]
async fn bundle_uses_explanatory_placeholders_when_logs_are_empty() {
    let temp = TempDir::new().expect("temporary logs directory");
    std::fs::write(temp.path().join("server.log"), "").expect("write empty active log");
    let service = DiagnosticBundleService::new(temp.path());

    let bundle = service
        .build(String::new(), false, generated_at())
        .await
        .expect("diagnostic bundle");
    let entries = zip_entries(&bundle.bytes);

    assert_eq!(
        entries.get("server.log").unwrap(),
        "No retained server logs were found.\n"
    );
    assert_eq!(
        entries.get("frontend.log").unwrap(),
        "No frontend warnings or errors were captured.\n"
    );
}

#[tokio::test]
async fn bundle_redacts_credentials_in_both_trust_boundary_inputs() {
    let temp = TempDir::new().expect("temporary logs directory");
    std::fs::write(
        temp.path().join("server.log"),
        "Authorization: Bearer server-secret\n",
    )
    .expect("write server log");
    std::fs::write(
        temp.path().join("server.trace.ndjson"),
        "{\"name\":\"trace\",\"authorization\":\"Bearer trace-secret\"}\n",
    )
    .expect("write server trace");
    let service = DiagnosticBundleService::new(temp.path());

    let bundle = service
        .build(
            "request api_key=frontend-secret\nurl=https://user:password@example.com/path\n"
                .to_owned(),
            false,
            generated_at(),
        )
        .await
        .expect("diagnostic bundle");
    let entries = zip_entries(&bundle.bytes);
    let combined = entries.values().cloned().collect::<String>();

    assert!(combined.contains("[REDACTED]"));
    assert!(!combined.contains("server-secret"));
    assert!(!combined.contains("trace-secret"));
    assert!(!combined.contains("frontend-secret"));
    assert!(!combined.contains("user:password"));
}

#[tokio::test]
async fn reads_the_zip4_legacy_fixture_with_redaction_and_placeholders() {
    let bytes = include_bytes!("fixtures/diagnostic-bundle-v4.zip");
    let (names, entries) = validated_zip_entries(bytes).expect("read Zip 4 fixture");

    assert_eq!(
        names,
        vec!["server.log", "server.trace.ndjson", "frontend.log"]
    );
    assert!(entries["server.log"].contains("[REDACTED]"));
    assert!(!entries["server.log"].contains("legacy-secret"));
    assert_eq!(
        entries["server.trace.ndjson"],
        "No retained server trace records were found.\n"
    );
    assert_eq!(
        entries["frontend.log"],
        "No frontend warnings or errors were captured.\n"
    );
}

#[test]
fn rejects_path_traversal_entry_names() {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    writer
        .start_file("../outside.log", options)
        .expect("write malicious fixture entry");
    writer.write_all(b"must not escape").expect("write entry");
    let bytes = writer.finish().expect("finish fixture").into_inner();

    assert_eq!(
        validated_zip_entries(&bytes).expect_err("traversal must be rejected"),
        "unsafe diagnostic archive entry: ../outside.log"
    );
}

#[tokio::test]
async fn current_bundle_is_listable_by_the_native_archive_tool() {
    let temp = TempDir::new().expect("temporary logs directory");
    let bundle = DiagnosticBundleService::new(temp.path())
        .build(String::new(), false, generated_at())
        .await
        .expect("diagnostic bundle");
    let archive_path = temp.path().join(&bundle.filename);
    std::fs::write(&archive_path, bundle.bytes).expect("write diagnostic archive");

    let output = if cfg!(windows) {
        Command::new("tar")
            .args(["-tf", archive_path.to_str().expect("UTF-8 archive path")])
            .output()
            .expect("Windows tar should list Zip archives")
    } else {
        Command::new("unzip")
            .args(["-t", archive_path.to_str().expect("UTF-8 archive path")])
            .output()
            .expect("unzip should validate Zip archives")
    };
    assert!(
        output.status.success(),
        "native archive validation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let listing = String::from_utf8_lossy(&output.stdout);
    for entry in ["server.log", "server.trace.ndjson", "frontend.log"] {
        assert!(listing.contains(entry), "missing {entry} in {listing}");
    }
}

/// Every entry of a bundle with a profiler directory, by archive name. Names must stay
/// inside the archive: either at the root or under `profiler/`.
fn profiler_zip_entries(bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let mut archive = ZipArchive::new(Cursor::new(bytes)).expect("readable zip");
    let mut entries = BTreeMap::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).expect("zip entry");
        let name = entry.name().to_owned();
        let enclosed = entry
            .enclosed_name()
            .unwrap_or_else(|| panic!("unsafe diagnostic archive entry: {name}"));
        let first = enclosed.components().next().expect("entry name");
        assert!(
            enclosed.components().count() == 1 || first.as_os_str() == "profiler",
            "entry outside the root and profiler/: {name}"
        );
        let mut contents = Vec::new();
        entry.read_to_end(&mut contents).expect("entry contents");
        entries.insert(name, contents);
    }
    entries
}

fn write_profiler_file(dir: &std::path::Path, relative: &str, contents: &[u8]) {
    let path = dir.join(relative);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("profiler directory");
    std::fs::write(path, contents).expect("profiler file");
}

fn set_age(dir: &std::path::Path, relative: &str, seconds: u64) {
    std::fs::OpenOptions::new()
        .write(true)
        .open(dir.join(relative))
        .expect("open for mtime")
        .set_modified(SystemTime::now() - Duration::from_secs(seconds))
        .expect("set mtime");
}

fn service_with_profiler(logs: &TempDir, profiler: &TempDir) -> DiagnosticBundleService {
    DiagnosticBundleService::new(logs.path()).with_profiler_dir(profiler.path())
}

#[tokio::test]
async fn bundle_adds_profiler_files_and_leaves_crash_dumps_out_by_default() {
    let logs = TempDir::new().expect("logs directory");
    let profiler = TempDir::new().expect("profiler directory");
    write_profiler_file(
        profiler.path(),
        "events-000001.ndjson",
        b"{\"kind\":\"x\"}\n",
    );
    write_profiler_file(profiler.path(), "findings.ndjson", b"{\"id\":\"f\"}\n");
    write_profiler_file(profiler.path(), "crashes/crash-1.json", b"{\"signal\":11}");
    write_profiler_file(
        profiler.path(),
        "crashes/crash-1.dmp",
        &[0x4d, 0x44, 0x4d, 0x50, 0],
    );
    write_profiler_file(profiler.path(), "crashes/UPPER.DMP", &[1, 2, 3]);

    let bundle = service_with_profiler(&logs, &profiler)
        .build(String::new(), false, generated_at())
        .await
        .expect("diagnostic bundle");
    let entries = profiler_zip_entries(&bundle.bytes);

    let names: Vec<&str> = entries.keys().map(String::as_str).collect();
    assert_eq!(
        names,
        vec![
            "frontend.log",
            "profiler/crashes/crash-1.json",
            "profiler/events-000001.ndjson",
            "profiler/findings.ndjson",
            "server.log",
            "server.trace.ndjson",
        ]
    );
    assert_eq!(entries["profiler/findings.ndjson"], b"{\"id\":\"f\"}\n");
}

#[tokio::test]
async fn bundle_carries_crash_dumps_byte_for_byte_when_requested() {
    let logs = TempDir::new().expect("logs directory");
    let profiler = TempDir::new().expect("profiler directory");
    // Invalid UTF-8 and a NUL: a lossy text round trip would change these bytes.
    let dump = vec![0x4d, 0x44, 0x4d, 0x50, 0x00, 0xff, 0xfe, 0x80, 0x93];
    write_profiler_file(profiler.path(), "crashes/crash-1.dmp", &dump);

    let bundle = service_with_profiler(&logs, &profiler)
        .build(String::new(), true, generated_at())
        .await
        .expect("diagnostic bundle");
    let entries = profiler_zip_entries(&bundle.bytes);

    assert_eq!(entries["profiler/crashes/crash-1.dmp"], dump);
}

#[tokio::test]
async fn bundle_redacts_profiler_text_but_not_dumps() {
    let logs = TempDir::new().expect("logs directory");
    let profiler = TempDir::new().expect("profiler directory");
    write_profiler_file(
        profiler.path(),
        "events-000001.ndjson",
        b"Authorization: Bearer profiler-secret\n",
    );

    let bundle = service_with_profiler(&logs, &profiler)
        .build(String::new(), false, generated_at())
        .await
        .expect("diagnostic bundle");
    let text = String::from_utf8(
        profiler_zip_entries(&bundle.bytes)["profiler/events-000001.ndjson"].clone(),
    )
    .expect("UTF-8 events");

    assert!(text.contains("[REDACTED]"));
    assert!(!text.contains("profiler-secret"));
}

#[tokio::test]
async fn bundle_without_profiler_data_has_no_profiler_entries() {
    let logs = TempDir::new().expect("logs directory");
    let missing = logs.path().join("never-created");

    let bundle = DiagnosticBundleService::new(logs.path())
        .with_profiler_dir(&missing)
        .build(String::new(), true, generated_at())
        .await
        .expect("diagnostic bundle");

    assert_eq!(
        validated_zip_entries(&bundle.bytes)
            .expect("root entries only")
            .0,
        vec!["server.log", "server.trace.ndjson", "frontend.log"]
    );
}

#[tokio::test]
async fn bundle_keeps_the_newest_profiler_files_within_the_size_limit() {
    let logs = TempDir::new().expect("logs directory");
    let profiler = TempDir::new().expect("profiler directory");
    let mib = 1024 * 1024;
    // Four 6 MiB events files against a 16 MiB budget: two fit, the third is cut, and
    // the oldest is left out.
    for (name, age) in [
        ("events-000001.ndjson", 400),
        ("events-000002.ndjson", 300),
        ("events-000003.ndjson", 200),
        ("events-000004.ndjson", 100),
    ] {
        // 1 KiB lines: redaction keeps a file that already ends in a newline as it is.
        write_profiler_file(
            profiler.path(),
            name,
            &"a".repeat(1023).add("\n").repeat(6 * 1024).into_bytes(),
        );
        set_age(profiler.path(), name, age);
    }

    let bundle = service_with_profiler(&logs, &profiler)
        .build(String::new(), false, generated_at())
        .await
        .expect("diagnostic bundle");
    let entries = profiler_zip_entries(&bundle.bytes);

    assert_eq!(entries["profiler/events-000004.ndjson"].len(), 6 * mib);
    assert_eq!(entries["profiler/events-000003.ndjson"].len(), 6 * mib);
    let cut = &entries["profiler/events-000002.ndjson"];
    assert!(cut.ends_with(b"\n[truncated]\n"));
    assert!(!entries.contains_key("profiler/events-000001.ndjson"));
    let note = String::from_utf8(entries["profiler/OMITTED.txt"].clone()).expect("UTF-8 note");
    assert!(note.contains("profiler/events-000001.ndjson"));
}

#[tokio::test]
async fn bundle_lists_a_crash_dump_that_does_not_fit_instead_of_cutting_it() {
    let logs = TempDir::new().expect("logs directory");
    let profiler = TempDir::new().expect("profiler directory");
    let mib = 1024 * 1024;
    write_profiler_file(profiler.path(), "crashes/huge.dmp", &vec![7u8; 17 * mib]);
    write_profiler_file(profiler.path(), "findings.ndjson", b"{}\n");

    let bundle = service_with_profiler(&logs, &profiler)
        .build(String::new(), true, generated_at())
        .await
        .expect("diagnostic bundle");
    let entries = profiler_zip_entries(&bundle.bytes);

    assert!(!entries.contains_key("profiler/crashes/huge.dmp"));
    assert!(entries.contains_key("profiler/findings.ndjson"));
    let note = String::from_utf8(entries["profiler/OMITTED.txt"].clone()).expect("UTF-8 note");
    assert!(note.contains("profiler/crashes/huge.dmp"));
}

/// Printable pseudo-random text: it compresses to roughly three quarters of its size.
fn noisy_text(len: usize) -> Vec<u8> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut state = 0x2545_f491_4f6c_dd1d_u64;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        for shift in 0..8 {
            if out.len() < len {
                out.push(ALPHABET[((state >> (shift * 6)) & 63) as usize]);
            }
        }
    }
    out
}

#[tokio::test]
async fn bundle_leaves_a_dump_out_when_the_logs_already_fill_the_archive() {
    let logs = TempDir::new().expect("logs directory");
    let profiler = TempDir::new().expect("profiler directory");
    let mib = 1024 * 1024;
    // Four 4 MiB logs of noisy text come to about 12 MiB of archive on their own.
    for name in ["server.log", "server.log.1", "server.log.2", "server.log.3"] {
        std::fs::write(logs.path().join(name), noisy_text(4 * mib - 1)).expect("noisy log");
    }
    write_profiler_file(profiler.path(), "crashes/big.dmp", &noisy_text(10 * mib));
    write_profiler_file(profiler.path(), "crashes/small.dmp", &noisy_text(2 * mib));
    set_age(profiler.path(), "crashes/big.dmp", 100);
    set_age(profiler.path(), "crashes/small.dmp", 200);

    let bundle = service_with_profiler(&logs, &profiler)
        .build(String::new(), true, generated_at())
        .await
        .expect("diagnostic bundle");
    let entries = profiler_zip_entries(&bundle.bytes);

    assert!(bundle.bytes.len() <= MAX_DIAGNOSTIC_ARCHIVE_BYTES);
    assert!(!entries.contains_key("profiler/crashes/big.dmp"));
    assert!(entries.contains_key("profiler/crashes/small.dmp"));
    let note = String::from_utf8(entries["profiler/OMITTED.txt"].clone()).expect("UTF-8 note");
    assert!(note.contains("profiler/crashes/big.dmp"));
}

#[cfg(unix)]
#[tokio::test]
async fn bundle_skips_a_profiler_directory_that_is_a_link() {
    let logs = TempDir::new().expect("logs directory");
    let state = TempDir::new().expect("state directory");
    let outside = TempDir::new().expect("outside directory");
    std::fs::write(
        outside.path().join("events-000001.ndjson"),
        "outside-secret\n",
    )
    .expect("outside file");
    let linked = state.path().join("profiler");
    std::os::unix::fs::symlink(outside.path(), &linked).expect("symlink");

    let bundle = DiagnosticBundleService::new(logs.path())
        .with_profiler_dir(&linked)
        .build(String::new(), true, generated_at())
        .await
        .expect("diagnostic bundle");

    assert_eq!(
        validated_zip_entries(&bundle.bytes)
            .expect("root entries only")
            .0,
        vec!["server.log", "server.trace.ndjson", "frontend.log"]
    );
}

#[cfg(unix)]
#[tokio::test]
async fn bundle_skips_a_crashes_directory_that_is_a_link() {
    let logs = TempDir::new().expect("logs directory");
    let profiler = TempDir::new().expect("profiler directory");
    let outside = TempDir::new().expect("outside directory");
    std::fs::write(outside.path().join("secret.json"), "outside-secret").expect("outside file");
    std::os::unix::fs::symlink(outside.path(), profiler.path().join("crashes")).expect("symlink");
    write_profiler_file(profiler.path(), "findings.ndjson", b"{}\n");

    let bundle = service_with_profiler(&logs, &profiler)
        .build(String::new(), true, generated_at())
        .await
        .expect("diagnostic bundle");
    let entries = profiler_zip_entries(&bundle.bytes);

    assert!(entries.contains_key("profiler/findings.ndjson"));
    assert!(
        !entries
            .keys()
            .any(|name| name.starts_with("profiler/crashes"))
    );
}

#[cfg(unix)]
#[tokio::test]
async fn bundle_never_follows_links_out_of_the_profiler_directory() {
    let logs = TempDir::new().expect("logs directory");
    let profiler = TempDir::new().expect("profiler directory");
    let outside = TempDir::new().expect("outside directory");
    std::fs::write(outside.path().join("secret.txt"), "outside-secret").expect("outside file");
    std::os::unix::fs::symlink(
        outside.path().join("secret.txt"),
        profiler.path().join("linked.ndjson"),
    )
    .expect("symlink");

    let bundle = service_with_profiler(&logs, &profiler)
        .build(String::new(), true, generated_at())
        .await
        .expect("diagnostic bundle");
    let entries = profiler_zip_entries(&bundle.bytes);

    assert!(!entries.keys().any(|name| name.starts_with("profiler/")));
}
