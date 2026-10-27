use std::{
    collections::BTreeMap,
    fmt,
    future::Future,
    io,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

#[cfg(test)]
use std::sync::{
    Mutex, OnceLock,
    atomic::{AtomicUsize, Ordering},
};
#[cfg(test)]
use tokio::sync::Notify;

use crate::{
    crypto::sha256_hex,
    git::{canonical_worktree_path_key, host_path_platform, normalize_worktree_path_key},
};

const MAX_CATALOG_WORKTREES: usize = 512;
const MAX_FINGERPRINT_NAME_BYTES: usize = 255;
const MAX_FINGERPRINT_FILE_BYTES: usize = 64 * 1024;
const MAX_FINGERPRINT_TOTAL_BYTES: usize = 1024 * 1024;
const MAX_FINGERPRINT_REF_COMPONENTS: usize = 64;
const REFTABLE_REF_MARKER: &[u8] = b"this repository uses the reftable format\n";

#[derive(Clone)]
pub(crate) struct CatalogRepositoryFingerprint {
    digest: String,
    _common_dir: Arc<DirectoryLease>,
}

impl PartialEq for CatalogRepositoryFingerprint {
    fn eq(&self, other: &Self) -> bool {
        self.digest == other.digest
    }
}

impl Eq for CatalogRepositoryFingerprint {}

impl fmt::Debug for CatalogRepositoryFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("CatalogRepositoryFingerprint")
            .field(&self.digest)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct FileIdentity {
    volume: u64,
    file: u64,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct TrustedDirectoryIdentity {
    path_key: String,
    file: FileIdentity,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct KnownWorktreeProof {
    identity: String,
    admin_link: Option<(TrustedDirectoryIdentity, String)>,
}

struct DirectoryLease {
    handle: std::fs::File,
    identity: FileIdentity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FingerprintFailure {
    Cancelled,
    Unreadable,
    UntrustedLayout,
    Malformed,
    LimitExceeded,
    ChangedDuringRead,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum FingerprintOutcome {
    Known(CatalogRepositoryFingerprint),
    Unknown(FingerprintFailure),
}

#[derive(Clone)]
pub(crate) struct FingerprintRequest {
    pub common_dir: PathBuf,
    pub primary_path: PathBuf,
    pub known_worktree_paths: Vec<PathBuf>,
    pub repository_lifecycle_epoch: u64,
    pub mutation_epoch: u64,
    pub cancellation: CancellationToken,
}

pub(crate) async fn read_catalog_repository_fingerprint(
    request: FingerprintRequest,
) -> FingerprintOutcome {
    match read_fingerprint_once(request.clone()).await {
        Ok(first) => {
            #[cfg(test)]
            if let Err(failure) =
                pause_after_first_pass(&request.common_dir, &request.cancellation).await
            {
                return FingerprintOutcome::Unknown(failure);
            }
            match read_fingerprint_once(request).await {
                Ok(second) if first == second => FingerprintOutcome::Known(first),
                Ok(_) => FingerprintOutcome::Unknown(FingerprintFailure::ChangedDuringRead),
                Err(failure) => FingerprintOutcome::Unknown(failure),
            }
        }
        Err(failure) => FingerprintOutcome::Unknown(failure),
    }
}

#[cfg(test)]
#[derive(Clone)]
struct FirstPassPause {
    common_dir: PathBuf,
    reached: Arc<Notify>,
    resume: Arc<Notify>,
}

#[cfg(test)]
#[derive(Clone)]
struct AdminReadPause {
    common_dir: PathBuf,
    target: AdminReadTarget,
    before_open_reached: Arc<Notify>,
    resume_open: Arc<Notify>,
    after_read_reached: Arc<Notify>,
    resume_read: Arc<Notify>,
    remaining_reads: Arc<AtomicUsize>,
}

#[cfg(test)]
#[derive(Clone, Copy, Eq, PartialEq)]
enum AdminReadTarget {
    SymbolicRef,
    WorktreeEntry,
}

#[cfg(test)]
static FIRST_PASS_PAUSE: OnceLock<Mutex<Vec<FirstPassPause>>> = OnceLock::new();
#[cfg(test)]
static ADMIN_READ_PAUSE: OnceLock<Mutex<Vec<AdminReadPause>>> = OnceLock::new();

#[cfg(test)]
fn install_first_pass_pause(common_dir: PathBuf) -> FirstPassPause {
    let pause = FirstPassPause {
        common_dir,
        reached: Arc::new(Notify::new()),
        resume: Arc::new(Notify::new()),
    };
    FIRST_PASS_PAUSE
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .expect("fingerprint first-pass pause")
        .push(pause.clone());
    pause
}

#[cfg(test)]
fn install_admin_read_pause(
    common_dir: PathBuf,
    target: AdminReadTarget,
    reads: usize,
) -> AdminReadPause {
    assert!(reads > 0, "admin-read pause requires at least one read");
    let pause = AdminReadPause {
        common_dir,
        target,
        before_open_reached: Arc::new(Notify::new()),
        resume_open: Arc::new(Notify::new()),
        after_read_reached: Arc::new(Notify::new()),
        resume_read: Arc::new(Notify::new()),
        remaining_reads: Arc::new(AtomicUsize::new(reads)),
    };
    ADMIN_READ_PAUSE
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .expect("fingerprint admin-read pause")
        .push(pause.clone());
    pause
}

#[cfg(test)]
async fn pause_after_first_pass(
    common_dir: &Path,
    cancellation: &CancellationToken,
) -> Result<(), FingerprintFailure> {
    let pause = {
        let mut pauses = FIRST_PASS_PAUSE
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .expect("fingerprint first-pass pause");
        pauses
            .iter()
            .position(|pause| pause.common_dir == common_dir)
            .map(|index| pauses.remove(index))
    };
    if let Some(pause) = pause {
        pause.reached.notify_one();
        tokio::select! {
            () = pause.resume.notified() => {}
            () = cancellation.cancelled() => return Err(FingerprintFailure::Cancelled),
        }
    }
    Ok(())
}

#[cfg(test)]
async fn pause_admin_read(
    common_dir: &Path,
    target: AdminReadTarget,
    cancellation: &CancellationToken,
    after_read: bool,
) -> Result<(), FingerprintFailure> {
    let pause = {
        let pauses = ADMIN_READ_PAUSE
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .expect("fingerprint admin-read pause");
        pauses
            .iter()
            .find(|pause| pause.common_dir == common_dir && pause.target == target)
            .cloned()
    };
    let Some(pause) = pause else { return Ok(()) };
    let (reached, resume) = if after_read {
        (&pause.after_read_reached, &pause.resume_read)
    } else {
        (&pause.before_open_reached, &pause.resume_open)
    };
    reached.notify_one();
    tokio::select! {
        () = resume.notified() => {}
        () = cancellation.cancelled() => return Err(FingerprintFailure::Cancelled),
    }
    if after_read && pause.remaining_reads.fetch_sub(1, Ordering::AcqRel) == 1 {
        ADMIN_READ_PAUSE
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .expect("fingerprint admin-read pause")
            .retain(|candidate| candidate.common_dir != common_dir || candidate.target != target);
    }
    Ok(())
}

async fn read_fingerprint_once(
    request: FingerprintRequest,
) -> Result<CatalogRepositoryFingerprint, FingerprintFailure> {
    check_cancelled(&request.cancellation)?;
    if request.known_worktree_paths.len() > MAX_CATALOG_WORKTREES {
        return Err(FingerprintFailure::LimitExceeded);
    }
    let common_identity =
        trusted_directory_identity(&request.common_dir, &request.cancellation).await?;
    let common_lease =
        open_directory_lease(&request.common_dir, &common_identity, &request.cancellation).await?;
    let primary_identity =
        trusted_directory_identity(&request.primary_path, &request.cancellation).await?;
    validate_primary_layout(
        &request.primary_path,
        &primary_identity,
        &common_identity,
        &request.cancellation,
    )
    .await?;

    let mut proof = FingerprintProof::default();
    proof.push("version", b"coddy-studio.catalog-fingerprint.v1")?;
    proof.push(
        "repository-lifecycle",
        &request.repository_lifecycle_epoch.to_le_bytes(),
    )?;
    proof.push("mutation", &request.mutation_epoch.to_le_bytes())?;
    proof.push_directory_identity("common-identity", &common_identity)?;
    proof.push_directory_identity("primary-identity", &primary_identity)?;
    let mut known_identities = Vec::with_capacity(request.known_worktree_paths.len());
    for path in &request.known_worktree_paths {
        known_identities.push(
            read_worktree_path_identity(path, &common_identity, &request.cancellation).await?,
        );
    }
    known_identities.sort();
    known_identities.dedup();
    for (index, known) in known_identities.iter().enumerate() {
        proof.push(&format!("known-path-{index}"), known.identity.as_bytes())?;
        if let Some((admin, backlink)) = &known.admin_link {
            proof.push_directory_identity(&format!("known-path-{index}-admin"), admin)?;
            proof.push(
                &format!("known-path-{index}-admin-backlink"),
                backlink.as_bytes(),
            )?;
        }
    }
    let config = read_bounded_relative_file(
        common_lease.clone(),
        std::ffi::OsString::from("config"),
        true,
        &request.cancellation,
    )
    .await?
    .ok_or(FingerprintFailure::Unreadable)?;
    let reftable_config = config_proves_reftable(&config);
    proof.push("config", &config)?;
    let reftable_signature = read_reftable_directory_proof(
        common_lease.clone(),
        "reftable",
        &request.cancellation,
        &mut proof,
    )
    .await?;
    let head = read_bounded_relative_file(
        common_lease.clone(),
        std::ffi::OsString::from("HEAD"),
        true,
        &request.cancellation,
    )
    .await?
    .ok_or(FingerprintFailure::Unreadable)?;
    proof.push("HEAD", &head)?;
    if let Some(reference) = parse_symbolic_head(&head)? {
        proof.push_file(
            "HEAD-ref",
            read_bounded_admin_file(
                &request.common_dir,
                common_lease.clone(),
                reference,
                false,
                reftable_config && reftable_signature,
                &request.cancellation,
            )
            .await?,
        )?;
    }
    for name in ["config.worktree", "packed-refs"] {
        proof.push_file(
            name,
            read_bounded_relative_file(
                common_lease.clone(),
                std::ffi::OsString::from(name),
                false,
                &request.cancellation,
            )
            .await?,
        )?;
    }
    read_worktree_admin_proof(
        &request.common_dir,
        common_lease.clone(),
        reftable_config && reftable_signature,
        &known_identities,
        &request.cancellation,
        &mut proof,
    )
    .await?;
    check_cancelled(&request.cancellation)?;
    Ok(CatalogRepositoryFingerprint {
        digest: sha256_hex(proof.bytes),
        _common_dir: common_lease,
    })
}

async fn read_reftable_directory_proof(
    parent: Arc<DirectoryLease>,
    label: &str,
    cancellation: &CancellationToken,
    proof: &mut FingerprintProof,
) -> Result<bool, FingerprintFailure> {
    let opened = match open_relative_entry(
        parent.clone(),
        std::ffi::OsString::from("reftable"),
        cancellation,
    )
    .await?
    {
        Ok(opened) => opened,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            proof.push(label, b"<absent>")?;
            return Ok(false);
        }
        Err(error) => return Err(map_relative_open_failure(error)),
    };
    let opened = tokio::fs::File::from_std(opened);
    let metadata = cancellable_io_result(cancellation, opened.metadata())
        .await?
        .map_err(map_io_failure)?;
    if !metadata.is_dir() || is_reparse_point(&metadata) {
        return Err(FingerprintFailure::UntrustedLayout);
    }
    let identity = file_identity(&opened, &metadata)?;
    if identity.volume != parent.identity.volume {
        return Err(FingerprintFailure::UntrustedLayout);
    }
    let reftable = Arc::new(DirectoryLease {
        handle: opened.into_std().await,
        identity,
    });
    let tables_list = read_bounded_relative_file(
        reftable.clone(),
        std::ffi::OsString::from("tables.list"),
        true,
        cancellation,
    )
    .await?
    .ok_or(FingerprintFailure::Unreadable)?;
    proof.push(&format!("{label}-tables.list"), &tables_list)?;
    let tables_list =
        std::str::from_utf8(&tables_list).map_err(|_| FingerprintFailure::UntrustedLayout)?;
    let mut names = Vec::new();
    for name in tables_list.lines() {
        if names.len() == MAX_CATALOG_WORKTREES {
            return Err(FingerprintFailure::LimitExceeded);
        }
        if name.is_empty()
            || name.len() > MAX_FINGERPRINT_NAME_BYTES
            || !name.ends_with(".ref")
            || !matches!(
                Path::new(&name).components().next(),
                Some(Component::Normal(_))
            )
            || Path::new(&name).components().count() != 1
            || names.contains(&name)
        {
            return Err(FingerprintFailure::UntrustedLayout);
        }
        names.push(name);
    }
    proof.push(
        &format!("{label}-count"),
        &(names.len() as u64).to_le_bytes(),
    )?;
    for (index, name) in names.iter().enumerate() {
        let contents = read_bounded_relative_file(
            reftable.clone(),
            std::ffi::OsString::from(name),
            true,
            cancellation,
        )
        .await?
        .ok_or(FingerprintFailure::Unreadable)?;
        if !valid_reftable_table(&contents) {
            return Err(FingerprintFailure::UntrustedLayout);
        }
        proof.push(&format!("{label}-{index}-name"), name.as_bytes())?;
        proof.push(&format!("{label}-{index}-contents"), &contents)?;
    }
    Ok(true)
}

fn valid_reftable_table(contents: &[u8]) -> bool {
    contents.starts_with(b"REFT")
        && ([68_usize, 100].into_iter().any(|footer| {
            contents.len() >= footer && contents[contents.len() - footer..].starts_with(b"REFT")
        }))
}

async fn trusted_directory_identity(
    path: &Path,
    cancellation: &CancellationToken,
) -> Result<TrustedDirectoryIdentity, FingerprintFailure> {
    let before = cancellable_io_result(cancellation, tokio::fs::symlink_metadata(path))
        .await?
        .map_err(map_io_failure)?;
    if !before.is_dir() || before.file_type().is_symlink() || is_reparse_point(&before) {
        return Err(FingerprintFailure::UntrustedLayout);
    }
    let opened = open_no_follow_directory(path, cancellation).await?;
    let opened_metadata = cancellable_io_result(cancellation, opened.metadata())
        .await?
        .map_err(map_io_failure)?;
    if !opened_metadata.is_dir() || is_reparse_point(&opened_metadata) {
        return Err(FingerprintFailure::UntrustedLayout);
    }
    let file = file_identity(&opened, &opened_metadata)?;
    if !metadata_matches_file_identity(&before, file) {
        return Err(FingerprintFailure::ChangedDuringRead);
    }
    let path_key = cancellable_io_result(cancellation, canonical_worktree_path_key(path))
        .await?
        .map_err(map_io_failure)?;
    let canonical = cancellable_io_result(cancellation, tokio::fs::canonicalize(path))
        .await?
        .map_err(map_io_failure)?;
    if path_key != normalize_worktree_path_key(&canonical, host_path_platform())
        || path_key != normalize_worktree_path_key(path, host_path_platform())
    {
        return Err(FingerprintFailure::UntrustedLayout);
    }
    let after = cancellable_io_result(cancellation, tokio::fs::symlink_metadata(path))
        .await?
        .map_err(map_io_failure)?;
    let confirmed = open_no_follow_directory(path, cancellation).await?;
    let confirmed_metadata = cancellable_io_result(cancellation, confirmed.metadata())
        .await?
        .map_err(map_io_failure)?;
    if !same_metadata(&before, &after)
        || !confirmed_metadata.is_dir()
        || is_reparse_point(&confirmed_metadata)
        || file_identity(&confirmed, &confirmed_metadata)? != file
    {
        return Err(FingerprintFailure::ChangedDuringRead);
    }
    Ok(TrustedDirectoryIdentity { path_key, file })
}

async fn validate_primary_layout(
    primary_path: &Path,
    primary_identity: &TrustedDirectoryIdentity,
    common_identity: &TrustedDirectoryIdentity,
    cancellation: &CancellationToken,
) -> Result<(), FingerprintFailure> {
    let dot_git = primary_path.join(".git");
    match cancellable_io_result(cancellation, tokio::fs::symlink_metadata(&dot_git)).await? {
        Ok(metadata)
            if metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && !is_reparse_point(&metadata) =>
        {
            let identity = trusted_directory_identity(&dot_git, cancellation).await?;
            if identity != *common_identity {
                return Err(FingerprintFailure::UntrustedLayout);
            }
            Ok(())
        }
        Ok(metadata)
            if metadata.is_file()
                && !metadata.file_type().is_symlink()
                && !is_reparse_point(&metadata) =>
        {
            let contents = read_bounded_file(&dot_git, true, cancellation)
                .await?
                .ok_or(FingerprintFailure::Unreadable)?;
            let target = parse_dot_git_file(primary_path, &contents)?;
            validate_admin_directory(target.as_path(), common_identity, cancellation)
                .await
                .map(|_| ())
        }
        Err(error)
            if error.kind() == io::ErrorKind::NotFound && primary_identity == common_identity =>
        {
            Ok(())
        }
        Ok(_) => Err(FingerprintFailure::UntrustedLayout),
        Err(error) => Err(map_io_failure(error)),
    }
}

async fn read_worktree_path_identity(
    path: &Path,
    common_identity: &TrustedDirectoryIdentity,
    cancellation: &CancellationToken,
) -> Result<KnownWorktreeProof, FingerprintFailure> {
    check_cancelled(cancellation)?;
    match cancellable_io_result(cancellation, tokio::fs::symlink_metadata(path)).await? {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let identity = cancellable_io_result(cancellation, canonical_worktree_path_key(path))
                .await?
                .map_err(map_io_failure)?;
            Ok(KnownWorktreeProof {
                identity: format!("missing:{identity}"),
                admin_link: None,
            })
        }
        Err(error) => Err(map_io_failure(error)),
        Ok(metadata)
            if metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && !is_reparse_point(&metadata) =>
        {
            let identity = trusted_directory_identity(path, cancellation).await?;
            let dot_git = path.join(".git");
            let dot_git_metadata =
                match cancellable_io_result(cancellation, tokio::fs::symlink_metadata(&dot_git))
                    .await?
                {
                    Ok(metadata) => metadata,
                    Err(error)
                        if error.kind() == io::ErrorKind::NotFound
                            && identity == *common_identity =>
                    {
                        return Ok(KnownWorktreeProof {
                            identity: format_directory_identity("present", &identity),
                            admin_link: None,
                        });
                    }
                    Err(error) => return Err(map_io_failure(error)),
                };
            if dot_git_metadata.is_dir()
                && !dot_git_metadata.file_type().is_symlink()
                && !is_reparse_point(&dot_git_metadata)
            {
                if trusted_directory_identity(&dot_git, cancellation).await? != *common_identity {
                    return Err(FingerprintFailure::UntrustedLayout);
                }
                return Ok(KnownWorktreeProof {
                    identity: format_directory_identity("present", &identity),
                    admin_link: None,
                });
            }
            if !dot_git_metadata.is_file()
                || dot_git_metadata.file_type().is_symlink()
                || is_reparse_point(&dot_git_metadata)
            {
                return Err(FingerprintFailure::UntrustedLayout);
            }
            let contents = read_bounded_file(&dot_git, true, cancellation)
                .await?
                .ok_or(FingerprintFailure::Unreadable)?;
            let admin_dir = parse_dot_git_file(path, &contents)?;
            let admin = validate_admin_directory(&admin_dir, common_identity, cancellation).await?;
            let backlink =
                cancellable_io_result(cancellation, canonical_worktree_path_key(&dot_git))
                    .await?
                    .map_err(map_io_failure)?;
            Ok(KnownWorktreeProof {
                identity: format_directory_identity("present", &identity),
                admin_link: Some((admin, backlink)),
            })
        }
        Ok(_) => Err(FingerprintFailure::UntrustedLayout),
    }
}

async fn read_worktree_admin_proof(
    common_dir: &Path,
    common_lease: Arc<DirectoryLease>,
    reftable_marker_allowed: bool,
    known_worktrees: &[KnownWorktreeProof],
    cancellation: &CancellationToken,
    proof: &mut FingerprintProof,
) -> Result<(), FingerprintFailure> {
    let mut known_admin_links = BTreeMap::new();
    for (admin, backlink) in known_worktrees
        .iter()
        .filter_map(|known| known.admin_link.as_ref())
    {
        if known_admin_links.insert(admin.clone(), backlink).is_some() {
            return Err(FingerprintFailure::UntrustedLayout);
        }
    }
    let worktrees = common_dir.join("worktrees");
    let before =
        match cancellable_io_result(cancellation, tokio::fs::symlink_metadata(&worktrees)).await? {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                proof.push("worktrees", b"<absent>")?;
                return Ok(());
            }
            Err(error) => return Err(map_io_failure(error)),
        };
    if !before.is_dir() || before.file_type().is_symlink() || is_reparse_point(&before) {
        return Err(FingerprintFailure::UntrustedLayout);
    }
    let worktrees_identity = trusted_directory_identity(&worktrees, cancellation).await?;
    let worktrees_lease =
        open_directory_lease(&worktrees, &worktrees_identity, cancellation).await?;
    let entries = read_directory_names(&worktrees, worktrees_lease.clone(), cancellation).await?;
    if entries.len() > MAX_CATALOG_WORKTREES {
        return Err(FingerprintFailure::LimitExceeded);
    }
    let mut entries = entries
        .into_iter()
        .map(|name| {
            let value = name
                .to_str()
                .ok_or(FingerprintFailure::Malformed)?
                .to_owned();
            if value.is_empty()
                || value.len() > MAX_FINGERPRINT_NAME_BYTES
                || Path::new(&value).components().count() != 1
                || !matches!(
                    Path::new(&value).components().next(),
                    Some(Component::Normal(_))
                )
            {
                return Err(FingerprintFailure::LimitExceeded);
            }
            Ok((value, name))
        })
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    proof.push("worktrees", &(entries.len() as u64).to_le_bytes())?;
    for (index, (name, entry_name)) in entries.into_iter().enumerate() {
        check_cancelled(cancellation)?;
        let opened = open_relative_entry(worktrees_lease.clone(), entry_name, cancellation)
            .await?
            .map_err(map_relative_open_failure)?;
        let opened = tokio::fs::File::from_std(opened);
        let metadata = cancellable_io_result(cancellation, opened.metadata())
            .await?
            .map_err(map_io_failure)?;
        if !metadata.is_dir() || is_reparse_point(&metadata) {
            return Err(FingerprintFailure::UntrustedLayout);
        }
        let identity = file_identity(&opened, &metadata)?;
        if identity.volume != worktrees_lease.identity.volume {
            return Err(FingerprintFailure::UntrustedLayout);
        }
        let entry_lease = Arc::new(DirectoryLease {
            handle: opened.into_std().await,
            identity,
        });
        let entry_identity = TrustedDirectoryIdentity {
            path_key: normalize_worktree_path_key(&worktrees.join(&name), host_path_platform()),
            file: identity,
        };
        proof.push(&format!("worktree-{index}-name"), name.as_bytes())?;
        proof.push_directory_identity(&format!("worktree-{index}-identity"), &entry_identity)?;
        #[cfg(test)]
        pause_admin_read(
            common_dir,
            AdminReadTarget::WorktreeEntry,
            cancellation,
            false,
        )
        .await?;
        let head = read_bounded_relative_file(
            entry_lease.clone(),
            std::ffi::OsString::from("HEAD"),
            true,
            cancellation,
        )
        .await?
        .ok_or(FingerprintFailure::Unreadable)?;
        proof.push(&format!("worktree-{index}-HEAD"), &head)?;
        if let Some(reference) = parse_symbolic_head(&head)? {
            proof.push_file(
                &format!("worktree-{index}-HEAD-ref"),
                read_bounded_admin_file(
                    common_dir,
                    common_lease.clone(),
                    reference,
                    false,
                    reftable_marker_allowed,
                    cancellation,
                )
                .await?,
            )?;
        }
        let gitdir = read_bounded_relative_file(
            entry_lease.clone(),
            std::ffi::OsString::from("gitdir"),
            true,
            cancellation,
        )
        .await?
        .ok_or(FingerprintFailure::Unreadable)?;
        proof.push(&format!("worktree-{index}-gitdir"), &gitdir)?;
        let gitdir_identity = parse_admin_gitdir(&gitdir, cancellation).await?;
        if let Some(expected_backlink) = known_admin_links.remove(&entry_identity)
            && gitdir_identity != *expected_backlink
        {
            return Err(FingerprintFailure::UntrustedLayout);
        }
        proof.push(
            &format!("worktree-{index}-gitdir-identity"),
            gitdir_identity.as_bytes(),
        )?;
        proof.push_file(
            &format!("worktree-{index}-locked"),
            read_bounded_relative_file(
                entry_lease.clone(),
                std::ffi::OsString::from("locked"),
                false,
                cancellation,
            )
            .await?,
        )?;
        proof.push_file(
            &format!("worktree-{index}-config.worktree"),
            read_bounded_relative_file(
                entry_lease.clone(),
                std::ffi::OsString::from("config.worktree"),
                false,
                cancellation,
            )
            .await?,
        )?;
        #[cfg(test)]
        pause_admin_read(
            common_dir,
            AdminReadTarget::WorktreeEntry,
            cancellation,
            true,
        )
        .await?;
        read_reftable_directory_proof(
            entry_lease,
            &format!("worktree-{index}-reftable"),
            cancellation,
            proof,
        )
        .await?;
    }
    if !known_admin_links.is_empty() {
        return Err(FingerprintFailure::ChangedDuringRead);
    }
    let after = cancellable_io_result(cancellation, tokio::fs::symlink_metadata(&worktrees))
        .await?
        .map_err(map_io_failure)?;
    if !same_metadata(&before, &after)
        || trusted_directory_identity(&worktrees, cancellation).await? != worktrees_identity
    {
        return Err(FingerprintFailure::ChangedDuringRead);
    }
    Ok(())
}

async fn read_directory_names(
    path: &Path,
    lease: Arc<DirectoryLease>,
    cancellation: &CancellationToken,
) -> Result<Vec<std::ffi::OsString>, FingerprintFailure> {
    let path = path.to_path_buf();
    let task = tokio::task::spawn_blocking(move || directory_names_blocking(&path, &lease.handle));
    tokio::select! {
        biased;
        () = cancellation.cancelled() => Err(FingerprintFailure::Cancelled),
        result = task => result
            .map_err(|_| FingerprintFailure::Unreadable)?
            .map_err(map_io_failure),
    }
}

#[cfg(windows)]
fn directory_names_blocking(
    path: &Path,
    _lease: &std::fs::File,
) -> io::Result<Vec<std::ffi::OsString>> {
    // Both this directory and its common-directory parent deny delete sharing while the
    // pathname-based Windows enumerator is open, so neither name can be rebound here.
    std::fs::read_dir(path)?
        .take(MAX_CATALOG_WORKTREES + 1)
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect()
}

#[cfg(unix)]
fn finish_directory_names(
    enumeration: io::Result<Vec<std::ffi::OsString>>,
    close_error: Option<io::Error>,
) -> io::Result<Vec<std::ffi::OsString>> {
    match enumeration {
        Err(error) => Err(error),
        Ok(names) => close_error.map_or(Ok(names), Err),
    }
}

#[cfg(target_os = "linux")]
fn unix_errno_location() -> *mut libc::c_int {
    // SAFETY: libc returns the calling thread's live errno storage.
    unsafe { libc::__errno_location() }
}

#[cfg(target_os = "macos")]
fn unix_errno_location() -> *mut libc::c_int {
    // SAFETY: libc returns the calling thread's live errno storage.
    unsafe { libc::__error() }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn directory_names_blocking(
    _path: &Path,
    lease: &std::fs::File,
) -> io::Result<Vec<std::ffi::OsString>> {
    use std::{
        ffi::CStr,
        os::{
            fd::{AsRawFd, FromRawFd},
            unix::ffi::OsStringExt,
        },
    };

    // SAFETY: the lease is a live directory descriptor and `.` is a stable self-reference.
    let duplicate = unsafe {
        libc::openat(
            lease.as_raw_fd(),
            c".".as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW,
        )
    };
    if duplicate < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fdopendir takes ownership of the fresh descriptor only on success.
    let directory = unsafe { libc::fdopendir(duplicate) };
    if directory.is_null() {
        // SAFETY: fdopendir failed, so this function still owns the descriptor.
        unsafe { drop(std::fs::File::from_raw_fd(duplicate)) };
        return Err(io::Error::last_os_error());
    }
    let mut names = Vec::new();
    let mut enumeration_error = None;
    loop {
        // SAFETY: the pointer addresses this thread's live errno storage.
        unsafe { *unix_errno_location() = 0 };
        // SAFETY: the directory stream remains live until closed below.
        let entry = unsafe { libc::readdir(directory) };
        if entry.is_null() {
            // SAFETY: the pointer addresses this thread's live errno storage.
            let errno = unsafe { *unix_errno_location() };
            if errno != 0 {
                enumeration_error = Some(io::Error::from_raw_os_error(errno));
            }
            break;
        }
        // SAFETY: d_name is NUL-terminated for a returned directory entry.
        let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if bytes != b"." && bytes != b".." {
            names.push(std::ffi::OsString::from_vec(bytes.to_vec()));
            if names.len() > MAX_CATALOG_WORKTREES {
                break;
            }
        }
    }
    // SAFETY: fdopendir returned this stream and it is closed exactly once.
    let close_error = (unsafe { libc::closedir(directory) } != 0).then(io::Error::last_os_error);
    finish_directory_names(
        enumeration_error.map_or_else(|| Ok(names), Err),
        close_error,
    )
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn directory_names_blocking(
    _path: &Path,
    _lease: &std::fs::File,
) -> io::Result<Vec<std::ffi::OsString>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "handle-bound directory enumeration is unsupported",
    ))
}

#[cfg(not(any(unix, windows)))]
fn directory_names_blocking(
    _path: &Path,
    _lease: &std::fs::File,
) -> io::Result<Vec<std::ffi::OsString>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "handle-bound directory enumeration is unsupported",
    ))
}

