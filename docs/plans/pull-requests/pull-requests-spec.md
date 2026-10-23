# Pull Requests — Specification

Date: 2026-09-20. Status: **approved by the requester** (design interview of
2026-09-20: every decision not about pull-request functionality accepted as
recommended; every decision that limited pull-request functionality reversed
so that this version delivers the full feature set of the hosts' PR pages).
This document is the authored source of truth for scope;
`pull-requests-plan.md` is the architecture and implementation plan that
argues from it. It follows the shape and rules of
`docs/plans/git-manager/git-manager-spec.md`, whose feature this one sits
beside.

Evidence base, all in `research/`: `bibcode-integration-surface.md` (every
place the Git Manager is wired, which this module mirrors),
`server-provider-layer.md` (the existing `gh`/`glab`/`az`/Bitbucket layer),
`settings-and-capabilities.md` (settings persistence and availability
patterns), `github-gitlab-web-specs.md` (the hosts' PR/MR feature sets, CLI
flags, permission models, with citations), and
`live-verification-2026-09-20.md` (facts confirmed against the real APIs and
the installed CLIs, including every item the web research could not confirm
and the API surfaces needed for the full feature set).

---

## 1. Outcome

Every BiBCode project gains a **Pull Requests** module: a project-scoped
centre view that lists the hosted pull requests (GitHub) or merge requests
(GitLab, including self-hosted instances) of that project's repository, opens
one for inspection in the shape of the host's own PR page, and lets the user
do everything the host's PR page lets that user do — read and write the
conversation, review with inline comments, approve, request changes, dismiss
or revoke, edit the pull request and its reviewers, assignees, labels and
milestone, update the branch, merge with the allowed method, enable
auto-merge, mark ready or draft, close, reopen, revert, react — exactly as far
as the host says that user may, and check the change out locally for
verification, in the main checkout or in a new worktree.

Entry point: a new icon button in the left panel's project header hover strip,
placed immediately after the existing Git Manager button. Clicking it opens
the project's Pull Requests view; clicking it again focuses the open view.

Supported hosts in this version: **GitHub** (github.com and GitHub Enterprise
hosts configured in `gh`) and **GitLab** (gitlab.com and self-hosted
instances configured in `glab`). Azure DevOps and Bitbucket render an
explicit unavailable state naming the host; nothing is attempted for them.

## 2. Hard constraints

Enforced by construction, not by UI omission; no RPC added by this feature may
perform a forbidden action.

1. **Server is the only authority on what the user may do.** Every action's
   availability is computed on the server from the host's own answer (viewer
   identity, repository permission, pull-request state, branch rules, host
   version) and delivered as `{ allowed, reason }`. The client renders
   reasons verbatim and derives no permission policy of its own. The host
   remains the final authority: a refused mutation surfaces the host's
   refusal as a structured error, never as raw CLI stderr.
2. **Single repository per view.** Every read and mutation is addressed by
   the selected checkout's `cwd` of one project; the module never touches
   another project's repository and never resolves a path client-side.
3. **Zero telemetry** (identical to the Git Manager invariant, § 12). The
   only outbound traffic is user-initiated provider-CLI traffic against the
   repository's own host and git traffic against the repository's own
   remotes. No background polling, no avatars, no third-party image fetches,
   no analytics, no remote feature flags, no new dependency that makes a
   network call.
4. **Provider-agnostic core.** One normalised model and one RPC surface; the
   GitHub and GitLab adapters live behind a single server-side trait, and
   every host-specific capability is declared by the adapter (§ 7.3) rather
   than special-cased in the client. No in-app sign-in, OAuth, or token
   storage: the CLIs' ambient credentials on the machine running the server
   are the credentials.
5. **No repository lifecycle** (unchanged from the Git Manager): nothing may
   add, clone, publish, or delete a repository.
6. **No new dependencies.** `react-markdown`, `remark-gfm`,
   `@pierre/diffs`, `lucide-react`, `reqwest`, `serde_json`, and the
   supervised process runner already cover every need.
7. **Every git invocation runs through the supervised process path** with
   timeout, output cap, and cancellation; every provider-CLI invocation runs
   through the same `ProcessRunner` and inherits the same limits.
8. **Log hygiene.** No branch names, PR titles, comment bodies, URLs, hosts,
   account names, or CLI stderr in log strings; stable codes plus lengths and
   counts only.
9. **Only public, documented host APIs.** Operations use the CLIs' commands
   and the hosts' public REST and GraphQL APIs through `gh api` / `glab api`.
   Internal web endpoints are never called; where a host has no public API
   for an action (GitHub suggestion apply), the action is shown as
   unavailable with that reason.

## 3. Scope

### 3.1 In scope

- **Module availability**: an on/off setting, environment capability flags,
  and a per-project context check that says exactly why the module cannot be
  used when it cannot (§ 6).
- **List view** matching the host's list page: state tabs, search, author,
  assignee, reviewer, draft, label, milestone, target-branch and review
  filters, sort, cursor/page-based loading, rows carrying the same signals
  the host shows (§ 8.1).
- **Detail view** matching the host's PR page: header, Conversation,
  Commits, Checks (GitHub) / Pipelines (GitLab), Files changed with inline
  diffs, an editable side column with reviewers, assignees, labels and
  milestone, and the merge box (§ 8.2).
- **Conversation**: general comments (create, edit own, delete own, minimize
  on GitHub), replies in threads, resolving and unresolving threads, review
  threads anchored to diff lines, system events, and reactions (read and
  toggle) on the pull request and on comments.
- **Review**: inline comments on diff lines (single and multi-line) collected
  into one submitted review; submission as Comment, Approve, or Request
  changes on both hosts (GitLab through GraphQL `mergeRequestRequestChanges`,
  gated on host version ≥ 17.2); revoke own approval (GitLab) and remove own
  change request (GitLab ≥ 17.8); dismiss a review with a message (GitHub);
  re-request review from a reviewer (both); suggestions rendered as
  suggestion blocks, applied on GitLab (single and batch) and shown as
  unappliable on GitHub with the reason.
- **Pull request editing**: title, description, base / target branch,
  reviewers, assignees, labels, milestone, GitHub "allow edits by
  maintainers" display, lock / unlock conversation (GitHub with reason;
  GitLab `discussion_locked`).
- **Branch maintenance**: update branch (GitHub merge or rebase; GitLab
  rebase, optional skip-CI), conflict state pointing to Checkout.
