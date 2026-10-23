# Git Manager Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan. This document stops at slice level by the requester's decision; run `/decompose-plan docs/plans/git-manager` to turn § Slices into atomic phase files with per-task steps before execution begins.

**Goal:** Give every BiBCode project a Git Manager — a project-scoped centre
view reproducing GitHub Desktop's interface and functionality for that
project's repository alone, with worktree-aware guards, live refresh, remote-
server support, and zero telemetry.

**Architecture:** The Rust server remains the single Git authority. It executes
every git command through the existing supervised process path, computes every
blocking condition, serialises mutating operations per repository through the
worktree catalog's existing repository lock, and streams operation progress.
The React client renders — virtualised lists, diffs, dialogs — and never
re-derives git policy. All traffic uses the existing typed WebSocket RPC; no
`DesktopBridge` work, and no new dependencies.

**Tech Stack:**

- **Rust / Axum / Tokio** — `apps/server`: git execution, RPC handlers,
  broadcaster, guards. Build `cargo build -p bibcode-server`; test `cargo test
  -p bibcode-server`; lint `cargo clippy -p bibcode-server --all-targets -- -D
  warnings`; format `cargo fmt --all --check`.
- **TypeScript / Effect Schema** — `packages/contracts`: schema-only wire
  contracts, no runtime logic.
- **React 19 / Vite+ / TanStack Router / zustand / Effect Atom** — `apps/web`.
  Checks `vp check`, `vp run typecheck`, `vp test <path>`. Reuse only what is
  already a dependency: `@base-ui/react`, `@legendapp/list`, `@pierre/diffs`,
  `@pierre/trees`, `lucide-react`, `zustand`, `dnd-kit`.

**Spec:** [`git-manager-spec.md`](git-manager-spec.md) in this folder. Executors
read both; the spec governs scope, this document governs construction.

**Research evidence:**
[`research/github-desktop-analysis.md`](research/github-desktop-analysis.md)
(behaviour contracts and exact git commands per feature),
[`research/bibcode-integration-surface.md`](research/bibcode-integration-surface.md)
(extension points, RPC gap table, remote-server routing),
[`research/worktree-checkout-restrictions.md`](research/worktree-checkout-restrictions.md)
(empirically verified occupancy behaviour and the guard table).

---

## Global Constraints

Every task's requirements implicitly include this section.

- **No repository lifecycle.** No RPC added by this feature may add, create,
  clone, publish, or remove a repository, or delete the repository. A code
  review that finds such a capability rejects the change outright.
- **Single repository per panel.** Every operation is addressed by the
  `cwd` of the selected checkout of one project.
- **Zero telemetry.** No analytics, crash reporting, usage counters, remote
  feature flags, avatar or identity fetches, or any third-party host contact —
  from the web panel, the server paths it adds, or any dependency. Permitted
  outbound traffic is user-initiated only: git network operations against the
  repository's own remotes, and provider-CLI calls for pull requests and
  checks. No background provider polling. Enforced by test (§ Validation).
- **Provider-agnostic.** No GitHub sign-in, OAuth, forks, or fork settings.
- **No new dependencies.** Everything needed is already in
  `apps/web/package.json` and `apps/server/Cargo.toml`. A task that believes
  otherwise stops and escalates rather than adding one.
- **Privileged desktop operations cross `DesktopBridge`; normal application
  traffic uses typed HTTP/WebSocket RPC in both browser and desktop modes.**
  Nothing in this plan is a bridge command.
- **`packages/contracts` is schema-only** — no runtime logic, no behavioural
  helpers.
- **Every new live RPC method needs exactly one declared scope** in
  `apps/server/src/auth/scope.rs`; a server test fails otherwise. Read-only
  methods must not require a write scope.
- **Log hygiene.** No internal context in log strings: branch names, ref names,
  absolute paths, remote URLs and git stderr must never be interpolated into
  log messages. Log stable codes plus lengths and counts, mirroring the
  existing `GitCommandError`, which carries `stdoutLength`/`stderrLength` and
  not the text. User-facing text is a payload field, never a log line.
- **Git subprocesses stay non-interactive.** Every new invocation goes through
  the existing git environment helper, which sets `GIT_TERMINAL_PROMPT=0`, an
  empty `GIT_ASKPASS`, `SSH_ASKPASS_REQUIRE=never` and
  `GIT_CONFIG_NOSYSTEM=1`, so a credential prompt fails fast instead of
  hanging a server task. Reads additionally use the lock-avoiding environment
  (`GIT_OPTIONAL_LOCKS=0`).
