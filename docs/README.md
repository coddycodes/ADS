# Documentation

This index points to the living documentation for the current Rust/Tauri 2
application. Dated plans, specifications, dependency reports, and performance
measurements preserve implementation history; they are not authoritative for
current behavior.

## Start here

- [Quick start](./getting-started/quick-start.md)
- [Workspace UI](./user/workspace-ui.md)
- [Provider setup](./providers/README.md)
- [Remote access](./user/remote-access.md)
- [Standalone server installation](./user/server-installation.md)
- [Architecture overview](./architecture/overview.md)

## User guides

- [Linux desktop and AppImage](./user/linux-desktop.md)
- [Workspace UI](./user/workspace-ui.md)
- [Keybindings](./user/keybindings.md)
- [Remote access](./user/remote-access.md)
- [Standalone server installation](./user/server-installation.md)
- [Project data safety and recovery](./guides/project-data-recovery.md)
- [Source control providers](./integrations/source-control-providers.md)

## Provider guides

- [Provider setup and capabilities](./providers/README.md)
- [Codex](./providers/codex.md)
- [Claude](./providers/claude.md)
- [OpenCode](./providers/opencode.md)
- [Cursor](./providers/cursor.md)

## Architecture

- [Overview](./architecture/overview.md)
- [Provider architecture](./architecture/providers.md)
- [Activity observation](./architecture/activity-observation.md)
- [Worktree catalog](./architecture/worktree-catalog.md)
- [RPC and orchestration](./architecture/rpc-and-orchestration.md)
- [Connection runtime](./architecture/connection-runtime.md)
- [Runtime modes](./architecture/runtime-modes.md)
- [Remote environments](./architecture/remote.md)
- [Profiler](./architecture/overview.md#profiler)

## Cloud and environment authentication

- [Coddy Connect](./cloud/coddy-connect-clerk.md)
- [Environment authentication](./cloud/environment-auth.md)
- [Connect authentication flow](./cloud/coddy-connect-auth-flow.md)

## Operations and reference

- [Testing runbooks](./testing/README.md)
- [Flaky-test diagnosis](./testing/flaky-tests.md)
- [Continuous integration](./operations/ci.md)
- [Release process](./operations/release.md)
- [Observability](./operations/observability.md)
- [Effect.fn checklist](./operations/effect-fn-checklist.md)
- [Encyclopedia](./reference/encyclopedia.md)
- [Workspace layout](./reference/workspace-layout.md)
- [Repository scripts](./reference/scripts.md)
- [Repository skills](./skills/): the source of the agent skills
  (`coddy-studio-profiler`, `coddy-studio-release`, `issue-delivery`) that the
  `.agents/skills/` and `.claude/skills/` stubs point to

## Historical material

- [`plans/`](./plans/README.md) contains dated design documents and plans.
- [`superpowers/`](./superpowers/README.md) contains completed planning and
  specification artifacts, such as the
  [profiler design](./superpowers/specs/2026-10-09-profiler-design.md), with its
  [research](./plans/2026-10-09-profiler-research.md) in `plans/`.
- [`dependency-upgrades/`](./dependency-upgrades/README.md) contains dated
  dependency reports and the ledger used by repository tooling.
- [`architecture/measurements/`](./architecture/measurements/README.md)
  contains dated performance measurements. The current baseline remains in
  [Desktop performance baseline](./architecture/desktop-performance-baseline.md).
