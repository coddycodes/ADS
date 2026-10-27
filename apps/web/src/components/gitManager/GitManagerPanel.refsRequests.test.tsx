// @vitest-environment happy-dom

import { AVAILABLE_CONNECTION_STATE } from "@coddy/client-runtime/connection";
import type { GitManagerRefsSnapshot, ScopedProjectRef } from "@coddy/contracts";
import { makeTestExecutionEnvironmentCapabilities } from "@coddy/shared/testSupport";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import { useGitManagerStore } from "../../gitManagerStore";

type QueryKind = "catalog" | "status" | "refs" | "signal" | "commits" | "stashes" | "empty";
interface TestQuery {
  readonly kind: QueryKind;
  readonly cwd: string;
  data: unknown;
  isPending: boolean;
  readonly refresh: ReturnType<typeof vi.fn>;
  readonly revalidate: ReturnType<typeof vi.fn>;
  readonly subscribe: (listener: () => void) => () => void;
  readonly version: () => number;
  readonly publish: () => void;
}

// One store and stable read actions per atom key, shared by all real subscribers.
const h = vi.hoisted(() => {
  const queries = new Map<string, TestQuery>();
  return {
    queries,
    liveSignalAvailable: true,
    noop: () => undefined,
    command: vi.fn(),
    query: (kind: QueryKind, cwd = ""): TestQuery => {
      const key = `${kind}:${cwd}`;
      const existing = queries.get(key);
      if (existing !== undefined) return existing;
      const listeners = new Set<() => void>();
      let version = 0;
      const query: TestQuery = {
        kind,
        cwd,
        data: null,
        isPending: false,
        refresh: vi.fn(),
        revalidate: vi.fn(),
        subscribe: (listener) => {
          listeners.add(listener);
          return () => {
            listeners.delete(listener);
          };
        },
        version: () => version,
        publish: () => {
          version += 1;
          for (const listener of listeners) listener();
        },
      };
      queries.set(key, query);
      return query;
    },
  };
});

vi.mock("../../state/query", async () => {
  const { useSyncExternalStore } = await import("react");
  return {
    useEnvironmentQuery: (atom: TestQuery | null) => {
      const query = atom ?? h.query("empty");
      useSyncExternalStore(query.subscribe, query.version);
      return {
        data: query.data,
        emission:
          query.data === null
            ? { _tag: "Initial", waiting: query.isPending }
            : { _tag: "Success", value: query.data, waiting: query.isPending },
        error: null,
        isPending: query.isPending,
        refresh: query.refresh,
        revalidate: query.revalidate,
        requiresRetry: false,
      };
    },
  };
});

vi.mock("../../state/entities", () => ({
  useProject: () => ({
    id: "project-1",
    environmentId: "environment-1",
    title: "Repository",
    workspaceRoot: "/opaque/main",
  }),
  useServerConfigs: () =>
    new Map([
      [
        "environment-1",
        {
          availableEditors: [],
          environment: {
            capabilities: makeTestExecutionEnvironmentCapabilities({
              gitManagerReads: true,
              gitManagerBranchSyncOperations: true,
              gitManagerStashMergeOperations: true,
              gitManagerRewriteOperations: true,
              gitManagerTagOperations: true,
              gitManagerLiveSignal: h.liveSignalAvailable,
              gitManagerPullRequests: true,
              gitManagerCommitOperations: true,
            }),
          },
        },
      ],
    ]),
}));

vi.mock("../../state/environments", () => ({
  useEnvironmentConnectionState: () => ({
    data: {
      ...AVAILABLE_CONNECTION_STATE,
      desired: true,
      phase: "connected",
      network: "online",
    },
  }),
  useEnvironment: () => ({ label: "Local" }),
  usePrimaryEnvironmentId: () => "environment-1",
}));

