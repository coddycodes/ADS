# Research: BibCode left panel, search, and connected environments — groundwork for an "Agents" view

Date: 2026-08-31. Read-only research against worktree `develop-3` (branch
`mubeda/develop-3`, clean tree). All paths are repo-relative; `path:line`
citations were verified against current source. CodeGraph was initialized and
synced in this worktree before research.

## Executive summary

- The left panel is a **two-column layout**: a 52px `EnvironmentRail` column
  (environment switcher) plus the main `ThreadSidebar` column
  (`apps/web/src/components/AppSidebarLayout.tsx:92-96`). The main column
  renders: brand header → environment context card (remote only) → a
  **"Search" row that is a button, not an input** (it opens the command
  palette) → conditional warnings → the **Projects** group (projects with
  nested thread rows) → separator → footer (update pills + Settings).
- **There is no inline search field.** The "search field" is
  `CommandDialogTrigger` at `apps/web/src/components/Sidebar.tsx:3842-3864`;
  query state and filtering live entirely in the command palette
  (`CommandPalette.tsx:211-212`, `CommandPalette.logic.ts:215-279`), which
  searches projects (title + workspace root) and non-archived threads
  (title + project title + branch) across **all** environments.
- BibCode is **already multi-connection**: `EnvironmentRegistry` supervises one
  live WebSocket per catalog entry simultaneously (spec decision D3 "soft
  switch" — `docs/plans/remote-servers/remote-servers-spec.md:50`;
  `docs/architecture/connection-runtime.md:359-368`). Rail selection scopes
  _presentation only_ via `activeEnvironmentIdAtom`; connections to other
  environments stay live and streaming.
- The entity closest to an "agent" is the **thread shell**
  (`OrchestrationThreadShell`, `packages/contracts/src/orchestration.ts:497-534`)
  with its embedded live `session` (`OrchestrationSession`,
  `orchestration.ts:345-372`: status idle/starting/running/ready/interrupted/
  stopped/error, provider, active turn, last error) plus attention booleans
  (`hasPendingApprovals`, `hasPendingUserInput`, `hasActionableProposedPlan`).
- **A cross-environment aggregate already exists and is live**:
  `threadShellsAtom` concatenates every catalog environment's scoped thread
  shells (`packages/client-runtime/src/state/threadShell.ts:149-173`), exposed
  as `useThreadShells()` (`apps/web/src/state/entities.ts:112-114`) — the
  sidebar consumes it today and then _narrows_ it by rail selection
  (`Sidebar.tsx:4051-4059`). An Agents view over all connected environments
  needs **no new RPC**, only a new presentation section — and one product
  decision about whether it deliberately bypasses the rail-scoping invariant.

---

## 1. Left panel structure (apps/web)

### Outer shell

`apps/web/src/components/AppSidebarLayout.tsx`:

- `AppSidebarLayout` (:56) wraps content in `SidebarProvider` →
  `<Sidebar side="left" collapsible="offcanvas" resizable>` (:80-91).
- Inside is a two-column flex row (:92): `<EnvironmentRail />` (:93) and the
  main column hosting `ThreadSidebar` — the default export of
  `apps/web/src/components/Sidebar.tsx` (:94-96).
- `<SidebarRail />` (:98) is the resize handle; width persists to localStorage
  key `chat_thread_sidebar_width` (:12).

### Top-to-bottom render order of the main column

Main return of `Sidebar()` at `Sidebar.tsx:4705-4812` (dialogs/portals
omitted):

