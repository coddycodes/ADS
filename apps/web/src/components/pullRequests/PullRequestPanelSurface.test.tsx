// @vitest-environment happy-dom
import { AVAILABLE_CONNECTION_STATE } from "@coddy/client-runtime/connection";
import { scopeThreadRef } from "@coddy/client-runtime/environment";
import { projectKey } from "@coddy/client-runtime/state/entities";
import type { ServerConfig } from "@coddy/contracts";
import { makeTestExecutionEnvironmentCapabilities } from "@coddy/shared/testSupport";
import { act, useContext, useEffect } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { usePullRequestsStore } from "../../pullRequestsStore";
import { selectThreadRightPanelState, useRightPanelStore } from "../../rightPanelStore";
import type { PullRequestsNavigation } from "./detail/pullRequestsNavigation";
import { context as availableContext } from "./testFixtures";

const h = vi.hoisted(() => ({
  enabled: true,
  connected: true,
  context: null as unknown,
  contextError: null as string | null,
  detailProps: null as Record<string, unknown> | null,
  detailMounts: 0,
  navigation: null as unknown,
  fileAnchor: null as string | null,
  navigate: vi.fn(),
  getContext: vi.fn((args: unknown) => ({ kind: "context", args })),
}));
vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => h.navigate,
  useLocation: () => ({ hash: "" }),
  Link: ({ children }: { children: React.ReactNode }) => <a>{children}</a>,
}));
vi.mock("../../state/entities", () => ({
  useServerConfigs: () =>
    new Map([
      [
        "env",
        {
          environment: {
            capabilities: makeTestExecutionEnvironmentCapabilities({
              pullRequestsReads: true,
              pullRequestsMutations: true,
            }),
          },
        } as ServerConfig,
      ],
    ]),
}));
vi.mock("../../state/environments", () => ({
  useEnvironmentConnectionState: () => ({
    data: h.connected
      ? { ...AVAILABLE_CONNECTION_STATE, desired: true, phase: "connected", network: "online" }
      : AVAILABLE_CONNECTION_STATE,
  }),
}));
vi.mock("../../hooks/useSettings", () => ({
  usePrimarySettings: (select: (s: { pullRequestsEnabled: boolean }) => unknown) =>
    select({ pullRequestsEnabled: h.enabled }),
}));
vi.mock("../../state/pullRequests", () => ({
  pullRequestsEnvironment: { getContext: h.getContext, requestContextRescan: vi.fn() },
}));
vi.mock("../../state/query", () => ({
  useEnvironmentQuery: (atom: { kind: string } | null) => ({
    data: atom === null ? null : h.context,
    emission: { _tag: "Initial" },
    error: atom === null ? null : h.contextError,
    isPending: false,
    refresh: vi.fn(),
    revalidate: vi.fn(),
    requiresRetry: false,
  }),
}));
vi.mock("./detail/PullRequestsDetailView", async () => {
  const { PullRequestsFileAnchorContext, PullRequestsNavigationContext } =
    await import("./detail/pullRequestsNavigation");
  return {
    PullRequestsDetailView: (props: Record<string, unknown>) => {
      const navigation = useContext(PullRequestsNavigationContext);
      const fileAnchor = useContext(PullRequestsFileAnchorContext);
      useEffect(() => {
        h.detailProps = props;
        h.navigation = navigation;
        h.fileAnchor = fileAnchor;
      });
      useEffect(() => {
        h.detailMounts += 1;
      }, []);
      return null;
    },
  };
});

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

