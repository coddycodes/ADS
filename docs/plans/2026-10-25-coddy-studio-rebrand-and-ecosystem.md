# Coddy Studio: Rebrand and Ecosystem Integration

Status: proposed. Authored 2026-10-25.
Owner: Coddy.
Supersedes the identity target of
[`docs/superpowers/specs/2026-07-31-bibcode-rebrand-design.md`](../superpowers/specs/2026-07-31-bibcode-rebrand-design.md);
its staged method and compatibility policy are retained.

## Goal

Turn this repository into **Coddy Studio**, the Coddy coding-agent GUI, and wire
it into the existing Coddy ecosystem so that it shares one identity, one device
registration, one configuration root, and one default agent.

Three decisions are fixed:

1. The canonical product name is **Coddy Studio**. Internal identifiers use the
   `coddy-studio` slug; the icon mark reuses the existing Coddy artwork.
2. The rebrand is complete: visible surfaces first, then internal identifiers,
   with compatibility only where an installed build, a persisted store, or an
   external peer would otherwise break.
3. Authentication is the **same system as the Coddy Device Agent** (OAuth 2.0
   Device Authorization Grant, RFC 8628) and the same device registration. The
   existing Clerk/BiBCode Connect identity is retired after parity.
4. The **Coddy CLI is the default and preferred agent**. Codex, Claude, OpenCode,
   and Cursor remain installable drivers but are disabled by default and must be
   re-enabled explicitly by the user.
5. All Studio-owned configuration and state live under **`~/.coddy/studio/`** and
   coexist with the rest of `~/.coddy/*` without overwriting sibling state.

## Non-goals

- Rewriting provider drivers, the RPC contract, or the storage engine.
- Changing the terminal, browser, Git, or worktree feature set.
- Publishing releases, pushing branches, or creating tags.
- Editing `.repos/`, which is vendored reference material.

## Identity Matrix

| Surface | Current | Target | Compatibility |
| --- | --- | --- | --- |
| Product name | BiBCode | Coddy Studio | none needed |
| Compact icon text | BiB | Coddy mark artwork | regenerate all formats |
| npm scope | `@bibcode/*` | `@coddy/*` | private, no alias |
| Root package | `@bibcode/monorepo` | `@coddy/studio-monorepo` | none |
| Oxlint plugin | `oxlint-plugin-bibcode` | `oxlint-plugin-coddy-studio` | none |
| Rust crate | `bibcode-server` | `coddy-studio-server` | none |
| Rust library | `bibcode_server` | `coddy_studio_server` | none |
| Rust binary | `bibcode` | `coddy-studio` | install shim `bibcode` for one release |
| Tauri product name | BiBCode | Coddy Studio | new bundle identity |
| Tauri identifier | `com.bibcode.desktop` | `com.coddy.studio` | no in-place upgrade |
| Deep-link scheme | `bibcode://` | `coddy-studio://` | accept legacy scheme |
| Environment prefix | `BIBCODE_*` | `CODDY_STUDIO_*` | read `BIBCODE_*` as fallback |
| Data root | `~/.bibcode` | `~/.coddy/studio` | adopt legacy store on first run |
| Storage lock | `.bibcode-storage.lock` | `.coddy-studio-storage.lock` | detect legacy lock |
| Descriptor route | `/.well-known/bibcode/environment` | `/.well-known/coddy-studio/environment` | serve both |
| RPC subprotocol | `bibcode.rpc.chunked.v1` | `coddy-studio.rpc.chunked.v1` | offer and accept both |
| Preview bootstrap | `/__bibcode/frame.js` | `/__coddy-studio/frame.js` | serve both |
| CI and artifacts | `bibcode-*` | `coddy-studio-*` | none |

### Environment prefix rules

`CODDY_` is shared with the Coddy agent and CLI. `internal/config/brand`
documents that `CODDY_API_KEY`, `CODDY_HOME`, and other agent-owned names must
not be redefined. Studio therefore uses `CODDY_STUDIO_` for its own variables
and reads the following ecosystem variables without writing them:

- `CODDY_HOME` — root of all Coddy state, default `~/.coddy`.
- `CODDY_API_URL` / `CODDY_BASE_URL` — hub base URL, default
  `https://api.coddy.id/v1`.
- `CODDY_API_KEY` — hub API key when key auth is used instead of device auth.

## Configuration and Data Layout

Studio resolves its own root as `$CODDY_HOME/studio`, defaulting to
`~/.coddy/studio`. It must never place files directly in `~/.coddy/`.

