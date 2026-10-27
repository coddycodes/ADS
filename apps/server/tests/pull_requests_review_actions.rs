#![cfg(unix)]
//! Mutations are exercised only against recording CLI stubs, never live hosts.
#[path = "support/pull_request_action_fixture.rs"]
mod pull_request_action_fixture;

use coddy_studio_server::pull_requests::model::ActionResult;
use pull_request_action_fixture::Fixture;
use serde_json::{Value, json};
use std::fs;

const BODY: &str = "private review body\nquotes ' \" ` $() \\ and unicode é";

#[tokio::test]
async fn pull_requests_github_comment_uses_stdin_and_pinned_host_repository() {
    let mut f = Fixture::new(false);
    f.raw_response("https://github.example.test/team/repo/pull/14#issuecomment-1");
    assert_eq!(
        f.run(json!({"action":"comment","body":BODY}))
            .await
            .unwrap(),
        ActionResult::Done
    );
    f.assert_args(
        1,
        &[
            "pr",
            "comment",
            "14",
            "--body-file",
            "-",
            "--repo",
            "github.example.test/team/repo",
        ],
    );
    assert_eq!(f.input(1), BODY);
    f.assert_body_private(1, BODY);
    assert!(
        fs::read_to_string(f.root.path().join("call-1.env"))
            .unwrap()
            .starts_with("github.example.test|")
    );
    assert_eq!(f.count(), 1);
}

#[tokio::test]
async fn pull_requests_github_edit_delete_and_dismiss_use_database_segment() {
    for (action, id, path, method, body) in [
        (
            "editComment",
            "ic:123:IC_node",
            "issues/comments/123",
            "PATCH",
            Some(json!({"body":BODY})),
        ),
        (
            "editComment",
            "rc:456:RC_node",
            "pulls/comments/456",
            "PATCH",
            Some(json!({"body":BODY})),
        ),
        (
            "deleteComment",
            "ic:123:IC_node",
            "issues/comments/123",
            "DELETE",
            None,
        ),
        (
            "deleteComment",
            "rc:456:RC_node",
            "pulls/comments/456",
            "DELETE",
            None,
        ),
        (
            "dismissReview",
            "rv:789:RV_node",
            "pulls/14/reviews/789/dismissals",
            "PUT",
            Some(json!({"message":BODY})),
        ),
    ] {
        let mut f = Fixture::new(false);
        f.raw_response("");
        let mut request = json!({"action":action});
        request[if action == "dismissReview" {
            "reviewId"
        } else {
            "commentId"
        }] = json!(id);
        if action == "editComment" {
            request["body"] = json!(BODY);
        }
        if action == "dismissReview" {
            request["message"] = json!(BODY);
        }
        assert_eq!(f.run(request).await.unwrap(), ActionResult::Done);
        let endpoint = format!("repos/team/repo/{path}");
        let mut expected = vec!["api", "--method", method, &endpoint];
        if body.is_some() {
            expected.extend(["--input", "-"]);
        }
        f.assert_args(1, &expected);
        if let Some(body) = body {
            assert_eq!(f.json_input(1), body);
            f.assert_body_private(1, BODY);
        }
    }
}

#[tokio::test]
async fn pull_requests_github_minimize_and_unminimize_use_node_segment() {
    for (minimized, mutation) in [(true, "minimizeComment"), (false, "unminimizeComment")] {
        let mut f = Fixture::new(false);
        f.response(json!({"data":{mutation:{"ok":true}}}));
        assert_eq!(f.run(json!({"action":"minimizeComment","commentId":"ic:123:IC_node","minimized":minimized})).await.unwrap(), ActionResult::Done);
        f.assert_args(1, &["api", "graphql", "--input", "-"]);
        let input = f.json_input(1);
        assert_eq!(input["variables"]["subjectId"], "IC_node");
        assert!(input["query"].as_str().unwrap().contains(mutation));
        if minimized {
            assert!(input["query"].as_str().unwrap().contains("OUTDATED"));
        }
    }
}

