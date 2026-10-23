# Phase 6 — Environment Rail & Context Card Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use
> checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the left-panel environment rail (Local + saved remote environments, selection
scoping the whole panel) and the environment context card, make "Add project" name its target
environment, and fix the primary-environment editor-list leak in sidebar rows.

**Architecture:** Pure view-model modules (`*.logic.ts`) drive two new presentational
components (`EnvironmentRail`, `EnvironmentContextCard`) mounted around the existing
`Sidebar.tsx` mega-component. Selection is presentation-only: the rail writes the existing
`activeEnvironmentIdAtom` and the panel filters what it _shows_; RPC routing continues to
follow each entity's own `environmentId` (spec D3/D4). No new cross-phase types are
introduced (master plan, "Cross-phase interfaces").

**Tech Stack:** React 19 + Effect Atom (`@effect/atom-react`), TanStack Router, Base UI menu
primitives (`apps/web/src/components/ui/menu.tsx`), Tailwind v4 theme tokens
(`apps/web/src/index.css`), vite-plus (`vp`) test runner with the repo's SSR
(`renderToStaticMarkup`) + capture-mock test conventions.

**Spec:** `docs/plans/remote-servers/remote-servers-spec.md` — §4.8 (UI contracts, verbatim),
decisions D3 (soft switch), D4 (entity-ownership routing), D5 (icon rail, mockup Variant B),
D6 (Local groups WSL backends via sub-picker), D16 (naming). Approved visual reference:
`docs/plans/remote-servers/mockups/left-panel-switcher.html`, **Variant B**. Master plan:
`docs/plans/remote-servers/remote-servers-plan.md` (this file is Phase 6; depends on Phases 2
and 4; Phase 7 depends on this phase).

## Global Constraints

(Copied from the master plan; every task's requirements implicitly include these.)

- Zero reference-product strings in code, identifiers, UI copy, or comments; product
  strings are "BiBCode"/"bibcode" by context (spec D16).
- `packages/contracts` stays schema-only; every new WS method gets a Rust mirror and an
  entry in the TS↔Rust parity manifests; every RPC method declares exactly one scope in
  `apps/server/src/auth/scope.rs`. (Phase 6 adds no WS methods and must not touch
  `packages/contracts`.)
- All new descriptor/contract fields are additive and decode-defaulted so older servers
  keep working (no breaking wire changes).
- No production Node runtime, no Electron, no sidecars; desktop-privileged operations
  cross `DesktopBridge`; normal traffic uses typed HTTP/WS RPC.
- Preserve unrelated worktree changes — in particular the user's pending deletions under
  `docs/plans/2026-08-24-environment-project-management/` must never be restored or
  committed by this work. **Commit only the explicit file lists given in each task's commit
  step; never `git add -A` / `git add .`.**
- Every phase: focused tests for changed behavior, `vp check`, `vp run typecheck`; Rust
  phases additionally `cargo fmt --all --check`, relevant Rust tests, and Clippy for
  affected targets with warnings denied; final `git diff`/`git status --short` review.
  (Phase 6 touches no Rust.)
- Living docs (`docs/architecture/remote.md`, `connection-runtime.md`, `overview.md`) and
  `docs/testing/` runbooks update in the same patch as the behavior they describe; phases
  that change no runbook-relevant behavior state "reviewed and remain accurate".

## Execution prerequisites

- Follow the repo `AGENTS.md` pre-work (read the docs index, run `git status --short`,
  run `codegraph sync .` if the binary is available).
- If `node_modules` is missing in this worktree, run `vp install` at the repo root once —
  every test command below fails with `ERR_MODULE_NOT_FOUND` without it.
- Focused test command form (run from the repo root):
  `vp test run --project unit apps/web/src/<path-to-test-file>`.

## Phase interfaces

**Consumes (must exist before the corresponding task runs):**

- Phase 2 — `CompatVerdict` type and a verdict function from
  `packages/client-runtime/src/connection/compat.ts`, re-exported via
  `@bibcode/client-runtime/connection` (the package's `./connection` export maps to
  `src/connection/index.ts`, which `export *`s its modules). This plan imports it as
  `computeCompatVerdict(descriptor)` taking the descriptor's
  `remoteProtocolVersion`/`minCompatibleRemoteProtocol` fields (spec §4.4). If Phase 2
  exported a different callable name, the fix is one line in
  `apps/web/src/connection/environmentCompat.ts` (Task 1) — nothing else in this phase
  imports it directly.
- Phase 4 — the `/settings/remote-servers` route (`apps/web/src/routes/settings.remote-servers.tsx`)
  with the renamed nav item, and a per-environment **disconnect latch** command atom
  `environmentCatalog.disconnect(environmentId)` (spec §6: "Manual Disconnect is a
  client-side latch on the saved environment (supervisor desired-state = disconnected); it
  never deletes credentials"), surfaced from `createEnvironmentCatalogAtoms`
  (`packages/client-runtime/src/state/connections.ts`) like the existing
  `retryNow`/`remove` commands. **If Phase 4 landed without this command, Task 6 is
  blocked — report the gap; do not implement the latch inside Phase 6.**
- Existing seams (verified against source on 2026-08-27):
  - `activeEnvironmentIdAtom` + `useActiveEnvironmentId`/`setActiveEnvironmentId`/
    `readActiveEnvironmentId` — `apps/web/src/state/entities.ts` (~line 59).
  - `useEnvironments`/`useEnvironment`/`usePrimaryEnvironmentId` —
    `apps/web/src/state/environments.ts`; presentation shape `{ entry, connection, serverConfig,
    environmentId, label, displayUrl, relayManaged }` with
    `connection.phase: "available" | "offline" | "connecting" | "reconnecting" | "connected" | "error"`
    (`packages/client-runtime/src/connection/presentation.ts`).
  - `DESKTOP_LOCAL_CONNECTION_ID_PREFIX` (`"local:"`) and `isDesktopLocalConnectionTarget`
    — `apps/web/src/connection/desktopLocal.ts` (D6 grouping key).
  - `environmentServerConfigsAtom` (`Map<EnvironmentId, ServerConfig>`) —
    `apps/web/src/state/server.ts`, already surfaced in `Sidebar.tsx` as
    `const serverConfigs = useServerConfigs()`.
  - `connectionStatusText` — `packages/client-runtime/src/connection/presentation.ts`.

**Produces (Phase 7 relies on these exact names):**

- `EnvironmentContextCard` props `updateBadge?: React.ReactNode` (badge slot next to the
  version line) and `onCheckForUpdates?: (environmentId: EnvironmentId) => void` (menu item
  handler). Phase 7 wires both; until then the badge slot renders nothing and the
  "Check for updates" menu item is **hidden** (hidden-until-capable — see Design decisions).
- `selectRemoteUpdateControlCapability(serverConfig)` in
  `apps/web/src/connection/environmentCompat.ts` — Phase 7 replaces its defensive read with
  the typed contract field it introduces.
- **Amber-dot input (amended §4.8: Phase 7 wires the update input into the Phase 6 dot):**
  `EnvironmentRailCandidate.updateAvailable: boolean`, a required pass-through parameter of
  `toEnvironmentRailCandidate` and of
  `resolveEnvironmentRailStatus({ phase, compat, updateAvailable })`. Phase 6 passes a
  constant `false` at the single `toEnvironmentRailCandidate` call site in
  `EnvironmentRail.tsx`. Phase 7's wiring step (pinned here so it needs no guessing):
  implement `useEnvironmentUpdateAvailability(): ReadonlyMap<EnvironmentId, boolean>`
  (true when its `RemoteUpdateSnapshot.state === "update-available"`), call it in
  `EnvironmentRail`, and replace that constant with
  `updateAvailability.get(environment.environmentId) ?? false`. No other Phase 6 file
  participates in the amber-dot input.

## Design decisions (argued once here; tasks implement them)

1. **The rail always renders, including with zero saved remotes** (browser and desktop, and
   in the mobile sheet). It is the discoverability surface for the whole feature: with no
   remotes it shows Local (selected), no divider, and the bottom "Add server…" / "Manage
   remote servers…" actions. Hiding it until a remote exists would leave no visible path to
   adding one from the main UI.
2. **Mount point: inside `<Sidebar>` as a flex-row column** (Task 4). The desktop sidebar is
   `position: fixed; left: 0` with a width-matched gap element
   (`apps/web/src/components/ui/sidebar.tsx`, `Sidebar`), so a plain flex sibling would be
   overlapped. Mounting inside means the rail collapses together with the offcanvas sidebar
   — acceptable: the toggle hides the whole left panel, and when open the layout is
   identical to mockup Variant B. `THREAD_SIDEBAR_MIN_WIDTH` grows by the rail width so the
   projects panel keeps its current minimum.
3. **"Local" = the primary environment plus every desktop-local (`local:`-prefixed)
   backend** (D6: WSL is "this machine"). Selecting Local shows the union of local
   environments' projects/threads (preserving today's merged local view); the sub-picker
   (shown only when desktop-local backends exist) sets which local environment is _active_
   (add-project default target). The rail highlights Local whenever the active environment
   is any local one. **A null/absent `activeEnvironmentId` means Local is selected AND the
   panel filters to Local** (amended §4.8) — "no selection" must never render as "show
   everything". One degenerate case: a catalog with no local environment at all (hosted
   static bootstrap before its first-saved-environment effect in
   `apps/web/src/routes/__root.tsx` runs) has no Local to scope to; the selector returns
   "no filtering" there for the instant until that effect selects the first saved
   environment, rather than blanking the panel.
4. **Selection writes `activeEnvironmentIdAtom` and nothing else** (D3 soft switch). No
   registry/supervisor call, no navigation. Tests pin this. Entity operations keep routing
   by the entity's own `environmentId` (D4) — the state layer already guarantees it; tests
   pin that selection does not rewire the editor-list/context-menu paths.
5. **"Check for updates" is hidden-until-capable.** The menu item renders only when the
   server advertises the `remoteUpdateControl` capability (spec §4.5, default-false,
   delivered by Phase 7) _and_ Phase 7 has injected `onCheckForUpdates`. A disabled-but-visible
   item would advertise an action no shipped server supports yet; hidden-until-capable also
   matches the repo's capability-downgrade convention (default-false booleans gate whole
   surfaces).
6. **Copy scope is spec §4.8 verbatim.** Only the "Add project" affordances gain
   "on \<name\>"; the panel's "Projects" section label stays "Projects". This deliberately
   diverges from the mockup's "Projects on AI-SERVER" section label — the mockup is a visual
   reference, §4.8 is the contract. Do not "fix" this during execution.
7. **Rail entry classification:** remote entries are catalog environments whose target is
   neither `PrimaryConnectionTarget` nor desktop-local — i.e. saved SSH, direct (Bearer),
   and relay environments, matching §4.8. The rail lists them regardless of
   `EnvironmentPresentationPolicy.presentsTarget` (that policy predates this feature and
   hides remote targets on desktop; Phase 4 evolves settings presentation, and the rail is
   the remote-environment surface by design). Degraded states render as status dots, not as
   missing entries.
8. **Stale selection self-heals.** If the active environment disappears from the catalog
   (removed in settings, relay teardown), the rail resets selection to Local once the
   catalog is ready. While the ghost id persists (at most one effect tick), the scope
   selector already treats an unresolvable selection as Local — the panel never flashes
   "show everything" on the way to the reset.

## File structure

| File                                                                                   | Status | Responsibility                                                                                          |
| -------------------------------------------------------------------------------------- | ------ | ------------------------------------------------------------------------------------------------------- |
| `apps/web/src/connection/environmentCompat.ts` (+`.test.ts`)                           | new    | Adapter over Phase 2's verdict fn; `remoteUpdateControl` capability selector                            |
| `apps/web/src/components/sidebar/environmentRail.logic.ts` (+`.test.ts`)               | new    | Pure rail view-model: status, avatar, model builder, visible-environment scoping, add-project label     |
| `apps/web/src/components/sidebar/EnvironmentRail.tsx` (+`.test.tsx`)                   | new    | 52px rail component (§4.8), radiogroup semantics, WSL sub-picker, bottom actions, stale-selection reset |
| `apps/web/src/components/sidebar/environmentContextCard.logic.ts` (+`.test.ts`)        | new    | Card view-model: visibility rule, status/version line, compat badge                                     |
| `apps/web/src/components/sidebar/EnvironmentContextCard.tsx` (+`.test.tsx`)            | new    | Context card (§4.8): name/status/version/badges + ⋯ menu                                                |
| `apps/web/src/components/AppSidebarLayout.tsx`                                         | modify | Mount rail inside `<Sidebar>`; min-width bump                                                           |
| `apps/web/src/components/Sidebar.tsx`                                                  | modify | Panel scoping by selection; card mount under brand row; editor-list leak fix; add-project label plumb   |
| `apps/web/src/components/CommandPalette.tsx`                                           | modify | "Add project" action title gains "on \<name\>"                                                          |
| `apps/web/src/components/add-project/useAddProjectWorkflow.ts`                         | modify | Default host = active environment; remote environments become hosts                                     |
| `apps/web/src/routes/settings.remote-servers.tsx`                                      | modify | Accept `?action=add-server` deep link (coordinate with Phase 4)                                         |
| `docs/architecture/connection-runtime.md`, `docs/testing/cross-platform-validation.md` | modify | Living docs + packaged-visual-validation runbook                                                        |

Naming note: `apps/web/src/components/ui/sidebar.tsx` already exports a `SidebarRail`
(the resize/collapse handle). The new component is `EnvironmentRail` — do not rename or
reuse `SidebarRail`.

---

### Task 1: Compat adapter and update-capability selector

**Files:**

- Create: `apps/web/src/connection/environmentCompat.ts`
- Test: `apps/web/src/connection/environmentCompat.test.ts`

**Interfaces:**

- Consumes: Phase 2's `computeCompatVerdict(descriptor)` + `CompatVerdict` from
  `@bibcode/client-runtime/connection` (see Phase interfaces; a Phase 2 rename lands here
  and only here).
- Produces:
  - `resolveEnvironmentCompatVerdict(serverConfig: ServerConfig | null): CompatVerdict | null`
  - `selectRemoteUpdateControlCapability(serverConfig: ServerConfig | null): boolean`
    Both consumed by Tasks 2, 3, and 6.

- [x] **Step 1: Write the failing test**

