#![cfg(unix)]

#[path = "support/executable_fixture.rs"]
mod executable_fixture;

use std::{
    fs,
    path::{Path, PathBuf},
};

use coddy_studio_server::pull_requests::{
    ContextRead, PullRequestsService,
    host::HostCommandRunner,
    model::{ListQuery, VocabularyKind},
};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

use coddy_studio_server::{
    RequestId, RpcRegistry, RpcRequest, ServerConfig, ServerRuntime,
    production::pull_requests_rpc::{
        ConfiguredPullRequestsRpcServices, register_pull_requests_rpc,
    },
};
use futures_util::SinkExt;
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[path = "support/websocket_frames.rs"]
mod websocket_frames;
use websocket_frames::next_frame_past_heartbeat;

const GITHUB_ORIGIN: &str = "https://github.com/example/repository.git";
const GITLAB_ORIGIN: &str = "ssh://git@git.acme.example/team/sub/repo.git";

struct Fixture {
    root: TempDir,
    cwd: PathBuf,
    runner: HostCommandRunner,
    service: PullRequestsService,
}

impl Fixture {
    async fn new(origin: Option<&str>) -> Self {
        let root = TempDir::new().unwrap();
        let cwd = root.path().join("checkout");
        fs::create_dir(&cwd).unwrap();
        let (gh, glab) = ProviderStub::install(root.path());
        let runner =
            HostCommandRunner::new(root.path().join("state")).with_commands(gh, glab, "git");
        let c = CancellationToken::new();
        runner
            .git(&cwd, &["init", "-q", "-b", "main"], &c)
            .await
            .unwrap();
        if let Some(origin) = origin {
            runner
                .git(&cwd, &["remote", "add", "origin", origin], &c)
                .await
                .unwrap();
        }
        let service = PullRequestsService::with_runner(runner.clone());
        Self {
            root,
            cwd,
            runner,
            service,
        }
    }

    async fn context(&self) -> Value {
        serde_json::to_value(
            self.service
                .context(&self.cwd, ContextRead::Rescan, &CancellationToken::new())
                .await
                .unwrap(),
        )
        .unwrap()
    }

    fn query(&self) -> ListQuery {
        serde_json::from_value(json!({"cwd":self.cwd,"state":"open","search":null,"author":null,"assignee":null,"reviewer":null,"reviewStatus":null,"draft":null,"labels":[],"milestone":null,"targetBranch":null,"sort":"newest","cursor":null})).unwrap()
    }

    fn mark(&self, name: &str) {
        fs::write(self.root.path().join(name), "").unwrap();
    }
    fn calls(&self, provider: &str) -> String {
        fs::read_to_string(self.root.path().join(format!("{provider}-calls"))).unwrap_or_default()
    }
}

struct ProviderStub;