#[tokio::test]
async fn pull_requests_github_reply_and_resolve_use_thread_segments_and_noop_current_state() {
    let mut f = Fixture::new(false);
    f.response(json!({"id":3}));
    assert_eq!(
        f.run(json!({"action":"replyThread","threadId":"th:TH_node:456","body":BODY}))
            .await
            .unwrap(),
        ActionResult::Done
    );
    f.assert_args(
        1,
        &[
            "api",
            "--method",
            "POST",
            "repos/team/repo/pulls/14/comments/456/replies",
            "--input",
            "-",
        ],
    );
    assert_eq!(f.json_input(1), json!({"body":BODY}));
    f.assert_body_private(1, BODY);
    for resolved in [true, false] {
        for current in [true, false] {
            let mut f = Fixture::new(false);
            f.response(json!({"data":{"node":{"isResolved":current}}}));
            let mutation = if resolved {
                "resolveReviewThread"
            } else {
                "unresolveReviewThread"
            };
            if resolved != current {
                f.response(json!({"data":{mutation:{"thread":{"isResolved":resolved}}}}));
            }
            assert_eq!(f.run(json!({"action":"resolveThread","threadId":"th:TH_node:456","resolved":resolved})).await.unwrap(),ActionResult::Done);
            assert_eq!(f.json_input(1)["variables"]["id"], "TH_node");
            assert_eq!(f.count(), if current == resolved { 1 } else { 2 });
            if current != resolved {
                assert_eq!(f.json_input(2)["variables"]["threadId"], "TH_node");
                assert!(
                    f.json_input(2)["query"]
                        .as_str()
                        .unwrap()
                        .contains(mutation)
                );
            }
        }
    }
}

#[tokio::test]
async fn pull_requests_github_reactions_route_each_target_and_toggle_only_viewer() {
    for (target, path) in [
        (Value::Null, "issues/14"),
        (json!("ic:123:IC_node"), "issues/comments/123"),
        (json!("rc:456:RC_node"), "pulls/comments/456"),
    ] {
        for (on, existing) in [(true, false), (true, true), (false, true), (false, false)] {
            let mut f = Fixture::new(false);
            f.response(json!({"login":"viewer"}));
            let mut reactions =
                vec![json!({"id":77,"content":"+1","user":{"login":"someone-else"}})];
            if existing {
                reactions.push(json!({"id":88,"content":"+1","user":{"login":"viewer"}}));
            }
            f.response(json!(reactions));
            if on != existing {
                f.response(json!({}));
            }
            assert_eq!(
                f.run(json!({"action":"react","targetId":target,"content":"+1","on":on}))
                    .await
                    .unwrap(),
                ActionResult::Done
            );
            f.assert_args(1, &["api", "user"]);
            f.assert_args(
                2,
                &[
                    "api",
                    &format!("repos/team/repo/{path}/reactions?content=%2B1&per_page=100"),
                ],
            );
            assert_eq!(f.count(), if on == existing { 2 } else { 3 });
            if on && !existing {
                f.assert_args(
                    3,
                    &[
                        "api",
                        "--method",
                        "POST",
                        &format!("repos/team/repo/{path}/reactions"),
                        "--input",
                        "-",
                    ],
                );
                assert_eq!(f.json_input(3), json!({"content":"+1"}));
            }
            if !on && existing {
                f.assert_args(
                    3,
                    &[
                        "api",
                        "--method",
                        "DELETE",
                        &format!("repos/team/repo/{path}/reactions/88"),
                    ],
                );
            }
        }
    }
}

fn review(event: &str) -> Value {
    json!({"action":"submitReview","event":event,"body":BODY,"headSha":"head-sha","comments":[
        {"path":"old.rs","line":9,"startLine":7,"side":"left","body":BODY},
        {"path":"new.rs","line":3,"startLine":null,"side":"right","body":BODY}
    ]})
}

#[tokio::test]
async fn pull_requests_github_review_posts_one_atomic_body_with_line_sides() {
    for (event, host_event) in [
        ("approve", "APPROVE"),
        ("request_changes", "REQUEST_CHANGES"),
        ("comment", "COMMENT"),
    ] {
        let mut f = Fixture::new(false);
        f.response(json!({"id":1}));
        assert_eq!(
            f.run(review(event)).await.unwrap(),
            ActionResult::ReviewSubmitted {
                review_posted: true,
                landed: 2,
                failed: vec![]
            }
        );
        f.assert_args(
            1,
            &[
                "api",
                "--method",
                "POST",
                "repos/team/repo/pulls/14/reviews",
                "--input",
                "-",
            ],
        );
        assert_eq!(
            f.json_input(1),
            json!({"commit_id":"head-sha","event":host_event,"body":BODY,"comments":[
                {"path":"old.rs","line":9,"start_line":7,"side":"LEFT","start_side":"LEFT","body":BODY},
                {"path":"new.rs","line":3,"side":"RIGHT","body":BODY}
            ]})
        );
        f.assert_body_private(1, BODY);
        assert_eq!(f.count(), 1);
    }
}

