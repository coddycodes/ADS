# Worktree catalog

The worktree catalog discovers Git worktrees that belong to a persisted project
but do not yet have a BiBCode workspace thread. It also owns the lifecycle of an
adopted worktree when its directory disappears, returns, or is explicitly
removed. The server is authoritative for paths, repository identity, Git
membership, availability, and destructive decisions. Browser and desktop
clients receive opaque keys and display state; neither client is a path or Git
authority.

## Ownership and sources of truth

| Concern                | Owner and durable source                                                                                                                                                                                                         |
| ---------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Repository identity    | The server resolves `git-common-dir`; a successful authoritative scan establishes the project's durable compare-and-set repository pin. Generic project metadata writes cannot create or replace the pin.                        |
| Current Git membership | A bounded server inventory of `git worktree list --porcelain -z` plus known-path probes. The repository observation is shared by projects that have the same verified common directory.                                          |
| Discovery preference   | The complete `ProjectWorktreeDiscoveryPolicy` persisted in project metadata: `hidden` or `shown`, prompt acknowledgement, and a capped baseline of known paths.                                                                  |
| Workspace ownership    | Orchestration projections and immutable thread events. A catalog snapshot joins them to the current inventory on the server.                                                                                                     |
| Availability           | The shared `WorkspaceAvailabilityRegistry`, indexed by workspace identity and the server's physical path identity. Filesystem RPCs retain an admission lease for the whole operation; mutations also cross a finalization fence. |
| Mutation arbitration   | Durable orchestration command receipts plus a process-local command claim, then the stable project lock and optional repository lock, retained by a server-owned operation after durable handoff.                                |

`packages/contracts` defines only the schemas and wire contracts. Runtime policy
lives in the server catalog, orchestration, and availability services; React
does not reproduce it.

Git Manager operations that can move a worktree HEAD or change the branch set
(checkout, create, rename, delete, pull, merge, rebase, cherry-pick, revert,
reset, and their continue/abort paths) invalidate every project view sharing
the repository when they finish, so sidebar worktree cards follow a checkout
without waiting for the polling interval. Fetch, push, and stash operations do
not. Git Manager mutations reuse the catalog service's same process-local lock set
and acquisition order: stable project identity first, then the optional lock
for the pinned physical repository. Their non-waiting acquisition reports
`operation-in-flight` when either lock is held, including by a catalog mutation;
they do not introduce a second repository mutex. Unlike durable catalog
commands, a Git Manager operation has no orchestration receipt, but its lock,
workspace-admission lease, cancellation token, supervised Git process, and VCS
mutation fence remain owned together until that request settles.

Pull Requests checkout shares these locks and invalidation rules. It validates
both source and target physical repository identities against the catalog pin,
including when an externally registered worktree is not yet adopted. A new
worktree's fetched branch is passed to the same managed creation transaction used
by `worktree.createManaged`, while retaining the repository lock. Pull Requests
planning reuses a matching branch only if it is unoccupied; divergent or occupied
names use `<head>-pr-<number>`, then `-2`, `-3`, and so on. Occupancy includes the
current checkout and registered worktrees whose directories are missing. The transaction
inherits the project's default workspace settings and creates the durable owner
that appears in the sidebar. Checkout uses the same bounded operation runtime.
Guards, host reads, branch planning and `ls-remote` remain cancellable. Immediately
before the first write, checkout hands off to an independent cancellation token:
a started provider checkout, branch fetch and managed creation run to completion
despite client disconnect, request cancellation, the 60-second read deadline or
availability loss. The server-owned task retains the project/repository lock and
availability admission until Git and durable owner settlement finish. Cancellation
ends only the caller's wait; normal refresh observes the resulting branch/workspace.
Its fetch status fence settles before managed creation's existing completion
notifications; these paths never acquire the same VCS mutation fence recursively.

Each checkout write command has a 24-hour safety ceiling, including the scoped
GitRepository clone used for managed creation and rollback; ordinary Git operations
retain their existing deadlines. Graceful server shutdown closes worktree operation
admission and drains these owners without a shorter abort timer before catalog and
orchestration teardown. The desktop's existing five-second in-process backend stop
wait can report a timeout but does not abort the owned server task. Forced host/OS
termination and the safety ceiling remain outside graceful completion. If Git
itself fails, inspect `git status` in the reported checkout path before retrying;
checkout does not reset the tree or remove lock files automatically.

