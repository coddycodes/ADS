# Remote Servers Phase 4 — Settings Section + Connect Tab Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Evolve the Connections settings section in place into a "Remote Servers" section at
`/settings/remote-servers` (old path redirecting) with a two-tab shell — this phase builds the
**Connect to a host** tab (saved-server rows with status/version/compat/transport, Add Server via
pasted pairing code, Advanced manual entry, troubleshooting) plus the `bibcode://pair` /
`/pair?code=…` deep-link entry points; Phase 5 fills the **Share this host** tab.

**Architecture:** Pure client-side UI evolution over the existing connection catalog and
environment presentation atoms, consuming Phase 2's `CompatVerdict` and Phase 3's pairing-code
verify-then-add flow. The 3,213-line `ConnectionsSettings.tsx` splits into a
`remote-servers/` directory (shell + ConnectTab + ShareTab + shared helpers) so Phase 5 slots
into `ShareTab.tsx` without another restructuring. One small Rust change registers the
`bibcode://` URL scheme in the Tauri 2 desktop host (deep-link + single-instance plugins).

**Tech Stack:** React 19 + TanStack Router (file routes, generated route tree), Effect Atom
commands, `@base-ui/react` primitives, vite-plus (`vp`) test runner, Tauri 2 (Rust) for
deep-link registration.

**Spec:** `docs/plans/remote-servers/remote-servers-spec.md` (§4.8 UI contracts, §4.2 pairing
code, §4.4 compat window, §6 failure policies; decisions D7, D12, D13, D16). Master plan:
`docs/plans/remote-servers/remote-servers-plan.md` (this file is Phase 4).
Current-state survey: `docs/plans/remote-servers/bibcode-current-state.md` §5 (verified against
source on 2026-08-27; line references below are from that verification).

## Global Constraints

(Copied from the master plan; every task's requirements implicitly include this section.)

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

## Phase-level design decisions (read before any task)

These are Phase 4 decisions; Phase 5/6/7 executors rely on them.

1. **Route map (D7).** `/settings/remote-servers` is the new section route.
   `/settings/connections` **always** redirects there (replace-style). On Windows desktop the
   old `/settings/connections` path used to render the WSL "Local environment" page; that page
   moves to its own route `/settings/local-environment` with the existing "Local environment"
   nav item (still Windows-desktop-only). Old Windows bookmarks to `/settings/connections`
   therefore now land on Remote Servers — deliberate: the WSL page keeps its nav item.
2. **The section now exists on every surface** (spec D2): browser, and desktop on macOS,
   Windows, and Linux. Today desktop macOS/Linux redirect Connections to General and Windows
   shows only the WSL page; those special cases are removed from
   `EnvironmentPresentationPolicy` (the `connectionsPresentation` field and its
   `ConnectionsPresentation` type are deleted). Content degradation happens _inside_ the page:
   SSH discovery and manual SSH entry render only when `window.desktopBridge` exists, exactly
   as the current code already branches. **Do not touch `presentsTarget` /
   `permitsConnectionAction`** — desktop-side environment-visibility widening is Phase 6's
   "primary-environment leak fixes", not this phase.
3. **Tab shell.** `RemoteServersSettings` renders two tabs, exactly the spec names:
   **"Connect to a host"** (default) and **"Share this host"**. In this phase the Share tab
   receives today's share-side content (network access/exposure, advertised endpoints,
   Tailscale, cloud link, pairing links, authorized clients, and their dialogs) **moved
   verbatim** into `ShareTab.tsx` with no behavior change; Phase 5 evolves that file in place.
   Note the consequence: this content becomes newly reachable on macOS/Linux desktop (it was
   unreachable there because the whole section redirected). Its `desktopBridge` branches were
   written for that case but have never been user-reachable on those platforms — flag this in
   the final report as residual risk for Phase 5 to validate.
4. **Connect-tab first-class content (D12).** Direct pairing (pasted pairing code) is the
   primary Add Server mode. SSH keeps its own first-class mode card (manual host + discovered
   SSH host rows, desktop only). Relay (BiBCode Connect) environment rows
   (`CloudRemoteEnvironmentRows`) stay first-class rows in the saved-servers list, unchanged.
   The **Advanced expander holds only today's manual endpoint + pairing-token entry** (the
   `connectPairing` → `ConnectionOnboarding.registerPairing` path) — it is _not_ a dumping
   ground for SSH.
5. **"Check for Server Updates" (spec §4.5, wired in Phase 7):** the button is **hidden behind
   a Phase 4-owned seam** (`SERVER_UPDATE_CHECK_ENABLED = false` constant that Phase 7 replaces
   with the `remoteUpdateControl`-capability predicate), not rendered disabled.
   _Justification:_ phases ship independently — a permanently disabled button in a shipped
   build is a dead control that looks broken and violates "predictable behavior"; and spec
   §4.5 already says the capability boolean gates "the whole surface", so hiding matches the
   final design. The placement, handler seam, and a test that renders it when the flag is
   forced on all land now, so Phase 7 only swaps the predicate and fills the handler.
6. **Deep links converge on `/pair?code=…`.** The desktop deep-link listener parses
   `bibcode://pair?code=…` and navigates to `/pair` with `search: { code }`; the web
   `/pair?code=…` route forwards **already-authenticated** sessions to
   `/settings/remote-servers?code=…` (Add Server dialog prefilled). A **fresh,
   unauthenticated device** is never gated on a pre-existing session (amended spec §4.2):
   the code itself carries the one-time credential, so the pairing route surface consumes
   the embedded `token` to establish the browser session with the serving host, then lands
   at the root — no Add Server step, because the primary session it just established _is_
   that server (saving it as a bearer entry too would duplicate the storage identity).
   One code path handles cold start, running instance, and plain browser.
   The desktop currently registers **no** deep-link handling (verified: no deep-link plugin in
   `apps/desktop/src-tauri/Cargo.toml`, no `"deep-link"` key in `tauri.conf.json`), so Task 9
   adds the Tauri 2 `tauri-plugin-deep-link` + `tauri-plugin-single-instance` registration.
7. **Copy rules (D16).** Entities are **environments**; the machine hosting them is a
   **server**; the local one is **"Local"**; version strings render as
   **`BiBCode v<serverVersion>`**. New/edited copy in surfaces this phase touches drops the
   word "backend" in user-facing text. No reference-product strings anywhere.
8. **Pinned badge copy** (this phase owns these strings; only "Limited compatibility" is
   spec-pinned):

   | Source                             | Value                                                                        | Rendered copy                                                                        | Tone        |
   | ---------------------------------- | ---------------------------------------------------------------------------- | ------------------------------------------------------------------------------------ | ----------- |
   | `CompatVerdict`                    | `compatible`                                                                 | _(no badge)_                                                                         | —           |
   | `CompatVerdict`                    | `legacy`                                                                     | `Limited compatibility`                                                              | warning     |
   | `CompatVerdict`                    | `server-too-old`                                                             | `Server update required`                                                             | destructive |
   | `CompatVerdict`                    | `client-too-old`                                                             | `App update required`                                                                | destructive |
   | Transport                          | bearer profile **with** `hostKey`                                            | `End-to-end encrypted`                                                               | neutral     |
   | Transport                          | bearer profile **without** `hostKey` (legacy)                                | `Unencrypted` + tooltip `Re-pair with a new pairing code to secure this connection.` | warning     |
   | Transport                          | SSH target                                                                   | `SSH tunnel`                                                                         | neutral     |
   | Transport                          | relay-managed                                                                | `BiBCode Connect`                                                                    | neutral     |
   | Probe failed, no cached descriptor | `Status unavailable` (underlying error preserved in the existing error line) | muted                                                                                |

## Interfaces consumed from Phases 2 and 3 (never redefine — align, don't duplicate)

From the master plan's interface summary and spec §4. Where the spec pins a name it is used
verbatim; the two names marked _(assumed)_ come from the master-plan summary's descriptions —
if the earlier phase landed a different export name, **use the landed name and adjust only the
Phase 4-owned wrappers**; never add a second definition.

- **Phase 2:** `CompatVerdict` from `packages/client-runtime/src/connection/compat.ts`
  (spec §4.4 shape: `{ kind: "compatible" } | { kind: "legacy" } | { kind: "server-too-old";
  serverVersion: number; minSupported: number } | { kind: "client-too-old";
  serverMinCompatible: number; clientVersion: number }`). Phase 2's landed per-environment
  accessor is `environmentSession.compatVerdictAtom(environmentId)` (an `Atom` of
  `CompatVerdict | null`, exported via `createEnvironmentSessionAtoms`; `apps/web/src/state/session.ts`
  already re-exports `environmentSession`). Phase 4's row code reads that atom — there is no
  `compat` field on `EnvironmentPresentation`; keep the read isolated in one Phase 4-owned
  helper.
- **Phase 3:** pairing-code payload schema in `packages/contracts/src/remotePairing.ts` —
  landed exports `RemotePairingCodePayload` (Schema.Struct over the spec §4.2 fields `v`,
  `endpoint`, `name`, `token`, `hostKey`, `reach`, `storageInstanceId`),
  `RemotePairingReach`, `REMOTE_PAIRING_CODE_VERSION`; the client-side codec is
  `parsePairingCode` / `encodePairingCode` from `packages/shared/src/pairingCode.ts`
  (accepts the bare code, `bibcode://pair?code=…`, and `http(s)://…/pair?code=…`; throws
  the tagged `PairingCodeParseError` / `PairingCodeUnsupportedVersionError` — use it rather
  than hand-rolling `Schema.decodeUnknownSync`); `hostKey` on `BearerConnectionProfile` in
  `packages/client-runtime/src/connection/catalog.ts` is **required-nullable**
  (`string | null`, decode-default `null` for legacy profiles — never `undefined`);
  `classifyPairingEndpoint(endpoint: string): "loopback" | "private-network" | "public" |
  "unconnectable"` from `packages/shared/src/advertisedEndpoint.ts` (spec §4.2); and the
  verify-then-add flow — `verifyAndAddPairingCode(input: VerifyPairingCodeInput)` on the
  `ConnectionOnboarding` service, where
  `VerifyPairingCodeInput = { readonly code: string; readonly allowLoopbackTunnel?: boolean }`
  (`allowLoopbackTunnel` is the explicit tunnel acknowledgement — without it, a
  loopback-endpoint code fails fast with the tagged
  `PairingLoopbackAcknowledgementRequiredError { endpoint }`). Classified failures arrive as
  `PairingAddError { reason, detail }` with `reason: PairingAddFailureReason =
  "unreachable" | "host-identity-mismatch" | "pairing-rejected" | "incompatible" |
  "duplicate-storage-identity"` (the five reasons are spec-pinned verbatim; the **field is
  `reason`, not `kind`**). Malformed/future codes surface as `PairingCodeParseError` /
  `PairingCodeUnsupportedVersionError` (both carry user-facing `message`s). All four error
  classes and `PairingAddFailureReason` are defined in
  `packages/client-runtime/src/connection/pairingAdd.ts` (`PairingCodeParseError` /
  `PairingCodeUnsupportedVersionError` in `packages/shared/src/pairingCode.ts`) — import
  them (re-exported via `@bibcode/client-runtime/connection`; add that re-export line if
  Phase 3 did not), never re-declare them.

## File structure

Created:

- `apps/web/src/components/ui/tabs.tsx` (+ `tabs.test.tsx`) — Base UI tabs wrapper.
- `apps/web/src/components/settings/remote-servers/RemoteServersSettings.tsx` — tab shell.
- `apps/web/src/components/settings/remote-servers/ConnectTab.tsx` — Connect-tab content.
- `apps/web/src/components/settings/remote-servers/ShareTab.tsx` — share-side content
  (verbatim move; Phase 5's file).
- `apps/web/src/components/settings/remote-servers/shared.tsx` — helpers/constants shared by
  both tabs + `remoteServersSettingsInternals` test export.
- `apps/web/src/components/settings/remote-servers/connectPresentation.ts` (+ test) — pure
  row/badge/failure-copy helpers.
- `apps/web/src/components/settings/remote-servers/testHarness.tsx` — the module-mock harness
  shared by the ConnectTab/ShareTab/RemoteServersSettings test files.
- `apps/web/src/routes/settings.remote-servers.tsx` (+ test), `settings.local-environment.tsx`
  (+ test).
- `apps/web/src/desktopDeepLink.ts` (+ test) — `bibcode://pair` URL parsing + root-route
  subscription component.
- `apps/web/src/components/auth/pairingCodeCredential.ts` (+ test) — embedded-token
  extraction for the fresh-device `/pair?code=…` path.
- `apps/desktop/src-tauri/tests/deep_link_config.rs` — config parity test.

Modified: `apps/web/src/routes/settings.connections.tsx` (becomes a redirect),
`apps/web/src/routes/pair.tsx`, `apps/web/src/routes/__root.tsx`,
`apps/web/src/components/auth/PairingRouteSurface.tsx` (optional `initialCredential`),
`apps/web/src/components/settings/SettingsSidebarNav.tsx`,
`apps/web/src/connection/environmentPresentationPolicy.ts`,
`apps/web/src/connection/onboarding.ts`, `apps/web/src/connection/catalog.ts`
(connect/disconnect command atoms),
`packages/client-runtime/src/connection/registry.ts` (connect/disconnect passthroughs),
`apps/web/src/components/Sidebar.tsx`,
`apps/web/src/components/ChatView.tsx`, `apps/web/src/routes/_chat.index.tsx`,
`packages/contracts/src/ipc.ts`, `apps/web/src/tauriDesktopBridge.ts`,
`apps/desktop/src-tauri/src/lib.rs`, `apps/desktop/src-tauri/Cargo.toml`, root `Cargo.toml`,
`apps/desktop/src-tauri/tauri.conf.json`, `apps/desktop/src-tauri/capabilities/default.json`,
docs + runbooks.

Deleted (after the split): `apps/web/src/components/settings/ConnectionsSettings.tsx` and
`ConnectionsSettings.test.tsx` (content redistributed). `ConnectionsSettings.logic.ts` and its
test **stay untouched** — they hold WSL selection logic consumed by
`LocalEnvironmentSettings.tsx`, which this phase does not change.

`apps/web/src/routeTree.gen.ts` is **generated** by `@tanstack/router-plugin` whenever the
apps/web Vite config loads — i.e. during `vp test run …`, `vp dev`, or
`vp run --filter @bibcode/web build`. After adding/removing route files, run the focused route
tests (or `vp run --filter @bibcode/web build` once if the tree did not regenerate under test
mode), confirm `apps/web/src/routeTree.gen.ts` changed in `git status`, and **commit it** with
the route change. Never hand-edit it.

Test invocation used throughout (run from the repository root):
`vp test run <space-separated test file paths>`.

---

### Task 1: Connect-tab presentation helpers (pure logic)

**Files:**

- Create: `apps/web/src/components/settings/remote-servers/connectPresentation.ts`
- Test: `apps/web/src/components/settings/remote-servers/connectPresentation.test.ts`

**Interfaces:**

- Consumes: `CompatVerdict` from `@bibcode/client-runtime/connection/compat` (Phase 2,
  spec §4.4); `PairingAddFailureReason` (type-only) from `@bibcode/client-runtime/connection`
  (Phase 3, `connection/pairingAdd.ts`); `EnvironmentPresentation` from `~/state/environments`
  (existing, includes `entry`, `relayManaged`); required-nullable `hostKey: string | null` on
  `BearerConnectionProfile` (Phase 3).
- Produces (used by Tasks 5, 5b, 6, 8):
  - `formatServerVersionLabel(serverVersion: string | null | undefined): string | null`
  - `describeCompatBadge(verdict: CompatVerdict | null): CompatBadge` where
    `CompatBadge = { readonly tone: "warning" | "destructive"; readonly label: string } | null`
  - `resolveTransportBadge(environment: TransportBadgeInput): TransportBadge | null` where
    `TransportBadge = { readonly kind: "e2ee" | "ssh" | "relay"; readonly label: string } |
    { readonly kind: "unencrypted"; readonly label: string; readonly guidance: string }`
  - `ADD_SERVER_FAILURE_REASONS: ReadonlyArray<PairingAddFailureReason>` (the five
    spec-pinned reasons, typed against Phase 3's union so drift fails typecheck)
  - `describeAddServerFailure(reason: PairingAddFailureReason): { readonly title: string; readonly detail: string }`
  - `resolvePairingAddFailureReason(error: unknown): PairingAddFailureReason | null`
    (reads the `reason` field off a `PairingAddError`-tagged value — never `kind`)
  - `isLoopbackAcknowledgementRequired(error: unknown): boolean`
    (`PairingLoopbackAcknowledgementRequiredError` tag check)
  - `normalizePairingCodeInput(value: string): string | null` (input hygiene: trims and
    unwraps the two URL forms to the bare code for stable command keys and prefills; the
    canonical parser is Phase 3's `parsePairingCode` in `@bibcode/shared/pairingCode` — the
    accepted forms here must stay a subset of what it accepts)

- [x] **Step 1: Write the failing test**

```ts
// apps/web/src/components/settings/remote-servers/connectPresentation.test.ts
import { describe, expect, it } from "vite-plus/test";

import {
  ADD_SERVER_FAILURE_REASONS,
  describeAddServerFailure,
  describeCompatBadge,
  formatServerVersionLabel,
  isLoopbackAcknowledgementRequired,
  normalizePairingCodeInput,
  resolvePairingAddFailureReason,
  resolveTransportBadge,
} from "./connectPresentation";

describe("formatServerVersionLabel", () => {
  it("renders the D16 version string and hides unknown versions", () => {
    expect(formatServerVersionLabel("1.4.2")).toBe("BiBCode v1.4.2");
    expect(formatServerVersionLabel("  ")).toBeNull();
    expect(formatServerVersionLabel(null)).toBeNull();
    expect(formatServerVersionLabel(undefined)).toBeNull();
  });
});

describe("describeCompatBadge", () => {
  it("maps every verdict kind to the pinned copy", () => {
    expect(describeCompatBadge(null)).toBeNull();
    expect(describeCompatBadge({ kind: "compatible" })).toBeNull();
    expect(describeCompatBadge({ kind: "legacy" })).toEqual({
      tone: "warning",
      label: "Limited compatibility",
    });
    expect(
      describeCompatBadge({ kind: "server-too-old", serverVersion: 0, minSupported: 1 }),
    ).toEqual({ tone: "destructive", label: "Server update required" });
    expect(
      describeCompatBadge({ kind: "client-too-old", serverMinCompatible: 2, clientVersion: 1 }),
    ).toEqual({ tone: "destructive", label: "App update required" });
  });
});

describe("resolveTransportBadge", () => {
  const bearer = (profile: unknown) => ({
    relayManaged: false,
    entry: {
      target: { _tag: "BearerConnectionTarget", connectionId: "bearer:x" },
      profile: profile === null ? { _tag: "None" } : { _tag: "Some", value: profile },
    },
  });

  it("labels relay, ssh, e2ee, and legacy-unencrypted saved servers", () => {
    expect(
      resolveTransportBadge({
        relayManaged: true,
        entry: { target: { _tag: "RelayConnectionTarget" }, profile: { _tag: "None" } },
      }),
    ).toEqual({ kind: "relay", label: "BiBCode Connect" });
    expect(
      resolveTransportBadge({
        relayManaged: false,
        entry: { target: { _tag: "SshConnectionTarget" }, profile: { _tag: "None" } },
      }),
    ).toEqual({ kind: "ssh", label: "SSH tunnel" });
    expect(
      resolveTransportBadge(
        bearer({
          _tag: "BearerConnectionProfile",
          hostKey: "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        }),
      ),
    ).toEqual({ kind: "e2ee", label: "End-to-end encrypted" });
    expect(
      resolveTransportBadge(bearer({ _tag: "BearerConnectionProfile", hostKey: null })),
    ).toEqual({
      kind: "unencrypted",
      label: "Unencrypted",
      guidance: "Re-pair with a new pairing code to secure this connection.",
    });
  });

  it("shows no transport badge for the desktop-managed local (WSL) environment", () => {
    expect(
      resolveTransportBadge({
        relayManaged: false,
        entry: {
          target: { _tag: "BearerConnectionTarget", connectionId: "local:wsl" },
          profile: { _tag: "None" },
        },
      }),
    ).toBeNull();
  });
});

describe("add-server failure copy", () => {
  it("has copy for all five spec-pinned failure reasons", () => {
    expect(ADD_SERVER_FAILURE_REASONS).toEqual([
      "unreachable",
      "host-identity-mismatch",
      "pairing-rejected",
      "incompatible",
      "duplicate-storage-identity",
    ]);
    for (const reason of ADD_SERVER_FAILURE_REASONS) {
      const described = describeAddServerFailure(reason);
      expect(described.title.length).toBeGreaterThan(0);
      expect(described.detail.length).toBeGreaterThan(0);
    }
    expect(describeAddServerFailure("pairing-rejected").title).toBe("Pairing rejected");
  });

  it("reads the reason off a PairingAddError and rejects everything else", () => {
    expect(
      resolvePairingAddFailureReason({
        _tag: "PairingAddError",
        reason: "host-identity-mismatch",
        detail: "pinned key changed",
      }),
    ).toBe("host-identity-mismatch");
    expect(resolvePairingAddFailureReason(new Error("boom"))).toBeNull();
    expect(
      resolvePairingAddFailureReason({ _tag: "PairingAddError", reason: "something-else" }),
    ).toBeNull();
    expect(resolvePairingAddFailureReason({ kind: "pairing-rejected" })).toBeNull();
  });

  it("detects the loopback-acknowledgement error by tag", () => {
    expect(
      isLoopbackAcknowledgementRequired({
        _tag: "PairingLoopbackAcknowledgementRequiredError",
        endpoint: "http://127.0.0.1:3773",
      }),
    ).toBe(true);
    expect(
      isLoopbackAcknowledgementRequired({ _tag: "PairingAddError", reason: "unreachable" }),
    ).toBe(false);
    expect(isLoopbackAcknowledgementRequired(new Error("boom"))).toBe(false);
  });
});

describe("normalizePairingCodeInput", () => {
  it("accepts raw codes, deep links, and web pair URLs", () => {
    expect(normalizePairingCodeInput("  abc123-_  ")).toBe("abc123-_");
    expect(normalizePairingCodeInput("bibcode://pair?code=abc123-_")).toBe("abc123-_");
    expect(normalizePairingCodeInput("http://192.168.1.20:3773/pair?code=abc123-_")).toBe(
      "abc123-_",
    );
    expect(normalizePairingCodeInput("")).toBeNull();
    expect(normalizePairingCodeInput("bibcode://pair")).toBeNull();
    expect(normalizePairingCodeInput("http://example.com/other?code=x")).toBe("x");
  });
});
```

- [x] **Step 2: Run the test to verify it fails**

Run: `vp test run apps/web/src/components/settings/remote-servers/connectPresentation.test.ts`
Expected: FAIL — module `./connectPresentation` does not exist.

- [x] **Step 3: Write the implementation**

```ts
// apps/web/src/components/settings/remote-servers/connectPresentation.ts
import type { CompatVerdict } from "@bibcode/client-runtime/connection/compat";
import type { PairingAddFailureReason } from "@bibcode/client-runtime/connection";

/** D16: version strings render as "BiBCode v<serverVersion>". */
export function formatServerVersionLabel(serverVersion: string | null | undefined): string | null {
  const trimmed = serverVersion?.trim() ?? "";
  return trimmed.length > 0 ? `BiBCode v${trimmed}` : null;
}

export type CompatBadge = {
  readonly tone: "warning" | "destructive";
  readonly label: string;
} | null;

export function describeCompatBadge(verdict: CompatVerdict | null): CompatBadge {
  if (verdict === null) return null;
  switch (verdict.kind) {
    case "compatible":
      return null;
    case "legacy":
      return { tone: "warning", label: "Limited compatibility" };
    case "server-too-old":
      return { tone: "destructive", label: "Server update required" };
    case "client-too-old":
      return { tone: "destructive", label: "App update required" };
  }
}

/**
 * Structural input so the helper stays pure and unit-testable without
 * constructing full catalog entries. `EnvironmentPresentation` from
 * `~/state/environments` satisfies it directly.
 */
export interface TransportBadgeInput {
  readonly relayManaged: boolean;
  readonly entry: {
    readonly target: { readonly _tag: string; readonly connectionId?: string };
    readonly profile:
      | { readonly _tag: "None" }
      | {
          readonly _tag: "Some";
          // Bearer profiles carry hostKey as required-nullable (Phase 3); other
          // profile kinds (SSH) have no such field, hence optional here.
          readonly value: { readonly _tag: string; readonly hostKey?: string | null };
        };
  };
}

export type TransportBadge =
  | { readonly kind: "e2ee" | "ssh" | "relay"; readonly label: string }
  | { readonly kind: "unencrypted"; readonly label: string; readonly guidance: string };

export function resolveTransportBadge(environment: TransportBadgeInput): TransportBadge | null {
  if (environment.relayManaged) return { kind: "relay", label: "BiBCode Connect" };
  const target = environment.entry.target;
  if (target._tag === "SshConnectionTarget") return { kind: "ssh", label: "SSH tunnel" };
  if (target._tag !== "BearerConnectionTarget") return null;
  // Desktop-managed local backends (WSL) surface as bearer targets with a
  // "local:" connection-id prefix; they are not remote transports.
  if (target.connectionId?.startsWith("local:")) return null;
  const profile = environment.entry.profile;
  const hostKey =
    profile._tag === "Some" && profile.value._tag === "BearerConnectionProfile"
      ? (profile.value.hostKey ?? null)
      : null;
  if (hostKey !== null && hostKey.length > 0) {
    return { kind: "e2ee", label: "End-to-end encrypted" };
  }
  return {
    kind: "unencrypted",
    label: "Unencrypted",
    guidance: "Re-pair with a new pairing code to secure this connection.",
  };
}

// The five reasons are spec §4.2-pinned; typing the list against Phase 3's
// PairingAddFailureReason union makes any drift a typecheck failure here.
export const ADD_SERVER_FAILURE_REASONS: ReadonlyArray<PairingAddFailureReason> = [
  "unreachable",
  "host-identity-mismatch",
  "pairing-rejected",
  "incompatible",
  "duplicate-storage-identity",
];

/** Reads the classified reason off Phase 3's PairingAddError (field: `reason`). */
export function resolvePairingAddFailureReason(error: unknown): PairingAddFailureReason | null {
  if (error === null || typeof error !== "object") return null;
  if ((error as { _tag?: unknown })._tag !== "PairingAddError") return null;
  const reason = (error as { reason?: unknown }).reason;
  return (ADD_SERVER_FAILURE_REASONS as readonly unknown[]).includes(reason)
    ? (reason as PairingAddFailureReason)
    : null;
}

/** Phase 3's distinct tunnel-acknowledgement error (spec §4.2). */
export function isLoopbackAcknowledgementRequired(error: unknown): boolean {
  return (
    error !== null &&
    typeof error === "object" &&
    (error as { _tag?: unknown })._tag === "PairingLoopbackAcknowledgementRequiredError"
  );
}

export function describeAddServerFailure(reason: PairingAddFailureReason): {
  readonly title: string;
  readonly detail: string;
} {
  switch (reason) {
    case "unreachable":
      return {
        title: "Server unreachable",
        detail:
          "Could not reach the server at the pairing code's address. Check that the server is running and that this device can reach its network.",
      };
    case "host-identity-mismatch":
      return {
        title: "Host identity changed",
        detail:
          "The server's identity key does not match this pairing code. Generate a fresh pairing code on the server and try again.",
      };
    case "pairing-rejected":
      return {
        title: "Pairing rejected",
        detail:
          "The server rejected this pairing code. Codes are single-use and expire — generate a new one on the server.",
      };
    case "incompatible":
      return {
        title: "Versions incompatible",
        detail:
          "This app and the server cannot talk to each other. Update the older side, then retry.",
      };
    case "duplicate-storage-identity":
      return {
        title: "Server already saved",
        detail:
          "A saved server already uses this server's storage identity. Reconnect or adopt the existing entry instead of adding a duplicate.",
      };
  }
}

/**
 * Accepts what a user is likely to paste: the bare base64url code, the
 * `bibcode://pair?code=…` deep link, or an `http(s)://…/pair?code=…` URL.
 */
export function normalizePairingCodeInput(value: string): string | null {
  const trimmed = value.trim();
  if (trimmed.length === 0) return null;
  if (/^[A-Za-z0-9_-]+$/u.test(trimmed)) return trimmed;
  try {
    const url = new URL(trimmed);
    const code = url.searchParams.get("code")?.trim() ?? "";
    return code.length > 0 ? code : null;
  } catch {
    return null;
  }
}
```

Note: if Phase 2 exported `CompatVerdict` from a different subpath than
`@bibcode/client-runtime/connection/compat`, import from the landed subpath — check
`packages/client-runtime/package.json` `exports` (it uses explicit subpath exports).

- [x] **Step 4: Run the test to verify it passes**

Run: `vp test run apps/web/src/components/settings/remote-servers/connectPresentation.test.ts`
Expected: PASS (all suites).

- [x] **Step 5: Commit**

```bash
git add apps/web/src/components/settings/remote-servers/connectPresentation.ts \
  apps/web/src/components/settings/remote-servers/connectPresentation.test.ts
git commit -m "feat(web): add Remote Servers connect-tab presentation helpers"
```

---

### Task 2: Tabs UI primitive

**Files:**

- Create: `apps/web/src/components/ui/tabs.tsx`
- Test: `apps/web/src/components/ui/tabs.test.tsx`

**Interfaces:**

- Consumes: `@base-ui/react/tabs` (repo already depends on `@base-ui/react@1.6.0` and wraps
  its primitives one file per component in `apps/web/src/components/ui/`; `dialog.tsx` is the
  pattern to imitate).
- Produces: `Tabs`, `TabsList`, `TabsTab`, `TabsPanel` — used by Task 3's shell.

First run `node -e "console.log(require.resolve('@base-ui/react/tabs', { paths: ['apps/web'] }))"`
to confirm the subpath exists in the installed version. If it does not resolve, do **not**
add a dependency: build the same four exports as an aria-correct manual implementation
(`role="tablist"` / `role="tab"` / `aria-selected` / `role="tabpanel"` with `hidden` on
unselected panels) with the identical props contract below, and note the substitution in the
final report.

- [x] **Step 1: Write the failing test**

```tsx
// apps/web/src/components/ui/tabs.test.tsx
// @vitest-environment happy-dom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { describe, expect, it } from "vite-plus/test";

import { Tabs, TabsList, TabsPanel, TabsTab } from "./tabs";

describe("Tabs", () => {
  it("renders the selected panel and switches on tab activation", async () => {
    const container = document.createElement("div");
    document.body.appendChild(container);
    const root = createRoot(container);
    await act(async () => {
      root.render(
        <Tabs defaultValue="one">
          <TabsList>
            <TabsTab value="one">One</TabsTab>
            <TabsTab value="two">Two</TabsTab>
          </TabsList>
          <TabsPanel value="one">first panel</TabsPanel>
          <TabsPanel value="two">second panel</TabsPanel>
        </Tabs>,
      );
    });

    expect(container.textContent).toContain("first panel");
    expect(container.textContent).not.toContain("second panel");

    const tabs = container.querySelectorAll('[role="tab"]');
    expect(tabs).toHaveLength(2);
    await act(async () => {
      (tabs[1] as HTMLElement).click();
    });
    expect(container.textContent).toContain("second panel");

    await act(async () => {
      root.unmount();
    });
    container.remove();
  });
});
```

- [x] **Step 2: Run the test to verify it fails**

Run: `vp test run apps/web/src/components/ui/tabs.test.tsx`
Expected: FAIL — module `./tabs` does not exist.

- [x] **Step 3: Write the implementation**

```tsx
// apps/web/src/components/ui/tabs.tsx
"use client";

import { Tabs as TabsPrimitive } from "@base-ui/react/tabs";
import type { ComponentProps } from "react";

import { cn } from "~/lib/utils";

function Tabs({ className, ...props }: ComponentProps<typeof TabsPrimitive.Root>) {
  return <TabsPrimitive.Root className={cn("flex min-w-0 flex-col gap-6", className)} {...props} />;
}

function TabsList({ className, ...props }: ComponentProps<typeof TabsPrimitive.List>) {
  return (
    <TabsPrimitive.List
      className={cn(
        "flex w-fit items-center gap-1 rounded-lg border border-border/60 bg-muted/40 p-1",
        className,
      )}
      {...props}
    />
  );
}

function TabsTab({ className, ...props }: ComponentProps<typeof TabsPrimitive.Tab>) {
  return (
    <TabsPrimitive.Tab
      className={cn(
        "rounded-md px-3 py-1.5 text-[13px] font-medium text-muted-foreground transition-colors",
        "hover:text-foreground data-selected:bg-background data-selected:text-foreground data-selected:shadow-sm",
        className,
      )}
      {...props}
    />
  );
}

function TabsPanel({ className, ...props }: ComponentProps<typeof TabsPrimitive.Panel>) {
  return (
    <TabsPrimitive.Panel className={cn("flex min-w-0 flex-col gap-8", className)} {...props} />
  );
}

export { Tabs, TabsList, TabsPanel, TabsTab };
```

- [x] **Step 4: Run the test to verify it passes**

Run: `vp test run apps/web/src/components/ui/tabs.test.tsx`
Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add apps/web/src/components/ui/tabs.tsx apps/web/src/components/ui/tabs.test.tsx
git commit -m "feat(web): add tabs UI primitive"
```

---

### Task 3: Split ConnectionsSettings into the remote-servers module (mechanical move + tab shell)

This is the D7 "evolution in place" restructuring. **No behavior change** beyond placing the
existing content under two tabs; every moved symbol keeps its implementation byte-for-byte
unless an import path forces an edit. The old route keeps working at the end of this task
(routes move in Task 4). Line numbers refer to
`apps/web/src/components/settings/ConnectionsSettings.tsx` at commit time of this plan.

**Files:**

- Create: `apps/web/src/components/settings/remote-servers/shared.tsx`
- Create: `apps/web/src/components/settings/remote-servers/ConnectTab.tsx`
- Create: `apps/web/src/components/settings/remote-servers/ShareTab.tsx`
- Create: `apps/web/src/components/settings/remote-servers/RemoteServersSettings.tsx`
- Create: `apps/web/src/components/settings/remote-servers/testHarness.tsx`
- Create: `apps/web/src/components/settings/remote-servers/RemoteServersSettings.test.tsx`
- Create: `apps/web/src/components/settings/remote-servers/ConnectTab.test.tsx`
- Create: `apps/web/src/components/settings/remote-servers/ShareTab.test.tsx`
- Delete: `apps/web/src/components/settings/ConnectionsSettings.tsx`,
  `apps/web/src/components/settings/ConnectionsSettings.test.tsx`
- Modify: `apps/web/src/routes/settings.connections.tsx` (import path only, this task)
- Move: `apps/web/src/components/settings/pairingUrls.ts` + `pairingUrls.test.ts` →
  `apps/web/src/components/settings/remote-servers/` (share-side helper; update its two
  importers, which are the moved share components themselves)

**Interfaces:**

- Consumes: everything the current `ConnectionsSettings.tsx` imports (unchanged).
- Produces (relied on by Tasks 4–8):
  - `RemoteServersSettings(props: { initialTab?: "connect" | "share" })` from
    `remote-servers/RemoteServersSettings.tsx`
  - `ConnectTab(props: {})` and `ShareTab(props: {})` (props grow in later tasks)
  - `remoteServersSettingsInternals` from `remote-servers/shared.tsx` (renamed from
    `connectionsSettingsInternals` — no compatibility alias)
  - `ITEM_ROW_CLASSNAME`, `ITEM_ROW_INNER_CLASSNAME` exported from `shared.tsx`

**Moved-symbol map** (source line → destination):

| Symbols (current lines)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                     | Destination                                         |
| ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------- |
| `DEFAULT_TAILSCALE_SERVE_PORT`, `EMPTY_ADVERTISED_ENDPOINTS`, `EMPTY_DISCOVERED_SSH_HOSTS` (146–148), `accessTimestampFormatter`/`formatAccessTimestamp` (150–209), `AccessScopeSummary` (210–253), `ConnectionStatusDot` + props (254–307), `formatDesktopSshTarget` (308), `parseManualDesktopSshTarget` (313), `parsePairingUrlFields` (371), `parseRemotePairingFields` (402), `formatDesktopSshConnectionError` (420), `AccessSectionPresentation`/`accessRowClassName`/`endpointRowClassName` (438–454), `ITEM_ROW_CLASSNAME` (432), `ITEM_ROW_INNER_CLASSNAME` (435), sort/record mappers (455–492), `selectPairingEndpoint` (493), `isTailscaleHttpsEndpoint` (514), `endpointDefaultPreferenceKey` (518), `resolveAdvertisedEndpointPairingUrl` (542), `resolveCurrentOriginPairingUrl` (555), `isHostedAppPairingUrl` (560), internals export (570–588, renamed `remoteServersSettingsInternals`) | `shared.tsx`                                        |
| `PairingLinkListRow` (589–973), `ConnectedClientListRow` (974–1053), `AuthorizedClientsHeaderAction` (1054–1227), `PairingClientsList` (1228–1288), `AdvertisedEndpointListRow` (1289–1374), `NetworkAccessDescription` (1375–1421), `CloudLinkSwitch` (1611), `ConfiguredCloudLinkRow` (1640), `CloudLinkRow` (1772)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       | `ShareTab.tsx`                                      |
| `SavedBackendListRow` (1422–1570), `DesktopSshHostRow` (1571–1609), `EmptyRemoteEnvironments` (1776), `RemoteEnvironmentRowsSkeleton` (1794), `ConfiguredCloudRemoteEnvironmentRows` (1808), `CloudRemoteEnvironmentRows` (1951)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            | `ConnectTab.tsx`                                    |
| `FullConnectionsSettings` (1968–3202)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       | split: see below                                    |
| `ConnectionsSettings` dispatch (3204–3213)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  | replaced by `RemoteServersSettings` + Task 4 routes |

**`FullConnectionsSettings` split** — move hooks/state/handlers to the tab that renders them:

- To **`ConnectTab`**: `useEnvironments`, `connectPairing`/`connectSshEnvironment` atom
  commands, `environmentCatalog.remove`/`retryNow`, `savedEnvironments` +
  `savedEnvironmentIds` + `savedDesktopSshEnvironmentsByAlias` +
  `savedDesktopSshEnvironmentKeys` memos, `sshConnectionError`/`connectingSshHostAlias`,
  all `savedBackend*` dialog state (2048–2058), `desktopSshHosts` query +
  `unsavedDiscoveredSshHosts`/`hasLoaded…`/`isLoading…` derivations, handlers
  `handleAddSavedBackend`, `handleConnectSavedBackend`, `handleRemoveSavedBackend`,
  `handleConnectSshHost`, `handleSavedBackendHostChange`, render helpers
  `renderConnectionModeCard`, `renderRemoteFields`, `renderRemoteModeBody`,
  `renderSshFields`, and the "Remote environments" `SettingsSection` JSX (3122–3199).
- To **`ShareTab`**: `usePrimaryEnvironment`, `usePrimarySessionState`,
  `currentSessionScopes`/`currentAuthPolicy`/`canManageLocalBackend`/`canManageRelay`,
  `authAccessChanges` + `desktopNetworkAccess` queries, all exposure/Tailscale/pairing-link/
  client-session state and handlers (2035–2047, 2059–2079, 2114–2335, 2523–2566),
  `primaryVersionMismatch`, render helpers `renderNetworkAccessToggle`,
  `renderEndpointRows`, `renderTailscaleRow`, `renderAuthorizedClients`,
  `renderNetworkAccessRow`, `renderDisabledNetworkAccessRow`, and the JSX for
  "This environment", "Authorized clients", the three exposure/Tailscale dialogs, and the
  non-admin fallback section (2901–3120).
- `desktopBridge = window.desktopBridge` is read independently in each tab.
- The Share tab keeps the section title "This environment" and all existing copy verbatim —
  Phase 5 owns its evolution.

- [x] **Step 1: Write the failing shell test**

```tsx
// apps/web/src/components/settings/remote-servers/RemoteServersSettings.test.tsx
// @vitest-environment happy-dom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { describe, expect, it, vi } from "vite-plus/test";

vi.mock("./ConnectTab", () => ({ ConnectTab: () => <div data-testid="connect-tab" /> }));
vi.mock("./ShareTab", () => ({ ShareTab: () => <div data-testid="share-tab" /> }));

import { RemoteServersSettings } from "./RemoteServersSettings";

async function render(element: React.ReactElement) {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  await act(async () => {
    root.render(element);
  });
  return { container, cleanup: () => act(async () => root.unmount()) };
}

describe("RemoteServersSettings", () => {
  it("renders both spec-named tabs with Connect selected by default", async () => {
    const { container, cleanup } = await render(<RemoteServersSettings />);
    const tabLabels = [...container.querySelectorAll('[role="tab"]')].map((tab) => tab.textContent);
    expect(tabLabels).toEqual(["Connect to a host", "Share this host"]);
    expect(container.querySelector('[data-testid="connect-tab"]')).not.toBeNull();
    expect(container.querySelector('[data-testid="share-tab"]')).toBeNull();
    await cleanup();
  });

  it("honors initialTab=share so deep links can land on the Share tab", async () => {
    const { container, cleanup } = await render(<RemoteServersSettings initialTab="share" />);
    expect(container.querySelector('[data-testid="share-tab"]')).not.toBeNull();
    await cleanup();
  });
});
```

- [x] **Step 2: Run it to verify it fails**

Run: `vp test run apps/web/src/components/settings/remote-servers/RemoteServersSettings.test.tsx`
Expected: FAIL — modules do not exist.

- [x] **Step 3: Create the shell and perform the mechanical split**

Shell (new code):

```tsx
// apps/web/src/components/settings/remote-servers/RemoteServersSettings.tsx
import { useState } from "react";

import { SettingsPageContainer } from "../settingsLayout";
import { Tabs, TabsList, TabsPanel, TabsTab } from "../../ui/tabs";
import { ConnectTab } from "./ConnectTab";
import { ShareTab } from "./ShareTab";

export type RemoteServersTab = "connect" | "share";

export function RemoteServersSettings({
  initialTab = "connect",
}: {
  readonly initialTab?: RemoteServersTab;
}) {
  const [tab, setTab] = useState<RemoteServersTab>(initialTab);
  return (
    <SettingsPageContainer>
      <Tabs value={tab} onValueChange={(value) => setTab(value === "share" ? "share" : "connect")}>
        <TabsList>
          <TabsTab value="connect">Connect to a host</TabsTab>
          <TabsTab value="share">Share this host</TabsTab>
        </TabsList>
        <TabsPanel value="connect">
          <ConnectTab />
        </TabsPanel>
        <TabsPanel value="share">
          <ShareTab />
        </TabsPanel>
      </Tabs>
    </SettingsPageContainer>
  );
}
```

`ConnectTab.tsx` / `ShareTab.tsx` skeletons (moved bodies fill the sections):

```tsx
// apps/web/src/components/settings/remote-servers/ConnectTab.tsx (skeleton)
export function ConnectTab() {
  // …moved hooks/state/handlers per the split table above…
  return <>{/* moved "Remote environments" SettingsSection, retitled in Task 5 */}</>;
}
```

```tsx
// apps/web/src/components/settings/remote-servers/ShareTab.tsx (skeleton)
export function ShareTab() {
  // …moved share-side hooks/state/handlers per the split table above…
  return (
    <>{/* moved "This environment" + "Authorized clients" sections and dialogs, verbatim */}</>
  );
}
```

Then:

1. Move every symbol per the map, fixing relative imports (`../../hooks/…` → `../../../hooks/…`
   is wrong — the new directory is one level deeper than `settings/`, so `../ui/…` becomes
   `../../ui/…`, `../../lib/utils` becomes `../../../lib/utils`, `~/`-prefixed imports are
   unchanged). Move `pairingUrls.ts` (+ its test) into `remote-servers/` and update its
   imports (`../../hostedPairing` → `../../../hostedPairing`, `../../pairingUrl` →
   `../../../pairingUrl`).
2. Rename `connectionsSettingsInternals` → `remoteServersSettingsInternals` in `shared.tsx`.
3. Point the existing route at the new shell (routes fully move in Task 4):

```tsx
// apps/web/src/routes/settings.connections.tsx — this task, minimal edit:
import { RemoteServersSettings } from "../components/settings/remote-servers/RemoteServersSettings";
import { LocalEnvironmentSettings } from "../components/settings/LocalEnvironmentSettings";
// keep the existing beforeLoad/connectionsRouteDestination logic for now, and replace the
// component with an inline dispatch identical to the old ConnectionsSettings():
function ConnectionsRouteComponent() {
  const policy = readCurrentEnvironmentPresentationPolicy();
  if (policy.connectionsPresentation === "local-wsl") return <LocalEnvironmentSettings />;
  if (policy.connectionsPresentation === "redirect-general") return null;
  return <RemoteServersSettings />;
}
```

4. Delete `ConnectionsSettings.tsx`.
5. Split `ConnectionsSettings.test.tsx`: extract its `vi.hoisted` harness + `vi.mock` block +
   mount/teardown utilities into `remote-servers/testHarness.tsx` (exported as-is; add the
   mock for `~/connection/onboarding` symbols it already contains), then distribute the test
   cases: share-side cases (pairing links, client sessions, endpoints, Tailscale, exposure,
   cloud link) render `<ShareTab />` directly in `ShareTab.test.tsx`; connect-side cases
   (saved rows, add dialog, SSH hosts) render `<ConnectTab />` in `ConnectTab.test.tsx`.
   Assertions stay identical — only the rendered component and the internals import
   (`remoteServersSettingsInternals` from `./shared`) change. Move the
   `ConnectionsSettings.logic.test.ts`-adjacent pure-helper cases that exercised
   `connectionsSettingsInternals` into `ConnectTab.test.tsx`/`ShareTab.test.tsx` as
   appropriate. Delete `ConnectionsSettings.test.tsx`.

- [x] **Step 4: Run the moved suites to verify they pass**

Run:

```
vp test run apps/web/src/components/settings/remote-servers/RemoteServersSettings.test.tsx \
  apps/web/src/components/settings/remote-servers/ConnectTab.test.tsx \
  apps/web/src/components/settings/remote-servers/ShareTab.test.tsx \
  apps/web/src/components/settings/remote-servers/pairingUrls.test.ts \
  apps/web/src/routes/settings.connections.test.tsx
```

Expected: PASS. Then `vp run typecheck` — expected clean (this is the guard against missed
import-path fixes).

- [x] **Step 5: Commit**

```bash
git add -A apps/web/src/components/settings apps/web/src/routes/settings.connections.tsx
git commit -m "refactor(web): split Connections settings into remote-servers tab modules"
```

---

### Task 4: Rename to Remote Servers — routes, redirect, nav, links, policy

**Files:**

- Create: `apps/web/src/routes/settings.remote-servers.tsx`
- Create: `apps/web/src/routes/settings.remote-servers.test.tsx`
- Create: `apps/web/src/routes/settings.local-environment.tsx`
- Create: `apps/web/src/routes/settings.local-environment.test.tsx`
- Modify: `apps/web/src/routes/settings.connections.tsx` (becomes a pure redirect),
  `apps/web/src/routes/settings.connections.test.tsx`
- Modify: `apps/web/src/components/settings/SettingsSidebarNav.tsx` (+ its test)
- Modify: `apps/web/src/connection/environmentPresentationPolicy.ts` (+ any test asserting
  `connectionsPresentation` — update alongside)
- Modify: `apps/web/src/components/Sidebar.tsx:4077`, `apps/web/src/components/ChatView.tsx:2360`,
  `apps/web/src/routes/_chat.index.tsx:63`
- Generated: `apps/web/src/routeTree.gen.ts` (commit the regenerated file)

**Interfaces:**

- Consumes: `RemoteServersSettings({ initialTab? })` (Task 3);
  `LocalEnvironmentSettings` (existing); `readCurrentEnvironmentPresentationPolicy` (existing).
- Produces: routes `/settings/remote-servers` (search: `{ tab?: "share"; code?: string }` —
  `code` consumed in Task 8), `/settings/local-environment`; policy without
  `connectionsPresentation`; nav item `{ label: "Remote Servers", to: "/settings/remote-servers" }`.

- [x] **Step 1: Write the failing route tests**

```tsx
// apps/web/src/routes/settings.remote-servers.test.tsx
import { describe, expect, it } from "vite-plus/test";

import { Route } from "./settings.remote-servers";

describe("/settings/remote-servers", () => {
  it("keeps only recognized search params", () => {
    const validate = Route.options.validateSearch;
    if (typeof validate !== "function") throw new Error("validateSearch is not registered.");
    expect(validate({ tab: "share", code: "abc", junk: "x" })).toEqual({
      tab: "share",
      code: "abc",
    });
    expect(validate({ tab: "bogus", code: "" })).toEqual({});
    expect(validate({})).toEqual({});
  });
});
```

```tsx
// apps/web/src/routes/settings.local-environment.test.tsx
import { describe, expect, it, vi } from "vite-plus/test";

import { createEnvironmentPresentationPolicy } from "~/connection/environmentPresentationPolicy";

const h = vi.hoisted(() => ({
  policy: null as ReturnType<
    typeof import("~/connection/environmentPresentationPolicy").createEnvironmentPresentationPolicy
  > | null,
}));

vi.mock("~/connection/currentEnvironmentPresentation", () => ({
  readCurrentEnvironmentPresentationPolicy: () => h.policy,
}));

import { Route } from "./settings.local-environment";

describe("/settings/local-environment", () => {
  it("redirects to Remote Servers unless the WSL local page applies", async () => {
    const beforeLoad = Route.options.beforeLoad;
    if (typeof beforeLoad !== "function") throw new Error("beforeLoad is not registered.");

    h.policy = createEnvironmentPresentationPolicy({ surface: "desktop", platform: "windows" });
    await expect(Promise.resolve().then(() => beforeLoad({} as never))).resolves.toBeUndefined();

    for (const input of [
      { surface: "desktop", platform: "macos" },
      { surface: "desktop", platform: "linux" },
      { surface: "browser", platform: "unknown" },
    ] as const) {
      h.policy = createEnvironmentPresentationPolicy(input);
      await expect(Promise.resolve().then(() => beforeLoad({} as never))).rejects.toMatchObject({
        options: { to: "/settings/remote-servers", replace: true },
      });
    }
  });
});
```

Rewrite `settings.connections.test.tsx` (replacing the `connectionsRouteDestination` suite):

```tsx
// apps/web/src/routes/settings.connections.test.tsx
import { describe, expect, it } from "vite-plus/test";

import { Route } from "./settings.connections";

describe("/settings/connections", () => {
  it("always redirects to /settings/remote-servers (D7)", async () => {
    const beforeLoad = Route.options.beforeLoad;
    if (typeof beforeLoad !== "function") throw new Error("beforeLoad is not registered.");
    await expect(Promise.resolve().then(() => beforeLoad({} as never))).rejects.toMatchObject({
      options: { to: "/settings/remote-servers", replace: true },
    });
  });
});
```

Update `SettingsSidebarNav.test.tsx` expectations: the base list gains
`["Remote Servers", "/settings/remote-servers"]` directly after General; the
Windows-only item becomes `["Local environment", "/settings/local-environment"]` inserted
after Remote Servers; the browser/macOS policies still exclude "Local environment".

- [x] **Step 2: Run tests to verify they fail**

Run:

```
vp test run apps/web/src/routes/settings.remote-servers.test.tsx \
  apps/web/src/routes/settings.local-environment.test.tsx \
  apps/web/src/routes/settings.connections.test.tsx \
  apps/web/src/components/settings/SettingsSidebarNav.test.tsx
```

Expected: FAIL — new route modules missing; nav expectations unmet.

- [x] **Step 3: Implement routes, nav, policy, and link updates**

```tsx
// apps/web/src/routes/settings.remote-servers.tsx
import { createFileRoute } from "@tanstack/react-router";

import { RemoteServersSettings } from "../components/settings/remote-servers/RemoteServersSettings";

export const Route = createFileRoute("/settings/remote-servers")({
  validateSearch: (search: Record<string, unknown>) => ({
    ...(search.tab === "share" ? { tab: "share" as const } : {}),
    ...(typeof search.code === "string" && search.code.length > 0 ? { code: search.code } : {}),
  }),
  component: RemoteServersRouteView,
});

function RemoteServersRouteView() {
  const { tab } = Route.useSearch();
  return <RemoteServersSettings initialTab={tab === "share" ? "share" : "connect"} />;
}
```

```tsx
// apps/web/src/routes/settings.connections.tsx — full replacement
import { createFileRoute, redirect } from "@tanstack/react-router";

export const Route = createFileRoute("/settings/connections")({
  beforeLoad: () => {
    // D7: the section evolved in place; the old path forwards permanently.
    throw redirect({ to: "/settings/remote-servers", replace: true });
  },
  component: () => null,
});
```

```tsx
// apps/web/src/routes/settings.local-environment.tsx
import { createFileRoute, redirect } from "@tanstack/react-router";

import { LocalEnvironmentSettings } from "../components/settings/LocalEnvironmentSettings";
import { readCurrentEnvironmentPresentationPolicy } from "~/connection/currentEnvironmentPresentation";

export const Route = createFileRoute("/settings/local-environment")({
  beforeLoad: () => {
    if (!readCurrentEnvironmentPresentationPolicy().showLocalEnvironmentSettings) {
      throw redirect({ to: "/settings/remote-servers", replace: true });
    }
  },
  component: LocalEnvironmentSettings,
});
```

`environmentPresentationPolicy.ts` — delete `ConnectionsPresentation` and the
`connectionsPresentation` field; derive `showLocalEnvironmentSettings` directly:

```ts
export interface EnvironmentPresentationPolicy {
  readonly surface: ClientPresentationSurface;
  readonly platform: DesktopHostPlatform;
  readonly showRemoteDeviceControls: boolean;
  readonly showLocalEnvironmentSettings: boolean;
  readonly presentsTarget: (target: ConnectionTarget) => boolean;
  readonly permitsConnectionAction: (target: ConnectionTarget) => boolean;
}

export function createEnvironmentPresentationPolicy(input: {
  readonly surface: ClientPresentationSurface;
  readonly platform: DesktopHostPlatform;
}): EnvironmentPresentationPolicy {
  const browser = input.surface === "browser";
  const presentsTarget = (target: ConnectionTarget) =>
    browser || isLocalDesktopTarget(input, target);

  return {
    ...input,
    showRemoteDeviceControls: browser,
    showLocalEnvironmentSettings: input.surface === "desktop" && input.platform === "windows",
    presentsTarget,
    permitsConnectionAction: presentsTarget,
  };
}
```

`SettingsSidebarNav.tsx`:

```ts
import { ServerIcon /* add to the existing lucide import */ } from "lucide-react";

export type SettingsSectionPath =
  | "/settings/general"
  | "/settings/remote-servers"
  | "/settings/local-environment"
  | "/settings/agents"
  | "/settings/status-bar"
  | "/settings/terminal"
  | "/settings/keybindings"
  | "/settings/providers"
  | "/settings/source-control"
  | "/settings/archived"
  | "/settings/about";

export const BASE_SETTINGS_NAV_ITEMS: ReadonlyArray<SettingsNavItem> = [
  { label: "General", to: "/settings/general", icon: Settings2Icon },
  { label: "Remote Servers", to: "/settings/remote-servers", icon: ServerIcon },
  { label: "Agents", to: "/settings/agents", icon: BotIcon },
  // …remaining items unchanged, "/settings/connections" removed…
];

const LOCAL_ENVIRONMENT_NAV_ITEM = {
  label: "Local environment",
  to: "/settings/local-environment",
  icon: MonitorIcon,
} as const;

export function settingsNavItemsFor(
  policy: EnvironmentPresentationPolicy,
): ReadonlyArray<SettingsNavItem> {
  return policy.showLocalEnvironmentSettings
    ? [
        BASE_SETTINGS_NAV_ITEMS[0]!,
        BASE_SETTINGS_NAV_ITEMS[1]!,
        LOCAL_ENVIRONMENT_NAV_ITEM,
        ...BASE_SETTINGS_NAV_ITEMS.slice(2),
      ]
    : BASE_SETTINGS_NAV_ITEMS;
}
```

In-app links (D16 copy — "backend"/"Connections" wording updates only where touched):

- `apps/web/src/components/Sidebar.tsx:4077`:
  `navigate({ to: "/settings/connections" })` → `navigate({ to: "/settings/remote-servers" })`.
- `apps/web/src/components/ChatView.tsx:2360`: same navigate change; the button label
  `Connections` → `Remote Servers`.
- `apps/web/src/routes/_chat.index.tsx:63`: `<Link to="/settings/connections" />` →
  `<Link to="/settings/remote-servers" />`; button copy `"Open Connections"` →
  `"Open Remote Servers"`; the `EmptyDescription` sentence "add a reachable backend manually"
  → "add a reachable server manually" (both branches).

Also grep for stragglers before finishing: `rg -n "settings/connections" apps/web/src`
must return only `routeTree.gen.ts` (until regenerated) and the redirect route file.

- [x] **Step 4: Regenerate the route tree and run tests**

Run the focused tests (this loads the apps/web Vite config, which regenerates
`apps/web/src/routeTree.gen.ts` via `@tanstack/router-plugin`):

```
vp test run apps/web/src/routes/settings.remote-servers.test.tsx \
  apps/web/src/routes/settings.local-environment.test.tsx \
  apps/web/src/routes/settings.connections.test.tsx \
  apps/web/src/components/settings/SettingsSidebarNav.test.tsx
```

Expected: PASS. Confirm `git status --short` shows `apps/web/src/routeTree.gen.ts` modified;
if it did not regenerate, run `vp run --filter @bibcode/web build` once. Then
`vp run typecheck` — expected clean (catches any missed `/settings/connections` `Link`/
`navigate` literals, which are type-checked against the generated tree).

- [x] **Step 5: Commit**

```bash
git add -A apps/web/src/routes apps/web/src/components/settings/SettingsSidebarNav.tsx \
  apps/web/src/connection/environmentPresentationPolicy.ts apps/web/src/components/Sidebar.tsx \
  apps/web/src/components/ChatView.tsx apps/web/src/routeTree.gen.ts
git commit -m "feat(web): rename Connections to Remote Servers with redirecting routes"
```

---

### Task 5: Connect-tab saved-server rows — status, version, compat, transport

Evolve the moved `SavedBackendListRow` into `RemoteServerRow` inside `ConnectTab.tsx` and
retitle the section per spec §4.8. Existing behavior (status dot, connect/disconnect button,
WSL "Managed above" special case, version-drift warning, error line with trace-ID copy) is
preserved; the row gains the D16 version label, the compat badge, the transport badge, and the
"Status unavailable" state.

**Files:**

- Modify: `apps/web/src/components/settings/remote-servers/ConnectTab.tsx`
- Test: `apps/web/src/components/settings/remote-servers/ConnectTab.test.tsx`

**Interfaces:**

- Consumes: Task 1 helpers (`formatServerVersionLabel`, `describeCompatBadge`,
  `resolveTransportBadge`); Phase 2's landed verdict accessor
  `environmentSession.compatVerdictAtom(environmentId)` (`Atom` of `CompatVerdict | null`;
  `environmentSession` is exported by `apps/web/src/state/session.ts`). The ConnectTab
  container reads the atom once per row and passes the value down as the row view-model's
  `compat` prop — the row component itself never touches the atom, so the harness below can
  feed `compat` directly; `Badge` from `../../ui/badge` (existing); `Tooltip` primitives
  (existing).
- Produces: `RemoteServerRow` (internal to ConnectTab; Phase 6's context card does **not**
  import it — Phase 6 consumes the same Task 1 helpers instead).

- [x] **Step 1: Write the failing tests** (append to `ConnectTab.test.tsx`, using the Task 3
      harness; `h.environments` entries are plain objects shaped like `EnvironmentPresentation`)

```tsx
describe("RemoteServerRow presentation", () => {
  it("shows the BiBCode version, compat badge, and transport badge for a saved server", async () => {
    h.environments = [
      {
        environmentId: EnvironmentId.make("env-1"),
        label: "AI-SERVER",
        relayManaged: false,
        compat: { kind: "legacy" },
        serverConfig: { environment: { serverVersion: "1.4.2" } },
        connection: { phase: "connected", error: null, traceId: null },
        entry: {
          target: { _tag: "BearerConnectionTarget", connectionId: "bearer:env-1" },
          profile: {
            _tag: "Some",
            value: { _tag: "BearerConnectionProfile", hostKey: null },
          },
        },
      },
    ];
    const markup = await renderConnectTab();
    expect(markup).toContain("AI-SERVER");
    expect(markup).toContain("BiBCode v1.4.2");
    expect(markup).toContain("Limited compatibility");
    expect(markup).toContain("Unencrypted");
  });

  it("renders Status unavailable when the probe failed and no descriptor is cached", async () => {
    h.environments = [
      {
        environmentId: EnvironmentId.make("env-2"),
        label: "LAB",
        relayManaged: false,
        compat: null,
        serverConfig: null,
        connection: { phase: "error", error: "connect ECONNREFUSED", traceId: null },
        entry: {
          target: { _tag: "BearerConnectionTarget", connectionId: "bearer:env-2" },
          profile: { _tag: "None" },
        },
      },
    ];
    const markup = await renderConnectTab();
    expect(markup).toContain("Status unavailable");
    expect(markup).toContain("status:error"); // underlying error line preserved
  });
});
```

(`renderConnectTab` is the harness's render helper from Task 3 — the same
static-markup/mount helper the old suite used for `ConnectionsSettings`.)

- [x] **Step 2: Run to verify it fails**

Run: `vp test run apps/web/src/components/settings/remote-servers/ConnectTab.test.tsx`
Expected: FAIL — version/compat/transport strings absent.

- [x] **Step 3: Implement the row upgrade**

In `ConnectTab.tsx`, rename `SavedBackendListRow` → `RemoteServerRow`
(`SavedBackendListRowProps` → `RemoteServerRowProps`) and inside it add:

```tsx
import { Badge } from "../../ui/badge";
import {
  describeCompatBadge,
  formatServerVersionLabel,
  resolveTransportBadge,
} from "./connectPresentation";

// inside RemoteServerRow, after the existing versionMismatch/sshTarget derivations:
const versionLabel = formatServerVersionLabel(environment.serverConfig?.environment.serverVersion);
const compatBadge = describeCompatBadge(environment.compat ?? null);
const transportBadge = resolveTransportBadge(environment);
const statusUnavailable =
  versionLabel === null && compatBadge === null && environment.connection.error !== null;
```

and render, replacing the current `metadataBits` paragraph block:

```tsx
<div className="flex flex-wrap items-center gap-1.5">
  {versionLabel ? <span className="text-xs text-muted-foreground">{versionLabel}</span> : null}
  {statusUnavailable ? (
    <span className="text-xs text-muted-foreground/70">Status unavailable</span>
  ) : null}
  {compatBadge ? (
    <Badge variant={compatBadge.tone === "destructive" ? "destructive" : "secondary"}>
      {compatBadge.label}
    </Badge>
  ) : null}
  {transportBadge ? (
    transportBadge.kind === "unencrypted" ? (
      <Tooltip>
        <TooltipTrigger render={<Badge variant="secondary">{transportBadge.label}</Badge>} />
        <TooltipPopup side="top">{transportBadge.guidance}</TooltipPopup>
      </Tooltip>
    ) : (
      <Badge variant="outline">{transportBadge.label}</Badge>
    )
  ) : null}
  {metadataBits.length > 0 ? (
    <span className="text-xs text-muted-foreground">{metadataBits.join(" · ")}</span>
  ) : null}
</div>
```

(Adjust `Badge` variant names to what `apps/web/src/components/ui/badge.tsx` actually exports
— read it first; if it lacks a destructive variant, use `className` with
`text-destructive border-destructive/40` instead. Do not add new variants to `badge.tsx`.)

Section copy in the same file: `SettingsSection title="Remote environments"` →
`title="Saved servers"`; the header action button label `Add environment` → `Add Server`
(aria-label and tooltip text included). Keep every other string as-is until Task 6.

- [x] **Step 4: Run to verify it passes**

Run: `vp test run apps/web/src/components/settings/remote-servers/ConnectTab.test.tsx`
Expected: PASS, including the pre-existing moved cases (update any that asserted the old
"Remote environments"/"Add environment" strings).

- [x] **Step 5: Commit**

```bash
git add apps/web/src/components/settings/remote-servers
git commit -m "feat(web): show version, compat, and transport on saved server rows"
```

---

### Task 5b: Non-destructive Disconnect latch (spec §6)

Today the saved-server row's "Disconnect" button calls `onRemove(environmentId)` — i.e.
disconnect **deletes the catalog entry**. Spec §6 pins the opposite: "Manual Disconnect is
a client-side latch on the saved environment (supervisor desired-state = disconnected); it
never deletes credentials," with **Remove** as a separate destructive action. The
supervisor already implements the latch (`EnvironmentSupervisor` exposes `connect` /
`disconnect` effects that flip its `desired` intent flag —
`packages/client-runtime/src/connection/supervisor.ts` ~lines 690–705), but
`EnvironmentRegistry` never surfaces it. This task adds the registry passthroughs and
rewires the row. Phase 6's context-card ⋯ menu consumes the same commands.

Scope note: the latch is session-scoped (a fresh app start reconnects saved environments),
which is exactly what spec §6 pins ("supervisor desired-state"). Persisting the latch
across restarts is a deliberate non-goal of this task; if product feedback wants it, that
is a catalog-profile field decision to take back to the spec first.

**Files:**

- Modify: `packages/client-runtime/src/connection/registry.ts` (service interface ~line 65
  and implementation return block ~line 735)
- Modify: `apps/web/src/components/settings/remote-servers/ConnectTab.tsx` (row actions +
  remove-confirmation dialog)
- Modify: `apps/web/src/components/settings/remote-servers/connectPresentation.ts`
  (`countRunningThreadsForEnvironment` helper)
- Test: `packages/client-runtime/src/connection/registry.test.ts`,
  `apps/web/src/components/settings/remote-servers/connectPresentation.test.ts`,
  `apps/web/src/components/settings/remote-servers/ConnectTab.test.tsx`

**Interfaces:**

- Consumes: `EnvironmentSupervisor` `connect`/`disconnect` effects (existing, supervisor.ts
  ~690–705 — they flip the supervisor's `desired` intent flag); the registry's
  `acquireSupervisor` + `EnvironmentNotRegisteredError` pattern already used by `retryNow`
  (registry.ts ~648–653); `registry.state(environmentId)` returning
  `SupervisorConnectionState` (fields `desired: boolean` and
  `phase: "available" | "offline" | "connecting" | "backoff" | "connected" | "blocked"`,
  `packages/client-runtime/src/connection/model.ts` ~162–180 — there is **no**
  `"disconnected"` phase literal anywhere in the union); `useThreadShells()` from
  `~/state/entities` (existing; `EnvironmentThreadShell` carries `environmentId`, and
  running work is `shell.session?.status === "running"` — the same seam
  `apps/web/src/components/Sidebar.tsx:636` already uses).
- Produces: `EnvironmentRegistry.disconnect(environmentId): Effect<void>` and
  `EnvironmentRegistry.connect(environmentId): Effect<void>` — the commands Phase 6's
  context-card menu and the Connect-tab row both call.
  **Pinned row derivation** (the observable seam is the presentation phase, because
  `presentConnectionState` does not surface `desired`): a latched environment settles at
  supervisor phase `"available"`, which `presentConnectionState`
  (`packages/client-runtime/src/connection/presentation.ts:36-38`) passes through as
  presentation phase `"available"`. The row therefore renders the muted status word
  **"Disconnected"** (gray dot) whenever `connection.phase === "available" || "offline"`,
  and the primary button reads "Connect" in that state / "Disconnect" when
  `phase === "connected"`. (A never-yet-connected registered entry also presents as
  `available` and legitimately reads as Disconnected.)
  **Remove confirmation** (spec §6): "Remove server…" lives in the row overflow menu and
  always opens a confirmation dialog; when the environment owns visible running work
  (`countRunningThreadsForEnvironment(...) > 0`) the dialog copy escalates. Also produces
  `countRunningThreadsForEnvironment(shells, environmentId): number` in
  `connectPresentation.ts`.

- [x] **Step 1: Write the failing registry test** (append to `registry.test.ts`, using the
      same harness as the existing `retryNow` case at ~line 809)

```ts
it.effect("disconnect latches the supervisor without removing the entry", () =>
  Effect.gen(function* () {
    const { registry, environmentId } = yield* registerBearerEnvironment();
    yield* registry.disconnect(environmentId);
    // The latch is the supervisor's desired-intent flag (spec §6 "supervisor
    // desired-state = disconnected"); there is no "disconnected" phase literal —
    // a latched supervisor settles at phase "available".
    const state = yield* registry.state(environmentId);
    expect(state.desired).toBe(false);
    expect(state.phase).not.toBe("connected");
    // the catalog entry (and its credentials) survive the latch:
    const entries = yield* SubscriptionRef.get(registry.entries);
    expect(entries.has(environmentId)).toBe(true);
    // and the latch is reversible without re-registering:
    yield* registry.connect(environmentId);
    const reconnected = yield* registry.state(environmentId);
    expect(reconnected.desired).toBe(true);
  }),
);

it.effect("disconnect on an unregistered environment is a no-op", () =>
  Effect.gen(function* () {
    const { registry } = yield* registerBearerEnvironment();
    yield* registry.disconnect(EnvironmentId.make("missing-environment"));
  }),
);
```

(`registerBearerEnvironment` names the file's existing setup helper — reuse whatever the
`retryNow` test actually calls. `registry.entries` is a
`SubscriptionRef<ReadonlyMap<EnvironmentId, ConnectionCatalogEntry>>`, hence `entries.has`.
If the harness's supervisor needs a settle step before the phase assertion, follow the
existing `retryNow` case's synchronization approach rather than sleeping.)

- [x] **Step 2: Run to verify it fails**

Run: `vp test run packages/client-runtime/src/connection/registry.test.ts`
Expected: FAIL — `registry.disconnect is not a function`.

- [x] **Step 3: Implement the registry passthroughs**

In `registry.ts`, extend the service interface next to `retryNow` (~line 92):

```ts
readonly connect: (environmentId: EnvironmentId) => Effect.Effect<void>;
readonly disconnect: (environmentId: EnvironmentId) => Effect.Effect<void>;
```

and the implementation beside `retryNow` (~line 648), mirroring its shape:

```ts
const connect = (environmentId: EnvironmentId) =>
  acquireSupervisor(environmentId).pipe(
    Effect.flatMap((supervisor) => supervisor.connect),
    Effect.catchTag("EnvironmentNotRegisteredError", () => Effect.void),
    Effect.withSpan("EnvironmentRegistry.connect"),
  );
const disconnect = (environmentId: EnvironmentId) =>
  acquireSupervisor(environmentId).pipe(
    Effect.flatMap((supervisor) => supervisor.disconnect),
    Effect.catchTag("EnvironmentNotRegisteredError", () => Effect.void),
    Effect.withSpan("EnvironmentRegistry.disconnect"),
  );
```

then add `connect,` and `disconnect,` to the `EnvironmentRegistry.of({ ... })` return
block (~line 735).

- [x] **Step 4: Run to verify it passes**

Run: `vp test run packages/client-runtime/src/connection/registry.test.ts`
Expected: PASS.

- [x] **Step 5: Write the failing row and confirmation tests**

First the pure helper (append to `connectPresentation.test.ts`):

```ts
describe("countRunningThreadsForEnvironment", () => {
  it("counts only running sessions belonging to the environment", () => {
    const shells = [
      { environmentId: "env-1", session: { status: "running" } },
      { environmentId: "env-1", session: { status: "idle" } },
      { environmentId: "env-1", session: null },
      { environmentId: "env-2", session: { status: "running" } },
    ];
    expect(countRunningThreadsForEnvironment(shells, "env-1")).toBe(1);
    expect(countRunningThreadsForEnvironment(shells, "env-3")).toBe(0);
  });
});
```

Then the row behavior (append to `ConnectTab.test.tsx`):

```tsx
it("Disconnect latches instead of removing, and Remove moves behind a confirmation", async () => {
  h.environments = [connectedBearerEnvironment("env-1", "AI-SERVER")];
  const markup = await renderConnectTab();
  expect(markup).toContain("Disconnect");
  // clicking Disconnect must invoke the latch command, not removal:
  await clickButton("Disconnect");
  expect(h.calls.disconnect).toEqual(["env-1"]);
  expect(h.calls.remove).toEqual([]);
  // the destructive action is an explicit menu item that opens a dialog:
  await openRowOverflowMenu("AI-SERVER");
  expect(await menuItems()).toContain("Remove server…");
  await clickMenuItem("Remove server…");
  expect(currentMarkup()).toContain("Remove AI-SERVER?");
  expect(h.calls.remove).toEqual([]); // nothing removed until confirmed
  await clickButton("Remove server");
  expect(h.calls.remove).toEqual(["env-1"]);
});

it("renders a latched (available) environment as Disconnected with a Connect action", async () => {
  h.environments = [
    {
      ...connectedBearerEnvironment("env-1", "AI-SERVER"),
      connection: { phase: "available", error: null, traceId: null },
    },
  ];
  const markup = await renderConnectTab();
  expect(markup).toContain("Disconnected");
  expect(markup).toContain("Connect");
});

it("escalates the remove confirmation when the environment owns running work", async () => {
  h.environments = [connectedBearerEnvironment("env-1", "AI-SERVER")];
  h.threadShells = [
    { environmentId: "env-1", session: { status: "running" } },
    { environmentId: "env-1", session: { status: "running" } },
  ];
  await renderConnectTab();
  await openRowOverflowMenu("AI-SERVER");
  await clickMenuItem("Remove server…");
  expect(currentMarkup()).toContain("2 running sessions");
});
```

(`connectedBearerEnvironment`, `clickButton`, `clickMenuItem`, `openRowOverflowMenu`,
`menuItems`, `currentMarkup` are Task 3 harness helpers — extend the harness with a
`calls.disconnect` recorder alongside the existing remove/connect recorders, and add
`h.threadShells` backing a `vi.mock` of `~/state/entities`'s `useThreadShells` (default
`[]`). Keep to the harness's actual interaction style — if the old suite asserts via
handler props rather than DOM clicks, assert the same way.)

- [x] **Step 6: Run to verify it fails**

Run:

```
vp test run apps/web/src/components/settings/remote-servers/connectPresentation.test.ts \
  apps/web/src/components/settings/remote-servers/ConnectTab.test.tsx
```

Expected: FAIL — helper missing; Disconnect still routes to remove; no confirmation dialog.

- [x] **Step 7: Implement the helper, rewire the row, add the confirmation dialog**

Pure helper in `connectPresentation.ts` (structural input so it stays testable without
constructing full `EnvironmentThreadShell`s):

```ts
export function countRunningThreadsForEnvironment(
  shells: ReadonlyArray<{
    readonly environmentId: string;
    readonly session?: { readonly status?: string } | null;
  }>,
  environmentId: string,
): number {
  return shells.filter(
    (shell) => shell.environmentId === environmentId && shell.session?.status === "running",
  ).length;
}
```

In `ConnectTab.tsx`, the container gains an atom-command wrapper for the two registry
commands (same pattern the file already uses for its remove/retry commands via the
connection catalog atoms in `apps/web/src/connection/catalog.ts` — if no
connect/disconnect command atom exists there yet, add
`environmentCatalog.connect(environmentId)` / `environmentCatalog.disconnect(environmentId)`
command atoms in that file delegating to the registry methods from Step 3, following the
file's existing command-atom idiom). Then in `RemoteServerRow`:

```tsx
// Pinned derivation (see Interfaces): latched-or-idle presents as "available"/"offline".
const isDisconnected =
  environment.connection.phase === "available" || environment.connection.phase === "offline";
// status word next to the dot (replaces nothing — new element beside the label):
{isDisconnected ? (
  <span className="text-xs text-muted-foreground/70">Disconnected</span>
) : null}
// primary action:
onClick={() =>
  void (isConnected ? onDisconnect(environmentId) : onConnect(environmentId))
}
```

with `onDisconnect` threaded from the container. The destructive path becomes a row
overflow menu (`Menu` primitives already imported by the moved code) with one item labeled
`Remove server…` that opens the confirmation dialog — **today's `onRemove` runs with no
confirmation at all** (the pre-split `SavedBackendListRow` invoked removal directly), so
the dialog is new behavior required by spec §6, not a relocation:

```tsx
const threadShells = useThreadShells(); // ~/state/entities
const [removalCandidate, setRemovalCandidate] = useState<{
  environmentId: EnvironmentId;
  label: string;
} | null>(null);
const runningCount =
  removalCandidate === null
    ? 0
    : countRunningThreadsForEnvironment(threadShells, removalCandidate.environmentId);

<AlertDialog
  open={removalCandidate !== null}
  onOpenChange={(open) => {
    if (!open) setRemovalCandidate(null);
  }}
>
  <AlertDialogPopup>
    <AlertDialogHeader>
      <AlertDialogTitle>
        {removalCandidate ? `Remove ${removalCandidate.label}?` : ""}
      </AlertDialogTitle>
      <AlertDialogDescription>
        {runningCount > 0
          ? `${runningCount} running ${runningCount === 1 ? "session" : "sessions"} on ${removalCandidate?.label} will keep running on the server but disappear from this device until you pair again. Removing deletes the saved server and its credentials from this device.`
          : "This deletes the saved server and its credentials from this device. The server itself is not affected."}
      </AlertDialogDescription>
    </AlertDialogHeader>
    <AlertDialogFooter>
      <AlertDialogClose render={<Button variant="outline" />}>Cancel</AlertDialogClose>
      <Button
        variant="destructive"
        onClick={() => {
          if (removalCandidate === null) return;
          const environmentId = removalCandidate.environmentId;
          setRemovalCandidate(null);
          void onRemove(environmentId);
        }}
      >
        Remove server
      </Button>
    </AlertDialogFooter>
  </AlertDialogPopup>
</AlertDialog>;
```

The menu item's handler is `setRemovalCandidate({ environmentId, label: environment.label })`;
`onRemove` itself (catalog removal via `environmentCatalog.remove`) is unchanged.

- [x] **Step 8: Run to verify it passes**

Run:

```
vp test run apps/web/src/components/settings/remote-servers/connectPresentation.test.ts \
  apps/web/src/components/settings/remote-servers/ConnectTab.test.tsx
```

Expected: PASS. Update any pre-existing moved case that asserted immediate removal on the
row button — removal now always flows through the menu item + confirmation dialog.

- [x] **Step 9: Commit**

```bash
git add packages/client-runtime/src/connection/registry.ts \
  packages/client-runtime/src/connection/registry.test.ts \
  apps/web/src/connection/catalog.ts \
  apps/web/src/components/settings/remote-servers
git commit -m "feat(web): disconnect latch and confirmed server removal"
```

---

### Task 6: Add Server flow — pairing-code first, Advanced manual entry, troubleshooting

Rework the Add dialog per spec §4.8 and D12/D13: primary mode = paste a pairing code
(consuming Phase 3's verify-then-add), SSH stays a first-class mode card, the old manual
endpoint+token entry moves under an **Advanced** expander inside the pairing-code mode, and a
**connection troubleshooting** expander renders the five classified failures' guidance.

**Files:**

- Modify: `apps/web/src/connection/onboarding.ts` (new atom command)
- Modify: `apps/web/src/components/settings/remote-servers/ConnectTab.tsx`
- Test: `apps/web/src/components/settings/remote-servers/ConnectTab.test.tsx`

**Interfaces:**

- Consumes (Phase 3 landed names — see the Interfaces-consumed section):
  `ConnectionOnboarding.verifyAndAddPairingCode(input: VerifyPairingCodeInput)` with
  `VerifyPairingCodeInput = { readonly code: string; readonly allowLoopbackTunnel?: boolean }`
  (the flow — live probe, E2EE handshake, authenticated `server.getConfig`, failure
  classification — is Phase 3's, spec §4.2); its tagged failures `PairingAddError { reason,
  detail }` (the five spec-pinned reasons live on **`reason`**),
  `PairingLoopbackAcknowledgementRequiredError { endpoint }`, `PairingCodeParseError`,
  `PairingCodeUnsupportedVersionError`; `parsePairingCode` from
  `@bibcode/shared/pairingCode` for the local pre-decode (accepts bare code + both URL
  forms); `classifyPairingEndpoint` from `@bibcode/shared/advertisedEndpoint` (returns
  `"loopback" | "private-network" | "public" | "unconnectable"` — only `"loopback"` drives
  the acknowledgement UI here). Task 1's `normalizePairingCodeInput`,
  `describeAddServerFailure`, `resolvePairingAddFailureReason`,
  `isLoopbackAcknowledgementRequired`, `ADD_SERVER_FAILURE_REASONS`.
  Existing `Collapsible` primitives from `../../ui/collapsible`, `Checkbox` from
  `../../ui/checkbox`.
- Produces: `connectRemoteServer` atom command in `apps/web/src/connection/onboarding.ts`
  with input `{ readonly code: string; readonly allowLoopbackTunnel?: boolean }` (Task 8 and
  Phase 6's "Add server…" rail entry reuse it); ConnectTab props gain
  `initialPairingCode?: string | null` (consumed by Task 8).

- [x] **Step 1: Write the failing tests** (in `ConnectTab.test.tsx`; extend the harness's
      `~/connection/onboarding` mock with `connectRemoteServer: h.atoms.connectRemoteServer` and a
      `h.commands.connectRemoteServer` mock, mirroring how `connectPairing` is mocked)

```tsx
describe("Add Server flow", () => {
  it("submits a normalized pairing code through connectRemoteServer", async () => {
    h.commands.connectRemoteServer.mockResolvedValue({ _tag: "Success", value: "env-9" });
    await openAddServerDialog(); // harness helper: pins the dialog-open state override
    await typePairingCode("bibcode://pair?code=abc123");
    await clickButton("Add Server");
    expect(h.commands.connectRemoteServer).toHaveBeenCalledWith({
      code: "abc123",
      allowLoopbackTunnel: false,
    });
  });

  it("renders classified failure copy for a rejected pairing (PairingAddError.reason)", async () => {
    h.commands.connectRemoteServer.mockResolvedValue({
      _tag: "Failure",
      cause: { _tag: "PairingAddError", reason: "pairing-rejected", detail: "expired" },
    });
    await openAddServerDialog();
    await typePairingCode("abc123");
    await clickButton("Add Server");
    const markup = currentMarkup();
    expect(markup).toContain("Pairing rejected");
    expect(markup).toContain("Codes are single-use and expire");
  });

  it("requires the tunnel acknowledgement for a this-computer (loopback) code", async () => {
    // parsePairingCode is harness-mocked; the payload decodes to a loopback endpoint
    h.decodedPairingCode = {
      v: 1,
      endpoint: "http://127.0.0.1:3773",
      name: "LOCAL-ONLY",
      token: "t",
      hostKey: "k",
      reach: "this-computer",
      storageInstanceId: "s",
    };
    await openAddServerDialog();
    await typePairingCode("abc123");
    const markup = currentMarkup();
    expect(markup).toContain("only reachable on the server itself");
    expect(addServerButtonDisabled()).toBe(true);
    await checkTunnelAcknowledgement();
    expect(addServerButtonDisabled()).toBe(false);
    // the acknowledgement travels with the retry input:
    h.commands.connectRemoteServer.mockResolvedValue({ _tag: "Success", value: "env-9" });
    await clickButton("Add Server");
    expect(h.commands.connectRemoteServer).toHaveBeenCalledWith({
      code: "abc123",
      allowLoopbackTunnel: true,
    });
  });

  it("reveals the acknowledgement when the flow itself demands it, then retries with it", async () => {
    // local pre-decode saw nothing loopback (e.g. mocked parse failure), but the
    // flow classified the endpoint as loopback:
    h.decodedPairingCode = null;
    h.commands.connectRemoteServer
      .mockResolvedValueOnce({
        _tag: "Failure",
        cause: {
          _tag: "PairingLoopbackAcknowledgementRequiredError",
          endpoint: "http://127.0.0.1:3773",
        },
      })
      .mockResolvedValueOnce({ _tag: "Success", value: "env-9" });
    await openAddServerDialog();
    await typePairingCode("abc123");
    await clickButton("Add Server");
    expect(currentMarkup()).toContain("only reachable on the server itself");
    await checkTunnelAcknowledgement();
    await clickButton("Add Server");
    expect(h.commands.connectRemoteServer).toHaveBeenLastCalledWith({
      code: "abc123",
      allowLoopbackTunnel: true,
    });
  });

  it("keeps manual endpoint+token entry under Advanced and SSH as a first-class mode", async () => {
    await openAddServerDialog();
    const markup = currentMarkup();
    expect(markup).toContain("Pairing code");
    expect(markup).toContain("SSH");
    expect(markup).toContain("Advanced");
    expect(markup).toContain("Troubleshooting");
  });
});
```

(`openAddServerDialog`, `typePairingCode`, `clickButton`, `currentMarkup`,
`addServerButtonDisabled`, `checkTunnelAcknowledgement` are small helpers added to
`testHarness.tsx` following the existing state-override + captured-controls approach of the
old suite; `h.decodedPairingCode` backs a harness `vi.mock` of
`@bibcode/shared/pairingCode` whose `parsePairingCode` returns the value (or throws when it
is `null`) so the loopback case does not need a real base64url payload.)

- [x] **Step 2: Run to verify it fails**

Run: `vp test run apps/web/src/components/settings/remote-servers/ConnectTab.test.tsx`
Expected: FAIL — `connectRemoteServer` mock unused, new dialog strings absent.

- [x] **Step 3: Implement**

Atom command (mirrors the existing `connectPairing` shape exactly):

```ts
// apps/web/src/connection/onboarding.ts — append:
export const connectRemoteServer = createRuntimeCommand(connectionAtomRuntime, {
  label: "web:connection:connect-remote-server",
  scheduler: onboardingScheduler,
  concurrency: {
    mode: "singleFlight",
    // The acknowledgement is part of the identity: an acknowledged retry must
    // not be deduplicated against an in-flight unacknowledged attempt.
    key: (input: { readonly code: string; readonly allowLoopbackTunnel?: boolean }) =>
      `${input.allowLoopbackTunnel === true ? "ack" : "raw"}:${input.code}`,
  },
  execute: (input: { readonly code: string; readonly allowLoopbackTunnel?: boolean }) =>
    ConnectionOnboarding.pipe(
      Effect.flatMap((onboarding) => onboarding.verifyAndAddPairingCode(input)),
    ),
});
```

Dialog rework in `ConnectTab.tsx`:

1. `savedBackendMode` type becomes `"pairing-code" | "manual" | "ssh"` with default
   `"pairing-code"`. Mode cards: **"Pairing code"** (icon `QrCodeIcon`, description
   `"Paste a pairing code from the server's Share tab."`) and **"SSH"** (unchanged card,
   desktop-bridge-only, unchanged fields/discovered-host rows/handlers). `"manual"` is not a
   card — it is the Advanced expander state inside the pairing-code mode.
2. Pairing-code mode body:

```tsx
import { parsePairingCode } from "@bibcode/shared/pairingCode";
import { classifyPairingEndpoint } from "@bibcode/shared/advertisedEndpoint";
import type { PairingAddFailureReason } from "@bibcode/client-runtime/connection";

const [pairingCodeInput, setPairingCodeInput] = useState("");
const [tunnelAcknowledged, setTunnelAcknowledged] = useState(false);
// set when the flow itself failed with PairingLoopbackAcknowledgementRequiredError
// even though the local pre-decode saw nothing loopback:
const [flowDemandsAcknowledgement, setFlowDemandsAcknowledgement] = useState(false);
const [addServerFailure, setAddServerFailure] = useState<PairingAddFailureReason | null>(null);
const connectRemoteServerCommand = useAtomCommand(connectRemoteServerAtom, {
  reportFailure: false,
});

const normalizedCode = normalizePairingCodeInput(pairingCodeInput);
const decodedCode = useMemo(() => {
  if (normalizedCode === null) return null;
  try {
    // Phase 3's canonical codec (@bibcode/shared/pairingCode) — accepts the bare
    // code and both URL forms; throws PairingCodeParseError /
    // PairingCodeUnsupportedVersionError on bad input.
    return parsePairingCode(normalizedCode);
  } catch {
    return null; // pre-decode is best-effort UI; the flow re-parses authoritatively
  }
}, [normalizedCode]);

const requiresTunnelAcknowledgement =
  flowDemandsAcknowledgement ||
  (decodedCode !== null &&
    (decodedCode.reach === "this-computer" ||
      classifyPairingEndpoint(decodedCode.endpoint) === "loopback"));

const handleAddServer = useCallback(async () => {
  if (normalizedCode === null) {
    setSavedBackendError("Enter a pairing code.");
    return;
  }
  setIsAddingSavedBackend(true);
  setSavedBackendError(null);
  setAddServerFailure(null);
  const result = await connectRemoteServerCommand({
    code: normalizedCode,
    allowLoopbackTunnel: tunnelAcknowledged,
  });
  setIsAddingSavedBackend(false);
  if (result._tag === "Failure") {
    if (isAtomCommandInterrupted(result)) return;
    const error = squashAtomCommandFailure(result);
    if (isLoopbackAcknowledgementRequired(error)) {
      // Reveal the acknowledgement row; the user checks it and submits again
      // (the retry then carries allowLoopbackTunnel: true).
      setFlowDemandsAcknowledgement(true);
      return;
    }
    const reason = resolvePairingAddFailureReason(error);
    if (reason !== null) {
      setAddServerFailure(reason);
    } else {
      // PairingCodeParseError / PairingCodeUnsupportedVersionError and anything
      // unclassified carry user-facing messages:
      setSavedBackendError(error instanceof Error ? error.message : "Failed to add the server.");
    }
    return;
  }
  setPairingCodeInput("");
  setTunnelAcknowledged(false);
  setFlowDemandsAcknowledgement(false);
  setAddBackendDialogOpen(false);
  toastManager.add({
    type: "success",
    title: "Server added",
    description: "The server is saved and will reconnect on app startup.",
  });
}, [connectRemoteServerCommand, normalizedCode, tunnelAcknowledged]);
```

JSX: a `Textarea` labeled **Pairing code** (placeholder `bibcode://pair?code=…`,
`spellCheck={false}`); when `requiresTunnelAcknowledgement`, a `Checkbox` row with the
copy `This address is only reachable on the server itself. I have set up a tunnel (SSH
   port forward or similar) from this device.` gating the button
(`disabled={requiresTunnelAcknowledgement && !tunnelAcknowledged}`); a primary Button
labeled `Add Server` (busy label `Adding…`); on `addServerFailure` render the
`describeAddServerFailure` title/detail in the existing destructive error panel style. 3. **Advanced expander** (inside pairing-code mode, below the primary button):

```tsx
<Collapsible>
  <CollapsibleTrigger className="text-xs text-muted-foreground underline underline-offset-2">
    Advanced: manual endpoint and token
  </CollapsibleTrigger>
  <CollapsibleContent>{renderRemoteModeBody()}</CollapsibleContent>
</Collapsible>
```

`renderRemoteFields`/`renderRemoteModeBody`, `handleSavedBackendHostChange`,
`parseRemotePairingFields`, and the `connectPairing`-based `handleAddSavedBackend` branch
move under this expander **unchanged** (this is the preserved
`ConnectionOnboarding.registerPairing` manual path; its button label stays distinct:
`Add manually`). No Phase 3 dependency in this sub-path. 4. **Troubleshooting expander** (below Advanced, same Collapsible pattern, trigger text
`Troubleshooting`), static list built from Task 1 copy:

```tsx
<ul className="space-y-2 text-xs text-muted-foreground">
  {ADD_SERVER_FAILURE_REASONS.map((reason) => {
    const described = describeAddServerFailure(reason);
    return (
      <li key={reason}>
        <span className="font-medium text-foreground">{described.title}.</span> {described.detail}
      </li>
    );
  })}
  <li>
    <span className="font-medium text-foreground">Still stuck?</span> Confirm both devices are on
    the same network or connected through a tunnel, then generate a fresh pairing code on the
    server's Share tab.
  </li>
</ul>
```

5. Dialog title/description: `Add Server` / `Connect this device to another BiBCode server.`
6. Add the `initialPairingCode?: string | null` prop: when non-null on mount, open the dialog
   in pairing-code mode with `pairingCodeInput` prefilled (a `useEffect` keyed on the prop).
   `RemoteServersSettings` forwards it (`<ConnectTab initialPairingCode={initialPairingCode} />`,
   new optional prop on the shell, default `null`).

- [x] **Step 4: Run to verify it passes**

Run: `vp test run apps/web/src/components/settings/remote-servers/ConnectTab.test.tsx apps/web/src/components/settings/remote-servers/RemoteServersSettings.test.tsx`
Expected: PASS (existing SSH-mode and manual-path cases still green — the manual path's
assertions now go through the Advanced expander's pinned open state via the harness override).

- [x] **Step 5: Commit**

```bash
git add apps/web/src/connection/onboarding.ts apps/web/src/components/settings/remote-servers
git commit -m "feat(web): add pairing-code Add Server flow with advanced manual entry"
```

---

### Task 7: "Check for Server Updates" placement seam (Phase 7 wires it)

**Files:**

- Modify: `apps/web/src/components/settings/remote-servers/ConnectTab.tsx`
- Test: `apps/web/src/components/settings/remote-servers/ConnectTab.test.tsx`

**Interfaces:**

- Produces: `SERVER_UPDATE_CHECK_ENABLED` constant + `showServerUpdateCheck?: boolean` prop on
  `ConnectTab` (defaulting to the constant) + an `onCheckForServerUpdates` no-op seam. Phase 7
  replaces the constant with the `remoteUpdateControl`-capability predicate (spec §4.5) and
  fills the handler with the max-2-concurrent `updater.check` fan-out.

- [x] **Step 1: Write the failing tests**

```tsx
describe("Check for Server Updates placement", () => {
  it("is hidden while the Phase 7 capability seam is off", async () => {
    const markup = await renderConnectTab();
    expect(markup).not.toContain("Check for Server Updates");
  });

  it("renders in the Saved servers header when the seam is enabled", async () => {
    const markup = await renderConnectTab({ showServerUpdateCheck: true });
    expect(markup).toContain("Check for Server Updates");
  });
});
```

- [x] **Step 2: Run to verify it fails**

Run: `vp test run apps/web/src/components/settings/remote-servers/ConnectTab.test.tsx`
Expected: FAIL — the enabled case finds no button.

- [x] **Step 3: Implement**

```tsx
// ConnectTab.tsx
// Phase 7 replaces this constant with a predicate over the saved environments'
// `remoteUpdateControl` capability (spec §4.5) and implements the handler
// (updater.check fan-out, max 2 concurrent). Until then the control is hidden:
// shipping a permanently disabled button would be a dead control, and the spec
// gates this whole surface on the capability anyway.
const SERVER_UPDATE_CHECK_ENABLED = false;

export function ConnectTab({
  initialPairingCode = null,
  showServerUpdateCheck = SERVER_UPDATE_CHECK_ENABLED,
}: {
  readonly initialPairingCode?: string | null;
  readonly showServerUpdateCheck?: boolean;
}) {
  // …
}
```

In the "Saved servers" `SettingsSection` `headerAction`, wrap the existing Add Server trigger
and the new button in a flex group:

```tsx
headerAction={
  <div className="flex items-center gap-1">
    {showServerUpdateCheck ? (
      <Button
        size="xs"
        variant="ghost"
        className="h-5 gap-1 rounded-sm px-1 text-[11px] font-normal text-muted-foreground/60 hover:text-muted-foreground"
        onClick={() => {
          /* Phase 7 wires updater.check fan-out here */
        }}
      >
        <RefreshCwIcon className="size-3" />
        <span>Check for Server Updates</span>
      </Button>
    ) : null}
    {/* existing Add Server Dialog trigger */}
  </div>
}
```

- [x] **Step 4: Run to verify it passes**

Run: `vp test run apps/web/src/components/settings/remote-servers/ConnectTab.test.tsx`
Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add apps/web/src/components/settings/remote-servers
git commit -m "feat(web): reserve Check for Server Updates placement behind capability seam"
```

---

### Task 8: Web `/pair?code=…` entry point

`/pair?code=…` is the single convergence point for deep links (design decision 6), with two
distinct outcomes pinned by amended spec §4.2:

- **Already-authenticated session** → forwarded to `/settings/remote-servers?code=…`, which
  auto-opens the Add Server dialog prefilled.
- **Fresh, unauthenticated device** → never gated on a pre-existing session: the pairing
  code itself carries the one-time credential (`token`, the existing `auth_pairing_links`
  token), so `PairingRouteSurface` — which already auto-submits a URL-carried token via
  `submitServerAuthCredential` (`apps/web/src/components/auth/PairingRouteSurface.tsx:50,
  88–97`) — consumes the embedded token to establish the browser session with the serving
  host, then lands at the root. It does **not** continue into Add Server: the primary
  session it just established _is_ that server, and saving it again as a bearer entry would
  collide on the storage identity.

**Files:**

- Modify: `apps/web/src/routes/pair.tsx`
- Modify: `apps/web/src/components/auth/PairingRouteSurface.tsx` (new optional
  `initialCredential` prop)
- Create: `apps/web/src/components/auth/pairingCodeCredential.ts` (+ test)
- Modify: `apps/web/src/routes/settings.remote-servers.tsx`
- Test: create `apps/web/src/routes/pair.test.tsx`; extend
  `apps/web/src/routes/settings.remote-servers.test.tsx`

**Interfaces:**

- Consumes: Task 4's `/settings/remote-servers` `validateSearch` (`code` param); Task 6's
  `initialPairingCode` prop chain; Phase 3's `parsePairingCode` (+ `encodePairingCode` in
  tests) from `@bibcode/shared/pairingCode`; existing `peekPairingTokenFromUrl` /
  `submitServerAuthCredential` machinery inside `PairingRouteSurface`.
- Produces: `/pair` search schema `{ code?: string }`;
  `extractEmbeddedPairingToken(code: string): string | null` in
  `apps/web/src/components/auth/pairingCodeCredential.ts`;
  `PairingRouteSurface` prop `initialCredential?: string`.

- [x] **Step 1: Write the failing tests**

```tsx
// apps/web/src/routes/pair.test.tsx
import { describe, expect, it } from "vite-plus/test";

import { Route } from "./pair";

describe("/pair with a pairing code", () => {
  it("validates the code search param", () => {
    const validate = Route.options.validateSearch;
    if (typeof validate !== "function") throw new Error("validateSearch is not registered.");
    expect(validate({ code: "abc" })).toEqual({ code: "abc" });
    expect(validate({ code: "" })).toEqual({});
    expect(validate({})).toEqual({});
  });

  it("forwards an authenticated client to Remote Servers with the code", async () => {
    const beforeLoad = Route.options.beforeLoad;
    if (typeof beforeLoad !== "function") throw new Error("beforeLoad is not registered.");
    await expect(
      Promise.resolve().then(() =>
        beforeLoad({
          context: { authGateState: { status: "authenticated" } },
          search: { code: "abc" },
        } as never),
      ),
    ).rejects.toMatchObject({
      options: { to: "/settings/remote-servers", search: { code: "abc" }, replace: true },
    });
  });

  it("still sends an authenticated client without a code to the root", async () => {
    const beforeLoad = Route.options.beforeLoad;
    if (typeof beforeLoad !== "function") throw new Error("beforeLoad is not registered.");
    await expect(
      Promise.resolve().then(() =>
        beforeLoad({
          context: { authGateState: { status: "authenticated" } },
          search: {},
        } as never),
      ),
    ).rejects.toMatchObject({ options: { to: "/", replace: true } });
  });

  it("never gates a fresh, unauthenticated device carrying a code (amended spec §4.2)", async () => {
    const beforeLoad = Route.options.beforeLoad;
    if (typeof beforeLoad !== "function") throw new Error("beforeLoad is not registered.");
    // Unauthenticated statuses fall through to the pairing surface — no redirect:
    await expect(
      Promise.resolve().then(() =>
        beforeLoad({
          context: { authGateState: { status: "pairing", auth: {} } },
          search: { code: "abc" },
        } as never),
      ),
    ).resolves.toMatchObject({ authGateState: { status: "pairing" } });
  });
});
```

New `apps/web/src/components/auth/pairingCodeCredential.test.ts`:

```ts
import { describe, expect, it } from "vite-plus/test";
import { encodePairingCode } from "@bibcode/shared/pairingCode";

import { extractEmbeddedPairingToken } from "./pairingCodeCredential";

describe("extractEmbeddedPairingToken", () => {
  it("returns the one-time token embedded in a valid pairing code", () => {
    const code = encodePairingCode({
      v: 1,
      endpoint: "http://192.168.1.20:3773",
      name: "AI-SERVER",
      token: "BCDFGHJKMNPQ",
      hostKey: "HcMLXPPBHFNvcbHrCVMH-DMh49rd5AGCzSCqAVJ49hM",
      reach: "another-device",
      storageInstanceId: "3f2f6a52-6f5f-4f4e-9d38-0a1e2ac21d11",
    });
    expect(extractEmbeddedPairingToken(code)).toBe("BCDFGHJKMNPQ");
  });

  it("returns null for garbage instead of throwing", () => {
    expect(extractEmbeddedPairingToken("not-a-code!!")).toBeNull();
  });
});
```

Extend `settings.remote-servers.test.tsx` (component-mock style of
`SettingsSidebarNav.test.tsx`; the harness mocks must be declared before the route import):

```tsx
const captured = vi.hoisted(() => ({ props: null as Record<string, unknown> | null }));

vi.mock("../components/settings/remote-servers/RemoteServersSettings", () => ({
  RemoteServersSettings: (props: Record<string, unknown>) => {
    captured.props = props;
    return null;
  },
}));

it("forwards the code search param as the initial pairing code", () => {
  const Component = Route.options.component;
  if (typeof Component !== "function") throw new Error("Route component is not registered.");
  vi.spyOn(Route, "useSearch").mockReturnValue({ code: "abc" } as never);
  vi.spyOn(Route, "useNavigate").mockReturnValue(vi.fn() as never);
  renderToStaticMarkup(<Component />);
  expect(captured.props).toMatchObject({
    initialTab: "connect",
    initialPairingCode: "abc",
  });
});
```

- [x] **Step 2: Run to verify it fails**

Run: `vp test run apps/web/src/routes/pair.test.tsx apps/web/src/routes/settings.remote-servers.test.tsx`
Expected: FAIL — `/pair` has no `validateSearch`; forwarding not implemented.

- [x] **Step 3: Implement**

`pair.tsx` changes:

```tsx
export const Route = createFileRoute("/pair")({
  validateSearch: (search: Record<string, unknown>) => ({
    ...(typeof search.code === "string" && search.code.length > 0 ? { code: search.code } : {}),
  }),
  beforeLoad: async ({ context, search }) => {
    const { authGateState } = context;
    if (authGateState.status === "hosted-pairing") {
      return { authGateState };
    }
    if (authGateState.status === "authenticated" || authGateState.status === "hosted-static") {
      if (search.code !== undefined) {
        throw redirect({
          to: "/settings/remote-servers",
          search: { code: search.code },
          replace: true,
        });
      }
      throw redirect({ to: "/", replace: true });
    }
    return { authGateState };
  },
  component: PairRouteView,
  pendingComponent: PairRoutePendingView,
});
```

New `apps/web/src/components/auth/pairingCodeCredential.ts`:

```ts
import { parsePairingCode } from "@bibcode/shared/pairingCode";

/**
 * A pairing code embeds the one-time pairing-link token (spec §4.2). A fresh,
 * unauthenticated device landing on /pair?code=… authenticates with that token
 * directly — never gated on a pre-existing session.
 */
export function extractEmbeddedPairingToken(code: string): string | null {
  try {
    const token = parsePairingCode(code).token.trim();
    return token.length > 0 ? token : null;
  } catch {
    return null; // fall back to the manual token field
  }
}
```

`PairingRouteSurface` gains an optional `initialCredential` prop that seeds the same
auto-submit path the URL-carried token uses today (one-line change at line 50):

```tsx
export function PairingRouteSurface({
  auth,
  initialCredential,
  initialErrorMessage,
  onAuthenticated,
}: {
  auth: AuthSessionState["auth"];
  initialCredential?: string;
  initialErrorMessage?: string;
  onAuthenticated: () => void;
}) {
  const autoPairTokenRef = useRef<string | null>(
    peekPairingTokenFromUrl() ?? initialCredential ?? null,
  );
  // …everything else unchanged: the existing useEffect auto-submits the ref'd
  // credential once via submitCredential…
```

and in `PairRouteView`, thread the embedded token through; `onAuthenticated` keeps its
existing root navigation (see the task intro for why the code flow does **not** continue
into Add Server after a fresh authentication):

```tsx
const { code } = Route.useSearch();
// …
<PairingRouteSurface
  auth={authGateState.auth}
  {...(code !== undefined
    ? (() => {
        const embedded = extractEmbeddedPairingToken(code);
        return embedded !== null ? { initialCredential: embedded } : {};
      })()
    : {})}
  onAuthenticated={() => {
    void navigate({ to: "/", replace: true });
  }}
  …
/>
```

`settings.remote-servers.tsx` — forward the code and clear it once consumed so refreshes do
not re-open the dialog:

```tsx
function RemoteServersRouteView() {
  const { tab, code } = Route.useSearch();
  const navigate = Route.useNavigate();
  return (
    <RemoteServersSettings
      initialTab={tab === "share" ? "share" : "connect"}
      initialPairingCode={code ?? null}
      onPairingCodeConsumed={() => {
        void navigate({ search: (previous) => ({ ...previous, code: undefined }), replace: true });
      }}
    />
  );
}
```

`RemoteServersSettings` gains `initialPairingCode?: string | null` and
`onPairingCodeConsumed?: () => void`, forwarding both to `ConnectTab`; `ConnectTab` calls
`onPairingCodeConsumed` when the prefilled dialog closes (add-success or cancel).

- [x] **Step 4: Run to verify it passes**

Run:

```
vp test run apps/web/src/routes/pair.test.tsx \
  apps/web/src/components/auth/pairingCodeCredential.test.ts \
  apps/web/src/routes/settings.remote-servers.test.tsx \
  apps/web/src/components/settings/remote-servers/RemoteServersSettings.test.tsx \
  apps/web/src/components/settings/remote-servers/ConnectTab.test.tsx
```

Expected: PASS. Route tree regenerates as in Task 4 (commit `routeTree.gen.ts` if changed).

- [x] **Step 5: Commit**

```bash
git add apps/web/src/routes apps/web/src/components/auth \
  apps/web/src/components/settings/remote-servers apps/web/src/routeTree.gen.ts
git commit -m "feat(web): pair-code entry authenticates fresh devices and prefills Add Server"
```

---

### Task 9: Desktop `bibcode://pair` deep-link registration

The desktop host has no deep-link handling today. Register the Tauri 2 deep-link plugin (with
the single-instance plugin so a second launch on Windows/Linux forwards its URL instead of
opening a second app), surface the URLs to the webview through the existing bridge-event
pattern, and navigate to `/pair?code=…` (Task 8 takes it from there).

**Files:**

- Modify: `Cargo.toml` (workspace deps), `apps/desktop/src-tauri/Cargo.toml`,
  `apps/desktop/src-tauri/src/lib.rs`, `apps/desktop/src-tauri/tauri.conf.json`,
  `apps/desktop/src-tauri/capabilities/default.json`
- Create: `apps/desktop/src-tauri/tests/deep_link_config.rs`
- Modify: `packages/contracts/src/ipc.ts`, `apps/web/src/tauriDesktopBridge.ts`,
  `apps/web/src/routes/__root.tsx`
- Create: `apps/web/src/desktopDeepLink.ts`, `apps/web/src/desktopDeepLink.test.ts`

**Interfaces:**

- Consumes: `tauriInvoke`/`tauriListen` helpers in `apps/web/src/tauriDesktopBridge.ts`
  (existing pattern: see `onProjectDataStatusChanged`); the tauri-plugin-deep-link runtime
  event `deep-link://new-url` (payload: array of URL strings) and command
  `plugin:deep-link|get_current`.
- Produces: `DesktopBridge` optional members `getPendingDeepLinks?: () =>
  Promise<ReadonlyArray<string>>` and `onDeepLink?: (listener: (urls: ReadonlyArray<string>)
  => void) => () => void`; `DESKTOP_DEEP_LINK_EVENT` constant in contracts;
  `resolvePairingDeepLink(rawUrl: string): { readonly code: string } | null` in
  `apps/web/src/desktopDeepLink.ts`.

- [x] **Step 1: Write the failing Rust config test**

```rust
// apps/desktop/src-tauri/tests/deep_link_config.rs
//! The bibcode:// URL scheme must stay registered in the bundler config and the
//! deep-link plugin permission must stay granted to the main webview.

#[test]
fn tauri_config_registers_the_bibcode_deep_link_scheme() {
    let config: serde_json::Value =
        serde_json::from_str(include_str!("../tauri.conf.json")).expect("valid tauri.conf.json");
    let schemes = config
        .pointer("/plugins/deep-link/desktop/schemes")
        .and_then(|value| value.as_array())
        .expect("plugins.deep-link.desktop.schemes present");
    assert_eq!(
        schemes
            .iter()
            .filter_map(|value| value.as_str())
            .collect::<Vec<_>>(),
        vec!["bibcode"],
    );
}

#[test]
fn default_capability_grants_deep_link_permission() {
    let capability: serde_json::Value =
        serde_json::from_str(include_str!("../capabilities/default.json"))
            .expect("valid default capability");
    let permissions = capability
        .get("permissions")
        .and_then(|value| value.as_array())
        .expect("permissions array present");
    assert!(
        permissions
            .iter()
            .filter_map(|value| value.as_str())
            .any(|permission| permission == "deep-link:default"),
        "deep-link:default permission missing from capabilities/default.json",
    );
}
```

- [x] **Step 2: Run to verify it fails**

Run: `cargo test -p bibcode-desktop --test deep_link_config`
Expected: FAIL — `plugins.deep-link` pointer is absent.

- [x] **Step 3: Implement the Rust/config side**

Root `Cargo.toml` `[workspace.dependencies]` (next to the existing tauri-plugin entries; let
cargo resolve the current 2.x):

```toml
tauri-plugin-deep-link = "2"
tauri-plugin-single-instance = { version = "2", features = ["deep-link"] }
```

`apps/desktop/src-tauri/Cargo.toml` `[dependencies]`:

```toml
tauri-plugin-deep-link.workspace = true
tauri-plugin-single-instance.workspace = true
```

`apps/desktop/src-tauri/src/lib.rs` — the single-instance plugin must be registered **first**
so a second launch forwards and exits; with its `deep-link` feature the forwarded argv is
routed to the deep-link plugin's event automatically:

```rust
    let builder = tauri::Builder::<bridge::DesktopRuntime>::new()
        .plugin(tauri_plugin_single_instance::init(|_app, _argv, _cwd| {}))
        .manage(backend::BackendSupervisor::new())
        // …existing .manage calls unchanged…
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build());
```

and inside the existing `.setup(move |app| { … })` closure, after
`window::restore_main_window_state(app.handle())?;`:

```rust
        // Dev/portable builds on Windows and Linux register the scheme at
        // runtime; installed builds get it from the bundler config. macOS only
        // supports bundle-time registration (dev-mode deep links are a known
        // no-op there — covered in the desktop runbooks).
        #[cfg(any(windows, target_os = "linux"))]
        {
            use tauri_plugin_deep_link::DeepLinkExt;
            if let Err(error) = app.deep_link().register_all() {
                tracing::warn!("failed to register bibcode:// deep-link handler: {error}");
            }
        }
```

`apps/desktop/src-tauri/tauri.conf.json` — extend the existing `plugins` object:

```json
  "plugins": {
    "updater": {
      "pubkey": "",
      "endpoints": []
    },
    "deep-link": {
      "desktop": {
        "schemes": ["bibcode"]
      }
    }
  },
```

`apps/desktop/src-tauri/capabilities/default.json` — add `"deep-link:default"` to
`permissions` (the default set includes `get-current` and the new-url event).

Run: `cargo test -p bibcode-desktop --test deep_link_config` — Expected: PASS.
Then `cargo fmt --all --check` and
`cargo clippy -p bibcode-desktop --all-targets -- -D warnings`.

- [x] **Step 4: Write the failing web test for URL parsing**

```ts
// apps/web/src/desktopDeepLink.test.ts
import { describe, expect, it } from "vite-plus/test";

import { resolvePairingDeepLink } from "./desktopDeepLink";

describe("resolvePairingDeepLink", () => {
  it("extracts the code from bibcode://pair deep links", () => {
    expect(resolvePairingDeepLink("bibcode://pair?code=abc123-_")).toEqual({
      code: "abc123-_",
    });
  });

  it("rejects other schemes, hosts, and codeless links", () => {
    expect(resolvePairingDeepLink("https://pair?code=abc")).toBeNull();
    expect(resolvePairingDeepLink("bibcode://other?code=abc")).toBeNull();
    expect(resolvePairingDeepLink("bibcode://pair")).toBeNull();
    expect(resolvePairingDeepLink("not a url")).toBeNull();
  });
});
```

Run: `vp test run apps/web/src/desktopDeepLink.test.ts` — Expected: FAIL (module missing).

- [x] **Step 5: Implement the web side**

`packages/contracts/src/ipc.ts` (contracts stay schema/type-only — a constant and two
optional function types match the existing `DesktopBridge` style):

```ts
/** Tauri deep-link plugin runtime event carrying the opened URLs. */
export const DESKTOP_DEEP_LINK_EVENT = "deep-link://new-url";

// In interface DesktopBridge, alongside the other optional members:
  /** URLs the OS handed to the app before the webview subscribed (cold start). */
  getPendingDeepLinks?: () => Promise<ReadonlyArray<string>>;
  /** Subscribe to bibcode:// URLs opened while the app is running. */
  onDeepLink?: (listener: (urls: ReadonlyArray<string>) => void) => () => void;
```

`apps/web/src/tauriDesktopBridge.ts` — in the object that already defines
`onProjectDataStatusChanged`:

```ts
    getPendingDeepLinks: async () =>
      (await tauriInvoke<ReadonlyArray<string> | null>("plugin:deep-link|get_current")) ?? [],
    onDeepLink: (listener: (urls: ReadonlyArray<string>) => void) =>
      tauriListen<ReadonlyArray<string>>(DESKTOP_DEEP_LINK_EVENT, listener),
```

`apps/web/src/desktopDeepLink.ts`:

```ts
import { useEffect } from "react";
import { useNavigate } from "@tanstack/react-router";

export function resolvePairingDeepLink(rawUrl: string): { readonly code: string } | null {
  let url: URL;
  try {
    url = new URL(rawUrl);
  } catch {
    return null;
  }
  if (url.protocol !== "bibcode:") return null;
  // WHATWG parsing of bibcode://pair?code=x puts "pair" in the host for
  // non-special schemes; some platforms surface it as the path instead.
  const isPairTarget =
    url.hostname === "pair" || url.pathname === "/pair" || url.pathname === "//pair";
  if (!isPairTarget) return null;
  const code = url.searchParams.get("code")?.trim() ?? "";
  return code.length > 0 ? { code } : null;
}

/** Mounted once by the root route in desktop mode; renders nothing. */
export function DesktopDeepLinkRouter() {
  const navigate = useNavigate();

  useEffect(() => {
    const bridge = window.desktopBridge;
    if (!bridge?.onDeepLink) return;
    const handleUrls = (urls: ReadonlyArray<string>) => {
      for (const rawUrl of urls) {
        const pairing = resolvePairingDeepLink(rawUrl);
        if (pairing) {
          void navigate({ to: "/pair", search: { code: pairing.code } });
          return;
        }
      }
    };
    void bridge
      .getPendingDeepLinks?.()
      .then(handleUrls)
      .catch(() => undefined);
    return bridge.onDeepLink(handleUrls);
  }, [navigate]);

  return null;
}
```

`apps/web/src/routes/__root.tsx` — in `RootRouteView`'s authenticated branch, next to
`<SlowRpcRequestToastCoordinator />`:

```tsx
<DesktopDeepLinkRouter />
```

(Browser mode is a no-op: `window.desktopBridge` is undefined.)

- [x] **Step 6: Run to verify it passes**

Run: `vp test run apps/web/src/desktopDeepLink.test.ts` — Expected: PASS.
Then `vp run typecheck` (contracts + web + bridge additions compile together) and re-run
`cargo test -p bibcode-desktop --test deep_link_config`,
`cargo fmt --all --check`, `cargo clippy -p bibcode-desktop --all-targets -- -D warnings`.

- [x] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock apps/desktop/src-tauri packages/contracts/src/ipc.ts \
  apps/web/src/tauriDesktopBridge.ts apps/web/src/desktopDeepLink.ts \
  apps/web/src/desktopDeepLink.test.ts apps/web/src/routes/__root.tsx
git commit -m "feat(desktop): register bibcode:// deep links and route pair codes"
```

---

### Task 9b: D8 settings data-source audit (bounded)

Spec D8 pins the split: **server-owned settings are per-environment** (served by the
runtime they configure) while **client/UI settings stay on the device** (appearance,
keybindings). No task anywhere implements or verifies this; before this phase ships the
renamed settings surface, audit that every settings panel reads/writes the data source D8
assigns it. This is an audit with in-place fixes for trivial leaks only — environment
_selection_ scoping is Phase 6; do not add environment pickers or re-scope panels here.

**Files:**

- Read: every `apps/web/src/routes/settings.*.tsx` and the components they render
  (`apps/web/src/components/settings/`).
- Modify: only files with trivial leaks (definition below), plus their closest existing
  test.
- Modify: this plan file's "Residual risks / follow-ups" section (append findings).

**Interfaces:**

- Consumes: `useClientSettings` (`apps/web/src/hooks/useSettings.ts` — device-local store),
  `usePrimaryEnvironment` / `primaryServerConfigAtom` and the environment-scoped query
  atoms (`useEnvironmentQuery` with `environmentId`-keyed inputs — server-owned per D8).
- Produces: an audit table in the final report + appended residual-risk entries. No new
  exports.

- [x] **Step 1: Enumerate every section's data sources**

```bash
ls apps/web/src/routes/settings.*.tsx
rg -n "useClientSettings|useEnvironmentQuery|usePrimaryEnvironment|primaryServerConfig|desktopBridge" \
  apps/web/src/components/settings --glob '!*test*' -l
```

Build a table (final-report artifact, not a committed file): section → component →
data source(s) → D8 classification (`device-local` / `server-owned:<which environment>` /
`desktop-host`). Every row must name the concrete atom/hook, not a guess — open each
component far enough to see its reads _and_ writes.

- [x] **Step 2: Verify server-owned panels target the environment they claim**

For each row classified server-owned, confirm both directions (read and mutation) are
keyed by the environment the UI presents. Two known-shape checks to perform explicitly:

1. Panels that configure "this server" (Providers, Agents, Diagnostics, Local environment)
   must read through the primary environment's atoms — not a mix of primary reads with
   unscoped writes (or vice versa).
2. Panels that are device-local per D8 (General/appearance, Keybindings, Status Bar,
   Terminal appearance) must not persist through a server RPC; if one does, record it —
   moving a persistence home is **not** trivial (it changes a persisted shape; spec
   amendment territory).

- [x] **Step 3: Fix trivial leaks in place**

Trivial means all of: single file, no contract/schema/persistence change, no new
cross-package dependency, and an existing test file that can gain the covering assertion.
Example shape: a panel deriving copy from a global/unscoped atom where the
environment-scoped equivalent already exists — swap the read, extend the component's test.
Anything bigger: do **not** fix; record it.

- [x] **Step 4: Record non-trivial findings**

Append each to "Residual risks / follow-ups" below as
`D8 audit: <section> — <finding> — <suggested owner phase or follow-up>`. If the audit
finds nothing, append `D8 audit: no violations found (audited <date>)`.

- [x] **Step 5: Validate and commit**

Run the extended tests from Step 3 plus `vp run typecheck`. Expected: PASS.

```bash
git add -A apps/web/src/components/settings docs/plans/remote-servers/phases/phase-4-settings-connect-tab.md
git commit -m "chore(web): audit settings data sources against D8 and fix trivial leaks"
```

(Commit only what actually changed; an all-clear audit commits just the plan-file note.)

---

### Task 10: Living docs, testing runbooks, and the full validation gate

**Files:**

- Modify: `docs/architecture/remote.md`
- Review (update only if they mention the Connections settings section):
  `docs/architecture/connection-runtime.md`, `docs/architecture/overview.md`
- Modify: `docs/testing/macos-desktop.md`, `docs/testing/windows-desktop.md`,
  `docs/testing/linux-desktop.md`

- [x] **Step 1: Update `docs/architecture/remote.md`**

Add a short section (placed near the pairing-code material Phase 3 documented) covering, in
present tense: the settings section is **Remote Servers** at `/settings/remote-servers`
(`/settings/connections` redirects; the Windows WSL page lives at
`/settings/local-environment`); Connect-tab contents (saved-server rows with version/compat/
transport, pairing-code Add Server, Advanced manual endpoint+token, SSH first-class, relay
rows unchanged); deep-link entry points (`bibcode://pair?code=…` registered via the Tauri
deep-link + single-instance plugins; web `/pair?code=…`; both converge on the Add Server
flow); and the macOS dev-mode limitation (scheme registration is bundle-time only there).

- [x] **Step 2: Review the other two architecture docs**

`rg -n -i "connections" docs/architecture/connection-runtime.md docs/architecture/overview.md`
— update any reference to the Connections settings section/route to Remote Servers; if none
exist, record "reviewed and remain accurate" in the final report.

- [x] **Step 3: Update the three desktop runbooks**

Each currently asserts (macos-desktop.md:189, windows-desktop.md:317, linux-desktop.md:149)
that Connections/SSH/pairing/relay/exposure UI is **absent** from ordinary desktop
presentation. That assertion is obsolete by design (spec D2): replace those bullets with the
new expectation, e.g. for macOS/Linux:

> - Settings shows a **Remote Servers** section with "Connect to a host" and
>   "Share this host" tabs; `/settings/connections` redirects to it. SSH discovery and
>   share-side exposure controls appear because the desktop bridge is present. Add Project
>   still offers no Host selector for the local machine (environment-rail scoping arrives in
>   a later phase).

and for Windows additionally:

> - "Local environment" remains in the settings nav and now lives at
>   `/settings/local-environment`; it is never empty.

Add a packaged-flow validation step to each runbook's native visual validation list:
opening `bibcode://pair?code=<any well-formed code>` from the OS while the app runs focuses
the running instance (Windows/Linux) and lands on the Add Server dialog with the code
prefilled. Keep runbooks free of execution-specific data (versions, timings, screenshots).

- [x] **Step 4: Full validation gate (report exact commands + outcomes)**

```bash
vp check
vp run typecheck
vp run --filter @bibcode/web test
cargo fmt --all --check
cargo clippy -p bibcode-desktop --all-targets -- -D warnings
cargo test -p bibcode-desktop
git status --short && git diff --stat
```

Review the diff for unintended edits (especially: no changes under `.codegraph/`, no
restoration of the pending deletions under `docs/plans/2026-08-24-environment-project-management/`,
no stray debug output, `Cargo.lock` changes limited to the two new plugins).

- [x] **Step 5: Commit**

```bash
git add docs/architecture docs/testing
git commit -m "docs: describe Remote Servers settings section and deep-link entry points"
```

---

## Self-review checklist (run after writing code, before declaring the phase done)

1. **Spec coverage (§4.8 Connect side + D7 + §6):** rename + redirect (Task 4); two tabs
   with spec-verbatim names (Task 3); saved-server rows with status/version/compat/update
   surface (Tasks 5, 7); non-destructive Disconnect latch + Remove behind a confirmation
   that escalates on running work (Task 5b, spec §6); Add Server via pairing code with the
   five classified `PairingAddError` reasons and the loopback tunnel acknowledgement —
   both proactive (pre-decode) and reactive (`PairingLoopbackAcknowledgementRequiredError`
   → retry with `allowLoopbackTunnel: true`) (Task 6); Advanced manual entry preserved
   (Task 6); troubleshooting expander (Task 6); Check for Server Updates placement
   (Task 7); deep-link entry points incl. the fresh-unauthenticated `/pair?code=…` path
   consuming the embedded token (Tasks 8, 9, amended spec §4.2); D8 data-source audit
   (Task 9b); D12 SSH/relay first-class (Tasks 3, 6); policy degradation — SSH and
   exposure controls hidden without the desktop bridge (preserved existing `desktopBridge`
   branches, Task 3).
2. **Out of scope (verify none leaked in):** no Share-tab evolution beyond the verbatim move
   (Phase 5); no environment rail or `presentsTarget` changes, no environment re-scoping of
   settings panels (Phase 6); no `updater.*` RPC (Phase 7); no changes to
   `packages/contracts` beyond the two optional `DesktopBridge` members and one event
   constant (no WS methods → no parity-manifest or scope entries needed); the disconnect
   latch stays session-scoped (no persisted catalog field).
3. **Copy audit:** all new user-facing strings use environments/server terminology, "Local",
   and `BiBCode v…`; no reference-product strings anywhere in the diff.
4. **Cross-phase names (all pinned by the sibling phase plans — verify against landed
   source at execution time, then align, never re-declare):** Phase 2's
   `environmentSession.compatVerdictAtom(environmentId)`; Phase 3's
   `verifyAndAddPairingCode` + `VerifyPairingCodeInput` (`allowLoopbackTunnel`),
   `PairingAddError.reason` / `PairingAddFailureReason`,
   `PairingLoopbackAcknowledgementRequiredError`, `parsePairingCode` / `encodePairingCode`,
   `classifyPairingEndpoint` (4-value union), required-nullable
   `BearerConnectionProfile.hostKey: string | null`. Each sits behind a single Phase 4-owned
   access point (row view-model, atom wrapper, `connectPresentation` helpers,
   `pairingCodeCredential`), so realignment stays a one-line change each. Error handling
   reads `reason` (never `kind`) and dispatches by `_tag`.

## Residual risks / follow-ups

Running list — the final report repeats it; Task 9b appends to it.

- Share-tab content becomes newly reachable on macOS/Linux desktop (previously
  `redirect-general`); its `desktopBridge` branches were never user-reachable there.
  Phase 5 validates that surface as it evolves the tab (design decision 3).
- `@base-ui/react/tabs` availability could not be verified in this worktree (no installed
  `node_modules`); Task 2 carries a resolve-check and an aria-correct fallback.
- macOS dev-mode deep links are bundle-time-only (Task 9); covered in runbooks, not
  testable unbundled.
- The disconnect latch is session-scoped by design (Task 5b scope note); persisting it
  across restarts would be a catalog-profile/spec change.
- D8 audit: Keybindings — the panel reads `primaryServerKeybindingsAtom` and writes through
  `serverEnvironment.upsertKeybinding` / `serverEnvironment.removeKeybinding`, so bindings follow
  the primary server instead of the client device — follow up with a contracts and persistence
  migration (audited 2026-08-27).
- D8 audit: Terminal — the WebGL renderer preference is `ServerSettings.terminal.webglEnabled`
  and therefore persists through the primary server RPC even though renderer presentation is a
  client/UI concern — follow up with a settings-schema and persistence migration (audited
  2026-08-27).
