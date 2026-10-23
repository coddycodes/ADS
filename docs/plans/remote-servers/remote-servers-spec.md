# Remote Servers — Design Specification

Status: **Approved** (decisions confirmed by the user in the design interview, 2026-08-27).
Location: `docs/plans/remote-servers/` (all specification and planning files for this
feature live here, per user instruction).

> **Superseded for current behavior (2026-08-29).** This document remains
> historical design evidence. The approved
> [Remote Server Remediation Round 2 Design](../../superpowers/specs/2026-08-29-remote-server-remediation-round-2-design.md)
> and the living [Remote architecture](../../architecture/remote.md) define the
> current implementation and invariants.

Companion documents:

- `orca-remote-servers-research.md` — primary-source research of the reference
  implementation (called "the reference implementation" hereafter; **no reference-product
  strings may appear in shipped code, UI copy, or identifiers**).
- `bibcode-current-state.md` — survey of the existing BiBCode seams this feature builds on.
- `remote-servers-plan.md` — master implementation plan (phases, ordering, interfaces).
- `mockups/left-panel-switcher.html` — approved UI mockup (Variant B).

## 1. Summary

BiBCode gains a first-class **Remote Servers** capability: any BiBCode server (desktop
in-process or headless `bibcode serve`) can be **shared** with other devices, and any
BiBCode client (desktop or browser) can **connect** to shared servers. The left panel
gains an **environment rail**: an icon rail listing "Local" plus every saved remote
environment. Selecting an environment scopes the entire panel and all operations (add
project, sessions, terminals, git, files) to that environment. A **Remote Servers**
settings section (evolved in place from today's Connections section) manages pairing,
sharing, connected clients, compatibility, and server updates.

The survey (`bibcode-current-state.md`) established that BiBCode is already
multi-environment end to end: `EnvironmentRegistry` supervises one connection per catalog
entry, state atoms key everything by `environmentId`, and the server has real auth
(pairing links, bearer/DPoP tokens, WS tickets, per-method scopes). This feature therefore
**fills gaps in an existing design**: transport security for direct connections, a
pairing-code format with pinned host identity, a protocol-compatibility window, remote
update control, share/exposure ceremony, the rail UI, and one existing bug (broken SSH
pairing bootstrap).

## 2. Decision log

Every decision below was put to the user with alternatives and confirmed on 2026-08-27.

| #   | Decision                                                                                                                                                                                                                                                                           | Alternatives considered                                            | Rationale                                                                                                                                                                                       |
| --- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| D1  | v1 scope = **Connect to a host + Share this host**; no Cloud VM tab at all                                                                                                                                                                                                         | Connect only; all three incl. Cloud VM                             | Connect alone is untestable without Share; Cloud VM drags in provisioning infra unrelated to core value                                                                                         |
| D2  | Feature exists in **both desktop and browser** modes; browser "Share" manages the server the web client is attached to; desktop-only mechanics (bind widening, firewall, SSH) cross `DesktopBridge`                                                                                | Desktop-only; Connect-everywhere/Share-desktop-only                | Client-runtime is shared; the server — not the client — owns share state                                                                                                                        |
| D3  | **Soft switch**: connections to paired servers stay alive in the background; running sessions keep streaming; rail selection scopes the view only                                                                                                                                  | Hard switch (one live connection); connect-on-select               | Long-running agent sessions are the point of a remote runtime                                                                                                                                   |
| D4  | **Entity-ownership routing**: an operation on an entity owned by environment X targets X regardless of rail selection                                                                                                                                                              | Global-mode routing                                                | Matches the reference implementation and BiBCode's existing `environmentId`-scoped state                                                                                                        |
| D5  | UI = **icon rail, mockup Variant B**: 52px edge rail + environment context card in the panel; card collapses when Local is selected                                                                                                                                                | Compact dropdown; horizontal strip; minimal rail (tooltips only)   | Full update/version parity (D9) needs a visible status surface                                                                                                                                  |
| D6  | Rail lists **Local (WSL backends grouped under it via sub-picker)** + each SSH/direct/relay remote as its own entry                                                                                                                                                                | Every backend flat; remotes only                                   | WSL is "this machine" to the user; remotes are not                                                                                                                                              |
| D7  | Settings: **evolve `/settings/connections` in place** into "Remote Servers" with Connect/Share tabs                                                                                                                                                                                | New section alongside; dismantle-and-rebuild                       | One source of truth for the saved-connection catalog (AGENTS.md: no duplicate sources of truth)                                                                                                 |
| D8  | Server-owned settings are **per-environment** (served by the selected runtime); client/UI settings stay on the device                                                                                                                                                              | Everything remote; operational-only                                | Appearance/keybindings belong to the device; providers/orchestration belong to the runtime                                                                                                      |
| D9  | **Full version/update capability** (the reference's surface adapted, not method-for-method parity — see §4.5 scope note): protocol window + compat verdict + "Check for Server Updates"                                                                                            | Handshake only; nothing                                            | User decision (Q6=a)                                                                                                                                                                            |
| D10 | Update install: **two modes** — desktop-hosted servers one-click; headless `bibcode serve` reports manual instructions. Schema reserves a third `supervised` mode                                                                                                                  | Three modes incl. supervised handoff; check-only                   | Supervised handoff is real infrastructure the reference ships only partially; the contract reserves it without a wire change                                                                    |
| D11 | **App-layer E2EE with pinned host identity key** for direct connections (Noise NK); existing auth rides inside the channel                                                                                                                                                         | Reuse plain `ws://` with warning; TLS with pinned self-signed cert | User decision (Q12=b), conceptually derived from the reference design (which uses a NaCl-box scheme; Noise NK is a strengthening); makes direct LAN first-class and safe without cert lifecycle |
| D12 | **Direct connection via pairing code is the first-class Add Server path**; SSH stays first-class; BiBCode Connect relay remains available as-is                                                                                                                                    | Relay-first with direct demoted to "Advanced"                      | Confirmed after research showed the reference uses no relay for server pairing; E2EE (D11) removes the plaintext objection                                                                      |
| D13 | Pairing UX modeled on the reference (QR is a BiBCode addition — the reference's runtime pairing has none): **intent radio** (Another device / This computer only / Custom address), address picker, generating an open-in-browser URL and a `bibcode://pair?code=…` deep link + QR | Single link; keep today's UX relocated                             | The intent radio drives the exposure-safety rule (D14)                                                                                                                                          |
| D14 | **Grant-driven bind widening**: generating an off-host offer rebinds loopback→wide (with rollback + Windows firewall); a "this computer only" grant never widens on later launches; revoking the last off-host grant reverts                                                       | Manual "Allow remote connections" toggle                           | Server never listens wide without a live reason                                                                                                                                                 |
| D15 | **SSH pairing repair is in scope** (restores the bootstrap the desktop SSH launcher invokes)                                                                                                                                                                                       | Leave broken                                                       | Shipping a remote-servers feature on a known-broken SSH bootstrap makes a flagship path a dead end                                                                                              |
| D16 | Naming: settings section **"Remote Servers"**; entities are **environments**; the local one is **"Local"**; product strings "BiBCode"/"bibcode" by context; zero reference-product strings                                                                                         | —                                                                  | User decision                                                                                                                                                                                   |

## 3. Threat model and security boundaries

A paired client gets the full RPC surface of the remote server — file read/write,
terminals, git, process spawn. **Pairing is granting your user account on that machine.**
The UI must say so at pairing time.

Three client cases with different guarantees (this distinction is load-bearing):

1. **Desktop client + pasted/scanned pairing code.** Full guarantee: the pairing code
   carries the host's static public key out of band; the Noise NK handshake authenticates
   the host (key pinning → `host-identity-mismatch` on change) and encrypts everything;
   the device token authenticates the client inside the channel.
2. **Browser client loaded from a trusted origin (e.g. `http://127.0.0.1:3773` local
   server) connecting _out_ to a remote.** Same guarantee as case 1: the page is served by
   a host the user already trusts; pinning holds.
3. **Browser client loaded _from the remote host itself_ over plain HTTP (the
   "Open in browser" URL).** E2EE cannot protect page integrity here: the page — including
   the E2EE code — traveled over the same unprotected channel it is meant to secure. This
   path is only as safe as its load channel (LAN you trust, SSH tunnel, Tailscale, relay
   HTTPS). The Share tab copy must state this, and the generator labels the browser URL
   "for networks you trust" while recommending the pairing code for BiBCode clients.

Other boundaries (all existing, preserved):

- Auth policy stays bind-address-derived (`remote-reachable` when non-loopback,
  `apps/server/src/auth/service.rs:151`); E2EE and pairing tokens layer on top of, never
  replace, the existing pairing-link / bearer / DPoP / WS-ticket / per-method-scope model.
- Credentials at rest stay in the existing stores: server side in the auth SQLite tables +
  `auth/secret_store.rs`; client side in the connection catalog's split profile/credential
  stores. Custody is honest, not renderer-isolated: the client runtime owns its outbound
  connections, so saved credentials are readable by client code — protected native
  storage (DPAPI) encrypts them at rest on Windows desktop, IndexedDB holds them in
  browser mode, and the split profile/credential stores keep secrets out of metadata
  listings. This is the repo's existing custody model, kept deliberately (the reference
  implementation's main-process custody has no analogue for a browser client); the
  residual browser XSS exposure is bounded by the existing CSP/session model and is
  documented in `docs/architecture/connection-runtime.md`. _(Corrected 2026-08-27 after
  external review — the earlier "renderer never sees credentials" claim overstated
  current behavior.)_
- Storage-identity gating (`storageInstanceId` adoption) is unchanged and must be surfaced
  in every new add/connect flow.
- Exposure minimization: loopback bind by default; widening only via D14's grant ceremony
  through the desktop host's exposure machinery (`DesktopServerExposureState`), never a
  bare config edit.
- Revocation: today, revoking a client removes its session and per-request
  reauthorization rejects subsequent calls (`apps/server/src/rpc/session.rs`) — live
  connections are **not** actively terminated. This feature adds active termination:
  revoking a paired client also closes its live WebSocket sessions (Phase 5), so a
  revoked device loses streams immediately, not at its next request. _(Corrected
  2026-08-27 after external review — the earlier wording claimed termination already
  existed.)_

## 4. Pinned cross-phase contracts

These names and shapes are **normative**. Phase plans and implementations consume them
verbatim; renaming requires updating this spec first.

### 4.1 Host identity key

- The server owns a static X25519 keypair, `host_identity`, generated on first use and
  stored via the existing secret store (`apps/server/src/auth/secret_store.rs`).
- Public key encoding everywhere: **base64url, unpadded, of the raw 32 bytes**.
- The key is distributed **only** inside pairing codes (out of band), never published on
  an unauthenticated plain-HTTP surface (publishing it there would reduce pinning to
  trust-on-first-use).

### 4.2 Pairing code (`bibcode://pair`)

Deep link: `bibcode://pair?code=<base64url(JSON)>`. Browser form: the server's own web
client at `<endpoint>/pair?code=<same>` — this route must work for a **fresh,
unauthenticated device**: the code itself carries the one-time credential, so the flow
consumes it to establish the browser session (integrating with the existing pairing
route surface), never gating on a pre-existing session. _(Pinned 2026-08-27 after
external review.)_ JSON payload (schema lives in
`packages/contracts/src/remotePairing.ts`, new file; Rust mirror in
`apps/server/src/auth/` with a TS↔Rust parity test following
`packages/contracts/src/authRustParity.test.ts`):

```jsonc
{
  "v": 1,                       // payload version; unknown v → "unsupported pairing code"
  "endpoint": "http://192.168.1.20:3773",  // advertised HTTP base URL
  "name": "AI-SERVER",          // human label chosen on the Share tab
  "token": "<one-time pairing token>",     // existing auth_pairing_links token
  "hostKey": "<base64url X25519 public key>",
  "reach": "another-device" | "this-computer" | "custom",
  "storageInstanceId": "<uuid>" // for early duplicate/adoption detection
}
```

- `reach: "this-computer"` codes carry a loopback endpoint; the Add Server flow requires
  an explicit tunnel acknowledgement before saving one (loopback classification helper in
  `packages/shared/src/advertisedEndpoint.ts` grows a `classifyPairingEndpoint` export).
- Verification before save: the client performs a live probe (descriptor fetch + E2EE
  handshake + authenticated `server.getConfig`) and classifies failures:
  `unreachable | host-identity-mismatch | pairing-rejected | incompatible | duplicate-storage-identity`.

### 4.3 E2EE channel (direct connections)

- Handshake: **`Noise_NK_25519_ChaChaPoly_SHA256`** — initiator (client) knows the
  responder's static key (`hostKey` from the pairing code). Rust: `snow` crate. TS (both
  browser and desktop webview): `@noble/curves` (x25519) + `@noble/ciphers`
  (chacha20poly1305) + `@noble/hashes` (sha256/hkdf) — pure-JS, no WebCrypto X25519
  dependency. The implementing phase re-verifies current library status before locking
  versions.
- Endpoint: new WebSocket route **`/ws-e2ee`** on the Axum server. The existing `/ws`
  route is untouched (legacy/loopback/relay/SSH clients keep using it).
- Framing: each WebSocket **binary** message is exactly one Noise message.
  Message 1 (client → server): NK `-> e, es`. Message 2 (server → client): NK `<- e, ee`.
  Thereafter each WS binary message payload is one Noise transport ciphertext whose
  plaintext is one **record**: a 1-byte flag (`0x00` final, `0x01` continuation)
  followed by a chunk of at most 65,518 bytes. The concatenated chunks of one
  record sequence are **exactly the bytes the plain `/ws` socket would carry**
  (the RPC protocol is unchanged and unaware of the wrapper). Records exist because
  Noise caps one message at 65,535 ciphertext bytes; a logical message is capped at
  64 MiB reassembled, and violations close the connection. _(Amended 2026-08-27 during
  plan review from the original one-message-per-RPC-frame wording, which the Noise cap
  makes unsatisfiable.)_
- Credential bootstrap happens **inside the channel** — no plaintext-HTTP credential
  exchange for hostKey targets. First client transport message: `e2ee_auth`, in one of two
  forms. First connect: `{"type":"e2ee_auth","pairing":"<one-time pairing token>"}` — the
  server performs the bootstrap exchange and mints the device session _inside the
  channel_, replying `{"type":"e2ee_authenticated","credential":{…bearer…},
  "environmentId":…,"storageInstanceId":…}`; the one-time token is consumed only by this
  successful in-channel exchange, so pre-auth failures (wrong host, transport loss) leave
  it retryable. Subsequent connects: `{"type":"e2ee_auth","bearer":"<stored access
  credential>"}`. No `/oauth/token` or WebSocket-ticket HTTP round-trips occur for
  hostKey targets — the only pre-auth HTTP is the unauthenticated descriptor fetch, used
  as a routing hint only and **re-verified inside the channel** (authenticated
  `server.getConfig` compared against the pairing payload's `environmentId`/
  `storageInstanceId` before anything is persisted). Failure replies:
  `{"type":"e2ee_error","code":"unauthorized"|"protocol"}` then close.
  _(Amended 2026-08-30: the successful exchange consumes the one-time token before
  any post-mint step. Any later failure, including principal-capacity binding, leaves
  it consumed by design; the global reservation precedes the exchange, so capacity
  cannot burn the link before minting.)_
  _(Amended 2026-08-30: delivery confirmation is server-decided from the consumed
  grant. Off-host grants create a `pending-pairing` session and return
  `pairingConfirmationRequired: true`, after which the client calls
  `auth.confirmPairing`; clients send no request flag. Migration 49 shipped the
  persisted `delivery_state` column. See D4 in the
  [Round 3 remediation design](./2026-08-30-remediation-round-3-design.md).)_
  _(Amended 2026-08-27 after external review: the original ticket-over-HTTP bootstrap
  leaked credentials on the plaintext hop the channel exists to protect.)_
- No-downgrade rule: sessions minted through `/ws-e2ee` are recorded with
  `transport: "e2ee"` and are **rejected by the plain `/ws` route and plain-HTTP bearer
  surfaces** — an intercepted or exfiltrated credential cannot be replayed onto an
  unencrypted channel. Loopback, relay, and SSH sessions are unchanged. Recording is
  claims-based: the transport rides as a signed claim on the session token
  (decode-default `"plain"` for pre-existing tokens), so enforcement is restart-safe
  without an `auth_sessions` migration; a persisted column is a possible later hardening,
  not a v1 requirement.
  _(Amended 2026-08-30: migration 49 added the persisted `delivery_state` column to
  `auth_sessions`.)_
- Pre-auth hardening: the reassembled `e2ee_auth` message is capped at 64 KiB (the 64 MiB
  logical-message bound applies only after authentication); handshake + auth must complete
  within the handshake timeout; unauthenticated in-flight E2EE connections are capped.
  Handshake messages must carry empty payloads — a non-empty payload is a protocol
  violation (close). A responder that cannot decrypt Message 1 (wrong pinned key) closes
  with WS close code 4403; an initiator holding a pinned key maps close-4403 or an AEAD
  failure on Message 2 to `host-identity-mismatch`. The encrypted outbound writer adopts
  the plain RPC session's write/join timeout policy — the wrapper must not weaken
  shutdown or reaping guarantees.
- Channel selection rule: a saved direct (Bearer) connection **with** a stored `hostKey`
  must use `/ws-e2ee`; one **without** (legacy) uses `/ws` and its environment surfaces an
  `unencrypted` transport badge with "re-pair to secure" guidance. Relay and SSH targets
  are unchanged (TLS / tunnel encryption respectively).
- HTTP calls for hostKey-bearing targets: the **only** plain-HTTP call is the
  unauthenticated descriptor fetch (`/.well-known/bibcode/environment`), used as a
  routing hint (see the bootstrap bullet above — `/oauth/token` and
  `/api/auth/websocket-ticket` are never used for hostKey targets); everything else rides
  the E2EE WS. If a client-runtime code path performs other HTTP calls against a hostKey
  target, the implementing phase routes them through the RPC channel or documents the
  exception in `docs/architecture/remote.md`.

### 4.4 Protocol compatibility window

New in `packages/contracts/src/environment.ts` (additive, decode-defaulted — older
servers keep working):

```ts
export const REMOTE_PROTOCOL_VERSION = 1;
export const MIN_COMPATIBLE_REMOTE_PROTOCOL = 1;
// ExecutionEnvironmentDescriptor gains (decode-default 0 = "legacy, pre-window"):
//   remoteProtocolVersion: number
//   minCompatibleRemoteProtocol: number
```

Verdict, computed in `packages/client-runtime/src/connection/compat.ts` (new):

```ts
export type CompatVerdict =
  | { kind: "compatible" }
  | { kind: "legacy" } // server predates the window (both fields 0)
  | { kind: "server-too-old"; serverVersion: number; minSupported: number }
  | { kind: "client-too-old"; serverMinCompatible: number; clientVersion: number };
```

Rules (two-way window, mirroring the reference): compatible iff
`serverVersion >= MIN_COMPATIBLE_REMOTE_PROTOCOL (client's floor)` **and**
`REMOTE_PROTOCOL_VERSION (client) >= minCompatibleRemoteProtocol (server's floor)`.
`legacy` renders as "Limited compatibility" and existing capability-boolean downgrade
governs behavior (the window supplements, never replaces, default-false capability
fields). The verdict is computed from the descriptor fetched on every connection attempt
(the resolver already fetches it) and cached on the environment presentation. Failed
probes are throttled by the supervisor's existing reconnection backoff (1/2/4/8/16 s) —
no separate probe-failure cache is introduced. Version fields are constrained to
non-negative integers on the wire. Deliberate divergences from the reference
implementation, kept by design: a single two-way minimum (not separate client/server
floors), and a usable `legacy` verdict with capability-boolean downgrade instead of
failing closed on absent version fields — capability downgrade is BiBCode's existing
documented compatibility invariant. _(Amended 2026-08-27 after external review.)_

### 4.5 Remote update contract

New `packages/contracts/src/remoteUpdate.ts` + WS methods in
`packages/contracts/src/rpc.ts` (+ Rust mirrors and parity-manifest entries):

```ts
export type RemoteUpdateInstallMode = "interactive" | "manual" | "supervised"; // v1 ships the first two
export interface RemoteUpdateSupport {
  installMode: RemoteUpdateInstallMode;
  reason: "available" | "manual-update-required" | "unpackaged-build" | "updater-unavailable";
}
export interface RemoteUpdateSnapshot {
  serverVersion: string;
  latestVersion: string | null;
  state:
    | "idle"
    | "checking"
    | "update-available"
    | "downloading"
    | "installing"
    | "up-to-date"
    | "error";
  error: string | null;
  support: RemoteUpdateSupport;
}
// WS methods (per-method scope in apps/server/src/auth/scope.rs):
//   "updater.status"  → RemoteUpdateSnapshot        (scope: server:read — or the closest existing read scope)
//   "updater.check"   → RemoteUpdateSnapshot        (scope: server:operate — or closest existing operate scope)
//   "updater.install" → RemoteUpdateSnapshot | error "remote_update_manual_required" (same scope as check)
```

- Desktop-hosted servers (`installMode: "interactive"`): `updater.install` requests the
  update through the desktop host's existing updater via a **server→host delegate
  injected at server construction** (following the repo's existing host-observer
  injection pattern) — not `DesktopBridge`, which is the renderer↔host seam and cannot
  carry a request originating from a remote client's RPC. The remote request reuses the
  updater's existing protection-drain path unchanged. _(Seam wording corrected
  2026-08-27 after external review.)_
- Headless `bibcode serve` (`installMode: "manual"`): `updater.check` refreshes the
  server's self-reported version but returns `latestVersion: null` — the server crate has
  no update-feed access (the feed URL lives only in the desktop release config), so
  headless servers cannot discover newer versions in v1; `updater.install` fails with
  `remote_update_manual_required` and the UI shows copy-paste update instructions.
  _(Clarified 2026-08-27 during plan review.)_
- `supervised` is schema-reserved; no v1 implementation.
- `RemoteUpdateSupport` is also embedded in the environment descriptor so the client knows
  before asking; the capability boolean `remoteUpdateControl` (default-false) gates the
  whole surface for older servers. **All three** descriptor producers publish these
  fields (well-known route, `server.getConfig`, and the Connect/relay descriptor in
  `lifecycle.rs`) — editing fewer is a latent bug.
- Scope note (D9): this surface is the reference's update capability _adapted_, not
  method-for-method parity — there is no separate `download` method (install downloads),
  and headless check semantics are as clarified above.
- "Check for Server Updates" (settings) fans `updater.check` across saved environments
  with **max 2 concurrent**.

### 4.6 Exposure (bind widening) state machine

Owned by the desktop host. There is no server→host command channel today, so exposure
_transitions_ require the desktop bridge: in browser mode without `window.desktopBridge`
(and against headless servers, where exposure is whatever the operator bound) the Share
tab still mints offers and revokes clients, but shows exposure state **read-only** with
CLI/desktop guidance instead of widening. _(Clarified 2026-08-27 after external review —
D2's "browser manages the server" means the pairing-grant surface, not remote bind
control.)_

- Persisted per pairing grant: `reach` (from §4.2). Derived desired exposure:
  `wide` iff ≥1 unrevoked **off-host** grant exists, else `loopback`. Off-host means:
  `reach = "another-device"`, or `reach = "custom"` whose endpoint classified off-host
  **at mint time** (a `custom` grant pointing at a loopback endpoint — an SSH tunnel or
  reverse proxy — must not widen; the mint persists the computed off-host flag per grant
  so derivation and generator agree). _(Rule pinned 2026-08-27 after external review
  found the generator and server derivation disagreed on `custom`.)_
  _(Amended 2026-08-30: native exposure widens only for
  `reach = "another-device"`. Every off-host `custom` address is externally managed and
  never drives the native listener. See the
  [Round 3 remediation design](./2026-08-30-remediation-round-3-design.md).)_
- Transitions (through the existing `DesktopServerExposureState` machinery, never a bare
  config edit): generating the first off-host offer → widen (rebind, Windows firewall
  scope update), with **rollback to loopback on bind failure** and the offer generation
  failing visibly. Revoking the last off-host grant → revert to loopback. A
  `this-computer` grant **never** causes a wide bind, including on later launches.
- Widening changes the server's auth policy to `remote-reachable` automatically (existing
  behavior, kept).
