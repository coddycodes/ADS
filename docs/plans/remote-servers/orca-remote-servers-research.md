# Orca "Remote Orca Servers" — feature research

Research of the Orca codebase at `/work/github/orca` (v1.4.178-rc.2, Electron app), covering
the BETA "Remote Orca Servers" settings section (tabs: **Connect to a host**, **Share this
host**, **Cloud VM**). All paths below are relative to the Orca repo root unless absolute.

Date: 2026-08-27. Line numbers refer to the working tree at research time.

## Summary

- **Topology.** A "remote Orca server" is a full Orca runtime running on another machine —
  either the desktop app, `orca serve` (the desktop binary headless, Xvfb-backed on Linux), or
  `orcad` (the runtime re-hosted on plain Node). The host exposes a **WebSocket RPC listener**;
  a paired client (another desktop, the web client, mobile, or the CLI) dials it directly.
  There is **no broker/relay for runtime-scope pairing** — the relay subsystem exists but is
  explicitly mobile-only (`src/shared/mobile-relay-pairing-offer.ts:82-91` rejects `relay` on
  `scope: 'runtime'` offers). Reachability is direct TCP: LAN, Tailscale, a reverse proxy, or a
  user-managed SSH local port-forward (loopback links are tagged `connectionDependency:
  'ssh-tunnel'`).
- **Security.** Every connection runs an application-layer E2EE handshake regardless of TLS:
  the client generates an ephemeral Curve25519 keypair, derives a shared key against the host's
  **static public key pinned in the pairing offer**, then sends an encrypted auth frame carrying
  a **per-device bearer token** minted by the host's `DeviceRegistry`
  (`src/shared/remote-runtime-request-websocket.ts`, `src/main/runtime/device-registry.ts`).
  Transport TLS (`wss://` with a self-signed cert) is supported but optional
  (`src/main/runtime/rpc/ws-transport.ts:173-179`).
- **Pairing.** The host mints an "access link" `orca://pair?code=<base64url JSON>` embedding
  `{endpoint, deviceToken, publicKeyB64, pairedDeviceId, scope}`. The client pastes it, Orca
  verifies it live with a `status.get` probe, then persists it as a `KnownRuntimeEnvironment`
  in `<userData>/orca-environments.json` (`src/shared/runtime-environment-store.ts:20`).
- **Protocol surface.** One JSON-RPC-like envelope (zod-validated, `.strip()` for forward
  compat) over the encrypted socket, **575 methods across ~46 namespaces** (`git`, `files`,
  `terminal`, `worktree`, `browser`, `orchestration`, `updater`, `status`, …), plus a binary
  terminal stream and a subscription channel ("shared control" connection).
- **Routing.** Every entity (repo, worktree, terminal, automation) carries an
  `ExecutionHostId` — `'local' | 'ssh:<targetId>' | 'runtime:<environmentId>'`
  (`src/shared/execution-host.ts:8-9`). The renderer resolves a `RuntimeClientTarget` and
  branches: local calls go to the in-process runtime, environment calls go over the paired
  WebSocket (`src/renderer/src/runtime/runtime-rpc-client.ts:82-93`). A registry merges local +
  remote + SSH hosts with health/compat for every host picker
  (`src/shared/execution-host-registry.ts:180`).
- **Compatibility.** A protocol-version window (`RUNTIME_PROTOCOL_VERSION = 3`,
  min-compatible 2 in both directions, `src/shared/protocol-version.ts:34-36`) is checked
  against every host's `status.get` before other RPCs; feature drift within the window is
  negotiated with ~60 named capability strings.

---

## 1. Architecture overview

### Host-side processes

Three ways to be a host, all serving the same runtime RPC:

1. **Desktop app** — the Electron main process hosts the runtime and can share itself
   ("Share this host"). The RPC server lives in `src/main/runtime/runtime-rpc.ts` (1,876
   lines): a WebSocket transport (`src/main/runtime/rpc/ws-transport.ts`), a Unix-socket
   transport for local CLI (`src/main/runtime/rpc/unix-socket-transport.ts`), and a long-poll
   HTTP fallback (`src/main/runtime/runtime-rpc-long-poll-transport.test.ts`,
   `runtime-rpc-websocket-long-poll-caps.test.ts`).
2. **`orca serve`** — the same binary headless. On Linux it auto-starts Xvfb
   (`docs/reference/headless-linux-server.md:5-11`). It prints a one-line JSON readiness
   contract (`type: "orca_server_ready"`, includes `runtimeId`, bound/advertised endpoints and
   a ready-made pairing URL — `docs/reference/headless-linux-server.md:104-120`).
3. **`orcad`** — the runtime re-hosted on plain Node without Electron
   (`src/main/orcad/orcad-entry.ts`, `main.ts`). Its operational contract is
   `docs/reference/orcad-operations.md`: a deployment is **two long-lived processes** — orcad
   (RPC, git, worktrees, persistence) plus a detached **terminal daemon** that owns every PTY
   and deliberately **outlives orcad**, so a runtime restart/update/rollback does not kill
   running terminals (`docs/reference/orcad-operations.md:9-28`). Data root is
   `$ORCA_USER_DATA` → `$XDG_DATA_HOME/Orca` → `~/.orca`, fenced by `<data-root>/orcad.lock`
   with owner/permission checks (`orcad-operations.md:50-75`).

