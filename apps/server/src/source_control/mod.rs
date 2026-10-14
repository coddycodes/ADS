pub mod checks;
mod discovery;
mod hosts;
mod pull_request;
pub mod repository_identity;

pub(crate) use discovery::provider_install_hint;
pub use hosts::{IdentifiedProvider, ProviderHosts};

#[allow(unused_imports)]
pub use discovery::{
    AuthStatus, DiscoveryStatus, SourceControlDiscovery, SourceControlDiscoveryResult,
    SourceControlProviderAuth, SourceControlProviderDiscoveryItem, VcsDiscoveryItem,
    VcsDiscoveryKind, WireOption,
};
pub(crate) use pull_request::GitLabCreateTransport;
pub(crate) use pull_request::PrivateJsonBody;
pub(crate) use pull_request::ProviderCommandFailure;
#[allow(unused_imports)]
pub(crate) use pull_request::ProviderCommandSpec;
pub(crate) use pull_request::parse_github_create_url;
pub(crate) use pull_request::provider_cli_environment;
#[allow(unused_imports)]
pub use pull_request::{
    ChangeRequestState, CreatePullRequestInput, CreatePullRequestOptions, CreatedPullRequest,
    MilestoneRef, PullRequestService, ResolvePullRequestInput, ResolvedPullRequest,
    SourceControlProviderError, parse_github_pull_request, parse_gitlab_merge_request,
};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    Github,
    Gitlab,
    AzureDevops,
    Bitbucket,
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInfo {
    pub kind: ProviderKind,
    pub name: String,
    pub base_url: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitHubAuthStatusAccount {
    pub host: String,
    pub account: String,
    pub authenticated: bool,
    pub active: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitHubAuthStatus {
    pub parsed: bool,
    pub accounts: Vec<GitHubAuthStatusAccount>,
}

#[derive(Deserialize)]
struct RawGitHubStatus {
    hosts: std::collections::HashMap<String, Vec<RawGitHubAccount>>,
}

#[derive(Deserialize)]
struct RawGitHubAccount {
    state: String,
    #[serde(default)]
    error: Option<String>,
    active: bool,
    host: String,
    login: String,
}

#[must_use]
pub fn parse_github_auth_status(text: &str) -> GitHubAuthStatus {
    let Ok(status) = serde_json::from_str::<RawGitHubStatus>(text) else {
        return GitHubAuthStatus {
            parsed: false,
            accounts: vec![],
        };
    };
    let accounts = status
        .hosts
        .into_values()
        .flatten()
        .filter_map(|account| {
            let host = non_empty(&account.host)?.to_lowercase();
            let login = non_empty(&account.login)?;
            Some(GitHubAuthStatusAccount {
                host,
                account: login,
                authenticated: account.state == "success",
                active: account.active,
                error: account.error.and_then(|error| non_empty(&error)),
            })
        })
        .collect();
    GitHubAuthStatus {
        parsed: true,
        accounts,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GitLabAuthStatusHost {
    pub host: String,
    pub account: Option<String>,
}

#[must_use]
pub fn parse_gitlab_auth_status(text: &str) -> Vec<GitLabAuthStatusHost> {
    let mut result = Vec::new();
    let mut current_host: Option<String> = None;
    let mut current_lines = Vec::new();
    let flush = |result: &mut Vec<GitLabAuthStatusHost>,
                 current_host: &mut Option<String>,
                 current_lines: &mut Vec<String>| {
        let Some(host) = current_host.take() else {
            return;
        };
        let joined = current_lines.join("\n");
        let account = joined
            .split("Logged in to ")
            .nth(1)
            .and_then(|value| value.split(" as ").nth(1))
            .and_then(|value| value.split_whitespace().next())
            .and_then(non_empty);
        result.push(GitLabAuthStatusHost { host, account });
        current_lines.clear();
    };
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if raw_line.len() == raw_line.trim_start().len() && looks_like_host(line) {
            flush(&mut result, &mut current_host, &mut current_lines);
            current_host = Some(line.to_lowercase());
        } else if current_host.is_some() {
            current_lines.push(line.to_owned());
        }
    }
    flush(&mut result, &mut current_host, &mut current_lines);
    result
}

/// glab's full-logout sentence, lowercased: v1.36 and v1.39 print the singular
/// form; v1.43 onward print the plural (v1.110 made it a lowercase error).
const GITLAB_LOGGED_OUT_PHRASES: [&str; 2] = [
    "no gitlab instance has been authenticated with glab",
    "no gitlab instances have been authenticated with glab",
];

/// Recognize glab's full-logout answer independently of wrapping, case or its login hint.
pub(crate) fn gitlab_auth_status_is_logged_out(text: &str) -> bool {
    let normalized = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    GITLAB_LOGGED_OUT_PHRASES
        .iter()
        .any(|phrase| normalized.contains(phrase))
}

/// The provider a remote's host name identifies on its own. Only the host is
/// inspected, so `github.company.example` or a `/gitlab/` path segment is not
/// mistaken for a hosted service; other hosts come from [`ProviderHosts`].
#[must_use]
pub fn provider_from_remote(remote: &str) -> ProviderInfo {
    let host = remote_host(remote).unwrap_or_default();
    if host == "github.com" || host.ends_with(".github.com") {
        provider(ProviderKind::Github, "GitHub", "https://github.com")
    } else if host.contains("gitlab") {
        provider(ProviderKind::Gitlab, "GitLab", &format!("https://{host}"))
    } else if host == "dev.azure.com"
        || host.ends_with(".dev.azure.com")
        || host.ends_with(".visualstudio.com")
    {
        provider(
            ProviderKind::AzureDevops,
            "Azure DevOps",
            "https://dev.azure.com",
        )
    } else if host.contains("bitbucket") {
        provider(
            ProviderKind::Bitbucket,
            "Bitbucket",
            "https://bitbucket.org",
        )
    } else {
        ProviderInfo {
            kind: ProviderKind::Unknown,
            name: "Unknown".into(),
            base_url: String::new(),
        }
    }
}

fn provider(kind: ProviderKind, name: &str, base_url: &str) -> ProviderInfo {
    ProviderInfo {
        kind,
        name: name.into(),
        base_url: base_url.into(),
    }
}

fn non_empty(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn looks_like_host(value: &str) -> bool {
    let without_port = value
        .rsplit_once(':')
        .filter(|(_, port)| port.chars().all(|character| character.is_ascii_digit()))
        .map_or(value, |(host, _)| host);
    !without_port.is_empty()
        && !without_port.contains(char::is_whitespace)
        && (without_port.contains('.') || without_port.starts_with('['))
}

pub fn remote_host(remote: &str) -> Option<String> {
    let value = remote.trim();
    if value.contains("://") {
        let url = url::Url::parse(value).ok()?;
        // IPv6 hosts come back bracketed; scp-like remotes carry them bare.
        return non_empty(url.host_str()?.trim_matches(['[', ']'])).map(|host| host.to_lowercase());
    }
    scp_host_and_path(value).map(|(host, _)| host.to_lowercase())
}

/// Git's scp-like remote, `[user@]host:path` or `[user@][host]:path`: a colon
/// before any slash. A lone letter before the colon is a Windows drive
/// (`C:\repo`), and anything with a slash before the colon is a local path.
fn scp_host_and_path(remote: &str) -> Option<(&str, &str)> {
    let host_start = remote.find('@').map_or(0, |at| at + 1);
    let (user, rest) = remote.split_at(host_start);
    let (host, path) = match rest.strip_prefix('[') {
        Some(bracketed) => bracketed.split_once("]:")?,
        None => rest.split_once(':')?,
    };
    let drive = user.is_empty() && host.len() == 1 && host.as_bytes()[0].is_ascii_alphabetic();
    (!host.is_empty() && !drive && !host.contains(['/', '\\']) && !user.contains(['/', '\\', ':']))
        .then_some((host, path))
}

/// The repository path from an HTTP/SSH URL or an scp-style Git remote.
pub fn remote_repository_path(remote: &str) -> Option<String> {
    let remote = remote.trim();
    let path = if remote.contains("://") {
        let url = url::Url::parse(remote).ok()?;
        url.host_str()?;
        url.path().to_owned()
    } else {
        scp_host_and_path(remote)?.1.to_owned()
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    (!path.is_empty()).then(|| path.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_requests_remote_repository_path_preserves_nested_groups() {
        for (remote, expected) in [
            ("https://host/a/b.git", Some("a/b")),
            ("git@host:a/b.git", Some("a/b")),
            ("ssh://git@host:2222/a/b/c.git", Some("a/b/c")),
            ("https://user@host:8443/a/b/c.git/", Some("a/b/c")),
            ("host:a/b.git", Some("a/b")),
            ("git@[fe80::1]:a/b.git", Some("a/b")),
            ("/local/repo", None),
            ("/local/repo:with-colon", None),
            ("./relative:repo", None),
            ("C:\\work\\repo", None),
            ("C:/work/repo", None),
            ("https://host/", None),
        ] {
            assert_eq!(
                remote_repository_path(remote).as_deref(),
                expected,
                "{remote}"
            );
        }
    }

    #[test]
    fn malformed_github_status_is_not_parsed() {
        assert!(!parse_github_auth_status("not-json").parsed);
    }

    #[test]
    fn recognizes_self_hosted_gitlab() {
        let info = provider_from_remote("ssh://git@gitlab.example.test/team/repo.git");
        assert_eq!(info.kind, ProviderKind::Gitlab);
        assert_eq!(info.base_url, "https://gitlab.example.test");
    }

    #[test]
    fn auth_and_remote_parsers_cover_filtered_accounts_and_host_variants() {
        let github = parse_github_auth_status(
            r#"{"hosts":{"github.com":[
                {"state":"success","active":true,"host":"GITHUB.COM","login":"octo"},
                {"state":"failure","error":" denied ","active":false,"host":"enterprise.test","login":"user"},
                {"state":"success","active":false,"host":" ","login":"ignored"}
            ]}}"#,
        );
        assert!(github.parsed);
        assert_eq!(github.accounts.len(), 2);
        assert_eq!(github.accounts[1].error.as_deref(), Some("denied"));

        let gitlab = parse_gitlab_auth_status(
            "gitlab.example.test:443\n  Logged in to gitlab.example.test as user\n\n[::1]:8443\n  not logged in\n",
        );
        assert_eq!(gitlab.len(), 2);
        assert_eq!(gitlab[0].account.as_deref(), Some("user"));
        assert_eq!(gitlab[1].account, None);

        assert_eq!(
            provider_from_remote("https://github.com/team/repo.git").kind,
            ProviderKind::Github
        );
        assert_eq!(
            provider_from_remote("https://dev.azure.com/team/repo").kind,
            ProviderKind::AzureDevops
        );
        assert_eq!(
            provider_from_remote("git@bitbucket.org:team/repo.git").kind,
            ProviderKind::Bitbucket
        );
        assert_eq!(provider_from_remote("local").kind, ProviderKind::Unknown);
        // Only the host counts: an Enterprise host or a path segment is not a hosted service.
        for remote in [
            "https://github.company.example/team/repo.git",
            "https://git.example.test/gitlab/repo.git",
            "/srv/gitlab/repo.git",
            "https://git.example.test/bitbucket/repo.git",
        ] {
            assert_eq!(
                provider_from_remote(remote).kind,
                ProviderKind::Unknown,
                "{remote}"
            );
        }
        assert_eq!(
            provider_from_remote("git@ssh.github.com:team/repo.git").kind,
            ProviderKind::Github
        );
        // scp-like remotes may omit the user.
        assert_eq!(
            provider_from_remote("github.com:team/repo.git").kind,
            ProviderKind::Github
        );
        let gitlab = provider_from_remote("gitlab.example.test:team/sub/repo.git");
        assert_eq!(
            (gitlab.kind, gitlab.base_url.as_str()),
            (ProviderKind::Gitlab, "https://gitlab.example.test")
        );
        for (remote, host) in [
            ("github.com:team/repo.git", Some("github.com")),
            ("git@Host.Example:team/repo.git", Some("host.example")),
            ("[fe80::1]:team/repo.git", Some("fe80::1")),
            ("ssh://git@[2001:DB8::1]:2222/a/b.git", Some("2001:db8::1")),
            ("https://host/path@thing.git", Some("host")),
            ("C:\\work\\repo", None),
            ("./relative:repo", None),
            ("local", None),
        ] {
            assert_eq!(remote_host(remote).as_deref(), host, "{remote}");
        }
        assert_eq!(
            provider_from_remote("git@ssh.dev.azure.com:v3/org/project/repo").kind,
            ProviderKind::AzureDevops
        );
        assert_eq!(
            provider_from_remote("https://user@gitlab.example.test:8443/team/repo.git").base_url,
            "https://gitlab.example.test"
        );
    }
}