| Order | Element                                                                       | Renderer                                                                                | Lines                                                                         |
| ----- | ----------------------------------------------------------------------------- | --------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------- |
| 1     | Header bar (mobile trigger + brand link + stage pill)                         | `SidebarChromeHeader` / `SidebarBrand`                                                  | Sidebar.tsx:3636-3657, 3669-3688                                              |
| 2     | Environment context card (remote env only)                                    | `SidebarEnvironmentContextCard` → `EnvironmentContextCard`                              | Sidebar.tsx:279-312, 4755                                                     |
| 3     | **"Search" row** (opens command palette)                                      | `SidebarGroup` → `SidebarMenu` → `CommandDialogTrigger` inside `SidebarProjectsContent` | Sidebar.tsx:3842-3864                                                         |
| 4     | ARM64/Intel build warning (conditional)                                       | `SidebarProjectsContent`                                                                | Sidebar.tsx:3865-3887                                                         |
| 5     | Local secondary backend status alerts                                         | `LocalSecondaryStatus`                                                                  | Sidebar.tsx:3888, 3366-3443                                                   |
| 6     | **"Projects" section header** (uppercase label + sort menu + add-project `+`) | `SidebarProjectsContent`                                                                | Sidebar.tsx:3889-3922                                                         |
| 7     | Project list (DnD variant when sort is `manual`, else plain)                  | `SidebarProjectItem` / `SidebarProjectListRow`                                          | Sidebar.tsx:3924-3995                                                         |
| 8     | Availability/empty state ("No projects yet", degraded, recovery)              | `SidebarProjectAvailability`                                                            | Sidebar.tsx:3997-4006; `components/sidebar/SidebarProjectAvailability.tsx:37` |
| 9     | Separator                                                                     | `SidebarSeparator`                                                                      | Sidebar.tsx:4807                                                              |
| 10    | Footer: provider-update pill, app-update pill, Settings                       | `SidebarChromeFooter`                                                                   | Sidebar.tsx:3690-3718                                                         |

Important structural facts:

- **Settings branch**: when the route starts with `/settings`
  (`isOnSettings`, Sidebar.tsx:4070) the entire body (items 2-9) is replaced by
  `SettingsSidebarNav` (Sidebar.tsx:4752), and the footer is not rendered. An
  Agents section placed in `SidebarProjectsContent` is invisible on settings
  routes.
- **No global "New thread" button, no Pinned section, no Archive section.**
  New-thread entry points are per-project (Sidebar.tsx:3144, 3160). Pinning is
  per-row ordering (`orderRowsWithPins`, Sidebar.tsx:1758-1762; state in
  `apps/web/src/sidebarWorkspaceMetaStore.ts`, keyed by `scopedThreadKey`).
  Archived threads are filtered out (`archivedAt === null` filters at
  Sidebar.tsx:4437-4440 and :1756), not shown in a section.
- **Per-project thread rows**: `SidebarProjectItem` (memo, Sidebar.tsx:1518)
  renders the project header, then `WorktreeDiscoverySection` (:3179),
  `SidebarPrimaryRow` (:3186, the `kind: "default"` thread), and
  `SidebarProjectThreadList` (:3200) with "Show more/less" overflow
  (:1413-1495) bounded by the `sidebarThreadPreviewCount` client setting.
- **Project grouping**: projects sharing a repository identity are collapsed
  into one `SidebarProjectSnapshot` — including **across environments**
  (`apps/web/src/sidebarProjectGrouping.ts:18, 57, 71`; consumed at
  Sidebar.tsx:4248-4279; tested with a remote environment in
  `apps/web/src/environmentGrouping.test.ts`).

### Section composition pattern

There is no generic "Section" component. Sections are ad-hoc compositions of
primitives from `apps/web/src/components/ui/sidebar.tsx` (exports at
:991-1017: `SidebarGroup`, `SidebarGroupLabel`, `SidebarMenu`,
`SidebarMenuItem`, `SidebarMenuButton`, `SidebarMenuSub`, `SidebarSeparator`,
…). The Projects group (Sidebar.tsx:3889-4007) is the template a new Agents
section would copy: `SidebarGroup` → raw `<span>` uppercase header row with
action buttons → `SidebarMenu` (with lazy auto-animate ref) → empty-state
component. The minimal template is the search row group (:3842-3864).

### Expand/collapse and per-row metadata persistence

1. **Project expansion (persisted)** — `apps/web/src/uiStateStore.ts`: plain
   zustand store with a debounced writer to localStorage key
   `bibcode:ui-state:v1` (:5). Keys: `projectExpandedById`, `projectOrder`
   (:24-27); helpers `resolveProjectExpanded` (:331-342, defaults expanded)
   and `setProjectExpanded` (:344-362). A new Agents section's collapse state
   would add a key here.
2. **Thread-list overflow (ephemeral)** — local `useState` set at
   Sidebar.tsx:4125-4127.
