#![cfg(unix)]
//! Mutations are exercised only against recording CLI stubs, never live hosts.
#[path = "support/pull_request_action_fixture.rs"]
mod pull_request_action_fixture;

use bibcode_server::pull_requests::model::{ActionResult, PullRequestsProvider};
use pull_request_action_fixture::{Fixture, executable_fixture};
use serde_json::{Value, json};
use std::fs;
use tokio_util::sync::CancellationToken;

const BODY: &str = "private edit or merge body\nquotes ' \" ` $() \\ and unicode é";

#[tokio::test]
async fn pull_requests_context_auth_probes_can_follow_all_context_reads() {
    use bibcode_server::pull_requests::{ContextRead, PullRequestsService};

    for gitlab in [false, true] {
        let mut f = Fixture::new(gitlab);
        if gitlab {
            f.gitlab_precheck(true);
        } else {
            f.github_precheck(true);
        }
        // Force the three context processes ahead of both login probes. Arrival
        // order must not change which prepared response each command receives.
        let script = f.root.path().join("cli");
        let source = fs::read_to_string(&script).unwrap();
        executable_fixture::write_executable(
            &script,
            source.replacen(
                "#!/bin/sh\n",
                "#!/bin/sh\nif [ \"$1\" = --version ]; then\n  until [ -f call-4.argv ]; do sleep 0.01; done\nfi\n",
                1,
            ),
        );

        let service = PullRequestsService::with_runner(f.runner.clone());
        let context = service
            .context(f.root.path(), ContextRead::Open, &CancellationToken::new())
            .await
            .unwrap();
        let context = serde_json::to_value(context).unwrap();
        assert_eq!(context["status"], "available", "{context}");
        assert_eq!(
            context["provider"],
            if gitlab { "gitlab" } else { "github" }
        );
        assert_eq!(context["account"]["login"], "viewer");
        assert_eq!(f.count(), 6);
        f.assert_args(5, &["--version"]);
        assert_eq!(&f.args(6)[..2], ["auth", "status"]);
    }
}

#[tokio::test]
async fn pull_requests_gitlab_action_process_counts() {
    for (name, action, extra_reads) in [
        (
            "comment",
            json!({"action":"comment","body":"measurement"}),
            0,
        ),
        (
            "labels",
            json!({"action":"setLabels","add":["bug"],"remove":[]}),
            0,
        ),
        (
            "milestone",
            json!({"action":"setMilestone","milestoneId":"42"}),
            0,
        ),
        (
            "assignee",
            json!({"action":"setAssignees","add":["new-user"],"remove":[]}),
            1,
        ),
    ] {
        let mut f = Fixture::new(true);
        f.gitlab_precheck(true);
        if name == "assignee" {
            f.change_response(7, |v| v["assignees"] = json!([]));
        }
        if extra_reads > 0 {
            f.response(json!([{"id":20,"username":"new-user"}]));
        }
        f.response(json!({}));
        f.rpc(action).await.unwrap();
        eprintln!(
            "GitLab {name}: {} total processes ({} host CLI + 1 git)",
            f.count(),
            f.count() - 1
        );
        assert_eq!(f.count(), 12 + extra_reads);
    }
}

#[tokio::test]
async fn pull_requests_gitlab_unavailable_context_invalidates_successful_auth_probes() {
    use bibcode_server::pull_requests::{
        ContextRead, PullRequestsService,
        model::{Context, UnavailableCode},
    };
    let mut f = Fixture::new(true);
    f.raw_response("https://gitlab.com/team/repo.git");
    f.raw_response("glab version 1.114.0");
    f.role("cli-version");
    f.raw_response("gitlab.com\n  ✓ Logged in to gitlab.com as viewer");
    f.role("auth");
    // The three context reads run together; a revoked token rejects each of them.
    for role in ["user", "project", "version"] {
        f.failure("HTTP 401: authentication required");
        f.role(role);
    }
    let service = PullRequestsService::with_runner(f.runner.clone());
    let c = CancellationToken::new();
    assert!(matches!(
        service
            .context(f.root.path(), ContextRead::Rescan, &c)
            .await
            .unwrap(),
        Context::Unavailable {
            code: UnavailableCode::NotAuthenticated,
            ..
        }
    ));
    let before = f.count();
    f.gitlab_precheck(true);
    f.response(json!({}));
    service
        .run_action(
            &f.request(json!({"action":"setLabels","add":["bug"],"remove":[]})),
            &c,
        )
        .await
        .unwrap();
    assert_eq!(f.count() - before, 12);
    assert_eq!(f.args(before + 2), vec!["--version"]);
    assert_eq!(
        f.args(before + 3),
        vec!["auth", "status", "--hostname", "gitlab.com"]
    );
}

