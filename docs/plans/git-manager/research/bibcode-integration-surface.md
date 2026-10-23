# Git Manager — BiBCode Integration Surface Research

Date: 2026-08-31. Research input for the planned "Git Manager" center panel: a
button in the left panel's project section (after "create worktree") that opens
a GitHub-Desktop-style git manager center panel scoped to one project's
repository, one manager per project, remote-server compatible.

This is a survey of the current integration surface with file citations. Line
numbers are from the working tree at the time of writing and will drift.

## Executive summary — extension points

- **Left-panel button**: add a sibling of the "New worktree" button inside the
  hover-action strip of the project header in
  `apps/web/src/components/Sidebar.tsx` (worktree button at `:3157-3172`),
  reusing `runProjectMemberAction` (`:2473-2497`) for grouped-project member
  disambiguation.
- **New center panel kind**: extend the three-kind `CenterSurface` union in
  `apps/web/src/centerPanelStore.ts:36-48`, its `sanitizeSurface` persistence
  gate (`:262-290`), the render dispatch in
  `apps/web/src/components/ChatView.tsx:1267-1308`, and the tab title/icon
  switches in `CenterPanelTabs.tsx:73-96` / `CenterPanelSplitLayout.tsx:97-114`.
  Layout math (`centerPanelLayout.ts`) is kind-agnostic and needs no change.
- **Crucial structural fact**: today center surfaces are _only_ chat threads
  and terminals, keyed per **host thread**; diff/source-control/files are
  **right-panel** per-thread surfaces (`rightPanelStore.ts:19-29`). A
  per-**project** git manager is the first center surface keyed by project, not
  thread. **Inactive center tabs fully unmount** — there is no LRU/mount cache
  today (`CenterPanelSurfaceHosts.tsx:240-252`); an LRU of ~2 mounted panels
  would be a new mechanism hooked into that predicate.
- **Git backend**: the server shells out to the `git` CLI
  (`apps/server/src/git/repository.rs:362`, `process.rs:133`; no
  git2/gitoxide). A rich `vcs.*`/`git.*` RPC surface already exists
  (status streaming, stage/unstage/discard, branch list/create/switch, log,
  pull, stacked commit/push/PR, diff preview). Missing for GitHub-Desktop
  parity: standalone commit/push RPCs, hunk staging, stash, merge/rebase,
  user-invoked fetch, branch delete/rename RPC, per-commit diff, conflict
  modeling.
- **New RPC method** = contracts (`WS_METHODS` + `Rpc.make` + `RpcGroup`), fixture
  regeneration, Rust `ACTIVE_RPC_METHODS` + scope table + handler registration,
  parity-test count bumps, client atom family. Exact checklist in §5.
- **Remote servers**: a project is `{ environmentId, projectId }`; all git RPCs
  are `cwd`-addressed and already routed per environment through
  `EnvironmentRegistry`/`EnvironmentSupervisor` atoms. The git manager gets
  remote support for free if it goes through `vcsEnvironment.*` atoms and never
  interprets `workspaceRoot` client-side.

## 1. Left panel project UI

### Structure

- The left panel is composed by
  `apps/web/src/components/AppSidebarLayout.tsx:79-103`: an `EnvironmentRail`
  (52px environment strip, `apps/web/src/components/sidebar/EnvironmentRail.tsx:130`)
  plus the main `ThreadSidebar` — the default export of
  `apps/web/src/components/Sidebar.tsx` (4813 lines; root component at `:4012`).
- Project section: `SidebarProjectsContent` (`Sidebar.tsx:3766`) renders the
  "Projects" header, sort menu (`ProjectSortMenu` `:3450`), add-project button
  (`:3905-3920`, testid `sidebar-add-project-trigger`), and the project list of
  `SidebarProjectItem` rows (memoized, `:1518-3357`).
- Project rows are grouped: `buildSidebarProjectSnapshots` in
  `apps/web/src/sidebarProjectGrouping.ts:71-156` merges physical projects into
  one visible `SidebarProjectSnapshot` (`:18-32`) per _logical project_ (see §6),
  with `memberProjects`/`environmentPresence` so one row can span local + remote
  environments.

### The "create worktree" button (the anchor for the new Git Manager button)

Rendered at `apps/web/src/components/Sidebar.tsx:3157-3172`, inside the
hover-revealed action strip (`<div>` at `:3140`) of the project header:
`FolderGit2Icon`, tooltip "New worktree", `data-testid="new-worktree-button"`,
`aria-label` "New worktree in <project.displayName>", shared class constant
`SIDEBAR_ICON_ACTION_BUTTON_CLASS` (`Sidebar.tsx:338-339`). The strip uses the
`group/project-header` hover/focus-within reveal pattern (`:3056`, `:3140`),
always visible on small screens.

Handler chain (the template the Git Manager button should copy):

1. `handleCreateWorktreeClick` (`Sidebar.tsx:2514-2519`) →
   `runProjectMemberAction` (`:2473-2497`): single-member groups act directly;
   multi-member groups open a native member chooser via
   `chooseProjectMember` (`:426-457`, `readLocalApi()?.contextMenu.show`).