3. **Pin/unread (persisted)** — `sidebarWorkspaceMetaStore.ts` (zustand
   `persist`, name `bibcode:sidebar-workspace-meta:v1` (:15), selectors
   `selectIsPinned`/`selectIsUnread` (:85, :89), keyed by `scopedThreadKey`).
4. **Sort/group prefs** are _client settings_, not UI store:
   `sidebarThreadSortOrder`, `sidebarProjectSortOrder`,
   `sidebarProjectGroupingMode`, `sidebarThreadPreviewCount` via
   `useClientSettings` (Sidebar.tsx:4071-4076; contracts in
   `packages/contracts/src/settings` — see Sidebar.tsx:93-98 imports).

### Performance patterns

No list virtualization anywhere. Instead: preview slicing per project
(`sidebarThreadPreviewCount`), heavy memoization (`SidebarThreadRow` :548,
`SidebarProjectItem` :1518, etc.), large `useMemo` chains for derived data
(:4016, :4265, :4317, :4441, :4473), `@formkit/auto-animate` attached lazily
via WeakSet-guarded callback refs (:4419-4435), thread-detail prewarming
capped at 10 (`SIDEBAR_THREAD_PREWARM_LIMIT`, `Sidebar.logic.ts:21`;
prewarmer mounts at Sidebar.tsx:4746-4748), stable empty sentinels, and DnD
mounted only in manual sort mode (:3924).

---

## 2. The search field

**Correction of the premise: there is no search input in the sidebar.** The
"search field" is a `CommandDialogTrigger` button (`SidebarMenuButton` with
`data-testid="command-palette-trigger"`, `SearchIcon`, label "Search", and the
`commandPalette.toggle` shortcut kbd) at
`apps/web/src/components/Sidebar.tsx:3842-3864`. It opens the global
**command palette**. "Below the search field" therefore concretely means:
inside `SidebarProjectsContent`, between the search-row group (:3842-3864)
and the Projects header row (:3889), a slot currently occupied only by the
conditional ARM64 warning (:3865-3887) and `LocalSecondaryStatus` alerts
(:3888).

Palette search mechanics:

- Query state: local `useState` + `useDeferredValue` in
  `apps/web/src/components/CommandPalette.tsx:211-212`; applied at :447-453.
- Filter helper: `filterCommandPaletteGroups(input: { activeGroups, query,
  isInSubmenu, projectSearchItems, threadSearchItems })` at
  `apps/web/src/components/CommandPalette.logic.ts:215`, behavior at
  :222-279. Leading `>` restricts to actions; empty query returns active
  groups; otherwise synthetic `projects-search`/`threads-search` groups are
  appended. Matching is normalized substring over `searchTerms` (:260-261),
  ranked exact > prefix > substring with earlier-term preference
  (`rankCommandPaletteItemMatch` :196-213).
- What is searchable: projects — `searchTerms: [title, workspaceRoot]`
  (`buildProjectActionItems`, :93-103); non-archived threads —
  `searchTerms: [thread.title, projectTitle, thread.branch]`
  (`buildThreadActionItems`, :122-163, archived filtered at :136). The thread
  source is the global `useThreadShells()` (`CommandPalette.tsx:219`), so the
  palette already searches **across all environments** regardless of rail
  selection.

---

## 3. Remote Servers / connected environments

### Model (client-runtime + contracts)

Living doc: `docs/architecture/connection-runtime.md`. Ownership chain
(:8-35): `ConnectionResolver` (catalog entry → `PreparedConnection`) →
`ConnectionDriver` (opens an `RpcSession`, verifies storage identity) →
`EnvironmentSupervisor` (desired state, retries, live session for **one**
environment) → `EnvironmentRegistry` (owns catalog entries and one scoped
supervisor per environment). Composition root:
`packages/client-runtime/src/connection/layer.ts`.

- Targets (`packages/client-runtime/src/connection/model.ts`; table at
  connection-runtime.md:37-54): `PrimaryConnectionTarget` (host-provided
  local), `BearerConnectionTarget` (direct, optionally E2EE via pinned
  `hostKey`), `RelayConnectionTarget` (BiBCode Connect), `SshConnectionTarget`
  (desktop SSH gateway), `UnavailableConnectionTarget`.
