# Task 9 report — guard and quiesce authoritatively missing workspaces

## Status

Completed and ready to commit after this report was written. The server now
installs one shared thread/path availability guard before an authoritative
missing catalog snapshot is published, rejects new workspace-dependent work
with the exact structured `WorkspaceUnavailableError`, and runs bounded
provider/terminal quiescence without deleting conversation or terminal
history.

## Fix round 6 — nonblocking terminal-signal drains

The sixth review follow-up removes the synchronous `Condvar` drain introduced
in round 5. Gate closure remains synchronously linearized with exact permit
acquisition, but already-owned permits now drain on Tokio without retaining the
availability mutex, catalog-entry state, or the catalog registry:

- a serialized two-phase availability reconciliation synchronously closes each
  superseded exact gate and returns owned `Notify`-backed drains;
- each drain enables its notification before inspecting the active-permit
  predicate, and the final permit wakes every waiter, so a last release cannot
  be lost between the predicate and await;
- refresh and bootstrap release all availability/catalog locks while awaiting
  drains, then refresh revalidates its lifecycle and mutation fence before
  finalizing the guard under the catalog publication lock. Guard finalization
  still precedes watch/entry publication and runtime-loss callbacks;
- cancellation drops the prepared reconciliation without exposing recovered
  state. Its exact old gate remains closed until active permits drain and only
  then reopens if the transition was not committed. Removal-guard completion is
  deferred across the same prepared interval, preventing a synchronous guard
  drop from publishing a half-reconciled replacement;
- recovery, removal, and newer loss therefore do not complete while an
  already-authorized cleanup still owns a permit, while cleanup yields, timers,
  the five-second outer deadline, cancellation, and RAII permit drop continue
  to run even on a current-thread Tokio runtime.

Strict RED was captured with a current-thread test whose newer-loss task closed
the old gate while two async cleanup tasks owned permits. The former production
implementation parked the only Tokio worker in `Condvar::wait`; an external
20-second watchdog terminated the test with exit 142 before either cleanup
could resume. The GREEN matrix adds current-thread cleanup timeout/yield,
multiple-permit, cancellation/reopen, and removal coverage. Catalog tests prove
`latest()` remains available while refresh/bootstrap wait, the old missing
guard and watch value remain visible during the drain, and recovered state is
published only after cleanup releases its permit. The real TerminalManager
cleanup-wins test now runs on the default current-thread runtime instead of a
two-worker runtime.

Round 6 verification completed with the following evidence:

- availability state owner: 17 passed;
- catalog service lifecycle/publication: 48 passed;
- WorktreeRuntime and reaper: 19 passed;
- terminal preserving quiesce: 2 passed;
- production orchestration RPC units: 15 passed;
- real-Git worktree catalog: 9 passed;
- workspace-unavailable orchestration and terminal integrations: 4 passed;
- full server library: 1,047 passed;
- `cargo test -p coddy-studio-server --tests`: the 1,047 library tests and every
  server integration target passed, including all 97 provider-terminal cases;
- `cargo fmt --all --check`, server all-targets Clippy with warnings denied,
  `vp check` (1,775 format and 1,309 lint files), and `vp run typecheck` (11/11)
  passed. Typecheck emitted only the repository's existing non-failing Effect
  Schema finite-number suggestions.

No wire schema, persisted shape, migration, dependency, provider identity,
cleanup deadline, or terminal-history contract changed in this round.

## Fix round 5 — terminal-signal linearization

The fifth review follow-up closes the remaining gap between WorktreeRuntime's
last transition-current check and the terminal manager lifecycle lock. Exact
session identity remains the terminal target authority, and the availability
registry now also owns a counted terminal-signal gate for each exact guarded
transition:

- cleanup captures every exact terminal identity, then acquires an owned signal
  permit that atomically revalidates the repository, path, availability state,
  and catalog generation against the current guard;
- recovery, removal, and a newer admitted loss invalidate that exact gate while
  holding registry state. If invalidation linearizes first, a cleanup paused
  after capture cannot acquire a permit or signal. If cleanup linearizes first,
  invalidation waits until exact identity validation, process signaling, and
  retained-history finalization release the permit;
