use super::*;
use crate::{pull_requests::permissions, test_support::TestSandbox};
use serde_json::json;
use std::fs;

fn fixture() -> (TestSandbox, GitLabHost, HostScope) {
    let s = TestSandbox::new("pr-gitlab-detail");
    for (name, body) in [
        (
            "detail.json",
            include_str!("../../../tests/fixtures/pull_requests/gitlab_detail.json"),
        ),
        (
            "approvals.json",
            include_str!("../../../tests/fixtures/pull_requests/gitlab_approvals.json"),
        ),
        (
            "reviewers.json",
            include_str!("../../../tests/fixtures/pull_requests/gitlab_reviewers.json"),
        ),
        (
            "project.json",
            include_str!("../../../tests/fixtures/pull_requests/gitlab_project.json"),
        ),
    ] {
        fs::write(s.path(name), body).unwrap();
    }
    fs::write(
        s.path("timeline.json"),
        include_str!("../../../tests/fixtures/pull_requests/gitlab_timeline_graphql.json"),
    )
    .unwrap();
    fs::write(s.path("version"), "17.9.1").unwrap();
    fs::write(
        s.path("invalid-field.json"),
        include_str!("../../../tests/fixtures/pull_requests/gitlab_detail_invalid_field.json"),
    )
    .unwrap();
    fs::write(s.path("metadata.json"),json!({"data":{"project":{"mergeRequest":{"commitCount":2,"resolvableDiscussionsCount":1,"diffStatsSummary":{"additions":7,"deletions":3,"fileCount":2},"sourceProject":{"fullPath":"contributor/cli"},"userPermissions":{"createNote":true,"pushToSourceBranch":false}}}}}).to_string()).unwrap();
    fs::write(s.path("versions.json"),json!([{"base_commit_sha":"version-base","start_commit_sha":"version-start","head_commit_sha":"head-sha"}]).to_string()).unwrap();
    let glab=s.executable_script("glab",r##"
printf '%s\n' "$*" >> calls
# With a `barrier` file, the five action reads each wait until all five have started.
if [ -e barrier ]; then
  case "$1 $2" in
    'api projects/gitlab-org%2Fcli'|'api projects/gitlab-org%2Fcli/merge_requests/3941'|'api projects/gitlab-org%2Fcli/merge_requests/3941/approvals'|'api projects/gitlab-org%2Fcli/merge_requests/3941/reviewers'|'api --method')
      mkdir -p started && : > "started/$$"
      n=0
      while [ "$(ls started | wc -l)" -lt 5 ]; do n=$((n+1)); [ "$n" -gt 100 ] && exit 75; sleep 0.05; done ;;
  esac
fi
case "$1 $2" in
  'api user') echo '{"id":8,"username":"viewer","name":null}' ;;
  'api version') if [ -e version-denied ]; then echo 'HTTP 403' >&2; exit 1; fi; printf '{"version":"%s"}\n' "$(cat version)" ;;
  'api projects/gitlab-org%2Fcli') cat project.json ;;
  'api projects/gitlab-org%2Fcli/merge_requests/3941') cat detail.json ;;
  'api projects/gitlab-org%2Fcli/merge_requests/3941/approvals') cat approvals.json ;;
  'api projects/gitlab-org%2Fcli/merge_requests/3941/reviewers') cat reviewers.json ;;
  'api projects/gitlab-org%2Fcli/merge_requests/3941/approval_state') if [ -e approval-state.json ]; then cat approval-state.json; else echo 'HTTP 404' >&2;exit 1;fi ;;
  'api projects/gitlab-org%2Fcli/merge_requests/3941/award_emoji?per_page=100&page=1') echo '[{"name":"thumbsup","user":{"username":"viewer"}},{"name":"thumbsup","user":{"username":"someone"}},{"name":"laughing","user":{"username":"viewer"}},{"name":"unsupported","user":{"username":"viewer"}}]' ;;
  'api projects/gitlab-org%2Fcli/merge_requests/3941/versions') cat versions.json ;;
  'api projects/gitlab-org%2Fcli/merge_requests/3941/closes_issues?per_page=100') echo '[{"iid":12,"title":"Issue","web_url":"https://gitlab.com/gitlab-org/cli/-/issues/12","references":{"relative":"#12"}}]' ;;
  'api projects/gitlab-org%2Fcli/merge_requests/3941/discussions?per_page=100&page=2') if [ -e discussions-next.json ]; then cat discussions-next.json; else cat discussions.json; fi ;;
  'api projects/gitlab-org%2Fcli/merge_requests/3941/discussions?per_page=100&page='*) cat discussions.json ;;
  'api projects/gitlab-org%2Fcli/merge_requests/3941/notes/'*'/award_emoji?per_page=100&page=1') echo '[]' ;;
  'api projects/gitlab-org%2Fcli/merge_requests/3941/notes/'*) cat note.json ;;
  'api projects/gitlab-org%2Fcli/merge_requests/3941/resource_label_events?per_page=100') echo '[{"id":1,"user":{"username":"viewer"},"action":"add","label":{"name":"bug"},"created_at":"2026-09-20T04:00:00Z"}]' ;;
  'api projects/gitlab-org%2Fcli/merge_requests/3941/resource_milestone_events?per_page=100') echo '[]' ;;
  'api projects/gitlab-org%2Fcli/merge_requests/3941/resource_state_events?per_page=100') echo '[]' ;;
  'api projects/gitlab-org%2Fcli/merge_requests/3941/commits?per_page=100') cat commits.json ;;
  'api projects/gitlab-org%2Fcli/merge_requests/3941/pipelines?per_page=1') cat pipelines.json ;;
  'api projects/2/pipelines/9/jobs?per_page=100') cat jobs.json ;;
  'api projects/gitlab-org%2Fcli/merge_requests/3941/diffs?per_page=50&page='*) cat diffs.json ;;
  'api --method') test "$5" = -H && test "$6" = 'Content-Type: application/json' && test "$7" = --input || exit 65
    if grep -q 'resolveNote' "$8"; then cat invalid-field.json
    elif grep -q 'PullRequestsGitLabTimeline' "$8"; then
      if grep -q 'after-100' "$8" && [ -e timeline-next.json ]; then cat timeline-next.json; else cat timeline.json; fi
    else cat metadata.json; fi ;;
  *) exit 64 ;;
