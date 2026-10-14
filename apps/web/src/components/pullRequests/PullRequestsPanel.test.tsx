// @vitest-environment happy-dom
import {
  AVAILABLE_CONNECTION_STATE,
  type SupervisorConnectionState,
} from "@bibcode/client-runtime/connection";
import { environmentRpcKey } from "@bibcode/client-runtime/state/runtime";
import type { PullRequestsContext, ScopedProjectRef, ServerConfig } from "@bibcode/contracts";
import { makeTestExecutionEnvironmentCapabilities } from "@bibcode/shared/testSupport";
import { act, useContext, useImperativeHandle, type Ref } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { usePullRequestsStore } from "../../pullRequestsStore";
import { PullRequestsContextRefresh } from "./pullRequestsContextRefresh";
import { context } from "./testFixtures";
const h = vi.hoisted(() => ({
  enabled: true,
  connection: null as SupervisorConnectionState | null,
  config: null as ServerConfig | null,
  context: null as PullRequestsContext | null,
  contextError: null as string | null,
  pending: false,
  catalogPending: false,
  catalogData: { worktrees: [] } as {
    worktrees: { path: string; branch: string; isBare: boolean }[];
  } | null,
  catalogError: null as string | null,
  getContext: vi.fn((args: unknown) => ({ kind: "context", args })),
  list: vi.fn((args: unknown) => ({ kind: "list", args })),
  requestContextRescan: vi.fn(),
  contextRefresh: null as (() => void) | null,
  catalog: vi.fn((args: unknown) => ({ kind: "catalog", args })),
  refresh: vi.fn(),
  revalidate: vi.fn(),
  listRefresh: vi.fn(),
  listProps: null as Record<string, unknown> | null,
  detailProps: null as Record<string, unknown> | null,
  dialogProps: null as Record<string, unknown> | null,
}));
vi.mock("../../state/entities", () => ({
  useProject: () => ({ id: "project", workspaceRoot: "Z:\\opaque\\main" }),
  useServerConfigs: () => new Map([["env", h.config]]),
}));
vi.mock("../../state/environments", () => ({
  useEnvironmentConnectionState: () => ({ data: h.connection }),
}));
vi.mock("../../hooks/useSettings", () => ({
  usePrimarySettings: (select: (s: { pullRequestsEnabled: boolean }) => unknown) =>
    select({ pullRequestsEnabled: h.enabled }),
}));
vi.mock("../../state/pullRequests", () => ({
  pullRequestsEnvironment: {
    getContext: h.getContext,
    list: h.list,
    requestContextRescan: h.requestContextRescan,
  },
}));
vi.mock("../../state/worktrees", () => ({ worktreeEnvironment: { catalog: h.catalog } }));
vi.mock("../../state/query", () => ({
  useEnvironmentQuery: (atom: { kind: string } | null) => ({
    data: atom?.kind === "context" ? h.context : atom?.kind === "catalog" ? h.catalogData : null,
    emission: { _tag: "Initial" },
    error:
      atom?.kind === "catalog" ? h.catalogError : atom?.kind === "context" ? h.contextError : null,
    isPending: atom?.kind === "catalog" ? h.catalogPending : h.pending,
    refresh: h.refresh,
    revalidate: h.revalidate,
    requiresRetry: false,
  }),
}));
vi.mock("../../lib/openPullRequestLink", () => ({ useOpenPrLink: () => vi.fn() }));
vi.mock("@tanstack/react-router", () => ({
  Link: ({ to, children }: { to: string; children: React.ReactNode }) => (
    <a href={to}>{children}</a>
  ),
}));
vi.mock("./list/PullRequestsListView", () => ({
  PullRequestsListView: (
    props: Record<string, unknown> & { ref?: Ref<{ refresh: () => void }> },
  ) => {
    h.listProps = props;
    h.contextRefresh = useContext(PullRequestsContextRefresh);
    useImperativeHandle(props.ref, () => ({ refresh: h.listRefresh }));
    return <div>List content</div>;
  },
}));
vi.mock("./detail/PullRequestsDetailView", () => ({
  PullRequestsDetailView: (props: Record<string, unknown>) => {
    h.detailProps = props;
    return <div>Detail content</div>;
  },
}));
vi.mock("../gitManager/provider/GitManagerCreatePullRequestDialog", () => ({
  GitManagerCreatePullRequestDialog: (props: Record<string, unknown>) => {
    h.dialogProps = props;
    return <div role="dialog" />;
  },
}));
import { PullRequestsPanel } from "./PullRequestsPanel";
let container: HTMLDivElement;
let root: Root;
const ref = { environmentId: "env", projectId: "project" } as ScopedProjectRef;
function button(label: string) {
  const b = [...container.querySelectorAll("button")].find((b) => b.textContent?.trim() === label);
  if (!b) throw new Error(`Missing ${label}`);
  return b;
}
beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  usePullRequestsStore.setState({ byProjectKey: {} });
  h.enabled = true;
  h.connection = {
    ...AVAILABLE_CONNECTION_STATE,
    desired: true,
    phase: "connected",
    network: "online",
  };
  h.config = {
    environment: {
      capabilities: makeTestExecutionEnvironmentCapabilities({
        pullRequestsReads: true,
        pullRequestsMutations: true,
      }),
    },
  } as ServerConfig;
  h.context = context;
  h.contextError = null;
  h.pending = false;
  h.catalogPending = false;
  h.catalogData = { worktrees: [] };
  h.catalogError = null;
  h.listProps = null;
  h.dialogProps = null;
  h.getContext.mockClear();
  h.list.mockClear();
  h.requestContextRescan.mockClear();
  h.contextRefresh = null;
  h.catalog.mockClear();
  h.refresh.mockClear();
  h.revalidate.mockClear();
  h.listRefresh.mockClear();
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
});
describe("PullRequestsPanel", () => {
  it("stops the worktree loading status after the live catalog emits", async () => {
    h.catalogPending = true;
    h.catalogData = null;
    await act(async () => root.render(<PullRequestsPanel projectRef={ref} />));
    expect(container.textContent).toContain("Loading worktrees…");
    h.catalogData = { worktrees: [{ path: "/another", branch: "feature-pr-14", isBare: false }] };
    // A live stream remains waiting between emissions and after a refresh.
    await act(async () => root.render(<PullRequestsPanel projectRef={ref} />));
    expect(container.textContent).not.toContain("Loading worktrees…");
    expect(
      container.querySelector('[aria-label="Worktree"]')?.hasAttribute("aria-describedby"),
    ).toBe(false);
  });
  it("treats an empty catalog as loaded and still exposes catalog errors", async () => {
    h.catalogPending = true;
    await act(async () => root.render(<PullRequestsPanel projectRef={ref} />));
    expect(container.textContent).not.toContain("Loading worktrees…");
    h.catalogError = "Catalog unavailable. Retry.";
    await act(async () => root.render(<PullRequestsPanel projectRef={ref} />));
    expect(container.textContent).toContain(h.catalogError);
  });

  it("issues no request when disconnected, even with cached context", async () => {
    h.connection = AVAILABLE_CONNECTION_STATE;
    await act(async () => root.render(<PullRequestsPanel projectRef={ref} />));
    expect(container.textContent).toContain("This environment is disconnected.");
    expect(h.getContext).not.toHaveBeenCalled();
    expect(h.catalog).not.toHaveBeenCalled();
    expect(h.list).not.toHaveBeenCalled();
    expect(h.listProps).toBeNull();
  });
  it("gates disabled settings and links to settings", async () => {
    h.enabled = false;
    await act(async () => root.render(<PullRequestsPanel projectRef={ref} />));
    expect(container.textContent).toContain(
      "Pull requests are turned off in Settings → Source Control.",
    );
    expect(container.querySelector("a")?.getAttribute("href")).toBe("/settings/source-control");
    expect(h.getContext).not.toHaveBeenCalled();
  });
  it("paints from the stored context, then reads it live once", async () => {
    h.context = { ...context, stale: true };
    await act(async () => root.render(<PullRequestsPanel projectRef={ref} />));
    expect(container.textContent).toContain("List content");
    expect(h.revalidate).toHaveBeenCalledTimes(1);
    await act(async () => root.render(<PullRequestsPanel projectRef={ref} />));
    expect(h.revalidate).toHaveBeenCalledTimes(1);
    // A failed live read leaves the stored copy; the next stored answer is read live again.
    h.context = { ...context, stale: true };
    await act(async () => root.render(<PullRequestsPanel projectRef={ref} />));
    expect(h.revalidate).toHaveBeenCalledTimes(2);
    h.context = { ...context, account: { ...context.account, login: "bob" } };
    await act(async () => root.render(<PullRequestsPanel projectRef={ref} />));
    expect(container.textContent).toContain("bob");
    expect(h.revalidate).toHaveBeenCalledTimes(2);
  });
  it("joins a live context read already in flight instead of restarting it", async () => {
    // A panel remounted by a route change finds the shared read still running.
    h.context = { ...context, stale: true };
    h.pending = true;
    await act(async () => root.render(<PullRequestsPanel projectRef={ref} />));
    expect(container.textContent).toContain("List content");
    expect(h.revalidate).not.toHaveBeenCalled();
  });
  it("keeps the stored context when its live read fails", async () => {
    h.context = { ...context, stale: true };
    h.contextError = "The host could not be reached.";
    await act(async () => root.render(<PullRequestsPanel projectRef={ref} />));
    expect(container.textContent).toContain("List content");
    expect(container.textContent).not.toContain(h.contextError);
  });
  it("loads context with an opaque checkout and passes the scope to the list", async () => {
    usePullRequestsStore.getState().setCheckoutCwd(ref, "Z:\\opaque\\worktree");
    await act(async () => root.render(<PullRequestsPanel projectRef={ref} />));
    expect(h.getContext).toHaveBeenCalledWith({
      environmentId: "env",
      input: { cwd: "Z:\\opaque\\worktree" },
    });
    expect(h.listProps).toMatchObject({
      scope: { environmentId: "env", cwd: "Z:\\opaque\\worktree" },
      projectRef: ref,
      context,
    });
    expect(container.textContent).toContain("owner/repo");
    expect(container.textContent).toContain("alice");
    await act(async () => button("Refresh").click());
    expect(h.listRefresh).toHaveBeenCalledOnce();
    await act(async () => button("New pull request").click());
    expect(h.dialogProps).toMatchObject({
      scope: { environmentId: "env", cwd: "Z:\\opaque\\worktree" },
      open: true,
      providerHint: { kind: context.provider, baseUrl: "https://github.com" },
    });
  });
  it("shows loading and actionable unavailable context with Rescan", async () => {
    h.context = null;
    h.pending = true;
    await act(async () => root.render(<PullRequestsPanel projectRef={ref} />));
    expect(container.querySelector('[role="status"]')?.textContent).toContain("Loading");
    h.pending = false;
    h.context = {
      status: "unavailable",
      code: "no_remote",
      message: "Add an origin remote.",
      authCommand: null,
      installHint: null,
      provider: null,
      host: null,
    };
    await act(async () => root.render(<PullRequestsPanel projectRef={ref} />));
    expect(container.textContent).toContain("Add an origin remote.");
    await act(async () => button("Rescan").click());
    expect(h.requestContextRescan).toHaveBeenCalledWith({
      environmentId: "env",
      input: { cwd: "Z:\\opaque\\main" },
    });
    expect(h.refresh).toHaveBeenCalled();
    expect(h.requestContextRescan.mock.invocationCallOrder[0]).toBeLessThan(
      h.refresh.mock.invocationCallOrder[0]!,
    );
    expect(h.listProps).toBeNull();
  });
  it("reads the first list page alongside a pending context, for Open and Closed only", async () => {
    const firstPage = {
      cwd: "Z:\\opaque\\main",
      state: "open",
      search: null,
      author: null,
      assignee: null,
      reviewer: null,
      reviewStatus: null,
      draft: null,
      labels: [],
      milestone: null,
      targetBranch: null,
      sort: "newest",
      cursor: null,
    };
    h.context = null;
    h.pending = true;
    await act(async () => root.render(<PullRequestsPanel projectRef={ref} />));
    expect(h.list).toHaveBeenCalledWith({ environmentId: "env", input: firstPage });

    h.pending = false;
    h.context = context;
    await act(async () => root.render(<PullRequestsPanel projectRef={ref} />));
    // The page stays available until the committed list acknowledges it.
    expect(h.listProps?.freshPageKey).toBe(
      environmentRpcKey({ environmentId: ref.environmentId, input: firstPage }),
    );
    const consume = h.listProps?.onFreshPageConsumed as (key: string) => void;
    await act(async () =>
      consume(environmentRpcKey({ environmentId: ref.environmentId, input: firstPage })),
    );
    expect(h.listProps?.freshPageKey).toBeNull();

    // Merged and All depend on the host (GitHub folds them into Closed): wait for context.
    usePullRequestsStore.getState().setListTab(ref, "merged");
    h.context = null;
    h.pending = true;
    h.list.mockClear();
    await act(async () => root.render(<PullRequestsPanel projectRef={ref} />));
    expect(h.list).not.toHaveBeenCalled();
    await act(async () =>
      root.render(<PullRequestsPanel projectRef={ref} number={14} tab="files" />),
    );
    expect(h.list).not.toHaveBeenCalled();
  });
  it("marks no page as fresh when the context was already available", async () => {
    await act(async () => root.render(<PullRequestsPanel projectRef={ref} />));
    expect(h.list).not.toHaveBeenCalled();
    expect(h.listProps?.freshPageKey).toBeNull();
  });
  it("drops the page read during a load whose context settles unavailable", async () => {
    h.context = null;
    h.pending = true;
    await act(async () => root.render(<PullRequestsPanel projectRef={ref} />));
    expect(h.list).toHaveBeenCalled();
    h.pending = false;
    h.context = {
      status: "unavailable",
      code: "not_authenticated",
      message: "Authenticate the provider CLI for this host, then rescan.",
      authCommand: "glab auth login --hostname git.example.test",
      installHint: null,
      provider: "gitlab",
      host: "git.example.test",
    };
    await act(async () => root.render(<PullRequestsPanel projectRef={ref} />));
    await act(async () => button("Rescan").click());
    h.context = context;
    await act(async () => root.render(<PullRequestsPanel projectRef={ref} />));
    // The list opens only after Rescan: the earlier page is not fresh, so it is read again.
    expect(h.listProps).not.toBeNull();
    expect(h.listProps?.freshPageKey).toBeNull();
  });
  it("rescans the context when a read reports lost authentication", async () => {
    await act(async () => root.render(<PullRequestsPanel projectRef={ref} />));
    expect(h.requestContextRescan).not.toHaveBeenCalled();
    await act(async () => h.contextRefresh?.());
    expect(h.requestContextRescan).toHaveBeenCalledWith({
      environmentId: "env",
      input: { cwd: "Z:\\opaque\\main" },
    });
    // Only the context is read again; the worktree catalog is left alone. Recovery is
    // automatic, so it re-reads without clearing an exhausted transport cut-off.
    expect(h.revalidate).toHaveBeenCalledTimes(1);
    expect(h.refresh).not.toHaveBeenCalled();
  });
  it("mounts the detail view with the number, tab, context and selected checkout", async () => {
    await act(async () =>
      root.render(<PullRequestsPanel projectRef={ref} number={14} tab="files" />),
    );
    expect(h.detailProps).toMatchObject({
      projectRef: ref,
      scope: { environmentId: "env", cwd: "Z:\\opaque\\main" },
      context,
      number: 14,
      tab: "files",
    });
    expect(h.listProps).toBeNull();
    expect(usePullRequestsStore.getState().selectViewState(ref).lastNumber).toBe(14);
  });
  it("explains disabled mutations and shows custom host vocabulary", async () => {
    h.config = {
      environment: {
        capabilities: makeTestExecutionEnvironmentCapabilities({ pullRequestsReads: true }),
      },
    } as ServerConfig;
    h.context = {
      ...context,
      provider: "gitlab",
      host: "company.test",
      capabilities: {
        ...context.capabilities,
        vocabulary: {
          ...context.capabilities.vocabulary,
          pullRequest: "merge request",
          pullRequests: "merge requests",
        },
      },
    };
    await act(async () => root.render(<PullRequestsPanel projectRef={ref} />));
    expect(container.textContent).toContain("company.test");
    expect(button("New merge request")).toMatchObject({
      disabled: true,
      title: "This environment does not support Pull Requests actions.",
    });
    expect(button("New merge request").parentElement?.title).toBe(
      "This environment does not support Pull Requests actions.",
    );
  });
});

