# Repository Layout

## Product Applications

- `/apps/desktop`: Tauri 2 desktop host. Rust owns native lifecycle, windows,
  settings, menus, dialogs, updates, WSL/SSH preparation, and the in-process
  server lifecycle.
- `/apps/server`: Rust/Axum/Tokio server and native `bibcode` CLI. It owns
  providers, Git, files, terminals, persistence, orchestration, HTTP/WebSocket
  RPC, authentication, diagnostics, and relay integration.
- `/apps/web`: shared React 19 + Vite UI for browser and Tauri WebView modes.
- `/apps/marketing`: Astro marketing site; it is not part of the desktop or
  server production runtime.

## Packages And Infrastructure

- `/packages/contracts`: schema-only Effect/Schema contracts for the desktop
  bridge, WebSocket/RPC, providers, models, sessions, and persisted protocol
  values.
- `/packages/client-runtime`: shared connection supervision, RPC sessions,
  environment caches, and client state used by browser and desktop clients.
- `/packages/shared`: cross-runtime TypeScript utilities exposed through
  explicit package subpaths.
- `/infra/relay`: BiBCode Connect relay deployment package.
- `/oxlint-plugin-bibcode`: repository-specific Oxlint rules.
- `/tools/updater-verifier`: Rust tool used to verify signed updater artifacts.
- `/third_party/portable-pty`: vendored Rust PTY crate included in the Cargo
  workspace.

## Repository Support

- `/scripts`: development, build, release, measurement, and repository tooling.
  Scripts may use Node.js at development time; production does not.
- `/.repos`: read-only source references synchronized by `vp run sync:repos`.
  Application code must never import from this directory.
- `/assets`: source branding and packaging assets.
- `/patches`: package-manager patches applied to third-party dependencies.
- `/.github`: CI, release, packaged-smoke, deployment, and repository-policy
  workflows.

The Vite+ workspace includes `apps/*`, `infra/*`, `oxlint-plugin-bibcode`,
`packages/*`, and `scripts`. The Cargo workspace includes the desktop host,
native server, and updater verifier; `third_party/portable-pty` is consumed as a
local dependency.

## Runtime Boundaries

The frontend talks to the server through typed WebSocket/RPC contracts. In a
packaged desktop build, the Tauri Rust host starts the server in-process,
installs `window.desktopBridge`, and exposes only native host capabilities
through Tauri commands/events. Browser mode connects directly to a native
`bibcode` server and has no native bridge.

## UI Workspace Model

- A project is a repository/workspace root in an environment.
- Every project has a primary card backed by an undeletable default thread for
  the main checkout.
- Worktree cards are workspace threads with `worktreePath` set.
- Center chat panels are sibling threads with `kind: "panel"` that share the
  host worktree while owning their own provider session and transcript.
- Center surfaces are arranged into tab groups. Up to four groups can be shown
  as resizable horizontal or vertical split panes; each group has its own
  active surface, while one group owns focus and new-panel creation.
- Right-panel surfaces are per-thread tools: Plan, Diff, Source Control, Files,
  individual files, Preview, Terminal, Activity, Device (kind `device`), and
  one tab per open pull or merge request. Preview is hidden when the active
  host does not report native preview support; Device is offered only while
  the environment's device previews are on.

Center layout, focus, tab order, and split ratios persist across reloads. Closing
a split pane merges its tabs into the adjacent layout; explicitly closing a tab
closes its panel thread or terminal session as appropriate.

See [Workspace UI](../user/workspace-ui.md) for the user-facing model.
