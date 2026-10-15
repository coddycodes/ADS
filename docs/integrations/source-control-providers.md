# Source Control Integrations

BiBCode uses Git for local repository operations and provider-specific tools for
hosted repository and pull-request operations. Authentication belongs on the
machine running the BiBCode server.

## Current capability matrix

| Operation                             | GitHub           | GitLab           | Bitbucket        | Azure DevOps     |
| ------------------------------------- | ---------------- | ---------------- | ---------------- | ---------------- |
| Clone a supplied Git URL              | Yes, through Git | Yes, through Git | Yes, through Git | Yes, through Git |
| Look up a repository by provider/name | Yes, `gh`        | Yes, `glab`      | No               | No               |
| Publish a local repository natively   | **Yes, `gh`**    | No               | No               | No               |
| Resolve the current PR/MR             | Yes              | Yes              | Yes              | Yes              |
| Read checks in Git Manager            | Yes, `gh`        | No               | No               | No               |
| Create a PR/MR                        | Yes              | Yes              | Yes              | Yes              |
| Open or prepare a PR branch locally   | Yes              | Yes              | Yes              | Yes              |

The dedicated **Pull Requests** module has this host matrix. “Yes” still
requires the action's server-authored permission and current host state; it is
not a promise that every authenticated account can write.

| Pull Requests operation                              | GitHub               | GitLab                                             | Bitbucket | Azure DevOps |
| ---------------------------------------------------- | -------------------- | -------------------------------------------------- | --------- | ------------ |
| List PRs / MRs                                       | Yes                  | Yes                                                | No        | No           |
| Review (comments, threads, reactions, inline review) | Yes                  | Yes                                                | No        | No           |
| Approve                                              | Yes; not your own PR | Yes; host eligibility                              | No        | No           |
| Request changes                                      | Yes                  | Yes; GitLab 17.2+ and listed reviewer              | No        | No           |
| Merge / auto-merge                                   | Yes                  | Yes; project merge method                          | No        | No           |
| Edit metadata                                        | Yes                  | Yes                                                | No        | No           |
| Checkout (current / other / new worktree)            | Yes                  | Yes                                                | No        | No           |
| Revert (creates a new request)                       | Yes; merged PR       | Yes; merged MR                                     | No        | No           |
| Delete                                               | No                   | Yes; owner grant                                   | No        | No           |
| Apply suggestion                                     | No; no public API    | Yes; applicable suggestion and source write access | No        | No           |
| Minimize comment / dismiss review                    | Yes                  | No                                                 | No        | No           |
| Revoke approval                                      | No                   | Yes                                                | No        | No           |
| Remove own change request                            | No                   | Yes; GitLab 17.8+                                  | No        | No           |

GitLab version-gated actions stay disabled with a reason if the server cannot
read the host version. Reviewer states themselves have no version gate.
GitLab's current public reads do not expose a custom maintainer-delete grant;
the server conservatively requires Owner access. Unsupported operations show
the host's reason. GitHub rejects auto-merge combined with bypass; GitLab
requires both permissions when both choices are selected. For GitLab merge
message writes, the API payload sends both `auto_merge` (renamed in 17.11) and
`merge_when_pipeline_succeeds` for older hosts.

“Clone from URL” is a generic Git clone. It does not require BiBCode to identify
the hosting provider, but the URL's normal SSH or HTTPS credentials must work on
the server.

Native repository publishing is implemented only for GitHub. The server rejects
publish requests for GitLab, Bitbucket, and Azure DevOps as unavailable, even if
a UI control happens to list those providers.

## Add an existing project

Open the Command Palette (`Cmd/Ctrl+K`) and choose **Add Project**. You can:

- browse to one existing project folder;
- clone a Git URL into a chosen destination; or
- create one new local Git repository.

Selecting a folder adds that folder as one project. The dialog does not scan a
parent directory for nested repositories or import multiple projects at once.

## Publish a local repository

GitHub publishing uses `gh repo create`, adds the selected remote, and pushes the
current branch. Install and authenticate GitHub CLI first, then use the publish
flow in the chat-header Git actions control:

```bash
gh auth login
```

