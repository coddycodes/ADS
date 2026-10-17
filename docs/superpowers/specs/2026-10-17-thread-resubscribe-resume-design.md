# Resume a thread subscription instead of resending its snapshot

Status: **Proposed** on 2026-10-17, awaiting approval. Follows #96 and #89.

## Problem and evidence

Every reconnect resubscribes each thread the client still holds, and
`orchestration.subscribeThread` always opens with a whole snapshot
(`thread_stream` in `apps/server/src/production/orchestration_rpc.rs`). A
reconnect usually follows a dropped link, not a change: the client already holds
the thread, often unchanged.

- A 200-message test thread costs about 6.5 MB per resubscribe (measured for
  #96 with headless Chromium on a local server). The thread measured for #89
  (40,170 activities) costs 6–10 MB.
- Plain WebSockets carry that size. E2EE links deflate it before encryption
  (`2026-10-13-e2ee-message-compression-design.md`). The link then carries
  less, but the server still serializes and compresses the snapshot, and the
  client still decodes and diffs all of it.
- Every thread state the client still holds resubscribes. That includes threads
  idling within their idle TTL, not only the visible one. A desktop that closes
  on liveness every 30 minutes (the reconnect issue) pays this cost all day.
- Since #96 the client keeps unchanged objects of a resent snapshot
  (`threadDetail.ts`), so memory no longer grows. The bytes, the decode and the
  diff remain.

Goal: a resubscribe sends only what changed since the revision the client
already holds. For an unchanged thread, that is almost nothing.

## Current mechanics this builds on

- `subscribeThread` takes `{ threadId, patches? }`. With `patches: true` the
  server keeps a `HeldThread` for each subscription: 64-bit fingerprints of each
  field and array entry, keyed by a random per-subscription key. After the
  opening snapshot it sends `patch` items. The held state dies with the
  subscription.
- `snapshotSequence` is the highest `last_applied_sequence` over all projection
  states, so it is global, not per thread.
- The event store is durable and replayable (`orchestration.replayEvents`), but
  the thread stream deliberately sends projections, not events. The client's
  event reducer is not pinned against the server projection. Patches are pinned
  (`packages/contracts/fixtures/thread-stream/lifecycle.json`).

## Alternatives

### 1. Client hint: a server-issued revision digest (recommended first step)

Each `snapshot` and `patch` item gains an optional `revision`. This is a 128-bit
digest of the whole thread value after that item. The server derives it from the
field fingerprints it already computes for `HeldThread`, using a per-process
secret key instead of the per-subscription one. The client records the
`revision` of the last item it fully applied. On resubscribe it sends
`resume: { revision, snapshotSequence }`.

The server then does one of three things:

- **Fast path.** The global sequence still equals `snapshotSequence`, so
  nothing committed since. The server sends `{ kind: "unchanged", revision }`
  without loading the thread.
- **Unchanged thread.** The server loads the thread and digests it. When the
  digest equals `revision`, it sends `unchanged`.
- **Changed thread.** The server sends a whole snapshot, as today.

- Server retention: none beyond the process key.
- Cost: one targeted thread load and a fingerprint walk per resubscribe. The fast
  path skips both. The walk is what each patch already costs.
- Limit: a thread that changed while the client was disconnected still costs a
  full snapshot.

### 2. Resume cursor with retained held state (second step, if measured to matter)

The server keeps the `HeldThread` of recent revisions after a subscription ends,
keyed by `(threadId, revision)`. It holds at most a few revisions per thread and
a global byte budget, for a short TTL (for example 2 minutes). A resume that
names a retained revision gets a `patch` against it, so a thread that changed
while the client was away costs only its changes.

- Server retention: fingerprints only, never content (#101). That is about
  8 bytes per field and per array entry, roughly 2 KB for a 200-message thread.
  Eviction is bounded by count, bytes and TTL.
- It needs alternative 1's revisions. Without them, the server cannot tell which
  held state the client actually applied.
- Risk: correctness rests on revision identity. A client must never resume from
  an item it did not finish applying.

### 3. Revision diff from replayed events

The client sends its last `snapshotSequence`. The server replays the thread's
durable events since then and the client folds them with
`applyThreadDetailEvent`.

- Server retention: none. The event log is already durable.
- Rejected for now. It reopens the client-reducer drift that snapshot-plus-patch
  removed. Projection-only fields (delivery state, queue order, read models
  built by SQL) have no event-level parity tests. `snapshotSequence` is global,
  so the replay scans every aggregate's events since the cursor.

## Compatibility

- **New client, old server.** The old server ignores the unknown `resume`
  field, because `SubscribeThreadInput` does not deny unknown fields. The
  opening snapshot arrives exactly as today. Old servers also never send
  `revision`, so the client never resumes.
- **Old client, new server.** The old client sends no `resume`. It receives a
  snapshot, and `revision` is an optional field it does not read.
- **The `unchanged` item** is sent only in answer to `resume`, so no old client
  ever receives it. It is a new member of `OrchestrationThreadStreamItem`
  (contracts), plus the Rust fixture in
  `packages/contracts/fixtures/thread-stream/`.

This is a wire-protocol addition, so it needs approval before it is built.

## Failure modes

- **Gap: an item was sent but never arrived.** The client resumes from the
  revision it actually applied. The server's current digest differs, so it
  sends a full snapshot (alternative 2: a patch against that older retained
  revision). The client therefore never applies a patch against a state it does
  not hold.
- **Partial stream.** The client records `revision` only after the item has
  been decoded and applied to its state, never on receipt.
- **Server restart.** The process key changes, so every old revision
  mismatches and the server sends a full snapshot. Alternative 2's retained
  state is lost the same way. Neither adds persisted shape.
- **Storage reset or another environment.** Revisions are keyed per process and
  scoped to the thread ID. A digest from another server cannot match except by
  a 2^-128 collision.
- **Deleted thread.** A resume fails with `Thread … was not found`, as the
  snapshot does today (#113 backoff unchanged).
- **Reconnect storms.** The fast path answers without touching SQLite. Without
  it, each resubscribe costs one targeted thread load (cheaper than today's load
  plus serialization). Alternative 2's retention is bounded by count, bytes and
  TTL, so a storm cannot grow it.
- **Crafted content.** Thread content comes from agents and tools. The digest
  key is secret and per process, so content cannot be crafted to collide and
  hide an update, the same reasoning as `HeldThread`'s key.
- **Client cache restore (extension).** A thread restored from the client cache
  at start-up could resume with its cached revision only within the same server
  process. Across restarts it falls back to a full snapshot.

## Expected savings

- **Unchanged thread on reconnect.** About 6.5 MB per 200-message thread (6–10
  MB for #89's thread) drops to an `unchanged` item of under 200 bytes. The
  client skips decoding and diffing a whole thread. Server CPU drops to a
  fingerprint walk, or nothing on the fast path.
- **A thread changed while disconnected.** Alternative 1 saves nothing here.
  Alternative 2 brings it down to the size of a patch, typically one or a few
  entries.
- **Per reconnect.** Savings scale with the number of thread states held, not
  just the visible thread.

## Validation plan

- Contract fixtures: `resume` → `unchanged`, `resume` → `snapshot` on a
  mismatch, and no `resume` → `snapshot`.
- Server tests: the fast path does not load the thread. A revision from another
  process key does not match. An unapplied item followed by a resume yields a
  snapshot.
- Client tests: the revision is recorded only after the item is applied, and
  `unchanged` keeps the held thread object.
- Harness: the #96 headless-Chromium reconnect loop, counting
  `orchestration.subscribeThread` bytes per reconnect, before and after.

## Decision requested

Approve alternative 1, with alternative 2 deferred until measurements show
changed-while-disconnected resubscribes matter. Or choose another alternative.
