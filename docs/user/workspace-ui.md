# Workspace UI

BiBCode is split into left, center, and right work areas. The left panel
navigates agents and project/worktree threads, the center panel runs chats and
terminals, and the right panel hosts project tools.

The bottom status bar shows Claude and Codex usage for the environment selected
in the left rail. Selecting a remote server changes both the displayed account
usage and the target of refresh or usage-reset actions. A server that has not
returned usage does not borrow the local server's values.

When a request has waited more than 15 seconds for a response, the status bar
shows a warning such as **2 slow requests**. Click it to see each request's name,
the environment it was sent to, and its start time. The warning goes away when
the requests finish. The hosted web app has no status bar, so there a bar holding
only this warning appears at the bottom while requests are slow.

## Left Panel

The panel opens 422px wide by default (a 370px projects panel beside the 52px
rail); on a narrower window it opens narrower so the main area keeps its
minimum width. Drag its right edge to resize it; the width is remembered, and
double-clicking the edge restores the default for the current window size.

The rail chooses which environment the panel shows: **Local** (this machine) or
one of your saved servers. The choice stays until you pick another entry;
background updates from this machine's server never switch it back to Local. If
the selected server is removed, the rail returns to Local. Saved servers show
the name you gave them on this device (see
[Remote access](./remote-access.md#name-a-saved-server)).

The **Environments | Repositories** switch at the top of the left panel picks
how projects are listed; the choice is remembered on this device.
**Environments** is the per-environment view described above. **Repositories**
hides the environment rail and lists one read-only card per Git repository,
matched by its `origin` remote, across every connected environment. Each card
holds one entry per checkout showing the environment, its connection state and
the folder path; that entry is the project itself, with the same menu, actions,
threads and worktrees. Projects without an `origin` get a card of their own
named after their folder. Unavailable environments appear dimmed. Cards are
sorted by repository name by default, so they keep their place as chats update;
the sort menu's **Sort repositories** choice also offers **Last user message**
and **Created at**, and is separate from the Environments view's project sort. In
this view the sort menu's grouping choice is hidden, and each entry's menu is the normal
project menu; its **Group into…** item affects the Environments view.
Collapsing a repository card hides all its entries. Each entry keeps its own
collapse state, so collapsing one environment's entry leaves the others open.
Collapsing or expanding the project in Environments also updates its entries
here, except other environments' entries you have already toggled in this
view. The header's **Add project** button (also offered when the view is
empty) opens Add Project with its **Host** selector, so the target environment
is chosen in the dialog rather than on the hidden rail. A changed `origin`
moves its entry to the new card after the next refresh of that project: when
the window regains focus, after a Git action, or when you reopen the project.
An unsent message draft belongs to the repository's project rather than one
checkout: starting a new thread from another environment's entry of the same
repository continues that draft there, and its branch and worktree choice
resets.
Switching back to **Environments** restores the rail with its previous
selection.

The **Search** row is followed by an **Agents** nav row, then Projects. Its
unread-count badge covers agents across all connected environments and is hidden when nothing is unread. Selecting
the row opens the full-screen Agents view; its top strip has a back arrow for
returning to the normal workspace, the title **agents**, and an **N unread**
badge.

The view's list column provides a filter, grouping by **Status** (the default),
**Project**, or **Environment**, an unread-only bell, and a menu with **Mark all
read**. Status groups have counts and appear in fixed **Working** → **Pending
Approval** → **Awaiting Input** → **Done** order, with Done collapsed by
default. Each row opens with the project name, the provider icon, the status
dot, and the relative time, then shows the branch as its main line, the thread
title, a one-line conversation preview, and a footer with the status, the
provider, and the environment badge. While a thread is working, the preview
shows its current tool; otherwise it shows the latest assistant message or
prompt. Rows from environments that are not live are greyed and add the
environment's availability to the badge. Unread rows carry a dot and stay bold
until visited.

Selecting an agent row marks it read and shows its live session in the right
pane while keeping the list visible. The per-row **Jump to workspace** action
exits to the normal workspace view and re-points the environment rail to that
row's environment.

Projects are shown as cards lifted off a recessed list background, in both the
Environments and Repositories views; in the Repositories view the repository is
the card and each environment checkout is a shaded band inside it. A project's
chats are workspace cards drawn as rows inside the project card, without their
own outline: the open card has a fill and selected cards a tinted outline. Dark
mode separates the layers more strongly and brightens secondary text on the
cards so it stays readable. A workspace card has up to three lines:

- **Line 1:** a status glyph, the title (bold while unread), a **primary**
  chip on the main checkout, and a pin when pinned. Hovering or focusing a
  worktree card shows **Archive**; holding the thread-jump modifier shows its
  number instead.
- **Line 2:** the branch (hidden when it equals the title), the pull or merge
  request number (`#12`, or `!57` on GitLab), coloured by state and opening
  the request when clicked, a dot for uncommitted changes, a terminal icon
  while a terminal process runs, and a globe that opens a discovered local
  server.
- **Line 3:** the provider icon, what the agent is doing (its current tool
  while working, otherwise the latest reply or prompt, or **Delivery failed**
  or **Delivery uncertain** when a message didn't land), the model (its short
  name, or its identifier such as `sonnet` when the catalog gives it none), and
  how long ago that was.

The glyph's shape carries the status: a hand (needs approval), a question mark
(waiting for your answer), a spinner (working or connecting), a warning
triangle (failed), a checklist (plan ready), a filled dot (finished, not opened
yet) and a hollow ring (idle). A collapsed project and the **Show more** row
show the most urgent glyph among the cards they hide. A collapsed project keeps
only the card you are viewing visible; its other cards and discovered worktrees
appear again when you expand it.

- The primary card represents the project's live checkout. Its title is the
  checkout's current branch, refreshed from Git rather than from a stored
  thread title. When Git cannot use the checkout, the title shows the project
  name and line 2 says **Not a Git repository**, **Repository unreadable**,
  **Repository not trusted**, or **Repository unavailable**, with the full
  explanation on hover and for screen readers.
- The primary card is backed by an undeletable default thread; to remove it,
  remove the project from its header.
- Worktree cards represent worktree threads. Creating a worktree creates both
  the Git worktree and its thread before the first message.
- Other chats open in a worktree show as **N more chats** under its card;
  chats in the main checkout count on the primary card.

Tab moves between cards; **Enter** opens one, and **Shift+F10** or the
**Menu** key opens its menu at the card.

The sidebar says **No projects yet** only after every configured environment
has connected and returned a successful empty project snapshot. During startup,
reconnects, unavailable environments, storage-location changes, or recovery
conditions it shows that availability state instead. Cached project rows stay
visible during those conditions and are replaced only after a newly accepted
environment completes synchronization. When an environment cannot connect, the
notice names it ("<name> is not connected."); hover or focus that line for the
reason, which the chat's banner states in full.

In a chat whose environment is not connected, the banner above the composer
gives the state in its title and the reason once in its body, and the composer
says only that the environment is not connected. The banner offers
**Reconnect** where this client reconnects in place. For a remote environment
in the desktop app, which manages remote connections in **Settings → Remote
Servers**, the banner and the sidebar notice offer **Open Remote Servers**
instead.

Use the project `+` action to create a worktree. The Create Worktree dialog has a
permanent Name field, an optional Smart/GitHub/Branch **Create From** selector,
an agent picker, advanced options, a Create more toggle, and Ctrl+Enter submit.
Selecting any branch suggests its name and shows **Reuse branch**. It is enabled and on by default
for a free local branch; for a remote branch or a branch already checked out
elsewhere it stays visible but disabled, with a note explaining that a new
branch will be created from it through the server's safe suffixed-branch flow.
If the branch list fails to load, the dialog says so and a typed name still
creates a new branch. Edited names are preserved. Typing an exact local
or remote branch selects that ref without repeating the same value as a result
row below the input. If the chosen remote branch becomes local before submit,
the server reuses it when free and still suffixes it when another worktree owns
it.

Use Add Project to open one existing project folder, clone a Git URL, or create
a new Git repository. It starts on the environment selected in the rail. On
macOS and Linux desktop, it shows a **Host** selector when saved remote servers
are available, offering **This device** and those servers, and omits it when
this device is the only host. On Windows, the selector is labelled **Location**
and also offers the usable WSL locations. Browser clients retain connected-host
selection. Local and mapped
WSL locations use the native folder picker. Remote hosts, and browser clients
without a native dialog, open a folder browser that lists the selected host's
directories; **Type a path instead** switches to manual entry of an absolute
or home-relative path. Selecting a folder adds that folder as one project and
does not scan for nested repositories.

While host information is loading, manual entry keeps the path editable and
explains why **Open project** is unavailable. The button becomes available when
the information arrives, keeping the entered path and selected host. Nothing is
submitted automatically.

In **Clone from URL**, **Browse…** beside **Parent folder** opens the native
picker for local and mapped WSL locations, or a directory browser on the selected
server for remote hosts and browser clients. The browser names the server and
starts at the entered parent folder. Choose a folder to return to the clone form;
the repository is cloned beneath it on that same server. **Cancel** or **Back**
keeps the Git URL and parent folder as entered. Manual path entry remains available.
**Create new project** offers the same **Browse…** for its parent folder: the
native picker locally, or the selected server's directory browser, returning to
the form with the project name kept.
If browsing fails or the connection drops, use **Refresh** in the browser or
return to the form; its input is retained.

While a clone runs, the clone form stays open with **Cancel clone**. If the
connection to the host drops, the clone keeps running there: the form shows
"Lost the connection to <host>. The clone continues on the server;
reconnecting…", and when the connection returns it resumes following the clone
and adds the project once it finishes. Cancelling stops Git, shows "Clone
cancelled.", and removes the folder the clone created; while disconnected, the
form shows **Cancelling…** until the host is back, and it accepts a new clone
only once the host has confirmed the cancel. If the clone had already finished,
the form says so and names its folder; the folder stays unregistered, and the
next **Clone** into it adds it. While the form shows the reconnecting line or
**Cancelling…**, you can close the dialog: the clone is cancelled once the host
is back, and a new clone of that URL into that folder waits for the cancel to
finish. Closing the window
during a clone asks the host to cancel it, but a closing or disconnected window
may not reach the host; the clone then keeps running there, and cloning the
same URL into the same folder later joins it or adds the finished repository.
A failed clone shows the reason in the form,
including a stalled transfer or an incomplete earlier clone in the chosen
folder (remove it or choose another folder). If the host cannot be reached
again, the form says so; cloning the same URL into the same folder later joins
the clone or adds the finished repository. The dialog closes once the project
has been added and opened.

Clicking a project header selects it and toggles its thread list; the header
stays highlighted as the selected node until you open a thread, and it is also
highlighted while that project's Git Manager or Pull Requests route is open.
Hovering or focusing a header shows **⋯** (project actions), **+** (New
worktree), **Git Manager** and, when enabled, **Pull Requests**.

Menus separate their groups:

- **Worktree card:** **Open in ›**, **Pull** · **Copy Path**, **Copy Branch
  Name**, **Copy Thread ID** · **Pin** or **Unpin**, **Mark as Unread** or
  **Mark as Read**, **Rename…** · **Delete Worktree…**. A thread without a
  worktree offers **Delete Thread** instead, with an ellipsis when deletion
  asks for confirmation. While a session in the worktree is running or starting, on the
  card or in one of its other chats, **Delete Worktree…** is disabled with
  "Stop the running session before deleting this worktree." (the card hides
  **Archive** while its session runs, too). The removal dialog follows the same
  rule however it opens (this menu, a missing worktree's **Remove from
  BiBCode**, or **Delete** on an archived worktree in **Settings → Archive**):
  its delete buttons stay disabled, with that sentence shown, until the session
  stops. The server also refuses deletion while a session is running or
  starting, so a turn started by another client is protected even before this
  window updates. A server refusal shows the same sentence and keeps the
  worktree. **Remove from BiBCode** remains available without deleting the
  checkout.
- **Primary card (the main checkout):** **Open in ›**, **Pull** · **Copy Path**,
  **Copy Branch Name** · **Pin** or **Unpin**, **Mark as Unread** or **Mark as
  Read**. It can't be deleted; remove the project from its header instead.
- **Project header** (**⋯** or right-click): **New Worktree…** · **Rename…**,
  **Group into…**, **Copy Path** · **Show Hidden Worktrees (N)** or **Hide
  Discovered Worktrees**, **Archived Threads** · **Remove Project…**. N appears
  once the project is expanded. Grouped projects list their members in a
  submenu for the actions that target one member.
- **Several selected cards:** **Mark as Unread (N)** · **Delete (N)**.

**Pull** runs `git pull` in that checkout. **Copy Branch Name** copies the
branch the card shows and is left out when the card shows none. On the local
desktop environment, **Open in → File Explorer** opens the repository folder
for a primary card or the worktree folder for a worktree card; it is left out for
remote environments and browser mode.

The desktop app shows native menus on macOS and Linux. In the browser and on
Windows the menu opens inside the app: its first enabled item is focused, the
arrow keys move between items (skipping separators, but stopping on disabled
items so you can read or hear why they are unavailable; choosing one does
nothing), **Home** and **End** jump to the ends, **→** and **←** open and
close a submenu, **Enter** or **Space** chooses, and **Escape** closes the menu
and returns focus to where you were.

External editors are listed when the server host can find them on `PATH`. Zed
is additionally detected through the `zeditor` alias, a Flatpak export
(`dev.zed.Zed`), `~/.local/bin`, the macOS app bundle CLI, and the Windows
per-user install directory. A Flatpak editor is launched with access to the
opened file's directory, so projects outside your home directory open without
extra Flatpak overrides. Detection runs on the server that owns the
environment, so a remote environment lists the editors installed on that remote
host.

### Discovering existing worktrees

When a connected server advertises worktree-catalog support, BiBCode can show
Git worktrees that already belong to a project repository but have no workspace
row. New projects start with discovery hidden. The first authoritative result
offers **Add**, **Add all**, or **Keep hidden**; the project menu can later
switch between hidden and shown discovery. A hidden acknowledged result is a
compact `Hiding N` summary, while shown results appear as dashed discovered
rows grouped by connected environment and project.

Adding a discovered row adopts that exact server-observed candidate as an
ordinary workspace. It does not create a Git worktree and does not run the
project's worktree-creation script. Concurrent clicks converge on the same
workspace. Discovered rows are grouped beneath their parent directory and use
compact branch or detached-HEAD labels. When labels would otherwise duplicate,
the row adds its final path component as a discriminator. The full host path is
available in a tooltip and accessible name; the compact row copy is separately
keyboard-focusable. The client submits only the project, opaque catalog key,
generation, and command data; the server rechecks the path and repository.

Catalog controls are absent for servers without the capability. Active
catalogs refresh after reconnect and when the window regains focus or becomes
visible. If an observation is degraded, the UI keeps the last authoritative
rows instead of treating them as deleted.

### Missing and removing worktrees

An adopted worktree that is authoritatively missing remains selectable. Its row
shows the branch, host path, registration/lock context, a warning, and actions
to retry verification or remove it. The same warning and disabled filesystem
work apply to all chat panels hosted by that workspace. A temporary Git,
permission, or probe failure is shown as verification unavailable and does not
claim that the directory is missing.

Removal always begins by loading a fresh server plan. For a present worktree the
dialog offers exactly these outcomes: cancel, remove the workspace from
BiBCode, or delete the Git worktree and remove it from BiBCode. Dirty changes
and stale-registration prune impact require separate confirmations. If the plan
changes before execution, the dialog requires review again.

For an already missing worktree, BiBCode may offer verified cleanup of its stale
Git registration before detaching. Cleanup failure is reported as a partial
outcome while the workspace can still be removed from BiBCode. A failed
deletion of a present worktree leaves the workspace attached. Removal requests
contain IDs, the plan token and generation, the selected mode, and confirmation
flags—not a filesystem path.
Deleting a worktree closes its workspace and linked-panel terminals under a
server-held fence before filesystem cleanup begins. Selecting a stale row while
deletion is in progress cannot reopen a terminal into that checkout. If an
external process still uses the worktree as its current directory, deletion
fails before removing files; close that process and retry the same row. If Git
removal succeeds but deleting the sidebar row fails, retrying that stale row is
safe even when a new worktree has since reused the old folder.

### Importing CLI sessions

To continue a conversation you started in Claude Code or Codex outside
BiBCode, open the project menu and choose **Import CLI sessions…**. The dialog
lists the sessions run in the project's folder on the server in the last 30
days, newest first, with the provider, last activity and message count. Select
sessions (or **Select all**) and choose **Import**. Each becomes a thread with
the session's recent messages (up to 200, always including the first prompt); your
next message in it continues the same CLI conversation. BiBCode opens the most
recently active imported thread and reports how many sessions were imported or
skipped and why.

A session imported earlier shows **Already imported** with **Open**; importing
it again does nothing. Conversations started in BiBCode, sessions run in a
subfolder, and sessions run in another worktree are not listed. Only the
default Claude Code and Codex providers are scanned, and a provider turned off
in settings is skipped.

## Center Panel

Terminal input pauses if delivery fails. **Reconnect input** reattaches to the
existing running process without restarting the agent or replaying discarded
keystrokes. Check the prompt before typing again. With an updated client and
server, normal typing no longer waits for each preceding write's reply;
network latency still affects when remote output appears.

The active thread's main chat starts as the first center tab. It can be reordered,
moved between split panes, or closed from the center layout without deleting the
thread. While present, it remains mounted throughout layout and tab changes. The
chat header `+` menu contains:

- enabled AI providers, which create new chat panels
- Reopen closed chat, which lists this thread's ten most recently closed chat
  panels, newest first; it is disabled with **No closed chats** when there are
  none, and offers a retry when closed chats could not be loaded
- Open Terminal, which creates a shell terminal panel in the current worktree
- enabled provider terminal actions, which launch the selected provider CLI in
  the current worktree using that provider instance's configured binary path
- Add custom action, which opens the custom action dialog

Each extra chat panel is an isolated AI session. For contributors, this is
implemented as a hidden sibling thread with `kind: "panel"` that shares the host
thread's project, branch, and worktree. Panel threads are hidden from the left
panel and from Settings → Archived. Closing a chat panel's tab stops its running
turn, keeps its queued messages waiting, and archives the panel thread, so its
history is kept and it can resume;
**Reopen closed chat** brings it back as the active tab (messages queued
behind a stopped turn then offer **Send now**), and a
**Failed to reopen chat panel** toast gives the reason if that fails. Deleting
the host thread deletes its chat panels, open or closed. A new chat panel's tab shows
**Opening chat…** until the server confirms the panel thread; if creation fails,
the tab closes and a **Failed to open chat panel** toast gives the reason.

Tabs persist across reloads. The host chat remains mounted while another center
tab is active, so its transcript, scroll state, and composer state are preserved.

Chat panels and center terminals, including AI Terminals, appear on every
client connected to the same server. A panel opened on another client is added
to this client's host thread as a new tab in the focused pane without switching
to it; its terminal history or chat messages load when you open the tab.
Closing a panel closes it on every client: closing a terminal tab ends its
session, and closing a chat panel archives its thread. A reopened chat panel
returns to every client. Tab order and splits stay per client.

Only the focused center pane may programmatically focus its terminal. Moving
focus to a chat pane leaves visible terminals mounted but prevents them from
reclaiming keyboard input until the user explicitly activates a terminal again.

Use `Cmd+J` on macOS or `Ctrl+J` elsewhere to create and focus a terminal tab in
the focused center pane. When a center terminal owns focus, `Cmd/Ctrl+N` creates
another terminal tab, `Cmd/Ctrl+D` creates a terminal in a new right-hand center
split, `Cmd/Ctrl+Shift+D` creates one in a new lower center split, and
`Cmd/Ctrl+W` closes the focused terminal. Closing the final tab in a split
collapses the empty pane. Infeasible splits, including attempts beyond the
four-pane limit or below the minimum pane size, show a notice without opening a
terminal session. Inside a terminal, `Shift+Enter` sends a soft newline (ESC CR)
so Codex and Claude Code prompts insert a line break instead of submitting;
plain `Enter` still submits.

Project script actions run in a visible center terminal. They reuse the focused
idle center terminal when possible and otherwise open a new center terminal.
The retired bottom terminal drawer and its bottom-toolbar toggle no longer
exist.

### Uploading chat attachments

When the server supports staged attachments and a message's total encoded
attachments exceed 256 KiB, its local pending row shows **Uploading 2 attachments
— 3.1 of 20 MiB** and **Cancel**. Progress counts bytes acknowledged by the
server. Attachments upload one after another; a dropped connection shows
**Reconnecting…**, then resumes the acknowledged bytes when the host returns.
An expired upload can restart once per file. Completed files are kept alive
while later attachments upload and verified before the message is sent. A
server without this capability keeps the
inline attachment path.

**Cancel** stops the upload and immediately restores its prompt, attachments,
terminal and element context, preview annotations and review comments to the
composer. Newer composer edits are kept alongside that snapshot. An upload
failure restores the same work and explains which file failed and how to retry.
Cancel remains available while reconnecting; once turn admission starts, the
upload action disappears and the normal message delivery controls apply.

A send made while an agent is running shows the same local pending upload row
before the server accepts its queue entry. It does not claim to be queued or
start another agent. The running agent's **Stop** remains available. The local
row disappears once queue admission completes. Completed uploads may be reused
by an unchanged send attempt after a failed admission; upload ids are never
saved in composer drafts, and abandoned attempts release their uploads or let
them expire while disconnected.

Switching threads or closing and reopening a view after the upload enters
normal message admission does not cancel or forget that send. Other views wait
for its outcome rather than sending a second copy. If its reply is lost, the next **Send** first checks the
original send without uploading a second copy. An unchanged restored draft
clears once that send is confirmed. New edits remain in the composer with an
**Earlier send confirmed** notice, so you can review them before sending another
message. If the original was not accepted and its upload expired, the error
asks you to send again; that explicit send uploads a fresh copy.

If the environment changes host or data store while a send is unresolved,
BiBCode keeps the draft and original intent and asks you to reconnect to the
original environment before sending another copy. A reconnect or credential
renewal on the same store checks the original send under your current permissions.

### Queued messages

While a turn runs or the provider is still accepting your previous message,
Enter queues your message and clears the composer so you can keep writing.
Queued cards show your text and attachment count below the working indicator,
in the order the server accepted them. Queueing does not start another turn or
change the sidebar's Working indicator. The queue is shared across connected
clients and survives page reloads, disconnects, and server restarts.

The first card says **Sends when the turn ends**; later cards say **Sends after
the messages above**. When the session is ready, the first eligible message
starts the next turn, and each later message waits its turn. The model and modes
selected when you queued it are retained. Its timeline timestamp updates when
it starts, placing it after the preceding reply.

On Codex and Claude, **Steer** or `Mod+Shift+Enter` sends the first queued message
to the running provider. Codex uses it at the next turn boundary. Claude consumes
it at the next tool boundary, or immediately after a reply without tool calls as
its next turn. The card shows **Steering…** until acknowledged, then becomes an
ordinary user message in its attributed turn. You can keep composing while it
waits. Cursor, Grok, and OpenCode explain that steering is unavailable; their
queued messages still send automatically. A rejected or unavailable steer
returns the message to the queue with its reason on the card.

**Cancel** removes the card for everyone and appends its text to the cancelling
client's draft. Attachments queued in that client session return up to the
attachment limit. After reload or cancellation from another client, unavailable
files are reported so you can attach them again. **Stop** restores queued messages
in order before interrupting. If cancellation fails, that card stays with an
error and Stop still interrupts.

Approvals and questions must be answered before automatic sending resumes.
Interrupts and errors hold queued messages even when the provider later becomes
ready. A held first card says **Waiting for you** and offers **Send now**, which
becomes available once the running turn and any pending approval or question are
done. Cancelling remains available while the message is queued; it is disabled
while steering is being acknowledged.

If an earlier message has a failed or uncertain delivery, the first queued card
shows **Waiting for an earlier message**. **Steer** and **Send now** are disabled
with **Retry or dismiss the earlier message first**, or **Dismiss the earlier
message first** when its model or options were refused and Retry cannot help.
Later cards keep their usual **Sends after the messages above** status.

Delivery notices name the provider instance the message was sent to, using its
configured name when available. If the instance is no longer available or an
older server supplied no instance identity, the notice uses the provider name.
A failed delivery whose model or options were refused keeps its failure detail
and offers only **Dismiss**: **Sending it again unchanged would fail, and later
messages wait behind it. Dismiss it, then send it again with another model or
without that option.** The message
remains in the timeline with its copy button so you can prepare the corrected
message. Other failed deliveries still offer Retry and Dismiss; uncertain
deliveries still warn that Retry could send a duplicate.

When BiBCode cannot resume a thread's provider conversation (the provider no
longer has it, for example after its history was cleared), it starts a new one
and the thread shows **Couldn't resume the previous <provider> conversation.
Started a new one with a summary of this thread.** Your next message reaches the
provider together with the thread's earlier messages (up to the last 40, about
24,000 characters, oldest left out first), so the agent can pick up where it left
off; the thread still shows only what you typed. A provider command starting with
`/` is sent as written, and the summary goes with the next ordinary message. A
retried message that had to start a new conversation shows **Sent in a new
conversation with a summary of earlier messages.**

