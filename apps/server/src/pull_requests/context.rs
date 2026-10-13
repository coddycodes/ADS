//! Resolve each explicit read against its origin and the CLI's host configuration.

use std::{
    future::Future,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use tokio_util::sync::CancellationToken;

use crate::{
    git::ProcessError,
    source_control::{
        IdentifiedProvider, ProviderKind, parse_github_auth_status, parse_gitlab_auth_status,
        provider_install_hint, remote_host, remote_repository_path,
    },
};

use super::{
    error::{CODES, PullRequestsOperationError, from_process_error},
    host::{HostCommandRunner, HostScope, ProcessFailure, PullRequestHost},
    model::{Context, PullRequestsProvider, UnavailableCode},
};

/// Whether a context read may reuse what explicit probes already answered.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContextRead {
    /// Opening the module or switching checkout: reuse the recorded host and
    /// the bounded 30 s answers.
    Open,
    /// Rescan and authentication recovery: forget the recorded host and ask again.
    Rescan,
}

#[derive(Clone, Debug, Default)]
pub struct DiscoveredHosts {
    pub github: Vec<String>,
    pub gitlab: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Unavailable {
    pub code: &'static str,
    pub message: String,
    pub provider: Option<ProviderKind>,
    pub host: Option<String>,
    pub install_hint: Option<String>,
    pub auth_command: Option<String>,
}

impl Unavailable {
    fn new(code: &'static str, message: impl Into<String>, scope: Option<&HostScope>) -> Self {
        Self {
            code,
            message: message.into(),
            provider: scope.map(|s| s.provider.kind()),
            host: scope.map(|s| s.host.clone()),
            install_hint: None,
            auth_command: None,
        }
    }

    pub fn operation_error(&self, operation: &str) -> PullRequestsOperationError {
        let mut error = PullRequestsOperationError::new(
            operation,
            if CODES.contains(&self.code) {
                self.code
            } else {
                "unavailable"
            },
        );
        error.message = self.message.clone();
        error
    }

    pub fn into_context(self) -> Result<Context, PullRequestsOperationError> {
        let code = match self.code {
            "no_remote" => UnavailableCode::NoRemote,
            "unsupported_provider" => UnavailableCode::UnsupportedProvider,
            "unknown_host" => UnavailableCode::UnknownHost,
            "cli_missing" => UnavailableCode::CliMissing,
            "not_authenticated" => UnavailableCode::NotAuthenticated,
            "repository_unreachable" => UnavailableCode::RepositoryUnreachable,
            "cli_too_old" => UnavailableCode::CliTooOld,
            _ => return Err(self.operation_error("pullRequests.getContext")),
        };
        Ok(Context::Unavailable {
            code,
            message: self.message,
            provider: self.provider,
            host: self.host,
            install_hint: self.install_hint,
            auth_command: self.auth_command,
        })
    }
}

fn cli(provider: PullRequestsProvider) -> &'static str {
    match provider {
        PullRequestsProvider::Github => "gh",
        PullRequestsProvider::Gitlab => "glab",
    }
}

fn process_unavailable(
    failure: &ProcessFailure,
    command: &str,
    scope: Option<&HostScope>,
) -> Unavailable {
    let error = from_process_error(
        "pullRequests.getContext",
        command,
        &failure.error,
        &failure.stderr,
    );
    Unavailable::new(error.code, error.message, scope)
}

pub async fn resolve_scope(
    runner: &HostCommandRunner,
    cwd: &Path,
    discovered_hosts: &DiscoveredHosts,
    c: &CancellationToken,
) -> Result<HostScope, Unavailable> {
    resolve_checked_scope(runner, cwd, discovered_hosts, ContextRead::Open, c).await
}

/// An identified GitHub/GitLab scope whose login must still be checked. Only read
/// paths may use it concurrently; mutations continue through `resolve_scope`.
pub(super) struct PendingScope {
    pub scope: HostScope,
    authentication: ScopeAuthentication,
}

enum ScopeAuthentication {
    Named,
    Recorded,
    Discovered(Option<String>),
}

impl PendingScope {
    /// Authenticate alongside a cancellable read. A login failure owns precedence,
    /// but its provider child must be cancelled and drained before returning it.
    /// After login succeeds, reject a checkout lost while either read was active.
    pub(super) async fn read_authenticated<T, F>(
        &self,
        runner: &HostCommandRunner,
        c: &CancellationToken,
        read: impl FnOnce(CancellationToken) -> F,
    ) -> Result<Result<T, PullRequestsOperationError>, Unavailable>
    where
        F: Future<Output = Result<T, PullRequestsOperationError>>,
    {
        let read_cancellation = c.child_token();
        let authenticate = async {
            let result = self.authenticate(runner, c).await;
            if result.is_err() {
                read_cancellation.cancel();
            }
            result
        };
        let (authenticated, result) = tokio::join!(authenticate, read(read_cancellation.clone()));
        authenticated?;
        validate_checkout(&self.scope.cwd).await?;
        Ok(result)
    }

