// @vitest-environment happy-dom

import {
  AVAILABLE_CONNECTION_STATE,
  type SupervisorConnectionState,
} from "@coddy/client-runtime/connection";
import type {
  GitManagerRefsSnapshot,
  ServerConfig,
  VcsStatusResult,
  VcsStatusStreamEvent,
} from "@coddy/contracts";
import { applyGitStatusStreamEvent } from "@coddy/shared/git";
import { makeTestExecutionEnvironmentCapabilities } from "@coddy/shared/testSupport";
import * as AsyncResult from "effect/reactivity/AsyncResult";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import { useGitManagerStore } from "../../gitManagerStore";
import type { GitManagerChangesViewProps } from "./changes/GitManagerChangesView";

// Queries publish through a tiny external store, so a new value re-renders only the
// components that read it, as a live atom subscription does.
const h = vi.hoisted(() => {
  const listeners = new Set<() => void>();
  let version = 0;
  return {
    statusByCwd: new Map<string, VcsStatusResult | null>(),
    refs: null as GitManagerRefsSnapshot | null,
    connection: null as SupervisorConnectionState | null,
    realChangesView: false,
    refreshStatus: vi.fn(),
    /** The latest toolbar props, whose callback switches the checkout. */
    toolbar: null as { onSelectedWorktreeChange: (cwd: string) => void } | null,
    subscribe: (listener: () => void) => {
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
    noop: () => undefined,
  };
});

vi.mock("../../state/query", async () => {
  const { useSyncExternalStore } = await import("react");
  return {
    useEnvironmentQuery: (atom: { kind: string; cwd?: string } | null) => {
      useSyncExternalStore(h.subscribe, h.version);
      const data =
        atom?.kind === "catalog"
          ? {
              worktrees: [
                { path: "/opaque/main", branch: "main", isPrimary: true },
                { path: "/opaque/feature", branch: "feature", isPrimary: false },
              ],
            }
          : atom?.kind === "status"
            ? (h.statusByCwd.get(atom.cwd!) ?? null)
            : atom?.kind === "refs"
              ? h.refs
              : atom?.kind === "signal"
                ? { generation: 1 }
                : null;
      return {
        data,
        emission: data === null ? { _tag: "Initial" } : { _tag: "Success", value: data },
        error: null,
        isPending: false,
        refresh: h.noop,
        revalidate: h.noop,
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
          environment: {
            capabilities: makeTestExecutionEnvironmentCapabilities({
              gitManagerReads: true,
              gitManagerBranchSyncOperations: true,
              gitManagerStashMergeOperations: true,
              gitManagerRewriteOperations: true,
              gitManagerTagOperations: true,
              gitManagerLiveSignal: true,
              gitManagerPullRequests: true,
            }),
          },
        } as ServerConfig,
      ],
    ]),
}));

vi.mock("../../state/environments", async () => {
  const { useSyncExternalStore } = await import("react");
  return {
    useEnvironmentConnectionState: () => {
      useSyncExternalStore(h.subscribe, h.version);
      return { data: h.connection };
    },
    useEnvironment: () => ({ label: "Local" }),
  };
});

vi.mock("../../state/worktrees", () => ({
  worktreeEnvironment: { catalog: () => ({ kind: "catalog" }) },
}));

vi.mock("../../state/vcs", () => ({
  vcsEnvironment: {
    status: ({ input }: { input: { cwd: string } }) => ({ kind: "status", cwd: input.cwd }),
    refreshStatus: "cmd:refresh-status",
  },
}));

vi.mock("../../state/use-atom-command", () => ({
  useAtomCommand: (command: unknown) =>
    command === "cmd:refresh-status" ? h.refreshStatus : h.noop,
}));
vi.mock("../../editorPreferences", () => ({ useOpenInPreferredEditor: () => h.noop }));

vi.mock("../../state/gitManager", () => ({
  gitManagerEnvironment: {
    signalWithDegradedFocusRefresh: () => ({ kind: "signal" }),
    getRefs: () => ({ kind: "refs" }),
    getStashes: () => ({ kind: "stashes" }),
  },
  runGitManagerOperation: vi.fn(),
}));