```
~/.coddy/
  config.yaml                 # ecosystem-shared; Studio reads device.* and writes only device.*
  credentials/
    device.key                # hex AES-256-GCM cipher; hub-only decrypt; Studio never re-encrypts
  cli/                        # Coddy CLI state, owned by the CLI
  codeworks/                  # rules, skills, workflows, owned by the installer
  hippocortex/                # shared memory and credential vault
  studio/                     # Studio-owned, this repository
    config.yaml               # Studio settings (non-secret)
    state.sqlite              # session/project store
    backups/
    logs/
    cache/
```

Rules:

- `~/.coddy/config.yaml` is merged, never replaced. Studio loads the YAML
  document, mutates only its own keys, and writes atomically with `0600`.
- Device identity and cipher are the ecosystem's single registration. Studio does
  not create a second device record.
- A legacy `~/.bibcode` store is adopted by reading and copying it into
  `~/.coddy/studio`; the original is preserved.
- Secrets stay out of `studio/config.yaml`. Credentials belong to the existing
  Coddy vault at `~/.coddy/hippocortex/data/credentials.db` or to
  `~/.coddy/credentials/`.

## Authentication: Coddy Device Agent Parity

### Flow

1. Studio ensures a device identity exists in `~/.coddy/config.yaml` under
   `device.*`: `device_id`, `device_otp`, `device_model`, `device_name`,
   `device_authorization_token`, `device_user_plan`, `device_last_checkpoint`,
   `device_os`, `label`, `environment`.
2. `coddy auth login` (or the embedded equivalent) calls
   `POST {hub}/v1/auth/device/register` with `device_id_key` and `device_name`.
3. The user approves a short code at `https://app.coddy.id/auth/device/authorize`.
4. Studio polls `GET {hub}/v1/auth/device/status?device_id=...` every 5 seconds
   until `confirmed`, `revoked`, or `expired`.
5. The resulting access token is persisted with the existing ecosystem keys in
   `~/.coddy/config.yaml`: `oauthtoken`, `accesstoken`, `refreshtoken`,
   `tokenexpiry`.
6. A five-minute verify loop calls `POST {hub}/v1/auth/device/verify` with
   `device_id` and `device_key_cipher`. On the first verify the payload is the
   plaintext canonical JSON; the hub returns the cipher, which Studio stores in
   `~/.coddy/credentials/device.key` with `0600`.
7. Plan changes and revocation arrive through verify and
   `POST {hub}/api/devices/{id}/heartbeat`.

### Local and remote enforcement

- Loopback (`127.0.0.1`, `::1`, `localhost`) keeps the ecosystem's loopback
  trust for the local Studio server.
- Remote and tunnel traffic requires `Authorization: Bearer <token>` or
  `X-Device-Auth: <device_authorization_token>`.
- JWT parameters match the hub: HS256, 15-minute access token, 7-day refresh
  token, refresh tokens stored hashed server-side.
- Studio never attempts to decrypt `device.key`; only the hub holds
  `JWT_SIGNING_KEY`.

### Verified constraints (2026-10-25)

These were confirmed against the agent and hub source, and they change the
implementation:

- `~/.coddy/config.yaml` is rewritten wholesale by the agent
  (`agent/src/internal/config/config.go`). There is no cross-process lock. Studio
  therefore **never writes device identity fields** and never replaces the file.
  Studio delegates sign-in to `coddy auth login` and reads `access_token`,
  `token_expiry`, and `device.*` afterwards. Its own settings live in
  `~/.coddy/studio/config.yaml`.
- `device_id`, `device_otp`, and `device_authorization_token` are ecosystem
  identity. Changing any of them depairs the machine or breaks tunnel
  authentication.
- `~/.coddy/credentials/device.key` is an opaque hub cipher; only the hub holds
  `JWT_SIGNING_KEY`. Studio validates that it is hex and moves it only when the
  hub returns a replacement.
- The device grant does not issue a usable refresh token today. Studio records
  the JWT `exp` claim rather than a 24-hour guess, and falls back to
  `coddy auth login` when the token expires.
- The stable machine credential is the non-expiring
  `device_authorization_token`, accepted by tunnel, heartbeat, mesh, and the
  local agent API.
- On Windows the Go writer's `0600` does not create a restrictive DACL. Studio
  secrets use DPAPI or an explicit DACL.
- `device_id` is not a secret: the hub's status endpoint returns a user JWT for
  an unauthenticated `device_id`. Studio treats `device_id` as a lookup key
  only and never logs tokens.

### Retirement of Clerk

