use std::{ffi::OsString, path::PathBuf, time::Duration};

use serde::{
    Serialize,
    ser::{SerializeMap, Serializer},
};
use tokio_util::sync::CancellationToken;

use crate::git::{OutputPolicy, ProcessOutput, ProcessRequest, ProcessRunner};

use super::{
    ProviderKind, parse_github_auth_status, parse_gitlab_auth_status, provider_cli_environment,
};

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const PROBE_OUTPUT_LIMIT: usize = 8_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WireOption<T>(pub Option<T>);

impl<T> WireOption<T> {
    fn some(value: T) -> Self {
        Self(Some(value))
    }

    fn none() -> Self {
        Self(None)
    }
}

impl<T: Serialize> Serialize for WireOption<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(if self.0.is_some() { 3 } else { 2 }))?;
        map.serialize_entry("_id", "Option")?;
        match self.0.as_ref() {
            Some(value) => {
                map.serialize_entry("_tag", "Some")?;
                map.serialize_entry("value", value)?;
            }
            None => map.serialize_entry("_tag", "None")?,
        }
        map.end()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DiscoveryStatus {
    Available,
    Missing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VcsDiscoveryKind {
    Git,
    Jj,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsDiscoveryItem {
    pub kind: VcsDiscoveryKind,
    pub label: String,
    pub executable: String,
    pub implemented: bool,
    pub status: DiscoveryStatus,
    pub version: WireOption<String>,
    pub install_hint: String,
    pub detail: WireOption<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthStatus {
    Authenticated,
    Unauthenticated,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SourceControlProviderAuthHost {
    pub host: String,
    pub account: Option<String>,
    pub authenticated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SourceControlProviderAuth {
    pub status: AuthStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hosts: Option<Vec<SourceControlProviderAuthHost>>,
    pub account: WireOption<String>,
    pub host: WireOption<String>,
    pub detail: WireOption<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceControlProviderDiscoveryItem {
    pub kind: ProviderKind,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executable: Option<String>,
    pub status: DiscoveryStatus,
    pub version: WireOption<String>,
    pub install_hint: String,
    pub detail: WireOption<String>,
    pub auth: SourceControlProviderAuth,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceControlDiscoveryResult {
    pub version_control_systems: Vec<VcsDiscoveryItem>,
    pub source_control_providers: Vec<SourceControlProviderDiscoveryItem>,
}

struct VcsProbe {
    kind: VcsDiscoveryKind,
    label: &'static str,
    executable: &'static str,
    version_args: &'static [&'static str],
    implemented: bool,
    install_hint: &'static str,
}

struct ProviderProbe {
    kind: ProviderKind,
    label: &'static str,
    executable: &'static str,
    version_args: &'static [&'static str],
    auth_args: &'static [&'static str],
    install_hint: &'static str,
}

const GIT_EXECUTABLE: &str = "git";

const VCS_PROBES: &[VcsProbe] = &[
    VcsProbe {
        kind: VcsDiscoveryKind::Git,
        label: "Git",
        executable: GIT_EXECUTABLE,
        version_args: &["--version"],
        implemented: true,
        install_hint: "Install Git from https://git-scm.com/downloads or with your package manager.",
    },
    VcsProbe {
        kind: VcsDiscoveryKind::Jj,
        label: "Jujutsu",
        executable: "jj",
        version_args: &["--version"],
        implemented: false,
        install_hint: "Install Jujutsu from https://github.com/jj-vcs/jj.",
    },
];

const PROVIDER_PROBES: &[ProviderProbe] = &[
    ProviderProbe {
        kind: ProviderKind::Github,
        label: "GitHub",
        executable: "gh",
        version_args: &["--version"],
        auth_args: &["auth", "status", "--json", "hosts"],
        install_hint: "Install GitHub CLI from https://cli.github.com/.",
    },
    ProviderProbe {
        kind: ProviderKind::Gitlab,
        label: "GitLab",
        executable: "glab",
        version_args: &["--version"],
        auth_args: &["auth", "status"],
        install_hint: "Install GitLab CLI from https://gitlab.com/gitlab-org/cli.",
    },
    ProviderProbe {
        kind: ProviderKind::AzureDevops,
        label: "Azure DevOps",
        executable: "az",
        version_args: &["version"],
        auth_args: &["account", "show", "--output", "json"],
        install_hint: "Install Azure CLI from https://learn.microsoft.com/cli/azure/install-azure-cli.",
    },
];

pub(crate) fn provider_install_hint(kind: ProviderKind) -> Option<&'static str> {
    PROVIDER_PROBES
        .iter()
        .find(|probe| probe.kind == kind)
        .map(|probe| probe.install_hint)
}

#[derive(Clone, Debug, Default)]
pub struct SourceControlDiscovery {
    runner: ProcessRunner,
    /// Tests resolve non-Git probes inside this directory.
    executable_dir: Option<PathBuf>,
}

impl SourceControlDiscovery {
    /// Git always uses the real executable; every other probe is redirected
    /// into this test-owned directory.
    #[doc(hidden)]
    pub fn with_executable_dir_for_integration_test(executable_dir: PathBuf) -> Self {
        assert!(
            executable_dir.is_absolute(),
            "fixture directory must be absolute"
        );
        Self {
            runner: ProcessRunner,
            executable_dir: Some(executable_dir),
        }
    }

    fn command(&self, executable: &str) -> PathBuf {
        if let Some(directory) = &self.executable_dir
            && executable != GIT_EXECUTABLE
        {
            return directory.join(executable);
        }
        PathBuf::from(executable)
    }

    pub async fn discover(
        &self,
        cwd: PathBuf,
        cancellation: &CancellationToken,
    ) -> SourceControlDiscoveryResult {
        let mut version_control_systems = Vec::with_capacity(VCS_PROBES.len());
        for spec in VCS_PROBES {
            let result = self
                .probe(&cwd, spec.executable, spec.version_args, None, cancellation)
                .await;
            let available = result.as_ref().is_some_and(|output| output.exit_code == 0);
            version_control_systems.push(VcsDiscoveryItem {
                kind: spec.kind,
                label: spec.label.into(),
                executable: spec.executable.into(),
                implemented: spec.implemented,
                status: if available {
                    DiscoveryStatus::Available
                } else {
                    DiscoveryStatus::Missing
                },
                version: first_line(result.as_ref()),
                install_hint: spec.install_hint.into(),
                detail: if result.is_none() {
                    WireOption::some("Command was not found on the server PATH.".into())
                } else {
                    WireOption::none()
                },
            });
        }

        let mut source_control_providers = Vec::with_capacity(PROVIDER_PROBES.len() + 1);
        for spec in PROVIDER_PROBES {
            let version = self
                .probe(
                    &cwd,
                    spec.executable,
                    spec.version_args,
                    Some(spec.kind),
                    cancellation,
                )
                .await;
            let available = version.as_ref().is_some_and(|output| output.exit_code == 0);
            let auth = if available {
                let output = self
                    .probe(
                        &cwd,
                        spec.executable,
                        spec.auth_args,
                        Some(spec.kind),
                        cancellation,
                    )
                    .await;
                parse_auth(spec.kind, output.as_ref())
            } else {
                unknown_auth("Hosting integration command was not found on the server PATH.")
            };
            source_control_providers.push(SourceControlProviderDiscoveryItem {
                kind: spec.kind,
                label: spec.label.into(),
                executable: Some(spec.executable.into()),
                status: if available {
                    DiscoveryStatus::Available
                } else {
                    DiscoveryStatus::Missing
                },
                version: first_line(version.as_ref()),
                install_hint: spec.install_hint.into(),
                detail: if version.is_none() {
                    WireOption::some("Command was not found on the server PATH.".into())
                } else {
                    WireOption::none()
                },
                auth,
            });
        }
        source_control_providers.push(SourceControlProviderDiscoveryItem {
            kind: ProviderKind::Bitbucket,
            label: "Bitbucket".into(),
            executable: None,
            status: DiscoveryStatus::Missing,
            version: WireOption::none(),
            install_hint: "Configure Bitbucket API credentials in server settings.".into(),
            detail: WireOption::none(),
            auth: unknown_auth("Bitbucket API credentials are not configured."),
        });
        SourceControlDiscoveryResult {
            version_control_systems,
            source_control_providers,
        }
    }

    async fn probe(
        &self,
        cwd: &std::path::Path,
        executable: &str,
        args: &[&str],
        provider: Option<ProviderKind>,
        cancellation: &CancellationToken,
    ) -> Option<ProcessOutput> {
        self.runner
            .run(
                ProcessRequest {
                    operation: "source-control.discovery.probe".into(),
                    command: self.command(executable),
                    args: args.iter().map(OsString::from).collect(),
                    cwd: cwd.to_path_buf(),
                    env: provider.map(provider_cli_environment).unwrap_or_default(),
                    stdin: None,
                    timeout: PROBE_TIMEOUT,
                    max_output_bytes: PROBE_OUTPUT_LIMIT,
                    output_policy: OutputPolicy::Truncate,
                    append_truncation_marker: true,
                    allow_non_zero_exit: true,
                },
                cancellation,
            )
            .await
            .ok()
    }
}

fn first_line(result: Option<&ProcessOutput>) -> WireOption<String> {
    result
        .into_iter()
        .flat_map(|output| output.stdout.lines().chain(output.stderr.lines()))
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map_or_else(WireOption::none, |line| WireOption::some(line.into()))
}

fn parse_auth(kind: ProviderKind, result: Option<&ProcessOutput>) -> SourceControlProviderAuth {
    let Some(result) = result else {
        return unknown_auth("Authentication status command failed.");
    };
    let combined = [result.stdout.as_str(), result.stderr.as_str()]
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    match kind {
        ProviderKind::Github => {
            let parsed = parse_github_auth_status(&combined);
            let account = parsed
                .accounts
                .iter()
                .find(|account| account.authenticated && account.active)
                .or_else(|| parsed.accounts.iter().find(|account| account.authenticated));
            let mut auth = account.map_or_else(
                || {
                    if parsed.parsed {
                        unauthenticated_auth()
                    } else {
                        unknown_auth("GitHub CLI authentication output was not recognized.")
                    }
                },
                |account| SourceControlProviderAuth {
                    status: AuthStatus::Authenticated,
                    hosts: None,
                    account: WireOption::some(account.account.clone()),
                    host: WireOption::some(account.host.clone()),
                    detail: WireOption::none(),
                },
            );
            if parsed.parsed {
                let mut accounts = parsed.accounts.iter().collect::<Vec<_>>();
                accounts.sort_by(|left, right| {
                    left.host.cmp(&right.host).then_with(|| {
                        (right.authenticated, right.active).cmp(&(left.authenticated, left.active))
                    })
                });
                accounts.dedup_by(|left, right| left.host == right.host);
                auth.hosts = Some(
                    accounts
                        .into_iter()
                        .map(|account| SourceControlProviderAuthHost {
                            host: account.host.clone(),
                            account: Some(account.account.clone()),
                            authenticated: account.authenticated,
                        })
                        .collect(),
                );
            }
            auth
        }
        ProviderKind::Gitlab => {
            let hosts = parse_gitlab_auth_status(&combined);
            if hosts.is_empty() {
                if super::gitlab_auth_status_is_logged_out(&combined) {
                    return SourceControlProviderAuth {
                        hosts: Some(Vec::new()),
                        ..unauthenticated_auth()
                    };
                }
                // Not a host list: an explicit discovery must not read it as "no hosts".
                return unknown_auth("GitLab CLI authentication output was not recognized.");
            }
            let mut auth = hosts
                .iter()
                .find(|host| host.account.is_some())
                .map_or_else(unauthenticated_auth, |host| SourceControlProviderAuth {
                    status: AuthStatus::Authenticated,
                    hosts: None,
                    account: WireOption(host.account.clone()),
                    host: WireOption::some(host.host.clone()),
                    detail: WireOption::none(),
                });
            auth.hosts = Some(
                hosts
                    .into_iter()
                    .map(|host| SourceControlProviderAuthHost {
                        authenticated: host.account.is_some(),
                        host: host.host,
                        account: host.account,
                    })
                    .collect(),
            );
            auth
        }
        ProviderKind::AzureDevops if result.exit_code == 0 => SourceControlProviderAuth {
            status: AuthStatus::Authenticated,
            hosts: None,
            account: WireOption::none(),
            host: WireOption::some("dev.azure.com".into()),
            detail: WireOption::none(),
        },
        _ => unauthenticated_auth(),
    }
}

fn unknown_auth(detail: &str) -> SourceControlProviderAuth {
    SourceControlProviderAuth {
        status: AuthStatus::Unknown,
        hosts: None,
        account: WireOption::none(),
        host: WireOption::none(),
        detail: WireOption::some(detail.into()),
    }
}

fn unauthenticated_auth() -> SourceControlProviderAuth {
    SourceControlProviderAuth {
        status: AuthStatus::Unauthenticated,
        hosts: None,
        account: WireOption::none(),
        host: WireOption::none(),
        detail: WireOption::none(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(exit_code: i32, stdout: &str, stderr: &str) -> ProcessOutput {
        ProcessOutput {
            exit_code,
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
            stdout_truncated: false,
            stderr_truncated: false,
        }
    }

    #[tokio::test]
    async fn discovery_covers_native_probe_inventory() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("bin");
        let discovery =
            SourceControlDiscovery::with_executable_dir_for_integration_test(directory.clone());
        assert_eq!(discovery.command("git"), PathBuf::from("git"));
        for executable in ["jj", "gh", "glab", "az"] {
            assert_eq!(discovery.command(executable), directory.join(executable));
        }
        let result = discovery
            .discover(root.path().to_path_buf(), &CancellationToken::new())
            .await;
        assert_eq!(result.version_control_systems.len(), VCS_PROBES.len());
        assert_eq!(
            result.source_control_providers.len(),
            PROVIDER_PROBES.len() + 1
        );
        assert_eq!(
            result.source_control_providers.last().unwrap().kind,
            ProviderKind::Bitbucket
        );
        assert!(
            result
                .source_control_providers
                .iter()
                .filter(|item| item.executable.is_some())
                .all(|item| item.status == DiscoveryStatus::Missing)
        );
        let jj = result
            .version_control_systems
            .iter()
            .find(|item| item.executable == "jj")
            .expect("Jujutsu probe");
        assert_eq!(jj.status, DiscoveryStatus::Missing);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn discovery_runs_glab_with_telemetry_off() {
        let sandbox = crate::test_support::TestSandbox::new("discovery-glab-telemetry");
        let directory = sandbox.path("bin");
        std::fs::create_dir(&directory).unwrap();
        // Answers only when telemetry is off.
        crate::test_support::TestSandbox::write_executable(
            &directory.join("glab"),
            "#!/bin/sh\n[ \"$GLAB_SEND_TELEMETRY\" = 0 ] || exit 3\necho 'glab 1.122.0'\n",
        );
        let discovery = SourceControlDiscovery::with_executable_dir_for_integration_test(directory);
        let result = discovery
            .discover(sandbox.root().to_path_buf(), &CancellationToken::new())
            .await;
        let gitlab = result
            .source_control_providers
            .iter()
            .find(|item| item.kind == ProviderKind::Gitlab)
            .expect("GitLab probe");
        assert_eq!(gitlab.status, DiscoveryStatus::Available);
    }

    #[test]
    fn gitlab_logout_is_unauthenticated_but_unrecognized_output_is_unknown() {
        // Captured with an isolated GLAB_CONFIG_DIR and an empty hosts map.
        let logged_out = parse_auth(
            ProviderKind::Gitlab,
            Some(&output(
                1,
                "",
                include_str!("../../tests/fixtures/pull_requests/glab_logged_out.txt"),
            )),
        );
        assert_eq!(logged_out.status, AuthStatus::Unauthenticated);
        assert_eq!(logged_out.hosts, Some(Vec::new()));

        let unknown = parse_auth(
            ProviderKind::Gitlab,
            Some(&output(
                1,
                "",
                "error: unexpected response from the keyring\n",
            )),
        );
        assert_eq!(unknown.status, AuthStatus::Unknown);
        assert_eq!(unknown.hosts, None);
    }

    #[test]
    fn gitlab_logout_recognizes_wrapped_core_phrase_without_punctuation() {
        for text in [
            "No GitLab instances have been authenticated with glab",
            "ERROR\n\n No GitLab instances have\n been authenticated\twith glab.\n",
            "NO GITLAB INSTANCES HAVE BEEN AUTHENTICATED WITH GLAB: run auth login",
        ] {
            let auth = parse_auth(ProviderKind::Gitlab, Some(&output(1, "", text)));
            assert_eq!(auth.status, AuthStatus::Unauthenticated, "{text}");
            assert_eq!(auth.hosts, Some(Vec::new()), "{text}");
        }
    }

    /// Each glab release family words the full logout differently; the strings are
    /// verbatim from glab's `auth/status/status.go` at the cited tags.
    #[test]
    fn gitlab_logout_recognizes_every_known_glab_phrasing() {
        for (versions, text) in [
            (
                "v1.36.0, v1.39.0",
                "No GitLab instance has been authenticated with glab. Run `glab auth login` to authenticate.\n",
            ),
            (
                "v1.43.0 to v1.100.0",
                "No GitLab instances have been authenticated with glab. Run `glab auth login` to authenticate.\n",
            ),
            (
                "v1.110.0, captured with 1.114.0",
                include_str!("../../tests/fixtures/pull_requests/glab_logged_out.txt"),
            ),
        ] {
            let auth = parse_auth(ProviderKind::Gitlab, Some(&output(1, "", text)));
            assert_eq!(auth.status, AuthStatus::Unauthenticated, "{versions}");
            assert_eq!(auth.hosts, Some(Vec::new()), "{versions}");
        }

        // One host's refusal (`auth status --hostname`) is not a full logout.
        let one_host = parse_auth(
            ProviderKind::Gitlab,
            Some(&output(
                1,
                "",
                "x gitlab.invalid has not been authenticated with glab; run `glab auth login --hostname gitlab.invalid` to authenticate\n",
            )),
        );
        assert_eq!(one_host.status, AuthStatus::Unknown);
        assert_eq!(one_host.hosts, None);
    }

    #[test]
    fn gitlab_configured_host_without_token_is_unauthenticated_not_logged_out() {
        // glab 1.114.0 captured offline: a user and network namespace, an isolated
        // GLAB_CONFIG_DIR that configures `gitlab.invalid` without a token, and no
        // reachable keyring. The host stays listed, unauthenticated.
        let auth = parse_auth(
            ProviderKind::Gitlab,
            Some(&output(
                1,
                "",
                include_str!("../../tests/fixtures/pull_requests/glab_no_token.txt"),
            )),
        );
        assert_eq!(auth.status, AuthStatus::Unauthenticated);
        assert_eq!(auth.account, WireOption::none());
        assert_eq!(
            auth.hosts,
            Some(vec![SourceControlProviderAuthHost {
                host: "gitlab.invalid".to_owned(),
                account: None,
                authenticated: false,
            }])
        );
    }

    #[test]
    fn auth_and_wire_helpers_cover_success_unknown_and_empty_results() {
        assert_eq!(first_line(None), WireOption::none());
        assert_eq!(
            first_line(Some(&output(0, "\n version 1 \n", "ignored"))),
            WireOption::some("version 1".to_owned())
        );
        assert_eq!(
            first_line(Some(&output(1, "", " error detail "))),
            WireOption::some("error detail".to_owned())
        );

        let github = parse_auth(
            ProviderKind::Github,
            Some(&output(
                0,
                r#"{"hosts":{"github.com":[{"state":"success","active":true,"host":"github.com","login":"octo"},{"state":"success","active":false,"host":"github.com","login":"inactive"}],"company.test":[{"state":"error","active":false,"host":"company.test","login":"old"}]}}"#,
                "",
            )),
        );
        assert_eq!(github.status, AuthStatus::Authenticated);
        assert_eq!(github.account.0.as_deref(), Some("octo"));
        assert_eq!(
            serde_json::to_value(&github).unwrap()["hosts"],
            serde_json::json!([
                { "host": "company.test", "account": "old", "authenticated": false },
                { "host": "github.com", "account": "octo", "authenticated": true }
            ])
        );
        assert_eq!(
            parse_auth(
                ProviderKind::Github,
                Some(&output(1, r#"{"hosts":{}}"#, "")),
            )
            .status,
            AuthStatus::Unauthenticated
        );
        assert_eq!(
            parse_auth(ProviderKind::Github, Some(&output(1, "unrecognized", "")),).status,
            AuthStatus::Unknown
        );

        let gitlab = parse_auth(
            ProviderKind::Gitlab,
            Some(&output(
                0,
                "gitlab.example.test\n  Logged in to gitlab.example.test as user\ncompany.test\n  not logged in\n",
                "",
            )),
        );
        assert_eq!(gitlab.status, AuthStatus::Authenticated);
        assert_eq!(gitlab.account.0.as_deref(), Some("user"));
        assert_eq!(
            serde_json::to_value(&gitlab).unwrap()["hosts"],
            serde_json::json!([
                { "host": "gitlab.example.test", "account": "user", "authenticated": true },
                { "host": "company.test", "account": null, "authenticated": false }
            ])
        );
        assert_eq!(
            parse_auth(
                ProviderKind::Gitlab,
                Some(&output(1, "gitlab.example.test\n  not logged in\n", "")),
            )
            .status,
            AuthStatus::Unauthenticated
        );
        let unrecognized = parse_auth(
            ProviderKind::Gitlab,
            Some(&output(
                1,
                "",
                "error: unexpected response from the keyring\n",
            )),
        );
        assert_eq!(unrecognized.status, AuthStatus::Unknown);
        assert_eq!(
            unrecognized.hosts, None,
            "an unrecognized answer lists no hosts"
        );
        assert_eq!(
            parse_auth(ProviderKind::AzureDevops, Some(&output(0, "{}", ""))).status,
            AuthStatus::Authenticated
        );
        assert_eq!(
            parse_auth(ProviderKind::AzureDevops, Some(&output(1, "", ""))).status,
            AuthStatus::Unauthenticated
        );
        assert_eq!(
            parse_auth(ProviderKind::Github, None).status,
            AuthStatus::Unknown
        );

        assert_eq!(
            serde_json::to_value(WireOption::some("value")).unwrap(),
            serde_json::json!({"_id":"Option","_tag":"Some","value":"value"})
        );
        assert_eq!(
            serde_json::to_value(WireOption::<String>::none()).unwrap(),
            serde_json::json!({"_id":"Option","_tag":"None"})
        );
    }
}
