# Left panel rows and width — Orca comparison research

Research of Orca's left panel (sidebar) item rendering in `/work/github/orca` (v1.4.197,
commit `122b8c25d7`, Electron) against BiBCode's left panel at `3d4bc210` (`main-3`). The
cited lines are unchanged at `fd5effbb`, which landed while this was being written. Date:
2026-09-24. Path conventions:

- BiBCode paths are relative to the repository root. Bare component file names (for
  example `Sidebar.tsx`, `ui/sidebar.tsx`, `AppSidebarLayout.test.tsx`) are under
  `apps/web/src/components/`.
- Orca paths are prefixed `orca/` (relative to `/work/github/orca`). Bare Orca file names
  in section 2 are under `orca/src/renderer/src/components/sidebar/`.
- Line numbers refer to the working trees at research time.

Method: direct source reading (the CodeGraph sync failed, so graph data was not used) and
pixel measurements with Python PIL on the user's screenshots. The screenshots are macOS
Retina captures, so CSS px = device px ÷ 2. Orca was not run: its checkout has no
`node_modules`, and installing would modify it.

Visual sources (the user's screenshots are not committed; paths are recorded for provenance):

- **U1** — BiBCode on macOS, 900×2198 device px:
  `/tmp/orca-paste-1790256352304-1ed3ba10-140a-4c3c-b2f7-bd5e082e4dd8.png`.
- **U2** — BiBCode environment context card with its ⋯ menu open:
  `/tmp/orca-paste-1790256284910-0136cffa-82a4-4bdd-921b-49a7df631ffc.png`.
- **U3** — Orca's current left panel, 524×3200 device px (262 CSS px wide):
  `/tmp/orca-paste-1790257150192-592a59d0-6fd5-4ead-82a0-910ce9bd380d.png`.
- **O1** — `orca/docs/assets/readme-hero.jpg`: the sidebar grouped by status (Pinned /
  In progress), with cards showing a repo badge, the branch, PR icons and agent rows.
- **O2** — `orca/docs/site/public/docs/ways-to-run-local-sidebar.png`: cards with a
  "Local Mac" host badge and a collapsed "3 agents" summary row.

## Summary

- **Width target: 422 CSS px in total, 370 px for the projects panel.** In U1 the window
  content starts at device x 4, the rail/panel separator is at x 106–107 and the panel's
  right edge is at x 846–847. That gives a 52 px rail (matches `w-[52px]`), a 370 px
  panel and a 422 px `--sidebar-width`. Both widths include each column's 1 px
  panel-separator border. Today a first launch opens at **320 px** (268 panel + 52 rail):
  `THREAD_SIDEBAR_DEFAULT_WIDTH = 268 + ENVIRONMENT_RAIL_WIDTH`
  (`apps/web/src/components/AppSidebarLayout.tsx:19`). To match U1, change it to
  `370 + ENVIRONMENT_RAIL_WIDTH`. Two other places pin 320 and must change with it: the
  test at `AppSidebarLayout.test.tsx:65-77` and the text at `docs/user/workspace-ui.md:14`.
- **One constraint breaks at 422.** Today the 320 default fits the desktop minimum window
  exactly: 960 − 320 = 640, which is `THREAD_MAIN_CONTENT_MIN_WIDTH`. That 640 px rule only
  runs while dragging (`AppSidebarLayout.tsx:114-115`). The first-launch value is applied
  as-is, as a constant CSS variable (`AppSidebarLayout.tsx:20-22`). A restored value is
  only clamped to min/max (`ui/sidebar.tsx:588-600`). At 422, any window narrower than
  1062 px leaves less than 640 px for the main content. Recommendation: add a clamp of the
  first-launch width to `innerWidth − 640`, never below the 260 px minimum.
- **Orca's "more space and information" comes from how much each item shows, not from
  width.** U3 is only 262 CSS px wide, narrower than BiBCode's current 268 px panel. Each
  Orca worktree is a 2–3 line card:
  - a status column, a host glyph, the name and a "primary" chip;
  - a muted branch line, with PR or ports icons on the right;
  - one 24 px row per agent session: state glyph, provider icon, session title, model,
    and age. Sub-agent rows nest under a guide line.
- **BiBCode draws one 24–28 px line per item, with small inline indicators.** In U1 the
  extra width stays empty. "No threads yet" repeats under every primary row, even though
  the primary row is itself a chat. The sidebar also breaks several UI.md rules:
  - text at 10, 11, 9 and 8 px;
  - alpha-reduced muted text;
  - 12 px thread titles, where UI.md specifies 13 px.
- **Recommendation: switch the rows to cards and keep the tree.** Replace the thread and
  primary rows with a "workspace card": a title line, a branch line and a session line.
  Keep BiBCode's 12 px text floor, its tokens and its environment rail. Nearly all the
  data is already on the client:
  - from the thread shell: title, branch, session provider and status,
    `modelSelection.model`, `conversationPreview` and timestamps;
  - from the VCS summary: branch, the dirty flag and the PR.
- **Server work is needed only for:**
  - nested session rows per worktree (the panel-thread shell has no host-thread link);
  - CI check status for the PR icon;
  - any Orca feature that needs saved per-workspace data (status columns, groups,
    parents, sleep).

  Showing GitHub owner avatars as project icons needs a privacy decision first.

- **Menus.** Orca's worktree menu is split into sections by separators. BiBCode's native
  menus have no separator support in the contract (`packages/contracts/src/ipc.ts:115-125`).
  **"Update" means different things in the two apps.** In Orca it edits the workspace's
  metadata. In BiBCode it runs `git pull` (`apps/web/src/components/Sidebar.tsx:1593`).
  Rename BiBCode's item to "Pull". BiBCode's project actions are only reachable by
  right-click; Orca shows a hover ⋯ button.
- **Discrepancy found.** `docs/user/workspace-ui.md:96-97` says primary rows offer
  "remove project". In the code, Remove exists only on the project header menu
  (`Sidebar.tsx:2988-2990`, `3020-3036`).

---

## 1. BiBCode left panel width model

### 1.1 Where the width is defined

| Constant / setting              | Value                                     | Source                                                                                                                                                                                                                         |
| ------------------------------- | ----------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `ENVIRONMENT_RAIL_WIDTH`        | 52 px                                     | `apps/web/src/components/AppSidebarLayout.tsx:14`; the rail renders `w-[52px] … border-r border-panel-separator` (`sidebar/EnvironmentRail.tsx:212-215`). Tailwind uses border-box, so the 1 px separator is inside the 52 px. |
| `THREAD_SIDEBAR_MIN_WIDTH`      | 13×16 + 52 = 260 px                       | `AppSidebarLayout.tsx:15`                                                                                                                                                                                                      |
| `THREAD_SIDEBAR_DEFAULT_WIDTH`  | 268 + 52 = **320 px**                     | `AppSidebarLayout.tsx:16-19`. The comment says "268px matches the reference app's rows".                                                                                                                                       |
| Maximum width                   | none (`Number.POSITIVE_INFINITY`)         | `ui/sidebar.tsx:210`; `AppSidebarLayout` passes no `maxWidth` (`AppSidebarLayout.tsx:111-117`)                                                                                                                                 |
| `THREAD_MAIN_CONTENT_MIN_WIDTH` | 640 px                                    | `AppSidebarLayout.tsx:23`. Used only by `shouldAcceptWidth` (`AppSidebarLayout.tsx:114-115`).                                                                                                                                  |
| Primitive fallback              | 16rem (256 px)                            | `ui/sidebar.tsx:31`, `174`. Overridden by the provider style (`AppSidebarLayout.tsx:20-22`, `105`).                                                                                                                            |
| Mobile sheet width              | `calc(100vw − 12px)`                      | `ui/sidebar.tsx:32`, `253-256`                                                                                                                                                                                                 |
| Storage key                     | `bibcode:sidebar-width:v2` (localStorage) | `AppSidebarLayout.tsx:12-13`                                                                                                                                                                                                   |

A single CSS variable, `--sidebar-width`, set on the provider wrapper, covers **rail + panel**.
The fixed `sidebar-container` is `w-(--sidebar-width)` and carries the orange right border
(`ui/sidebar.tsx:299-311`; `AppSidebarLayout.tsx:110`). Inside it, a flex row splits the
width: the rail is fixed at 52 px and the panel is `flex-1` (`AppSidebarLayout.tsx:119-124`).

### 1.2 Persistence and restore

- **Drag.** The resize handle is a 16 px hit area straddling the edge
  (`ui/sidebar.tsx:620-631`). During a drag it updates the variable once per animation
  frame, but only after `shouldAcceptWidth` passes (`ui/sidebar.tsx:472-519`). On pointer-up
  it writes the width as a JSON number to `bibcode:sidebar-width:v2`
  (`ui/sidebar.tsx:393-408`).
- **Restore.** After mount, the rail's effect reads the key, clamps the value to
  `[min, max]` and applies it (`ui/sidebar.tsx:588-600`). It does **not** run
  `shouldAcceptWidth`.
- **Reset.** Double-clicking the handle restores the default and removes the key
  (`ui/sidebar.tsx:569-586`). This is documented at `docs/user/workspace-ui.md:14-15`.
- **Retired key.** At boot, `chat_thread_sidebar_width` (from the 256 px era) is removed
  (`apps/web/src/clientStateMigrations.ts:2-9`; `apps/web/src/main.tsx:14-16`). Commit
  `d46f46fa` introduced `v2` so that the 320 px default would apply once to existing
  profiles.
- **Collapsed state.** The open/collapsed state is written to a `sidebar_state` cookie
  (`ui/sidebar.tsx:130-136`) but never read back. `AppSidebarLayout` always passes
  `defaultOpen` (`AppSidebarLayout.tsx:95`, `104`), so the panel always opens expanded.

### 1.3 Desktop, browser and mobile

- **Desktop (Tauri) and browsers** run the same React code.
- **Desktop window:** 1280×900 by default, 960×640 minimum
  (`apps/desktop/src-tauri/tauri.conf.json:18-21`). With the 320 px default, the default
  window leaves 960 px for the main content. The minimum window leaves exactly 640 px,
  which equals `THREAD_MAIN_CONTENT_MIN_WIDTH`.
- **Browser:** any viewport. The stored width is kept per origin.
- **Below 768 CSS px** (`useIsMobile` = `max-md`, `apps/web/src/hooks/useMediaQuery.ts:8`,
  `85-87`), the sidebar becomes a Sheet overlay at `calc(100vw − 12px)`. It cannot be
  resized and ignores the stored width (`ui/sidebar.tsx:202-205`, `239-275`). The desktop
  container is `hidden … md:flex` (`ui/sidebar.tsx:280`, `301`).
- **`/agents` route:** renders without the left sidebar (`AppSidebarLayout.tsx:93-99`).
- **Window resize:** the sidebar width does not respond; only the main content shrinks.

### 1.4 First launch today

320 CSS px in total: the rail is 52 (51 content + 1 border) and the projects panel is 268
(267 content + 1 border). On a Retina display that is 640 device px.

### 1.5 Target measured from U1

Separator columns were found as orange pixels (the panel-separator is `#d8610e` at 25%,
about `(247, 219, 204)` over white) on rows y = 300, 700, 1200, 1500 and 1900:

| Edge                                 | Device x | CSS px                    |
| ------------------------------------ | -------- | ------------------------- |
| Window content start                 | 4        | 0                         |
| Rail / panel separator (rail border) | 106–107  | rail = 104 / 2 = **52**   |
| Panel right edge (container border)  | 846–847  | panel = 740 / 2 = **370** |
| Total `--sidebar-width`              | 4 … 847  | **422**                   |

That is 102 px wider than the current default. It is presumably a width the user dragged
to, stored under `v2` on that Mac.

### 1.6 Change needed

1. `AppSidebarLayout.tsx:16-19`: `THREAD_SIDEBAR_DEFAULT_WIDTH = 370 + ENVIRONMENT_RAIL_WIDTH`
   (422). Rewrite the comment: the width now comes from the user's measurement, not from
   the reference app.
2. Update the test at `AppSidebarLayout.test.tsx:65-77`, which expects `"320px"` and `320`.
   `ui/sidebar.rail.test.tsx:442-455` uses its own 320 fixture and needs no change.
3. Update `docs/user/workspace-ui.md:14` ("The panel opens 320px wide") to 422 px, or
   "370px beside the 52px rail". The mockup's 268 px panel
   (`docs/plans/remote-servers/mockups/left-panel-switcher.html:197-198`) stays as history.
4. Keep the 260 px minimum. A maximum is optional. Today dragging is bounded only by the
   640 px main-content rule; Orca caps its whole sidebar at 500 px
   (`orca/src/renderer/src/components/sidebar/index.tsx:35`).
5. **Small windows — pick one:**
   - **(a) Recommended.** Apply the main-content rule to the first-launch width:
     `max(260, min(422, window.innerWidth − 640))`, computed at mount. Optionally apply it
     to restored widths too. A 960 px desktop window then opens at 320 and any window of
     1062 px or wider at 422. This keeps today's invariant, adds no setting, and a width
     the user dragged to still wins. It belongs in the shared primitive's
     default/restore path (`ui/sidebar.tsx:588-600`), next to the drag rule it mirrors.
   - (b) Raise Tauri `minWidth` to 1062. Not recommended: the window would no longer fit a
     1024 px-wide display.
   - (c) Lower `THREAD_MAIN_CONTENT_MIN_WIDTH` to 538. This squeezes the chat and terminal
     at the minimum window size.
   - (d) Accept a 538 px main area at the minimum window size.
6. **Existing profiles.** Widths stored under `v2` keep winning. If existing users should
   also see the new default, bump the key to `v3` and retire `v2` in
   `clientStateMigrations.ts`, as commit `d46f46fa` did. See Open questions.

---

## 2. Orca's left panel (source and screenshots)

Bare file names in this section live under `orca/src/renderer/src/components/sidebar/`;
`main.css` is `orca/src/renderer/src/assets/main.css`.

### 2.1 Layout from top to bottom

`orca/src/renderer/src/components/sidebar/index.tsx:164-207` composes the panel:

1. **`SidebarNav`**
   - Search: a pill with `bg-worktree-sidebar-foreground/5`, 13 px medium text, shortcut
     keys shown on hover (`SidebarNav.tsx:90-118`).
   - Tasks (`SidebarTaskNavButton.tsx:183`), optional Artifacts and Skills, Automations
     (`SidebarNav.tsx:204`).
   - Orca Mobile with a "New" pill at `text-[10px]` (`SidebarNav.tsx:244-248`).
2. **`SidebarHeader`: "Projects"** (`text-xs font-semibold text-muted-foreground/80`, `h-8`
   row), with four actions (`SidebarHeader.tsx:49-128`; `sidebar-header-actions.tsx:17-101`):
   - **bell** — "View activity": switches the panel body to the agents list;
   - **sliders** — the workspace options menu;
   - **folder-plus** — Add project;
   - **+** — New workspace.
3. **`WorktreeList`** — a virtualized list (TanStack Virtual) of project header rows,
   worktree cards and notice rows.
4. **`SetupScriptPromptCard`** and the **`SidebarToolbar`** footer (`SidebarToolbar.tsx:74-100`):
   - left: settings and help menu;
   - right: crosshair (scroll to the current workspace) and the Kanban board toggle.

Orca has **no environment rail.** Host section headers
(`worktree-list/rows/HostSectionHeader.tsx:104`) are skipped in two cases:

- the default Projects view with all hosts in scope ("project is the user's primary
  object and host is context inside it");
- one visible host or fewer (`host-section-rows.ts:166-193`).

So U3 has no host headers, and host context comes from the per-card glyph and badge (see
2.3). Host headers appear only for explicit multi-host filters or other groupings.

### 2.2 Width

- **Defaults:** 280 px by default, 220 px minimum, 500 px maximum.
- **Persistence:** the width is saved in Orca's persisted UI state (main process), not in
  localStorage, and clamped on hydration.
  - `orca/src/shared/constants.ts:248`;
  - `orca/src/renderer/src/components/sidebar/index.tsx:34-35`, `140-148`;
  - `orca/src/renderer/src/store/slices/ui/ui-slice-hydration-actions.ts:69`, `124-129`;
  - `orca/src/renderer/src/store/slices/ui/ui-slice-hydration-sanitizers.ts:21`, `96-105`.
- **U3 measures 262 CSS px.** The footer icons sit 18 CSS px from both edges, so the
  capture spans the whole sidebar.

### 2.3 Worktree card (the item the user likes)

**Configuration shown in U3: Orca's defaults.**

- Card style: `experimentalNewWorktreeCardStyle: false` and `compactWorktreeCards: false`
  (`orca/src/shared/default-global-settings.ts:249`, `251`;
  `use-worktree-card-foundation.ts:47-48`).
- Card properties: status, unread, issue, linear-issue, jira-issue, pr, automation, cli,
  comment, ports, inline-agents (`orca/src/shared/worktree/card-properties.ts:5`,
  `13-25`; `orca/src/shared/constants.ts:284`).
- Agent activity: `'compact'` (`orca/src/shared/constants.ts:36`).

This explains three details of U3. The amber bell is the legacy status slot. "primary" is
an inline badge. The branch line repeats even when it equals the name ("alpha / alpha"),
because the detailed mode never hides a duplicate branch
(`worktree-card-presentation.tsx:82-86`).

**Card anatomy:**

- **Surface.** `rounded-lg`, `ml-1`, `pr-1.5`, with `pt-1.25 pb-1.5` padding (`py-2` when
  the card is title-only) and a transparent 1 px border (`worktree-card-surface.tsx:49-63`).
- **Status column** (left, top-aligned, `pt-[2px]`, `worktree-card-parent-content.tsx:133-153`).
  `StatusIndicator` (`StatusIndicator.tsx:43-95`) shows:
  - working: a spinner;
  - needs permission: a question glyph;
  - done or active: an 8 px emerald dot;
  - inactive: a grey dot (`bg-neutral-500/40`).

  Unread replaces all of these with a 13 px filled amber bell, and hovering reveals a
  mark-read toggle (`WorktreeCardStatusSlot.tsx:181`, `225-245`). The full status set is
  active, working, monitoring, permission, interrupted, done and inactive
  (`orca/src/renderer/src/lib/worktree-status.ts:15-42`).

- **Title line** (`worktree-card-header.tsx`):
  - **Host glyph:** lucide `Server` (`size-3`), for SSH projects ("Project on SSH host",
    `WorktreeCardSshHostControl.tsx:139-155`) or Orca-server projects ("Project on
    {hostName}", `worktree-card-header.tsx:118-128`). `ServerOff` means disconnected.
    This is the stacked-rectangles icon in U3: it means "this project lives on another
    host", not "worktree".
  - **Repo chip:** only when projects are mixed, e.g. in the pinned section (`158-170`).
  - **Title:** `text-[13px] leading-5`. Unread titles are `font-semibold text-foreground`,
    read titles normal weight (`173-175`; `WorktreeTitleInlineRename.tsx:339-341`).
  - **Badges:** "rename failed"; **"primary"** (`h-[16px] text-[10px]`, `215-233`);
    "sparse".
  - **Delete:** a quick action shown on hover (`294-318`).
- **Meta line** (`worktree-card-meta-row.tsx:48-107`):
  - left:
    - repo badge (hidden when the list is grouped by project);
    - host context badge (only when a group spans more than one host,
      `WorktreeHostContextBadge.tsx:10`);
    - **branch** at 11 px muted (`76-77`) — or a detached-HEAD badge;
    - conflict-operation badge;
    - prompt-cache timer.
  - right-aligned detail icons:
    - ports: a Plug icon (`WorktreeCardPorts.tsx:29-52`);
    - metadata badges: notes, automation, CLI, GitHub issue, Linear, Jira, PR/MR
      (`WorktreeCardMetaBadges.tsx:88-152`).

  The PR icon's color follows its checks: rose when failing, amber when pending, emerald
  when passing or open, purple when merged, muted when closed or draft
  (`worktree-review-helpers.tsx:33-79`).

- **Secondary rows** (`worktree-card-secondary-rows.tsx:41-75`): a remote-branch conflict
  warning, the inline agent list and a chip for child worktrees.
- **Agent rows** (compact mode: `WorktreeCardAgents.tsx:357-400`;
  `worktree-card-compact-agent-row.tsx:18-66`, `217-290`). Each row is `h-6` (24 px) with
  11 px text and `gap-1`, and contains, in order:
  1. an optional disclosure chevron hung in the card gutter (`-ml-5`);
  2. the `AgentStateDot` (`orca/src/renderer/src/components/AgentStateDot.tsx:95-137`):
     done = emerald `CircleCheck`, working = spinner, waiting or permission = question
     glyph;
  3. the provider icon (13 px);
  4. the **primary text**: the conversation name or prompt, otherwise the state label;
  5. a trailing "‑ secondary": tool preview, then last assistant message, then agent type;
  6. the **model**, monospace 10 px, `max-w-24`;
  7. "+N" when children are collapsed, and the cache timer;
  8. the **age** (10 px): time since done, or since start.

  Children render inside `.worktree-agent-lineage-children`: a 12 px left margin with a
  1 px guide line (`orca/src/renderer/src/assets/main.css:1545-1553`). More than one root
  agent collapses into an "N agents" summary row (`WorktreeCardAgents.tsx:357-400`; O2).
  One 30 s clock drives each non-empty list (`WorktreeCardAgents.tsx:197`).

- **Measured in U3 (CSS px).**

  | Distance                          | CSS px                                           |
  | --------------------------------- | ------------------------------------------------ |
  | Name line → branch line (centers) | 21–23                                            |
  | Branch line → first agent row     | 24                                               |
  | Agent row pitch                   | 26                                               |
  | Two-line card pitch               | 59 (BiBCode: 25–32 per row)                      |
  | Project header → first card       | 36                                               |
  | Branch text left edge             | 42, from the sidebar edge (under the host glyph) |
  | Title left edge                   | 59, from the sidebar edge                        |
  | Right inset of the ages           | 14                                               |

### 2.4 Card states

| State                     | Look                                                                                 | Source                                                                   |
| ------------------------- | ------------------------------------------------------------------------------------ | ------------------------------------------------------------------------ |
| Hover                     | 4% foreground wash, **no border**                                                    | `main.css:1271-1277`                                                     |
| Active                    | 8% foreground wash, faint border, 1 px shadow ("filled rounded card", `fixes` in U3) | `main.css:1281-1295`                                                     |
| Secondary active          | Same worktree shown twice (e.g. a pinned copy): ring-tinted border and accent wash   | `main.css:1297-1307`; `worktree-list/navigation/use-active-row.ts:57-69` |
| Multi-selected            | `border-worktree-sidebar-ring/35 bg-worktree-sidebar-accent/70 ring-1`               | `worktree-card-surface.tsx:61-62`                                        |
| Sleeping                  | Text tokens mixed toward the surface in oklab                                        | `main.css:1316-1330`                                                     |
| Runtime disconnected      | `opacity-60`                                                                         | `worktree-card-surface.tsx:77`                                           |
| Deleting                  | 50% opacity, grayscale and a label overlay                                           | `worktree-card-surface.tsx:70`, `92-101`                                 |
| Agent row hover / focused | 1.25% wash / accent fill                                                             | `main.css:1332-1350`                                                     |

The bordered `open-trello` card in U3 matches the multi-selected or secondary-active
styling. It is not the plain hover style.

### 2.5 Project header row

- **Row:** `h-7`, left padding 10 px, a 16 px icon, the name at `text-[13px]
  font-semibold leading-none`, plus fork, path-status and scan indicators
  (`worktree-list/rows/SectionHeader.tsx:235-340`).
- **Icon (`RepoIconGlyph`):** an image (uploaded, repo file, website favicon or GitHub
  owner avatar), an emoji, or a lucide icon; the default is Folder
  (`orca/src/renderer/src/components/repo/repo-icon.tsx:134-177`). The avatar URL is
  `https://<host>/<owner>.png?size=64` (`orca/src/shared/repo-icon.ts:3`, `56`).
- **Interaction:** click or Enter toggles collapse; drag reorders in manual order
  (`SectionHeader.tsx:275-296`).
- **Hover actions** (`SectionHeader.tsx:343-400`): collapse chevron, **⋯ Project actions**,
  **+ Create workspace**. On hover-capable devices they are absolutely positioned and fade
  in; on touch devices they stay in the normal flow (`ProjectHeaderActions.tsx:9-17`).
- **⋯ menu** (`worktree-list/rows/repo-header-project-actions.tsx:57`, `120-172`):
  1. Project Settings
  2. Change Project Icon
  3. Show hidden worktrees / Hide non-Orca worktrees
  4. New group from project
  5. Move to group ›
  6. Remove from group
  7. — separator —
  8. **Remove Project** (destructive)

  These are exactly the items the user saw on right-click. However, the header row has no
  `onContextMenu` (`SectionHeader.tsx`), and the only other code touching
  `data-repo-header-id` is `project-header-drop.ts:136`. **The right-click trigger was not
  located in this checkout**; the menu contents are sourced from the ⋯ dropdown.

### 2.6 Worktree context menu

Right-clicking anywhere on a card opens it (`onContextMenuCapture`,
`WorktreeContextMenuView.tsx:110-128`). Alt + right-click also reveals a developer
submenu (`123`, `302-305`). Items from `WorktreeContextMenuView.tsx:157-388`:

| Item                                                           | What it does                                                                                                                                                                                                                                    | State it needs                   |
| -------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------- |
| "Workspace" label                                              | Section header                                                                                                                                                                                                                                  | —                                |
| **Update** (pencil)                                            | Opens the edit-meta dialog: display name, linked issue (GitHub/Linear/Jira), linked PR/MR, comment (`use-worktree-context-menu-commands.ts:126-137`; `WorktreeMetaDialog.tsx:352-387`)                                                          | Host-persisted worktree metadata |
| Move to Status ›                                               | Radio submenu of workspace statuses (default Todo / In progress / In review / Done); writes `workspaceStatus` or syncs the board (`WorktreeStatusMenuItems.tsx:14-45`; `orca/src/shared/workspace-status-defaults.ts:10-20`; commands `98-124`) | Kanban status per worktree       |
| Open in ›                                                      | Configurable apps, SSH-aware, plus "Customize apps…" (`WorktreeOpenInMenu.tsx:359-377`)                                                                                                                                                         | App list                         |
| Copy Path, Copy Worktree Name                                  | Clipboard (commands `42-47`). The user's build shows no Copy Worktree Name; HEAD has it (`189`).                                                                                                                                                | —                                |
| Pin / Unpin                                                    | `setWorktreesPinnedAndReveal` (commands `55-57`)                                                                                                                                                                                                | Host metadata                    |
| Mark Read / Unread                                             | `updateWorktreeMeta({ isUnread })` (commands `48-54`)                                                                                                                                                                                           | Host metadata                    |
| New group from project; Move to group ›; Remove from group     | User-named project groups (commands `58-97`)                                                                                                                                                                                                    | Project-group store              |
| Set / Change Parent Worktree…; Open Parent; Remove from Parent | Worktree lineage (`worktree-context-menu-policy.ts:104-121`)                                                                                                                                                                                    | Parent link                      |
| Sleep (+ Sleep subtree)                                        | Closes the worktree's terminals and panes to free memory, deactivating the worktree first (`sleep-worktree-flow.ts:12-24`; commands `138-150`)                                                                                                  | Pane lifecycle                   |
| Delete (⌘⇧⌫)                                                   | Deferred delete intent. Primary rows instead show a disabled "Delete Worktree" (with tooltip) and "Remove Project from Orca"; worktrees with children show "Delete with Descendants…" (`320-388`)                                               | —                                |

### 2.7 Grouping, sorting, filters, hidden rows, empty state

- **Options menu** (`sidebar-workspace-option-items.ts`):
  - group by None / Status / PR / Project (`8-33`);
  - card layout Detailed / Compact (`35-48`);
  - agent activity Compact / Full (`50-66`);
  - sort Name / Smart / Recent / Project / Manual (`218-266`);
  - project order Manual / Recent (`269-290`).

  The saved default sort is `recent` (`orca/src/shared/constants.ts:256`); a stored `recent` is migrated
  once to `smart` (`orca/src/main/persistence/loading-store/normalize-loaded-ui-state.ts:33`).
  Smart sort ranks by attention class, blocked and waiting first
  (`smart-attention.ts:32`, `127`).

- **Filters:** hide sleeping, hide CLI-created, per-project filter (`SidebarFilter.tsx:219-310`).
- **Hidden discovered worktrees:** "Hiding N discovered worktree(s)". The chevron expands
  the list; the × keeps them hidden, and they can be recovered from the project menu
  (`ImportedWorktreesVisibilityLine.tsx:104-113`, `145-186`).
- **Empty state:** "No workspaces found" with Clear Filters
  (`worktree-list/listing/EmptyState.tsx:20-33`).
- **Host sections** have a Host actions menu (`HostSectionHeaderMenu.tsx:205-261`):
  Rename…, Connect/Disconnect, Check connection, Manage host….

### 2.8 U3 element → Orca source → data

| U3 element                                         | Source                                                                                                        | Data                                                                                                                                                                                                                                                 |
| -------------------------------------------------- | ------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Search / Tasks / Automations / Orca Mobile · New   | `SidebarNav.tsx:100-248`; `SidebarTaskNavButton.tsx:183`                                                      | Settings flags                                                                                                                                                                                                                                       |
| Projects + bell, sliders, folder-plus, +           | `SidebarHeader.tsx:49-128`; `sidebar-header-actions.tsx:17-101`                                               | `sidebarBody`, options                                                                                                                                                                                                                               |
| Project header (folder icon or owner avatar, bold) | `worktree-list/rows/SectionHeader.tsx:317-334`; `orca/src/renderer/src/components/repo/repo-icon.tsx:134-177` | `repo.repoIcon` (auto-detected avatar or favicon, or user-chosen)                                                                                                                                                                                    |
| Green / grey dot                                   | `StatusIndicator.tsx:85-95`                                                                                   | Live terminal and agent activity                                                                                                                                                                                                                     |
| Orange bell + bold name                            | `WorktreeCardStatusSlot.tsx:232`; `WorktreeTitleInlineRename.tsx:339-341`                                     | `worktree.isUnread`, set on background terminal activity (`orca/src/renderer/src/store/slices/worktrees/session/worktree-unread-activity.ts:21-26`; `orca/src/renderer/src/components/terminal-pane/pty-connection/pane-pty-visibility-bind.ts:231`) |
| Server-stack glyph                                 | `worktree-card-header.tsx:106-128`; `WorktreeCardSshHostControl.tsx:139-155`                                  | Repo host (SSH or Orca server)                                                                                                                                                                                                                       |
| "primary" chip                                     | `worktree-card-header.tsx:215-233`                                                                            | `worktree.isMainWorktree`                                                                                                                                                                                                                            |
| Muted branch line                                  | `worktree-card-meta-row.tsx:73-79`                                                                            | `worktree.branch`                                                                                                                                                                                                                                    |
| Right icon: PR state, or plug                      | `WorktreeCardMetaBadges.tsx:152`; `worktree-review-helpers.tsx:65-79`; `WorktreeCardPorts.tsx:29-52`          | Linked or hosted PR with check status; forwarded ports                                                                                                                                                                                               |
| Session rows (state, provider, title, model, age)  | `worktree-card-compact-agent-row.tsx:217-290`                                                                 | Agent status entries (hooks), model, times                                                                                                                                                                                                           |
| Chevron + nested rows on a guide line              | `WorktreeCardAgents.tsx:300-355`; `main.css:1545-1553`                                                        | Agent lineage (orchestration workers, subagents)                                                                                                                                                                                                     |
| Filled card / bordered card                        | `main.css:1281-1307`; `worktree-card-surface.tsx:59-63`                                                       | Active / multi-select                                                                                                                                                                                                                                |
| "Hiding 2 discovered worktrees ×"                  | `ImportedWorktreesVisibilityLine.tsx:104-186`                                                                 | External-worktree visibility                                                                                                                                                                                                                         |
| Footer icons                                       | `SidebarToolbar.tsx:74-100`                                                                                   | —                                                                                                                                                                                                                                                    |

Orca's worktree cards show **no diff stats**; no insertion or deletion field appears in
any card component.

---

## 3. BiBCode's current rows

Bare file names in sections 3 and 4 live under `apps/web/src/components/` (for example
`Sidebar.tsx`, `sidebar/EnvironmentRail.tsx`, `ui/sidebar.tsx`) unless a longer path is
given.

### 3.1 Layout from top to bottom (U1)

- **Rail (52 px)** (`sidebar/EnvironmentRail.tsx:212-326`), top to bottom:
  - a topbar spacer (the fixed sidebar toggle sits over it);
  - **Local** (Monitor icon + status dot), then a divider;
  - one remote entry per server: a 26 px letter avatar in a 36 px button, with a
    status dot;
  - at the bottom, **Add server** (+) and **Manage** (sliders).
- **Panel** (`Sidebar.tsx:4899-4963`), top to bottom:
  - the brand row "BiBCode" (`3744-3796`);
  - the **environment context card**, remote environments only (`4905`;
    `sidebar/EnvironmentContextCard.tsx`);
  - **Search** with ⌘K (`3956-3978`);
  - **Agents** with an unread count (`sidebar/AgentsNavRow.tsx:48-66`);
  - the **PROJECTS** header, uppercase with `tracking-wider`, with Sidebar options
    (ArrowUpDown) and Add project (FolderPlus) (`4004-4037`);
  - the project list (`4039-4116`);
  - availability states (`4118-4127`);
  - a separator and the **Settings** footer (`3798-3826`).
- **Measured in U1 (CSS px).**

  | Distance                                                                  | CSS px |
  | ------------------------------------------------------------------------- | ------ |
  | Search → Agents                                                           | 36.8   |
  | Collapsed project header pitch (`h-7` + `gap-1`)                          | 32     |
  | Header → "Hiding…"                                                        | 25.5   |
  | "Hiding…" → primary row                                                   | 28.5   |
  | Primary row → "No threads yet"                                            | 25.7   |
  | Nested row text left edge, from the window edge (~43 from the panel edge) | ~95    |
  | Right inset of the "primary" chip                                         | ~20    |

### 3.2 Project header (`Sidebar.tsx:3124-3282`)

- **Row:** `SidebarMenuButton size="sm"`, i.e. `h-7` (`ui/sidebar.tsx:820`), with
  `gap-2 px-2 py-1.5`. Its right padding reserves room for the hover strip: `pr-14`, or
  `pr-20` for remote-only projects (`Sidebar.tsx:3134-3136`).
- **Leading chevron** (`size-3.5`, rotates when open). When the project is collapsed and a
  thread has a status, a 9 px status dot replaces the chevron and swaps back to it on
  hover (`3144-3171`).
- **`ProjectFavicon`** (`size-3.5`). It is a favicon file found in the repository by the
  server, otherwise a muted folder icon (`ProjectFavicon.tsx:8-28`;
  `apps/server/src/project/mod.rs:5-19`, `40-45`).
- **Name** `text-[13px] font-medium text-foreground/90`, plus "N projects" when projects
  are grouped (`3173-3182`).
- **Cloud / container icon** for remote-only projects. It fades out on hover
  (`3187-3213`) and appears on **every** project header in U1, because the rail already
  scopes the list to Ai-server.
- **Hover strip** (`3214-3281`):
  - an **invisible** "New main-branch chat" button that still takes up space (`3222`;
    deliberate, per `docs/superpowers/specs/2026-08-02-project-toolbar-actions-design.md`);
  - New worktree (FolderGit2);
  - Git Manager (GitBranch);
  - Pull Requests (GitPullRequest), when enabled.
- **Right-click menu** (`2261-2283`), with no visible ⋯ button:
  1. Rename
  2. Group into…
  3. Copy Path
  4. Show hidden worktrees / Hide discovered worktrees
  5. Archived threads
  6. Remove (destructive)

### 3.3 Primary row (`Sidebar.tsx:1306-1381`)

- **Row:** `SidebarMenuSubButton size="sm"` with `resolveThreadRowClassName`: `h-6
  sm:h-7`, `text-[13px]`, `text-foreground/80`. Active rows get `bg-accent/85
  font-medium`; selected rows get `bg-primary/15–22` (`Sidebar.logic.ts:414-443`).
- **Content:** a 1.5-size status dot, the unread dot, the pin, the title (the checkout's
  live branch from the VCS summary), and a "primary" chip (`text-xs`, bordered,
  `bg-muted`) (`Sidebar.tsx:1356-1377`).
- **Missing compared with thread rows:** the status dot is there (`Sidebar.tsx:1357-1363`),
  but there is no status label, no agent sub-row, no time and no PR icon.
- **Empty state below it:** each expanded project then shows **"No threads yet"**
  whenever there are no workspace threads (`1430-1439`, `1884`). This happens even though
  the primary row is itself backed by the default chat thread (`Sidebar.logic.ts:643-651`).

### 3.4 Workspace (thread) row (`Sidebar.tsx:560-1249`)

- **Line (24/28 px), left side** (`964-1026`):
  - unread dot (sky);
  - pin icon;
  - **PR icon** (a button that opens the PR; its color is the state: open, merged,
    closed — `979-995`; `ThreadStatusIndicators.tsx:37-67`);
  - status label (`ThreadStatusLabel`: a dot plus "Working" / "Pending Approval" / …,
    shown only at `md`+, at **`text-[10px]`**, `ThreadStatusIndicators.tsx:151-197`);
  - **title at `text-xs` (12 px)** (`Sidebar.tsx:1013`);
  - the worktree/branch label at **`text-[10px]` mono, `text-muted-foreground/70`**,
    capped at `max-w-24` (`ThreadStatusIndicators.tsx:117-146`).
- **Line, right side** (`1027-1167`):
  - discovered-port Globe button (opens a preview);
  - terminal-running icon (pulses);
  - a cloud icon for remote threads (`1122-1136`);
  - a ⌘-jump label while the modifier is held;
  - otherwise the relative time, `formatRelativeTimeLabel` → "5m ago", at
    `text-muted-foreground/40` (`1152-1162`; `apps/web/src/timestampFormat.ts:90-106`).

  On hover, the time is swapped for Archive (with an optional inline Confirm).

- **Extra sub-lines** (`1169-1246`):
  - worktree availability warning;
  - "Agent – Delivery failed/uncertain";
  - "Claude Code – Running · 1h" while a session is starting, running or errored.
- **Data queried per row:**
  - the VCS summary, or full status for the active row (`654-666`);
  - the worktree catalog, for worktree rows (`640-647`);
  - running terminals and discovered ports;
  - pin, unread and last-visited from local stores.
- **Unread** is detected client-side when a turn settles while the thread is not open
  (`sidebar/useAgentsUnread.ts:9-44`). Pin and unread are saved per device in
  `bibcode:sidebar-workspace-meta:v1` (`apps/web/src/sidebarWorkspaceMetaStore.ts:15`).

### 3.5 Other panel elements

- **Discovery.** The collapsed "Hiding N discovered worktree(s)" line is a ghost button
  with EyeOff and a chevron (`WorktreeDiscoverySection.tsx:502-514`). It has no dismiss ×.
  The expanded discovery UI uses 8–11 px text (see 3.8).
- **Show more / Show less:** `h-6` rows at `text-xs` (`Sidebar.tsx:1474-1506`), governed
  by `sidebarThreadPreviewCount` (default 6, `packages/contracts/src/settings.ts:60`).
- **Sidebar options** (`Sidebar.tsx:3558-3705`): Sort projects (Last user message /
  Created at / Manual), Sort threads, Visible threads (1–15), Group projects (by
  repository / by repository path / keep separate).
- **Environment context card** (`EnvironmentContextCard.tsx:56-115`):
  - `mx-2 rounded-[10px] border bg-background px-2.5 py-2`;
  - name at 13 px semibold;
  - second line at **`text-[11px]`**: status · "BiBCode v…" · compatibility badge at
    **`text-[10px]`** · `ServerUpdateBadge` ("Up to date" / "Status unavailable";
    `ServerUpdateBadge.tsx:31-62`);
  - ⋯ trigger, `size-6`, opening **Disconnect** (listed first), **Check for updates**
    (only with remote update control), **Manage…** (U2). Orca's host menu for comparison:
    Rename…, Connect/Disconnect, Check connection, a separator, Manage host….
- **Agents row:** always shows the count, **including "0"** (`AgentsNavRow.tsx:61-63`).

### 3.6 Context menus

All menus go through `api.contextMenu.show`. On desktop the menu is native: headers and
icons are dropped, and the only separator is one inserted automatically before the first
destructive item (`packages/contracts/src/ipc.ts:115-125`;
`apps/desktop/src-tauri/src/context_menu.rs:196-202`).

- **Workspace row** (`Sidebar.tsx:2788-2813`):
  1. Update (= **git pull**, `1593`)
  2. Open in › (File Explorer on the local desktop, plus detected editors)
  3. Rename thread
  4. Mark read / unread
  5. Pin / Unpin
  6. Copy Path
  7. Copy Thread ID
  8. Delete Worktree / Delete
- **Multi-select** (`2433-2437`): Mark unread (N), Delete (N).
- **Primary row** (`3020-3036`): Update, Open in ›, Copy Path, Mark read / unread,
  Pin / Unpin. It has **no remove-project item**, which contradicts
  `docs/user/workspace-ui.md:96-97`.
- **Project header:** see 3.2.

### 3.7 Side by side

| Aspect           | Orca (U3, default config)                                                           | BiBCode (U1)                                                                                         |
| ---------------- | ----------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------- |
| Panel width      | 262 in U3; default 280, range 220–500                                               | 320 default (268 panel), 260 min, no max; U1 422 (370 panel)                                         |
| Host scoping     | Inline host sections, per-card `Server` glyph, host badge when mixed                | Environment rail plus context card; a cloud icon on every remote project and thread row (redundant)  |
| Item shape       | 2–3 line card, `rounded-lg`, ~53 px (2 lines) / ~77 px (3 lines)                    | Single 24–28 px line plus occasional sub-lines                                                       |
| Status           | Left status column: spinner / question / emerald or grey dot; amber bell for unread | Inline 6 px dot, or dot + 10 px label; sky unread dot                                                |
| Title            | 13 px, semibold when unread                                                         | Primary: 13 px branch. Thread: 12 px title (UI.md says 13 px)                                        |
| Branch           | Own 11 px muted line                                                                | Inline 10 px mono label (`max-w-24`) on thread rows; the title _is_ the branch on the primary row    |
| PR               | Right of the branch line, check-colored                                             | Before the title, state-colored, no check status                                                     |
| Agent sessions   | One row each: state, provider icon, title, model, age; nested children              | One status sub-line while running ("Claude Code – Running · 1h"); no preview, model or provider icon |
| Time             | On session rows ("6m")                                                              | On the thread line ("5m ago", 40% muted), hidden on hover                                            |
| Hover / selected | 4% wash / filled card with faint border                                             | `bg-accent` / `bg-accent/85` + medium weight; selection `bg-primary/15–22`                           |
| Project header   | Icon or avatar, 13 px semibold, hover chevron · ⋯ · +                               | Chevron, favicon, 13 px medium, hover [hidden] · worktree · Git Manager · PRs; right-click-only menu |
| Menus            | In-app dropdown with sections, icons, submenus, shortcut hint                       | Native menu, flat, one automatic separator                                                           |
| Empty            | Primary card alone; "No workspaces found" only for filters                          | "No threads yet" under each primary row                                                              |

### 3.8 UI.md violations in the current sidebar

UI.md (`UI.md:171-176`) sets a 12 px floor, 13 px titles, solid `text-muted-foreground`,
no letter-spacing, and notes that the sidebar has already been swept. The current code
still has:

- **Thread titles are `text-xs`.** `Sidebar.tsx:1013` renders thread titles at 12 px, but
  UI.md:172 says thread titles are 13px regular.
- **Status label.** `text-[10px]` at `ThreadStatusIndicators.tsx:186`.
- **Worktree label.** `text-[10px]` and `text-muted-foreground/70` at
  `ThreadStatusIndicators.tsx:140`. It was added on 2026-09-17, after the floor rule
  (commit `ec4a6b76`).
- **Relative time.** `text-muted-foreground/40` at `Sidebar.tsx:1156`.
- **Environment context card.** `text-[11px]` at `EnvironmentContextCard.tsx:66` and
  `text-[10px]` at `EnvironmentContextCard.tsx:74`.
- **Update status badge.** The "Status unavailable" variant uses
  `text-muted-foreground/70` (`ServerUpdateBadge.tsx:46`).
- **Rail avatar.** `text-[10px]` plus `tracking-wide` at `EnvironmentRail.tsx:85`.
- **Worktree discovery UI.** `WorktreeDiscoverySection.tsx:124`, `155`, `159`, `182`,
  `200`, `431`, `436`, `454`, `473`, `483` and `532` use 8, 9 and 11 px. The collapsed
  line (`505`, `text-[9px]`) is overridden by the Button's `sm:text-xs` only at 640 px
  and wider (`ui/button.tsx:31`).
- **Settings footer.** It is a navigation row rendered at `text-xs` muted
  (`Sidebar.tsx:3820`); UI.md:172 wants 13 px medium.
- **PROJECTS label.** Uppercase `tracking-wider` (`Sidebar.tsx:4006`). This is minor, but
  UI.md warns about letter-spacing under hinting.

---

## 4. Recommendation

### 4.1 Direction

Keep BiBCode's structure: the environment rail, the context card, the project tree with a
leading chevron, the hover action strip, and native menus. Change what an item shows:
turn the one-line thread/primary rows into **workspace cards** stacked like Orca's. Stay
inside UI.md:

- all text 12 px or larger; titles 13 px;
- solid `text-muted-foreground` for secondary text; alpha only on icon tints;
- no letter-spacing;
- status carried by **glyph shape**, with color only helping to tell items apart
  (UI.md:208-210);
- orange only for panel edges and focus.

Where Orca uses 10–11 px, use `text-xs`. The 370 px panel is what makes room for 12 px
text on a second and third line without cutting titles short.

### 4.2 Proposed card anatomy (CSS px)

| Part             | Content                                                                                                                                                                                                                                                                                | Size / type                                                                                                                    |
| ---------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------ |
| Surface          | Whole card clickable; `rounded-lg`, `px-2 py-1.5`, `gap-1` between lines; 2 px between cards; 8 px between projects                                                                                                                                                                    | —                                                                                                                              |
| Status column    | Fixed 16 px column. Working/connecting: spinner. Pending approval: shield or hand glyph. Awaiting input: question glyph. Plan ready: list-check. Unseen completion: filled dot. Idle: hollow dot. Failed: alert. Priority from `resolveThreadStatusPill` (`Sidebar.logic.ts:445-510`). | `size-4` slot                                                                                                                  |
| Line 1           | Title (thread title; the live branch for the primary row), "primary" chip, pin; right side: hover actions (archive / confirm) and ⌘ jump label                                                                                                                                         | 13 px regular `text-foreground/80`; **semibold `text-foreground` when unread** (as in `AgentsRow.tsx:111-126`); chip `text-xs` |
| Line 2           | Branch — hidden when it equals the title, with the full value in the tooltip (Orca compact rule, `worktree-card-presentation.tsx:82-86`); right side: PR icon (button), dirty dot (`hasWorkingTreeChanges`), terminal-running, ports                                                   | 12 px `text-muted-foreground`, sans                                                                                            |
| Line 3 (session) | Provider icon (`ProviderInstanceIcon`), preview (tool while working, otherwise latest assistant message, otherwise prompt — `resolveAgentPreviewLine`), model (mono), age ("5m")                                                                                                       | 24 px tall (click target); 12 px muted; model `max-w-28`                                                                       |
| Nested sessions  | Panel threads of the same worktree, indented under a 1 px `border` guide line, with a disclosure chevron and "+N" when collapsed                                                                                                                                                       | 24 px rows                                                                                                                     |
| Hover            | `bg-accent/60` wash, no border                                                                                                                                                                                                                                                         | —                                                                                                                              |
| Active           | Filled `bg-accent` card with a subtle `border`. Weight stays reserved for unread, so active loses today's `font-medium`.                                                                                                                                                               | —                                                                                                                              |
| Keyboard focus   | `ring-2 ring-ring` (orange)                                                                                                                                                                                                                                                            | —                                                                                                                              |

Resulting heights: about 52 px for a 2-line card and 80 px for a 3-line card (Orca: ~53 /
~77). Drop the per-row cloud icon inside the rail-scoped list. The context card already
names the environment, and the Agents view keeps showing environment badges.

### 4.3 Concrete component changes and the data each needs

| #   | Change                                                                                                                                                                             | Files                                                                                                                                | Data needed                                                                                                                                                                                                          | Client or server?                                                                                                                                                                                                                        |
| --- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | First-launch width 422, clamped to `innerWidth − 640` (min 260); update test and docs                                                                                              | `AppSidebarLayout.tsx`, `ui/sidebar.tsx`, `AppSidebarLayout.test.tsx`, `docs/user/workspace-ui.md`                                   | Viewport width                                                                                                                                                                                                       | Client                                                                                                                                                                                                                                   |
| 2   | `SidebarWorkspaceCard` replacing the visuals of `SidebarThreadRow` and `SidebarPrimaryRow`: lines 1–2, status column, states                                                       | `Sidebar.tsx`, `Sidebar.logic.ts`, a new `sidebar/WorkspaceCard*.tsx`                                                                | Thread shell: `title`, `branch`, `worktreePath`, `kind`, pending flags, `latestTurn`; VCS summary: `refName`, `hasWorkingTreeChanges`, `pr` (`packages/contracts/src/vcs.ts:62-112`)                                 | **Client** — already queried per row (`Sidebar.tsx:654-666`)                                                                                                                                                                             |
| 3   | Session line (provider icon, preview, model, age); absorbs the "Running · 1h" and "Delivery failed" sub-lines                                                                      | Same, reusing `resolveAgentProvider` / `resolveAgentPreviewLine` (`sidebar/agentsSection.logic.ts:67-91`) and `ProviderInstanceIcon` | `session.status`, `providerName`, `updatedAt`; `modelSelection.model`; `conversationPreview { prompt, tool, assistantMessage }`; `unresolvedDelivery` (`packages/contracts/src/orchestration.ts:362-377`, `503-550`) | **Client**. Relative ages need one app-level minute clock, not a timer per row.                                                                                                                                                          |
| 4   | Nested session rows for a worktree's panel threads                                                                                                                                 | `Sidebar.tsx`, `Sidebar.logic.ts` (`splitPrimaryAndWorkspaceThreads` currently drops panels, `643-651`)                              | The link from panel thread to host thread                                                                                                                                                                            | **Server + contract.** `OrchestrationThreadShell` has no `hostThreadId` (only `WorktreeCreatePanelInput` has it, `packages/contracts/src/rpc.ts:313-319`). Grouping by `worktreePath` alone is ambiguous for threads without a worktree. |
| 5   | PR icon colored by CI check state                                                                                                                                                  | `ThreadStatusIndicators.tsx`                                                                                                         | Check rollup per PR                                                                                                                                                                                                  | **Server.** Not in the VCS summary or `ChangeRequest` (`packages/contracts/src/sourceControl.ts:24-36`); exists only per PR in `pullRequests.getChecks` (`packages/contracts/src/rpc.ts:441`).                                           |
| 6   | Diff stats per card (optional; Orca doesn't show them)                                                                                                                             | —                                                                                                                                    | Insertions / deletions                                                                                                                                                                                               | Only for the active row (full status, `packages/contracts/src/git.ts:240-282`); for all rows it needs a summary extension (**server**)                                                                                                   |
| 7   | Remove "No threads yet" when the primary row exists; offer "New worktree" as a text action instead                                                                                 | `Sidebar.tsx:1430-1439`, `1884`                                                                                                      | —                                                                                                                                                                                                                    | Client                                                                                                                                                                                                                                   |
| 8   | Project header: a visible **⋯** (same items as right-click) plus **+ New worktree**; keep Git Manager / PR icons; remove the invisible placeholder; sentence-case "Projects" label | `Sidebar.tsx:3124-3282`, `4004-4037`                                                                                                 | —                                                                                                                                                                                                                    | Client                                                                                                                                                                                                                                   |
| 9   | Project icon: keep the local favicon. Owner avatars need an outbound fetch to the forge (Orca: `github.com/<owner>.png`, `orca/src/shared/repo-icon.ts:56`)                        | `ProjectFavicon.tsx`, server asset resolver                                                                                          | Remote owner, image                                                                                                                                                                                                  | **Server + privacy decision** (the Git Manager is zero-telemetry)                                                                                                                                                                        |
| 10  | Typography sweep (3.8), plus hiding the "0" Agents badge                                                                                                                           | Files listed in 3.8; `AgentsNavRow.tsx`                                                                                              | —                                                                                                                                                                                                                    | Client                                                                                                                                                                                                                                   |
| 11  | Context card: 12 px text; menu order Check for updates, Manage…, then — Disconnect; clearer "update status unknown" copy                                                           | `EnvironmentContextCard.tsx`, `ServerUpdateBadge.tsx`                                                                                | —                                                                                                                                                                                                                    | Client                                                                                                                                                                                                                                   |

### 4.4 Context menu changes

- **Rename BiBCode's "Update" to "Pull".** It runs `vcsEnvironment.pull`
  (`Sidebar.tsx:1593`), while Orca's "Update" edits workspace metadata (UI.md: specific
  verb labels). Do not reuse "Update" for anything else.
- **Add separators.** The ContextMenuItem contract gets a separator item, rendered by both
  the native Tauri builder and the web fallback (`packages/contracts/src/ipc.ts:115-125`;
  `apps/desktop/src-tauri/src/context_menu.rs:196-202`; `apps/web/src/contextMenuFallback.ts`). The menu can then
  follow Orca's grouping: Open / Copy — Pin / Unread — Rename — Delete. This is a
  cross-package change (contracts, desktop, web).
- **Cheap additions:** "Copy Branch Name" on workspace and primary rows. Keep "Copy
  Thread ID".
- **Gaps that need product decisions and saved data (server)** — each is new persisted,
  per-workspace or per-project state, so do not copy them by default:
  - Move to Status (kanban columns);
  - project groups (new / move / remove);
  - Set Parent Worktree (lineage);
  - Sleep (stop the worktree's agent sessions and terminals in one step);
  - Change Project Icon;
  - Orca's "Update" dialog (linked issue, linked PR, notes).
- **Pin and unread are local to each device** in BiBCode, but saved on the host in Orca.
  Syncing them across devices would be server work.
- **Keep native menus on desktop** (platform convention). Orca's in-app dropdown with
  icons and a "Workspace" header is not needed to reach the same grouping.

### 4.5 What to keep

- The environment rail and the context card (spec decision D5,
  `docs/plans/remote-servers/remote-servers-spec.md:52`, `379-403`), with rail selection
  scoping only what is shown.
- Project selection and highlighting (`docs/user/workspace-ui.md:92-94`).
- The Git Manager and Pull Requests entry points.
- Discovery semantics, pinned-first ordering, ⌘1… jump hints, inline rename and archive
  confirm.
- The `data-text-surface` wrapper on the sidebar content (`ui/sidebar.tsx:720`).

### 4.6 What not to copy

- Orca's sub-12 px sizes and alpha-reduced muted text.
- Orca's color-only green versus grey dots.
- The per-card host glyph inside a rail-scoped list.
- Orca's full Detailed/Compact and properties menu: UI.md advises against settings the
  team could decide itself. Pick one card layout.

---

## Open questions

1. Should existing profiles get the new 422 px default (bump the storage key to `v3` and
   retire `v2`, like `d46f46fa`), or should only first launches change, as asked?
2. Small windows: is the recommended first-launch clamp (320 at a 960 px window, 422 from
   1062 px) acceptable? Or should a 538 px main area at the minimum window be accepted?
3. Project names: UI.md sets project titles at 13 px **medium**, while Orca uses
   semibold ("bold"). Keep medium, or amend UI.md?
4. Taller cards fit about half as many workspaces per screen. Should the default
   "Visible threads" (6) drop, or should inactive projects start collapsed?
5. Nested panel sessions: add `hostThreadId` to the thread shell (server), or show panel
   threads only as a "+N chats" count until then?
6. Owner avatars and CI check colors both need outbound or forge calls. Are they wanted,
   given the zero-telemetry stance?

## Documentation discrepancies found

- `docs/user/workspace-ui.md:96-97` says workspace row menus include "remove project for
  primary rows". The primary-row menu (`Sidebar.tsx:3020-3036`) has no such item; the code
  comment at `Sidebar.tsx:2988-2990` places removal on the project header menu
  ("Remove", `Sidebar.tsx:2275`). Align the doc when the menus change.
- UI.md:176 describes the sidebar as swept to the typography rules, but the items in 3.8
  remain.