vi.mock("./GitManagerToolbar", () => ({
  GitManagerToolbar: (props: { onSelectedWorktreeChange: (cwd: string) => void }) => {
    h.toolbar = props;
    return null;
  },
}));
vi.mock("./changes/GitManagerChangesView", async (importOriginal) => {
  const { GitManagerChangesView } =
    await importOriginal<typeof import("./changes/GitManagerChangesView")>();
  return {
    GitManagerChangesView: (props: GitManagerChangesViewProps) =>
      h.realChangesView ? <GitManagerChangesView {...props} /> : <div data-testid="changes-view" />,
  };
});
vi.mock("./history/GitManagerHistoryView", () => ({
  GitManagerHistoryView: () => <div data-testid="history-view" />,
}));
vi.mock("./tags/GitManagerTagsView", () => ({
  GitManagerTagsView: () => <div data-testid="tags-view" />,
}));
vi.mock("./dialogs/GitManagerBranchDialogs", () => ({ GitManagerBranchDialogs: () => null }));
vi.mock("./tags/GitManagerTagDialog", () => ({ GitManagerTagDialog: () => null }));
vi.mock("./merge/GitManagerMergeDialog", () => ({ GitManagerMergeDialog: () => null }));
vi.mock("./rewrite/GitManagerResetDialog", () => ({ GitManagerResetDialog: () => null }));

import { GitManagerPanel } from "./GitManagerPanel";

const projectRef = { environmentId: "environment-1", projectId: "project-1" } as never;

// Status frames recorded from a live server while `.git/HEAD` was broken ("not a ref") and
// then repaired; a malformed `.git/config` produced the same unreadable frame.
const DIRTY_LOCAL = {
  defaultRefName: "main",
  hasPrimaryRemote: false,
  hasWorkingTreeChanges: true,
  isDefaultRef: true,
  isRepo: true,
  refName: "main",
  workingTree: {
    deletions: 0,
    files: [
      { area: "unstaged", deletions: 0, insertions: 1, path: "a.txt", status: "modified" },
      { area: "untracked", deletions: 0, insertions: 1, path: "c.txt", status: "untracked" },
    ],
    insertions: 2,
  },
} as const;
const CLEAN_LOCAL = {
  ...DIRTY_LOCAL,
  hasWorkingTreeChanges: false,
  workingTree: { deletions: 0, files: [], insertions: 0 },
} as const;
const REMOTE = {
  aheadCount: 0,
  aheadOfDefaultCount: 0,
  behindCount: 0,
  hasUpstream: false,
  pr: null,
} as const;
const UNREADABLE: ReadonlyArray<VcsStatusStreamEvent> = [
  {
    _tag: "localUpdated",
    local: {
      hasPrimaryRemote: false,
      hasWorkingTreeChanges: false,
      isDefaultRef: false,
      isRepo: false,
      refName: null,
      workingTree: { deletions: 0, files: [], insertions: 0 },
    },
  },
  { _tag: "remoteUpdated", remote: null },
];
const recovered = (local: typeof DIRTY_LOCAL | typeof CLEAN_LOCAL): VcsStatusStreamEvent[] => [
  { _tag: "localUpdated", local },
  { _tag: "remoteUpdated", remote: REMOTE },
];

const REFS: GitManagerRefsSnapshot = {
  conflictedPaths: [],
  defaultBranch: "main",
  detachedSha: null,
  generation: 1,
  headRef: "main",
  inProgressOperation: null,
  isDirty: true,
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
      worktreePath: "/opaque/main",
    },
  ],
  remoteBranches: [],
  remotes: [],
  tags: [],
  worktrees: [],
};
const MERGING_REFS: GitManagerRefsSnapshot = {
  ...REFS,
  inProgressOperation: { kind: "merge", current: null, total: null },
};

let container: HTMLDivElement;
let root: Root;

function deferredRead() {
  let resolve!: (result: unknown) => void;
  const promise = new Promise<unknown>((settle) => {
    resolve = settle;
  });
  return { promise, resolve };
}

/** Applies each frame as its own emission, the way the status stream delivers them. */
async function deliver(
  frames: ReadonlyArray<VcsStatusStreamEvent>,
  cwd = "/opaque/main",
): Promise<void> {
  for (const frame of frames) {
    await act(async () => {
      h.statusByCwd.set(cwd, applyGitStatusStreamEvent(h.statusByCwd.get(cwd) ?? null, frame));
      h.publish();
    });
  }
}

function tab(name: "Changes" | "History" | "Tags"): HTMLElement {
  const match = [...container.querySelectorAll<HTMLElement>('[role="tab"]')].find(
    (element) => element.textContent?.trim() === name,
  );
  expect(match).toBeDefined();
  return match!;
}