fn parse_dot_git_file(worktree: &Path, contents: &[u8]) -> Result<PathBuf, FingerprintFailure> {
    let value = parse_single_line(contents)?;
    let raw = value
        .strip_prefix("gitdir: ")
        .ok_or(FingerprintFailure::Malformed)?;
    let target = Path::new(raw);
    Ok(if target.is_absolute() {
        target.to_path_buf()
    } else {
        worktree.join(target)
    })
}

async fn validate_admin_directory(
    admin_dir: &Path,
    common_identity: &TrustedDirectoryIdentity,
    cancellation: &CancellationToken,
) -> Result<TrustedDirectoryIdentity, FingerprintFailure> {
    let identity = trusted_directory_identity(admin_dir, cancellation).await?;
    let worktrees_identity = normalize_worktree_path_key(
        &Path::new(&common_identity.path_key).join("worktrees"),
        host_path_platform(),
    );
    let prefix = format!("{worktrees_identity}/");
    let suffix = identity
        .path_key
        .strip_prefix(&prefix)
        .ok_or(FingerprintFailure::UntrustedLayout)?;
    if suffix.is_empty() || suffix.contains('/') {
        return Err(FingerprintFailure::UntrustedLayout);
    }
    Ok(identity)
}

async fn parse_admin_gitdir(
    contents: &[u8],
    cancellation: &CancellationToken,
) -> Result<String, FingerprintFailure> {
    let value = parse_single_line(contents)?;
    let path = Path::new(value);
    if !path.is_absolute() || path.file_name().and_then(|name| name.to_str()) != Some(".git") {
        return Err(FingerprintFailure::Malformed);
    }
    cancellable_io_result(cancellation, canonical_worktree_path_key(path))
        .await?
        .map_err(map_io_failure)
}