Additionally `src/relay/` is Orca's **SSH relay agent** — a Node program Orca deploys onto
plain SSH hosts (git/fs/pty handlers over an SSH channel). That is the older "Add Remote Host →
SSH" path, distinct from (and complementary to) the Orca-server pairing path; both appear in
the same "REMOTE HOSTS" settings area (`src/renderer/src/components/sidebar/AddRemoteHostDialog.tsx:23`
— `AddRemoteHostMode = 'ssh' | 'server'`).

### Transport

- WebSocket (`ws` library) client-side: `src/shared/remote-runtime-request-websocket.ts`.
- The listener is HTTP or HTTPS: TLS is used **when a cert/key is configured**
  (`src/main/runtime/rpc/ws-transport.ts:173-179`); a self-signed cert is generated once per
  host and its fingerprint pinned by mobile QR pairing
  (`src/main/runtime/tls-certificate.ts:1-4`). Runtime pairing endpoints are typically plain
  `ws://` (`src/main/runtime/runtime-rpc.ts:62-66`); confidentiality/authenticity comes from
  the app-layer E2EE channel either way.
- **Bind policy** is deliberately conservative: default loopback; widened to all interfaces
  only when the user explicitly generates an off-host offer ("STA-2370" comments,
  `src/main/runtime/runtime-rpc.ts:57, 77, 1378-1460`; orcad pins `--bind` to a literal IP and
  refuses hostname binds, `docs/reference/orcad-operations.md:30-48`).
- **No relay/broker for runtime scope.** The `relay` block in a pairing offer (director/cell
  URLs, invite tokens) is validated as mobile-only: `Relay is invalid for runtime scope`
  (`src/shared/mobile-relay-pairing-offer.ts:82-91`). The docs and UI steer users toward
  Tailscale/LAN or an SSH tunnel (`docs/reference/headless-linux-server.md:66-82`,
  `src/renderer/src/components/sidebar/AddRemoteHostDialog.tsx:256-263`).

### E2EE handshake (both request and shared-control sockets)

Client side (`src/shared/remote-runtime-request-connection.ts:198-256`,
`src/shared/remote-runtime-request-websocket.ts:31-98`,
`src/shared/remote-runtime-client-handshake.ts`):

1. Open WS to `pairing.endpoint`; generate ephemeral keypair; derive
   `sharedKey = ECDH(ephemeralSecret, host publicKeyB64 from offer)`.
2. Send plaintext `{type: 'e2ee_hello', publicKeyB64: <ephemeral pub>}`.
3. Host replies `{type: 'e2ee_ready'}`.
4. Client sends **encrypted** `{type: 'e2ee_auth', deviceToken, clientCapabilities}`.
5. Host validates the token against its `DeviceRegistry`
   (`src/main/runtime/runtime-rpc.ts:1703-1740`) and replies encrypted
   `{type: 'e2ee_authenticated'}` (or an `unauthorized` error).
6. All subsequent RPC frames are encrypted with `sharedKey`; binary terminal frames ride the
   same socket.

## 2. Pairing / the "Add Server" flow

### The pairing code

`orca://pair?code=<base64url(JSON)>` or the bare base64url payload
(`src/shared/pairing.ts:14-27, 65-81`). Payload schema (`PairingOfferSchema`,
`src/shared/mobile-relay-pairing-offer.ts:70-102`):

```ts
{
  v: 2,
  endpoint: string,          // ws(s)://host:port
  deviceToken: string,       // per-device bearer token minted by the host
  publicKeyB64: string,      // host's static Curve25519 public key (pinned)
  pairedDeviceId?: string,
  scope?: 'mobile' | 'runtime',
  relay?: {...}              // mobile-only; invalid with scope 'runtime'
}
```

### Client-side add flow

UI: `RuntimeEnvironmentsPane` ("Add Server" form: name + pairing code) and the sidebar's
`AddRemoteHostDialog` (mode `'server'`). Both call
`window.api.runtimeEnvironments.verifyAndAddFromPairingCode({name, pairingCode, allowLoopback})`
(`src/renderer/src/components/settings/RuntimeEnvironmentsPane.tsx:456-540`,
`src/renderer/src/components/sidebar/AddRemoteHostDialog.tsx:240-307`).

Main-process verification (`src/main/ipc/runtime-environment-pairing-verification.ts`):

1. `parseHostAccessLink` (`src/shared/remote-pairing-address.ts:89-147`) — rejects
   mobile-scope links, non-`ws(s)` destinations, and non-connectable hosts (`0.0.0.0`,
   port 0); classifies the endpoint as `loopback | tailscale | lan | public | custom`.
2. **Loopback guard**: a loopback link is refused unless the user checks the SSH-tunnel
   override (`allowLoopback`); accepted loopback environments are saved with
   `connectionDependency: 'ssh-tunnel'`
   (`runtime-environment-pairing-verification.ts:30-36, 65-71`;
   `src/shared/runtime-environment-store.ts:140-155`).
3. **Live probe**: `status.get` over a real E2EE connection with a 15s timeout, then
   `verifyRemotePairingRuntimeStatus`. Failures are classified into
   `access-link-invalid | host-unreachable | host-identity-mismatch | connection-interrupted |
   environment-save-failed` (`runtime-environment-pairing-verification.ts:38-127`) — note
   `host-identity-mismatch` fires when the reached host's key does not match the pinned
   `publicKeyB64` (`pairingStage === 'host-identity'`).
4. Persist via `addEnvironmentFromPairingCode`.

### Persistence (client)

`<userData>/orca-environments.json` (`src/shared/runtime-environment-store.ts:20-37`), a
hardened ("secure file", `0600`/ACL) JSON document capped at 1 MB:

```ts
// src/shared/runtime-environments.ts:23-42
KnownRuntimeEnvironment = {
  id: string,                       // randomUUID at add time
  name: string,                     // user label, unique, used as CLI selector
  createdAt, updatedAt: number,
  pairingRevision?: number,         // bumped on re-pair; guards stale credentials
  pairedDeviceId?: string,
  lastUsedAt: number | null,        // throttled writes (60s) on each round-trip
  runtimeId: string | null,         // learned from the host's status.get
  source?: 'manual' | 'ephemeral-vm',
  connectionDependency?: 'ssh-tunnel',
  endpoints: [{ id, kind: 'websocket', label, endpoint, deviceToken, publicKeyB64 }],
  preferredEndpointId: string
}
```

The renderer only ever sees a **redacted** projection with `deviceToken`/`publicKeyB64`
stripped (`redactRuntimeEnvironment`, `src/shared/runtime-environments.ts:44-53`); credentials
stay in the main process.

### Host-side credential mint

`DeviceRegistry` (`src/main/runtime/device-registry.ts`) is a hardened JSON file in the host's
userData. Each entry: `{deviceId: uuid, name, token: randomBytes(24).hex, scope:
'mobile'|'runtime', pairedAt, lastSeenAt, pairingReach: 'network'|'this-computer'}`. Offer
generation (`src/main/runtime/runtime-rpc.ts:~698-770`) resolves the advertised endpoint
(`src/main/runtime/pairing-endpoint.ts` — host, host:port, or full `ws(s)://` URL; wildcards
refused), reuses or mints a **pending** device entry
(`getOrCreatePendingDevice` coalesces regenerate clicks; `rotatePendingDevice` explicitly
invalidates a leaked never-used token — `device-registry.ts:100-141`), and returns
`{pairingUrl, endpoint, deviceId, webClientUrl}` — `webClientUrl` being a browser URL served by
the host's static web-client handler (`src/main/runtime/rpc/static-web-client-handler.ts`).

## 3. Connection lifecycle

### Connections

Two socket kinds per environment, both E2EE and keyed by the environment's pairing offer:

- **Request connection** (`RemoteRuntimeRequestConnection`,
  `src/shared/remote-runtime-request-connection.ts`) — request/response; lazily opened,
  cached per environment in main (`src/main/ipc/runtime-environment-request-connections.ts`),
  **auto-closes after 60s idle** (`IDLE_CLOSE_MS`, line 38). Requests carry per-request
  timeouts; a timeout closes and re-dials.
- **Shared control connection** (`RemoteRuntimeSharedControlConnection`,
  `src/shared/remote-runtime-shared-control-connection.ts`) — long-lived multiplexed channel
  for subscriptions/events (terminal streams, session-tab sync, file watches). It owns
  **reconnect**: edge-triggered on close with backoff `[250, 500, 1000, 2000, 4000, 8000,
  15000, 30000] ms`, reset after 30s of stable readiness
  (`src/shared/remote-runtime-shared-control-reconnect.ts:40-47`,
  `-stability.ts`); capability gated behind
  `remote-runtime.shared-control.v1` (`src/shared/protocol-version.ts:43`).

### Liveness