The right-panel Source Control surface currently renders publish actions as
disabled; publishing is not wired there.

## Pull requests and merge requests

From the Git actions or Source Control UI, BiBCode can:

- detect an open PR/MR for the current branch;
- generate proposed title and description text;
- push and create a PR/MR;
- open the hosted review in a browser; and
- switch to the review branch or create a worktree for it.

Provider terminology follows the host: GitLab uses merge requests, while the
other supported hosts use pull requests.

The separate Pull Requests server RPC surface supports GitHub and GitLab
context, lazy picker vocabularies, paginated repository lists, and independent
detail, timeline, commits, checks/pipelines, and file reads. It resolves
the selected checkout's origin and supports custom hosts configured in the
provider CLI. Every request pins the host and repository. Its review mutations
support comments, reactions, threads and reviews, including GitHub review
dismissal/minimization and GitLab approval revocation, change-request removal
and suggestion application. The server also supports title/body/base edits,
reviewer/assignee/label/milestone changes, lock/unlock, branch updates,
merge/auto-merge, draft and open/closed state changes, revert creation, and
GitLab deletion. Checkout supports the selected checkout, another project worktree or a new managed worktree. Started Git writes continue after cancellation; reconnect and refresh to see the result. GitHub cannot delete
a pull request or apply suggestions through a public API; GitLab
multi-line review drafts currently post at the end line. Partial review errors
report comments already posted so a refresh can precede a retry. Permissions and merge
readiness are computed by the server, including host and version restrictions.
Merge pins the viewed head and validates the selected method and any bypass or
auto-merge permission before mutation. GitLab merge messages stay in a private
body file. Revert returns the created request, and a partial GitLab revert failure
states whether the branch or revert commit already exists so the user can inspect
the host before retrying. GitHub milestone picker IDs stay numeric and are
resolved to titles at edit time. Large and binary diffs retain file rows without text patches. Its dedicated
project-header module now provides the repository list, filters, explicit
pagination/refresh, context recovery, and reviewable detail tabs with server
readiness and permission reasons. Conversation and file lists are virtualized;
file patches render lazily and remote markdown images become browser links.
Comments, edits, replies, reactions, inline review, review decisions, dismissal,
re-request, and supported suggestion actions use the same typed command path.
Review receipts include `reviewPosted` plus failed path/line/body entries so
retries retain unsent work without repeating a posted summary. GitHub thread
comment reads carry authoritative minimization state. Request metadata editing
and merge are available with server permission reasons. Metadata edits offer five-second
Undo; merge uses the loaded head and confirms method, target, branch deletion and
auto/bypass choices. Draft/state/lock, revert and GitLab deletion are supported.
Checkout follows the guarded [worktree catalog lifecycle](../architecture/worktree-catalog.md); the existing pull-request creation dialog remains available. **Settings → Source Control** can disable the module and lists
every configured GitHub/GitLab host with its redacted account and authentication
status. Discovery retains the legacy primary-account summary and adds an
optional `auth.hosts` array without any additional provider probe. Existing Git
Manager and Source Control operations remain as described below.

Passive workspace summaries publish their fresh Git/provider base before the
optional PR lookup. While that lookup is pending or fails, a same-branch and
same-provider PR completed in the previous producer cycle may appear for one
cycle with its original observation time and `stale` state. It expires in the
following cycle unless the provider refreshes it; fresh local base fields are
not replaced by the prior whole summary.

The project-scoped Git Manager has a narrower on-demand provider pane. Its
current pull-request read supports GitHub, GitLab, and Azure DevOps; Bitbucket
returns unavailable on this surface even though the existing Source Control
integration can resolve and create Bitbucket pull requests elsewhere. Check
reads use `gh pr view <number> --json statusCheckRollup` and are available only
for GitHub in this pass. That command reports an open pull request without
checks as an empty collection with a zero exit, so the pane renders the pull
request with no check rows; `gh pr checks` cannot distinguish that case from a
missing pull request or rejected credentials, which all exit 1 with empty
output. Rollup entries fold the way `gh pr checks` folds them: newest run per
check-run name and workflow or per status context, with the state taken from
the context state, else the completed conclusion, else the run status.