    pub(super) async fn authenticate(
        &self,
        runner: &HostCommandRunner,
        c: &CancellationToken,
    ) -> Result<(), Unavailable> {
        let (status, record_host) = match &self.authentication {
            ScopeAuthentication::Named => (None, false),
            ScopeAuthentication::Recorded => (provider_status(runner, &self.scope, c).await, true),
            ScopeAuthentication::Discovered(status) => (status.clone(), true),
        };
        let result = ensure_authenticated(runner, &self.scope, status.as_deref(), c).await;
        match &result {
            Ok(()) if record_host => runner
                .provider_hosts()
                .record(&self.scope.host, self.scope.provider.kind()),
            Err(unavailable) if unavailable.code == "not_authenticated" => {
                runner.provider_hosts().forget(&self.scope.host);
            }
            Err(unavailable) if unavailable.code == "cli_missing" => {
                validate_checkout(&self.scope.cwd).await?;
            }
            _ => {}
        }
        result
    }
}

async fn resolve_checked_scope(
    runner: &HostCommandRunner,
    cwd: &Path,
    discovered_hosts: &DiscoveredHosts,
    read: ContextRead,
    c: &CancellationToken,
) -> Result<HostScope, Unavailable> {
    let pending = resolve_pending_scope(runner, cwd, discovered_hosts, read, c).await?;
    pending.authenticate(runner, c).await?;
    Ok(pending.scope)
}

pub(super) async fn resolve_pending_scope(
    runner: &HostCommandRunner,
    cwd: &Path,
    discovered_hosts: &DiscoveredHosts,
    read: ContextRead,
    c: &CancellationToken,
) -> Result<PendingScope, Unavailable> {
    validate_checkout(cwd).await?;
    let result = resolve_existing_checkout_scope(runner, cwd, discovered_hosts, read, c).await;
    // ENOENT may mean the checkout disappeared between commands, not a missing CLI.
    if matches!(&result, Err(unavailable) if unavailable.code == "cli_missing") {
        validate_checkout(cwd).await?;
    }
    result
}

/// A checkout's origin before a provider is admitted: only CLI discovery can say
/// which provider serves its host.
pub(super) struct RemoteScope {
    cwd: PathBuf,
    host: String,
    repository: String,
}

impl RemoteScope {
    fn admit(&self, provider: PullRequestsProvider) -> HostScope {
        HostScope {
            cwd: self.cwd.clone(),
            host: self.host.clone(),
            repository: self.repository.clone(),
            provider,
        }
    }

    /// The one admission check: only providers this module implements become a scope.
    fn require_supported_provider(
        &self,
        provider: ProviderKind,
    ) -> Result<PullRequestsProvider, Unavailable> {
        PullRequestsProvider::from_kind(provider).ok_or_else(|| {
            self.unavailable(
                "unsupported_provider",
                "Pull Requests supports GitHub and GitLab repositories on this environment.",
                provider,
            )
        })
    }

    /// Before admission an answer names the host and the provider identified so far.
    fn unavailable(
        &self,
        code: &'static str,
        message: impl Into<String>,
        provider: ProviderKind,
    ) -> Unavailable {
        self.rescope(Unavailable::new(code, message, None), provider)
    }

    /// Names this origin's host, and the provider identified so far, on an answer
    /// that was built without a scope.
    fn rescope(&self, unavailable: Unavailable, provider: ProviderKind) -> Unavailable {
        Unavailable {
            provider: Some(provider),
            host: Some(self.host.clone()),
            ..unavailable
        }
    }
}

/// What the origin decides before any provider CLI is asked.
pub(super) enum RemoteAdmission {
    /// The host name alone identifies a supported provider.
    Named(HostScope),
    /// An explicit probe recorded this host's supported provider.
    Recorded(HostScope),
    /// Neither: CLI discovery decides, or reports `unknown_host`.
    Unidentified(RemoteScope),
}

