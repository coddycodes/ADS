# Architecture

BiBCode has one React/Vite frontend and one native Rust backend. Browser mode
connects to a running `bibcode` server. Desktop mode runs the same frontend in
Tauri 2 and starts the primary Axum/Tokio server in-process. Native shell
capabilities cross a narrow `DesktopBridge`; application traffic uses the same
typed HTTP and WebSocket RPC boundaries in both modes.

```mermaid
flowchart TB
  Browser["Browser\nReact + Vite"]
  Desktop["Tauri 2 desktop\nRust host + system WebView"]
  Bridge["DesktopBridge\nTauri commands and events"]
  Server["BiBCode server\nRust + Axum + Tokio"]
  Providers["Codex / Claude / Cursor / OpenCode"]

  Browser -->|"HTTP + Effect RPC over WebSocket"| Server
  Desktop --> Bridge
  Desktop -->|"starts primary runtime"| Server
  Server -->|"provider-native protocols"| Providers
```

## Components

- **Tauri host (`apps/desktop`)** owns native windows, menus, dialogs, updates,
  WSL and SSH launch, the desktop connection catalog, and backend lifecycle.
  Windows protects the catalog with DPAPI. Atomic compare-only and
  compare-and-write transitions cross privileged bridge commands and are
  serialized by the native catalog owner across all WebViews in that host. A
  protected bridge collapses a legacy renderer catalog into native storage
  before publication, then uses native storage exclusively. macOS and Linux
  advertise that native protection is unavailable and use the renderer's
  transactional IndexedDB catalog instead; any legacy renderer value is
  atomically migrated into that single source before it is cleared.
- **React app (`apps/web`)** owns the user interface and client-side state. It
  uses hash history in desktop mode and browser history on the web. Preview
  content is hosted in Tauri child webviews; preview automation is brokered by
  the Rust server and consumed by the React host. On the Tauri desktop host only
  tab status, open, and navigate are supported and advertised; snapshot, click,
  type, and the other automation calls are not yet. Its native views drive
  only the visible tab, so open always shows the tab, and a request for a
  thread that is not on screen times out. The desktop keeps one long-lived
  native view per environment's preview storage (the local environment keeps
  the original profile; each other environment gets its own), because
  recreating child webviews while switching tabs disconnected the app; logical
  tabs rebind to their environment's view. In browser mode there is no child
  webview: the client advertises only status and open, and open shows a prompt
  and returns `pending-user` at once; the page opens only if the user clicks Open. The server prefers the host
  with more operations, so a connected desktop serves automation first.
  Loopback dev servers on a remote environment load through the server's
  preview gateway. Typography and text-contrast
  rules live in [`UI.md`](../../UI.md).
- **Desktop adapter (`apps/web/src/tauriDesktopBridge.ts`)** installs
  `window.desktopBridge` only when Tauri globals are present. Tauri commands and
  events implement privileged operations; browser fallbacks are limited to
  explicitly safe capabilities.
- **Server (`apps/server`)** is both a Rust library and the native `bibcode`
  binary. It owns HTTP/WebSocket RPC, authentication, SQLite persistence,
  orchestration, providers, terminals, Git, files, diagnostics, relay access,
  and process supervision. Its worktree catalog joins bounded live Git and
  filesystem observations with durable project and canonical-thread
  projections. A nullable project repository-key pin is stored outside the
  rebuildable projection, established only by a trusted primary-checkout scan,
  and joined into project reads; generic projection writes cannot change it,
  and projection rewind/replay preserves it. It fences later fallback anchors
  and is not a persisted live catalog.
  Each project also carries a server-authored `repositoryIdentity` derived from
  its checkout's `origin` remote: a canonical key of the lowercased host and the
  repository path without `.git`, plus the remote (stored without userinfo,
  query or fragment, so no credentials persist), name, owner, provider and
  checkout root. A remote whose path has an empty segment has no identity. It lives in
  the rebuildable project projection and changes only through the
  server-internal `project.repository-identity.set` command, which emits
  `project.meta-updated` without changing `updatedAt`. A reconcile runs right
  after project creation or a workspace move (so a new project is briefly
  ungrouped), once at startup for every project, and after a healthy
  worktree-catalog scan: at most every five minutes per project, or at once
  when the primary checkout's `.git/config` size or modification time changed
  since the last scan (one stat in a tracked task, bounded to three seconds, one
  outstanding per project; an unknown stamp falls back to the interval, and the
  catalog refresh never waits for it, so a stalled network mount cannot block it).
  Reconciles are serialized
  per project, bounded to four concurrent Git reads, cancellable while waiting
  for either, and run as tracked tasks
  off the effects worker and catalog refresh paths, joined at shutdown. A
  checkout that is missing, unreadable, refused, or no longer a repository
  keeps the stored value; only a confirmed repository whose `origin` was
  removed or changed updates it. Clients group projects by it but cannot set
  it. It is unrelated to the path-based repository-key pin, which fences
  worktree adoption on one machine. Projects sharing a repository may share
  Git observation, but retain isolated latest-value snapshots, streams, thread
  joins, subscribers, suppressions, and mutation epochs. Catalog views retain
  the last authoritative arrays through degraded observations and cancel
  pending poll, Git, and probe work after their final subscriber before bounded
  idle eviction. Project and repository lifecycle epochs make poller
  initialization transferable across subscriber aborts and prevent canceled
  prior-lifecycle work from publishing into an immediate reattachment.
  For an already trusted repository, Focus may reuse that observation when two
  bounded, no-follow Git-admin fingerprint passes match the proof captured
  before its last successful real scan. Unknown or changed proofs fail open to
  Git; Explicit requests always scan; and Focus reconciles with real inventory
  at exactly five minutes from the last successful real-scan completion. Reuse
  still rebuilds the project join, suppressions, availability, generation, and
  healthy-publication effects. Managed create, retarget, and removal invalidate
  the shared proof only after their terminal durable settlement, while managed
  creation retains its bounded path suppression.
  VCS status has one server-owned coordination entry per canonical worktree.
  Local and full reads are shared independently, each caller retains its own
  cancellation lease, and only final-lease release or a lifecycle/mutation
  fence cancels the physical read. A read gate and checked mutation epoch
  prevent work admitted before or during a mutation from publishing afterward;
  post-fetch remote reads and pull-request enrichment carry that same epoch
  through their final internal publication or delivery check without changing
  public status schemas. Cached remote state additionally carries its producing
  ref identity; an external ref change clears it and resets branch-specific
  client remote fields before reconciliation;
  the final idle transition across a queued mutation burst requests one
  coalesced trailing local refresh before reopening the gate. The local fallback
  scheduler remains independent from remote reconciliation, so a slow fetch or
  remote probe cannot delay editor-save status publication.

  Active status subscriptions resolve the canonical worktree, Git directory,
  and common directory in one bounded Git read, then install execution-host
  native watches before the initial local snapshot read. Working-tree and Git
  metadata signals collapse at a 125 ms trailing edge, with exactly one trailing
  read retained when signals arrive during a physical read. Overflow, watcher
  loss, and setup unavailability are sticky fallback states; they preserve the
  subscription and cannot be cleared by a later ordinary event. Active
  subscriptions also retain a completion-based safety read after the greater of
  60 seconds or four times the last completed read duration, capped at 300
  seconds. Explicit mutation, workspace, focus/menu catch-up, and structured
  terminal-process exit invalidations remain immediate; automatic fetch and
  post-fetch remote reconciliation remain independent. There is no periodic
  `symbolic-ref` worker; watcher and safety local observations carry branch
  identity in their porcelain result.

  Final status-subscriber release retires the worktree epoch, scheduler,
  watcher, reads, and lifecycle tasks before an immediate reattachment may
  create a fresh watcher generation. Server shutdown closes subscription and
  read admission and awaits owned setup, physical reads, and lifecycle tasks.
  A completed terminal launched as a structured BiBCode command notifies the
  deepest active canonical worktree ancestor after terminal exit publication;
  provider, session, and delivery events do not. Commands typed inside a
  persistent interactive shell have no structured completion boundary and
  therefore converge through native watcher events or the safety read. Native
  watching covers only filesystem paths visible to the server execution host;
  remote-host changes outside that boundary require an explicit invalidation or
  safety convergence.

  One status observation reads porcelain-v2 branch and file state once and runs
  staged or unstaged numstat only for areas that are present. Each unmerged path
  appears once, as an unstaged `conflicted` entry, and the Git Manager detector
  reports any merge, rebase, cherry-pick, revert, or squash left in progress as
  optional `operationInProgress` (omitted when the probe fails). A failed porcelain
  read whose repository probe also refuses the folder becomes the non-repository
  result, which says, when the server can tell, whether no repository exists, Git
  cannot read one, or Git refuses to trust it. A readable non-work-tree result
  carries no reason; a failed read with a work-tree probe or cancellation remains
  an error. Status and
  background-observation Git reads set `GIT_OPTIONAL_LOCKS=0`; fetch and
  mutations keep the ordinary Git environment.

  Passive VCS summaries use a separate latest-value producer per canonical
  worktree. One porcelain-v2 status read supplies repository, named/detached or
  unborn identity, and dirty state without numstat or file-row materialization.
  A failed porcelain read is classified like the status observation, carrying
  optional `repositoryUnavailableReason` when `isRepo: false`, while an
  operational failure in a working repository keeps the prior summary as stale.
  The existing bounded origin-provider read and pull-request service add
  provider and matching named-branch PR state. Each producer cycle publishes
  its fresh base before optional PR enrichment. A PR completed in cycle N may
  be carried only into cycle N+1 for the same ref and provider while that
  cycle's enrichment is pending or fails; the publication keeps the PR's
  original `observedAt` and sets `stale: true`, then expires it in cycle N+2
  unless enrichment refreshed it. The carried value is only the prior PR, not
  necessarily the exact prior summary: current local and provider base fields
  still come from the fresh cycle. Missing provider configuration and an empty
  provider PR list are successful absence. An operational Git or provider-
  discovery failure before a fresh base retains the exact prior summary and
  observation time as stale. Subscribers share the producer, refresh after 30
  seconds or as soon as a mutation finishes or a local change is reported for
  that worktree, and cancel and await in-flight Git or provider work when the final
  subscriber leaves. This path never starts automatic fetch.

  Automatic fetch has a separate owner per canonical Git common directory. A
  subscribed worktree resolves that identity with one bounded read for its
  lifecycle. Each interval performs at most one repository-wide upstream
  discovery and one exact single- or multi-remote fetch, then signals every
  attached worktree to compute its own branch, upstream, default-ref, provider,
  and pull-request result. The default interval is 180 seconds; live changes,
  bounded failure backoff, and `0 = disabled` remain supported. The client keeps
  serial mutation ordering but runs
  `vcs.refreshStatus` on a separate latest-per-environment/worktree lane, so
  focus, visible-document, menu-open, and post-action freshness cannot queue a
  mutation behind an active read.

  The Git driver's clone, fetch, pull, and push carry Git's HTTP stall guard
  (`-c http.lowSpeedLimit=1000 -c http.lowSpeedTime=60`); a stall is reported
  as "The transfer stalled…" (Git Manager failure code `network-stalled`).
  Transfers a user can cancel (clone, and the Git Manager operation stream's
  fetch, pull, push, remote-branch deletion, and tag push) run on the
  network-transfer variant with a 24-hour safety bound. Transfers nobody can
  cancel run on the bounded variant, capped at 10 minutes: automatic fetch,
  `vcs.pull`, and the stacked-action and publish pushes, wherever they start
  (chat header, Sidebar Pull, Source Control panel, or the Git Manager's
  Create PR dialog). SSH transports have no stall guard, so a dead SSH link ends
  only by cancellation, those bounds, or the connection's own keepalive
  settings. The orchestration bootstrap fetch and the Pull Requests checkout
  fetch run outside the driver with their own budgets and no stall guard.
  Clone runs in a server-owned clone runtime keyed by
  destination (canonical parent plus leaf). It reserves its destination before
  Git runs, and its owned task removes only a destination that clone created
  after a failure, timeout, stall, cancellation, or panic of the transfer,
  before it reports the outcome. A caller that asked to
  `detach` can lose its socket without stopping the clone; a later session
  re-attaches, and `vcs.cancelClone` or shutdown stops it. An orphaned clone runs
  to completion under the 24-hour bound and, for HTTP(S), the stall guard. A new
  clone into a destination still being cleaned up waits for that cleanup.
  Reusing an existing destination requires a `HEAD` that resolves to a commit
  and an index file (`git rev-parse --git-path index`); otherwise the error
  names the incomplete clone and asks the user to remove it or choose another
  folder.

  Shared observations never bypass per-caller anchor validation, and final
  view/repository ownership release is atomic against concurrent attachment. A
  scan leader moves the repository single-flight guard into repository-owned
  work, while its project-view caller waits with view cancellation. Detaching
  that view therefore releases its refresh lock immediately; an alias may keep
  the exact-anchor observation alive within the current repository lifecycle.
  Mutation invalidations overwrite one per-view pending epoch and are drained
  by at most one lifecycle-owned worker. When that worker queues behind a
  pre-mutation scan, it alone may recover from the stored stale result with one
  current-epoch scan; ordinary waiters still receive the identical stale error.
  Invalidations before the recovery fence coalesce into that scan, while a
  later mutation produces at most the next serialized recovery step. Final
  detach cancels, aborts, and clears the worker slot, releasing its project
  refresh lock even if a dependency await is not cancellation-aware. A
  reattachment cannot inherit that project-view work or result, though it may
  coalesce with an exact-anchor observation still owned by an aliased
  repository lifecycle. External-checkout adoption is serialized by stable
  project and physical-repository identity, revalidates live Git membership,
  and persists canonical ordinary workspace ownership atomically without
  creating Git state. Ownership uses the catalog's host-path identity key, so
  lexical aliases retain one owner across replay even when the checkout is
  missing. Public adoption receipts bind the canonical opaque payload and an
  immutable result. Only healthy authoritative catalog observations may
  reconcile durable adopted-thread branch metadata.
  The standalone `bibcode` binary may perform a final sweep of descendants
  rooted at its own process after its managed owners shut down. Reusable
  in-process runtimes—including the desktop server—share their host PID and
  therefore skip that sweep: provider and terminal owners still stop their
  registered processes, while host-owned children remain under the host
  lifecycle.