```ts
// apps/web/src/connection/environmentCompat.test.ts
import type { ServerConfig } from "@bibcode/contracts";
import { describe, expect, it, vi } from "vite-plus/test";

const computeCompatVerdict = vi.hoisted(() => vi.fn());
vi.mock("@bibcode/client-runtime/connection", async (importOriginal) => ({
  ...(await importOriginal<Record<string, unknown>>()),
  computeCompatVerdict,
}));

import {
  resolveEnvironmentCompatVerdict,
  selectRemoteUpdateControlCapability,
} from "./environmentCompat";

function serverConfigWith(environment: Record<string, unknown>): ServerConfig {
  return { environment } as unknown as ServerConfig;
}

describe("resolveEnvironmentCompatVerdict", () => {
  it("returns null when the environment has never delivered a config", () => {
    expect(resolveEnvironmentCompatVerdict(null)).toBeNull();
    expect(computeCompatVerdict).not.toHaveBeenCalled();
  });

  it("delegates to the client-runtime verdict for a delivered descriptor", () => {
    const descriptor = { remoteProtocolVersion: 1, minCompatibleRemoteProtocol: 1 };
    computeCompatVerdict.mockReturnValue({ kind: "compatible" });
    expect(resolveEnvironmentCompatVerdict(serverConfigWith(descriptor))).toEqual({
      kind: "compatible",
    });
    expect(computeCompatVerdict).toHaveBeenCalledWith(descriptor);
  });
});

describe("selectRemoteUpdateControlCapability", () => {
  it("defaults to hidden for null config and for servers without the capability", () => {
    expect(selectRemoteUpdateControlCapability(null)).toBe(false);
    expect(selectRemoteUpdateControlCapability(serverConfigWith({ capabilities: {} }))).toBe(false);
  });

  it("is true only for an explicit capability boolean", () => {
    expect(
      selectRemoteUpdateControlCapability(
        serverConfigWith({ capabilities: { remoteUpdateControl: true } }),
      ),
    ).toBe(true);
    expect(
      selectRemoteUpdateControlCapability(
        serverConfigWith({ capabilities: { remoteUpdateControl: "yes" } }),
      ),
    ).toBe(false);
  });
});
```

- [x] **Step 2: Run the test to verify it fails**

Run: `vp test run --project unit apps/web/src/connection/environmentCompat.test.ts`
Expected: FAIL — module `./environmentCompat` not found.

- [x] **Step 3: Write the implementation**

```ts
// apps/web/src/connection/environmentCompat.ts
import { computeCompatVerdict, type CompatVerdict } from "@bibcode/client-runtime/connection";
import type { ServerConfig } from "@bibcode/contracts";

/**
 * Compat verdict for an environment (spec §4.4). `null` means the environment
 * has never delivered a descriptor (never connected in this session and no
 * cached config) — render "Status unavailable"-style degradation, not "legacy".
 */
export function resolveEnvironmentCompatVerdict(
  serverConfig: ServerConfig | null,
): CompatVerdict | null {
  if (serverConfig === null) {
    return null;
  }
  return computeCompatVerdict(serverConfig.environment);
}

/**
 * Whether the environment's server advertises remote update control
 * (spec §4.5 `remoteUpdateControl`, default-false capability boolean).
 *
 * Phase 7 introduces the typed contract field; until then this reads the
 * capability defensively off the decoded descriptor so Phase 6 ships no
 * contract change. Phase 7 replaces the cast with the typed field — this
 * function is the only place that reads it.
 */
export function selectRemoteUpdateControlCapability(serverConfig: ServerConfig | null): boolean {
  if (serverConfig === null) {
    return false;
  }
  const capabilities = serverConfig.environment.capabilities as {
    readonly remoteUpdateControl?: unknown;
  };
  return capabilities.remoteUpdateControl === true;
}
```

If Phase 2's export is not named `computeCompatVerdict`, adjust the import line (and the
test's mock key) to the name Phase 2 shipped — verify with
`rg "export (function|const)" packages/client-runtime/src/connection/compat.ts`.

- [x] **Step 4: Run the test to verify it passes**

Run: `vp test run --project unit apps/web/src/connection/environmentCompat.test.ts`
Expected: PASS (4 tests).

- [x] **Step 5: Commit**

```bash
git add apps/web/src/connection/environmentCompat.ts apps/web/src/connection/environmentCompat.test.ts
git commit -m "feat(web): add environment compat + update-capability selectors"
```

---

### Task 2: Environment-rail view-model (pure logic)

**Files:**

- Create: `apps/web/src/components/sidebar/environmentRail.logic.ts`
- Test: `apps/web/src/components/sidebar/environmentRail.logic.test.ts`

**Interfaces:**

- Consumes: `isDesktopLocalConnectionTarget` (`apps/web/src/connection/desktopLocal.ts`),
  `compareSidebarDisplayText` (`apps/web/src/sidebarProjectGrouping.ts`), `CompatVerdict`
  type, `EnvironmentConnectionPhase` type.
- Produces (consumed by Tasks 3, 5, 6, 8):
  - `type EnvironmentRailStatus = "connected" | "disconnected" | "attention" | "error"`
  - `interface EnvironmentRailCandidate { environmentId; label; isPrimary; isDesktopLocal; phase; compat; updateAvailable }`
  - `toEnvironmentRailCandidate(input): EnvironmentRailCandidate`
  - `isLocalRailCandidate(candidate): boolean`
  - `resolveEnvironmentRailStatus(input): EnvironmentRailStatus`
  - `environmentLetterAvatar(label: string): string`
  - `buildEnvironmentRailModel(input): EnvironmentRailModel`
  - `selectRailVisibleEnvironmentIds(input): ReadonlySet<EnvironmentId> | null`
  - `resolveAddProjectTargetLabel(input): string | null`

- [x] **Step 1: Write the failing test**

```ts
// apps/web/src/components/sidebar/environmentRail.logic.test.ts
import type { ConnectionTarget } from "@bibcode/client-runtime/connection";
import { EnvironmentId } from "@bibcode/contracts";
import { describe, expect, it } from "vite-plus/test";

import {
  buildEnvironmentRailModel,
  environmentLetterAvatar,
  resolveAddProjectTargetLabel,
  resolveEnvironmentRailStatus,
  selectRailVisibleEnvironmentIds,
  toEnvironmentRailCandidate,
  type EnvironmentRailCandidate,
} from "./environmentRail.logic";

const ENV_PRIMARY = EnvironmentId.make("env-primary");
const ENV_WSL = EnvironmentId.make("env-wsl");
const ENV_REMOTE_A = EnvironmentId.make("env-remote-a");
const ENV_REMOTE_B = EnvironmentId.make("env-remote-b");

function candidate(
  overrides: Partial<EnvironmentRailCandidate> & { environmentId: EnvironmentId },
): EnvironmentRailCandidate {
  return {
    label: "AI-SERVER",
    isPrimary: false,
    isDesktopLocal: false,
    phase: "connected",
    compat: null,
    updateAvailable: false,
    ...overrides,
  };
}

const primary = candidate({ environmentId: ENV_PRIMARY, label: "Local", isPrimary: true });
const wsl = candidate({ environmentId: ENV_WSL, label: "Ubuntu", isDesktopLocal: true });
const remoteA = candidate({ environmentId: ENV_REMOTE_A, label: "AI-SERVER" });
const remoteB = candidate({ environmentId: ENV_REMOTE_B, label: "build-farm", phase: "error" });

describe("toEnvironmentRailCandidate", () => {
  const asCandidate = (target: ConnectionTarget, updateAvailable = false) =>
    toEnvironmentRailCandidate({
      environmentId: ENV_REMOTE_A,
      label: "x",
      target,
      phase: "connected",
      compat: null,
      updateAvailable,
    });

  it("classifies primary, desktop-local (local: prefix), and remote targets", () => {
    expect(asCandidate({ _tag: "PrimaryConnectionTarget" } as ConnectionTarget).isPrimary).toBe(
      true,
    );
    const local = asCandidate({
      _tag: "BearerConnectionTarget",
      connectionId: "local:wsl-ubuntu",
    } as ConnectionTarget);
    expect(local.isDesktopLocal).toBe(true);
    const remote = asCandidate({
      _tag: "BearerConnectionTarget",
      connectionId: "paired-1",
    } as ConnectionTarget);
    expect(remote.isPrimary).toBe(false);
    expect(remote.isDesktopLocal).toBe(false);
  });

  it("passes the updateAvailable flag through (Phase 7 wires the real input)", () => {
    const remoteTarget = {
      _tag: "BearerConnectionTarget",
      connectionId: "paired-1",
    } as ConnectionTarget;
    expect(asCandidate(remoteTarget, false).updateAvailable).toBe(false);
    expect(asCandidate(remoteTarget, true).updateAvailable).toBe(true);
  });
});

describe("resolveEnvironmentRailStatus", () => {
  it("maps phases and verdicts to the four §4.8 dot states", () => {
    const status = (input: Partial<Parameters<typeof resolveEnvironmentRailStatus>[0]>) =>
      resolveEnvironmentRailStatus({
        phase: "connected",
        compat: null,
        updateAvailable: false,
        ...input,
      });
    expect(status({})).toBe("connected");
    expect(status({ compat: { kind: "compatible" } })).toBe("connected");
    expect(status({ phase: "available" })).toBe("disconnected");
    expect(status({ phase: "offline" })).toBe("disconnected");
    expect(status({ phase: "reconnecting" })).toBe("disconnected");
    expect(status({ phase: "error" })).toBe("error");
    expect(status({ compat: { kind: "legacy" } })).toBe("attention");
    expect(status({ updateAvailable: true })).toBe("attention");
    expect(status({ compat: { kind: "server-too-old", serverVersion: 0, minSupported: 1 } })).toBe(
      "error",
    );
    expect(
      status({ compat: { kind: "client-too-old", serverMinCompatible: 2, clientVersion: 1 } }),
    ).toBe("error");
    // Disconnected environments with a stale bad verdict still read as disconnected.
    expect(status({ phase: "available", compat: { kind: "legacy" } })).toBe("disconnected");
  });
});

describe("environmentLetterAvatar", () => {
  it("derives one- and two-word initials", () => {
    expect(environmentLetterAvatar("AI-SERVER")).toBe("AS");
    expect(environmentLetterAvatar("build farm")).toBe("BF");
    expect(environmentLetterAvatar("staging")).toBe("ST");
    expect(environmentLetterAvatar("x")).toBe("X");
    expect(environmentLetterAvatar("  ")).toBe("?");
  });
});

describe("buildEnvironmentRailModel", () => {
  it("groups locals under one entry and sorts remotes by label", () => {
    const model = buildEnvironmentRailModel({
      candidates: [remoteB, primary, wsl, remoteA],
      activeEnvironmentId: ENV_PRIMARY,
    });
    expect(model.localSelected).toBe(true);
    expect(model.localTargetEnvironmentId).toBe(ENV_PRIMARY);
    // Sub-picker exists because a desktop-local backend exists (D6); primary first.
    expect(model.localSubEntries.map((entry) => entry.environmentId)).toEqual([
      ENV_PRIMARY,
      ENV_WSL,
    ]);
    expect(model.localSubEntries[0]?.label).toBe("This device");
    expect(model.remotes.map((entry) => entry.label)).toEqual(["AI-SERVER", "build-farm"]);
    expect(model.remotes[1]?.status).toBe("error");
  });

  it("has no sub-picker without desktop-local backends and treats null active as Local", () => {
    const model = buildEnvironmentRailModel({
      candidates: [primary, remoteA],
      activeEnvironmentId: null,
    });
    expect(model.localSubEntries).toEqual([]);
    expect(model.localSelected).toBe(true);
    expect(model.remotes[0]?.selected).toBe(false);
  });

  it("marks the active remote selected and Local unselected", () => {
    const model = buildEnvironmentRailModel({
      candidates: [primary, remoteA],
      activeEnvironmentId: ENV_REMOTE_A,
    });
    expect(model.localSelected).toBe(false);
    expect(model.remotes[0]?.selected).toBe(true);
  });
});

describe("selectRailVisibleEnvironmentIds", () => {
  const scope = [
    { environmentId: ENV_PRIMARY, isLocal: true },
    { environmentId: ENV_WSL, isLocal: true },
    { environmentId: ENV_REMOTE_A, isLocal: false },
  ];

  it("null selection means Local: filters to local/desktop-local environments only (amended §4.8)", () => {
    expect(
      selectRailVisibleEnvironmentIds({ candidates: scope, activeEnvironmentId: null }),
    ).toEqual(new Set([ENV_PRIMARY, ENV_WSL]));
  });

  it("an unresolvable (ghost) selection also scopes to Local, never to everything", () => {
    expect(
      selectRailVisibleEnvironmentIds({
        candidates: scope,
        activeEnvironmentId: ENV_REMOTE_B,
      }),
    ).toEqual(new Set([ENV_PRIMARY, ENV_WSL]));
  });

  it("degenerate catalog without any local environment applies no filter", () => {
    const remoteOnly = [{ environmentId: ENV_REMOTE_A, isLocal: false }];
    expect(
      selectRailVisibleEnvironmentIds({ candidates: remoteOnly, activeEnvironmentId: null }),
    ).toBeNull();
    // An explicitly selected remote still scopes to itself.
    expect(
      selectRailVisibleEnvironmentIds({
        candidates: remoteOnly,
        activeEnvironmentId: ENV_REMOTE_A,
      }),
    ).toEqual(new Set([ENV_REMOTE_A]));
  });

  it("shows the union of local environments when a local one is active (D6)", () => {
    const visible = selectRailVisibleEnvironmentIds({
      candidates: scope,
      activeEnvironmentId: ENV_WSL,
    });
    expect(visible).toEqual(new Set([ENV_PRIMARY, ENV_WSL]));
  });

  it("shows only the selected remote environment", () => {
    const visible = selectRailVisibleEnvironmentIds({
      candidates: scope,
      activeEnvironmentId: ENV_REMOTE_A,
    });
    expect(visible).toEqual(new Set([ENV_REMOTE_A]));
  });
});

describe("resolveAddProjectTargetLabel", () => {
  const labeled = [
    { environmentId: ENV_PRIMARY, isLocal: true, label: "Local" },
    { environmentId: ENV_REMOTE_A, isLocal: false, label: "AI-SERVER" },
  ];

  it("names the remote target and stays silent for Local/unknown", () => {
    expect(
      resolveAddProjectTargetLabel({ candidates: labeled, activeEnvironmentId: ENV_REMOTE_A }),
    ).toBe("AI-SERVER");
    expect(
      resolveAddProjectTargetLabel({ candidates: labeled, activeEnvironmentId: ENV_PRIMARY }),
    ).toBeNull();
    expect(
      resolveAddProjectTargetLabel({ candidates: labeled, activeEnvironmentId: null }),
    ).toBeNull();
  });
});
```