esac
"##,"");
    let host = GitLabHost::new(Arc::new(
        HostCommandRunner::new(s.path("state")).with_commands("missing-gh", glab, "git"),
    ));
    let scope = HostScope {
        cwd: s.root().into(),
        host: "gitlab.com".into(),
        repository: "gitlab-org/cli".into(),
        provider: PullRequestsProvider::Gitlab,
    };
    (s, host, scope)
}

#[tokio::test]
async fn pull_requests_gitlab_action_detail_reads_in_one_wave() {
    let (s, host, scope) = fixture();
    let c = CancellationToken::new();
    let context = host.context(&scope, &c).await.unwrap();
    // Reads that run one after another wait out the barrier and fail.
    fs::write(s.path("barrier"), "").unwrap();
    host.action_detail(&scope, 3941, &context, &c)
        .await
        .expect("the five action reads run together");
    assert_eq!(fs::read_dir(s.path("started")).unwrap().count(), 5);
}

#[tokio::test]
async fn pull_requests_gitlab_context_reuses_positive_answers_for_thirty_seconds() {
    let (s, host, scope) = fixture();
    let c = CancellationToken::new();
    host.context(&scope, &c).await.unwrap();
    assert_eq!(
        fs::read_to_string(s.path("calls")).unwrap().lines().count(),
        3
    );
    host.context(&scope, &c).await.unwrap();
    assert_eq!(
        fs::read_to_string(s.path("calls")).unwrap().lines().count(),
        3,
        "a warm context must not rerun user/project/version"
    );
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(31)).await;
    tokio::time::resume();
    host.context(&scope, &c).await.unwrap();
    assert_eq!(
        fs::read_to_string(s.path("calls")).unwrap().lines().count(),
        6
    );
}

#[tokio::test]
async fn pull_requests_gitlab_live_diff_note_keeps_thread_suggestion_and_resource_events() {
    let (s, host, scope) = fixture();
    for (target, source) in [
        (
            "timeline.json",
            include_str!(
                "../../../tests/fixtures/pull_requests/gitlab_live_inline/timeline-graphql-mr1.json"
            ),
        ),
        (
            "note.json",
            include_str!("../../../tests/fixtures/pull_requests/gitlab_live_inline/note-4042.json"),
        ),
        (
            "detail.json",
            include_str!("../../../tests/fixtures/pull_requests/gitlab_live_inline/mr.json"),
        ),
        (
            "approvals.json",
            include_str!("../../../tests/fixtures/pull_requests/gitlab_live_inline/approvals.json"),
        ),
        (
            "reviewers.json",
            include_str!("../../../tests/fixtures/pull_requests/gitlab_live_inline/reviewers.json"),
        ),
    ] {
        fs::write(s.path(target), source).unwrap();
    }
    let timeline = host
        .timeline(&scope, 3941, &CancellationToken::new())
        .await
        .unwrap();
    let value = serde_json::to_value(timeline).unwrap();
    let items = value["items"].as_array().unwrap();
    let thread = items.iter().find(|item| item["kind"] == "thread").unwrap();
    let id = thread["comments"][0]["id"].as_str().unwrap();
    assert!(matches!(
        parse::action_id(id).unwrap(),
        parse::ActionId::Note { note: 4042, .. }
    ));
    assert_eq!(thread["comments"][0]["suggestion"]["id"], "3");
    assert_eq!(
        thread["comments"][0]["suggestion"]["toContent"],
        "line two (suggested)\n"
    );
    assert!(
        thread["comments"][0]["suggestion"]["applicable"]
            .as_bool()
            .unwrap()
    );
    assert!(
        fs::read_to_string(s.path("calls"))
            .unwrap()
            .contains("/notes/4042 --hostname")
    );
    assert!(items.iter().any(|item| item["kind"] == "comment"));
    assert!(items.iter().any(|item| item["kind"] == "event"));
}