Git Manager pull-request and check data refresh only on explicit user action,
never on a timer. Opening the pane or leaving it idle issues no provider call;
choosing **Refresh** invokes the environment-scoped RPC, whose server handler
runs the configured provider CLI when that provider is supported.

**Create pull request** in Git Manager, Pull Requests, Source Control, and the
chat Git actions opens the same review dialog. It reads local status and refs,
shows the detected provider and separate **Source branch** and **Target branch**
selectors. Both lists contain only origin-tracking branches; local-only branches,
other remotes, and symbolic `HEAD` entries are excluded. The source starts at the
current checkout's branch; if origin does not have it yet, creating the request
publishes it first. In Source Control, **Push & create** and **Create** first publish
a branch that has no upstream yet (`push --set-upstream origin <branch>`), so the
dialog opens with that branch already on origin; a failed push reports the error
and leaves the dialog closed. A branch that already tracks an upstream, which may
have another name or remote, a host that is not identified yet, a checkout with
uncommitted changes, and any server without branch-selection support open the
dialog directly; creating the request then publishes the source explicitly to
its same-named origin branch. Selecting another source never checks it out. Title and description suggestions
come from that source's tip commit, using a pinned commit query rather than the
repository-wide history. Changing source preserves edited content and replaces
only untouched suggestions. The target branch
starts unselected: the user must choose an option before creation is enabled.
Typing a name alone does not select it. The searchable list requests remote refs,
retains them even when a local branch has the same name, strips the origin prefix,
and excludes the source
branch unless the action will create a new feature branch. Fetch updates the
available remote branches. Status refreshes and retries preserve the selection;
reopening the dialog, changing repositories, or changing source branch requires
a new selection. Choosing `main`, `master`, or the repository's default branch
replaces the target hint with a red warning naming that branch; creation stays
available. Its wording follows the host: a
GitLab repository gets **Create merge request**, `!N` references and
"merge request" throughout; a missing provider keeps "pull request", while an
explicit unknown provider uses "change request". Until status has answered,
including after a failed read with no earlier answer, the Git Manager's
Show/Hide toggle, its provider pane and the dialog use the neutral "change
request", unless the Pull Requests panel supplied the host to the dialog.
Shared presentation also owns
the provider name and number prefix. When the
host is not identified yet, the dialog says so and names the two ways to
identify it (open Pull Requests for the project, or Rescan in **Settings →
Source Control**); without an `origin` remote it asks for one. Opened from Pull
Requests, the dialog shows the host that panel already identified while the
status has not named it yet, and leaves the provider check to the server. A
disabled primary button explains why on hover and to screen readers. Apart from
that Source Control push, nothing is pushed or created until
the dialog's primary action runs the existing `git.runStackedAction`
`create_pr` route with the reviewed `pullRequestTitle`, `pullRequestBody`, and
required `pullRequestBaseBranch`, plus `pullRequestHeadBranch` for the selected
source. A selected local source is pushed explicitly to the same-named branch on
origin without force or tags; a source that exists only on origin needs no push.
Other checkout changes stay untouched. Combined commit/push/request actions use
`commit_push_pr` through the same review; their source stays on the current or
newly generated branch, with that constraint explained beside the field.
The server rejects a missing, blank,
invalid, or identical source/target branch before mutations; it never falls back
to the repository default, `master`, or `main`. Both routes resolve the provider
before creating a branch, committing or pushing,
so an unidentified host fails with "Nothing was published." and the same
guidance. A request it creates makes the Pull Requests list read its GitLab
totals again.
On servers that advertise `pullRequestCreateOptions`, GitHub and GitLab
requests also take create-time options. **Mark as draft** sits under the title;
**Assignee** (with **Assign to me**), **Reviewer**, **Milestone** and
**Labels** follow the description and search the repository's members,
milestones and labels. GitLab takes one assignee and one reviewer; GitHub takes
several. GitLab adds **Delete source branch** and **Squash commits**, which
start from the project's settings; a project that always or never squashes
locks that box and says why. While those settings load, fail to load or go
unreported, untouched boxes show as indeterminate and leave the decision to the
project, so the request never contradicts what the dialog showed. A failed
picker or settings read offers **Retry** and never blocks creation. GitHub
applies some options after creating the request: if one of those steps fails,
the request is reported as created with a warning naming what was not applied,
and it is never retried. When an open request already exists for the branch,
it opens as before and, if the user chose any option, the dialog says the new
options were not applied; merge boxes filled from the project's settings alone
do not count.
Azure DevOps and Bitbucket keep the plain dialog; the server refuses options
for them before anything is published.
An older environment that does not advertise branch-selection support disables
creation with instructions to update its BiBCode server. The action also checks
the live connection's support before mutation, including after reconnect.
The dialog renders separate publishing/creation phases when the connected server
provides them. Native stacked actions currently emit start and terminal outcomes
without per-phase progress. A failed combined action rereads Git before Retry:
a clean tree resumes publication/request creation, an already-created feature
branch is reused, and a changed source requires explicit target review again.
A retry never
duplicates a pull request because the server resolves an existing open pull
request for the branch (`opened_existing`) before creating one. Plain **Push**
continues to publish without creating a request.
**Done** after creating or finding a request from the Source Control panel opens
it as a right-panel tab beside the thread for GitHub and GitLab (while Pull
Requests is on in Settings), and only closes the dialog everywhere else.