### Composer context window

In the normal composer footer, controls remain visible in this order: MCP
status, context-window usage, then send or stop. Both status controls are
capability gated by the selected provider instance:

| Provider         | MCP-status control | Context-window control |
| ---------------- | ------------------ | ---------------------- |
| Codex            | Supported          | Supported              |
| Claude           | Supported          | Supported              |
| Cursor           | Disabled           | Disabled               |
| Grok             | Disabled           | Disabled               |
| OpenCode         | Disabled           | Disabled               |
| Other or unknown | Disabled           | Disabled               |

Disabled providers still show the corresponding control with an unavailable
tooltip, but the control cannot open a status popover. Stale activity does not
override the selected provider's capability.

For Claude, the MCP popover starts with the status reported during session
initialization and refreshes after successful provider responses. It preserves
the last valid snapshot if a refresh is unavailable.

A supported provider with no valid reading shows an awaiting-data popover until
the first provider response. Once measured, the meter's popover shows active
usage, the maximum when known, and lifetime processed tokens when supplied.
Usage above 90 percent is presented as a warning through the meter's red
treatment. Automatic-compaction support is stated when the provider reports it.

The access and reasoning controls remain icon-only in the composer toolbar.
When Full Access is selected, its lock icon is red in both the toolbar and the
access menu. When the selected reasoning level is the highest level advertised
by the active provider and model, the toolbar's reasoning bars and the selected
level title in the menu are red; lower levels remain neutral.

