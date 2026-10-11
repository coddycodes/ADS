# Desktop preview automation and annotation — design

Status: approved 2026-10-09 (synthetic input on every platform).
Research input (historical, verify before reuse):
`docs/plans/2026-10-06-internal-browser-and-mobile-emulator-research.md` §2.5 rows
9–10 (picker/annotate, real agent automation) and §5 Phase 2 item 11.

## Goal

Agents and users get the same preview tools on the desktop's internal browser
that the contracts already define: agent automation (`snapshot`, `click`,
`type`, `press`, `scroll`, `evaluate`, `waitFor`) and the annotation picker
(pick elements or regions, comment, send to the agent). Today the Tauri bridge
(`apps/web/src/tauriPreviewBridge.ts`) reports all of them unsupported, and the
desktop advertises only `status`, `open`, and `navigate`.

## Constraints

- Preview webviews have **no IPC** by design (a previewed page must not reach
  Tauri commands). Every result travels by the host evaluating script in the
  page and reading a JSON value back (`PlatformWebviewOps::eval_json`, already
  implemented on Windows, macOS, and Linux).
- Only WebView2 has a DevTools protocol in process; WKWebView and WebKitGTK do
  not. One engine must work on all three.
- No Node runtime or sidecar (`AGENTS.md`).

## Decisions

1. **One injected page engine.** A TypeScript module (new
   `packages/preview-page-agent`, bundled to a single IIFE at build time and
   embedded in the desktop binary with `include_str!`) installed with
   `WebviewBuilder::initialization_script` so it runs before page scripts on
   every navigation. It defines a frozen `window.__bibcodePreview` with the
   operations below and keeps console and network buffers.
2. **Synthetic input (recommended).** Clicks dispatch `pointerdown`/
   `mousedown`/`pointerup`/`mouseup`/`click` at the element's center after
   `scrollIntoView`; typing focuses the element, sets the value through the
   native setter, and fires `input`/`change` (contenteditable uses
   `beforeinput`/`insertText`); keys dispatch `keydown`/`keypress`/`keyup` and
   perform the default action for Enter (form submit), Tab (focus move), and
   Backspace/Delete in text fields. Events are `isTrusted: false`; apps that
   refuse untrusted events are a documented limitation.
3. **Locators:** the contract's Playwright-style subset — `role=<role>[name="…"]`
   (exact or `/regex/`), `text=…` (substring or `"exact"`), `css=` or a plain
   CSS selector, `>> nth=<n>`, and `>>` chaining; anything else fails with
   "This selector form isn't supported in the desktop browser yet" naming the
   form. Roles and accessible names follow the ARIA in HTML mapping (implicit
   roles, `aria-label`, `aria-labelledby`, label/alt/title/text).
4. **Snapshot:** URL, title, loading state, visible text (capped), interactive
   elements with role/name/stable selector/rect, a simplified accessibility
   tree (roles and names, depth- and size-capped), console entries and network
   entries from the engine's buffers (console methods and `error`/
   `unhandledrejection` hooks; `fetch`/`XMLHttpRequest` wrappers plus
   `PerformanceResourceTiming` for other loads), the host's action timeline,
   and the existing native screenshot.
5. **Evaluate** runs the expression as an async function body in the page,
   JSON-serializes the result with a size cap (1 MiB) and cycle handling, and
   returns errors with their message and stack.
6. **WaitFor** polls in the page (locator, text, or URL) until the timeout from
   the contract (default 15 s, max 60 s), so one host call waits.
7. **Annotation picker:** `pickElement` injects an overlay into a closed shadow
   root (themed by `setAnnotationTheme`) that supports element picking,
   region selection, freehand strokes, and a comment box; React component
   name and source frames are read best-effort from the React DevTools global
   hook/fibers (react-grab's approach). The host polls
   `__bibcodePreview.takeAnnotation()` every 150 ms until submit or cancel,
   then adds the native screenshot; `cancelPickElement` removes the overlay.
8. **Capabilities:** the Tauri bridge registers `automation: true` and
   `picker: true` and advertises the full operation set except
   `recordingStart`/`recordingStop` (recording stays unsupported).

## Components

- `packages/preview-page-agent` (TypeScript, tested with happy-dom/jsdom-like
  fixtures under `vp test`): engine, locator resolver, accessibility names,
  input synthesis, buffers, overlay.
- Desktop (`apps/desktop/src-tauri/src/preview/`): embed the bundle as an
  initialization script for preview webviews; new commands
  `desktop_preview_automation(tab_id, operation, input_json)` and
  `desktop_preview_annotation_poll(tab_id)` that call `eval_json` with bounded
  timeouts and map engine errors to the existing typed preview automation
  errors.
- Web (`tauriPreviewBridge.ts`): implement `automation.*`, `pickElement`,
  `cancelPickElement`, `setAnnotationTheme`; register capabilities.

## Security

- The engine runs in the page's own context with no privileges: it can only do
  what page script can do. Results are size-capped JSON strings parsed by the
  host; nothing from the page is evaluated on the host side.
- A page can tamper with `__bibcodePreview` (it is the user's or agent's own
  dev app); the bundle is frozen and re-installed on navigation, and the host
  validates every result against the contract schemas.

## Testing

- Engine unit tests: locator forms (role/name, text, css, nth, chaining, errors),
  accessible names, click/type/press event sequences and default actions,
  evaluate serialization caps and errors, waitFor timeouts, console/network
  buffering, snapshot shape, overlay pick/region/comment/submit/cancel.
- Desktop: command routing and timeout mapping; bundle embedded and installed.
- Web: bridge methods call the commands and validate results; capabilities
  advertised.
- Native validation (runbook): agent `preview_*` tools against a Vite app on
  Windows, macOS, and Linux; annotation round trip into the composer.

## Out of scope

Recording, trusted (OS-level) input, browser-mode automation, cross-origin
iframes inside previewed pages (the engine sees the main frame only).