- **Every git invocation runs through the supervised process path**
  (`apps/server/src/git/process.rs`) with a timeout, an output cap, and a
  cancellation token.
- **Force-push is always `--force-with-lease`.** Bare `--force` is forbidden.
  So are `--ignore-other-worktrees`, `git worktree add -f`, and plumbing
  `update-ref` used to bypass a worktree guard.
- **Living documentation ships in the same patch as the behaviour change**, and
  affected runbooks under `docs/testing/` are updated or explicitly recorded as
  reviewed and unchanged.

> **Line numbers in this plan are indicative and drift.** Re-verify every cited
> location before editing. Where this plan and the research documents disagree
> with the working tree, the working tree wins and the discrepancy is reported.

---

## Why this shape

BiBCode users manage worktrees and threads in the app, but every real git
operation — reviewing what an agent changed, staging a subset of it, committing,
switching branches, understanding why a checkout is refused — happens in an
external client today. The existing per-thread Source Control panel covers the
narrow in-thread case; it is not a git workbench.

Two things make this different from simply porting GitHub Desktop:

1. **Repositories move while you look at them.** Agent sessions commit and
   write files continuously. Desktop's freshness model — window focus plus
   skewed timers, no filesystem watching at all — is not sufficient here, and
   its `--skip`-based history paging silently duplicates and drops rows under
   concurrent commits.
2. **The repository may not be on this machine.** Remote Server support is
   shipped, git RPCs are already `cwd`-addressed and environment-routed, so the
   panel gets remote support essentially free — provided it never resolves a
   path client-side and never assumes a local filesystem.

## Architecture

### Ownership

| Concern                                            | Owner                                              |
| -------------------------------------------------- | -------------------------------------------------- |
| Git execution, parsing, error classification       | `apps/server/src/git/`                             |
| Blocking conditions and their user-facing messages | a new pure guards module in `apps/server/src/git/` |
| Operation serialisation                            | the worktree catalog's existing repository lock    |
| Live change signal                                 | the existing `StatusBroadcaster`, extended         |
| Wire shapes and capability flags                   | `packages/contracts`                               |
| Environment routing, retry, subscription re-attach | `packages/client-runtime`                          |
| Rendering, view state, virtualisation, diffs       | `apps/web`                                         |

The client renders `{ operation, code, message }` triples verbatim and computes
no git policy. This is the single most important structural rule in the plan:
every guard, every message, every "why is this disabled" answer originates
server-side, because only the server can see the repository.

### Server

- **Invocation.** The server shells out to the `git` CLI; there is no `git2` or
  `gitoxide` dependency and this plan does not add one. New operations extend
  `apps/server/src/git/repository.rs` and its neighbours, following the
  existing request/parse/error shape in `model.rs`, `parser.rs` and
  `process.rs`. Exact command lines and parse formats for every feature are
  recorded per-feature in `research/github-desktop-analysis.md` § 3 and should
  be treated as the specification for each git call.
- **The staging model.** GitHub Desktop hides git's index: the checkbox and
  line selection _are_ the staging state, rebuilt into the index at commit
  time (`git reset -- .`, then stage fully-selected paths via `update-index`,
  then apply per-line patches with `git apply --cached --unidiff-zero`, then
  `git commit -F -`). BiBCode's existing panel instead stages incrementally
  through `vcs.stageFiles`/`unstageFiles`. **The Git Manager keeps BiBCode's
  visible-index model** — it is simpler, matches the existing panel it shares
  state with (spec § 12, decision 12), and avoids a rebuild racing an agent's
  concurrent `git add`. Desktop's patch-application machinery is still the
  reference for _hunk and line staging_, which has no equivalent today.
- **Mutation discipline.** Every git mutation passes through the status
  broadcaster's mutation fence (`begin_mutation` → guard), or it races the
  streaming status. The stage/unstage arm in
  `apps/server/src/production/git_vcs.rs` is the template.
- **Serialisation.** Mutating operations serialise per repository through the
  worktree catalog's existing repository lock, acquired in the catalog's
  existing order (project lock, then repository lock keyed by the canonical
  common directory). This plan introduces **no second lock**: a push or merge
  and a worktree add or remove on the same physical repository must never
  interleave, and one arbiter is the only way to guarantee that. A second
  concurrent operation is rejected with `operation-in-flight`, never queued
  silently.
- **Guards.** One pure module takes the parsed refs, the worktree inventory,
  the dirty flag, the default branch, the in-progress-operation state and the
  lock state, and returns the blocked list per ref. Pure, so it is unit-tested
  without a repository. Guards are re-validated under the repository lock
  immediately before execution; a stale client receives a structured blocked
  error, never raw stderr.
