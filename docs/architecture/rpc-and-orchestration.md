# RPC and orchestration

BiBCode uses Effect RPC over one authenticated WebSocket per connected
environment. The same protocol is used by browser and Tauri clients; the
desktop bridge is reserved for host-native capabilities.

## Full orchestration HTTP snapshot

The existing `GET /api/orchestration/snapshot` endpoint requires
`orchestration:read` and returns the declared `OrchestrationReadModel`.
Its camel-case project/default-thread identities and full message, activity,
plan, checkpoint and session records come from one full repository snapshot
load, the only remaining reader of the whole store.
The HTTP projection includes archived and deleted records with their markers;
WS shell snapshots retain their existing active/archived filtering.
Shared pure project/thread serializers keep the public field policy aligned,
with borrowed rows grouped once per thread for the full response. Persistence
states, command receipts and raw projection column names are not wire fields.
HTTP fields declared optional rather than nullable are omitted when absent;
the existing WS shell/thread output remains unchanged.

## Provider capabilities by workspace

`server.getProviderCapabilities` is a read-only unary RPC with
`{ instanceId, cwd }` input, authorized by `orchestration:read`. It returns the
selected provider's commands, skills,
agents, and partial-discovery issues for that directory and its user configuration.
Invalid contexts and failed discovery use `ServerProviderCapabilitiesError`.
Successful catalogs are served from a server cache and refreshed in the
background ([providers](./providers.md#workspace-capability-discovery)).
The request cannot override server-owned provider settings or credentials. It
uses the same authenticated environment RPC path in browser and desktop clients.
See [provider capability ownership and lifecycle](./providers.md#workspace-capability-discovery).

## CLI session import

`agentSessions.scan { projectId }` and
`agentSessions.import { projectId, sessions: [{ provider, sessionId }] }` are
unary RPCs that both require `orchestration:operate`, because scan returns
transcript content (first prompts as titles) from the server host's home
directory, outside BiBCode's own data. They are owned by
[`agent_sessions_rpc`](../../apps/server/src/production/agent_sessions_rpc.rs);
the bounded transcript reader is
[`agent_sessions`](../../apps/server/src/agent_sessions/mod.rs). Both fail with
`AgentSessionsError { message }`.

Scan reads Claude Code and Codex transcripts on the **server host** for the
built-in `claudeAgent` and `codex` instances (a disabled driver is not
scanned). It lists sessions whose first recorded working directory is the
project's workspace root after canonicalization, whose file changed in the last
30 days, newest first, at most 200 (`truncated` reports more). Each candidate
carries title, last activity and visible message count. `alreadyImported` is set
once thread `import:<instanceId>:<sessionId>` was deleted or its history command
was accepted, and `threadId` names that thread while it is live; an import
interrupted before its history committed stays selectable. Sessions another
BiBCode thread already runs (a `provider_turn_outbox.provider_session_id` or a
`provider_session_runtime` cursor owned by a thread other than their import)
are left out, because BiBCode's own conversations write into the same homes;
import skips them too. The scan keeps only metadata; import reads the
transcript again.

Import handles each requested session independently and reports it as
`imported { sessionId, threadId }` or `skipped { sessionId, reason }`. It
re-reads and re-validates the transcript, then:

1. dispatches `thread.create` with command ID `<threadId>:create`, unless an
   interrupted import already created the thread;
2. inserts a `suspended` `provider_session_runtime` row with the CLI session as
   its resume cursor only when the thread has no row, turn, or user message yet
   (see
   [imported CLI sessions](./providers.md#imported-cli-sessions)); a thread that
   started its own conversation before an interrupted import finished keeps it
   and the session is skipped;
3. dispatches the server-internal `thread.history.import` with command ID
   `<threadId>:history`, which emits one `thread.message-sent` per message
   (`turnId: null`, `streaming: false`, no attachments) for at most 200
   messages: the first user message and the latest ones. Its events carry
   `metadata.historyImport: true`, so effects capture no checkpoint baseline
   for them; the first real turn takes its own. The engine rejects it
   once the thread has a turn or a user message, so a turn another client
   started during the import keeps CLI history out of its conversation.

Both commands use a digest of the step and thread ID, so a retry after a
partial failure replays the committed steps; an accepted history command marks
the session imported. A deleted import keeps its thread ID and is skipped
rather than recreated. `thread.history.import` is rejected by public
`orchestration.dispatchCommand` like every other server-internal command.

## Session establishment

`ConnectionResolver` first produces a `PreparedConnection`. Remote bearer and
DPoP clients exchange their credential for a short-lived, one-purpose
WebSocket ticket and put only `wsTicket` on the `/ws` URL. `RpcSessionFactory`
then opens the socket, builds the Effect RPC client, and opens the connection's
one `subscribeServerConfig` stream. The session is ready only after the socket
opens and the first snapshot arrives; later config subscriptions on the same
session replay that stream.

The client allows 15 seconds from attempt start to a connected socket (for E2EE,
after authentication), then bounds the first snapshot by liveness and a
120-second configuring ceiling; see [Connection runtime](./connection-runtime.md#state-and-retry-policy).

Primary desktop/browser bootstraps may already have a host-authorized socket
URL, but they enter the same session and RPC pipeline.

## Wire protocol

The TypeScript client is built by
[`makeWsRpcProtocolClient`](../../packages/client-runtime/src/rpc/protocol.ts)
from the schema-only `WsRpcGroup`. The Rust mirror is
[`apps/server/src/rpc/message.rs`](../../apps/server/src/rpc/message.rs).

| Direction        | `_tag`                | Purpose                                                                           |
| ---------------- | --------------------- | --------------------------------------------------------------------------------- |
| Client to server | `Request`             | Numeric-string request ID, RPC tag, payload, headers, and optional trace context. |
| Client to server | `Ack`                 | Acknowledge streamed values for flow control.                                     |
| Client to server | `Interrupt`           | Cancel one request and its server work.                                           |
| Client to server | `Ping` / `Eof`        | Probe or close the protocol session.                                              |
| Server to client | `Chunk`               | Deliver one or more stream values.                                                |
| Server to client | `Exit`                | Complete with an Effect success or typed failure cause.                           |
| Server to client | `Defect`              | Report a protocol/session defect not tied to a normal typed failure.              |
| Server to client | `Pong`                | Answer a protocol probe.                                                          |
| Server to client | `ClientProtocolError` | Report malformed or unsupported client protocol input.                            |

A stream has one `Chunk` in flight: the server sends the next only after the
client's `Ack` for the previous one. Items the handler queued meanwhile leave
together in that next `Chunk`, in order, up to 64 KiB of encoded values (or the
connection's message limit when smaller); a failure or an item that does not
fit goes out after it. A slow link therefore costs a stream one round trip per
`Chunk` rather than one per item: before this, typing over a 250 ms link echoed
one key per round trip.

Schemas validate payloads at the client boundary. The Rust session validates
request IDs, registered method names, authorization scopes, cancellation, and
stream flow before invoking handlers. The read loop never waits on the
database: it checks IDs, capacity and the method's declared scope at once, then
starts the request's task, which authorizes the session against the auth store
and takes the maintenance admission before the handler runs. A request that
waits there is in flight, so `Interrupt` cancels it, and it keeps an E2EE
message's inbound byte permits until it is admitted. A refusal (a missing scope,
an invalid session, maintenance) that cannot be queued before its 5-second
deadline goes through the control lane, and one that reaches neither ends the
session. A periodic task that holds the single database worker therefore delays
only the requests behind it, and the read loop keeps answering `Ping`. When a
request's task ends, however it ends (a reply, a stream terminal, a refusal, an
interrupt or a panic), the session cancels the request's cancellation token, so
work its handler started ends with the request rather than with the connection.
A stream producer that kept waiting for events after its error terminal held an
engine event receiver for as long as the socket stayed open (#113).

Each connection has one writer task that owns the socket sink. It drains a
bounded control lane before every queued message: RPC `Pong`, interrupt exits,
`RpcOutboundAdmissionError` terminals, and client protocol errors (80 entries,
one per in-flight request plus room for Pongs). A `Ping` never ends the read
loop; when the lane is full its `Pong` is dropped, because the client's
liveness counts any inbound data. Terminal output and other data never use the
lane. Every write is bounded by progress: a record must be accepted within 20
seconds, a whole frame within its message deadline, and every message within
30 seconds plus its size at 16 KiB/s. When a deadline passes, the writer ends
the session at once, so the socket closes instead of staying open and silent.

Plain and E2EE sessions share one heartbeat rule. Once a socket is
authenticated (plain `/ws` after the upgrade, E2EE after `e2ee_authenticated`),
the heartbeat asks the writer for a WebSocket Ping every 15 seconds, which it
sends between frames or records. Ping deadlines advance from their scheduled
times, so small check delays do not accumulate. Every inbound frame, including
the browser's automatic Pong, counts as activity. A data write counts only if it
had to wait at least 100 ms for the peer to drain the socket before it completed; a write the
socket accepts at once, or after a brief internal handoff, proves nothing about
the peer, so small frames the server keeps sending to a frozen client never move
the silence origin. No control write (Ping, Pong, interrupt, admission terminal or
protocol error) is data progress. The heartbeat checks every 5 seconds and ends the session after
45 seconds without activity, so a stopped reader is reaped within 50 seconds.
A check that fires more than 10 seconds late, because the process was suspended
or starved, restarts the silence clock and Ping schedule with one immediate Ping
instead of reaping or sending catch-up Pings.
How a session ended, and the info log line an abnormal end writes, are described
in [Connection ends](#connection-ends).
Inbound activity counts per complete frame, so a single plain request that
takes longer than 45 s to arrive on an otherwise idle connection is reaped (and
the client's own 30 s monitor ends it first); E2EE requests travel in records.
[Remote environments](./remote.md#direct-connection-e2ee) covers what reaping
releases on E2EE sessions.

A plain `/ws` client may offer the WebSocket subprotocol
`bibcode.rpc.chunked.v1`. When the server selects it, every RPC message over 64
KiB leaves as binary frames, one record per frame, in the E2EE record format:
`0x00` for the final record, `0x01` for a continuation, and `0x02` for a
stand-alone control message sent between the records of another message, which
the client returns without touching the partial message. Smaller messages stay
whole text frames, individual requests are never split, and the limits are 64 MiB and
2,048 records per message. Without the echoed subprotocol the server keeps
today's whole text frames. E2EE sockets always use records; `0x02` control
records are used there only when the client lists `interleave-v1` in
`e2ee_auth` and the server confirms it. Likewise, `0x10` marks every record of a
raw-deflate compressed server message only after the client lists `deflate-v1`
and the server confirms it ([Remote environments](./remote.md#direct-connection-e2ee)).

A plain client that can inflate raw deflate offers
`bibcode.rpc.chunked-deflate.v1` before `bibcode.rpc.chunked.v1`, except on a
loopback socket that is not an SSH tunnel (a backend on the same machine). The
server prefers it unless `BIBCODE_REMOTE_COMPRESSION=0` is set. Selecting it means
chunked records as above, and a data message of at least 16 KiB that compresses
smaller leaves in the `deflate-v1` form: binary records that all carry `0x10`,
even below 64 KiB. A server that predates it selects `bibcode.rpc.chunked.v1`
([design](../superpowers/specs/2026-10-13-e2ee-message-compression-design.md#addendum-2026-10-17-compression-on-plain-ws)).

A response that does not fit the connection's message limit (64 MiB on E2EE and
record-framed plain sockets) fails only its own request with a typed
`RpcResponseTooLargeError { method, bytes, limitBytes }`; the session stays
open. Clients decode it for every method through the client-runtime
`RpcTransportErrors` middleware, and its message reads "This result is too large
to send (<size>; limit 64 MiB)."
An unpaged `orchestration.replayEvents` stops reading and fails with the same
error as soon as its events pass 64 MiB, on every framing, so `bytes` is a lower
bound there.

## Server composition

`ProductionRuntime::start` constructs the durable services first, then
registers their RPC adapters in `RpcRegistry`:

- `OrchestrationEngine` owns command admission, persisted events, snapshots,
  and projections;
- `ProviderRuntimeSupervisor` owns provider session processes and native
  protocol adapters;
- `ActivityCancellationService` owns bounded, generation-fenced targeted
  cancellation admission and dispatches only server-held provider targets. The
  provider supervisor accepts those targets only for the matching current live
  session, runtime generation, and control registration, then invokes the
  driver's targeted-cancellation seam outside the ordered root turn-delivery
  lane. Drivers without an exact provider-native adapter fail closed with
  `targetUnavailable`; this path never translates an activity request into a
  root turn interrupt;
- `TurnDeliveryService` routes admitted turns to provider runtimes while
  preserving delivery and recovery invariants;
- activity, preview, Git/VCS, terminal, settings, diagnostics, authentication,
  and lifecycle services register their own unary or streaming methods.

The authoritative method inventory is
[`ACTIVE_RPC_METHODS`](../../apps/server/src/rpc/methods.rs). The authoritative
authorization mapping is
[`required_scope`](../../apps/server/src/auth/scope.rs); adding a live method
without exactly one declared scope fails a server test.

## VCS status and mutation coordination

`subscribeVcsStatus` and `vcs.refreshStatus` add the optional
[`repositoryUnavailableReason`](../superpowers/specs/2026-09-26-vcs-repository-state-design.md)
to their local status part when `isRepo` is false; the production server
coordinates their work by canonical worktree path.
The status owner has independent Local and Full read keys. Concurrent callers
for one key share one physical load while retaining cancellation leases; one
caller leaving does not cancel peers, while final-lease release removes and
cancels that load. Subscription bootstrap, delayed local fallback, workspace
invalidation, and explicit full refresh all use this owner.

`subscribeVcsStatusSummary` is the read-scoped passive counterpart. Its
latest-value stream shares one 30-second producer per canonical cwd; a
subscriber that joins a running producer receives its current value at once
instead of waiting for the next publication. It emits only repository identity, dirty, provider, matching named-branch PR,
observation time, and stale state. It performs no numstat, full-file storage,
or fetch. After a failed status read, the shared repository probe and bounded
classifier decide the same optional `repositoryUnavailableReason` as the status
stream: malformed config or an ownership refusal becomes `isRepo: false`
instead of an error, with a reason only when the server can tell.
Clients decode a reason they do not know, in either stream, as absent.
A finished local mutation or a reported local change (a terminal
command exiting) for the same worktree starts a fresh cycle at once, so passive
labels such as the sidebar branch follow a checkout without waiting for the
deadline; status reads, including watcher-driven reads, also nudge it when
repository availability (`isRepo` or `repositoryUnavailableReason`) changes, so
the sidebar follows a break or repair at once.
The left panel reads a card's branch, PR badge and dirty dot from the latest
summary even when it is stale: a failed PR lookup marks a just-read base stale,
and a stale PR is the last one the server read for the summary's own ref and
provider. Hiding them would show an older branch, a missing PR, or a clean
checkout. A thread's recorded `branch` is the fallback when no summary names
one, and a workspace card's PR badge matches the branch the card names. An
Agents view row names its branch by the same rule while its environment is live.
Each successful base cycle publishes current local and provider state. A same-ref/provider PR from the immediately prior cycle may be carried
only into cycle N+1 while enrichment runs or fails, preserving its original
`observedAt` with `stale: true`; cycle N+2 expires it unless enrichment refreshes
it. An initial base-load failure with no prior value remains an error. If PR
enrichment fails without an eligible carried PR, the stream retains the
just-published current local/provider base as stale with `pr: null`; successful
provider or open-PR absence clears the corresponding fields on the next fresh
value.
Final-subscriber release cancels and awaits the in-flight read before removing
the generation, so an immediate alias reattachment cannot inherit or be erased
by old lifecycle cleanup. Final worktree-lifecycle cancellation also retires the
status-owner entry once active mutations and cancellation-ignoring physical
reads finish, so paths that are never reattached do not accumulate owner state.

The first active status subscriber resolves worktree, Git directory, and common
directory with one bounded, cancellation-aware Git command. When Git cannot
resolve them, the watcher uses the nearest `.git` directory with a repository
marker found by the classifier's bounded discovery walk, with its parent as the
worktree root, so a repaired HEAD or config triggers a read. These fallback
roots feed only the watcher; Git Manager identity and fetch attachment still
resolve through Git. Plain folders, gitfiles, and trust changes still converge
through the safety read or **Retry**. The server installs
native watches for those admitted execution-host paths before the initial local
read. Watcher readiness is proved by a final, test-owned-style sentinel watch in
a unique temporary directory outside all user and Git roots. Setup attempts
cleanup of that watch and directory on every outcome, never writes into an
admitted root, and treats cleanup failure as setup unavailability.
Readiness failure, watcher loss, overflow, or unavailable delivery preserves the
subscription in sticky fallback health. An ordinary later event cannot mark it
healthy.

When a local or full status observation, including a new subscription's snapshot,
sees a previously unavailable repository become available, its active lifecycle
reattaches automatic fetch if it has no fetch identity. One owned common-directory
resolution runs at a time; rapid break/repair observations coalesce into a trailing
resolution of the latest available state. Resolution and publication cross the
same mutation epoch fence as status reads, so a retired read waits for settlement
and resolves again. Publication verifies the lifecycle and attaches only its
current subscribers to the shared physical-repository fetch owner. Final release
and shutdown cancel and drain that owned work. Healthy unchanged observations and
already attached lifecycles perform no additional attachment discovery.

Working-tree and metadata signals use a 125 ms trailing debounce. A signal that
arrives during a physical local read retains exactly one trailing read. Explicit
mutation and workspace invalidations bypass that debounce, as do structured
terminal-process completions; focus, reveal, and Git-menu catch-up retain their
explicit refresh path. While a subscriber remains active, the local scheduler
also reads after `max(60 seconds, 4 * last completed read duration)`, capped at
300 seconds. Remote fetch and post-fetch remote reconciliation have separate
workers, so blocked remote work does not delay local invalidation. No periodic
`symbolic-ref` worker remains: watcher and safety local observations read branch
identity from the authoritative porcelain result.

Each canonical worktree has one lifecycle identifier and owned task tracker.
Final release advances the shared status-read epoch before its retirement fence
can admit a replacement, cancels the scheduler and watcher, and waits for all
old lifecycle tasks before retrying subscription setup with a fresh watcher
generation. First-lifecycle task insertion is reserved atomically with
registration, including concurrent backpressure removal. Shutdown closes the
broadcaster and status-read admission fences, rejects later subscribers, and
awaits subscription setup, physical status workers, shared
physical-repository fetch workers, retired generations, and active lifecycle
tasks; late reads cannot publish into a later lifecycle. Fetch shutdown clears
repository and worktree ownership only after closing admission, cancels every
worker, and retains join ownership even when an injected process runner ignores
cancellation.

A status subscriber whose bounded queue fills is removed; its stream delivers
all queued publications and then ends with a `GitCommandError` whose operation
is `GitStatusBroadcaster.fellBehind`, also used when setup exhausts its admission
retries. The client runtime resubscribes only on that error with backoff starting
at 250 ms, doubling to a 30 s cap, and resetting after the stream stays up for
30 s. Shutdown and cancellation still end the stream without an error.

`ProductionRuntime` connects only the structured `TerminalManager` process-exit
callback to local invalidation. An explicit worktree path has priority;
otherwise the callback chooses the deepest canonical active worktree ancestor
of the command cwd and does nothing when none is active. The terminal's Exited
event and metadata publication complete without holding the generation
publication lock across that callback. Provider lifecycle, session, completion,
and delivery events do not enter this path. Commands typed inside a persistent
interactive shell have no structured completion metadata, so their changes rely
on execution-host native watching and the 60-second safety fallback. Filesystem
changes that occur only on an unmounted remote host are outside the native-watch
boundary and likewise require an explicit invalidation or safety convergence.

Post-fetch remote reconciliation acquires the same worktree epoch before Git
work and rechecks it around publication. Stream and
unary pull-request enrichment carry that producing epoch internally through
their final delivery check; the public status schemas contain no epoch field.
Cached remote state is reused only with a local snapshot from the same epoch
and ref identity. A watcher, safety, or explicit local observation of a ref
change clears server cache,
requests current-branch reconciliation, and makes the shared stream reducer
reset upstream, ahead/behind, default-delta, and pull-request fields while
same-ref local invalidation preserves them.

A physical observation starts with one
`git -c core.quotePath=false status --porcelain=2 --branch --untracked-files=all`.
Its headers supply branch, upstream, and ahead/behind state. Staged and
unstaged numstat commands run concurrently only when the porcelain records show
those areas. Each unmerged record yields one `conflicted` entry in the unstaged
area, and the Git Manager in-progress detector runs alongside the remote-name
read to fill optional `operationInProgress`; a failed probe omits the field and
never fails status. Default-ref, remote-provider, and pull-request enrichment
remain separate when the requested result needs them. If porcelain fails, only a
successful existing repository probe returning false maps to the compatible
non-repository result; otherwise the original structured Git error is returned.
Status and background-observation Git reads use `GIT_OPTIONAL_LOCKS=0`. Fetch
and mutation paths do not.

Every effectful registered Git mutation is validated before admission and then
crosses one status-mutation guard for its whole operation. Admission closes the
read gate, advances the epoch, and retires Local and Full reads. The server-owned
continuation retains the workspace admission, process cleanup, terminal result,
and guard even if the RPC waiter is interrupted or dropped; panic still reaches
the outer RPC defect boundary after guard settlement. Empty file lists and
decode, path, mode, or pull-request resolution failures that have not reached a
Git effect do not fence status. Stacked actions acquire one guard around the
complete action and repository calls do not reacquire it.

`sourceControl.publishRepository` likewise validates its provider, repository,
visibility, remote, and protocol before admission, then owns GitHub repository
creation plus the push as one mutation through terminal success, partial
failure, panic, or caller disconnect. Repository and remote operands cannot be
option-shaped; GitHub receives `--remote=<name>`, and the final Git command
pushes and sets upstream for exactly that selected remote with option
termination, regardless of an older branch upstream.

`projects.writeFile` uses the same guard. After admission it continues through
the terminal filesystem result and then settles the status guard. A successful
content-only write to an existing ordinary file retains the workspace entry
index; creating a path or writing a built-in Git classification control
invalidates it. An error or panic after a possible filesystem effect invalidates
the index before preserving the original failure or unwind. Managed worktree
create, retarget, detach, and destructive removal do not move under the VCS
mutation lock: their existing command/project/repository lock order and
runtime-owned continuation settle first. After terminal durable or Git outcome
and catalog-lock release, they notify the affected canonical status paths and
invalidate only their physical-repository fetch-owner keys. Replay-only and
preflight failures do not notify; partial Git/durable failures that may have
changed state do.

Automatic fetch is owned separately by canonical `--git-common-dir`. The first
subscription lifecycle resolves that key with one bounded read. One owner keeps
the live interval and bounded failure backoff, discovers the attached branches'
upstream remotes once, and starts at most one exact fetch process per deadline:
`git fetch --quiet -- <remote>` or
`git fetch --quiet --multiple -- <sorted-remotes...>`. Successful fetches fan
out only a coalesced reconciliation signal. Every attached worktree recomputes
and publishes its own branch-specific remote result. The default interval is
180 seconds. Live interval changes, bounded failure backoff, and `0 = disabled`
remain supported, and the last attachment cancels the owner.

The same subscriber-owned broadcaster lifecycle carries the Git Manager's
repository-change signal; it does not start another poller or watcher. The
worktree root keeps its native recursive watch, including nested `.git` entries.
Linux notify/inotify expands recursion internally; macOS uses FSEvents and
Windows uses native subtree watching. Application code does not register every
worktree directory. Events for the top-level `objects/` and `lfs/` stores and
for submodule object stores (`modules/<name>/objects/`) are filtered after
delivery from worktree recursion; other submodule metadata still refreshes
status. Git metadata outside that root is watched
non-recursively at its root and recursively only at `refs/`, `logs/`,
`reftable/`, and `worktrees/`. The common directory's own object stores are
never watched; the recursive `worktrees/` watch does reach linked worktrees'
submodule Git directories (`worktrees/<name>/modules/`), whose object-store
events are filtered.
Only a sibling's `HEAD`, `reftable/**`, and administrative-directory
creation/removal invalidate this worktree; sibling index and log churn is
ignored. A shared metadata-layout classifier supplies both watch admission and
event classification, respecting uppercase-normalized Windows path keys.

Ref-shaped metadata includes `refs/**`, `logs/refs/stash`, `packed-refs`, root
`HEAD`/`*_HEAD`, `reftable/**`, and sibling `worktrees/<name>/HEAD` and
`worktrees/<name>/reftable/**`. New metadata containers invalidate refs. Only
first-level store roots (`refs/`, `logs/`, `reftable/`, `worktrees/`) are
queued for native recursive registration, outside the native callback; deeper
directories and ref lock renames never start registration work, so a large
fetch cannot overflow the queue. The queue stays bounded and would coalesce to a
rescan on overflow. On Linux, every directory under the recursive `worktrees/`
watch, including those submodule object folders, costs one inotify watch. Every
full rescan
publishes `RefMetadata` after reinstallation, even if reinstallation fails;
both native-overflow and registration-queue-overflow gaps therefore reconcile
refs. Cancellation, retirement, and shutdown await in-flight registration. A retained ref version
prevents a later index or working-tree event from overwriting an unconsumed ref
change.

Ref events alone use a 125 ms trailing debounce with a one-second cap from the
first ref event. Continuous working-tree/index writes do not postpone that
lane. Local and ref reads remain independent and each retains one trailing
refresh. The lightweight signature hashes bounded `for-each-ref` output for
heads, remotes, tags, and `refs/stash` (including `worktreepath`), structured
symbolic/object HEAD identity from `symbolic-ref` and `rev-parse`, and the stash
reflog. Loose stash logs are read as bounded regular files; reftable stores use
a bounded `git stash list` observation because they have no `logs/refs/stash`
file. This covers non-top stash drop/pop even when the stash tip is unchanged.
No full remote-status read runs for a ref burst. Signature observations share a
per-lifecycle lock, only changed hashes increment generation, and a read rejected
by a mutation fence waits for settlement and retries within the same lifecycle.
Retirement and cancellation prevent publication into a replacement lifecycle.
Remote-status publication and pending local reconciliation complete before
signature I/O. A failed HEAD or stash-reflog observation is logged without
changing the signature or suppressing remote status, and is retried on the next
trigger. Automatic fetch keeps its independent remote-tracking role.

`subscribeGitManagerSignal` carries cwd, generation, and optional
`watcherDegraded`. The watcher source owns sticky degraded health; health changes
publish without incrementing generation. Client-runtime consumes the platform's
shared `ConnectionWakeups.focusVisibility` stream, whose browser adapter coalesces focus/visibility
returns. The explicit `signalWithDegradedFocusRefresh` atom enables one shared
fallback per environment/worktree: only a true degraded flag invalidates active
refs, commit-page, and stash queries on these returns. Supervisor
`application-active` wakeups remain limited to visible-document transitions. Healthy
watchers and older servers omitting the field do not enable it. Plain signal
access starts no focus subscription, and no query polling is added.

The client runtime keeps mutations on the serial `(environmentId, cwd)` lane.
Only `vcs.refreshStatus` uses a separate `latest` lane with the same key: callers
share the active refresh and signals during it retain one newest trailing
refresh. Window focus is still debounced, transition to a visible document and
Git-menu open still request freshness, and hidden documents request nothing.
Successful pull, stage, unstage, discard, stacked action, and repository
publication still trigger their subscription refresh. A post-mutation signal
therefore cannot be satisfied by an older active read, while the server epoch
remains the correctness boundary across clients.

## Git Manager flow

The Git Manager uses the same authenticated Effect RPC session as every other
application surface. Its unary methods are:

| Methods                                                                                                       | Scope                   | Responsibility                                                                                                                                                                                                          |
| ------------------------------------------------------------------------------------------------------------- | ----------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `gitManager.getRefs`, `gitManager.getCommits`, `gitManager.getDiff`                                           | `orchestration:read`    | Read refs and worktree occupancy, page history, and load working-tree, commit, stash, text, binary, or image diffs.                                                                                                     |
| `gitManager.getStashes`, `gitManager.previewMerge`, `gitManager.listPullRequests`, `gitManager.getRemoteTags` | `orchestration:read`    | Read the native stash list, compute a merge preview, explicitly ask the configured provider integration for the current pull request and checks, or ask one remote which tags it advertises (`git ls-remote`, network). |
| `gitManager.commit`, `gitManager.undoCommit`, `gitManager.discard`                                            | `orchestration:operate` | Commit or amend selected paths, undo the latest eligible local commit, or discard whole-file changes.                                                                                                                   |
| `gitManager.stagePartial`, `gitManager.unstagePartial`, `gitManager.discardPartial`                           | `orchestration:operate` | Apply a generation-checked patch to the index or working tree.                                                                                                                                                          |

`subscribeGitManagerSignal` is the read-scoped generation/health stream described in
the VCS section above. `gitManager.runOperation` is the one operate-scoped
streaming command for branch lifecycle, fetch/pull/push, stash, merge, history
rewrite, conflict continuation or resolution, and tag operations. It emits one
`started` event, zero or more `output` events—one capped chunk per completed Git
command in the current supervised-process implementation—and exactly one
`finished` or `failed` event. Client interrupt and socket cancellation reach the
supervised child process.

`merge-into {source, target}` merges a full source ref into a local branch that
is not checked out, without touching any worktree. It is advertised by the
`gitManagerMergeIntoOperations` capability; an older server cannot decode the
variant, so clients send it only when advertised. The server refuses a target
that is checked out in the selected worktree (`target-is-current`), missing or
written as a revision expression (`local-branch-not-found`), a symbolic ref
(`invalid-request`), or checked out in another worktree (blocked
`worktree-checked-out`); a dirty selected worktree does not block it. On Git
older than 2.38 it fails with `git-too-old`. Otherwise it resolves
`refs/heads/<target>` and the source once, reports "Already up to date." when
the source is already contained, and computes the merge with
`merge-tree --write-tree` (with `--attr-source=<target>` on Git 2.43 or
later). A conflict fails with `conflicts` before any write, because no checkout
holds the conflict; unrelated histories fail with `unrelated-histories`. The
commit is created with `commit-tree -p <target> -p <source>`, passing `-S` when
`commit.gpgSign` is true, and published with
`git -c maintenance.auto=false -c fetch.writeCommitGraph=false -c fetch.bundleURI= fetch --no-tags --no-prune --no-recurse-submodules --no-write-fetch-head --quiet . <commit>:refs/heads/<target>`
and `GIT_REFLOG_ACTION="merge-into <source>"`. Git's refusal for a branch in use
by any worktree maps to blocked `worktree-checked-out`; any other publish failure
re-reads the target and reports `non-fast-forward` if it moved, or Git's error
otherwise. Commit and merge hooks do not run; a `reference-transaction` hook
does and can reject the publish. `gitManager.previewMerge` accepts an optional
`target` and then compares against `refs/heads/<target>`, returning
`current = target`; it fails with `git-too-old` below Git 2.38 and with
`merge-tree-failed` (carrying Git's first stderr line) for any merge-tree result
other than clean, conflicted, or Git's unrelated-histories refusal.

Branch selections send `branch-checkout.name` as a fully qualified
`refs/heads/<branch>` or `refs/remotes/<remote>/<branch>` ref, keeping local
names that resemble remote refs unambiguous. The server resolves that exact snapshot
entry and uses a non-forcing tracking checkout, so a similarly named local
branch cannot shadow the requested remote. A missing remote ref or an existing
local destination produces an actionable failure; neither falls back to a
different branch or replaces local history. Local branch names and unambiguous
short remote names remain accepted by the operation. The explicit `bring`
checkout strategy skips only the dirty-worktree guard: Git still rejects
overwrites, and operation-in-progress and worktree-occupancy guards still apply.
The `stash` UI choice completes the existing stash operation before checkout.
Checkout failures remain visible inside an open dirty-changes dialog.

History preserves pinned pages, ordering, selection, and scroll context while
splicing new commits. Overlapping entries take the fresh page data. The first
page is read once the signal's availability is known, then on a signal change
or when the repository generation moves past the loaded pages (in-app
operations while the watcher is degraded), each time under a never-reused key;
the watcher's echo of a repository-triggered read is absorbed, so one change
never causes two reads. Absorption relies on generation 0 meaning "no signature
yet": the server's first computed signature always advances it. A name-only ref
change (for example renaming a branch that is not checked out) leaves the
repository generation unchanged, so if its signal is absorbed as an echo, its
decorations update on the next refresh. Each completed first-page response captures one
retained-page batch through the existing `getCommits` RPC, with original pinned
tips/offsets and at most two reads in flight. Offset zero is omitted when its
tips match the fresh first page, and a first page covering all loaded rows
needs no retained read. Changed tips require the old pinned graph only to
update rows outside the new page. Retained responses from older generations
cannot overwrite newer decorations. Completed first-page reads, including
degraded-focus refreshes, own this batch; there is no independent retained-page
focus refresh. The key is local cache identity and is never
sent over RPC. Responses update decoration arrays without replacing retained
rows or cursors. Git's `%D` output is the only decoration source, preserving
`origin/HEAD` and Git's ordering; the client does not reconstruct labels from
refs. Same-tip ref renames also refresh decorations. Query lifetimes cancel
obsolete or unmounted batches, and selected-row lookup uses a SHA index.
Background failures preserve loaded commits and offer a Retry action.

Every Git Manager mutation revalidates the selected checkout after admission;
operations with server-authored blocked conditions recompute those reasons
there as well. Mutations reuse `WorktreeCatalogService`'s existing project lock followed by
its optional physical-repository lock. Inside those locks, branch and sync
operations then take the checkout's status-mutation guard before building that
refs snapshot and hold it through execution, so `vcs.*` writes, which take the
guard but not the project lock, cannot change the repository between validation
and execution. Unlike `vcs.*` failures that reach no Git effect, a branch or
sync operation that ends blocked still fences status reads, because it held the
guard while validating. The non-waiting acquisition returns the structured
`operation-in-flight` blocked reason when either lock is occupied; there is no
second Git Manager lock and no silently queued competing operation. The client
uses each returned `GitManagerBlockedReason.message` verbatim in disabled-state
and failure presentation instead of recreating Git or worktree policy.

Git Manager, review-preview, and commit-context diff reads share command-local
Git settings that always emit `a/` and `b/` path prefixes. User or repository
settings such as `diff.mnemonicPrefix` and `diff.noprefix` cannot alter the
patch format consumed by partial selection, the Diff panel's file parser,
and generated commit subjects.

History paging is pinned to repository tips. The first page resolves at most
512 unique head, remote, and tag tips and returns their SHAs; later pages echo
those SHAs so offsets remain stable. When the generation changes, the client
deduplicates and splices newly observed commits above the loaded snapshot. An
explicit refresh or an unresolvable pinned tip resets the pages. Repositories
over the cap fall back to `--all` paging with `degradedToAllPaging: true`, and
the History view warns that new commits may shift the list.

## Pull Requests contract boundary

`packages/contracts/src/pullRequests.ts` defines the repository-scoped Pull
Requests model, server-authored permissions and `PullRequestsOperationError`.
Eight unary reads (`pullRequests.getContext`, `getVocabulary`, `list`, `get`,
`getTimeline`, `getCommits`, `getChecks`, `getFiles`) require
`orchestration:read`; `pullRequests.runAction` and `pullRequests.checkout`
require `orchestration:operate`. Every request carries the selected checkout's
`cwd`; the client does not resolve repository paths or compute permissions.

`capabilities.vocabulary.pullRequest` and `pullRequests` are compatibility-only
request nouns. Both remain required and are sent by the GitHub and GitLab
adapters so older clients can decode context. Current clients ignore those two
fields and derive nouns and number prefixes from the provider kind through
`@bibcode/shared/sourceControl`; the remaining vocabulary fields still describe
host-specific controls.

### Pull Requests flow

`git.runStackedAction` carries the explicitly reviewed `pullRequestBaseBranch`
for `create_pr` and `commit_push_pr`. The field is optional in the shared input
schema because commit/push-only actions do not use it; the server requires it
for request actions and rejects it on other actions. Target syntax and
source/target identity are checked before branch, commit, push, or hosting
mutations. There is no default-branch fallback. All client creation entry points
use the shared review dialog, which requires a fresh explicit selection on open.
Both branch selectors request `vcs.listRefs` with `refKind: "remote"` and
`includeMatchingRemoteRefs: true`, then restrict results to origin branches.
Ordinary request creation prefills the checkout only when it has a matching
origin-tracking branch. Combined commit actions keep their fixed current or
generated source because they explicitly publish it before creating the request.

The same action accepts an optional `pullRequestHeadBranch`. An explicitly
selected source is validated against local heads or origin-tracking branches
before mutation. `create_pr` publishes an existing local source with an exact,
non-forced origin refspec without tags; an origin-only source skips publication.
Neither changes the checkout, index, or working files. A combined commit action
accepts only the current source and then uses its current/generated branch.
Callers omitting the source retain current-branch behavior. The review reads the
selected source's tip from `gitManager.getRefs` and pins `getCommits` to that SHA,
so unrelated branches cannot supply the request title or description.
The authenticated environment advertises `gitPullRequestBranchSelection`
(default false for older servers). The dialog requires it and the client action
checks it again on the same live session used for the mutation stream. Older
servers therefore cannot silently ignore the reviewed branch fields.

The same action takes optional `pullRequestOptions` on request actions:
`draft`, `assignees`, `reviewers` and `labels` (Pull Requests vocabulary entry
ids; at most 20, 20 and 50), `milestone` (`{ id, title }`), and the GitLab-only
`removeSourceBranch` and `squash`. Before any branch, commit or push, the server
refuses options the resolved provider cannot honor: merge options outside
GitLab, non-numeric GitLab person or milestone ids, and any option for Azure
DevOps or Bitbucket. GitHub receives `gh pr create` flags (`--draft`,
`--assignee`, `--reviewer`, CSV-quoted `--label`, `--milestone <title>`);
GitLab receives one JSON body over stdin (`glab api … --input -`) with
`assignee_ids`, `reviewer_ids`, `labels`, `milestone_id`,
`remove_source_branch`, `squash`, and a `Draft: ` title prefix added unless the
title already carries a draft marker. Requests without options keep the plain
commands. When `gh` exits 1 after printing the request URL, the step reports
`created` with `pr.warning` and is not retried; an existing open request reports
that the options were not applied. Servers advertise `pullRequestCreateOptions`
(default false for older servers); the dialog shows the fields and the client
action sends them only when it is present. `pullRequests.getCreateDefaults`
supplies the dialog's "Assign to me" viewer and GitLab's `squash_option` and
`remove_source_branch_after_merge`; the dialog re-reads it on every open.

| Method                           | Required scope          | Responsibility                                                          |
| -------------------------------- | ----------------------- | ----------------------------------------------------------------------- |
| `pullRequests.getContext`        | `orchestration:read`    | Resolve origin, host/account, repository policy and availability.       |
| `pullRequests.getVocabulary`     | `orchestration:read`    | Load bounded searchable picker values on demand.                        |
| `pullRequests.getCreateDefaults` | `orchestration:read`    | Viewer and GitLab merge defaults for the create dialog.                 |
| `pullRequests.list`              | `orchestration:read`    | Page the repository list for explicit tabs/filters.                     |
| `pullRequests.get`               | `orchestration:read`    | Read detail and compute permissions/readiness.                          |
| `pullRequests.getTimeline`       | `orchestration:read`    | Read bounded comments, reviews, threads and events.                     |
| `pullRequests.getCommits`        | `orchestration:read`    | Read the request's commit list.                                         |
| `pullRequests.getChecks`         | `orchestration:read`    | Read GitHub checks or GitLab pipeline jobs.                             |
| `pullRequests.getFiles`          | `orchestration:read`    | Read file metadata, bounded patches and diff refs.                      |
| `pullRequests.runAction`         | `orchestration:operate` | Revalidate and serialize review/metadata/merge/state mutations.         |
| `pullRequests.checkout`          | `orchestration:operate` | Guard current/other/new-worktree checkout and settle durable ownership. |

Browser and desktop clients use these same typed unary RPCs; native actions such
as opening a host URL retain the existing DesktopBridge/local API boundary.
Reads begin on route/tab/picker open, user filters/pagination, Refresh, Rescan,
or successful-action invalidation. No timer, window focus, constructor, or idle
worker initiates provider traffic. Mounted list and detail routes of both
hosts paint from `pullRequests.readSnapshot` (display copies in SQLite; zero
host calls) while their live reads run. A mounted GitLab merge request list or
detail route also opens `pullRequests.subscribe` only while the document is
visible, and keeps the client's 20 s mounted-query refresh timer
paused from the subscription's first, all-false event until the subscription
ends. Successful unary `list` (first page), `get`, `getTimeline`, `getCommits`,
`getChecks`, and `getFiles` reads store those display copies. One server poller
per list key runs the list read and one merge-request probe `GET` 20 s after
each tick, stores the list page and the probe fingerprint, and emits the names
of the query families that changed; it never runs the detail, timeline,
commits, checks, or files reads. The client answers `list` with
`readSnapshot`, keeping whichever copy has the newer `observedAt` (a live first
page counts as observed at the newest stored copy already seen), and answers
the other names with the unary reads. Every subscriber of a list key receives
its list events, but only the open number of the subscriber driving the poller
is probed, so a second window open on another number of the same list key
gets list events only. Hiding the document or leaving the route drops the
subscription, resumes the client timer, and stops the shared poller when no
subscriber remains. The route still prefetches `get` plus `getTimeline` for a
row hovered 150 ms, at most two per environment, and an open within 5 s of that
prefetch shows it without reading again. `pullRequests.runAction` and
`pullRequests.checkout` re-read the host and do not use snapshots; a successful
`runAction` deletes that number's rows. GitHub routes store and paint display
copies but do not subscribe or poll. The
explicit Refresh action is unchanged. `pullRequests.getContext` accepts an
optional
`rescan`: without it the server reuses its bounded 30 s probe and host-context
answers (the origin is still read); with it the server clears those caches and
forgets the origin host's recorded provider before resolving. Only Rescan and
authentication recovery set it. It also accepts an optional `allowStale`, which the
panel sends on its first read of a checkout: when a stored context exists, the
server answers from it with `stale: true` and no host call, and the panel then
reads live at once and replaces it; a failed live read keeps the stored context
on screen; each stored answer is followed by one live read. Each live
`available` answer is stored as the display copy (kind `context`);
`repository_unreachable`, `cli_missing` and `cli_too_old` answers replace it with
an empty tombstone, and `not_authenticated`, or a Rescan answering
`unknown_host`, deletes every row for the host. A context read that started
before such an invalidation never writes its answer afterwards. On
its first read after start the service records the provider of every host that
has a stored context, so a self-hosted origin resolves without CLI discovery.
Older clients never send `allowStale` and always read live. `pullRequests.list` accepts an optional
`refreshTotals`, set only by an explicit list Refresh: GitLab's three
repository-wide totals are otherwise reused for 30 s per host and repository
across filters and pages, and cleared by Rescan, auth/permission failures, any
successful `runAction`, and a request created through `git.runStackedAction`
(the Git VCS service reports it through `CreatedRequestObserver`).
`server.discoverSourceControl` accepts an optional `recordHosts`: only
**Settings → Source Control** (and its Rescan) sets it, making each CLI's
authenticated host list the recorded hosts. Recognized full GitLab logout
reports Unauthenticated with an empty list and clears recorded GitLab hosts;
unrecognized output reports Unknown without a host list and retains them.
Background reads such as the publish dialog's never write them. The five-second Undo lifetime and 300 ms
input debounce do not poll. Detail mounts `get` plus the active tab's query;
Files also mounts timeline for line-anchored threads. Inactive atom invalidation
does not mount extra reads.

`@bibcode/client-runtime/state/pull-requests` exposes the query families and
commands. Reads have bounded stale times without polling; mutations share the
existing VCS scheduler, serialized by environment and checkout path. The
`pullRequestsEnabled` client preference defaults to true. The environment
flags `pullRequestsReads` and `pullRequestsMutations` default to false when
omitted by older servers and are advertised by the current server.

Production registers all ten handlers through `PullRequestsRpcServices` with
the environment state directory and durable repositories. The server's
`pull_requests` module implements all eight reads for GitHub and GitLab:
context, lazy vocabulary, paginated lists, detail, timeline, commits,
checks/pipelines, and files. `runAction` also implements conversation and
review mutations: comments, edits/deletes, reactions, replies/resolution,
review submission, and each host's supported review controls. It also implements
metadata edits, locking, branch updates, merge/auto-merge, draft and open/closed
state changes, GitLab deletion, and revert creation. Checkout uses the guarded
worktree catalog path described below.

### Permissions and serialized mutations

Detail reads context once and joins host observations with the pure
`permissions::compute` policy. The server owns every action reason, merge
method/default, version gate, and readiness explanation. The client renders
those values verbatim; the host remains authoritative when a later mutation
runs. GitHub approvals need no write grant, but self-approval is denied;
GitLab comment/approval/merge answers come from the host, including public
readers with no project membership. GitHub suggestion application
remains unavailable because there is no public API. GitLab versions that
cannot be read retain the explicit version-unavailable reason. The current
public GitLab reads do not expose a custom maintainer-delete grant, so that
branch of the permission rule remains closed. GitLab readiness leaves the
active auto-merge method null when unknown; the repository default remains in
merge permissions.

Mutations resolve the checkout, acquire a gate shared by service clones for the
normalized provider/host/repository/request number, and read fresh permission
observations before checking the same pure policy that the client displays.
GitLab mutation prechecks read the MR, approvals, reviewers, GraphQL permission
metadata and current project access/merge policy. `ActionDetail` carries those
inputs and action operands without constructing the full UI detail; approval-rule
display, reaction counts, closing issues and display-only diff-base recovery are
not read for a mutation. Full detail retains those reads. Cached context never
replaces the fresh MR/head or project access/policy check. Denials preserve
the permission reason; unsupported host capabilities return `unavailable`.
Review submission rejects a changed head before starting side effects. GitHub
submits one atomic review; a comment-related 422 reports zero landed comments
and a failure for every inline draft. GitLab fetches diff versions once,
rechecks the head, reads that immutable version’s diff to derive rename paths
and context-line coordinates, posts inline discussions individually, then the general
summary and review event. Inline failures do not discard successful comments.
A later summary/event failure reports the number already posted and tells the
client to refresh before retrying. Success performs no server reread; the
client owns explicit invalidation. The result's `landed` count covers inline
comments, while a partial-error message also counts a posted general summary.
The version diff uses the 8 MiB patch budget and is scanned once per selected
file; missing/unavailable positions remain failed drafts.

Merge requires the viewed head SHA and a method from the computed allowed list.
Immediate merge checks `merge`; bypass checks `mergeBypass`; scheduling auto-merge
checks `enableAutoMerge`. GitLab requests selecting both special paths must satisfy
both permissions. GitHub rejects the combination with `invalid_request` before
the adapter starts a CLI process; its merge controls must offer the two modes
separately. These permissions include access and their own readiness rules,
so immediate-merge readiness does not block an authorized bypass or scheduled merge.
GitHub pins the head with `--match-head-commit`; GitLab pins it with `--sha` or
the REST merge body's `sha`. GitLab bypass first calls the public
`mergeRequestUpdate(overrideRequestedChanges: true)` mutation. If merging then
fails, the error reports that the override already succeeded. Delete checks
fresh host capability and permission; its contract has no head SHA.

An internal action context carries the fresh metadata snapshot to the adapter;
it is never client supplied or persisted. GitLab reviewer and assignee updates
resolve each retained login once and send the complete current-plus-add-minus-remove
ID list. GitHub milestone IDs remain milestone numbers, resolved to titles before
`gh pr edit --milestone`. GitLab milestone IDs are numeric database IDs.
Empty metadata edits do not start an interactive CLI.
The mutation gate covers the read/modify/write sequence across different checkouts
of the same host request. Waiting respects the RPC cancellation token and overall
deadline; unrelated requests remain independent. Weak gate entries are removed on
subsequent admission, without a timer or permanent per-request queue. For hosted-only actions, this remote
identity gate does not use the local cwd status-mutation lock or trigger local Git
refreshes. Checkout additionally participates in the catalog and VCS mutation lifecycle. External host edits still remain subject to the host's API concurrency
semantics.

GitLab merge messages use REST with a private JSON body file. Plain CLI merges
explicitly pass `--auto-merge=false` for immediate merging because the CLI defaults
to enabling auto-merge. A revert creates
`revert-<iid>-<eight-character-sha>` from the target branch, reverts the known
merge commit (or known squash commit), then calls `source_control::PullRequestService::create`.
That service owns creation payloads and response normalization; an injected,
crate-private GitLab transport keeps this module's calls inside `HostCommandRunner`.
Classified command failures cross that internal seam without changing the legacy
source-control wire error. A partial revert error states which steps succeeded
and requires inspecting the host before retrying; it does not retry or delete the
created branch. GitHub revert reuses the existing create URL parser. Both return
the created request's number and URL without rereading it.

### Checkout lifecycle

`pullRequests.checkout` uses the same per-request action gate and the worktree
catalog's non-waiting project/repository lock. Under that lock it resolves project
ownership and verifies both physical Git common directories against the catalog's
repository pin, then evaluates the shared Git Manager guards for the target.
Unknown targets and targets owned by another project return a successful `blocked`
payload. Registered external worktrees are accepted only after repository identity
and live membership checks. Dirty or in-progress targets are blocked before the
provider checkout command. A current branch still runs the ordinary non-forcing
provider checkout: its name alone cannot prove it contains the request's head.

New-worktree checkout fetches the provider head ref with the bounded,
non-interactive Git runner and reuses a matching local branch only when it is
unoccupied. A divergent tip or occupancy in any worktree (including the current
checkout or a registration whose directory is missing) selects `<head>-pr-<number>`,
then `<head>-pr-<number>-2` and subsequent suffixes on further collisions. Planning
uses the live worktree snapshot under the catalog lock before invoking the existing
managed creation transaction; it does not rely on generic numeric worktree naming. It inherits the project's
default workspace settings; the transaction owns Git rollback, durable workspace
creation and catalog/status publication. The shared worktree operation runtime
retains admission and cleanup through durable handoff. Only pre-write reads and
planning receive the request token. At write admission, a separate token shields
the complete Git/write-and-owner sequence; each write command uses a 24-hour safety
ceiling instead of the 60-second request budget or managed Git's ordinary 30-second
budget. The catalog lock and availability lease stay with that task even if the
RPC wait ends. The preparation deadline and admission loss also notify the waiter,
without interrupting the write. The cancellation observer ends before normal read
scope cleanup, so successful completion cannot manufacture a cancelled wait.

After handoff, an interrupted wait returns `PullRequestsOperationError` with code
`timeout`, `retryable: false`, and the neutral message “Checkout continues in the
background; refresh to see the result”. Pre-write cancellation states that no Git
changes were made. Browser interruption and socket/heartbeat loss have an unknown
outcome and show “Checkout may still be running; refresh to see the result”. Pending
UI state clears in all cases; none automatically retry or report the cancelled wait
as a failed checkout. Context/detail and worktree catalog refresh use their normal
paths after reconnect. Git's own failure remains an error: inspect `git status` in
the checkout path before retrying, with no automatic reset or lock-file removal.

Graceful server shutdown drains the existing worktree operation runtime; it does
not cancel started writes. That runtime has no additional drain deadline; the
24-hour per-command ceiling remains. The desktop's existing five-second stop wait
does not abort the in-process server owner. Forced process exit is outside this
lifetime guarantee. Runtime admission failures are translated into
`PullRequestsOperationError` at the RPC boundary. Local Git mutations invalidate
catalog views even when Git fails after fetching. The web uses the shared environment catalog query for targets. Catalog loading
is shown only while there is no snapshot: Effect's stream `waiting` flag remains
true between emissions and is not a refresh-progress signal. The web
shows server-authored blocked reasons, and offers explicit Git Manager navigation
for successful receipts; stale toast retries recheck current mutation eligibility.

Mutation bodies use GitHub stdin or GitLab's private body file. GitLab review
GraphQL commands contain only escaped identifiers via `-f query=…`; comment
bodies never enter argv. Timeline identifiers retain their host kind, database
and node segments when passed back to actions; malformed or inappropriate
kinds fail as typed request errors. Reactions and thread resolution read the
current state before changing it so repeating the current state succeeds
without another mutation. GitLab multi-line comments currently post on their
end line: this server has no directly usable SHA-1 dependency for the host's
line codes, and does not substitute Git object hashes.

Tabs read independently. GitHub timelines use bounded GraphQL connections
(including nested thread comments), while GitLab joins discussions, resource
events, and synthetic approval/reviewer entries. Comment/review/thread ids are
opaque host-specific identifiers carrying the prefixes needed by later
mutations. GitLab note GlobalIDs accept the `Note` subtype family (including
`DiffNote`, `DiscussionNote` and `LegacyDiffNote`); the wire/action ids retain
the numeric REST note id and opaque discussion hash. Non-note GlobalID kinds
remain rejected at the note boundary. GitHub body reaction flags come from the supplementary GraphQL
read; GitLab body awards are explicitly paginated. GitLab timeline GraphQL
pages include discussion notes, award emoji and positions, keeping the number
of child processes bounded independently of note count.
Synthetic GitLab review timestamps use the MR update timestamp
because approvals have no submission timestamp. The GitHub check read reuses
the existing newest-run fold and adds workflow grouping and nullable timing
metadata; GitLab groups jobs from the latest pipeline by stage. GitLab detail
uses bounded public GraphQL metadata for counts and source-branch permissions
absent from the REST record and reads closing issues separately.

### Client views and drafts

The web shell uses project-scoped `/pull-requests` and `/pull-requests/$number`
routes inside the chat route inset. Availability checks the client preference,
passive connection state, and read capability before mounting context or
catalog queries; it never dials a disconnected environment. Context and list
reads use only the client-runtime atom families; `getContext` stays keyed by
`{ cwd }`, and Rescan or recovery registers a one-shot `rescan` for that key
(likewise `refreshTotals` for the list's first page). Reads of the key send it
until one is answered, a success or the server's typed failure; an interrupted
read or a lost connection keeps it. The current value stays visible while it
runs. A list or detail/tab authentication failure rescans context once through
the panel's existing refresh callback. While the context is pending on the
list route, the panel mounts the first list page for the stored filters when
the stored tab is Open or Closed, so the two reads overlap. It remembers that
page's `environmentRpcKey` as a value, capturing a changed prefetch key through
a guarded state adjustment during render. The list treats the page as fresh
on its first open only. Consumption occurs after the list commits, so
StrictMode or suspended renders cannot consume it; the key is dropped once the list opened, on a detail
route, or when the context settles unavailable. Any other open refreshes a cached page or failure
and shows loading until the new read answers.
There are no provider timers or focus refreshes; the 300 ms text-input debounce
is scheduled only by typing.

`apps/web/src/pullRequestsStore.ts` owns version-1 local view state under
`bibcode:pull-requests-state:v1`, keyed by the physical environment/project
identity and bounded to the two most recently used projects. It persists the
opaque checkout, tab, filters, sort, scroll position, last number, viewed files,
and separate per-number comment/review/edit/merge drafts. Server results and
permissions remain in atoms. Paging retains prior rows in the mounted view,
resets on scope/filter changes and explicit Refresh, and requests a further
page only through Load more when a non-null cursor exists. Rows are virtualized
at 56 px. Navigation back to a saved offset reopens only enough cursor pages
to restore that viewport. Scroll is saved on navigation and pagehide without
writing storage on every scroll event. Because a checkout path can acquire a
new origin/account, each explicit list/picker/detail/tab open refreshes a cached value
before displaying it (without restarting an already-pending mount read, and
without re-reading the first page the panel loaded during the same open).
Rescan identity changes remount repository-bound UI, clearing open dialogs and
picker state before new data appears. Creation uses the existing Git Manager
dialog and mutation capability reason.

`PullRequestsDetailView` mounts `get` plus the active tab read; Files also
mounts timeline in parallel for existing inline threads. Its route search
stores `conversation|commits|checks|files`; Refresh invalidates `get` and that
tab, including timeline when Files is active. Timeline ids remain opaque, including host kind prefixes. The
conversation and file lists use LegendList; file bodies mount through
IntersectionObserver with a 600 px margin and apply the Git Manager size
ladder before parsing. File anchors use `#file=<encodeURIComponent(path)>`.
Viewed paths persist in the existing per-project, per-number store; the
whitespace toggle starts from the existing client setting and only changes
the rendered patch, retaining source line coordinates. The full files result
retains diff refs for later review submission.

Markdown reuses the sanitizer exported by ChatMarkdown and replaces images
with links opened through the local shell API. No avatar or host image is
loaded. Readiness text and denied-action reasons are displayed verbatim.
Review controls use the server permission, including visible disabled controls
for unsupported host actions. The environment mutation capability gate applies to write controls and before
dispatch; read-only Refresh/Retry and local draft recovery remain available.
Metadata, merge and state controls use the same command path. Checkout uses
`pullRequestsEnvironment.checkout` through its sibling checkout runner.
Approval rules have no edit action/permission in the current contract and are
displayed without an edit control.

`useRunPullRequestsAction(scope, number)` is the detail surface's hosted-action write
path through `pullRequestsEnvironment.runAction`. The detail view supplies
refresh handles; inactive affected atoms are invalidated without extra query
subscriptions. Success (including a `reviewSubmitted` partial receipt) refreshes
`get` plus these queries:

| Actions                                                                                 | Queries                  |
| --------------------------------------------------------------------------------------- | ------------------------ |
| comment, editComment, deleteComment, minimizeComment, react, replyThread, resolveThread | timeline                 |
| submitReview                                                                            | timeline, files          |
| revokeApproval, removeOwnChangeRequest, dismissReview, rerequestReview                  | timeline                 |
| applySuggestions                                                                        | timeline, files, commits |
| editPullRequest, setDraft, close, reopen                                                | timeline                 |
| setReviewers, setAssignees, setLabels, setMilestone, lock, unlock, disableAutoMerge     | none beyond get          |
| updateBranch                                                                            | commits, checks, files   |
| merge                                                                                   | timeline, commits        |

A `deleted` receipt navigates to the project list; `pullRequestCreated` navigates
to its returned number on Conversation. These receipts do not re-read the old
detail. A `merged` receipt with `autoMergeEnabled: true` reports **Auto-merge
enabled**; otherwise it reports **Merged**. Both refresh the merge queries.
No optimistic cache writes, provider timers, direct provider requests, or focus
refreshes are introduced. A typed failure is toasted and rethrown. `stale_head` offers an
explicit Refresh action for detail and all four tab atoms and keeps every
pending comment and merge draft. Structured error messages and optional host
detail are displayed verbatim; no failure automatically retries a write. Review requests send
the loaded detail's `headSha` and strip only locally generated draft ids.
Receipts reconcile the submitted snapshot with current drafts, preserving
concurrent edits/additions. Failed entries include the original body, so the
client matches path + line + body. Indistinguishable duplicate drafts are
preserved conservatively. `reviewPosted` distinguishes a completed review/event
and summary from GitHub's atomic rejection: only a confirmed posted summary
is cleared, and retrying partial success defaults to Comment without repeating
an approval. A thrown failure keeps all drafts. Thread comments include the
host's `minimized` state; both initial and paginated GitHub comment reads select
`isMinimized`, while GitLab reports false and denies the unsupported action.

Title and description edits use the existing per-request drafts and clear only
the successfully submitted value. Cancel/Escape retains the draft. Merge drafts
add a backward-compatible `mergeDraftEdited` boolean: legacy nonempty messages
remain edited, while legacy empty defaults remain untouched. Explicitly blank
new edits stay blank across reloads; clearing a successful merge draft restores
host-derived defaults. Merge confirmation snapshots retain the selected method,
target, deletion/auto/bypass choices and loaded detail head SHA until submission.
GitHub auto-merge selection disables bypass with an explanation; GitLab supports
both when the server grants both permissions. Read-only viewers have the server's
merge denial and no method picker.

Metadata and branch vocabularies mount only while their picker is open. Complete
results filter locally; truncated results search the server after a 300 ms typing
debounce that is cancelled on close. Reviewers exclude the author. Metadata writes
send exact add/remove deltas; milestone values remain numeric strings. A successful
metadata, lock or draft-state edit may offer a five-second toast whose Undo dispatches
its exact inverse through the scoped command hook, once and only on explicit click.
Expiry and dismissal perform no write. A clicked Undo uses the hook's explicit
`waitForPending` admission option to run once after any current command settles;
ordinary duplicate submissions are still rejected. This queues an authorized
intent and never retries a dispatched mutation. Picker selection reconciles on
each authoritative detail receipt, including a receipt identical to the initial
selection after Undo. Metadata controls remain disabled with “Refreshing request
details…” while the detail query is pending after a successful write or Undo;
command completion alone cannot enable a checkbox showing the previous state.
Late revert/delete results retain success feedback but
automatic navigation is fenced by the initiating view's mounted identity. A base change confirms pending review-comment
loss and clears those comments only after the host accepts the change. Linked issues
remain description-derived and have no separate editing action.

Version-1 drafts additionally persist `replyDrafts`, `inlineDrafts`, and
`reviewBody`; old snapshots sanitize these to empty values. Text is cleared
only after a confirmed action, and a successful old submission cannot erase a
newer draft. Gutter selections use path, source-side line/range, and side;
annotation slots anchor composers, pending comments, and threads. Unrenderable
or missing lines retain their annotations below the diff; drafts for files no
longer present have an editable/removable recovery section. Comment links use
the loaded request URL because timeline contracts have no comment permalink;
opaque ids are never decoded to manufacture host URLs.

### Context reuse

Each service still reads the selected checkout's origin for every RPC. Successful
CLI presence/auth/discovery probes and parsed GitLab host contexts are memoized
for at most **30 seconds from the start of their load**, without extending that
window on hits. Each cache has at most **32 entries**, evicts the oldest admitted
entry when full, and owns no timer or worker. Context keys contain normalized
host plus the exact repository path; probe keys additionally include provider
and argv. Values belong to this server runtime, remain in memory and are never
persisted or logged.

Concurrent loads of the same key share a cancellable lock; unrelated keys load
independently. Failures and cancelled loads are not cached. Invalidating or
evicting an entry detaches its in-flight load, so late completion cannot restore
a superseded cache entry. Only `pullRequests.getContext` with `rescan`
(Rescan/auth recovery) clears context/probe caches before reading. Authentication,
repository-denial/missing and CLI-availability failures also invalidate them.

Once the provider and remote scope are resolved, context and list reads share
one authenticated-read helper that runs the login recheck concurrently with
the host read. A failed login cancels the host read and awaits its supervised
child cleanup before returning the login error, discarding any host response.
Unknown hosts still require discovery before any adapter is selected. Mutation paths
finish authentication before host reads or writes. Created-request totals
invalidation reuses the same remote-to-scope decoder and only selects GitHub
or GitLab adapters. After a successful login check, both context and list
recheck checkout availability before accepting the host response or its error,
including when all provider children started before the directory disappeared
and returned successful responses.

GitLab timeline reads reuse an available context viewer; reaction mutations use
the viewer carried by their fresh permission observations. Neither needs a
second `api user` when that identity is already held. Identity/version can lag
external CLI-account or host-version changes within the bounded window; use
Rescan for immediate refresh. Every mutation still reads fresh repository access,
merge policy, MR state/head, approvals, reviewers and host permission flags under
the per-request gate, and the host remains authoritative on the write itself.

### CLI boundary and resource limits

Each explicit read resolves the selected checkout's origin once. Known
providers use the existing source-control detection; unknown hosts are matched
against bounded CLI auth discovery. Host and repository identity are pinned on
every subsequent command. Context returns actionable unavailable data for
missing remotes, unsupported/unconfigured hosts, missing or outdated CLIs,
authentication, and repository access. Process failures remain typed errors.

`HostCommandRunner` is the module's single process boundary and delegates all
Git/provider work to `ProcessRunner`. It supplies noninteractive Git settings,
host environment variables, repository operands, cancellation, output caps,
and deadlines. One overall deadline covers context/vocabulary (30 seconds),
lists, detail/tab reads and ordinary mutations (60 seconds), including their
child calls; cancellation awaits bounded process cleanup. Checkout is the
documented exception: after the guarded write handoff, each Git/provider write
has a 24-hour safety ceiling and an independent cancellation token. The caller
may stop waiting while the operation owner retains locks/admission and settles
the write; it must not inherit the short read/mutation deadline. Metadata JSON responses are
capped at 1 MiB. Patch reads, including GitLab's patch-bearing diff JSON, use
an 8 MiB budget. Per-file patches over 1 MiB are omitted with `tooLarge: true`;
binary rows remain without text patches. Whole-patch overflow returns a
truncated file list with omitted patches. GitLab recovers that list from
bounded metadata-only `diffStats`; when a diff never arrived, its unavailable
change-kind metadata is represented conservatively as `modified`. GitHub inventories at the CLI’s
100-file boundary are completed with bounded GraphQL pages. Timelines
cap paginated connections/discussions at ten pages, resource events at 100 per
type, GitHub file metadata at ten 100-file pages, and GitLab file reads at
twenty 50-file pages. GitLab award reads cap at ten 100-award pages; a capped
note read marks the timeline truncated, while detail reports `output_limit`
because its contract has no truncation field. GitLab commits and latest
pipeline jobs currently use the specified first 100-row read; their contracts
have no truncation field. The GitLab body helper writes only a private temporary
file under the state directory and removes it after success, failure, or
cancellation. GitLab private-file requests set `Content-Type: application/json`
explicitly, including GraphQL reads with query/variables in that file.
Construction and idle time start no provider work or poller. Source tripwires
cover every module file and the RPC adapter: no direct process constructors,
`reqwest` HTTP client, periodic intervals or thread workers; async task spawning
is confined to the request-owning production boundary. A constructor tripwire
checks the actual default command specifications (`gh`, `glab`, `git`).
One-shot request deadlines and the protected checkout write ceiling remain
permitted. Bodies never appear in argv: GitHub uses stdin, and GitLab uses the
private body helper; process output and user/host content stay out of logs.

GitHub lists use GraphQL cursors unless filters require CLI search. Search
never requests comment bodies and currently reports comment count zero with
unknown totals and no next cursor. GitLab lists use 30-row pages and optional
header totals; list rows do not include approval decisions or pipeline status.
Review-status filters return an actionable `unavailable` error until per-row approval
reads are added to list filtering. Filtered result totals remain unknown while tab counts
remain available. GitLab version gates close when the version cannot be read;
the wire context exposes a null version, and internal context retains the
reason for permission computation. No host data or CLI stderr is logged.

## Worktree catalog flow

The catalog protocol is server-resolved and capability gated:

| RPC                              | Scope                   | Boundary                                                                                    |
| -------------------------------- | ----------------------- | ------------------------------------------------------------------------------------------- |
| `subscribeWorktreeCatalog`       | `orchestration:read`    | Stream the latest project snapshot; the request contains only `projectId`.                  |
| `vcs.refreshWorktreeCatalog`     | `orchestration:read`    | Request one bounded observation and return its snapshot.                                    |
| `worktree.updateDiscoveryPolicy` | `orchestration:operate` | Persist a policy derived from an authoritative generation with unchanged candidates.        |
| `worktree.createManaged`         | `orchestration:operate` | Resolve the project root and checkout path, create Git state, then persist one owner.       |
| `worktree.createPanel`           | `orchestration:operate` | Derive panel project, kind, branch, and path from a persisted host thread.                  |
| `worktree.retarget`              | `orchestration:operate` | Resolve an opaque catalog key/generation before changing a workspace owner path.            |
| `worktree.adopt`                 | `orchestration:operate` | Resolve an opaque catalog candidate and create, restore, or return its canonical workspace. |
| `worktree.getRemovalPlan`        | `orchestration:read`    | Bind current removal facts into an opaque plan token.                                       |
| `worktree.removeFromBibCode`     | `orchestration:operate` | Detach ownership without a Git mutation.                                                    |
| `worktree.remove`                | `orchestration:operate` | Perform the explicitly selected verified Git action and apply its detach rules.             |

The browser never supplies a checkout path to these methods. Repository trust,
path normalization, catalog joins, availability, plan validation, and Git
effects remain server-owned. See [Worktree catalog](./worktree-catalog.md) for
the full lifecycle and resource bounds.

This dedicated surface is exclusive for worktree authority. Generic
`orchestration.dispatchCommand`, over either WebSocket or HTTP, rejects
client-supplied discovery policy, thread kind/worktree path, bootstrap
worktree root, metadata path retargeting, adopted-owner deletion, and project
deletion while an adopted owner exists. Ordinary non-worktree orchestration
continues through the generic surface; the server derives the permitted
default/workspace kind and project working directory. Internal resolved
commands remain unavailable to public dispatch. The orchestration engine
repeats ownership/deletion validation so another in-process caller cannot
bypass the public decoder.

`subscribeWorktreeCatalog` publishes the server-owned latest catalog snapshot
for one persisted project through a watch-backed RPC stream: the atomic initial
read is marked seen, and acknowledgement lag replaces pending state instead of
queueing stale generations. Request cancellation also cancels bootstrap before
it can retain a catalog view. `vcs.refreshWorktreeCatalog` accepts `projectId`
plus an optional `reason`. Omission or `explicit` requests an authoritative Git
observation; only `focus` may reuse a healthy matching repository fingerprint,
and even then the server performs real inventory at exactly five minutes from
the last successful real scan. Both methods require `orchestration:read`;
clients never submit baseline or checkout paths.

The additive reason is gated by the default-false
`worktreeCatalogRefreshReason` capability. Current catalog-capable servers
advertise it. New clients keep Focus and Explicit as separate local
single-flight classes, but omit `reason` for an older server; manual Retry
remains Explicit. Negotiation never retries a rejected request with a different
meaning. Fingerprint reuse skips only repository inventory: per-project joins,
suppressions, availability probes, generation publication, and authoritative
branch reconciliation still execute from the rebuilt healthy snapshot.

Catalog entry lifetime is shared by subscriptions and unary users. Refresh,
adoption, retarget, removal, trusted-anchor resolution, and current-snapshot
reads acquire scoped unary ownership alongside subscriber ownership. When the
combined subscriber/unary-user count reaches zero, the view schedules
pointer-checked eviction after 60 seconds; reuse cancels the old timer, and
cancellation cannot evict a newly installed entry or repository observation.

`worktree.updateDiscoveryPolicy` requires `orchestration:operate`. An
acknowledgement must name a generation whose eligible candidate paths still
match the last authoritative snapshot; a refresh that changed nothing does not
invalidate it. The server derives the baseline from eligible normalized paths
in that snapshot,
deduplicates it, caps it at 512, and persists the complete policy through the
durable `project.meta.update` command. The policy handler digests the decoded
public payload, acquires its cancellation-aware command claim, and transfers
that claim plus the digest into engine persistence. Receipt lookup remains
cancellable while the claim is local. Mutation serialization then acquires the
stable project-identity lock before the optional physical-repository lock. The
policy mutation task may outlive an interrupted RPC: cancellation wins while
waiting for either lock or before engine-envelope handoff, but a successfully
enqueued envelope retains both mutation locks and the command claim until its
terminal receipt. A different command therefore re-reads the complete policy
after its predecessor commits instead of enqueueing a stale full replacement.
This command-claim, project, repository order is the same order used by
adoption and removal. It prevents both command/project lock cycles and a
project switching mutexes while its durable trust pin is established, while
still serializing known cross-project repository aliases. An opposite-payload
retry fails with typed `command-conflict`. A digest-less accepted legacy
receipt replays only when its project identity, terminal sequence, sole empty-
metadata `project.meta-updated` event, and exact complete policy payload prove
that it was the same policy operation; unrelated project, generic metadata,
adoption, and removal receipts fail closed as `command-conflict`.
Digest-less `reserved` or `prepared` policy receipts still follow the engine's
aggregate-checked restart-resume path; only accepted legacy replay requires the
terminal-event proof.

`worktree.createManaged` accepts project/ref intent, an idempotency command
identity, a thread identity/title, and ordinary thread defaults. It does not
accept `cwd` or a target path. Under the project/repository mutation locks the
server resolves the persisted project root, lets the Git owner select the
managed checkout path, and then hands an internal workspace-thread creation to
orchestration. If owner persistence fails after Git creation, a private
rollback verifies and removes only that exact just-created registered
nonprimary checkout and the actual branch reported as newly created by Git,
including an automatically suffixed branch; the rollback is not registered as
a public RPC. Public `vcs.createWorktree` is also absent. Pull-request worktree
mode resolves the PR branch and then invokes this same atomic owner-creation
boundary, while `git.preparePullRequestThread` remains local-checkout-only.

`worktree.createPanel` accepts only a persisted host thread plus new panel
identity/title/defaults. The server re-reads the host under its mutation lock
and derives project, `panel` kind, branch, and worktree path. The panel
thread records the host as `hostThreadId` on `thread.created`, persisted in
`projection_threads.host_thread_id` and emitted on thread shells and details so
every client can open the panel under its host; public `thread.create` rejects a
client-supplied `hostThreadId`. A generic `thread.delete` emits `thread.deleted`
for every non-deleted panel whose `hostThreadId` is the deleted thread, live or
archived, in id order before the thread's own event and in the same command, so
the existing deletion effects clean up their sessions and terminals. Similarly,
`worktree.retarget` accepts project/thread IDs, an opaque worktree key, and an
expected catalog generation. It refreshes and revalidates present
nonprimary/nonbare membership and exclusive ownership before dispatching the
resolved metadata change. Neither operation accepts generic path or kind
authority.

`worktree.adopt` also requires `orchestration:operate`. Its public payload
contains an opaque catalog key, expected generation, project ID, command ID,
and ordinary thread defaults; it never accepts a checkout path. The handler
digests that decoded public payload before server resolution. An existing
receipt is checked before project/catalog lookup: an identical accepted retry
returns its durable result, while reuse of the command ID with any different
public field returns `command-conflict` without resolving or exposing a path.
The final resolved dispatch carries the same admission digest, so concurrent
preflight misses still conflict transactionally. The handler holds the same
stable project-then-physical-repository mutation locks. A stale
or non-authoritative generation forces one bounded refresh before the server
rechecks current registration, directory presence, nonprimary/nonbare
eligibility, canonical common-directory membership, and canonical thread
ownership. Present paths are canonicalized only after current Git membership
is proven. Adoption is read-only with respect to Git: it never creates or
repairs a worktree and never auto-runs a worktree-creation script.

After resolution, the server dispatches internal
`worktree.adopt-resolved` planning. The orchestration engine creates an
ordinary workspace thread, returns an existing active owner, or restores an
archived owner while updating the discovery baseline in the same
`persist_command` transaction. The public result is exactly the canonical
thread ID plus `created`, `existing`, or `restored`; replay of an accepted
command returns the original disposition from immutable result metadata on
the receipt-linked adoption event, without consulting a mutable thread
projection. Present but malformed metadata, or metadata inconsistent with the
transaction's immutable thread event, fails closed as an internal error. A
legacy receipt with no result metadata may recover `created` or `restored`
only from its matching immutable `thread.created` or `thread.unarchived`
event; a project-only legacy receipt cannot reconstruct `existing` from
current ownership and also fails closed. Canonical ownership compares one
server physical identity for create, metadata retarget, adoption, restart, and
replay. Present paths use filesystem canonicalization; a missing leaf uses its
canonical nearest existing ancestor plus normalized suffix. POSIX comparison
remains case-sensitive, while Windows drive and UNC keys normalize separators
and use native invariant uppercase mapping compatible with Windows ordinal
caseless identity. Symlink, macOS alias, lexical, non-ASCII special-fold, and
missing-leaf spellings therefore cannot create a second owner. Resolved
adoption and branch reconciliation command variants are rejected by
`orchestration.dispatchCommand` even though trusted server services may admit
them directly.

Worktree removal is a server-resolved three-method flow. The read-scoped
`worktree.getRemovalPlan` accepts only project and thread IDs and returns a
fresh catalog generation plus an opaque, versioned SHA-256 token. The token
length-frames the physical project, thread, repository identity, current
worktree key or missing-path identity, availability, dirty counts, target lock
state, and sorted normalized `(registration path, exact prune reason)` impact.
The operate-scoped `worktree.remove` accepts that token, generation, one of the
two explicit modes, and explicit dirty/prune confirmations; it never accepts a
path. Under the stable project-then-repository mutation locks, the server
re-resolves the persisted path, installs `Removing`, reruns Git preflight, and
rejects any token drift before mutation. Present deletion uses verified Git
removal and preserves the local branch. Missing cleanup uses the verified
target/prune owners; locked registrations and replacement directories fail
closed, while an attempted cleanup that fails may detach with a bounded
`failed` outcome. `worktree.removeFromBibCode` performs the same atomic detach
without requesting a Git mutation.

Planning and execution resolve the catalog's trusted repository anchor rather
than assuming the primary checkout is reachable. A pinned project may use the
primary, a present adopted sibling, or the lifetime common directory only when
the chosen anchor is not the target and resolves to the durable repository key.
After quiesce and while mutation locks are still owned, execution resolves the
anchor again and rechecks the pin immediately before Git. Missing primary,
anchor substitution, pin drift, or repository mismatch therefore cannot
transfer destructive authority.

After any successful Git decision, internal `worktree.detach-resolved` deletes
same-path panels, the canonical workspace thread, and its discovery baseline
entry in one orchestration transaction. Accepted-command result metadata makes
identical retries exact even after the thread projection is deleted; changed
payload reuse conflicts. A present Git mutation or absence verification
failure does not detach. The `worktreeCatalog` capability is advertised only
with the complete catalog handler set, scopes, and wire fixtures.

The durable detach transaction emits the same-path panel `thread.deleted`
events in stable order, the canonical `thread.deleted`, and one
`project.meta-updated` policy compaction. The receipt-linked immutable removal
result commits with those events. Adoption similarly commits its immutable
result, thread create/unarchive event when needed, and policy compaction in one
transaction. These event groups, rather than the current projection, are the
accepted replay proof.

Every command-ID reservation path first acquires the orchestration engine's
process-local command-admission claim. The engine keeps a weak, bounded registry
of per-ID gates, so exactly one live claimant may inspect or change that ID's
durable receipt or perform external preparation. Same-ID callers wait without
provider, attachment, Git, quiesce, or detach effects; after acquiring the claim
they re-read the receipt and replay, conflict, or resume it. Handing admitted
work to the engine worker transfers a shared claim that remains live after RPC
cancellation and is released only after terminal persistence or exact owned
reservation cleanup. Command claims are acquired before project/catalog and
workspace-admission locks. They are intentionally process-local: a restarted
engine has a fresh registry and may resume durable `reserved` or `prepared`
receipts.

Catalog mutations execute in a runtime-owned operation task. Client
`Interrupt`, socket closure, or request cancellation may win while command
admission, catalog refresh, or mutation-lock acquisition is still before the
engine-envelope handoff. Once the envelope is handed off, caller cancellation
only ends the wait: the server owner retains the command claim,
project/repository/physical-owner locks, removal reservation and cleanup slot,
`Removing` guard, quiesce lease, and any rollback responsibility until a
durable accepted/rejected terminal result exists. Runtime shutdown first stops
accepting these operations and drains their tasks, then shuts down catalog and
process owners. This is the lifecycle boundary tested by post-enqueue
WebSocket-interrupt and socket-close races.

The catalog-operation runtime has one named global bound of 64 in-flight
server-owned lifetimes. It performs non-waiting admission before spawning, so
there is no unbounded waiter queue. Saturation and closed shutdown admission are
typed `WorktreeOperationError` results (`operation-capacity` and
`operation-shutting-down`). Each accepted task owns its permit through the
terminal result; completion releases capacity and shutdown closes admission
before draining all accepted tasks.

The clone runtime (`apps/server/src/production/clone_operations.rs`, owned by
`GitVcsRpcServices`) serves `vcs.clone` and `sourceControl.cloneRepository`. It
keys one live clone per destination: the canonical parent folder plus the leaf
derived from `directoryName` or the URL. The leaf must be a single folder name:
the server refuses a `directoryName` that is a path, `.`, or `..`, and a URL that
yields no usable name (never echoing the URL) before admission, the disk check,
a cancel, or the reservation, so a destination key always names the folder the
kernel resolves. A request for the same URL joins the live clone and shares its
outcome; another URL is refused as `busy` at once, without naming the running
clone's URL. Errors follow each method's declared union. `vcs.clone` reports
the leaf refusal as a `GitCommandError` and runtime refusals (`busy`,
`capacity`, `shutting-down`, `not-in-progress`, `cancelled`) as
`GitCloneOperationError` reasons. `vcs.cancelClone` answers `{ cancelled }`,
or a `GitCommandError` for the leaf refusal or a parent folder it cannot
resolve; it has no runtime refusal. `sourceControl.cloneRepository` reports
every clone-runtime failure, the leaf refusal and a failed clone included, as a
`SourceControlRepositoryError` carrying the same detail. A new clone of the same
URL waits for a cancelled one's cleanup, then starts. Admission never waits: at
most 16 clones are live, and a full or closed runtime answers `capacity` or
`shutting-down`. Each clone owns a root cancellation token, a tracked task, and
a clone of its starter's admission permit, so the update drain names a detached
`vcs.clone` until Git has stopped and its cleanup has finished. Git runs in a
task of its own: the clone's task removes the folder it created after any
failure, cancellation, or panic of that transfer, and only then publishes the
outcome and frees its slot. A cancelled clone whose folder could not be removed
reports that folder, not a clean cancel. `attach: true` is join-only: it joins
a live clone, returns a failed or cancelled outcome retained for the same URL,
or runs the reuse check without reserving (the finished clone's path, the
incomplete-clone error, or `not-in-progress`). An attach that raced a new clone
admits again and joins it. Outcomes are retained for five minutes per
destination and URL and swept on every access. None is dropped early: once
live clones plus retained outcomes reach 256, a new clone is refused with
`capacity` until outcomes expire.
`vcs.cancelClone` takes the clone's own input, cancels a live clone of that
URL, and answers `{ cancelled }` after that clone's outcome is published:
`true` unless Git had already succeeded, and it never touches a finished clone.
A caller that leaves a waiting `vcs.clone` or `vcs.cancelClone` gets an
`Interrupt` exit, never a typed failure. Shutdown closes admission, cancels
every live clone, and drains beside the worktree-removal tasks, so Git stops
and each partial folder is removed before providers and terminals shut down.
Clients send `attach` and `detach` only when the server advertises
`vcsCloneReattach`: `CloneInput` accepts unknown fields, so an older server
would run an `attach` as a new clone.

With that claim held, an already-accepted retry is replayed first. Every new
removal must then acquire one finite runtime-cleanup lifetime slot before receipt
reservation, `Removing`, quiesce, Git, or detach; saturation returns the retryable
`cleanup-capacity` failure with no removal mutation. The slot follows the request
through foreground cleanup, queued retry, retry backoff, cancellation, or
shutdown. Once admitted, removal atomically inserts an immutable generic command
receipt in `reserved` state. A different payload or project for that command ID
conflicts even when requests hold different physical-repository locks. Generic
commands finalize receipts with compare-and-set semantics: an intervening
removal reservation/preparation cannot be overwritten, and project creation
reserves its identity before directory or Git initialization. After the fresh
plan token and mode-specific preflight pass, the receipt advances to `prepared`
and is re-proved immediately before Git; successful detach upgrades it to
`accepted` in the same transaction as the events. After a live predecessor has
released its claim, matching `reserved` work is safely re-preflighted, while
matching `prepared` work may infer a prior verified Git success only when the
exact target is now missing and unregistered. This makes a crash between Git and
detach resumable without allowing an unvalidated request to claim success.

RPC preparation that can change an external provider or publish attachment
files follows the same durable arbitration rule. A model-changing
`thread.meta.update` reserves the exact command aggregate and canonical payload
digest before calling the provider. A turn start reserves that identity before
attachment publication, provider identity lookup, or delivery-route freezing.
An attachment sent by id without its bytes is accepted only when an accepted
command already attached that id in the same thread, the stored file's size
equals the request's claimed `sizeBytes`, and, when a digest was recorded for
that id, the file still matches it; the attachment cap and each id's shape are
checked before that lookup, and any other id is refused as `InvalidRequest`
before anything persists.
An accepted replay performs none of those effects; a concurrent matching caller
waits for the live claim and then replays without repeating preparation. A
matching reserved receipt is restart-resumable, while a changed payload conflicts
before external work.
Provider failure leaves the matching metadata reservation resumable. Turn
failures before worker enqueue release only the exact matching reserved receipt,
and a failed or canceled worker command performs the same conditional release,
so it cannot delete a replacement, accepted, or rejected receipt. Attachment
publication remains rollback-owned until the command, references, and provider
outbox commit atomically; startup scavenges a final file left by a process crash
after reservation and publication but before that transaction.

### Attachment staging

Servers with `capabilities.attachmentStaging` accept chat attachments through
`uploads.begin`, `uploads.append`, `uploads.get`, and `uploads.cancel`. These
unary RPCs use the existing WebSocket framing and authorization;
all four require `orchestration:operate`. Attachments travel as several
acknowledged requests, each carrying at most 1 MiB of canonical base64-decoded
bytes. The client normally keeps at most two smaller chunks outstanding.
Each append acknowledges only bytes written and flushed to disk. A completing
append requires a verified SHA-256 supplied either at begin or completion.
Zero-byte uploads finalize with a digest at begin or an empty append.

The Rust server owns one registry shared by upload handlers and durable turn
admission. Ownership follows the authenticated session across reconnects.
Unknown, expired, and foreign upload IDs return the same `UploadError` with
`reason: "not_found"`; cancellation of inaccessible IDs returns an empty success.
There are at most 16 open stages per owner, 10 MiB per stage, and 256 MiB of
declared staging bytes across the server. Partial and complete unbound stages
expire after ten idle minutes; begin, acknowledged append, and get refresh
activity. An operation may keep completed stages alive with get while uploading
other attachments. A minute sweeper and admission-time expiry release quotas.
Server startup removes only owned UUID upload leaves in `attachment-uploads/`;
durable attachments remain in `attachments/`. Shutdown closes staging admission,
joins admitted writers and the sweeper, and removes remaining stages.

A turn names `uploadId` instead of `dataUrl`. The staged branch runs before
attachment-by-ID reuse, rejects both byte sources and duplicate upload IDs, and
checks owner, completion, name, type, MIME, size, and verified content digest.
It holds the upload's write guard while publishing a final attachment through a
hard link or bounded copy. Only finals created by that batch are rollback-owned;
identical existing files are adopted. Failed durable admission removes owned
finals and retains completed stages for retry. The durable commit consumes the
bound stages and quotas, leaving only attachment metadata in events and the
provider delivery outbox. Direct and queued turns share this transaction.
Accepted command replays skip staging preparation. Legacy clients and servers
continue using inline data URLs when the capability is absent.

Canonical workspace ownership is protected by a server-owned global fence keyed
by physical host identity. Present paths canonicalize through the filesystem;
missing leaves resolve the nearest existing ancestor and append their normalized
suffix. The same key strategy covers Git/catalog joins, owner create/delete/
retarget/adopt/detach, availability, project-root mutations, removal, and
cross-project cleanup. Windows drive and UNC keys use native invariant
uppercase mapping compatible with Windows ordinal caseless identity; POSIX keys
remain case-sensitive. Mutations acquire deterministically
ordered keys before worker enqueue. Generic public creation cannot nominate a
kind or path; trusted resolved commands still acquire every affected physical
owner key. Removal discovers a server-owned candidate key, acquires it, reruns
the authoritative unique-owner preflight, and retains the lease through verified
Git and detach publication. A mutation that waited behind a committed owner
change re-resolves the current old/new keys before reacquiring the fence. A
committed removal invalidates the stale waiter instead of allowing it to publish
ownership, and detach retains the transaction-time ownership check as a defense.

Every healthy authoritative catalog publication also compares active adopted
worktrees with durable thread branch metadata. A change dispatches one
idempotent `thread.meta-updated` command whose ID contains the thread ID plus a
versioned hash of the observed branch and HEAD, never a path. Unchanged healthy
observations emit nothing, and refreshing/degraded retained snapshots never
reconcile branch state.

The production runtime owns one catalog service built from the same Git
repository and orchestration repositories used by Git/VCS and project state.
After its Git rollback identity and durable owner receipt settle, successful
dedicated managed creation records the exact server-created physical path for
bounded suppression and invalidates the shared repository fingerprint plus its
project catalog. Retarget invalidates after its durable receipt, and dedicated
verified Git removal invalidates the fingerprint and every matching live view
of the durably pinned repository after terminal settlement. The public raw-path
`vcs.removeWorktree` method is not registered,
typed, scoped, or bridged; the same is true of raw `vcs.createWorktree`. The
only rollback removal is a private server call
for the exact just-created registered checkout when managed owner persistence
fails. Pin mismatches and unverifiable identities fail observation closed.
Observation failure never changes a successful Git response into a failure.
Server-owned destructive Git primitives re-read repository inventory before
and after mutation and never accept a client path as authority. Repository-wide
stale-registration cleanup binds confirmation to a versioned digest of every
sorted normalized registration path and its exact dry-run prune reason; either
kind of drift fails closed before mutation. Runtime shutdown permanently
closes the service under one lifecycle-registration mutex before draining
pollers, queued mutation refreshes, repository-observation leaders, scans, and
eviction work. Every spawned task registers an abort handle under that mutex
and removes it through a completion guard, so shutdown can abort and wait for
the bounded active set, and a final release racing the terminal transition
cannot register post-drain eviction. Ordinary view detach still permits an
aliased subscribed view to keep an exact-anchor repository observation alive;
terminal shutdown aborts and joins every such leader. Task registration takes
entry state before the short-lived lifecycle mutex. Shutdown holds the
lifecycle mutex only long enough to mark terminal and copy abort handles, then
releases it before acquiring the main registry, entry, or repository locks.
Observation result publication takes the lifecycle mutex before repository
state and skips publication after terminal transition. Later subscribe,
refresh, invalidation, and release paths cannot restart the service.

### Terminal image paste

Servers with `capabilities.terminalImagePaste` accept
`terminal.stageImagePaste { uploadId, name, mimeType, sizeBytes }`, which needs
`terminal:operate`; the upload before it needs `orchestration:operate`, which
every issued scope bundle that grants `terminal:operate` includes. A terminal program reads its own host's clipboard, never the
client's, so the web terminal turns an image-only paste (no `text/plain`) into a
staged upload and asks the server for a file the program can open. The upload
uses the `chat-attachment` target with type `image`, so it shares the staging
limits, ownership, and expiry above. The server binds the completed upload,
accepts only PNG, JPEG, GIF, and WebP, copies it to
`terminal-pastes/<uuid>.<ext>` under the state directory, retires the stage, and
returns the absolute path. The client sends that path as terminal input,
escaped for the server platform (backslashes on POSIX, quotes on Windows) when
it contains other characters, and wrapped in bracketed-paste markers when the
program enabled them. The paste
reserves its place in the terminal's persistent input queue, so input typed
during the upload waits behind the path within the 1 MiB pending limit, and a
terminal restart drops both. The terminal shows the upload with a Cancel
action, which releases the held input. A result for an earlier terminal process is dropped, a failed commit
releases its stage, and closing the viewport aborts the upload. Codex and Claude Code
attach an image from a pasted path. Copies are not tied to threads: each paste
removes files in `terminal-pastes/` older than 24 hours. Against a server
without the capability, the terminal reports that image paste needs a newer
server instead of sending an empty paste.

### Missing-workspace runtime guard

The production runtime owns one `WorkspaceAvailabilityRegistry` and injects
that same instance into the catalog, orchestration, terminal, Git/VCS, and
workspace/file/search/review RPC owners. The registry is the server-side source
of truth for whether an adopted workspace may begin new path-dependent work.
It indexes both the durable thread ID and the workspace's physical identity.
Present paths canonicalize through the filesystem; missing paths resolve their
nearest existing ancestor and retain a normalized missing suffix. Path guards
therefore cover the workspace root and descendants across present and missing
symlink/macOS/lexical aliases. The public failure is the structured
`WorkspaceUnavailableError`, including the thread ID, last-known path, and
catalog availability.
If physical identity resolution returns anything other than `NotFound`, the
registry returns typed `WorkspaceIdentityError` and aborts guard admission,
loss/removal installation, recovery, or snapshot reconciliation without
changing existing authority. Only a genuinely missing suffix is resolved
through its nearest existing ancestor.

Only a healthy authoritative catalog snapshot may change this state. Catalog
reconciliation synchronously closes every superseded terminal-signal gate, then
drains already-owned permits asynchronously without retaining availability,
catalog-entry, or catalog-registry locks. The refresh fence is revalidated after
that drain. While the catalog publication lock is held again, reconciliation
commits the guard change before the new snapshot becomes visible or a
runtime-loss callback can run. Bootstrap follows the same close, unlocked
drain, and commit-before-publication order. Degraded scans retain the prior
state and perform no teardown. Loss work is admitted once per `(threadId,
generation, availability)` transition. Exact recovery clears the guard only
when the same physical path is present again in the same physical repository;
an active removal guard takes precedence over catalog loss and recovery.

Missing-path identity also collapses duplicate separators plus lexical `.` and
`..` components without escaping POSIX roots, Windows drive roots, or UNC share
roots; Windows comparison uses native invariant uppercase mapping compatible
with ordinal caseless comparison rather than ASCII or Unicode lowercase.
Public work admission takes a path-scoped lease after resolving the durable
thread projection. This includes panel threads that do not appear in the
workspace catalog: their persisted worktree path, or their project root when
they have no override, is authoritative. File, browse, search, asset, transfer,
review, and filesystem-mutation handlers retain that lease for the entire
operation, not just an entry-point check. Mutations acquire its finalization
permit before the filesystem/durable commit boundary. Turn and process handlers retain the
lease through durable command admission or external-process publication. An
ordered terminal input session retains the lease it took at
`terminal.beginInput` for its whole life, so its frames need no admission of
their own (see [Ordered terminal input](#ordered-terminal-input)). Guard
installation and lease admission are serialized, so loss either waits for work
owned by an earlier lease and finalization permit or rejects a later lease.
Lease drop, including cancellation and error unwinding, releases every
thread/path scope. Every lease also carries the exact loss error and a
cancellation token.
The turn RPC transfers its lease into the queued engine envelope, so client
disconnect only stops the caller wait: an already-admitted command retains its
existing durable-delivery semantics. Authoritative loss cancels that envelope
before persistence and the worker drops the lease only after it has produced
the dispatch result. The same envelope carries a generic commit fence into the
blocking SQLite worker for both accepted commands and persisted plan-error
rejections. Immediately before the real transaction commit, the worker takes
an owned finalization permit. Loss and removal serialize with that permit: if
loss wins, permit acquisition is rejected and dropping the transaction rolls
back its receipt, events, projections, attachment references, and provider
outbox; if commit finalization wins, guard publication waits until the local
commit has completed. Permit drop on success or error always wakes a waiting
loss transition. Gate rejection publishes the exact structured loss on the
admission while holding that same gate, before the SQLite worker can observe
rejection. The RPC therefore reports `WorkspaceUnavailableError` even when the
rejected database result is ready before cancellation notification. The fence
is persistence-generic; neither orchestration nor SQLite imports worktree
availability policy. Nested removal guards retain independent tokens;
arbitrary drop order cannot reveal a pending missing workspace before the last
removal completes, and removal cancels already-admitted matching work just like
authoritative loss.

Removal quiesce uses an exact identity minted by its `RemovalGuard`, not a
synthetic catalog-loss generation. Provider and terminal cleanup resolve
same-path aliases while projections still exist and run under the existing
five-second deadline, global concurrency cap, bounded reaper queue, and runtime
shutdown owner. A failed or timed-out attempt returns
`orphanCleanupPending: true` only after the reaper has retained a distinct
exact removal token and the resolved alias IDs. Durable detach commits that
lease so cleanup may finish after projections disappear; any preflight, Git,
or orchestration failure drops the lease, cancels the queued retry, and releases
its retained guard before it can affect a later session.

The retry supervisor admits removals through the finite lifetime slots described
above. Its nonblocking handoff can therefore contain no more removal jobs than
there are slot holders, including foreground, queued, active, and retry-backoff
work. A round-robin scheduler shares capacity among catalog-loss work, newly
queued removals, and retries. Failed and timed-out removal jobs retain their
exact `Removing` guard and slot, wait a cancellation-aware bounded backoff, and
re-resolve aliases on every attempt instead of hot-looping. The server seeds
retries with preflight-known aliases so same-project panels remain cleanable
after detach. Shutdown cancels and drops active, queued, and backoff work,
releasing both ownership and capacity.
Destructive removal resolution additionally spans live projects durably pinned
to the same repository identity and exact physical checkout path; unrelated
or unpinned repositories are excluded. A verified Git removal/cleanup
invalidates every live catalog view sharing the repository entry, whereas
detach-only and failed cleanup remain scoped to the initiating project.

The guard rejects a new turn before durable admission; terminal open, restart,
write, input begin, and restart-on-attach, and seals live input sessions;
client Git status and mutations; and project
file, search, mutation, editor, asset, and review operations. It intentionally
allows catalog refresh, conversation/history reads, non-restarting terminal
attach, terminal close, dedicated detach/removal, ordinary non-worktree thread
deletion, and direct internal cleanup Git operations. Generic deletion of an
adopted owner, and project deletion while any adopted owner remains, fails
closed. Guard checks occur before the affected owner starts durable or external
side effects, so a path disappearing between client resolution and handler
execution cannot fall through to a generic filesystem or process failure.

Provider delivery and restart reconciliation repeat the persisted thread/path
admission immediately before provider routing. This closes the gap between a
durable turn commit and asynchronous delivery. Git process boundaries likewise
hold a path lease through command execution or the lifetime of a long-lived
subscription. Loss cancels the operation child token and stream publication
returns the same structured unavailable error. Terminal process publication
checks its lease cancellation after spawn and again under the manager
publication lock; a PTY that finishes spawning after loss is killed by its
uncommitted-process owner and is never inserted as a live session.

Initial PTY spawn and prepared-command fallback both run on Tokio's blocking
pool. Unix PTY cleanup marks inherited descriptors close-on-exec, preserving
Rust's spawn-reporting socket until exec succeeds or reports an error. A failed
executable or interpreter launch cannot publish a successful PTY process.
The blocking task constructs the uncommitted-process guard before
returning its result; the join carries that guard until session supervision
takes ownership. The guard also retains the per-terminal operation lock, so
cancelling the caller cannot admit a same-key replacement before the late
process has been killed. Cancelling or dropping the async caller therefore kills
an unclaimed late process when the blocking result is discarded, including after
terminal-manager shutdown. The native PTY backend retains its process waiter,
which cleans up the process group or Windows Job and reaps the root independently
of the caller's async task. A started native spawn cannot be forcibly cancelled;
this ownership guarantee applies when that call returns, without blocking an
async runtime worker for its duration.

Runtime shutdown adds a separate per-runtime process-admission fence before
provider and terminal managers drain. A terminal spawned after that fence is
rejected as shut down and its uncommitted PTY owner kills and waits for the
process tree before the RPC completes. Registered terminal and provider roots
are captured by exact PID and creation time; residual cleanup follows only
their descendant closure and cannot signal children registered to a peer
runtime in the same application process.

Native PTY exit completion is retained even when no observer is subscribed.
A terminal that exits before manager subscription, or while close replaces the
supervisor's subscription with its cleanup subscription, still exposes its
completed reap result to the later subscriber. Closing that terminal does not
wait for a second exit notification or extend the cleanup deadline.

For each admitted loss transition, `WorktreeRuntime` resolves every live
ordinary or panel thread in the same persisted project whose physical path
matches the guarded physical workspace. It deduplicates those IDs, appends one
warning activity to the catalog owner with a deterministic transition-derived
ID, requests every affected provider session to stop, and quiesces every
affected terminal. Terminal quiesce
signals all processes before waiting and retains each session as an exited
snapshot with its bounded transcript; it does not use destructive terminal
close. Conversation and thread rows are likewise retained. A warning-write
failure is logged but cannot prevent process cleanup.

Provider cleanup captures the supervisor's exact active runtime identity only
while the loss transition remains current, then asks the supervisor to stop
that identity and settle the thread. The actor rechecks identity against its
current thread session; a live replacement is neither stopped nor settled.
When no live session remains and the projection is starting, connecting, or
running, it uses the shared restart reconciliation function at the loss time:
streaming assistant text is retained and settled, the active turn is cleared,
the turn ends as error, and the session reports `session_stopped` with
"Provider session stopped because its workspace became unavailable. Review
delivery status before continuing." A shutdown error does not skip settlement
after detach. Cleanup also requests settlement without a captured identity, so
a later attempt can retry a failed settlement. Ready projections are left alone;
removal cleanup retains its existing stop-only behavior. Accepted start publication remains owned by the supervisor's current driver
identity and delivery generation. A per-session publication fence captures the
terminal revision before native delivery and serializes both running writes
with the pump's complete core batches. Cancellation, closure or any terminal
observed since admission suppresses optimistic running publication without
changing Accepted or resending input, including unknown native turn IDs. A fresh
later admission uses the newer revision. Detach closes and drains admitted
submitted core writers before pump abort and runtime deletion; successful
restart uses the same drain and a fresh replacement fence after native shutdown.
Shutdown failure preserves the existing old-session behavior. Retry resolution repeats
capture only while its transition ownership is current, and recovery/newer-loss
cancellation still short-circuits the whole
attempt. Terminal cleanup applies the same transition-scoped pattern to every
session for the affected thread: it captures the exact session, generation, and
process only while the loss transition is current, then acquires a counted
terminal-signal permit owned by that exact guarded transition. Permit
acquisition revalidates the transition after capture and is the linearization
boundary with recovery or a newer loss. If invalidation wins, the stale cleanup
cannot signal; if cleanup wins, synchronous gate closure prevents later permits
while an asynchronous, lost-wake-safe drain waits for the already-owned permit
covering the terminal lifecycle-lock identity check and process signal.
Concurrent canonical and persisted-alias cleanup calls hold independent
permits, so the drain waits for every signal already authorized without parking
a Tokio worker. Cancellation keeps the old gate closed until those permits
drain, then reopens it only if the prepared catalog transition was not committed.
Registry invalidation never takes a terminal lock, and permit release never
re-enters the registry. A stale cleanup therefore skips both an unchanged
recovered session and a recovered replacement published under the same terminal
key. Exit finalization keeps its existing exact-generation check, so an older
process result cannot overwrite replacement history.

The single graceful cleanup deadline starts when loss quiescence begins and is
five seconds. Warning persistence and known canonical provider/terminal
cleanup start immediately, in parallel with persisted alias resolution. Any
resolved non-canonical aliases are cleaned under that same original deadline;
a stuck resolver therefore cannot delay cleanup of the known owner. Timeout or
cancellation drops an owned terminal-signal permit with the cleanup future, so
the signal gate adds no separate wait beyond the existing cleanup lifetime.
Active admissions are canceled rather than awaited without a bound.

Incomplete, failed, or panicked cleanup is marked as
`orphanCleanupPending` and handed to the runtime-owned reaper. Its queue and
active set are bounded to 64 jobs. One runtime-owned semaphore admits at most
16 cleanup attempts globally across overlapping catalog observers and reaper
work. Each reaper job owns exactly one independently five-second-bounded
attempt, including fresh alias resolution, and always releases its permit on
success, error, timeout, recovery, or shutdown. Failure retains the marker for
later explicit reconciliation; it does not loop and monopolize a permit. The
ownership is keyed to the exact transition. Exact recovery or a newer loss
transition cancels stale queued/active ownership before it can stop recovered
or newer sessions, including ownership retained after queue saturation. Only
confirmed provider-and-terminal success while ownership is still current
clears the marker. Saturation is logged without clearing the workspace guard
or orphan marker. `ProductionRuntime` shuts down the catalog observer first,
then cancels and drains the reaper's queued and active futures before stopping
provider and terminal owners.

## Provider turn flow

```mermaid
sequenceDiagram
  participant UI as Client runtime
  participant RPC as RpcRegistry
  participant Engine as OrchestrationEngine
  participant Delivery as TurnDeliveryService
  participant Provider as ProviderRuntimeSupervisor

  UI->>RPC: thread.turn.start
  RPC->>Engine: validate and admit command
  alt queued requested while session is running or starting
    Engine->>Engine: persist user message and queued/start outbox row
    Note over Engine,Delivery: queued rows create no turn or Working state
  else ordinary start
    Engine->>Delivery: pending/start delivery
    Delivery->>Provider: provider-native turn start
  end
  Engine-->>RPC: durable command result
  RPC-->>UI: typed Exit
  Engine-->>UI: subscribeThread / subscribeShell snapshots

  opt Steer queued head while running, with provider capability
    UI->>RPC: thread.turn.steer(messageId)
    RPC->>Engine: validate head and active turn
    Engine->>Delivery: pending/steer with expected turn ID
    Delivery->>Provider: ProviderDriver.steer
    alt provider acknowledges input
      Provider->>Engine: delivered with runtime turn ID
      Engine-->>UI: user message belongs to that turn
    else definite rejection or unavailable turn
      Provider->>Engine: return to queued/start with delivery.detail
    end
  end

  Provider->>Engine: canonical turn settle
  alt ready, unheld head, no pending approval/question or delivery
    Engine->>Delivery: wake after committed ready
    Delivery->>Engine: thread.turn.promote(oldest messageId)
    Engine->>Engine: stamp promotion time and persist pending/start
    Engine->>Delivery: one promoted turn
    Delivery->>Provider: provider-native turn start
  else interrupted or error
    Engine->>Engine: hold queued rows until explicit Send now
  end
  Engine-->>UI: durable turn, delivery and queue updates
```

Unary command acceptance is not a promise that an external provider process
will finish successfully. Provider delivery and completion are reflected by
subsequent durable orchestration events. Resync after reconnect is snapshot-first:
`subscribeShell` and `subscribeThread` send a whole snapshot. `subscribeThread` and
`subscribeShell` register their durable-event receiver before reading the
initial snapshot, so a commit concurrent with that read is queued and then
projected instead of being lost between snapshot and live delivery. Live
updates are coalesced: a thread stream re-reads its thread 100 ms after the
first of a burst of its events, and the shell stream re-reads the shell
500 ms after the first event of a burst. A thread subscriber that sends
`patches: true` then receives a `patch` item against the snapshot it holds:
changed fields are replaced, removed fields listed, and each changed array
sends its new length and only the entries that differ, by index, so an
appended activity or a message updated in place costs one entry instead of
the whole thread. To compute the next patch the server keeps, per subscription,
a 64-bit fingerprint of each field of the snapshot it last sent and of each
array entry, never the thread itself: a held copy grew the server by about
twice the thread's content for as long as the thread stayed open (#101). The
client decodes only the changed fields with the thread's field
schemas, and `packages/contracts/fixtures/thread-stream/lifecycle.json` pins
that patches rebuild the server's snapshot exactly. Older clients omit
`patches` and keep receiving whole snapshots; older servers ignore it.

A resubscribe resumes instead of resending the thread
([design](../superpowers/specs/2026-10-17-thread-resubscribe-resume-design.md)).
For a `patches: true` subscriber, every `snapshot` and `patch` item carries a
`revision`: a 128-bit digest of the thread's held fingerprints, keyed by
random keys secret to the server process. The client records the revision of
the last item it applied in full and sends it on each resubscribe as
`resume: { revision, snapshotSequence }`. The server loads the thread as
before, and answers `{ kind: "unchanged", snapshotSequence, revision }` only
when the thread still has that revision and its sequence is at least the
client's. It then patches against that thread. Anything else gets the whole
snapshot: a change the client missed, a revision from before a restart (new
keys), one the server never produced, a sequence newer than the server's, or a
client that does not apply patches. The client clears its revision whenever it
changes the thread without one (an event item, deletion), so it never resumes
from a thread it does not hold. Older clients send no `resume` and older
servers ignore it and send no `revision`, so either side alone keeps today's
full snapshot. `packages/contracts/fixtures/thread-stream/resume.json` pins the
items; revisions in it are placeholders, because they mean nothing to another
process.
A thread stream ends with its first error, such as `Thread … was not found`
for a missing or deleted thread, whose targeted load reads none of its rows.
The client retries a failed `subscribeThread` or `subscribeActivity` with
backoff starting at 250 ms, doubling to a 30 s cap, and resetting after the
stream stays up for 30 s: a thread can appear just after its view subscribes,
while a view of a deleted thread retried every 250 ms for as long as it stayed
mounted (#113).
The client's thread detail projection keeps every object of a new snapshot or patch
that equals what it already shows, and remembers which source object resolved
to which shown one so later patches compare only what they changed. A resync
snapshot therefore does not hand the UI a second copy of an unchanged thread;
each copy used to stay pinned by memoized render closures (#96).
Every shell snapshot, live or from `orchestration.getArchivedShellSnapshot`,
is read in one SQLite job: projects, the undeleted live or archived threads
whose project row exists, and per thread only its session, latest turn, the
messages tied for its newest user and assistant message, and, while the latest
turn runs, the tool activities tied for that turn's newest. A thread snapshot
reads that one thread's rows. Neither reads the rest of the store, so their
cost follows the threads shown and the thread opened rather than the whole
message and activity history; their wire output equals what the full-store
load produces, and only the HTTP read model below loads the whole store. A
thread's activities are read in the order of their
`(thread_id, sequence, created_at, activity_id)` index, so the listing needs
no temporary sort that would spill large payloads to a temporary file.
`orchestration.replayEvents` with `paged: true` returns about 1 MiB pages
`{ events, exhausted }`, keeping at least one event when any remain. Replay
resumes from the last applied `sequence` with no server state; the cursor is
scoped to the environment and storage instance. An array answer from an old
server is one exhausted page.
`orchestration.replayEvents` refuses a negative `fromSequenceExclusive` with
`InvalidRequest`, as the contract's non-negative integer requires.

The `subscribeShell` thread-shell contract has an additive optional
`conversationPreview` field with `prompt`, `tool`, and `assistantMessage`
members capped at 200, 160, and 320 characters respectively. The server trims
the values and truncates them at character boundaries; `tool` is populated only
while the latest turn is running. The field is omitted when there is no preview
content, and older servers omit it.

A turn whose delivery was refused, or whose fate the provider never confirmed,
is reported in the open thread by the delivery notice and, independently, as an
unresolved-delivery field on the thread shell. `OrchestrationEngine` derives that
field from the durable outbox inside the same transaction that commits the
delivery transition, so it is atomic with the transition and is recomputed —
and therefore cleared — by every later transition to pending, delivered or
dismissed. Deriving it keeps the projection owner unchanged: no delivery
component writes the provider session, and the field never alters session
`status`, provider identity, runtime mode, the active turn, or any turn row.

A launch refused because the selected model or session does not accept the
turn's own options fails the delivery once, because the unchanged durable
payload would be refused on every attempt. Its failed delivery carries the
optional reason `modelSelectionRefused`, also used when a frozen turn's model
or options are refused before delivery. A launch refused because the provider
itself cannot be driven, such as an unsupported version, or because its
executable is missing (`ProviderRuntimeError::MissingExecutable`), fails the
delivery once without a reason; a retry after the setting is fixed, or once the
executable exists at the same path, launches normally (a retry keeps the
turn's original route). Other launch failures, such as another spawn error,
stay definitely not sent and retry with backoff: 50 ms, doubling to a 30-second
cap per delivery (a one-second cap retried a provider that could not start
every second). A retry that fails again with the same detail returns the
delivery to exactly what the thread already shows, so it records the attempt
but appends no `thread.turn-delivery-updated` event. A provider that asks to
send the message later (OpenCode answering 408 or 429) leaves it definitely not
sent too; its next attempt waits for the backoff and for any `Retry-After` it
named, capped at 30 seconds. The delivery detail is what the turn shows, so every one is plain text from a single
formatter that names the provider by its instance label, never by a driver id:
the refusal itself, such as "Fast Mode is not supported by the selected model.";
for a spawn failure, that the provider could not start and why; for an uncertain
delivery, that the Claude instance never confirmed a written message, or that
BiBCode can't check after a restart whether a provider without exact
reconciliation received it. Error text, logs and runtime rows keep the internal
detail.

### Durable message queue

The server owns the queue through durable outbox rows and projected user
messages. Delivery metadata includes state `queued`, optional `mode` (`start`
or `steer`, absent means `start`), and optional `held`. Turn-start commands accept an
optional boolean `queued`; steer and promote commands address an existing
message by `threadId` and `messageId`. Promote is available to both client and
internal callers. Delivery resolution recognizes `cancel`. Provider snapshots
may advertise `supportsTurnSteer`; omission does not advertise support.

The client runtime exposes steer/promote commands through the existing typed
RPC and serial per-thread command scheduler. A `thread.turn-steer-requested`
event marks the existing message's delivery pending with mode `steer` without
changing the session or latest turn. Delivery updates replace delivery data,
apply optional `held`/`mode` payload overrides, bind an optional `turnId`, and
remove the addressed message when `withdrawn` is true. Missing messages are
not recreated by these updates.

The web composer requests queued admission while a turn is running, the
session is starting, or an earlier non-queued start delivery remains pending or
sending. It decides this before the local dispatch/busy guard and creates no
new local dispatch or optimistic message for the enqueue. The request-in-flight,
provider-conflict, workspace, and environment guards still apply.
Cards derive from the thread snapshot in its durable FIFO order, after the
working indicator; queued messages and pending/sending steers are excluded from
normal message rows and busy delivery state. Once delivered, a steered user
message belongs to its attributed turn, including that turn's fold.

Only the head can steer, using the bound provider instance's advertised
capability. Held heads and non-running sessions offer Send now; it waits for
running/starting sessions and pending approvals/questions to clear. Cancel
withdraws through the existing command and restores text to the cancelling
client's current draft. A session-local file cache holds only locally enqueued
Files; missing files and attachment-limit overflow are reported. Stop cancels
and restores queued messages head to tail before interrupting, continuing past
individual cancellation failures. Failed queue actions remain visible on the
card. The composer consumes the configured steer shortcut before submit,
menu selection, or Shift+Enter newline handling.

Queue admission and delivery are owned by the Rust server. Admission resolves
`queued: true` against the projected session: running or starting retains the
flag and persists a queued outbox row; other states rewrite it to false and use
normal pending delivery. The original request digest still owns command replay.
Admission stores the model selection even when the request omitted it, together
with the turn's modes and provider route. Queued admission records the user
message and complete delivery metadata without a turn-start event, pending turn,
or session-status change.

The outbox persists `queued`, delivery `mode`, and `held` alongside the original
command receipt and payload. The message projection stores delivery state, mode
and hold metadata for snapshots and event replay; it is a view of the outbox,
which remains the queue's source of truth.

The outbox's nullable `failure_reason` and the message projection's nullable
`delivery_reason` carry a state-scoped typed reason: `modelSelectionRefused` only
for a failed delivery, and `startedNewConversation` only for a delivered retry.
The engine writes the reason with the state and delivery event in one
transaction; `thread.turn-delivery-updated` projects its nested `delivery.reason`
into messages and snapshots. Retry and dismiss clear failed-delivery reasons;
delivered rows remain terminal. Every later
delivery update replaces the projected reason, clearing it when absent. Event
replay restores the reason. Old rows and events have no reason, and contracts
decode unknown reason values as an absent key. Delivery events and projected
messages also carry the delivery's `providerInstanceId`, from the outbox's
`provider_instance_id` through the nullable `delivery_provider_instance_id`
projection column into snapshots. The projection keeps it once set; old rows
and events lack it.

The web decides which deliveries wait behind an unresolved one by comparing
message `createdAt`, relying on the outbox `created_at` equalling the admitted
message's `createdAt`; only promotion restamps the message, later.

This W1 order proxy has a blind spot when the failed head was itself promoted:
its message `createdAt` is the promotion time, while its outbox row retains the
enqueue time. A pending start admitted between those times sits behind it in
outbox order, but `waitsBehind` returns false, so that row shows no waiting line.
Conversely, a promoted pending row whose enqueue time is older than the head's
can show a transient false waiting line until it is claimed. Both errors are
permissive or transient; the outbox remains authoritative for delivery order.

The delivery worker examines the oldest queued row per thread before filling
its available slots. Queue FIFO uses the original command receipt's durable
result sequence, so equal client timestamps or clock skew cannot reorder
admitted messages. Thread snapshots retain that same order for queued messages;
clients rendering queue cards preserve it instead of sorting their timestamps.

Automatic promotion requires an unheld head, a ready session, no pending/sending
delivery, no running projected turn, no pending approval or user-input
request, and no [checkpoint revert](#checkpoint-revert) of the thread in
flight; the delivery worker also claims nothing for such a thread. The gate reads approvals and the latest durable user-input activity
for each request, rather than the thread summary's input counter. A running
turn placeholder continues to block the next promotion after provider acceptance
releases the worker slot, until the provider's running and ready cycle settles
that turn. An unbound placeholder for a delivery dismissed while pending or
failed does not block promotion; resolution metadata records that prior state
so a rejected head can be dismissed without stranding its tail. Dismissal of
sending or uncertain work does not prove the provider received nothing, so
those placeholders and bound running turns continue to block automatic
promotion. OpenCode, whose session status can be read, releases a placeholder
dismissed while uncertain once it shows the session idle: the delivery worker
asks on every pass (a ready session update wakes it) and, while OpenCode
reports the session busy, again after a backoff of up to 10 seconds. A session
whose status cannot be read keeps the placeholder, and a manual send still
clears it. Other providers keep the rule unchanged. An archived `panel` thread (a closed chat panel) never promotes
automatically, and delivery neither selects nor claims its pending rows, so
neither its queue nor a prompt sent just before closing starts work after its
tab closed; unarchiving it resumes both. A delivery claimed before the panel
closed is refused at the provider supervisor before any driver I/O and returns
to pending, and accepting `thread.archive` for a panel interrupts its provider
session on the server, so closing a panel stops its work without relying on the
client's view of the session. Explicit Send now remains available under its client gate.

Pending start deliveries from older clients also wait while the session is
running or starting; the SQLite claim repeats this check so a stale worker read
cannot claim through it.

`ThreadTurnPromote` reconstructs the normal turn-start event from the stored
payload, clears that row's hold, and moves it to pending in the same transaction
as the start/delivery events and projections. It rechecks eligibility inside
that transaction. Server-originated promotions use the existing `server:`
command-id convention and require the automatic gate. Client **Send now** may
promote the head when the session is neither running nor starting and no
pending/sending row exists, including a held head. It clears only that hold.
The delivery service registers its existing `Arc<Notify>` with the engine;
after committed ready, steer, promote, resolve, unarchive, and relevant
request-resolution commands, the engine wakes the worker without retaining the
service itself.
The event's `createdAt` is the promote command's time. Its message projector
stamps the addressed user message's `created_at` and `updated_at` to that time
in the same transaction, so the promoted prompt appears after the preceding
turn's later reply. Replay and the client reducer apply that event time as well.
Still-queued rows retain their enqueue timestamps and durable receipt FIFO;
steering emits no turn-start event and preserves the message's enqueue time.

Interrupt requests and error/interrupted session updates atomically hold every
queued row and emit a complete delivery update per message. A later ready event preserves
those holds. A provider that has not settled the turn 10 seconds after Stop is
retired by the provider supervisor and projected `interrupted`, so a hung
provider cannot keep the session `running`
([providers](./providers.md#failure-attribution)); a Claude write without a replay
60 seconds after it was written settles as an uncertain delivery instead of
holding its delivery slot. Cancelling accepts only queued rows: the row becomes dismissed,
the delivery event carries `withdrawn: true`, and its projector deletes only
the addressed message. The row and original payload remain durable, while
same-command replay remains idempotent. Queued rows never enter crash
reconciliation or provider claiming; restart preserves their queued state and
payload until an eligible promotion or explicit user action. Before delivery
starts, startup reconciles abandoned live runtime rows and every projected
session still starting, connecting, or running without a live runtime row,
including sessions whose row graceful shutdown left `suspended` (the row keeps its
resume cursor). It settles the abandoned turn's
streaming assistant messages, clears the active turn, and projects the existing
restart error as `session_stopped`. That error settlement holds queued messages
for explicit **Send now** and releases the pending-start claim gate. Completed
reconciliation does not dispatch again on a later startup; ready/idle/stopped
projections without live runtimes retain their existing state.

A deleted thread (including every thread of a deleted project) can never accept
a session projection, so its work is terminal rather than retryable. Each
delivery-worker pass dismisses every `pending` row whose thread is deleted,
with a plain detail, before any claim, bootstrap, or provider launch; this also
settles rows stranded by an earlier retry loop. Other states are untouched.
Startup reconciliation skips a deleted thread and drops any runtime row a late
delivery left behind, so it does not warn and retry on every start.

Workspace-loss settlement uses the same error rule: every queued row is held,
and pending or sending steer rows latch the hold. The queued head shows
**Waiting for you**; **Send now** works once no running or starting session
exists and the workspace admits work again. Settlement wakes the delivery
worker to re-examine pending, non-queued starts. A refusal before provider routing
is definitely not sent and may retry with backoff. Loss after routing has begun
is uncertain: cancelling the route cannot undo a possible provider write. That
message stays behind the existing explicit uncertain-retry confirmation, even
after the workspace returns. While the workspace is
unavailable, admission refuses and delivery retries with backoff; after recovery,
the existing provider-loss relaunch starts a replacement session. A retry bound
to the stopped session's native identity recovers when its exact provider/instance
runtime is absent or has no resume cursor. A guarded update clears only that
sending start's native identity at its current attempt, retaining the command
receipt, FIFO position, payload, model/options and delivery key. It refuses changed
delivery ownership or newly restored/conflicting runtime identity; another native
session's cursor remains a rejection. The retry starts without resume, freezes to
the new native session, and immediate acceptance records `startedNewConversation`
with the delivered state. A retry that resumes its frozen session but finds the
provider no longer has it (the driver starts a different conversation) recovers
the same way inside `launch_session`: before the new cursor is saved, the saved
cursor is cleared and the same guarded update releases the start, so the retry is
delivered once in the new conversation instead of failing its identity check; a
crash between those steps leaves a cursorless runtime the next retry recovers
from. Reconciliation launches never release a frozen start. A restart
reconciliation whose launch finds the frozen conversation gone starts nothing:
it shuts the replacement down, keeps the saved cursor and the frozen row, and
settles the row uncertain because whether the lost conversation received it is
unknowable, so a later restart reaches the same answer. An explicit Retry then
releases it into a new conversation as above. The new conversation receives the thread's earlier
messages ([context handoff](./providers.md#resume-failures-and-context-handoff)),
and the user message shows "Sent in a new conversation with a summary of earlier
messages." in muted status text. A
still-resumable frozen session resumes normally without that notice. Automatic
reconciliation never unfreezes ambiguous work. Nonaccepted fresh attempts keep
their existing outcomes and no notice; the fact is not retained for a later
accepted attempt. Unfrozen fresh starts remain outside this recovery policy. The
settled turn retains its partial assistant text and ends as error.

`ThreadTurnSteer` validates the queued head, a running session with an active
turn, and the driver-owned capability shared with inventory. It atomically
moves the row to pending/steer, clears its hold, and emits
`thread.turn-steer-requested { turnId }` plus a complete nested delivery update.
It creates no turn-start event or pending turn and never changes session state.
The requested turn ID remains durable in that event; delivery rechecks it against
the active session instead of retargeting a newer turn.

The existing provider supervisor preserves frozen route/session identity and
per-thread serialization while dispatching `ProviderDriver::steer`. It does not
reconfigure or launch a replacement runtime for steering. Acceptance sets the
message's `turn_id` through delivery-updated `turnId`; it does not restore running
state if the turn settled while acknowledgement was in flight. A stale/settled
turn or definite rejection atomically returns the row to queued/start, resets
attempts, retains the reason in `delivery.detail`, and wakes normal promotion. Other outcomes retain
the existing retry/uncertain behavior. A target lookup failure after claim is
known not sent and returns to pending, so transient database failures cannot
strand a newly claimed steer in sending.

Interrupt, error and provider-reported interrupted updates also latch the hold
on pending/sending steer rows. Their delivery updates retain the actual state;
if steering is then rejected, the requeued message stays held even after ready.
Recovery applies each driver's existing crash truth table to steer rows. Codex
reconciles the same delivery key and uses the requested turn event for attribution
when acceptance was lost; Claude sending rows become uncertain.

### Assistant message identity

Provider assistant text preserves a native runtime `itemId` when the provider
exposes one. The server converts it to a thread-namespaced orchestration
message ID before persistence. Providers whose protocol does not expose a
message identity use one deterministic assistant message per thread turn.

Terminal turn projection completes every existing streaming assistant message
for that thread and turn and never creates an empty assistant message. The
client therefore receives the same message boundaries from live events and
reloaded SQLite projections; Markdown rendering does not infer or repair
provider message boundaries. A live completion that no longer owns a matching
streaming assistant row is accepted idempotently without appending an event, so
a projector rewind cannot reinterpret that no-op with historical upsert
semantics. Genuine historical message events retain their established replay
behavior.

The turn's final assistant pointer follows durable message chronology, ordered
by message creation time and then message ID. A delayed completion for an older
assistant item therefore cannot replace a later answer, including when provider
events share a timestamp. Startup reconciliation and an unexpected provider
event-stream end settle the exact abandoned turn's existing assistant rows;
they retain provider failure and session error state without inserting fallback
text. This terminal settlement performs one thread-scoped read and no per-delta
database work. If both live settlement attempts fail after an unexpected stream
end, the durable error runtime retains thread-scoped recovery ownership: the
next startup settles the exact stored message and turn identities without
rewriting the original provider or session error.

### Context-window usage flow

Provider-native usage data is normalized in the server runtime as canonical
`thread.token-usage.updated`. `ProviderRuntimeSupervisor` maps that canonical
event to an informational `context-window.updated` thread activity, which the
`OrchestrationEngine` appends through the same durable event and typed
subscription path as other provider activity.

The append-only event log preserves every accepted context activity for audit
and replay. Durable projections and client snapshots retain only the latest
valid context-window activity for each turn, so a newer valid reading replaces
the prior valid reading for that turn. A malformed row cannot evict a valid row,
and reverting a turn removes only that turn's projected usage; neither behavior
creates a separate usage cache.

### Checkpoint revert

`thread.checkpoint.revert` is accepted at once; the orchestration effect does
the work. A provider cannot undo a rewind, so the effect orders the steps to
keep a failure from leaving the files and the conversation out of step:

1. It checks that the workspace is a Git repository and that the target
   checkpoint ref (or `HEAD`, for turn 0) resolves.
2. It asks the provider to drop the newer turns. If the session was suspended
   while idle, the supervisor first reopens it with the launch a new message
   would use: the thread's provider, modes and saved resume cursor. One case
   differs: a conversation the provider no longer has fails the revert instead
   of starting a new conversation, because a new one has nothing to rewind.
   For the same reason, a thread with no saved conversation for its provider
   fails without starting a session.
   After the rewind, the session idles again under the normal deadline. A
   provider that refuses, or a session that cannot be reopened, fails the
   revert here, and nothing changes.
3. It restores the files from the checkpoint ref, deletes the newer checkpoint
   refs, and dispatches `thread.revert.complete`.

Only Codex and OpenCode can rewind a conversation. This is the driver-owned
`supportsConversationRewind` capability (see
[Providers](./providers.md)), so the supervisor never asks Claude, Cursor or
Grok to rewind. For them, step 2 reports `FilesOnly`. The effect then restores
the files and keeps the thread's messages, checkpoints and turn count, because
the provider keeps its conversation. It adds an informational
`checkpoint.revert.files-only` activity that says the conversation was not
rewound.

The effect also refuses a revert while the thread's session holds a turn,
whichever client asked for it: it is `running` or `starting`, or it still names
an active turn. This is the same test idle suspension uses for a busy session.
A running turn would write over the restored files, or the rewind would cut it
off. The supervisor likewise refuses, before it rewinds or reports files only,
while the live session still runs background work that keeps idle suspension
away: a Claude background task, or a subagent or work item Activity shows
running, reported within the last 30 minutes. The session reads idle then, but
its process would write over the restored files. Work BiBCode cannot see does
not block a revert.

A revert and the thread's turn deliveries exclude each other. The revert
reserves the thread on `OrchestrationEngine` before its busy check and releases
it once the revert is recorded or has failed; a delivery holds the same
reservation from just before its claim until the provider has the turn. A
revert therefore waits for a delivery already in flight, and then usually
refuses its running turn. No turn starts from its busy check until it ends,
whichever client sent the message, and also when a session the revert reopened
reads `ready`. Delivery skips the reserved thread instead of claiming its
messages or promoting its queue automatically, so a message sent or queued
meanwhile keeps its place and is delivered after the revert, in queue order;
the release wakes delivery. A client's **Send now** still moves the queue head
to pending, where it waits the same way.
The slow part of a revert does not hold the effects worker, which serves every
thread. The worker parks the reverting thread, and a waiter does the
preparation off the worker: it takes the reservation, which can wait for a
delivery in flight, then runs step 1 and the busy check, which only read the
checkout. If the provider can rewind, it then reopens a session that was
suspended while idle, which can be a cold start. Meanwhile the thread's later
effects, such as a turn baseline, queue behind the revert in event order, and
other threads' checkpoint work goes on. Once the preparation is done, the
worker runs the rewind and the restore, then the queued effects. Every effect
that writes still runs on the worker one at a time, so checkpoint work on a
checkout that sibling threads share never overlaps. A parked thread holds at
most 64 events, and at most four threads are parked at once; past either bound
the worker waits for the revert itself, as it did before parking.
The reservation is in memory only. That is enough because a revert never
outlives its process: the effects worker does not replay a revert requested
before a restart.

Once the provider has rewound, the revert must be recorded, or a retry would
rewind the provider again. Shutdown therefore waits for step 3 instead of
interrupting it, and a restore that fails is still recorded: the work log
then says the conversation was rewound but the files were not restored. Every
failure is a `checkpoint.revert.failed` activity.

## Targeted Activity cancellation flow

Targeted Activity control uses typed WebSocket RPC in browser and desktop
modes. It does not cross `DesktopBridge`. Reads and `subscribeActivity` require
`orchestration:read`; `activity.cancelSubtree` and
`activity.retrySubtreeCancellation` are maintenance-classified mutations and
require `orchestration:operate`. Both mutations reject terminal Activity scopes
before provider I/O.

```mermaid
sequenceDiagram
  participant UI as Activity panel
  participant RPC as Activity RPC
  participant Cancel as Cancellation service
  participant Provider as Current provider runtime
  participant Stream as Activity subscription

  UI->>RPC: cancelSubtree(scope, scopeId, actorId, controlRevision)
  RPC->>Cancel: authorize and admit canonical subtree
  Cancel-->>Stream: requested overlay / Stopping
  Cancel->>Provider: exact server-held targets, selected actor first
  RPC-->>UI: accepted, inProgress, or alreadyTerminal
  Provider-->>Stream: authoritative lifecycle events
  Cancel-->>Stream: operation removed or partial residual summary
  UI->>RPC: retrySubtreeCancellation(rootActorId, operationRevision)
  RPC->>Cancel: residuals plus late descendants under original fence only
```

The client supplies canonical scope and actor identities plus concurrency
revisions; it never supplies descendants or provider-native thread, turn, task,
process, or agent identifiers. Admission installs the cancellation fence before
provider dispatch. The selected actor is sent first, descendants use bounded
parallelism, each native attempt has a two-second timeout, and one operation has
a lifecycle-owned ten-second deadline. The deadline finalizes any still-active
residual as `partial` even after dispatch draining has ended; it is fenced by
runtime generation, operation ownership, and a private deadline identity and
cannot terminalize provider observation. Coverage, residual, and public
operation-revision reconciliation leaves that deadline identity unchanged;
retry and absorption create a fresh deadline window. Duplicate and overlapping
requests join or absorb the existing operation without broadening the canonical
boundary.

Observation history and its revision persist in SQLite. Exact handles,
cancellation fences, operation summaries, residuals, and the independently
monotonic control revision are bounded runtime state only. Reconnect can recover
the current server's control snapshot; restart or provider-generation
replacement invalidates it. `Stopping` is server-authoritative intent, while
provider events remain the sole authority for terminal lifecycle. A partial
retry is fenced by its operation revision and cannot recompute parents,
siblings, or unrelated work. Public operation revisions are allocated from one
checked registry-lifetime monotonic high-water counter, so a stable scope/root
pair cannot replay an old retry revision after replacement or bounded scope
eviction; exhaustion fails closed before provider I/O. Runtime cleanup retains
only bounded target-free scope/actor revision tombstones so a stable public
scope cannot reuse a stale pre-restart control fence; no operation or
provider-native identity survives.

## Workspace entry index and refresh

`projects.listEntries` and `projects.searchEntries` are served from one
server-owned entry index per canonical workspace root, built on first use and
retained for later requests. Concurrent cold callers share one build, and an
invalidation generation prevents an older in-flight scan from becoming current.

A Git build starts one concurrent wave of two reads with `GIT_OPTIONAL_LOCKS=0`,
request cancellation, and a workspace-index-specific ten-second post-spawn
execution bound:

```text
git -c core.quotePath=false ls-files -z -t --cached --others --deleted --exclude-standard --
git -c core.quotePath=false ls-files -z --others --ignored --exclude-standard --directory --
```

The tagged listing distinguishes cached, deleted, and ordinary untracked paths;
deleted paths are excluded from the tree. The second listing supplies ignored
files and directory roots. Ignored roots and every descendant are still loaded
eagerly. A separate directory walk adds empty directories because `git
ls-files` reports only files. If either Git read cannot start, times out,
truncates, exits unsuccessfully, or returns malformed classification data, the
complete Git snapshot is rejected and that request uses the existing bounded
filesystem fallback. Non-Git workspaces use the same fallback. Both paths are
bounded by entry count, total memory, and path length; exceeding a bound returns
the entries accepted so far with `truncated: true`.

Lazy ignored-directory loading remains a separately reviewed follow-up. It is
required only if a representative native measurement finds ignored walking
above 500 ms at p95 or above 50% of p95 cache-build time; until then the eager
tree is the supported behavior.

The index is the server's cached view of the tree. A cached root is rebuilt when
something invalidates it:

- `projects.createEntry`, `projects.renameEntry`, `projects.deleteEntry`, and
  `projects.duplicateEntry` drop the cached index after a successful mutation;
- `projects.writeFile` drops it when the write creates a file or parent, or when
  either the logical path or safely resolved effective target contains a
  case-insensitive `.gitignore` or `.git` component. An ordinary existing-file
  content save keeps the index while still notifying VCS status. Arbitrary
  custom `core.excludesFile` paths are not retained as cache provenance, so
  changing one requires explicit `refresh: true`;
- `subscribeProjectEntries` streams a signal whenever a periodic sweep observes
  that the path set under `cwd` changed outside the application. The sweep stats
  the directories the last scan saw rather than walking the tree, because
  adding, renaming, or removing an entry moves its parent directory's mtime
  while an in-place content edit does not. The index keeps that directory set
  apart from its entry list, because past the entry limit the list keeps files
  and drops directories, and a sweep that took its directories from the list
  went blind to every nested change. Every listed directory is swept. A
  directory the list dropped is swept within the entry count limit and the
  memory the list leaves, so the index stays within its one memory bound. A truncated
  index therefore pays a rebuild for each burst of changes it sees, even when
  the change falls outside the listed entries. The first subscriber for a root
  starts the sweep and the last one to leave stops it. The sweep runs every two
  seconds and reports a burst of changes once, at the first sweep that sees
  nothing new; while changes keep arriving it reports about every ten seconds.
  A report drops the cached index and rebuilds it before deciding whether to
  signal, so a subscriber that re-lists on the signal sees the new state. It
  stays silent only when the rebuilt entries (paths, kinds, ignored flags, and
  truncation) match the list it last announced and nothing else (a mutation or
  an explicit refresh) rebuilt the index since: an atomic save (write a
  temporary file, rename it over the target) moves its directory but lists the
  same entries, and every signal costs each subscriber a full re-read. A failed
  rebuild is signalled. The rebuild holds a path admission and cancels its scan
  when the workspace is lost, the last subscriber leaves, or the server shuts
  down, so it holds up none of them. The event carries only `cwd` — the index
  stays the single source of truth for entry data, so subscribers re-list rather
  than apply a diff, which also keeps the payload independent of how many files
  changed. Subscribing resyncs the index and then signals, so a list read
  before that first signal could predate the resync. The web client therefore
  subscribes and reads the list once, after that signal (or a failed
  subscription), while a thread is open, so opening Files paints at once with
  no read of its own. A later signal while Files is closed only marks the list
  stale; the open panel re-reads on each signal, including one it missed while
  closed, after painting the stale list;
- `projects.listEntries` accepts an optional boolean `refresh`. Omitting it, or
  sending `false`, serves the retained index and preserves compatibility for
  older clients. `refresh: true` drops the cached index before listing, so the
  result includes files and directories created or removed outside the
  application.

A client that changes the workspace only through this RPC surface therefore
normally needs no `refresh`; the explicit exceptions are an immediate rescan of
external changes and a content change to a configured custom
`core.excludesFile`. The flag changes cache admission only. It does not widen
the path boundary, bypass the workspace-availability guard, or otherwise change
what the caller may read.

Moving an entry is a rename. The file tree's drag-and-drop move sends
`projects.renameEntry` with the source and destination as workspace-relative
paths under the request's workspace root; there is no separate move method.
Every method on this surface holds a path admission lease for the whole
operation, and the mutations additionally cross the finalization fence described
in [Missing-workspace runtime guard](#missing-workspace-runtime-guard).

## Ordered terminal input

Current servers advertise `terminalOrderedInput`. The client negotiates from
the actual RPC session's initial configuration; an omitted/false capability
keeps the serialized `terminal.write` path. A failure on the ordered path does
not retry through the legacy method.

| Method                 | Input                                                                 | Successful result                                          |
| ---------------------- | --------------------------------------------------------------------- | ---------------------------------------------------------- |
| `terminal.beginInput`  | Thread, terminal, and a monotonically increasing `attachmentSequence` | An opaque `inputId`                                        |
| `terminal.writeInput`  | Thread, terminal, `inputId`, zero-based frame `sequence`, and `data`  | The matching `inputId` and `sequence`, after the PTY write |
| `terminal.cancelInput` | Thread, terminal, and `inputId`                                       | Null; unknown/already removed leases are harmless          |

All three use `terminal:operate`. The server binds each lease to the physical
RPC connection and captured terminal process generation. Reconnecting with the
same bearer still creates a different connection identity. A newer attachment
supersedes the previous lease; a delayed older begin cannot supersede the newer
attachment. Cancellation and terminal generation changes invalidate leases.

`terminal.beginInput` is the input session's only workspace resolution: it
reads the thread's persisted workspace (its worktree path, or its project root)
and takes the [workspace admission](#missing-workspace-runtime-guard) for the
thread and that path, and the input lease holds that admission until the lease
ends. Frames are therefore admitted in memory and a keystroke never waits for
the database. Workspace loss or removal cancels the admission, which seals the
lease like a closed connection: a frame in flight fails with
`WorkspaceUnavailableError`, later frames fail with `TerminalInputError`, and
the admission is released so the loss or removal does not wait for it. A new
`terminal.beginInput` then fails with `WorkspaceUnavailableError`.

A changed workspace re-admits the session in place: frames keep their
`inputId` and order and the user sees nothing. The production runtime feeds
orchestration events to `ServerTerminalServices::watch_workspace_changes`,
which re-resolves and replaces the admission of the thread's leases on
`thread.created` and on `thread.meta-updated` carrying `worktreePath`. On
`project.meta-updated` carrying `workspaceRoot` it re-admits only the leases
admitted for that project's root, that is, of its threads without a worktree of
their own; each admission records that origin when it resolves, so the filter
reads no database. A session whose workspace is still being re-resolved is
included, since its origin is not known yet. Other sessions, of other projects
and of threads with their own worktree, are untouched. A lagged subscription re-admits every lease, since the
missed changes are unknown. While a round resolves, the session's frames wait for its
result, so none writes under an admission the round may replace or refuse; a
frame that already holds its turn when a round starts writes under the
admission current at that moment, as a frame admitted just before the change
did under per-frame admission. Sessions resolve independently, so one slow
workspace holds only its own frames, with at most one resolution in flight per
session: changes that arrive meanwhile coalesce into one more resolution at the
latest round. A result older than one already applied, or than the latest round
started for the session, is ignored, and the replaced admission is released. Only the loss of the session's
current admission seals it, atomically with re-admission, and a frame checks
for that loss itself before and while it writes. When the new workspace cannot
be admitted (unavailable, or its
resolution fails), the lease is sealed and the reconnecting
`terminal.beginInput` reports `WorkspaceUnavailableError`. A begin reads the
re-admission epoch before it resolves the workspace and again after its lease
registers; if a round started in between, the begin re-admits its own lease
in a round of its own before it returns, so later rounds see it pending, and
fails with the admission error instead of returning a sealed lease, so a
change committed during that read is not missed.

The Rust terminal owner reserves each frame from a shared per-connection window
of 16 frames and 256 KiB before the frame waits for its turn. Each frame is at
most 16 KiB of UTF-8. Reordered arrivals wait for the missing sequence without
writing a suffix early. A five-second gap deadline, duplicate/invalid sequence,
overflow, dropped admitted work, or I/O failure seals the lease and wakes all
waiters. PTY delivery uses the existing generation publication guard and exact
process identity. A write already executing when cancellation arrives can have
completed; it is never replayed after a lost acknowledgement.

The shared client runtime counts unary and persistent stream requests against
the physical connection's 64-request limit. Input begins/writes stop admission
at 56 occupied slots, leaving eight for other operations; input also shares the
16-frame/256-KiB window. Its aggregate waiting dispatch payload is capped at
1 MiB. Each terminal's renderer scheduler and negotiated binding separately cap
pending data at 1 MiB, so the renderer limit is per terminal. Frames split at
Unicode scalar boundaries. Reset cancels the captured old lease asynchronously
without deleting or cancelling a newer binding, and stale replies cannot
revive failed input. Lease preparation starts after confirmed attachment to
avoid an additional setup round trip on the first keystroke. Successful open
and restart commands also prepare the confirmed terminal on the same captured
session before returning, so immediate programmatic writes work before the
renderer arrives. Idempotent open preserves an already-live binding.

The renderer retains its input binding across visual hide/reparent operations.
Process/attachment changes reset it. On delivery failure it pauses input and
offers **Reconnect input**, which refreshes the attachment to the same running
process and never replays discarded text. This removes acknowledgement queuing
during ordinary typing; remote echo still includes network round-trip latency.

## Terminal size and reply ownership

`terminalSizeOwnership` is negotiated from the environment capability set and is
decoded as false when absent. The terminal manager owns PTY dimensions and one
optional attachment owner; renderers mirror that applied state.

| Protocol surface        | Additions and behavior                                                                                                                                                    |
| ----------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `terminal.attach` input | Optional `sizeClaim`, a fresh random token for each renderer mount; optional fitted `cols`/`rows` initialize a missing PTY and never resize an existing one.              |
| `terminal.resize` input | Optional `sizeClaim`; a registered claim transfers ownership, including at equal dimensions. Omitting it clears ownership for legacy callers.                             |
| Attach snapshot         | Optional `size { cols, rows, sizeClaim }`; the whole object may be absent for older servers and `sizeClaim: null` means an unowned current size.                          |
| Attach snapshot         | `oscColorResponderActive`, default false when absent, reports whether the actual spawned PTY color responder is active.                                                   |
| Attach snapshot         | `firstAttachmentGrant`, default false when absent, grants one startup-history reply pass only before any claim, or a claimed stream surviving a restart, has consumed it. |
| Attach event            | `resized { threadId, terminalId, size }`, only on streams opened with `sizeClaim`; never on `subscribeTerminalEvents` or operational-log records.                         |

First-attachment grant selection and ownership assignment are atomic under the
PTY generation publication lock. Attach assigns an owner only when unowned; it
grants startup-history replies only when no claim has been applied and no
claim-carrying stream survived a restart into this generation. A claimed attach
or applied resize claim consumes the latch. Surviving claimed streams consume
it only on the restart path: `Restarted` resets their sequence cursors, so they
answer the restarted process's startup queries live as owner or under the
null-claim rule. Restart without those streams permits one new grant.

A non-restart `terminal.open` on an exited session publishes `Started` at
sequence 1. Surviving streams do not reset their cursors for that event and drop
it and early output, so those registrations do not consume the new generation's
first-attachment grant. The next claim-carrying attach can receive it once.
Dropping that `Started` event is a known limitation of surviving streams. Later
and concurrent
attaches neither renew a consumed grant nor displace a live owner. Claim
eligibility ends with the exact attachment lifetime; dropping the owner
publishes a null owner without changing dimensions, and stale resize requests
cannot reclaim it. Manager shutdown cancels and drains attachment cleanup
workers.

Accepted resize/claim changes publish before subsequent PTY output under the
same lock; failed resizes preserve state and exact no-ops emit nothing. A
renderer retains one pending claim until RPC completion, on success or failure.
The ordered echo updates applied dimensions and reply ownership. Notice
suppression persists through successful RPC completion until applied size,
ownership, or generation changes, including when success emits no echo. Input
and reply decisions do not treat a pending claim as applied ownership. Null
ownership permits live replies while attached renderers immediately try to claim
it; mirrors suppress device/status/palette query replies. OSC 10/11/12 are
suppressed for all renderers when the server responder is active; otherwise the
owner can answer. The server continues handling `CSI ? 996 n`.

The transcript runtime delivers the first-attachment grant once to the renderer
with the matching claim. Its history replay may answer startup DSR/DA; ordinary
replay drops xterm `onData` through the final parse callback, including cached
remounts. Input before the first snapshot uses the existing bounded scheduler
and negotiated lease. No reply-shaped input filtering is used.

UI creation uses `terminal.open`, retaining its workspace admission lease and
workspace-loss fence. An attach carrying `cwd` can still create a missing
session without that lease/fence, for example when it arrives before open or
recreates a terminal after a server restart (a known gap). Open and attach send fitted dimensions only when a mounted renderer can
supply them; new right-panel reservations have none and use server defaults.

Against a server without `terminalSizeOwnership`, every replay is guarded;
startup-history queries are not answered on that transitional path. Live replies
and legacy fit-and-resize behavior remain available. See [terminal attachment
fidelity](overview.md#terminal-attachment-fidelity) for interaction and
dismissal behavior, and the [validation
runbook](../testing/cross-platform-validation.md#terminal-size-and-reply-ownership)
for replay, multi-window, and legacy-client checks.

## Provider usage refresh

`server.getProviderUsage` reads the server's current provider-usage snapshots.
The status bar targets the environment selected in the rail for this query and
for refresh or usage-reset commands. Switching environments changes the query
and interaction state; a late command result remains in its originating
environment's cache.
`server.refreshProviderUsage` accepts an optional provider list and an optional
boolean `force`. Omitting `force`, or sending `false`, uses the normal refresh
throttle; `force: true` starts an explicit fetch even inside that interval.
The default preserves compatibility for older clients.

Forced refresh changes admission only. It does not authorize credential
mutation or account management: provider usage fetchers remain observers of
the local provider CLI's account. The client waits for the refresh command to
settle before refreshing the query, so a committed snapshot is not displayed
one cycle late. Background status-bar polling is single-flighted separately
from a forced manual request; repeated manual activation still shares one
manual request per environment.

## Worktree removal flow

`worktree.remove` refuses with `WorktreeRemovalError { reason: "session-running" }`
and "Stop the running session before deleting this worktree." when any session
it would stop has projected status `running` or `starting` **and** the provider
supervisor holds a live session for that thread. The check asks the supervisor
only about threads with those projected statuses; a liveness lookup failure
also refuses removal with `session-running`. A stale projection without a live
session no longer blocks deletion. This includes workspace-loss cleanup, whose
`stop_session` does not settle the projected status. After resolving
workspace ownership, an early check verifies the known checkout threads before
marking the workspace `Removing`. Prepared retries skip this early check so
their verified post-Git state can be resolved.

After plan validation and Git preflight, the authoritative check waits up to
five seconds for existing workspace admissions to drain while `Removing`
refuses new admissions. A drain timeout returns the same typed refusal. The
check resolves the quiescer's full thread set, including other projects sharing
the repository, and checks their projected statuses and provider liveness before
durable removal preparation, quiesce, or Git mutation. Refusal drops the removal guard and
restores availability without stopping sessions or deleting the checkout.
Cancellation is checked before quiesce and again afterward, before the trusted
repository anchor is re-resolved and Git mutation starts.

The authoritative check is bypassed only for a prepared retry in
`delete-git-worktree` mode whose target is verified `missing-unregistered`:
Git already succeeded, and durable detach must finish even with a running
session row. Accepted receipts replay before either check. The foreground
quiesce and its reaper retries suspend a current provider session only when its
projection is neither `running` nor `starting`, has no active turn, and the
supervisor has no active delivery generation. Suspension shuts down the driver
and retains its resume cursor in a `suspended` runtime row. Other current
sessions are stopped; `worktree.removeFromBibCode` retains its detach-only
behavior, including stopping running sessions. Pending, queued, and uncertain
deliveries alone do not cause the session refusal.

Both removal RPCs tombstone the owner and its panels through the shared
`thread.deleted` transaction, which also deletes each deleted thread's provider
runtime row regardless of status. Other threads retain their rows. Suspension
uses an atomic conditional upsert that refuses to recreate or update runtime
state after thread deletion, so a late reaper attempt cannot leave an orphan.

If removal fails or is cancelled after quiesce, idle provider conversations
remain resumable on the next send. Terminals stay closed with their history
kept and can be restarted when the checkout is available. A partially completed
filesystem removal cannot be undone; a prepared retry completes durable detach
after verified Git removal.

`vcs.removeWorktree` is a server-held terminal, persistence, and filesystem
critical section. `ownerThreadId` identifies the workspace owner, and the
production server verifies its persisted project and worktree path. `threadIds`
must include that owner and helps invalidate known generations, while the authoritative fence
is also keyed by lexical and canonical target paths (including Windows junction
aliases) and discovers every server-known terminal under it. The server installs
the fence before closing terminals, invalidates
an open already in flight, rejects later open, attach, and restart operations,
and retains the fence until Git removal and its durable receipt settle. Once
admitted, that owned task finishes even if its initiating RPC is interrupted or
disconnected: it retains a clone of the RPC mutation permit, is tracked by the
production runtime, and is drained before maintenance or shutdown can complete.
A failed operation drops the fence so an explicit retry can launch terminals
again.

The server writes a random identity durably into the linked worktree's Git
administrative directory and root, then persists a
`worktree_removal_receipts` row keyed by the owner thread. The repository
revalidates that identity and the exact linked-worktree `.git` backlink inside
the removal transaction. On Windows it atomically moves the admitted directory
through its delete-capable filesystem handle, rather than resolving the source
name a second time, to a
deterministic nonce-bound quarantine and creates a minimal identity-checked
tombstone at the registered path. On Windows tombstone creation atomically
returns the same delete-capable directory handle used by the transaction; there
is no create-then-reopen gap, and its no-delete parent handle remains held while
the marker and Git backlink are initialized. The checkout quarantine,
tombstone, and administrative quarantine handles remain owned continuously
through deregistration and final deletion; path absence after admission is
never accepted as cleanup success. While a no-delete filesystem handle pins that
tombstone, the server atomically moves the nonce-verified administrative
directory out of Git's `worktrees` namespace; it never issues a path-based Git
delete that could target a replacement. Recursive cleanup pins each verified
directory object against name rebinding, verifies every Windows descendant by
file identity before deleting it, never follows reparse points such as
junctions at either the root or descendant level, and deletes the now-empty root
through its bound handle. An empty
registered path without the transaction marker fails closed instead of being
treated as a recoverable tombstone. Durable marker publication still uses a
write-through rename. A terminal or external process that still owns the
checkout as its current directory makes quarantine admission fail without
deleting checkout contents or changing Git registration. Interrupted attempts
recover the deterministic quarantine rather than treating path absence as
success. The browser still performs thread deletion after the VCS result. After
cleanup succeeds, the receipt moves to `removed`; a later retry after a
thread-deletion failure returns success without inspecting or deleting a
replacement that now occupies the old path.

## Failure diagnostics and elapsed timing

The configured local `TraceDiagnosticsStore` records failed unary handlers and
terminal domain failures from stacked Git actions. Unary registration owns its
handler timer. The stacked-action owner records the failure before converting
it to an `action_failed` progress chunk; generic stream transport does not infer
domain errors from arbitrary message bodies. Both owners reuse the registry's
configured recorder and preserve the original result if diagnostic writing fails.

Process errors provide private closed failure facts to source-control and Git
Manager error mapping. User-facing launch messages may contain a known tool
label and numeric OS error, but never raw error sources, command arguments or
process output. A not-found launch category does not assert that the executable
is absent rather than a required directory or interpreter.

Native trace records and responses add optional `durationMeasured` on occurrences
and recent failures, and `measuredCount` on span summaries. Existing finite numeric
duration fields remain compatible with older clients. New failures measure
server-handler/action elapsed time with the monotonic clock before logging;
untimed events are explicitly unmeasured. Unmarked legacy zero durations are
unavailable, while positive legacy durations remain measured. Unknown samples
retain their failure/count evidence but do not contribute to latency statistics
or slow rankings. An older server's summary lacks a known measured denominator,
so current clients display its summary timing as unavailable.

## Profiler timing and client reports

The [Profiler](./overview.md#profiler) adds two unary methods, whose schemas are
in [`profiler.ts`](../../packages/contracts/src/profiler.ts). A server advertises
them with the environment descriptor's `capabilities.profiler`, which decodes as
`false` for an older server. Clients send neither method, poll no status, and
start no collector for an environment without it, because an older server
answers an unknown method with a connection-level defect: it fails every request
in flight on that connection and silences its config stream while the socket
stays open. An older server also stores and echoes back an `enableProfiler: true`
it never acts on, so clients ignore that setting without the capability. A
client that still receives `Unknown request tag: server.getProfilerStatus` or
`server.reportProfilerClientBatch` treats it as a permanent refusal for that
environment.

| RPC                                | Scope                   | Purpose                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| ---------------------------------- | ----------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `server.getProfilerStatus`         | `orchestration:read`    | `ProfilerStatus`: `enabled`, `recordingSince`, `diskBytes`, `eventCount`, `droppedEvents`, `lastCrashAt`, `capabilities` (`nativeDumps`, `osCrashRecords`, and `storage`: `ok`, `failing`, or `locked` when another process records to the folder), and the 20 newest findings without evidence. It reads the profiler directory on a blocking thread.                                                                                                                                                                                                |
| `server.reportProfilerClientBatch` | `orchestration:operate` | One client batch. It answers `{ accepted: true }` once recorded, `{ accepted: false }` while the profiler is off, and `{ accepted: false, reason: "too-frequent" }`, with nothing written, for a batch within 5 s of the same `clientId`'s previous one (the collector sends every 10 s; the last 256 client ids are remembered). A batch over 64 KiB or with more than 64 records or histograms fails with `ProfilerClientBatchTooLarge`, an invalid field with `ProfilerClientBatchInvalid`, before the toggle is checked and with nothing written. |

### Server timing

While the profiler is on, the unary `register` wrapper in `rpc/session.rs` times
every handler from its start, and a stream is timed to its first item with the
outcome `first-item`. With the profiler off the timer reads, inserts, and
allocates nothing. Each `(method, outcome)` has a window of six 5-minute slots on
the monotonic clock, each holding 100 log-scale buckets that grow by a factor of
1.12. The five finished slots are the baseline; the current one is excluded.

- **Absolute tiers raise findings.** A unary request that takes at least 1 s
  raises an `rpc.slow` finding of severity warn, and one of at least 10 s an
  error. The detector raises them on its 5 s tick, one per `(method, outcome)`,
  so a finding lags its request by up to 5 s.
- **The relative tier is log-only.** After 100 baseline samples, a request over
  max(3 × p95, floor) is written as an `rpc.timing` event with
  `tier: "relative"` and never becomes a finding. The floor is 250 ms, or 1 s
  for background methods (`git.`, `vcs.`, `worktree.`, `pullRequests.`, and
  `server.refresh` prefixes, and `projects.searchEntries`). It is promoted to
  findings only once each method has a baseline from real sessions.
- Every request over a tier writes an `rpc.timing` event with its method,
  outcome, duration, tier, baseline p95, sample count, and correlation ID.
- **Long-running methods** wait on networks, hosting services, provider CLIs,
  model calls, or setup hooks. The list is `is_long_running` in
  [`latency.rs`](../../apps/server/src/diagnostics/profiler/latency.rs): repository
  clone, publish, lookup, and discovery (`vcs.clone`,
  `sourceControl.cloneRepository`, `publishRepository`, and `lookupRepository`,
  and `server.discoverSourceControl`); `vcs.pull` and `vcs.generateCommitMessage`;
  `gitManager.commit`, `gitManager.getRemoteTags`, and
  `gitManager.listPullRequests`; `git.resolvePullRequest` and
  `git.preparePullRequestThread`; the pull request reads and actions
  `pullRequests.checkout`, `runAction`, `list`, `get`, `getTimeline`,
  `getCommits`, `getChecks`, `getFiles`, `getContext`, `getVocabulary`, and
  `getCreateDefaults`; `updater.check` and `updater.install`;
  `server.updateProvider`, `server.refreshProviders`, and
  `server.refreshProviderUsage`; `worktree.createManaged` and `worktree.remove`
  (which deletes the whole tree, dependencies and build output included); and
  `agentSessions.import`. They still emit `rpc.timing`, but never raise
  `rpc.slow` or count toward saturation.
- **Hangs.** Each 5 s tick checks the unary requests still in flight. One past
  its budget, 10 s or 300 s for a long-running method, raises one `rpc.hang`
  finding (error) before it finishes. Streams are log-only and never hang-checked,
  because an event stream may wait any time for its first item.
- **Saturation.** When five or more distinct methods are slow in the same tick,
  one `rpc.saturation` finding replaces their `rpc.slow` findings.
- **Histograms.** Each finished slot is written as one `rpc.histogram` event per
  `(method, outcome)`.
- **Fences.** A request in flight across a profiler stop and start is not
  measured. On Windows the monotonic clock counts suspended time, so a detector
  tick more than 30 s late means the machine slept, and the requests spanning it
  are discarded. Elsewhere the clock stops during a suspend, so a late tick is a
  stall that the hang check reports.

### Correlation join

The wire request ID is not visible to the client's observer, and one trace spans
several RPCs, so neither can match a client measurement to the server's. While
the profiler is on for an environment, the client therefore sends a per-request
correlation ID, `<clientId>:<counter>`, in the `x-bibcode-profiler-rid` RPC
header. The server keeps the last 4,096 `(correlationId, method, serverMs,
queuedMs)` entries, ignoring IDs over 64 bytes. A client record joins only an
entry with its correlation ID and the same method; there is no time-based
fallback. An unjoined record has `joinedBy: "none"` and `slowPart: "unknown"`.

A joined record splits the client's wait into `serverMs`, `networkMs = rttMs −
serverMs`, and `renderMs`, and the largest part names the slow part: `server`,
`network`, or `render`. The session loop has no frame-receipt instant yet, so
queue time is not measured: `queuedMs` is `null` in events and 0 in the join
log, and server queueing counts as network. Only durations cross the wire,
never timestamps.

### Client collector

[`clientCollector.ts`](../../apps/web/src/profiler/clientCollector.ts) observes
every RPC through the environment request observer. It measures only while at
least one connected environment has `capabilities.profiler` and its
`enableProfiler` on, and then records:

- the round trip of each request, and its render time, from the response to the
  second animation frame after it;
- main-thread stalls, from Long Animation Frames where the engine supports them
  (WebView2, Chromium), and elsewhere from a 500 ms heartbeat that counts more
  than 50 ms of lateness as a stall and pauses while the page is hidden;
- slow interactions from Event Timing, over 104 ms, where supported; and
- the JavaScript heap where `performance.memory` exists (Chromium).

Every 10 s it sends each profiled environment at most one batch, with one
upload in flight per environment. A batch carries per-method histograms in the
server's buckets, individual records for requests over 250 ms, stall and
interaction totals, the heap size, and the capabilities it detected. Request
data goes only to the environment that served the request; stalls,
interactions, and heap go to every profiled environment. A sample that spans a
visibility change or a sleep is dropped. A sleep shows as the wall clock
running more than 5 s ahead of the monotonic clock; with Long Animation Frames,
whose engines count sleep in the monotonic clock, a heartbeat over 60 s late
counts too. An authorization refusal, or an older server's unknown-method
defect, stops reporting to that environment for the session; every other failure
is swallowed, so profiling never surfaces an error. The batch RPC is excluded from the slow-request acknowledgement
indicator.

The server drops records and histograms of methods it does not serve. Each
record becomes a `client.request` event, and each batch a `client.batch` event,
under the source `client:<clientId>`. Findings from a batch use the source
`client`:

- `rpc.slow` when a record's round trip plus render crosses an absolute tier,
  unless its method is long-running, with the slow part in the fingerprint;
- `client.stall` (warn) when stalls add up to more than 1 s in the batch; and
- `client.interaction` (warn) when the slowest interaction took over 500 ms.

[Remote environments](./remote.md#profiler-data) describes where reports go when
a client is connected to several environments.

### Connection ends

When a session ends, plain or E2EE, the session loop classifies why from its own
side: a client close frame (`client-liveness-timeout` for code 4408,
`client-closed` otherwise), a socket that ended without one (`client-gone`), a
client `Eof` (`client-eof`), the heartbeat (`heartbeat-timeout`), a writer
failure (`writer-stalled`, `writer-closed`, `writer-failed`), or the server
(`server`). `ConnectionLiveness` keeps the connection's start, its last inbound
frame, the end of its last outbound RPC message (heartbeat Pings excluded), and
the longest wait of a queued data message before its write began. Every
abnormal end writes one info log line, `RPC connection ended`, with those
durations, the queue depth, the requests in flight and the client id; a
1000 or 1001 close, an `Eof`, and a server-side end log at debug level.

While the profiler is on, the same end is an `rpc.disconnect` event. The
session also notes the client id of its first request that carries the
correlation header; when that client's previous connection ended in this
process, it writes `rpc.reconnect` with the gap between them. The last 256
disconnected clients are remembered. The fields are in the
[events reference](../skills/bibcode-profiler/references/events.md#rpcdisconnect).

## Invariants

- The server owns one durable FIFO per thread. Reloads, disconnects, and server
  restarts preserve queued rows in order; queued rows never become uncertain
  merely because the server restarted.
- Queued messages never look like work: no turn-start event, pending turn,
  session-status change, or sidebar Working state is created by enqueueing.
- Automatic promotion sends only the oldest unheld message into a `ready`
  session, with no outstanding approval, question, or active delivery. Interrupt
  and error settles hold queued rows; a later ready event does not clear holds.
- Promotion snapshots no new user choices: the enqueued model and modes are
  retained, while the message timestamp becomes the promotion time.
- Steering uses the provider's native operation and attributes the message to
  its runtime turn without starting a second BiBCode turn. Codex preserves the
  active turn ID; Claude attributes its matching echo to the runtime turn
  current when that echo arrives.
- Cancel never discards user text. It withdraws the queued message and restores
  it to the cancelling client's draft. Stop drains queued text in FIFO order
  before interrupting; unavailable attachments and failed cancellations remain
  visible to the user.
- Contracts define the wire; server and client fixtures guard compatibility.
- One connection supervisor owns reconnects. The Effect RPC protocol does not
  retry sockets independently.
- Authorization is checked at each HTTP route or RPC method, not inferred from
  successful authentication alone.
- `GET`/`POST /api/transfers/{token}` authenticate with a five-minute HMAC
  token minted by `projects.createDownloadUrl` (read scope) or
  `projects.createUploadUrl` (operate scope) while those RPCs hold the
  workspace path lease. Transfer and asset tokens share the server secret but
  not the MAC input: each signature covers a purpose string, so an asset
  capability can never redeem as a transfer capability, and the reverse.
  A download token names one file or folder under a normalized workspace root.
  A folder is sized against the archive limits when its URL is minted, so an
  oversized folder is refused as a `ProjectTransferError` the panel can show;
  the route repeats the pre-scan before streaming because the tree can grow
  between mint and redemption. A file download carries `Content-Length`; a zip
  is produced as it streams and stays chunked. Archive producer failures fail the
  response stream rather than ending it successfully with a partial ZIP. Desktop
  clients choose the destination before minting the download URL, so time spent
  in the native picker does not consume the token lifetime. Native download
  failures preserve bounded server error details and recovery guidance; transfer
  capability URLs are excluded from error messages and logs. `Content-Disposition` names the
  entry with an ASCII-safe `filename` plus an RFC 5987 `filename*`, because a
  raw non-ASCII or control byte is not a legal header value and would fail the
  whole response. One file-name policy (`transfer::upload`) governs both
  directions: a plain, non-empty name with no path separator or control
  character, within the 255-byte path-component limit. The `.part` sibling is
  sized separately by `partial_transfer_file_name`, which echoes only as much
  of the name as its own budget allows, so the partial never constrains the
  name a user may upload. The additional Windows rules (`< > : " | ? *`, a trailing dot or
  space, the reserved device names) are applied for the filesystem that will
  store the file, not the host asking — the upload route applies them only when
  the server itself runs on Windows, and a Windows desktop host renames a
  download it cannot store rather than refusing it, so a workspace file
  legitimately named `report:v2.txt` on Linux stays transferable. An upload token names one
  directory, one file name, and a byte cap: `POST /api/transfers/{token}` takes
  the name from the token, so a leaked URL can write only that one file, and a
  `?name=` that disagrees with the token is refused with 400 rather than
  honoured. Uploads reserve the target name, stream into a per-invocation
  `.part` file, and rename into place, refusing collisions unless
  `overwrite=1`, never replacing directories, and removing the partial and any
  reservation this server created on failure. The upload handler
  invalidates the entries index for that root and notifies the Git status
  broadcaster.
- Cancellation flows from client interrupt or socket closure until the
  operation's documented handoff. After a durable engine handoff, the
  server-owned lifecycle continues to a terminal receipt while only the caller
  wait is canceled.
- Cancellation flows from client interrupt or socket closure into registered
  handlers and supervised processes. An inline handler is dropped on interrupt
  or teardown instead of observing its token; the supervised runner's drop
  guard then stops the whole process tree (the process group on Unix, the job
  on Windows), as it does for an aborted request task, runtime shutdown, or a
  panic. The guard never waits (tokio reaps the root, the adopting parent reaps
  descendants) and signals only while the root is unreaped, so a run dropped
  after its root was reaped but before its output closed is not signalled; a
  descendant that still holds the pipes then survives until it exits. A failed
  run, which already owns an orderly termination path, still signals its group
  after the root was reaped: a descendant that is still in the group keeps the
  group id reserved, so the kill reaches exactly the stragglers, and only an
  empty group's id could be reused, which takes a PID wrap-around in between.
  Work that must finish cleanup before the caller continues cancels its token
  instead: a clone's owned transfer task stops the process group and removes the
  destination it created. With `detach`, admission into the clone runtime is
  the clone's handoff: caller cancellation (Interrupt, socket teardown, a
  dropped handler) then ends only that caller's wait, and only
  `vcs.cancelClone` or shutdown stops the clone. Without `detach`, a starter's
  cancellation still cancels its clone; a caller that joined a running clone
  never cancels it by leaving.
- Git Manager mutations use the worktree catalog's project-then-repository lock
  order and fail a competing operation with `operation-in-flight`; they do not
  introduce an independent repository lock.
- Git Manager guard text is authored by the server and rendered verbatim by the
  client. A stale client decision never authorizes a mutation.
- Git Manager provider data is requested only by an explicit user refresh; its
  live repository signal performs Git reads only and never polls a provider.
- Worktree removal holds one server terminal-admission fence from terminal
  shutdown through filesystem and Git settlement; client-side teardown alone
  is never the deletion safety boundary.
- Durable orchestration state, not a WebSocket connection, is the recovery
  boundary.
- A refused or unconfirmed turn delivery is visible outside the open thread as
  engine-derived thread state, never by a delivery component writing the
  provider session.