- the gate counts concurrent canonical and persisted-alias cleanup calls, so
  invalidation cannot return after only one already-authorized signal finishes;
- registry invalidation does not acquire a terminal lock, terminal cleanup does
  not retain the registry lock while acquiring its permit or manager lifecycle
  lock, and permit drop never re-enters the registry. Cancellation, cleanup
  failure, panic unwinding, and the existing five-second outer deadline all
  drop owned permits without adding a detached task or a second deadline.

The strict RED added real TerminalManager-backed initial-cleanup, fresh-reaper,
and cleanup-wins scenarios before the gate API existed; it failed to compile on
the missing transition signal methods and WorktreeRuntime integration. GREEN
uses deterministic notifications immediately before permit acquisition and
immediately after the permit is owned. Recovery-wins tests preserve the same
unchanged live session with zero kill attempts, successful write and attach,
running status, and intact history for both initial cleanup and a fresh reaper
retry. The cleanup-wins test pauses before the terminal lifecycle acquisition,
proves recovery has reached and is waiting on exact gate invalidation, then
confirms two captured sessions are killed, finalized as exited, and retain
their separate histories before recovery returns. A later replacement remains
running, writable, and attachable. A registry test independently proves that a
newer loss waits for both of two current permits and rejects the stale
transition afterward.

Round 5 verification completed with the following evidence:

- availability state owner: 14 passed;
- WorktreeRuntime and reaper: 19 passed;
- terminal manager: 29 passed;
- production orchestration RPC units: 15 passed;
- orchestration engine: 26 passed;
- production orchestration RPC integration: 17 passed;
- production server-terminal RPC integration: 12 passed;
- worktree catalog integration: 9 passed;
- production worktree catalog RPC integration: 18 passed;
- full serial server library: 1,042 passed;
- every server integration target passed. The known load-sensitive
  `agent_activity_hung_factory_does_not_block_terminal_disable_or_later_settings_updates`
  case was excluded from the combined provider-terminal run, which passed 96
  tests, and then passed alone in 1.07 seconds;
- `cargo fmt --all --check`, server all-targets Clippy with warnings denied,
  `vp check`, and `vp run typecheck` passed. Typecheck emitted only the
  repository's existing non-failing Effect Schema finite-number suggestions.

No wire schema, persisted shape, migration, dependency, commit-fence,
provider-identity, exact loss-publication, disconnect, reaper, or terminal
history contract changed in this round.

## Fix round 4 — exact terminal cleanup and stable loss failures

The fourth review follow-up closes the remaining terminal-selection and RPC
error-ordering races without a wire-schema, persisted-shape, migration, or
dependency change:

- terminal cleanup now receives the exact workspace-loss transition. It
  captures every live terminal's session, generation, and process only while
  that transition is current, then rechecks the session-map pointer,
  generation-registry pointer, and process pointer under the terminal
  lifecycle lock before signaling. A recovered replacement published under
  the same thread and terminal IDs is therefore not a target of stale initial
  cleanup or a reaper retry;
- exact terminal quiesce still signals all current captured sessions before
  waiting, aggregates failures, and finalizes successful exits into their
  retained bounded histories. The existing exact-generation exit guard remains
  the second line of protection after target selection;
- each admission finalization gate now owns its loss cancellation and publishes
  the exact `WorkspaceUnavailableError` while holding the gate lock immediately
  before changing the gate to rejected. Cancellation notification follows the
  commit-order decision, but a SQLite `CommitRejected` result can no longer be
  observed before its structured loss value;
- turn RPC dispatch maps a generic engine cancellation back to that exact
  published loss when database rejection wins the RPC select. Ordinary engine
  cancellation remains unchanged when no workspace loss is attached.

The terminal RED specified two-session identity capture, replacement, and
resumed exact cleanup, and failed because the manager exposed only thread-wide
selection at execution time. GREEN proves the replacement remains running,
writable, attachable, and retains its transcript while the other captured
session exits with history intact. Existing recovery/reaper coverage also
asserts that an exact recovered transition cannot reach a fresh terminal
cleanup attempt.

