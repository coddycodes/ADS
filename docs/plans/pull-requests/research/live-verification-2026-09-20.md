# Live verification addendum — 2026-09-20

Facts checked against real endpoints and the CLIs installed on the
requester's Linux workstation, to settle items the web research could not
confirm from documentation alone (`github-gitlab-web-specs.md`, § "Summary of
explicitly unconfirmed items").

## Installed tooling

Phase 01 implementation verification correction: the installed `glab 1.114.0`
rejects `glab mr list --state opened` with `Unknown flag: --state.` Its actual
help states that Open is the default; the state selectors are `--all`,
`--closed`, and `--merged`. The `--state` entry in the recorded inventory below
is inaccurate. Phase 01 omits a state flag for Open and records the command
output in `tasks.md`.

| Tool   | Version             | Auth state on this machine                                                            |
| ------ | ------------------- | ------------------------------------------------------------------------------------- |
| `gh`   | 2.97.0 (2026-07-31) | Logged in to github.com as `mubeda` (keyring, https)                                  |
| `glab` | 1.114.0             | Installed, **not authenticated** (gitlab.com returns 401); no company host configured |

Consequence: GitLab behaviour can be unit-tested here only through fixture
scripts. End-to-end GitLab validation needs the requester's company server.

## GitHub

- `gh pr list --json` / `gh pr view --json` accept exactly these fields:
  `additions assignees author autoMergeRequest baseRefName baseRefOid body
  changedFiles closed closedAt closingIssuesReferences comments commits
  createdAt deletions files fullDatabaseId headRefName headRefOid
  headRepository headRepositoryOwner id isCrossRepository isDraft labels
  latestReviews maintainerCanModify mergeCommit mergeStateStatus mergeable
  mergedAt mergedBy milestone number potentialMergeCommit projectCards
  projectItems reactionGroups reviewDecision reviewRequests reviews state
  statusCheckRollup title updatedAt url`. Requesting `comments` in a list
  returns full comment bodies for every row; list reads must not ask for it.
- A merged PR reports `mergeable: "UNKNOWN"` and `mergeStateStatus: "UNKNOWN"`;
  `reviewDecision` is `""` in `gh` JSON when GraphQL returns `null`.
- `gh pr view --json files` yields `[{path, additions, deletions, changeType}]`;
  `gh pr diff --name-only` yields paths; `gh pr checks --json
  name,state,bucket,workflow,link,startedAt,completedAt` works.
- REST `GET repos/{owner}/{repo}` returns `permissions {admin, maintain, pull,
  push, triage}`, `allow_merge_commit`, `allow_squash_merge`,
  `allow_rebase_merge`, `allow_auto_merge`, `delete_branch_on_merge`,
  `default_branch`.
- REST `GET repos/{owner}/{repo}/pulls/{n}/comments` returns line comments
  with `path`, `line`, `side`, `in_reply_to_id`, `pull_request_review_id`.
- GraphQL fields confirmed to exist (queried successfully):
  `repository { viewerPermission viewerCanAdminister mergeCommitAllowed
  squashMergeAllowed rebaseMergeAllowed autoMergeAllowed deleteBranchOnMerge }`,
  `pullRequest { isDraft mergeable mergeStateStatus reviewDecision
  viewerCanUpdate viewerDidAuthor viewerCanMergeAsAdmin
  viewerCanEnableAutoMerge viewerCanDisableAutoMerge viewerCanApplySuggestion
  viewerCanEditFiles viewerCanDeleteHeadRef viewerLatestReview { state } locked
  headRepositoryOwner { login } baseRef { refUpdateRule {
  requiredApprovingReviewCount requiredStatusCheckContexts
  requiresCodeOwnerReviews viewerCanPush viewerAllowedToDismissReviews }
  branchProtectionRule { requiredApprovingReviewCount requiresStatusChecks
  requiredStatusCheckContexts } } reviewThreads(first) { totalCount nodes {
  isResolved isOutdated path line viewerCanResolve comments { nodes { author {
  login } body } } } } }`, and
  `repository.pullRequests(first, after, states, orderBy) { totalCount
  pageInfo { hasNextPage endCursor } nodes { … } }` with cursor paging.
- GraphQL fields confirmed **not** to exist: `PullRequest.viewerMergeHeadRefPermissions`,
  `RefUpdateRule.requiresStatusChecks`.
- On a repository where the viewer has no write access (`openai/codex`):
  `viewerPermission: "READ"`, `viewerCanUpdate: false`,
  `viewerCanMergeAsAdmin: false`, `refUpdateRule: null`,
  `branchProtectionRule: null`. On the requester's own repository:
  `viewerPermission: "ADMIN"`, `viewerCanUpdate: true`,
  `viewerCanMergeAsAdmin: true`.
- `gh pr merge` flags on the installed version: `--merge --squash --rebase
  --delete-branch --auto --disable-auto --admin --match-head-commit SHA
  --subject --body --body-file --author-email`. `gh pr review`: `--approve
  --comment --request-changes --body --body-file`. `gh pr checkout`:
  `--branch --detach --force --recurse-submodules`.

## GitLab (gitlab.com public project `gitlab-org/cli`, unauthenticated)

- `GET /projects/:id/merge_requests/:iid` includes `user: { can_merge }`,
  `detailed_merge_status`, `merge_status` (deprecated), `has_conflicts`,
  `blocking_discussions_resolved`, `draft`, `squash`, `squash_on_merge`,
  `should_remove_source_branch`, `force_remove_source_branch`,
  `merge_when_pipeline_succeeds`, `merge_after`, `sha`, `changes_count`,
  `user_notes_count`, `head_pipeline { id status web_url }`, `reviewers[]`,
  `assignees[]`, `labels[]`, `source_project_id`, `target_project_id`,
  `web_url`.
- `GET /projects/:id/merge_requests/:iid/approvals` includes `approved`,
  `approvals_required`, `approvals_left`, `user_can_approve`,
  `user_has_approved`, `approved_by[]`, `merge_status`,
  `require_password_to_approve`. (Both fields the web research flagged as
  unconfirmed exist.)
- `GET /projects/:id/merge_requests?state=opened` list rows carry
  `iid title state draft author reviewers assignees labels source_branch
  target_branch detailed_merge_status has_conflicts
  blocking_discussions_resolved user_notes_count upvotes downvotes
  created_at updated_at merged_at closed_at web_url references sha
  merge_when_pipeline_succeeds` (no pipeline status; the head pipeline needs
  the single-MR read or `/pipelines`).
- `GET /projects/:id` `permissions`, `merge_method`, `squash_option`,
  `only_allow_merge_if_pipeline_succeeds`,
  `only_allow_merge_if_all_discussions_are_resolved` are `null` without
  authentication; they require a token.
- `GET /projects/:id/merge_requests/:iid/discussions` returns discussions with
  `notes[] { id type system resolvable resolved position { new_path … } body
  author created_at }`.
- `glab` flags on the installed version: `mr list -F json --state
  (--all/--closed/--merged) --author --assignee --reviewer --label
  --not-label --draft --not-draft --search --target-branch --source-branch
  --milestone --order --sort --page --per-page`; `mr view -F json
  [--comments --resolved --unresolved]`; `mr diff --raw`; `mr approve
  [--sha]`; `mr revoke`; `mr merge --squash --remove-source-branch
  --auto-merge --rebase --sha -m --squash-message -y`; `mr checkout -b -f
  -u`; `mr note`; `mr close`; `mr reopen`; `mr update --ready --draft
  --reviewer --assignee --label --remove-source-branch
  --squash-before-merge`; `glab api`; `glab auth status --hostname`. There
  is no `--when-pipeline-succeeds` and no `--track`.
- `glab config` keeps one default `host:` plus per-host entries; `glab auth
  status` prints one block per configured host (already parsed by
  `parse_gitlab_auth_status`).

## Addendum (same day): surfaces needed for full PR functionality

GitHub, confirmed on the installed `gh` and through GraphQL introspection:

- `gh pr edit` flags: `--title --body --body-file - --base --milestone
  --remove-milestone --add-assignee/--remove-assignee (@me) --add-label/
  --remove-label --add-reviewer (adds or re-requests) --remove-reviewer
  --add-project/--remove-project`.
- `gh pr update-branch [--rebase]`, `gh pr lock [--reason off_topic|resolved|
  spam|too_heated]`, `gh pr unlock`, `gh pr revert` exist.
- GraphQL mutations present: `addPullRequestReview`, `submitPullRequestReview`,
  `updatePullRequestReview`, `deletePullRequestReview`,
  `dismissPullRequestReview`, `addPullRequestReviewThread`,
  `addPullRequestReviewThreadReply`, `addPullRequestReviewComment`,
  `updatePullRequestReviewComment`, `deletePullRequestReviewComment`,
  `resolveReviewThread`, `unresolveReviewThread`, `addComment`,
  `updateIssueComment`, `deleteIssueComment`, `minimizeComment`,
  `unminimizeComment`, `addReaction`, `removeReaction`, `updatePullRequest`,
  `updatePullRequestBranch`, `convertPullRequestToDraft`,
  `markPullRequestReadyForReview`, `closePullRequest`, `reopenPullRequest`,
  `enablePullRequestAutoMerge`, `disablePullRequestAutoMerge`,
  `addAssigneesToAssignable`, `removeAssigneesFromAssignable`,
  `addLabelsToLabelable`, `removeLabelsFromLabelable`.
- **No** public mutation applies a review suggestion (nothing named
  `applySuggestion` or similar exists); `viewerCanApplySuggestion` is read
  only. GitHub suggestions therefore cannot be applied through the API.
- REST: `GET/POST pulls/{n}/reviews`, `PUT pulls/{n}/reviews/{id}/dismissals`
  (`message`), `GET pulls/{n}/requested_reviewers`, `POST/DELETE
  pulls/{n}/requested_reviewers`, `GET/PATCH/DELETE pulls/comments/{id}`,
  `POST pulls/{n}/comments/{id}/replies`, `GET/PATCH/DELETE
  issues/comments/{id}`, reactions (`issues/{n}/reactions`,
  `issues/comments/{id}/reactions`, `pulls/comments/{id}/reactions`, with
  `content` in `+1 -1 laugh confused heart hooray rocket eyes`), `PUT
  pulls/{n}/update-branch`. Comment objects carry a `reactions` summary
  (`+1`, `-1`, … `total_count`).

GitLab, confirmed against gitlab.com (REST unauthenticated where public;
GraphQL by schema introspection):

- REST `GET …/merge_requests/:iid/reviewers` returns `[{ user, state,
  created_at }]` with `state` in `unreviewed | reviewed | requested_changes |
  approved | unapproved | review_started`.
- GraphQL enum `MergeRequestReviewState = UNREVIEWED REVIEWED
  REQUESTED_CHANGES APPROVED UNAPPROVED REVIEW_STARTED`; mutations
  `mergeRequestRequestChanges(projectPath, iid)`,
  `mergeRequestDestroyRequestedChanges(projectPath, iid)`,
  `mergeRequestReviewerRereview(projectPath, iid, userId)`,
  `mergeRequestSetReviewers(projectPath, iid, reviewerUsernames,
  operationMode)`, `mergeRequestUpdate(… state, targetBranch,
  removeSourceBranch, overrideRequestedChanges)`. The REST issue
  gitlab-org/gitlab#451995 ("Add API for the Request changes reviewer
  status") is still open, so request-changes is GraphQL-only. The feature
  itself shipped default-on in 17.2 (flag removed 17.3); "remove your change
  request" in 17.8; maintainers can bypass with a warning.
- REST update: `PUT …/merge_requests/:iid` with `title description
  assignee_ids reviewer_ids labels add_labels remove_labels milestone_id
  remove_source_branch squash state_event target_branch discussion_locked
  allow_collaboration`.
- Approvals: `POST …/approve` (`sha`, `approval_password`), `POST
  …/unapprove`, `PUT …/reset_approvals` (bot tokens only).
- Discussions: `POST …/discussions` with `position[base_sha start_sha
  head_sha position_type new_path old_path new_line old_line line_range]`;
  `POST …/discussions/:id/notes` (`body`); `PUT …/discussions/:id`
  (`resolved`); `PUT/DELETE …/discussions/:id/notes/:note_id`.
- Suggestions: `PUT /suggestions/:id/apply` (`commit_message`), `PUT
  /suggestions/batch_apply` (`ids[]`, `commit_message`); Developer+.
- Emoji reactions: `GET/POST/DELETE …/merge_requests/:iid/award_emoji`
  (`name`), and `…/merge_requests/:iid/notes/:note_id/award_emoji`.
- `glab mr` subcommands on 1.114: `approve approvers checkout close create
  delete diff for issues list merge note{create,delete,list,reopen,resolve,
  update} rebase [--skip-ci] reopen revoke subscribe todo unsubscribe update
  view`. `glab mr note create` (experimental) supports `-m --file --line
  --old-line --reply`.
