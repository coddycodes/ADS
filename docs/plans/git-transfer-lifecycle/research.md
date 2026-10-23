# Git transfer lifecycle: reconnect-surviving clones and transport-helper cleanup — research

Research for two residuals of the "Clone from URL" fix (network budgets, Cancel, owned
cleanup) recorded in
[`docs/superpowers/specs/2026-09-23-clone-network-budget-design.md`](../../superpowers/specs/2026-09-23-clone-network-budget-design.md)
(lines 112-119):

1. A WebSocket reconnect cancels an in-flight clone and removes its folder.
2. Interrupting an inline Git RPC on Linux/macOS kills only the top-level `git`, so transport
   helpers (`git-remote-http`, `ssh`, …) keep running.

Date: 2026-09-24. The clone fix was committed on `main-3` during this research as
`fd5effbb` ("fix(git): let clones outlive 30 s, cancel cleanly and refuse half clones").
Line numbers refer to that commit; the cited anchors were re-checked against it after the
commit landed.

Sources and method:

- BiBCode source, tests, and living docs, read directly (CodeGraph sync succeeded and was
  used only for orientation; every claim below was confirmed in source).
- External crates at the locked versions, read from the Cargo registry
  (`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/`): **tokio 1.53.1**,
  **tokio-util 0.7.19**, **process-wrap 9.1.0** (`Cargo.lock:4160-4163`, `6112-6113`,
  `6173-6174`).
- Effect **4.0.0-beta.107**, read from the vendored `.repos/effect-smol/` (same version as the
  installed `node_modules/.pnpm/effect@4.0.0-beta.107`).
- Orca at `/work/github/orca`, v1.4.197, commit `122b8c25`. Orca paths below are relative to
  that repository.
- First-party docs: Microsoft job objects, POSIX.1-2024 base definitions, Git's
  `gitremote-helpers`.
- One OS-level reproduction with git 2.55.0 (Appendix A). No project build or test run.

## Summary

### Problem 1 — a reconnect cancels an in-flight clone

- **Server path.** Session teardown cancels every in-flight request token
  (`apps/server/src/rpc/session.rs:791-794`). `run_unary`'s biased `select!` then returns
  and drops the handler (`session.rs:1100-1110`). The clone's transfer token is a child of
  the request token and is also cancelled by a drop guard
  (`apps/server/src/git/repository.rs:4851-4855`), so the owned task stops Git and removes
  the destination (`repository.rs:4883-4919`). The residual is by construction, not a bug in
  the fix.
- **What drops the socket.** A close, the client's Effect RPC pinger (one unanswered 5-second
  ping ends the socket), an offline event, or a failed 15-second health probe when the page
  becomes visible. The server has no heartbeat of its own; it only answers `Ping` with
  `Pong`.
- **Client behavior.** The client never retries an in-flight unary request. The pending
  `vcs.clone` fails with an `RpcClientError`, or with an interrupt if the connection scope
  closes first (the order is a race). The dialog then shows either
  `Clone failed: SocketCloseError: <code>` (or `SocketOpenError: timeout waiting for "open"`
  after a ping timeout) or "The clone stopped before it finished. Try again." The user starts
  over, and the server has already deleted the partial clone.
- **Precedents.** `run_owned_git_mutation` gives _drop-safety_ (cleanup completes after the
  waiter goes away) but not _caller-independence_: its token still derives from the request
  (`git_vcs.rs:457-462`). Caller-independent lifecycles already exist: the durable
  `vcs.removeWorktree` task (tracked, fresh token, admission-permit clone, durable receipt;
  `git_vcs.rs:1084-1131`), the Pull Requests checkout write handoff
  (`pull_requests_rpc.rs:214-254`), and the worktree catalog operation runtime
  (`worktree_catalog_rpc.rs:373-416`).
- **Orca.** A remote-runtime `repo.clone` survives a disconnect _by omission_: the handler
  ignores the socket abort signal it is given. Orca serializes clones per destination, so a
  retry joins the running clone, and the server registers the repository itself on success.
  Remote clones cannot be cancelled at all.
- **Recommendation (Alternative A).** Make clone a runtime-owned, tracked operation keyed by
  its destination:
  - For requests that opt in with `detach: true`, request cancellation or a socket drop ends
    only the caller's wait. Older clients keep today's Interrupt-cancels semantics.
  - After a reconnect the client re-attaches with a join-only `vcs.clone` call. With no live
    entry, that call answers from a retained failure or the existing reuse check.
  - An explicit `vcs.cancelClone` stops the clone.
  - A clone with no attached waiter is cancelled after an orphan deadline.
  - The server keeps a failure outcome briefly for a client that re-attaches.
  - The operation holds a maintenance admission permit, and shutdown cancels then drains it.

  The client-runtime owns the re-attach loop, and a default-false capability gates it.
  Alternative B (an operation id with an observation stream and progress) is the path if
  progress display or multi-window observation is wanted. Alternative C (a durable command
  receipt) buys little, because Git cannot resume a clone.

### Problem 2 — interrupting an inline Git RPC leaves helpers running

- **Path.** Interrupt or teardown makes `run_unary` drop the handler
  (`session.rs:1100-1110`). The supervised runner then never reaches its cancellation branch
  (`apps/server/src/process/supervised.rs:215-217`), and never reaches the group kill in
  `terminate_and_wait_owned` (`supervised.rs:172-174, 364-413`). Instead the boxed child is
  dropped.
  - process-wrap's `ProcessGroupChild` has no `Drop` implementation.
  - tokio's `kill_on_drop` sends SIGKILL to the root PID only
    (tokio `src/process/mod.rs:1123-1129`).
  - The same happens when straggling request tasks are aborted after 1 second
    (`session.rs:807-813`) and when the runtime shuts down.
- **Reproduced** with git 2.55.0 (Appendix A). After SIGKILL on the root, `git remote-http`
  and `git-remote-http` survive in the same process group, reparented to `systemd --user`,
  and keep the TCP connection open. `killpg` ends them.
- **Affected.** Handlers that run Git or provider CLIs _inline_ in the unary future:
  - Git Manager reads, notably `gitManager.getRemoteTags` (`git ls-remote`).
  - `vcs.listRefs`, `vcs.listCommits`, `vcs.generateCommitMessage`.
  - The pull-request enrichment inside `vcs.refreshStatus`.
  - `git.resolvePullRequest`, `sourceControl.lookupRepository`,
    `server.discoverSourceControl`.
  - The eight `pullRequests.*` reads and `pullRequests.runAction`.

  Owned mutations, streams, and clone cancel through the token and are unaffected. Windows is
  unaffected, because closing the job handle kills the tree.

- **Recommendation (Alternative A).** Add a drop guard in `run_supervised_with_observer`. If
  a run did not finish, the guard calls `start_kill()` (`killpg(SIGKILL)` on Unix, job
  termination on Windows), and only while `child.id()` is `Some`. That condition means the
  root is not reaped, so POSIX keeps the process-group id reserved. The guard logs failures
  and leaves reaping to tokio's orphan queue (for the root) and to the adopting parent (for
  helpers). It never waits, spawns, or blocks in `Drop`. This single choke point covers every
  drop path and every `run_supervised` caller. A stalled-remote RPC test proves it.