#[tokio::test]
async fn pull_requests_github_422_reports_every_inline_comment_failed_without_retry() {
    let mut f = Fixture::new(false);
    f.failure("gh: Pull request review comment position is invalid (HTTP 422)");
    let result = f.run(review("approve")).await.unwrap();
    let ActionResult::ReviewSubmitted { landed, failed, .. } = result else {
        panic!("wrong result")
    };
    assert_eq!(landed, 0);
    assert_eq!(failed.len(), 2);
    assert_eq!(failed[0].path, "old.rs");
    assert_eq!(failed[1].line, 3);
    assert!(
        failed
            .iter()
            .all(|f| f.message.contains("position is invalid"))
    );
    assert_eq!(f.count(), 1);
}

#[tokio::test]
async fn pull_requests_github_review_requires_body_except_approval_and_preserves_rejection_detail()
{
    for event in ["comment", "request_changes"] {
        let f = Fixture::new(false);
        let mut request = review(event);
        request["body"] = Value::Null;
        if event == "comment" {
            request["comments"] = json!([]);
        }
        let error = f.run(request).await.unwrap_err();
        assert_eq!(error.code, "host_rejected");
        assert_eq!(
            error.message,
            if event == "request_changes" {
                "A comment is required when requesting changes."
            } else {
                "Write a review summary or add an inline comment."
            }
        );
        assert_eq!(f.count(), 0);
    }
    let mut f = Fixture::new(false);
    f.failure("gh: Review rejected by branch policy (HTTP 422)");
    let error = f.run(review("approve")).await.unwrap_err();
    assert_eq!(error.code, "host_rejected");
    assert_eq!(
        error.host_detail.as_deref(),
        Some("gh: Review rejected by branch policy (HTTP 422)")
    );
    assert_eq!(f.count(), 1);
}

#[tokio::test]
async fn pull_requests_github_rerequest_review_and_unsupported_actions() {
    let mut f = Fixture::new(false);
    f.raw_response("");
    assert_eq!(
        f.run(json!({"action":"rerequestReview","login":"alice"}))
            .await
            .unwrap(),
        ActionResult::Done
    );
    f.assert_args(
        1,
        &[
            "pr",
            "edit",
            "14",
            "--add-reviewer",
            "alice",
            "--repo",
            "github.example.test/team/repo",
        ],
    );
    for request in [
        json!({"action":"revokeApproval"}),
        json!({"action":"removeOwnChangeRequest"}),
        json!({"action":"applySuggestions","suggestionIds":["1"],"commitMessage":null}),
    ] {
        let f = Fixture::new(false);
        let error = f.run(request.clone()).await.unwrap_err();
        assert_eq!(error.code, "unavailable");
        if request["action"] == "applySuggestions" {
            assert!(error.message.contains("no public API"));
        }
        assert_eq!(f.count(), 0);
    }
}

#[tokio::test]
async fn pull_requests_github_ids_reject_unknown_wrong_kind_and_malformed_before_process() {
    for id in [
        "123",
        "unknown:123:node",
        "ev:node",
        "rv:123:node",
        "ic:1",
        "ic:0:node",
        "ic:1:",
        "ic:1:node:extra",
        "ic:1/../../2:node",
        "nt:discussion:123",
    ] {
        let f = Fixture::new(false);
        let error = f
            .run(json!({"action":"editComment","commentId":id,"body":BODY}))
            .await
            .unwrap_err();
        assert_eq!(error.code, "host_rejected", "{id}");
        assert_eq!(f.count(), 0, "{id}");
    }
}

const GL_MR: &str = "projects/team%2Frepo/merge_requests/14";
impl Fixture {
    fn assert_gl_body(&self, call: usize, method: &str, path: &str, body: Value) {
        let args = self.args(call);
        assert_eq!(&args[..4], &["api", "--method", method, path]);
        assert_eq!(
            &args[4..7],
            &["-H", "Content-Type: application/json", "--input"]
        );
        assert_eq!(&args[8..], &["--hostname", "gitlab.example.test"]);
        assert_eq!(self.json_input(call), body);
        self.assert_body_private(call, BODY);
        assert!(
            fs::read_to_string(self.root.path().join(format!("call-{call}.env")))
                .unwrap()
                .contains("|gitlab.example.test|1")
        );
    }
    fn assert_gl_graphql(&self, call: usize, mutation: &str) -> String {
        let args = self.args(call);
        assert_eq!(&args[..3], &["api", "graphql", "-f"]);
        assert!(args[3].starts_with("query="));
        assert!(args[3].contains(mutation));
        assert!(args[3].contains("projectPath: \"team/repo\""));
        assert!(args[3].contains("iid: \"14\""));
        assert_eq!(&args[4..], &["--hostname", "gitlab.example.test"]);
        args[3].clone()
    }
}

