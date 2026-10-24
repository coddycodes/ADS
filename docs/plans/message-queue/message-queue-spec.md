# Message Queue — Specification

Date: 2026-09-22. Status: **approved by the requester** (design interview of
2026-09-22: server-owned queue via the delivery outbox; both exits, auto-send
when the turn ends and an explicit Steer into the running turn; everything
else accepted as recommended). This document is the authored source of truth
for scope; `message-queue-plan.md` is the architecture and implementation
plan that argues from it. It follows the shape and rules of
`docs/plans/pull-requests/pull-requests-spec.md`.

Evidence base, all in `research/`: `upstream-queue.md` (the feature as the upstream project
ships it and what BibCode changes), `bibcode-send-path.md` (composer, busy
state, timeline, drafts, tests), `bibcode-turn-lifecycle.md` (command entry,
outbox, delivery loop, drivers, projector, provider steer facts verified
upstream), `chat-view-comparison.md` (the wider gap list, out of scope here).

---

## 1. Outcome

While a provider turn is running, the user can keep composing. Pressing
Enter **queues** the message instead of interrupting or silently steering.
The queued message appears in the timeline directly under the working
indicator as a **Queued card**: the message body, a clock icon and the word
`Queued`, a status line, a **Steer** button (arrow up) and a **Cancel** button
(x). Several messages can be queued; they are ordered first-in first-out per
thread.

A queued message leaves the queue in exactly one of three ways:

1. **Auto-send.** When the running turn ends and the session is ready, the
   oldest queued message starts the next turn. One message per settle; the
   next one waits for that turn to end.
2. **Steer.** The user presses Steer on the head card (or `Mod+Shift+Enter`).
   The message is handed to the provider immediately and injected into the
   running turn at the provider's next boundary. It becomes part of that turn.
3. **Cancel.** The user presses Cancel. The message is withdrawn and its text
   and attachments return to the composer of the client that cancelled it.

The queue is **server-owned and durable**: it survives page reloads, client
disconnects and server restarts, and every connected client (browser,
desktop, remote) sees and can act on the same queue.

This also closes a correctness hole: today Enter during a running turn
reaches the provider as an unlabelled follow-up whose effect is
provider-defined, and on Codex it re-points the active turn id so a later
Stop may target the wrong turn (`research/bibcode-send-path.md`,
`research/bibcode-turn-lifecycle.md`).

## 2. Hard constraints

- **The server owns the queue.** Queued messages are outbox rows plus their
  projected user messages. No client keeps a second queue. A client renders
  what the thread snapshot says and issues commands.
- **A queued message never looks like work.** It emits no
  `thread.turn-start-requested`, creates no pending projection turn, does not
  flip `session.status`, and never shows the sidebar "Working" pill.
- **Auto-send only into a ready session.** After `error` or `interrupted`
  the head is held and the card says so; the user chooses Steer (if a turn is
  running again), waits for the next settle, or cancels. No auto-send while
  an approval or a user-input question is pending.
- **Steer is a real provider operation, not a second turn start.** A new
  `ProviderDriver::steer` injects into the active turn. Codex calls
  `turn/steer` with the expected active turn id; Claude writes the user line
  without starting a new runtime turn. The steered message is attributed to
  the running turn. Providers without a steer path (Cursor, Grok, OpenCode)
  declare `supportsTurnSteer` false and the Steer control is hidden with the
  reason available in the card status.
- **Model and modes are snapshotted at enqueue** (model selection, effort,
  runtime mode, interaction mode) exactly as a normal turn start does today,
  consistent with the started-thread provider lock.
- **Cancel never discards user text.** The cancelling client restores the
  prompt into its composer (appending to any existing draft with a blank
  line). Attachments are restored from that client's own enqueue cache
  (the `File` objects it queued in this session), re-added up to the provider
  limit with overflow reported; when the cache has no entry (another client,
  or after a reload) the client restores the text and reports how many
  attachments it could not restore. Attachment bytes live only in the server's
  attachment store, which has no client read path in this version. Other
  clients see the card disappear.
- **Stop drains, then interrupts.** Pressing Stop first cancels every queued
  message of that thread (head to tail) and restores them into the composer
  of the client that pressed Stop, in order, joined by blank lines, and only
  then sends the interrupt. Cancelling first matters because Codex reports an
  interrupted turn as `turn/completed` and the session returns to `ready`,
  which would otherwise promote the head a moment after Stop.
