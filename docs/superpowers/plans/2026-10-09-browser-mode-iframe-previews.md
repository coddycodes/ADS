# Browser-mode iframe previews — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** In browser mode, gateway previews render inside BiBCode's preview panel in an iframe, with the address bar, title, Back, Forward, and Reload following the page.

**Architecture:** The gateway makes every target frameable by pages on its own host, serves a same-origin navigation reporter (`/__bibcode/frame.js`), and inserts it into HTML heads. The web app gains a browser-mode `DesktopPreviewBridge` backed by iframes: the existing preview panel (tabs, chrome, surface bounds) drives it exactly as it drives native child webviews, and reporter messages become tab state.

**Tech Stack:** Rust (hyper gateway in `apps/server/src/preview/gateway/`), TypeScript/React (`apps/web`), `vp test`.

**Spec:** `docs/superpowers/specs/2026-10-09-browser-mode-iframe-previews-design.md`

## Global Constraints

- Framing: every gateway response carries `Content-Security-Policy: frame-ancestors http://<host>:* https://<host>:*` (`<host>` = the request's host without port); upstream `X-Frame-Options` is removed and `frame-ancestors` directives are stripped from upstream CSP headers (other directives kept).
- Reporter: `GET /__bibcode/frame.js` is served without a session, `content-type: text/javascript`, `cache-control: no-cache`.
- Injection: only `200` `text/html` responses without `content-encoding` (or `identity`); `<script src="/__bibcode/frame.js"></script>` right after the first `<head…>` tag found in the first 64 KiB; otherwise pass through unchanged; `content-length` is dropped when injecting.
- Document requests (`sec-fetch-dest: document` or `iframe`) are sent upstream with `accept-encoding: identity`.
- Bootstrap accepts `ui=<origin>`; it is kept only if it is `http`/`https`, has no path/query, and its host equals the gateway request's host; the bootstrap page stores it in `sessionStorage["bibcode-ui-origin"]`.
- Reporter messages: `{ type: "bibcode-preview-frame", url, title, canGoBack, canGoForward }` posted to the stored UI origin only; commands `{ type: "bibcode-preview-command", command: "back" | "forward" | "reload" }` obeyed only from that origin.
- Browser mode frames only when `location.protocol === "http:"`; HTTPS UIs keep opening previews in new tabs.
- iframe: `sandbox="allow-scripts allow-same-origin allow-forms allow-popups allow-popups-to-escape-sandbox allow-modals allow-downloads"`, `allow="clipboard-read; clipboard-write; fullscreen"`, `referrerpolicy="no-referrer"`.
- Browser-mode automation stays status/open only; the frame bridge registers `automation: false`, `picker: false`, `recording: false`.

## Review Focus

- A dev server that compresses HTML even when asked for identity: the page must still load (uninjected), with navigation sync off.
- A page without `<head>` or with a huge preamble: passes through unchanged, never truncated.
- Messages from another frame or origin (another preview, an ad iframe inside the page) must not move the address bar.
- An HTTPS UI must never try to frame (mixed content): tab fallback.
- Closing and reopening a panel tab must not leak iframes or message listeners.

---

### Task 1: Gateway framing, reporter, and injection

**Files:** `apps/server/src/preview/gateway/proxy.rs`, new `apps/server/src/preview/gateway/frame.rs` (header rewrite, injector, reporter script, `ui` validation), `apps/server/src/preview/gateway/mod.rs`, tests in `apps/server/tests/preview_gateway.rs` and `frame.rs` unit tests; `docs/architecture/remote.md` (Preview gateway).

- [ ] Unit tests (`frame.rs`): `frame_ancestors_policy("box.lan:41000") == "frame-ancestors http://box.lan:* https://box.lan:*"` (IPv6 `[::1]:p` → `http://[::1]:*`); `strip_frame_ancestors("default-src 'self'; frame-ancestors 'none'; script-src 'self'") == "default-src 'self'; script-src 'self'"` and a policy that was only `frame-ancestors` becomes empty (header removed); `validated_ui_origin("http://box.lan:3773", "box.lan:41000") == Some(..)`, other host/path/scheme → `None`; injector: `<html><head lang=x><title>` → script after `<head lang=x>`, split across two chunks, no `<head>` within 64 KiB → unchanged bytes, `<HEAD>` case-insensitive, `<header>` not matched.
- [ ] Integration tests (`preview_gateway.rs`, existing harness): upstream HTML with `X-Frame-Options: DENY` and `content-length` → response has the gateway `frame-ancestors` CSP, no X-Frame-Options, script injected, no content-length; gzip HTML passes unchanged; `/__bibcode/frame.js` 200 without cookie; bootstrap with `ui=` sets the sessionStorage line in its page and frame-ancestors on the bootstrap response; document request reaches upstream with `accept-encoding: identity`.
- [ ] Implement; run `cargo test -p bibcode-server --test preview_gateway` and `--lib preview::gateway`.
- [ ] Docs: framing, reporter, injection, `ui` parameter in `docs/architecture/remote.md`.
- [ ] Gates (fmt, clippy server, tests), Codex review, commit `feat(server): let BiBCode frame gateway previews and follow their navigation`.

### Task 2: Browser-mode frame preview bridge

**Files:** new `apps/web/src/browser/framePreviewBridge.ts` (+ test), `apps/web/src/components/preview/previewBridge.ts`, `apps/web/src/previewStateStore.ts` (`isPreviewSupportedInRuntime`), `apps/web/src/browser/openLink.ts` (app destination in browser mode only for gateway URLs), `apps/web/src/browser/previewGateway.ts` (`ui=` on the bootstrap URL in browser mode), tests.

- [ ] Tests (`framePreviewBridge.test.ts`, happy-dom/jsdom per existing web tests): `createTab` adds a sandboxed iframe (exact sandbox/allow/referrerpolicy) inside a fixed layer; `setBounds` positions/sizes it and hides it when not visible; `navigate` sets `src`; a `load` event reports `Success` with the last navigated URL; a reporter message from that iframe's `contentWindow` and the gateway origin updates `navStatus`, title, `canGoBack`/`canGoForward`; messages from another window or origin are ignored; `goBack`/`goForward`/`refresh` post commands to the frame's origin; `closeTab` removes the iframe and its listener; unsupported operations reject with the existing unsupported error; capabilities register automation/picker/recording false.
- [ ] Tests: `previewBridge` resolves to the frame bridge in browser mode on `http:` and to `null` on `https:`; `isPreviewSupportedInRuntime` follows it; `openLink` in browser mode opens gateway URLs in the panel and other URLs in the system browser; `resolveForNavigation` adds `&ui=<location.origin>` in browser mode only.
- [ ] Implement; run `vp test run src/browser src/components/preview src/previewStateStore`.
- [ ] Gates (`vp check`, typecheck, web suite), `UI.md` and `vercel-react-best-practices` reviews of touched UI, Codex review, commit `feat(web): show gateway previews inside the panel in browser mode`.

### Task 3: Docs and runbooks

- [ ] `docs/user/workspace-ui.md` (browser mode previews in the panel; HTTPS UI opens tabs), `docs/architecture/overview.md` (browser mode preview host), `docs/testing/cross-platform-validation.md` (browser-mode panel preview: Vite app, navigation, Back/Forward, HMR, a page that forbids framing, an HTTPS UI falling back), spec status line, commit `docs: browser-mode iframe previews`.
