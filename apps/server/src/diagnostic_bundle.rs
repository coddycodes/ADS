use std::{
    fs::{self, File},
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};

use thiserror::Error;
use time::OffsetDateTime;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

use crate::{
    diagnostics::redact_sensitive_text,
    logging::{SERVER_LOG_MAX_BYTES, retained_server_log_paths},
};

const FRONTEND_LOG_MAX_BYTES: usize = 512 * 1024;
const SERVER_LOG_ENTRY: &str = "server.log";
const SERVER_TRACE_ENTRY: &str = "server.trace.ndjson";
const FRONTEND_LOG_ENTRY: &str = "frontend.log";
const EMPTY_SERVER_LOG: &str = "No retained server logs were found.\n";
const EMPTY_SERVER_TRACE: &str = "No retained server trace records were found.\n";
const EMPTY_FRONTEND_LOG: &str = "No frontend warnings or errors were captured.\n";
const PROFILER_ENTRY_DIR: &str = "profiler";
const PROFILER_CRASHES_DIR: &str = "crashes";
const PROFILER_OMITTED_ENTRY: &str = "profiler/OMITTED.txt";
/// The largest archive the desktop host will save. Profiler files are only added while the
/// archive, as built so far, leaves room for them.
pub const MAX_DIAGNOSTIC_ARCHIVE_BYTES: usize = 20 * 1024 * 1024;
/// Archive bytes held back for what the compressor has not written out yet, and for the
/// archive's own directory.
const ARCHIVE_SLACK_BYTES: u64 = 1024 * 1024;
/// Uncompressed bytes of profiler files, crash dumps included, one bundle may carry. It
/// bounds the work and memory of a bundle; the archive limit above bounds its size. The
/// newest files are taken first; whatever does not fit is listed in `profiler/OMITTED.txt`.
const PROFILER_BUNDLE_MAX_BYTES: u64 = 4 * SERVER_LOG_MAX_BYTES;
/// One text file is read up to the profiler's rotation size and marked when truncated.
const PROFILER_TEXT_FILE_MAX_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Clone)]
pub struct DiagnosticBundleService {
    logs_dir: PathBuf,
    profiler_dir: Option<PathBuf>,
}

pub struct DiagnosticBundle {
    pub filename: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Error)]