- Supervisor phases: `available | offline | connecting | backoff | connected |
  blocked`, with 1/2/4/8/16s transient backoff
  (connection-runtime.md:289-305).
- `environmentId` is the logical routing identity; `storageInstanceId` gates
  synchronization against a swapped persistent store
  (connection-runtime.md:432-441).
- Compat verdict per environment: `compat.ts` computes
  `compatible | legacy | server-too-old | client-too-old` from the descriptor
  (connection-runtime.md:447-463).
- **One server = one environment** — there is no server-side
  multi-environment multiplexing; Remote Servers is a client-side catalog
  feature (`docs/plans/remote-servers/remote-servers-spec.md:413-415`).

### "All connected environments" concretely: multi-connection is the design

Spec decision **D3 (soft switch)**: "connections to paired servers stay alive
in the background; running sessions keep streaming; rail selection scopes the
view only" (`remote-servers-spec.md:50`). Decision **D4 (entity-ownership
routing)**: an operation on an entity owned by environment X targets X
regardless of rail selection (`remote-servers-spec.md:51`). The living doc
confirms: "Selection never changes supervisor desired state—connections to
other environments stay live and streaming—and operations on an entity always
route to the entity's own `environmentId` regardless of selection"
(`docs/architecture/connection-runtime.md:359-368`). So the client holds
**multiple live WebSocket connections concurrently**, one per desired catalog
environment; there is no connect-on-select.

Caveat: an environment can be explicitly **disconnected** (desired state
latch, `EnvironmentRegistry` intent — connection-runtime.md:307-314) or
transiently unreachable; its cached shell snapshot remains the render source
but is non-authoritative (`shell projection authority`,
connection-runtime.md:393-408). "All connected environments" in practice
means "all desired catalog environments, each rendered from live or cached
data with a per-environment availability status."

### UI presentation of environments

- **`EnvironmentRail`** (`apps/web/src/components/sidebar/EnvironmentRail.tsx:126`,
  mounted as a sibling column at `AppSidebarLayout.tsx:93`): radiogroup with a
  Local entry (WSL sub-picker via `local:` connection-id prefix grouping), a
  divider, one `RemoteEntryButtonWithUpdate` per saved remote (:249-263) with
  avatar + status dot (connected/disconnected/attention/error, :28-33), then
  "Add server…" and "Manage remote servers…" (:265-302). Selection calls
  `setActiveEnvironmentId` (:151-153).
- **`activeEnvironmentIdAtom`** lives at `apps/web/src/state/entities.ts:59`
  with hooks `useActiveEnvironmentId`/`readActiveEnvironmentId`/
  `setActiveEnvironmentId` (:64-74).
- **Scoping rule**: `selectRailVisibleEnvironmentIds`
  (`apps/web/src/components/sidebar/environmentRail.logic.ts:147-167`)
  returns the set of local environment ids when Local (or nothing) is
  selected, or a singleton set for a selected remote. The sidebar filters
  both projects and threads with it (Sidebar.tsx:4016-4028, 4042-4058). The
  spec pins the invariant: "no selection must never render as 'show
  everything'" (`remote-servers-spec.md:404-410`).
- **`EnvironmentContextCard`**
  (`apps/web/src/components/sidebar/EnvironmentContextCard.tsx:30`): shown
  under the brand row only when a remote environment is active; name, status,
  version, compat badge, ⋯ menu (Disconnect / Check for updates / Manage…).
- **Settings**: `/settings/remote-servers` with Connect/Share tabs
  (`apps/web/src/components/settings/remote-servers/ConnectTab.tsx` exists and
  itself consumes the global `useThreadShells()` at :742 to warn about
  running work).

### Environment state atoms available to a new view

- Catalog + per-environment connection state:
  `createEnvironmentCatalogAtoms`
  (`packages/client-runtime/src/state/connections.ts:28-160`) —
  `catalogValueAtom` (`{ isReady, entries: Map<EnvironmentId, ConnectionCatalogEntry> }`),
  `networkStatusValueAtom`, `stateAtom(environmentId)` (Atom.family over the
  supervisor's `SubscriptionRef`), plus commands (`connect`, `disconnect`,
  `retryNow`, `acceptStorageIdentity`, …). Web instantiation:
  `apps/web/src/connection/catalog.ts:5` (`environmentCatalog`).