- Server pings all clients on an interval; **3 consecutive missed pongs** marks a socket dead
  (one miss is "unknown", tuned for cellular/Tailscale blackholes; comment cites the web
  client's matching 45s budget) — `src/main/runtime/rpc/remote-runtime-server-heartbeat.ts:1-14`.
- Client-side, protocol pings/pongs are the liveness signal for half-open tunnels that never
  deliver `close` (`src/shared/remote-runtime-request-websocket.ts:25-28`,
  `remote-runtime-shared-control-open.ts:22-24`).

### Status, "Connected", "Compatible", version display

The settings pane loads `runtimeEnvironments.list()`, then probes each with
`runtimeEnvironments.getStatus({selector, timeoutMs: 10_000})` → `status.get`
(`RuntimeEnvironmentsPane.tsx:315-423`). Derivations:

- **Connection state** (`getRuntimeServerConnectionState`,
  `RuntimeEnvironmentsPane.tsx:215-231`): `connected` = probe succeeded and compat not
  blocked; `checking` while loading; `disconnected` otherwise. Being "Connected" is explicitly
  independent of being the default **Active Server** (Advanced selector).
- **Compatibility** (`evaluateHostDetails`, `RuntimeEnvironmentsPane.tsx:84-93` →
  `evaluateRuntimeCompat`, `src/shared/protocol-compat.ts:20-54`): compares
  `RUNTIME_PROTOCOL_VERSION`/`MIN_COMPATIBLE_RUNTIME_SERVER_VERSION` against the server's
  `runtimeProtocolVersion`/`minCompatibleRuntimeClientVersion` from `status.get`. Verdict is
  `ok` ("Compatible") or `blocked` with reason `client-too-old` ("Update client") /
  `server-too-old` ("Update server") plus a human description
  (`describeRuntimeCompatBlock`, `protocol-compat.ts:56-64`).
- **Server version** ("Orca v1.4.188") comes from `status.get`'s `appVersion`
  (`src/main/runtime/rpc/methods/status.ts:8-16`); the up-to-date badge from the remote
  updater snapshot (§4).
- `RuntimeStatus` shape: `src/shared/runtime-session-contracts.ts:60-88` — `runtimeId`,
  `pairedDeviceId`, `runtimeProtocolVersion`, `minCompatibleRuntimeClientVersion`,
  `capabilities[]`, `degradations[]`, `appVersion`, `remoteUpdateSupport`, `remoteControl`
  (shared-control diagnostics), `hostPlatform`, `deviceScope`.

### Enforcement on every call

`callRuntimeRpc` preflights every non-`status.get` environment call with a cached
compatibility check (probe `status.get`, `assertRuntimeStatusCompatible`); failures are cached
for 60s so an offline runtime costs one timeout, not one per feature; a reconnect to a
**different `runtimeId`** invalidates the verdict
(`src/renderer/src/runtime/runtime-rpc-client.ts:46-145, 189-209`). Fine-grained drift inside
the compatible window is handled by capability probes
(`runtimeEnvironmentSupportsCapability`, `runtime-rpc-client.ts:282-320`) against ~60
capability constants (`src/shared/protocol-version.ts:38-216`), with an explicit written
contract for what does/doesn't need a version bump (`docs/reference/remote-wire-compatibility.md`).

### Connect / Disconnect / Remove

`src/main/ipc/runtime-environment-connectivity-handlers.ts`:

- `runtimeEnvironments:connect` — clears the manual-disconnect flag and re-probes status
  (lines 121-131).
- `runtimeEnvironments:disconnect` — **non-destructive**: adds the environment id to an
  in-memory `manuallyDisconnectedEnvironmentIds` set, invalidates the cached transports; the
  saved server stays. While set, every call/status answers
  `runtime_manually_disconnected` (lines 27-45, 111-120).
- `runtimeEnvironments:remove` — refused while the environment is the Active Server
  ("Choose another Active Server in Advanced before removing"); otherwise deletes from the
  store, tears down transports, and retires client-hosted browser partition storage
  (lines 85-110). On success `markEnvironmentUsed` records `runtimeId`/`pairedDeviceId`
  learned from live status (`src/main/ipc/runtime-environment-transport-routing.ts:77-83`).

## 4. Server updates ("Check for Server Updates")

Client side: a zustand slice drives the whole flow over ordinary runtime RPC —
`updater.getStatus`, `updater.check`, `updater.download`, `updater.install`
(`src/renderer/src/store/slices/remote-server-updates.ts:20-49`), batched at **max 2
concurrent servers** (`MAX_CONCURRENT_REMOTE_SERVER_UPDATES`, line 18;
`src/renderer/src/runtime/remote-server-update-batch.ts`). "Check for Server Updates" calls
`refreshRemoteServerUpdates` which runs `updater.check` per saved environment; the per-row
badge (up-to-date / update available / unsupported) renders from the returned snapshot
(`RemoteServerUpdateStatus`, `src/renderer/src/components/settings/RemoteServerUpdateStatus.tsx`).

Server side: the four `updater.*` RPC methods
(`src/main/runtime/rpc/methods/updater.ts`) delegate to an adapter
(`src/main/runtime/remote-server-updater.ts`) that `src/main/index.ts:701-706` wires to the
app's real electron-updater. The shared contract
(`src/shared/remote-server-update.ts`):

```ts
REMOTE_SERVER_UPDATE_CAPABILITY = "updater.remote-control.v1";
RemoteServerUpdateSupport = {
  installMode: "interactive" | "supervised-headless-serve" | "unsupported-headless-serve",
  automatic: boolean,
  reason:
    "available" | "manual-service-update-required" | "unpackaged-build" | "updater-unavailable",
};
RemoteServerUpdaterSnapshot = { appVersion, runtimeId, support, status: UpdateStatus };
```

`status.get` itself embeds `appVersion` + `remoteUpdateSupport` so the client knows before
asking whether remote install is possible (`rpc/methods/status.ts:8-16`). How the server
actually installs:

- **interactive** — a desktop host installs like a normal app update.
- **supervised-headless-serve** — a headless `orca serve` under a supervisor: `updater.install`
  writes a handoff file (`{phase: 'install-requested', fromVersion, targetVersion,
  servingPid}`) that an external supervisor watches to restart into the new build; currently
  macOS-gated (`src/main/serve-update-handoff.ts:22-38`; behavioral spec in
  `src/main/updater.headless-serve-install.test.ts`).
- **unsupported-headless-serve** — e.g. a Linux system package or orcad: `updater.check/install`
  throw `remote_update_manual_required` and the UI shows manual-update help
  (`remote-server-updater.ts:25-36`, `getRemoteServerManualUpdateHelp` in
  `RemoteServerUpdateStatus.tsx`).

Mixed versions are the designed steady state; the wire-compat contract and a cross-version
harness (current tree vs newest release tag, both skew directions) enforce it
(`docs/reference/remote-wire-compatibility.md:78-101`).

## 5. "Share this host"

UI: the `share` workflow of `RuntimeEnvironmentsPane` renders `RuntimePairingUrlGenerator` /
`RuntimePairingGeneratorForm` (`src/renderer/src/components/settings/RuntimePairingGeneratorForm.tsx`):

- Intent radio: **Another device** (recommended; Tailscale/LAN address), **This computer
  only** (loopback link), **Custom address** (SSH tunnel / reverse proxy / hostname;
  validated by `parseServerShareAddress`, `src/shared/network/server-share-address.ts`).
- Address picker fed by the host's network interfaces (same list mobile uses), with refresh
  (lines 60-298); **Generate Access Link** produces two artifacts: an "Open in browser" web
  client URL and a "Pair another Orca client" `orca://pair?...` URL (lines 325-366).

Mechanics on generate (main process): resolve advertised endpoint
(`src/main/runtime/pairing-endpoint.ts` — the advertised address never changes the bind; it is
combined with the actual bound port), mint/reuse a pending `DeviceEntry` with
`scope: 'runtime'` and a recorded `pairingReach`, encode the offer
(`src/main/runtime/runtime-rpc.ts:~698-770`). Generating an off-host offer is the explicit
opt-in that **widens the listener** from loopback to all interfaces (rebind ceremony with
rollback at `runtime-rpc.ts:1378-1510`); a "This computer only" grant must never cause a wide
bind on later launches (`pairingReach: 'this-computer'`, `device-registry.ts:26-28, 100-113`).
On Windows the widen also manages a firewall rule scope
(`src/main/runtime/windows-firewall-remote-scope.ts`).

Authorization of connecting devices is exactly the device registry: token lookup on the
encrypted auth frame; `lastSeenAt` bookkeeping (deferred writes)
(`device-registry.ts:205-253`). **Revocation** = `removeDevice` (`device-registry.ts:143-153`);
active connections for a removed device are terminated (`terminateDeviceConnections`
wiring at `runtime-rpc.ts:1420-1424`; behavior pinned by
`src/main/runtime/runtime-rpc-device-revocation.test.ts`). A paired-devices management UI
exists for mobile scope (`src/renderer/src/components/settings/MobilePane.tsx`); for runtime
scope the settings surface is the generator plus disconnect/remove on the client side —
and for orcad specifically, credential administration (list/revoke/rotate, expiring pending
offers) is documented as **not delivered yet**
(`docs/reference/orcad-operations.md:184-188`).

## 6. Cloud VM tab (ephemeral VM runtimes)

UI: workflow `'cloud-vm'` renders `EphemeralVmRuntimesSection` + `CloudVmSetupGuide`
(`RuntimeEnvironmentsPane.tsx:48-50, 283`). Mechanics:

- Users define **recipes** in `orca.yaml` (`OrcaVmRecipe`,
  `src/shared/orca-yaml-hook-types.ts`) — arbitrary user commands for
  `start` / `suspend` / `resume` / `destroy`. Orca spawns them with a JSON context payload
  (instance/recipe/workspace/repo/branch, `src/shared/ephemeral-vm-recipe-runner.ts:23-47`)
  and parses a JSON **result** from stdout.
- The recipe result's `connection` is a discriminated union
  (`src/shared/ephemeral-vm-recipes.ts:66-85`):
  `{type: 'orca-server', pairingCode, projectRoot}` — the VM runs an Orca server and hands
  back a pairing code, which Orca adds as a runtime environment with
  `source: 'ephemeral-vm'` — or `{type: 'ssh', target, projectRoot}` — a full SSH target
  spec (host/port/identity/proxy/port-forwards).
- Provisioned VM records persist in `<userData>/orca-ephemeral-vm-runtimes.json`
  (`src/shared/ephemeral-vm-runtime-store.ts:31`), with status/cleanup state machines,
  resume-integrity checks and failed-start cleanup
  (`src/main/ephemeral-vm-runtime-service.ts`, `ephemeral-vm-resume-integrity.ts`,
  `ephemeral-vm-failed-start-cleanup.ts`).
- VM-created environments are hidden from the user-managed "Connect" list
  (`isUserManagedRuntimeEnvironment`, `src/shared/runtime-environments.ts:97-107`) and their
  SSH targets use a reserved `runtime-ssh-` id prefix so they never appear as user SSH hosts
  (`src/shared/execution-host.ts:61-69`).

## 7. Remote operation routing

### The host-id abstraction

`ExecutionHostId = 'local' | 'ssh:${targetId}' | 'runtime:${environmentId}'`
(`src/shared/execution-host.ts:5-16`). Scoping rules:

- Repos carry `executionHostId` (with legacy `connectionId` fallback → SSH), worktrees carry
  `hostId`; precedence worktree → repo → default local
  (`execution-host.ts:150-172`). Persistence is host-partitioned (e.g.
  `src/main/persistence-host-partitioned-sessions.test.ts`,
  `persistence-duplicate-repo-id-host-scope.test.ts`).
- List RPCs accept a host **scope**: an omitted scope means "this host", `'all'` fans out
  (`requestedExecutionHostScope`, `execution-host.ts:118-122`; e.g.
  `automation.list-host-scope.v1` capability gating in `protocol-version.ts:130-134`).
- `buildExecutionHostRegistry` (`src/shared/execution-host-registry.ts:180-291`) merges: the
  fixed local entry, every saved runtime environment (label = user name), every referenced
  SSH target — each with `health: 'local' | 'available' | 'connecting' | 'blocked' |
  'disconnected' | 'error'` derived from live status + compat verdict + shared-control state.
  This one registry feeds sidebar host pickers, new-workspace host selection, port scanner,
  task preflight, etc.

### Call routing

The renderer resolves `RuntimeClientTarget = {kind: 'local'} | {kind: 'environment',
environmentId}` — `getActiveRuntimeTarget(settings)` maps the global
`settings.activeRuntimeEnvironmentId` (the Advanced "Active Server"), and
`settingsForRuntimeOwner` overrides it per-entity so an operation on a worktree owned by
environment X targets X regardless of the global default
(`src/renderer/src/runtime/runtime-client-target.ts`). Then `callRuntimeRpc(target, method,
params)` branches (`runtime-rpc-client.ts:82-93`):

- local → `window.api.runtime.call(...)` (IPC into the in-process runtime dispatcher);
- environment → `window.api.runtimeEnvironments.call({selector, method, params, timeoutMs,
  expectedEnvironmentPairingRevision})` → main-process transport routing
  (`src/main/ipc/runtime-environment-transport-routing.ts`,
  `runtime-environment-connectivity-handlers.ts:189-228`) → E2EE WebSocket.

The same 575-method surface is served locally and remotely — **the runtime API is
location-transparent**; only the transport differs. Events flow back per environment over the
shared-control subscriptions; the renderer subscribes to the active environment plus every
environment with a live status (`src/renderer/src/hooks/ipc-events/
runtime-environment-subscription-selection.ts:15-29`), and transport gaps trigger project
refresh + SSH-state invalidation for that environment (same file, lines 96-203).

## 8. State/model shape and protocol surface

### RPC envelope

`src/shared/runtime-rpc-envelope.ts`: request = `{id, deviceToken, method, params}`
(serialized then encrypted; `remote-runtime-request-connection.ts:66-73`); response =

```ts
Success  = { id, ok: true,  result, _meta: { runtimeId } }
Failure  = { id, ok: false, error: { code, message, data? }, _meta?: { runtimeId | null } }
Keepalive = { _keepalive: true }
```

All zod-`.strip()`ed so unknown additive fields never break an older peer. Methods are
declared with `defineMethod({name, params: zodSchema | null, handler})`
(`src/main/runtime/rpc/core.ts`; example `src/main/runtime/rpc/methods/status.ts`).

### Method surface (575 methods, by namespace)

Counted from `rg "name: '" src/main/runtime/rpc/methods` (non-test):

| namespace         | n       |     | namespace                                                            | n      |
| ----------------- | ------- | --- | -------------------------------------------------------------------- | ------ |
| browser           | 88      |     | projectGroup / automation / artifacts                                | 7 each |
| github            | 49      |     | projectHostSetup / plugins                                           | 6 each |
| linear            | 41      |     | ssh / settings / preflight / host / folderWorkspace / clipboard      | 5 each |
| orchestration     | 38      |     | updater / hostedReview                                               | 4 each |
| git               | 35      |     | ui / notifications / nativeChat / aiVault                            | 3 each |
| terminal          | 34      |     | workspacePorts / runtime / project / pairing / markdown / agentTeams | 2 each |
| files             | 29      |     | status / stats / network / diagnostics / windows-audit               | 1 each |
| jira              | 23      |     | gitlab                                                               | 21     |
| repo / emulator   | 19 each |     | worktree                                                             | 17     |
| skills / computer | 15 each |     | session                                                              | 13     |
| accounts          | 11      |     | speech                                                               | 8      |

Representative names: `status.get`; `worktree.create/rm/list…`; `git.status/branchDiff/
bulkStage/abortMerge…`; `files.read/write/watch/search…`; `terminal.create/send/show…` (plus a
negotiated **binary terminal stream** with permanent opcodes and capability-negotiated
extensions — `src/shared/terminal-stream-protocol.ts`,
`docs/reference/remote-wire-compatibility.md:31-59`); `updater.getStatus/check/download/
install`; `pairing.getEndpoints/provisionRelay` (mobile relay);
`host.platform`, `host.wsl.isAvailable`… (`src/main/runtime/rpc/methods/host-capabilities.ts`).

### Client-side stores/types recap

- `KnownRuntimeEnvironment` / `RuntimeEnvironmentStore` — `src/shared/runtime-environments.ts`
  (persisted `orca-environments.json`).
- `PairingOffer` — `src/shared/mobile-relay-pairing-offer.ts`.
- `RuntimeStatus` — `src/shared/runtime-session-contracts.ts:60`.
- `RuntimeCompatVerdict` — `src/shared/protocol-compat.ts`.
- `ExecutionHostId`/registry — `src/shared/execution-host.ts`,
  `execution-host-registry.ts`.
- Renderer store slices: `runtime-status.ts` (environments + per-env status map),
  `remote-server-updates.ts` (`src/renderer/src/store/slices/`).
- Host device grants: `DeviceEntry` — `src/main/runtime/device-registry.ts:17-29`.

## 9. Failure behavior

- **Network loss / half-open tunnels**: protocol ping/pong on both ends; server reaps after 3
  consecutive missed probes (`remote-runtime-server-heartbeat.ts`); client shared-control
  reconnects with capped exponential backoff, unbounded retries ("roaming outages are
  unbounded"), OS-resume/online events fast-forward a scheduled retry
  (`remote-runtime-shared-control-reconnect.ts`). Request sockets simply fail pending
  requests and re-dial on next use (`remote-runtime-request-connection.ts:126-149`).
- **Stale sessions / duplicate delivery**: request ids are retired on the shared-control
  channel so a reconnect cannot double-settle
  (`src/shared/remote-runtime-shared-control-retired-request-ids.ts`); terminal create is
  idempotent via `clientMutationId` behind `terminal.create-idempotency.v2`
  (`protocol-version.ts:105-107`); worktree create likewise
  (`worktree.create-idempotency.v1`).
- **Re-pairing races**: every environment call can carry
  `expectedEnvironmentPairingRevision`; the revision guard rejects calls made with a
  superseded credential (`src/main/ipc/runtime-environment-revision-guard.ts`;
  `pairingRevision` maintained in `runtime-environment-store.ts:99-138`).
- **Server restart**: on the host, daemon-owned PTYs survive a runtime restart and the next
  start reattaches (`docs/reference/orcad-operations.md:9-28, 127-134`). On the client, a
  successful status after reconnect re-arms shared control
  (`runtime-environment-transport-routing.ts:77-83`), and a **different `runtimeId`** at the
  same endpoint drops cached compatibility/capability verdicts
  (`runtime-rpc-client.ts:189-209`).
- **Partial streams / mixed versions**: governed by the three wire rules (optional fields OK;
  new opcodes must be negotiated or they vanish silently; changed published content is a wire
  change too), enforced by the cross-version harness
  (`docs/reference/remote-wire-compatibility.md`).
- **Offline environment cost**: failed compat probes are cached 60s so a startup burst pays
  one timeout, not one per feature (`runtime-rpc-client.ts:23-24, 137-144`); probe failures
  render as "Status unavailable" with the error preserved
  (`RuntimeEnvironmentsPane.tsx:383-402`).
- **Manual disconnect** is a client-side latch (`runtime_manually_disconnected`), not a server
  operation — the server cannot distinguish it from network loss (§3).

## 10. Security model

- **Trust boundaries.** Pairing offer = (endpoint, bearer token, pinned host public key). The
  E2EE channel authenticates the host by key pinning (a different host at the same endpoint
  fails as `host-identity-mismatch`) and the client by per-device token. Transport TLS is
  optional hardening; the app layer does not rely on it.
- **What a paired runtime-scope client can do**: everything — the full 575-method surface,
  which includes arbitrary file read/write, terminal/PTY spawn, git, browser automation and
  credential import on the host. Pairing is effectively granting your user account on that
  machine. Scope is the only privilege tier: `mobile`-scope tokens are restricted to an RPC
  allowlist and denied everything else with code `forbidden`
  (`src/main/runtime/runtime-rpc-mobile-method-allowlist.test.ts`;
  `runtime-rpc-client.ts:39-44`); legacy registry entries without a scope default to the
  weaker `mobile` (`device-registry.ts:284-289`).
- **Token lifecycle**: random 24-byte tokens; minted pending → bound on first use
  (`lastSeenAt` 0→nonzero); explicit rotation of never-used tokens ("Regenerate");
  revocation by device removal, which also terminates live connections. **No expiry** on
  device tokens (the only TTL in the system is the 10-minute mobile relay invite,
  `mobile-relay-pairing-offer.ts:13, 57-66`). Credential administration for headless orcad
  hosts is an acknowledged gap (`docs/reference/orcad-operations.md:184-188`).
- **At-rest**: both sides store credentials in permission-hardened JSON
  (`writeSecureJsonFile` — `0600`/`0700`, PowerShell ACLs on Windows; orcad refuses to start
  on a data root owned by someone else or that cannot be tightened,
  `orcad-operations.md:50-67`). Orca's renderer never receives tokens (redaction).
- **Exposure minimization**: loopback bind by default; widening requires an explicit off-host
  offer and is recorded per grant (`pairingReach`) so it never persists beyond intent;
  wildcard/port-0 endpoints are unconnectable and refused in links; loopback links require an
  explicit SSH-tunnel acknowledgement on the client.

---

## Mapping notes for BiBCode

Context: BiBCode is Rust/Axum server + React web + Tauri desktop; desired UX is a server
switcher in the existing left panel — "Local" plus remote environments — with all project
operations scoped to the selected environment.

### Maps cleanly

- **`ExecutionHostId` ≙ the left-panel switcher.** Orca's `'local' | 'runtime:<envId>'` union
  (`src/shared/execution-host.ts`) plus `buildExecutionHostRegistry` (one pure function from
  {saved envs, live statuses, settings} → labeled entries with health) is exactly the model
  for a "Local + remotes" switcher. BiBCode already has typed contracts in
  `packages/contracts`; an `ExecutionHostId`-like tagged string and a registry selector fit
  there naturally. Orca's crucial detail: **entities carry their host id** (repo
  `executionHostId`, worktree `hostId`), so selection is scoping/filtering, not a global mode
  — switching the panel never re-targets an existing project.
- **Pairing offer shape.** `{endpoint, per-device token, pinned host public key}` encoded as
  a paste-able URL, verified by a live `status.get` before saving, is transport-agnostic and
  small. BiBCode's server is already a network server (Axum + WS), so the "Share this host"
  side is _simpler_ than Orca's: no listener-widening ceremony is needed if the server
  already binds deliberately; keep the per-device token registry + pending/rotate/revoke
  semantics (`device-registry.ts` is a good, small spec).
- **Version/compat policy.** The three-number window (`RUNTIME_PROTOCOL_VERSION`,
  min-compatible in both directions) + capability strings + "absent field = protocol 0" +
  `.strip()`-style tolerant decoding (`protocol-compat.ts`, `protocol-version.ts`,
  `remote-wire-compatibility.md`) is directly portable to BiBCode's schema-first contracts
  (effect/Schema instead of zod). Adopt the status-preflight-with-cache pattern
  (`runtime-rpc-client.ts`) rather than checking per feature.
- **Connection lifecycle**: idle-closing request channel + one long-lived subscription
  channel with edge-triggered reconnect, server ping with 3-missed-probe reaping, and the
  `runtimeId`-change cache invalidation. BiBCode already has a WebSocket RPC and
  reconnect/partial-stream priorities (AGENTS.md core priorities), so these are policies to
  copy, not new machinery.
- **`connectionDependency: 'ssh-tunnel'` + loopback classification**
  (`remote-pairing-address.ts`) — cheap, high-value UX guard worth copying verbatim.
- **Daemon-outlives-server invariant.** Orca's strongest operational idea: PTYs live in a
  detached daemon so server restart/update never kills running agent work
  (`orcad-operations.md`). BiBCode's server already owns process supervision; if remote
  servers are expected to update in place, this separation (or an equivalent
  survive-restart story for sessions) is the thing to design early — it also determines what
  "Check for Server Updates → install" can promise.
- **Remote update surface**: `updater.getStatus/check/download/install` + a
  `RemoteServerUpdateSupport{installMode, reason}` in the status payload, with an honest
  `unsupported → manual instructions` path, translates directly.

### Does not map / differs

- **Who holds credentials and dials out.** In Orca, the Electron **main process** is the
  router: the renderer calls IPC (`window.api.runtimeEnvironments.call`), main holds
  `orca-environments.json` (tokens) and owns the E2EE sockets; the renderer only ever sees
  redacted environments. BiBCode's browser client talks to its **own local Rust server** —
  so the equivalent seam is in Axum: the local `bibcode` server should store paired-server
  credentials, own the outbound connections, and proxy/scope RPCs, keeping tokens out of the
  web client entirely. In pure-browser mode (no local server) this proxying is impossible —
  note Orca's web client is _served by the remote host itself_ (`webClientUrl`), which is a
  clean answer BiBCode could reuse: the remote server serves the web UI directly instead of
  being proxied.
- **App-layer E2EE.** Orca needs Curve25519-over-plaintext-WS largely because its default
  transport is un-TLS'd `ws://` between peers. BiBCode could instead standardize on TLS (or
  keep the E2EE design if "paste a link, works over any tunnel" matters). Either way, keep
  the **pinned host identity key** concept — it is what turns a bearer link into a mutually
  authenticated pairing.