The RPC RED paused accepted and rejected SQLite transactions before
finalization, rejected the gate, then paused loss before cancellation
notification. Releasing SQLite deterministically made the old code return
`OrchestrationDispatchCommandError`; GREEN returns the exact structured
`WorkspaceUnavailableError` in both persistence branches while preserving
their rollback behavior. Existing commit-wins cases continue to prove that
loss waits for accepted and rejected transaction finalization.

Round 4 verification completed with the following focused and broad evidence:

- availability state owner: 13 passed;
- terminal manager: 29 passed, including the exact two-session replacement
  regression;
- worktree runtime and reaper: 16 passed;
- production orchestration RPC units: 15 passed, including the forced accepted
  and rejected persistence ordering matrix;
- orchestration engine: 26 passed;
- production orchestration RPC integration: 17 passed;
- production server-terminal RPC integration: 12 passed;
- provider exact-recovery regression: 1 passed;
- full server library: 1,038 passed;
- every server integration target passed. The known load-sensitive
  `agent_activity_hung_factory_does_not_block_terminal_disable_or_later_settings_updates`
  case was excluded from the combined provider-terminal run, which passed
  96 tests, and then passed alone in 0.99 seconds;
- `cargo fmt --all --check`, server all-targets Clippy with warnings denied,
  `vp check`, and `vp run typecheck` passed. Typecheck emitted only the
  repository's existing non-failing Effect Schema finite-number suggestions.

Two verbose aggregate runs were interrupted after detached output handling and
a back-to-back library invocation stalled around an unrelated production
control test. A clean exact rerun of that control test passed in 0.53 seconds,
the clean full library rerun passed all 1,038 tests in 55.99 seconds, and all
integration targets then passed in separate fresh test processes. This keeps
the final evidence complete without treating runner-output or repeated-suite
state as an application failure.

## Fix round 3 — atomic SQLite finalization and exact provider cleanup

The third review follow-up closes the two remaining ordering gaps without a
wire-schema, persisted-shape, or migration change:

- every workspace-bound queued turn now carries a generic `CommitFence` with
  its retained lifetime. The fence is acquired synchronously inside the real
  SQLite transaction immediately before `COMMIT`, after the event, projection,
  receipt, attachment-reference, and provider-outbox writes. Loss/removal and
  final commit therefore have one deterministic order: a loss-owned fence
  rejects finalization and the transaction rolls back, while a commit-owned
  fence delays loss/guard publication until durable commit completes;
- plan-error rejection receipts now use an explicit transaction and the same
  finalization fence. They can no longer autocommit after a workspace loss that
  already won admission finalization;
- the finalization permit is owned rather than a borrowed mutex guard, so it is
  safe on the blocking database thread. Every success, SQLite error, early
  return, or unwind drops the permit, wakes synchronous loss, and leaves no
  lock-order path back into the registry;
- provider cleanup now captures the supervisor's exact active driver identity
  only while the loss transition is current. `stop_session_if_current` checks
  that identity inside the actor; an old stop resumed after exact recovery and
  replacement is a no-op. Reaper retries re-resolve aliases and recapture
  identities only for current ownership. The surrounding recovery/shutdown
  cancellation still short-circuits the attempt;
- terminal generation semantics were audited: cleanup snapshots the exact
  generation/process under the lifecycle lock, and exit finalization already
  ignores a replaced generation. No terminal change was required.

Deterministic RED evidence covered both finalization orders for a successful
turn and for a persisted rejection. With loss paused at the SQLite boundary,
the old successful path still produced a receipt and outbox, increased events
from 6 to 10, and projected the user message. Moving the commit-wins barrier
before permit acquisition let loss finish before either successful or rejected
commit. The provider RED failed because there was no exact-session capture or
conditional-stop API; the prior thread-only stop could target whatever runtime
was current when it eventually executed.