---

## 1. Problem 1 evidence — why a reconnect stops a clone

### 1.1 The server path that stops the clone

1. **Teardown cancels every request token.** When the socket loop exits, the session cancels
   `session_shutdown` and then every in-flight request token
   (`apps/server/src/rpc/session.rs:791-794`). It then joins the request tasks for at most
   `PUMP_JOIN_TIMEOUT` = 1 s and aborts stragglers (`session.rs:41, 798-814`). The
   connection's drop guard also cancels `session_shutdown` on every exit path
   (`session.rs:680`).
2. **Interrupt uses the same token.** `ClientMessage::Interrupt` calls
   `request.cancellation.cancel()` (`session.rs:851-855`). A handler cannot tell teardown
   from an explicit Interrupt through its own token. The only discriminator is
   `RpcSessionContext::connection_closed()` (`session.rs:305-315`), which teardown cancels
   _before_ the request tokens (`session.rs:791`).
3. **`run_unary` drops the handler.** `select! { biased; cancelled => { send Interrupt exit;
   return } result = handler(...) }` (`session.rs:1100-1110`). The handler future is dropped
   without being polled again.
4. **Clone is inline in the handler.** `vcs.clone` and `sourceControl.cloneRepository` are
   routed through the inline `_ =>` branch into `handle_admitted_unary`
   (`apps/server/src/production/git_vcs.rs:632-639, 820-837, 867-871, 1559-1595`) with
   `operation_cancellation = cancellation.child_token()` (`git_vcs.rs:457`).
5. **The clone is cancelled from both sides.** `clone_repository` spawns the transfer and its
   cleanup as one task, but derives the transfer token from the request token and arms a
   drop guard (`repository.rs:4851-4871`):

   ```rust
   let transfer_cancellation = cancellation.child_token();
   let _cancel_transfer_on_drop = transfer_cancellation.clone().drop_guard();
   ```

   A `DropGuard` "will cancel this token (and all its children) on drop unless disarmed"
   (tokio-util `src/sync/cancellation_token.rs:271-277`). So either the parent token
   (teardown or Interrupt) or the handler drop cancels the transfer. The task then stops Git
   (token path, §3.1) and removes only a destination it created
   (`repository.rs:4904-4918, 6947-6962`).

6. **Documented contract.** "Cancellation flows from client interrupt or socket closure until
   the operation's documented handoff"
   (`docs/architecture/rpc-and-orchestration.md:1931-1934`), and clone has no handoff. The
   spec lists the consequence as a residual (spec lines 117-118).

The clone task itself is an untracked `tokio::spawn` (`repository.rs:4859`). Dropping its
`JoinHandle` "detaches" it (tokio `src/runtime/task/join.rs:18`), which is what lets cleanup
finish after the RPC ends. Nothing drains it at shutdown (§1.7).

### 1.2 What drops a connection

- **Client liveness.** `makeProtocolSocket` races the socket against a pinger
  (`.repos/effect-smol/packages/effect/src/unstable/rpc/RpcClient.ts:1095-1107`). The pinger
  sends a ping every 5 seconds, and if the previous ping got no pong it opens the timeout
  latch (`RpcClient.ts:1168-1195`, delay at `1183`). A single pong that arrives more than
  about 5 seconds late ends the socket with `SocketOpenError { kind: "Timeout" }`. Orca, by
  contrast, tolerates three consecutive missed probes
  (`src/main/runtime/rpc/remote-runtime-server-heartbeat.ts:3-7`).
- **Server.** The server has no heartbeat. It only answers `Ping` with `Pong` through the
  ordinary outbound queue (`session.rs:835-837`).
- **Supervisor.** `monitorConnectedLease` ends a live lease on an explicit disconnect or
  retry, on `NetworkChanged` offline, on a relay credential change, and when the
  `application-active` probe fails its 15-second deadline
  (`packages/client-runtime/src/connection/supervisor.ts:33, 381-449`). The lease also ends
  when `session.closed` fires (`supervisor.ts:537-552`).
- **Recovery.** Retries follow 1/2/4/8/16-second backoff
  (`docs/architecture/connection-runtime.md:343-347`). The RPC protocol's own reconnect is
  disabled on purpose: `retryTransientErrors: false`, `retryPolicy: Schedule.recurs(0)`
  (`packages/client-runtime/src/rpc/session.ts:165-170`; `connection-runtime.md:364-366`).

A remote clone over SSH forwarding or relay therefore dies on any transient blip longer than
about one ping interval. The user's reported environment — a remote Fedora server at about
218 KiB/s (spec lines 7-19) — is exactly where this happens.

### 1.3 What the client runtime and the dialog do

1. **Clone is one unary command.** It runs through `createEnvironmentRpcCommand` on the
   serial VCS lane (`packages/client-runtime/src/state/vcs.ts:162-167`;
   `state/vcsCommandScheduler.ts:35-48`) and calls `request(tag, input)`. `request` resolves
   the _current_ session, or fails with `EnvironmentRpcUnavailableError` when there is none
   (`packages/client-runtime/src/rpc/client.ts:110-126, 136-144`).
2. **Socket loss fails every pending request.** On socket end, the protocol runs
   `onDisconnect`, then broadcasts `ClientProtocolError` (`RpcClient.ts:1112-1134`). The
   client resolves every pending entry with `Exit.fail(RpcClientError)`
   (`RpcClient.ts:758-764`). With `recurs(0)` the retry falls straight through to
   `broadcastError` (`RpcClient.ts:1135-1138`), and later sends fail with the stored error
   (`RpcClient.ts:1147-1149`).
3. **The outcome is racy.** If the supervisor closes the connection scope first, the client's
   shutdown finalizer resolves entries with `Exit.interrupt` instead (`RpcClient.ts:285-291`).
4. **No domain retry exists.** Subscriptions switch to the next session
   (`rpc/client.ts:227-255`), and a transport failure on a durable subscription only logs
   (`rpc/client.ts:186-205`). Unary commands have no equivalent.
5. **What the dialog shows.** `adaptAtomResult` maps an interrupt to `error: null` and
   anything else to the squashed failure (`apps/web/src/components/add-project/useAddProjectWorkflow.ts:679-687`).
   `addProjectOperations.clone` turns a failure into `Failed` and `null` into `Stopped`
   (`addProjectOperations.ts:78-106, 176-193`). `submitClone` then shows one of two
   messages:
   - `Clone failed: <message>` (`useAddProjectWorkflow.ts:570-571`). Because `Schema.Error`
     derives from `globalThis.Error` (`.repos/effect-smol/packages/effect/src/internal/core.ts:566-586`),
     the message is `RpcClientError.message`, i.e. `"<reason tag>: <reason message>"`
     (`RpcClientError.ts:54-56`). Examples: `SocketCloseError: 1006`, or
     `SocketOpenError: timeout waiting for "open"` after a ping timeout (`Socket.ts:270-275,
     302-307`). Neither tells the user what happened or what to do (`UI.md` actionable-error
     rule).
   - "The clone stopped before it finished. Try again." (`useAddProjectWorkflow.ts:159-164,
     575-578`), when the interrupt wins. The comment at `useAddProjectWorkflow.ts:160-161`
     anticipates only this path.