While a provider turn is active, the timeline shows a reversed paint-and-fade
dotted square followed by `Waiting for` and a whole-second elapsed timer, such
as `Waiting for 3s`. The timer is anchored to the persisted user-message time
after reload and never moves backward when the provider start time arrives.
The animation uses the current theme's muted foreground and becomes static when
reduced motion is requested. A later `pending` delivery blocked behind an
unresolved failed or uncertain delivery shows the
muted line **Waiting for an earlier message. Retry or dismiss it to send this
one.**, or **Waiting for an earlier message. Dismiss it to send this one.** when
the earlier message's model or options were refused. Resolve that earlier
delivery's notice before the pending message can run.
The composer offers `Cancel queued message` for this blocked pending delivery.
That control cancels an already admitted start; the durable **Queued** cards
above have their own Cancel action and let you continue composing.

Question and approval composer footers retain their specialized controls and do
not gain the normal context-window control.

Center tabs can be arranged into as many as four visible split panes. Drag a tab
within its strip to reorder it, into another pane to move it, or onto a pane edge
to create a left, right, upper, or lower split. The tab context menu offers the
same four moves. Each pane has its own active tab; the focused pane owns the
center creation actions, so new chats and terminals open there.

Drag pane dividers to resize them. Layout, focus, tab order, and split ratios
persist across reloads. Closing a split pane merges its tabs into the adjacent
layout without closing chats or terminals. Explicit tab close commands remain
pane-local and do archive their chat panel thread or end their terminal
session.