pub enum DiagnosticBundleError {
    #[error("failed to read a retained server log")]
    ReadServerLog(#[source] std::io::Error),
    #[error("failed to construct the diagnostic archive")]
    Zip(#[source] zip::result::ZipError),
    #[error("failed to write diagnostic archive contents")]
    Write(#[source] std::io::Error),
    #[error("diagnostic archive construction task failed")]
    Join(#[source] tokio::task::JoinError),
}

impl DiagnosticBundleService {
    #[must_use]
    pub fn new(logs_dir: impl AsRef<Path>) -> Self {
        Self {
            logs_dir: logs_dir.as_ref().to_path_buf(),
            profiler_dir: None,
        }
    }

    /// Adds the profiler data directory to the bundle, under `profiler/`.
    #[must_use]
    pub fn with_profiler_dir(mut self, profiler_dir: impl AsRef<Path>) -> Self {
        self.profiler_dir = Some(profiler_dir.as_ref().to_path_buf());
        self
    }

    /// Crash dumps can hold fragments of process memory, so they are left out unless the
    /// caller asked for them.
    pub async fn build(
        &self,
        frontend_log: String,
        include_crash_dumps: bool,
        generated_at: OffsetDateTime,
    ) -> Result<DiagnosticBundle, DiagnosticBundleError> {
        let logs_dir = self.logs_dir.clone();
        let profiler_dir = self.profiler_dir.clone();
        tokio::task::spawn_blocking(move || {
            build_bundle(
                &logs_dir,
                profiler_dir.as_deref(),
                &frontend_log,
                include_crash_dumps,
                generated_at,
            )
        })
        .await
        .map_err(DiagnosticBundleError::Join)?
    }
}

fn build_bundle(
    logs_dir: &Path,
    profiler_dir: Option<&Path>,
    frontend_log: &str,
    include_crash_dumps: bool,
    generated_at: OffsetDateTime,
) -> Result<DiagnosticBundle, DiagnosticBundleError> {
    let server_log = collect_server_logs(logs_dir)?;
    let server_trace =
        collect_retained_logs(&logs_dir.join(SERVER_TRACE_ENTRY), EMPTY_SERVER_TRACE)?;
    let mut frontend_log = if frontend_log.trim().is_empty() {
        EMPTY_FRONTEND_LOG.to_owned()
    } else {
        redact_sensitive_text(&bounded_frontend_log(frontend_log))
    };
    if !frontend_log.ends_with('\n') {
        frontend_log.push('\n');
    }
    let cursor = Cursor::new(Vec::new());
    let mut writer = ZipWriter::new(cursor);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o600);
    writer
        .start_file(SERVER_LOG_ENTRY, options)
        .map_err(DiagnosticBundleError::Zip)?;
    writer
        .write_all(server_log.as_bytes())
        .map_err(DiagnosticBundleError::Write)?;
    writer
        .start_file(SERVER_TRACE_ENTRY, options)
        .map_err(DiagnosticBundleError::Zip)?;
    writer
        .write_all(server_trace.as_bytes())
        .map_err(DiagnosticBundleError::Write)?;
    writer
        .start_file(FRONTEND_LOG_ENTRY, options)
        .map_err(DiagnosticBundleError::Zip)?;
    writer
        .write_all(frontend_log.as_bytes())
        .map_err(DiagnosticBundleError::Write)?;
    if let Some(profiler_dir) = profiler_dir {
        write_profiler_entries(&mut writer, options, profiler_dir, include_crash_dumps)?;
    }
    let bytes = writer
        .finish()
        .map_err(DiagnosticBundleError::Zip)?
        .into_inner();
    Ok(DiagnosticBundle {
        filename: format!(
            "coddy-studio-diagnostics-{:04}{:02}{:02}T{:02}{:02}{:02}Z.zip",
            generated_at.year(),
            u8::from(generated_at.month()),
            generated_at.day(),
            generated_at.hour(),
            generated_at.minute(),
            generated_at.second(),
        ),
        bytes,
    })
}

struct ProfilerFile {
    path: PathBuf,
    entry: String,
    len: u64,
    modified: SystemTime,
    is_dump: bool,
}

/// Adds the profiler directory's files, newest first, within `PROFILER_BUNDLE_MAX_BYTES`.
/// A file that cannot be read or does not fit is listed in `profiler/OMITTED.txt` instead
/// of failing the whole bundle. Symbolic links are never followed, not even to a linked
/// profiler or `crashes` directory.
fn write_profiler_entries(
    writer: &mut ZipWriter<Cursor<Vec<u8>>>,
    options: SimpleFileOptions,
    profiler_dir: &Path,
    include_crash_dumps: bool,
) -> Result<(), DiagnosticBundleError> {
    let mut omitted = Vec::new();
    let mut files = Vec::new();
    // `read_dir` follows a link, so a linked directory is skipped rather than listed.
    if is_real_directory(profiler_dir) {
        list_profiler_files(
            profiler_dir,
            PROFILER_ENTRY_DIR,
            include_crash_dumps,
            &mut files,
            &mut omitted,
        );
        let crashes_dir = profiler_dir.join(PROFILER_CRASHES_DIR);
        if is_real_directory(&crashes_dir) {
            list_profiler_files(
                &crashes_dir,
                &format!("{PROFILER_ENTRY_DIR}/{PROFILER_CRASHES_DIR}"),
                include_crash_dumps,
                &mut files,
                &mut omitted,
            );
        }
    }
    files.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then_with(|| a.entry.cmp(&b.entry))
    });

    let mut remaining = PROFILER_BUNDLE_MAX_BYTES;
    for file in files {
        // Room left in the archive, counting a file as if it did not compress at all.
        let archive_room = (MAX_DIAGNOSTIC_ARCHIVE_BYTES as u64)
            .saturating_sub(ARCHIVE_SLACK_BYTES)
            .saturating_sub(
                writer
                    .get_ref()
                    .map_or(0, |archive| archive.get_ref().len()) as u64,
            );
        let budget = remaining.min(archive_room);
        // A dump is useless cut short, so it is bundled whole or not at all.
        let fits = if file.is_dump {
            file.len <= budget
        } else {
            budget > 0
        };
        if !fits {
            omitted.push(format!("{} (over the bundle size limit)", file.entry));
            continue;
        }
        let limit = if file.is_dump {
            file.len
        } else {
            budget.min(PROFILER_TEXT_FILE_MAX_BYTES)
        };
        let Some(contents) = read_profiler_file(&file, limit) else {
            omitted.push(format!("{} (could not be read)", file.entry));
            continue;
        };
        remaining = remaining.saturating_sub(contents.len() as u64);
        writer
            .start_file(&file.entry, options)
            .map_err(DiagnosticBundleError::Zip)?;
        writer
            .write_all(&contents)
            .map_err(DiagnosticBundleError::Write)?;
    }
    if !omitted.is_empty() {
        writer
            .start_file(PROFILER_OMITTED_ENTRY, options)
            .map_err(DiagnosticBundleError::Zip)?;
        writer
            .write_all(b"These profiler files were left out of this bundle:\n")
            .map_err(DiagnosticBundleError::Write)?;
        for line in omitted {
            writer
                .write_all(format!("{line}\n").as_bytes())
                .map_err(DiagnosticBundleError::Write)?;
        }
    }
    Ok(())
}

fn is_real_directory(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
}

fn list_profiler_files(
    dir: &Path,
    entry_prefix: &str,
    include_crash_dumps: bool,
    files: &mut Vec<ProfilerFile>,
    omitted: &mut Vec<String>,
) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(_) => {
            omitted.push(format!("{entry_prefix}/ (directory could not be read)"));
            return;
        }
    };
    for item in entries.flatten() {
        // `DirEntry::file_type` does not follow links, so a symlink is never `is_file`.
        if !item.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        let Some(name) = item.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !is_plain_file_name(&name) {
            continue;
        }
        let is_dump = name.to_ascii_lowercase().ends_with(".dmp");
        if is_dump && !include_crash_dumps {
            continue;
        }
        let Ok(metadata) = item.metadata() else {
            omitted.push(format!("{entry_prefix}/{name} (could not be read)"));
            continue;
        };
        files.push(ProfilerFile {
            path: item.path(),
            entry: format!("{entry_prefix}/{name}"),
            len: metadata.len(),
            modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            is_dump,
        });
    }
}