#[test]
fn pull_requests_gitlab_live_resource_events_keep_add_remove_and_milestone_names() {
    for (kind, source, expected) in [
        (
            "label",
            include_str!(
                "../../../tests/fixtures/pull_requests/gitlab_live_inline/label-events.json"
            ),
            vec!["labeled", "unlabeled", "labeled"],
        ),
        (
            "milestone",
            include_str!(
                "../../../tests/fixtures/pull_requests/gitlab_live_inline/milestone-events.json"
            ),
            vec!["milestoned", "demilestoned"],
        ),
    ] {
        let rows: Vec<Value> = serde_json::from_str(source).unwrap();
        let parsed: Vec<_> = rows
            .iter()
            .map(|row| serde_json::to_value(parse::resource_event(row, kind).unwrap()).unwrap())
            .collect();
        assert_eq!(
            parsed
                .iter()
                .map(|item| item["event"].as_str().unwrap())
                .collect::<Vec<_>>(),
            expected
        );
        for (row, item) in rows.iter().zip(parsed) {
            assert_eq!(
                item["detail"],
                row[kind][if kind == "label" { "name" } else { "title" }]
            );
        }
    }
}

#[tokio::test]
async fn pull_requests_gitlab_detail_obeys_host_answers_and_reads_reviewers_rules_reactions_counts()
{
    let (s, host, scope) = fixture();
    let c = CancellationToken::new();
    let context = host.context(&scope, &c).await.unwrap();
    let raw = host.detail(&scope, 3941, &context, &c).await.unwrap();
    let (p, r) = permissions::compute(&raw.inputs);
    let d = raw.detail_without_permissions;
    assert!(!p.approve.allowed);
    assert!(p.approve.reason.is_some());
    assert!(!p.merge.permission.allowed);
    assert!(p.request_changes.allowed);
    assert_eq!(r.status, ReadinessStatus::ReviewRequired);
    assert_eq!(r.summary, "1 approval required.");
    assert_eq!(
        p.merge.permission.reason.as_deref(),
        Some("GitLab does not allow your account to merge this merge request.")
    );
    assert_eq!(d.reviewers[0].state, ReviewerState::Unreviewed);
    assert_eq!(d.reviewers[1].state, ReviewerState::Commented);
    assert_eq!(
        raw.inputs.gl.unwrap().pipeline_status.as_deref(),
        Some("success")
    );
    assert_eq!(d.approval_rules[0].required, 1);
    assert_eq!(d.approval_rules[0].approved, 0);
    assert_eq!(d.commit_count, 2);
    assert_eq!(d.additions, 7);
    assert_eq!(d.deletions, 3);
    assert_eq!(d.tab_counts.conversation, Some(3));
    assert_eq!(d.base_sha, "base-sha");
    assert_eq!(d.head_repository.as_deref(), Some("contributor/cli"));
    assert_eq!(d.linked_issues[0].reference, "#12");
    assert_eq!(d.reactions.len(), 2);
    assert_eq!(d.reactions[0].count, 2);
    assert!(d.reactions[0].viewer_reacted);
    assert!(context.version_unavailable_reason().is_none());
    let calls = fs::read_to_string(s.path("calls")).unwrap();
    assert_eq!(
        calls.lines().filter(|l| l.starts_with("api user ")).count(),
        1
    );
    assert!(calls.lines().all(|l| l.ends_with("--hostname gitlab.com")));
}