- [x] **Step 2: Run the test to verify it fails**

Run: `vp test run --project unit apps/web/src/components/sidebar/environmentRail.logic.test.ts`
Expected: FAIL — module `./environmentRail.logic` not found.

- [x] **Step 3: Write the implementation**

```ts
// apps/web/src/components/sidebar/environmentRail.logic.ts
import type {
  CompatVerdict,
  ConnectionTarget,
  EnvironmentConnectionPhase,
} from "@bibcode/client-runtime/connection";
import type { EnvironmentId } from "@bibcode/contracts";

import { isDesktopLocalConnectionTarget } from "../../connection/desktopLocal";
import { compareSidebarDisplayText } from "../../sidebarProjectGrouping";

/** §4.8 status dots: green / gray / amber / red. */
export type EnvironmentRailStatus = "connected" | "disconnected" | "attention" | "error";

export interface EnvironmentRailCandidate {
  readonly environmentId: EnvironmentId;
  readonly label: string;
  readonly isPrimary: boolean;
  readonly isDesktopLocal: boolean;
  readonly phase: EnvironmentConnectionPhase;
  readonly compat: CompatVerdict | null;
  readonly updateAvailable: boolean;
}

export function toEnvironmentRailCandidate(input: {
  readonly environmentId: EnvironmentId;
  readonly label: string;
  readonly target: ConnectionTarget;
  readonly phase: EnvironmentConnectionPhase;
  readonly compat: CompatVerdict | null;
  /** Amber-dot input. Phase 6 call sites pass `false`; Phase 7 wires the real
   * value from `useEnvironmentUpdateAvailability()` (see Phase interfaces). */
  readonly updateAvailable: boolean;
}): EnvironmentRailCandidate {
  return {
    environmentId: input.environmentId,
    label: input.label,
    isPrimary: input.target._tag === "PrimaryConnectionTarget",
    isDesktopLocal: isDesktopLocalConnectionTarget(input.target),
    phase: input.phase,
    compat: input.compat,
    updateAvailable: input.updateAvailable,
  };
}

/** Local = this machine: the primary environment or a host-managed `local:` backend (D6). */
export function isLocalRailCandidate(
  candidate: Pick<EnvironmentRailCandidate, "isPrimary" | "isDesktopLocal">,
): boolean {
  return candidate.isPrimary || candidate.isDesktopLocal;
}

export function resolveEnvironmentRailStatus(
  input: Pick<EnvironmentRailCandidate, "phase" | "compat" | "updateAvailable">,
): EnvironmentRailStatus {
  if (input.phase === "error") {
    return "error";
  }
  if (input.phase !== "connected") {
    return "disconnected";
  }
  if (
    input.compat !== null &&
    (input.compat.kind === "server-too-old" || input.compat.kind === "client-too-old")
  ) {
    return "error";
  }
  if (input.updateAvailable || input.compat?.kind === "legacy") {
    return "attention";
  }
  return "connected";
}

export function environmentLetterAvatar(label: string): string {
  const words = label
    .trim()
    .split(/[\s_-]+/)
    .filter((word) => word.length > 0);
  const first = words[0];
  if (first === undefined) {
    return "?";
  }
  const second = words[1];
  return second === undefined
    ? first.slice(0, 2).toUpperCase()
    : `${first[0] ?? ""}${second[0] ?? ""}`.toUpperCase();
}

export interface EnvironmentRailEntry {
  readonly environmentId: EnvironmentId;
  readonly label: string;
  readonly avatar: string;
  readonly status: EnvironmentRailStatus;
  readonly selected: boolean;
}

export interface EnvironmentRailModel {
  readonly localSelected: boolean;
  readonly localStatus: EnvironmentRailStatus;
  /** Non-empty only when desktop-local (WSL) backends exist (D6). Primary first. */
  readonly localSubEntries: ReadonlyArray<EnvironmentRailEntry>;
  /** Direct click target when there is no sub-picker: the primary environment. */
  readonly localTargetEnvironmentId: EnvironmentId | null;
  readonly remotes: ReadonlyArray<EnvironmentRailEntry>;
}

export function buildEnvironmentRailModel(input: {
  readonly candidates: ReadonlyArray<EnvironmentRailCandidate>;
  readonly activeEnvironmentId: EnvironmentId | null;
}): EnvironmentRailModel {
  const locals = input.candidates.filter(isLocalRailCandidate);
  const primary = locals.find((local) => local.isPrimary) ?? null;
  const desktopLocals = locals.filter((local) => local.isDesktopLocal);
  const localIds = new Set(locals.map((local) => local.environmentId));
  const localSelected =
    input.activeEnvironmentId === null || localIds.has(input.activeEnvironmentId);

  const toEntry = (candidate: EnvironmentRailCandidate): EnvironmentRailEntry => ({
    environmentId: candidate.environmentId,
    label: candidate.label,
    avatar: environmentLetterAvatar(candidate.label),
    status: resolveEnvironmentRailStatus(candidate),
    selected: candidate.environmentId === input.activeEnvironmentId,
  });

  return {
    localSelected,
    localStatus: primary === null ? "disconnected" : resolveEnvironmentRailStatus(primary),
    localSubEntries:
      desktopLocals.length === 0
        ? []
        : [
            ...(primary === null ? [] : [{ ...toEntry(primary), label: "This device" }]),
            ...desktopLocals.map(toEntry),
          ],
    localTargetEnvironmentId: primary?.environmentId ?? null,
    remotes: input.candidates
      .filter((candidate) => !isLocalRailCandidate(candidate))
      .map(toEntry)
      .sort((left, right) => compareSidebarDisplayText(left.label, right.label)),
  };
}

export interface RailEnvironmentScopeCandidate {
  readonly environmentId: EnvironmentId;
  readonly isLocal: boolean;
}

/**
 * Which environments the panel presents for the current rail selection (D3:
 * scoping is presentation-only). Amended §4.8: a null/absent selection means
 * **Local is selected and the panel filters to Local** — "no selection" must
 * never render as "show everything". An unresolvable (ghost) selection also
 * scopes to Local, matching the rail's imminent self-heal reset. `null` (no
 * filtering) is returned only for the degenerate catalog with no local
 * environment at all (hosted static bootstrap before its first-environment
 * effect selects a saved environment).
 */
export function selectRailVisibleEnvironmentIds(input: {
  readonly candidates: ReadonlyArray<RailEnvironmentScopeCandidate>;
  readonly activeEnvironmentId: EnvironmentId | null;
}): ReadonlySet<EnvironmentId> | null {
  const localIds = input.candidates
    .filter((candidate) => candidate.isLocal)
    .map((candidate) => candidate.environmentId);
  const localScope = (): ReadonlySet<EnvironmentId> | null =>
    localIds.length === 0 ? null : new Set(localIds);

  if (input.activeEnvironmentId === null) {
    return localScope();
  }
  const active = input.candidates.find(
    (candidate) => candidate.environmentId === input.activeEnvironmentId,
  );
  if (active === undefined || active.isLocal) {
    return localScope();
  }
  return new Set([input.activeEnvironmentId]);
}

/** §4.8: "Add project" copy becomes "Add project on <name>" for a remote selection. */
export function resolveAddProjectTargetLabel(input: {
  readonly candidates: ReadonlyArray<RailEnvironmentScopeCandidate & { readonly label: string }>;
  readonly activeEnvironmentId: EnvironmentId | null;
}): string | null {
  if (input.activeEnvironmentId === null) {
    return null;
  }
  const active = input.candidates.find(
    (candidate) => candidate.environmentId === input.activeEnvironmentId,
  );
  if (active === undefined || active.isLocal) {
    return null;
  }
  return active.label;
}
```

- [x] **Step 4: Run the test to verify it passes**

Run: `vp test run --project unit apps/web/src/components/sidebar/environmentRail.logic.test.ts`
Expected: PASS (all describes green).

- [x] **Step 5: Commit**

```bash
git add apps/web/src/components/sidebar/environmentRail.logic.ts apps/web/src/components/sidebar/environmentRail.logic.test.ts
git commit -m "feat(web): environment rail view-model"
```

---

### Task 3: `EnvironmentRail` component

**Files:**

- Create: `apps/web/src/components/sidebar/EnvironmentRail.tsx`
- Test: `apps/web/src/components/sidebar/EnvironmentRail.test.tsx`

**Interfaces:**

- Consumes: Task 1 (`resolveEnvironmentCompatVerdict`), Task 2 (model builders),
  `useEnvironments` (`apps/web/src/state/environments.ts`),
  `useActiveEnvironmentId`/`setActiveEnvironmentId` (`apps/web/src/state/entities.ts`),
  `Menu`/`MenuTrigger`/`MenuPopup`/`MenuItem` (`apps/web/src/components/ui/menu.tsx`),
  `Tooltip`/`TooltipTrigger`/`TooltipPopup` (`apps/web/src/components/ui/tooltip.tsx`),
  Phase 4's `/settings/remote-servers` route (deep link only).
- Produces: `EnvironmentRail` (mounted by Task 4). Test ids used by later tasks/tests:
  `environment-rail`, `environment-rail-local`, `environment-rail-entry-<environmentId>`,
  `environment-rail-add-server`, `environment-rail-manage`. Phase 7 wiring point: the
  single `toEnvironmentRailCandidate` call in this component passes
  `updateAvailable: false`; Phase 7 replaces it with
  `useEnvironmentUpdateAvailability().get(environment.environmentId) ?? false`
  (hook name pinned in "Phase interfaces / Produces").

**Behavior pinned by tests (D3):** clicking an entry calls `setActiveEnvironmentId` and
performs no navigation and no catalog/registry command; a vanished active id resets to Local.

- [x] **Step 1: Write the failing test**

The web app's component tests run SSR (`renderToStaticMarkup`) with capture-mocks (see the
header comments of `apps/web/src/components/Sidebar.test.tsx` and
`apps/web/src/components/ui/sidebar.rail.test.tsx`). Mock the state hooks, tooltip, and menu
so click handlers can be invoked directly off captured props.

