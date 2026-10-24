# Research — remote chat stalls in "Working" and never answers (2026-10-05)

Worktree `main-2` at `df40c0d5`. Line numbers drift; re-verify before editing.
This is a historical research note (see [`docs/plans/README.md`](../../README.md)).
Folder choice: `remote-servers/` matches the reported trigger (saved remote
environments). The strongest finding turned out to be server-side delivery
state, which overlaps `message-queue/research/`; this note is filed here because
the brief is about the remote symptom.

Reported symptom: on a remote environment the chat panel loses connection and
does not recover; the thread stays **Working**; after **Stop** a new message
never gets a response.

Evidence sources: source and tests in this worktree, living docs, and read-only
inspection of this machine's desktop server state
(`~/.bibcode/userdata/logs/server.log`, `server.trace.ndjson`,
`logs/provider/events.log`, and `state.sqlite` opened with
`sqlite3 -readonly "file:…?mode=ro"`; only IDs, states, counts and timestamps
are quoted). No source file was changed.

## Confirmed root cause for mechanism 1 (added after reproduction)

**A message that starts with a Claude slash command (skill or command, e.g.
`/diagnosing-bugs …`) is never acknowledged.** The Claude CLI, run with
BiBCode's flags (`--print --input-format stream-json --output-format
stream-json --replay-user-messages …`, `provider_runtime.rs:11991-12008`),
replays the _expanded_ command (`<command-message>…</command-message>
<command-name>/…</command-name>…`) instead of the text BiBCode wrote.
`ClaudeAcknowledgements::acknowledge` matches the replayed first text block
(`claude_replayed_user_text`, `provider_runtime.rs:10349-10374`) by exact
string against the registered text (`provider_runtime.rs:10313-10327`), so it
never resolves, and the post-write wait has no deadline
(`provider_runtime.rs:12575-12595`). The delivery stays `sending`, holds a
delivery slot, keeps the chat **Working**, and blocks every later message on
the thread; **Stop** does not end the wait.

Evidence:

- Both stuck rows in the live database start with a slash command
  (`fc9c723e/e0212e44` → `/diagnosing-bugs`, `fc9c723e/510ab756` →
  `/superpowers:receiving-code-review`; text inspected only for its leading
  token).
- Differential loop (deterministic, 3/3 runs): `scratchpad/claude_echo_loop.py`
  spawns the CLI with the flags above and writes one user message exactly as
  BiBCode does (`provider_runtime.rs:12646`):

  | Input                               | Replayed first text block                                                                             | Exact match |
  | ----------------------------------- | ----------------------------------------------------------------------------------------------------- | ----------- |
  | `Reply with exactly PONG.`          | same 24 characters                                                                                    | yes         |
  | `/caveman Reply with exactly PONG.` | `<command-message>caveman</command-message>\n<command-name>/caveman</command-name>…` (134 characters) | **no**      |

- A second, independent reproduction in the browser (remote environment behind
  a fault-injecting proxy): stopping the remote Codex process (`SIGSTOP`)
  mid-turn reproduces the same user-visible symptom — **Working** forever,
  **Stop** has no effect, the follow-up never sends — and it clears only when
  the provider resumes. This confirms mechanism 4's "interrupt waits for the
  provider with no deadline". Socket faults (silent freeze, reset, 150 s
  outage) and a server kill/restart all **recovered**: the transport is not the
  cause.

Fix direction (not implemented here): acknowledge a slash-command echo
(recognise the `<command-name>` expansion for the registered text, or
acknowledge by turn start rather than by text), and give the post-write
acknowledgement and the interrupt path a deadline that settles the delivery as
uncertain and retires the provider session instead of waiting forever.

## Summary — ranked mechanisms

| #   | Mechanism                                                                                                                                                                | Layer                                      | Confidence                                      | Fits "Working forever"                   | Fits "new message never answered"                          | Fixed by reconnect? | Fixed by reload?         |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------ | ----------------------------------------------- | ---------------------------------------- | ---------------------------------------------------------- | ------------------- | ------------------------ |
| 1   | Claude delivery acknowledgement never matches, so the outbox row stays `sending` forever; the thread's later rows are never attempted                                    | server (provider runtime + delivery queue) | **High — observed live on this machine**        | yes (`activeDelivery` = `sending`)       | yes (newer rows blocked, then refused and retried forever) | no                  | no (server restart only) |
| 2   | A stream failure the client cannot decode (`RpcOutboundAdmissionError`, or any `Defect`) kills the shell/thread subscription fiber permanently while the socket stays up | client runtime + server RPC session        | Medium-high (code-confirmed, not log-confirmed) | yes (frozen detail/shell)                | yes (server answers, client never shows it)                | no                  | yes                      |
| 3   | `thread_stream` ignores broadcast `Lagged` instead of re-snapshotting, so a slow remote consumer can lose the thread's final state                                       | server orchestration RPC                   | Medium (code-confirmed)                         | yes, until the next event on that thread | no (the next send repairs it)                              | yes                 | yes                      |
| 4   | Provider supervisor is a single actor; Codex `turn/interrupt` and other JSON-RPC requests have no timeout; the projected session is only cleared by provider events      | server provider runtime                    | Medium-low (code-only)                          | yes (`session.status = running`)         | yes (starts wait while running/starting)                   | no                  | no                       |
| 5   | Transport churn: client liveness monitor declares death in one wake after a delayed timer; 30-minute periodic 4408 closes in the logs                                    | client runtime                             | Medium for churn, low for "never reconnects"    | indirectly                               | no                                                         | n/a                 | n/a                      |
| 6   | Shell subscription ends after a typed failure and is not retried until the next reconnect                                                                                | client runtime                             | Low-medium                                      | yes (shell owns `session`)               | no                                                         | yes                 | yes                      |
| 7   | Client request admission (64 slots per socket) can queue unary commands behind long-lived streams                                                                        | client runtime                             | Low (no evidence the limit is reached)          | possibly                                 | yes (send/stop RPCs queue silently)                        | yes                 | yes                      |

The transport itself recovers. The server log shows 246 client liveness closes
(4408) and 25 server heartbeat reaps since 2026-09-28. Each is followed by new
sessions, so half-open sockets are detected on both sides within about a minute
(mechanism 5). The chat that stays "Working" while connected is best explained
by mechanism 1. Mechanism 1 is reproduced in the live database now, and an
identical earlier incident matches the `thread.turn-delivery.resolve` invariant
failures quoted in the brief. Mechanisms 2 and 3 are the remote-specific ways
the client view can go stale while the socket stays up.

Field triage, cheapest first:

- Compare the sidebar pill with the chat panel. Mechanism 1 leaves the sidebar
  idle and the chat **Working**. The sidebar uses only `session.status`, while
  the chat also counts a `pending`/`sending` delivery.
- Reload the page. Mechanism 2 clears and shows the server's answer; mechanism
  1 does not.
- Query the outbox: `SELECT thread_id,message_id,state,attempts,updated_at FROM
  provider_turn_outbox WHERE state NOT IN ('delivered','dismissed');`

## Evidence per mechanism

### 1. Claude acknowledgement wait without timeout wedges the thread's delivery queue

**Live instance (queried 2026-10-05 ~22:50Z):**

```
thr      msg      provider_kind state   attempts created_at               updated_at
fc9c723e e0212e44 claudeAgent   sending 1        2026-10-05T15:40:25.163Z 2026-10-05T15:40:25.322Z
fc9c723e 510ab756 claudeAgent   pending 0        2026-10-05T16:41:28.855Z 2026-10-05T16:41:28.855Z
```

- `projection_thread_sessions` for `fc9c723e`: `status=ready`, updated
  `2026-10-05T15:44:43.213Z`. `provider_session_runtime`: `ready`.
- `logs/provider/events.log` for `fc9c723e`: 1,128 events from 15:40:58 to
  15:44:43.166, ending with `turn.completed completed turnId 7bb8c644…`. Claude
  received and finished the turn, but BiBCode never recorded the delivery as
  `delivered`. The newer message has had zero attempts for six hours.

**Earlier instance — this is the thread in the brief's log lines** (`02e48ee2`):

```
msg      state     attempts created_at   updated_at
63208eae dismissed 1        18:52:31.820 2026-10-03T19:15:59.031  (first Stop: dismissed)
b67cd877 dismissed 0        19:05:46.879 2026-10-03T19:15:58.515
16dc3300 dismissed 0        19:14:05.775 2026-10-03T19:16:00.530
bb449f21 pending   36171    19:16:09.276 2026-10-05T22:50:54Z     (message sent after Stop)
```

- The provider log shows turn `43f276cd…` running from 18:53:01 and completing
  at 18:58:30 and again at 19:02:00 (same turn id). The session projection
  became `ready` at `2026-10-03T19:02:00.683Z`. Even so, `63208eae` stayed
  undelivered until the user dismissed it, and the rows created at 19:05 and
  19:14 were never attempted.
- The trace shows two `orchestration.dispatchCommand` failures at
  19:15:59.248 and 19:15:59.438: `thread.turn-delivery.resolve: Message
  '63208eae…' does not have a cancellable, uncertain, or failed delivery`.
  The first dismissal had already committed at 19:15:59.031, so these were
  repeated Stop/dismiss clicks against a row that was by then `dismissed`.
- The message sent after Stop (`bb449f21`) has been retried 36,171 times over
  two days (about one attempt every 5 s) and is still `pending`. Its current
  `last_error` is `invariant failed (thread.session.set): Thread '02e48ee2…' is
  deleted`. The thread was deleted later, and `server.log:522` (2026-10-05
  12:21:19Z) shows startup reconciliation failing on the same deleted thread.

**Code path.**

- The Claude driver registers an acknowledgement keyed by the exact prompt
  text, writes the line, then waits. It leaves the wait only on an
  acknowledgement, a session-retire token, or Claude's output closing. There is
  no deadline
  (`apps/server/src/production/provider_runtime.rs:12547-12552`, `:12578-12596`):

  ```rust
  let outcome = tokio::select! {
      biased;
      result = acknowledgement_rx => match result { Ok(turn_id) => Accepted { turn_id }, … },
      () = acknowledgement_cancellation.cancelled() => Ambiguous { … },
      () = self.output.cancellation.cancelled() => Ambiguous { … },
  };
  ```

- An acknowledgement fires only when Claude replays a `user` line whose first
  text block equals the registered key byte for byte (`:10313-10327`,
  `:10352-10372`). Lines that arrive through the authenticated-hook path are
  never acknowledged (`:13482-13484`). The acknowledgement token is reset only
  by `start_session` and `restore_from_snapshot`
  (`provider/claude/runtime.rs:1562-1568`, `:2281-2283`). A turn that completes
  and leaves the session idle therefore does not end the wait.
- The delivery task awaits the router with no timeout
  (`production/turn_delivery.rs:1184-1186`), and the row is moved out of
  `sending` only after the router returns (`:1210-1219`). Each task holds one of
  the service's `MAX_CONCURRENT_THREADS = 4` permits for its lifetime (`:34`,
  `:801-803`, `:871-889`). Its thread stays in `in_flight_threads`, so the
  thread's newer rows are never selected (`:852-858`). Four such wedges stop
  provider delivery for **every** thread on the server.
- While that generation is active, the supervisor refuses the next delivery
  for the thread with "thread … already has provider work awaiting ordered
  completion" (`provider_runtime.rs:5062-5072`). That is retried, which accounts
  for `bb449f21`'s attempt count. `ThreadSessionStop`, runtime/interaction mode
  changes and model `ThreadMetaUpdate` are deferred behind the same generation
  (`:5560-5573`, `:4866-4884`). The one command that would retire the session,
  and cancel the wait, queues behind the wait itself.
- **Stop** does not free it. Claude's `interrupt` only writes a control request
  (`:12668-12679`). `handle_command` then updates only the runtime row
  (`:6480-6483`, `persist_entry` → `persist_runtime` at `:7974-8014`). The
  `thread.turn-interrupt-requested` projector marks only the turn row
  (`orchestration/engine.rs:6072-6097`). **Dismiss** is allowed for
  `pending|sending|uncertain|failed` (`engine.rs:5078-5082`) and removes the
  row, but the detached delivery task and its active generation remain.

**Client presentation.**

- `findActiveDeliveryMessage` returns the oldest unresolved non-queued message
  when it is `pending`/`sending` (`apps/web/src/components/ChatView.logic.ts:539-551`).
- That sets `isSendActivelyWorking` (`ChatView.tsx:1108`), which makes
  `isWorking` true (`ChatView.tsx:2762-2766`).
- The sidebar pill reads only `session.status === "running"`
  (`Sidebar.logic.ts:476-494`), so the two disagree.
- `onInterrupt` with `phase !== "running"` dismisses the last cancellable
  delivery instead of interrupting (`ChatView.tsx:5884-5901`). This produced
  the dismissals and the duplicate-dismiss invariant errors above.

**Why the text did not match is not established** (see Open questions). Both
wedged messages were the first message of a new Claude thread. Neither had
attachments, carriage returns, or leading/trailing whitespace (payload metadata
only). Two earlier messages on thread `6b329a70` were delivered normally.

### 2. Undecodable stream failure kills subscriptions permanently

- **Server side.** When a chunk cannot be admitted to the outbound queue
  (64 frames, `rpc/session.rs:39`) or to the byte budget within
  `OUTBOUND_SEND_TIMEOUT = 5 s` (`:42`, `:1394-1440`), `run_stream` ends that one
  subscription with `RpcOutboundAdmissionError` (`:1216-1228`, `:1567-1573`).
  The session stays open, and the server does not log the event.
- **Client side, contract.** No TypeScript schema declares that tag. A repo
  grep finds it only in `rpc/session.rs`. The transport-error middleware
  declares only `RpcResponseTooLargeError`
  (`packages/client-runtime/src/rpc/transportErrors.ts:9-12`), and
  `subscribeShell`/`subscribeThread` errors are
  `OrchestrationGetSnapshotError | EnvironmentRpcError`
  (`packages/contracts/src/rpc.ts:1550-1565`). `EnvironmentRpcError` is a closed
  union (`contracts/src/auth.ts:321-324`).
- **Client side, decode.** Effect's client decodes the exit with `orDie`, so
  the stream fails with a **Die** cause
  (`node_modules/.pnpm/effect@4.0.0-beta.107/…/rpc/RpcClient.ts:741-752`).
  A server `Defect` frame (sent on a handler panic, an unknown tag, the
  in-flight limit or an invalid session; `rpc/session.rs:905`, `:914`, `:955`,
  `:1067`) is worse: it calls `clearEntries(Exit.die(...))` and fails every
  pending request and stream on the client (`RpcClient.ts:294-305`,
  `:592-593`).
- **Client side, subscription.** `subscribeInSession` swallows only failures
  whose every reason is `Fail(RpcClientError)`, retries only typed `Fail`s, and
  re-raises anything else (`packages/client-runtime/src/rpc/client.ts:206-213`,
  `:271`). The thread subscription runs `forkScoped` with no restart
  (`state/threads.ts:198-206`). The shell projection is a single
  `Stream.switchMap(...).runForEach` fiber (`state/shell.ts:284-302`). In both
  cases the `switchMap` that would re-bind on the next session lives inside
  the dead fiber.
- **Effect on the UI.** The thread view freezes with its last data and flips
  between cached and synchronizing status (`threads.ts:184-196`). Reconnects
  do not repair it; only disposal of the atom does, after a 5-minute idle TTL
  (`state/threadRetention.ts:3`) or a reload. Because the merged thread takes
  `session` and `latestTurn` from the shell (`state/threadDetail.ts:34-62`), a
  dead shell freezes **Working** directly.
- **Why remote.** Slow links are where 5 s admission lapses. Every thread
  event re-sends a full thread snapshot, and every orchestration event re-sends
  a full shell snapshot (`production/orchestration_rpc.rs:906-982`). Each
  stream waits for a client `Ack` per chunk (`rpc/session.rs:1230-1245`; the
  client acks only after enqueuing, `RpcClient.ts:562-578`).
- **Contradiction with the code's own intent.** `send_stream_terminal` is
  documented as "a subscription must never end silently"
  (`rpc/session.rs:1441-1444`), but the terminal it sends is not decodable by
  the client.

### 3. `thread_stream` drops lagged events without re-snapshotting

`apps/server/src/production/orchestration_rpc.rs:965-974` (thread) compared
with `:924-929` (shell):

```rust
// thread_stream
Ok(event) if event.event.aggregate_kind == "thread" && event.event.aggregate_id == input.thread_id => { send_snapshot(…) }
Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
// shell_stream
Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => { send_snapshot(&sender, shell_snapshot(&engine, false).await) … }
```

- **Capacities.** The engine broadcast holds 128 events
  (`orchestration/engine.rs:1795`). Each stream buffers `STREAM_CAPACITY = 16`
  snapshots (`orchestration_rpc.rs:48`) and keeps only one chunk unacknowledged
  (`rpc/session.rs:1230-1245`).
- **Load.** On this machine one Claude turn produced 1,782 provider events in
  about 9 minutes on a single thread (thread `02e48ee2`, 18:53–19:02).
- **Failure sequence.** A slow remote consumer blocks the task in
  `send_snapshot`. If 128 or more events from other aggregates arrive after the
  thread's last event, that event is evicted. The task then sees `Lagged`, skips
  it, and the detail view keeps its pre-completion snapshot until the next event
  on that thread.
- **Contrast.** Unlike mechanism 2, any later send repairs it, and so does a
  reconnect (`subscribeThread` starts with a full snapshot).
- **Contradiction with the docs.** This contradicts the snapshot-first claim
  in `docs/architecture/rpc-and-orchestration.md:1674-1678`, which covers only
  the gap between snapshot and subscription, not later lag.

### 4. Provider stall or interrupt hang keeps the projected session running

- **Single actor.** One `run_supervisor` actor owns every session and awaits
  `handle_command` inline (`provider_runtime.rs:4786-4830`, `:4886-4895`).
- **No timeouts.** Codex JSON-RPC `request` has no timeout
  (`provider/codex/protocol.rs:288-290`, `:366-376`), and `turn/interrupt` uses
  it (`provider/codex/runtime.rs:1351-1374`). A test deliberately builds a
  "supervisor stalled in provider send" state
  (`provider_runtime.rs:19331-19349`).
- **RPC coupling.** The interrupt RPC awaits provider routing before it replies
  (`production/orchestration_rpc.rs:476-512`).
- **Projection.** Interrupting does not project the session out of `running`
  (mechanism 1, Stop bullet). New starts wait while the projected session is
  `running`/`starting` (`turn_delivery.rs:829-840`;
  `rpc-and-orchestration.md:1805-1820`), and queued rows are held after an
  interrupt (`rpc-and-orchestration.md:1838-1841`).
- **Status.** No log or database evidence on this machine: both wedged
  sessions are `ready`.
- **No client coupling.** The provider turn is driven entirely server-side. No
  provider lifecycle depends on a WebSocket subscriber. `run_session` cancels
  only the request tasks of the closing socket (`rpc/session.rs:781-818`).

### 5. Transport: detection works, with false positives and a 30-minute cadence

- **Client liveness.** The client counts every raw inbound frame
  (`rpc/session.ts:187-195`). After 10 s of silence it sends an RPC `Ping`, and
  after three intervals it closes with 4408 (`rpc/liveness.ts:20-24`, `:93-127`;
  `rpc/livenessProtocol.ts:283-309`). Server Pongs are dropped when the control
  lane is full (`rpc/session.rs:836-840`).
- **Retry policy.** The supervisor retries transient failures forever (1–16 s,
  then a 60/120/300 s idle ladder for unselected environments after 5 min). It
  stops only on `ConnectionBlockedError`/`ConnectionStorageChangedError`
  (`connection/supervisor.ts:35-48`, `:767-819`).
- **Server heartbeat.** The server pings every 15 s and reaps after 45 s of
  silence, with late-wake tolerance (`rpc/transport.rs:48-65`, `:137-180`). The
  writer ends the session on `Stalled`/`Closed` without a close frame
  (`:365-376`, `:525-560`).
- **Defect: one-wake death.** The client monitor has no late-wake guard. After
  a timer delay of 20 s or more (sleep, background-tab throttling, a blocked
  main thread) it advances `deadline` and counts all three intervals in one
  wake without awaiting a Pong (`liveness.ts:108-126`). A healthy socket is
  closed. Tests cover only on-time ticks (`rpc/liveness.test.ts:20-107`). This
  causes reconnect churn, not a permanent stall.
- **Logs.** `server.log` has 246 `close_code=4408`, 25 `heartbeat limit` reaps
  (silent 45.0–50.05 s), 7 `RPC writer gave up … error=Closed` and 1
  `error=Stalled` (2026-10-03T16:21:48Z). 133 of 245 intervals between
  consecutive 4408s are 1,795–1,806 s (median 1,800.87 s), around the clock,
  for example 2026-10-02 16:55:54 → 17:25:55 → 17:55:56 … 23:26:06. Each
  connection seems to die about 29.5 min after it opens. No 30-minute connection
  lifetime was found in the server or client code. Failed `assets.createUrl`
  spans occur 3–5 s _after_ some 4408s, which fits re-queries after reconnect
  (correlation only; the client's asset refresh interval is also 30 min,
  `state/assets.ts:8`).
- **Re-subscription works on the normal path.** The protocol publishes the
  disconnect, then fails pending calls with `RpcClientError`
  (`livenessProtocol.ts:325-331`). Subscriptions drain on that error and
  re-subscribe on the next session through `switchMap` (`rpc/client.ts:206-225`,
  `:284-312`). Shell and thread resync with full snapshots (thread:
  `threads.ts:157-161`; shell authority rules in `connection-runtime.md:797-805`).

### 6. Shell typed failure ends the subscription until reconnect

`state/shell.ts:284-293` passes `onExpectedFailure` without
`retryExpectedFailureAfter`. `subscribeInSession` then returns the handled
stream, which completes without retrying (`rpc/client.ts:250-256`). For
example, `OrchestrationGetSnapshotError` from a transient database error leaves
the shell non-authoritative while the socket is healthy. Thread subscriptions
retry after 250 ms (`threads.ts:203-204`).

### 7. Client admission slots

- **Client limit.** One owner admits at most 64 concurrent requests per socket,
  counting each stream for its lifetime (56 for terminal input,
  `rpc/inputAdmission.ts:21-50`). A unary request beyond that waits silently.
  Stop and send would then hang with no error.
- **Server limit.** The server also caps at 64 and sends a `Defect` above that
  (`rpc/session.rs:901-908`). Because the client frees a slot when its stream
  finalizer runs, before the server removes the request from `in_flight`
  (`:722-729`), a client near 64 can trip the server limit, which then triggers
  mechanism 2's `clearEntries`.
- **Status.** Unverified that real clients approach 64 streams; there are
  per-project worktree, VCS and terminal streams plus per-open-thread streams.

## Gaps where code and living docs disagree

- `docs/architecture/connection-runtime.md:666-669` says a session switch "ends
  the old stream and subscribes again through the new session". This is not
  true after a Die-caused failure (mechanism 2). A handled shell failure
  (mechanism 6) does recover on the next session: only the inner subscription
  completes and the outer `switchMap` in `state/shell.ts` subscribes again
  (covered by `rpc/client.test.ts` "keeps handled domain failures dormant until
  a replacement session arrives"), so it is a dormancy window, not a
  contradiction.
- `docs/architecture/rpc-and-orchestration.md:1674-1678` describes resync as
  snapshot-first with no lost commits. `thread_stream` discards `Lagged`
  (mechanism 3).
- `rpc-and-orchestration.md:118-121` and `rpc/session.rs:1441-1444` describe
  transport failures that end only one request and are decoded by every
  client. `RpcOutboundAdmissionError` is not decodable, and `Defect` affects
  all requests.
- No living doc states a bound on Claude delivery acknowledgement, on
  `turn/interrupt`, or on the delivery router. `rpc-and-orchestration.md`
  describes uncertain and failed outcomes, but a delivery that never returns
  has no documented outcome and holds a 4-slot permit.
- `docs/user/remote-access.md:229-235` and `connection-runtime.md:474-484`
  promise automatic retry. That holds in code for transient failures; the
  reported never-reconnects is better explained by stale view state than by
  the supervisor.
- `connection-runtime.md:509-520` documents liveness timing but not
  delayed-timer behavior. The server rule (late checks restart the clock,
  `rpc-and-orchestration.md:95-98`) has no client equivalent.
- Thread deletion leaves `pending` outbox rows retrying against an invariant
  failure forever (`bb449f21`: 36,171 attempts). Startup reconciliation also
  keeps retrying the deleted thread's runtime row (`provider_runtime.rs:2063-2068`).

## Reproduction ideas (distinguishing fault injections)

| Injection                                                                                                                                                | Expected if this mechanism is the cause                                                                                                                                                                                                                                                                                                                                                                                                                                                    | Distinguishes                                                         |
| -------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------- |
| **Claude ack miss.** Claude fixture that runs a turn but never replays the `user` line, or replays it altered.                                           | Outbox row stays `sending`. Session `ready`, sidebar idle, chat **Working**. While the acknowledgement wait is active, the next send stays `pending` with **zero** attempts (`fill_available_slots` in `turn_delivery.rs` skips threads in `in_flight_threads`), matching the live row; attempts climb ("already has provider work…") only in later retry scenarios after the wait ends without settling. Stop does nothing. A fifth concurrent wedge on four threads blocks all delivery. | 1 vs 2/3: reload and reconnect do not help. Database shows `sending`. |
| **Admission timeout.** Hold the per-connection outbound budget, or block the writer sink 6–19 s (below the 20 s `Stalled` limit) while a thread streams. | Client logs a Die on `subscribeThread`/`subscribeShell`. The socket stays connected and the view freezes. A later forced reconnect does **not** unfreeze it; reload or 5 minutes away does.                                                                                                                                                                                                                                                                                                | 2 vs 3.                                                               |
| **Server defect.** Panic one unrelated RPC handler (or send an unknown tag).                                                                             | Every stream on that client dies at once; the socket stays up.                                                                                                                                                                                                                                                                                                                                                                                                                             | 2 (Defect variant).                                                   |
| **Ack stall plus noise.** Withhold client `Ack`s for one thread stream during a busy turn while another thread emits more than 128 events, then release. | Thread detail lacks the final delivery or turn state until the next thread event. A reconnect fixes it.                                                                                                                                                                                                                                                                                                                                                                                    | 3.                                                                    |
| **Interrupt hang.** Codex fixture that never answers `turn/interrupt`.                                                                                   | Interrupt RPC hangs. All threads' provider commands queue behind it. Session stays `running`.                                                                                                                                                                                                                                                                                                                                                                                              | 4.                                                                    |
| **Silent link.** Blackhole the socket (iptables DROP / pause tunnel) for 60 s.                                                                           | Client 4408 within about 30 s, server reap within 50 s, reconnect, full snapshot. Chat recovers.                                                                                                                                                                                                                                                                                                                                                                                           | Transport baseline (5).                                               |
| **Delayed timers.** Freeze the renderer 25 s (debugger pause / `while` loop) with an otherwise healthy link.                                             | Immediate 4408 with `rpc.liveness.idle_ms` of about 25–30 s and two Pings in the same tick.                                                                                                                                                                                                                                                                                                                                                                                                | 5 false-positive.                                                     |
| **Close and reconnect.** Kill the TCP connection (RST) mid-turn.                                                                                         | `connection-lost`, reconnect, resubscribe; chat resyncs.                                                                                                                                                                                                                                                                                                                                                                                                                                   | Confirms the closed-socket path is healthy.                           |

## Open questions

1. Why did Claude's replayed `user` text not match the registered key for the
   first message of threads `02e48ee2` and `fc9c723e`?
   - Candidates: a transform in `prepare_turn_input`; a first text block that
     differs from the prompt; the replay arriving through the
     authenticated-hook path (`provider_runtime.rs:13482`); or the replay
     arriving before registration on a fresh session.
   - What would settle it: a capture of Claude's raw stdout for one wedged turn.
2. What produces the 1,800.9 s period of client 4408 closes?
   - The server log does not record peer address or client metadata, so local
     webview and remote client cannot be told apart.
   - Candidates outside this repository: a relay, tunnel or NAT with a
     30-minute flow lifetime that blackholes rather than closes.
   - Logging `current_client_metadata` and the transport kind on session end
     would settle it.
3. Do production clients ever hit `RpcOutboundAdmissionError` or a `Defect`?
   - The server does not log either.
   - Client logs ("Durable RPC subscription lost its transport" covers only the
     `RpcClientError` path) would not show the Die path either.
4. Was the brief's "loses connection" observation a real socket loss? Mechanism
   1 shows **Working** with a healthy connection, so client-side connection
   status at the time is needed.
5. The second `turn.completed` for the same turn id (`43f276cd…`, 18:58:30 and
   19:02:00) suggests a continuation under one turn id. Does that interact with
   `hasServerAcknowledgedLocalDispatch`, which compares `latestTurn` fields
   (`ChatView.logic.ts:715-771`)?