- **Merge**: merge, squash, rebase (only the methods the repository allows),
  optional delete-source-branch, auto-merge / merge-when-pipeline-succeeds
  (enable and disable), custom subject and body, admin bypass (GitHub) and
  requested-changes override (GitLab) as explicit secondary actions, pinned
  to the head commit the user saw.
- **State changes**: mark ready for review, convert to draft, close, reopen,
  delete the merge request (GitLab only; GitHub has no delete), revert a
  merged pull request into a new pull request (GitHub `gh pr revert`;
  GitLab: revert the merge commit onto a new branch and open a merge
  request).
- **Checkout for local verification**: into the main checkout or a chosen
  worktree (`gh pr checkout` / `glab mr checkout`), or into a new managed
  worktree created from the PR head ref (§ 8.5), subject to the Git Manager
  guard table.
- **Create pull request**: the existing Git Manager create dialog is opened
  from the list view; no second creation path.
- **Open in browser** for the PR, every check, pipeline, job, commit and
  comment.
- **Settings**: a "Pull requests" switch under Settings → Source Control and
  the existing discovery rows extended with a per-host authentication list
  (§ 6.1).

### 3.2 Out of scope, permanently

- Azure DevOps and Bitbucket beyond the explicit unavailable state (they keep
  their existing resolve/create paths elsewhere).
- Applying suggestions on GitHub (no public API; confirmed by schema
  introspection). The suggestion is rendered and can be copied.
- Administration of branch protection, approval rules, merge queues / merge
  trains, CODEOWNERS, repository merge settings. The module honours the
  host's answers about them.
- GitHub Projects fields, GitLab time tracking, to-dos, subscriptions,
  notifications, and issue management beyond displaying linked issues.
- Cross-repository search, organisation-wide inboxes, "PRs assigned to me
  across all projects".
- Rendering hosted images, avatars, or badges (constraint 3).
- Any GitHub or GitLab sign-in flow inside BiBCode.

## 4. Surface and lifecycle

- **Route.** `/project/$environmentId/$projectId/pull-requests` (list) and
  `/project/$environmentId/$projectId/pull-requests/$number` (detail), both
  rendered as the centre view inside the existing `ChatRouteInset`, like the
  Git Manager route. The route encodes the panel, so reload and restart land
  back on the same list or the same pull request; the detail route keeps the
  active tab in a `tab` search parameter.
- **Sidebar.** The project header hover strip gains a `GitPullRequestIcon`
  button, `aria-label="Pull Requests for <project>"`,
  `data-testid="pull-requests-button"`, after the Git Manager button; the
  project row highlights (`aria-current="page"`) while either Pull Requests
  route of that project is active. The existing `pathname.endsWith("/git")`
  check becomes a route-prefix match shared by both modules. The button is
  shown for every project when the module setting is on (§ 6.1); the view
  explains availability, the button does not guess it.
- **Project identity is physical**, `(environmentId, projectId)`, with the
  same grouped-row disambiguation the Git Manager and New-worktree buttons
  use.
- **Checkout scope.** The view operates on one checkout of the project, the
  main checkout by default, selectable from the same worktree selector the
  Git Manager toolbar uses. The checkout matters for two things only: the
  `origin` remote that identifies the hosted repository, and where a local
  checkout of a PR lands.
- **View-state cache** for the two most recently used projects, holding
  state, not mounted components: selected checkout, list tab and filters,
  scroll anchor, last opened number, the "viewed" file set per pull request,
  and unsent drafts (comment box text, in-progress comment edits, pending
  inline review comments, merge subject/body, title/description edits).
  Drafts are the eviction cost users resent, so they survive navigation and
  reload.
- **Unavailable environments** render the same explicit states as the Git
  Manager (disconnected, pending, unsupported capability) and never dial an
  environment the user disconnected.

## 5. Host identification

The hosted repository is identified from the selected checkout's `origin`
remote on the server:

1. `provider_from_remote` (existing) classifies `github.com`, any host
   containing `gitlab`, `dev.azure.com` / `visualstudio.com`, and `bitbucket`.
2. When that yields Unknown, the host is matched against the hosts the CLIs
   are configured for: `gh auth status --json hosts` (already parsed) and
   `glab auth status` (already parsed per host). A match assigns the
   provider. This is what makes a company GitLab at `git.acme.example` or a
   GitHub Enterprise host work without new configuration.
3. Still unknown → the module is unavailable for that project with the
   reason "`<host>` is not a configured GitHub or GitLab host on
   `<environment>`. Run `gh auth login --hostname <host>` or `glab auth login
   --hostname <host>` there, then rescan." Rescan re-runs the context read.

Every provider command carries the host explicitly (`GH_HOST=<host>` for
`gh`; `GITLAB_HOST=<host>` plus `--hostname <host>` on `glab api`) and the
repository explicitly (`--repo <host>/<owner>/<name>` / `--repo
<host>/<path>`), so multi-host setups and mis-detected default hosts cannot
route a request to the wrong instance. The owner/name path is parsed from
the remote URL with the existing `remote_host` helper extended for the path.

The GitLab adapter reads the instance version once per context
(`glab api version`) and declares version-gated capabilities (§ 7.3) from
it: request changes (≥ 17.2), remove own change request (≥ 17.8). GitHub Enterprise versions are not gated: every used field
exists on all supported GHES releases, and an unknown field returns a
structured "not supported by this host" error rather than a crash.

## 6. Availability and settings

Availability is layered; each layer answers with a reason the client shows.

### 6.1 Setting

A client setting `pullRequestsEnabled` (default **on**) rendered as a
`Switch` row "Pull requests" under **Settings → Source Control**, above the
provider discovery rows. Off hides the sidebar button and makes the routes
render "Pull requests are turned off in Settings → Source Control" with a
link. It is a client setting because it is a preference about what the UI
shows, not a property of any one environment; it applies to every environment
at once, like the other client settings (decision D2).

The Source Control discovery rows for GitHub and GitLab list every configured
host with its account (GitHub already reports hosts; GitLab's parsed host
blocks are surfaced the same way), so the user can see which hosts the module
will recognise.

### 6.2 Environment capability

Two new default-false flags on `ExecutionEnvironmentCapabilities`:
`pullRequestsReads` and `pullRequestsMutations`, advertised `true` by this
server, populated in the two `control.rs` sites and the shared test helper.
An older environment degrades to "This environment does not support Pull
Requests" (reads) or read-only (mutations), mirroring
`gitManagerAvailability.ts`.

