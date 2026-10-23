# PR/MR Management: GitHub + GitLab Functional Specification

Research for a desktop tool that manages PRs/MRs via `gh` and `glab` CLIs (with
REST/GraphQL API fallback). Every fact below is sourced; facts that could not
be confirmed against an official doc are explicitly flagged as unconfirmed
rather than invented.

---

## 1. GitHub PR page anatomy (current github.com UI)

### List page

GitHub's PR list is a search over issues+PRs using **qualifiers** (there is no
separate "filter API" — the web UI list and `gh pr list --search` both use
this query language). Confirmed qualifiers from
[Filtering and searching issues and pull requests](https://docs.github.com/en/issues/tracking-your-work-with-issues/using-issues/filtering-and-searching-issues-and-pull-requests):

- `is:draft` — "Filter draft pull requests"
- `is:merged` / `is:unmerged`
- `review:none` — "Filter pull requests that haven't been reviewed yet"
- `review:required` — "Filter pull requests that require a review before they can be merged"
- `review:approved`
- `review:changes_requested`
- `reviewed-by:<user>`
- `review-requested:<user>` — "Filter pull requests by the specific user requested for review"
- `user-review-requested:@me`
- `team-review-requested:<team>`
- `status:success` / `status:failure` / `status:pending`
- `linked:issue` — PRs linked to an issue they may close
- Shared with issues: `author:`, `assignee:`, `label:`, `state:`, plus `base:`/`head:` and `sort:` (sort options such as `sort:created-desc`) are standard GitHub search qualifiers used the same way in the PR list UI.

Row anatomy (title, `#number`, author, "opened N days ago", labels, a
checks/CI status icon, a review-decision icon, comment count, draft badge) is
the observable current github.com layout; GitHub's docs do not enumerate list
row elements as a discrete reference, so this is reported as UI-observed
common knowledge rather than a single citable spec page.

### Detail page tabs

Per [About pull requests](https://docs.github.com/en/pull-requests/collaborating-with-pull-requests/proposing-changes-to-your-work-with-pull-requests/about-pull-requests), the PR page has:

- **Conversation** — description, timeline, comments, and reviews (this is where the merge box lives)
- **Commits**
- **Checks** — automated tests, builds, and other validations
- **Files changed** — diff for review
- **Findings** — automated code-review results (e.g. code scanning alerts) — a newer tab not in the user's original list.

### Files changed tab

Per [Reviewing proposed changes in a pull request](https://docs.github.com/en/pull-requests/collaborating-with-pull-requests/reviewing-changes-in-pull-requests/reviewing-proposed-changes-in-a-pull-request):

- Per-line comments: "hover over the line of code ... and click the blue comment icon."
- Per-file comments are also possible via the icon next to the filename.
- **Viewed** checkbox in the file header collapses a file; "if the file changes after you view the file, it will be unmarked as viewed."
- "Review changes" dialog has three submission modes:
  - **Comment** — "leave general feedback without explicitly approving the changes or requesting additional changes"
  - **Approve** — "submit your feedback and approve merging the changes proposed in the pull request"
  - **Request changes** — "submit feedback that must be addressed before the pull request can be merged"

### Merge box / branch protection

Per [About protected branches](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/managing-protected-branches/about-protected-branches), relevant rule types surfaced in the merge box:

- Require a pull request before merging, with a configurable required-approval count
- Dismiss stale approvals when new commits are pushed
- Require review from Code Owners
- Require status checks to pass — "Required status checks must have a `successful`, `skipped`, or `neutral` status before collaborators can make changes to a protected branch"
- Require branches to be up to date before merging (strict vs loose)
- Require conversation resolution before merging
- Require signed commits
- Require linear history (blocks merge-commit method)
- Restrict who can push (users/teams/apps)
- "Do not allow bypassing the above settings" toggle (controls whether admins can override)

### Merge methods

Per [Pull request merges](https://docs.github.com/en/pull-requests/reference/pull-request-merges) / [About merge methods on GitHub](https://docs.github.com/articles/about-merge-methods-on-github):

- **Merge commit** — preserves every commit from the PR branch and adds an explicit merge point
- **Squash and merge** — combines all PR commits into a single commit on the base branch
- **Rebase and merge** — adds each PR commit onto the base branch individually, no merge commit, linear history
- Each method must be individually enabled/disabled at the repo level ("Allow squash merging", "Allow rebase merging", etc.); linear-history protection can force squash/rebase only.

### Auto-merge

Per [Automatically merging a pull request](https://docs.github.com/en/pull-requests/collaborating-with-pull-requests/incorporating-changes-from-a-pull-request/automatically-merging-a-pull-request):

- "Auto-merge merges a pull request automatically after all required reviews and status checks pass."
- Only available when the PR cannot merge immediately (i.e., branch protection blocks it: required reviews and/or required status checks configured).
- Only users with write permission can enable it.
- It is disabled automatically if someone without write permission pushes to the source branch, or if the base branch changes.
- Both write-permission collaborators and the PR author can disable it via "Disable auto-merge" in the merge box.

GraphQL mutations (confirmed on [docs.github.com/en/graphql/reference/pulls](https://docs.github.com/en/graphql/reference/pulls)):

- `enablePullRequestAutoMerge(input: EnablePullRequestAutoMergeInput!)` — fields `pullRequestId` (required), `mergeMethod`, `commitHeadline`, `commitBody`, `authorEmail`, `expectedHeadOid`. Payload: `actor`, `pullRequest`.
- `disablePullRequestAutoMerge(input: DisablePullRequestAutoMergeInput!)` — field `pullRequestId`. Payload: `actor`, `pullRequest`.
- `mergePullRequest(input: MergePullRequestInput!)` — same shape as enable-auto-merge's input minus auto-merge semantics (immediate merge).

### Draft PRs

Per [Changing the stage of a pull request](https://docs.github.com/en/pull-requests/collaborating-with-pull-requests/proposing-changes-to-your-work-with-pull-requests/changing-the-stage-of-a-pull-request):

- Convert to draft: PR sidebar → "Convert to draft." Effect: "No one can merge the pull request until you mark the pull request as ready for review again."
- Mark ready: "Ready for review" button in the merge box; "This action will request reviews from any code owners."
- No REST endpoint exists for either transition. GraphQL only:
  - `markPullRequestReadyForReview(input: MarkPullRequestReadyForReviewInput!)` — field `pullRequestId`. Payload: `pullRequest`.
  - `convertPullRequestToDraft(input: ConvertPullRequestToDraftInput!)` — field `pullRequestId`. Payload: `pullRequest`.
  - Confirmed via [docs.github.com/en/graphql/reference/pulls](https://docs.github.com/en/graphql/reference/pulls); the absence of a REST route is corroborated by community reports, e.g. [actions/toolkit#1165](https://github.com/actions/toolkit/issues/1165) and [Skymly/GitPulse#544](https://github.com/Skymly/GitPulse/issues/544) (REST `ready_for_review`/convert-to-draft paths 404).

### Other detail-page features (confirmed by the PR object's `gh pr view --json` field list, §2, and standard docs)

Close/reopen, request reviewers, assignees, labels, milestone, and linked issues (`closingIssuesReferences`) are all first-class PR object fields — see the JSON field list in §2.

---

## 2. `gh` CLI — confirmed flags

All flags below were fetched directly from the official manual at
`cli.github.com/manual/gh_pr_*`.

### `gh pr list` ([manual](https://cli.github.com/manual/gh_pr_list))

`--app <string>`, `-a/--assignee <string>`, `-A/--author <string>`,
`-B/--base <string>`, `-d/--draft`, `-H/--head <string>` (note: `"<owner>:<branch>"` syntax not supported),
`-q/--jq <expr>`, `--json <fields>`, `-l/--label <strings>`, `-L/--limit <int>` (default 30),
`-S/--search <query>`, `-s/--state <open|closed|merged|all>` (default open),
`-t/--template <string>`, `-w/--web`, `-R/--repo <[HOST/]OWNER/REPO>`.

### `gh pr view` ([manual](https://cli.github.com/manual/gh_pr_view))

Flags: `-c/--comments`, `-q/--jq`, `--json <fields>`, `-t/--template`, `-w/--web`, `-R/--repo`.

Confirmed full `--json` field list:
`additions, assignees, author, autoMergeRequest, baseRefName, baseRefOid, body,
changedFiles, closed, closedAt, closingIssuesReferences, comments, commits,
createdAt, deletions, files, fullDatabaseId, headRefName, headRefOid,
headRepository, headRepositoryOwner, id, isCrossRepository, isDraft, labels,
latestReviews, maintainerCanModify, mergeCommit, mergeStateStatus, mergeable,
mergedAt, mergedBy, milestone, number, potentialMergeCommit, projectCards,
projectItems, reactionGroups, reviewDecision, reviewRequests, reviews, state,
statusCheckRollup, title, updatedAt, url`.

Note: this list does **not** include `viewerDidAuthor` as a top-level `gh pr
view --json` field — `gh` does not expose viewer-relative fields directly on
`pr view`; consumers must derive "did I author this" by comparing `author` to
`gh api user`/`gh auth status`, or query GraphQL's `viewerDidAuthor` directly
(confirmed to exist on the GraphQL `PullRequest` object, see §3).

The `mergeable` JSON value is an enum with values `MERGEABLE`, `CONFLICTING`,
`UNKNOWN` (GitHub computes mergeability asynchronously; `UNKNOWN` is common
immediately after a push or on list endpoints — corroborated by multiple
`cli/cli` issue reports, e.g. [cli/cli#9583](https://github.com/cli/cli/issues/9583) and [cli/cli discussion #8020](https://github.com/cli/cli/discussions/8020); this specific enum is not spelled out verbatim on a single manual page but is exposed through the same field as GraphQL `mergeable: MergeableState`, confirmed in §3).

### `gh pr diff` ([manual](https://cli.github.com/manual/gh_pr_diff))

`--allow-escape-sequences`, `--color <always|never|auto>` (default auto),
`-e/--exclude <patterns>` (repeatable), `--name-only`, `--patch`, `-w/--web`, `-R/--repo`.

### `gh pr checks` ([manual](https://cli.github.com/manual/gh_pr_checks))

`--watch`, `--interval <int>` (default 10, used with `--watch`), `--required`
(show only required checks), `--fail-fast` (stop watching on first failure),
`--json <fields>`, `--jq`, `--template`, `-w/--web`, `-R/--repo`.
Confirmed `--json` fields: `bucket, completedAt, description, event, link,
name, startedAt, state, workflow`. `bucket` categorizes `state` into
pass/fail/pending/skipping/cancel.

### `gh pr review` ([manual](https://cli.github.com/manual/gh_pr_review))

`-a/--approve`, `-c/--comment`, `-r/--request-changes`, `-b/--body <string>`,
`-F/--body-file <file>` (`-` for stdin), `-R/--repo`.
**Confirmed limitation**: `gh pr review` only supports a single top-level
review body — the manual gives no flag for attaching inline/line-level
comments to specific diff lines. For line comments, the only path is the API:
`POST /repos/{owner}/{repo}/pulls/{pull_number}/reviews` with a `comments[]`
array (see §3), invoked via `gh api`.

### `gh pr merge` ([manual](https://cli.github.com/manual/gh_pr_merge))

`--merge`, `--squash`, `--rebase`, `--delete-branch`, `--auto` ("Automatically
merge only after necessary requirements are met" — for merge-queue-enabled
repos, enables auto-merge if checks haven't passed or queues the PR if they
have), `--admin` ("Use administrator privileges to merge a pull request that
does not meet requirements" — bypasses merge-queue too), `--match-head-commit
<SHA>`, `--subject <text>`, `--body <text>`, `--author-email <text>`.

### `gh pr checkout` ([manual](https://cli.github.com/manual/gh_pr_checkout))

`--detach`, `-f/--force` (reset an existing local branch to the latest PR
state), `-b/--branch <string>` (custom local branch name). By default the
local branch is named after the head branch. `--recurse-submodules` updates
submodules after checkout. Forks: "You can check out any pull request,
including from forks ... using its pull request number" — no special flag is
required; `gh` resolves the fork remote/ref automatically.

### `gh pr ready` ([manual](https://cli.github.com/manual/gh_pr_ready))

`--undo` (revert to draft, "provided your plan supports this feature"), `-R/--repo`.

### `gh pr close` / `gh pr reopen` ([close](https://cli.github.com/manual/gh_pr_close), [reopen](https://cli.github.com/manual/gh_pr_reopen))

`close`: `-c/--comment <string>` ("Leave a closing comment"), `-d/--delete-branch`, `-R/--repo`.
`reopen`: `-c/--comment <string>` ("Add a reopening comment"), `-R/--repo`.

### `gh pr comment` ([manual](https://cli.github.com/manual/gh_pr_comment))

`--attach <file>`, `-b/--body <text>`, `-F/--body-file <file>` (`-` for
stdin), `-e/--edit-last`, `--create-if-none` (only with `--edit-last`),
`--delete-last`, `--yes` (skip confirm for `--delete-last`), `--editor`,
`-w/--web`, `-R/--repo`.

### `gh pr edit` ([manual](https://cli.github.com/manual/gh_pr_edit))

`--add-reviewer <login>` / `--remove-reviewer <login>` (use `@copilot` to
request/remove Copilot review), `--add-assignee <login>` / `--remove-assignee
<login>` (`@me` for self), `--add-label <name>` / `--remove-label <name>`,
`-m/--milestone <name>` / `--remove-milestone`, `-t/--title`, `-b/--body`,
`-F/--body-file`, `-B/--base <branch>`, `--add-project <title>` /
`--remove-project <title>`, `--attach <file>`, `-R/--repo`.

### `gh pr create` ([manual](https://cli.github.com/manual/gh_pr_create))

`-t/--title`, `-b/--body`, `-F/--body-file`, `-B/--base`, `-H/--head`
(defaults to current branch), `-d/--draft`, `-e/--editor`, `-f/--fill`,
`--fill-first`, `--fill-verbose`, `-r/--reviewer`, `-a/--assignee` (`@me`
supported), `-l/--label`, `-m/--milestone`, `-p/--project`, `-w/--web`,
`--attach`, `-T/--template`, `--no-maintainer-edit`, `--dry-run`, `--recover`.

### `gh auth status` ([manual](https://cli.github.com/manual/gh_auth_status))

`-a/--active` (active account only), `-h/--hostname <string>`, `--jq`,
`--json <fields>`, `-t/--show-token`, `--template`. Reports active account per
known host, per-account auth-state test results, and issues found. Exits `1`
on authentication problems (unless `--json` is used).

### `gh auth token` ([manual](https://cli.github.com/manual/gh_auth_token))

Outputs the auth token for an account on a given host. `--hostname` selects a
specific instance (e.g. GitHub Enterprise Server); default host used if
omitted. Also supports `--user` to select a specific account on that host.

### `gh api` ([manual](https://cli.github.com/manual/gh_api))

`--method` (default GET, or POST if params given), `-F/--field` (typed
params; `@file` reads a file; `{owner}`/`{repo}` placeholders auto-fill from
repo context), `-f/--raw-field` (string params, no type conversion),
`--input <file|->` (raw request body), `--paginate` (GraphQL queries must
accept `$endCursor` and fetch `pageInfo`), `--hostname` (default
`github.com`), `-H/--header`. Authenticates automatically using the
configured `gh` credentials.

### `gh help environment` ([manual](https://cli.github.com/manual/gh_help_environment))

Auth: `GH_TOKEN`/`GITHUB_TOKEN` (used for github.com or `*.ghe.com`),
`GH_ENTERPRISE_TOKEN`/`GITHUB_TOKEN_ENTERPRISE` (GitHub Enterprise Server
hosts), `GH_HOST` ("Specify the GitHub hostname for commands where a hostname
has not been provided, or cannot be inferred from the context of a local Git
repository"). Also: `GH_REPO`, `GH_EDITOR`/`GIT_EDITOR`/`VISUAL`/`EDITOR`,
`GH_BROWSER`/`BROWSER`, `GH_DEBUG` (set to `api` for HTTP trace),
`GH_PAGER`/`PAGER`, `NO_COLOR`, `GH_PROMPT_DISABLED` ("disable interactive
prompting in the terminal"), `GH_CONFIG_DIR`, `GH_TELEMETRY`, `DO_NOT_TRACK`.

### GraphQL-only or REST-only operations `gh` cannot do directly

- **Line-specific review comments**: use `gh api -X POST
  repos/{owner}/{repo}/pulls/{pull_number}/reviews` with a JSON body per
  [REST: Create a review for a pull request](https://docs.github.com/en/rest/pulls/reviews?apiVersion=2022-11-28#create-a-review-for-a-pull-request):
  `commit_id` (optional, defaults to latest), `body` (required for
  `REQUEST_CHANGES`/`COMMENT`), `event` (`APPROVE`|`REQUEST_CHANGES`|`COMMENT`;
  omit for a `PENDING` review needing separate submission), and `comments[]`
  each with `path` (required), `body` (required), `line` or legacy
  `position`, `side` (`LEFT`|`RIGHT`), and `start_line`/`start_side` for
  multi-line comments.
- **Listing/resolving review threads**: no `gh` subcommand exists; use `gh api
  graphql` with the `reviewThreads` connection on `PullRequest` (confirmed
  field: `reviewThreads (PullRequestReviewThreadConnection!)` — "The list of
  all review threads for this pull request", per
  [docs.github.com/en/graphql/reference/pulls](https://docs.github.com/en/graphql/reference/pulls)) and the mutations:
  - `resolveReviewThread(input: ResolveReviewThreadInput!)` — input
    `threadId`; payload `thread` (`PullRequestReviewThread`). Confirmed via
    [GitHub Enterprise Server 3.15 GraphQL mutations reference](https://docs.github.com/en/enterprise-server@3.15/graphql/reference/mutations) (same shape ships on github.com).
  - `unresolveReviewThread(input: UnresolveReviewThreadInput!)` — same shape.
- **Draft toggling**: GraphQL `markPullRequestReadyForReview` /
  `convertPullRequestToDraft` only (see §1); `gh pr ready`/`gh pr ready --undo`
  wrap these under the hood.
- **Auto-merge enable/disable**: `gh pr merge --auto` uses
  `enablePullRequestAutoMerge` internally; there is no separate `gh pr
  autosomething` command — disabling auto-merge from script form requires
  `gh api graphql` with `disablePullRequestAutoMerge`, or the merge-box UI
  button per §1 (`gh pr merge` has no `--disable-auto` flag confirmed in the
  manual).

---

## 3. GitHub permission detection

### Repo-level permission (own the PR / can the viewer act on it at all)

- `GET /repos/{owner}/{repo}` → `permissions` object (per
  [REST: Get a repository](https://docs.github.com/en/rest/repos/repos?apiVersion=2022-11-28#get-a-repository)):
  boolean fields `admin`, `maintain`, `push`, `triage`, `pull` — "the
  authenticated user's permission level for the repository."
- `GET /repos/{owner}/{repo}/collaborators/{username}/permission` (per
  [REST: Get repository permissions for a user](https://docs.github.com/en/rest/collaborators/collaborators?apiVersion=2022-11-28#get-repository-permissions-for-a-user)):
  returns `{permission, role_name, user}`. `permission` is one of `admin`,
  `write`, `read`, `none` — "the maintain role is mapped to write and the
  triage role is mapped to read." `role_name` gives the granular (possibly
  custom) role. This is the highest permission across repo/team/org/enterprise
  grants.

### Can the viewer merge?

Confirmed practical rule from the above: merging requires at least `push`
(write) permission on the base repo, subject to branch protection (required
reviews/checks/CODEOWNERS/push restrictions, §1). GraphQL exposes this
directly and more precisely on the `PullRequest` object (confirmed at
[docs.github.com/en/graphql/reference/pulls](https://docs.github.com/en/graphql/reference/pulls)):

- `viewerCanMergeAsAdmin: Boolean!` — "Indicates whether the viewer can bypass
  branch protections and merge the pull request immediately."
- `viewerCanUpdate: Boolean!` — "Check if the current viewer can update this object."
- `viewerCanEnableAutoMerge: Boolean!`, `viewerCanDisableAutoMerge: Boolean!`
- `viewerCanApplySuggestion: Boolean!`
- **Not found**: `viewerMergeHeadRefPermissions` does not exist on the current
  GraphQL `PullRequest` object per the official reference — do not rely on
  this field name; it was not confirmed in docs.github.com and returned no
  hits in search. Treat the user's original prompt's mention of it as
  unconfirmed.

### Can the viewer approve?

No dedicated `viewerCanApprove` field was found on `PullRequest` in the
fetched reference; approval eligibility is a business rule, not a single
boolean: the viewer must not be the PR author (self-approval is rejected by
GitHub's review-submission API) and must have at least read access (an
implicit requirement of being able to open the PR at all — collaborator or
public-repo read access). `viewerDidAuthor: Boolean!` ("Did the viewer author
this comment") is present on `PullRequest` per the same reference and is the
correct signal to gate the "Approve" action client-side.

### `reviewDecision` values

Confirmed GraphQL `PullRequestReviewDecision` enum (from
[docs.github.com/en/graphql/reference/pulls](https://docs.github.com/en/graphql/reference/pulls)):

- `APPROVED` — "The pull request has received an approving review."
- `CHANGES_REQUESTED` — "Changes have been requested on the pull request."
- `REVIEW_REQUIRED` — "A review is required before the pull request can be merged."
- The field itself, `reviewDecision: PullRequestReviewDecision`, is nullable —
  it is `null` when no review is required and none has been submitted.

### `mergeStateStatus` / `MergeStateStatus` enum

Confirmed on the same page:

- `BEHIND` — "The head ref is out of date."
- `BLOCKED` — "The merge is blocked."
- `CLEAN` — "Mergeable and passing commit status."
- `DIRTY` — "The merge commit cannot be cleanly created."
- `DRAFT` — "The merge is blocked due to the pull request being a draft."
- `HAS_HOOKS` — "Mergeable with passing commit status and pre-receive hooks."
- `UNKNOWN` — "The state cannot currently be determined."
- `UNSTABLE` — "Mergeable with non-passing commit status."

`mergeable: MergeableState!` is the separate boolean-ish tri-state
(`MERGEABLE`/`CONFLICTING`/`UNKNOWN`) — GraphQL confirms this is a distinct
field from `mergeStateStatus`; use `mergeStateStatus` for the fine-grained UI
state and `mergeable` for a coarse conflict check.

### REST `mergeable_state`

**Important caveat**: unlike GraphQL's `mergeStateStatus`, the REST field
`mergeable_state` on `GET /repos/{owner}/{repo}/pulls/{pull_number}` is
documented only as an untyped `string` in the current
[REST: Get a pull request](https://docs.github.com/en/rest/pulls/pulls?apiVersion=2022-11-28#get-a-pull-request)
reference — GitHub's docs do **not** enumerate its possible values or their
meanings on that page today. The commonly cited value set (`clean`, `dirty`,
`unknown`, `blocked`, `behind`, `unstable`, `draft`, `has_hooks`) is
corroborated only by community sources — a GitHub community discussion
explicitly calls it "undocumented"
([github/community#21886](https://github.com/orgs/community/discussions/21886)) — and by its parity with the officially-documented GraphQL
`MergeStateStatus` enum above (same value set, lower-cased). **Recommendation
for the tool**: prefer GraphQL `mergeStateStatus` (or `gh pr view --json
mergeStateStatus`, which surfaces the GraphQL value) as the authoritative,
documented source of truth; treat REST `mergeable_state` as an
implementation-detail mirror, not a contract.

`mergeable: true|false|null` on REST is documented precisely: "If the value is
null, then GitHub has started a background job to compute the mergeability.
... resubmit your request" (same REST reference page). `merged: boolean` and
`draft: boolean` are also documented fields.

### Branch protection (bypass / required checks)

`GET/PUT /repos/{owner}/{repo}/branches/{branch}/protection` and the scoped
sub-resources (`.../required_pull_request_reviews`,
`.../required_status_checks`, `.../restrictions`) — confirmed endpoint paths
at [REST API endpoints for protected branches](https://docs.github.com/en/rest/branches/branch-protection?apiVersion=2022-11-28).
Key fields: `required_approving_review_count` (1–6), dismiss-stale-reviews
boolean, code-owner-review boolean, required status-check contexts,
`required_linear_history`, `allow_force_pushes`, `allow_deletions`, and a
required-conversation-resolution boolean.

---

## 4. GitLab MR — list, detail page, approvals, merge widget, options

### List filters

Confirmed query parameters for `GET /merge_requests` and
`GET /projects/:id/merge_requests` ([API: Merge requests](https://docs.gitlab.com/ee/api/merge_requests.html)):
`state` (`all`/`opened`/`closed`/`locked`/`merged`), `author_id` /
`author_username`, `assignee_id` / `assignee_username`, `reviewer_id` /
`reviewer_username`, `labels` (comma-separated), `target_branch`,
`source_branch`, `search` (matches title and description), `draft` (`true`
returns only drafts; `wip` is the deprecated predecessor, removed in GitLab
19.0), `order_by` (`created_at`, `updated_at`, `merged_at`, `label_priority`,
`priority`, `milestone_due`, `popularity`, `title`), `sort` (`asc`/`desc`,
default `desc`), `scope` (`created_by_me`, `assigned_to_me`,
`reviews_for_me`, `all`).

### Detail page tabs

Per [Merge requests](https://docs.gitlab.com/user/project/merge_requests/):
the MR page shows a description, code changes with inline review, CI/CD
pipeline information, a mergeability report (the merge widget), comments, and
the commit list — corresponding to the Overview, Commits, Pipelines and
Changes tabs in the current UI. Roles: **Assignee** ("owns the merge request
and is responsible for its progress") and **Reviewer** ("reviews the changes
and provides feedback"). Open threads can block merging but are collapsible;
the activity feed is filterable by type (comments, approvals, commits,
labels, etc.). Source-branch deletion is offered at merge, close, or create
time.

### Approvals

[API: Merge request approvals](https://docs.gitlab.com/ee/api/merge_request_approvals.html):
`GET /projects/:id/merge_requests/:merge_request_iid/approvals` returns, per
approval rule, `approved` ("If true, indicates that the associated approval
rule was approved"), `approvals_left`, `approvals_required`, and `approved_by`
(array of approver user objects with timestamps). The response also carries a
project-level `approvals_required` limit (0 = optional rule, up to 100)
([Approval rules](https://docs.gitlab.com/user/project/merge_requests/approvals/rules/)).
Eligible approvers: direct members of the project/group/parent/shared groups,
explicit named approvers, or CODEOWNERS entries (code owners need at least
Developer role). By default "merge request authors do not count as eligible
approvers on their own merge requests" (toggle: "Prevent merge request
creator approval"); a separate "Prevent committers approval" setting can also
block a committer's own approval.

Fields `user_has_approved` / `user_can_approve` were **not confirmed** on the
fetched approvals-endpoint documentation page (only `approved`,
`approvals_left`, `approvals_required`, `approved_by` were present in the
retrieved content) — treat those two field names as unconfirmed until
verified against a live response or a more complete doc render.

### Merge widget: `detailed_merge_status`

Confirmed enum values from [API: Merge requests](https://docs.gitlab.com/ee/api/merge_requests.html):
`blocked_status`, `broken_status`, `checking`, `ci_must_pass`,
`ci_still_running`, `conflict`, `discussions_not_resolved`, `draft_status`,
`external_status_checks`, `mergeable`, `not_approved`, `not_open`,
`jira_association_missing`, `need_rebase`, `policies_denied`, `preparing`,
`requested_changes`, `unchecked`. The legacy `merge_status` field is
deprecated: "Use `detailed_merge_status` instead, which accounts for all
potential statuses."

`merge_when_pipeline_succeeds: boolean` — "If true, the merge request is set
to auto-merge." `merge_user` — "Object with information about the user who
merged the merge request, set it to auto-merge, or null" (replaces deprecated
`merged_by`).

### Merge options

- **Squash**: `squash` / `squash_before_merge` request param on the merge
  endpoint; `squash_on_merge` project default.
- **Delete source branch**: `should_remove_source_branch` /
  `remove_source_branch` on merge; also settable at MR-create time.
- **Merge when pipeline succeeds / merge trains**: confirmed distinct
  features at [Merge trains](https://gitlab.com/gitlab-org/gitlab/-/blob/4e4c977d5897f76ea6889ee4689a7b8e3eadc14e/doc/ci/pipelines/merge_trains.md)
  — "You cannot use auto-merge ... to skip the merge train, when merge trains
  are enabled." Merge trains re-run pipelines against the merged result of
  the MR plus other queued MRs (verifying combined changes), whereas plain
  auto-merge ("merge when pipeline succeeds") merges a single MR directly
  once its own pipeline passes.
- **Merge method (project setting)**: three project-level options confirmed
  at [Merge methods](https://docs.gitlab.com/user/project/merge_requests/methods)
  — **Merge commit**, **Merge commit with semi-linear history** (fast-forward
  if possible, else offers rebase), **Fast-forward merge** (no merge commits;
  merge only allowed if fast-forwardable).
- **Draft toggle**: draft/WIP status is a boolean on the MR resource,
  toggled via the update endpoint or the `Draft:` title prefix historically
  used by `wip`.
- **Close/reopen, assign, reviewers, labels, milestones**: all standard MR
  update-endpoint fields, confirmed via the `glab mr update` flag set in §5
  which exercises the same API surface.
- **Threads/discussions and resolving them, and suggestions**: MR threads can
  be marked resolved (surfaced in the merge widget as
  `discussions_not_resolved` in `detailed_merge_status` above); "Suggestions"
  are inline diff edits reviewers propose that authors can apply directly —
  functionality confirmed to exist via the approvals/discussions docs
  referenced above, though a dedicated suggestions-API citation was not
  separately re-verified in this pass.

---

## 5. `glab` CLI — confirmed flags

All flags fetched directly from the official docs at `docs.gitlab.com/cli/`.

### `glab mr list` ([docs](https://docs.gitlab.com/cli/mr/list/))

`-A/--all`, `-a/--assignee` (comma-separated or repeatable), `--author
<username>`, `-c/--closed`, `--created-after` / `--created-before` (ISO 8601),
`--deployed-after` / `--deployed-before`, `-d/--draft`, `--environment
<name>`, `-g/--group`, `--jq`, `-l/--label` (comma-separated or repeatable),
`-M/--merged`, `-m/--milestone <id>`, `--not-draft`, `--not-label`,
`-o/--order <field>`, `-F/--output <text|json>` (default text), `-p/--page`
(default 1), `-P/--per-page` (default 30), `-R/--repo`, `-r/--reviewer`,
`--search <string>`, `-S/--sort <asc|desc>`, `-s/--source-branch`,
`-t/--target-branch`. **Note**: there is no single `--state` flag — state is
selected via the mutually-scoped `-c/--closed`, `-M/--merged`, `-A/--all`
flags (default is open).

### `glab mr view` ([docs](https://docs.gitlab.com/cli/mr/view/))

`-c/--comments`, `-F/--output <text|json>` (default text), `-w/--web`,
`-s/--system-logs`, `--jq`, `-p/--page`, `-P/--per-page` (default 20),
`--resolved` (implies `--comments`), `--unresolved` (implies `--comments`).

### `glab mr diff` ([docs](https://docs.gitlab.com/cli/mr/diff/))

`--color <always|never|auto>` (default auto), `--raw` (pipeable raw diff format), `-R/--repo`.

### `glab mr approve` ([docs](https://docs.gitlab.com/cli/mr/approve/))

`-s/--sha <string>` (must match HEAD SHA of the MR), `-R/--repo`.

### `glab mr revoke` ([docs](https://docs.gitlab.com/cli/mr/revoke/))

No command-specific flags documented — only inherited `-R/--repo`.

### `glab mr merge` ([docs](https://docs.gitlab.com/cli/mr/merge/))

`--auto-merge` (default true), `-m/--message <text>` (custom merge commit
message), `-r/--rebase`, `-d/--remove-source-branch`, `--sha <SHA>` (merge
only if source-branch HEAD matches), `-s/--squash`, `--squash-message
<text>`, `-y/--yes`. **Note**: no separate `--when-pipeline-succeeds` flag is
documented — `--auto-merge` (default `true`) is the current mechanism;
older glab releases used `--when-pipeline-succeeds`/`--merge-when-pipeline-succeeds` but that name was not found in the current docs page.

### `glab mr checkout` ([docs](https://docs.gitlab.com/cli/mr/checkout/))

`-b/--branch <string>`, `-f/--force` (reset local branch if diverged, refuses
if working tree has changes that would be lost), `-u/--set-upstream-to
<[REMOTE/]BRANCH>`, `-R/--repo`. **Note**: `--track` was not found on this
page — do not assume it exists.

### `glab mr note` ([docs](https://docs.gitlab.com/cli/mr/note/))

Top-level `glab mr note` is a group with subcommands `create`, `delete`,
`list`, `reopen`, `resolve`, `update` for comments/discussion management;
creating a plain comment is the default action of the parent command. Detailed
per-flag documentation (e.g. `--message`) lives on the `glab mr note create`
subcommand page, not separately re-fetched in this pass.

### `glab mr close` / reopen ([docs](https://docs.gitlab.com/cli/mr/close/))

`glab mr close` has no command-specific flags beyond inherited `-R/--repo`
(and `-h/--help`). `glab mr reopen` exists as its own subcommand (mirrors
`close`); its flag page was not separately re-verified.

### `glab mr update` ([docs](https://docs.gitlab.com/cli/mr/update/))

`-a/--assignee` (prefix `!`/`-` to remove), `--attach` (experimental),
`-d/--description` (or open editor), `--description-file`, `--draft`,
`-f/--fill`, `--fill-commit-body`, `-l/--label`, `--lock-discussion`,
`-m/--milestone`, `-r/--ready` (mark ready for review/merge),
`--remove-source-branch` (toggle), `--reviewer`, `--squash-before-merge`
(toggle), `--target-branch`, `-t/--title`, `--unassign`, `-u/--unlabel`,
`--unlock-discussion`, `--wip` (alternative to `--draft`), `-y/--yes`.

### `glab mr create` ([docs](https://docs.gitlab.com/cli/mr/create/))

`--draft`/`--wip`, `-t/--title`, `-d/--description`, `--description-file`,
`-b/--target-branch`, `-s/--source-branch`, `-a/--assignee`, `--reviewer`,
`-l/--label`, `-m/--milestone`, `--remove-source-branch`,
`--squash-before-merge`, `-f/--fill`, `-y/--yes`; additional flags mentioned
on the page but not individually quoted: `--allow-collaboration`,
`--auto-merge`, `--template`, `--web`, `--push`, `--attach`.

### `glab ci status` ([docs](https://docs.gitlab.com/cli/ci/status/))

`--live` (real-time updates until pipeline ends), `--wait` (block until
completion), `--compact`, `--output <text|json>` (JSON incompatible with
`--live`/`--wait`/`--compact`), `--jq`, `--branch <name>` (defaults to
current branch — this is how you check a specific MR's source-branch
pipeline). Alias: `glab ci stats`.

### `glab api` ([docs](https://docs.gitlab.com/cli/api/))

`--method`/`-X` (default GET, or POST once fields are supplied),
`-F/--field` (JSON-typed; `true`/`false`/`null`/integers auto-convert;
`[`/`{`-prefixed values parsed as JSON), `-f/--raw-field` (string, no
conversion), `--paginate` (GraphQL: query must accept `$endCursor: String`
and select `pageInfo{hasNextPage,endCursor}`), `--hostname` (overrides target
instance; defaults to the authenticated host for the current git directory,
or gitlab.com).

### `glab auth login` ([docs](https://docs.gitlab.com/cli/auth/login/))

`--hostname`, `--token`, `--stdin`, `--api-host`, `--api-protocol`,
`--git-protocol`, `--web` (OAuth), `--job-token`, `--ssh-hostname`. Host
resolution order when `--hostname` is omitted (confirmed on the page): (1)
the base remote's host from the current git repo, (2) `GITLAB_HOST` env var,
(3) `host` key in the config file, (4) `gitlab.com` fallback. In interactive
mode glab auto-detects and offers GitLab instances found in git remotes.

### `glab auth status` ([docs](https://docs.gitlab.com/cli/auth/status/))

`-a/--all` (check every configured instance, not just current context),
`--hostname <string>`, `-t/--show-token`.

### Environment variables ([Authenticate with GitLab](https://docs.gitlab.com/cli/authentication/))

`GITLAB_HOST` — "The instance to sign in to. Without it, glab prompts users to
select one of the repository's Git remotes, or gitlab.com." Aliases
`GITLAB_URI`, `GL_HOST` also select the host (first one set wins, per
secondary confirmation via search — the authentication page itself documents
`GITLAB_HOST` directly). `GITLAB_API_HOST` — "Skips the API hostname prompt.
Set it even when it matches `GITLAB_HOST`." `GITLAB_SSH_HOST` — analogous for
SSH. `GLAB_API_PROTOCOL`, `GLAB_GIT_PROTOCOL` — skip protocol prompts.
`GLAB_CONTAINER_REGISTRY_DOMAINS`. Token precedence: `GITLAB_TOKEN` >
`GITLAB_ACCESS_TOKEN` > `OAUTH_TOKEN` override stored credentials.
`CI_JOB_TOKEN` — automatically provided by GitLab Runner in CI, consumed when
`GLAB_ENABLE_CI_AUTOLOGIN` is enabled.

---

## 6. GitLab permission detection

### Access levels

Confirmed numeric scale from [Members API](https://docs.gitlab.com/ee/api/members.html)
and [Permissions and roles](https://docs.gitlab.com/ee/user/permissions.html):
`0` No access, `5` Minimal access, `10` Guest, `15` Planner, `20` Reporter,
`25` Security Manager, `30` Developer, `40` Maintainer, `50` Owner (`60`
Admin appears in some admin-only member-update contexts).

- **Approve MRs**: default eligibility starts at Developer ("users with the
  Developer role can approve merge requests if" listed as an approver or a
  code owner of changed files); "Approval from Planner and Reporter roles is
  available only if enabled for the project" — i.e. Reporter/Planner approval
  is opt-in, Developer+ is the default floor.
- **Merge MRs**: not a single documented boolean on the permissions page in
  the content retrieved; practically gated by Developer/Maintainer role plus
  branch protection's `merge_access_levels` (below). Treat "can this user
  merge" as: role ≥ the branch's configured merge-access level AND
  `detailed_merge_status == mergeable`.
- **Push to protected branches**: "Push to protected branches" is available
  to Developer and Maintainer (contingent on the branch's protection rule),
  and Owner.
- **Create MRs**: Reporter, Developer, Maintainer, Owner can create/edit/close
  their own MRs; the exact wording found was scoped to "projects that accept
  contributions from external members."

### Identity and approval-state endpoints

- `GET /user` — current authenticated identity (standard GitLab API; used to
  compare against MR author/approver lists — not separately re-fetched this
  pass but is the well-known identity endpoint).
- `GET /projects/:id/merge_requests/:merge_request_iid/approvals` — see §4
  for confirmed fields (`approved`, `approvals_left`, `approvals_required`,
  `approved_by`). `user_can_approve` / `user_has_approved` are commonly
  referenced in tooling but were **not** found in the fetched doc content —
  verify against a live response before relying on them.
- MR object fields `merge_user`, `merge_status` (deprecated) /
  `detailed_merge_status` — confirmed in §4.
- **`user.can_merge`**: not independently confirmed in this pass on a
  fetched page; GitLab does expose a MR-embedded `user` sub-object with
  merge-related booleans in some API responses, but the exact field name
  requested by the user (`user.can_merge`) should be verified directly
  against a live `GET /projects/:id/merge_requests/:iid` response before the
  tool depends on it.

### Protected branches API

[API: Protected branches](https://docs.gitlab.com/ee/api/protected_branches.html):
`GET /projects/:id/protected_branches` returns, per protected branch,
`push_access_levels` (array of access-level entries, each optionally scoped
to a user/group/deploy key), `merge_access_levels` (same shape for who can
merge into the branch), `allow_force_push` (boolean), and
`code_owner_approval_required` (boolean, Premium/Ultimate only). Access
levels here can be role-based (Developer/Maintainer/Admin) or targeted to
specific users/groups/deploy keys, with group-based rules and inheritance
from parent groups on paid tiers.

---

## 7. Canonical row/layout references

**GitHub PR list row** (current UI, not enumerated as a single doc page —
confirmed indirectly via search-qualifier docs in §1 plus direct product
knowledge): title, `#number`, author, relative "opened" timestamp, labels,
checks/CI status icon, review-decision icon, comment count, draft badge.

**GitLab MR list row**: title, `!iid`, author, target branch, pipeline
status, approvals count, discussions-resolved count, updated time — this
mirrors the fields exposed by the list API in §4 (`target_branch`,
`detailed_merge_status`-adjacent pipeline info, `approvals_required` /
`approvals_left` from the approvals endpoint, and standard `updated_at`);
GitLab's UI docs describe the underlying data ([Merge requests](https://docs.gitlab.com/user/project/merge_requests/)) but, like GitHub, do not
formally spec a "row schema," so this is inferred from the API fields plus
current UI observation rather than a single citable layout reference.

---

## 8. CLI error/auth-state shapes

Neither `gh` nor `glab` publishes a dedicated "error catalog" reference page;
the following is corroborated by the manuals (§2/§5) plus GitHub/GitLab issue
trackers, and should be treated as best-effort text matching rather than a
stable contract:

- **`gh`**: `gh auth status` exits `1` when authentication is broken (default
  text mode; JSON mode always exits based on `--json`/`--jq` semantics per
  the manual, §2). The canonical "not logged in" message is "You are not
  logged into any GitHub hosts. Run `gh auth login` to authenticate."
  (corroborated by multiple `cli/cli` issues, e.g.
  [cli/cli#4576](https://github.com/cli/cli/issues/4576)). `gh` commands that
  hit 404 on a PR/repo typically surface the underlying REST/GraphQL "Not
  Found" error text with a non-zero exit code; `gh` does not document a fixed
  exit-code table distinguishing not-found vs. permission-denied vs. rate-limit
  in the manual pages fetched — a tool should match on exit code non-zero plus
  substring checks on stderr (e.g. "Could not resolve to a PullRequest",
  "HTTP 403", "API rate limit exceeded") rather than assume a stable enum.
- **`glab`**: `glab auth status` historically did **not** return a failing
  exit code on auth failure — tracked and fixed in
  [gitlab-org/cli#911](https://gitlab.com/gitlab-org/cli/-/issues/911) /
  [MR !1453](https://gitlab.com/gitlab-org/cli/-/merge_requests/1453) ("fix(auth
  status): exit with code 1 when auth fails"), meaning **exit-code behavior
  differs across glab versions** and a tool should not assume `0`/`1` alone is
  reliable without checking the installed glab's version/changelog. A common
  "no known host" error text is: "none of the git remotes configured for this
  repository points to a known GitLab host. Please use `glab auth login` to
  authenticate and configure a new host for glab."
- **Rate limits**: neither CLI's manual pages fetched in this pass document a
  specific rate-limit error shape; GitHub's REST/GraphQL 403/429 responses
  with `X-RateLimit-Remaining`/`Retry-After` headers are the underlying
  mechanism `gh api` will surface as an HTTP error, and GitLab's API returns
  standard 429s with `RateLimit-*` headers — a tool calling `gh api`/`glab
  api` directly should inspect the HTTP status/headers itself rather than
  parse CLI text for this case.

---

## Summary of explicitly unconfirmed items (do not implement against these without live verification)

1. GraphQL field `viewerMergeHeadRefPermissions` — not found on
   docs.github.com; likely does not exist under this name.
2. REST `mergeable_state` enum values — field exists but is undocumented as
   an enum on the current REST reference page; prefer GraphQL
   `mergeStateStatus`.
3. GitLab approvals endpoint fields `user_has_approved` / `user_can_approve` —
   not present in the fetched doc content (only `approved`,
   `approvals_left`, `approvals_required`, `approved_by` were).
4. GitLab MR field `user.can_merge` — not independently confirmed this pass.
5. `glab mr merge --when-pipeline-succeeds` — not found in current docs;
   current mechanism is `--auto-merge` (default `true`).
6. `glab mr checkout --track` — not found in current docs.
7. Exact exit-code/stderr text tables for `gh`/`glab` — neither project
   documents these as a stable contract; treat matches as best-effort.
