# Pull and merge requests in a thread's right panel

Status: **Approved** on 2026-10-15. Refined while planning (see "Planning refinements").

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
  `{ id: \`pullRequest:${number}\`; kind: "pullRequest"; number: number; provider: SourceControlProviderKind; tab: PullRequestsDetailTab }`.
  `provider` is recorded when the surface opens, so the tab label needs no
  query.
  `PullRequestsDetailTab` is `"conversation" | "commits" | "checks" | "files"`
  (`apps/web/src/pullRequestsStore.ts`).
- A thread may hold several such surfaces. Opening a number that is already
  open activates it; it never adds a duplicate.
- New actions:
  - `openPullRequest(ref, { number, provider }, tab?)`: add or activate the
    surface and show the panel. A new surface starts on `tab` or Conversation;
    activating an existing surface keeps its saved tab unless a tab is passed.
  - `setPullRequestTab(ref, number, tab)`: record the selected detail tab.
- `pullRequest` is excluded from the generic singleton `open`/`toggle` kinds,
  like `file`, `terminal` and `activity`.
- The surfaces persist with the thread's right-panel state. Saved state is
  normalized by content on every load (`migratePersistedRightPanelState` runs
  for `migrate` and `merge`), so `RIGHT_PANEL_STORAGE_VERSION` stays 9: an older
  build drops the surfaces it does not know. Normalization drops a
  `pullRequest` surface whose `number` is not a positive safe integer, whose
  `id` does not match, or whose `provider` is unknown, and resets an unknown
  `tab` to Conversation, without discarding the thread's other surfaces.
- `RightPanelTabs` labels the tab with a pull request icon and the request
  reference from `formatChangeRequestNumber` (`!89` on GitLab, `#89` on
  GitHub).
- `ChatView` renders `PullRequestPanelSurface` for the active `pullRequest`
  surface, beside the existing `sourceControl`, `diff` and `plan` branches. It
  receives the thread ref, the project ref, `gitCwd` and `workspaceUnavailable`,
  the same scope `SourceControlPanel` receives.

## 2. Detail navigation and the panel host

### Navigation interface

`PullRequestsDetailView`, `PullRequestsFiles` and the conversation's file links
stop importing the router. They read a navigation from React context
(`PullRequestsNavigationContext`, `usePullRequestsNavigation()`), which every
host provides; reading it without a provider throws.

```ts
interface PullRequestsNavigation {
  /** The user picked another detail tab. */
  selectTab(tab: PullRequestsDetailTab): void;
  /** A conversation link to a file: the Changes tab, scrolled to that file. */
  showFile(path: string): void;
  /** The file tree selected a file: replace the anchor in place. */
  setFileAnchor(path: string): void;
  /** An action created another request (a revert request). */
  openRequest(number: number): void;
  /** The request was deleted. */
  requestGone(): void;
  /** "Back to merge requests", or null where there is no list to go back to. */
  readonly back: (() => void) | null;
}
```

The file anchor was the URL hash (`#file=<path>`); a panel has no URL, so the
host owns it. It lives in its own `PullRequestsFileAnchorContext`
(`usePullRequestsFileAnchor()`, `null` without a provider), not on the
navigation object: the navigation holds only methods, so its identity does not
change when the anchor does, and an anchor change re-renders `PullRequestsFiles`
rather than every action consumer.

`usePullRequestsAction`'s `navigate(number | null)` maps onto `openRequest`
(a number) and `requestGone` (`null`).

- **Full page:** `PullRequestsRouteNavigation` provides the router adapter:
  exactly the navigations made today (tab change with `replace`, the file
  anchor in the hash, the list, another request's conversation tab). URLs and
  behaviour do not change. The conversation's file link becomes a button that
  calls `showFile` instead of a router `<Link>`.
- **Panel adapter:**
  - `selectTab` calls `setPullRequestTab`; `showFile` also sets the file
    anchor, which the host keeps in component state.
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
unavailable. Otherwise it renders the detail view with the panel adapter and
`scope = { environmentId, cwd: gitCwd }`. The context loading it shares with the
full page (stored context, then one live read; rescan; auth recovery) moves from
`PullRequestsPanel` into a `usePullRequestsContextQuery` hook both use.

`PullRequestsDetailView` gains a `headerActions` slot in its top row, where the
full page shows "Back to merge requests". The panel puts **Open full page**
there (navigates to `/project/$environmentId/$projectId/pull-requests/$number`
with the surface's tab); the detail header already has **Open in browser**.
Checkout and every other action behave as on the full page, against the
thread's checkout.

## 3. Entry points and layout

### Done in the create dialog

`GitManagerCreatePullRequestDialog` gains an optional
`onDone?: (request: { number: number; provider: SourceControlProviderKind }) => void`. **Done** calls it after the
request was created or an existing one was found, when the host returned a
number (`GitRunStackedActionResult.pr.number`), then closes the dialog. Without
a number, Done only closes, as today. Only `SourceControlPanel` passes `onDone`;
it calls `openPullRequest(threadRef, number)`. The Git actions control, the Pull
Requests page and the git manager keep today's Done.

### "Open {MR/PR} in panel" in the thread

Source Control's menu gains an item right after "View {MR/PR}", labelled "Open
{MR/PR} in panel", in both of its menu models: the staging-area menu
(`apps/web/src/components/SourceControlPrimaryAction.logic.ts`) and the legacy
menu (`buildMenuItems` in `apps/web/src/components/GitActionsControl.logic.ts`,
through a `canOpenInPanel` option). It is present only when the branch has an
open request, and calls `openPullRequest` with `status.pr.number`. The primary
"View {MR/PR}" button and menu item keep opening the browser. The chat header's
`GitActionsControl` is not changed: it always renders with `hideTrigger`, so its
menu is never shown.

### "Open beside a thread…" on the full page

An action in the full-page detail view's `headerActions` slot opens a picker
listing the project's threads that are not archived:

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

## Planning refinements

Found while reading the code for the plan, after approval:

- No storage version bump: saved panel state is normalized by content on load.
- The surface records `provider`, so the tab label needs no query; no title
  tooltip.
- Navigation comes from React context, with the Changes tab's file anchor in a
  companion context, because `PullRequestsFiles` and the conversation's file
  links used the router and the URL hash.
- The panel's "Open full page" sits in a `headerActions` slot of the detail
  view; "Open in browser" was already in the detail header.
- The conversation's file link is a button that calls `showFile`, so on the full
  page it no longer offers Ctrl/Cmd-click, middle-click or Copy link; an optional
  `fileHref(path)` on the navigation could restore an `<a>` there later.

## Out of scope

- A request list inside the panel.
- Changing what "View {MR/PR}" does.
- The chat header's Git actions menu (hidden in production).
- Opening the panel from the Git actions control's or git manager's create
  dialog.