6. **Other dialog behavior.** The host list comes from the environment catalog, not the
   connection state (`useAddProjectWorkflow.ts:718-760`), so a reconnect does not trigger the
   "selected host disconnected" reset. Unmounting the dialog deliberately aborts the clone
   (`useAddProjectWorkflow.ts:258-266`), and **Cancel clone** uses the atom command's abort
   signal (`useAddProjectWorkflow.ts:591-593, 818-830`; `connection-runtime.md:450-456`).

### 1.4 Server-owned lifecycles that already outlive the caller

| Lifecycle                                                                                               | Token after the RPC wait ends                                                                                                                                               | Owner / shutdown                                                                                                                                            | Survives restart?                                                                                        |
| ------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------- |
| Git mutations via `run_owned_git_mutation` (`vcs.pull`, stage, switch, publish…) (`git_vcs.rs:643-692`) | Request child token (`git_vcs.rs:457, 462`): teardown still cancels Git. The continuation keeps admission, guard, and cleanup (`rpc-and-orchestration.md:171-179`).         | Untracked `tokio::spawn` (`git_vcs.rs:438-447`)                                                                                                             | No                                                                                                       |
| Git Manager mutations (`git_manager_rpc.rs:711-824`)                                                    | Request token → `lock_cancellation` child (`git_manager_rpc.rs:776-777`)                                                                                                    | Untracked `tokio::spawn` (`git_manager_rpc.rs:1341-1353`)                                                                                                   | No                                                                                                       |
| Clone (this fix) (`repository.rs:4851-4880`)                                                            | Request child token plus drop guard                                                                                                                                         | Untracked `tokio::spawn` (`repository.rs:4859`)                                                                                                             | No; half-clone left (§1.7)                                                                               |
| `vcs.removeWorktree` (`git_vcs.rs:1084-1131`)                                                           | Fresh `CancellationToken::new()` (`git_vcs.rs:1089`): caller-independent                                                                                                    | `WorktreeRemovalTaskTracker` (`git_vcs.rs:42-134`), holds an admission-permit clone (`git_vcs.rs:1087`), drained in `quiesce_for_update` (`runtime.rs:611`) | Durable receipt (`persistence/migrations.rs:2334-2349`; `rpc-and-orchestration.md:1824-1870`)            |
| `pullRequests.checkout` write (`pull_requests_rpc.rs:198-268`)                                          | Request token before the write handoff, a separate token after it. An interrupted wait reports "Checkout continues in the background" (`rpc-and-orchestration.md:504-523`). | Worktree operation runtime, drained without cancelling started writes (`rpc-and-orchestration.md:524-528`)                                                  | No                                                                                                       |
| Worktree catalog operations (`worktree_catalog_rpc.rs:362-417`)                                         | "Once the envelope is handed off, caller cancellation only ends the wait" (`rpc-and-orchestration.md:979-989`)                                                              | 64-permit runtime with non-waiting admission, closed and drained at shutdown (`rpc-and-orchestration.md:991-997`)                                           | Durable command receipts; `reserved`/`prepared` resumable (`rpc-and-orchestration.md:966-977, 999-1016`) |
| Turns (`rpc-and-orchestration.md:1142-1145`)                                                            | Lease transferred into the engine envelope; "client disconnect only stops the caller wait"                                                                                  | Orchestration engine                                                                                                                                        | Durable outbox                                                                                           |
| Terminal sessions (`docs/architecture/overview.md:832-839, 851-880`)                                    | Keyed by thread/terminal; the same bearer reconnects as a new connection identity (`rpc-and-orchestration.md:1699-1703`) and re-attaches with transcript replay             | Terminal manager                                                                                                                                            | No (attach may recreate, `overview.md:915-918`)                                                          |
| Activity cancellation (`rpc-and-orchestration.md:1598-1602`)                                            | "Reconnect can recover the current server's control snapshot; restart … invalidates it"                                                                                     | Runtime state                                                                                                                                               | No                                                                                                       |

Idempotency keys already on the wire:

- `project.create` and other orchestration commands carry a client-generated `commandId`
  (`packages/client-runtime/src/operations/commands.ts:85-95`), backed by
  `orchestration_command_receipts` (`migrations.rs:849-869`).
- `worktree.createManaged` takes "an idempotency command identity"
  (`rpc-and-orchestration.md:860-861`).

The governing rule: "Durable orchestration state, not a WebSocket connection, is the recovery
boundary" (`rpc-and-orchestration.md:1951-1952`).

### 1.5 What it takes for clone to become server-owned

1. **Live outside the request task.** Straggling request tasks are aborted after 1 s
   (`session.rs:807-813`). Clone already spawns; the only couplings are
   `child_token()`/`drop_guard()` (`repository.rs:4854-4855`) and the untracked spawn.
2. **A re-attach key.**
   - **The destination is already one.** `CloneDestinationRegistry::acquire` serializes
     clones per normalized destination (`repository.rs:6849-6933`, key at `6901`), and
     `reuse_existing_clone` returns a finished clone with a matching origin
     (`repository.rs:4958-5056`). Today a retry _waits_ behind a running clone and then
     reuses it (spec lines 97-100).
   - **An operation id is the alternative.** It would follow `commandId`, or Orca's
     `clientMutationId` (§1.6).
3. **An explicit cancel channel.** Interrupt and teardown share the token (§1.1 point 2), and
   a disconnected client cannot send an Interrupt at all. Cancel-after-reconnect therefore
   needs its own RPC (`orchestration:operate`, like `vcs.clone`:
   `apps/server/src/auth/scope.rs:116, 124`). Whether a request's token cancellation means
   _detach_ or _cancel_ is best chosen by the request itself (an opt-in field, §2 A), not
   inferred on the server.
4. **Cleanup on failure.** The existing owned cleanup is identity-checked: it removes only a
   directory whose device/inode matches the one this clone reserved
   (`repository.rs:225-307, 6947-6962`).
5. **A bounded lifetime with nobody attached.** Transfers keep Git's HTTP stall guard and the
   24-hour bound (`overview.md:152-161`). SSH has no stall guard. An orphan deadline is
   needed on top.
6. **Multiple windows.** Each window has its own connection. The principal is stable across
   reconnects, but the connection identity is not (`rpc-and-orchestration.md:1699-1701`).
7. **Maintenance.** Today the request task holds the `vcs.clone` admission permit for the
   whole clone (`session.rs:961-981, 1012-1013`). Update preparation drains permits and
   names blockers on timeout (`apps/server/src/maintenance.rs:192-238`). A detached clone
   should clone the permit, like removal does (`git_vcs.rs:1087`), so an update keeps
   reporting "vcs.clone" as a blocker instead of silently killing it. `RpcPermit` is `Clone`
   (`maintenance.rs:302-303`).