function panelState() {
  return selectThreadRightPanelState(useRightPanelStore.getState().byThreadKey, threadRef);
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  useRightPanelStore.setState({ byThreadKey: {} });
  usePullRequestsStore.setState({ byProjectKey: {} });
  useRightPanelStore
    .getState()
    .openPullRequest(threadRef, { number: 89, provider: "gitlab" }, "commits");
  h.detailProps = null;
  h.navigation = null;
  h.fileAnchor = null;
  h.navigate.mockReset();
  h.getContext.mockClear();
  h.enabled = true;
  h.connected = true;
  h.contextError = null;
  h.context = {
    ...availableContext,
    provider: "gitlab",
    host: "gitlab.example.test",
    repository: "team/repo",
    webUrl: "https://gitlab.example.test/team/repo",
  };
});
afterEach(async () => {
  await act(async () => root.unmount());
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
    let state = panelState();
    expect(state.surfaces.map((entry) => entry.id)).toEqual(["pullRequest:89", "pullRequest:90"]);
    expect(state.surfaces[0]).toMatchObject({ tab: "files" });
    expect(state.activeSurfaceId).toBe("pullRequest:90");
    await act(async () => navigation.requestGone());
    state = panelState();
    expect(state.surfaces.map((entry) => entry.id)).toEqual(["pullRequest:90"]);
  });

  it("closes a deleted request's tab even after the panel moved to another tab", async () => {
    await render();
    const discardDeleted = (h.navigation as PullRequestsNavigation).discardDeleted!;
    useRightPanelStore.getState().openPullRequest(threadRef, { number: 90, provider: "gitlab" });
    await act(async () => root.render(null));
    discardDeleted();
    expect(panelState().surfaces.map((entry) => entry.id)).toEqual(["pullRequest:90"]);
  });

  it("shows a conversation file link on the Changes tab at that file", async () => {
    await render();
    const navigation = h.navigation as PullRequestsNavigation;
    await act(async () => navigation.showFile("src/a.ts"));
    expect(h.fileAnchor).toBe("src/a.ts");
    // The anchor travels in its own context: the navigation object keeps its identity.
    expect(h.navigation).toBe(navigation);
    expect(panelState().surfaces[0]).toMatchObject({ tab: "files" });
  });

  it("explains instead of loading when the workspace is unavailable, and recovers", async () => {
    await render({ workspaceUnavailable: "The worktree was removed." });
    expect(container.textContent).toContain("The worktree was removed.");
    expect(h.detailProps).toBeNull();
    expect(h.getContext).not.toHaveBeenCalled();
    await render();
    expect(h.detailProps).toMatchObject({ number: 89 });
  });

  it("explains without reading when the environment is disconnected", async () => {
    h.connected = false;
    await render();
    expect(container.textContent).toContain("This environment is disconnected.");
    expect(h.getContext).not.toHaveBeenCalled();
    expect(h.detailProps).toBeNull();
  });

  it("explains when pull requests are turned off in settings", async () => {
    h.enabled = false;
    await render();
    expect(container.textContent).toContain("Open Source Control settings");
    expect(h.detailProps).toBeNull();
  });

  it("explains when the repository has no supported host", async () => {
    h.context = {
      status: "unavailable",
      message: "No GitHub or GitLab remote.",
      authCommand: null,
      installHint: null,
    };
    await render();
    expect(container.textContent).toContain("No GitHub or GitLab remote.");
    expect(h.detailProps).toBeNull();
  });

  it("gives the detail view the panel's full width", async () => {
    await render();
    const host = container.firstElementChild as HTMLElement;
    expect(host.className.split(" ")).toEqual(
      expect.arrayContaining(["flex", "min-h-0", "min-w-0", "flex-1", "flex-col"]),
    );
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
    const headerActions = h.detailProps!.headerActions as (detail: {
      number: number;
    }) => React.ReactElement;
    const host = document.createElement("div");
    const actionsRoot = createRoot(host);
    await act(async () => actionsRoot.render(headerActions({ number: 89 })));
    expect(host.textContent).toBe("Open full page");
    await act(async () => host.querySelector("button")!.click());
    expect(h.navigate).toHaveBeenCalledWith({
      to: "/project/$environmentId/$projectId/pull-requests/$number",
      params: { environmentId: "env", projectId: "project", number: "89" },
      search: { tab: "commits" },
    });
    expect(usePullRequestsStore.getState().byProjectKey[projectKey(projectRef)]?.checkoutCwd).toBe(
      "/repo/worktree",
    );
    await act(async () => actionsRoot.unmount());
  });
});