## Git Manager

Each project header in the left panel has a **Git Manager** branch icon directly
after **New worktree**. It opens the project-scoped centre route
`/project/<environmentId>/<projectId>/git`; opening it again navigates to the
same route instead of creating a second manager or centre tab. While it is
open, that project's header row stays highlighted in the left panel, and the
toolbar leads with the project's name (its checkout path on hover) and the
environment it is connected to. The selected
environment owns every path and Git process, so a remote project's checkout
path remains opaque to the browser.

The toolbar has three segments:

1. **Worktree** selects the main checkout or one of the project's catalogued
   worktrees. A first open or reload starts on the main checkout; a later
   selection is remembered while the current client session remains alive.
2. **Branch** groups local branches into Default, Recent, and Other, and offers
   create, checkout, rename, delete, and merge actions. **Remote branches** lists
   fetched remote-tracking branches such as `origin/develop`; search matches
   both local and remote names without regard to case. Selecting a remote
   branch creates and checks out a local branch with that upstream. If its
   local name already exists, checkout stops and asks you to select or rename
   that local branch; existing work is never replaced. Remote rows offer
   checkout only. Use **Fetch** to discover branches added on the remote.
   **New branch** starts from the checked-out branch (or the current HEAD
   commit when HEAD is detached), never from the repository default; its
   **From** field shows that source and searches every local and remote branch
   to start elsewhere. Creating a branch from a History commit starts at that
   commit. A new branch never tracks its source, so one started from
   `origin/main` publishes under its own name. **Check out after creating** is
   on by default; turn it off to only create the branch, leaving the checked-out
   branch and working tree as they were. The same segment exposes
   tag creation, deletion, and push actions. Symbolic remote default pointers
   such as `origin/HEAD` are not branch rows, while an actual local branch named
   `origin` remains available.
3. **Sync** derives fetch, pull, push, publish-branch, and diverged
   force-with-lease states from the current branch and upstream. Push, publish,
   and force push confirm in one dialog that offers **Also push tags**: every
   local tag then travels with the branch in one atomic push, and a tag the
   remote rejects cancels the whole push. Creating a tag from History offers
   **Push to `<remote>` after creating**; if that push fails, the tag still
   exists locally and the failure says so. A configured
   upstream whose remote-tracking ref has not been fetched keeps the local
   repository usable; ahead/behind remain unknown at zero until Fetch obtains
   that ref instead of making the complete Git Manager unavailable.

The manager keeps the chosen tab for the session but does not save it, so it
opens on **History** after a reload. A reconnect keeps the chosen tab and is not
an opening. Opening it, or switching to another
worktree, selects History for a clean checkout and **Changes** while a merge is
pending; otherwise the chosen tab stays. The manager also returns to History
when a checkout with pending changes becomes clean, for example after a commit
or discard, and selects Changes when a merge starts, since the merge is
finished there. A clean checkout never pulls the user off the **Tags** tab,
which is unrelated to the working tree. A repository Git cannot read is not
a clean checkout: the chosen tab stays during the failure and after repair.
**Changes**, **History** and **Tags** show the same explanation with **Retry**:
no repository (run `git init`); Git can't read it (check `.git`, for example
HEAD or config); or Git doesn't trust another user's repository (run
`git config --global --add safe.directory <folder>` with the selected checkout's
path quoted for the server's shell: single quotes for POSIX shells; forward
slashes in PowerShell single quotes on Windows).
When an older server omits the reason, the message suggests `git init`
or checking an existing repository's `.git` folder. The toolbar shows **No
branch** and **Sync unavailable**; branch, tag, sync, stash, merge and rebase
actions are disabled with that reason. Tabs and Worktree stay usable.
Everything reloads automatically once status reports that Git can read the
repository again; a repaired HEAD or config file is noticed within a moment,
while after `git init` or trusting the folder the manager rechecks within about
a minute, or at once with **Retry**.

The **Tags** tab lists local tags newest first, then one collapsible section
per remote with the tags that remote currently advertises, queried with
`git ls-remote` when the tab opens and again on its refresh button. Remote rows
are marked **not fetched** or **differs locally** against the local set; an
unreachable remote shows its reason with Retry instead of hiding the local
list. Local rows offer Push and Delete through the tag dialog. Each section's
collapsed state is remembered per project.

The **Changes** tab filters and groups working-directory changes, keeps file
inclusion separate from row selection, renders per-file diffs, uses whole-file
inclusion for commits, supports line/hunk stage or unstage, and confirms discard. Its commit box
supports summary and description, no-verify, signoff, allow-empty, co-author
trailers, amend, and eligible undo. Binary, submodule, and oversized content use
explicit interstitials; repository-owned image bytes can be viewed as 2-up,
swipe, onion-skin, or difference without loading an external image.

The **History** tab pages a flat commit list and shows the selected commit's
metadata, changed files, and diff. New repository generations are spliced above
the loaded tip-pinned snapshot; repositories beyond the pin limit show the
less-stable paging warning. Author circles use only locally derived initials and
color. Right-click a commit to access reset, revert, cherry-pick, reorder,
branch or tag creation, and SHA-copy actions. Contiguous multi-commit selections
also offer cherry-pick, squash, and reorder. History actions use the panel's
mounted confirmation and multi-commit dialogs.