```tsx
// apps/web/src/components/sidebar/EnvironmentRail.test.tsx
import { renderToStaticMarkup } from "react-dom/server";
import type { ReactElement, ReactNode } from "react";
import { describe, expect, it, vi } from "vite-plus/test";

import { EnvironmentId } from "@bibcode/contracts";

const h = vi.hoisted(() => ({
  environments: [] as Array<unknown>,
  isReady: true,
  activeEnvironmentId: null as string | null,
  setActiveEnvironmentId: vi.fn(),
  navigate: vi.fn(),
  catalogCommandCalls: [] as Array<string>,
  buttons: [] as Array<Record<string, unknown>>,
  menuItems: [] as Array<Record<string, unknown>>,
  reset() {
    h.environments = [];
    h.isReady = true;
    h.activeEnvironmentId = null;
    h.setActiveEnvironmentId.mockReset();
    h.navigate.mockReset();
    h.catalogCommandCalls = [];
    h.buttons = [];
    h.menuItems = [];
  },
}));

vi.mock("../../state/environments", () => ({
  useEnvironments: () => ({ environments: h.environments, isReady: h.isReady }),
}));
vi.mock("../../state/entities", () => ({
  useActiveEnvironmentId: () => h.activeEnvironmentId,
  setActiveEnvironmentId: h.setActiveEnvironmentId,
}));
vi.mock("../../connection/environmentCompat", () => ({
  resolveEnvironmentCompatVerdict: () => null,
}));
// D3 sentinel: the rail must never import/execute catalog commands. Registering
// the mock proves it: any call would be recorded (and the assertion below pins zero).
vi.mock("../../connection/catalog", () => ({
  environmentCatalog: new Proxy(
    {},
    {
      get: (_target, key) => {
        h.catalogCommandCalls.push(String(key));
        return {};
      },
    },
  ),
}));
vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => h.navigate,
}));
vi.mock("../ui/tooltip", () => ({
  Tooltip: ({ children }: { children?: ReactNode }) => <>{children}</>,
  TooltipPopup: () => null,
  TooltipTrigger: ({ render, children }: { render?: ReactElement; children?: ReactNode }) => {
    if (render) {
      h.buttons.push({ ...(render.props as Record<string, unknown>) });
    }
    return <>{children}</>;
  },
}));
vi.mock("../ui/menu", () => ({
  Menu: ({ children }: { children?: ReactNode }) => <>{children}</>,
  MenuPopup: ({ children }: { children?: ReactNode }) => <>{children}</>,
  MenuTrigger: ({ render, children }: { render?: ReactElement; children?: ReactNode }) => {
    if (render) {
      h.buttons.push({ ...(render.props as Record<string, unknown>) });
    }
    return <>{children}</>;
  },
  MenuItem: (props: Record<string, unknown>) => {
    h.menuItems.push(props);
    return null;
  },
}));

// React effects do not run under renderToStaticMarkup; capture them manually so
// the stale-selection reset can be asserted.
const effects: Array<() => void | (() => void)> = [];
vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  return {
    ...actual,
    useEffect: (effect: () => void | (() => void)) => {
      effects.push(effect);
    },
  };
});

import { EnvironmentRail } from "./EnvironmentRail";

const ENV_PRIMARY = EnvironmentId.make("env-primary");
const ENV_REMOTE = EnvironmentId.make("env-remote");
const ENV_WSL = EnvironmentId.make("env-wsl");

function environment(input: {
  environmentId: string;
  label: string;
  target: Record<string, unknown>;
  phase?: string;
}) {
  return {
    environmentId: input.environmentId,
    label: input.label,
    entry: { target: input.target },
    connection: { phase: input.phase ?? "connected", error: null, traceId: null },
    serverConfig: null,
  };
}

function renderRail() {
  effects.length = 0;
  h.buttons = [];
  h.menuItems = [];
  const markup = renderToStaticMarkup(<EnvironmentRail />);
  return markup;
}

function buttonByTestId(testId: string) {
  return h.buttons.find((props) => props["data-testid"] === testId);
}

describe("EnvironmentRail", () => {
  it("renders Local plus sorted remote entries with radio semantics", () => {
    h.reset();
    h.environments = [
      environment({
        environmentId: ENV_PRIMARY,
        label: "Local",
        target: { _tag: "PrimaryConnectionTarget" },
      }),
      environment({
        environmentId: ENV_REMOTE,
        label: "AI-SERVER",
        target: { _tag: "BearerConnectionTarget", connectionId: "paired-1" },
      }),
    ];
    h.activeEnvironmentId = ENV_PRIMARY;
    const markup = renderRail();
    expect(markup).toContain('role="radiogroup"');
    const local = buttonByTestId("environment-rail-local");
    expect(local?.["aria-checked"]).toBe(true);
    expect(local?.tabIndex).toBe(0);
    const remote = buttonByTestId(`environment-rail-entry-${ENV_REMOTE}`);
    expect(remote?.["aria-checked"]).toBe(false);
    expect(remote?.tabIndex).toBe(-1);
    expect(buttonByTestId("environment-rail-add-server")).toBeDefined();
    expect(buttonByTestId("environment-rail-manage")).toBeDefined();
  });

  it("still renders with zero saved remotes (discoverability surface)", () => {
    h.reset();
    h.environments = [
      environment({
        environmentId: ENV_PRIMARY,
        label: "Local",
        target: { _tag: "PrimaryConnectionTarget" },
      }),
    ];
    h.activeEnvironmentId = ENV_PRIMARY;
    const markup = renderRail();
    expect(markup).toContain('data-testid="environment-rail"');
    expect(buttonByTestId("environment-rail-local")).toBeDefined();
    expect(buttonByTestId("environment-rail-add-server")).toBeDefined();
    // No divider between Local and an empty remote list.
    expect(markup).not.toContain('data-testid="environment-rail-divider"');
  });

  it("selection writes the active-environment atom and nothing else (D3)", () => {
    h.reset();
    h.environments = [
      environment({
        environmentId: ENV_PRIMARY,
        label: "Local",
        target: { _tag: "PrimaryConnectionTarget" },
      }),
      environment({
        environmentId: ENV_REMOTE,
        label: "AI-SERVER",
        target: { _tag: "BearerConnectionTarget", connectionId: "paired-1" },
      }),
    ];
    h.activeEnvironmentId = ENV_PRIMARY;
    renderRail();
    const remote = buttonByTestId(`environment-rail-entry-${ENV_REMOTE}`);
    (remote?.onClick as () => void)();
    expect(h.setActiveEnvironmentId).toHaveBeenCalledExactlyOnceWith(ENV_REMOTE);
    expect(h.navigate).not.toHaveBeenCalled();
    // Soft switch: no supervisor/catalog desired-state change on selection.
    expect(h.catalogCommandCalls).toEqual([]);
  });

  it("groups WSL backends under a Local sub-picker keyed by the local: prefix (D6)", () => {
    h.reset();
    h.environments = [
      environment({
        environmentId: ENV_PRIMARY,
        label: "Local",
        target: { _tag: "PrimaryConnectionTarget" },
      }),
      environment({
        environmentId: ENV_WSL,
        label: "Ubuntu",
        target: { _tag: "BearerConnectionTarget", connectionId: "local:wsl-ubuntu" },
      }),
    ];
    h.activeEnvironmentId = ENV_PRIMARY;
    renderRail();
    expect(h.menuItems.map((item) => item.children)).toEqual(["This device", "Ubuntu"]);
    (h.menuItems[1]?.onClick as () => void)();
    expect(h.setActiveEnvironmentId).toHaveBeenCalledExactlyOnceWith(ENV_WSL);
  });

  it("deep-links the bottom actions to the Remote Servers settings section", () => {
    h.reset();
    h.environments = [
      environment({
        environmentId: ENV_PRIMARY,
        label: "Local",
        target: { _tag: "PrimaryConnectionTarget" },
      }),
    ];
    renderRail();
    (buttonByTestId("environment-rail-add-server")?.onClick as () => void)();
    expect(h.navigate).toHaveBeenCalledWith({
      to: "/settings/remote-servers",
      search: { action: "add-server" },
    });
    (buttonByTestId("environment-rail-manage")?.onClick as () => void)();
    expect(h.navigate).toHaveBeenCalledWith({ to: "/settings/remote-servers" });
    expect(h.setActiveEnvironmentId).not.toHaveBeenCalled();
  });

  it("resets a stale selection back to Local once the catalog is ready", () => {
    h.reset();
    h.environments = [
      environment({
        environmentId: ENV_PRIMARY,
        label: "Local",
        target: { _tag: "PrimaryConnectionTarget" },
      }),
    ];
    h.activeEnvironmentId = ENV_REMOTE; // removed environment
    renderRail();
    for (const effect of effects) effect();
    expect(h.setActiveEnvironmentId).toHaveBeenCalledExactlyOnceWith(ENV_PRIMARY);
  });

  it("does not reset while the catalog is still loading", () => {
    h.reset();
    h.isReady = false;
    h.environments = [];
    h.activeEnvironmentId = ENV_REMOTE;
    renderRail();
    for (const effect of effects) effect();
    expect(h.setActiveEnvironmentId).not.toHaveBeenCalled();
  });
});
```

- [x] **Step 2: Run the test to verify it fails**

Run: `vp test run --project unit apps/web/src/components/sidebar/EnvironmentRail.test.tsx`
Expected: FAIL — module `./EnvironmentRail` not found.

- [x] **Step 3: Write the implementation**

```tsx
// apps/web/src/components/sidebar/EnvironmentRail.tsx
import { useNavigate } from "@tanstack/react-router";
import { MonitorIcon, PlusIcon, Settings2Icon } from "lucide-react";
import * as React from "react";
import { useCallback, useEffect, useMemo, useRef } from "react";

import type { EnvironmentId } from "@bibcode/contracts";

import { resolveEnvironmentCompatVerdict } from "../../connection/environmentCompat";
import { cn } from "../../lib/utils";
import { setActiveEnvironmentId, useActiveEnvironmentId } from "../../state/entities";
import { useEnvironments } from "../../state/environments";
import { Menu, MenuItem, MenuPopup, MenuTrigger } from "../ui/menu";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";
import {
  buildEnvironmentRailModel,
  toEnvironmentRailCandidate,
  type EnvironmentRailEntry,
  type EnvironmentRailStatus,
} from "./environmentRail.logic";

const STATUS_DOT_CLASS: Record<EnvironmentRailStatus, string> = {
  connected: "bg-success",
  disconnected: "bg-muted-foreground/50",
  attention: "bg-warning",
  error: "bg-destructive",
};

const RAIL_BUTTON_CLASS =
  "relative flex size-9 items-center justify-center rounded-[10px] text-muted-foreground transition-colors hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring outline-hidden";

const RAIL_BUTTON_SELECTED_CLASS =
  "bg-accent text-foreground before:absolute before:-left-2 before:top-2 before:bottom-2 before:w-[3px] before:rounded-full before:bg-primary";

function StatusDot({ status }: { readonly status: EnvironmentRailStatus }) {
  return (
    <span
      data-status={status}
      className={cn(
        "absolute right-0.5 bottom-0.5 size-2 rounded-full border-2 border-sidebar",
        STATUS_DOT_CLASS[status],
      )}
    />
  );
}

function RemoteEntryButton({
  entry,
  onSelect,
}: {
  readonly entry: EnvironmentRailEntry;
  readonly onSelect: (environmentId: EnvironmentId) => void;
}) {
  return (
    <Tooltip>
      <TooltipTrigger
        render={
          <button
            type="button"
            role="radio"
            aria-checked={entry.selected}
            tabIndex={entry.selected ? 0 : -1}
            aria-label={entry.label}
            data-testid={`environment-rail-entry-${entry.environmentId}`}
            className={cn(RAIL_BUTTON_CLASS, entry.selected && RAIL_BUTTON_SELECTED_CLASS)}
            onClick={() => onSelect(entry.environmentId)}
          />
        }
      >
        <span
          className={cn(
            "flex size-[26px] items-center justify-center rounded-lg text-[10px] font-semibold tracking-wide",
            entry.selected ? "bg-primary text-primary-foreground" : "bg-muted",
          )}
        >
          {entry.avatar}
        </span>
        <StatusDot status={entry.status} />
      </TooltipTrigger>
      <TooltipPopup side="right">{entry.label}</TooltipPopup>
    </Tooltip>
  );
}

/**
 * 52px environment rail (spec §4.8, mockup Variant B). Selection is
 * presentation-only (D3): it writes `activeEnvironmentIdAtom` and never
 * touches the connection registry, supervisors, or the router. Entity
 * operations keep routing by each entity's own environmentId (D4).
 */
export function EnvironmentRail() {
  const { environments, isReady } = useEnvironments();
  const activeEnvironmentId = useActiveEnvironmentId();
  const navigate = useNavigate();

  const candidates = useMemo(
    () =>
      environments.map((environment) =>
        toEnvironmentRailCandidate({
          environmentId: environment.environmentId,
          label: environment.label,
          target: environment.entry.target,
          phase: environment.connection.phase,
          compat: resolveEnvironmentCompatVerdict(environment.serverConfig),
          // Phase 7 wiring point (amended §4.8): replace this constant with
          // `useEnvironmentUpdateAvailability().get(environment.environmentId) ?? false`
          // — see the "Phase interfaces / Produces" pin in this plan.
          updateAvailable: false,
        }),
      ),
    [environments],
  );
  const model = useMemo(
    () => buildEnvironmentRailModel({ candidates, activeEnvironmentId }),
    [candidates, activeEnvironmentId],
  );

  const selectEnvironment = useCallback((environmentId: EnvironmentId) => {
    setActiveEnvironmentId(environmentId);
  }, []);

  // Self-heal a stale selection: if the active environment was removed from
  // the catalog, fall back to Local instead of filtering the panel forever.
  useEffect(() => {
    if (!isReady || activeEnvironmentId === null) {
      return;
    }
    if (candidates.some((candidate) => candidate.environmentId === activeEnvironmentId)) {
      return;
    }
    setActiveEnvironmentId(model.localTargetEnvironmentId);
  }, [activeEnvironmentId, candidates, isReady, model.localTargetEnvironmentId]);

  // Roving focus for the radiogroup (WAI-ARIA radio pattern).
  const handleRadioKeyDown = useCallback((event: React.KeyboardEvent<HTMLDivElement>) => {
    if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) {
      return;
    }
    const radios = Array.from(
      event.currentTarget.querySelectorAll<HTMLButtonElement>('[role="radio"]'),
    );
    if (radios.length === 0) {
      return;
    }
    const currentIndex = radios.findIndex((radio) => radio === document.activeElement);
    const nextIndex =
      event.key === "Home"
        ? 0
        : event.key === "End"
          ? radios.length - 1
          : event.key === "ArrowDown"
            ? (currentIndex + 1 + radios.length) % radios.length
            : (currentIndex - 1 + radios.length) % radios.length;
    event.preventDefault();
    radios[nextIndex]?.focus();
  }, []);

  const localButtonProps = {
    type: "button" as const,
    role: "radio" as const,
    "aria-checked": model.localSelected,
    tabIndex: model.localSelected ? 0 : -1,
    "aria-label": "Local — this machine",
    "data-testid": "environment-rail-local",
    className: cn(RAIL_BUTTON_CLASS, model.localSelected && RAIL_BUTTON_SELECTED_CLASS),
  };

  return (
    <div
      data-testid="environment-rail"
      className="flex h-full w-[52px] shrink-0 flex-col items-center gap-2 border-r border-border bg-sidebar py-2"
    >
      <div
        role="radiogroup"
        aria-label="Environments"
        className="flex flex-col items-center gap-2"
        onKeyDown={handleRadioKeyDown}
      >
        {model.localSubEntries.length > 0 ? (
          <Menu>
            <MenuTrigger render={<button {...localButtonProps} />}>
              <MonitorIcon className="size-[18px]" />
              <StatusDot status={model.localStatus} />
            </MenuTrigger>
            <MenuPopup side="right" align="start">
              {model.localSubEntries.map((entry) => (
                <MenuItem
                  key={entry.environmentId}
                  onClick={() => selectEnvironment(entry.environmentId)}
                >
                  {entry.label}
                </MenuItem>
              ))}
            </MenuPopup>
          </Menu>
        ) : (
          <Tooltip>
            <TooltipTrigger
              render={
                <button
                  {...localButtonProps}
                  onClick={() => {
                    if (model.localTargetEnvironmentId !== null) {
                      selectEnvironment(model.localTargetEnvironmentId);
                    }
                  }}
                />
              }
            >
              <MonitorIcon className="size-[18px]" />
              <StatusDot status={model.localStatus} />
            </TooltipTrigger>
            <TooltipPopup side="right">Local — this machine</TooltipPopup>
          </Tooltip>
        )}
        {model.remotes.length > 0 ? (
          <div
            data-testid="environment-rail-divider"
            role="presentation"
            className="h-px w-6 bg-border"
          />
        ) : null}
        {model.remotes.map((entry) => (
          <RemoteEntryButton key={entry.environmentId} entry={entry} onSelect={selectEnvironment} />
        ))}
      </div>
      <div className="flex-1" />
      <Tooltip>
        <TooltipTrigger
          render={
            <button
              type="button"
              aria-label="Add server…"
              data-testid="environment-rail-add-server"
              className={RAIL_BUTTON_CLASS}
              onClick={() =>
                void navigate({
                  to: "/settings/remote-servers",
                  search: { action: "add-server" },
                })
              }
            />
          }
        >
          <PlusIcon className="size-[18px]" />
        </TooltipTrigger>
        <TooltipPopup side="right">Add server…</TooltipPopup>
      </Tooltip>
      <Tooltip>
        <TooltipTrigger
          render={
            <button
              type="button"
              aria-label="Manage remote servers…"
              data-testid="environment-rail-manage"
              className={RAIL_BUTTON_CLASS}
              onClick={() => void navigate({ to: "/settings/remote-servers" })}
            />
          }
        >
          <Settings2Icon className="size-[18px]" />
        </TooltipTrigger>
        <TooltipPopup side="right">Manage remote servers…</TooltipPopup>
      </Tooltip>
    </div>
  );
}
```

Type note: if the typed router rejects the `search` object because Phase 4's route declares
no search schema, Task 9 adds `validateSearch` to the route — implement Task 9's route change
first in that case, or temporarily cast `search: { action: "add-server" } as never` and
remove the cast in Task 9 (leave a `// Task 9 removes this cast` comment only if the interim
state must compile).

- [x] **Step 4: Run the test to verify it passes**

Run: `vp test run --project unit apps/web/src/components/sidebar/EnvironmentRail.test.tsx`
Expected: PASS (7 tests).

- [x] **Step 5: Commit**