#[tokio::test]
async fn pull_requests_gitlab_detail_request_changes_tracks_reviewer_and_instance_version() {
    let (s, host, scope) = fixture();
    let c = CancellationToken::new();
    fs::write(s.path("version"), "16.11.7").unwrap();
    let context = host.context(&scope, &c).await.unwrap();
    let raw = host.detail(&scope, 3941, &context, &c).await.unwrap();
    assert_eq!(
        permissions::compute(&raw.inputs)
            .0
            .request_changes
            .reason
            .as_deref(),
        Some("This GitLab instance is 16.11; requesting changes needs 17.2.")
    );
    fs::write(s.path("version"), "17.9.1").unwrap();
    fs::write(
        s.path("invalid-field.json"),
        include_str!("../../../tests/fixtures/pull_requests/gitlab_detail_invalid_field.json"),
    )
    .unwrap();
    fs::write(s.path("reviewers.json"), "[]").unwrap();
    let context = host.context(&scope, &c).await.unwrap();
    let raw = host.detail(&scope, 3941, &context, &c).await.unwrap();
    assert!(!permissions::compute(&raw.inputs).0.request_changes.allowed);
    fs::write(s.path("version-denied"), "").unwrap();
    host.invalidate_context(); // Model the explicit Rescan after a host-version change.
    let context = host.context(&scope, &c).await.unwrap();
    let raw = host.detail(&scope, 3941, &context, &c).await.unwrap();
    assert_eq!(
        permissions::compute(&raw.inputs)
            .0
            .request_changes
            .reason
            .as_deref(),
        Some("GitLab version could not be read")
    );
}

#[tokio::test]
async fn pull_requests_gitlab_detail_uses_approval_state_and_falls_back_to_latest_version_refs() {
    let (s, host, scope) = fixture();
    let c = CancellationToken::new();
    fs::write(s.path("approval-state.json"),json!({"rules":[{"name":"Owners","approvals_required":2,"approved_by":[{"username":"alice"}],"eligible_approvers":[{"username":"alice"},{"username":"bob"}]}]}).to_string()).unwrap();
    let mut v: Value =
        serde_json::from_str(&fs::read_to_string(s.path("detail.json")).unwrap()).unwrap();
    v["diff_refs"] = Value::Null;
    fs::write(s.path("detail.json"), v.to_string()).unwrap();
    let context = host.context(&scope, &c).await.unwrap();
    let raw = host.detail(&scope, 3941, &context, &c).await.unwrap();
    let d = raw.detail_without_permissions;
    assert_eq!(d.base_sha, "version-base");
    assert_eq!(d.approval_rules[0].name, "Owners");
    assert_eq!(d.approval_rules[0].approved, 1);
    assert_eq!(d.approval_rules[0].required, 2);
    assert_eq!(d.approval_rules[0].approvers.len(), 2);
}

#[tokio::test]
async fn pull_requests_gitlab_timeline_maps_position_note_ids_suggestions_and_system_events() {
    let (s, host, scope) = fixture();
    fs::write(
        s.path("timeline.json"),
        include_str!("../../../tests/fixtures/pull_requests/gitlab_timeline_graphql.json"),
    )
    .unwrap();
    fs::write(s.path("note.json"),json!({"suggestions":[{"id":7,"appliable":true,"applied":false,"from_line":2,"to_line":2,"from_content":"old","to_content":"new"}]}).to_string()).unwrap();
    let timeline = host
        .timeline(&scope, 3941, &CancellationToken::new())
        .await
        .unwrap();
    assert!(!timeline.truncated);
    let v = serde_json::to_value(timeline).unwrap();
    assert_eq!(v["items"][0]["id"], "nt:discussion-2:11");
    assert_eq!(v["items"][0]["viewerIsAuthor"], true);
    let t = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["kind"] == "thread")
        .unwrap();
    assert_eq!(t["id"], "ds:discussion-1");
    assert_eq!(t["path"], "src/main.rs");
    assert_eq!(t["line"], 2);
    assert_eq!(t["side"], "right");
    assert_eq!(t["isResolved"], false);
    assert_eq!(t["canResolve"], true);
    assert_eq!(t["comments"][0]["id"], "nt:discussion-1:10");
    assert_eq!(t["comments"][0]["suggestion"]["id"], "7");
    assert_eq!(t["comments"][0]["suggestion"]["applicable"], true);
    assert!(
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["event"] == "commits_added")
    );
    assert!(
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["event"] == "labeled" && v["detail"] == "bug")
    );
}

