# BiBCode current-state survey for a "Remote Servers" feature

Date: 2026-08-27. Historical survey — verify paths against current source before
reuse (see `docs/plans/README.md` conventions).

## Summary

The headline finding is that **most of a "Remote Servers" feature already
exists as architecture**. BiBCode is not a single-connection app:

- The client runtime (`packages/client-runtime`) is already multi-environment.
  `EnvironmentRegistry` owns one supervised connection per catalog entry, and
  three remote target kinds — direct **Bearer** endpoints, **Relay** (BiBCode
  Connect), and desktop-managed **SSH** — are already persistable saved
  connections (`packages/client-runtime/src/connection/model.ts`).
- Client state is already keyed per environment: `ScopedProjectRef` /
  `ScopedThreadRef` carry `environmentId` everywhere
  (`packages/contracts/src/environment.ts`), and the atom layer keys queries,
  commands, and caches by environment.
- The server already has a real auth model (pairing links, bearer/DPoP access
  tokens, WebSocket tickets, per-method scopes) and an explicit policy matrix
  that switches when the bind address is non-loopback
  (`apps/server/src/auth/service.rs`).
- A settings surface for remote backends already exists:
  `apps/web/src/components/settings/ConnectionsSettings.tsx` has an
  "add backend" dialog with `savedBackendMode: "remote" | "ssh"`, pairing-link
  management, relay integration, and SSH host discovery.

So the work for "Remote Servers" is largely **filling gaps in an existing
design**, not carving new seams. The sharpest gaps: fresh SSH setup is broken
today (it invokes a removed CLI command), the server has no TLS of its own,
and a handful of UI conveniences still assume the primary environment (e.g.
the sidebar editor list).

Authoritative living docs read for this survey:
`docs/architecture/overview.md`, `docs/architecture/connection-runtime.md`,
`docs/architecture/remote.md`, `docs/architecture/rpc-and-orchestration.md`,
`docs/reference/workspace-layout.md`, `docs/README.md`.

---

## 1. Left panel (projects sidebar)

**Components.** The left panel is `apps/web/src/components/Sidebar.tsx`
(~large single file with per-project and per-thread row components), hosted by
`apps/web/src/components/AppSidebarLayout.tsx`. Supporting pieces:

- `apps/web/src/components/ProjectFavicon.tsx` — project row icon.
- `apps/web/src/components/sidebar/SidebarProjectAvailability.tsx` — worktree
  availability warnings per project row.
- `apps/web/src/components/ThreadStatusIndicators.tsx` — thread status, PR
  state, worktree indicators.
