# Research: Orca's "Agents" view (left panel / sidebar)

Primary-source research of the Orca codebase at `/work/github/orca` (Electron
app: `src/main` = Electron main process, `src/renderer` = React UI,
`src/shared` = shared types/logic). All paths below are relative to that repo
root. Line numbers were verified against the working tree on 2026-08-31.

## Executive summary

Orca does **not** have one monolithic "Agents panel". Agent presence in the
left sidebar is spread across three surfaces, with different gating:

1. **Primary, always-available surface — inline agent rows inside each
   worktree card.** The left sidebar is a worktree (workspace) list; each
   worktree card can render a live list of the coding-agent sessions running in
   that worktree's terminals (`src/renderer/src/components/sidebar/WorktreeCardAgents.tsx`).
   This is gated by the per-user card display property `'inline-agents'`
   (labelled **"Agent activity"** in the card options menu) and rendered in
   `'compact'` mode by default. This is the closest analog to a BibCode
   "Agents" view.
2. **A nav entry literally labelled "Agents"** in the sidebar's top navigation
   strip (`src/renderer/src/components/sidebar/SidebarNav.tsx:232-260`). It is
   experimental (`settings.experimentalActivity === true`), shows a Bell icon
   plus an unread-completions badge, and opens the **Activity page**
   (`src/renderer/src/components/activity/ActivityPrototypePage.tsx`) — a
   full-window prototype that groups agent "threads" by status/project/worktree
   with search. It is a flag-guarded prototype, not the main agents UI.
3. **An experimental Agent Dashboard popout**
   (`settings.experimentalAgentDashboardPopout`), hosted from the sidebar
   (`sidebar/AgentDashboardSidebarHost.tsx`, entry
   `sidebar/AgentDashboardSidebarEntry.tsx`). Not covered in depth here, but it
   shares the same row model (`DashboardAgentRow`).

The data model is event-driven: agent CLIs (Claude, Codex, Gemini, …) get
**hooks installed** by Orca; those hooks POST status JSON to a **loopback HTTP
server** in the Electron main process; main fans events out to the renderer
over IPC (`agentStatus:set` / `agentStatus:clear`, with an
`agentStatus:getSnapshot` invoke for hydration); the renderer applies them to a
Zustand store map `agentStatusByPaneKey`, from which per-worktree selectors
derive the sidebar rows. There is **no polling**: liveness comes from push
events plus a 30-second UI tick that decays stale statuses.

---

## 1. Where the Agents view lives in Orca's UI

### Sidebar composition

`src/renderer/src/components/Sidebar.tsx` just re-exports
`src/renderer/src/components/sidebar/index.tsx`, whose layout
(index.tsx:114-138) is, top to bottom:

- `<SidebarNav />` — fixed nav strip: **Search** button (opens the
  worktree/tab jump palette modal), Setup Guide entry, Tasks button, optional
  Artifacts / Skills / Automations / Agent Dashboard / **Agents** / Orca
  Mobile entries.
- `<SidebarHeader />` — projects header row (group-by toggle, filter menu,
  add-project, etc.).
- `<WorktreeList />` — the scrollable, virtualized worktree list. **This is
  where agent rows appear**, nested inside each `WorktreeCard`.
- `<SetupScriptPromptCard />` and `<SidebarToolbar />` — fixed bottom chrome.

The sidebar is resizable (220–500 px, index.tsx:27-28) and its width lives in
the store (`sidebarWidth`).

### The "Agents" nav button

`sidebar/SidebarNav.tsx:22-26`:

```ts
export function shouldShowAgentsButton(
  settings: Pick<GlobalSettings, "experimentalActivity"> | null | undefined,
): boolean {
  return settings?.experimentalActivity === true;
}
```

The button itself (SidebarNav.tsx:232-260) renders a `Bell` icon, the label
`'Agents'`, an unread count badge from
`useActivityUnreadCount(showAgentsButton, 'sidebar-badge')`, and calls
`openActivityPage` (a store action that sets `activeView = 'activity'`).
`aria-current='page'` marks it active when `activeView === 'activity'`.

### The inline agent list inside worktree cards

`sidebar/worktree-card-secondary-rows.tsx:67-73` mounts it:

```tsx
{showInlineAgentList && (
  <WorktreeCardAgents
    worktreeId={worktree.id}
    agents={agentActivityDisplayMode === 'compact' ? compactInlineAgentRows : undefined}
    ...
```

Gating (`sidebar/use-worktree-card-secondary-details.ts:88`):

```ts
const showInlineAgentList = cardProps.includes("inline-agents") && (newCardStyle || !compactCards);
```

`'inline-agents'` is one of the worktree-card display properties the user can
toggle per card list ("Agent activity",
`sidebar/worktree-card-display-property-options.ts:63-70`). The display mode is
a global setting `agentActivityDisplayMode: 'compact' | 'full'`, default
`'compact'` (`src/shared/constants.ts:35-39`).

### Relationship to search

The sidebar's **Search** button (SidebarNav.tsx:109-137) does not filter the
sidebar in place; it opens the `'worktree-palette'` modal —
`components/WorktreeJumpPalette.tsx`, whose input placeholder is "Search open
tabs, files, URLs, agents…" (i18n `en.json:3359`). The palette searches
worktrees, tabs, files, URLs **and agent sessions**, decorating rows with live
agent state (see §5). The `SidebarFilter` component is a separate facet menu
(hide sleeping workspaces, hide default branch, repo visibility, …) that
filters **worktrees**, not agent rows.

---

## 2. What an "agent" is in Orca's data model

An agent is **a live coding-agent session bound to a terminal pane**, keyed by
a stable **pane key**:

> `/** Composite key: \`${tabId}:${leafId}\` where leafId is a stable UUID layout leaf. */`—`src/shared/agent-status-types.ts:115-116`

Terminal tabs (`src/shared/terminal-tab-types.ts:5-56`) belong to a worktree
(`worktreeId`), can be split into panes (layout leaves), and may record
`launchAgent?: TuiAgent` — the agent Orca launched in the tab, used to show the
provider icon before the first hook event arrives
(terminal-tab-types.ts:40-45).

### Core status record — `AgentStatusEntry`

`src/shared/agent-status-types.ts:100-163` (abridged, field-for-field):

```ts
export type AgentStatusEntry = {
  state: AgentStatusState; // 'working' | 'blocked' | 'waiting' | 'done'
  workingMode?: "monitoring"; // background work, only while working
  prompt: string; // user's most recent prompt (cached per turn)
  updatedAt: number; // ms of last status update
  stateStartedAt: number; // ms when current state first reported
  agentType?: AgentType; // 'claude' | 'codex' | ... | arbitrary string
  model?: string;
  paneKey: string; // `${tabId}:${leafId}`
  terminalHandle?: string;
  worktreeId?: string; // attribution stamped by main
  connectionId?: string | null; // transport authority (SSH conn or local)
  tabId?: string;
  terminalTitle?: string;
  stateHistory: AgentStateHistoryEntry[]; // rolling log, cap 20
  toolName?: string; // e.g. "Edit", "Bash"
  toolInput?: string; // short preview (file path, command)
  interactivePrompt?: string; // full AskUserQuestion JSON, live only
  lastAssistantMessage?: string;
  lastCompletedAssistantMessage?: string;
  interrupted?: boolean; // done-by-cancel
  sessionBoundary?: boolean; // done that is a session boundary, not a turn
  orchestration?: AgentStatusOrchestrationContext; // parent/child dispatch context
  subagents?: AgentSubagentSnapshot[]; // live in-process children (max 32)
  providerSession?: AgentProviderSessionMetadata; // provider session id, for CLI resume
  promptInteractionKey?: string;
  restoredUnconfirmed?: boolean; // hydrated-from-disk, no live hook yet
};
```