```bash
git add apps/web/src/components/sidebar/EnvironmentRail.tsx apps/web/src/components/sidebar/EnvironmentRail.test.tsx
git commit -m "feat(web): environment rail component"
```

---

### Task 4: Mount the rail in `AppSidebarLayout`

**Files:**

- Modify: `apps/web/src/components/AppSidebarLayout.tsx` (whole file is 97 lines; the layout
  return is at the bottom)
- Test: `apps/web/src/components/AppSidebarLayout.test.tsx` (extend)

**Interfaces:**

- Consumes: `EnvironmentRail` (Task 3).
- Produces: the rail is visible in every left-panel state (desktop fixed sidebar, mobile
  sheet); `THREAD_SIDEBAR_MIN_WIDTH` accounts for the rail column.

Mounting rationale: see Design decision 2 (the desktop sidebar is `position: fixed`, so the
rail must live inside `<Sidebar>`; it collapses with the offcanvas panel, which is the
intended toggle behavior for the whole left panel).

- [x] **Step 1: Write the failing test**

Open `apps/web/src/components/AppSidebarLayout.test.tsx`, follow its existing mock setup
(add a mock for the rail module alongside the existing `./Sidebar` mock), and add:

```tsx
vi.mock("./sidebar/EnvironmentRail", () => ({
  EnvironmentRail: () => <div data-testid="environment-rail-mock" />,
}));
```

```tsx
it("mounts the environment rail inside the left sidebar, before the panel content", () => {
  const markup = renderLayout(); // reuse the file's existing render helper
  const railIndex = markup.indexOf("environment-rail-mock");
  const panelIndex = markup.indexOf("thread-sidebar-mock"); // the file's existing Sidebar mock marker
  expect(railIndex).toBeGreaterThan(-1);
  expect(panelIndex).toBeGreaterThan(-1);
  expect(railIndex).toBeLessThan(panelIndex);
});
```

If the existing test file mocks `./Sidebar` with a different marker, use that marker's text
in place of `thread-sidebar-mock`; if it has no render helper, render
`<AppSidebarLayout>content</AppSidebarLayout>` with `renderToStaticMarkup` under the file's
existing mocks.

- [x] **Step 2: Run the test to verify it fails**

Run: `vp test run --project unit apps/web/src/components/AppSidebarLayout.test.tsx`
Expected: FAIL — `environment-rail-mock` not found in markup.

- [x] **Step 3: Implement the mount**

In `apps/web/src/components/AppSidebarLayout.tsx`:

1. Add the import:

```tsx
import { EnvironmentRail } from "./sidebar/EnvironmentRail";
```

2. Bump the minimum width so the projects panel keeps its current minimum beside the 52px
   rail (change the existing constant):

```tsx
const ENVIRONMENT_RAIL_WIDTH = 52;
const THREAD_SIDEBAR_MIN_WIDTH = 13 * 16 + ENVIRONMENT_RAIL_WIDTH;
```

3. Replace the `<Sidebar>…</Sidebar>` children (currently `<ThreadSidebar />` followed by
   `<SidebarRail />`) with:

```tsx
<Sidebar
  side="left"
  collapsible="offcanvas"
  className="border-r border-border bg-card text-foreground"
  resizable={{
    minWidth: THREAD_SIDEBAR_MIN_WIDTH,
    shouldAcceptWidth: ({ nextWidth, wrapper }) =>
      wrapper.clientWidth - nextWidth >= THREAD_MAIN_CONTENT_MIN_WIDTH,
    storageKey: THREAD_SIDEBAR_WIDTH_STORAGE_KEY,
  }}
>
  <div className="flex h-full min-h-0 flex-row">
    <EnvironmentRail />
    <div className="flex h-full min-h-0 min-w-0 flex-1 flex-col">
      <ThreadSidebar />
    </div>
  </div>
  <SidebarRail />
</Sidebar>
```

(`SidebarRail` here is the pre-existing resize handle from `./ui/sidebar` — unchanged.)

- [x] **Step 4: Run the tests to verify they pass**

Run: `vp test run --project unit apps/web/src/components/AppSidebarLayout.test.tsx`
Expected: PASS, including the pre-existing cases.

- [x] **Step 5: Commit**

```bash
git add apps/web/src/components/AppSidebarLayout.tsx apps/web/src/components/AppSidebarLayout.test.tsx
git commit -m "feat(web): mount environment rail in the left panel"
```

---

### Task 5: Panel scoping by rail selection (D3/D4 pins)

**Files:**

- Modify: `apps/web/src/components/Sidebar.tsx` — main `Sidebar()` component (declared
  `export default function Sidebar()` at ~line 3961; data sources `useProjects()` /
  `useThreadShells()` at ~lines 3962–3966)
- Test: `apps/web/src/components/Sidebar.test.tsx` (extend harness + cases)

**Interfaces:**

- Consumes: `selectRailVisibleEnvironmentIds` (Task 2), `useActiveEnvironmentId` (existing),
  `isDesktopLocalConnectionTarget` (already imported in `Sidebar.tsx`).
- Produces: the panel presents only the selected environment's projects/threads (Local =
  union of local environments). Everything downstream (`sidebarProjects`, grouping,
  `sortedProjects`, `threadsByProjectKey`) consumes the filtered arrays unchanged.

- [x] **Step 1: Extend the test harness and write the failing tests**

In `apps/web/src/components/Sidebar.test.tsx`:

1. Add to the hoisted `h.state` object: `activeEnvironmentId: null as string | null`, and
   reset it to `null` in the `beforeEach` state-reset block.
2. Extend the `vi.mock("../state/entities", …)` factory with:

```ts
  useActiveEnvironmentId: () => h.state.activeEnvironmentId,
  setActiveEnvironmentId: (environmentId: unknown) => {
    h.state.activeEnvironmentId = environmentId as string | null;
  },
```

3. The file's `environmentFixture` helper already builds `entry.target` with
   `PrimaryConnectionTarget`/`BearerConnectionTarget` and a `connectionId` — reuse it.
4. Add a describe block (reuse the file's existing full-`Sidebar` render pattern — the same
   one the project-availability tests use — and its `makeProject`/`makeThread` factories):

```tsx
staticDescribe("Sidebar environment scoping (rail selection)", () => {
  const ENV_REMOTE = EnvironmentId.make("env-remote");

  function seedTwoEnvironments() {
    h.state.environments = [
      environmentFixture({ environmentId: ENV_MAIN, label: "Local", primary: true }),
      environmentFixture({
        environmentId: ENV_REMOTE,
        label: "AI-SERVER",
        connectionId: "paired-1",
      }),
    ];
    h.state.primaryEnvironmentId = ENV_MAIN;
    h.state.projects = [
      makeProject("project-a"),
      makeProject("project-b", {
        environmentId: ENV_REMOTE,
        title: "Remote Repo",
        workspaceRoot: "/srv/remote-repo",
      }),
    ];
    h.state.threads = [
      makeThread("thread-local"),
      makeThread("thread-remote", {
        environmentId: ENV_REMOTE,
        projectId: ProjectId.make("project-b"),
      }),
    ];
  }

  it("filters to local environments while no selection is set (amended §4.8: null = Local)", () => {
    seedTwoEnvironments();
    h.state.activeEnvironmentId = null;
    const markup = renderSidebar(); // the file's existing helper for rendering <Sidebar />
    expect(markup).toContain("Repo A");
    expect(markup).not.toContain("Remote Repo");
  });

  it("filters projects and threads to the selected remote environment (D3 presentation-only)", () => {
    seedTwoEnvironments();
    h.state.activeEnvironmentId = ENV_REMOTE;
    h.state.commandCalls = [];
    const markup = renderSidebar();
    expect(markup).toContain("Remote Repo");
    expect(markup).not.toContain("Repo A");
    // Soft switch: rendering a filtered panel dispatched no environment
    // lifecycle command (no retry/register/remove/adopt — nothing).
    expect(
      h.state.commandCalls.filter((call: { label?: string }) =>
        String(call.label ?? "").startsWith("environment."),
      ),
    ).toEqual([]);
  });

  it("keeps local environments merged when a local environment is selected (D6)", () => {
    seedTwoEnvironments();
    h.state.environments.push(
      environmentFixture({
        environmentId: EnvironmentId.make("env-wsl"),
        label: "Ubuntu",
        connectionId: "local:wsl-ubuntu",
      }),
    );
    h.state.projects.push(
      makeProject("project-wsl", {
        environmentId: EnvironmentId.make("env-wsl"),
        title: "WSL Repo",
        workspaceRoot: "/home/user/wsl-repo",
      }),
    );
    h.state.activeEnvironmentId = ENV_MAIN;
    const markup = renderSidebar();
    expect(markup).toContain("Repo A");
    expect(markup).toContain("WSL Repo");
    expect(markup).not.toContain("Remote Repo");
  });
});
```

Note: if the existing file names its render helper differently (search it for
`renderToStaticMarkup(<Sidebar`), use that helper/name; `h.state.commandCalls` is the file's
existing atom-command capture (`h.runCommand` pushes into it).

- [x] **Step 2: Run the tests to verify they fail**

Run: `vp test run --project unit apps/web/src/components/Sidebar.test.tsx`
Expected: all three new cases FAIL — without the scoping code both environments always
render, including in the null-selection case.

- [x] **Step 3: Implement the scoping**

In `apps/web/src/components/Sidebar.tsx`, main `Sidebar()` component:

1. Add imports (top of file, alongside the existing `../state/entities` import group):

```ts
import { useActiveEnvironmentId } from "../state/entities";
import { selectRailVisibleEnvironmentIds } from "./sidebar/environmentRail.logic";
```

(`useProjects`, `useThreadShells`, `isDesktopLocalConnectionTarget` are already imported.)

2. Replace the two data-source lines at the top of `Sidebar()`:

```ts
const projects = useProjects();
```

and

```ts
const sidebarThreads = useThreadShells();
```

with:

```ts
const allProjects = useProjects();
const { environments } = useEnvironments();
const activeEnvironmentId = useActiveEnvironmentId();
// D3: rail selection scopes presentation only. Filtering happens here at the
// panel's data edge; connections, supervisors, and per-entity RPC routing
// (D4) are untouched by selection.
const visibleEnvironmentIds = useMemo(
  () =>
    selectRailVisibleEnvironmentIds({
      activeEnvironmentId,
      candidates: environments.map((environment) => ({
        environmentId: environment.environmentId,
        isLocal:
          environment.entry.target._tag === "PrimaryConnectionTarget" ||
          isDesktopLocalConnectionTarget(environment.entry.target),
      })),
    }),
  [activeEnvironmentId, environments],
);
const projects = useMemo(
  () =>
    visibleEnvironmentIds === null
      ? allProjects
      : allProjects.filter((project) => visibleEnvironmentIds.has(project.environmentId)),
  [allProjects, visibleEnvironmentIds],
);
```

(The component already calls `const { environments } = useEnvironments();` — keep the single
existing call and place the new memos after it instead of adding a duplicate.)

```ts
const allSidebarThreads = useThreadShells();
const sidebarThreads = useMemo(
  () =>
    visibleEnvironmentIds === null
      ? allSidebarThreads
      : allSidebarThreads.filter((thread) => visibleEnvironmentIds.has(thread.environmentId)),
  [allSidebarThreads, visibleEnvironmentIds],
);
```

Everything downstream keeps its current names (`projects`, `sidebarThreads`), so grouping,
sorting, and rendering are untouched.

- [x] **Step 4: Run the tests to verify they pass**

Run: `vp test run --project unit apps/web/src/components/Sidebar.test.tsx`
Expected: PASS — the new describe and all pre-existing Sidebar cases.

- [x] **Step 5: Commit**

```bash
git add apps/web/src/components/Sidebar.tsx apps/web/src/components/Sidebar.test.tsx
git commit -m "feat(web): scope the projects panel to the selected environment"
```

---

### Task 6: `EnvironmentContextCard`

**Files:**

- Create: `apps/web/src/components/sidebar/environmentContextCard.logic.ts`
- Create: `apps/web/src/components/sidebar/EnvironmentContextCard.tsx`
- Test: `apps/web/src/components/sidebar/environmentContextCard.logic.test.ts`
- Test: `apps/web/src/components/sidebar/EnvironmentContextCard.test.tsx`
- Modify: `apps/web/src/components/Sidebar.tsx` (mount under the brand row, ~line 4657)
- Modify: `apps/web/src/components/Sidebar.test.tsx` (capture-mock for the card)

**Interfaces:**

- Consumes: Tasks 1–2; `connectionStatusText` (`@bibcode/client-runtime/connection`);
  `useEnvironment`/`useActiveEnvironmentId`; **Phase 4's** `environmentCatalog.disconnect`
  command atom (see Phase interfaces — if absent, stop and report).
- Produces (Phase 7 contract): `EnvironmentContextCardProps` with optional
  `updateBadge?: React.ReactNode` and `onCheckForUpdates?: (environmentId: EnvironmentId) => void`.

- [x] **Step 1: Write the failing logic test**