#[tokio::test]
async fn pull_requests_gitlab_comments_reply_edit_and_delete_keep_discussion_note_ids() {
    for (request, method, path, body) in [
        (
            json!({"action":"comment","body":BODY}),
            "POST",
            format!("{GL_MR}/notes"),
            Some(json!({"body":BODY})),
        ),
        (
            json!({"action":"editComment","commentId":"nt:abc123:456","body":BODY}),
            "PUT",
            format!("{GL_MR}/discussions/abc123/notes/456"),
            Some(json!({"body":BODY})),
        ),
        (
            json!({"action":"deleteComment","commentId":"nt:abc123:456"}),
            "DELETE",
            format!("{GL_MR}/discussions/abc123/notes/456"),
            None,
        ),
        (
            json!({"action":"replyThread","threadId":"ds:abc123","body":BODY}),
            "POST",
            format!("{GL_MR}/discussions/abc123/notes"),
            Some(json!({"body":BODY})),
        ),
    ] {
        let mut f = Fixture::new(true);
        f.raw_response("");
        assert_eq!(f.run(request).await.unwrap(), ActionResult::Done);
        if let Some(body) = body {
            f.assert_gl_body(1, method, &path, body);
        } else {
            f.assert_args(
                1,
                &[
                    "api",
                    "--method",
                    method,
                    &path,
                    "--hostname",
                    "gitlab.example.test",
                ],
            );
        }
        assert_eq!(f.count(), 1);
    }
}

#[tokio::test]
async fn pull_requests_gitlab_resolve_is_idempotent_and_uses_resolvable_notes() {
    for current in [true, false] {
        for resolved in [true, false] {
            let mut f = Fixture::new(true);
            f.response(
                json!({"notes":[{"resolvable":true,"resolved":current},{"resolvable":false}]}),
            );
            if current != resolved {
                f.response(json!({}));
            }
            assert_eq!(
                f.run(json!({"action":"resolveThread","threadId":"ds:abc123","resolved":resolved}))
                    .await
                    .unwrap(),
                ActionResult::Done
            );
            f.assert_args(
                1,
                &[
                    "api",
                    &format!("{GL_MR}/discussions/abc123"),
                    "--hostname",
                    "gitlab.example.test",
                ],
            );
            assert_eq!(f.count(), if current == resolved { 1 } else { 2 });
            if current != resolved {
                f.assert_args(
                    2,
                    &[
                        "api",
                        "--method",
                        "PUT",
                        &format!("{GL_MR}/discussions/abc123?resolved={resolved}"),
                        "--hostname",
                        "gitlab.example.test",
                    ],
                );
            }
        }
    }
}

#[tokio::test]
async fn pull_requests_gitlab_reactions_reverse_mapping_and_toggle_viewers_award() {
    for (content, name) in [
        ("+1", "thumbsup"),
        ("-1", "thumbsdown"),
        ("laugh", "laughing"),
        ("confused", "confused"),
        ("heart", "heart"),
        ("hooray", "tada"),
        ("rocket", "rocket"),
        ("eyes", "eyes"),
    ] {
        for target in [Value::Null, json!("nt:abc123:456")] {
            let base = if target.is_null() {
                GL_MR.to_owned()
            } else {
                format!("{GL_MR}/notes/456")
            };
            for (on, existing) in [(true, false), (true, true), (false, true), (false, false)] {
                let mut f = Fixture::new(true);
                f.response(json!({"username":"viewer"}));
                let mut awards =
                    vec![json!({"id":7,"name":name,"user":{"username":"someone-else"}})];
                if existing {
                    awards.push(json!({"id":8,"name":name,"user":{"username":"viewer"}}));
                }
                f.response(json!(awards));
                if on != existing {
                    f.response(json!({}));
                }
                assert_eq!(
                    f.run(json!({"action":"react","targetId":target,"content":content,"on":on}))
                        .await
                        .unwrap(),
                    ActionResult::Done
                );
                f.assert_args(1, &["api", "user", "--hostname", "gitlab.example.test"]);
                f.assert_args(
                    2,
                    &[
                        "api",
                        &format!("{base}/award_emoji?per_page=100&page=1"),
                        "--hostname",
                        "gitlab.example.test",
                    ],
                );
                assert_eq!(f.count(), if on == existing { 2 } else { 3 });
                if on && !existing {
                    f.assert_gl_body(
                        3,
                        "POST",
                        &format!("{base}/award_emoji"),
                        json!({"name":name}),
                    );
                }
                if !on && existing {
                    f.assert_args(
                        3,
                        &[
                            "api",
                            "--method",
                            "DELETE",
                            &format!("{base}/award_emoji/8"),
                            "--hostname",
                            "gitlab.example.test",
                        ],
                    );
                }
            }
        }
    }
}

fn versions() -> Value {
    json!([{"id":42,"base_commit_sha":"base-sha","start_commit_sha":"start-sha","head_commit_sha":"head-sha"}])
}

