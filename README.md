# Coddy Studio

Coddy Studio is a web and Tauri 2 desktop GUI for coding agents. The current provider
drivers are Codex, Claude, OpenCode, and Cursor (Early Access).

Coddy Studio is part of the Coddy ecosystem. It shares the Coddy device identity,
the `~/.coddy` configuration root, and Coddy CLI integration with the rest of the
Coddy platform.

This project is derived from the public [mubeda/CoddyStudio repository](https://github.com/mubeda/CoddyStudio).

## Technology

- Desktop: Tauri 2 with a Rust host and the operating system WebView.
- Frontend: React 19, Vite, TypeScript, TanStack Router, Zustand, and Effect.
- Application server: Rust with Axum and Tokio, embedded in the desktop process
  and available as the native `coddy-studio` headless server.
- Transport: typed WebSocket/RPC contracts shared by browser and desktop clients.

The React frontend is shared unchanged between browser and desktop modes. Tauri-specific behavior is
kept behind `window.desktopBridge`. Production builds contain no Electron,
Node.js runtime, or TypeScript server.

## Origins and acknowledgements

Coddy Studio began as a fork of [pingdotgg/t3code](https://github.com/pingdotgg/t3code). It has since
been substantially rewritten: the original Electron and Node.js runtime was replaced by a Rust
application server and a Tauri 2 desktop host. Coddy Studio is now a Rust/Tauri application and does not
ship Electron or a production Node.js runtime.

Coddy Studio also uses code and ideas from [stablyai/orca](https://github.com/stablyai/orca), alongside
workflow inspiration from [Conductor](https://conductor.build/). We are deeply grateful to the
maintainers and contributors of T3 Code and Orca. Their important open-source work made Coddy Studio
possible and continues to help shape it.

## Installation

> [!WARNING]
> Install and authenticate at least one configured provider before use:
>
> - Codex: install [Codex CLI](https://developers.openai.com/codex/cli) and run `codex login`
> - Claude: install [Claude Code](https://claude.com/product/claude-code) and run `claude auth login`
> - Cursor: install [Cursor CLI](https://cursor.com/cli) and run `cursor-agent login`
> - OpenCode: install [OpenCode](https://opencode.ai) and run `opencode auth login`

### Desktop app

Download the latest desktop build for your platform from
[GitHub Releases](https://github.com/mubeda/CoddyStudio/releases):

- macOS: `.dmg` (Apple Silicon `arm64` or Intel `x64`)
- Windows: `.exe` installer (ARM64 or x64)
- Linux: `.AppImage` (ARM64 or x64)

The same GitHub Release contains standalone `coddy-studio` server archives for macOS,
Windows, and Linux on both architectures. Linux also provides direct-download `.deb`
and `.rpm` packages. See [Standalone server installation](docs/user/server-installation.md)
for checksums, packaged web assets, listener safety, installation, and manual updates.

Desktop releases are built by the Tauri 2 pipeline in `apps/desktop`.

> [!NOTE]
> macOS releases are ad-hoc signed without an Apple Developer identity or
> notarization. On first launch, macOS will block the app; open System Settings
> → Privacy & Security and choose **Open Anyway** for Coddy Studio. For local testing,
> the quarantine flag can instead be removed with
> `xattr -dr com.apple.quarantine "/Applications/Coddy Studio.app"`.
> Because each ad-hoc signed build has a new code identity, macOS also asks
> again for folder and network permissions after every update; grant them once
> per version.
> Windows releases remain unsigned; choose "More info" → "Run anyway" if
> SmartScreen warns.

> [!IMPORTANT]
> On Windows, Coddy Studio 0.7.x closes as soon as you choose **Install** on an
> update, before the installer starts, and stays on the old version
> ([#95](https://github.com/mubeda/CoddyStudio/issues/95)). Download the latest
> `.exe` installer from [GitHub Releases](https://github.com/mubeda/CoddyStudio/releases)
> and run it: it keeps your projects and settings. Version 0.8.0 and later
> install their updates themselves.

### Run from source

See [Getting started](./docs/getting-started/quick-start.md), or jump to the
[contributor setup](#if-you-really-want-to-contribute-still-read-this-first)
below to install the toolchain and run the app locally.

Source development uses Node.js only for frontend and repository tooling. It is
not shipped as part of the application runtime.

## Current UI

Coddy Studio is organized around three work areas:

- The left panel groups projects, the primary checkout for each project, and
  eager worktree threads. Use it to add local projects, clone repositories,
  create worktrees, pin/unread rows, and switch between agent sessions.
- The center panel hosts the active chat. Its `+` menu can open additional AI
  chat panels, a terminal panel in the same worktree, or the custom action
  dialog. Up to four split panes can share a worktree while keeping their agent
  sessions isolated.
- The right panel hosts Browser, Terminal, Diff, Activity, Plan, Files, and
  Source Control tools. Source Control supports staging, commit history, AI
  commit messages, per-file actions, and pull/push/PR flows. Files supports
  context menus, create/rename/delete/duplicate, external open/preview, and
  explicit Ctrl/Cmd+S saves.

Project headers also open **Git Manager** for local Git work and
**Pull Requests** for GitHub/GitLab repository lists, reviews, metadata edits,
merge and guarded checkout. Pull Requests shows host permission reasons and
refreshes on explicit interaction, with local initials and image links.

See [Workspace UI](./docs/user/workspace-ui.md) for the detailed guide.

## Some notes

We are very very early in this project. Expect bugs.

We are not accepting contributions yet.

There's no public docs site yet, so start with the [documentation index](./docs/README.md).

## Documentation

- [Documentation index](./docs/README.md)
- [Getting started](./docs/getting-started/quick-start.md)
- [Workspace UI](./docs/user/workspace-ui.md)
- [Source Control](./docs/integrations/source-control-providers.md#source-control-panel)
- [Architecture overview](./docs/architecture/overview.md)
- [Provider guides](./docs/providers/README.md)
- [Operations](./docs/operations/ci.md)
- [Reference](./docs/reference/encyclopedia.md)

## If you REALLY want to contribute still.... read this first

### Install `vp`

Coddy Studio uses Vite+ so you'll need to install the global `vp` command-line tool.

#### macOS / Linux

```bash
curl -fsSL https://vite.plus | bash
```

#### Windows

```bash
irm https://vite.plus/ps1 | iex
```

Checkout their getting started guide for more information: https://viteplus.dev/guide/

### Install dependencies

```bash
vp install
```

Read [CONTRIBUTING.md](./CONTRIBUTING.md) before opening an issue or PR.