vi.mock("../../state/worktrees", () => ({
  worktreeEnvironment: { catalog: () => h.query("catalog") },
}));
vi.mock("../../state/vcs", () => ({
  vcsEnvironment: {
    status: ({ input }: { input: { cwd: string } }) => h.query("status", input.cwd),
    refreshStatus: {},
    stageFiles: {},
    unstageFiles: {},
  },
}));
vi.mock("../../state/gitManager", () => ({
  gitManagerEnvironment: {
    getRefs: ({ input }: { input: { cwd: string } }) => h.query("refs", input.cwd),
    signalWithDegradedFocusRefresh: ({ input }: { input: { cwd: string } }) =>
      h.query("signal", input.cwd),
    getCommits: ({ input }: { input: { cwd: string } }) => h.query("commits", input.cwd),
    getStashes: ({ input }: { input: { cwd: string } }) => h.query("stashes", input.cwd),
    commit: {},
    undoCommit: {},
    discard: {},
  },
  runGitManagerOperation: vi.fn(),
}));
vi.mock("../../state/shell", () => ({ shellEnvironment: { openInEditor: {} } }));
vi.mock("../../state/use-atom-command", () => ({ useAtomCommand: () => h.command }));
vi.mock("../../editorPreferences", () => ({ useOpenInPreferredEditor: () => h.noop }));

vi.mock("./history/GitManagerHistoryView", () => ({
  GitManagerHistoryView: () => <div data-testid="history-view" />,
}));
vi.mock("./changes/GitManagerAgentActivity", () => ({ GitManagerAgentActivity: () => null }));
vi.mock("./changes/GitManagerCommitBox", () => ({ GitManagerCommitBox: () => null }));
vi.mock("./changes/GitManagerDiffPane", () => ({ GitManagerDiffPane: () => null }));
vi.mock("./dialogs/GitManagerBranchDialogs", () => ({ GitManagerBranchDialogs: () => null }));
vi.mock("./dialogs/GitManagerSwitchWithChangesDialog", () => ({
  GitManagerSwitchWithChangesDialog: () => null,
}));
vi.mock("./tags/GitManagerTagDialog", () => ({ GitManagerTagDialog: () => null }));
vi.mock("./merge/GitManagerMergeDialog", () => ({ GitManagerMergeDialog: () => null }));
vi.mock("./rewrite/GitManagerResetDialog", () => ({ GitManagerResetDialog: () => null }));

import { GitManagerPanel } from "./GitManagerPanel";

const MAIN = "/opaque/main";
const FEATURE = "/opaque/feature";
const projectRef = {
  environmentId: "environment-1",
  projectId: "project-1",
} as ScopedProjectRef;
const REFS: GitManagerRefsSnapshot = {
  conflictedPaths: [],
  defaultBranch: "main",
  detachedSha: null,
  generation: 1,
  headRef: "main",
  inProgressOperation: null,
  isDirty: false,
  localBranches: [
    {
      ahead: 0,
      behind: 0,
      blocked: [],
      current: true,
      isDefault: true,
      name: "main",
      tipSha: "7".repeat(40),
      upstream: null,
      worktreePath: MAIN,
    },
  ],
  remoteBranches: [],
  remotes: [],
  tags: [],
  worktrees: [],
};

let container: HTMLDivElement;
let root: Root;

async function publish(kind: QueryKind, cwd: string, next: Partial<TestQuery> = {}): Promise<void> {
  await act(async () => {
    const query = h.query(kind, cwd);
    Object.assign(query, next);
    query.publish();
  });
}

async function signal(generation: number | null, cwd = MAIN): Promise<void> {
  await publish("signal", cwd, { data: generation === null ? null : { cwd, generation } });
}

function expectReads(main: number, feature = 0): void {
  expect(h.query("refs", MAIN).revalidate).toHaveBeenCalledTimes(main);
  expect(h.query("refs", FEATURE).revalidate).toHaveBeenCalledTimes(feature);
  for (const query of h.queries.values()) expect(query.refresh).not.toHaveBeenCalled();
}

async function selectTab(name: "Changes" | "History"): Promise<void> {
  const tab = [...container.querySelectorAll<HTMLElement>('[role="tab"]')].find(
    (element) => element.textContent?.trim() === name,
  );
  expect(tab).toBeDefined();
  await act(async () => tab!.click());
  expect(container.querySelector('[role="tab"][aria-selected="true"]')?.textContent).toBe(name);
  expect(container.querySelector('section[aria-label="Changes"]') !== null).toBe(
    name === "Changes",
  );
}

async function openManagerOn(name: "Changes" | "History"): Promise<void> {
  await act(async () => root.render(<GitManagerPanel projectRef={projectRef} />));
  expect(container.querySelector('[data-testid="git-manager-project"]')?.textContent).toBe(
    "Repository",
  );
  await selectTab(name);
}