```ts
// apps/web/src/components/sidebar/environmentContextCard.logic.test.ts
import type { ConnectionTarget } from "@bibcode/client-runtime/connection";
import type { ServerConfig } from "@bibcode/contracts";
import { describe, expect, it, vi } from "vite-plus/test";

const compat = vi.hoisted(() => ({ verdict: null as unknown }));
vi.mock("../../connection/environmentCompat", () => ({
  resolveEnvironmentCompatVerdict: () => compat.verdict,
  selectRemoteUpdateControlCapability: (serverConfig: unknown) => serverConfig !== null,
}));

import {
  buildEnvironmentContextCardView,
  resolveCompatBadge,
} from "./environmentContextCard.logic";

const remoteTarget = {
  _tag: "BearerConnectionTarget",
  connectionId: "paired-1",
} as ConnectionTarget;

function view(overrides: Partial<Parameters<typeof buildEnvironmentContextCardView>[0]> = {}) {
  return buildEnvironmentContextCardView({
    label: "AI-SERVER",
    target: remoteTarget,
    connection: { phase: "connected", error: null, traceId: null },
    serverConfig: {
      environment: { serverVersion: "0.4.2", capabilities: {} },
    } as unknown as ServerConfig,
    ...overrides,
  });
}

describe("buildEnvironmentContextCardView", () => {
  it("is hidden for Local: primary and desktop-local targets (D6)", () => {
    expect(view({ target: { _tag: "PrimaryConnectionTarget" } as ConnectionTarget })).toBeNull();
    expect(
      view({
        target: {
          _tag: "BearerConnectionTarget",
          connectionId: "local:wsl-ubuntu",
        } as ConnectionTarget,
      }),
    ).toBeNull();
  });

  it("renders name, status text, and the BiBCode version line for a remote", () => {
    compat.verdict = { kind: "compatible" };
    const card = view();
    expect(card?.name).toBe("AI-SERVER");
    expect(card?.statusText).toBe("Connected");
    expect(card?.versionLine).toBe("BiBCode v0.4.2");
    expect(card?.compatBadge).toBeNull();
    expect(card?.showUpdateActions).toBe(true);
  });

  it("degrades without a delivered server config: no version line, no update actions", () => {
    compat.verdict = null;
    const card = view({
      serverConfig: null,
      connection: { phase: "reconnecting", error: "boom", traceId: null },
    });
    expect(card?.versionLine).toBeNull();
    expect(card?.compatBadge).toBeNull();
    expect(card?.showUpdateActions).toBe(false);
    expect(card?.statusText).toContain("Reconnecting");
  });
});

describe("resolveCompatBadge", () => {
  it("maps verdicts to badge copy", () => {
    expect(resolveCompatBadge(null)).toBeNull();
    expect(resolveCompatBadge({ kind: "compatible" })).toBeNull();
    expect(resolveCompatBadge({ kind: "legacy" })).toEqual({
      label: "Limited compatibility",
      tone: "warning",
    });
    expect(
      resolveCompatBadge({ kind: "server-too-old", serverVersion: 0, minSupported: 1 }),
    ).toEqual({ label: "Server update required", tone: "error" });
    expect(
      resolveCompatBadge({ kind: "client-too-old", serverMinCompatible: 2, clientVersion: 1 }),
    ).toEqual({ label: "App update required", tone: "error" });
  });
});
```

- [x] **Step 2: Run it to verify it fails**

Run: `vp test run --project unit apps/web/src/components/sidebar/environmentContextCard.logic.test.ts`
Expected: FAIL — module not found.

- [x] **Step 3: Implement the logic module**

```ts
// apps/web/src/components/sidebar/environmentContextCard.logic.ts
import {
  connectionStatusText,
  type CompatVerdict,
  type ConnectionTarget,
  type EnvironmentConnectionPresentation,
} from "@bibcode/client-runtime/connection";
import type { ServerConfig } from "@bibcode/contracts";

import { isDesktopLocalConnectionTarget } from "../../connection/desktopLocal";
import {
  resolveEnvironmentCompatVerdict,
  selectRemoteUpdateControlCapability,
} from "../../connection/environmentCompat";
import { resolveEnvironmentRailStatus, type EnvironmentRailStatus } from "./environmentRail.logic";

export interface EnvironmentCompatBadge {
  readonly label: string;
  readonly tone: "warning" | "error";
}

export function resolveCompatBadge(compat: CompatVerdict | null): EnvironmentCompatBadge | null {
  if (compat === null) {
    return null;
  }
  switch (compat.kind) {
    case "compatible":
      // Phase 7 renders "Up to date" from RemoteUpdateSnapshot via the
      // card's updateBadge slot; compatibility alone earns no badge.
      return null;
    case "legacy":
      return { label: "Limited compatibility", tone: "warning" };
    case "server-too-old":
      return { label: "Server update required", tone: "error" };
    case "client-too-old":
      return { label: "App update required", tone: "error" };
  }
}

export interface EnvironmentContextCardView {
  readonly name: string;
  readonly status: EnvironmentRailStatus;
  readonly statusText: string;
  readonly versionLine: string | null;
  readonly compatBadge: EnvironmentCompatBadge | null;
  readonly showUpdateActions: boolean;
}

/** Card view-model; `null` = card hidden (Local selected — primary or desktop-local). */
export function buildEnvironmentContextCardView(input: {
  readonly label: string;
  readonly target: ConnectionTarget;
  readonly connection: EnvironmentConnectionPresentation;
  readonly serverConfig: ServerConfig | null;
}): EnvironmentContextCardView | null {
  if (
    input.target._tag === "PrimaryConnectionTarget" ||
    isDesktopLocalConnectionTarget(input.target)
  ) {
    return null;
  }
  const compat = resolveEnvironmentCompatVerdict(input.serverConfig);
  const serverVersion = input.serverConfig?.environment.serverVersion ?? null;
  return {
    name: input.label,
    status: resolveEnvironmentRailStatus({
      phase: input.connection.phase,
      compat,
      updateAvailable: false,
    }),
    statusText: connectionStatusText(input.connection),
    versionLine: serverVersion === null ? null : `BiBCode v${serverVersion}`,
    compatBadge: resolveCompatBadge(compat),
    showUpdateActions: selectRemoteUpdateControlCapability(input.serverConfig),
  };
}
```

- [x] **Step 4: Run the logic test to verify it passes**

Run: `vp test run --project unit apps/web/src/components/sidebar/environmentContextCard.logic.test.ts`
Expected: PASS.

- [x] **Step 5: Write the failing component test**

```tsx
// apps/web/src/components/sidebar/EnvironmentContextCard.test.tsx
import { renderToStaticMarkup } from "react-dom/server";
import type { ReactElement, ReactNode } from "react";
import { describe, expect, it, vi } from "vite-plus/test";

import { EnvironmentId } from "@bibcode/contracts";

const h = vi.hoisted(() => ({
  environment: null as Record<string, unknown> | null,
  activeEnvironmentId: null as string | null,
  navigate: vi.fn(),
  commandCalls: [] as Array<{ label?: string; input: unknown }>,
  menuItems: [] as Array<Record<string, unknown>>,
  reset() {
    h.environment = null;
    h.activeEnvironmentId = null;
    h.navigate.mockReset();
    h.commandCalls = [];
    h.menuItems = [];
  },
}));

vi.mock("../../state/entities", () => ({
  useActiveEnvironmentId: () => h.activeEnvironmentId,
}));
vi.mock("../../state/environments", () => ({
  useEnvironment: () => h.environment,
}));
vi.mock("../../state/use-atom-command", () => ({
  useAtomCommand: (command: { label?: string }) => (input: unknown) => {
    h.commandCalls.push({ label: command.label, input });
    return Promise.resolve({ _tag: "Success", value: undefined });
  },
}));
vi.mock("../../connection/catalog", () => ({
  environmentCatalog: { disconnect: { label: "environment-catalog:disconnect" } },
}));
vi.mock("../../connection/environmentCompat", () => ({
  resolveEnvironmentCompatVerdict: () => null,
  selectRemoteUpdateControlCapability: (serverConfig: unknown) => serverConfig !== null,
}));
vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => h.navigate,
}));
vi.mock("../ui/menu", () => ({
  Menu: ({ children }: { children?: ReactNode }) => <>{children}</>,
  MenuPopup: ({ children }: { children?: ReactNode }) => <>{children}</>,
  MenuTrigger: ({ children }: { render?: ReactElement; children?: ReactNode }) => <>{children}</>,
  MenuItem: (props: Record<string, unknown>) => {
    h.menuItems.push(props);
    return null;
  },
}));

import { EnvironmentContextCard } from "./EnvironmentContextCard";

const ENV_REMOTE = EnvironmentId.make("env-remote");

function remoteEnvironment(serverConfig: unknown = null) {
  return {
    environmentId: ENV_REMOTE,
    label: "AI-SERVER",
    entry: { target: { _tag: "BearerConnectionTarget", connectionId: "paired-1" } },
    connection: { phase: "connected", error: null, traceId: null },
    serverConfig,
  };
}

describe("EnvironmentContextCard", () => {
  it("renders nothing for Local", () => {
    h.reset();
    h.activeEnvironmentId = ENV_REMOTE;
    h.environment = {
      ...remoteEnvironment(),
      entry: { target: { _tag: "PrimaryConnectionTarget" } },
    };
    expect(renderToStaticMarkup(<EnvironmentContextCard />)).toBe("");
  });

  it("renders name, status, and version line for a remote environment", () => {
    h.reset();
    h.activeEnvironmentId = ENV_REMOTE;
    h.environment = remoteEnvironment({
      environment: { serverVersion: "0.4.2", capabilities: {} },
    });
    const markup = renderToStaticMarkup(<EnvironmentContextCard />);
    expect(markup).toContain("AI-SERVER");
    expect(markup).toContain("Connected");
    expect(markup).toContain("BiBCode v0.4.2");
  });

  it("menu: Disconnect dispatches the Phase 4 latch; Manage… deep-links settings", () => {
    h.reset();
    h.activeEnvironmentId = ENV_REMOTE;
    h.environment = remoteEnvironment({
      environment: { serverVersion: "0.4.2", capabilities: {} },
    });
    renderToStaticMarkup(<EnvironmentContextCard />);
    const labels = h.menuItems.map((item) => item.children);
    expect(labels).toContain("Disconnect");
    expect(labels).toContain("Manage…");
    (h.menuItems.find((item) => item.children === "Disconnect")?.onClick as () => void)();
    expect(h.commandCalls).toEqual([
      { label: "environment-catalog:disconnect", input: ENV_REMOTE },
    ]);
    (h.menuItems.find((item) => item.children === "Manage…")?.onClick as () => void)();
    expect(h.navigate).toHaveBeenCalledWith({ to: "/settings/remote-servers" });
  });

  it("hides Check for updates until Phase 7 injects a handler (hidden-until-capable)", () => {
    h.reset();
    h.activeEnvironmentId = ENV_REMOTE;
    h.environment = remoteEnvironment({
      environment: { serverVersion: "0.4.2", capabilities: {} },
    });
    renderToStaticMarkup(<EnvironmentContextCard />);
    expect(h.menuItems.map((item) => item.children)).not.toContain("Check for updates");

    h.menuItems = [];
    const onCheckForUpdates = vi.fn();
    renderToStaticMarkup(<EnvironmentContextCard onCheckForUpdates={onCheckForUpdates} />);
    const item = h.menuItems.find((entry) => entry.children === "Check for updates");
    expect(item).toBeDefined();
    (item?.onClick as () => void)();
    expect(onCheckForUpdates).toHaveBeenCalledWith(ENV_REMOTE);
  });

  it("renders the Phase 7 update-badge slot verbatim", () => {
    h.reset();
    h.activeEnvironmentId = ENV_REMOTE;
    h.environment = remoteEnvironment({
      environment: { serverVersion: "0.4.2", capabilities: {} },
    });
    const markup = renderToStaticMarkup(
      <EnvironmentContextCard updateBadge={<span data-testid="update-badge">Up to date</span>} />,
    );
    expect(markup).toContain('data-testid="update-badge"');
  });
});
```

Note on "Check for updates": `selectRemoteUpdateControlCapability` is mocked truthy here, so
these cases pin that the _handler prop_ is the second gate — without Phase 7's
`onCheckForUpdates` the item never renders even on a capable server.

- [x] **Step 6: Run it to verify it fails**

Run: `vp test run --project unit apps/web/src/components/sidebar/EnvironmentContextCard.test.tsx`
Expected: FAIL — module not found.

- [x] **Step 7: Implement the component**

```tsx
// apps/web/src/components/sidebar/EnvironmentContextCard.tsx
import { useNavigate } from "@tanstack/react-router";
import { EllipsisIcon } from "lucide-react";
import * as React from "react";
import { useMemo } from "react";

import type { EnvironmentId } from "@bibcode/contracts";

import { environmentCatalog } from "../../connection/catalog";
import { cn } from "../../lib/utils";
import { useActiveEnvironmentId } from "../../state/entities";
import { useEnvironment } from "../../state/environments";
import { useAtomCommand } from "../../state/use-atom-command";
import { Menu, MenuItem, MenuPopup, MenuTrigger } from "../ui/menu";
import { buildEnvironmentContextCardView } from "./environmentContextCard.logic";
import type { EnvironmentRailStatus } from "./environmentRail.logic";

const CARD_STATUS_DOT_CLASS: Record<EnvironmentRailStatus, string> = {
  connected: "bg-success",
  disconnected: "bg-muted-foreground/50",
  attention: "bg-warning",
  error: "bg-destructive",
};

export interface EnvironmentContextCardProps {
  /** Phase 7 slot: update state badge ("Up to date" / "Update available"). */
  readonly updateBadge?: React.ReactNode;
  /** Phase 7 handler: shows the "Check for updates" menu item when the server
   * advertises remoteUpdateControl AND this handler is provided. */
  readonly onCheckForUpdates?: (environmentId: EnvironmentId) => void;
}

/**
 * Environment context card (spec §4.8, mockup Variant B): rendered under the
 * brand row; hidden while Local is selected. Disconnect is Phase 4's
 * client-side latch (spec §6) — it never deletes credentials.
 */
export function EnvironmentContextCard(props: EnvironmentContextCardProps) {
  const activeEnvironmentId = useActiveEnvironmentId();
  const environment = useEnvironment(activeEnvironmentId);
  const navigate = useNavigate();
  const disconnectEnvironment = useAtomCommand(environmentCatalog.disconnect, {
    reportFailure: false,
  });

  const view = useMemo(
    () =>
      environment === null
        ? null
        : buildEnvironmentContextCardView({
            label: environment.label,
            target: environment.entry.target,
            connection: environment.connection,
            serverConfig: environment.serverConfig,
          }),
    [environment],
  );

  if (view === null || activeEnvironmentId === null) {
    return null;
  }

  return (
    <div
      data-testid="environment-context-card"
      className="mx-2 mb-1 flex items-center gap-2 rounded-[10px] border border-border bg-background px-2.5 py-2"
    >
      <span
        data-status={view.status}
        className={cn("size-2 shrink-0 rounded-full", CARD_STATUS_DOT_CLASS[view.status])}
      />
      <div className="min-w-0 flex-1">
        <div className="truncate text-[13px] font-semibold text-foreground">{view.name}</div>
        <div className="flex min-w-0 items-center gap-1 truncate text-[11px] text-muted-foreground">
          <span className="truncate">{view.statusText}</span>
          {view.versionLine ? <span aria-hidden>·</span> : null}
          {view.versionLine ? <span className="shrink-0">{view.versionLine}</span> : null}
          {view.compatBadge ? (
            <span
              data-tone={view.compatBadge.tone}
              className={cn(
                "shrink-0 rounded-full px-1.5 py-px text-[10px] font-medium",
                view.compatBadge.tone === "error"
                  ? "bg-destructive/12 text-destructive-foreground"
                  : "bg-warning/15 text-warning-foreground",
              )}
            >
              {view.compatBadge.label}
            </span>
          ) : null}
          {props.updateBadge ?? null}
        </div>
      </div>
      <Menu>
        <MenuTrigger
          render={
            <button
              type="button"
              aria-label="Environment actions"
              data-testid="environment-context-card-menu"
              className="inline-flex size-6 shrink-0 items-center justify-center rounded-md text-muted-foreground hover:bg-accent hover:text-foreground"
            />
          }
        >
          <EllipsisIcon className="size-4" />
        </MenuTrigger>
        <MenuPopup align="end">
          <MenuItem onClick={() => void disconnectEnvironment(activeEnvironmentId)}>
            Disconnect
          </MenuItem>
          {view.showUpdateActions && props.onCheckForUpdates !== undefined ? (
            <MenuItem onClick={() => props.onCheckForUpdates?.(activeEnvironmentId)}>
              Check for updates
            </MenuItem>
          ) : null}
          <MenuItem onClick={() => void navigate({ to: "/settings/remote-servers" })}>
            Manage…
          </MenuItem>
        </MenuPopup>
      </Menu>
    </div>
  );
}
```