- **Git Manager (`apps/server/src/git/manager/`)** owns repository generations,
  refs and worktree occupancy snapshots, tip-pinned history pages (at most 100
  commits and about 1 MiB each), diff and
  patch parsing, server-authored guards, in-progress/conflict inspection, and
  the branch, sync, stash, merge, rewrite, conflict, and tag operation
  primitives. `apps/server/src/production/git_manager_rpc.rs` adapts those
  owners to the typed RPC registry and the worktree catalog's existing mutation
  arbitration. Review preview sources stop capturing git output at the patch
  cap, discard the last incomplete file, and set `truncated`; History budgets
  count serialized JSON including escaping.

  Git Manager signals reuse the status watcher's lifecycle. The worktree root
  retains one native recursive watch, including its `.git` tree. The notify
  backend owns recursion: inotify directory watches on Linux, FSEvents on
  macOS, and subtree watching on Windows. Events from the top-level `objects/`
  and `lfs/` stores and from submodule object stores (`modules/<name>/objects/`)
  are filtered; the rest of a submodule's Git directory still refreshes status.
  Git metadata outside the worktree has non-recursive root watches and native
  recursive watches only on `refs/`, `logs/`, `reftable/`, and `worktrees/`,
  registering stores as they appear. Its own object stores are not watched; the
  recursive `worktrees/` watch does reach linked worktrees' submodule Git
  directories (`worktrees/<name>/modules/`), whose object-store events are
  filtered, and on Linux each of those directories costs an inotify watch.
  Sibling events invalidate only
  HEAD/reftable state and administrative-directory creation/removal. One
  metadata classifier handles registration and event routing, including
  Windows keys normalized to uppercase. Every full rescan, whether caused by
  native overflow or the bounded registration queue, invalidates refs after
  reinstallation, including failed rescans.
  Only ref-shaped events reset the 125 ms ref debounce, capped at one second
  from the first event, so continuous file/index writes cannot starve it.
  Local and ref reads remain independent and retain one trailing refresh.

  The lightweight signature hashes heads, remotes, tags, `refs/stash`, worktree
  occupancy, structured symbolic/object HEAD identity, and the stash reflog. A
  HEAD that names no valid ref and no commit (an interrupted clone's
  placeholder) is hashed as its own state rather than failing the read.
  Each burst uses bounded `for-each-ref`, `symbolic-ref`, and `rev-parse` reads
  plus a bounded loose reflog read; reftable repositories require a bounded
  `git stash list` read for their table-backed reflog. Only a changed hash bumps
  generation. It does not run full remote status, and index-only events do not
  start signal reads. Signature reads serialize, retry after a mutation fence
  settles, and cannot publish after repository retirement. Remote status
  publishes before signature I/O; a failed signature input is logged, leaves
  the signature unchanged, and retries on the next trigger.

  Signals also carry optional `watcherDegraded` without changing generation on
  health-only updates. The explicit client-runtime
  `signalWithDegradedFocusRefresh` accessor consumes the existing platform
  `ConnectionWakeups.focusVisibility` stream. Its browser adapter shares
  listeners and coalesces focus/visibility returns; only degraded repositories refetch active refs,
  commits, and stashes. Healthy/legacy signals and plain signal subscriptions
  do not enable this fallback. No polling is added; automatic fetch retains
  its independent role and default 180 s interval. History reads its first page
  once the signal's availability is known (one read on open) and again on
  either trigger: a signal change, or a repository generation that moves past
  the loaded pages (in-app operations while the watcher is degraded). Each read
  uses a never-reused key, and the watcher's echo of a repository-triggered
  read is absorbed, so one change never causes two reads. The absorption relies
  on the server advancing a signal from generation 0 at its first computed
  signature. The stash list (a reflog read plus one `git stash show` per entry)
  is read only while the Stashes panel is open: once on opening, and on each
  signal change or in-app operation while open. Closing the panel drops the
  query, so a closed panel reads nothing (not even on reconnect) and shows no
  count.
  A completed response captures one retained-page batch with at most two reads
  in flight. The same-tip first page already supplies offset zero, so that page
  is omitted from the batch. A first page covering all loaded rows needs no
  retained batch; otherwise changed tips require reading the old pinned graph.
  Older-generation replies cannot regress newer decorations. Git decorations
  remain verbatim. Loaded pages, selection, and scroll
  survive background errors, which offer Retry.

  On Unix the supervised process runner retries a spawn that fails with
  `ETXTBSY` (the executable is still open for writing, typically a helper
  that was just installed or rewritten, or a fork of this process that has
  not exec'd yet) every 25 ms for at most one second and never past the run's
  own deadline. Every other spawn error is returned immediately.
  The OpenCode and Codex provider-terminal helper launchers use the same retry
  before waiting for readiness. A run whose
  future is dropped before it settles (an interrupted inline RPC, an aborted
  task, runtime shutdown, or a panic) kills its whole process group or Windows
  job while the root is unreaped, without waiting; see the cancellation
  invariants in [RPC and orchestration](./rpc-and-orchestration.md).

- **Contracts (`packages/contracts`)** contains Effect schemas and TypeScript
  contracts only. It defines persisted models, RPC methods, HTTP APIs, desktop
  bridge values, and provider events without application runtime logic.
- **Client runtime (`packages/client-runtime`)** owns environment registration,
  connection supervision, authorization, RPC sessions, and shared client state.
  It is used by browser and desktop clients.
- **Shared runtime (`packages/shared`)** contains runtime utilities used by
  multiple packages through explicit subpath exports. `sourceControl` owns
  common request nouns, provider display names, and number prefixes (`!` for
  GitLab, `#` otherwise) used by the PR UI and Git actions.

## AppImage child environments

`bibcode_server::process::isolate_appimage_environment(&mut command)` is the
cross-platform entry point for child isolation and is a no-op off Linux.
Standard, Tokio, and portable-pty command adapters live beside the policy in
`apps/server/src/process/appimage.rs`. Standard and Tokio commands inherit by
default; a command with a cleared environment uses
`ChildCommand::Process { command, inheritance: EnvironmentInheritance::Cleared }`
with its standard command builder. The inheritance mode belongs only to that
process adapter; PTYs already own their complete captured environment.

Provider commands use
`crate::provider::environment::sanitize_provider_subprocess_environment` to
remove `RUST_LOG` and apply child isolation. Production provider services and
provider terminals both depend on this provider-owned module.

Covered children include terminal PTYs, providers, their probes and helpers,
Git and source-control helpers (including review Git commands), server-launched
editors/file managers, and desktop-owned SSH, Tailscale, and host
backend-shutdown `kill` commands. Isolation changes only the selected command.
The desktop process, WebKitGTK-owned children, and BiBCode's own binaries retain
the bundled environment; desktop external-backend launches and the generic
supervised process runner deliberately do not apply this policy. The login-shell
PATH probe also inherits the original desktop environment: its output hydrates
the desktop's own PATH before Tauri starts, so stripping bundled entries there
would change the desktop's executable ordering. Before any Linux relaunch (an
update restart or **Restart BiBCode**), the host marks every descriptor above
stderr close-on-exec (`apps/desktop/src-tauri/src/relaunch.rs`), so the new
process no longer inherits the old AppImage runtime's keep-alive pipe. The old
FUSE mount, its daemon and the replaced AppImage therefore end with the old
process; the relaunched app still inherits the old environment, including stale
`.mount_*` entries behind the new mount's own paths.