fn parse_single_line(contents: &[u8]) -> Result<&str, FingerprintFailure> {
    let value = std::str::from_utf8(contents).map_err(|_| FingerprintFailure::Malformed)?;
    let value = value.strip_suffix('\n').unwrap_or(value);
    let value = value.strip_suffix('\r').unwrap_or(value);
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(FingerprintFailure::Malformed);
    }
    Ok(value)
}

async fn read_bounded_admin_file(
    _common_dir: &Path,
    common_lease: Arc<DirectoryLease>,
    relative: &Path,
    required: bool,
    reftable_marker_allowed: bool,
    cancellation: &CancellationToken,
) -> Result<Option<Vec<u8>>, FingerprintFailure> {
    let components = relative
        .components()
        .map(|component| match component {
            Component::Normal(name) if !name.is_empty() => Ok(name.to_os_string()),
            _ => Err(FingerprintFailure::Malformed),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if relative.is_absolute()
        || components.len() < 2
        || components.len() > MAX_FINGERPRINT_REF_COMPONENTS
        || components
            .iter()
            .any(|name| name.to_string_lossy().len() > MAX_FINGERPRINT_NAME_BYTES)
    {
        return Err(FingerprintFailure::Malformed);
    }
    let mut current = common_lease.clone();
    let mut leases = vec![common_lease];
    for (index, name) in components[..components.len() - 1].iter().enumerate() {
        let opened = match open_relative_entry(current.clone(), name.clone(), cancellation).await? {
            Ok(opened) => opened,
            Err(error) if error.kind() == io::ErrorKind::NotFound && !required => return Ok(None),
            Err(error) => return Err(map_relative_open_failure(error)),
        };
        let opened = tokio::fs::File::from_std(opened);
        let metadata = cancellable_io_result(cancellation, opened.metadata())
            .await?
            .map_err(map_io_failure)?;
        if metadata.is_dir() && !is_reparse_point(&metadata) {
            let identity = file_identity(&opened, &metadata)?;
            if identity.volume != leases[0].identity.volume {
                return Err(FingerprintFailure::UntrustedLayout);
            }
            let lease = Arc::new(DirectoryLease {
                handle: opened.into_std().await,
                identity,
            });
            current = lease.clone();
            leases.push(lease);
            continue;
        }
        let exact_reftable_marker = !required
            && reftable_marker_allowed
            && index == 1
            && components.len() >= 3
            && components[0] == "refs"
            && components[1] == "heads"
            && metadata.is_file()
            && !is_reparse_point(&metadata);
        if exact_reftable_marker {
            let contents = read_bounded_open_file(opened.into_std().await, cancellation).await?;
            if contents == REFTABLE_REF_MARKER {
                return Ok(None);
            }
        }
        return Err(FingerprintFailure::UntrustedLayout);
    }
    #[cfg(test)]
    pause_admin_read(
        _common_dir,
        AdminReadTarget::SymbolicRef,
        cancellation,
        false,
    )
    .await?;
    let value = read_bounded_relative_file(
        current,
        components
            .last()
            .expect("validated admin reference")
            .clone(),
        required,
        cancellation,
    )
    .await?;
    #[cfg(test)]
    pause_admin_read(
        _common_dir,
        AdminReadTarget::SymbolicRef,
        cancellation,
        true,
    )
    .await?;
    Ok(value)
}

fn config_proves_reftable(config: &[u8]) -> bool {
    let Ok(config) = std::str::from_utf8(config) else {
        return false;
    };
    let mut section = String::new();
    let mut ref_storage = None;
    let mut repository_format = None;
    for raw_line in config.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') {
            let Some(name) = line
                .strip_prefix('[')
                .and_then(|line| line.strip_suffix(']'))
            else {
                return false;
            };
            section = name.trim().to_ascii_lowercase();
            if section.starts_with("include") || section.contains(['\"', '\\']) {
                return false;
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return false;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        if key.starts_with("include") || value.contains(['\r', '\n', '\\']) {
            return false;
        }
        match (section.as_str(), key.as_str()) {
            ("extensions", "refstorage") if ref_storage.replace(value).is_some() => return false,
            ("core", "repositoryformatversion") if repository_format.replace(value).is_some() => {
                return false;
            }
            _ => {}
        }
    }
    ref_storage.is_some_and(|value| value.eq_ignore_ascii_case("reftable"))
        && repository_format == Some("1")
}

async fn open_directory_lease(
    path: &Path,
    expected: &TrustedDirectoryIdentity,
    cancellation: &CancellationToken,
) -> Result<Arc<DirectoryLease>, FingerprintFailure> {
    let options = directory_lease_open_options();
    let opened = cancellable_io_result(cancellation, options.open(path))
        .await?
        .map_err(map_io_failure)?;
    let metadata = cancellable_io_result(cancellation, opened.metadata())
        .await?
        .map_err(map_io_failure)?;
    if !metadata.is_dir() || is_reparse_point(&metadata) {
        return Err(FingerprintFailure::UntrustedLayout);
    }
    let identity = file_identity(&opened, &metadata)?;
    if identity != expected.file {
        return Err(FingerprintFailure::ChangedDuringRead);
    }
    Ok(Arc::new(DirectoryLease {
        handle: opened.into_std().await,
        identity,
    }))
}

fn directory_lease_open_options() -> tokio::fs::OpenOptions {
    let mut options = tokio::fs::OpenOptions::new();
    #[cfg(unix)]
    {
        options.read(true);
        options.custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY,
            FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };

        options
            .access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options
}

async fn read_bounded_relative_file(
    parent: Arc<DirectoryLease>,
    name: std::ffi::OsString,
    required: bool,
    cancellation: &CancellationToken,
) -> Result<Option<Vec<u8>>, FingerprintFailure> {
    check_cancelled(cancellation)?;
    let opened = match open_relative_entry(parent, name, cancellation).await? {
        Ok(opened) => opened,
        Err(error) if error.kind() == io::ErrorKind::NotFound && !required => return Ok(None),
        Err(error) => return Err(map_relative_open_failure(error)),
    };
    read_bounded_open_file(opened, cancellation).await.map(Some)
}

async fn read_bounded_open_file(
    opened: std::fs::File,
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, FingerprintFailure> {
    let file = tokio::fs::File::from_std(opened);
    let before = cancellable_io_result(cancellation, file.metadata())
        .await?
        .map_err(map_io_failure)?;
    if !before.is_file() || is_reparse_point(&before) {
        return Err(FingerprintFailure::UntrustedLayout);
    }
    if before.len() > MAX_FINGERPRINT_FILE_BYTES as u64 {
        return Err(FingerprintFailure::LimitExceeded);
    }
    let identity = file_identity(&file, &before)?;
    let mut bytes = Vec::with_capacity(before.len() as usize);
    let mut limited = file.take(MAX_FINGERPRINT_FILE_BYTES as u64 + 1);
    tokio::select! {
        result = limited.read_to_end(&mut bytes) => result.map_err(map_io_failure)?,
        () = cancellation.cancelled() => return Err(FingerprintFailure::Cancelled),
    };
    if bytes.len() > MAX_FINGERPRINT_FILE_BYTES {
        return Err(FingerprintFailure::LimitExceeded);
    }
    let after = cancellable_io_result(cancellation, limited.get_ref().metadata())
        .await?
        .map_err(map_io_failure)?;
    if file_identity(limited.get_ref(), &after)? != identity || after.len() != bytes.len() as u64 {
        return Err(FingerprintFailure::ChangedDuringRead);
    }
    Ok(bytes)
}

async fn open_relative_entry(
    parent: Arc<DirectoryLease>,
    name: std::ffi::OsString,
    cancellation: &CancellationToken,
) -> Result<io::Result<std::fs::File>, FingerprintFailure> {
    let task =
        tokio::task::spawn_blocking(move || open_relative_entry_blocking(&parent.handle, &name));
    tokio::select! {
        biased;
        () = cancellation.cancelled() => Err(FingerprintFailure::Cancelled),
        result = task => result.map_err(|_| FingerprintFailure::Unreadable),
    }
}

#[cfg(unix)]
fn open_relative_entry_blocking(
    parent: &std::fs::File,
    name: &std::ffi::OsStr,
) -> io::Result<std::fs::File> {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    };

    let name = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "entry name contains NUL"))?;
    // SAFETY: the parent descriptor is retained by the caller, the name is NUL-terminated,
    // and O_NOFOLLOW prevents resolving a substituted leaf link.
    let descriptor = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if descriptor < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openat returned one owned descriptor, transferred exactly once.
    Ok(unsafe { std::fs::File::from_raw_fd(descriptor) })
}

