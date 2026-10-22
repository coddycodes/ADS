use std::{
    collections::{BTreeSet, HashMap, HashSet},
    ffi::OsString,
    fmt, fs,
    future::Future,
    io::{self, Write},
    path::{Component, Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::diagnostics::redact_sensitive_text;
use crate::source_control::{self, ProviderHosts};

use super::manager::graph::MAX_DIFF_BUFFER_SIZE;
use super::manager::in_progress::detect_in_progress_operation;
use super::worktree::{WorktreePruneDryRunRecord, parse_worktree_prune_dry_run};
use super::{
    ChangeRequest, CreateWorktreeInput, GitCommandDiagnostics, GitCommandError,
    GitPrunableWorktree, GitWorktreeInventory, GitWorktreeRecord, GitWorktreeRemovalInspection,
    ManagedWorktreeRollback, OutputPolicy, ProcessError, ProcessOutput, ProcessRequest,
    ProcessRunner, PullStatus, SourceControlProviderInfo, VcsCommit, VcsCreateWorktreeResult,
    VcsListCommitsResult, VcsListRefsResult, VcsPullResult, VcsRef, VcsRepositoryUnavailableReason,
    VcsStagingArea, VcsStatusLocalResult, VcsStatusRemoteResult, VcsStatusResult, VcsStatusSummary,
    VcsWorkingTree, VcsWorkingTreeFile, VcsWorkingTreeFileStatus, VcsWorktree,
    canonical_worktree_path_key, git_worktree_prune_impact_digest, host_path_platform,
    normalize_worktree_path_key, parse_numstat, parse_porcelain_v2_line, parse_worktree_porcelain,
};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const REPOSITORY_CLASSIFICATION_TIMEOUT: Duration = Duration::from_secs(2);
const DEFAULT_OUTPUT_LIMIT: usize = 1_000_000;
const SUMMARY_STATUS_OUTPUT_LIMIT: usize = 64 * 1024;
const WATCH_ROOTS_OUTPUT_LIMIT: usize = 16 * 1024;
const WORKTREE_INVENTORY_OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
const GIT_MANAGER_HISTORY_OUTPUT_LIMIT: usize = 16 * 1024 * 1024;
const GIT_MANAGER_TIPS_OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
const GIT_MANAGER_STASH_REFLOG_LIMIT: usize = 4 * 1024 * 1024;
const WORKTREE_REMOVAL_STATUS_OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
const WORKTREE_PRUNE_OUTPUT_LIMIT: usize = 4 * 1024 * 1024;
const WORKTREE_PRUNE_GITDIR_LIMIT: u64 = 64 * 1024;
const COMMIT_FIELD_SEPARATOR: char = '\x1f';
const CLONE_OPERATION: &str = "GitVcsDriver.clone";
/// Git's HTTP(S) stall guard for network transfers: stop when fewer than 1000 bytes/s are
/// transferred for 60 seconds. `-c` reaches Git's transport helpers through the
/// environment; transports without HTTP (SSH, local paths) ignore it.
const NETWORK_TRANSFER_CONFIG: &[&str] = &[
    "-c",
    "http.lowSpeedLimit=1000",
    "-c",
    "http.lowSpeedTime=60",
];
/// The fixed start of curl's low-speed abort, which Git relays on stderr.
const CURL_LOW_SPEED_ABORT: &str = "Operation too slow";
pub(crate) const NETWORK_TRANSFER_STALLED: &str = "The transfer stalled (under 1 KB/s for 60 seconds). Check the connection to the remote and try again.";
const MAX_AUTOMATIC_WORKTREE_SUFFIX_ATTEMPTS: usize = 100;
const WORKTREE_REMOVE_RETRY_ATTEMPTS: usize = 20;
const WORKTREE_REMOVE_RETRY_DELAY: Duration = Duration::from_millis(50);
const WORKTREE_REMOVAL_IDENTITY_FILE: &str = "bibcode-removal-identity";
const WORKTREE_REMOVAL_ROOT_IDENTITY_FILE: &str = ".bibcode-removal-identity";
const WORKTREE_REMOVAL_TOMBSTONE_FILE: &str = ".bibcode-removal-tombstone";

#[derive(Clone, Copy)]
struct GitExecutionOptions {
    allow_non_zero_exit: bool,
    max_output_bytes: usize,
    output_policy: OutputPolicy,
}

struct GitExecutionInput {
    environment: Vec<(OsString, OsString)>,
    stdin: Option<Vec<u8>>,
}

struct RewriteTempFiles {
    directory: PathBuf,
    todo: PathBuf,
    message: Option<PathBuf>,
}

impl Drop for RewriteTempFiles {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.todo);
        if let Some(message) = self.message.as_ref() {
            let _ = fs::remove_file(message);
        }
        let _ = fs::remove_dir(&self.directory);
    }
}

pub(crate) type BoxGitProcessFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ProcessOutput, ProcessError>> + Send + 'a>>;

pub(crate) trait GitProcessRunner: Send + Sync {
    fn run<'a>(
        &'a self,
        request: ProcessRequest,
        cancellation: &'a CancellationToken,
    ) -> BoxGitProcessFuture<'a>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitManagerCommitOutcome {
    pub sha: Option<String>,
    pub empty: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitManagerHeadCommit {
    pub sha: String,
    pub parents: Vec<String>,
    pub message: String,
}

impl GitProcessRunner for ProcessRunner {
    fn run<'a>(
        &'a self,
        request: ProcessRequest,
        cancellation: &'a CancellationToken,
    ) -> BoxGitProcessFuture<'a> {
        Box::pin(ProcessRunner::run(self, request, cancellation))
    }
}

pub type BoxWorktreeBaseDirectoryFuture<'a> =
    Pin<Box<dyn Future<Output = Option<PathBuf>> + Send + 'a>>;

pub trait WorktreeBaseDirectoryProvider: Send + Sync {
    fn worktree_base_directory<'a>(&'a self) -> BoxWorktreeBaseDirectoryFuture<'a>;
}

#[derive(Debug, Default)]
struct DefaultWorktreeBaseDirectory;

impl WorktreeBaseDirectoryProvider for DefaultWorktreeBaseDirectory {
    fn worktree_base_directory<'a>(&'a self) -> BoxWorktreeBaseDirectoryFuture<'a> {
        Box::pin(async { None })
    }
}

#[derive(Clone)]
pub struct GitRepository {
    runner: Arc<dyn GitProcessRunner>,
    worktree_settings: Arc<dyn WorktreeBaseDirectoryProvider>,
    worktree_porcelain_z_supported: Arc<Mutex<Option<bool>>>,
    git_version: Arc<Mutex<Option<crate::git::GitVersion>>>,
    command_timeout: Duration,
    /// `-c` configuration placed before the subcommand of every command this variant runs.
    command_config: &'static [&'static str],
    /// Read only on failed Git discovery; tests replace this without changing process state.
    discovery_environment: fn(&str) -> Option<OsString>,
    /// Hosts identified by explicit provider probes; status reads only read it.
    /// `default()` starts with a private, empty observation, so status names only
    /// providers its host name identifies. That is safe where provider identity
    /// is unused (project init, the review diff's repository check) and in tests.
    /// A repository whose status reaches the UI (VCS status, Git Manager, the create
    /// flow) must share the server's observation through `with_worktree_settings`
    /// or `with_provider_hosts`.
    provider_hosts: Arc<ProviderHosts>,
}

pub(crate) struct StatusObservation {
    local: VcsStatusLocalResult,
    remote: StatusRemoteObservation,
}

enum RepositoryProbe {
    WorkTree,
    NonWorkTree,
    Refused(String),
}

pub(crate) struct GitWatchRoots {
    pub worktree_root: PathBuf,
    pub git_dir: PathBuf,
    pub common_dir: PathBuf,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct GitManagerSignalHead {
    pub reference: Option<String>,
    pub oid: Option<String>,
}

pub(crate) struct ObservedRemoteStatus {
    pub ref_name: Option<String>,
    pub remote: Option<VcsStatusRemoteResult>,
}

struct StatusRemoteObservation {
    upstream_ref: Option<String>,
    ahead_count: u64,
    behind_count: u64,
}

impl Default for GitRepository {
    fn default() -> Self {
        Self {
            runner: Arc::new(ProcessRunner),
            worktree_settings: Arc::new(DefaultWorktreeBaseDirectory),
            worktree_porcelain_z_supported: Arc::new(Mutex::new(None)),
            git_version: Arc::new(Mutex::new(None)),
            command_timeout: DEFAULT_TIMEOUT,
            command_config: &[],
            discovery_environment: |name| std::env::var_os(name),
            provider_hosts: Arc::default(),
        }
    }
}

impl fmt::Debug for GitRepository {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitRepository")
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
struct WorktreePathPolicy {
    configured_base: Option<PathBuf>,
}

impl WorktreePathPolicy {
    fn path_for(&self, cwd: &Path, target_ref: &str) -> PathBuf {
        let repo = cwd
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("repository");
        let base = self
            .configured_base
            .clone()
            .unwrap_or_else(|| cwd.parent().unwrap_or(cwd).join(".bibcode-worktrees"));
        base.join(repo).join(target_ref.replace('/', "-"))
    }

    fn configured_base(&self) -> Option<&Path> {
        self.configured_base.as_deref()
    }
}

struct OwnedWorktreePath {
    path: PathBuf,
    identity: OwnedWorktreePathIdentity,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OwnedWorktreePathIdentity {
    device: u64,
    inode: u64,
}

#[cfg(windows)]
type OwnedWorktreePathIdentity = WindowsFileIdentity;

impl fmt::Debug for OwnedWorktreePath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OwnedWorktreePath")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WorktreePathReservationStage {
    Parent,
    Destination,
}

#[derive(Debug)]
struct WorktreePathReservationError {
    stage: WorktreePathReservationStage,
    source: io::Error,
}

impl WorktreePathReservationError {
    fn kind(&self) -> io::ErrorKind {
        self.source.kind()
    }

    fn is_destination_collision(&self) -> bool {
        self.stage == WorktreePathReservationStage::Destination
            && self.kind() == io::ErrorKind::AlreadyExists
    }
}

impl OwnedWorktreePath {
    fn reserve(path: PathBuf) -> Result<Self, WorktreePathReservationError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| WorktreePathReservationError {
                stage: WorktreePathReservationStage::Parent,
                source,
            })?;
        }
        let lease = create_removal_directory_lease(&path).map_err(|source| {
            WorktreePathReservationError {
                stage: WorktreePathReservationStage::Destination,
                source,
            }
        })?;
        let identity = match owned_worktree_path_identity(&lease) {
            Ok(identity) => identity,
            Err(source) => {
                let _ = delete_bound_directory_root_blocking(lease, &path);
                return Err(WorktreePathReservationError {
                    stage: WorktreePathReservationStage::Destination,
                    source,
                });
            }
        };
        drop(lease);
        Ok(Self { path, identity })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn identity(&self) -> OwnedWorktreePathIdentity {
        self.identity
    }
}

impl GitRepository {
    /// Only the owned checkout transaction uses this clone; ordinary Git operations
    /// retain their existing deadlines. The caller supplies its independent token.
    pub(crate) fn for_checkout_write(&self) -> Self {
        Self {
            command_timeout: super::CHECKOUT_WRITE_TIMEOUT,
            ..self.clone()
        }
    }

    /// Transfers the user can cancel (clone and the Git Manager's fetch, pull, push,
    /// remote-branch delete and tag push) run on this variant: a long safety bound replaces
    /// the read deadline and Git's HTTP stall guard is on, so a slow transfer that keeps
    /// moving finishes while a stalled one fails, and Cancel stops it. Credentials and the
    /// non-interactive environment are unchanged.
    pub(crate) fn for_network_transfer(&self) -> Self {
        Self {
            command_timeout: super::NETWORK_TRANSFER_TIMEOUT,
            command_config: NETWORK_TRANSFER_CONFIG,
            ..self.clone()
        }
    }

    /// Transfers no user can cancel (the automatic fetch, and the pull, stacked-action push
    /// and publish push that have no Cancel) keep the HTTP stall guard with a bounded
    /// deadline, because SSH transfers have no stall detection.
    pub(crate) fn for_bounded_transfer(&self) -> Self {
        Self {
            command_timeout: super::BOUNDED_TRANSFER_TIMEOUT,
            command_config: NETWORK_TRANSFER_CONFIG,
            ..self.clone()
        }
    }

    /// The server's repository, using its shared host observation for status reads.
    pub fn with_worktree_settings(
        worktree_settings: Arc<dyn WorktreeBaseDirectoryProvider>,
        provider_hosts: Arc<ProviderHosts>,
    ) -> Self {
        Self {
            runner: Arc::new(ProcessRunner),
            worktree_settings,
            worktree_porcelain_z_supported: Arc::new(Mutex::new(None)),
            git_version: Arc::new(Mutex::new(None)),
            command_timeout: DEFAULT_TIMEOUT,
            command_config: &[],
            discovery_environment: |name| std::env::var_os(name),
            provider_hosts,
        }
    }

    /// Shares the server's host observation, which provider probes fill elsewhere.
    #[must_use]
    pub fn with_provider_hosts(mut self, provider_hosts: Arc<ProviderHosts>) -> Self {
        self.provider_hosts = provider_hosts;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_runner_for_test(runner: Arc<dyn GitProcessRunner>) -> Self {
        Self {
            runner,
            worktree_settings: Arc::new(DefaultWorktreeBaseDirectory),
            worktree_porcelain_z_supported: Arc::new(Mutex::new(None)),
            git_version: Arc::new(Mutex::new(None)),
            command_timeout: DEFAULT_TIMEOUT,
            command_config: &[],
            discovery_environment: |name| std::env::var_os(name),
            provider_hosts: Arc::default(),
        }
    }

    /// Shortens the ordinary command deadline so a test can prove which commands outlive it.
    #[cfg(test)]
    pub(crate) fn with_command_timeout_for_test(self, command_timeout: Duration) -> Self {
        Self {
            command_timeout,
            ..self
        }
    }

    async fn worktree_path_policy(
        &self,
        cwd: &Path,
    ) -> Result<WorktreePathPolicy, GitCommandError> {
        let configured_base = self.worktree_settings.worktree_base_directory().await;
        if let Some(path) = configured_base.as_ref() {
            let available = tokio::fs::metadata(path)
                .await
                .map(|metadata| metadata.is_dir())
                .unwrap_or(false);
            if !available {
                return Err(workspace_unavailable_error(cwd, path));
            }
        }
        Ok(WorktreePathPolicy { configured_base })
    }

    async fn execute(
        &self,
        operation: &str,
        cwd: &Path,
        args: &[String],
        allow_non_zero_exit: bool,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.execute_with_options(
            operation,
            cwd,
            args,
            GitExecutionOptions {
                allow_non_zero_exit,
                max_output_bytes: DEFAULT_OUTPUT_LIMIT,
                output_policy: OutputPolicy::Truncate,
            },
            cancellation,
        )
        .await
    }

    async fn execute_with_options(
        &self,
        operation: &str,
        cwd: &Path,
        args: &[String],
        options: GitExecutionOptions,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.execute_with_environment(
            operation,
            cwd,
            args,
            options,
            git_environment(),
            cancellation,
        )
        .await
    }

    pub(crate) async fn execute_with_stdin(
        &self,
        operation: &str,
        cwd: &Path,
        args: &[String],
        stdin: Vec<u8>,
        allow_non_zero_exit: bool,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.execute_with_environment_and_stdin(
            operation,
            cwd,
            args,
            GitExecutionOptions {
                allow_non_zero_exit,
                max_output_bytes: DEFAULT_OUTPUT_LIMIT,
                output_policy: OutputPolicy::Truncate,
            },
            GitExecutionInput {
                environment: git_environment(),
                stdin: Some(stdin),
            },
            cancellation,
        )
        .await
    }

    async fn execute_read(
        &self,
        operation: &str,
        cwd: &Path,
        args: &[String],
        allow_non_zero_exit: bool,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.execute_with_environment(
            operation,
            cwd,
            args,
            GitExecutionOptions {
                allow_non_zero_exit,
                max_output_bytes: DEFAULT_OUTPUT_LIMIT,
                output_policy: OutputPolicy::Truncate,
            },
            git_read_environment(),
            cancellation,
        )
        .await
    }

    async fn execute_with_environment(
        &self,
        operation: &str,
        cwd: &Path,
        args: &[String],
        options: GitExecutionOptions,
        environment: Vec<(OsString, OsString)>,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.execute_with_environment_and_stdin(
            operation,
            cwd,
            args,
            options,
            GitExecutionInput {
                environment,
                stdin: None,
            },
            cancellation,
        )
        .await
    }

    async fn execute_with_environment_and_stdin(
        &self,
        operation: &str,
        cwd: &Path,
        args: &[String],
        options: GitExecutionOptions,
        input: GitExecutionInput,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        let args = self
            .command_config
            .iter()
            .map(OsString::from)
            .chain(args.iter().map(OsString::from))
            .collect::<Vec<_>>();
        let argument_count = args.len();
        self.runner
            .run(
                ProcessRequest {
                    operation: operation.to_owned(),
                    command: PathBuf::from("git"),
                    args,
                    cwd: cwd.to_path_buf(),
                    env: input.environment,
                    stdin: input.stdin,
                    timeout: self.command_timeout,
                    max_output_bytes: options.max_output_bytes,
                    output_policy: options.output_policy,
                    append_truncation_marker: false,
                    allow_non_zero_exit: options.allow_non_zero_exit,
                },
                cancellation,
            )
            .await
            .map_err(|error| git_error(operation, cwd, argument_count, error))
    }

    pub(crate) async fn git_manager_resolve_tips(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        let args = strings(&[
            "for-each-ref",
            "--format=%(refname)%09%(objectname)",
            "refs/heads",
            "refs/remotes",
            "refs/tags",
        ]);
        self.execute_with_environment(
            "GitManager.resolveTips",
            cwd,
            &args,
            GitExecutionOptions {
                allow_non_zero_exit: false,
                max_output_bytes: GIT_MANAGER_TIPS_OUTPUT_LIMIT,
                output_policy: OutputPolicy::Error,
            },
            git_read_environment(),
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_validate_tips(
        &self,
        cwd: &Path,
        tips: &[String],
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        let mut stdin = Vec::new();
        for tip in tips {
            stdin.extend_from_slice(tip.as_bytes());
            stdin.extend_from_slice(b"^{commit}\n");
        }
        self.execute_with_environment_and_stdin(
            "GitManager.validateTips",
            cwd,
            &strings(&["cat-file", "--batch-check=%(objectname) %(objecttype)"]),
            GitExecutionOptions {
                allow_non_zero_exit: false,
                max_output_bytes: GIT_MANAGER_TIPS_OUTPUT_LIMIT,
                output_policy: OutputPolicy::Error,
            },
            GitExecutionInput {
                environment: git_read_environment(),
                stdin: Some(stdin),
            },
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_log_page(
        &self,
        cwd: &Path,
        tips: &[String],
        degraded_to_all_paging: bool,
        offset: usize,
        limit: usize,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        let format = concat!(
            "--format=%x1e%H%x1f%h%x1f%s%x1f%b%x1f%an%x1f%ae%x1f%at%x1f",
            "%cn%x1f%ce%x1f%ct%x1f%P%x1f%D"
        );
        let mut args = vec![
            "log".to_owned(),
            "--no-show-signature".to_owned(),
            "--no-color".to_owned(),
            "-z".to_owned(),
            "--date=raw".to_owned(),
            "--name-only".to_owned(),
            format!("--skip={offset}"),
            format!("--max-count={limit}"),
            format.to_owned(),
        ];
        if degraded_to_all_paging {
            args.push("--all".to_owned());
        } else {
            args.extend(tips.iter().cloned());
        }
        args.push("--".to_owned());
        self.execute_with_environment(
            "GitManager.getCommits",
            cwd,
            &args,
            GitExecutionOptions {
                allow_non_zero_exit: false,
                max_output_bytes: GIT_MANAGER_HISTORY_OUTPUT_LIMIT,
                output_policy: OutputPolicy::Error,
            },
            git_read_environment(),
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_refs(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.git_manager_bounded_read(
            "GitManager.getRefs.refs",
            cwd,
            &strings(&[
                "for-each-ref",
                "--format=%(refname)%09%(objectname)%09%(upstream)%09%(worktreepath)%09%(HEAD)",
                "refs/heads",
                "refs/remotes",
                "refs/tags",
            ]),
            false,
            GIT_MANAGER_TIPS_OUTPUT_LIMIT,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_worktrees(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.git_manager_bounded_read(
            "GitManager.getRefs.worktrees",
            cwd,
            &strings(&["worktree", "list", "--porcelain"]),
            false,
            WORKTREE_INVENTORY_OUTPUT_LIMIT,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_ahead_behind(
        &self,
        cwd: &Path,
        local: &str,
        upstream: &str,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        let range = format!("refs/heads/{local}...{upstream}");
        self.git_manager_bounded_read(
            "GitManager.getRefs.aheadBehind",
            cwd,
            &[
                "rev-list".into(),
                "--left-right".into(),
                "--count".into(),
                range,
                "--".into(),
            ],
            false,
            64 * 1024,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_head_ref(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.git_manager_bounded_read(
            "GitManager.getRefs.headRef",
            cwd,
            &strings(&["symbolic-ref", "--quiet", "--short", "HEAD"]),
            true,
            64 * 1024,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_head_sha(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.git_manager_bounded_read(
            "GitManager.getRefs.headSha",
            cwd,
            &strings(&["rev-parse", "--verify", "HEAD"]),
            true,
            64 * 1024,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_status(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.git_manager_bounded_read(
            "GitManager.getRefs.status",
            cwd,
            &strings(&[
                "-c",
                "core.quotePath=false",
                "status",
                "--porcelain=2",
                "-z",
                "--untracked-files=all",
            ]),
            false,
            WORKTREE_REMOVAL_STATUS_OUTPUT_LIMIT,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_conflicted_paths(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.git_manager_bounded_read(
            "GitManager.getRefs.conflicts",
            cwd,
            &strings(&["diff", "--name-only", "--diff-filter=U", "-z", "--"]),
            false,
            WORKTREE_REMOVAL_STATUS_OUTPUT_LIMIT,
            cancellation,
        )
        .await
    }

    pub async fn git_manager_conflict_states(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<Vec<super::manager::conflicts::GitManagerConflictState>, GitCommandError> {
        let status = self
            .execute(
                "GitManager.conflicts.status",
                cwd,
                &strings(&[
                    "-c",
                    "core.quotePath=false",
                    "status",
                    "--porcelain=2",
                    "-z",
                    "--untracked-files=all",
                ]),
                true,
                cancellation,
            )
            .await?;
        let records = status
            .stdout
            .split('\0')
            .flat_map(str::lines)
            .filter_map(parse_porcelain_v2_line)
            .collect::<Vec<_>>();
        let entries = super::manager::conflicts::unmerged_entries(&records);
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        let markers = self
            .execute(
                "GitManager.conflicts.markers",
                cwd,
                &strings(&["diff", "--check"]),
                true,
                cancellation,
            )
            .await?;
        let numstat = self
            .execute(
                "GitManager.conflicts.numstat",
                cwd,
                &strings(&["diff", "--numstat", "-z"]),
                true,
                cancellation,
            )
            .await?;
        let mut attribute_input = entries
            .iter()
            .flat_map(|entry| entry.path.bytes().chain(std::iter::once(0)))
            .collect::<Vec<_>>();
        if attribute_input.last() != Some(&0) {
            attribute_input.push(0);
        }
        let attributes = self
            .execute_with_stdin(
                "GitManager.conflicts.attributes",
                cwd,
                &strings(&["check-attr", "--stdin", "-z", "merge"]),
                attribute_input,
                false,
                cancellation,
            )
            .await?;
        let marker_output = format!("{}\n{}", markers.stdout, markers.stderr);
        let marker_counts = super::manager::conflicts::count_conflict_markers(&marker_output);
        let binary_paths =
            super::manager::conflicts::binary_conflict_paths(&numstat.stdout, &attributes.stdout);
        Ok(super::manager::conflicts::build_conflict_states(
            &entries,
            &binary_paths,
            &marker_counts,
        ))
    }

    pub(crate) async fn git_manager_conflict_side_deleted(
        &self,
        cwd: &Path,
        path: &str,
        side: super::manager::conflicts::ConflictSide,
        cancellation: &CancellationToken,
    ) -> Result<Option<bool>, GitCommandError> {
        let status = self
            .execute(
                "GitManager.resolveConflict.status",
                cwd,
                &strings(&[
                    "-c",
                    "core.quotePath=false",
                    "status",
                    "--porcelain=2",
                    "-z",
                    "--untracked-files=all",
                ]),
                true,
                cancellation,
            )
            .await?;
        let records = status
            .stdout
            .split('\0')
            .flat_map(str::lines)
            .filter_map(parse_porcelain_v2_line)
            .collect::<Vec<_>>();
        Ok(super::manager::conflicts::unmerged_entries(&records)
            .into_iter()
            .find(|entry| entry.path == path)
            .map(|entry| entry.side_deleted(side)))
    }

    pub(crate) async fn git_manager_remotes(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.git_manager_bounded_read(
            "GitManager.getRefs.remotes",
            cwd,
            &strings(&["remote"]),
            false,
            DEFAULT_OUTPUT_LIMIT,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_git_dir(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.git_manager_bounded_read(
            "GitManager.getRefs.gitDir",
            cwd,
            &strings(&["rev-parse", "--git-dir"]),
            false,
            64 * 1024,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_in_progress_paths(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.git_manager_bounded_read(
            "GitManager.getRefs.inProgressPaths",
            cwd,
            &strings(&[
                "rev-parse",
                "--git-path",
                "MERGE_HEAD",
                "--git-path",
                "CHERRY_PICK_HEAD",
                "--git-path",
                "REVERT_HEAD",
                "--git-path",
                "SQUASH_MSG",
                "--git-path",
                "rebase-merge",
                "--git-path",
                "rebase-apply",
                "--git-path",
                "rebase-merge/msgnum",
                "--git-path",
                "rebase-merge/end",
                "--git-path",
                "sequencer/abort-safety",
                "--git-path",
                "sequencer/head",
                "--git-path",
                "sequencer/todo",
            ]),
            false,
            128 * 1024,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_count_commit_range(
        &self,
        cwd: &Path,
        from: &str,
        to: &str,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.git_manager_bounded_read(
            "GitManager.getRefs.inProgressCount",
            cwd,
            &[
                "rev-list".to_owned(),
                "--count".to_owned(),
                format!("{from}..{to}"),
                "--".to_owned(),
            ],
            false,
            64 * 1024,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_signal_refs(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.git_manager_bounded_read(
            "GitManager.signal.refs",
            cwd,
            &strings(&[
                "for-each-ref",
                "--format=%(objectname)%09%(refname)%09%(worktreepath)",
                "refs/heads",
                "refs/remotes",
                "refs/tags",
                "refs/stash",
            ]),
            false,
            GIT_MANAGER_TIPS_OUTPUT_LIMIT,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_signal_head(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<GitManagerSignalHead, GitCommandError> {
        let head_args = strings(&["symbolic-ref", "--quiet", "--short", "HEAD"]);
        let oid_args = strings(&["rev-parse", "--verify", "--quiet", "HEAD"]);
        let (head, oid) = tokio::try_join!(
            self.git_manager_bounded_read(
                "GitManager.signal.headRef",
                cwd,
                &head_args,
                true,
                64 * 1024,
                cancellation,
            ),
            self.git_manager_bounded_read(
                "GitManager.signal.headSha",
                cwd,
                &oid_args,
                true,
                64 * 1024,
                cancellation,
            ),
        )?;
        // `symbolic-ref --quiet` exits 1 for a detached HEAD and 128 when HEAD names
        // no valid ref, such as Git's clone placeholder `refs/heads/.invalid`;
        // `rev-parse --verify --quiet` exits 1 when HEAD has no commit. A HEAD with
        // neither (an interrupted clone, on files or reftable) is a stable state.
        let ref_name = match head.exit_code {
            0 if !head.stdout.trim().is_empty() => Some(head.stdout.trim().to_owned()),
            1 => None,
            128 if oid.exit_code == 1 => None,
            _ => {
                return Err(simple_error(
                    "GitManager.signal.headRef",
                    cwd,
                    "Could not read HEAD's symbolic ref.",
                ));
            }
        };
        let sha = match oid.exit_code {
            0 if !oid.stdout.trim().is_empty() => Some(oid.stdout.trim().to_owned()),
            1 => None,
            _ => {
                return Err(simple_error(
                    "GitManager.signal.headSha",
                    cwd,
                    "Could not resolve HEAD.",
                ));
            }
        };
        Ok(GitManagerSignalHead {
            reference: ref_name,
            oid: sha,
        })
    }

    pub(crate) async fn git_manager_signal_stash_reflog(
        &self,
        cwd: &Path,
        common_dir: &Path,
        cancellation: &CancellationToken,
    ) -> Result<Vec<u8>, GitCommandError> {
        let operation = "GitManager.signal.stashReflog";
        let observation = async {
            let path = common_dir.join("logs/refs/stash");
            let file = match git_metadata_open_options().open(path).await {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    // Reftable keeps reflogs in its tables rather than loose log files.
                    let tables = tokio::fs::try_exists(common_dir.join("reftable/tables.list"))
                        .await
                        .map_err(|error| simple_error(operation, cwd, &error.to_string()))?;
                    return if tables {
                        self.git_manager_bounded_read(
                            operation,
                            cwd,
                            &strings(&["stash", "list", "--format=%H%x00%gs%x00"]),
                            false,
                            GIT_MANAGER_STASH_REFLOG_LIMIT,
                            cancellation,
                        )
                        .await
                        .map(|output| output.stdout.into_bytes())
                    } else {
                        Ok(Vec::new())
                    };
                }
                Err(error) => return Err(simple_error(operation, cwd, &error.to_string())),
            };
            if !file
                .metadata()
                .await
                .map_err(|error| simple_error(operation, cwd, &error.to_string()))?
                .is_file()
            {
                return Err(simple_error(
                    operation,
                    cwd,
                    "The stash reflog must be a regular file.",
                ));
            }
            let mut bytes = Vec::new();
            file.take(GIT_MANAGER_STASH_REFLOG_LIMIT as u64 + 1)
                .read_to_end(&mut bytes)
                .await
                .map_err(|error| simple_error(operation, cwd, &error.to_string()))?;
            if bytes.len() > GIT_MANAGER_STASH_REFLOG_LIMIT {
                return Err(simple_error(
                    operation,
                    cwd,
                    "The stash reflog exceeded its byte limit.",
                ));
            }
            Ok(bytes)
        };
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(simple_error(operation, cwd, "Stash reflog observation was cancelled.")),
            result = tokio::time::timeout(DEFAULT_TIMEOUT, observation) => result.unwrap_or_else(|_| Err(simple_error(operation, cwd, "Stash reflog observation timed out."))),
        }
    }

    pub(crate) async fn git_manager_default_ref(
        &self,
        cwd: &Path,
        current: Option<&str>,
        cancellation: &CancellationToken,
    ) -> Result<Option<String>, GitCommandError> {
        self.default_ref(cwd, current, cancellation).await
    }

    pub(crate) async fn git_manager_bounded_read(
        &self,
        operation: &str,
        cwd: &Path,
        args: &[String],
        allow_non_zero_exit: bool,
        max_output_bytes: usize,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.execute_with_environment(
            operation,
            cwd,
            args,
            GitExecutionOptions {
                allow_non_zero_exit,
                max_output_bytes,
                output_policy: OutputPolicy::Error,
            },
            git_read_environment(),
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_working_tree_diff(
        &self,
        cwd: &Path,
        path: &str,
        staged: bool,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        let mut args = vec!["diff".to_owned()];
        if staged {
            args.push("--cached".to_owned());
        }
        args.extend(strings(&[
            "--no-ext-diff",
            "--patch-with-raw",
            "-z",
            "--no-color",
            "HEAD",
            "--",
        ]));
        args.push(path.to_owned());
        self.git_manager_diff_read(
            "GitManager.getDiff.workingTree",
            cwd,
            &args,
            false,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_intent_to_add(
        &self,
        cwd: &Path,
        path: &str,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        let mut args = strings(&["add", "--intent-to-add", "--"]);
        args.push(path.to_owned());
        self.execute(
            "GitManager.partial.intentToAdd",
            cwd,
            &args,
            false,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_clear_intent_to_add(
        &self,
        cwd: &Path,
        path: &str,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        let mut args = strings(&["reset", "--"]);
        args.push(path.to_owned());
        self.execute(
            "GitManager.partial.clearIntentToAdd",
            cwd,
            &args,
            false,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_apply_partial_patch(
        &self,
        operation: &str,
        cwd: &Path,
        patch: Vec<u8>,
        cached: bool,
        reverse: bool,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        let mut args = vec!["apply".to_owned()];
        if cached {
            args.push("--cached".to_owned());
        }
        if reverse {
            args.push("--reverse".to_owned());
        }
        args.extend(strings(&["--unidiff-zero", "--whitespace=nowarn", "-"]));
        self.execute_with_stdin(operation, cwd, &args, patch, false, cancellation)
            .await
    }

    pub(crate) async fn git_manager_untracked_paths(
        &self,
        cwd: &Path,
        path: &str,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        let mut args = strings(&["ls-files", "--others", "--exclude-standard", "-z", "--"]);
        args.push(path.to_owned());
        self.git_manager_bounded_read(
            "GitManager.getDiff.untrackedPaths",
            cwd,
            &args,
            false,
            DEFAULT_OUTPUT_LIMIT,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_untracked_diff(
        &self,
        cwd: &Path,
        path: &str,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        let mut args = strings(&[
            "diff",
            "--no-ext-diff",
            "--patch-with-raw",
            "-z",
            "--no-color",
            "--no-index",
            "--",
            "/dev/null",
        ]);
        args.push(path.to_owned());
        self.git_manager_diff_read(
            "GitManager.getDiff.untracked",
            cwd,
            &args,
            true,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_commit_diff(
        &self,
        cwd: &Path,
        sha: &str,
        path: &str,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        let mut args = vec!["log".to_owned(), sha.to_owned()];
        args.extend(strings(&[
            "-m",
            "-1",
            "--first-parent",
            "--patch-with-raw",
            "--format=",
            "-z",
            "--no-color",
            "--",
        ]));
        args.push(path.to_owned());
        self.git_manager_diff_read("GitManager.getDiff.commit", cwd, &args, false, cancellation)
            .await
    }

    pub(crate) async fn git_manager_stash_list(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.git_manager_bounded_read(
            "GitManager.getStashes.list",
            cwd,
            &strings(&[
                "log",
                "-g",
                "-z",
                "--no-show-signature",
                "--format=%gD%x1f%H%x1f%gs%x1f%ct%x1f%P",
                "refs/stash",
                "--",
            ]),
            true,
            GIT_MANAGER_TIPS_OUTPUT_LIMIT,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_stash_file_list(
        &self,
        cwd: &Path,
        selector: &str,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.git_manager_bounded_read(
            "GitManager.getStashes.files",
            cwd,
            &[
                "stash".into(),
                "show".into(),
                // Untracked files a stash saved (Git 2.32+) are part of its contents.
                "--include-untracked".into(),
                selector.into(),
                "--raw".into(),
                "--numstat".into(),
                "-z".into(),
                "--format=format:".into(),
                "--no-show-signature".into(),
                "--".into(),
            ],
            false,
            GIT_MANAGER_TIPS_OUTPUT_LIMIT,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_stash_diff(
        &self,
        cwd: &Path,
        selector: &str,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.git_manager_diff_read(
            "GitManager.getDiff.stash",
            cwd,
            &[
                "stash".into(),
                "show".into(),
                "-p".into(),
                "--include-untracked".into(),
                selector.into(),
                "--no-color".into(),
            ],
            false,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_resolve_merge_tip(
        &self,
        cwd: &Path,
        revision: &str,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.git_manager_bounded_read(
            "GitManager.previewMerge.resolveTip",
            cwd,
            &[
                "rev-parse".into(),
                "--verify".into(),
                "--end-of-options".into(),
                format!("{revision}^{{commit}}"),
            ],
            true,
            64 * 1024,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_merge_ahead_behind(
        &self,
        cwd: &Path,
        ours_tip: &str,
        theirs_tip: &str,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.git_manager_bounded_read(
            "GitManager.previewMerge.aheadBehind",
            cwd,
            &[
                "rev-list".into(),
                "--left-right".into(),
                "--count".into(),
                format!("{ours_tip}...{theirs_tip}"),
                "--".into(),
            ],
            false,
            64 * 1024,
            cancellation,
        )
        .await
    }

    /// `git --version`, read once per repository value and cached; `None` when unparseable.
    pub(crate) async fn git_manager_git_version(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<Option<crate::git::GitVersion>, GitCommandError> {
        if let Some(version) = *self
            .git_version
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
        {
            return Ok(Some(version));
        }
        let output = self
            .git_manager_bounded_read(
                "GitManager.gitVersion",
                cwd,
                &strings(&["--version"]),
                true,
                4 * 1024,
                cancellation,
            )
            .await?;
        let version = crate::git::GitVersion::parse(&output.stdout);
        if let Some(version) = version {
            *self
                .git_version
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(version);
        }
        Ok(version)
    }

    pub(crate) async fn git_manager_merge_tree(
        &self,
        cwd: &Path,
        ours_tip: &str,
        theirs_tip: &str,
        attr_source: Option<&str>,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        let mut args = Vec::new();
        if let Some(tree) = attr_source {
            args.push(format!("--attr-source={tree}"));
        }
        args.extend(strings(&[
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            "-z",
            ours_tip,
            theirs_tip,
        ]));
        self.git_manager_bounded_read(
            "GitManager.previewMerge.mergeTree",
            cwd,
            &args,
            true,
            GIT_MANAGER_TIPS_OUTPUT_LIMIT,
            cancellation,
        )
        .await
    }

    async fn git_manager_diff_read(
        &self,
        operation: &str,
        cwd: &Path,
        args: &[String],
        allow_non_zero_exit: bool,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        let mut canonical_args = strings(&super::CANONICAL_DIFF_CONFIG);
        canonical_args.extend_from_slice(args);
        self.execute_with_environment(
            operation,
            cwd,
            &canonical_args,
            GitExecutionOptions {
                allow_non_zero_exit,
                max_output_bytes: MAX_DIFF_BUFFER_SIZE + 1,
                output_policy: OutputPolicy::Truncate,
            },
            git_read_environment(),
            cancellation,
        )
        .await
    }

    pub(crate) async fn run(
        &self,
        operation: &str,
        cwd: &Path,
        args: &[String],
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.execute(operation, cwd, args, false, cancellation)
            .await
    }

    async fn run_read(
        &self,
        operation: &str,
        cwd: &Path,
        args: &[String],
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.execute_read(operation, cwd, args, false, cancellation)
            .await
    }

    pub async fn is_repository(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<bool, GitCommandError> {
        Ok(matches!(
            self.probe_repository(cwd, cancellation).await?,
            RepositoryProbe::WorkTree
        ))
    }

    async fn probe_repository(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<RepositoryProbe, GitCommandError> {
        let result = self
            .execute_read(
                "GitVcsDriver.detectRepository",
                cwd,
                &strings(&["rev-parse", "--is-inside-work-tree"]),
                true,
                cancellation,
            )
            .await?;
        Ok(if result.exit_code != 0 {
            RepositoryProbe::Refused(result.stderr)
        } else if result.stdout.trim() == "true" {
            RepositoryProbe::WorkTree
        } else {
            RepositoryProbe::NonWorkTree
        })
    }

    pub async fn repository_root(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<Option<PathBuf>, GitCommandError> {
        if !self.is_repository(cwd, cancellation).await? {
            return Ok(None);
        }
        let output = self
            .run_read(
                "GitVcsDriver.repositoryRoot",
                cwd,
                &strings(&["rev-parse", "--show-toplevel"]),
                cancellation,
            )
            .await?;
        Ok(Some(PathBuf::from(output.stdout.trim())))
    }

    pub(crate) async fn resolve_common_dir(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<PathBuf, GitCommandError> {
        let output = self
            .run_read(
                "GitVcsDriver.resolveCommonDir",
                cwd,
                &strings(&["rev-parse", "--git-common-dir"]),
                cancellation,
            )
            .await?;
        let value = git_output_line(&output.stdout)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                simple_error(
                    "GitVcsDriver.resolveCommonDir",
                    cwd,
                    "Git did not report one common directory.",
                )
            })?;
        let lexical = normalize_path_lexically(&resolve_git_metadata_path(cwd, value));
        Ok(tokio::fs::canonicalize(&lexical).await.unwrap_or(lexical))
    }

    pub(crate) async fn resolve_watch_roots(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<GitWatchRoots, GitCommandError> {
        const OPERATION: &str = "GitVcsDriver.resolveWatchRoots";
        let output = self
            .execute_with_environment(
                OPERATION,
                cwd,
                &strings(&[
                    "rev-parse",
                    "--path-format=absolute",
                    "--show-toplevel",
                    "--git-dir",
                    "--git-common-dir",
                ]),
                GitExecutionOptions {
                    allow_non_zero_exit: false,
                    max_output_bytes: WATCH_ROOTS_OUTPUT_LIMIT,
                    output_policy: OutputPolicy::Error,
                },
                git_read_environment(),
                cancellation,
            )
            .await?;
        let values = output.stdout.lines().collect::<Vec<_>>();
        if values.len() != 3 || values.iter().any(|value| value.is_empty()) {
            return Err(simple_error(
                OPERATION,
                cwd,
                "Git did not report exactly one worktree, Git directory, and common directory.",
            ));
        }
        let resolve = |value: &str| {
            let lexical = normalize_path_lexically(&resolve_git_metadata_path(cwd, value));
            async move { tokio::fs::canonicalize(&lexical).await.unwrap_or(lexical) }
        };
        let (worktree_root, git_dir, common_dir) =
            tokio::join!(resolve(values[0]), resolve(values[1]), resolve(values[2]),);
        Ok(GitWatchRoots {
            worktree_root,
            git_dir,
            common_dir,
        })
    }

    /// Watch an ordinary checkout that Git could not resolve, without using the
    /// filesystem evidence as a Git Manager identity or fetch attachment.
    pub(crate) async fn fallback_watch_roots(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Option<GitWatchRoots> {
        bounded_repository_discovery(
            async {
                let (cwd, across_filesystems) =
                    repository_discovery_start(cwd, self.discovery_environment).await?;
                let RepositoryDiscovery::Directory(worktree_root) =
                    discover_repository_from_ancestors(cwd, across_filesystems, |path| {
                        tokio::fs::symlink_metadata(path)
                    })
                    .await?
                else {
                    return None;
                };
                let git_dir = tokio::fs::canonicalize(worktree_root.join(".git"))
                    .await
                    .ok()?;
                Some(GitWatchRoots {
                    worktree_root,
                    common_dir: git_dir.clone(),
                    git_dir,
                })
            },
            cancellation,
        )
        .await
    }

    pub async fn worktree_inventory(
        &self,
        anchor: &Path,
        cancellation: &CancellationToken,
    ) -> Result<GitWorktreeInventory, GitCommandError> {
        let common_dir_output = self
            .execute_with_options(
                "GitVcsDriver.worktreeInventory.commonDir",
                anchor,
                &strings(&["rev-parse", "--git-common-dir"]),
                GitExecutionOptions {
                    allow_non_zero_exit: false,
                    max_output_bytes: WORKTREE_INVENTORY_OUTPUT_LIMIT,
                    output_policy: OutputPolicy::Error,
                },
                cancellation,
            )
            .await?;
        ensure_authoritative_inventory_output(
            "GitVcsDriver.worktreeInventory.commonDir",
            anchor,
            &common_dir_output,
        )?;
        let common_dir_value = git_output_line(&common_dir_output.stdout).ok_or_else(|| {
            simple_error(
                "GitVcsDriver.worktreeInventory.commonDir",
                anchor,
                "Git reported an invalid common Git directory.",
            )
        })?;
        if common_dir_value.is_empty() {
            return Err(simple_error(
                "GitVcsDriver.worktreeInventory.commonDir",
                anchor,
                "Git did not report a common Git directory.",
            ));
        }
        let common_dir_path = PathBuf::from(common_dir_value);
        let common_dir = if common_dir_path.is_absolute() {
            common_dir_path
        } else {
            normalize_path_lexically(&anchor.join(common_dir_path))
        };

        let (records, nul_delimited) = self.worktree_records(anchor, cancellation).await?;
        Ok(GitWorktreeInventory {
            common_dir,
            records,
            nul_delimited,
        })
    }

    /// The registered worktrees from one `git worktree list --porcelain`, and whether Git
    /// NUL-delimited its output.
    pub(crate) async fn worktree_records(
        &self,
        anchor: &Path,
        cancellation: &CancellationToken,
    ) -> Result<(Vec<GitWorktreeRecord>, bool), GitCommandError> {
        let (output, nul_delimited) = self.worktree_porcelain_output(anchor, cancellation).await?;
        ensure_authoritative_inventory_output(
            "GitVcsDriver.worktreeInventory.list",
            anchor,
            &output,
        )?;
        let records = parse_worktree_porcelain(&output.stdout, nul_delimited).map_err(|error| {
            simple_error(
                "GitVcsDriver.worktreeInventory.parse",
                anchor,
                &format!("Git worktree inventory is malformed: {error}"),
            )
        })?;
        Ok((records, nul_delimited))
    }

    /// The directory the managed worktrees of the repository at `cwd` are created in, or
    /// `None` while the configured base directory is unavailable.
    pub(crate) async fn managed_worktree_parent(&self, cwd: &Path) -> Option<PathBuf> {
        let policy = self.worktree_path_policy(cwd).await.ok()?;
        policy.path_for(cwd, "_").parent().map(Path::to_path_buf)
    }

    pub async fn inspect_worktree_removal(
        &self,
        anchor: &Path,
        record: &GitWorktreeRecord,
        cancellation: &CancellationToken,
    ) -> Result<GitWorktreeRemovalInspection, GitCommandError> {
        validate_removable_worktree(anchor, record)?;
        let inventory = self.worktree_inventory(anchor, cancellation).await?;
        let fresh = exact_worktree_record(&inventory, record).ok_or_else(|| {
            worktree_record_mismatch_error("GitVcsDriver.inspectWorktreeRemoval", anchor, record)
        })?;
        let inspection = match tokio::fs::symlink_metadata(&fresh.path).await {
            Ok(_) => {
                let target_inventory = self.worktree_inventory(&fresh.path, cancellation).await?;
                let expected_common_dir = canonical_common_dir(
                    "GitVcsDriver.inspectWorktreeRemoval",
                    anchor,
                    &inventory.common_dir,
                )
                .await?;
                let target_common_dir = canonical_common_dir(
                    "GitVcsDriver.inspectWorktreeRemoval",
                    anchor,
                    &target_inventory.common_dir,
                )
                .await?;
                if normalized_worktree_path(&target_common_dir)
                    != normalized_worktree_path(&expected_common_dir)
                    || exact_worktree_record(&target_inventory, record).is_none()
                {
                    return Err(simple_error(
                        "GitVcsDriver.inspectWorktreeRemoval",
                        anchor,
                        "The path currently belongs to a different repository than the registered worktree.",
                    ));
                }
                let output = self
                    .execute_with_options(
                        "GitVcsDriver.inspectWorktreeRemoval.status",
                        &fresh.path,
                        &strings(&[
                            "-c",
                            "core.quotePath=false",
                            "status",
                            "--porcelain=v2",
                            "-z",
                            "--untracked-files=all",
                            "--ignored=no",
                        ]),
                        GitExecutionOptions {
                            allow_non_zero_exit: false,
                            max_output_bytes: WORKTREE_REMOVAL_STATUS_OUTPUT_LIMIT,
                            output_policy: OutputPolicy::Error,
                        },
                        cancellation,
                    )
                    .await?;
                parse_worktree_removal_status(anchor, &output.stdout)?
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                GitWorktreeRemovalInspection::default()
            }
            Err(error) => {
                return Err(simple_error(
                    "GitVcsDriver.inspectWorktreeRemoval",
                    anchor,
                    &format!(
                        "The registered worktree path '{}' could not be inspected: {error}",
                        display_path(&fresh.path)
                    ),
                ));
            }
        };
        let verified = self.worktree_inventory(anchor, cancellation).await?;
        if exact_worktree_record(&verified, record).is_none() {
            return Err(worktree_record_mismatch_error(
                "GitVcsDriver.inspectWorktreeRemoval",
                anchor,
                record,
            ));
        }
        Ok(inspection)
    }

    pub async fn remove_worktree_verified(
        &self,
        anchor: &Path,
        record: &GitWorktreeRecord,
        force_dirty: bool,
        cancellation: &CancellationToken,
    ) -> Result<(), GitCommandError> {
        let inspection = self
            .inspect_worktree_removal(anchor, record, cancellation)
            .await?;
        if !force_dirty
            && (inspection.tracked_change_count != 0 || inspection.untracked_file_count != 0)
        {
            return Err(simple_error(
                "GitVcsDriver.removeWorktreeVerified",
                anchor,
                "The worktree is dirty; explicit dirty-worktree confirmation is required.",
            ));
        }

        let inventory = self.worktree_inventory(anchor, cancellation).await?;
        let fresh = exact_worktree_record(&inventory, record).ok_or_else(|| {
            worktree_record_mismatch_error("GitVcsDriver.removeWorktreeVerified", anchor, record)
        })?;
        let target_path = fresh.path.clone();
        match tokio::fs::symlink_metadata(&target_path).await {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let impact = self
                    .worktree_prune_impact(anchor, &inventory, cancellation)
                    .await?;
                let exact_target_only = matches!(
                    impact.as_slice(),
                    [entry]
                        if normalized_worktree_path(&entry.path)
                            == normalized_worktree_path(&target_path)
                );
                if !exact_target_only {
                    return Err(simple_error(
                        "GitVcsDriver.removeWorktreeVerified",
                        anchor,
                        "The missing worktree registration cannot be cleaned without pruning additional registrations.",
                    ));
                }
                let impact_digest = git_worktree_prune_impact_digest(&impact);
                return self
                    .prune_worktrees_verified(anchor, fresh, &impact_digest, cancellation)
                    .await;
            }
            Err(error) => {
                return Err(simple_error(
                    "GitVcsDriver.removeWorktreeVerified",
                    anchor,
                    &format!(
                        "The registered worktree path '{}' could not be inspected before protected removal: {error}",
                        display_path(&target_path)
                    ),
                ));
            }
        }
        self.remove_worktree(anchor, &target_path, true, cancellation)
            .await?;
        let verification = self
            .worktree_inventory(anchor, &CancellationToken::new())
            .await?;
        if !inventory_contains_path(&verification, &target_path) {
            return Ok(());
        }
        Err(simple_error(
            "GitVcsDriver.removeWorktreeVerified.verify",
            anchor,
            "Protected Git worktree removal completed, but the exact target registration survived verification.",
        ))
    }

    pub async fn preview_worktree_prune(
        &self,
        anchor: &Path,
        cancellation: &CancellationToken,
    ) -> Result<Vec<GitPrunableWorktree>, GitCommandError> {
        let before = self.worktree_inventory(anchor, cancellation).await?;
        let impact = self
            .worktree_prune_impact(anchor, &before, cancellation)
            .await?;
        let after = self.worktree_inventory(anchor, cancellation).await?;
        if sorted_prunable_records(&before) != sorted_prunable_records(&after) {
            return Err(simple_error(
                "GitVcsDriver.previewWorktreePrune.verify",
                anchor,
                "Git worktree registrations changed while the prune impact was being inspected.",
            ));
        }
        Ok(impact)
    }

    pub async fn prune_worktrees_verified(
        &self,
        anchor: &Path,
        target: &GitWorktreeRecord,
        expected_impact_digest: &str,
        cancellation: &CancellationToken,
    ) -> Result<(), GitCommandError> {
        validate_removable_worktree(anchor, target)?;
        let inventory = self.worktree_inventory(anchor, cancellation).await?;
        let impact = self
            .worktree_prune_impact(anchor, &inventory, cancellation)
            .await?;
        let fresh = exact_worktree_record(&inventory, target).ok_or_else(|| {
            worktree_record_mismatch_error("GitVcsDriver.pruneWorktreesVerified", anchor, target)
        })?;
        if !fresh.is_prunable
            || !impact.iter().any(|entry| {
                normalized_worktree_path(&entry.path) == normalized_worktree_path(&fresh.path)
            })
        {
            return Err(simple_error(
                "GitVcsDriver.pruneWorktreesVerified",
                anchor,
                "The exact target registration is not part of Git's current prune impact.",
            ));
        }
        if git_worktree_prune_impact_digest(&impact) != expected_impact_digest {
            return Err(simple_error(
                "GitVcsDriver.pruneWorktreesVerified",
                anchor,
                "Git's current prune impact does not match the caller-confirmed impact digest.",
            ));
        }

        let target_path = fresh.path.clone();
        let mutation = self
            .run(
                "GitVcsDriver.pruneWorktreesVerified",
                anchor,
                &strings(&["worktree", "prune", "--expire", "now"]),
                cancellation,
            )
            .await;
        let verification = self
            .worktree_inventory(anchor, &CancellationToken::new())
            .await?;
        if !inventory_contains_path(&verification, &target_path) {
            return Ok(());
        }
        match mutation {
            Ok(_) => Err(simple_error(
                "GitVcsDriver.pruneWorktreesVerified.verify",
                anchor,
                "Git reported successful worktree pruning, but the exact target registration survived verification.",
            )),
            Err(error) => Err(error),
        }
    }

    async fn worktree_prune_impact(
        &self,
        anchor: &Path,
        inventory: &GitWorktreeInventory,
        cancellation: &CancellationToken,
    ) -> Result<Vec<GitPrunableWorktree>, GitCommandError> {
        let common_dir = canonical_common_dir(
            "GitVcsDriver.previewWorktreePrune",
            anchor,
            &inventory.common_dir,
        )
        .await?;
        let output = self
            .execute_with_options(
                "GitVcsDriver.previewWorktreePrune",
                anchor,
                &strings(&[
                    "worktree",
                    "prune",
                    "--dry-run",
                    "--verbose",
                    "--expire",
                    "now",
                ]),
                GitExecutionOptions {
                    allow_non_zero_exit: false,
                    max_output_bytes: WORKTREE_PRUNE_OUTPUT_LIMIT,
                    output_policy: OutputPolicy::Error,
                },
                cancellation,
            )
            .await?;
        if !output.stdout.is_empty() {
            return Err(simple_error(
                "GitVcsDriver.previewWorktreePrune.parse",
                anchor,
                "Git worktree prune preview produced unexpected standard output.",
            ));
        }
        let dry_run_records = parse_worktree_prune_dry_run(&output.stderr).map_err(|error| {
            simple_error(
                "GitVcsDriver.previewWorktreePrune.parse",
                anchor,
                &format!("Git worktree prune preview is malformed: {error}"),
            )
        })?;
        let resolved =
            resolve_worktree_prune_paths(anchor, &common_dir, dry_run_records, cancellation)
                .await?;
        let mut records = sorted_prunable_records(inventory);
        if resolved.len() != records.len() {
            return Err(simple_error(
                "GitVcsDriver.previewWorktreePrune.verify",
                anchor,
                "Git's dry-run path and reason impact does not match the authoritative worktree inventory.",
            ));
        }
        let mut impact = Vec::with_capacity(resolved.len());
        for (dry_run, path) in resolved {
            let key = normalized_worktree_path(&path);
            let Some(index) = records.iter().position(|record| {
                normalized_worktree_path(&record.path) == key
                    && record.prunable_reason.as_deref() == Some(dry_run.reason.as_str())
            }) else {
                return Err(simple_error(
                    "GitVcsDriver.previewWorktreePrune.verify",
                    anchor,
                    "Git's dry-run path and reason impact does not match the authoritative worktree inventory.",
                ));
            };
            let record = records.remove(index);
            impact.push(GitPrunableWorktree {
                path: record.path,
                prune_reason: dry_run.reason,
                locked: record.locked,
                lock_reason: record.lock_reason,
            });
        }
        if !records.is_empty() {
            return Err(simple_error(
                "GitVcsDriver.previewWorktreePrune.verify",
                anchor,
                "Git's dry-run path and reason impact does not match the authoritative worktree inventory.",
            ));
        }
        impact.sort_by(|left, right| {
            normalized_worktree_path(&left.path)
                .cmp(&normalized_worktree_path(&right.path))
                .then_with(|| left.prune_reason.cmp(&right.prune_reason))
        });
        Ok(impact)
    }

    async fn worktree_porcelain_output(
        &self,
        anchor: &Path,
        cancellation: &CancellationToken,
    ) -> Result<(ProcessOutput, bool), GitCommandError> {
        if self.worktree_porcelain_z_support() != Some(false) {
            let args = strings(&["worktree", "list", "--porcelain", "-z"]);
            let output = self
                .execute_with_options(
                    "GitVcsDriver.worktreeInventory.listZ",
                    anchor,
                    &args,
                    GitExecutionOptions {
                        allow_non_zero_exit: true,
                        max_output_bytes: WORKTREE_INVENTORY_OUTPUT_LIMIT,
                        output_policy: OutputPolicy::Error,
                    },
                    cancellation,
                )
                .await?;
            if output.exit_code == 0 {
                self.set_worktree_porcelain_z_support(true);
                return Ok((output, true));
            }
            if !is_unsupported_porcelain_z_diagnostic(&output.stderr) {
                return Err(command_output_error(
                    "GitVcsDriver.worktreeInventory.listZ",
                    anchor,
                    args.len(),
                    &output,
                    &actionable_git_failure(&output.stderr, &output.stdout),
                ));
            }
            self.set_worktree_porcelain_z_support(false);
        }

        let args = strings(&["worktree", "list", "--porcelain"]);
        let output = self
            .execute_with_options(
                "GitVcsDriver.worktreeInventory.list",
                anchor,
                &args,
                GitExecutionOptions {
                    allow_non_zero_exit: false,
                    max_output_bytes: WORKTREE_INVENTORY_OUTPUT_LIMIT,
                    output_policy: OutputPolicy::Error,
                },
                cancellation,
            )
            .await?;
        Ok((output, false))
    }

    fn worktree_porcelain_z_support(&self) -> Option<bool> {
        self.worktree_porcelain_z_supported
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .to_owned()
    }

    fn set_worktree_porcelain_z_support(&self, supported: bool) {
        *self
            .worktree_porcelain_z_supported
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(supported);
    }

    /// Only a confirmed non-work-tree result replaces the original status error.
    /// Its reason stays optional when classification is inconclusive or times out.
    async fn classify_status_failure(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
        error: GitCommandError,
    ) -> Result<Option<VcsRepositoryUnavailableReason>, GitCommandError> {
        let probe = match self.probe_repository(cwd, cancellation).await {
            Ok(RepositoryProbe::WorkTree) | Err(_) => return Err(error),
            Ok(probe) => probe,
        };
        let reason = bounded_repository_classification(
            classify_repository_unavailable(cwd, &probe, self.discovery_environment),
            cancellation,
        )
        .await;
        if cancellation.is_cancelled() {
            return Err(error);
        }
        Ok(reason)
    }

    pub(crate) async fn observe_status(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<StatusObservation, GitCommandError> {
        let status = match self
            .run_read(
                "GitVcsDriver.statusDetailsLocal.status",
                cwd,
                &strings(&[
                    "-c",
                    "core.quotePath=false",
                    "status",
                    "--porcelain=2",
                    "--branch",
                    "--untracked-files=all",
                ]),
                cancellation,
            )
            .await
        {
            Ok(status) => status,
            Err(error) => {
                let reason = self
                    .classify_status_failure(cwd, cancellation, error)
                    .await?;
                return Ok(StatusObservation {
                    local: reason.map_or_else(
                        VcsStatusLocalResult::non_repository,
                        VcsStatusLocalResult::non_repository_with_reason,
                    ),
                    remote: StatusRemoteObservation {
                        upstream_ref: None,
                        ahead_count: 0,
                        behind_count: 0,
                    },
                });
            }
        };
        let records: Vec<_> = status
            .stdout
            .lines()
            .filter_map(parse_porcelain_v2_line)
            .collect();
        let has_staged_changes = records.iter().any(|record| record.index_changed);
        let has_unstaged_changes = records.iter().any(|record| record.worktree_changed);
        let (staged_stats, unstaged_stats) = match (has_staged_changes, has_unstaged_changes) {
            (true, true) => {
                let staged_args = strings(&[
                    "-c",
                    "core.quotePath=false",
                    "diff",
                    "--cached",
                    "--numstat",
                ]);
                let unstaged_args = strings(&["-c", "core.quotePath=false", "diff", "--numstat"]);
                let (staged, unstaged) = tokio::try_join!(
                    self.run_read(
                        "GitVcsDriver.statusDetailsLocal.stagedNumstat",
                        cwd,
                        &staged_args,
                        cancellation,
                    ),
                    self.run_read(
                        "GitVcsDriver.statusDetailsLocal.unstagedNumstat",
                        cwd,
                        &unstaged_args,
                        cancellation,
                    ),
                )?;
                (
                    parse_numstat(&staged.stdout),
                    parse_numstat(&unstaged.stdout),
                )
            }
            (true, false) => {
                let staged = self
                    .run_read(
                        "GitVcsDriver.statusDetailsLocal.stagedNumstat",
                        cwd,
                        &strings(&[
                            "-c",
                            "core.quotePath=false",
                            "diff",
                            "--cached",
                            "--numstat",
                        ]),
                        cancellation,
                    )
                    .await?;
                (parse_numstat(&staged.stdout), HashMap::new())
            }
            (false, true) => {
                let unstaged = self
                    .run_read(
                        "GitVcsDriver.statusDetailsLocal.unstagedNumstat",
                        cwd,
                        &strings(&["-c", "core.quotePath=false", "diff", "--numstat"]),
                        cancellation,
                    )
                    .await?;
                (HashMap::new(), parse_numstat(&unstaged.stdout))
            }
            (false, false) => (HashMap::new(), HashMap::new()),
        };
        let remote_args = strings(&["remote"]);
        let (remotes, in_progress) = tokio::join!(
            self.execute_read(
                "GitVcsDriver.statusDetailsLocal.remotes",
                cwd,
                &remote_args,
                true,
                cancellation,
            ),
            // ponytail: one `rev-parse --git-path` per status read; cache the resolved
            // paths per worktree if status reads become a measured hot spot.
            detect_in_progress_operation(self, cwd, cancellation),
        );
        let remotes = remotes?;
        // A failed probe degrades to no field; status must not fail because of it.
        let operation_in_progress = in_progress.ok().flatten().map(|operation| operation.kind);
        let remote_names: Vec<&str> = remotes.stdout.lines().map(str::trim).collect();
        let has_primary_remote = remote_names.contains(&"origin");
        let (ref_name, upstream_ref, ahead_count, behind_count) =
            parse_branch_headers(&status.stdout);
        let default_ref_name = self
            .default_ref(cwd, ref_name.as_deref(), cancellation)
            .await?;
        let source_control_provider = if has_primary_remote {
            self.remote_provider(cwd, cancellation).await?
        } else {
            None
        };
        let mut files = Vec::new();
        for record in records {
            if record.untracked {
                let insertions = untracked_line_count(&cwd.join(&record.path)).await;
                files.push(VcsWorkingTreeFile {
                    path: record.path,
                    insertions,
                    deletions: 0,
                    status: Some(VcsWorkingTreeFileStatus::Untracked),
                    area: Some(VcsStagingArea::Untracked),
                });
                continue;
            }
            if record.unmerged {
                // One entry per conflicted path: staging it is how the user marks it resolved.
                let (insertions, deletions) =
                    unstaged_stats.get(&record.path).copied().unwrap_or((0, 0));
                files.push(VcsWorkingTreeFile {
                    path: record.path,
                    insertions,
                    deletions,
                    status: Some(VcsWorkingTreeFileStatus::Conflicted),
                    area: Some(VcsStagingArea::Unstaged),
                });
                continue;
            }
            if record.index_changed {
                let (insertions, deletions) =
                    staged_stats.get(&record.path).copied().unwrap_or((0, 0));
                files.push(VcsWorkingTreeFile {
                    path: record.path.clone(),
                    insertions,
                    deletions,
                    status: Some(record.index_status),
                    area: Some(VcsStagingArea::Staged),
                });
            }
            if record.worktree_changed {
                let (insertions, deletions) =
                    unstaged_stats.get(&record.path).copied().unwrap_or((0, 0));
                files.push(VcsWorkingTreeFile {
                    path: record.path,
                    insertions,
                    deletions,
                    status: Some(record.worktree_status),
                    area: Some(VcsStagingArea::Unstaged),
                });
            }
        }
        files.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then(area_order(left.area).cmp(&area_order(right.area)))
        });
        let insertions = files.iter().map(|file| file.insertions).sum();
        let deletions = files.iter().map(|file| file.deletions).sum();
        let working_tree = VcsWorkingTree {
            files,
            insertions,
            deletions,
        };
        Ok(StatusObservation {
            local: VcsStatusLocalResult {
                is_repo: true,
                repository_unavailable_reason: None,
                source_control_provider,
                has_primary_remote,
                is_default_ref: ref_name.is_some() && ref_name == default_ref_name,
                ref_name,
                default_ref_name,
                has_working_tree_changes: !working_tree.files.is_empty(),
                working_tree,
                operation_in_progress,
            },
            remote: StatusRemoteObservation {
                upstream_ref,
                ahead_count,
                behind_count,
            },
        })
    }

    pub async fn local_status(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<VcsStatusLocalResult, GitCommandError> {
        Ok(self.observe_status(cwd, cancellation).await?.local)
    }

    pub async fn summary_status(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<VcsStatusSummary, GitCommandError> {
        let args = strings(&[
            "-c",
            "core.quotePath=false",
            "status",
            "--porcelain=2",
            "--branch",
            "--untracked-files=normal",
        ]);
        let status = self
            .execute_with_environment(
                "GitVcsDriver.summaryStatus.status",
                cwd,
                &args,
                GitExecutionOptions {
                    allow_non_zero_exit: true,
                    max_output_bytes: SUMMARY_STATUS_OUTPUT_LIMIT,
                    output_policy: OutputPolicy::Error,
                },
                git_read_environment(),
                cancellation,
            )
            .await?;
        if status.exit_code != 0 {
            let error = command_output_error(
                "GitVcsDriver.summaryStatus.status",
                cwd,
                args.len(),
                &status,
                "Git status summary failed.",
            );
            let reason = self
                .classify_status_failure(cwd, cancellation, error)
                .await?;
            return Ok(VcsStatusSummary::non_repository(
                reason,
                summary_observed_at(),
            ));
        }
        let (ref_name, detached_head) = parse_summary_identity(cwd, &status.stdout)?;
        let has_working_tree_changes = status
            .stdout
            .lines()
            .any(|line| parse_porcelain_v2_line(line).is_some());
        let source_control_provider = self.remote_provider(cwd, cancellation).await?;
        Ok(VcsStatusSummary {
            is_repo: true,
            repository_unavailable_reason: None,
            ref_name,
            detached_head,
            has_working_tree_changes,
            source_control_provider,
            pr: None,
            observed_at: summary_observed_at(),
            stale: false,
        })
    }

    pub async fn remote_status(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<Option<VcsStatusRemoteResult>, GitCommandError> {
        Ok(self.observed_remote_status(cwd, cancellation).await?.remote)
    }

    pub(crate) async fn observed_remote_status(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<ObservedRemoteStatus, GitCommandError> {
        if !self.is_repository(cwd, cancellation).await? {
            return Ok(ObservedRemoteStatus {
                ref_name: None,
                remote: None,
            });
        }
        let status = self
            .run_read(
                "GitVcsDriver.statusDetailsRemote.status",
                cwd,
                &strings(&[
                    "status",
                    "--porcelain=2",
                    "--branch",
                    "--untracked-files=no",
                ]),
                cancellation,
            )
            .await?;
        let (branch, upstream, ahead_count, behind_count) = parse_branch_headers(&status.stdout);
        let default_ref = self
            .default_ref(cwd, branch.as_deref(), cancellation)
            .await?;
        let observation = StatusRemoteObservation {
            upstream_ref: upstream,
            ahead_count,
            behind_count,
        };
        let remote = self
            .status_remote_result(
                cwd,
                branch.as_deref(),
                default_ref.as_deref(),
                &observation,
                cancellation,
            )
            .await?;
        Ok(ObservedRemoteStatus {
            ref_name: branch,
            remote: Some(remote),
        })
    }

    async fn status_remote_result(
        &self,
        cwd: &Path,
        branch: Option<&str>,
        default_ref: Option<&str>,
        observation: &StatusRemoteObservation,
        cancellation: &CancellationToken,
    ) -> Result<VcsStatusRemoteResult, GitCommandError> {
        let ahead_of_default_count = match (branch, default_ref) {
            (Some(current), Some(default)) if current == default => Some(0),
            (Some(_), Some(default)) => {
                let range = format!("{default}..HEAD");
                let count = self
                    .execute_read(
                        "GitVcsDriver.statusDetailsRemote.defaultDelta",
                        cwd,
                        &["rev-list".into(), "--count".into(), range],
                        true,
                        cancellation,
                    )
                    .await?;
                (count.exit_code == 0).then(|| count.stdout.trim().parse::<u64>().unwrap_or(0))
            }
            _ => Some(observation.ahead_count),
        };
        Ok(VcsStatusRemoteResult {
            has_upstream: observation.upstream_ref.is_some(),
            ahead_count: observation.ahead_count,
            behind_count: observation.behind_count,
            ahead_of_default_count,
            pr: None::<ChangeRequest>,
        })
    }

    pub async fn refresh_remote_status(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<Option<VcsStatusRemoteResult>, GitCommandError> {
        if !self.is_repository(cwd, cancellation).await? {
            return Ok(None);
        }
        let upstream = self
            .execute_read(
                "GitVcsDriver.refreshRemoteStatus.upstream",
                cwd,
                &strings(&["rev-parse", "--abbrev-ref", "@{upstream}"]),
                true,
                cancellation,
            )
            .await?;
        if upstream.exit_code == 0 {
            let upstream = upstream.stdout.trim();
            let remotes = self
                .run_read(
                    "GitVcsDriver.refreshRemoteStatus.remotes",
                    cwd,
                    &strings(&["remote"]),
                    cancellation,
                )
                .await?;
            let mut remote_names: Vec<&str> = remotes
                .stdout
                .lines()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .collect();
            remote_names.sort_by_key(|name| std::cmp::Reverse(name.len()));
            if let Some(remote) = remote_names
                .into_iter()
                .find(|remote| upstream.starts_with(&format!("{remote}/")))
            {
                self.for_network_transfer()
                    .run(
                        "GitVcsDriver.refreshRemoteStatus.fetch",
                        cwd,
                        &["fetch".into(), "--quiet".into(), remote.into()],
                        cancellation,
                    )
                    .await?;
            }
        }
        self.remote_status(cwd, cancellation).await
    }

    pub(crate) async fn automatic_fetch_upstream_remotes(
        &self,
        cwd: &Path,
        ref_names: &BTreeSet<String>,
        cancellation: &CancellationToken,
    ) -> Result<(), GitCommandError> {
        if ref_names.is_empty() {
            return Ok(());
        }
        let upstreams = self
            .run_read(
                "GitVcsDriver.automaticFetch.upstreams",
                cwd,
                &strings(&[
                    "for-each-ref",
                    "--format=%(refname:short)%00%(upstream:remotename)",
                    "refs/heads",
                ]),
                cancellation,
            )
            .await?;
        let remotes = upstreams
            .stdout
            .lines()
            .filter_map(|line| line.split_once('\0'))
            .filter(|(ref_name, remote)| ref_names.contains(*ref_name) && !remote.is_empty())
            .map(|(_, remote)| remote.to_owned())
            .collect::<BTreeSet<_>>();
        if remotes.is_empty() {
            return Ok(());
        }
        let mut args = strings(&["fetch", "--quiet"]);
        if remotes.len() > 1 {
            args.push("--multiple".to_owned());
        }
        args.push("--".to_owned());
        args.extend(remotes);
        self.for_bounded_transfer()
            .run(
                "GitVcsDriver.automaticFetch.fetch",
                cwd,
                &args,
                cancellation,
            )
            .await?;
        Ok(())
    }

    pub async fn status(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<VcsStatusResult, GitCommandError> {
        let observation = self.observe_status(cwd, cancellation).await?;
        let remote = if observation.local.is_repo {
            self.status_remote_result(
                cwd,
                observation.local.ref_name.as_deref(),
                observation.local.default_ref_name.as_deref(),
                &observation.remote,
                cancellation,
            )
            .await?
        } else {
            VcsStatusRemoteResult {
                has_upstream: false,
                ahead_count: 0,
                behind_count: 0,
                ahead_of_default_count: Some(0),
                pr: None,
            }
        };
        Ok(VcsStatusResult {
            local: observation.local,
            remote,
        })
    }

    pub async fn stage_files(
        &self,
        cwd: &Path,
        paths: &[String],
        cancellation: &CancellationToken,
    ) -> Result<(), GitCommandError> {
        if paths.is_empty() {
            return Ok(());
        }
        validate_pathspecs("GitVcsDriver.stageFiles", cwd, paths)?;
        let mut args = strings(&["add", "--"]);
        args.extend(paths.iter().cloned());
        self.run("GitVcsDriver.stageFiles", cwd, &args, cancellation)
            .await?;
        Ok(())
    }

    pub async fn unstage_files(
        &self,
        cwd: &Path,
        paths: &[String],
        cancellation: &CancellationToken,
    ) -> Result<(), GitCommandError> {
        if paths.is_empty() {
            return Ok(());
        }
        validate_pathspecs("GitVcsDriver.unstageFiles", cwd, paths)?;
        let head = self
            .execute(
                "GitVcsDriver.unstageFiles.verifyHead",
                cwd,
                &strings(&["rev-parse", "--verify", "--quiet", "HEAD"]),
                true,
                cancellation,
            )
            .await?;
        let mut args = if head.exit_code == 0 {
            strings(&["restore", "--staged", "--"])
        } else {
            strings(&["rm", "--cached", "-r", "--ignore-unmatch", "--"])
        };
        args.extend(paths.iter().cloned());
        self.run("GitVcsDriver.unstageFiles", cwd, &args, cancellation)
            .await?;
        Ok(())
    }

    pub async fn discard_files(
        &self,
        cwd: &Path,
        paths: &[String],
        cancellation: &CancellationToken,
    ) -> Result<(), GitCommandError> {
        if paths.is_empty() {
            return Ok(());
        }
        validate_pathspecs("GitVcsDriver.discardFiles", cwd, paths)?;
        let mut list_args = strings(&["-c", "core.quotePath=false", "ls-files", "-z", "--"]);
        list_args.extend(paths.iter().cloned());
        let tracked = self
            .run(
                "GitVcsDriver.discardFiles.listTracked",
                cwd,
                &list_args,
                cancellation,
            )
            .await?;
        let tracked_paths: Vec<String> = tracked
            .stdout
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect();
        if !tracked_paths.is_empty() {
            let mut restore_args = strings(&["restore", "--worktree", "--"]);
            restore_args.extend(tracked_paths);
            self.run(
                "GitVcsDriver.discardFiles.restore",
                cwd,
                &restore_args,
                cancellation,
            )
            .await?;
        }
        let status = self.local_status(cwd, cancellation).await?;
        let untracked: HashSet<&str> = status
            .working_tree
            .files
            .iter()
            .filter(|file| file.area == Some(VcsStagingArea::Untracked))
            .map(|file| file.path.as_str())
            .collect();
        let requested_untracked: Vec<String> = paths
            .iter()
            .filter(|path| {
                untracked.contains(path.as_str())
                    || untracked
                        .iter()
                        .any(|candidate| candidate.starts_with(&format!("{path}/")))
            })
            .cloned()
            .collect();
        if !requested_untracked.is_empty() {
            let mut clean_args = strings(&["clean", "-fd", "--"]);
            clean_args.extend(requested_untracked);
            self.run(
                "GitVcsDriver.discardFiles.clean",
                cwd,
                &clean_args,
                cancellation,
            )
            .await?;
        }
        Ok(())
    }

    pub async fn git_manager_tracked_paths(
        &self,
        cwd: &Path,
        paths: &[String],
        cancellation: &CancellationToken,
    ) -> Result<Vec<String>, GitCommandError> {
        if paths.is_empty() {
            return Ok(Vec::new());
        }
        validate_pathspecs("GitManager.discard", cwd, paths)?;
        let mut args = strings(&["-c", "core.quotePath=false", "ls-files", "-z", "--"]);
        args.extend(paths.iter().cloned());
        let tracked = self
            .run_read("GitManager.discard.tracked", cwd, &args, cancellation)
            .await?;
        Ok(tracked
            .stdout
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect())
    }

    pub async fn git_manager_restore_tracked_paths(
        &self,
        cwd: &Path,
        paths: &[String],
        cancellation: &CancellationToken,
    ) -> Result<(), GitCommandError> {
        if paths.is_empty() {
            return Ok(());
        }
        validate_pathspecs("GitManager.discard", cwd, paths)?;
        let mut args = strings(&["checkout", "HEAD", "--"]);
        args.extend(paths.iter().cloned());
        self.run("GitManager.discard.restore", cwd, &args, cancellation)
            .await?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn list_refs(
        &self,
        cwd: &Path,
        query: Option<&str>,
        cursor: usize,
        limit: usize,
        include_matching_remote_refs: bool,
        ref_kind: Option<&str>,
        cancellation: &CancellationToken,
    ) -> Result<VcsListRefsResult, GitCommandError> {
        if !self.is_repository(cwd, cancellation).await? {
            return Ok(VcsListRefsResult {
                refs: vec![],
                is_repo: false,
                has_primary_remote: false,
                next_cursor: None,
                total_count: 0,
            });
        }
        let branches = self
            .run(
                "GitVcsDriver.listRefs",
                cwd,
                &strings(&[
                    "for-each-ref",
                    "--format=%(refname:short)%09%(HEAD)%09%(committerdate:unix)",
                    "refs/heads",
                    "refs/remotes",
                ]),
                cancellation,
            )
            .await?;
        let remotes = self
            .run(
                "GitVcsDriver.listRefs.remoteNames",
                cwd,
                &strings(&["remote"]),
                cancellation,
            )
            .await?;
        let remote_names: Vec<String> = remotes
            .stdout
            .lines()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect();
        let default_ref = self.default_ref(cwd, None, cancellation).await?;
        let worktrees = self.worktree_map(cwd, cancellation).await?;
        let mut refs_with_time = Vec::new();
        for line in branches.stdout.lines() {
            let mut fields = line.split('\t');
            let Some(name) = fields.next().map(str::trim).filter(|name| !name.is_empty()) else {
                continue;
            };
            if name.contains("HEAD ->") || name.ends_with("/HEAD") {
                continue;
            }
            let current = fields.next().is_some_and(|head| head.trim() == "*");
            let timestamp = fields
                .next()
                .and_then(|value| value.trim().parse::<u64>().ok())
                .unwrap_or(0);
            let remote_name = remote_names
                .iter()
                .find(|remote| name.starts_with(&format!("{remote}/")))
                .cloned();
            let is_remote = remote_name.is_some();
            refs_with_time.push((
                VcsRef {
                    name: name.to_owned(),
                    is_remote: Some(is_remote),
                    remote_name,
                    current,
                    is_default: !is_remote && default_ref.as_deref() == Some(name),
                    worktree_path: (!is_remote).then(|| worktrees.get(name).cloned()).flatten(),
                },
                timestamp,
            ));
        }
        if !include_matching_remote_refs {
            let locals: HashSet<String> = refs_with_time
                .iter()
                .filter(|(reference, _)| reference.is_remote != Some(true))
                .map(|(reference, _)| reference.name.clone())
                .collect();
            refs_with_time.retain(|(reference, _)| {
                reference.is_remote != Some(true)
                    || reference
                        .remote_name
                        .as_ref()
                        .and_then(|remote| reference.name.strip_prefix(&format!("{remote}/")))
                        .is_none_or(|branch| !locals.contains(branch))
            });
        }
        refs_with_time.retain(|(reference, _)| match ref_kind {
            Some("local") => reference.is_remote != Some(true),
            Some("remote") => reference.is_remote == Some(true),
            _ => true,
        });
        if let Some(query) = query {
            let query = query.to_lowercase();
            refs_with_time.retain(|(reference, _)| reference.name.to_lowercase().contains(&query));
        }
        refs_with_time.sort_by(|(left, left_time), (right, right_time)| {
            ref_priority(left)
                .cmp(&ref_priority(right))
                .then(right_time.cmp(left_time))
                .then(left.name.cmp(&right.name))
        });
        let total_count = refs_with_time.len();
        let refs: Vec<VcsRef> = refs_with_time
            .into_iter()
            .skip(cursor)
            .take(limit.min(200))
            .map(|(reference, _)| reference)
            .collect();
        let next_cursor = (cursor + refs.len() < total_count).then_some(cursor + refs.len());
        Ok(VcsListRefsResult {
            refs,
            is_repo: true,
            has_primary_remote: remote_names.iter().any(|remote| remote == "origin"),
            next_cursor,
            total_count,
        })
    }

    pub(crate) async fn local_branch_names(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<Vec<String>, GitCommandError> {
        let args = strings(&["for-each-ref", "--format=%(refname:short)", "refs/heads"]);
        let output = self
            .execute_with_environment(
                "GitVcsDriver.localBranchNames",
                cwd,
                &args,
                GitExecutionOptions {
                    allow_non_zero_exit: false,
                    max_output_bytes: DEFAULT_OUTPUT_LIMIT,
                    output_policy: OutputPolicy::Error,
                },
                git_read_environment(),
                cancellation,
            )
            .await?;
        if output.stdout_truncated || output.stderr_truncated {
            return Err(command_output_error(
                "GitVcsDriver.localBranchNames",
                cwd,
                args.len(),
                &output,
                "Git local-branch output was truncated and is not authoritative.",
            ));
        }
        Ok(output
            .stdout
            .lines()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect())
    }

    pub async fn list_commits(
        &self,
        cwd: &Path,
        limit: usize,
        cursor: usize,
        cancellation: &CancellationToken,
    ) -> Result<VcsListCommitsResult, GitCommandError> {
        let format = format!(
            "--pretty=format:%H{COMMIT_FIELD_SEPARATOR}%h{COMMIT_FIELD_SEPARATOR}%s{COMMIT_FIELD_SEPARATOR}%an{COMMIT_FIELD_SEPARATOR}%at"
        );
        let args = vec![
            "log".into(),
            format!("--max-count={}", limit.min(200) + 1),
            format!("--skip={cursor}"),
            format,
        ];
        let result = self
            .execute("GitVcsDriver.listCommits", cwd, &args, true, cancellation)
            .await?;
        if result.exit_code != 0 {
            let stderr = result.stderr.to_lowercase();
            if stderr.contains("not a git repository")
                || stderr.contains("does not have any commits yet")
                || stderr.contains("bad default revision")
            {
                return Ok(VcsListCommitsResult {
                    commits: vec![],
                    next_cursor: None,
                });
            }
            return Err(command_output_error(
                "GitVcsDriver.listCommits",
                cwd,
                args.len(),
                &result,
                "Git log failed.",
            ));
        }
        let mut commits: Vec<VcsCommit> = result.stdout.lines().filter_map(parse_commit).collect();
        let has_more = commits.len() > limit;
        commits.truncate(limit);
        Ok(VcsListCommitsResult {
            commits,
            next_cursor: has_more.then_some(cursor + limit),
        })
    }

    pub async fn create_worktree(
        &self,
        input: CreateWorktreeInput,
        cancellation: &CancellationToken,
    ) -> Result<VcsCreateWorktreeResult, GitCommandError> {
        let target_ref = input
            .new_ref_name
            .as_deref()
            .unwrap_or(input.ref_name.as_str());
        let requested_path = match input.path.as_deref() {
            Some(path) => Some(resolve_explicit_worktree_path(&input.cwd, path).await),
            None => None,
        };
        let path_policy = if requested_path.is_none() {
            Some(self.worktree_path_policy(&input.cwd).await?)
        } else {
            None
        };
        let existing_ref_is_local = input.new_ref_name.is_none()
            && self
                .local_branch_exists(&input.cwd, &input.ref_name, cancellation)
                .await?;
        if existing_ref_is_local {
            let worktrees = self.worktree_map(&input.cwd, cancellation).await?;
            if worktrees.contains_key(&input.ref_name) {
                return self
                    .create_suffixed_worktree_from_occupied_branch(
                        &input.cwd,
                        &input.ref_name,
                        requested_path.as_ref(),
                        path_policy.as_ref(),
                        cancellation,
                    )
                    .await;
            }
        }

        let path = requested_path.clone().unwrap_or_else(|| {
            path_policy
                .as_ref()
                .expect("implicit worktree path has a policy")
                .path_for(&input.cwd, target_ref)
        });
        let path_string = display_path(&path);
        let new_ref_existed = if let Some(new_ref) = input.new_ref_name.as_deref() {
            self.local_branch_exists(&input.cwd, new_ref, cancellation)
                .await?
        } else {
            false
        };
        if new_ref_existed
            && let Some(new_ref) = input.new_ref_name.as_deref()
            && self
                .worktree_map(&input.cwd, cancellation)
                .await?
                .contains_key(new_ref)
        {
            return self
                .create_suffixed_worktree_from_occupied_branch(
                    &input.cwd,
                    new_ref,
                    requested_path.as_ref(),
                    path_policy.as_ref(),
                    cancellation,
                )
                .await;
        }
        let owned_path = if requested_path.is_none()
            && path_policy
                .as_ref()
                .and_then(WorktreePathPolicy::configured_base)
                .is_some()
        {
            Some(reserve_implicit_worktree_path(
                &input.cwd,
                &path,
                path_policy.as_ref().expect("implicit path has a policy"),
            )?)
        } else {
            None
        };
        let mut args = strings(&["worktree", "add"]);
        if let Some(new_ref) = input.new_ref_name.as_deref()
            && !new_ref_existed
        {
            args.extend(["-b".into(), new_ref.into()]);
        }
        let checkout_ref = if new_ref_existed {
            input
                .new_ref_name
                .as_deref()
                .expect("an existing requested branch has a name")
        } else {
            input.ref_name.as_str()
        };
        args.extend([path_string, checkout_ref.into()]);
        if let Err(mut error) = self
            .run(
                "GitVcsDriver.createWorktree",
                &input.cwd,
                &args,
                cancellation,
            )
            .await
        {
            if let Some(owned_path) = owned_path.as_ref()
                && let Err(cleanup_error) =
                    self.cleanup_owned_worktree(&input.cwd, owned_path).await
            {
                error.detail = format!(
                    "{}\nOwned worktree cleanup also failed: {}",
                    error.detail, cleanup_error.detail
                )
                .into();
            }
            if let Some(new_ref) = input.new_ref_name.as_deref()
                && !new_ref_existed
                && let Err(rollback_error) = self.rollback_created_branch(&input.cwd, new_ref).await
            {
                error.detail = format!(
                    "{}\nWorktree branch rollback also failed: {}",
                    error.detail, rollback_error.detail
                )
                .into();
            }
            if input.new_ref_name.is_none()
                && existing_ref_is_local
                && self
                    .worktree_map(&input.cwd, cancellation)
                    .await?
                    .contains_key(&input.ref_name)
            {
                return self
                    .create_suffixed_worktree_from_occupied_branch(
                        &input.cwd,
                        &input.ref_name,
                        requested_path.as_ref(),
                        path_policy.as_ref(),
                        cancellation,
                    )
                    .await;
            }
            return Err(error);
        }
        let canonical_path = tokio::fs::canonicalize(&path).await.unwrap_or(path);
        let path_string = display_path(&canonical_path);
        if let (Some(new_ref), Some(base_ref)) = (
            input.new_ref_name.as_deref(),
            input.base_ref_name.as_deref(),
        ) {
            self.run(
                "GitVcsDriver.createWorktree.configureBaseRef",
                &input.cwd,
                &[
                    "config".into(),
                    format!("branch.{new_ref}.bibcode-base-ref"),
                    base_ref.into(),
                ],
                cancellation,
            )
            .await?;
        }
        Ok(VcsCreateWorktreeResult {
            worktree: VcsWorktree {
                path: path_string,
                ref_name: target_ref.to_owned(),
            },
            rollback: Some(ManagedWorktreeRollback {
                created_path: canonical_path,
                created_branch: input.new_ref_name.filter(|_| !new_ref_existed),
            }),
        })
    }

    pub async fn ensure_worktree(
        &self,
        input: CreateWorktreeInput,
        cancellation: &CancellationToken,
    ) -> Result<VcsCreateWorktreeResult, GitCommandError> {
        let target_ref = input
            .new_ref_name
            .as_deref()
            .unwrap_or(input.ref_name.as_str());
        if let Some(path) = self
            .worktree_map(&input.cwd, cancellation)
            .await?
            .get(target_ref)
        {
            return Ok(VcsCreateWorktreeResult {
                worktree: VcsWorktree {
                    path: path.clone(),
                    ref_name: target_ref.to_owned(),
                },
                rollback: None,
            });
        }
        self.create_worktree(input, cancellation).await
    }

    async fn create_suffixed_worktree_from_occupied_branch(
        &self,
        cwd: &Path,
        base_ref: &str,
        requested_path: Option<&PathBuf>,
        path_policy: Option<&WorktreePathPolicy>,
        cancellation: &CancellationToken,
    ) -> Result<VcsCreateWorktreeResult, GitCommandError> {
        for suffix in 2..(2 + MAX_AUTOMATIC_WORKTREE_SUFFIX_ATTEMPTS) {
            let candidate = format!("{base_ref}-{suffix}");
            if self
                .local_branch_exists(cwd, &candidate, cancellation)
                .await?
            {
                continue;
            }

            let path = requested_path.cloned().unwrap_or_else(|| {
                path_policy
                    .expect("implicit worktree path has a policy")
                    .path_for(cwd, &candidate)
            });
            let owned_path = match OwnedWorktreePath::reserve(path.clone()) {
                Ok(owned_path) => owned_path,
                Err(error)
                    if requested_path.is_none()
                        && error.kind() == io::ErrorKind::AlreadyExists
                        && (path_policy
                            .and_then(WorktreePathPolicy::configured_base)
                            .is_none()
                            || error.is_destination_collision()) =>
                {
                    continue;
                }
                Err(error) => {
                    return Err(worktree_reservation_error(
                        cwd,
                        &path,
                        error,
                        path_policy.and_then(WorktreePathPolicy::configured_base),
                    ));
                }
            };
            self.prepare_detached_owned_worktree(cwd, base_ref, &owned_path, cancellation)
                .await?;

            match self
                .checkout_owned_suffixed_branch(cwd, &owned_path, &candidate, cancellation)
                .await
            {
                Ok(()) => {
                    let canonical_path = tokio::fs::canonicalize(owned_path.path())
                        .await
                        .unwrap_or(path);
                    return Ok(VcsCreateWorktreeResult {
                        worktree: VcsWorktree {
                            path: display_path(&canonical_path),
                            ref_name: candidate.clone(),
                        },
                        rollback: Some(ManagedWorktreeRollback {
                            created_path: canonical_path,
                            created_branch: Some(candidate),
                        }),
                    });
                }
                Err(error) => {
                    if cancellation.is_cancelled() {
                        return Err(error);
                    }
                    let cleanup_token = CancellationToken::new();
                    let candidate_was_claimed = self
                        .local_branch_exists(cwd, &candidate, &cleanup_token)
                        .await?;
                    if candidate_was_claimed {
                        continue;
                    }
                    return Err(error);
                }
            }
        }

        Err(simple_error(
            "GitVcsDriver.createWorktree",
            cwd,
            &format!(
                "No available worktree branch was found for '{base_ref}' after {MAX_AUTOMATIC_WORKTREE_SUFFIX_ATTEMPTS} attempts. Remove an unused suffixed branch or choose another base."
            ),
        ))
    }

    async fn prepare_detached_owned_worktree(
        &self,
        cwd: &Path,
        base_ref: &str,
        owned_path: &OwnedWorktreePath,
        cancellation: &CancellationToken,
    ) -> Result<(), GitCommandError> {
        let args = vec![
            "worktree".into(),
            "add".into(),
            "--detach".into(),
            display_path(owned_path.path()),
            base_ref.into(),
        ];
        if let Err(mut error) = self
            .run("GitVcsDriver.createWorktree", cwd, &args, cancellation)
            .await
        {
            if let Err(cleanup_error) = self.cleanup_owned_worktree(cwd, owned_path).await {
                error.detail = format!(
                    "{}\nOwned worktree cleanup also failed: {}",
                    error.detail, cleanup_error.detail
                )
                .into();
            }
            return Err(error);
        }
        Ok(())
    }

    async fn checkout_owned_suffixed_branch(
        &self,
        cwd: &Path,
        owned_path: &OwnedWorktreePath,
        candidate: &str,
        cancellation: &CancellationToken,
    ) -> Result<(), GitCommandError> {
        if let Err(mut error) = self
            .run(
                "GitVcsDriver.createWorktree.createBranch",
                owned_path.path(),
                &["checkout".into(), "-b".into(), candidate.into()],
                cancellation,
            )
            .await
        {
            let cleanup_token = CancellationToken::new();
            let canonical_owned_path = tokio::fs::canonicalize(owned_path.path())
                .await
                .unwrap_or_else(|_| owned_path.path().to_path_buf());
            let candidate_is_owned = match self.worktree_map(cwd, &cleanup_token).await {
                Ok(worktrees) => worktrees.get(candidate).is_some_and(|registered| {
                    same_worktree_path(registered, &canonical_owned_path)
                }),
                Err(inspection_error) => {
                    error.detail = format!(
                        "{}\nOwned worktree branch inspection also failed: {}",
                        error.detail, inspection_error.detail
                    )
                    .into();
                    false
                }
            };
            if let Err(cleanup_error) = self.cleanup_owned_worktree(cwd, owned_path).await {
                error.detail = format!(
                    "{}\nOwned worktree cleanup also failed: {}",
                    error.detail, cleanup_error.detail
                )
                .into();
            }
            if candidate_is_owned
                && let Err(rollback_error) = self.rollback_created_branch(cwd, candidate).await
            {
                error.detail = format!(
                    "{}\nWorktree branch rollback also failed: {}",
                    error.detail, rollback_error.detail
                )
                .into();
            }
            return Err(error);
        }
        Ok(())
    }

    async fn cleanup_owned_worktree(
        &self,
        cwd: &Path,
        owned_path: &OwnedWorktreePath,
    ) -> Result<(), GitCommandError> {
        let cleanup_token = CancellationToken::new();
        let mut args = strings(&["worktree", "remove", "--force", "--"]);
        args.push(display_path(owned_path.path()));
        let removal_error = self
            .run(
                "GitVcsDriver.createWorktree.cleanup",
                cwd,
                &args,
                &cleanup_token,
            )
            .await
            .err();
        let still_registered = self
            .worktree_paths(cwd, &cleanup_token)
            .await?
            .iter()
            .any(|registered| same_worktree_path(registered, owned_path.path()));
        if still_registered {
            return Err(removal_error.unwrap_or_else(|| {
                simple_error(
                    "GitVcsDriver.createWorktree.cleanup",
                    cwd,
                    &format!(
                        "The owned worktree '{}' is still registered after cleanup.",
                        display_path(owned_path.path())
                    ),
                )
            }));
        }
        match tokio::fs::symlink_metadata(owned_path.path()).await {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(simple_error(
                    "GitVcsDriver.createWorktree.cleanup",
                    cwd,
                    &format!(
                        "The owned worktree path '{}' could not be inspected after Git cleanup: {error}",
                        display_path(owned_path.path())
                    ),
                ));
            }
            Ok(_) => {}
        }
        let lease = open_removal_directory_lease_async(owned_path.path())
            .await
            .map_err(|error| {
                simple_error(
                    "GitVcsDriver.createWorktree.cleanup",
                    cwd,
                    &format!(
                        "The owned worktree path '{}' could not be bound for cleanup: {error}",
                        display_path(owned_path.path())
                    ),
                )
            })?;
        verify_owned_worktree_path_identity(&lease, owned_path.identity()).map_err(|error| {
            simple_error(
                "GitVcsDriver.createWorktree.cleanup",
                cwd,
                &format!(
                    "The owned worktree path '{}' was replaced before cleanup: {error}",
                    display_path(owned_path.path())
                ),
            )
        })?;
        remove_bound_owned_directory_with_lease(owned_path.path(), lease)
            .await
            .map_err(|error| {
                simple_error(
                    "GitVcsDriver.createWorktree.cleanup",
                    cwd,
                    &format!(
                        "The owned worktree path '{}' could not be removed: {error}",
                        display_path(owned_path.path())
                    ),
                )
            })
    }

    pub(crate) async fn rollback_managed_worktree_creation(
        &self,
        cwd: &Path,
        rollback: &ManagedWorktreeRollback,
    ) -> Result<(), GitCommandError> {
        let cancellation = CancellationToken::new();
        let created_key = canonical_worktree_path_key(&rollback.created_path)
            .await
            .map_err(|error| {
                simple_error(
                    "GitVcsDriver.createWorktree.rollback",
                    cwd,
                    &format!("The created worktree identity could not be verified: {error}"),
                )
            })?;
        let inventory = self.worktree_inventory(cwd, &cancellation).await?;
        let mut created_record = None;
        for record in inventory.records {
            let record_key = canonical_worktree_path_key(&record.path)
                .await
                .map_err(|error| {
                    simple_error(
                        "GitVcsDriver.createWorktree.rollback",
                        cwd,
                        &format!("A registered worktree identity could not be verified: {error}"),
                    )
                })?;
            if record_key == created_key {
                created_record = Some(record);
                break;
            }
        }
        let record = created_record.ok_or_else(|| {
            simple_error(
                "GitVcsDriver.createWorktree.rollback",
                cwd,
                "The exact created worktree is no longer registered.",
            )
        })?;
        if record.is_primary || record.is_bare {
            return Err(simple_error(
                "GitVcsDriver.createWorktree.rollback",
                cwd,
                "The exact created worktree resolved to a protected checkout.",
            ));
        }
        self.remove_worktree_verified(cwd, &record, true, &cancellation)
            .await?;
        if let Some(branch) = rollback.created_branch.as_deref() {
            self.rollback_created_branch(cwd, branch).await?;
        }
        Ok(())
    }

    async fn local_branch_exists(
        &self,
        cwd: &Path,
        branch: &str,
        cancellation: &CancellationToken,
    ) -> Result<bool, GitCommandError> {
        let output = self
            .execute(
                "GitVcsDriver.createWorktree.branchExists",
                cwd,
                &[
                    "show-ref".into(),
                    "--verify".into(),
                    "--quiet".into(),
                    format!("refs/heads/{branch}"),
                ],
                true,
                cancellation,
            )
            .await?;
        match output.exit_code {
            0 => Ok(true),
            1 => Ok(false),
            _ => Err(command_output_error(
                "GitVcsDriver.createWorktree.branchExists",
                cwd,
                4,
                &output,
                &actionable_git_failure(&output.stderr, &output.stdout),
            )),
        }
    }

    async fn rollback_created_branch(
        &self,
        cwd: &Path,
        branch: &str,
    ) -> Result<(), GitCommandError> {
        let cancellation = CancellationToken::new();
        if !self.local_branch_exists(cwd, branch, &cancellation).await? {
            return Ok(());
        }
        self.run(
            "GitVcsDriver.createWorktree.rollbackBranch",
            cwd,
            &["branch".into(), "-D".into(), "--".into(), branch.into()],
            &cancellation,
        )
        .await?;
        Ok(())
    }

    pub async fn remove_worktree(
        &self,
        cwd: &Path,
        path: &Path,
        force: bool,
        cancellation: &CancellationToken,
    ) -> Result<(), GitCommandError> {
        let registered_paths = self.worktree_paths(cwd, cancellation).await?;
        let registered_index = registered_worktree_path_index(
            "GitVcsDriver.removeWorktree.identity",
            cwd,
            &registered_paths,
            path,
        )
        .await?;
        let was_registered = registered_index.is_some();
        if force && registered_index.is_some_and(|index| index > 0) {
            let nonce = self
                .prepare_worktree_removal_identity(cwd, path, cancellation)
                .await?;
            return self
                .remove_worktree_with_identity(cwd, path, true, &nonce, cancellation)
                .await;
        }
        if force && !was_registered && self.is_managed_orphaned_worktree_path(cwd, path).await {
            let nonce = self
                .prepare_worktree_removal_identity(cwd, path, cancellation)
                .await?;
            return self
                .remove_worktree_with_identity(cwd, path, true, &nonce, cancellation)
                .await;
        }
        let mut args = strings(&["worktree", "remove"]);
        if force {
            args.push("--force".into());
        }
        args.push(display_path(path));
        let error = match self
            .run("GitVcsDriver.removeWorktree", cwd, &args, cancellation)
            .await
        {
            Ok(_) => return Ok(()),
            Err(error) => error,
        };
        let Ok(paths) = self.worktree_paths(cwd, cancellation).await else {
            return Err(error);
        };
        if !matches!(
            registered_worktree_path_index(
                "GitVcsDriver.removeWorktree.verifyIdentity",
                cwd,
                &paths,
                path,
            )
            .await,
            Ok(None)
        ) {
            return Err(error);
        }
        if !path.exists() {
            return Ok(());
        }
        if !was_registered {
            return Err(error);
        }
        Err(with_filesystem_cleanup_deferred(error, path))
    }

    async fn is_managed_orphaned_worktree_path(&self, cwd: &Path, path: &Path) -> bool {
        let Ok(metadata) = tokio::fs::symlink_metadata(path).await else {
            return false;
        };
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || tokio::fs::symlink_metadata(path.join(".git")).await.is_ok()
        {
            return false;
        }
        let Ok(path_policy) = self.worktree_path_policy(cwd).await else {
            return false;
        };
        let expected_path = path_policy.path_for(cwd, "managed-orphan-candidate");
        let Some(expected_parent) = expected_path.parent() else {
            return false;
        };
        let (Ok(canonical_expected_parent), Ok(canonical_path)) = (
            tokio::fs::canonicalize(expected_parent).await,
            tokio::fs::canonicalize(path).await,
        ) else {
            return false;
        };
        canonical_path.parent() == Some(canonical_expected_parent.as_path())
    }

    pub async fn prepare_worktree_removal_identity(
        &self,
        cwd: &Path,
        path: &Path,
        cancellation: &CancellationToken,
    ) -> Result<String, GitCommandError> {
        if cancellation.is_cancelled() {
            return Err(worktree_identity_marker_error(
                cwd,
                path,
                "removal was cancelled before identity preparation",
            ));
        }
        let admin_path = self
            .registered_linked_worktree_admin_path(cwd, path, cancellation)
            .await;
        if admin_path.is_none() && !self.is_managed_orphaned_worktree_path(cwd, path).await {
            return Err(registered_worktree_identity_error(cwd, path));
        }

        let admin_marker = admin_path
            .as_ref()
            .map(|admin_path| admin_path.join(WORKTREE_REMOVAL_IDENTITY_FILE));
        let root_marker = path.join(WORKTREE_REMOVAL_ROOT_IDENTITY_FILE);
        let admin_nonce = match &admin_marker {
            Some(marker) => read_optional_identity_marker(marker)
                .await
                .map_err(|error| worktree_identity_marker_error(cwd, path, &error.to_string()))?,
            None => None,
        };
        let root_nonce = read_optional_identity_marker(&root_marker)
            .await
            .map_err(|error| worktree_identity_marker_error(cwd, path, &error.to_string()))?;
        if let (Some(admin_nonce), Some(root_nonce)) = (&admin_nonce, &root_nonce)
            && admin_nonce != root_nonce
        {
            return Err(worktree_identity_marker_error(
                cwd,
                path,
                "the Git administration and worktree identity markers disagree",
            ));
        }
        let nonce = admin_nonce
            .or(root_nonce)
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        if Uuid::parse_str(&nonce).is_err() {
            return Err(worktree_identity_marker_error(
                cwd,
                path,
                "the existing identity marker is not a valid removal nonce",
            ));
        }
        if let Some(marker) = admin_marker
            && !tokio::fs::try_exists(&marker).await.unwrap_or(false)
        {
            write_durable_identity_marker(marker, nonce.clone())
                .await
                .map_err(|error| worktree_identity_marker_error(cwd, path, &error.to_string()))?;
        }
        if !tokio::fs::try_exists(&root_marker).await.unwrap_or(false) {
            write_durable_identity_marker(root_marker, nonce.clone())
                .await
                .map_err(|error| worktree_identity_marker_error(cwd, path, &error.to_string()))?;
        }
        Ok(nonce)
    }

    pub async fn verify_worktree_removal_identity(
        &self,
        cwd: &Path,
        path: &Path,
        expected_nonce: &str,
        cancellation: &CancellationToken,
    ) -> Result<(), GitCommandError> {
        verify_identity_marker(
            cwd,
            path,
            &path.join(WORKTREE_REMOVAL_ROOT_IDENTITY_FILE),
            expected_nonce,
        )
        .await?;
        if let Some(admin_path) = self
            .registered_linked_worktree_admin_path(cwd, path, cancellation)
            .await
        {
            verify_identity_marker(
                cwd,
                path,
                &admin_path.join(WORKTREE_REMOVAL_IDENTITY_FILE),
                expected_nonce,
            )
            .await
        } else if self.is_managed_orphaned_worktree_path(cwd, path).await {
            Ok(())
        } else {
            Err(registered_worktree_identity_error(cwd, path))
        }
    }

    pub async fn remove_worktree_with_identity(
        &self,
        cwd: &Path,
        path: &Path,
        force: bool,
        expected_nonce: &str,
        cancellation: &CancellationToken,
    ) -> Result<(), GitCommandError> {
        if !force {
            return Err(simple_error(
                "GitVcsDriver.removeWorktree",
                cwd,
                "Protected worktree removal requires explicit force confirmation.",
            ));
        }
        let quarantine = worktree_removal_quarantine_path(path, expected_nonce)
            .map_err(|error| worktree_identity_marker_error(cwd, path, &error.to_string()))?;
        let admin_quarantine = self
            .worktree_removal_admin_quarantine_path(cwd, expected_nonce, cancellation)
            .await?;
        let registered_paths = self.worktree_paths(cwd, cancellation).await?;
        let was_registered = registered_worktree_path_index(
            "GitVcsDriver.removeWorktree.identity",
            cwd,
            &registered_paths,
            path,
        )
        .await?
        .is_some();
        let path_exists = tokio::fs::symlink_metadata(path).await.is_ok();
        let quarantine_exists = tokio::fs::symlink_metadata(&quarantine).await.is_ok();

        if !quarantine_exists && !path_exists {
            if was_registered {
                return Err(worktree_identity_marker_error(
                    cwd,
                    path,
                    "Git still registers the worktree, but neither its directory nor its durable quarantine exists",
                ));
            }
            if tokio::fs::symlink_metadata(&admin_quarantine).await.is_ok() {
                let admin_lease = open_removal_directory_lease_async(&admin_quarantine)
                    .await
                    .map_err(|error| worktree_cleanup_error(cwd, &admin_quarantine, &error))?;
                remove_bound_identity_directory_with_lease(
                    &admin_quarantine,
                    WORKTREE_REMOVAL_IDENTITY_FILE,
                    expected_nonce,
                    cancellation,
                    admin_lease,
                )
                .await
                .map_err(|error| worktree_cleanup_error(cwd, &admin_quarantine, &error))?;
            }
            return Ok(());
        }

        if quarantine_exists
            && tokio::fs::symlink_metadata(quarantine.join(WORKTREE_REMOVAL_ROOT_IDENTITY_FILE))
                .await
                .is_err()
        {
            if was_registered {
                return Err(worktree_identity_marker_error(
                    cwd,
                    path,
                    "Git still registers the worktree, but its durable quarantine has no removal identity",
                ));
            }
            let quarantine_lease = open_removal_directory_lease_async(&quarantine)
                .await
                .map_err(|error| worktree_cleanup_error(cwd, &quarantine, &error))?;
            remove_bound_identity_directory_with_lease(
                &quarantine,
                WORKTREE_REMOVAL_ROOT_IDENTITY_FILE,
                expected_nonce,
                cancellation,
                quarantine_lease,
            )
            .await
            .map_err(|error| worktree_cleanup_error(cwd, &quarantine, &error))?;
            if tokio::fs::symlink_metadata(&admin_quarantine).await.is_ok() {
                let admin_lease = open_removal_directory_lease_async(&admin_quarantine)
                    .await
                    .map_err(|error| worktree_cleanup_error(cwd, &admin_quarantine, &error))?;
                remove_bound_identity_directory_with_lease(
                    &admin_quarantine,
                    WORKTREE_REMOVAL_IDENTITY_FILE,
                    expected_nonce,
                    cancellation,
                    admin_lease,
                )
                .await
                .map_err(|error| worktree_cleanup_error(cwd, &admin_quarantine, &error))?;
            }
            return Ok(());
        }

        let (quarantine_lease, tombstone_lease) = if !quarantine_exists {
            self.verify_worktree_removal_identity(cwd, path, expected_nonce, cancellation)
                .await?;
            quarantine_worktree_with_tombstone(path, &quarantine, expected_nonce, cancellation)
                .await
                .map_err(|error| worktree_removal_admission_error(cwd, path, &error))?
        } else {
            let quarantine_lease = open_removal_directory_lease_async(&quarantine)
                .await
                .map_err(|error| worktree_identity_marker_error(cwd, path, &error.to_string()))?;
            verify_identity_marker(
                cwd,
                path,
                &quarantine.join(WORKTREE_REMOVAL_ROOT_IDENTITY_FILE),
                expected_nonce,
            )
            .await?;
            let tombstone_lease =
                ensure_worktree_removal_tombstone(path, &quarantine, expected_nonce)
                    .await
                    .map_err(|error| {
                        worktree_identity_marker_error(cwd, path, &error.to_string())
                    })?;
            (quarantine_lease, tombstone_lease)
        };

        verify_identity_marker(
            cwd,
            path,
            &quarantine.join(WORKTREE_REMOVAL_ROOT_IDENTITY_FILE),
            expected_nonce,
        )
        .await?;
        let admin_quarantine_lease = if tokio::fs::symlink_metadata(&admin_quarantine).await.is_ok()
        {
            Some(
                open_removal_directory_lease_async(&admin_quarantine)
                    .await
                    .map_err(|error| {
                        worktree_identity_marker_error(cwd, path, &error.to_string())
                    })?,
            )
        } else {
            None
        };
        let admin_source = if admin_quarantine_lease.is_some() {
            None
        } else {
            self.linked_worktree_admin_path_from_quarantine(cwd, &quarantine, cancellation)
                .await
        };
        let admin_source_lease = if admin_quarantine_lease.is_none() && was_registered {
            let admin_source = admin_source
                .as_deref()
                .ok_or_else(|| registered_worktree_identity_error(cwd, path))?;
            Some(
                open_removal_directory_lease_async(admin_source)
                    .await
                    .map_err(|error| {
                        worktree_identity_marker_error(cwd, path, &error.to_string())
                    })?,
            )
        } else {
            None
        };
        let (quarantine_lease, admin_quarantine_lease) =
            deregister_worktree_admin_and_remove_tombstone(
                path,
                &quarantine,
                admin_source.as_deref(),
                &admin_quarantine,
                expected_nonce,
                WorktreeDeregistrationLeases {
                    quarantine: quarantine_lease,
                    tombstone: tombstone_lease,
                    admin_quarantine: admin_quarantine_lease,
                    admin_source: admin_source_lease,
                },
            )
            .await
            .map_err(|error| worktree_identity_marker_error(cwd, path, &error.to_string()))?;

        remove_bound_identity_directory_with_lease(
            &quarantine,
            WORKTREE_REMOVAL_ROOT_IDENTITY_FILE,
            expected_nonce,
            cancellation,
            quarantine_lease,
        )
        .await
        .map_err(|error| worktree_cleanup_error(cwd, &quarantine, &error))?;
        if let Some(admin_lease) = admin_quarantine_lease {
            remove_bound_identity_directory_with_lease(
                &admin_quarantine,
                WORKTREE_REMOVAL_IDENTITY_FILE,
                expected_nonce,
                cancellation,
                admin_lease,
            )
            .await
            .map_err(|error| worktree_cleanup_error(cwd, &admin_quarantine, &error))?;
        }
        Ok(())
    }

    async fn worktree_removal_admin_quarantine_path(
        &self,
        cwd: &Path,
        nonce: &str,
        cancellation: &CancellationToken,
    ) -> Result<PathBuf, GitCommandError> {
        let output = self
            .run(
                "GitVcsDriver.removeWorktree.commonDir",
                cwd,
                &strings(&["rev-parse", "--git-common-dir"]),
                cancellation,
            )
            .await?;
        let common_dir = resolve_git_metadata_path(cwd, output.stdout.trim());
        let common_dir = tokio::fs::canonicalize(&common_dir)
            .await
            .unwrap_or(common_dir);
        Ok(common_dir.join("bibcode-worktree-removals").join(nonce))
    }

    async fn linked_worktree_admin_path_from_quarantine(
        &self,
        cwd: &Path,
        quarantine: &Path,
        cancellation: &CancellationToken,
    ) -> Option<PathBuf> {
        let git_file = quarantine.join(".git");
        let metadata = tokio::fs::symlink_metadata(&git_file).await.ok()?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return None;
        }
        let contents = tokio::fs::read_to_string(&git_file).await.ok()?;
        let raw_admin_path = contents.trim().strip_prefix("gitdir: ")?;
        let canonical_admin_path =
            tokio::fs::canonicalize(resolve_git_metadata_path(quarantine, raw_admin_path))
                .await
                .ok()?;
        let output = self
            .run(
                "GitVcsDriver.removeWorktree.commonDir",
                cwd,
                &strings(&["rev-parse", "--git-common-dir"]),
                cancellation,
            )
            .await
            .ok()?;
        let common_dir = resolve_git_metadata_path(cwd, output.stdout.trim());
        let canonical_worktrees_dir = tokio::fs::canonicalize(common_dir.join("worktrees"))
            .await
            .ok()?;
        canonical_admin_path
            .parent()
            .is_some_and(|parent| same_path(parent, &canonical_worktrees_dir))
            .then_some(canonical_admin_path)
    }

    async fn registered_linked_worktree_admin_path(
        &self,
        cwd: &Path,
        path: &Path,
        cancellation: &CancellationToken,
    ) -> Option<PathBuf> {
        let git_file = path.join(".git");
        let Ok(metadata) = tokio::fs::symlink_metadata(&git_file).await else {
            return None;
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return None;
        }
        let Ok(contents) = tokio::fs::read_to_string(&git_file).await else {
            return None;
        };
        let raw_admin_path = contents.trim().strip_prefix("gitdir: ")?;
        let admin_path = resolve_git_metadata_path(path, raw_admin_path);
        let Ok(canonical_admin_path) = tokio::fs::canonicalize(&admin_path).await else {
            return None;
        };
        let Ok(common_dir_output) = self
            .run(
                "GitVcsDriver.removeWorktree.commonDir",
                cwd,
                &strings(&["rev-parse", "--git-common-dir"]),
                cancellation,
            )
            .await
        else {
            return None;
        };
        let common_dir = resolve_git_metadata_path(cwd, common_dir_output.stdout.trim());
        let Ok(canonical_worktrees_dir) =
            tokio::fs::canonicalize(common_dir.join("worktrees")).await
        else {
            return None;
        };
        if !canonical_admin_path
            .parent()
            .is_some_and(|parent| same_path(parent, &canonical_worktrees_dir))
        {
            return None;
        }
        let Ok(backlink) = tokio::fs::read_to_string(canonical_admin_path.join("gitdir")).await
        else {
            return None;
        };
        let backlink_path = resolve_git_metadata_path(&canonical_admin_path, backlink.trim());
        let (Ok(canonical_backlink), Ok(canonical_git_file)) = (
            tokio::fs::canonicalize(backlink_path).await,
            tokio::fs::canonicalize(git_file).await,
        ) else {
            return None;
        };
        same_path(&canonical_backlink, &canonical_git_file).then_some(canonical_admin_path)
    }

    pub async fn create_ref(
        &self,
        cwd: &Path,
        ref_name: &str,
        switch_ref: bool,
        cancellation: &CancellationToken,
    ) -> Result<String, GitCommandError> {
        let args = if switch_ref {
            vec!["switch".into(), "-c".into(), ref_name.into()]
        } else {
            vec!["branch".into(), ref_name.into()]
        };
        self.run("GitVcsDriver.createRef", cwd, &args, cancellation)
            .await?;
        Ok(ref_name.to_owned())
    }

    pub(crate) async fn git_manager_create_branch(
        &self,
        cwd: &Path,
        name: &str,
        start_point: Option<&str>,
        checkout: bool,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        let mut args = if checkout {
            vec!["switch".into(), "-c".into(), name.into()]
        } else {
            vec!["branch".into(), name.into()]
        };
        if let Some(start_point) = start_point {
            args.push(start_point.into());
        }
        // A new branch never tracks its start point: one created from `origin/main`
        // would otherwise pull from and push to `main`.
        args.push("--no-track".into());
        self.execute("GitManager.branchCreate", cwd, &args, true, cancellation)
            .await
    }

    pub(crate) async fn git_manager_checkout_local_branch(
        &self,
        cwd: &Path,
        name: &str,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.execute(
            "GitManager.branchCheckout.local",
            cwd,
            &["switch".into(), name.into()],
            true,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_checkout_remote_branch(
        &self,
        cwd: &Path,
        local_name: &str,
        remote_ref: &str,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.execute(
            "GitManager.branchCheckout.remote",
            cwd,
            &[
                "switch".into(),
                "-c".into(),
                local_name.into(),
                "--track".into(),
                remote_ref.into(),
            ],
            true,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_rename_branch(
        &self,
        cwd: &Path,
        old_name: &str,
        new_name: &str,
        cancellation: &CancellationToken,
    ) -> Result<Vec<ProcessOutput>, GitCommandError> {
        let first = self
            .execute(
                "GitManager.branchRename",
                cwd,
                &[
                    "branch".into(),
                    "-m".into(),
                    old_name.into(),
                    new_name.into(),
                ],
                true,
                cancellation,
            )
            .await?;
        let retry_case_only =
            first.exit_code != 0 && old_name != new_name && old_name.eq_ignore_ascii_case(new_name);
        if !retry_case_only {
            return Ok(vec![first]);
        }
        let retry = self
            .execute(
                "GitManager.branchRename.forceCase",
                cwd,
                &[
                    "branch".into(),
                    "-M".into(),
                    old_name.into(),
                    new_name.into(),
                ],
                true,
                cancellation,
            )
            .await?;
        Ok(vec![first, retry])
    }

    pub(crate) async fn git_manager_delete_branch(
        &self,
        cwd: &Path,
        name: &str,
        force: bool,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.execute(
            "GitManager.branchDelete",
            cwd,
            &[
                "branch".into(),
                if force { "-D" } else { "-d" }.into(),
                name.into(),
            ],
            true,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_delete_remote_branch(
        &self,
        cwd: &Path,
        remote: &str,
        name: &str,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.for_network_transfer()
            .execute(
                "GitManager.remoteBranchDelete",
                cwd,
                &["push".into(), remote.into(), format!(":{name}")],
                true,
                cancellation,
            )
            .await
    }

    pub(crate) async fn git_manager_fetch(
        &self,
        cwd: &Path,
        remote: &str,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.for_network_transfer()
            .execute(
                "GitManager.fetch",
                cwd,
                &[
                    "fetch".into(),
                    "--prune".into(),
                    "--recurse-submodules=on-demand".into(),
                    remote.into(),
                ],
                true,
                cancellation,
            )
            .await
    }

    /// Lists the tags a remote currently advertises. Network-bound like fetch,
    /// non-interactive, and a non-zero exit is returned to the caller so an
    /// unreachable remote can be reported instead of failing the read.
    pub(crate) async fn git_manager_ls_remote_tags(
        &self,
        cwd: &Path,
        remote: &str,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.execute(
            "GitManager.tags.listRemote",
            cwd,
            &["ls-remote".into(), "--tags".into(), remote.into()],
            true,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_pull(
        &self,
        cwd: &Path,
        remote: &str,
        cancellation: &CancellationToken,
    ) -> Result<Vec<ProcessOutput>, GitCommandError> {
        let pull_ff = self
            .execute(
                "GitManager.pull.pullFf",
                cwd,
                &strings(&["config", "--get", "pull.ff"]),
                true,
                cancellation,
            )
            .await?;
        let pull_rebase = self
            .execute(
                "GitManager.pull.pullRebase",
                cwd,
                &strings(&["config", "--get", "pull.rebase"]),
                true,
                cancellation,
            )
            .await?;
        let mut args = strings(&["-c", "rebase.backend=merge", "pull"]);
        if pull_ff.exit_code != 0 && pull_rebase.exit_code != 0 {
            args.push("--ff".into());
        }
        args.extend(strings(&["--recurse-submodules", remote]));
        let pull = self
            .for_network_transfer()
            .execute("GitManager.pull", cwd, &args, true, cancellation)
            .await?;
        Ok(vec![pull_ff, pull_rebase, pull])
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn git_manager_push(
        &self,
        cwd: &Path,
        remote: &str,
        local_branch: &str,
        remote_branch: Option<&str>,
        set_upstream: bool,
        force_with_lease: bool,
        push_tags: bool,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        let refspec = remote_branch.map_or_else(
            || local_branch.to_owned(),
            |remote_branch| format!("{local_branch}:{remote_branch}"),
        );
        let mut args = vec!["push".into(), remote.into(), refspec];
        if set_upstream {
            args.push("--set-upstream".into());
        }
        if force_with_lease {
            args.push("--force-with-lease".into());
        }
        if push_tags {
            // All local tags travel with the branch; `--atomic` keeps a tag the
            // remote rejects from leaving the branch half-pushed.
            args.push("--tags".into());
            args.push("--atomic".into());
        }
        self.for_network_transfer()
            .execute("GitManager.push", cwd, &args, true, cancellation)
            .await
    }

    pub(crate) async fn git_manager_stash_push(
        &self,
        cwd: &Path,
        message: &str,
        paths: &[String],
        include_untracked: bool,
        cancellation: &CancellationToken,
    ) -> Result<Vec<ProcessOutput>, GitCommandError> {
        let mut outputs = Vec::new();
        if !paths.is_empty() {
            let mut args = strings(&["add", "--"]);
            args.extend(paths.iter().cloned());
            outputs.push(
                self.execute("GitManager.stashPush.stage", cwd, &args, true, cancellation)
                    .await?,
            );
            if outputs.last().is_some_and(|output| output.exit_code != 0) {
                return Ok(outputs);
            }
        }
        let mut args = strings(&["stash", "push"]);
        if include_untracked {
            // Untracked files only; `--all` would also sweep ignored files.
            args.push("--include-untracked".into());
        }
        args.extend(["-m".into(), message.into()]);
        outputs.push(
            self.execute("GitManager.stashPush", cwd, &args, true, cancellation)
                .await?,
        );
        Ok(outputs)
    }

    pub(crate) async fn git_manager_stash_apply(
        &self,
        cwd: &Path,
        selector: &str,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.execute(
            "GitManager.stashApply",
            cwd,
            &[
                "stash".into(),
                "apply".into(),
                "--quiet".into(),
                selector.into(),
            ],
            true,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_stash_pop(
        &self,
        cwd: &Path,
        selector: &str,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.execute(
            "GitManager.stashPop",
            cwd,
            &[
                "stash".into(),
                "pop".into(),
                "--quiet".into(),
                selector.into(),
            ],
            true,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_stash_drop(
        &self,
        cwd: &Path,
        selector: &str,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.execute(
            "GitManager.stashDrop",
            cwd,
            &["stash".into(), "drop".into(), selector.into()],
            true,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_merge(
        &self,
        cwd: &Path,
        source: &str,
        no_verify: bool,
        squash: bool,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        let mut args = vec!["merge".to_owned()];
        if squash {
            // `--squash` never commits, but `merge.ff` and `branch.<name>.mergeOptions`
            // can inject `--no-ff`, `--ff-only`, or `--commit`, each of which makes Git
            // reject or abort the squash, so its controls are pinned explicitly too.
            args.extend(strings(&["--squash", "--ff", "--no-commit"]));
        } else {
            // "Merge commit" mode must always record a merge commit. Git's default
            // fast-forwards when it can, and `branch.<name>.mergeOptions` may inject
            // `--no-commit` or `--squash`, so every control is pinned on the command
            // line. `--no-ff` cannot combine with `--squash`, so squash mode stays apart.
            args.extend(strings(&[
                "--no-ff",
                "--no-squash",
                "--commit",
                "--no-edit",
            ]));
        }
        if no_verify {
            args.push("--no-verify".to_owned());
        }
        args.push(source.to_owned());
        self.execute("GitManager.merge", cwd, &args, true, cancellation)
            .await
    }

    pub(crate) async fn git_manager_is_ancestor(
        &self,
        cwd: &Path,
        ancestor: &str,
        descendant: &str,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.git_manager_bounded_read(
            "GitManager.mergeInto.isAncestor",
            cwd,
            &[
                "merge-base".into(),
                "--is-ancestor".into(),
                ancestor.into(),
                descendant.into(),
            ],
            true,
            4 * 1024,
            cancellation,
        )
        .await
    }

    /// `commit.gpgSign`; commit-tree ignores it, so callers pass `-S` themselves. Exit 1
    /// means the key is unset; any other non-zero exit (an invalid boolean) is not "off".
    pub(crate) async fn git_manager_commit_gpg_sign(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.git_manager_bounded_read(
            "GitManager.mergeInto.gpgSign",
            cwd,
            &strings(&["config", "--bool", "--get", "commit.gpgSign"]),
            true,
            4 * 1024,
            cancellation,
        )
        .await
    }

    /// Exit 0 prints the branch a symbolic ref points at; exit 1 means a regular ref.
    pub(crate) async fn git_manager_symbolic_ref(
        &self,
        cwd: &Path,
        reference: &str,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.git_manager_bounded_read(
            "GitManager.mergeInto.symbolicRef",
            cwd,
            &["symbolic-ref".into(), "-q".into(), reference.into()],
            true,
            4 * 1024,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_commit_tree(
        &self,
        cwd: &Path,
        tree: &str,
        parents: [&str; 2],
        message: &str,
        sign: bool,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        let mut args = vec!["commit-tree".to_owned()];
        if sign {
            args.push("-S".to_owned());
        }
        args.extend([
            tree.to_owned(),
            "-p".to_owned(),
            parents[0].to_owned(),
            "-p".to_owned(),
            parents[1].to_owned(),
            "-F".to_owned(),
            "-".to_owned(),
        ]);
        self.execute_with_stdin(
            "GitManager.mergeInto.commitTree",
            cwd,
            &args,
            message.as_bytes().to_vec(),
            true,
            cancellation,
        )
        .await
    }

    /// Fast-forward-only publish: Git refuses a branch checked out, rebased, or bisected
    /// in any worktree, which `update-ref` would not (docs/architecture/overview.md).
    pub(crate) async fn git_manager_publish_merge(
        &self,
        cwd: &Path,
        commit: &str,
        target: &str,
        source: &str,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        let mut environment = git_environment();
        environment.push((
            "GIT_REFLOG_ACTION".into(),
            format!("merge-into {source}").into(),
        ));
        self.execute_with_environment(
            "GitManager.mergeInto.publish",
            cwd,
            &[
                "-c".into(),
                "maintenance.auto=false".into(),
                "-c".into(),
                "fetch.writeCommitGraph=false".into(),
                // A configured bundle URI would otherwise be downloaded and imported
                // into refs/bundles/ by this local self-fetch.
                "-c".into(),
                "fetch.bundleURI=".into(),
                "fetch".into(),
                "--no-tags".into(),
                "--no-prune".into(),
                "--no-recurse-submodules".into(),
                "--no-write-fetch-head".into(),
                "--quiet".into(),
                ".".into(),
                format!("{commit}:refs/heads/{target}"),
            ],
            GitExecutionOptions {
                allow_non_zero_exit: true,
                max_output_bytes: DEFAULT_OUTPUT_LIMIT,
                output_policy: OutputPolicy::Truncate,
            },
            environment,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_squash_merge_commit(
        &self,
        cwd: &Path,
        no_verify: bool,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        let mut args = strings(&["commit", "--no-edit"]);
        if no_verify {
            args.push("--no-verify".to_owned());
        }
        self.execute(
            "GitManager.squashMerge.commit",
            cwd,
            &args,
            true,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_rebase(
        &self,
        cwd: &Path,
        base: &str,
        target: &str,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.execute(
            "GitManager.rebase",
            cwd,
            &[
                "-c".to_owned(),
                "rebase.backend=merge".to_owned(),
                "rebase".to_owned(),
                base.to_owned(),
                target.to_owned(),
            ],
            true,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_interactive_rebase(
        &self,
        cwd: &Path,
        todo: &str,
        message: Option<&str>,
        last_retained_commit_ref: Option<&str>,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        let operation = if message.is_some() {
            "GitManager.squash"
        } else {
            "GitManager.reorder"
        };
        let files = create_rewrite_temp_files(operation, cwd, todo, message).await?;
        let sequence_editor = editor_redirect_command(&files.todo);
        let mut environment = git_environment();
        environment.push((
            "GIT_EDITOR".into(),
            files.message.as_ref().map_or_else(
                || OsString::from(":"),
                |path| editor_redirect_command(path).into(),
            ),
        ));
        let mut args = strings(&[
            "-c",
            "rebase.backend=merge",
            "-c",
            &format!("sequence.editor={sequence_editor}"),
            "rebase",
            "-i",
        ]);
        args.push(last_retained_commit_ref.map_or_else(|| "--root".to_owned(), str::to_owned));
        self.execute_with_environment(
            operation,
            cwd,
            &args,
            GitExecutionOptions {
                allow_non_zero_exit: true,
                max_output_bytes: DEFAULT_OUTPUT_LIMIT,
                output_policy: OutputPolicy::Truncate,
            },
            environment,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_cherry_pick(
        &self,
        cwd: &Path,
        shas: &[String],
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        let mut args = strings(&["cherry-pick", "--empty=keep", "-m", "1"]);
        args.extend(shas.iter().cloned());
        self.execute("GitManager.cherryPick", cwd, &args, true, cancellation)
            .await
    }

    pub(crate) async fn git_manager_revert(
        &self,
        cwd: &Path,
        sha: &str,
        merge_commit: bool,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        let mut args = vec!["revert".to_owned()];
        if merge_commit {
            args.extend(strings(&["-m", "1"]));
        }
        args.push(sha.to_owned());
        self.execute("GitManager.revert", cwd, &args, true, cancellation)
            .await
    }

    pub(crate) async fn git_manager_reset(
        &self,
        cwd: &Path,
        sha: &str,
        mode: &str,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        self.execute(
            "GitManager.reset",
            cwd,
            &["reset".to_owned(), format!("--{mode}"), sha.to_owned()],
            true,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_rewrite_log_order(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<Vec<String>, GitCommandError> {
        let output = self
            .execute(
                "GitManager.rewrite.logOrder",
                cwd,
                &strings(&["rev-list", "--first-parent", "HEAD", "--"]),
                false,
                cancellation,
            )
            .await?;
        Ok(output
            .stdout
            .lines()
            .map(str::trim)
            .filter(|sha| !sha.is_empty())
            .map(str::to_owned)
            .collect())
    }

    pub(crate) async fn git_manager_commit_parent(
        &self,
        cwd: &Path,
        sha: &str,
        cancellation: &CancellationToken,
    ) -> Result<Option<String>, GitCommandError> {
        let output = self
            .execute(
                "GitManager.rewrite.parent",
                cwd,
                &[
                    "rev-list".to_owned(),
                    "--parents".to_owned(),
                    "-n".to_owned(),
                    "1".to_owned(),
                    sha.to_owned(),
                    "--".to_owned(),
                ],
                false,
                cancellation,
            )
            .await?;
        let mut fields = output.stdout.split_whitespace();
        if fields.next() != Some(sha) {
            return Err(simple_error(
                "GitManager.rewrite.parent",
                cwd,
                "Git returned malformed rewrite parent data.",
            ));
        }
        Ok(fields.next().map(str::to_owned))
    }

    pub(crate) async fn git_manager_is_merge_commit(
        &self,
        cwd: &Path,
        sha: &str,
        cancellation: &CancellationToken,
    ) -> Result<bool, GitCommandError> {
        let output = self
            .execute(
                "GitManager.revert.parents",
                cwd,
                &[
                    "show".to_owned(),
                    "-s".to_owned(),
                    "--format=%P".to_owned(),
                    sha.to_owned(),
                    "--".to_owned(),
                ],
                false,
                cancellation,
            )
            .await?;
        Ok(output.stdout.split_whitespace().nth(1).is_some())
    }

    pub(crate) async fn git_manager_index_is_empty(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<bool, GitCommandError> {
        let output = self
            .execute(
                "GitManager.continue.staged",
                cwd,
                &strings(&["diff", "--cached", "--quiet", "--"]),
                true,
                cancellation,
            )
            .await?;
        match output.exit_code {
            0 => Ok(true),
            1 => Ok(false),
            _ => Err(simple_error(
                "GitManager.continue.staged",
                cwd,
                "Git could not inspect the staged conflict resolution.",
            )),
        }
    }

    pub(crate) async fn git_manager_continue(
        &self,
        cwd: &Path,
        operation: &str,
        skip_empty: bool,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        let args = match operation {
            "rebase" => vec![
                "-c".to_owned(),
                "rebase.backend=merge".to_owned(),
                "rebase".to_owned(),
                if skip_empty { "--skip" } else { "--continue" }.to_owned(),
            ],
            "cherry-pick" => strings(&["cherry-pick", "--continue"]),
            "revert" => strings(&["revert", "--continue"]),
            "merge" => strings(&["commit", "--no-edit", "--cleanup=strip"]),
            _ => {
                return Err(simple_error(
                    "GitManager.continue",
                    cwd,
                    "Git Manager received an unsupported continue operation.",
                ));
            }
        };
        let mut environment = git_environment();
        environment.push(("GIT_EDITOR".into(), ":".into()));
        self.execute_with_environment(
            "GitManager.continue",
            cwd,
            &args,
            GitExecutionOptions {
                allow_non_zero_exit: true,
                max_output_bytes: DEFAULT_OUTPUT_LIMIT,
                output_policy: OutputPolicy::Truncate,
            },
            environment,
            cancellation,
        )
        .await
    }

    pub(crate) async fn git_manager_abort(
        &self,
        cwd: &Path,
        operation: &str,
        cancellation: &CancellationToken,
    ) -> Result<ProcessOutput, GitCommandError> {
        let args = match operation {
            "rebase" => strings(&["-c", "rebase.backend=merge", "rebase", "--abort"]),
            "cherry-pick" => strings(&["cherry-pick", "--abort"]),
            "revert" => strings(&["revert", "--abort"]),
            "merge" => strings(&["merge", "--abort"]),
            _ => {
                return Err(simple_error(
                    "GitManager.abort",
                    cwd,
                    "Git Manager received an unsupported abort operation.",
                ));
            }
        };
        self.execute("GitManager.abort", cwd, &args, true, cancellation)
            .await
    }

    pub(crate) async fn git_manager_resolve_conflict(
        &self,
        cwd: &Path,
        path: &str,
        side: super::manager::conflicts::ConflictSide,
        selected_side_deleted: bool,
        cancellation: &CancellationToken,
    ) -> Result<Vec<ProcessOutput>, GitCommandError> {
        let plan =
            super::manager::conflicts::plan_manual_resolution(path, side, selected_side_deleted);
        let checkout = self
            .execute(
                "GitManager.resolveConflict.checkout",
                cwd,
                &plan[0],
                true,
                cancellation,
            )
            .await?;
        if checkout.exit_code != 0 && !selected_side_deleted {
            return Ok(vec![checkout]);
        }
        let stage = self
            .execute(
                "GitManager.resolveConflict.stage",
                cwd,
                &plan[1],
                true,
                cancellation,
            )
            .await?;
        Ok(vec![checkout, stage])
    }

    pub async fn switch_ref(
        &self,
        cwd: &Path,
        ref_name: &str,
        cancellation: &CancellationToken,
    ) -> Result<Option<String>, GitCommandError> {
        self.run(
            "GitVcsDriver.switchRef",
            cwd,
            &["switch".into(), ref_name.into()],
            cancellation,
        )
        .await?;
        self.current_ref(cwd, cancellation).await
    }

    pub async fn rename_ref(
        &self,
        cwd: &Path,
        old_ref: &str,
        new_ref: &str,
        cancellation: &CancellationToken,
    ) -> Result<String, GitCommandError> {
        if old_ref == new_ref {
            return Ok(new_ref.to_owned());
        }
        self.run(
            "GitVcsDriver.renameBranch",
            cwd,
            &["branch".into(), "-m".into(), old_ref.into(), new_ref.into()],
            cancellation,
        )
        .await?;
        Ok(new_ref.to_owned())
    }

    pub async fn init(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<(), GitCommandError> {
        self.run(
            "GitVcsDriver.initRepo",
            cwd,
            &strings(&["init"]),
            cancellation,
        )
        .await?;
        Ok(())
    }

    /// Clones `url` into `parent_dir` without serializing against other clones of the same
    /// destination. Production RPCs go through `CloneRuntime`, which owns one clone per
    /// destination; this primitive serves tests and the runtime-free coverage suite. Cancelling
    /// `cancellation` or dropping the future stops Git and removes the folder this call created.
    pub async fn clone_repository(
        &self,
        url: &str,
        parent_dir: &Path,
        directory_name: Option<&str>,
        cancellation: &CancellationToken,
    ) -> Result<PathBuf, GitCommandError> {
        let leaf = clone_destination_leaf(url, directory_name)
            .map_err(|error| simple_error(CLONE_OPERATION, parent_dir, &error.to_string()))?;
        let reserved = match self
            .reserve_clone_destination(url, parent_dir, &leaf, cancellation)
            .await?
        {
            CloneReservation::Existing(path) => return Ok(path),
            CloneReservation::Reserved(reserved) => reserved,
        };
        let transfer_cancellation = cancellation.child_token();
        let _cancel_transfer_on_drop = transfer_cancellation.clone().drop_guard();
        let task = tokio::spawn(async move { reserved.run(&transfer_cancellation).await });
        match task.await {
            Ok(result) => result,
            Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            Err(error) => Err(simple_error(
                CLONE_OPERATION,
                parent_dir,
                &format!("The clone stopped before it finished: {error}"),
            )),
        }
    }

    /// Creates the destination atomically, which records that it did not exist before. An
    /// existing destination is never cloned into or deleted: a finished clone of the same URL
    /// is returned as [`CloneReservation::Existing`], anything else is an actionable error.
    pub(crate) async fn reserve_clone_destination(
        &self,
        url: &str,
        parent_dir: &Path,
        leaf: &CloneLeaf,
        cancellation: &CancellationToken,
    ) -> Result<CloneReservation, GitCommandError> {
        let destination = parent_dir.join(leaf.as_str());
        match OwnedWorktreePath::reserve(destination.clone()) {
            Ok(owned) => Ok(CloneReservation::Reserved(ReservedClone {
                repository: self.for_network_transfer(),
                url: url.to_owned(),
                parent_dir: parent_dir.to_path_buf(),
                leaf: leaf.clone(),
                destination: owned,
            })),
            Err(error) if error.is_destination_collision() => self
                .reuse_existing_destination(url, parent_dir, &destination, cancellation)
                .await
                .map(CloneReservation::Existing),
            Err(error) => Err(simple_error(
                CLONE_OPERATION,
                parent_dir,
                &format!(
                    "Could not create {} ({}). Check that the parent folder exists and is writable, or choose another folder.",
                    display_path(&destination),
                    error.source
                ),
            )),
        }
    }

    /// The reuse check without reserving: `Ok(None)` when nothing exists at the destination,
    /// the finished clone's path, or the error that names why the folder cannot be reused.
    pub(crate) async fn inspect_clone_destination(
        &self,
        url: &str,
        parent_dir: &Path,
        leaf: &CloneLeaf,
        cancellation: &CancellationToken,
    ) -> Result<Option<PathBuf>, GitCommandError> {
        let destination = parent_dir.join(leaf.as_str());
        match tokio::fs::symlink_metadata(&destination).await {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            _ => self
                .reuse_existing_destination(url, parent_dir, &destination, cancellation)
                .await
                .map(Some),
        }
    }

    async fn reuse_existing_destination(
        &self,
        url: &str,
        parent_dir: &Path,
        destination: &Path,
        cancellation: &CancellationToken,
    ) -> Result<PathBuf, GitCommandError> {
        match tokio::fs::symlink_metadata(destination).await {
            Ok(metadata) if metadata.is_dir() => {
                self.reuse_existing_clone(url, destination, cancellation)
                    .await
            }
            Ok(metadata) => {
                let kind = if metadata.file_type().is_symlink() {
                    "A link"
                } else if metadata.is_file() {
                    "A file"
                } else {
                    "An item"
                };
                let (name, location) = destination_name_and_location(destination);
                Err(simple_error(
                    CLONE_OPERATION,
                    parent_dir,
                    &format!(
                        "{kind} named {name} already exists at {location}. Remove it or choose another parent folder."
                    ),
                ))
            }
            Err(error) => Err(simple_error(
                CLONE_OPERATION,
                parent_dir,
                &format!("Failed to inspect the existing clone destination: {error}"),
            )),
        }
    }

    async fn reuse_existing_clone(
        &self,
        url: &str,
        destination: &Path,
        cancellation: &CancellationToken,
    ) -> Result<PathBuf, GitCommandError> {
        let Some(repository_root) = self.repository_root(destination, cancellation).await? else {
            let (name, location) = destination_name_and_location(destination);
            return Err(simple_error(
                CLONE_OPERATION,
                destination,
                &format!(
                    "A folder named {name} already exists at {location} and is not a Git repository. Remove it or choose another parent folder."
                ),
            ));
        };
        let canonical_destination =
            tokio::fs::canonicalize(destination)
                .await
                .map_err(|error| {
                    simple_error(
                        CLONE_OPERATION,
                        destination,
                        &format!("Failed to inspect the existing clone destination: {error}"),
                    )
                })?;
        let canonical_repository_root =
            tokio::fs::canonicalize(&repository_root)
                .await
                .map_err(|error| {
                    simple_error(
                        CLONE_OPERATION,
                        destination,
                        &format!("Failed to inspect the existing Git repository root: {error}"),
                    )
                })?;
        if canonical_destination != canonical_repository_root {
            // The folder belongs to another repository, so removing it is not suggested.
            let (name, location) = destination_name_and_location(destination);
            return Err(simple_error(
                CLONE_OPERATION,
                destination,
                &format!(
                    "A folder named {name} already exists at {location} inside another Git repository. Choose another parent folder."
                ),
            ));
        }
        let origin = self
            .execute(
                "GitVcsDriver.clone.inspectOrigin",
                destination,
                &strings(&["config", "--get", "remote.origin.url"]),
                true,
                cancellation,
            )
            .await?;
        if origin.exit_code != 0 {
            let (name, location) = destination_name_and_location(destination);
            return Err(simple_error(
                CLONE_OPERATION,
                destination,
                &format!(
                    "A Git repository named {name} already exists at {location} without an origin remote. Choose another parent folder, or add that folder as a project instead."
                ),
            ));
        }
        if origin.stdout.trim() != url {
            let (name, location) = destination_name_and_location(destination);
            return Err(simple_error(
                CLONE_OPERATION,
                destination,
                &format!(
                    "A Git repository named {name} already exists at {location} with a different origin. Choose another parent folder."
                ),
            ));
        }
        let incomplete = || {
            simple_error(
                CLONE_OPERATION,
                destination,
                &format!(
                    "An incomplete clone exists at {}. Remove it or choose another folder.",
                    display_path(destination)
                ),
            )
        };
        // An interrupted clone keeps its origin but never checks out a commit: HEAD stays
        // Git's `refs/heads/.invalid` placeholder, or names a branch without commits.
        let head = self
            .execute_read(
                "GitVcsDriver.clone.inspectHead",
                destination,
                &strings(&["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]),
                true,
                cancellation,
            )
            .await?;
        if head.exit_code != 0 || head.stdout.trim().is_empty() {
            return Err(incomplete());
        }
        // A clone killed during checkout has a resolvable HEAD but no index: Git writes the
        // index once, after checkout, even for an empty tree. A stale `index.lock` may belong
        // to a running Git command, so only the missing index marks the clone incomplete.
        let index = self
            .execute_read(
                "GitVcsDriver.clone.inspectIndex",
                destination,
                &strings(&["rev-parse", "--git-path", "index"]),
                true,
                cancellation,
            )
            .await?;
        let index_path = PathBuf::from(index.stdout.trim());
        let index_path = if index_path.is_absolute() {
            index_path
        } else {
            destination.join(index_path)
        };
        let has_index = index.exit_code == 0
            && !index.stdout.trim().is_empty()
            && tokio::fs::metadata(&index_path)
                .await
                .is_ok_and(|metadata| metadata.is_file());
        if !has_index {
            return Err(incomplete());
        }
        Ok(destination.to_path_buf())
    }

    pub async fn pull_current_branch(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<VcsPullResult, GitCommandError> {
        let ref_name = self.current_ref(cwd, cancellation).await?.ok_or_else(|| {
            simple_error(
                "GitVcsDriver.pullCurrentBranch",
                cwd,
                "Cannot pull from detached HEAD.",
            )
        })?;
        let upstream = self
            .execute(
                "GitVcsDriver.pullCurrentBranch.upstream",
                cwd,
                &strings(&["rev-parse", "--abbrev-ref", "@{upstream}"]),
                true,
                cancellation,
            )
            .await?;
        let upstream_ref = (upstream.exit_code == 0)
            .then(|| upstream.stdout.trim().to_owned())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                simple_error(
                    "GitVcsDriver.pullCurrentBranch",
                    cwd,
                    "Current branch has no upstream.",
                )
            })?;
        let before = self
            .run(
                "GitVcsDriver.pullCurrentBranch.before",
                cwd,
                &strings(&["rev-parse", "HEAD"]),
                cancellation,
            )
            .await?;
        self.for_bounded_transfer()
            .run(
                "GitVcsDriver.pullCurrentBranch",
                cwd,
                &strings(&["pull", "--ff-only"]),
                cancellation,
            )
            .await?;
        let after = self
            .run(
                "GitVcsDriver.pullCurrentBranch.after",
                cwd,
                &strings(&["rev-parse", "HEAD"]),
                cancellation,
            )
            .await?;
        Ok(VcsPullResult {
            status: if before.stdout.trim() == after.stdout.trim() {
                PullStatus::SkippedUpToDate
            } else {
                PullStatus::Pulled
            },
            ref_name,
            upstream_ref: Some(upstream_ref),
        })
    }

    pub async fn commit(
        &self,
        cwd: &Path,
        message: &str,
        file_paths: Option<&[String]>,
        preserve_index: bool,
        cancellation: &CancellationToken,
    ) -> Result<Option<String>, GitCommandError> {
        if !preserve_index {
            if let Some(paths) = file_paths.filter(|paths| !paths.is_empty()) {
                let _ = self
                    .execute(
                        "GitVcsDriver.prepareCommitContext.reset",
                        cwd,
                        &strings(&["reset"]),
                        true,
                        cancellation,
                    )
                    .await?;
                let mut args = strings(&["add", "-A", "--"]);
                args.extend(paths.iter().cloned());
                self.run(
                    "GitVcsDriver.prepareCommitContext.addSelected",
                    cwd,
                    &args,
                    cancellation,
                )
                .await?;
            } else {
                self.run(
                    "GitVcsDriver.prepareCommitContext.addAll",
                    cwd,
                    &strings(&["add", "-A"]),
                    cancellation,
                )
                .await?;
            }
        }
        let staged = self
            .run(
                "GitVcsDriver.prepareCommitContext.stagedSummary",
                cwd,
                &strings(&["diff", "--cached", "--name-status"]),
                cancellation,
            )
            .await?;
        if staged.stdout.trim().is_empty() {
            return Ok(None);
        }
        self.run(
            "GitVcsDriver.commit",
            cwd,
            &["commit".into(), "-m".into(), message.into()],
            cancellation,
        )
        .await?;
        let sha = self
            .run(
                "GitVcsDriver.commit.sha",
                cwd,
                &strings(&["rev-parse", "HEAD"]),
                cancellation,
            )
            .await?;
        Ok(Some(sha.stdout.trim().to_owned()))
    }

    pub async fn commit_with_options(
        &self,
        cwd: &Path,
        args: &[String],
        message: &[u8],
        allow_empty: bool,
        cancellation: &CancellationToken,
    ) -> Result<GitManagerCommitOutcome, GitCommandError> {
        let status_args = strings(&[
            "-c",
            "core.quotePath=false",
            "status",
            "--porcelain=2",
            "-z",
            "--untracked-files=all",
        ]);
        let status = self
            .execute_read(
                "GitManager.commit.status",
                cwd,
                &status_args,
                false,
                cancellation,
            )
            .await?;
        let has_staged_changes = status
            .stdout
            .split('\0')
            .filter_map(parse_porcelain_v2_line)
            .any(|record| record.index_changed);
        let amending = args.iter().any(|argument| argument == "--amend");
        if !has_staged_changes && !allow_empty && !amending {
            return Ok(GitManagerCommitOutcome {
                sha: None,
                empty: true,
            });
        }

        self.execute_with_stdin(
            "GitManager.commit",
            cwd,
            args,
            message.to_vec(),
            false,
            cancellation,
        )
        .await?;
        let sha = self
            .run(
                "GitManager.commit.sha",
                cwd,
                &strings(&["rev-parse", "HEAD"]),
                cancellation,
            )
            .await?;
        Ok(GitManagerCommitOutcome {
            sha: Some(sha.stdout.trim().to_owned()),
            empty: false,
        })
    }

    pub async fn undo_head_commit(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<GitManagerHeadCommit, GitCommandError> {
        let head = self
            .run(
                "GitManager.undoCommit.head",
                cwd,
                &strings(&[
                    "show",
                    "-s",
                    "--no-show-signature",
                    "--format=%H%x00%P%x00%B",
                    "HEAD",
                ]),
                cancellation,
            )
            .await?;
        let head = parse_git_manager_head_commit(cwd, &head.stdout)?;
        if let Some(parent) = head.parents.first() {
            self.run(
                "GitManager.undoCommit.reset",
                cwd,
                &["reset".into(), "--mixed".into(), parent.clone()],
                cancellation,
            )
            .await?;
            return Ok(head);
        }

        let deleted = self
            .run(
                "GitManager.undoCommit.initial.deleted",
                cwd,
                &strings(&["diff", "--name-only", "--diff-filter=D", "-z", "HEAD", "--"]),
                cancellation,
            )
            .await?;
        let deleted_paths = deleted
            .stdout
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if !deleted_paths.is_empty() {
            let mut args = strings(&["checkout", "HEAD", "--"]);
            args.extend(deleted_paths);
            self.run(
                "GitManager.undoCommit.initial.restoreDeleted",
                cwd,
                &args,
                cancellation,
            )
            .await?;
        }
        self.run(
            "GitManager.undoCommit.initial.deleteHead",
            cwd,
            &strings(&["update-ref", "-d", "HEAD"]),
            cancellation,
        )
        .await?;
        self.run(
            "GitManager.undoCommit.initial.unstage",
            cwd,
            &strings(&["reset", "--", "."]),
            cancellation,
        )
        .await?;
        Ok(head)
    }

    pub async fn git_manager_head_tags(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<Vec<String>, GitCommandError> {
        let tags = self
            .run_read(
                "GitManager.undoCommit.tags",
                cwd,
                &strings(&["tag", "--points-at", "HEAD"]),
                cancellation,
            )
            .await?;
        Ok(tags
            .stdout
            .lines()
            .filter(|tag| !tag.is_empty())
            .map(str::to_owned)
            .collect())
    }

    pub async fn push_current_branch(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<String, GitCommandError> {
        let branch = self.current_ref(cwd, cancellation).await?.ok_or_else(|| {
            simple_error(
                "GitVcsDriver.pushCurrentBranch",
                cwd,
                "Cannot push from detached HEAD.",
            )
        })?;
        let upstream = self
            .execute(
                "GitVcsDriver.pushCurrentBranch.upstream",
                cwd,
                &strings(&["rev-parse", "--abbrev-ref", "@{upstream}"]),
                true,
                cancellation,
            )
            .await?;
        let args = if upstream.exit_code != 0 {
            vec![
                "push".into(),
                "--set-upstream".into(),
                "origin".into(),
                branch.clone(),
            ]
        } else if let Some(remote) = self
            .renamed_upstream_remote(cwd, &branch, cancellation)
            .await?
        {
            // Publish under the local name and track it, so status compares with the branch
            // that was pushed. A non-fast-forward is still refused: nothing forces.
            vec![
                "push".into(),
                "--set-upstream".into(),
                "--".into(),
                remote,
                format!("refs/heads/{branch}:refs/heads/{branch}"),
            ]
        } else {
            strings(&["push"])
        };
        self.for_bounded_transfer()
            .run("GitVcsDriver.pushCurrentBranch", cwd, &args, cancellation)
            .await?;
        Ok(branch)
    }

    /// The remote to publish `branch` to when its upstream has another name (#157), such as a
    /// branch made with `git checkout -b feature-x origin/develop`. Git's default
    /// `push.default=simple` refuses a bare `git push` there, whether it is unset or set to
    /// `simple`. `None` keeps the bare push: the upstream has the branch's own name, or the
    /// user configured another `push.default` or a push remote, which Git then honours. A local upstream (`.`) publishes to
    /// `origin`.
    async fn renamed_upstream_remote(
        &self,
        cwd: &Path,
        branch: &str,
        cancellation: &CancellationToken,
    ) -> Result<Option<String>, GitCommandError> {
        let config = |key: String| async move {
            self.execute_read(
                "GitVcsDriver.pushCurrentBranch.config",
                cwd,
                &["config".into(), "--get".into(), key],
                true,
                cancellation,
            )
            .await
            .map(|output| {
                (output.exit_code == 0)
                    .then(|| output.stdout.trim().to_owned())
                    .filter(|value| !value.is_empty())
            })
        };
        let merge = config(format!("branch.{branch}.merge")).await?;
        if merge.is_none_or(|merge| merge == format!("refs/heads/{branch}")) {
            return Ok(None);
        }
        // An explicit `simple` is Git's default spelled out (often left by old Git warnings):
        // it chooses no other target, so it refuses the same push.
        if config("push.default".to_owned())
            .await?
            .is_some_and(|mode| !mode.eq_ignore_ascii_case("simple"))
        {
            return Ok(None);
        }
        for key in [
            "remote.pushDefault".to_owned(),
            format!("branch.{branch}.pushRemote"),
        ] {
            if config(key).await?.is_some() {
                return Ok(None);
            }
        }
        let remote = config(format!("branch.{branch}.remote"))
            .await?
            .filter(|remote| remote != ".")
            .unwrap_or_else(|| "origin".to_owned());
        Ok(Some(remote))
    }

    pub async fn push_current_branch_to_remote(
        &self,
        cwd: &Path,
        remote_name: &str,
        cancellation: &CancellationToken,
    ) -> Result<String, GitCommandError> {
        let branch = self.current_ref(cwd, cancellation).await?.ok_or_else(|| {
            simple_error(
                "GitVcsDriver.pushCurrentBranchToRemote",
                cwd,
                "Cannot push from detached HEAD.",
            )
        })?;
        self.for_bounded_transfer()
            .run(
                "GitVcsDriver.pushCurrentBranchToRemote",
                cwd,
                &[
                    "push".into(),
                    "--set-upstream".into(),
                    "--".into(),
                    remote_name.into(),
                    branch.clone(),
                ],
                cancellation,
            )
            .await?;
        Ok(branch)
    }

    pub async fn commit_context(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<String, GitCommandError> {
        let mut staged_args = strings(&super::CANONICAL_DIFF_CONFIG);
        staged_args.extend(strings(&["diff", "--cached", "--patch", "--stat"]));
        let staged = self
            .run(
                "GitVcsDriver.commitContext.staged",
                cwd,
                &staged_args,
                cancellation,
            )
            .await?;
        if !staged.stdout.trim().is_empty() {
            return Ok(staged.stdout);
        }
        let mut working_args = strings(&super::CANONICAL_DIFF_CONFIG);
        working_args.extend(strings(&["diff", "--patch", "--stat"]));
        let working = self
            .run(
                "GitVcsDriver.commitContext.working",
                cwd,
                &working_args,
                cancellation,
            )
            .await?;
        Ok(working.stdout)
    }

    pub(super) async fn current_ref(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<Option<String>, GitCommandError> {
        let result = self
            .execute_read(
                "GitVcsDriver.currentRef",
                cwd,
                &strings(&["symbolic-ref", "--quiet", "--short", "HEAD"]),
                true,
                cancellation,
            )
            .await?;
        Ok((result.exit_code == 0)
            .then(|| result.stdout.trim().to_owned())
            .filter(|value| !value.is_empty()))
    }

    async fn default_ref(
        &self,
        cwd: &Path,
        current: Option<&str>,
        cancellation: &CancellationToken,
    ) -> Result<Option<String>, GitCommandError> {
        let origin_head = self
            .execute_read(
                "GitVcsDriver.defaultRef.originHead",
                cwd,
                &strings(&["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"]),
                true,
                cancellation,
            )
            .await?;
        if origin_head.exit_code == 0 {
            let value = origin_head
                .stdout
                .trim()
                .trim_start_matches("refs/remotes/origin/")
                .to_owned();
            if !value.is_empty() {
                return Ok(Some(value));
            }
        }
        for candidate in ["main", "master"] {
            let result = self
                .execute_read(
                    "GitVcsDriver.defaultRef.candidate",
                    cwd,
                    &[
                        "show-ref".into(),
                        "--verify".into(),
                        "--quiet".into(),
                        format!("refs/heads/{candidate}"),
                    ],
                    true,
                    cancellation,
                )
                .await?;
            if result.exit_code == 0 || current == Some(candidate) {
                return Ok(Some(candidate.to_owned()));
            }
        }
        Ok(current.map(str::to_owned))
    }

    async fn remote_provider(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<Option<SourceControlProviderInfo>, GitCommandError> {
        Ok(self
            .origin_url(cwd, cancellation)
            .await?
            .and_then(|url| provider_info(&self.provider_hosts, &url)))
    }

    /// The origin's host name, for messages that name the host.
    pub async fn origin_host(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<Option<String>, GitCommandError> {
        Ok(self
            .origin_url(cwd, cancellation)
            .await?
            .as_deref()
            .and_then(source_control::remote_host))
    }

    pub(crate) async fn origin_url(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<Option<String>, GitCommandError> {
        let remote = self
            .execute_read(
                "GitVcsDriver.remoteProvider",
                cwd,
                &strings(&["config", "--get", "remote.origin.url"]),
                true,
                cancellation,
            )
            .await?;
        if remote.stdout_truncated || remote.stderr_truncated {
            return Err(command_output_error(
                "GitVcsDriver.remoteProvider",
                cwd,
                3,
                &remote,
                "Git remote-provider output was truncated.",
            ));
        }
        if remote.exit_code == 0 {
            return Ok(Some(remote.stdout.trim().to_owned()));
        }
        if remote.exit_code == 1
            && remote.stdout.trim().is_empty()
            && remote.stderr.trim().is_empty()
        {
            return Ok(None);
        }
        Err(command_output_error(
            "GitVcsDriver.remoteProvider",
            cwd,
            3,
            &remote,
            "Git could not read the origin provider configuration.",
        ))
    }

    async fn worktree_map(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<HashMap<String, String>, GitCommandError> {
        let output = self
            .run(
                "GitVcsDriver.listRefs.worktreeList",
                cwd,
                &strings(&["worktree", "list", "--porcelain"]),
                cancellation,
            )
            .await?;
        let mut map = HashMap::new();
        let mut path: Option<String> = None;
        for line in output.stdout.lines() {
            if let Some(value) = line.strip_prefix("worktree ") {
                path = Some(value.to_owned());
            } else if let (Some(branch), Some(path)) =
                (line.strip_prefix("branch refs/heads/"), path.as_ref())
            {
                map.insert(branch.to_owned(), display_path(Path::new(path)));
            } else if line.is_empty() {
                path = None;
            }
        }
        Ok(map)
    }

    async fn worktree_paths(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<Vec<String>, GitCommandError> {
        let output = self
            .run(
                "GitVcsDriver.worktreePaths",
                cwd,
                &strings(&["worktree", "list", "--porcelain"]),
                cancellation,
            )
            .await?;
        Ok(output
            .stdout
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .map(|path| display_path(Path::new(path)))
            .collect())
    }
}

async fn read_optional_identity_marker(path: &Path) -> Result<Option<String>, io::Error> {
    match tokio::fs::read_to_string(path).await {
        Ok(value) if value.trim().is_empty() => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the identity marker is empty",
        )),
        Ok(value) => Ok(Some(value.trim().to_owned())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

async fn verify_identity_marker(
    cwd: &Path,
    worktree_path: &Path,
    marker_path: &Path,
    expected_nonce: &str,
) -> Result<(), GitCommandError> {
    let marker = read_optional_identity_marker(marker_path)
        .await
        .map_err(|error| worktree_identity_marker_error(cwd, worktree_path, &error.to_string()))?;
    if marker.as_deref() != Some(expected_nonce) {
        return Err(worktree_identity_marker_error(
            cwd,
            worktree_path,
            "the current directory has a different removal identity",
        ));
    }
    Ok(())
}

async fn write_durable_identity_marker(path: PathBuf, nonce: String) -> Result<(), io::Error> {
    tokio::task::spawn_blocking(move || {
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "identity marker has no parent")
        })?;
        let staged = parent.join(format!(
            ".{}.{}.tmp",
            path.file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("bibcode-removal-identity"),
            Uuid::new_v4()
        ));
        let result = (|| {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&staged)?;
            file.write_all(nonce.as_bytes())?;
            file.sync_all()?;
            drop(file);
            rename_path_blocking(&staged, &path)?;
            sync_directory_blocking(parent)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&staged);
        }
        result
    })
    .await
    .map_err(|error| io::Error::other(format!("identity marker task failed: {error}")))?
}

fn sync_directory_blocking(path: &Path) -> Result<(), io::Error> {
    #[cfg(windows)]
    {
        // Windows directory handles do not reliably support FlushFileBuffers. File contents are
        // synced before publication and every safety-critical rename uses MOVEFILE_WRITE_THROUGH.
        let _ = path;
        Ok(())
    }
    #[cfg(not(windows))]
    {
        fs::File::open(path)?.sync_all()
    }
}

#[cfg(windows)]
fn rename_path_blocking(source: &Path, destination: &Path) -> Result<(), io::Error> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: Both paths are valid NUL-terminated UTF-16 strings and remain alive for the call.
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    } != 0
    {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(windows))]
fn rename_path_blocking(source: &Path, destination: &Path) -> Result<(), io::Error> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn rename_bound_directory_blocking(
    lease: &RemovalDirectoryLease,
    source: &Path,
    destination: &Path,
) -> Result<(), io::Error> {
    use std::{
        mem::{offset_of, size_of},
        os::windows::{ffi::OsStrExt, io::AsRawHandle},
    };

    use windows_sys::Win32::Storage::FileSystem::{
        FILE_RENAME_INFO, FileRenameInfo, SetFileInformationByHandle,
    };

    verify_removal_directory_lease(lease, source)?;
    match fs::symlink_metadata(destination) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "the bound rename destination already exists",
            ));
        }
    }
    let destination = if destination.is_absolute() {
        destination.to_path_buf()
    } else {
        std::env::current_dir()?.join(destination)
    };
    let destination_name = destination.as_os_str().encode_wide().collect::<Vec<_>>();
    if destination_name.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "rename destination contains an interior NUL",
        ));
    }
    let header_size = offset_of!(FILE_RENAME_INFO, FileName);
    let information_size =
        size_of::<FILE_RENAME_INFO>() + destination_name.len() * size_of::<u16>();
    let mut information = vec![0usize; information_size.div_ceil(size_of::<usize>())];
    let information_ptr = information.as_mut_ptr().cast::<u8>();
    // SAFETY: information is pointer-aligned, sized for the FILE_RENAME_INFO header plus the
    // UTF-16 name, and all referenced handles and buffers remain alive through the system call.
    unsafe {
        let rename = information_ptr.cast::<FILE_RENAME_INFO>();
        (*rename).Anonymous.ReplaceIfExists = false;
        (*rename).RootDirectory = std::ptr::null_mut();
        (*rename).FileNameLength = (destination_name.len() * size_of::<u16>()) as u32;
        std::ptr::copy_nonoverlapping(
            destination_name.as_ptr(),
            information_ptr.add(header_size).cast::<u16>(),
            destination_name.len(),
        );
        if SetFileInformationByHandle(
            lease.handle.as_raw_handle(),
            FileRenameInfo,
            information_ptr.cast(),
            information_size as u32,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    verify_removal_directory_lease(lease, &destination)
}

#[cfg(not(windows))]
fn rename_bound_directory_blocking(
    lease: &RemovalDirectoryLease,
    source: &Path,
    destination: &Path,
) -> Result<(), io::Error> {
    verify_removal_directory_lease(lease, source)?;
    fs::rename(source, destination)?;
    verify_removal_directory_lease(lease, destination)
}

fn worktree_removal_quarantine_path(path: &Path, nonce: &str) -> Result<PathBuf, io::Error> {
    Uuid::parse_str(nonce).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "the removal identity is not a valid UUID",
        )
    })?;
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "worktree path has no parent directory",
        )
    })?;
    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "worktree path has no file name",
        )
    })?;
    Ok(parent.join(format!(
        ".{}.bibcode-removal-{nonce}",
        file_name.to_string_lossy()
    )))
}

async fn quarantine_worktree_with_tombstone(
    source: &Path,
    quarantine: &Path,
    nonce: &str,
    cancellation: &CancellationToken,
) -> Result<(RemovalDirectoryLease, RemovalDirectoryLease), io::Error> {
    if cancellation.is_cancelled() {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "worktree removal was cancelled",
        ));
    }
    let source = source.to_path_buf();
    let quarantine = quarantine.to_path_buf();
    let nonce = nonce.to_owned();
    let cancellation = cancellation.clone();
    tokio::task::spawn_blocking(move || {
        let lease = open_removal_directory_lease(&source)?;
        verify_identity_marker_blocking(
            &source,
            WORKTREE_REMOVAL_ROOT_IDENTITY_FILE,
            &nonce,
        )?;
        verify_removal_directory_lease(&lease, &source)?;
        if cancellation.is_cancelled() {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "worktree removal was cancelled",
            ));
        }
        rename_bound_directory_blocking(&lease, &source, &quarantine)?;
        let parent = source.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "worktree path has no parent")
        })?;
        if let Err(error) = verify_identity_marker_blocking(
            &quarantine,
            WORKTREE_REMOVAL_ROOT_IDENTITY_FILE,
            &nonce,
        ) {
            let rollback = rename_bound_directory_blocking(&lease, &quarantine, &source);
            let _ = sync_directory_blocking(parent);
            return match rollback {
                Ok(()) => Err(io::Error::new(
                    error.kind(),
                    format!(
                        "the directory moved into worktree quarantine did not have the admitted identity: {error}"
                    ),
                )),
                Err(rollback_error) => Err(io::Error::new(
                    error.kind(),
                    format!(
                        "the directory moved into worktree quarantine did not have the admitted identity: {error}; rollback retained it at '{}': {rollback_error}",
                        display_path(&quarantine)
                    ),
                )),
            };
        }
        let tombstone_lease = match sync_directory_blocking(parent).and_then(|()| {
            create_worktree_removal_tombstone_blocking(&source, &quarantine, &nonce)
        }) {
            Ok(lease) => lease,
            Err(error) => {
            let rollback = rename_bound_directory_blocking(&lease, &quarantine, &source);
            return match rollback {
                Ok(()) => Err(error),
                Err(rollback_error) => Err(io::Error::new(
                    error.kind(),
                    format!(
                        "worktree quarantine admission failed: {error}; rollback retained the worktree at '{}': {rollback_error}",
                        display_path(&quarantine)
                    ),
                )),
            };
            }
        };
        sync_directory_blocking(parent)?;
        Ok((lease, tombstone_lease))
    })
    .await
    .map_err(|error| io::Error::other(format!("worktree quarantine task failed: {error}")))?
}

async fn ensure_worktree_removal_tombstone(
    path: &Path,
    quarantine: &Path,
    nonce: &str,
) -> Result<RemovalDirectoryLease, io::Error> {
    let path = path.to_path_buf();
    let quarantine = quarantine.to_path_buf();
    let nonce = nonce.to_owned();
    tokio::task::spawn_blocking(move || {
        match open_removal_directory_lease(&path) {
            Ok(lease) => {
                verify_or_repair_worktree_removal_tombstone_blocking(&path, &quarantine, &nonce)?;
                verify_removal_directory_lease(&lease, &path)?;
                return Ok(lease);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let lease = create_worktree_removal_tombstone_blocking(&path, &quarantine, &nonce)?;
        if let Some(parent) = path.parent() {
            sync_directory_blocking(parent)?;
        }
        Ok(lease)
    })
    .await
    .map_err(|error| io::Error::other(format!("worktree tombstone task failed: {error}")))?
}

fn create_worktree_removal_tombstone_blocking(
    path: &Path,
    quarantine: &Path,
    nonce: &str,
) -> Result<RemovalDirectoryLease, io::Error> {
    let lease = create_removal_directory_lease(path)?;
    let mut marker_created = false;
    let mut git_file_created = false;
    let result = (|| {
        let mut marker = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path.join(WORKTREE_REMOVAL_TOMBSTONE_FILE))?;
        marker_created = true;
        marker.write_all(nonce.as_bytes())?;
        marker.sync_all()?;
        drop(marker);
        let quarantined_git_file = quarantine.join(".git");
        if quarantined_git_file.is_file() {
            let contents = fs::read(&quarantined_git_file)?;
            let mut git_file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(path.join(".git"))?;
            git_file_created = true;
            git_file.write_all(&contents)?;
            git_file.sync_all()?;
        }
        sync_directory_blocking(path)
    })();
    if let Err(error) = result {
        if git_file_created {
            let _ = remove_bound_regular_file(&lease, path, ".git", None);
        }
        if marker_created {
            let _ = remove_bound_regular_file(&lease, path, WORKTREE_REMOVAL_TOMBSTONE_FILE, None);
        }
        if bound_directory_is_empty(&lease, path).unwrap_or(false) {
            let _ = delete_bound_directory_root_blocking(lease, path);
        }
        return Err(error);
    }
    Ok(lease)
}

fn verify_or_repair_worktree_removal_tombstone_blocking(
    path: &Path,
    quarantine: &Path,
    expected_nonce: &str,
) -> Result<(), io::Error> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::other(
            "the removal tombstone is not a real directory",
        ));
    }
    let marker = fs::read_to_string(path.join(WORKTREE_REMOVAL_TOMBSTONE_FILE))?;
    if marker.trim() != expected_nonce {
        return Err(io::Error::other(
            "the removal tombstone identity does not match",
        ));
    }
    let mut has_git_file = false;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name();
        if name != WORKTREE_REMOVAL_TOMBSTONE_FILE && name != ".git" {
            return Err(io::Error::other(
                "the removal tombstone contains unexpected files",
            ));
        }
        let file_type = entry.file_type()?;
        if !file_type.is_file() || file_type.is_symlink() {
            return Err(io::Error::other(
                "the removal tombstone contains an unsafe entry",
            ));
        }
        has_git_file |= name == ".git";
    }
    let quarantined_git_file = quarantine.join(".git");
    let quarantine_has_git_file = quarantined_git_file.is_file();
    if has_git_file && !quarantine_has_git_file {
        return Err(io::Error::other(
            "an orphan-removal tombstone unexpectedly contains a Git link",
        ));
    }
    if quarantine_has_git_file && !has_git_file {
        let contents = fs::read(quarantined_git_file)?;
        let mut git_file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path.join(".git"))?;
        git_file.write_all(&contents)?;
        git_file.sync_all()?;
        sync_directory_blocking(path)?;
    }
    Ok(())
}

struct RemovalDirectoryLease {
    handle: fs::File,
    #[cfg(windows)]
    _parent_handle: Option<fs::File>,
}

struct WorktreeDeregistrationLeases {
    quarantine: RemovalDirectoryLease,
    tombstone: RemovalDirectoryLease,
    admin_quarantine: Option<RemovalDirectoryLease>,
    admin_source: Option<RemovalDirectoryLease>,
}

#[cfg(windows)]
fn create_removal_directory_lease(path: &Path) -> Result<RemovalDirectoryLease, io::Error> {
    use std::{
        mem::size_of,
        os::windows::{
            ffi::OsStrExt,
            fs::OpenOptionsExt,
            io::{AsRawHandle, FromRawHandle},
        },
    };

    use windows_sys::Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            FILE_CREATE, FILE_DIRECTORY_FILE, FILE_OPEN_REPARSE_POINT,
            FILE_SYNCHRONOUS_IO_NONALERT, NtCreateFile,
        },
    };
    use windows_sys::Win32::{
        Foundation::{HANDLE, OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError, UNICODE_STRING},
        Storage::FileSystem::{
            DELETE, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ, FILE_SHARE_WRITE, SYNCHRONIZE,
        },
        System::IO::IO_STATUS_BLOCK,
    };

    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "removal directory has no parent",
        )
    })?;
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "removal directory has no file name",
        )
    })?;
    let parent = fs::OpenOptions::new()
        .access_mode(FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(parent)?;
    let mut name = name.encode_wide().collect::<Vec<_>>();
    let name_bytes = name
        .len()
        .checked_mul(size_of::<u16>())
        .and_then(|length| u16::try_from(length).ok())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "directory name is too long"))?;
    if name.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "removal directory name contains an interior NUL",
        ));
    }
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
    // SAFETY: every pointer references initialized storage that remains alive for the call;
    // NtCreateFile initializes handle on success and the returned handle is adopted exactly once.
    let status = unsafe {
        NtCreateFile(
            std::ptr::from_mut(&mut handle),
            DELETE | FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            std::ptr::from_ref(&attributes),
            std::ptr::from_mut(&mut status_block),
            std::ptr::null(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            FILE_CREATE,
            FILE_DIRECTORY_FILE | FILE_SYNCHRONOUS_IO_NONALERT | FILE_OPEN_REPARSE_POINT,
            std::ptr::null(),
            0,
        )
    };
    if status < 0 {
        // SAFETY: status is the NTSTATUS returned directly by NtCreateFile.
        let windows_error = unsafe { RtlNtStatusToDosError(status) };
        return Err(io::Error::from_raw_os_error(windows_error as i32));
    }
    // SAFETY: NtCreateFile succeeded and transferred one owned Windows handle.
    let handle = unsafe { fs::File::from_raw_handle(handle) };
    let lease = RemovalDirectoryLease {
        handle,
        _parent_handle: Some(parent),
    };
    verify_removal_directory_lease(&lease, path)?;
    Ok(lease)
}

#[cfg(not(windows))]
fn create_removal_directory_lease(path: &Path) -> Result<RemovalDirectoryLease, io::Error> {
    fs::create_dir(path)?;
    open_removal_directory_lease(path)
}

fn open_removal_directory_lease(path: &Path) -> Result<RemovalDirectoryLease, io::Error> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::other(
            "the removal target is not a real directory",
        ));
    }
    #[cfg(windows)]
    let handle = {
        use std::os::windows::fs::OpenOptionsExt;

        use windows_sys::Win32::Storage::FileSystem::{
            DELETE, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ, FILE_SHARE_WRITE,
        };

        fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES | DELETE)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?
    };
    #[cfg(not(windows))]
    let handle = fs::File::open(path)?;
    let lease = RemovalDirectoryLease {
        handle,
        #[cfg(windows)]
        _parent_handle: None,
    };
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        };

        let identity = windows_file_identity(&lease.handle)?;
        if identity.attributes & FILE_ATTRIBUTE_DIRECTORY == 0
            || identity.attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        {
            return Err(io::Error::other(
                "the removal target is a reparse point or not a directory",
            ));
        }
    }
    verify_removal_directory_lease(&lease, path)?;
    Ok(lease)
}

fn verify_removal_directory_lease(
    lease: &RemovalDirectoryLease,
    path: &Path,
) -> Result<(), io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let leased = lease.handle.metadata()?;
        let current = fs::symlink_metadata(path)?;
        if leased.dev() != current.dev() || leased.ino() != current.ino() {
            return Err(io::Error::other(
                "the removal directory was rebound during cleanup",
            ));
        }
    }
    #[cfg(windows)]
    {
        // The directory handle omits FILE_SHARE_DELETE, so Windows cannot rename, replace, or
        // delete this filesystem object until the lease is dropped.
        let _ = (&lease.handle, path);
    }
    Ok(())
}

#[cfg(unix)]
fn owned_worktree_path_identity(
    lease: &RemovalDirectoryLease,
) -> Result<OwnedWorktreePathIdentity, io::Error> {
    use std::os::unix::fs::MetadataExt;

    let metadata = lease.handle.metadata()?;
    Ok(OwnedWorktreePathIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(windows)]
fn owned_worktree_path_identity(
    lease: &RemovalDirectoryLease,
) -> Result<OwnedWorktreePathIdentity, io::Error> {
    windows_file_identity(&lease.handle)
}

fn verify_owned_worktree_path_identity(
    lease: &RemovalDirectoryLease,
    expected: OwnedWorktreePathIdentity,
) -> Result<(), io::Error> {
    if owned_worktree_path_identity(lease)? != expected {
        return Err(io::Error::other(
            "the reserved worktree directory identity changed",
        ));
    }
    Ok(())
}

fn verify_identity_marker_blocking(
    path: &Path,
    marker_name: &str,
    expected_nonce: &str,
) -> Result<(), io::Error> {
    let marker_path = path.join(marker_name);
    let metadata = fs::symlink_metadata(&marker_path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(io::Error::other(
            "the bound removal directory identity marker is not a real file",
        ));
    }
    let marker = fs::read_to_string(marker_path)?;
    if marker.trim() != expected_nonce {
        return Err(io::Error::other(
            "the bound removal directory identity does not match",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn unix_directory_entry_names(
    directory_fd: std::os::fd::RawFd,
) -> Result<Vec<OsString>, io::Error> {
    use std::{ffi::CStr, os::unix::ffi::OsStringExt};

    // SAFETY: dup returns a new owned descriptor or -1. fdopendir takes ownership only on success.
    let duplicated = unsafe { libc::dup(directory_fd) };
    if duplicated < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: duplicated is a valid directory descriptor owned by this function.
    let directory = unsafe { libc::fdopendir(duplicated) };
    if directory.is_null() {
        // SAFETY: fdopendir failed and did not take ownership of duplicated.
        unsafe { libc::close(duplicated) };
        return Err(io::Error::last_os_error());
    }
    let mut names = Vec::new();
    loop {
        // SAFETY: directory remains valid until closed below.
        let entry = unsafe { libc::readdir(directory) };
        if entry.is_null() {
            break;
        }
        // SAFETY: d_name is NUL-terminated for the returned dirent.
        let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if bytes != b"." && bytes != b".." {
            names.push(OsString::from_vec(bytes.to_vec()));
        }
    }
    // SAFETY: directory was returned by fdopendir and is closed exactly once.
    if unsafe { libc::closedir(directory) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(names)
}

#[cfg(unix)]
fn unix_name(name: &std::ffi::OsStr) -> Result<std::ffi::CString, io::Error> {
    use std::os::unix::ffi::OsStrExt;

    std::ffi::CString::new(name.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "filesystem entry contains an interior NUL",
        )
    })
}

#[cfg(unix)]
fn clear_unix_directory_fd(
    directory_fd: std::os::fd::RawFd,
    preserved_name: Option<&std::ffi::OsStr>,
) -> Result<(), io::Error> {
    for name in unix_directory_entry_names(directory_fd)? {
        if preserved_name.is_some_and(|preserved| preserved == name) {
            continue;
        }
        let c_name = unix_name(&name)?;
        let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: directory_fd is open, c_name is NUL-terminated, and metadata is writable.
        if unsafe {
            libc::fstatat(
                directory_fd,
                c_name.as_ptr(),
                metadata.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fstatat succeeded and initialized metadata.
        let metadata = unsafe { metadata.assume_init() };
        if metadata.st_mode & libc::S_IFMT == libc::S_IFDIR {
            // SAFETY: arguments are valid; O_NOFOLLOW prevents traversing a substituted symlink.
            let child_fd = unsafe {
                libc::openat(
                    directory_fd,
                    c_name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if child_fd < 0 {
                return Err(io::Error::last_os_error());
            }
            let child_result = clear_unix_directory_fd(child_fd, None);
            // SAFETY: child_fd is owned by this scope and closed exactly once.
            let close_result = unsafe { libc::close(child_fd) };
            child_result?;
            if close_result != 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: directory_fd and c_name remain valid for unlinkat.
            if unsafe { libc::unlinkat(directory_fd, c_name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
                return Err(io::Error::last_os_error());
            }
        } else {
            // SAFETY: directory_fd and c_name remain valid for unlinkat.
            if unsafe { libc::unlinkat(directory_fd, c_name.as_ptr(), 0) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WindowsFileIdentity {
    volume: u64,
    file: u64,
    attributes: u32,
}

#[cfg(windows)]
fn windows_file_identity(file: &fs::File) -> Result<WindowsFileIdentity, io::Error> {
    use std::{mem::MaybeUninit, os::windows::io::AsRawHandle};

    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    // SAFETY: information points to writable storage and the file handle remains valid.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: GetFileInformationByHandle succeeded and initialized information.
    let information = unsafe { information.assume_init() };
    Ok(WindowsFileIdentity {
        volume: u64::from(information.dwVolumeSerialNumber),
        file: (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
        attributes: information.dwFileAttributes,
    })
}

#[cfg(windows)]
fn open_windows_removal_entry(path: &Path) -> Result<fs::File, io::Error> {
    use std::os::windows::fs::OpenOptionsExt;

    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    };

    fs::OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES | DELETE)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

#[cfg(windows)]
fn delete_windows_entry_by_handle(file: &fs::File) -> Result<(), io::Error> {
    use std::{mem::size_of, os::windows::io::AsRawHandle};

    use windows_sys::Win32::Storage::FileSystem::{
        FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO,
        FILE_DISPOSITION_INFO_EX, FileDispositionInfo, FileDispositionInfoEx,
        SetFileInformationByHandle,
    };

    let disposition = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    };
    // SAFETY: file is valid and disposition points to the declared structure for the call.
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfoEx,
            std::ptr::from_ref(&disposition).cast(),
            size_of::<FILE_DISPOSITION_INFO_EX>() as u32,
        )
    } != 0
    {
        return Ok(());
    }
    let fallback = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: file is valid and fallback points to the declared structure for the call.
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfo,
            std::ptr::from_ref(&fallback).cast(),
            size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    } != 0
    {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
struct WindowsRemovalEntry {
    path: PathBuf,
    identity: WindowsFileIdentity,
}

#[cfg(windows)]
fn collect_windows_removal_entries(
    path: &Path,
    preserved_name: Option<&str>,
    entries: &mut Vec<WindowsRemovalEntry>,
) -> Result<(), io::Error> {
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    };

    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if preserved_name.is_some_and(|preserved| entry.file_name() == preserved) {
            continue;
        }
        let entry_path = entry.path();
        let handle = open_windows_removal_entry(&entry_path)?;
        let identity = windows_file_identity(&handle)?;
        let directory = identity.attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
        let traversable_directory =
            directory && identity.attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0;
        if traversable_directory {
            collect_windows_removal_entries(&entry_path, None, entries)?;
        }
        entries.push(WindowsRemovalEntry {
            path: entry_path,
            identity,
        });
    }
    Ok(())
}

#[cfg(windows)]
fn clear_windows_directory_bound(
    path: &Path,
    preserved_name: Option<&str>,
) -> Result<(), io::Error> {
    clear_windows_directory_bound_after_snapshot(path, preserved_name, || {})
}

#[cfg(windows)]
fn clear_windows_directory_bound_after_snapshot(
    path: &Path,
    preserved_name: Option<&str>,
    after_snapshot: impl FnOnce(),
) -> Result<(), io::Error> {
    let mut entries = Vec::new();
    collect_windows_removal_entries(path, preserved_name, &mut entries)?;
    after_snapshot();
    for entry in entries {
        let handle = open_windows_removal_entry(&entry.path)?;
        if windows_file_identity(&handle)? != entry.identity {
            return Err(io::Error::other(
                "a removal descendant was rebound before deletion",
            ));
        }
        delete_windows_entry_by_handle(&handle)?;
        drop(handle);
    }
    Ok(())
}

fn clear_bound_directory_contents(
    lease: &RemovalDirectoryLease,
    path: &Path,
    preserved_name: Option<&str>,
) -> Result<(), io::Error> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;

        let _ = path;
        clear_unix_directory_fd(
            lease.handle.as_raw_fd(),
            preserved_name.map(std::ffi::OsStr::new),
        )
    }
    #[cfg(windows)]
    {
        verify_removal_directory_lease(lease, path)?;
        clear_windows_directory_bound(path, preserved_name)
    }
}

fn remove_bound_regular_file(
    lease: &RemovalDirectoryLease,
    path: &Path,
    name: &str,
    expected_contents: Option<&str>,
) -> Result<(), io::Error> {
    #[cfg(unix)]
    {
        use std::os::fd::{AsRawFd, FromRawFd};

        let _ = path;
        let name = unix_name(std::ffi::OsStr::new(name))?;
        // SAFETY: the leased directory fd is open and name is NUL-terminated. O_NOFOLLOW
        // prevents opening a substituted symlink.
        let file_fd = unsafe {
            libc::openat(
                lease.handle.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if file_fd < 0 {
            let error = io::Error::last_os_error();
            return if error.kind() == io::ErrorKind::NotFound {
                Ok(())
            } else {
                Err(error)
            };
        }
        // SAFETY: file_fd is owned by this scope and transferred exactly once.
        let mut file = unsafe { fs::File::from_raw_fd(file_fd) };
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(io::Error::other(
                "a protected removal file became an unsafe entry",
            ));
        }
        if let Some(expected) = expected_contents {
            use io::Read;

            let mut actual = String::new();
            file.read_to_string(&mut actual)?;
            if actual.trim() != expected {
                return Err(io::Error::other(
                    "a protected removal file identity does not match",
                ));
            }
        }
        // SAFETY: the leased directory fd and NUL-terminated name remain valid.
        if unsafe { libc::unlinkat(lease.handle.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        drop(file);
        Ok(())
    }
    #[cfg(windows)]
    {
        use std::{io::Read, os::windows::fs::OpenOptionsExt};

        use windows_sys::Win32::Storage::FileSystem::{
            DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_SHARE_READ,
            FILE_SHARE_WRITE,
        };

        verify_removal_directory_lease(lease, path)?;
        let entry_path = path.join(name);
        let mut file = match fs::OpenOptions::new()
            .access_mode(FILE_READ_DATA | FILE_READ_ATTRIBUTES | DELETE)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(entry_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        let identity = windows_file_identity(&file)?;
        if identity.attributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
            return Err(io::Error::other(
                "a protected removal file became an unsafe entry",
            ));
        }
        if let Some(expected) = expected_contents {
            let mut actual = String::new();
            file.read_to_string(&mut actual)?;
            if actual.trim() != expected {
                return Err(io::Error::other(
                    "a protected removal file identity does not match",
                ));
            }
        }
        delete_windows_entry_by_handle(&file)?;
        drop(file);
        Ok(())
    }
}

fn bound_directory_is_empty(lease: &RemovalDirectoryLease, path: &Path) -> Result<bool, io::Error> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;

        let _ = path;
        Ok(unix_directory_entry_names(lease.handle.as_raw_fd())?.is_empty())
    }
    #[cfg(windows)]
    {
        let _ = lease;
        Ok(fs::read_dir(path)?.next().transpose()?.is_none())
    }
}

fn delete_bound_directory_root_blocking(
    lease: RemovalDirectoryLease,
    path: &Path,
) -> Result<(), io::Error> {
    if !bound_directory_is_empty(&lease, path)? {
        return Err(io::Error::other(
            "a bound removal directory gained unexpected data before root deletion",
        ));
    }
    verify_removal_directory_lease(&lease, path)?;
    #[cfg(windows)]
    {
        delete_windows_entry_by_handle(&lease.handle)?;
        drop(lease);
        Ok(())
    }
    #[cfg(not(windows))]
    {
        fs::remove_dir(path)?;
        drop(lease);
        Ok(())
    }
}

async fn deregister_worktree_admin_and_remove_tombstone(
    path: &Path,
    quarantine: &Path,
    admin_source: Option<&Path>,
    admin_quarantine: &Path,
    nonce: &str,
    leases: WorktreeDeregistrationLeases,
) -> Result<(RemovalDirectoryLease, Option<RemovalDirectoryLease>), io::Error> {
    let path = path.to_path_buf();
    let quarantine = quarantine.to_path_buf();
    let admin_source = admin_source.map(Path::to_path_buf);
    let admin_quarantine = admin_quarantine.to_path_buf();
    let nonce = nonce.to_owned();
    tokio::task::spawn_blocking(move || {
        let WorktreeDeregistrationLeases {
            quarantine: quarantine_lease,
            tombstone: tombstone_lease,
            admin_quarantine: admin_quarantine_lease,
            admin_source: admin_source_lease,
        } = leases;
        verify_removal_directory_lease(&quarantine_lease, &quarantine)?;
        verify_or_repair_worktree_removal_tombstone_blocking(&path, &quarantine, &nonce)?;
        verify_removal_directory_lease(&tombstone_lease, &path)?;

        let mut admin_quarantine_lease = admin_quarantine_lease;
        if let Some(admin_lease) = admin_quarantine_lease.as_ref() {
            verify_removal_directory_lease(admin_lease, &admin_quarantine)?;
            verify_identity_marker_blocking(
                &admin_quarantine,
                WORKTREE_REMOVAL_IDENTITY_FILE,
                &nonce,
            )?;
        } else if let Some(admin_lease) = admin_source_lease {
            let admin_source = admin_source.as_deref().ok_or_else(|| {
                io::Error::other("the registered worktree admin path disappeared")
            })?;
            verify_identity_marker_blocking(admin_source, WORKTREE_REMOVAL_IDENTITY_FILE, &nonce)?;
            verify_removal_directory_lease(&admin_lease, admin_source)?;
            let admin_parent = admin_quarantine.parent().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "admin quarantine has no parent",
                )
            })?;
            fs::create_dir_all(admin_parent)?;
            sync_directory_blocking(admin_parent)?;
            rename_bound_directory_blocking(&admin_lease, admin_source, &admin_quarantine)?;
            if let Err(error) = verify_identity_marker_blocking(
                &admin_quarantine,
                WORKTREE_REMOVAL_IDENTITY_FILE,
                &nonce,
            ) {
                let _ =
                    rename_bound_directory_blocking(&admin_lease, &admin_quarantine, admin_source);
                return Err(error);
            }
            if let Some(source_parent) = admin_source.parent() {
                sync_directory_blocking(source_parent)?;
            }
            sync_directory_blocking(admin_parent)?;
            admin_quarantine_lease = Some(admin_lease);
        }

        verify_removal_directory_lease(&quarantine_lease, &quarantine)?;
        verify_or_repair_worktree_removal_tombstone_blocking(&path, &quarantine, &nonce)?;
        verify_removal_directory_lease(&tombstone_lease, &path)?;
        remove_bound_regular_file(&tombstone_lease, &path, ".git", None)?;
        remove_bound_regular_file(
            &tombstone_lease,
            &path,
            WORKTREE_REMOVAL_TOMBSTONE_FILE,
            Some(&nonce),
        )?;
        if !bound_directory_is_empty(&tombstone_lease, &path)? {
            return Err(io::Error::other(
                "the removal tombstone gained unexpected contents",
            ));
        }
        delete_bound_directory_root_blocking(tombstone_lease, &path)?;
        if let Some(parent) = path.parent() {
            sync_directory_blocking(parent)?;
        }
        Ok((quarantine_lease, admin_quarantine_lease))
    })
    .await
    .map_err(|error| io::Error::other(format!("worktree deregistration task failed: {error}")))?
}

async fn open_removal_directory_lease_async(
    path: &Path,
) -> Result<RemovalDirectoryLease, io::Error> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || open_removal_directory_lease(&path))
        .await
        .map_err(|error| io::Error::other(format!("directory lease task failed: {error}")))?
}

async fn remove_bound_identity_directory_with_lease(
    path: &Path,
    marker_name: &str,
    expected_nonce: &str,
    cancellation: &CancellationToken,
    lease: RemovalDirectoryLease,
) -> Result<(), io::Error> {
    let path = path.to_path_buf();
    let marker_name = marker_name.to_owned();
    let expected_nonce = expected_nonce.to_owned();
    let cancellation = cancellation.clone();
    tokio::task::spawn_blocking(move || {
        let mut last_error = None;
        for attempt in 0..WORKTREE_REMOVE_RETRY_ATTEMPTS {
            if cancellation.is_cancelled() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "worktree removal was cancelled",
                ));
            }
            match clear_bound_identity_directory_once(&lease, &path, &marker_name, &expected_nonce)
            {
                Ok(()) => return delete_bound_directory_root_blocking(lease, &path),
                Err(error) => last_error = Some(error),
            }
            if attempt + 1 < WORKTREE_REMOVE_RETRY_ATTEMPTS {
                std::thread::sleep(WORKTREE_REMOVE_RETRY_DELAY);
            }
        }
        Err(last_error.unwrap_or_else(|| io::Error::other("bound directory cleanup failed")))
    })
    .await
    .map_err(|error| io::Error::other(format!("bound directory cleanup task failed: {error}")))?
}

async fn remove_bound_owned_directory_with_lease(
    path: &Path,
    lease: RemovalDirectoryLease,
) -> Result<(), io::Error> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut last_error = None;
        for attempt in 0..WORKTREE_REMOVE_RETRY_ATTEMPTS {
            match clear_bound_directory_contents(&lease, &path, None) {
                Ok(()) => return delete_bound_directory_root_blocking(lease, &path),
                Err(error) => last_error = Some(error),
            }
            if attempt + 1 < WORKTREE_REMOVE_RETRY_ATTEMPTS {
                std::thread::sleep(WORKTREE_REMOVE_RETRY_DELAY);
            }
        }
        Err(last_error.unwrap_or_else(|| io::Error::other("owned directory cleanup failed")))
    })
    .await
    .map_err(|error| io::Error::other(format!("owned directory cleanup task failed: {error}")))?
}

/// The destination's own name and the folder that holds it, as a user would look for them.
fn destination_name_and_location(destination: &Path) -> (String, String) {
    let name = destination.file_name().map_or_else(
        || display_path(destination),
        |name| name.to_string_lossy().into_owned(),
    );
    let location = destination
        .parent()
        .map_or_else(|| display_path(destination), display_path);
    (name, location)
}

/// The folder a clone creates under its parent: exactly one normal path component. Only
/// [`clone_destination_leaf`] makes one, so no path, `.`, `..`, absolute, or prefixed name reaches
/// a clone's destination key, the disk check, a cancel, or the reservation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CloneLeaf(String);

impl CloneLeaf {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// Why a clone has no valid folder name. The text never names the URL, which may embed
/// credentials.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum CloneLeafError {
    /// The caller's `directoryName` is not a single folder name.
    InvalidName(String),
    /// The URL's last segment gives no usable folder name.
    Underivable,
}

impl fmt::Display for CloneLeafError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName(name) => write!(
                formatter,
                "The folder name \"{name}\" must be a single name, without slashes or \"..\"."
            ),
            Self::Underivable => formatter.write_str(
                "Could not work out a folder name from the repository URL. Check that the URL ends with the repository's name.",
            ),
        }
    }
}

/// The folder a clone creates under its parent: `directory_name` when given, otherwise the
/// URL's last path segment without a trailing `.git`. Either must be exactly one normal path
/// component (no separator, `.`, `..`, root, or prefix), or the destination key, the kernel's
/// path resolution, and the folder the cleanup removes could disagree.
pub(crate) fn clone_destination_leaf(
    url: &str,
    directory_name: Option<&str>,
) -> Result<CloneLeaf, CloneLeafError> {
    let single_folder_name = |leaf: &str| {
        let mut components = Path::new(leaf).components();
        matches!(
            (components.next(), components.next()),
            (Some(Component::Normal(name)), None) if name == std::ffi::OsStr::new(leaf)
        )
    };
    match directory_name {
        Some(name) if single_folder_name(name) => Ok(CloneLeaf(name.to_owned())),
        Some(name) => Err(CloneLeafError::InvalidName(name.to_owned())),
        None => {
            let derived = url
                .trim_end_matches(['/', '\\'])
                .rsplit(['/', '\\', ':'])
                .next()
                .unwrap_or("repository")
                .trim_end_matches(".git");
            if single_folder_name(derived) {
                Ok(CloneLeaf(derived.to_owned()))
            } else {
                Err(CloneLeafError::Underivable)
            }
        }
    }
}

/// What reserving a clone destination found.
pub(crate) enum CloneReservation {
    /// The destination did not exist. This clone created it and owns its cleanup.
    Reserved(ReservedClone),
    /// A finished clone of the same URL is already there.
    Existing(PathBuf),
}

/// A destination one clone created. The transfer and the cleanup are separate steps, so an
/// owner that runs the transfer in another task keeps the cleanup even if that task panics.
pub(crate) struct ReservedClone {
    repository: GitRepository,
    url: String,
    parent_dir: PathBuf,
    leaf: CloneLeaf,
    destination: OwnedWorktreePath,
}

impl ReservedClone {
    /// Runs `git clone` into the reserved folder and nothing else. When it fails, the runner
    /// has already stopped and reaped Git's process group; removing the folder is the caller's.
    pub(crate) async fn transfer(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<PathBuf, GitCommandError> {
        self.repository
            .run(
                CLONE_OPERATION,
                &self.parent_dir,
                &[
                    "clone".into(),
                    "--".into(),
                    self.url.clone(),
                    self.leaf.as_str().to_owned(),
                ],
                cancellation,
            )
            .await?;
        Ok(self.destination.path().to_path_buf())
    }

    /// Removes the folder this clone created, after proving it is still that folder.
    pub(crate) async fn remove_destination(&self) -> Result<(), io::Error> {
        remove_owned_clone_destination(&self.destination).await
    }

    /// `error` with the failed removal appended, so the user learns which folder to remove.
    pub(crate) fn with_cleanup_failure(
        &self,
        mut error: GitCommandError,
        cleanup_error: &io::Error,
    ) -> GitCommandError {
        error.detail = format!(
            "{}\nThe incomplete clone at {} could not be removed ({cleanup_error}). Remove it before trying again.",
            error.detail,
            display_path(self.destination.path()),
        )
        .into();
        error
    }

    /// The transfer, then, on any failure, the removal of the folder this clone created.
    pub(crate) async fn run(
        self,
        cancellation: &CancellationToken,
    ) -> Result<PathBuf, GitCommandError> {
        match self.transfer(cancellation).await {
            Ok(path) => Ok(path),
            Err(error) => match self.remove_destination().await {
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(self.with_cleanup_failure(error, &cleanup_error)),
            },
        }
    }
}

/// Removes a clone destination this call created, after proving that the directory at the
/// path is still the one it created.
async fn remove_owned_clone_destination(destination: &OwnedWorktreePath) -> Result<(), io::Error> {
    match tokio::fs::symlink_metadata(destination.path()).await {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
        Ok(_) => {}
    }
    let lease = open_removal_directory_lease_async(destination.path()).await?;
    if owned_worktree_path_identity(&lease)? != destination.identity() {
        return Err(io::Error::other(
            "the folder was replaced after the clone started",
        ));
    }
    remove_bound_owned_directory_with_lease(destination.path(), lease).await
}

fn clear_bound_identity_directory_once(
    lease: &RemovalDirectoryLease,
    path: &Path,
    marker_name: &str,
    expected_nonce: &str,
) -> Result<(), io::Error> {
    match verify_identity_marker_blocking(path, marker_name, expected_nonce) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if bound_directory_is_empty(lease, path)? {
                return Ok(());
            }
            return Err(io::Error::other(
                "the removal marker disappeared while the directory still contains data",
            ));
        }
        Err(error) => return Err(error),
    }
    verify_removal_directory_lease(lease, path)?;
    clear_bound_directory_contents(lease, path, Some(marker_name))?;
    verify_removal_directory_lease(lease, path)?;
    remove_bound_regular_file(lease, path, marker_name, Some(expected_nonce))?;
    if !bound_directory_is_empty(lease, path)? {
        return Err(io::Error::other(
            "the bound removal directory gained new contents during cleanup",
        ));
    }
    Ok(())
}

#[cfg(test)]
fn run_worktree_removal_probe_blocking(
    source: &Path,
    probe: &Path,
    cancellation: &CancellationToken,
    after_probe_rename: impl FnOnce(),
) -> Result<(), io::Error> {
    rename_worktree_path_with_retries_blocking(source, probe, Some(cancellation))?;
    after_probe_rename();
    rename_worktree_path_with_retries_blocking(probe, source, None).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "the safety probe retained the worktree at '{}': {error}",
                display_path(probe)
            ),
        )
    })
}

#[cfg(test)]
fn rename_worktree_path_with_retries_blocking(
    source: &Path,
    destination: &Path,
    cancellation: Option<&CancellationToken>,
) -> Result<(), io::Error> {
    let mut last_error = None;
    for attempt in 0..WORKTREE_REMOVE_RETRY_ATTEMPTS {
        if cancellation.is_some_and(CancellationToken::is_cancelled) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "worktree removal was cancelled",
            ));
        }
        match rename_path_blocking(source, destination) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
        if attempt + 1 < WORKTREE_REMOVE_RETRY_ATTEMPTS {
            if cancellation.is_some_and(CancellationToken::is_cancelled) {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "worktree removal was cancelled",
                ));
            }
            std::thread::sleep(WORKTREE_REMOVE_RETRY_DELAY);
        }
    }
    Err(last_error.unwrap_or_else(|| io::Error::other("worktree rename failed")))
}

async fn bounded_repository_classification(
    classification: impl Future<Output = Option<VcsRepositoryUnavailableReason>>,
    cancellation: &CancellationToken,
) -> Option<VcsRepositoryUnavailableReason> {
    bounded_repository_discovery(classification, cancellation).await
}

async fn bounded_repository_discovery<T>(
    discovery: impl Future<Output = Option<T>>,
    cancellation: &CancellationToken,
) -> Option<T> {
    tokio::select! {
        biased;
        () = cancellation.cancelled() => None,
        result = tokio::time::timeout(REPOSITORY_CLASSIFICATION_TIMEOUT, discovery) => result.ok().flatten(),
    }
}

async fn classify_repository_unavailable(
    cwd: &Path,
    probe: &RepositoryProbe,
    environment: impl Fn(&str) -> Option<OsString>,
) -> Option<VcsRepositoryUnavailableReason> {
    let RepositoryProbe::Refused(stderr) = probe else {
        return None;
    };
    if stderr.contains("detected dubious ownership") {
        return Some(VcsRepositoryUnavailableReason::Untrusted);
    }
    let (cwd, across_filesystems) = repository_discovery_start(cwd, environment).await?;
    repository_reason_from_ancestors(cwd, across_filesystems, |path| {
        tokio::fs::symlink_metadata(path)
    })
    .await
}

async fn repository_discovery_start(
    cwd: &Path,
    environment: impl Fn(&str) -> Option<OsString>,
) -> Option<(PathBuf, bool)> {
    if environment("GIT_DIR").is_some() || environment("GIT_CEILING_DIRECTORIES").is_some() {
        return None;
    }
    let cwd = tokio::fs::canonicalize(cwd).await.ok()?;
    let across_filesystems = environment("GIT_DISCOVERY_ACROSS_FILESYSTEM")
        .as_deref()
        .and_then(|value| value.to_str())
        .is_some_and(git_boolean_is_true);
    Some((cwd, across_filesystems))
}

async fn repository_reason_from_ancestors<F, Fut>(
    cwd: PathBuf,
    across_filesystems: bool,
    symlink_metadata: F,
) -> Option<VcsRepositoryUnavailableReason>
where
    F: Fn(PathBuf) -> Fut,
    Fut: Future<Output = io::Result<fs::Metadata>>,
{
    discover_repository_from_ancestors(cwd, across_filesystems, symlink_metadata)
        .await
        .map(|discovery| match discovery {
            RepositoryDiscovery::Absent => VcsRepositoryUnavailableReason::Absent,
            RepositoryDiscovery::Directory(_)
            | RepositoryDiscovery::GitFile
            | RepositoryDiscovery::DanglingLink => VcsRepositoryUnavailableReason::Unreadable,
        })
}

enum RepositoryDiscovery {
    Absent,
    Directory(PathBuf),
    GitFile,
    DanglingLink,
}

async fn discover_repository_from_ancestors<F, Fut>(
    mut cwd: PathBuf,
    across_filesystems: bool,
    symlink_metadata: F,
) -> Option<RepositoryDiscovery>
where
    F: Fn(PathBuf) -> Fut,
    Fut: Future<Output = io::Result<fs::Metadata>>,
{
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;

    #[cfg(unix)]
    let starting_device = if across_filesystems {
        None
    } else {
        Some(tokio::fs::metadata(&cwd).await.ok()?.dev())
    };
    #[cfg(not(unix))]
    let _ = across_filesystems;

    loop {
        let git_dir = cwd.join(".git");
        match symlink_metadata(git_dir.clone()).await {
            Ok(metadata) => {
                let metadata = if metadata.is_symlink() {
                    match tokio::fs::metadata(&git_dir).await {
                        Ok(metadata) => metadata,
                        Err(error)
                            if matches!(
                                error.kind(),
                                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                            ) =>
                        {
                            return Some(RepositoryDiscovery::DanglingLink);
                        }
                        Err(_) => return None,
                    }
                } else {
                    metadata
                };
                if metadata.is_file() {
                    return Some(RepositoryDiscovery::GitFile);
                }
                if metadata.is_dir() {
                    for marker in ["HEAD", "config", "objects", "refs"] {
                        match symlink_metadata(git_dir.join(marker)).await {
                            Ok(_) => return Some(RepositoryDiscovery::Directory(cwd)),
                            Err(error)
                                if matches!(
                                    error.kind(),
                                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                                ) => {}
                            Err(_) => return None,
                        }
                    }
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) => {}
            Err(_) => return None,
        }
        let Some(parent) = cwd.parent() else {
            return Some(RepositoryDiscovery::Absent);
        };
        #[cfg(unix)]
        if let Some(device) = starting_device
            && tokio::fs::metadata(parent).await.ok()?.dev() != device
        {
            return Some(RepositoryDiscovery::Absent);
        }
        cwd = parent.to_path_buf();
    }
}

fn git_boolean_is_true(value: &str) -> bool {
    if ["true", "yes", "on"]
        .iter()
        .any(|word| value.eq_ignore_ascii_case(word))
    {
        return true;
    }
    // Git also accepts nonzero signed integers (base 0, with optional k/m/g units).
    let value = value.trim_start_matches(|character| matches!(character, ' ' | '\t'..='\r'));
    let (sign, value) = if let Some(value) = value.strip_prefix('-') {
        (-1_i64, value)
    } else {
        (1_i64, value.strip_prefix('+').unwrap_or(value))
    };
    let (value, factor) = match value.as_bytes().last() {
        Some(b'k' | b'K') => (&value[..value.len() - 1], 1_i64 << 10),
        Some(b'm' | b'M') => (&value[..value.len() - 1], 1_i64 << 20),
        Some(b'g' | b'G') => (&value[..value.len() - 1], 1_i64 << 30),
        _ => (value, 1),
    };
    let (digits, radix) = if let Some(value) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        (value, 16)
    } else if let Some(value) = value
        .strip_prefix("0b")
        .or_else(|| value.strip_prefix("0B"))
    {
        (value, 2)
    } else if value.starts_with('0') {
        (value, 8)
    } else {
        (value, 10)
    };
    // Reject extra signs: from_str_radix would otherwise accept them after prefix removal.
    if digits.starts_with(['+', '-']) {
        return false;
    }
    i64::from_str_radix(digits, radix)
        .ok()
        .and_then(|number| number.checked_mul(sign)?.checked_mul(factor))
        .and_then(|number| i32::try_from(number).ok())
        .is_some_and(|number| number != 0)
}

pub(crate) fn git_environment() -> Vec<(OsString, OsString)> {
    [
        ("GCM_INTERACTIVE", "never"),
        ("GIT_ASKPASS", ""),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("LC_ALL", "C"),
        ("LANG", "C"),
        ("SSH_ASKPASS", ""),
        ("SSH_ASKPASS_REQUIRE", "never"),
    ]
    .into_iter()
    .map(|(key, value)| (key.into(), value.into()))
    .collect()
}

pub(crate) fn git_read_environment() -> Vec<(OsString, OsString)> {
    let mut environment = git_environment();
    environment.push(("GIT_OPTIONAL_LOCKS".into(), "0".into()));
    environment
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

async fn resolve_explicit_worktree_path(cwd: &Path, requested_path: &Path) -> PathBuf {
    let repository_cwd = match tokio::fs::canonicalize(cwd).await {
        Ok(path) => path,
        Err(_) if cwd.is_absolute() => cwd.to_path_buf(),
        Err(_) => std::env::current_dir()
            .map(|current_dir| current_dir.join(cwd))
            .unwrap_or_else(|_| cwd.to_path_buf()),
    };
    let path = if requested_path.is_absolute() {
        requested_path.to_path_buf()
    } else {
        repository_cwd.join(requested_path)
    };
    normalize_path_lexically(&path)
}

fn normalize_path_lexically(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                let can_pop = matches!(
                    normalized.components().next_back(),
                    Some(Component::Normal(_))
                );
                if can_pop {
                    normalized.pop();
                } else if !normalized.has_root() {
                    normalized.push(component.as_os_str());
                }
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn same_worktree_path(registered: &str, path: &Path) -> bool {
    let candidate = display_path(path);
    if cfg!(windows) {
        registered.eq_ignore_ascii_case(&candidate)
    } else {
        registered == candidate
    }
}

async fn create_rewrite_temp_files(
    operation: &str,
    cwd: &Path,
    todo_contents: &str,
    message_contents: Option<&str>,
) -> Result<RewriteTempFiles, GitCommandError> {
    let mut directory = None;
    for _ in 0..8 {
        let candidate =
            std::env::temp_dir().join(format!("bibcode-git-manager-rewrite-{}", Uuid::new_v4()));
        match tokio::fs::create_dir(&candidate).await {
            Ok(()) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;

                    if tokio::fs::set_permissions(&candidate, fs::Permissions::from_mode(0o700))
                        .await
                        .is_err()
                    {
                        let _ = tokio::fs::remove_dir(&candidate).await;
                        return Err(simple_error(
                            operation,
                            cwd,
                            "Git Manager could not protect its temporary rewrite directory.",
                        ));
                    }
                }
                directory = Some(candidate);
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(_) => {
                return Err(simple_error(
                    operation,
                    cwd,
                    "Git Manager could not create its temporary rewrite directory.",
                ));
            }
        }
    }
    let directory = directory.ok_or_else(|| {
        simple_error(
            operation,
            cwd,
            "Git Manager could not reserve a temporary rewrite directory.",
        )
    })?;
    let todo = directory.join("todo");
    let message = message_contents.map(|_| directory.join("message"));
    let files = RewriteTempFiles {
        directory,
        todo,
        message,
    };
    if tokio::fs::write(&files.todo, todo_contents).await.is_err() {
        return Err(simple_error(
            operation,
            cwd,
            "Git Manager could not write its temporary rewrite todo.",
        ));
    }
    if let (Some(path), Some(contents)) = (files.message.as_ref(), message_contents)
        && tokio::fs::write(path, contents).await.is_err()
    {
        return Err(simple_error(
            operation,
            cwd,
            "Git Manager could not write its temporary squash message.",
        ));
    }
    Ok(files)
}

fn editor_redirect_command(path: &Path) -> String {
    let path = display_path(path)
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('`', "\\`");
    format!("cat \"{path}\" >")
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        ffi::OsString,
        fs,
        path::{Path, PathBuf},
        process::Command,
        sync::{Arc, Mutex},
    };

    use tokio_util::sync::CancellationToken;

    use super::{
        GitManagerCommitOutcome, GitProcessRunner, GitRepository, OutputPolicy, ProcessError,
        ProcessOutput, ProcessRequest, git_read_environment, normalize_path_lexically,
    };
    use crate::test_support::TestSandbox;

    const EXPECTED_FUSED_OPERATIONS: [&str; 5] = [
        "GitVcsDriver.statusDetailsLocal.status",
        "GitVcsDriver.statusDetailsLocal.remotes",
        "GitManager.getRefs.inProgressPaths",
        "GitVcsDriver.defaultRef.originHead",
        "GitVcsDriver.remoteProvider",
    ];

    struct RecordingGitRunner {
        outputs: HashMap<String, ProcessOutput>,
        requests: Mutex<Vec<ProcessRequest>>,
    }

    #[derive(Clone, Copy)]
    enum WatchRootFailure {
        Cancelled,
        OutputLimit,
    }

    struct FailingWatchRootRunner {
        failure: WatchRootFailure,
        requests: Mutex<Vec<ProcessRequest>>,
    }

    impl GitProcessRunner for FailingWatchRootRunner {
        fn run<'a>(
            &'a self,
            request: ProcessRequest,
            cancellation: &'a CancellationToken,
        ) -> super::BoxGitProcessFuture<'a> {
            let operation = request.operation.clone();
            let max_output_bytes = request.max_output_bytes;
            if matches!(self.failure, WatchRootFailure::Cancelled) {
                assert!(cancellation.is_cancelled());
            }
            self.requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(request);
            let failure = self.failure;
            Box::pin(async move {
                Err(match failure {
                    WatchRootFailure::Cancelled => ProcessError::Cancelled { operation },
                    WatchRootFailure::OutputLimit => ProcessError::OutputLimit {
                        operation,
                        stream: "stdout",
                        max_bytes: max_output_bytes,
                        observed_bytes: max_output_bytes + 1,
                    },
                })
            })
        }
    }

    impl RecordingGitRunner {
        fn status_fixture() -> Self {
            let status = process_output(
                "# branch.oid 0000000000000000000000000000000000000000\n# branch.head main\n# branch.upstream origin/main\n# branch.ab +0 -0\n",
            );
            Self {
                outputs: HashMap::from([
                    (
                        "GitVcsDriver.detectRepository".into(),
                        process_output("true\n"),
                    ),
                    (
                        "GitVcsDriver.statusDetailsLocal.status".into(),
                        status.clone(),
                    ),
                    (
                        "GitVcsDriver.statusDetailsLocal.stagedNumstat".into(),
                        process_output(""),
                    ),
                    (
                        "GitVcsDriver.statusDetailsLocal.unstagedNumstat".into(),
                        process_output(""),
                    ),
                    (
                        "GitVcsDriver.statusDetailsLocal.remotes".into(),
                        process_output("origin\n"),
                    ),
                    (
                        "GitManager.getRefs.inProgressPaths".into(),
                        no_in_progress_paths(),
                    ),
                    (
                        "GitVcsDriver.defaultRef.originHead".into(),
                        process_output("refs/remotes/origin/main\n"),
                    ),
                    (
                        "GitVcsDriver.remoteProvider".into(),
                        process_output("https://github.com/example/repository.git\n"),
                    ),
                    ("GitVcsDriver.statusDetailsRemote.status".into(), status),
                ]),
                requests: Mutex::new(Vec::new()),
            }
        }

        fn dirty_tracked_fixture() -> Self {
            let status = process_output(
                "# branch.oid 0000000000000000000000000000000000000000\n# branch.head feature/test\n# branch.upstream origin/feature/test\n# branch.ab +2 -1\n1 MM N... 100644 100644 100644 0000000000000000000000000000000000000000 0000000000000000000000000000000000000000 file.rs\n",
            );
            Self {
                outputs: HashMap::from([
                    (
                        "GitVcsDriver.detectRepository".into(),
                        process_output("true\n"),
                    ),
                    (
                        "GitVcsDriver.statusDetailsLocal.status".into(),
                        status.clone(),
                    ),
                    (
                        "GitVcsDriver.statusDetailsLocal.stagedNumstat".into(),
                        process_output("2\t1\tfile.rs\n"),
                    ),
                    (
                        "GitVcsDriver.statusDetailsLocal.unstagedNumstat".into(),
                        process_output("4\t5\tfile.rs\n"),
                    ),
                    (
                        "GitVcsDriver.statusDetailsLocal.remotes".into(),
                        process_output("origin\n"),
                    ),
                    (
                        "GitManager.getRefs.inProgressPaths".into(),
                        no_in_progress_paths(),
                    ),
                    (
                        "GitVcsDriver.defaultRef.originHead".into(),
                        process_output("refs/remotes/origin/main\n"),
                    ),
                    (
                        "GitVcsDriver.remoteProvider".into(),
                        process_output("https://github.com/example/repository.git\n"),
                    ),
                    ("GitVcsDriver.statusDetailsRemote.status".into(), status),
                    (
                        "GitVcsDriver.statusDetailsRemote.defaultDelta".into(),
                        process_output("3\n"),
                    ),
                ]),
                requests: Mutex::new(Vec::new()),
            }
        }

        fn clean_fixture() -> Self {
            Self::status_fixture()
        }

        fn requests(&self) -> Vec<ProcessRequest> {
            self.requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }

        fn operation_names(&self) -> Vec<String> {
            self.requests()
                .into_iter()
                .map(|request| request.operation)
                .collect()
        }

        fn count_arg(&self, argument: &str) -> usize {
            self.requests()
                .iter()
                .filter(|request| request.args.iter().any(|value| value == argument))
                .count()
        }
    }

    fn process_output(stdout: &str) -> ProcessOutput {
        ProcessOutput {
            exit_code: 0,
            stdout: stdout.to_owned(),
            stderr: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
        }
    }

    fn process_result(exit_code: i32, stdout: &str, stderr: &str) -> ProcessOutput {
        ProcessOutput {
            exit_code,
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
            stdout_truncated: false,
            stderr_truncated: false,
        }
    }

    /// The arguments a network transfer receives: Git's stall guard, then the command.
    fn network_args(args: &[&str]) -> Vec<OsString> {
        [
            "-c",
            "http.lowSpeedLimit=1000",
            "-c",
            "http.lowSpeedTime=60",
        ]
        .iter()
        .chain(args)
        .map(OsString::from)
        .collect()
    }

    fn assert_network_transfer(request: &ProcessRequest, args: &[&str]) {
        assert_eq!(request.args, network_args(args), "{}", request.operation);
        assert_eq!(
            request.timeout,
            crate::git::NETWORK_TRANSFER_TIMEOUT,
            "{} can be cancelled and runs on the network-transfer budget",
            request.operation
        );
    }

    fn assert_bounded_transfer(request: &ProcessRequest, args: &[&str]) {
        assert_eq!(request.args, network_args(args), "{}", request.operation);
        assert_eq!(
            request.timeout,
            crate::git::BOUNDED_TRANSFER_TIMEOUT,
            "{} has no Cancel and keeps the bounded deadline",
            request.operation
        );
    }

    fn assert_default_budget(request: &ProcessRequest) {
        assert_eq!(
            request.timeout,
            super::DEFAULT_TIMEOUT,
            "{} keeps the ordinary deadline",
            request.operation
        );
        assert!(
            !request
                .args
                .iter()
                .any(|argument| argument.to_string_lossy().starts_with("http.lowSpeed")),
            "{} has no stall guard",
            request.operation
        );
    }

    /// Eleven `--git-path` answers that name no existing file, so the probe reports no operation.
    fn no_in_progress_paths() -> ProcessOutput {
        process_output(
            &(0..11)
                .map(|index| format!(".git/bibcode-none-{index}"))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    impl GitProcessRunner for RecordingGitRunner {
        fn run<'a>(
            &'a self,
            request: ProcessRequest,
            _cancellation: &'a CancellationToken,
        ) -> super::BoxGitProcessFuture<'a> {
            let output = self
                .outputs
                .get(&request.operation)
                .cloned()
                .expect("fixture Git output");
            self.requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(request);
            Box::pin(async move { Ok(output) })
        }
    }

    #[tokio::test]
    async fn repository_unavailable_plain_folder_is_absent() {
        let Some(root) = plain_folder_fixture("repository_unavailable_plain_folder_is_absent")
        else {
            return;
        };
        assert_repository_reason(&GitRepository::default(), root.path(), Some("absent")).await;
    }

    fn plain_folder_fixture(test: &str) -> Option<tempfile::TempDir> {
        let root = tempfile::tempdir().expect("plain folder");
        let probe = Command::new("git")
            .args(["rev-parse", "--is-inside-work-tree"])
            .current_dir(root.path())
            .output()
            .expect("fixture Git starts");
        if probe.status.success() {
            eprintln!(
                "{test}: skipping plain-folder test because the temporary directory {} is inside a Git repository",
                root.path().display()
            );
            return None;
        }
        Some(root)
    }

    #[tokio::test]
    async fn repository_unavailable_markerless_git_ancestor_is_absent() {
        let Some(root) =
            plain_folder_fixture("repository_unavailable_markerless_git_ancestor_is_absent")
        else {
            return;
        };
        let git_dir = root.path().join(".git");
        fs::create_dir(&git_dir).expect("empty git directory");
        let nested = root.path().join("nested/deep");
        fs::create_dir_all(&nested).expect("nested folder");
        assert_repository_reason(&GitRepository::default(), &nested, Some("absent")).await;

        fs::write(git_dir.join("unrelated"), "not repository metadata").expect("unrelated file");
        assert_repository_reason(&GitRepository::default(), &nested, Some("absent")).await;
    }

    #[tokio::test]
    async fn repository_unavailable_single_git_marker_is_unreadable() {
        let Some(root) =
            plain_folder_fixture("repository_unavailable_single_git_marker_is_unreadable")
        else {
            return;
        };
        for marker in ["HEAD", "config", "objects", "refs"] {
            let folder = root.path().join(marker);
            let git_dir = folder.join(".git");
            fs::create_dir_all(&git_dir).expect("git directory");
            if matches!(marker, "objects" | "refs") {
                fs::create_dir(git_dir.join(marker)).expect("repository directory marker");
            } else {
                fs::write(git_dir.join(marker), "").expect("repository file marker");
            }
            assert_repository_reason(&GitRepository::default(), &folder, Some("unreadable")).await;
        }
    }

    #[tokio::test]
    async fn repository_unavailable_markerless_git_directory_continues_to_damaged_ancestor() {
        let root = tempfile::tempdir().expect("damaged ancestor");
        repository_fixture_git(root.path(), &["init", "-q", "-b", "main"]);
        fs::write(root.path().join(".git/config"), "[invalid").expect("damaged config");
        let nested = root.path().join("nested");
        fs::create_dir_all(nested.join(".git")).expect("empty nested git directory");
        assert_repository_reason(&GitRepository::default(), &nested, Some("unreadable")).await;
    }

    fn repository_fixture_git(cwd: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "NUL" } else { "/dev/null" },
            )
            .env("GIT_AUTHOR_NAME", "Repository State Test")
            .env("GIT_AUTHOR_EMAIL", "repository@example.test")
            .env("GIT_COMMITTER_NAME", "Repository State Test")
            .env("GIT_COMMITTER_EMAIL", "repository@example.test")
            .output()
            .expect("fixture Git starts");
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("Git output")
            .trim()
            .to_owned()
    }

    async fn assert_repository_reason(
        repository: &GitRepository,
        cwd: &Path,
        reason: Option<&str>,
    ) {
        let local = repository
            .local_status(cwd, &CancellationToken::new())
            .await
            .expect("local status");
        assert!(!local.is_repo);
        let encoded = serde_json::to_value(local).expect("status JSON");
        assert_eq!(
            encoded
                .get("repositoryUnavailableReason")
                .and_then(serde_json::Value::as_str),
            reason
        );
    }

    #[tokio::test]
    async fn repository_unavailable_damaged_metadata_is_unreadable() {
        for (path, contents) in [("HEAD", "not a ref"), ("HEAD", ""), ("config", "[invalid")] {
            let root = tempfile::tempdir().expect("damaged repository");
            repository_fixture_git(root.path(), &["init", "-q", "-b", "main"]);
            fs::write(root.path().join(".git").join(path), contents).expect("damage metadata");
            assert_repository_reason(&GitRepository::default(), root.path(), Some("unreadable"))
                .await;
        }
    }

    #[tokio::test]
    async fn repository_unavailable_damaged_ancestor_is_unreadable() {
        let root = tempfile::tempdir().expect("damaged repository");
        repository_fixture_git(root.path(), &["init", "-q", "-b", "main"]);
        fs::write(root.path().join(".git/HEAD"), "not a ref").expect("damage HEAD");
        let nested = root.path().join("nested/deep");
        fs::create_dir_all(&nested).expect("nested folder");
        assert_repository_reason(&GitRepository::default(), &nested, Some("unreadable")).await;
    }

    async fn assert_fallback_watch_roots(cwd: &Path, worktree: &Path) {
        let roots = GitRepository::default()
            .fallback_watch_roots(cwd, &CancellationToken::new())
            .await
            .expect("damaged repository has fallback watch roots");
        assert_eq!(roots.worktree_root, fs::canonicalize(worktree).unwrap());
        assert_eq!(
            roots.git_dir,
            fs::canonicalize(worktree.join(".git")).unwrap()
        );
        assert_eq!(roots.common_dir, roots.git_dir);
    }

    #[tokio::test]
    async fn repository_unavailable_fallback_watch_roots_for_damaged_head() {
        let root = tempfile::tempdir().expect("damaged repository");
        repository_fixture_git(root.path(), &["init", "-q", "-b", "main"]);
        fs::write(root.path().join(".git/HEAD"), "not a ref").expect("damage HEAD");
        assert_fallback_watch_roots(root.path(), root.path()).await;
    }

    #[tokio::test]
    async fn repository_unavailable_fallback_watch_roots_for_malformed_config() {
        let root = tempfile::tempdir().expect("damaged repository");
        repository_fixture_git(root.path(), &["init", "-q", "-b", "main"]);
        fs::write(root.path().join(".git/config"), "[invalid").expect("damage config");
        assert_fallback_watch_roots(root.path(), root.path()).await;
    }

    #[tokio::test]
    async fn repository_unavailable_fallback_watch_roots_use_the_damaged_ancestor() {
        let root = tempfile::tempdir().expect("damaged repository");
        repository_fixture_git(root.path(), &["init", "-q", "-b", "main"]);
        fs::write(root.path().join(".git/HEAD"), "not a ref").expect("damage HEAD");
        let nested = root.path().join("nested/deep");
        fs::create_dir_all(&nested).expect("nested folder");
        assert_fallback_watch_roots(&nested, root.path()).await;

        fs::create_dir(nested.join(".git")).expect("marker-less nested git directory");
        fs::write(nested.join(".git/unrelated"), "not metadata").expect("unrelated file");
        assert_fallback_watch_roots(&nested, root.path()).await;
    }

    #[tokio::test]
    async fn repository_unavailable_fallback_watch_roots_require_a_repository_marker() {
        let Some(root) = plain_folder_fixture(
            "repository_unavailable_fallback_watch_roots_require_a_repository_marker",
        ) else {
            return;
        };
        let repository = GitRepository::default();
        assert!(
            repository
                .fallback_watch_roots(root.path(), &CancellationToken::new())
                .await
                .is_none()
        );
        fs::create_dir(root.path().join(".git")).expect("marker-less git directory");
        fs::write(root.path().join(".git/unrelated"), "not metadata").expect("unrelated file");
        assert!(
            repository
                .fallback_watch_roots(root.path(), &CancellationToken::new())
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn repository_unavailable_fallback_watch_roots_accept_each_repository_marker() {
        let root = tempfile::tempdir().expect("repository markers");
        for marker in ["HEAD", "config", "objects", "refs"] {
            let worktree = root.path().join(marker);
            fs::create_dir_all(worktree.join(".git")).expect("git directory");
            fs::write(worktree.join(".git").join(marker), "").expect("repository marker");
            assert_fallback_watch_roots(&worktree, &worktree).await;
        }
    }

    #[tokio::test]
    async fn repository_unavailable_fallback_watch_roots_reject_gitfiles() {
        let root = tempfile::tempdir().expect("damaged repository");
        repository_fixture_git(root.path(), &["init", "-q", "-b", "main"]);
        fs::write(root.path().join(".git/HEAD"), "not a ref").expect("damage HEAD");
        let nested = root.path().join("nested");
        fs::create_dir(&nested).expect("nested folder");
        fs::write(nested.join(".git"), "gitdir: missing-admin\n").expect("dangling gitfile");
        assert!(
            GitRepository::default()
                .fallback_watch_roots(&nested, &CancellationToken::new())
                .await
                .is_none(),
            "a gitfile must stop discovery before the damaged parent"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn repository_unavailable_fallback_watch_roots_reject_dangling_git_links() {
        let root = tempfile::tempdir().expect("dangling git entry");
        std::os::unix::fs::symlink(root.path().join("missing"), root.path().join(".git"))
            .expect("dangling git link");
        assert!(
            GitRepository::default()
                .fallback_watch_roots(root.path(), &CancellationToken::new())
                .await
                .is_none()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn repository_unavailable_fallback_watch_roots_canonicalize_git_directory_links() {
        let root = tempfile::tempdir().expect("symlinked git directory");
        let worktree = root.path().join("worktree");
        let git_dir = root.path().join("metadata");
        fs::create_dir(&worktree).expect("worktree directory");
        fs::create_dir(&git_dir).expect("git directory");
        fs::write(git_dir.join("HEAD"), "not a ref").expect("damaged HEAD");
        std::os::unix::fs::symlink(&git_dir, worktree.join(".git")).expect("git directory link");
        assert_fallback_watch_roots(&worktree, &worktree).await;
    }

    #[tokio::test]
    async fn repository_unavailable_fallback_watch_roots_reject_discovery_overrides() {
        let root = tempfile::tempdir().expect("damaged repository");
        fs::create_dir(root.path().join(".git")).expect("git directory");
        fs::write(root.path().join(".git/HEAD"), "not a ref").expect("damaged HEAD");
        for environment in [
            (|name: &str| (name == "GIT_DIR").then(OsString::new)) as fn(&str) -> Option<OsString>,
            |name: &str| (name == "GIT_CEILING_DIRECTORIES").then(|| OsString::from("/")),
        ] {
            let repository = GitRepository {
                discovery_environment: environment,
                ..GitRepository::default()
            };
            assert!(
                repository
                    .fallback_watch_roots(root.path(), &CancellationToken::new())
                    .await
                    .is_none()
            );
        }
    }

    #[tokio::test]
    async fn repository_unavailable_fallback_watch_roots_are_cancellable() {
        let root = tempfile::tempdir().expect("damaged repository");
        fs::create_dir(root.path().join(".git")).expect("git directory");
        fs::write(root.path().join(".git/HEAD"), "not a ref").expect("damaged HEAD");
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        assert!(
            GitRepository::default()
                .fallback_watch_roots(root.path(), &cancellation)
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn repository_unavailable_fallback_watch_roots_reject_metadata_errors() {
        let root = tempfile::tempdir().expect("missing worktree");
        assert!(
            GitRepository::default()
                .fallback_watch_roots(&root.path().join("missing"), &CancellationToken::new())
                .await
                .is_none()
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.path().join(".git"), root.path().join(".git"))
                .expect("git symlink loop");
            assert!(
                GitRepository::default()
                    .fallback_watch_roots(root.path(), &CancellationToken::new())
                    .await
                    .is_none()
            );
        }
    }

    #[tokio::test]
    async fn repository_unavailable_deleted_worktree_admin_is_unreadable() {
        let root = tempfile::tempdir().expect("repository and worktree");
        let main = root.path().join("main");
        let linked = root.path().join("linked");
        fs::create_dir(&main).expect("main folder");
        repository_fixture_git(&main, &["init", "-q", "-b", "main"]);
        let tree = repository_fixture_git(&main, &["mktree"]);
        let commit = repository_fixture_git(&main, &["commit-tree", &tree, "-m", "fixture"]);
        repository_fixture_git(&main, &["update-ref", "HEAD", &commit]);
        repository_fixture_git(
            &main,
            &[
                "worktree",
                "add",
                "--detach",
                linked.to_str().expect("path"),
                "HEAD",
            ],
        );
        fs::remove_dir_all(main.join(".git/worktrees")).expect("remove linked admin directory");
        assert_repository_reason(&GitRepository::default(), &linked, Some("unreadable")).await;
    }

    #[tokio::test]
    async fn repository_unavailable_healthy_and_bare_repositories_omit_reason() {
        let root = tempfile::tempdir().expect("healthy repository");
        repository_fixture_git(root.path(), &["init", "-q", "-b", "main"]);
        let repository = GitRepository::default();
        let local = repository
            .local_status(root.path(), &CancellationToken::new())
            .await
            .expect("healthy status");
        assert!(local.is_repo);
        assert!(
            serde_json::to_value(local)
                .expect("status JSON")
                .get("repositoryUnavailableReason")
                .is_none()
        );
        assert_repository_reason(&repository, &root.path().join(".git"), None).await;
        let bare = tempfile::tempdir().expect("bare repository");
        repository_fixture_git(bare.path(), &["init", "-q", "--bare"]);
        assert_repository_reason(&repository, bare.path(), None).await;
    }

    #[test]
    fn repository_unavailable_reason_wire_round_trip_and_omission() {
        let local = super::VcsStatusLocalResult::non_repository();
        let mut encoded = serde_json::to_value(&local).expect("status JSON");
        assert!(encoded.get("repositoryUnavailableReason").is_none());
        assert_eq!(
            serde_json::from_value::<super::VcsStatusLocalResult>(encoded.clone()).unwrap(),
            local
        );
        for reason in ["absent", "unreadable", "untrusted"] {
            encoded["repositoryUnavailableReason"] = reason.into();
            let decoded: super::VcsStatusLocalResult =
                serde_json::from_value(encoded.clone()).expect("reason decodes");
            assert_eq!(serde_json::to_value(decoded).unwrap(), encoded);
        }
    }

    #[tokio::test]
    async fn repository_unavailable_discovery_overrides_leave_the_reason_unknown() {
        let Some(root) = plain_folder_fixture(
            "repository_unavailable_discovery_overrides_leave_the_reason_unknown",
        ) else {
            return;
        };
        for environment in [
            (|name: &str| (name == "GIT_DIR").then(OsString::new)) as fn(&str) -> Option<OsString>,
            |name: &str| (name == "GIT_CEILING_DIRECTORIES").then(|| OsString::from("/")),
        ] {
            let repository = GitRepository {
                discovery_environment: environment,
                ..GitRepository::default()
            };
            assert_repository_reason(&repository, root.path(), None).await;
        }
    }

    #[tokio::test]
    async fn repository_unavailable_dubious_ownership_takes_precedence_over_discovery_overrides() {
        let reason = super::classify_repository_unavailable(
            Path::new("/does-not-exist"),
            &super::RepositoryProbe::Refused(
                "fatal: detected dubious ownership in repository at '/repo'".into(),
            ),
            |_| Some(OsString::new()),
        )
        .await;
        assert_eq!(
            reason,
            Some(super::VcsRepositoryUnavailableReason::Untrusted)
        );
    }

    struct DifferentOwnerGitRunner;

    impl GitProcessRunner for DifferentOwnerGitRunner {
        fn run<'a>(
            &'a self,
            mut request: ProcessRequest,
            cancellation: &'a CancellationToken,
        ) -> super::BoxGitProcessFuture<'a> {
            request
                .env
                .push(("GIT_TEST_ASSUME_DIFFERENT_OWNER".into(), "1".into()));
            request.env.push((
                "GIT_CONFIG_GLOBAL".into(),
                if cfg!(windows) { "NUL" } else { "/dev/null" }.into(),
            ));
            Box::pin(async move { super::ProcessRunner.run(request, cancellation).await })
        }
    }

    #[tokio::test]
    async fn repository_unavailable_real_git_ownership_refusal_is_untrusted() {
        let root = tempfile::tempdir().expect("untrusted repository");
        repository_fixture_git(root.path(), &["init", "-q", "-b", "main"]);
        assert_repository_reason(
            &GitRepository::with_runner_for_test(Arc::new(DifferentOwnerGitRunner)),
            root.path(),
            Some("untrusted"),
        )
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn repository_unavailable_classification_timeout_is_unknown() {
        let started = tokio::time::Instant::now();
        let reason = super::bounded_repository_classification(
            std::future::pending(),
            &CancellationToken::new(),
        )
        .await;
        assert_eq!(reason, None);
        assert_eq!(started.elapsed(), std::time::Duration::from_secs(2));
    }

    #[tokio::test]
    async fn repository_unavailable_classification_is_cancellable() {
        let cancellation = CancellationToken::new();
        let classification =
            super::bounded_repository_classification(std::future::pending(), &cancellation);
        tokio::pin!(classification);
        tokio::select! {
            biased;
            _ = &mut classification => panic!("classification must wait"),
            () = tokio::task::yield_now() => {},
        }
        cancellation.cancel();
        assert_eq!(classification.await, None);
    }

    #[tokio::test]
    async fn repository_unavailable_metadata_errors_leave_the_reason_unknown() {
        let Some(root) =
            plain_folder_fixture("repository_unavailable_metadata_errors_leave_the_reason_unknown")
        else {
            return;
        };
        let reason =
            super::repository_reason_from_ancestors(root.path().to_path_buf(), false, |_| async {
                Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
            })
            .await;
        assert_eq!(reason, None);
        let missing = root.path().join("missing");
        assert_eq!(
            super::classify_repository_unavailable(
                &missing,
                &super::RepositoryProbe::Refused(String::new()),
                |_| None
            )
            .await,
            None
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn repository_unavailable_dangling_git_entry_is_unreadable() {
        let root = tempfile::tempdir().expect("dangling metadata");
        std::os::unix::fs::symlink(root.path().join("missing"), root.path().join(".git"))
            .expect("dangling git entry");
        assert_repository_reason(&GitRepository::default(), root.path(), Some("unreadable")).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn repository_unavailable_symlinked_git_directory_requires_a_marker() {
        let Some(root) = plain_folder_fixture(
            "repository_unavailable_symlinked_git_directory_requires_a_marker",
        ) else {
            return;
        };
        let git_dir = root.path().join("metadata");
        fs::create_dir(&git_dir).expect("metadata directory");
        std::os::unix::fs::symlink(&git_dir, root.path().join(".git")).expect("git directory link");
        assert_repository_reason(&GitRepository::default(), root.path(), Some("absent")).await;
        fs::write(git_dir.join("HEAD"), "not a ref").expect("damaged HEAD");
        assert_repository_reason(&GitRepository::default(), root.path(), Some("unreadable")).await;
    }

    #[tokio::test]
    async fn repository_unavailable_git_marker_errors_leave_the_reason_unknown() {
        let root = tempfile::tempdir().expect("discovery start");
        let directory = fs::metadata(root.path()).expect("directory metadata");
        let reason =
            super::repository_reason_from_ancestors(root.path().to_path_buf(), false, |path| {
                std::future::ready(if path.file_name().is_some_and(|name| name == ".git") {
                    Ok(directory.clone())
                } else {
                    Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
                })
            })
            .await;
        assert_eq!(reason, None);
    }

    #[tokio::test]
    async fn repository_unavailable_missing_git_markers_walk_to_the_root() {
        let root = tempfile::tempdir().expect("discovery start");
        let directory = fs::metadata(root.path()).expect("directory metadata");
        for kind in [
            std::io::ErrorKind::NotFound,
            std::io::ErrorKind::NotADirectory,
        ] {
            let reason = super::repository_reason_from_ancestors(
                fs::canonicalize(root.path()).unwrap(),
                true,
                |path| {
                    std::future::ready(if path.file_name().is_some_and(|name| name == ".git") {
                        Ok(directory.clone())
                    } else {
                        Err(std::io::Error::from(kind))
                    })
                },
            )
            .await;
            assert_eq!(reason, Some(super::VcsRepositoryUnavailableReason::Absent));
        }
    }

    #[tokio::test]
    async fn repository_unavailable_missing_entries_walk_to_the_root() {
        let root = tempfile::tempdir().expect("discovery start");
        for kind in [
            std::io::ErrorKind::NotFound,
            std::io::ErrorKind::NotADirectory,
        ] {
            let reason = super::repository_reason_from_ancestors(
                fs::canonicalize(root.path()).unwrap(),
                true,
                |_| std::future::ready(Err(std::io::Error::from(kind))),
            )
            .await;
            assert_eq!(reason, Some(super::VcsRepositoryUnavailableReason::Absent));
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn repository_unavailable_checks_the_device_before_inspecting_parent_entries() {
        use std::os::unix::fs::MetadataExt;

        let proc = fs::metadata("/proc").expect("proc filesystem");
        let root = fs::metadata("/").expect("root filesystem");
        assert_ne!(proc.dev(), root.dev(), "proc is a separate filesystem");
        for (across_filesystems, expected) in [
            (false, super::VcsRepositoryUnavailableReason::Absent),
            (true, super::VcsRepositoryUnavailableReason::Unreadable),
        ] {
            // Model a .git entry beyond the real proc mount without writing to either root.
            let reason = super::repository_reason_from_ancestors(
                PathBuf::from("/proc"),
                across_filesystems,
                |path| {
                    std::future::ready(if matches!(path.to_str(), Some("/.git" | "/.git/HEAD")) {
                        Ok(root.clone())
                    } else {
                        Err(std::io::Error::from(std::io::ErrorKind::NotFound))
                    })
                },
            )
            .await;
            assert_eq!(reason, Some(expected));
        }
    }

    #[test]
    fn repository_unavailable_discovery_accepts_git_true_values() {
        for value in [
            "true",
            "TRUE",
            "yes",
            "Yes",
            "on",
            "ON",
            "1",
            "2",
            "-1",
            "+1",
            "01",
            "0x10",
            "0b1",
            "0B10",
            "-0b1",
            "1k",
            "1M",
            "1g",
            " 1",
            "\u{000b}1",
            "-2147483648",
        ] {
            assert!(
                super::git_boolean_is_true(value),
                "{value:?} is true in Git"
            );
        }
        for value in [
            "false",
            "no",
            "off",
            "0",
            "0k",
            "0b0",
            "0b2",
            "",
            "true ",
            "unknown",
            "08",
            "++1",
            "0x-1",
            "2147483648",
            "-2147483649",
        ] {
            assert!(
                !super::git_boolean_is_true(value),
                "{value:?} is not true in Git"
            );
        }
    }

    #[tokio::test]
    async fn repository_unavailable_worktree_probe_preserves_the_status_error() {
        let root = tempfile::tempdir().expect("repository with corrupt index");
        repository_fixture_git(root.path(), &["init", "-q", "-b", "main"]);
        fs::write(root.path().join(".git/index"), "invalid index").expect("damage index");
        let repository = GitRepository::default();
        assert!(
            repository
                .is_repository(root.path(), &CancellationToken::new())
                .await
                .unwrap()
        );
        let error = repository
            .local_status(root.path(), &CancellationToken::new())
            .await
            .expect_err("a readable worktree keeps the error");
        assert_eq!(
            error.operation.as_ref(),
            "GitVcsDriver.statusDetailsLocal.status"
        );
        assert!(error.detail.contains("index"));
    }

    #[tokio::test]
    async fn repository_unavailable_healthy_path_never_reads_discovery_environment() {
        let repository = GitRepository {
            discovery_environment: |_| {
                panic!("healthy status must not classify repository discovery")
            },
            ..GitRepository::with_runner_for_test(Arc::new(RecordingGitRunner::clean_fixture()))
        };
        assert!(
            repository
                .local_status(Path::new("/repo"), &CancellationToken::new())
                .await
                .unwrap()
                .is_repo
        );
    }

    #[tokio::test]
    async fn git_manager_stash_reflog_observation_is_bounded_and_cancellable() {
        let root = tempfile::tempdir().expect("reflog fixture");
        let repository = GitRepository::default();
        let cancellation = CancellationToken::new();
        let observe =
            || repository.git_manager_signal_stash_reflog(root.path(), root.path(), &cancellation);
        assert!(observe().await.expect("absent reflog").is_empty());
        let path = root.path().join("logs/refs/stash");
        fs::create_dir_all(path.parent().expect("reflog parent")).expect("reflog directory");
        fs::write(&path, b"first entry\nsecond entry\n").expect("reflog");
        assert_eq!(
            observe().await.expect("reflog bytes"),
            b"first entry\nsecond entry\n"
        );
        fs::write(&path, b"second entry\n").expect("non-top rewrite");
        assert_eq!(
            observe().await.expect("rewritten reflog"),
            b"second entry\n"
        );
        fs::write(&path, vec![0; super::GIT_MANAGER_STASH_REFLOG_LIMIT + 1])
            .expect("oversized reflog");
        assert!(
            observe()
                .await
                .expect_err("bounded reflog")
                .detail
                .contains("byte limit")
        );
        fs::remove_file(&path).expect("remove file");
        fs::create_dir(&path).expect("non-file reflog");
        assert!(observe().await.is_err());
        cancellation.cancel();
        assert!(
            observe()
                .await
                .expect_err("cancelled")
                .detail
                .contains("cancelled")
        );
    }

    #[tokio::test]
    async fn git_manager_signal_head_distinguishes_named_detached_and_unborn() {
        for (head, oid, expected_ref, expected_oid) in [
            (
                process_result(0, "main\n", ""),
                process_result(0, "deadbeef\n", ""),
                Some("main"),
                Some("deadbeef"),
            ),
            (
                process_result(1, "", ""),
                process_result(0, "deadbeef\n", ""),
                None,
                Some("deadbeef"),
            ),
            (
                process_result(0, "main\n", ""),
                process_result(1, "", ""),
                Some("main"),
                None,
            ),
            // An interrupted clone's placeholder HEAD, on files and on reftable.
            (
                process_result(128, "", "fatal: No such ref: HEAD"),
                process_result(1, "", ""),
                None,
                None,
            ),
            (
                process_result(1, "", ""),
                process_result(1, "", ""),
                None,
                None,
            ),
        ] {
            let runner = Arc::new(RecordingGitRunner {
                outputs: HashMap::from([
                    ("GitManager.signal.headRef".into(), head),
                    ("GitManager.signal.headSha".into(), oid),
                ]),
                requests: Mutex::new(Vec::new()),
            });
            let head = GitRepository::with_runner_for_test(runner.clone())
                .git_manager_signal_head(Path::new("/repo"), &CancellationToken::new())
                .await
                .expect("HEAD observation");
            assert_eq!(head.reference.as_deref(), expected_ref);
            assert_eq!(head.oid.as_deref(), expected_oid);
            assert!(
                runner
                    .requests
                    .lock()
                    .expect("requests")
                    .iter()
                    .all(|request| request
                        .env
                        .iter()
                        .any(|(name, value)| name == "GIT_OPTIONAL_LOCKS" && value == "0"))
            );
        }
    }

    #[tokio::test]
    async fn git_manager_signal_head_rejects_invalid_or_failed_reads() {
        for (head, oid, expected_operation) in [
            (
                process_result(128, "", "fatal"),
                process_result(0, "deadbeef\n", ""),
                "GitManager.signal.headRef",
            ),
            (
                process_result(0, "", ""),
                process_result(0, "deadbeef\n", ""),
                "GitManager.signal.headRef",
            ),
            (
                process_result(0, "main\n", ""),
                process_result(128, "", "fatal"),
                "GitManager.signal.headSha",
            ),
            (
                process_result(128, "", "fatal"),
                process_result(128, "", "fatal"),
                "GitManager.signal.headRef",
            ),
            (
                process_result(0, "main\n", ""),
                process_result(0, "", ""),
                "GitManager.signal.headSha",
            ),
        ] {
            let runner = Arc::new(RecordingGitRunner {
                outputs: HashMap::from([
                    ("GitManager.signal.headRef".into(), head),
                    ("GitManager.signal.headSha".into(), oid),
                ]),
                requests: Mutex::new(Vec::new()),
            });
            let error = GitRepository::with_runner_for_test(runner)
                .git_manager_signal_head(Path::new("/repo"), &CancellationToken::new())
                .await
                .expect_err("HEAD failure must not become a signature");
            assert_eq!(error.operation.as_ref(), expected_operation);
        }
    }

    #[test]
    fn background_git_reads_disable_optional_locks() {
        let environment = git_read_environment();

        assert!(
            environment
                .iter()
                .any(|(key, value)| key == "GIT_OPTIONAL_LOCKS" && value == "0")
        );
    }

    #[tokio::test]
    async fn current_ref_uses_the_read_only_git_environment() {
        let runner = Arc::new(RecordingGitRunner {
            outputs: HashMap::from([(
                "GitVcsDriver.currentRef".into(),
                process_output("feature/test\n"),
            )]),
            requests: Mutex::new(Vec::new()),
        });
        let repository = GitRepository::with_runner_for_test(runner.clone());

        assert_eq!(
            repository
                .current_ref(Path::new("/repo"), &CancellationToken::new())
                .await
                .expect("current ref fixture succeeds")
                .as_deref(),
            Some("feature/test")
        );
        let requests = runner.requests();
        assert_eq!(requests.len(), 1);
        assert!(
            requests[0]
                .env
                .iter()
                .any(|(key, value)| key == "GIT_OPTIONAL_LOCKS" && value == "0")
        );
    }

    #[tokio::test]
    async fn git_manager_branch_create_uses_the_requested_start_and_tracking_policy() {
        let runner = Arc::new(RecordingGitRunner {
            outputs: HashMap::from([(
                "GitManager.branchCreate".into(),
                process_output("created\n"),
            )]),
            requests: Mutex::new(Vec::new()),
        });
        let repository = GitRepository::with_runner_for_test(runner.clone());

        repository
            .git_manager_create_branch(
                Path::new("/repo"),
                "feature/topic",
                Some("main"),
                false,
                &CancellationToken::new(),
            )
            .await
            .expect("branch creation succeeds");

        assert_eq!(
            runner.requests()[0].args,
            ["branch", "feature/topic", "main", "--no-track"]
                .into_iter()
                .map(OsString::from)
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn git_manager_branch_create_and_checkout_never_tracks_its_start_point() {
        let runner = Arc::new(RecordingGitRunner {
            outputs: HashMap::from([(
                "GitManager.branchCreate".into(),
                process_output("created\n"),
            )]),
            requests: Mutex::new(Vec::new()),
        });
        let repository = GitRepository::with_runner_for_test(runner.clone());

        repository
            .git_manager_create_branch(
                Path::new("/repo"),
                "feature/topic",
                Some("refs/remotes/origin/main"),
                true,
                &CancellationToken::new(),
            )
            .await
            .expect("branch creation succeeds");

        // A remote start point must not become the new branch's upstream: pushing
        // `feature/topic` would otherwise target `main`.
        assert_eq!(
            runner.requests()[0].args,
            [
                "switch",
                "-c",
                "feature/topic",
                "refs/remotes/origin/main",
                "--no-track"
            ]
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn git_manager_branch_checkout_supports_local_and_remote_tracking_refs() {
        let runner = Arc::new(RecordingGitRunner {
            outputs: HashMap::from([
                (
                    "GitManager.branchCheckout.local".into(),
                    process_output("local\n"),
                ),
                (
                    "GitManager.branchCheckout.remote".into(),
                    process_output("remote\n"),
                ),
            ]),
            requests: Mutex::new(Vec::new()),
        });
        let repository = GitRepository::with_runner_for_test(runner.clone());

        repository
            .git_manager_checkout_local_branch(
                Path::new("/repo"),
                "topic",
                &CancellationToken::new(),
            )
            .await
            .expect("local checkout succeeds");
        repository
            .git_manager_checkout_remote_branch(
                Path::new("/repo"),
                "topic-remote",
                "origin/topic",
                &CancellationToken::new(),
            )
            .await
            .expect("remote checkout succeeds");

        let requests = runner.requests();
        assert_eq!(
            requests[0].args,
            ["switch", "topic"]
                .into_iter()
                .map(OsString::from)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            requests[1].args,
            ["switch", "-c", "topic-remote", "--track", "origin/topic"]
                .into_iter()
                .map(OsString::from)
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn git_manager_case_only_branch_rename_retries_with_capital_m() {
        let runner = Arc::new(RecordingGitRunner {
            outputs: HashMap::from([
                (
                    "GitManager.branchRename".into(),
                    process_result(1, "", "case collision"),
                ),
                (
                    "GitManager.branchRename.forceCase".into(),
                    process_output("renamed\n"),
                ),
            ]),
            requests: Mutex::new(Vec::new()),
        });
        let repository = GitRepository::with_runner_for_test(runner.clone());

        let outputs = repository
            .git_manager_rename_branch(
                Path::new("/repo"),
                "Feature",
                "feature",
                &CancellationToken::new(),
            )
            .await
            .expect("case-only rename succeeds on retry");

        assert_eq!(outputs.len(), 2);
        let requests = runner.requests();
        assert_eq!(requests[0].args[1], "-m");
        assert_eq!(requests[1].args[1], "-M");
    }

    #[tokio::test]
    async fn git_manager_branch_delete_keeps_force_and_remote_deletion_explicit() {
        let runner = Arc::new(RecordingGitRunner {
            outputs: HashMap::from([
                (
                    "GitManager.branchDelete".into(),
                    process_output("deleted\n"),
                ),
                (
                    "GitManager.remoteBranchDelete".into(),
                    process_output("remote deleted\n"),
                ),
            ]),
            requests: Mutex::new(Vec::new()),
        });
        let repository = GitRepository::with_runner_for_test(runner.clone());

        repository
            .git_manager_delete_branch(Path::new("/repo"), "topic", true, &CancellationToken::new())
            .await
            .expect("local delete succeeds");
        repository
            .git_manager_delete_remote_branch(
                Path::new("/repo"),
                "origin",
                "topic",
                &CancellationToken::new(),
            )
            .await
            .expect("remote delete succeeds");

        let requests = runner.requests();
        assert_eq!(
            requests[0].args,
            ["branch", "-D", "topic"]
                .into_iter()
                .map(OsString::from)
                .collect::<Vec<_>>()
        );
        assert_default_budget(&requests[0]);
        assert_network_transfer(&requests[1], &["push", "origin", ":topic"]);
    }

    #[tokio::test]
    async fn git_manager_fetch_uses_prune_submodules_and_noninteractive_environment() {
        let runner = Arc::new(RecordingGitRunner {
            outputs: HashMap::from([("GitManager.fetch".into(), process_output("fetched\n"))]),
            requests: Mutex::new(Vec::new()),
        });
        let repository = GitRepository::with_runner_for_test(runner.clone());

        repository
            .git_manager_fetch(Path::new("/repo"), "origin", &CancellationToken::new())
            .await
            .expect("fetch succeeds");

        let request = &runner.requests()[0];
        assert_network_transfer(
            request,
            &[
                "fetch",
                "--prune",
                "--recurse-submodules=on-demand",
                "origin",
            ],
        );
        for (key, expected) in [
            ("GIT_TERMINAL_PROMPT", "0"),
            ("GIT_ASKPASS", ""),
            ("SSH_ASKPASS_REQUIRE", "never"),
        ] {
            assert!(
                request
                    .env
                    .iter()
                    .any(|(actual_key, value)| actual_key == key && value == expected)
            );
        }
    }

    #[tokio::test]
    async fn git_manager_pull_adds_ff_only_when_both_pull_settings_are_unset() {
        let runner = Arc::new(RecordingGitRunner {
            outputs: HashMap::from([
                ("GitManager.pull.pullFf".into(), process_result(1, "", "")),
                (
                    "GitManager.pull.pullRebase".into(),
                    process_result(1, "", ""),
                ),
                ("GitManager.pull".into(), process_output("pulled\n")),
            ]),
            requests: Mutex::new(Vec::new()),
        });
        let repository = GitRepository::with_runner_for_test(runner.clone());

        let outputs = repository
            .git_manager_pull(Path::new("/repo"), "origin", &CancellationToken::new())
            .await
            .expect("pull succeeds");

        assert_eq!(outputs.len(), 3);
        let requests = runner.requests();
        assert_default_budget(&requests[0]);
        assert_default_budget(&requests[1]);
        assert_network_transfer(
            &requests[2],
            &[
                "-c",
                "rebase.backend=merge",
                "pull",
                "--ff",
                "--recurse-submodules",
                "origin",
            ],
        );
    }

    #[tokio::test]
    async fn git_manager_push_modes_build_explicit_refspecs_and_lease_only_force() {
        let runner = Arc::new(RecordingGitRunner {
            outputs: HashMap::from([("GitManager.push".into(), process_output("pushed\n"))]),
            requests: Mutex::new(Vec::new()),
        });
        let repository = GitRepository::with_runner_for_test(runner.clone());

        repository
            .git_manager_push(
                Path::new("/repo"),
                "origin",
                "topic",
                Some("review/topic"),
                true,
                true,
                false,
                &CancellationToken::new(),
            )
            .await
            .expect("push succeeds");
        repository
            .git_manager_push(
                Path::new("/repo"),
                "origin",
                "topic",
                None,
                false,
                false,
                true,
                &CancellationToken::new(),
            )
            .await
            .expect("push with tags succeeds");

        let requests = runner.requests();
        let args = &requests[0].args;
        assert_network_transfer(
            &requests[0],
            &[
                "push",
                "origin",
                "topic:review/topic",
                "--set-upstream",
                "--force-with-lease",
            ],
        );
        assert!(!args.iter().any(|argument| argument == "--force"));
        // Pushing tags travels with the branch atomically.
        assert_network_transfer(
            &requests[1],
            &["push", "origin", "topic", "--tags", "--atomic"],
        );
    }

    #[tokio::test]
    async fn git_manager_merge_commands_match_the_command_contract() {
        let runner = Arc::new(RecordingGitRunner {
            outputs: HashMap::from([
                ("GitManager.merge".into(), process_output("")),
                ("GitManager.squashMerge.commit".into(), process_output("")),
            ]),
            requests: Mutex::new(Vec::new()),
        });
        let repository = GitRepository::with_runner_for_test(runner.clone());
        let cwd = Path::new("/repo");
        let cancellation = CancellationToken::new();

        for (no_verify, squash) in [(false, false), (true, false), (false, true), (true, true)] {
            repository
                .git_manager_merge(cwd, "topic", no_verify, squash, &cancellation)
                .await
                .expect("merge command");
        }
        for no_verify in [false, true] {
            repository
                .git_manager_squash_merge_commit(cwd, no_verify, &cancellation)
                .await
                .expect("squash merge commit command");
        }

        let args = runner
            .requests()
            .into_iter()
            .map(|request| request.args)
            .collect::<Vec<_>>();
        let expected = [
            vec![
                "merge",
                "--no-ff",
                "--no-squash",
                "--commit",
                "--no-edit",
                "topic",
            ],
            vec![
                "merge",
                "--no-ff",
                "--no-squash",
                "--commit",
                "--no-edit",
                "--no-verify",
                "topic",
            ],
            vec!["merge", "--squash", "--ff", "--no-commit", "topic"],
            vec![
                "merge",
                "--squash",
                "--ff",
                "--no-commit",
                "--no-verify",
                "topic",
            ],
            vec!["commit", "--no-edit"],
            vec!["commit", "--no-edit", "--no-verify"],
        ]
        .into_iter()
        .map(|args| args.into_iter().map(OsString::from).collect::<Vec<_>>())
        .collect::<Vec<_>>();
        assert_eq!(args, expected);
    }

    #[tokio::test]
    async fn git_manager_history_rewrite_commands_match_the_command_contract() {
        let runner = Arc::new(RecordingGitRunner {
            outputs: HashMap::from([
                ("GitManager.rebase".into(), process_output("")),
                ("GitManager.cherryPick".into(), process_output("")),
                ("GitManager.revert".into(), process_output("")),
                ("GitManager.reset".into(), process_output("")),
            ]),
            requests: Mutex::new(Vec::new()),
        });
        let repository = GitRepository::with_runner_for_test(runner.clone());
        let cwd = Path::new("/repo");
        let cancellation = CancellationToken::new();

        repository
            .git_manager_rebase(cwd, "main", "topic", &cancellation)
            .await
            .expect("rebase command");
        repository
            .git_manager_cherry_pick(cwd, &["aaaaaaa".into(), "bbbbbbb".into()], &cancellation)
            .await
            .expect("cherry-pick command");
        repository
            .git_manager_revert(cwd, "ccccccc", false, &cancellation)
            .await
            .expect("non-merge revert command");
        repository
            .git_manager_revert(cwd, "ddddddd", true, &cancellation)
            .await
            .expect("merge revert command");
        for mode in ["hard", "soft", "mixed"] {
            repository
                .git_manager_reset(cwd, "eeeeeee", mode, &cancellation)
                .await
                .expect("reset command");
        }

        let args = runner
            .requests()
            .into_iter()
            .map(|request| request.args)
            .collect::<Vec<_>>();
        let expected = [
            vec!["-c", "rebase.backend=merge", "rebase", "main", "topic"],
            vec![
                "cherry-pick",
                "--empty=keep",
                "-m",
                "1",
                "aaaaaaa",
                "bbbbbbb",
            ],
            vec!["revert", "ccccccc"],
            vec!["revert", "-m", "1", "ddddddd"],
            vec!["reset", "--hard", "eeeeeee"],
            vec!["reset", "--soft", "eeeeeee"],
            vec!["reset", "--mixed", "eeeeeee"],
        ]
        .into_iter()
        .map(|args| args.into_iter().map(OsString::from).collect::<Vec<_>>())
        .collect::<Vec<_>>();
        assert_eq!(args, expected);
        assert!(runner.requests().iter().all(|request| {
            request
                .env
                .iter()
                .any(|(key, value)| key == "GIT_TERMINAL_PROMPT" && value == "0")
                && !request
                    .env
                    .iter()
                    .any(|(key, _)| key == "GIT_OPTIONAL_LOCKS")
        }));
    }

    #[tokio::test]
    async fn interactive_rebase_uses_temporary_todo_and_editor_files_then_cleans_them() {
        let runner = Arc::new(RecordingGitRunner {
            outputs: HashMap::from([
                ("GitManager.squash".into(), process_output("")),
                ("GitManager.reorder".into(), process_output("")),
            ]),
            requests: Mutex::new(Vec::new()),
        });
        let repository = GitRepository::with_runner_for_test(runner.clone());
        let cancellation = CancellationToken::new();

        repository
            .git_manager_interactive_rebase(
                Path::new("/repo"),
                "pick aaaaaaa\nsquash bbbbbbb\n",
                Some("combined message\n"),
                Some("aaaaaaa^"),
                &cancellation,
            )
            .await
            .expect("squash rebase command");
        repository
            .git_manager_interactive_rebase(
                Path::new("/repo"),
                "pick bbbbbbb\npick aaaaaaa\n",
                None,
                None,
                &cancellation,
            )
            .await
            .expect("root reorder command");

        let requests = runner.requests();
        assert_eq!(requests.len(), 2);
        for (index, request) in requests.iter().enumerate() {
            assert_eq!(request.args[0], "-c");
            assert_eq!(request.args[1], "rebase.backend=merge");
            assert_eq!(request.args[2], "-c");
            let sequence_editor = request.args[3].to_string_lossy();
            let todo_path = editor_file_path(&sequence_editor, "sequence.editor=")
                .expect("sequence editor file path");
            assert!(!todo_path.exists(), "todo file is removed after execution");
            assert_eq!(request.args[4], "rebase");
            assert_eq!(request.args[5], "-i");
            assert_eq!(
                request.args[6],
                if index == 0 { "aaaaaaa^" } else { "--root" }
            );
            assert!(request.stdin.is_none());
        }
        let squash_editor = requests[0]
            .env
            .iter()
            .find(|(key, _)| key == "GIT_EDITOR")
            .map(|(_, value)| value.to_string_lossy().into_owned())
            .expect("squash editor");
        let message_path =
            editor_file_path(&squash_editor, "").expect("squash message editor file path");
        assert!(
            !message_path.exists(),
            "message file is removed after execution"
        );
        assert!(
            requests[1]
                .env
                .iter()
                .any(|(key, value)| key == "GIT_EDITOR" && value == ":")
        );
    }

    fn editor_file_path(editor: &str, prefix: &str) -> Option<PathBuf> {
        editor
            .strip_prefix(prefix)?
            .strip_prefix("cat \"")?
            .strip_suffix("\" >")
            .map(PathBuf::from)
    }

    #[tokio::test]
    async fn recovery_commands_set_the_editor_and_preserve_command_order() {
        let runner = Arc::new(RecordingGitRunner {
            outputs: HashMap::from([
                ("GitManager.continue".into(), process_output("")),
                ("GitManager.abort".into(), process_output("")),
                (
                    "GitManager.resolveConflict.checkout".into(),
                    process_output(""),
                ),
                (
                    "GitManager.resolveConflict.stage".into(),
                    process_output(""),
                ),
            ]),
            requests: Mutex::new(Vec::new()),
        });
        let repository = GitRepository::with_runner_for_test(runner.clone());
        let cwd = Path::new("/repo");
        let cancellation = CancellationToken::new();

        for (operation, skip) in [
            ("rebase", false),
            ("rebase", true),
            ("cherry-pick", false),
            ("revert", false),
            ("merge", false),
        ] {
            repository
                .git_manager_continue(cwd, operation, skip, &cancellation)
                .await
                .expect("continue command");
        }
        for operation in ["rebase", "cherry-pick", "revert", "merge"] {
            repository
                .git_manager_abort(cwd, operation, &cancellation)
                .await
                .expect("abort command");
        }
        repository
            .git_manager_resolve_conflict(
                cwd,
                "nested/file.txt",
                crate::git::manager::conflicts::ConflictSide::Ours,
                false,
                &cancellation,
            )
            .await
            .expect("ours resolution");
        repository
            .git_manager_resolve_conflict(
                cwd,
                "deleted.txt",
                crate::git::manager::conflicts::ConflictSide::Theirs,
                true,
                &cancellation,
            )
            .await
            .expect("deleted-side resolution");

        let requests = runner.requests();
        let args = requests
            .iter()
            .map(|request| {
                request
                    .args
                    .iter()
                    .map(|argument| argument.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            [
                vec!["-c", "rebase.backend=merge", "rebase", "--continue"],
                vec!["-c", "rebase.backend=merge", "rebase", "--skip"],
                vec!["cherry-pick", "--continue"],
                vec!["revert", "--continue"],
                vec!["commit", "--no-edit", "--cleanup=strip"],
                vec!["-c", "rebase.backend=merge", "rebase", "--abort"],
                vec!["cherry-pick", "--abort"],
                vec!["revert", "--abort"],
                vec!["merge", "--abort"],
                vec!["checkout", "--ours", "--", "nested/file.txt"],
                vec!["add", "--", "nested/file.txt"],
                vec!["checkout", "--theirs", "--", "deleted.txt"],
                vec!["rm", "--", "deleted.txt"],
            ]
        );
        assert!(requests[..5].iter().all(|request| {
            request
                .env
                .iter()
                .any(|(key, value)| key == "GIT_EDITOR" && value == ":")
        }));
    }

    #[tokio::test]
    async fn conflict_state_uses_porcelain_markers_numstat_and_binary_attributes() {
        let runner = Arc::new(RecordingGitRunner {
            outputs: HashMap::from([
                (
                    "GitManager.conflicts.status".into(),
                    process_output(
                        "u UU N... 100644 100644 100644 100644 aaaaaaa bbbbbbb ccccccc text.txt\0\
                         u AA N... 100644 100644 100644 100644 aaaaaaa bbbbbbb ccccccc image.dat\0",
                    ),
                ),
                (
                    "GitManager.conflicts.markers".into(),
                    process_result(
                        2,
                        "",
                        "text.txt:1: leftover conflict marker\n\
                         text.txt:3: leftover conflict marker\n\
                         text.txt:5: leftover conflict marker\n",
                    ),
                ),
                (
                    "GitManager.conflicts.numstat".into(),
                    process_output("-\t-\timage.dat\0"),
                ),
                (
                    "GitManager.conflicts.attributes".into(),
                    process_output("image.dat\0merge\0binary\0"),
                ),
            ]),
            requests: Mutex::new(Vec::new()),
        });
        let repository = GitRepository::with_runner_for_test(runner.clone());

        let states = repository
            .git_manager_conflict_states(Path::new("/repo"), &CancellationToken::new())
            .await
            .expect("conflict state");

        assert_eq!(states.len(), 2);
        assert_eq!(states[0].path, "text.txt");
        assert_eq!(states[0].marker_count, 3);
        assert_eq!(
            states[1].kind,
            crate::git::manager::conflicts::ConflictKind::Binary
        );
        let requests = runner.requests();
        let args = requests
            .iter()
            .map(|request| request.args.clone())
            .collect::<Vec<_>>();
        let expected = [
            vec![
                "-c",
                "core.quotePath=false",
                "status",
                "--porcelain=2",
                "-z",
                "--untracked-files=all",
            ],
            vec!["diff", "--check"],
            vec!["diff", "--numstat", "-z"],
            vec!["check-attr", "--stdin", "-z", "merge"],
        ]
        .into_iter()
        .map(|args| args.into_iter().map(OsString::from).collect::<Vec<_>>())
        .collect::<Vec<_>>();
        assert_eq!(args, expected);
        assert_eq!(
            requests[3].stdin.as_deref(),
            Some(b"text.txt\0image.dat\0".as_slice())
        );
        assert!(requests.iter().all(|request| {
            !request
                .env
                .iter()
                .any(|(key, _)| key == "GIT_OPTIONAL_LOCKS")
        }));
    }

    #[tokio::test]
    async fn rewrite_inspection_reads_history_parents_and_staged_emptiness() {
        let runner = Arc::new(RecordingGitRunner {
            outputs: HashMap::from([
                (
                    "GitManager.rewrite.logOrder".into(),
                    process_output("bbbbbbb\naaaaaaa\n"),
                ),
                (
                    "GitManager.rewrite.parent".into(),
                    process_output("aaaaaaa 0000000\n"),
                ),
                (
                    "GitManager.revert.parents".into(),
                    process_output("1111111 2222222\n"),
                ),
                (
                    "GitManager.continue.staged".into(),
                    process_result(1, "", ""),
                ),
            ]),
            requests: Mutex::new(Vec::new()),
        });
        let repository = GitRepository::with_runner_for_test(runner.clone());
        let cancellation = CancellationToken::new();

        assert_eq!(
            repository
                .git_manager_rewrite_log_order(Path::new("/repo"), &cancellation)
                .await
                .expect("history order"),
            ["bbbbbbb", "aaaaaaa"]
        );
        assert_eq!(
            repository
                .git_manager_commit_parent(Path::new("/repo"), "aaaaaaa", &cancellation)
                .await
                .expect("parent probe"),
            Some("0000000".to_owned())
        );
        assert!(
            repository
                .git_manager_is_merge_commit(Path::new("/repo"), "ccccccc", &cancellation)
                .await
                .expect("merge-parent probe")
        );
        assert!(
            !repository
                .git_manager_index_is_empty(Path::new("/repo"), &cancellation)
                .await
                .expect("staged probe")
        );

        let args = runner
            .requests()
            .into_iter()
            .map(|request| request.args)
            .collect::<Vec<_>>();
        let expected = [
            vec!["rev-list", "--first-parent", "HEAD", "--"],
            vec!["rev-list", "--parents", "-n", "1", "aaaaaaa", "--"],
            vec!["show", "-s", "--format=%P", "ccccccc", "--"],
            vec!["diff", "--cached", "--quiet", "--"],
        ]
        .into_iter()
        .map(|args| args.into_iter().map(OsString::from).collect::<Vec<_>>())
        .collect::<Vec<_>>();
        assert_eq!(args, expected);
    }

    #[tokio::test]
    async fn git_manager_commit_uses_stdin_and_preserves_the_visible_index() {
        let runner = Arc::new(RecordingGitRunner {
            outputs: HashMap::from([
                (
                    "GitManager.commit.status".into(),
                    process_output("1 M. N... 100644 100644 100644 aaaaaaa bbbbbbb tracked.txt\0"),
                ),
                ("GitManager.commit".into(), process_output("")),
                (
                    "GitManager.commit.sha".into(),
                    process_output("0123456789012345678901234567890123456789\n"),
                ),
            ]),
            requests: Mutex::new(Vec::new()),
        });
        let repository = GitRepository::with_runner_for_test(runner.clone());
        let message = b"summary\n\ndescription\n";

        let result = repository
            .commit_with_options(
                Path::new("/repo"),
                &[
                    "commit".into(),
                    "--no-verify".into(),
                    "-F".into(),
                    "-".into(),
                ],
                message,
                false,
                &CancellationToken::new(),
            )
            .await
            .expect("commit succeeds");

        assert_eq!(
            result,
            GitManagerCommitOutcome {
                sha: Some("0123456789012345678901234567890123456789".into()),
                empty: false,
            }
        );
        let requests = runner.requests();
        assert_eq!(requests[1].stdin.as_deref(), Some(message.as_slice()));
        assert_eq!(
            requests[1].args,
            ["commit", "--no-verify", "-F", "-"]
                .into_iter()
                .map(OsString::from)
                .collect::<Vec<_>>()
        );
        assert!(requests.iter().all(|request| {
            !request
                .args
                .iter()
                .any(|arg| arg == "reset" || arg == "add")
        }));
    }

    #[tokio::test]
    async fn git_manager_partial_apply_modes_keep_index_and_worktree_targets_distinct() {
        let runner = Arc::new(RecordingGitRunner {
            outputs: HashMap::from([
                ("GitManager.stagePartial.apply".into(), process_output("")),
                ("GitManager.unstagePartial.apply".into(), process_output("")),
                ("GitManager.discardPartial.apply".into(), process_output("")),
            ]),
            requests: Mutex::new(Vec::new()),
        });
        let repository = GitRepository::with_runner_for_test(runner.clone());
        let patch = b"diff payload\n".to_vec();
        let cancellation = CancellationToken::new();

        for (operation, cached, reverse) in [
            ("GitManager.stagePartial.apply", true, false),
            ("GitManager.unstagePartial.apply", true, true),
            ("GitManager.discardPartial.apply", false, true),
        ] {
            repository
                .git_manager_apply_partial_patch(
                    operation,
                    Path::new("/repo"),
                    patch.clone(),
                    cached,
                    reverse,
                    &cancellation,
                )
                .await
                .expect("partial apply");
        }

        let requests = runner.requests();
        let args = requests
            .iter()
            .map(|request| {
                request
                    .args
                    .iter()
                    .map(|argument| argument.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            [
                vec![
                    "apply",
                    "--cached",
                    "--unidiff-zero",
                    "--whitespace=nowarn",
                    "-"
                ],
                vec![
                    "apply",
                    "--cached",
                    "--reverse",
                    "--unidiff-zero",
                    "--whitespace=nowarn",
                    "-"
                ],
                vec![
                    "apply",
                    "--reverse",
                    "--unidiff-zero",
                    "--whitespace=nowarn",
                    "-"
                ],
            ]
        );
        assert!(
            requests
                .iter()
                .all(|request| request.stdin.as_deref() == Some(patch.as_slice()))
        );
    }

    #[tokio::test]
    async fn git_manager_undo_uses_a_mixed_reset_to_the_first_parent() {
        let runner = Arc::new(RecordingGitRunner {
            outputs: HashMap::from([
                (
                    "GitManager.undoCommit.head".into(),
                    process_output(
                        "2222222222222222222222222222222222222222\0\
                         1111111111111111111111111111111111111111\0\
                         subject\n\nbody\n",
                    ),
                ),
                ("GitManager.undoCommit.reset".into(), process_output("")),
            ]),
            requests: Mutex::new(Vec::new()),
        });
        let repository = GitRepository::with_runner_for_test(runner.clone());

        let undone = repository
            .undo_head_commit(Path::new("/repo"), &CancellationToken::new())
            .await
            .expect("undo succeeds");

        assert_eq!(undone.sha, "2222222222222222222222222222222222222222");
        assert_eq!(undone.message, "subject\n\nbody\n");
        assert_eq!(
            runner.requests()[1].args,
            [
                "reset",
                "--mixed",
                "1111111111111111111111111111111111111111"
            ]
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn git_manager_discard_resolves_tracked_paths_without_mutating() {
        let runner = Arc::new(RecordingGitRunner {
            outputs: HashMap::from([(
                "GitManager.discard.tracked".into(),
                process_output("dir/tracked.txt\0root.txt\0"),
            )]),
            requests: Mutex::new(Vec::new()),
        });
        let repository = GitRepository::with_runner_for_test(runner.clone());

        let tracked = repository
            .git_manager_tracked_paths(
                Path::new("/repo"),
                &["dir".into(), "root.txt".into()],
                &CancellationToken::new(),
            )
            .await
            .expect("tracked paths resolve");

        assert_eq!(tracked, ["dir/tracked.txt", "root.txt"]);
        assert_eq!(
            runner.requests()[0].args,
            [
                "-c",
                "core.quotePath=false",
                "ls-files",
                "-z",
                "--",
                "dir",
                "root.txt"
            ]
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn git_manager_undo_restores_deleted_files_from_an_initial_commit() {
        fn git(cwd: &Path, args: &[&str]) -> std::process::Output {
            Command::new("git")
                .args(args)
                .current_dir(cwd)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_AUTHOR_NAME", "Git Manager Test")
                .env("GIT_AUTHOR_EMAIL", "git-manager@example.test")
                .env("GIT_COMMITTER_NAME", "Git Manager Test")
                .env("GIT_COMMITTER_EMAIL", "git-manager@example.test")
                .output()
                .expect("git fixture starts")
        }

        let fixture = tempfile::tempdir().expect("temporary repository");
        assert!(
            git(fixture.path(), &["init", "-q", "-b", "main"])
                .status
                .success()
        );
        assert!(
            git(fixture.path(), &["config", "core.autocrlf", "false"])
                .status
                .success()
        );
        fs::write(fixture.path().join("tracked.txt"), "initial\n").expect("tracked fixture");
        assert!(
            git(fixture.path(), &["add", "tracked.txt"])
                .status
                .success()
        );
        assert!(
            git(fixture.path(), &["commit", "-q", "-m", "Initial subject"])
                .status
                .success()
        );
        fs::remove_file(fixture.path().join("tracked.txt")).expect("delete tracked fixture");

        let undone = GitRepository::default()
            .undo_head_commit(fixture.path(), &CancellationToken::new())
            .await
            .expect("initial commit undo succeeds");

        assert!(undone.parents.is_empty());
        assert!(undone.message.starts_with("Initial subject"));
        assert_eq!(
            fs::read_to_string(fixture.path().join("tracked.txt")).expect("restored file"),
            "initial\n"
        );
        assert!(
            !git(fixture.path(), &["rev-parse", "--verify", "HEAD"])
                .status
                .success()
        );
        assert_eq!(
            String::from_utf8(git(fixture.path(), &["status", "--porcelain=v1"]).stdout)
                .expect("UTF-8 status"),
            "?? tracked.txt\n"
        );
    }

    #[tokio::test]
    async fn selected_remote_push_ignores_existing_upstream_and_terminates_options() {
        let runner = Arc::new(RecordingGitRunner {
            outputs: HashMap::from([
                (
                    "GitVcsDriver.currentRef".into(),
                    process_output("feature/test\n"),
                ),
                (
                    "GitVcsDriver.pushCurrentBranchToRemote".into(),
                    process_output(""),
                ),
            ]),
            requests: Mutex::new(Vec::new()),
        });
        let repository = GitRepository::with_runner_for_test(runner.clone());

        assert_eq!(
            repository
                .push_current_branch_to_remote(
                    Path::new("/repo"),
                    "upstream",
                    &CancellationToken::new(),
                )
                .await
                .expect("selected remote push succeeds"),
            "feature/test"
        );
        let requests = runner.requests();
        assert_eq!(requests.len(), 2);
        assert_default_budget(&requests[0]);
        // Publishing pushes this way and offers no Cancel, so the push stays bounded.
        assert_bounded_transfer(
            &requests[1],
            &["push", "--set-upstream", "--", "upstream", "feature/test"],
        );
    }

    #[tokio::test]
    async fn current_branch_pull_and_push_run_only_the_transfer_on_the_bounded_budget() {
        let runner = Arc::new(RecordingGitRunner {
            outputs: HashMap::from([
                ("GitVcsDriver.currentRef".into(), process_output("main\n")),
                (
                    "GitVcsDriver.pullCurrentBranch.upstream".into(),
                    process_output("origin/main\n"),
                ),
                (
                    "GitVcsDriver.pullCurrentBranch.before".into(),
                    process_output("1111111111111111111111111111111111111111\n"),
                ),
                ("GitVcsDriver.pullCurrentBranch".into(), process_output("")),
                (
                    "GitVcsDriver.pullCurrentBranch.after".into(),
                    process_output("2222222222222222222222222222222222222222\n"),
                ),
                (
                    "GitVcsDriver.pushCurrentBranch.upstream".into(),
                    process_result(128, "", "fatal: no upstream configured"),
                ),
                ("GitVcsDriver.pushCurrentBranch".into(), process_output("")),
            ]),
            requests: Mutex::new(Vec::new()),
        });
        let repository = GitRepository::with_runner_for_test(runner.clone());
        let cancellation = CancellationToken::new();

        repository
            .pull_current_branch(Path::new("/repo"), &cancellation)
            .await
            .expect("pull succeeds");
        repository
            .push_current_branch(Path::new("/repo"), &cancellation)
            .await
            .expect("push succeeds");

        let requests = runner.requests();
        let mut transfers = 0;
        for request in &requests {
            match request.operation.as_str() {
                "GitVcsDriver.pullCurrentBranch" => {
                    transfers += 1;
                    assert_bounded_transfer(request, &["pull", "--ff-only"]);
                }
                "GitVcsDriver.pushCurrentBranch" => {
                    transfers += 1;
                    assert_bounded_transfer(request, &["push", "--set-upstream", "origin", "main"]);
                }
                _ => assert_default_budget(request),
            }
        }
        assert_eq!(transfers, 2);
    }

    #[test]
    fn stalled_transfer_output_maps_to_the_stall_message() {
        let error = super::git_error(
            "GitVcsDriver.clone",
            Path::new("/parent"),
            9,
            ProcessError::NonZeroExit {
                operation: "GitVcsDriver.clone".to_owned(),
                exit_code: 128,
                stdout_length: 0,
                stderr_length: 120,
                stdout: "".into(),
                stderr: "Cloning into 'repo'...\nerror: RPC failed; curl 28 Operation too slow. Less than 1000 bytes/sec transferred the last 60 seconds\nfatal: expected flush after ref listing\n".into(),
            },
        );

        assert_eq!(
            error.detail.as_ref(),
            "The transfer stalled (under 1 KB/s for 60 seconds). Check the connection to the remote and try again."
        );
        assert_eq!(error.detail.as_ref(), super::NETWORK_TRANSFER_STALLED);
        assert!(super::reports_stalled_transfer(&error.detail));
        let other = super::git_error(
            "GitVcsDriver.clone",
            Path::new("/parent"),
            9,
            ProcessError::NonZeroExit {
                operation: "GitVcsDriver.clone".to_owned(),
                exit_code: 128,
                stdout_length: 0,
                stderr_length: 40,
                stdout: "".into(),
                stderr: "fatal: repository 'x' not found\n".into(),
            },
        );
        assert_eq!(other.detail.as_ref(), "fatal: repository 'x' not found");
    }

    #[tokio::test]
    async fn full_status_reuses_one_porcelain_snapshot() {
        let runner = Arc::new(RecordingGitRunner::dirty_tracked_fixture());
        let repository = GitRepository::with_runner_for_test(runner.clone());

        let result = repository
            .status(Path::new("/repo"), &CancellationToken::new())
            .await
            .expect("status fixture succeeds");

        assert_eq!(result.local.ref_name.as_deref(), Some("feature/test"));
        assert_eq!(result.remote.ahead_count, 2);
        assert_eq!(result.remote.behind_count, 1);
        assert_eq!(runner.count_arg("status"), 1);
        assert_eq!(runner.count_arg("--numstat"), 2);
    }

    #[tokio::test]
    async fn clean_status_starts_no_numstat_process() {
        let runner = Arc::new(RecordingGitRunner::clean_fixture());
        let repository = GitRepository::with_runner_for_test(runner.clone());

        repository
            .status(Path::new("/repo"), &CancellationToken::new())
            .await
            .expect("status fixture succeeds");

        assert_eq!(runner.count_arg("--numstat"), 0);
    }

    #[tokio::test]
    async fn summary_repository_unavailable_plain_folder_is_absent() {
        let Some(root) =
            plain_folder_fixture("summary_repository_unavailable_plain_folder_is_absent")
        else {
            return;
        };
        assert_summary_repository_reason(&GitRepository::default(), root.path(), Some("absent"))
            .await;
    }

    async fn assert_summary_repository_reason(
        repository: &GitRepository,
        cwd: &Path,
        reason: Option<&str>,
    ) {
        let summary = repository
            .summary_status(cwd, &CancellationToken::new())
            .await
            .expect("unavailable summary is not an error");
        assert!(!summary.is_repo);
        assert!(summary.ref_name.is_none());
        assert!(summary.detached_head.is_none());
        assert!(summary.source_control_provider.is_none());
        assert!(summary.pr.is_none());
        assert!(!summary.has_working_tree_changes);
        assert!(!summary.stale);
        assert!(summary.observed_at.contains('T'));
        let encoded = serde_json::to_value(summary).expect("summary JSON");
        assert_eq!(
            encoded.get("repositoryUnavailableReason"),
            reason.map(serde_json::Value::from).as_ref()
        );
    }

    #[tokio::test]
    async fn summary_repository_unavailable_damaged_head_is_unreadable() {
        for contents in ["not a ref", ""] {
            let root = tempfile::tempdir().expect("damaged repository");
            repository_fixture_git(root.path(), &["init", "-q", "-b", "main"]);
            fs::write(root.path().join(".git/HEAD"), contents).expect("damage HEAD");
            assert_summary_repository_reason(
                &GitRepository::default(),
                root.path(),
                Some("unreadable"),
            )
            .await;
        }
    }

    #[tokio::test]
    async fn summary_repository_unavailable_malformed_config_is_unreadable_not_an_error() {
        let root = tempfile::tempdir().expect("damaged repository");
        repository_fixture_git(root.path(), &["init", "-q", "-b", "main"]);
        fs::write(root.path().join(".git/config"), "[invalid").expect("damage config");
        assert_summary_repository_reason(
            &GitRepository::default(),
            root.path(),
            Some("unreadable"),
        )
        .await;
    }

    #[tokio::test]
    async fn summary_repository_unavailable_real_git_ownership_refusal_is_untrusted() {
        let root = tempfile::tempdir().expect("untrusted repository");
        repository_fixture_git(root.path(), &["init", "-q", "-b", "main"]);
        assert_summary_repository_reason(
            &GitRepository::with_runner_for_test(Arc::new(DifferentOwnerGitRunner)),
            root.path(),
            Some("untrusted"),
        )
        .await;
    }

    #[tokio::test]
    async fn summary_repository_unavailable_bare_repository_omits_reason() {
        let root = tempfile::tempdir().expect("bare repository");
        repository_fixture_git(root.path(), &["init", "-q", "--bare"]);
        assert_summary_repository_reason(&GitRepository::default(), root.path(), None).await;
    }

    #[tokio::test]
    async fn summary_repository_unavailable_discovery_overrides_omit_reason() {
        let Some(root) =
            plain_folder_fixture("summary_repository_unavailable_discovery_overrides_omit_reason")
        else {
            return;
        };
        for environment in [
            (|name: &str| (name == "GIT_DIR").then(OsString::new)) as fn(&str) -> Option<OsString>,
            |name: &str| (name == "GIT_CEILING_DIRECTORIES").then(|| OsString::from("/")),
        ] {
            let repository = GitRepository {
                discovery_environment: environment,
                ..GitRepository::default()
            };
            assert_summary_repository_reason(&repository, root.path(), None).await;
        }
    }

    #[tokio::test]
    async fn summary_repository_unavailable_worktree_probe_preserves_the_status_error() {
        let root = tempfile::tempdir().expect("repository with corrupt index");
        repository_fixture_git(root.path(), &["init", "-q", "-b", "main"]);
        fs::write(root.path().join(".git/index"), "invalid index").expect("damage index");
        let repository = GitRepository::default();
        assert!(
            repository
                .is_repository(root.path(), &CancellationToken::new())
                .await
                .unwrap()
        );
        let error = repository
            .summary_status(root.path(), &CancellationToken::new())
            .await
            .expect_err("a readable worktree keeps the error");
        assert_eq!(
            error.operation.as_ref(),
            "GitVcsDriver.summaryStatus.status"
        );
        assert_eq!(error.detail.as_ref(), "Git status summary failed.");
    }

    #[tokio::test]
    async fn summary_status_uses_one_porcelain_read_without_numstat_or_file_storage() {
        let runner = Arc::new(RecordingGitRunner {
            outputs: HashMap::from([
                (
                    "GitVcsDriver.summaryStatus.status".into(),
                    process_output(
                        "# branch.oid 0123456789abcdef0123456789abcdef01234567\n# branch.head feature/test\n1 .M N... 100644 100644 100644 0000000000000000000000000000000000000000 0000000000000000000000000000000000000000 tracked.txt\n",
                    ),
                ),
                (
                    "GitVcsDriver.remoteProvider".into(),
                    process_output("https://github.com/acme/repo.git\n"),
                ),
            ]),
            requests: Mutex::new(Vec::new()),
        });
        let repository = GitRepository {
            discovery_environment: |_| panic!("healthy summary must not classify discovery"),
            ..GitRepository::with_runner_for_test(runner.clone())
        };

        let summary = repository
            .summary_status(Path::new("/repo"), &CancellationToken::new())
            .await
            .expect("summary fixture succeeds");

        assert!(summary.is_repo);
        assert!(
            serde_json::to_value(&summary)
                .expect("summary JSON")
                .get("repositoryUnavailableReason")
                .is_none()
        );
        assert_eq!(summary.ref_name.as_deref(), Some("feature/test"));
        assert_eq!(summary.detached_head, None);
        assert!(summary.has_working_tree_changes);
        assert_eq!(
            summary
                .source_control_provider
                .as_ref()
                .map(|provider| provider.kind),
            Some(crate::git::ProviderKind::Github)
        );
        assert!(summary.pr.is_none());
        assert!(summary.observed_at.contains('T'));
        assert!(!summary.stale);
        assert_eq!(
            runner.operation_names(),
            [
                "GitVcsDriver.summaryStatus.status",
                "GitVcsDriver.remoteProvider"
            ]
        );
        assert_eq!(runner.count_arg("status"), 1);
        assert_eq!(runner.count_arg("--numstat"), 0);
        let requests = runner.requests();
        assert_eq!(
            requests[0].args,
            [
                "-c",
                "core.quotePath=false",
                "status",
                "--porcelain=2",
                "--branch",
                "--untracked-files=normal",
            ]
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
        );
        assert_eq!(requests[0].max_output_bytes, 64 * 1024);
    }

    #[tokio::test]
    async fn local_branch_names_uses_one_bounded_read_only_query() {
        let runner = Arc::new(RecordingGitRunner {
            outputs: HashMap::from([(
                "GitVcsDriver.localBranchNames".into(),
                process_output("feature/test\nmain\nrelease/2026\n"),
            )]),
            requests: Mutex::new(Vec::new()),
        });
        let repository = GitRepository::with_runner_for_test(runner.clone());

        let branches = repository
            .local_branch_names(Path::new("/repo"), &CancellationToken::new())
            .await
            .expect("local branch query succeeds");

        assert_eq!(branches, ["feature/test", "main", "release/2026"]);
        assert_eq!(runner.operation_names(), ["GitVcsDriver.localBranchNames"]);
        let requests = runner.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].args,
            ["for-each-ref", "--format=%(refname:short)", "refs/heads",]
                .into_iter()
                .map(OsString::from)
                .collect::<Vec<_>>()
        );
        assert_eq!(requests[0].max_output_bytes, 1_000_000);
        assert_eq!(requests[0].output_policy, OutputPolicy::Error);
        assert!(
            requests[0]
                .env
                .iter()
                .any(|(name, value)| name == "GIT_OPTIONAL_LOCKS" && value == "0")
        );
    }

    #[tokio::test]
    async fn local_branch_names_rejects_truncated_output() {
        let mut output = process_output("main\n");
        output.stdout_truncated = true;
        let runner = Arc::new(RecordingGitRunner {
            outputs: HashMap::from([("GitVcsDriver.localBranchNames".into(), output)]),
            requests: Mutex::new(Vec::new()),
        });
        let repository = GitRepository::with_runner_for_test(runner);

        let error = repository
            .local_branch_names(Path::new("/repo"), &CancellationToken::new())
            .await
            .expect_err("truncated branch names are not authoritative");

        assert!(error.detail.contains("truncated"));
    }

    #[tokio::test]
    async fn summary_status_distinguishes_detached_unborn_and_non_repository_states() {
        for (status, expected_ref, expected_detached, expected_repo) in [
            (
                process_result(
                    0,
                    "# branch.oid fedcba9876543210fedcba9876543210fedcba98\n# branch.head (detached)\n",
                    "",
                ),
                None,
                Some("fedcba9876543210fedcba9876543210fedcba98"),
                true,
            ),
            (
                process_result(
                    0,
                    "# branch.oid (initial)\n# branch.head main\n? first.txt\n",
                    "",
                ),
                None,
                None,
                true,
            ),
            (
                process_result(128, "", "fatal: not a git repository"),
                None,
                None,
                false,
            ),
        ] {
            let mut outputs = HashMap::from([("GitVcsDriver.summaryStatus.status".into(), status)]);
            if expected_repo {
                outputs.insert(
                    "GitVcsDriver.remoteProvider".into(),
                    process_result(1, "", ""),
                );
            } else {
                outputs.insert(
                    "GitVcsDriver.detectRepository".into(),
                    process_result(128, "", "fatal: not a git repository"),
                );
            }
            let runner = Arc::new(RecordingGitRunner {
                outputs,
                requests: Mutex::new(Vec::new()),
            });
            let repository = GitRepository::with_runner_for_test(runner.clone());

            let summary = repository
                .summary_status(Path::new("/repo"), &CancellationToken::new())
                .await
                .expect("summary state succeeds");

            assert_eq!(summary.is_repo, expected_repo);
            assert_eq!(summary.ref_name.as_deref(), expected_ref);
            assert_eq!(summary.detached_head.as_deref(), expected_detached);
            assert_eq!(
                summary.has_working_tree_changes,
                expected_repo && expected_ref.is_none() && expected_detached.is_none()
            );
            assert_eq!(runner.count_arg("status"), 1);
            assert_eq!(runner.count_arg("--numstat"), 0);
        }
    }

    /// A self-hosted origin has no provider until an explicit probe records its host;
    /// then status reads classify it with its own base URL, still spawning only Git.
    #[tokio::test]
    async fn status_provider_uses_recorded_hosts_with_their_own_base_url() {
        let runner = Arc::new(RecordingGitRunner {
            outputs: HashMap::from([
                (
                    "GitVcsDriver.summaryStatus.status".into(),
                    process_output(
                        "# branch.oid 0123456789abcdef0123456789abcdef01234567\n# branch.head main\n",
                    ),
                ),
                (
                    "GitVcsDriver.remoteProvider".into(),
                    process_output("https://luna.tripunkt.de/tripunkt/customer-portal.git\n"),
                ),
            ]),
            requests: Mutex::new(Vec::new()),
        });
        let hosts = Arc::new(crate::source_control::ProviderHosts::default());
        let repository =
            GitRepository::with_runner_for_test(runner.clone()).with_provider_hosts(hosts.clone());
        let cancellation = CancellationToken::new();
        let read = || repository.summary_status(Path::new("/repo"), &cancellation);

        assert_eq!(read().await.unwrap().source_control_provider, None);
        hosts.record(
            "luna.tripunkt.de",
            crate::source_control::ProviderKind::Gitlab,
        );
        assert_eq!(
            read().await.unwrap().source_control_provider,
            Some(super::SourceControlProviderInfo {
                kind: crate::git::ProviderKind::Gitlab,
                name: "GitLab".into(),
                base_url: "https://luna.tripunkt.de".into(),
            })
        );
        assert_eq!(runner.count_arg("status"), 2, "only Git ran");
    }

    #[tokio::test]
    async fn summary_provider_absence_is_distinct_from_operational_or_truncated_failure() {
        let status = process_output(
            "# branch.oid 0123456789abcdef0123456789abcdef01234567\n# branch.head main\n",
        );
        for provider in [
            process_result(2, "", "fatal: invalid Git config"),
            ProcessOutput {
                exit_code: 0,
                stdout: "https://github.com/acme/repository.git\n".to_owned(),
                stderr: String::new(),
                stdout_truncated: true,
                stderr_truncated: false,
            },
        ] {
            let runner = Arc::new(RecordingGitRunner {
                outputs: HashMap::from([
                    ("GitVcsDriver.summaryStatus.status".into(), status.clone()),
                    ("GitVcsDriver.remoteProvider".into(), provider),
                ]),
                requests: Mutex::new(Vec::new()),
            });
            let error = GitRepository::with_runner_for_test(runner)
                .summary_status(Path::new("/repo"), &CancellationToken::new())
                .await
                .expect_err("provider operation failure must not become absence");
            assert_eq!(error.operation.as_ref(), "GitVcsDriver.remoteProvider");
        }
    }

    #[tokio::test]
    async fn summary_normal_untracked_mode_reports_a_nested_untracked_directory_dirty() {
        let sandbox = TestSandbox::new("summary-nested-untracked");
        let git = sandbox.executable_on_path("git");
        assert!(
            Command::new(&git)
                .args(["init", "--quiet", "-b", "main"])
                .current_dir(sandbox.root())
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .status()
                .expect("git init starts")
                .success()
        );
        fs::create_dir_all(sandbox.path("nested/deep")).expect("nested untracked directory");
        fs::write(sandbox.path("nested/deep/file.txt"), "untracked\n")
            .expect("nested untracked file");

        let summary = GitRepository::default()
            .summary_status(sandbox.root(), &CancellationToken::new())
            .await
            .expect("nested untracked summary");

        assert!(summary.is_repo);
        assert_eq!(summary.ref_name, None);
        assert!(summary.has_working_tree_changes);
        assert!(summary.pr.is_none());
    }

    #[tokio::test]
    async fn status_records_fused_physical_git_requests() {
        let runner = Arc::new(RecordingGitRunner::status_fixture());
        let repository = GitRepository::with_runner_for_test(runner.clone());

        repository
            .status(Path::new("/repo"), &CancellationToken::new())
            .await
            .expect("status fixture succeeds");

        assert_eq!(runner.operation_names(), EXPECTED_FUSED_OPERATIONS);
        assert!(runner.requests().iter().all(|request| {
            request
                .env
                .iter()
                .any(|(key, value)| key == "GIT_OPTIONAL_LOCKS" && value == "0")
        }));
    }

    #[tokio::test]
    async fn common_dir_resolution_is_one_bounded_read_with_canonical_relative_output() {
        let sandbox = TestSandbox::new("git-common-dir-canonical");
        let cwd = sandbox.path("worktree");
        let common_dir = sandbox.path("common.git");
        fs::create_dir_all(&cwd).expect("worktree fixture");
        fs::create_dir_all(&common_dir).expect("common-dir fixture");
        let runner = Arc::new(RecordingGitRunner {
            outputs: HashMap::from([(
                "GitVcsDriver.resolveCommonDir".to_owned(),
                process_output("../common.git\n"),
            )]),
            requests: Mutex::new(Vec::new()),
        });
        let repository = GitRepository::with_runner_for_test(runner.clone());

        let resolved = repository
            .resolve_common_dir(&cwd, &CancellationToken::new())
            .await
            .expect("common directory resolves");

        assert_eq!(
            resolved,
            fs::canonicalize(&common_dir).expect("canonical common directory")
        );
        let requests = runner.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].args,
            ["rev-parse", "--git-common-dir"]
                .into_iter()
                .map(OsString::from)
                .collect::<Vec<_>>()
        );
        assert!(
            requests[0]
                .env
                .iter()
                .any(|(key, value)| { key == "GIT_OPTIONAL_LOCKS" && value == "0" })
        );
    }

    #[tokio::test]
    async fn watch_root_resolution_is_one_bounded_optional_lock_free_read() {
        let sandbox = TestSandbox::new("git-watch-root-resolution");
        let cwd = sandbox.path("worktree");
        let git_dir = sandbox.path("common.git/worktrees/worktree");
        let common_dir = sandbox.path("common.git");
        fs::create_dir_all(&cwd).expect("worktree fixture");
        fs::create_dir_all(&git_dir).expect("Git directory fixture");
        let runner = Arc::new(RecordingGitRunner {
            outputs: HashMap::from([(
                "GitVcsDriver.resolveWatchRoots".to_owned(),
                process_output(&format!(
                    "{}\n{}\n{}\n",
                    cwd.display(),
                    git_dir.display(),
                    common_dir.display()
                )),
            )]),
            requests: Mutex::new(Vec::new()),
        });
        let repository = GitRepository::with_runner_for_test(runner.clone());

        let roots = repository
            .resolve_watch_roots(&cwd, &CancellationToken::new())
            .await
            .expect("watch roots resolve");

        assert_eq!(roots.worktree_root, fs::canonicalize(&cwd).unwrap());
        assert_eq!(
            roots.git_dir,
            fs::canonicalize(&git_dir).expect("canonical Git directory")
        );
        assert_eq!(roots.common_dir, fs::canonicalize(&common_dir).unwrap());
        let requests = runner.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].args,
            [
                "rev-parse",
                "--path-format=absolute",
                "--show-toplevel",
                "--git-dir",
                "--git-common-dir"
            ]
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
        );
        assert_eq!(requests[0].timeout, super::DEFAULT_TIMEOUT);
        assert_eq!(
            requests[0].max_output_bytes,
            super::WATCH_ROOTS_OUTPUT_LIMIT
        );
        assert_eq!(requests[0].output_policy, OutputPolicy::Error);
        assert!(
            requests[0]
                .env
                .iter()
                .any(|(name, value)| { name == "GIT_OPTIONAL_LOCKS" && value == "0" })
        );
    }

    #[tokio::test]
    async fn watch_root_resolution_rejects_malformed_output_and_keeps_missing_paths_lexical() {
        let sandbox = TestSandbox::new("git-watch-root-shapes");
        let cwd = sandbox.path("worktree");
        fs::create_dir_all(&cwd).expect("worktree fixture");
        let malformed = Arc::new(RecordingGitRunner {
            outputs: HashMap::from([(
                "GitVcsDriver.resolveWatchRoots".to_owned(),
                process_output("/only/worktree\n/only/git-dir\n"),
            )]),
            requests: Mutex::new(Vec::new()),
        });
        assert!(
            GitRepository::with_runner_for_test(malformed)
                .resolve_watch_roots(&cwd, &CancellationToken::new())
                .await
                .is_err()
        );

        let missing = Arc::new(RecordingGitRunner {
            outputs: HashMap::from([(
                "GitVcsDriver.resolveWatchRoots".to_owned(),
                process_output(".\n../missing/admin\n../missing/common\n"),
            )]),
            requests: Mutex::new(Vec::new()),
        });
        let roots = GitRepository::with_runner_for_test(missing)
            .resolve_watch_roots(&cwd, &CancellationToken::new())
            .await
            .expect("missing watch roots retain lexical identities");
        assert_eq!(roots.worktree_root, fs::canonicalize(&cwd).unwrap());
        assert_eq!(
            roots.git_dir,
            normalize_path_lexically(&cwd.join("../missing/admin"))
        );
        assert_eq!(
            roots.common_dir,
            normalize_path_lexically(&cwd.join("../missing/common"))
        );
    }

    #[tokio::test]
    async fn watch_root_resolution_forwards_cancellation_to_its_only_bounded_read() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let runner = Arc::new(FailingWatchRootRunner {
            failure: WatchRootFailure::Cancelled,
            requests: Mutex::new(Vec::new()),
        });

        let error = match GitRepository::with_runner_for_test(runner.clone())
            .resolve_watch_roots(Path::new("/repo"), &cancellation)
            .await
        {
            Err(error) => error,
            Ok(_) => panic!("cancelled watch-root resolution must fail"),
        };

        assert_eq!(error.operation.as_ref(), "GitVcsDriver.resolveWatchRoots");
        assert_eq!(error.detail.as_ref(), "Git command was interrupted.");
        assert_eq!(
            runner
                .requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn watch_root_resolution_propagates_its_output_limit_without_retry() {
        let runner = Arc::new(FailingWatchRootRunner {
            failure: WatchRootFailure::OutputLimit,
            requests: Mutex::new(Vec::new()),
        });

        let error = match GitRepository::with_runner_for_test(runner.clone())
            .resolve_watch_roots(Path::new("/repo"), &CancellationToken::new())
            .await
        {
            Err(error) => error,
            Ok(_) => panic!("oversized watch-root output must fail"),
        };

        assert_eq!(error.operation.as_ref(), "GitVcsDriver.resolveWatchRoots");
        assert_eq!(
            error.detail.as_ref(),
            "Git command output exceeded its limit."
        );
        let requests = runner
            .requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].max_output_bytes,
            super::WATCH_ROOTS_OUTPUT_LIMIT
        );
        assert_eq!(requests[0].output_policy, OutputPolicy::Error);
    }

    #[tokio::test]
    async fn common_dir_resolution_uses_lexical_fallback_when_target_is_missing() {
        let sandbox = TestSandbox::new("git-common-dir-lexical");
        let cwd = sandbox.path("worktree");
        fs::create_dir_all(&cwd).expect("worktree fixture");
        let runner = Arc::new(RecordingGitRunner {
            outputs: HashMap::from([(
                "GitVcsDriver.resolveCommonDir".to_owned(),
                process_output("../missing/common.git\n"),
            )]),
            requests: Mutex::new(Vec::new()),
        });
        let repository = GitRepository::with_runner_for_test(runner);

        let resolved = repository
            .resolve_common_dir(&cwd, &CancellationToken::new())
            .await
            .expect("missing common directory uses lexical identity");

        assert_eq!(
            resolved,
            normalize_path_lexically(&cwd.join(PathBuf::from("../missing/common.git")))
        );
    }
}

async fn registered_worktree_path_index(
    operation: &str,
    cwd: &Path,
    registered_paths: &[String],
    path: &Path,
) -> Result<Option<usize>, GitCommandError> {
    let expected = canonical_worktree_path_key(path).await.map_err(|error| {
        simple_error(
            operation,
            cwd,
            &format!(
                "The requested worktree path '{}' could not be resolved to a physical identity: {error}",
                display_path(path)
            ),
        )
    })?;
    for (index, registered) in registered_paths.iter().enumerate() {
        let registered_path = Path::new(registered);
        let candidate = canonical_worktree_path_key(registered_path)
            .await
            .map_err(|error| {
                simple_error(
                    operation,
                    cwd,
                    &format!(
                        "The registered worktree path '{}' could not be resolved to a physical identity: {error}",
                        display_path(registered_path)
                    ),
                )
            })?;
        if candidate == expected {
            return Ok(Some(index));
        }
    }
    Ok(None)
}

fn exact_worktree_record<'a>(
    inventory: &'a GitWorktreeInventory,
    expected: &GitWorktreeRecord,
) -> Option<&'a GitWorktreeRecord> {
    inventory.records.iter().find(|candidate| {
        normalized_worktree_path(&candidate.path) == normalized_worktree_path(&expected.path)
            && candidate.head == expected.head
            && candidate.branch == expected.branch
            && candidate.is_primary == expected.is_primary
            && candidate.is_bare == expected.is_bare
            && candidate.locked == expected.locked
            && candidate.lock_reason == expected.lock_reason
            && candidate.is_prunable == expected.is_prunable
            && candidate.prunable_reason == expected.prunable_reason
    })
}

fn inventory_contains_path(inventory: &GitWorktreeInventory, path: &Path) -> bool {
    let expected = normalized_worktree_path(path);
    inventory
        .records
        .iter()
        .any(|candidate| normalized_worktree_path(&candidate.path) == expected)
}

fn sorted_prunable_records(inventory: &GitWorktreeInventory) -> Vec<GitWorktreeRecord> {
    let mut records = inventory
        .records
        .iter()
        .filter(|record| record.is_prunable && !record.locked)
        .cloned()
        .collect::<Vec<_>>();
    records.sort_by(|left, right| {
        normalized_worktree_path(&left.path).cmp(&normalized_worktree_path(&right.path))
    });
    records
}

fn normalized_worktree_path(path: &Path) -> String {
    normalize_worktree_path_key(path, host_path_platform())
}

async fn canonical_common_dir(
    operation: &str,
    anchor: &Path,
    common_dir: &Path,
) -> Result<PathBuf, GitCommandError> {
    tokio::fs::canonicalize(common_dir).await.map_err(|error| {
        simple_error(
            operation,
            anchor,
            &format!(
                "The common Git directory '{}' could not be verified: {error}",
                display_path(common_dir)
            ),
        )
    })
}

async fn resolve_worktree_prune_paths(
    anchor: &Path,
    common_dir: &Path,
    dry_run_records: Vec<WorktreePruneDryRunRecord>,
    cancellation: &CancellationToken,
) -> Result<Vec<(WorktreePruneDryRunRecord, PathBuf)>, GitCommandError> {
    if dry_run_records.is_empty() {
        return Ok(Vec::new());
    }
    let worktrees_dir = common_dir.join("worktrees");
    verify_prune_directory(&worktrees_dir)
        .await
        .map_err(|error| {
            prune_identity_error(
                anchor,
                &format!("Git's worktree administration directory could not be verified: {error}"),
            )
        })?;

    let mut resolved = Vec::with_capacity(dry_run_records.len());
    let mut path_keys = HashSet::with_capacity(dry_run_records.len());
    for record in dry_run_records {
        if cancellation.is_cancelled() {
            return Err(prune_identity_error(
                anchor,
                "Git worktree prune impact verification was interrupted.",
            ));
        }
        let path = resolve_worktree_prune_path(&worktrees_dir, &record.admin_id, cancellation)
            .await
            .map_err(|error| {
                prune_identity_error(
                    anchor,
                    &format!(
                        "Git's dry-run administration identity '{}' could not be verified: {error}",
                        record.admin_id
                    ),
                )
            })?;
        if !path_keys.insert(normalized_worktree_path(&path)) {
            return Err(prune_identity_error(
                anchor,
                "Git's dry-run contains duplicate normalized worktree paths.",
            ));
        }
        resolved.push((record, path));
    }
    if cancellation.is_cancelled() {
        return Err(prune_identity_error(
            anchor,
            "Git worktree prune impact verification was interrupted.",
        ));
    }
    Ok(resolved)
}

async fn verify_prune_directory(path: &Path) -> io::Result<()> {
    let metadata = tokio::fs::symlink_metadata(path).await?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected a non-symlink directory",
        ));
    }
    if tokio::fs::canonicalize(path).await? != path {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "directory identity does not match its canonical path",
        ));
    }
    Ok(())
}

async fn resolve_worktree_prune_path(
    worktrees_dir: &Path,
    admin_id: &str,
    cancellation: &CancellationToken,
) -> io::Result<PathBuf> {
    let admin_dir = worktrees_dir.join(admin_id);
    verify_prune_directory(&admin_dir).await?;
    let gitdir_path = admin_dir.join("gitdir");
    let before = tokio::fs::symlink_metadata(&gitdir_path).await?;
    if !before.is_file() || before.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected a non-symlink gitdir file",
        ));
    }

    let file = git_metadata_open_options().open(&gitdir_path).await?;
    let opened = file.metadata().await?;
    if !opened.is_file() || !same_prune_file_identity(&before, &opened) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "gitdir identity changed while it was opened",
        ));
    }
    let capacity = usize::try_from(opened.len())
        .unwrap_or(usize::MAX)
        .min(WORKTREE_PRUNE_GITDIR_LIMIT as usize);
    let mut bytes = Vec::with_capacity(capacity);
    let mut limited = file.take(WORKTREE_PRUNE_GITDIR_LIMIT + 1);
    let read = limited.read_to_end(&mut bytes);
    tokio::select! {
        result = read => result?,
        () = cancellation.cancelled() => {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "impact verification cancelled"));
        }
    };
    if bytes.len() as u64 > WORKTREE_PRUNE_GITDIR_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "gitdir identity exceeds its byte limit",
        ));
    }
    let after = tokio::fs::symlink_metadata(&gitdir_path).await?;
    if !after.is_file()
        || after.file_type().is_symlink()
        || !same_prune_file_identity(&opened, &after)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "gitdir identity changed while it was read",
        ));
    }
    verify_prune_directory(&admin_dir).await?;
    parse_prune_gitdir_path(bytes)
}

fn git_metadata_open_options() -> tokio::fs::OpenOptions {
    let mut options = tokio::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    #[cfg(windows)]
    options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    options
}

#[cfg(unix)]
fn same_prune_file_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_prune_file_identity(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.file_type() == right.file_type()
        && left.len() == right.len()
        && left.modified().ok() == right.modified().ok()
}

fn parse_prune_gitdir_path(mut bytes: Vec<u8>) -> io::Result<PathBuf> {
    if bytes.pop() != Some(b'\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "gitdir identity is unterminated",
        ));
    }
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    if bytes.is_empty() || bytes.contains(&b'\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "gitdir identity is empty or malformed",
        ));
    }
    let value = String::from_utf8(bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "gitdir identity is not valid UTF-8",
        )
    })?;
    let gitdir = PathBuf::from(value);
    if !gitdir.is_absolute() || gitdir.file_name().is_none_or(|name| name != ".git") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "gitdir identity is not an absolute worktree .git path",
        ));
    }
    gitdir.parent().map(Path::to_path_buf).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "gitdir identity has no worktree parent",
        )
    })
}

fn prune_identity_error(anchor: &Path, detail: &str) -> GitCommandError {
    simple_error("GitVcsDriver.previewWorktreePrune.verify", anchor, detail)
}

fn validate_removable_worktree(
    anchor: &Path,
    record: &GitWorktreeRecord,
) -> Result<(), GitCommandError> {
    if record.is_bare {
        return Err(simple_error(
            "GitVcsDriver.worktreeRemovalPreflight",
            anchor,
            "Bare worktrees are protected and cannot be removed.",
        ));
    }
    if record.is_primary {
        return Err(simple_error(
            "GitVcsDriver.worktreeRemovalPreflight",
            anchor,
            "The primary worktree is protected and cannot be removed.",
        ));
    }
    if record.locked {
        let reason = record
            .lock_reason
            .as_deref()
            .unwrap_or("Git did not provide a lock reason");
        return Err(simple_error(
            "GitVcsDriver.worktreeRemovalPreflight",
            anchor,
            &format!("The worktree registration is locked: {reason}"),
        ));
    }
    Ok(())
}

fn worktree_record_mismatch_error(
    operation: &str,
    anchor: &Path,
    record: &GitWorktreeRecord,
) -> GitCommandError {
    simple_error(
        operation,
        anchor,
        &format!(
            "The registered worktree record '{}' changed or does not belong to this repository.",
            display_path(&record.path)
        ),
    )
}

fn parse_worktree_removal_status(
    anchor: &Path,
    output: &str,
) -> Result<GitWorktreeRemovalInspection, GitCommandError> {
    if output.is_empty() {
        return Ok(GitWorktreeRemovalInspection::default());
    }
    if !output.ends_with('\0') {
        return Err(simple_error(
            "GitVcsDriver.inspectWorktreeRemoval.status",
            anchor,
            "Git returned an unterminated worktree status record.",
        ));
    }
    let mut inspection = GitWorktreeRemovalInspection::default();
    let mut records = output.split_terminator('\0');
    while let Some(record) = records.next() {
        if record.starts_with("? ") {
            if parse_porcelain_v2_line(record).is_none() {
                return Err(malformed_worktree_removal_status(anchor));
            }
            inspection.untracked_file_count = inspection
                .untracked_file_count
                .checked_add(1)
                .ok_or_else(|| malformed_worktree_removal_status(anchor))?;
            continue;
        }
        if record.starts_with("1 ") || record.starts_with("u ") {
            if parse_porcelain_v2_line(record).is_none() {
                return Err(malformed_worktree_removal_status(anchor));
            }
            inspection.tracked_change_count = inspection
                .tracked_change_count
                .checked_add(1)
                .ok_or_else(|| malformed_worktree_removal_status(anchor))?;
            continue;
        }
        if record.starts_with("2 ") {
            if parse_porcelain_v2_line(record).is_none() || records.next().is_none_or(str::is_empty)
            {
                return Err(malformed_worktree_removal_status(anchor));
            }
            inspection.tracked_change_count = inspection
                .tracked_change_count
                .checked_add(1)
                .ok_or_else(|| malformed_worktree_removal_status(anchor))?;
            continue;
        }
        if record.starts_with("! ") {
            continue;
        }
        return Err(malformed_worktree_removal_status(anchor));
    }
    Ok(inspection)
}

fn malformed_worktree_removal_status(anchor: &Path) -> GitCommandError {
    simple_error(
        "GitVcsDriver.inspectWorktreeRemoval.status",
        anchor,
        "Git returned a malformed worktree status record.",
    )
}

fn ensure_authoritative_inventory_output(
    operation: &str,
    cwd: &Path,
    output: &ProcessOutput,
) -> Result<(), GitCommandError> {
    if output.stdout_truncated || output.stderr_truncated {
        return Err(simple_error(
            operation,
            cwd,
            "Git worktree inventory output was truncated and is not authoritative.",
        ));
    }
    Ok(())
}

fn is_unsupported_porcelain_z_diagnostic(stderr: &str) -> bool {
    let diagnostic = stderr.to_ascii_lowercase();
    [
        "unknown option `z'",
        "unknown option 'z'",
        "unknown option: -z",
        "unknown switch `z'",
        "unknown switch 'z'",
        "unrecognized option '-z'",
    ]
    .into_iter()
    .any(|needle| diagnostic.contains(needle))
}

fn git_output_line(output: &str) -> Option<&str> {
    let line = output.strip_suffix('\n').unwrap_or(output);
    let line = line.strip_suffix('\r').unwrap_or(line);
    (!line.contains(['\n', '\r'])).then_some(line)
}

fn same_path(left: &Path, right: &Path) -> bool {
    let left = display_path(left);
    let right = display_path(right);
    if cfg!(windows) {
        left.eq_ignore_ascii_case(&right)
    } else {
        left == right
    }
}

fn resolve_git_metadata_path(base: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn parse_branch_headers(stdout: &str) -> (Option<String>, Option<String>, u64, u64) {
    let mut branch = None;
    let mut upstream = None;
    let mut has_upstream_comparison = false;
    let mut ahead = 0;
    let mut behind = 0;
    for line in stdout.lines() {
        if let Some(value) = line.strip_prefix("# branch.head ") {
            if value != "(detached)" {
                branch = Some(value.to_owned());
            }
        } else if let Some(value) = line.strip_prefix("# branch.upstream ") {
            upstream = Some(value.to_owned());
        } else if let Some(value) = line.strip_prefix("# branch.ab ") {
            has_upstream_comparison = true;
            for part in value.split_whitespace() {
                if let Some(value) = part.strip_prefix('+') {
                    ahead = value.parse().unwrap_or(0);
                } else if let Some(value) = part.strip_prefix('-') {
                    behind = value.parse().unwrap_or(0);
                }
            }
        }
    }
    if !has_upstream_comparison {
        upstream = None;
    }
    (branch, upstream, ahead, behind)
}

fn parse_summary_identity(
    cwd: &Path,
    stdout: &str,
) -> Result<(Option<String>, Option<String>), GitCommandError> {
    let mut oid = None;
    let mut head = None;
    for line in stdout.lines() {
        if let Some(value) = line.strip_prefix("# branch.oid ") {
            oid = Some(value.trim());
        } else if let Some(value) = line.strip_prefix("# branch.head ") {
            head = Some(value.trim());
        }
    }
    match (oid, head) {
        (Some("(initial)"), Some(_)) => Ok((None, None)),
        (Some(oid), Some("(detached)")) if !oid.is_empty() => Ok((None, Some(oid.to_owned()))),
        (Some(_), Some(head)) if !head.is_empty() && !head.starts_with('(') => {
            Ok((Some(head.to_owned()), None))
        }
        _ => Err(simple_error(
            "GitVcsDriver.summaryStatus.status",
            cwd,
            "Git returned incomplete branch identity in the status summary.",
        )),
    }
}

fn summary_observed_at() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .expect("UTC timestamps are always RFC 3339 representable")
}

async fn untracked_line_count(path: &Path) -> u64 {
    let Ok(bytes) = tokio::fs::read(path).await else {
        return 0;
    };
    if bytes.is_empty() {
        0
    } else {
        bytes.iter().filter(|byte| **byte == b'\n').count() as u64
            + u64::from(bytes.last() != Some(&b'\n'))
    }
}

fn area_order(area: Option<VcsStagingArea>) -> u8 {
    match area {
        Some(VcsStagingArea::Staged) => 0,
        Some(VcsStagingArea::Unstaged) => 1,
        Some(VcsStagingArea::Untracked) => 2,
        None => 3,
    }
}

fn parse_git_manager_head_commit(
    cwd: &Path,
    output: &str,
) -> Result<GitManagerHeadCommit, GitCommandError> {
    let mut fields = output.splitn(3, '\0');
    let sha = fields.next().unwrap_or_default().trim();
    let parents = fields.next().unwrap_or_default();
    let message = fields.next();
    if sha.is_empty() || message.is_none() {
        return Err(simple_error(
            "GitManager.undoCommit.head",
            cwd,
            "Git returned malformed HEAD commit data.",
        ));
    }
    Ok(GitManagerHeadCommit {
        sha: sha.to_owned(),
        parents: parents.split_whitespace().map(str::to_owned).collect(),
        message: message.unwrap_or_default().to_owned(),
    })
}

pub(crate) fn validate_pathspecs(
    operation: &str,
    cwd: &Path,
    paths: &[String],
) -> Result<(), GitCommandError> {
    for path in paths {
        let parsed = Path::new(path);
        if path.trim().is_empty()
            || parsed.is_absolute()
            || parsed
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(simple_error(
                operation,
                cwd,
                "File pathspec must be a non-empty repository-relative path without traversal.",
            ));
        }
    }
    Ok(())
}

fn parse_commit(line: &str) -> Option<VcsCommit> {
    let mut fields = line.split(COMMIT_FIELD_SEPARATOR);
    let sha = fields.next()?.to_owned();
    let short_sha = fields.next()?.to_owned();
    if sha.is_empty() || short_sha.is_empty() {
        return None;
    }
    Some(VcsCommit {
        sha,
        short_sha,
        subject: fields.next().unwrap_or_default().to_owned(),
        author_name: fields.next().unwrap_or_default().to_owned(),
        authored_at_ms: fields
            .next()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(0)
            .saturating_mul(1000),
    })
}

fn ref_priority(reference: &VcsRef) -> u8 {
    if reference.current {
        0
    } else if reference.is_default {
        1
    } else {
        2
    }
}

/// The origin's provider under the shared classification: well-known host names
/// first, then hosts that explicit provider probes identified (never a spawn here).
fn provider_info(hosts: &ProviderHosts, remote: &str) -> Option<SourceControlProviderInfo> {
    let info = match hosts.identify(remote) {
        source_control::IdentifiedProvider::Named(info)
        | source_control::IdentifiedProvider::Recorded(info) => info,
        source_control::IdentifiedProvider::Unknown => return None,
    };
    Some(SourceControlProviderInfo {
        kind: info.kind.into(),
        name: info.name,
        base_url: info.base_url,
    })
}

fn git_error(
    operation: &str,
    cwd: &Path,
    argument_count: usize,
    error: ProcessError,
) -> GitCommandError {
    let failure = error.safe_facts();
    let (exit_code, stdout_length, stderr_length, detail) = match error {
        ProcessError::NonZeroExit {
            exit_code,
            stdout_length,
            stderr_length,
            stdout,
            stderr,
            ..
        } => (
            Some(exit_code),
            Some(stdout_length),
            Some(stderr_length),
            actionable_git_failure(&stderr, &stdout),
        ),
        ProcessError::Cancelled { .. } => (None, None, None, "Git command was interrupted.".into()),
        ProcessError::Timeout { .. } => (None, None, None, "Git command timed out.".into()),
        ProcessError::OutputLimit { .. } => (
            None,
            None,
            None,
            "Git command output exceeded its limit.".into(),
        ),
        _ => (None, None, None, failure.message("git")),
    };
    GitCommandError {
        tag: "GitCommandError",
        operation: operation.into(),
        command: "git".into(),
        cwd: display_path(cwd).into(),
        diagnostics: Some(Box::new(GitCommandDiagnostics {
            failure: Some(failure),
            argument_count: Some(argument_count),
            exit_code,
            stdout_length,
            stderr_length,
        })),
        detail: detail.into(),
    }
}

/// Whether Git's output, or an error detail already mapped from it, reports that the
/// HTTP stall guard stopped a network transfer.
pub(crate) fn reports_stalled_transfer(text: &str) -> bool {
    text.contains(CURL_LOW_SPEED_ABORT) || text.contains(NETWORK_TRANSFER_STALLED)
}

fn actionable_git_failure(stderr: &str, stdout: &str) -> String {
    if reports_stalled_transfer(stderr) || reports_stalled_transfer(stdout) {
        return NETWORK_TRANSFER_STALLED.to_owned();
    }
    let output = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    if output.trim().is_empty() {
        return "Git process exited with a non-zero status.".to_owned();
    }
    redact_sensitive_text(output.trim())
}

fn simple_error(operation: &str, cwd: &Path, detail: &str) -> GitCommandError {
    GitCommandError {
        tag: "GitCommandError",
        operation: operation.into(),
        command: "git".into(),
        cwd: display_path(cwd).into(),
        diagnostics: None,
        detail: detail.into(),
    }
}

fn worktree_cleanup_error(cwd: &Path, path: &Path, error: &io::Error) -> GitCommandError {
    simple_error(
        "GitVcsDriver.removeWorktree",
        cwd,
        &format!(
            "The registered worktree path '{}' could not be cleaned before Git removal: {error}",
            display_path(path)
        ),
    )
}

fn worktree_removal_admission_error(cwd: &Path, path: &Path, error: &io::Error) -> GitCommandError {
    simple_error(
        "GitVcsDriver.removeWorktree",
        cwd,
        &format!(
            "The registered worktree path '{}' is still in use, so removal stopped before deleting any files: {error}",
            display_path(path)
        ),
    )
}

fn registered_worktree_identity_error(cwd: &Path, path: &Path) -> GitCommandError {
    simple_error(
        "GitVcsDriver.removeWorktree",
        cwd,
        &format!(
            "The registered worktree path '{}' no longer has a verifiable linked-worktree identity, so it was not deleted.",
            display_path(path)
        ),
    )
}

fn worktree_identity_marker_error(cwd: &Path, path: &Path, detail: &str) -> GitCommandError {
    simple_error(
        "GitVcsDriver.removeWorktree",
        cwd,
        &format!(
            "The registered worktree path '{}' does not match the durable removal identity, so it was not deleted: {detail}",
            display_path(path)
        ),
    )
}

fn with_filesystem_cleanup_deferred(mut error: GitCommandError, path: &Path) -> GitCommandError {
    error.detail = format!(
        "{}\nGit deregistered the worktree, but the remaining directory '{}' was preserved because it no longer has a transaction-bound removal identity.",
        error.detail,
        display_path(path),
    )
    .into();
    error
}

fn reserve_implicit_worktree_path(
    cwd: &Path,
    path: &Path,
    path_policy: &WorktreePathPolicy,
) -> Result<OwnedWorktreePath, GitCommandError> {
    OwnedWorktreePath::reserve(path.to_path_buf()).map_err(|error| {
        worktree_reservation_error(cwd, path, error, path_policy.configured_base())
    })
}

fn worktree_reservation_error(
    cwd: &Path,
    path: &Path,
    error: WorktreePathReservationError,
    configured_base: Option<&Path>,
) -> GitCommandError {
    if !error.is_destination_collision()
        && let Some(configured_base) = configured_base
    {
        return workspace_unavailable_error(cwd, configured_base);
    }

    let detail = if error.kind() == io::ErrorKind::AlreadyExists {
        format!(
            "The requested worktree path '{}' already exists. Choose an absent path so BiBCode can safely own its cleanup.",
            display_path(path)
        )
    } else {
        format!(
            "The worktree path '{}' could not be reserved: {}",
            display_path(path),
            error.source
        )
    };
    simple_error("GitVcsDriver.createWorktree", cwd, &detail)
}

fn workspace_unavailable_error(cwd: &Path, workspace: &Path) -> GitCommandError {
    simple_error(
        "GitVcsDriver.createWorktree",
        cwd,
        &format!(
            "Workspace {} is unavailable. Choose another folder in Settings → General.",
            display_path(workspace)
        ),
    )
}

fn command_output_error(
    operation: &str,
    cwd: &Path,
    argument_count: usize,
    output: &ProcessOutput,
    detail: &str,
) -> GitCommandError {
    GitCommandError {
        tag: "GitCommandError",
        operation: operation.into(),
        command: "git".into(),
        cwd: display_path(cwd).into(),
        diagnostics: Some(Box::new(GitCommandDiagnostics {
            failure: None,
            argument_count: Some(argument_count),
            exit_code: Some(output.exit_code),
            stdout_length: Some(output.stdout.len()),
            stderr_length: Some(output.stderr.len()),
        })),
        detail: detail.into(),
    }
}

fn display_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    if let Some(unc_path) = raw.strip_prefix(r"\\?\UNC\") {
        return format!("//{}", unc_path.replace('\\', "/"));
    }
    raw.strip_prefix(r"\\?\").unwrap_or(&raw).replace('\\', "/")
}

#[cfg(test)]
mod worktree_ownership_tests {
    use std::{
        collections::VecDeque,
        fs,
        path::Path,
        process::Command,
        sync::{Arc, Mutex},
    };

    use tempfile::TempDir;
    use tokio_util::sync::CancellationToken;

    use super::{
        CreateWorktreeInput, GitProcessRunner, GitPrunableWorktree, GitRepository,
        GitWorktreeRecord, OutputPolicy, OwnedWorktreePath, ProcessOutput, ProcessRequest,
        WORKTREE_INVENTORY_OUTPUT_LIMIT, display_path, ensure_authoritative_inventory_output,
        git_output_line, is_unsupported_porcelain_z_diagnostic, parse_branch_headers,
        run_worktree_removal_probe_blocking,
    };
    #[cfg(unix)]
    use super::{
        WORKTREE_PRUNE_GITDIR_LIMIT, WorktreePruneDryRunRecord, resolve_worktree_prune_paths,
    };
    use crate::git::git_worktree_prune_impact_digest;

    #[cfg(windows)]
    use super::{
        clear_windows_directory_bound_after_snapshot, create_removal_directory_lease,
        delete_bound_directory_root_blocking, open_removal_directory_lease,
        rename_bound_directory_blocking,
    };

    #[test]
    fn cancellation_after_probe_rename_still_restores_the_original_path() {
        let parent = tempfile::tempdir().expect("probe parent");
        let source = parent.path().join("worktree");
        let probe = parent.path().join(".worktree.probe");
        fs::create_dir(&source).expect("source worktree");
        fs::write(source.join("sentinel.txt"), "preserve\n").expect("sentinel");
        let cancellation = CancellationToken::new();

        run_worktree_removal_probe_blocking(&source, &probe, &cancellation, || {
            assert!(probe.exists(), "probe owns the directory between renames");
            cancellation.cancel();
        })
        .expect("rollback ignores cancellation after the first rename");

        assert!(source.join("sentinel.txt").exists());
        assert!(!probe.exists());
    }

    #[cfg(windows)]
    #[test]
    fn removal_directory_lease_prevents_windows_path_rebinding() {
        let parent = tempfile::tempdir().expect("lease parent");
        let source = parent.path().join("quarantine");
        let replacement_name = parent.path().join("quarantine-moved");
        fs::create_dir(&source).expect("quarantine fixture");
        let lease = open_removal_directory_lease(&source).expect("bind quarantine object");

        fs::rename(&source, &replacement_name)
            .expect_err("a bound removal object must not be renamed or replaced");
        assert!(source.exists());

        drop(lease);
        fs::rename(&source, &replacement_name).expect("dropping the lease releases the object");
    }

    #[cfg(windows)]
    #[test]
    fn removal_renames_and_deletes_the_bound_windows_directory_object() {
        let parent = tempfile::tempdir().expect("lease parent");
        let source = parent.path().join("source");
        let destination = parent.path().join("destination");
        let lease = create_removal_directory_lease(&source)
            .expect("atomically create and bind source object");

        rename_bound_directory_blocking(&lease, &source, &destination)
            .expect("rename the exact leased object by handle");
        assert!(!source.exists());
        assert!(destination.exists());

        delete_bound_directory_root_blocking(lease, &destination)
            .expect("delete the exact leased root by handle");
        assert!(!destination.exists());
    }

    #[cfg(windows)]
    #[test]
    fn removal_rejects_a_child_rebound_after_its_identity_snapshot() {
        let parent = tempfile::tempdir().expect("cleanup parent");
        let quarantine = parent.path().join("quarantine");
        let child = quarantine.join("child");
        let moved_child = quarantine.join("original-child");
        let sibling = parent.path().join("sibling");
        fs::create_dir_all(&child).expect("quarantined child");
        fs::write(child.join("original.txt"), "original\n").expect("original file");
        fs::create_dir(&sibling).expect("sibling directory");
        fs::write(sibling.join("sibling.txt"), "must survive\n").expect("sibling sentinel");

        clear_windows_directory_bound_after_snapshot(&quarantine, None, || {
            fs::rename(&child, &moved_child).expect("move snapshotted child aside");
            fs::rename(&sibling, &child).expect("rebind child name to sibling");
        })
        .expect_err("the rebound child identity must stop cleanup");

        assert_eq!(
            fs::read_to_string(child.join("sibling.txt")).expect("sibling survives"),
            "must survive\n"
        );
        assert!(moved_child.join("original.txt").exists());
    }

    #[cfg(windows)]
    #[test]
    fn removal_deletes_a_junction_without_traversing_its_target() {
        let parent = tempfile::tempdir().expect("cleanup parent");
        let quarantine = parent.path().join("quarantine");
        let outside = parent.path().join("outside");
        let linked = quarantine.join("linked-outside");
        fs::create_dir(&quarantine).expect("quarantine directory");
        fs::create_dir(&outside).expect("outside directory");
        fs::write(outside.join("sentinel.txt"), "must survive\n").expect("outside sentinel");
        junction::create(&outside, &linked).expect("junction fixture");

        clear_windows_directory_bound_after_snapshot(&quarantine, None, || {})
            .expect("cleanup deletes only the junction object");

        assert_eq!(
            fs::read_to_string(outside.join("sentinel.txt")).expect("outside target survives"),
            "must survive\n"
        );
        assert!(!linked.exists());
    }

    #[cfg(windows)]
    #[test]
    fn removal_rejects_a_root_junction_without_traversing_its_target() {
        let parent = tempfile::tempdir().expect("cleanup parent");
        let outside = parent.path().join("outside");
        let quarantine = parent.path().join("quarantine-junction");
        fs::create_dir(&outside).expect("outside directory");
        fs::write(outside.join("sentinel.txt"), "must survive\n").expect("outside sentinel");
        junction::create(&outside, &quarantine).expect("root junction fixture");

        assert!(
            open_removal_directory_lease(&quarantine).is_err(),
            "a root reparse point must never be admitted for recursive cleanup"
        );

        assert_eq!(
            fs::read_to_string(outside.join("sentinel.txt")).expect("outside target survives"),
            "must survive\n"
        );
    }

    #[test]
    fn branch_headers_require_an_actual_upstream_comparison() {
        assert_eq!(
            parse_branch_headers("# branch.head main\n# branch.upstream origin/main\n"),
            (Some("main".into()), None, 0, 0)
        );
        assert_eq!(
            parse_branch_headers(
                "# branch.head main\n# branch.upstream origin/main\n# branch.ab +0 -0\n"
            ),
            (Some("main".into()), Some("origin/main".into()), 0, 0)
        );
    }

    #[test]
    fn inventory_rejects_truncated_output_and_only_falls_back_for_explicit_z_diagnostics() {
        let output = ProcessOutput {
            exit_code: 0,
            stdout: "worktree /repo\0\0".to_owned(),
            stderr: String::new(),
            stdout_truncated: true,
            stderr_truncated: false,
        };
        assert!(
            ensure_authoritative_inventory_output(
                "GitVcsDriver.worktreeInventory.list",
                Path::new("/repo"),
                &output,
            )
            .is_err()
        );
        assert!(is_unsupported_porcelain_z_diagnostic(
            "error: unknown option `z'\nusage: git worktree list"
        ));
        assert!(!is_unsupported_porcelain_z_diagnostic(
            "fatal: not a git repository"
        ));
        assert_eq!(
            git_output_line(" /repo with space \n"),
            Some(" /repo with space ")
        );
        assert_eq!(git_output_line("/one\n/two\n"), None);
    }

    #[tokio::test]
    async fn inventory_executes_legacy_fallback_once_and_caches_it_per_repository() {
        let runner = Arc::new(FakeGitRunner::new([
            process_output(0, ".git\n", ""),
            process_output(129, "", "error: unknown switch `z'"),
            process_output(
                0,
                "worktree /repo\nHEAD abc\nbranch refs/heads/main\n\n",
                "",
            ),
            process_output(0, ".git\n", ""),
            process_output(
                0,
                "worktree /repo\nHEAD abc\nbranch refs/heads/main\n\n",
                "",
            ),
        ]));
        let repository = GitRepository::with_runner_for_test(runner.clone());
        let anchor = tempfile::tempdir().expect("inventory anchor");
        let token = CancellationToken::new();

        for _ in 0..2 {
            let inventory = repository
                .worktree_inventory(anchor.path(), &token)
                .await
                .expect("legacy inventory");
            assert!(!inventory.nul_delimited);
        }

        let requests = runner.requests();
        let nul_request = requests
            .iter()
            .find(|request| request.args.iter().any(|argument| argument == "-z"))
            .expect("one capability probe");
        assert!(nul_request.allow_non_zero_exit);
        assert_eq!(nul_request.output_policy, OutputPolicy::Error);
        assert_eq!(
            nul_request.max_output_bytes,
            WORKTREE_INVENTORY_OUTPUT_LIMIT
        );
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.args.iter().any(|argument| argument == "-z"))
                .count(),
            1
        );
        assert_eq!(
            requests
                .iter()
                .filter(|request| {
                    request
                        .args
                        .iter()
                        .any(|argument| argument == "--porcelain")
                        && !request.args.iter().any(|argument| argument == "-z")
                })
                .count(),
            2
        );
        assert!(
            requests
                .iter()
                .filter(|request| {
                    request
                        .args
                        .iter()
                        .any(|argument| argument == "--porcelain")
                        && !request.args.iter().any(|argument| argument == "-z")
                })
                .all(|request| !request.allow_non_zero_exit)
        );
    }

    #[tokio::test]
    async fn verified_prune_rejects_a_successful_exit_when_the_target_registration_survives() {
        let fixture = tempfile::tempdir().expect("surviving target fixture");
        let admin_dir = fixture.path().join(".git/worktrees/target");
        fs::create_dir_all(&admin_dir).expect("worktree admin fixture");
        let target_path = fixture.path().join("target");
        fs::write(
            admin_dir.join("gitdir"),
            format!("{}/.git\n", display_path(&target_path)),
        )
        .expect("target admin identity");
        let inventory = format!(
            "worktree {}\0HEAD aaa\0branch refs/heads/main\0\0worktree {}\0HEAD bbb\0branch refs/heads/feature\0prunable stale\0\0",
            display_path(fixture.path()),
            display_path(&target_path),
        );
        let runner = Arc::new(FakeGitRunner::new([
            process_output(0, ".git\n", ""),
            process_output(0, &inventory, ""),
            process_output(0, "", "Removing worktrees/target: stale\n"),
            process_output(0, ".git\n", ""),
            process_output(0, &inventory, ""),
            process_output(0, ".git\n", ""),
            process_output(0, &inventory, ""),
            process_output(0, "", "Removing worktrees/target: stale\n"),
            process_output(0, "", ""),
            process_output(0, ".git\n", ""),
            process_output(0, &inventory, ""),
        ]));
        let repository = GitRepository::with_runner_for_test(runner.clone());
        let target = GitWorktreeRecord {
            path: target_path,
            head: Some("bbb".into()),
            branch: Some("feature".into()),
            is_primary: false,
            is_bare: false,
            locked: false,
            lock_reason: None,
            is_prunable: true,
            prunable_reason: Some("stale".into()),
        };
        let expected = repository
            .preview_worktree_prune(fixture.path(), &CancellationToken::new())
            .await
            .expect("preview");
        let digest = git_worktree_prune_impact_digest(&expected);

        let error = repository
            .prune_worktrees_verified(fixture.path(), &target, &digest, &CancellationToken::new())
            .await
            .expect_err("surviving target invalidates successful prune exit");
        assert!(error.detail.to_ascii_lowercase().contains("survived"));
        assert_eq!(
            runner
                .requests()
                .iter()
                .filter(|request| {
                    request.args.iter().any(|argument| argument == "prune")
                        && !request.args.iter().any(|argument| argument == "--dry-run")
                })
                .count(),
            1
        );
        let requests = runner.requests();
        let mutation_index = requests
            .iter()
            .position(|request| {
                request.args.iter().any(|argument| argument == "prune")
                    && !request.args.iter().any(|argument| argument == "--dry-run")
            })
            .expect("prune mutation request");
        assert!(
            mutation_index > 0
                && requests[mutation_index - 1]
                    .args
                    .iter()
                    .any(|argument| argument == "--dry-run"),
            "the confirmed dry-run is the final Git command before prune mutation"
        );
    }

    #[tokio::test]
    async fn verified_prune_rejects_a_same_reason_admin_identity_swap_before_mutation() {
        let fixture = tempfile::tempdir().expect("prune identity fixture");
        let common_dir = fixture.path().join(".git");
        let admin_dir = common_dir.join("worktrees/target");
        fs::create_dir_all(&admin_dir).expect("worktree admin fixture");
        let old_path = fixture.path().join("old-target");
        let new_path = fixture.path().join("new-target");
        fs::write(
            admin_dir.join("gitdir"),
            format!("{}/.git\n", display_path(&old_path)),
        )
        .expect("old admin identity");
        let inventory = format!(
            "worktree {}\0HEAD aaa\0branch refs/heads/main\0\0worktree {}\0HEAD bbb\0branch refs/heads/feature\0prunable stale\0\0",
            display_path(fixture.path()),
            display_path(&old_path),
        );
        let after_prune = format!(
            "worktree {}\0HEAD aaa\0branch refs/heads/main\0\0",
            display_path(fixture.path()),
        );
        let replacement_gitdir = admin_dir.join("gitdir");
        let replacement_path = new_path.clone();
        let runner = Arc::new(FakeGitRunner::with_dry_run_action(
            [
                process_output(0, ".git\n", ""),
                process_output(0, &inventory, ""),
                process_output(0, "", "Removing worktrees/target: stale\n"),
                process_output(0, "", ""),
                process_output(0, ".git\n", ""),
                process_output(0, &after_prune, ""),
            ],
            Arc::new(move || {
                fs::write(
                    &replacement_gitdir,
                    format!("{}/.git\n", display_path(&replacement_path)),
                )
                .expect("swap admin identity");
            }),
        ));
        let repository = GitRepository::with_runner_for_test(runner.clone());
        let target = GitWorktreeRecord {
            path: old_path.clone(),
            head: Some("bbb".into()),
            branch: Some("feature".into()),
            is_primary: false,
            is_bare: false,
            locked: false,
            lock_reason: None,
            is_prunable: true,
            prunable_reason: Some("stale".into()),
        };
        let confirmed = [GitPrunableWorktree {
            path: old_path,
            prune_reason: "stale".into(),
            locked: false,
            lock_reason: None,
        }];

        repository
            .prune_worktrees_verified(
                fixture.path(),
                &target,
                &git_worktree_prune_impact_digest(&confirmed),
                &CancellationToken::new(),
            )
            .await
            .expect_err("same-reason replacement cannot inherit confirmed impact");
        assert_eq!(
            runner
                .requests()
                .iter()
                .filter(|request| {
                    request.args.iter().any(|argument| argument == "prune")
                        && !request.args.iter().any(|argument| argument == "--dry-run")
                })
                .count(),
            0,
            "identity drift is rejected before repository-wide prune"
        );
    }

    #[tokio::test]
    async fn verified_prune_rejects_same_path_reason_drift_before_mutation() {
        let fixture = tempfile::tempdir().expect("prune reason fixture");
        let common_dir = fixture.path().join(".git");
        let target_admin = common_dir.join("worktrees/target");
        let other_admin = common_dir.join("worktrees/other");
        fs::create_dir_all(&target_admin).expect("target admin fixture");
        fs::create_dir_all(&other_admin).expect("other admin fixture");
        let target_path = fixture.path().join("target");
        let other_path = fixture.path().join("other");
        fs::write(
            target_admin.join("gitdir"),
            format!("{}/.git\n", display_path(&target_path)),
        )
        .expect("target admin identity");
        fs::write(
            other_admin.join("gitdir"),
            format!("{}/.git\n", display_path(&other_path)),
        )
        .expect("other admin identity");
        let inventory = format!(
            "worktree {}\0HEAD aaa\0branch refs/heads/main\0\0worktree {}\0HEAD bbb\0branch refs/heads/target\0prunable stale\0\0worktree {}\0HEAD ccc\0branch refs/heads/other\0prunable reason-after-preview\0\0",
            display_path(fixture.path()),
            display_path(&target_path),
            display_path(&other_path),
        );
        let after_prune = format!(
            "worktree {}\0HEAD aaa\0branch refs/heads/main\0\0",
            display_path(fixture.path()),
        );
        let runner = Arc::new(FakeGitRunner::new([
            process_output(0, ".git\n", ""),
            process_output(0, &inventory, ""),
            process_output(
                0,
                "",
                "Removing worktrees/target: stale\nRemoving worktrees/other: reason-after-preview\n",
            ),
            process_output(0, "", ""),
            process_output(0, ".git\n", ""),
            process_output(0, &after_prune, ""),
        ]));
        let repository = GitRepository::with_runner_for_test(runner.clone());
        let target = GitWorktreeRecord {
            path: target_path.clone(),
            head: Some("bbb".into()),
            branch: Some("target".into()),
            is_primary: false,
            is_bare: false,
            locked: false,
            lock_reason: None,
            is_prunable: true,
            prunable_reason: Some("stale".into()),
        };
        let confirmed = [
            GitPrunableWorktree {
                path: target_path,
                prune_reason: "stale".into(),
                locked: false,
                lock_reason: None,
            },
            GitPrunableWorktree {
                path: other_path,
                prune_reason: "reason-before-preview".into(),
                locked: false,
                lock_reason: None,
            },
        ];

        repository
            .prune_worktrees_verified(
                fixture.path(),
                &target,
                &git_worktree_prune_impact_digest(&confirmed),
                &CancellationToken::new(),
            )
            .await
            .expect_err("same-path reason drift invalidates confirmed impact");
        assert_eq!(
            runner
                .requests()
                .iter()
                .filter(|request| {
                    request.args.iter().any(|argument| argument == "prune")
                        && !request.args.iter().any(|argument| argument == "--dry-run")
                })
                .count(),
            0,
            "reason drift is rejected before repository-wide prune"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn prune_admin_identity_resolution_rejects_symlinks_oversize_and_cancellation() {
        use std::os::unix::fs::symlink;

        let fixture = tempfile::tempdir().expect("admin identity fixture");
        let common_dir = fixture.path().join(".git");
        let worktrees_dir = common_dir.join("worktrees");
        let outside_admin = fixture.path().join("outside-admin");
        fs::create_dir_all(&outside_admin).expect("outside admin fixture");
        fs::create_dir_all(&worktrees_dir).expect("worktrees admin root");
        let common_dir = fs::canonicalize(&common_dir).expect("canonical common directory");
        fs::write(outside_admin.join("gitdir"), "/target/.git\n").expect("outside gitdir fixture");
        symlink(&outside_admin, worktrees_dir.join("linked")).expect("symlinked admin fixture");
        let record = |admin_id: &str| WorktreePruneDryRunRecord {
            admin_id: admin_id.to_owned(),
            reason: "stale".to_owned(),
        };

        resolve_worktree_prune_paths(
            fixture.path(),
            &common_dir,
            vec![record("linked")],
            &CancellationToken::new(),
        )
        .await
        .expect_err("symlinked admin directory is rejected");

        let oversized_admin = worktrees_dir.join("oversized");
        fs::create_dir(&oversized_admin).expect("oversized admin fixture");
        fs::write(
            oversized_admin.join("gitdir"),
            vec![b'x'; WORKTREE_PRUNE_GITDIR_LIMIT as usize + 1],
        )
        .expect("oversized gitdir fixture");
        resolve_worktree_prune_paths(
            fixture.path(),
            &common_dir,
            vec![record("oversized")],
            &CancellationToken::new(),
        )
        .await
        .expect_err("oversized gitdir identity is rejected");

        let cancellation = CancellationToken::new();
        cancellation.cancel();
        resolve_worktree_prune_paths(
            fixture.path(),
            &common_dir,
            vec![record("oversized")],
            &cancellation,
        )
        .await
        .expect_err("cancelled identity resolution cannot approach mutation");
    }

    struct FakeGitRunner {
        outputs: Mutex<VecDeque<ProcessOutput>>,
        requests: Mutex<Vec<ProcessRequest>>,
        dry_run_action: Option<Arc<dyn Fn() + Send + Sync>>,
    }

    impl FakeGitRunner {
        fn new(outputs: impl IntoIterator<Item = ProcessOutput>) -> Self {
            Self {
                outputs: Mutex::new(outputs.into_iter().collect()),
                requests: Mutex::new(Vec::new()),
                dry_run_action: None,
            }
        }

        fn with_dry_run_action(
            outputs: impl IntoIterator<Item = ProcessOutput>,
            dry_run_action: Arc<dyn Fn() + Send + Sync>,
        ) -> Self {
            Self {
                outputs: Mutex::new(outputs.into_iter().collect()),
                requests: Mutex::new(Vec::new()),
                dry_run_action: Some(dry_run_action),
            }
        }

        fn requests(&self) -> Vec<ProcessRequest> {
            self.requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        }
    }

    impl GitProcessRunner for FakeGitRunner {
        fn run<'a>(
            &'a self,
            request: ProcessRequest,
            _cancellation: &'a CancellationToken,
        ) -> super::BoxGitProcessFuture<'a> {
            if request.args.iter().any(|argument| argument == "--dry-run")
                && let Some(action) = self.dry_run_action.as_ref()
            {
                action();
            }
            self.requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(request);
            let output = self
                .outputs
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .pop_front()
                .expect("fake output");
            Box::pin(async move { Ok(output) })
        }
    }

    fn process_output(exit_code: i32, stdout: &str, stderr: &str) -> ProcessOutput {
        ProcessOutput {
            exit_code,
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
            stdout_truncated: false,
            stderr_truncated: false,
        }
    }

    fn git(cwd: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "BiBCode Test")
            .env("GIT_AUTHOR_EMAIL", "bibcode@example.invalid")
            .env("GIT_COMMITTER_NAME", "BiBCode Test")
            .env("GIT_COMMITTER_EMAIL", "bibcode@example.invalid")
            .output()
            .expect("git starts");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn repository() -> TempDir {
        let repo = tempfile::tempdir().expect("temporary repository");
        git(repo.path(), &["init", "-b", "main"]);
        fs::write(repo.path().join("README.md"), "base\n").expect("write fixture");
        git(repo.path(), &["add", "README.md"]);
        git(repo.path(), &["commit", "-m", "initial"]);
        repo
    }

    #[tokio::test]
    async fn ensure_worktree_reuses_the_canonical_path_for_an_existing_branch() {
        let repo = repository();
        let parent = tempfile::tempdir().expect("worktree parent");
        let path = parent.path().join("feature");
        let input = CreateWorktreeInput {
            cwd: repo.path().to_path_buf(),
            ref_name: "main".to_owned(),
            new_ref_name: Some("feature".to_owned()),
            base_ref_name: None,
            path: Some(path),
        };
        let repository = GitRepository::default();
        let created = repository
            .create_worktree(input.clone(), &CancellationToken::new())
            .await
            .expect("create worktree");
        let ensured = repository
            .ensure_worktree(input, &CancellationToken::new())
            .await
            .expect("ensure existing worktree");

        assert_eq!(ensured.worktree.path, created.worktree.path);
        assert_eq!(ensured.worktree.ref_name, "feature");
        let worktrees = git(repo.path(), &["worktree", "list", "--porcelain"]);
        assert_eq!(worktrees.matches("branch refs/heads/feature").count(), 1);
        assert!(!worktrees.contains("refs/heads/feature-2"));
    }

    #[tokio::test]
    async fn failed_detached_preparation_cleans_its_owned_reserved_path() {
        let repo = repository();
        let parent = tempfile::tempdir().expect("worktree parent");
        let path = parent.path().join("reserved");
        let owned = OwnedWorktreePath::reserve(path.clone()).expect("reserve absent path");

        GitRepository::default()
            .prepare_detached_owned_worktree(
                repo.path(),
                "missing-ref",
                &owned,
                &CancellationToken::new(),
            )
            .await
            .expect_err("missing base fails detached preparation");

        assert!(!path.exists());
        assert!(
            !git(repo.path(), &["worktree", "list", "--porcelain"])
                .replace('\\', "/")
                .contains(&display_path(&path))
        );
    }

    #[tokio::test]
    async fn cancelled_detached_preparation_cleans_its_owned_reserved_path() {
        let repo = repository();
        let parent = tempfile::tempdir().expect("worktree parent");
        let path = parent.path().join("reserved");
        let owned = OwnedWorktreePath::reserve(path.clone()).expect("reserve absent path");
        let cancelled = CancellationToken::new();
        cancelled.cancel();

        GitRepository::default()
            .prepare_detached_owned_worktree(repo.path(), "main", &owned, &cancelled)
            .await
            .expect_err("cancelled detached preparation fails");

        assert!(!path.exists());
        assert!(
            !git(repo.path(), &["worktree", "list", "--porcelain"])
                .replace('\\', "/")
                .contains(&display_path(&path))
        );
    }

    #[tokio::test]
    async fn cancelled_branch_checkout_cleans_the_prepared_detached_worktree() {
        let repo = repository();
        let parent = tempfile::tempdir().expect("worktree parent");
        let path = parent.path().join("reserved");
        let owned = OwnedWorktreePath::reserve(path.clone()).expect("reserve absent path");
        let repository = GitRepository::default();
        repository
            .prepare_detached_owned_worktree(repo.path(), "main", &owned, &CancellationToken::new())
            .await
            .expect("Git accepts an atomically reserved empty directory");
        let cancelled = CancellationToken::new();
        cancelled.cancel();

        repository
            .checkout_owned_suffixed_branch(repo.path(), &owned, "main-2", &cancelled)
            .await
            .expect_err("cancelled checkout fails");

        assert!(!path.exists());
        assert!(git(repo.path(), &["branch", "--list", "main-2"]).is_empty());
        assert!(
            !git(repo.path(), &["worktree", "list", "--porcelain"])
                .replace('\\', "/")
                .contains(&display_path(&path))
        );
    }

    #[tokio::test]
    async fn cancelled_branch_checkout_rolls_back_branch_claimed_by_owned_worktree() {
        let repo = repository();
        let parent = tempfile::tempdir().expect("worktree parent");
        let path = parent.path().join("reserved");
        let owned = OwnedWorktreePath::reserve(path.clone()).expect("reserve absent path");
        let repository = GitRepository::default();
        repository
            .prepare_detached_owned_worktree(repo.path(), "main", &owned, &CancellationToken::new())
            .await
            .expect("prepare detached worktree");
        git(&path, &["checkout", "-b", "main-2"]);
        let cancelled = CancellationToken::new();
        cancelled.cancel();

        repository
            .checkout_owned_suffixed_branch(repo.path(), &owned, "main-2", &cancelled)
            .await
            .expect_err("cancelled checkout fails");

        assert!(!path.exists());
        assert!(git(repo.path(), &["branch", "--list", "main-2"]).is_empty());
    }

    #[tokio::test]
    async fn competing_branch_checkout_failure_cleans_only_the_owned_worktree() {
        let repo = repository();
        let parent = tempfile::tempdir().expect("worktree parent");
        let path = parent.path().join("reserved");
        let owned = OwnedWorktreePath::reserve(path.clone()).expect("reserve absent path");
        let repository = GitRepository::default();
        repository
            .prepare_detached_owned_worktree(repo.path(), "main", &owned, &CancellationToken::new())
            .await
            .expect("prepare detached worktree");
        git(repo.path(), &["branch", "main-2"]);

        repository
            .checkout_owned_suffixed_branch(
                repo.path(),
                &owned,
                "main-2",
                &CancellationToken::new(),
            )
            .await
            .expect_err("competing branch makes checkout fail");

        assert!(!path.exists());
        assert_eq!(git(repo.path(), &["branch", "--list", "main-2"]), "main-2");
        assert!(
            !git(repo.path(), &["worktree", "list", "--porcelain"])
                .replace('\\', "/")
                .contains(&display_path(&path))
        );
    }
}

#[cfg(test)]
mod clone_tests {
    use std::{
        ffi::OsString,
        fs,
        path::{Path, PathBuf},
        process::Command,
        sync::{Arc, Mutex},
        time::Duration,
    };

    use tokio::{
        io::AsyncReadExt,
        net::{TcpListener, TcpStream},
        sync::mpsc,
        time::{Instant, timeout},
    };
    use tokio_util::sync::CancellationToken;

    use super::{
        BoxGitProcessFuture, DEFAULT_TIMEOUT, GitProcessRunner, GitRepository,
        NETWORK_TRANSFER_CONFIG, NETWORK_TRANSFER_STALLED, ProcessError, ProcessRequest,
        ProcessRunner, display_path,
    };
    use crate::test_support::TestSandbox;

    const CLONE_FIXTURE_DEADLINE: Duration = Duration::from_secs(20);

    /// Runs real Git with extra environment, optionally through a slow wrapper for `clone`.
    struct FixtureGitRunner {
        slow_clone: Option<PathBuf>,
        environment: Vec<(OsString, OsString)>,
        requests: Mutex<Vec<ProcessRequest>>,
    }

    impl FixtureGitRunner {
        fn new(environment: &[(&str, &str)]) -> Self {
            Self {
                slow_clone: None,
                environment: environment
                    .iter()
                    .map(|(key, value)| ((*key).into(), (*value).into()))
                    .collect(),
                requests: Mutex::new(Vec::new()),
            }
        }

        fn with_slow_clone(mut self, script: PathBuf) -> Self {
            self.slow_clone = Some(script);
            self
        }

        fn clone_request(&self) -> ProcessRequest {
            self.requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .iter()
                .find(|request| request.operation == super::CLONE_OPERATION)
                .cloned()
                .expect("a clone request was issued")
        }
    }

    impl GitProcessRunner for FixtureGitRunner {
        fn run<'a>(
            &'a self,
            mut request: ProcessRequest,
            cancellation: &'a CancellationToken,
        ) -> BoxGitProcessFuture<'a> {
            if let Some(script) = self.slow_clone.as_ref()
                && request.args.iter().any(|argument| argument == "clone")
            {
                request.command = script.clone();
            }
            request.env.extend(self.environment.iter().cloned());
            self.requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(request.clone());
            Box::pin(async move { ProcessRunner.run(request, cancellation).await })
        }
    }

    /// Writes part of a clone into the destination, then fails the way the runner reports
    /// a timeout or a cancellation after it stopped Git.
    struct PartialCloneRunner {
        failure: fn(String) -> ProcessError,
        requests: Mutex<Vec<ProcessRequest>>,
    }

    impl GitProcessRunner for PartialCloneRunner {
        fn run<'a>(
            &'a self,
            request: ProcessRequest,
            _cancellation: &'a CancellationToken,
        ) -> BoxGitProcessFuture<'a> {
            let destination = request
                .cwd
                .join(request.args.last().expect("clone destination"));
            fs::create_dir_all(destination.join(".git/objects/pack")).expect("partial clone");
            fs::write(destination.join(".git/HEAD"), "ref: refs/heads/.invalid\n")
                .expect("placeholder HEAD");
            let pack = destination.join(".git/objects/pack/tmp_pack_partial");
            fs::write(&pack, b"partial pack").expect("partial pack");
            // Git creates its temporary pack read-only, as in the field snapshot.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;

                fs::set_permissions(&pack, fs::Permissions::from_mode(0o444))
                    .expect("read-only partial pack");
            }
            let error = (self.failure)(request.operation.clone());
            self.requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(request);
            Box::pin(async move { Err(error) })
        }
    }

    /// Accepts HTTP connections and never answers them, like a remote whose link died.
    async fn stalling_remote() -> (String, mpsc::UnboundedReceiver<TcpStream>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("stalling remote listener");
        let url = format!(
            "http://{}/stalled.git",
            listener.local_addr().expect("stalling remote address")
        );
        let (connections, accepted) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                if connections.send(stream).is_err() {
                    break;
                }
            }
        });
        (url, accepted)
    }

    /// Returns once the peer closed the connection; Git holds it until its process exits.
    async fn wait_for_close(stream: &mut TcpStream) {
        let mut buffer = [0_u8; 4096];
        while let Ok(read) = stream.read(&mut buffer).await {
            if read == 0 {
                return;
            }
        }
    }

    async fn wait_until_removed(path: &Path) {
        let deadline = Instant::now() + CLONE_FIXTURE_DEADLINE;
        while path.exists() {
            assert!(
                Instant::now() < deadline,
                "{} was not removed",
                path.display()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn no_proxy() -> [(&'static str, &'static str); 2] {
        [("no_proxy", "*"), ("NO_PROXY", "*")]
    }

    fn git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Clone Test")
            .env("GIT_AUTHOR_EMAIL", "clone@example.test")
            .env("GIT_COMMITTER_NAME", "Clone Test")
            .env("GIT_COMMITTER_EMAIL", "clone@example.test")
            .output()
            .expect("git fixture starts");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn file_url(path: &Path) -> String {
        let normalized = path.to_string_lossy().replace('\\', "/");
        format!("file:///{}", normalized.trim_start_matches('/'))
    }

    /// A repository with one commit, cloneable through a `file://` URL.
    fn source_repository(sandbox: &TestSandbox) -> String {
        let source = sandbox.path("source");
        fs::create_dir(&source).expect("source repository");
        git(&source, &["init", "-q", "-b", "main"]);
        fs::write(source.join("tracked.txt"), "tracked\n").expect("tracked file");
        git(&source, &["add", "tracked.txt"]);
        git(
            &source,
            &[
                "-c",
                "commit.gpgSign=false",
                "commit",
                "-q",
                "-m",
                "initial",
            ],
        );
        file_url(&source)
    }

    fn clone_parent(sandbox: &TestSandbox) -> PathBuf {
        let parent = sandbox.path("clones");
        fs::create_dir(&parent).expect("clone parent");
        parent
    }

    fn network_args(args: &[&str]) -> Vec<OsString> {
        NETWORK_TRANSFER_CONFIG
            .iter()
            .chain(args)
            .map(OsString::from)
            .collect()
    }

    #[tokio::test]
    async fn a_clone_transfer_outliving_the_ordinary_deadline_succeeds() {
        let sandbox = TestSandbox::new("git-clone-slow-transfer");
        let url = source_repository(&sandbox);
        let parent = clone_parent(&sandbox);
        // The transfer takes about a second while the ordinary deadline is shortened to
        // 250 ms, standing in for a large clone against the 30-second read deadline.
        let slow_git = sandbox.executable_script(
            "slow-git",
            "sleep 1\nexec git \"$@\"",
            "@echo off\r\nping -n 2 127.0.0.1 >nul\r\ngit %*\r\nexit /b %ERRORLEVEL%",
        );
        let runner = Arc::new(FixtureGitRunner::new(&[]).with_slow_clone(slow_git));
        let repository = GitRepository::with_runner_for_test(runner.clone())
            .with_command_timeout_for_test(Duration::from_millis(250));

        let ordinary = repository
            .run(
                "GitVcsDriver.clone.control",
                &parent,
                &["clone".into(), "--".into(), url.clone(), "control".into()],
                &CancellationToken::new(),
            )
            .await
            .expect_err("the ordinary deadline stops the slow transfer");
        assert_eq!(ordinary.detail.as_ref(), "Git command timed out.");

        let cloned = repository
            .clone_repository(&url, &parent, Some("slow"), &CancellationToken::new())
            .await
            .expect("the network-transfer budget lets a moving transfer finish");

        assert_eq!(cloned, parent.join("slow"));
        assert_eq!(
            fs::read_to_string(cloned.join("tracked.txt"))
                .expect("cloned file")
                .replace("\r\n", "\n"),
            "tracked\n"
        );
        let request = runner.clone_request();
        assert_eq!(request.args[..4], network_args(&[])[..]);
        assert_eq!(request.timeout, crate::git::NETWORK_TRANSFER_TIMEOUT);
    }

    #[tokio::test]
    async fn a_stalled_clone_fails_with_the_stall_message_and_removes_its_destination() {
        let sandbox = TestSandbox::new("git-clone-stalled");
        let parent = clone_parent(&sandbox);
        let (url, _connections) = stalling_remote().await;
        // Git's environment override shortens the 60-second stall window for the fixture;
        // the production command line still carries the 60-second setting asserted below.
        let mut environment = no_proxy().to_vec();
        environment.push(("GIT_HTTP_LOW_SPEED_TIME", "1"));
        let runner = Arc::new(FixtureGitRunner::new(&environment));
        let repository = GitRepository::with_runner_for_test(runner.clone());

        let error = timeout(
            CLONE_FIXTURE_DEADLINE,
            repository.clone_repository(&url, &parent, Some("stalled"), &CancellationToken::new()),
        )
        .await
        .expect("the stall guard ends the transfer")
        .expect_err("a stalled transfer fails");

        assert_eq!(error.detail.as_ref(), NETWORK_TRANSFER_STALLED);
        assert!(!parent.join("stalled").exists());
        assert!(parent.is_dir());
        let request = runner.clone_request();
        assert_eq!(
            request.args,
            network_args(&["clone", "--", &url, "stalled"])
        );
    }

    #[tokio::test]
    async fn a_cancelled_clone_stops_git_and_removes_its_destination() {
        let sandbox = TestSandbox::new("git-clone-cancelled");
        let parent = clone_parent(&sandbox);
        let destination = parent.join("cancelled");
        let (url, mut connections) = stalling_remote().await;
        let repository =
            GitRepository::with_runner_for_test(Arc::new(FixtureGitRunner::new(&no_proxy())));
        let cancellation = CancellationToken::new();

        let clone = repository.clone_repository(&url, &parent, Some("cancelled"), &cancellation);
        tokio::pin!(clone);
        let mut connection = tokio::select! {
            result = &mut clone => panic!("a stalled clone cannot finish: {result:?}"),
            connection = connections.recv() => connection.expect("Git connects to the remote"),
        };
        assert!(destination.is_dir(), "the clone created its destination");
        cancellation.cancel();
        let error = timeout(CLONE_FIXTURE_DEADLINE, clone)
            .await
            .expect("cancellation ends the clone")
            .expect_err("a cancelled clone fails");

        assert_eq!(error.detail.as_ref(), "Git command was interrupted.");
        assert!(!destination.exists(), "cleanup finished before the result");
        timeout(CLONE_FIXTURE_DEADLINE, wait_for_close(&mut connection))
            .await
            .expect("the Git process holding the connection was stopped");
    }

    #[tokio::test]
    async fn dropping_a_clone_still_stops_git_and_removes_its_destination() {
        let sandbox = TestSandbox::new("git-clone-dropped");
        let parent = clone_parent(&sandbox);
        let destination = parent.join("dropped");
        let (url, mut connections) = stalling_remote().await;
        let repository =
            GitRepository::with_runner_for_test(Arc::new(FixtureGitRunner::new(&no_proxy())));
        let never_cancelled = CancellationToken::new();

        let mut clone =
            Box::pin(repository.clone_repository(&url, &parent, Some("dropped"), &never_cancelled));
        let mut connection = tokio::select! {
            result = &mut clone => panic!("a stalled clone cannot finish: {result:?}"),
            connection = connections.recv() => connection.expect("Git connects to the remote"),
        };
        assert!(destination.is_dir(), "the clone created its destination");
        // An RPC interrupt drops the handler future instead of awaiting it.
        drop(clone);

        timeout(CLONE_FIXTURE_DEADLINE, wait_for_close(&mut connection))
            .await
            .expect("the Git process holding the connection was stopped");
        wait_until_removed(&destination).await;
        assert!(parent.is_dir());
    }

    #[tokio::test]
    async fn clones_into_different_folders_do_not_wait_for_each_other() {
        let sandbox = TestSandbox::new("git-clone-independent-destinations");
        let url = source_repository(&sandbox);
        let parent = clone_parent(&sandbox);
        let (stalled_url, mut connections) = stalling_remote().await;
        let repository =
            GitRepository::with_runner_for_test(Arc::new(FixtureGitRunner::new(&no_proxy())));

        let stalled_cancellation = CancellationToken::new();
        let mut stalled = Box::pin(repository.clone_repository(
            &stalled_url,
            &parent,
            Some("stalled"),
            &stalled_cancellation,
        ));
        let _connection = tokio::select! {
            result = &mut stalled => panic!("a stalled clone cannot finish: {result:?}"),
            connection = connections.recv() => connection.expect("the stalled clone connects"),
        };

        let other_cancellation = CancellationToken::new();
        let cloned = tokio::select! {
            result = &mut stalled => panic!("a stalled clone cannot finish: {result:?}"),
            result = timeout(
                CLONE_FIXTURE_DEADLINE,
                repository.clone_repository(&url, &parent, Some("other"), &other_cancellation),
            ) => result.expect("another folder clones while the first is running"),
        };
        assert_eq!(
            cloned.expect("the other clone succeeds"),
            parent.join("other")
        );

        stalled_cancellation.cancel();
        timeout(CLONE_FIXTURE_DEADLINE, stalled)
            .await
            .expect("cancellation ends the stalled clone")
            .expect_err("the stalled clone was cancelled");
    }

    #[tokio::test]
    async fn timed_out_and_cancelled_transfers_remove_the_partial_clone() {
        fn timed_out(operation: String) -> ProcessError {
            ProcessError::Timeout {
                operation,
                timeout_ms: 1,
            }
        }
        fn cancelled(operation: String) -> ProcessError {
            ProcessError::Cancelled { operation }
        }

        for (failure, detail) in [
            (
                timed_out as fn(String) -> ProcessError,
                "Git command timed out.",
            ),
            (cancelled, "Git command was interrupted."),
        ] {
            let sandbox = TestSandbox::new("git-clone-partial");
            let parent = clone_parent(&sandbox);
            let runner = Arc::new(PartialCloneRunner {
                failure,
                requests: Mutex::new(Vec::new()),
            });
            let repository = GitRepository::with_runner_for_test(runner.clone());
            let url = "https://example.test/repository.git";

            let error = repository
                .clone_repository(url, &parent, Some("partial"), &CancellationToken::new())
                .await
                .expect_err("the transfer failed");

            assert_eq!(error.detail.as_ref(), detail);
            assert!(!parent.join("partial").exists());
            let requests = runner
                .requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            assert_eq!(requests.len(), 1);
            assert_eq!(
                requests[0].args,
                network_args(&["clone", "--", url, "partial"])
            );
            assert_eq!(requests[0].timeout, crate::git::NETWORK_TRANSFER_TIMEOUT);
            assert_ne!(requests[0].timeout, DEFAULT_TIMEOUT);
        }
    }

    #[tokio::test]
    async fn a_failed_clone_removes_only_a_destination_it_created() {
        let sandbox = TestSandbox::new("git-clone-failed");
        let parent = clone_parent(&sandbox);
        let missing = file_url(&sandbox.path("missing.git"));
        let repository = GitRepository::default();

        let created = repository
            .clone_repository(
                &missing,
                &parent,
                Some("created"),
                &CancellationToken::new(),
            )
            .await
            .expect_err("a missing remote cannot be cloned");
        assert!(!created.detail.is_empty());
        assert!(!parent.join("created").exists());

        let existing = parent.join("existing");
        fs::create_dir(&existing).expect("pre-existing folder");
        fs::write(existing.join("notes.txt"), "keep me\n").expect("user file");
        let rejected = repository
            .clone_repository(
                &missing,
                &parent,
                Some("existing"),
                &CancellationToken::new(),
            )
            .await
            .expect_err("an existing folder is never cloned into");
        assert_eq!(
            rejected.detail.as_ref(),
            format!(
                "A folder named existing already exists at {} and is not a Git repository. Remove it or choose another parent folder.",
                display_path(&parent)
            )
        );
        assert_eq!(
            fs::read_to_string(existing.join("notes.txt")).expect("user file survives"),
            "keep me\n"
        );
    }

    #[tokio::test]
    async fn clone_destination_errors_name_the_folder_and_the_next_step() {
        let sandbox = TestSandbox::new("git-clone-destination-errors");
        let parent = clone_parent(&sandbox);
        let url = "https://example.test/repository.git";
        let repository = GitRepository::default();

        fs::write(parent.join("taken"), "a file\n").expect("file in the way");
        let file = repository
            .clone_repository(url, &parent, Some("taken"), &CancellationToken::new())
            .await
            .expect_err("a file is never replaced");
        assert_eq!(
            file.detail.as_ref(),
            format!(
                "A file named taken already exists at {}. Remove it or choose another parent folder.",
                display_path(&parent)
            )
        );
        assert_eq!(
            fs::read_to_string(parent.join("taken")).expect("the file survives"),
            "a file\n"
        );

        let outer = parent.join("outer");
        fs::create_dir(&outer).expect("outer repository");
        git(&outer, &["init", "-q", "-b", "main"]);
        fs::create_dir(outer.join("inner")).expect("folder inside the repository");
        let nested = repository
            .clone_repository(url, &outer, Some("inner"), &CancellationToken::new())
            .await
            .expect_err("a folder inside another repository is not reused");
        assert_eq!(
            nested.detail.as_ref(),
            format!(
                "A folder named inner already exists at {} inside another Git repository. Choose another parent folder.",
                display_path(&outer)
            )
        );
        assert!(outer.join("inner").is_dir());

        // A parent path that runs through a file cannot hold the new folder.
        let blocked_parent = parent.join("taken");
        let blocked = repository
            .clone_repository(
                url,
                &blocked_parent,
                Some("repo"),
                &CancellationToken::new(),
            )
            .await
            .expect_err("the destination cannot be created");
        let prefix = format!(
            "Could not create {} (",
            display_path(&blocked_parent.join("repo"))
        );
        assert!(
            blocked.detail.starts_with(&prefix)
                && blocked.detail.ends_with(
                    "). Check that the parent folder exists and is writable, or choose another folder."
                ),
            "unexpected creation error: {}",
            blocked.detail
        );

        // An existing repository is reused only when its origin is the requested URL.
        let no_origin = parent.join("no-origin");
        fs::create_dir(&no_origin).expect("repository without origin");
        git(&no_origin, &["init", "-q", "-b", "main"]);
        let missing = repository
            .clone_repository(url, &parent, Some("no-origin"), &CancellationToken::new())
            .await
            .expect_err("a repository without origin is not reused");
        assert_eq!(
            missing.detail.as_ref(),
            format!(
                "A Git repository named no-origin already exists at {} without an origin remote. Choose another parent folder, or add that folder as a project instead.",
                display_path(&parent)
            )
        );
        git(
            &no_origin,
            &["remote", "add", "origin", "https://example.test/other.git"],
        );
        let other = repository
            .clone_repository(url, &parent, Some("no-origin"), &CancellationToken::new())
            .await
            .expect_err("a repository with another origin is not reused");
        assert_eq!(
            other.detail.as_ref(),
            format!(
                "A Git repository named no-origin already exists at {} with a different origin. Choose another parent folder.",
                display_path(&parent)
            )
        );
        assert!(no_origin.join(".git").is_dir());
    }

    #[tokio::test]
    async fn reuse_rejects_a_placeholder_or_commitless_clone_and_keeps_it() {
        let sandbox = TestSandbox::new("git-clone-incomplete");
        let parent = clone_parent(&sandbox);
        let url = "https://example.test/repository.git";
        let placeholder = parent.join("placeholder");
        fs::create_dir(&placeholder).expect("placeholder clone");
        git(&placeholder, &["init", "-q", "-b", "main"]);
        git(&placeholder, &["config", "remote.origin.url", url]);
        fs::write(placeholder.join(".git/HEAD"), "ref: refs/heads/.invalid\n")
            .expect("placeholder HEAD");
        fs::create_dir_all(placeholder.join(".git/objects/pack")).expect("pack directory");
        fs::write(
            placeholder.join(".git/objects/pack/tmp_pack_partial"),
            b"partial",
        )
        .expect("partial pack");
        let commitless = parent.join("commitless");
        fs::create_dir(&commitless).expect("commitless clone");
        git(&commitless, &["init", "-q", "-b", "main"]);
        git(&commitless, &["config", "remote.origin.url", url]);
        let repository = GitRepository::default();

        for (name, destination) in [("placeholder", &placeholder), ("commitless", &commitless)] {
            let error = repository
                .clone_repository(url, &parent, Some(name), &CancellationToken::new())
                .await
                .expect_err("an incomplete clone is not adopted");
            assert_eq!(
                error.detail.as_ref(),
                format!(
                    "An incomplete clone exists at {}. Remove it or choose another folder.",
                    display_path(destination)
                )
            );
            assert!(destination.join(".git").is_dir(), "{name} is kept");
        }
        assert!(
            placeholder
                .join(".git/objects/pack/tmp_pack_partial")
                .exists()
        );
    }

    #[tokio::test]
    async fn reuse_refuses_an_index_less_leftover_but_reuses_an_empty_tree_clone() {
        let sandbox = TestSandbox::new("git-clone-index-less");
        let parent = clone_parent(&sandbox);
        let url = source_repository(&sandbox);
        let repository = GitRepository::default();

        // A crash during checkout: HEAD resolves, Git never wrote the index, and a stale lock
        // is left behind. A lock alone may belong to a running Git command, so it is not the test.
        let crashed = parent.join("crashed");
        git(&parent, &["clone", "-q", "--", &url, "crashed"]);
        fs::remove_file(crashed.join(".git/index")).expect("remove the index");
        fs::write(crashed.join(".git/index.lock"), b"").expect("stale index lock");
        let error = repository
            .clone_repository(&url, &parent, Some("crashed"), &CancellationToken::new())
            .await
            .expect_err("an index-less leftover is not adopted");
        assert_eq!(
            error.detail.as_ref(),
            format!(
                "An incomplete clone exists at {}. Remove it or choose another folder.",
                display_path(&crashed)
            )
        );
        assert!(crashed.join(".git").is_dir(), "the leftover is kept");

        // Git writes the index even for an empty tree, so such a clone is still reused.
        let empty_source = sandbox.path("empty-source");
        fs::create_dir(&empty_source).expect("empty source repository");
        git(&empty_source, &["init", "-q", "-b", "main"]);
        git(
            &empty_source,
            &[
                "-c",
                "commit.gpgSign=false",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "empty",
            ],
        );
        let empty_url = file_url(&empty_source);
        git(&parent, &["clone", "-q", "--", &empty_url, "empty"]);
        assert!(parent.join("empty/.git/index").is_file());
        let reused = repository
            .clone_repository(
                &empty_url,
                &parent,
                Some("empty"),
                &CancellationToken::new(),
            )
            .await
            .expect("an empty-tree clone is reused");
        assert_eq!(reused, parent.join("empty"));
    }

    /// A validated folder name for tests that reserve or inspect a destination directly.
    fn leaf(name: &str) -> super::CloneLeaf {
        super::clone_destination_leaf("https://example.test/unused.git", Some(name))
            .expect("a single folder name")
    }

    #[test]
    fn the_destination_leaf_is_the_directory_name_or_the_url_s_last_segment() {
        use super::clone_destination_leaf;
        let leaf_of = |url: &str, name: Option<&str>| {
            clone_destination_leaf(url, name)
                .expect("a single folder name")
                .as_str()
                .to_owned()
        };
        assert_eq!(leaf_of("https://example.test/org/demo.git/", None), "demo");
        assert_eq!(leaf_of("git@example.test:org/demo.git", None), "demo");
        assert_eq!(leaf_of(r"C:\repos\demo", None), "demo");
        assert_eq!(
            leaf_of("https://example.test/org/demo.git", Some("custom")),
            "custom"
        );
        assert_eq!(
            leaf_of("https://example.test/org/demo.git.git", None),
            "demo"
        );
    }

    #[test]
    fn the_destination_leaf_must_be_a_single_folder_name() {
        use super::{CloneLeafError, clone_destination_leaf};

        let url = "https://example.test/org/demo.git";
        for name in [
            ".", "..", "../x", "a/b", "a/../b", "/abs", "repo/", "x/.", "",
        ] {
            let error =
                clone_destination_leaf(url, Some(name)).expect_err("a path is not a folder name");
            assert_eq!(error, CloneLeafError::InvalidName(name.to_owned()));
            assert_eq!(
                error.to_string(),
                format!(
                    "The folder name \"{name}\" must be a single name, without slashes or \"..\"."
                )
            );
        }
        for nameless in [
            "https://user:secret@example.test/org/.git",
            "https://example.test/..",
            "https://example.test/.",
        ] {
            let error = clone_destination_leaf(nameless, None).expect_err("no usable folder name");
            assert_eq!(error, CloneLeafError::Underivable);
            assert!(
                !error.to_string().contains("example.test"),
                "never echoes the URL: {error}"
            );
            assert!(
                !error.to_string().contains("secret"),
                "never echoes the URL: {error}"
            );
        }
        #[cfg(windows)]
        for name in [r"C:x", r"a\b", r"\\?\C:\x"] {
            clone_destination_leaf(url, Some(name))
                .expect_err("a Windows path is not a folder name");
        }
    }

    #[tokio::test]
    async fn reserving_a_finished_clone_returns_it_without_a_transfer() {
        let sandbox = TestSandbox::new("git-clone-reserve-existing");
        let parent = clone_parent(&sandbox);
        let url = source_repository(&sandbox);
        git(&parent, &["clone", "-q", "--", &url, "finished"]);
        let runner = Arc::new(FixtureGitRunner::new(&[]));
        let repository = GitRepository::with_runner_for_test(runner.clone());

        let reservation = repository
            .reserve_clone_destination(&url, &parent, &leaf("finished"), &CancellationToken::new())
            .await
            .expect("a finished clone is reused");
        let super::CloneReservation::Existing(path) = reservation else {
            panic!("a finished clone must not be reserved again");
        };
        assert_eq!(path, parent.join("finished"));
        assert!(
            runner
                .requests
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .iter()
                .all(|request| request.operation != super::CLONE_OPERATION),
            "reuse runs no transfer"
        );
    }

    #[tokio::test]
    async fn inspecting_a_destination_reports_nothing_a_finished_clone_or_an_incomplete_one() {
        let sandbox = TestSandbox::new("git-clone-inspect");
        let parent = clone_parent(&sandbox);
        let url = source_repository(&sandbox);
        let repository = GitRepository::default();
        let token = CancellationToken::new();

        assert_eq!(
            repository
                .inspect_clone_destination(&url, &parent, &leaf("absent"), &token)
                .await
                .expect("an absent destination is not an error"),
            None
        );
        assert!(
            !parent.join("absent").exists(),
            "inspecting never creates the folder"
        );

        git(&parent, &["clone", "-q", "--", &url, "finished"]);
        assert_eq!(
            repository
                .inspect_clone_destination(&url, &parent, &leaf("finished"), &token)
                .await
                .expect("a finished clone is found"),
            Some(parent.join("finished"))
        );

        let incomplete = parent.join("incomplete");
        fs::create_dir(&incomplete).expect("incomplete clone");
        git(&incomplete, &["init", "-q", "-b", "main"]);
        git(&incomplete, &["config", "remote.origin.url", &url]);
        let error = repository
            .inspect_clone_destination(&url, &parent, &leaf("incomplete"), &token)
            .await
            .expect_err("an incomplete clone is reported");
        assert!(error.detail.contains("An incomplete clone exists at"));
        assert!(
            incomplete.join(".git").is_dir(),
            "inspecting keeps the folder"
        );
    }

    #[tokio::test]
    async fn an_invalid_folder_name_is_refused_before_anything_is_created() {
        let sandbox = TestSandbox::new("git-clone-invalid-leaf");
        let parent = clone_parent(&sandbox);
        let url = source_repository(&sandbox);
        let repository = GitRepository::default();

        let error = repository
            .clone_repository(&url, &parent, Some("a/../b"), &CancellationToken::new())
            .await
            .expect_err("a path is not a folder name");
        assert_eq!(
            error.detail.as_ref(),
            "The folder name \"a/../b\" must be a single name, without slashes or \"..\"."
        );
        assert!(
            !parent.join("a").exists(),
            "no folder is created for the name's first part"
        );
        assert!(
            !parent.join("b").exists(),
            "nothing is cloned where the path leads"
        );
    }
}