/// Decode the remote once for both scope resolution and created-request invalidation,
/// and admit the provider its host identifies.
pub(super) fn scope_from_remote(
    cwd: &Path,
    remote: &str,
    identified: &IdentifiedProvider,
) -> Result<RemoteAdmission, Unavailable> {
    let Some(host) = remote_host(remote) else {
        return Err(Unavailable::new(
            "no_remote",
            "This checkout has no hosted origin remote.",
            None,
        ));
    };
    let remote_scope = RemoteScope {
        cwd: cwd.into(),
        host,
        repository: remote_repository_path(remote).unwrap_or_default(),
    };
    // Unknown hosts still need CLI discovery before provider admission.
    let provider = match identified {
        IdentifiedProvider::Named(info) | IdentifiedProvider::Recorded(info) => {
            Some(remote_scope.require_supported_provider(info.kind)?)
        }
        IdentifiedProvider::Unknown => None,
    };
    if remote_scope.repository.is_empty() {
        return Err(remote_scope.unavailable(
            "no_remote",
            "The origin remote has no repository path.",
            identified.kind(),
        ));
    }
    Ok(match (identified, provider) {
        (IdentifiedProvider::Named(_), Some(provider)) => {
            RemoteAdmission::Named(remote_scope.admit(provider))
        }
        (IdentifiedProvider::Recorded(_), Some(provider)) => {
            RemoteAdmission::Recorded(remote_scope.admit(provider))
        }
        (IdentifiedProvider::Unknown, _) | (_, None) => RemoteAdmission::Unidentified(remote_scope),
    })
}

pub(super) async fn validate_checkout(cwd: &Path) -> Result<(), Unavailable> {
    let (problem, recovery) = match tokio::fs::metadata(cwd).await {
        Ok(metadata) if metadata.is_dir() => return Ok(()),
        Ok(_) => (
            "is not a directory",
            "Select another checkout, then rescan.",
        ),
        Err(error) if error.kind() == ErrorKind::NotFound => (
            "no longer exists",
            "Select another checkout or restore this directory, then rescan.",
        ),
        Err(error) if error.kind() == ErrorKind::NotADirectory => (
            "is not a directory",
            "Select another checkout, then rescan.",
        ),
        Err(_) => (
            "cannot be accessed",
            "Check its availability and permissions, then rescan.",
        ),
    };
    Err(Unavailable::new(
        "repository_unreachable",
        format!(
            "The selected checkout `{}` {problem} on this environment. {recovery}",
            cwd.display()
        ),
        None,
    ))
}

async fn resolve_existing_checkout_scope(
    runner: &HostCommandRunner,
    cwd: &Path,
    discovered_hosts: &DiscoveredHosts,
    read: ContextRead,
    c: &CancellationToken,
) -> Result<PendingScope, Unavailable> {
    let remote = match runner.git(cwd, &["remote", "get-url", "origin"], c).await {
        Ok(output) => output.stdout.trim().to_owned(),
        Err(failure)
            if matches!(&failure.error, ProcessError::NonZeroExit { .. })
                && (failure.stderr.contains("No such remote")
                    || failure.stderr.contains("not a git repository")) =>
        {
            return Err(Unavailable::new(
                "no_remote",
                "This checkout has no origin remote.",
                None,
            ));
        }
        Err(failure) => return Err(process_unavailable(&failure, "git", None)),
    };
    let recorded = runner.provider_hosts();
    if read == ContextRead::Rescan
        && let Some(host) = remote_host(&remote)
    {
        recorded.forget(&host);
    }
    let (scope, authentication) = match scope_from_remote(
        cwd,
        &remote,
        &recorded.identify(&remote),
    )? {
        RemoteAdmission::Named(scope) => (scope, ScopeAuthentication::Named),
        RemoteAdmission::Recorded(scope) => (scope, ScopeAuthentication::Recorded),
        RemoteAdmission::Unidentified(remote_scope) => {
            let discovery =
                if discovered_hosts.github.is_empty() && discovered_hosts.gitlab.is_empty() {
                    discover_hosts(runner, &remote_scope, c).await?
                } else {
                    Discovery {
                        hosts: discovered_hosts.clone(),
                        github_status: None,
                        gitlab_status: None,
                    }
                };
            let hosts = &discovery.hosts;
            let host = &remote_scope.host;
            let provider = if hosts
                .github
                .iter()
                .any(|configured| configured.eq_ignore_ascii_case(host))
            {
                PullRequestsProvider::Github
            } else if hosts
                .gitlab
                .iter()
                .any(|configured| configured.eq_ignore_ascii_case(host))
            {
                PullRequestsProvider::Gitlab
            } else {
                let mut unavailable = remote_scope.unavailable(
                    "unknown_host",
                    format!(
                        "`{host}` is not a configured GitHub or GitLab host on this environment. Run `gh auth login --hostname {host}` or `glab auth login --hostname {host}` there, then rescan."
                    ),
                    ProviderKind::Unknown,
                );
                let command = if hosts.gitlab.is_empty() {
                    "gh"
                } else {
                    "glab"
                };
                unavailable.auth_command = Some(format!("{command} auth login --hostname {host}"));
                recorded.forget(host);
                return Err(unavailable);
            };
            let status = match provider {
                PullRequestsProvider::Github => discovery.github_status,
                PullRequestsProvider::Gitlab => discovery.gitlab_status,
            };
            (
                remote_scope.admit(provider),
                ScopeAuthentication::Discovered(status),
            )
        }
    };
    Ok(PendingScope {
        scope,
        authentication,
    })
}

