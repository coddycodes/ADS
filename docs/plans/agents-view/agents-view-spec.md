# Agents View — Design Specification

Status: approved and implemented (v1 approved 2026-08-31 and shipped on
`mubeda/develop-3`; the §7 v2 revision was confirmed by the user on
2026-09-01). This document is the AGENTS.md-required design record for the
feature: alternatives and trade-offs were settled in user interviews (decision
logs below).

Research grounding (same directory):

- `research-orca-agents-view.md` — the reference product's agent surfaces,
  data model, event pipeline, and the Activity page deep dive (§8), which is
  the surface the user chose to replicate.
- `research-bibcode-left-panel.md` — BiBCode's sidebar structure, command
  palette, multi-environment connection runtime, thread-shell aggregate, and
  the constraints inherited from the Remote Servers spec.

## 1. Summary

A new **Agents** section in the web app's left panel, rendered between the
Search row and the Projects header, listing one row per **non-archived thread
with a live session** across **all connected environments**. Rows carry
Orca-parity context — status pill, latest prompt, current tool line, last
assistant message snippet — grouped under fixed status headers with counts,
filterable by a new inline input, bolded while unvisited, and clickable to
open the thread in the center panel while moving the environment rail
selection to the row's environment.

The preview context does not exist on the wire today; the server gains three
capped, additive preview fields on the thread-shell contract, populated from
the projection data it already loads. No new subscriptions, no per-thread
fan-out, no CLI hooks: previews ride the existing per-environment
`orchestration.subscribeShell` stream, and the cross-environment aggregation
reuses the existing client-side `threadShellsAtom`.

## 2. Decision log

