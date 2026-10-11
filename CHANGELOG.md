# Changelog

## [v0.9.0] - 2026-10-09

BiBCode v0.9.0 opens clicked links in the in-app browser by default, reaches a development server on the server's localhost from SSH, LAN, tailnet, and WSL clients, keeps chat panels and provider conversations across clients and restarts, and merges a branch without checking it out. This release includes the changes since v0.8.2. Database migrations 053 and 054 ship with this release.

### Links

- Settings → General → **Open links in** chooses **BiBCode browser** or **System browser**. The new `browserLinkTarget` default is `app`, so a clicked link opens in the in-app browser. The row is shown only where that browser exists.
- Chat links, chat file chips, terminal URLs, OSC 8 hyperlinks, terminal `.html`, `.htm`, and `.pdf` paths, preview popups, and agent `preview_open` share one router. In chat, a modifier-click or middle-click opens the other target. In the terminal, Ctrl/Cmd+Shift-click does.
- A server-loopback URL (`localhost`, `127.0.0.0/8`, `0.0.0.0`, `[::]`, `*.localhost`) is resolved for the thread's environment. On the same host it opens unchanged. For a LAN, tailnet, WSL, or host name it is rewritten onto the server's address. A public IP, or a server this client cannot reach, shows a notice with **Copy link** instead of opening the wrong machine. A development server on the server's own localhost uses the preview gateway below, including over SSH.
- Network and device paths (`\\server`, `//server`, `\\?\`, `\\.\`) and `file://` URLs with a host are refused from the terminal. An OSC 8 link whose visible text does not match its destination asks for confirmation. Chat links to network or device paths never become file links.
- On the desktop, preview popups become new preview tabs in the same thread, and the opener plugin's click capture is off. Agent-written HTML, XHTML, SVG, and `*+xml` assets are served with a sandbox content security policy. Discovered ports are attributed to the terminal that started them.

### Preview gateway

- A development server on the server's `localhost` opens in the BiBCode browser from a desktop over SSH, a browser on the LAN or tailnet, and WSL. Shared preview state keeps the canonical `http://localhost:<port>` URL, and each client loads the address it can reach.
- The gateway binds the address the caller used to reach the server and refuses public and proxied callers with `not-reachable`. Access uses a 60-second single-use capability URL. An HttpOnly cookie is set on a bootstrap page, then the target path replaces it. BiBCode Connect (relay) environments still get the unreachable notice. HTTPS development servers are not proxied.
- Provider sessions and every terminal receive `BROWSER=bibcode-open-url`. `bibcode open-url <url>` posts to the server, and the client showing the thread opens it. If no client is listening, the helper prints the URL. On Windows the helper is a `bibcode-open-url.exe` alias.
- A typed server refusal (HTTPS, a non-loopback URL, or nothing listening) is shared. A failure of this client's own reach stays on that client and shows an overlay with Reload.

### Chat panels and sessions

- An AI terminal or chat panel opened in one client appears, with its existing work, in every client connected to the same server. Closing one closes it everywhere. A tab adopted from another client does not take focus. A label taken from the command follows the session's current command; a custom label stays.
- Closing a chat panel archives it instead of deleting it, so its history and provider resume state survive. **+ → Reopen closed chat** lists up to 10 of the host's archived panels, newest first, and brings the panel back on every client. The menu shows **No closed chats** when there are none, and **Couldn't load closed chats. Select to retry.** when loading fails. Settings → Archived hides panel threads. Deleting a thread also deletes its panel threads.
- A provider session stays up while a subagent is still working. Idle suspension no longer shuts the driver down 60 seconds after the turn that launched those subagents. A clean server shutdown saves each session as suspended with its resume cursor, so an ordinary restart or update continues the same provider conversation. Explicit stop, delete, and restart are unchanged.
- **Import CLI sessions…** on the project menu imports Claude Code and Codex CLI sessions whose working directory is the project root, from the last 30 days, at most 200, excluding BiBCode's own conversations. The imported thread shows that history, and the next message resumes the same CLI conversation. A session that is already imported offers **Open**.
- When a provider conversation cannot be resumed, BiBCode starts a new one and sends a bounded summary of this thread. The thread records "Couldn't resume the previous <provider> conversation. Started a new one with a summary of this thread." The composer notice reads "Sent in a new conversation with a summary of earlier messages." The stored message keeps the typed text. A Cursor `session/load` error starts a replacement session only when the provider says that saved session is gone. Any other JSON-RPC error is returned and the saved session id stays in place. OpenCode starts a new session only on a 404. Claude resumes only when its transcript file still exists.

### Git

- The Git Manager **Merge…** dialog has an **Into** picker. Choose another local branch to merge into it without checking it out: your files and checked-out branch stay as they are, a merge that would conflict is refused before anything changes, and a branch checked out in another worktree is blocked. The result is published with a fast-forward-only update, so a target that moved is left unchanged.
- The Merge dialog lists remote branches as sources.
- Source Control has **Merge into current branch…**: pick a local or remote branch, **Fetch** it, and merge it into the checked-out branch. A merge in progress shows **Commit merge** and **Abort**, with **Ours**, **Theirs**, and **Mark resolved** for each conflicted file. **Commit merge** stays disabled until every conflict is resolved, and the control shows "Resolve and stage every conflicted file first."
- Source Control and the Git Manager Changes tab no longer lose their file list while a merge has conflicts, and conflicted files appear once.
- On Git older than 2.38, the merge preview says which Git version it needs instead of reporting unrelated histories.

### Pull and merge requests

- GitLab detail and timeline reads that do not depend on each other now run together, and suggestion note bodies on one discussion page run together too. The list was already one wave.
- While the document is visible, a GitLab merge request paints from the last stored snapshot and stays current. Hiding the document stops that refresh. Merge, review, comment, and the other actions stay inactive until a live detail succeeds on this visit. A row hovered for 150 ms prefetches detail and timeline, at most two per environment, and a prefetch younger than 5 seconds is shown when the merge request opens.
- GitLab Overview shows **Approve** under the description. It approves the current head and does not post pending inline comments. After approval the same control becomes **Revoke approval**. Until the live detail succeeds it stays inactive with **Loading…**. GitHub still approves from the review popover, and GitHub routes stay on demand.
- Snapshots live in the environment SQLite database, capped at 32 MiB. Files over 1 MiB are not stored. Logout and authentication or permission failures delete that host's rows. Rescan does not.

### Database

- Migration 053, `PullRequestSnapshots`, creates `pull_request_snapshots` with host, project, kind, key, fingerprint, payload, observed time, and generation. That table holds the GitLab merge-request snapshots above.
- Migration 054, `ProjectionThreadHostThread`, adds a nullable `host_thread_id` column on `projection_threads` when that table exists without the column. A panel thread records its host thread so every client can open it as a tab under that host.

### Tests

- An intentional hermetic-guard abort still ends in `SIGABRT`. It now clears the core limit and, on Linux, the dumpable flag before aborting, so a passing guard test no longer writes a core or raises a desktop crash notification. Release builds without the hermetic feature are unchanged.

### Known limitations

- Windows and macOS builds, and the native desktop checks for link routing, cross-client panels, the preview gateway, and Source Control merge, were not run. Validation for those features was on Linux. The Linux preview child-webview geometry check was launched, but the desktop session was locked, so the UI was not driven. Live preview-gateway procedures (SSH preview, the brainstorm companion over SSH, browser-mode cross-site bootstrap, revocation, the cross-port 403, and whether Codex agents receive `BIBCODE_OPEN_URL_AUTH`) were not run. `fetch .` publishing was not exercised on reftable repositories, and a packaged end-to-end walk through Source Control merge was not run.
- On desktop, agent `preview_open` always shows the tab (`show: false` is ignored), and a request for a thread that is not on screen times out. Server ports on BiBCode Connect still show the unreachable notice. HTTPS development servers are not proxied. A hostname that resolves to `127.0.1.1` on the same machine is refused as proxied; use `localhost`. A reverse proxy on another LAN host, global IPv6 to a `::`-bound server, and dev Vite over the LAN are refused. The desktop does not report native page-load failures; after a page has loaded, a dropped forward shows the webview's own error page, and Reload recovers.
- A visible terminal tab still closes itself when its session reports `closed` or `exited`. A restart that fails while another client is viewing the tab closes it there too. Split arrangement and tab order stay per client. Terminal scrollback is still lost when the server restarts.
- Session continuity applies to BiBCode-hosted agent sessions after the desktop app is rebuilt and reinstalled. A provider that keeps emitting no-op events can hold a session until the 30-minute quiet cap, and a projection that fails to apply can leave a subagent marked live until that cap. A prompt that is already being sent when a chat panel closes still starts. A Codex model taken from an imported transcript may be retired. Symlinked workspace paths may list Claude sessions that cannot be resumed.
- A reconcile that finds a provider conversation gone can replace the saved cursor while the row stays frozen to the old id, and a later explicit Retry then fails the identity check. A released frozen retry shows both the new-conversation activity and the notice. A crash right after sending can resend the handoff once.
- Pipe-pattern core handlers are not asserted to skip a filesystem core. The dumpable clear still applies on Linux. Hermetic report mode and allowed roots are unchanged.
- Open-url tokens live 8 hours with no renewal. `delivered` means a subscriber exists, not that the URL was opened. A desktop SSH forward can be shared across threads after an ephemeral port is reused.

### Downloads

Desktop installers and standalone server distributions support macOS, Linux, and Windows on ARM64 and x64. Linux server `.deb` and `.rpm` packages are included for both architectures. Stable desktop updater payloads and signatures use the six-target `latest.json` manifest.

On macOS, copy BiBCode.app from the DMG to Applications before launching it. macOS bundles remain ad-hoc signed and unnotarized; Windows installers remain without Authenticode.

**Full Changelog**: https://github.com/mubeda/BibCode/compare/v0.8.2...v0.9.0

## [v0.8.2] - 2026-10-07

BiBCode v0.8.2 makes the left panel easier to read with lifted project and repository cards, adds projects from the Repositories view on any environment, keeps repositories in name order, lets New Branch start from any local or remote branch without switching to it, warns before targeting the main branch in a pull or merge request, and shows progress while a chat panel opens on a slow remote server. This release includes the changes since v0.8.1.

### Left panel

- Projects (Environments view) and repositories (Repositories view) are now lifted cards on a recessed list background. Inside a repository card each environment checkout sits on its own shaded band, and chats are borderless rows with a clearer fill for the open one. Dark mode separates the layers much more strongly and brightens secondary text on the cards so paths and ages stay readable.
- The Repositories view sorts repositories by name by default, so cards keep their place as chats update. Its sort menu offers **Sort repositories** (Name, Last user message, Created at), separate from the Environments view's project sort.
- The Repositories view has an **Add project** button in its header and empty state. Because that view hides the environment rail, Add Project now shows its **Host** selector on macOS and Linux desktop whenever saved remote servers exist, so you pick the target environment in the dialog.
- **Create new project** gains **Browse…** for its parent folder: the native picker on this device, or the selected remote server's directory browser, returning to the form with the project name kept.

### Git

- **New Branch** has a **From** field that searches every local and remote branch; it defaults to the checked-out branch, the current HEAD when detached, or the commit you opened it from in History.
- **Check out after creating** is on by default. Turn it off to only create the branch: the checkout and working tree stay on the branch you were on.
- A new branch never tracks its source, so a branch created from `origin/main` publishes under its own name instead of pulling from and pushing to `main`.
- Choosing `main`, `master`, or the repository's default branch as the target of a pull or merge request shows a red warning naming the branch. Creation stays available.

### Chat panels

- Opening a chat panel on a remote server with latency no longer shows an empty tab while the server creates the chat. The panel shows **Opening chat…** until the chat arrives; a failed creation still closes the panel with an error.

### Known limitations and qualification still in progress

- The new panel layout and dialogs were verified in the web client in light and dark themes; native packaged visual validation on macOS, Windows, and Linux has not been run yet.
- Native qualification for remote updates ([#16](https://github.com/mubeda/BibCode/issues/16)), the typography audit ([#27](https://github.com/mubeda/BibCode/issues/27)), the light/dark screenshot matrix ([#29](https://github.com/mubeda/BibCode/issues/29)), and the manual SSH check on a real Mac ([#38](https://github.com/mubeda/BibCode/issues/38)) remain open.
- End-to-end-encrypted file transfers ([#18](https://github.com/mubeda/BibCode/issues/18)) remain deferred.

### Downloads

Desktop installers and standalone server distributions support macOS, Linux, and Windows on ARM64 and x64. Linux server `.deb` and `.rpm` packages are included for both architectures. Stable desktop updater payloads and signatures use the six-target `latest.json` manifest.

On macOS, copy BiBCode.app from the DMG to Applications before launching it. macOS bundles remain ad-hoc signed and unnotarized; Windows installers remain without Authenticode.

**Full Changelog**: https://github.com/mubeda/BibCode/compare/v0.8.1...v0.8.2

## [v0.8.1] - 2026-10-06

BiBCode v0.8.1 lets you set draft, people, labels, milestone, and merge options when creating a pull or merge request, pastes clipboard images into terminal programs on any host, and fixes terminal copy and browser discovery behind the UI proxy. This release includes the changes since v0.8.0.

### Pull and merge request options

- The create dialog now offers **Mark as draft**, assignees with **Assign to me**, reviewers, a milestone, and labels on GitHub and GitLab. GitLab merge requests also get **Delete source branch** and **Squash commits**, prefilled from the project's settings; a project that always or never squashes shows the box locked with a note. GitLab takes one assignee and one reviewer, GitHub several.
- Pickers search the host as you type. A picker or settings read that fails offers **Retry** and never blocks creation; untouched merge boxes then leave the decision to the project.
- When GitHub creates the pull request but cannot apply an option (for example a reviewer without access on github.com), the request is reported as created with a warning naming what was not applied, and it is never retried. When a request already exists for the branch, the dialog says your chosen options were not applied.
- Provider failures show the provider's own message. GitLab request bodies travel in a private temporary file instead of on the command line.
- The dialog brings the outcome into view when the request settles. Azure DevOps and Bitbucket keep the plain dialog, and older servers without `pullRequestCreateOptions` show no new fields.

### Terminal

- Pasting a screenshot or other clipboard image into a terminal running Codex or Claude Code now attaches it, including on remote, WSL, and SSH environments. The image is uploaded to the terminal's server, saved there, and its path is pasted. A status with **Cancel** shows during the upload, typed input waits behind the path, and older servers print a message instead of sending an empty paste.
- Selections in terminals running Codex, opencode, or other mouse-tracking programs no longer disappear on the next hover or right-click, so keyboard and context-menu copy work. The desktop selection menu gains **Copy**.

### Connection

- A browser primary behind the loopback UI proxy now discovers and stores its server through the same authenticated HTTP routing it signs in with. The WebSocket endpoint, the desktop-managed bypass, and non-loopback access are unchanged.

### Agent workflow

- Added the `issue-delivery` skill and a CI-checked delivery checker for agent issue batches, and agents now confirm a delegated reviewer is running before waiting on it.

### Known limitations and qualification still in progress

- Create options have not yet been exercised against live GitHub and GitLab hosts, and terminal image paste is unverified in the macOS and Linux webviews and with Windows servers.
- Native qualification for remote updates ([#16](https://github.com/mubeda/BibCode/issues/16)), the typography audit ([#27](https://github.com/mubeda/BibCode/issues/27)), the light/dark screenshot matrix ([#29](https://github.com/mubeda/BibCode/issues/29)), and the manual SSH check on a real Mac ([#38](https://github.com/mubeda/BibCode/issues/38)) remain open.
- End-to-end-encrypted file transfers ([#18](https://github.com/mubeda/BibCode/issues/18)) remain deferred.

### Downloads

Desktop installers and standalone server distributions support macOS, Linux, and Windows on ARM64 and x64. Linux server `.deb` and `.rpm` packages are included for both architectures. Stable desktop updater payloads and signatures use the six-target `latest.json` manifest.

On macOS, copy BiBCode.app from the DMG to Applications before launching it. macOS bundles remain ad-hoc signed and unnotarized; Windows installers remain without Authenticode.

**Full Changelog**: https://github.com/mubeda/BibCode/compare/v0.8.0...v0.8.1

## [v0.8.0] - 2026-10-05

BiBCode v0.8.0 adds a Repositories view to the left panel, keeps remote chats from hanging in Working, and fixes terminal copy, project collapse, and the merge-request flow. This release includes the changes since v0.7.4.

### Repositories view

- The left panel has an Environments | Repositories toggle. The Repositories view groups projects from every connected environment under one read-only card per Git repository, with a card per environment that keeps all of the project's normal actions. The environment rail is hidden in this view, and an empty view links back to Environments.
- Each card collapses independently and the primary local environment is listed first. The selected view and group expansion are remembered.
- The server derives each project's repository identity from its `origin` remote and publishes it with the project. Credentials, query strings, and fragments are stripped from the stored remote URL. Identities are reconciled after project creation or a workspace move, at startup, and on catalog scans, when `.git/config` changes or at most every five minutes otherwise. The work runs off the catalog refresh lock, with bounded Git reads.
- Composer drafts stay reachable when a project gains a repository identity.

### Chat reliability

- A Claude message that starts with a slash command or skill (for example `/superpowers:receiving-code-review …`) no longer hangs in Working. The Claude CLI echoes these messages in their expanded command form, which BiBCode now recognizes as the delivery acknowledgement.
- A Claude message that is never acknowledged no longer holds its thread forever. After 60 seconds it is marked uncertain, so you can retry or dismiss it, and later messages can go through.
- Stop now works when a provider stops responding. If a provider process BiBCode owns has not settled the turn within 10 seconds, BiBCode ends its session, marks the turn interrupted, and keeps the conversation so the next message resumes it in a new process. An OpenCode session on an external server is only asked to abort, since BiBCode can't end that server's work.
- Messages queued for a deleted thread are dismissed instead of being retried forever, and server startup no longer retries sessions of deleted threads.

### Git, terminal, and interface fixes

- Push + MR and MR now both push first and then open the shared pull/merge-request review dialog. The source defaults to the pushed branch, and remote branches are listed as sources. The dialog asks you to publish when the branch is not on the remote yet.
- Ctrl+C copies text selected in AI terminal chats even when the CLI enables mouse tracking. A plain drag selects text, and Shift+drag (Option+drag on macOS) sends the mouse to the CLI.
- Collapsing a project now hides its discovered-worktree section and primary card.
- Git branch footer labels stay inside their controls. Connection notices give guidance on server availability and network reachability instead of raw fetch errors.

### Validation and CI

- Registered manual dispatch for the visual and remote-UI release qualification workflows. These remain registration-only on `main`.

### Known limitations and qualification still in progress

- Native qualification for remote updates ([#16](https://github.com/mubeda/BibCode/issues/16)), the typography audit ([#27](https://github.com/mubeda/BibCode/issues/27)), the light/dark screenshot matrix ([#29](https://github.com/mubeda/BibCode/issues/29)), and the manual SSH check on a real Mac ([#38](https://github.com/mubeda/BibCode/issues/38)) remain open.
- End-to-end-encrypted file transfers ([#18](https://github.com/mubeda/BibCode/issues/18)) remain deferred.

### Downloads

Desktop installers and standalone server distributions support macOS, Linux, and Windows on ARM64 and x64. Linux server `.deb` and `.rpm` packages are included for both architectures. Stable desktop updater payloads and signatures use the six-target `latest.json` manifest.

On macOS, copy BiBCode.app from the DMG to Applications before launching it. macOS bundles remain ad-hoc signed and unnotarized; Windows installers remain without Authenticode.

**Full Changelog**: https://github.com/mubeda/BibCode/compare/v0.7.4...v0.8.0

## [v0.7.4] - 2026-10-05

BiBCode v0.7.4 improves message recovery, attachment uploads, Git workflows, and remote-update feedback. This release includes the changes since v0.7.2.

### Chat, attachments, and session recovery

- Large chat attachments use server staging with sequential upload progress, verified completion, and turn-bound attachment ownership. Upload cancellation restores the prompt and attachments; navigation and an uncertain admission preserve the original attempt instead of silently duplicating a send.
- Retrying a message after its provider session loses resumable state can start a new conversation and explains that outcome. Intentional session stops have distinct feedback from lost connections.
- Provider delivery settlement fences late events and publications after workspace loss, keeping stopped sessions from being projected as running again. The conservative provider idle policy and its recovery after sends without a turn, turn-less sessions, and failed turns are now documented explicitly.

### Git, projects, and interface fixes

- Clone parent folders can be browsed on the selected server. Add Project waits for host platform information and preserves its entered data while metadata loads.
- Automatic fetch attaches again when an unavailable Git repository is repaired.
- Hosting CLI launch failures retain their category and recovery guidance in request dialogs. Diagnostics distinguish unavailable timing from measured short operations without exposing private launch context.
- Git Manager keeps selected rows aligned with the opened diff, preserves selection through unchanged status refreshes, and shares remaining Changes-panel space between its list and diff. Settings uses the height available above the status bar.
- Side-by-side image diffs use each image's natural dimensions within the preview pane, avoiding one-pixel thumbnails.
- Expanded the readable text-size floor across chat, Git Manager, Settings, sidebar, and other controls. Complete native visual and typography qualification remains tracked below.

### Remote updates and protocol reliability

- Remote-server updates use shared named confirmation from Settings and sidebar, advisory running-work counts, download/protection/restart progress, actionable failure and Retry, and operator instructions for manual hosts. Host notices identify the requester; browser reload prompts preserve unsent input.
- Desktop primary HTTP requests share the bridge-owned bearer cache, so backend restart invalidates old credentials; a late failed exchange cannot clear its replacement.
- The orchestration HTTP snapshot now uses its declared client read model with full thread details and archived/deleted markers. Shared Rust serializers preserve existing WebSocket output; a populated cross-language fixture guards decoding.
- Heartbeat regressions distinguish controlled scheduling cadence from real-connection survival, and Git status overflow retry has cross-language contract coverage.

### Runtime, packaging, and validation

- Native process sampling starts lazily instead of scanning all host processes on every server construction; requested diagnostics and owned-process cleanup retain on-demand discovery.
- Automated provider, hosting, Git, SSH, and desktop fixtures isolate user configuration and credentials. The hermetic guard rejects host provider/hosting executable resolution and credential reads; Unix test-child watchdogs reap owned descendants after parent interruption or death.
- Desktop shutdown-race tests observe the exact runtime join. SSH fixtures retain explicit cleanup and loopback behavior across native platforms; delayed-interpreter watchdog fixtures establish their intended TERM immunity before interpreter startup.
- Packaged composer fixtures advertise the native Claude skill catalog consistently. Queue smoke checks verify visual FIFO independently of temporary virtualized DOM slot order, retaining reload, steering, and exact provider-input checks.
- Windows upgrade evidence records a timed-out version probe as unavailable after verified child cleanup, preserving the overall installer deadline and strict installed-version checks.
- Release preflight explains workflow-token permission mismatches before native builds. Cold macOS Intel CI and seeded-build budgets account for the complete required jobs, preserving runtime assertions and all supported platform rows. Both macOS release jobs verify Finder's rendered icon from their exact mounted DMG payload before upload.
- Intel desktop and server distributions build on supported macOS 15; desktop installers retain the same signature and Finder pixel checks. Native macOS 26 Intel runtime validation remains separate. macOS 15 verification does not establish hosted macOS 26 Intel Finder qualification.
- AppImage packaging reports unsupported GdkPixbuf loader layouts with actionable guidance. Native Windows trust-command coverage, living procedures, and issue-specific evidence reports were expanded.

### Known limitations and qualification still in progress

- The remote-update feature is integrated ([#16](https://github.com/mubeda/BibCode/issues/16)); native qualification remains incomplete. The previous-stable Windows qualification did not establish the installed candidate, and observation hardening is separate from an actual installation pass.
- The remaining typography audit ([#27](https://github.com/mubeda/BibCode/issues/27)), complete light/dark screenshot matrix ([#29](https://github.com/mubeda/BibCode/issues/29)), and required manual SSH check on a real Mac ([#38](https://github.com/mubeda/BibCode/issues/38)) remain open. Source and hermetic tests do not establish those native scenarios as passed.
- End-to-end-encrypted file transfers ([#18](https://github.com/mubeda/BibCode/issues/18)) remain deferred and are excluded from this release.
- The supplementary throttled browser immediate-close profile for legacy whole-request attachment delivery remains unqualified. API-side queue completion does not guarantee receiver delivery; staged uploads require their normal completion/admission flow.
- The desktop restart guard retains its documented check-then-exit limitation around an update admitted concurrently with a manual restart.

### Downloads

Desktop installers and standalone server distributions support macOS, Linux, and Windows on ARM64 and x64. Linux server `.deb` and `.rpm` packages are included for both architectures. Stable desktop updater payloads and signatures use the six-target `latest.json` manifest.

On macOS, copy BiBCode.app from the DMG to Applications before launching it. macOS bundles remain ad-hoc signed and unnotarized; Windows installers remain without Authenticode.

**Full Changelog**: https://github.com/mubeda/BibCode/compare/v0.7.2...v0.7.4

## [v0.7.2] - 2026-10-01

BiBCode v0.7.2 restores skill discovery across AI providers and fixes request
creation, terminal clipboard shortcuts, file-tree refreshes, and folder downloads.

### AI skills and provider configuration

- The Chat AI command menu discovers skills for the active workspace and provider
  instance, including user-level skills outside the repository. Codex, Claude,
  Cursor, and OpenCode use their effective user configuration instead of an
  environment-wide catalog discovered from the server's own directory.
- Switching workspaces or provider configuration cannot display an older
  workspace's catalog. Loading, partial discovery, and failures have feedback
  and Retry preserves the draft.
- Cursor discovers nested and linked skills across its supported roots. Claude
  skill visibility and invocation aliases are preserved. OpenCode requests use
  the workspace directory and retain skill classification.
- Configured sensitive environment values reach native provider launch and
  discovery without being mistaken for redacted placeholders.

### Pull and merge requests

- Every creation entry point opens the shared review dialog. The target starts
  unselected and must be chosen explicitly before any commit, push, or request
  creation; typing a branch name alone cannot enable the action.
- Source and target dropdowns show only fetched branches from origin, including
  remote branches that also have a local copy. Local-only branches, other
  remotes, and symbolic HEAD entries are excluded. Use Fetch to refresh the list.
  An unpublished checkout is not preselected for ordinary request creation.
- Selecting another source keeps the current checkout and working files intact.
  Title and description suggestions come from that source's tip commit, rather
  than the newest commit anywhere in the repository. Edited content survives
  source changes, and changing source requires a new target selection.
- The server receives the exact reviewed branches, rejects invalid or identical
  choices before mutation, and never substitutes master or main as the target.
  Selected local sources publish with an exact origin refspec without force or
  tags; origin-only sources need no push.
- Combined commit/push/request actions explain their fixed source and reread Git
  before retrying, preserving completed commits and generated branches. Changed
  sources require review again; existing requests are reused instead of duplicated.
- Older servers that cannot honor source and target choices disable creation
  with update guidance, including after reconnect. Plain Push remains separate
  from request creation.

### Terminals and files

- Unix PTY launches preserve Rust's spawn handshake. Failed executable or
  interpreter launches report an error instead of returning false success and
  aborting the child process.
- Ctrl+C copies selected AI-terminal output and Ctrl+V pastes through the
  terminal's native clipboard handling. Ctrl+C without selection still interrupts.
  Selecting text keeps keyboard focus; Add to chat is available on right-click.
- Files refreshes no longer crash with “Unknown directory child index” when
  an entry changes between a file and a folder. Surviving selection and focus
  remain usable after the refresh.
- Folder-download failures retain their reason and recovery guidance instead
  of showing only a generic error. Sanitized diagnostics identify client,
  desktop, and server failure stages without exposing signed download URLs.
- Desktop downloads mint their access token after destination selection, so
  waiting in the picker does not consume the token lifetime. Failed archive
  production rejects the download stream, and failed desktop streams remove
  partial files instead of saving an incomplete ZIP as a successful download.

### Validation and documentation

- Added behavioral regressions for workspace/user skill catalogs, provider
  configuration, remote request selectors, reconnect compatibility, partial
  retries, native clipboard behavior, tree transitions, and download failures.
- Updated provider, architecture, source-control, observability, and native
  validation documentation to describe the supported behavior and diagnostics.

### Downloads

Desktop installers and standalone server distributions are provided for macOS,
Linux, and Windows on ARM64 and x64. Linux server `.deb` and `.rpm` packages are
included for both architectures. Stable desktop updater payloads and signatures
are available through `latest.json`.

On macOS, copy BiBCode.app from the DMG to Applications before launching it.

**Full Changelog**: https://github.com/mubeda/BibCode/compare/v0.7.1...v0.7.2

## [v0.7.1] - 2026-09-29

BiBCode v0.7.1 adds remote branch checkout to Git Manager and improves
switching branches with uncommitted work.

### Git Manager

- The branch picker searches fetched remote branches alongside local
  branches, without regard to case. Select `origin/develop`, for example,
  to create and check out a local `develop` branch that tracks it. Use
  **Fetch** to discover branches added on the remote.
- Remote branches appear in their own group, with checkout as their action.
  Existing local branches are preserved; if the local name already exists,
  the error explains how to select or rename it. Local and remote rows stay
  distinct even when their displayed names match.
- **Bring my changes** carries non-conflicting edits while preserving Git's
  overwrite protection, occupied-worktree checks, and operation-in-progress
  guards. **Leave my changes** saves a stash before switching. Failed
  checkouts show their error inside the changes dialog and allow retry.
- Partial staging of new files works when Git is configured to use mnemonic
  diff prefixes or omit prefixes. Git Manager now requests a consistent
  patch format, preserving unselected file contents through partial stage,
  unstage, and discard operations.

### Diffs and commit messages

- Review diffs retain their file names when Git uses mnemonic, omitted, or
  custom diff prefixes. Both review sources and Git Manager request the
  same canonical patch format without changing your Git configuration.
- Generated commit messages name the changed files correctly with those
  Git display settings, instead of falling back to a generic subject.

### Connections

- Heartbeat scheduling no longer accumulates delays when timers wake
  slightly late.

### Release reliability and contributor documentation

- CI and release verification now run package test tasks one at a time as
  intended, keeping Rust compilation from competing with running suites.
  The full CI test job has enough time for both Rust passes, SSH integration,
  and cleanup without shortening any checks. Native CI also allows cold
  macOS Intel builds to finish host tests, recovery probes, and packaging.
- SSH tunnel test fixtures use numeric loopback without reverse-DNS lookups,
  making their readiness independent of the build host's DNS configuration.
- Claude hook tests wait for server admission before toggling activity,
  removing a timing assumption from stale-request checks.

- Release, flaky-test diagnosis, and Linux desktop-validation procedures
  now live in shared repository documentation for Codex and Claude Code.
  Automated checks keep the two copies of the release skill synchronized.

### Downloads

Desktop installers and standalone server distributions are provided for
macOS, Linux, and Windows on ARM64 and x64. Linux server `.deb` and `.rpm`
packages are included for both architectures. Stable desktop updater
payloads and signatures remain available through `latest.json`.

On macOS, copy BiBCode.app from the DMG to Applications before launching it.

**Full Changelog**: https://github.com/mubeda/BibCode/compare/v0.7.0...v0.7.1

## [v0.7.0] - 2026-09-27

BiBCode v0.7.0 makes remote work more reliable: clones survive reconnects,
slow connections can finish loading, and shared terminals stay readable
across windows. The release also brings workspace cards to the left panel,
faster Git updates, clearer message-delivery errors, and Linux desktop and
update-recovery fixes.

### Chat and agents

- Failed and uncertain messages explain what happened, name the provider
  instance, and say how **Retry** and **Dismiss** affect the queue. A refused
  model or option fails once instead of retrying forever; its notice offers
  **Dismiss** and explains how to send a corrected message. Later messages
  say **Waiting for an earlier message** instead of offering a **Send now**
  that cannot work.
- A queued turn that starts as the previous turn finishes is no longer cut
  off by the previous turn's idle timeout. If a workspace disappears during
  a turn, the conversation stops showing **Working**, keeps the partial
  reply, and holds queued messages for **Send now** after the workspace
  returns.
- Provider instances use the same name in Settings, model pickers, chat
  headers, context meters and delivery notices. Partially configured
  providers no longer appear ready in Settings but disabled when sending;
  Cursor is enabled by default unless you explicitly turned it off. A
  disabled provider no longer claims to be installed before it is checked.
- Codex custom models accept their configured reasoning effort, and Claude
  Fast Mode follows the selected model's capabilities, including aliases.
  A later multi-select question keeps your selections instead of answering
  itself after the first pick.
- The work log uses plain titles such as **Session ready** and **Turn
  completed**, names tool steps, and shows provider warnings with their
  explanation. Context-window and question-card text is easier to read.

### Workspace and terminals

- The left panel groups the primary checkout and worktrees into outlined
  workspace cards, showing status, branch, pull or merge request, local
  changes, terminals, model, age and other chats. It opens wider on a first
  launch while respecting smaller windows and your saved width.
- Card and project menus group their actions and add **Pull**, **Copy Branch
  Name**, **New Worktree…** and **Show Hidden Worktrees (N)**. Cards and
  in-app menus support keyboard navigation; disabled menu items remain
  reachable so you can read or hear why they are unavailable.
- When several windows share a terminal, the window you use controls its
  size and the others mirror it without garbling the screen. **Sized for
  another window** offers **Fit to this window**. Reopening or reloading a
  terminal no longer replays terminal-query replies into your command line.
- Tabs visibly mark their selection, and the command palette highlights
  the action Enter will run after filtering. **Retry** keeps keyboard focus
  while it works, explains when it is waiting for a connection, and cannot
  start duplicate reads while busy.

### Git Manager and source control

- Commits, branches, tags, stashes and fetches made from another terminal,
  including BiBCode's terminal panel, refresh the Git Manager promptly.
  History decorations stay current, duplicate refreshes are reduced, and
  long stash lists scroll inside their panel.
- **Clone from URL** no longer stops after 30 seconds and keeps running on
  the server through a dropped connection. The dialog reconnects to the
  same clone, and **Cancel clone** reaches the server after reconnection.
  Failed or cancelled clones clean up the folder they created; retries
  refuse incomplete earlier clones instead of adding a broken project.
- Git transfers allow time for large repositories and report stalled HTTP
  transfers clearly. Repositories without a commit show **No commits yet**
  instead of **Detached HEAD**.
- Changes, History, Tags and the primary workspace card explain whether a
  folder has no repository, damaged Git metadata, or an ownership trust
  problem. Unavailable actions show the same reason, and trust commands
  quote the folder for the server's shell. Repairing HEAD or config is
  noticed automatically, without leaving stale branches or errors behind.
- The chosen tab survives Git failures and reconnects. **Retry** stays busy
  for the checkout it is reading even when you switch tabs or worktrees;
  a failed History refresh keeps the commits already loaded.
- **Delete Worktree…** refuses while a session in that checkout is running
  or starting, including one started by another client. If removal fails
  after stopping idle sessions, their conversation context remains available
  for the next message.
- Self-hosted GitLab instances are recognised through configured `glab`
  hosts even when their domain has no "gitlab" in it. Pull Requests opens
  and switches repositories faster, GitLab uses **merge request** and `!N`,
  and create dialogs show the host's real address. While the host is still
  loading, the wording stays **change request**.

### Remote servers and sharing

- Large History pages and diffs can finish over slow connections without
  repeatedly disconnecting. A silent connection is detected and shows why
  BiBCode is reconnecting; a read interrupted twice waits for **Retry**
  instead of downloading the same result in a loop.
- Connecting allows up to two minutes to receive the server's settings
  after the socket is ready, with **Receiving settings from…** progress in
  the remote-server row and connection notices. **Slow requests** now lives
  in the status bar, with request and server names, instead of covering
  content with a persistent toast.
- SSH environments launch and pair correctly, reuse their saved credential
  on reconnect, and restart a server that stopped behind an existing tunnel.
  Remote steps have time limits and actionable errors; pairing work on the
  host is also bounded if the client goes away.
- Hosted pairing links stay open until you choose **Pair this backend**.
  Incomplete links explain what is missing instead of showing unrelated
  connection advice.
- **Rename…** in **Settings → Remote Servers** changes a saved server's
  name on this device without reconnecting it. Headless `bibcode serve`
  defaults to its hostname, and selecting a remote server no longer jumps
  back to Local during startup or background updates. **Share This Host**
  explains when a private default network route is missing.

### Desktop

- The Linux AppImage follows the system light or dark theme at launch and
  during live changes, including its native menus. Explicit Light and Dark
  choices stay in place. System-theme changes also reach every app route,
  and the browser UI served by `bibcode serve` paints its saved theme without
  flashing light first.
- Protected in-app updates work while **Share This Host** is enabled,
  keeping the pre-update backup and shutdown checks in place.
- If an update fails and the local server cannot restart, the app explains
  the failure, including a port already in use, and offers **Restart server**.
  A failed retry also offers **Restart BiBCode**. The downloaded update and
  unsent drafts are retained while the server recovers.
- Restarting an AppImage after an update or from recovery releases the old
  runtime and mount instead of keeping them alive until the app quits.

### Server reliability and performance

- Reconnecting clients reuse recent provider checks for up to five minutes,
  reducing repeated CLI launches and unchanged settings traffic. Manual
  refreshes and settings changes still take effect immediately.
- Git status resumes automatically after a busy client falls behind.
  Long-running subscription retries release their old resources, and event
  recovery reads bounded pages without forwarding events twice. Oversized
  RPC responses fail their own request while keeping the connection usable.
- Interrupted Git commands stop their helper processes too. Terminal commands
  that exit immediately report their exit status reliably, and provider
  helpers briefly retry an executable that is busy being updated. Attachment
  reuse is limited to the conversation that attached it.

### Developer and test infrastructure

- Server and desktop tests isolate provider and hosting CLIs, terminal homes
  and SSH configuration from the developer's machine. Regression coverage
  exercises slow links, SSH reconnects, process cleanup and React Compiler
  behavior, with bounded waits and fewer timing-dependent test failures.

### Downloads

On macOS, copy BiBCode.app from the DMG to Applications before launching it.

Desktop installers and standalone server distributions are provided for macOS,
Linux, and Windows on ARM64 and x64. Linux server `.deb` and `.rpm` packages are
included for both architectures. Stable desktop updater payloads and signatures
remain available through `latest.json`.

**Full Changelog**: https://github.com/mubeda/BibCode/compare/v0.6.2...v0.7.0

## [v0.6.2] - 2026-09-23

BiBCode v0.6.2 fixes terminals and tools on the Linux AppImage. Git, Python and
other system tools work again in BiBCode terminals, and so do Claude Code
status line scripts and the commands your agents run, on Fedora, Ubuntu,
Omarchy and other distributions.

### Fixed

- `git pull`, `git fetch` and other HTTPS Git commands in a BiBCode terminal
  on the Linux AppImage no longer fail with a `libcurl` "symbol lookup error",
  and system tools no longer print `libpcre2` "no version information
  available" warnings. BiBCode was handing the AppImage's own bundled
  libraries and paths to every program it started; terminals, agents and
  tools now get your normal system environment, as they would in an SSH
  session.
- Claude Code status line scripts and other tools that run the system Python
  start normally in BiBCode terminals on the AppImage instead of failing with
  "Failed to import encodings".
- Chat-mode and terminal-mode agents, their probes and helper processes, Git
  and source-control commands, and editors and file managers opened from
  BiBCode also run without AppImage paths or the launcher's forced GTK theme,
  display backend and Python settings. Extracted AppImages
  (`squashfs-root/AppRun`) are covered too.
- Commands started from BiBCode no longer search the current directory for
  libraries when the AppImage launcher left an empty library path.

### Downloads

On macOS, copy BiBCode.app from the DMG to Applications before launching it.

Desktop installers and standalone server distributions are provided for macOS,
Linux, and Windows on ARM64 and x64. Linux server `.deb` and `.rpm` packages are
included for both architectures. Stable desktop updater payloads and signatures
remain available through `latest.json`.

**Full Changelog**: https://github.com/mubeda/BibCode/compare/v0.6.1...v0.6.2

## [v0.6.1] - 2026-09-23

BiBCode v0.6.1 lets you keep typing while an agent is working. Messages you
send during a turn join a durable queue, send themselves in order when the
turn ends, or go straight into the running turn when you steer them. The
release also makes the Linux AppImage use the native Wayland backend, and
hardens provider and terminal recovery after a crash or a restart.

### Message queue

- Send a message while an agent is working and it becomes a queued card in
  the conversation instead of waiting behind a disabled composer. Cards keep
  their order, and the first one starts the next turn automatically when the
  current turn finishes.
- **Steer** on the first queued card delivers it into the turn that is still
  running, on Codex and on Claude, without starting a second turn. **Cancel**
  takes the message back out of the queue and returns its text to the
  composer, attachments included.
- **Stop** drains the queue back into your draft before it interrupts the
  turn, so nothing is sent by surprise and nothing is lost.
- The queue lives on the server, so every window sees the same queue: a
  browser tab, the desktop app and a remote client stay in step, and a reload
  or a server restart never double-sends or drops a queued message.
- Queued messages never send themselves into an approval prompt, a question,
  or a turn that ended in an error or an interruption. Those cards say
  **Waiting for you** and offer **Send now**.
- `Mod+Shift+Enter` steers the first queued message from the composer.

### Fixed

- Sending after a provider process exits mid-turn now starts a new provider
  session instead of failing against the dead connection. This includes
  **Send now** on a queued message that is waiting for you.
- A server restart while a turn was running no longer leaves the conversation
  stuck as if it were still working. The interrupted turn is settled, queued
  messages are held, and **Send now** resumes the conversation.
- Closing or restarting a terminal while it is still opening no longer starts
  the original command a second time. Terminal processes are also spawned off
  the async runtime, so a busy machine cannot stall unrelated work while a
  terminal starts.
- The Linux AppImage prefers the native Wayland backend on a Wayland session
  and falls back to X11 elsewhere. This fixes the window rendering about a
  third too large on Hyprland and Omarchy with fractional scaling (#15). Set
  `BIBCODE_GDK_BACKEND=x11` to force the previous behaviour.

### Downloads

On macOS, copy BiBCode.app from the DMG to Applications before launching it.

Desktop installers and standalone server distributions are provided for macOS,
Linux, and Windows on ARM64 and x64. Linux server `.deb` and `.rpm` packages are
included for both architectures. Stable desktop updater payloads and signatures
remain available through `latest.json`.

**Full Changelog**: https://github.com/mubeda/BibCode/compare/v0.6.0...v0.6.1

## [v0.6.0] - 2026-09-21

BiBCode v0.6.0 adds a Pull Requests module: a project-scoped view, next to
the Git Manager, that lists pull requests and merge requests, opens them,
reviews, edits, merges and checks them out locally, on GitHub and on
self-hosted GitLab.

### Pull Requests

- A **Pull Requests** button on each project header (after Git Manager) opens
  the hosted repository for the selected checkout. The view detects whether it
  can be used and explains what is missing: no `origin`, an unsupported
  provider (Azure DevOps, Bitbucket), a host that the `gh`/`glab` CLI does not
  know (with the exact `auth login --hostname` command), a missing CLI, or a
  host that is not authenticated. **Settings → Source Control → Pull
  requests** turns the module off and lists every configured GitHub and
  GitLab host with its authentication state.
- List with Open/Closed tabs (GitLab: Open, Merged, Closed, All), search,
  author, assignee, reviewer, review-status, draft, label, milestone and
  target-branch filters, sort, and explicit **Load more**.
- Detail page in the shape of the GitHub pull request page: header with
  state, branches and server-computed merge readiness; Conversation with
  comments, reviews, threads, suggestions, reactions and system events;
  Commits; Checks (GitLab: Pipelines); Files changed with lazy diffs,
  **Viewed** state and an ignore-whitespace toggle. The selected tab lives in
  the URL.
- Reviewing: post, edit and delete comments; reactions; reply, resolve and
  unresolve threads; inline comments from a diff line with **Insert
  suggestion**; a pending review submitted as Comment, Approve or Request
  changes against the head you reviewed; dismiss and re-request reviews;
  GitLab approve, revoke, remove your change request and apply suggestions
  with an optional commit message.
- Editing: title, description and base branch; reviewers, assignees, labels
  and milestone through searchable pickers with a five-second **Undo**;
  lock (GitHub reasons) and unlock; convert to draft and mark ready; close
  and reopen.
- Merge box with the methods the repository allows, subject and body,
  delete-branch and auto-merge options, and a confirmation that names the
  method, target and options; bypass of branch requirements with its own
  warning; update branch or rebase when behind; revert a merged request into
  a new one; delete a merge request (GitLab).
- **Checkout** for local verification into the current checkout, another
  worktree or a **New worktree…**; blocked checkouts show the Git Manager's
  reason; a started checkout always completes even if the window closes.
- Every control that the host does not allow stays visible, disabled, with
  the host's reason. Drafts (comments, replies, pending review, title,
  description, merge message) survive navigation and reload. Actions on the
  same request are serialized on the server; permissions are re-read before
  each write; a changed head is reported instead of merged.
- Verified end to end on GitHub and on a self-hosted GitLab 19.3 instance.
  Along the way: `glab` JSON bodies now send an explicit content type, GitLab
  inline notes with subclass ids parse, and repeated CLI probes are memoized
  for thirty seconds so GitLab actions take about half the time.

### Git Manager and Source Control

- Worktree toolbars no longer show **Loading worktrees…** after the list has
  loaded.
- Worktrees created from a pull request use the head branch name, or
  `<head>-pr-<n>` when that branch is already checked out elsewhere.

### Server

- A supervised process spawn retries briefly while the executable is still
  busy being written, and fixture capture reads wait for content (test-only
  robustness on hosted runners).

### Downloads

On macOS, copy BiBCode.app from the DMG to Applications before launching it.

Desktop installers and standalone server distributions are provided for macOS,
Linux, and Windows on ARM64 and x64. Linux server `.deb` and `.rpm` packages are
included for both architectures. Stable desktop updater payloads and signatures
remain available through `latest.json`.

**Full Changelog**: https://github.com/mubeda/BibCode/compare/v0.5.13...v0.6.0

## [v0.5.13] - 2026-09-18

BiBCode v0.5.13 shows where pending changes live in the Source Control panel,
narrows Files-panel upload tokens to a single file, and makes download and
upload names behave on every platform.

### Source Control

- Pending changes are grouped by folder inside each section. A folder row
  shows the full relative directory (shortened from the start so the deepest
  part stays readable), a file count, a collapse chevron, and a checkbox that
  stages, unstages, or selects every file in that folder in one request. File
  rows underneath show only the name and its diff stats.
- A **Group by folder / Flat list** toggle next to the section actions is
  remembered per user. The flat list keeps file names visible and shortens
  only the directory.

### Git Manager

- **New branch** now forks from the branch you have checked out (or the
  current HEAD commit when HEAD is detached) instead of the repository's
  default branch, and the dialog shows that base branch in a highlighted chip
  before you confirm.

### Files panel transfers

- An upload URL is now bound to one file name; a leaked upload link can no
  longer write any other name in that folder, and a request that names a
  different file is refused.
- Download names with non-ASCII characters arrive intact in browser mode
  (`Content-Disposition` carries an RFC 5987 encoded name), and a name that
  previously broke the response no longer does.
- Upload names are validated for the filesystem that stores them: Windows-only
  rules (reserved names, `<>:"|?*`, trailing dots or spaces) apply only when
  the server runs on Windows, so Linux and macOS workspaces keep accepting
  their legal names. Names up to 255 bytes upload; the desktop host shortens
  an over-long download name instead of refusing it, and on Windows renames a
  download that Windows could not store, reporting the saved path.
- A refused upload now names the file in its message.

### Desktop

- macOS builds stay ad-hoc signed; the README and the release runbook explain
  that folder and network permissions are asked again after each update.

### Downloads

On macOS, copy BiBCode.app from the DMG to Applications before launching it.

Desktop installers and standalone server distributions are provided for macOS,
Linux, and Windows on ARM64 and x64. Linux server `.deb` and `.rpm` packages are
included for both architectures. Stable desktop updater payloads and signatures
remain available through `latest.json`.

**Full Changelog**: https://github.com/mubeda/BibCode/compare/v0.5.12...v0.5.13

## [v0.5.12] - 2026-09-17

BiBCode v0.5.12 adds Download and Upload to the Files panel for local and
remote environments, gives the Git Manager a Tags tab with push options, and
fixes four left-panel and Git Manager presentation problems.

### Files panel

- **Download** on a right-clicked file saves that file; on a folder it saves a
  `.zip` of the folder. The desktop app asks for a destination folder and never
  overwrites an existing file (`name (2).ext`); a browser uses its own
  download location. Folders over 2 GiB or 200,000 entries are refused with a
  message before the download starts; symbolic links and special files are
  skipped, ignored files are included.
- **Upload Files…** on a folder row, or on the tree background for the
  workspace root, opens a file picker and uploads the chosen files into that
  folder. It is disabled on file rows with the reason in the label. A file
  that already exists asks before it is replaced; a failure names the file and
  the rest of the batch still uploads. Files up to 1 GiB each are accepted.
- Both actions work for remote environments. Transfers stream over two new
  token-authenticated routes on the server that owns the workspace
  (`/api/transfers/{token}`, five-minute signed tokens bound to one file,
  folder, or upload directory), so they work on every connection profile.
  After an upload the file tree and the Git status refresh at once.

### Git Manager tags

- A **Tags** tab after Changes and History lists local tags and, per remote,
  the tags that remote advertises, in collapsible sections whose state is
  remembered per project. Remote tags are queried when the tab opens and on
  its refresh button; rows are marked when a tag is missing locally or points
  elsewhere, and an unreachable remote shows its reason with Retry.
- Creating a tag offers **Push to `<remote>` after creating**; if that push
  fails the tag still exists locally and the failure says so.
- Push, publish, and force push confirm in one dialog with **Also push tags**,
  which sends every local tag with the branch in one atomic push.
- Annotated tags are compared by the commit they point to on both sides, so a
  tag that matches the remote is no longer reported as differing.

### Left panel and Git Manager

- A selected project node stays selected in the left panel until another
  thread is opened.
- The Git Manager toolbar shows the connected environment next to the project.
- The merge dialog's **Merge commit** option is the default and stays visibly
  selected.
- A thread that runs in its own worktree shows its branch as a muted label
  after its title instead of a folder icon that looked like the project's
  "New worktree" action.

### Server

- Asset and transfer URL tokens share one signed-token module with separate
  purposes, so a token minted for one can never be redeemed as the other.
- The route inventory used by the maintenance audit now lists the transfer
  routes.

### Downloads

On macOS, copy BiBCode.app from the DMG to Applications before launching it.

Desktop installers and standalone server distributions are provided for macOS,
Linux, and Windows on ARM64 and x64. Linux server `.deb` and `.rpm` packages are
included for both architectures. Stable desktop updater payloads and signatures
remain available through `latest.json`.

**Full Changelog**: https://github.com/mubeda/BibCode/compare/v0.5.11...v0.5.12

## [v0.5.11] - 2026-09-16

BiBCode v0.5.11 makes the left panel follow Git Manager branch changes at
once and makes the active host and the managed project visible at a glance.

### Left panel and Git Manager

- The project row's branch label updates as soon as a Git Manager or
  source-control mutation finishes, or a terminal command exits, instead of
  waiting for the passive status summary's 30-second cycle.
- Worktree rows follow a Git Manager checkout, branch create, rename, delete,
  pull, merge, rebase, cherry-pick, revert, or reset without waiting for the
  worktree catalog's polling interval.
- The Git Manager shows which project it manages: the project's header row
  stays highlighted in the left panel while the manager is open, and the
  toolbar leads with the project's name, with its checkout path on hover.

### Environment rail

- The selected host is framed by a primary-colored ring and unselected hosts
  are muted, so the active host is unambiguous next to the panel separator.

### Downloads

On macOS, copy BiBCode.app from the DMG to Applications before launching it.

Desktop installers and standalone server distributions are provided for macOS,
Linux, and Windows on ARM64 and x64. Linux server `.deb` and `.rpm` packages are
included for both architectures. Stable desktop updater payloads and signatures
remain available through `latest.json`.

**Full Changelog**: https://github.com/mubeda/BibCode/compare/v0.5.10...v0.5.11

## [v0.5.10] - 2026-09-15

BiBCode v0.5.10 makes Git Manager merges keep their promises, detects and
launches Zed as an external editor, lets Add Project browse remote host
folders, and fixes Create Worktree and terminal input problems.

### Git Manager merges

- **Merge commit** always records a merge commit. It never fast-forwards and
  overrides repository settings such as `merge.ff` or branch merge options
  that would skip the commit or squash it.
- **Squash merge** now works in repositories that set `merge.ff=false`,
  `merge.ff=only`, or a branch `--commit` merge option; Git previously
  rejected or aborted the squash.
- The bypass-hooks option of the squash-merge operation now applies to the
  squash commit itself.
- The merge dialog disables Merge and says **Nothing to merge** when the
  source branch has no commits the current branch lacks.

### External editors

- **Open in Zed** now appears when Zed is installed. Zed is detected on `PATH`,
  through the `zeditor` alias, a Flatpak export (`dev.zed.Zed`),
  `~/.local/bin`, the macOS app bundle CLI, and the Windows per-user install
  directory.
- Every external editor is resolved through one shared server catalog.
  Detection runs on the server that owns the environment, so a remote
  environment lists the editors installed on that remote host.
- Flatpak editors are launched with access to the opened file's directory, so
  projects outside the home directory open without extra Flatpak overrides.

### Add Project on remote hosts

- Remote hosts, and browser clients without a native dialog, open a folder
  browser that lists the selected host's directories; **Type a path instead**
  switches to manual entry of an absolute or home-relative path. Local and WSL
  locations keep the native folder picker.
- The Settings remote directory picker uses the same browser and stays stable
  through the dialog's exit transition.

### Create Worktree

- The **Reuse branch** control is visible whenever a branch is selected. It is
  enabled for a free local branch and disabled, with an explanation, for a
  remote branch or a branch already checked out elsewhere.
- A branch list that fails to load is reported in the dialog with a recovery
  hint, and a typed name still creates a new branch.

### Terminals

- `Shift+Enter` inside a terminal sends a soft newline, so Codex and Claude
  Code prompts insert a line break instead of submitting; plain `Enter` still
  submits.

### Release engineering

- Raised the publication preflight budget to 60 minutes after v0.5.9 approval
  runs reached the previous 30- and 45-minute limits while the full test graph
  was still running. Every gate stays enabled, and the workflow contract and
  release runbook record the measured limits.

### Downloads

On macOS, copy BiBCode.app from the DMG to Applications before launching it.

Desktop installers and standalone server distributions are provided for macOS,
Linux, and Windows on ARM64 and x64. Linux server `.deb` and `.rpm` packages are
included for both architectures. Stable desktop updater payloads and signatures
remain available through `latest.json`.

**Full Changelog**: https://github.com/mubeda/BibCode/compare/v0.5.9...v0.5.10

## [v0.5.9] - 2026-09-11

BiBCode v0.5.9 fixes Git network operations from the Linux AppImage and adds
repeatable compatibility coverage for Debian, Ubuntu, Fedora, and Arch.

### Linux Git compatibility

- Fixed **Fetch origin** and other system Git operations failing from the Linux
  AppImage with `symbol lookup error` when Fedora's `libcurl` loaded an
  incompatible `libnghttp2` bundled by the AppImage.
- Isolated Git and source-control helper processes from both the current
  AppImage library directory and stale `.mount_*` directories inherited after
  application updates. This applies to text and binary-output Git commands.
- Preserved unrelated host library directories, credential and SSH-agent
  settings, command-local environment overrides, and Linux loader semantics for
  current-directory entries. The desktop process environment remains unchanged.
- Kept ordinary non-AppImage server launches unchanged and avoided
  distribution-specific library paths, so the fix uses each host's own system
  Git dependency set.

### Validation and supported distributions

- Added a production-runner regression that builds an intentionally
  incompatible shared library, proves the system Git HTTPS helper fails when it
  loads that library, and verifies the patched runner restores normal helper
  initialization without contacting an external Git server.
- Added CI coverage using system Git on Debian 12 and 13, Ubuntu 22.04 and
  24.04, Fedora 44, and Arch rolling. Coverage includes extracted AppImages,
  paths containing spaces, stale mounts, colon and semicolon separators, and
  parent-environment isolation.
- Reproduced the original Fedora failure and verified a patched dry-run fetch
  against the affected repository completed without a loader error or changing
  refs or `FETCH_HEAD`.
- Updated the Linux testing runbook, runtime architecture, CI reference, and
  script reference to define the cross-distribution Git compatibility contract
  and distinguish it from complete packaged desktop qualification.

### Downloads

On macOS, copy BiBCode.app from the DMG to Applications before launching it.

Desktop installers and standalone server distributions are provided for macOS,
Linux, and Windows on ARM64 and x64. Linux server `.deb` and `.rpm` packages are
included for both architectures. Stable desktop updater payloads and signatures
remain available through `latest.json`.

**Full Changelog**: https://github.com/mubeda/BibCode/compare/v0.5.8...v0.5.9

## [v0.5.8] - 2026-09-08

BiBCode v0.5.8 improves remote terminal responsiveness and makes AI usage status
follow the server you are working on.

### Remote terminal responsiveness

- Added capability-negotiated, ordered terminal input. Updated clients can send
  subsequent keystrokes before the previous write's reply, reducing the extra
  queue delay on remote connections. A controlled 120 ms round-trip test reduced
  median time queued before sending from about 59 ms to under 1 ms. Network
  round-trip latency still affects when remote output appears.
- Added bounded input windows and reserved RPC capacity for control operations,
  including when terminal traffic competes with persistent subscriptions.
  Unicode paste and control sequences retain their delivery order.
- Bound each input lease to its physical connection and exact terminal process.
  Failed, duplicate, incomplete, disconnected, or stale input cannot be replayed
  into a replacement process. A delayed old attachment request cannot replace
  a newer attachment's lease.
- Added **Reconnect input** after delivery failures. It reattaches to the
  existing agent process without restarting it or replaying discarded text.
- Fixed input preparation after open, reopen, restart, and fresh attachment so
  immediate programmatic writes work before the terminal renderer arrives.
  Visual panel hiding and moving preserve the existing input binding.
- Older servers retain the existing serialized input path. Update both the
  desktop client and remote server to activate ordered input; failures on the
  new path never silently retry through the old path.

### Correct AI usage and environment status

- Claude and Codex usage now comes from the environment selected in the rail,
  rather than always using the primary local server.
- Manual/background refresh and usage-reset actions target that same selected
  environment. Late responses remain associated with their original server.
- A loading or disconnected remote server no longer borrows local account
  usage. Terminal counts and remote diagnostics also follow the selection,
  while the separate local desktop diagnostic source remains available.

### Reliability and validation

- Fixed newly active center panels remaining blank while a geometry update is
  waiting for paint. Initial and activation layout measurements run before
  paint and refresh replaced targets, while ordinary resizing remains batched.

- Fixed macOS process termination when navigation cancels an in-flight WebKit
  request. Optimized builds now retain the exception unwinding required by
  Wry's recovery path, with a native release-profile regression and build guard.

- Wait for native navigation and the new document's load event after desktop
  test reloads. This avoids sending JavaScript into a document being replaced.
  Readiness generations are recorded only in test builds. Backend logs and
  bounded macOS crash summaries are retained for native test failures. macOS
  smoke tests now verify and run an isolated installation copied from the DMG.
  Scenario reloads restore the test motion guard, while motion assertions
  explicitly exercise native transitions.

- Made packaged desktop validation independent of suspended macOS paint frames,
  wait for native viewport resizing and workspace terminal controls, and target
  Pierre selection shortcuts
  at the shadow editor on Windows. Existing edit, undo, and lifecycle assertions
  remain in the native platform suite.

- Added regression coverage for input ordering, queue bounds, cancellation,
  stale attachments, legacy-server compatibility, immediate writes after
  lifecycle changes, and provider-usage isolation.
- Added a native encrypted-transport test that delivers reversed frame arrivals
  into a real PTY in the correct order and refuses another socket's lease use,
  even when it authenticates with the same bearer.
- Integrated current main without reverting its toolchain changes and
  regenerated the new RPC fixtures against the current Effect protocol.
- Updated the connection/RPC architecture, workspace guide, and native testing
  procedures for input recovery and selected-server usage.

### Downloads

On macOS, copy BiBCode.app from the DMG to Applications before launching it.

Desktop installers and standalone server distributions are provided for macOS,
Linux, and Windows on ARM64 and x64. Linux server `.deb` and `.rpm` packages are
included for both architectures. Stable desktop updater payloads and signatures
remain available through `latest.json`.

**Full Changelog**: https://github.com/mubeda/BibCode/compare/v0.5.7...v0.5.8

## [v0.5.7] - 2026-09-07

BiBCode v0.5.7 makes remote servers easier to identify when sharing a host or
saving a paired connection.

### Remote server sharing

- The **Share this host → Address** dropdown now displays each endpoint's IP
  address or hostname instead of repeated labels such as “Local network” and
  “Private network”. Active LAN, Tailscale, and NetBird interfaces can be
  distinguished directly in the list, with duplicate endpoint URLs removed by
  the existing selection logic.
- **Automatic (LAN)** remains available. Explicit network addresses appear once
  native sharing is enabled; endpoint availability, default selection, public
  address restrictions, and pairing behavior remain unchanged.

### Server aliases

- Added **Server alias (optional)** to the **Add Server** pairing-code form.
  The alias is saved on the connecting device and appears in its saved-server
  list and environment rail, including after reconnecting or restarting.
- Aliases are trimmed; blank input uses the name supplied by the pairing code.
  Failed pairing preserves the entered alias for retry, while successful
  pairing clears the completed form.
- Aliases use the existing connection catalog labels and retain the
  storage-instance identity introduced in v0.5.5. They do not rename the remote
  host, change its identity, or affect names on other clients.

### Reliability, validation, and documentation

- Fixed loopback/SSH-tunnel pairing being rolled back when an already-active
  standard credential could not call the administrative confirmation RPC. The
  client now verifies that credential through its connection supervisor;
  server permission checks and pending-pairing confirmation remain enforced.
- Fixed a terminal cleanup race by retaining a child process's exit result for
  late subscribers, including children that finish before a watcher attaches.

- Added regression coverage for address selection, alias persistence and
  hydration, blank aliases, and failed-pairing retry behavior.
- Updated the remote-access guide, connection-runtime documentation, and
  macOS, Linux, and Windows native validation procedures.

**Full Changelog**: https://github.com/mubeda/BibCode/compare/v0.5.6...v0.5.7

## [v0.5.6] - 2026-09-04

BiBCode v0.5.6 is a supported dependency and toolchain convergence release. It
moves the web, desktop, server, relay, and repository tooling onto one reviewed
set of current compatible versions while preserving the native Rust/Tauri
runtime and the existing browser/desktop RPC boundary.

### Toolchains and frontend foundations

- Standardized development on Node.js 26.8.1, pnpm 11.25.0, Rust/Cargo 1.98.0,
  Vite+ 0.3.0, Vite 8.2.2, and the single Vitest 4.1.11 runtime supplied by
  Vite+. The main workspace now uses TypeScript 7.0.2; the Astro marketing app
  remains on TypeScript 6.0.3 until its public checker supports TypeScript 7.
- Updated React and React DOM to 19.2.8 with matching types, and refreshed the
  Base UI, Lexical, TanStack, fonts, icons, DOM test runtime, and other
  maintained frontend dependencies as compatible cohorts.
- Moved Pierre Diffs to stable 1.3.6 and its public editing API, removed the
  retired Pierre prerelease patch, and retained diff review, partial staging,
  editor history, undo, and conversation rendering behavior.

### Runtime and dependency compatibility

- Converged Effect core and its selected v4 companions on
  `4.0.0-beta.107`. Alchemy remains on the newest compatible beta.72 release,
  paired with the exact Drizzle ORM and Kit RC5 build required by that train,
  avoiding the incompatible Effect RC.112 split.
- Updated the supported Rust and Tauri dependency floors under Rust 1.98,
  including the source compatibility repairs required by the new compiler and
  libraries. The application remains a native Rust/Tauri desktop and server
  runtime; no production Node.js service or Electron host was introduced.
- Retained WebdriverIO 9.29 and the Tauri automation service/plugin 1.2 cohort.
  The 1.3 line remains blocked until upstream fixes teardown ordering and aligns
  its globals/expect dependency train.

### Reliability, security, and maintenance

- Reworked vendored-reference synchronization into an exact,
  history-independent snapshot transaction. Linked worktrees share one writer
  lock; literal Git paths, pruning, casing, and file modes are preserved; and
  typed failures, defects, interruption, and timed rollback all quiesce their
  child processes before recovery advances. An unverifiable recovery keeps the
  lock for explicit repair instead of exposing partial state.
- Refreshed the exact Effect and Alchemy reference snapshots and made the
  dependency ledger declaration-complete: 221 audited entries, no pending row,
  and no unaccounted direct dependency or toolchain declaration.
- Hardened Clerk frontend-domain validation by rejecting malformed or
  unsupported Punycode labels before URL construction. Valid IDN custom Clerk
  domains remain unsupported until BiBCode owns one deterministic
  cross-runtime IDNA validator.
- Updated immutable GitHub Actions pins, release/test contracts, native port
  probing, process-watchdog coverage, raw Git parsing, and repository identity
  checks used by the cross-platform build and packaged-test paths. The Git
  broadcaster cancellation regression now proves synchronous lifecycle
  cancellation and joined cleanup without depending on a five-second scheduler
  race, and Claude hook-plumbing coverage uses an isolated probe context under
  parallel load. Catalog joins now reserve a workspace's current direct path
  before any retained-snapshot fallback, preventing duplicate old/new ownership
  after a retarget; its integration coverage uses an owned healthy-refresh
  boundary.

### Supported release assets

- Desktop installers for macOS 11+ on Apple Silicon and Intel, Linux on ARM64
  and x64, and Windows 11 ARM64 plus Windows 10/11 x64.
- Standalone server archives for all six OS/architecture targets, plus ARM64
  and x64 Debian and RPM packages and `bibcode-server-SHA256SUMS`.
- Signed updater payloads and a six-target `latest.json` manifest for the
  stable in-app update channel. macOS applications remain ad-hoc signed and
  unnotarized, and Windows installers remain without Authenticode.

**Full Changelog**: https://github.com/mubeda/BibCode/compare/v0.5.5...v0.5.6

## [v0.5.5] - 2026-09-04

BiBCode v0.5.5 fixes desktop pairing: **Add Server** refused every pairing code
with "Server already saved" even when no server was saved at all.

### Pairing fixes

- A saved remote server is now identified by the host's storage instance id
  rather than by the environment id the host declares about itself. Every
  BiBCode server declares the same id — `local` — including the server the
  desktop app runs in process, so the app's own **Local** environment and every
  remote host claimed one key in the client's environment registry. Pairing saw
  that key already taken and refused the code; removing the saved server could
  not help, because the entry it collided with was Local, which cannot be
  removed. Two different remote servers would have overwritten each other the
  same way. Reproduced in the desktop app on a data root that had never seen the
  remote host, and verified afterwards against an independent second server:
  both hosts declare `local` with different storage ids, the remote pairs
  end-to-end encrypted, and it now appears in the environment rail alongside
  Local instead of replacing it.
- The host's declared id is kept beside the client's own key, and the connection
  resolver checks the endpoint against that stored value on every connect, so
  the identity assertion is preserved rather than dropped. Servers saved before
  this release carry the declared id in their existing field and resolve through
  it unchanged — no re-pairing and no migration.
- The manual endpoint-and-token path identifies a server the same way, keeping
  the declared id for hosts that report no storage id.
- "Server already saved" now names the entry that was collided with instead of
  printing one generic sentence, so the dialog says which saved server to
  reconnect to or adopt.

### Documentation

- The remote architecture document records how a saved remote is identified and
  why a host's declared id cannot key it. The Linux, macOS, and Windows desktop
  runbooks check that a paired server appears alongside Local and that a second
  offer from the same host is refused by name.

## [v0.5.4] - 2026-09-04

BiBCode v0.5.4 fixes the desktop workspace sidebar: the floating **Toggle main
sidebar** control sat on top of the environment rail's first entry, so aiming
at **Local** collapsed the sidebar instead of selecting the environment.

### Interface fixes

- The environment rail now reserves the same topbar strip the thread sidebar
  header reserves, so the first environment entry starts below the fixed
  toggle instead of underneath it. The rail's separator line is continued
  across the reserved strip, keeping the header edge unbroken between rail and
  sidebar. On a native macOS titlebar the control resolves to the top-left
  52px column, which is exactly the rail's width, so the two overlapped
  completely; a WebKit geometry harness built from the shipped stylesheet
  reported 672 px^2 of overlap and a hit-test at Local's centre landing on the
  toggle, and reports no overlap with Local receiving the hit after the fix.
  The thread sidebar brand drops its control-clearance margin because the
  control no longer overlays that header, while the collapsed-sidebar centre
  panel header keeps using the shared offset variable. A regression assertion
  pins the reserved strip ahead of the environments group.

## [v0.5.3] - 2026-09-03

BiBCode v0.5.3 fixes the encrypted pairing channel in every desktop app: with
the v0.5.1 connect policy and the v0.5.2 macOS transport exception in place,
pairing still ended as "Server unreachable" because the client rejected the
server's handshake reply.

### Connection fixes

- The end-to-end-encrypted channel now reads WebSocket frames delivered as
  `ArrayBuffer`, which is what every real browser hands over once the socket
  is switched to `binaryType "arraybuffer"`. The client previously treated
  such frames as non-binary, failed the Noise handshake as a protocol error
  right after receiving the server's reply, and closed the socket without
  sending its pairing message. Verified end to end inside a WebKit page
  carrying the app's policy against a live server: the handshake completes,
  the pairing token is consumed, and the session is minted. A regression test
  drives the handshake through a socket that delivers `ArrayBuffer` frames.

### Test reliability

- The blocked-remote cancellation test in the Git status broadcaster awaits
  the cancellation before releasing the blocking permit, removing a race that
  failed a release preflight under CI load.

## [v0.5.2] - 2026-09-03

BiBCode v0.5.2 is a macOS-only fix on top of v0.5.1: the desktop app can now
reach plain-HTTP remote servers on a LAN or tailnet, which App Transport
Security had been refusing before any packet left the machine.

### Connection fixes

- The macOS bundle now merges an `Info.plist` that relaxes App Transport
  Security for web content only (`NSAllowsArbitraryLoadsInWebContent` and
  `NSAllowsLocalNetworking`) and declares the Local Network usage description,
  so **Add Server → Pairing code** works against `http://` servers. Native
  code keeps the default policy, and the hardening test rejects the blanket
  `NSAllowsArbitraryLoads`.

### Documentation

- The remote architecture and release documents record the macOS gate beside
  the webview connect policy introduced in v0.5.1.

## [v0.5.1] - 2026-09-03

BiBCode v0.5.1 makes headless servers pairable from the desktop app without a
browser detour, keeps them running across reboots as a per-user service, and
fixes two connection blockers found in real use: the desktop webview refused
plain-HTTP remote servers, and a WebKit downgrade left the Linux desktop unable
to open its connection database.

### Headless server pairing

- Added `bibcode pairing offer`, which mints an encrypted `bibcode://pair?code=…`
  offer directly against the data root, beside a running server, for pasting
  into **Add Server → Pairing code**. Endpoint and reach rules are shared with
  the Share tab, and the command fails closed until the server has started on
  that data root once.
- `bibcode serve` now prints a ready-to-paste `pairingCode` in its startup line
  when bound to a routable address. `--no-startup-pairing-offer` (or
  `BIBCODE_NO_STARTUP_PAIRING_OFFER=1`) keeps that credential out of service
  logs; loopback and wildcard binds print none.
- The Add Server dialog names the CLI as a source of pairing codes alongside the
  Share tab.

### Per-user background service

- Added `bibcode service install | uninstall | status`, which installs a systemd
  user unit on Linux (enabling lingering so it starts at boot), a LaunchAgent on
  macOS, or a logon scheduled task on Windows, running `bibcode serve` as the
  invoking user with that shell's `PATH` so provider CLIs keep finding their
  credentials. Nothing ships inside the Linux packages; the definition is
  written on request.

### Connection fixes

- Fixed the desktop webview policy that made every Add Server attempt against
  a LAN or tailnet server fail with "Server unreachable" before a packet left
  the machine, on all desktop platforms. `connect-src` now admits `http:` and
  `ws:` in addition to loopback and TLS, while scripts and default sources stay
  same-origin; a hardening test pins the policy.
- The Linux desktop now offers its acknowledged reset when WebKit reports
  "Unable to establish IDB database file", which happens when an AppImage
  bundling an older WebKitGTK opens a connection database migrated by a newer
  one. The dialog explains each cause, and the runbook documents the data-root
  isolation that avoids it.
- Server bind failures now report the address and the operating-system error
  instead of a bare "failed to bind the server listener".

### Release reliability

- The updater signature verifier covers all six supported desktop targets,
  including Linux ARM64 and Windows ARM64, and manual release repair runs stay
  pinned to the immutable tag while using current release tooling.

### Documentation

- Remote access and standalone server guides cover the CLI pairing offer, the
  startup pairing code, and running the server as a per-user service. The
  remote architecture document records the offline offer path, the startup
  offer, the service installer, and the webview connect policy. The three OS
  runbooks add headless pairing, restart, and service validation steps.

## [v0.5.0] - 2026-09-03

BiBCode v0.5.0 adds first-class remote environments, a complete project-level
Git Manager, and a dedicated Agents view. It also expands native desktop and
standalone-server releases to six OS/architecture targets and hardens the
connection, process, packaging, and test boundaries exercised by those
features.

### Remote servers and secure connectivity

- Added **Remote Servers** settings, pairing-code and `bibcode://` deep-link
  flows, manual and SSH-assisted connection setup, saved-server lifecycle
  controls, compatibility/version status, remote update actions, and an
  environment rail that scopes projects and actions to the selected machine.
- Added authenticated Noise NK transport with bounded record fragmentation,
  no-downgrade credentials, pinned host identity, transport-scoped sessions,
  and explicit protocol-compatibility negotiation for off-host connections.
- Added grant-derived sharing state and verified exposure transitions. Windows
  remote access uses a program-scoped firewall rule and rolls back firewall,
  listener, and persisted state when widening or narrowing cannot complete.
- Hardened pairing and session authority across cancellation, reconnects,
  concurrent server processes, stale delivery, duplicate requests, revocation,
  forwarded loopback peers, and bounded per-principal byte/message capacity.
  Remote failures remain typed and retryable without silently substituting a
  local or WSL backend.
- Added remote server update discovery and installation through the existing
  desktop-owned updater boundary, including status badges and bounded
  multi-server checks.

### Project-level Git Manager

- Added a GitHub Desktop-shaped centre panel for each project, covering working
  tree and staged changes, per-line and per-hunk selection, commit/amend/undo,
  safe discard, paged history and diffs, branches, tags, fetch/pull/push,
  publication and force-with-lease, stashes, merge previews, rebase,
  cherry-pick, squash, reorder, revert, reset, conflict recovery, and image
  diffs.
- Added an on-demand pull-request and checks pane with GitHub, GitLab, and Azure
  provider resolution. Pull-request creation now opens a non-mutating review
  dialog and only publishes the branch and creates the PR after final
  confirmation; retries reuse an existing pull request.
- Kept Git Manager activity server-owned, bounded, non-interactive, and free of
  background telemetry. Repository generations fence stale reads, concurrent
  mutations share one owner, capabilities degrade independently, and remote
  environments retain the same typed RPC boundary.
- Improved the final packaged behavior found during macOS and Windows native
  validation: clean checkouts select **History**, dirty/loading selections are
  preserved, merge recovery stays on **Changes**, history rejects stale pages,
  symbolic remote `*/HEAD` pointers are not shown as branches, and a temporarily
  missing upstream tracking ref no longer makes the manager unavailable.
- Redesigned the pull-request review dialog for readable repository/base/head
  metadata, clear branch-publication guidance, stable padding, and a fixed
  action footer.

### Agents view and interface polish

- Added a full-height **Agents** view with environment/project/status grouping,
  filtering, unread state, provider identity, branch and conversation previews,
  live detail, and capped server-pushed shell summaries.
- Raised the sidebar and navigation text floor, increased the default sidebar
  width with double-click reset, and improved Linux WebKitGTK font hinting and
  subpixel rendering without changing other applications' GTK settings.
- Added a shared orange panel-separator token and aligned the environment rail,
  sidebar, content headers, Git Manager, diff/file/preview panels, terminal,
  status bar, and top frame in both themes.
- Corrected route, capability, stale-status, dialog, and layout edge cases found
  during packaged macOS, Linux, and Windows interaction testing.

### Native releases and standalone server packages

- Expanded the release matrix to macOS, Linux, and Windows on both ARM64 and
  x64. Desktop downloads now include two DMGs, two AppImages, and two NSIS
  installers; the signed updater manifest contains all six matching targets.
- Added standalone `bibcode` server archives for all six targets plus native
  ARM64/x64 Debian and RPM packages, install-and-run container tests, a sorted
  SHA-256 manifest, and optional minisign signatures.
- Hardened Windows ARM64 and x64 builds with architecture-aware MSVC setup,
  drive-safe portable ZIP creation, checkout-local Vite+ execution, cached NSIS
  tooling, explicit sidecar execution manifests, bind-only port probes that do
  not trigger firewall prompts, and targeted cleanup for test-generated rules.
- Hardened Linux packaging around an Ubuntu 20.04 server compatibility build,
  native package smoke matrices, AppImage portability, minimal RPM curl
  dependencies, and ownership restoration after container builds.
- Hardened macOS/Linux startup PATH hydration so login-shell probes detach from
  inherited controlling terminals, accept a complete framed result without
  waiting for lingering descendants, and reap their process group before
  desktop startup continues.

### Data, compatibility, and validation

- Database migrations 46–49 add pairing reach metadata, a durable pairing-offer
  idempotency ledger, shared authentication-authority revision state, and active
  pairing-delivery state. Existing stores continue through the verified
  pre-migration backup path.
- Remote transport, Git Manager, Agents-view, provider-check, and update
  contracts are additive; no intentional breaking API change is documented.
- Expanded native CI, release-candidate validation, packaged desktop UI and
  upgrade smoke tests, remote Docker validation, source-control fixtures, and
  cross-platform runbooks. The macOS Git Manager flow was exercised against a
  real private GitHub repository through branch, diff, staging, commit, history,
  push, pull-request, checks, tag, fetch, and checkout behavior.
- Stabilized release gates by target-gating macOS-only test imports, giving
  uncached Rust workspace jobs an explicit workflow budget, and replacing a
  scheduler-sensitive Git broadcaster teardown timer with canonical lifecycle
  retirement. Windows forced-shutdown coverage now exercises the production
  timeout instead of a scheduler-sensitive 250 ms fixture window, and
  cross-process auth revocation coverage distinguishes queued prior revisions
  from the forbidden post-revocation event. Authority-expiry coverage starts
  its short-lived grant only after nonessential fixture setup is complete.
- Removed an accidental literal NUL from tracked TypeScript source and added a
  repository-wide guard so text-source corruption fails tests immediately.

### Supported downloads and trust

- macOS 11+ Apple Silicon (`arm64`) DMG
- macOS 11+ Intel (`x64`) DMG
- Linux ARM64 AppImage
- Linux x64 AppImage
- Windows 11 ARM64 NSIS installer
- Windows 10/11 x64 NSIS installer
- Standalone server archives for all six targets, plus Linux ARM64/x64 `.deb`
  and `.rpm` packages and `bibcode-server-SHA256SUMS`

macOS builds remain ad-hoc signed and unnotarized, and Windows installers remain
without Authenticode. Tauri updater payloads are independently signed and
verified by BiBCode before publication.

**Full changelog:** [v0.4.2...v0.5.0](https://github.com/mubeda/BibCode/compare/v0.4.2...v0.5.0)

## [v0.4.2] - 2026-08-25

BiBCode v0.4.2 makes desktop updates easier to understand and recover from,
while keeping Codex and Claude terminal interfaces readable across app-theme
changes.

### Safer, observable desktop updates

- Added live update-protection progress for every local backend, including the
  current protection stage, elapsed time, and active mutation count while
  BiBCode drains work, quiesces the runtime, checkpoints SQLite, creates a
  verified backup, and stops the backend.
- Classified RPC methods through the typed contract inventory so long-lived
  read subscriptions do not block an update, while unknown methods still fail
  closed as mutations.
- Kept verified backup protection as the default and the primary retry path.
  Installing without a backup is available only after a real protection
  attempt fails and the user explicitly acknowledges the risk; the native host
  rejects forged first-attempt bypasses.
- Preserved exact backend-topology safety for the acknowledged fallback: the
  desktop host still snapshots and stops the running native and WSL backends,
  restarts that same set if installation fails, and reports each environment
  as skipped instead of protected.
- Improved the protection-failure dialog so retry, exact secondary exclusions,
  and the destructive no-backup action remain distinct and correctly laid out.

### Terminal and agent compatibility

- Kept each Codex terminal on its launch palette until an explicit restart, so
  changing the BiBCode app theme cannot make Codex composer text disappear or
  leave the terminal half-repainted on macOS, Linux, or Windows.
- Applied the resolved terminal palette when opening agent and script
  terminals, including the OSC foreground, background, cursor, and Windows
  console markers that terminal applications snapshot at spawn.
- Prevented a light in-band color-scheme reply from selecting Claude Code's
  broken fullscreen light path inside the embedded xterm host, while retaining
  dark-scheme and OSC color support.
- Removed inherited `NO_COLOR`/disabled-color host settings from PTY launches
  unless the launch explicitly opts out, and advertised true-color support so
  agent TUIs do not silently lose their ANSI colors.
- Changed the device-local terminal-theme default to **Follow app theme**;
  **Always dark** remains available for users who prefer a fixed terminal
  palette.

### Release reliability

- Prevented the chat surface from reading an unavailable nested platform value
  while server configuration is still partial, so opening a local draft cannot
  fail during configuration bootstrap.
- Made the startup activity-recovery test use a current fixture timestamp so it
  continues to exercise unresolved-versus-completed recovery after the
  production 30-day completed-activity retention window advances.

### Compatibility and downloads

- No database migration or intentional breaking API change is included. New
  updater progress and skipped-protection fields are additive and decode with
  safe defaults.
- The release pipeline provides macOS 11+ Apple Silicon and Intel DMGs, a Linux
  x64 AppImage, and a Windows 10/11 x64 NSIS installer, plus signed updater
  payloads and the four-platform `latest.json` manifest.
- macOS builds remain ad-hoc signed and unnotarized; Windows installers remain
  without Authenticode. Tauri updater payloads are independently signed and
  verified by BiBCode.

**Full changelog:** [v0.4.1...v0.4.2](https://github.com/mubeda/BibCode/compare/v0.4.1...v0.4.2)

## [v0.4.1] - 2026-08-24

BiBCode v0.4.1 is a reliability release for Git/worktree coordination, the
Files surface, provider error reporting, and embedded terminals. It replaces
poll-heavy or ambiguous state with lifecycle-owned observation, keeps UI state
honest across races and failures, and restores the intended Create Worktree
flow.

### Source control and worktrees

- Replaced per-project branch polling with server-owned, event-driven VCS
  observation. Native worktree and Git-metadata watches coalesce bursts, retain
  one trailing read, and use a bounded 60–300 second safety read when native
  observation is unavailable or misses an event.
- Added shared local/full status-read owners with cancellation leases, mutation
  epochs, publication fences, deterministic shutdown, and clean reattachment.
  A slow remote fetch, provider lookup, or cancelled caller can no longer delay
  or publish over newer local state.
- Added one automatic-fetch owner per physical repository. Linked worktrees
  share the default 180-second fetch, exact remotes are fetched once, failures
  use bounded backoff, and `0` still disables automatic fetch.
- Added lightweight passive VCS summaries for sidebar state. Fresh local and
  provider data publishes independently from pull-request enrichment; a prior
  matching PR may be retained for only one stale cycle, and provider failures
  no longer make the repository appear clean or unavailable.
- Reduced Git work by using one porcelain-v2 status snapshot, running numstat
  only for areas that exist, and setting `GIT_OPTIONAL_LOCKS=0` on background
  reads. Added native measurement tooling for Git-process rate and foreground
  mutation queue latency.
- Added bounded, no-follow repository fingerprints so healthy Focus refreshes
  can reuse trusted worktree inventory between mandatory five-minute Git
  reconciliations. Unknown, changed, malformed, replaced, or over-limit inputs
  fail open to a real inventory scan.
- Hardened create, publish, retarget, detach, remove, file-write, and stacked Git
  mutations so their status/catalog invalidation settles after the actual
  filesystem, Git, and durable ownership result—even when the requester
  disconnects, cancellation races, or a panic occurs.
- Reworked Create Worktree around one permanent Name editor plus optional
  Smart/GitHub/Branch sources. Exact local and remote refs no longer appear as a
  duplicate result, free local branches enable **Reuse branch** by default, and
  edited names remain stable.
- If a selected remote branch becomes local before submission, the server now
  reuses that free local branch. A branch already checked out by another
  worktree retains the safe suffixed-branch fallback, including concurrent
  creation races.
- Added strict Bitbucket request/body bounds and exact selected-remote handling
  when publishing repositories, including protection against option-shaped
  remote names.

### Files surface

- Added a server-authoritative workspace entry stream and immediate **Refresh**
  action. External file/folder creates, renames, and removals rescan without
  collapsing expanded folders; watcher startup now closes the initial-snapshot
  race with an explicit resync.
- Rebuilt the cached file index from concurrent tracked/ignored Git listings
  plus bounded directory walks. Ignored trees and empty directories remain
  visible, cold callers share one build, and ordinary warm reads start no Git
  work.
- Added explicit cache invalidation for create, rename, delete, duplicate, new
  parent paths, `.gitignore`, and `.git` classification controls while keeping
  ordinary existing-file content saves inexpensive.
- Added drag-and-drop moves to folders or the workspace root, correct new-entry
  parent selection, open-tab path updates after rename/move, and close-on-delete
  behavior.
- Failed, cancelled, or availability-raced moves and mutations now roll the
  optimistic tree back to server truth instead of leaving duplicated or stale
  rows. Refresh failures preserve the existing tree and remain retryable.
- Kept every directory as its own row rather than merging single-child folder
  chains, so actions and paths always target the directory the user sees.

### Providers, delivery, and runtime lifecycle

- Added compatible runtime error classes beside provider messages. The UI can
  now distinguish a provider-reported failure from a BiBCode transport,
  permission, or validation failure without guessing who is at fault.
- Preserved provider-native failure details and terminal reasons for Claude,
  Codex, Cursor, Grok, and OpenCode. Fixed healthy Codex turns with `error: null`
  being reported as failures and mapped documented refusal, output-limit, and
  content-filter outcomes truthfully.
- Made refused or uncertain delivery visible in the sidebar and thread status,
  derived atomically from the durable outbox. Retry, dismissal, or successful
  delivery clears it without overwriting provider-session identity or state.
- Hardened OpenCode event-stream handling so connection failures, HTTP errors,
  clean EOF, chunk failures, and explicit stop all publish their terminal state
  and close the provider stream. Supervisors no longer wait forever on a sender
  retained by a completed runtime.
- Tightened process, logging, provider, VCS, watcher, workspace, and catalog
  ownership across shutdown and reattachment so cancellation-ignoring work is
  still awaited and cannot publish into a replacement lifecycle.

### Terminal and interface fixes

- Added a device-local Terminal theme setting: **Always dark** by default,
  **Always light**, or **Follow app theme**. Terminal OSC colors and xterm's
  extended grayscale palette now match that choice, avoiding dark TUI panels on
  an otherwise light terminal.
- Fixed resize bursts that could strand Codex or another full-screen TUI at an
  intermediate PTY size. Every requested resize is retained and the worker
  coalesces only to the newest dimensions.
- Fixed center-panel separators disappearing against adjacent surfaces.
- Prevented stale sidebar and thread-status updates from overwriting newer
  unresolved-delivery, worktree, provider, or availability state.

### Data, compatibility, and documentation

- Database migrations 44–45 add provider error attribution and unresolved
  delivery projection fields. Existing stores upgrade through the normal
  verified pre-migration backup path.
- Contract additions are additive and older error-class values decode safely as
  unknown. No intentional breaking API change is documented.
- Expanded living architecture, source-control, workspace, observability, and
  cross-platform validation documentation. Added implementation plans and
  reference screenshots for the future full Git Manager UI; those planning
  documents do not claim that the complete planned Git Manager interface ships
  in v0.4.1.

### Validation and supported downloads

- `vp check`, all 11 `vp run typecheck` targets, 8,266 Vite+ tests, release
  smoke, Rust formatting, workspace Clippy with warnings denied, the full Rust
  workspace test suite, and the production desktop build passed with Rust
  1.97.1.
- Packaged macOS interaction was visually verified with Codex Computer Use,
  including the single-editor Create Worktree dialog and the checked/unchecked
  **Reuse branch** behavior.
- The release pipeline builds and verifies macOS 11+ Apple Silicon and Intel
  DMGs, a Linux x64 AppImage, and a Windows 10/11 x64 NSIS installer, together
  with signed updater payloads and the four-platform `latest.json` manifest.
- macOS builds remain ad-hoc signed and unnotarized; Windows installers remain
  without Authenticode. Tauri updater payloads are independently signed and
  verified by BiBCode.

### Known limitations

- External Files changes are detected within seconds rather than instantly;
  use **Refresh** for an immediate rescan. An arbitrary custom Git
  `core.excludesFile` also requires manual Refresh after it changes.
- The complete Git Manager interface described in the new implementation plans
  is future work; this release ships its VCS coordination, summaries,
  measurement, and reliability foundations.

**Full changelog:** [v0.4.0...v0.4.1](https://github.com/mubeda/BibCode/compare/v0.4.0...v0.4.1)

## [v0.4.0] - 2026-08-17

### Highlights

- Added an authoritative worktree catalog that discovers existing Git worktrees, lets users adopt one or all discovered checkouts without recreating them, and preserves physical identity across path aliases and reconnects.
- Added explicit recovery and removal flows for missing or present worktrees, including fresh server-side plans, dirty/stale-registration confirmations, durable retry receipts, and identity-safe cleanup on Windows, macOS, and Linux.
- Improved local desktop presentation: macOS and Linux now focus on the local environment, Windows keeps truthful WSL location and recovery controls, Cursor is enabled as a supported provider, and legacy Grok actions are hidden.
- Improved Activity timestamps and hierarchy while bounding Claude fallback ambiguity so stale or unrelated processes are not presented as controllable activity.
- Hardened provider, terminal, logging, persistence, update, and shutdown ownership under parallel load, including bounded OpenCode reaping, isolated native fixtures, and per-runtime process cleanup that preserves sibling desktop runtimes.
- Hardened Linux packaging and expanded repeatable native validation across macOS arm64/x64, Linux x64, and Windows x64.

### Data and compatibility

- Database migrations 40–43 add per-project worktree discovery state, repository identity pins, and durable worktree-removal receipts. Existing stores are migrated through the normal verified pre-migration backup path.
- No intentional breaking API change is documented. Older servers that do not advertise worktree-catalog support continue without the new catalog controls.
- macOS artifacts remain ad-hoc signed and unnotarized; Windows installers remain unsigned.

### Known issues

- Native Windows, Linux, and both macOS architectures require their respective release runners for final installer and updater verification.

**Full changelog:** [v0.3.13...v0.4.0](https://github.com/mubeda/BibCode/compare/v0.3.13...v0.4.0)

[v0.4.0]: https://github.com/mubeda/BibCode/releases/tag/v0.4.0
[v0.4.1]: https://github.com/mubeda/BibCode/releases/tag/v0.4.1
[v0.4.2]: https://github.com/mubeda/BibCode/releases/tag/v0.4.2
[v0.5.0]: https://github.com/mubeda/BibCode/releases/tag/v0.5.0