/// Both CLIs' configured hosts, plus each probe's status text. Probes run with the
/// host pinned (`GH_HOST`/`GITLAB_HOST`), so the text already answers whether this
/// host is logged in and the authentication check can reuse it.
struct Discovery {
    hosts: DiscoveredHosts,
    github_status: Option<String>,
    gitlab_status: Option<String>,
}

fn discovery_args(provider: PullRequestsProvider) -> &'static [&'static str] {
    match provider {
        PullRequestsProvider::Github => &["auth", "status", "--json", "hosts"],
        PullRequestsProvider::Gitlab => &["auth", "status"],
    }
}

fn status_text(provider: PullRequestsProvider, output: &super::host::CommandOutput) -> String {
    match provider {
        PullRequestsProvider::Github => output.stdout.clone(),
        PullRequestsProvider::Gitlab => format!("{}\n{}", output.stdout, output.stderr),
    }
}

/// The scope provider's discovery answer: its configured hosts and login status.
async fn discovery_probe(
    runner: &HostCommandRunner,
    scope: &HostScope,
    c: &CancellationToken,
) -> Result<String, ProcessFailure> {
    runner
        .probe(scope, discovery_args(scope.provider), c)
        .await
        .or_else(ProcessFailure::into_output)
        .map(|output| status_text(scope.provider, &output))
}

/// The recorded provider's discovery answer alone, reused by the authentication check.
async fn provider_status(
    runner: &HostCommandRunner,
    scope: &HostScope,
    c: &CancellationToken,
) -> Option<String> {
    discovery_probe(runner, scope, c).await.ok()
}

async fn discover_hosts(
    runner: &HostCommandRunner,
    remote: &RemoteScope,
    c: &CancellationToken,
) -> Result<Discovery, Unavailable> {
    // The two CLIs answer independently; probing them together halves discovery.
    let github_probe = remote.admit(PullRequestsProvider::Github);
    let gitlab_probe = remote.admit(PullRequestsProvider::Gitlab);
    let (github, gitlab) = tokio::join!(
        discovery_probe(runner, &github_probe, c),
        discovery_probe(runner, &gitlab_probe, c),
    );
    let mut discovery = Discovery {
        hosts: DiscoveredHosts::default(),
        github_status: None,
        gitlab_status: None,
    };
    for (provider, outcome) in [
        (PullRequestsProvider::Github, github),
        (PullRequestsProvider::Gitlab, gitlab),
    ] {
        let text = match outcome {
            Ok(text) => text,
            Err(failure) if c.is_cancelled() => {
                let unavailable = process_unavailable(&failure, cli(provider), None);
                return Err(remote.rescope(unavailable, ProviderKind::Unknown));
            }
            Err(_) => continue,
        };
        match provider {
            PullRequestsProvider::Github => {
                discovery.hosts.github = parse_github_auth_status(&text)
                    .accounts
                    .into_iter()
                    .map(|account| account.host)
                    .collect();
                discovery.github_status = Some(text);
            }
            PullRequestsProvider::Gitlab => {
                discovery.hosts.gitlab = parse_gitlab_auth_status(&text)
                    .into_iter()
                    .map(|host| host.host)
                    .collect();
                discovery.gitlab_status = Some(text);
            }
        }
    }
    Ok(discovery)
}

/// Whether a status answer shows this host logged in, without failure markers.
fn status_authenticated(provider: PullRequestsProvider, text: &str, host: &str) -> bool {
    match provider {
        PullRequestsProvider::Github => {
            let status = parse_github_auth_status(text);
            status.parsed
                && status.accounts.iter().any(|account| {
                    account.host.eq_ignore_ascii_case(host)
                        && account.active
                        && account.authenticated
                })
        }
        PullRequestsProvider::Gitlab => gitlab_authenticated(text, host),
    }
}