Above the tabs, **Stashes** opens the full native stash list with per-entry diff
and apply, pop, and drop actions. Choosing **Leave my changes** while switching
branches creates a normal visible stash. **Merge…** loads a server preview and
starts a merge-commit or squash merge. **Merge commit** never fast-forwards:
whenever the source has commits the current branch lacks, it records a merge
commit. Both modes override repository merge settings such as `merge.ff` or
branch merge options that would otherwise skip the commit, squash, or reject
the merge. A source with no commits the current branch lacks is reported as
nothing to merge and Merge stays disabled. Sources include remote branches,
listed under **Remote**. **Into** chooses the branch to merge into; it defaults
to the checked-out branch. Choosing another local branch updates that branch
without checking it out: your files and the checked-out branch do not change,
the checked-out branch can be the source, Squash is not offered, and commit
hooks do not run (commit signing is still honored). A merge into another branch
that would conflict stays disabled with a hint to check that branch out and
merge there, and a branch checked out in another worktree is blocked with the
worktree's path. On Git older than 2.38 there is no preview: a merge into the
checked-out branch stays available, and merging into another branch is
disabled. **Rebase…** opens a branch chooser
and warns when the rewrite will require updating an upstream with
force-with-lease.
Repositories with a merge, rebase, cherry-pick, or revert in progress show a
continue/abort strip, and conflicted paths are marked in Changes. For supported
conflicted operations, resolve each listed path with Ours or Theirs in the
panel's conflict list, then choose Continue once it is enabled.

**Show pull requests** (**Show merge requests** on GitLab) opens the provider
pane without making a request. Pull-request and check data load only when
**Refresh** is pressed, and the pane never starts a
provider timer. The pane's toggle, heading, status messages, request number and
Create button follow the provider: GitLab uses merge-request wording, `!N` and
**Create merge request**. Until the repository status has loaded, the toggle,
the pane and its review dialog say “change request”; a loaded status that
names no host keeps pull-request wording. Its
create-pull-request review
dialog groups repository, base, and head details separately from branch-publication status, then keeps the
editable title and description in one padded form above the fixed action footer.
Against servers that support it, the form also offers **Mark as draft**,
**Assignee** (with **Assign to me**), **Reviewer**, **Milestone** and **Labels**
for GitHub and GitLab, and GitLab's **Delete source branch** and **Squash
commits**, which start from the project's settings.
On GitLab it says **Create merge request** and uses `!N`. A self-hosted host
that BiBCode has not identified yet shows "Not identified yet" with the next
step (open Pull Requests for the project or Rescan in **Settings → Source
Control**); nothing is published until the host is identified. **New merge
request** in Pull Requests already knows the host and shows it right away.
Hover a disabled **Create** button to see why it is unavailable.
GitHub checks are available; other providers currently return checks
unavailable.

Project view state is stored under `bibcode:git-manager-state:v1` for the two
most recently used physical projects. Its persisted project record contains
`selectedRef`, `selectedCommitSha`, `multiCommitSelection`,
`selectedFilePath`, `selectedStashSha`, `stashPaneOpen`, `imageDiffMode`,
`providerPaneOpen`, `lineSelectionByPath`, `filterText`, `loadedPageCount`,
`loadedPageCursors`, `scrollAnchor`, `commitDraft`, and the `lastUsedAt` value
that enforces eviction. The companion toolbar record contains
`branchFilterText` and `openDropdown`. The mounted commit box does not currently
read the legacy `commitDraft` field; its active draft instead uses the shared
`bibcode:source-control-panel-state:v1` store keyed by environment and checkout,
so the right-panel Source Control surface and Git Manager do not diverge. The
selected worktree is intentionally session-only and resets to the main checkout
after a reload. Evicted or hidden projects retain no mounted panel or live Git
Manager subscription.

## Pull Requests

Hover a project header and choose **Pull Requests**, immediately after Git
Manager, to browse the hosted repository for one checkout. Grouped projects
use the same environment/member picker as other project actions. The project
header stays highlighted on the list and detail routes, including after reload.

### Availability and setup

**Settings → Source Control → Pull requests** controls the button and views
for every environment. Turning it off hides the button; an existing route
explains the setting and links back to Source Control. GitHub and GitLab
provider rows list configured hosts, with account names redacted until revealed.

The repository header shows the provider, account, and custom host. Choose a
worktree to change the checkout scope, **Refresh** to reload the first list
page, or **Rescan** to recheck repository context. Opening the module or
switching worktree reuses host answers from the last 30 seconds and loads the
first list page alongside the context; Rescan asks the host again. A
disconnected environment
stays disconnected. Missing remotes, tools, authentication, and repository
access show server advice, install hints, and a copyable login command when
available. The checkout selector remains available for recovery when a saved
checkout cannot resolve context. Availability identifies `no_remote` (add an
`origin`), `unsupported_provider` (Azure DevOps or Bitbucket), `unknown_host`
(configure that host with `gh auth login --hostname <host>` or
`glab auth login --hostname <host>`), `cli_missing` (install the named CLI on
the server), and `not_authenticated` (log in to the selected host). A missing
checkout path says to select another checkout or restore the directory, then
Rescan. An older server without read support explains that the environment does
not support Pull Requests; without mutation support the view stays read-only.

### List and detail views

GitHub has Open and Closed tabs (Closed includes merged); GitLab has Open,
Merged, Closed, and All. Search applies after 300 ms or immediately on Enter.
Author, assignee, reviewer, review status, draft, labels, milestone, target
branch, and sort controls filter the list. Picker vocabulary loads on first
open. **Load more** appears only when the host supplies a next page; provider
errors remain visible with **Retry** and filtered errors offer **Clear filters**.
GitHub search currently has no further page or total count and does not display
its unknown comment counts as zero badges. GitLab review-status filters
currently return advice to clear that filter because list data lacks approvals.

List rows open a detail view with the title, branches, state, and
server-reported merge readiness. Conversation, Commits, Checks/Pipelines, and
Files changed/Changes show counts and keep the selected tab in the `tab` search
parameter (`conversation`, `commits`, `checks`, or `files`). Only
the active tab loads its data; Files also loads review threads. Detail
**Refresh** reloads the header and active tab, including those threads in Files.
Reviewers, assignees, labels, milestone, linked issues, and GitLab approval rules appear alongside the review, or above the merge box on narrow screens.
Comments, reviews, threads, suggestions, and system events retain host order;
actors use local initials and names/logins, and remote markdown images become
labelled browser links without loading avatars or image URLs. File diffs load
near the viewport,
with **Viewed** saved per request, a local **Ignore whitespace** toggle, and
host links for unavailable or oversized content and a Binary file fallback.

### Checkout for local verification

**Checkout** checks out the request in the selected checkout. Its arrow menu
lists **Current checkout**, other project worktrees and **New worktree…**, which
creates a managed workspace from the request head. Dirty targets and unfinished Git operations
show the server's reason. If another worktree holds the branch, **Switch to that
worktree** retries there. A success toast names the branch and path and offers
**Open Git Manager there**; opening it selects that worktree. Existing work and
conflicting local branches are preserved. A matching branch is reused when it is
free. If its name is already checked out anywhere or points to another commit,
the new worktree uses `<head>-pr-<number>`, then `<head>-pr-<number>-2` and so on
if needed. Other unavailable actions show the host's permission reason.

Once checkout starts writing, closing the view/browser or losing the connection
ends only your wait; the checkout continues on the server. Reconnect, reopen Pull
Requests and use **Refresh** to see the result, or check the project's worktree list.
A cancelled wait clears the busy button and shows a neutral background message;
cancellation before writing says that no Git changes were made. Checkout is never
retried automatically. If Git itself reports a failure, run `git status` in the
checkout path shown by the action and inspect its state before retrying.

### Comments and pending reviews

Post comments with Write/Preview and Ctrl/Cmd+Enter. The comment menu offers
own-comment editing and confirmed deletion, host-supported minimize/unminimize,
Copy link, and Open on host. Reactions toggle from the count or eight-emoji
picker. Threads keep reply drafts and offer Resolve/Unresolve with the host's
permission reason when unavailable.

Click or drag a diff line to compose an inline comment. **Insert suggestion**
prefills the selected new source lines. **Add review comment** saves an amber
pending card; **Add single comment** posts it immediately. The sticky Review
bar in Conversation and Files opens a popover with a summary and Comment,
Approve, and Request changes, plus separate Revoke approval and Remove my change request controls. A review
uses the loaded head commit. On a changed head, Refresh and inspect the new
commits before submitting again; pending comments remain. Partial results keep
failed comments by file, line, and body and list host messages. An already-posted
summary is cleared and retries default to Comment so they do not repeat the
summary or approval. Saved comments for files no longer in the diff remain
available to edit or remove.

