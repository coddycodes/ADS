// @vitest-environment happy-dom

import { AVAILABLE_CONNECTION_STATE } from "@coddy/client-runtime/connection";
import type { GitManagerRefsSnapshot, ScopedProjectRef, VcsStatusResult } from "@coddy/contracts";
import { makeTestExecutionEnvironmentCapabilities } from "@coddy/shared/testSupport";
import * as Cause from "effect/Cause";
import * as RpcClientError from "effect/rpc/RpcClientError";
import * as Socket from "effect/socket/Socket";
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
  error: string | null;
  cause: Cause.Cause<unknown> | null;
  requiresRetry: boolean;
  readonly refresh: ReturnType<typeof vi.fn<() => void>>;
  readonly revalidate: ReturnType<typeof vi.fn<() => void>>;
  readonly subscribe: (listener: () => void) => () => void;
  readonly subscribers: () => number;
  readonly mounts: () => number;
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
      let mounts = 0;
      const query: TestQuery = {
        kind,
        cwd,
        data: null,
        isPending: false,
        error: null,
        cause: null,
        requiresRetry: false,
        refresh: vi.fn(),
        revalidate: vi.fn(),
        subscribe: (listener) => {
          if (listeners.size === 0) mounts += 1;
          listeners.add(listener);
          return () => {
            listeners.delete(listener);
          };
        },
        subscribers: () => listeners.size,
        mounts: () => mounts,
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
  const { useMemo, useSyncExternalStore } = await import("react");
  return {
    useEnvironmentQuery: (atom: TestQuery | null) => {
      const query = atom ?? h.query("empty");
      useSyncExternalStore(query.subscribe, query.version);
      const cause = useMemo(
        () => query.cause ?? (query.error === null ? null : Cause.fail(new Error(query.error))),
        [query.cause, query.error],
      );
      return {
        data: query.data,
        emission:
          cause !== null
            ? { _tag: "Failure", cause, waiting: query.isPending }
            : query.data === null
              ? { _tag: "Initial", waiting: query.isPending }
              : { _tag: "Success", value: query.data, waiting: query.isPending },
        error: query.error,
        isPending: query.isPending,
        refresh: query.refresh,
        revalidate: query.revalidate,
        requiresRetry: query.requiresRetry,
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
vi.mock("./changes/GitManagerCommitBox", () => ({
  GitManagerCommitBox: ({ latestCommit }: { latestCommit: unknown }) => (
    <div>{latestCommit === null ? "Undo unavailable" : "Undo ready"}</div>
  ),
}));
vi.mock("./stash/GitManagerStashList", () => ({
  GitManagerStashList: () => <div data-testid="stash-list" />,
}));
vi.mock("./stash/GitManagerStashDiff", () => ({ GitManagerStashDiff: () => null }));
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

function button(label: string): HTMLButtonElement {
  const match = [...container.querySelectorAll<HTMLButtonElement>("button")].find(
    (element) =>
      element.textContent?.trim() === label || element.getAttribute("aria-label") === label,
  );
  if (match === undefined) throw new Error(`Missing button: ${label}`);
  return match;
}

// RetryButton stays focusable while inactive and exposes that state with aria-disabled.
function isInactive(button: HTMLButtonElement): boolean {
  return button.getAttribute("aria-disabled") === "true";
}

async function selectTab(name: "Changes" | "History" | "Tags"): Promise<void> {
  const tab = [...container.querySelectorAll<HTMLElement>('[role="tab"]')].find(
    (element) => element.textContent?.trim() === name,
  );
  expect(tab).toBeDefined();
  await act(async () => tab!.click());
  expect(container.querySelector('[role="tab"][aria-selected="true"]')?.textContent).toBe(name);
}

async function openManager(): Promise<void> {
  await act(async () => root.render(<GitManagerPanel projectRef={projectRef} />));
}

function unavailable(reason?: VcsStatusResult["repositoryUnavailableReason"]) {
  return { ...READABLE, isRepo: false, refName: null, repositoryUnavailableReason: reason };
}

const READABLE = {
  isRepo: true,
  hasWorkingTreeChanges: false,
  refName: "main",
  workingTree: { files: [], insertions: 0, deletions: 0 },
};
const CATALOG_ERROR =
  "Git command failed in GitVcsDriver.worktreeInventory (/opaque/main): fatal: not a git repository (or any of the parent directories): .git";
const UNREADABLE_MESSAGE =
  "Git can't read this repository. Check its .git folder, for example a damaged HEAD or config file.";
const HEDGED_MESSAGE =
  "Git can't read this folder as a repository. Run git init to create one, or check its .git folder if it already is one.";

function expectDisabled(label: string, message: string): void {
  const action = button(label);
  expect(action.disabled).toBe(true);
  expect(action.title).toBe(message);
  const description = action.getAttribute("aria-describedby");
  expect(description).not.toBeNull();
  expect(document.getElementById(description!)?.textContent).toBe(message);
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
  useGitManagerStore.setState({ byProjectKey: {}, toolbarByProjectKey: {} });
  h.queries.clear();
  h.liveSignalAvailable = true;
  h.command.mockReset();
  h.command.mockResolvedValue({ _tag: "Success" });
  h.query("catalog").data = {
    worktrees: [
      { path: MAIN, branch: "main", isPrimary: true },
      { path: FEATURE, branch: "feature", isPrimary: false },
    ],
  };
  for (const cwd of [MAIN, FEATURE]) {
    h.query("refs", cwd).data = REFS;
    h.query("refs", cwd).isPending = false;
    h.query("commits", cwd).data = { commits: [], hasMore: false };
    h.query("status", cwd).data = unavailable("unreadable");
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

describe("GitManagerPanel repository unavailable", () => {
  it.each([
    ["absent", "This folder isn't a Git repository. Run git init to create one."],
    ["unreadable", UNREADABLE_MESSAGE],
    [undefined, HEDGED_MESSAGE],
  ] as const)(
    "describes Worktree with the %s repository message when the catalog fails",
    async (reason, message) => {
      Object.assign(h.query("catalog"), { data: null, error: CATALOG_ERROR });
      h.query("status", MAIN).data = unavailable(reason);
      await openManager();

      expect(document.getElementById("git-manager-worktree-status")?.textContent).toBe(message);
      const trigger = button("Worktree");
      expect(trigger.getAttribute("aria-describedby")).toBe("git-manager-worktree-status");
      expect(trigger.disabled).toBe(false);
      expect(document.body.textContent).not.toContain(CATALOG_ERROR);
    },
  );

  it("keeps the Worktree catalog error when the repository is readable", async () => {
    Object.assign(h.query("catalog"), { data: null, error: CATALOG_ERROR });
    h.query("status", MAIN).data = READABLE;
    await openManager();

    expect(document.getElementById("git-manager-worktree-status")?.textContent).toBe(CATALOG_ERROR);
    expect(button("Worktree").getAttribute("aria-describedby")).toBe("git-manager-worktree-status");
    expect(button("Worktree").disabled).toBe(false);
  });

  it("keeps the Worktree loading description when the catalog has not failed", async () => {
    Object.assign(h.query("catalog"), { data: null, isPending: true });
    await openManager();

    expect(document.getElementById("git-manager-worktree-status")?.textContent).toBe(
      "Loading worktrees…",
    );
    expect(button("Worktree").getAttribute("aria-describedby")).toBe("git-manager-worktree-status");
    expect(button("Worktree").disabled).toBe(false);
  });

  it("keeps Worktree without a status description when the catalog loaded successfully", async () => {
    await openManager();

    expect(document.getElementById("git-manager-worktree-status")).toBeNull();
    expect(button("Worktree").getAttribute("aria-describedby")).toBeNull();
    expect(button("Worktree").disabled).toBe(false);
  });

  it.each([
    ["absent", "This folder isn't a Git repository. Run git init to create one.", "git init"],
    ["unreadable", UNREADABLE_MESSAGE, null],
    [
      "untrusted",
      "Git doesn't trust this repository because another user owns it. Run git config --global --add safe.directory /opaque/main to trust it.",
      "git config --global --add safe.directory /opaque/main",
    ],
    [undefined, HEDGED_MESSAGE, "git init"],
  ] as const)(
    "explains %s across every tab and disables actions despite cached refs",
    async (reason, message, command) => {
      h.query("status", MAIN).data = unavailable(reason);
      await openManager();

      for (const tab of ["Changes", "History", "Tags"] as const) {
        await selectTab(tab);
        const panel = container.querySelector('[role="alert"]');
        expect(panel?.textContent).toContain(`Could not load ${tab.toLowerCase()}`);
        expect(panel?.textContent).toContain(message);
        expect(panel?.querySelector("code")?.textContent ?? null).toBe(command);
        expect(button("Retry").textContent).toBe("Retry");
        expect(container.querySelector('[data-testid="history-view"]')).toBeNull();
        expect(container.querySelector('section[aria-label="Tags"]')).toBeNull();
        expect(container.querySelector('section[aria-label="Changes"]')).toBeNull();
      }
      expect(button("Choose branch").textContent).toBe("No branch");
      expect(container.textContent).not.toContain("Detached HEAD");
      expect(container.textContent).not.toContain("Loading repository state…");
      for (const label of [
        "Choose branch",
        "Tags…",
        "Sync unavailable",
        "Stashes",
        "Merge…",
        "Rebase…",
      ]) {
        expectDisabled(label, message);
      }
      expect(button("Worktree").disabled).toBe(false);
      expect(button("Show pull requests").disabled).toBe(false);
    },
  );

  it("quotes a selected checkout containing spaces in the rendered trust command", async () => {
    const cwd = "/opaque/my checkout";
    h.query("catalog").data = {
      worktrees: [
        { path: MAIN, branch: "main", isPrimary: true },
        { path: cwd, branch: "with-spaces", isPrimary: false },
      ],
    };
    h.query("status", cwd).data = unavailable("untrusted");
    h.query("refs", cwd).data = REFS;
    await openManager();
    await selectCheckout("with-spaces");

    for (const tab of ["Changes", "History", "Tags"] as const) {
      await selectTab(tab);
      expect(container.querySelector('[role="alert"] code')?.textContent).toBe(
        "git config --global --add safe.directory '/opaque/my checkout'",
      );
    }
    expectDisabled(
      "Choose branch",
      "Git doesn't trust this repository because another user owns it. Run git config --global --add safe.directory '/opaque/my checkout' to trust it.",
    );
  });

  it("keeps Retry busy per checkout across all tabs and targets the current checkout", async () => {
    let finish!: () => void;
    h.command.mockReturnValueOnce(
      new Promise<void>((resolve) => {
        finish = resolve;
      }),
    );
    await openManager();
    await act(async () => button("Retry").click());
    expect(h.command).toHaveBeenCalledExactlyOnceWith({
      environmentId: "environment-1",
      input: { cwd: MAIN },
    });
    for (const tab of ["Tags", "Changes", "History"] as const) {
      await selectTab(tab);
      expect(button("Retrying…").textContent).toBe("Retrying…");
      expect(isInactive(button("Retrying…"))).toBe(true);
    }
    await selectCheckout("feature");
    expect(button("Retry").textContent).toBe("Retry");
    expect(isInactive(button("Retry"))).toBe(false);
    await act(async () => button("Retry").click());
    expect(h.command).toHaveBeenLastCalledWith({
      environmentId: "environment-1",
      input: { cwd: FEATURE },
    });
    await selectCheckout("Main Checkout");
    expect(button("Retrying…").textContent).toBe("Retrying…");
    await act(async () => finish());
    expect(button("Retry").textContent).toBe("Retry");
    expect(h.command).toHaveBeenCalledTimes(2);
  });

  it.each(["Changes", "History", "Tags"] as const)(
    "refreshes refs once on repair with %s selected, preserving the tab",
    async (tab) => {
      await openManager();
      await selectTab(tab);
      const refs = h.query("refs", MAIN);
      refs.revalidate.mockClear();

      await publish("status", MAIN, { data: READABLE });
      expect(refs.revalidate).toHaveBeenCalledOnce();
      expect(refs.refresh).not.toHaveBeenCalled();
      expect(container.querySelector('[role="alert"]')).toBeNull();
      expect(container.querySelector('[role="tab"][aria-selected="true"]')?.textContent).toBe(tab);
      expect(button("Choose branch").textContent).toBe("main");
      expect(button("Choose branch").disabled).toBe(false);
      expect(button("Tags…").disabled).toBe(false);
      expect(
        container.querySelector(
          tab === "History" ? '[data-testid="history-view"]' : `section[aria-label="${tab}"]`,
        ),
      ).not.toBeNull();

      await publish("status", MAIN, { data: { ...READABLE } });
      expect(refs.revalidate).toHaveBeenCalledOnce();
      await publish("status", MAIN, { data: unavailable("untrusted") });
      expect(container.querySelector('[role="alert"]')).not.toBeNull();
      await publish("status", MAIN, { data: READABLE });
      expect(refs.revalidate).toHaveBeenCalledTimes(2);
    },
  );

  it("replaces an already mounted History and removes the cached branch when Git fails", async () => {
    h.query("status", MAIN).data = READABLE;
    await openManager();
    expect(container.querySelector('[data-testid="history-view"]')).not.toBeNull();
    await publish("status", MAIN, { data: unavailable("unreadable") });
    expect(container.querySelector('[data-testid="history-view"]')).toBeNull();
    expect(container.querySelector('[role="alert"]')?.textContent).toContain(UNREADABLE_MESSAGE);
    expect(button("Choose branch").textContent).toBe("No branch");
  });

  it("shows unavailable instead of detached or loading when no refs were cached", async () => {
    h.query("refs", MAIN).data = null;
    h.query("refs", MAIN).error = "Git returned malformed repository ref state.";
    await openManager();
    expect(button("Choose branch").textContent).toBe("No branch");
    expectDisabled("Sync unavailable", UNREADABLE_MESSAGE);
    expect(container.textContent).not.toContain("Detached HEAD");
    expect(container.textContent).not.toContain("Loading repository state…");
  });

  it("keeps the current loading presentation until the status stream answers", async () => {
    h.query("status", MAIN).data = null;
    h.query("status", MAIN).isPending = true;
    h.query("refs", MAIN).data = null;
    await openManager();
    expect(container.textContent).toContain("Loading repository state…");
    expect(container.textContent).not.toContain("Sync unavailable");
  });

  it("does not treat switching from a broken checkout to a readable one as a repair", async () => {
    h.query("status", FEATURE).data = READABLE;
    await openManager();
    await selectCheckout("feature");
    expect(h.query("refs", MAIN).revalidate).not.toHaveBeenCalled();
    expect(h.query("refs", FEATURE).revalidate).not.toHaveBeenCalled();
    expect(button("Choose branch").textContent).toBe("main");
  });

  it("coalesces a repair and signal step into one refs reread while stashes remount", async () => {
    h.query("status", MAIN).data = READABLE;
    h.query("stashes", MAIN).data = [];
    await signal(1);
    await openManager();
    await act(async () => button("Stashes").click());
    expect(container.querySelector('[data-testid="stash-list"]')).not.toBeNull();
    const stashes = h.query("stashes", MAIN);
    expect(stashes.mounts()).toBe(1);
    await publish("status", MAIN, { data: unavailable("unreadable") });
    expect(container.querySelector('[data-testid="stash-list"]')).toBeNull();
    expect(stashes.subscribers()).toBe(0);
    expect(
      container.querySelector('[aria-label="Repository stash browser"]')?.textContent,
    ).toContain(UNREADABLE_MESSAGE);
    const refs = h.query("refs", MAIN);
    refs.revalidate.mockClear();
    stashes.revalidate.mockClear();
    await act(async () => {
      h.query("status", MAIN).data = READABLE;
      h.query("status", MAIN).publish();
      h.query("signal", MAIN).data = { generation: 2 };
      h.query("signal", MAIN).publish();
    });
    expect(refs.revalidate).toHaveBeenCalledOnce();
    expect(stashes.mounts()).toBe(2);
    expect(stashes.revalidate).not.toHaveBeenCalled();
    expect(container.querySelector('[data-testid="stash-list"]')).not.toBeNull();
    await publish("status", MAIN, { data: { ...READABLE } });
    expect(refs.revalidate).toHaveBeenCalledOnce();
    expect(stashes.mounts()).toBe(2);
    expect(stashes.revalidate).not.toHaveBeenCalled();
  });

  it("reloads failed refs and the Undo read after repair without displaying the old refs failure", async () => {
    const refs = h.query("refs", MAIN);
    const commits = h.query("commits", MAIN);
    const failure = "Git returned malformed repository ref state.";
    Object.assign(refs, { data: null, error: failure });
    Object.assign(commits, { data: null, error: "Git could not read HEAD." });
    await openManager();
    await selectTab("Changes");
    refs.revalidate.mockImplementation(() => {
      refs.isPending = true;
      refs.publish();
    });

    await publish("status", MAIN, { data: READABLE });
    expect(refs.revalidate).toHaveBeenCalledOnce();
    expect(commits.revalidate).toHaveBeenCalledOnce();
    expect(container.textContent).not.toContain(failure);
    expect(container.textContent).toContain("Connecting to changes…");
    await publish("refs", MAIN, { data: REFS, error: null, isPending: false });
    await publish("commits", MAIN, {
      data: { commits: [{ committedAtMs: Date.now(), parents: ["parent"] }] },
      error: null,
    });
    expect(container.querySelector('section[aria-label="Changes"]')).not.toBeNull();
    expect(container.textContent).toContain("Undo ready");
    expect(button("Choose branch").textContent).toBe("main");
    await publish("status", MAIN, { data: { ...READABLE } });
    expect(refs.revalidate).toHaveBeenCalledOnce();
    expect(commits.revalidate).toHaveBeenCalledOnce();
  });

  it("rereads stashes only on later signal steps while open, with a baseline per checkout", async () => {
    for (const cwd of [MAIN, FEATURE]) {
      h.query("status", cwd).data = READABLE;
      h.query("stashes", cwd).data = [];
    }
    const mainStashes = h.query("stashes", MAIN);
    const featureStashes = h.query("stashes", FEATURE);
    await openManager();
    await signal(0);
    await signal(1);
    expect(mainStashes.mounts()).toBe(0);
    expect(mainStashes.revalidate).not.toHaveBeenCalled();
    await act(async () => button("Toggle repository stashes").click());
    expect(mainStashes.mounts()).toBe(1);
    expect(mainStashes.revalidate).not.toHaveBeenCalled();
    await signal(2);
    await signal(2);
    expect(mainStashes.revalidate).toHaveBeenCalledOnce();
    await signal(null);
    await signal(3);
    expect(mainStashes.revalidate).toHaveBeenCalledOnce();
    await signal(4);
    expect(mainStashes.revalidate).toHaveBeenCalledTimes(2);

    await signal(9, FEATURE);
    await selectCheckout("feature");
    expect(mainStashes.subscribers()).toBe(0);
    expect(featureStashes.mounts()).toBe(1);
    expect(featureStashes.revalidate).not.toHaveBeenCalled();
    await signal(10, FEATURE);
    expect(featureStashes.revalidate).toHaveBeenCalledOnce();
    await act(async () => button("Toggle repository stashes").click());
    await signal(11, FEATURE);
    expect(featureStashes.subscribers()).toBe(0);
    expect(featureStashes.revalidate).toHaveBeenCalledOnce();
    await act(async () => button("Toggle repository stashes").click());
    expect(featureStashes.mounts()).toBe(2);
    expect(featureStashes.revalidate).toHaveBeenCalledOnce();
  });

  it("reloads a failed Undo read when the repair happens after leaving Changes", async () => {
    const commits = h.query("commits", MAIN);
    Object.assign(commits, { data: null, error: "Git could not read HEAD." });
    await openManager();
    await selectTab("Changes");
    await selectTab("History");
    await publish("status", MAIN, { data: READABLE });
    expect(commits.revalidate).not.toHaveBeenCalled();
    await selectTab("Changes");
    expect(commits.revalidate).toHaveBeenCalledOnce();
    await publish("commits", MAIN, {
      data: { commits: [{ committedAtMs: Date.now(), parents: ["parent"] }] },
      error: null,
    });
    expect(container.textContent).toContain("Undo ready");
    expect(commits.revalidate).toHaveBeenCalledOnce();
  });

  it("keeps the old refs failure hidden when entering Changes during recovery", async () => {
    const refs = h.query("refs", MAIN);
    const failure = "Git returned malformed repository ref state.";
    Object.assign(refs, { data: null, error: failure });
    refs.revalidate.mockImplementation(() => {
      refs.isPending = true;
      refs.publish();
    });
    await openManager();
    await publish("status", MAIN, { data: READABLE });
    await selectTab("Changes");
    expect(container.textContent).toContain("Connecting to changes…");
    expect(container.textContent).not.toContain(failure);
    // A fresh read that still fails must remain actionable, even with the same message.
    await publish("refs", MAIN, { isPending: false, cause: Cause.fail(new Error(failure)) });
    expect(container.textContent).toContain(failure);
    expect(button("Retry").textContent).toBe("Retry");
    expect(refs.revalidate).toHaveBeenCalledOnce();
  });

  it.each([false, true])(
    "keeps a settled transport failure actionable after recovery (cutoff latched: %s)",
    async (requiresRetry) => {
      const refs = h.query("refs", MAIN);
      Object.assign(refs, {
        data: null,
        error: "The connection dropped before the result arrived.",
        requiresRetry,
        cause: Cause.fail(
          new RpcClientError.RpcClientError({
            reason: new Socket.SocketCloseError({ code: 1006, closeReason: "socket closed" }),
          }),
        ),
      });
      await openManager();
      await selectTab("Changes");
      await publish("status", MAIN, { data: READABLE });
      expect(container.textContent).not.toContain("Connecting to changes…");
      expect(container.textContent).toContain("The connection dropped before the result arrived.");
      await act(async () => button("Retry").click());
      expect(refs.refresh).toHaveBeenCalledOnce();
    },
  );
});
