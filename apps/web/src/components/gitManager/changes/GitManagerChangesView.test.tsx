// @vitest-environment happy-dom

import { EnvironmentRpcUnavailableError } from "@coddy/client-runtime/rpc";
import { GitManagerOperationError, type VcsRepositoryUnavailableReason } from "@coddy/contracts";
import * as Cause from "effect/Cause";
import * as AsyncResult from "effect/reactivity/AsyncResult";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import { useGitManagerStore } from "../../../gitManagerStore";
import { useSourceControlPanelStore } from "../../../sourceControlPanelStore";
import type { ChangeRow } from "./changesList.logic";

const h = vi.hoisted(() => ({
  status: null as Record<string, unknown> | null,
  freshStatus: null as Record<string, unknown> | null,
  refs: {
    conflictedPaths: [] as string[],
  } as Record<string, unknown> | null,
  commits: null as Record<string, unknown> | null,
  commitsEmission: null as AsyncResult.AsyncResult<unknown, unknown> | null,
  signalGeneration: 1 as number | null,
  liveSignalAvailable: true,
  availableEditors: [] as string[],
  statusError: null as string | null,
  statusEmission: null as AsyncResult.AsyncResult<unknown, unknown> | null,
  refsError: null as string | null,
  refsEmission: null as AsyncResult.AsyncResult<unknown, unknown> | null,
  refreshStatusQuery: vi.fn(),
  revalidateStatusQuery: vi.fn(),
  statusAtom: vi.fn((target: unknown) => ({ kind: "status", target })),
  refsAtom: vi.fn((target: unknown) => ({ kind: "refs", target })),
  commitsAtom: vi.fn((target: unknown) => ({ kind: "commits", target })),
  signalAtom: vi.fn((target: unknown) => ({ kind: "signal", target })),
  refreshRefs: vi.fn(),
  revalidateRefs: vi.fn(),
  refreshCommits: vi.fn(),
  revalidateCommits: vi.fn(),
  contextMenuShow: vi.fn(
    (
      _items: ReadonlyArray<{ label: string }>,
      _position?: { x: number; y: number },
    ): Promise<string | null> => Promise.resolve(null),
  ),
  openInEditor: vi.fn(() => Promise.resolve({ _tag: "Success" })),
  onRetry: vi.fn(),
  refreshStatus: vi.fn(),
  stageFiles: vi.fn(),
  unstageFiles: vi.fn(),
  commit: vi.fn(),
  undoCommit: vi.fn(),
  discard: vi.fn(),
  listProps: null as Record<string, unknown> | null,
  listRenderCount: 0,
}));

vi.mock("../../../state/vcs", () => ({
  vcsEnvironment: {
    status: h.statusAtom,
    refreshStatus: "cmd:refresh-status",
    stageFiles: "cmd:stage-files",
    unstageFiles: "cmd:unstage-files",
  },
}));

vi.mock("../../../state/gitManager", () => ({
  gitManagerEnvironment: {
    getRefs: h.refsAtom,
    getCommits: h.commitsAtom,
    signalWithDegradedFocusRefresh: h.signalAtom,
    commit: "cmd:commit",
    undoCommit: "cmd:undo-commit",
    discard: "cmd:discard",
  },
}));

const NO_READ_ACTIONS = { refresh: () => undefined, revalidate: () => undefined };

/** Explicit Retry (`refresh`) and automatic re-reads (`revalidate`) go to separate spies. */
function readActionsFor(kind: string | undefined) {
  if (kind === "status")
    return { refresh: h.refreshStatusQuery, revalidate: h.revalidateStatusQuery };
  if (kind === "refs") return { refresh: h.refreshRefs, revalidate: h.revalidateRefs };
  if (kind === "commits") return { refresh: h.refreshCommits, revalidate: h.revalidateCommits };
  return NO_READ_ACTIONS;
}

/** A settled failure is not pending; a read after Retry waits on the failure it replaces. */
function pendingFor(kind: string | undefined, data: unknown): boolean {
  if (kind === "status" && h.statusEmission !== null) return h.statusEmission.waiting;
  if (kind === "refs" && h.refsError !== null) return false;
  if (kind === "commits" && h.commitsEmission !== null) return h.commitsEmission.waiting;
  return data === null;
}

vi.mock("../../../state/query", () => ({
  useEnvironmentQuery: (atom: { kind?: string } | null) => {
    const kind = atom?.kind;
    const { refresh, revalidate } = readActionsFor(kind);
    const data =
      kind === "status"
        ? h.status
        : kind === "refs"
          ? h.refs
          : kind === "commits"
            ? h.commits
            : kind === "signal" && h.signalGeneration !== null
              ? { generation: h.signalGeneration }
              : null;
    return {
      data,
      emission:
        kind === "status" && h.statusEmission !== null
          ? h.statusEmission
          : kind === "refs" && h.refsEmission !== null
            ? h.refsEmission
            : kind === "commits" && h.commitsEmission !== null
              ? h.commitsEmission
              : { _tag: data === null ? "Initial" : "Success", waiting: data === null },
      error: kind === "status" ? h.statusError : kind === "refs" ? h.refsError : null,
      isPending: pendingFor(kind, data),
      refresh,
      revalidate,
      requiresRetry: false,
    };
  },
}));

