#![cfg(unix)]
//! Recorded response shapes with synthetic content; no authenticated host/network traffic.
#[path = "support/executable_fixture.rs"]
mod executable_fixture;

use coddy_studio_server::{
    RequestId, RpcRequest,
    production::pull_requests_rpc::ConfiguredPullRequestsRpcServices,
    pull_requests::{ContextRead, PullRequestsService, host::HostCommandRunner},
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

struct Fixture {
    root: TempDir,
    cwd: PathBuf,
    runner: HostCommandRunner,
    rpc: ConfiguredPullRequestsRpcServices,
    number: u64,
}
impl Fixture {
    async fn new(gitlab: bool) -> Self {
        let root = TempDir::new().unwrap();
        let cwd = root.path().join("checkout");
        fs::create_dir(&cwd).unwrap();
        for (name, contents) in [
            (
                "github-detail.json",
                include_str!("fixtures/pull_requests/github_detail.json"),
            ),
            (
                "github-viewer.json",
                include_str!("fixtures/pull_requests/github_detail_viewer.json"),
            ),
            (
                "github-timeline.json",
                include_str!("fixtures/pull_requests/github_timeline.json"),
            ),
            (
                "github-repository.json",
                include_str!("fixtures/pull_requests/github_repository.json"),
            ),
            (
                "gitlab-detail.json",
                include_str!("fixtures/pull_requests/gitlab_detail.json"),
            ),
            (
                "gitlab-project.json",
                include_str!("fixtures/pull_requests/gitlab_project.json"),
            ),
            (
                "gitlab-approvals.json",
                include_str!("fixtures/pull_requests/gitlab_approvals.json"),
            ),
            (
                "gitlab-reviewers.json",
                include_str!("fixtures/pull_requests/gitlab_reviewers.json"),
            ),
            (
                "gitlab-timeline.json",
                include_str!("fixtures/pull_requests/gitlab_timeline_graphql.json"),
            ),
            (
                "gitlab-invalid-field.json",
                include_str!("fixtures/pull_requests/gitlab_detail_invalid_field.json"),
            ),
        ] {
            fs::write(root.path().join(name), contents).unwrap();
        }
        write(
            root.path(),
            "metadata.json",
            json!({"data":{"project":{"mergeRequest":{"commitCount":2,"resolvableDiscussionsCount":1,"diffStatsSummary":{"additions":2,"deletions":1,"fileCount":2},"sourceProject":{"fullPath":"contributor/cli"},"userPermissions":{"createNote":true,"pushToSourceBranch":true},"diffStats":[{"path":"text.rs","additions":1,"deletions":1},{"path":"image.png","additions":0,"deletions":0}]}}}}),
        );
        write(
            root.path(),
            "github-commits.json",
            json!({"commits":[{"oid":"abcdef123456","messageHeadline":"Subject","messageBody":"Body","authors":[{"login":"viewer","name":"Viewer"}],"authoredDate":"2026-09-20T00:00:00Z"}]}),
        );
        write(
            root.path(),
            "github-checks.json",
            json!({"statusCheckRollup":[{"name":"test","workflowName":"CI","status":"COMPLETED","conclusion":"FAILURE","startedAt":"2026-09-20T00:00:00Z","completedAt":"2026-09-20T00:01:00Z"}]}),
        );
        write(
            root.path(),
            "github-files.json",
            json!({"files":[{"path":"text.rs","additions":1,"deletions":1,"changeType":"MODIFIED"},{"path":"image.png","additions":0,"deletions":0,"changeType":"ADDED"}],"baseRefOid":"base-sha","headRefOid":"head-sha"}),
        );
        fs::write(root.path().join("patch"),"diff --git a/text.rs b/text.rs\n--- a/text.rs\n+++ b/text.rs\n@@ -1 +1 @@\n-old\n+new\ndiff --git a/image.png b/image.png\nGIT binary patch\nliteral 0\n").unwrap();
        write(
            root.path(),
            "gitlab-commits.json",
            json!([{"id":"abcdef123456","short_id":"abcdef12","title":"Subject","message":"Subject\n\nBody","author_name":"Author Name","author_email":"private@example.test","authored_date":"2026-09-20T00:00:00Z","web_url":"https://gitlab.com/commit/abcdef123456"}]),
        );
        write(
            root.path(),
            "gitlab-pipelines.json",
            json!([{"id":9,"project_id":2,"status":"success","web_url":"https://gitlab.com/pipeline/9"}]),
        );
        write(
            root.path(),
            "gitlab-jobs.json",
            json!([{"name":"compile","stage":"build","status":"success","duration":3.5},{"name":"test","stage":"test","status":"success","duration":4.0}]),
        );
        write(
            root.path(),
            "gitlab-diffs.json",
            json!([{"old_path":"text.rs","new_path":"text.rs","diff":"@@ -1 +1 @@\n-old\n+new\n"},{"old_path":"image.png","new_path":"image.png","binary":true,"new_file":true,"diff":""}]),
        );
        fs::write(root.path().join("version"), "17.9.1").unwrap();
        let gh = script(
            root.path(),
            "gh",
            r#"
printf '%s|%s\n' "$GH_HOST" "$*" >> "$FIXTURE_DIR/gh-calls"
case "$1 $2" in
  '--version ') echo 'gh version 2.97.0' ;;
  'auth status') echo '{"hosts":{"github.com":[{"state":"success","active":true,"host":"github.com","login":"viewer"}]}}' ;;
  'api user') echo '{"login":"viewer","name":null}' ;;
  'api repos/'*) cat "$FIXTURE_DIR/github-repository.json" ;;
  'api graphql') cat > "$FIXTURE_DIR/query.json"; if grep -q PullRequestsFiles "$FIXTURE_DIR/query.json"; then if grep -q files-next "$FIXTURE_DIR/query.json"; then cat "$FIXTURE_DIR/github-files-next.json"; else cat "$FIXTURE_DIR/github-files-first.json";fi; elif grep -q PullRequestsTimeline "$FIXTURE_DIR/query.json"; then cat "$FIXTURE_DIR/github-timeline.json"; else cat "$FIXTURE_DIR/github-viewer.json";fi ;;
  'pr view') case "$5" in
    commits) cat "$FIXTURE_DIR/github-commits.json" ;;
    statusCheckRollup) cat "$FIXTURE_DIR/github-checks.json" ;;
    files,baseRefOid,headRefOid) cat "$FIXTURE_DIR/github-files.json" ;;
    *) cat "$FIXTURE_DIR/github-detail.json" ;; esac ;;
  'pr diff') cat "$FIXTURE_DIR/patch" ;;
  *) exit 64 ;;
