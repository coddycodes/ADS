# Preview gateway over BiBCode Connect (relay) — design

Status: approved in conversation 2026-10-11; spec awaiting review.
Issue: [#68](https://github.com/mubeda/BibCode/issues/68).
Builds on: [preview gateway (Phase 1)](2026-10-07-internal-browser-preview-gateway-design.md),
[SSH same-port](2026-10-09-preview-ssh-same-port-design.md), and
[browser-mode iframe previews](2026-10-09-browser-mode-iframe-previews-design.md).
Third of the Phase 1 leftovers (relay reach), each with its own spec.

## Goal

From a client connected through BiBCode Connect (desktop or browser mode),
opening `http://localhost:8123/` for a dev server on the environment's loopback
loads it through the relay at its own `https://` origin. The address bar,
history, and agent tools keep showing the canonical `localhost` URL. Reload,
WebSockets (Vite HMR), and close/expiry behave as on LAN. Today relay
environments show "This address is on <label>, not this computer. Opening its
ports from here isn't supported yet."

What the user decided:

- **Origin scheme: a fixed pool of 4 preview hostnames per environment**,
  provisioned with the environment and covered by the zone's Universal SSL
  certificate (option B below).
- **Migration: automatic re-link.** A linked environment without preview
  hostnames is re-linked once per app session by its primary client.
- **Tunnel restore after restart** is part of this change: the server restarts
  its Connect tunnel from the stored runtime config at startup.
- Relay connections carry no E2EE (`e2ee: null` in
  `packages/client-runtime/src/connection/resolver.ts`), so the issue's "allow on
  E2EE profiles?" question does not arise: preview traffic is as visible to
  Cloudflare as relay RPC already is.

## Current state

- Each relay environment is one Cloudflare tunnel in the tunnel zone
  (`RELAY_TUNNEL_ZONE_NAME`) with one flat hostname,
  `<stage>-<16 hex>.<zone>` (`managedEndpointHostname`), and ingress
  `[{ hostname, service: <server origin> }, { service: "http_status:404" }]`
  (`infra/relay/src/environments/ManagedEndpointProvider.ts`). The origin is
  always the server's loopback HTTP port.
- The relay provisions during the primary client's link
  (`EnvironmentLinker`), returns `endpointRuntime`
  (`{ providerKind, connectorToken, tunnelId, tunnelName }`), and the client
  posts it to the server's `connect.relayConfig` route, which starts the
  connector and stores the config (`cloud-endpoint-runtime-config`). Nothing
  reads that config back: after a restart the tunnel stays down until the next
  link.
- Relay traffic reaches the server as `cloudflared` → `127.0.0.1:<main port>`
  with `Host: <environment hostname>`. `gateway_bind_host` refuses a loopback
  socket under a non-loopback `Host` as `not-reachable` ("proxied address"), and
  the web resolver refuses relay environments before calling the server.

## Architecture decision record

| Option | Decision |
| --- | --- |
| B. Fixed pool `<env>-p1…p4.<zone>`, provisioned with the environment | **Chosen.** Free, works the moment the environment is provisioned, fits the existing provider flow and ownership fences. Costs: at most 4 relay preview origins at once per environment; 5 DNS records per environment instead of 1 |
| A. Nested wildcard `*.<env>.<zone>` | Needs Advanced Certificate Manager and one wildcard certificate per environment (issuance delay, CA and certificate-pack limits) |
| C. Per-target hostname through the Cloudflare API on each open | Puts the relay in every open (latency, rate limits) and leaves records to garbage-collect |
| D. Alternate HTTPS ports on the environment hostname | 5 slots, often blocked by corporate firewalls, unverified through Tunnel |
| Path prefix on the environment hostname | Rejected in Phase 1: breaks root-relative apps and shares an origin |
| Separate router listener on the server | The relay knows one origin per environment; a second port is a new provisioning field and tunnel rule. Host routing on the main port needs neither |

## 1. Relay provisioning (`infra/relay`)

- `managedEndpointPreviewHostnames(hostname)` derives 4 names from the
  environment's allocated hostname: its first label plus `-p1` … `-p4`, under
  the same zone. A label that would exceed 63 characters shortens its stage
  prefix and keeps the 16-hex hash. These names never collide with environment
  hostnames, which end in the hash.
- `provision` adds, after the environment's own record:
  - ingress rules `{ hostname: <preview name>, service: <server origin> }`
    after the environment rule and before the catch-all 404;
  - one proxied CNAME per preview name to `<tunnel id>.cfargotunnel.com`,
    through `ensureDnsRecord` with no stored id, so an existing record is
    found by name and updated and a missing one is created.
- `deprovision` lists each preview name's records and deletes them (ignoring
  not-found) before deleting the environment record and the tunnel.