- Presentation per environment: `useEnvironments()` /
  `useEnvironment(environmentId)` in `apps/web/src/state/environments.ts:49-99`
  (label, displayUrl, relayManaged, connection presentation);
  `useEnvironmentConnectionState(environmentId)` (:110-112).
- Per-environment availability for data rendering: `EnvironmentShellState`
  with `EnvironmentAvailabilityStatus = starting | synchronizing | live |
  degraded | storage-changed | recovery-required | unavailable |
  configuration-error` (`packages/client-runtime/src/state/shell.ts:33-47`).

---

## 4. Sessions/agents data model and live updates

### What is an "agent" today

Three layers (all verified in contracts):

1. **Thread shell — the UI-facing agent entity.** `OrchestrationThreadShell`
   (`packages/contracts/src/orchestration.ts:497-534`): `id`, `projectId`,
   `title`, `modelSelection`, `runtimeMode`, `interactionMode`, optional
   `kind`, `branch`, `worktreePath`, `latestTurn`, `createdAt`/`updatedAt`/
   `archivedAt`, embedded `session`, `latestUserMessageAt`, and attention
   flags `hasPendingApprovals`, `hasPendingUserInput`,
   `hasActionableProposedPlan`, `unresolvedDelivery`. The shell has **no
   `environmentId`** — scoping is added client-side:
   `EnvironmentThreadShell extends OrchestrationThreadShell { environmentId }`
   (`packages/client-runtime/src/state/models.ts:15-17`, via
   `scopeThreadShell` :32-37). Refs: `ScopedThreadRef = { environmentId,
   threadId }` (`packages/contracts/src/environment.ts:96-100`).
2. **Session — the live runtime of the agent.** `OrchestrationSession`
   (`orchestration.ts:345-372`): `status` (`OrchestrationSessionStatus =
   idle | starting | running | ready | interrupted | stopped | error`,
   :345-353), `providerName`, `providerInstanceId`, `runtimeMode`,
   `activeTurnId`, `lastError`(+class), `updatedAt`. Embedded in the shell,
   so **live agent status arrives on the shell stream without opening the
   thread**.
3. **Provider instance — configuration/routing, not a process.**
   `ProviderInstanceId` is a user-defined slug
   (`packages/contracts/src/providerInstance.ts:70-95`); one instance backs
   many threads.

A fourth, finer-grained layer is the **activity protocol v2** (subagents and
background tasks per thread/terminal scope): actors + work items + entries
with lifecycle `starting | running | waiting | unknown | completed | failed |
cancelled | interrupted` (`packages/contracts/src/activity.ts:51-61,
124-142`; living doc `docs/architecture/activity-observation.md:24-45`).
It is capability-gated (`activityProtocolVersion: 2`), subscribed **per open
thread scope only** with `idleTTL = 0`
(`packages/client-runtime/src/state/activity.ts:41, 667-736`), and rendered
in the center/right "Subagents"/"Background Tasks" docks
(`apps/web/src/components/activity/ActivityDock.tsx`; expansion state in
`apps/web/src/activityDockStore.ts`, keyed `environmentId:projectId`).

Naming traps: `apps/web/src/routes/settings.agents.tsx` →
`AgentsSettingsPanel` is **default-agent settings**, not running agents; and
`packages/client-runtime/src/state/session.ts` / `apps/web/src/state/session.ts`
are the **transport** session (connection bootstrap), not agent sessions.

### Status the sidebar derives today

`resolveThreadStatusPill` (`apps/web/src/components/Sidebar.logic.ts:445-498`,
priority table :126-133): `Pending Approval (5) > Awaiting Input (4) >
Working/Connecting (3) > Plan Ready (2) > Completed (1)`, mapped from
`hasPendingApprovals`, `hasPendingUserInput`, `session.status === "running"`
(pulsing "Working"), `"starting"` ("Connecting"), and
`hasActionableProposedPlan`. An Agents view can reuse this exact policy
function.

### Live-update pipeline