esac
"#,
        );
        let glab = script(
            root.path(),
            "glab",
            r##"
printf '%s|%s\n' "$GITLAB_HOST" "$*" >> "$FIXTURE_DIR/glab-calls"
case "$1 $2" in
  '--version ') echo 'glab version 1.114.0' ;;
  'auth status') printf 'gitlab.com\n  ✓ Logged in to gitlab.com as viewer\n' ;;
  'api user') echo '{"id":8,"username":"viewer","name":null}' ;;
  'api version') printf '{"version":"%s"}\n' "$(cat "$FIXTURE_DIR/version")" ;;
  'api projects/gitlab-org%2Fcli') cat "$FIXTURE_DIR/gitlab-project.json" ;;
  'api projects/gitlab-org%2Fcli/merge_requests/3941') cat "$FIXTURE_DIR/gitlab-detail.json" ;;
  'api projects/gitlab-org%2Fcli/merge_requests/3941/approvals') cat "$FIXTURE_DIR/gitlab-approvals.json" ;;
  'api projects/gitlab-org%2Fcli/merge_requests/3941/reviewers') cat "$FIXTURE_DIR/gitlab-reviewers.json" ;;
  'api projects/gitlab-org%2Fcli/merge_requests/3941/approval_state') if [ -e "$FIXTURE_DIR/approval-state-error" ]; then cat "$FIXTURE_DIR/approval-state-error" >&2;exit 1;fi;echo '{"rules":[]}' ;;
  'api projects/gitlab-org%2Fcli/merge_requests/3941/award_emoji') if [ -e "$FIXTURE_DIR/body-awards-legacy.json" ]; then cat "$FIXTURE_DIR/body-awards-legacy.json"; else echo '[{"name":"thumbsup","user":{"username":"viewer"}}]';fi ;;
  'api projects/gitlab-org%2Fcli/merge_requests/3941/award_emoji?per_page=100&page=1') if [ -e "$FIXTURE_DIR/body-awards-first.json" ]; then cat "$FIXTURE_DIR/body-awards-first.json"; else echo '[{"name":"thumbsup","user":{"username":"viewer"}}]';fi ;;
  'api projects/gitlab-org%2Fcli/merge_requests/3941/award_emoji?per_page=100&page=2') cat "$FIXTURE_DIR/body-awards-next.json" ;;
  'api projects/gitlab-org%2Fcli/merge_requests/3941/closes_issues?per_page=100') echo '[{"iid":12,"references":{"relative":"#12"},"title":"Issue","web_url":"https://gitlab.com/issue/12"}]' ;;
  'api projects/gitlab-org%2Fcli/merge_requests/3941/versions') echo '[{"base_commit_sha":"base-sha","start_commit_sha":"start-sha","head_commit_sha":"head-sha"}]' ;;
  'api projects/gitlab-org%2Fcli/merge_requests/3941/notes/10') echo '{"suggestions":[{"id":7,"appliable":true,"applied":false,"from_line":2,"to_line":2,"from_content":"old","to_content":"new"}]}' ;;
  'api projects/gitlab-org%2Fcli/merge_requests/3941/resource_'*) echo '[]' ;;
  'api projects/gitlab-org%2Fcli/merge_requests/3941/commits?per_page=100') cat "$FIXTURE_DIR/gitlab-commits.json" ;;
  'api projects/gitlab-org%2Fcli/merge_requests/3941/pipelines?per_page=1') cat "$FIXTURE_DIR/gitlab-pipelines.json" ;;
  'api projects/2/pipelines/9/jobs?per_page=100') cat "$FIXTURE_DIR/gitlab-jobs.json" ;;
  'api projects/gitlab-org%2Fcli/merge_requests/3941/diffs?per_page=50&page=1') cat "$FIXTURE_DIR/gitlab-diffs.json" ;;
  'api --method') test "$3 $4 $5" = 'POST graphql -H' || exit 65
    test "$6" = 'Content-Type: application/json' && test "$7" = --input || exit 65
    if grep -q 'resolveNote' "$8"; then cat "$FIXTURE_DIR/gitlab-invalid-field.json"
    elif grep -q 'PullRequestsGitLabTimeline' "$8"; then
      if grep -q 'after-100' "$8" && [ -e "$FIXTURE_DIR/gitlab-timeline-next.json" ]; then cat "$FIXTURE_DIR/gitlab-timeline-next.json"; else cat "$FIXTURE_DIR/gitlab-timeline.json"; fi
    else cat "$FIXTURE_DIR/metadata.json"; fi ;;
  *) exit 64 ;;