## Identity and trust

Clients address candidates by project ID, opaque worktree key, and catalog
generation. Adoption and removal requests never carry a checkout path. The
server resolves physical host-path identity, verifies Git membership, resolves
repository identity, and looks up the persisted workspace path before acting.

One physical-identity algorithm is used for catalog joins, canonical-owner
uniqueness, availability guards, mutation locks, removal, and cross-project
cleanup. A present path is canonicalized through the filesystem. A missing
path canonicalizes its nearest existing ancestor and appends the normalized
missing suffix, so a symlinked parent, macOS `/var` alias, or lexical `.`/`..`
spelling cannot create a second owner or bypass `Removing`. POSIX comparison
remains case-sensitive. Windows drive and UNC comparison normalizes separators
and uses the native invariant uppercase mapping compatible with Windows ordinal
caseless identity, including non-ASCII folds such as capital and small Greek
sigma. Win32 verbatim drive and UNC prefixes normalize to their ordinary path
forms, while foreign POSIX absolute paths retain POSIX case-sensitive identity.
Only `NotFound` may trigger nearest-existing-ancestor resolution. Permission,
symlink-loop, and all other identity failures return a typed
`WorkspaceIdentityError` and abort the authority-changing transition rather
than falling back to a second lexical owner.

Physical identity keys are comparison and arbitration data; they do not replace
the persisted display path. Adoption and ordinary thread mutations preserve the
exact server-resolved checkout path and its separator/casing spelling for the
workspace record and UI.

Catalog joins reserve every workspace thread that has a current direct
projection-to-inventory path match before considering retained-snapshot
fallbacks. A retained fallback may preserve ownership only when that thread has
no current direct match. Retargeting therefore cannot attach one workspace
owner to both its previous checkout and its new checkout, and branch
reconciliation follows the current durable path.

The first authoritative scan through the configured primary checkout is the
only operation that may establish the durable repository pin. Once pinned, a
fallback anchor is accepted only when it resolves to that same identity. Anchor
selection prefers the configured primary checkout, then a present adopted
worktree, then a previously verified common directory retained for the service
lifetime. A different repository at a reused path cannot replace the pin.

The pin survives database restart and projection replay. This separates durable
repository trust from mutable project metadata and prevents a temporarily
missing primary checkout from transferring authority to an unrelated
repository.

The pin is a local Git-common-directory hash; cross-environment grouping uses
the separate `repositoryIdentity` described in the architecture overview.

## Observation and catalog snapshots

One repository observation is shared by every subscribed project with the same
verified repository identity. Each project still receives its own joined view:
eligible external worktrees, adopted workspaces, discovery policy, generation,
observation time, authority flag, and scan health.

The production bounds are intentionally finite:

| Resource                      | Bound                |
| ----------------------------- | -------------------- |
| Concurrent repository scans   | 4                    |
| Concurrent known-path probes  | 8                    |
| Individual probe timeout      | 1 second             |
| Active polling interval       | 2 seconds            |
| Result reuse window           | 1 second             |
| Idle repository-view eviction | 60 seconds           |
| Failed-scan retry backoff     | up to 30 seconds     |
| Discovery baseline            | 512 normalized paths |
| Runtime-owned mutations       | 64 in flight         |

### Repository fingerprint reuse

After a repository has one trusted successful inventory, the server can prove
whether its retained inventory is still plausible without starting Git. The
versioned fingerprint includes the repository lifecycle and mutation epochs;
physical identities for the common directory, primary checkout, and known
worktree paths; common `config`, `HEAD`, selected symbolic refs,
`config.worktree`, `packed-refs`, and validated reftable state; and sorted
linked-worktree entry names plus each entry's identity, `HEAD`, `gitdir`,
`locked`, `config.worktree`, and reftable state.