async function selectCheckout(name: string): Promise<void> {
  const trigger = container.querySelector<HTMLButtonElement>('[aria-label="Worktree"]');
  expect(trigger).not.toBeNull();
  await act(async () => trigger!.click());
  const option = [...document.querySelectorAll<HTMLElement>('[role="option"]')].find((element) =>
    element.textContent?.includes(name),
  );
  expect(option).toBeDefined();
  await act(async () => option!.click());
  expect(trigger!.textContent).toContain(name);
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  useGitManagerStore.setState({ byProjectKey: {} });
  h.queries.clear();
  h.liveSignalAvailable = true;
  h.query("catalog").data = {
    worktrees: [
      { path: MAIN, branch: "main", isPrimary: true },
      { path: FEATURE, branch: "feature", isPrimary: false },
    ],
  };
  for (const cwd of [MAIN, FEATURE]) {
    h.query("refs", cwd).data = REFS;
    h.query("refs", cwd).isPending = true;
    h.query("commits", cwd).data = { commits: [], hasMore: false };
    h.query("status", cwd).data = {
      isRepo: true,
      hasWorkingTreeChanges: false,
      refName: "main",
      workingTree: { files: [], insertions: 0, deletions: 0 },
    };
  }
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("GitManagerPanel refs requests", () => {
  it.each(["Changes", "History"] as const)(
    "revalidates once per generation step with %s active, preserving the pending mount read",
    async (tab) => {
      await openManagerOn(tab);
      expectReads(0);
      await signal(0);
      expectReads(0);
      await signal(1);
      expectReads(1);
      await signal(2);
      expectReads(2);
      await signal(2);
      expectReads(2);
      await publish("refs", MAIN, { isPending: false });
      expectReads(2);
      await selectTab(tab === "Changes" ? "History" : "Changes");
      expectReads(2);
    },
  );

  it("revalidates once when the first generation finds cached, idle refs", async () => {
    await openManagerOn("Changes");
    await publish("refs", MAIN, { isPending: false });
    expectReads(0);
    await signal(0);
    expectReads(1);
    await signal(0);
    expectReads(1);
    await signal(1);
    expectReads(2);
  });

  it("preserves a read that starts before the first generation arrives", async () => {
    h.query("refs", MAIN).isPending = false;
    await openManagerOn("Changes");
    await publish("refs", MAIN, { isPending: true });
    await signal(0);
    expectReads(0);
    await publish("refs", MAIN, { isPending: false });
    expectReads(0);
    await signal(1);
    expectReads(1);
  });

  it.each([true, false])(
    "starts a new checkout baseline when its refs pending state is %s",
    async (isPending) => {
      await openManagerOn("Changes");
      await signal(0);
      expectReads(0);
      await signal(2);
      expectReads(1);
      await publish("refs", FEATURE, { isPending });
      // Deliver before switching: even an immediately available generation is a new baseline.
      await signal(2, FEATURE);
      expectReads(1);
      await selectCheckout("feature");
      const firstReads = isPending ? 0 : 1;
      expectReads(1, firstReads);
      await signal(3, FEATURE);
      expectReads(1, firstReads + 1);
      await signal(9, MAIN);
      expectReads(1, firstReads + 1);
      await selectCheckout("Main Checkout");
      expectReads(1, firstReads + 1);
      await signal(10);
      expectReads(2, firstReads + 1);
    },
  );

  it.each([true, false])(
    "starts a new baseline after the signal reconnects with refs pending %s",
    async (isPending) => {
      await openManagerOn("Changes");
      await signal(0);
      expectReads(0);
      await signal(1);
      expectReads(1);
      await signal(null);
      expectReads(1);
      await publish("refs", MAIN, { isPending });
      await signal(1);
      const firstReads = isPending ? 1 : 2;
      expectReads(firstReads);
      await signal(2);
      expectReads(firstReads + 1);
    },
  );

  it("revalidates cached refs once when the panel reopens", async () => {
    await openManagerOn("History");
    await signal(0);
    expectReads(0);
    await act(async () => root.render(null));
    await signal(null);
    await publish("refs", MAIN, { isPending: false });
    await openManagerOn("History");
    expectReads(0);
    await signal(0);
    expectReads(1);
    await signal(1);
    expectReads(2);
  });

  it("does not revalidate without the live signal capability", async () => {
    h.liveSignalAvailable = false;
    h.query("refs", MAIN).isPending = false;
    await openManagerOn("Changes");
    expectReads(0);
    for (const generation of [0, 1, 2]) {
      await signal(generation);
      expectReads(0);
    }
    await selectTab("History");
    expectReads(0);
  });
});