2. `openWorktreeForProjectMember` (`:2506-2512`) →
   `openCreateWorktreeDialog(scopeProjectRef(environmentId, projectId))`
   (`:4086-4089`) → `CreateWorktreeDialog` mounted at `:4707-4714`.
3. The dialog submits via
   `useAtomCommand(worktreeEnvironment.createManaged)`
   (`apps/web/src/components/CreateWorktreeDialog.tsx:392-394`, call `:441-457`)
   → `packages/client-runtime/src/state/worktrees.ts:455-468` →
   RPC `worktree.createManaged` (`packages/contracts/src/rpc.ts:351`, Rpc at
   `:962-970`).

Note the important step-1 nuance for the Git Manager: because a sidebar row may
represent several physical projects (possibly on different environments), a
"per project" panel action must either disambiguate the member like
`runProjectMemberAction` does, or define the panel per _physical_ project ref.

### Other project actions (wiring pattern)

- New main-branch chat: button at `Sidebar.tsx:3141-3156` →
  `useNewThreadHandler` (`apps/web/src/hooks/useHandleNewThread.ts:35`) —
  local draft, no immediate RPC.
- Rename/remove project: `useAtomCommand(projectEnvironment.update/delete)`
  (`Sidebar.tsx:1549-1554`) →
  `packages/client-runtime/src/state/projectCommands.ts:94-105` →
  `orchestration.dispatchCommand` with `project.meta.update` / `project.delete`
  (`packages/client-runtime/src/operations/commands.ts:95-113`).
- Context menus are native, via `readLocalApi()?.contextMenu.show(...)`
  (project header menu `Sidebar.tsx:2112-2270`; primary-row menu with
  Update/Open-in/Pin at `:2922-3052`; browser fallback in
  `apps/web/src/contextMenuFallback.ts`).
- Rows are drag-sortable with dnd-kit (`Sidebar.tsx:3924-3968`).

State libraries: server-derived state uses **Effect Atom**
(`@effect/atom-react` over `effect/unstable/reactivity`; commands via
`apps/web/src/state/use-atom-command.ts` and `connectionAtomRuntime` in
`apps/web/src/connection/runtime.ts`); local UI state uses **zustand**
(`uiStateStore.ts`, `centerPanelStore.ts`, `rightPanelStore.ts`, etc.).

## 2. Center panel system

### Model

- Kinds: exactly three. `apps/web/src/centerPanelStore.ts:36-48`:

  ```ts
  export const CENTER_PANEL_KINDS = ["chat-host", "chat", "terminal"] as const;
  export type CenterSurface =
    | { id: typeof HOST_SURFACE_ID; kind: "chat-host" }
    | { id: `chat:${string}`; kind: "chat"; threadId: ThreadId; providerLabel?: string }
    | { id: `terminal:${string}`; kind: "terminal"; terminalId: string; label?; command? };
  ```

  Diff, Source Control, Files, Plan, Preview, Activity are **right-panel**
  kinds (`apps/web/src/rightPanelStore.ts:19-29`); Settings are routes
  (`apps/web/src/routes/settings.*.tsx`). So the Git Manager is genuinely a new
  center-panel kind, with the right-panel Source Control/Diff surfaces as prior
  art for content.

- Layout: pure algebra in `apps/web/src/centerPanelLayout.ts` — leaf/split tree
  (`:17-31`), max 4 groups (`MAX_CENTER_PANEL_GROUPS = 4`, `:1`), split ratio
  clamped to `[0.15, 0.85]` (`:3-4`), automatic collapse/merge
  (`removeLeaf` `:384`, `pruneEmptyCenterPanelGroups` `:393`,
  `mergeCenterPanelGroup` `:256`). Entirely kind-agnostic (opaque surface ids).
- Store: `apps/web/src/centerPanelStore.ts` (zustand + `persist`). State is
  keyed **per host thread**: `byThreadKey: Record<scopedThreadKey, ThreadCenterPanelState>`
  (`:67-72`). Actions (`:71-111`): `openChatPanel`, `openTerminalPanel`/
  `placeTerminalPanel`, `focusGroup`, `activateSurface`, `dropSurface`,
  `mergeGroup`, `setSplitRatio`, `closeSurface`/`closeOtherSurfaces`/
  `closeSurfacesToRight`/`closeAllSurfaces` (close actions return removed
  surfaces so `centerPanelActions.ts:129-144` can run side effects — delete
  panel thread / close terminal session), `removeThread`.
- Routing: none. TanStack Router routes only to a thread
  (`apps/web/src/routes/_chat.$environmentId.$threadId.tsx:78-80`); center
  layout is not in the URL. The only coupling is a staleness guard in
  `ChatView.tsx:1338-1341`/`1667-1681`/`4136-4140`.

### Rendering and mount behavior

