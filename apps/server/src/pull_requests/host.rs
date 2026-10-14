//! The only process boundary for hosted pull request operations.

use std::{
    ffi::{OsStr, OsString},
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use super::{error::PullRequestsOperationError, model::*};

use tokio_util::sync::CancellationToken;

use crate::{
    git::{OutputPolicy, ProcessError, ProcessRequest, ProcessRunner, git_environment},
    source_control::{ProviderCommandSpec, ProviderHosts, ProviderKind, provider_cli_environment},
};

/// One admitted repository: every hosted read and write runs against this scope.
#[derive(Clone, Debug)]
pub struct HostScope {
    pub cwd: PathBuf,
    pub host: String,
    pub repository: String,
    /// Decided once, when the scope is admitted (`context::scope_from_remote` or
    /// CLI discovery); an unsupported provider never becomes a scope.
    pub provider: PullRequestsProvider,
}

impl HostScope {
    pub(super) fn context_key(&self) -> (String, String) {
        (self.host.to_ascii_lowercase(), self.repository.clone())
    }
}

type ProbeKey = (bool, String, String, Vec<OsString>);

#[derive(Clone, Debug)]
pub struct HostCommandRunner {
    runner: ProcessRunner,
    gh: ProviderCommandSpec,
    glab: ProviderCommandSpec,
    git: ProviderCommandSpec,
    state_dir: PathBuf,
    probes: Arc<super::cache::ContextCache<ProbeKey, CommandOutput>>,
    /// The server's host observation: scope resolution records and forgets hosts here.
    /// `new` starts with a private one, so the runner records and forgets hosts for
    /// itself only: status never sees them and Settings discovery never helps it,
    /// while resolution still works through CLI discovery. Tests rely on that
    /// isolation; the server shares one observation through `with_provider_hosts`
    /// (`production/runtime.rs`).
    provider_hosts: Arc<ProviderHosts>,
}

#[cfg(test)]
pub(super) fn command_specs_for_test(runner: &HostCommandRunner) -> [&ProviderCommandSpec; 3] {
    [&runner.gh, &runner.glab, &runner.git]
}

#[derive(Clone, Copy, Debug)]
pub enum Budget {
    Read,
    Large,
    Mutation,
    CheckoutWrite,
}

impl Budget {
    pub fn timeout(self) -> Duration {
        match self {
            Self::Read => Duration::from_secs(30),
            Self::Large | Self::Mutation => Duration::from_secs(60),
            Self::CheckoutWrite => crate::git::CHECKOUT_WRITE_TIMEOUT,
        }
    }

    fn output_limit(self) -> usize {
        match self {
            Self::Large => 8 * 1024 * 1024,
            Self::Read | Self::Mutation | Self::CheckoutWrite => 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

#[derive(Debug)]
pub struct ProcessFailure {
    pub error: ProcessError,
    pub stderr: String,
}

impl ProcessFailure {
    pub fn into_output(self) -> Result<CommandOutput, Self> {
        match self.error {
            ProcessError::NonZeroExit {
                stdout,
                stderr,
                exit_code,
                ..
            } => Ok(CommandOutput {
                stdout: stdout.into(),
                stderr: stderr.into(),
                exit_code,
            }),
            _ => Err(self),
        }
    }
}

impl HostCommandRunner {
    #[must_use]
    pub fn new(state_dir: PathBuf) -> Self {
        Self {
            runner: ProcessRunner,
            gh: ProviderCommandSpec::new("gh", []),
            glab: ProviderCommandSpec::new("glab", []),
            git: ProviderCommandSpec::new("git", []),
            state_dir,
            probes: Arc::default(),
            provider_hosts: Arc::default(),
        }
    }

    /// Shares the host observation that status reads and Settings discovery also use.
    #[must_use]
    pub fn with_provider_hosts(mut self, provider_hosts: Arc<ProviderHosts>) -> Self {
        self.provider_hosts = provider_hosts;
        self
    }

    pub(super) fn provider_hosts(&self) -> &ProviderHosts {
        &self.provider_hosts
    }

    #[must_use]
    pub fn with_commands(
        mut self,
        gh: impl Into<PathBuf>,
        glab: impl Into<PathBuf>,
        git: impl Into<PathBuf>,
    ) -> Self {
        self.gh = ProviderCommandSpec::new(gh, []);
        self.glab = ProviderCommandSpec::new(glab, []);
        self.git = ProviderCommandSpec::new(git, []);
        self.probes.clear();
        self
    }

    pub async fn gh(
        &self,
        scope: &HostScope,
        args: &[impl AsRef<OsStr>],
        budget: Budget,
        stdin: Option<&[u8]>,
        c: &CancellationToken,
    ) -> Result<CommandOutput, ProcessFailure> {
        self.execute(
            self.provider_request(scope, PullRequestsProvider::Github, args, budget, stdin),
            c,
        )
        .await
    }

    pub async fn glab(
        &self,
        scope: &HostScope,
        args: &[impl AsRef<OsStr>],
        budget: Budget,
        stdin: Option<&[u8]>,
        c: &CancellationToken,
    ) -> Result<CommandOutput, ProcessFailure> {
        self.execute(
            self.provider_request(scope, PullRequestsProvider::Gitlab, args, budget, stdin),
            c,
        )
        .await
    }

    /// Discovery and presence/auth probes retain the read cap with a shorter deadline.
    /// The probe runs the scope's provider CLI.
    pub(super) async fn probe(
        &self,
        scope: &HostScope,
        args: &[impl AsRef<OsStr>],
        c: &CancellationToken,
    ) -> Result<CommandOutput, ProcessFailure> {
        let mut request = self.provider_request(scope, scope.provider, args, Budget::Read, None);
        request.timeout = Duration::from_secs(5);
        let key = (
            scope.provider == PullRequestsProvider::Github,
            scope.host.to_ascii_lowercase(),
            scope.repository.clone(),
            request.args.clone(),
        );
        self.probes
            .get_or_load(
                key,
                c,
                || ProcessFailure {
                    error: ProcessError::Cancelled {
                        operation: "pullRequests.probe".into(),
                    },
                    stderr: String::new(),
                },
                self.execute(request, c),
            )
            .await
    }

    pub(super) fn clear_context_probes(&self) {
        self.probes.clear();
    }

    pub async fn git(
        &self,
        cwd: &Path,
        args: &[impl AsRef<OsStr>],
        c: &CancellationToken,
    ) -> Result<CommandOutput, ProcessFailure> {
        self.git_with_budget(cwd, args, Duration::from_secs(10), c)
            .await
    }

    pub(super) async fn git_with_budget(
        &self,
        cwd: &Path,
        args: &[impl AsRef<OsStr>],
        timeout: Duration,
        c: &CancellationToken,
    ) -> Result<CommandOutput, ProcessFailure> {
        self.execute(
            ProcessRequest {
                operation: "pullRequests.git".into(),
                command: self.git.executable.clone(),
                args: self
                    .git
                    .args(args.iter().map(|arg| arg.as_ref().to_owned())),
                cwd: cwd.into(),
                env: git_environment(),
                stdin: None,
                timeout,
                max_output_bytes: Budget::Read.output_limit(),
                output_policy: OutputPolicy::Error,
                append_truncation_marker: false,
                allow_non_zero_exit: true,
            },
            c,
        )
        .await
    }

    pub async fn glab_api_with_body(
        &self,
        scope: &HostScope,
        method: &str,
        path: &str,
        body: &serde_json::Value,
        c: &CancellationToken,
    ) -> Result<CommandOutput, ProcessFailure> {
        let private_file = crate::source_control::PrivateJsonBody::write(
            &self.state_dir.join("pull-requests"),
            body,
        )
        .await
        .map_err(body_io_error)?;
        self.glab(
            scope,
            &[
                OsStr::new("api"),
                OsStr::new("--method"),
                OsStr::new(method),
                OsStr::new(path),
                OsStr::new("-H"),
                OsStr::new("Content-Type: application/json"),
                OsStr::new("--input"),
                private_file.path().as_os_str(),
            ],
            Budget::Mutation,
            None,
            c,
        )
        .await
    }

    fn provider_request(
        &self,
        scope: &HostScope,
        provider: PullRequestsProvider,
        args: &[impl AsRef<OsStr>],
        budget: Budget,
        stdin: Option<&[u8]>,
    ) -> ProcessRequest {
        let mut args: Vec<OsString> = args.iter().map(|arg| arg.as_ref().to_owned()).collect();
        let (github, cli, command) = match provider {
            PullRequestsProvider::Github => (true, "gh", &self.gh),
            PullRequestsProvider::Gitlab => (false, "glab", &self.glab),
        };
        if args
            .first()
            .is_some_and(|arg| arg == if github { "pr" } else { "mr" })
        {
            args.extend([
                "--repo".into(),
                format!("{}/{}", scope.host, scope.repository).into(),
            ]);
        } else if !github && args.first().is_some_and(|arg| arg == "api") {
            args.extend(["--hostname".into(), scope.host.clone().into()]);
        }
        let mut env: Vec<(OsString, OsString)> = vec![("NO_COLOR".into(), "1".into())];
        if github {
            env.extend([
                ("GH_HOST".into(), scope.host.clone().into()),
                ("GH_PROMPT_DISABLED".into(), "1".into()),
                ("GH_NO_UPDATE_NOTIFIER".into(), "1".into()),
                ("CLICOLOR".into(), "0".into()),
            ]);
        } else {
            env.extend([
                ("GITLAB_HOST".into(), scope.host.clone().into()),
                ("GLAB_CHECK_UPDATE".into(), "false".into()),
            ]);
        }
        env.extend(provider_cli_environment(provider.kind()));
        ProcessRequest {
            operation: format!("pullRequests.{cli}"),
            command: command.executable.clone(),
            args: command.args(args),
            cwd: scope.cwd.clone(),
            env,
            stdin: stdin.map(<[u8]>::to_vec),
            timeout: budget.timeout(),
            max_output_bytes: budget.output_limit(),
            output_policy: OutputPolicy::Error,
            append_truncation_marker: false,
            allow_non_zero_exit: true,
        }
    }

    async fn execute(
        &self,
        request: ProcessRequest,
        c: &CancellationToken,
    ) -> Result<CommandOutput, ProcessFailure> {
        let operation = request.operation.clone();
        let output = self
            .runner
            .run(request, c)
            .await
            .map_err(|error| ProcessFailure {
                error,
                stderr: String::new(),
            })?;
        if output.exit_code != 0 {
            return Err(ProcessFailure {
                error: ProcessError::NonZeroExit {
                    operation,
                    exit_code: output.exit_code,
                    stdout_length: output.stdout.len(),
                    stderr_length: output.stderr.len(),
                    stdout: output.stdout.into(),
                    stderr: output.stderr.clone().into(),
                },
                stderr: output.stderr,
            });
        }
        Ok(CommandOutput {
            stdout: output.stdout,
            stderr: output.stderr,
            exit_code: output.exit_code,
        })
    }
}

fn body_io_error(source: std::io::Error) -> ProcessFailure {
    ProcessFailure {
        error: ProcessError::Stdin {
            operation: "pullRequests.bodyFile".into(),
            source,
        },
        stderr: String::new(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct HostVersion {
    pub major: u64,
    pub minor: u64,
}

impl HostVersion {
    pub fn parse(version: &str) -> Option<Self> {
        let mut parts = version.split('.');
        Some(Self {
            major: parts.next()?.parse().ok()?,
            minor: parts.next()?.parse().ok()?,
        })
    }
}

#[derive(Clone, Debug)]
pub struct HostContext {
    pub provider: PullRequestsProvider,
    pub host: String,
    pub host_version: Option<String>,
    pub repository: String,
    pub default_branch: String,
    pub account: Actor,
    pub repository_permission: RepositoryPermission,
    pub merge_policy: MergePolicy,
    pub web_url: String,
    pub raw_host_version: Option<String>,
    pub gitlab_access_level: Option<u32>,
}

impl HostContext {
    pub fn version(&self) -> Option<HostVersion> {
        self.raw_host_version
            .as_deref()
            .and_then(HostVersion::parse)
    }

    /// Phase 03 permission reasons consume this; the Phase 00 context wire
    /// contract has no reason field on its capability booleans.
    pub fn version_unavailable_reason(&self) -> Option<&'static str> {
        (self.provider == PullRequestsProvider::Gitlab && self.version().is_none())
            .then_some("GitLab version could not be read")
    }
}

pub use super::model::{ActionRequest, ActionResult, DetailRaw};

pub type HostFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, PullRequestsOperationError>> + Send + 'a>>;

pub trait PullRequestHost: Send + Sync {
    fn invalidate_context(&self) {}
    /// A successful mutation can change the repository-wide list totals.
    fn invalidate_totals(&self, _scope: &HostScope) {}
    fn kind(&self) -> ProviderKind;
    fn capabilities(&self, version: Option<&HostVersion>) -> HostCapabilities;
    fn context<'a>(
        &'a self,
        scope: &'a HostScope,
        c: &'a CancellationToken,
    ) -> HostFuture<'a, HostContext>;
    fn vocabulary<'a>(
        &'a self,
        scope: &'a HostScope,
        kind: VocabularyKind,
        query: Option<&'a str>,
        c: &'a CancellationToken,
    ) -> HostFuture<'a, Vocabulary>;
    /// Hosts without per-request defaults return none.
    fn create_defaults<'a>(
        &'a self,
        _scope: &'a HostScope,
        _c: &'a CancellationToken,
    ) -> HostFuture<'a, CreateDefaults> {
        Box::pin(async { Ok(CreateDefaults::default()) })
    }
    fn list<'a>(
        &'a self,
        scope: &'a HostScope,
        query: &'a ListQuery,
        c: &'a CancellationToken,
    ) -> HostFuture<'a, ListPage>;
    fn detail<'a>(
        &'a self,
        _scope: &'a HostScope,
        _number: u64,
        _context: &'a HostContext,
        _c: &'a CancellationToken,
    ) -> HostFuture<'a, DetailRaw> {
        unavailable("pullRequests.get")
    }

    fn action_detail<'a>(
        &'a self,
        scope: &'a HostScope,
        number: u64,
        context: &'a HostContext,
        c: &'a CancellationToken,
    ) -> HostFuture<'a, super::model::ActionDetail> {
        Box::pin(async move { self.detail(scope, number, context, c).await.map(Into::into) })
    }
    fn timeline<'a>(
        &'a self,
        _scope: &'a HostScope,
        _number: u64,
        _c: &'a CancellationToken,
    ) -> HostFuture<'a, Timeline> {
        unavailable("pullRequests.getTimeline")
    }
    fn commits<'a>(
        &'a self,
        _scope: &'a HostScope,
        _number: u64,
        _c: &'a CancellationToken,
    ) -> HostFuture<'a, Commits> {
        unavailable("pullRequests.getCommits")
    }
    fn checks<'a>(
        &'a self,
        _scope: &'a HostScope,
        _number: u64,
        _c: &'a CancellationToken,
    ) -> HostFuture<'a, Checks> {
        unavailable("pullRequests.getChecks")
    }
    fn files<'a>(
        &'a self,
        _scope: &'a HostScope,
        _number: u64,
        _c: &'a CancellationToken,
    ) -> HostFuture<'a, Files> {
        unavailable("pullRequests.getFiles")
    }
    fn run_action<'a>(
        &'a self,
        _scope: &'a HostScope,
        _action: &'a ActionRequest,
        _context: &'a ActionContext,
        _c: &'a CancellationToken,
    ) -> HostFuture<'a, ActionResult> {
        unavailable("pullRequests.runAction")
    }
    fn head_branch<'a>(
        &'a self,
        scope: &'a HostScope,
        number: u64,
        c: &'a CancellationToken,
    ) -> HostFuture<'a, String> {
        Box::pin(async move {
            let context = self.context(scope, c).await?;
            Ok(self
                .detail(scope, number, &context, c)
                .await?
                .detail_without_permissions
                .head_branch)
        })
    }
    fn head_ref_spec(&self, number: u64) -> String;
}