- **The 575-method monolith.** Orca serves its entire runtime API remotely because desktop
  and server are the same process; location transparency falls out for free. BiBCode's
  server is already the API boundary, so the equivalent move is different: make the _web
  client's existing RPC_ connectable to N servers (connection manager keyed by environment in
  `packages/client-runtime`) rather than tunneling one server through another. Orca's
  per-request `selector` + `_meta.runtimeId` envelope is still the right wire pattern for
  attribution.
- **Relay**: Orca has no relay for desktop-to-server pairing (mobile-only); BiBCode already
  has relay infrastructure ambitions (AGENTS.md mentions relay integration/Alchemy) — that
  is a place BiBCode can exceed Orca rather than copy it.
- **Electron-specific machinery** — client-hosted browser pages/leases, mobile E2EE v2
  framing, Xvfb-for-headless — has no BiBCode analogue; skip.
- **SSH-relay-agent mode** (`src/relay/`): Orca's second remoting path (deploy a Node agent
  over SSH). BiBCode's single-binary Rust server makes "install bibcode on the remote and
  pair" strictly simpler; probably no need for an agent-injection path.

### Suggested minimal concept set for BiBCode

1. `RemoteEnvironment {id, name, endpoint, deviceToken, hostPublicKey, pairingRevision,
   source}` persisted server-side (secure file or existing DB), redacted toward the UI.
