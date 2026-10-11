# Browser-mode iframe previews — design

Status: implemented 2026-10-09 (navigation reporter injected; same-host framing); see Implementation notes.
Builds on: [preview gateway (Phase 1)](2026-10-07-internal-browser-preview-gateway-design.md)
("Browser mode opens gateway targets in a new top-level tab (no iframe)") and
[SSH same-port](2026-10-09-preview-ssh-same-port-design.md). Second of the Phase 1
leftovers. Relay reach (#68), SOCKS profiles (#69), and emulators (#70) are
separate issues.

## Goal

In browser mode (BiBCode's web UI in a normal browser), a dev server on the
server's loopback (`http://localhost:5173`) opens **inside BiBCode's preview
panel**, like the desktop's internal browser, instead of in a new browser tab.
The address bar shows the canonical `localhost` URL and follows the page;
Back, Forward, and Reload work.

## Current state

- `resolveForNavigation` (`apps/web/src/browser/previewGateway.ts`) mints a gateway
  capability and returns `http://<server>:<gateway port>/__bibcode/bootstrap?…`;
  browser mode opens it in a new tab (`openPendingTab`, `browserTab.ts`) and the
  preview panel has no live view.
- The gateway is plain HTTP on its own port per target, with a `SameSite=Lax`
  session cookie and Origin enforcement for writes and WebSocket upgrades.

## Decisions

1. **Same-site framing works without new cookies.** The UI
   (`http://<host>:<server port>`) and a gateway target
   (`http://<host>:<gateway port>`) share scheme and host, so they are the same
   *site*: the `Lax` gateway cookie is sent inside the iframe. Another site that
   frames the gateway is cross-site, gets no cookie, and sees the `401` page.
2. **Mixed content falls back to a tab.** An HTTPS UI (Tailscale Serve, a TLS
   reverse proxy) cannot frame the HTTP gateway. Then the preview keeps opening
   in a new tab, with the panel showing "Opened in a new tab: this BiBCode
   page is served over HTTPS and previews are HTTP" and an **Open again** action.
3. **Framing headers.** The bootstrap's validated UI origin is kept in the
   gateway session; responses for that session answer
   `Content-Security-Policy: frame-ancestors 'self' <that UI origin>` and drop the
   upstream's `X-Frame-Options` and `frame-ancestors` directives, so dev
   servers that forbid framing still preview, and only the BiBCode UI that
   opened the preview can frame it. Sessions without a UI origin (desktop,
   system-browser tabs) keep the upstream's rules (tightened 2026-10-09 after a
   security review).
4. **Navigation reporting (recommended).** For `text/html` responses the gateway
   inserts `<script src="/__bibcode/frame.js"></script>` as the first element
   of `<head>` (a same-origin script, allowed by `script-src 'self'`). The
   script posts `{ url, title, canGoBack, canGoForward }` to `window.parent`
   with the UI origin as `targetOrigin` on load, `popstate`, `hashchange`, and
   `history.pushState`/`replaceState`, and obeys `{ command: "back" |
   "forward" | "reload" }` messages from that origin only. The panel maps the
   reported gateway URL back to the canonical `localhost` URL. Back and Forward
   need the Navigation API (it traverses only the frame's history; `history`
   would move BiBCode's own tab); without it they are disabled. Pages whose CSP
   blocks it (`strict-dynamic`, nonce-only) still preview; the address bar then
   shows the last opened URL and Back/Forward are disabled with a tooltip
   ("This page doesn't allow BiBCode to follow its navigation").
5. **The iframe** is `sandbox="allow-scripts allow-same-origin allow-forms
   allow-popups allow-popups-to-escape-sandbox allow-modals allow-downloads"`,
   `allow="clipboard-read; clipboard-write; fullscreen"`, and
   `referrerpolicy="no-referrer"`. The gateway origin differs from the UI
   origin, so the page cannot reach the UI's DOM or storage.
6. **Automation in browser mode is unchanged** (status and open only; open now
   shows the page in the panel). Driving a cross-origin iframe needs the
   desktop engine's design; out of scope here.

## Components

- **Server (`apps/server/src/preview/gateway/proxy.rs`):** response header
  rewrite (decision 3); HTML head injection with a bounded streaming rewriter
  (only the first chunk up to `<head>`; bodies stay streamed; compressed bodies
  are passed through uninjected); `GET /__bibcode/frame.js` serving the
  reporter (static, cacheable, no cookie needed).
- **Web:** a `BrowserModePreviewFrame` surface in the preview panel replacing
  the "opened in a new tab" state when framing is possible; message handling
  keyed by the frame's `contentWindow` and origin; canonical URL mapping via
  `canonicalizePreviewUrl`; Back/Forward/Reload wired to messages (Reload
  re-resolves through the gateway when the session expired).
- **Fallback:** HTTPS UI, gateway refusals, or a user preference "Open
  previews in a new tab" keep today's tab behaviour.

## Error handling

- Gateway `401`/`502` pages render inside the frame with their own copy; the
  panel offers **Reopen** (re-resolve, new capability).
- `postMessage` from any other source or origin is ignored.
- A frame that never reports within 5 s after load keeps working without
  navigation sync (decision 4 fallback).

## Testing

- Server: header rewrite (X-Frame-Options dropped, frame-ancestors set),
  injection only into `text/html` heads, compressed/streamed bodies untouched,
  `/__bibcode/frame.js` served without auth.
- Web: frame surface choice (http UI → frame, https UI → tab), message
  validation (origin/source), URL mapping, Back/Forward/Reload commands,
  CSP-blocked fallback state.
- Runbook: browser mode on a LAN server — preview a Vite app in the panel,
  navigate, Back/Forward, HMR, and an HTTPS UI falling back to a tab.

## Implementation notes (2026-10-09)

Where the shipped behavior narrows or departs from the decisions above:

- **Bridge, not a new surface.** Browser mode implements `DesktopPreviewBridge`
  with iframes (`apps/web/src/browser/framePreviewBridge.ts`) mounted inside
  each tab's `BrowserSurfaceSlot`, so they share the panel's stacking and focus
  context (the narrow-window sheet included), and `PreviewView` is unchanged
  apart from capability gating (`screenshot`, `pageTools`). A slot that
  remounts (the panel moving into the sheet) takes its frame along, which
  reloads the page it last reported.
- **Same host only.** The gateway keeps a UI origin only when its host equals
  the gateway's, so the panel frames only gateways on the page's own host.
  Other environments' gateway links open in new tabs. A same-host server's
  loopback goes through its gateway rather than direct, keeping the literal
  loopback address a direct open would load; HTTPS and `*.localhost` names,
  which the gateway refuses, still open directly in a new tab.
- **UI policy.** The served UI adds `frame-src 'self' http://<its host>:*`;
  IPv6 hosts get no host source, so an IPv6 page frames nothing and keeps the
  new-tab behavior.
- **HTTPS fallback.** An HTTPS page has no preview bridge: there is no Browser
  surface, the preview shortcut explains why, and links open in new tabs. The
  panel state with **Open again** was not built.
- **Link setting.** **Open links in** stays desktop-only. In browser mode
  gateway links on the page's host open in the panel (modifier-click: new tab)
  and every other link opens a new tab.
- **Leaving the frame.** A same-host page opens directly (no gateway) in a new
  tab or without a thread. The reporter opens plain clicks on links to other
  origins in a new tab, except links to the server's `localhost`, which it asks
  BiBCode to open as new preview tabs (`onNewWindowRequest`); a redirect to another host is blocked by the browser
  inside the frame, and Reload returns to the last reported page.
- **Keys inside the frame.** BiBCode's shortcuts don't reach the host while the
  frame has focus (no key relay); the reporter keeps Ctrl/Cmd+R and F5 to the
  frame. A page script's `window.open` of a loopback URL still opens this
  computer's localhost.
- **Failures stay local.** A frame's refusal never fails the shared tab.
- **Hidden surfaces.** Like the desktop's native views, a frame closes when
  its surface stops being the active one, so switching to Terminal and back
  reloads the page.
- **Deferred.** Back/Forward disabled without the Navigation API, or on a page
  whose CSP blocks the reporter, have no explanatory tooltip; the first frame
  `load` (the bootstrap page) settles the tab before the reporter speaks.

## Out of scope

Automation in browser mode, relay (#68), SOCKS (#69), HTTPS gateway targets.