### Pull Requests command inventory

Successful CLI discovery/auth probes and GitLab host contexts have a request-driven
30-second cache (at most 32 entries per cache, no polling). The origin is still
read each time. Opening the module and switching checkout reuse cached answers;
**Rescan** and authentication recovery send `rescan` on `pullRequests.getContext`,
which bypasses them. Authentication/repository-access failures invalidate them.
GitLab's repository-wide tab totals are reused for 30 seconds across filter
changes and pages; an explicit list **Refresh** (`refreshTotals`), a successful
action (including a request created from the Git Manager or chat), Rescan or
expiry reads them again. Once the host is identified, the login recheck runs
alongside the context or list read; a failed login takes precedence and the
provider read is cancelled and awaited before its result is discarded. Both
paths recheck that the checkout still exists before accepting a host response.
The GitHub context reads its user, repository and viewer permission together.
GitLab writes always re-read project
access/merge policy and the MR permission/head observations, while omitting
UI-only precheck reads. Timeline and reaction paths reuse a viewer already
available in context.

All module host access uses the server's supervised `HostCommandRunner` and
only `gh`, `glab`, or `git`. No browser HTTP request, avatar lookup, or module
background polling contacts the provider. The shared worktree catalog owns
managed-worktree creation after the module fetches the requested head.

| Operation family                  | GitHub commands                                                                                                                               | GitLab commands                                                                                                                                                                   |
| --------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Context and vocabulary            | `gh auth status`, `gh api user`, repository/branch/label/milestone/collaborator REST and GraphQL reads                                        | `glab auth status`, `glab api user`, project/version/member/branch/label/milestone reads                                                                                          |
| List and detail                   | `gh api graphql`; filtered lists use `gh pr list --search`; `gh pr view` plus bounded GraphQL metadata                                        | `glab mr list -F json` (open default, `--closed`, `--merged`, `--all`); `glab api` project/MR/approval/reviewer reads                                                             |
| Timeline, commits, checks, files  | Bounded GraphQL timeline/thread/comment connections; `gh pr view` commits/check rollup/files; `gh pr diff --patch`                            | Bounded GraphQL discussions with notes/awards/positions and REST resource events; REST commits, pipeline jobs, diffs and versions                                                 |
| Comments, reviews, reactions      | `gh pr comment --body-file -`; REST review/comment/reply/reaction writes; GraphQL resolve/minimize; REST dismiss; `gh pr edit --add-reviewer` | REST notes/discussions/awards/suggestions; `glab mr approve --sha`, `glab mr revoke`; GraphQL request/remove changes and re-request review                                        |
| Metadata, state and branch update | `gh pr edit`, `lock`, `unlock`, `ready [--undo]`, `close`, `reopen`, `update-branch [--rebase]`                                               | REST MR updates; `glab mr update --ready/--draft`, `close`, `reopen`, `rebase [--skip-ci]`                                                                                        |
| Merge and cancel auto-merge       | `gh pr merge` with an allowed method, `--match-head-commit`, optional `--auto` or `--admin`, and `--disable-auto`                             | `glab mr merge --sha -y` without message edits, or REST MR merge with pinned SHA and private subject/body; GraphQL requested-change override when allowed; REST cancel auto-merge |
| Revert / delete                   | `gh pr revert`; no delete command                                                                                                             | REST target branch creation, merge-commit revert and MR creation; `glab mr delete`                                                                                                |
| Checkout / worktree fetch         | `gh pr checkout` in the target; `git fetch origin refs/pull/<n>/head:<branch>`                                                                | `glab mr checkout` in the target; `git fetch origin refs/merge-requests/<n>/head:<branch>`                                                                                        |

