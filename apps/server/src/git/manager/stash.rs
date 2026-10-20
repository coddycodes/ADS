//! Native Git stash parsing and operations.

use std::{collections::HashMap, path::Path};

use serde::Serialize;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::git::{GitCommandError, GitRepository, ProcessOutput};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitManagerStashRecord {
    pub selector: String,
    pub sha: String,
    pub message: String,
    pub committed_at_ms: u64,
    pub parents: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum GitManagerChangedFileStatus {
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    Untracked,
    Unmerged,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitManagerChangedFile {
    pub path: String,
    pub status: GitManagerChangedFileStatus,
    pub insertions: u64,
    pub deletions: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitManagerStashEntry {
    pub index: u64,
    pub sha: String,
    pub message: String,
    pub committed_at_ms: u64,
    pub parents: Vec<String>,
    pub files: Vec<GitManagerChangedFile>,
}

#[derive(Debug, Error)]
pub enum GitManagerStashError {
    #[error("Git returned malformed stash state")]
    Malformed,
    #[error("the requested stash is no longer present")]
    NotFound,
    #[error("Git could not read stash state")]
    CommandFailed,
    #[error(transparent)]
    Git(#[from] GitCommandError),
}

#[derive(Clone, Debug)]
struct RawChangedFile {
    path: String,
    status: GitManagerChangedFileStatus,
}

#[must_use]
pub fn parse_stash_list(stdout: &str) -> Vec<GitManagerStashRecord> {
    stdout
        .split('\0')
        .filter(|record| !record.is_empty())
        .filter_map(|record| {
            let fields = record.split('\u{1f}').collect::<Vec<_>>();
            let [selector, sha, message, committed_at, parents] = fields.as_slice() else {
                return None;
            };
            Some(GitManagerStashRecord {
                selector: selector
                    .strip_prefix("refs/")
                    .unwrap_or(selector)
                    .to_string(),
                sha: (*sha).to_owned(),
                message: (*message).to_owned(),
                committed_at_ms: committed_at.parse::<u64>().ok()?.saturating_mul(1_000),
                parents: parents.split_whitespace().map(str::to_owned).collect(),
            })
        })
        .collect()
}

#[must_use]
pub fn parse_stash_file_list(stdout: &str) -> Vec<GitManagerChangedFile> {
    let fields = stdout.split('\0').collect::<Vec<_>>();
    let mut raw_files = Vec::new();
    let mut stats = HashMap::<String, (u64, u64)>::new();
    let mut index = 0;
    while index < fields.len() {
        let field = fields[index];
        if field.starts_with(':') {
            let status = field.split_whitespace().last().unwrap_or_default();
            let Some(first_path) = fields.get(index + 1).filter(|path| !path.is_empty()) else {
                index += 1;
                continue;
            };
            let (path, consumed) = if status.starts_with(['R', 'C']) {
                match fields.get(index + 2).filter(|path| !path.is_empty()) {
                    Some(new_path) => ((*new_path).to_owned(), 3),
                    None => ((*first_path).to_owned(), 2),
                }
            } else {
                ((*first_path).to_owned(), 2)
            };
            raw_files.push(RawChangedFile {
                path,
                status: changed_file_status(status),
            });
            index += consumed;
            continue;
        }
        if let Some((insertions, remainder)) = field.split_once('\t')
            && let Some((deletions, inline_path)) = remainder.split_once('\t')
        {
            let insertions = parse_numstat_count(insertions);
            let deletions = parse_numstat_count(deletions);
            if inline_path.is_empty() {
                if let Some(new_path) = fields.get(index + 2).filter(|path| !path.is_empty()) {
                    stats.insert((*new_path).to_owned(), (insertions, deletions));
                    index += 3;
                    continue;
                }
            } else {
                stats.insert(inline_path.to_owned(), (insertions, deletions));
            }
        }
        index += 1;
    }

    let mut files = Vec::with_capacity(raw_files.len() + stats.len());
    for file in raw_files {
        let (insertions, deletions) = stats.remove(&file.path).unwrap_or_default();
        files.push(GitManagerChangedFile {
            path: file.path,
            status: file.status,
            insertions,
            deletions,
        });
    }
    files.extend(
        stats
            .into_iter()
            .map(|(path, (insertions, deletions))| GitManagerChangedFile {
                path,
                status: GitManagerChangedFileStatus::Modified,
                insertions,
                deletions,
            }),
    );
    files
}

fn changed_file_status(status: &str) -> GitManagerChangedFileStatus {
    match status.as_bytes().first().copied() {
        Some(b'A') => GitManagerChangedFileStatus::Added,
        Some(b'D') => GitManagerChangedFileStatus::Deleted,
        Some(b'R') => GitManagerChangedFileStatus::Renamed,
        Some(b'C') => GitManagerChangedFileStatus::Copied,
        Some(b'U') => GitManagerChangedFileStatus::Unmerged,
        _ => GitManagerChangedFileStatus::Modified,
    }
}

fn parse_numstat_count(value: &str) -> u64 {
    value.parse().unwrap_or(0)
}

pub fn resolve_stash_selector<'a>(
    entries: &'a [GitManagerStashRecord],
    sha: &str,
) -> Result<&'a str, GitManagerStashError> {
    entries
        .iter()
        .find(|entry| entry.sha == sha)
        .map(|entry| entry.selector.as_str())
        .ok_or(GitManagerStashError::NotFound)
}

pub async fn stash_records(
    repository: &GitRepository,
    cwd: &Path,
    cancellation: &CancellationToken,
) -> Result<Vec<GitManagerStashRecord>, GitManagerStashError> {
    let output = repository.git_manager_stash_list(cwd, cancellation).await?;
    match output.exit_code {
        128 => Ok(Vec::new()),
        0 => {
            let records = parse_stash_list(&output.stdout);
            let input_count = output
                .stdout
                .split('\0')
                .filter(|row| !row.is_empty())
                .count();
            (records.len() == input_count)
                .then_some(records)
                .ok_or(GitManagerStashError::Malformed)
        }
        _ => Err(GitManagerStashError::CommandFailed),
    }
}

pub async fn list_stashes(
    repository: &GitRepository,
    cwd: &Path,
    cancellation: &CancellationToken,
) -> Result<Vec<GitManagerStashEntry>, GitManagerStashError> {
    let records = stash_records(repository, cwd, cancellation).await?;
    let mut entries = Vec::with_capacity(records.len());
    for record in records {
        let index = stash_index(&record.selector).ok_or(GitManagerStashError::Malformed)?;
        let files = repository
            .git_manager_stash_file_list(cwd, &record.selector, cancellation)
            .await?;
        entries.push(GitManagerStashEntry {
            index,
            sha: record.sha,
            message: record.message,
            committed_at_ms: record.committed_at_ms,
            parents: record.parents,
            files: parse_stash_file_list(&files.stdout),
        });
    }
    Ok(entries)
}

pub async fn resolve_current_stash_selector(
    repository: &GitRepository,
    cwd: &Path,
    sha: &str,
    cancellation: &CancellationToken,
) -> Result<String, GitManagerStashError> {
    let records = stash_records(repository, cwd, cancellation).await?;
    resolve_stash_selector(&records, sha).map(str::to_owned)
}

pub async fn diff(
    repository: &GitRepository,
    cwd: &Path,
    sha: &str,
    path: &str,
    cancellation: &CancellationToken,
) -> Result<ProcessOutput, GitManagerStashError> {
    let selector = resolve_current_stash_selector(repository, cwd, sha, cancellation).await?;
    let mut output = repository
        .git_manager_stash_diff(cwd, &selector, cancellation)
        .await
        .map_err(GitManagerStashError::Git)?;
    output.stdout = filter_patch_for_path(&output.stdout, path);
    Ok(output)
}

fn filter_patch_for_path(patch: &str, path: &str) -> String {
    let marker = "diff --git ";
    let old_path = format!("--- a/{path}");
    let new_path = format!("+++ b/{path}");
    let rename_from = format!("rename from {path}");
    let rename_to = format!("rename to {path}");
    patch
        .split(marker)
        .skip(1)
        .filter(|section| {
            section.lines().any(|line| {
                line == old_path || line == new_path || line == rename_from || line == rename_to
            })
        })
        .map(|section| format!("{marker}{section}"))
        .collect::<Vec<_>>()
        .join("")
}

/// Saves the working tree in a new stash entry. `include_untracked` adds
/// untracked files (`--include-untracked`); ignored files are never stashed.
pub async fn push(
    repository: &GitRepository,
    cwd: &Path,
    message: &str,
    paths: &[String],
    include_untracked: bool,
    cancellation: &CancellationToken,
) -> Result<Vec<ProcessOutput>, GitCommandError> {
    repository
        .git_manager_stash_push(cwd, message, paths, include_untracked, cancellation)
        .await
}

pub async fn apply(
    repository: &GitRepository,
    cwd: &Path,
    index: u64,
    cancellation: &CancellationToken,
) -> Result<ProcessOutput, GitCommandError> {
    repository
        .git_manager_stash_apply(cwd, &stash_selector(index), cancellation)
        .await
}

pub async fn pop(
    repository: &GitRepository,
    cwd: &Path,
    index: u64,
    cancellation: &CancellationToken,
) -> Result<ProcessOutput, GitCommandError> {
    repository
        .git_manager_stash_pop(cwd, &stash_selector(index), cancellation)
        .await
}

pub async fn drop_stash(
    repository: &GitRepository,
    cwd: &Path,
    index: u64,
    cancellation: &CancellationToken,
) -> Result<ProcessOutput, GitCommandError> {
    repository
        .git_manager_stash_drop(cwd, &stash_selector(index), cancellation)
        .await
}

fn stash_selector(index: u64) -> String {
    format!("stash@{{{index}}}")
}

fn stash_index(selector: &str) -> Option<u64> {
    selector
        .strip_prefix("stash@{")?
        .strip_suffix('}')?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, process::Command};

    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::git::GitRepository;

    fn git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git fixture starts");
        assert!(
            output.status.success(),
            "git fixture failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn parses_a_nul_delimited_stash_log_into_ordered_entries() {
        let stdout = "stash@{0}\u{1f}abc123\u{1f}WIP on feature: 1234567 tidy\u{1f}1735689600\u{1f}p1 p2 p3\0\
                      stash@{1}\u{1f}def456\u{1f}On master: wip\u{1f}1735689000\u{1f}q1 q2\0";
        let entries = parse_stash_list(stdout);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].selector, "stash@{0}");
        assert_eq!(entries[0].sha, "abc123");
        assert_eq!(entries[0].parents, vec!["p1", "p2", "p3"]);
        assert_eq!(entries[1].message, "On master: wip");
    }

    #[test]
    fn parses_raw_numstat_rows_including_a_rename() {
        let stdout = concat!(
            ":100644 100644 aaaaaaa bbbbbbb M\0tracked.txt\0",
            ":100644 100644 ccccccc ccccccc R100\0old.txt\0new.txt\0",
            "3\t1\ttracked.txt\0",
            "0\t0\t\0old.txt\0new.txt\0",
        );

        let files = parse_stash_file_list(stdout);

        assert_eq!(files.len(), 2);
        assert_eq!(files[0].path, "tracked.txt");
        assert_eq!(files[0].status, GitManagerChangedFileStatus::Modified);
        assert_eq!((files[0].insertions, files[0].deletions), (3, 1));
        assert_eq!(files[1].path, "new.txt");
        assert_eq!(files[1].status, GitManagerChangedFileStatus::Renamed);
        assert_eq!((files[1].insertions, files[1].deletions), (0, 0));
    }

    #[test]
    fn resolves_a_stash_sha_to_its_current_selector() {
        let entries = parse_stash_list(
            "stash@{0}\u{1f}new\u{1f}newer\u{1f}2\u{1f}p\0\
             stash@{1}\u{1f}wanted\u{1f}older\u{1f}1\u{1f}p\0",
        );

        assert_eq!(
            resolve_stash_selector(&entries, "wanted").expect("stash remains present"),
            "stash@{1}"
        );
    }

    #[test]
    fn missing_stash_sha_is_a_structured_failure() {
        let entries = parse_stash_list("stash@{0}\u{1f}other\u{1f}message\u{1f}1\u{1f}p\0");

        assert!(matches!(
            resolve_stash_selector(&entries, "dropped"),
            Err(GitManagerStashError::NotFound)
        ));
    }

    fn committed_fixture() -> tempfile::TempDir {
        let fixture = tempfile::tempdir().expect("temporary repository");
        git(fixture.path(), &["init", "-q", "-b", "main"]);
        git(fixture.path(), &["config", "user.name", "Git Manager Test"]);
        git(
            fixture.path(),
            &["config", "user.email", "git-manager@example.test"],
        );
        fs::write(fixture.path().join("tracked.txt"), "base\n").expect("base file");
        git(fixture.path(), &["add", "tracked.txt"]);
        git(fixture.path(), &["commit", "-q", "-m", "base"]);
        fixture
    }

    #[tokio::test]
    async fn listing_and_diff_show_the_untracked_files_a_stash_saved() {
        let fixture = committed_fixture();
        fs::write(fixture.path().join("notes.md"), "my notes\n").expect("untracked file");
        git(
            fixture.path(),
            &["stash", "push", "-q", "--include-untracked", "-m", "saved"],
        );

        let repository = GitRepository::default();
        let cancellation = CancellationToken::new();
        let entries = list_stashes(&repository, fixture.path(), &cancellation)
            .await
            .expect("stash list");

        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0].files,
            vec![GitManagerChangedFile {
                path: "notes.md".into(),
                status: GitManagerChangedFileStatus::Added,
                insertions: 1,
                deletions: 0,
            }]
        );
        let patch = diff(
            &repository,
            fixture.path(),
            &entries[0].sha,
            "notes.md",
            &cancellation,
        )
        .await
        .expect("stash diff");
        assert!(patch.stdout.contains("+my notes"), "{}", patch.stdout);
    }

    #[tokio::test]
    async fn push_saves_untracked_files_only_when_asked_and_never_ignored_ones() {
        let repository = GitRepository::default();
        let cancellation = CancellationToken::new();
        for include_untracked in [false, true] {
            let fixture = committed_fixture();
            fs::write(fixture.path().join(".gitignore"), "*.log\n").expect("ignore rules");
            git(fixture.path(), &["add", ".gitignore"]);
            git(fixture.path(), &["commit", "-q", "-m", "ignore logs"]);
            fs::write(fixture.path().join("tracked.txt"), "edited\n").expect("tracked edit");
            fs::write(fixture.path().join("notes.md"), "my notes\n").expect("untracked file");
            fs::write(fixture.path().join("build.log"), "ignored\n").expect("ignored file");

            let outputs = push(
                &repository,
                fixture.path(),
                "saved",
                &[],
                include_untracked,
                &cancellation,
            )
            .await
            .expect("stash push");

            assert_eq!(outputs.last().expect("stash output").exit_code, 0);
            assert_eq!(
                fs::read_to_string(fixture.path().join("tracked.txt")).expect("tracked file"),
                "base\n"
            );
            assert_eq!(fixture.path().join("notes.md").exists(), !include_untracked);
            assert!(fixture.path().join("build.log").exists());
            let entries = list_stashes(&repository, fixture.path(), &cancellation)
                .await
                .expect("stash list");
            let mut paths = entries[0]
                .files
                .iter()
                .map(|file| file.path.as_str())
                .collect::<Vec<_>>();
            paths.sort_unstable();
            if include_untracked {
                assert_eq!(paths, ["notes.md", "tracked.txt"]);
            } else {
                assert_eq!(paths, ["tracked.txt"]);
            }
        }
    }

    #[tokio::test]
    async fn conflicting_pop_keeps_the_stash_entry() {
        let fixture = tempfile::tempdir().expect("temporary repository");
        git(fixture.path(), &["init", "-q", "-b", "main"]);
        git(fixture.path(), &["config", "user.name", "Git Manager Test"]);
        git(
            fixture.path(),
            &["config", "user.email", "git-manager@example.test"],
        );
        fs::write(fixture.path().join("tracked.txt"), "base\n").expect("base file");
        git(fixture.path(), &["add", "tracked.txt"]);
        git(fixture.path(), &["commit", "-q", "-m", "base"]);
        fs::write(fixture.path().join("tracked.txt"), "stashed\n").expect("stash content");
        git(fixture.path(), &["stash", "push", "-q", "-m", "saved"]);
        fs::write(fixture.path().join("tracked.txt"), "current\n").expect("current content");
        git(fixture.path(), &["commit", "-qam", "current"]);

        let repository = GitRepository::default();
        let cancellation = CancellationToken::new();
        let output = pop(&repository, fixture.path(), 0, &cancellation)
            .await
            .expect("conflicting pop returns Git output");

        assert_ne!(output.exit_code, 0);
        assert_eq!(
            stash_records(&repository, fixture.path(), &cancellation)
                .await
                .expect("stash remains readable")
                .len(),
            1
        );
    }
}
