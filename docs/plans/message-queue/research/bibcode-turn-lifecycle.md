# Research — BibCode server turn lifecycle (2026-09-22)

Worktree `main-3` at `4d57551f`. Line numbers drift; re-verify before editing.

## Command entry

Every thread mutation is `orchestration.dispatchCommand`
(`apps/server/src/production/orchestration_rpc.rs:115`; operate scope,
`auth/scope.rs:87`). `ThreadTurnStart` branches early (`:127`) →
`dispatch_turn_command` (`:459`) → `dispatch_reserved_turn_command` (`:528`):
attachments materialised (`:541`), `turn_identity` (`:626`), then the command
and a durable outbox row are committed in one transaction
(`CommandAdmission { provider_turn: Some(NewProviderTurnDelivery{…}) }`,
`:562-572`) and `turn_delivery.wake()` is called (`:581`).

Engine reducer for `ThreadTurnStart` (`orchestration/engine.rs:3927-4045`)
emits `thread.message-sent` (user, `turnId: null`) and
`thread.turn-start-requested`; it checks only thread existence, never whether
a turn is running. `InternalOrchestrationCommand`
(`packages/contracts/src/orchestration.ts:973-982`) is the precedent for
server-originated commands.

## Persistence

`apps/server/src/persistence/migrations.rs:619` `MIGRATIONS` (49 entries).
Tables: `orchestration_events`, `orchestration_command_receipts`,
`projection_threads`, `projection_thread_messages` (with `delivery_state`,
`delivery_provider`, `delivery_detail`), `projection_thread_sessions`,
`projection_turns`, and `provider_turn_outbox` (migration 39, `:2168`):

```
command_id PK, thread_id, message_id (unique), provider_instance_id,
provider_kind, provider_session_id, delivery_key, payload_json,
state CHECK IN ('pending','sending','delivered','uncertain','dismissed','failed'),
attempts, last_error, created_at, updated_at
```

Repositories: `list_provider_turn_deliveries` (`repositories.rs:619`),
`claim_provider_turn` (`:673`), `freeze_provider_turn_session` (`:701`),
`get_thread_session` (`:869`), `replace_pending_turn_start` (`:949`).

## Delivery loop

`apps/server/src/production/turn_delivery.rs`: `wake: Arc<Notify>` (`:62`),
`run` (`:421`), `fill_available_slots` (`:757-830`) loads
`Pending | Sending | Uncertain | Failed`, computes retry backoffs, takes
`claimable_oldest_per_thread` (`:925`; pending rows only, oldest per thread),
skips `in_flight_commands` and `in_flight_threads`, then
`prepare_claim_and_deliver` (`:932`) → `execute_bootstrap_prerequisites` →
`claim_provider_turn` → `deliver_claimed`. `in_flight_threads` is released in
`DeliveryCompletion` handling (`:857`), i.e. when the **provider accepted**
the prompt, not when the turn ends. Outcome transitions go through
`engine.transition_turn_delivery` (`:1090-1140`) with a
conflict-tolerant loop.

## Provider drivers

`ProviderDriver` trait (`production/provider_runtime.rs:282`): `start`,
`send`, `deliver` (default wraps `send`), `reconcile`, `interrupt`,
`approve`, `answer`, `set_mode`, `set_interaction_mode`, …
`ProviderDeliveryOutcome` (`:254`): `Accepted { turn_id } |
DefinitelyNotSent | Ambiguous | Rejected`.