impl ProviderStub {
    // Same inline executable-script seam as production_git_manager_rpc.rs.
    // Integration tests cannot access the crate-private TestSandbox helper.
    fn install(root: &Path) -> (PathBuf, PathBuf) {
        write_json(
            root,
            "gh-auth.json",
            &json!({"hosts":{"github.com":[{"state":"success","active":true,"host":"github.com","login":"mubeda"}]}}),
        );
        fs::write(root.join("glab-auth"), "gitlab.com\n  x HTTP 401\ngit.acme.example\n  ✓ Logged in to git.acme.example as alice\n").unwrap();
        fs::write(
            root.join("github-repository.json"),
            include_str!("fixtures/pull_requests/github_repository.json"),
        )
        .unwrap();
        fs::write(
            root.join("gitlab-project.json"),
            include_str!("fixtures/pull_requests/gitlab_project.json"),
        )
        .unwrap();
        let row: Value =
            serde_json::from_str(include_str!("fixtures/pull_requests/github_row.json")).unwrap();
        let mut node = row.clone();
        node["labels"] = json!({"nodes":row["labels"]});
        node["totalCommentsCount"] = json!(3);
        node["commits"] = json!({"nodes":[{"commit":{"statusCheckRollup":{"state":"FAILURE"}}}]});
        node.as_object_mut().unwrap().remove("statusCheckRollup");
        let mut second = node.clone();
        second["number"] = json!(15);
        second["state"] = json!("OPEN");
        second["mergedAt"] = Value::Null;
        second["closedAt"] = Value::Null;
        write_json(
            root,
            "github-graphql.json",
            &json!({"data":{"repository":{"viewerPermission":"ADMIN","pullRequests":{"totalCount":4,"pageInfo":{"hasNextPage":true,"endCursor":"end-cursor"},"nodes":[node,second]},"closed":{"totalCount":8}}}}),
        );
        write_json(root, "github-search.json", &json!([row]));
        let row: Value =
            serde_json::from_str(include_str!("fixtures/pull_requests/gitlab_row.json")).unwrap();
        write_json(
            root,
            "gitlab-list.json",
            &json!(
                (1..=30)
                    .map(|number| {
                        let mut row = row.clone();
                        row["iid"] = json!(number);
                        row
                    })
                    .collect::<Vec<_>>()
            ),
        );
        let gh = Self::script(
            root,
            "gh",
            r#"
printf '%s|%s\n' "$GH_HOST" "$*" >> "$FIXTURE_DIR/gh-calls"
case "$1 $2" in
  '--version ') if [ -e "$FIXTURE_DIR/gh-missing" ]; then exit 1; fi; echo 'gh version 2.97.0' ;;
  'auth status')
    if [ -e "$FIXTURE_DIR/gh-unauthenticated" ]; then echo 'You are not logged into any GitHub hosts' >&2; exit 1; fi
    cat "$FIXTURE_DIR/gh-auth.json" ;;
  'api user') echo '{"login":"mubeda","name":""}' ;;
  'api graphql')
    cat > "$FIXTURE_DIR/graphql-input"
    if [ -e "$FIXTURE_DIR/rate-limited" ]; then echo 'API rate limit exceeded TOKEN secret' >&2; exit 1; fi
    cat "$FIXTURE_DIR/github-graphql.json" ;;
  'api repos/example/repository')
    if [ -e "$FIXTURE_DIR/repository-forbidden" ]; then echo 'HTTP 403 forbidden' >&2; exit 1; fi
    cat "$FIXTURE_DIR/github-repository.json" ;;
  'api repos/example/repository/labels?per_page=100') echo '[{"name":"bug","color":"d73a4a","description":"Bug fix"}]' ;;
  'api repos/example/repository/milestones?state=open&per_page=100') echo '[{"number":3,"title":"Release","description":null}]' ;;
  'api repos/example/repository/collaborators?per_page=100') echo '[{"login":"alice","name":"Alice"}]' ;;
  'api repos/example/repository/branches?per_page=100') echo '[{"name":"main"}]' ;;
  'pr list') cat "$FIXTURE_DIR/github-search.json" ;;
  *) echo 'unexpected fixture command' >&2; exit 64 ;;
esac
"#,
        );
        let glab = Self::script(
            root,
            "glab",
            r##"
printf '%s|%s\n' "$GITLAB_HOST" "$*" >> "$FIXTURE_DIR/glab-calls"
case "$1 $2" in
  '--version ') echo 'glab 1.114.0' ;;
  'auth status') cat "$FIXTURE_DIR/glab-auth" ;;
  'api user') echo '{"username":"alice","name":"Alice","id":7}' ;;
  'api version') echo '{"version":"17.9.1"}' ;;
  'api projects/team%2Fsub%2Frepo') cat "$FIXTURE_DIR/gitlab-project.json" ;;
  'api projects/team%2Fsub%2Frepo/labels?per_page=100') echo '[{"id":1,"name":"bug","color":"#d73a4a","description":"Bug fix"}]' ;;
  'api projects/team%2Fsub%2Frepo/milestones?state=active&per_page=100') echo '[{"id":3,"title":"Release","description":null}]' ;;
  'api projects/team%2Fsub%2Frepo/members/all?per_page=100') echo '[{"id":7,"username":"alice","name":"Alice"}]' ;;
  'api projects/team%2Fsub%2Frepo/repository/branches?per_page=100') echo '[{"name":"main"}]' ;;
  'api -i')
    if [ -e "$FIXTURE_DIR/no-counts" ]; then exit 1; fi
    printf 'HTTP/2 200 OK\r\nx-total: 42\r\n\r\n[]' ;;
  'mr list')
    case " $* " in *' --state '*) echo 'Unknown flag: --state' >&2; exit 1 ;; esac
    cat "$FIXTURE_DIR/gitlab-list.json" ;;
  *) echo 'unexpected fixture command' >&2; exit 64 ;;
esac
"##,
        );
        (gh, glab)
    }

    fn script(root: &Path, name: &str, body: &str) -> PathBuf {
        let path = root.join(name);
        let directory = root.to_string_lossy().replace('\'', "'\\''");
        executable_fixture::write_executable(
            &path,
            format!("#!/bin/sh\nFIXTURE_DIR='{directory}'\n{body}"),
        );
        path
    }
}

fn write_json(root: &Path, name: &str, value: &Value) {
    fs::write(root.join(name), value.to_string()).unwrap();
}