- `apps/web/src/components/CenterPanelWorkspace.tsx` (dnd root) renders
  `CenterPanelSplitLayout.tsx` (panes and tab strips; pane bodies are **empty
  measured placeholders**, `:291-296`) plus an absolutely-positioned overlay,
  `CenterPanelSurfaceHosts.tsx`, which mounts actual surface content aligned to
  measured pane rects (`:113-198`). Splitting/resizing therefore never remounts
  content.
- Mount decision (`CenterPanelSurfaceHosts.tsx:240-252`): a surface is mounted
  only if it is some group's **active** tab, _except_ the host chat surface
  (`chat:host`), which stays mounted and is hidden with
  `visibility:hidden; pointer-events:none` (`:227-234`, `:272-275`).
  **Inactive tabs unmount; there is no LRU or mounted-panel pool anywhere in
  `apps/web/src`.** Terminal continuity across unmount comes from server-side
  PTY sessions, not client component retention (`terminalRetirement.ts:28-49`,
  `CenterTerminalPanel.tsx:55-69`).
  → The requested "LRU cache of ~2 mounted git manager panels" means widening
  the always-mounted predicate at `CenterPanelSurfaceHosts.tsx:251` into a
  policy (e.g. host surface + up to N most-recently-visible git-manager
  surfaces).
- Render dispatch: the single kind→component switch is `renderCenterSurface`
  inside `LiveCenterPanelWorkspace`, `apps/web/src/components/ChatView.tsx:1243-1329`
  (switch at `:1267-1308`).

### Persistence

- zustand `persist` with key `bibcode:center-panel-state:v1`, version 3
  (`centerPanelStore.ts:114-115`, config `:594-603`), partialized to
  `byThreadKey`, storage = `localStorage`.
- Migration/sanitization: `migratePersistedCenterPanelState` (`:292-333`) +
  `sanitizeSurface` (`:262-290`) — **unknown kinds are silently dropped**, so a
  new kind must add a sanitize branch or every persisted Git Manager tab
  disappears on reload — + layout repair
  `repairCenterPanelLayoutState` (`centerPanelLayout.ts:299-341`).
- Dead-thread GC: `apps/web/src/ThreadLifecycleReconciler.tsx:20-56` removes
  center/right panel state for deleted threads.

### To add a new center panel kind you touch

All dispatch sites are exhaustive `switch (surface.kind)` statements, so the
compiler flags missed sites:

1. `centerPanelStore.ts:36` kind tuple; `:39-48` `CenterSurface` variant with a
   unique id prefix (e.g. `` `git-manager:${string}` ``); a factory near
   `:122-138`; an `openXxxPanel` action (mirror `openChatPanel` `:340-345`);
   `sanitizeSurface` branch `:262-290`; `removeThread` cross-thread cleanup
   `:550-592` if the surface references thread/project state.
2. `components/ChatView.tsx:1267-1308` render dispatch.
3. `components/CenterPanelTabs.tsx:73-84` tab title, `:87-96` tab icon;
   `components/CenterPanelSplitLayout.tsx:97-114` pane aria-label.
4. `centerPanelActions.ts:129-144` close side effects (if any).
5. `ThreadLifecycleReconciler.tsx:24-28` if the surface owns a `ThreadId`;
   `CenterPanelSurfaceHosts.tsx:251` if it must stay mounted while hidden.
6. Tests: `centerPanelStore.test.ts`, `centerPanelLayout.test.ts`,
   `centerPanelActions.test.ts`, `CenterPanelTabs(.dom).test.tsx`,
   `CenterPanelSplitLayout.test.tsx`, `CenterPanelSurfaceHosts.test.tsx`,
   `CenterPanelWorkspace.surface-hosts.test.tsx`.

