// @vitest-environment happy-dom

import {
  AVAILABLE_CONNECTION_STATE,
  type SupervisorConnectionState,
} from "@coddy/client-runtime/connection";
import type { ServerConfig } from "@coddy/contracts";
import { makeTestExecutionEnvironmentCapabilities } from "@coddy/shared/testSupport";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

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
  catalog: {
    worktrees: [
      { path: "/opaque/main", branch: "main", isPrimary: true },
      { path: "/opaque/worktree", branch: "feature", isPrimary: false },
    ],
  } as Record<string, unknown> | null,
  catalogAtom: vi.fn((args: { input: { projectId: string } }) => ({
    kind: "catalog",
    projectId: args.input.projectId,
  })),
  signalAtom: vi.fn((args: { input: { cwd: string } }) => ({
    kind: "signal",
    cwd: args.input.cwd,
  })),
  toolbarProps: [] as Array<Record<string, unknown>>,
}));

vi.mock("../../state/entities", () => ({
  useProject: () => h.project,
  useServerConfigs: () =>
    new Map(h.serverConfig === null ? [] : [["environment-1", h.serverConfig]]),
}));

vi.mock("../../state/environments", () => ({
  useEnvironmentConnectionState: () => ({ data: h.connectionState }),
  useEnvironment: () => ({ label: "Local" }),
}));

vi.mock("../../state/query", () => ({
  useEnvironmentQuery: (atom: { kind: string } | null) => ({
    data: atom?.kind === "catalog" ? h.catalog : null,
    emission: { _tag: "Initial" },
    error: null,
    isPending: false,
    refresh: () => undefined,
    revalidate: () => undefined,
    requiresRetry: false,
  }),
}));

vi.mock("../../state/worktrees", () => ({
  worktreeEnvironment: { catalog: h.catalogAtom },
}));

vi.mock("../../state/gitManager", () => ({
  gitManagerEnvironment: {
    signalWithDegradedFocusRefresh: h.signalAtom,
    getCommits: () => ({ kind: "commits" }),
    getHistoryFirstPage: () => ({ kind: "commits" }),
    commit: { label: "test:commit" },
    undoCommit: { label: "test:undo-commit" },
    discard: { label: "test:discard" },
  },
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
    return <div data-testid="git-manager-toolbar" />;
  },
}));

import { GitManagerPanel } from "./GitManagerPanel";

const projectRef = {
  environmentId: "environment-1",
  projectId: "project-1",
} as never;

let container: HTMLDivElement;
let root: Root | null;

function config(gitManagerReads: boolean): ServerConfig {
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
      }),
    },
  } as ServerConfig;
}

async function mountPanel(): Promise<void> {
  await act(async () => root?.render(<GitManagerPanel projectRef={projectRef} />));
}

function latestToolbarProps(): Record<string, unknown> {
  const props = h.toolbarProps.at(-1);
  expect(props).toBeDefined();
  return props!;
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  h.connectionState = {
    ...AVAILABLE_CONNECTION_STATE,
    desired: true,
    phase: "connected",
    network: "online",
  };
  h.serverConfig = config(true);
  h.toolbarProps.length = 0;
  h.catalogAtom.mockClear();
  h.signalAtom.mockClear();
  useGitManagerStore.setState({ byProjectKey: {} });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root?.unmount());
  root = null;
  container?.remove();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("GitManagerPanel checkout lifecycle", () => {
  it("opens on the main checkout despite a valid stored worktree for this project", async () => {
    useGitManagerStore.getState().setSelectedWorktree(projectRef, "/opaque/worktree");

    await mountPanel();

    expect(latestToolbarProps().selectedWorktreeCwd).toBe("/opaque/main");
    expect(h.signalAtom).toHaveBeenLastCalledWith(
      expect.objectContaining({ input: { cwd: "/opaque/main" } }),
    );
  });

  it("switches the active checkout for the lifetime of the mounted panel", async () => {
    await mountPanel();

    await act(async () => {
      (latestToolbarProps().onSelectedWorktreeChange as (cwd: string) => void)("/opaque/worktree");
    });

    expect(latestToolbarProps().selectedWorktreeCwd).toBe("/opaque/worktree");
    expect(h.signalAtom).toHaveBeenLastCalledWith(
      expect.objectContaining({ input: { cwd: "/opaque/worktree" } }),
    );
  });

  it("returns to the main checkout after the panel remounts", async () => {
    await mountPanel();
    await act(async () => {
      (latestToolbarProps().onSelectedWorktreeChange as (cwd: string) => void)("/opaque/worktree");
    });
    expect(latestToolbarProps().selectedWorktreeCwd).toBe("/opaque/worktree");

    await act(async () => root?.unmount());
    root = createRoot(container);
    h.toolbarProps.length = 0;
    h.signalAtom.mockClear();
    await mountPanel();

    expect(latestToolbarProps().selectedWorktreeCwd).toBe("/opaque/main");
    expect(h.signalAtom).toHaveBeenLastCalledWith(
      expect.objectContaining({ input: { cwd: "/opaque/main" } }),
    );
  });
});