- **Interrupt and error hold the queue.** When the server applies
  `thread.turn-interrupt-requested`, or a session settle to `error`, every
  `queued` row of that thread is marked `held`. Held rows are never promoted
  automatically; the head card reads `Waiting for you` and offers **Send
  now** (start a new turn with it) and Cancel. Send now clears the hold on
  that row only. This covers a client that crashed between Stop and its
  cancels, and older clients that do not drain.
- **FIFO at the claim gate.** The delivery loop never claims a `pending/start`
  row while the thread's projected session is `running` or `starting`. A
  turn start admitted by an older client during a running turn is therefore
  held until that turn settles instead of being delivered mid-turn.
- **Recovery.** A `queued` row survives restart as `queued`. It never becomes
  `uncertain` because nothing was sent. A `steer` delivery follows the
  existing crash truth table of its driver.
- **No new dependencies** in `apps/web`, `packages/*` or `apps/server`.
- **`packages/contracts` stays schema-only.** New commands and events are
  registered in every place the repository gates and every pinned fixture
  count is re-read and bumped in the same change.
- **Living documentation ships with the behaviour** (Phase 04 plus per-phase
  notes); affected `docs/testing/` runbooks are updated or recorded as
  reviewed and unchanged.
- **Commit policy:** phases are commit-free. The coordinator commits only
  when the requester asks; every commit message carries a resume of the task.

## 3. States and transitions

`TurnDeliveryState` gains `queued`. Outbox rows gain `mode ∈ {start, steer}`
and a `held` flag.

```
enqueue (turn.start, queued=true)          → queued/start
queued/start  --settle (session ready)------→ pending/start   (server promote; emits turn-start-requested; not when held)
queued/start  --Steer (turn running)--------→ pending/steer    (turn-steer-requested; head only)
queued/start  --Send now (not running)------→ pending/start   (client promote; clears held on that row)
queued/start  --Cancel----------------------→ withdrawn        (row dismissed, message removed)
queued/*      --interrupt-requested / error → queued, held=1   (all rows of the thread)
pending/steer --driver Accepted-------------→ delivered        (message.turnId = active turn)
pending/steer --no active turn--------------→ queued/start     (falls back to auto-send)
pending/start --claim gate: session not running → sending → delivered | uncertain | failed
```

Settle means a `thread.session-set` whose status leaves `running` for
`ready`. Codex reports an interrupted turn through `turn/completed` and its
session returns to `ready`, so `ready` alone cannot mean "safe to auto-send";
the `held` flag set on interrupt or error is what stops promotion. `stopped`
and `idle` never promote.

Promotion is atomic per thread: at most one `pending/start` row may exist
for a thread at a time, and the claim gate refuses it while the session is
`running` or `starting`; the loop still serialises per thread through
`in_flight_threads`.

## 4. Commands and events

Contracts (`packages/contracts/src/orchestration.ts`):

- `thread.turn.start` gains optional `queued: boolean` (client and server
  variants). Admission resolves it: when the client asked for `queued` and
  the projected session is `running` or `starting`, admission keeps
  `queued: true`, writes the outbox row as `queued`, and the engine reducer
  records the message with delivery `queued` and emits **no**
  `thread.turn-start-requested`. Otherwise admission rewrites the flag to
  `false` before dispatch and the existing path runs (the race where the turn
  ended between keypress and receipt). No Rust-only field is added to the
  command; the resolved flag round-trips through the contract.
- New client command `thread.turn.steer { threadId, messageId }`. Valid only
  for the head `queued` row of that thread while its session is `running`
  and the provider instance declares `supportsTurnSteer`. Emits
  `thread.turn-steer-requested { threadId, messageId, turnId, createdAt }`.
- `thread.turn-delivery.resolve` gains action `cancel`. Valid only for a
  `queued` row. The outbox row becomes `dismissed`; the emitted
  `thread.turn-delivery-updated` carries `withdrawn: true` and the projector
  deletes the user message row; the client reducer removes the message.
- New command `thread.turn.promote { threadId, messageId }`, dispatched by
  the delivery loop on settle (server actor) and by a client as **Send now**
  on the head `queued` row while the session is not `running`. It emits
  `thread.turn-start-requested` for the stored message (same payload shape as
  today, from the snapshotted model selection and modes) and a
  `thread.turn-delivery-updated` to `pending`, clearing `held` on that row.