#[tokio::test]
async fn pull_requests_github_context_mirrors_the_phase_zero_wire_contract() {
    let f = Fixture::new(Some(GITHUB_ORIGIN)).await;
    let context = f.context().await;
    assert_eq!(context["status"], "available");
    assert_eq!(context["provider"], "github");
    assert_eq!(context["repository"], "example/repository");
    assert_eq!(context["repositoryPermission"], "admin");
    assert_eq!(
        context["mergePolicy"]["methods"],
        json!(["merge", "squash", "rebase"])
    );
    assert_eq!(context["capabilities"]["dismissReview"], true);
    assert!(f.calls("glab").is_empty());
}

#[tokio::test]
async fn pull_requests_custom_gitlab_host_is_discovered_and_every_call_has_gitlab_host() {
    let f = Fixture::new(Some(GITLAB_ORIGIN)).await;
    let context = f.context().await;
    assert_eq!(context["status"], "available");
    assert_eq!(context["provider"], "gitlab");
    assert_eq!(context["host"], "git.acme.example");
    assert_eq!(context["repository"], "team/sub/repo");
    assert_eq!(context["hostVersion"], "17.9.1");
    assert_eq!(context["capabilities"]["requestChanges"], true);
    assert_eq!(context["capabilities"]["removeOwnChangeRequest"], true);
    let calls = f.calls("glab");
    assert!(!calls.is_empty());
    assert!(
        calls
            .lines()
            .all(|line| line.starts_with("git.acme.example|")),
        "{calls}"
    );
    assert!(
        calls
            .lines()
            .filter(|line| line.contains("|api "))
            .all(|line| line.contains("--hostname git.acme.example"))
    );
}

/// Opening the panel or switching checkout reuses the bounded answers (probes, the
/// recorded host and the host context); Rescan clears them and asks every probe again.
#[tokio::test]
async fn pull_requests_opening_reuses_context_answers_and_rescan_rereads_them() {
    let f = Fixture::new(Some(GITLAB_ORIGIN)).await;
    let c = CancellationToken::new();
    let count = |provider: &str| f.calls(provider).lines().count();
    let first = f
        .service
        .context(&f.cwd, ContextRead::Open, &c)
        .await
        .unwrap();
    // Cold: gh and glab discovery, `glab --version`, then user, project and version.
    // Discovery already proved the login, so no `auth status --hostname` follows.
    let cold = (count("gh"), count("glab"));
    assert_eq!(
        cold,
        (1, 5),
        "gh:\n{}\nglab:\n{}",
        f.calls("gh"),
        f.calls("glab")
    );
    let warm = f
        .service
        .context(&f.cwd, ContextRead::Open, &c)
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&warm).unwrap(),
        serde_json::to_value(&first).unwrap()
    );
    assert_eq!(
        (count("gh"), count("glab")),
        cold,
        "a warm open starts no provider process"
    );
    f.service
        .context(&f.cwd, ContextRead::Rescan, &c)
        .await
        .unwrap();
    assert_eq!(
        (count("gh"), count("glab")),
        (cold.0 + 1, cold.1 + 5),
        "Rescan re-reads discovery, the CLI and the host context"
    );
}

#[tokio::test]
async fn pull_requests_expired_gitlab_login_overlaps_reads_and_keeps_auth_error_precedence() {
    for (read, fail, page_fails) in [
        ("list", false, false),
        ("context", false, false),
        ("list", true, false),
        ("context", true, false),
        ("list", true, true),
        ("context", true, true),
    ] {
        let f = Fixture::new(Some(GITLAB_ORIGIN)).await;
        fs::write(f.root.path().join("read"), read).unwrap();
        let glab = ProviderStub::script(
            f.root.path(),
            "glab-barrier",
            r#"
wait_for() {
  i=0
  while [ ! -e "$FIXTURE_DIR/$1" ]; do
    i=$((i + 1)); if [ "$i" -gt 40 ]; then echo 'read ran alone' >&2; exit 75; fi
    sleep 0.025
  done
}
if [ -e "$FIXTURE_DIR/recheck" ]; then
  case "$1 $2" in
    'auth status')
      : > "$FIXTURE_DIR/auth-started"
      wait_for page-finished
      if [ -e "$FIXTURE_DIR/fail" ]; then echo 'No GitLab instances have been authenticated with glab; run glab auth login to authenticate.' >&2; exit 1; fi ;;
    'mr list'|'api user')
      if [ "$1 $2" = 'mr list' ] && [ "$(cat "$FIXTURE_DIR/read")" != list ]; then exit 64; fi
      wait_for auth-started
      : > "$FIXTURE_DIR/page-finished"
      if [ -e "$FIXTURE_DIR/page-fails" ]; then echo 'HTTP 403 forbidden' >&2; exit 1; fi ;;
  esac
fi
exec "$FIXTURE_DIR/glab" "$@"
"#,
        );
        let hosts =
            std::sync::Arc::new(coddy_studio_server::source_control::ProviderHosts::default());
        let service = PullRequestsService::with_runner(
            f.runner
                .clone()
                .with_provider_hosts(hosts.clone())
                .with_commands(f.root.path().join("gh"), glab, "git"),
        );
        let c = CancellationToken::new();
        service
            .context(&f.cwd, ContextRead::Open, &c)
            .await
            .unwrap();
        service.list(&f.cwd, f.query(), &c).await.unwrap();
        tokio::time::pause();
        tokio::time::advance(std::time::Duration::from_secs(31)).await;
        tokio::time::resume();
        f.mark("recheck");
        if fail {
            f.mark("fail");
        }
        if page_fails {
            f.mark("page-fails");
        }
        if read == "list" {
            let page = service.list(&f.cwd, f.query(), &c).await;
            if fail {
                assert_eq!(page.unwrap_err().code, "not_authenticated");
            } else {
                assert_eq!(page.unwrap().rows.len(), 30);
            }
        } else {
            let context = serde_json::to_value(
                service
                    .context(&f.cwd, ContextRead::Open, &c)
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(
                context["status"],
                if fail { "unavailable" } else { "available" }
            );
            if fail {
                assert_eq!(context["code"], "not_authenticated");
            }
        }
        assert!(
            f.root.path().join("page-finished").exists(),
            "{read}: page and login must overlap"
        );
        if fail {
            assert_eq!(hosts.provider("git.acme.example"), None);
        }
    }
}