#[tokio::test]
async fn pull_requests_gitlab_review_positions_fetch_versions_once_then_body_then_event() {
    for event in ["approve", "request_changes", "comment"] {
        let mut f = Fixture::new(true);
        f.response(versions());
        f.response(version_detail());
        f.response(json!({}));
        f.response(json!({}));
        f.response(json!({}));
        if event == "approve" {
            f.raw_response("");
        }
        if event == "request_changes" {
            f.response(json!({"data":{"mergeRequestRequestChanges":{"errors":[]}}}));
        }
        assert_eq!(
            f.run(review(event)).await.unwrap(),
            ActionResult::ReviewSubmitted {
                review_posted: true,
                landed: 2,
                failed: vec![]
            }
        );
        f.assert_args(
            1,
            &[
                "api",
                &format!("{GL_MR}/versions"),
                "--hostname",
                "gitlab.example.test",
            ],
        );
        for (call, path, side, line) in [(3, "old.rs", "old_line", 9), (4, "new.rs", "new_line", 3)]
        {
            // No directly usable SHA-1 dependency: the permitted fallback posts the end line.
            f.assert_gl_body(call,"POST",&format!("{GL_MR}/discussions"),json!({"body":BODY,"position":{"base_sha":"base-sha","start_sha":"start-sha","head_sha":"head-sha","position_type":"text","old_path":path,"new_path":path,side:line}}));
        }
        f.assert_gl_body(5, "POST", &format!("{GL_MR}/notes"), json!({"body":BODY}));
        if event == "approve" {
            f.assert_args(
                6,
                &[
                    "mr",
                    "approve",
                    "14",
                    "--sha",
                    "head-sha",
                    "--repo",
                    "gitlab.example.test/team/repo",
                ],
            );
        }
        if event == "request_changes" {
            f.assert_gl_graphql(6, "mergeRequestRequestChanges");
        }
        assert_eq!(f.count(), if event == "comment" { 5 } else { 6 });
    }
}

#[tokio::test]
async fn pull_requests_gitlab_partial_review_keeps_successful_comments_and_still_approves() {
    let mut f = Fixture::new(true);
    f.response(versions());
    f.response(version_detail());
    f.response(json!({}));
    f.failure("glab: invalid position (HTTP 400)");
    f.response(json!({}));
    f.raw_response("");
    let mut request = review("approve");
    request["body"] = Value::Null;
    request["comments"]
        .as_array_mut()
        .unwrap()
        .push(json!({"path":"third.rs","line":5,"startLine":null,"side":"right","body":BODY}));
    let result = f.run(request).await.unwrap();
    let ActionResult::ReviewSubmitted { landed, failed, .. } = result else {
        panic!("wrong result")
    };
    assert_eq!(landed, 2);
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].path, "new.rs");
    assert_eq!(failed[0].line, 3);
    assert_eq!(failed[0].message, "glab: invalid position (HTTP 400)");
    f.assert_args(
        6,
        &[
            "mr",
            "approve",
            "14",
            "--sha",
            "head-sha",
            "--repo",
            "gitlab.example.test/team/repo",
        ],
    );
    assert_eq!(f.count(), 6);
}

#[tokio::test]
async fn pull_requests_gitlab_event_and_summary_failure_report_landed_count() {
    for summary_failure in [true, false] {
        let mut f = Fixture::new(true);
        f.response(versions());
        f.response(version_detail());
        f.response(json!({}));
        f.response(json!({}));
        if !summary_failure {
            f.response(json!({}));
        }
        f.failure("glab: rejected by project policy (HTTP 400)");
        let error = f.run(review("approve")).await.unwrap_err();
        assert_eq!(error.code, "host_rejected");
        assert!(error.message.contains(if summary_failure {
            "2 comments were posted"
        } else {
            "3 comments were posted"
        }));
        assert!(error.message.contains(if summary_failure {
            "review summary failed"
        } else {
            "approval failed"
        }));
        assert_eq!(
            error.host_detail.as_deref(),
            Some("glab: rejected by project policy (HTTP 400)")
        );
        assert_eq!(f.count(), if summary_failure { 5 } else { 6 });
    }
}

#[tokio::test]
async fn pull_requests_gitlab_versions_head_race_rejects_before_posts() {
    let mut f = Fixture::new(true);
    let mut value = versions();
    value[0]["head_commit_sha"] = json!("new-head");
    f.response(value);
    assert_eq!(
        f.run(review("approve")).await.unwrap_err().code,
        "stale_head"
    );
    assert_eq!(f.count(), 1);
}

