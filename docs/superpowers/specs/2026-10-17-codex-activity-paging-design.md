# Codex activity history paging

Status: **Approved** on 2026-10-17 (the follow-up recommended in #120).

## Goal

Codex activity reconciliation recovers a thread's recent history without
asking Codex for the whole history. Until now every reconciliation read of the
root thread and of each descendant was `thread/read` with `includeTurns: true`,
which serializes every turn and item of the thread into one JSON-RPC line. A
long thread makes that line large without bound. Activity keeps only the
newest 20 turns and 200 items of it.

## Decision

History comes from `thread/turns/list`, newest first. `thread/read` stays, but
only as a metadata read with `includeTurns: false`, because `thread/turns/list`
carries no thread metadata (`id`, `parentThreadId`, `createdAt`, `updatedAt`,
`status`), and reconciliation needs that metadata for topology, versions, and
status.

For each thread read by a reconciliation pass:

1. `thread/read` `{threadId, includeTurns: false}` returns metadata. A reply
   for a different thread id is handled as today (ignored for that thread) and
   no turns are requested.
2. `thread/turns/list` `{threadId, limit: 20, sortDirection: "desc",
   itemsView: "full"}`, plus `cursor` on later pages, returns turns until a
   stop condition holds.
3. The collected turns are put back in chronological order, attached to the
   metadata as `thread.turns`, and decoded by the existing bounded
   `decode_thread_read_response` (keeps the newest 20 turns and 200 items,
   reduces command output to a presence marker, then applies the 512 KiB
   bound).

Durable delivery reconciliation (`delivery_exists`) also pages:
`thread/turns/list` with the same parameters, scanning each page newest first
for the delivery key's `userMessage.clientId`. It stops at the first match and
reports absent only after the last page. Nothing is retained between pages, so
no size bound applies to the scan. The transport's line limit already bounds
each page.

Rejected alternatives:

- **Full `thread/read`, trimmed after decode** (the #120 fix alone). It keeps
  the reply unbounded and transfers and parses the whole history on every
  pass.
- **`thread/items/list`.** It is item-granular and would bound replies by item
  count. In Codex 0.151 it is served only by the paginated thread store: for
  legacy threads the store returns `Unsupported`, which the app server maps to
  `method_not_found("thread/items/list is not supported yet")` (-32601). Under
  the #120 classification that would read as a missing method, and it would
  split recovery into two code paths by thread format.
- **`itemsView: "summary"`.** It returns only the first user message and the
  final agent message of each turn. Activity needs command, tool, reasoning,
  and `subAgentActivity` items.

## Page size and stop conditions

Activity needs the newest **20 turns** (`CODEX_RECONCILIATION_TURN_LIMIT`,
`MAX_RECONCILED_TURNS`) and, across them, the newest **200 items**
(`CODEX_RECONCILIATION_ITEM_LIMIT`, `MAX_RECONCILED_ENTRIES`). Older turns
inside the 20 keep their turn status for completion entries even when their
items fall outside the 200.

The page size is **20 turns**, so one request normally returns exactly what
activity keeps. Paging continues only when Codex returns a short page with a
cursor. Stop when any of these holds:

- 20 turns are collected;
- `nextCursor` is absent, empty, or repeats an earlier cursor;
- a page returns no turns.

Items are not a stop condition. The protocol has no per-turn item limit, so
the items of the turns already fetched are trimmed locally. Each page is
reduced as it arrives: the newest 200 items across the turns kept so far stay,
command output becomes the presence marker, and the 512 KiB bound is checked on
what is kept. At most one raw page and the bounded retained history are in
memory at once, and a page that pushes the kept history over the bound fails
the read before another page is requested.

A `nextCursor` that is absent or null ends paging. A non-string cursor is a
malformed page, not the end of history, so that an incomplete history is never
accepted as complete.

Why 20 rather than a smaller page: Codex 0.151 rebuilds the full turn list
from the rollout on every `thread/turns/list` request for legacy threads ("it
still replays the entire rollout on every request"). Fewer, larger pages keep
Codex's work per pass at one replay per thread, the same as today. A smaller
page would shrink reply lines further but multiply that replay for threads
with many light turns. The root thread is read on every pass, which makes that
cost recurring.

## Thread formats

Verified in the Codex `rust-v0.151.0` source
(`app-server/src/request_processors/thread_processor.rs`,
`thread_turns_list_response_inner`):

- **Paginated threads** (`historyMode: "paginated"`, the default for new
  threads from 0.151): served by the thread store's turn index. `itemsView:
  "full"` hydrates each returned turn's items. Cost is proportional to the
  page.
- **Legacy threads:** the server replays the rollout, reconstructs turns,
  merges the live active-turn snapshot, applies the items view, and paginates
  by an anchor-turn cursor. The cursor is a turn id, so a rollback or
  compaction between pages can invalidate it (-32600 "invalid cursor: anchor
  turn is no longer present").

`thread/turns/list` exists from at least Codex 0.130. Before 0.151 it was
`#[experimental]`, and BiBCode opts in with `experimentalApi: true`. The 0.145
and 0.151 parameter shapes are identical (`threadId`, `cursor`, `limit`,
`sortDirection`, `itemsView`).

## Reply-line risk removed

The stdio transport rejects a line over 8 MiB (`max_stdout_line_bytes`) with
`ProtocolError::LineTooLong`. That ends the stdout loop and closes the Codex
connection for the whole session. A full-history `thread/read` of a long
thread could reach that size during an ordinary reconciliation pass.
Durable-delivery reconciliation had the same exposure. With paging, a reply
line holds at most 20 turns, independent of thread length. The remaining
ceiling: 20 turns whose items together exceed 8 MiB still produce one oversized
line. No request in the supported protocol avoids this for legacy threads,
which have no item paging.

## Failure handling

The #120 rule applies to every reconciliation method: **only a missing-method
answer** (-32601, or -32600 "unknown variant `<method>`",
`is_missing_method_error`) marks a method unsupported.

| Failure | Effect |
| --- | --- |
| `thread/read` missing method | `thread/read` unsupported, warning `Codex activity method thread/read is unsupported`, history recovery downgrades (unchanged). |
| `thread/turns/list` missing method | History reads are unavailable: the same read-support downgrade, with warning `Codex activity method thread/turns/list is unsupported`. |
| Metadata or page fails to decode, or exceeds the bound after reduction | That thread is skipped. One `tracing::warn!` per method and cause per runtime (no reply content). The pass continues, and the next pass reads the thread again. |
| Any other request error (transport, -32600 such as an invalidated cursor, cancellation) | Unchanged transient path: the pass publishes stale and returns, or returns on cancellation. |
| `thread/list` or `thread/backgroundTerminals/list` page fails to decode | No longer a downgrade. Warn once, stop paging that method for this pass, and treat the snapshot as partial. For background terminals this means non-authoritative: no interruption is inferred. |
| `delivery_exists` page fails to decode or a request fails | The reconciliation outcome is `Unavailable`, as today. |

## Test migration

About 65 scripted expectations in `apps/server/tests/provider_codex.rs` (53
`expect_request` steps and about 15 manual peer arms) and about 13 in-crate
tests in `runtime.rs` answer `thread/read` with full history.

- `ScriptedPeer` gains `expect_thread_history(thread_id, full_read_result)`.
  The test keeps describing the thread as one full `thread/read` result. The
  step expects the metadata read, answers with the result minus `turns`, then
  serves the `thread/turns/list` requests that follow for that thread by
  emulating Codex's newest-first cursor paging. It checks each request's
  parameters, then hands the next unrelated message to the following step.
  Existing `expect_request("thread/read", …).respond(full)` steps become
  `expect_thread_history(…)` mechanically. Steps that answer with an error keep
  `expect_request` with `includeTurns: false`.
- Manual peers call one shared `thread_history_reply(request, full_result)`
  for both methods.
- In-crate `runtime.rs` tests use the same pattern through a local helper.
- Tests that encoded "a decode failure downgrades" for `thread/list` or
  background terminals are rewritten to the missing-method rule, and each
  rewrite is listed in the delivery report.

New coverage:

- A thread far beyond 8 MiB of history recovers its newest turns through one
  page. The test asserts no request asks for `includeTurns: true` and that
  every reply the peer sends stays under 512 KiB.
- Short pages with cursors continue to 20 turns. A repeated cursor stops
  paging.
- A missing `thread/turns/list` downgrades. An undecodable page is retried on
  the next pass.
- Delivery: found on the first page, found on a later page, absent after the
  last page.

## Out of scope

- The provider-terminal observer (`provider_terminal/codex.rs`) still reads
  descendants with full `thread/read`. Its WebSocket client rejects any
  message over 512 KiB as a read error (`SystemCodexRemoteClient::read_value`).
  That error fails the observer connection, which moves it into
  unavailable/reconnect recovery, so a long descendant thread can repeatedly
  knock the terminal observer offline. This is a remaining risk. The observer
  should adopt the same paging in a follow-up.
- Item-level paging for legacy threads, which Codex does not offer.