- **Error classification.** A stderr-to-code table is needed for actionable
  failures: authentication, non-fast-forward, local-changes-overwritten,
  conflicts, cancelled. This is for _error reporting only_. Occupancy and other
  guard conditions are pre-computed from the worktree catalog or
  `for-each-ref --format='%(worktreepath)'`, never from stderr matching — git's
  wording is version-specific, and GitHub Desktop deleted its own stderr-regex
  implementation in favour of a pre-computed check.
- **Live signal.** Extend the existing `StatusBroadcaster` with a cheap
  refs/HEAD/worktree signature per repository, emitting a new generation when
  it changes. No additional poller task and no new watcher subsystem: the
  broadcaster already runs a local status poller and a remote/ref poller per
  repository, and the signature check belongs on the existing ref tick. Both
  pollers start only when the first subscriber arrives, so a Git Manager
  subscription must trigger that same subscribe-driven start even when no
  status subscriber exists.
- **History paging.** One `git log` invocation per page with an explicit
  record and field separator, bounded and capped. **Pages are pinned to a
  resolved tip snapshot, not to `--all` with a raw `--skip`.** The first
  request resolves current ref tips and returns them; every later page passes
  them back, so offsets stay valid however much the repository moves. A
  generation bump splices new commits above the pinned snapshot rather than
  discarding loaded pages, preserving scroll position and selection. A full
  reset happens only on explicit refresh or when the pinned tips can no longer
  be resolved. The tip list is capped; a repository above the cap falls back to
  `--all` paging and accepts reload-on-bump, which the UI states rather than
  silently degrading.

### Contracts

New methods are needed because the existing surface has real gaps. Confirmed
present today: streaming status, file-level stage/unstage/discard, branch
list/create/switch, commit log metadata, pull (`--ff-only` hard-coded),
working-tree and branch-range diffs, and a stacked commit/push/PR stream.
Confirmed absent: standalone commit and push, hunk staging, stash of any kind,
merge, rebase, cherry-pick, revert, amend, tag, reflog, branch delete, a branch
rename RPC (the server function exists, unexposed), user-invoked fetch,
per-commit diff, and conflict states in the file-status schema.

Design rules for the additions:

- A commit-graph page carries, per commit: sha, short sha, ordered parents, ref
  decorations, subject, body, author and committer identity and timestamps.
  Today's `VcsCommit` has neither parents nor decorations and **must not be
  changed in place** — existing callers depend on it.
- A refs snapshot carries local branches (upstream, ahead, behind, tip sha,
  current, default, `worktreePath`), remote branches, tags, worktrees, plus
  repository-level head ref, detached sha, dirty flag, default branch, remotes,
  the in-progress operation if any, and conflicted paths.
- Every ref carries its blocked reasons as `{ operation, code, message }`,
  authored server-side; `message` is rendered verbatim.
- Both reads carry a monotonically increasing generation.
- Per-commit diffs need a **read-scoped** method. They cannot reuse
  `review.getDiffPreview`, which is mapped to a review _write_ scope; browsing
  history read-only must not require a write scope.
- Every new method is gated behind a default-false capability flag so older or
  third-party servers degrade feature by feature.

**Naming collision — read before naming anything.** `GitManagerError` and
`GitManagerServiceError` already exist in `packages/contracts/src/git.ts`
(~:419-453) and mean something else entirely: they are the generic error types
of the server's _internal_ git service, behind methods like
`vcs.refreshStatus`. They predate this feature and must not be repurposed,
renamed, or shadowed.

The naming decision that follows: RPC methods take the `gitManager.*` prefix,
which is unused today (existing git surfaces are `vcs.*`, `git.*`, `worktree.*`
and `review.*`). Schema symbols take the `GitManager` prefix — `GitManagerRefs
Snapshot`, `GitManagerCommitPage`, `GitManagerBlockedReason`,
`GitManagerOperationEvent` — with the single exception that this feature's
error type is `GitManagerOperationError`, because `GitManagerError` is taken.