GREEN coverage now proves loss-wins leaves no receipt, message, event, or
outbox; commit-wins keeps loss blocked until all accepted-turn artifacts are
durable; and the same two orderings respectively roll back or retain the exact
rejected receipt without provider delivery. A direct permit-drop regression
proves the error/unwind path wakes loss and rejects reuse. The provider recovery
test captures the old identity, clears the exact guard, starts a replacement,
resumes the stale stop, and proves the replacement remains routable.

Fix-round-3 validation completed with the following focused matrix:

```text
availability/finalization state machine                 13 passed
warning/quiesce/reaper runtime                          16 passed
orchestration engine                                    26 passed
orchestration RPC, including four commit orders         17 passed
provider workspace loss and exact recovery               2 passed
catalog service                                         46 passed
real-Git catalog                                         9 passed
terminal manager                                        28 passed
production terminal RPC                                 12 passed
turn-delivery subprocess recovery                        8 passed
```

The unfiltered full-server run passed all 1,037 library tests and every
integration target reached before the repository's documented load-sensitive
`agent_activity_hung_factory_does_not_block_terminal_disable_or_later_settings_updates`
case timed out at its one-second manager-preparation deadline (96 sibling tests
passed). That exact test passed immediately in isolation, 1/1 in 1.00 seconds.
A second full-server run with only that exact test skipped then passed all
1,037 library tests, all integration targets (including the eight subprocess
recovery tests), and doc-tests; the supervisor target passed 96/96 with one
filtered test.

## Fix round 2 — cancellation handoff and transition-scoped cleanup

The second review follow-up closes the remaining cancellation/deadline gaps
without changing an RPC schema or persisted event shape:

- turn admission now crosses the RPC/engine queue boundary in a type-erased
  `CommandLifetimeGuard`. Dropping or interrupting the client wait does not
  release the workspace lease or cancel an already-admitted durable command.
  Authoritative loss cancels the retained worker token at deterministic checks
  before planning and immediately before persistence, so the worker cannot
  create a receipt, message, or outbox row after guard installation;
- every active admission carries the exact structured loss and a cancellation
  token. Loss and removal cancel matching thread/path leases synchronously.
  Terminal open, setup, restart, and restart-on-attach propagate that token
  through spawn and manager publication. A late spawn remains owned by
  `UncommittedPtyProcess`, is killed, and is never published;
- provider routing/reconciliation and Git unary, stacked-action, and status
  stream work select the same loss token. Git uses a child request token, so
  loss cancels external work while the exact `WorkspaceUnavailableError` wins
  publication;
- the one five-second graceful deadline now begins at quiesce entry. Known
  canonical provider/terminal cleanup and warning persistence start
  immediately while persisted aliases resolve in parallel. A never-returning
  resolver cannot consume the deadline before canonical cleanup is polled;
- orphan ownership is keyed to the exact thread/repository/generation/path/state
  transition. Exact recovery or a newer loss cancels stale queued, active, or
  saturated ownership before it can affect recovered/newer sessions. Resolver
  failure cleans the canonical owner immediately and leaves one owned retry
  that re-resolves every alias;
- every reaper job owns one fresh attempt with its own five-second deadline.
  Timeout/error releases the shared permit and retains the marker rather than
  looping indefinitely. Queue capacity remains 64, the shared observer/reaper
  concurrency bound remains 16, and runtime shutdown cancels and drains every
  queued/active future before provider and terminal owners stop.

Deterministic RED evidence included: an interrupted RPC released its lease and
the engine persisted a post-loss message; a never-released publication lease
could delay loss indefinitely; canonical cleanup call count remained zero
while alias resolution consumed the deadline; resolver failure had no complete
repeatable alias cleanup; stale retries survived recovery/newer transitions;
hung reapers monopolized every shared permit; and an already-spawned PTY could
publish after loss. The paired disconnect characterization proves that RPC
disconnect without workspace loss still commits and replays exactly as before,
whereas disconnect followed by loss creates neither receipt nor message.

Fix-round-2 focused matrix after the final self-review:

```text
worktree catalog/registry, including removal             58 passed
workspace runtime/panel/history/reaper/deadline          16 passed
terminal manager lifecycle/publication/history           28 passed
terminal production RPC                                  12 passed
orchestration engine                                     26 passed
orchestration production RPC/replay                      15 passed
Git/VCS production boundary                               4 passed
shared missing-path normalization                         1 passed
provider route publication fence                          1 passed
```

The Round 2 changes add no public error union or contract field. The existing
generated RPC corpus therefore remains unchanged; fixture generation/parity is
covered by the broad repository gates below rather than a schema regeneration.

Fix-round-2 final gates:

```text
cargo test -p coddy-studio-server -j 2 -- --test-threads=1
  library                                             1,036 passed
  integrations before provider_terminal_supervisor      all passed
  provider_terminal_supervisor                       96 passed, 1 failed

isolated known provider-terminal timeout                     1 passed

cargo test -p coddy-studio-server -j 2 -- --test-threads=1 \
  --skip agent_activity_hung_factory_does_not_block_terminal_disable_or_later_settings_updates
  every non-skipped library/integration/doc target          passed

cargo fmt --all --check                                    passed
cargo clippy -p coddy-studio-server --all-targets -- -D warnings
                                                           passed
vp check                              1,775 format / 1,309 lint files passed
vp run typecheck                                         11/11 tasks passed
rpcRustParity + Rust fixture exporter parity              7/7 passed
```

The only unskipped broad failure is the same unchanged two-second
provider-terminal timeout documented in Fix Round 1, at
`apps/server/tests/provider_terminal_supervisor.rs:8399`. Under the full serial
load it returned `manager preparation timeout: Elapsed(())`; the immediate
isolated exact rerun passed in 0.98 seconds. No file in that test's owner changed
in this round. The exact-skip run then proved every other server target, while
the isolated pass records the load-sensitive nature rather than hiding the
failure. Typecheck again printed only the repository's existing non-failing
Effect finite-number suggestions.

## Fix round 1 — review closure

The review follow-up closes five concurrency/identity gaps without changing the
public error schema or regenerated RPC fixtures:

- persisted thread projections now resolve catalog owners, ordinary threads,
  and panels onto the same normalized physical path; loss deduplicates and
  stops every matching provider session and terminal while retaining every
  thread and message row;
- the central registry owns cancellation-safe thread/path admission leases.
  Turn admission, asynchronous provider delivery and reconciliation, terminal
  open/restart/write/setup publication, and Git process publication hold the
  appropriate lease. Guard installation sees the lease atomically, waits for
  earlier publication, and rejects later admission;
- path keys now collapse duplicate separators and lexical `.`/`..` for missing
  paths while clamping absolute traversal at POSIX roots, Windows drive roots,
  and UNC share roots. Relative leading `..` remains meaningful;
- nested removal uses independent depth tokens, so arbitrary guard-drop order
  preserves the latest pending missing state until the last token releases;
- cleanup uses no detached Tokio task. Initial cleanup is a boxed cancellable
  future, timeout transfers ownership to a bounded active/queued reaper, and
  failures or captured panics create repeatable attempts. Counted orphan
  ownership clears only after confirmed provider and terminal success;
- one shared semaphore bounds all overlapping catalog observers and reaper
  retries. Runtime shutdown cancels and drops queued/active cleanup futures,
  joins the single reaper owner, and proves zero active jobs before provider and
  terminal shutdown.

Deterministic RED cases observed the old behavior directly: a persisted panel
turn passed the catalog-owner-only guard; a terminal open paused between PTY
spawn and manager publication escaped quiescence; the final nested removal
guard restored availability instead of the pending loss; two concurrent
observers exceeded a per-call concurrency budget; cleanup errors cleared their
orphan signal without retry; and shutdown had no owned active-future count.
All race coverage uses semaphores, channels, paused time, or task-state
barriers—never timing sleeps.

Fix-round focused results before broad validation:

```text
availability/admission/removal state machine             9 passed
warning/panel-resolution/reaper/global-bound runtime    11 passed
worktree path identity                                    6 passed
real-Git catalog                                          9 passed
orchestration owner/panel admission                       2 passed
terminal guard/history/publication race                   3 passed
provider session stop                                     1 passed
provider delivery/reconciliation guard                    1 passed
Git and workspace public boundaries                       2 passed
lexical replay ownership                                  1 passed
```

The persistence-backed panel test creates a canonical workspace plus a panel
using a lexical path alias, persists conversation history for both, runs the
runtime loss observer, verifies provider and terminal cleanup for both IDs, and
then reloads both retained threads and messages. The terminal integration test
separately proves retained PTY transcript access after process quiescence; the
provider integration test proves session shutdown without deleting its thread.

Fix-round final gates:

```text
cargo test -p coddy-studio-server -j 2 -- --test-threads=1
  library                                              1,022 passed
  integrations before provider_terminal_supervisor       all passed
  provider_terminal_supervisor                       96 passed, 1 failed

cargo test -p coddy-studio-server -j 2 -- --test-threads=1 \
  --skip agent_activity_hung_factory_does_not_block_terminal_disable_or_later_settings_updates
  every non-skipped library/integration/doc target        passed

cargo fmt --all --check                                  passed
cargo clippy -p coddy-studio-server --all-targets -- -D warnings
                                                         passed
vp check                           1,775 format / 1,309 lint files passed
vp run typecheck                                      11/11 tasks passed
```

The one unskipped broad failure is an unchanged pre-existing
provider-terminal test at
`apps/server/tests/provider_terminal_supervisor.rs:8399`. Its fixed two-second
wrapper expires with `manager preparation timeout: Elapsed(())` while exercising
an intentionally hung factory. The exact isolated test was rerun three times,
including once on an otherwise idle test system, and remained red. Neither the
test nor its `provider_terminal`/`terminal::manager` owners differ from base
`c9d87a64`; the fix round did not modify that unrelated timeout. The explicit
skip run demonstrates that every other server target passes and does not hide
the isolated failure.

No RPC error union, schema, manifest, or generated fixture changed in this fix
round. The prior Task 9 corpus therefore remains stable at 89 methods, 16
streams, 247 total fixtures, and 228 schema fingerprints; regeneration was not
required. The typecheck gate printed only the repository's existing non-failing
Effect finite-number suggestions.

## Ownership and lifecycle

- `apps/server/src/worktree_catalog/availability.rs` owns the central
  `WorkspaceAvailabilityRegistry`. It indexes durable thread IDs and
  host-platform-normalized paths, including root/descendant checks and a
  canonical request-path check when the request still exists.
- Missing loss work is coalesced by `(threadId, generation, availability)`.
  Repeated authoritative snapshots in the already-guarded state do not start a
  second cleanup. A same-generation transition from `missing-registered` to
  `missing-unregistered` is still admitted once.
- Degraded/non-authoritative snapshots are no-ops. Recovery clears only the
  matching thread, normalized path, and physical repository. `removing` owns
  precedence and retains the newest matching missing state for restoration if
  the removal guard is released.
- Pending orphan cleanup is an exact transition-owned record rather than a
  last-writer-wins thread boolean. One older reaper completion therefore
  cannot clear a newer transition's pending marker, while recovery/newer loss
  can cancel that exact stale owner. Queue saturation deliberately retains both
  the guard and transition marker.
- `WorktreeCatalogService` reconciles availability while its publication state
  is still locked and before `watch::send_replace`. The same ordering is used
  during initial catalog bootstrap, before the catalog entry/watch channel is
  inserted and before the first subscription returns. Only then is the
  asynchronous runtime-loss observer invoked.
- `apps/server/src/production/worktree_runtime.rs` owns warning persistence,
  provider stop, terminal quiescence, the five-second shared deadline, at most
  16 concurrent observer/reaper attempts, and the bounded 64-entry reaper
  queue. Warning persistence, canonical cleanup, and alias resolution begin
  together; a failed/stalled warning or resolver cannot delay canonical
  cleanup or extend the five-second bound. `ProductionRuntime` shuts the
  catalog before the reaper, then the provider and terminal owners.

