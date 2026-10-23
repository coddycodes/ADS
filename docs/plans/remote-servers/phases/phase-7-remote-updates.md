# Phase 7: Remote Server Updates Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Any BiBCode client can see whether a saved remote server is up to date, trigger a check, and — for desktop-hosted servers — install the update remotely through the desktop host's existing updater; headless `bibcode serve` reports honest manual-update instructions.

**Architecture:** A new `updater.status` / `updater.check` / `updater.install` WS RPC surface (contract in `packages/contracts/src/remoteUpdate.ts`, Rust mirror in `apps/server/src/remote_update.rs`) served by a `RemoteUpdateService` that consults an optional host-injected `RemoteUpdateDelegate`. The desktop host implements the delegate over its existing `DesktopUpdateManager` (the same flow a local user triggers, including the d8daae10 update-protection drain of the in-process backend); headless servers have no delegate and answer in manual mode. The environment descriptor embeds `RemoteUpdateSupport` and a default-false `remoteUpdateControl` capability so clients know before asking. Client-side, per-environment snapshot atoms plus a max-2-concurrent fan-out drive the settings rows and context-card badge.

**Tech Stack:** Rust (Axum/Tokio server, Tauri 2 desktop host, `tauri-plugin-updater`), TypeScript (Effect Schema contracts, Effect Atom client-runtime, React/Tailwind web UI), TS↔Rust RPC parity fixtures.

**Spec:** `docs/plans/remote-servers/remote-servers-spec.md` — §4.5 is the normative contract for every name and wire shape in this plan; §4.8 names the UI slots; the master plan is `docs/plans/remote-servers/remote-servers-plan.md` (this file is Phase 7; depends on Phases 2, 4, 6).

## Global Constraints

- Zero reference-product strings in code, identifiers, UI copy, or comments; product
  strings are "BiBCode"/"bibcode" by context (spec D16).
- `packages/contracts` stays schema-only; every new WS method gets a Rust mirror and an
  entry in the TS↔Rust parity manifests; every RPC method declares exactly one scope in
  `apps/server/src/auth/scope.rs`.
- All new descriptor/contract fields are additive and decode-defaulted so older servers
  keep working (no breaking wire changes).
- No production Node runtime, no Electron, no sidecars; desktop-privileged operations
  cross `DesktopBridge`; normal traffic uses typed HTTP/WS RPC.
- Preserve unrelated worktree changes — in particular the user's pending deletions under
  `docs/plans/2026-08-24-environment-project-management/` must never be restored or
  committed by this work.
- Every phase: focused tests for changed behavior, `vp check`, `vp run typecheck`; Rust
  phases additionally `cargo fmt --all --check`, relevant Rust tests, and Clippy for
  affected targets with warnings denied; final `git diff`/`git status --short` review.
- Living docs (`docs/architecture/remote.md`, `connection-runtime.md`, `overview.md`) and
  `docs/testing/` runbooks update in the same patch as the behavior they describe; phases
  that change no runbook-relevant behavior state "reviewed and remain accurate".

## Pinned phase decisions (read before any task)

These decisions were made after reading the current source; each records the evidence.
Tasks below implement them; do not silently revisit them.

1. **Scopes: `updater.status` → `orchestration:read`; `updater.check` and
   `updater.install` → `orchestration:operate`.** The spec allows "closest existing
   scopes or a new `server:read`/`server:operate` pair". Two facts decide it
   (`apps/server/src/auth/model.rs`): (a) `STANDARD_SCOPES` — what every normally paired
   client token carries — already includes `terminal:operate`, i.e. arbitrary shell on
   the host, so a dedicated updater scope would add no real privilege boundary; and
   (b) scopes are persisted per session, so introducing a new scope would orphan every
   already-paired client (their stored tokens would lack it) until re-pairing. The
   `server.*` read/mutate methods already use exactly this read/operate split
   (`apps/server/src/auth/scope.rs:35-41, 75-82`).
2. **Host seam: a `RemoteUpdateDelegate` trait defined in `apps/server`, implemented in
   `apps/desktop`, injected at server start.** This follows the existing
   `DesktopUiProcessObserver` pattern (`apps/server/src/diagnostics/resource_sampler.rs:53`,
   injected via `ServerRuntime::start_with_ui_process_observer`,
   `apps/server/src/lifecycle.rs:107`; manual `Pin<Box<dyn Future>>`, no `async_trait`
   crate). There is no existing server→host push channel — the d8daae10 maintenance seam
   (`/api/maintenance/update/*` + `x-bibcode-desktop-bootstrap-token`,
   `apps/server/src/maintenance.rs`) runs desktop→server only and is reused _unchanged_:
   when the delegate triggers the desktop install flow, `DesktopUpdateManager::install_update`
   already drains the in-process backend through it. Only the InProcess primary desktop
   backend gets a delegate; WSL/external backends and headless `bibcode serve` are
   manual mode. Amended spec §4.5 (2026-08-27) explicitly blesses this server→host
   delegate injected at server construction: `DesktopBridge` is the renderer↔host seam
   and cannot carry a request that originates from a remote client's RPC, so the
   Global Constraints line about desktop-privileged operations crossing `DesktopBridge`
   does not apply to this seam.
3. **`RemoteUpdateSupport` lives on `ServerConfig`** (`apps/server/src/config.rs`,
   headless default `manual`/`manual-update-required`). The desktop host derives the
   value from the _same_ `app.updater()` availability check the delegate flow uses, so
   the descriptor and the RPC behavior cannot drift. Amended spec §4.5: **all three**
   descriptor producers read it — editing fewer is a latent bug:
   `apps/server/src/http.rs` (`/.well-known/bibcode/environment`),
   `apps/server/src/production/control.rs::environment_descriptor` (`server.getConfig`),
   and the Connect/relay descriptor built inline in `apps/server/src/lifecycle.rs`
   (~line 270, the `json!` block handed to `ConnectMcpService::open`).
4. **Headless `latestVersion` is honestly `null`.** The update feed URL exists only in
   the desktop release config (`apps/desktop/src-tauri/tauri.release.conf.json` —
   GitHub `latest.json`; the dev config has empty endpoints), and `apps/server` has no
   feed knowledge or updater dependency. So for `installMode: "manual"` servers,
   `updater.check` performs a _self_ version lookup: it returns a fresh snapshot with
   `serverVersion` (so a client can confirm a manual update landed), `latestVersion: null`,
   `state: "idle"`. It never fabricates knowledge of the newest release. `updater.install`
   fails with `remote_update_manual_required` and the UI shows copy-paste instructions.
   This is the documented v1 choice (recorded in `docs/architecture/remote.md`, Task 9);
   teaching servers a feed URL is a possible follow-up, not this phase.
5. **Update state is server-owned.** The snapshot is held by `DesktopUpdateManager`
   (desktop) or is statically derivable (manual mode); the client re-queries
   `updater.status` to restore it. Spec §6's "update state survives navigation
   (atom-held snapshot)" means the _query atom family_ keeps the last snapshot per
   environment — no client-side persistence is built.
6. **Check/status failures ride inside the snapshot** (`state: "error"`, `error` string).
   RPC-level typed errors exist only for `updater.install`
   (`RemoteUpdateInstallError`, code `remote_update_manual_required`) plus the standard
   `EnvironmentRpcError` transport/auth unions.

## File map

| File                                                                                                                                                            | Responsibility                                                                                   |
| --------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------ |
| `packages/contracts/src/remoteUpdate.ts` (new)                                                                                                                  | Schema-only remote-update contract (spec §4.5 verbatim)                                          |
| `packages/contracts/src/remoteUpdate.test.ts` (new)                                                                                                             | Decode tests incl. Rust wire-shape parity samples                                                |
| `packages/contracts/src/environment.ts`                                                                                                                         | `remoteUpdateControl` capability + embedded `remoteUpdateSupport`                                |
| `packages/contracts/src/rpc.ts`                                                                                                                                 | `WS_METHODS` entries + three `Rpc.make` defs + `WsRpcGroup`                                      |
| `packages/contracts/fixtures/rpc-wire/*`                                                                                                                        | Regenerated manifest + typed-failure fixtures (generated)                                        |
| `apps/server/src/remote_update.rs` (new)                                                                                                                        | Rust contract mirror, `RemoteUpdateDelegate` trait, `RemoteUpdateService`, manual-required error |
| `apps/server/src/production/remote_update_rpc.rs` (new)                                                                                                         | `register_remote_update_rpc`                                                                     |
| `apps/server/src/rpc/methods.rs`, `apps/server/src/auth/scope.rs`                                                                                               | Method inventory + scope declarations                                                            |
| `apps/server/src/config.rs`                                                                                                                                     | `remote_update_support` field + builder                                                          |
| `apps/server/src/http.rs`, `apps/server/src/production/control.rs`, `apps/server/src/lifecycle.rs` (Connect descriptor)                                         | All three descriptor producers                                                                   |
| `apps/server/src/lifecycle.rs`, `apps/server/src/production/runtime.rs`                                                                                         | Delegate threading into the registry                                                             |
| `apps/server/tests/remote_update_rpc.rs` (new)                                                                                                                  | End-to-end WS tests (manual + delegate paths)                                                    |
| `apps/desktop/src-tauri/src/remote_update_delegate.rs` (new)                                                                                                    | Desktop delegate, support derivation, state mapping                                              |
| `apps/desktop/src-tauri/src/backend.rs`, `lib.rs`                                                                                                               | Delegate installation + `start_with_desktop_integration` call                                    |
| `packages/client-runtime/src/state/remoteUpdates.ts` (new)                                                                                                      | Snapshot/check/install atoms + max-2 fan-out helper                                              |
| `packages/client-runtime/src/state/remoteUpdates.test.ts` (new)                                                                                                 | Fan-out concurrency + failure-isolation tests                                                    |
| `apps/web/src/state/remoteUpdates.ts` (new)                                                                                                                     | App-level atom instantiation                                                                     |
| `apps/web/src/components/settings/ServerUpdateBadge.tsx` (new)                                                                                                  | Badge + manual-instructions copy + check-all hook                                                |
| `apps/web/src/components/settings/ServerUpdateBadge.test.tsx` (new)                                                                                             | Badge/copy logic tests                                                                           |
| Settings Connect tab + `EnvironmentContextCard` props + `environmentRail.logic.ts` `updateAvailable` + `environmentCompat.ts` selector (Phase 4/6 deliverables) | Interface-level wiring incl. rail amber dot (Task 8)                                             |
| `docs/architecture/overview.md`, `docs/architecture/remote.md`, `docs/testing/*-desktop.md`                                                                     | Living docs + runbooks (Task 9)                                                                  |

---

### Task 1: Remote update contract schemas

**Files:**

- Create: `packages/contracts/src/remoteUpdate.ts`
- Create: `packages/contracts/src/remoteUpdate.test.ts`
- Modify: `packages/contracts/src/index.ts` (add one export line)

**Interfaces:**

- Consumes: `TrimmedNonEmptyString` from `packages/contracts/src/baseSchemas.ts`.
- Produces (later tasks import these exact names from `@bibcode/contracts`):
  `RemoteUpdateInstallMode`, `RemoteUpdateSupportReason`, `RemoteUpdateSupport`,
  `RemoteUpdateState`, `RemoteUpdateSnapshot` (schemas + same-named types),
  `REMOTE_UPDATE_MANUAL_REQUIRED` (`"remote_update_manual_required"`), and
  `RemoteUpdateInstallError` (tagged error class, `_tag: "RemoteUpdateInstallError"`,
  field `code`).

- [x] **Step 1: Write the failing test**

Create `packages/contracts/src/remoteUpdate.test.ts`:

```ts
import { describe, expect, it } from "vite-plus/test";
import * as Schema from "effect/Schema";

import {
  REMOTE_UPDATE_MANUAL_REQUIRED,
  RemoteUpdateInstallError,
  RemoteUpdateSnapshot,
  RemoteUpdateSupport,
} from "./remoteUpdate.ts";

const decodeSnapshot = Schema.decodeUnknownSync(RemoteUpdateSnapshot);
const decodeSupport = Schema.decodeUnknownSync(RemoteUpdateSupport);
const decodeInstallError = Schema.decodeUnknownSync(RemoteUpdateInstallError);

describe("RemoteUpdateSnapshot", () => {
  it("decodes the desktop-hosted interactive shape", () => {
    const snapshot = decodeSnapshot({
      serverVersion: "0.4.2",
      latestVersion: "0.5.0",
      state: "update-available",
      error: null,
      support: { installMode: "interactive", reason: "available" },
    });
    expect(snapshot.latestVersion).toBe("0.5.0");
    expect(snapshot.support.installMode).toBe("interactive");
  });

  it("decodes the headless manual shape with a null latest version", () => {
    const snapshot = decodeSnapshot({
      serverVersion: "0.4.2",
      latestVersion: null,
      state: "idle",
      error: null,
      support: { installMode: "manual", reason: "manual-update-required" },
    });
    expect(snapshot.latestVersion).toBeNull();
    expect(snapshot.state).toBe("idle");
  });

  it("keeps the schema-reserved supervised mode decodable", () => {
    const support = decodeSupport({ installMode: "supervised", reason: "available" });
    expect(support.installMode).toBe("supervised");
  });

  it("rejects unknown states", () => {
    expect(() =>
      decodeSnapshot({
        serverVersion: "0.4.2",
        latestVersion: null,
        state: "rebooting",
        error: null,
        support: { installMode: "manual", reason: "manual-update-required" },
      }),
    ).toThrow();
  });
});

describe("RemoteUpdateInstallError", () => {
  it("decodes the exact Rust manual-required wire shape", () => {
    const error = decodeInstallError({
      _tag: "RemoteUpdateInstallError",
      code: "remote_update_manual_required",
    });
    expect(error.code).toBe(REMOTE_UPDATE_MANUAL_REQUIRED);
    expect(error.message.length).toBeGreaterThan(0);
  });
});
```