fn unavailable<'a, T: 'a>(operation: &'static str) -> HostFuture<'a, T> {
    Box::pin(async move { Err(PullRequestsOperationError::new(operation, "unavailable")) })
}

#[cfg(test)]
mod trait_tests {
    use super::*;

    struct ReadOnlyHost;
    impl PullRequestHost for ReadOnlyHost {
        fn kind(&self) -> ProviderKind {
            ProviderKind::Github
        }
        fn capabilities(&self, _: Option<&HostVersion>) -> HostCapabilities {
            unreachable!()
        }
        fn context<'a>(
            &'a self,
            _: &'a HostScope,
            _: &'a CancellationToken,
        ) -> HostFuture<'a, HostContext> {
            unreachable!()
        }
        fn vocabulary<'a>(
            &'a self,
            _: &'a HostScope,
            _: VocabularyKind,
            _: Option<&'a str>,
            _: &'a CancellationToken,
        ) -> HostFuture<'a, Vocabulary> {
            unreachable!()
        }
        fn list<'a>(
            &'a self,
            _: &'a HostScope,
            _: &'a ListQuery,
            _: &'a CancellationToken,
        ) -> HostFuture<'a, ListPage> {
            unreachable!()
        }
        fn head_ref_spec(&self, number: u64) -> String {
            format!("refs/pull/{number}/head")
        }
    }

    #[tokio::test]
    async fn pull_requests_trait_is_object_safe_and_later_operations_are_unavailable() {
        let host: &dyn PullRequestHost = &ReadOnlyHost;
        let scope = HostScope {
            cwd: PathBuf::new(),
            host: "github.com".into(),
            repository: "example/repository".into(),
            provider: PullRequestsProvider::Github,
        };
        let c = CancellationToken::new();
        let context = HostContext {
            provider: PullRequestsProvider::Github,
            host: "github.com".into(),
            host_version: None,
            repository: "example/repository".into(),
            default_branch: "main".into(),
            account: Actor {
                login: "viewer".into(),
                name: None,
                is_bot: false,
            },
            repository_permission: RepositoryPermission::Read,
            merge_policy: MergePolicy {
                methods: vec![],
                default_method: None,
                delete_branch_default: false,
                auto_merge_allowed: false,
                requires_pipeline_success: false,
                requires_resolved_discussions: false,
            },
            web_url: "https://github.com/example/repository".into(),
            raw_host_version: None,
            gitlab_access_level: None,
        };
        let failures: Vec<PullRequestsOperationError> = vec![
            host.detail(&scope, 1, &context, &c).await.unwrap_err(),
            host.timeline(&scope, 1, &c).await.unwrap_err(),
            host.commits(&scope, 1, &c).await.unwrap_err(),
            host.checks(&scope, 1, &c).await.unwrap_err(),
            host.files(&scope, 1, &c).await.unwrap_err(),
            host.run_action(
                &scope,
                &ActionRequest::Close {
                    target: ActionTarget {
                        cwd: String::new(),
                        number: 1,
                    },
                },
                &ActionContext::default(),
                &c,
            )
            .await
            .unwrap_err(),
        ];
        for failure in failures {
            assert_eq!(failure.code, "unavailable");
            assert_eq!(failure.message, "Not implemented in this server build.");
        }
    }

    #[test]
    fn pull_requests_version_parses_major_minor_without_guessing() {
        assert_eq!(
            HostVersion::parse("17.9.1-ee"),
            Some(HostVersion {
                major: 17,
                minor: 9
            })
        );
        assert!(HostVersion::parse("unknown").is_none());
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_support::TestSandbox;
    use std::os::unix::fs::PermissionsExt;

    fn scope(sandbox: &TestSandbox) -> HostScope {
        HostScope {
            cwd: sandbox.root().into(),
            host: "git.acme.example".into(),
            repository: "team/sub/repo".into(),
            provider: PullRequestsProvider::Gitlab,
        }
    }

    #[tokio::test]
    async fn pull_requests_runner_pins_github_environment_and_repository() {
        let s = TestSandbox::new("pr-gh-runner");
        let script = s.executable_script("gh", "printf '%s\\n' \"$GH_HOST|$GH_PROMPT_DISABLED|$GH_NO_UPDATE_NOTIFIER|$NO_COLOR|$CLICOLOR|$*\"", "");
        let runner =
            HostCommandRunner::new(s.path("state")).with_commands(&script, &script, &script);
        let c = CancellationToken::new();
        let output = runner
            .gh(&scope(&s), &["pr", "list"], Budget::Read, None, &c)
            .await
            .unwrap();
        assert_eq!(
            output.stdout.trim(),
            "git.acme.example|1|1|1|0|pr list --repo git.acme.example/team/sub/repo"
        );
        let output = runner
            .gh(&scope(&s), &["api", "user"], Budget::Read, None, &c)
            .await
            .unwrap();
        assert!(!output.stdout.contains("--repo"));
    }

    #[tokio::test]
    async fn pull_requests_runner_pins_gitlab_api_and_mr_without_invalid_global_flag() {
        let s = TestSandbox::new("pr-glab-runner");
        let script = s.executable_script(
            "glab",
            "printf '%s\\n' \"$GITLAB_HOST|$GLAB_CHECK_UPDATE|$GLAB_SEND_TELEMETRY|$NO_COLOR|$*\"",
            "",
        );
        let runner =
            HostCommandRunner::new(s.path("state")).with_commands(&script, &script, &script);
        let c = CancellationToken::new();
        let api = runner
            .glab(&scope(&s), &["api", "user"], Budget::Read, None, &c)
            .await
            .unwrap();
        assert_eq!(
            api.stdout.trim(),
            "git.acme.example|false|0|1|api user --hostname git.acme.example"
        );
        let mr = runner
            .glab(&scope(&s), &["mr", "list"], Budget::Large, None, &c)
            .await
            .unwrap();
        assert_eq!(
            mr.stdout.trim(),
            "git.acme.example|false|0|1|mr list --repo git.acme.example/team/sub/repo"
        );
    }

    #[tokio::test]
    async fn pull_requests_runner_enforces_timeout_output_cap_and_cancellation() {
        let s = TestSandbox::new("pr-runner-bounds");
        let script = s.executable_script("gh", "sleep 2", "");
        let runner =
            HostCommandRunner::new(s.path("state")).with_commands(&script, &script, &script);
        let c = CancellationToken::new();
        let mut request = runner.provider_request(
            &scope(&s),
            PullRequestsProvider::Github,
            &["pr", "list"],
            Budget::Read,
            None,
        );
        request.timeout = Duration::from_millis(100);
        let error = runner.execute(request, &c).await.unwrap_err();
        assert!(matches!(error.error, ProcessError::Timeout { .. }));
        c.cancel();
        assert!(matches!(
            runner
                .gh(&scope(&s), &["pr", "list"], Budget::Read, None, &c)
                .await
                .unwrap_err()
                .error,
            ProcessError::Cancelled { .. }
        ));
        let script = s.executable_script("large", "printf 'oversized'", "");
        let runner = runner.with_commands(&script, &script, &script);
        let mut request = runner.provider_request(
            &scope(&s),
            PullRequestsProvider::Github,
            &["api", "user"],
            Budget::Read,
            None,
        );
        request.max_output_bytes = 4;
        assert!(matches!(
            runner
                .execute(request, &CancellationToken::new())
                .await
                .unwrap_err()
                .error,
            ProcessError::OutputLimit { .. }
        ));
    }

    #[tokio::test]
    async fn pull_requests_gh_body_helpers_send_json_and_plain_text_only_on_stdin() {
        let s = TestSandbox::new("pr-gh-body");
        let script = s.executable_script(
            "gh",
            "printf '%s\\n' \"$@\" > argv\ncat > received-stdin\necho '{}'",
            "",
        );
        let runner =
            HostCommandRunner::new(s.path("state")).with_commands(&script, &script, &script);
        let body = "private review text with quotes ' and unicode é";
        let json_body = serde_json::json!({"body":body}).to_string();
        for (args, input) in [
            (
                vec![
                    "api",
                    "--method",
                    "POST",
                    "repos/team/repo/pulls/1/reviews",
                    "--input",
                    "-",
                ],
                json_body.as_str(),
            ),
            (vec!["pr", "comment", "1", "--body-file", "-"], body),
        ] {
            runner
                .gh(
                    &scope(&s),
                    &args,
                    Budget::Mutation,
                    Some(input.as_bytes()),
                    &CancellationToken::new(),
                )
                .await
                .unwrap();
            assert_eq!(
                std::fs::read_to_string(s.path("received-stdin")).unwrap(),
                input
            );
            let argv = std::fs::read_to_string(s.path("argv")).unwrap();
            assert!(!argv.contains(body));
            assert!(!argv.lines().any(|arg| arg == "--body"));
        }
    }

    #[tokio::test]
    async fn pull_requests_context_probes_reuse_success_without_background_processes() {
        let s = TestSandbox::new("pr-probe-cache");
        let script =
            s.executable_script("gh", "echo called >> calls\necho 'gh version 2.97.0'", "");
        let runner =
            HostCommandRunner::new(s.path("state")).with_commands(&script, &script, &script);
        let c = CancellationToken::new();
        let github = HostScope {
            provider: PullRequestsProvider::Github,
            ..scope(&s)
        };
        for _ in 0..2 {
            runner.probe(&github, &["--version"], &c).await.unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(s.path("calls"))
                .unwrap()
                .lines()
                .count(),
            1
        );
        c.cancel();
        assert!(
            runner.probe(&github, &["--version"], &c).await.is_err(),
            "cached probes still respect cancellation"
        );
    }

    #[tokio::test]
    async fn pull_requests_glab_private_json_bodies_declare_content_type_for_rest_and_graphql() {
        let s = TestSandbox::new("pr-glab-json-content-type");
        let script = s.executable_script(
            "glab",
            r#"printf '%s\n' "$@" > argv
if [ "$5" != -H ] || [ "$6" != 'Content-Type: application/json' ] || [ "$7" != --input ]; then
  printf '%s\n' '{"error":"The provided content-type is not supported."}' >&2
  exit 1
fi
cat "$8"
"#,
            "",
        );
        let runner =
            HostCommandRunner::new(s.path("state")).with_commands(&script, &script, &script);
        for (method, endpoint, body) in [
            (
                "POST",
                "projects/team%2Frepo/merge_requests/14/notes",
                serde_json::json!({"body":"private note"}),
            ),
            (
                "PUT",
                "projects/team%2Frepo/merge_requests/14",
                serde_json::json!({"description":"private description"}),
            ),
            (
                "POST",
                "graphql",
                serde_json::json!({"query":"query Request($iid: String!) { project(fullPath: \"team/repo\") { mergeRequest(iid: $iid) { title } } }", "variables":{"iid":"14"}}),
            ),
        ] {
            let result = runner
                .glab_api_with_body(
                    &scope(&s),
                    method,
                    endpoint,
                    &body,
                    &CancellationToken::new(),
                )
                .await
                .expect("REST and GraphQL JSON bodies must declare their content type");
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&result.stdout).unwrap(),
                body
            );
            let argv = std::fs::read_to_string(s.path("argv")).unwrap();
            let args: Vec<_> = argv.lines().collect();
            assert_eq!(
                &args[..7],
                &[
                    "api",
                    "--method",
                    method,
                    endpoint,
                    "-H",
                    "Content-Type: application/json",
                    "--input"
                ]
            );
            assert_eq!(&args[8..], &["--hostname", "git.acme.example"]);
            assert!(
                !Path::new(args[7]).exists(),
                "private body is removed after the call"
            );
            assert!(!argv.contains("private note") && !argv.contains("private description"));
            assert!(
                !argv.contains("query Request"),
                "GraphQL stays in the private body file"
            );
        }
    }

    #[tokio::test]
    async fn pull_requests_body_file_is_private_and_removed_on_success_and_failure() {
        let s = TestSandbox::new("pr-body-file");
        let script = s.executable_script("glab", "printf '%s\\n' \"$@\" > argv\nwhile [ \"$1\" != --input ]; do shift; done\nprintf '%s' \"$2\" > body-path\ncat \"$2\" > received-body\ncat \"$2\"\nsleep 0.2\nif [ -e fail ]; then exit 1; fi", "");
        let runner =
            HostCommandRunner::new(s.path("state")).with_commands(&script, &script, &script);
        for fail in [false, true] {
            if fail {
                std::fs::write(s.path("fail"), "").unwrap();
            }
            let _ = std::fs::remove_file(s.path("body-path"));
            let scope = scope(&s);
            let c = CancellationToken::new();
            let body = serde_json::json!({ "body": "private body never in argv" });
            let call =
                runner.glab_api_with_body(&scope, "POST", "projects/team%2Frepo/notes", &body, &c);
            let inspect = async {
                let path = tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        if let Ok(path) = std::fs::read_to_string(s.path("body-path"))
                            && !path.is_empty()
                        {
                            break PathBuf::from(path);
                        }
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await
                .unwrap();
                assert!(path.starts_with(s.path("state/pull-requests")));
                assert_eq!(
                    std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
                path
            };
            let (result, path) = tokio::join!(call, inspect);
            assert_eq!(result.is_err(), fail);
            assert!(!path.exists());
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(
                    &std::fs::read_to_string(s.path("received-body")).unwrap()
                )
                .unwrap(),
                body
            );
            let argv = std::fs::read_to_string(s.path("argv")).unwrap();
            assert!(!argv.contains("private body never in argv"));
            assert!(!argv.lines().any(|arg| arg == "--body"));
        }
    }

    #[tokio::test]
    async fn pull_requests_git_uses_existing_noninteractive_environment() {
        let s = TestSandbox::new("pr-git-env");
        let script = s.executable_script(
            "git",
            "printf '%s' \"$GIT_TERMINAL_PROMPT|$GCM_INTERACTIVE|$SSH_ASKPASS_REQUIRE\"",
            "",
        );
        let runner =
            HostCommandRunner::new(s.path("state")).with_commands(&script, &script, &script);
        let output = runner
            .git(
                s.root(),
                &["remote", "get-url", "origin"],
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(output.stdout, "0|never|never");
    }
}