GitLab suggestions offer Apply with an optional commit message; selecting
several enables **Apply N selected** in Files. Unsupported Apply keeps its
reason visible and offers Copy. Dismiss review requires a message and
confirmation; eligible reviewers offer Re-request. Unsupported host controls
stay visible with their reasons. Successful writes refresh affected views.
Comment text, edits, replies, inline drafts, pending comments, and review
summaries survive navigation and reload; failures preserve them.

### Editing and Undo

Use the title pencil or description **Edit** to change the request in place.
Descriptions offer Write/Preview; Save publishes the edit, while Cancel or Escape
keeps its draft. Reviewers, assignees, labels and milestones have searchable
pickers. Changes apply immediately and offer **Undo** for five seconds; expiry
does nothing. Milestone **Clear** removes the milestone. Changing the base branch
asks before clearing pending review comments and keeps them if the change fails.
The base confirmation says “Changing the base clears N pending review comments.”
(with singular wording for one). Title, description, and base saves do not offer
Undo; the five-second Undo applies to reviewer, assignee, label, milestone, lock
and draft-state changes. Clicking it reverses the exact edit once and may wait
for a running action; closing the toast or letting it expire sends no write.
Picker choices stay disabled with “Refreshing request details…” until the
refreshed selection arrives after a write or Undo.
Linked issues come from the description.

### Merge and other host actions

The merge box shows allowed methods and the repository's defaults. GitLab shows
its project method as text. Edit the merge subject/body, choose branch deletion
and optional auto-merge, then inspect the confirmation's method and target before
merging. A read-only viewer sees the server reason without a method picker.
**Auto-merge enabled** means merging has been scheduled, not completed; use
**Disable auto-merge** to cancel it. GitHub disables bypass while auto-merge is
selected, and the server rejects the auto-merge/bypass combination on GitHub.
The normal confirmation title is “Merge pull request” on GitHub and “Merge
merge request” on GitLab. Request-specific recovery messages and accessibility
labels use the same host vocabulary; the module remains **Pull Requests**.
For a target named `main`, the confirmation says “Merge now into main.” or
“Merge automatically when requirements pass into main.” and lists Method,
Delete branch, and Auto-merge.
Bypass additionally says “Branch requirements will be bypassed.” on GitHub or
“Requested changes will be overridden.” on GitLab. Behind branches offer
**Update branch** or **Rebase**,
with **Skip CI** on GitLab. A changed head asks you to refresh and try again;
merge drafts remain. Partial-progress messages state what completed and never
trigger an automatic retry.

**More actions** offers Mark ready/Convert to draft, Lock/Unlock, Close/Reopen,
Copy URL and Open in browser. GitHub Lock lists the host's reasons. Merged requests
offer **Revert**, whose confirmation explains that it creates a new request;
success opens that request. GitLab **Delete** names the request and host, for
example “Delete !14. This cannot be undone on gitlab.company.example.”, then
returns to the list after success. Revert says “This creates a new pull request
that reverts #14.” (or “merge request” and `!14` on GitLab). Dismiss review names
the reviewer and host, for example “This dismisses alice’s review on github.com.
Include a message explaining why.”, and requires that message. These actions require explicit confirmation; Close/Reopen do not.
Title, description and merge-message drafts survive navigation and reload.

### Navigation, refresh, and saved work

**New pull request**
(or the host's equivalent wording) reuses the Git Manager creation dialog:
opening it creates nothing, and only its explicit primary action publishes.
Git Manager's current-branch pane also links each row to **Open in Pull Requests**,
selecting that pane's checkout before navigation.
The local `bibcode:pull-requests-state:v1` cache retains the two most recently
used physical projects (LRU), including checkout, filters, tab, sort, scroll
position, viewed files, and unsent per-review drafts. This module does not fetch avatar images or refresh on a timer or
window focus.

The server serializes actions for the same provider, host, repository and
request number, including actions from different checkouts or clients. An
action rereads current permissions before writing; the host can still refuse
it. Approval and merge carry the head you reviewed. A stale-head error says
“This pull request changed; reload and try again” and offers Refresh while
preserving drafts. Errors include server recovery advice; partial writes report
what landed and are never retried automatically.

## Right Panel

The right panel hosts persistent tool surfaces for the active thread. Use its
`+` menu to add Browser, Terminal, Files, Diff, or Source Control. Activity and
Plan surfaces can also appear when the active provider/session supplies them.

- **Browser** opens a local application preview or URL. It is a Tauri child
  webview, so it exists in the desktop app on macOS 14+, Windows, and Linux, not
  in a browser tab.
- **Terminal** starts a shell in the active workspace.
- **Diff** reviews branch or worktree changes.
- **Activity** shows structured provider activity when available.
- **Plan** displays the active agent plan when available.

### Opening links

Web links open in the BiBCode browser by default. **Settings → General → Open
links in** chooses **BiBCode browser** or **System browser**; a change applies to
the next click. Without a thread, without desktop preview support (for example
macOS before 14), or in a browser tab, web links open in the system browser, and
the setting is not shown.

- **Chat:** Ctrl/Cmd-, Shift-, or Alt-click or middle-click opens the other
  target. Right-click on an `http(s)` link offers **Open in BiBCode browser**,
  **Open in system browser**, and **Copy link**. Any
  modifier-click on an `.html`, `.htm`, or `.pdf` file chip opens
  it in the editor instead of the browser. Links to network or device paths
  (`file://server/…`, `file:////server/…`, `\\server\share`, `\\?\…`, `\\.\…`)
  never become file chips, so they can't open in the editor or the browser; a
  `//host/…` link stays an ordinary web link.
- **Terminal:** Ctrl/Cmd-click opens a link (Cmd on macOS); adding Shift opens
  the other target. `.html`, `.htm`, and `.pdf` paths, including a bare
  `index.html` and `file:///` URLs, open in the BiBCode browser; Ctrl/Cmd+Shift
  opens them in the editor, and every other path opens in the editor. Without a
  thread or desktop preview support, they open in the editor too. If such a
  file can't be previewed (for example, the environment isn't connected), a
  "Couldn't preview this file" notice offers **Open in editor**. A printed path
  that starts with two separators (`\\server\share`, `//server/share`, `\\?\…`,
  `\\.\…`) is refused with "Network paths can't be opened from the terminal."
  A `file://` URL with a host shows "Unable to open this file link." Relative
  links under a network working directory, such as a WSL `\\wsl.localhost\…`
  folder, still open. A hyperlink a program emits (OSC 8)
  needs the same Ctrl/Cmd activation. When its visible text doesn't match its
  destination, a menu shows **Open …** with the destination and **Copy link**.
- **Browser tabs:** popups and `target=_blank` links inside a preview tab open
  as a new tab in the same thread. `about:blank` popups are dropped. A
  `target=_blank` link in the app itself, such as a pull request title in Git
  Manager, opens in the system browser.

