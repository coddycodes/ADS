// @vitest-environment happy-dom

import {
  AVAILABLE_CONNECTION_STATE,
  type SupervisorConnectionState,
} from "@coddy/client-runtime/connection";
import type {
  GitManagerRefsSnapshot,
  GitManagerStashEntry,
  ServerConfig,
  VcsStatusResult,
} from "@coddy/contracts";
import { makeTestExecutionEnvironmentCapabilities } from "@coddy/shared/testSupport";
import * as Cause from "effect/Cause";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";

import { useGitManagerStore } from "../../gitManagerStore";

const h = vi.hoisted(() => ({
  connectionState: null as SupervisorConnectionState | null,
  serverConfig: null as ServerConfig | null,
  project: {
    id: "project-1",
    environmentId: "environment-1",
    title: "Repository",
    workspaceRoot: "/opaque/main",
  } as Record<string, unknown> | null,
  catalog: null as Record<string, unknown> | null,
  catalogPending: false,
  queryAtoms: [] as Array<{ kind: string; cwd?: string } | null>,
  effects: [] as Array<() => void | (() => void)>,
  activeSubscriptions: 0,
  catalogAtom: vi.fn((args: { input: { projectId: string } }) => ({
    kind: "catalog",
    projectId: args.input.projectId,
  })),
  signalAtom: vi.fn((args: { input: { cwd: string } }) => ({
    kind: "signal",
    cwd: args.input.cwd,
  })),
  refsAtom: vi.fn((args: { input: { cwd: string } }) => ({
    kind: "refs",
    cwd: args.input.cwd,
  })),
  stashesAtom: vi.fn((args: { input: { cwd: string } }) => ({
    kind: "stashes",
    cwd: args.input.cwd,
  })),
  statusAtom: vi.fn((args: { input: { cwd: string } }) => ({
    kind: "status",
    cwd: args.input.cwd,
  })),
  listPullRequests: vi.fn((args: { input: { cwd: string } }) => ({
    kind: "provider",
    cwd: args.input.cwd,
  })),
  toolbarProps: [] as Array<Record<string, unknown>>,
  historyProps: [] as Array<Record<string, unknown>>,

  tagsViewProps: [] as Array<Record<string, unknown>>,
  refsSnapshot: null as GitManagerRefsSnapshot | null,
  status: null as VcsStatusResult | null,
  /** A failed status read: its message, with `status` left at its last answer. */
  statusError: null as string | null,
  stashes: null as ReadonlyArray<GitManagerStashEntry> | null,
  refreshed: [] as string[],
  signalGeneration: 1 as number | null,
  runOperation: vi.fn((_registry: unknown, _target: unknown, _onEvent: unknown) => ({
    result: new Promise(() => undefined),
    cancel: vi.fn(),
  })),
  branchDialogProps: [] as Array<Record<string, unknown>>,
  historyTagDialogProps: [] as Array<Record<string, unknown>>,
}));

vi.mock("react", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react")>()),
  useEffect: (effect: () => void | (() => void)) => h.effects.push(effect),
}));

vi.mock("../../state/entities", () => ({
  useProject: () => h.project,
  useServerConfigs: () =>
    new Map(h.serverConfig === null ? [] : [["environment-1", h.serverConfig]]),
  useThreadShellsForProjectRefs: () => [],
}));

vi.mock("../../state/environments", () => ({
  useEnvironmentConnectionState: () => ({ data: h.connectionState }),
  useEnvironment: () => ({ label: "Local" }),
}));

vi.mock("../../state/query", () => ({
  useEnvironmentQuery: (atom: { kind: string; cwd?: string } | null) => {
    h.queryAtoms.push(atom);
    if (atom !== null) {
      h.effects.push(() => {
        h.activeSubscriptions += 1;
        return () => {
          h.activeSubscriptions -= 1;
        };
      });
    }
    const data =
      atom?.kind === "catalog"
        ? h.catalog
        : atom?.kind === "refs"
          ? h.refsSnapshot
          : atom?.kind === "status"
            ? h.status
            : atom?.kind === "signal"
              ? { generation: h.signalGeneration }
              : atom?.kind === "stashes"
                ? h.stashes
                : null;
    // Automatic re-reads record the same way as refreshes.
    const refresh = () => {
      if (atom !== null) h.refreshed.push(atom.kind);
    };
    const error = atom?.kind === "status" ? h.statusError : null;
    return {
      data,
      error,
      isPending: atom?.kind === "catalog" && h.catalogPending,
      refresh,
      revalidate: refresh,
      requiresRetry: false,
      emission:
        error !== null
          ? { _tag: "Failure", cause: Cause.fail(new Error(error)) }
          : data === null
            ? { _tag: "Initial" }
            : { _tag: "Success", value: data },
    };
  },
}));