Clerk/BiBCode Connect is kept behind a build flag during migration. It is
removed from the default build once device auth covers sign-in, session
restore, remote access, and revocation. No Clerk secret is required at runtime
after that gate.

## Agent Provider: Coddy CLI First

### Default driver

The Coddy CLI exposes a client/server mode that is a better integration surface
than terminal scraping:

- start: `coddy cli server --host <tcp-or-unix>` (default host is
  platform-specific; HTTP over the local socket or loopback TCP);
- workspace and agent calls under `/v1/workspaces/{id}/...`;
- event stream: `GET /v1/workspaces/{id}/events` (SSE);
- prompt, cancel, permission, MCP, skill, model, and session endpoints;
- generated OpenAPI served by the CLI, consumed once to pin the contract.

The Rust server gains a `coddy` provider driver that supervises one CLI server
per workspace host, maps SSE events to the existing provider event model, and
maps permission requests to the existing approval UI. `coddy cli run` remains a
fallback when the server mode is unavailable.

Verified constraints for the driver:

- Force TCP (`--host tcp://127.0.0.1:<port>`). The Windows default is a
  message-mode named pipe that generic HTTP clients handle poorly.
- Set `CODDY_SERVER_IDLE_TIMEOUT=0` while Studio is attached; the stream is the
  claim, and the server otherwise exits after 60 seconds idle.
- The event stream has no replay and the broker is lossy. On reconnect, re-fetch
  `GET /v1/workspaces/{id}/sessions/{sid}/messages` and rebuild derived state.
- Only `run_complete` terminates a turn. Message parts are cumulative, not
  deltas, and `run_complete` can arrive before the final message; reconcile its
  `text`.
- A `permission_request` can be dropped under back-pressure and block the turn.
  Studio keeps a watchdog that re-fetches pending permissions.
- Cancel is explicit: `POST /v1/workspaces/{id}/agent/sessions/{sid}/cancel`.
  Dropping the connection does not cancel the run.
- `run` and `models` have no `--json`; provider probing uses `GET /v1/version`
  and `GET /v1/models`, never stdout parsing.

### Driver policy

- `coddy` is enabled by default and is the first entry in the model/provider
  picker.
- `codex`, `claude`, `opencode`, and `cursor` are registered but disabled. The
  user enables them in Settings; the UI explains that they are third-party
  agents and are not covered by Coddy device auth.
- A workspace pinned to a disabled driver falls back to Coddy with a visible
  notice, never a silent failure.

### Ecosystem reuse

Coddy CLI already reads `~/.coddy/cli` (config sections, `auth.json`),
`~/.coddy/codeworks` (rules, skills, workflows), and the cortex MCP defaults
(`hippocortex :9090`, `neocortex :9093`). Studio consumes those instead of
duplicating them. Studio adds no second model catalog: the hub's `/v1/models`
remains the only source.

## Roadmap

Each phase has an explicit exit gate. Later phases do not start before the
previous gate passes.

### P0 — Toolchain and baseline

- Node 26.12.1, pnpm 11.28.5, Rust 1.98+ with MSVC, MSVC Build Tools.
- Install workspace dependencies and record the baseline test result.

Gate: `pnpm install`, `pnpm run typecheck`, and `cargo check` complete before
any edit.

### P1 — Visible rebrand

- Product name, window titles, menus, dialogs, onboarding, errors, accessibility
  labels, HTML metadata, marketing copy.
- Icon and packaging assets regenerated from the Coddy artwork.
- Installer display names and artifact titles.

Gate: identity scan for retired visible names finds only documented
exceptions; icon formats decode and are visually inspected; focused UI,
packaging, and asset tests pass; no internal identifier renamed yet.

### P2 — Internal identifiers

- npm scope, Rust crates, binaries, env prefix, data root, deep links, protocol
  subprotocols, well-known routes, lock names, CI filters, docs.

Gate: dependency graph rebuilds; lockfiles refresh; compatibility tests prove
legacy env vars, legacy scheme, legacy descriptor route, and legacy data root
are still accepted; `pnpm run typecheck`, `pnpm run lint`, focused Rust tests,
`cargo fmt --check`, and Clippy pass.

### P3 — Configuration root

- Resolve `~/.coddy/studio`; merge `~/.coddy/config.yaml` device keys atomically.
- Adopt `~/.bibcode` without deleting it.
- Move Studio caches and logs under the new root.

Gate: migration tests cover fresh install, legacy install, read-only root, and
malformed `config.yaml`; no sibling `~/.coddy` key is modified.

### P4 — Device-auth parity

