use std::{
    collections::HashSet,
    ffi::OsString,
    io,
    path::{Path, PathBuf},
};

use thiserror::Error;

use crate::crypto::sha256_hex;

use super::GitWorktreeRecord;

const MAX_WORKTREE_RECORDS: usize = 512;
const WORKTREE_KEY_VERSION: &str = "coddy-studio.worktree.v1";
const WORKTREE_PRUNE_IMPACT_VERSION: &str = "coddy-studio.worktree-prune-impact.v2";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostPathPlatform {
    Posix,
    Windows,
}

#[must_use]
pub const fn host_path_platform() -> HostPathPlatform {
    if cfg!(windows) {
        HostPathPlatform::Windows
    } else {
        HostPathPlatform::Posix
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct WorktreeRepositoryKey(String);

impl WorktreeRepositoryKey {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct WorktreeKey(String);

impl WorktreeKey {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum WorktreeParseError {
    #[error("Git worktree porcelain output is empty")]
    Empty,
    #[error("Git worktree porcelain output has an unterminated record")]
    Unterminated,
    #[error("Git worktree porcelain output contains an empty record")]
    EmptyRecord,
    #[error("Git worktree porcelain output contains more than {MAX_WORKTREE_RECORDS} records")]
    TooManyRecords,
    #[error("Git worktree porcelain record is missing its worktree path")]
    MissingPath,
    #[error("Git worktree porcelain record has duplicate {field} fields")]
    DuplicateField { field: &'static str },
    #[error("Git worktree porcelain record has an invalid {field} field")]
    InvalidField { field: &'static str },
}

#[derive(Debug, Error, Eq, PartialEq)]
pub(crate) enum WorktreePruneParseError {
    #[error("Git worktree prune dry-run output has an unterminated record")]
    Unterminated,
    #[error("Git worktree prune dry-run output contains more than {MAX_WORKTREE_RECORDS} records")]
    TooManyRecords,
    #[error("Git worktree prune dry-run output contains a malformed record")]
    MalformedRecord,
    #[error("Git worktree prune dry-run output contains a duplicate admin record")]
    DuplicateRecord,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorktreePruneDryRunRecord {
    pub admin_id: String,
    pub reason: String,
}

#[derive(Debug, Error)]
pub enum WorktreeIdentityError {
    #[error("failed to canonicalize the common Git directory {path}")]
    CommonDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to canonicalize the registered worktree path {path}")]
    WorktreePath {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[must_use]
pub fn normalize_worktree_path_key(path: &Path, platform: HostPathPlatform) -> String {
    let mut source = match platform {
        HostPathPlatform::Posix => path.to_string_lossy().into_owned(),
        HostPathPlatform::Windows => path.to_string_lossy().replace('\\', "/"),
    };
    if platform == HostPathPlatform::Windows {
        source = strip_windows_verbatim_prefix(source);
        if looks_like_posix_absolute_path(&source) {
            return normalize_lexical_components(&source, HostPathPlatform::Posix);
        }
    }
    let normalized = normalize_lexical_components(&source, platform);
    match platform {
        HostPathPlatform::Posix => normalized,
        HostPathPlatform::Windows => windows_ordinal_case_key(&normalized),
    }
}

#[cfg(not(windows))]
fn windows_ordinal_case_key(value: &str) -> String {
    value.to_uppercase()
}

#[cfg(windows)]
fn windows_ordinal_case_key(value: &str) -> String {
    use windows_sys::Win32::Globalization::{
        LCMAP_UPPERCASE, LCMapStringEx, LOCALE_NAME_INVARIANT,
    };

    let source = value.encode_utf16().collect::<Vec<_>>();
    let Ok(source_len) = i32::try_from(source.len()) else {
        return value.to_uppercase();
    };
    // SAFETY: all pointers reference valid UTF-16 buffers for the lengths supplied.
    let required = unsafe {
        LCMapStringEx(
            LOCALE_NAME_INVARIANT,
            LCMAP_UPPERCASE,
            source.as_ptr(),
            source_len,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
            std::ptr::null(),
            0,
        )
    };
    if required <= 0 {
        return value.to_uppercase();
    }
    let mut output = vec![0_u16; required as usize];
    // SAFETY: output has exactly the capacity reported by the first LCMapStringEx call.
    let written = unsafe {
        LCMapStringEx(
            LOCALE_NAME_INVARIANT,
            LCMAP_UPPERCASE,
            source.as_ptr(),
            source_len,
            output.as_mut_ptr(),
            required,
            std::ptr::null(),
            std::ptr::null(),
            0,
        )
    };
    if written <= 0 {
        return value.to_uppercase();
    }
    output.truncate(written as usize);
    String::from_utf16(&output).unwrap_or_else(|_| value.to_uppercase())
}

/// Resolves a worktree path through existing host ancestors before normalizing it.
///
/// The leaf may already have been removed. Resolving the nearest existing ancestor keeps
/// aliases such as macOS `/var` and `/private/var` on one server-owned identity key.
pub async fn canonical_worktree_path_key(path: &Path) -> io::Result<String> {
    let platform = host_path_platform();
    let source = path.to_string_lossy();
    if platform == HostPathPlatform::Posix && looks_like_windows_absolute_path(&source) {
        let foreign_source = source.replace('\\', "/");
        return Ok(normalize_worktree_path_key(
            Path::new(&foreign_source),
            HostPathPlatform::Windows,
        ));
    }
    if uses_foreign_posix_identity(path, platform) {
        return Ok(normalize_worktree_path_key(path, HostPathPlatform::Windows));
    }
    let lexical = PathBuf::from(normalize_worktree_path_key(path, platform));
    let absolute = if lexical.is_absolute() {
        lexical
    } else {
        std::env::current_dir()?.join(lexical)
    };
    match tokio::fs::canonicalize(&absolute).await {
        Ok(canonical) => {
            return Ok(normalize_worktree_path_key(
                &canonical,
                host_path_platform(),
            ));
        }
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        Err(_) => {}
    }

    let mut ancestor = absolute.as_path();
    let mut missing = Vec::<OsString>::new();
    loop {
        match tokio::fs::canonicalize(ancestor).await {
            Ok(mut canonical) => {
                for component in missing.iter().rev() {
                    canonical.push(component);
                }
                return Ok(normalize_worktree_path_key(
                    &canonical,
                    host_path_platform(),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let component = ancestor.file_name().ok_or(error)?;
                missing.push(component.to_os_string());
                ancestor = ancestor.parent().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        "worktree path has no existing ancestor",
                    )
                })?;
            }
            Err(error) => return Err(error),
        }
    }
}

fn strip_windows_verbatim_prefix(path: String) -> String {
    if let Some(remainder) = path.strip_prefix("//?/UNC/") {
        return format!("//{remainder}");
    }
    path.strip_prefix("//?/").unwrap_or(&path).to_owned()
}

fn looks_like_posix_absolute_path(path: &str) -> bool {
    path.starts_with('/') && !path.starts_with("//")
}

pub(crate) fn uses_foreign_posix_identity(path: &Path, platform: HostPathPlatform) -> bool {
    platform == HostPathPlatform::Windows
        && looks_like_posix_absolute_path(&path.to_string_lossy().replace('\\', "/"))
}

fn looks_like_windows_absolute_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    (bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\'))
        || path.starts_with("\\\\")
        || path.starts_with("//")
}

fn normalize_lexical_components(path: &str, platform: HostPathPlatform) -> String {
    let collapsed = collapse_separators(path, platform == HostPathPlatform::Windows);
    let (prefix, remainder, locked_components, anchored) = match platform {
        HostPathPlatform::Windows if collapsed.starts_with("//") => {
            ("//", &collapsed[2..], 2, true)
        }
        HostPathPlatform::Windows
            if collapsed.as_bytes().get(1) == Some(&b':')
                && collapsed
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphabetic) =>
        {
            if collapsed.as_bytes().get(2) == Some(&b'/') {
                (&collapsed[..3], &collapsed[3..], 0, true)
            } else {
                (&collapsed[..2], &collapsed[2..], 0, false)
            }
        }
        _ if collapsed.starts_with('/') => ("/", &collapsed[1..], 0, true),
        _ => ("", collapsed.as_str(), 0, false),
    };

    let mut components: Vec<&str> = Vec::new();
    for component in remainder.split('/') {
        match component {
            "" | "." => {}
            ".." if components.len() > locked_components && components.last() != Some(&"..") => {
                components.pop();
            }
            ".." if anchored => {}
            ".." => components.push(component),
            _ => components.push(component),
        }
    }

    if components.is_empty() {
        return if prefix.is_empty() {
            if collapsed.is_empty() { "" } else { "." }.to_owned()
        } else {
            prefix.to_owned()
        };
    }
    let joined = components.join("/");
    format!("{prefix}{joined}")
}

fn collapse_separators(path: &str, preserve_unc_prefix: bool) -> String {
    let preserve_unc_prefix = preserve_unc_prefix && path.starts_with("//");
    let mut normalized = String::with_capacity(path.len());
    let mut previous_was_separator = false;
    for (index, character) in path.char_indices() {
        if character != '/' {
            normalized.push(character);
            previous_was_separator = false;
            continue;
        }
        if !previous_was_separator || (preserve_unc_prefix && index == 1) {
            normalized.push(character);
        }
        previous_was_separator = true;
    }
    normalized
}

#[must_use]
pub fn worktree_repository_key(
    common_dir: &Path,
    platform: HostPathPlatform,
) -> WorktreeRepositoryKey {
    WorktreeRepositoryKey(opaque_key(
        &normalize_worktree_path_key(common_dir, platform),
        None,
    ))
}

#[must_use]
pub fn worktree_key(common_dir: &Path, path: &Path, platform: HostPathPlatform) -> WorktreeKey {
    WorktreeKey(opaque_key(
        &normalize_worktree_path_key(common_dir, platform),
        Some(&normalize_worktree_path_key(path, platform)),
    ))
}

pub async fn resolved_worktree_keys(
    common_dir: &Path,
    git_path: &Path,
    platform: HostPathPlatform,
) -> Result<(WorktreeRepositoryKey, WorktreeKey), WorktreeIdentityError> {
    let common_dir = canonicalize_or_git_path(common_dir, true).await?;
    let git_path = canonicalize_or_git_path(git_path, false).await?;
    Ok((
        worktree_repository_key(&common_dir, platform),
        worktree_key(&common_dir, &git_path, platform),
    ))
}

async fn canonicalize_or_git_path(
    path: &Path,
    common_dir: bool,
) -> Result<PathBuf, WorktreeIdentityError> {
    match tokio::fs::canonicalize(path).await {
        Ok(path) => Ok(path),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(path.to_path_buf()),
        Err(source) if common_dir => Err(WorktreeIdentityError::CommonDir {
            path: path.to_path_buf(),
            source,
        }),
        Err(source) => Err(WorktreeIdentityError::WorktreePath {
            path: path.to_path_buf(),
            source,
        }),
    }
}

pub fn parse_worktree_porcelain(
    output: &str,
    nul_delimited: bool,
) -> Result<Vec<GitWorktreeRecord>, WorktreeParseError> {
    let raw_records = if nul_delimited {
        split_nul_records(output)?
    } else {
        split_legacy_records(output)?
    };
    if raw_records.is_empty() {
        return Err(WorktreeParseError::Empty);
    }
    if raw_records.len() > MAX_WORKTREE_RECORDS {
        return Err(WorktreeParseError::TooManyRecords);
    }
    raw_records
        .into_iter()
        .enumerate()
        .map(|(index, fields)| parse_record(&fields, !nul_delimited, index == 0))
        .collect()
}

pub(crate) fn parse_worktree_prune_dry_run(
    output: &str,
) -> Result<Vec<WorktreePruneDryRunRecord>, WorktreePruneParseError> {
    if !output.is_empty() && !output.ends_with('\n') {
        return Err(WorktreePruneParseError::Unterminated);
    }
    let mut admins = HashSet::new();
    let mut reasons = Vec::new();
    for line in output.lines() {
        if reasons.len() == MAX_WORKTREE_RECORDS {
            return Err(WorktreePruneParseError::TooManyRecords);
        }
        let Some(record) = line.strip_prefix("Removing worktrees/") else {
            return Err(WorktreePruneParseError::MalformedRecord);
        };
        let Some((admin, reason)) = record.split_once(": ") else {
            return Err(WorktreePruneParseError::MalformedRecord);
        };
        if admin.is_empty()
            || reason.is_empty()
            || admin.contains('\0')
            || reason.contains('\0')
            || !matches!(
                Path::new(admin).components().collect::<Vec<_>>().as_slice(),
                [std::path::Component::Normal(_)]
            )
        {
            return Err(WorktreePruneParseError::MalformedRecord);
        }
        if !admins.insert(admin) {
            return Err(WorktreePruneParseError::DuplicateRecord);
        }
        reasons.push(WorktreePruneDryRunRecord {
            admin_id: admin.to_owned(),
            reason: reason.to_owned(),
        });
    }
    Ok(reasons)
}

#[must_use]
pub fn git_worktree_prune_impact_digest(impact: &[super::GitPrunableWorktree]) -> String {
    let mut entries = impact
        .iter()
        .map(|entry| {
            (
                normalize_worktree_path_key(&entry.path, host_path_platform()),
                entry.prune_reason.as_str(),
            )
        })
        .collect::<Vec<_>>();
    entries.sort_by(|(left_path, left_reason), (right_path, right_reason)| {
        left_path
            .cmp(right_path)
            .then_with(|| left_reason.cmp(right_reason))
    });
    let mut input = Vec::with_capacity(
        WORKTREE_PRUNE_IMPACT_VERSION.len()
            + entries
                .iter()
                .map(|(path, reason)| path.len() + reason.len() + 16)
                .sum::<usize>()
            + 8,
    );
    input.extend_from_slice(WORKTREE_PRUNE_IMPACT_VERSION.as_bytes());
    push_prune_digest_length(&mut input, entries.len());
    for (path, reason) in entries {
        push_prune_digest_field(&mut input, &path);
        push_prune_digest_field(&mut input, reason);
    }
    sha256_hex(input)
}

fn push_prune_digest_field(input: &mut Vec<u8>, field: &str) {
    push_prune_digest_length(input, field.len());
    input.extend_from_slice(field.as_bytes());
}

fn push_prune_digest_length(input: &mut Vec<u8>, length: usize) {
    let length = u64::try_from(length).expect("bounded prune impact length fits u64");
    input.extend_from_slice(&length.to_be_bytes());
}

fn opaque_key(common_dir_key: &str, path_key: Option<&str>) -> String {
    let mut input = Vec::with_capacity(
        WORKTREE_KEY_VERSION.len() + common_dir_key.len() + path_key.map_or(0, str::len) + 2,
    );
    input.extend_from_slice(WORKTREE_KEY_VERSION.as_bytes());
    input.push(b'\0');
    input.extend_from_slice(common_dir_key.as_bytes());
    if let Some(path_key) = path_key {
        input.push(b'\0');
        input.extend_from_slice(path_key.as_bytes());
    }
    sha256_hex(input)
}

fn split_nul_records(output: &str) -> Result<Vec<Vec<&str>>, WorktreeParseError> {
    if output.is_empty() {
        return Err(WorktreeParseError::Empty);
    }
    if !output.ends_with("\0\0") {
        return Err(WorktreeParseError::Unterminated);
    }
    let mut records = Vec::new();
    let mut fields = Vec::new();
    for field in output.split_terminator('\0') {
        if field.is_empty() {
            if fields.is_empty() {
                return Err(WorktreeParseError::EmptyRecord);
            }
            records.push(std::mem::take(&mut fields));
        } else {
            fields.push(field);
        }
    }
    if !fields.is_empty() {
        return Err(WorktreeParseError::Unterminated);
    }
    Ok(records)
}

fn split_legacy_records(output: &str) -> Result<Vec<Vec<&str>>, WorktreeParseError> {
    if output.is_empty() {
        return Err(WorktreeParseError::Empty);
    }
    if !output.ends_with("\n\n") {
        return Err(WorktreeParseError::Unterminated);
    }
    let mut records = Vec::new();
    for record in output[..output.len() - 1].split("\n\n") {
        if record.is_empty() {
            return Err(WorktreeParseError::EmptyRecord);
        }
        let fields: Vec<&str> = record.lines().collect();
        if fields.is_empty() || fields.iter().any(|field| field.is_empty()) {
            return Err(WorktreeParseError::EmptyRecord);
        }
        records.push(fields);
    }
    Ok(records)
}

fn parse_record(
    fields: &[&str],
    legacy: bool,
    is_primary: bool,
) -> Result<GitWorktreeRecord, WorktreeParseError> {
    let mut seen = HashSet::new();
    let mut path = None;
    let mut head = None;
    let mut branch = None;
    let mut is_bare = false;
    let mut locked = false;
    let mut lock_reason = None;
    let mut is_prunable = false;
    let mut prunable_reason = None;
    let mut detached = false;
    let mut unborn = false;

    for field in fields {
        let (name, value) = split_field(field);
        let singleton = match name {
            "worktree" => Some("worktree"),
            "HEAD" => Some("HEAD"),
            "branch" => Some("branch"),
            "bare" => Some("bare"),
            "locked" => Some("locked"),
            "prunable" => Some("prunable"),
            "detached" => Some("detached"),
            "unborn" => Some("unborn"),
            _ => None,
        };
        if let Some(singleton) = singleton
            && !seen.insert(singleton)
        {
            return Err(WorktreeParseError::DuplicateField { field: singleton });
        }
        match name {
            "worktree" => {
                let value = value.ok_or(WorktreeParseError::MissingPath)?;
                if value.is_empty() {
                    return Err(WorktreeParseError::MissingPath);
                }
                path = Some(PathBuf::from(if legacy {
                    decode_c_style(value)?
                } else {
                    value.to_owned()
                }));
            }
            "HEAD" => {
                head = Some(
                    value
                        .filter(|value| !value.is_empty())
                        .ok_or(WorktreeParseError::InvalidField { field: "HEAD" })?
                        .to_owned(),
                );
            }
            "branch" => {
                let branch_ref = value
                    .filter(|value| !value.is_empty())
                    .ok_or(WorktreeParseError::InvalidField { field: "branch" })?;
                branch = Some(
                    branch_ref
                        .strip_prefix("refs/heads/")
                        .ok_or(WorktreeParseError::InvalidField { field: "branch" })?
                        .to_owned(),
                );
            }
            "bare" => {
                if value.is_some() {
                    return Err(WorktreeParseError::InvalidField { field: "bare" });
                }
                is_bare = true;
            }
            "locked" => {
                locked = true;
                lock_reason = value.map(str::to_owned).filter(|value| !value.is_empty());
            }
            "prunable" => {
                is_prunable = true;
                prunable_reason = value.map(str::to_owned).filter(|value| !value.is_empty());
            }
            "detached" => {
                if value.is_some() {
                    return Err(WorktreeParseError::InvalidField { field: "detached" });
                }
                detached = true;
            }
            "unborn" => {
                if value.is_some() {
                    return Err(WorktreeParseError::InvalidField { field: "unborn" });
                }
                unborn = true;
            }
            _ => {}
        }
    }

    let path = path.ok_or(WorktreeParseError::MissingPath)?;
    Ok(GitWorktreeRecord {
        path,
        head,
        branch: (!detached && !unborn).then_some(branch).flatten(),
        is_primary,
        is_bare,
        locked,
        lock_reason,
        is_prunable,
        prunable_reason,
    })
}

fn split_field(field: &str) -> (&str, Option<&str>) {
    match field.split_once(' ') {
        Some((name, value)) => (name, Some(value)),
        None => (field, None),
    }
}

fn decode_c_style(value: &str) -> Result<String, WorktreeParseError> {
    if !value.starts_with('"') {
        return Ok(value.to_owned());
    }
    if value.len() < 2 || !value.ends_with('"') {
        return Err(WorktreeParseError::InvalidField { field: "worktree" });
    }
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 1;
    while index < bytes.len() - 1 {
        let byte = bytes[index];
        if byte != b'\\' {
            decoded.push(byte);
            index += 1;
            continue;
        }
        index += 1;
        let Some(escaped) = bytes.get(index).copied() else {
            return Err(WorktreeParseError::InvalidField { field: "worktree" });
        };
        let value = match escaped {
            b'a' => b'\x07',
            b'b' => b'\x08',
            b'f' => b'\x0c',
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'v' => b'\x0b',
            b'\\' => b'\\',
            b'"' => b'"',
            b'0'..=b'7' => {
                let digits = bytes
                    .get(index..index + 3)
                    .filter(|digits| digits.iter().all(|digit| matches!(digit, b'0'..=b'7')))
                    .ok_or(WorktreeParseError::InvalidField { field: "worktree" })?;
                index += 2;
                (digits[0] - b'0') * 64 + (digits[1] - b'0') * 8 + (digits[2] - b'0')
            }
            _ => return Err(WorktreeParseError::InvalidField { field: "worktree" }),
        };
        decoded.push(value);
        index += 1;
    }
    String::from_utf8(decoded).map_err(|_| WorktreeParseError::InvalidField { field: "worktree" })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        HostPathPlatform, WorktreeIdentityError, canonical_worktree_path_key,
        normalize_worktree_path_key, parse_worktree_porcelain, parse_worktree_prune_dry_run,
        resolved_worktree_keys, worktree_key, worktree_repository_key,
    };

    #[test]
    fn parses_nul_records_with_special_paths_and_state() {
        let records = parse_worktree_porcelain(
            "worktree /repo main\ncopy\0HEAD abc123\0branch refs/heads/feature/space\0locked maintenance\0\0worktree /repo linked\0HEAD def456\0detached\0prunable stale admin\0\0",
            true,
        )
        .expect("NUL porcelain parses");

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].path, Path::new("/repo main\ncopy"));
        assert_eq!(records[0].branch.as_deref(), Some("feature/space"));
        assert!(records[0].locked);
        assert_eq!(records[0].lock_reason.as_deref(), Some("maintenance"));
        assert!(records[0].is_primary);
        assert_eq!(records[1].branch, None);
        assert!(records[1].is_prunable);
        assert_eq!(records[1].prunable_reason.as_deref(), Some("stale admin"));
    }

    #[test]
    fn preserves_prunable_state_with_and_without_a_reason_and_ignores_unknown_fields() {
        let records = parse_worktree_porcelain(
            "worktree /prunable\nlocked\nprunable\nfuture-field preserved\n\nworktree /reasoned\nlocked maintenance\nprunable stale registration\n\n",
            false,
        )
        .expect("legacy porcelain with forward-compatible fields parses");

        assert!(records[0].locked);
        assert_eq!(records[0].lock_reason, None);
        assert!(records[0].is_prunable);
        assert_eq!(records[0].prunable_reason, None);
        assert!(records[1].locked);
        assert_eq!(records[1].lock_reason.as_deref(), Some("maintenance"));
        assert!(records[1].is_prunable);
        assert_eq!(
            records[1].prunable_reason.as_deref(),
            Some("stale registration")
        );
    }

    #[test]
    fn parses_legacy_c_quoted_paths_and_unborn_bare_records() {
        let records = parse_worktree_porcelain(
            "worktree \"/repo path\\nnext\"\nHEAD abc123\nbranch refs/heads/main\n\nworktree /bare\nbare\nbranch refs/heads/unborn\nunborn\n\n",
            false,
        )
        .expect("legacy porcelain parses");

        assert_eq!(records[0].path, Path::new("/repo path\nnext"));
        assert_eq!(records[0].branch.as_deref(), Some("main"));
        assert!(records[1].is_bare);
        assert_eq!(records[1].branch, None);
    }

    #[test]
    fn rejects_malformed_and_non_authoritative_records() {
        for (porcelain, nul_delimited) in [
            ("", false),
            ("worktree /one\nworktree /two\n\n", false),
            ("worktree /one\nbranch refs/heads/main", false),
            ("HEAD abc\n\n", false),
            ("worktree /one\0HEAD abc\0", true),
        ] {
            assert!(parse_worktree_porcelain(porcelain, nul_delimited).is_err());
        }

        let many = (0..513)
            .map(|index| format!("worktree /repo/{index}\n\n"))
            .collect::<String>();
        assert!(parse_worktree_porcelain(&many, false).is_err());
    }

    #[test]
    fn parses_bounded_verbose_prune_output_and_rejects_unknown_records() {
        assert_eq!(
            parse_worktree_prune_dry_run(
                "Removing worktrees/one: gitdir file points to non-existent location\nRemoving worktrees/two: stale admin\n"
            )
            .expect("bounded prune preview parses"),
            vec![
                super::WorktreePruneDryRunRecord {
                    admin_id: "one".to_owned(),
                    reason: "gitdir file points to non-existent location".to_owned(),
                },
                super::WorktreePruneDryRunRecord {
                    admin_id: "two".to_owned(),
                    reason: "stale admin".to_owned(),
                },
            ]
        );
        for output in [
            "unexpected output\n".to_owned(),
            "Removing worktrees/one: stale".to_owned(),
            "Removing worktrees/: stale\n".to_owned(),
            "Removing worktrees/../escape: stale\n".to_owned(),
            "Removing worktrees/one: \n".to_owned(),
            (0..513)
                .map(|index| format!("Removing worktrees/{index}: stale\n"))
                .collect::<String>(),
        ] {
            assert!(parse_worktree_prune_dry_run(&output).is_err());
        }
    }

    #[test]
    fn normalizes_host_path_keys_and_derives_stable_opaque_keys() {
        assert_eq!(
            normalize_worktree_path_key(Path::new(r"C:\Repo\Work\"), HostPathPlatform::Windows),
            "C:/REPO/WORK"
        );
        assert_eq!(
            normalize_worktree_path_key(
                Path::new(r"\\Server\Share\Repo\"),
                HostPathPlatform::Windows
            ),
            "//SERVER/SHARE/REPO"
        );
        assert_ne!(
            normalize_worktree_path_key(Path::new("/Repo"), HostPathPlatform::Posix),
            normalize_worktree_path_key(Path::new("/repo"), HostPathPlatform::Posix)
        );
        assert_ne!(
            normalize_worktree_path_key(Path::new(r"/repo\\name"), HostPathPlatform::Posix),
            normalize_worktree_path_key(Path::new("/repo/name"), HostPathPlatform::Posix)
        );
        assert_eq!(
            normalize_worktree_path_key(Path::new(r"C:\Repo\Work\"), HostPathPlatform::Windows),
            normalize_worktree_path_key(Path::new("c:/repo/work"), HostPathPlatform::Windows)
        );
        assert_eq!(
            normalize_worktree_path_key(Path::new(r"\\?\C:\Repo\Work"), HostPathPlatform::Windows,),
            "C:/REPO/WORK",
            "Win32 verbatim drive prefixes are not part of path identity",
        );
        assert_eq!(
            normalize_worktree_path_key(
                Path::new(r"\\?\UNC\Server\Share\Repo"),
                HostPathPlatform::Windows,
            ),
            "//SERVER/SHARE/REPO",
            "Win32 verbatim UNC prefixes normalize to ordinary UNC identity",
        );
        assert_eq!(
            normalize_worktree_path_key(Path::new("/repo/Feature"), HostPathPlatform::Windows),
            "/repo/Feature",
            "foreign POSIX paths retain their case-sensitive identity on Windows",
        );
        assert_eq!(
            normalize_worktree_path_key(
                Path::new(r"\\Server\Share\Repo"),
                HostPathPlatform::Windows
            ),
            normalize_worktree_path_key(
                Path::new("//server/share/repo"),
                HostPathPlatform::Windows
            )
        );
        assert_eq!(
            normalize_worktree_path_key(
                Path::new("/repo//worktrees/./feature/src/../src"),
                HostPathPlatform::Posix,
            ),
            "/repo/worktrees/feature/src"
        );
        assert_eq!(
            normalize_worktree_path_key(Path::new("/../../repo/worktree"), HostPathPlatform::Posix,),
            "/repo/worktree",
            "absolute parent traversal cannot escape the POSIX root",
        );
        assert_eq!(
            normalize_worktree_path_key(Path::new("../../repo"), HostPathPlatform::Posix),
            "../../repo",
            "relative parent components retain their unresolved meaning",
        );
        assert_eq!(
            normalize_worktree_path_key(
                Path::new(r"C:\Repo\\Work\.\src\..\src"),
                HostPathPlatform::Windows,
            ),
            "C:/REPO/WORK/SRC"
        );
        assert_eq!(
            normalize_worktree_path_key(
                Path::new(r"C:\..\..\Repo\Work"),
                HostPathPlatform::Windows,
            ),
            "C:/REPO/WORK",
            "drive-root parent traversal cannot escape the drive root",
        );
        assert_eq!(
            normalize_worktree_path_key(Path::new(r"C:..\Repo\.\Work"), HostPathPlatform::Windows,),
            "C:../REPO/WORK",
            "drive-relative parent traversal must remain drive-relative",
        );
        assert_eq!(
            normalize_worktree_path_key(
                Path::new(r"\\Server\Share\Repo\..\Work"),
                HostPathPlatform::Windows,
            ),
            "//SERVER/SHARE/WORK"
        );
        assert_eq!(
            normalize_worktree_path_key(
                Path::new(r"\\Server\Share\..\..\Work"),
                HostPathPlatform::Windows,
            ),
            "//SERVER/SHARE/WORK",
            "UNC parent traversal cannot escape the share root",
        );

        let repository = worktree_repository_key(Path::new("/repo/.git"), HostPathPlatform::Posix);
        let worktree = worktree_key(
            Path::new("/repo/.git"),
            Path::new("/repo"),
            HostPathPlatform::Posix,
        );
        assert_eq!(
            repository.as_str(),
            "645ba9aff325a5b317ca9f1e74cf2cb08fae039ffd44fce517bc46891d96343e"
        );
        assert_eq!(
            worktree.as_str(),
            "6ddf15a6da57b88326fff5d03a6f4e18c16682c3aac7e8a1c17232559d495294"
        );
        assert_eq!(
            worktree,
            worktree_key(
                Path::new("/repo/.git/"),
                Path::new("/repo/"),
                HostPathPlatform::Posix,
            )
        );
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn foreign_windows_absolute_paths_are_not_anchored_to_the_posix_process_directory() {
        assert_eq!(
            canonical_worktree_path_key(Path::new(r"C:\Repo\.worktrees\bootstrap"))
                .await
                .expect("drive path remains a foreign absolute path"),
            "C:/REPO/.WORKTREES/BOOTSTRAP"
        );
        assert_eq!(
            canonical_worktree_path_key(Path::new(r"\\SÉRVEUR\PARTAGÉ\Δelta"))
                .await
                .expect("UNC path remains a foreign absolute path"),
            "//SÉRVEUR/PARTAGÉ/ΔELTA"
        );
    }

    #[test]
    fn windows_path_identity_case_folds_non_ascii_drive_and_unc_components() {
        assert_eq!(
            normalize_worktree_path_key(
                Path::new(r"C:\RÉPO\Ünicode\Feature"),
                HostPathPlatform::Windows,
            ),
            normalize_worktree_path_key(
                Path::new(r"c:/répo/ünicode/feature"),
                HostPathPlatform::Windows,
            ),
            "drive paths use Unicode-aware case folding"
        );
        assert_eq!(
            normalize_worktree_path_key(
                Path::new(r"\\SÉRVEUR\PARTAGÉ\Δelta"),
                HostPathPlatform::Windows,
            ),
            normalize_worktree_path_key(
                Path::new("//sérveur/partagé/δelta"),
                HostPathPlatform::Windows,
            ),
            "UNC server, share, and path components share one Unicode identity"
        );
        assert_eq!(
            normalize_worktree_path_key(Path::new(r"C:\RÉPO\Σ\Feature"), HostPathPlatform::Windows,),
            normalize_worktree_path_key(Path::new(r"c:/répo/σ/feature"), HostPathPlatform::Windows,),
            "Windows ordinal caseless identity treats capital and small sigma as one path"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_native_present_and_missing_aliases_share_ordinal_identity() {
        let root = tempfile::tempdir().expect("Windows identity root");
        let present = root.path().join("RÉPO").join("Σ");
        std::fs::create_dir_all(&present).expect("present Unicode path");
        let present_alias = root.path().join("répo").join("σ");
        assert_eq!(
            canonical_worktree_path_key(&present)
                .await
                .expect("present identity"),
            canonical_worktree_path_key(&present_alias)
                .await
                .expect("present alias identity")
        );

        let missing = present.join("MISSING").join("Σ");
        let missing_alias = present_alias.join("missing").join("σ");
        assert_eq!(
            canonical_worktree_path_key(&missing)
                .await
                .expect("missing identity"),
            canonical_worktree_path_key(&missing_alias)
                .await
                .expect("missing alias identity")
        );
    }

    #[tokio::test]
    async fn canonical_key_resolution_distinguishes_missing_and_other_io_errors() {
        let present = tempfile::tempdir().expect("present path fixture");
        let missing = present.path().join("missing");
        let invalid = Path::new("\0not-a-path");

        let resolved =
            resolved_worktree_keys(present.path(), present.path(), HostPathPlatform::Posix)
                .await
                .expect("present paths canonicalize");
        let canonical_present = tokio::fs::canonicalize(present.path())
            .await
            .expect("canonical present fixture");
        assert_eq!(
            resolved.1,
            worktree_key(
                &canonical_present,
                &canonical_present,
                HostPathPlatform::Posix
            )
        );
        assert!(
            resolved_worktree_keys(present.path(), &missing, HostPathPlatform::Posix)
                .await
                .expect("missing worktree falls back to Git path")
                .1
                .as_str()
                .len()
                == 64
        );
        assert!(matches!(
            resolved_worktree_keys(present.path(), invalid, HostPathPlatform::Posix).await,
            Err(WorktreeIdentityError::WorktreePath { .. })
        ));
        assert!(matches!(
            resolved_worktree_keys(invalid, present.path(), HostPathPlatform::Posix).await,
            Err(WorktreeIdentityError::CommonDir { .. })
        ));
    }
}