vi.mock("../../state/worktrees", () => ({
  worktreeEnvironment: { catalog: h.catalogAtom },
}));

vi.mock("../../state/vcs", () => ({
  vcsEnvironment: {
    status: h.statusAtom,
    refreshStatus: { label: "test:refresh-status" },
    stageFiles: { label: "test:stage-files" },
    unstageFiles: { label: "test:unstage-files" },
  },
}));

vi.mock("../../state/gitManager", () => ({
  gitManagerEnvironment: {
    signalWithDegradedFocusRefresh: h.signalAtom,
    getRefs: h.refsAtom,
    getStashes: h.stashesAtom,
    getCommits: () => ({ kind: "commits" }),
    listPullRequests: h.listPullRequests,
    commit: { label: "test:commit" },
    undoCommit: { label: "test:undo-commit" },
    discard: { label: "test:discard" },
  },
  runGitManagerOperation: h.runOperation,
}));

vi.mock("../../state/sourceControlActions", () => ({
  useGitStackedAction: () => ({
    run: vi.fn(),
    isPending: false,
    error: null,
  }),
}));

vi.mock("../ui/tabs", () => ({
  Tabs: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  TabsList: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  TabsPanel: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  TabsTab: ({ children }: { children: React.ReactNode }) => <button>{children}</button>,
}));

vi.mock("./GitManagerToolbar", () => ({
  GitManagerToolbar: (props: Record<string, unknown>) => {
    h.toolbarProps.push(props);
    return (
      <div data-testid="git-manager-toolbar">
        {props.branchSyncDisabledReason as React.ReactNode}
        {props.stashMergeDisabledReason as React.ReactNode}
        {props.tagDisabledReason as React.ReactNode}
      </div>
    );
  },
}));

vi.mock("./history/GitManagerHistoryView", () => ({
  GitManagerHistoryView: (props: Record<string, unknown>) => {
    h.historyProps.push(props);
    return <div data-testid="git-manager-history" />;
  },
}));

vi.mock("./tags/GitManagerTagsView", () => ({
  GitManagerTagsView: (props: Record<string, unknown>) => {
    h.tagsViewProps.push(props);
    return <div data-testid="git-manager-tags" />;
  },
}));

vi.mock("./dialogs/GitManagerBranchDialogs", () => ({
  GitManagerBranchDialogs: (props: Record<string, unknown>) => {
    h.branchDialogProps.push(props);
    return props.dialog === null ? null : <div role="dialog">History branch dialog</div>;
  },
}));

vi.mock("./tags/GitManagerTagDialog", () => ({
  GitManagerTagDialog: (props: Record<string, unknown>) => {
    h.historyTagDialogProps.push(props);
    return props.open === true ? <div role="dialog">History tag dialog</div> : null;
  },
}));

import { GitManagerPanel } from "./GitManagerPanel";

const projectRef = {
  environmentId: "environment-1",
  projectId: "project-1",
} as never;

function config(
  gitManagerReads: boolean,
  overrides: Parameters<typeof makeTestExecutionEnvironmentCapabilities>[0] = {},
): ServerConfig {
  return {
    environment: {
      capabilities: makeTestExecutionEnvironmentCapabilities({
        gitManagerReads,
        gitManagerBranchSyncOperations: true,
        gitManagerStashMergeOperations: true,
        gitManagerRewriteOperations: true,
        gitManagerTagOperations: true,
        gitManagerLiveSignal: true,
        gitManagerPullRequests: true,
        ...overrides,
      }),
    },
  } as ServerConfig;
}

function connection(overrides: Partial<SupervisorConnectionState>): SupervisorConnectionState {
  return { ...AVAILABLE_CONNECTION_STATE, ...overrides };
}

function renderPanel(): string {
  h.effects.length = 0;
  h.queryAtoms.length = 0;
  h.toolbarProps.length = 0;
  h.historyProps.length = 0;
  h.tagsViewProps.length = 0;
  return renderToStaticMarkup(<GitManagerPanel projectRef={projectRef} />);
}