#[tokio::test]
async fn pull_requests_unconfigured_custom_host_has_the_required_auth_command() {
    let f = Fixture::new(Some(GITLAB_ORIGIN)).await;
    fs::write(
        f.root.path().join("glab-auth"),
        "gitlab.com\n  x HTTP 401\n",
    )
    .unwrap();
    let context = f.context().await;
    assert_eq!(context["status"], "unavailable");
    assert_eq!(context["code"], "unknown_host");
    assert_eq!(
        context["authCommand"],
        "glab auth login --hostname git.acme.example"
    );
}

#[tokio::test]
async fn pull_requests_failed_login_cancels_and_reaps_inflight_context_and_list_reads() {
    for read in ["context", "list"] {
        let f = Fixture::new(Some("https://github.com/example/repository.git")).await;
        let gh = ProviderStub::script(
            f.root.path(),
            "gh-login-cancels-read",
            r#"
case "$1 $2" in
  'auth status')
    i=0
    while [ ! -e "$FIXTURE_DIR/read-pid" ]; do
      i=$((i + 1)); if [ "$i" -gt 80 ]; then exit 75; fi
      sleep 0.025
    done
    echo 'You are not logged into any GitHub hosts' >&2
    exit 1 ;;
  'api user'|'api graphql')
    if [ "$1 $2" = 'api user' ] || [ -e "$FIXTURE_DIR/list-read" ]; then
      printf '%s' "$$" > "$FIXTURE_DIR/read-pid"
      exec sleep 20
    fi ;;
esac
exec "$FIXTURE_DIR/gh" "$@"
"#,
        );
        if read == "list" {
            f.mark("list-read");
        }
        let service = PullRequestsService::with_runner(f.runner.clone().with_commands(
            gh,
            f.root.path().join("glab"),
            "git",
        ));
        let cancellation = CancellationToken::new();
        let operation = async {
            if read == "context" {
                match service
                    .context(&f.cwd, ContextRead::Open, &cancellation)
                    .await
                {
                    Ok(context) => serde_json::to_value(context).unwrap()["code"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                    Err(error) => error.code.to_owned(),
                }
            } else {
                service
                    .list(&f.cwd, f.query(), &cancellation)
                    .await
                    .unwrap_err()
                    .code
                    .to_owned()
            }
        };
        tokio::pin!(operation);
        let completed = tokio::select! {
            result = &mut operation => Some(result),
            () = tokio::time::sleep(std::time::Duration::from_secs(3)) => None,
        };
        if completed.is_none() {
            // Drain this test's child even on the regression path.
            cancellation.cancel();
            let _ = operation.await;
        }
        assert_eq!(
            completed.as_deref(),
            Some("not_authenticated"),
            "{read}: login failure must not wait for the provider read"
        );
        let pid: libc::pid_t = fs::read_to_string(f.root.path().join("read-pid"))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "{read}: provider child was not reaped before returning"
        );
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
        assert!(
            !cancellation.is_cancelled(),
            "the parent request token stays usable"
        );
    }
}