#[cfg(windows)]
fn open_relative_entry_blocking(
    parent: &std::fs::File,
    name: &std::ffi::OsStr,
) -> io::Result<std::fs::File> {
    use std::{
        mem::size_of,
        os::windows::{
            ffi::OsStrExt,
            io::{AsRawHandle, FromRawHandle},
        },
    };
    use windows_sys::Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            FILE_OPEN, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT, NtCreateFile,
        },
    };
    use windows_sys::Win32::{
        Foundation::{HANDLE, OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError, UNICODE_STRING},
        Storage::FileSystem::{
            FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE,
            SYNCHRONIZE,
        },
        System::IO::IO_STATUS_BLOCK,
    };

    let mut name = name.encode_wide().collect::<Vec<_>>();
    if name.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "entry name contains NUL",
        ));
    }
    let name_bytes = name
        .len()
        .checked_mul(size_of::<u16>())
        .and_then(|length| u16::try_from(length).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "entry name is too long"))?;
    let unicode_name = UNICODE_STRING {
        Length: name_bytes,
        MaximumLength: name_bytes,
        Buffer: name.as_mut_ptr(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle(),
        ObjectName: std::ptr::from_ref(&unicode_name),
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: std::ptr::null(),
        SecurityQualityOfService: std::ptr::null(),
    };
    let mut status_block = IO_STATUS_BLOCK::default();
    let mut handle: HANDLE = std::ptr::null_mut();
    // SAFETY: all referenced storage remains alive for the call; a successful call returns
    // one owned handle, adopted exactly once below.
    let status = unsafe {
        NtCreateFile(
            std::ptr::from_mut(&mut handle),
            FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            std::ptr::from_ref(&attributes),
            std::ptr::from_mut(&mut status_block),
            std::ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            FILE_OPEN,
            FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
            std::ptr::null(),
            0,
        )
    };
    if status < 0 {
        // SAFETY: status is the NTSTATUS returned directly by NtCreateFile.
        let windows_error = unsafe { RtlNtStatusToDosError(status) };
        return Err(io::Error::from_raw_os_error(windows_error as i32));
    }
    // SAFETY: NtCreateFile succeeded and transferred one owned handle.
    Ok(unsafe { std::fs::File::from_raw_handle(handle) })
}