- [x] **Step 2: Run test to verify it fails**

Run: `vp test packages/contracts/src/remoteUpdate.test.ts`
Expected: FAIL — cannot resolve `./remoteUpdate.ts`.

- [x] **Step 3: Write minimal implementation**

Create `packages/contracts/src/remoteUpdate.ts` (spec §4.5 verbatim; `supervised` is
schema-reserved, v1 ships `interactive` and `manual` only):

```ts
import * as Schema from "effect/Schema";

import { TrimmedNonEmptyString } from "./baseSchemas.ts";

export const RemoteUpdateInstallMode = Schema.Literals([
  "interactive",
  "manual",
  // Schema-reserved (spec D10); no v1 implementation.
  "supervised",
]);
export type RemoteUpdateInstallMode = typeof RemoteUpdateInstallMode.Type;

export const RemoteUpdateSupportReason = Schema.Literals([
  "available",
  "manual-update-required",
  "unpackaged-build",
  "updater-unavailable",
]);
export type RemoteUpdateSupportReason = typeof RemoteUpdateSupportReason.Type;

export const RemoteUpdateSupport = Schema.Struct({
  installMode: RemoteUpdateInstallMode,
  reason: RemoteUpdateSupportReason,
});
export type RemoteUpdateSupport = typeof RemoteUpdateSupport.Type;

export const RemoteUpdateState = Schema.Literals([
  "idle",
  "checking",
  "update-available",
  "downloading",
  "installing",
  "up-to-date",
  "error",
]);
export type RemoteUpdateState = typeof RemoteUpdateState.Type;

export const RemoteUpdateSnapshot = Schema.Struct({
  serverVersion: TrimmedNonEmptyString,
  latestVersion: Schema.NullOr(TrimmedNonEmptyString),
  state: RemoteUpdateState,
  error: Schema.NullOr(Schema.String),
  support: RemoteUpdateSupport,
});
export type RemoteUpdateSnapshot = typeof RemoteUpdateSnapshot.Type;

export const REMOTE_UPDATE_MANUAL_REQUIRED = "remote_update_manual_required" as const;

export class RemoteUpdateInstallError extends Schema.TaggedErrorClass<RemoteUpdateInstallError>()(
  "RemoteUpdateInstallError",
  {
    code: Schema.Literal(REMOTE_UPDATE_MANUAL_REQUIRED),
  },
) {
  override get message(): string {
    return "This server cannot install updates remotely; update it manually on its host.";
  }
}
```

Add to `packages/contracts/src/index.ts`, directly after the `remoteAccess.ts` line:

```ts
export * from "./remoteUpdate.ts";
```

- [x] **Step 4: Run test to verify it passes**

Run: `vp test packages/contracts/src/remoteUpdate.test.ts`
Expected: PASS (5 tests).

- [x] **Step 5: Commit**

```bash
git add packages/contracts/src/remoteUpdate.ts packages/contracts/src/remoteUpdate.test.ts packages/contracts/src/index.ts
git commit -m "feat(contracts): add remote update contract (spec 4.5)"
```

---

### Task 2: Embed remote update support in the environment descriptor (TS)

**Files:**

- Modify: `packages/contracts/src/environment.ts`
- Modify: `packages/contracts/src/environment.test.ts`

**Interfaces:**

- Consumes: `RemoteUpdateSupport` from `./remoteUpdate.ts` (Task 1).
- Produces: `ExecutionEnvironmentCapabilities.remoteUpdateControl: boolean`
  (decode-default `false`) and
  `ExecutionEnvironmentDescriptor.remoteUpdateSupport: RemoteUpdateSupport | null`
  (decode-default `null`). Later tasks gate all UI on
  `capabilities.remoteUpdateControl === true`.

- [x] **Step 1: Write the failing test**

Append to `packages/contracts/src/environment.test.ts` (reuse the existing `descriptor`
fixture object and `decodeExecutionEnvironmentDescriptor` helper defined at the top of
that file):

```ts
describe("remote update descriptor surface", () => {
  it("defaults remoteUpdateControl to false and remoteUpdateSupport to null for older servers", () => {
    const decoded = decodeExecutionEnvironmentDescriptor({
      ...descriptor,
      capabilities: { repositoryIdentity: true },
    });
    expect(decoded.capabilities.remoteUpdateControl).toBe(false);
    expect(decoded.remoteUpdateSupport).toBeNull();
  });

  it("decodes an embedded remote update support block", () => {
    const decoded = decodeExecutionEnvironmentDescriptor({
      ...descriptor,
      capabilities: { repositoryIdentity: true, remoteUpdateControl: true },
      remoteUpdateSupport: { installMode: "manual", reason: "manual-update-required" },
    });
    expect(decoded.capabilities.remoteUpdateControl).toBe(true);
    expect(decoded.remoteUpdateSupport).toEqual({
      installMode: "manual",
      reason: "manual-update-required",
    });
  });
});
```

- [x] **Step 2: Run test to verify it fails**

Run: `vp test packages/contracts/src/environment.test.ts`
Expected: FAIL — `remoteUpdateControl` is not a known capability / `remoteUpdateSupport`
missing from the decoded descriptor.

- [x] **Step 3: Write minimal implementation**

In `packages/contracts/src/environment.ts`:

```ts
import { RemoteUpdateSupport } from "./remoteUpdate.ts";
```

Add to `ExecutionEnvironmentCapabilities` (after `activityProtocolVersion`):

```ts
  remoteUpdateControl: Schema.Boolean.pipe(Schema.withDecodingDefault(Effect.succeed(false))),
```

Add to `ExecutionEnvironmentDescriptor` (after `storageInstanceId`):

```ts
  remoteUpdateSupport: Schema.NullOr(RemoteUpdateSupport).pipe(
    Schema.withDecodingDefault(Effect.succeed(null)),
  ),
```

- [x] **Step 4: Run tests to verify they pass**

Run: `vp test packages/contracts/src/environment.test.ts packages/contracts/src/`
Expected: PASS, including all pre-existing descriptor tests (the additions are
decode-defaulted, so no existing fixture needs the new fields).

- [x] **Step 5: Commit**

```bash
git add packages/contracts/src/environment.ts packages/contracts/src/environment.test.ts
git commit -m "feat(contracts): embed remote update support in the environment descriptor"
```

---

### Task 3: Rust contract mirror, delegate trait, and RemoteUpdateService

**Files:**

- Create: `apps/server/src/remote_update.rs`
- Modify: `apps/server/src/lib.rs` (declare `pub mod remote_update;` and re-export the
  public types alongside the existing re-exports)
- Modify: `apps/server/src/config.rs` (`remote_update_support` field + builder + test)

**Interfaces:**

- Consumes: nothing new.
- Produces (exact names later tasks use):
  - `bibcode_server::remote_update::{RemoteUpdateInstallMode, RemoteUpdateSupportReason,
    RemoteUpdateSupport, RemoteUpdateState, RemoteUpdateSnapshot, HostUpdaterStatus,
    HostUpdaterFuture, RemoteUpdateDelegate, RemoteUpdateService,
    remote_update_manual_required_error}`
  - `RemoteUpdateService::new(server_version: String, support: RemoteUpdateSupport,
    delegate: Option<Arc<dyn RemoteUpdateDelegate>>) -> RemoteUpdateService` with
    `pub async fn status(&self) -> RemoteUpdateSnapshot`,
    `pub async fn check(&self) -> RemoteUpdateSnapshot`,
    `pub async fn install(&self) -> Result<RemoteUpdateSnapshot, serde_json::Value>`.
  - `ServerConfig.remote_update_support: RemoteUpdateSupport` (default
    `manual` / `manual-update-required`) and
    `ServerConfig::with_remote_update_support(self, support) -> Self`.

- [x] **Step 1: Write the failing tests**

Create `apps/server/src/remote_update.rs` with the tests module first (the module body
comes in Step 3; writing tests against the not-yet-written API makes `cargo test` fail
to compile, which is this step's red):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    fn manual_support() -> RemoteUpdateSupport {
        RemoteUpdateSupport {
            install_mode: RemoteUpdateInstallMode::Manual,
            reason: RemoteUpdateSupportReason::ManualUpdateRequired,
        }
    }

    fn interactive_support() -> RemoteUpdateSupport {
        RemoteUpdateSupport {
            install_mode: RemoteUpdateInstallMode::Interactive,
            reason: RemoteUpdateSupportReason::Available,
        }
    }

    struct FixtureDelegate;

    impl RemoteUpdateDelegate for FixtureDelegate {
        fn status(&self) -> HostUpdaterFuture {
            Box::pin(async {
                HostUpdaterStatus {
                    latest_version: Some("9.9.9".to_owned()),
                    state: RemoteUpdateState::UpdateAvailable,
                    error: None,
                }
            })
        }

        fn check(&self) -> HostUpdaterFuture {
            self.status()
        }

        fn request_install(&self) -> HostUpdaterFuture {
            Box::pin(async {
                HostUpdaterStatus {
                    latest_version: Some("9.9.9".to_owned()),
                    state: RemoteUpdateState::Installing,
                    error: None,
                }
            })
        }
    }

    #[test]
    fn snapshot_serializes_to_the_exact_contract_wire_shape() {
        let snapshot = RemoteUpdateSnapshot {
            server_version: "0.4.2".to_owned(),
            latest_version: None,
            state: RemoteUpdateState::Idle,
            error: None,
            support: manual_support(),
        };
        assert_eq!(
            serde_json::to_value(&snapshot).expect("snapshot serializes"),
            json!({
                "serverVersion": "0.4.2",
                "latestVersion": null,
                "state": "idle",
                "error": null,
                "support": { "installMode": "manual", "reason": "manual-update-required" }
            })
        );
    }

    #[test]
    fn manual_required_error_matches_the_typescript_tagged_error() {
        assert_eq!(
            remote_update_manual_required_error(),
            json!({
                "_tag": "RemoteUpdateInstallError",
                "code": "remote_update_manual_required"
            })
        );
    }

    #[tokio::test]
    async fn manual_service_reports_idle_null_latest_and_refuses_install() {
        let service = RemoteUpdateService::new("0.4.2".to_owned(), manual_support(), None);
        let snapshot = service.check().await;
        assert_eq!(snapshot.server_version, "0.4.2");
        assert_eq!(snapshot.latest_version, None);
        assert_eq!(snapshot.state, RemoteUpdateState::Idle);
        assert_eq!(snapshot.support, manual_support());

        let error = service.install().await.expect_err("manual install must fail");
        assert_eq!(error, remote_update_manual_required_error());
    }

    #[tokio::test]
    async fn interactive_service_consults_the_delegate() {
        let service = RemoteUpdateService::new(
            "0.4.2".to_owned(),
            interactive_support(),
            Some(Arc::new(FixtureDelegate)),
        );
        let checked = service.check().await;
        assert_eq!(checked.latest_version.as_deref(), Some("9.9.9"));
        assert_eq!(checked.state, RemoteUpdateState::UpdateAvailable);

        let installing = service.install().await.expect("interactive install accepted");
        assert_eq!(installing.state, RemoteUpdateState::Installing);
    }

    #[tokio::test]
    async fn interactive_support_without_a_delegate_degrades_to_manual_behavior() {
        // Defensive: never panic if wiring forgot the delegate.
        let service = RemoteUpdateService::new("0.4.2".to_owned(), interactive_support(), None);
        assert_eq!(service.status().await.state, RemoteUpdateState::Idle);
        assert!(service.install().await.is_err());
    }
}
```

Also append to the tests module in `apps/server/src/config.rs`:

```rust
    #[test]
    fn server_config_defaults_to_manual_remote_update_support() {
        let config = ServerConfig::new("/tmp/bibcode-test");
        assert_eq!(
            config.remote_update_support,
            crate::remote_update::RemoteUpdateSupport {
                install_mode: crate::remote_update::RemoteUpdateInstallMode::Manual,
                reason: crate::remote_update::RemoteUpdateSupportReason::ManualUpdateRequired,
            }
        );
    }
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test -p bibcode-server remote_update`
Expected: COMPILE ERROR (types not defined) — that is the red state.

- [x] **Step 3: Write minimal implementation**

Top of `apps/server/src/remote_update.rs` (above the tests module):

```rust
//! Remote server update contract mirror and host-updater seam (spec section 4.5).
//!
//! The TypeScript source of truth is `packages/contracts/src/remoteUpdate.ts`;
//! serde attributes here must keep the wire shapes byte-identical.