2. Host: device registry (mint/pending/rotate/revoke), pairing-link generation, `status.get`
   with `{serverVersion, protocolVersion, minCompatibleClient, capabilities[]}`.
3. Client: verify-then-add flow with classified failures; per-environment connection manager
   (request + subscription channels, reconnect policy); compat preflight cache.
4. UI: host registry selector in the left panel; health dot + compat verdict; non-destructive
   disconnect; remove blocked while active.

---

## Errata (2026-08-27, from external cross-verification against the reference source)

An independent review verified this document against the reference repository and found
these corrections. The body above is kept as originally researched; where they conflict,
this section wins:

1. **Exposure does not auto-revert in the reference.** §10's "widening … never persists
   beyond intent" overstates it: the reference explicitly never narrows the listener back
   after widening (`src/main/ipc/mobile.ts:156`); revocation removes the device and
   terminates its sockets only. BiBCode's grant-driven auto-revert (spec §4.6) is an
   intentional improvement, not parity.
2. **No long-poll HTTP fallback transport.** §1's transport description mischaracterizes
   "long-poll": the cited tests exercise long-running RPCs with keepalives over the
   framed Unix/named-pipe transport and the WebSocket transport — an operation mode, not
   an HTTP transport.
3. **Self-signed TLS is dormant code, not shipped behavior.** The WS transport can accept
   TLS material (`ws-transport.ts:173`) but production construction supplies none, the
   TLS-certificate module has no non-test caller, and pairing offers carry no certificate
   fingerprint.