- Legacy grants (pairing links/sessions issued before `reach` existed decode as
  `reach = null`): they never cause auto-widening, but they **block auto-revert** — a
  server exposed via today's manual toggle stays exposed until its null-reach grants are
  revoked or the operator narrows explicitly. _(Added 2026-08-27 during plan review to
  protect existing manual-exposure users.)_

### 4.7 SSH pairing bootstrap (repair)

The desktop SSH launcher (`apps/desktop/src-tauri/src/ssh.rs`) invokes a removed CLI
command. Repair: the native CLI gains **`bibcode pairing issue`** (name final; exact
flags owned by the phase plan) which creates a one-time pairing link against a given data
root and prints the pairing credential JSON in the exact shape
`parse_remote_pairing_credential` expects. `ssh.rs` is updated to invoke it, and
`docs/architecture/remote.md`'s "Current limitations" entry is removed in the same change.

### 4.8 UI contracts

- **Environment rail** (new `apps/web/src/components/sidebar/EnvironmentRail.tsx`):
  52px vertical rail. Top: Local entry (with sub-picker when desktop-local WSL backends
  exist — grouping keyed off the existing `local:` connection-id prefix). Divider. One
  entry per saved remote environment (SSH, direct, relay), letter-avatar + status dot
  (`connected` green / `disconnected` gray / `attention` amber for update-available or
  compat-limited / `error` red). Bottom: "Add server…" (opens the Add Server flow) and
  "Manage remote servers…" (deep-links `/settings/remote-servers`). Selection writes the
  existing `activeEnvironmentIdAtom`.
