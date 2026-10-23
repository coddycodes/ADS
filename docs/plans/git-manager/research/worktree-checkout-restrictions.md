# Worktree checkout restrictions — Git, GitHub Desktop, and BibCode today

Research capture, 2026-08-31. Question: when a branch is already checked out in
another worktree (or is the current branch of the main checkout), which
operations are restricted, how does GitHub Desktop present/handle that, and
what does BibCode already do?

All Git behavior below was verified empirically with **git 2.55.0** on Linux in
a scratch repository (main checkout on `master`, linked worktree `wt-feature`
holding `feature`). Error phrasing has changed across git versions, so any
stderr matching must treat these strings as version-sensitive; the
recommendation below is to pre-compute occupancy instead of parsing stderr.

---

## A. What git itself enforces

### A.1 Operations git refuses when the branch is checked out elsewhere

| Operation                                                                 | Result                                                                                               | Exit code |
| ------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------- | --------- |
| `git checkout <branch>`                                                   | **refused**                                                                                          | 128       |
| `git switch <branch>`                                                     | **refused**                                                                                          | 128       |
| `git checkout -B <branch> <ref>` (force-reset + checkout)                 | **refused**                                                                                          | 128       |
| `git branch -d <branch>` / `git branch -D <branch>`                       | **refused**                                                                                          | 1         |
| `git branch -f <branch> <ref>` (force-move the ref)                       | **refused**                                                                                          | 128       |
| `git rebase <upstream> <branch>` (implicit checkout)                      | **refused**                                                                                          | 128       |
| `git worktree add <path> <branch>` (second worktree for the same branch)  | **refused**                                                                                          | 128       |
| `git fetch <remote> <src>:<branch>` (ref update into occupied branch)     | **refused**                                                                                          | 128       |
| `git branch -m <branch> <new>` (rename)                                   | **allowed** — the other worktree's HEAD is updated to the new name                                   | 0         |
| `git merge <branch>` (merging the occupied branch _into_ the current one) | **allowed** — only the current branch moves                                                          | 0         |
| `git update-ref refs/heads/<branch> <ref>` (plumbing)                     | **allowed, silently** — leaves the other worktree's index/working tree out of sync with its new HEAD | 0         |

Verbatim stderr captured (paths abbreviated to `.../wt-feature`):

- `git checkout feature`, `git switch feature`, `git checkout -B feature master`,
  `git rebase master feature`, `git worktree add ../wt-feature2 feature`:

  ```text
  fatal: 'feature' is already used by worktree at '.../wt-feature'
  ```

- `git branch -d feature` / `git branch -D feature`:

  ```text
  error: cannot delete branch 'feature' used by worktree at '.../wt-feature'
  ```

- `git branch -f feature master`:

  ```text
  fatal: cannot force update the branch 'feature' used by worktree at '.../wt-feature'
  ```

- `git fetch . master:feature`:

  ```text
  fatal: refusing to fetch into branch 'refs/heads/feature' checked out at '.../wt-feature'
  ```

Notes:

- "Merge into an occupied branch" is not a distinct git operation — `merge`
  only ever writes the _current_ branch. The real vectors for moving an
  occupied branch from outside its worktree are `branch -f`, `fetch <src>:<dst>`,
  and `push` into a non-bare checkout; the first two are refused as shown
  above. A rebase _of_ the occupied branch is refused because rebase implicitly
  checks it out.