- Wire protocol: Effect RPC over **one authenticated WebSocket per connected
  environment** (`docs/architecture/rpc-and-orchestration.md:1-39`; client
  protocol `packages/client-runtime/src/rpc/protocol.ts`, Rust mirror
  `apps/server/src/rpc/message.rs`).
- Shell stream (thread/project lists): methods in
  `packages/contracts/src/orchestration.ts:27-35` (`subscribeShell`,
  `subscribeThread`, `dispatchCommand`, …). Wire shapes:
  `OrchestrationShellSnapshot { snapshotSequence, projects, threads,
  updatedAt }` (:536-542) then sequenced `OrchestrationShellStreamEvent`s
  (`project-upserted | project-removed | thread-upserted | thread-removed`,
  :544-566).
- Client sync: `packages/client-runtime/src/state/shell.ts:255-305`
  subscribes `subscribeShell` per environment only while
  `connection.phase === "connected"`, generation/session-fenced, persists to
  the per-environment cache; exposed via `createEnvironmentShellAtoms` →
  `stateAtom(environmentId)` (:504-523). Snapshot retention through
  reconnects is authoritative-only (connection-runtime.md:393-418).
- Thread detail (chat content): separate `subscribeThread` per open thread
  (`packages/client-runtime/src/state/threads.ts:199-206`), 5-minute
  `Atom.setIdleTTL` (:229-248), status per thread
  `EnvironmentThreadStatus = empty | cached | synchronizing | live | deleted`
  (:26). An Agents view does **not** need this layer for status.
- Web wiring: `apps/web/src/state/shell.ts` (environment snapshot atom) and
  `apps/web/src/state/threads.ts:23-26`
  (`environmentThreadShells = createEnvironmentThreadShellAtoms({
  catalogValueAtom, snapshotAtom })`).

### The existing cross-environment aggregate

`createEnvironmentThreadShellAtoms`
(`packages/client-runtime/src/state/threadShell.ts:32-186`) provides:

- per-environment atoms: `environmentThreadsAtom` (:38),
  `environmentThreadRefsAtom` (:55), `environmentThreadRefsByProjectAtom`
  (:70);
- **global aggregates**: `threadRefsAtom` (:150-160) iterates
  `get(input.catalogValueAtom).entries.keys()` and concatenates every
  environment's refs; `threadShellsAtom` (:162-173) maps refs to
  `EnvironmentThreadShell`s. Both are referentially memoized
  (`threadRefsEqual` / `arrayElementsEqual`) so consumers re-render only on
  real change.

Hooks: `useThreadShells()` (`apps/web/src/state/entities.ts:112-114`) and
`useThreadShellsForProjectRefs(refs)` (:116-120) — both already span
environments. Current consumers: `Sidebar.tsx:4051-4059` (then narrowed by
`visibleEnvironmentIds`), `CommandPalette.tsx:219`, `ConnectTab.tsx:742`.
Mounting the aggregate transitively mounts every catalog environment's shell
subscription; the sidebar already does this today, so an Agents view adds no
new subscription cost at shell granularity.

Precedent for an explicitly-keyed multi-environment fan-out (if needed):
`createArchivedThreadSnapshotsAtomFamily`
(`packages/client-runtime/src/state/archivedThreads.ts:40-69`), keyed by a
sorted joined environment-id string, rolling up
`{ snapshots, error, isLoading }`; web wrapper
`apps/web/src/lib/archivedThreadsState.ts:20-50`.

---

## 5. State management patterns to follow

1. **Effect Atom (`effect/unstable/reactivity` + `@effect/atom-react`) for
   server-derived state.** Pattern: a `create<Domain>Atoms(runtime | inputs)`
   factory in `packages/client-runtime/src/state/*` returning `Atom.family`
   per environment/entity plus derived global atoms, instantiated once in
   `apps/web/src/state/*` or `apps/web/src/connection/*`, consumed via
   `useAtomValue`. Examples: `createEnvironmentCatalogAtoms`
   (`connections.ts:28`), `createEnvironmentShellAtoms` (`shell.ts:504`),
   `createEnvironmentThreadShellAtoms` (`threadShell.ts:32`),
   `createEnvironmentActivityAtoms` (`activity.ts:667`).