The gate checks a few variables before copying the full environment. A nonempty
`APPIMAGE` and an absolute, non-root `APPDIR` activate isolation. For an
extracted AppDir without `APPIMAGE`, require the current executable to be under
`APPDIR` and `$APPDIR/AppRun` to exist. This supports `squashfs-root/AppRun` on
Ubuntu without FUSE while rejecting a stray `APPDIR` in an ordinary installation.
Once gated, the policy inspects every variable in the effective child environment
and removes entries rooted in `APPDIR` or absolute paths containing a stale
`.mount_*` component. This covers launcher and plugin additions, including
Python, Qt, and GStreamer settings, without a fixed path-variable list.

Values are split on colons, with semicolons also accepted for `LD_LIBRARY_PATH`
and spaces for `LD_PRELOAD`. Values with no bundled entry remain byte-for-byte
unchanged, including non-path strings and non-UTF-8 values. Retained entries
keep their order and host credentials such as `SSH_AUTH_SOCK` survive. If every
surviving entry is empty, the variable is unset: AppRun appends a trailing
separator when the original value is empty. A launcher-created empty loader
path must never become `LD_LIBRARY_PATH=.` and search the working directory.
Empty entries alongside real host entries, including an explicitly configured
`.`, remain unchanged. The host library search path is never replaced with
distribution-specific directories.

The policy also removes `APPDIR`, `APPIMAGE`, `ARGV0`, and `OWD`, plus the
launcher's forced `GDK_BACKEND`, `PYTHONDONTWRITEBYTECODE`, and `GTK_PATH`, and
the user-only `GTK_THEME` override (inherited or supplied through
`APPIMAGE_GTK_THEME`). The hook replaces `GTK_PATH` without retaining the user's value, so
its host directories must also be removed. Terminal shells can reapply user
settings from their rc files. Command-local overrides and removals participate
in the effective environment; cleared process commands never recover ambient
values. PTYs expose their complete captured environment through a raw
`iter_full_env` accessor in vendored portable-pty, so non-UTF-8 command overrides
cannot escape the policy. Ordinary non-AppImage launches keep their configured
environment, and no selected child changes the parent environment.

Linux file-manager launching takes opener candidates from `open::commands`,
isolates each command, and starts a fallible detached reaper thread before
spawning a child. The command uses the safe `process_group(0)` API; the thread
calls `spawn`, reports its result, then owns the child handle until `wait` completes.
The first successful spawn returns promptly while the opener may keep running;
thread or spawn errors try the next candidate in the crate's order. Other
platforms keep `open::that_detached`, including Windows ShellExecute. Desktop
URL/path opening through `tauri-plugin-opener` remains a separate limitation:
the plugin spawns its own opener commands without an environment customization
API. Linux file reveal uses D-Bus and does not spawn a local opener.

Linux desktop startup's `deep_link().register_all()` invokes the plugin's
`update-desktop-database` and `xdg-mime` registration commands. These inherit
the desktop's AppImage environment without an environment customization API.
A failure to start a command reaches the startup warning, but the plugin
discards the commands' exit statuses. If a tool starts and then exits nonzero
because of bundled libraries, registration fails silently and can leave the
`bibcode://` handler missing.

The GTK hook appends `XDG_DATA_DIRS` after a forced `/usr/share` entry.
When the original variable is unset, stripping the AppDir leaves `/usr/share:`:
the usual `/usr/local/share` default is lost and an empty entry relative to the
working directory remains. The child policy cannot reconstruct the pre-launch
value, and the packaging wrapper leaves this hook behavior unchanged.

## Project-data ownership and identity

The Rust server is the source of truth for the requested and effective data
root, state kind, store classification, storage-instance marker, SQLite
database, verified backups, and offline recovery. The desktop host does not
invent a path or manipulate those files through renderer input. It resolves a
native or WSL environment from its authoritative launch plan and coordinates
privileged multi-backend inspection, update protection, stop, recovery, and
exact-plan restart around the server-owned persistence operations.

The resolved base root defaults to the current user's `~/.bibcode`. An explicit
CLI `--base-dir` takes precedence, followed by `BIBCODE_HOME`, followed by the
desktop bootstrap root. Development and installed desktop builds use that same
base root by default but intentionally select different state kinds: `dev` and
`userdata`. Changing the account, home/profile, explicit root, drive, mount, or
the target of a symlink/junction therefore selects a different effective
store; it is not evidence that the previous store was deleted. Recovery and
diagnostics always report both the requested root and the canonical effective
root so that alias changes are visible.

`environmentId` is a logical routing identity. `storageInstanceId` is the
persistent UUID of one prepared store. The client records the first observed
non-null storage identity for a stable target and blocks a different identity
before synchronization or cache consumption. That boundary cannot reconstruct
history that predates marker deployment: the first protected release cannot
detect that an earlier build already switched to another valid unmarked
BiBCode database until the database receives a UUID and a client accepts it.

The server classifies its persistent SQLite store before opening it for normal
read/write traffic. The absence of both `state.sqlite` and its
`environment-id` storage-instance marker is the only automatic first-run case.
An existing unmarked database is adopted only after a read-only integrity,
migration-ledger, and required-table check recognizes it as a BiBCode store;
missing, malformed, corrupt, or unrelated state is preserved and startup fails
closed without creating or replacing either file. Validation opens the source
read-only and uses SQLite's online-backup API to build a coherent in-memory
snapshot, then inspects only that snapshot. Backup work runs on a blocking
worker in bounded positive-page batches and yields after progress or contention
so a live writer or checkpoint can proceed. One absolute deadline begins before
the source is opened and governs both backup and post-backup inspection. The
in-memory snapshot installs a SQLite progress handler that checks the same
cancellation token and deadline during `quick_check`, migration-ledger reads,
and required-table queries. Dropping startup therefore cancels a queued worker
before source open, or interrupts a running backup or inspection query, and
releases SQLite state before marker publication or migration. This remains
coherent while another server commits or checkpoints and never materializes a
full store copy in a temporary directory. Classification does not mutate
persistent database, WAL, or marker bytes and entries. SQLite may create or
update `state.sqlite-shm` as volatile WAL-index coordination; SHM contains no
database content and is not required for crash recovery. Once validation
succeeds, normal SQLite locking continues to support sequential or simultaneous
server processes sharing that established store. Graceful server join sends an
explicit close through the bounded SQLite queue after previously admitted jobs.
The worker then closes even if a stale cloned database handle remains; those
handles reject later calls as unavailable. Join still waits for the worker's
positive close notification before returning.

Desktop development and installed builds use the same resolved base data root
by default, but select separate `dev` and `userdata` state kinds. Rust desktop
unit tests are forbidden from resolving that real default root: every Tauri
mock that reaches persistence must install an explicit per-test temporary data
root. This boundary is enforced in the shared desktop root resolver so test
cleanup, mock backend lifecycle, window-state fixtures, and IPC fixtures cannot
read, replace, or recursively remove a developer's or installed application's
store on Windows, macOS, or Linux.

After store preparation succeeds, the runtime copies the marker's persistent
UUID into runtime-only server configuration. Every current-server descriptor
surface publishes that UUID as `storageInstanceId`: the public
`/.well-known/bibcode/environment` response, `server.getConfig`, the initial
`subscribeServerConfig` snapshot, lifecycle welcome and ready events, and
BiBCode Connect descriptors. A clean restart of the same store therefore
reports the same identity. New local servers always publish a UUID string;
contract decoding maps the omitted field from an older or third-party remote
server to `null`. Normal environment descriptors never include the requested
or effective data root, alias diagnostics, or any other local filesystem path.

Every current-server descriptor surface also publishes the remote protocol
compatibility window: `remoteProtocolVersion` and
`minCompatibleRemoteProtocol` (both `1` today). The numbers are defined once
per language — TypeScript in `packages/contracts/src/environment.ts`
(`REMOTE_PROTOCOL_VERSION`, `MIN_COMPATIBLE_REMOTE_PROTOCOL`) and Rust in
`apps/server/src/http.rs` — and serialized by the well-known descriptor
route, `server.getConfig`, the initial `subscribeServerConfig` snapshot,
lifecycle welcome and ready events, and BiBCode Connect descriptors. Contract
decoding maps both fields to `0` for an older or third-party server, which
clients classify as legacy limited compatibility. The window supplements —
never replaces — the default-false capability booleans that continue to gate
optional behavior.

