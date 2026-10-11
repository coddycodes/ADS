# Internal browser preview gateway (Phase 1) — design

Status: approved in conversation 2026-10-07; spec awaiting review.
Depends on: [link routing (Phase 0)](2026-10-07-internal-browser-link-routing-design.md).
Research input (historical, verify before reuse):
`docs/plans/2026-10-06-internal-browser-and-mobile-emulator-research.md`.

## Goal

A dev server or tool running on the BiBCode server's loopback (for example
`http://localhost:5173`, or the brainstorming companion's keyed
`http://localhost:PORT/?key=…`) opens correctly in the internal browser from any
client: local, LAN, WSL, and desktop-managed SSH, plus a new browser tab in
browser mode. Tools that launch a browser (`$BROWSER`, `BRAINSTORM_OPEN_CMD`)
land in BiBCode instead of the server's desktop.

What the user decided:

- Gateway only; no SSH same-port forwarding, no SOCKS profiles. The previewed
  app gets a new origin; apps that hard-code `localhost` origins (OAuth
  redirects, CORS allowlists) are an accepted limitation.
- One gateway listener port per target (origin isolation by port).
- BiBCode Connect (relay) is deferred: relay environments show an honest
  notice. Each relay environment is a Cloudflare tunnel with one hostname and a
  catch-all 404 (`infra/relay/src/environments/ManagedEndpointProvider.ts`),
  so relay reach needs separate infra work.
- Browser mode opens gateway targets in a new top-level tab (no iframe).
- Both `BROWSER` and `BRAINSTORM_OPEN_CMD` point at a BiBCode open-URL shim.
- Linux child-webview geometry is fixed here if a runtime check shows it is
  broken.

## Architecture decision record

| Option | Why not chosen |
| --- | --- |
| Path prefix on the UI origin (`/preview/5173/…`) | Breaks root-relative apps (the companion redirects to `/`, cookie `Path=/`); puts previewed apps on BiBCode's origin |
| Subdomain per target (`<cap>.localhost`) | Unusable for LAN/WSL by IP; `*.localhost` resolution unverified on WKWebView/WebKitGTK |
| SOCKS-routed preview profile | Proxy is per profile not per view; macOS needs `macos-proxy`; WebView2 loopback bypass; nothing for browser mode |
| SSH same-port `-L` | SSH only, port collisions; may be added later if origin changes hurt |

## 1. Prerequisite hardening: Origin enforcement

Today normal mode allows every origin (`apps/server/src/http.rs` CORS layer) and
cookie-authenticated requests have no Origin check. Cookies ignore port, so in
browser mode any app on the same host (including a gateway target) is same-site
and receives the `SameSite=Lax` session cookie on top-level navigations and
same-site subrequests.

Change: when a request authenticates **by the session cookie**, require that:

- non-`GET`/`HEAD`/`OPTIONS` HTTP requests carry `Origin` equal to the server's
  own UI origin (scheme+host+port of the request's `Host`) or a desktop scheme
  (`bibcode://app`, `bibcode-dev://app`, dev URL in dev mode);
- WebSocket upgrades authenticated by cookie carry such an `Origin`.

Header/bearer/DPoP/`wsTicket` authentication is unchanged. Ships as its own
commit before any gateway code; it fixes an existing gap.

## 2. Gateway

Owner: `apps/server/src/preview/gateway/` (new module), wired from
`production/workspace_preview.rs`.

### Admission

RPC `preview.gatewayOpen({ threadId, url })` (the canonical loopback URL, so the server can reject HTTPS itself) →
`{ gatewayPort, capability, expiresAtMs }`.

- Requires `orchestration:operate` (the same grant `preview.open` already needs
  in `apps/server/src/auth/scope.rs`). A gateway gives full read/write access to
  the upstream service, so read scope is not enough.
- Target host is always server loopback; the client sends only a port.
- Admitted: any **loopback** port the caller names. Every `gatewayOpen` call is an
  explicit user or agent action (click, URL-bar submit, agent `preview_open`,
  claimed `open-url` request) by an `orchestration:operate` caller, so no
  separate attribution or explicit-port list is kept. Non-loopback hosts and
  non-HTTP schemes → `PreviewError::TargetNotAdmitted`.
- Upstream address family: try `127.0.0.1` then `::1` (first that accepts).
- HTTP upstreams only. An `https://localhost:<port>` target is rejected with
  "HTTPS dev servers can't be previewed through the gateway yet; serve over HTTP
  or open it on <environment> directly." TLS upstreams are out of scope.
- Idempotent per `(threadId, port)` under one lock: a second call reuses the
  listener and mints a fresh capability.

### Listener lifecycle

- One `TcpListener` per admitted target on an ephemeral port, bound to the same
  interface as the main server (loopback for local/SSH; the configured bind for
  LAN/WSL).
- Closed when: all preview tabs for that target close (from `PreviewManager`
  events), 10 minutes with no connections, the thread is deleted, or server
  shutdown. Shutdown drains in-flight connections with the server's existing
  shutdown deadline.
- Limits: 64 concurrent upstream connections per target, 256 global; fixed
  64 KiB copy buffers; no body buffering (stream both ways with backpressure).
  Over-limit → `503` with `retry-after`.

### Auth

1. Capability: HMAC token (reuse the asset token signer with a new purpose
   `"preview-gateway"`), bound to `(gatewayPort, upstreamPort, threadId,
   principal session id)`, 60 s TTL, single use.
2. Bootstrap: the client navigates to `/__bibcode/bootstrap?cap=<token>&to=<path+query+fragment>`.
   The gateway validates, sets `bibcode-gw-<gatewayPort>=<gateway session id>;
   HttpOnly; SameSite=Strict; Path=/`, and returns a `200` HTML page that calls
   `location.replace(to)`. The follow-up navigation is initiated by the gateway
   site itself, so the `Strict` cookie is sent even when BiBCode's UI is on a
   different site (a cross-site `302` chain would withhold it). The port in the
   cookie name keeps targets on the same host from colliding. `to` must be a
   path (no scheme/host).
3. Every later request and WebSocket upgrade requires that cookie. Missing or
   invalid → `401` page: "This preview link expired. Go back to BiBCode and open
   it again."
4. Gateway sessions are bound to the granting principal: each request and each
   live connection checks the principal through the auth service
   (`authenticate_token` semantics; cached for at most 30 s). Expiry or
   revocation ends the gateway session and closes its live connections within
   that 30 s window (a periodic check, not the revocation connection registry,
   which would list every preview connection as a connected client). Maximum gateway session lifetime
   is the principal's expiry; a listener closing also ends its sessions.

### Request Origin check

Cookies ignore port, so a malicious app on another port of the same host is
same-site with the gateway and the browser sends the gateway cookie on its
requests despite `SameSite=Strict`. Therefore, **before any rewrite**:

- WebSocket upgrades and non-`GET`/`HEAD`/`OPTIONS` requests must carry an
  `Origin` equal to this gateway's client-facing origin (scheme + request `Host`).
  Missing or different → `403`.
- Only after that check passes is `Origin` rewritten for upstream.

### Forwarding

Request to the upstream loopback address:

- Strip: BiBCode's session cookie, every `bibcode-gw-*` cookie, `authorization`,
  `dpop`, hop-by-hop headers.
- Rewrite `Host` → `localhost:<port>`.
- WebSocket upgrade (after the Origin check): rewrite `Origin` →
  `http://localhost:<port>` (satisfies the companion's
  `Origin === "http://" + Host` check), proxy frames bidirectionally.

Response from upstream:

- `Set-Cookie`: drop `Domain=` attribute; drop any cookie whose name equals
  BiBCode's session cookie or starts with `bibcode-gw-`.
- `Location` pointing at `localhost:<port>`/`127.0.0.1:<port>`/`[::1]:<port>` →
  rewrite to the gateway origin as seen by the client (from the request `Host`).
- Everything else passes through, including `X-Frame-Options`/CSP (gateway
  targets are always top-level).

Accepted limitation: previewed apps on the same host share a cookie jar with
each other (cookies ignore port). The desktop preview profile is already
isolated from the main webview.

### E2EE note

Gateway traffic is plain HTTP on the server's bind, outside Noise, like
`/api/assets` today. Document in `docs/architecture/remote.md`.

## 3. Client reach

### Shared state stays canonical

Preview tab state is shared across clients through `PreviewManager` and preview
events, and `DesktopPreviewTabHosts.tsx` navigates to the shared URL. Gateway
URLs are client-specific (a single-use capability; over SSH, a client-local
port), so they must never enter shared state:

- `preview.open`/`navigate` and tab state always carry the **canonical** URL as
  the server sees it (`http://localhost:5173/path?key=…`).
- Each client resolves the canonical URL locally right before handing it to its
  webview (`NativePreviewTabHost` initial navigation, URL bar submit, reload),
  and reports nav status back in canonical form (gateway origin mapped back to
  `localhost:<port>`).

### Resolution

Phase 0's `resolvePreviewTarget` gains a gateway step for loopback URLs on
non-local environments:

1. Call `preview.gatewayOpen` → `{ gatewayPort, capability }`.
2. Map to a client-reachable origin:

| Topology | Gateway origin |
| --- | --- |
| Local primary | not used (loopback is already correct) |
| LAN / tailnet / WSL | `http://<environment host>:<gatewayPort>` |
| SSH (desktop-managed) | `http://127.0.0.1:<localPort>` from `DesktopBridge.sshForward` |
| Relay | `unreachable` (Phase 0 copy) |
| Public-IP bearer host | `unreachable` (Phase 0 copy): gateway traffic is plain HTTP and is not exposed to public binds in this phase |

3. Navigate to the bootstrap URL carrying the original path, query (including
   the companion's `?key=`), and fragment.

### Reload and re-bootstrap

- Desktop: the preview Reload button re-runs resolution for gateway-backed tabs
  (fresh capability, forward re-established if needed) instead of calling the
  native refresh directly. Native refresh is used for local tabs only.
- A webview-initiated reload that hits the gateway `401`/`502` page shows that
  page; the Reload button recovers it.
- Browser mode: the upstream tab contains no BiBCode code; its `401` page tells
  the user to reopen from BiBCode. Accepted limitation.

### `DesktopBridge.sshForward(environmentId, remotePort) → localPort`

- Owned by the desktop SSH tunnel owner (`apps/desktop/src-tauri/src/ssh.rs`).
- Adds `-L <free local>:127.0.0.1:<remotePort>` to the live connection via
  ControlMaster `-O forward` when the connection has a control socket, otherwise
  a dedicated `ssh -N -L` child admitted and reaped by the same owner.
- Local port is any free loopback port, so collisions do not occur.
- Released when the last client tab using it closes or its gateway port changes
  (client reference count), or with the tunnel; idempotent per
  `(environmentId, remotePort)`; at most 8 forwards per tunnel (least recently
  requested evicted) so leaks can't exhaust the shared SSH child capacity.
- Follows the existing SSH process admission, cancellation, and reaping rules.

### Browser mode

- User clicks: open `about:blank` **synchronously** in the click handler, then
  navigate it after resolution (gateway RPC and any delay can outlive transient
  activation). Resolution failure closes the blank tab and shows the notice. If
  the browser blocks the tab (`window.open` returns `null`), show the
  "wants to open" prompt below instead.
- Server-initiated opens cannot open a window without a gesture. They show an
  in-app prompt: "<Agent | A command> wants to open <url>" with **Open** (opens
  the tab as above) and **Dismiss**. UI.md review required.
- Agent `preview_open` in browser mode: register a browser-mode automation host
  in `PreviewAutomationHosts.tsx` (today it registers nothing without a native
  bridge, so the broker rejects). It implements only `status` (no automation)
  and `open`: `open` enqueues the prompt and returns immediately with
  `{ status: "pending-user" }`; it does not wait for the click. All other
  automation methods reject as unsupported.

## 4. Open-URL hook

### Credential

- The server mints a per-session token scoped to one thread and one
  capability: requesting an open for that thread. Lifetime: the provider or
  terminal session.
- Injected as `BIBCODE_OPEN_URL_AUTH` and `BIBCODE_OPEN_URL_ENDPOINT` into
  provider launches (`production/provider_runtime.rs`, alongside the MCP
  credential) and terminal spawns (`terminal/pty.rs` env assembly).

### Shim

- The server writes `bibcode-open-url` (POSIX sh) and `bibcode-open-url.cmd`
  (Windows) into its runtime directory at startup, mode 0755, and prepends that
  directory to `PATH` for provider and terminal sessions.
- The shim runs `"<current_exe>" open-url "$1"` and passes its argument as argv
  data; it performs no evaluation of its own. `current_exe` is the `bibcode`
  binary for standalone servers and the desktop binary for desktop-local mode;
  the desktop `main` handles an `open-url` argv before starting Tauri (no window,
  exits when done).
- `BROWSER=bibcode-open-url` and `BRAINSTORM_OPEN_CMD=bibcode-open-url` are set
  in the same sessions.
- Known limitation outside our control: a caller that builds a shell command
  line (the brainstorming companion runs `BRAINSTORM_OPEN_CMD` through a shell
  with a JSON-quoted URL) can expand `$…`, backticks, or `$(…)` before the shim
  starts. The companion's own URLs (`?key=<hex>`) contain none of these. The
  shim's tests cover metacharacters passed as argv to prove the shim itself adds
  no evaluation.

### `open-url` subcommand

- Validates the argument is an `http(s)` URL (else exit 2 with a message).
- POSTs `{ url }` to `BIBCODE_OPEN_URL_ENDPOINT` (a new
  `POST /api/preview/open-url` route on the main server) with
  `authorization: Bearer <token>`. Header authentication, so section 1's Origin
  rule does not apply.
- The server emits `PreviewEvent::OpenRequested { requestId, threadId, url }`.
  It does **not** create a tab itself.
- Exit 0 and print the URL when no client is subscribed, so the calling tool
  does not fail. Network/auth failure → print the URL, exit 0, log to stderr.

### Client handling (open requests vs opened tabs)

Handling `opened` events by calling the router would loop (`openLink` →
`preview.open` → another `opened`). The two are kept apart:

- `OpenRequested` is a request. A client acts on it only after winning
  `preview.claimOpenRequest({ requestId })` (first claimer wins; a request
  expires unclaimed after 60 s). Only clients currently showing that thread try
  to claim. The winner routes it through `openLink` with the user's setting (no
  modifier): `"system"` → system browser after resolution; `"app"` →
  `preview.open` with the canonical URL; browser mode → prompt.
- `opened` stays a presentation event: clients add the tab to state, reveal the
  browser panel if the thread is visible, and resolve the canonical URL locally.
  It never calls `openLink` or `preview.open`.
- Agent `preview_open` keeps its existing path (it already creates the tab
  through the automation host); it does not use `OpenRequested`.

## 5. Linux child-webview geometry (conditional)

Task 1 is a runtime check on Wayland and X11 with a dev build: does the preview
child follow `desktop_preview_set_bounds` on panel move/resize?

- Works → add the check to `docs/testing/` and stop.
- Broken → mount preview children in a `gtk::Fixed` overlay in
  `apps/desktop/src-tauri/src/preview/platform/linux.rs` and route bounds
  through it; validate on both display servers.

Result (2026-10-09): broken on both display servers — Tauri packs Linux child
webviews into the window's content box and wry moves only children of a
`GtkFixed`. Fixed by wrapping the main webview's slot in a `GtkOverlay` with a
pass-through `GtkFixed` layer; an ignored display test verifies placement on
Wayland and X11.

## Failure and edge cases

- Upstream not listening → gateway `502` page: "Nothing is listening on port
  <port> on <environment>."
- Capability replayed or expired → `401` page (see Auth).
- Principal revoked or expired → gateway session ends, live connections
  cancelled, next request `401`.
- Cross-port request without matching `Origin` → `403`.
- SSH tunnel drops → forwards die; the tab shows a connection error; the
  Reload button re-resolves (no automatic re-forward of stale tabs).
- Server restart → all listeners and gateway sessions gone; Reload re-resolves.
- Two clients open the same target → same listener, separate gateway sessions,
  each with its own capability and (over SSH) its own local port.
- Two clients showing the thread receive one `OpenRequested` → one claim wins;
  the other does nothing.
- Duplicate `gatewayOpen` calls race → idempotent per `(threadId, port)` under
  one lock.

## Testing

- Rust unit/integration (`apps/server`):
  - Section 1 Origin enforcement (cookie vs bearer, HTTP vs WS).
  - Admission: operate scope required; non-loopback or non-HTTP target →
    rejected; HTTPS target rejected; `::1`-only upstream reached.
  - Capability single-use, expiry, binding to principal; bootstrap `to` must be
    a path; bootstrap sets the cookie and returns the `location.replace` page.
  - Principal revocation cancels a live gateway WebSocket.
  - Gateway Origin check: cross-port WS upgrade and POST → `403` before rewrite.
  - Header and cookie stripping both directions; `Location` rewrite; WS proxy
    with Origin rewrite against a fixture that enforces the companion's check.
  - Limits → 503; idle teardown; shutdown drain.
  - `OpenRequested` emission and single-winner `claimOpenRequest`, expiry.
- Rust (`apps/desktop`): `sshForward` admission, idempotency, release; `open-url`
  argv handling without Tauri startup; shim passes metacharacter URLs as argv
  unchanged.
- Vitest: resolver gateway mapping per topology; shared tab state stays
  canonical (gateway URL never sent in `preview.open`/`reportStatus`); Reload
  re-resolves gateway tabs; browser-mode synchronous blank tab and blocked-tab
  fallback; browser-mode automation host `open` → prompt + `pending-user`;
  `opened` reveals the panel and never calls `preview.open`.
- Native browser check (manual, recorded in the runbook): cross-site bootstrap
  in browser mode (BiBCode UI on one site, gateway on another) keeps the
  `Strict` cookie after `location.replace`.
- End-to-end: over desktop-managed SSH, run the brainstorming
  `start-server.sh --open` in a thread terminal → companion loads in the
  internal browser, live reload works.
- `docs/testing/`: SSH and browser-mode preview procedures; Linux geometry check.
- Reviews: `UI.md` (notices, prompt, 401/403/502 pages) and
  `vercel-react-best-practices`; security review of the gateway diff.

## Docs

- `docs/architecture/remote.md`: gateway surface, per-target listeners, Origin
  enforcement, SSH forwards, E2EE caveat, relay limitation.
- `docs/architecture/overview.md`: gateway in runtime topology.
- `docs/user/workspace-ui.md`: remote previews, browser mode, `$BROWSER`
  behavior.

## Out of scope

Relay reach (needs relay ingress/DNS/cert work), SSH same-port forwarding
(see [2026-10-09-preview-ssh-same-port-design.md](2026-10-09-preview-ssh-same-port-design.md)),
SOCKS profiles, iframe previews in browser mode, picker/annotation, automation
parity, device streaming, emulators.