function selectedTab(): string | null {
  return container.querySelector('[role="tab"][aria-selected="true"]')?.textContent?.trim() ?? null;
}

function retryButton(): HTMLButtonElement {
  const button = container.querySelector<HTMLButtonElement>('[role="alert"] button');
  if (button === null) throw new Error("Missing Retry button");
  return button;
}

function isInactive(button: HTMLButtonElement): boolean {
  return button.getAttribute("aria-disabled") === "true";
}

async function openManagerOn(name: "Changes" | "History" | "Tags"): Promise<void> {
  await act(async () => root.render(<GitManagerPanel projectRef={projectRef} />));
  await act(async () => tab(name).click());
  expect(selectedTab()).toBe(name);
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  useGitManagerStore.setState({ byProjectKey: {} });
  h.statusByCwd.clear();
  h.realChangesView = false;
  h.refreshStatus.mockReset();
  h.refs = REFS;
  h.connection = {
    ...AVAILABLE_CONNECTION_STATE,
    desired: true,
    phase: "connected",
    network: "online",
  };
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("GitManagerPanel selected tab", () => {
  it("keeps each checkout's Retry busy across checkout and tab switches", async () => {
    h.realChangesView = true;
    const readA = deferredRead();
    const readB = deferredRead();
    h.refreshStatus.mockReturnValueOnce(readA.promise).mockReturnValueOnce(readB.promise);
    await deliver(UNREADABLE);
    await deliver(UNREADABLE, "/opaque/feature");
    await openManagerOn("Changes");

    expect(retryButton().textContent).toBe("Retry");
    await act(async () => retryButton().click());
    expect(retryButton().textContent).toBe("Retrying…");
    expect(isInactive(retryButton())).toBe(true);

    await act(async () => h.toolbar?.onSelectedWorktreeChange("/opaque/feature"));
    expect(retryButton().textContent).toBe("Retry");
    expect(isInactive(retryButton())).toBe(false);
    await act(async () => retryButton().click());
    expect(retryButton().textContent).toBe("Retrying…");
    expect(h.refreshStatus).toHaveBeenNthCalledWith(1, {
      environmentId: "environment-1",
      input: { cwd: "/opaque/main" },
    });
    expect(h.refreshStatus).toHaveBeenNthCalledWith(2, {
      environmentId: "environment-1",
      input: { cwd: "/opaque/feature" },
    });

    await act(async () => h.toolbar?.onSelectedWorktreeChange("/opaque/main"));
    expect(retryButton().textContent).toBe("Retrying…");
    expect(isInactive(retryButton())).toBe(true);

    await act(async () => tab("History").click());
    expect(container.querySelector('[role="alert"]')?.textContent).toContain(
      "Could not load history",
    );
    expect(retryButton().textContent).toBe("Retrying…");
    await act(async () => tab("Changes").click());
    expect(retryButton().textContent).toBe("Retrying…");
    expect(isInactive(retryButton())).toBe(true);

    await act(async () => readB.resolve(AsyncResult.success(h.statusByCwd.get("/opaque/feature"))));
    expect(retryButton().textContent).toBe("Retrying…");
    await act(async () => readA.resolve(AsyncResult.success(h.statusByCwd.get("/opaque/main"))));
    expect(retryButton().textContent).toBe("Retry");
    expect(isInactive(retryButton())).toBe(false);

    await act(async () => h.toolbar?.onSelectedWorktreeChange("/opaque/feature"));
    expect(retryButton().textContent).toBe("Retry");
    expect(h.refreshStatus).toHaveBeenCalledTimes(2);
  });

  it("keeps Retry busy when Changes unmounts for History and mounts again", async () => {
    h.realChangesView = true;
    const read = deferredRead();
    h.refreshStatus.mockReturnValueOnce(read.promise);
    await deliver(UNREADABLE);
    await openManagerOn("Changes");
    await act(async () => retryButton().click());
    expect(retryButton().textContent).toBe("Retrying…");

    await act(async () => tab("History").click());
    expect(container.querySelector('[role="alert"]')?.textContent).toContain(
      "Could not load history",
    );
    expect(retryButton().textContent).toBe("Retrying…");
    await act(async () => tab("Changes").click());
    expect(retryButton().textContent).toBe("Retrying…");
    expect(isInactive(retryButton())).toBe(true);

    await act(async () => read.resolve(AsyncResult.success(h.statusByCwd.get("/opaque/main"))));
    expect(retryButton().textContent).toBe("Retry");
    expect(isInactive(retryButton())).toBe(false);
    expect(h.refreshStatus).toHaveBeenCalledOnce();
  });

  it("stays on Changes while Git cannot read the repository and after it recovers", async () => {
    await deliver([
      {
        _tag: "snapshot",
        local: DIRTY_LOCAL,
        remote: null,
      },
    ]);
    await openManagerOn("Changes");

    await deliver(UNREADABLE);
    expect(selectedTab()).toBe("Changes");
    expect(container.querySelector('[data-testid="changes-view"]')).not.toBeNull();

    await deliver(recovered(DIRTY_LOCAL));
    expect(selectedTab()).toBe("Changes");
    expect(container.querySelector('[data-testid="changes-view"]')).not.toBeNull();
  });

  it("stays on Changes when a clean repository fails and recovers clean", async () => {
    await deliver([
      {
        _tag: "snapshot",
        local: CLEAN_LOCAL,
        remote: REMOTE,
      },
    ]);
    await openManagerOn("Changes");

    await deliver(UNREADABLE);
    expect(selectedTab()).toBe("Changes");

    await deliver(recovered(CLEAN_LOCAL));
    expect(selectedTab()).toBe("Changes");
  });

  it("keeps Changes when it is picked from Tags on a clean checkout", async () => {
    await deliver([
      {
        _tag: "snapshot",
        local: CLEAN_LOCAL,
        remote: REMOTE,
      },
    ]);
    await openManagerOn("Tags");

    await act(async () => tab("Changes").click());

    expect(selectedTab()).toBe("Changes");
  });

  it("underlines the selected tab through the data-active attribute Base UI sets", async () => {
    await deliver([
      {
        _tag: "snapshot",
        local: DIRTY_LOCAL,
        remote: null,
      },
    ]);
    await openManagerOn("Changes");

    expect(tab("Changes").hasAttribute("data-active")).toBe(true);
    expect(tab("History").hasAttribute("data-active")).toBe(false);
    expect(tab("Changes").className).toMatch(/(^|\s)data-active:border-foreground(\s|$)/);
    expect(tab("Changes").className).not.toMatch(/data-selected:/);
  });

  it("treats another checkout like opening the panel on it", async () => {
    await deliver([
      {
        _tag: "snapshot",
        local: CLEAN_LOCAL,
        remote: REMOTE,
      },
    ]);
    await deliver(recovered(CLEAN_LOCAL), "/opaque/feature");
    // A pick on a clean checkout stays; moving to another clean checkout starts over.
    await openManagerOn("Changes");

    await act(async () => h.toolbar?.onSelectedWorktreeChange("/opaque/feature"));

    expect(selectedTab()).toBe("History");
  });

  it("still moves from Changes to History when a dirty checkout becomes clean", async () => {
    await deliver([
      {
        _tag: "snapshot",
        local: DIRTY_LOCAL,
        remote: null,
      },
    ]);
    await openManagerOn("Changes");

    await deliver([{ _tag: "localUpdated", local: CLEAN_LOCAL }]);

    expect(selectedTab()).toBe("History");
  });

  it("keeps hand-picked Changes on a clean checkout across reconnecting", async () => {
    await deliver(recovered(CLEAN_LOCAL));
    await openManagerOn("Changes");

    await act(async () => {
      h.connection = { ...h.connection!, phase: "backoff" };
      h.publish();
    });
    expect(
      container.querySelector('[data-testid="git-manager-unavailable"]')?.textContent,
    ).toContain("Reconnecting to Local");
    expect(selectedTab()).toBeNull();

    await act(async () => {
      h.connection = { ...h.connection!, phase: "connected" };
      h.publish();
    });

    expect(selectedTab()).toBe("Changes");
  });

  it("selects History when the whole panel is unmounted and opened again on a clean checkout", async () => {
    await deliver(recovered(CLEAN_LOCAL));
    await openManagerOn("Changes");

    await act(async () => root.unmount());
    root = createRoot(container);
    await act(async () => root.render(<GitManagerPanel projectRef={projectRef} />));

    expect(selectedTab()).toBe("History");
  });

  it("keeps hand-picked Changes when status reloads after reconnecting", async () => {
    await deliver(recovered(CLEAN_LOCAL));
    await openManagerOn("Changes");

    await act(async () => {
      h.connection = { ...h.connection!, phase: "backoff" };
      h.statusByCwd.set("/opaque/main", null);
      h.publish();
    });
    expect(container.querySelector('[data-testid="git-manager-unavailable"]')).not.toBeNull();

    await act(async () => {
      h.connection = { ...h.connection!, phase: "connected" };
      h.publish();
    });
    expect(selectedTab()).toBe("Changes");

    await deliver(recovered(CLEAN_LOCAL));
    expect(selectedTab()).toBe("Changes");
  });

  it("keeps hand-picked Changes while a mounted status subscription reloads", async () => {
    await deliver(recovered(CLEAN_LOCAL));
    await openManagerOn("Changes");

    await act(async () => {
      h.statusByCwd.set("/opaque/main", null);
      h.publish();
    });
    expect(selectedTab()).toBe("Changes");

    await deliver(recovered(CLEAN_LOCAL));
    expect(selectedTab()).toBe("Changes");
  });

  it.each([false, true])(
    "keeps Changes when refs reload after status on reconnect (merge pending: %s)",
    async (mergePending) => {
      const refs = mergePending ? MERGING_REFS : REFS;
      h.refs = refs;
      await deliver(recovered(CLEAN_LOCAL));
      await openManagerOn("Changes");

      await act(async () => {
        h.connection = { ...h.connection!, phase: "backoff" };
        h.statusByCwd.set("/opaque/main", null);
        h.refs = null;
        h.publish();
      });
      expect(container.querySelector('[data-testid="git-manager-unavailable"]')).not.toBeNull();

      await act(async () => {
        h.connection = { ...h.connection!, phase: "connected" };
        h.publish();
      });
      expect(selectedTab()).toBe("Changes");

      await deliver(recovered(CLEAN_LOCAL));
      expect(selectedTab()).toBe("Changes");

      await act(async () => {
        h.refs = refs;
        h.publish();
      });
      expect(selectedTab()).toBe("Changes");
    },
  );

  it.each(["History", "Tags"] as const)(
    "keeps a manual pick of %s when a pending merge's refs reload after reconnecting",
    async (name) => {
      h.refs = MERGING_REFS;
      await deliver(recovered(CLEAN_LOCAL));
      await openManagerOn(name);

      await act(async () => {
        h.connection = { ...h.connection!, phase: "backoff" };
        h.refs = null;
        h.publish();
      });
      expect(selectedTab()).toBeNull();

      await act(async () => {
        h.connection = { ...h.connection!, phase: "connected" };
        h.publish();
      });
      expect(selectedTab()).toBe(name);

      await act(async () => {
        h.refs = MERGING_REFS;
        h.publish();
      });
      expect(selectedTab()).toBe(name);
    },
  );

  it.each(["status", "refs"] as const)(
    "still selects History on a clean opening when %s loads first",
    async (first) => {
      h.refs = null;
      await openManagerOn("Changes");
      const loadStatus = () => deliver(recovered(CLEAN_LOCAL));
      const loadRefs = () =>
        act(async () => {
          h.refs = REFS;
          h.publish();
        });

      await (first === "status" ? loadStatus() : loadRefs());
      await (first === "status" ? loadRefs() : loadStatus());

      expect(selectedTab()).toBe("History");
    },
  );

  it.each(["status", "refs"] as const)(
    "still selects Changes on opening with a pending merge when %s loads first",
    async (first) => {
      h.refs = null;
      await openManagerOn("History");
      const loadStatus = () => deliver(recovered(CLEAN_LOCAL));
      const loadRefs = () =>
        act(async () => {
          h.refs = MERGING_REFS;
          h.publish();
        });

      await (first === "status" ? loadStatus() : loadRefs());
      await (first === "status" ? loadRefs() : loadStatus());

      expect(selectedTab()).toBe("Changes");
    },
  );

  it("keeps hand-picked Changes when switching to a dirty checkout until that checkout becomes clean", async () => {
    await deliver(recovered(CLEAN_LOCAL));
    await deliver(recovered(DIRTY_LOCAL), "/opaque/feature");
    await act(async () => root.render(<GitManagerPanel projectRef={projectRef} />));
    expect(selectedTab()).toBe("History");
    await act(async () => tab("Changes").click());

    await act(async () => h.toolbar?.onSelectedWorktreeChange("/opaque/feature"));
    expect(selectedTab()).toBe("Changes");

    await deliver(recovered(CLEAN_LOCAL), "/opaque/feature");
    expect(selectedTab()).toBe("History");
  });
});