async fn ensure_authenticated(
    runner: &HostCommandRunner,
    scope: &HostScope,
    discovered_status: Option<&str>,
    c: &CancellationToken,
) -> Result<(), Unavailable> {
    let provider = scope.provider;
    if let Err(failure) = runner.probe(scope, &["--version"], c).await {
        let missing = matches!(failure.error, ProcessError::NonZeroExit { .. })
            || matches!(&failure.error, ProcessError::Spawn { source, .. } if source.kind() == std::io::ErrorKind::NotFound);
        if !missing {
            return Err(process_unavailable(&failure, cli(provider), Some(scope)));
        }
        let mut unavailable = Unavailable::new(
            "cli_missing",
            format!("`{}` is not installed on this environment", cli(provider)),
            Some(scope),
        );
        unavailable.install_hint = provider_install_hint(provider.kind()).map(str::to_owned);
        return Err(unavailable);
    }
    // A discovery answer already checked this host's login; only a missing or failed
    // answer needs the explicit per-host probe and its failure classification.
    if discovered_status.is_some_and(|text| status_authenticated(provider, text, &scope.host)) {
        return Ok(());
    }
    let args = match provider {
        PullRequestsProvider::Github => vec![
            "auth",
            "status",
            "--hostname",
            scope.host.as_str(),
            "--json",
            "hosts",
        ],
        PullRequestsProvider::Gitlab => vec!["auth", "status", "--hostname", scope.host.as_str()],
    };
    let output = runner
        .probe(scope, &args, c)
        .await
        .or_else(ProcessFailure::into_output)
        .map_err(|failure| process_unavailable(&failure, cli(provider), Some(scope)))?;
    if status_authenticated(provider, &status_text(provider, &output), &scope.host) {
        return Ok(());
    }
    let text = format!("{}\n{}", output.stdout, output.stderr);
    let lower = text.to_lowercase();
    if lower.contains("unknown flag") || lower.contains("unknown command") {
        return Err(Unavailable::new(
            "cli_too_old",
            "Update the provider CLI on this environment, then rescan.",
            Some(scope),
        ));
    }
    let mut unavailable = Unavailable::new(
        "not_authenticated",
        "Authenticate the provider CLI for this host, then rescan.",
        Some(scope),
    );
    unavailable.auth_command = Some(format!(
        "{} auth login --hostname {}",
        cli(provider),
        scope.host
    ));
    Err(unavailable)
}

fn gitlab_authenticated(text: &str, host: &str) -> bool {
    let parsed = parse_gitlab_auth_status(text);
    if !parsed
        .iter()
        .any(|entry| entry.host.eq_ignore_ascii_case(host) && entry.account.is_some())
    {
        return false;
    }
    let mut in_host = false;
    for raw in text.lines() {
        let line = raw.trim();
        if parsed
            .iter()
            .any(|entry| entry.host.eq_ignore_ascii_case(line))
        {
            in_host = line.eq_ignore_ascii_case(host);
        }
        if in_host
            && (line.starts_with("x ")
                || line.starts_with("X ")
                || line.starts_with("✗ ")
                || line.starts_with("× ")
                || line.contains("401"))
        {
            return false;
        }
    }
    true
}