Key vocabulary points:

- **Wire states are exactly four**: `AGENT_STATUS_STATES = ['working',
  'blocked', 'waiting', 'done']` (agent-status-types.ts:23). **`'idle'` is
  renderer-derived**, not a wire state — a fresh-but-quiet or stale entry decays
  to `'idle'` in the row builder (see §4).
- `agentType` is open-ended: a `WellKnownAgentType` union (claude, codex,
  gemini, amp, cursor, copilot, droid, grok, devin, …) plus `(string & {})`
  because "agent types aren't a fixed set (custom agents exist)"
  (agent-status-types.ts:26-51).
- Field lengths are hard-capped on parse (toolName 60, toolInput 160,
  lastAssistantMessage 8000, interactivePrompt 16000, agentType 40, model 120;
  agent-status-types.ts:234-268) so a buggy/malicious agent cannot bloat the
  cache or IPC.
- `AgentStatusOrchestrationContext` (agent-status-types.ts:71-82) carries
  `taskId` / `dispatchId` / `parentPaneKey` / `coordinatorHandle` etc. for
  panes spawned by another agent (multi-agent orchestration): "parent/child
  hierarchy is pane-level state, not worktree lineage".
- `AgentSubagentSnapshot` (agent-status-types.ts:88-98) describes in-process
  children (Claude Task-tool subagents/teammates) with their own
  `state: 'working' | 'blocked' | 'waiting' | 'idle'`; the sidebar renders them
  as indented child rows with no PTY of their own.

### Row model — `DashboardAgentRow`

The sidebar and the dashboard share one row shape,
`src/renderer/src/components/dashboard/useDashboardData.ts:8-31`:

```ts
export type DashboardAgentRow = {
  paneKey: string; // synthetic for 'subagent' rows
  entry: AgentStatusEntry;
  tab: TerminalTab;
  agentType: AgentType;
  rowSource?: "live" | "retained" | "subagent";
  state: AgentStatusState | "idle"; // 'idle' = stale-decayed
  activationPaneKey?: string; // subagent rows focus their parent's pane
  startedAt: number; // oldest stateHistory entry, else updatedAt
  lineage?: {
    depth: 0 | 1;
    parentPaneKey?: string;
    isFirstSibling: boolean;
    isLastSibling: boolean;
    childCount: number;
  };
};
```

### Retained (finished) agents — `RetainedAgentEntry`

`src/renderer/src/store/slices/agent-status.ts:55-65`: a snapshot of a
finished/vanished agent "kept so the dashboard and sidebar hover keep showing
the completion until the user clicks the worktree". It stores the full `tab`
snapshot (the tab may already be gone), `worktreeId`, `agentType`,
`startedAt`. Replacement policy `shouldReplaceRetainedWithLive`
(agent-status.ts:565-581) prefers newer `startedAt`, then provider-session
identity, then `updatedAt`.

### Where the list of agents comes from (store slices)

The renderer's Zustand store (`useAppStore`) holds, among others:

- `agentStatusByPaneKey: Record<string, AgentStatusEntry>` — live statuses
  (`store/slices/agent-status.ts`, main writer `setAgentStatus`, ~68 callers).
- `retainedAgentsByPaneKey: Record<string, RetainedAgentEntry>` — done
  snapshots.
- `migrationUnsupportedByPtyId` — panes whose identity migration failed;
  synthesized into a blocked pseudo-entry
  (`lib/migration-unsupported-agent-entry.ts:11-39`).
- `tabsByWorktree: Record<string, TerminalTab[]>`,
  `terminalLayoutsByTabId`, `runtimePaneTitlesByTabId`, `ptyIdsByTabId` —
  terminal topology used to attribute entries to worktrees and derive
  title-based fallback rows.
- `acknowledgedAgentsByPaneKey: Record<string, number>` — per-pane "seen at"
  timestamps for unread/bold treatment.
- `agentActivityDisplayMode`, `agentStatusEpoch`, plus orchestration indexes.

### Dual evidence model (important)

The comment atop `agent-status-types.ts:1-3` says explicit status "comes from
hooks … never inferred from terminal titles", but that describes only the
_explicit_ channel. Orca in fact runs **two evidence layers**:

1. **Hook status (authoritative)** — the `AgentStatusEntry` pipeline above.
2. **Terminal-title heuristics (fallback)** — for agents with no hooks
   installed. `classifyTitleActivity` / `getWorkingAgentsPerWorktree`
   (`renderer/src/lib/agent-status.ts:44-94`) scrape OSC-set pane/tab titles to
   detect "working" agents, gated on a live PTY so slept tabs don't count.
   `sidebar/worktree-title-derived-agent-rows.ts` (`buildTitleDerivedAgentRows`)
   turns title evidence into sidebar rows for panes that never produced a hook
   entry, and the smart sort records `'title-heuristic'` as an attention cause
   (`sidebar/smart-attention.ts:29-33`).

---

## 3. Rendering details

### Per-worktree row derivation

`sidebar/useWorktreeAgentRows.ts:42-123` — hook used by each worktree card.
Composition (in `useMemo`):

```ts
applyAgentRowLineage(
  buildWorktreeAgentRows({
    tabs,
    entries,
    retained,
    runtimePaneTitlesByTabId,
    ptyIdsByTabId,
    terminalLayoutsByTabId,
    runtimeAgentOrchestrationByPaneKey,
    now,
  }),
);
```

with each input pulled through **indexed per-worktree selectors**
(`sidebar/worktree-agent-row-selectors.ts`) wrapped in `useShallow`. The
comment at useWorktreeAgentRows.ts:48-52 explains why:

> "Subscribing to the whole agentStatusByPaneKey map would make every
> on-screen card re-render on any agent-status update anywhere — O(worktrees²)
> render amplification. Pre-filtering here means the card only re-renders when
> something relevant to THIS worktree changes."

The selector module caches a `tabId → worktreeId` index and per-worktree entry
buckets, rebuilt once per store-slice identity change and shared by all cards.

`buildWorktreeAgentRows` (`sidebar/worktree-agent-rows.ts:204-330`) does:

1. Bucket live entries by tabId (via `parsePaneKey`).
2. For each tab's entries: merge runtime orchestration context, compute
   freshness (`isExplicitAgentStatusFresh`, threshold
   `AGENT_STATUS_STALE_AFTER_MS = 30 min`,
   agent-status-types.ts:244-248), **decay stale
   working/blocked/waiting to `'idle'`**, emit a `'live'` row plus
   `buildSubagentChildRows(...)` child rows.