## Runtime preservation

- The production provider action sends `ThreadSessionStop`. A missing provider
  session is already quiescent; other failures are logged without suppressing
  terminal cleanup.
- `TerminalManager::quiesce_thread_preserving_history` snapshots all sessions
  for the thread, signals every live PTY before waiting, and finalizes each
  successful exit into the existing retained session. It does not use the
  destructive terminal-close path.
- A kill failure is aggregated after every process has been attempted; the
  first failed kill cannot prevent later terminals from being signaled.
- The global terminal lifecycle lock covers only snapshot-and-signal ordering.
  It is released before process/tree exit waits, so unrelated terminal
  lifecycle work is not held behind the graceful cleanup wait.
- Read-only attach remains allowed after loss and returns the exact prior
  bounded transcript with `Exited` status. New open/restart/write and
  restart-on-attach remain guarded. Thread rows, conversation messages, and
  terminal session history are preserved.
- Each admitted transition appends one warning activity using a deterministic
  ID derived from thread, physical repository, generation, and availability.
  Duplicate refreshes do not append a second activity. Warning persistence
  failure is diagnostic only and cannot prevent provider/terminal cleanup.

## Guarded public boundaries

The production runtime injects the same registry instance into the catalog and
all affected public owners:

- orchestration: `thread.turn.start` is rejected after command validation but
  before attachment materialization, durable admission, or provider delivery;
- terminal: open, restart, write, and restart-on-attach are rejected before PTY
  operations; ordinary attach, resize, clear, and close remain available;
- Git/VCS/source-control/editor: every client request or stream carrying a
  guarded `cwd` is rejected before broadcaster subscription, progress
  publication, process launch, or repository mutation;
- workspace: file reads/writes, entry mutation/list/search, browse with a cwd,
  review, workspace assets, and project favicons are guarded before the owning
  filesystem, index, Git review, or asset service runs;
- internal cleanup continues to call `GitRepository` directly and does not pass
  through client-path guards.

Catalog refresh, conversation/history reads, terminal close/read-only attach,
and thread delete/detach remain allowed as required.

## Contracts and fixtures

`WorkspaceUnavailableError` was added to the affected RPC error unions,
including stream declarations. The generated Rust wire corpus was regenerated
in the same change:

- 89 methods and 16 streams;
- 171 typed-failure fixtures;
- 247 total fixtures;
- 228 schema fingerprints;
- 57 stream-shape fixtures;
- 69 changed fixture paths: 35 modified and 34 new typed failures.

The 34 new unary typed-failure fixtures cover assets, browse, Git pull-request
helpers, orchestration dispatch, all guarded project methods, review, editor,
source-control discovery/publish, terminal open/restart/write, and guarded VCS
operations. The affected streaming schemas are checked by the same manifest,
fingerprint, TypeScript parity, and Rust `rpc_wire` suites.

## TDD evidence

### RED

The implementation was driven through focused deterministic failures. The
important RED observations were:

- registry tests initially failed to compile because the availability types
  and state owner did not exist;
- catalog ordering tests initially failed to compile because there was no
  runtime-loss observer, and initial-bootstrap coverage later observed callback
  count `0` for an already-missing workspace;
- the registry initially rejected a valid same-generation availability change
  and mishandled filesystem roots;
- the paused-time reaper shutdown test hung because cancellation was not
  selected while awaiting an active job;
- boundary tests first failed because orchestration, terminal, Git/VCS, and
  workspace owners had no shared-registry injection or guard calls;
- terminal preservation coverage initially had no coherent quiesce API; the
  follow-up kill-failure test proved the first kill error left another PTY
  unsignaled;
- a pending-warning test observed provider/terminal cleanup call count `0`,
  proving warning persistence could block all cleanup;
- a two-job orphan test proved one completion cleared another pending cleanup
  under the original boolean representation;
- contract decoding failed before `WorkspaceUnavailableError` joined the RPC
  unions; the fixture exporter then reported the old 137/213/194 expectations
  instead of the final 171/247/228 corpus;