- `TurnDelivery` gains optional `mode` and `held` so the card can render
  `Steering…` and `Waiting for you` from the snapshot;
  `thread.turn-delivery-updated` carries them, plus `withdrawn` and `turnId`.
- `OrchestrationEventType` gains `thread.turn-steer-requested`.
- `ServerProvider` gains `supportsTurnSteer?: boolean` (Codex and Claude
  true; others omitted, read as false, mirroring `supportsMcpStatus`).
- New keybinding command `thread.steerQueuedMessage`, default
  `mod+shift+enter`, `when: editableFocus` in the chat composer.

Every new or changed command shape is added to
`apps/server/tests/orchestration.rs` `command_values()`; every new event
shape is exported to the wire fixtures and the pinned counts in
`packages/contracts/scripts/export-rust-rpc-fixtures.test.ts` and
`apps/server/tests/rpc_wire.rs` are bumped.

## 5. Server behaviour

- **Migration 50** recreates `provider_turn_outbox` with `queued` in the
  state check and a `mode TEXT NOT NULL DEFAULT 'start' CHECK(mode IN
  ('start','steer'))` column, and widens
  `projection_thread_messages.delivery_state` the same way. Existing rows
  keep their values.
- **Admission.** `dispatch_reserved_turn_command` writes the outbox row as
  `queued` when the command says `queued: true` and the projected session is
  `running` or `starting`; the message projection receives
  `delivery_state = 'queued'` in the same transaction.
- **Delivery loop.** `fill_available_slots` never claims `queued` rows and
  never claims a `pending/start` row while the thread's projected session is
  `running` or `starting`. On every `thread.session-set` event whose status
  is `ready` the loop is woken and, for that thread, dispatches
  `thread.turn.promote` for the oldest `queued` row that is not `held`, if
  there is no `pending`/`sending` row and no pending approval or user input
  on the thread. `pending/steer` rows are claimable only while the session is
  `running`; if the session is not running when a steer row is claimed, the
  row is rewritten to `queued/start` and the hook re-evaluates.
- **Hold.** Applying `thread.turn-interrupt-requested`, or a session settle
  to `error`, sets `held = 1` on every `queued` row of the thread in the same
  transaction and emits a `thread.turn-delivery-updated { held: true }` per
  row. `thread.turn.promote` clears `held` on the promoted row.
- **Driver steer.** `ProviderDriver::steer(text, attachments, expected_turn_id)
  -> ProviderDeliveryOutcome`, default `Rejected { detail: "provider does not
  support steering" }`. Codex: `turn/steer { threadId, input, expectedTurnId,
  clientUserMessageId }`; a precondition failure maps to `Rejected`. Claude:
  encode the user line, register the acknowledgement waiter, write, **do not
  call `runtime.start_turn`**; `Accepted { turn_id: current }`. On
  `Accepted` the message's `turn_id` is set to the active turn and the row is
  `delivered`; on `Rejected` the row returns to `queued/start`.
- **Codex active turn fix.** `send_turn` no longer overwrites
  `active_turn_id` when the session is already `running`; the id the
  provider reports as active stays authoritative for `turn/interrupt`.
- **Interrupt.** The interrupt command is unchanged, but applying
  `thread.turn-interrupt-requested` now marks the thread's `queued` rows
  `held` (see Hold). The client cancels queued messages **before** it sends
  the interrupt (§ 6); rows left behind by a crashed or older client stay
  `queued` and `held`, and their head card reads `Waiting for you`.
- **Snapshots.** Queued messages are ordinary messages in the thread
  snapshot with `delivery.state === "queued"`; no new stream or RPC method.

## 6. Client behaviour

- **Composer.** When `phase === "running"` (or the session is `starting`),
  Enter sends `thread.turn.start` with `queued: true`, clears the composer
  and draft exactly as a normal send, records the queued `File` objects in a
  session-local enqueue cache keyed by message id, and shows no optimistic
  "working" state. The Stop button stays. Queued rows do not count towards
  `isSendBusy`, so more messages can be queued. `Mod+Shift+Enter` is
  intercepted in the composer key handler before the Shift+Enter newline
  path and steers the head card when steer is available.