- **Rename is NOT protected.** `git branch -m feature feature2` succeeded and
  the linked worktree's `HEAD` followed
  (`git -C wt-feature symbolic-ref HEAD` → `refs/heads/feature2`). Renaming the
  main checkout's current branch from inside a linked worktree also succeeded
  (exit 0; the main checkout's HEAD followed). If a product wants renames of
  agent-held branches blocked, that is pure app policy.
- **`git update-ref` bypasses the guard entirely** (plumbing performs no
  worktree check): after `git update-ref refs/heads/feature master` from the
  main checkout, the linked worktree reported staged deletions it never made
  (`git -C wt-feature status --short` → `D  b.txt`). Server code must never
  route ref updates through plumbing "to get past" the porcelain refusal.

### A.2 Escape hatches

- `git checkout --ignore-other-worktrees feature` and
  `git switch --ignore-other-worktrees feature` both **succeed**
  (`Switched to branch 'feature'`), leaving the same branch checked out in two
  worktrees — a state git otherwise prevents.
- `git worktree add -f <path> <branch>` likewise succeeds in creating a second
  worktree for an occupied branch (exit 0).
- Neither `git branch -d` nor `-D` has an override flag; deletion requires the
  occupying registration to go away first (see A.3).

### A.3 Missing / prunable / locked worktrees

Deletion protection outlives the worktree directory. After `rm -rf wt-feature`
(worktree registration still present, listed as prunable):

```text
error: cannot delete branch 'feature' used by worktree at '.../wt-feature'
```

— identical error whether the missing worktree was locked or not. Only after
`git worktree prune` does `git branch -D feature` succeed
(`Deleted branch feature (was 8f7bfa1).`). A guard message for this case
should therefore say "prune the stale worktree registration first", and the
pre-computed occupancy check must count _registered_ worktrees, including ones
whose directory is missing.

### A.4 Cheaply detecting occupancy

`git worktree list --porcelain` emits one stanza per worktree, blank-line
separated, with these observed fields (git 2.55.0):

```text
worktree /abs/path            # always first
HEAD <sha>                    # always
branch refs/heads/<name>      # for a branch checkout
detached                      # instead of `branch` for detached HEAD
locked [reason]               # optional; reason on same line if given
prunable gitdir file points to non-existent location   # optional, with reason
```

(`bare` appears for a bare main entry; not exercised here.)

`git for-each-ref --format='%(refname:short) -> %(worktreepath)'` is supported
by git 2.55.0 (the atom exists since git 2.22, previously `%(worktreepath)` was
unavailable) and returns the checkout path per local branch in one call:

```text
feature -> .../wt-feature
master -> .../main-repo
topic ->
```

Empty value ⇒ the branch is free. This is the cheapest way to mark occupied
refs when listing branches; `worktree list --porcelain` additionally supplies
`locked`/`prunable`/`detached` metadata that `%(worktreepath)` does not.

`git stash` interactions: stashes are shared, not per-worktree — a stash
created inside the linked worktree is immediately visible from the main
checkout (`git stash list` → `stash@{0}: WIP on feature: …`), because
`refs/stash` lives in the common dir. A stash list UI must not present stashes
as belonging to one worktree.

## B. GitHub Desktop (/work/github/desktop)

Framing: this checkout tracks `desktop/desktop@development` (clean tree), so
the worktree feature is **unreleased upstream work**, not a fork-local patch.
"Stock" below means _released_ GitHub Desktop, i.e. the code before the
worktree commits.

### B.1 Feature flag

`app/src/lib/feature-flag.ts:126-127`:
`export const enableWorktreeSupport = () => true` — unconditionally on (it was
a beta gate when introduced). Crucially, the flag gates **only UI entry
points** (menu items, toolbar dropdown, branch context-menu items, repo list
context menu — e.g. `app/src/ui/app.tsx:967,987,3569-3570,3785`,
`app/src/ui/branches/branches-container.tsx:298,396`,
`app/src/lib/menu-update.ts:269-271`). It does **not** gate `_refreshWorktrees`
(`app/src/lib/stores/app-store.ts:4123,4379`) or the checkout redirect
(app-store.ts:4594) — with the flag off, the redirect still happens; the user
just has no worktree UI.

### B.2 Branch rows do NOT indicate worktree occupancy

- `app/src/models/branch.ts`: `Branch` has no worktree field.
- `app/src/lib/git/for-each-ref.ts:16-22`: `getBranches` requests
  `%(refname)`, `%(refname:short)`, `%(upstream:short)`, `%(objectname)`,
  `%(symref)` — **no `%(worktreepath)` anywhere in `app/src`**.
- `app/src/ui/branches/branch-list-item.tsx`: zero occurrences of "worktree";
  the row icon is only `check` (current) or `gitBranch`.

Occupancy is surfaced in a separate toolbar worktree dropdown/list instead
(`app/src/ui/toolbar/worktree-dropdown.tsx`,
`app/src/ui/worktrees/worktree-list-item.tsx:25-45`). The branch list's only
worktree affordance is a context-menu item
`Checkout in new worktree…`
(`app/src/ui/branches/branch-list-item-context-menu.tsx:54-60`), which opens
the Add Worktree dialog pre-filled with
`${repo.name}-${branch.nameWithoutRemote}`
(`app/src/ui/toolbar/branch-dropdown.tsx:414-432`).

### B.3 Checkout of an occupied branch: silently redirected

`app/src/lib/stores/app-store.ts:4593-4600`, inside `_checkoutBranch`:

```ts
// If the branch is checked out in another worktree, switch to that worktree
// instead of checking out the branch in the current worktree.
const wt = repositoryState.worktrees.find((wt) => wt.branch === branch.ref);
if (wt) {
  return this._switchWorktree(repository, wt);
}
```

No dialog, no confirmation — the app switches its selected repository onto the
occupying worktree (`_switchWorktree`, app-store.ts:6054-6103, pre-seeding UI
state via `repositoryStateCache.seedFromWorktree`). The occupancy data comes
from `_refreshWorktrees` parsing `git worktree list --porcelain -z`
(`app/src/lib/git/worktree.ts:56`, model
`WorktreeEntry { path, head, branch, isDetached, type, isLocked, isPrunable }`
in `app/src/models/worktree.ts`).

Historically this evolved in three steps (commit hashes in the reference
checkout): `4e6d9320ac` attempted the checkout and parsed
`/fatal: '.*?' is already used by worktree at '(.+?)'/` off stderr to find the
path; `28d3abf38c` moved to the pre-flight state check and kept the regex as a
race safety net; `cc2a0b0124` deleted the regex fallback. Today a race
(worktree state not yet refreshed) falls through to the generic
`CheckoutError` path (app-store.ts:4636-4639). `--ignore-other-worktrees`
appears nowhere in the repo.

### B.4 Error classification: none for this case

- dugite is pinned at **3.2.3** (`app/yarn.lock:636-639`); its `GitError`
  enum/regex table has **no entry** for "already used by worktree" —
  (`node_modules` is not installed in that checkout; the enum was checked
  against dugite v3.2.3's published `lib/errors.ts`, the version resolved by
  the lockfile) — corroborated by the exhaustive
  `assertNever` default in `getDescriptionForError`
  (`app/src/lib/git/core.ts:573-575`), whose ~50-case switch has no worktree
  case.
- Therefore `result.gitError` is undefined, `GitError.message` falls back to
  raw stderr (`core.ts:141-181`), and the user sees a generic dialog titled
  **"Error"** whose body is git's own
  `fatal: '<branch>' is already used by worktree at '<path>'`, with a checkout
  retry action. The Add Worktree dialog likewise pushes raw `git worktree add`
  failures straight to `dispatcher.postError(e)`.

### B.5 Other worktree-aware behavior worth copying

- The branch pruner excludes branches checked out in linked worktrees from
  automatic deletion
  (`app/src/lib/stores/helpers/branch-pruner.ts:202-218`).
- Adding a path that turns out to be a linked worktree of a known repository
  switches to it instead of adding a duplicate repo
  (`app/src/ui/dispatcher/dispatcher.ts:2082-2094`).
- Deleting the _current_ worktree first switches back to the main worktree;
  failures get a dedicated `DeleteWorktreeFailed` popup
  (app-store.ts:6105-6167).

### B.6 Stock vs. flagged summary

Released GitHub Desktop: no worktree state or UI at all — checkout of an
occupied branch runs `git checkout`, dugite does not classify the failure, and
the user hits a dead-end "Error" dialog with raw stderr. The development-branch
additions replace that with pre-computed occupancy (`worktree list
--porcelain -z` cached in repository state) and a silent redirect to the
occupying worktree, plus explicit "checkout in new worktree" affordances — but
still no per-branch-row indicator, no `%(worktreepath)`, and no classification
of the raw error in the race case.

## C. BibCode today (/work/workspaces/orca/BibCode/develop-2)

**Precise answer to "block, warn, or raw error?": all three, in layers.**
(a) The branch toolbar _avoids_ the occupied case by design — selecting an
occupied branch retargets the thread to that branch's existing worktree instead
of running `git switch`. (b) The Create Worktree dialog _pre-computes and
warns_, auto-suffixing a new branch name. (c) The server itself has **no
guard**: `switch_ref` is a bare `git switch`, and if it ever runs against an
occupied branch the redacted raw git stderr is surfaced verbatim in an error
toast. No classification of the occupied-branch error exists anywhere in the
codebase.

### C.1 The server already knows which branch each worktree holds

- `apps/server/src/git/repository.rs:3375-3403` (`worktree_map`, operation id
  `GitVcsDriver.listRefs.worktreeList`) parses `git worktree list --porcelain`
  into a `branch name → worktree path` map.
- `list_refs` (repository.rs:1593-1654) attaches it per local ref:
  `worktree_path: (!is_remote).then(|| worktrees.get(name).cloned()).flatten()`
  (repository.rs:1649). The refs listing itself uses
  `for-each-ref --format=%(refname:short)%09%(HEAD)%09%(committerdate:unix)`
  (repository.rs:1597-1601) — it does **not** use `%(worktreepath)`; occupancy
  comes from the separate `worktree list` call.
- The contract exposes it: `VcsRef.worktreePath`
  (`packages/contracts/src/git.ts:93`).
- The worktree catalog's `WorktreeDescriptor`
  (`apps/server/src/worktree_catalog/model.rs:37-55`) carries `branch`, `head`,
  `is_primary`, `is_bare`, `locked`, `lock_reason`, `registration_state`,
  `directory_state`, `adoption_state` — everything a guard needs, including the
  missing-directory case (A.3) via `directory_state`.

### C.2 Branch toolbar: occupied branches are retargeted, not checked out

- `resolveBranchSelectionTarget`
  (`apps/web/src/components/BranchToolbar.logic.ts:100-127`): when the chosen
  ref has a `worktreePath`, it returns
  `{ checkoutCwd: refName.worktreePath, …, reuseExistingWorktree: true }`.
- `selectBranch`
  (`apps/web/src/components/BranchToolbarBranchSelector.tsx:379-441`): for
  `reuseExistingWorktree` it calls `setThreadBranch(name, nextWorktreePath,
  worktreeKey)` and returns **without invoking `git switch` at all**
  (lines 395-403) — the thread's environment moves to the worktree that already
  holds the branch. Only free branches reach the `vcs.switchRef` RPC
  (lines 412-421).
- Branch rows do indicate occupancy: an item whose `worktreePath` differs from
  the active project cwd renders a `worktree` badge (a `current` badge wins)
  — `BranchToolbarBranchSelector.tsx:647-653`.

### C.3 Create Worktree dialog: pre-computed policy, suffixed branches

- `canReuseBranch` (`apps/web/src/components/CreateWorktreeDialog.logic.ts:69-72`):
  only a local, non-current branch with `worktreePath == null` can be checked
  out as-is; `isOccupiedLocalBranch` (lines 74-76) treats "current branch of
  the main checkout" and "held by a worktree" identically.
- The dialog's messaging (`CreateWorktreeDialog.tsx:678-686`):
  `"<name>" is already checked out. A new branch ("<name>-2" or the next
  available name) will be created from it.` — with
  `suggestNextAvailableBranchName` (logic.ts:86-92) picking the suffix.
- The server enforces the same policy independently:
  `create_worktree` (repository.rs:1786-1958) checks `worktree_map` up front
  (1808-1821) and routes occupied branches to
  `create_suffixed_worktree_from_occupied_branch` (repository.rs:1985-2073),
  which creates a detached worktree then `switch -c <base>-N` inside it,
  retrying on races; it re-checks after a failed `worktree add` in case a race
  made the branch occupied mid-flight (1911-1927). `ensure_worktree`
  (1960-1983) short-circuits to the canonical existing path when the branch
  already has a worktree. Occupancy is determined from the worktree list —
  never by matching git's stderr.

### C.4 What happens when a switch does hit an occupied branch

- `switch_ref` (repository.rs:2877-2891) runs plain `git switch <ref>` — no
  pre-check, no `--ignore-other-worktrees`. Likewise `rename_ref`
  (2893-2911) is plain `branch -m` and `create_ref` (2860-2875) is
  `switch -c`/`branch`. There is **no user-facing delete-branch RPC**; the only
  `branch -D` is the internal worktree-creation rollback
  (`rollback_created_branch`, repository.rs:2330-2347).
- On failure, `git_error` (repository.rs:6272-6315) sets
  `GitCommandError.detail` to the redacted raw stderr
  (`actionable_git_failure`, 6317-6327). No stderr classification for
  worktree conflicts exists (`rg "already used by worktree"` matches only the
  CreateWorktreeDialog tests/UI).
- Client side, the toast is
  `title: "Failed to switch ref."` with
  `description: toBranchActionErrorMessage(squashAtomCommandFailure(result))`
  (`BranchToolbarBranchSelector.tsx:430-439`), which renders
  `GitCommandError.message` (`packages/contracts/src/git.ts:389-404`):

  ```text
  Git command failed in GitVcsDriver.switchRef (<cwd>): fatal: '<branch>' is already used by worktree at '<path>'
  ```

  So the fallback layer is: attempt, then surface the raw git error text.

- The git-manager guard surface does not exist yet: there is no
  `apps/server/src/git/guards.rs` or `graph.rs`, and
  `packages/contracts/src/git.ts` has no `VcsGraphBlockedReason` — only the
  generic `GitManagerError` class (git.ts:419-429).

## Prior art: the 2026-08-18 git-manager plan's guard design (historical)

`docs/superpowers/plans/2026-08-18-git-manager/master-plan.md` and
`phases/PHASE-05-ref-tree.md` designed — but **never implemented** (see C.4);
this is historical evidence, not current behavior — a server-authored guard
system:

- A pure module `apps/server/src/git/guards.rs` (master-plan.md:146, 302)
  taking parsed refs, the worktree inventory, the dirty flag, the default
  branch, and running-operation state, returning a blocked list per ref.
  Example message: `Checkout is blocked: this branch is already checked out in
  the worktree at <path>.`
- Contract shape (master-plan.md:198-229): `VcsGraphBlockedCode` =
  `worktree-checked-out | dirty-working-tree | operation-in-flight |
  merge-in-progress | protected-branch | current-branch | no-upstream |
  detached-head | no-remote`, carried as
  `{ operation, code, message }` triples on every branch/remote-branch/tag,
  with `message` rendered verbatim as the tooltip. Note: the code list has
  **no case for rename-of-a-held-branch** — relevant given A.1's finding that
  git itself allows the rename.
- Client rule (PHASE-05-ref-tree.md:103-119, 145): the web `refBlockedReason`
  helper is a pure lookup over server-supplied reasons — _no Git policy
  computed client-side_; disabled controls expose the server message via
  tooltip and `aria-describedby`; unknown codes fail closed
  (PHASE-05-ref-tree.md:154).
- Execution-time re-validation: guards are re-checked server-side when the
  operation runs; a stale client is rejected with `blocked`
  (master-plan.md:453). Mutations serialize through the worktree catalog's
  existing repository lock, with a second operation rejected as
  `operation-in-flight`, never queued (master-plan.md:108, 452).
- Failure classification for push/pull was planned as _exit status plus stderr
  matching_ for `authentication` / `non-fast-forward` only
  (master-plan.md:465) — worktree occupancy was always meant to be
  pre-computed, not stderr-matched.

## Recommended guard set for the Git Manager

Two classes of condition:

- **git-enforced** — git refuses the operation itself; the panel should still
  pre-compute and disable the control (better UX than a failure toast), but a
  race that slips through yields a classifiable, harmless error we can map to
  the same message. Never "fix" these by adding `--ignore-other-worktrees`,
  `worktree add -f`, or `update-ref` (A.2 escape hatches bypass real
  protection).
- **app-policy** — git allows the operation; if we want it blocked, only a
  pre-computed guard can do it.

Occupancy should come from the data the server already has — `worktree_map` /
the worktree catalog (C.1) — optionally cheapened to one
`for-each-ref --format='%(worktreepath)'` call (supported on git ≥ 2.22).
Do **not** classify by stderr matching: the strings above are from git 2.55.0
and have changed across versions.

| Operation                                                    | Blocking condition                                                                     | Class                                                            | Suggested user-facing message                                                                                                                                                                                                                              |
| ------------------------------------------------------------ | -------------------------------------------------------------------------------------- | ---------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Checkout / switch                                            | branch has `worktreePath` ≠ this checkout                                              | git-enforced                                                     | `Checkout is blocked: this branch is already checked out in the worktree at <path>.` Offer "Switch to that worktree" instead (the toolbar already does this, C.2).                                                                                         |
| Checkout / switch                                            | branch is the current branch here                                                      | app-policy (git no-ops)                                          | Disable with `Already checked out.`                                                                                                                                                                                                                        |
| Delete branch                                                | branch has `worktreePath` (including a registered worktree whose directory is missing) | git-enforced                                                     | `Delete is blocked: this branch is checked out in the worktree at <path>.` For a prunable registration (catalog `directory_state` says the directory is gone): `…in a worktree registration whose directory is missing — remove/prune the worktree first.` |
| Delete branch                                                | branch is current / default                                                            | app-policy for default; git-enforced for current                 | `Delete is blocked: this is the current branch.` / `…the default branch.`                                                                                                                                                                                  |
| Rename branch                                                | branch has `worktreePath` owned by an **agent thread**                                 | **app-policy** (git allows it and retargets the worktree's HEAD) | `Rename is blocked: an agent worktree at <path> has this branch checked out.` If allowed instead, the catalog/thread branch fields must be updated in the same operation.                                                                                  |
| Force-move (`branch -f`), reset a branch to a commit         | branch has `worktreePath`                                                              | git-enforced                                                     | `Cannot move this branch: it is checked out in the worktree at <path>.`                                                                                                                                                                                    |
| Rebase a branch (from outside its worktree)                  | branch has `worktreePath`                                                              | git-enforced                                                     | Same message as checkout; offer to run inside the owning worktree.                                                                                                                                                                                         |
| Pull / fetch with explicit `<src>:<dst>` into a local branch | destination branch has `worktreePath` ≠ cwd                                            | git-enforced                                                     | `Cannot update <branch>: it is checked out in the worktree at <path>.`                                                                                                                                                                                     |
| Create worktree from occupied branch                         | branch has `worktreePath` or is current                                                | app-policy (git would refuse the plain add)                      | Keep the existing behavior: warn and auto-suffix (`"<name>" is already checked out. A new branch ("<name>-2" …) will be created from it.` — C.3).                                                                                                          |
| Any mutation                                                 | another git-manager operation holds the repository lock                                | app-policy                                                       | `Blocked: <operation> is already running.` (`operation-in-flight`, per the plan's lock design.)                                                                                                                                                            |
| Checkout / merge / rebase                                    | dirty working tree in the target checkout                                              | git-enforced (checkout may also succeed and carry changes)       | `Blocked: the working tree has uncommitted changes.`                                                                                                                                                                                                       |

Cross-cutting requirements, all consistent with the historical plan and with
what exists today:

1. **Server-authored messages, pure lookup client-side** — the client renders
   `{ operation, code, message }` verbatim and computes no policy (prior-art
   section; matches how `worktreePath` already flows to the UI).
2. **Re-validate at execution time** — pre-computed guards go stale; the server
   must re-check occupancy under the repository lock before running, and a
   stale client gets a structured `blocked` error, not raw stderr.
3. **Keep raw stderr as the last line of defense** — today's toast pipeline
   (C.4) already surfaces `GitCommandError.detail`; guards reduce how often
   users see it, they don't replace it.
4. **Stash lists are repository-wide** (A.4) — do not scope them per worktree
   in the UI.
5. **Redirect beats refusal for checkout** — both GitHub Desktop (B.3) and
   BibCode's own toolbar (C.2) converged on "switch to the occupying worktree"
   as the primary affordance; the Git Manager's checkout guard should offer
   the same action, not just a disabled control. GitHub Desktop also tried the
   stderr-regex route (`/fatal: '.*?' is already used by worktree at
   '(.+?)'/`) and deleted it in favor of the pre-computed check — independent
   confirmation of recommendation "pre-compute, don't parse".
6. **Protect held branches from automated cleanup** — GitHub Desktop's branch
   pruner explicitly excludes branches checked out in linked worktrees
   (B.5); any future BibCode branch-cleanup feature needs the same exclusion.

## Experiment log

Scratch repo: `scratchpad/gitlab/main-repo` (+ `wt-feature`, `wt-topic`,
`wt-detached`), git 2.55.0. Every command, exit code, and stderr line quoted in
section A was captured from that repository on 2026-08-31.
