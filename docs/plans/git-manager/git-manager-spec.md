# Git Manager — Specification

Date: 2026-08-31. Status: approved by the requester through a structured
design interview (four rounds, all questions answered). This document is the
authored source of truth for scope; `git-manager-plan.md` in this folder is the
architecture and implementation plan that argues from it.

Supersedes `docs/superpowers/plans/2026-08-18-git-manager/` — an earlier,
never-implemented Fork-style plan whose scope explicitly excluded the working
directory, staging, commit and stash. See § Supersession.

---

## 1. Outcome

Every BiBCode project gains a **Git Manager**: a project-scoped centre view that
reproduces GitHub Desktop's interface and functionality for that project's
repository, and nothing else.

Entry point: a new icon button in the left panel's project header hover strip,
placed immediately after the existing "New worktree" button. Clicking it opens
the project's Git Manager; clicking it again when that view is already open
focuses it rather than creating a second one.

Reference implementation studied for behaviour: the GitHub Desktop source tree
at `/work/github/desktop`. Findings are recorded in
`research/github-desktop-analysis.md`; the BiBCode integration surface in
`research/bibcode-integration-surface.md`; worktree-occupancy behaviour in
`research/worktree-checkout-restrictions.md`. Those three documents are the
evidence base for this specification.

## 2. Hard constraints

These are non-negotiable. Each must be enforced by construction, not by UI
omission alone — no RPC added by this feature may perform a forbidden action.

1. **No repository lifecycle.** The panel cannot add, create, clone, publish,
   or remove a repository, and cannot delete the repository. There is no
   repository switcher, no repository list, and no "recent repositories".
2. **Single repository.** Every action targets the repository of the project
   the panel was opened for. The panel never reads or writes another project's
   repository.
3. **One manager per project.** One Git Manager exists per physical project;
   only one is visible at a time.
4. **Zero telemetry.** The feature emits no analytics, crash or error
   reporting, usage counters, feature-flag fetches, avatar or identity
   lookups, or metadata enrichment, and contacts no third-party host — from
   the web panel, from the server code paths it adds, and from any dependency
   it introduces. The only permitted outbound traffic is user-initiated: git
   network operations against the repository's own configured remotes, and
   provider-CLI calls for pull requests and checks. Both happen only in
   response to an explicit user action; neither is polled in the background.
   See § 9.
5. **Provider-agnostic.** No GitHub sign-in, no OAuth, no fork creation or
   fork-settings management. Pull requests and checks use BiBCode's existing
   provider-CLI integration (`gh` / `glab` / `az`), which works without any
   in-app sign-in.
6. **No new dependencies** unless the plan justifies one explicitly; the web
   app already carries every library this feature needs.

## 3. Scope

### 3.1 In scope

Full GitHub Desktop functional parity except § 3.2, delivered in vertical
slices (§ 10):

- **Changes view** — working-directory file list with per-file inclusion,
  file filtering, context-menu actions (ignore, reveal, open, copy path),
  discard (whole file, selection, all), commit box with summary/description,
  commit options (`--no-verify`, `--signoff`, `--allow-empty`), co-author
  trailers, amend, undo commit, submodule and conflict presentation.
- **Hunk- and line-level staging** — per-line and per-hunk selection with
  drag-select, and partial discard of a selection.
- **History view** — paged commit list, commit detail (metadata, changed
  files, per-file diff), commit context-menu actions, multi-selection,
  compare-to-branch with ahead/behind and a mergeability preview.
- **Branch operations** — list, create, checkout, rename, delete, merge,
  squash-merge, rebase.
- **History rewriting** — cherry-pick, squash, reorder, revert, reset,
  including the drag-and-drop affordances Desktop provides.
- **Conflict handling** — conflicted file list, marker counting,
  ours/theirs resolution for binary and manual conflicts, continue/abort.
- **Sync** — fetch, pull, push, publish-branch (`push -u`), force-push
  (`--force-with-lease` only).
- **Stash** — full native stash list with apply, pop, drop, and per-entry
  diff (§ 6.3).
- **Tags** — create, delete, push.
- **Worktrees** — a worktree selector that scopes the panel to the main
  checkout or any of the project's worktrees (§ 6.1), and occupancy-aware
  guards (§ 7).