#[tokio::test]
async fn pull_requests_gitlab_revoke_remove_request_and_rerequest_resolves_user_first() {
    let mut f = Fixture::new(true);
    f.raw_response("");
    assert_eq!(
        f.run(json!({"action":"revokeApproval"})).await.unwrap(),
        ActionResult::Done
    );
    f.assert_args(
        1,
        &[
            "mr",
            "revoke",
            "14",
            "--repo",
            "gitlab.example.test/team/repo",
        ],
    );
    let mut f = Fixture::new(true);
    f.response(json!({"data":{"mergeRequestDestroyRequestedChanges":{"errors":[]}}}));
    assert_eq!(
        f.run(json!({"action":"removeOwnChangeRequest"}))
            .await
            .unwrap(),
        ActionResult::Done
    );
    f.assert_gl_graphql(1, "mergeRequestDestroyRequestedChanges");
    let mut f = Fixture::new(true);
    f.response(json!([{"id":10,"username":"different"},{"id":42,"username":"alice"}]));
    f.response(json!({"data":{"mergeRequestReviewerRereview":{"errors":[]}}}));
    assert_eq!(
        f.run(json!({"action":"rerequestReview","login":"alice"}))
            .await
            .unwrap(),
        ActionResult::Done
    );
    f.assert_args(
        1,
        &[
            "api",
            "users?username=alice",
            "--hostname",
            "gitlab.example.test",
        ],
    );
    assert!(
        f.assert_gl_graphql(2, "mergeRequestReviewerRereview")
            .contains("userId: \"gid://gitlab/User/42\"")
    );
    assert_eq!(f.count(), 2);
}

#[tokio::test]
async fn pull_requests_gitlab_apply_suggestions_single_and_batch_use_private_bodies() {
    for (ids, path) in [
        (vec!["123"], "suggestions/123/apply"),
        (vec!["123", "456"], "suggestions/batch_apply"),
    ] {
        for message in [Value::Null, json!(BODY)] {
            let mut f = Fixture::new(true);
            f.response(json!({}));
            assert_eq!(
                f.run(
                    json!({"action":"applySuggestions","suggestionIds":ids,"commitMessage":message})
                )
                .await
                .unwrap(),
                ActionResult::Done
            );
            let mut expected = json!({});
            if ids.len() > 1 {
                expected["ids"] = json!([123, 456]);
            }
            if !message.is_null() {
                expected["commit_message"] = message;
            }
            f.assert_gl_body(1, "PUT", path, expected);
            assert_eq!(f.count(), 1);
        }
    }
}

#[tokio::test]
async fn pull_requests_gitlab_rejects_bad_ids_and_unsupported_actions_without_calls() {
    for id in [
        "123",
        "ic:123:node",
        "ap:123",
        "ev:1",
        "nt:abc:0",
        "nt:abc:1:extra",
        "nt::1",
        "nt:abc:1/../../2",
    ] {
        let f = Fixture::new(true);
        let error = f
            .run(json!({"action":"editComment","commentId":id,"body":BODY}))
            .await
            .unwrap_err();
        assert_eq!(error.code, "host_rejected", "{id}");
        assert_eq!(f.count(), 0);
    }
    for request in [
        json!({"action":"minimizeComment","commentId":"nt:abc:1","minimized":true}),
        json!({"action":"dismissReview","reviewId":"ap:1","message":BODY}),
    ] {
        let f = Fixture::new(true);
        let error = f.run(request).await.unwrap_err();
        assert_eq!(error.code, "unavailable");
        assert_eq!(f.count(), 0);
    }
}

#[tokio::test]
async fn pull_requests_rpc_actions_precheck_denial_and_stale_head_have_no_mutation_calls() {
    for gitlab in [false, true] {
        for stale in [false, true] {
            let mut f = Fixture::new(gitlab);
            if gitlab {
                f.gitlab_precheck(stale);
            } else {
                f.github_precheck(!stale);
            }
            let expected_reads = f.responses;
            let mut request = review("approve");
            if stale {
                request["headSha"] = json!("old-head");
            }
            let error = f.rpc(request).await.unwrap_err();
            assert_eq!(
                error["code"],
                if stale { "stale_head" } else { "forbidden" },
                "{error}"
            );
            assert_eq!(error["operation"], "pullRequests.runAction");
            if !stale {
                assert_eq!(
                    error["message"],
                    if gitlab {
                        "GitLab does not allow your account to approve this merge request."
                    } else {
                        "You cannot approve your own pull request."
                    }
                );
            }
            assert_eq!(f.count(), expected_reads);
            for call in 1..=f.count() {
                let args = f.args(call);
                assert!(
                    !args
                        .iter()
                        .any(|arg| arg == "reviews" || arg == "approve" || arg == "/notes")
                );
            }
        }
    }
}