pub async fn build_context(
    host: &dyn PullRequestHost,
    _runner: &HostCommandRunner,
    scope: &HostScope,
    c: &CancellationToken,
) -> Result<Context, PullRequestsOperationError> {
    let context = match host.context(scope, c).await {
        Ok(context) => context,
        Err(error) if matches!(error.code, "not_found" | "forbidden") => {
            return Unavailable::new("repository_unreachable", "This repository cannot be reached with the authenticated account. Check repository access, then rescan.", Some(scope)).into_context();
        }
        Err(error)
            if matches!(
                error.code,
                "cli_too_old" | "not_authenticated" | "cli_missing"
            ) =>
        {
            if error.code == "cli_missing"
                && let Err(unavailable) = validate_checkout(&scope.cwd).await
            {
                return unavailable.into_context();
            }
            let mut unavailable = Unavailable::new(error.code, error.message, Some(scope));
            if error.code == "not_authenticated" {
                unavailable.auth_command = Some(format!(
                    "{} auth login --hostname {}",
                    cli(scope.provider),
                    scope.host
                ));
            }
            if error.code == "cli_missing" {
                unavailable.install_hint =
                    provider_install_hint(scope.provider.kind()).map(str::to_owned);
            }
            return unavailable.into_context();
        }
        Err(error) => return Err(error),
    };
    let capabilities = host.capabilities(context.version().as_ref());
    Ok(Context::Available {
        provider: context.provider,
        host: context.host,
        host_version: context.host_version,
        repository: context.repository,
        default_branch: context.default_branch,
        account: context.account,
        repository_permission: context.repository_permission,
        merge_policy: context.merge_policy,
        capabilities: Box::new(capabilities),
        web_url: context.web_url,
        stale: false,
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_support::TestSandbox;

    const GH: &str = r#"case "$1 $2" in
      '--version ') echo 'gh version 2.97.0' ;;
      'auth status') echo '{"hosts":{"github.com":[{"state":"success","active":true,"host":"github.com","login":"mubeda"}]}}' ;;
      *) exit 64 ;;
    esac"#;
    const GLAB: &str = r#"case "$1 $2" in
      '--version ') echo 'glab 1.114.0' ;;
      'auth status') printf 'gitlab.com\n  x HTTP 401\ngit.acme.example\n  ✓ Logged in to git.acme.example as alice\n' ;;
      *) exit 64 ;;
    esac"#;

    fn runner(s: &TestSandbox, remote: Option<&str>, gh: &str, glab: &str) -> HostCommandRunner {
        let git = s.executable_script(
            "git",
            &remote.map_or_else(
                || "echo \"error: No such remote 'origin'\" >&2; exit 2".into(),
                |r| format!("printf '%s' '{r}'"),
            ),
            "",
        );
        let gh = s.executable_script("gh", gh, "");
        let glab = s.executable_script("glab", glab, "");
        HostCommandRunner::new(s.path("state")).with_commands(gh, glab, git)
    }

    #[tokio::test]
    async fn pull_requests_resolves_known_and_discovered_hosts() {
        for (remote, provider, host, repository) in [
            (
                "https://github.com/example/repository.git",
                PullRequestsProvider::Github,
                "github.com",
                "example/repository",
            ),
            (
                "ssh://git@git.acme.example/team/sub/repo.git",
                PullRequestsProvider::Gitlab,
                "git.acme.example",
                "team/sub/repo",
            ),
        ] {
            let s = TestSandbox::new("pr-scope");
            let runner = runner(&s, Some(remote), GH, GLAB);
            let scope = resolve_scope(
                &runner,
                s.root(),
                &DiscoveredHosts::default(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
            assert_eq!(scope.provider, provider);
            assert_eq!(scope.host, host);
            assert_eq!(scope.repository, repository);
        }
    }

    #[tokio::test]
    async fn pull_requests_scope_reports_no_remote_unsupported_and_unknown() {
        for (remote, code) in [
            (None, "no_remote"),
            (
                Some("https://dev.azure.com/org/repo"),
                "unsupported_provider",
            ),
            (
                Some("git@bitbucket.org:team/repo.git"),
                "unsupported_provider",
            ),
            (
                Some("ssh://git@git.acme.example/team/sub/repo.git"),
                "unknown_host",
            ),
        ] {
            let s = TestSandbox::new("pr-unavailable");
            let runner = runner(&s, remote, GH, "printf 'gitlab.com\\n  x HTTP 401\\n'");
            let unavailable = resolve_scope(
                &runner,
                s.root(),
                &DiscoveredHosts::default(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
            assert_eq!(unavailable.code, code);
            if code == "unknown_host" {
                assert_eq!(
                    unavailable.auth_command.as_deref(),
                    Some("glab auth login --hostname git.acme.example")
                );
            }
            assert!(matches!(
                unavailable.into_context().unwrap(),
                Context::Unavailable { .. }
            ));
        }
    }

    #[tokio::test]
    async fn pull_requests_auth_failures_and_install_hints_are_actionable() {
        for (gh, code) in [
            (
                "if [ \"$1\" = --version ]; then exit 0; fi; echo 'You are not logged into any GitHub hosts' >&2; exit 1",
                "not_authenticated",
            ),
            ("exit 1", "cli_missing"),
            (
                "if [ \"$1\" = --version ]; then exit 0; fi; echo 'unknown flag: --json' >&2; exit 1",
                "cli_too_old",
            ),
        ] {
            let s = TestSandbox::new("pr-auth");
            let runner = runner(
                &s,
                Some("https://github.com/example/repository.git"),
                gh,
                GLAB,
            );
            let u = resolve_scope(
                &runner,
                s.root(),
                &DiscoveredHosts::default(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
            assert_eq!(u.code, code);
            if code == "cli_missing" {
                assert_eq!(
                    u.install_hint.as_deref(),
                    Some("Install GitHub CLI from https://cli.github.com/.")
                );
            }
            if code == "not_authenticated" {
                assert_eq!(
                    u.auth_command.as_deref(),
                    Some("gh auth login --hostname github.com")
                );
            }
        }
    }

    /// glab 1.114.0's answer for a configured host without a token, captured offline.
    /// Discovery still names the host as GitLab, so the user gets that host's login
    /// command instead of `unknown_host`, and nothing is recorded.
    #[tokio::test]
    async fn pull_requests_configured_gitlab_host_without_token_asks_for_its_login() {
        let s = TestSandbox::new("pr-no-token");
        let fixture = s.path("glab-no-token.txt");
        std::fs::write(
            &fixture,
            include_str!("../../tests/fixtures/pull_requests/glab_no_token.txt"),
        )
        .unwrap();
        let glab = format!(
            r#"case "$1 $2" in
  '--version ') echo 'glab 1.114.0' ;;
  'auth status') cat '{}' >&2; exit 1 ;;
  *) exit 64 ;;
esac"#,
            fixture.display()
        );
        let runner = runner(&s, Some("https://gitlab.invalid/team/repo.git"), GH, &glab);
        let unavailable = resolve_scope(
            &runner,
            s.root(),
            &DiscoveredHosts::default(),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert_eq!(unavailable.code, "not_authenticated");
        assert_eq!(unavailable.provider, Some(ProviderKind::Gitlab));
        assert_eq!(
            unavailable.auth_command.as_deref(),
            Some("glab auth login --hostname gitlab.invalid")
        );
        assert_eq!(runner.provider_hosts().provider("gitlab.invalid"), None);
    }

    #[test]
    fn pull_requests_gitlab_auth_checks_only_the_requested_host_block() {
        let text = "gitlab.com\n  x HTTP 401\ngit.acme.example\n  ✓ Logged in to git.acme.example as alice\n";
        assert!(gitlab_authenticated(text, "git.acme.example"));
        assert!(!gitlab_authenticated(text, "gitlab.com"));
        assert!(!gitlab_authenticated(
            "git.acme.example\n  Logged in to git.acme.example as alice\n  x Invalid credentials\n",
            "git.acme.example"
        ));
    }

    /// A custom host needs both CLIs' host lists. Each fake discovery probe waits
    /// until the other one is running, so sequential discovery finds no host.
    #[tokio::test]
    async fn pull_requests_custom_host_discovery_runs_both_probes_together_and_reuses_the_login() {
        const ARRIVE: &str = r#"arrive() {
  : > "$1.$$"; i=0
  while [ "$(ls "$1".* 2>/dev/null | wc -l)" -lt "$2" ]; do
    i=$((i + 1)); if [ "$i" -gt 60 ]; then rm -f "$1.$$"; echo "$1 probe ran alone" >&2; exit 75; fi
    sleep 0.05
  done
}"#;
        for (login, expected) in [
            ("  ✓ Logged in to git.acme.example as alice", Ok(())),
            (
                "  ✓ Logged in to git.acme.example as alice\n  x HTTP 401",
                Err("not_authenticated"),
            ),
        ] {
            let s = TestSandbox::new("pr-discovery");
            let gh = format!(
                r#"{ARRIVE}
printf 'gh %s\n' "$*" >> calls
case "$1 $2" in
  '--version ') echo 'gh version 2.97.0' ;;
  'auth status') arrive discovery 2; echo '{{"hosts":{{"github.com":[{{"state":"success","active":true,"host":"github.com","login":"mubeda"}}]}}}}' ;;
  *) exit 64 ;;
esac"#
            );
            let glab = format!(
                r#"{ARRIVE}
printf 'glab %s\n' "$*" >> calls
case "$1 $2 $3" in
  '--version  ') echo 'glab 1.114.0' ;;
  'auth status ') arrive discovery 2; printf 'git.acme.example\n{login}\n' ;;
  'auth status --hostname') printf 'git.acme.example\n{login}\n' ;;
  *) exit 64 ;;