4. **The cross-version harness covers terminal streaming only** — it explicitly excludes
   session tabs, agent publications, file/Git RPC, E2EE, and relay
   (`docs/reference/remote-wire-compatibility.md:78`). §4/§9 describe its enforcement too
   broadly.
5. **The E2EE scheme is a NaCl box construction, not Noise.** The reference sends an
   ephemeral public key and derives one shared key against the pinned static host key
   with random-nonce `nacl.box.after` frames (`e2ee-crypto.ts`). BiBCode's Noise NK
   choice is a strengthening inspired by, not copied from, the reference.
6. **Failed compat probes are reused only opt-in** (`reuseRecentCompatibilityFailure`);
   normal foreground calls retry immediately. §9's unconditional 60-second description is
   too strong.
7. **Runtime-server pairing has no QR** in the reference; QR belongs to the separate
   mobile surface. BiBCode's Share-tab QR is an addition.
8. **BiBCode relay is shipped, not an "ambition"** — relay production code exists
   (`apps/server/src/production/mod.rs`) and BiBCode Connect is a documented active
   control plane (`docs/architecture/remote.md`).
9. **The credential-custody mapping note (§"Does not map")** recommends Axum-owned
   custody with a redacted renderer; BiBCode deliberately keeps its existing
   client-runtime custody model instead — see spec §3 for the honest custody statement
   and rationale.