| #   | Decision                                                                                                                                                                                                          | Why / alternative rejected                                                                                                                                                                            |
| --- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| D1  | Always-visible section in the left panel between the Search row and the Projects header.                                                                                                                          | User decision. A toggleable panel mode (Orca's full-window Activity page) was offered and declined.                                                                                                   |
| D2  | Cross-environment: the section ignores rail selection and renders rows from every catalog environment.                                                                                                            | The point of the feature. This is a deliberate, recorded exception to the Remote Servers rail-scoping invariant (§3.6).                                                                               |
| D3  | Row set: every thread with `archivedAt === null` and `session !== null`, including finished/idle sessions.                                                                                                        | User decision ("all threads with a live session"); matches the reference Activity page's DONE group. Attention-only was offered and declined.                                                         |
| D4  | Row content: Orca-parity previews — latest prompt, current tool line, last assistant message — plus status pill, project/branch, environment badge, relative time.                                                | User decision ("Orca way").                                                                                                                                                                           |
| D5  | Preview transport: the server pushes capped preview fields on the existing shell stream (the reference's paired-client projection pattern). No per-thread activity subscriptions, no managed CLI hooks.           | One stream, bounded by design; reliability-first. The server already supervises the provider processes, so hook installation (the reference's Electron-local mechanism) is unnecessary.               |
| D6  | Grouping v1: fixed status groups in pinned order WORKING → PENDING APPROVAL → AWAITING INPUT → DONE, counts in headers, empty groups elided. No group-by dropdown.                                                | User decision; dropdown (status/project/environment) is an explicit fast-follow, out of scope.                                                                                                        |
| D7  | Rows from a non-live environment render greyed with that environment's availability badge; cached data is never presented as live.                                                                                | User decision (option "shown, visibly stale"); shell-projection-authority rule in `docs/architecture/connection-runtime.md`. Explicitly-disconnected environments are also shown greyed (not hidden). |
| D8  | Clicking a row opens the thread in the center panel **and** moves `activeEnvironmentIdAtom` to the row's environment.                                                                                             | User decision; otherwise the rail/Projects section and the center panel disagree about the active environment. Verified: route navigation does not currently sync rail selection.                     |
| D9  | Inline filter input at the top of the section, matching over a precomputed lowercase haystack across all environments, reusing `normalizeSearchText` from `CommandPalette.logic.ts`. 2 KiB byte cap fails closed. | User decision; first inline input in the sidebar, deliberate. One matching policy, not two.                                                                                                           |
| D10 | Volume bounds: groups collapsible, DONE collapsed by default, per-group cap of 5 rows with "Show more", whole section collapsible via its header (persisted).                                                     | User accepted recommendation; keeps the no-virtualization sidebar viable.                                                                                                                             |
| D11 | Unread: rows render bold until visited, reusing `sidebarWorkspaceMetaStore` (`unreadThreadKeys`, keyed by `scopedThreadKey`). No unread-count badge in v1.                                                        | User decision. New trigger required: nothing marks threads unread today (verified — `markUnread` has no production callers).                                                                          |
| D12 | Aggregation stays client-side in existing atoms.                                                                                                                                                                  | Settled architecture: one server = one environment (`remote-servers-spec.md`); `threadShellsAtom` already concatenates all environments and is already mounted by the sidebar.                        |
| D13 | Status policy has one source: the row's group derives from `resolveThreadStatusPill` (`Sidebar.logic.ts:445`), not a second status policy.                                                                        | Duplicate policy is a maintenance defect; the sidebar's pill and the Agents group can never disagree.                                                                                                 |
| D14 | No stable-task-title machinery (the reference's terse-follow-up filter).                                                                                                                                          | BiBCode threads have server-owned titles; the reference needed title synthesis because its rows are terminal panes. YAGNI.                                                                            |
| D15 | Copy and identifiers: section label "Agents", components `AgentsSection`/`agentsSection.logic`. The existing `settings.agents.tsx` (default-agent settings) and activity-protocol "actors" are unrelated.         | Naming-collision note from research; identifiers chosen to not collide.                                                                                                                               |

## 3. Pinned contracts

### 3.1 Wire: `conversationPreview` on the thread shell

`packages/contracts/src/orchestration.ts` gains:

```ts
export const OrchestrationConversationPreview = Schema.Struct({
  /** Newest user message text, truncated to ≤ 200 chars. */
  prompt: Schema.NullOr(TrimmedNonEmptyString),
  /** Newest tool activity summary of the running latest turn, ≤ 160 chars. */
  tool: Schema.NullOr(TrimmedNonEmptyString),
  /** Newest assistant message text, truncated to ≤ 320 chars. */
  assistantMessage: Schema.NullOr(TrimmedNonEmptyString),
});
export type OrchestrationConversationPreview = typeof OrchestrationConversationPreview.Type;
```

`OrchestrationThreadShell` gains one **additive** field:

```ts
conversationPreview: Schema.optional(Schema.NullOr(OrchestrationConversationPreview)),
```

Compatibility: older servers omit the field; clients decode `undefined` and
render rows without preview lines. No other wire shape changes. The schema
fingerprint in the RPC Rust-parity manifest is regenerated in the same change.

### 3.2 Server population semantics

Populated in `thread_shell(...)` (`apps/server/src/production/orchestration_rpc.rs:1011`)
from the already-loaded `Snapshot`:

- `prompt` — text of the thread's newest `ProjectionThreadMessage` with
  `role == "user"`.
- `assistantMessage` — text of the thread's newest message with
  `role == "assistant"` (streaming or complete).
- `tool` — `summary` of the thread's newest `ProjectionThreadActivity` with
  `tone == "tool"` **and** `turn_id` equal to the thread's latest turn id,
  and only while that latest turn's `state == "running"`; otherwise `null`.
  (Reference rule: a leftover tool line must never read as still-running.)
- The field is **omitted** when the thread has no preview content (the same wire
  shape an older server produces); it is never an all-null struct.
- Text is trimmed server-side before the empty check and truncation: the client
  decodes members as `TrimmedNonEmptyString`, which rejects whitespace-only
  strings after its decode-time trim, so an untrimmed emission is a decode
  failure for the whole shell snapshot.

**Truncation** is `char`-boundary safe: take the first N `char`s and append
`…` only when truncated. Caps: prompt 200, tool 160, assistantMessage 320.
"Newest" = maximum `created_at`, tie-broken by later vector position (the
projection loads rows in insertion order).

**Performance**: `shell_snapshot` (same file, :838) builds one
`HashMap<&str, PreviewSource>` in a single pass over `snapshot.messages` and
`snapshot.activities` before mapping threads, so shell building stays
O(messages + activities + threads), not O(threads × messages). The shell
stream already re-sends a full snapshot on every engine event (verified,
`shell_stream` :753-783), so previews need **no new emission triggers** and
add only their capped bytes per send.

### 3.3 Row set and status groups (client policy)

New module `apps/web/src/components/sidebar/agentsSection.logic.ts` (pure,
exported, colocated tests) is the single home of Agents-view policy:

- **Row inclusion**: `shell.archivedAt === null && shell.session !== null`.
- **Group** (type `AgentGroupId = "working" | "blocked" | "waiting" | "done"`)
  derives from `resolveThreadStatusPill({ thread: shell })`:

  | Pill label               | Group     |
  | ------------------------ | --------- |
  | `Working` / `Connecting` | `working` |
  | `Pending Approval`       | `blocked` |
  | `Awaiting Input`         | `waiting` |
  | `Plan Ready`             | `waiting` |
  | `Completed` / `null`     | `done`    |

- **Group order and labels** (empty groups elided):
  `working` "Working" → `blocked` "Pending Approval" → `waiting`
  "Awaiting Input" → `done` "Done". A `Plan Ready` row sits in the
  Awaiting Input group but keeps its own violet pill.
- **Sort within a group**: `updatedAt` descending, tie-broken by
  `scopedThreadKey` ordinal so re-renders never reshuffle equal rows.
- **Environment liveness**: a row is stale unless its environment's
  `EnvironmentShellAvailability.status === "live"`
  (`useEnvironmentShellSummary().statuses`).
- **Filter**: `normalizeSearchText` (imported from `CommandPalette.logic.ts`)
  substring match against a precomputed haystack of thread title, project
  title, branch, environment label, provider name, pill label, and the three
  preview strings. Query longer than 2,048 UTF-8 bytes matches nothing.

### 3.4 UI contract

- **Placement**: inside `SidebarProjectsContent`
  (`apps/web/src/components/Sidebar.tsx`), directly after the search-row
  `SidebarGroup` (:3842-3864) and before the ARM64 warning block. Hidden on
  `/settings` routes along with the rest of the sidebar body (existing
  behavior, accepted).
- **Section header**: uppercase "Agents" label styled like the Projects
  header, a total-count pill, and a collapse chevron. Section expansion
  persists in `uiStateStore` (`agentsSectionExpanded: boolean`, default
  `true`).
- **Filter input**: first row inside the expanded section. Ephemeral
  `useState` + `useDeferredValue`; no persistence.
- **Groups**: sticky-free simple headers (label + count pill), each
  collapsible; expansion persists in `uiStateStore`
  (`agentsGroupExpandedById: Record<string, boolean>`), defaults: `done` →
  `false`, others → `true`. Per-group preview cap `AGENTS_GROUP_PREVIEW_COUNT
  = 5` with a "Show more"/"Show less" row (ephemeral state, Projects-overflow
  pattern).
- **Row anatomy** (top line → bottom): status dot + pill (reuse
  `ThreadStatusPill` colors/pulse), thread title (bold while unread),
  project title · branch, preview line (tool line while the pill is
  `Working`, otherwise `assistantMessage`, otherwise `prompt`, otherwise
  omitted, single line, truncated), environment badge (label; greyed row +
  availability text when stale), relative time from `updatedAt`.
- **Click**: `markRead(scopedThreadKey)` → `setActiveEnvironmentId(ref.environmentId)`
  → `navigateToThread(ref)` (the existing `/$environmentId/$threadId` route).
- **Unread trigger** (new, lives with the section): when a row's
  `latestTurn` transitions into `completed | interrupted | error` (rising
  edge on `(turnId, state)`) and the thread is not the currently open route
  thread, call `markUnread(scopedThreadKey)`. Bold styling reads
  `selectIsUnread`. No unread-count badge in v1.
- **Accessibility**: the group list renders with `role="list"`; rows are
  buttons with `aria-label` naming title, status, and environment.

### 3.5 Persistence keys

| Store                       | Key                        | Shape                              |
| --------------------------- | -------------------------- | ---------------------------------- |
| `uiStateStore`              | `agentsSectionExpanded`    | `boolean`, default `true`          |
| `uiStateStore`              | `agentsGroupExpandedById`  | `Record<string, boolean>`          |
| `sidebarWorkspaceMetaStore` | `unreadThreadKeys` (reuse) | existing `string[]` of scoped keys |

### 3.6 Rail-scoping exception (Remote Servers spec amendment)

`docs/plans/remote-servers/remote-servers-spec.md` §4.8 pins "no selection
must never render as 'show everything'". This feature adds, in the same
patch, an explicit exception clause: _the Agents section is the single
cross-environment surface in the panel; it ignores rail selection by design,
and its row click re-points rail selection to the row's environment so every
other surface remains scoped._ The living doc
`docs/architecture/connection-runtime.md` presentation-scoping paragraph gets
the same one-sentence exception.

## 4. What this feature is NOT

- Not a subscription to activity-protocol actors (sub-agents/background
  tasks); shell-level signal only. Actor depth is a separately designed
  extension.
- Not a group-by dropdown, unread-count badge, per-row dismissal, retained
  snapshots beyond the live shell list, or keyboard list navigation — all
  explicit fast-follow candidates, none in v1.
- Not a server-side cross-environment aggregate (one server = one
  environment stands).
- Not a change to the command palette, Projects section, rail behavior for
  any other surface, or thread-detail streams.
- Not list virtualization; volume is bounded by D10.
- No production Node runtime, no new sidecars, no DesktopBridge changes.

## 5. Failure and lifecycle policies

- **Environment unreachable / reconnecting / disconnected**: rows render from
  the cached snapshot, greyed, with the availability status; click still
  navigates (thread view has its own unavailable handling).
- **Older server (no `conversationPreview`)**: rows render without a preview
  line; everything else works.
- **No live session on any thread**: the section renders its header and an
  empty state ("No agents yet"), never disappears (discoverability).
- **Thread removed while listed**: rows derive from `threadShellsAtom`; a
  removed thread drops out on the next snapshot. Stale unread keys for
  removed threads are inert (existing store behavior, unchanged).
- **Route thread finishing its turn**: no unread mark (the user is looking
  at it) — the trigger checks the open route.

## 6. Validation obligations

- Focused tests: contracts decode/parity, Rust preview-population unit tests,
  `agentsSection.logic.ts` policy tests, section component tests
  (grouping/filter/unread/click), unread-trigger tests.
- `vp check`, `vp run typecheck`; Rust: `cargo fmt --all --check`, affected
  tests, Clippy with warnings denied.
- apps/web changes reviewed against the `vercel-react-best-practices` skill.
- Same-patch docs: `docs/architecture/rpc-and-orchestration.md` (shell
  contract field), `docs/plans/remote-servers/remote-servers-spec.md` §4.8
  exception, `docs/architecture/connection-runtime.md` scoping paragraph.
- `docs/testing/` runbooks: review the packaged-UI-flow runbooks; if native
  visual validation flows enumerate sidebar sections, add the Agents section,
  else state "reviewed and remain accurate".

## 7. V2 revision — full Agents view (2026-09-01, approved)

After using v1, the user requested the reference product's actual
click-through experience (screenshots on file): a slim entry that opens a
**full-takeover Agents view** with a back arrow, list column, and an embedded
live detail pane. Confirmed in a follow-up interview on 2026-09-01. This
section supersedes parts of D1, D6, and D11; everything else above stands.

### 7.1 V2 decision log

| #   | Decision                                                                                                                                                                                                                                                                             | Supersedes     |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------------- |
| D16 | The in-panel Agents section (rows/groups/filter) is removed. A slim **Agents nav row** sits below the Search row: icon, label, unread-count badge. Clicking navigates to `/agents`.                                                                                                  | D1 (surface)   |
| D17 | `/agents` is a full-takeover route: `AppSidebarLayout` renders no sidebar/rail/sidebar-control on it (the reference hides its worktree sidebar the same way). The page owns a top strip — back arrow, lowercase title "agents", "N unread" badge — a list column, and a detail pane. | D1 (surface)   |
| D18 | List-column toolbar: the filter input, a **group-by select** (Status default / Project / Environment), an **unread-only bell toggle**, and a kebab menu with **Mark all read**.                                                                                                      | D6 (dropdown)  |
| D19 | Clicking a row **selects in place**: the detail pane mounts the thread's live `ChatView` (`routeKind: "server"`) while the list stays. Empty state: "Select an agent to view its activity". Selection is page-local, cleared when the row leaves the list.                           | —              |
| D20 | A per-row hover "jump to workspace" affordance exits the view with v1's click behavior: mark read → `setActiveEnvironmentId` → `navigateToThread`. The back arrow exits via `router.history.back()` with `/` fallback.                                                               | D8 (relocated) |
| D21 | Unread count (rows whose key is in `unreadThreadKeys`) renders on the nav-row badge and the view's top strip. Selecting a row marks it read (viewing = read). The unread-only toggle keeps the currently selected row visible even after it flips to read.                           | D11 (badge)    |
| D22 | Group-by modes: `status` keeps the fixed v1 order; `project` groups by project title and `environment` by environment label, groups ordered by their newest member (recency), rows recency-sorted within.                                                                            | D6             |

### 7.2 V2 pinned contracts

- **Route**: TanStack file route `apps/web/src/routes/agents.tsx`
  (`createFileRoute("/agents")`), deep-linkable; reload lands with no
  selection. `AppSidebarLayout` checks the pathname (`useLocation`) and, for
  `/agents`, renders `children` full-bleed without the `Sidebar`,
  `EnvironmentRail`, or `SidebarControl`.
- **Single-owner rule**: the embedded detail `ChatView` and the normal thread
  route are never mounted simultaneously — guaranteed structurally because
  `/agents` replaces the chat route; no portal machinery is needed or
  permitted.
- **Unread trigger placement**: `useAgentsUnread` moves out of the removed
  section into the always-mounted **nav row component**, still fed by the full
  `buildAgentRows` output, so transitions mark unread while the user is
  anywhere in the app. The nav row also derives the badge count.
- **Logic module**: `agentsSection.logic.ts` gains
  `AgentsGroupByMode = "status" | "project" | "environment"` and
  `buildAgentViewGroups(rows, { query, groupBy, unreadOnly, unreadThreadKeys, selectedKey })`
  → `ReadonlyArray<AgentViewGroup>` (`{ id, label, rows }`), implementing
  D18/D21/D22 as pure policy; `groupAgentRows` remains for compatibility until
  the section is removed, then is deleted with it.
- **Persistence**: group-by mode and unread-only toggle are page-local
  ephemeral state in v2 (the reference keeps them per-window); the v1
  `agentsSectionExpanded` / `agentsGroupExpandedById` uiState keys become the
  view's group-collapse store — section-expansion semantics retire with the
  section, and group collapse keeps Done-collapsed-by-default via
  `resolveAgentsGroupExpanded`, now keyed per group-by mode as
  `"<mode>:<groupId>"` (only `status:done` defaults collapsed).
- **Copy**: nav row label "Agents"; top strip title "agents"; badge
  `"{n} unread"`; detail empty state "Select an agent to view its activity".

### 7.3 V2 validation obligations

Same gates as §6, plus: route tests for `/agents` (takeover layout, back
navigation, selection → embedded ChatView, deep link), nav-row badge tests,
and a live Playwright pass matching the reference screenshots (list + detail,
back arrow round-trip). Docs updated in the same patch: `user/workspace-ui.md`
and `reference/encyclopedia.md` (v1 section wording → nav row + full view),
`architecture/connection-runtime.md` and
`plans/remote-servers/remote-servers-spec.md` exception sentences (the
cross-environment surface is now the Agents view + its nav badge), and
`testing/cross-platform-validation.md` enumeration.