esac"#
            );
            let runner = runner(
                &s,
                Some("https://git.acme.example/team/sub/repo.git"),
                &gh,
                &glab,
            );
            let result = resolve_scope(
                &runner,
                s.root(),
                &DiscoveredHosts::default(),
                &CancellationToken::new(),
            )
            .await;
            let calls = std::fs::read_to_string(s.path("calls")).unwrap();
            let explicit_probe = calls.contains("glab auth status --hostname git.acme.example");
            match expected {
                Ok(()) => {
                    let scope = result.unwrap();
                    assert_eq!(scope.provider, PullRequestsProvider::Gitlab);
                    assert!(
                        !explicit_probe,
                        "discovery already proved this host's login:\n{calls}"
                    );
                }
                Err(code) => {
                    assert_eq!(result.unwrap_err().code, code);
                    assert!(
                        explicit_probe,
                        "a failed login must be rechecked explicitly:\n{calls}"
                    );
                }
            }
        }
    }

    /// Discovery records an authenticated custom host. Later reads skip the other CLI
    /// and recheck the login with the recorded provider's discovery probe;
    /// `not_authenticated`, Rescan and `unknown_host` each forget the host on their own.
    #[tokio::test]
    async fn pull_requests_scope_records_skips_and_forgets_custom_hosts() {
        let s = TestSandbox::new("pr-recorded-hosts");
        let gh = r#"printf 'gh %s\n' "$*" >> calls
case "$1 $2" in
  '--version ') echo 'gh version 2.97.0' ;;
  'auth status') echo '{"hosts":{"github.com":[{"state":"success","active":true,"host":"github.com","login":"mubeda"}]}}' ;;
  *) exit 64 ;;