function refsSnapshot(overrides: Partial<GitManagerRefsSnapshot> = {}): GitManagerRefsSnapshot {
  return {
    generation: 1,
    headRef: "main",
    detachedSha: null,
    isDirty: false,
    defaultBranch: "main",
    remotes: ["origin"],
    localBranches: [
      {
        name: "main",
        tipSha: "a".repeat(40),
        upstream: "origin/main",
        ahead: 1,
        behind: 0,
        current: true,
        isDefault: true,
        worktreePath: "/opaque/main",
        blocked: [],
      },
      {
        name: "feature/base",
        tipSha: "f".repeat(40),
        upstream: null,
        ahead: 0,
        behind: 0,
        current: false,
        isDefault: false,
        worktreePath: null,
        blocked: [],
      },
    ],
    remoteBranches: [],
    tags: [],
    worktrees: [],
    inProgressOperation: null,
    conflictedPaths: [],
    ...overrides,
  };
}

function vcsStatus(hasWorkingTreeChanges: boolean): VcsStatusResult {
  return {
    isRepo: true,
    sourceControlProvider: { kind: "github", name: "GitHub", baseUrl: "https://github.com" },
    hasPrimaryRemote: true,
    isDefaultRef: true,
    refName: "main",
    defaultRefName: "main",
    hasWorkingTreeChanges,
    workingTree: { files: [], insertions: 0, deletions: 0 },
    hasUpstream: true,
    aheadCount: 0,
    behindCount: 0,
    pr: null,
  } as VcsStatusResult;
}

beforeEach(() => {
  h.connectionState = connection({
    desired: true,
    phase: "connected",
    network: "online",
  });
  h.serverConfig = config(true);
  h.project = {
    id: "project-1",
    environmentId: "environment-1",
    title: "Repository",
    workspaceRoot: "/opaque/main",
  };
  h.catalogPending = false;
  h.catalog = {
    worktrees: [
      { path: "/opaque/main", branch: "main", isPrimary: true },
      { path: "/opaque/worktree", branch: "feature", isPrimary: false },
    ],
  };
  h.activeSubscriptions = 0;
  h.refsSnapshot = refsSnapshot();
  h.status = vcsStatus(false);
  h.statusError = null;
  h.stashes = null;
  h.refreshed.length = 0;
  h.signalGeneration = 1;
  h.catalogAtom.mockClear();
  h.signalAtom.mockClear();
  h.refsAtom.mockClear();
  h.stashesAtom.mockClear();
  h.statusAtom.mockClear();
  h.listPullRequests.mockClear();
  h.runOperation.mockClear();
  h.historyProps.length = 0;
  h.tagsViewProps.length = 0;
  h.branchDialogProps.length = 0;
  h.historyTagDialogProps.length = 0;
  useGitManagerStore.setState({ byProjectKey: {} });
});