8. **Project registration** happens client-side, after the clone returns
   (`addProjectOperations.ts:194-202`), with a client-resolved default model selection
   (`useAddProjectWorkflow.ts:777-816`). A re-attached call returns the same path, so
   registration can stay client-side. A finished clone that nobody registered remains
   reusable by `reuse_existing_clone`.
9. **Client placement.** "React presentation does not own sockets or retry loops"
   (`connection-runtime.md:31-32`), and domain requests "fail or wait according to the domain
   API" (`connection-runtime.md:517-518`). The re-attach loop therefore belongs in
   `packages/client-runtime/src/state/`, waiting on supervisor sessions like `subscribe()`
   does (`rpc/client.ts:236-249`).
10. **Compatibility in both directions.** Additive default-false capability booleans are the
    established pattern (`packages/contracts/src/environment.ts:31-61`;
    `connection-runtime.md:458-482`).
    - The client must not auto-retry against an older server, where a retry would start a
      fresh clone.
    - A newer server must keep an older client's `Interrupt`-based Cancel working
      (`useAddProjectWorkflow.ts:591-593`). So detaching must be opt-in per request, not the
      new default.

### 1.6 How Orca handles long operations across reconnects

- **Remote-runtime clone survives disconnect by omission.** The WebSocket dispatch registers
  a per-socket abort signal (`src/main/runtime/runtime-rpc/runtime-rpc-websocket-dispatch.ts:101,
  151`; `runtime-rpc-binary-routing.ts:33-41`) and passes it into every unary handler context
  (`src/main/runtime/rpc/rpc-streaming-dispatcher.ts:105-109`). But the `repo.clone` handler
  never uses `context.signal` (`src/main/runtime/rpc/methods/repo.ts:128-136`). The clone
  controller takes no signal and spawns `git clone --progress` without an abort path or
  deadline (`src/main/runtime/runtime-repository-clone-controller.ts:35-39, 100-144`). On
  success the server registers the repository itself (`store.addRepo`, `:146-186`).
- **Retry joins by destination.** `inFlightByPath` chains clones per destination
  (`runtime-repository-clone-controller.ts:30-77`). A later call returns the
  already-registered repository (`:90-98`).
- **Client.** `callRuntimeRpc(target, 'repo.clone', …, { timeoutMs: 10 * 60_000 })`
  (`src/renderer/src/components/sidebar/useAddRepoCloneFlow.ts:140-151`). On socket close,
  every pending request is rejected (`src/shared/remote-runtime-request-connection.ts:126-140`).
  There is no re-attach; the repository appears through the repository list.
- **No remote cancel.** `repos:cloneAbort` only covers local IPC clones and SSH-relay clones
  (`src/main/ipc/repos/repo-clone-lifecycle.ts:101-114`;
  `src/main/ipc/repos/remote-repo-clone.ts:141-147`). The SSH-relay path also has a 10-minute
  timeout and rejects a concurrent clone into the same destination
  (`remote-repo-clone.ts:71-74, 87-99`).
- **Generic idempotency.** `worktree.create` deduplicates on `clientMutationId`: failures drop
  immediately, and successes linger 60 s so a retry whose reply was lost still reconciles
  (`src/main/runtime/orca-runtime-terminal-create-deduplication.ts:174-203`;
  `src/main/runtime/orca-runtime-core.ts:64-73`). Terminal create has in-flight dedupe
  (`src/main/runtime/remote-runtime-terminal-create-idempotency.ts:5-36`). Both are
  capability-gated (`src/shared/protocol-version.ts:113-117, 131-133`).
- **Restart.** Orca's daemon-owned PTYs outlive a runtime restart
  (`docs/plans/remote-servers/orca-remote-servers-research.md:65-72, 510-511`). Nothing
  comparable exists for clones.

Takeaway: Orca's user-visible behavior ("the clone keeps going; retrying gives you the
result") comes from a destination lock plus server-side registration, not from a re-attach
protocol. Its gaps are an unbounded orphan and no remote cancel. BiBCode's existing
destination registry makes the same join cheap, while keeping Cancel and bounds.

### 1.7 Server restart

- **Graceful shutdown today.** `quiesce_for_update` drains the worktree runtimes and removal
  tasks (`apps/server/src/production/runtime.rs:598-635`), but not clone tasks. On runtime
  shutdown, spawned tasks "are dropped" at their next yield (tokio
  `src/runtime/runtime.rs:30-37`). The dropped clone task runs no cleanup, and the runner's
  drop kills only the root `git` (§3.1). The partial destination stays, which is the spec
  residual at line 113.
- **Standalone descendant sweep.** The standalone binary sweeps descendants still rooted at
  its PID (`apps/server/src/production/server_terminal.rs:207-232, 319-330`;
  `overview.md:197-202`). That can catch `git` and its helpers if they are still attached.
  The embedded desktop host skips the sweep.
- **After restart.** `reuse_existing_clone` refuses the leftover: "An incomplete clone exists
  at <path>. Remove it or choose another folder." (`repository.rs:5034-5054`).
- **What a server-owned clone must do.**
  - Close admission, cancel every transfer token, and drain the cleanup in
    `quiesce_for_update` before the process owners stop. Draining without cancelling, as the
    checkout write does (`rpc-and-orchestration.md:524-528`), would hold shutdown for hours.
  - A client that re-attaches after a restart must get a clear "no clone in progress" answer,
    not a silent fresh clone.
  - For a crash, either add a durable receipt (destination plus reserved device/inode, like
    `worktree_removal_receipts`) with an identity-checked startup sweep, or clone into a
    temporary sibling and rename on success so a partial clone never occupies the final name.
    Both are optional follow-ups; the actionable refusal already covers a crash safely.

## 2. Problem 1 — alternatives and recommendation

### A. Detached clone operation keyed by destination (recommended)

Server:

- **A runtime-owned registry.** Replace the process-global static `CloneDestinationRegistry`
  (`repository.rs:6879-6883`) with a registry owned by `GitVcsRpcServices`, like
  `WorktreeRemovalTaskTracker`. It uses non-waiting bounded admission with a typed "busy"
  error, like the catalog runtime (`worktree_catalog_rpc.rs:369, 377-385`).
  - Each entry, keyed by the normalized destination, holds: the URL; a root transfer token
    (`CancellationToken::new()`, not a request child); a status watch (`Running`,
    `Succeeded(path)`, `Failed(error)`, `Cancelled`); an attached-waiter count; an
    admission-permit clone; and the tracked task.
- **Start-or-join.** `vcs.clone` and `sourceControl.cloneRepository` start a clone or join a
  running one for the same URL.
- **Join-only re-attach.** An additive `attach: true` makes the call join-only, so a
  re-attaching client never silently starts a new clone after a restart, a cancel from
  elsewhere, or a failure. When no live entry exists, a join-only call answers
  deterministically:
  - A failure retained within its TTL returns that failure.
  - Otherwise the server runs the existing reuse check (`repository.rs:4958-5056`). A complete
    clone with a matching origin returns `Succeeded(path)`. A placeholder or commit-less HEAD
    returns the actionable incomplete-clone error. An absent destination returns "No clone is
    in progress for <path>", and the dialog offers Clone again.