2. **Commands** via `createRuntimeCommand` + `createAtomCommandScheduler`
   (`connections.ts:31-32, 80-144`) with serial/keyed concurrency; React
   invokes through `useAtomCommand` (`apps/web/src/state/use-atom-command.ts`).
3. **Async query envelope**: `useEnvironmentQuery(atom)` returns
   `{ data, emission, error, isPending, refresh }`
   (`apps/web/src/state/query.ts:26-39`).
4. **Referential memoization inside atoms** (previous-value capture +
   `arrayElementsEqual`) rather than React-side deep compares
   (`threadShell.ts:140-173`).
5. **Zustand only for UI-local persisted preferences**: `uiStateStore.ts`
   (expansion/order, manual debounced persistence, key
   `bibcode:ui-state:v1`), `sidebarWorkspaceMetaStore.ts` (pins/unread,
   `persist` middleware), `activityDockStore.ts` (dock expansion, bounded and
   sanitized). New Agents-view collapse/filter prefs belong here (or in
   client settings if they should sync like sort orders).
6. **Presentation policy as exported pure selectors** in `.logic.ts` files
   with colocated tests (`Sidebar.logic.ts`, `environmentRail.logic.ts`,
   `environmentContextCard.logic.ts`) — a new Agents section should put its
   row-derivation/filter/sort policy in an `agents*.logic.ts` with tests.
7. **React performance rules**: memoized row components, narrow selectors,
   `useShallow`, deferred values for search, no virtualization (bounded lists
   instead). AGENTS.md requires verifying apps/web changes against the
   `vercel-react-best-practices` skill (user memory note).

---

## 6. Where a new left-panel Agents section plugs in

Concrete files to touch:

- **`apps/web/src/components/Sidebar.tsx`** — insert the section inside
  `SidebarProjectsContent` (definition :3766) between the search-row group
  (:3842-3864) and the Projects header (:3889), or as a sibling component
  rendered from the main return next to `SidebarProjectsContent` (:4756) if
  it should not share that component's props. Copy the Projects group
  composition (`SidebarGroup` + uppercase header `<span>` + `SidebarMenu`).
- **New `apps/web/src/components/sidebar/AgentsSection.tsx`** (+
  `agentsSection.logic.ts` + tests) following the
  `EnvironmentContextCard`/`EnvironmentRail` file pattern in that directory.
- **Data**: `useThreadShells()` (`apps/web/src/state/entities.ts:112`) +
  `useEnvironments()` (`apps/web/src/state/environments.ts:49`) +
  `environmentCatalog.stateAtom(environmentId)` /
  `useEnvironmentConnectionState` for per-environment connection status +
  `resolveThreadStatusPill` (`Sidebar.logic.ts:445`) for row status. No new
  contracts or server work required at shell granularity.
- **Persistence**: a new expansion key in `apps/web/src/uiStateStore.ts`
  (follow `projectExpandedById`).
- **Navigation**: rows should link like existing thread rows
  (TanStack Router `Link` / `threadRoutes.ts`; thread keys via
  `scopedThreadKey` — note the two key encodings: NUL-separated `threadKey`
  for atom families (`packages/client-runtime/src/state/entities.ts:52-54`)
  vs colon-separated `scopedThreadKey` for React/DOM keys
  (`packages/client-runtime/src/environment/scoped.ts:18-36`)).

Constraints inherited from the Remote Servers UI:

- **Rail-scoping invariant (the sharp one).** The approved spec pins that
  rail selection scopes panel presentation and that "no selection must never
  render as 'show everything'" (`remote-servers-spec.md:404-410`);
  `Sidebar.tsx:4016-4058` implements it. An "agents across ALL connected
  environments" section deliberately bypasses this invariant for its own
  rows. That is a product decision to make explicitly (and possibly to
  record per the AGENTS.md design-approval rule), not a technical blocker.
- **Entity-ownership routing (D4)**: clicking an agent row must route to the
  entity's own `environmentId` regardless of rail selection — which the
  existing thread-row navigation already does; a cross-environment row click
  may additionally want to switch `activeEnvironmentIdAtom` so the rest of
  the panel follows.
- **Settings routes hide the body** (Sidebar.tsx:4750-4758) — the Agents
  section will not render there.