- Every call goes through the existing `withOperationFence`. Records are
  found by name, so the allocation table and its migrations do not change.
  New failure stages: `ensure-preview-dns-record`, `delete-preview-dns-record`.
- `RelayManagedEndpointRuntimeConfig` (`packages/contracts/src/relay.ts`)
  gains optional `hostname` and `previewHostnames`; `provision` fills both.

## 2. Server (`apps/server`)

### Endpoint state and restore

- `ManagedEndpointConfig` gains `hostname: Option<String>` and
  `preview_hostnames: Vec<String>` (default empty). Names must be lowercase DNS
  names; a malformed one rejects the config. The server uses the first 4 and
  ignores more, so a newer relay cannot break an older server.
- `ManagedEndpointRuntime` publishes the current
  `RelayPreviewHosts { hostname, previews }` from the last applied config. A
  config whose connector key (token, tunnel id, tunnel name) is unchanged keeps
  the running connector but still replaces the published hostnames. Disabling
  the endpoint clears them.
- **Restore at startup.** After `ConnectMcpService::open`, the server reads the
  stored link and, when linked with a non-null `cloud-endpoint-runtime-config`,
  applies it in a background task (same supervised, attributed spawn as a
  link). A failure is logged with `tracing::warn!` and leaves the server
  running without the tunnel, as today. Shutdown is unchanged: quiesce calls
  `managed_endpoint.shutdown()`, and `apply` refuses once shutdown has started.
- `connect.linkState` adds `managedEndpoint: boolean` (a stored non-null
  runtime config) and `previewHostnames: boolean` (that config names preview
  hostnames).

### Admission

- A `ClientReach` whose local IP is loopback and whose `Host` (port removed,
  case-insensitive) equals the published environment `hostname` is **relay
  reach**. It no longer fails as `Proxied`.
- Relay reach with no published preview hostnames fails with the new reason
  `relay-not-provisioned`: "Previews over BiBCode Connect aren't set up for
  <environment> yet."
- A relay target still gets its per-target loopback listener (key unchanged),
  so capabilities, cookies, sessions, limits, and teardown stay keyed by
  gateway port.

### Slots

- The gateway keeps 4 slots, one per preview hostname, each holding at most
  one target. `gatewayOpen` over relay reuses the target's slot, else takes a
  free one, else takes the slot whose target has no live connections and the
  oldest activity. When every slot's target has live connections, the call
  fails with the new reason `relay-busy` (logged with `tracing::warn!`).
- A target releases its slot when it closes (any teardown case). Taking over a
  slot never leaks a session: gateway cookies are named per gateway port, so
  the old target's cookie authenticates nothing on the new one. Browser storage
  on that hostname is shared with whatever used the slot before, as
  `localhost:<port>` is shared between apps locally.
- `PreviewGatewayOpenResult` gains optional `origin`
  (`https://<preview hostname>`), set only for relay reach.
  `PreviewGatewayError.reason` gains `relay-not-provisioned` and `relay-busy`.

### Host router on the main port

- One layer in front of every main-server route: when the socket's local
  address is loopback and `Host` (port removed, case-insensitive) equals one of
  the published preview hostnames, the request is handled by the gateway and
  never reaches BiBCode's API. A slot with a target dispatches in-process to
  that target's existing gateway handler (no extra network hop); an empty slot
  answers the gateway's `401` page. Any other `Host` takes the normal routes,
  so another environment's preview names (which Cloudflare routes to that
  environment's own tunnel anyway) are never served here.
- Requests arriving this way are **HTTPS-facing** (TLS ends at Cloudflare):
  - the client-facing origin is `https://<Host>`, so the Origin rule for
    writes and WebSocket upgrades compares against it;
  - the bootstrap cookie adds `Secure`
    (`bibcode-gw-<port>=<id>; HttpOnly; Secure; SameSite=Lax; Path=/`);
  - upstream `Location` headers are rewritten to the https origin;
  - upstream `Origin` is still rewritten to `http://localhost:<port>`.
- Requests on a target's own listener are unchanged (`http`).

## 3. Client (`apps/web`)