#[tokio::test]
async fn pull_requests_list_rechecks_checkout_after_successful_provider_read() {
    let f = Fixture::new(Some(GITHUB_ORIGIN)).await;
    let gh = ProviderStub::script(
        f.root.path(),
        "gh-list-removes-checkout",
        r#"
case "$1 $2" in
  'api graphql')
    i=0
    while [ ! -e "$FIXTURE_DIR/auth-finished" ]; do
      i=$((i + 1)); if [ "$i" -gt 80 ]; then exit 75; fi
      sleep 0.025
    done
    mv "$FIXTURE_DIR/checkout" "$FIXTURE_DIR/moved-checkout" ;;
  'auth status') : > "$FIXTURE_DIR/auth-finished" ;;
esac
exec "$FIXTURE_DIR/gh" "$@"
"#,
    );
    let service = PullRequestsService::with_runner(f.runner.clone().with_commands(
        gh,
        f.root.path().join("glab"),
        "git",
    ));
    let error = service
        .list(&f.cwd, f.query(), &CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code, "unavailable");
    assert!(error.message.contains("no longer exists"));
}

#[tokio::test]
async fn pull_requests_unsupported_provider_and_no_origin_are_context_data() {
    for (origin, code) in [
        (
            Some("https://dev.azure.com/org/repo"),
            "unsupported_provider",
        ),
        (
            Some("git@bitbucket.org:team/repo.git"),
            "unsupported_provider",
        ),
        (None, "no_remote"),
    ] {
        let f = Fixture::new(origin).await;
        assert_eq!(f.context().await["code"], code);
        assert!(f.calls("gh").is_empty() && f.calls("glab").is_empty());
    }
}

#[tokio::test]
async fn pull_requests_missing_checkout_is_context_data() {
    let f = Fixture::new(Some(GITHUB_ORIGIN)).await;
    fs::remove_dir_all(&f.cwd).unwrap();
    let context = f.context().await;
    assert_eq!(context["status"], "unavailable");
    assert_eq!(context["code"], "repository_unreachable");
    assert_eq!(
        context["message"],
        format!(
            "The selected checkout `{}` no longer exists on this environment. Select another checkout or restore this directory, then rescan.",
            f.cwd.display()
        )
    );
    assert!(context["installHint"].is_null());
    assert!(f.calls("gh").is_empty() && f.calls("glab").is_empty());
}