- Device grant login, token persistence, verify loop, heartbeat, revocation.
- Loopback and remote enforcement switched to device credentials.
- Clerk/Connect removed from the default build.

Gate: end-to-end login against the dev hub; token refresh; revoke; offline
startup; remote request authorized by `device_authorization_token`; Clerk-free
runtime verified.

### P5 — Coddy provider default

- `coddy` driver over `coddy cli server`; SSE translation; permission bridge;
  cancellation; session resume.
- Third-party drivers disabled by default with explicit enablement.

Gate: a live coding session through Coddy CLI in desktop and browser modes;
cancel mid-stream; tool approval; resume after restart; disabled-driver fallback
notice.

### P6 — Hub tunnel and fleet

- Replace the BiBCode Connect relay path with the Coddy tunnel protocol and
  device pairing.

Gate: paired device reachable through the hub; master/failover handover
preserves session state.

### P7 — Hippocortex logging

- Stream Studio operational logs into `~/.coddy/hippocortex/data/logs.db`; no
  cloud log storage.

Gate: log write, rotate, and tail; cloud payload audit shows no log bodies.

### P8 — Model gateway

- Route model discovery and calls through the Coddy hub / OmniRoute, including
  local-runner fallback.

Gate: model list parity with the hub; failover to local runner; no second
catalog.

## Risks

| Risk | Mitigation |
| --- | --- |
| Deep rename breaks upstream merges | Land P1 and P2 as separate commits; keep a rename map in this document |
| Existing `~/.bibcode` users lose state | Read-copy adoption; never delete; migration tests |
| Device-auth lockout | Keep Clerk flag until P4 gate passes; loopback trust preserved |
| Bundle identity change drops auto-update | New identifier ships as a fresh install; publish migration note |
| CLI server contract drift | Pin the generated OpenAPI and add a contract test |
| Config merge corrupts `~/.coddy/config.yaml` | YAML round-trip with unknown-key preservation, atomic write, backup |

## Verification Commands

Recorded here so every phase uses the same gates:

```powershell
pnpm install --frozen-lockfile
pnpm run typecheck
pnpm run lint
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Focused suites are added per phase. A phase is not complete until its gate and
the repository's own `AGENTS.md` completion requirements are satisfied.

## Implementation Log

### 2026-10-25 — First pass (P1 and P3)

Environment:

- Node 26.12.1 and pnpm 11.28.5 installed; Rust 1.98.1 toolchain with Clippy and
  rustfmt installed; MSVC 2026 Build Tools detected; `pnpm install` completed.

P1, visible rebrand:

- Product name is `Coddy Studio` in the desktop branding source, web fallback,
  window title, HTML title and splash, marketing site, and living docs.
- `BiBCode Connect` is presented as `Coddy Connect` in prose; the feature itself
  is untouched.
- All icon formats regenerated from the Coddy mark at `logos/favicon.png`:
  universal and macOS PNG, ICO, ICNS, web favicon set, Apple touch icon, and the
  packaging `logo.svg`. Old `bibcode-black-*` and `black-*` assets removed.
- `assets/prod/logo.svg` embeds the raster master because the upstream vector
  master was not part of this fork; replace it when a vector source is available.
- 1,996 word-boundary replacements across 470 files. Identifier-adjacent names
  (`isBiBCodeAction`, `BiBCodeVcs...`) are deliberately untouched for P2.
- Verified: `branding.test.ts` 11/11, `SidebarBrand.test.tsx` 2/2,
  `apply-web-brand-assets.test.ts` 6/6.
- Known exceptions by design: `LICENSE`, `CHANGELOG.md`, dated plans,
  `docs/superpowers`, dependency and measurement reports, the generic Git URL
  fixture in `packages/shared/src/git.test.ts`, the Windows mutex fixture, and
  `apps/server/package/nfpm.yaml` (maintainer identity needs an owner decision).

P3, configuration root:

- The resolved default data root is `$HOME/.coddy/studio`.
- Precedence: `--base-dir`, then `CODDY_STUDIO_HOME`, then `BIBCODE_HOME`
  (legacy), then `CODDY_HOME/studio`, then the default.
- Desktop and server share the same precedence helper.
- Living documentation now refers to `~/.coddy/studio`; historical plans,
  superpowers artifacts, dependency reports, and dated measurements keep their
  original paths.
- Legacy `~/.bibcode` adoption (copy, never delete) is not implemented yet; it
  stays in the P3 gate.

Pending in this pass: full `pnpm run typecheck`, `cargo fmt --check`, Clippy,
P2 identifier migration, P4 device auth, and P5 Coddy provider.