#[tokio::test]
async fn pull_requests_gitlab_timeline_adds_synthetic_reviews_and_disables_applied_suggestions() {
    let (s, host, scope) = fixture();
    fs::write(
        s.path("timeline.json"),
        include_str!("../../../tests/fixtures/pull_requests/gitlab_timeline_graphql.json"),
    )
    .unwrap();
    fs::write(s.path("note.json"),json!({"suggestions":[{"id":7,"appliable":true,"applied":true,"from_line":2,"to_line":2,"from_content":"old","to_content":"new"}]}).to_string()).unwrap();
    fs::write(
        s.path("approvals.json"),
        json!({"approved_by":[{"user":{"id":5,"username":"alice"}}]}).to_string(),
    )
    .unwrap();
    fs::write(
        s.path("reviewers.json"),
        json!([{"user":{"id":6,"username":"bob"},"state":"requested_changes"}]).to_string(),
    )
    .unwrap();
    let timeline = host
        .timeline(&scope, 3941, &CancellationToken::new())
        .await
        .unwrap();
    let v = serde_json::to_value(timeline).unwrap();
    let items = v["items"].as_array().unwrap();
    assert!(items.iter().any(|v| v["id"] == "ap:5"
        && v["state"] == "approved"
        && v["submittedAt"] == "2026-09-20T00:00:00Z"));
    assert!(
        items
            .iter()
            .any(|v| v["id"] == "ap:6" && v["state"] == "changes_requested")
    );
    let t = items.iter().find(|v| v["kind"] == "thread").unwrap();
    assert_eq!(t["comments"][0]["suggestion"]["applicable"], false);
}