it("keeps checkout recovery available when the saved checkout cannot resolve context", async () => {
  usePullRequestsStore.getState().setCheckoutCwd(ref, "/missing");
  h.context = {
    status: "unavailable",
    code: "no_remote",
    message: "No origin remote.",
    provider: null,
    host: null,
    installHint: null,
    authCommand: null,
  };
  await act(async () => root.render(<PullRequestsPanel projectRef={ref} />));
  const picker = container.querySelector<HTMLButtonElement>('[aria-label="Worktree"]');
  expect(picker).not.toBeNull();
  await act(async () => picker!.click());
  await act(async () =>
    [...document.querySelectorAll<HTMLElement>('[role="option"]')]
      .find((o) => o.textContent?.includes("Main checkout"))!
      .click(),
  );
  expect(usePullRequestsStore.getState().selectViewState(ref).checkoutCwd).toBe("Z:\\opaque\\main");
  expect(h.getContext).toHaveBeenLastCalledWith({
    environmentId: "env",
    input: { cwd: "Z:\\opaque\\main" },
  });
});

it("resets the available subtree when Rescan changes repository identity", async () => {
  await act(async () => root.render(<PullRequestsPanel projectRef={ref} />));
  await act(async () => button("New pull request").click());
  expect(container.querySelector('[role="dialog"]')).not.toBeNull();
  await act(async () => button("Rescan").click());
  h.context = {
    ...context,
    repository: "other/repo",
    account: { ...context.account, login: "bob" },
  };
  await act(async () => root.render(<PullRequestsPanel projectRef={ref} />));
  expect(container.textContent).toContain("other/repo");
  expect(container.querySelector('[role="dialog"]')).toBeNull();
});