**Design wrinkle**: the store is keyed per host thread, so "one git manager per
project" does not fall out of the current model — a git-manager surface would
live inside whichever thread's center layout it was opened in. The plan must
choose between (a) a project-keyed surface inside the existing thread-keyed
layout with open-time dedup (find/focus an existing `git-manager:<projectKey>`
tab across the active thread's groups), or (b) a separate project-keyed panel
slice. `projectKey`/`parseProjectKey` helpers exist at
`packages/client-runtime/src/state/entities.ts:48-72`.

## 3. Existing git functionality

### 3.1 Server (Rust)

**Invocation**: no git library — `Cargo.lock` has no `git2`/`gix`/libgit2. All
operations spawn the `git` CLI via `tokio::process::Command` through a
supervised runner (`apps/server/src/git/process.rs:133`,
`run_supervised` with timeout/output caps/cancellation); the binary is
hard-coded as `PathBuf::from("git")` in
`apps/server/src/git/repository.rs:362`. Reads use a locks-avoiding
environment (`git_read_environment()` `repository.rs:4775`,
`GIT_OPTIONAL_LOCKS=0` per `docs/architecture/overview.md:113-118`). A second
spawn site builds diff previews (`apps/server/src/production/runtime.rs:790-824`,
`git diff --no-ext-diff --patch --minimal`).

**Modules** (`apps/server/src/git/`): `repository.rs` (7.3k lines, all
operations), `process.rs`, `parser.rs` (porcelain-v2 + numstat),
`broadcaster.rs` (`StatusBroadcaster` fan-out; `subscribe` `:187`,
`begin_mutation` `:654`, `refresh_status` `:899`), `status_owner.rs`
(single-writer status fences/leases), `watcher.rs` (notify-based FS watching),
`summary.rs` (passive per-worktree summaries), `fetch_owner.rs` (background
periodic fetch per common dir), `worktree.rs`, `model.rs`.

**Implemented `GitRepository` operations** (all `repository.rs`): status
local/summary/remote/full (`:1162-1427`), stage `:1457`, unstage `:1474`,
discard (`restore` + `clean -fd`) `:1504`, list refs `:1574`, log
(`list_commits`, paged metadata) `:1739`, branch create `:2860` /
switch `:2877` / rename `:2893` (rename has no RPC), init `:2913`,
clone `:2928`, pull (`--ff-only` hard-coded) `:3045/:3087`,
commit `:3110`, push / push-with-upstream `:3177/:3213`, commit-context diff
for AI messages `:3242`, default ref `:3288`, background fetch `:1380`,
worktree add/list/remove/prune with quarantine machinery
(`:1786`, `:510`, `:2349`, `:733-753`, `:3619-4729`).

**Not present anywhere**: stash, merge, rebase, cherry-pick, revert, amend,
tag, reflog, hunk staging (`add -p`/`apply --cached`), branch delete.

**Adjacent modules**: `apps/server/src/vcs/mod.rs` is a nominal 107-line
driver shim (`VcsDriverKind = Git | Jj | Unknown` `:10`) not on the RPC path —
`GitVcsRpcServices` holds `Arc<GitRepository>` directly
(`apps/server/src/production/git_vcs.rs:196`).
`apps/server/src/source_control/` (3.7k lines) does provider detection and PR
integration by shelling to `gh`/`glab`/`az` (`pull_request.rs:137-139`) or
Bitbucket REST. `apps/server/src/worktree_catalog/` (14k lines) owns worktree
discovery/adoption/removal (see `docs/architecture/worktree-catalog.md`).

**Mutation discipline**: every git mutation must pass through
`StatusBroadcaster::begin_mutation` → `StatusMutationGuard`
(`broadcaster.rs:654-659`; template: the stage/unstage arm at
`git_vcs.rs:522-560`), otherwise it races the streaming status. New git-manager
mutations (stash, merge, …) must follow this.

### 3.2 Contracts (RPC surface)

Schemas in `packages/contracts/src/git.ts`, `vcs.ts`, `sourceControl.ts`,
`worktree.ts`, `review.ts`; method names in `rpc.ts` `WS_METHODS`
(`:310-408`, vcs block `:335-353`, streams `:425-437`). Inventory:

| Method                                                                                                                                                                                                               | Mode            | Notes                                                                                                                                      |
| -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------- | ------------------------------------------------------------------------------------------------------------------------------------------ |
| `subscribeVcsStatus`                                                                                                                                                                                                 | stream          | `VcsStatusInput{cwd}` → snapshot/localUpdated/remoteUpdated events (`git.ts:281-293`)                                                      |
| `subscribeVcsStatusSummary`                                                                                                                                                                                          | stream (latest) | capability-gated passive summary (`vcs.ts:72-117`)                                                                                         |
| `git.runStackedAction`                                                                                                                                                                                               | stream          | actions `commit`, `push`, `create_pr`, `commit_push`, `commit_push_pr` (`git.ts:11-17`) with phase/hook progress events (`git.ts:455-503`) |
| `vcs.refreshStatus`, `vcs.pull`, `vcs.listRefs`, `vcs.listCommits`, `vcs.clone`, `vcs.createRef`, `vcs.switchRef`, `vcs.init`, `vcs.stageFiles`, `vcs.unstageFiles`, `vcs.discardFiles`, `vcs.generateCommitMessage` | unary           | all `cwd`-addressed (`git.ts:109-223`)                                                                                                     |
| `git.resolvePullRequest`, `git.preparePullRequestThread`                                                                                                                                                             | unary           | PR resolution (`git.ts:172-182`)                                                                                                           |
| `sourceControl.lookupRepository` / `.cloneRepository` / `.publishRepository`, `server.discoverSourceControl`                                                                                                         | unary           | provider integration (`sourceControl.ts`)                                                                                                  |
| `review.getDiffPreview`                                                                                                                                                                                              | unary           | source kinds only `working-tree` and `branch-range` (`review.ts:13`) — **no per-commit diff**                                              |
| `subscribeWorktreeCatalog` (latest stream), `vcs.refreshWorktreeCatalog`, `worktree.adopt/createManaged/createPanel/retarget/getRemovalPlan/removeFromBibCode/remove/updateDiscoveryPolicy`                          | —               | worktree catalog surface (`worktree.ts`, `rpc.ts:934-1011`)                                                                                |

Scopes: reads `orchestration:read`, mutations `orchestration:operate`
(`apps/server/src/auth/scope.rs:42-104`). Legacy `vcs.createWorktree` /
`vcs.removeWorktree` are intentionally absent from `WS_METHODS`
(`packages/contracts/src/rpc.test.ts:81-83`). `VcsListRemotesResult`
(`vcs.ts:56-60`) exists but no RPC uses it.

### 3.3 Shared

`packages/shared/src/git.ts` (pure helpers): branch-name
sanitization/derivation (`:20-96`), remote URL normalization + provider
detection (`:103-205`), and the client-side status-stream reducer
`applyGitStatusStreamEvent`/`mergeGitStatusParts` (`:207-308`).
`packages/shared/src/sourceControl.ts` holds change-request ("PR"/"MR")
terminology per provider.

### 3.4 Web

Existing git UI (all per-thread, right panel or chat header):

- `components/SourceControlPanel.tsx` (1230 lines): live status via
  `vcsEnvironment.status({ environmentId, input: { cwd } })` (`:144-176`),
  stage/unstage/discard row actions
  (`SourceControlRowActions.logic.ts:41-90`), persisted commit-message draft
  (`sourceControlPanelStore.ts`), AI commit message, commit/push/PR via
  `useGitStackedAction` (`state/sourceControlActions.ts:285`), commit list
  (`SourceControlCommits.tsx:47`, metadata only, not clickable to a diff).
- `components/DiffPanel.tsx` / `DiffPanelShell.tsx`: `review.getDiffPreview`
  (`:345-362`) with unstaged/branch scope toggle (`diffPanelStore.ts:8-11`);
  diff rendering via `@pierre/diffs` with a worker pool
  (`components/DiffWorkerPoolProvider.tsx`, `lib/diffRendering.ts`).
- `components/BranchToolbar*.tsx`: branch selector with `switchRef`,
  `createRef` (+switch), worktree retarget
  (`BranchToolbarBranchSelector.tsx:116-122,458`).
- `components/GitActionsControl.tsx` (chat header) and
  `components/CreateWorktreeDialog.tsx` / `WorktreeRemovalDialog.tsx`.
- Action hooks take a project-agnostic scope already:
  `SourceControlActionScope = { environmentId, cwd }`
  (`state/sourceControlActions.ts:43,127-135`) — ready for reuse by a
  project-scoped panel.

### 3.5 Gap analysis for a GitHub-Desktop-style manager

Reusable as-is: change list with live streaming status, file-level
stage/unstage/discard, commit (via stacked action), push/pull, branch
list/create/switch, history metadata, working-tree/branch diffs, PR create,
worktree management, clone/init/publish.

Missing, by layer:

| Feature                                       | Server                                                                       | Contract/RPC                                      | UI                  |
| --------------------------------------------- | ---------------------------------------------------------------------------- | ------------------------------------------------- | ------------------- |
| Standalone `vcs.commit` / `vcs.push` unary    | fn exists (`repository.rs:3110/:3177`)                                       | missing (stacked stream only)                     | via stacked control |
| Hunk-level staging                            | missing                                                                      | missing (file-path arrays only, `git.ts:119-122`) | missing             |
| Stash push/pop/list                           | missing                                                                      | missing                                           | missing             |
| Merge / rebase / cherry-pick / revert / amend | missing                                                                      | missing                                           | missing             |
| User-invoked fetch                            | background only (`fetch_owner.rs`)                                           | missing                                           | missing             |
| Non-ff pull                                   | `--ff-only` hard-coded (`repository.rs:3087`)                                | —                                                 | —                   |
| Branch delete                                 | missing                                                                      | missing                                           | missing             |
| Branch rename                                 | fn exists (`repository.rs:2893`)                                             | missing                                           | missing             |
| Per-commit diff ("click a commit")            | missing                                                                      | `review.ts:13` lacks a commit source kind         | missing             |
| Merge-conflict modeling                       | porcelain unmerged states not in `VcsWorkingTreeFileStatus` (`git.ts:48-55`) | missing                                           | missing             |
| Remotes list/add                              | internal only (`repository.rs:1078`)                                         | `VcsListRemotesResult` unwired (`vcs.ts:56`)      | missing             |

## 4. RPC/WebSocket protocol

### Definition (contracts)

Effect RPC (`effect/unstable/rpc`). Per method: a `WS_METHODS` name
(`packages/contracts/src/rpc.ts:310-437`), an `Rpc.make` (unary example
`WsVcsRefreshStatusRpc` `:783-792`; streaming adds `stream: true`,
`WsSubscribeVcsStatusRpc` `:748-758`), and registration in the single
`RpcGroup.make` (`:1297-1399`). New schema files must be re-exported from
`packages/contracts/src/index.ts`.

### Server side

- Method inventory: `apps/server/src/rpc/methods.rs:56-159`
  (`ACTIVE_RPC_METHODS`, built from `read_unary`/`mutation_unary`/
  `read_stream`/`mutation_stream`).
- `RpcRegistry` (`apps/server/src/rpc/session.rs:332-462`):
  `register_unary(_with_context)`, `register_stream` (handler returns
  `mpsc::Receiver<RpcStreamChunk>`), `register_latest_stream`
  (`watch::Receiver`, latest-value semantics — used by
  `subscribeVcsStatusSummary` and `subscribeWorktreeCatalog`).
  `validate_complete()` (`:468-489`) fails startup if inventory and
  registrations disagree; assembly in
  `apps/server/src/production/runtime.rs:374-398` (`register_git_vcs_rpc` among
  them).
- Auth: central `required_scope` table (`apps/server/src/auth/scope.rs:11-124`);
  a method without a scope is a connection defect at dispatch
  (`rpc/session.rs:868-878`); a Rust test enforces exactly one scope per active
  method (`scope.rs:144-152`).
- Git handlers: `apps/server/src/production/git_vcs.rs` —
  `GIT_VCS_UNARY_METHODS` `:169-188` + dispatch `handle_admitted_unary` `:676`,
  streams wired in `register_git_vcs_rpc` `:395-419`; the
  `subscribeVcsStatus` stream handler (`:1116-1250`) decodes the payload, takes
  a workspace admission lease (`guard_git_path`), subscribes to
  `StatusBroadcaster`, and forwards publications until cancellation.

### Client side

- Transport: `RpcSessionFactory` (`packages/client-runtime/src/rpc/session.ts:98-210`)
  — protocol-level reconnect deliberately disabled (`:163-177`); the
  environment supervisor owns retry.
- Environment-scoped verbs: `packages/client-runtime/src/rpc/client.ts` —
  `request` `:136`, `runStream` `:144`, durable `subscribe` `:225-253`
  (switchMaps over session changes, so subscriptions transparently re-attach on
  reconnect). Subscription tags are a hand-maintained union
  (`EnvironmentSubscriptionRpcTag` `:42-58`) — a new stream must be added there.
- Atom factories: `packages/client-runtime/src/state/runtime.ts` —
  `createEnvironmentRpcQueryAtomFamily` `:592` (refires per connect
  generation), `createEnvironmentRpcSubscriptionAtomFamily` `:613`,
  `createEnvironmentRpcCommand` `:645` (with lane schedulers). Atom keys are
  `JSON.stringify([environmentId, input])` (`:423-429`).
- Domain module: `createVcsEnvironmentAtoms`
  (`packages/client-runtime/src/state/vcs.ts:110-212`); web instantiation
  `apps/web/src/state/vcs.ts:8`; consumption via `useEnvironmentQuery`
  (`apps/web/src/state/query.ts:26-39`) and the `sourceControlActions.ts`
  hooks. VCS mutations are serialized on a `(environmentId, cwd)` lane;
  `vcs.refreshStatus` runs on a separate latest lane
  (`state/vcs.ts:33-40`, `docs/architecture/rpc-and-orchestration.md:214-220`)
  — new git-manager mutations must reuse these lanes, not raw `request`.
- `packages/client-runtime` has no barrel export; a new domain module needs an
  `exports` entry in `packages/client-runtime/package.json:5-70`.

### Checklist to add one new RPC method

1. Contracts: schemas in the domain file; `WS_METHODS` entry; `Rpc.make`;
   `RpcGroup.make` registration (omission = hard "stale identifier" failure in
   `packages/contracts/scripts/export-rust-rpc-fixtures.ts:762-765`).
2. Regenerate fixtures:
   `pnpm --filter @bibcode/contracts generate:rust-rpc-fixtures` (rewrites
   `packages/contracts/fixtures/rpc-wire/`).
3. Rust: `ACTIVE_RPC_METHODS` entry (`rpc/methods.rs`), one `required_scope`
   arm (`auth/scope.rs`), handler + registration in the owning
   `production/*_rpc.rs` module.
4. Parity tests: `packages/contracts/src/rpcRustParity.test.ts:295-385`
   (regenerates cleanly), plus **hand-bumped counts** in
   `apps/server/tests/rpc_wire.rs:92-95` (currently 65 stream shapes / 23
   orchestration event shapes / 242 typed failures).
5. Client: subscription tag union (`rpc/client.ts:42-58`) if streaming; atom in
   the domain factory (`state/vcs.ts`); web wrapper + hook usage.
6. `vp run check:contracts` runs the fixture/parity pipeline
   (`docs/reference/scripts.md`).

Worked example commit: `9978001a` (updater.status/check/install). Global rule:
`docs/plans/remote-servers/remote-servers-plan.md:31-33` and
`docs/architecture/rpc-and-orchestration.md:63-67`.

### Live updates / file watching

- Git status is **watcher-driven**: the `notify` crate
  (`apps/server/Cargo.toml:27`) behind `GitWatchService`
  (`apps/server/src/git/watcher.rs:322-400`; native backend wraps
  `notify::RecommendedWatcher` `:250-271`). Watches are installed per
  subscription for exactly three roots — worktree root, git dir, common dir
  (`git/broadcaster.rs:247-281`). Signals debounce at a 125 ms trailing edge;
  sticky fallback states + a 60-300 s safety re-read cover watcher loss
  (`docs/architecture/rpc-and-orchestration.md:98-142`,
  `docs/architecture/overview.md:84-110`).
- Invalidation paths into the stream: mutation fence (`begin_mutation`),
  explicit `vcs.refreshStatus`, catalog mutation fan-out, structured terminal
  process exit.
- The worktree catalog is **poll + fingerprint** based, not notify
  (`worktree_catalog/service.rs:177-182`, 2 s poll / 60 s idle eviction / 5 min
  fingerprint reconciliation).
- Implication: a git manager's change list and branch state can ride the
  existing `subscribeVcsStatus`/`subscribeVcsStatusSummary` streams unchanged;
  anything outside those three watch roots (or changed on a remote host outside
  the server's filesystem view) converges only via safety reads or explicit
  invalidation.

## 5. Remote server support

- Model (`docs/architecture/remote.md:1-22`): one server process = one
  **environment**; `environmentId` is the stable routing identity; remote
  clients use the same HTTP + Effect RPC API. The spec
  (`docs/plans/remote-servers/remote-servers-spec.md:7-12`) marks itself
  superseded — current truth is `docs/architecture/remote.md` +
  `connection-runtime.md`. All 7 implementation phases in
  `docs/plans/remote-servers/phases/` are checked complete and corroborated in
  code (E2EE `apps/server/src/rpc/e2ee.rs`; environment rail
  `apps/web/src/components/sidebar/EnvironmentRail.tsx`; remote updates
  `apps/server/src/production/remote_update_rpc.rs`).
- Addressing: `ScopedProjectRef = { environmentId, projectId }`
  (`packages/contracts/src/environment.ts:90-94`). Routing rule (spec D4):
  an operation on an entity owned by environment X targets X regardless of
  which environment rail is selected; rail selection scopes the view only (D3).
- Client routing chain: `EnvironmentRegistry` (catalog + one
  `EnvironmentSupervisor` per environment;
  `packages/client-runtime/src/connection/registry.ts:63-137`) →
  supervisor-owned `RpcSession`. Components never touch sessions directly —
  they use environment-scoped atoms/commands (§4). Retry ladder 1→16 s;
  subscriptions re-attach automatically; queries refire per connect generation;
  a deliberately disconnected environment must not be re-dialed by a mounting
  panel (`docs/architecture/connection-runtime.md:289-317`).
- Capability negotiation: descriptor capabilities all decode-default to false
  (`packages/contracts/src/environment.ts:30-42`: `repositoryIdentity`,
  `worktreeCatalog`, `vcsStatusSummary`, …) plus the
  `remoteProtocolVersion` window (`:44-45`; verdicts in
  `client-runtime/src/connection/compat.ts`). Pattern: read the capability
  from the same session used for the request
  (`client-runtime/src/state/vcs.ts:78-99`;
  `docs/architecture/connection-runtime.md:326-355`).

Constraints on the git manager panel:

1. All git operations already run on the server owning the repo — the panel
   must simply issue `vcsEnvironment.*` atoms with the project's
   `environmentId` and never call a local git or resolve paths client-side.
2. Git RPCs are `cwd`-addressed (`VcsStatusInput{cwd}`,
   `packages/contracts/src/git.ts:109-112`): resolve `projectId →
   EnvironmentProject.workspaceRoot` via `useProject(ref)`
   (`apps/web/src/state/entities.ts:122-125`); the path is a remote-host
   absolute path — treat it as opaque.
3. Any new panel store must key by `(environmentId, projectId)` (use
   `projectKey`), never bare `projectId` — ids can collide across environments.
4. New RPCs the git manager adds must be capability-gated for older/third-party
   servers (default-false booleans), like `vcsStatusSummary` is today.
5. Handle `EnvironmentRpcUnavailableError` (no live session,
   `rpc/client.ts:16-22`) distinctly from git errors; render pending state
   before the first connect generation.

## 6. Projects model

- Server record: `OrchestrationProject`
  (`packages/contracts/src/orchestration.ts:266-286`) — branded `ProjectId`
  (`baseSchemas.ts:32-33`), `title`, `workspaceRoot`, optional
  `repositoryIdentity` (`environment.ts:79-88`: `canonicalKey`, `locator`,
  provider/owner/name), scripts, worktree discovery policy. No
  `environmentId` on the record — the client scopes it:
  `EnvironmentProject extends OrchestrationProjectShell { environmentId }`
  (`packages/client-runtime/src/state/models.ts:11-13`).
  (`packages/contracts/src/project.ts` is the per-project _filesystem_ RPC
  surface, not the record.)
- Client state pipeline: `orchestration.subscribeShell` subscription
  (`client-runtime/src/state/shell.ts:287`) → per-environment snapshot atom →
  `createEnvironmentProjectAtoms`
  (`client-runtime/src/state/projectEntities.ts:18-108`:
  `environmentProjectsAtom`, `projectAtomFamily` keyed by `projectKey(ref)`) →
  hooks `useProjects`/`useProject`
  (`apps/web/src/state/entities.ts:104-125`).
- Logical vs physical project
  (`packages/client-runtime/src/state/projectGrouping.ts`, re-exported as
  `apps/web/src/logicalProject.ts`): physical key =
  `environmentId + normalized workspaceRoot` (`:63-71`); logical key =
  repository identity (± intra-repo path), deliberately
  environment-independent (`deriveLogicalProjectKey` `:118-137`) — this is how
  one sidebar row spans a repo's local and remote checkouts. "One git manager
  per project" must decide which of these "project" means (see Open questions).
- Existing per-project singleton precedents: one draft thread per logical
  project (`apps/web/src/composerDraftStore.ts:341-364`), per-project mutation
  lanes (`client-runtime/src/state/worktrees.ts:331-339`), per-project worktree
  catalog subscriptions (`worktrees.ts:423-428`), per-project expansion/order
  in `uiStateStore.ts:25-26`.
- Threads reference projects by `projectId`
  (`orchestration.ts:497-508`, with `branch`/`worktreePath`); a project =
  primary checkout + N worktrees sharing one repository
  (`docs/reference/encyclopedia.md:8-45`,
  `docs/reference/workspace-layout.md:54-67`).

## 7. Conventions

### Web (apps/web)

- React 19 + React Compiler (babel plugin), Vite/vite-plus
  (`apps/web/package.json`). Styling: Tailwind CSS 4 utility classes +
  `class-variance-authority` + `tailwind-merge` (`cn`); component primitives
  from `@base-ui/react`; icons `lucide-react`; drag-and-drop `dnd-kit`;
  virtualized lists via `@legendapp/list`; diffs via `@pierre/diffs` (worker
  pool) and trees via `@pierre/trees`; router TanStack (file-based routes in
  `apps/web/src/routes/`).
- State: Effect Atom for server data (`@effect/atom-react`,
  `connectionAtomRuntime`), zustand (+`persist` on versioned
  `bibcode:*` localStorage keys with migrate/sanitize functions) for local UI
  state. Logic is split into pure `.logic.ts`/plain `.ts` modules colocated
  with components.
- Tests colocated as `*.test.ts` / `*.test.tsx` (happy-dom, msw, fake-indexeddb;
  `.dom.test.tsx` for DOM-heavy cases), run with `vp test`
  (`docs/reference/scripts.md`).

### Server (apps/server)

- One directory module per domain (`git/`, `vcs/`, `worktree_catalog/`,
  `source_control/`, `production/` for RPC wiring, `rpc/`, `auth/`); RPC
  registration modules named `production/*_rpc.rs`; typed tagged error enums
  mirrored in contracts fixtures; unit tests inline (`#[cfg(test)]`, e.g.
  `auth/scope.rs:144`) plus integration tests in `apps/server/tests/`
  (`rpc_wire.rs`).
- Requirements per AGENTS.md: `cargo fmt --all --check`, relevant Rust tests,
  Clippy with warnings denied; `vp check` + `vp run typecheck` for the
  workspace; `vp run check:contracts` after any contract change.

### Contracts

- Schema-only (no runtime logic); every WS method has a Rust mirror, parity
  fixtures, and exactly one auth scope; deterministic fixture export scripts
  under `packages/contracts/scripts/`.

## 8. Open questions for the plan

1. **Which working tree does the panel target?** A project's repository spans
   the primary checkout plus worktrees. The existing Source Control panel is
   per-thread (one `cwd`); a project-scoped manager must pick a default `cwd`
   (primary `workspaceRoot`?) and decide whether/how to switch among worktrees.
2. **What does "one per project" key on** — physical `(environmentId,
   projectId)` or the sidebar's logical project (which can span environments)?
   The sidebar button will need member disambiguation either way
   (`runProjectMemberAction`).
3. **Where does the panel live in the thread-keyed center layout?** Option (a):
   a `git-manager` surface inside the current thread's layout with open-time
   dedup/focus; option (b): a project-keyed slice parallel to `byThreadKey`.
   (a) is the smaller change but means the same project's manager can exist in
   several threads' layouts unless dedup is cross-thread.
4. **LRU-of-2 mounting** is a new policy in
   `CenterPanelSurfaceHosts.tsx:249-252`; define eviction (most-recently-
   visible) and whether server-side subscriptions (status stream) are held only
   while mounted or for all open tabs.
5. **RPC additions and sequencing**: standalone `vcs.commit`/`vcs.push` (or
   reuse the stacked stream), per-commit diff source kind for
   `review.getDiffPreview`, fetch, stash, branch delete, hunk staging — each is
   a full §4 checklist item plus a capability flag; hunk staging additionally
   needs new server-side plumbing (`apply --cached`) and conflict states need
   schema changes (`git.ts:48-55`).