esac
"##,
        );
        let runner =
            HostCommandRunner::new(root.path().join("state")).with_commands(gh, glab, "git");
        let c = CancellationToken::new();
        runner
            .git(&cwd, &["init", "-q", "-b", "main"], &c)
            .await
            .unwrap();
        runner
            .git(
                &cwd,
                &[
                    "remote",
                    "add",
                    "origin",
                    if gitlab {
                        "https://gitlab.com/gitlab-org/cli.git"
                    } else {
                        "https://github.com/openai/codex.git"
                    },
                ],
                &c,
            )
            .await
            .unwrap();
        let rpc = ConfiguredPullRequestsRpcServices {
            service: PullRequestsService::with_runner(runner.clone()),
            repositories: None,
            worktrees: None,
        };
        Self {
            root,
            cwd,
            runner,
            rpc,
            number: if gitlab { 3941 } else { 35882 },
        }
    }
    fn put(&self, name: &str, value: Value) {
        write(self.root.path(), name, value);
    }
    fn json(&self, name: &str) -> Value {
        serde_json::from_str(&fs::read_to_string(self.root.path().join(name)).unwrap()).unwrap()
    }
    fn calls(&self, provider: &str) -> String {
        fs::read_to_string(self.root.path().join(format!("{provider}-calls"))).unwrap_or_default()
    }
    async fn read(&self, method: &str) -> Result<Value, Value> {
        self.rpc
            .read_unary(
                RpcRequest {
                    id: RequestId::try_from("1").unwrap(),
                    tag: format!("pullRequests.{method}"),
                    payload: json!({"cwd":self.cwd,"number":self.number}),
                    headers: vec![],
                    trace_id: None,
                    span_id: None,
                    sampled: None,
                },
                CancellationToken::new(),
            )
            .await
    }
}
fn write(root: &Path, name: &str, v: Value) {
    fs::write(root.join(name), v.to_string()).unwrap();
}
fn script(root: &Path, name: &str, body: &str) -> PathBuf {
    let path = root.join(name);
    let quoted = root.to_string_lossy().replace('\'', "'\\''");
    executable_fixture::write_executable(
        &path,
        format!("#!/bin/sh\nexport FIXTURE_DIR='{quoted}'\n{body}"),
    );
    path
}
fn assert_permission_reasons(detail: &Value) {
    for p in detail["permissions"].as_object().unwrap().values() {
        assert_eq!(p["reason"].is_null(), p["allowed"] == true, "{p}");
    }
}