vi.mock("../../../state/entities", () => ({
  useProject: () => ({ workspaceRoot: "/repo/main" }),
  useServerConfigs: () =>
    new Map([
      [
        "environment-1",
        {
          availableEditors: h.availableEditors,
          environment: {
            capabilities: {
              gitManagerCommitOperations: true,
              gitManagerLiveSignal: h.liveSignalAvailable,
            },
          },
        },
      ],
    ]),
  useThreadShellsForProjectRefs: () => [],
}));

vi.mock("../../../editorPreferences", () => ({
  useOpenInPreferredEditor: () => vi.fn(),
}));

vi.mock("../../../localApi", () => ({
  readLocalApi: () => ({ contextMenu: { show: h.contextMenuShow } }),
}));

vi.mock("../../../state/use-atom-command", () => ({
  useAtomCommand: (command: unknown) => {
    switch (command) {
      case "cmd:refresh-status":
        return h.refreshStatus;
      case "cmd:stage-files":
        return h.stageFiles;
      case "cmd:unstage-files":
        return h.unstageFiles;
      case "cmd:commit":
        return h.commit;
      case "cmd:undo-commit":
        return h.undoCommit;
      case "cmd:discard":
        return h.discard;
      default:
        return h.openInEditor;
    }
  },
}));

vi.mock("./GitManagerChangesList", () => ({
  GitManagerChangesList: (props: { rows: ReadonlyArray<ChangeRow> }) => {
    h.listProps = props as unknown as Record<string, unknown>;
    h.listRenderCount += 1;
    return (
      <div>
        {props.rows.map((row) => (
          <span key={row.path}>{row.path}</span>
        ))}
      </div>
    );
  },
}));

import { GitManagerChangesView } from "./GitManagerChangesView";
import { GitManagerChangeRow } from "./GitManagerChangeRow";

const projectRef = { environmentId: "environment-1", projectId: "project-1" } as never;

function statusWith(path: string) {
  return {
    workingTree: {
      files: [
        {
          path,
          insertions: 1,
          deletions: 0,
          status: "modified",
          area: "unstaged",
        },
      ],
    },
  };
}

/** The status a live server streamed while `.git/HEAD` was broken; a bad `.git/config` matched it. */
function unreadableStatus() {
  return {
    isRepo: false,
    hasPrimaryRemote: false,
    isDefaultRef: false,
    refName: null,
    hasWorkingTreeChanges: false,
    workingTree: { files: [], insertions: 0, deletions: 0 },
    hasUpstream: false,
    aheadCount: 0,
    behindCount: 0,
    aheadOfDefaultCount: 0,
    pr: null,
  };
}

let container: HTMLDivElement;
let root: Root | null;

async function renderView(
  cwd = "/repo/main",
  retrying = false,
  overrides: Partial<React.ComponentProps<typeof GitManagerChangesView>> = {},
) {
  await act(async () =>
    root?.render(
      <GitManagerChangesView
        scope={{ environmentId: "environment-1" as never, cwd }}
        projectRef={projectRef}
        refsRecovering={false}
        repositoryUnavailable={
          h.statusError === null && h.status?.isRepo === false
            ? ((h.status.repositoryUnavailableReason as
                | VcsRepositoryUnavailableReason
                | undefined) ?? "unknown")
            : null
        }
        retrying={retrying}
        onRetry={h.onRetry}
        {...overrides}
      />,
    ),
  );
}

function buttonWithText(text: string): HTMLButtonElement {
  const button = [...document.querySelectorAll("button")].find(
    (candidate) =>
      (candidate.textContent?.includes(text) ?? false) ||
      candidate.getAttribute("aria-label") === text,
  );
  if (!(button instanceof HTMLButtonElement)) throw new Error(`Missing button: ${text}`);
  return button;
}

function isInactive(button: HTMLButtonElement): boolean {
  return button.getAttribute("aria-disabled") === "true";
}

function checkboxLabelWithText(text: string): HTMLLabelElement {
  const label = [...document.querySelectorAll("label")].find((candidate) =>
    candidate.textContent?.includes(text),
  );
  if (!(label instanceof HTMLLabelElement)) throw new Error(`Missing checkbox label: ${text}`);
  return label;
}