- **Environment context card** (new `EnvironmentContextCard.tsx`, rendered by
  `Sidebar.tsx` under the brand row): hidden when Local is selected; otherwise shows
  name, status, `BiBCode v<serverVersion>` + up-to-date/compat badge, and a ⋯ menu
  (Disconnect / Check for updates / Manage…). "Add project" copy becomes
  "Add project on <name>" when a remote environment is selected.
- **Settings**: nav item renamed to "Remote Servers"; route becomes
  `/settings/remote-servers` with the old `/settings/connections` path redirecting.
  Two tabs: **Connect to a host** (saved servers list with status/version/compat/update
  rows, Add Server, Check for Server Updates, Advanced expander for manual endpoint+token
  entry, connection troubleshooting expander) and **Share this host** (intent radio,
  address picker fed by advertised endpoints, Generate produces browser URL + pairing
  code + QR, paired-clients list with revocation, exposure state). Sections degrade per
  `EnvironmentPresentationPolicy` (no desktop bridge ⇒ no SSH discovery, no
  exposure-widening controls).
  _(Amended 2026-08-30: off-host endpoint observations are emitted with
  `status: "unavailable"` until the listener is wide; tailnet IPv4 status is likewise
  derived from the current exposure mode.)_
- Selection semantics (D3/D4): the rail scopes _presentation_ (which environment's
  projects/threads the panel shows and where "Add project" lands); RPC routing continues
  to follow each entity's `environmentId`. A null/absent `activeEnvironmentId` means
  **Local is selected and the panel filters to Local** — "no selection" must never render
  as "show everything". Exception: the Agents view (opened from the Agents nav row,
  whose unread badge also aggregates across environments) is the single
  cross-environment surface; it ignores rail selection by design, and its
  jump-to-workspace action re-points rail selection to the row's environment so every
  other surface remains scoped. The rail's `attention` (amber) dot covers both
  compatibility-limited and update-available states; Phase 7 wires the update input into
  the Phase 6 dot.