### 6.3 Per-project context

`pullRequests.getContext` runs on open and on Rescan, never on a timer, and
returns one of:

- `available` with: provider (`github` | `gitlab`), host, host version
  (GitLab), repository `owner/name` (or GitLab full path), the authenticated
  account, the viewer's repository permission (GitHub `viewerPermission` +
  `permissions{admin,maintain,push,triage,pull}`; GitLab
  `permissions.project_access` / `group_access` access level), the
  repository's merge policy (allowed methods, delete-branch default,
  auto-merge allowed; GitLab `merge_method`, `squash_option`,
  `only_allow_merge_if_pipeline_succeeds`,
  `only_allow_merge_if_all_discussions_are_resolved`), the default branch,
  and the host capability set (§ 7.3). Picker vocabularies (labels,
  milestones, assignable users / eligible approvers, branches) load lazily
  through `pullRequests.getVocabulary` on first use of a picker.
- `unavailable` with a `code` and a server-authored `message`: no `origin`
  remote; host is Azure DevOps or Bitbucket; host not a configured GitHub or
  GitLab host (§ 5); CLI not installed on this environment (with the install
  hint discovery already carries); CLI not authenticated for this host (with
  the exact `auth login --hostname` command); repository not reachable with
  this account (404 / 403 from the host); CLI version too old for a required
  flag.

The context is cached client-side per project for the session and refreshed
by Rescan or after an authentication error.

## 7. Permissions

### 7.1 Shape

The server computes, per pull request, a `PullRequestsPermissions` object
whose every field is `{ allowed: boolean, reason: string | null }`:
`comment`, `editOwnComment`, `deleteOwnComment`, `minimizeComment`, `react`,
`resolveThreads`, `review` (submit any review), `approve`,
`requestChanges`, `revokeApproval`, `removeOwnChangeRequest`,
`dismissReview`, `rerequestReview`, `applySuggestion`, `editPullRequest`
(title, description, base branch), `editReviewers`, `editAssignees`,
`editLabels`, `editMilestone`, `lock`, `unlock`, `updateBranch`, `merge`,
`mergeBypass` (GitHub admin merge / GitLab override requested changes),
`enableAutoMerge`, `disableAutoMerge`, `markReady`, `convertToDraft`,
`close`, `reopen`, `delete`, `revert`, `checkout`. `merge` additionally
carries `methods` (the subset of merge, squash, rebase the repository allows
and the PR state permits) and `deleteBranchDefault`; `updateBranch` carries
`methods` (`merge`, `rebase`).

### 7.2 Inputs and rules

| Signal                                                 | GitHub                                                                                                                                                                                                                                                                                                                                                | GitLab                                                                                                                                                                                                                           |
| ------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Viewer identity                                        | `gh api user` (`login`)                                                                                                                                                                                                                                                                                                                               | `glab api user` (`username`, `id`)                                                                                                                                                                                               |
| Repository permission                                  | REST `permissions`, GraphQL `viewerPermission`                                                                                                                                                                                                                                                                                                        | project `permissions.project_access.access_level` / `group_access` (10 guest … 50 owner)                                                                                                                                         |
| PR-level host answers                                  | `viewerCanUpdate`, `viewerCanMergeAsAdmin`, `viewerCanEnableAutoMerge`, `viewerCanDisableAutoMerge`, `viewerCanApplySuggestion`, `viewerDidAuthor`, `mergeStateStatus`, `mergeable`, `reviewDecision`, `isDraft`, `state`, `locked`, `maintainerCanModify`, `reviewThreads[].viewerCanResolve`, `baseRef.refUpdateRule.viewerAllowedToDismissReviews` | `user.can_merge`, `detailed_merge_status`, `has_conflicts`, `blocking_discussions_resolved`, `draft`, `state`, `discussion_locked`, reviewers `[].state`, approvals `user_can_approve` / `user_has_approved` / `approvals_left`  |
| Comment, react, edit or delete own comment             | read access, not locked (locked: only `push`)                                                                                                                                                                                                                                                                                                         | read access, not locked; own notes only                                                                                                                                                                                          |
| Minimize comment                                       | `push` (GitHub only)                                                                                                                                                                                                                                                                                                                                  | not offered (host has no equivalent)                                                                                                                                                                                             |
| Approve                                                | not the author, PR open, not locked                                                                                                                                                                                                                                                                                                                   | `user_can_approve`                                                                                                                                                                                                               |
| Request changes                                        | same as Approve                                                                                                                                                                                                                                                                                                                                       | a listed reviewer, host ≥ 17.2, PR open                                                                                                                                                                                          |
| Revoke approval / remove change request                | not offered (GitHub has no self-revoke)                                                                                                                                                                                                                                                                                                               | `user_has_approved` / own state is `requested_changes` and host ≥ 17.8                                                                                                                                                           |
| Dismiss review                                         | `push` or `viewerAllowedToDismissReviews`, review state APPROVED or CHANGES_REQUESTED                                                                                                                                                                                                                                                                 | not offered (GitLab has no dismissal; reset approvals is bot-only)                                                                                                                                                               |
| Re-request review                                      | `viewerCanUpdate`, reviewer has reviewed                                                                                                                                                                                                                                                                                                              | author or developer+, reviewer state not `unreviewed`                                                                                                                                                                            |
| Apply suggestion                                       | never (no public API; reason says so)                                                                                                                                                                                                                                                                                                                 | developer+ and suggestion `applicable`                                                                                                                                                                                           |
| Edit PR, reviewers, assignees, labels, milestone, base | `viewerCanUpdate` (labels/milestone also need `triage`)                                                                                                                                                                                                                                                                                               | author or developer+ (labels/milestone: reporter+)                                                                                                                                                                               |
| Lock / unlock                                          | `push`                                                                                                                                                                                                                                                                                                                                                | developer+                                                                                                                                                                                                                       |
| Update branch                                          | `viewerCanUpdate`, `mergeStateStatus == BEHIND` (or `mergeable` and behind by commits), not cross-repo without `maintainerCanModify`                                                                                                                                                                                                                  | `detailed_merge_status == need_rebase` and the viewer can push to the source branch                                                                                                                                              |
| Merge                                                  | `push` permission or `viewerCanMergeAsAdmin`, PR open, not draft, `mergeStateStatus` in CLEAN / HAS_HOOKS / UNSTABLE (BLOCKED, BEHIND, DIRTY, DRAFT carry their reason)                                                                                                                                                                               | `user.can_merge` and `detailed_merge_status == mergeable` (every other status becomes the reason: `not_approved`, `ci_must_pass`, `conflict`, `discussions_not_resolved`, `draft_status`, `need_rebase`, `requested_changes`, …) |
| Merge bypass                                           | `viewerCanMergeAsAdmin` and `mergeStateStatus == BLOCKED`                                                                                                                                                                                                                                                                                             | `user.can_merge` and `detailed_merge_status == requested_changes` (GraphQL `overrideRequestedChanges`)                                                                                                                           |
| Auto-merge                                             | `viewerCanEnableAutoMerge` / `viewerCanDisableAutoMerge`                                                                                                                                                                                                                                                                                              | `user.can_merge`, pipeline running, not already set / already set                                                                                                                                                                |
| Ready / draft, close, reopen                           | `viewerCanUpdate`                                                                                                                                                                                                                                                                                                                                     | author or developer+                                                                                                                                                                                                             |
| Delete                                                 | never (GitHub cannot delete PRs)                                                                                                                                                                                                                                                                                                                      | owner, or maintainer where the project allows it (host answer surfaced)                                                                                                                                                          |
| Revert                                                 | `push`, PR merged, merge commit known                                                                                                                                                                                                                                                                                                                 | developer+, MR merged, merge commit known                                                                                                                                                                                        |
| Checkout                                               | always when the module is available; the Git Manager guard table governs the target checkout                                                                                                                                                                                                                                                          | same                                                                                                                                                                                                                             |