use std::{future::Future, pin::Pin, sync::Arc};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RemoteUpdateInstallMode {
    Interactive,
    Manual,
    /// Schema-reserved (spec D10); no v1 implementation.
    Supervised,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RemoteUpdateSupportReason {
    Available,
    ManualUpdateRequired,
    UnpackagedBuild,
    UpdaterUnavailable,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteUpdateSupport {
    pub install_mode: RemoteUpdateInstallMode,
    pub reason: RemoteUpdateSupportReason,
}

impl RemoteUpdateSupport {
    #[must_use]
    pub const fn manual() -> Self {
        Self {
            install_mode: RemoteUpdateInstallMode::Manual,
            reason: RemoteUpdateSupportReason::ManualUpdateRequired,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RemoteUpdateState {
    Idle,
    Checking,
    UpdateAvailable,
    Downloading,
    Installing,
    UpToDate,
    Error,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteUpdateSnapshot {
    pub server_version: String,
    pub latest_version: Option<String>,
    pub state: RemoteUpdateState,
    pub error: Option<String>,
    pub support: RemoteUpdateSupport,
}

/// What the hosting process's updater knows; the service adds `server_version`
/// and `support` to build the wire snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostUpdaterStatus {
    pub latest_version: Option<String>,
    pub state: RemoteUpdateState,
    pub error: Option<String>,
}

pub type HostUpdaterFuture = Pin<Box<dyn Future<Output = HostUpdaterStatus> + Send>>;

/// Implemented by the desktop host (pattern: `DesktopUiProcessObserver`).
/// Consulted only when `RemoteUpdateSupport.install_mode` is `Interactive`.
pub trait RemoteUpdateDelegate: Send + Sync + 'static {
    fn status(&self) -> HostUpdaterFuture;
    fn check(&self) -> HostUpdaterFuture;
    /// Starts (or joins) the host install flow and returns the current status;
    /// callers poll `status` for progress. Install failures ride in
    /// `HostUpdaterStatus { state: Error, error: Some(..) }`.
    fn request_install(&self) -> HostUpdaterFuture;
}

/// Wire error for `updater.install` on servers that cannot install remotely.
/// Must stay byte-identical to `RemoteUpdateInstallError` in
/// `packages/contracts/src/remoteUpdate.ts`.
#[must_use]
pub fn remote_update_manual_required_error() -> Value {
    json!({
        "_tag": "RemoteUpdateInstallError",
        "code": "remote_update_manual_required",
    })
}

#[derive(Clone)]
pub struct RemoteUpdateService {
    server_version: String,
    support: RemoteUpdateSupport,
    delegate: Option<Arc<dyn RemoteUpdateDelegate>>,
}

impl RemoteUpdateService {
    #[must_use]
    pub fn new(
        server_version: String,
        support: RemoteUpdateSupport,
        delegate: Option<Arc<dyn RemoteUpdateDelegate>>,
    ) -> Self {
        Self {
            server_version,
            support,
            delegate,
        }
    }

    fn interactive_delegate(&self) -> Option<&Arc<dyn RemoteUpdateDelegate>> {
        if self.support.install_mode == RemoteUpdateInstallMode::Interactive {
            self.delegate.as_ref()
        } else {
            None
        }
    }

    fn manual_status() -> HostUpdaterStatus {
        HostUpdaterStatus {
            latest_version: None,
            state: RemoteUpdateState::Idle,
            error: None,
        }
    }

    fn snapshot(&self, status: HostUpdaterStatus) -> RemoteUpdateSnapshot {
        RemoteUpdateSnapshot {
            server_version: self.server_version.clone(),
            latest_version: status.latest_version,
            state: status.state,
            error: status.error,
            support: self.support,
        }
    }

    pub async fn status(&self) -> RemoteUpdateSnapshot {
        let status = match self.interactive_delegate() {
            Some(delegate) => delegate.status().await,
            None => Self::manual_status(),
        };
        self.snapshot(status)
    }

    pub async fn check(&self) -> RemoteUpdateSnapshot {
        let status = match self.interactive_delegate() {
            Some(delegate) => delegate.check().await,
            None => Self::manual_status(),
        };
        self.snapshot(status)
    }

    pub async fn install(&self) -> Result<RemoteUpdateSnapshot, Value> {
        match self.interactive_delegate() {
            Some(delegate) => Ok(self.snapshot(delegate.request_install().await)),
            None => Err(remote_update_manual_required_error()),
        }
    }
}
```

In `apps/server/src/lib.rs`, next to the existing module declarations add
`pub mod remote_update;` (follow the file's existing ordering/re-export style; the
public path `bibcode_server::remote_update::...` is what the desktop crate imports).

In `apps/server/src/config.rs`:

```rust
use crate::remote_update::RemoteUpdateSupport;
```

Add to `ServerConfig` (after `storage_instance_id`):

```rust
    /// How this server can be updated remotely (spec section 4.5). Headless
    /// default is manual; the desktop host overrides at launch.
    pub remote_update_support: RemoteUpdateSupport,
```

In `ServerConfig::new`, initialize with:

```rust
            remote_update_support: RemoteUpdateSupport::manual(),
```

And add the builder next to `with_bind`:

```rust
    #[must_use]
    pub fn with_remote_update_support(mut self, support: RemoteUpdateSupport) -> Self {
        self.remote_update_support = support;
        self
    }
```

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test -p bibcode-server remote_update && cargo test -p bibcode-server config`
Expected: PASS (5 new remote_update tests + config default test).

- [x] **Step 5: Format, lint, commit**

```bash
cargo fmt --all
cargo clippy -p bibcode-server --all-targets -- -D warnings
git add apps/server/src/remote_update.rs apps/server/src/lib.rs apps/server/src/config.rs
git commit -m "feat(server): add remote update contract mirror, delegate seam, and service"
```

---

### Task 4: Serialize the support block from ALL THREE descriptor producers

Amended spec §4.5: the well-known route, `server.getConfig`, **and** the Connect/relay
descriptor all publish `remoteUpdateSupport` + `remoteUpdateControl` — editing fewer is
a latent bug.

**Files:**

- Modify: `apps/server/src/production/control.rs` (`environment_descriptor`, ~line 2139,
  plus its tests around line 4996)
- Modify: `apps/server/src/http.rs` (`EnvironmentDescriptor` / `EnvironmentCapabilities`
  structs, ~lines 260–300)
- Modify: `apps/server/src/lifecycle.rs` (inline Connect descriptor `json!` block
  ~line 270 — extract to a testable helper — plus a unit test in its tests module)

**Interfaces:**

- Consumes: `ServerConfig.remote_update_support` (Task 3).
- Produces: `server.getConfig`'s `environment`, `/.well-known/bibcode/environment`, and
  the Connect/relay descriptor all carry `"remoteUpdateSupport": {...}` and
  `"capabilities": { ..., "remoteUpdateControl": true }`; plus
  `connect_environment_descriptor(config: &ServerConfig) -> serde_json::Value` in
  `lifecycle.rs`. Task 5's integration test asserts the well-known route; this task's
  unit tests pin `server.getConfig` and the Connect descriptor.

- [x] **Step 1: Write the failing test**

Append to the tests module of `apps/server/src/production/control.rs`, modeled on the
neighboring `environment_descriptor_advertises_complete_worktree_catalog_surface` test
(reuse the same config fixture that test uses — it already sets `storage_instance_id`):

```rust
    #[test]
    fn environment_descriptor_advertises_remote_update_control_and_support() {
        let config = descriptor_test_config(); // reuse/extract the fixture used by the
                                               // worktree-catalog descriptor test
        let descriptor = environment_descriptor(&config, false);
        assert_eq!(descriptor["capabilities"]["remoteUpdateControl"], true);
        assert_eq!(
            descriptor["remoteUpdateSupport"],
            serde_json::json!({ "installMode": "manual", "reason": "manual-update-required" })
        );
    }
```

If the existing test builds its config inline rather than via a helper, extract a
`fn descriptor_test_config() -> ServerConfig` helper in the tests module and use it from
both tests (do not duplicate the fixture — the neighboring
`environment_descriptor_advertises_complete_worktree_catalog_surface` test uses
`running_test_config(temp.path())`; reuse that if it exists).

Also append to the tests module of `apps/server/src/lifecycle.rs` (the Connect
descriptor producer):

```rust
    #[test]
    fn connect_descriptor_advertises_remote_update_support() {
        let mut config = ServerConfig::new("/tmp/bibcode-connect-descriptor-test");
        config.storage_instance_id = Some(
            crate::persistence::StorageInstanceId::from_uuid(uuid::Uuid::nil()),
        );
        let descriptor = connect_environment_descriptor(&config);
        assert_eq!(descriptor["capabilities"]["repositoryIdentity"], true);
        assert_eq!(descriptor["capabilities"]["remoteUpdateControl"], true);
        assert_eq!(
            descriptor["remoteUpdateSupport"],
            serde_json::json!({ "installMode": "manual", "reason": "manual-update-required" })
        );
    }
```

- [x] **Step 2: Run tests to verify they fail**

Run: `cargo test -p bibcode-server environment_descriptor_advertises_remote_update`
Expected: FAIL — `remoteUpdateControl` is `Value::Null`.

Run: `cargo test -p bibcode-server connect_descriptor_advertises_remote_update`
Expected: COMPILE ERROR — `connect_environment_descriptor` does not exist yet.

- [x] **Step 3: Write minimal implementation (all three producers)**

`apps/server/src/production/control.rs`, in `environment_descriptor`:

```rust
        "remoteUpdateSupport": config.remote_update_support,
```

(placed after `"storageInstanceId"`), and inside the `"capabilities"` object:

```rust
            "remoteUpdateControl": true,
```

`apps/server/src/http.rs` — extend the serialized structs and the handler:

```rust
use crate::remote_update::RemoteUpdateSupport;
```

```rust
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EnvironmentDescriptor {
    environment_id: String,
    label: String,
    platform: PlatformDescriptor,
    server_version: String,
    storage_instance_id: String,
    remote_update_support: RemoteUpdateSupport,
    capabilities: EnvironmentCapabilities,
}
```

```rust
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EnvironmentCapabilities {
    repository_identity: bool,
    remote_update_control: bool,
}
```

And in `environment_descriptor` (the Axum handler):

```rust
        remote_update_support: config.remote_update_support,
        capabilities: EnvironmentCapabilities {
            repository_identity: true,
            remote_update_control: true,
        },
```

`apps/server/src/lifecycle.rs` — extract the inline Connect descriptor into a helper so
it becomes testable, and add the two fields. Replace the `let descriptor =
serde_json::json!({ ... });` block (~line 270) with a call to:

```rust
fn connect_environment_descriptor(config: &ServerConfig) -> serde_json::Value {
    serde_json::json!({
        "environmentId": config.environment_id,
        "label": config.environment_label,
        "platform": { "os": std::env::consts::OS, "arch": std::env::consts::ARCH },
        "serverVersion": config.server_version,
        "storageInstanceId": config
            .storage_instance_id
            .expect("a running server has a prepared persistent store")
            .to_string(),
        "remoteUpdateSupport": config.remote_update_support,
        "capabilities": { "repositoryIdentity": true, "remoteUpdateControl": true },
    })
}
```

and at the original site:

```rust
                let descriptor = connect_environment_descriptor(&config);
```

(If Phase 2 already added protocol-window fields to this block, carry them into the
helper unchanged.)

Note: if Phase 2 already added `remoteProtocolVersion`/`minCompatibleRemoteProtocol` to
the http.rs/control.rs structs, keep those fields — this task only adds the two update
fields.

- [x] **Step 4: Run tests to verify they pass**

Run: `cargo test -p bibcode-server environment_descriptor && cargo test -p bibcode-server connect_descriptor && cargo test -p bibcode-server http`
Expected: PASS (new tests green; existing descriptor tests untouched).

- [x] **Step 5: Format, lint, commit**

```bash
cargo fmt --all
cargo clippy -p bibcode-server --all-targets -- -D warnings
git add apps/server/src/production/control.rs apps/server/src/http.rs apps/server/src/lifecycle.rs
git commit -m "feat(server): advertise remote update support from all three descriptor producers"
```

---

### Task 5: Wire the updater RPC surface end-to-end (TS methods + Rust inventory, scopes, registration)

This task is deliberately one unit: `apps/server/src/production/runtime.rs:114` calls
`registry.validate_complete()` at startup and `apps/server/tests/rpc_wire.rs` compares
`ACTIVE_RPC_METHODS` against the generated manifest — so the TS method definitions,
regenerated fixtures, Rust inventory, scopes, and handler registration must land
together to stay green.

**Files:**

- Modify: `packages/contracts/src/rpc.ts` (`WS_METHODS` ~line 304; `Rpc.make` defs after
  the server-meta block ~line 520; `WsRpcGroup` ~line 1258)
- Regenerate: `packages/contracts/fixtures/rpc-wire/` (manifest + new
  `typed-failures/updater__*.json` files — generated, must be committed)
- Modify: `apps/server/src/rpc/methods.rs` (`ACTIVE_RPC_METHODS`)
- Modify: `apps/server/src/auth/scope.rs` (`required_scope` + tests)
- Create: `apps/server/src/production/remote_update_rpc.rs`
- Modify: `apps/server/src/production/mod.rs` (declare the module)
- Modify: `apps/server/src/production/runtime.rs` (construct the service, register)
- Modify: `apps/server/tests/rpc_wire.rs` (pinned counts)
- Create: `apps/server/tests/remote_update_rpc.rs`

**Interfaces:**

- Consumes: `RemoteUpdateSnapshot`, `RemoteUpdateInstallError` (Task 1);
  `RemoteUpdateService` (Task 3); `ServerConfig.remote_update_support` (Task 3).
- Produces: wire methods `"updater.status"`, `"updater.check"`, `"updater.install"`
  (`WS_METHODS.updaterStatus/updaterCheck/updaterInstall`;
  `WsUpdaterStatusRpc`/`WsUpdaterCheckRpc`/`WsUpdaterInstallRpc`);
  `register_remote_update_rpc(registry: &mut RpcRegistry, service: RemoteUpdateService)`.
  In this task the production runtime constructs the service with `delegate: None`
  (headless/manual); Task 6 threads the desktop delegate.

- [x] **Step 1: Write the failing integration test**

Create `apps/server/tests/remote_update_rpc.rs`:

```rust
use bibcode_server::{RpcExit, ServerConfig, ServerMessage, ServerRuntime};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio_tungstenite::{connect_async, tungstenite::Message};

fn disable_provider_processes(root: &std::path::Path) {
    let settings = root.join("userdata/settings.json");
    std::fs::create_dir_all(settings.parent().expect("settings parent"))
        .expect("settings directory");
    std::fs::write(
        settings,
        serde_json::to_vec(&json!({
            "providers": {
                "codex": {"enabled": false},
                "claudeAgent": {"enabled": false},
                "cursor": {"enabled": false},
                "grok": {"enabled": false},
                "opencode": {"enabled": false}
            }
        }))
        .expect("settings JSON"),
    )
    .expect("settings fixture");
}

type WsStream = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

async fn call_unary(socket: &mut WsStream, id: &str, method: &str) -> ServerMessage {
    let request = json!({
        "_tag": "Request",
        "id": id,
        "tag": method,
        "payload": {},
        "headers": []
    });
    socket
        .send(Message::Text(request.to_string().into()))
        .await
        .expect("request sends");
    loop {
        let message = socket
            .next()
            .await
            .expect("socket yields")
            .expect("frame decodes");
        if let Message::Text(text) = message {
            let decoded: ServerMessage =
                serde_json::from_str(&text).expect("server message decodes");
            if matches!(decoded, ServerMessage::Exit { .. }) {
                return decoded;
            }
        }
    }
}

#[tokio::test]
async fn headless_server_answers_manual_update_surface() {
    let temp = TempDir::new().expect("data root");
    disable_provider_processes(temp.path());
    let config = ServerConfig::new(temp.path())
        .with_bind("127.0.0.1", 0)
        .with_unsafe_no_auth();
    let handle = ServerRuntime::start(config).await.expect("server starts");

    // Descriptor advertises the surface before any RPC (covers apps/server/src/http.rs).
    let descriptor: Value = reqwest::get(format!(
        "http://{}/.well-known/bibcode/environment",
        handle.local_addr()
    ))
    .await
    .expect("descriptor fetch")
    .json()
    .await
    .expect("descriptor JSON");
    assert_eq!(descriptor["capabilities"]["remoteUpdateControl"], true);
    assert_eq!(
        descriptor["remoteUpdateSupport"],
        json!({ "installMode": "manual", "reason": "manual-update-required" })
    );

    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket connects");

    let ServerMessage::Exit {
        exit: RpcExit::Success { value: Some(status) },
        ..
    } = call_unary(&mut socket, "1", "updater.status").await
    else {
        panic!("updater.status must succeed");
    };
    assert_eq!(status["state"], "idle");
    assert_eq!(status["latestVersion"], Value::Null);
    assert_eq!(status["support"]["installMode"], "manual");
    assert_eq!(status["serverVersion"], env!("CARGO_PKG_VERSION"));

    let checked = call_unary(&mut socket, "2", "updater.check").await;
    assert!(matches!(
        checked,
        ServerMessage::Exit {
            exit: RpcExit::Success { value: Some(ref value) },
            ..
        } if value["state"] == "idle" && value["latestVersion"] == Value::Null
    ));

    let install = call_unary(&mut socket, "3", "updater.install").await;
    let ServerMessage::Exit { exit, .. } = install else {
        panic!("expected exit");
    };
    let failure = serde_json::to_value(&exit).expect("exit serializes");
    let failure_text = failure.to_string();
    assert!(
        failure_text.contains("RemoteUpdateInstallError")
            && failure_text.contains("remote_update_manual_required"),
        "manual install must fail with the typed error, got {failure_text}"
    );

    socket.close(None).await.expect("close socket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}
```

(Reuse the exact send/receive idioms from `apps/server/tests/rpc_wire.rs` and
`apps/server/tests/production_maintenance.rs` if the helper above needs adjusting to the
current `ServerMessage` API — those two files are the authoritative harness examples.
`reqwest` and `tempfile` are already dev-dependencies of `apps/server`; verify in
`apps/server/Cargo.toml` and add to `[dev-dependencies]` only if missing.)

- [x] **Step 2: Run test to verify it fails**

Run: `cargo test -p bibcode-server --test remote_update_rpc`
Expected: FAIL — `updater.status` is not a registered method (the request errors or the
scope map returns none).

- [x] **Step 3: Add the TS method definitions**

In `packages/contracts/src/rpc.ts`:

1. Import the contract next to the other `./` imports:

```ts
import { RemoteUpdateInstallError, RemoteUpdateSnapshot } from "./remoteUpdate.ts";
```

2. Add to `WS_METHODS` after the "Server meta" block:

```ts
  // Remote updater methods
  updaterStatus: "updater.status",
  updaterCheck: "updater.check",
  updaterInstall: "updater.install",
```

3. Add the three RPC definitions after `WsServerConsumeCodexRateLimitResetRpc`:

```ts
export const WsUpdaterStatusRpc = Rpc.make(WS_METHODS.updaterStatus, {
  payload: Schema.Struct({}),
  success: RemoteUpdateSnapshot,
  error: EnvironmentRpcError,
});

export const WsUpdaterCheckRpc = Rpc.make(WS_METHODS.updaterCheck, {
  payload: Schema.Struct({}),
  success: RemoteUpdateSnapshot,
  error: EnvironmentRpcError,
});

export const WsUpdaterInstallRpc = Rpc.make(WS_METHODS.updaterInstall, {
  payload: Schema.Struct({}),
  success: RemoteUpdateSnapshot,
  error: Schema.Union([RemoteUpdateInstallError, EnvironmentRpcError]),
});
```

4. Add the three to `RpcGroup.make(...)` (`WsRpcGroup`), after
   `WsServerConsumeCodexRateLimitResetRpc`:

```ts
  WsUpdaterStatusRpc,
  WsUpdaterCheckRpc,
  WsUpdaterInstallRpc,
```

- [x] **Step 4: Regenerate the TS↔Rust wire fixtures**

```bash
cd packages/contracts && node scripts/export-rust-rpc-fixtures.ts && cd ../..
git status --short packages/contracts/fixtures
```

Expected: `fixtures/rpc-wire/manifest.json` modified; new
`fixtures/rpc-wire/typed-failures/updater__status-*.json`,
`updater__check-*.json`, `updater__install-*.json` files appear. All generated files are
part of this commit — none may be left untracked.

Then run the contracts suite: `vp test packages/contracts/src/`
Expected: PASS (`rpcRustParity.test.ts` reads the regenerated manifest).

- [x] **Step 5: Add the Rust method inventory + scopes**

`apps/server/src/rpc/methods.rs` — `ACTIVE_RPC_METHODS` is name-sorted and the
`rpc_wire` test compares it _ordered_ against the generated manifest. Insert between
`mutation_unary("terminal.write"),` and `mutation_unary("vcs.clone"),`:

```rust
    mutation_unary("updater.check"),
    mutation_unary("updater.install"),
    read_unary("updater.status"),
```

(Confirm this ordering matches the regenerated `manifest.json`; the manifest is the
authority.)

`apps/server/src/auth/scope.rs` — add to `required_scope` (decision 1 in the pinned
phase decisions): add `"updater.status"` to the existing `SCOPE_ORCHESTRATION_READ`
match arm (the list containing `"server.getConfig"`), and add
`"updater.check" | "updater.install"` to the `SCOPE_ORCHESTRATION_OPERATE` arm (the
list containing `"server.updateSettings"`). Extend the scope test with:

```rust
        assert_eq!(
            required_scope("updater.status"),
            Some(SCOPE_ORCHESTRATION_READ)
        );
        for method in ["updater.check", "updater.install"] {
            assert_eq!(
                required_scope(method),
                Some(SCOPE_ORCHESTRATION_OPERATE),
                "wrong updater scope for {method}"
            );
        }
```

Run: `cargo test -p bibcode-server scope`
Expected: PASS (`every_active_rpc_method_has_exactly_one_declared_scope` — this test
was the free red gate the moment the methods entered `ACTIVE_RPC_METHODS`).

- [x] **Step 6: Register the handlers in the production runtime**

Create `apps/server/src/production/remote_update_rpc.rs`:

```rust
//! Registers the `updater.*` RPC surface (spec section 4.5) over a
//! `RemoteUpdateService`.

use crate::{remote_update::RemoteUpdateService, rpc::RpcRegistry};

pub fn register_remote_update_rpc(registry: &mut RpcRegistry, service: RemoteUpdateService) {
    let status = service.clone();
    registry.register_unary("updater.status", move |_request, _cancellation| {
        let service = status.clone();
        async move {
            Ok(serde_json::to_value(service.status().await).expect("snapshot serializes"))
        }
    });

    let check = service.clone();
    registry.register_unary("updater.check", move |_request, _cancellation| {
        let service = check.clone();
        async move {
            Ok(serde_json::to_value(service.check().await).expect("snapshot serializes"))
        }
    });

    registry.register_unary("updater.install", move |_request, _cancellation| {
        let service = service.clone();
        async move {
            service
                .install()
                .await
                .map(|snapshot| serde_json::to_value(snapshot).expect("snapshot serializes"))
        }
    });
}
```

Declare it in `apps/server/src/production/mod.rs` (`pub mod remote_update_rpc;`,
matching the file's existing style).

In `apps/server/src/production/runtime.rs`, next to the existing
`register_server_terminal_rpc(&mut registry, terminal_services.clone());` call
(~line 394), construct and register the service from the config the runtime already
receives (delegate threading arrives in Task 6; this task passes `None`):

```rust
        crate::production::remote_update_rpc::register_remote_update_rpc(
            &mut registry,
            crate::remote_update::RemoteUpdateService::new(
                config.server_version.clone(),
                config.remote_update_support,
                remote_update_delegate.clone(),
            ),
        );
```

For this task, define `let remote_update_delegate: Option<
std::sync::Arc<dyn crate::remote_update::RemoteUpdateDelegate>> = None;` immediately
above the call (Task 6 replaces this local with a threaded parameter — the variable
name is the seam).

- [x] **Step 7: Update the pinned counts in `rpc_wire.rs`**

In `apps/server/tests/rpc_wire.rs`, `rust_registry_matches_the_active_typescript_rpc_group`
pins literals. Update:

```rust
    assert_eq!(rust_methods.len(), 100);
```

(97 + 3 unary methods; the stream count stays 18.) The remaining pinned literals
(`typed_failure_fixtures.len()`, shape counts) must be synced to the values in the
**regenerated** `packages/contracts/fixtures/rpc-wire/manifest.json` — read them from
the manifest, do not guess. The pins exist to force conscious review of generated-fixture
drift; syncing them here _is_ that review.

- [x] **Step 8: Run tests to verify everything passes**

```bash
cargo test -p bibcode-server --test rpc_wire
cargo test -p bibcode-server --test remote_update_rpc
cargo test -p bibcode-server scope
vp test packages/contracts/src/
```

Expected: all PASS. The integration test now sees `updater.status` → manual snapshot,
`updater.check` → manual snapshot, `updater.install` → `RemoteUpdateInstallError`.

- [x] **Step 9: Format, lint, typecheck, commit**

```bash
cargo fmt --all
cargo clippy -p bibcode-server --all-targets -- -D warnings
vp run typecheck
git add packages/contracts/src/rpc.ts packages/contracts/fixtures \
  apps/server/src/rpc/methods.rs apps/server/src/auth/scope.rs \
  apps/server/src/production/remote_update_rpc.rs apps/server/src/production/mod.rs \
  apps/server/src/production/runtime.rs apps/server/tests/rpc_wire.rs \
  apps/server/tests/remote_update_rpc.rs
git commit -m "feat(rpc): ship updater.status/check/install with scopes and parity fixtures"
```

---

### Task 6: Desktop delegate — interactive install through the host updater

**Files:**

- Modify: `apps/server/src/lifecycle.rs` (`start_with_desktop_integration`, thread the
  delegate through `start_internal`)
- Modify: `apps/server/src/production/runtime.rs` (accept the delegate parameter;
  replace Task 5's `None` local)
- Modify: `apps/server/tests/remote_update_rpc.rs` (add the delegate-path test)
- Create: `apps/desktop/src-tauri/src/remote_update_delegate.rs`
- Modify: `apps/desktop/src-tauri/src/lib.rs` (declare module; install the delegate in
  the setup closure that already spawns `updates::run_background_update_checks`)
- Modify: `apps/desktop/src-tauri/src/backend.rs` (store the installed delegate +
  support, mirroring the existing `install_ui_process_observer` slot; use
  `start_with_desktop_integration` for the InProcess primary backend and set
  `server_config.remote_update_support`)

**Interfaces:**

- Consumes: `RemoteUpdateDelegate`, `HostUpdaterStatus`, `HostUpdaterFuture`,
  `RemoteUpdateState`, `RemoteUpdateSupport`, `RemoteUpdateInstallMode`,
  `RemoteUpdateSupportReason` from `bibcode_server::remote_update` (Task 3);
  `DesktopUpdateManager` (`state`/`check_for_update`/`download_update`/`install_update`)
  and `DesktopUpdateInstallInput` from `apps/desktop/src-tauri/src/updates.rs`;
  `BackendSupervisor` from `apps/desktop/src-tauri/src/backend.rs`.
- Produces:
  - `ServerRuntime::start_with_desktop_integration(config, ui_process_observer,
    update_delegate: Arc<dyn RemoteUpdateDelegate>) -> Result<ServerHandle, ServerError>`
  - `DesktopRemoteUpdateDelegate::<R>::new(app: AppHandle<R>) -> Arc<DesktopRemoteUpdateDelegate<R>>`
  - `derive_remote_update_support(updater_enabled: bool) -> RemoteUpdateSupport`
  - `map_desktop_update_state(state: &serde_json::Value) -> HostUpdaterStatus`
  - `BackendSupervisor::install_remote_update_integration(delegate, support)`

- [x] **Step 1: Write the failing server test (delegate is consulted end-to-end)**

Append to `apps/server/tests/remote_update_rpc.rs` (reusing that file's
`disable_provider_processes` and `call_unary` helpers):

```rust
use std::sync::Arc;

use bibcode_server::remote_update::{
    HostUpdaterFuture, HostUpdaterStatus, RemoteUpdateDelegate, RemoteUpdateInstallMode,
    RemoteUpdateState, RemoteUpdateSupport, RemoteUpdateSupportReason,
};

struct FixtureHostUpdater;

impl RemoteUpdateDelegate for FixtureHostUpdater {
    fn status(&self) -> HostUpdaterFuture {
        Box::pin(async {
            HostUpdaterStatus {
                latest_version: Some("9.9.9".to_owned()),
                state: RemoteUpdateState::UpdateAvailable,
                error: None,
            }
        })
    }

    fn check(&self) -> HostUpdaterFuture {
        self.status()
    }

    fn request_install(&self) -> HostUpdaterFuture {
        Box::pin(async {
            HostUpdaterStatus {
                latest_version: Some("9.9.9".to_owned()),
                state: RemoteUpdateState::Installing,
                error: None,
            }
        })
    }
}

#[tokio::test]
async fn desktop_integrated_server_routes_install_through_the_delegate() {
    let temp = TempDir::new().expect("data root");
    disable_provider_processes(temp.path());
    let config = ServerConfig::new(temp.path())
        .with_bind("127.0.0.1", 0)
        .with_unsafe_no_auth()
        .with_remote_update_support(RemoteUpdateSupport {
            install_mode: RemoteUpdateInstallMode::Interactive,
            reason: RemoteUpdateSupportReason::Available,
        });
    let handle = ServerRuntime::start_with_desktop_integration(
        config,
        std::sync::Arc::new(
            bibcode_server::diagnostics::UnavailableDesktopUiProcessObserver,
        ),
        Arc::new(FixtureHostUpdater),
    )
    .await
    .expect("server starts");

    let (mut socket, _) = connect_async(format!("ws://{}/ws", handle.local_addr()))
        .await
        .expect("WebSocket connects");

    let checked = call_unary(&mut socket, "1", "updater.check").await;
    assert!(matches!(
        checked,
        ServerMessage::Exit {
            exit: RpcExit::Success { value: Some(ref value) },
            ..
        } if value["latestVersion"] == "9.9.9" && value["state"] == "update-available"
    ));

    let install = call_unary(&mut socket, "2", "updater.install").await;
    assert!(matches!(
        install,
        ServerMessage::Exit {
            exit: RpcExit::Success { value: Some(ref value) },
            ..
        } if value["state"] == "installing"
    ));

    socket.close(None).await.expect("close socket");
    handle.shutdown();
    handle.join().await.expect("server joins");
}
```

- [x] **Step 2: Run test to verify it fails**

Run: `cargo test -p bibcode-server --test remote_update_rpc`
Expected: COMPILE ERROR — `start_with_desktop_integration` does not exist.

- [x] **Step 3: Thread the delegate through the server**

`apps/server/src/lifecycle.rs`:

```rust
use crate::remote_update::RemoteUpdateDelegate;
```

Add the public start variant next to `start_with_ui_process_observer`:

```rust
    pub async fn start_with_desktop_integration(
        config: ServerConfig,
        ui_process_observer: Arc<dyn DesktopUiProcessObserver>,
        update_delegate: Arc<dyn RemoteUpdateDelegate>,
    ) -> Result<ServerHandle, ServerError> {
        Self::start_internal(
            config,
            None,
            ui_process_observer,
            ProcessTreeCleanup::EmbeddedHost,
            Some(update_delegate),
        )
        .await
    }
```

Extend `start_internal` with a final parameter
`update_delegate: Option<Arc<dyn RemoteUpdateDelegate>>`; every existing start variant
passes `None` (compiler-driven — update all four callers in this file). Pass it into
`ProductionRuntime::start_with_process_tree_cleanup(...)` as a new trailing argument.

`apps/server/src/production/runtime.rs`: add the matching
`remote_update_delegate: Option<Arc<dyn crate::remote_update::RemoteUpdateDelegate>>`
parameter to `start_with_process_tree_cleanup` (and thread it through any intermediate
constructor to the registration site), delete Task 5's `let remote_update_delegate =
None;` local, and let the existing `register_remote_update_rpc` call consume the
parameter. Update any other callers of the changed constructor (tests included) with
`None` — let the compiler enumerate them.

- [x] **Step 4: Run the server test to verify it passes**

Run: `cargo test -p bibcode-server --test remote_update_rpc`
Expected: PASS (both the manual test from Task 5 and the new delegate test).

- [x] **Step 5: Write the failing desktop unit tests**

Create `apps/desktop/src-tauri/src/remote_update_delegate.rs` starting with its tests
module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn support_derivation_is_honest_about_the_updater() {
        if cfg!(debug_assertions) {
            let support = derive_remote_update_support(true);
            assert_eq!(support.install_mode, RemoteUpdateInstallMode::Manual);
            assert_eq!(support.reason, RemoteUpdateSupportReason::UnpackagedBuild);
        } else {
            let enabled = derive_remote_update_support(true);
            assert_eq!(enabled.install_mode, RemoteUpdateInstallMode::Interactive);
            assert_eq!(enabled.reason, RemoteUpdateSupportReason::Available);

            let disabled = derive_remote_update_support(false);
            assert_eq!(disabled.install_mode, RemoteUpdateInstallMode::Manual);
            assert_eq!(disabled.reason, RemoteUpdateSupportReason::UpdaterUnavailable);
        }
    }

    #[test]
    fn maps_every_desktop_updater_state_onto_the_wire_contract() {
        let cases = [
            (json!({"status": "idle", "phase": "idle"}), RemoteUpdateState::Idle),
            (json!({"status": "disabled", "phase": "idle"}), RemoteUpdateState::Idle),
            (json!({"status": "checking", "phase": "checking"}), RemoteUpdateState::Checking),
            (json!({"status": "up-to-date", "phase": "idle"}), RemoteUpdateState::UpToDate),
            (
                json!({"status": "available", "phase": "available", "availableVersion": "0.5.0"}),
                RemoteUpdateState::UpdateAvailable,
            ),
            (
                json!({"status": "downloading", "phase": "available", "availableVersion": "0.5.0"}),
                RemoteUpdateState::Downloading,
            ),
            (
                json!({"status": "downloaded", "phase": "available", "downloadedVersion": "0.5.0"}),
                RemoteUpdateState::UpdateAvailable,
            ),
            (
                json!({"status": "downloaded", "phase": "protecting", "downloadedVersion": "0.5.0"}),
                RemoteUpdateState::Installing,
            ),
            (
                json!({"status": "downloaded", "phase": "installing", "downloadedVersion": "0.5.0"}),
                RemoteUpdateState::Installing,
            ),
            (
                json!({"status": "error", "phase": "failed", "message": "boom"}),
                RemoteUpdateState::Error,
            ),
        ];
        for (state, expected) in cases {
            let mapped = map_desktop_update_state(&state);
            assert_eq!(mapped.state, expected, "for desktop state {state}");
        }

        let available = map_desktop_update_state(
            &json!({"status": "available", "phase": "available", "availableVersion": "0.5.0"}),
        );
        assert_eq!(available.latest_version.as_deref(), Some("0.5.0"));
        assert_eq!(available.error, None);

        let failed = map_desktop_update_state(
            &json!({"status": "error", "phase": "failed", "message": "boom"}),
        );
        assert_eq!(failed.error.as_deref(), Some("boom"));
    }
}
```

Run: `cargo test -p bibcode-desktop remote_update_delegate`
Expected: COMPILE ERROR (module body missing) — red.

- [x] **Step 6: Implement the desktop delegate**

Fill in `apps/desktop/src-tauri/src/remote_update_delegate.rs` above the tests module:

```rust
//! Bridges the in-process server's remote-update seam (spec section 4.5) onto the
//! desktop host's real updater. `updater.install` triggers exactly the flow a local
//! user triggers — including d8daae10's update-protection drain of the backend.

use std::sync::Arc;

use bibcode_server::remote_update::{
    HostUpdaterFuture, HostUpdaterStatus, RemoteUpdateDelegate, RemoteUpdateInstallMode,
    RemoteUpdateState, RemoteUpdateSupport, RemoteUpdateSupportReason,
};
use serde_json::Value;
use tauri::{AppHandle, Manager, Runtime};

use crate::backend::BackendSupervisor;
use crate::updates::{DesktopUpdateInstallInput, DesktopUpdateManager};

/// The same facts feed `ServerConfig.remote_update_support` and this delegate, so the
/// descriptor and the RPC behavior cannot drift.
#[must_use]
pub fn derive_remote_update_support(updater_enabled: bool) -> RemoteUpdateSupport {
    if cfg!(debug_assertions) {
        RemoteUpdateSupport {
            install_mode: RemoteUpdateInstallMode::Manual,
            reason: RemoteUpdateSupportReason::UnpackagedBuild,
        }
    } else if updater_enabled {
        RemoteUpdateSupport {
            install_mode: RemoteUpdateInstallMode::Interactive,
            reason: RemoteUpdateSupportReason::Available,
        }
    } else {
        RemoteUpdateSupport {
            install_mode: RemoteUpdateInstallMode::Manual,
            reason: RemoteUpdateSupportReason::UpdaterUnavailable,
        }
    }
}

#[must_use]
pub fn map_desktop_update_state(state: &Value) -> HostUpdaterStatus {
    let phase = state["phase"].as_str().unwrap_or("idle");
    let status = state["status"].as_str().unwrap_or("idle");
    let latest_version = state["availableVersion"]
        .as_str()
        .or_else(|| state["downloadedVersion"].as_str())
        .map(str::to_owned);
    let mapped = match (phase, status) {
        ("protecting" | "installing", _) => RemoteUpdateState::Installing,
        ("failed", _) | (_, "error") => RemoteUpdateState::Error,
        (_, "checking") => RemoteUpdateState::Checking,
        (_, "downloading") => RemoteUpdateState::Downloading,
        (_, "available" | "downloaded") => RemoteUpdateState::UpdateAvailable,
        (_, "up-to-date") => RemoteUpdateState::UpToDate,
        _ => RemoteUpdateState::Idle,
    };
    let error = if mapped == RemoteUpdateState::Error {
        state["message"].as_str().map(str::to_owned)
    } else {
        None
    };
    HostUpdaterStatus {
        latest_version,
        state: mapped,
        error,
    }
}

pub struct DesktopRemoteUpdateDelegate<R: Runtime> {
    app: AppHandle<R>,
}

impl<R: Runtime> DesktopRemoteUpdateDelegate<R> {
    pub fn new(app: AppHandle<R>) -> Arc<Self> {
        Arc::new(Self { app })
    }
}

impl<R: Runtime> RemoteUpdateDelegate for DesktopRemoteUpdateDelegate<R> {
    fn status(&self) -> HostUpdaterFuture {
        let app = self.app.clone();
        Box::pin(async move {
            let state = app.state::<DesktopUpdateManager>().state(&app);
            map_desktop_update_state(&state)
        })
    }

    fn check(&self) -> HostUpdaterFuture {
        let app = self.app.clone();
        Box::pin(async move {
            let result = app
                .state::<DesktopUpdateManager>()
                .check_for_update(app.clone())
                .await;
            map_desktop_update_state(&result["state"])
        })
    }

    fn request_install(&self) -> HostUpdaterFuture {
        let app = self.app.clone();
        Box::pin(async move {
            // Kick off the full host flow in the background; the remote client polls
            // `updater.status` for progress. Install failures surface there as
            // state "error".
            tauri::async_runtime::spawn(run_remote_install(app.clone()));
            let state = app.state::<DesktopUpdateManager>().state(&app);
            let mut mapped = map_desktop_update_state(&state);
            if !matches!(
                mapped.state,
                RemoteUpdateState::Error | RemoteUpdateState::Installing
            ) {
                // The spawned flow is now driving; report forward motion immediately
                // so the caller's snapshot is not a stale "update-available".
                mapped.state = RemoteUpdateState::Installing;
            }
            mapped
        })
    }
}

async fn run_remote_install<R: Runtime>(app: AppHandle<R>) {
    let updates = app.state::<DesktopUpdateManager>();
    let state = updates.state(&app);
    let needs_download = state["downloadedVersion"].as_str().is_none();
    if needs_download {
        if state["availableVersion"].as_str().is_none() {
            let checked = updates.check_for_update(app.clone()).await;
            if checked["state"]["availableVersion"].as_str().is_none() {
                return; // up to date, disabled, or the check failed — status reflects it
            }
        }
        let downloaded = updates.download_update(app.clone()).await;
        if downloaded["state"]["downloadedVersion"].as_str().is_none() {
            return; // download rejected or failed — status reflects it
        }
    }
    let backend = app.state::<BackendSupervisor>();
    let _ = updates
        .install_update(&app, backend.inner(), DesktopUpdateInstallInput::default())
        .await;
}
```

Note: `DesktopUpdateInstallInput::default()` means protection stays ON for remote
installs — a remote client can never skip the backup drain (skip requires the local
failure-acknowledgement flow from d8daae10). If `updates.rs` marks
`DesktopUpdateManager`, `DesktopUpdateInstallInput`, or the needed methods `pub(crate)`
only, that visibility already suffices (same crate); widen nothing.

- [x] **Step 7: Wire the delegate into the backend launch**

`apps/desktop/src-tauri/src/lib.rs`: declare `mod remote_update_delegate;` and, in the
setup closure that already spawns `updates::run_background_update_checks` (~line 107),
install the integration on the managed supervisor:

```rust
        {
            use tauri_plugin_updater::UpdaterExt as _;
            let backend = app.state::<crate::backend::BackendSupervisor>();
            backend.install_remote_update_integration(
                crate::remote_update_delegate::DesktopRemoteUpdateDelegate::new(
                    update_app.clone(),
                ),
                crate::remote_update_delegate::derive_remote_update_support(
                    update_app.updater().is_ok(),
                ),
            );
        }
```

(`update_app` is the `AppHandle` clone already taken for the background-check spawn;
reuse it or take another clone — match the surrounding code.)

`apps/desktop/src-tauri/src/backend.rs`: mirror the existing
`install_ui_process_observer` slot —

1. Add fields to `BackendSupervisor` alongside the ui-process-observer storage:

```rust
    remote_update_delegate:
        std::sync::Mutex<Option<std::sync::Arc<dyn bibcode_server::remote_update::RemoteUpdateDelegate>>>,
    remote_update_support:
        std::sync::Mutex<Option<bibcode_server::remote_update::RemoteUpdateSupport>>,
```

2. Add the installer:

```rust
    pub fn install_remote_update_integration(
        &self,
        delegate: std::sync::Arc<dyn bibcode_server::remote_update::RemoteUpdateDelegate>,
        support: bibcode_server::remote_update::RemoteUpdateSupport,
    ) {
        *self.remote_update_delegate.lock().expect("delegate slot") = Some(delegate);
        *self.remote_update_support.lock().expect("support slot") = Some(support);
    }
```

3. In `start_managed_backend`, for `BackendLaunchTarget::InProcess` only (~line 1740):
   set the config support and pick the start variant —

```rust
            if let Some(support) = *self.remote_update_support.lock().expect("support slot") {
                server_config = server_config.with_remote_update_support(support);
            }
            let delegate = self
                .remote_update_delegate
                .lock()
                .expect("delegate slot")
                .clone();
            let handle = match delegate {
                Some(delegate) => {
                    ServerRuntime::start_with_desktop_integration(
                        server_config,
                        ui_process_observer,
                        delegate,
                    )
                    .await
                }
                None => {
                    ServerRuntime::start_with_ui_process_observer(
                        server_config,
                        ui_process_observer,
                    )
                    .await
                }
            };
```

(Adapt variable names to the surrounding code; ExternalProcess/WSL launches are
untouched — those servers answer in their own headless manual mode.)

- [x] **Step 8: Run desktop and server tests to verify they pass**

```bash
cargo test -p bibcode-desktop remote_update_delegate
cargo test -p bibcode-desktop backend
cargo test -p bibcode-server --test remote_update_rpc
```

Expected: PASS. Existing backend tests that construct `BackendSupervisor` may need the
two new fields defaulted — extend the constructor/`Default` in one place, not each test.

- [x] **Step 9: Format, lint, commit**

```bash
cargo fmt --all
cargo clippy -p bibcode-server --all-targets -- -D warnings
cargo clippy -p bibcode-desktop --all-targets -- -D warnings
git add apps/server/src/lifecycle.rs apps/server/src/production/runtime.rs \
  apps/server/tests/remote_update_rpc.rs apps/desktop/src-tauri/src/remote_update_delegate.rs \
  apps/desktop/src-tauri/src/backend.rs apps/desktop/src-tauri/src/lib.rs
git commit -m "feat(desktop): route remote updater.install through the host updater"
```

---

### Task 7: Client-runtime update atoms and the max-2-concurrent fan-out

**Files:**

- Create: `packages/client-runtime/src/state/remoteUpdates.ts`
- Create: `packages/client-runtime/src/state/remoteUpdates.test.ts`
- Modify: `packages/client-runtime/package.json` (add the subpath export)

**Interfaces:**

- Consumes: `WS_METHODS.updaterStatus/updaterCheck/updaterInstall` (Task 5);
  `createEnvironmentRpcQueryAtomFamily` / `createEnvironmentRpcCommand` and
  `type AtomCommandResult` from `packages/client-runtime/src/state/runtime.ts`;
  `EnvironmentId`, `RemoteUpdateSnapshot` from `@bibcode/contracts`.
- Produces (imported as `@bibcode/client-runtime/state/remoteUpdates`):
  - `MAX_CONCURRENT_REMOTE_UPDATE_CHECKS = 2`
  - `fanOutRemoteUpdateChecks<A, E>(environmentIds, check, maxConcurrent?) =>
    Promise<ReadonlyArray<RemoteUpdateFanOutResult<A, E>>>` where `check` returns
    `Promise<AtomCommandResult<A, E>>` (the **settled** result a `useAtomCommand`
    dispatcher resolves with — typed failures are VALUES with `_tag: "Failure"`, they
    do not reject; see `apps/web/src/state/use-atom-command.ts` and
    `runAtomCommand` in `packages/client-runtime/src/state/runtime.ts`) and
    `RemoteUpdateFanOutResult<A, E> = { environmentId, outcome:
    { kind: "success"; result: AtomCommandResult<A, E> } |
    { kind: "failure"; result: AtomCommandResult<A, E> | null; error: unknown } }` —
    classification inspects `result._tag`, mirroring the Phase 4 plan's
    `result._tag === "Failure"` handling.
  - `isRemoteUpdateAvailable(snapshot: RemoteUpdateSnapshot | null): boolean` —
    Task 8 feeds this into Phase 6's rail-dot input.
  - `createRemoteUpdateEnvironmentAtoms(runtime)` returning `{ snapshot, check, install }`
    — `snapshot` is a query atom family keyed
    `{ environmentId, input: {} }` yielding `RemoteUpdateSnapshot`; `check`/`install`
    are commands with per-environment single-flight.

- [x] **Step 1: Write the failing tests**

Create `packages/client-runtime/src/state/remoteUpdates.test.ts`. The fan-out is a pure
promise scheduler — deliberately testable with no sockets, atoms, or Effect runtime.
Crucial semantics under test: `useAtomCommand` dispatchers RESOLVE with a settled
`AtomCommandResult` (typed failures are values with `_tag: "Failure"`,
`packages/client-runtime/src/state/runtime.ts` `runAtomCommand`) — a rejection-based
helper would silently count failures as successes:

```ts
import { describe, expect, it } from "@effect/vitest";
import { EnvironmentId, type RemoteUpdateSnapshot } from "@bibcode/contracts";

import type { AtomCommandResult } from "./runtime.ts";
import {
  MAX_CONCURRENT_REMOTE_UPDATE_CHECKS,
  fanOutRemoteUpdateChecks,
  isRemoteUpdateAvailable,
} from "./remoteUpdates.ts";

const flushMicrotasks = () => new Promise<void>((resolve) => setTimeout(resolve, 0));

// The fan-out classifies purely on `_tag`; minimal settled stand-ins are enough.
const settledSuccess = <A>(value: A): AtomCommandResult<A, never> =>
  ({ _tag: "Success", value }) as unknown as AtomCommandResult<A, never>;
const settledFailure = <E>(error: E): AtomCommandResult<never, E> =>
  ({ _tag: "Failure", cause: { error } }) as unknown as AtomCommandResult<never, E>;

describe("fanOutRemoteUpdateChecks", () => {
  it("exports the spec-pinned limit of two", () => {
    expect(MAX_CONCURRENT_REMOTE_UPDATE_CHECKS).toBe(2);
  });

  it("never runs more than two checks at once and preserves input order", async () => {
    const ids = ["env-a", "env-b", "env-c", "env-d"].map((id) => EnvironmentId.make(id));
    const releasers = new Map<string, () => void>();
    let inFlight = 0;
    let peak = 0;

    const batch = fanOutRemoteUpdateChecks(ids, (environmentId) => {
      inFlight += 1;
      peak = Math.max(peak, inFlight);
      return new Promise<AtomCommandResult<string, never>>((resolve) => {
        releasers.set(environmentId, () => {
          inFlight -= 1;
          resolve(settledSuccess(`checked:${environmentId}`));
        });
      });
    });

    await flushMicrotasks();
    expect(releasers.size).toBe(2);
    expect(peak).toBe(2);

    releasers.get(ids[0]!)!();
    await flushMicrotasks();
    expect(releasers.size).toBe(3);
    expect(peak).toBe(2);

    for (const release of [...releasers.values()]) release();
    await flushMicrotasks();
    for (const release of [...releasers.values()]) release();

    const results = await batch;
    expect(results.map((result) => result.environmentId)).toEqual(ids);
    expect(results.every((result) => result.outcome.kind === "success")).toBe(true);
    expect(peak).toBe(2);
  });

  it("classifies a settled Failure VALUE as a failure, not a success", async () => {
    const ids = ["env-a", "env-b", "env-c"].map((id) => EnvironmentId.make(id));
    const results = await fanOutRemoteUpdateChecks(ids, (environmentId) =>
      environmentId === ids[1]
        ? Promise.resolve(settledFailure("unreachable"))
        : Promise.resolve(settledSuccess("ok")),
    );
    expect(results.map((result) => result.outcome.kind)).toEqual(["success", "failure", "success"]);
    const failure = results[1]!.outcome;
    expect(failure.kind === "failure" && failure.result?._tag).toBe("Failure");
  });

  it("also isolates a thrown rejection (defensive) instead of aborting the batch", async () => {
    const ids = ["env-a", "env-b"].map((id) => EnvironmentId.make(id));
    const results = await fanOutRemoteUpdateChecks(ids, (environmentId) =>
      environmentId === ids[0]
        ? Promise.reject(new Error("dispatcher blew up"))
        : Promise.resolve(settledSuccess("ok")),
    );
    expect(results.map((result) => result.outcome.kind)).toEqual(["failure", "success"]);
    const failure = results[0]!.outcome;
    expect(failure.kind === "failure" && failure.result).toBeNull();
    expect(failure.kind === "failure" && failure.error).toBeInstanceOf(Error);
  });

  it("handles an empty environment list", async () => {
    await expect(
      fanOutRemoteUpdateChecks([], () => Promise.resolve(settledSuccess("ok"))),
    ).resolves.toEqual([]);
  });
});

describe("isRemoteUpdateAvailable", () => {
  const base: RemoteUpdateSnapshot = {
    serverVersion: "0.4.2",
    latestVersion: "0.5.0",
    state: "update-available",
    error: null,
    support: { installMode: "interactive", reason: "available" },
  };

  it("is true only for update-available snapshots", () => {
    expect(isRemoteUpdateAvailable(base)).toBe(true);
    expect(isRemoteUpdateAvailable({ ...base, state: "up-to-date" })).toBe(false);
    expect(isRemoteUpdateAvailable({ ...base, state: "error" })).toBe(false);
    expect(isRemoteUpdateAvailable(null)).toBe(false);
  });
});
```

(If the double-release loop proves brittle, collect fresh releasers after each flush —
the assertion that matters is `peak === 2` with all four results delivered. If the real
`SettledAsyncResult` tags differ from `"Success"`/`"Failure"`, follow the Phase 4 plan's
`result._tag === "Failure"` idiom and `packages/client-runtime/src/state/runtime.ts` —
those are the authority.)

- [x] **Step 2: Run tests to verify they fail**

Run: `vp test packages/client-runtime/src/state/remoteUpdates.test.ts`
Expected: FAIL — cannot resolve `./remoteUpdates.ts`.

- [x] **Step 3: Write minimal implementation**

Create `packages/client-runtime/src/state/remoteUpdates.ts`:

```ts
import { type EnvironmentId, type RemoteUpdateSnapshot, WS_METHODS } from "@bibcode/contracts";
import type { Atom } from "effect/unstable/reactivity";

import {
  type AtomCommandResult,
  createEnvironmentRpcCommand,
  createEnvironmentRpcQueryAtomFamily,
} from "./runtime.ts";
import type { EnvironmentRegistry } from "../connection/registry.ts";

/** Spec section 4.5: "Check for Server Updates" fans out with max 2 concurrent. */
export const MAX_CONCURRENT_REMOTE_UPDATE_CHECKS = 2;

export interface RemoteUpdateFanOutResult<A, E> {
  readonly environmentId: EnvironmentId;
  readonly outcome:
    | { readonly kind: "success"; readonly result: AtomCommandResult<A, E> }
    | {
        readonly kind: "failure";
        /** The settled Failure result, or null when the dispatcher itself threw. */
        readonly result: AtomCommandResult<A, E> | null;
        readonly error: unknown;
      };
}

/**
 * Runs `check` for every environment with bounded concurrency. One environment's
 * failure never aborts the batch; results keep input order.
 *
 * IMPORTANT: `check` is expected to resolve with a SETTLED `AtomCommandResult`
 * (`useAtomCommand`/`runAtomCommand` semantics — typed failures are values with
 * `_tag: "Failure"`, they do not reject). Classification therefore inspects the
 * settled result's tag; the catch branch is only a defensive net for a
 * dispatcher that throws outright.
 */
export async function fanOutRemoteUpdateChecks<A, E>(
  environmentIds: ReadonlyArray<EnvironmentId>,
  check: (environmentId: EnvironmentId) => Promise<AtomCommandResult<A, E>>,
  maxConcurrent: number = MAX_CONCURRENT_REMOTE_UPDATE_CHECKS,
): Promise<ReadonlyArray<RemoteUpdateFanOutResult<A, E>>> {
  const results = new Array<RemoteUpdateFanOutResult<A, E>>(environmentIds.length);
  let nextIndex = 0;
  const worker = async (): Promise<void> => {
    while (nextIndex < environmentIds.length) {
      const index = nextIndex;
      nextIndex += 1;
      const environmentId = environmentIds[index]!;
      try {
        const result = await check(environmentId);
        results[index] =
          result._tag === "Success"
            ? { environmentId, outcome: { kind: "success", result } }
            : { environmentId, outcome: { kind: "failure", result, error: result } };
      } catch (error) {
        results[index] = { environmentId, outcome: { kind: "failure", result: null, error } };
      }
    }
  };
  const workerCount = Math.min(maxConcurrent, environmentIds.length);
  await Promise.all(Array.from({ length: workerCount }, worker));
  return results;
}

/** Feeds Phase 6's rail-dot `updateAvailable` input (spec section 4.8). */
export function isRemoteUpdateAvailable(snapshot: RemoteUpdateSnapshot | null): boolean {
  return snapshot?.state === "update-available";
}

/**
 * Per-environment update-state surface. The server owns the snapshot
 * (`updater.status` restores it after navigation/reconnect); the query atom family
 * keeps the last value per environment for instant re-render (spec section 6).
 */
export function createRemoteUpdateEnvironmentAtoms<R, ER>(
  runtime: Atom.AtomRuntime<EnvironmentRegistry | R, ER>,
) {
  return {
    snapshot: createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "environment-data:remote-update:snapshot",
      tag: WS_METHODS.updaterStatus,
      staleTimeMs: 30_000,
    }),
    check: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:remote-update:check",
      tag: WS_METHODS.updaterCheck,
      concurrency: {
        mode: "singleFlight",
        key: ({ environmentId }) => environmentId,
      },
    }),
    install: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:remote-update:install",
      tag: WS_METHODS.updaterInstall,
      concurrency: {
        mode: "singleFlight",
        key: ({ environmentId }) => environmentId,
      },
    }),
  };
}
```

(Before writing, skim `packages/client-runtime/src/state/server.ts` — the atoms block
above mirrors its `refreshProviders` single-flight idiom; if the `concurrency` option
shape differs in the current tree, follow `server.ts`.)

Add the subpath export to `packages/client-runtime/package.json`, alphabetically among
the existing `./state/*` entries:

```json
    "./state/remoteUpdates": {
      "types": "./src/state/remoteUpdates.ts",
      "default": "./src/state/remoteUpdates.ts"
    },
```

- [x] **Step 4: Run tests to verify they pass**

Run: `vp test packages/client-runtime/src/state/remoteUpdates.test.ts`
Expected: PASS (6 tests).

- [x] **Step 5: Commit**

```bash
git add packages/client-runtime/src/state/remoteUpdates.ts \
  packages/client-runtime/src/state/remoteUpdates.test.ts \
  packages/client-runtime/package.json
git commit -m "feat(client-runtime): remote update atoms and bounded check fan-out"
```

---

### Task 8: Web surface — badge, manual instructions, check-all, and slot wiring

Phase 4 (settings Connect tab) and Phase 6 (environment rail + context card) land
before this task, and their plan files pin the integration names this task consumes
(Phase 6's Interfaces "Produces" block says "Phase 7 relies on these exact names").
Wiring is interface-level: do not re-implement Phase 4/6 surfaces here. The Phase 6
slots are:

- `EnvironmentContextCard` props `updateBadge?: React.ReactNode` and
  `onCheckForUpdates?: (environmentId: EnvironmentId) => void` (the card's "Check for
  updates" menu item is hidden until the handler is passed);
- `selectRemoteUpdateControlCapability(serverConfig)` in
  `apps/web/src/connection/environmentCompat.ts` — Phase 7 replaces its defensive read
  with the typed contract field;
- `EnvironmentRailCandidate.updateAvailable: boolean` in
  `apps/web/src/components/sidebar/environmentRail.logic.ts` (currently constant
  `false` in `toEnvironmentRailCandidate`) — amended spec §4.8: **Phase 7 wires the
  update input into the Phase 6 rail dot** (`resolveEnvironmentRailStatus` already
  returns `"attention"` when `updateAvailable` is true).

If any name drifted, re-read `phases/phase-6-environment-rail.md`'s Interfaces blocks
and the landed source before wiring.

**Files:**

- Create: `apps/web/src/state/remoteUpdates.ts`
- Create: `apps/web/src/components/settings/ServerUpdateBadge.tsx`
- Create: `apps/web/src/components/settings/ServerUpdateBadge.test.tsx`
- Modify: the Phase 4 Connect-tab server-row component (badge + per-row actions +
  "Check for Server Updates" button)
- Modify: `apps/web/src/components/Sidebar.tsx` (or wherever Phase 6 mounts
  `EnvironmentContextCard`) — pass `updateBadge` + `onCheckForUpdates`
- Modify: `apps/web/src/connection/environmentCompat.ts` (+ its test) — typed
  capability read
- Modify: `apps/web/src/components/sidebar/environmentRail.logic.ts` (+ its test) and
  `apps/web/src/components/sidebar/EnvironmentRail.tsx` — feed `updateAvailable`

**Interfaces:**

- Consumes: `createRemoteUpdateEnvironmentAtoms`, `fanOutRemoteUpdateChecks`,
  `isRemoteUpdateAvailable`, `MAX_CONCURRENT_REMOTE_UPDATE_CHECKS` (Task 7);
  `RemoteUpdateSnapshot` from `@bibcode/contracts` (Task 1); `connectionAtomRuntime`
  from `apps/web/src/connection/runtime`; `useEnvironments` from
  `apps/web/src/state/environments.ts`; `serverEnvironment.configValueAtom` from
  `apps/web/src/state/server.ts`; the Phase 6 names listed above.
- Produces: `remoteUpdateEnvironment` (app-level atoms),
  `ServerUpdateBadge`, `serverUpdateBadgeVariant`, `manualUpdateInstructions`;
  `selectRemoteUpdateControlCapability` now reads the typed
  `capabilities.remoteUpdateControl` field.

- [x] **Step 1: Write the failing logic tests**

Create `apps/web/src/components/settings/ServerUpdateBadge.test.tsx`:

```tsx
// @vitest-environment happy-dom

import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vite-plus/test";
import type { RemoteUpdateSnapshot } from "@bibcode/contracts";

import {
  ServerUpdateBadge,
  manualUpdateInstructions,
  serverUpdateBadgeVariant,
} from "./ServerUpdateBadge";

const manualSnapshot: RemoteUpdateSnapshot = {
  serverVersion: "0.4.2",
  latestVersion: null,
  state: "idle",
  error: null,
  support: { installMode: "manual", reason: "manual-update-required" },
};

const interactiveSnapshot: RemoteUpdateSnapshot = {
  ...manualSnapshot,
  latestVersion: "0.5.0",
  state: "update-available",
  support: { installMode: "interactive", reason: "available" },
};

describe("serverUpdateBadgeVariant", () => {
  it("maps every snapshot state onto a badge variant", () => {
    expect(serverUpdateBadgeVariant(null)).toBe("unknown");
    expect(serverUpdateBadgeVariant(manualSnapshot)).toBe("manual");
    expect(serverUpdateBadgeVariant({ ...manualSnapshot, state: "up-to-date" })).toBe("up-to-date");
    expect(serverUpdateBadgeVariant(interactiveSnapshot)).toBe("update-available");
    expect(serverUpdateBadgeVariant({ ...interactiveSnapshot, state: "checking" })).toBe("busy");
    expect(serverUpdateBadgeVariant({ ...interactiveSnapshot, state: "downloading" })).toBe("busy");
    expect(serverUpdateBadgeVariant({ ...interactiveSnapshot, state: "installing" })).toBe("busy");
    expect(
      serverUpdateBadgeVariant({ ...interactiveSnapshot, state: "error", error: "boom" }),
    ).toBe("error");
  });
});

describe("ServerUpdateBadge", () => {
  it("names the available version when known", () => {
    const markup = renderToStaticMarkup(<ServerUpdateBadge snapshot={interactiveSnapshot} />);
    expect(markup).toContain("0.5.0");
    expect(markup).toContain('data-variant="update-available"');
  });

  it("labels manual servers without claiming update knowledge", () => {
    const markup = renderToStaticMarkup(<ServerUpdateBadge snapshot={manualSnapshot} />);
    expect(markup).toContain("Manual updates");
  });
});

describe("manualUpdateInstructions", () => {
  it("gives copy-paste steps that mention the running version", () => {
    const instructions = manualUpdateInstructions("0.4.2");
    expect(instructions).toContain("bibcode serve");
    expect(instructions).toContain("0.4.2");
  });
});
```

- [x] **Step 2: Run tests to verify they fail**

Run: `vp test apps/web/src/components/settings/ServerUpdateBadge.test.tsx`
Expected: FAIL — module `./ServerUpdateBadge` does not exist.

- [x] **Step 3: Write the minimal implementation**

Create `apps/web/src/state/remoteUpdates.ts`:

```ts
import { createRemoteUpdateEnvironmentAtoms } from "@bibcode/client-runtime/state/remoteUpdates";

import { connectionAtomRuntime } from "../connection/runtime";

export const remoteUpdateEnvironment = createRemoteUpdateEnvironmentAtoms(connectionAtomRuntime);
```

Create `apps/web/src/components/settings/ServerUpdateBadge.tsx`:

```tsx
import type { RemoteUpdateSnapshot } from "@bibcode/contracts";

export type ServerUpdateBadgeVariant =
  | "up-to-date"
  | "update-available"
  | "busy"
  | "manual"
  | "error"
  | "unknown";

export function serverUpdateBadgeVariant(
  snapshot: RemoteUpdateSnapshot | null,
): ServerUpdateBadgeVariant {
  if (snapshot === null) return "unknown";
  switch (snapshot.state) {
    case "error":
      return "error";
    case "checking":
    case "downloading":
    case "installing":
      return "busy";
    case "update-available":
      return "update-available";
    case "up-to-date":
      return "up-to-date";
    case "idle":
      return snapshot.support.installMode === "manual" ? "manual" : "unknown";
  }
}

const BADGE_LABELS: Record<ServerUpdateBadgeVariant, string> = {
  "up-to-date": "Up to date",
  "update-available": "Update available",
  busy: "Updating…",
  manual: "Manual updates",
  error: "Update status error",
  unknown: "Status unavailable",
};

const BADGE_CLASSES: Record<ServerUpdateBadgeVariant, string> = {
  "up-to-date": "border-border text-muted-foreground",
  "update-available": "border-amber-500/40 text-amber-600 dark:text-amber-400",
  busy: "border-border text-muted-foreground animate-pulse",
  manual: "border-border text-muted-foreground",
  error: "border-destructive/40 text-destructive",
  unknown: "border-border text-muted-foreground/70",
};

export function ServerUpdateBadge({ snapshot }: { snapshot: RemoteUpdateSnapshot | null }) {
  const variant = serverUpdateBadgeVariant(snapshot);
  const label =
    variant === "update-available" && snapshot?.latestVersion != null
      ? `Update to v${snapshot.latestVersion}`
      : BADGE_LABELS[variant];
  return (
    <span
      data-variant={variant}
      className={`inline-flex items-center rounded border px-1.5 py-0.5 text-xs ${BADGE_CLASSES[variant]}`}
    >
      {label}
    </span>
  );
}

/**
 * Headless servers cannot install remotely and have no update feed
 * (pinned phase decision 4): show honest operator steps, never a fabricated
 * "latest version".
 */
export function manualUpdateInstructions(serverVersion: string): string {
  return [
    "# Update this BiBCode server manually on its host:",
    "# 1. Stop the running server (Ctrl+C or your service manager).",
    "# 2. Install the latest bibcode build (replace the binary on PATH).",
    "# 3. Restart it:",
    "bibcode serve",
    "",
    `# Currently running: v${serverVersion}`,
  ].join("\n");
}
```

- [x] **Step 4: Run tests to verify they pass**

Run: `vp test apps/web/src/components/settings/ServerUpdateBadge.test.tsx`
Expected: PASS (5 tests).

- [x] **Step 5: Wire the settings and context-card slots (interface-level)**

All wiring is gated on the capability boolean via Phase 6's selector — first replace
its defensive read with the typed field this phase introduced
(`apps/web/src/connection/environmentCompat.ts`):

```ts
export function selectRemoteUpdateControlCapability(serverConfig: ServerConfig | null): boolean {
  return serverConfig?.environment.capabilities.remoteUpdateControl === true;
}
```

(update its unit test to feed the typed capability field instead of the untyped
record). Older servers (decode-default `false`) render none of this surface. Snapshot
reads use `remoteUpdateEnvironment.snapshot({ environmentId, input: {} })`; a
not-yet-loaded or failed query renders `<ServerUpdateBadge snapshot={null} />`
("Status unavailable" — spec §6 keeps the underlying error in the query result for a
tooltip).

1. **Connect-tab server rows** (Phase 4 component): per remote-environment row, render
   `<ServerUpdateBadge …/>` next to the existing version/compat text, plus:
   - `installMode === "interactive"` and variant `update-available`: an "Update"
     button dispatching `remoteUpdateEnvironment.install` for that environment (via
     `useAtomCommand`, matching how the row's other commands are dispatched in that
     file).
   - `installMode === "manual"`: an expander/tooltip rendering
     `manualUpdateInstructions(snapshot.serverVersion)` in a `<pre>` with a copy
     button, reusing the row component's existing copy-button primitive. This is also
     the surface an `updater.install` failure result
     (`RemoteUpdateInstallError`, code `remote_update_manual_required`) routes to —
     remember the dispatcher RESOLVES with a settled result; inspect
     `result._tag === "Failure"`, never a catch block.
2. **"Check for Server Updates"** (Connect-tab header action): a button that collects
   the saved remote environment ids from `useEnvironments()` (exclude the local/primary
   entry), and runs:

```ts
const checkAll = async () => {
  const outcomes = await fanOutRemoteUpdateChecks(remoteEnvironmentIds, (environmentId) =>
    runCheck({ environmentId, input: {} }),
  );
  // outcomes with kind "failure" (settled Failure results or thrown dispatches)
  // leave their rows on "Status unavailable"; no toast storm — the per-row badge
  // is the surface.
};
```

where `runCheck` is the `useAtomCommand(remoteUpdateEnvironment.check, …)` dispatcher
— it RESOLVES with `AtomCommandResult` (typed failures are `_tag: "Failure"` values);
the Task 7 fan-out classifies on that tag and caps concurrency at 2. Row badges
update reactively as each check result lands in the snapshot family
(invalidate/refresh the row's snapshot atom after its check settles successfully,
following the file's existing refresh idiom). 3. **Context card** (Phase 6's `EnvironmentContextCard`): pass the two props Phase 6
exposed for exactly this purpose — do not add new menu plumbing:

```tsx
<EnvironmentContextCard
  {...existingProps}
  updateBadge={remoteUpdateControl ? <ServerUpdateBadge snapshot={snapshot} /> : undefined}
  onCheckForUpdates={
    remoteUpdateControl ? (environmentId) => void runCheck({ environmentId, input: {} }) : undefined
  }
/>
```

(at the mount site in `Sidebar.tsx`; `remoteUpdateControl` comes from
`selectRemoteUpdateControlCapability(serverConfig)`. Phase 6 keeps the "Check for
updates" menu item hidden while `onCheckForUpdates` is undefined —
hidden-until-capable.) 4. Extend the nearest existing test file for the row component (or
`SettingsPanels.test.tsx` if the rows are exercised there) with one focused case:
capability-off renders no badge; capability-on renders the badge markup. Follow that
file's existing harness idioms.

- [x] **Step 6: Wire update-available into the Phase 6 rail dot (amended spec §4.8)**

Phase 6's `toEnvironmentRailCandidate`
(`apps/web/src/components/sidebar/environmentRail.logic.ts`) hardcodes
`updateAvailable: false` with a comment reserving it for Phase 7;
`resolveEnvironmentRailStatus` already turns the dot amber (`"attention"`) when it is
true. Wire it now:

1. Failing test first — extend `environmentRail.logic.test.ts` (Phase 6's test file for
   this module):

```ts
it("passes updateAvailable through to the candidate", () => {
  const candidate = toEnvironmentRailCandidate({
    ...baseCandidateInput, // reuse the file's existing input fixture
    updateAvailable: true,
  });
  expect(candidate.updateAvailable).toBe(true);
  expect(
    resolveEnvironmentRailStatus({
      phase: "connected",
      compat: null,
      updateAvailable: true,
    }),
  ).toBe("attention");
});
```

Run: `vp test apps/web/src/components/sidebar/environmentRail.logic.test.ts` —
FAIL (the input type has no `updateAvailable` member).

2. Extend `toEnvironmentRailCandidate`'s input with
   `readonly updateAvailable: boolean;` and replace the hardcoded
   `updateAvailable: false,` (and its Phase-7 reservation comment) with
   `updateAvailable: input.updateAvailable,`. Update Phase 6's existing call sites and
   test fixtures to pass `updateAvailable: false` where no snapshot is in play.

3. In `EnvironmentRail.tsx`'s candidates memo, feed the real value from the snapshot
   family via Task 7's helper:

```tsx
toEnvironmentRailCandidate({
  environmentId: environment.environmentId,
  label: environment.label,
  target: environment.entry.target,
  phase: environment.connection.phase,
  compat: resolveEnvironmentCompatVerdict(environment.serverConfig),
  updateAvailable: isRemoteUpdateAvailable(updateSnapshotFor(environment.environmentId)),
});
```

where `updateSnapshotFor` reads
`remoteUpdateEnvironment.snapshot({ environmentId, input: {} })` through the
component's atom hooks and yields the last-known `RemoteUpdateSnapshot | null`
(null while loading/failed/capability-off — the dot then depends on compat only).
Follow the component's existing per-environment atom-read idiom for the map.

4. Re-run: `vp test apps/web/src/components/sidebar/` — PASS, including Phase 6's
   pre-existing rail tests (updated fixtures included).

- [x] **Step 7: Run the web suites**

```bash
vp test apps/web/src/components/settings/ apps/web/src/components/sidebar/ \
  apps/web/src/connection/environmentCompat.test.ts
vp run typecheck
```

Expected: PASS, including the extended row, rail-logic, and capability-selector tests.

- [x] **Step 8: Commit**

```bash
git add apps/web/src/state/remoteUpdates.ts \
  apps/web/src/components/settings/ServerUpdateBadge.tsx \
  apps/web/src/components/settings/ServerUpdateBadge.test.tsx \
  apps/web/src/components/settings/ apps/web/src/components/sidebar/ \
  apps/web/src/components/Sidebar.tsx apps/web/src/connection/
git commit -m "feat(web): server update badges, check-all fan-out, and amber rail dots"
```

---

### Task 9: Living documentation and testing runbooks

This phase changes packaged UI flows (settings update surface) and adds an RPC surface,
so runbooks get a real edit — not a "reviewed and remain accurate" statement.

**Files:**

- Modify: `docs/architecture/overview.md` (updater surface)
- Modify: `docs/architecture/remote.md` (remote update contract + headless decision)
- Modify: `docs/testing/linux-desktop.md`, `docs/testing/macos-desktop.md`,
  `docs/testing/windows-desktop.md` (one validation step each)

**Interfaces:**

- Consumes: everything shipped in Tasks 1–8 (documents must describe the code as
  landed, not as planned).

- [x] **Step 1: Update `docs/architecture/overview.md`**

In the section that documents desktop updates / update protection (extended by
d8daae10), add a subsection:

```markdown
### Remote server updates

Every server answers the `updater.status` / `updater.check` / `updater.install` RPC
methods (contract: `packages/contracts/src/remoteUpdate.ts`; Rust mirror:
`apps/server/src/remote_update.rs`). All three environment-descriptor producers — the
well-known route (`apps/server/src/http.rs`), `server.getConfig`
(`apps/server/src/production/control.rs`), and the Connect/relay descriptor
(`apps/server/src/lifecycle.rs`) — embed `remoteUpdateSupport` and advertise the
surface with the default-false `remoteUpdateControl` capability, so clients know the
install mode before asking.

- Desktop-hosted (in-process) servers run in `interactive` mode: `updater.install`
  routes through the host's `DesktopUpdateManager` via the `RemoteUpdateDelegate`
  seam (`apps/desktop/src-tauri/src/remote_update_delegate.rs`), so a remote install
  runs the same update-protection drain as a local one and can never skip backup
  protection.
- Headless `bibcode serve` (and WSL/external desktop backends) run in `manual` mode:
  `updater.check` refreshes the server's own version, `latestVersion` is always
  `null` (the server has no update feed), and `updater.install` fails with
  `remote_update_manual_required`; clients render copy-paste operator instructions.

Scopes: `updater.status` requires `orchestration:read`; `updater.check` and
`updater.install` require `orchestration:operate` (`apps/server/src/auth/scope.rs`).
```

- [x] **Step 2: Update `docs/architecture/remote.md`**

Add a "Remote server updates" section stating: the wire contract (three methods,
snapshot shape, install error), the descriptor embedding + capability gate, the
client-side "Check for Server Updates" fan-out (max 2 concurrent,
`packages/client-runtime/src/state/remoteUpdates.ts`), and this explicit design
decision:

```markdown
The update feed URL is baked into the desktop release configuration only
(`apps/desktop/src-tauri/tauri.release.conf.json`); the server binary has no feed
access. A `manual`-mode server therefore never reports a `latestVersion` — the
snapshot is honest about what the server can know, and manual-mode UI copy instructs
the operator instead of guessing. Teaching servers a feed URL is a possible future
extension, deliberately out of scope for v1.
```

- [x] **Step 3: Update the three desktop runbooks**

Append one step to the packaged-UI validation flow of each of
`docs/testing/linux-desktop.md`, `docs/testing/macos-desktop.md`,
`docs/testing/windows-desktop.md` (match each file's existing step formatting):

```markdown
- Remote server updates: with a second BiBCode server saved (headless `bibcode serve`
  is sufficient), open the Remote Servers settings, run "Check for Server Updates",
  and confirm each saved server row shows an update badge ("Manual updates" for a
  headless server) and that the manual-instructions copy block renders with a copy
  button. Rows for servers that are offline must show "Status unavailable" without
  blocking the rest of the batch.
```

- [x] **Step 4: Verify docs against the landed code**

Re-read each edited section and confirm every path, method name, scope, and behavior
matches the implementation from Tasks 1–8 (AGENTS.md: living docs change in the same
patch and must match source). `docs/architecture/connection-runtime.md` needs no change
in this phase (no connection/catalog behavior changed) — state exactly that in the
final report.

- [x] **Step 5: Commit**

```bash
git add docs/architecture/overview.md docs/architecture/remote.md docs/testing/
git commit -m "docs: document the remote server update surface and validation steps"
```

---

## Final validation gate (master plan, run after Task 9)

Run and report every command, anything that could not run, and residual risk:

```bash
vp check
vp run typecheck
vp test packages/contracts/src/ packages/client-runtime/src/state/remoteUpdates.test.ts \
  apps/web/src/components/settings/ServerUpdateBadge.test.tsx \
  apps/web/src/components/sidebar/environmentRail.logic.test.ts \
  apps/web/src/connection/environmentCompat.test.ts
cargo fmt --all --check
cargo test -p bibcode-server remote_update
cargo test -p bibcode-server connect_descriptor
cargo test -p bibcode-server scope
cargo test -p bibcode-server --test remote_update_rpc
cargo test -p bibcode-server --test rpc_wire
cargo test -p bibcode-desktop remote_update_delegate
cargo clippy -p bibcode-server --all-targets -- -D warnings
cargo clippy -p bibcode-desktop --all-targets -- -D warnings
git status --short && git diff --stat
```

`git status --short` must show no strays: the regenerated
`packages/contracts/fixtures/rpc-wire/` files are committed (Task 5), and the user's
pending deletions under `docs/plans/2026-08-24-environment-project-management/` are
untouched.

**Residual risks to carry into the report:**

- The desktop interactive install restarts the whole desktop app; remote clients
  observe a disconnect and rely on the existing supervisor backoff to reconnect. This
  is by design (update protection ran first) but should be stated in the report.
- `run_remote_install` reuses the local updater flow; if a local user is mid-update,
  the manager's in-flight guards make the remote request a no-op that reports current
  state — verify once manually on a packaged build (runbook step from Task 9).
- Effect/atom API drift: if `createEnvironmentRpcQueryAtomFamily`'s option names moved,
  follow `packages/client-runtime/src/state/server.ts`, and consult
  `.repos/effect-smol/LLMS.md` before adjusting any Effect usage.