- **Cached vs live**: per shell-projection authority, rows from a
  disconnected/blocked environment render from cached snapshots; the section
  must be able to badge environment liveness
  (`EnvironmentShellState`/`EnvironmentAvailabilityStatus`,
  `packages/client-runtime/src/state/shell.ts:33-47`) rather than implying
  everything shown is live.
- **Naming collision**: "Agents" already means default-agent settings
  (`settings.agents.tsx`) and activity "actors" are "provider-attributed
  agents" (`activity.ts` docs). Pick copy/identifiers that do not collide.

---

## Open questions / constraints for the Agents view

1. **Rail selection vs "ALL environments".** Does the Agents section ignore
   `activeEnvironmentIdAtom` (cross-environment by definition, bypassing the
   spec's scoping invariant for this one section) or respect it (making it
   "agents in the selected environment")? If it ignores selection, should
   selecting a row also move the rail selection to the row's environment?
   Product decision; the spec invariant (`remote-servers-spec.md:404-410`)
   should be amended or explicitly excepted in the same change.
2. **Where aggregation happens — settled, not open.** Client-side, in
   client-runtime atoms: the server forbids cross-environment multiplexing
   ("one server = one environment", `remote-servers-spec.md:413-415`) and the
   client aggregate already exists (`threadShellsAtom`,
   `threadShell.ts:162-173`) and is already mounted by the sidebar. Any new
   derived "agents" atom should live beside it in
   `packages/client-runtime/src/state/` if it encodes shared policy, or in
   `apps/web` if it is pure presentation.
3. **What counts as an "agent" row.** Every non-archived thread (sidebar
   precedent: `archivedAt === null`)? Only shells with a non-null `session`?
   Only "active" statuses (`running`/`starting`/attention flags)? Does the
   undeletable per-project default thread count? This determines whether the
   view is "all sessions" or "currently working agents".
4. **Depth: shell-level vs activity actors.** Shell-level signal
   (`session.status` + `hasPendingApprovals`/`hasPendingUserInput`/
   `hasActionableProposedPlan`) is free — it rides the existing per-environment
   shell streams. Sub-agent actors (activity protocol v2) would require one
   live subscription per thread scope per environment (`idleTTL = 0`,
   capability-gated, per-provider support varies —
   `activity.ts:667-736`, `docs/architecture/activity-observation.md`).
   Recommendation from the evidence: shell-level for v1; actors are the
   expensive extension and need a fan-out/bounding design of their own.
5. **Search scope.** There is no inline sidebar filter today. Does the Agents
   view get its own inline input (a new pattern for the sidebar), or is
   "search agents" served by extending `filterCommandPaletteGroups` (which
   already searches threads across environments)? If inline, reuse
   `normalizeSearchText`/ranking from `CommandPalette.logic.ts` rather than a
   second matching policy.
6. **Presentation of disconnected/stale environments.** How are agents from a
   `backoff`/`blocked`/explicitly-disconnected environment shown — greyed
   with a status dot (rail precedent), grouped under an environment header
   with its availability status, or hidden? Cached rows must not be presented
   as live (shell projection authority, connection-runtime.md:393-408).
7. **Grouping and ordering.** Group by environment (matching the rail), by
   project (matching the Projects section), or flat sorted by
   activity/recency (`latestTurn`, `updatedAt`, `latestUserMessageAt`,
   `threadSort.ts` exists in client-runtime)? Pinned-first
   (`sidebarWorkspaceMetaStore`) or status-priority-first
   (`THREAD_STATUS_PRIORITY` in `Sidebar.logic.ts:126-133`)?
8. **Volume bounds.** No virtualization exists; the Projects section bounds
   rows via preview counts. A cross-environment agents list needs its own
   bound (e.g. status filter, per-environment cap, or "show more") to keep
   the no-virtualization pattern viable.
9. **Design-approval gate.** AGENTS.md requires an approved design document
   before implementing a non-trivial architectural decision; if the Agents
   view changes the rail-scoping semantics or adds a cross-environment
   activity fan-out, that design (this folder) needs explicit approval
   first. Runbook note: a purely additive left-panel section likely still
   triggers the `docs/testing/` review rule for "packaged UI flows included
   in native visual validation".