**Registration is a hard gate, not an afterthought.** Each method must be
registered in every one of: `WS_METHODS`, `Rpc.make`, and the exported RPC
group (`packages/contracts/src/rpc.ts`); the regenerated, checked-in wire
fixtures under `packages/contracts/fixtures/rpc-wire/`; `ACTIVE_RPC_METHODS`
(`apps/server/src/rpc/methods.rs`); `required_scope`
(`apps/server/src/auth/scope.rs`); the handler and dispatch table
(`apps/server/src/production/git_vcs.rs`), with streaming methods also joining
the stream method list; and the maintenance allowlist for read-only methods.
Two separate places additionally carry **hard-coded counts** that fail the
build when they disagree — the fixture export script
(`packages/contracts/scripts/export-rust-rpc-fixtures.ts`) and the Rust wire
test (`apps/server/tests/rpc_wire.rs`). Both must be re-read and bumped in the
same change; the numbers recorded in the research documents are already stale
relative to each other and must not be trusted without re-reading the files.

### Client

- **Route.** A project-scoped route, `/project/$environmentId/$projectId/git`,
  rendered as the centre view. The centre-panel store is thread-keyed and
  stays that way — the Git Manager deliberately does not become a
  `CenterSurface` kind, which avoids touching the store's persisted-state
  sanitiser, its exhaustive kind switches, and its mount predicate entirely.
- **Sidebar button.** A sibling of the "New worktree" button in the project
  header hover strip (`apps/web/src/components/Sidebar.tsx`), reusing the same
  shared icon-button class, the same hover/focus-within reveal, and the same
  member-disambiguation handler that resolves a grouped row to one physical
  project. Idempotent: it navigates to the project's Git Manager, focusing the
  existing view rather than creating a second one.
- **View state.** A zustand store, persisted, keyed by `(environmentId,
  projectId)` — never bare `projectId`, since ids collide across environments.
  It holds an LRU of the two most recently used projects' view state and the
  commit-message draft, and evicts the third. It holds _state only_; no panel
  is kept mounted and no subscription is held for a project not being viewed.
- **Server data.** Through the existing environment-scoped Effect Atom
  families, never raw RPC calls: query atoms refire per connect generation,
  subscription atoms re-attach transparently across reconnects, and mutations
  run on the existing per-`(environmentId, cwd)` lane so they serialise
  correctly. A new streaming method must also be added to the subscription-tag
  union in the client runtime.
- **Shared state with the existing Source Control panel.** The commit-message
  draft and staging state for a given `(environmentId, cwd)` are one source of
  truth shared by both surfaces, so a message typed in one appears in the
  other. The existing action hooks already take an `{ environmentId, cwd }`
  scope and are reused rather than reimplemented.
- **Diffs.** Reuse the existing `@pierre/diffs` worker-pool renderer and its
  helpers. Port the reference implementation's _policy_ — the size ladder,
  whitespace toggle, expandable context, intra-line highlighting, image diff
  modes, submodule and binary interstitials — not its CodeMirror engine. The
  interactive line and hunk gutter for partial staging is new work either way.
- **Lists.** Virtualised with `@legendapp/list` at the fixed row heights in the
  spec. Evaluate `@pierre/trees` for the ref tree before hand-rolling one; if a
  bespoke component wins because of per-row actions and guard states, record
  why in the phase notes rather than leaving the choice unexplained.
- **Author identity.** Rendered from commit data only — name, email, and a
  deterministic identicon or initials derived locally from the email. No
  avatar host is contacted; the reference implementation's
  `avatars.githubusercontent.com` lookup is deliberately not ported (spec
  § 6.7, constraint 4).
- **Agent-activity indicator.** When an agent session is running in the
  selected checkout, the panel shows a passive indicator, so a file list that
  keeps changing is explained. This is presentation only — it never gates or
  delays a git operation (spec § 6.2).
- **Bounded caches.** The commit lookup is LRU-bounded. The reference
  implementation's equivalent map is unbounded and its own source flags this
  as a known defect; do not reproduce it.
- **Accessibility.** Every icon-only control has an `aria-label`; every
  disabled control exposes its server-authored reason through both a tooltip
  and `aria-describedby`; lists are keyboard-navigable; the reference
  implementation's accelerators are the starting keymap.

## Slices

Each slice ships a usable increment, is independently testable end to end, and
leaves the application shippable. Slice 0 is foundation and ships behind the
panel being reachable but read-only.

**Slice 0 — Contracts and server reads.** Schemas and RPC registration for the
refs snapshot, commit pages, per-commit diff, and the guards payload;
`for-each-ref` and worktree-inventory reads; the pure guards module with unit
tests covering every code, including the negative case; tip-pinned paging.
Gate: fixture regeneration and both hard-coded count bumps.