- **Queued card.** New timeline row kind `queued-message`, appended after
  the `working` row, one per queued message in order. Messages whose delivery
  is `queued`, or `pending` with `mode: "steer"`, are excluded from the normal
  `message` rows so they render once. Anatomy: right-aligned dashed bubble
  with the message body (same renderer as user messages), attachment count
  line when any, then a footer with clock icon, `Queued`, a status line, a
  primary action (arrow up) and Cancel (x, `aria-label="Cancel and return to
  the composer"`). The primary action is Steer (`aria-label="Steer into the
  running turn"`) while a turn runs and steer is available, and Send now
  (`aria-label="Send now"`) when the head is held or the session is idle.
  Status copy: head while running and steer available: "Sends when the turn
  ends. Steer to send it now."; head while running and steer unavailable:
  "Sends when the turn ends."; head held or session not running: "Waiting
  for you"; not head: "Sends after the messages above."; while a steer is in
  flight: "Steering…" with both buttons disabled. Pointer-down on either
  button prevents default so the composer keeps focus. Disabled controls
  carry their reason in a tooltip (UI.md "disabled states explain
  themselves").
- **Steered message rendering.** Once a steered message is `delivered` its
  `turnId` is the running turn's, and it renders as a normal user row at its
  position inside that turn. Turn-fold derivation treats a user message whose
  `turnId` matches an existing turn group as part of that group: it neither
  starts a new boundary nor ends the fold.
- **Cancel.** Sends `thread.turn-delivery.resolve { action: "cancel" }` and,
  on success, restores the text into the composer draft for that thread and
  the attachments from the enqueue cache when present, reporting any it could
  not restore. A failed cancel leaves the card and shows the error inline in
  the card.
- **Stop.** The client first cancels every queued message of the thread head
  to tail (restoring each in order), then calls `interruptThreadTurn`. A
  cancel failure does not block the interrupt; the row stays and will be
  held by the server.
- **Reducer.** `thread.turn-steer-requested` marks the message `pending`
  with `mode: "steer"` (the card shows `Steering…`); a
  `thread.turn-delivery-updated` with `withdrawn: true` removes the message,
  with `held` updates the flag, and with `turnId` on a `delivered` steered
  message sets its `turnId`.
- **Sidebar and status.** Queued messages never produce `Working`; the
  thread row shows no new pill. Unread logic ignores queued messages.
- **Keybinding.** `thread.steerQueuedMessage` appears in the keybindings
  documentation and settings like every other command.

## 7. Out of scope

Mid-turn auto-injection at tool boundaries without a user action (the upstream project's
default); a follow-up setting to invert Enter and `Mod+Enter`; editing a
queued card in place; queueing on a thread that has no session yet (a
normal send starts it); restoring attachment bytes on cancel from another
client or after a reload (needs a read path to the server attachment store,
a follow-up); the wider chat-view gaps recorded in
`research/chat-view-comparison.md`.

## 8. Acceptance

All of the following are demonstrated by tests and, for the user-visible
parts, by a WebdriverIO scenario against the packaged desktop UI with the
stub providers plus a coordinator-driven Playwright pass against the web
build with Codex and Claude:

1. Enter during a running turn produces a `queued` message and card, no
   `turn-start-requested`, no `Working` change, and the composer can queue a
   second message.
2. The head queued message starts the next turn after the running turn
   settles to `ready`; the second waits for that turn to settle.
3. Steer on the head card delivers the message into the running turn on
   Codex (`turn/steer` observed with the active turn id) and on Claude (a
   user line written without a new `turn.started`); the message is attributed
   to the running turn in the snapshot.
4. Steer is unavailable on Cursor, Grok and OpenCode; auto-send still works.
5. Cancel removes the card everywhere and restores text and, from the
   cancelling client's enqueue cache, attachments in that client's composer.
6. Stop returns every queued message to the composer in order and then
   interrupts the turn; nothing auto-sends after Stop.
7. Reloading the page mid-queue and restarting the server mid-queue both
   preserve the queue in order; a queued row after restart is still
   `queued`.
8. No auto-send while an approval or question is pending; after an
   interrupt or an `error` settle every queued row is `held`, the head card
   reads `Waiting for you`, and Send now starts it.
9. A `thread.turn.start` without `queued` admitted during a running turn
   (an older client) is held at the claim gate until that turn settles; it is
   never delivered mid-turn. The web client never sends one.
10. A steer whose turn ends before delivery falls back to `queued/start` and
    auto-sends on the next `ready` settle.
