# Connection runtime

`@bibcode/client-runtime` gives browser and desktop clients one supervised
connection model for local, manually paired, relay, and SSH environments. The
public package has no root export; callers use focused subpaths such as
`connection`, `authorization`, `rpc`, `relay`, and `state/<domain>`.

## Ownership

- `ConnectionResolver` converts a catalog entry into a `PreparedConnection`.
  It recovers profiles and credentials and performs bearer, DPoP, relay, or SSH
  preparation as required by the target. Every prepared connection retains the
  complete current `ExecutionEnvironmentDescriptor`; an unauthenticated primary
  fetches the public descriptor, and bearer plus fresh or cached DPoP attempts
  fetch it during every preparation rather than inferring it from a saved token.
  The one exception saves a round trip: the web platform fetches the primary's
  descriptor to register it and hands that result to the environment's next
  attempt, which takes it once, within 10 seconds, instead of fetching it again.
  Every later attempt, a reconnect included, fetches a fresh one.
- Environment bootstrap decodes and retains the complete environment
  descriptor on `KnownEnvironment`, including its nullable
  `storageInstanceId`, rather than reducing it to a label and logical ID.
- `ConnectionDriver` reports `preparing`, atomically verifies and if necessary
  persists the prepared descriptor's storage identity, then reports `opening`
  and creates an `RpcSession`. It reports `configuring` after the socket connects
  (for E2EE, after authentication), before the first snapshot and identity check.
  After the session is ready it verifies the descriptor from the first
  `subscribeServerConfig` snapshot through the same identity owner. Only then
  does it report `synchronizing` and return a live lease. A prepared mismatch
  cannot open a socket, and a backend restart between HTTP preparation and
  WebSocket configuration cannot publish synchronization or a live lease.
- From `configuring` on, the supervisor publishes the session, wrapped by
  `makeVerifiedSessionGate`, as `subscriptionSession`. Read subscriptions follow
  it (`subscribe` for the tags in `PRE_VERIFICATION_SUBSCRIPTIONS`, and the
  shell), so they leave alongside the config request rather than one round trip
  after it. Those tags are the server's `read_stream` methods except
  `preview.tunnelOpen` and `previewAutomation.connect`; each buffers up to 16
  values. Everything else, unary or mutating, still reaches the session through
  `session`, which is published only when the attempt connects, so its
  guarantees are unchanged. The gate opens after the identity check passes and
  the supervisor reports `connected`: no data from an unverified server reaches
  domain state, and held data meets a connected environment. `session` then
  publishes the same object, so nothing subscribes twice. An attempt that ends
  unverified clears `subscriptionSession`, which interrupts its followers; their
  held data is dropped. What changes is that a read subscription's request (its
  input, such as a thread ID or cwd) reaches the server before the config's
  identity check; the descriptor's identity check before the socket opened still
  applies.
- `EnvironmentSupervisor` owns desired state, connectivity, retries, the
  prepared connection, and the live RPC session for one environment.
- `EnvironmentRegistry` owns catalog entries and their scoped supervisors. It
  reconciles platform-provided registrations and exposes environment-scoped
  execution to domain state.
- The web Tauri bridge owns the primary bearer exchange promise. It shares one
  exchange across concurrent consumers, clears a failed exchange for retry,
  and invalidates the promise when `desktop:backend-ready` publishes a new
  bootstrap. A rejected exchange clears only its own cached promise; a late
  rejection from before backend-ready cannot discard a replacement exchange.
  The bridge also caches the local topology; backend-ready replaces it, and a
  topology read already in flight cannot write the old address back; if it
  fails, its waiting callers get the restarted topology instead of the error.
  Primary HTTP requests read through that same owner; they do not
  retain a separate bearer promise across a backend restart. This matters
  because a new desktop-bootstrap exchange supersedes earlier sessions of the
  same method. Browser primary requests continue using their session cookie.
- Domain modules under `state/*` consume the registry and expose focused Atom
  constructors. React presentation does not own sockets or retry loops.

For a browser primary behind the configured loopback UI proxy, the web adapter
resolves one topology snapshot through the existing primary HTTP routing policy.
Descriptor discovery, the emitted `PrimaryConnectionTarget` and its registration
cache signature use that effective HTTP address, so later broker preparation
uses the same HTTP route as cookie authentication. The WebSocket address stays
at the snapshot's backend endpoint. Desktop-managed primaries bypass the UI
proxy and retain their native HTTP address and bearer owner. A non-loopback
backend or an unmatched development origin also retains its direct HTTP address.
A change in the effective HTTP address refreshes the registration instead of
reusing a broker target from the previous route; failed or withheld topology
reads keep the existing retention rules. HTTP routing validation is part of that
same topology-read boundary, so an invalid development-origin hint retains the
cached primary and polling continues when the hint is corrected.

`ConnectionWakeups` exposes two streams. `changes` carries credential changes
and `application-active`, which the browser emits only on `visibilitychange`
to a visible document. Window focus alone does not emit it or trigger the
supervisor's environment probes. `focusVisibility` carries coalesced visible
focus/visibility returns for Git Manager's explicit degraded-focus subscription;
the supervisor does not consume this stream. The web adapter shares one
focus/blur/visibility listener set and releases it after its final consumer.
Credential notifications retain their separate stream. Only a server watcher
reporting fallback-required health enables Git Manager query invalidation;
client-runtime does not access browser globals.

Browser wakeups share a sliding buffer of one frame. A slow consumer cannot
backpressure delivery to supervisors. Every frame carries running totals
(application-active count and focus-return sequence), and each consumer tracks
progress from where it subscribed, so coalescing can drop intermediate frames
but never an `application-active` wakeup or a focus return. Intermediate wakeups may coalesce while a
consumer is stalled; wakeups request current-state reconciliation, not replay.