#[tokio::test]
async fn pull_requests_gitlab_warm_actions_reuse_context_but_refresh_permissions_and_ui_reads() {
    use bibcode_server::pull_requests::{ContextRead, PullRequestsService};
    let mut f = Fixture::new(true);
    f.gitlab_precheck(true);
    f.response(json!({}));
    let service = PullRequestsService::with_runner(f.runner.clone());
    let c = CancellationToken::new();
    let action = json!({"action":"setLabels","add":["bug"],"remove":[]});
    service
        .run_action(&f.request(action.clone()), &c)
        .await
        .unwrap();
    assert_eq!(f.count(), 12);
    let observation = |call| {
        serde_json::from_str::<Value>(
            &fs::read_to_string(f.root.path().join(format!("response-{call}"))).unwrap(),
        )
        .unwrap()
    };
    let mr = observation(7);
    let approvals = observation(8);
    let reviewers = observation(9);
    let metadata = observation(10);
    let project = observation(11);
    for (name, request, extra_reads) in [
        (
            "comment",
            json!({"action":"comment","body":"measurement"}),
            0,
        ),
        ("labels", action.clone(), 0),
        (
            "milestone",
            json!({"action":"setMilestone","milestoneId":"42"}),
            0,
        ),
        (
            "assignee",
            json!({"action":"setAssignees","add":["new-user"],"remove":[]}),
            1,
        ),
    ] {
        let before = f.count();
        f.raw_response("https://gitlab.com/team/repo.git");
        let mut fresh_mr = mr.clone();
        if name == "assignee" {
            fresh_mr["assignees"] = json!([]);
        }
        f.gitlab_action_reads([&fresh_mr, &approvals, &reviewers, &metadata, &project]);
        if extra_reads > 0 {
            f.response(json!([{"id":20,"username":"new-user"}]));
        }
        f.response(json!({}));
        service.run_action(&f.request(request), &c).await.unwrap();
        assert_eq!(
            f.count() - before,
            7 + extra_reads,
            "one origin + fresh permissions + mutation/lookup"
        );
        for call in before + 1..=f.count() {
            let args = f.args(call).join(" ");
            assert!(
                !args.contains("auth status")
                    && !args.contains("api user ")
                    && !args.contains("api version")
            );
            assert!(
                !args.contains("approval_state")
                    && !args.contains("award_emoji")
                    && !args.contains("closes_issues")
            );
        }
        eprintln!("GitLab warm {name}: {} total processes", f.count() - before);
    }

    let before = f.count();
    f.raw_response("https://gitlab.com/team/repo.git");
    // Detail reads run together; each reserves its response by argv.
    for (value, role) in [
        (&mr, "merge-request"),
        (&approvals, "approvals"),
        (&reviewers, "reviewers"),
        (&json!({"rules":[]}), "approval-state"),
        (&json!([]), "awards"),
        (&json!([]), "issues"),
        (&metadata, "graphql"),
    ] {
        f.response(value.clone());
        f.role(role);
    }
    service.get(f.root.path(), 14, &c).await.unwrap();
    assert_eq!(f.count() - before, 8);
    eprintln!(
        "GitLab post-action detail: {} total processes",
        f.count() - before
    );

    let before = f.count();
    f.raw_response("https://gitlab.com/team/repo.git");
    // Timeline reads run together too.
    for (value, role) in [
        (&mr, "merge-request"),
        (&approvals, "approvals"),
        (&reviewers, "reviewers"),
        (
            &json!({"data":{"project":{"mergeRequest":{"userPermissions":{"createNote":true},"discussions":{"nodes":[],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}}),
            "graphql",
        ),
        (&json!([]), "label-events"),
        (&json!([]), "milestone-events"),
        (&json!([]), "state-events"),
    ] {
        f.response(value.clone());
        f.role(role);
    }
    service.timeline(f.root.path(), 14, &c).await.unwrap();
    assert_eq!(f.count() - before, 8, "timeline reuses the context viewer");
    eprintln!(
        "GitLab post-action timeline: {} total processes",
        f.count() - before
    );

    // Cached Developer context must not grant a write after project access is revoked.
    let before = f.count();
    f.raw_response("https://gitlab.com/team/repo.git");
    let mut revoked = project.clone();
    revoked["permissions"] = json!({});
    f.gitlab_action_reads([&mr, &approvals, &reviewers, &metadata, &revoked]);
    let error = service
        .run_action(&f.request(action), &c)
        .await
        .unwrap_err();
    assert_eq!(error.code, "forbidden");
    assert_eq!(f.count() - before, 6, "fresh denial starts no write");

    // Permission/auth failures invalidate both context and CLI-probe answers.
    let before = f.count();
    f.gitlab_precheck(true);
    f.response(json!({}));
    service
        .run_action(
            &f.request(json!({"action":"setLabels","add":["bug"],"remove":[]})),
            &c,
        )
        .await
        .unwrap();
    assert_eq!(
        f.count() - before,
        12,
        "a denied cached context must be re-probed"
    );

    // An explicit context request is Rescan and bypasses still-young cache entries.
    let before = f.count();
    f.raw_response("https://gitlab.com/team/repo.git");
    f.raw_response("glab version 1.114.0");
    f.role("cli-version");
    f.raw_response("gitlab.com\n  ✓ Logged in to gitlab.com as viewer");
    f.role("auth");
    f.response(json!({"username":"viewer","name":null}));
    f.role("user");
    f.response(project);
    f.role("project");
    f.response(json!({"version":"19.3.2"}));
    f.role("version");
    service
        .context(f.root.path(), ContextRead::Rescan, &c)
        .await
        .unwrap();
    assert_eq!(
        f.count() - before,
        6,
        "Rescan reads auth, viewer, project and host version again"
    );
}

#[tokio::test]
async fn pull_requests_gitlab_metadata_exact_private_bodies() {
    for (request, body) in [
        (
            json!({"action":"editPullRequest","title":"New title","body":BODY,"baseBranch":"release"}),
            json!({"title":"New title","description":BODY,"target_branch":"release"}),
        ),
        (
            json!({"action":"editPullRequest","body":""}),
            json!({"description":""}),
        ),
        (
            json!({"action":"setLabels","add":["bug","help wanted"],"remove":["old"]}),
            json!({"add_labels":"bug,help wanted","remove_labels":"old"}),
        ),
        (
            json!({"action":"setMilestone","milestoneId":"7"}),
            json!({"milestone_id":7}),
        ),
        (
            json!({"action":"setMilestone","milestoneId":null}),
            json!({"milestone_id":0}),
        ),
        (
            json!({"action":"lock","reason":null}),
            json!({"discussion_locked":true}),
        ),
        (
            json!({"action":"unlock"}),
            json!({"discussion_locked":false}),
        ),
    ] {
        let mut f = Fixture::new(true);
        f.response(json!({}));
        assert_eq!(
            f.run(request.clone()).await.unwrap(),
            ActionResult::Done,
            "{request}"
        );
        f.assert_api(1, "PUT", "projects/team%2Frepo/merge_requests/14");
        assert_eq!(f.json_input(1), body);
        assert_eq!(f.count(), 1);
    }
}

#[tokio::test]
async fn pull_requests_gitlab_state_and_branch_exact_commands() {
    for (request, args, result) in [
        (
            json!({"action":"updateBranch","method":"rebase","skipCi":false}),
            vec!["mr", "rebase", "14"],
            ActionResult::Done,
        ),
        (
            json!({"action":"updateBranch","method":"rebase","skipCi":true}),
            vec!["mr", "rebase", "14", "--skip-ci"],
            ActionResult::Done,
        ),
        (
            json!({"action":"setDraft","draft":false}),
            vec!["mr", "update", "14", "--ready"],
            ActionResult::Done,
        ),
        (
            json!({"action":"setDraft","draft":true}),
            vec!["mr", "update", "14", "--draft"],
            ActionResult::Done,
        ),
        (
            json!({"action":"close"}),
            vec!["mr", "close", "14"],
            ActionResult::Done,
        ),
        (
            json!({"action":"reopen"}),
            vec!["mr", "reopen", "14"],
            ActionResult::Done,
        ),
        (
            json!({"action":"delete"}),
            vec!["mr", "delete", "14"],
            ActionResult::Deleted,
        ),
    ] {
        let mut f = Fixture::new(true);
        f.raw_response("");
        assert_eq!(f.run(request).await.unwrap(), result);
        let mut args = args;
        args.extend(["--repo", "gitlab.example.test/team/repo"]);
        f.assert_args(1, &args);
        assert_eq!(f.count(), 1);
    }
    let mut f = Fixture::new(true);
    f.response(json!({}));
    assert_eq!(
        f.run(json!({"action":"disableAutoMerge"})).await.unwrap(),
        ActionResult::Done
    );
    f.assert_args(
        1,
        &[
            "api",
            "--method",
            "POST",
            "projects/team%2Frepo/merge_requests/14/cancel_merge_when_pipeline_succeeds",
            "--hostname",
            "gitlab.example.test",
        ],
    );
}

#[tokio::test]
async fn pull_requests_gitlab_merge_plain_pins_sha_and_method_flags() {
    for method in ["merge", "squash", "rebase"] {
        for auto in [false, true] {
            let mut f = Fixture::new(true);
            f.raw_response("Merge accepted");
            let mut request = merge_request(method);
            request["auto"] = json!(auto);
            request["deleteBranch"] = json!(true);
            assert_eq!(
                f.run(request).await.unwrap(),
                ActionResult::Merged {
                    merged_sha: None,
                    auto_merge_enabled: auto
                }
            );
            let mut args = vec!["mr", "merge", "14", "--sha", "head-sha", "-y"];
            if method == "squash" {
                args.push("--squash");
            }
            args.push("--remove-source-branch");
            args.push(if auto {
                "--auto-merge"
            } else {
                "--auto-merge=false"
            });
            if method == "rebase" {
                args.push("--rebase");
            }
            args.extend(["--repo", "gitlab.example.test/team/repo"]);
            f.assert_args(1, &args);
            assert_eq!(f.count(), 1);
        }
    }
}

#[tokio::test]
async fn pull_requests_gitlab_merge_messages_use_rest_and_return_merge_sha() {
    for (subject, body, message) in [
        (Some("Subject"), Some(BODY), format!("Subject\n\n{BODY}")),
        (Some("Subject"), None, "Subject".into()),
        (None, Some(BODY), BODY.into()),
        (None, Some(""), "".into()),
    ] {
        for method in ["merge", "squash", "rebase"] {
            for auto in [false, true] {
                let mut f = Fixture::new(true);
                f.response(json!({"merge_commit_sha":"merged-sha"}));
                let mut request = merge_request(method);
                request["subject"] = json!(subject);
                request["body"] = json!(body);
                request["auto"] = json!(auto);
                request["deleteBranch"] = json!(true);
                assert_eq!(
                    f.run(request).await.unwrap(),
                    ActionResult::Merged {
                        merged_sha: Some("merged-sha".into()),
                        auto_merge_enabled: auto
                    }
                );
                f.assert_api(1, "PUT", "projects/team%2Frepo/merge_requests/14/merge");
                assert_eq!(
                    f.json_input(1),
                    json!({"sha":"head-sha","squash":method=="squash","should_remove_source_branch":true,"auto_merge":auto,"merge_when_pipeline_succeeds":auto,"merge_commit_message":message,"squash_commit_message":message})
                );
                assert_eq!(f.count(), 1);
            }
        }
    }
}

#[tokio::test]
async fn pull_requests_gitlab_merge_auto_and_bypass_remain_supported() {
    for with_message in [false, true] {
        let mut f = Fixture::new(true);
        f.response(json!({"data":{"mergeRequestUpdate":{"errors":[]}}}));
        if with_message {
            f.response(json!({"merge_commit_sha":null}));
        } else {
            f.raw_response("");
        }
        let mut request = merge_request("squash");
        request["auto"] = json!(true);
        request["bypass"] = json!(true);
        if with_message {
            request["body"] = json!(BODY);
        }
        assert_eq!(
            f.run(request).await.unwrap(),
            ActionResult::Merged {
                merged_sha: None,
                auto_merge_enabled: true
            }
        );
        f.assert_args(1, &["api","graphql","-f", "query=mutation PullRequestsOverrideRequestedChanges { mergeRequestUpdate(input: { projectPath: \"team/repo\", iid: \"14\", overrideRequestedChanges: true }) { errors } }", "--hostname","gitlab.example.test"]);
        if with_message {
            f.assert_api(2, "PUT", "projects/team%2Frepo/merge_requests/14/merge");
            assert_eq!(
                f.json_input(2),
                json!({"sha":"head-sha","squash":true,"should_remove_source_branch":false,"auto_merge":true,"merge_when_pipeline_succeeds":true,"merge_commit_message":BODY,"squash_commit_message":BODY})
            );
        } else {
            f.assert_args(
                2,
                &[
                    "mr",
                    "merge",
                    "14",
                    "--sha",
                    "head-sha",
                    "-y",
                    "--squash",
                    "--auto-merge",
                    "--repo",
                    "gitlab.example.test/team/repo",
                ],
            );
        }
        assert_eq!(f.count(), 2);
    }
}

#[tokio::test]
async fn pull_requests_gitlab_bypass_graphql_precedes_merge_and_errors_stop_it() {
    for rejected in [false, true] {
        let mut f = Fixture::new(true);
        f.response(json!({"data":{"mergeRequestUpdate":{"errors":if rejected { vec!["Cannot override"] } else { vec![] }}}}));
        if !rejected {
            f.raw_response("");
        }
        let mut request = merge_request("merge");
        request["bypass"] = json!(true);
        let result = f.run(request).await;
        f.assert_args(1, &["api","graphql","-f", "query=mutation PullRequestsOverrideRequestedChanges { mergeRequestUpdate(input: { projectPath: \"team/repo\", iid: \"14\", overrideRequestedChanges: true }) { errors } }", "--hostname","gitlab.example.test"]);
        if rejected {
            assert_eq!(result.unwrap_err().code, "host_rejected");
            assert_eq!(f.count(), 1);
        } else {
            assert_eq!(
                result.unwrap(),
                ActionResult::Merged {
                    merged_sha: None,
                    auto_merge_enabled: false
                }
            );
            f.assert_args(
                2,
                &[
                    "mr",
                    "merge",
                    "14",
                    "--sha",
                    "head-sha",
                    "-y",
                    "--auto-merge=false",
                    "--repo",
                    "gitlab.example.test/team/repo",
                ],
            );
            assert_eq!(f.count(), 2);
        }
    }
}

#[tokio::test]
async fn pull_requests_gitlab_reviewers_and_assignees_use_fresh_full_lists_and_deduplicate_lookups()
{
    for reviewers in [true, false] {
        let mut f = Fixture::new(true);
        f.gitlab_precheck(true);
        f.response(json!([{"id":8,"username":"viewer"}]));
        f.response(json!([{"id":20,"username":"new/user"}]));
        f.response(json!({}));
        let result = f.rpc(json!({"action":if reviewers { "setReviewers" } else { "setAssignees" }, "add":["viewer","new/user","new/user"], "remove":[if reviewers {"reviewer"} else {"author"}]})).await.unwrap();
        assert_eq!(result, json!({"kind":"done"}));
        f.assert_args(
            12,
            &["api", "users?username=viewer", "--hostname", "gitlab.com"],
        );
        f.assert_args(
            13,
            &[
                "api",
                "users?username=new%2Fuser",
                "--hostname",
                "gitlab.com",
            ],
        );
        let body = f.json_input(14);
        assert_eq!(
            body,
            if reviewers {
                json!({"reviewer_ids":[8,20]})
            } else {
                json!({"assignee_ids":[8,20]})
            }
        );
        f.assert_api_host(
            14,
            "PUT",
            "projects/team%2Frepo/merge_requests/14",
            "gitlab.com",
        );
        assert_eq!(f.count(), 14);
    }
}

#[tokio::test]
async fn pull_requests_gitlab_revert_creates_branch_then_commit_then_merge_request() {
    let mut f = Fixture::new(true);
    f.gitlab_precheck(true);
    f.change_response(7, |v| {
        v["state"] = json!("merged");
        v["merge_commit_sha"] = json!("abcdef0123456789");
    });
    f.response(json!({"name":"revert-14-abcdef01"}));
    f.response(json!({"id":"reverted-sha"}));
    f.response(json!({"iid":29,"title":"Revert \"CLI update\"","web_url":"https://gitlab.com/team/repo/-/merge_requests/29","source_branch":"revert-14-abcdef01","target_branch":"main","state":"opened"}));
    assert_eq!(
        f.rpc(json!({"action":"revert"})).await.unwrap(),
        json!({"kind":"pullRequestCreated","number":29,"url":"https://gitlab.com/team/repo/-/merge_requests/29"})
    );
    for (call, path, body) in [
        (
            12,
            "projects/team%2Frepo/repository/branches",
            json!({"branch":"revert-14-abcdef01","ref":"main"}),
        ),
        (
            13,
            "projects/team%2Frepo/repository/commits/abcdef0123456789/revert",
            json!({"branch":"revert-14-abcdef01"}),
        ),
        (
            14,
            "projects/team%2Frepo/merge_requests",
            json!({"source_branch":"revert-14-abcdef01","target_branch":"main","title":"Revert \"CLI update\"","description":"Reverts https://gitlab.com/gitlab-org/cli/-/merge_requests/3941"}),
        ),
    ] {
        let args = f.args(call);
        assert_eq!(&args[..4], ["api", "--method", "POST", path]);
        assert_eq!(
            &args[4..7],
            ["-H", "Content-Type: application/json", "--input"]
        );
        assert_eq!(&args[8..], ["--hostname", "gitlab.com"]);
        assert_eq!(f.json_input(call), body);
        f.assert_body_private(call, BODY);
    }
    assert_eq!(f.count(), 14);
}

#[tokio::test]
async fn pull_requests_phase07_rpc_github_dispatches_after_fresh_precheck() {
    let mut f = Fixture::new(false);
    f.github_edit_precheck();
    f.raw_response("");
    assert_eq!(
        f.rpc(json!({"action":"close"})).await.unwrap(),
        json!({"kind":"done"})
    );
    f.assert_args(9, &["pr", "close", "14", "--repo", "github.com/team/repo"]);
    assert_eq!(f.count(), 9);
}

#[tokio::test]
async fn pull_requests_github_metadata_and_state_exact_commands() {
    for (request, args, input) in [
        (
            json!({"action":"editPullRequest","title":"Title with spaces","body":BODY,"baseBranch":"release"}),
            vec![
                "pr",
                "edit",
                "14",
                "--title",
                "Title with spaces",
                "--body-file",
                "-",
                "--base",
                "release",
            ],
            Some(BODY),
        ),
        (
            json!({"action":"editPullRequest","body":""}),
            vec!["pr", "edit", "14", "--body-file", "-"],
            Some(""),
        ),
        (
            json!({"action":"setReviewers","add":["a","b"],"remove":["c"]}),
            vec![
                "pr",
                "edit",
                "14",
                "--add-reviewer",
                "a,b",
                "--remove-reviewer",
                "c",
            ],
            None,
        ),
        (
            json!({"action":"setAssignees","add":["@me"],"remove":["c"]}),
            vec![
                "pr",
                "edit",
                "14",
                "--add-assignee",
                "@me",
                "--remove-assignee",
                "c",
            ],
            None,
        ),
        (
            json!({"action":"setLabels","add":["bug","help wanted"],"remove":["old"]}),
            vec![
                "pr",
                "edit",
                "14",
                "--add-label",
                "bug,help wanted",
                "--remove-label",
                "old",
            ],
            None,
        ),
        (
            json!({"action":"setMilestone","milestoneId":null}),
            vec!["pr", "edit", "14", "--remove-milestone"],
            None,
        ),
        (
            json!({"action":"lock","reason":"off_topic"}),
            vec!["pr", "lock", "14", "--reason", "off_topic"],
            None,
        ),
        (
            json!({"action":"lock","reason":null}),
            vec!["pr", "lock", "14"],
            None,
        ),
        (json!({"action":"unlock"}), vec!["pr", "unlock", "14"], None),
        (
            json!({"action":"updateBranch","method":"merge","skipCi":true}),
            vec!["pr", "update-branch", "14"],
            None,
        ),
        (
            json!({"action":"updateBranch","method":"rebase","skipCi":false}),
            vec!["pr", "update-branch", "14", "--rebase"],
            None,
        ),
        (
            json!({"action":"disableAutoMerge"}),
            vec!["pr", "merge", "14", "--disable-auto"],
            None,
        ),
        (
            json!({"action":"setDraft","draft":false}),
            vec!["pr", "ready", "14"],
            None,
        ),
        (
            json!({"action":"setDraft","draft":true}),
            vec!["pr", "ready", "14", "--undo"],
            None,
        ),
        (json!({"action":"close"}), vec!["pr", "close", "14"], None),
        (json!({"action":"reopen"}), vec!["pr", "reopen", "14"], None),
    ] {
        let mut f = Fixture::new(false);
        f.raw_response("");
        assert_eq!(
            f.run(request.clone()).await.unwrap(),
            ActionResult::Done,
            "{request}"
        );
        let mut args = args;
        args.extend(["--repo", "github.example.test/team/repo"]);
        f.assert_args(1, &args);
        assert_eq!(f.input(1), input.unwrap_or_default());
        f.assert_body_private(1, BODY);
        assert_eq!(f.count(), 1);
    }
}

#[tokio::test]
async fn pull_requests_github_milestone_number_resolves_title_before_edit() {
    let mut f = Fixture::new(false);
    f.response(json!({"number":7,"title":"Sprint seven"}));
    f.raw_response("");
    assert_eq!(
        f.run(json!({"action":"setMilestone","milestoneId":"7"}))
            .await
            .unwrap(),
        ActionResult::Done
    );
    f.assert_args(1, &["api", "repos/team/repo/milestones/7"]);
    f.assert_args(
        2,
        &[
            "pr",
            "edit",
            "14",
            "--milestone",
            "Sprint seven",
            "--repo",
            "github.example.test/team/repo",
        ],
    );
    assert_eq!(f.count(), 2);
}

fn merge_request(method: &str) -> Value {
    json!({"action":"merge","method":method,"headSha":"head-sha","deleteBranch":false,
        "auto":false,"bypass":false,"subject":null,"body":null})
}

#[tokio::test]
async fn pull_requests_github_merge_pins_head_and_transports_body_on_stdin() {
    for method in ["merge", "squash", "rebase"] {
        let mut f = Fixture::new(false);
        f.raw_response("");
        let mut request = merge_request(method);
        request["subject"] = json!("Merge subject");
        request["body"] = json!(BODY);
        request["deleteBranch"] = json!(true);
        assert_eq!(
            f.run(request).await.unwrap(),
            ActionResult::Merged {
                merged_sha: None,
                auto_merge_enabled: false
            }
        );
        f.assert_args(
            1,
            &[
                "pr",
                "merge",
                "14",
                &format!("--{method}"),
                "--match-head-commit",
                "head-sha",
                "--delete-branch",
                "--subject",
                "Merge subject",
                "--body-file",
                "-",
                "--repo",
                "github.example.test/team/repo",
            ],
        );
        assert_eq!(f.input(1), BODY);
        f.assert_body_private(1, BODY);
        assert_eq!(f.count(), 1);
    }
}

#[tokio::test]
async fn pull_requests_github_merge_auto_and_bypass_flags_and_result() {
    for auto in [false, true] {
        for bypass in [false, true] {
            let mut f = Fixture::new(false);
            f.raw_response("");
            let mut request = merge_request("squash");
            request["auto"] = json!(auto);
            request["bypass"] = json!(bypass);
            let result = f.run(request).await;
            if auto && bypass {
                let error = result.unwrap_err();
                assert_eq!(
                    serde_json::to_value(error).unwrap(),
                    json!({
                        "_tag":"PullRequestsOperationError",
                        "operation":"pullRequests.runAction",
                        "code":"invalid_request",
                        "message":"Auto-merge and bypass cannot be combined on GitHub.",
                        "hostDetail":null,
                        "retryable":false,
                    })
                );
                assert_eq!(f.count(), 0);
                continue;
            }
            assert_eq!(
                result.unwrap(),
                ActionResult::Merged {
                    merged_sha: None,
                    auto_merge_enabled: auto
                }
            );
            let mut args = vec![
                "pr",
                "merge",
                "14",
                "--squash",
                "--match-head-commit",
                "head-sha",
            ];
            if auto {
                args.push("--auto");
            }
            if bypass {
                args.push("--admin");
            }
            args.extend(["--repo", "github.example.test/team/repo"]);
            f.assert_args(1, &args);
            assert_eq!(f.count(), 1);
        }
    }
}

#[tokio::test]
async fn pull_requests_github_revert_returns_created_request() {
    let mut f = Fixture::new(false);
    f.raw_response("Creating revert pull request\nhttps://github.example.test/team/repo/pull/29\n");
    assert_eq!(
        f.run(json!({"action":"revert"})).await.unwrap(),
        ActionResult::PullRequestCreated {
            number: 29,
            url: "https://github.example.test/team/repo/pull/29".into()
        }
    );
    f.assert_args(
        1,
        &[
            "pr",
            "revert",
            "14",
            "--repo",
            "github.example.test/team/repo",
        ],
    );
    assert_eq!(f.count(), 1);
}

#[tokio::test]
async fn pull_requests_github_delete_is_unavailable_without_process() {
    let f = Fixture::new(false);
    let error = f.run(json!({"action":"delete"})).await.unwrap_err();
    assert_eq!(error.code, "unavailable");
    assert_eq!(f.count(), 0);
}

#[tokio::test]
async fn pull_requests_github_empty_edits_do_not_start_interactive_cli() {
    for request in [
        json!({"action":"editPullRequest"}),
        json!({"action":"setReviewers","add":[],"remove":[]}),
        json!({"action":"setAssignees","add":[],"remove":[]}),
        json!({"action":"setLabels","add":[],"remove":[]}),
    ] {
        let f = Fixture::new(false);
        assert_eq!(f.run(request).await.unwrap(), ActionResult::Done);
        assert_eq!(f.count(), 0);
    }
}

#[tokio::test]
async fn pull_requests_github_milestone_lookup_and_revert_parse_failures_are_typed() {
    let mut f = Fixture::new(false);
    f.failure("HTTP 404: Not Found");
    assert_eq!(
        f.run(json!({"action":"setMilestone","milestoneId":"7"}))
            .await
            .unwrap_err()
            .code,
        "not_found"
    );
    assert_eq!(f.count(), 1);
    let mut f = Fixture::new(false);
    f.raw_response("not a pull request URL");
    assert_eq!(
        f.run(json!({"action":"revert"})).await.unwrap_err().code,
        "invalid_response"
    );
    assert_eq!(f.count(), 1);
}

impl Fixture {
    /// Edits need update rights on the pull request.
    fn github_edit_precheck(&mut self) {
        self.github_precheck(false);
        self.change_response(8, |metadata| {
            metadata["data"]["repository"]["pullRequest"]["viewerCanUpdate"] = json!(true);
            metadata["data"]["repository"]["viewerPermission"] = json!("ADMIN");
        });
    }
    fn assert_api(&self, call: usize, method: &str, path: &str) {
        self.assert_api_host(call, method, path, &self.scope.host);
    }
    fn assert_api_host(&self, call: usize, method: &str, path: &str, host: &str) {
        let actual = self.args(call);
        assert_eq!(&actual[..4], ["api", "--method", method, path]);
        assert_eq!(
            &actual[4..7],
            ["-H", "Content-Type: application/json", "--input"]
        );
        assert_eq!(&actual[8..], ["--hostname", host]);
        self.assert_body_private(call, BODY);
    }
    fn change_response(&self, call: usize, change: impl FnOnce(&mut Value)) {
        let path = self.root.path().join(format!("response-{call}"));
        let mut value: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        change(&mut value);
        fs::write(path, value.to_string()).unwrap();
    }
}

impl Fixture {
    fn merge_precheck(&mut self) {
        if self.scope.provider == PullRequestsProvider::Gitlab {
            self.gitlab_precheck(true);
            self.change_response(7, |v| {
                v["user"]["can_merge"] = json!(true);
                v["detailed_merge_status"] = json!("mergeable");
            });
        } else {
            self.github_edit_precheck();
            self.change_response(7, |v| {
                v["mergeStateStatus"] = json!("CLEAN");
                v["mergeable"] = json!("MERGEABLE");
            });
        }
    }
}

#[tokio::test]
async fn pull_requests_rpc_merge_guard_failures_start_no_host_mutation() {
    for gitlab in [false, true] {
        for failure in ["method", "stale", "bypass", "permission", "auto"] {
            let mut f = Fixture::new(gitlab);
            f.merge_precheck();
            let mut request = merge_request("squash");
            match failure {
                "method" => f.change_response(if gitlab { 11 } else { 5 }, |v| {
                    v[if gitlab {
                        "squash_option"
                    } else {
                        "allow_squash_merge"
                    }] = if gitlab { json!("never") } else { json!(false) };
                }),
                "stale" => {
                    request["headSha"] = json!("old-head");
                    request["bypass"] = json!(true);
                }
                "bypass" => request["bypass"] = json!(true),
                "permission" if gitlab => {
                    f.change_response(7, |v| v["user"]["can_merge"] = json!(false))
                }
                "permission" => {
                    f.change_response(5, |v| v["permissions"] = json!({"pull":true}));
                    for call in [6, 8] {
                        f.change_response(call, |v| {
                            v["data"]["repository"]["viewerPermission"] = json!("READ")
                        });
                    }
                }
                "auto" => {
                    request["auto"] = json!(true);
                    if !gitlab {
                        f.change_response(8, |v| {
                            v["data"]["repository"]["pullRequest"]["viewerCanEnableAutoMerge"] =
                                json!(false)
                        });
                    }
                }
                _ => unreachable!(),
            }
            let error = f.rpc(request).await.unwrap_err();
            assert_eq!(
                error["code"],
                if failure == "stale" {
                    "stale_head"
                } else {
                    "forbidden"
                },
                "{gitlab} {failure}: {error}"
            );
            if failure == "method" {
                assert_eq!(
                    error["message"],
                    "Squash merging is not allowed in this repository"
                );
            }
            assert_eq!(f.count(), if gitlab { 11 } else { 8 }, "{failure}");
        }
    }
}

#[tokio::test]
async fn pull_requests_rpc_merge_results_and_special_paths() {
    for gitlab in [false, true] {
        for special in ["plain", "auto", "bypass"] {
            let mut f = Fixture::new(gitlab);
            f.merge_precheck();
            let mut request = merge_request("squash");
            request["auto"] = json!(special == "auto");
            request["bypass"] = json!(special == "bypass");
            if special != "plain" {
                if gitlab {
                    f.change_response(7, |v| {
                        v["detailed_merge_status"] = json!(if special == "auto" {
                            "ci_still_running"
                        } else {
                            "requested_changes"
                        });
                        v["head_pipeline"]["status"] = json!("running");
                    });
                } else {
                    f.change_response(7, |v| v["mergeStateStatus"] = json!("BLOCKED"));
                    f.change_response(8, |v| {
                        let pr = &mut v["data"]["repository"]["pullRequest"];
                        pr["viewerCanMergeAsAdmin"] = json!(special == "bypass");
                        pr["viewerCanEnableAutoMerge"] = json!(special == "auto");
                    });
                }
            }
            if gitlab && special == "bypass" {
                f.response(json!({"data":{"mergeRequestUpdate":{"errors":[]}}}));
            }
            f.raw_response("");
            assert_eq!(
                f.rpc(request).await.unwrap(),
                json!({"kind":"merged","mergedSha":null,"autoMergeEnabled":special=="auto"})
            );
            assert_eq!(
                f.count(),
                if gitlab {
                    12 + usize::from(special == "bypass")
                } else {
                    9
                }
            );
        }
    }
}

#[tokio::test]
async fn pull_requests_rpc_merge_empty_or_missing_head_never_spawns() {
    for head in [Some(""), Some("   "), None] {
        let f = Fixture::new(false);
        let mut request = merge_request("squash");
        if let Some(head) = head {
            request["headSha"] = json!(head);
        } else {
            request.as_object_mut().unwrap().remove("headSha");
        }
        assert_eq!(f.rpc(request).await.unwrap_err()["code"], "host_rejected");
        assert_eq!(f.count(), 0);
    }
}

#[tokio::test]
async fn pull_requests_gitlab_remove_all_people_and_lookup_failure_never_overwrite_with_partial_ids()
 {
    for reviewers in [false, true] {
        let action = if reviewers {
            "setReviewers"
        } else {
            "setAssignees"
        };
        let mut f = Fixture::new(true);
        f.gitlab_precheck(true);
        f.response(json!({}));
        assert_eq!(
            f.rpc(json!({"action":action,"add":[],"remove":["author","viewer","reviewer"]}))
                .await
                .unwrap(),
            json!({"kind":"done"})
        );
        assert_eq!(
            f.json_input(12),
            if reviewers {
                json!({"reviewer_ids":[]})
            } else {
                json!({"assignee_ids":[]})
            }
        );
        assert_eq!(f.count(), 12);
        let mut f = Fixture::new(true);
        f.gitlab_precheck(true);
        f.response(json!([{"username":"someone-else","id":100}]));
        assert_eq!(
            f.rpc(json!({"action":action,"add":["new"],"remove":[]}))
                .await
                .unwrap_err()["code"],
            "not_found"
        );
        assert_eq!(f.count(), 12);
    }
}

#[tokio::test]
async fn pull_requests_gitlab_revert_stops_at_each_failure_with_partial_progress_and_typed_error() {
    for fail_at in 1..=3 {
        let mut f = Fixture::new(true);
        f.gitlab_precheck(true);
        f.change_response(7, |v| {
            v["state"] = json!("merged");
            v["merge_commit_sha"] = json!("abcdef0123456789");
        });
        for _ in 1..fail_at {
            f.response(json!({}));
        }
        f.failure("HTTP 403: insufficient permissions");
        let error = f.rpc(json!({"action":"revert"})).await.unwrap_err();
        assert_eq!(error["code"], "forbidden", "step {fail_at}: {error}");
        assert_eq!(error["hostDetail"], "HTTP 403: insufficient permissions");
        if fail_at > 1 {
            assert!(
                error["message"]
                    .as_str()
                    .unwrap()
                    .contains(if fail_at == 2 {
                        "branch was created"
                    } else {
                        "branch and commit were created"
                    })
            );
            assert!(error["message"].as_str().unwrap().contains("Refresh"));
            assert_eq!(error["retryable"], false);
        }
        assert_eq!(f.count(), 11 + fail_at);
        for call in 12..=11 + fail_at {
            f.assert_body_private(call, BODY);
        }
    }
}

#[tokio::test]
async fn pull_requests_gitlab_bypass_then_failed_merge_reports_applied_override() {
    let mut f = Fixture::new(true);
    f.response(json!({"data":{"mergeRequestUpdate":{"errors":[]}}}));
    f.failure("HTTP 409: SHA does not match");
    let mut request = merge_request("squash");
    request["bypass"] = json!(true);
    request["body"] = json!(BODY);
    let error = f.run(request).await.unwrap_err();
    assert_eq!(error.code, "stale_head");
    assert!(error.message.contains("Requested changes were overridden"));
    assert!(!error.retryable);
    assert_eq!(f.count(), 2);
    f.assert_body_private(2, BODY);
}

#[tokio::test]
async fn pull_requests_malformed_milestone_ids_never_reach_host() {
    for gitlab in [false, true] {
        for id in ["0", "title", "1/../../issues", "-1"] {
            let f = Fixture::new(gitlab);
            assert_eq!(
                f.run(json!({"action":"setMilestone","milestoneId":id}))
                    .await
                    .unwrap_err()
                    .code,
                "host_rejected"
            );
            assert_eq!(f.count(), 0);
        }
    }
}

#[tokio::test]
async fn pull_requests_gitlab_revert_uses_known_squash_commit_when_no_merge_commit() {
    let mut f = Fixture::new(true);
    f.gitlab_precheck(true);
    f.change_response(7, |v| {
        v["state"] = json!("merged");
        v["squash_commit_sha"] = json!("12345678abcdef01");
    });
    f.response(json!({}));
    f.response(json!({}));
    f.response(json!({"iid":30,"title":"Revert","web_url":"https://gitlab.com/team/repo/-/merge_requests/30","source_branch":"revert-14-12345678","target_branch":"main","state":"opened"}));
    assert_eq!(
        f.rpc(json!({"action":"revert"})).await.unwrap()["number"],
        30
    );
    assert_eq!(
        f.json_input(12),
        json!({"branch":"revert-14-12345678","ref":"main"})
    );
    assert_eq!(
        f.args(13)[3],
        "projects/team%2Frepo/repository/commits/12345678abcdef01/revert"
    );
    assert_eq!(
        fs::read_to_string(f.root.path().join("call-14.env")).unwrap(),
        "|gitlab.com|1"
    );
    assert_eq!(f.count(), 14);
}