Reasons are short, specific, and actionable ("You cannot approve your own
pull request", "Merging is blocked: 1 approving review required", "Squash
merging is not allowed in this repository", "Your `glab` account has Reporter
access; merging needs Developer or higher", "This GitLab instance is 16.11;
requesting changes needs 17.2"). Permissions are advisory pre-checks; the
host's refusal wins and is shown as an error on the action.

### 7.3 Host capabilities

Each adapter declares a static-plus-version capability set the client uses
only for vocabulary and control visibility, never for permission:
`vocabulary` (pull request / merge request, Checks / Pipelines, Files changed
/ Changes), `requestChanges`, `revokeApproval`, `removeOwnChangeRequest`,
`dismissReview`, `applySuggestion`, `minimizeComment`, `deletePullRequest`,
`lockReasons`, `updateBranchMethods`, `mergeMethodsSource` (per-PR choice on
GitHub; project setting on GitLab), `autoMergeLabel` ("Enable auto-merge" /
"Merge when pipeline succeeds"), `reviewerStates`, `closedTabIncludesMerged`.

## 8. Views

Copy and iconography follow the host through the vocabulary capability.

### 8.1 List

- **Header**: provider icon, `owner/name`, host when not the default host,
  authenticated account, Refresh, Rescan (context), **New pull request** →
  the existing Git Manager create dialog, worktree selector.
- **Tabs**: GitHub Open / Closed (closed includes merged, with a merged
  marker on rows, as github.com does); GitLab Open / Merged / Closed / All.
  Counts appear when the host provides them cheaply (GraphQL `totalCount`;
  GitLab `x-total` header via `glab api -i`).
- **Filters**: text search (title/body), author (Anyone / Me / a login),
  assignee, reviewer (GitLab natively, GitHub via search qualifiers), review
  status (GitHub: review required, approved, changes requested; GitLab:
  approved / not approved), draft (all / drafts / ready), labels, milestone,
  target branch, sort (newest, oldest, recently updated, most commented).
- **Rows** (GitHub): state icon (open green, draft grey, merged purple,
  closed red), title, labels, `#N opened <relative time> by <login>`,
  review-decision text, checks summary icon (pass / fail / pending), comment
  count, "Draft" badge. Rows (GitLab): title, labels, `!N · created <time>
  by <name>`, pipeline status icon, approvals `x of y`, discussions
  resolved/unresolved count, updated time. Never an avatar.
- **Loading**: GitHub via GraphQL `pullRequests(first: 30, after: cursor,
  states, orderBy)` with the same field set as the list rows; GitLab via
  `glab mr list -F json --per-page 30 --page N` (open is the CLI default; `--closed`, `--merged`, `--all` select the other tabs — glab 1.114 has no `--state` flag). "Load more" appends; Refresh
  reloads the first page. Text search on GitHub uses the search qualifiers
  route (`gh pr list --search`) and the UI notes it is rate-limited
  separately.
- **Empty and error states**: "No open pull requests", "Nothing matches
  these filters" with a clear-filters action, and provider errors with the
  host's message and a Retry.

### 8.2 Detail

- **Header**: title (editable in place when `editPullRequest`), `#N` /
  `!N`, state pill, "`author` wants to merge N commits into `base` from
  `head`" (GitLab: "requested to merge `source` into `target`"), base branch
  editable when allowed, fork marker when cross-repository, Open in browser,
  Checkout split button (§ 8.5), Refresh, and an overflow menu with Mark
  ready / Convert to draft, Lock / Unlock (GitHub asks for the reason),
  Close / Reopen, Revert (merged only), Delete (GitLab, destructive
  confirmation), Copy URL.
- **Tabs with counts**: Conversation, Commits, Checks / Pipelines, Files
  changed / Changes; the active tab is in the URL.
- **Conversation**: the description rendered as GitHub-flavoured markdown
  (`react-markdown` + `remark-gfm` with the existing sanitiser) with Edit
  when allowed; then the timeline in submission order: comments (edit /
  delete own, minimize on GitHub, reactions bar), reviews (state, body,
  Dismiss with message on GitHub), review threads (file path, line,
  resolved/outdated markers, replies, Resolve / Unresolve, suggestion blocks
  with Apply on GitLab), and system events the host reports (labelled,
  assigned, review requested, closed, reopened, marked ready, merged).
  Every remote image is replaced by a labelled link ("image: <alt> — open in
  browser"); nothing is fetched. A comment box with markdown preview sits at
  the end; the review submission popover (§ 8.3) lives in the header of the
  Files tab and mirrors here.
- **Side column** (editable where permitted, each with its own reason when
  not): reviewers with their latest review state and a Re-request action,
  assignees, labels, milestone, linked issues / closing references, and for
  GitLab the approval rules with `x of y`. Pickers search the vocabularies
  (§ 6.3); labels show host colours; changes apply immediately with an undo
  toast for five seconds (UI.md: recoverable).
- **Commits**: sha, subject, author, date, per commit; clicking one opens the
  host page. Where the commit exists in the selected checkout after a
  Checkout, "Open in Git Manager" appears.
- **Checks / Pipelines**: GitHub `statusCheckRollup` folded exactly as the
  existing `checks.rs` does, grouped by workflow, with state, duration and
  link; GitLab head pipeline with its jobs (`/pipelines/:id/jobs`) grouped by
  stage with status and link. No auto-refresh; Refresh re-reads.
- **Files changed**: file tree with additions/deletions, "Viewed" checkbox
  per file (client state), whitespace toggle, and per-file diffs rendered by
  `@pierre/diffs` `FileDiff` from the host's patch (`gh pr diff --patch`;
  GitLab `/merge_requests/:iid/diffs` paginated), with the Git Manager size
  ladder (parse cap, "show diff anyway" gate, long-line degradation,
  highlight cap). Existing review comments anchor to their lines; clicking or
  dragging in the line gutter adds a pending inline comment (single or
  multi-line) with an "insert suggestion" helper that wraps the selected
  lines in a suggestion block.
- **Merge box** (bottom of Conversation, GitHub placement): one of the
  host's states with its explanation — mergeable, checks pending or failing
  (with count), reviews required (with count and who), changes requested
  (with who), conflicts (with "resolve locally" pointing to Checkout),
  behind base (GitHub, with Update branch: merge or rebase) / needs rebase
  (GitLab, with Rebase and skip-CI), draft, blocked by rules or merge train,
  already merged (with Revert), closed. Controls: merge method (only allowed
  methods; default = repository default), subject and body (prefilled from
  the host's convention), "Delete branch after merge" (default from
  repository setting), "Enable auto-merge" / "Merge when pipeline succeeds"
  and its Disable counterpart when the host allows it, the primary Merge
  button, and the explicit secondary bypass action ("Merge without waiting
  for requirements" for GitHub admins; "Merge despite requested changes" on
  GitLab) that states what it bypasses and requires confirmation. Every
  disabled control explains itself.

### 8.3 Reviewing

Inline comments are collected client-side as a pending review shown as a
counter ("Review 3 pending comments"). Submitting opens the popover: body,
and Comment / Approve / Request changes (GitLab additionally: Revoke approval
when already approved; Remove my change request when the viewer's state is
`requested_changes`). Submission is one server call that reports partial
failure precisely (which comments landed, which did not; the rest stays
pending; nothing is silently dropped):

- GitHub: `POST repos/{o}/{r}/pulls/{n}/reviews` with `event`, `body`,
  `commit_id`, and `comments[] { path, line, side, start_line, start_side,
  body }` via `gh api --input -` (the `gh pr review` command cannot carry
  inline comments).
- GitLab: each inline comment via `POST …/merge_requests/:iid/discussions`
  with `position` (`base_sha`, `start_sha`, `head_sha`, `position_type:
  text`, `new_path`, `old_path`, `new_line` / `old_line`, `line_range` for
  multi-line; the three SHAs come from `…/merge_requests/:iid/versions`),
  the general body via `POST …/notes`, then `glab mr approve --sha <head>` /
  GraphQL `mergeRequestRequestChanges`.

Replies use `pulls/{n}/comments/{id}/replies` / `discussions/:id/notes`;
resolve uses GraphQL `resolveReviewThread` / `unresolveReviewThread` and
GitLab `PUT discussions/:id` `resolved`; edit and delete use
`PATCH/DELETE pulls/comments/{id}` and `issues/comments/{id}` / `PUT/DELETE
discussions/:id/notes/:note_id`; dismissal uses `PUT
pulls/{n}/reviews/{id}/dismissals` with the message; re-request uses `gh pr
edit --add-reviewer` / GraphQL `mergeRequestReviewerRereview`; suggestions
apply with `PUT /suggestions/:id/apply` or `batch_apply` with an optional
commit message; reactions use REST reactions (`+1 -1 laugh confused heart
hooray rocket eyes`) / `award_emoji`.

### 8.4 Editing the pull request

Title and description edit in place (markdown editor with preview) and save
through `gh pr edit --title / --body-file -` or GitLab `PUT
…/merge_requests/:iid` (`title`, `description`). Reviewers, assignees,
labels and milestone save per change through `gh pr edit --add-… /
--remove-…` and GitLab `add_labels` / `remove_labels` / `assignee_ids` /
`reviewer_ids` / `milestone_id`. The base branch picker lists the
repository's branches (`gh api repos/{o}/{r}/branches` / `projects/:id/
repository/branches`) and saves with `--base` / `target_branch`. Lock and
unlock use `gh pr lock --reason` / `gh pr unlock` and `discussion_locked`.
Every edit re-reads the detail afterwards.

### 8.5 Checkout for local verification

Split button "Checkout" with: **Current checkout** (the selected checkout),
**Another worktree…** (picker of the project's worktrees), **New worktree…**
(the existing create-worktree dialog, pre-filled).

- Local checkout runs `gh pr checkout <n> [--branch <name>]` / `glab mr
  checkout <n> -b <name>` in the target checkout's `cwd`; forks work without
  adding remotes because both CLIs fetch the PR head ref. Before running, the
  server evaluates the Git Manager guards (dirty tree, branch held by another
  worktree → redirect to that worktree, operation in progress) and returns the
  blocked reason instead of failing mid-way.
- New worktree: the server fetches `refs/pull/<n>/head` (GitHub) or
  `refs/merge-requests/<n>/head` (GitLab) from `origin` into a local branch
  named after the head branch (suffix `-pr-<n>` on collision), then calls the
  existing managed-worktree creation path; the result opens the worktree as
  the Git Manager's "New worktree" does today.
- After a successful checkout the view shows where the branch now lives and
  offers "Open Git Manager there".

### 8.6 Revert and delete

Revert (merged PRs only) runs `gh pr revert <n>` on GitHub, which creates the
revert branch and pull request; on GitLab the server runs `POST
/projects/:id/repository/commits/:merge_commit_sha/revert` with a new
`branch` named `revert-<n>-<short sha>` from the target branch, then creates
the merge request through the existing `create` path. Both show the new pull
request and navigate to it. Delete (GitLab only) runs `glab mr delete <n>`
behind a confirmation that names the MR and states that deletion is
permanent on the host.

## 9. Behaviour decisions

- **Refresh model.** Provider reads happen on route open, on Refresh, on
  Rescan, and immediately after a successful mutation (the affected read is
  re-fetched). Nothing runs on a timer or on window focus.
- **Optimistic concurrency on merge and approve.** Merge and approval carry
  the head SHA the user was looking at (`--match-head-commit` / `--sha`); a
  changed head turns into "The pull request changed since you loaded it —
  refresh and review the new commits", never a merge of unseen commits.
- **Draft preservation.** Comment text, comment edits, pending inline
  comments, title/description edits, and merge subject/body persist per pull
  request in the view-state cache until sent or explicitly discarded; a
  failed submission keeps them.
- **Undo for metadata edits.** Reviewer, assignee, label and milestone
  changes apply immediately and show a five-second undo toast that reverses
  the exact change; destructive actions (delete MR, dismiss review, delete
  comment, revert) confirm first and state what happens on the host.
- **Errors are actionable.** Every provider error is a structured
  `PullRequestsOperationError { code, message, host detail, retryable }`
  authored on the server: not authenticated (with the login command), not
  found, forbidden (with the permission the host expects), rate limited
  (with the reset time when the host reports it), CLI missing or too old,
  host version too old, timeout, host unreachable, stale head. Raw stderr is
  never shown.
- **Identity rendering.** Logins and names from the host's payload only,
  with initials; no avatar URL is ever loaded (constraint 3).
- **Locked conversations, merged and closed PRs** show their state and
  disable the actions the host disables, with the reason.
- **Existing Git Manager pane.** The Git Manager toolbar's "Show pull
  requests" pane stays what it is, a current-branch summary with checks, and
  gains one link, "Open in Pull Requests", into this module's detail route.
  Its `gitManager.listPullRequests` RPC is unchanged; the new module does not
  reuse it because it lists one branch's PR, not the repository's (decision
  D8).

## 10. Provider command inventory

Per operation, the exact host call the server makes; each runs through
`ProcessRunner` with `GH_HOST` / `GITLAB_HOST` set, `--hostname` on `glab
api`, and `--repo` pinned. `…` abbreviates `projects/:path/merge_requests/:iid`.

| Operation                                  | GitHub (`gh`)                                                                                                                                                                        | GitLab (`glab`)                                                                                                            |
| ------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------- |
| Context: identity                          | `gh api user`                                                                                                                                                                        | `glab api user`                                                                                                            |
| Context: repository                        | `gh api repos/{o}/{r}` + GraphQL `repository { viewerPermission viewerCanAdminister mergeCommitAllowed squashMergeAllowed rebaseMergeAllowed autoMergeAllowed deleteBranchOnMerge }` | `glab api projects/:path` + `glab api version`                                                                             |
| Context: auth                              | `gh auth status --hostname <host> --json hosts`                                                                                                                                      | `glab auth status --hostname <host>`                                                                                       |
| Vocabularies (lazy)                        | `gh api repos/{o}/{r}/labels`, `…/milestones`, `…/collaborators`, `…/branches`                                                                                                       | `glab api projects/:path/labels`, `…/milestones`, `…/members/all`, `…/repository/branches`, `glab mr approvers -F json`    |
| List                                       | `gh api graphql` `repository.pullRequests(first:30, after, states, orderBy)` with row fields; search filter via `gh pr list --search … --json`                                       | `glab mr list -F json --per-page 30 --page N` + filters; `glab api -i` for totals                                          |
| Detail                                     | `gh pr view <n> --json <fields without comments>` + GraphQL viewer/threads/branch-rule query                                                                                         | `glab api …` + `…/approvals` + `…/reviewers`                                                                               |
| Timeline                                   | `gh pr view <n> --json comments,reviews` + REST `pulls/{n}/comments` + GraphQL `reviewThreads` + REST `issues/{n}/timeline`                                                          | `glab api …/discussions` (paginated) + `…/resource_state_events`, `…/resource_label_events`, `…/resource_milestone_events` |
| Commits                                    | `gh pr view <n> --json commits`                                                                                                                                                      | `glab api …/commits`                                                                                                       |
| Checks / pipelines                         | `gh pr view <n> --json statusCheckRollup` (existing fold)                                                                                                                            | `glab api …/pipelines` + `…/pipelines/:id/jobs`                                                                            |
| Files + diff                               | `gh pr view <n> --json files` + `gh pr diff <n> --patch`                                                                                                                             | `glab api …/diffs?per_page=50` + `…/versions`                                                                              |
| Comment                                    | `gh pr comment <n> --body-file -`                                                                                                                                                    | `glab api -X POST …/notes --input <private file>`                                                                          |
| Edit / delete comment                      | REST `PATCH/DELETE issues/comments/{id}`, `pulls/comments/{id}`                                                                                                                      | `PUT/DELETE …/discussions/:id/notes/:note_id`                                                                              |
| Minimize comment                           | GraphQL `minimizeComment` / `unminimizeComment`                                                                                                                                      | —                                                                                                                          |
| Reply / resolve                            | REST `pulls/{n}/comments/{id}/replies`; GraphQL `resolveReviewThread` / `unresolveReviewThread`                                                                                      | `POST …/discussions/:id/notes`; `PUT …/discussions/:id` `resolved`                                                         |
| Submit review                              | REST `POST pulls/{n}/reviews` `{ event, body, commit_id, comments[] }` via `gh api --input -`                                                                                        | `POST …/discussions` with `position`; `POST …/notes`; `glab mr approve <n> --sha`; GraphQL `mergeRequestRequestChanges`    |
| Revoke / remove change request             | —                                                                                                                                                                                    | `glab mr revoke <n>`; GraphQL `mergeRequestDestroyRequestedChanges`                                                        |
| Dismiss review                             | REST `PUT pulls/{n}/reviews/{id}/dismissals` `{ message }`                                                                                                                           | —                                                                                                                          |
| Re-request review                          | `gh pr edit <n> --add-reviewer <login>`                                                                                                                                              | GraphQL `mergeRequestReviewerRereview(userId)`                                                                             |
| Apply suggestion                           | — (unavailable, reason shown)                                                                                                                                                        | `PUT /suggestions/:id/apply`, `PUT /suggestions/batch_apply`                                                               |
| Reactions                                  | REST `POST/DELETE issues/{n}/reactions`, `issues/comments/{id}/reactions`, `pulls/comments/{id}/reactions`                                                                           | `POST/DELETE …/award_emoji`, `…/notes/:note_id/award_emoji`                                                                |
| Edit title / body / base                   | `gh pr edit <n> --title / --body-file - / --base`                                                                                                                                    | `PUT …` `{ title, description, target_branch }`                                                                            |
| Reviewers / assignees / labels / milestone | `gh pr edit <n> --add-reviewer … --remove-assignee … --add-label … --milestone … --remove-milestone`                                                                                 | `PUT …` `{ reviewer_ids, assignee_ids, add_labels, remove_labels, milestone_id }`                                          |
| Lock / unlock                              | `gh pr lock <n> --reason <r>` / `gh pr unlock <n>`                                                                                                                                   | `PUT …` `{ discussion_locked }`                                                                                            |
| Update branch                              | `gh pr update-branch <n> [--rebase]`                                                                                                                                                 | `glab mr rebase <n> [--skip-ci]`                                                                                           |
| Merge                                      | `gh pr merge <n> --merge                                                                                                                                                             | --squash                                                                                                                   | --rebase [--delete-branch] [--auto] [--admin] --match-head-commit <sha> [-t -F -]` | `glab mr merge <n> [--squash] [--remove-source-branch] [--auto-merge] [--rebase] --sha <sha> -y [-m]`; bypass via GraphQL `mergeRequestUpdate(overrideRequestedChanges: true)` then merge |
| Disable auto-merge                         | `gh pr merge <n> --disable-auto`                                                                                                                                                     | `glab api -X POST …/cancel_merge_when_pipeline_succeeds`                                                                   |
| Ready / draft                              | `gh pr ready <n>` / `gh pr ready <n> --undo`                                                                                                                                         | `glab mr update <n> --ready` / `--draft`                                                                                   |
| Close / reopen                             | `gh pr close <n>` / `gh pr reopen <n>`                                                                                                                                               | `glab mr close <n>` / `glab mr reopen <n>`                                                                                 |
| Delete                                     | —                                                                                                                                                                                    | `glab mr delete <n>`                                                                                                       |
| Revert                                     | `gh pr revert <n>`                                                                                                                                                                   | `POST projects/:path/repository/commits/:sha/revert` `{ branch }` + existing create path                                   |
| Checkout                                   | `gh pr checkout <n> [--branch]` in the target cwd                                                                                                                                    | `glab mr checkout <n> -b <name>` in the target cwd                                                                         |
| Worktree fetch                             | `git fetch origin refs/pull/<n>/head:<branch>`                                                                                                                                       | `git fetch origin refs/merge-requests/<n>/head:<branch>`                                                                   |

Bodies never appear in `argv`: `gh` reads them from stdin (`--body-file -`,
`gh api --input -`); `glab api` reads them with `--input <file>` from a
private (0600) temporary file in the server's state directory that is
deleted after the call (the installed glab 1.114 documents `@-` stdin only
for `--form`; the plan verifies stdin support and prefers it when present).
`glab api --hostname <host>` and `-i` (response headers, for `x-total`) are
confirmed on the installed version; `glab mr note` is not used because its
`create` subcommand is marked experimental.

## 11. Performance and limits

- Timeouts: reads 30 s, list and diff reads 60 s, mutations 60 s; one
  overall deadline per RPC that covers every child call it makes.
- Output caps: 1 MiB for list/detail/timeline JSON, 8 MiB for a patch
  (`OutputPolicy::Error` → structured "too large" error that still returns
  the file list, so the user can open the host page). The Git Manager diff
  ladder applies to rendering.
- Page sizes: 30 rows per list page, 50 diffs per GitLab diff page, 100
  discussions / comments per page with a 10-page cap, vocabularies capped at
  200 entries with server-side search beyond that.
- Concurrency: one unary read per tab (`get`, `getTimeline`, `getChecks`,
  `getFiles`, `getCommits`), each a query atom the client mounts when the tab
  is shown, matching the Git Manager's query-atom pattern; the detail header
  and permissions come from `get` alone so the page paints before the other
  tabs load. Mutations are unary and serialised per pull request through
  the existing VCS command scheduler.
- Client: list virtualised with fixed row heights; markdown rendering
  memoised per comment id + updated timestamp; diffs rendered lazily per
  file when scrolled into view.

## 12. Zero-telemetry invariant

Testable, as for the Git Manager: a web test mirroring
`gitManagerTelemetry.test.tsx` asserts no `fetch` and no `Image` construction
on render and on idle, exactly one provider dispatch per explicit action, and
that markdown with remote images renders a link and no `<img>`; a server test
mirroring `explicit_checks_handler_is_the_only_non_git_process_surface`
asserts the module spawns only `gh`, `glab`, and `git`, and only from a
request handler; a source tripwire asserts no timer or poller in the module's
server paths.

## 13. Delivery shape and execution model

Vertical slices, each leaving the application shippable:

0. Contracts, capability flags, scopes, wire fixtures, method registration
   tripwires.
1. Server: host identification, context and availability, vocabularies,
   list, for GitHub and GitLab, with fixture-script tests for both.
2. Web: routes, sidebar button, view-state store, settings switch, list view
   with all availability states.
3. Server: detail, permissions, timeline, commits, checks/pipelines, files
   and diffs.
4. Web: detail view, read-only, all four tabs, side column, merge box states.
5. Server: conversation and review mutations (comment, edit, delete,
   minimize, react, reply, resolve, submit review, approve, request changes,
   revoke, dismiss, re-request, apply suggestion).
6. Web: comment box, review flow, inline comments and suggestions, reactions.
7. Server: pull-request editing, lock, update branch, merge, auto-merge,
   ready/draft, close/reopen, delete, revert.
8. Web: editable header and side column, merge box controls, overflow
   actions, confirmations and undo.
9. Checkout: local and new-worktree, guards, "Open Git Manager there".
10. Docs (workspace-ui, source-control-providers, rpc-and-orchestration,
    architecture overview, testing runbooks), telemetry tripwires, full
    verification.

Execution follows the requester's delegation rule: each phase is implemented
by Codex through `codex:rescue` from its phase file; the coordinator (this
session) reviews every diff against the spec, runs the gates (`vp check`,
`vp run typecheck`, `cargo fmt --all --check`, Clippy with warnings denied,
the affected Rust and package tests), reviews React changes with
`vercel-react-best-practices` and the UI against `UI.md`, and verifies each
web phase visually with Playwright before accepting it. No phase is
"complete" on Codex's word alone.

## 14. Validation

- Unit and integration tests per phase, in the existing files' style
  (`TestSandbox::executable_script` stubs for `gh` and `glab`, recorded
  JSON from the live-verification addendum as fixtures).
- Contract parity fixtures regenerated and counts bumped.
- **GitHub end to end**: Playwright against the web app with the
  `mubeda/BibCode` repository (13 pull requests exist), covering list,
  detail, files, a comment, an inline review, a label edit with undo, a
  checkout into a worktree on a throwaway PR, and the permission reasons on
  a read-only repository (`openai/codex`, where the viewer has READ).
- **GitLab end to end**: cannot run on the development workstation (`glab`
  has no authenticated host there). The requester runs the same script
  against the company GitLab server, or provides a gitlab.com project token
  for a throwaway project; the plan marks this phase incomplete until one of
  the two happens. Fixture tests cover the GitLab adapter in the meantime.
- Desktop e2e: the packaged-app spec that clicks "Git Manager for …" gains a
  sibling clicking "Pull Requests for …" and asserting the route.

## 15. Approaches considered

1. **A tab inside the Git Manager.** Smallest wiring; but the Git Manager is
   a local-repository workbench with a live status signal, while PRs are a
   hosted, on-demand surface with its own list/detail navigation and
   reload-safe deep links. Mixing them would force provider state into the
   Git Manager store and blur the zero-polling boundary. Rejected.
2. **A right-panel surface** (like Source Control or Files). Reachable from
   any thread, but per-thread state does not fit a repository-level list, and
   a detail page with four tabs and a diff needs the centre width. Rejected.
3. **A sibling project route with its own sidebar button** (chosen). Mirrors
   the Git Manager's proven wiring point for point, gives reload-safe URLs
   for list and detail, keeps state per project, and matches the request
   literally.

For the transport: a generic "run this CLI command" RPC was rejected (the
client would derive policy and could run arbitrary commands); a REST-only
implementation using stored tokens was rejected (constraint 4, and the CLIs
already hold the credentials); the chosen shape is typed RPCs whose server
handlers call the CLIs and, where the CLIs lack an operation, the hosts'
public APIs through `gh api` / `glab api`.

## 16. Decision log

| #   | Decision                                                                                                                                               | Rationale                                                                                                                       | Status                                                    |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------- |
| D1  | Detail is a child route `/pull-requests/$number` with the tab in a search parameter                                                                    | Reload-safe, matches "the route encodes the panel", sidebar highlight becomes a prefix match                                    | approved                                                  |
| D2  | The on/off switch is a client setting under Settings → Source Control, default on                                                                      | A UI preference, applies to every environment; per-environment availability comes from § 6.3                                    | approved                                                  |
| D3  | Inline review comments, single and multi-line, ship in this version                                                                                    | The host's review model is the point of the module; both hosts expose it via API                                                | approved                                                  |
| D4  | Checkout into a new worktree ships in this version, via `refs/pull                                                                                     | merge-requests/<n>/head`                                                                                                        | Fork PRs need no remote; reuses the managed-worktree path | approved |
| D5  | Reviewers, assignees, labels, milestone, title, description and base branch are editable                                                               | Full functionality requested; both hosts expose the edits (`gh pr edit`, GitLab update endpoint)                                | approved (reversed from read-only)                        |
| D6  | GitHub and GitLab only; Azure and Bitbucket render unavailable                                                                                         | Requested scope; keeps the adapter trait honest with two real implementations                                                   | approved                                                  |
| D7  | The sidebar button appears for every project when the setting is on; the view explains availability                                                    | The client cannot know host reachability; hiding would hide the reason                                                          | approved                                                  |
| D8  | The Git Manager's provider pane stays and links into the module; no shared list RPC                                                                    | Different question (one branch vs the repository); avoids two sources of truth by keeping each narrow                           | approved                                                  |
| D9  | GitHub list paging via GraphQL cursors; GitLab via page numbers                                                                                        | `gh pr list --limit` has no cursor; GraphQL confirmed live                                                                      | approved                                                  |
| D10 | Merge and approve always pin the head SHA the user saw                                                                                                 | Prevents acting on unseen commits; both hosts support it                                                                        | approved                                                  |
| D11 | Remote images in markdown become links; no avatars                                                                                                     | Zero-telemetry constraint; `ChatMarkdown` today has no `img` override, so the module supplies one                               | approved                                                  |
| D12 | Bodies never in `argv` (stdin or private temp file), hosts via env / `--hostname`, repository via `--repo` on every call                               | Keeps text out of the process table; prevents wrong-host routing                                                                | approved                                                  |
| D13 | GitLab "Request changes" ships, through GraphQL `mergeRequestRequestChanges`, gated on host version ≥ 17.2; "remove my change request" gated on ≥ 17.8 | Full functionality requested; REST has no endpoint (gitlab-org/gitlab#451995 open) but GraphQL does, confirmed by introspection | approved (reversed from deferred)                         |
| D14 | GitHub suggestions render but cannot be applied; GitLab suggestions apply singly and in batch                                                          | GitHub has no public apply API (schema has no such mutation); GitLab documents `/suggestions/:id/apply`                         | approved                                                  |
| D15 | Reactions are read and toggled on the PR and on comments                                                                                               | Part of the host's page; REST reactions / award emoji exist on both hosts                                                       | approved                                                  |
| D16 | Review dismissal (GitHub), re-request review (both), comment edit/delete/minimize ship                                                                 | Part of the host's review model; public mutations confirmed                                                                     | approved                                                  |
| D17 | Revert ships (`gh pr revert`; GitLab commit revert + create MR); Delete ships for GitLab only behind confirmation                                      | Both appear on the hosts' PR pages; GitHub cannot delete PRs                                                                    | approved                                                  |
| D18 | Update branch ships (GitHub merge or rebase; GitLab rebase with optional skip-CI)                                                                      | Merge-box action on both hosts                                                                                                  | approved                                                  |
| D19 | GitLab to-dos, subscriptions, time tracking and GitHub Projects fields stay out                                                                        | Not pull-request management; each is a separate host feature                                                                    | approved                                                  |
| D20 | Only public, documented host APIs; nothing internal                                                                                                    | Self-hosted GitLab and GHES vary; internal endpoints break silently                                                             | approved                                                  |