- **Pull requests and checks** — read and create via the existing provider
  CLI, refreshed only on explicit user action.
- **Diff viewer** — text diffs with syntax highlighting, whitespace toggle,
  expandable context, intra-line highlights, image diffs, binary and
  submodule interstitials, and the size ladder in § 8.

### 3.2 Out of scope, permanently

- Repository lifecycle of any kind (constraint 1).
- GitHub sign-in, SAML re-authentication, token management, fork creation,
  fork settings, upstream-remote management, "publish repository",
  push-protection and repository-rules dialogs, the tutorial repository.
- GitHub-hosted avatars and any other third-party asset fetch (constraint 4).
- A Fork-style lane commit graph. GitHub Desktop's history is a flat list and
  that is the requested interface; the superseded plan's lane graph is not
  carried forward.
- Copilot-branded features from the reference tree (they are feature-flagged,
  non-upstream additions).

## 4. Surface and lifecycle

- **Route.** The panel is a project-scoped route,
  `/project/$environmentId/$projectId/git`, rendered as the centre view. It is
  not a modal, not a native window, and not a tab in the centre tab strip.
  Navigating to a thread leaves the panel; navigating back to the project
  route returns to it.
- **Reload survives.** The route encodes the panel, so a page reload or
  application restart lands back in the Git Manager on the same project.
- **Project identity is physical.** A manager is keyed by
  `(environmentId, projectId)`. A sidebar row that groups several physical
  projects — the same repository present on more than one environment —
  disambiguates the member exactly as the "New worktree" button does today,
  because each checkout is a different repository on a different machine.
- **View-state cache.** View state for the **two** most recently used projects
  is cached: selected worktree, active tab, selected ref, selected commit,
  selected file, filter text, loaded history pages, scroll anchor, and the
  commit-message draft. Returning to a cached project paints immediately and
  revalidates in the background. A third project evicts the oldest.
  **The cache holds state, not mounted components** — no panel is kept
  mounted while hidden, and no server subscription is held for a project that
  is not being viewed.
- **Unavailable environments.** When the owning environment is disconnected,
  reconnecting, or too old to serve a required RPC, the panel renders an
  explicit unavailable state naming the reason. It never dials an environment
  the user deliberately disconnected. Each new RPC is gated behind a
  default-false capability flag so an older server degrades feature by
  feature instead of erroring.

## 5. Toolbar

Three segments, mirroring GitHub Desktop's structure with segment 1
repurposed, since a repository switcher is forbidden by constraint 1:

1. **Worktree selector** — shows the current checkout and switches the panel
   between the main checkout and the project's worktrees. Its dropdown also
   carries repository information and repo-scoped actions (path, remotes,
   open in editor, open in terminal). This is the panel's only context
   switcher.