Terminal input preparation, sequenced writes, and cancellation retain the exact
`RpcSession` object. A replacement session cannot inherit queued input. The
physical session owns request admission, including long-lived subscriptions,
and reserves capacity for control operations while terminal input is busy.
The terminal state owner shares one negotiated writer across callers for a
terminal; the renderer's per-terminal scheduler handles coalescing and bounded
pending input. See [Ordered terminal input](./rpc-and-orchestration.md#ordered-terminal-input)
for the wire methods, limits, and failure rules.

The composition root is
[`connection/layer.ts`](../../packages/client-runtime/src/connection/layer.ts).

## Remote-update presentation lifetime

The client-runtime update coordinator retains the per-environment run; React
views do not own its cancellation or restart loop. The web confirmation-request
Atom is separate transient intent, shared by Settings, sidebar, and Retry
toasts. Its root-mounted dialog observes `activeWork` only while open; that
query's zero idle retention gives every confirmation fresh counts. Row/card
status observation pauses during a run, while the rail retains its existing
bounded observation. A restart follows current connection generations without
changing desired intent; explicit user disconnection is respected. Per-boot
identity is never persisted or substituted for storage identity.

## Targets

Canonical targets are defined in
[`connection/model.ts`](../../packages/client-runtime/src/connection/model.ts).

| Target                        | Preparation                                                                                                                                                                                                    |
| ----------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `PrimaryConnectionTarget`     | Uses the host-provided HTTP/WebSocket address and optional primary bearer credential. It is runtime-provided, not persisted as a saved connection.                                                             |
| `BearerConnectionTarget`      | Loads a saved endpoint profile and bearer credential, validates the environment identity, then exchanges/uses authorization.                                                                                   |
| `RelayConnectionTarget`       | Uses the Clerk session and relay to obtain a DPoP-bound environment bootstrap, then prepares direct HTTP/WSS access.                                                                                           |
| `SshConnectionTarget`         | Asks the desktop SSH gateway for a tunnel (`ensureTunnel`, no token), then authorizes with the saved bearer; a missing or rejected bearer is minted once (`mintBearer`) and saved, and a rejected mint blocks. |
| `UnavailableConnectionTarget` | Retains a platform-owned desired environment and its cached projections without an endpoint or credential; preparation fails transiently before transport work.                                                |

Bearer, relay, and SSH targets may be persisted in the connection catalog.
Unavailable targets are reconciled only from host topology and are never
persisted as saved connections.
Profiles and credentials remain separate so catalog metadata can be listed
without exposing secrets.

Pairing-code onboarding accepts an optional client-local display label. It
trims that alias and falls back to the pairing payload's name when blank,
then saves the result in the existing bearer target and profile `label` fields.
The environment rail and saved-server list use the catalog target label, which
survives catalog hydration and reconnects. The alias does not change the remote
descriptor, environment identity, pairing credential, or server-side name.
Without an alias the label is the offer's name or the descriptor label; a
headless web-mode `bibcode serve` server declares its hostname there (see
[Remote architecture](./remote.md#design-rules)).

**Rename…** in **Settings → Remote Servers** rewrites that same target label
for a saved bearer, relay, or SSH environment through
`EnvironmentRegistry.rename`. The label is trimmed and must not be blank. One
catalog transition (`ConnectionRegistrationStore.relabel`) replaces only the
target label in place, so no persisted field or schema change is involved and
profiles, credentials, DPoP tokens, and accepted identities stay as saved. The
registry then republishes the entry without installing a runtime: the live
supervisor, its session, desired intent, and every followed stream are kept, so
a rename neither reconnects nor resynchronizes. The registry maintains
`serviceScopes[id].entry === entries[id]` for every installed scope; rename
updates both references together and updates the supervisor's target Ref. The
Settings row title, rail entry and avatar initials, and selected server's
workspace card use the saved label. Disconnect reasons read the current saved
catalog label from the supervisor's target Ref when the failure is published,
so a rename during a live session also names the next disconnect correctly.
Storage-identity errors still use the server's reported name. When a test
server is paused while BiBCode stays visible, the visible disconnect reason uses
the saved name after the 30-second liveness timeout, followed by an endpoint
timeout; the health-check message does not become visible in this scenario.
Restoring a hidden or minimized window during the pause runs the
application-active health check, whose 15-second timeout can be reported
first.
A failed or missing durable relabel leaves all runtime and persisted-target
bookkeeping unchanged. Platform-managed environments (the primary and
desktop-local backends) are named by their host and cannot be renamed.

## Pinned direct profiles and sessions

`BearerConnectionProfile` additively stores `hostKey` in the existing schema-v1
catalog document. The catalog boundary normalizes missing, empty, and
whitespace-only values to `null`, so no catalog version bump or compatibility
alias is required. Browser storage persists the document normally; the protected
desktop catalog encrypts and restores the same serialized document as opaque
bytes and does not interpret the new field.

`ConnectionResolver` treats a non-null `hostKey` as a hard channel-selection
rule. It fetches only the unauthenticated descriptor, derives `/ws-e2ee`, and
builds `PreparedConnection.e2ee` with the pinned key and one of two auth forms:
`{ kind: "pairing", token }` during verify-then-add or
`{ kind: "bearer", credential }` on later connections. It does not exchange the
credential at `/oauth/token` or request `/api/auth/websocket-ticket`.
`PreparedConnection.httpAuthorization` is `null` whenever `e2ee` is non-null,
preventing an E2EE-only bearer from being projected as plain-HTTP authority.
A null host key preserves the legacy ticketed `/ws` path.

`RpcSessionFactory` wraps the ordinary Effect socket with the Noise NK and
record-layer adapter before constructing the unchanged RPC protocol. The
wrapper completes the handshake, sends the in-channel auth message, validates
`e2ee_authenticated`, and only then exposes RPC bytes. The resulting
`RpcSession.e2eeAuthenticated` effect publishes the authenticated response for
pairing identity checks; it resolves to `null` for non-E2EE sessions. Handshake
identity and authentication failures are blocked connection failures, while a
deadline or transport failure follows the supervisor's bounded transient retry
policy.

The verify-then-add flow uses that session to compare the descriptor, pairing
payload, `e2ee_authenticated` response, and initial configuration identities. The
pairing authentication message carries no confirmation request flag. The server
decides delivery from the consumed grant: an off-host grant is persisted as
`pending-pairing` and returns `pairingConfirmationRequired: true`; an on-host or
legacy grant is immediately `active` and omits the flag. Migration 49 added the
persisted `delivery_state` column. For a pending reply, the client registers the
verified profile and credential, persists the accepted storage identity, then
calls `auth.confirmPairing` on that same bootstrap channel. Only local
persistence and the confirmation transition run uninterruptibly. Closing before
confirmation lets the server revoke the still-pending session; the server also
runs a best-effort age-gated sweep at startup and every 60 seconds, revoking only
pending sessions older than the two-minute grace.

Pairing opens no second pinned socket. A successful confirmation is conclusive
and wakes the registered environment supervisor. When confirmation delivery is
ambiguous, the supervisor's own connection with the saved credential is the
bearer proof. The client observes `EnvironmentRegistry.stateChanges` for at most
30 seconds, interruptibly: `connected` proves activation, while blocked
`authentication`, `host-identity`, or `storage-changed` state conclusively
rejects the credential. A conclusive rejection conditionally rolls back the
registration and accepted identity and fails with `pairing-rejected`; an
inconclusive window retains the durable local authority and leaves later
recovery to the supervisor. Any confirmation-RPC failure remains a pairing
failure: a definitive rejection rolls back before the bootstrap scope closes,
while ambiguous delivery proceeds only through the supervisor proof above.

Compatibility remains additive. A server predating confirmation delivers an
active credential and may reject `auth.confirmPairing` with the exact
authenticated unknown-request-tag defect; the client recognizes only that exact
legacy shape. A prior confirmation-capable server may omit
`pairingConfirmationRequired`, so the client may make the same idempotent
confirmation attempt. Other ambiguous transport or response loss is resolved
through the supervisor proof above rather than fresh handshakes. This prevents a
routing endpoint from substituting a different logical environment or
persistent store after the pinned handshake without leaving a
delivered-but-unpersisted durable client.

Loopback offers can deliver an already-active standard credential without a
pending-confirmation capability. When that pinned handshake does not require
confirmation, an `access:write` denial of the confirmation RPC is resolved
through the same supervisor bearer proof instead of discarding the saved
connection. Pending-confirmation handshakes and off-host scope denials still
roll back. Server authorization rules are unchanged; the fallback never grants
administrative scopes and rolls back a credential that the supervisor rejects.

Presentation uses `connectionTransportSecurity` as the shared policy:
non-null bearer `hostKey` is `e2ee`, null is `unencrypted`, relay and SSH are
`channel-secured`, and primary or desktop-local targets are `local`. Legacy
direct profiles remain usable but surface re-pair guidance instead of silently
claiming encrypted transport.

## Accepted storage identity

The schema-v1 connection catalog additively stores accepted non-null storage
identities without changing its schema version. Browser mode persists the whole
catalog in IndexedDB. A desktop host that advertises protected catalog support
sends the same serialized catalog through the desktop bridge, which protects
and restores it as one opaque value. Desktop hosts without that capability use
the same IndexedDB backend as browser mode; neither path has a second ongoing
storage-identity source.

Catalog transitions use exact raw revisions, not a renderer-owned document
cache. Each lookup or transition starts with a fresh backend read. A mutating
transition decodes that revision, reapplies its transformation, and attempts to
replace the exact serialized value it read. A keep transition compares that
same raw revision without encoding or writing a document. A conflict rereads
and reapplies either transition, with a bounded eight-attempt limit and a
cooperative yield between conflicts. This preserves disjoint target, profile,
credential, DPoP-token, and accepted-identity changes made by separately
constructed stores. Corrupt-catalog recovery never replaces authoritative bytes
implicitly. The catalog owner quarantines the exact corrupt revision when its
backend supports quarantine, publishes structured `recovery-required` health,
and rejects mutations while that revision remains authoritative. An explicit
reset uses exact compare-and-set to install an empty catalog; a concurrent valid
repair wins without being overwritten.

Pairing-add compensation uses that same cross-runtime boundary. The registration
store removes a failed add only when the current durable registration-owned
target, profile, and credential fields still equal the exact registration being
compensated. A conflict rereads and re-evaluates the condition, so a replacement
committed by another tab, WebView, or client runtime wins intact. Compensation
removes only those registration-owned fields; a same-environment DPoP token or
accepted storage identity remains untouched. The compare-only result carries the
current durable catalog entry when another registration won. The environment
registry then retires the failed add's local supervisor and installs that winner
without clearing environment-owned data. When compensation itself removes the
registration, the registry closes the local supervisor and clears owned runtime
data only after the conditional CAS succeeds.

A definitive pairing confirmation rejection or conclusive supervisor bearer
rejection applies the same conditional rule to accepted identity state: it
restores the previous value, or removes the attempt's value, only while the
value written by that attempt is still current. When activation remains
ambiguous, the local registration is retained instead. Registration and
identity rollback are retried from the bootstrap scope finalizer when
interruption prevents the first cleanup pass. A concurrent replacement remains
authoritative and is never reverted by the older failed add.

In browser mode, and in desktop mode when native catalog protection is
unavailable, IndexedDB performs both compare-only and conditional `put`
transitions in `readwrite` transactions on the catalog key, so its transaction
serialization coordinates independent stores, tabs, and WebViews. The
compare-only transaction performs a `get` but never a `put`, including for an
absent catalog. On a capable desktop host, `compareConnectionCatalog` and
`compareAndSetConnectionCatalog` are privileged typed `DesktopBridge` commands.
The Rust catalog owner holds one process-wide mutex across compare-only reads,
protected reads, comparison, and writes; legacy `get`, `set`, and `clear`
commands use the same owner. Desktop bridge contract version 3 introduces the
compare-only command alongside protected compare-and-set and the
`protectedConnectionCatalog` capability. The renderer never emulates either
atomic transition with separate bridge calls. A host without protected CAS
explicitly selects IndexedDB and atomically migrates any legacy renderer-local
catalog into it before clearing the legacy value.

Before exposing a protected version 3 bridge, the desktop adapter collapses a
legacy renderer-local catalog into the native source. It first reads native
storage. If native is absent and the legacy value is non-blank, it attempts one
native CAS from `null` to that exact value, then rereads native whether it won
or lost. An already-present or concurrent native value is authoritative and is
never overwritten by the legacy copy. Only after an authoritative native read
or post-CAS confirmation does the adapter clear the renderer copy. Absent and
blank renderer values require no CAS. Once installed, protected `get`,
compare-only, CAS, set, and clear operations use native storage exclusively;
Keep never falls back to or rewrites renderer storage.

If reading the renderer legacy source, reading native storage, adopting, or
confirming fails, adapter installation preserves both sources and exposes the
catalog capability as unknown. Catalog consumers then fail with a redacted
typed persistence error without writing IndexedDB or clearing either source.
During unprotected-host migration, an already-valid IndexedDB value is the
exact canonical winner. If IndexedDB instead contains corrupt bytes and the
legacy catalog is valid, the renderer replaces those exact corrupt bytes by
CAS and quarantines them before clearing the only valid legacy copy. Migration
uses the same eight-attempt bound as ordinary catalog updates.

Catalog protection capability has three states. Only bridge contract version 3
with an explicit `protectedConnectionCatalog: true` selects protected native
compare-only/CAS, and only version 3 with an explicit `false` selects IndexedDB
and legacy migration. Rejected metadata, older bridge contracts, and missing
capability fields, plus failed protected-source collapse, are `unknown`:
catalog startup and mutation fail with a redacted typed error, without writing
IndexedDB, invoking native clear, or deleting a legacy renderer copy. This
prevents transient metadata failure, migration failure, or version skew from
downgrading a Windows DPAPI catalog into renderer storage.

Once a compare-only or CAS call is issued it is uninterruptible through
transaction/command completion. There is no in-memory catalog publication
afterward, so a caller interrupted around commit cannot leave stale renderer
state that overwrites the durable winner on a later update. On protected hosts,
the native mutex coordinates every WebView in one desktop host. Separate
simultaneously running desktop processes are not yet coordinated by an OS/file
lock and remain a documented residual risk.

Accepted identities use target keys that remain stable when endpoints, labels,
credentials, or local paths change:

| Target  | Accepted-identity key   |
| ------- | ----------------------- |
| Primary | `platform:primary`      |
| Bearer  | `bearer:<connectionId>` |
| Relay   | `relay:<environmentId>` |
| SSH     | `ssh:<connectionId>`    |

Keys never contain bearer/DPoP credentials, endpoint URLs, SSH host/profile
data, or filesystem paths. `AcceptedStorageIdentityStore` exposes an atomic
transition over the shared catalog in addition to direct catalog reads and
writes. The transition callback receives the accepted identity from each fresh
revision and is re-evaluated after every conflict; only the result computed for
the successful revision is returned. First observation, equality, change, and
a version-skewed `null` report are represented as `Bootstrap`, `Accepted`,
`Changed`, and `Unverifiable`. Bootstrap uses CAS to persist the first non-null
identity. Accepted, Changed, and Unverifiable decisions use compare-only and do
not rewrite an existing catalog, create an absent catalog, or depend on the
catalog writer. Equality and unverifiable reports keep the current baseline,
and an unverifiable report never erases it. When concurrent runtimes bootstrap
the same target, the first CAS winner persists its identity and every loser
re-decides against that winner before it can open a session.

`ConnectionDriver` applies this decision table after descriptor preparation
and before `RpcSessionFactory.connect`. Bootstrap persistence must complete
before a socket opens. It applies the table again to the session's cached
initial configuration after `session.ready` and before synchronization or
lease publication, closing the session scope if the WebSocket reached a
different store. An accepted match continues normally, and an unverifiable
`null` continues without deleting a prior baseline. A changed non-null identity
fails with a structured `ConnectionStorageChangedError` that retains the target
key plus both identities; the supervisor publishes it as a blocked state
without publishing prepared/session state or consuming a new snapshot.

Adoption is an explicit `EnvironmentRegistry.acceptStorageIdentity` command.
It is valid only while that environment's current supervisor state is blocked
by the structured storage-change error. The registry conditionally transitions
exactly the error's accepted identity to its reported identity, then signals
one normal retry. If another runtime has already changed the durable baseline,
the transition fails without overwriting that newer value or scheduling the
retry. The operation is serialized with registration/removal for the logical
environment and becomes uninterruptible once it owns that lease, so
cancellation cannot commit the user decision without also scheduling the
retry. Adoption neither clears environment caches nor reads, writes, copies,
or merges either server database.

This identity boundary starts when both sides understand persistent store
UUIDs. An older server reports `null`, and the client deliberately treats that
as unverifiable without erasing an accepted UUID. Likewise, the first
protected release cannot infer whether an earlier release had already selected
a different valid database before either database had a marker. Once a store
publishes an identity and the client accepts it, subsequent switches are
blocked before synchronization. See
[Project data safety and recovery](../guides/project-data-recovery.md) for the
observable platform scenarios and supported recovery actions.

## State and retry policy

Chat attachment staging is owned by `operations/uploadStager.ts` and
`operations/attachmentStaging.ts`. The decision reads `attachmentStaging` from
the actual carrying session's initial config. A turn stays inline when the
capability is absent or all data-URL characters total at most 256 KiB; otherwise
all its attachments stage sequentially over typed `uploads.*` RPCs. No method
probe is sent to an old server.

The stager starts with 64 KiB raw chunks, permits at most two outstanding
appends and adapts measured acknowledgement time toward one second per chunk,
between 16 KiB and 1 MiB. Progress counts acknowledged raw bytes. Incremental
SHA-256 uses bounded clones at outstanding chunk boundaries; it is available
on plain HTTP origins without WebCrypto and supplies its digest on the final
append. After a transport cut, outstanding calls are joined before following
the registry's next session. Capability is checked again, `uploads.get` selects
the retained hash checkpoint, and an unknown or expired upload can begin again
once per file. An impossible checkpoint fails rather than guessing.

Completed-but-unbound files stay alive during long multi-file sends through
one operation-owned, sequential `uploads.get` sweep per minute, bounded to five
seconds. The task is cancelled and joined on every operation exit. Final
validation, including reused send attempts, restages a disappeared file under
the same once-per-file budget already used by active resume; the validation
passes remain bounded by file count. No keepalive task owns reconnect retries.

A separate client command retains completed stages during queued/outstanding
turn admission, before the per-thread dispatch scheduler starts. It shares the
bounded keeper and follows the registry's current session without taking over
reconnect policy. Route disposal does not abort or release an admitting send;
its outcome stops and joins the keeper. Success forgets the consumed stages;
definite failure after navigation restores the captured old draft and releases
unbound IDs, while an in-view failure may retain them for retry.

Transient staged/pending/replay ownership lives in the client-runtime admission
owner instantiated once by application thread state, keyed by environment and
thread. Views only subscribe to its busy state and acquire an atomic send
lease. Route and component disposal do not retire unresolved admission identity,
and a result arriving after unmount updates the same application record a newly
mounted view reads. Multiple views cannot issue independent sends or replays
for a leased thread. No upload, replay or authority state is persisted in drafts.

A captured authority binds the store ID and host identity. The serial command
checks both against the actual carrying session after its lane opens, then
uses that exact session for dispatch; client-only authority never enters the
wire command. Blocked/removed environments and changed host/store identities
leave unresolved intent intact and refuse a fresh send. Authenticated session
or cookie rotation alone does not change the durable/global command receipt:
current orchestration:operate permissions still apply, accepted exact replays
skip binding, and an unaccepted foreign-owner source returns typed not_found.
Fresh legacy inline sends remain usable without staging/store-identity support.

A lost staged-admission reply retains its immutable original command, including
command/message IDs, timestamp and upload references. An explicit Send replays
that exact receipt input before any upload get/restaging. Accepted replays do
not need already consumed stages. Typed not_found proves non-acceptance and
permits fresh IDs on a subsequent send; further ambiguous failures retain the
original replay. Composer edits made meanwhile are kept for review, without
silently being sent as part of that reconciliation. No replay IDs are persisted.

An invocation's AbortSignal remains an interruption, with bounded best-effort
cancellation of its unbound stages. The UI restores work immediately. Completed
stages belong to an in-memory send attempt until durable turn admission, retry,
expiry or abandonment; staged ids never enter persisted composer drafts. This
operation has no whole-upload deadline and does not change supervisor retry
policy or liveness evidence. Legacy inline sends, large editor writes and large
prompts remain subject to the existing whole-request/transport size and silence
limits; this feature makes only capable chat attachment sends resumable.

The supervisor publishes these phases:

- `available`: disconnected and not requested;
- `offline`: requested while network state is offline;
- `connecting`: preparing, opening, configuring, or synchronizing;
- `backoff`: a transient failure is waiting for retry;
- `connected`: the WebSocket is open and the first `subscribeServerConfig`
  snapshot arrived;
- `blocked`: configuration, authentication, permission, capability, or a
  changed persistent store requires an explicit wakeup or user action.

Establishment has 15 seconds from the attempt's start to a connected socket
(for E2EE, an authenticated one). In `configuring`, the first snapshot and
identity check can take up to 120 seconds while the liveness monitor keeps the
socket alive. After 5 seconds, status reads "Receiving settings from <environment>
over a slow connection…". At the ceiling, the attempt fails with "<environment>
took more than 2 minutes to send its settings." and follows the normal retry
ladder. A dead link during `configuring` ends under the liveness rule below,
with "No data from <environment> for 30 seconds. The connection is too slow or
was lost." The slow-setup notice replaces any previous attempt's error in
connection status and clears when the attempt advances or ends.

Transient failures retry after 1, 2, 4, 8, then 16 seconds, with 16 seconds as
the cap, and every delay moves by up to ±15 % so reconnecting clients spread
out. The sequence continues while the connection remains desired. After five
minutes of continuous failure, an environment that is not selected in the
environment rail (`EnvironmentSelection`) retries after 60, 120, then 300
seconds. Connect, retry, network-change and wakeup signals still end any wait
at once, selecting the environment ends its idle wait and returns it to the
normal ladder, and blocked states are unchanged. A stable
30-second connection resets accumulated backoff. Network changes, credential
changes, catalog reconciliation, and explicit retry requests wake the
supervisor. Disconnect and scope closure interrupt in-flight work.

`EnvironmentRegistry` owns desired connection intent outside disposable
supervisor scopes. Registration and startup hydration establish saved
environments as desired; explicit Connect writes `true`, explicit Disconnect
writes `false`, and removal clears the entry. Catalog/profile drift recreates a
scope with the stored intent, so a deliberately disconnected environment does
not reconnect merely because its supervisor was replaced. Passive state lookup
may materialize a cold supervisor for state publication, but it preserves the
stored intent and does not dial while that intent is disconnected. The registry
builds every supervisor, a replacement included, with the stored intent, so the
first state it publishes already carries that intent: a desired environment
starts at `connecting` (`offline` while the network is offline), never at
`available`.

Environment commands may place a deadline around the complete lazy
`runInEnvironment` effect. That deadline includes supervisor acquisition,
readiness, and RPC execution, rather than starting only after a session exists.
Remote update checks use 30 seconds; interruption releases their two-at-a-time
fan-out slot and isolates the timeout to that environment's result.

`RpcSessionFactory` disables protocol-owned reconnects. This is deliberate: one
supervisor owns retry state, status, cancellation, and generation fencing, so a
stale socket cannot silently become current.

Liveness is decided by the client from inbound data alone. The WebSocket that
`RpcSessionFactory` creates records every raw inbound message, including E2EE
records before reassembly, so a large response that is still arriving keeps the
connection alive. After 10 seconds without inbound data the client sends the
RPC `Ping`; after three such intervals (30 seconds, each ±10 % jitter) it closes
the socket with code 4408 and reason `liveness timeout`, logs `liveness-timeout`
with the silent time, and fails the session with a `ConnectionTransientError`
whose reason is `liveness-timeout`. Each wake of the monitor counts at most one
interval. A timer that fires late, because the page was frozen or throttled in
the background or its main thread was blocked, sends its Ping and gives the
server a full interval to answer, so a page that resumes after minutes does not
close a live connection at once. In a throttled page the limit stretches to
three wakes. A close frame from the server reports
`connection-closed`, and an abnormal closure (1006) or socket error reports
`connection-lost`. The protocol is a local copy of Effect's
`makeProtocolSocket` in `rpc/livenessProtocol.ts`; re-check it against upstream
on every Effect upgrade and delete it once upstream exposes a configurable
pinger. The supervisor uses the current saved catalog name in "No data from <environment> for 30 seconds. The
connection is too slow or was lost.", "<environment> closed the connection.",
and "The connection to <environment> was lost."; renaming a connected environment
changes the name used by the next disconnect without replacing its session.
While the supervisor retries,
connection status appends "Reconnecting…", and Git Manager shows "Reconnecting to
<environment>. Git Manager loads when the connection is back."

Each session opens exactly one `subscribeServerConfig` stream. Readiness waits
for its first snapshot, and every later config subscription on that session
replays the same stream (a snapshot of the current config, then live events),
so the roughly 250 KB config crosses the wire once per connection. The stream
starts once the socket is connected (for E2EE, authenticated), so a session
that ends earlier never sends it. Its first snapshot is bounded by inbound
progress (liveness) and the 120-second configuring ceiling, not the 15-second
setup deadline. The `application-active` health probe sends
an RPC `Ping` and succeeds at the next inbound message instead of re-fetching
the config.

Query atoms re-run when a new connection generation arrives. A request cut off
by a transport failure (`RpcClientError`) is re-issued once on the next
connection. After that automatic attempt fails, mount revalidation, periodic refresh,
cache invalidation and later connections keep showing
the failure ("The connection dropped before the result arrived.") until the
user presses Retry, so a payload the link cannot carry is not requested again on
every reconnect. The bookkeeping guarantees:

- One attempt per request runs at a time. Views that share a request wait for the
  running attempt and then decide, so there are never concurrent re-issues.
- An interrupted re-issue is not a failure by itself: when the next evaluation runs
  on the same session, the request keeps the automatic budget and re-issues. When
  that session is gone by then, the interrupt counts as a cut-off (next rule), and
  the request waits for Retry.
- An interrupt counts as a cut-off when that attempt's own session is gone: its
  generation was replaced or is no longer connected. The RPC client interrupts a
  closing session's requests before the drop is reported, so an interrupt that sees
  the session still connected is settled by the next evaluation: the same generation
  means a refresh on the live session and changes nothing; a newer one counts it as
  cut off.
- Known limit of that rule: a request interrupted on a live session (for example, its
  view unmounted) and evaluated again only after an unrelated later reconnect is
  treated as cut off. That costs its one automatic retry, and Retry is still offered.

In the web client, `useEnvironmentQuery` exposes `refresh` for
explicit Retry and Refresh actions and `revalidate` for every automatic re-read
(timers, Git and file signals, opening a view, finished mutations); only
`refresh` clears the latch. Its `requiresRetry` is true only while the rendered
result is the latched failure: `useEnvironmentQuery` reads the latch against the
emission it renders, because the React Compiler re-runs that read only when its
arguments change.
Views format a failed query with one rule (`formatEnvironmentQueryError`): a
cut-off, or an attempt interrupted by its closing session, shows the
connection-dropped copy, an error with a non-blank message shows that message, and
anything else shows the view's own fallback.
The file browser, Git Manager's Changes view and History offer **Retry**, bound
to `refresh`, with any failed load they show in place of their content. Retry
ignores activation but stays focusable (`aria-disabled`) to preserve keyboard
focus while a read runs (**Retrying…**, or History's loading state) and, in
the file browser while the environment is not connected and in Changes when a
read found no session (**Waiting for the connection…**), because those views
read again by themselves once the connection is back. The file preview, the diff panes, the stash list, commit detail
and merge preview do not offer Retry yet.
`requiresRetry` does not decide whether Retry is shown: `usePullRequestsQuery`
uses it so that opening a view neither re-reads a latched failure nor hides it
behind a fresh load.

## Worktree catalog subscriptions

The worktree catalog is capability gated through one exported policy selector.
The client reads `worktreeCatalog` from the connected session's negotiated
configuration and uses that same session for the request, so reconnect cannot
pair an old capability decision with a new socket. A false or missing capability
starts no catalog subscription and makes no catalog refresh, policy, managed-
create, panel-create, retarget, adoption, plan, detach, destructive-removal, or
bulk RPC call. Discovery and destructive-removal controls remain hidden. The
only older-server fallback is an explicitly confirmed legacy detach through
ordinary thread deletion; it leaves Git and files untouched and never invokes a
raw-path destructive method. Active, archived, direct, and bulk entry points use
this same policy.

Focus refresh reasons are negotiated independently through the default-false
`worktreeCatalogRefreshReason` capability. The scheduler retains Focus as its
logical refresh class, but the request builder omits `reason` immediately before
sending through a session that did not advertise support. This preserves old-
server Explicit behavior without merging Focus with a concurrent manual Retry
and without retrying a rejected request on another wire shape.

For a capable environment, `state/worktrees` owns one catalog atom per
`(environmentId, projectId)` with no client idle grace period. The server view
itself owns bounded sharing and pointer-checked 60-second idle eviction after
the final subscription or unary operation releases it.

The RPC stream is latest-value state. Client state accepts only a current
authoritative generation as new catalog content. If a later scan is degraded,
it retains the last authoritative candidate and adopted-workspace arrays while
publishing the new health status. Environment and project grouping in React is
presentation only; it does not merge the scoped catalog sources or grant the
browser authority over paths.

**Environment rail selection.** The web client's left panel carries an
environment rail (`apps/web/src/components/sidebar/EnvironmentRail.tsx`):
Local (the primary environment plus host-managed `local:` desktop backends,
grouped per the `DESKTOP_LOCAL_CONNECTION_ID_PREFIX` convention) and one entry
per saved remote environment. Selection writes `activeEnvironmentIdAtom` and
scopes _presentation only_: the panel filters which environments' projects and
threads it shows, and **Add project** starts on the selected environment. The
Repositories view hides the rail, so Add Project's own host selector (shown
whenever more than one host is available) is how a target is chosen there.
The web entities module remembers the previous non-null primary identity in a
keep-alive atom, so it survives an authentication-gate remount without being
persisted across app reloads. The root reconciles that identity when its primary
config identity changes. On first load it selects the primary only if nothing
is selected; on an identity change it follows the new
primary only if nothing or the old primary is selected. This also runs on
`/agents` and with the mobile rail closed. Routine config emissions and repeated
welcomes preserve any other selection. Startup defaults read the current entity
atom when they run, and welcome navigation requires that the primary is still
selected. A selected saved environment that leaves the catalog falls back to
Local in the rail.
When the target cannot use the native folder dialog, the dialog embeds
`RemoteDirectoryBrowser`, which lists directories through the read-scoped
`filesystem.browse` RPC on that environment's server, while its **New folder**
action uses the operate-scoped `projects.createEntry` RPC, the same write path
Settings already exposes.
The status bar also reads that selected identity for provider usage, refresh,
usage-reset actions, process diagnostics, and terminal counts. Its usage query
and reset overlay remain keyed by environment; the primary connection is not a
fallback for an unselected or unavailable server. Desktop resource presentation
can additionally query the primary local host through its separate diagnostic
source.
Exception: the Agents view (opened from the Agents nav row, whose unread badge
also aggregates across environments) is the single cross-environment surface;
it ignores rail selection by design, and its jump-to-workspace action re-points
rail selection to the row's environment so every other surface remains scoped.
Selection never changes supervisor desired state—connections to other
environments stay live and streaming—and operations on an entity always route
to the entity's own `environmentId` regardless of selection. When a remote
environment is selected, a context card under the brand row shows its
connection status, server version, and compatibility verdict.

One client-runtime presentation selector governs workspace-action availability.
A cold/no-status row, `present`, and retained `verification-unavailable` remain
usable because none proves absence. Only authoritative `missing-registered`,
`missing-unregistered`, and `removing` disable path-dependent actions. Sidebar,
chat, and panel surfaces call this selector rather than reimplementing the
decision.

Subscription acquisition resolves the current supervisor session. When a
connection is replaced after disconnect or reconnect, the session switch ends
the old stream and subscribes again through the new session. Replacing an
environment registration follows the same scoped switch: followed streams
re-bind whenever the registry installs a runtime for the environment
(registration, platform reconciliation, rollback replacement, or detected
catalog drift, even of an identical entry). Registry publication helpers own
entries and their installation tokens: installs advance the token, removals
delete it, and metadata-only renames preserve it. Replacements, including
catalog drift, use the same lease-locked order: close the old scope, publish
the entry and new token, then create the replacement supervisor. Followers wait
for their first registration. Removal releases the inner subscription and
switches to an empty stream while the follower keeps waiting; registering the
same environment ID again re-binds it to the new supervisor. They key on the
installation token, not the catalog entry, so a rename leaves them subscribed.
Window focus and document visibility request one single-flight refresh per distinct physical
project, even if several rows or panels render it.

Managed creation, panel creation, retargeting, adoption, policy, and removal
updates are serialized in the appropriate project/host lanes in the client
runtime; **Add all** is also bounded to four concurrent candidate operations and
one bulk lane per environment. These are responsiveness bounds, not correctness
locks: the server's command receipts, mutation locks, generation checks,
physical identity, and repository verification remain authoritative.

Atom commands accept an optional per-call abort signal. Aborting a running
command disposes its atom, which interrupts its fiber; the RPC client then
sends `Interrupt`, which the server maps to the request's cancellation token.
An invocation aborted while still queued settles when its turn comes and never
starts. On `singleFlight`, a caller that joins a running execution cannot abort
it; on `latest`, the newest caller's abort interrupts the shared run.
Add Project's clone command is `serial` per destination. Against a server
without `vcsCloneReattach`, it is today's single `vcs.clone` call, and **Cancel
clone**, or the dialog unmounting, aborts it, so the interrupt stops the clone.

With the capability, the command (`state/vcsClone.ts`) owns the re-attach loop;
its next-session wait and transport-loss classification live in
`connection/nextSession.ts`, shared with staged attachment uploads.
React owns no retry loop. It sends `detach: true`. On `RpcClientError`,
`EnvironmentRpcUnavailableError`, or an interrupt it did not request (the RPC
client resumes pending calls with an interrupt when the socket closes), it
reports `reconnecting`, waits for the next session as `subscribe()` does, and
re-issues with `attach: true`. The wait follows the environment's current
supervisor through the registry's `followStream`: when the registry replaces
the supervisor (a changed platform registration, for example), the loop takes
the replacement's session, and a removed environment ends the wait. Typed
failures never re-attach. There is no attempt limit and no timer. The loop ends
at an outcome, at an abort, when the environment is blocked, disconnected by the
user, or removed, or on a session without the capability; the last two end with
`VcsCloneStoppedError`.

**Cancel clone** then sends `vcs.cancelClone` on its own command lane, because
the clone holds the destination's serial lane. The cancel waits for a session
that advertises `vcsCloneReattach` (it stops with `VcsCloneStoppedError` on one
that does not). The form stays in **Cancelling…** until both the clone request
and that cancel have settled: the cancel is keyed by destination, so a new
clone into the same folder must not start while a cancel may still be retrying.
The client runtime enforces this, not the dialog. The cancel command records
itself in a module-level registry keyed by environment, exact URL, and leaf
(the parent folder is left out, because only the server can canonicalize its
spelling), and a clone of that URL into that leaf waits for every pending cancel
to settle before it dispatches, even after the dialog unmounted and a new one
mounted. The key compares an explicit `directoryName` exactly, so on a
case-insensitive file system two spellings of one folder name get separate keys;
the dialog never sends one, and the leaf derived from one URL is always the
same. After that wait the clone continues on the environment's current
supervisor: it still fails at once if that supervisor is not connected, and
ends with `VcsCloneStoppedError` if the environment was removed. The cancel
follows the current supervisor the same way the clone does. While the cancel is
pending, the form stays **Cancelling…** and follows the connection: "The clone
stops when <host> reconnects." shows while it is down. After the clone request
settles, a cancel that is still pending no longer moves that line with the
connection; it keeps re-sending, and the form reports once it settles.
Only a failed or stopped cancel ends the clone wait early; otherwise the clone
request reports its own outcome, including a folder that could not be removed.
A clone that finishes while its cancel is pending is not added as a project;
the form names its folder and says that the next **Clone** adds it.
The dialog can be closed while the clone waits for its host, that is while it
shows the reconnecting line or **Cancelling…**, but not while cloning or adding
the project. Closing it, like unmounting it, requests `vcs.cancelClone` unless
**Cancel clone** already did, then ends the wait; the cancel goes out when the
host is back, and the client runtime keeps it registered until it settles.
Closing the dialog's window does the same as best effort: a closing or
disconnected window may never get the cancel out, and then the clone keeps
running on the host (the orphan policy). An abort alone only stops following
the clone.

## Git Manager capability negotiation

The negotiated environment descriptor carries additive Git Manager and request
branch-selection capabilities. Every field decodes to `false` when an older or
third-party server omits it, and current WebSocket server configurations
advertise them as `true`:

| Capability                       | Surface it describes                                                         | Shipped false-or-missing behavior                                                                                                            |
| -------------------------------- | ---------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| `gitManagerReads`                | Panel shell, refs, history, diffs, stashes, and merge preview                | The project route renders **Git Manager Unavailable**, names the missing capability, and starts no Git Manager query or signal subscription. |
| `gitManagerCommitOperations`     | Commit/amend, undo, and whole-file discard                                   | The Changes view remains readable and keeps these controls disabled with an explanatory reason.                                              |
| `gitManagerPartialStaging`       | Partial stage, unstage, and discard                                          | The diff remains readable, but line/hunk mutation controls are disabled with an explanatory reason.                                          |
| `gitManagerBranchSyncOperations` | Branch lifecycle and fetch/pull/push                                         | Declared and advertised, but the current React toolbar does not yet consult this field.                                                      |
| `gitManagerStashMergeOperations` | Stash and merge operations                                                   | Declared and advertised, but the current React repository surfaces do not yet consult this field.                                            |
| `gitManagerMergeIntoOperations`  | `merge-into`: merge into a branch that is not checked out                    | The Merge dialog hides its **Into** picker, so merges target only the checked-out branch.                                                    |
| `gitManagerRewriteOperations`    | Rebase, cherry-pick, squash, reorder, revert, reset, and conflict operations | Declared and advertised, but the current React surface does not yet consult this field.                                                      |
| `gitManagerTagOperations`        | Tag create, delete, and push                                                 | Declared and advertised, but the current React tag surface does not yet consult this field.                                                  |
| `gitManagerLiveSignal`           | `subscribeGitManagerSignal`                                                  | Declared and advertised, but the current React panel does not yet use the field to suppress the subscription.                                |
| `gitManagerPullRequests`         | Explicit pull-request and check reads                                        | Declared and advertised, but the current React provider pane does not yet consult this field.                                                |
| `gitPullRequestBranchSelection`  | Reviewed source/target branches for request creation                         | Default false; the creation dialog and mutation session require support so an older server cannot ignore selected branches.                  |

Consequently, the shipped older-server degradation is complete for the base
panel, commit operations, partial staging, and reviewed request creation. The six remaining feature flags
preserve wire compatibility by decoding false, but they do not yet prevent the
corresponding React controls from issuing an unavailable method. Treat that as
a current compatibility limitation rather than assuming the advertised
feature-by-feature split is fully enforced by the client.

## Shell projection authority

Shell projection keeps connection availability separate from project count.
Each desired environment is `starting`, `synchronizing`, `live`, `degraded`,
`storage-changed`, `recovery-required`, `unavailable`, or
`configuration-error`. A cached snapshot remains the render source while an
environment reconnects or is blocked, but it is not authoritative. Only a
successful shell snapshot transitions the environment to `live`, including a
successful snapshot containing zero projects. The global shell summary permits
the genuine empty-project presentation only after the environment catalog is
loaded, contains at least one desired environment, and every desired
environment is `live` with a snapshot. Missing snapshots may yield no project
rows for rendering, but never establish an empty catalog. Catalog
`recovery-required` health is a configuration error in this projection, so
corrupt persisted bytes cannot be interpreted as a confirmed empty project
list. Health messages are redacted and do not expose raw catalog contents.

Shell authority is fenced to the exact supervisor session and connection
generation. Connection-state and session publication are observed as one
serialized projection, so either publication order starts the same subscription
without letting a prior-session stream become current. Reconnect, blocked,
backoff, session replacement, or generation change immediately retains the
cached snapshot as non-authoritative and requires a new full snapshot. Deltas
are ignored until that snapshot arrives; afterward only strictly newer deltas
may update state or the persistence queue. Duplicate and stale deltas publish
nothing.

## Data boundary

A session becomes ready only after the socket connects and the first
`subscribeServerConfig` snapshot arrives. Current servers publish the same
prepared `storageInstanceId` through that snapshot, `server.getConfig`, and
lifecycle welcome and ready events as through the well-known descriptor. Domain requests resolve the current scoped session
through the registry; they fail or wait according to the domain API instead of
retaining a global client. Removing a saved environment also removes its
registration, profile, credential, supervisor scope, and environment-keyed
client state.

`environmentId` remains the logical routing identity. `storageInstanceId` is
the persistent-store UUID supplied by a current server on its initial
descriptor. The descriptor decoder accepts an omitted field as `null` for
older or third-party remote servers; it still validates any supplied value as
a trimmed non-empty string. The connection catalog can retain the last
explicitly accepted non-null value; an older descriptor reporting `null` never
replaces it. Prepared-descriptor mismatch gating happens before session
creation, and initial-configuration mismatch gating happens before
synchronization, lease publication, or cache consumption. Neither decision is
inferred by the bootstrap helper or authorization token cache.
`bootId` is random per server start and published only in environment descriptors;
it is never persisted or used for storage identity or accepted-identity checks.

See [Remote architecture](./remote.md) for access methods and
[RPC and orchestration](./rpc-and-orchestration.md) for the wire boundary, and
[Worktree catalog](./worktree-catalog.md) for discovery and lifecycle rules.

## Protocol compatibility verdict

`packages/client-runtime/src/connection/compat.ts` computes a `CompatVerdict`
(`compatible`, `legacy`, `server-too-old`, or `client-too-old`) from the
`remoteProtocolVersion` / `minCompatibleRemoteProtocol` pair on the
environment descriptor. The rule is a two-way window: compatible iff the
server's version meets this client's floor and this client's version meets
the server's floor; a descriptor with both fields decode-defaulted to `0`
predates the window and is `legacy` ("Limited compatibility"), with the
existing default-false capability booleans still governing behavior. The
verdict rides the descriptor the resolver fetches on every connection
attempt: `createEnvironmentSessionAtoms(...).compatVerdictAtom(environmentId)`
derives it from the supervisor's prepared connection and is `null` until an
attempt has produced a descriptor. Failed descriptor probes have no separate
cache: retry pacing is the supervisor's existing 1/2/4/8/16 s reconnection
backoff, so a startup burst against an unreachable environment is already
throttled to one attempt per backoff step.