Git-admin descendants are opened relative to retained trusted directory
handles with no-follow semantics. The proof permits at most 512 known paths,
linked-worktree entries, or reftable tables, 255 bytes per name, 64 symbolic-ref
components, 64 KiB per file, and 1 MiB total input. Reads are sequential, and
two complete matching passes are required. An unreadable, malformed, escaping,
replaced, symlinked, junction/reparse, canceled, changed-during-read,
unsupported, or over-limit input produces `Unknown`. `Unknown` fails open to
the normal bounded Git inventory; it never proves the retained inventory
current.

For every real scan after bootstrap, the server captures the fingerprint
before starting Git. It publishes that proof for reuse only with the matching
successful authoritative observation after repository lifecycle, mutation,
observation-identity, and project-publication fences still match. A mutation
during inventory therefore cannot be hidden by a later fingerprint capture.

Only a healthy authoritative **Focus** refresh may reuse a matching known
fingerprint. **Explicit** refresh—including manual Retry—and FirstSubscriber,
metadata, availability, mutation, degraded-recovery, changed, or unknown cases
run real inventory. Reuse never resets time: at exactly five minutes from the
completion of the last successful real scan, Focus performs another
authoritative inventory. Before that boundary, reuse skips only the repository
Git observation; the project projection is reloaded, generation is advanced,
suppressions and current availability are applied, and anchor and lifecycle
fences still run.

Managed create records its exact path suppression and invalidates the shared
repository fingerprint only after Git rollback identity and the terminal
durable owner receipt have settled. Retarget and removal likewise invalidate
after their terminal durable settlement; repository-wide Git removal fans the
invalidation out to every live project view. This preserves the existing
managed-creation suppression while forcing the next Focus through real
inventory.

The subscription is latest-value state, not an event queue: a slow consumer may
skip intermediate generations but receives the newest snapshot. Subscriptions
and unary refresh, adoption, removal, anchor, and latest-snapshot consumers all
hold scoped active-user ownership. When the combined subscriber/unary-user
count reaches zero, the service cancels its lifecycle work as appropriate and
schedules pointer-checked idle eviction after 60 seconds. A concurrent reuse
cancels that eviction without allowing an old timer to evict the replacement
entry. Explicit refresh participates in the same single-flight, bounded
observation.

Only a definite not-found result marks a known workspace missing. Permission
errors, timeouts, canceled probes, and Git failures do not manufacture absence.
After an authoritative result, a failed scan retains the last authoritative
candidate and adopted-workspace arrays and reports degraded scan health. The
snapshot fields `generation`, `observedAt`, `authoritative`, `scanStatus`, and
degraded details let clients distinguish stale-but-useful state from a fresh
authoritative observation.

## Discovery policy and adoption

New projects default to hidden discovery. An authoritative generation may be
acknowledged as hidden, switched to shown, or compacted after adoption. The
acknowledgement proves which candidates the user saw, not that no refresh ran
since: Focus reuse, the shallow poller, and Git Manager revalidation advance
the generation without changing candidates. The server accepts generation G
when the eligible candidate paths have not changed from G through its last
authoritative snapshot, and derives and caps the baseline from that snapshot.
A candidate set that changed after G is `stale-generation`. Clients do not
submit paths. Incremental project metadata events omit the policy when it did
not change, so unrelated updates and event replay preserve the complete stored
policy.

Adoption accepts a project ID, opaque worktree key, expected generation,
command ID, and ordinary thread defaults. The handler:

1. claims and digests the public command before resolving catalog data;
2. acquires the stable project lock and then the optional repository lock;
3. refreshes a stale or non-authoritative generation and revalidates current
   membership, repository identity, eligibility, presence, and ownership;
4. creates one ordinary workspace, restores the archived owner, or returns the
   existing owner; and
5. records the immutable result and discovery-policy compaction in the same
   orchestration transaction.

Adoption is read-only with respect to Git. It does not create or repair a Git
worktree, and it does not run the project's worktree-creation script.
Concurrent matching adoption requests converge on one canonical thread while
the adopted workspace retains the exact path reported by the catalog.

Accepted retries replay the immutable `created`, `existing`, or `restored`
result. Modern receipts must contain consistent result metadata. The accepted
legacy exception is deliberately narrow: a metadata-free receipt may prove
`created` or `restored` only from its matching immutable thread event. It cannot
reconstruct `existing` from mutable projections and otherwise fails closed.