#[tokio::test]
async fn pull_requests_rpc_actions_dispatch_comment_with_fresh_reads_and_no_read_after_success() {
    for gitlab in [false, true] {
        let mut f = Fixture::new(gitlab);
        if gitlab {
            f.gitlab_precheck(true);
        } else {
            f.github_precheck(false);
        }
        f.raw_response("");
        assert_eq!(
            f.rpc(json!({"action":"comment","body":BODY}))
                .await
                .unwrap(),
            json!({"kind":"done"})
        );
        assert_eq!(f.count(), f.responses);
        assert_eq!(
            f.input(f.count()),
            if gitlab {
                json!({"body":BODY}).to_string()
            } else {
                BODY.to_owned()
            }
        );
        f.assert_body_private(f.count(), BODY);
    }
}

#[tokio::test]
async fn pull_requests_rpc_actions_bad_requests_never_spawn() {
    for request in [
        json!({"action":"unknown","body":"private invalid request"}),
        json!({"action":"comment","body":BODY,"number":0}),
        json!({"action":"comment","body":BODY,"number":-1}),
        json!({"action":"comment","body":BODY,"number":1.5}),
        json!({"action":"comment","body":BODY,"number":9007199254740992u64}),
        json!({"action":"comment","body":BODY,"cwd":" "}),
        json!({"action":"submitReview","event":"approve","headSha":" ","body":null,"comments":[]}),
        json!({"action":"submitReview","event":"approve","headSha":"head","body":null,"comments":[{"path":"a.rs","line":0,"startLine":null,"side":"right","body":BODY}]}),
        json!({"action":"submitReview","event":"approve","headSha":"head","body":null,"comments":[{"path":"a.rs","line":1,"startLine":2,"side":"right","body":BODY}]}),
    ] {
        let f = Fixture::new(false);
        let error = f.rpc(request).await.unwrap_err();
        assert_eq!(error["code"], "host_rejected");
        assert_eq!(error["message"], "The Pull Requests request is invalid.");
        assert!(!error.to_string().contains(BODY));
        assert_eq!(f.count(), 0);
    }
}

#[tokio::test]
async fn pull_requests_rpc_actions_host_rejection_preserves_operation_and_host_detail() {
    let mut f = Fixture::new(false);
    f.github_precheck(false);
    f.failure("gh: comments are disabled (HTTP 403)");
    let error = f
        .rpc(json!({"action":"comment","body":BODY}))
        .await
        .unwrap_err();
    assert_eq!(error["operation"], "pullRequests.runAction");
    assert_eq!(error["code"], "forbidden");
    assert_eq!(error["hostDetail"], "gh: comments are disabled (HTTP 403)");
}

#[tokio::test]
async fn pull_requests_github_multiline_422_stderr_still_reports_atomic_comment_failure() {
    let mut f = Fixture::new(false);
    f.failure("gh: Validation Failed (HTTP 422)\nPull request review comment position is invalid");
    let ActionResult::ReviewSubmitted { landed, failed, .. } =
        f.run(review("approve")).await.unwrap()
    else {
        panic!("wrong result")
    };
    assert_eq!(landed, 0);
    assert_eq!(failed.len(), 2);
    assert_eq!(f.count(), 1);
}

#[tokio::test]
async fn pull_requests_graphql_actions_surface_payload_errors_and_redact_credentials() {
    for gitlab in [false, true] {
        for top_level in [false, true] {
            for sentence in [
                "The host refused this action",
                "Permission token glpat-secret must not escape",
            ] {
                let mut f = Fixture::new(gitlab);
                let field = if gitlab {
                    "mergeRequestDestroyRequestedChanges"
                } else {
                    "minimizeComment"
                };
                let response = if top_level {
                    json!({"errors":[{"message":sentence}]})
                } else {
                    json!({"data":{field:{"errors":[sentence]}}})
                };
                f.response(response);
                let error=f.run(if gitlab {json!({"action":"removeOwnChangeRequest"})}else{json!({"action":"minimizeComment","commentId":"ic:1:IC_node","minimized":true})}).await.unwrap_err();
                assert_eq!(error.code, "host_rejected");
                assert_eq!(error.operation, "pullRequests.runAction");
                assert_eq!(
                    error.host_detail,
                    if sentence.contains("token") {
                        None
                    } else {
                        Some(sentence.into())
                    }
                );
                assert!(
                    !serde_json::to_string(&error)
                        .unwrap()
                        .contains("glpat-secret")
                );
            }
        }
    }
}