Direct remote connections use the server-owned pairing/auth authority and the
client-runtime-owned transport policy described in
[Remote architecture](./remote.md). Noise E2EE bounds unauthenticated work by
global, exact-peer, IPv4 `/24` or IPv6 `/64`, and loopback-forwarder admission.
Authenticated assembly uses cancellable fit-first byte pressure, a 10-second
incomplete-message progress deadline, and releases permits after dispatch rather
than handler completion. Outbound fit-first admission keeps a five-second
reservation deadline; the connection writer then gives each record 20 seconds
to be accepted and each message 30 seconds plus its size at 16 KiB/s, and ends
the session when either passes. Desktop exposure remains a privileged
`DesktopBridge` operation: native starts are local-only, WSL exposure is
externally managed, and only authoritative live grants or an explicit
legacy-resume action can request a wide native bind. Pairing credentials remain
pending until the verified client persists local state and confirms the session.
Desktop discovery advertises only usable IPv4 addresses until a dual-stack
listener exists; public candidates are labeled, never defaulted, and require an
explicit warning acknowledgement. Plain `/ws` caps individual frames at 16 MiB
while retaining the 64 MiB reassembled-message cap; a client that offers the
`bibcode.rpc.chunked.v1` subprotocol receives messages over 64 KiB as binary
records instead.
Hosted pairing rejects URL userinfo and renders and submits one normalized host;
legacy query credentials are scrubbed after capture. Saved blank host keys
normalize to the legacy plain-transport representation. The environment registry
owns explicit desired connection intent outside disposable supervisor scopes, so
passive state reads and catalog-driven scope replacement cannot reconnect an
explicitly disconnected environment.

First-run creation initializes a randomized same-directory staged SQLite file,
closes it without retained journal sidecars, and publishes it at `state.sqlite`
with an atomic no-replace hard link. Platform file identity checks bind cleanup
and the final reopen to that staged filesystem object, so a competing or
replacement final path is never configured, migrated, removed, or overwritten.

Every startup acquires the effective root's cross-process
`.bibcode-storage.lock` on a bounded blocking worker before store
classification. The guard remains held through validation, non-mutating
migration inspection, any required backup, the migration transaction, and the
normal database open. It is released when preparation returns, so established
servers retain SQLite's supported multi-process read/write behavior. A
read-only inspection connection determines the pending migration suffix without
creating the migration ledger or changing persistent pragmas or user bytes. A
genuinely new empty store applies its initial migrations without a redundant
backup. The reusable guard API also requires an explicit timeout and
cancellation token; dropping a waiter cancels its blocking worker so it cannot
become a hidden lock owner later.

An existing store with pending migrations must publish a verified generation
under
`<effective-root>/backups/<userdata-or-dev>/<storage-instance-id>/<backup-id>/`
before the migration transaction begins. SQLite's online backup API copies the
live connection, including committed WAL data, in bounded cancellable page
batches into a same-filesystem staging directory. The server then closes and
checks the staged database, records its pre-migration ledger version, hashes it
with SHA-256, writes a path-free manifest, flushes both files and supported
directories, atomically renames the generation, and reloads it for checksum and
`quick_check` verification. Unix backup directories and files use `0700` and
`0600`; Windows generations inherit the private data-root ACL. Only after that
publication is verified may migration start or retention remove older verified
generations. Every backup ancestor is identity-bound beneath the effective root,
and symlink, junction, or reparse ancestors are rejected before generation
writes. Verified generations contain exactly one singly-linked `state.sqlite`
and one singly-linked `manifest.json`; their manifest ledger version must match
an exact migration prefix recognized by the running binary, and their canonical
UTC creation time must agree with the generation's filesystem publication time.
Retention keeps the newest three for the store and state kind, ordered by that
filesystem time with a backup-ID tie-break. It reopens and revalidates each
expired generation, deletes only the two bound files, and removes the now-empty
directory without recursive deletion. Staging entries, extra or linked content,
malformed manifests, location or identity mismatches, future-schema ledgers,
implausible timestamps, and failed verification are reported and never selected
for deletion. Newly created backup ancestors and their parents are flushed
before the next descendant is created on platforms that support directory
syncing.

Offline recovery is explicit and state-kind scoped. `bibcode storage inspect`
classifies the resolved store and inventories verified backups without changing
database, WAL, marker, or catalog bytes. `bibcode storage restore --backup-id`
accepts only a selected generation whose manifest, location, storage identity,
checksum, SQLite integrity, and migration prefix verify for that same state
kind. Restore reopens the verified database through a no-follow, read-capable
file handle before copying it into recovery staging; on Windows, directories
remain attributes-only while database files explicitly request generic read
access. `bibcode storage start-empty` is the separate destructive choice. Both
mutating commands first acquire an exclusive runtime lock and the storage
operation lock, write and flush a recovery journal, and preserve every existing
database, WAL, SHM, and marker entry in a private recovery generation before
installing a verified database or allowing the next startup to create a new
store. Normal server handles hold the runtime lock shared, so multiple
established servers remain supported while offline recovery fails closed if any
server owns the root. Startup refuses to create or open SQLite while a recovery
journal or strict recovery-staging entry remains. A crash or cancellation after
preservation therefore leaves recoverable files and an explicit incomplete
operation instead of silently converting the root into a first-run store.

## Desktop project-data recovery

The desktop host exposes inspection and recovery only through privileged,
desktop-only bridge commands. The renderer supplies an environment identifier
and, for restore, a verified backup identifier; it never supplies a filesystem
path, executable, distro command, or shell text. Rust resolves the selected
environment from the authoritative backend launch plan before inspecting or
mutating its store. Native environments call the server persistence library
directly. A WSL plan records one validated absolute Linux data root when it is
created, sends that exact root as `bibcodeHome` in the server bootstrap, and
reuses it for recovery. This preserves an explicit WSL `BIBCODE_HOME`; recovery
does not guess `$HOME/.bibcode`. WSL inspection and recovery invoke the bundled
`bibcode storage inspect|restore|start-empty` CLI with an argument vector and a
bounded output/time budget, never a shell command string.

Inspection is read-only and happens before a backend is stopped. A selected
restore generation must verify before the desktop enters its serialized
project-data operation. The operation prevents concurrent update or backend
start coordination, stops only the selected environment, commits the server's
journaled restore or start-empty workflow, and restarts the exact registered
launch plan only after a committed result. If restart fails, the result still
reports the committed recovery and carries the separate restart error; a
failed validation does not stop or restart the backend. The dialog's retry
action starts that exact plan when it is stopped and then re-inspects it;
retrying an already-running target is inert. Opening a store path and exporting
redacted diagnostics also resolve the path inside Rust rather than accepting
renderer-controlled paths.

If a desktop-owned backend fails after its launch plan has been resolved, the
supervisor retains that exact plan as a stopped recovery target while
withholding its connection bootstrap. The host emits only a
`desktop:project-data-status-changed` invalidation containing the environment
identifier. On mount and after each invalidation, the renderer re-reads the
Rust-owned status classification through `getProjectDataStatuses`; it never
infers recovery from an HTTP error or trusts the event as a classification,
path, storage identifier, or diagnostic source. This closes both startup races:
a failure recorded before the WebView mounts and one recorded immediately
after it subscribes.

The recovery dialog opens automatically only for a recovery-required local
desktop environment and remains available manually for a storage-identity
change. Restore requires an explicit backup selection and confirmation.
Start-empty has a separate confirmation which states that the prior store is
preserved rather than deleted; after commit, adopting the replacement storage
identity remains a distinct explicit action before connection retry. Desktop
storage adoption uses the native dialog boundary and performs no transition
when confirmation is cancelled or unavailable; browser mode retains its
browser-dialog fallback through the same local API. Remote
environments cannot invoke this local privileged recovery surface. Existing
T4Code files are neither scanned nor migrated: if they overlap a current root,
the same current-store classifier and recovery rules apply without a
compatibility alias or fallback.

## Runtime topology

The desktop WebView loads the bundled `apps/web` build (`frontendDist`) or the
Vite development URL. Separately, the Tauri host starts the primary backend
through `BackendSupervisor` and publishes its ready descriptor to the renderer.
The renderer then establishes the normal HTTP/WebSocket connection.

