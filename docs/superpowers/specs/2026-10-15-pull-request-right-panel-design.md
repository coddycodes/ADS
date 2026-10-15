# Pull and merge requests in a thread's right panel

Status: **Draft** for review on 2026-10-15.

## Goal

The user's request: open the merge request detail (the full Pull Requests page
view of one request) in a thread's right panel, and open it automatically when
**Done** is pressed after creating a merge request from the Source Control
panel.

Decisions taken while brainstorming:

1. **Scope:** Done opens the request in the panel, and there is also a general
   way to open a request there.
2. **General entry points:** both inside the thread (next to the current
   branch's "View MR") and from the full-page detail view through a thread
   picker.
3. **Panel content:** one request per right-panel tab, with the full detail
   (Conversation, Commits, Pipelines, Changes, and every action). No list and no
   "Back to merge requests" in the panel. Several requests open as several tabs.
4. **Existing "View MR" actions** keep opening the browser. A separate "Open
   in panel" action is added next to them.

Approach chosen: a new right-panel surface kind that hosts the existing
`PullRequestsDetailView`, made independent of the router. Rejected: running the
full-page route inside the panel with a second router (two routers, chrome to
hide, panel state outside the thread's), and a smaller summary panel (not the
full detail the user chose).

## 1. Right-panel surface and persisted state

`apps/web/src/rightPanelStore.ts`:

- `RIGHT_PANEL_KINDS` gains `"pullRequest"`, and `RightPanelSurface` gains
  `{ id: \`pullRequest:${number}\`; kind: "pullRequest"; number: number; tab: PullRequestsDetailTab }`.
  `PullRequestsDetailTab` is `"conversation" | "commits" | "checks" | "files"`
  (`apps/web/src/pullRequestsStore.ts`).
- A thread may hold several such surfaces. Opening a number that is already
  open activates it; it never adds a duplicate.
- New actions:
  - `openPullRequest(ref, number, tab = "conversation")`: add or activate the
    surface and show the panel. Activating an existing surface keeps its saved
    tab unless a tab is passed explicitly.
  - `setPullRequestTab(ref, number, tab)`: record the selected detail tab.
- `pullRequest` is excluded from the generic singleton `open`/`toggle` kinds,
  like `file`, `terminal` and `activity`.
- The surfaces persist with the thread's right-panel state.
  `RIGHT_PANEL_STORAGE_VERSION` goes from 9 to 10. The migration keeps v9 state
  unchanged; validation drops a `pullRequest` surface whose `number` is not a
  positive integer or whose `tab` is unknown, instead of discarding the thread's
  panel state.
- `RightPanelTabs` labels the tab with the provider icon and the request
  reference (`!89` on GitLab, `#89` on GitHub, from the change request
  presentation). The request title is the tooltip once the detail has loaded.
- `ChatView` renders `PullRequestPanelSurface` for the active `pullRequest`
  surface, beside the existing `sourceControl`, `diff` and `plan` branches. It
  receives the thread ref, the project ref, `gitCwd` and `workspaceUnavailable`,
  the same scope `SourceControlPanel` receives.

## 2. Detail navigation and the panel host

### Navigation interface

`PullRequestsDetailView` stops importing the router. It receives:

```ts
interface PullRequestsDetailNavigation {
  /** The user picked another detail tab. */
  selectTab(tab: PullRequestsDetailTab): void;
  /** An action created another request (a revert request). */
  openRequest(number: number): void;
  /** The request was deleted. */
  requestGone(): void;
  /** "Back to merge requests", or null where there is no list to go back to. */
  back: (() => void) | null;
}
```

`usePullRequestsAction`'s `navigate(number | null)` maps onto `openRequest`
(a number) and `requestGone` (`null`).

- **Full page:** the pull-requests route builds the router adapter: exactly the
  navigations the view makes today (tab change with `replace`, the list, another
  request's conversation tab). URLs and behaviour do not change.
- **Panel adapter:**
  - `selectTab` calls `setPullRequestTab`.
  - `openRequest(n)` calls `openPullRequest(thread, n)`, so a revert request
    opens as a new tab beside the original.
  - `requestGone` closes this surface; the existing "Request deleted" toast
    remains.
  - `back` is `null`.

### `PullRequestPanelSurface`

A new component under `apps/web/src/components/pullRequests/`. It resolves what
`PullRequestsPanel` resolves for the full page, scoped to the thread:

- availability: connection state, server support and the Pull Requests setting
  (`resolvePullRequestsAvailability`);
- the project (from the thread's project ref);
- the repository context for the thread's `gitCwd`
  (`pullRequestsEnvironment.getContext`).

When pull requests are not usable, it renders the existing
`PullRequestsUnavailableState` with its reason inside the tab: turned off in
settings, environment disconnected, no supported host, or the thread's workspace
unavailable. Otherwise it renders a compact header and the detail view with the
panel adapter and `scope = { environmentId, cwd: gitCwd }`.

The compact header shows the repository, **Open in browser** (the request's web
URL) and **Open full page** (navigates to
`/project/$environmentId/$projectId/pull-requests/$number` with the surface's
tab). Checkout and every other action behave as on the full page, against the
thread's checkout.

## 3. Entry points and layout

### Done in the create dialog

`GitManagerCreatePullRequestDialog` gains an optional
`onDone?: (request: { number: number }) => void`. **Done** calls it after the
request was created or an existing one was found, when the host returned a
number (`GitRunStackedActionResult.pr.number`), then closes the dialog. Without
a number, Done only closes, as today. Only `SourceControlPanel` passes `onDone`;
it calls `openPullRequest(threadRef, number)`. The Git actions control, the Pull
Requests page and the git manager keep today's Done.

### "Open {MR/PR} in panel" in the thread

Both menu models gain an item right after "View {MR/PR}", labelled "Open
{MR/PR} in panel": Source Control's
(`apps/web/src/components/SourceControlPrimaryAction.logic.ts`) and the Git
actions control's (`buildMenuItems` in
`apps/web/src/components/GitActionsControl.logic.ts`). It is present only when
the branch has an open request and the caller has a thread to open it in.
`SourceControlPanel` and `GitActionsControl` call `openPullRequest` with
`status.pr.number`. The primary "View {MR/PR}" button and menu item keep opening
the browser.

### "Open beside a thread…" on the full page

A header action in the full-page detail view opens a picker listing the
project's threads that are not archived:

- threads whose `branch` equals the request's `headBranch` first, marked
  "On this branch";
- then the rest, most recently active first.

Picking a thread opens the request's surface in that thread (`openPullRequest`)
and navigates to the thread. With no thread in the project, the action is
disabled and its tooltip says there is no thread to open it beside.

### Layout

The detail view's root becomes a size container. The side column (Reviewers,
Assignees, Labels, Milestone, Linked issues, Approval rules) and its inline
variant in `PullRequestsConversation` switch on the container's width instead of
the viewport's (`lg:` becomes the equivalent container breakpoint). On a wide
window the full page looks the same; in the narrow panel the side column is
shown inline, as on a narrow window today.

## 4. Edge cases

- **Request not found** (deleted on the host, wrong number): the detail query's
  existing error card and Retry, inside the tab. The tab stays until closed.
- **Workspace unavailable** (worktree removed, environment disconnected): the
  host shows the unavailable state; the persisted surface reloads the request
  once the workspace returns.
- **Several threads:** surfaces belong to their thread, like terminal and
  browser surfaces.
- **Same request on the full page and in a panel:** both read the same cached
  queries, so an action in one refreshes the other.
- **Remote, SSH and WSL environments:** supported through the thread's
  environment, as Source Control is. No desktop bridge is involved.
- **Profiler, crash and trace coverage:** none applies. No new RPC method, child
  process, worktree removal path, binary, service or event.

## Testing

Test-first, at the public seams:

- `rightPanelStore`:
  - `openPullRequest` adds a surface, activates an existing one without
    duplicating it, and shows the panel;
  - `setPullRequestTab` persists the tab;
  - v9 state migrates to v10 unchanged, and a malformed `pullRequest` surface is
    dropped without losing the thread's other surfaces.
- `PullRequestsDetailView` with a fake navigation: tab change calls
  `selectTab`; a revert calls `openRequest(n)`; a delete calls `requestGone`; no
  back link when `back` is `null`. The existing full-page tests keep covering
  the router adapter.
- Create dialog: Done calls `onDone` with the number only after the request was
  created or found, and does not call it when the outcome has no number.
- `SourceControlPanel`: Done opens the panel surface; "Open in panel" is listed
  only for an open request in a thread and opens the surface.
- Picker ordering: branch matches first, then most recent; archived threads
  excluded; disabled with a reason when the project has no thread.
- `PullRequestPanelSurface`: each unavailable reason, the header's two actions,
  and the detail view rendered with the thread's scope.

## Documentation

In the same change:

- `docs/user/workspace-ui.md`: the right panel's request tab and its three entry
  points.
- `docs/architecture/overview.md`: the `pullRequest` surface and the detail
  view's navigation interface.
- `CHANGELOG.md` entry.

Reviews required by `AGENTS.md`: `vercel-react-best-practices` for every changed
component and hook, and `UI.md` for the new tab, header, menu item and picker.

## Out of scope

- A request list inside the panel.
- Changing what "View {MR/PR}" does.
- Opening the panel from the Git actions control's or git manager's create
  dialog.