#[cfg(not(any(unix, windows)))]
fn open_relative_entry_blocking(
    _: &std::fs::File,
    _: &std::ffi::OsStr,
) -> io::Result<std::fs::File> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "handle-relative filesystem access is unsupported",
    ))
}

async fn read_bounded_file(
    path: &Path,
    required: bool,
    cancellation: &CancellationToken,
) -> Result<Option<Vec<u8>>, FingerprintFailure> {
    check_cancelled(cancellation)?;
    let before =
        match cancellable_io_result(cancellation, tokio::fs::symlink_metadata(path)).await? {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound && !required => return Ok(None),
            Err(error) => return Err(map_io_failure(error)),
        };
    if !before.is_file() || before.file_type().is_symlink() || is_reparse_point(&before) {
        return Err(FingerprintFailure::UntrustedLayout);
    }
    if before.len() > MAX_FINGERPRINT_FILE_BYTES as u64 {
        return Err(FingerprintFailure::LimitExceeded);
    }
    let file = open_no_follow_file(path, cancellation).await?;
    let opened = cancellable_io_result(cancellation, file.metadata())
        .await?
        .map_err(map_io_failure)?;
    if !opened.is_file() || !same_file_identity(&before, &opened) {
        return Err(FingerprintFailure::ChangedDuringRead);
    }
    let mut bytes = Vec::with_capacity(before.len() as usize);
    let mut limited = file.take(MAX_FINGERPRINT_FILE_BYTES as u64 + 1);
    tokio::select! {
        result = limited.read_to_end(&mut bytes) => result.map_err(map_io_failure)?,
        () = cancellation.cancelled() => return Err(FingerprintFailure::Cancelled),
    };
    check_cancelled(cancellation)?;
    if bytes.len() > MAX_FINGERPRINT_FILE_BYTES {
        return Err(FingerprintFailure::LimitExceeded);
    }
    let after = cancellable_io_result(cancellation, tokio::fs::symlink_metadata(path))
        .await?
        .map_err(map_io_failure)?;
    if !same_file_identity(&opened, &after) || after.len() != bytes.len() as u64 {
        return Err(FingerprintFailure::ChangedDuringRead);
    }
    Ok(Some(bytes))
}

fn no_follow_open_options() -> tokio::fs::OpenOptions {
    let mut options = tokio::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    #[cfg(windows)]
    options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    options
}

async fn open_no_follow_file(
    path: &Path,
    cancellation: &CancellationToken,
) -> Result<tokio::fs::File, FingerprintFailure> {
    let options = no_follow_open_options();
    cancellable_io_result(cancellation, options.open(path))
        .await?
        .map_err(map_io_failure)
}

fn no_follow_directory_open_options() -> tokio::fs::OpenOptions {
    let mut options = tokio::fs::OpenOptions::new();
    #[cfg(unix)]
    {
        options.read(true);
        options.custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };

        options
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options
}

async fn open_no_follow_directory(
    path: &Path,
    cancellation: &CancellationToken,
) -> Result<tokio::fs::File, FingerprintFailure> {
    let options = no_follow_directory_open_options();
    cancellable_io_result(cancellation, options.open(path))
        .await?
        .map_err(map_io_failure)
}

fn parse_symbolic_head(head: &[u8]) -> Result<Option<&Path>, FingerprintFailure> {
    let value = std::str::from_utf8(head).map_err(|_| FingerprintFailure::Malformed)?;
    let value = value.strip_suffix('\n').unwrap_or(value);
    let value = value.strip_suffix('\r').unwrap_or(value);
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(FingerprintFailure::Malformed);
    }
    let Some(reference) = value.strip_prefix("ref: ") else {
        return if matches!(value.len(), 40 | 64)
            && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            Ok(None)
        } else {
            Err(FingerprintFailure::Malformed)
        };
    };
    let path = Path::new(reference);
    if !reference.starts_with("refs/")
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(FingerprintFailure::Malformed);
    }
    Ok(Some(path))
}

#[derive(Default)]
struct FingerprintProof {
    bytes: Vec<u8>,
}

impl FingerprintProof {
    fn push(&mut self, label: &str, value: &[u8]) -> Result<(), FingerprintFailure> {
        let next = self
            .bytes
            .len()
            .checked_add(label.len())
            .and_then(|length| length.checked_add(value.len()))
            .and_then(|length| length.checked_add(16))
            .ok_or(FingerprintFailure::LimitExceeded)?;
        if next > MAX_FINGERPRINT_TOTAL_BYTES {
            return Err(FingerprintFailure::LimitExceeded);
        }
        self.bytes
            .extend_from_slice(&(label.len() as u64).to_le_bytes());
        self.bytes.extend_from_slice(label.as_bytes());
        self.bytes
            .extend_from_slice(&(value.len() as u64).to_le_bytes());
        self.bytes.extend_from_slice(value);
        Ok(())
    }

    fn push_file(&mut self, label: &str, value: Option<Vec<u8>>) -> Result<(), FingerprintFailure> {
        match value {
            Some(value) => self.push(label, &value),
            None => self.push(label, b"<absent>"),
        }
    }

    fn push_directory_identity(
        &mut self,
        label: &str,
        identity: &TrustedDirectoryIdentity,
    ) -> Result<(), FingerprintFailure> {
        self.push(&format!("{label}-path"), identity.path_key.as_bytes())?;
        self.push(
            &format!("{label}-volume"),
            &identity.file.volume.to_le_bytes(),
        )?;
        self.push(&format!("{label}-file"), &identity.file.file.to_le_bytes())
    }
}

fn format_directory_identity(label: &str, identity: &TrustedDirectoryIdentity) -> String {
    format!(
        "{label}:{}:{}:{}",
        identity.path_key, identity.file.volume, identity.file.file
    )
}

async fn cancellable_io_result<T>(
    cancellation: &CancellationToken,
    operation: impl Future<Output = io::Result<T>>,
) -> Result<io::Result<T>, FingerprintFailure> {
    tokio::select! {
        biased;
        () = cancellation.cancelled() => Err(FingerprintFailure::Cancelled),
        result = operation => Ok(result),
    }
}