3. Add title-derived fallback rows (`buildTitleDerivedAgentRows`).
4. Add live rows for **worktree-attributed entries whose tab is not yet known
   to this renderer** ("orchestration workers can be attributed to a worktree
   by main before their tab is present", worktree-agent-rows.ts:268-296) using
   a synthesized fallback tab.
5. Append `'retained'` rows (state forced to `'done'`) for finished agents not
   already covered.
6. Sort deterministically.

### Sorting

`sidebar/worktree-agent-row-order.ts:17-24`:

```ts
compareWorktreeAgentRows =
  startedAt → tab.sortOrder → tab.createdAt → paneKey (ordinal)
```

i.e. agents are ordered by when they started, with stable tie-breaks so
"hook pings [that] rebuild the live entry list in a different iteration order"
never reshuffle the list (worktree-agent-rows.ts:326-328).

### Grouping

- **Within a card**: rows form a two-level **lineage tree** (parent agents and
  their dispatched children / subagents), built by `buildAgentRowLineageTree`
  (`dashboard/agent-row-lineage.ts`) from `orchestration.parentPaneKey` and
  `subagents`. Parents get a disclosure chevron with a child count; children
  render indented with tree guide-lines (`DashboardAgentRow.tsx:226-232`),
  container `role={hasLineage ? 'tree' : 'group'}` and
  `aria-label='Agents'` (WorktreeCardAgents.tsx:371-373, 413-414).
- **Across the sidebar**: grouping is by **worktree card**, which are
  themselves grouped by project/repo/host per the sidebar's `groupBy` setting
  (`sidebar/rendered-sidebar-worktree-order.ts:38-117` replays the
  buildRows → host-section → pinned-policy pipeline). Agent state also feeds
  the **worktree list's "Smart" sort**
  (`sidebar/smart-attention.ts:17-26`): class 1 = needs you
  (blocked/waiting), 2 = done recently, 3 = working, 4 = idle.
- **In the Activity page**: threads grouped by status / project / worktree via
  `buildActivityThreadGroups` (`activity/ActivityPrototypePage.tsx:1018,
  1058`), default `groupBy = 'status'` (line 1426).

### Row anatomy

Two renderers, chosen by `agentActivityDisplayMode`:

- **Full rows** — `dashboard/DashboardAgentRow.tsx:89-…`: agent icon
  (who) + `AgentStateDot` (what state) + primary text (conversation name ??
  prompt ?? state label) + model, tool preview (`toolName`/`toolInput`, only
  shown while working/waiting so "a leftover tool line [never] reads as
  still-running", lines 154-158), last assistant message, relative
  timestamps (`formatTimeAgo`, "just now / 5m ago / 2h ago / 3d ago"),
  interrupted marker, and an X-dismiss revealed on hover
  (`group/agent-row` scoping, line 199-200).
- **Compact rows** — `sidebar/worktree-card-compact-agent-row.tsx`:
  one dense line with dot + primary + secondary (tool preview → last
  assistant message → agent-type label; interrupted/monitoring get explicit
  labels, lines 40-60), short time ("now / 5m / 2h / 1d"), plus an optional
  prompt-cache countdown timer (`CacheTimer`).
- **Compact collapsed summary** — when compact rows exceed the threshold, a
  single **summary pill** (`CompactAgentSummaryButton`,
  WorktreeCardAgents.tsx:375-392) shows grouped counts by state, ordered
  waiting → blocked → working → monitoring → interrupted → done → idle
  (`sidebar/worktree-card-agent-summary.ts:11-19`), expandable in place.

### Status indicator vocabulary

`components/AgentStateDot.tsx` defines the shared `AgentDotState`:
`working` (spinner) | `monitoring` | `blocked` | `waiting` | `interrupted` |
`failed` | `done` (check icon) | `idle` (grey dot) | `permission` (question
glyph; the title-heuristic flow's collapsed blocked+waiting). The header
comment (lines 6-17) is a small design doc: two distinct glyphs per row — one
for _who_ (agent icon via `AgentIcon` / `agentTypeToIconAgent`,
`lib/agent-status.ts:100-145`) and one for _what state_ (the dot).
`asDotState` maps row state + `workingMode` into it; `interrupted === true`
overrides to the `interrupted` dot (DashboardAgentRow.tsx:172-175). The
worktree-level rollup maps hook state to card status via
`mapAgentStatusStateToVisualStatus` (working→working, blocked/waiting→
permission, done→done; `lib/agent-status.ts:167-177`).

### Click behavior and actions

`sidebar/WorktreeCardAgents.tsx:150-190`:

- **Click a live row** → `handleActivateAgentTab(tabId, paneKey)`:
  validates the pane key, then `activateAndRevealWorktree(worktreeId)`
  ("every user-initiated worktree switch must route through
  activateAndRevealWorktree — cross-repo activation + nav history") and
  `activateTabAndFocusPane(tabId, leafId, { ackPaneKeyOnSuccess: paneKey,
  flashFocusedPane: true, scrollToBottomIfOutputSinceLastView: true })`. So a
  click opens that worktree, that tab, focuses that pane, flashes it, and
  marks the row acknowledged. Malformed/mismatched rows are dismissed instead
  of guessed at.
- **Click a subagent row** → focuses the parent's pane
  (`activationPaneKey`, DashboardAgentRow.tsx:118-124).
- **Click a retained row** → deliberately inert ("activating would resume
  sleeping sessions", line 187-189).
- **X button (hover)** → `handleDismissAgent(paneKey)` =
  `dropAgentStatus` + `dismissRetainedAgent` (lines 103-109).
- **Send-target mode** — when the card's send-prompt popover is targeting
  agents, row clicks are captured to select this agent as prompt destination
  (`deriveRunningAgentSendTargets`, `sendPromptToSidebarAgentTarget`,
  lines 121-148).
- **Disclosure chevron** — toggles child rows; expansion state lives in the
  store, not local `useState`, so virtualizer recycling doesn't reset it
  (WorktreeCardAgents.tsx:197-199 comment).
- **No right-click context menu on agent rows** — `rg ContextMenu` over
  `DashboardAgentRow.tsx` and `worktree-card-compact-agent-row.tsx` finds
  none; kill/close is done in the terminal or tab bar, not from the agent row.
  Clicks on the agent list swallow bubbling so they don't trigger the card's
  own activate/edit handlers (WorktreeCardAgents.tsx:405-412).
- **Unvisited bolding** — `unvisitedByPaneKey` compares
  `acknowledgedAgentsByPaneKey[paneKey]` against `entry.stateStartedAt`; rows
  render bold until the tab is visited (WorktreeCardAgents.tsx:92-101).
- **Focused-pane highlight** — `useFocusedAgentPaneKey(worktreeId)` marks the
  row whose pane currently has focus.

---

## 4. Live updates — how the list refreshes

### Source: managed hooks + loopback HTTP server (main process)

- Orca ships **per-CLI hook installers** —
  `src/main/agent-hooks/managed-agent-hook-registry.ts:39-…` registers
  `install()` for claude, openclaude, codex, gemini, antigravity, amp, cursor,
  droid, command-code, grok, copilot, kimi, hermes, devin, … Each writes the
  agent's native hook config (e.g. Claude settings hooks) pointing at Orca.
- The **agent hook server** (`src/main/agent-hooks/server.ts`) is a loopback
  HTTP server; its port + auth are published atomically to an **endpoint
  file** (`ORCA_AGENT_HOOK_PORT`, …) that hook scripts read
  (`src/shared/agent-hook-listener/endpoint-publication.ts:9-40`). The
  parsing pipeline lives in `src/shared/agent-hook-listener/` so the SSH
  relay can host the identical pipeline without Electron (server.ts:2
  comment). Remote panes arrive via the relay/WSL ingest paths with a
  `connectionId` stamped.
- A secondary channel exists via terminal escape sequences (**OSC 9999** JSON
  payloads, `parseAgentStatusPayload`, agent-status-types.ts:409-421), and
  title changes feed the heuristic layer.
- Statuses are also persisted to `last-status.json` with a TTL; on startup
  they hydrate as `restoredUnconfirmed` entries that freshness gates treat as
  immediately stale (agent-status-types.ts:159-162).

### Fan-out: main → renderer IPC

`src/main/index.ts:1687-1775` — `agentHookServer.setListener(...)` builds an
`AgentStatusIpcPayload` (payload + `paneKey`, `tabId`, `worktreeId`,
`connectionId`, `receivedAt`, `stateStartedAt`, orchestration context,
provider session, …) and sends `mainWindow.webContents.send('agentStatus:set',
statusEvent)` (also to the dashboard popout window). Pane teardown sends
`agentStatus:clear`. The preload exposes subscription plus
`ipcRenderer.invoke('agentStatus:getSnapshot')` for full-state hydration
(`src/preload/index.ts:5115-5126`).

The wire shape is `AgentStatusIpcPayload`
(`src/shared/agent-status-ipc-payload.ts:26-47`); the clear shape supports
both single-pane and per-connection transient clears (SSH disconnect batches,
lines 50-56).

### Apply: renderer bridge → store

`renderer/src/hooks/ipc-events/agent-status-ipc-bridge.ts`
(`registerAgentStatusIpcBridge`) wires listeners and a batch path;
`agent-status-event-applicator.ts:36-295` (`createAgentStatusEventApplicator`)
is the guts. Notable mechanics a re-implementation should copy:

- **Attribution**: resolve `paneKey` → tab/worktree via a routing index; if
  the pane is unknown but the payload carries runtime-backed `worktreeId`
  attribution, accept it against the worktree (lines 82-92).
- **Pending queue + replay**: events for tabs the renderer doesn't know yet
  are queued (`enqueuePendingAgentStatus`) and replayed later instead of
  dropped (lines 93-115); an applied event flushes queued duplicates for the
  same pane.
- **Ordering/dedup guards**: drop events older than the stored `updatedAt`
  (lines 139-142); per-connection transient-clear watermarks drop late events
  from a disconnected transport (lines 119-125); connection-ownership check
  drops events from the wrong transport authority (lines 126-138).
- **Commit**: `store.setAgentStatus(paneKey, payload, terminalTitle, timing,
  routing, metadata)` plus post-commit completion-notification observation and
  optional tab-title sync.

### Refresh cadence in the UI

- **No polling.** Store writes re-run the narrow per-worktree selectors; a
  per-worktree **freshness signature** selector
  (`sidebar/worktree-agent-freshness-selector.ts`) plus one shared
  `useNow(30_000)` tick per non-empty card (WorktreeCardAgents.tsx:192-193;
  "zero-agent cards never mount this … idle worktrees pay no timer cost")
  re-evaluate stale decay and "Xm ago" labels.
- **Stale decay**: fresh threshold `AGENT_STATUS_STALE_AFTER_MS = 30 min`;
  quiet working/blocked/waiting rows render as `'idle'`
  (worktree-agent-rows.ts:236-249).
- **Paired web/mobile clients** (no Electron IPC): the host publishes
  session-tab snapshots whose terminal surfaces embed a **projected** agent
  status (`pickParsedAgentStatusPayload`, agent-status-types.ts:202-228 —
  deliberately excludes `launchToken`/`connectionId` etc. so transport
  secrets never reach paired clients). The web runtime mirrors them into the
  same `agentStatusByPaneKey` map
  (`renderer/src/runtime/web-session-tabs-sync.ts:4160-4300`,
  `applyWebSessionTabsStorePatch`, with `agentStatusEntryEqual` at 2262-2287
  to skip no-op writes).

---

## 5. Filtering / search interplay

- **Jump palette** (`components/WorktreeJumpPalette.tsx`) is the "search"
  surface: placeholder "Search open tabs, files, URLs, agents…". It reads
  `agentStatusByPaneKey`, `retainedAgentsByPaneKey`,
  `sleepingAgentSessionsByPaneKey`, `paneForegroundAgentByPaneKey`, and
  unread-completion flags to render agent identity/state on tab rows and keep
  running-agent workspaces visible under "Hide sleeping" (lines 779-996).
  Performance note worth copying: the palette takes a **non-reactive
  snapshot** of the two hottest maps when it opens instead of subscribing —
  "subscribing re-rendered the whole palette on every agent transition"
  (lines 779-786).
- **SidebarFilter** (`sidebar/SidebarFilter.tsx`) filters worktrees (sleeping,
  default-branch, automation-generated, repos, hosts) — it does not filter
  agent rows directly, but hiding a worktree hides its agents; live-agent
  worktrees are exempted from "hide sleeping".
- **Activity page** has its own search box:
  `activityThreadMatchesSearchQuery` with a 2 KB query cap
  (ActivityPrototypePage.tsx:1095-1120) plus a group-by selector.

---

## 6. State management pattern

- **One Zustand store** (`useAppStore`) composed of slices
  (`store/slices/agent-status.ts` is the agent slice: `agentStatusByPaneKey`,
  `retainedAgentsByPaneKey`, `setAgentStatus`, `dropAgentStatus`,
  `dropAgentStatusByWorktree` with shutdown reasons and retained completion
  evidence, `dismissRetainedAgent`, `recordAgentProviderSession`, …).
- **Module-level memoized selector caches** keyed by slice identity
  (worktree-agent-row-selectors.ts:33-50) provide per-worktree indexed
  lookups; components subscribe with `useShallow` to arrays/records scoped to
  their own worktree.
- **Derivation in hooks**, not in the store: `useWorktreeAgentRows` builds
  rows in `useMemo`; expansion state and ack maps live in the store so they
  survive list virtualization remounts.
- **`React.memo` boundaries** everywhere (SidebarNav, WorktreeCardAgents,
  DashboardAgentRow) and lazy loading for flagged surfaces
  (`lazyWithRetry(() => import('./AgentDashboardSidebarEntry'))`).
- Snapshot purity discipline: selectors must return stable references for
  unchanged snapshots — e.g. migration entries convert through a `WeakMap`
  cache because "fresh objects with Date.now() … break useSyncExternalStore's
  cached-snapshot contract" (useWorktreeAgentRows.ts:56-58,
  migration-unsupported-agent-entry.ts:6-16).

---

## 7. Implications for a BibCode implementation

BibCode (React/Vite web + Rust/Axum server + Tauri desktop, WebSocket RPC)
maps onto Orca's architecture cleanly — Orca's _paired-client_ path (host
publishes projections over a socket) is actually a closer analog than its
Electron IPC path.

**Data model**

1. Define a four-state wire vocabulary (`working | blocked | waiting | done`)
   with `interrupted` / `sessionBoundary` refinements, and derive `idle`
   client-side from staleness — do not put `idle` on the wire.
2. Key agents by a **stable session/pane key** owned by the server (Orca:
   `tabId:leafId` with UUID leaves; BibCode: session id or session+pane id).
   Every event carries the key plus routing (`worktreeId`, transport id) and
   timing (`receivedAt`, `stateStartedAt`).
3. Carry rich but capped context per entry: prompt, model, agentType (open
   string set), toolName/toolInput previews, lastAssistantMessage, bounded
   `stateHistory`, optional `subagents[]` and `orchestration` parent linkage.
   Enforce max lengths server-side.
4. Keep a `RetainedAgentEntry` analog: when a session/pane dies in `done`,
   snapshot entry+tab+worktree so the UI keeps showing the completion until
   the user visits or dismisses it.

**Event model**

5. Server-push over the existing WebSocket: `agent_status_set` /
   `agent_status_clear` events **plus a snapshot RPC** for
   connect/reconnect hydration (Orca's `agentStatus:getSnapshot`). The
   snapshot half is what makes reconnects deterministic — directly aligned
   with BibCode's reliability-first priorities.
6. Client applicator with: drop-if-older-than-stored `updatedAt`; pending
   queue + replay for events that reference not-yet-known
   sessions/worktrees; per-connection clear watermarks; equality check before
   store writes to avoid no-op re-renders.
7. Status ingestion server-side: BibCode's server already supervises the
   agent processes, so it can synthesize status from provider events
   directly; if external CLIs are supported, copy Orca's managed-hook +
   loopback-endpoint pattern (endpoint file with port/token, per-provider
   installers, shared parsing module) and the OSC/title heuristic as a
   fallback evidence layer for hookless agents.
8. Persist last-known statuses with a TTL and rehydrate them flagged
   `restoredUnconfirmed` (treated as stale until a live event confirms).

**UI structure**

9. Primary surface: per-worktree/workspace cards in the left panel, each with
   an inline agent list; a compact mode with a collapsed state-summary pill
   for dense lists; optionally a flag-guarded cross-workspace "Agents"
   page for triage (Orca's Activity page: group by status/project, search,
   unread badge).
10. Row = provider icon (who) + state glyph (what) + primary text
    (conversation name → prompt → state label) + tool/assistant preview +
    relative time; tool preview only in working/waiting states. Two-level
    lineage tree for orchestrated children with disclosure and `role="tree"`.
11. Interactions: click focuses the owning worktree + terminal/session pane
    and acknowledges the row; hover-X dismisses; retained rows inert;
    unvisited rows bold via an `ackAt < stateStartedAt` comparison. No
    context menu is needed for parity.
12. Sorting: `startedAt → tab order → created → key` inside a card; feed
    agent state into workspace-level "smart" ordering
    (needs-you → done → working → idle).

**Performance mechanics worth copying verbatim**

13. Per-worktree indexed selectors + shallow subscriptions so one agent ping
    re-renders one card, not the whole sidebar (the "O(worktrees²) render
    amplification" comment). One shared 30 s clock tick, mounted only for
    non-empty lists, for time-ago labels and stale decay
    (`STALE_AFTER = 30 min`).
14. Stable-snapshot discipline for derived entries (cache synthesized objects;
    never `Date.now()` inside a selector), and non-reactive snapshots for
    hot maps in transient surfaces like a search palette.

**Key Orca files to mine while implementing**

| Concern                    | Path                                                                                                                                                      |
| -------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Sidebar composition        | `src/renderer/src/components/sidebar/index.tsx`                                                                                                           |
| Nav "Agents" entry + flags | `src/renderer/src/components/sidebar/SidebarNav.tsx`                                                                                                      |
| Inline agent list          | `src/renderer/src/components/sidebar/WorktreeCardAgents.tsx`                                                                                              |
| Row building               | `src/renderer/src/components/sidebar/worktree-agent-rows.ts`, `useWorktreeAgentRows.ts`, `worktree-agent-row-selectors.ts`, `worktree-agent-row-order.ts` |
| Row rendering              | `src/renderer/src/components/dashboard/DashboardAgentRow.tsx`, `sidebar/worktree-card-compact-agent-row.tsx`, `components/AgentStateDot.tsx`              |
| Types                      | `src/shared/agent-status-types.ts`, `src/shared/agent-status-ipc-payload.ts`, `src/shared/terminal-tab-types.ts`                                          |
| Store slice                | `src/renderer/src/store/slices/agent-status.ts`                                                                                                           |
| Event apply pipeline       | `src/renderer/src/hooks/ipc-events/agent-status-event-applicator.ts`, `agent-status-ipc-bridge.ts`                                                        |
| Main-process fanout        | `src/main/index.ts:1687-1775`, `src/main/agent-hooks/server.ts`, `managed-agent-hook-registry.ts`                                                         |
| Paired-client projection   | `src/renderer/src/runtime/web-session-tabs-sync.ts`                                                                                                       |
| Title heuristics           | `src/renderer/src/lib/agent-status.ts`, `sidebar/worktree-title-derived-agent-rows.ts`, `sidebar/smart-attention.ts`                                      |
| Activity page (flagged)    | `src/renderer/src/components/activity/ActivityPrototypePage.tsx`                                                                                          |

---

## 8. Activity page deep dive (ActivityPrototypePage)

The target surface chosen for BibCode. Everything below is from
`src/renderer/src/components/activity/ActivityPrototypePage.tsx` (2070 lines;
"prototype keeps the real-data adapter and visual skeleton together", line 1)
and its helpers, verified against the working tree.

### 8.1 The thread model

**Store inputs.** The page component subscribes with one `useShallow` selector
(ActivityPrototypePage.tsx:1453-1466): `agentStatusByPaneKey`,
`migrationUnsupportedByPtyId`, `retainedAgentsByPaneKey`, `tabsByWorktree`,
`worktreeMap`, `repoMap`, `acknowledgedAgentsByPaneKey`, the two ack actions,
and `generatedTitlesEnabled` (`settings.tabAutoGenerateTitle`). It additionally
subscribes to `agentStatusEpoch` as a pure invalidation dep — "so the memo
recomputes when freshness boundaries expire even without new PTY data" (line
1467-1468) — and reads `Date.now()` _inside_ the memo body, not as a dep
(1480-1481). No polling, no interval.

**Two-stage derivation**, both plain exported functions (unit-tested):

_Stage 1 — `buildActivityEvents` (lines 621-779)_ produces:

- `events: ActivityEvent[]` — an **append-only notification feed** of
  `done | blocked | waiting` occurrences (`ActivityEventState`, line 90).
  For every entry it emits one event per qualifying `stateHistory` snapshot
  plus one for the current state ("Activity is append-only; when a pane
  continues (done→working), stateHistory is the only record of the previous
  done/blocking event", line 597), **skipping session-boundary dones** ("
  SessionStart creates an idle row, not an 'Agent finished' activity event",
  line 610-611). Event identity is
  `` `agent:${paneKey}:${state}:${timestamp}` `` with a dedup set (565-569),
  and each event is stamped `unread: acknowledgedAt < timestamp` (581).
  Sources, in order: live `agentStatusByPaneKey` entries (`agentAlive: true`,
  646-680), synthesized blocked entries for migration-unsupported panes
  (`agentAlive: false`, 682-717), and `retainedAgentsByPaneKey`
  (`agentAlive: false`, 719-743). History snapshots are re-materialized via
  `historyEntrySnapshot` (533-549), which strips per-turn tool/assistant
  fields so old events don't show current-turn context.
- `liveAgentByPaneKey` — a separate map of **fresh live states**
  (`working | blocked | waiting`, with `working`+`workingMode==='monitoring'`
  mapped to `'monitoring'`; `freshActivityLiveAgentState`, 491-504, gated by
  `isExplicitAgentStatusFresh` at the 30-min threshold). Comment at 656:
  "live status is separate from history; a fresh working turn updates the
  thread without counting as an unread done/blocked/waiting event."

  Panes in worktrees the worktree map doesn't know (floating/standalone
  terminals) get a synthetic worktree with repoId
  `'__activity_standalone__'` and displayName "Floating terminal"/"Standalone
  terminal" (`standaloneActivityWorktree`, 506-528).

  **Caps**: events are sorted newest-first, then capped to **80 events
  globally** with **5 per pane** (`EVENTS_PER_PANE_CAP`, line 531), and each
  pane's newest event is reserved _before_ the global cap fills "so … a pane
  [can't be pushed] out of the window and hid[den]" (749-761).

_Stage 2 — `buildAgentPaneThreads` (lines 781-870)_ folds events + live map
into `AgentPaneThread[]`. **A thread's identity is the pane key** — "keyed per
agent pane (tab + leaf id), not per workspace, so the list shows one row per
agent; paneKey is `${tabId}:${leafId}`" (line 125). Thread shape (126-141):
`paneKey`, `paneTitle`, `worktree`, `repo`, `tab`, `agentType`,
`currentAgentState` / `currentAgentEntry` (null unless fresh-live),
`responsePreview`, `latestTimestamp`, `latestEvent`, `events[]` (desc),
`migrationUnsupportedPtyId?`, `unread` (true if **any** member event is
unread, 811). The live snapshot, when present, **overwrites** the thread's
title/worktree/tab/agentType/preview/timestamp — "row title/time/target must
follow the active turn (not historical events) so a running agent never shows
the previous prompt as primary" (848). A live-only thread (agent working,
no done/blocked history yet) has `events: []`, `latestEvent: null`,
`unread: false` (830-845). Threads sort by `latestTimestamp` desc (869).

So one thread aggregates: the pane's live status entry **plus** its retained
completion snapshot **plus** up to 5 historical notification events — all
joined on `paneKey`. Provider sessions are not a join key here.

### 8.2 Status grouping — `buildActivityThreadGroups`

`groupBy` is page-local state, **default `'status'`** (line 1426), with values
`'status' | 'project' | 'worktree' | 'agent'` (line 89).

Per-thread status is `threadAgentState(thread) = currentAgentState ??
latestEvent?.state ?? 'done'` (978-980) — i.e. a fresh live state wins,
otherwise the newest event's state, otherwise done. `getActivityThreadGroup`
(990-1016) maps a thread to `{key, label}`:

- `status`: key is the state itself, except an **interrupted done** (no live
  state, `latestEvent.entry.interrupted`) gets the distinct key
  `'done:interrupted'` and label "Interrupted" (996-998).
- `project`: `project:${repo.id}` / repo display name, with a
  `project:unknown` / "Unknown project" bucket.
- `worktree`: `worktree:${worktree.id}` / worktree display name.
- `agent`: `agent:${agentType}` / formatted agent label.

`buildActivityThreadGroups` (1018-1035) — **the function the page actually
renders** (1521-1524) — builds groups in **thread encounter order**: since the
thread list is sorted newest-first, groups appear ordered by their newest
member, and each group's `threads` keep that recency order. It sets only
`{key, label, threads}` — no `id`/`state`.

A second exported variant, `groupActivityThreadsByStatus` (1058-1079), imposes
the **fixed order** `ACTIVITY_STATUS_GROUP_ORDER = ['working', 'monitoring',
'blocked', 'waiting', 'done', 'interrupted']` (166-173), elides empty groups
(`flatMap` returning `[]`, 1064-1068), and attaches `id` + a `state` dot for
the header. As of this reading it is exercised only by tests — the rendered
page path is the encounter-order variant. (For BibCode's fixed
WORKING→BLOCKED/WAITING→DONE order, `groupActivityThreadsByStatus` is the
right template.)

**Counts**: the sticky group header (`ActivityStatusGroupHeader`, 1200-1216)
renders an optional state dot (only when `group.state` is set), the uppercase
label, and a pill with `group.threads.length`. **Empty groups never render** —
groups are built only from existing threads. An empty _list_ renders "No agent
activity matches these filters." (1908-1915).

### 8.3 Row anatomy (`ThreadRow`, lines 1235-1422)

Layout per row (top to bottom, left to right):

- **Unread bar**: absolute 2px primary-colored left bar when `thread.unread`
  (1288-1290). Design note at 1280: "selected = tint+shadow, beats hover;
  unread = weight + left bar only; stacking all three confused selected vs
  unread on hover."
- **Leading glyphs**: `ThreadAgentStateIndicator` (AgentStateDot `md` with a
  tooltip naming the state, 1183-1198) + provider `AgentIcon` (size 14).
- **Project label**: repo badge mark + uppercase repo display name, fallback
  "Unknown project" (`ActivityProjectLabel`, 947-962).
- **Primary line — workspace title**: `getActivityThreadWorkspaceTitle`
  (`lib/activity-thread-display.ts:85-94`): `worktree.displayName` →
  `branch` → `'Workspace'`. Bold (`font-semibold`) when unread, otherwise
  `font-medium` (1306). `line-clamp-2 break-words`, or single-line `truncate`
  in compact mode (1305).
- **Secondary line — task title** (`thread.paneTitle`), rendered only when it
  differs from the workspace title (1312). Built by
  `getActivityThreadTaskTitle` (`activity-thread-display.ts:97-138`) with this
  fallback chain: tab `customTitle` → orchestration
  `displayName`/`taskTitle` (only while it still describes live work,
  67-82) → `generatedTitle` (only if the setting allows) → the **live prompt
  if substantive** → the newest substantive prompt from `stateHistory` →
  live tab title (if it differs from the default) → `defaultTitle` →
  `'Terminal'`. "Substantive" excludes terse follow-ups —
  `isTerseAgentFollowUpPrompt` rejects ≤24-char yes/ok/proceed/lgtm-style
  replies (17-29) so "the live turn prompt ('yes', 'ok proceed') must not
  replace the task title" (ActivityPrototypePage.tsx:460).
- **Status preview line** (non-compact only, and only when non-empty and not
  duplicating the two titles, 1258-1262): `thread.responsePreview` rendered as
  **inline markdown** (`CommentMarkdown` flattened to one `truncate` line via
  `[&_*]:inline` overrides, 1323-1332), truncated for render to **320 chars**
  with surrogate-pair-safe slicing + `...`
  (`activityThreadResponseRenderPreview` /
  `ACTIVITY_THREAD_RESPONSE_RENDER_PREVIEW_MAX_LENGTH = 320`, 165, 229-254).
  The preview's fallback chain (`getActivityThreadStatusPreview`,
  `activity-thread-display.ts:157-177`): `'Interrupted by user'` if
  interrupted → `` `toolName: toolInput` `` (only in working/waiting states —
  `showsAgentToolPreview`, `lib/agent-row-tool-preview.ts:14-16`) →
  `lastAssistantMessage` unless it is a mislabeled echo of the user's own
  prompt (140-154) → `''`. `resolveActivityThreadStatusPreview` (180-203)
  additionally **keeps the previous preview** across a transient empty hook
  ping, but only within the same turn (a substantive new prompt clears it).
- **Footer line**: agent-type label (10px muted), plus a hover-revealed
  **"Jump to workspace"** external-link button when the worktree exists
  (1335-1370).
- **Trailing column**: unread indicator — a filled amber bell when unread
  (1375-1382), or a hover-revealed plain bell button "Mark thread unread"
  when read (1384-1413) — and `EventTime`: relative time
  (`formatUiRelativeTime`) with the absolute date in a tooltip (872-891,
  1415).

Compact mode (kebab menu toggle) collapses both titles to single-line
truncation and drops the status preview line entirely (1258-1259, 1305).

### 8.4 Filter input, group-by select, bell toggle, kebab menu

Header toolbar (1790-1882), left to right:

- **Filter input** — placeholder "Filter...", search icon, page-local `query`
  state. Matching is `activityThreadMatchesSearchQuery` (1104-1119):
  lowercase substring over a concatenation (`threadSearchText`, 1081-1093) of
  pane title, workspace title, branch, repo name, agent-type label, state
  label, current prompt (both display-processed and raw), current assistant
  summary, response preview, and the latest event's title/summary/meta
  strings. Normalization: `trim().toLowerCase()`; an empty query matches all.
  The cap is `ACTIVITY_SEARCH_QUERY_MAX_BYTES = 2 * 1024` **bytes** (1095),
  checked with a UTF-8 byte-length helper; an over-cap query matches
  **nothing** (1111-1112, and 1505 short-circuits to hiding all threads).
  **Cmd/Ctrl+F focuses it** via a capture-phase window listener
  (`handleActivityFilterFocusShortcut`, 1121-1181, 1673-1685) — unless focus
  is inside the portaled terminal, which keeps Cmd+F for terminal search
  (1131-1140).
- **Group-by `Select`** — this is **group-by, not filter-by**: values Status /
  Project / Worktree / Agent (1805-1845), aria-label "Group agent activity
  by".
- **BellDot `Toggle`** — a **filter**: "Show unread threads only"
  (`readFilter: 'all' | 'unread'`, 1425, 1846-1873). Filtering keeps the
  currently selected thread visible even after auto-mark-read flips it to
  read, "else unread-only mode makes the clicked row vanish" (1507-1513).
- **Kebab (`MoreVertical`) menu** — `ActivityThreadOptionsMenu` (893-945):
  a "Compact mode" checkbox and "Mark all read" (disabled when nothing is
  unread; kept behind the overflow because it is "low-frequency and
  destructive-feeling", 1874).

### 8.5 Unread model

Ack state lives in the **UI store slice**:
`acknowledgedAgentsByPaneKey: Record<string, number>` — a per-pane "read up
to" timestamp (`store/slices/ui.ts:1227-1286`).

- `acknowledgeAgents(paneKeys)` stamps `max(Date.now(),
  latestAgentTurnTimestamp(entry))` — the clock-skew guard: "a remote/SSH
  execution host can stamp a turn ahead of this clock, and every unread rule
  is `ackAt < turnTimestamp`" (ui.ts:1241-1244). It only writes when the ack
  actually advances (`prev < stamp`, not `!==`, ui.ts:1236) and also
  dismisses the matching desktop notifications (ui.ts:1282-1285).
- `unacknowledgeAgents(paneKeys)` simply deletes the keys (ui.ts:1286-1300) —
  that's the "Mark thread unread" bell.

**What counts as unread**: an event is unread when
`ackAt < event.timestamp` (ActivityPrototypePage.tsx:581); a thread is unread
when any of its events is (811). Only `done | blocked | waiting` produce
events, so a working agent is never "unread". The badge counter
(`useActivityUnreadCount.ts`) counts in two modes (line 18):
`'agent-events'` counts **every** unacknowledged done/blocked/waiting event
including history entries — "the titlebar badge must mirror that event count"
(55-61) — while `'sidebar-badge'` (used by the SidebarNav "Agents" button)
counts current states only, plus unread worktrees, with a special case
recovering a completion displaced by a session-boundary done (63-79).
Session-boundary dones are never unread (68-73). Recompute is driven by
`sortEpoch` as a cheap invalidation signal instead of subscribing to the hot
`agentStatusByPaneKey` map (101-124).

**When things become read**:

1. **Auto-mark-read on selection** — an effect (1727-1760) acknowledges the
   selected thread once its detail view is actually showing: either the
   portaled terminal is ready (`visiblePortalReady` and it is the visible
   thread) or the thread has a detail-only view (no live tab /
   migration-unsupported). It is bounded to **once per turn** via
   `` `${paneKey}:${latestTimestamp}` `` in a ref — the long comment at
   1736-1739 documents the React #185 infinite-loop this prevents when a
   remote host stamps turns ahead of the local clock.
2. **Jump to workspace** marks the thread read before navigating (1767).
3. **Mark all read** acknowledges every unread thread's paneKey (1773-1779).
4. Clicking an agent row in the _sidebar_ acks via
   `activateTabAndFocusPane({ ackPaneKeyOnSuccess })` (§3) — same map.

**"0 unread" header badge**: when the Activity page is open, the titlebar
main strip is replaced by `ActivityTitlebarControls`
(`app-shell/TitlebarMainStrip.tsx:49-51`): a Back arrow ("Close agents" →
`closeActivityPage`), a Bell icon, the lowercase title "agents", and a
`Badge` reading `{unreadCount} unread` from
`useActivityUnreadCount(true, 'agent-events')`
(`activity/ActivityTitlebarControls.tsx:10-56`).

### 8.6 Selection → center panel (terminal portal)

Selection is page-local: `selectedPaneKey` (1432). `selectThread` (1722-1725)
does two things: `setSelectedPaneKey(paneKey)` and
`activateThreadTerminal(thread)` (1695-1720), which — only if the thread's tab
is still live — switches the app's active repo/worktree/tab-type and calls
`activateTabAndFocusPane(tabId, leafId, { scrollToBottomIfOutputSinceLastView:
true })`. So clicking a row _also_ re-points the app's global active
workspace; the "Jump to workspace" button additionally leaves the Activity
page via `activateAndRevealWorktree` (1762-1769).

The center pane does **not** mount a second terminal. The workspace
`<Terminal />` workbench stays mounted (hidden) while Activity is open
(`AppWorkspaceShell.tsx:182-199`, `shouldMountTerminalWorkbench` sticky at
`use-app-chrome-layout.ts:65`, visibility = `workspaceChromeActive` at 143),
and the Activity page **portals the existing TerminalPane DOM** into its
detail slot:

- A module-level external store (`activity-terminal-portal.ts`) holds portal
  descriptors `{slotId, requestToken, target, worktreeId, tabId, paneKey,
  forceUnavailable, active}`; the page publishes them with
  `setActivityTerminalPortals` in a `useLayoutEffect` "before paint so
  Terminal's portal subscriber rerenders in the same commit" (1659-1664).
  Descriptors carry their own worktree/tab routing because deriving from
  global active state "introduced a race … briefly portaling a different
  terminal into the activity slot ('flash' of the wrong terminal)"
  (activity-terminal-portal.ts, comment above `setActivityTerminalPortals`).
  `Terminal.tsx` consumes them via `useActivityTerminalPortals` +
  `createPortal` (Terminal.tsx:417, 2504).
- **Double-buffered swap**: two portal slots (`'primary'`/`'secondary'`,
  1434-1437, 1991-2012, stacked absolutely with opacity/z-index switching).
  The newly selected thread is _staged_ in the inactive slot; a
  `MutationObserver`-driven readiness probe
  (`useActivityTerminalPortalStatus`, 294-406) checks the portaled DOM for
  the right `data-terminal-tab-id`/`data-leaf-id`, a PTY binding and an
  `.xterm-screen`, and only then the page swaps slots
  (`resolveActivityPortalSwap`, 1625-1657) — so switching threads never
  flashes the previous terminal. `displayedPaneKey` tracks what is actually
  on screen vs `selectedPaneKey` (what was clicked).
- While loading: a delayed (180 ms, line 164) "Connecting terminal..." chip;
  when the pane can't render: "Terminal unavailable" (2013-2040).
- Stale selection is cleared when the thread disappears from the list
  (1496-1502).

**Keyboard**: rows are `role="button"` with `tabIndex={0}`; Enter/Space
selects, with a guard so activating a link inside the markdown preview doesn't
also select the row (1218-1233, 1269-1278). Cmd/Ctrl+F focuses the filter.
There is **no arrow-key list navigation** on this page.

### 8.7 Layout and how the page replaces the normal shell

`activeView === 'activity'` mounts `<ActivityPrototypePage />` as the active
page (`app-shell/AppWorkspaceShell.tsx:76`) and **hides the worktree sidebar
entirely** — "Activity/Space are full-page navigation surfaces (like
Settings), so the worktree sidebar is hidden there":
`showSidebar = activeView !== 'settings' && activeView !== 'activity' &&
activeView !== 'space'` (`app-shell/use-app-chrome-layout.ts:75-77`). The
titlebar swaps to the Back/bell/unread strip (§8.5). The page itself
(1782-2068) is:

- `<aside>` **thread list**, fixed default width **480 px** ("thread cards
  are the primary surface; 480px lets prompts fill line-clamp-3", 1438),
  **resizable 320-720 px** via the same `useSidebarResize` hook the sidebar
  uses (1439-1451), with a drag handle styled like the sidebar's
  (1917-1939). Inside: the toolbar (§8.4), then a scrollable region of
  `<section aria-label="{group} activity">` blocks, each a sticky
  backdrop-blurred group header plus its `ThreadRow`s (1883-1916).
- `<section>` **detail pane** takes the remaining width: a header with state
  dot + agent icon + `paneTitle` (line-clamp-3) + repo badge + worktree name
  (1946-1969), then the portaled terminal (§8.6). Empty states: "No activity
  yet." when there are no threads, "Select an agent to view its activity"
  when nothing is selected (2045-2064).

### 8.8 Retained/done rows on this page

- Retained entries feed the event feed like live ones (719-743) but with
  `agentAlive: false`; their threads usually land in the done (or
  "Interrupted") group.
- **Click**: `activateThreadTerminal` bails early when the thread's tab is no
  longer live — "retained-agent threads can outlive their tab; without a live
  tab, reorienting the workspace and focusing a dead tab id would just
  confuse the user" (1701-1706). Selection still happens, and the detail pane
  shows a placeholder instead of a terminal: "Agent terminal closed. Open a
  new terminal in this workspace to continue." (or "Standalone terminal
  unavailable in Activity." for synthetic worktrees) (1971-1988). Because
  that placeholder is a "detail-only view", selecting such a thread
  auto-marks it read immediately (1744-1750).
- **Dismissal**: the Activity page has **no per-thread dismiss** (unlike the
  sidebar rows' hover-X). Rows leave the list when their retained entry is
  dropped elsewhere (sidebar dismiss, worktree visit) or ages out; the page's
  own affordances are read/unread only. A vanished selection is cleaned up at
  1496-1502.

### 8.9 Mapping onto the BibCode design

Agreed BibCode shape: an **always-visible cross-environment Agents section in
the existing left panel** (not a full-window page), rows = **threads with live
sessions including done**, **server-pushed capped previews on the shell
stream**, **fixed status groups WORKING → BLOCKED/WAITING → DONE with DONE
collapsed**, inline filter, bold-unvisited unread, click opens the thread in
the center and switches the rail environment.

Direct carry-overs from the Activity page:

1. **Thread = stable session identity** joining live status + finished
   snapshot + bounded history. BibCode's join key is its thread/session id
   (server-owned) instead of `tabId:leafId`; keep Orca's rule that the live
   turn overwrites row title/target while a _stable task title_ survives
   terse follow-ups (`getActivityThreadTaskTitle`'s chain: user title →
   orchestration label → generated title → substantive prompt → history →
   default). The terse-follow-up filter (≤24-char yes/ok/proceed) is cheap
   and high-value.
2. **Fixed status groups**: use the `groupActivityThreadsByStatus` template —
   fixed order array, elide empty groups, group header = label + count pill,
   sticky headers. That variant also supplies the header's state dot (it sets
   `group.state`; Orca's rendered encounter-ordered path omits it, so the
   dot never actually shows there today). BibCode collapses DONE by default
   (a deliberate extension; Orca renders all groups expanded and, in its
   rendered path, orders groups by recency rather than the fixed array).
3. **Preview pipeline server-side**: Orca computes `responsePreview`
   client-side from capped hook fields; BibCode should compute/cap the
   preview on the server and push it on the shell/session stream. Keep the
   semantics: interrupted label → `tool: input` only in working/waiting →
   last assistant message with the "mislabeled user prompt" echo guard →
   retain previous preview across a transient empty ping within the same
   turn. Cap render length ~320 chars, surrogate-safe truncation.
4. **Unread**: per-thread `ackAt` timestamp; unread ⇔
   `ackAt < event.timestamp` over done/blocked/waiting transitions only
   (working never unread; session-boundary dones excluded). Stamp acks as
   `max(now, latestTurnTimestamp)` to survive server/client clock skew —
   Orca's React #185 comment (ui.ts:1241-1244, page 1736-1739) is a real bug
   class BibCode's remote-server architecture will hit. Auto-mark-read once
   per `(threadId, latestTimestamp)` when the detail view actually renders;
   manual "mark unread" deletes the ack; "mark all read" batches. Unread UI =
   bold title + left bar (+ optional bell), and a badge counting
   unacknowledged events.
5. **Selection**: clicking a row selects it _and_ re-points the active
   environment/session (Orca: repo + worktree + tab + pane focus; BibCode:
   switch rail env + open thread in center). Clear selection when the thread
   disappears. Keep row-click guards for nested interactive elements.
6. **Filter**: substring match over a precomputed lowercase haystack (titles,
   branch, project, agent label, state label, prompt, preview), trim +
   lowercase, byte-length cap (~2 KB) that fails closed, Cmd/Ctrl+F focus
   shortcut that yields to a focused terminal.
7. **Recompute policy**: derive threads in a memo keyed by the status-store
   snapshot plus an epoch counter for freshness expiry; read `Date.now()`
   inside the memo. Cap the event window (Orca: 80 global / 5 per pane, with
   newest-per-pane reserved) so one chatty agent can't evict others.

Deliberate divergences (and what they release BibCode from):

- **Section in the left panel, not a full-window page** — BibCode does not
  hide its nav/sidebar and does not need Orca's replaced-titlebar
  (Back/bell/"N unread") chrome; the unread badge belongs on the section
  header instead. The list width is the panel width; Orca's 320-720 px
  resizable list + resize handle doesn't apply.
- **Thread-shell entity vs pane threads** — Orca's threads are derived
  client-side by folding a per-pane status map; BibCode owns a first-class
  thread/session entity on the server, so the whole Stage-1/Stage-2 fold
  (events from `stateHistory`, retained-entry merging, migration-unsupported
  synthesis, standalone-worktree buckets) collapses into a server-pushed
  thread list + per-thread update events. Keep only the client-side ordering
  (latest-activity desc) and grouping.
- **No DOM portal gymnastics** — Orca portals a single mounted xterm across
  surfaces to avoid double PTY ownership, with double-buffered slots,
  MutationObserver readiness probing and swap reconciliation (§8.6). BibCode
  opens the thread in the center pane through its normal session-view
  routing, so none of that machinery is needed — but the _reason_ it exists
  (never render two live owners of one terminal; never flash the wrong
  session while switching) is a requirement BibCode must satisfy through its
  own view lifecycle.
- **DONE collapsed by default** — an extension; Orca has no per-group
  collapse on this page.
