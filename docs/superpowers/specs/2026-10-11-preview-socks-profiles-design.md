# SOCKS-routed preview profiles (true-origin previews, Phase 2) — design proposal

Status: **approved for implementation, Linux first** (2026-10-14). The user
chose to build it now, with the tunnel carried by the web client's own
connection (see [Decisions](#decisions-2026-10-14)). Windows and macOS keep the
gateway until their engine spikes pass.
Issue: [#69](https://github.com/mubeda/BibCode/issues/69).
Builds on: [preview gateway (Phase 1)](2026-10-07-internal-browser-preview-gateway-design.md),
[SSH same-port](2026-10-09-preview-ssh-same-port-design.md) (§6 per-environment
preview storage), and [gateway over BiBCode Connect](2026-10-11-preview-gateway-relay-design.md).
Research input (historical): `docs/plans/2026-10-06-internal-browser-and-mobile-emulator-research.md` §2.4 (a), §2.6.

## Goal

On the desktop, a preview of `http://localhost:5173` from an SSH, LAN, WSL, or
relay environment loads at exactly `http://localhost:5173`, through that
environment, so cookies, OAuth redirects (including `response_mode=form_post`),
and CORS allowlists behave as they do on the server. This upgrades the gateway;
it does not replace it. Browser mode keeps the gateway.

## What the gateway already covers

| Topology | Origin today | Remaining gap SOCKS closes |
| --- | --- | --- |
| Desktop SSH, local port free | real (`localhost:P`, same-port forward) | `form_post` callbacks (the gateway cookie is `Lax`) |
| Desktop SSH, local port busy | `127.0.0.1:<random>` | real origin |
| LAN / tailnet / WSL | `http://<host>:<gateway port>` | real origin, OAuth, CORS |
| BiBCode Connect | `https://<env>-pN.<zone>` (after #68) | real origin, OAuth, CORS |
| Browser mode | gateway origin | none (no webview to proxy) |

Anything the gateway rewrites (`Host`, `Origin` on writes, `Location`,
`Set-Cookie` `Domain=`) also behaves natively through SOCKS.

## Linux spike result (2026-10-11)

Throwaway probe (not kept in the repository; recipe in the appendix):
wry 0.55.1 + tao 0.35.3 (the versions in `Cargo.lock`), WebKitGTK 2.52.6,
Wayland session (Hyprland). One `WebContext` per profile directory with
`WebViewBuilder::with_proxy_config(ProxyConfig::Socks5(127.0.0.1:<port>))`,
which wry applies as `NetworkProxySettings(socks5://…, ignore=[])` on the
context's website data manager. A logging, CONNECT-only SOCKS5 relay recorded
every target.

| Probe from the page | SOCKS request seen | Meaning |
| --- | --- | --- |
| Top-level `http://localhost:18572/` | `CONNECT localhost:18572` (domain) | loopback **is** proxied with the empty ignore list |
| `fetch("http://localhost:…/fetch-probe")` | `CONNECT localhost:…` (domain) | subresources proxied |
| `new WebSocket("ws://localhost:…/ws-probe")` | `CONNECT localhost:…` (domain) | WebSockets (Vite HMR) proxied |
| `fetch("http://127.0.0.1:…/ip-probe")` | `CONNECT 127.0.0.1:…` (IPv4) | loopback literals proxied |
| Top-level `http://spike-remote.invalid:…/` (resolvable only by the proxy) | `CONNECT spike-remote.invalid:…` (domain); page loaded | names resolve at the proxy (`socks5h` behaviour); no local DNS lookup |

Not covered by the spike: HTTPS targets, IPv6 literals, DNS prefetch, WebRTC,
and child-webview geometry (fixed separately in Phase 1). **Windows (WebView2)
and macOS (WKWebView) are unverified**: they need the same probe on those
machines.

## Engine requirements

| Engine | How the proxy attaches | Required work | Status |
| --- | --- | --- | --- |
| WebKitGTK | per `WebContext` (per profile) via `NetworkProxySettings`, empty ignore list | none beyond `proxy_url` per environment profile | **verified** (above) |
| WebView2 | `--proxy-server=` on the environment's browser args; Chromium bypasses loopback implicitly | `additional_browser_args` with `--proxy-bypass-list=<-loopback>` (replaces wry's defaults, which must be re-added); one `data_directory` per environment (already true) | unverified |
| WKWebView | KVC `proxyConfigurations` on the data store, behind wry `mac-proxy` = Tauri `macos-proxy` | enable the feature; macOS 14+; per-environment data store (already true) | unverified, including loopback handling |

## Alternatives

| Option | Decision |
| --- | --- |
| A. One desktop SOCKS server per environment, split routing: loopback destinations tunnel to the environment, all others connect directly from this computer | **Recommended.** Matches today's behaviour for public sites; one mechanism for every topology |
| B. `ssh -D` for SSH environments (all traffic through the server) | Only SSH; sends public-site traffic and DNS through the server, unlike every other topology |
| C. Proxy every destination through the environment | Changes where public sites are fetched from and leaks nothing new, but doubles traffic over relay and slows browsing |
| D. Keep the gateway only | No work; `form_post`, LAN/WSL/relay origins stay broken |

## Proposed design (option A)

### Desktop (`apps/desktop`)

- A Rust SOCKS5 server per environment that has previews (no sidecar),
  bound to `127.0.0.1:<random>`:
  - `CONNECT` only (no `BIND`, no `UDP ASSOCIATE`), no authentication (WebView2
    supports none), 64 concurrent connections, 64 KiB buffers;
  - destinations `localhost`, `*.localhost`, `127.0.0.0/8`, `::1` go through the
    environment's tunnel; any other destination is connected directly from this
    computer (DNS resolved here), as an unproxied browser would.
- The environment's long-lived preview host (one per partition, never
  recreated) is created with `proxy_url` pointing at that server. Turning SOCKS
  on or off for an environment therefore takes effect on the next app start,
  or the next first preview of that environment in a session.
- Leak policy for proxied profiles: DNS prefetch off (WebKitGTK
  `enable-dns-prefetching=false`; WebView2 `--dns-prefetch-disable`); WebRTC off
  or proxied-only (WebKitGTK `enable-webrtc=false`, its default; WebView2
  `--force-webrtc-ip-handling-policy=disable_non_proxied_udp`; WKWebView to be
  determined in its spike).

### Tunnel to the environment

Every topology uses one tunnel through BiBCode's own connection, so SSH, LAN,
WSL, and relay behave the same and E2EE connections keep previews inside Noise.

- `preview.tunnelOpen({ threadId, port })` (operate scope, the same admission
  as `gatewayOpen`: loopback only, the client names a port, never a host)
  opens a TCP connection to the server's loopback and returns a stream id.
- Data moves in both directions as bounded chunks with per-stream credit
  (receiver grants N chunks; the sender stops at zero), so a slow page cannot
  balloon server memory. Limits: 64 streams per client, 256 per server.
- Close, error, principal revocation, and disconnect end the stream; the
  desktop resets the matching SOCKS connection.
- Transport choice (open question 2): typed RPC streams (base64 inside JSON,
  allowed today, ~33 % overhead plus per-chunk RPC cost) or a dedicated
  authenticated binary WebSocket (a carve-out to the typed-RPC rule).

### Server (`apps/server`)

- Tunnel handler under `preview/`, reusing the gateway's admission
  (`admit`), principal re-check (30 s), and limits.
- Registered through the `rpc/session.rs` wrappers; long-lived streams are
  added to `is_long_running` so the profiler raises no `rpc.slow`/`rpc.hang`.

### Web (`apps/web`)

- The resolver gains "load the canonical URL directly" for environments whose
  preview host is proxied: no gateway call, no bootstrap. The gateway stays the
  path for browser mode, engines without SOCKS support, and tunnel failure.
- Tunnel failure (open or mid-stream) falls back to the gateway with a notice:
  "Couldn't reach <env> directly; this preview runs on a different address."

## Security

- The SOCKS listener is reachable by any local process, like today's SSH `-L`
  forwards; it adds the remote loopback ports this desktop's user can already
  open through BiBCode. Split routing means it never becomes an open proxy to
  other networks beyond what the local machine already reaches.
- Each environment's listener tunnels only to that environment; a profile is
  bound to one listener.
- The tunnel only ever connects to the server's loopback.
- E2EE connections carry tunnel data inside Noise; relay without E2EE is
  visible to Cloudflare, like relay RPC.

## Decisions (2026-10-14)

1. **Build it now.**
2. **Transport: typed RPC streams.** No carve-out to the typed-RPC rule: the
   tunnel is an RPC stream (server to client) plus a unary write (client to
   server), the same shape as `terminal.attach` and `terminal.writeInput`.
3. **Linux first.** WebKitGTK is the only verified engine; the Windows and
   macOS preview hosts are created without a proxy until their spikes pass,
   and those desktops keep the gateway.
4. **Data path: through the web client.** The connection to an SSH, LAN, WSL
   or relay environment (auth, E2EE, relay) lives in the web client, so the
   desktop's SOCKS server hands each loopback `CONNECT` to the web view over
   new `DesktopBridge` streaming commands, and the web client opens the
   tunnel on that environment's RPC connection:

   ```text
   preview webview -> SOCKS (desktop Rust, 127.0.0.1) -> DesktopBridge (raw bytes)
     -> web client -> preview.tunnel* RPC -> server -> TCP 127.0.0.1:<port>
   ```

   A Rust RPC client per environment was rejected: it would duplicate the
   client-runtime connection stack (auth, Noise, relay). `ssh -D` was rejected
   as the only mechanism: SSH only, and all preview traffic through the server.

## Implementation (2026-10-14)

Living description: [Preview tunnels](../../architecture/remote.md#preview-tunnels).
Differences from the proposal above:

- `preview.tunnelOpen` takes `{ port }`: a tunnel belongs to the caller's
  session, not to a thread, since one environment's profile serves every
  thread's previews. It refuses the server's own port.
- Credit is the transport's own: a stream chunk is sent only after the client
  acknowledged the last, and the web client acknowledges only after the
  desktop wrote it; writes are one awaited chunk at a time.
- Not built yet: falling back to the gateway, with a notice, when a tunnel
  fails mid-session (a tunneled page shows its own connection error until the
  environment reconnects), the WebRTC policy (WebKitGTK's default is off), and
  the Windows and macOS engines.

## Open questions for the user (answered above)

1. **Worth building now?** After #68, every topology reaches previews; SOCKS
   adds real origins for LAN/WSL/relay, `form_post` OAuth, and busy-port SSH.
   Proceed now, or wait for a concrete failing app?
2. **Tunnel transport:** typed RPC streams (no rule change, slower) or a
   dedicated binary WebSocket (faster, a carve-out to the typed-RPC rule)?
3. **Platforms:** ship Linux first (verified engine) while the WebView2 and
   WKWebView spikes run, or wait for all three?

## Testing (when approved)

- Rust (desktop): SOCKS parser and CONNECT-only policy; loopback vs direct
  routing; limits; profile creation with `proxy_url` per engine (unit-level
  argument assembly for WebView2 and the macOS feature gate).
- Rust (server): tunnel admission, credit-based flow control, revocation,
  limits, cleanup on disconnect.
- Web: resolver chooses direct load for proxied environments and falls back to
  the gateway on tunnel failure.
- Native: this spike's probe on each OS, plus an OAuth `form_post` app and a
  DNS/WebRTC leak page, recorded per OS in a test report.

## Appendix: spike recipe

Build a 30-line wry program that creates `WebContext::new(Some(<profile dir>))`,
`WebViewBuilder::new_with_web_context(&mut ctx).with_proxy_config(ProxyConfig::Socks5(ProxyEndpoint { host: "127.0.0.1", port }))`,
loads a URL given on the command line, and exits after 8 s. Run a CONNECT-only
SOCKS5 relay that logs `atyp` and target for each request and maps the name
`spike-remote.invalid` to `127.0.0.1`, and a static HTTP server whose page
fetches `/fetch-probe` on `localhost`, opens `ws://localhost/ws-probe`, and
fetches `/ip-probe` on `127.0.0.1`. Load `http://localhost:<port>/`, then
`http://spike-remote.invalid:<port>/`, and compare the relay log with the table
above. On Windows add `additional_browser_args("--proxy-bypass-list=<-loopback> …")`;
on macOS build with the `mac-proxy` wry feature on macOS 14+.