esac"#;
        let glab = r#"printf 'glab %s\n' "$*" >> calls
case "$1 $2 $3" in
  '--version  ') echo 'glab 1.114.0' ;;
  'auth status '|'auth status --hostname')
    if [ -e logged-out ]; then printf 'git.acme.example\n  x HTTP 401\n'
    elif [ -e unconfigured ]; then
      : > probing; while [ ! -e release ]; do sleep 0.02; done
      printf 'gitlab.com\n  x HTTP 401\n'
    else printf 'git.acme.example\n  ✓ Logged in to git.acme.example as alice\n'; fi ;;
  *) exit 64 ;;
esac"#;
        let runner = runner(&s, Some("https://git.acme.example/team/repo.git"), gh, glab);
        let hosts = runner.provider_hosts();
        let c = CancellationToken::new();
        let calls = || std::fs::read_to_string(s.path("calls")).unwrap_or_default();
        let none = DiscoveredHosts::default();
        let resolve = || resolve_scope(&runner, s.root(), &none, &c);

        assert_eq!(
            resolve().await.unwrap().provider,
            PullRequestsProvider::Gitlab
        );
        assert_eq!(
            hosts.provider("git.acme.example"),
            Some(ProviderKind::Gitlab)
        );
        assert!(calls().contains("gh auth status"), "{}", calls());

        runner.clear_context_probes();
        std::fs::remove_file(s.path("calls")).unwrap();
        assert_eq!(
            resolve().await.unwrap().provider,
            PullRequestsProvider::Gitlab
        );
        let recorded = calls();
        assert!(
            !recorded.contains("gh "),
            "a recorded host skips gh:\n{recorded}"
        );
        assert!(
            recorded.contains("glab auth status\n") && !recorded.contains("--hostname"),
            "the login is rechecked with glab's discovery probe:\n{recorded}"
        );

        runner.clear_context_probes();
        std::fs::write(s.path("logged-out"), "").unwrap();
        assert_eq!(resolve().await.unwrap_err().code, "not_authenticated");
        assert_eq!(hosts.provider("git.acme.example"), None);

        // Rescan: a host recorded under the wrong provider would only ask gh; Rescan
        // forgets it, so discovery answers again and records what glab reports.
        std::fs::remove_file(s.path("logged-out")).unwrap();
        runner.clear_context_probes();
        hosts.record("git.acme.example", ProviderKind::Github);
        let rescanned = resolve_checked_scope(
            &runner,
            s.root(),
            &DiscoveredHosts::default(),
            ContextRead::Rescan,
            &c,
        )
        .await
        .unwrap();
        assert_eq!(rescanned.provider, PullRequestsProvider::Gitlab);
        assert_eq!(
            hosts.provider("git.acme.example"),
            Some(ProviderKind::Gitlab)
        );

        // `unknown_host` without Rescan: another explicit probe recorded the host
        // while this read's discovery ran; this read's answer forgets it again.
        runner.clear_context_probes();
        hosts.forget("git.acme.example");
        std::fs::write(s.path("unconfigured"), "").unwrap();
        let concurrent_record = async {
            while !s.path("probing").exists() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            hosts.record("git.acme.example", ProviderKind::Gitlab);
            std::fs::write(s.path("release"), "").unwrap();
        };
        let (read, ()) = tokio::join!(resolve(), concurrent_record);
        assert_eq!(read.unwrap_err().code, "unknown_host");
        assert_eq!(hosts.provider("git.acme.example"), None);
    }

    #[tokio::test]
    async fn pull_requests_cancelled_scope_is_a_typed_process_error() {
        let s = TestSandbox::new("pr-cancel");
        let runner = runner(
            &s,
            Some("https://github.com/example/repository.git"),
            GH,
            GLAB,
        );
        let c = CancellationToken::new();
        c.cancel();
        let u = resolve_scope(&runner, s.root(), &DiscoveredHosts::default(), &c)
            .await
            .unwrap_err();
        assert_eq!(u.into_context().unwrap_err().code, "timeout");
    }
}