- `apps/web/src/components/CreateWorktreeDialog.tsx` — new worktree rows.
- Grouping logic: `apps/web/src/state/environments.ts` (via
  `useEnvironments`/`useEnvironment`), `apps/web/src/logicalProject.ts`
  (logical/physical project keys across environments, exercised by
  `apps/web/src/environmentGrouping.test.ts`), and
  `@bibcode/client-runtime/state/projectGrouping`
  (`packages/client-runtime/src/state/projectGrouping.ts`). Projects from
  different environments can be _grouped_ for presentation; grouping is
  presentation-only and never merges server-side catalogs
  (`docs/architecture/connection-runtime.md`, "Worktree catalog
  subscriptions").

**Listing.** `Sidebar.tsx` reads `useProjects`, `useThreadShells`,
`useServerConfigs` from `apps/web/src/state/entities.ts`, which projects
environment-keyed atoms (`environmentProjects` from
`apps/web/src/state/projects.ts`, `environmentThreadShells` from
`apps/web/src/state/threads.ts`). Rows are already environment-aware: a
thread row resolves its environment via
`useEnvironment(thread.environmentId)` and renders a remote environment label
when the thread does not belong to the primary environment (`Sidebar.tsx`
~lines 552–563, including the `isDesktopLocalConnectionTarget` check for
host-managed local backends).

**Adding a project.** The flow lives in
`apps/web/src/components/add-project/`:

- `AddProjectDialog.tsx` + `AddProjectSteps.tsx` — dialog UI (folder pick,
  create, clone), also reachable from
  `apps/web/src/components/CommandPalette.tsx`.
- `useAddProjectWorkflow.ts` — wires dialog state to commands. It obtains
  `createProject` via `useAtomCommand(projectEnvironment.create, ...)`
  (~line 561) and passes an explicit `environmentId` with every call.
- `addProjectOperations.ts` — pure orchestration of
  `createProject`/`cloneRepository`/`openProject`; every operation input
  carries `environmentId` (lines 27–49), so add-project is already
  environment-scoped.

**RPC path.** `projectEnvironment.create` comes from
`packages/client-runtime/src/state/projectCommands.ts`
(`createProjectEnvironmentAtoms`, ~line 89:
`environment-data:commands:project:create`), which calls `createProject` in
`packages/client-runtime/src/operations/commands.ts` (~line 83). That
dispatches a durable orchestration command `{ type: "project.create" }`
through the generic `orchestration.dispatchCommand` RPC on the
environment-scoped session. Removal is the sibling `project.delete` command
(same file). Server-side admission is `OrchestrationEngine` via
`apps/server/src/production/orchestration_rpc.rs`.

---

## 2. Client connection architecture

**Already multi-environment.** This is the single most important answer for
Remote Servers: the server URL is _not_ singular. The connection runtime
(`packages/client-runtime`, docs in
`docs/architecture/connection-runtime.md`) is built around a catalog of
environments, each with its own supervised connection:

- **Targets** (`packages/client-runtime/src/connection/model.ts`):
  `PrimaryConnectionTarget` (host-provided `httpBaseUrl`/`wsBaseUrl`, never
  persisted), `BearerConnectionTarget` (saved endpoint profile + separately
  stored pairing credential), `RelayConnectionTarget` (BiBCode Connect,
  Clerk + DPoP), `SshConnectionTarget` (desktop-managed SSH forwarding), and
  `UnavailableConnectionTarget` (platform-owned desired-but-endpointless, e.g.
  a failed secondary WSL backend). `PersistedConnectionTarget` = Bearer |
  Relay | SSH.
- **Catalog** (`packages/client-runtime/src/connection/catalog.ts`,
  `profileStore.ts`, `credentialStore.ts`): schema-v1 catalog persisted in
  IndexedDB in browser mode, or protected native storage (DPAPI on Windows)
  through the desktop bridge; profiles and credentials are stored separately
  so metadata can be listed without secrets. Exact-revision CAS transitions,
  corrupt-catalog quarantine, and accepted `storageInstanceId` baselines are
  all owned here (`storageIdentity.ts`).
- **Resolver** (`connection/resolver.ts`): converts a catalog entry into a
  `PreparedConnection`, performing bearer/DPoP/relay/SSH preparation and
  fetching the full `ExecutionEnvironmentDescriptor` on every attempt.
- **Driver** (`connection/driver.ts`): preparing → storage-identity check →
  opening (`RpcSessionFactory`) → `server.getConfig` re-check →
  synchronizing → live lease.
- **Supervisor/registry** (`connection/supervisor.ts`, `registry.ts`):
  `EnvironmentSupervisor` owns desired state, retries (1/2/4/8/16 s backoff),
  and the live RPC session for one environment; `EnvironmentRegistry` owns
  catalog entries and their scoped supervisors and exposes
  environment-scoped execution to `state/*`. Composition root:
  `packages/client-runtime/src/connection/layer.ts`.
- **RPC** (`packages/client-runtime/src/rpc/`): `protocol.ts` builds the
  Effect RPC client from the schema-only `WsRpcGroup`
  (`packages/contracts/src/rpc.ts`); `session.ts` gates readiness on the
  socket plus a successful `server.getConfig`; `http.ts` handles HTTP calls.
  Protocol-level reconnects are deliberately disabled — the supervisor owns
  retry (`docs/architecture/connection-runtime.md`).
- **Authorization** (`packages/client-runtime/src/authorization/`):
  `remote.ts` exchanges bearer/DPoP credentials, obtains WebSocket tickets,
  and sets `url.pathname = "/ws"` (lines 187, 210); `tokenStore.ts` caches
  tokens.
- **State** (`packages/client-runtime/src/state/*`): domain atoms
  (projects, threads, shell, vcs, worktrees, terminal, …) that resolve the
  current scoped session through the registry per call — there is no global
  client object.

**WebSocket URL derivation.**

- Generic derivation: `deriveWsBaseUrl` in
  `packages/shared/src/advertisedEndpoint.ts` (http→ws, https→wss), plus
  `packages/shared/src/remote.ts` normalization; the resolver then appends
  `/ws` (`packages/client-runtime/src/connection/resolver.ts:58`).
- **Browser mode**: the primary target comes from
  `apps/web/src/environments/primary/target.ts` with sources `"configured"`
  (explicit config), `"window-origin"` (derived from `window.location`), or
  `"desktop-managed"`.
- **Desktop mode**: the Tauri host starts the backend and publishes a
  `DesktopEnvironmentBootstrap` with explicit `httpBaseUrl` and `wsBaseUrl`;
  the renderer reads it through the bridge (`getLocalEnvironmentBootstraps`,
  see `apps/web/src/connection/desktopLocal.ts` and
  `useDesktopLocalBootstraps.ts`). Secondary desktop-local backends (e.g. a
  parallel WSL server) are registered as _bearer_ targets whose connection id
  carries the `local:` prefix (`DESKTOP_LOCAL_CONNECTION_ID_PREFIX` in
  `desktopLocal.ts`) — i.e. even the desktop's own extra backends already go
  through the generic remote-connection machinery.
- Remote bearer/DPoP connections never put long-lived credentials in the URL:
  they carry only a short-lived `wsTicket` query parameter
  (`docs/architecture/rpc-and-orchestration.md`, "Session establishment").

---

## 3. Server side (apps/server)

**Project persistence and scoping.** Projects are SQLite projections:
`projection_projects` (`apps/server/src/persistence/migrations.rs` ~line 908)
with columns for `project_id`, `title`, `workspace_root`, model selection,
scripts, and worktree discovery JSON, plus a separate
`project_worktree_repository_pins` table (~line 2305). Reads/writes in
`apps/server/src/persistence/repositories.rs` (`list_projects` ~line 331).
There is **no environment column**: one server process _is_ one environment
("A BiBCode server represents one execution environment",
`docs/architecture/remote.md`). Environment scoping is a purely client-side
concept (the client keys everything by the `environmentId` the server
advertises). The store carries a persistent `storageInstanceId` UUID that all
descriptor surfaces publish (`docs/architecture/overview.md`,
"Project-data ownership and identity").

**Auth model.** `apps/server/src/auth/` (service.rs, token.rs, dpop.rs,
scope.rs, http.rs, rpc.rs, secret_store.rs):

- Policy matrix in `AuthService::build`
  (`apps/server/src/auth/service.rs:151–162`), keyed by
  `(unsafe_no_auth, ServerMode, remote_reachable)` where `remote_reachable =
  !is_loopback_host(config.host)`: `"unsafe-no-auth"`,
  `"desktop-managed-local"`, `"loopback-browser"`, `"remote-reachable"`.
  Binding to a non-loopback host automatically selects the remote policy —
  auth is not merely "localhost-only trust".
- Bootstrap methods: desktop bootstrap token (desktop mode) and one-time
  pairing tokens; session methods: browser session cookie, bearer access
  token, DPoP access token (`AuthDescriptor`, service.rs ~lines 158–188).
- Pairing links and sessions are durable: `auth_pairing_links` and
  `auth_sessions` tables (migrations.rs ~lines 1375, 1390, 1921, 1937), with
  issue/list/revoke APIs (`issue_pairing`, `list_pairings`, `revoke_pairing`,
  `list_clients`, `revoke_client`, `revoke_other_clients` — service.rs).
- HTTP: manual pairing exchanges a bootstrap at `/oauth/token` and gets a
  WebSocket ticket from `/api/auth/websocket-ticket`
  (`docs/architecture/remote.md`, "Direct bearer access";
  `exchange_bootstrap`, `issue_websocket_ticket`,
  `verify_websocket_ticket` in service.rs).
- Per-method authorization: the authoritative scope map is `required_scope`
  in `apps/server/src/auth/scope.rs` (scopes such as `orchestration:read`,
  `orchestration:operate`, `terminal:operate`, `relay:write`,
  `review:write`); a live RPC method without exactly one declared scope fails
  a server test. Method inventory: `ACTIVE_RPC_METHODS` in
  `apps/server/src/rpc/methods.rs`.
- `--unsafe-no-auth` exists as an explicit opt-out
  (`apps/server/src/config.rs`, checked in `apps/server/src/http.rs:206`).

**Accepting remote clients today.**

- **Listen address/port**: yes — `ServerConfig { host, port }` with default
  `127.0.0.1:3773` (`apps/server/src/config.rs`, `DEFAULT_PORT`,
  `with_bind`); CLI/bootstrap can override host and port
  (config.rs ~lines 310–444). Binding happens in
  `apps/server/src/lifecycle.rs:165` (`TcpListener::bind`) →
  `axum::serve` (line 345).
- **TLS**: **none in the server**. No rustls/native-tls dependency in
  `apps/server/Cargo.toml`; the server serves plain HTTP/WS. Hosted HTTPS
  comes only from the BiBCode Connect relay's managed endpoint or an external
  tunnel; `docs/architecture/remote.md` notes hosted-HTTPS clients must not
  select plain-HTTP endpoints (mixed content).
- **Auth**: already ready — non-loopback bind flips policy to
  `remote-reachable` and requires one-time-token pairing; DPoP binding is
  supported for Connect-issued tokens (`apps/server/src/auth/dpop.rs`).
- **Endpoint advertisement**: `AdvertisedEndpoint` contracts
  (`packages/contracts/src/remoteAccess.ts`) model provider kind (core,
  private network, tunnel, manual), reachability, hosted-HTTPS and desktop
  compatibility; URL normalization in
  `packages/shared/src/advertisedEndpoint.ts`. Desktop can advertise
  Tailscale endpoints (`apps/desktop/src-tauri/src/tailscale.rs`).
- **Public descriptor**: unauthenticated clients fetch
  `/.well-known/bibcode/environment` for the environment descriptor
  (`docs/architecture/overview.md`).

So a remote client connecting to a `bibcode` server is a _supported_ path
already (bearer pairing or Connect relay); the missing pieces are operator
ergonomics (TLS/reverse-proxy guidance, first-class UI) rather than protocol.

---

## 4. Contracts package (RPC definitions and versioning)

**Definitions.** `packages/contracts` is schema-only (enforced convention).
The WebSocket protocol is defined in `packages/contracts/src/rpc.ts`:

- `WS_METHODS` (rpc.ts ~line 304) is the canonical method-name table;
  orchestration methods add `ORCHESTRATION_WS_METHODS`
  (`packages/contracts/src/orchestration.ts`).
- Each method is an `Rpc.make(...)` with typed payload/success/error schemas,
  collected into `RpcGroup` (`WsRpcGroup`) using
  `effect/unstable/rpc` (rpc.ts lines 1–3, 425+).
- The Rust mirror of the wire frames is
  `apps/server/src/rpc/message.rs`; TS↔Rust drift is caught by
  `packages/contracts/src/rpcRustParity.test.ts`, which pins a manifest of
  method names and unary/stream modes (plus `authRustParity.test.ts` and
  `persistenceRustParity.test.ts` for their domains).

**Versioning / compatibility.** There is **no global protocol-version
handshake**. Compatibility is handled by:

- **Capability negotiation** on the environment descriptor:
  `ExecutionEnvironmentCapabilities`
  (`packages/contracts/src/environment.ts:23–33`) — additive booleans that
  decode-default to `false` (`repositoryIdentity`, `worktreeCatalog`,
  `worktreeCatalogRefreshReason`, `vcsStatusSummary`) plus
  `activityProtocolVersion: NullOr(Literal(2))`. Clients must downgrade when
  a server cannot prove support (`docs/architecture/overview.md`,
  "Boundaries and invariants"); e.g. the worktree-catalog client gates every
  catalog RPC on the negotiated capability
  (`docs/architecture/connection-runtime.md`).
- `serverVersion` (informational string) and nullable `storageInstanceId` on
  `ExecutionEnvironmentDescriptor` (environment.ts:36–46); omitted fields
  from older/third-party servers decode as defaults rather than failing.
- The **desktop bridge** has its own separate contract version (currently
  v3, gating protected catalog CAS —
  `docs/architecture/connection-runtime.md`); it is unrelated to the RPC
  wire.

For Remote Servers this means: connecting to an older/newer remote BiBCode
server is expected to work by capability downgrade, and any new
remote-server-specific server support should be advertised as a new
default-false capability field.

---

## 5. Settings UI structure

**Mechanism.** Settings sections are TanStack Router routes — one file per
section under `apps/web/src/routes/settings.*.tsx` (`settings.general.tsx`,
`settings.connections.tsx`, `settings.agents.tsx`, `settings.providers.tsx`,
etc.) hosted by `apps/web/src/routes/settings.tsx`. The nav is a static
array, not a plugin registry:

- `BASE_SETTINGS_NAV_ITEMS` and `SettingsSectionPath` in
  `apps/web/src/components/settings/SettingsSidebarNav.tsx` (lines 33–62),
  with `settingsNavItemsFor(policy)` conditionally inserting the
  "Local environment" item based on
  `EnvironmentPresentationPolicy`
  (`apps/web/src/connection/environmentPresentationPolicy.ts`,
  read via `readCurrentEnvironmentPresentationPolicy` in
  `apps/web/src/connection/currentEnvironmentPresentation.ts`).
- Shared layout primitives: `SettingsSection`, `SettingsRow`,
  `SettingsPageContainer` in
  `apps/web/src/components/settings/settingsLayout.tsx`.

**Where "Remote Servers" slots in.** Adding a section = add a
`SettingsSectionPath` entry + nav item in `SettingsSidebarNav.tsx`, add a
route file `apps/web/src/routes/settings.remote-servers.tsx`, and build the
panel from `settingsLayout.tsx` primitives.

**Precedent — it already exists.**
`apps/web/src/components/settings/ConnectionsSettings.tsx` (route
`/settings/connections`) _is_ a remote-servers manager today:

- an "Add backend" dialog with `savedBackendMode: "remote" | "ssh"`
  (~line 2049), taking an endpoint URL + pairing credential for remote, or a
  discovered SSH host for SSH;
- SSH host discovery through the desktop bridge
  (`apps/web/src/state/desktopSshHosts.ts` →
  `window.desktopBridge.discoverSshHosts()`), rendered as
  `DesktopSshHostRow`s (~line 1577);
- pairing-link creation/revocation and QR/pairing URLs
  (`apps/web/src/components/settings/pairingUrls.ts`), client-session
  listing/revocation, advertised endpoints, server exposure state
  (`DesktopServerExposureState`), and relay (BiBCode Connect) registration
  (`RelayConnectionTarget`, `RelayClientInstallDialog` in
  `apps/web/src/components/cloud/`).
- Onboarding for manual bearer pairing lives in
  `packages/client-runtime/src/connection/onboarding.ts` (saves the profile,
  exchanges the bootstrap, derives `wsBaseUrl`, ~line 207).

A "Remote Servers" feature would either grow this section or split it; the
persistence (connection catalog), commands (registry
register/remove/acceptStorageIdentity), and presentation policies already
exist.

---

## 6. Desktop host (apps/desktop)

**In-process server start.** `apps/desktop/src-tauri/src/backend.rs`:

- `BackendSupervisor` (~line 588) owns backend lifecycle;
  `start_default` (~line 1075) / `start` (~line 1176) resolve a
  `BackendLaunchPlan` and run `start_managed_backend` (~line 1733).
- `BackendLaunchTarget::InProcess { data_root, .. }` (~line 172) runs the
  Rust server as a library inside the Tauri process (the primary backend);
  `BackendLaunchTarget::ExternalProcess { program, .. }` (~line 176) runs an
  external process — for WSL that program is `wsl.exe` (~line 2536) launching
  the bundled Linux `bibcode` with an explicit `bibcodeHome` root.
- The host then publishes the bound address plus a desktop bootstrap token to
  the renderer (`ServerConfig.desktop_bootstrap_token`,
  `apps/server/src/config.rs`; sequence in
  `docs/architecture/overview.md`, "Runtime topology"). WSL-only mode fails
  closed as `wsl-primary-unavailable`; secondary WSL failures publish
  `wsl-secondary-unavailable` with a stable identity and no endpoint
  (surfaced client-side as `UnavailableConnectionTarget`).

**SSH launch mechanics — the closest existing analog to remote servers.**
`apps/desktop/src-tauri/src/ssh.rs` (public contract test:
`apps/desktop/src-tauri/tests/ssh_public_contract.rs`):

- `discover_ssh_hosts` (~line 2001) parses `~/.ssh/config` and known_hosts;
- `ensure_environment` (~line 434) probes or launches a remote `bibcode`,
  parses the remote launch result and pairing credential
  (`parse_remote_launch_result` ~line 1304,
  `parse_remote_pairing_credential` ~line 1287), establishes local port
  forwarding (`forward`/`forward_with_auth` ~lines 301–309), and returns a
  local HTTP/WSS bootstrap + bearer credential to the connection runtime;
- askpass handling uses a private per-tunnel helper directory with strict
  cleanup ownership (`docs/architecture/remote.md`, "Desktop-managed SSH");
- `disconnect_environment` (~line 522) tears down forwarding.

SSH is deliberately a _desktop_ capability — browser clients cannot assume
it. `apps/desktop/src-tauri/src/tailscale.rs` contributes discovered
Tailscale advertised endpoints. **Known break:** fresh SSH setup currently
invokes the removed `bibcode auth pairing create` CLI command while the
native CLI exposes only `start`, `serve`, and `storage`
(`docs/architecture/remote.md`, "Current limitations").

---

## 7. State management in apps/web

**Layers.** State is Effect Atom-based (`effect/unstable/reactivity` +
`@effect/atom-react`), split between:

- `packages/client-runtime/src/state/*` — domain atoms (shell, threads,
  projects, worktrees, vcs, terminal, activity, …) built over
  `EnvironmentRegistry`. Helpers in `state/runtime.ts`
  (`createEnvironmentRpcCommand`, `createEnvironmentRpcQueryAtomFamily`,
  `createEnvironmentRpcSubscriptionAtomFamily`, per-lane command schedulers)
  make **`environmentId` part of every key** — e.g. VCS mutations serialize
  on `(environmentId, cwd)` lanes
  (`docs/architecture/rpc-and-orchestration.md`), worktree catalog atoms key
  on `(environmentId, projectId)`
  (`docs/architecture/connection-runtime.md`).
- `apps/web/src/state/*` — app-level projections. `entities.ts` exposes
  `useProjects` / `useThreadShells` / `useServerConfigs` where server configs
  are a `Map<EnvironmentId, ServerConfig>`
  (`environmentServerConfigsAtom` in `apps/web/src/state/server.ts`).
  Environment presentation lives in `apps/web/src/state/environments.ts` +
  `presentation.ts` over the connection catalog atoms in
  `apps/web/src/connection/catalog.ts`.
- Scoped identity helpers: `scopedProjectKey`, `scopedThreadKey`,
  `scopeProjectRef`, `scopeThreadRef` in
  `packages/client-runtime/src/environment/scoped.ts` (used pervasively in
  `Sidebar.tsx`).
- The one Zustand store touching workspace rows,
  `apps/web/src/sidebarWorkspaceMetaStore.ts` (pins/unread), already keys by
  `scopedThreadKey` (environmentId + threadId) per its own header comment.

**Per-environment scoping difficulty: low.** The state layer was built for
multiple environments; removing a saved environment already removes its
registration, supervisor scope, and environment-keyed client state
(`docs/architecture/connection-runtime.md`, "Data boundary"). The residual
global bits are deliberate singletons:

- `activeEnvironmentIdAtom` (`apps/web/src/state/entities.ts` ~line 60) —
  the currently focused environment;
- `primaryEnvironmentIdAtom` (`apps/web/src/state/primaryEnvironment.ts`) —
  the host-provided primary;
- a few primary-environment conveniences leak across environments, e.g.
  `Sidebar.tsx` ~line 1536: `// TODO(orca-port): this reads the PRIMARY
  server's available editors; rows belonging to a different (remote)
  environment may see an editor list that doesn't match their actual
  backend.`

---

## Integration risk notes (hardest seams)

1. **Fresh SSH setup is broken today.** The desktop SSH launcher and
   forwarding exist, but pairing invokes the removed
   `bibcode auth pairing create` command; the native CLI exposes only
   `start`/`serve`/`storage` (`docs/architecture/remote.md`, "Current limitations").
   Any Remote Servers work touching SSH must first restore a pairing path
   (CLI subcommand or an equivalent bootstrap emitted by `serve`).
2. **No server TLS.** `apps/server` serves plain HTTP/WS
   (no rustls/native-tls in `apps/server/Cargo.toml`;
   `apps/server/src/lifecycle.rs:165`). Direct remote access over untrusted
   networks currently depends on SSH forwarding, the Connect relay's managed
   endpoint, or a user-provided tunnel/reverse proxy — and hosted-HTTPS
   clients are forbidden from selecting plain-HTTP endpoints (mixed content,
   `docs/architecture/remote.md`). A "Remote Servers" UI must be honest about
   which endpoints a browser client can actually use.
3. **Auth policy is bind-address-derived.** `remote_reachable =
   !is_loopback_host(host)` (`apps/server/src/auth/service.rs:151`). Exposing
   a server means rebinding it (CLI `--host`), which changes its auth policy
   and, in desktop update-protection paths, is explicitly denied for
   ordinary native wildcard binds (`docs/architecture/overview.md`, "Desktop
   update protection"). Remote-servers UX that "exposes" a local desktop
   server must go through the host's exposure machinery
   (`DesktopServerExposureState` in ConnectionsSettings), not just a config
   edit.
4. **Storage-identity gating.** A different non-null `storageInstanceId`
   blocks synchronization before any cache consumption and requires an
   explicit user adoption (`EnvironmentRegistry.acceptStorageIdentity`,
   `docs/architecture/connection-runtime.md`). New remote-server flows must
   surface the blocked state and adoption action, and must not create a
   second storage-identity source outside the connection catalog.
5. **Catalog persistence is single-host-coordinated only.** Protected native
   catalog storage exists on Windows only; other platforms use IndexedDB.
   Separate simultaneously-running desktop processes are not coordinated by
   an OS/file lock — a documented residual risk
   (`docs/architecture/connection-runtime.md`). More saved remote servers
   means more concurrent writers against this seam.
6. **Capability skew with third-party/older remote servers.** All optional
   behavior must ride default-false capability fields
   (`packages/contracts/src/environment.ts`); there is no protocol version
   handshake to lean on. Contract decoding already tolerates older servers
   (`storageInstanceId` → null), and any new remote-server surface must keep
   that property.
7. **Primary-environment leaks in UI.** Grouped sidebar rows read the
   primary server's editor list (`Sidebar.tsx` ~1536); similar
   primary-flavored conveniences (`primaryServerConfigAtom`,
   `usePrimaryEnvironmentId` consumers) need an audit when remote
   environments become common rather than exceptional.
8. **SSH/desktop-only capabilities in a shared UI.** SSH discovery/launch and
   several advertised-endpoint providers cross `DesktopBridge` and do not
   exist in browser mode (`docs/architecture/remote.md`). The settings
   surface must degrade per `EnvironmentPresentationPolicy` rather than
   assuming the bridge.
9. **One server = one environment.** The server has no notion of serving
   multiple environments, and `projection_projects` has no environment
   column. "Remote Servers" should therefore stay a _client-side catalog_
   feature (more `KnownEnvironment`s), not a server-side multiplexing
   feature — matching the existing design rule in
   `docs/architecture/remote.md`.