- **Detach is opt-in per request.** An additive `detach: true` asks the server to treat request
  cancellation (Interrupt or teardown) as _detach_: it only decrements the waiter count. That
  is the same documented handoff as catalog operations (`rpc-and-orchestration.md:979-989`).
  - Without the field, token cancellation _cancels_ the clone exactly as today. An older
    client whose **Cancel clone** still sends `Interrupt` (`useAddProjectWorkflow.ts:591-593`;
    `connection-runtime.md:450-456`) therefore keeps a working Cancel.
  - New clients never cancel with `Interrupt` and old clients never detach. The server does
    not need the `connection_closed()` discriminator from §1.1.
- **Explicit cancel.** `vcs.cancelClone { destination }` (`orchestration:operate`) cancels
  the transfer token, and the existing cleanup removes only a destination this clone created.
- **Orphan deadline.** Proposed: 15 minutes with no attached waiter, then cancel. Success
  needs no retention, because the finished clone on disk is reusable. A failure is retained
  for a short TTL (proposed 5 minutes) so an `attach` call reports the real reason ("The
  transfer stalled…").
- **Shutdown.** Close admission, cancel everything, and drain, next to
  `worktree_removal_tasks.close_and_drain()` (`runtime.rs:611`). Problem 2's guard is the
  backstop for any task that is dropped anyway.

Client:

- **A client-runtime command re-attaches.** It sends `detach: true` on every clone request.
  On a transport failure (`RpcClientError`, `EnvironmentRpcUnavailableError`, or an interrupt
  the user did not cause), it publishes a "reconnecting" phase, waits for the next connected
  session within the orphan deadline, and re-issues `vcs.clone` with `attach: true` and
  `detach: true`.
- **Cancel** sends `vcs.cancelClone` right away if connected, otherwise after reconnect.
  Unmount keeps today's safe default, which is to cancel.
- **Copy** for the dialog: "Lost the connection to <host>. The clone continues on the server;
  reconnecting…"
- **Gating:** a new capability, `vcsCloneReattach` (default false). The client sends
  `detach`/`attach` and re-attaches only when the server advertises it. The server detaches
  only when a request asks for it, so both old-client/new-server and new-client/old-server
  pairs keep today's semantics.

Trade-offs:

- For:
  - Smallest wire delta: two optional fields, one RPC, one capability.
  - Reuses the destination lease and reuse logic that the current rulings already rely on.
  - Matches Orca's proven "retry joins" behavior, while keeping Cancel and bounds.
  - Fixes the shutdown residual.
- Against:
  - No progress display.
  - Other windows observe a clone only by targeting the same destination.
  - Path-keyed cancel lets any operate-scoped client cancel a clone into a path it names.
    That is the same authority needed to start one there.
  - Keys stay lexical, so a symlinked alias still collides as it does today.

### B. Clone operation resource with an id and an observation stream

- **API.** `vcs.startClone { operationId, url, parentDir, directoryName }` returns once the
  operation is accepted. `subscribeCloneOperation { operationId }` is a latest-value stream
  carrying state and progress. `vcs.cancelClone { operationId }` cancels. An optional
  `listCloneOperations` lets other windows discover running clones. Terminal states are
  retained for a TTL.
- **Re-attach is automatic.** `subscribe()` already switches to the next session
  (`rpc/client.ts:227-255`), with latest-value semantics like `subscribeWorktreeCatalog`
  (`rpc-and-orchestration.md:806-810`).
- **Progress needs a streaming output path.** The supervised runner buffers output until exit
  (`supervised.rs:262-276, 309-341`), so parsing `git clone --progress` means per-line
  output, like the Git Manager's per-command output events.

Trade-offs:

- For:
  - Delivers the progress display the spec deferred (spec lines 45-47).
  - Real multi-window observation.
  - Generalizes to the Git Manager's cancellable transfers, which die on teardown the same
    way (`rpc-and-orchestration.md:305-307`; `git_manager_rpc.rs:253`).
  - No client retry loop.
- Against:
  - The largest change: contracts, RPC wire fixtures (`packages/contracts/fixtures/rpc-wire`),
    client state, server state, and progress parsing.
  - Two-step start/observe race handling.

### C. Durable clone command with a receipt

- **Mechanism.** Admit clone like an orchestration command with a `commandId` receipt
  (`rpc-and-orchestration.md:966-977, 1004-1016`) and a durable row. Optionally register the
  project server-side on success, as Orca does.
- **For:**
  - Restart-aware reporting and identity-checked cleanup.
  - The outcome would appear in shell snapshots without any re-attach.
- **Against:**
  - The heaviest option.
  - A clone cannot resume after a restart (only re-run or clean up), so durability mostly
    buys cleanup.
  - Server-side registration would move default model selection from the client
    (`useAddProjectWorkflow.ts:777-816`) to the server.

### Recommendation

Implement **A**. Shape the registry entry so it can later carry B's watch and progress stream
and C's receipt without a rewrite. Land Problem 2's guard first: it is the backstop that makes
A's shutdown behavior safe. Record the new handoff in the invariants
(`rpc-and-orchestration.md:1931-1940`): "for a clone requested with `detach`, admission into
the clone runtime is the handoff; caller cancellation then only ends the wait, and
`vcs.cancelClone` stops it; without `detach`, cancellation still stops the clone."

## 3. Problem 2 evidence — why an interrupt leaves helpers running

### 3.1 The exact code path

1. **Cancellation.** Interrupt cancels the request token (`session.rs:851-855`). Teardown
   cancels all of them (`session.rs:791-794`).
2. **Drop.** `run_unary`'s biased `select!` takes the cancelled branch, sends the Interrupt
   exit, and returns, dropping the handler future (`session.rs:1100-1110`). Two other paths
   drop in-flight Git futures the same way:
   - The 1-second abort of request tasks that outlive teardown (`session.rs:807-813`).
   - Runtime shutdown dropping every task at its next yield (tokio
     `src/runtime/runtime.rs:30-37`). This includes _owned_ tasks such as the clone and
     mutation tasks.
3. **The runner never sees the cancellation.** For an inline handler, the dropped future is
   inside `run_supervised_with_observer` (`supervised.rs:102-176`), mid-`execute_child`
   (`supervised.rs:165-171`).
   - Its cancellation branch (`supervised.rs:215-217`) never runs.
   - The error-path cleanup never runs either: `terminate_and_wait_owned`
     (`supervised.rs:172-174, 364-401`) → `request_termination` → `start_kill()`
     (`supervised.rs:403-413`).
4. **The child is dropped.** On Unix, the `Box<dyn ChildWrapper>` is a `ProcessGroupChild`.
   The wrap order is `KillOnDrop`, then `ProcessGroup::leader()`
   (`apps/server/src/process/background.rs:131-137`), and process-wrap stacks `wrap_child`
   in insertion order (process-wrap `src/generic_wrap.rs:103-106`).
   - `ProcessGroupChild` defines `start_kill` as `killpg(SIGKILL)` (process-wrap
     `src/tokio/process_group.rs:113-115, 174-176`), but has **no `Drop`**
     (`process_group.rs:61-65`; the file has no `impl Drop`).
   - Dropping it drops the inner tokio `Child`, whose `ChildDropGuard::drop` calls `kill()`
     because `KillOnDrop` set `kill_on_drop(true)` (tokio `src/process/mod.rs:1123-1129`;
     process-wrap `src/tokio/kill_on_drop.rs:15-19`). That is SIGKILL to the **root PID
     only**.
   - tokio queues the root for "best-effort" reaping: the caveat doc (`src/process/mod.rs:645-663`),
     `Reaper::drop` (`src/process/unix/reap.rs:117-130`), and a queue drained after each
     driver park that observes SIGCHLD (`src/runtime/process.rs:31-39`).
5. **Descendants survive.** Git spawns helpers "as an independent process", talks to them
   over stdin/stdout, and ships `git-remote-http(s)` for HTTP transports
   ([gitremote-helpers](https://git-scm.com/docs/gitremote-helpers)). The helpers inherit the
   group created at spawn (`process_group.rs:87-89`), survive the root's SIGKILL, and are
   reparented. A helper blocked on network I/O holds its connection until the peer answers or
   closes (Appendix A). A helper that is mid-transfer is expected to run until it next touches
   the dead parent's pipes.

The token path works: cancelling the token kills a parent, child, and grandchild tree
(`apps/server/tests/process_runner.rs:331-385`, fixture `804-855`, assertion `960-970`). Only
the drop path is broken.

**Windows is unaffected.** `WindowsSupervisedChild` owns an `Arc<WindowsJob>`
(`background.rs:68-127`) created with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`
(`apps/server/src/process/windows_job.rs:37-48`), and its `Drop` closes the handle
(`windows_job.rs:124-129`). That flag "causes all processes associated with the job to
terminate when the last handle to the job is closed"
([JOBOBJECT_BASIC_LIMIT_INFORMATION](https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-jobobject_basic_limit_information)).
An in-flight job wait holds an `Arc` clone only after `terminate()` has already run
(`background.rs:115-121`).

### 3.2 Reproduction (git 2.55.0, Fedora 44 kernel 7.2.7)

The script in Appendix A reproduces the runner's semantics. It points `git ls-remote` at a
loopback listener that accepts and never answers, runs it as the leader of a new process group
(like `ProcessGroup::leader()`), SIGKILLs and reaps the root only (like tokio's
`kill_on_drop`), then calls `killpg` (like `start_kill()`):

```text
helper sent: GET /stalled.git/info/refs?service=git-upload-pack HTTP/1.1
root pid=829403 pgid=829403
group before root SIGKILL:
    829403  829402  829403  S  git -c http.proxy= ls-remote http://127.0.0.1:49731/stalled.git
    829404  829403  829403  S  /usr/libexec/git-core/git remote-http http://127.0.0.1:49731/stalled.git ...
    829405  829404  829403  S  /usr/libexec/git-core/git-remote-http http://127.0.0.1:49731/stalled.git ...
group after root SIGKILL + reap:
    829404  3158  829403  S  /usr/libexec/git-core/git remote-http ...
    829405  829404  829403  S  /usr/libexec/git-core/git-remote-http ...
helper connection: still open after 1 s
group after killpg: empty
helper connection after killpg: closed (EOF)
```

PID 3158 is `/usr/lib/systemd/systemd --user`, a subreaper, which adopted and will reap the
orphans. BiBCode sets `PR_SET_CHILD_SUBREAPER` only inside one isolated test
(`git_vcs.rs:5017-5020`), so production orphans go to init or the session subreaper, not to
the server. The existing RPC test states the same fact: "Git holds the connection until the
runner stops its process group" (`apps/server/tests/production_git_vcs_rpc.rs:2300-2301`).

### 3.3 Which RPCs are affected

On Interrupt or teardown, an **inline** handler is dropped (helpers leak). An **owned or
stream** handler sees its token cancelled, and the runner kills and reaps the group.

| Surface                                            | Inline (dropped)                                                                                                                                                                                                                                                                                                                                                                                                                                                      | Owned / token path                                                                                                                                                                                                                                                                                                                                              |
| -------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `git_vcs.rs` unary (`git_vcs.rs:169-189, 412-423`) | `vcs.listRefs`, `vcs.listCommits` (`727-755`); `vcs.generateCommitMessage` (`838-848`); `vcs.refreshStatus` PR enrichment via gh/glab (`711-720`; the status read itself is owned by the status owner, `apps/server/src/git/status_owner.rs:847`); `git.resolvePullRequest` (`849-853`) and the resolution phase of `git.preparePullRequestThread` (`590-595`); `server.discoverSourceControl` (`854-861`); `sourceControl.lookupRepository` (`862-866`, `1520-1557`) | `vcs.pull`, `createRef`, `switchRef`, `init`, `stage`/`unstage`/`discardFiles`, the mutation phase of `preparePullRequestThread`, `sourceControl.publishRepository` (`459-631`, via `run_owned_git_mutation`). `vcs.clone` and `sourceControl.cloneRepository`: inline handler, but the transfer is an owned task with a drop guard (`repository.rs:4851-4871`) |
| `git_vcs.rs` streams (`git_vcs.rs:191-195`)        | —                                                                                                                                                                                                                                                                                                                                                                                                                                                                     | `subscribeVcsStatus`, `subscribeVcsStatusSummary`, `git.runStackedAction` (spawned producer, `1347-1453`)                                                                                                                                                                                                                                                       |
| Git Manager (`git_manager_rpc.rs:998-1034`)        | `getRefs`, `getCommits`, `getDiff`, `getStashes`, **`getRemoteTags` (network `ls-remote`, `402-414`)**, `previewMerge`, `listPullRequests` (gh/glab, `877-948`)                                                                                                                                                                                                                                                                                                       | Six mutations (`await_server_owned_mutation`, `725, 1341-1353`); `runOperation` and `subscribeGitManagerSignal` (spawned, `159, 324`)                                                                                                                                                                                                                           |
| Pull Requests (`pull_requests_rpc.rs:326-345`)     | Eight reads (`97-184`) and `runAction` (`191-197`): gh, glab, and git via `HostCommandRunner` → `ProcessRunner` (`rpc-and-orchestration.md:727-736`)                                                                                                                                                                                                                                                                                                                  | `checkout` (worktree operation runtime, `198-268`)                                                                                                                                                                                                                                                                                                              |
| Worktree catalog, projects                         | Entry-index `git ls-files` build uses a fresh, never-cancelled token (`apps/server/src/workspace/rpc.rs:947`), so drop is the only stop. `ls-files` rarely has descendants.                                                                                                                                                                                                                                                                                           | Catalog operations and scans are runtime- or repository-owned (`rpc-and-orchestration.md:979-997`; `overview.md:173-178`)                                                                                                                                                                                                                                       |

Two handlers appear in `handle_admitted_unary` but are not registered:
`vcs.createWorktree` and `vcs.removeWorktree` (`git_vcs.rs:756-819`) are absent from
`GIT_VCS_UNARY_METHODS` (`git_vcs.rs:169-189`). The durable removal path is separate.

Which helpers realistically appear:

- **Network reads:** HTTP(S) transport helpers (demonstrated), `ssh` for SSH remotes, and
  credential helpers.
- **Local reads:** can spawn children for submodule status, LFS or other filter processes,
  and textconv or external diff drivers.
- **gh/glab:** single Go binaries, but they may run `git`.
- **Only the interrupt window leaks.** An uninterrupted stalled read hits its own deadline,
  and the timeout path kills the group.

### 3.4 Constraints on a fix

- **Reaping.** tokio reaps only the root. Helpers are reparented to init or the nearest
  subreaper, which reaps them. No zombie accrues to the server unless it becomes a subreaper
  (§3.2).
- **Process-group id reuse.**
  - POSIX: a process lifetime ends "when its process ID is returned to the system", i.e. when
    it is reaped (POSIX.1-2024 base definitions
    [3.285](https://pubs.opengroup.org/onlinepubs/9799919799/basedefs/V1_chap03.html)).
  - A process ID "shall not be reused … until the process group lifetime ends" when a group
    with that id exists
    ([4.17](https://pubs.opengroup.org/onlinepubs/9799919799/basedefs/V1_chap04.html)).
  - A group ends when its last member leaves
    ([3.283](https://pubs.opengroup.org/onlinepubs/9799919799/basedefs/V1_chap03.html)).
  - So `killpg(pgid)` is safe while the root is _unreaped_. After the root is reaped it is
    safe only while another member remains.
  - tokio's `Child::id()` returns `None` "once the child has been polled to completion …
    where the OS identifier could be reused" (tokio `src/process/mod.rs:1216-1227`).
    process-wrap forwards `id()` to it (process-wrap `src/tokio/core.rs:221-222`). That makes
    `id().is_some()` a race-free, non-reaping "root not yet reaped" test.
  - **An existing exposure.** `execute_child`'s `try_join!` can reap the root while pipes
    stay open (`supervised.rs:262-276`). The error path then calls `killpg` after the reap
    (`supervised.rs:172-174`). Today this relies on the pipe holder being a same-group member.
- **No blocking in `Drop`.** `killpg` is one non-blocking syscall. A blocking `waitpid` would
  stall a runtime worker; process-wrap itself moves its blocking group wait to `spawn_blocking`
  (`process_group.rs:202`).
- **Runtime shutdown.** `Drop` may run while the runtime is dropping tasks
  (tokio `src/runtime/runtime.rs:30-37`), so the fix cannot depend on spawning a reaper task.
  A synchronous kill is sufficient for termination. The root's zombie is reaped by the orphan
  queue while the runtime lives, or by init at process exit.
- **The standalone sweep does not cover orphans.** It covers only descendants still rooted at
  the server PID (`server_terminal.rs:319-330`). The desktop host skips it
  (`overview.md:197-202`).
- **Blast radius.** `configure_supervised_background_command_wrap` is also used by long-lived
  owners: provider runtime (`apps/server/src/production/provider_runtime.rs:5484`), managed
  endpoint (`production/managed_endpoint.rs:200`), and provider terminals
  (`provider_terminal/codex.rs:2049, 3321`; `provider_terminal/opencode.rs:1942, 3835`). A
  drop kill inside the wrapper layer would change their semantics. A guard inside
  `run_supervised_with_observer` affects only one-shot supervised runs: Git
  (`apps/server/src/git/process.rs:134, 200`), `ProcessRunner`
  (`apps/server/src/process/runner.rs:224`), and provider CLI probes
  (`provider_terminal/claude.rs:1619, 1854-1862`; `codex.rs:1885-1897`;
  `opencode.rs:1788`).

## 4. Problem 2 — alternatives and recommendation

### A. Drop guard in the supervised runner (recommended)

Wrap the spawned child in a guard for the life of `run_supervised_with_observer`:

```rust
struct SupervisedChildGuard { child: Option<Box<dyn ChildWrapper>> }

impl Drop for SupervisedChildGuard {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else { return };
        // Root not yet reaped: its PID, and so the process-group id, cannot be reused.
        if child.id().is_some() {
            let mut report = ProcessCleanupReport::default();
            request_termination(&mut *child, &mut report); // killpg(SIGKILL) / job terminate
            log_cleanup_failures("dropped supervised process", &report);
        }
        // Dropping the child SIGKILLs the root (kill_on_drop) and queues it for tokio's reaper.
    }
}
```

- **Arming and disarming.** Arm right after spawn (`supervised.rs:159-163`). Disarm once
  `execute_child` returns `Ok` (the root is reaped). On `Err`, move the child into the
  existing `terminate_and_wait_owned` (`supervised.rs:172-174`).
- **Coverage.** Inline RPC drop, request-task abort, runtime shutdown, and panic unwinding —
  for every `run_supervised` caller.
- **Consistency.** Matches the Windows semantics, where handle close kills the tree.
- **For:** One small change in the process module; no RPC or contract change; the
  cancellation path stays the orderly one.
- **Against:**
  - Reaping on the drop path is best-effort, as it already is for the root today.
  - There is no "cleanup finished" ordering. Callers that need completion ordering must keep
    using the token path, as clone does for retry-after-cancel (spec lines 97-100).

### B. Child token plus drop guard per call site, or inside `run_supervised`

- **Per call site.** Apply the clone pattern: spawn the inline work with
  `cancellation.child_token().drop_guard()`, so a drop becomes a token cancel and the task
  runs `terminate_and_wait_owned` (kill, bounded wait, reap, log).
- **Generic variant.** `run_supervised` spawns its own execution task and holds only the
  guard.
- **For:** Reuses the tested orderly path, with deterministic reaping and logging.
- **Against:**
  - The per-site form has many sites (§3.3) and is easy to miss for new handlers.
  - The generic form costs a task per process and needs `'static` inputs (including the
    spawn observer, `supervised.rs:78-87`).
  - Neither covers runtime shutdown, because the spawned task is dropped too. A.'s guard is
    still needed underneath.

### C. Run every Git RPC as an owned task

- **Mechanism.** Route inline reads through `await_server_owned_rpc`
  (`git_vcs.rs:438-447`) and its Git Manager and Pull Requests equivalents, passing the
  request child token. Interrupt then reaches the runner as a cancellation.
- **For:** Consistent with the mutation pattern.
- **Against:**
  - A `'static`-cloning refactor across three adapters.
  - Reads briefly outlive their RPC.
  - Does not cover runtime shutdown or request-task abort.

A fourth variant was considered and rejected: change `run_unary` to keep polling the handler
after sending the Interrupt exit. That alters the prompt-Interrupt contract and the teardown
join (`session.rs:798-814`), and still misses shutdown.

### Recommendation

Implement **A** as the mandatory safety net, and keep the token path as the orderly one. Use
B or C selectively only where completion ordering matters; clone already does. Harden the
existing error path by gating its `killpg` on `child.id().is_some()` too, or record the
exposure in §3.4 as an accepted residual. Then rewrite the Unix-leak invariant
(`rpc-and-orchestration.md:1935-1940`) and drop the residual from the spec (line 114-116).

Tests that prove it:

1. **Process-runner drop test** (`apps/server/tests/process_runner.rs`, next to `331-385`).
   Start `ProcessRunner.run_with_cancellation` on `write_process_tree_script` without
   cancelling the token. After the three `.ready` files exist, **drop or abort** the future,
   then call `assert_cleanup_sentinels_remain_absent`. Today it fails on Unix because the
   child and grandchild write `survived`. It passes on Windows (job object) and after the fix.
2. **Guard unit tests** in `supervised.rs`, using the existing `TrackingChild`/`PendingChild`
   fakes (`supervised.rs:541-760`):
   - Dropping an armed guard calls `start_kill` exactly once.
   - A disarmed or completed guard does not.
   - A child whose `id()` is `None` is not signalled.
3. **RPC-level Interrupt test** (`apps/server/tests/production_git_manager_rpc.rs`). Mirror
   `interrupting_a_clone_stops_git_and_removes_the_destination_it_created`
   (`production_git_vcs_rpc.rs:2238-2321`) for `gitManager.getRemoteTags`: stalled loopback
   remote, accept the helper's connection, send `Interrupt`, expect the interrupt exit
   (`2323-2335`), then `wait_for_connection_close` (`2337`). The OS-level reproduction
   (Appendix A) predicts that today it times out; this test is the proof. With the fix it
   sees EOF.
4. **Teardown variant.** Same fixture, but close the WebSocket instead of interrupting
   (covers `session.rs:791-814`).
5. **Optional Linux zombie check** in a re-exec'd test with `PR_SET_CHILD_SUBREAPER`, using
   the pattern at `git_vcs.rs:5008-5073`. After the drop, the group is empty and the root's
   `/proc/<pid>` disappears within a bound.

## 5. How the two fixes interact

- Problem 2's guard makes a dropped clone task (runtime shutdown, abort) kill Git's whole
  tree. Without it, Problem 1's detached clone would leave `index-pack` or helpers writing
  into a destination that no cleanup will remove.
- Problem 1's cancel-then-drain gives the orderly shutdown the guard cannot: the guard kills,
  but only the owned task removes the partial destination.
- After Problem 1, a socket drop no longer cancels the clone transfer. It still ends the
  _wait_, so the waiter's own inline Git work, if any, falls under Problem 2's guard.

## 6. Documentation and runbooks an implementation must update

- `docs/architecture/rpc-and-orchestration.md:1931-1940`: the cancellation invariants (clone
  handoff; Unix drop leak).
- `docs/architecture/overview.md:152-171`: the clone lifecycle paragraph.
- `docs/architecture/connection-runtime.md:450-456`: **Cancel clone** would use
  `vcs.cancelClone` instead of the abort signal alone.
- The spec's residuals (spec lines 112-119).
- The contracts' RPC wire fixtures and manifest (`packages/contracts/fixtures/rpc-wire/`)
  for the new field, RPC, and capability.
- Per `AGENTS.md` ("Testing Runbook Maintenance"), process cancellation and cleanup changes
  require reviewing `docs/testing/cross-platform-validation.md` and the execution-report
  template.

## 7. Open questions

1. **Orphan policy.** How long may a clone run with nobody attached before it is cancelled?
   The proposal is 15 minutes. Orca instead lets it finish unbounded. The cost is disk and
   network use, against losing a long clone after a laptop sleep.
2. **Server-side registration.** Should the server register the project on success, as Orca's
   `store.addRepo` does? That makes the outcome durable without re-attach, but moves
   default-model resolution server-side.
3. **Closing the dialog.** Should closing it cancel (today's safe default,
   `useAddProjectWorkflow.ts:258-266`) or continue in the background with a notification?
   That is a `UI.md` "keep defaults safe" versus "preserve user work" decision.
4. **Other transfers.** Should the Git Manager's cancellable transfers (`runOperation`) and
   the token-derived `vcs.pull`/stacked pushes stop dying on reconnect too?
   Alternative B would generalize.
5. **Cancel key.** `vcs.cancelClone` by destination (the authority is the path) or by an
   operation id (needs the id on the wire)?
6. **Crash cleanup.** A durable receipt with an identity-checked startup sweep, cloning into
   a temporary sibling and renaming, or keep refusing leftovers with the actionable message?
7. **Liveness.** One missed 5-second pong ends the socket (`RpcClient.ts:1168-1195`). Orca
   tolerates three. Effect's pinger has no knob, so tolerance would need a protocol wrapper
   or a server-side keepalive policy.
8. **Error-path hardening.** Gate `killpg` after a reap on `id()` as well (§3.4), trading a
   possible surviving same-group straggler for zero reuse exposure? _Resolved 2026-09-24
   (batch-1 drop guard): the error path stays ungated, because a descendant still in the
   group keeps its id reserved; a pin test covers it. The drop guard itself signals only an
   unreaped root._
9. **Duplicate projects.** Two windows that clone into one destination both register a
   project (`addProjectOperations.ts:108-144`). Does `project.create` dedupe by workspace
   root? This predates the clone fix and was not verified. _Resolved 2026-09-24: it does;
   `project.create` returns the existing project for the same canonical root
   (`apps/server/src/orchestration/engine.rs:2913-2955`)._
10. **Destination keys.** Keys are lexical (`repository.rs:6901`). Canonicalize the existing
    parent before joining the leaf, so symlinked aliases share one registry entry?

## Appendix A — reproduction script

Python 3.11+, git 2.55.0. It signals only processes it started.

```python
import os, signal, socket, subprocess, time

def group_members(pgid):
    out = subprocess.run(["ps", "-eo", "pid=,ppid=,pgid=,stat=,args="], capture_output=True, text=True).stdout
    rows = [line.split(None, 4) for line in out.splitlines() if line.strip()]
    return ["  ".join(r) for r in rows if len(r) >= 4 and r[2] == str(pgid)]

listener = socket.socket(); listener.bind(("127.0.0.1", 0)); listener.listen(4)
port = listener.getsockname()[1]
env = dict(os.environ, GIT_TERMINAL_PROMPT="0", GIT_CONFIG_NOSYSTEM="1")
root = subprocess.Popen(["git", "-c", "http.proxy=", "ls-remote", f"http://127.0.0.1:{port}/stalled.git"],
                        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                        env=env, process_group=0)          # like ProcessGroup::leader()
conn, _ = listener.accept(); conn.settimeout(5.0)
request = b""
while b"\r\n\r\n" not in request:
    request += conn.recv(4096)                             # drain the helper's HTTP request
pgid = os.getpgid(root.pid)
print(group_members(pgid))
os.kill(root.pid, signal.SIGKILL); root.wait()             # like tokio kill_on_drop + reaping
time.sleep(0.5); print(group_members(pgid))                # helpers survive, reparented
conn.settimeout(1.0)
try: print("EOF" if conn.recv(1) == b"" else "data")
except socket.timeout: print("still open")                 # observed: still open
os.killpg(pgid, signal.SIGKILL)                            # like start_kill()
conn.settimeout(2.0); print("EOF" if conn.recv(1) == b"" else "data")  # observed: EOF
```