Before Tauri starts on macOS and Linux, the desktop host captures `PATH` from
the user's login shell inside one bounded session and process group, preventing
an inherited controlling terminal from suspending the interactive shell. A
complete delimited PATH frame is authoritative even when shell shutdown or a
descendant keeps the stdout pipe open: the host terminates and reaps that group,
joins the reader, then installs the merged PATH while startup is still
single-threaded. Missing, malformed, oversized, or incomplete frames leave the
inherited PATH unchanged.
See [AppImage child environments](#appimage-child-environments) for the probe's
environment policy.

```mermaid
sequenceDiagram
  participant Host as Tauri host
  participant Server as Rust server runtime
  participant UI as React WebView
  participant Provider as Provider process

  Host->>Server: start primary runtime with desktop bootstrap
  Server-->>Host: bound address and ready descriptor
  Host-->>UI: backend-ready event / bootstrap query
  UI->>Server: authenticate and open Effect RPC session
  UI->>Host: invoke privileged DesktopBridge operation
  Server->>Provider: provider-native request
  Provider-->>Server: provider event
  Server-->>UI: RPC result or stream chunk
```

The primary backend uses `BackendLaunchTarget::InProcess`. Optional WSL
backends use `BackendLaunchTarget::ExternalProcess`, so not every desktop
environment shares the host process. SSH forwarding is owned by the Tauri host;
provider, terminal, and managed relay processes are supervised by the server.
Neither path introduces a production Node server or packaged helper sidecar.

The server can also open preview gateway listeners. Each one reverse-proxies
one loopback dev server for one thread to a client that is not on the server's
host. A listener binds an ephemeral port on the local address the caller's RPC
connection arrived on, and the server refuses callers that reached it on a
public address or through a reverse proxy. It is opened through
`preview.gatewayOpen` and closed when its tabs close, after 10 idle minutes,
when its thread is deleted, or at shutdown. Agent and terminal sessions reach
clients the other way: their `$BROWSER` shim posts to
`POST /api/preview/open-url`, and the server broadcasts an `openRequested`
event that one client claims. See
[Preview gateway](remote.md#preview-gateway) and
[Open requests and automation hosts](remote.md#open-requests-and-automation-hosts).

When WSL-only mode is selected, that intent is authoritative even if an older
persisted document has a stale disabled-backend flag. WSL planning and primary
startup fail closed as a tagged `wsl-primary-unavailable` desktop state. The
host does not substitute the native Windows backend or publish a fallback
bootstrap. Retry and distro selection keep WSL as the primary target;
**Switch to Windows** atomically clears the WSL-only/backend flags and follows
the normal backend restart.

A failure of an optional secondary WSL backend remains non-blocking for a
native Windows primary, but does not remove that secondary from desired
topology. The host publishes a stable configured identity and a tagged
`wsl-secondary-unavailable` error with null endpoints and no credential. The
renderer keeps the environment registration and cached shell/thread data while
the resolver rejects connection attempts before creating a transport or
session. Explicit disable or distro replacement is what removes that desired
identity and clears its environment cache.

## Grant-driven desktop exposure

Native wildcard binds now occur exclusively through the **Share this host**
offer ceremony. The renderer widens through the verified
`desktop_bridge_apply_server_exposure` operation only when minting an off-host
**Another device** grant for a native primary. A bidirectional app-level
reconciler derives the requested topology from live server grants: it widens a
local-only native runtime when an off-host `another-device` reason exists and
restores loopback after the final native-managed reason is revoked. Custom
addresses remain externally managed and never authorize native listener or
firewall mutation. WSL-only primaries use their WSL-owned advertised endpoint
and never enter this native exposure state machine. The previous manual
network-access toggle is gone; server grant metadata is the policy source of
truth. Every fresh native desktop process starts local-only, so the durable
desktop setting records the last completed transition but is not startup
permission to listen wide.

The native exposure coordinator also serializes WSL/native topology changes.
Entering WSL-only first converges native settings, backend, and firewall to
local-only; leaving it starts native local-only. Local recovery attempts every
safeguard even after an earlier failure. The Windows firewall worker owns late
commands after its five-second caller deadline and removes a late-enabled rule
before processing a later enable. In the renderer, unmount may cancel work only
before the first privileged apply; once narrowing commits, its authoritative
refetch and one required compensating widen run to completion.

## Desktop update protection

The Tauri host coordinates update installation across the complete local
backend topology. It atomically snapshots the running primary and secondary
backends, prevents a new backend start from entering that snapshot, and keeps
configured-but-unavailable secondaries visible as unprotected. Verified backup
protection is the default and cannot be bypassed on the first install attempt.
A user may proceed past a failed secondary by selecting that exact environment,
or proceed without any verified backup only after a protection attempt fails
and the user explicitly acknowledges the unsafe fallback. The Tauri host, not
the renderer, enforces that transition. A bypassed attempt still snapshots and
stops the exact running backend set before invoking the installer, and restarts
that set if installation fails; it does not call the prepare or commit APIs and
reports every environment as skipped rather than protected.

Each included backend exposes an authenticated maintenance API only in desktop
mode. Native desktop runtimes must be loopback-bound. An external WSL runtime
may use its wildcard bind only when its desktop bootstrap explicitly marks the
transport as WSL-owned; the ordinary native wildcard case remains denied. The
maintenance owner admits status and other read traffic, rejects new mutating
HTTP and WebSocket RPC operations, and keeps a permit until every admitted
mutation has committed or failed. RPC mutability comes from the typed method
inventory; long-lived read streams therefore never hold a mutation permit, and
unknown methods fail closed as mutations. Each admitted mutation has a bounded,
sanitized operation label and age for diagnostics, without arguments, query
values, or payloads. Preparation then drains existing mutation permits with a
bound, quiesces runtime-owned writers, queues, providers, terminals, and
background tasks, checkpoints SQLite's WAL, and publishes and reloads a
verified `PreUpdate` backup while holding the store-operation lock. The host
polls authenticated maintenance status while preparation is pending and emits
the current stage, elapsed time, and active-mutation count to the protection
dialog. A failed status poll does not cancel or replace the authoritative
prepare request.

Every desktop-mode runtime with a bootstrap token owns the maintenance
coordinator regardless of its bind. The host obtains the primary's coordinator
through `ServerHandle::update_maintenance` and calls it in process; WSL and
other external backends use the loopback HTTP API. Both transports use a 45 s
prepare bound, poll progress every 250 ms with a 2 s status bound, and allow
10 s for commit or cancel; either successful finish exits the backend. Both
report the same failure message text without transport details. The HTTP
invariant is unchanged: maintenance routes are exposed only on loopback or a
WSL-owned wildcard bind.

Each in-process server runtime owns a distinct bounded process-attribution
registry shared by its provider, terminal, provider-helper, and managed-endpoint
owners. Runtime quiesce closes
that registry to new roots and captures its exact `(pid, creation-time)`
identities before either manager shuts down. A provider or PTY that finishes
spawning after this fence is rejected with the typed shutdown outcome and its
uncommitted owner terminates and reaps it before returning. Existing provider
process-group or Windows Job owners, terminal PTY owners, independently spawned
provider helpers, and the managed endpoint tunnel perform their
normal cleanup first; a final native sample kills only residual identities in
the captured runtime-owned closure, including descendants forked after the
initial sample. It never sweeps every descendant of the shared application
PID, so shutting down one embedded runtime cannot terminate a sibling
runtime's provider, terminal, helper, or tunnel children. PID reuse is excluded by creation-time
identity checks, registry entries leave with their process owners, and shutdown
is idempotent without awaiting while the registry lock is held.

Preparation is single-flight and returns a lease-bound identifier. Commit and
cancel must present that identifier. Commit sends its HTTP response before the
backend exits cleanly; cancellation, preparation failure, and lease expiry also
exit instead of resuming a partially quiesced runtime. The host marks those
exits as expected, stops every backend from the captured running set, and does
not invoke the platform installer until every included backend has committed
and stopped. A prepare, cancel, commit, stop, or installer failure attempts to
restart the exact prior running set before update coordination is released.
Any restart onto a port its stopped predecessor held (this update recovery, a
project-data restart of the target it stopped, a crash restart, or an exposure
or topology restart that re-plans a held port) lets the in-process server retry
a bind that finds the port still in use for up to 3 s, backing off from 25 ms
to 250 ms. First starts and WSL backends fail at once, and an expired window
reports the original bind error.
If that recovery restart fails, the supervisor keeps the exact launch plan
registered as stopped, records a typed update-recovery failure in its slot,
and withholds its connection bootstrap. In-process `ServerError::Bind` failures
with `AddrInUse` retain a typed port-in-use reason and the conflicting port;
other failures retain their original detail and use the plan's port. The update
manager derives `backendRecovery` from registered, stopped supervisor slots
with these failures; first-start failures do not create update-recovery entries.
A supervisor listener re-emits the current update state whenever that derived
list changes, after the supervisor releases its state lock. A failed installation
also emits one `desktop:project-data-status-changed` invalidation per current
recovery entry.
Checks, downloads, and installations (including protection bypass) pause while
any recovery entry remains, preserving the downloaded update and recovery state.

The update dialog and toast offer **Restart server**, calling the privileged
`retryProjectData` bridge command sequentially for each failed environment and
then requesting its normal connection retry. Any successful restart into a slot,
including a retry or an exposure/topology restart, clears its recovery entry;
removing the slot also drops the entry. A failed project-data restart replaces
an existing entry's typed failure, and an admission failure leaves it intact.
Once every entry is cleared, **Retry installation** becomes available. A failed
retry also offers **Restart BiBCode** through the optional `restartApp` bridge command.
The host rejects that command during an install or exclusive update/project-data
operation; otherwise it requests the normal application restart, whose exit
path persists window state, stops backends through `stop_for_exit`, and shuts
down SSH forwarding. Recovery never automatically retries or selects a new port.

The restart guard intentionally retains a known check-then-exit limitation:
it reads the update and project-data admission flags, but does not hold an
atomic restart reservation through `request_restart`. An installation admitted
after those reads can therefore be interrupted by the restart. This is an
accepted risk matching an ordinary manual quit during installation, not a
guarantee that restart and installation can never overlap. The guard continues
to refuse operations already observed in flight. Closing the remaining window
would require a shared admission protocol covering restart and every install
entry point; the current decision is to retain the existing quit/recovery
semantics rather than imply that independent flag reads provide that protocol.

In a desktop renderer, a missing primary bootstrap is a typed topology-read
failure, never a fallback to the development server or WebView origin. The
platform poll retains the registered primary and its cached data and composer
drafts while that bootstrap is withheld, and reuses its registration when the
same endpoints return. Browser and hosted endpoint fallbacks are unchanged.

Stopping the primary in-process backend never sweeps descendants of the shared
desktop PID; doing so would terminate the system WebView before the installer
can take ownership of application restart.

### Remote server updates

Every server answers the `updater.activeWork`, `updater.status`, `updater.check`, and
`updater.install` RPC methods (contract:
`packages/contracts/src/remoteUpdate.ts`; Rust mirror:
`apps/server/src/remote_update.rs`). All three environment-descriptor
producers—the well-known route (`apps/server/src/http.rs`),
`server.getConfig` (`apps/server/src/production/control.rs`), and the
Connect/relay descriptor (`apps/server/src/lifecycle.rs`)—embed
`remoteUpdateSupport` and advertise the surface with the default-false
`remoteUpdateControl` capability. All three also publish `bootId` and the
default-false `remoteUpdateProgress` capability, which advertises the snapshot's
progress fields, `bootId`, and `updater.activeWork`. Clients therefore know the
install mode before asking. `updater.activeWork` returns
`{runningTurns, liveTerminals, queuedMessages}` counted across all clients through
its own read method so the one-second status poll never waits on the store; a
failed count answers the typed `RemoteUpdateActiveWorkError`.

- Desktop-hosted in-process servers run in `interactive` mode.
  `updater.install` routes through the host's `DesktopUpdateManager` via the
  `RemoteUpdateDelegate` seam
  (`apps/desktop/src-tauri/src/remote_update_delegate.rs`), so remote install
  uses the same update-protection drain as local install and cannot skip backup
  protection.
  `updater.install` passes the caller's paired-client metadata (label, OS, address
  when known, and an 8-character session prefix), looked up through
  `AuthService::list_clients`, to `RemoteUpdateDelegate::request_install`; it
  never crosses `DesktopBridge`.
  The desktop keeps the requester as `requestedBy` on its update state while the
  flow runs and clears it when the flow ends; a second remote request joins that
  flow, and the delegate fills `downloadPercent`, `targetVersion`, and `installStage`.
  A remote install stopped by a secondary environment's protection says
  "Finish the update on the host."
- Headless `bibcode serve` and WSL/external desktop backends run in `manual`
  mode. `updater.check` refreshes the server's own version,
  `latestVersion` remains `null` because the server has no update feed, and
  `updater.install` fails with `remote_update_manual_required`. Clients render
  copyable operator instructions instead of an install action.
  `installKind` is derived from the static asset layout: a sibling `web/` means
  `archive`, `share/bibcode/web` means `system-package`, and an explicit
  `--static-dir` or no resolved directory means `unknown`; desktop hosts report
  `unknown`.

`updater.activeWork` and `updater.status` require `orchestration:read`; `updater.check` and
`updater.install` require `orchestration:operate`
(`apps/server/src/auth/scope.rs`).
Desktop delegate calls and each client-side per-environment update check are
independently bounded to 30 seconds. The client deadline includes supervisor
acquisition, readiness, and RPC execution; timeout interrupts the whole lazy
operation and releases one of the two update-check workers.

The web runtime owns one remote-update confirmation request and one update run
per environment. `AppRoot` hosts the confirmation independently of Settings and
sidebar view lifetimes; fresh running-work counts are advisory. Both entry
points and failure Retry actions require confirmation. Active progress replaces
the update action, and settled row failure can be dismissed without cancelling
an active run. Manual hosts show install/platform-specific operator steps. Host
request notices are informational and link to sharing management. A browser
whose primary server reconnects on a new boot/version offers an explicit Reload
button and preserves unsent input until it is chosen.

### WebView engines

The WebView engine is the operating system's, so it differs per platform:
WKWebView on macOS, WebKitGTK on Linux, and WebView2 on Windows. Browser API
support therefore varies between desktop hosts, and between desktop and browser
mode. The frontend feature-detects optional APIs and supplies its own fallback
rather than assuming the Chromium behavior that browser mode and Windows
happen to share. Linux text rendering has its own rules; see
[Linux webview text rendering](#linux-webview-text-rendering).

Optimized native builds preserve exception unwinding. On macOS, Wry can receive
an Objective-C exception while completing a URL-scheme request cancelled by
navigation; its catch boundary must recover without terminating the host.
`panic=abort` is therefore rejected by the macOS desktop crate. Existing Rust
panic boundaries and drop-based cleanup also retain their unwind behavior.

Center-pane geometry is published during layout on initial mount, when a body
target is replaced, and when active groups or surfaces change. An active panel must not remain hidden behind
a stale rectangle snapshot while WebKit defers paint. Ordinary resize work
remains frame-batched, and equivalent activation state does not force another
measurement.

Center chat-panel creation reserves and activates its client surface before the
server command settles. A confirmed command failure removes that reservation;
an interrupted result is ambiguous because durable thread creation may already
have committed, so the surface remains visible instead of silently orphaning a
valid panel thread. The reservation is renderer-local and remains protected
from older authoritative snapshots until a snapshot actually observes the new
thread; normal remote-deletion reconciliation resumes after that observation.
Authoritative thread removal later clears every surface that references that
thread. Reopening a closed (archived) chat panel reserves its surface the same
way before unarchiving the thread and removes it on a confirmed failure. This
ordering is shared by browser and all desktop hosts; it does not depend on a
WebView-specific scheduling delay.

Center-panel layout is renderer-local, but the panels it holds are shared.
Center terminals open or attach with `centerPanel: true`; the terminal manager
keeps that marker (sticky once set, including across restart and reopen) and
the launch command of the current process, including its provider env
defaults, and publishes both on `TerminalSummary`. A restart or reopen without
a command reports none, matching the default shell it spawns. Right-panel shells never carry the marker. Panel threads
record `hostThreadId`. Each environment's thread lifecycle reconciler adopts a
center terminal session or live panel thread the first time it sees one, as an
inactive tab in the host thread's focused group, skipping terminal ids reserved
by an in-flight local open and chat panels still pending locally. Adoption only
on first sight keeps a local close from being undone by later status updates.
Closing a chat panel archives its thread; a tracked panel thread that leaves the
live list (archived or deleted) loses its tab on every client unless it is
still pending locally, and is adopted again if it is unarchived. Lifecycle
reconciliation does not retain archived panel threads, so a tab persisted by a
client that was away when the panel closed is dropped too.
The reconciler reads the raw `subscribeTerminalMetadata` stream as whole
delivered batches (an atom alone keeps only a batch's last event). Metadata
`remove` events carry a `reason`: only `closed` (an explicit close) drops the
tab and releases that client's retained terminal input; `shutdown` (server stop
or update) and `restarted` (a failed restart) keep it, as does a reconnect
snapshot that omits a session, so the tab can relaunch its command on attach.
Tabs the client still shows follow the session's current launch command.

The terminal's WebGL renderer is the current instance. xterm keeps its canvas
backing store aligned to the exact device-pixel box by observing
`ResizeObserver`'s `device-pixel-content-box`; WebKit does not implement that
box and throws from `observe`, leaving the backing store misaligned with its
CSS box on any non-integer device pixel ratio, which rescales every glyph.
[`terminalDevicePixelCorrection.ts`](../../apps/web/src/components/terminalDevicePixelCorrection.ts)
restores the correction from `getBoundingClientRect` where the native box is
unavailable, and stays inert where it is. The fallback applies each corrected
backing-store size through xterm's WebGL resize layers so the drawing viewport
and glyph shader resolution change atomically with the canvas; resizing the
canvas alone is not a valid renderer state during center-panel splits.

A terminal session may keep running after its center panel stops rendering. The
per-terminal input scheduler therefore survives renderer unmounts so an
immediate replacement can preserve ordering, but its retained writer is
created outside the renderer effect's closure and captures only the command and
terminal identity. Teardown detaches the transcript and disposes xterm and
WebGL without abandoning input already accepted by the scheduler. A later
renderer retargets error presentation, while the retained writer cannot keep
the departed renderer or its terminal buffers reachable.

Terminal clipboard shortcuts use xterm's trusted native copy/paste events,
including bracketed paste, without requiring the asynchronous Clipboard API.
An image-only paste is uploaded to the terminal's environment, which returns
the path of a server copy for the program to read
([terminal image paste](rpc-and-orchestration.md#terminal-image-paste)).
Shifted copy selects xterm's hidden textarea for the native copy command, then
restores its pending input. Bare Ctrl+C remains terminal input when nothing is
selected. Selecting output keeps keyboard focus in the terminal; **Copy** and
**Add to chat** are available from the selected text's explicit context menu.
Menu copy uses the Clipboard API and falls back to the textarea copy command,
because the native menu can resolve after the click's user activation lapses.
While the running program has enabled mouse tracking (Codex, opencode and other
TUIs), the viewport inverts xterm's force-selection modifier: a plain primary
drag selects text for copying, and Shift+drag (Option+drag on macOS) is
reported to the program. xterm clears its selection on every report it sends,
so while a selection exists a right-click is not reported and, under
any-motion tracking (`?1003`), neither is pointer motion without a pressed
button. xterm's link detection still sees that motion, so links stay
clickable. A click or keystroke clears the selection and reporting resumes.

### Terminal attachment fidelity

Servers advertise `terminalSizeOwnership` (decoded as false for older servers).
The terminal manager owns the applied PTY size. Snapshots carry one optional
`size { cols, rows, sizeClaim }` object, so absent size is distinct from a
complete size with a null owner. They also carry `oscColorResponderActive`,
defaulting to false when absent; the PTY backend reports whether its actual
spawned color responder is active. `firstAttachmentGrant` also defaults to false
and permits one startup-history replay with replies enabled.

Each renderer mount sends a fresh random claim with `terminal.attach`. Under the
generation publication lock, an attachment atomically acquires ownership only if
the session is unowned. It receives `firstAttachmentGrant: true` only when no
claim has been applied in this generation and no claim-carrying stream survived
a restart into it. The grant latch is consumed by a claimed attach, an applied
resize claim, or claimed streams surviving a restart. Later/concurrent attaches
never renew the grant or displace a live owner.

Restart publishes `Restarted`, which resets surviving streams' sequence cursors.
Those renderers see the restarted process's startup output live and may answer
while unowned. Restart without such streams allows one new grant. Reopening an
exited session through `terminal.open` instead publishes a `Started` event with
sequence 1. That event does not reset surviving streams' cursors, so they drop
it and early output (a known limitation of surviving streams). A reopen
therefore leaves the first-attachment grant available for the next
claim-carrying attach.

Resize requests publish `resized { size }` after the PTY accepts the size,
before subsequent output can publish. Claim-only changes publish; exact no-ops
do not. Failed resizes preserve the prior state. Legacy resizes without a claim
clear the owner. Open/attach dimensions initialize new PTYs; attaching to an
existing process preserves its dimensions.

Attachment lifetime owns claim eligibility. Dropping the owner unregisters its
claim and signals a manager-tracked cleanup worker to publish null ownership
under the current generation's publication lock, preserving PTY dimensions.
Attachment identities prevent an old stream from clearing a replacement that
reuses its claim, including across process restarts. Queued requests from an
unregistered claim cannot resurrect it. Manager shutdown fences new claim
registrations, cancels and awaits these cleanup workers, then drains sessions.

A single internal ordered bus carries output and size changes. Public event
receivers filter `resized`, so `subscribeTerminalEvents` retains its original
variant set and resize events are not operational-log records. Attach streams
also filter it unless their request carried a claim. Older clients therefore
never receive the new event; optional snapshot fields remain additive.

The window in use owns size. It claims on focused attach; on terminal focus,
click, or keypress whenever it is a mirror or its fitted size differs; on its
own layout changes while already owner; and through **Fit to this window**. A
null-claim snapshot or event prompts an immediate claim even without focus.
While ownership is null, renderers permit live replies until a claim wins. Each
renderer keeps at most one claim pending until its resize RPC completes, whether
successfully or with failure; the echo updates the applied size. A layout change
deferred during a pending claim is reconsidered on completion; a new
attachment/process snapshot retires the prior generation's request.

The notice stays hidden while claiming and after success until applied size,
ownership, or generation changes, avoiding a flash before xterm parses the echo.
Failure releases that suppression immediately. Mirrors resize xterm to the PTY
dimensions instead of fitting their container; only a different-size mirror
shows **Sized for another window** with fit and dismiss actions. Manual
dismissal is retained by terminal identity and applied dimensions/owner across
renderer and component remounts. It is cleared by a changed applied size/owner
or terminal retirement, alongside the retained input resources. Equal-size
mirrors and the owner show no notice. Clipping or extra space is intentional;
all windows render at the program's width.

The viewport measures before subscribing and includes available fitted
`cols`/`rows` on attach. UI creation uses `terminal.open` with its workspace
admission lease and workspace-loss fence. Newly reserved right-panel terminals
have no mounted renderer and open without dimensions. Center/script opens can
read an existing mounted renderer's fit on demand. Unavailable geometry uses
server defaults, followed by the owner's first measured layout/claim.
Separately, an attach carrying `cwd` can still create a missing session without
the open command's admission lease and workspace-loss fence (a known gap);
attach may arrive before open or recreate a missing session after a server
restart. No extra measuring terminal
or font-settings subscription is needed.

The shared transcript runtime hydrates size and responder activity, and carries
size changes in stream order with raw output. The output sink's xterm write
callbacks apply size/claim after preceding output has parsed and before later
output. Resize signals do not publish lifecycle metadata or allocate a new
transcript. The runtime consumes the first-attachment grant once for the
matching renderer claim; cached remounts cannot reuse it. Raw bounded transcript
replay is retained.

During ordinary reset/history writes, a replay guard drops xterm `onData` until
the final write callback; input typed during this brief interval is also
dropped. The granted first-attachment history pass allows replies. Input before
the first snapshot uses the existing bounded input scheduler and lease instead
of being silently dropped. Reattaching the transcript to the same renderer
preserves its WebGL context. Parser hooks let only the current reply owner
answer CSI device/status/mode queries, DCS status-string queries, and OSC
palette/color queries. OSC 10/11/12 queries are swallowed by every renderer only
when the snapshot says the server color responder is active; otherwise the owner
lets xterm answer. `CSI ? 996 n` stays swallowed (xterm does not implement it).
The PTY reader keeps its synchronous answers and original transcript bytes.
Input is never filtered by reply shape.

Without `terminalSizeOwnership`, clients retain local fit-and-resize behavior
and live replies, with conditional server-color-query suppression. Every replay
is guarded on that transitional path, so startup-history queries are not
answered by those clients.

### Linux AppImage GTK packaging

See the [AppImage child environment policy](#appimage-child-environments).

The desktop build's `beforeBuildCommand` runs
`scripts/prepare-tauri-appimage-tools.ts`, which prepares the repository GTK
plugin wrapper in `target/.tauri` alongside the upstream plugin pinned by URL
and SHA-256. Linux Tauri configuration uses that project-local tools directory
for local, release, and packaged UI builds.

`scripts/tauri/linuxdeploy-plugin-gtk.sh` delegates to the pinned plugin and
preserves discovery calls without an AppDir. After a successful deployment it
validates that `apprun-hooks/linuxdeploy-plugin-gtk.sh` exists and contains
exactly one line each for `export GDK_BACKEND=x11`, the `gsettings` GTK theme
lookup, the `APPIMAGE_GTK_THEME` default, and the `GTK_THEME` export. A missing
hook, absent line, or duplicate line fails packaging before post-processing
changes the AppDir, making upstream drift visible.

The wrapper removes bundled `libwayland-client.so*` files and symlinks under
`usr/lib*` and verifies their absence, keeping the system Wayland client with
the system Mesa/EGL stack. It then rewrites the hook's export to
`export GDK_BACKEND="${BIBCODE_GDK_BACKEND:-wayland,x11}"`. GTK tries Wayland
first and falls back to X11; the override is evaluated when the AppImage
launches. `BIBCODE_GDK_BACKEND=x11` restores the previous backend selection.
Preferring native Wayland avoids oversized rendering from integer GTK scaling
under Xwayland on fractionally scaled Hyprland/Omarchy desktops.

The wrapper also removes the GTK theme lookup and default, and replaces the
forced `GTK_THEME` export with a conditional export only for a nonempty,
user-supplied `APPIMAGE_GTK_THEME`. An inherited `GTK_THEME` otherwise passes
through unchanged. It verifies that no forced export remains and exactly one
conditional override exists. All post-processing runs before AppImage
assembly and updater signing.

On Linux AppImage startup (`APPIMAGE` set), the host keeps the bundled,
known-good Adwaita theme through a process-only `gtk-theme-name` GtkSettings
override, unless the user supplied a nonempty `GTK_THEME` or
`APPIMAGE_GTK_THEME`. Application-set GtkSettings outrank session XSETTINGS
without changing the user's desktop settings. Unlike `GTK_THEME`, this keeps
the light/dark variant free to follow `gtk-application-prefer-dark-theme`.

The async `DesktopBridge.setTheme` command resolves **System** on Linux through
the portal settings `Read` method on a blocking worker, then applies that
scheme explicitly to avoid tao's `SetTheme(None)` forcing light. It uses the
existing GTK/GIO dependency: `WebviewWindow::theme()` would dispatch the
blocking portal call back to Tauri's main thread, even from an async command.
The direct read is independent of any explicit window theme and uses tao's
five-second call timeout and light fallback if the portal is unavailable.
Tao's portal subscription continues to update the variant live. Linux managed state remembers
the last requested choice and serializes theme commands; a main-thread
GtkSettings notification handler reasserts **Light** or **Dark** if a portal
change conflicts with that explicit choice. **System** never reasserts, and an
already-matching notification is a no-op. The webview and native menubar/dialogs
therefore share the selected variant, subject to user environment overrides.
Other platforms retain `set_theme(None)` for **System**.

### Linux webview text rendering

The Linux host and the web UI share responsibility for text quality in
WebKitGTK. The web side applies its Linux rules only when a desktop bridge is
installed and the user agent reports Linux:
[`linuxWebkitTypography.ts`](../../apps/web/src/linuxWebkitTypography.ts) sets
`data-linux-webkit` on the root element before the first render, so browser
mode on Linux and the macOS and Windows hosts are unaffected.

**Hinting.** WebKitGTK disables subpixel glyph positioning and fully hints
glyphs when GTK reports `gtk-xft-hinting` enabled with `gtk-xft-hintstyle`
`hintfull`, which renders DM Sans body text with uneven whole-pixel letter
gaps. During setup the host reads those GTK values and, only in that state,
sets its own process-local `gtk-xft-hintstyle` to `hintslight`
([`linux_text_rendering.rs`](../../apps/desktop/src-tauri/src/linux_text_rendering.rs)).
The trigger is the GTK value, whichever desktop produced it; the user's system
preference is unchanged, and the host's native menu and dialogs use the same
pinned hinting. The override runs after Tauri has created the main window and
reaches the webview because WebKitGTK propagates GtkSettings changes to its web
processes; it is not re-evaluated if the session setting changes while the app
is running.

**Density.** Under the pinned hinting the Linux CSS in
[`index.css`](../../apps/web/src/index.css) renders body text at weight 450 and
`pre` and `code` at 400 to keep stroke density, and never adds letter-spacing,
because fractional per-glyph advances snap into whole-pixel gaps under hinting.

**LCD text in scrollers.** WebKitGTK paints text inside a composited scroll
container with grayscale antialiasing unless an ancestor inside that scroller
paints an opaque background. The scroller's own background does not count, a
nested scroller starts over, and alpha theme tokens such as `--muted` and
`--accent` never count. A `mask-image` on the scrolled content or an ancestor
with `opacity` below 1 forces grayscale even with a painted ancestor. Reading
surfaces therefore mark the content wrapper inside their scroller with
`data-text-surface="background|card|sidebar|popover"`, which the Linux CSS paints with
the matching opaque token, and the Linux CSS removes the `ScrollArea`
scroll-fade mask, so Linux has no scroll fade. Coverage today is the sidebar
content, the chat timeline rows, the settings column, the Agents list, and the
Create pull/merge request dialog with its branch and option lists. Other
scrollers, including remaining menus, dialogs, popovers, the git manager lists,
and file preview, render grayscale text on Linux until they adopt a wrapper.
Any new scrollable reading surface must add one.

## Request and event flow

1. The client runtime resolves a connection target and obtains any required
   bearer, DPoP, relay, or SSH authorization.
2. `RpcSessionFactory` opens a WebSocket and starts the connection's one
   `subscribeServerConfig` stream; its first snapshot is the session's initial
   configuration, and later config subscribers replay the same stream.
3. Effect RPC schemas encode requests and decode unary results or streams.
4. The Rust `RpcRegistry` authorizes and routes each method.
5. Orchestration commands are admitted and persisted before provider delivery.
6. Provider runtimes translate commands to provider-native protocols and feed
   normalized events back into durable projections.

Messages submitted while a turn runs enter a durable, server-owned FIFO and
appear as queued cards without marking new work active. When the session settles
to ready, the oldest eligible message starts the next turn with its saved model
and modes and a new promotion timestamp. Codex and Claude support explicit
steering through their native protocols; Cancel restores queued text to the
cancelling client's composer, and Stop drains the queue before interrupting.
Pending approvals/questions block automatic sending, interrupt/error settles
hold the queue, and queued rows survive reloads and server restarts.

See [RPC and orchestration](./rpc-and-orchestration.md) and
[Connection runtime](./connection-runtime.md) for the detailed boundaries.

## Boundaries and invariants

- React does not import Rust or native-host implementation details.
- Privileged desktop behavior crosses `DesktopBridge` commands and events.
- Whole-catalog renderer updates use fresh reads and exact compare-and-set.
  Compare-only transitions validate the same fresh raw revision without a
  document write. In browser and unprotected desktop modes each transition owns
  one IndexedDB `readwrite` transaction; on a protected desktop host both own
  the same native catalog-owner lock and are never emulated with separate
  JavaScript bridge calls. Before protected bridge publication, a native-null
  host adopts a non-blank legacy catalog by CAS and confirms the authoritative
  native winner before clearing the renderer copy. A failure preserves both
  sources and leaves catalog operations fail-closed. Unprotected legacy
  migration preserves an existing valid IndexedDB winner and uses exact CAS
  before replacing corrupt IndexedDB bytes with the only valid legacy catalog.
  A connection-database `VersionError` and WebKit's permanent open failure,
  `Unable to establish IDB database file`, are handled before the connection
  runtime starts. The WebKit failure is produced when an older bundled WebKitGTK
  opens a database migrated by a newer one. The shell offers a non-destructive
  reload plus a separately acknowledged destructive reset that a double-click
  cannot confirm. A blocked deletion remains visibly queued on its original
  request until success or error; the shell reloads automatically only after
  actual deletion success. Otherwise unavailable databases remain
  non-destructive and show actionable health instead of looping at boot.
  Outside that migration, a corrupt authoritative catalog is never rewritten
  as empty: it is quarantined when supported, publishes redacted
  recovery-required health, blocks mutation, and requires an explicit
  exact-revision reset. The shell cannot confirm an empty project list while
  this health is active.
- Accepted storage-identity decisions are atomic catalog transitions: the
  decision is recomputed after every conflict and returned only from the
  winning revision. Keep decisions compare without writing; Set decisions use
  CAS. Both the prepared HTTP descriptor and the WebSocket session's
  initial configuration must pass that owner before synchronization, live
  publication, or cache consumption.
- Project count is never used as an availability signal. The client retains
  accepted cached shell snapshots through reconnect and blocked states, and
  only a successful live shell snapshot can replace those rows or confirm an
  empty project catalog. Catalog loading, zero desired environments, and any
  non-live desired environment remain explicitly non-authoritative.
- Desktop catalog capability is fail-closed and tri-state. Only bridge version
  3 with an explicit protected/unprotected flag may select native compare-only,
  native CAS, or IndexedDB migration; rejected, older, or incomplete metadata
  and failed protected-source collapse perform no IndexedDB write, native
  overwrite, or renderer clear.
- WSL-only desktop startup never falls back to a native Windows backend.
  Planning and primary-start failures remain tagged through the desktop bridge;
  only an explicit settings action switches the primary runtime to Windows.
- Forced linked-worktree deletion is admitted only after the server verifies
  the persisted workspace-thread owner, fences the canonical target path plus
  every server-known terminal under it, closes their current processes, and
  blocks open, attach, and restart for the duration of the VCS operation. The
  server persists a removal receipt keyed by the workspace thread and binds it
  to a random identity stored durably in both the linked worktree's Git
  administrative directory and its root. The repository revalidates that
  administrative `.git` file, backlink, and identity inside the removal
  transaction, then atomically renames that exact checkout through its bound
  filesystem handle on Windows to a deterministic nonce-bound quarantine. A
  tiny verified tombstone, atomically created with its bound handle on Windows,
  occupies the registered
  path while the server atomically moves the nonce-verified administrative
  directory out of Git's `worktrees` namespace. No path-based Git delete can
  race with a replacement. Recursive cleanup begins only after deregistration,
  holds the checkout, tombstone, and Git-administration leases continuously,
  pins the verified filesystem object against rebinding, verifies descendant
  identities, rejects Windows reparse points at the root and descendants, and deletes the empty
  root through its handle. An empty registered path without the transaction
  marker fails closed. A Windows process that still owns the checkout as its
  current directory therefore makes the initial rename fail before any file is
  deleted. The receipt and deterministic quarantine make interrupted cleanup
  safely retryable across client and server restarts. Once cleanup completes,
  the receipt makes a stale retry a no-op even if another worktree later reuses
  that path. Compatibility cleanup of a
  previously deregistered directory is limited to a normal directory without a
  `.git` entry whose canonical parent is exactly BiBCode's computed
  per-repository worktree namespace; primary checkouts and arbitrary paths are
  never pre-cleaned.
- Configured secondary WSL planning/start failures remain desired, unavailable
  topology with stable identity and no endpoint/session. They do not remove
  cached project/thread state; explicit disable or distro replacement does.
- Desktop update installation requires a verified pre-update backup for the
  primary and every non-excluded running secondary by default. Only a failed
  protection attempt followed by explicit user acknowledgement may bypass all
  backups. Protected and bypassed attempts both stop the captured running set
  before invoking the installer and restart that exact set on failure.
  In-process backend shutdown preserves desktop-owned WebView descendants.
- Normal application traffic uses HTTP and WebSocket RPC in every host.
- `packages/contracts` remains schema-only.
- Rust owns all production backend behavior. TypeScript is limited to clients,
  contracts, shared utilities, relay infrastructure, and development tooling.
- The Git Manager performs no repository lifecycle: it cannot add, create,
  clone, publish, remove, or delete a repository, and every request remains
  scoped to the checkout selected for its project.
- The server's Pull Requests module owns GitHub/GitLab context, vocabulary,
  list, detail, timeline, commit, check/pipeline, and file reads through typed RPC and a single bounded `ProcessRunner`
  wrapper. Host detection, authentication, and capabilities are server-owned;
  permissions and merge readiness are computed only on the server, and reads
  start only on explicit requests. Successful CLI probes, GitLab context
  observations and GitLab tab totals use bounded 30-second caches (32 entries
  each); opening the module reuses them, while Rescan (`getContext` with
  `rescan`) and auth failures invalidate them, and mutations still re-read
  repository access/policy and request permission/head observations. No worker
  polls them. `source_control::ProviderHosts` is the server's in-memory record of
  authenticated hosts that explicit probes identified (Pull Requests scope
  resolution, and Settings discovery only when it sends `recordHosts`; at most
  64, forgotten on Rescan, `unknown_host` and `not_authenticated`). One shared
  instance serves Git status, summaries, Settings discovery and Pull Requests,
  which classify origins by host name first and then through it, without
  spawning a provider CLI. The `create_pr`/`commit_push_pr` stacked actions
  resolve the provider before any branch, commit or push, and report a created
  request to the Pull Requests service (`CreatedRequestObserver`) so its cached
  GitLab totals for that repository are read again.
  The web module adds project-scoped
  list/detail routes, capability/connection gating, and a two-project persisted
  view-state cache. The detail surface mounts the header and active tab's query
  (Files also reads timeline for anchored threads), virtualizes timeline/files,
  renders diffs near the viewport, and replaces remote markdown images with
  browser links. Review, metadata, merge, and state writes share one command hook and refresh
  the affected query atoms without optimistic cache changes. Comment, reply,
  title/description, merge-message, inline-composer, pending-review, and
  review-summary drafts survive navigation and reload. Metadata changes offer
  an explicit five-second Undo; merge confirmations pin the loaded head.
  Checkout shares the per-request action gate and worktree catalog guards,
  creates managed workspace ownership for new worktrees, and keeps started Git
  writes alive through client disconnect until settlement (with the documented
  24-hour per-command safety ceiling). Only `gh`, `glab`, and `git` run through
  the module’s process boundary; it owns no HTTP client or background poller. See
  [Pull Requests contract boundary](./rpc-and-orchestration.md#pull-requests-contract-boundary).
- Git Manager force-push always uses `--force-with-lease`. Its execution paths
  forbid bare `--force`, `--ignore-other-worktrees`, forced
  `git worktree add -f`, and plumbing `update-ref` as ways to bypass the
  server's worktree-aware guards. `merge-into` publishes its merge commit with
  fast-forward-only `git fetch . <commit>:refs/heads/<target>`, which Git
  refuses for a branch checked out, rebased, or bisected in any worktree; it
  never uses `update-ref`.
- Git worktree registration, directory availability, and path ownership are
  resolved by the server catalog. Clients do not infer recovery from directory
  existence or treat a degraded observation as an authoritative empty set.
  Persisted thread projections resolve ordinary and panel aliases to the same
  physical workspace, and short-lived server admission leases serialize new
  durable/process publication against authoritative loss and bounded cleanup.
  Loss cancellation remains attached to queued engine work after an RPC caller
  disconnects. Each queued durable turn also carries an owned synchronous
  finalization fence into the SQLite worker: authoritative loss either rejects
  the transaction before commit or waits for the local commit to finish before
  publishing the guard. The runtime starts canonical cleanup immediately under
  one five-second deadline while resolving any persisted aliases in parallel.
- Capability negotiation controls optional behavior such as activity and
  preview automation; clients must downgrade when a server cannot prove support.
- Host WebView engines differ by platform, so optional browser APIs are
  feature-detected and given a frontend fallback rather than assumed present.
- Activity observation is bounded, authorized, and independent for structured
  provider chat and managed provider terminals. See
  [Activity observation](./activity-observation.md).

## Performance

The Tauri/Rust migration retained the React frontend while removing the bundled
Chromium/Electron shell and Node server process. Historical measurements and
the repeatable capture commands are recorded in the
[Desktop Performance Baseline](./desktop-performance-baseline.md).

The native process sampler starts with an empty process table, so constructing
the production runtime does not enumerate host processes. A requested diagnostics
sample or non-empty process-ownership cleanup refreshes the host process table on
the blocking pool. Full discovery remains necessary to identify previously unknown
descendants for attribution and cleanup; direct PID identity checks use the native
targeted lookup independently of that table. CPU observations establish their
baseline on the first requested sample and retain it for subsequent samples.