## 5. What this feature is NOT

- No Cloud VM / provisioning surface (D1).
- No server-side multi-environment multiplexing: one server = one environment
  (`projection_projects` stays environment-column-free); Remote Servers is a client-side
  catalog feature.
- No production Node runtime, no new sidecars (AGENTS.md).
- No relay redesign: BiBCode Connect keeps working exactly as today.
- No E2EE for relay or SSH targets (they already have channel security).
- No supervised-headless update installer in v1 (schema-reserved only).
- No new settings framework: the static nav array + route-file pattern stays.

## 6. Failure and lifecycle policies (adopted from the reference, mapped to BiBCode)

- Reconnect stays owned by `EnvironmentSupervisor` (existing 1/2/4/8/16s backoff);
  protocol-level reconnects stay disabled.
- A different `storageInstanceId` at the same endpoint blocks synchronization pending
  explicit adoption (existing `acceptStorageIdentity`), surfaced in the context card and
  server rows.
- Manual **Disconnect** is a client-side latch on the saved environment (supervisor
  desired-state = disconnected); it never deletes credentials. **Remove** deletes the
  catalog entry + credentials and is blocked while the environment owns visible running
  work without an explicit confirmation.
- Compat/status probe failures render as "Status unavailable" with the underlying error
  preserved; retry pacing is the supervisor's reconnection backoff (see §4.4 — no
  separate probe cache).
- Update state is per-environment and survives navigation (atom-held snapshot).

## 7. Documentation obligations (same-patch, per AGENTS.md)

- `docs/architecture/remote.md` — E2EE channel, pairing-code format, exposure ceremony,
  SSH repair (limitation entry removed).
- `docs/architecture/connection-runtime.md` — hostKey on saved connections, `/ws-e2ee`
  selection rule, compat verdict.
- `docs/architecture/overview.md` — protocol window on the descriptor; updater surface.
- `docs/testing/` runbooks — wherever test commands, provider visibility, packaged UI
  flows, or validation evidence change (per-phase check; phases that change none state
  "reviewed and remain accurate").