fn version_detail() -> Value {
    json!({"base_commit_sha":"base-sha","start_commit_sha":"start-sha","head_commit_sha":"head-sha","diffs":[
        {"old_path":"old.rs","new_path":"old.rs","diff":"@@ -9 +9 @@\n-old\n+new\n"},
        {"old_path":"new.rs","new_path":"new.rs","diff":"@@ -3 +3 @@\n-old\n+new\n"},
        {"old_path":"third.rs","new_path":"third.rs","diff":"@@ -5 +5 @@\n-old\n+new\n"}
    ]})
}

#[tokio::test]
async fn pull_requests_gitlab_review_renames_and_context_lines_use_version_coordinates() {
    let mut f = Fixture::new(true);
    f.response(versions());
    let mut detail = version_detail();
    detail["diffs"] = json!([{"old_path":"src/old.rs","new_path":"src/new.rs","diff":"@@ -10,3 +10,4 @@\n context\n+added\n middle\n-old end\n+new end\n"}]);
    f.response(detail);
    for _ in 0..4 {
        f.response(json!({}));
    }
    let comments = json!([
        {"path":"src/new.rs","line":12,"startLine":null,"side":"right","body":BODY},
        {"path":"src/new.rs","line":11,"startLine":null,"side":"left","body":BODY},
        {"path":"src/new.rs","line":11,"startLine":null,"side":"right","body":BODY},
        {"path":"src/new.rs","line":12,"startLine":null,"side":"left","body":BODY}
    ]);
    assert_eq!(f.run(json!({"action":"submitReview","event":"comment","body":null,"headSha":"head-sha","comments":comments})).await.unwrap(),ActionResult::ReviewSubmitted{review_posted:true,landed:4,failed:vec![]});
    f.assert_args(
        2,
        &[
            "api",
            &format!("{GL_MR}/versions/42"),
            "--hostname",
            "gitlab.example.test",
        ],
    );
    for (call, old, new) in [
        (3, Some(11), Some(12)),
        (4, Some(11), Some(12)),
        (5, None, Some(11)),
        (6, Some(12), None),
    ] {
        let position = f.json_input(call)["position"].clone();
        assert_eq!(position["old_path"], "src/old.rs");
        assert_eq!(position["new_path"], "src/new.rs");
        assert_eq!(position.get("old_line").and_then(Value::as_u64), old);
        assert_eq!(position.get("new_line").and_then(Value::as_u64), new);
    }
    assert_eq!(f.count(), 6);
}

#[tokio::test]
async fn pull_requests_gitlab_review_unavailable_position_remains_failed_and_other_comments_land() {
    let mut f = Fixture::new(true);
    f.response(versions());
    f.response(version_detail());
    f.response(json!({}));
    let mut request = review("comment");
    request["body"] = Value::Null;
    request["comments"][0]["line"] = json!(999);
    let ActionResult::ReviewSubmitted { landed, failed, .. } = f.run(request).await.unwrap() else {
        panic!("wrong result")
    };
    assert_eq!(landed, 1);
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].line, 999);
    assert!(failed[0].message.contains("Refresh"));
    assert_eq!(f.count(), 3);
}

#[tokio::test]
async fn pull_requests_review_receipt_identifies_posted_review_and_failed_body() {
    // An atomic GitHub rejection must never discard the unsent summary.
    let mut github = Fixture::new(false);
    github.failure("gh: Pull request review comment position is invalid (HTTP 422)");
    let result = serde_json::to_value(github.run(review("comment")).await.unwrap()).unwrap();
    assert_eq!(result["reviewPosted"], false);
    assert_eq!(result["failed"][0]["body"], BODY);
    // GitLab posts a summary even if every inline position failed.
    let mut gitlab = Fixture::new(true);
    gitlab.response(versions());
    gitlab.response(version_detail());
    gitlab.failure("glab: invalid position (HTTP 400)");
    gitlab.failure("glab: invalid position (HTTP 400)");
    gitlab.response(json!({}));
    let result = serde_json::to_value(gitlab.run(review("comment")).await.unwrap()).unwrap();
    assert_eq!(result["landed"], 0);
    assert_eq!(result["reviewPosted"], true);
    assert_eq!(result["failed"][0]["body"], BODY);
    assert_eq!(gitlab.count(), 5);
}
#[tokio::test]
async fn pull_requests_github_inline_only_comment_supplies_empty_review_body() {
    let mut fixture = Fixture::new(false);
    fixture.response(json!({"id":1}));
    let mut request = review("comment");
    request["body"] = Value::Null;
    let result = serde_json::to_value(fixture.run(request).await.unwrap()).unwrap();
    assert_eq!(result["landed"], 2);
    assert_eq!(result["reviewPosted"], true);
    assert_eq!(fixture.json_input(1)["body"], "");
}