#[tokio::test]
async fn pull_requests_github_read_viewer_detail_matches_wire_permissions_and_readiness() {
    let f = Fixture::new(false).await;
    let d = f.read("get").await.unwrap();
    assert_eq!(d["number"], 35882);
    assert_eq!(d["permissions"]["merge"]["allowed"], false);
    assert_eq!(
        d["permissions"]["merge"]["reason"],
        "Write access is required for this action."
    );
    assert_eq!(d["permissions"]["approve"]["allowed"], true);
    assert!(
        d["permissions"]["applySuggestion"]["reason"]
            .as_str()
            .unwrap()
            .contains("no public API")
    );
    assert_eq!(d["readiness"]["status"], "review_required");
    assert!(
        d["readiness"]["details"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d.as_str().unwrap().contains("1 approving review"))
    );
    assert_permission_reasons(&d);
    assert_eq!(d["tabCounts"]["conversation"], 3);
    assert_eq!(d["tabCounts"]["checks"], 3);
    assert_eq!(
        f.calls("gh")
            .lines()
            .filter(|l| *l == "github.com|api user")
            .count(),
        1
    );
    let keys = d
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let mut expected = vec![
        "number",
        "title",
        "body",
        "state",
        "isDraft",
        "locked",
        "lockReason",
        "author",
        "createdAt",
        "updatedAt",
        "mergedAt",
        "closedAt",
        "mergedBy",
        "headBranch",
        "baseBranch",
        "headSha",
        "baseSha",
        "isCrossRepository",
        "headRepository",
        "maintainerCanModify",
        "commitCount",
        "changedFiles",
        "additions",
        "deletions",
        "labels",
        "milestone",
        "assignees",
        "reviewers",
        "approvalRules",
        "linkedIssues",
        "reactions",
        "readiness",
        "permissions",
        "url",
        "tabCounts",
    ];
    expected.sort_unstable();
    assert_eq!(keys, expected);
    assert!(!d.to_string().contains("avatar"));
}

