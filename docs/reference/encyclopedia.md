# Encyclopedia

This is a living glossary for BiBCode. It explains common terms used in the
codebase and UI.

## Project And Workspace

### Project

The top-level workspace record in an environment. A project points at a
workspace root and owns the visible primary and worktree cards in the left panel.

### Workspace Root

The filesystem path for a project checkout. Git, file, terminal, and provider
operations run relative to this root unless a thread has a worktree path.

### Primary Workspace Card

The left-panel card for a project's live checkout. It is backed by the project's
default thread, is titled with the live checkout branch, and cannot be deleted
as a normal thread.

### Default Thread

The undeletable thread that backs a project's primary card. Removing it is modeled
as removing the project, not deleting a thread.

### Worktree

A Git worktree used as an isolated workspace. Worktree threads have
`worktreePath` set and run chats, terminals, filesystem, and source-control
operations in that path.

### Workspace Thread

A normal visible thread for a project primary checkout or worktree. It owns
conversation history, provider session state, activities, checkpoints, and
workspace metadata.

### Panel Thread

A hidden sibling thread with `kind: "panel"`. Panel threads share the host
thread's project, branch, and worktree but own an isolated provider session and
transcript. They appear as center-panel tabs, not left-panel cards; the host's card counts them as **N more chats**.

## UI Surfaces

### Left Panel

The navigator for Search, the cross-environment Agents nav row, and
environment-scoped projects with their workspace cards. The Agents row's unread
badge aggregates across environments and is hidden when nothing is unread;
selecting the row opens the full-screen Agents view. The panel also shows
project groups, the primary card and worktree cards, pin/unread state, and
grouped context menus.

### Workspace Card

One checkout in the left panel: the primary card for the live checkout or a
worktree card. The shape of its status glyph carries the thread's state; its
second line shows the branch, the pull or merge request, uncommitted changes
and a running terminal; its third line shows the provider, what the agent is
doing, the model and how long ago.

### Agents View

The full-takeover cross-environment thread navigator opened from the Agents nav
row. Its top strip has a back arrow and **N unread** badge. Its list column has
a filter, grouping by Status, Project, or Environment, an unread-only bell, and
a **Mark all read** menu. Status groups show counts, with Done collapsed by
default. Rows show status, title, project · branch, conversation preview,
environment, and relative time; non-live environments are greyed, and rows
remain bold and unread until visited. Selecting a row marks it read and shows
its live session in the right pane while the list remains visible.

The view and the nav row's unread badge are the sanctioned exception to
environment-rail scoping. The per-row **Jump to workspace** action exits the
view and re-points rail selection to the row's environment.

### Center Panel

The main chat and terminal workspace. Center surfaces live in tab groups, with
up to four groups arranged as resizable horizontal or vertical split panes.
Each group has its own active tab; the focused group receives newly created AI
chat and terminal panels. Layout, focus, tab order, and split ratios persist
across reloads.

### Center Surface

A chat or terminal tab inside one center tab group. The host chat represents the
selected workspace thread. Extra chat surfaces use panel threads; closing one
deletes that panel thread. Closing a split pane instead merges its surfaces into
an adjacent group without closing them.

### Right Panel

The tool surface area for the active thread. Its supported surface kinds are
Plan, Diff, Source Control, Files, an individual file, Preview, Terminal,
Activity, and a pull or merge request. Singleton tools and resource-backed
file/browser/terminal/request tabs share one ordered, persisted surface rail.

### Source Control

The right-panel Git UI for the active project/worktree. It groups files by
staged, unstaged, and untracked state; exposes stage/unstage/discard/delete
actions; provides commit history and AI commit messages; and drives commit,
pull, push, and PR actions. Its publish control is currently disabled; GitHub
publishing is available from the chat-header Git actions control.

### Files Manager

The right-panel filesystem UI for the active project/worktree. It supports
context menus for files, folders, and background space; create, rename, delete,
duplicate, copy path, add folder as project, external editor, preview, and
explicit Ctrl/Cmd+S saves. Each directory is one expandable row, entries move by
dragging them onto a folder row or the tree root, and Refresh rescans the
workspace on the server so externally created files appear. Expansion state
survives refreshes and mutations.

### Custom Action

A project script/action exposed through the chat header `+` menu and script
commands. Script keybindings use the `script.{id}.run` command shape.

## Orchestration

### Command

A typed request to change domain state, such as creating a project, creating a
thread, starting a turn, or deleting a panel thread.

### Domain Event

A persisted fact that something happened. The server projects domain events
into read models and pushes user-visible updates to clients.

### Projection

A read-optimized view derived from events. Browser clients consume projections
through the WebSocket transport and typed contracts.

### Receipt

A lightweight runtime signal emitted when async work reaches a stable milestone,
such as checkpoint capture, diff finalization, or turn quiescence.

### Quiesced

A turn has gone quiet and stable: provider work and follow-up processing have
settled far enough for tests and orchestration to continue deterministically.

## Provider Runtime

### Provider Driver

The implementation that probes, launches, and translates one backend agent
protocol. BiBCode supports four built-in drivers: Codex, Claude, Cursor, and
OpenCode.

### Provider Instance

A configured provider entry with its own display name, settings, credentials,
home path, environment variables, and model availability. An instance has a
user-facing routing ID and references one provider driver; multiple instances
may use the same driver.

### Session

The live provider-backed runtime attached to a thread. Workspace threads and
panel threads each own their own session.

### Runtime Mode

The safety/access mode for a session. The exact persisted values and UI labels
are `approval-required` (Supervised), `auto-accept-edits` (Auto-accept edits),
and `full-access` (Full access).

### Interaction Mode

The agent interaction style for a session, such as default or plan mode.

### Environment

A local or remote server connection and its host-scoped projects, threads,
terminals, provider runtimes, and diagnostics. Selecting a remote environment
changes the host on which those operations run; it does not merge remote and
local process or resource state.

### Activity Actor

A provider-observed participant, such as a subagent, shown in the Activity
right-panel surface when the selected provider exposes reliable activity data.

### Activity Work Item

A provider-observed background task or unit of work associated with an activity
actor and thread or terminal scope.

## Checkpointing

### Checkpoint

A saved snapshot of workspace state at a particular turn.

### Checkpoint Baseline

The starting checkpoint used to compute later diffs for a thread timeline.

### Turn Diff

The changed-file summary and patch for one turn.

## Related Docs

- [Workspace UI](../user/workspace-ui.md)
- [Repository layout](./workspace-layout.md)
- [Architecture overview](../architecture/overview.md)
- [Runtime modes](../architecture/runtime-modes.md)