- **Codex** driver at `:5616`; runtime `provider/codex/runtime.rs:1186`
  `send_turn` sends `turn/start` and then **overwrites**
  `session.active_turn_id` with the returned id (`:1224-1225`).
  `interrupt_turn` (`:1278`) uses `active_turn_id`. Upstream
  `turn/steer` (added 2026-02-05, `openai/codex#10821`;
  `codex-rs/app-server-protocol/src/protocol/v2/turn.rs`): `TurnSteerParams {
  threadId, clientUserMessageId?, input: UserInput[], expectedTurnId }`
  ("Required active turn id precondition. The request fails when it does not
  match the currently active turn."), `TurnSteerResponse { turnId }`.
  `turn/start` while a turn is active also steers ("Ignored when this request
  steers an already-active turn"). Installed `codex-cli 0.155.1` is the
  latest release.
- **Claude** driver at `:8704`; `deliver` (`:8750-8830`) creates a fresh
  `turn_id`, encodes a `{"type":"user",…}` line, registers an acknowledgement
  waiter, calls `runtime.start_turn(TurnInput{turn_id,input})`
  (`provider/claude/runtime.rs:1594`, which resets `current_turn_id` and
  emits `turn.started`) and writes the line. A mid-turn deliver therefore
  re-labels the running turn. Claude's streaming input mode documents
  "Queued messages: send multiple messages that process sequentially, with
  ability to interrupt"
  (code.claude.com/docs/en/agent-sdk/streaming-vs-single-mode); installed
  Claude Code 2.1.278.
- **Cursor** (`:5922`), **Grok** (`:6168`), **OpenCode** (`:6415`): no
  steer path; `deliver` rejects only on transport or encoding errors.

Two facts that shape hold and acknowledgement handling:

- Codex maps an interrupted `turn/completed` to session status `ready`
  (`provider/codex/runtime.rs:2496-2510`; only `failed` becomes `error`), so
  a `ready` settle right after Stop is normal and cannot by itself mean
  "safe to auto-send".
- The Claude driver acknowledges a delivery write when any non-hook
  `type: user` line arrives on stdout (`emit_claude_value`,
  `provider_runtime.rs:9636-9640`); during a running turn tool-result lines
  are also `type: user`, so a steer acknowledgement must not accept those.

Provider capability flags are produced in
`production/provider_inventory.rs` (`supportsMcpStatus` at `:1595`, set for
Codex and Claude only) and declared in `packages/contracts/src/server.ts:172`
`ServerProvider` (`supportsMcpStatus`, `supportsContextWindowUsage`, …).

## Turn state machine

`OrchestrationLatestTurnState = running | interrupted | completed | error`;
`OrchestrationSessionStatus = idle | starting | running | ready | interrupted
| stopped | error`. Projector `orchestration/engine.rs:5546`:
`thread.turn-start-requested` deletes the previous `turn_id IS NULL` row and
inserts a `running` placeholder; `thread.session-set` promotes/settles
(`:5561`, `:5605`); `thread.message-sent` (assistant) settles when not
streaming and not running (`:5615-5639`); `thread.turn-interrupt-requested`
→ `interrupted` (`:5663`). `thread.turn-delivery-updated` projector (`:5302`)
updates `projection_thread_messages.delivery_*`. Delivery resolution
(`:4700-4760`): `retry` allowed from `uncertain | failed`, `dismiss` from
`pending | sending | uncertain | failed`; emits `thread.turn-delivery-updated`.

Clients get full re-snapshots on every durable event
(`orchestration_rpc.rs:783-826`); client reduction in
`packages/client-runtime/src/state/threadReducer.ts` (`thread.turn-start-requested`
at `:152`, `thread.message-sent` at `:190`).

## Existing "run it when current work finishes" pattern

`DeferredConfigurations` in the supervisor (`provider_runtime.rs:2710`):
configuration commands arriving during an active delivery are queued
(capacity-bounded) and drained by `SupervisorMessage::DrainDeferred`
scheduled from `DeliveryComplete` via `schedule_deferred_drain` (`:2690`).

## Tests

`apps/server/tests/orchestration.rs` (`command_values()` at `:22` lists every
command shape), `production_orchestration_rpc.rs:461`,
`production_provider_runtime.rs`, `turn_delivery_recovery.rs` (crash/restart
truth table at `:660`), `rpc_wire.rs:70-85` (pinned counts: 131 methods,
288 typed failures, 380 fixtures, 358 fingerprints, 23 orchestration event
shapes in `packages/contracts/scripts/export-rust-rpc-fixtures.test.ts:101-116`).

## Docs to update

`docs/architecture/rpc-and-orchestration.md` § Provider turn flow (`:1220`)
and § Invariants (`:1558`); `docs/architecture/providers.md` § Execution path
(`:20`); `docs/architecture/overview.md` § Request and event flow;
`docs/user/workspace-ui.md` composer paragraphs (`:239-248`);
`docs/user/keybindings.md`.
