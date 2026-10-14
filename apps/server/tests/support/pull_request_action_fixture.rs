//! A recording provider CLI for Pull Requests action tests. Each call answers with
//! the next prepared response; concurrent context reads and auth probes take
//! their argv role's slot, regardless of process arrival order.
//! Mutations are exercised only against this stub, never live hosts.

#[path = "executable_fixture.rs"]
pub(crate) mod executable_fixture;

use bibcode_server::pull_requests::{
    github::GitHubHost,
    gitlab::GitLabHost,
    host::{HostCommandRunner, HostScope, PullRequestHost},
    model::{ActionRequest, ActionResult, PullRequestsProvider},
};
use serde_json::{Value, json};
use std::{fs, path::Path, sync::Arc};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

pub struct Fixture {
    pub root: TempDir,
    pub scope: HostScope,
    pub host: Box<dyn PullRequestHost>,
    pub runner: HostCommandRunner,
    pub responses: usize,
}

impl Fixture {
    pub fn new(gitlab: bool) -> Self {
        let root = TempDir::new().unwrap();
        let script = root.path().join("cli");
        executable_fixture::write_executable(
            &script,
            r#"#!/bin/sh
until mkdir count.lock 2>/dev/null; do sleep 0.01; done
n=0
if [ -f count ]; then n=$(cat count); fi
n=$((n+1))
printf '%s' "$n" > count
slot=$n
# Context reads overlap both login probes. Assign every concurrent command by
# argv, and claim its response while holding the call-number lock.
case "$1 $2" in
  '--version ') role=cli-version ;;
  'auth status') role=auth ;;
  'api user') role=user ;;
  'api version') role=version ;;
  'api projects/team%2Frepo') role=project ;;
  'api projects/team%2Frepo/merge_requests/'*'/approvals') role=approvals ;;
  'api projects/team%2Frepo/merge_requests/'*'/reviewers') role=reviewers ;;
  'api projects/team%2Frepo/merge_requests/'*'/approval_state') role=approval-state ;;
  'api projects/team%2Frepo/merge_requests/'*'/award_emoji'*) role=awards ;;
  'api projects/team%2Frepo/merge_requests/'*'/closes_issues'*) role=issues ;;
  'api projects/team%2Frepo/merge_requests/'*'/resource_label_events'*) role=label-events ;;
  'api projects/team%2Frepo/merge_requests/'*'/resource_milestone_events'*) role=milestone-events ;;
  'api projects/team%2Frepo/merge_requests/'*'/resource_state_events'*) role=state-events ;;
  'api projects/team%2Frepo/merge_requests/'*/*) role= ;;
  'api projects/team%2Frepo/merge_requests/'*) role=merge-request ;;
  'api repos/team/repo') role=repository ;;
  'api graphql') role=graphql ;;
  'api --method') if [ "$4" = graphql ]; then role=graphql; else role=; fi ;;
  *) role= ;;
esac
if [ -n "$role" ] && [ -f "role-$role" ]; then
  slot=$(head -n 1 "role-$role")
  tail -n +2 "role-$role" > "role-$role.rest"
  if [ -s "role-$role.rest" ]; then mv "role-$role.rest" "role-$role"; else rm -f "role-$role" "role-$role.rest"; fi
fi
rmdir count.lock
printf '%s\000' "$@" > "call-$n.argv"
printf '%s|%s|%s' "$GH_HOST" "$GITLAB_HOST" "$NO_COLOR" > "call-$n.env"
cat > "call-$n.stdin"
while [ "$#" -gt 0 ]; do
  if [ "$1" = --input ] && [ "$2" != - ]; then
    printf '%s' "$2" > "call-$n.path"
    (stat -c '%a' "$2" 2>/dev/null || stat -f '%Lp' "$2") > "call-$n.mode"
    cp "$2" "call-$n.body"
  fi
  shift
done
if [ ! -f "response-$slot" ]; then echo 'Unexpected process call' >&2; exit 64; fi
cat "response-$slot"
if [ -f "error-$slot" ]; then cat "error-$slot" >&2; exit 1; fi
"#,
        );
        let runner = Arc::new(
            HostCommandRunner::new(root.path().join("state"))
                .with_commands(&script, &script, &script),
        );
        let host: Box<dyn PullRequestHost> = if gitlab {
            Box::new(GitLabHost::new(runner.clone()))
        } else {
            Box::new(GitHubHost::new(runner.clone()))
        };
        let scope = HostScope {
            cwd: root.path().to_owned(),
            host: if gitlab {
                "gitlab.example.test"
            } else {
                "github.example.test"
            }
            .into(),
            repository: "team/repo".into(),
            provider: if gitlab {
                PullRequestsProvider::Gitlab
            } else {
                PullRequestsProvider::Github
            },
        };
        Self {
            root,
            scope,
            host,
            runner: (*runner).clone(),
            responses: 0,
        }
    }
    pub fn response(&mut self, value: Value) {
        self.raw_response(&value.to_string());
    }
    pub fn raw_response(&mut self, value: &str) {
        self.responses += 1;
        fs::write(
            self.root
                .path()
                .join(format!("response-{}", self.responses)),
            value,
        )
        .unwrap();
    }
    pub fn failure(&mut self, message: &str) {
        self.raw_response("");
        fs::write(
            self.root.path().join(format!("error-{}", self.responses)),
            message,
        )
        .unwrap();
    }
    /// Marks the last response as a concurrent command: `cli-version`/`auth`
    /// probes, GitLab `user`/`project`/`version`, GitHub
    /// `user`/`repository`/`graphql`, or GitLab merge request reads
    /// (`merge-request`, `approvals`, `reviewers`, `approval-state`, `awards`,
    /// `issues`, `graphql`, `*-events`). Every call in an overlapping group must
    /// reserve its own role; only sequential calls may use arrival-order slots.
    /// A role reserved again queues behind its earlier reservation, so a later
    /// group's read of the same argv takes the later response.
    pub fn role(&self, role: &str) {
        use std::io::Write;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root.path().join(format!("role-{role}")))
            .unwrap();
        writeln!(file, "{}", self.responses).unwrap();
    }
    /// The five permission reads an action makes together: merge request,
    /// approvals, reviewers, GraphQL metadata and project.
    pub fn gitlab_action_reads(&mut self, reads: [&Value; 5]) {
        for (value, role) in reads.into_iter().zip([
            "merge-request",
            "approvals",
            "reviewers",
            "graphql",
            "project",
        ]) {
            self.response(value.clone());
            self.role(role);
        }
    }
    pub fn request(&self, mut value: Value) -> ActionRequest {
        value["cwd"] = json!(self.scope.cwd);
        value["number"] = json!(14);
        serde_json::from_value(value).unwrap()
    }
    pub async fn run(
        &self,
        value: Value,
    ) -> Result<ActionResult, bibcode_server::pull_requests::error::PullRequestsOperationError>
    {
        self.host
            .run_action(
                &self.scope,
                &self.request(value),
                &Default::default(),
                &CancellationToken::new(),
            )
            .await
    }
    pub fn count(&self) -> usize {
        fs::read_to_string(self.root.path().join("count"))
            .unwrap_or_default()
            .parse()
            .unwrap_or(0)
    }
    pub fn args(&self, call: usize) -> Vec<String> {
        fs::read_to_string(self.root.path().join(format!("call-{call}.argv")))
            .unwrap()
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    }
    pub fn input(&self, call: usize) -> String {
        let path = self.root.path().join(format!("call-{call}.body"));
        fs::read_to_string(if path.exists() {
            path
        } else {
            self.root.path().join(format!("call-{call}.stdin"))
        })
        .unwrap()
    }
    pub fn json_input(&self, call: usize) -> Value {
        serde_json::from_str(&self.input(call)).unwrap()
    }
    pub fn assert_args(&self, call: usize, expected: &[&str]) {
        assert_eq!(self.args(call), expected);
    }
    pub fn assert_body_private(&self, call: usize, body: &str) {
        let args = self.args(call);
        assert!(!args.iter().any(|a| a.contains(body) || a == "--body"));
        if self.scope.provider == PullRequestsProvider::Gitlab {
            let path =
                fs::read_to_string(self.root.path().join(format!("call-{call}.path"))).unwrap();
            assert!(Path::new(&path).starts_with(self.root.path().join("state/pull-requests")));
            assert!(!Path::new(&path).exists());
            assert_eq!(
                fs::read_to_string(self.root.path().join(format!("call-{call}.mode")))
                    .unwrap()
                    .trim(),
                "600"
            );
        }
    }
}

impl Fixture {
    pub async fn rpc(&self, mut payload: Value) -> Result<Value, Value> {
        use bibcode_server::{
            RequestId, RpcRequest,
            production::pull_requests_rpc::ConfiguredPullRequestsRpcServices,
            pull_requests::PullRequestsService,
        };
        if payload.get("cwd").is_none() {
            payload["cwd"] = json!(self.scope.cwd);
        }
        if payload.get("number").is_none() {
            payload["number"] = json!(14);
        }
        let rpc = ConfiguredPullRequestsRpcServices {
            service: PullRequestsService::with_runner(self.runner.clone()),
            repositories: None,
            worktrees: None,
        };
        rpc.mutation_unary(
            RpcRequest {
                id: RequestId::try_from("1").unwrap(),
                tag: "pullRequests.runAction".into(),
                payload,
                headers: vec![],
                trace_id: None,
                span_id: None,
                sampled: None,
            },
            CancellationToken::new(),
        )
        .await
    }
    pub fn github_precheck(&mut self, own: bool) {
        self.raw_response("https://github.com/team/repo.git");
        self.raw_response("gh version 2.97.0");
        self.role("cli-version");
        self.response(json!({"hosts":{"github.com":[{"state":"success","active":true,"host":"github.com","login":"viewer"}]}}));
        self.role("auth");
        self.response(json!({"login":"viewer"}));
        self.role("user");
        self.response(
            serde_json::from_str(include_str!(
                "../fixtures/pull_requests/github_repository.json"
            ))
            .unwrap(),
        );
        self.role("repository");
        self.response(json!({"data":{"repository":{"viewerPermission":"ADMIN"}}}));
        self.role("graphql");
        let mut detail: Value =
            serde_json::from_str(include_str!("../fixtures/pull_requests/github_detail.json"))
                .unwrap();
        detail["number"] = json!(14);
        if own {
            detail["author"]["login"] = json!("viewer");
        }
        self.response(detail);
        let mut metadata: Value = serde_json::from_str(include_str!(
            "../fixtures/pull_requests/github_detail_viewer.json"
        ))
        .unwrap();
        metadata["data"]["repository"]["pullRequest"]["viewerDidAuthor"] = json!(own);
        self.response(metadata);
    }
    pub fn gitlab_precheck(&mut self, can_approve: bool) {
        self.raw_response("https://gitlab.com/team/repo.git");
        self.raw_response("glab version 1.114.0");
        self.role("cli-version");
        self.raw_response("gitlab.com\n  ✓ Logged in to gitlab.com as viewer");
        self.role("auth");
        self.response(json!({"username":"viewer","name":null}));
        self.role("user");
        self.response(
            serde_json::from_str(include_str!(
                "../fixtures/pull_requests/gitlab_project.json"
            ))
            .unwrap(),
        );
        self.role("project");
        self.response(json!({"version":"17.9.0"}));
        self.role("version");
        let mut detail: Value =
            serde_json::from_str(include_str!("../fixtures/pull_requests/gitlab_detail.json"))
                .unwrap();
        detail["iid"] = json!(14);
        let mut approvals: Value = serde_json::from_str(include_str!(
            "../fixtures/pull_requests/gitlab_approvals.json"
        ))
        .unwrap();
        approvals["user_can_approve"] = json!(can_approve);
        let reviewers: Value = serde_json::from_str(include_str!(
            "../fixtures/pull_requests/gitlab_reviewers.json"
        ))
        .unwrap();
        let metadata = json!({"data":{"project":{"mergeRequest":{"commitCount":2,"resolvableDiscussionsCount":1,"diffStatsSummary":{"fileCount":2,"additions":2,"deletions":1},"sourceProject":{"fullPath":"fork/repo"},"userPermissions":{"createNote":true,"pushToSourceBranch":true}}}}});
        let project: Value = serde_json::from_str(include_str!(
            "../fixtures/pull_requests/gitlab_project.json"
        ))
        .unwrap();
        self.gitlab_action_reads([&detail, &approvals, &reviewers, &metadata, &project]);
    }
}