#[tokio::test]
async fn pull_requests_gitlab_commits_keep_author_name_without_inventing_email_login() {
    let (s, host, scope) = fixture();
    fs::write(s.path("commits.json"),json!([{"id":"0123456789","short_id":"01234567","title":"Title","message":"Title\n\nBody","author_name":"Author Name","author_email":"do-not-use@example.test","authored_date":"2026-09-20T00:00:00Z","web_url":"https://gitlab.com/commit/0123456789"}]).to_string()).unwrap();
    let r = host
        .commits(&scope, 3941, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(r.commits[0].author.login, "Author Name");
    assert_eq!(r.commits[0].author.name, None);
    assert_eq!(r.commits[0].short_sha, "01234567");
    assert_eq!(r.commits[0].body.as_deref(), Some("Body"));
}

#[tokio::test]
async fn pull_requests_gitlab_checks_group_latest_pipeline_jobs_by_stage() {
    let (s, host, scope) = fixture();
    fs::write(
        s.path("pipelines.json"),
        json!([{"id":9,"project_id":2,"web_url":"https://gitlab.com/pipeline/9"}]).to_string(),
    )
    .unwrap();
    fs::write(s.path("jobs.json"),json!([{"name":"compile","stage":"build","status":"success","web_url":"https://gitlab.com/job/1","started_at":null,"finished_at":null,"duration":4.5},{"name":"unit","stage":"test","status":"failed"},{"name":"manual","stage":"test","status":"manual"}]).to_string()).unwrap();
    let checks = host
        .checks(&scope, 3941, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(checks.groups.len(), 2);
    assert_eq!(checks.groups[0].name, "build");
    assert_eq!(checks.groups[0].checks[0].duration_seconds, Some(4.5));
    assert_eq!(checks.groups[1].name, "test");
    assert_eq!(checks.groups[1].checks[1].state, CheckState::Skipped);
    assert_eq!(checks.summary, CheckSummary::Failure);
    assert_eq!(
        checks.pipeline_url.as_deref(),
        Some("https://gitlab.com/pipeline/9")
    );
    fs::write(s.path("pipelines.json"), "[]").unwrap();
    assert_eq!(
        host.checks(&scope, 3941, &CancellationToken::new())
            .await
            .unwrap()
            .summary,
        CheckSummary::None
    );
}

#[tokio::test]
async fn pull_requests_gitlab_files_prefix_hunks_keep_renames_binary_and_collapsed_rows() {
    let (s, host, scope) = fixture();
    fs::write(s.path("diffs.json"),json!([
      {"old_path":"old.rs","new_path":"new.rs","renamed_file":true,"diff":"@@ -1 +1 @@\n-old\n+new\n"},
      {"old_path":"binary.png","new_path":"binary.png","diff":"Binary files a/binary.png and b/binary.png differ"},
      {"old_path":"large.rs","new_path":"large.rs","collapsed":true,"diff":""}
    ]).to_string()).unwrap();
    let files = host
        .files(&scope, 3941, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(files.files.len(), 3);
    assert_eq!(files.files[0].previous_path.as_deref(), Some("old.rs"));
    assert!(
        files.files[0]
            .patch
            .as_deref()
            .unwrap()
            .starts_with("--- a/old.rs\n+++ b/new.rs\n")
    );
    assert_eq!(files.files[0].additions, 1);
    assert_eq!(files.files[0].deletions, 1);
    assert_eq!(files.files[1].change_type, ChangeType::Binary);
    assert_eq!(files.files[1].patch, None);
    assert!(files.files[2].too_large);
    assert_eq!(files.diff_refs.base_sha, "version-base");
    assert_eq!(files.diff_refs.start_sha, "version-start");
    assert!(!files.truncated);
}

#[tokio::test]
async fn pull_requests_gitlab_large_file_drops_patch_at_one_mib_but_keeps_row() {
    let (s, host, scope) = fixture();
    fs::write(s.path("diffs.json"),json!([{"old_path":"large.rs","new_path":"large.rs","diff":format!("@@ -0,0 +1 @@\n+{}\n","x".repeat(1024*1024))}]).to_string()).unwrap();
    let files = host
        .files(&scope, 3941, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(files.files.len(), 1);
    assert!(files.files[0].too_large);
    assert_eq!(files.files[0].patch, None);
}

#[tokio::test]
async fn pull_requests_gitlab_diffs_and_discussions_stop_at_page_caps() {
    let (s, host, scope) = fixture();
    fs::write(
        s.path("diffs.json"),
        json!(
            (0..50)
                .map(|n| json!({"old_path":format!("f{n}"),"new_path":format!("f{n}"),"diff":""}))
                .collect::<Vec<_>>()
        )
        .to_string(),
    )
    .unwrap();
    let files = host
        .files(&scope, 3941, &CancellationToken::new())
        .await
        .unwrap();
    assert!(files.truncated);
    let calls = fs::read_to_string(s.path("calls")).unwrap();
    assert_eq!(calls.lines().filter(|l| l.contains("/diffs?")).count(), 20);
    let discussions = (0..100)
        .map(|n| {
            let mut note = graphql_note(n, "unknown event");
            note["system"] = json!(true);
            graphql_discussion(n, vec![note])
        })
        .collect::<Vec<_>>();
    fs::write(
        s.path("timeline.json"),
        graphql_timeline_page(discussions, true).to_string(),
    )
    .unwrap();
    let timeline = host
        .timeline(&scope, 3941, &CancellationToken::new())
        .await
        .unwrap();
    assert!(timeline.truncated);
    let calls = fs::read_to_string(s.path("calls")).unwrap();
    assert_eq!(
        calls.lines().filter(|l| l.contains("POST graphql")).count(),
        10
    );
}

#[tokio::test]
async fn pull_requests_gitlab_whole_patch_over_eight_mib_keeps_metadata_rows() {
    let (s, host, scope) = fixture();
    fs::write(
        s.path("diffs.json"),
        json!([{"old_path":"huge.rs","new_path":"huge.rs","diff":"x".repeat(9*1024*1024)}])
            .to_string(),
    )
    .unwrap();
    let mut metadata: Value =
        serde_json::from_str(&fs::read_to_string(s.path("metadata.json")).unwrap()).unwrap();
    metadata["data"]["project"]["mergeRequest"]["diffStats"] = json!([{"path":"huge.rs","additions":1,"deletions":0},{"path":"small.rs","additions":2,"deletions":1}]);
    fs::write(s.path("metadata.json"), metadata.to_string()).unwrap();
    let files = host
        .files(&scope, 3941, &CancellationToken::new())
        .await
        .unwrap();
    assert!(files.truncated);
    assert_eq!(files.files.len(), 2);
    assert!(files.files.iter().all(|f| f.patch.is_none() && f.too_large));
}

#[tokio::test]
async fn pull_requests_gitlab_round1_live_permission_shape_allows_detail_without_resolve_note() {
    let (s, host, scope) = fixture();
    let c = CancellationToken::new();
    let context = host.context(&scope, &c).await.unwrap();
    let raw = host.detail(&scope, 3941, &context, &c).await.unwrap();
    assert!(permissions::compute(&raw.inputs).0.resolve_threads.allowed);
    let mut metadata: Value =
        serde_json::from_str(&fs::read_to_string(s.path("metadata.json")).unwrap()).unwrap();
    metadata["data"]["project"]["mergeRequest"]["userPermissions"]["createNote"] = json!(false);
    fs::write(s.path("metadata.json"), metadata.to_string()).unwrap();
    let raw = host.detail(&scope, 3941, &context, &c).await.unwrap();
    assert!(!permissions::compute(&raw.inputs).0.resolve_threads.allowed);
    metadata["data"]["project"]["mergeRequest"]["userPermissions"]["createNote"] = json!(true);
    metadata["data"]["project"]["mergeRequest"]["resolvableDiscussionsCount"] = json!(0);
    fs::write(s.path("metadata.json"), metadata.to_string()).unwrap();
    let raw = host.detail(&scope, 3941, &context, &c).await.unwrap();
    assert!(!permissions::compute(&raw.inputs).0.resolve_threads.allowed);
}

fn graphql_timeline_page(discussions: Vec<Value>, next: bool) -> Value {
    json!({
        "data": {"project": {"mergeRequest": {
            "userPermissions": {"createNote": true},
            "discussions": {
                "nodes": discussions,
                "pageInfo": {"hasNextPage": next, "endCursor": if next {Some("after-100")} else {None}}
            }
        }}}
    })
}

fn graphql_note(number: u64, body: &str) -> Value {
    json!({"id":format!("gid://gitlab/Note/{number}"),"body":body,"system":false,"resolvable":false,"resolved":false,"createdAt":"2026-09-20T00:00:00Z","updatedAt":"2026-09-20T00:00:00Z","author":{"username":"viewer","name":null},"position":null,"awardEmoji":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}})
}

fn graphql_discussion(number: u64, notes: Vec<Value>) -> Value {
    json!({"id":format!("gid://gitlab/Discussion/discussion-{number}"),"resolvable":false,"resolved":false,"notes":{"nodes":notes,"pageInfo":{"hasNextPage":false,"endCursor":null}}})
}

#[tokio::test]
async fn pull_requests_gitlab_round1_150_notes_use_two_graphql_pages_and_nine_processes() {
    let (s, host, scope) = fixture();
    let rest=(0..150).map(|n|json!({"id":format!("discussion-{n}"),"notes":[{"id":n+1000,"body":format!("Note {n}"),"system":false,"author":{"username":"viewer"},"created_at":"2026-09-20T00:00:00Z"}]})).collect::<Vec<_>>();
    fs::write(s.path("discussions.json"), json!(rest[..100]).to_string()).unwrap();
    fs::write(
        s.path("discussions-next.json"),
        json!(rest[100..]).to_string(),
    )
    .unwrap();
    let rows = (0..150)
        .map(|n| graphql_discussion(n, vec![graphql_note(n + 1000, &format!("Note {n}"))]))
        .collect::<Vec<_>>();
    fs::write(
        s.path("timeline.json"),
        graphql_timeline_page(rows[..100].to_vec(), true).to_string(),
    )
    .unwrap();
    fs::write(
        s.path("timeline-next.json"),
        graphql_timeline_page(rows[100..].to_vec(), false).to_string(),
    )
    .unwrap();
    let result = host
        .timeline(&scope, 3941, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        result
            .items
            .iter()
            .filter(|i| matches!(i, TimelineItem::Comment { .. }))
            .count(),
        150
    );
    assert!(!result.truncated);
    let calls = fs::read_to_string(s.path("calls")).unwrap();
    assert_eq!(
        calls.lines().count(),
        9,
        "processes must be seven fixed reads plus two GraphQL pages"
    );
    assert_eq!(
        calls.lines().filter(|c| c.contains("POST graphql")).count(),
        2
    );
    assert!(!calls.contains("/award_emoji"));
    assert!(!calls.contains("/notes/"));
}

#[tokio::test]
async fn pull_requests_gitlab_round1_thread_resolution_and_reactions_use_graphql_observations() {
    let (s, host, scope) = fixture();
    fs::write(
        s.path("discussions.json"),
        include_str!("../../../tests/fixtures/pull_requests/gitlab_discussions.json"),
    )
    .unwrap();
    fs::write(s.path("note.json"), r#"{"suggestions":[]}"#).unwrap();
    let mut page: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/pull_requests/gitlab_timeline_graphql.json"
    ))
    .unwrap();
    page["data"]["project"]["mergeRequest"]["userPermissions"]["createNote"] = json!(false);
    page["data"]["project"]["mergeRequest"]["discussions"]["nodes"][0]["notes"]["nodes"][0]["awardEmoji"]
        ["nodes"] = json!([{"name":"thumbsup","user":{"username":"viewer"}},{"name":"heart","user":{"username":"someone"}}]);
    fs::write(s.path("timeline.json"), page.to_string()).unwrap();
    let timeline = host
        .timeline(&scope, 3941, &CancellationToken::new())
        .await
        .unwrap();
    let thread = timeline
        .items
        .iter()
        .find_map(|item| {
            if let TimelineItem::Thread {
                id,
                path,
                line,
                side,
                can_resolve,
                comments,
                ..
            } = item
            {
                Some((id, path, line, side, can_resolve, comments))
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(thread.0, "ds:discussion-1");
    assert_eq!(thread.1, "src/main.rs");
    assert_eq!(*thread.2, Some(2));
    assert_eq!(*thread.3, DiffSide::Right);
    assert!(!*thread.4);
    assert_eq!(thread.5[0].id, "nt:discussion-1:10");
    assert_eq!(thread.5[0].reactions.len(), 2);
    assert!(thread.5[0].reactions[0].viewer_reacted);
    page["data"]["project"]["mergeRequest"]["userPermissions"]["createNote"] = json!(true);
    fs::write(s.path("timeline.json"), page.to_string()).unwrap();
    let timeline = host
        .timeline(&scope, 3941, &CancellationToken::new())
        .await
        .unwrap();
    assert!(timeline.items.iter().any(|i| matches!(
        i,
        TimelineItem::Thread {
            can_resolve: true,
            ..
        }
    )));
    page["data"]["project"]["mergeRequest"]["discussions"]["nodes"][0]["resolvable"] = json!(false);
    fs::write(s.path("timeline.json"), page.to_string()).unwrap();
    let timeline = host
        .timeline(&scope, 3941, &CancellationToken::new())
        .await
        .unwrap();
    assert!(timeline.items.iter().any(|i| matches!(
        i,
        TimelineItem::Thread {
            can_resolve: false,
            ..
        }
    )));
}

#[tokio::test]
async fn pull_requests_gitlab_round1_unread_nested_connections_mark_timeline_truncated() {
    let (s, host, scope) = fixture();
    fs::write(s.path("discussions.json"),r#"[{"id":"discussion-1","notes":[{"id":10,"system":false,"body":"comment","author":{"username":"viewer"},"created_at":"2026-09-20T00:00:00Z"}]}]"#).unwrap();
    for nested in ["notes", "awards"] {
        let mut note = graphql_note(10, "comment");
        if nested == "awards" {
            note["awardEmoji"]["pageInfo"] = json!({"hasNextPage":true,"endCursor":"more-awards"});
        }
        let mut discussion = graphql_discussion(1, vec![note]);
        if nested == "notes" {
            discussion["notes"]["pageInfo"] = json!({"hasNextPage":true,"endCursor":"more-notes"});
        }
        fs::write(
            s.path("timeline.json"),
            graphql_timeline_page(vec![discussion], false).to_string(),
        )
        .unwrap();
        let timeline = host
            .timeline(&scope, 3941, &CancellationToken::new())
            .await
            .unwrap();
        assert!(timeline.truncated, "{nested}");
    }
}

#[tokio::test]
async fn pull_requests_gitlab_round1_suggestion_reads_have_one_shared_fixed_budget() {
    let (s, host, scope) = fixture();
    let mut row = graphql_discussion(1, vec![]);
    let notes = (0..150)
        .map(|n| {
            let mut note = graphql_note(n + 1000, "```suggestion\nnew\n```");
            note["resolvable"] = json!(true);
            note["position"] =
                json!({"newPath":"src/main.rs","oldPath":"src/main.rs","newLine":2,"oldLine":null});
            note
        })
        .collect::<Vec<_>>();
    row["resolvable"] = json!(true);
    let rows = notes
        .into_iter()
        .enumerate()
        .map(|(n, note)| {
            let mut d = row.clone();
            d["id"] = json!(format!("gid://gitlab/Discussion/discussion-{n}"));
            d["notes"]["nodes"] = json!([note]);
            d
        })
        .collect::<Vec<_>>();
    fs::write(
        s.path("timeline.json"),
        graphql_timeline_page(rows[..100].to_vec(), true).to_string(),
    )
    .unwrap();
    fs::write(
        s.path("timeline-next.json"),
        graphql_timeline_page(rows[100..].to_vec(), false).to_string(),
    )
    .unwrap();
    let rest = (0..150).map(|n| json!({"id":format!("discussion-{n}"),"notes":[{"id":n+1000,"system":false,"body":"```suggestion\nnew\n```","author":{"username":"viewer"},"created_at":"2026-09-20T00:00:00Z","resolvable":true,"resolved":false,"position":{"new_path":"src/main.rs","new_line":2}}]})).collect::<Vec<_>>();
    fs::write(s.path("discussions.json"), json!(rest[..100]).to_string()).unwrap();
    fs::write(
        s.path("discussions-next.json"),
        json!(rest[100..]).to_string(),
    )
    .unwrap();
    fs::write(s.path("note.json"),json!({"suggestions":[{"id":7,"appliable":true,"applied":false,"from_line":2,"to_line":2,"from_content":"old","to_content":"new"}]}).to_string()).unwrap();
    let result = host
        .timeline(&scope, 3941, &CancellationToken::new())
        .await
        .unwrap();
    let suggestions = result
        .items
        .iter()
        .filter_map(|i| {
            if let TimelineItem::Thread { comments, .. } = i {
                comments[0].suggestion.as_ref()
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(suggestions.len(), 150);
    assert_eq!(suggestions.iter().filter(|s| s.id.is_some()).count(), 20);
    assert!(suggestions.iter().skip(20).all(|s| !s.applicable));
    assert!(result.truncated);
    let calls = fs::read_to_string(s.path("calls")).unwrap();
    assert_eq!(calls.lines().count(), 29);
    assert_eq!(calls.lines().filter(|l| l.contains("/notes/")).count(), 20);
    assert!(!calls.contains("/award_emoji"));
}