- `classifyEnvironmentReach` returns `{ kind: "relay" }` for
  `RelayConnectionTarget`; `resolvePreviewTarget` returns
  `{ kind: "gateway", via: "relay", url }` (canonical `localhost`).
- `resolveForNavigation` calls `gatewayOpen` and builds the bootstrap URL on
  the returned `origin`; `canonicalOrigins` maps it back to the canonical origin
  so shared state, history, and agent tools stay canonical. A result without
  `origin` (a server without this change) fails client-locally with the
  current relay copy.
- Failures: `relay-not-provisioned` (and `not-reachable`, which an older relay
  config without `hostname` produces) → "Previews over BiBCode Connect aren't
  set up for <label> yet. Open BiBCode on <label> to finish setting them up.";
  `relay-busy` → "Too many previews of <label> are open over BiBCode Connect.
  Close one, then try again; one closed in a web browser takes up to 10 minutes
  to free up." (retryable). All stay client-local (they depend
  on how this client reached the environment), like other reach failures.
- **Browser mode.** A BiBCode page on another site cannot frame a relay preview
  (the `Lax` gateway cookie is not sent cross-site). A clicked link already
  takes the new-tab path (`openGatewayInSystemBrowser`), because browser mode
  frames only gateway previews on its own host. A navigation inside the panel
  (address bar, Reload, agent `preview_open`) fails client-locally with "<label>
  is reached through BiBCode Connect, so its previews can't show inside this
  page. Ctrl-click (⌘-click on Mac) the address in the chat or terminal to open
  it in a new tab." It applies to any server-returned preview origin, including
  a BiBCode page served over plain HTTP from the managed endpoint.
- **Auto re-link.** When the primary environment's link state is
  `linked && managedEndpoint && !previewHostnames` and the user is signed in to
  Connect, the client re-runs the existing link flow
  (`linkPrimaryEnvironmentToCloud`) once per app session, in the background,
  without the relay-client install prompt (a missing relay client fails the
  re-link instead). The relay provisions the missing names (and is idempotent
  for everything else), and the server publishes them. A failure is logged and
  retried in the next session; the Share tab keeps its manual Link control.

## 4. Failure and edge cases

- Relay without this change deployed: re-link returns no `previewHostnames`;
  the client tries again next session, and relay previews keep the
  "aren't set up" notice.
- Environment linked without managed tunnels (`managedEndpoint: false`): no
  re-link. Its relay endpoint is not a published hostname, so `gatewayOpen`
  keeps failing with the existing `not-reachable` message.
- Tunnel down or Cloudflare error: the webview shows the edge error page;
  Reload re-resolves.
- Server restart: targets, slots, and gateway sessions are gone; the tunnel and
  published hostnames come back from the stored config; Reload re-resolves.
- Upstream not listening, expired capability, principal revoked, cross-origin
  write: unchanged gateway behavior (`502`, `401`, `401`, `403`).
- Two relay clients open the same target: same listener and slot, separate
  gateway sessions.

## 5. Security

- A preview hostname alone grants nothing: every request needs the gateway
  cookie, which only a single-use, 60 s, principal-bound capability creates.
- `Secure; HttpOnly; SameSite=Lax` cookie scoped to the preview host; principal
  binding, 30 s re-check, and revocation unchanged.
- BiBCode credentials (session cookie, `authorization`, `dpop`) are never
  forwarded upstream (unchanged), and preview hosts never reach BiBCode's API.
- The Origin rule accepts only `https://<preview host>` for relay-routed writes
  and upgrades, and still refuses cross-site ones.
- Relay preview traffic is visible to Cloudflare, like all relay traffic.
- Cookie tossing: a previewed app's script can set cookies for the whole tunnel
  zone, reaching other preview hosts in that zone. The gateway already drops
  `Domain=` from upstream `Set-Cookie` but cannot stop `document.cookie`.
  Documented; adding the tunnel zone to the Public Suffix List is a follow-up.
- The router trusts `https` only for requests that arrive on a loopback socket
  with a published preview `Host`; a local process that forges that `Host` still
  needs the gateway cookie.

## 6. Testing

- `infra/relay` (`ManagedEndpointProvider.test.ts`, `deploymentConfig` tests):
  preview name derivation (length limit, hash kept); provision creates 4
  records and ingress rules in order before the 404; re-provision of an
  existing allocation updates or creates missing preview records without
  duplicates; deprovision deletes preview records before the tunnel; fences
  apply; runtime config carries `hostname` and `previewHostnames`.