2. **Branch dropdown** — the plain branch list (Desktop's non-GitHub variant):
   filter box, Default / Recent / Other grouping, current-branch marker, "New
   branch", and a "merge into current branch" action at the foot. Pull
   requests are a separate surface, not a tab here.
3. **Sync button** — Desktop's push/pull/fetch state machine, minus its two
   "Publish repository" states, which constraint 1 forbids. Force-push is
   offered only when the branch has genuinely diverged, never as a default.

## 6. Behaviour decisions

### 6.1 Worktrees

The panel operates on one checkout at a time, selected in toolbar segment 1.
**On open it always selects the project's main checkout**; switching to a
worktree is an explicit user action, and the selection is remembered in the
view-state cache for the session.

Worktrees are selectable and fully operable as checkouts. Creating and
removing worktrees stays with the existing New-worktree and removal flows —
the Git Manager does not duplicate them.

### 6.2 Concurrent agent activity

Unlike GitHub Desktop, BiBCode repositories are modified continuously by
running agent sessions. Therefore:

- **Live refresh.** Status, refs and history stay current from repository
  change events, not from window focus and timers. GitHub Desktop performs no
  filesystem watching at all; BiBCode already has a `notify`-based watcher and
  must use it. Desktop's focus-plus-timer model is the _fallback contract_ —
  what must remain correct if watching is lost.
- **No locking against agents.** Committing takes a fresh status snapshot
  immediately before staging and commits what is selected, per ordinary git
  semantics. There is no attempt to pause or coordinate with agent sessions.
- **Visible cause.** When an agent session is running in the selected
  checkout, the panel shows a passive indicator, so a file list that keeps
  moving is explained rather than mysterious.

### 6.3 Stash

The panel shows the **full native stash list** (`git stash list`) with apply,
pop, drop, and a per-entry diff.

GitHub Desktop instead recognises exactly one stash per branch, tagged with a
magic message marker, and hides every other entry — including stashes made on
the command line. In a tool where agents run git constantly, that model would
hide real work. "Leave my changes" on branch switch (§ 6.4) therefore creates
an ordinary, visible stash, and the dialog says so.

### 6.4 Switching branches with uncommitted changes

Desktop's dialog is kept: "Leave my changes" (stash them) or "Bring my
changes" (carry them across), with the difference that the stash created is an
ordinary visible entry.

### 6.5 Destructive actions

Discard, force-push, branch delete, tag delete, undo commit, reset and revert
are all kept, each behind an explicit confirmation that states exactly what
will happen. Whole-file discard moves files to the OS trash where the platform
supports it, as Desktop does, falling back to a permanent-discard confirmation
when trashing fails. Force-push is always `--force-with-lease`, never bare
`--force`.

### 6.6 Operations started outside the panel

The panel detects in-progress merges, rebases and cherry-picks from repository
state (`MERGE_HEAD`, `rebase-merge/*`, `sequencer/*`, `CHERRY_PICK_HEAD`,
`SQUASH_MSG`), regardless of whether BiBCode started them. It shows the
continue/abort affordance for them and blocks conflicting mutations with a
stated reason. This also makes the state survive reconnects and server
restarts, which matters for remote-hosted projects.

### 6.7 Identity and avatars

Commit authors are rendered from local data only: name, email, and a
deterministic identicon or initials derived from the email. No avatar is
fetched from any host (constraint 4). GitHub Desktop contacts
`avatars.githubusercontent.com` for every author, even in non-GitHub
repositories; that behaviour is explicitly not copied.

## 7. Guards

Blocking conditions are **computed by the server, authored as user-facing
messages by the server, and rendered verbatim by the client**. The client
derives no git policy. Occupancy is read from the worktree catalog the server
already maintains, or from a single
`git for-each-ref --format='%(worktreepath)'`; it is never inferred by
matching git's stderr, whose wording is version-specific.

Two classes:

- **git-enforced** — git refuses the operation itself. The panel still
  pre-computes the condition and disables or redirects the control, because a
  failure toast is a worse experience than a disabled button; a race that
  slips through produces a structured error carrying the same message. These
  refusals are never bypassed with `--ignore-other-worktrees`, `worktree add
  -f`, or `update-ref`.
- **app-policy** — git permits the operation and only a pre-computed guard can
  block it.

Verified on git 2.55.0, a branch held by another worktree causes git to refuse
`checkout`, `switch`, `checkout -B`, `branch -d/-D`, `branch -f`, `rebase`,
`worktree add` reusing the branch, and `fetch <src>:<branch>` — and the
refusal persists even when that worktree's directory is missing, until the
worktree is pruned. Git does **not** refuse `git branch -m`: renaming a held
branch succeeds and the other worktree's HEAD silently follows.

Required guards:

| Operation                              | Condition                                                            | Class          | Behaviour                                                                                                       |
| -------------------------------------- | -------------------------------------------------------------------- | -------------- | --------------------------------------------------------------------------------------------------------------- |
| Checkout / switch                      | branch held by another worktree                                      | git-enforced   | **Redirect** (§ 7.1)                                                                                            |
| Checkout / switch                      | branch is already current here                                       | app-policy     | Disabled: "Already checked out."                                                                                |
| Checkout / merge / rebase              | working tree dirty                                                   | git-enforced   | Blocked, naming the uncommitted changes                                                                         |
| Delete branch                          | branch held by a worktree (including one whose directory is missing) | git-enforced   | Blocked, naming the worktree path; the missing-directory case says the worktree must be removed or pruned first |
| Delete branch                          | branch is current or default                                         | mixed          | Blocked with the specific reason                                                                                |
| Rename branch                          | branch held by another worktree                                      | **app-policy** | Blocked (§ 7.2)                                                                                                 |
| Force-move / reset a branch            | branch held by another worktree                                      | git-enforced   | Blocked, naming the worktree path                                                                               |
| Pull / fetch into a named local branch | destination held by another worktree                                 | git-enforced   | Blocked, naming the worktree path                                                                               |
| Any mutation                           | another Git Manager operation holds the repository lock              | app-policy     | Blocked: `operation-in-flight`                                                                                  |
| Any mutation                           | a merge, rebase or cherry-pick is in progress                        | app-policy     | Blocked, naming the pending operation; the resolve/abort path is exempt                                         |

Guards are re-validated server-side under the repository lock immediately
before execution, because a pre-computed guard can go stale. A stale client
receives a structured blocked error, not raw stderr. Raw git stderr remains
the last line of defence, redacted as today.

### 7.1 Occupied-branch checkout redirects

Checking out a branch that another worktree holds **switches the panel to that
worktree** rather than failing. The branch row is marked as held and names the
path, and the action reads as a switch, not a checkout. The switch is always
visible — the panel's selected worktree changes and says why; it is never
silent.

This matches both reference implementations: GitHub Desktop's development
branch redirects in exactly this way (and deleted an earlier stderr-matching
implementation in favour of a pre-computed check), and BiBCode's own branch
toolbar already retargets threads to the occupying worktree.

### 7.2 Renaming a held branch

Blocked in the first pass. Git would permit it, but BiBCode records branch
names in thread and worktree-catalog records, so an unguarded rename
desynchronises them. Allowing it requires a transactional rename — git plus
catalog plus thread records, atomic under the repository lock — which is a
separate change and does not belong in the same slice as the rename control.

## 8. Performance

GitHub Desktop's measured constants are adopted as the starting contract:

- History pages of 100 commits, with infinite scroll triggered ten rows from
  the bottom and a re-entrancy guard.
- Virtualised lists with fixed row heights: 29px changed-file rows, 50px
  history rows, 30px branch rows.
- The diff size ladder: above ~70MB the diff is not parsed at all; above
  ~4.4MB it is offered behind "show diff anyway"; any line longer than 5000
  characters degrades the file to large-text; syntax highlighting is capped at
  1MB of content.
- Reads use git's lock-avoiding options so the panel never blocks concurrent
  git in agent sessions.

Two BiBCode-specific additions, both correcting weaknesses identified in the
reference implementation:

- **History pages are pinned to a resolved tip snapshot**, not to a raw
  `--skip` offset. With agents committing while a user scrolls, an offset
  cursor silently duplicates and drops rows. A generation bump splices new
  commits above the pinned snapshot rather than discarding loaded pages.
- **Commit lookup is LRU-bounded.** Desktop's equivalent map is unbounded and
  its own source flags this as a known defect.

Every git invocation runs through the existing supervised process path with a
timeout, an output cap, and a cancellation token, so a client interrupt or a
server shutdown stops the child process.

## 9. Zero-telemetry invariant

Constraint 4 is testable, and the plan must make it so:

- **Forbidden:** any outbound request from the feature to a host that is
  neither a configured remote of this repository nor the configured provider
  endpoint of an explicit provider action. This includes avatars, gravatars,
  identicon services, font or asset CDNs, analytics, error reporting, usage
  counters, and remote feature flags.
- **Permitted, user-initiated only:** `fetch`, `pull`, `push` and equivalent
  git network operations against the repository's own remotes; provider-CLI
  invocations for pull requests and checks.
- **No background polling of the provider.** Pull-request and check data
  refresh only when the user asks. There is no periodic provider call.
- **No new dependency** may introduce a network call; this is checked when the
  plan proposes any dependency, and the answer is expected to be that none is
  needed.

## 10. Delivery shape

Vertical slices. Each slice ships a usable, independently testable increment
and leaves the application shippable; each is validated end to end including
against a remote-hosted project.

Ordering:

1. Read-only: panel shell, route, sidebar button, worktree selector, changes
   list, history list, diffs.
2. Staging and committing: include/exclude, commit, amend, undo, discard.
3. Branches and sync: branch list and lifecycle, toolbar, fetch/pull/push.
4. Stash and merge.
5. Hunk- and line-level staging, partial discard.
6. Rebase, cherry-pick, squash, reorder, revert, reset, and conflict
   resolution.
7. Tags, image diffs, pull requests and checks.

## 11. Supersession

`docs/superpowers/plans/2026-08-18-git-manager/` contains an earlier Git
Manager plan — a 605-line master plan and thirteen decomposed phases — that
was never implemented; every phase in its tracker is `pending` and none of the
RPCs it designs exist in the code. Its product shape differs fundamentally:
a Fork-style lane graph that explicitly excluded the working directory,
staging, commit, stash, rebase, cherry-pick and conflict resolution — the core
of this specification.

That plan is superseded in full. Its technical findings are carried forward
into `git-manager-plan.md` because they were verified against this codebase:
tip-pinned history paging, extending the existing status broadcaster with a
refs/HEAD/worktree signature instead of adding a poller, reusing the worktree
catalog's repository lock rather than introducing a second lock, the
server-authored blocked-reason module, and the checked-in RPC wire-fixture
count gate.

## 12. Decision log

Recorded so later readers know which choices were made deliberately and why.

| #   | Decision                                                                 | Rationale                                                                                                                                                         |
| --- | ------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | Panel operates on a selectable checkout, defaulting to the main checkout | Agents work in worktrees, so the changes users want to review often live there; defaulting to the main checkout keeps the entry point predictable                 |
| 2   | Pull requests and checks kept; sign-in and forks excluded                | BiBCode's provider-CLI integration already delivers PRs without any in-app sign-in                                                                                |
| 3   | Ambient credentials only for network operations                          | The machines that own these repositories already push from them; a credential-prompt flow across a remote-server boundary is a separate security-sensitive design |
| 4   | All destructive actions kept, each behind confirmation                   | Parity with the reference implementation; removing them would push users back to the CLI                                                                          |
| 5   | Live refresh plus plain git commit semantics                             | Better than the reference implementation, which does no watching; coordinating with agents would be complexity without a correctness gain                         |
| 6   | Commit draft and view state persisted per project                        | A lost half-written commit message is the eviction cost users actually resent                                                                                     |
| 7   | Button focuses an existing manager rather than opening a second          | "One manager per project" stated as a constraint                                                                                                                  |
| 8   | Vertical slices                                                          | Every phase leaves the application shippable and end-to-end testable                                                                                              |
| 9   | Supersede the 2026-08-18 plan                                            | Nothing was implemented, and its exclusions contradict this specification                                                                                         |
| 10  | Project route, not a centre tab                                          | "One per project, isolated to it" falls out for free; smaller change against the thread-keyed centre-panel store                                                  |
| 11  | Cache view state, not mounted panels                                     | Mounted hidden panels hold live subscriptions and diff-worker memory for no visible gain                                                                          |
| 12  | Coexist with the existing per-thread Source Control panel, sharing state | Two independent commit drafts for one checkout would be a defect source                                                                                           |
| 13  | Physical project keying                                                  | Every action must run on the machine owning that checkout                                                                                                         |
| 14  | Full native stash list                                                   | Marker-scoped stashes would hide work done by agents and on the CLI                                                                                               |
| 15  | Reuse the existing diff renderer, port the reference's limits            | The worker-pool renderer is the performance-critical part and already exists and is tuned                                                                         |
| 16  | Local-only author identity                                               | No third-party contact, no email leakage, works offline and on remote servers                                                                                     |
| 17  | Occupied-branch checkout redirects rather than refuses                   | Both reference implementations converged on it independently                                                                                                      |
| 18  | Rename of a held branch blocked for now                                  | Allowing it needs a transactional catalog update, a separate change                                                                                               |
| 19  | Detect externally started operations                                     | Agents run git constantly; tracking only our own operations would show a confidently wrong repository                                                             |
| 20  | Zero telemetry, enforced by test                                         | Stated as a mandatory constraint by the requester                                                                                                                 |