describe("GitManagerPanel", () => {
  it("reports catalog loading only until the first live snapshot", () => {
    h.catalogPending = true;
    renderPanel();
    expect(h.toolbarProps.at(-1)).toMatchObject({ catalogPending: false });
    h.catalog = null;
    renderPanel();
    expect(h.toolbarProps.at(-1)).toMatchObject({ catalogPending: true });
  });

  it("renders the disconnected reason and starts no RPC-backed atom", () => {
    h.connectionState = connection({
      desired: false,
      phase: "available",
      network: "online",
    });

    const markup = renderPanel();

    expect(markup).toContain("This environment is disconnected.");
    expect(h.catalogAtom).not.toHaveBeenCalled();
    expect(h.signalAtom).not.toHaveBeenCalled();
    expect(h.queryAtoms).toEqual([null, null, null]);
  });

  it("renders tabs and targets the selected checkout only while ready", () => {
    useGitManagerStore
      .getState()
      .setSelectedWorktree(
        { environmentId: "environment-1", projectId: "different-project" } as never,
        "/opaque/different-worktree",
      );
    const markup = renderPanel();

    expect(markup).toContain("Changes");
    expect(markup).toContain("History");
    expect(markup).toContain("Tags");
    expect(h.catalogAtom).toHaveBeenCalledTimes(1);
    expect(h.signalAtom).toHaveBeenCalledWith(
      expect.objectContaining({ input: { cwd: "/opaque/main" } }),
    );
    expect(h.toolbarProps[0]).toMatchObject({
      mainCheckoutCwd: "/opaque/main",
      selectedWorktreeCwd: "/opaque/main",
    });
    (h.toolbarProps[0]?.onSelectedWorktreeChange as ((cwd: string) => void) | undefined)?.(
      "/opaque/worktree",
    );
    expect(useGitManagerStore.getState().selectViewState(projectRef).selectedWorktreeCwd).toBe(
      "/opaque/worktree",
    );
  });

  it.each([
    [
      "gitManagerBranchSyncOperations",
      "This environment does not support Git Manager branch and sync operations.",
    ],
    [
      "gitManagerStashMergeOperations",
      "This environment does not support Git Manager stash and merge operations.",
    ],
    [
      "gitManagerRewriteOperations",
      "This environment does not support Git Manager rewrite operations.",
    ],
    ["gitManagerTagOperations", "This environment does not support Git Manager tag operations."],
    [
      "gitManagerPullRequests",
      "This environment does not support Git Manager pull request operations.",
    ],
  ] as const)("degrades only the %s surface and renders its reason", (capability, reason) => {
    h.serverConfig = config(true, { [capability]: false });

    const markup = renderPanel();

    expect(markup).toContain(reason);
    expect(markup).toContain("Changes");
    expect(markup).toContain("History");
    expect(markup).not.toContain("Git Manager Unavailable");
    expect(h.refsAtom).toHaveBeenCalled();
  });

  it("uses explicit reads without a live subscription or timer when live updates are unsupported", () => {
    const setIntervalSpy = vi.spyOn(window, "setInterval");
    const setTimeoutSpy = vi.spyOn(window, "setTimeout");
    h.serverConfig = config(true, { gitManagerLiveSignal: false });

    try {
      const markup = renderPanel();

      expect(markup).toContain(
        "This environment does not support Git Manager live updates. Use Refresh to load new repository data.",
      );
      expect(markup).toContain("Changes");
      expect(markup).toContain("History");
      expect(h.signalAtom).not.toHaveBeenCalled();
      expect(h.refsAtom).toHaveBeenCalled();
      expect(setIntervalSpy).not.toHaveBeenCalled();
      expect(setTimeoutSpy).not.toHaveBeenCalled();
    } finally {
      setIntervalSpy.mockRestore();
      setTimeoutSpy.mockRestore();
    }
  });

  it("releases every server subscription when the panel unmounts", () => {
    renderPanel();
    const expectedSubscriptions = h.queryAtoms.filter((atom) => atom !== null).length;
    const cleanups = h.effects.map((effect) => effect()).filter((value) => value !== undefined);

    expect(h.activeSubscriptions).toBe(expectedSubscriptions);
    for (const cleanup of cleanups) cleanup?.();
    expect(h.activeSubscriptions).toBe(0);
  });

  it("does not mount the provider pane while it is disabled", () => {
    expect(renderPanel()).not.toContain("Pull requests and checks");
    expect(h.listPullRequests).not.toHaveBeenCalled();
  });

  it.each([
    [
      "a GitHub host",
      { kind: "github", name: "Forge", baseUrl: "https://forge.invalid" },
      "pull requests",
      "Pull requests",
    ],
    [
      "a GitLab host",
      { kind: "gitlab", name: "Forge", baseUrl: "https://forge.invalid" },
      "merge requests",
      "Merge requests",
    ],
    ["a status without a provider", undefined, "pull requests", "Pull requests"],
  ] as const)(
    "toggles the provider pane for %s using its vocabulary without requesting data",
    async (_label, sourceControlProvider, plural, title) => {
      (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
      h.status = { ...vcsStatus(false), sourceControlProvider };
      const container = document.createElement("div");
      document.body.append(container);
      const root = createRoot(container);

      try {
        await act(async () => root.render(<GitManagerPanel projectRef={projectRef} />));

        const toggle = container.querySelector<HTMLButtonElement>(
          `button[aria-label="Show ${plural} and checks"]`,
        );
        expect(toggle?.textContent).toBe(`Show ${plural}`);
        expect(container.querySelector(`section[aria-label="${title} and checks"]`)).toBeNull();
        expect(h.listPullRequests).not.toHaveBeenCalled();
        await act(async () => toggle!.click());

        expect(toggle?.textContent).toBe(`Hide ${plural}`);
        expect(toggle?.getAttribute("aria-label")).toBe(`Hide ${plural} and checks`);
        expect(container.textContent).toContain(`${title} and checks`);
        expect(container.textContent).toContain("load only when you choose Refresh");
        expect(h.listPullRequests).not.toHaveBeenCalled();

        const refresh = [...container.querySelectorAll<HTMLButtonElement>("button")].find(
          (button) => button.textContent?.includes("Refresh"),
        );
        expect(refresh).toBeDefined();
        await act(async () => refresh?.click());

        expect(h.listPullRequests).toHaveBeenCalledOnce();
        await act(async () => toggle!.click());
        expect(toggle?.textContent).toBe(`Show ${plural}`);
        expect(container.querySelector(`section[aria-label="${title} and checks"]`)).toBeNull();
        expect(h.listPullRequests).toHaveBeenCalledOnce();
      } finally {
        await act(async () => root.unmount());
        container.remove();
        (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
      }
    },
  );

  it.each([
    ["has not answered", null],
    ["failed", "Git status failed."],
  ] as const)(
    "names requests neutrally in the toggle and the pane while status %s",
    async (_state, statusError) => {
      (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
      h.status = null;
      h.statusError = statusError;
      const container = document.createElement("div");
      document.body.append(container);
      const root = createRoot(container);

      try {
        await act(async () => root.render(<GitManagerPanel projectRef={projectRef} />));

        const toggle = container.querySelector<HTMLButtonElement>(
          'button[aria-label="Show change requests and checks"]',
        );
        expect(toggle?.textContent).toBe("Show change requests");
        await act(async () => toggle!.click());

        expect(toggle?.getAttribute("aria-label")).toBe("Hide change requests and checks");
        expect(toggle?.textContent).toBe("Hide change requests");
        const pane = container.querySelector('section[aria-label="Change requests and checks"]');
        expect(pane?.querySelector("h2")?.textContent).toBe("Change requests and checks");
        expect(pane?.textContent).toContain("Create change request");
        // The host is still unnamed, so no surface in the panel uses a host's noun.
        expect(container.textContent).not.toMatch(/pull requests?|merge requests?/i);
        expect(h.listPullRequests).not.toHaveBeenCalled();
      } finally {
        await act(async () => root.unmount());
        container.remove();
        (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
      }
    },
  );

  it("opens the multi-commit dialog when History chooses an operation", async () => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    useGitManagerStore.getState().setActiveTab(projectRef, "history");
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);

    try {
      await act(async () => root.render(<GitManagerPanel projectRef={projectRef} />));
      const historyProps = h.historyProps.at(-1);
      expect(historyProps).toBeDefined();
      const onAction = historyProps?.onAction;
      expect(onAction).toBeTypeOf("function");

      await act(async () =>
        (onAction as (action: unknown) => void)({
          _tag: "cherry-pick",
          shas: ["b".repeat(40)],
        }),
      );

      expect(document.body.textContent).toContain("Cherry-Pick in Progress");
      expect(h.runOperation).toHaveBeenCalledWith(
        expect.anything(),
        expect.objectContaining({
          input: expect.objectContaining({ _tag: "cherry-pick", shas: ["b".repeat(40)] }),
        }),
        expect.any(Function),
      );
    } finally {
      await act(async () => root.unmount());
      container.remove();
      (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
    }
  });

  it("opens on History, or on Changes while a merge is pending", async () => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    // The React mock collects effects; flush the ones a mount registered.
    const openManager = async () => {
      const container = document.createElement("div");
      document.body.append(container);
      const root = createRoot(container);
      await act(async () => root.render(<GitManagerPanel projectRef={projectRef} />));
      await act(async () => {
        for (const effect of h.effects.splice(0)) effect();
      });
      const tab = useGitManagerStore.getState().selectViewState(projectRef).activeTab;
      await act(async () => root.unmount());
      container.remove();
      return tab;
    };

    try {
      // A tab picked last time does not carry over: the manager opens on History.
      useGitManagerStore.getState().setActiveTab(projectRef, "changes");
      expect(await openManager()).toBe("history");

      h.refsSnapshot = refsSnapshot({
        inProgressOperation: { kind: "merge", current: null, total: null },
        conflictedPaths: ["src/conflicted.bin"],
      });
      expect(await openManager()).toBe("changes");

      h.refsSnapshot = refsSnapshot({
        inProgressOperation: { kind: "rebase", current: 1, total: 2 },
      });
      expect(await openManager()).toBe("history");
    } finally {
      (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
    }
  });

  it("moves from Changes to History when the checkout becomes clean", async () => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    h.effects.length = 0;
    h.status = vcsStatus(true);
    useGitManagerStore.getState().setActiveTab(projectRef, "changes");
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);

    try {
      await act(async () => root.render(<GitManagerPanel projectRef={projectRef} />));
      await act(async () => {
        for (const effect of h.effects.splice(0)) effect();
      });
      expect(useGitManagerStore.getState().selectViewState(projectRef).activeTab).toBe("changes");

      h.status = vcsStatus(false);
      h.signalGeneration = 2;
      await act(async () =>
        root.render(
          <GitManagerPanel
            projectRef={{ environmentId: "environment-1", projectId: "project-1" } as never}
          />,
        ),
      );
      await act(async () => {
        for (const effect of h.effects.splice(0)) effect();
      });

      expect(useGitManagerStore.getState().selectViewState(projectRef).activeTab).toBe("history");
    } finally {
      await act(async () => root.unmount());
      container.remove();
      (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
    }
  });

  it("reads stashes only while the Stashes pane is open, then on each signal change", async () => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    const stash = (index: number): GitManagerStashEntry => ({
      index,
      sha: String(index).repeat(40),
      message: `WIP ${index}`,
      committedAtMs: index,
      parents: [],
      files: [],
    });
    h.effects.length = 0;
    h.stashes = [stash(1), stash(2)];
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);
    const stashButton = () =>
      container.querySelector<HTMLButtonElement>('[aria-label="Toggle repository stashes"]');
    const flushEffects = () =>
      act(async () => {
        for (const effect of h.effects.splice(0)) effect();
      });
    const bumpSignal = async (generation: number) => {
      h.signalGeneration = generation;
      await act(async () =>
        root.render(
          <GitManagerPanel
            projectRef={{ environmentId: "environment-1", projectId: "project-1" } as never}
          />,
        ),
      );
      await flushEffects();
    };

    try {
      await act(async () => root.render(<GitManagerPanel projectRef={projectRef} />));
      await flushEffects();
      // A closed pane reads no stashes and shows no count, even across a signal change.
      expect(useGitManagerStore.getState().selectViewState(projectRef).stashPaneOpen).toBe(false);
      expect(stashButton()?.textContent).toBe("Stashes");
      h.refreshed.length = 0;
      await bumpSignal(2);
      expect(h.refreshed).toEqual(["refs"]);
      expect(h.stashesAtom).not.toHaveBeenCalled();

      // Opening the pane mounts the stash query, which reads the current list;
      // no extra refresh follows. (The React mock runs every effect on each
      // render, so the unguarded refs refresh repeats here; the stash guard
      // compares generations and still holds.)
      h.refreshed.length = 0;
      await act(async () => stashButton()?.click());
      await flushEffects();
      expect(h.stashesAtom).toHaveBeenCalledWith(
        expect.objectContaining({ input: { cwd: "/opaque/main" } }),
      );
      expect(h.refreshed).not.toContain("stashes");
      expect(stashButton()?.textContent).toBe("Stashes (2)");

      // An external `git stash push` bumps the signal while the pane is open.
      h.refreshed.length = 0;
      h.stashes = [stash(3), stash(1), stash(2)];
      await bumpSignal(3);
      expect(h.refreshed).toEqual(expect.arrayContaining(["refs", "stashes"]));
      expect(stashButton()?.textContent).toBe("Stashes (3)");
    } finally {
      await act(async () => root.unmount());
      container.remove();
      (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
    }
  });

  it("does not re-read a just-mounted stash list when the signal (re)subscribes", async () => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    h.effects.length = 0;
    h.stashes = [];
    h.signalGeneration = null;
    useGitManagerStore.getState().setStashPaneOpen(projectRef, true);
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);
    const flushEffects = () =>
      act(async () => {
        for (const effect of h.effects.splice(0)) effect();
      });
    const renderWithSignal = async (generation: number) => {
      h.signalGeneration = generation;
      await act(async () =>
        root.render(
          <GitManagerPanel
            projectRef={{ environmentId: "environment-1", projectId: "project-1" } as never}
          />,
        ),
      );
      await flushEffects();
    };

    try {
      await act(async () => root.render(<GitManagerPanel projectRef={projectRef} />));
      await flushEffects();
      expect(h.stashesAtom).toHaveBeenCalled();
      // The first generation after (re)subscribing is covered by the mount read.
      h.refreshed.length = 0;
      await renderWithSignal(5);
      expect(h.refreshed).not.toContain("stashes");
      // The next generation is a real change and re-reads the open list.
      await renderWithSignal(6);
      expect(h.refreshed).toContain("stashes");
    } finally {
      await act(async () => root.unmount());
      container.remove();
      (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
    }
  });

  it("shows no Stashes count while the pane is closed or stashes are unavailable", () => {
    h.stashes = [];
    expect(renderPanel()).toContain(">Stashes</button>");
    expect(h.stashesAtom).not.toHaveBeenCalled();
    h.serverConfig = config(true, { gitManagerStashMergeOperations: false });
    expect(renderPanel()).not.toContain("Stashes (");
  });

  it("mounts the conflict list for an externally conflicted history operation", async () => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    h.refsSnapshot = refsSnapshot({
      inProgressOperation: { kind: "cherry-pick", current: 1, total: 2 },
      conflictedPaths: ["src/conflicted.bin"],
    });
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);

    try {
      await act(async () => root.render(<GitManagerPanel projectRef={projectRef} />));

      expect(document.body.querySelector('section[aria-label="Conflicted files"]')).not.toBeNull();
      expect(document.body.textContent).toContain("src/conflicted.bin");
      const resolve = document.body.querySelector<HTMLButtonElement>(
        '[aria-label="Resolve src/conflicted.bin with theirs"]',
      );
      expect(resolve).not.toBeNull();
      await act(async () => resolve?.click());
      expect(h.runOperation).toHaveBeenCalledWith(
        expect.anything(),
        expect.objectContaining({
          input: expect.objectContaining({
            _tag: "resolve-conflict",
            path: "src/conflicted.bin",
            side: "theirs",
          }),
        }),
        expect.any(Function),
      );
    } finally {
      await act(async () => root.unmount());
      container.remove();
      (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
    }
  });

  it("leaves an external non-conflicted operation in the existing resumable strip", async () => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    h.refsSnapshot = refsSnapshot({
      inProgressOperation: { kind: "rebase", current: 1, total: 2 },
      conflictedPaths: [],
    });
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);

    try {
      await act(async () => root.render(<GitManagerPanel projectRef={projectRef} />));

      expect(document.body.textContent).toContain("Rebase underway");
      expect(document.body.textContent).not.toContain("Rebase in Progress");
    } finally {
      await act(async () => root.unmount());
      container.remove();
      (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
    }
  });

  it("keeps reset behind a destructive confirmation before dispatching", async () => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    useGitManagerStore.getState().setActiveTab(projectRef, "history");
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);
    const sha = "c".repeat(40);

    try {
      await act(async () => root.render(<GitManagerPanel projectRef={projectRef} />));
      const onAction = h.historyProps.at(-1)?.onAction;
      expect(onAction).toBeTypeOf("function");
      await act(async () => (onAction as (action: unknown) => void)({ _tag: "reset", sha }));

      expect(document.body.textContent).toContain("Reset to ccccccc?");
      expect(h.runOperation).not.toHaveBeenCalled();
      const hardReset = [...document.body.querySelectorAll<HTMLButtonElement>("button")].find(
        (button) => button.textContent === "Discard Changes and Reset",
      );
      expect(hardReset?.className).toContain("destructive");

      await act(async () => hardReset?.click());
      expect(h.runOperation).toHaveBeenCalledWith(
        expect.anything(),
        expect.objectContaining({
          input: expect.objectContaining({ _tag: "reset", mode: "hard", sha }),
        }),
        expect.any(Function),
      );
    } finally {
      await act(async () => root.unmount());
      container.remove();
      (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
    }
  });

  it("routes revert, create-branch, and create-tag intents through existing parent owners", async () => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    useGitManagerStore.getState().setActiveTab(projectRef, "history");
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);
    const sha = "d".repeat(40);

    try {
      await act(async () => root.render(<GitManagerPanel projectRef={projectRef} />));
      const onAction = h.historyProps.at(-1)?.onAction as (action: unknown) => void;

      await act(async () => onAction({ _tag: "revert", sha }));
      expect(h.runOperation).toHaveBeenCalledWith(
        expect.anything(),
        expect.objectContaining({ input: expect.objectContaining({ _tag: "revert", sha }) }),
        expect.any(Function),
      );

      await act(async () => onAction({ _tag: "create-branch", sha }));
      expect(h.branchDialogProps.at(-1)?.dialog).toEqual({
        kind: "create",
        baseBranch: null,
        baseCommit: sha,
      });

      await act(async () => onAction({ _tag: "create-tag", sha }));
      expect(h.historyTagDialogProps.at(-1)).toMatchObject({
        action: "create",
        open: true,
        targetSha: sha,
      });
    } finally {
      await act(async () => root.unmount());
      container.remove();
      (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
    }
  });

  it("creates a branch from a History commit without checking it out when asked", async () => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    useGitManagerStore.getState().setActiveTab(projectRef, "history");
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);
    const sha = "e".repeat(40);

    try {
      await act(async () => root.render(<GitManagerPanel projectRef={projectRef} />));
      const onAction = h.historyProps.at(-1)?.onAction as (action: unknown) => void;
      await act(async () => onAction({ _tag: "create-branch", sha }));
      const props = h.branchDialogProps.at(-1)!;
      expect(Array.isArray(props.remoteRefs)).toBe(true);
      // The mocked operation never settles, so the submission is not awaited.
      await act(async () => {
        void (props.onSubmit as (submission: unknown) => Promise<void>)({
          kind: "create",
          name: "from-commit",
          startPoint: sha,
          checkout: false,
        });
      });
      expect(h.runOperation).toHaveBeenCalledWith(
        expect.anything(),
        expect.objectContaining({
          input: expect.objectContaining({
            _tag: "branch-create",
            name: "from-commit",
            startPoint: sha,
            checkout: false,
          }),
        }),
        expect.any(Function),
      );
    } finally {
      await act(async () => root.unmount());
      container.remove();
      (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
    }
  });

  it("opens the existing multi-commit chooser from the Rebase control", async () => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);

    try {
      await act(async () => root.render(<GitManagerPanel projectRef={projectRef} />));
      const rebase = [...container.querySelectorAll<HTMLButtonElement>("button")].find(
        (button) => button.textContent === "Rebase…",
      );
      expect(rebase).toBeDefined();

      await act(async () => rebase?.click());
      expect(document.body.textContent).toContain("Choose a Branch to Rebase");
      const baseBranch = document.body.querySelector<HTMLButtonElement>(
        '[aria-label="Choose branch feature/base"]',
      );
      expect(baseBranch).not.toBeNull();
      await act(async () => baseBranch?.click());
      const confirmRewrite = [...document.body.querySelectorAll<HTMLButtonElement>("button")].find(
        (button) => button.textContent === "Rewrite History",
      );
      expect(confirmRewrite).toBeDefined();
      await act(async () => confirmRewrite?.click());
      expect(h.runOperation).toHaveBeenCalledWith(
        expect.anything(),
        expect.objectContaining({
          input: expect.objectContaining({
            _tag: "rebase",
            base: "feature/base",
            target: "main",
          }),
        }),
        expect.any(Function),
      );
    } finally {
      await act(async () => root.unmount());
      container.remove();
      (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
    }
  });

  it("dispatches squash and drag reorder intents through the multi-commit dialog owner", async () => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    useGitManagerStore.getState().setActiveTab(projectRef, "history");
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);
    const first = "1".repeat(40);
    const second = "2".repeat(40);

    try {
      h.runOperation.mockImplementationOnce(() => ({
        result: Promise.resolve({
          _tag: "Success",
          value: { _tag: "finished", operation: "squash", message: "Squashed." },
        }),
        cancel: vi.fn(),
      }));
      await act(async () => root.render(<GitManagerPanel projectRef={projectRef} />));
      const onAction = h.historyProps.at(-1)?.onAction as (action: unknown) => void;

      await act(async () =>
        onAction({ _tag: "squash", shas: [first, second], message: "Combined change" }),
      );
      expect(document.body.textContent).toContain("Rewrite Squash History?");
      expect(h.runOperation).not.toHaveBeenCalled();
      const confirmSquash = [...document.body.querySelectorAll<HTMLButtonElement>("button")].find(
        (button) => button.textContent === "Rewrite History",
      );
      await act(async () => confirmSquash?.click());
      expect(document.body.textContent).toContain("Squash in Progress");
      expect(h.runOperation).toHaveBeenLastCalledWith(
        expect.anything(),
        expect.objectContaining({
          input: expect.objectContaining({
            _tag: "squash",
            shas: [first, second],
            message: "Combined change",
          }),
        }),
        expect.any(Function),
      );
      await act(async () => onAction({ _tag: "reorder", shas: [second], insertBeforeSha: first }));
      expect(document.body.textContent).toContain("Rewrite Reorder History?");
      const confirmReorder = [...document.body.querySelectorAll<HTMLButtonElement>("button")].find(
        (button) => button.textContent === "Rewrite History",
      );
      await act(async () => confirmReorder?.click());
      expect(document.body.textContent).toContain("Reorder in Progress");
      expect(h.runOperation).toHaveBeenLastCalledWith(
        expect.anything(),
        expect.objectContaining({
          input: expect.objectContaining({
            _tag: "reorder",
            shas: [second],
            insertBeforeSha: first,
          }),
        }),
        expect.any(Function),
      );
    } finally {
      await act(async () => root.unmount());
      container.remove();
      (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
    }
  });
});