- `apps/server`:
  - config parsing (valid, malformed, more than 4 names), published hostnames
    updated without restarting an unchanged connector, cleared on disable;
  - startup restore applies the stored config and logs a failure without
    stopping startup;
  - admission: relay reach accepted, `relay-not-provisioned` without names;
  - slots: assignment, reuse, takeover of an idle slot, refusal when all are
    live, release on teardown;
  - router: preview `Host` reaches the target's handler, empty slot → `401`,
    other `Host` → normal routes, non-loopback socket → normal routes;
  - HTTPS-facing behavior: Origin rule on `https://`, `Secure` cookie,
    `Location` rewrite, WebSocket upgrade through the router;
  - `connect.linkState` flags.
- `apps/web`: resolver relay path and origin mapping, missing `origin`,
  failure copy, browser-mode tab fallback, auto re-link trigger conditions and
  once-per-session behavior.
- Contracts: RPC fixtures regenerated for the new optional fields and error
  reason.
- Native: a relay step in `docs/testing/cross-platform-validation.md`
  (desktop and browser-mode client; Vite HMR; reload; close and expiry). It
  needs a deployed relay stage, so this change records it as **not run here**.

## 7. Documentation

- `docs/architecture/remote.md`: gateway over relay (slots, router, HTTPS
  facing, E2EE and cookie-tossing notes), tunnel restore at startup; remove the
  relay limitation.
- `docs/user/workspace-ui.md`: relay previews and browser-mode tab behavior.
- `infra/relay/README.md`: preview hostnames per environment and their DNS
  cost.
- `docs/testing/cross-platform-validation.md`: the relay step.
- A pointer from the Phase 1 spec's "Out of scope".

## Profiler, crash, and trace coverage

- Child processes: the restored connector uses the existing supervised spawn
  and `register_identity` in `ManagedEndpointRuntime::apply`.
- RPC methods: no new method; `preview.gatewayOpen` keeps its wrapper.
- Shutdown: unchanged (`managed_endpoint.shutdown()` in quiesce).
- Logs: restore failure and slot exhaustion use `tracing::warn!`; no prompt,
  message, or page content is logged.
- No new profiler events or findings.

## Out of scope

Framing relay previews in browser mode, HTTPS upstreams, more than 4 slots,
wildcard certificates, and the SOCKS profiles of #69.

## Review addenda (2026-10-12)

Codex reviews of the implementation changed four decisions above:

- **No slot takeover.** A live target keeps its slot; a slot frees only when its
  target closes (tab closed, idle 10 minutes, thread deleted, shutdown). A page
  left open on a reassigned hostname would carry the newer target's cookie and
  reach the wrong app. A refused open removes the target it created.
- **Edge cache.** Every response on a preview hostname carries
  `Cloudflare-CDN-Cache-Control: no-store`, so Cloudflare never serves an
  authenticated response to another request.
- **Restore and re-link fences.** Link, unlink, and the startup restore share
  one server lock. The link records the port the tunnel was provisioned for;
  a restore skips a tunnel provisioned for another port and `linkState`
  reports `managedEndpointStale`, which the background re-link also handles.
  The background re-link runs on the link scheduler, re-reads the link state,
  only for the signed-in account the link belongs to, and names that account
  in `relayConfig` (`requireLinkGeneration`, a digest of the rotated
  environment credential reported as `linkGeneration`); the server refuses it
  with a conflict if the link changed, even to the same account. The relay-side link a refused attempt made
  stays (removing it could delete a newer link; the relay has no conditional
  unlink). Each client tab keeps its own canonical mapping for preview
  hostnames, and the client bootstraps on a returned `origin` for any non-SSH
  connection (a browser opened at the managed endpoint is a primary one).
- **Shutdown.** The gateway's drain starts when shutdown is signaled, because
  the main port's graceful shutdown waits for preview exchanges.
- **Service workers.** No service worker script loads on a preview hostname
  (`403` for requests with `Service-Worker`), since a worker registered by one
  target would outlive it and intercept the next target's bootstrap.
- **Mount point.** The background re-link observer mounts in the root route's
  authenticated branch, so it never caches a pre-pairing `401` link state.
- **Browser cache and HTTPS.** Every response on a preview hostname is sent
  `Cache-Control: no-store` (a revalidated entry could still be another
  target's content), and
  relay hostnames reached over plain HTTP redirect (`308`) to HTTPS, so no
  BiBCode page there frames previews it can't load.