fn check_cancelled(cancellation: &CancellationToken) -> Result<(), FingerprintFailure> {
    if cancellation.is_cancelled() {
        Err(FingerprintFailure::Cancelled)
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn file_identity(
    _file: &tokio::fs::File,
    metadata: &std::fs::Metadata,
) -> Result<FileIdentity, FingerprintFailure> {
    use std::os::unix::fs::MetadataExt;

    Ok(FileIdentity {
        volume: metadata.dev(),
        file: metadata.ino(),
    })
}

#[cfg(windows)]
fn file_identity(
    file: &tokio::fs::File,
    _metadata: &std::fs::Metadata,
) -> Result<FileIdentity, FingerprintFailure> {
    use std::{mem::MaybeUninit, os::windows::io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    // SAFETY: `information` is writable storage for the exact Win32 structure,
    // and `file` keeps the borrowed operating-system handle alive for the call.
    let succeeded =
        unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, information.as_mut_ptr()) };
    if succeeded == 0 {
        return Err(FingerprintFailure::Unreadable);
    }
    // SAFETY: a nonzero return means Windows initialized the structure.
    let information = unsafe { information.assume_init() };
    Ok(FileIdentity {
        volume: u64::from(information.dwVolumeSerialNumber),
        file: (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
    })
}

#[cfg(not(any(unix, windows)))]
fn file_identity(
    _file: &tokio::fs::File,
    _metadata: &std::fs::Metadata,
) -> Result<FileIdentity, FingerprintFailure> {
    Err(FingerprintFailure::UntrustedLayout)
}

#[cfg(unix)]
fn metadata_matches_file_identity(metadata: &std::fs::Metadata, identity: FileIdentity) -> bool {
    use std::os::unix::fs::MetadataExt;

    metadata.dev() == identity.volume && metadata.ino() == identity.file
}

#[cfg(not(unix))]
fn metadata_matches_file_identity(_: &std::fs::Metadata, _: FileIdentity) -> bool {
    true
}

fn map_io_failure(error: io::Error) -> FingerprintFailure {
    if error.kind() == io::ErrorKind::NotFound {
        FingerprintFailure::ChangedDuringRead
    } else {
        FingerprintFailure::Unreadable
    }
}

fn map_relative_open_failure(error: io::Error) -> FingerprintFailure {
    #[cfg(unix)]
    if error.raw_os_error() == Some(libc::ELOOP) {
        return FingerprintFailure::UntrustedLayout;
    }
    map_io_failure(error)
}

#[cfg(unix)]
fn same_file_identity(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_file_identity(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    left.len() == right.len()
        && left.file_type() == right.file_type()
        && left.modified().ok() == right.modified().ok()
}

#[cfg(unix)]
fn same_metadata(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    same_file_identity(left, right)
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
}

#[cfg(not(unix))]
fn same_metadata(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    same_file_identity(left, right)
}

#[cfg(windows)]
fn is_reparse_point(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_reparse_point(_: &std::fs::Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use std::{path::Path, path::PathBuf, process::Command};

    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    use super::{
        FingerprintOutcome, FingerprintRequest, MAX_CATALOG_WORKTREES, MAX_FINGERPRINT_FILE_BYTES,
        read_catalog_repository_fingerprint,
    };

    struct FingerprintFixture {
        root: TempDir,
        primary: PathBuf,
        linked: Option<PathBuf>,
    }

    impl FingerprintFixture {
        async fn new() -> Self {
            let root = tempfile::tempdir().expect("fingerprint fixture");
            let primary = root.path().join("primary");
            std::fs::create_dir(&primary).expect("primary directory");
            git(&primary, &["init", "--initial-branch=main"]);
            std::fs::write(primary.join("README.md"), "base\n").expect("fixture file");
            git(&primary, &["add", "README.md"]);
            git(&primary, &["commit", "-m", "initial"]);
            let primary = std::fs::canonicalize(primary).expect("canonical primary directory");
            Self {
                root,
                primary,
                linked: None,
            }
        }

        async fn new_reftable() -> Option<Self> {
            let root = tempfile::tempdir().expect("reftable fingerprint fixture");
            let primary = root.path().join("primary");
            std::fs::create_dir(&primary).expect("primary directory");
            if !try_git(
                &primary,
                &["init", "--initial-branch=main", "--ref-format=reftable"],
            ) {
                return None;
            }
            std::fs::write(primary.join("README.md"), "base\n").expect("fixture file");
            git(&primary, &["add", "README.md"]);
            git(&primary, &["commit", "-m", "initial"]);
            let primary = std::fs::canonicalize(primary).expect("canonical primary directory");
            Some(Self {
                root,
                primary,
                linked: None,
            })
        }

        async fn new_bare() -> Self {
            let root = tempfile::tempdir().expect("bare fingerprint fixture");
            let primary = root.path().join("bare.git");
            std::fs::create_dir(&primary).expect("bare directory");
            git(&primary, &["init", "--bare", "--initial-branch=main"]);
            let primary = std::fs::canonicalize(primary).expect("canonical bare directory");
            Self {
                root,
                primary,
                linked: None,
            }
        }

        async fn read(&self) -> FingerprintOutcome {
            let common_dir = tokio::fs::canonicalize(if self.primary.join(".git").is_dir() {
                self.primary.join(".git")
            } else {
                self.primary.clone()
            })
            .await
            .expect("canonical common directory");
            read_catalog_repository_fingerprint(FingerprintRequest {
                common_dir,
                primary_path: self.primary.clone(),
                known_worktree_paths: std::iter::once(self.primary.clone())
                    .chain(self.linked.iter().cloned())
                    .collect(),
                repository_lifecycle_epoch: 1,
                mutation_epoch: 2,
                cancellation: CancellationToken::new(),
            })
            .await
        }

        async fn read_known(&self) -> super::CatalogRepositoryFingerprint {
            match self.read().await {
                FingerprintOutcome::Known(value) => value,
                outcome => panic!("expected known fingerprint, got {outcome:?}"),
            }
        }

        fn add_worktree(&mut self) {
            let linked = self.root.path().join("feature");
            git(
                &self.primary,
                &[
                    "worktree",
                    "add",
                    "-b",
                    "feature",
                    linked.to_str().expect("UTF-8 fixture path"),
                ],
            );
            self.linked = Some(std::fs::canonicalize(linked).expect("canonical linked worktree"));
        }

        fn move_worktree(&mut self) {
            let current = self.linked.as_ref().expect("linked worktree");
            let moved = self.root.path().join("feature-moved");
            git(
                &self.primary,
                &[
                    "worktree",
                    "move",
                    current.to_str().expect("UTF-8 fixture path"),
                    moved.to_str().expect("UTF-8 fixture path"),
                ],
            );
            self.linked = Some(std::fs::canonicalize(moved).expect("canonical moved worktree"));
        }

        fn lock_worktree(&self) {
            git(
                &self.primary,
                &[
                    "worktree",
                    "lock",
                    self.linked
                        .as_ref()
                        .expect("linked worktree")
                        .to_str()
                        .expect("UTF-8 fixture path"),
                ],
            );
        }

        fn checkout_detached(&self) {
            git(
                self.linked.as_ref().expect("linked worktree"),
                &["checkout", "--detach"],
            );
        }

        fn commit(&self) {
            let linked = self.linked.as_ref().expect("linked worktree");
            std::fs::write(linked.join("feature.txt"), "feature\n").expect("feature file");
            git(linked, &["add", "feature.txt"]);
            git(linked, &["commit", "-m", "feature change"]);
        }

        fn commit_primary(&self) {
            std::fs::write(self.primary.join("primary.txt"), "next\n").expect("primary file");
            git(&self.primary, &["add", "primary.txt"]);
            git(&self.primary, &["commit", "-m", "primary change"]);
        }

        fn pack_refs(&self) {
            git(&self.primary, &["pack-refs", "--all", "--prune"]);
        }

        fn remove_worktree(&mut self) {
            let linked = self.linked.as_ref().expect("linked worktree").clone();
            git(
                &self.primary,
                &[
                    "worktree",
                    "unlock",
                    linked.to_str().expect("UTF-8 fixture path"),
                ],
            );
            git(
                &self.primary,
                &[
                    "worktree",
                    "remove",
                    "--force",
                    linked.to_str().expect("UTF-8 fixture path"),
                ],
            );
        }
    }

    fn git(cwd: &Path, args: &[&str]) {
        assert!(try_git(cwd, args), "git {args:?} failed");
    }

    fn try_git(cwd: &Path, args: &[&str]) -> bool {
        let output = Command::new("git")
            .current_dir(cwd)
            .args([
                "-c",
                "user.name=Coddy Studio Test",
                "-c",
                "user.email=test@coddy-studio.local",
            ])
            .args(args)
            .output()
            .expect("run git fixture command");
        if !output.status.success() {
            eprintln!(
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        output.status.success()
    }

    fn copy_directory(source: &Path, destination: &Path) {
        std::fs::create_dir(destination).expect("copied directory");
        for entry in std::fs::read_dir(source).expect("source directory") {
            let entry = entry.expect("source entry");
            let target = destination.join(entry.file_name());
            if entry.file_type().expect("source entry type").is_dir() {
                copy_directory(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).expect("copied file");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn directory_enumeration_preserves_read_error_over_close_error() {
        let error = super::finish_directory_names(
            Err::<Vec<std::ffi::OsString>, _>(std::io::Error::from_raw_os_error(41)),
            Some(std::io::Error::from_raw_os_error(42)),
        )
        .expect_err("directory enumeration error");

        assert_eq!(error.raw_os_error(), Some(41));
    }

    #[tokio::test]
    async fn stable_repo_has_stable_fingerprint() {
        let fixture = FingerprintFixture::new().await;
        let first = fixture.read().await;
        assert!(matches!(first, FingerprintOutcome::Known(_)), "{first:?}");
        assert_eq!(first, fixture.read().await);
    }

    #[tokio::test]
    async fn repository_recreated_at_the_same_path_changes_fingerprint() {
        let fixture = FingerprintFixture::new().await;
        let baseline = fixture.read_known().await;
        let baseline_digest = baseline.digest.clone();
        #[cfg(windows)]
        drop(baseline);
        let displaced = fixture.root.path().join("displaced-primary");
        std::fs::rename(&fixture.primary, &displaced).expect("displace primary repository");
        std::fs::create_dir(&fixture.primary).expect("replacement primary directory");
        copy_directory(&displaced.join(".git"), &fixture.primary.join(".git"));

        assert_ne!(baseline_digest, fixture.read_known().await.digest);
        #[cfg(not(windows))]
        drop(baseline);
    }

    #[tokio::test]
    async fn changes_after_add_move_lock_checkout_commit_and_remove() {
        let mut fixture = FingerprintFixture::new().await;
        let baseline = fixture.read_known().await;

        fixture.add_worktree();
        let after_add = fixture.read_known().await;
        assert_ne!(baseline, after_add);

        fixture.move_worktree();
        let after_move = fixture.read_known().await;
        assert_ne!(after_add, after_move);

        fixture.lock_worktree();
        let after_lock = fixture.read_known().await;
        assert_ne!(after_move, after_lock);

        fixture.checkout_detached();
        let after_checkout = fixture.read_known().await;
        assert_ne!(after_lock, after_checkout);

        fixture.commit();
        let after_commit = fixture.read_known().await;
        assert_ne!(after_checkout, after_commit);

        fixture.remove_worktree();
        assert_ne!(after_commit, fixture.read_known().await);
    }

    #[tokio::test]
    async fn bare_repo_has_a_stable_known_fingerprint() {
        let fixture = FingerprintFixture::new_bare().await;
        let first = fixture.read().await;
        assert!(matches!(first, FingerprintOutcome::Known(_)), "{first:?}");
        assert_eq!(first, fixture.read().await);
    }

    #[tokio::test]
    async fn packed_ref_signature_is_included() {
        let fixture = FingerprintFixture::new().await;
        let baseline = fixture.read_known().await;
        fixture.pack_refs();
        assert_ne!(baseline, fixture.read_known().await);
    }

    #[tokio::test]
    async fn reftable_signature_is_included_when_git_supports_it() {
        let Some(fixture) = FingerprintFixture::new_reftable().await else {
            return;
        };
        let baseline = fixture.read_known().await;
        fixture.commit_primary();
        assert_ne!(baseline, fixture.read_known().await);
    }

    #[tokio::test]
    async fn malformed_reftable_ref_marker_is_unknown() {
        let Some(fixture) = FingerprintFixture::new_reftable().await else {
            return;
        };
        std::fs::write(
            fixture.primary.join(".git").join("refs").join("heads"),
            "not a supported reftable marker\n",
        )
        .expect("malformed reftable marker");
        assert_eq!(
            fixture.read().await,
            FingerprintOutcome::Unknown(super::FingerprintFailure::UntrustedLayout)
        );
    }

    #[tokio::test]
    async fn reftable_marker_outside_refs_heads_is_unknown() {
        let Some(fixture) = FingerprintFixture::new_reftable().await else {
            return;
        };
        let common_dir = fixture.primary.join(".git");
        std::fs::write(common_dir.join("HEAD"), "ref: refs/marker/main\n")
            .expect("custom symbolic HEAD");
        std::fs::write(
            common_dir.join("refs").join("marker"),
            super::REFTABLE_REF_MARKER,
        )
        .expect("misplaced reftable marker");

        assert_eq!(
            fixture.read().await,
            FingerprintOutcome::Unknown(super::FingerprintFailure::UntrustedLayout)
        );
    }

    #[tokio::test]
    async fn reftable_marker_without_config_proof_is_unknown() {
        let Some(fixture) = FingerprintFixture::new_reftable().await else {
            return;
        };
        let config_path = fixture.primary.join(".git").join("config");
        let config = std::fs::read_to_string(&config_path).expect("reftable config");
        std::fs::write(
            config_path,
            config.replace("refstorage = reftable", "refstorage = files"),
        )
        .expect("config without reftable proof");

        assert_eq!(
            fixture.read().await,
            FingerprintOutcome::Unknown(super::FingerprintFailure::UntrustedLayout)
        );
    }

    #[tokio::test]
    async fn malformed_reftable_table_signature_is_unknown() {
        let Some(fixture) = FingerprintFixture::new_reftable().await else {
            return;
        };
        let reftable = fixture.primary.join(".git").join("reftable");
        let table = std::fs::read_to_string(reftable.join("tables.list"))
            .expect("reftable table list")
            .lines()
            .next()
            .expect("committed fixture table")
            .to_owned();
        std::fs::write(reftable.join(table), "not a reftable\n").expect("malformed reftable table");

        assert_eq!(
            fixture.read().await,
            FingerprintOutcome::Unknown(super::FingerprintFailure::UntrustedLayout)
        );
    }

    #[tokio::test]
    async fn reftable_marker_in_normal_repository_is_unknown() {
        let fixture = FingerprintFixture::new().await;
        let heads = fixture.primary.join(".git").join("refs").join("heads");
        std::fs::remove_dir_all(&heads).expect("remove normal heads directory");
        std::fs::write(&heads, super::REFTABLE_REF_MARKER).expect("false reftable marker");
        std::fs::create_dir(fixture.primary.join(".git").join("reftable"))
            .expect("false reftable directory");
        std::fs::write(
            fixture
                .primary
                .join(".git")
                .join("reftable")
                .join("tables.list"),
            "",
        )
        .expect("false reftable signature");

        assert_eq!(
            fixture.read().await,
            FingerprintOutcome::Unknown(super::FingerprintFailure::UntrustedLayout)
        );
    }

    #[tokio::test]
    async fn malformed_or_escaping_gitdir_is_unknown() {
        let mut fixture = FingerprintFixture::new().await;
        fixture.add_worktree();
        let common_dir = std::fs::canonicalize(fixture.primary.join(".git"))
            .expect("canonical common directory");
        std::fs::write(
            common_dir.join("worktrees").join("feature").join("gitdir"),
            "../../outside\n",
        )
        .expect("escaping gitdir");
        assert!(matches!(
            fixture.read().await,
            FingerprintOutcome::Unknown(_)
        ));
    }

    #[tokio::test]
    async fn linked_worktree_dot_git_retargeted_to_sibling_admin_entry_is_unknown() {
        let mut fixture = FingerprintFixture::new().await;
        fixture.add_worktree();
        let linked = fixture.linked.as_ref().expect("linked worktree");
        let sibling = fixture.root.path().join("sibling");
        git(
            &fixture.primary,
            &[
                "worktree",
                "add",
                "-b",
                "sibling",
                sibling.to_str().expect("UTF-8 sibling path"),
            ],
        );
        let baseline = fixture.read_known().await;

        std::fs::write(
            linked.join(".git"),
            std::fs::read(sibling.join(".git")).expect("sibling dot-git pointer"),
        )
        .expect("retarget linked dot-git pointer");

        assert!(matches!(
            fixture.read().await,
            FingerprintOutcome::Unknown(_)
        ));
        assert_ne!(fixture.read().await, FingerprintOutcome::Known(baseline));
    }

    #[tokio::test]
    async fn symbolic_ref_through_intermediate_directory_link_is_unknown() {
        let fixture = FingerprintFixture::new().await;
        let common_dir = fixture.primary.join(".git");
        let outside_refs = fixture.root.path().join("outside-refs");
        std::fs::create_dir_all(outside_refs.join("heads")).expect("outside refs directory");
        std::fs::copy(
            common_dir.join("refs").join("heads").join("main"),
            outside_refs.join("heads").join("main"),
        )
        .expect("outside main ref");
        std::fs::remove_dir_all(common_dir.join("refs")).expect("remove trusted refs directory");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside_refs, common_dir.join("refs"))
            .expect("intermediate refs symlink");
        #[cfg(windows)]
        junction::create(&outside_refs, common_dir.join("refs"))
            .expect("intermediate refs junction");

        assert!(matches!(
            fixture.read().await,
            FingerprintOutcome::Unknown(super::FingerprintFailure::UntrustedLayout)
        ));

        #[cfg(windows)]
        junction::delete(common_dir.join("refs")).expect("remove refs junction");
    }

    #[tokio::test]
    async fn transient_symbolic_ref_parent_swap_cannot_change_the_anchored_read() {
        let fixture = FingerprintFixture::new().await;
        let baseline = fixture.read_known().await;
        let common_dir = std::fs::canonicalize(fixture.primary.join(".git"))
            .expect("canonical common directory");
        let pause = super::install_admin_read_pause(
            common_dir.clone(),
            super::AdminReadTarget::SymbolicRef,
            2,
        );
        let reader = tokio::spawn(read_catalog_repository_fingerprint(FingerprintRequest {
            common_dir: common_dir.clone(),
            primary_path: fixture.primary.clone(),
            known_worktree_paths: vec![fixture.primary.clone()],
            repository_lifecycle_epoch: 1,
            mutation_epoch: 2,
            cancellation: CancellationToken::new(),
        }));
        let refs = common_dir.join("refs");
        let displaced = common_dir.join("refs-original");
        for _ in 0..2 {
            pause.before_open_reached.notified().await;
            let swapped = std::fs::rename(&refs, &displaced).is_ok();
            if swapped {
                std::fs::create_dir_all(refs.join("heads")).expect("replacement refs directory");
                std::fs::write(
                    refs.join("heads").join("main"),
                    format!("{}\n", "0".repeat(40)),
                )
                .expect("replacement main ref");
            }
            pause.resume_open.notify_one();
            pause.after_read_reached.notified().await;
            if swapped {
                std::fs::remove_dir_all(&refs).expect("remove replacement refs");
                std::fs::rename(&displaced, &refs).expect("restore original refs");
            }
            pause.resume_read.notify_one();
        }

        assert_eq!(
            reader.await.expect("fingerprint reader"),
            FingerprintOutcome::Known(baseline)
        );
    }

    #[tokio::test]
    async fn transient_worktree_entry_swap_is_anchored_or_rejected() {
        let mut fixture = FingerprintFixture::new().await;
        fixture.add_worktree();
        let baseline = fixture.read_known().await;
        let linked = fixture.linked.as_ref().expect("linked worktree").clone();
        let common_dir = std::fs::canonicalize(fixture.primary.join(".git"))
            .expect("canonical common directory");
        let pause = super::install_admin_read_pause(
            common_dir.clone(),
            super::AdminReadTarget::WorktreeEntry,
            if cfg!(windows) { 2 } else { 1 },
        );
        let reader = tokio::spawn(read_catalog_repository_fingerprint(FingerprintRequest {
            common_dir: common_dir.clone(),
            primary_path: fixture.primary.clone(),
            known_worktree_paths: vec![fixture.primary.clone(), linked],
            repository_lifecycle_epoch: 1,
            mutation_epoch: 2,
            cancellation: CancellationToken::new(),
        }));
        let entry = common_dir.join("worktrees").join("feature");
        let displaced = common_dir.join("worktrees").join("feature-original");
        for cycle in 0..if cfg!(windows) { 2 } else { 1 } {
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                pause.before_open_reached.notified(),
            )
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "fingerprint pass {cycle} did not reach worktree open; reader finished: {}",
                    reader.is_finished()
                )
            });
            let swapped = std::fs::rename(&entry, &displaced).is_ok();
            #[cfg(unix)]
            assert!(
                swapped,
                "fingerprint pass {cycle} must rebind the entry name"
            );
            #[cfg(windows)]
            assert!(
                !swapped,
                "fingerprint pass {cycle} must keep the leased entry name bound"
            );
            if swapped {
                copy_directory(&displaced, &entry);
                std::fs::write(entry.join("HEAD"), format!("{}\n", "0".repeat(40)))
                    .expect("replacement worktree HEAD");
            }
            pause.resume_open.notify_one();
            tokio::time::timeout(
                std::time::Duration::from_secs(10),
                pause.after_read_reached.notified(),
            )
            .await
            .unwrap_or_else(|_| panic!("fingerprint pass {cycle} did not finish worktree reads"));
            if swapped {
                std::fs::remove_dir_all(&entry).expect("remove replacement worktree entry");
                std::fs::rename(&displaced, &entry).expect("restore worktree entry");
            }
            pause.resume_read.notify_one();
        }

        let outcome = reader.await.expect("fingerprint reader");
        #[cfg(windows)]
        assert_eq!(outcome, FingerprintOutcome::Known(baseline));
        #[cfg(unix)]
        {
            drop(baseline);
            assert_eq!(
                outcome,
                FingerprintOutcome::Unknown(super::FingerprintFailure::ChangedDuringRead)
            );
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn retained_fingerprint_blocks_common_directory_rebinding_until_drop() {
        let fixture = FingerprintFixture::new().await;
        let fingerprint = fixture.read_known().await;
        let common_dir = fixture.primary.join(".git");
        let displaced = fixture.primary.join(".git-displaced");
        let rebound_while_retained = std::fs::rename(&common_dir, &displaced);
        if rebound_while_retained.is_ok() {
            std::fs::rename(&displaced, &common_dir).expect("restore unexpectedly rebound common");
        }
        assert!(
            rebound_while_retained.is_err(),
            "retained fingerprint must lease the common directory"
        );

        drop(fingerprint);
        std::fs::rename(&common_dir, &displaced).expect("released lease permits rebinding");
        std::fs::rename(&displaced, &common_dir).expect("restore released common directory");
    }

    #[tokio::test]
    async fn escaping_primary_dot_git_file_is_unknown() {
        let root = tempfile::tempdir().expect("escaping primary fixture");
        let common_dir = root.path().join("common.git");
        let primary = root.path().join("linked");
        std::fs::create_dir(&common_dir).expect("common directory");
        std::fs::create_dir(&primary).expect("linked directory");
        git(&common_dir, &["init", "--bare", "--initial-branch=main"]);
        std::fs::write(primary.join(".git"), "gitdir: ../../outside\n")
            .expect("escaping dot-git file");
        assert!(matches!(
            read_catalog_repository_fingerprint(FingerprintRequest {
                common_dir: std::fs::canonicalize(common_dir).expect("canonical common directory"),
                primary_path: primary,
                known_worktree_paths: Vec::new(),
                repository_lifecycle_epoch: 1,
                mutation_epoch: 2,
                cancellation: CancellationToken::new(),
            })
            .await,
            FingerprintOutcome::Unknown(_)
        ));
    }

    #[tokio::test]
    async fn cancellation_and_bounds_are_unknown_without_partial_proof() {
        let fixture = FingerprintFixture::new().await;
        let common_dir = std::fs::canonicalize(fixture.primary.join(".git"))
            .expect("canonical common directory");
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert_eq!(
            read_catalog_repository_fingerprint(FingerprintRequest {
                common_dir: common_dir.clone(),
                primary_path: fixture.primary.clone(),
                known_worktree_paths: Vec::new(),
                repository_lifecycle_epoch: 1,
                mutation_epoch: 2,
                cancellation,
            })
            .await,
            FingerprintOutcome::Unknown(super::FingerprintFailure::Cancelled)
        );

        assert_eq!(
            read_catalog_repository_fingerprint(FingerprintRequest {
                common_dir,
                primary_path: fixture.primary.clone(),
                known_worktree_paths: (0..=MAX_CATALOG_WORKTREES)
                    .map(|index| fixture.root.path().join(format!("missing-{index}")))
                    .collect(),
                repository_lifecycle_epoch: 1,
                mutation_epoch: 2,
                cancellation: CancellationToken::new(),
            })
            .await,
            FingerprintOutcome::Unknown(super::FingerprintFailure::LimitExceeded)
        );
    }

    #[tokio::test]
    async fn oversized_or_disappearing_required_inputs_are_unknown() {
        let fixture = FingerprintFixture::new().await;
        let common_dir = std::fs::canonicalize(fixture.primary.join(".git"))
            .expect("canonical common directory");
        std::fs::write(
            common_dir.join("config.worktree"),
            vec![b'x'; MAX_FINGERPRINT_FILE_BYTES + 1],
        )
        .expect("oversized optional input");
        assert_eq!(
            fixture.read().await,
            FingerprintOutcome::Unknown(super::FingerprintFailure::LimitExceeded)
        );

        std::fs::remove_file(common_dir.join("config.worktree")).expect("remove oversized input");
        std::fs::remove_file(common_dir.join("config")).expect("remove required config");
        assert!(matches!(
            fixture.read().await,
            FingerprintOutcome::Unknown(_)
        ));
    }

    #[tokio::test]
    async fn malformed_head_is_unknown() {
        let fixture = FingerprintFixture::new().await;
        std::fs::write(
            fixture.primary.join(".git").join("HEAD"),
            "not-an-object-id\n",
        )
        .expect("malformed HEAD");
        assert_eq!(
            fixture.read().await,
            FingerprintOutcome::Unknown(super::FingerprintFailure::Malformed)
        );
    }

    #[tokio::test]
    async fn worktree_entry_count_overflow_never_produces_a_partial_fingerprint() {
        let fixture = FingerprintFixture::new().await;
        let worktrees = fixture.primary.join(".git").join("worktrees");
        std::fs::create_dir(&worktrees).expect("worktrees directory");
        for index in 0..=MAX_CATALOG_WORKTREES {
            std::fs::create_dir_all(worktrees.join(index.to_string())).expect("admin entry");
        }
        assert!(
            matches!(
                fixture.read().await,
                FingerprintOutcome::Unknown(
                    super::FingerprintFailure::LimitExceeded
                        | super::FingerprintFailure::ChangedDuringRead
                )
            ),
            "an over-limit worktree directory must fail closed without a partial fingerprint"
        );
    }

    #[tokio::test]
    async fn lifecycle_and_mutation_epochs_are_fingerprint_inputs() {
        let fixture = FingerprintFixture::new().await;
        let common_dir = std::fs::canonicalize(fixture.primary.join(".git"))
            .expect("canonical common directory");
        let read = |repository_lifecycle_epoch, mutation_epoch| {
            read_catalog_repository_fingerprint(FingerprintRequest {
                common_dir: common_dir.clone(),
                primary_path: fixture.primary.clone(),
                known_worktree_paths: Vec::new(),
                repository_lifecycle_epoch,
                mutation_epoch,
                cancellation: CancellationToken::new(),
            })
        };
        assert_ne!(read(1, 1).await, read(2, 1).await);
        assert_ne!(read(1, 1).await, read(1, 2).await);
    }

    #[tokio::test]
    async fn mutation_between_complete_passes_is_unknown() {
        let fixture = FingerprintFixture::new().await;
        let common_dir = std::fs::canonicalize(fixture.primary.join(".git"))
            .expect("canonical common directory");
        let pause = super::install_first_pass_pause(common_dir.clone());
        let reader = tokio::spawn(read_catalog_repository_fingerprint(FingerprintRequest {
            common_dir: common_dir.clone(),
            primary_path: fixture.primary.clone(),
            known_worktree_paths: Vec::new(),
            repository_lifecycle_epoch: 1,
            mutation_epoch: 2,
            cancellation: CancellationToken::new(),
        }));
        tokio::time::timeout(std::time::Duration::from_secs(1), pause.reached.notified())
            .await
            .expect("first fingerprint pass reaches the mutation pause");
        std::fs::write(common_dir.join("packed-refs"), "# pack-refs with: peeled\n")
            .expect("inter-pass mutation");
        pause.resume.notify_one();
        assert_eq!(
            reader.await.expect("fingerprint reader"),
            FingerprintOutcome::Unknown(super::FingerprintFailure::ChangedDuringRead)
        );
    }

    #[tokio::test]
    async fn cancellation_while_fingerprint_is_in_flight_is_prompt() {
        let fixture = FingerprintFixture::new().await;
        let common_dir = std::fs::canonicalize(fixture.primary.join(".git"))
            .expect("canonical common directory");
        let pause = super::install_first_pass_pause(common_dir.clone());
        let cancellation = CancellationToken::new();
        let mut reader = tokio::spawn(read_catalog_repository_fingerprint(FingerprintRequest {
            common_dir,
            primary_path: fixture.primary.clone(),
            known_worktree_paths: Vec::new(),
            repository_lifecycle_epoch: 1,
            mutation_epoch: 2,
            cancellation: cancellation.clone(),
        }));
        tokio::time::timeout(std::time::Duration::from_secs(1), pause.reached.notified())
            .await
            .expect("first fingerprint pass reaches the cancellation pause");
        cancellation.cancel();
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), &mut reader).await;
        if result.is_err() {
            pause.resume.notify_one();
        }
        assert_eq!(
            result
                .expect("in-flight cancellation should be prompt")
                .expect("fingerprint reader"),
            FingerprintOutcome::Unknown(super::FingerprintFailure::Cancelled)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlinked_admin_directory_is_unknown() {
        use std::os::unix::fs::symlink;

        let fixture = FingerprintFixture::new().await;
        let outside = fixture.root.path().join("outside-reftable");
        std::fs::create_dir(&outside).expect("outside reftable");
        symlink(&outside, fixture.primary.join(".git").join("reftable")).expect("reftable symlink");
        assert!(matches!(
            fixture.read().await,
            FingerprintOutcome::Unknown(super::FingerprintFailure::UntrustedLayout)
        ));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn junction_admin_directory_is_unknown() {
        let fixture = FingerprintFixture::new().await;
        let outside = fixture.root.path().join("outside-reftable");
        let linked = fixture.primary.join(".git").join("reftable");
        std::fs::create_dir(&outside).expect("outside reftable");
        if let Err(error) = junction::create(&outside, &linked) {
            eprintln!("skipping junction assertion: Windows denied junction creation: {error}");
            return;
        }
        assert!(matches!(
            fixture.read().await,
            FingerprintOutcome::Unknown(super::FingerprintFailure::UntrustedLayout)
        ));
        junction::delete(&linked).expect("remove fixture junction");
    }
}