async function changeInput(input: HTMLInputElement, value: string): Promise<void> {
  await act(async () => {
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
    setter?.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
    input.dispatchEvent(new Event("change", { bubbles: true }));
  });
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  h.status = null;
  h.freshStatus = null;
  h.refs = {
    conflictedPaths: [],
    headRef: "main",
    isDirty: false,
    localBranches: [],
  };
  h.commits = null;
  h.commitsEmission = null;
  h.signalGeneration = 1;
  h.liveSignalAvailable = true;
  h.availableEditors = [];
  h.statusError = null;
  h.statusEmission = null;
  h.refsError = null;
  h.refsEmission = null;
  h.refreshStatusQuery.mockClear();
  h.revalidateStatusQuery.mockClear();
  h.listProps = null;
  h.listRenderCount = 0;
  h.statusAtom.mockClear();
  h.refsAtom.mockClear();
  h.commitsAtom.mockClear();
  h.signalAtom.mockClear();
  h.refreshRefs.mockClear();
  h.revalidateRefs.mockClear();
  h.refreshCommits.mockClear();
  h.revalidateCommits.mockClear();
  h.contextMenuShow.mockClear();
  h.openInEditor.mockClear();
  h.onRetry.mockClear();
  h.refreshStatus.mockReset();
  h.refreshStatus.mockImplementation(() =>
    Promise.resolve(AsyncResult.success(h.freshStatus ?? h.status)),
  );
  h.stageFiles.mockReset();
  h.stageFiles.mockResolvedValue(AsyncResult.success({}));
  h.unstageFiles.mockReset();
  h.unstageFiles.mockResolvedValue(AsyncResult.success({}));
  h.commit.mockReset();
  h.commit.mockResolvedValue(AsyncResult.success({ sha: "abc", empty: false }));
  h.undoCommit.mockReset();
  h.undoCommit.mockResolvedValue(
    AsyncResult.success({ summary: "Restored", description: "", coAuthors: [] }),
  );
  h.discard.mockReset();
  h.discard.mockResolvedValue(
    AsyncResult.success({ trashed: [], permanentlyDiscarded: [], trashUnavailable: [] }),
  );
  useGitManagerStore.setState({ byProjectKey: {} });
  useSourceControlPanelStore.setState({ byThreadKey: {}, byCwdKey: {} });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root?.unmount());
  root = null;
  container.remove();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("GitManagerChangesView", () => {
  it("revalidates a persistently failing latest-commit read only once after repair", async () => {
    h.status = unreadableStatus();
    h.commitsEmission = AsyncResult.failure(Cause.fail(new Error("Git could not read HEAD.")));

    await renderView();
    expect(h.revalidateCommits).not.toHaveBeenCalled();

    h.status = { ...statusWith("src/repaired.ts"), isRepo: true };
    await renderView();
    expect(h.revalidateCommits).toHaveBeenCalledOnce();
    expect(h.refreshCommits).not.toHaveBeenCalled();

    for (let attempt = 0; attempt < 3; attempt += 1) {
      h.commitsEmission = { ...h.commitsEmission, waiting: true };
      await renderView();
      expect(h.revalidateCommits).toHaveBeenCalledOnce();
      h.commitsEmission = AsyncResult.failure(Cause.fail(new Error("Git could not read HEAD.")));
      await renderView();
      expect(h.revalidateCommits).toHaveBeenCalledOnce();
    }
    expect(h.refreshCommits).not.toHaveBeenCalled();
  });

  it("revalidates a cached latest-commit failure once when entering Changes after repair", async () => {
    h.status = { ...statusWith("src/repaired.ts"), isRepo: true };
    h.commitsEmission = AsyncResult.failure(Cause.fail(new Error("Git could not read HEAD.")));

    await renderView();
    expect(h.revalidateCommits).toHaveBeenCalledOnce();
    h.commitsEmission = { ...h.commitsEmission, waiting: true };
    await renderView();
    h.commitsEmission = AsyncResult.failure(Cause.fail(new Error("Git could not read HEAD.")));
    await renderView();
    expect(h.revalidateCommits).toHaveBeenCalledOnce();

    await act(async () => root?.render(null));
    await renderView();
    expect(h.revalidateCommits).toHaveBeenCalledTimes(2);
    await renderView();
    expect(h.revalidateCommits).toHaveBeenCalledTimes(2);
    expect(h.refreshCommits).not.toHaveBeenCalled();
  });

  it("revalidates the latest-commit failure once more after a second outage and repair", async () => {
    h.status = unreadableStatus();
    h.commitsEmission = AsyncResult.failure(Cause.fail(new Error("Git could not read HEAD.")));
    await renderView();

    h.status = { ...statusWith("src/repaired.ts"), isRepo: true };
    await renderView();
    expect(h.revalidateCommits).toHaveBeenCalledOnce();

    h.status = unreadableStatus();
    await renderView();
    expect(h.revalidateCommits).toHaveBeenCalledOnce();
    h.status = null;
    await renderView();
    expect(h.revalidateCommits).toHaveBeenCalledOnce();

    h.status = { ...statusWith("src/repaired-again.ts"), isRepo: true };
    await renderView();
    expect(h.revalidateCommits).toHaveBeenCalledTimes(2);
    h.commitsEmission = { ...h.commitsEmission, waiting: true };
    await renderView();
    h.commitsEmission = AsyncResult.failure(Cause.fail(new Error("Git could not read HEAD.")));
    await renderView();
    expect(h.revalidateCommits).toHaveBeenCalledTimes(2);
    expect(h.refreshCommits).not.toHaveBeenCalled();
  });

  it("waits for a pending latest-commit read to settle after the repository becomes readable", async () => {
    h.status = unreadableStatus();
    h.commitsEmission = {
      ...AsyncResult.failure(Cause.fail(new Error("Git could not read HEAD."))),
      waiting: true,
    };
    await renderView();
    expect(h.revalidateCommits).not.toHaveBeenCalled();

    h.status = { ...statusWith("src/repaired.ts"), isRepo: true };
    await renderView();
    expect(h.revalidateCommits).not.toHaveBeenCalled();
    h.commitsEmission = { ...h.commitsEmission, waiting: false };
    await renderView();
    expect(h.revalidateCommits).toHaveBeenCalledOnce();
    expect(h.refreshCommits).not.toHaveBeenCalled();
  });

  it("keeps explicit change reads available without opening the live signal", async () => {
    h.liveSignalAvailable = false;
    h.status = statusWith("src/manual-refresh.ts");

    await renderView();

    expect(h.signalAtom).not.toHaveBeenCalled();
    expect(h.statusAtom).toHaveBeenCalledOnce();
    expect(h.refsAtom).toHaveBeenCalledOnce();
    expect(container.textContent).toContain("src/manual-refresh.ts");
  });

  it("repopulates from the status subscription after a reconnect", async () => {
    await renderView();
    expect(container.textContent).toContain("Connecting to changes");

    h.status = statusWith("src/before.ts");
    await renderView();
    expect(container.textContent).toContain("src/before.ts");

    h.status = null;
    await renderView();
    expect(container.textContent).toContain("Connecting to changes");

    h.status = statusWith("src/after.ts");
    await renderView();
    expect(container.textContent).toContain("src/after.ts");
    expect(h.statusAtom).toHaveBeenCalledWith({
      environmentId: "environment-1",
      input: { cwd: "/repo/main" },
    });
  });

  it("distinguishes an unavailable environment from a Git read failure", async () => {
    h.statusError = "Remote environment is not connected.";
    h.statusEmission = AsyncResult.failure(
      Cause.fail(
        new EnvironmentRpcUnavailableError({
          environmentId: "environment-1",
          message: h.statusError,
        }),
      ),
    );

    await renderView();

    expect(container.textContent).toContain("Environment unavailable");
    expect(container.textContent).toContain("Remote environment is not connected.");
    // The read resumes by itself when the connection is back; Retry could not reach it.
    expect(buttonWithText("Waiting for the connection…").getAttribute("aria-disabled")).toBe(
      "true",
    );
    expect(container.textContent).not.toContain("Retry");
  });

  it("offers Retry when changes fail to load, and reads status again explicitly", async () => {
    h.statusError = "The connection dropped before the result arrived.";
    h.statusEmission = AsyncResult.failure(Cause.fail(new Error(h.statusError)));

    await renderView();
    expect(h.revalidateRefs).not.toHaveBeenCalled();

    expect(container.textContent).toContain("Could not load changes");
    await act(async () => buttonWithText("Retry").click());
    expect(h.refreshStatusQuery).toHaveBeenCalledOnce();
    expect(h.refreshRefs).not.toHaveBeenCalled();
    expect(h.revalidateStatusQuery).not.toHaveBeenCalled();
    expect(h.revalidateRefs).not.toHaveBeenCalled();
  });

  it("retries only the read that failed", async () => {
    h.status = statusWith("src/loaded.ts");
    h.refs = null;
    h.refsError = "Git could not read the branches.";

    await renderView();
    h.revalidateRefs.mockClear();

    expect(container.textContent).toContain("Git could not read the branches.");
    await act(async () => buttonWithText("Retry").click());
    expect(h.refreshRefs).toHaveBeenCalledOnce();
    expect(h.refreshStatusQuery).not.toHaveBeenCalled();
    expect(h.revalidateRefs).not.toHaveBeenCalled();
    expect(h.revalidateStatusQuery).not.toHaveBeenCalled();
  });

  it("keeps the failure on screen and disables Retry while changes are read again", async () => {
    h.statusError = "The connection dropped before the result arrived.";
    h.statusEmission = {
      ...AsyncResult.failure(Cause.fail(new Error(h.statusError))),
      waiting: true,
    };

    await renderView();

    expect(container.textContent).toContain("Could not load changes");
    expect(buttonWithText("Retrying…").getAttribute("aria-disabled")).toBe("true");
  });

  it("explains a repository Git cannot read instead of showing no changes", async () => {
    h.status = unreadableStatus();

    await renderView();

    expect(container.textContent).toContain("Could not load changes");
    expect(container.textContent).toContain(
      "Git can't read this folder as a repository. Run git init to create one, or check its .git folder if it already is one.",
    );
    expect(container.textContent).not.toContain("No local changes");
    expect(container.querySelector("#git-manager-summary")).toBeNull();
    expect(buttonWithText("Retry").textContent).toBe("Retry");
    expect(isInactive(buttonWithText("Retry"))).toBe(false);
  });

  it("calls onRetry for an unreadable repository and shows busy from its props", async () => {
    h.status = unreadableStatus();
    await renderView();
    h.revalidateRefs.mockClear();

    await act(async () => buttonWithText("Retry").click());

    expect(h.onRetry).toHaveBeenCalledOnce();
    expect(h.refreshStatus).not.toHaveBeenCalled();
    expect(buttonWithText("Retry").textContent).toBe("Retry");

    await renderView("/repo/main", true);
    expect(buttonWithText("Retrying…").textContent).toBe("Retrying…");
    expect(buttonWithText("Retrying…").getAttribute("aria-disabled")).toBe("true");
    // The status stream itself is healthy, and the refs keep their last good answer.
    expect(h.refreshStatusQuery).not.toHaveBeenCalled();
    expect(h.refreshRefs).not.toHaveBeenCalled();
    expect(h.revalidateRefs).not.toHaveBeenCalled();

    await renderView("/repo/main", false);

    expect(container.textContent).toContain("Git can't read this folder as a repository.");
    expect(buttonWithText("Retry").textContent).toBe("Retry");
    expect(isInactive(buttonWithText("Retry"))).toBe(false);
  });

  it("uses the retry state supplied for the selected checkout", async () => {
    h.status = unreadableStatus();
    await renderView("/repo/main", true);
    expect(buttonWithText("Retrying…").textContent).toBe("Retrying…");
    expect(buttonWithText("Retrying…").getAttribute("aria-disabled")).toBe("true");

    await renderView("/repo/other", false);
    expect(buttonWithText("Retry").textContent).toBe("Retry");
    expect(isInactive(buttonWithText("Retry"))).toBe(false);

    await renderView("/repo/main", true);
    expect(buttonWithText("Retrying…").textContent).toBe("Retrying…");
    expect(isInactive(buttonWithText("Retrying…"))).toBe(true);
  });

  it("explains the unreadable repository rather than the refs failure it causes", async () => {
    h.status = unreadableStatus();
    h.refs = null;
    h.refsError = "Git could not complete the requested read.";

    await renderView();

    expect(container.textContent).toContain("Git can't read this folder as a repository.");
    expect(container.textContent).not.toContain("Git could not complete the requested read.");
  });

  it("waits for the connection instead of blaming the repository", async () => {
    h.status = unreadableStatus();
    h.statusError = "Remote environment is not connected.";
    h.statusEmission = AsyncResult.failure(
      Cause.fail(
        new EnvironmentRpcUnavailableError({
          environmentId: "environment-1",
          message: h.statusError,
        }),
      ),
    );

    await renderView();

    expect(container.textContent).toContain("Environment unavailable");
    expect(container.textContent).not.toContain("Git can't read this folder as a repository.");
    expect(buttonWithText("Waiting for the connection…").getAttribute("aria-disabled")).toBe(
      "true",
    );
  });

  it("shows the changes again once Git can read the repository", async () => {
    h.status = unreadableStatus();
    await renderView();

    h.status = statusWith("src/repaired.ts");
    await renderView();

    expect(container.textContent).toContain("src/repaired.ts");
    expect(container.textContent).not.toContain("Could not load changes");
  });

  it("uses availability supplied by the panel instead of deriving it from its own status read", async () => {
    h.status = statusWith("src/cached.ts");
    await renderView("/repo/selected", false, { repositoryUnavailable: "untrusted" });
    expect(container.querySelector('[role="alert"]')?.textContent).toContain(
      "Git doesn't trust this repository because another user owns it. Run git config --global --add safe.directory /repo/selected to trust it.",
    );
    expect(container.querySelector("code")?.textContent).toBe(
      "git config --global --add safe.directory /repo/selected",
    );
    expect(container.textContent).not.toContain("src/cached.ts");
  });

  it("keeps an unavailable-environment refs failure ahead of the repository explanation", async () => {
    h.status = unreadableStatus();
    h.refsError = "Remote environment is not connected.";
    h.refsEmission = AsyncResult.failure(
      Cause.fail(
        new EnvironmentRpcUnavailableError({
          environmentId: "environment-1",
          message: h.refsError,
        }),
      ),
    );
    await renderView();
    expect(container.textContent).toContain("Environment unavailable");
    expect(container.textContent).not.toContain("Git can't read this folder as a repository.");
    expect(buttonWithText("Waiting for the connection…").textContent).toBe(
      "Waiting for the connection…",
    );
    expect(isInactive(buttonWithText("Waiting for the connection…"))).toBe(true);
  });

  it("keeps inclusion local while selection updates the shared view state", async () => {
    h.status = statusWith("src/selected.ts");
    await renderView();
    const listProps = h.listProps as {
      rows: ReadonlyArray<ChangeRow>;
      onSelect: (path: string) => void;
      onToggle: (path: string) => void;
    };

    expect(listProps.rows[0]?.inclusion).toBe("all");
    await act(async () => listProps.onToggle("src/selected.ts"));
    expect((h.listProps as { rows: ReadonlyArray<ChangeRow> }).rows[0]?.inclusion).toBe("none");

    await act(async () => listProps.onSelect("src/selected.ts"));
    expect(useGitManagerStore.getState().selectViewState(projectRef).selectedFilePath).toBe(
      "src/selected.ts",
    );
  });

  it("uses the local context-menu path and exposes confirmed discard", async () => {
    h.status = statusWith("src/menu.ts");
    h.contextMenuShow.mockResolvedValueOnce("toggle-inclusion");
    await renderView();
    const listProps = h.listProps as {
      rows: ReadonlyArray<ChangeRow>;
      onContextMenu: (path: string, position: { x: number; y: number }) => void;
    };

    await act(async () => listProps.onContextMenu("src/menu.ts", { x: 12, y: 24 }));
    await vi.waitFor(() => expect(h.contextMenuShow).toHaveBeenCalledOnce());

    const items = h.contextMenuShow.mock.calls[0]?.[0] ?? [];
    expect(items.map((item) => item.label)).toEqual(
      expect.arrayContaining([
        "Ignore file",
        "Ignore folder",
        "Ignore all .ts",
        "Exclude selected",
        "Discard changes",
        "Copy path",
        "Copy relative path",
        "Reveal",
        "Open in editor",
      ]),
    );
    await vi.waitFor(() =>
      expect((h.listProps as { rows: ReadonlyArray<ChangeRow> }).rows[0]?.inclusion).toBe("none"),
    );
  });

  it("refreshes status immediately before staging the selected paths", async () => {
    h.status = statusWith("src/selected.ts");
    h.freshStatus = {
      workingTree: {
        files: [
          {
            path: "src/selected.ts",
            insertions: 1,
            deletions: 0,
            status: "modified",
            area: "unstaged",
          },
          {
            path: "src/appeared-after-click.ts",
            insertions: 1,
            deletions: 0,
            status: "modified",
            area: "staged",
          },
        ],
      },
    };
    await renderView();

    await act(async () => buttonWithText("Commit 1 files to main").click());

    await vi.waitFor(() => expect(h.commit).toHaveBeenCalledOnce());
    expect(h.refreshStatus).toHaveBeenCalledWith({
      environmentId: "environment-1",
      input: { cwd: "/repo/main" },
    });
    expect(h.stageFiles).toHaveBeenCalledWith({
      environmentId: "environment-1",
      input: { cwd: "/repo/main", filePaths: ["src/selected.ts"] },
    });
    expect(h.unstageFiles).toHaveBeenCalledWith({
      environmentId: "environment-1",
      input: { cwd: "/repo/main", filePaths: ["src/appeared-after-click.ts"] },
    });
    expect(h.refreshStatus.mock.invocationCallOrder[0]).toBeLessThan(
      h.stageFiles.mock.invocationCallOrder[0]!,
    );
    expect(h.refreshStatus.mock.invocationCallOrder[0]).toBeLessThan(
      h.unstageFiles.mock.invocationCallOrder[0]!,
    );
  });

  it("does not re-render the changes list while the shared draft is typed", async () => {
    h.status = statusWith("src/selected.ts");
    await renderView();
    const listRenderCount = h.listRenderCount;

    await changeInput(
      container.querySelector<HTMLInputElement>("#git-manager-summary")!,
      "Draft without list churn",
    );

    expect(h.listRenderCount).toBe(listRenderCount);
  });

  it("passes amend, options, and co-authors through the environment command", async () => {
    h.status = statusWith("src/selected.ts");
    h.freshStatus = h.status;
    h.refs = {
      conflictedPaths: [],
      headRef: "main",
      isDirty: false,
      localBranches: [{ current: true, upstream: null, ahead: 0 }],
    };
    h.commits = {
      commits: [{ committedAtMs: Date.now(), parents: ["parent"] }],
    };
    await renderView();
    await changeInput(
      container.querySelector<HTMLInputElement>("#git-manager-summary")!,
      "Commit options",
    );
    await changeInput(
      container.querySelector<HTMLInputElement>("#git-manager-co-author")!,
      "Ada Lovelace <ada@example.test>",
    );
    await act(async () => buttonWithText("Add Co-author").click());
    await act(async () => buttonWithText("Amend Last Commit").click());
    await act(async () => buttonWithText("Commit Options").click());
    for (const label of ["Bypass Commit Hooks", "Signed-off-by", "Allow Empty"]) {
      await act(async () => checkboxLabelWithText(label).click());
    }

    await act(async () => buttonWithText("Commit 1 files to main").click());

    await vi.waitFor(() => expect(h.commit).toHaveBeenCalledOnce());
    expect(h.commit).toHaveBeenCalledWith({
      environmentId: "environment-1",
      input: {
        cwd: "/repo/main",
        summary: "Commit options",
        description: "",
        amend: true,
        noVerify: true,
        signoff: true,
        allowEmpty: true,
        coAuthors: [{ name: "Ada Lovelace", email: "ada@example.test" }],
      },
    });
  });

  it("renders a server-authored blocked reason verbatim", async () => {
    const reason = "Finish the server-observed merge before committing.";
    h.status = statusWith("src/selected.ts");
    h.freshStatus = h.status;
    h.commit.mockResolvedValueOnce(
      AsyncResult.failure(
        Cause.fail(
          new GitManagerOperationError({
            operation: "gitManager.commit",
            code: "merge-in-progress",
            message: reason,
            blocked: {
              operation: "commit",
              code: "merge-in-progress",
              message: reason,
            },
          }),
        ),
      ),
    );
    await renderView();

    await act(async () => buttonWithText("Commit 1 files to main").click());

    await vi.waitFor(() => expect(container.textContent).toContain(reason));
  });

  it("refreshes refs and the latest commit only after the commit succeeds", async () => {
    h.status = statusWith("src/selected.ts");
    h.freshStatus = h.status;
    await renderView();
    h.revalidateRefs.mockClear();
    h.revalidateCommits.mockClear();

    await act(async () => buttonWithText("Commit 1 files to main").click());

    await vi.waitFor(() => expect(h.commit).toHaveBeenCalledOnce());
    await vi.waitFor(() => expect(h.revalidateRefs).toHaveBeenCalledOnce());
    expect(h.revalidateCommits).toHaveBeenCalledOnce();
    expect(h.revalidateRefs.mock.invocationCallOrder[0]).toBeGreaterThan(
      h.commit.mock.invocationCallOrder[0]!,
    );
    expect(h.refreshRefs).not.toHaveBeenCalled();
    expect(h.refreshCommits).not.toHaveBeenCalled();
  });

  it("does not refresh refs or history reads when the commit fails", async () => {
    const reason = "pre-commit hook rejected the change";
    h.status = statusWith("src/selected.ts");
    h.freshStatus = h.status;
    h.commit.mockResolvedValueOnce(
      AsyncResult.failure(
        Cause.fail(
          new GitManagerOperationError({
            operation: "gitManager.commit",
            code: "git-command-failed",
            message: reason,
            blocked: null,
          }),
        ),
      ),
    );
    await renderView();
    h.revalidateRefs.mockClear();
    h.revalidateCommits.mockClear();

    await act(async () => buttonWithText("Commit 1 files to main").click());

    await vi.waitFor(() => expect(container.textContent).toContain(reason));
    expect(h.revalidateRefs).not.toHaveBeenCalled();
    expect(h.revalidateCommits).not.toHaveBeenCalled();
    expect(h.refreshRefs).not.toHaveBeenCalled();
    expect(h.refreshCommits).not.toHaveBeenCalled();
  });

  it("does not discard from the context menu until the confirmation is accepted", async () => {
    h.status = statusWith("src/discard.ts");
    h.contextMenuShow.mockResolvedValueOnce("discard");
    await renderView();
    const listProps = h.listProps as {
      onContextMenu: (path: string, position: { x: number; y: number }) => void;
    };

    await act(async () => listProps.onContextMenu("src/discard.ts", { x: 12, y: 24 }));
    await vi.waitFor(() => expect(document.body.textContent).toContain("Move Changes to Trash?"));
    expect(h.discard).not.toHaveBeenCalled();

    await act(async () => buttonWithText("Move to Trash").click());
    await vi.waitFor(() =>
      expect(h.discard).toHaveBeenCalledWith({
        environmentId: "environment-1",
        input: {
          cwd: "/repo/main",
          paths: ["src/discard.ts"],
          permitPermanent: false,
        },
      }),
    );
  });

  it("offers permanent discard only for paths the server reports as not trashed", async () => {
    h.status = statusWith("src/unavailable-trash.ts");
    h.contextMenuShow.mockResolvedValueOnce("discard");
    h.discard
      .mockResolvedValueOnce(
        AsyncResult.success({
          trashed: [],
          permanentlyDiscarded: [],
          trashUnavailable: ["src/unavailable-trash.ts"],
        }),
      )
      .mockResolvedValueOnce(
        AsyncResult.success({
          trashed: [],
          permanentlyDiscarded: ["src/unavailable-trash.ts"],
          trashUnavailable: [],
        }),
      );
    await renderView();
    const listProps = h.listProps as {
      onContextMenu: (path: string, position: { x: number; y: number }) => void;
    };

    await act(async () => listProps.onContextMenu("src/unavailable-trash.ts", { x: 12, y: 24 }));
    await vi.waitFor(() => expect(document.body.textContent).toContain("Move Changes to Trash?"));
    await act(async () => buttonWithText("Move to Trash").click());
    await vi.waitFor(() =>
      expect(document.body.textContent).toContain("server reported that OS trash is unavailable"),
    );

    await act(async () => buttonWithText("Discard Permanently").click());
    await vi.waitFor(() => expect(h.discard).toHaveBeenCalledTimes(2));
    expect(h.discard).toHaveBeenLastCalledWith({
      environmentId: "environment-1",
      input: {
        cwd: "/repo/main",
        paths: ["src/unavailable-trash.ts"],
        permitPermanent: true,
      },
    });
  });

  it("reveals a file through the environment-scoped file-manager command", async () => {
    h.status = statusWith("src/menu.ts");
    h.availableEditors = ["file-manager"];
    h.contextMenuShow.mockResolvedValueOnce("reveal");
    await renderView();
    const listProps = h.listProps as {
      onContextMenu: (path: string, position: { x: number; y: number }) => void;
    };

    await act(async () => listProps.onContextMenu("src/menu.ts", { x: 12, y: 24 }));
    await vi.waitFor(() =>
      expect(h.openInEditor).toHaveBeenCalledWith({
        environmentId: "environment-1",
        input: { cwd: "/repo/main/src", editor: "file-manager" },
      }),
    );
  });

  it("renders the filter-miss blank slate with a clear action", async () => {
    h.status = statusWith("src/visible.ts");
    useGitManagerStore.getState().setFilterText(projectRef, "missing");

    await renderView();

    expect(container.textContent).toContain("No changed files match these filters.");
    expect(container.textContent).toContain("Clear filters");
  });

  it("keeps mouse selection separate from keyboard inclusion toggling", async () => {
    const onSelect = vi.fn();
    const onToggle = vi.fn();
    await act(async () =>
      root?.render(
        <GitManagerChangeRow
          row={{
            path: "src/keyboard.ts",
            status: "modified",
            area: "unstaged",
            insertions: 1,
            deletions: 0,
            inclusion: "all",
            conflicted: false,
            submodule: false,
            disabledReason: null,
          }}
          selected={false}
          onContextMenu={() => undefined}
          onOpenExternal={() => undefined}
          onSelect={onSelect}
          onToggle={onToggle}
        />,
      ),
    );
    const row = container.querySelector<HTMLElement>("[role='option']")!;

    await act(async () => row.dispatchEvent(new MouseEvent("click", { bubbles: true })));
    expect(onSelect).toHaveBeenCalledWith("src/keyboard.ts");
    expect(onToggle).not.toHaveBeenCalled();

    await act(async () =>
      row.dispatchEvent(new KeyboardEvent("keydown", { bubbles: true, key: "Enter" })),
    );
    expect(onToggle).toHaveBeenCalledWith("src/keyboard.ts");
  });

  it("renders a disabled reason verbatim and blocks keyboard inclusion", async () => {
    const onToggle = vi.fn();
    await act(async () =>
      root?.render(
        <GitManagerChangeRow
          row={{
            path: "vendor/nested",
            status: "modified",
            area: "unstaged",
            insertions: 0,
            deletions: 0,
            inclusion: "none",
            conflicted: false,
            submodule: true,
            disabledReason: "Server-authored nested checkout reason.",
          }}
          selected={false}
          onContextMenu={() => undefined}
          onOpenExternal={() => undefined}
          onSelect={() => undefined}
          onToggle={onToggle}
        />,
      ),
    );
    const row = container.querySelector<HTMLElement>("[role='option']")!;
    const checkbox = container.querySelector<HTMLElement>("[role='checkbox']")!;

    expect(container.textContent).toContain("Server-authored nested checkout reason.");
    expect(checkbox.getAttribute("aria-describedby")).toBeTruthy();
    expect(checkbox.getAttribute("title")).toBe("Server-authored nested checkout reason.");
    await act(async () =>
      row.dispatchEvent(new KeyboardEvent("keydown", { bubbles: true, key: " " })),
    );
    expect(onToggle).not.toHaveBeenCalled();
  });
});
