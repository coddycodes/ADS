# GitHub Desktop Analysis for the BibCode Git Manager Panel

Research document, 2026-08-31. Source studied: the complete GitHub Desktop
source tree at `/work/github/desktop` (read-only reference). All file paths
below are relative to that repo root (e.g. `app/src/lib/git/status.ts`).

**Caveat:** the studied checkout is not stock upstream GitHub Desktop. It
carries feature-flagged additions — linked-worktree support
(`enableWorktreeSupport()`), a Copilot dialog suite, and resizable toolbar
buttons — all gated in `app/src/lib/feature-flag.ts`. These are marked
**[FLAG]** and are not part of the classic Desktop UX we intend to replicate.

**BibCode context / hard constraints:** the Git Manager is a center panel for a
SINGLE already-known repository (the project's repo). We must NOT support
adding/creating/cloning repositories or deleting the repository. A toolbar
provides branch/repository actions. BibCode's backend is Rust (`apps/server`),
so §3 records the exact git commands and parse formats each feature needs.

---

## 1. Feature inventory

### 1.1 Changes view (working directory)

| Feature             | Behavior contract                                                                                                                                                                                                                                                                                               | Key sources                                                                                                                                                               |
| ------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| File list           | One row per changed path, tri-state checkbox (On/Off/Mixed per `DiffSelectionType All/Partial/None`), status octicon, 29px rows, virtualized, filterable                                                                                                                                                        | `app/src/ui/changes/filter-changes-list.tsx` (`RowHeight = 29` at :85; the old `changes-list.tsx` no longer exists), `app/src/ui/changes/changed-file.tsx`                |
| Include-all header  | Tri-state checkbox mirroring `WorkingDirectoryStatus.includeAll`; when a filter is active it operates only on visible files; label "N of M changed files"                                                                                                                                                       | `filter-changes-list.tsx:1248-1281`, `app/src/models/status.ts:405-421`                                                                                                   |
| Selection semantics | Mouse click changes viewed file only; Space/Enter toggles inclusion; a Partial file toggles to excluded; double-click opens in external editor                                                                                                                                                                  | `app/src/ui/changes/sidebar.tsx:301-336`, `filter-changes-list.tsx:1132-1134`                                                                                             |
| File filter         | Free text + five AND-combined boolean filters (included/excluded/new/modified/deleted); "hidden changes will be committed" warning + `ConfirmCommitFilteredChanges` popup; filters cleared after commit                                                                                                         | `app/src/ui/changes/changes-list-filter-options.tsx`, `app/src/lib/app-state.ts` (`IFileListFilterState`), `filter-changes-list.tsx:1394-1422`                            |
| File context menu   | Discard (…), Ignore file / Ignore folder (ancestor submenu) / Ignore all `<ext>` (max 5 extensions), Include/Exclude selected, Copy (relative) path, Reveal, Open in editor / default program                                                                                                                   | `filter-changes-list.tsx:657-857`                                                                                                                                         |
| Partial staging     | Per-line checkboxes on the gutter, drag-select ranges, per-hunk handles with All/Partial/None state; "hunk" = a run of consecutive added/deleted lines, not a `@@` hunk; disabled while committing or when whitespace is hidden                                                                                 | `app/src/ui/diff/side-by-side-diff.tsx` (drag :1246-1354, hunk click :1386-1402), `app/src/ui/diff/side-by-side-diff-row.tsx`, `app/src/ui/diff/diff-explorer.ts`         |
| DiffSelection model | Immutable: default type (All/None) + a `Set` of diverging unified-diff line indices + optional selectable-line set; `withLineSelection/withRangeSelection/withToggleLineSelection/withSelectAll/withSelectNone`                                                                                                 | `app/src/models/diff/diff-selection.ts` (`export enum DiffSelectionType { All, Partial, None }` at :6)                                                                    |
| Commit box          | Summary (required) + description; button "Commit N files to **branch**"; disabled without summary (except single-file placeholder auto-summary "Update foo.ts"); >50-char summary hint (`IdealSummaryLength = 50`, `app/src/lib/wrap-rich-text-commit-message.ts:11`); Cmd/Ctrl+Enter commits                   | `app/src/ui/changes/commit-message.tsx` (1854 lines; the real component — `ui/commit-message/commit-message-dialog.tsx` is just a dialog wrapper reused by squash/reword) |
| Commit options      | Bypass commit hooks (`--no-verify`), Signed-off-by (`--signoff`), Allow empty (`--allow-empty`); pre-commit gates: oversized-file (LFS) check, conflict-marker check                                                                                                                                            | `commit-message.tsx:1082-1129`, `app/src/ui/changes/sidebar.tsx:159-221`                                                                                                  |
| Co-authors          | Toggleable author input with autocomplete; emits `Co-Authored-By: Name <email>` trailers; unknown authors trigger a confirmation dialog. GitHub-gated in Desktop (`isCoAuthorInputEnabled`) — BibCode can keep the trailer UI without the GitHub autocomplete                                                   | `commit-message.tsx:577-615, 816`, `app/src/lib/git/interpret-trailers.ts`                                                                                                |
| Amend               | "Amend last commit" mode entered from history context menu; warns via `WarnForcePush` if the commit was pushed; inline notice with "Stop amending"; suppresses the Undo strip                                                                                                                                   | `app/src/lib/stores/app-store.ts:5735-5770`, `commit-message.tsx:1251-1267`                                                                                               |
| Undo commit         | Inline strip under the sidebar for the most recent local commit ("Committed <time> — Undo"); hidden when commit has tags, while amending, or during rebase conflicts; disabled while pushing/committing; warning dialog when working dir dirty; merge commits always warn                                       | `app/src/ui/changes/undo-commit.tsx`, `sidebar.tsx:338-386`, `app/src/ui/undo/warn-local-changes-before-undo.tsx`, `app-store.ts:5790-5828`                               |
| Discard             | Whole-file discard moves files to OS trash first (retry dialog offers permanent discard on trash failure); "Discard all"; partial discard of a right-clicked line/range; confirmation dialogs list up to `MaxFilesToList = 10` paths                                                                            | `app/src/ui/discard-changes/` , `app/src/lib/stores/git-store.ts:1545-1620`, `app/src/lib/git/apply.ts:102-120`                                                           |
| Stash               | One logical stash per branch via message marker `!!GitHub_Desktop<branch>` (`app/src/lib/git/stash.ts:19`); "Stashed Changes" row under the list opens a read-only stash diff viewer with Restore/Discard; switch-branch prompt offers "leave changes" (stash) vs "bring changes"; overwrite-stash confirmation | `app/src/ui/stashing/`, `app/src/ui/stash-changes/stash-and-switch-branch-dialog.tsx`, `app/src/models/uncommitted-changes-strategy.ts`                                   |
| .gitignore          | Context-menu ignore actions append to root `.gitignore`; repository-settings tab has a plain-textarea editor of the file                                                                                                                                                                                        | `app/src/lib/git/gitignore.ts`, `app/src/ui/repository-settings/git-ignore.tsx`                                                                                           |
| Submodules          | List rows show submodule state: uncommittable (dirty submodule, checkbox disabled) vs partially committable (forced Mixed); diff pane shows a "Submodule changes" interstitial (old/new SHA, inner-changes note) instead of a text diff                                                                         | `filter-changes-list.tsx:429-470`, `app/src/ui/diff/submodule-diff.tsx`, `app/src/models/status.ts:113`                                                                   |
| Blank slates        | "No local changes" pane with suggested actions (view stash → pull/push → PR); filter-miss blank slate with "Clear filters"; multi-selection pane "N files selected"                                                                                                                                             | `app/src/ui/changes/no-changes.tsx`, `multiple-selection.tsx`                                                                                                             |

### 1.2 History view

| Feature                      | Behavior contract                                                                                                                                                                                                                                                                                                                                | Key sources                                                                                                                                                  |
| ---------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Commit list                  | 100 commits per batch (`CommitBatchSize = 100`, `app/src/lib/stores/git-store.ts:104`); infinite scroll triggers ≤10 rows from bottom (`CloseToBottomThreshold = 10`, `app/src/ui/history/compare.tsx:79`) with 500ms re-entrancy guard; 50px rows; multi-select; unpushed-commit ↑ indicator; first tag rendered as chip                        | `app/src/ui/history/commit-list.tsx`, `commit-list-item.tsx`                                                                                                 |
| Commit detail                | Expandable summary (72-char wrap into body), avatar/author, short SHA + copy button, +/− line counts, tags; resizable changed-file list (29px rows); per-file diff (read-only); throttled file loading (200ms)                                                                                                                                   | `app/src/ui/history/selected-commits.tsx`, `expandable-commit-summary.tsx`, `file-list.tsx`                                                                  |
| Commit context menu (single) | Amend (row 0), Undo (local, row 0), Reset to commit (rows within local history), Checkout commit (detached-HEAD warning dialog), Reorder, Revert, Create branch from commit, Create tag, Delete tag (unpushed only), Cherry-pick, Copy SHA / tags, View on GitHub                                                                                | `commit-list.tsx:724-865`                                                                                                                                    |
| Commit context menu (multi)  | Cherry-pick N / Squash N / Reorder N commits (contiguity computed; merge commits block squash/reorder)                                                                                                                                                                                                                                           | `commit-list.tsx:926-952`, `compare.tsx:298-326`                                                                                                             |
| Non-contiguous selection     | Diff suppressed; blank slate explains range selection and drag affordances                                                                                                                                                                                                                                                                       | `selected-commits.tsx:340-369`                                                                                                                               |
| Compare-to-branch            | Filter box atop history swaps in a branch list; picking a branch shows tabs "Behind (N)" / "Ahead (N)"; Behind tab includes a merge call-to-action with a dropdown of exactly three actions: Create a merge commit / Squash and merge / Rebase, plus a mergeability preview (clean / N conflicted files / unrelated histories)                   | `app/src/ui/history/compare.tsx`, `compare-branch-list-item.tsx`, `merge-call-to-action-with-conflicts.tsx`, `app/src/ui/lib/update-branch.ts:10-31`         |
| Drag & drop                  | Single drag payload type (`DragType.Commit`); drop targets: branch row = cherry-pick, "New branch" pseudo-row = cherry-pick to new branch, another commit = squash, list insertion point = reorder, PR row = cherry-pick onto PR branch [GH]. Keyboard reorder mode (↑/↓ + Enter) exists. Drag manager is a singleton outside app state for perf | `app/src/models/drag-drop.ts`, `app/src/lib/drag-and-drop-manager.ts`, `app/src/ui/lib/draggable.tsx`, `app/src/ui/lib/list/list-item-insertion-overlay.tsx` |

### 1.3 Branches & operations

| Feature                  | Behavior contract                                                                                                                                                                                                                                                                                                                                                | Key sources                                                                                                                                             |
| ------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Branch foldout           | Filterable list grouped Default / Recent (max 5, from reflog; `RecentBranchesLimit = 5`, `git-store.ts:109`) / Other; 30px rows; current branch check-mark; per-row last-commit date; "New Branch" button; bottom "Choose a branch to merge into **current**" button; Branches/Pull-Requests tab bar only for GitHub repos                                       | `app/src/ui/branches/branch-list.tsx`, `group-branches.ts`, `branches-container.tsx`                                                                    |
| Create branch            | Name validated (duplicate check immediate, ref rules debounced 500ms); base choice via segmented control: default branch vs current branch (target-commit and detached/unborn variants); uncommitted changes handled at checkout time, not in this dialog                                                                                                        | `app/src/ui/create-branch/create-branch-dialog.tsx`                                                                                                     |
| Switch branch w/ changes | Strategy enum `AskForConfirmation / StashOnCurrentBranch / MoveToNewBranch`; "Switch branch" dialog with two options ("Leave my changes" = stash, "Bring my changes"); overwrite-stash warning when a stash exists                                                                                                                                               | `app/src/ui/stash-changes/stash-and-switch-branch-dialog.tsx`, `app-store.ts:4577-4643`                                                                 |
| Rename / delete          | Rename dialog (local branches only; validates rules; warns about remote presence). Delete dialog warns "cannot be undone" + optional "Yes, delete this branch on the remote" checkbox when the branch exists upstream                                                                                                                                            | `app/src/ui/rename-branch/`, `app/src/ui/delete-branch/delete-branch-dialog.tsx:218-242`                                                                |
| Merge                    | "Choose a branch to merge into X" dialog with mergeability preview (via `merge-tree`), ahead/behind count, and the 3-way operation dropdown (merge / squash-merge / rebase)                                                                                                                                                                                      | `app/src/ui/multi-commit-operation/choose-branch/merge-choose-branch-dialog.tsx`, `base-choose-branch-dialog.tsx` (no `ui/merge/` directory exists)     |
| Rebase                   | Chooser previews "This will update X by applying its N commits on top of Y" / fast-forward variant; force-push warning dialog before history rewrites (`Rebase/Squash/Reorder/Amend`); Changes tab swaps commit box for a "Continue rebase" button disabled until conflicts resolved                                                                             | `choose-branch/rebase-choose-branch-dialog.tsx`, `multi-commit-operation/dialog/warn-force-push-dialog.tsx`, `app/src/ui/changes/continue-rebase.tsx`   |
| Multi-commit framework   | One state machine drives merge/rebase/cherry-pick/squash/reorder: steps `ChooseBranch, WarnForcePush, ShowProgress, ShowConflicts, HideConflicts, ConfirmAbort, CreateBranch` (+2 Copilot [FLAG]); progress dialog "Commit i of N"; conflicts dialog lists unmerged files with Open-in-editor / ours-theirs resolution; abort confirmation; success/undo banners | `app/src/models/multi-commit-operation.ts`, `app/src/ui/multi-commit-operation/`                                                                        |
| Conflict resolution      | Marker conflicts show "N conflicts" (= ceil(markerCount/3)) and resolve to a green check at 0 markers; binary/manual conflicts get a "Resolve ▾" ours/theirs menu (strings passed literally to `git checkout --ours/--theirs`); resolved rows offer Undo; committing files with live markers raises a warning dialog                                             | `app/src/ui/lib/conflicts/unmerged-file.tsx`, `app/src/models/manual-conflict-resolution.ts`, `app/src/ui/merge-conflicts/commit-conflicts-warning.tsx` |
| Tags                     | Create-tag dialog (max name 245 chars, duplicate check); delete-tag confirmation (context menu allows deleting unpushed tags only); unpushed tags tracked per-repo in localStorage and pushed as extra refspecs with the next branch push; folded into the toolbar "ahead" count                                                                                 | `app/src/ui/create-tag/`, `app/src/ui/delete-tag/`, `app/src/lib/stores/helpers/tags-to-push-storage.ts`                                                |
| Banners                  | Success banners 5s (merge/rebase, undo-completed) or 15s with an Undo link (cherry-pick/squash/reorder); conflict banners are non-dismissable with a "View conflicts" link                                                                                                                                                                                       | `app/src/models/banner.ts`, `app/src/ui/banners/`                                                                                                       |
| Undo/redo of operations  | Cherry-pick/squash/reorder banners carry `onUndo` which resets to the recorded pre-operation tip (`originalBranchTip` in `IMultiCommitOperationState`)                                                                                                                                                                                                           | `app/src/lib/app-state.ts`, `app/src/ui/banners/success-banner.tsx`                                                                                     |

### 1.4 Push / pull / fetch

Single toolbar button, state machine detailed in §2.2. Force push uses
`--force-with-lease` only, with a confirmation dialog
(`app/src/ui/rebase/confirm-force-push.tsx`). Pull-behind blocks push with a
`PushNeedsPull` dialog. Fetch is available in every dropdown state.

### 1.5 Diff viewer capabilities

- Unified and side-by-side text diff with syntax highlighting (CodeMirror 5
  modes; content cap `MaxHighlightContentLength = 1MB`,
  `app/src/ui/diff/syntax-highlighting/index.ts:20`), intra-line highlights
  (`MaxIntraLineDiffStringLength = 1024`, `app/src/ui/diff/diff-helpers.tsx`),
  hidden-bidi-char detection, whitespace toggle, expandable context
  (`DiffHunkExpansionType { None, Up, Down, Both, Short }`,
  `app/src/models/diff/raw-diff.ts`).
- Diff kinds: `DiffType { Text, Image, Binary, Submodule, LargeText, Unrenderable }`
  (`app/src/models/diff/diff-data.ts:10-23`). Size ladder in §3.4.
- Image diffs (png/jpg/jpeg/gif/ico/webp/bmp/avif) with 2-up, swipe, onion-skin
  and difference modes (`app/src/ui/diff/image-diffs/`), both sides delivered
  as base64 data URIs.
- Diff rows virtualized with a `CellMeasurerCache` (default row 20px,
  `app/src/ui/diff/side-by-side-diff.tsx:76`).

### 1.6 Features to EXCLUDE per BibCode constraints

- Repository list / multi-repo management: the entire repository dropdown
  contents (`app/src/ui/repositories-list/`), Add/Create/Clone repository
  dialogs (`PopupType.AddRepository/CreateRepository/CloneRepository`,
  `app/src/ui/add-repository/`, `app/src/ui/clone-repository/`),
  Remove repository (`PopupType.RemoveRepository`,
  `app/src/ui/remove-repository/`), repository alias
  (`PopupType.ChangeRepositoryAlias`), "recent repositories" grouping.
- Publish repository (`app/src/ui/publish-repository/`), fork creation
  (`app/src/ui/forks/create-fork-dialog.tsx`), tutorial-repository creation
  (`app/src/lib/stores/helpers/create-tutorial-repository.ts`).
- The GitHub.com-entangled set in §5 (exclude or defer).
- [FLAG] worktree UI and Copilot dialogs (non-upstream additions).

---

## 2. UI structure

### 2.1 Layout

Window = title bar → toolbar → (single) banner slot → repository view → modal
popup stack (`app/src/ui/app.tsx:4027-4059, 3329-3342`). Repository view
(`app/src/ui/repository.tsx:645-653`) is a flex row:

- **Sidebar** (resizable, default 250px, min 220 — `app-store.ts:455-475,
  2713-2726`): a `TabBar` with `Changes` (badge = changed-file count) and
  `History` tabs, then either `ChangesSidebar` or `CompareSidebar`.
  Ctrl+Tab toggles tabs (`repository.tsx:687-715`); Cmd/Ctrl+1/2 select them
  via the menu (`app/src/main-process/menu/build-default-menu.ts:185-191`).
- **Main pane**: Changes tab → selected-file diff / no-changes blank slate /
  multi-selection pane / stash viewer; History tab → `SelectedCommits`
  (expandable summary + resizable file list + diff).
- Resizable panels are controlled components with drag + double-click-reset +
  keyboard ±5px events (`app/src/ui/resizable/resizable.tsx`); widths persist
  to localStorage as `IConstrainedValue`s.

### 2.2 Toolbar — three segments

Composition at `app/src/ui/app.tsx:3887-3907`; foldout primitive
`app/src/ui/toolbar/dropdown.tsx` (styles `Foldout` = full-height panel,
`MultiOption` = separate action + chevron buttons; overlay click-outside;
FocusTrap).

1. **Repository dropdown** (width locked to sidebar width) — button shows repo
   icon/name, subtitle "Current repository", tooltip = path; foldout is the
   multi-repo list + "Add ▾" menu (`app.tsx:3471-3528`,
   `app/src/ui/repositories-list/repositories-list.tsx`).
   **BibCode replaces this segment** — the project repo is fixed; keep the slot
   for repo-scoped info/actions (path, open-in-editor/shell, repo settings).
2. **Branch dropdown** (`app/src/ui/toolbar/branch-dropdown.tsx`) — title =
   branch name; variants: unborn (openable only if branches exist), detached
   ("On <sha7>" / "Detached HEAD"), checkout progress (spinner + %,
   not openable), rebase-in-progress (disabled). Foldout =
   `BranchesContainer`: Branches / Pull Requests tabs (tab bar only for GitHub
   repos — a plain branch list otherwise, which is exactly BibCode's case),
   filter box, grouped branch list, bottom merge button. Cmd/Ctrl+B toggles it.
3. **Push/pull button** (`app/src/ui/toolbar/push-pull-button.tsx`, evaluated
   in order):
   1. progress → disabled with progress bar,
   2. no remote → "Publish repository" [EXCLUDE for BibCode: repo is fixed;
      show a disabled/explanatory state instead],
   3. unborn → Fetch,
   4. detached → disabled "Publish branch",
   5. no upstream (`aheadBehind === null`) → "Publish branch" (`push -u`),
   6. ahead=behind=0 and no tags to push → "Fetch <remote>" + "Last fetched
      <relative>" / "Never fetched",
   7. force-push recommended → "Force push <remote>" (custom double-arrow icon),
   8. behind>0 → "Pull <remote>" (or "with rebase" per `pull.rebase`),
   9. else → "Push <remote>".
      Ahead/behind counts render inside the button (tags-to-push added to ahead).
      Dropdown items: Fetch always; Force push offered only in the pull state when
      available, with an inline history-rewrite warning
      (`push-pull-button.tsx:435-510, 619-623`, `push-pull-button-dropdown.tsx`).
      A revert-in-progress replaces this button with a progress stub
      (`app/src/ui/toolbar/revert-progress.tsx`).

### 2.3 Foldouts, popups, banners

`FoldoutType` (verbatim, `app/src/lib/app-state.ts:437-444`):

```ts
export enum FoldoutType {
  Repository,
  Branch,
  AppMenu,
  AddMenu, // vestigial — never shown in this tree
  PushPull,
  Worktree, // [FLAG]
}
```

Foldouts are single-slot state (`currentFoldout: Foldout | null`); popups are a
stack (`allPopups`, `app/src/lib/popup-manager.ts`), all rendered modally by a
switch in `App.popupContent` (`app.tsx:1626-3005`).

`PopupType` (`app/src/models/popup.ts:32-125`) has ~90 members. The subset a
single-repo, provider-agnostic Git Manager needs:

- Branch ops: `RenameBranch`, `DeleteBranch`, `DeleteRemoteBranch`,
  `CreateBranch`, `StashAndSwitchBranch`, `ConfirmOverwriteStash`.
- Changes/commit: `ConfirmDiscardChanges`, `ConfirmDiscardSelection`,
  `ConfirmDiscardStash`, `ConfirmCommitFilteredChanges`,
  `CommitConflictsWarning`, `UnknownAuthors`, `CommitMessage` (squash/reword
  editor), `HookFailed`, `CommitProgress`.
- History ops: `MultiCommitOperation` (merge/rebase/cherry-pick/squash/reorder
  wizard), `WarnLocalChangesBeforeUndo`, `WarningBeforeReset`,
  `ConfirmCheckoutCommit`, `CreateTag`, `DeleteTag`, `UnreachableCommits`.
- Sync: `ConfirmForcePush`, `WarnForcePush`, `PushNeedsPull`,
  `LocalChangesOverwritten`, `GenericGitAuthentication`, `UntrustedCertificate`,
  `AddSSHHost`, `SSHKeyPassphrase`, `SSHUserPassword`.
- Excluded: repo lifecycle (`AddRepository`, `CreateRepository`,
  `CloneRepository`, `RemoveRepository`, `PublishRepository`,
  `ChangeRepositoryAlias`), GitHub-specific (`SignIn`, `CreateFork`,
  `ChooseForkSettings`, `UpstreamAlreadyExists`, `DeletePullRequest`,
  `PushBranchCommits`, `StartPullRequest`, `PullRequest*`, `CICheckRunRerun`,
  `SAMLReauthRequired`, `InvalidatedToken`, `PushProtectionError`,
  `BypassPushProtection`, `PushRejectedDueToMissingWorkflowScope`,
  `OversizedFiles` is GitHub's 100MB rule — optional), app-shell
  (`Preferences`, `About`, `ReleaseNotes`, `MoveToApplicationsFolder`, tutorial,
  test popups), [FLAG] worktree/Copilot popups.

Banners (`app/src/models/banner.ts`): one visible at a time, rendered above
the content with `role="alert"`. Success banners auto-dismiss (5s; 15s when
they carry Undo); conflict banners are sticky with a "View conflicts" link.
Members relevant here: `SuccessfulMerge`, `MergeConflictsFound`,
`SuccessfulRebase`, `RebaseConflictsFound`, `BranchAlreadyUpToDate`,
`SuccessfulCherryPick`, `CherryPickConflictsFound`, `CherryPickUndone`,
`SuccessfulSquash`, `SquashUndone`, `SuccessfulReorder`, `ReorderUndone`,
`ConflictsFound`.

### 2.4 Dispatcher pattern and menu commands

UI → `Dispatcher` façade (`app/src/ui/dispatcher/dispatcher.ts`, 4334 lines) →
underscore methods on `AppStore` → `emitUpdate()` → root React `setState`.
Menu commands arrive as IPC `MenuEvent`s handled by one switch in
`app.tsx:449-560`. Relevant accelerators
(`app/src/main-process/menu/build-default-menu.ts`): Push `Cmd/Ctrl+P`, Pull
`Cmd/Ctrl+Shift+P`, Fetch `Cmd/Ctrl+Shift+T`, New branch `Cmd/Ctrl+Shift+N`,
Rename `Cmd/Ctrl+Shift+R`, Delete `Cmd/Ctrl+Shift+D`, Discard all
`Cmd/Ctrl+Shift+Backspace`, Stash all `Cmd/Ctrl+Shift+S`, Compare to branch
`Cmd/Ctrl+Shift+B`, Merge `Cmd/Ctrl+Shift+M`, Squash-merge `Cmd/Ctrl+Shift+H`,
Rebase `Cmd/Ctrl+Shift+E`, Changes/History `Cmd/Ctrl+1/2`, Branch foldout
`Cmd/Ctrl+B`, focus commit summary `Cmd/Ctrl+G`, toggle stash `Ctrl+H`.

---

## 3. Git backend approach (command-level contracts for the Rust port)

Desktop shells out to a bundled git via dugite. Central wrapper `git()` in
`app/src/lib/git/core.ts`: args + repo path + op name + options
(`successExitCodes` default {0}, `expectedErrors` as parsed error enum, stdin,
`encoding: 'buffer'`, `TERM=dumb`, 256KB rolling combined-output capture for
error display, stderr/stdout `processCallback` for progress parsing). Errors
are regex-classified (dugite `parseError`) into a `GitError` enum mapped to
human messages (`core.ts getDescriptionForError`) — BibCode's Rust server
needs an equivalent stderr-classification table for actionable errors
(auth failure, non-fast-forward, local-changes-overwritten, conflicts, …).
`gitRebaseArguments()` forces `-c rebase.backend=merge` on any rebase-capable
command.

### 3.1 Status

`git --no-optional-locks status --untracked-files=all --branch --porcelain=2 -z`
(`app/src/lib/git/status.ts:212-224`; exit 128 tolerated ⇒ "not a repo").
Porcelain-v2 parser (`app/src/lib/status-parser.ts`): entry kinds `1` changed,
`2` renamed/copied (+score), `u` unmerged, `?` untracked; submodule code
`S<C><M><U>`; headers `# branch.oid/head/upstream/ab` give tip SHA, branch,
upstream, and ahead/behind for free. Extra state gathered per refresh:

- merge: `.git/MERGE_HEAD` exists (`app/src/lib/git/merge.ts:138-141`);
  squash-msg: `.git/SQUASH_MSG` (`merge.ts:151-154`).
- rebase: `.git/REBASE_HEAD` + `.git/rebase-merge/{orig-head,head-name,onto}`
  (`app/src/lib/git/rebase.ts:88-137`); progress snapshot from
  `rebase-merge/{msgnum,end}` (`rebase.ts:150-258`).
- cherry-pick: `.git/CHERRY_PICK_HEAD`; sequencer snapshot from
  `.git/sequencer/{abort-safety,head,todo}` (`app/src/lib/git/cherry-pick.ts:216-260`).
- conflict markers: `git diff --check` parsed for "leftover conflict marker"
  lines (`app/src/lib/git/diff-check.ts`; UI conflicts = ceil(markers/3)).
- binary conflicted files: `git diff --numstat -z <ref>` (`-\t-\t` rows) +
  `git check-attr --stdin -z merge` for merge=binary (`diff.ts:948-997`).

One "status refresh" is therefore ~5–7 git invocations. Special-casing in
`status.ts buildStatusMap`: index-added-then-worktree-deleted entries are
skipped; an untracked entry replaces a staged-delete entry at the same path.

### 3.2 The no-visible-index staging model

Desktop hides git's index entirely; the checkbox/line selection state _is_ the
staging model, applied at commit time:

1. `createCommit` (`app/src/lib/git/commit.ts`): `git reset -- .` (unstageAll)
   → `stageFiles` → `git commit -F -` (message on stdin) with optional
   `--amend / --no-verify / --signoff / --allow-empty`.
2. `stageFiles` (`app/src/lib/git/update-index.ts:109-169`): fully-selected
   paths via `git update-index --add --remove --replace -z --stdin`; renames
   stage the old path with `--force-remove` first; staged deletions re-forced;
   partially-selected files via `applyPatchToIndex`.
3. `applyPatchToIndex` (`app/src/lib/git/apply.ts:12-84`): builds a patch of
   only the selected lines (`app/src/lib/patch-formatter.ts` `formatPatch`) and
   pipes it to `git apply --cached --unidiff-zero --whitespace=nowarn -`.
   Renames are recreated via `git add --update old` + `git ls-tree HEAD old` +
   `git update-index --add --cacheinfo <mode> <oid> newPath`.
4. Partial discard = reverse patch applied to the worktree:
   `git apply --unidiff-zero --whitespace=nowarn -`
   (`apply.ts discardChangesFromSelection`); full discard = trash + `git
   checkout HEAD -- <paths>` (`app/src/lib/git/checkout.ts:210-219`).

Merge-conflict commit: `git commit --no-edit --cleanup=strip` after staging +
manual resolutions (`commit.ts createMergeCommit`).

### 3.3 Diffs

`app/src/lib/git/diff.ts`:

- Working dir: `git diff [-w] --no-ext-diff --patch-with-raw -z --no-color HEAD -- <path>`;
  new/untracked via `--no-index -- /dev/null <path>` (exit 1 = changes);
  renamed files diff against the index. Line-endings warning parsed from
  stderr.
- Commit: `git log <sha> [-w] -m -1 --first-parent --patch-with-raw --format= -z --no-color -- <path> [oldPath]`.
- Commit range: `git diff <oldest>^ <latest> --patch-with-raw … `, retrying
  with the empty-tree SHA when `<oldest>^` doesn't exist
  (`NullTreeSHA`, `app/src/lib/git/diff-index.ts`).
- Branch compare: `git diff --merge-base <base> <comparison> --patch-with-raw …`
  per file, file list via `git diff --merge-base … -C -M -z --raw --numstat --`,
  merge base via `git merge-base` (exit 1/128 = none).
- Changed files of a commit: `git log <sha> -C -M -m -1 --no-show-signature --first-parent --raw --format=format: --numstat -z --`
  parsed by `parseRawLogWithNumstat` (`app/src/lib/git/log.ts:276-334`) —
  rename/copy detection with score (`R085` ⇒ rename-with-modifications).

### 3.4 Diff limits (worth copying verbatim)

`diff.ts:41-61`: `MaxDiffBufferSize = 70e6` (70MB → `Unrenderable`, not even
parsed); `MaxReasonableDiffSize = 70e6/16` (~4.375MB → `LargeText`, shown only
after "Show diff anyway"); `MaxCharactersPerLine = 5000` (any longer line →
`LargeText`). Binary + known image extension → image diff via
`git show <commitish>:<path>` blob (`app/src/lib/git/show.ts`).

### 3.5 History and branches

- Commits: `git log [range] --date=raw [--max-count=N] [--skip=N] -z
  --format=<delimited: %H %h %s %b "%an <%ae> %ad" "%cn <%ce> %cd" %P
  %(trailers:unfold,only) %D> --no-show-signature --no-color --`
  (`log.ts:120-205`), NUL-delimited custom parser
  (`app/src/lib/git/git-delimiter-parser.ts`); tags from `%D`; summary/body
  capped at 100KB; exit 128 tolerated (unborn HEAD ⇒ empty history).
- Branch list: `git for-each-ref` with delimited
  `%(refname) %(refname:short) %(upstream:short) %(objectname) %(symref)` over
  `refs/heads refs/remotes` (`app/src/lib/git/for-each-ref.ts:13-64`).
- Ahead/behind: `git rev-list --left-right --count <A...B> --`
  (`app/src/lib/git/rev-list.ts:54-89`); branch-vs-upstream uses the
  symmetric-difference range.
- Create `git branch <name> [start] [--no-track]`; rename
  `git branch -m old new` (retry `-M` for case-only renames,
  `app/src/lib/git/branch.ts:48-96`); delete local `git branch -D`; delete
  remote `git push <remote> :<branch>` with fallback `update-ref -d` of the
  remote-tracking ref (`branch.ts:115-139`).
- Checkout: `git checkout [--progress] <name> [-b short]` (remote branches get
  `-b`), progress parsed from stderr, submodules updated after (weighted 0.9 /
  0.1) (`checkout.ts:102-146`); checkout commit = plain `git checkout <sha>`.
- Post-fetch fast-forward of non-current branches: candidates from
  for-each-ref local-vs-upstream SHA comparison, then
  `git fetch . --show-forced-updates --no-write-fetch-head --stdin` with
  `upstreamRef:localRef` pairs on stdin and `GIT_REFLOG_ACTION=pull`
  (`app/src/lib/git/fetch.ts:103-141`).
- Merged-branch safety for delete prompts: `git branch --merged <branch>`
  (`branch.ts:183-206`).

### 3.6 Network operations

- Fetch: `git fetch [--progress] --prune --recurse-submodules=on-demand <remote>`
  (`fetch.ts:39-89`); progress parsers for fetch/pull/push/checkout live in
  `app/src/lib/progress/` and read `--progress` stderr.
- Pull: `-c rebase.backend=merge git pull [--ff if pull.ff unset]
  --recurse-submodules [--progress] [--no-verify] <remote>` (`pull.ts:29-130`).
- Push: `git push <remote> <local>[:<remoteBranch>] [tag refspecs…]
  [--set-upstream when no upstream] [--force-with-lease] [--no-verify]
  [--progress]` (`push.ts:48-119`). Never bare `--force`.
- Unpushed-tag detection: `git push <remote> <branch> --follow-tags --dry-run
  --no-verify --porcelain` parsed (`app/src/lib/git/tag.ts:86-131`).
- Default remote = `origin` else first remote
  (`app/src/lib/stores/helpers/find-default-remote.ts`); current remote =
  the tip branch's upstream remote else default. Default branch resolved from
  git alone: remote HEAD symref → `init.defaultBranch` → `main`
  (`app/src/lib/find-default-branch.ts`, `app/src/lib/helpers/default-branch.ts`).

### 3.7 Merge / rebase / cherry-pick / squash / reorder / revert

- Merge: `git merge [--squash] [--no-verify] <branch>`; results Success /
  AlreadyUpToDate (stdout == `Already up to date.\n`) / Failed; squash then
  `git commit --no-edit` (`merge.ts:30-93`).
- Mergeability preview: `git merge-tree --write-tree --name-only --no-messages
  -z <oursTip> <theirsTip>` (exit 0/1); conflicted count = NUL count − 1;
  unrelated histories ⇒ Invalid (`app/src/lib/git/merge-tree.ts`).
- Rebase: `-c rebase.backend=merge git rebase <base> <target>`; progress from
  stderr lines `Rebasing (n/m)` (`rebase.ts:279-316`); continue = stage files +
  resolutions then `git rebase --continue` with `GIT_EDITOR=:` (or `--skip`
  when the current commit becomes empty); abort `git rebase --abort`. Result
  classification: exit 0 (+ "is up to date" regex ⇒ AlreadyUpToDate) /
  RebaseConflicts ⇒ ConflictsEncountered / UnresolvedConflicts ⇒
  OutstandingFilesNotStaged (`rebase.ts:410-428`).
- Interactive rebase (squash/reorder/amend-older): write a todo file, run
  `git -c sequence.editor=cat "<todo>" > rebase [--no-verify] -i
  <lastRetainedCommitRef | --root>` with `GIT_EDITOR=:`
  (`rebase.ts:570-627`). Squash todo built by replaying log order with
  pick/squash lines and the message injected via `GIT_EDITOR=cat "<msg>" >`
  (`app/src/lib/git/squash.ts`); reorder todo likewise
  (`app/src/lib/git/reorder.ts`). `lastRetainedCommitRef` = `<oldestTouched>^`
  or null ⇒ `--root` (`app/src/ui/history/commit-list.tsx:322-330`).
- Cherry-pick: `git cherry-pick <shas…> --empty=keep -m 1`; progress from
  stdout `[branch sha] summary` lines (`cherry-pick.ts:141-198`).
- Revert: `git revert [-m 1 if merge commit] <sha>` (`app/src/lib/git/revert.ts`).
- Manual (binary/ours-theirs) resolution: `git checkout --ours|--theirs <path>`
  then `git add <path>` or `git rm <path>` when the chosen side deleted
  (`app/src/lib/git/stage.ts`, `checkout.ts checkoutConflictedFile`,
  `add.ts`, `rm.ts`).

### 3.8 Stash

`app/src/lib/git/stash.ts`. Desktop-owned entries carry the message marker
`!!GitHub_Desktop<branch>` (:19) — a message-tagging convention, not git
semantics; BibCode should use its own marker (e.g. `!!BibCode<branch>`).
List: `git log -g -z --format=<%gD %H %gs %T %P> refs/stash --` (exit 128 = no
stash); only marker-matching entries surface, LIFO ⇒ first per branch is the
live one. Create: stage untracked files first, then
`git stash push -m "<marker>"`. Pop: `git stash pop --quiet <ref>`; a
conflicting pop (exit 1, empty stderr) leaves the entry, so it is dropped
manually. Drop: `git stash drop <ref>`. Stash file list: `git stash show <sha>
--raw --numstat -z --format=format: --no-show-signature --`. Move to another
branch: `git commit-tree` + `git stash store` + drop old (:95-118).

### 3.9 Undo commit, reset, tags, gitignore

- Undo commit (`app/src/lib/stores/git-store.ts:673-742`): normal case
  `git reset --mixed <parentSHA>`; initial-commit case restores deleted files
  (`git checkout HEAD -- paths`), deletes the HEAD ref
  (`git update-ref -d HEAD`), unstages all; then restores message + co-authors
  into the commit box. Reset-to-commit uses the same `reset` helper
  (`app/src/lib/git/reset.ts`: `--hard` / `--soft` / mixed).
- Tags: create `git tag -a -m '' <name> <sha>`; delete `git tag -d <name>`;
  list `git show-ref --tags -d` normalizing the `^{}` annotated-tag suffix
  (`tag.ts:13-76`).
- Gitignore: read/write the root `.gitignore` directly (line endings per
  `core.autocrlf`), append escaped patterns (`gitignore.ts`).
- Co-author trailers: `git interpret-trailers --parse` / `--trailer
  Co-Authored-By=…` (`app/src/lib/git/interpret-trailers.ts`).

---

## 4. State and refresh model

### 4.1 Architecture

Dispatcher (stateless façade) → `AppStore` (10.8k-line god store) → event-kit
`emitUpdate`, coalesced to **one emit per animation frame** (immediate when the
window is hidden) → single root React `setState`
(`app/src/lib/stores/app-store.ts:1182-1208`, `app/src/ui/app.tsx:318-324`).
Per-repo split: `GitStore` (one per repo, never evicted;
`app/src/lib/stores/git-store.ts`) holds git-derived truth — tip, branches,
remotes, history SHAs, `commitLookup: Map<sha, Commit>` (unbounded — a known
wart; use an LRU in BibCode), stashes, tags-to-push, draft commit message.
`RepositoryStateCache` (`app/src/lib/stores/repository-state-cache.ts`) holds
UI-facing state: selection, diff, compare state, `isCommitting`,
`isPushPullFetchInProgress`, conflict state.

### 4.2 Freshness: NO filesystem watching

There is no working-directory watcher anywhere (the only `fs.watch` is a log
tailer, `app/src/lib/tailer.ts`). Freshness comes from three sources:

1. **Window focus**: main process forwards focus/blur
   (`app/src/main-process/app-window.ts:196-201`); on focus the renderer runs a
   full `refreshRepository` of the selected repo, un-debounced
   (`app/src/ui/index.tsx:362-383`). Blur pauses background pollers
   (`app-store.ts:7972-7990`).
2. **Explicit post-action refresh**: `_refreshRepository`
   (`app-store.ts:4048-4142`) = status → sidebar indicator → `loadRemotes` →
   `loadBranches` → section refresh + lastFetched + stashes + author + tags →
   compare re-init. Called after commit (deliberately not awaited so the
   commit button unblocks fast, `app-store.ts:3739-3756`), checkout, section
   switch, etc.
3. **Timers** (all skewed by ≤30s to avoid thundering herds, all
   elapsed-time-aware):

| What                                                                        | Interval                                          | Source                                                                 |
| --------------------------------------------------------------------------- | ------------------------------------------------- | ---------------------------------------------------------------------- |
| Selected-repo background fetch                                              | server poll-interval, default 60 min, floor 5 min | `app/src/lib/stores/helpers/background-fetcher.ts:7-23`                |
| Global fetch throttle (vs `FETCH_HEAD` mtime — survives restarts)           | 30 min                                            | `app-store.ts:528-531, 2351-2386`                                      |
| All-repos sidebar indicator sweep (skips selected repo; pausable mid-sweep) | 15 min, first run +2 min                          | `helpers/repository-indicator-updater.ts:3-14`, `app-store.ts:533-536` |
| PR list poll [GH]                                                           | 30 min, floor 2 min                               | `helpers/pull-request-updater.ts:5-12`                                 |
| CI status poll [GH]                                                         | 3 min, 6 concurrent, 60s cache floor              | `commit-status-store.ts:107-125`                                       |
| Branch prune (merged, >14 days stale, protected names skipped)              | timer 4h, gate 24h                                | `helpers/branch-pruner.ts:22-35,85-88,148-188`                         |

**BibCode note:** BibCode already has workspace change detection
(`docs/plans/2026-08-18-workspace-change-detection-design.md`) and a Rust
server that can watch the filesystem; Desktop's focus+timer model is the
_fallback contract_ (what must stay correct without watching), and its
post-action refresh sequencing is directly reusable.

### 4.3 Concurrency and locking

No general git command queue. Optimistic drop-if-busy flags double as spinner
state: `withPushPullFetch` (one network op per repo,
`app-store.ts:5427-5449`), `withIsCommitting` (`app-store.ts:5364-5390`),
`requestsInFight` de-dupes history batch loads (drop, not queue;
`git-store.ts:120,179-232`). Real limiters only for ahead/behind (`pLimit(1)`,
LRU 2500 entries keyed by OIDs so the key doubles as invalidation, negative
results cached — `app/src/lib/stores/ahead-behind-store.ts`) and CI fetches
(`pLimit(6)`). Failures funnel through `performFailableOperation`
(`git-store.ts:929-945`) → error-with-metadata → error-handler chain → error
dialog carrying a typed `RetryAction`
(`app/src/models/retry-actions.ts`: Push/Pull/Fetch/Checkout/Merge/Rebase/
CherryPick/Squash/Reorder/DiscardChanges/PopStash…).

### 4.4 Large-repo/perf levers

- Status: porcelain-v2 buffer-mode parse, unbounded (`maxBuffer: Infinity` for
  buffer encoding, `core.ts:231-235`) — no file-count truncation.
  `--no-optional-locks` avoids blocking concurrent git.
- History: 100/batch, scroll threshold 10 rows, 500ms guard; only SHAs stored
  in compare state, `Commit` objects in the lookup map.
- Diff ladder: 70MB / ~4.375MB / 5000-char line (§3.4); 1MB syntax-highlight
  cap; 10MB cap on the multi-file diff-text path.
- Lists: custom `List`/`SectionFilterList` over react-virtualized `Grid`
  (`app/src/ui/lib/list/list.tsx`), `overscanRowCount={4}`, fixed row heights
  (history 50, changes 29, branches 30, commit-file 29), memoized invalidation
  hashes; diff rows via `CellMeasurerCache`.
- Frame-coalesced emits + `memoizeOne`/`shallowEquals` everywhere.

---

## 5. GitHub.com API entanglement (exclude/defer for provider-agnostic BibCode)

`app/src/lib/api.ts` (~2.5k lines) is the GitHub/GHES client; capability
gating in `app/src/lib/endpoint-capabilities.ts`. Entangled features:

- **Pull requests**: PR list tab in the branch foldout, PR badge + quick view,
  notifications, checks (`app/src/lib/stores/pull-request-store.ts`,
  `app/src/ui/branches/pull-request-*`, `app/src/ui/open-pull-request/`).
  Note: the "Preview Pull Request" diff itself is _local git_
  (merge-base compare, `app-store.ts:9861-9905`) — only the final "Create"
  button is a github.com URL. A provider-agnostic "compare branches" view can
  keep the local part.
- **CI status**: `commit-status-store.ts`, `app/src/ui/check-runs/`,
  `ui/branches/ci-status.tsx` (renders only on PR rows + toolbar PR badge).
- **Forks/upstream**: fork detection, contribution target, `upstream` remote
  management (`app/src/ui/choose-fork-settings/`,
  `helpers/find-upstream-remote.ts`).
- **Auth/policy**: GitHub sign-in, SAML reauth, token invalidation, secret
  scanning push protection, repository rules/rulesets (API-fetched; commit-box
  branch/message validation fails open without API), protected-branch warning
  (`api.fetchPushControl`; fails open — `app-store.ts:1484-1565`),
  workflow-scope push rejection.
- **Avatars**: no gravatar fallback exists; even non-GitHub repos hit
  `avatars.githubusercontent.com` by email
  (`app/src/ui/lib/avatar.tsx:190-273`). BibCode needs its own resolver
  (initials/identicon; optional gravatar).
- **Autocomplete**: issue/user mention providers are API-backed; emoji
  autocomplete is local (vendored `gemoji/`).
- **Copilot commit-message generation / conflict resolution** [FLAG]: Copilot
  API by default with a BYOK seam (`app/src/lib/copilot/byok.ts`) — BibCode
  has its own providers; treat as inspiration only.
- **Publish/tutorial/clone-by-account, notifications (Alive WebSocket),
  "View on GitHub"-family menu items, GitHub markdown link filters,
  feature flags from the API.**

Silent dependencies inside "core git" features to be aware of: branch pruning
bails out entirely for non-GitHub repos (`helpers/branch-pruner.ts:139`); the
background-fetch interval header degrades to 60 min; PR/CI/notification
surfaces return empty and hide rather than error. Default-branch detection is
pure git and survives (§3.6).

---

## 6. Implications for the BibCode Git Manager (summary)

1. **Scope**: replicate the sidebar (Changes/History), main diff pane, and a
   three-segment toolbar where segment 1 becomes a fixed-repo info/actions
   button, segment 2 is the branch dropdown (the non-GitHub variant — plain
   branch list, no PR tab), and segment 3 is the push/pull state machine minus
   the two "Publish repository" states.
2. **Backend**: every feature above maps to the exact commands in §3; the Rust
   server should expose them as typed RPCs mirroring Desktop's operation
   results (e.g. rebase result enum, merge result enum, progress events
   parsed from `--progress` stderr / `Rebasing (n/m)` / cherry-pick stdout),
   plus the `.git` state probes (MERGE_HEAD, `rebase-merge/*`, `sequencer/*`,
   CHERRY_PICK_HEAD, SQUASH_MSG) that make externally-started operations
   resumable in the UI.
3. **Contract-critical invariants**: hidden index rebuilt per commit
   (§3.2); `--force-with-lease` only; stash marker convention;
   ahead/behind from the status header (free) vs rev-list (arbitrary pairs,
   OID-keyed cache); diff size ladder; single-banner + popup-stack UI model;
   drop-if-busy operation flags surfaced as spinners.
4. **Don't copy**: unbounded commit lookup map, the stale "too many files"
   error message (`git-store.ts:689`), GitHub-avatar fallback, the vestigial
   `FoldoutType.AddMenu`.