A link to `localhost`, a `*.localhost` name, any `127.x.x.x` address, `::1`, or a
wildcard address (`0.0.0.0`, `[::]`) means the server's machine, not this
computer. From a thread on this computer's own server it opens directly. From
any other thread BiBCode never opens this computer's `localhost`, whatever the
target, modifier, or setting; see
[Previewing the server's dev servers](#previewing-the-servers-dev-servers).

A file outside the thread's workspace can't be previewed; the notice offers
**Open in editor**. If the system browser fails to open a link, the notice shows
the link with **Copy link**.

HTML, XHTML, SVG, and XML files the agent wrote are served to the browser in a
sandbox with no origin of their own, so their scripts can't use `localStorage`
or cookies. The agent's `preview_open` tool works on desktop for opening and
navigating a tab; reading the page, clicking, and typing aren't supported yet.
On desktop it always shows the tab it opens, and a request for a thread that
isn't on screen times out. In a browser tab, `preview_open` shows the open
prompt (below) and tells the agent the open is waiting for you.
A server a terminal starts is listed as a discovered port for that terminal and
its thread.

### Previewing the server's dev servers

A dev server or tool listening on the server's loopback, such as
`http://localhost:5173`, opens through the server's preview gateway when the
thread's environment is:

- on a LAN, tailnet (`100.64.0.0/10`), or WSL address, or reached by a host
  name such as `devbox` or `box.lan`; the preview loads from
  `http://<server address>:<gateway port>`;
- desktop-managed SSH, on desktop only; the desktop forwards the gateway port
  over the SSH connection. The preview loads from the same address as on the
  server, such as `http://localhost:5173`, so sign-in redirects and other
  links to that address keep working. When that port is already in use on
  your computer, it loads from `http://127.0.0.1:<local port>` instead, and a
  note says the port is in use and that apps expecting it (OAuth sign-in, for
  example) may not work until you free it.

The gateway opens one port per thread and dev-server port, so the client must
be able to reach that host on ports other than BiBCode's own. The BiBCode
browser's address bar and the thread's shared tab state keep showing the
`localhost` address, and its **Reload** opens the address through the gateway
again. A
gateway port closes when its last BiBCode browser tab closes, after 10 minutes
without connections, when its thread is deleted, or when the server stops.
Closing a system-browser or browser tab doesn't close it; the idle timeout
does.

Over SSH, the desktop keeps a tab's forward for a minute after you switch to
another tab, so switching back reuses it; closing the tab ends its forward at
once. An address opened in the system browser keeps its forward for 5 minutes.

The previewed app runs on a new origin, not `localhost`. The gateway sends the
app `Host: localhost:<port>`, but anything the browser checks against the page's
own origin breaks if the app hard-codes `localhost`: OAuth redirect URLs, CORS
allowlists, and links or scripts that name `http://localhost:<port>`.

These cases show "Can't open this address here" with the address and **Copy
link**:

| Situation                                                           | Message                                                                                                               |
| ------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------- |
| BiBCode Connect, a public IP address, or SSH in a browser tab       | "This address is on <environment>, not this computer. Opening its ports from here isn't supported yet."               |
| The environment isn't connected, or its SSH connection is down      | "<environment> isn't connected. Reconnect it, then open the link again."                                              |
| An `https://` dev server                                            | "HTTPS dev servers can't be previewed through the gateway yet; serve over HTTP or open it on <environment> directly." |
| Nothing listens on the port                                         | "Nothing is listening on port <port> on <environment>."                                                               |
| The server refused the address                                      | "BiBCode can only preview plain HTTP addresses on <environment>'s own localhost."                                     |
| You reached the server on a public address                          | "Previews aren't available on a public address. Open it on <environment> directly."                                   |
| You reached the server through a reverse proxy or Tailscale Serve   | "Previews aren't available through a proxied address. Open it on <environment> directly."                             |
| The gateway or SSH forward failed, or this client's session expired | "Couldn't open a preview connection to <environment>. Try again, or reconnect <environment> if it keeps failing."     |
| A terminal with no thread                                           | "Open this address from a thread's chat or terminal to reach it from here."                                           |

When a BiBCode browser tab's address can't load, the tab shows the reason in
place of the page, and **Reload** resolves the address again and reopens it.
For BiBCode's own reasons (the messages above) the page reads "Can't show this
page here" and shows the sentence alone, without network error codes or
connection tips. A network error reads "This site can't be reached".

A refusal about the address itself (HTTPS, nothing listening, an address the
server won't preview) shows the same on every client. A failure on this
computer's side shows on this computer only, because another client may load
the page fine: its SSH forward, its BiBCode session, the public or proxied
address it reached the server on, or a failure resolving or forwarding the
address. After a page has loaded, a dropped forward shows the webview's own
error page instead; **Reload** recovers. An agent asking for the tab's status,
or opening a new tab, gets the same reason as an error.

Gateway traffic is plain HTTP on the server's address, outside BiBCode's
encrypted transport, so previews need a direct private route to the server: a
LAN, tailnet, or WSL address, or a desktop-managed SSH connection. When the
environment's address is a public IP address, BiBCode doesn't use the gateway.
When you reach the server on a public address under a host name, or through a
reverse proxy or Tailscale Serve, the server refuses the preview with the
messages above. See [Reverse proxies](./remote-access.md#reverse-proxies).

**In a browser tab**, a gateway link opens a new browser tab. The tab opens
blank at once and loads the preview after BiBCode resolves it; if resolution
fails, the tab closes and the notice appears. If the browser blocks the new
tab, a bar at the top of the window asks again:

- "Agent wants to open <address>": the agent's `preview_open`.
- "A command wants to open <address>": a `$BROWSER` request (below).
- "A command in “<thread title>” wants to open <address>": a `$BROWSER`
  request from a thread that isn't on screen, on desktop too. Its button reads
  **Show thread and open**.
- "Your browser blocked a new tab for <address>": a click whose tab was
  blocked, followed by "Allow pop-ups for this site to open links directly."

**Open** opens the address from your click (**Show thread and open** first
shows the thread the bar names); **Copy link** copies the address and keeps the request;
**Dismiss** drops the request. The bar shows one request at a time, with
"(N more waiting)" when more are queued, and keeps a request whose open failed
so you can try again. The gateway pages a preview tab can show:

- "This preview link expired. Go back to BiBCode and open it again." The
  preview's sign-in is gone: the link was already used or is older than 60
  seconds, or your BiBCode session ended or was revoked. A live preview stops
  within 30 seconds of its BiBCode session being revoked.
- "Nothing is listening on port <port> on <environment>." The dev server
  stopped.
- "This request didn't come from the preview itself, so the gateway blocked
  it." Another page, such as another preview, tried to send data or open a
  live connection to this one.
- "This preview has too many open connections. Try again in a moment."

Reloading such a browser tab works while its gateway port is open. After the
port closes, the tab shows a connection error or the expired page; open the
link again from BiBCode, which starts a new gateway session.

**Commands that open a browser.** Every agent and terminal session gets
`BROWSER` and `BRAINSTORM_OPEN_CMD` set to `bibcode-open-url` (a
`bibcode-open-url.exe` on Windows), so tools that launch a browser through
either variable, such as the brainstorming companion, ask BiBCode instead of
opening a browser on the server's machine. One BiBCode client takes the
request. A visible client showing that thread takes it at once. Otherwise,
after 2 seconds, a visible client takes it and asks with the "A command in
“<thread title>” wants to open" bar; a client whose window is hidden takes it
only while showing the thread. On desktop, a request from a thread on screen
opens like a clicked link, in the system browser when the thread is only shown
in a side panel. In a browser tab it always waits for **Open** on the bar. A
request no client takes within 60 seconds is dropped.

`bibcode-open-url` prints the address instead when it has no BiBCode session
(for example, run outside an agent or terminal session) or when BiBCode can't
take the request. Its credential lasts 8 hours from the start of the agent or
terminal session and is not renewed, so a longer session falls back to
printing. It also prints when the command can't reach BiBCode, for example
inside a sandbox without network access. See
[Provider architecture](../architecture/providers.md) for the mechanism.

### Activity and targeted Stop

Activity combines provider-attributed observation with capability-gated
control. The dock shows one provider icon for the active scope; each Subagents
row shows one provider icon for its actor. Active and Done counts are the only
multiplicity signal: they are primary row content, while elapsed time is
secondary metadata aligned beneath the section title. The same Activity
presentation is used in the inline right panel and its responsive sheet.

Activity record details format Started, Ended, and event instants using the
user's timestamp preference. The exact canonical RFC 3339 value remains
available in the semantic time metadata and hover tooltip.

Subagents follow the canonical actor hierarchy, using indentation and a
connector for a visible parent. Missing, invalid, cyclic, or otherwise unusable
parentage safely renders the actor as a root rather than inventing a hierarchy.
In a structured-chat **Subagents** roster, an active actor shows a persistent
trailing action only while the current provider runtime has proved a current
admitted control target for that actor. The row and action are separate
keyboard-focusable controls: the row opens detail, while the action acts
immediately and does not open detail. Its accessible label and tooltip name the
actor and the number of currently active child agents included in the subtree.
An active actor without a current exact provider target keeps its observed
**Running** lifecycle and shows read-only **Stop unavailable** in the action
column. That label performs no RPC and is distinct from server-authoritative
**Stopping**; it makes restart or target-retirement state explicit without
inventing cancellation authority.

**Stop subtree** targets the selected actor and every attributable descendant
in its canonical subtree; **Stop** targets an actor with no active descendants.
Neither targets the actor's parent, siblings, root chat, unrelated work, or an
Activity-enabled terminal. Unsupported and terminal actors have no action. The
composer Stop remains the separate root-turn action.

After admission, every currently covered active actor shows **Stopping** and
its action is disabled. This label is server-authoritative intent, not a
completed lifecycle: the row moves to Done only after provider events report a
terminal state. If dispatch finishes with active residuals, the panel reports
the bounded remaining count and offers **Retry remaining**. Retry is constrained
by the server to residuals and late descendants under the original cancellation
fence; it cannot expand to a parent, sibling, or replacement provider runtime.
An operation that still has active residuals ten seconds after admission becomes
partial even if provider delivery returned without a terminal lifecycle event,
so the UI cannot remain on **Stopping** indefinitely.
Reconnect restores the current server's control state, while a server restart
requires the new runtime to prove exact targets again.

Right-panel terminals retain their internal terminal grouping and splitting.
When a right-panel terminal owns focus, the terminal new, split right, split
down, and close shortcuts operate within that right-panel terminal surface;
`Cmd/Ctrl+J` still creates a center terminal.

### Source Control

The Source Control panel is Orca-parity for the shipped local Git workflow:

- The primary action is adaptive. With staged files it defaults to Commit. With
  only unstaged or untracked files it becomes Stage All Changes. Clean-tree
  states then move through pull, push, and PR actions when available. **Push &
  create MR/PR** and **Create MR/PR** publish a branch that is not on the remote
  yet, then open the shared review dialog with that branch as the source. Publish is
  currently shown disabled in this right-panel surface; the separate GitHub
  publish flow lives in the chat-header Git actions control.
- The dropdown is always rendered and disables unavailable actions instead of
  hiding them.
- Files are grouped into staged, unstaged, and untracked sections with status
  badges.
- Inside each section, pending files sit under a folder header that names the
  full relative directory, counts its files, and collapses with its chevron.
  Files in the repository root come first under `/`, then directories in path
  order. A directory too long for the panel truncates from its start so the
  deepest folder stays readable, and the header tooltip carries the full path.
  A collapsed folder stays collapsed until the panel or its section is
  collapsed; the state is not persisted.
- The folder header checkbox stages or unstages every file in that folder in one
  request, and selects or deselects them in selection mode. It shows a mixed
  state when only some of the folder's files are staged or selected.
- **Group by folder** next to the panel actions switches between the folder view
  and a flat list whose rows each show their own directory. The choice is
  remembered per user; folder grouping is the default.
- Per-file hover actions support stage, unstage, discard, restore deleted files,
  and delete untracked files. Destructive actions require confirmation.
- Row context menus provide view, copy path, copy relative path, open in external
  editor, ignore file name, and ignore parent folder when the corresponding host
  actions are available.
- Commit history and AI commit-message generation are available in the panel.
- **Merge into current branch…** in the dropdown opens the merge dialog for the
  checked-out branch with local and remote sources. **Fetch** refreshes the
  selected remote branch's remote, or every remote for a local source, before
  you merge; Merge records a merge commit. The entry explains why it is
  disabled: no Git Manager support on the environment, a detached HEAD, an
  operation already in progress, or uncommitted changes.
- While a merge is in progress the panel shows a merge strip. **Commit merge**
  stays disabled until every conflict is resolved and staged; **Abort** asks
  for confirmation and restores the pre-merge state. **Merge Changes** lists
  each conflicted file with **Ours**, **Theirs**, **Mark resolved** (stage the
  file as you edited it) and open-in-editor. The usual Commit action is hidden
  until the merge finishes.
- Successful saves from the built-in file editor notify active Source Control
  subscriptions immediately. Periodic status polling remains a fallback for
  changes made by external tools.

Stash, amend, rebase, and squash merges are intentionally not present in this
right-panel Source Control surface; this matches the Orca reference behavior for this pass. The
project-scoped Git Manager is a separate centre surface and does provide stash
operations and amend.

### Files

The Files surface is a full file manager for the active workspace:

- Every directory is its own row with its own expand arrow. A folder whose only
  child is another folder is not merged into a single combined row, so each row
  names exactly one directory.
- Git-ignored files and directory roots remain visible with ignored styling,
  and the contents below an ignored directory are loaded eagerly with the rest
  of the tree.
- Right-click files, folders, or the tree background to create files/folders,
  rename, delete, duplicate, copy paths, add a folder as a project, download or
  upload files, open in an external editor, or open previewable files in the
  preview browser.
- **New File…** and **New Folder…** create the entry in the clicked folder. On a
  file row they use that file's parent directory, and on the tree background
  they use the workspace root.
- **Download** saves the right-clicked file, or a `.zip` of the right-clicked
  folder, into a folder you choose (the desktop app asks for the folder; a
  browser uses its own download location). The zip includes ignored files such
  as `.git` and `node_modules`; symbolic links are skipped. Folders larger than
  2 GiB or 200,000 entries are refused with a message that names the limit,
  before the download starts. On the desktop app a
  download never overwrites an existing file with the same name; it saves as
  `name (2).ext` instead.
- **Upload Files…** on a folder row, or on the tree background for the
  workspace root, opens a file picker and uploads the chosen files into that
  folder. It is disabled on file rows. An upload that would replace an existing
  file asks first. Files up to 1 GiB each are accepted. A name that is not a
  plain file name — empty, `.`, `..`, containing a path separator or a control
  character, or longer than 255 bytes — is always refused with a message naming
  the rule and the file it applies to.
  When the server holding the workspace runs on **Windows**, the names Windows
  itself cannot store are refused too: those containing `< > : " | ? *`, ending
  in a dot or a space, or named after a device such as `CON` or `COM1`. A
  server on Linux or macOS accepts those names, because its filesystem stores
  them.
- Downloads follow the same reasoning from the other side. A workspace file
  whose name Windows cannot store is still downloadable: the desktop app on
  Windows saves it under the closest name Windows accepts (forbidden characters
  become `_`, trailing dots and spaces are dropped, a device name gains a `_`),
  and the toast shows the path it actually wrote. A name longer than 255 bytes
  is shortened to fit, keeping its extension, rather than refused. On Linux and macOS the name
  is kept as is, and a browser download always follows the browser's own rules. Both actions work for
  remote environments; the transfer goes to the server that owns the workspace.
- Drag one or more files and folders onto a folder row, or onto the tree's root
  area, to move them there. Entries already in the target folder stay put. A
  move the server rejects is reported and the tree resyncs to the server's state
  rather than keeping the dragged row in its new place. Dragging is disabled
  while the workspace is unavailable. If availability changes during a drag,
  the optimistic move is likewise resynced instead of remaining on screen.
  Dragging entries to or from the operating system's file manager is not
  supported.
- Open file tabs follow renames and moves, and close when their file is deleted.
- The tree follows changes made outside BiBCode. While the Files surface is open,
  the server watches the workspace and the tree picks up files and folders
  created, renamed, or removed by other tools within a few seconds. Editing a
  file's contents outside BiBCode does not change the tree, because the tree
  lists paths rather than contents.
- The panel header offers collapse all folders, expand all folders, search, and
  Refresh. **Refresh** rescans the workspace on the server immediately, rather
  than waiting for the next check. The tree background context menu offers the
  same Refresh. While that rescan is pending the action is disabled and labelled
  **Refreshing…**; repeated requests share that same rescan. A server or
  transport failure is reported and the existing tree remains available after
  its query is reconciled.
- Saves to built-in Git classification controls such as `.gitignore` and files
  under `.git` automatically rescan the tree. If the repository configures an
  arbitrary custom `core.excludesFile`, editing that custom file is not detected
  from the current cache; use **Refresh** after changing it. Saving content to
  an existing ordinary file keeps the cached path list, while creating a file or
  parent folder rebuilds it.
- Expanded folders stay expanded. Refreshing, and creating, renaming, deleting,
  duplicating, or moving an entry, does not collapse the tree.
- Every selected file shows a Save, Undo, and Redo toolbar below its
  breadcrumbs. Markdown files also show their rendered/source toggle in this
  toolbar. While a file is active, edits remain pending until Save or
  Ctrl/Cmd+S is used. Switching to any other right-panel surface or hiding the
  panel saves pending edits in the background. Undo and Redo use independent
  native history for each open source file. Read-only views keep unavailable
  actions visible but disabled.

## Current limitations

- Staged-row diff viewing in the right-panel Source Control surface does not yet
  use a true `git diff --cached` source. Git Manager diffs are a separate path.
- Git Manager has no configurable keybinding command IDs yet; use its sidebar
  entry point and the keyboard-operable controls within the panel.
- Outside changes are detected by a periodic check, so the tree updates within
  seconds rather than instantly. Use Refresh when you want it immediately.
- Changes to an arbitrary custom Git `core.excludesFile` require **Refresh**;
  automatic classification-control provenance is not yet cached.
- File mutations validate the workspace-relative target before the later
  path-based filesystem call; they do not yet use an anchored, no-follow handle.
  A dangling symlink or concurrently rebound ancestor can therefore race that
  validation. Do not mutate a workspace whose path topology is controlled by
  untrusted concurrent software.
