# Pull Requests in a Thread's Right Panel Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Open a pull/merge request's full detail as a tab in a thread's right
panel: automatically after Done in Source Control's create dialog, from "Open
{MR/PR} in panel" next to "View" in Source Control's menu, and from the full
page through a thread picker.

**Architecture:** A new `pullRequest` right-panel surface (one per request
number, persisted per thread) hosts the existing `PullRequestsDetailView`. The
detail view and its descendants stop using the router and read a
`PullRequestsNavigation` from React context: the full page provides a router
adapter (unchanged URLs and behaviour), the panel provides a store adapter. The
panel host reuses the full page's availability and context loading through an
extracted `usePullRequestsContextQuery` hook.

**Tech Stack:** React 19.2 + React Compiler, TanStack Router, zustand (persist),
Effect atoms (`useEnvironmentQuery`), Tailwind 4 container queries, Vite+ test
(`vite-plus/test`, happy-dom).

**Spec:** `docs/superpowers/specs/2026-10-15-pull-request-right-panel-design.md`

## Global Constraints

- Repository root: `/work/workspaces/orca/BibCode/makara`, branch `mubeda/pr-right-panel`.
- `vp` is not on PATH: run `node scripts/run-local-vp.mjs <args>` from the repo root.
- Web unit tests: `cd apps/web && node ../../scripts/run-local-vp.mjs test run --project unit <files>`.
- Lint is warning-free and `vp check` fails on any warning (`lint.options.denyWarnings`); no `oxlint-disable` comments.
- Every component and hook change follows the `vercel-react-best-practices` rules and `UI.md`.
- No new RPC method, server change, contract change, child process or desktop bridge call.
- `RIGHT_PANEL_STORAGE_VERSION` stays `9`.
- Copy: "Open {MR/PR} in panel" (`{MR/PR}` is the provider's short label: "MR" on GitLab, "PR" on GitHub), "Open full page", "Open beside a thread…", "On this branch".
- Run `codex review -c sandbox_mode="read-only" --uncommitted` before every commit and address its findings.
- Commit messages end with:
  ```
  Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
  Claude-Session: https://claude.ai/code/session_01RsdQGFUPxjNZ8GQocThxT1
  ```

## Review Focus

1. A thread opens the same request twice (Done after "Open in panel"): exactly one tab, activated, keeping its selected detail tab. (Task 1)
2. The thread's workspace disappears or the environment disconnects while a request tab is open: the tab shows the unavailable reason instead of an error loop, and recovers when the workspace returns. (Task 5)
3. A revert action in the panel opens the revert request as a second tab; a delete closes only that tab. (Task 5)
4. The full page keeps today's URLs: tab changes replace history, conversation file links push `#file=…`, the tree replaces it. (Task 2)
5. A saved panel state with a corrupted `pullRequest` entry (bad number, mismatched id, unknown provider or tab) loads without losing the thread's other tabs. (Task 1)

---

### Task 1: `pullRequest` right-panel surface

**Files:**
- Modify: `apps/web/src/rightPanelStore.ts`
- Modify: `apps/web/src/components/RightPanelTabs.tsx:211-283`
- Test: `apps/web/src/rightPanelStore.test.ts`

**Interfaces:**
- Produces:
  ```ts
  export type PullRequestRightPanelSurface = {
    id: `pullRequest:${number}`;
    kind: "pullRequest";
    number: number;
    provider: SourceControlProviderKind;
    tab: PullRequestsDetailTab;
  };
  // RightPanelStoreState additions:
  openPullRequest: (
    ref: ScopedThreadRef,
    request: { number: number; provider: SourceControlProviderKind },
    tab?: PullRequestsDetailTab,
  ) => void;
  setPullRequestTab: (ref: ScopedThreadRef, number: number, tab: PullRequestsDetailTab) => void;
  export function pullRequestSurfaceId(number: number): `pullRequest:${number}`;
  ```

- [ ] **Step 1: Write the failing tests**

Append to the `describe("rightPanelStore", …)` block in `apps/web/src/rightPanelStore.test.ts`:

```ts
  it("opens one pull request surface per number and keeps its tab on reopen", () => {
    const store = useRightPanelStore.getState();
    store.open(refA, "sourceControl");
    store.openPullRequest(refA, { number: 89, provider: "gitlab" });
    store.setPullRequestTab(refA, 89, "checks");
    store.openPullRequest(refA, { number: 89, provider: "gitlab" });
    expect(selectThreadRightPanelState(useRightPanelStore.getState().byThreadKey, refA)).toEqual({
      isOpen: true,
      activeSurfaceId: "pullRequest:89",
      surfaces: [
        { id: "sourceControl", kind: "sourceControl" },
        { id: "pullRequest:89", kind: "pullRequest", number: 89, provider: "gitlab", tab: "checks" },
      ],
    });
  });

  it("opens a second request as another tab and honours an explicit tab", () => {
    const store = useRightPanelStore.getState();
    store.openPullRequest(refA, { number: 89, provider: "gitlab" });
    store.openPullRequest(refA, { number: 90, provider: "gitlab" }, "files");
    store.openPullRequest(refA, { number: 89, provider: "gitlab" }, "commits");
    const state = selectThreadRightPanelState(useRightPanelStore.getState().byThreadKey, refA);
    expect(state.activeSurfaceId).toBe("pullRequest:89");
    expect(state.surfaces).toEqual([
      { id: "pullRequest:89", kind: "pullRequest", number: 89, provider: "gitlab", tab: "commits" },
      { id: "pullRequest:90", kind: "pullRequest", number: 90, provider: "gitlab", tab: "files" },
    ]);
  });

  it("ignores a tab change for a request that is not open", () => {
    useRightPanelStore.getState().setPullRequestTab(refA, 7, "files");
    expect(useRightPanelStore.getState().byThreadKey).toEqual({});
  });

  it("keeps valid pull request surfaces and drops corrupted ones on load", () => {
    expect(
      migratePersistedRightPanelState({
        byThreadKey: {
          "env-1:thread-A": {
            isOpen: true,
            activeSurfaceId: "pullRequest:89",
            surfaces: [
              { id: "pullRequest:89", kind: "pullRequest", number: 89, provider: "gitlab", tab: "files" },
              { id: "pullRequest:5", kind: "pullRequest", number: 5, provider: "github", tab: "nope" },
              { id: "pullRequest:6", kind: "pullRequest", number: 7, provider: "github", tab: "files" },
              { id: "pullRequest:0", kind: "pullRequest", number: 0, provider: "github", tab: "files" },
              { id: "pullRequest:8", kind: "pullRequest", number: 8, provider: "svn", tab: "files" },
              { id: "diff", kind: "diff" },
            ],
          },
        },
      }),
    ).toEqual({
      byThreadKey: {
        "env-1:thread-A": {
          isOpen: true,
          activeSurfaceId: "pullRequest:89",
          surfaces: [
            { id: "pullRequest:89", kind: "pullRequest", number: 89, provider: "gitlab", tab: "files" },
            {
              id: "pullRequest:5",
              kind: "pullRequest",
              number: 5,
              provider: "github",
              tab: "conversation",
            },
            { id: "diff", kind: "diff" },
          ],
        },
      },
    });
  });

  it("statically excludes pull requests from generic open and toggle actions", () => {
    const genericPullRequestCallsMustFail = () => {
      // @ts-expect-error Pull request surfaces open through openPullRequest.
      useRightPanelStore.getState().open(refA, "pullRequest");
      // @ts-expect-error Pull request surfaces must not use the generic toggle action.
      useRightPanelStore.getState().toggle(refA, "pullRequest");
    };
    void genericPullRequestCallsMustFail;
    expect(true).toBe(true);
  });
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cd apps/web && node ../../scripts/run-local-vp.mjs test run --project unit src/rightPanelStore.test.ts`
Expected: FAIL — `openPullRequest is not a function` and the migration test drops every `pullRequest` entry.

- [ ] **Step 3: Implement the surface in `apps/web/src/rightPanelStore.ts`**

Add the imports:

```ts
import { ACTIVITY_ID_MAX_LENGTH, type ScopedThreadRef, SourceControlProviderKind } from "@bibcode/contracts";
import type { PullRequestsDetailTab } from "./pullRequestsStore";
```

(`SourceControlProviderKind` is both the schema and the type exported by `packages/contracts/src/sourceControl.ts`; keep `ACTIVITY_ID_MAX_LENGTH` and `ScopedThreadRef` as they are.)

Add `"pullRequest"` to `RIGHT_PANEL_KINDS` after `"activity"`, and the surface type and its union member:

```ts
export type PullRequestRightPanelSurface = {
  id: `pullRequest:${number}`;
  kind: "pullRequest";
  number: number;
  provider: SourceControlProviderKind;
  tab: PullRequestsDetailTab;
};
```

In `RightPanelSurface`, add `| PullRequestRightPanelSurface` after `| ActivityRightPanelSurface`.

Add a type alias for the generic singleton kinds and use it everywhere the store currently writes `Exclude<RightPanelKind, "file" | "terminal" | "activity">` (the `open` and `toggle` signatures in `RightPanelStoreState`):

```ts
type GenericRightPanelKind = Exclude<RightPanelKind, "file" | "terminal" | "activity" | "pullRequest">;
```

and change `singletonSurface`'s parameter to `Exclude<GenericRightPanelKind, "preview">`.

Add the actions to `RightPanelStoreState`:

```ts
  openPullRequest: (
    ref: ScopedThreadRef,
    request: { number: number; provider: SourceControlProviderKind },
    tab?: PullRequestsDetailTab,
  ) => void;
  setPullRequestTab: (ref: ScopedThreadRef, number: number, tab: PullRequestsDetailTab) => void;
```

Add the helpers after `terminalSurface`:

```ts
const PULL_REQUEST_DETAIL_TABS: ReadonlyArray<PullRequestsDetailTab> = [
  "conversation",
  "commits",
  "checks",
  "files",
];
const isSourceControlProviderKind = Schema.is(SourceControlProviderKind);

export function pullRequestSurfaceId(number: number): `pullRequest:${number}` {
  return `pullRequest:${number}`;
}

function normalizePullRequestSurface(
  value: Record<string, unknown>,
): PullRequestRightPanelSurface | null {
  const { number, provider, tab } = value;
  if (typeof number !== "number" || !Number.isSafeInteger(number) || number < 1) return null;
  if (value.id !== pullRequestSurfaceId(number) || !isSourceControlProviderKind(provider)) {
    return null;
  }
  return {
    id: pullRequestSurfaceId(number),
    kind: "pullRequest",
    number,
    provider,
    tab: PULL_REQUEST_DETAIL_TABS.includes(tab as PullRequestsDetailTab)
      ? (tab as PullRequestsDetailTab)
      : "conversation",
  };
}
```

Import `Schema` from `effect` at the top (`import { Schema } from "effect";`), following the other web modules that decode with `Schema.is`.

In `normalizePersistedSurface`, add before `default:`:

```ts
    case "pullRequest":
      return normalizePullRequestSurface(value);
```

Add the two actions inside the store, next to `openFile`:

```ts
      openPullRequest: (ref, request, tab) =>
        set((state) => ({
          byThreadKey: updateThread(state.byThreadKey, scopedThreadKey(ref), (current) => {
            const id = pullRequestSurfaceId(request.number);
            const existing = current.surfaces.find((surface) => surface.id === id);
            if (existing) {
              return {
                isOpen: true,
                activeSurfaceId: id,
                surfaces:
                  tab === undefined
                    ? current.surfaces
                    : current.surfaces.map((surface) =>
                        surface.id === id ? { ...surface, tab } : surface,
                      ),
              };
            }
            return upsertSurface(current, {
              id,
              kind: "pullRequest",
              number: request.number,
              provider: request.provider,
              tab: tab ?? "conversation",
            });
          }),
        })),
      setPullRequestTab: (ref, number, tab) =>
        set((state) => ({
          byThreadKey: updateThread(state.byThreadKey, scopedThreadKey(ref), (current) => {
            const id = pullRequestSurfaceId(number);
            if (!current.surfaces.some((surface) => surface.id === id)) return current;
            return {
              ...current,
              surfaces: current.surfaces.map((surface) =>
                surface.id === id && surface.kind === "pullRequest" ? { ...surface, tab } : surface,
              ),
            };
          }),
        })),
```

- [ ] **Step 4: Label and icon the tab in `apps/web/src/components/RightPanelTabs.tsx`**

Import `formatChangeRequestNumber` from `@bibcode/shared/sourceControl` and `GitPullRequest` from `lucide-react` (next to the existing `GitPullRequestArrow` import). In `surfaceTitle`, add:

```ts
    case "pullRequest":
      return formatChangeRequestNumber(surface.provider, surface.number);
```

In `SurfaceIcon`, add:

```tsx
    case "pullRequest":
      return <GitPullRequest className="size-3.5 shrink-0" />;
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cd apps/web && node ../../scripts/run-local-vp.mjs test run --project unit src/rightPanelStore.test.ts src/components/RightPanelTabs.test.tsx`
Expected: PASS. Then `cd apps/web && npx tsc --noEmit -p tsconfig.json` — expected 0 errors (TypeScript reports any switch that misses the new kind).

- [ ] **Step 6: Lint, review and commit**

```bash
node scripts/run-local-vp.mjs lint apps/web/src/rightPanelStore.ts apps/web/src/rightPanelStore.test.ts apps/web/src/components/RightPanelTabs.tsx
node scripts/run-local-vp.mjs fmt apps/web/src/rightPanelStore.ts apps/web/src/rightPanelStore.test.ts apps/web/src/components/RightPanelTabs.tsx
codex review -c sandbox_mode="read-only" --uncommitted
git add apps/web/src/rightPanelStore.ts apps/web/src/rightPanelStore.test.ts apps/web/src/components/RightPanelTabs.tsx
git commit -m "feat(web): pull request right-panel surface" -m "<the two trailer lines from Global Constraints>"
```

---

### Task 2: Navigation context for the detail view

**Files:**
- Create: `apps/web/src/components/pullRequests/detail/pullRequestsNavigation.tsx`
- Modify: `apps/web/src/components/pullRequests/detail/PullRequestsDetailView.tsx:17,61,91-107,303-318,337-353`
- Modify: `apps/web/src/components/pullRequests/detail/PullRequestsFiles.tsx:20,49-57,203-205,234-245`
- Modify: `apps/web/src/components/pullRequests/detail/PullRequestsTimelineItem.tsx:218-228`
- Modify: `apps/web/src/components/pullRequests/PullRequestsPanel.tsx:143-152`
- Modify: `apps/web/src/components/pullRequests/detail/testFixtures.ts`
- Test: `apps/web/src/components/pullRequests/detail/pullRequestsNavigation.test.tsx` (new), and the existing `PullRequestsDetailView.test.tsx`, `PullRequestsFiles.test.tsx`, `PullRequestsFiles.integration.test.tsx`, `PullRequestsTimelineItem.test.tsx`, `review/PullRequestsReviewFile.test.tsx`, `PullRequestsConversation.test.tsx`

**Interfaces:**
- Produces:
  ```ts
  export interface PullRequestsNavigation {
    readonly fileAnchor: string | null;
    selectTab(tab: PullRequestsDetailTab): void;
    showFile(path: string): void;
    setFileAnchor(path: string): void;
    openRequest(number: number): void;
    requestGone(): void;
    readonly discardDeleted: (() => void) | null;
    readonly back: (() => void) | null;
  }
  export const PullRequestsNavigationContext: React.Context<PullRequestsNavigation | null>;
  export function usePullRequestsNavigation(): PullRequestsNavigation; // throws without a provider
  export function PullRequestsRouteNavigation(props: {
    projectRef: ScopedProjectRef;
    number: number;
    children: ReactNode;
  }): ReactNode;
  export function pathFromHash(hash: string): string | null;
  // detail/testFixtures.ts
  export function fakeNavigation(overrides?: Partial<PullRequestsNavigation>): PullRequestsNavigation;
  ```

- [ ] **Step 1: Write the failing test for the route adapter**

Create `apps/web/src/components/pullRequests/detail/pullRequestsNavigation.test.tsx`:

```tsx
// @vitest-environment happy-dom
import { act } from "react";
import { createRoot } from "react-dom/client";
import { describe, expect, it, vi } from "vite-plus/test";

const h = vi.hoisted(() => ({ navigate: vi.fn(), hash: "" }));
vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => h.navigate,
  useLocation: () => ({ hash: h.hash }),
}));

import {
  pathFromHash,
  type PullRequestsNavigation,
  PullRequestsRouteNavigation,
  usePullRequestsNavigation,
} from "./pullRequestsNavigation";

const projectRef = { environmentId: "env", projectId: "project" } as never;
const detailRoute = "/project/$environmentId/$projectId/pull-requests/$number";
const params = { environmentId: "env", projectId: "project", number: "14" };

async function captureRouteNavigation(): Promise<PullRequestsNavigation> {
  let captured: PullRequestsNavigation | null = null;
  function Probe() {
    captured = usePullRequestsNavigation();
    return null;
  }
  const root = createRoot(document.createElement("div"));
  await act(async () =>
    root.render(
      <PullRequestsRouteNavigation projectRef={projectRef} number={14}>
        <Probe />
      </PullRequestsRouteNavigation>,
    ),
  );
  return captured!;
}

describe("PullRequestsRouteNavigation", () => {
  it("keeps the full page's URLs for every navigation", async () => {
    h.hash = "#file=src%2Fa%20b.ts";
    const navigation = await captureRouteNavigation();
    expect(navigation.fileAnchor).toBe("src/a b.ts");
    navigation.selectTab("commits");
    navigation.showFile("docs/x.md");
    navigation.setFileAnchor("docs/y.md");
    navigation.openRequest(29);
    navigation.requestGone();
    navigation.back?.();
    expect(h.navigate.mock.calls.map(([call]) => call)).toEqual([
      { to: detailRoute, params, search: { tab: "commits" }, hash: "", replace: true },
      { to: detailRoute, params, search: { tab: "files" }, hash: "file=docs%2Fx.md" },
      { to: detailRoute, params, search: { tab: "files" }, hash: "file=docs%2Fy.md", replace: true },
      { to: detailRoute, params: { ...params, number: "29" }, search: { tab: "conversation" }, hash: "" },
      { to: "/project/$environmentId/$projectId/pull-requests", params: projectRef },
      { to: "/project/$environmentId/$projectId/pull-requests", params: projectRef },
    ]);
  });

  it("reads no anchor from an unrelated or malformed hash", () => {
    expect(pathFromHash("")).toBeNull();
    expect(pathFromHash("#other=1")).toBeNull();
    expect(pathFromHash("#file=%E0%A4%A")).toBeNull();
  });

  it("throws when a consumer has no navigation host", async () => {
    function Orphan() {
      usePullRequestsNavigation();
      return null;
    }
    const root = createRoot(document.createElement("div"));
    const error = vi.spyOn(console, "error").mockImplementation(() => undefined);
    await expect(act(async () => root.render(<Orphan />))).rejects.toThrow(
      "Pull request views need a PullRequestsNavigationContext provider.",
    );
    error.mockRestore();
  });
});
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cd apps/web && node ../../scripts/run-local-vp.mjs test run --project unit src/components/pullRequests/detail/pullRequestsNavigation.test.tsx`
Expected: FAIL — module `./pullRequestsNavigation` not found.

- [ ] **Step 3: Create `apps/web/src/components/pullRequests/detail/pullRequestsNavigation.tsx`**

```tsx
import type { ScopedProjectRef } from "@bibcode/contracts";
import { useLocation, useNavigate } from "@tanstack/react-router";
import { createContext, type ReactNode, useContext, useMemo } from "react";

import type { PullRequestsDetailTab } from "../../../pullRequestsStore";

/** Where a request's detail view lives: the full page (router) or a thread's right panel. */
export interface PullRequestsNavigation {
  /** The file the Changes tab scrolls to, or null. */
  readonly fileAnchor: string | null;
  /** The user picked another detail tab. */
  selectTab(tab: PullRequestsDetailTab): void;
  /** A conversation link to a file: the Changes tab, scrolled to that file. */
  showFile(path: string): void;
  /** The file tree selected a file: replace the anchor in place. */
  setFileAnchor(path: string): void;
  /** An action created another request (a revert request). */
  openRequest(number: number): void;
  /** The request was deleted while its view was shown. */
  requestGone(): void;
  /**
   * The request was deleted, whether or not its view is still shown: a panel closes the
   * deleted request's tab even after the user switched tabs. Null on the full page, which
   * never navigates once the user has left.
   */
  readonly discardDeleted: (() => void) | null;
  /** "Back to …", or null where there is no list to go back to. */
  readonly back: (() => void) | null;
}

export const PullRequestsNavigationContext = createContext<PullRequestsNavigation | null>(null);

export function usePullRequestsNavigation(): PullRequestsNavigation {
  const navigation = useContext(PullRequestsNavigationContext);
  if (navigation === null) {
    throw new Error("Pull request views need a PullRequestsNavigationContext provider.");
  }
  return navigation;
}

export function pathFromHash(hash: string): string | null {
  const value = hash.replace(/^#/, "");
  if (!value.startsWith("file=")) return null;
  try {
    return decodeURIComponent(value.slice(5));
  } catch {
    return null;
  }
}

const DETAIL_ROUTE = "/project/$environmentId/$projectId/pull-requests/$number";
const LIST_ROUTE = "/project/$environmentId/$projectId/pull-requests";

/** The full page: every navigation is the URL it was before the panel existed. */
export function PullRequestsRouteNavigation({
  projectRef,
  number,
  children,
}: {
  projectRef: ScopedProjectRef;
  number: number;
  children: ReactNode;
}) {
  const navigate = useNavigate();
  const { hash } = useLocation();
  const fileAnchor = pathFromHash(hash);
  const navigation = useMemo<PullRequestsNavigation>(() => {
    const params = { ...projectRef, number: String(number) };
    const fileHash = (path: string) => `file=${encodeURIComponent(path)}`;
    const toList = () => void navigate({ to: LIST_ROUTE, params: projectRef });
    return {
      fileAnchor,
      selectTab: (tab) =>
        void navigate({ to: DETAIL_ROUTE, params, search: { tab }, hash: "", replace: true }),
      showFile: (path) =>
        void navigate({ to: DETAIL_ROUTE, params, search: { tab: "files" }, hash: fileHash(path) }),
      setFileAnchor: (path) =>
        void navigate({
          to: DETAIL_ROUTE,
          params,
          search: { tab: "files" },
          hash: fileHash(path),
          replace: true,
        }),
      openRequest: (target) =>
        void navigate({
          to: DETAIL_ROUTE,
          params: { ...projectRef, number: String(target) },
          search: { tab: "conversation" },
          hash: "",
        }),
      requestGone: toList,
      discardDeleted: null,
      back: toList,
    };
  }, [fileAnchor, navigate, number, projectRef]);
  return <PullRequestsNavigationContext value={navigation}>{children}</PullRequestsNavigationContext>;
}
```

- [ ] **Step 4: Run the new test to verify it passes**

Run: `cd apps/web && node ../../scripts/run-local-vp.mjs test run --project unit src/components/pullRequests/detail/pullRequestsNavigation.test.tsx`
Expected: PASS (3 tests).

- [ ] **Step 5: Move the consumers onto the context**

`PullRequestsDetailView.tsx`:
- Remove `import { useNavigate } from "@tanstack/react-router";` and `const navigate = useNavigate();`.
- Add `import { usePullRequestsNavigation } from "./pullRequestsNavigation";` and, where `navigate` was, `const navigation = usePullRequestsNavigation();`.
- Replace `navigateAfterAction` with:

```ts
  const navigateAfterAction = useCallback(
    (target: number | null) => {
      if (target === null) navigation.requestGone();
      else navigation.openRequest(target);
    },
    [navigation],
  );
```

- In `actionRefresh`, add `discardDeleted: navigation.discardDeleted ?? undefined,` next to `navigate: navigateAfterAction,` (and `navigation.discardDeleted` to its dependency list).
- Add an optional prop to `PullRequestsDetailViewProps`:

```ts
  /** Extra actions in the top row, given the loaded detail (the panel's "Open full page"). */
  headerActions?: ((detail: PullRequestsDetail) => ReactNode) | undefined;
```

  (import `type ReactNode` from `react`; destructure `headerActions` in the component).
- Replace the top-row block (`<div className="shrink-0 px-4 pt-2"> … Back to … </div>`) with:

```tsx
        {navigation.back !== null || (headerActions && detail) ? (
          <div className="flex shrink-0 items-center justify-between gap-2 px-4 pt-2">
            {navigation.back !== null ? (
              <Button size="sm" variant="link" onClick={navigation.back}>
                Back to {presentation.pluralLongName}
              </Button>
            ) : (
              <span />
            )}
            {headerActions && detail ? headerActions(detail) : null}
          </div>
        ) : null}
```

  `detail` is the value the component already derives before `PullRequestsQueryState`; if it is declared later, move that declaration above this block.
- Replace the `Tabs` `onValueChange` body's `void navigate({ … })` with `navigation.selectTab(value);`.

`usePullRequestsAction.ts`:
- Add to `PullRequestsRefreshHandles`:

```ts
  /** Called on delete even after the view unmounted (the panel closes the deleted tab). */
  discardDeleted?: (() => void) | undefined;
```

- Change `REFRESH_AFTER_ACTION`'s key type to `Exclude<keyof PullRequestsRefreshHandles, "navigate" | "discardDeleted">`.
- In the `deleted` branch, call it before the mounted check:

```ts
        if (result.value.kind === "deleted") {
          toastManager.add({ type: "success", title: "Request deleted" });
          refresh.discardDeleted?.();
          if (mounted.current) refresh.navigate(null);
        }
```

- Add to `usePullRequestsAction.test.tsx`, after "does not redirect another view after late $kind completion":

```tsx
  it("lets the panel discard a request deleted after its view unmounted", async () => {
    const discardDeleted = vi.fn();
    await act(async () =>
      root.render(
        <PullRequestsActionProvider
          scope={scope}
          number={14}
          requestKind="pull request"
          refresh={{ ...refresh, discardDeleted }}
          disabledReason={null}
        >
          <Probe />
        </PullRequestsActionProvider>,
      ),
    );
    let finish!: (result: unknown) => void;
    h.command.mockReturnValueOnce(
      new Promise((resolve) => {
        finish = resolve;
      }),
    );
    let current!: Promise<unknown>;
    act(() => {
      current = actions.run({ action: "delete" });
    });
    await act(async () => root.unmount());
    await act(async () => {
      finish({ _tag: "Success", value: { kind: "deleted" } });
      await current;
    });
    expect(discardDeleted).toHaveBeenCalledOnce();
    expect(refresh.navigate).not.toHaveBeenCalled();
  });
```

  (match the provider props the file's existing `render`/`beforeEach` uses — `scope`, `number`, `requestKind`, `disabledReason` — and re-create `root` if the test's `afterEach` expects to unmount it).

`PullRequestsFiles.tsx`:
- Remove `useLocation`/`useNavigate` from the router import (drop the import if nothing else remains) and delete the local `pathFromHash`.
- Add `import { usePullRequestsNavigation } from "./pullRequestsNavigation";`, then replace `const navigate = useNavigate(); const { hash } = useLocation(); const anchorPath = pathFromHash(hash);` with:

```ts
  const navigation = usePullRequestsNavigation();
  const anchorPath = navigation.fileAnchor;
```

- In `selectFile`, replace the `void navigate({ … })` call with `navigation.setFileAnchor(path);` and its dependency list with `[navigation, scrollToFile]`.

`PullRequestsTimelineItem.tsx`: add a small component above the item component and use it in place of the `<Link …>` element (same children):

```tsx
/** The conversation's link to a file opens the Changes tab at that file, wherever the view is. */
function PullRequestsFileLink({ path, children }: { path: string; children: ReactNode }) {
  const navigation = usePullRequestsNavigation();
  return (
    <button
      type="button"
      className="break-all text-left font-mono text-primary underline underline-offset-4"
      onClick={() => navigation.showFile(path)}
    >
      {children}
    </button>
  );
}
```

```tsx
        <PullRequestsFileLink path={item.path}>
          {item.path}
          {item.line === null
            ? ""
            : `:${item.startLine !== null && item.startLine !== item.line ? `${item.startLine}–` : ""}${item.line}`}
        </PullRequestsFileLink>
```

  Remove the `Link` import if unused; import `type ReactNode` and `usePullRequestsNavigation`.

`PullRequestsPanel.tsx`: wrap the detail view the full page renders:

```tsx
        <PullRequestsRouteNavigation projectRef={projectRef} number={number}>
          <PullRequestsDetailView
            key={number}
            scope={scope}
            projectRef={projectRef}
            context={context}
            number={number}
            tab={tab}
            mutationsDisabledReason={mutationsDisabledReason}
          />
        </PullRequestsRouteNavigation>
```

  (import `PullRequestsRouteNavigation` from `./detail/pullRequestsNavigation`).

`detail/testFixtures.ts`: add

```ts
import { vi } from "vite-plus/test";
import type { PullRequestsNavigation } from "./pullRequestsNavigation";

export function fakeNavigation(overrides: Partial<PullRequestsNavigation> = {}): PullRequestsNavigation {
  return {
    fileAnchor: null,
    selectTab: vi.fn(),
    showFile: vi.fn(),
    setFileAnchor: vi.fn(),
    openRequest: vi.fn(),
    requestGone: vi.fn(),
    discardDeleted: null,
    back: null,
    ...overrides,
  };
}
```

- [ ] **Step 6: Give the existing tests a navigation host**

- `PullRequestsPanel.test.tsx`: the full page now renders `PullRequestsRouteNavigation`, so extend its `@tanstack/react-router` mock (which only has `Link`) with `useNavigate: () => vi.fn()` and `useLocation: () => ({ hash: "" })`.
- `PullRequestsDetailView.test.tsx`: in `render(…)`, wrap `<PullRequestsDetailView …/>` in `<PullRequestsRouteNavigation projectRef={projectRef} number={number}>`. Its router mock already provides `useNavigate`/`useLocation`, so every existing `h.navigate` assertion stays as is.
- `PullRequestsFiles.test.tsx`, `PullRequestsFiles.integration.test.tsx`, `review/PullRequestsReviewFile.test.tsx`: wrap each rendered `<PullRequestsFiles …/>` in `<PullRequestsRouteNavigation projectRef={projectRef} number={<the number the test passes>}>`; their router mocks stay.
- `PullRequestsTimelineItem.test.tsx`: drop the `@tanstack/react-router` mock; wrap rendered items in `<PullRequestsNavigationContext value={navigation}>` with `const navigation = fakeNavigation();`. Replace the assertion that the markup contains `?tab=files#file=src%2Fa%20b%23%C3%A9.ts` with a mounted click:

```tsx
    const showFile = vi.fn();
    await mount(
      <PullRequestsNavigationContext value={fakeNavigation({ showFile })}>
        <PullRequestsTimelineItem {...props} item={item} />
      </PullRequestsNavigationContext>,
    );
    await click("src/a b#é.ts");
    expect(showFile).toHaveBeenCalledWith("src/a b#é.ts");
```

  (use the item/fixture the replaced assertion used; `mount` and `click` come from `../review/testHelpers`).
- `PullRequestsConversation.test.tsx`: wrap renders that include thread items with a path in `<PullRequestsNavigationContext value={fakeNavigation()}>`.

- [ ] **Step 7: Run the pull request tests**

Run: `cd apps/web && node ../../scripts/run-local-vp.mjs test run --project unit src/components/pullRequests`
Expected: PASS, with the same test count as before plus the 3 new navigation tests.

- [ ] **Step 8: Lint, review and commit**

```bash
node scripts/run-local-vp.mjs lint apps/web/src/components/pullRequests
node scripts/run-local-vp.mjs fmt apps/web/src/components/pullRequests
cd apps/web && npx tsc --noEmit -p tsconfig.json && cd ../..
codex review -c sandbox_mode="read-only" --uncommitted
git add apps/web/src/components/pullRequests
git commit -m "refactor(web): pull request views take their navigation from context" -m "<the two trailer lines from Global Constraints>"
```

---

### Task 3: Share the repository-context loading

**Files:**
- Create: `apps/web/src/components/pullRequests/usePullRequestsContextQuery.ts`
- Modify: `apps/web/src/components/pullRequests/PullRequestsPanel.tsx:196-205,258-292`
- Test: existing `apps/web/src/components/pullRequests/PullRequestsPanel.test.tsx` and `pullRequestsTelemetry.test.tsx` (behaviour unchanged)

**Interfaces:**
- Produces:
  ```ts
  export function usePullRequestsContextQuery(input: {
    environmentId: EnvironmentId;
    cwd: string | null;
    enabled: boolean;
  }): {
    contextAtom: Atom.Atom<AsyncResult.AsyncResult<PullRequestsContext, string>> | null;
    query: EnvironmentQueryView<PullRequestsContext, string>;
    /** A stored context is shown while its live read runs. */
    storedContext: boolean;
    /** Rescan: bypass the server's context caches and read again (the Rescan button). */
    rescan: () => void;
    /** Auth recovery: bypass the caches and revalidate, keeping a latched cut-off. */
    recover: () => void;
  };
  ```
  (Use the exact atom and query view types `pullRequestsEnvironment.getContext` and `useEnvironmentQuery` return; the literal generic arguments above are illustrative — copy them from the current `contextAtom`/`query` declarations in `PullRequestsPanel.tsx`.)

- [ ] **Step 1: Extract the hook**

Create `usePullRequestsContextQuery.ts` holding, unchanged, the code that `PullRequestsPanel` runs between `contextAtom` and `recoverContext` that concerns the context: the `contextAtom` memo (with `enabled` replacing `ready && catalogProjectId !== null`), `const query = useEnvironmentQuery(contextAtom)`, `storedContext`, `storedAnswer`, `contextReading`, the `readContextLive` effect event and its effect, `requestContextRescan`, `rescanContext` (renamed `rescan`) and `recoverContext` (renamed `recover`). Return `{ contextAtom, query, storedContext, rescan, recover }`.

- [ ] **Step 2: Use it in `PullRequestsPanel`**

Replace the moved code with:

```ts
  const {
    contextAtom,
    query,
    storedContext,
    rescan: rescanContext,
    recover: recoverContext,
  } = usePullRequestsContextQuery({
    environmentId,
    cwd,
    enabled: ready && catalogProjectId !== null,
  });
```

Keep `contextPending`, the prefetch logic and `const rescan = () => { rescanContext(); catalog.refresh(); };` as they are.

- [ ] **Step 3: Run the pull request tests**

Run: `cd apps/web && node ../../scripts/run-local-vp.mjs test run --project unit src/components/pullRequests`
Expected: PASS, same count as after Task 2.

- [ ] **Step 4: Lint, review and commit**

```bash
node scripts/run-local-vp.mjs lint apps/web/src/components/pullRequests/PullRequestsPanel.tsx apps/web/src/components/pullRequests/usePullRequestsContextQuery.ts
node scripts/run-local-vp.mjs fmt apps/web/src/components/pullRequests/PullRequestsPanel.tsx apps/web/src/components/pullRequests/usePullRequestsContextQuery.ts
codex review -c sandbox_mode="read-only" --uncommitted
git add apps/web/src/components/pullRequests/PullRequestsPanel.tsx apps/web/src/components/pullRequests/usePullRequestsContextQuery.ts
git commit -m "refactor(web): share pull request context loading" -m "<the two trailer lines from Global Constraints>"
```

---

### Task 4: Detail layout follows its own width

**Files:**
- Modify: `apps/web/src/components/pullRequests/detail/PullRequestsDetailView.tsx` (root `div` and the side column `div` at the old line 410)
- Modify: `apps/web/src/components/pullRequests/detail/PullRequestsConversation.tsx:135`
- Test: `apps/web/src/components/pullRequests/detail/PullRequestsDetailView.test.tsx`

- [ ] **Step 1: Write the failing test**

Add to `PullRequestsDetailView.test.tsx`:

```tsx
  it("lays out the side column by its own width, not the window's", async () => {
    await render();
    const root = container.querySelector('[data-text-surface="background"]')!;
    expect(root.className).toContain("@container/pull-request");
    const aside = container.querySelector('[data-pull-request-side-column="wide"]')!;
    expect(aside.className).toContain("@3xl/pull-request:block");
    expect(aside.className).not.toContain("lg:block");
  });
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cd apps/web && node ../../scripts/run-local-vp.mjs test run --project unit src/components/pullRequests/detail/PullRequestsDetailView.test.tsx -t "own width"`
Expected: FAIL — no `@container/pull-request` class.

- [ ] **Step 3: Switch the breakpoints**

In `PullRequestsDetailView.tsx`, the root element becomes
`<div className="@container/pull-request flex min-h-0 min-w-0 flex-1 flex-col" data-text-surface="background">`
and the wide side column becomes
`<div data-pull-request-side-column="wide" className="hidden w-64 shrink-0 overflow-auto border-l border-border @3xl/pull-request:block">`.

In `PullRequestsConversation.tsx`, `<div className="lg:hidden">` becomes `<div className="@3xl/pull-request:hidden">`.

- [ ] **Step 4: Run the tests**

Run: `cd apps/web && node ../../scripts/run-local-vp.mjs test run --project unit src/components/pullRequests/detail`
Expected: PASS.

- [ ] **Step 5: Lint, review and commit**

```bash
node scripts/run-local-vp.mjs lint apps/web/src/components/pullRequests/detail
node scripts/run-local-vp.mjs fmt apps/web/src/components/pullRequests/detail
codex review -c sandbox_mode="read-only" --uncommitted
git add apps/web/src/components/pullRequests/detail
git commit -m "fix(web): pull request detail lays out by its own width" -m "<the two trailer lines from Global Constraints>"
```

---

### Task 5: The panel host and its wiring in ChatView

**Files:**
- Create: `apps/web/src/components/pullRequests/PullRequestPanelSurface.tsx`
- Modify: `apps/web/src/components/ChatView.tsx` (the `rightPanelContent` chain around line 6692)
- Test: `apps/web/src/components/pullRequests/PullRequestPanelSurface.test.tsx` (new)

**Interfaces:**
- Consumes: `useRightPanelStore().openPullRequest / setPullRequestTab / closeSurface`, `PullRequestRightPanelSurface`, `pullRequestSurfaceId` (Task 1); `PullRequestsNavigationContext`, `PullRequestsNavigation` (Task 2); `usePullRequestsContextQuery` (Task 3); `PullRequestsDetailView` with `headerActions` (Task 2).
- Produces:
  ```ts
  export function PullRequestPanelSurface(props: {
    threadRef: ScopedThreadRef;
    projectRef: ScopedProjectRef | null;
    gitCwd: string | null;
    workspaceUnavailable: string | null;
    surface: PullRequestRightPanelSurface;
  }): ReactNode;
  ```

- [ ] **Step 1: Write the failing tests**

Create `apps/web/src/components/pullRequests/PullRequestPanelSurface.test.tsx`. It mocks the same modules `PullRequestsPanel.test.tsx` mocks for availability and context (`../../hooks/useSettings`, `../../state/entities`, `../../state/environments`, `../../state/query`, `../../state/pullRequests`), mocks `./detail/PullRequestsDetailView` to capture props and the navigation it reads, and uses the real `~/rightPanelStore`:

```tsx
// @vitest-environment happy-dom
import { scopeThreadRef } from "@bibcode/client-runtime/environment";
import { act, useContext, useEffect } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const h = vi.hoisted(() => ({
  enabled: true,
  connected: true,
  context: null as unknown,
  contextError: null as string | null,
  detailProps: null as Record<string, unknown> | null,
  detailMounts: 0,
  navigation: null as unknown,
  navigate: vi.fn(),
}));

vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => h.navigate,
  Link: ({ children }: { children: React.ReactNode }) => <a>{children}</a>,
}));
// Availability and context: mirror PullRequestsPanel.test.tsx's mocks for these modules,
// driven by h.enabled, h.connected, h.context and h.contextError.
vi.mock("./detail/PullRequestsDetailView", async () => {
  const { PullRequestsNavigationContext } = await import("./detail/pullRequestsNavigation");
  return {
    PullRequestsDetailView: (props: Record<string, unknown>) => {
      h.detailProps = props;
      h.navigation = useContext(PullRequestsNavigationContext);
      useEffect(() => {
        h.detailMounts += 1;
      }, []);
      return null;
    },
  };
});

import { projectKey } from "@bibcode/client-runtime/state/entities";
import { usePullRequestsStore } from "../../pullRequestsStore";
import { useRightPanelStore, selectThreadRightPanelState } from "../../rightPanelStore";
import type { PullRequestsNavigation } from "./detail/pullRequestsNavigation";
import { PullRequestPanelSurface } from "./PullRequestPanelSurface";

const threadRef = scopeThreadRef("env" as never, "thread-1" as never);
const projectRef = { environmentId: "env", projectId: "project" } as never;
const surface = {
  id: "pullRequest:89",
  kind: "pullRequest",
  number: 89,
  provider: "gitlab",
  tab: "commits",
} as const;
let root: Root;
let container: HTMLDivElement;

async function render(overrides: Partial<Parameters<typeof PullRequestPanelSurface>[0]> = {}) {
  await act(async () =>
    root.render(
      <PullRequestPanelSurface
        threadRef={threadRef}
        projectRef={projectRef}
        gitCwd="/repo/worktree"
        workspaceUnavailable={null}
        surface={surface}
        {...overrides}
      />,
    ),
  );
}

beforeEach(() => {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  useRightPanelStore.setState({ byThreadKey: {} });
  useRightPanelStore.getState().openPullRequest(threadRef, { number: 89, provider: "gitlab" }, "commits");
  h.detailProps = null;
  h.navigation = null;
  h.navigate.mockReset();
  h.enabled = true;
  h.connected = true;
  h.contextError = null;
  h.context = {
    status: "available",
    provider: "gitlab",
    host: "gitlab.example.test",
    repository: "team/repo",
    webUrl: "https://gitlab.example.test/team/repo",
    account: { login: "alice" },
  };
});
afterEach(() => {
  act(() => root.unmount());
  container.remove();
});

describe("PullRequestPanelSurface", () => {
  it("renders the request in the thread's checkout on the surface's tab", async () => {
    await render();
    expect(h.detailProps).toMatchObject({
      scope: { environmentId: "env", cwd: "/repo/worktree" },
      number: 89,
      tab: "commits",
    });
    expect((h.navigation as PullRequestsNavigation).back).toBeNull();
  });

  it("keeps tab changes, revert requests and deletes inside the thread's panel", async () => {
    await render();
    const navigation = h.navigation as PullRequestsNavigation;
    await act(async () => navigation.selectTab("files"));
    await act(async () => navigation.openRequest(90));
    let state = selectThreadRightPanelState(useRightPanelStore.getState().byThreadKey, threadRef);
    expect(state.surfaces.map((entry) => entry.id)).toEqual(["pullRequest:89", "pullRequest:90"]);
    expect(state.surfaces[0]).toMatchObject({ tab: "files" });
    expect(state.activeSurfaceId).toBe("pullRequest:90");
    await act(async () => navigation.requestGone());
    state = selectThreadRightPanelState(useRightPanelStore.getState().byThreadKey, threadRef);
    expect(state.surfaces.map((entry) => entry.id)).toEqual(["pullRequest:90"]);
  });

  it("closes a deleted request's tab even after the panel moved to another tab", async () => {
    await render();
    const discardDeleted = (h.navigation as PullRequestsNavigation).discardDeleted!;
    useRightPanelStore.getState().openPullRequest(threadRef, { number: 90, provider: "gitlab" });
    await act(async () => root.render(null));
    discardDeleted();
    expect(
      selectThreadRightPanelState(useRightPanelStore.getState().byThreadKey, threadRef).surfaces.map(
        (entry) => entry.id,
      ),
    ).toEqual(["pullRequest:90"]);
  });

  it("shows a conversation file link on the Changes tab at that file", async () => {
    await render();
    await act(async () => (h.navigation as PullRequestsNavigation).showFile("src/a.ts"));
    expect((h.navigation as PullRequestsNavigation).fileAnchor).toBe("src/a.ts");
    expect(
      selectThreadRightPanelState(useRightPanelStore.getState().byThreadKey, threadRef).surfaces[0],
    ).toMatchObject({ tab: "files" });
  });

  it("explains instead of loading when the workspace is unavailable", async () => {
    await render({ workspaceUnavailable: "The worktree was removed." });
    expect(container.textContent).toContain("The worktree was removed.");
    expect(h.detailProps).toBeNull();
  });

  it("explains when pull requests are turned off in settings", async () => {
    h.enabled = false;
    await render();
    expect(container.textContent).toContain("Open Source Control settings");
    expect(h.detailProps).toBeNull();
  });

  it("explains when the repository has no supported host", async () => {
    h.context = { status: "unavailable", message: "No GitHub or GitLab remote.", authCommand: null, installHint: null };
    await render();
    expect(container.textContent).toContain("No GitHub or GitLab remote.");
  });

  it("remounts the detail when the checkout's repository or account changes", async () => {
    await render();
    const first = h.detailMounts;
    h.context = { ...(h.context as object), repository: "team/other" };
    await render();
    expect(h.detailMounts).toBe(first + 1);
  });

  it("opens the full page on the surface's tab and the thread's checkout", async () => {
    usePullRequestsStore.getState().setCheckoutCwd(projectRef, "/repo/other-worktree");
    await render();
    const headerActions = h.detailProps!.headerActions as (detail: { number: number }) => React.ReactElement;
    const host = document.createElement("div");
    const actionsRoot = createRoot(host);
    await act(async () => actionsRoot.render(headerActions({ number: 89 })));
    await act(async () => host.querySelector("button")!.click());
    expect(h.navigate).toHaveBeenCalledWith({
      to: "/project/$environmentId/$projectId/pull-requests/$number",
      params: { environmentId: "env", projectId: "project", number: "89" },
      search: { tab: "commits" },
    });
    expect(
      usePullRequestsStore.getState().byProjectKey[projectKey(projectRef)]?.checkoutCwd,
    ).toBe("/repo/worktree");
    act(() => actionsRoot.unmount());
  });
});
```

Copy the availability/context `vi.mock` blocks from `PullRequestsPanel.test.tsx` (the `useSettings`, `state/entities`, `state/environments`, `state/query` and `state/pullRequests` mocks) and drive them from `h.enabled`, `h.connected`, `h.context` and `h.contextError`.

- [ ] **Step 2: Run them to verify they fail**

Run: `cd apps/web && node ../../scripts/run-local-vp.mjs test run --project unit src/components/pullRequests/PullRequestPanelSurface.test.tsx`
Expected: FAIL — module `./PullRequestPanelSurface` not found.

- [ ] **Step 3: Implement `PullRequestPanelSurface.tsx`**

```tsx
import type { ScopedProjectRef, ScopedThreadRef } from "@bibcode/contracts";
import { useNavigate } from "@tanstack/react-router";
import { type ReactNode, useMemo, useState } from "react";

import { usePrimarySettings } from "../../hooks/useSettings";
import { usePullRequestsStore } from "../../pullRequestsStore";
import { type PullRequestRightPanelSurface, useRightPanelStore } from "../../rightPanelStore";
import { useProject, useServerConfigs } from "../../state/entities";
import { useEnvironmentConnectionState } from "../../state/environments";
import { Button } from "../ui/button";
import { Skeleton } from "../ui/skeleton";
import { PullRequestsDetailView } from "./detail/PullRequestsDetailView";
import {
  type PullRequestsNavigation,
  PullRequestsNavigationContext,
} from "./detail/pullRequestsNavigation";
import {
  PULL_REQUESTS_DISABLED_IN_SETTINGS_MESSAGE,
  resolvePullRequestsAvailability,
  resolvePullRequestsMutationsDisabledReason,
} from "./pullRequestsAvailability";
import { PullRequestsContextRefresh } from "./pullRequestsContextRefresh";
import { PullRequestsUnavailableState } from "./PullRequestsUnavailableState";
import { usePullRequestsContextQuery } from "./usePullRequestsContextQuery";

export interface PullRequestPanelSurfaceProps {
  threadRef: ScopedThreadRef;
  projectRef: ScopedProjectRef | null;
  gitCwd: string | null;
  workspaceUnavailable: string | null;
  surface: PullRequestRightPanelSurface;
}

/** A request's detail as a thread's right-panel tab, against the thread's checkout. */
export function PullRequestPanelSurface({
  threadRef,
  projectRef,
  gitCwd,
  workspaceUnavailable,
  surface,
}: PullRequestPanelSurfaceProps) {
  const navigate = useNavigate();
  const { environmentId } = threadRef;
  const project = useProject(projectRef);
  const connection = useEnvironmentConnectionState(environmentId);
  const serverConfig = useServerConfigs().get(environmentId) ?? null;
  const enabled = usePrimarySettings((settings) => settings.pullRequestsEnabled);
  const availability = resolvePullRequestsAvailability(connection.data, serverConfig, enabled);
  const usable =
    availability.kind === "ready" && project !== null && gitCwd !== null && workspaceUnavailable === null;
  const { query, storedContext, rescan, recover } = usePullRequestsContextQuery({
    environmentId,
    cwd: gitCwd,
    enabled: usable,
  });
  const [fileAnchor, setFileAnchor] = useState<string | null>(null);
  const { number, provider, id } = surface;
  const navigation = useMemo<PullRequestsNavigation>(() => {
    const store = () => useRightPanelStore.getState();
    return {
      fileAnchor,
      selectTab: (tab) => store().setPullRequestTab(threadRef, number, tab),
      showFile: (path) => {
        setFileAnchor(path);
        store().setPullRequestTab(threadRef, number, "files");
      },
      setFileAnchor,
      openRequest: (target) => store().openPullRequest(threadRef, { number: target, provider }),
      requestGone: () => store().closeSurface(threadRef, id),
      discardDeleted: () => store().closeSurface(threadRef, id),
      back: null,
    };
  }, [fileAnchor, id, number, provider, threadRef]);
  const scope = useMemo(() => ({ environmentId, cwd: gitCwd ?? "" }), [environmentId, gitCwd]);

  if (availability.kind !== "ready") {
    return (
      <PullRequestsUnavailableState
        reason={
          availability.kind === "disabled_in_settings"
            ? PULL_REQUESTS_DISABLED_IN_SETTINGS_MESSAGE
            : availability.kind === "unsupported"
              ? "This environment does not support Pull Requests."
              : availability.reason
        }
        disabledInSettings={availability.kind === "disabled_in_settings"}
      />
    );
  }
  if (workspaceUnavailable !== null) return <PullRequestsUnavailableState reason={workspaceUnavailable} />;
  if (project === null || projectRef === null || gitCwd === null) {
    return <PullRequestsUnavailableState reason="Waiting for project data." />;
  }
  if ((query.error !== null && !storedContext) || query.data?.status === "unavailable") {
    const unavailable = query.data?.status === "unavailable" ? query.data : null;
    return (
      <PullRequestsUnavailableState
        reason={query.error ?? unavailable!.message}
        authCommand={unavailable?.authCommand ?? null}
        installHint={unavailable?.installHint ?? null}
        onRescan={rescan}
        isPending={query.isPending}
      />
    );
  }
  if (query.data === null) {
    return (
      <div role="status" className="space-y-3 p-4">
        <span className="sr-only">
          Loading {resolveChangeRequestPresentationForKind(provider).longName}…
        </span>
        <Skeleton className="h-8 w-60" />
        <Skeleton className="h-14 w-full" />
      </div>
    );
  }
  const openFullPage = () => {
    // The full page reads its checkout from the project's saved choice: carry the thread's.
    usePullRequestsStore.getState().setCheckoutCwd(projectRef, gitCwd);
    void navigate({
      to: "/project/$environmentId/$projectId/pull-requests/$number",
      params: { ...projectRef, number: String(number) },
      search: { tab: surface.tab },
    });
  };
  return (
    <PullRequestsContextRefresh value={recover}>
      <PullRequestsNavigationContext value={navigation}>
        <PullRequestsDetailView
          // A different repository or account at the same checkout path remounts the
          // detail, as on the full page: its queries are keyed by checkout and number.
          key={JSON.stringify([
            gitCwd,
            number,
            query.data.provider,
            query.data.host,
            query.data.repository,
            query.data.account.login,
          ])}
          scope={scope}
          projectRef={projectRef}
          context={query.data}
          number={number}
          tab={surface.tab}
          mutationsDisabledReason={resolvePullRequestsMutationsDisabledReason(serverConfig)}
          headerActions={(): ReactNode => (
            <Button size="sm" variant="link" onClick={openFullPage}>
              Open full page
            </Button>
          )}
        />
      </PullRequestsNavigationContext>
    </PullRequestsContextRefresh>
  );
}
```

Add `import { resolveChangeRequestPresentationForKind } from "@bibcode/shared/sourceControl";` at the top (it gives "merge request" on GitLab, "pull request" on GitHub). `provider` comes from the `surface` destructuring above the early returns.

- [ ] **Step 4: Render it from ChatView**

In `apps/web/src/components/ChatView.tsx`, add a lazy import next to the other panel lazies:

```ts
const PullRequestPanelSurface = lazy(() =>
  import("./pullRequests/PullRequestPanelSurface").then((module) => ({
    default: module.PullRequestPanelSurface,
  })),
);
```

and a branch in `rightPanelContent` right after the `sourceControl` branch:

```tsx
    ) : activeRightPanelSurface?.kind === "pullRequest" ? (
      <Suspense fallback={null}>
        <PullRequestPanelSurface
          key={`${scopedThreadKey(activeThreadRef)}:${activeRightPanelSurface.id}`}
          threadRef={activeThreadRef}
          projectRef={activeProjectRef}
          gitCwd={gitCwd}
          workspaceUnavailable={workspaceUnavailable}
          surface={activeRightPanelSurface}
        />
      </Suspense>
```

- [ ] **Step 5: Run the tests**

Run: `cd apps/web && node ../../scripts/run-local-vp.mjs test run --project unit src/components/pullRequests/PullRequestPanelSurface.test.tsx src/components/ChatView.test.tsx src/components/ChatView.hooks.test.tsx`
Expected: PASS.

- [ ] **Step 6: Lint, review and commit**

```bash
node scripts/run-local-vp.mjs lint apps/web/src/components/pullRequests apps/web/src/components/ChatView.tsx
node scripts/run-local-vp.mjs fmt apps/web/src/components/pullRequests/PullRequestPanelSurface.tsx apps/web/src/components/pullRequests/PullRequestPanelSurface.test.tsx apps/web/src/components/ChatView.tsx
cd apps/web && npx tsc --noEmit -p tsconfig.json && cd ../..
codex review -c sandbox_mode="read-only" --uncommitted
git add apps/web/src/components/pullRequests/PullRequestPanelSurface.tsx apps/web/src/components/pullRequests/PullRequestPanelSurface.test.tsx apps/web/src/components/ChatView.tsx
git commit -m "feat(web): show a pull or merge request in a thread's right panel" -m "<the two trailer lines from Global Constraints>"
```

---

### Task 6: Done opens the request from Source Control

**Files:**
- Modify: `apps/web/src/components/gitManager/provider/GitManagerCreatePullRequestDialog.tsx:97-113,645-649`
- Modify: `apps/web/src/components/SourceControlPanel.tsx:1385-1400`
- Test: `apps/web/src/components/gitManager/provider/GitManagerCreatePullRequestDialog.test.tsx`, `apps/web/src/components/SourceControlPanel.test.tsx`

**Interfaces:**
- Consumes: `openPullRequest(ref, { number, provider })` (Task 1).
- Produces: `GitManagerCreatePullRequestDialogProps.onDone?: (request: { number: number; provider: SourceControlProviderKind }) => void`.

- [ ] **Step 1: Write the failing dialog tests**

Add to `GitManagerCreatePullRequestDialog.test.tsx`, next to "locks every field once the merge request is created and offers only Done":

```tsx
  it("hands the created request to onDone when Done is pressed", async () => {
    h.status = status({
      sourceControlProvider: { kind: "gitlab", name: "GitLab", baseUrl: "https://gitlab.example.test" },
    });
    const onDone = vi.fn();
    const { onOpenChange } = await renderDialog(vi.fn(), vi.fn(), null, { onDone });
    await chooseTarget("release/next");
    h.script = [{ outcome: "success", events: [finished("created")] }];
    await act(async () => button("Publish and create merge request").click());
    expect(onDone).not.toHaveBeenCalled();
    await act(async () => button("Done").click());
    expect(onDone).toHaveBeenCalledWith({ number: 7, provider: "gitlab" });
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });

  it("only closes on Done when the host returned no request number", async () => {
    h.status = status({
      sourceControlProvider: { kind: "gitlab", name: "GitLab", baseUrl: "https://gitlab.example.test" },
    });
    const onDone = vi.fn();
    const { onOpenChange } = await renderDialog(vi.fn(), vi.fn(), null, { onDone });
    await chooseTarget("release/next");
    h.script = [{ outcome: "success", events: [finished("created", { number: undefined })] }];
    await act(async () => button("Publish and create merge request").click());
    await act(async () => button("Done").click());
    expect(onDone).not.toHaveBeenCalled();
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });
```

If the test file's `finished(...)` helper does not take overrides, extend it with an optional second parameter that is spread into the result's `pr` object, so the second test can produce a result without `number`.

- [ ] **Step 2: Run them to verify they fail**

Run: `cd apps/web && node ../../scripts/run-local-vp.mjs test run --project unit src/components/gitManager/provider/GitManagerCreatePullRequestDialog.test.tsx -t "Done"`
Expected: FAIL — `onDone` never called.

- [ ] **Step 3: Implement `onDone` in the dialog**

Add to `GitManagerCreatePullRequestDialogProps`:

```ts
  /** Done after a request was created or found; the Source Control panel opens it beside the thread. */
  readonly onDone?: (request: { number: number; provider: SourceControlProviderKind }) => void;
```

(import `type SourceControlProviderKind` from `@bibcode/contracts`; destructure `onDone`).

Replace the Done button:

```tsx
            <Button
              onClick={() => {
                if (settled && progress.number !== null && provider !== null)
                  onDone?.({ number: progress.number, provider: provider.kind });
                onOpenChange(false);
              }}
            >
              Done
            </Button>
```

`provider` is the dialog's existing `const provider = review === null ? hintedProvider(providerHint) : review.provider;` (a `SourceControlProviderInfo | null`); `settled` narrows `progress` to the `created`/`existing` members that carry `number`.

- [ ] **Step 4: Write the failing Source Control test**

In `SourceControlPanel.test.tsx`, extend the store mock and state:

```ts
// testState:
  openPullRequest: vi.fn(),
// vi.mock("~/rightPanelStore", …):
    getState: () => ({ open: testState.rightPanelOpen, openPullRequest: testState.openPullRequest }),
```

reset it in `beforeEach` with `testState.openPullRequest.mockReset();`, and add after "pushes before opening the merge request review…":

```ts
  it("opens the created merge request beside the thread on Done", async () => {
    testState.statusQuery.data = status({
      refName: "feature/test",
      hasUpstream: false,
      aheadCount: 1,
      sourceControlProvider: { kind: "gitlab", name: "GitLab", baseUrl: "https://gitlab.test" },
    });
    const props = buildProps();
    render(props);
    buttonsByText("Push & create MR")[0]?.onClick?.();
    await flushPromises();
    render(props);
    captured.createPullRequest?.onDone?.({ number: 89, provider: "gitlab" });
    expect(testState.openPullRequest).toHaveBeenCalledWith(props.threadRef, {
      number: 89,
      provider: "gitlab",
    });
  });
```

- [ ] **Step 5: Wire Done in `SourceControlPanel.tsx`**

On the `<GitManagerCreatePullRequestDialog …>` element add:

```tsx
          onDone={(request) => useRightPanelStore.getState().openPullRequest(threadRef, request)}
```

- [ ] **Step 6: Run the tests**

Run: `cd apps/web && node ../../scripts/run-local-vp.mjs test run --project unit src/components/gitManager/provider/GitManagerCreatePullRequestDialog.test.tsx src/components/SourceControlPanel.test.tsx`
Expected: PASS.

- [ ] **Step 7: Lint, review and commit**

```bash
node scripts/run-local-vp.mjs lint apps/web/src/components/gitManager/provider apps/web/src/components/SourceControlPanel.tsx apps/web/src/components/SourceControlPanel.test.tsx
node scripts/run-local-vp.mjs fmt apps/web/src/components/gitManager/provider/GitManagerCreatePullRequestDialog.tsx apps/web/src/components/gitManager/provider/GitManagerCreatePullRequestDialog.test.tsx apps/web/src/components/SourceControlPanel.tsx apps/web/src/components/SourceControlPanel.test.tsx
codex review -c sandbox_mode="read-only" --uncommitted
git add apps/web/src/components/gitManager/provider/GitManagerCreatePullRequestDialog.tsx apps/web/src/components/gitManager/provider/GitManagerCreatePullRequestDialog.test.tsx apps/web/src/components/SourceControlPanel.tsx apps/web/src/components/SourceControlPanel.test.tsx
git commit -m "feat(web): Done opens the created request beside the thread" -m "<the two trailer lines from Global Constraints>"
```

---

### Task 7: "Open {MR/PR} in panel" next to "View"

**Files:**
- Modify: `apps/web/src/components/SourceControlPrimaryAction.logic.ts:62-85,384-391`
- Modify: `apps/web/src/components/GitActionsControl.logic.ts:16-23,94-166`
- Modify: `apps/web/src/components/SourceControlPanel.tsx:346-352,831-851,1195-1199`
- Test: `apps/web/src/components/SourceControlPrimaryAction.logic.test.ts`, `apps/web/src/components/GitActionsControl.logic.test.ts`, `apps/web/src/components/SourceControlPanel.test.tsx`

`GitActionsControl.tsx` is not changed: its only production caller (`ChatHeaderActions`) always passes `hideTrigger`, so its menu is never shown. `buildMenuItems` is also Source Control's legacy-branch menu, which is why it gains the option.

**Interfaces:**
- Consumes: `openPullRequest` (Task 1).
- Produces: Source Control menu item `{ id: "open_pr_panel", kind: "open_pr_panel", label: "Open {MR/PR} in panel" }`; `buildMenuItems(gitStatus, isBusy, hasPrimaryRemote = true, canOpenInPanel = false)` adds `{ id: "pr_panel", kind: "open_pr_panel", icon: "pr", label: "Open {MR/PR} in panel" }`.

- [ ] **Step 1: Write the failing logic tests**

In `SourceControlPrimaryAction.logic.test.ts`, change the expected ids in "swaps the PR slot to a View action when a PR is open" to end with `"open_pr", "open_pr_panel"` and add:

```ts
    assert.deepInclude(menuItem(items, "open_pr_panel"), {
      label: "Open PR in panel",
      disabled: false,
      kind: "open_pr_panel",
      group: "remote",
    });
```

In `GitActionsControl.logic.test.ts`, add inside "when: ref is clean and has an open PR":

```ts
  it("buildMenuItems offers opening the PR in the thread's panel only when asked", () => {
    const withPr = status({
      pr: {
        number: 11,
        title: "Existing PR",
        url: "https://example.com/pr/11",
        baseRef: "main",
        headRef: "feature/test",
        state: "open",
      },
    });
    assert.notInclude(
      buildMenuItems(withPr, false).map((item) => item.id),
      "pr_panel",
    );
    assert.deepInclude(buildMenuItems(withPr, false, true, true).at(-1), {
      id: "pr_panel",
      label: "Open PR in panel",
      disabled: false,
      icon: "pr",
      kind: "open_pr_panel",
    });
    assert.notInclude(
      buildMenuItems(status(), false, true, true).map((item) => item.id),
      "pr_panel",
    );
  });
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cd apps/web && node ../../scripts/run-local-vp.mjs test run --project unit src/components/SourceControlPrimaryAction.logic.test.ts src/components/GitActionsControl.logic.test.ts`
Expected: FAIL — no `open_pr_panel` / `pr_panel` item.

- [ ] **Step 3: Add the items**

`SourceControlPrimaryAction.logic.ts`: add `| "open_pr_panel"` to `SourceControlMenuItemId` and to `SourceControlMenuItem["kind"]`; in `buildSourceControlMenuItems`'s initial `items` array, right after the `open_pr`/`create_pr` entry, add:

```ts
    changeRequestsEnabled && hasOpenPr
      ? {
          id: "open_pr_panel",
          label: `Open ${terminology.shortLabel} in panel`,
          group: "remote",
          kind: "open_pr_panel",
          ...gate(undefined),
        }
      : null,
```

`GitActionsControl.logic.ts`: change `GitActionMenuItem` to `id: "commit" | "push" | "pr" | "pr_panel"` and `kind: "open_dialog" | "open_pr" | "open_pr_panel"`; add the parameter `canOpenInPanel = false` to `buildMenuItems`, and change its final `return [ … ];` to build the array into `const items: GitActionMenuItem[] = [ … ];`, then:

```ts
  if (canOpenInPanel && hasOpenPr) {
    items.push({
      id: "pr_panel",
      label: `Open ${terminology.shortLabel} in panel`,
      disabled: !canOpenPr,
      icon: "pr",
      kind: "open_pr_panel",
    });
  }
  return items;
```

- [ ] **Step 4: Run the logic tests**

Run: `cd apps/web && node ../../scripts/run-local-vp.mjs test run --project unit src/components/SourceControlPrimaryAction.logic.test.ts src/components/GitActionsControl.logic.test.ts`
Expected: PASS.

- [ ] **Step 5: Write the failing Source Control panel test**

Add to `SourceControlPanel.test.tsx`:

```ts
  it("opens the branch's open merge request in the thread's panel from the menu", () => {
    testState.statusQuery.data = status({
      refName: "feature/test",
      sourceControlProvider: { kind: "gitlab", name: "GitLab", baseUrl: "https://gitlab.test" },
      pr: {
        number: 89,
        title: "Fix it",
        url: "https://gitlab.test/team/repo/-/merge_requests/89",
        baseRef: "main",
        headRef: "feature/test",
        state: "open",
      },
    });
    const props = buildProps();
    render(props);
    menuItemsByText("Open MR in panel")[0]?.onClick?.();
    expect(testState.openPullRequest).toHaveBeenCalledWith(props.threadRef, {
      number: 89,
      provider: "gitlab",
    });
  });
```

Use the test file's existing helper for finding rendered menu items (the one the existing "View MR"/menu tests use); if it only has `buttonsByText`, add `menuItemsByText` alongside it with the same shape, matching captured `MenuItem` props by text.

- [ ] **Step 6: Wire the items in the components**

`SourceControlPanel.tsx`:

```ts
  const openPrInPanel = useCallback(() => {
    const pr = status?.pr?.state === "open" ? status.pr : null;
    if (!pr) return;
    useRightPanelStore.getState().openPullRequest(threadRef, {
      number: pr.number,
      provider: status?.sourceControlProvider?.kind ?? "unknown",
    });
  }, [status, threadRef]);
```

- `menuItems` (legacy branch) becomes `buildMenuItems(status, isBusy, hasPrimaryRemote, true)`.
- `onScMenuItem` gains `case "open_pr_panel": openPrInPanel(); return;` (and `openPrInPanel` in its deps).
- The legacy menu's `onClick` gains `if (item.kind === "open_pr_panel") return openPrInPanel();` before the `open_pr` line.

- [ ] **Step 7: Run the tests**

Run: `cd apps/web && node ../../scripts/run-local-vp.mjs test run --project unit src/components/SourceControlPanel.test.tsx src/components/SourceControlPrimaryAction.logic.test.ts src/components/GitActionsControl.logic.test.ts`
Expected: PASS.

- [ ] **Step 8: Lint, review and commit**

```bash
node scripts/run-local-vp.mjs lint apps/web/src/components/SourceControlPrimaryAction.logic.ts apps/web/src/components/GitActionsControl.logic.ts apps/web/src/components/SourceControlPanel.tsx
node scripts/run-local-vp.mjs fmt apps/web/src/components/SourceControlPrimaryAction.logic.ts apps/web/src/components/SourceControlPrimaryAction.logic.test.ts apps/web/src/components/GitActionsControl.logic.ts apps/web/src/components/GitActionsControl.logic.test.ts apps/web/src/components/SourceControlPanel.tsx apps/web/src/components/SourceControlPanel.test.tsx
codex review -c sandbox_mode="read-only" --uncommitted
git add apps/web/src/components/SourceControlPrimaryAction.logic.ts apps/web/src/components/SourceControlPrimaryAction.logic.test.ts apps/web/src/components/GitActionsControl.logic.ts apps/web/src/components/GitActionsControl.logic.test.ts apps/web/src/components/SourceControlPanel.tsx apps/web/src/components/SourceControlPanel.test.tsx
git commit -m "feat(web): open the branch's request in the thread's panel" -m "<the two trailer lines from Global Constraints>"
```

---

### Task 8: "Open beside a thread…" on the full page

**Files:**
- Create: `apps/web/src/components/pullRequests/openBesideThread.logic.ts`
- Create: `apps/web/src/components/pullRequests/OpenBesideThreadButton.tsx`
- Modify: `apps/web/src/components/pullRequests/PullRequestsPanel.tsx` (the wrapped `PullRequestsDetailView` from Task 2)
- Test: `apps/web/src/components/pullRequests/openBesideThread.logic.test.ts`, `apps/web/src/components/pullRequests/OpenBesideThreadButton.test.tsx`

**Interfaces:**
- Consumes: `openPullRequest` (Task 1); `headerActions` (Task 2); `useThreadShellsForProjectRefs` (`apps/web/src/state/entities.ts`).
- Produces:
  ```ts
  export interface BesideThreadCandidate {
    readonly ref: ScopedThreadRef;
    readonly title: string;
    readonly onBranch: boolean;
  }
  export function rankBesideThreadCandidates(
    threads: ReadonlyArray<EnvironmentThreadShell>,
    headBranch: string,
  ): BesideThreadCandidate[];
  export function OpenBesideThreadButton(props: {
    projectRef: ScopedProjectRef;
    number: number;
    provider: SourceControlProviderKind;
    headBranch: string;
  }): ReactNode;
  ```

- [ ] **Step 1: Write the failing logic test**

Create `openBesideThread.logic.test.ts`:

```ts
import type { EnvironmentThreadShell } from "@bibcode/client-runtime/state/models";
import { describe, expect, it } from "vite-plus/test";

import { rankBesideThreadCandidates } from "./openBesideThread.logic";

const thread = (
  id: string,
  branch: string | null,
  updatedAt: string,
  archivedAt: string | null = null,
) =>
  ({
    id,
    environmentId: "env",
    title: `Thread ${id}`,
    branch,
    updatedAt,
    archivedAt,
  }) as unknown as EnvironmentThreadShell;

describe("rankBesideThreadCandidates", () => {
  it("lists threads on the request's branch first, then the most recent, without archived ones", () => {
    const ranked = rankBesideThreadCandidates(
      [
        thread("old", "main", "2026-10-01T00:00:00.000Z"),
        thread("branch-old", "feature/x", "2026-10-02T00:00:00.000Z"),
        thread("new", null, "2026-10-14T00:00:00.000Z"),
        thread("archived", "feature/x", "2026-10-15T00:00:00.000Z", "2026-10-15T01:00:00.000Z"),
        thread("branch-new", "feature/x", "2026-10-10T00:00:00.000Z"),
      ],
      "feature/x",
    );
    expect(ranked.map((candidate) => [candidate.title, candidate.onBranch])).toEqual([
      ["Thread branch-new", true],
      ["Thread branch-old", true],
      ["Thread new", false],
      ["Thread old", false],
    ]);
    expect(ranked[0]!.ref).toEqual({ environmentId: "env", threadId: "branch-new" });
  });

  it("returns nothing for a project without live threads", () => {
    expect(rankBesideThreadCandidates([], "feature/x")).toEqual([]);
  });
});
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cd apps/web && node ../../scripts/run-local-vp.mjs test run --project unit src/components/pullRequests/openBesideThread.logic.test.ts`
Expected: FAIL — module not found.

- [ ] **Step 3: Implement the ranking**

```ts
import { scopeThreadRef } from "@bibcode/client-runtime/environment";
import type { EnvironmentThreadShell } from "@bibcode/client-runtime/state/models";
import type { ScopedThreadRef } from "@bibcode/contracts";

export interface BesideThreadCandidate {
  readonly ref: ScopedThreadRef;
  readonly title: string;
  readonly onBranch: boolean;
}

/** Threads to open a request beside: on its branch first, then the most recently active. */
export function rankBesideThreadCandidates(
  threads: ReadonlyArray<EnvironmentThreadShell>,
  headBranch: string,
): BesideThreadCandidate[] {
  return threads
    .filter((thread) => thread.archivedAt === null)
    .map((thread) => ({ thread, onBranch: thread.branch === headBranch }))
    .toSorted(
      (left, right) =>
        Number(right.onBranch) - Number(left.onBranch) ||
        right.thread.updatedAt.localeCompare(left.thread.updatedAt),
    )
    .map(({ thread, onBranch }) => ({
      ref: scopeThreadRef(thread.environmentId, thread.id),
      title: thread.title,
      onBranch,
    }));
}
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cd apps/web && node ../../scripts/run-local-vp.mjs test run --project unit src/components/pullRequests/openBesideThread.logic.test.ts`
Expected: PASS.

- [ ] **Step 5: Write the failing button test**

Create `OpenBesideThreadButton.test.tsx` (happy-dom). Mock `../../state/entities` so `useThreadShellsForProjectRefs` returns `h.threads`, mock `@tanstack/react-router`'s `useNavigate` to `h.navigate`, and use the real `../../rightPanelStore`:

```tsx
  it("opens the request in the chosen thread's panel and goes to that thread", async () => {
    h.threads = [thread("t1", "feature/x", "2026-10-10T00:00:00.000Z")];
    await render(<OpenBesideThreadButton projectRef={projectRef} number={89} provider="gitlab" headBranch="feature/x" />);
    await click("Open beside a thread…");
    expect(document.body.textContent).toContain("On this branch");
    await click("Thread t1, on this branch");
    expect(
      selectThreadRightPanelState(
        useRightPanelStore.getState().byThreadKey,
        { environmentId: "env", threadId: "t1" } as never,
      ).activeSurfaceId,
    ).toBe("pullRequest:89");
    expect(h.navigate).toHaveBeenCalledWith({
      to: "/$environmentId/$threadId",
      params: { environmentId: "env", threadId: "t1" },
    });
  });

  it("is disabled with a reason when the project has no thread", async () => {
    h.threads = [];
    await render(<OpenBesideThreadButton projectRef={projectRef} number={89} provider="gitlab" headBranch="feature/x" />);
    const trigger = [...document.querySelectorAll("button")].find(
      (button) => button.textContent === "Open beside a thread…",
    )!;
    expect(trigger.disabled).toBe(true);
    expect(trigger.title).toBe("This project has no thread to open it beside.");
  });
```

Reuse the `thread(...)` factory from the logic test (copy it into this file) and the `render`/`click` style from `review/testHelpers` (`mount`, `click`).

- [ ] **Step 6: Implement the button**

```tsx
import type { ScopedProjectRef, SourceControlProviderKind } from "@bibcode/contracts";
import { useNavigate } from "@tanstack/react-router";
import { useMemo } from "react";

import { useRightPanelStore } from "../../rightPanelStore";
import { useThreadShellsForProjectRefs } from "../../state/entities";
import { Button } from "../ui/button";
import { Menu, MenuItem, MenuPopup, MenuTrigger } from "../ui/menu";
import { rankBesideThreadCandidates } from "./openBesideThread.logic";

const NO_THREAD_REASON = "This project has no thread to open it beside.";

/** The full page's way to pin a request beside one of the project's threads. */
export function OpenBesideThreadButton({
  projectRef,
  number,
  provider,
  headBranch,
}: {
  projectRef: ScopedProjectRef;
  number: number;
  provider: SourceControlProviderKind;
  headBranch: string;
}) {
  const navigate = useNavigate();
  const projectRefs = useMemo(() => [projectRef], [projectRef]);
  const threads = useThreadShellsForProjectRefs(projectRefs);
  const candidates = useMemo(
    () => rankBesideThreadCandidates(threads, headBranch),
    [headBranch, threads],
  );
  if (candidates.length === 0) {
    return (
      <Button size="sm" variant="outline" disabled title={NO_THREAD_REASON}>
        Open beside a thread…
      </Button>
    );
  }
  return (
    <Menu>
      <MenuTrigger render={<Button size="sm" variant="outline" />}>Open beside a thread…</MenuTrigger>
      <MenuPopup align="end">
        {candidates.map((candidate) => (
          <MenuItem
            key={`${candidate.ref.environmentId}:${candidate.ref.threadId}`}
            aria-label={candidate.onBranch ? `${candidate.title}, on this branch` : candidate.title}
            onClick={() => {
              useRightPanelStore.getState().openPullRequest(candidate.ref, { number, provider });
              void navigate({
                to: "/$environmentId/$threadId",
                params: {
                  environmentId: candidate.ref.environmentId,
                  threadId: candidate.ref.threadId,
                },
              });
            }}
          >
            <span className="truncate">{candidate.title}</span>
            {candidate.onBranch ? (
              <span className="ml-auto text-xs text-muted-foreground">On this branch</span>
            ) : null}
          </MenuItem>
        ))}
      </MenuPopup>
    </Menu>
  );
}
```

Check `MenuTrigger`'s `render` prop usage against another `MenuTrigger` in the codebase (e.g. `SourceControlPanel.tsx`) and match it; if the disabled `Button`'s native `title` is not shown by the project's button, wrap it in the same disabled-reason pattern `GitActionsControl.tsx` uses (`Popover` with `openOnHover`) so the reason is visible.

- [ ] **Step 7: Put it on the full page**

In `PullRequestsPanel.tsx`, pass `headerActions` to the wrapped `PullRequestsDetailView` (from Task 2):

```tsx
            headerActions={(detail) => (
              <OpenBesideThreadButton
                projectRef={projectRef}
                number={detail.number}
                provider={context.provider}
                headBranch={detail.headBranch}
              />
            )}
```

- [ ] **Step 8: Run the tests**

Run: `cd apps/web && node ../../scripts/run-local-vp.mjs test run --project unit src/components/pullRequests`
Expected: PASS.

- [ ] **Step 9: Lint, review and commit**

```bash
node scripts/run-local-vp.mjs lint apps/web/src/components/pullRequests
node scripts/run-local-vp.mjs fmt apps/web/src/components/pullRequests
cd apps/web && npx tsc --noEmit -p tsconfig.json && cd ../..
codex review -c sandbox_mode="read-only" --uncommitted
git add apps/web/src/components/pullRequests
git commit -m "feat(web): open a request beside one of the project's threads" -m "<the two trailer lines from Global Constraints>"
```

---

### Task 9: Documentation, reviews and full gates

**Files:**
- Modify: `docs/user/workspace-ui.md` (the right panel section and the Pull Requests section)
- Modify: `docs/architecture/overview.md` (the web client's right panel / pull requests description)
- Modify: `CHANGELOG.md` (new `## [Unreleased]` entry at the top if none exists)
- Modify: `docs/testing/cross-platform-validation.md` (the create-dialog paragraph near "From Source Control, **Push & create PR/MR**", and "Pull Requests web shell validation" items 8 onward)

- [ ] **Step 1: User documentation**

In `docs/user/workspace-ui.md`, where the right panel's surfaces are listed, add:

```md
A pull or merge request can open as a right-panel tab beside a thread, with its
Conversation, Commits, Pipelines and Changes and every action, against the
thread's checkout:

- **Done** in the Source Control panel's create dialog opens the request it
  created or found.
- **Open MR in panel** (**Open PR in panel** on GitHub) next to **View MR** in
  the Source Control panel's menu opens the branch's open request.
- **Open beside a thread…** on the full Pull Requests page lists the project's
  threads, those on the request's branch first, and opens the request in the
  one you choose.

Each request is its own tab; **Open full page** shows it on the Pull Requests
page. Deleting the request closes its tab, and a revert request opens in a new
one.
```

- [ ] **Step 2: Architecture documentation**

In `docs/architecture/overview.md`, where the web client's right-panel surfaces (`rightPanelStore`) are described, add:

```md
- `pullRequest` surfaces (`pullRequest:<number>`, with the provider and the
  selected detail tab) host `PullRequestsDetailView` per thread, scoped to the
  thread's checkout. The detail view and its file links read a
  `PullRequestsNavigation` from React context instead of the router: the full
  page provides `PullRequestsRouteNavigation` (URLs unchanged), the panel a
  store adapter (tab, revert-opens-a-tab, delete-closes-the-tab, file anchor in
  component state).
```

- [ ] **Step 3: Changelog**

At the top of `CHANGELOG.md`, under `## [Unreleased]` (create the heading above `## [v0.9.3]` if it does not exist) and a `### Pull and merge requests` subheading:

```md
- A pull or merge request can open as a tab in a thread's right panel, beside the conversation. **Done** after creating one from the Source Control panel opens it there, **Open MR in panel** opens the branch's open request, and the full Pull Requests page offers **Open beside a thread…**.
```

- [ ] **Step 4: Native validation runbook**

In `docs/testing/cross-platform-validation.md`:

- In the create-dialog paragraph, after the sentence ending "…cancelling the dialog after the push must not create a request.", add: "After the request is created or found, **Done** in the dialog opened from Source Control must open it as a right-panel tab beside the thread (one tab per request; Done again does not duplicate it); **Done** from the chat Git action, the Pull Requests page or the Git Manager only closes the dialog."
- In "Pull Requests web shell validation" item 8, replace "Confirm metadata moves above the merge box below the large-screen breakpoint." with "Confirm metadata moves above the merge box when the detail is narrower than its container breakpoint (48rem): in a right-panel tab and in a narrow window."
- Add a new numbered item after item 8:

```md
9. Right-panel requests: with a thread on a branch that has an open request,
   choose **Open MR in panel** (**Open PR in panel** on GitHub) from the
   Source Control menu; it opens one right-panel tab labelled `!N`/`#N` beside the thread, against the thread's
   checkout. In the tab, switch tabs, follow a conversation file link (it must
   select Changes and scroll to the file), reload and confirm the tab and its
   selected section persist, then use **Open full page** and confirm the
   `?tab=` URL. On a disposable request, a revert must open the revert request
   as a second tab and a delete must close only its tab. From the full page,
   **Open beside a thread…** must list the project's threads with those on the
   request's branch first ("On this branch"), open the request in the chosen
   thread and navigate to it; with no thread it is disabled with its reason.
   Remove the thread's worktree and confirm the tab shows the unavailable
   reason instead of an error.
```

  Renumber the following items.

- [ ] **Step 5: Reviews required by AGENTS.md**

- Run the `vercel-react-best-practices` review over every component and hook changed in Tasks 1–8 (`PullRequestPanelSurface`, `pullRequestsNavigation`, `PullRequestsDetailView`, `PullRequestsFiles`, `PullRequestsTimelineItem`, `OpenBesideThreadButton`, `SourceControlPanel`, `GitActionsControl`, `RightPanelTabs`, `GitManagerCreatePullRequestDialog`, `usePullRequestsContextQuery`) and fix what it finds.
- Review the new tab, menu items, picker and panel states against `UI.md`: defaults (Done opens one tab, never duplicates), disabled states that explain themselves ("This project has no thread to open it beside."), actionable unavailable states, and no lost work.

- [ ] **Step 6: Full gates**

```bash
node scripts/run-local-vp.mjs check
node scripts/run-local-vp.mjs run typecheck
cd apps/web && node ../../scripts/run-local-vp.mjs test run --passWithNoTests --project unit && cd ../..
```

Expected: `vp check` prints "Found no warnings or lint errors", typecheck exits 0, every web unit test passes.

- [ ] **Step 7: Review and commit**

```bash
node scripts/run-local-vp.mjs fmt docs/user/workspace-ui.md docs/architecture/overview.md CHANGELOG.md docs/testing/cross-platform-validation.md
codex review -c sandbox_mode="read-only" --uncommitted
git add docs/user/workspace-ui.md docs/architecture/overview.md CHANGELOG.md docs/testing/cross-platform-validation.md
git commit -m "docs: pull and merge requests in a thread's right panel" -m "<the two trailer lines from Global Constraints>"
```

Profiler coverage: none applies (no child process, RPC method, worktree removal, binary, service or event). `docs/testing/cross-platform-validation.md` is updated for the new packaged UI flows (Step 4); the other runbooks under `docs/testing/` were reviewed and remain accurate.