Bodies, descriptions, review summaries and merge messages never enter argv.
GitHub reads body data from stdin; GitLab API writes use a private temporary
JSON file (0600 on Unix, restricted ACL on Windows), removed after success,
failure or cancellation. Every private-file GitLab API request, including
GraphQL reads, passes `-H "Content-Type: application/json"` before `--input`:
`glab api --method <M> <path> -H "Content-Type: application/json" --input <file>`.
The explicit header makes the raw file payload JSON on hosts that reject an
empty content type; GraphQL queries and variables remain in the private file.
Host/repository operands and `GH_HOST` / `GITLAB_HOST`
pin every request; `glab api` also receives `--hostname`. Every `glab` that
BiBCode starts, including status lookups, request creation and Settings
discovery, runs with `GLAB_SEND_TELEMETRY=0`: otherwise glab sends a telemetry
event to the host before each command returns, which roughly doubles the cost
of every call on a slow link. Your own `glab` configuration is unchanged. See the
[RPC flow and limits](../architecture/rpc-and-orchestration.md#pull-requests-flow).

## Source Control panel

The right-panel Source Control surface manages the active project or worktree:

- The primary action commits staged files, stages all when nothing is staged,
  and exposes available pull, push, and PR states for clean trees.
- The dropdown remains visible and shows unavailable actions as disabled.
- Changes are grouped into Staged Changes, Changes, and Untracked Files with
  per-file status badges.
- The per-file checkbox stages or unstages normally. **Select** mode chooses
  arbitrary files for bulk discard, delete, or ignore actions.
- Hover actions can stage, unstage, discard, restore a deleted file, or delete an
  untracked file. Destructive actions require confirmation.
- Context menus can view a file, copy its path, open it externally, or add ignore
  rules for its name or parent folder when available.
- Commit history and AI commit-message generation are available.
- **Merge into current branch…** fetches and merges a local or remote branch
  into the checked-out branch through Git Manager operations, and a merge in
  progress shows Commit merge / Abort with per-file Ours, Theirs, and Mark
  resolved.

The panel intentionally has no stash or amend action. A staged row also does not
yet open a true `git diff --cached` view.

## Provider setup

### GitHub

Install [GitHub CLI](https://cli.github.com/) and authenticate:

```bash
gh auth login
```

GitHub supports provider lookup, native publish, and pull-request operations.
Open **Settings → Source Control** and rescan to verify the server-side CLI and
account.

### GitLab

Install [GitLab CLI](https://gitlab.com/gitlab-org/cli) and authenticate:

```bash
glab auth login
```

GitLab supports provider lookup and merge-request operations. Native repository
publishing is not implemented.

### GitHub Enterprise and self-hosted GitLab

Run authentication on the machine hosting the selected BiBCode environment,
using the same account/configuration as its server process:

```bash
gh auth login --hostname github.company.example
glab auth login --hostname gitlab.company.example
```

Use the actual hostname from the checkout's `origin`. BiBCode recognizes
configured CLI hosts, including GitLab subgroup repository paths; it does not
guess a provider from an arbitrary hostname. Only the host name counts:
`github.com`, a host containing `gitlab`, Azure DevOps and Bitbucket hosts are
recognized by name; any other host must be identified by the CLIs. Remotes may
use URLs or the scp-like `host:path` form, with or without `user@`. Opening Pull
Requests for the project, or opening **Settings → Source Control** (and its
Rescan), records each authenticated configured host in memory; other screens
that list providers, such as the publish dialog, do not. From then on repository
status, sidebar summaries, the Git Manager pane and the create dialog use it
with the host's own address (for example `https://gitlab.company.example`).
An explicit Settings rescan after full GitLab logout reports **Unauthenticated**
and forgets recorded GitLab hosts. Unrecognized CLI output reports **Unknown**
and keeps them. Rescan, `unknown_host` and `not_authenticated` forget a host, and a server
restart starts empty. An `unknown_host` state offers the
login advice, while `not_authenticated` names the selected provider and host.
Open **Settings → Source Control**, leave **Pull requests** enabled and Rescan
the provider. Check the per-host authentication/account lines (accounts remain
redacted until revealed), then **Rescan** in Pull Requests to refresh context.
Authentication to another host alone does not authorize this repository.
Missing checkout directories require restoring the path or choosing a reachable
worktree before rescanning.

### Azure DevOps

Install [Azure CLI](https://learn.microsoft.com/cli/azure/install-azure-cli), add
the DevOps extension, and sign in:

```bash
az extension add --name azure-devops
az login
```

BiBCode invokes `az repos pr` with repository auto-detection for pull-request
operations. Native repository lookup and publishing are not implemented.

### Bitbucket

Bitbucket pull-request operations use its REST API directly. Configure either a
bearer access token:

```bash
export BIBCODE_BITBUCKET_ACCESS_TOKEN="your-access-token"
```

or an Atlassian email/API-token pair:

```bash
export BIBCODE_BITBUCKET_EMAIL="you@example.com"
export BIBCODE_BITBUCKET_API_TOKEN="your-api-token"
```

Restart the server after changing its environment. The `origin` remote must be a
recognizable Bitbucket URL so BiBCode can identify the workspace and repository.
Each Bitbucket REST operation has one 30-second deadline spanning request and
response-body work. Response bodies are capped at 1 MiB before JSON decoding;
both declared and chunked oversized responses fail with a typed provider error.

The current Source Control discovery screen does not probe these environment
variables; it always reports Bitbucket as missing/unknown. That status is not a
credential test. Attempting a Bitbucket PR operation returns a specific error if
credentials are absent.

## Troubleshooting

- Confirm `git` and the relevant provider CLI are on the BiBCode server's
  `PATH`, not only on the browser machine.
- A newly configured upstream may precede its local remote-tracking ref, notably
  in a single-branch clone. Git Manager keeps local branches usable in that
  state; use **Fetch** to materialize the tracking ref and ahead/behind counts.
- Rescan **Settings → Source Control** after installing or authenticating `gh`,
  `glab`, or `az`.
- Check whether the remote uses SSH or HTTPS and whether that transport's Git
  credentials work in a server-side shell.
- For Bitbucket, verify the server process inherited the environment variables
  and restart it after changes.

## CLI launch failures

The provider CLI must be available on the machine running the selected
BiBCode environment. A failure to start `glab`, `gh`, or `az` is different from a
CLI that starts and returns an authentication, network or hosting-service error.
Launch errors report a closed operating-system category and, when available, its
numeric error code. They do not expose command arguments, environment values,
credentials or raw process output.

A required file or directory being absent does not by itself prove the CLI is
not installed: executable lookup, an interpreter or loader, and the repository
working directory can fail at the same launch boundary. Check the CLI's
availability to that environment and the repository folder's accessibility.
Permission errors require checking executable and directory access. On macOS,
desktop startup already attempts bounded login-shell PATH hydration; its local
log records whether hydration succeeded or its failure category, without the
PATH value. Do not change PATH merely because launching a CLI failed.

The create-request dialog retains the reviewed branches, title and description.
Retry remains explicit. A creation failure does not mean earlier commit or push
steps were rolled back; retain and inspect any already-published work before
retrying. The corresponding failed stacked action appears in local trace
diagnostics with safe failure details and measured action timing.