#[tokio::test]
async fn pull_requests_non_directory_checkout_is_context_data() {
    let f = Fixture::new(Some(GITHUB_ORIGIN)).await;
    fs::remove_dir_all(&f.cwd).unwrap();
    fs::write(&f.cwd, "a file, not a checkout").unwrap();
    for cwd in [f.cwd.clone(), f.cwd.join("nested")] {
        let context = serde_json::to_value(
            f.service
                .context(&cwd, ContextRead::Rescan, &CancellationToken::new())
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(context["status"], "unavailable");
        assert_eq!(context["code"], "repository_unreachable");
        assert_eq!(
            context["message"],
            format!(
                "The selected checkout `{}` is not a directory on this environment. Select another checkout, then rescan.",
                cwd.display()
            )
        );
        assert!(context["installHint"].is_null());
    }
    assert!(f.calls("gh").is_empty() && f.calls("glab").is_empty());
}

#[tokio::test]
async fn pull_requests_checkout_removed_during_resolution_is_not_a_missing_cli() {
    let f = Fixture::new(Some(GITHUB_ORIGIN)).await;
    let git = ProviderStub::script(
        f.root.path(),
        "git-removes-checkout",
        r#"
mv "$FIXTURE_DIR/checkout" "$FIXTURE_DIR/moved-checkout"
printf '%s\n' 'https://github.com/example/repository.git'
"#,
    );
    let service = PullRequestsService::with_runner(f.runner.clone().with_commands(
        f.root.path().join("gh"),
        f.root.path().join("glab"),
        git,
    ));
    let context = serde_json::to_value(
        service
            .context(&f.cwd, ContextRead::Rescan, &CancellationToken::new())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(context["code"], "repository_unreachable");
    assert!(
        context["message"]
            .as_str()
            .unwrap()
            .contains("no longer exists on this environment")
    );
    assert!(context["installHint"].is_null());
}

#[tokio::test]
async fn pull_requests_checkout_removed_during_host_context_is_not_a_missing_cli() {
    let f = Fixture::new(Some(GITHUB_ORIGIN)).await;
    // The host context reads start together, so the checkout disappears during the
    // login check that precedes them: each of them then fails to start.
    let gh = ProviderStub::script(
        f.root.path(),
        "gh-removes-checkout",
        r#"
if [ "$1 $2 $3" = 'auth status --hostname' ]; then
  mv "$FIXTURE_DIR/checkout" "$FIXTURE_DIR/moved-checkout"
fi
exec "$FIXTURE_DIR/gh" "$@"
"#,
    );
    let service = PullRequestsService::with_runner(f.runner.clone().with_commands(
        gh,
        f.root.path().join("glab"),
        "git",
    ));
    let context = serde_json::to_value(
        service
            .context(&f.cwd, ContextRead::Rescan, &CancellationToken::new())
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(context["code"], "repository_unreachable");
    assert!(
        context["message"]
            .as_str()
            .unwrap()
            .contains("no longer exists on this environment")
    );
    assert!(context["installHint"].is_null());
}

#[tokio::test]
async fn pull_requests_github_checkout_disappearing_mid_joined_context_is_unavailable() {
    let f = Fixture::new(Some(GITHUB_ORIGIN)).await;
    let gh = ProviderStub::script(
        f.root.path(),
        "gh-mid-read-removal",
        r#"
wait_for() {
  i=0
  while [ ! -e "$FIXTURE_DIR/$1" ]; do
    i=$((i + 1)); if [ "$i" -gt 80 ]; then echo 'context reads did not overlap' >&2; exit 75; fi
    sleep 0.025
  done
}
if [ -e "$FIXTURE_DIR/remove-during-read" ]; then
  case "$1 $2" in
    'api user')
      : > "$FIXTURE_DIR/user-started"
      wait_for repository-started
      wait_for viewer-started
      mv "$FIXTURE_DIR/checkout" "$FIXTURE_DIR/moved-checkout"
      : > "$FIXTURE_DIR/checkout-removed" ;;
    'api repos/example/repository')
      : > "$FIXTURE_DIR/repository-started"
      wait_for checkout-removed ;;
    'api graphql')
      : > "$FIXTURE_DIR/viewer-started"
      wait_for checkout-removed ;;
  esac
fi
exec "$FIXTURE_DIR/gh" "$@"
"#,
    );
    let service = PullRequestsService::with_runner(f.runner.clone().with_commands(
        gh,
        f.root.path().join("glab"),
        "git",
    ));
    let c = CancellationToken::new();
    service
        .context(&f.cwd, ContextRead::Open, &c)
        .await
        .unwrap();
    f.mark("remove-during-read");
    let context = serde_json::to_value(
        service
            .context(&f.cwd, ContextRead::Open, &c)
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(f.root.path().join("checkout-removed").exists());
    assert_eq!(context["code"], "repository_unreachable");
    assert!(
        context["message"]
            .as_str()
            .unwrap()
            .contains("no longer exists")
    );
    assert!(context["installHint"].is_null());
}

#[tokio::test]
async fn pull_requests_github_auth_missing_cli_and_repository_access_failures_are_actionable() {
    for (marker, code) in [
        ("gh-unauthenticated", "not_authenticated"),
        ("gh-missing", "cli_missing"),
        ("repository-forbidden", "repository_unreachable"),
    ] {
        let f = Fixture::new(Some(GITHUB_ORIGIN)).await;
        f.mark(marker);
        let context = f.context().await;
        assert_eq!(context["code"], code);
        if code == "not_authenticated" {
            assert_eq!(
                context["authCommand"],
                "gh auth login --hostname github.com"
            );
        }
        if code == "cli_missing" {
            assert_eq!(
                context["installHint"],
                "Install GitHub CLI from https://cli.github.com/."
            );
        }
    }
    let f = Fixture::new(Some(GITHUB_ORIGIN)).await;
    let service = PullRequestsService::with_runner(f.runner.clone().with_commands(
        f.root.path().join("missing"),
        f.root.path().join("glab"),
        "git",
    ));
    assert_eq!(
        serde_json::to_value(
            service
                .context(&f.cwd, ContextRead::Rescan, &CancellationToken::new())
                .await
                .unwrap()
        )
        .unwrap()["code"],
        "cli_missing"
    );
}

#[tokio::test]
async fn pull_requests_list_pages_preserve_graphql_cursor_and_gitlab_page_number() {
    let gh = Fixture::new(Some(GITHUB_ORIGIN)).await;
    let page = gh
        .service
        .list(&gh.cwd, gh.query(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 2);
    assert_eq!(page.next_cursor.as_deref(), Some("end-cursor"));
    assert_eq!(page.counts.unwrap().open, Some(4));
    let glab = Fixture::new(Some(GITLAB_ORIGIN)).await;
    let page = glab
        .service
        .list(&glab.cwd, glab.query(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(page.rows.len(), 30);
    assert_eq!(page.next_cursor.as_deref(), Some("2"));
    assert!(glab.calls("glab").contains("--page 1"));
    assert!(
        glab.calls("glab")
            .contains("--repo git.acme.example/team/sub/repo")
    );
}

#[tokio::test]
async fn pull_requests_gitlab_filtered_total_is_unknown_while_tab_counts_remain_available() {
    let f = Fixture::new(Some(GITLAB_ORIGIN)).await;
    for (field, value, argument) in [
        ("author", json!("alice"), "--author alice"),
        ("assignee", json!("alice"), "--assignee alice"),
        ("reviewer", json!("alice"), "--reviewer alice"),
        ("labels", json!(["bug"]), "--label bug"),
        ("milestone", json!("3"), "--milestone 3"),
        ("targetBranch", json!("main"), "--target-branch main"),
        ("draft", json!("only"), "--draft"),
        ("search", json!("fix"), "--search fix"),
    ] {
        let mut query = serde_json::to_value(f.query()).unwrap();
        query[field] = value;
        let page = f
            .service
            .list(
                &f.cwd,
                serde_json::from_value(query).unwrap(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(page.total_count, None, "{field}");
        assert_eq!(page.counts.unwrap().open, Some(42));
        assert!(
            f.calls("glab")
                .lines()
                .rfind(|line| line.contains("|mr list"))
                .unwrap()
                .contains(argument)
        );
    }
}

#[tokio::test]
async fn pull_requests_gitlab_approval_filters_report_unavailable_instead_of_false_empty_results() {
    let f = Fixture::new(Some(GITLAB_ORIGIN)).await;
    for status in ["approved", "not_approved"] {
        let mut query = serde_json::to_value(f.query()).unwrap();
        query["reviewStatus"] = json!(status);
        let error = f
            .service
            .list(
                &f.cwd,
                serde_json::from_value(query).unwrap(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, "unavailable");
        assert_eq!(error.operation, "pullRequests.list");
        assert!(error.message.contains("Clear the review status filter"));
    }
    assert!(!f.calls("glab").contains("|mr list"));
}

#[tokio::test]
async fn pull_requests_search_pins_only_pr_commands_and_omits_comment_bodies() {
    let f = Fixture::new(Some(GITHUB_ORIGIN)).await;
    let mut query = f.query();
    query.search = Some("fix".into());
    let page = f
        .service
        .list(&f.cwd, query, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(page.rows[0].comment_count, 0);
    assert!(page.next_cursor.is_none());
    assert!(page.total_count.is_none());
    let calls = f.calls("gh");
    assert!(
        calls.lines().any(|line| line.contains("|pr list")
            && line.contains("--repo github.com/example/repository"))
    );
    assert!(
        calls
            .lines()
            .filter(|line| line.contains("|api "))
            .all(|line| !line.contains("--repo"))
    );
    assert!(!calls.contains(",comments"));
}

#[tokio::test]
async fn pull_requests_vocabularies_use_host_ids_and_remain_lazy() {
    for origin in [GITHUB_ORIGIN, GITLAB_ORIGIN] {
        let f = Fixture::new(Some(origin)).await;
        f.context().await;
        assert!(!f.calls("gh").contains("/labels") && !f.calls("glab").contains("/labels"));
        for (kind, id) in [
            (VocabularyKind::Labels, "bug"),
            (VocabularyKind::Milestones, "3"),
            (
                VocabularyKind::Users,
                if origin == GITHUB_ORIGIN {
                    "alice"
                } else {
                    "7"
                },
            ),
            (VocabularyKind::Branches, "main"),
        ] {
            let vocabulary = f
                .service
                .vocabulary(&f.cwd, kind, None, &CancellationToken::new())
                .await
                .unwrap();
            assert_eq!(vocabulary.entries.len(), 1);
            assert_eq!(vocabulary.entries[0].id, id);
            assert!(!vocabulary.truncated);
        }
    }
}

#[tokio::test]
async fn pull_requests_invalid_host_json_and_rate_limit_use_safe_typed_failures() {
    let f = Fixture::new(Some(GITHUB_ORIGIN)).await;
    write_json(f.root.path(), "github-graphql.json", &json!({}));
    assert_eq!(
        f.service
            .list(&f.cwd, f.query(), &CancellationToken::new())
            .await
            .unwrap_err()
            .code,
        "invalid_response"
    );
    f.mark("rate-limited");
    let error = f
        .service
        .list(&f.cwd, f.query(), &CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code, "rate_limited");
    assert!(error.retryable);
    assert!(error.host_detail.is_none());
    assert!(!error.message.contains("secret"));
}

#[tokio::test]
async fn pull_requests_gitlab_named_host_uses_detection_without_discovery() {
    let f = Fixture::new(Some("ssh://git@gitlab.acme.example/team/sub/repo.git")).await;
    fs::write(
        f.root.path().join("glab-auth"),
        "gitlab.acme.example\n  ✓ Logged in to gitlab.acme.example as alice\n",
    )
    .unwrap();
    let context = f.context().await;
    assert_eq!(context["provider"], "gitlab");
    assert_eq!(context["status"], "available");
    assert!(f.calls("gh").is_empty());
}

fn configured(f: &Fixture) -> ConfiguredPullRequestsRpcServices {
    ConfiguredPullRequestsRpcServices {
        service: f.service.clone(),
        repositories: None,
        worktrees: None,
    }
}

fn rpc_request(method: &str, payload: Value) -> RpcRequest {
    RpcRequest {
        id: RequestId::try_from("1").unwrap(),
        tag: method.into(),
        payload,
        headers: vec![],
        trace_id: None,
        span_id: None,
        sampled: None,
    }
}

fn assert_failure_fixture_shape(error: &Value) {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Failure {
        #[serde(rename = "_tag")]
        tag: String,
        operation: String,
        code: String,
        message: String,
        host_detail: Option<String>,
        retryable: bool,
    }
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../packages/contracts/fixtures/rpc-wire/typed-failures/pullRequests__list-00.json"
    ))
    .unwrap();
    let expected = &fixture["exit"]["cause"][0]["error"];
    assert_eq!(
        error.as_object().unwrap().keys().collect::<Vec<_>>(),
        expected.as_object().unwrap().keys().collect::<Vec<_>>()
    );
    let decoded: Failure = serde_json::from_value(error.clone()).unwrap();
    assert_eq!(decoded.tag, "PullRequestsOperationError");
    assert_eq!(decoded.operation, "pullRequests.list");
    assert_eq!(decoded.code, "rate_limited");
    assert!(!decoded.message.is_empty());
    assert!(decoded.host_detail.is_none());
    assert!(decoded.retryable);
}

#[tokio::test]
async fn pull_requests_rpc_handlers_decode_reads_and_preserve_typed_failure_fixture_shape() {
    let f = Fixture::new(Some(GITHUB_ORIGIN)).await;
    let services = configured(&f);
    let context = services
        .read_unary(
            rpc_request("pullRequests.getContext", json!({"cwd":f.cwd})),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(context["status"], "available");
    let page = services
        .read_unary(
            rpc_request(
                "pullRequests.list",
                serde_json::to_value(f.query()).unwrap(),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(page["rows"].as_array().unwrap().len(), 2);
    f.mark("rate-limited");
    let error = services
        .read_unary(
            rpc_request(
                "pullRequests.list",
                serde_json::to_value(f.query()).unwrap(),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_failure_fixture_shape(&error);
}

#[tokio::test]
async fn pull_requests_rpc_registry_round_trips_context_list_and_typed_failure() {
    let f = Fixture::new(Some(GITHUB_ORIGIN)).await;
    let mut registry = RpcRegistry::empty();
    register_pull_requests_rpc(&mut registry, configured(&f));
    let config = ServerConfig::new(f.root.path().join("server"))
        .with_bind("127.0.0.1", 0)
        .with_unsafe_no_auth();
    let server = ServerRuntime::start_with_registry(config, registry)
        .await
        .expect("RPC fixture listener starts");
    let (mut socket, _) = connect_async(format!("ws://{}/ws", server.local_addr()))
        .await
        .unwrap();
    for (id, method, payload) in [
        ("1", "pullRequests.getContext", json!({"cwd":f.cwd})),
        (
            "2",
            "pullRequests.list",
            serde_json::to_value(f.query()).unwrap(),
        ),
        (
            "3",
            "pullRequests.list",
            serde_json::to_value(f.query()).unwrap(),
        ),
    ] {
        if id == "3" {
            f.mark("rate-limited");
        }
        socket
            .send(Message::Text(
                json!({"_tag":"Request","id":id,"tag":method,"payload":payload,"headers":[]})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        let frame = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            next_frame_past_heartbeat(&mut socket),
        )
        .await
        .unwrap()
        .unwrap()
        .unwrap();
        let Message::Text(text) = frame else {
            panic!("expected RPC text frame")
        };
        let response: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(response["_tag"], "Exit");
        assert_eq!(response["requestId"], id);
        match id {
            "1" => assert_eq!(response["exit"]["value"]["status"], "available"),
            "2" => assert_eq!(
                response["exit"]["value"]["rows"].as_array().unwrap().len(),
                2
            ),
            _ => assert_failure_fixture_shape(&response["exit"]["cause"][0]["error"]),
        }
    }
    socket.close(None).await.unwrap();
    server.shutdown();
    server.join().await.unwrap();
}