**Slice 1 — Panel shell, read-only.** Route, sidebar button, view-state store
with LRU-2, toolbar skeleton with the worktree selector, Changes and History
tabs, changed-file list, commit list, commit detail, diffs, local author
identity rendering, the agent-activity indicator, and the LRU-bounded commit
lookup. Unavailable-state rendering for disconnected and capability-lacking
environments. No mutation
exists yet, which makes this slice safe to ship early.

**Slice 2 — Staging and committing.** Include/exclude, commit box with summary,
description, co-author trailers and commit options, amend, undo commit,
discard (file, all, selection), submodule and conflict presentation in the
list. Standalone commit RPC; shared draft with the existing Source Control
panel.

**Slice 3 — Branches and sync.** Branch list and dropdown, create, checkout
with the occupancy redirect, rename (guarded), delete, the switch-with-changes
dialog, and the full push/pull/fetch state machine minus its publish states.
Streaming operation progress with cancel and a collapsible output area.

**Slice 4 — Stash and merge.** Full native stash list with apply, pop, drop and
per-entry diff; merge and squash-merge with the `merge-tree` mergeability
preview; the in-progress-operation detection from repository state; the
live refs/HEAD/worktree signature and generation splicing.

**Slice 5 — Partial staging.** Per-line and per-hunk selection with
drag-select, patch construction, `apply --cached` staging, and reverse-patch
partial discard.

**Slice 6 — History rewriting and conflicts.** Rebase, cherry-pick, squash,
reorder, revert, reset, the multi-commit operation state machine with progress
and abort, the conflict list with marker counting and ours/theirs resolution,
and force-push-after-rewrite warnings.

**Slice 7 — Tags, image diffs, provider surfaces.** Tag create, delete and
push; image diff modes; pull requests and checks through the existing provider
CLI, refreshed only on explicit user action.

## Risks

| Risk                                                                                       | Mitigation                                                                                                                                |
| ------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------- |
| Scope: this is a large feature and every slice touches contracts, server and web           | Slices are independently shippable; slice 1 is read-only and de-risks the whole surface before any mutation exists                        |
| The registration and fixture-count gates fail the build in non-obvious ways                | Slice 0 carries the whole contract surface so the gate is crossed once, deliberately, with both count sites re-read from the working tree |
| Concurrent agent writes make status and history flicker or mis-page                        | Tip-pinned paging, generation splicing, the mutation fence, and the passive agent-active indicator                                        |
| Guards computed client-side would drift from server truth                                  | Structural rule: server authors every message; client renders verbatim; guards re-validated under the lock before execution               |
| A second lock deadlocks against the worktree catalog                                       | Reuse the catalog's lock in its existing acquisition order; introduce none                                                                |
| A dependency or asset quietly introduces a network call, breaking the telemetry constraint | No new dependencies; explicit test asserting the feature's network surface                                                                |
| Remote-hosted projects behave differently from local ones                                  | Every slice is validated against a remote-hosted project; paths stay opaque and client-side path resolution is forbidden                  |
| Line numbers and counts in this plan and the research documents drift                      | Stated explicitly above; the working tree wins and discrepancies are reported                                                             |

## Validation

Per slice:

1. Focused tests for every changed behaviour — Rust unit tests for parsing,
   guards and error classification; web component and logic tests colocated as
   `*.test.ts(x)`.
2. Contract gates: `vp run check:contracts` after any contract change,
   including fixture regeneration and both count bumps.
3. `vp check` and `vp run typecheck`.
4. Rust: `cargo fmt --all --check`, `cargo test -p bibcode-server`, and
   `cargo clippy -p bibcode-server --all-targets -- -D warnings`.
5. End-to-end validation against **both** a local project and a remote-hosted
   project, since the routing difference is the feature's main environmental
   risk.
6. **Telemetry test.** An explicit test asserting the feature contacts no host
   other than a configured remote or the configured provider, and that no
   background timer issues provider calls. This is a required deliverable, not
   a nice-to-have; the constraint is mandatory.
7. Final `git diff` and `git status --short` review for unintended edits,
   generated files, debug output and dependency drift.
8. `docs/testing/` runbooks updated in the same change when a slice alters
   test commands, packaged UI flows included in native visual validation, or
   required validation evidence — or explicitly recorded as reviewed and
   unchanged.

## Documentation to update

- `docs/architecture/` — the owning document for git and VCS flow, whenever a
  slice changes protocol flow, persisted shape, lifecycle guarantees or
  documented invariants.
- `docs/reference/` — any new script or command.
- `docs/user/` — the panel's user-facing behaviour, once slice 1 ships.
- `docs/superpowers/plans/2026-08-18-git-manager/` — carries a superseded
  pointer to this folder (see spec § 11).