Discovery-policy mutation uses the same command-claim, project-lock, and
repository-lock order. Once a command crosses the engine-envelope handoff, RPC
cancellation no longer releases its locks or claim; they remain until a
terminal receipt prevents a following mutation from committing stale policy.
A digest-free accepted legacy policy receipt replays only when the exact
project, terminal sequence, sole policy event, and complete payload prove the
operation. Reserved or prepared legacy receipts resume through normal
aggregate-checked recovery.

The same authority boundary covers every worktree-bearing owner mutation:

- `worktree.createManaged` accepts project/ref intent and ordinary thread
  defaults; the server resolves the project root, chooses the checkout path,
  performs Git creation, and persists the workspace owner. Its private rollback
  carries the exact path and actual branch returned by Git, including an
  automatically suffixed branch, and removes only state created by that
  operation when owner persistence fails. Directory reservation records the
  physical object identity, releases its handle while Git populates the
  checkout, then reopens a deletion-fenced handle and verifies the Windows file
  identity or POSIX device/inode before rollback. A replacement at the same
  path is never removed. This is not a public raw-path primitive. Pull-request
  worktree creation resolves the PR first, then uses this same atomic
  owner-creation RPC; the legacy PR preparation RPC is local-checkout-only. If
  remote-ref selection becomes stale because the corresponding local branch
  appears before Git creation starts, the operation reuses that now-existing
  unoccupied local branch. Ordinary managed creation from an occupied branch keeps
  its existing safe numeric-suffix policy; Pull Requests checkout first applies the
  PR-specific naming policy described above.
- `worktree.createPanel` accepts a host thread and derives project, kind,
  branch, and path from that persisted host.
- `worktree.retarget` accepts an opaque worktree key and expected generation;
  it revalidates catalog membership and ownership before changing a workspace.
- `worktree.adopt`, `worktree.updateDiscoveryPolicy`,
  `worktree.removeFromBibCode`, and `worktree.remove` retain their dedicated
  server-resolved adoption, policy, detach, and removal responsibilities.

Generic orchestration remains available for ordinary non-worktree commands,
but it cannot set discovery policy, nominate workspace kind/path, supply a
worktree bootstrap root, retarget a worktree path, delete an adopted owner, or
force-delete a project containing adopted owners. The HTTP and WebSocket
generic dispatch paths use the same validator. The engine repeats owner and
project deletion checks as a defense in depth.

## Availability, panels, and recovery

Adopted workspaces can be `present`, `verification-unavailable`,
`missing-registered`, `missing-unregistered`, or `removing`. Every filesystem-
backed file, browse, search, asset, review, and mutation RPC acquires the shared
server availability lease before starting and retains it until the entire
operation settles. A mutation enters the lease's finalization fence before its
filesystem or durable commit boundary. Authoritative loss or removal therefore
either rejects a later operation or waits for the already-admitted operation
and its finalization to finish before publishing the guard.

A panel thread is not a separate filesystem owner. Its persisted host
association and worktree path map it to the host workspace's availability, so
chat panels, terminals, Git, files, previews, and other consumers all observe
the same warning and admission decision. The browser cannot override this
mapping.

The first authoritative loss installs the guard before cleanup and schedules
exactly one bounded cleanup lifecycle for the host and its panels. Canonical
cleanup starts immediately while panel aliases resolve in parallel. Provider
and terminal cleanup uses persisted identities; stale work cannot terminate a
replacement that recovered at the same path. The five-second graceful bound is
shared with explicit removal. Reaper capacity is finite (64 lifetimes, with 16
cleanup attempts at once); saturation retains the guard and orphan marker for
retry rather than admitting unsafe work.

Recovery clears the guard only after the exact physical path and pinned
repository membership are verified again. A different directory or repository
at the same spelling is not recovery.

## Removal

Removal is a server-resolved plan/execute flow:

| RPC                          | Scope                   | Purpose                                                                                                                    |
| ---------------------------- | ----------------------- | -------------------------------------------------------------------------------------------------------------------------- |
| `worktree.getRemovalPlan`    | `orchestration:read`    | Resolve the current target and return an opaque versioned token plus dirty, lock, prune-impact, and availability facts.    |
| `worktree.removeFromBibCode` | `orchestration:operate` | Atomically detach BiBCode ownership without asking Git to remove anything.                                                 |
| `worktree.remove`            | `orchestration:operate` | Execute an explicitly chosen Git deletion or stale-registration cleanup, then detach only under the mode's verified rules. |

Before reservation, `Removing`, cleanup, Git mutation, or detach, a new removal
must acquire one of the same 64 cleanup-lifetime slots. Saturation returns the
retryable `cleanup-capacity` error with no side effects. The slot follows the
operation through foreground work, queueing, retry backoff, cancellation, and
shutdown.

Removal planning and execution use the catalog-selected trusted repository
anchor, not an unconditional primary-checkout working directory. A pinned
project may use its reachable primary, a present adopted sibling, or the
lifetime common directory only when that anchor resolves to the durable
repository key and is not the removal target. Execution re-resolves and
revalidates the trusted anchor under the mutation lock after quiesce and before
Git. A missing primary therefore does not transfer trust, and an anchor or pin
change aborts without mutation.

The removal-plan token binds the physical project, thread, repository identity,
worktree or missing-path identity, availability, dirty counts, lock state, and
the sorted normalized paths with their exact prune reasons. Immediately before
mutation, verified stale cleanup reruns the inventory and dry-run and compares
that exact digest. A changed path, reason, lock, directory, repository, owner,
or generation requires a new plan. The server never substitutes recursive
directory deletion for Git worktree removal.

Present worktrees require a separate user choice between detach-only and Git
deletion, plus explicit dirty and prune-impact confirmations when applicable.
A failed present deletion retains BiBCode ownership. For an already missing
worktree, optional verified stale-registration cleanup may fail while the
bounded outcome is reported and detach still succeeds. An accepted removal is
one atomic transaction: same-path panel deletions, canonical thread deletion,
discovery-baseline compaction, and immutable result metadata. Exact retries
therefore replay after the projection disappears. A prepared receipt can
recover a crash after Git mutation only when the exact target is now proven
missing and unregistered.

Cancellation may win while a request is waiting for admission, locks, or the
engine-envelope handoff. After handoff, a runtime-owned operation retains its
command claim, project/repository locks, cleanup slot, reservation, `Removing`
guard, and quiesce/removal ownership until a durable terminal result exists.
WebSocket interruption or socket closure stops only the caller's wait at that
point. Runtime shutdown stops accepting new catalog operations and drains these
owners before catalog and process teardown.

The operation runtime admits at most 64 server-owned mutation lifetimes with a
non-waiting semaphore acquisition before spawn. Saturation returns the
structured retryable `WorktreeOperationError` with reason
`operation-capacity`; shutdown returns `operation-shutting-down`. The permit is
owned through the terminal operation result, is released on completion, and
shutdown closes admission before draining every accepted task. There is no
unbounded waiter queue.

### Audit