#[tokio::test]
async fn pull_requests_github_merged_coddy_studio_14_can_revert_but_not_merge() {
    let mut f = Fixture::new(false).await;
    f.number = 14;
    f.runner
        .git(
            &f.cwd,
            &[
                "remote",
                "set-url",
                "origin",
                "https://github.com/mubeda/CoddyStudio.git",
            ],
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let mut d = f.json("github-detail.json");
    d["number"] = json!(14);
    d["state"] = json!("MERGED");
    d["mergeStateStatus"] = json!("UNKNOWN");
    d["mergeable"] = json!("UNKNOWN");
    d["url"] = json!("https://github.com/mubeda/CoddyStudio/pull/14");
    f.put("github-detail.json", d);
    let mut v = f.json("github-viewer.json");
    v["data"]["repository"]["viewerPermission"] = json!("ADMIN");
    v["data"]["repository"]["pullRequest"]["mergeCommit"] = json!({"oid":"merge-sha"});
    f.put("github-viewer.json", v);
    let d = f.read("get").await.unwrap();
    assert_eq!(d["readiness"]["status"], "merged");
    assert_eq!(d["permissions"]["revert"]["allowed"], true);
    assert_eq!(d["permissions"]["merge"]["allowed"], false);
    assert!(
        f.calls("gh")
            .contains("--repo github.com/mubeda/CoddyStudio")
    );
}

#[tokio::test]
async fn pull_requests_gitlab_3941_host_answers_reviewers_pipeline_and_version_gates() {
    let f = Fixture::new(true).await;
    let d = f.read("get").await.unwrap();
    assert_eq!(d["permissions"]["approve"]["allowed"], false);
    assert!(d["permissions"]["approve"]["reason"].is_string());
    assert_eq!(d["readiness"]["status"], "review_required");
    assert_eq!(d["reviewers"][0]["state"], "unreviewed");
    assert_eq!(d["reviewers"][1]["state"], "commented");
    assert_eq!(d["permissions"]["requestChanges"]["allowed"], true);
    assert_permission_reasons(&d);
    let checks = f.read("getChecks").await.unwrap();
    assert_eq!(checks["summary"], "success");
    assert_eq!(checks["groups"][0]["name"], "build");
    assert_eq!(checks["groups"][1]["name"], "test");
    assert!(checks["groups"][0]["checks"][0]["startedAt"].is_null());
    assert!(checks["groups"][0]["checks"][0]["completedAt"].is_null());
    f.put("gitlab-reviewers.json", json!([]));
    assert_eq!(
        f.read("get").await.unwrap()["permissions"]["requestChanges"]["allowed"],
        false
    );
    fs::write(f.root.path().join("version"), "16.11.0").unwrap();
    f.rpc
        .service
        .context(&f.cwd, ContextRead::Rescan, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        f.read("get").await.unwrap()["permissions"]["requestChanges"]["reason"],
        "This GitLab instance is 16.11; requesting changes needs 17.2."
    );
    assert!(
        f.calls("glab")
            .lines()
            .filter(|l| l.contains("|api "))
            .all(|l| l.ends_with("--hostname gitlab.com"))
    );
}

#[tokio::test]
async fn pull_requests_detail_timelines_keep_host_positions_and_opaque_ids() {
    let gh = Fixture::new(false).await;
    let timeline = gh.read("getTimeline").await.unwrap();
    let t = timeline["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "thread")
        .unwrap();
    assert_eq!(t["path"], "codex-rs/rust-toolchain.toml");
    assert_eq!(t["line"], 2);
    assert_eq!(t["side"], "right");
    assert_eq!(t["isResolved"], false);
    assert_eq!(t["canResolve"], false);
    assert_eq!(t["id"], "th:TH_3:3");
    assert_eq!(t["comments"][0]["id"], "rc:3:RC_3");
    let gl = Fixture::new(true).await;
    let timeline = gl.read("getTimeline").await.unwrap();
    let t = timeline["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["kind"] == "thread")
        .unwrap();
    assert_eq!(t["path"], "src/main.rs");
    assert_eq!(t["id"], "ds:discussion-1");
    assert_eq!(t["comments"][0]["id"], "nt:discussion-1:10");
    assert_eq!(t["comments"][0]["suggestion"]["id"], "7");
}

#[tokio::test]
async fn pull_requests_detail_commits_and_checks_return_contract_shapes_for_both_hosts() {
    for gitlab in [false, true] {
        let f = Fixture::new(gitlab).await;
        let commits = f.read("getCommits").await.unwrap();
        assert_eq!(commits["commits"][0]["subject"], "Subject");
        assert_eq!(commits["commits"][0]["body"], "Body");
        assert!(commits["commits"][0]["url"].is_string());
        let checks = f.read("getChecks").await.unwrap();
        assert_eq!(
            checks["summary"],
            if gitlab { "success" } else { "failure" }
        );
        assert!(
            checks["groups"][0]["checks"][0]
                .get("durationSeconds")
                .is_some()
        );
    }
}

#[tokio::test]
async fn pull_requests_detail_files_keep_binary_rows_and_diff_refs_for_both_hosts() {
    for gitlab in [false, true] {
        let f = Fixture::new(gitlab).await;
        let files = f.read("getFiles").await.unwrap();
        assert_eq!(files["files"].as_array().unwrap().len(), 2);
        assert!(
            files["files"][0]["patch"]
                .as_str()
                .unwrap()
                .contains("+new")
        );
        assert_eq!(files["files"][1]["changeType"], "binary");
        assert!(files["files"][1]["patch"].is_null());
        assert_eq!(files["diffRefs"]["headSha"], "head-sha");
        assert_eq!(
            files["diffRefs"]["startSha"],
            if gitlab { "start-sha" } else { "base-sha" }
        );
    }
}

#[tokio::test]
async fn pull_requests_detail_nine_mib_patches_return_truncated_lists_without_errors() {
    for gitlab in [false, true] {
        let f = Fixture::new(gitlab).await;
        if gitlab {
            f.put(
                "gitlab-diffs.json",
                json!([{"old_path":"text.rs","new_path":"text.rs","diff":"x".repeat(9*1024*1024)}]),
            );
        } else {
            fs::write(f.root.path().join("patch"), "x".repeat(9 * 1024 * 1024)).unwrap();
        }
        let files = f.read("getFiles").await.unwrap();
        assert_eq!(files["truncated"], true);
        assert_eq!(files["files"].as_array().unwrap().len(), 2);
        assert!(
            files["files"]
                .as_array()
                .unwrap()
                .iter()
                .all(|f| f["patch"].is_null() && f["tooLarge"] == true)
        );
    }
}

#[tokio::test]
async fn pull_requests_detail_per_file_cap_retains_rows_for_both_hosts() {
    for gitlab in [false, true] {
        let f = Fixture::new(gitlab).await;
        let hunk = format!("@@ -0,0 +1 @@\n+{}\n", "x".repeat(1024 * 1024));
        if gitlab {
            f.put(
                "gitlab-diffs.json",
                json!([{"old_path":"text.rs","new_path":"text.rs","diff":hunk}]),
            );
        } else {
            fs::write(
                f.root.path().join("patch"),
                format!("diff --git a/text.rs b/text.rs\n--- a/text.rs\n+++ b/text.rs\n{hunk}"),
            )
            .unwrap();
        }
        let files = f.read("getFiles").await.unwrap();
        assert_eq!(files["files"][0]["path"], "text.rs");
        assert_eq!(files["files"][0]["tooLarge"], true);
        assert!(files["files"][0]["patch"].is_null());
        assert_eq!(files["truncated"], false);
    }
}

#[tokio::test]
async fn pull_requests_detail_json_cap_is_one_mib_and_errors_keep_requested_operation() {
    let f = Fixture::new(false).await;
    let mut d = f.json("github-detail.json");
    d["body"] = json!("x".repeat(1024 * 1024));
    f.put("github-detail.json", d);
    let error = f.read("get").await.unwrap_err();
    assert_eq!(error["code"], "output_limit");
    assert_eq!(error["operation"], "pullRequests.get");
    fs::write(f.root.path().join("github-checks.json"), "not-json").unwrap();
    let error = f.read("getChecks").await.unwrap_err();
    assert_eq!(error["code"], "invalid_response");
    assert_eq!(error["operation"], "pullRequests.getChecks");
}

#[tokio::test]
async fn pull_requests_detail_optional_approval_state_ignores_only_forbidden_or_not_found() {
    let f = Fixture::new(true).await;
    for status in ["HTTP 403", "HTTP 404"] {
        fs::write(f.root.path().join("approval-state-error"), status).unwrap();
        let d = f.read("get").await.unwrap();
        assert_eq!(d["approvalRules"][0]["required"], 1);
    }
    fs::write(f.root.path().join("approval-state-error"), "HTTP 500").unwrap();
    let error = f.read("get").await.unwrap_err();
    assert_eq!(error["code"], "host_rejected");
    assert_eq!(error["operation"], "pullRequests.get");
}

#[tokio::test]
async fn pull_requests_detail_cancellation_and_inactive_service_start_no_host_work() {
    let f = Fixture::new(false).await;
    assert!(f.calls("gh").is_empty());
    let c = CancellationToken::new();
    c.cancel();
    let error = f.rpc.service.get(&f.cwd, f.number, &c).await.unwrap_err();
    assert_eq!(error.code, "timeout");
    assert!(f.calls("gh").is_empty());
}

#[tokio::test]
async fn pull_requests_review_fix_gitlab_native_suggestion_appliable_is_normalized() {
    let f = Fixture::new(true).await;
    let timeline = f.read("getTimeline").await.unwrap();
    let thread = timeline["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["kind"] == "thread")
        .unwrap();
    assert_eq!(thread["comments"][0]["suggestion"]["applicable"], true);
}

#[tokio::test]
async fn pull_requests_review_fix_gitlab_scalar_and_array_detail_are_typed_errors() {
    let f = Fixture::new(true).await;
    for value in [json!("shape drift"), json!([]), json!(42), Value::Null] {
        f.put("gitlab-detail.json", value);
        let error = f.read("get").await.unwrap_err();
        assert_eq!(error["code"], "invalid_response");
        assert_eq!(error["operation"], "pullRequests.get");
    }
}

#[tokio::test]
async fn pull_requests_review_fix_github_body_reactions_use_graphql_viewer_state() {
    let f = Fixture::new(false).await;
    let mut detail = f.json("github-detail.json");
    let mut viewer = f.json("github-viewer.json");
    viewer["data"]["repository"]["pullRequest"]["reactionGroups"] =
        detail["reactionGroups"].clone();
    viewer["data"]["repository"]["pullRequest"]["reactionGroups"][0]["viewerHasReacted"] =
        json!(true);
    for group in detail["reactionGroups"].as_array_mut().unwrap() {
        group.as_object_mut().unwrap().remove("viewerHasReacted");
    }
    f.put("github-detail.json", detail);
    f.put("github-viewer.json", viewer);
    assert_eq!(
        f.read("get").await.unwrap()["reactions"][0]["viewerReacted"],
        true
    );
}

#[tokio::test]
async fn pull_requests_review_fix_github_files_beyond_cli_first_hundred_are_not_lost() {
    let f = Fixture::new(false).await;
    let first = (0..100)
        .map(
            |n| json!({"path":format!("f{n}.rs"),"additions":1,"deletions":0,"changeType":"ADDED"}),
        )
        .collect::<Vec<_>>();
    f.put(
        "github-files.json",
        json!({"files":first,"baseRefOid":"base-sha","headRefOid":"head-sha"}),
    );
    f.put("github-files-first.json",json!({"data":{"repository":{"pullRequest":{"files":{"nodes":first,"pageInfo":{"hasNextPage":true,"endCursor":"files-next"}}}}}}));
    f.put("github-files-next.json",json!({"data":{"repository":{"pullRequest":{"files":{"nodes":[{"path":"f100.rs","additions":1,"deletions":0,"changeType":"ADDED"}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}));
    fs::write(
        f.root.path().join("patch"),
        "diff --git a/f100.rs b/f100.rs\n--- /dev/null\n+++ b/f100.rs\n@@ -0,0 +1 @@\n+last\n",
    )
    .unwrap();
    let files = f.read("getFiles").await.unwrap();
    assert_eq!(files["files"].as_array().unwrap().len(), 101);
    assert_eq!(files["truncated"], false);
    assert!(
        files["files"][100]["patch"]
            .as_str()
            .unwrap()
            .contains("+last")
    );
}

#[tokio::test]
async fn pull_requests_review_fix_gitlab_note_and_thread_reactions_are_observed() {
    let f = Fixture::new(true).await;
    let mut page = f.json("gitlab-timeline.json");
    for discussion in page["data"]["project"]["mergeRequest"]["discussions"]["nodes"]
        .as_array_mut()
        .unwrap()
    {
        for note in discussion["notes"]["nodes"].as_array_mut().unwrap() {
            note["awardEmoji"]["nodes"] = json!([{"name":"thumbsup","user":{"username":"viewer"}},{"name":"heart","user":{"username":"someone"}}]);
        }
    }
    f.put("gitlab-timeline.json", page);
    let timeline = f.read("getTimeline").await.unwrap();
    let items = timeline["items"].as_array().unwrap();
    let comment = items.iter().find(|v| v["kind"] == "comment").unwrap();
    let thread = items.iter().find(|v| v["kind"] == "thread").unwrap();
    for reactions in [&comment["reactions"], &thread["comments"][0]["reactions"]] {
        assert_eq!(reactions.as_array().unwrap().len(), 2);
        assert_eq!(reactions[0]["content"], "+1");
        assert_eq!(reactions[0]["viewerReacted"], true);
    }
}

#[tokio::test]
async fn pull_requests_review_fix_gitlab_body_reactions_paginate_without_losing_viewer() {
    let f = Fixture::new(true).await;
    let first = (0..100)
        .map(|n| json!({"id":n,"name":"thumbsup","user":{"username":format!("user-{n}")}}))
        .collect::<Vec<_>>();
    f.put("body-awards-legacy.json", json!(first[..20]));
    f.put("body-awards-first.json", json!(first));
    f.put(
        "body-awards-next.json",
        json!([{"id":101,"name":"thumbsup","user":{"username":"viewer"}}]),
    );
    let d = f.read("get").await.unwrap();
    assert_eq!(d["reactions"][0]["count"], 101);
    assert_eq!(d["reactions"][0]["viewerReacted"], true);
}

#[tokio::test]
async fn pull_requests_review_fix_gitlab_public_reader_uses_host_comment_permission() {
    let f = Fixture::new(true).await;
    let mut project = f.json("gitlab-project.json");
    project["permissions"] = Value::Null;
    f.put("gitlab-project.json", project);
    let mut metadata = f.json("metadata.json");
    metadata["data"]["project"]["mergeRequest"]["userPermissions"]["createNote"] = json!(true);
    f.put("metadata.json", metadata.clone());
    let d = f.read("get").await.unwrap();
    assert_eq!(d["permissions"]["comment"]["allowed"], true);
    assert_eq!(d["permissions"]["react"]["allowed"], true);
    metadata["data"]["project"]["mergeRequest"]["userPermissions"]["createNote"] = json!(false);
    f.put("metadata.json", metadata);
    assert_eq!(
        f.read("get").await.unwrap()["permissions"]["comment"]["allowed"],
        false
    );
}

#[tokio::test]
async fn pull_requests_review_fix_github_file_pagination_reports_remaining_pages_at_cap() {
    let f = Fixture::new(false).await;
    let first = (0..100)
        .map(
            |n| json!({"path":format!("f{n}.rs"),"additions":1,"deletions":0,"changeType":"ADDED"}),
        )
        .collect::<Vec<_>>();
    f.put(
        "github-files.json",
        json!({"files":first,"baseRefOid":"base-sha","headRefOid":"head-sha"}),
    );
    let page = json!({"data":{"repository":{"pullRequest":{"files":{"nodes":first,"pageInfo":{"hasNextPage":true,"endCursor":"files-next"}}}}}});
    f.put("github-files-first.json", page.clone());
    f.put("github-files-next.json", page);
    let files = f.read("getFiles").await.unwrap();
    assert_eq!(files["truncated"], true);
    assert_eq!(
        f.calls("gh")
            .lines()
            .filter(|l| l.contains("api graphql"))
            .count(),
        10
    );
}

#[tokio::test]
async fn pull_requests_gitlab_active_auto_merge_does_not_invent_method_from_project_default() {
    let f = Fixture::new(true).await;
    let mut detail = f.json("gitlab-detail.json");
    detail["merge_when_pipeline_succeeds"] = json!(true);
    f.put("gitlab-detail.json", detail);
    let d = f.read("get").await.unwrap();
    assert_eq!(d["readiness"]["autoMerge"]["enabled"], true);
    assert!(d["readiness"]["autoMerge"]["method"].is_null());
    assert_eq!(d["permissions"]["merge"]["defaultMethod"], "squash");
}

#[tokio::test]
async fn pull_requests_gitlab_round1_rpc_150_notes_keep_ids_with_bounded_processes() {
    let f = Fixture::new(true).await;
    let mut first = f.json("gitlab-timeline.json");
    let template = first["data"]["project"]["mergeRequest"]["discussions"]["nodes"][1].clone();
    let nodes = (0..150)
        .map(|n| {
            let mut discussion = template.clone();
            discussion["id"] = json!(format!("gid://gitlab/Discussion/discussion-{n}"));
            discussion["notes"]["nodes"][0]["id"] =
                json!(format!("gid://gitlab/Note/{}", n + 1000));
            discussion
        })
        .collect::<Vec<_>>();
    let mut second = first.clone();
    first["data"]["project"]["mergeRequest"]["discussions"]["nodes"] = json!(nodes[..100]);
    first["data"]["project"]["mergeRequest"]["discussions"]["pageInfo"] =
        json!({"hasNextPage":true,"endCursor":"after-100"});
    second["data"]["project"]["mergeRequest"]["discussions"]["nodes"] = json!(nodes[100..]);
    f.put("gitlab-timeline.json", first);
    f.put("gitlab-timeline-next.json", second);
    let timeline = f.read("getTimeline").await.unwrap();
    assert_eq!(timeline["items"].as_array().unwrap().len(), 150);
    assert_eq!(timeline["items"][149]["id"], "nt:discussion-149:1149");
    assert_eq!(timeline["truncated"], false);
    let calls = f.calls("glab");
    // Two scope/auth probes, seven fixed reads, and two GraphQL pages. The
    // existing scope also runs one real git remote read, for 12 processes total.
    assert_eq!(calls.lines().count(), 11);
    assert_eq!(
        calls.lines().filter(|l| l.contains("POST graphql")).count(),
        2
    );
    assert!(!calls.contains("/notes/"));
    assert!(!calls.contains("/award_emoji"));
}