Adjust the `useAtomCommand` import path to the app's actual module
(`apps/web/src/state/use-atom-command.ts` — verify with `rg "useAtomCommand" apps/web/src/state`)
and the `environmentCatalog.disconnect` reference to Phase 4's shipped name (see Phase
interfaces; if the command does not exist, **stop and report** — do not substitute
`remove`, which deletes credentials).

- [x] **Step 8: Run the component test to verify it passes**

Run: `vp test run --project unit apps/web/src/components/sidebar/EnvironmentContextCard.test.tsx`
Expected: PASS (5 tests).

- [x] **Step 9: Mount the card in `Sidebar.tsx` and pin it in `Sidebar.test.tsx`**

In `apps/web/src/components/Sidebar.tsx` add the import:

```ts
import { EnvironmentContextCard } from "./sidebar/EnvironmentContextCard";
```

Then in the main `Sidebar()` return (~line 4657), directly after `<SidebarChromeHeader />`
and inside the non-settings branch, mount the card:

```tsx
      <SidebarChromeHeader />

      {isOnSettings ? (
        <SettingsSidebarNav pathname={pathname} />
      ) : (
        <>
          <EnvironmentContextCard />
          <SidebarProjectsContent
```

(The settings branch replaces the panel with the settings nav; the card belongs to the
projects view. "Under the brand row" per §4.8 = immediately below `SidebarChromeHeader`.)

In `apps/web/src/components/Sidebar.test.tsx`, add a capture-mock next to the other
component mocks:

```tsx
vi.mock("./sidebar/EnvironmentContextCard", () => ({
  EnvironmentContextCard: () => <div data-testid="environment-context-card-mock" />,
}));
```

and one assertion inside the Task 5 describe block:

```tsx
it("mounts the environment context card under the brand row", () => {
  seedTwoEnvironments();
  h.state.activeEnvironmentId = ENV_REMOTE;
  const markup = renderSidebar();
  expect(markup).toContain("environment-context-card-mock");
});
```

- [x] **Step 10: Run both suites to verify they pass**

Run: `vp test run --project unit apps/web/src/components/Sidebar.test.tsx apps/web/src/components/sidebar/EnvironmentContextCard.test.tsx`
Expected: PASS.

- [x] **Step 11: Commit**

```bash
git add apps/web/src/components/sidebar/environmentContextCard.logic.ts apps/web/src/components/sidebar/environmentContextCard.logic.test.ts apps/web/src/components/sidebar/EnvironmentContextCard.tsx apps/web/src/components/sidebar/EnvironmentContextCard.test.tsx apps/web/src/components/Sidebar.tsx apps/web/src/components/Sidebar.test.tsx
git commit -m "feat(web): environment context card under the brand row"
```

---

### Task 7: Fix the primary-environment editor-list leak (D4)

**Files:**

- Modify: `apps/web/src/components/Sidebar.tsx` — `SidebarProjectItem` component:
  the `availableEditors` declaration at ~line 1538 (with a three-line TODO comment above
  it), its uses at ~line 2653 (`handleThreadContextMenu`) and ~line 2885
  (`handlePrimaryRowContextMenu`), and both `useCallback` dependency arrays (~lines 2848, 2988)
- Test: `apps/web/src/components/Sidebar.test.tsx` (extend)

**Interfaces:**

- Consumes: `serverConfigs` (already `const serverConfigs = useServerConfigs()` at
  ~line 1502 in the same component; `Map<EnvironmentId, ServerConfig>` from
  `environmentServerConfigsAtom` in `apps/web/src/state/server.ts`).
- Produces: per-row editor lists keyed by the row's own `environmentId` — grouped sidebar
  rows belonging to a remote environment see that environment's editors.

- [x] **Step 1: Write the failing test**

Add to `apps/web/src/components/Sidebar.test.tsx` (same describe conventions; the harness
already captures `api.contextMenu.show` via `h.spies.contextMenuShow` and exposes
`h.state.serverConfigs` and `h.state.localApi`):

```tsx
staticDescribe("Sidebar per-environment editor lists", () => {
  const ENV_REMOTE = EnvironmentId.make("env-remote");

  it("builds the Open-in submenu from the row's own environment (D4), not the primary", async () => {
    h.state.environments = [
      environmentFixture({ environmentId: ENV_MAIN, label: "Local", primary: true }),
      environmentFixture({
        environmentId: ENV_REMOTE,
        label: "AI-SERVER",
        connectionId: "paired-1",
      }),
    ];
    h.state.primaryEnvironmentId = ENV_MAIN;
    h.state.serverConfigs = new Map([
      [ENV_MAIN, { environment: {}, availableEditors: ["vscode"] }],
      [ENV_REMOTE, { environment: {}, availableEditors: ["cursor"] }],
    ]);
    h.state.projects = [
      makeProject("project-b", {
        environmentId: ENV_REMOTE,
        title: "Remote Repo",
        workspaceRoot: "/srv/remote-repo",
      }),
    ];
    const remoteThread = makeThread("thread-remote", {
      environmentId: ENV_REMOTE,
      projectId: ProjectId.make("project-b"),
      worktreePath: "/srv/remote-repo-wt",
      branch: "main",
    });
    h.state.threads = [remoteThread];
    h.state.localApi = makeLocalApi(); // the file's existing local-api fixture helper
    h.spies.contextMenuShow.mockResolvedValue(null);

    await openThreadContextMenu(remoteThread); // see note below
    const items = h.spies.contextMenuShow.mock.calls.at(-1)?.[0] as Array<{
      id: string;
      children?: Array<{ id: string }>;
    }>;
    const openIn = items.find((item) => item.id === "open-in");
    const childIds = (openIn?.children ?? []).map((child) => child.id);
    expect(childIds).toContain("open-in:cursor");
    expect(childIds).not.toContain("open-in:vscode");
  });
});
```

Note: the file already contains context-menu tests that render the sidebar and invoke a
thread row's `onContextMenu`/`handleThreadContextMenu` capture — reuse the exact invocation
helper those tests use (search the file for `contextMenuShow.mockImplementation`) instead of
`openThreadContextMenu`, and reuse its local-api fixture (search for
`h.state.localApi =`) instead of `makeLocalApi` if named differently. The worktree-status
argument must satisfy `selectWorktreeWorkspaceActionsAvailable` (copy the shape from the
neighboring "open-in" test) so the submenu is populated.

- [x] **Step 2: Run it to verify it fails**

Run: `vp test run --project unit apps/web/src/components/Sidebar.test.tsx`
Expected: the new case FAILS — the submenu is built from the primary server's editors
(`open-in:vscode` present, `open-in:cursor` absent).

- [x] **Step 3: Implement the fix**

In `apps/web/src/components/Sidebar.tsx`, `SidebarProjectItem`:

1. At ~line 1535–1538: **delete the three-line reference-port TODO comment** (the one noting
   that rows read the PRIMARY server's editor list — do not keep any part of it) and replace
   the declaration

```ts
const availableEditors = useAtomValue(primaryServerConfigAtom)?.availableEditors ?? [];
```

with a per-environment resolver (D4: rows resolve against their own backend):

```ts
// Editor lists are per-environment: a grouped row belonging to a remote
// environment resolves "Open in" against its own backend's editors (D4).
const availableEditorsFor = useCallback(
  (environmentId: EnvironmentId): ReadonlyArray<EditorId> =>
    serverConfigs.get(environmentId)?.availableEditors ?? EMPTY_EDITOR_IDS,
  [serverConfigs],
);
```

Add near the file's other module-level constants:

```ts
const EMPTY_EDITOR_IDS: ReadonlyArray<EditorId> = [];
```

and add `EditorId` to the existing type imports from `@bibcode/contracts` (top of file).
`primaryServerConfigAtom` stays imported — it is still used by `useSidebarStageLabel`.

2. In `handleThreadContextMenu` (~line 2653), change only the declaration line (the comment
   block above it stays as-is):

```ts
const openInEditorOptions = EDITORS.filter((editor) =>
  availableEditorsFor(thread.environmentId).includes(editor.id),
);
```

and in its dependency array (~line 2848) replace the `availableEditors,` entry with
`availableEditorsFor,`.

3. In `handlePrimaryRowContextMenu` (~line 2885):

```ts
const openInEditorOptions = EDITORS.filter((editor) =>
  availableEditorsFor(project.environmentId).includes(editor.id),
);
```

and in its dependency array (~line 2988) replace `availableEditors,` with
`availableEditorsFor,`.

(Other comments in the file carrying the same reference-port TODO tag — e.g. the
fallback-provider note at ~line 2251 — are out of scope; leave them.)

- [x] **Step 4: Run the tests to verify they pass**

Run: `vp test run --project unit apps/web/src/components/Sidebar.test.tsx`
Expected: PASS — the new case and every pre-existing "open-in" case (those seed only the
primary environment, whose editors now resolve through the map to the same values).

- [x] **Step 5: Commit**

```bash
git add apps/web/src/components/Sidebar.tsx apps/web/src/components/Sidebar.test.tsx
git commit -m "fix(web): resolve sidebar editor lists per row environment"
```

---

### Task 8: "Add project on \<name\>" and active-environment default host

**Files:**

- Modify: `apps/web/src/components/Sidebar.tsx` (add-project trigger, ~lines 3854–3869 in
  `SidebarProjectsContent`; plumb one new prop from `Sidebar()`)
- Modify: `apps/web/src/components/CommandPalette.tsx` ("Add project" action item,
  ~line 405)
- Modify: `apps/web/src/components/add-project/useAddProjectWorkflow.ts` (hosts list +
  initial host)
- Test: `apps/web/src/components/add-project/useAddProjectWorkflow.public.test.tsx` (extend)
- Test: `apps/web/src/components/Sidebar.test.tsx` (extend)

**Interfaces:**

- Consumes: `resolveAddProjectTargetLabel` (Task 2), `useActiveEnvironmentId`,
  `isDesktopLocalConnectionTarget`.
- Produces: `AddProjectWorkflowStateInput.initialEnvironmentId: EnvironmentId | null` (new
  field on the exported input interface of `useAddProjectWorkflowState`).

Scope note (§4.8 verbatim): only "Add project" affordances change copy. The "Projects"
section label is untouched (Design decision 6). Remote hosts route through the existing
manual-path flow: `canUseNativeHostFolderPicker` (`apps/web/src/components/hostFolderPicker.ts`)
already returns false for non-primary hosts without a `desktopInstanceId`, so `browse()`
falls back to the host-path step — no picker change is needed.

- [x] **Step 1: Write the failing workflow tests**

In `apps/web/src/components/add-project/useAddProjectWorkflow.public.test.tsx`, follow the
file's existing setup (it exercises `useAddProjectWorkflowState` with an explicit `input`
object) and add:

```tsx
describe("active-environment defaulting", () => {
  it("defaults the selected host to the active environment when it is a listed host", () => {
    const workflow = renderWorkflowState({
      // reuse the file's host fixtures: primary host + one remote host
      hosts: [primaryHostFixture, remoteHostFixture],
      primaryEnvironmentId: primaryHostFixture.environmentId,
      initialEnvironmentId: remoteHostFixture.environmentId,
    });
    expect(workflow.selectedHost.environmentId).toBe(remoteHostFixture.environmentId);
  });

  it("falls back to the primary host when the active environment is not a host", () => {
    const workflow = renderWorkflowState({
      hosts: [primaryHostFixture],
      primaryEnvironmentId: primaryHostFixture.environmentId,
      initialEnvironmentId: EnvironmentId.make("env-unknown"),
    });
    expect(workflow.selectedHost.environmentId).toBe(primaryHostFixture.environmentId);
  });
});
```

(Adapt fixture/helper names to the file's own — search it for `useAddProjectWorkflowState(`.
Every existing call site of the state hook in the test file gains
`initialEnvironmentId: null`.)

- [x] **Step 2: Run to verify failure**

Run: `vp test run --project unit apps/web/src/components/add-project/useAddProjectWorkflow.public.test.tsx`
Expected: FAIL — `initialEnvironmentId` is not part of `AddProjectWorkflowStateInput` /
selected host ignores it.

- [x] **Step 3: Implement the workflow changes**

In `apps/web/src/components/add-project/useAddProjectWorkflow.ts`:

1. Add to `AddProjectWorkflowStateInput`:

```ts
  /** Rail-selected environment; wins over the primary host when it is a listed host. */
  readonly initialEnvironmentId: EnvironmentId | null;
```

2. Replace the `primaryHost` helper with (and update its two call sites — the initial
   `useMemo` and the dialog-open reset effect):

```ts
function initialWorkflowHost(input: AddProjectWorkflowStateInput): AddProjectHostOption {
  return (
    (input.initialEnvironmentId !== null
      ? input.hosts.find((host) => host.environmentId === input.initialEnvironmentId)
      : undefined) ??
    input.hosts.find((host) => host.environmentId === input.primaryEnvironmentId) ??
    input.hosts[0] ??
    fallbackHost(input.primaryEnvironmentId)
  );
}
```

3. In `useAddProjectWorkflow` (the wiring hook at the bottom of the file):
   - read the selection: `const activeEnvironmentId = useActiveEnvironmentId();`
     (import from `../../state/entities`), and pass
     `initialEnvironmentId: activeEnvironmentId` into `useAddProjectWorkflowState`'s input;
   - make saved remote environments hosts on every surface by extending the
     `presentedEnvironments` filter inside the `hosts` memo:

```ts
const presentedEnvironments = environments.filter(
  (environment) =>
    presentation.presentsTarget(environment.entry.target) ||
    isRemoteEnvironmentTarget(environment.entry.target),
);
```

with a local helper above the hook:

```ts
import type { ConnectionTarget } from "@bibcode/client-runtime/connection";

/** Saved remote environments (SSH, direct, relay) are add-project hosts (§4.8). */
function isRemoteEnvironmentTarget(target: ConnectionTarget): boolean {
  return (
    (target._tag === "BearerConnectionTarget" ||
      target._tag === "RelayConnectionTarget" ||
      target._tag === "SshConnectionTarget") &&
    !isDesktopLocalConnectionTarget(target)
  );
}
```

- extend the `usableHosts` filter in the same memo so remote hosts survive on desktop:

```ts
const usableHosts = catalogHosts.filter(
  (host) =>
    presentation.surface === "browser" ||
    host.isPrimary ||
    host.desktopInstanceId !== null ||
    remoteEnvironmentIds.has(host.environmentId),
);
```

where `remoteEnvironmentIds` is computed once at the top of the memo:

```ts
const remoteEnvironmentIds = new Set(
  environments
    .filter((environment) => isRemoteEnvironmentTarget(environment.entry.target))
    .map((environment) => environment.environmentId),
);
```

- [x] **Step 4: Run to verify the workflow tests pass**

Run: `vp test run --project unit apps/web/src/components/add-project/useAddProjectWorkflow.public.test.tsx apps/web/src/components/add-project/useAddProjectWorkflow.test.tsx`
Expected: PASS (update any existing case that constructs the input object to include
`initialEnvironmentId: null`).

- [x] **Step 5: Write the failing copy tests, then implement the copy**

Test (add to the Task 5 describe in `apps/web/src/components/Sidebar.test.tsx`):

```tsx
it('labels the add-project trigger "Add project on <name>" for a remote selection', () => {
  seedTwoEnvironments();
  h.state.activeEnvironmentId = ENV_REMOTE;
  const markup = renderSidebar();
  expect(markup).toContain("Add project on AI-SERVER");

  h.state.activeEnvironmentId = ENV_MAIN;
  const localMarkup = renderSidebar();
  expect(localMarkup).not.toContain("Add project on");
});
```

Run `vp test run --project unit apps/web/src/components/Sidebar.test.tsx` — expected FAIL.

Implementation:

1. `apps/web/src/components/Sidebar.tsx`, main `Sidebar()` (after the Task 5 memos):

```ts
const addProjectLabel = useMemo(() => {
  const remoteLabel = resolveAddProjectTargetLabel({
    activeEnvironmentId,
    candidates: environments.map((environment) => ({
      environmentId: environment.environmentId,
      label: environment.label,
      isLocal:
        environment.entry.target._tag === "PrimaryConnectionTarget" ||
        isDesktopLocalConnectionTarget(environment.entry.target),
    })),
  });
  return remoteLabel === null ? "Add project" : `Add project on ${remoteLabel}`;
}, [activeEnvironmentId, environments]);
```

(import `resolveAddProjectTargetLabel` alongside the Task 5 logic import). Add
`addProjectLabel: string` to `SidebarProjectsContentProps`, pass
`addProjectLabel={addProjectLabel}` where `SidebarProjectsContent` is rendered, and in
`SidebarProjectsContent` replace the trigger's fixed copy (~lines 3854–3869):
`aria-label="Add project"` → `aria-label={addProjectLabel}` and the tooltip text
`<TooltipPopup side="right">Add project</TooltipPopup>` →
`<TooltipPopup side="right">{addProjectLabel}</TooltipPopup>`.

2. `apps/web/src/components/CommandPalette.tsx`: in the component that builds `actionItems`
   (~line 405), compute the same label. The component already has access to hooks — add:

```ts
const { environments } = useEnvironments();
const activeEnvironmentId = useActiveEnvironmentId();
const addProjectTargetLabel = resolveAddProjectTargetLabel({
  activeEnvironmentId,
  candidates: environments.map((environment) => ({
    environmentId: environment.environmentId,
    label: environment.label,
    isLocal:
      environment.entry.target._tag === "PrimaryConnectionTarget" ||
      isDesktopLocalConnectionTarget(environment.entry.target),
  })),
});
```

(imports: `useEnvironments` from `../state/environments`, `useActiveEnvironmentId` from
`../state/entities`, `resolveAddProjectTargetLabel` from
`./sidebar/environmentRail.logic`, `isDesktopLocalConnectionTarget` from
`../connection/desktopLocal`; if the action list is built outside the component, thread the
label in as a parameter). Then change the item title:

```ts
    title:
      addProjectTargetLabel === null
        ? "Add project"
        : `Add project on ${addProjectTargetLabel}`,
```

Run `vp test run --project unit apps/web/src/components/Sidebar.test.tsx` — expected PASS.
If `apps/web/src/components/CommandPalette.test.tsx` exists (check with `ls`), run it too
and update its expectations only if it pins the old constant title.

- [x] **Step 6: Commit**

```bash
git add apps/web/src/components/Sidebar.tsx apps/web/src/components/Sidebar.test.tsx apps/web/src/components/CommandPalette.tsx apps/web/src/components/add-project/useAddProjectWorkflow.ts apps/web/src/components/add-project/useAddProjectWorkflow.public.test.tsx apps/web/src/components/add-project/useAddProjectWorkflow.test.tsx
git commit -m "feat(web): environment-aware add-project target and copy"
```

---

### Task 9: `?action=add-server` deep link on the Remote Servers route

**Files:**

- Modify: `apps/web/src/routes/settings.remote-servers.tsx` (Phase 4's route file)
- Test: colocated with the route's existing tests if Phase 4 created any; otherwise add the
  search-validation unit test below as
  `apps/web/src/routes/settingsRemoteServersSearch.test.ts` against an exported helper

Coordination note: Phase 4 owns this route. If Phase 4 already implemented search handling
for its Add Server flow, verify the rail's `search: { action: "add-server" }` (Task 3)
matches it, adapt the rail if the key differs, and skip the rest of this task.

- [x] **Step 1: Write the failing test**

```ts
// apps/web/src/routes/settingsRemoteServersSearch.test.ts
import { describe, expect, it } from "vite-plus/test";

import { validateRemoteServersSearch } from "./settings.remote-servers";

describe("validateRemoteServersSearch", () => {
  it("accepts the add-server action and drops everything else", () => {
    expect(validateRemoteServersSearch({ action: "add-server" })).toEqual({
      action: "add-server",
    });
    expect(validateRemoteServersSearch({})).toEqual({});
    expect(validateRemoteServersSearch({ action: "other" })).toEqual({});
  });
});
```

- [x] **Step 2: Run it to verify it fails**

Run: `vp test run --project unit apps/web/src/routes/settingsRemoteServersSearch.test.ts`
Expected: FAIL — `validateRemoteServersSearch` is not exported.

- [x] **Step 3: Implement**

In `apps/web/src/routes/settings.remote-servers.tsx`:

```ts
export interface RemoteServersSearch {
  readonly action?: "add-server";
}

export function validateRemoteServersSearch(search: Record<string, unknown>): RemoteServersSearch {
  return search["action"] === "add-server" ? { action: "add-server" } : {};
}
```

Wire it into the route definition (`createFileRoute("/settings/remote-servers")({ … })`):

```ts
  validateSearch: validateRemoteServersSearch,
```

and in the route component, open Phase 4's Add Server flow when the action arrives, then
clear the param so refresh/back does not reopen it:

```tsx
const { action } = Route.useSearch();
const navigate = useNavigate();
useEffect(() => {
  if (action !== "add-server") {
    return;
  }
  openAddServerDialog(); // Phase 4's existing opener on the Connect tab
  void navigate({ to: "/settings/remote-servers", replace: true, search: {} });
}, [action, navigate]);
```

Adapt `openAddServerDialog` to Phase 4's actual opener (a `setState` or dialog-store call in
the Connect tab component — locate it with `rg -i "add server" apps/web/src/components/settings`).
If Task 3 left a `search … as never` cast in `EnvironmentRail.tsx`, remove it now.

- [x] **Step 4: Run tests to verify they pass**

Run: `vp test run --project unit apps/web/src/routes/settingsRemoteServersSearch.test.ts apps/web/src/components/sidebar/EnvironmentRail.test.tsx`
Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add apps/web/src/routes/settings.remote-servers.tsx apps/web/src/routes/settingsRemoteServersSearch.test.ts apps/web/src/components/sidebar/EnvironmentRail.tsx
git commit -m "feat(web): add-server deep link into Remote Servers settings"
```

---

### Task 10: Living docs, runbooks, and the phase validation gate

**Files:**

- Modify: `docs/architecture/connection-runtime.md`
- Modify: `docs/testing/cross-platform-validation.md`
- Review only: `docs/testing/windows-desktop.md`, `docs/testing/linux-desktop.md`,
  `docs/testing/macos-desktop.md`, `docs/testing/README.md`, `docs/architecture/remote.md`,
  `docs/architecture/overview.md`

- [x] **Step 1: Update `docs/architecture/connection-runtime.md`**

Locate the client-presentation discussion (the section describing that project grouping is
presentation-only) and add a paragraph:

> **Environment rail selection.** The web client's left panel carries an environment rail
> (`apps/web/src/components/sidebar/EnvironmentRail.tsx`): Local (the primary environment
> plus host-managed `local:` desktop backends, grouped per the
> `DESKTOP_LOCAL_CONNECTION_ID_PREFIX` convention) and one entry per saved remote
> environment. Selection writes `activeEnvironmentIdAtom` and scopes _presentation only_:
> the panel filters which environments' projects and threads it shows, and "Add project"
> targets the selected environment. Selection never changes supervisor desired state —
> connections to other environments stay live and streaming — and operations on an entity
> always route to the entity's own `environmentId` regardless of selection. When a remote
> environment is selected, a context card under the brand row shows its connection status,
> server version, and compatibility verdict.

- [x] **Step 2: Update the packaged visual-validation runbook**

In `docs/testing/cross-platform-validation.md`, section "Packaged visual validation", extend
the first coverage bullet from "Add Project and environment presentation;" to:

```markdown
- Add Project and environment presentation, including the left-panel environment
  rail (Local entry with its WSL sub-picker where applicable, saved-server
  entries with status dots, the add/manage affordances) and the environment
  context card with its ⋯ menu when a remote environment is selected —
  verifying that switching rail selection filters the projects panel without
  interrupting running sessions on other environments;
```

- [x] **Step 3: Review the remaining runbooks and living docs**

Read `docs/testing/windows-desktop.md`, `linux-desktop.md`, `macos-desktop.md`,
`docs/testing/README.md`, `docs/architecture/remote.md`, and
`docs/architecture/overview.md` against this phase's changes. Phase 6 changes no test
commands, package scripts, provider visibility, worktree lifecycle, or process behavior, so
they are expected to need no edits — the final report must state they were **reviewed and
remain accurate** (or update them if drift is found).

- [x] **Step 4: Run the full phase validation gate**

From the repo root:

```bash
vp test run --project unit apps/web/src
vp check
vp run typecheck
```

Expected: all pass. Then review the working tree:

```bash
git status --short
git diff --stat
```

Confirm: no `.codegraph/` changes staged, no edits outside the files this plan names, and
the user's pending deletions under `docs/plans/2026-08-24-environment-project-management/`
are untouched.

- [x] **Step 5: Commit**

```bash
git add docs/architecture/connection-runtime.md docs/testing/cross-platform-validation.md
git commit -m "docs: environment rail selection semantics and visual-validation coverage"
```

- [x] **Step 6: Report**

Report the exact commands run (including any that could not run and why), the runbook
review statement, and residual risk (at minimum: Phase 4 disconnect-latch dependency if it
was missing; Phase 2 verdict-function naming if adapted; typed-router search-param handling
if the Task 3 cast workaround was needed).

---

## Self-review (executed while writing this plan)

- **Spec coverage (§4.8 line by line):** 52px rail → Task 3/4; Local top entry with WSL
  sub-picker keyed on the `local:` prefix → Tasks 2/3; divider → Task 3; one entry per saved
  remote with letter-avatar + 4-state status dot → Tasks 2/3; bottom "Add server…" and
  "Manage remote servers…" deep-linking `/settings/remote-servers` → Tasks 3/9; selection
  writes `activeEnvironmentIdAtom` → Task 3; context card under the brand row, hidden for
  Local, name/status/`BiBCode v<serverVersion>`/badge + ⋯ menu (Disconnect / Check for
  updates / Manage…) → Task 6; "Add project on \<name\>" → Task 8; D3/D4 selection semantics
  → Tasks 3/5 tests; master-plan Phase 6 row's "primary-environment leak fixes" → Task 7.
- **Placeholder scan:** every step carries runnable code or an exact edit location; the two
  intentional adaptation points (Phase 2 verdict name, Phase 4 disconnect command name) are
  confined to single named lines with verification commands.
- **Type consistency:** `EnvironmentRailCandidate`/`EnvironmentRailEntry`/
  `EnvironmentRailModel`/`EnvironmentRailStatus` (Task 2) are consumed by those names in
  Tasks 3, 5, 6; `resolveEnvironmentCompatVerdict`/`selectRemoteUpdateControlCapability`
  (Task 1) in Tasks 3, 6; `initialEnvironmentId` (Task 8) matches its test;
  `validateRemoteServersSearch` (Task 9) matches the rail's `search` object from Task 3.
- **Deltas from the coordinator's external review (amended spec §4.8, "Selection
  semantics"):**
  1. _Null selection = Local, filtered._ `selectRailVisibleEnvironmentIds` now scopes a
     null/absent — and a ghost/unresolvable — `activeEnvironmentId` to the local
     environment set instead of "no filtering"; "show everything" survives only in the
     degenerate no-local-environment catalog. Updated: Task 2 implementation + three scope
     tests (null → local set pinned explicitly), Task 5's null-selection Sidebar test and
     its expected-failure note, Design decisions 3 and 8.
  2. _Amber update-dot wiring pinned for Phase 7._ `resolveEnvironmentRailStatus` accepts
     `updateAvailable` (already tested → `attention`); `toEnvironmentRailCandidate` now
     takes it as a required pass-through parameter (with a pass-through test), and the
     Phase 7 seam is pinned by name in "Phase interfaces / Produces" and at the single
     call site in `EnvironmentRail.tsx`:
     `useEnvironmentUpdateAvailability(): ReadonlyMap<EnvironmentId, boolean>` replaces
     the constant `false`.