While the [Profiler](./overview.md#profiler) is on, the worktree audit records
removal outcomes and scans the catalog. It only observes: it never prunes,
deletes, or refreshes anything.

**Removal outcomes.** `worktree.remove` and `worktree.removeFromBibCode` write
one `worktree.removal` event per outcome, with the mode, success, the error's
kebab-case `reason`, the duration, and whether the admission drain timed out, a
capacity limit was hit, or the client cancelled. A reason that is not plain
kebab-case reads as `unknown`, so no path reaches a fingerprint, and a replay of
an accepted removal records nothing. A failure becomes a
`worktree.removal-failed` finding, fingerprinted by its reason, only when it is
not the protocol working as designed:

| Outcome                                                                                                                                                                                                                                                                           | Finding                            |
| --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------- |
| `stale-plan`, `stale-generation`, `session-running`, `thread-not-found`, `operation-shutting-down`, or `orchestration-failed` after the client cancelled                                                                                                                          | None; the event only               |
| An admission-drain timeout, which answers `session-running`                                                                                                                                                                                                                       | Warn, as `admission-drain-timeout` |
| `cleanup-capacity`, `operation-capacity`, `dirty-confirmation-required`, `prune-confirmation-required`, `locked`, `protected-target`, `environment-unsupported`, `command-conflict`, `ownership-conflict`, `replacement-conflict`, `repository-mismatch`, `workspace-unavailable` | Warn                               |
| `git-failed`, `internal`, `orchestration-failed` without a cancel, `workspace-identity-unavailable`, and any other reason, `unknown` included                                                                                                                                     | Error                              |

**Scan.** The first scan runs when the profiler turns on and then every 10
minutes, at most one at a time; a tick that finds the previous scan's
filesystem work still running, or the catalog not built yet, looks again a
second later. A scan reads the catalog's guards and owned worktrees, the
directories one level under each managed worktree directory (up to 500 per
directory), each project's `git worktree list --porcelain` (8 s per project), and
the working directories of the processes BiBCode owns (5 s). The comparison with
the filesystem and Git is abandoned after 30 s, and only managed directories
whose owners and registrations were all read are compared. The catalog and
process facts are audited regardless, and the scan's `worktree.audit` event says
what was covered (`compared`, `processesRead`). Findings are fingerprinted by
kind and reason, never a path, so each problem is one finding however many
worktrees show it; the redacted path appears in the summary and evidence.

| Finding                          | Severity | Condition                                                                                                                                          |
| -------------------------------- | -------- | -------------------------------------------------------------------------------------------------------------------------------------------------- |
| `worktree.missing`               | Warn     | A catalog entry is `missing-registered` or `missing-unregistered`.                                                                                 |
| `worktree.stuck-removal`         | Error    | A `removing` guard has been held for more than 10 minutes.                                                                                         |
| `worktree.orphan-dir`            | Warn     | A directory in a managed worktree directory has no catalog owner and no Git registration.                                                          |
| `worktree.registration-mismatch` | Warn     | Git registers a worktree in a managed worktree directory, its directory is gone, and no catalog entry owns it.                                     |
| `worktree.held-open`             | Warn     | A process BiBCode owns works inside a missing worktree, inside one stuck in removal, or in a deleted directory under a managed worktree directory. |

`worktree.held-open` reads working directories on Linux and macOS only. On
Windows the process step reads nothing, so the audit event's `processesRead` is
`null` there.

## Protocol, capability, and operations

The live method names and scopes are mirrored in
[`rpc.ts`](../../packages/contracts/src/rpc.ts),
[`methods.rs`](../../apps/server/src/rpc/methods.rs), and
[`scope.rs`](../../apps/server/src/auth/scope.rs). A server advertises the
`worktreeCatalog` capability only when the complete handler set is available.
The client gates subscriptions and every direct, bulk, archived, creation,
retarget, adoption, policy, plan, detach, and destructive-removal command from
the capability read from the negotiated session, and sends the request through
that same session. A false or missing capability makes no catalog-method call.
The sole compatibility fallback is explicit legacy detach-only via ordinary
thread deletion; it leaves Git and files untouched and never invokes a raw
destructive worktree method.

The optional refresh `reason` is additive and separately gated by the
default-false `worktreeCatalogRefreshReason` capability. Current catalog-capable
servers advertise it and accept `focus` or `explicit`; omission remains
Explicit. A new client connected to an older catalog server keeps Focus as the
local scheduler class but omits `reason` on the wire. Manual Retry continues to
omit the field, and clients do not use decode-error retries for negotiation.

Client presentation uses one shared availability selector. Cold/no catalog
status, `present`, and retained `verification-unavailable` state remain usable;
only authoritative `missing-registered`, `missing-unregistered`, and
`removing` disable workspace actions. Sidebar and chat/panel surfaces use the
same rule.

Operational evidence is available through catalog health fields, typed RPC
errors, removal outcomes, and structured warnings for alias resolution,
availability persistence, cleanup, saturation, and reaper failures. Normal
logs identify projects, threads, repositories, and worktrees without granting
clients or logs path authority. See [Connection runtime](./connection-runtime.md)
for reconnect behavior and [RPC and orchestration](./rpc-and-orchestration.md)
for durable command flow.