/// Profiler file names are generated by the profiler itself; anything else, such as a name
/// that could act as a path inside the archive, is not bundled.
fn is_plain_file_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// Dumps are copied byte for byte. Text files are read up to `limit`, marked when
/// truncated, and redacted like the logs.
fn read_profiler_file(file: &ProfilerFile, limit: u64) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(&file.path)
        .ok()?
        .take(limit.saturating_add(u64::from(!file.is_dump)))
        .read_to_end(&mut bytes)
        .ok()?;
    if file.is_dump {
        return Some(bytes);
    }
    let truncated = bytes.len() as u64 > limit;
    if truncated {
        bytes.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    }
    let mut text = redact_sensitive_text(&String::from_utf8_lossy(&bytes));
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if truncated {
        text.push_str("[truncated]\n");
    }
    Some(text.into_bytes())
}

fn collect_server_logs(logs_dir: &Path) -> Result<String, DiagnosticBundleError> {
    collect_retained_logs(&logs_dir.join(SERVER_LOG_ENTRY), EMPTY_SERVER_LOG)
}

fn collect_retained_logs(
    active_log: &Path,
    empty_message: &str,
) -> Result<String, DiagnosticBundleError> {
    let mut combined = String::new();
    for path in retained_server_log_paths(active_log) {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(contents) = read_bounded_if_present(&path)? else {
            continue;
        };
        if contents.trim().is_empty() {
            continue;
        }
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str(&format!("===== {name} =====\n"));
        combined.push_str(&redact_sensitive_text(&contents));
        if !combined.ends_with('\n') {
            combined.push('\n');
        }
    }
    if combined.is_empty() {
        Ok(empty_message.to_owned())
    } else {
        Ok(combined)
    }
}

fn read_bounded_if_present(path: &Path) -> Result<Option<String>, DiagnosticBundleError> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(DiagnosticBundleError::ReadServerLog(error)),
    };
    let limit = SERVER_LOG_MAX_BYTES.saturating_add(1);
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(limit)
        .read_to_end(&mut bytes)
        .map_err(DiagnosticBundleError::ReadServerLog)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > SERVER_LOG_MAX_BYTES {
        bytes.truncate(usize::try_from(SERVER_LOG_MAX_BYTES).unwrap_or(usize::MAX));
        bytes.extend_from_slice(b"\n[truncated]\n");
    }
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

fn bounded_frontend_log(input: &str) -> String {
    if input.len() <= FRONTEND_LOG_MAX_BYTES {
        return input.to_owned();
    }
    let mut output =
        String::from_utf8_lossy(&input.as_bytes()[..FRONTEND_LOG_MAX_BYTES]).into_owned();
    output.push_str("\n[truncated]\n");
    output
}