- the first warnings-denied Clippy run found one `collapsible_if` in the
  restart-on-attach guard.

All timing/concurrency tests use paused Tokio time, barriers, semaphores, or
observable task state rather than sleeps.

### GREEN

Final focused lifecycle matrix:

```text
availability registry state machine                    6 passed
catalog service lifecycle/publication ordering        46 passed
production warning/quiesce/reaper                      5 passed
terminal preserving quiesce                            2 passed
real-Git worktree catalog                              9 passed
orchestration unavailable boundary                     1 passed
terminal unavailable/history boundary                  2 passed
Git unavailable boundary                               1 passed
workspace unavailable boundary                         1 passed
provider workspace-loss stop                           1 passed
Rust rpc_wire parity                                   13 passed
TypeScript contracts/exporter/rpcRustParity            30 passed (4 files)
```

The real-Git lifecycle test creates a registered worktree, removes its
directory, observes `missing-registered`, verifies the guard, prunes/recreates
the exact path, and proves authoritative recovery clears it.

## Final validation

Commands run successfully after the final implementation changes:

```sh
cargo test -p coddy-studio-server --lib worktree_catalog::availability::tests -- --nocapture
cargo test -p coddy-studio-server --lib worktree_catalog::tests -- --nocapture
cargo test -p coddy-studio-server --lib production::worktree_runtime::tests -- --nocapture
cargo test -p coddy-studio-server --lib 'terminal::manager::tests::workspace_quiesce_' -- --nocapture
cargo test -p coddy-studio-server --test worktree_catalog -- --nocapture
cargo test -p coddy-studio-server --test production_orchestration_rpc workspace_unavailable -- --nocapture
cargo test -p coddy-studio-server --test production_server_terminal_rpc workspace_unavailable -- --nocapture
cargo test -p coddy-studio-server --test production_git_vcs_rpc workspace_unavailable -- --nocapture
cargo test -p coddy-studio-server --test workspace_rpc workspace_unavailable -- --nocapture
cargo test -p coddy-studio-server --test production_provider_runtime workspace_loss -- --nocapture
cargo test -p coddy-studio-server --test rpc_wire -- --nocapture
vp test run packages/contracts/src/rpcRustParity.test.ts packages/contracts/scripts/export-rust-rpc-fixtures.test.ts packages/contracts/src/rpc.test.ts packages/contracts/src/worktree.test.ts
vp run --filter @coddy/contracts generate:rust-rpc-fixtures
cargo test -p coddy-studio-server --quiet
cargo fmt --all --check
cargo clippy -p coddy-studio-server --all-targets -- -D warnings
vp check
vp run typecheck
git diff --check
```

Results:

- full server: 1,013 library tests plus every integration and doc-test target
  passed;
- Rust format and all-targets Clippy with warnings denied passed;
- `vp check` passed all 1,775 format files and 1,309 lint files;
- workspace typecheck passed all 11 tasks. It printed the repository's existing
  non-failing Effect Schema finite-number suggestions; none are introduced by
  this task;
- fixture regeneration was stable and `git diff --check` passed.

## Documentation and self-review

`docs/architecture/rpc-and-orchestration.md` now records the shared registry
topology, authoritative/degraded ordering, normalized path identity, public
guard/allow boundaries, deterministic warning, retained terminal/conversation
history, five-second cleanup bound, 16-way initial quiesce admission, 64-entry
reaper, saturation semantics, and runtime shutdown ownership.

Final review specifically checked canonical/lexical path behavior, filesystem
roots and descendants, missing paths, repository-scoped recovery, duplicate
and availability-change transitions, bootstrap publication, removal
precedence, warning failure/stall, recovery/reaper overlap, queue saturation,
shutdown cancellation, terminal kill aggregation, durable-admission ordering,
contract error parity, and direct internal Git cleanup. No generated dependency
or vendored subtree changed.

Residual platform risk is limited to native process-tree termination semantics
already owned by the terminal/provider supervisors; the availability runtime
retains the guard and pending-cleanup signal when termination cannot be
confirmed or the reaper queue is saturated.
