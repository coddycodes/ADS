// @vitest-environment happy-dom
import { AVAILABLE_CONNECTION_STATE } from "@coddy/client-runtime/connection";
import {
  EnvironmentId,
  ProjectId,
  type GitManagerRefsSnapshot,
  type GitManagerCommitEntry,
} from "@coddy/contracts";
import { makeTestExecutionEnvironmentCapabilities } from "@coddy/shared/testSupport";
import { act, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vite-plus/test";

const h = vi.hoisted(() => ({
  project: {
    id: "history-changes-visibility",
    title: "Owned visual repository",
    workspaceRoot: "/owned/primary",
  },
  status: null as unknown,
  refs: null as unknown,
  page: null as unknown,
  swatches: [] as string[],
  getDiff: vi.fn(
    (request: { input: { source: { _tag: string; path: string; staged?: boolean } } }) => ({
      kind: "diff",
      source: request.input.source,
    }),
  ),
  noop: () => {},
  command: vi.fn(async () => ({ _tag: "Success", value: undefined })),
}));
vi.mock("../../state/query", () => ({
  useEnvironmentQuery: (atom: { kind: string; source?: { _tag: string; path: string } } | null) => {
    let data: unknown =
      atom?.kind === "catalog"
        ? {
            worktrees: [
              { path: "/owned/primary", branch: "main", isPrimary: true },
              { path: "/owned/managed", branch: "codex/delivery-retry-light", isPrimary: false },
            ],
          }
        : atom?.kind === "status"
          ? h.status
          : atom?.kind === "refs"
            ? h.refs
            : atom?.kind === "signal"
              ? { generation: 1 }
              : atom?.kind === "commits" || atom?.kind === "history"
                ? h.page
                : null;
    if (atom?.kind === "diff" && atom.source) {
      const source = atom.source;
      const path = source.path;
      const patch =
        `:100644 100644 1111111 0000000 M\0${path}\0\0diff --git a/${path} b/${path}\nindex 1111111..2222222 100644\n` +
        (path.endsWith(".png")
          ? `Binary files a/${path} and b/${path} differ\n`
          : `--- a/${path}\n+++ b/${path}\n@@ -1 +1 @@\n-before\n+after\n`);
      data =
        source._tag === "commit" && path.endsWith(".png")
          ? {
              _tag: "image",
              generation: 1,
              source,
              byteLength: 358,
              longestLineLength: 0,
              before: { mimeType: "image/png", contentBase64: h.swatches[1] },
              after: { mimeType: "image/png", contentBase64: h.swatches[0] },
            }
          : {
              _tag: "patch",
              generation: 1,
              source,
              byteLength: patch.length,
              longestLineLength: 127,
              patch,
            };
    }
    return {
      data,
      error: null,
      isPending: false,
      emission:
        data === null ? { _tag: "Initial" } : { _tag: "Success", value: data, waiting: false },
      refresh: h.noop,
      revalidate: h.noop,
      requiresRetry: false,
    };
  },
}));
vi.mock("../../state/entities", () => ({
  useProject: () => h.project,
  useThreadShellsForProjectRefs: () => [],
  useServerConfigs: () =>
    new Map([
      [
        "local",
        {
          environment: {
            capabilities: makeTestExecutionEnvironmentCapabilities({
              gitManagerReads: true,
              gitManagerLiveSignal: true,
              gitManagerBranchSyncOperations: true,
              gitManagerStashMergeOperations: true,
              gitManagerRewriteOperations: true,
              gitManagerTagOperations: true,
              gitManagerCommitOperations: true,
              gitManagerPartialStaging: true,
            }),
          },
        },
      ],
    ]),
}));
vi.mock("../../state/environments", () => ({
  useEnvironmentConnectionState: () => ({
    data: { ...AVAILABLE_CONNECTION_STATE, desired: true, phase: "connected", network: "online" },
  }),
  useEnvironment: () => ({ label: "Local" }),
  usePrimaryEnvironmentId: () => "local",
}));
vi.mock("../../state/worktrees", () => ({
  worktreeEnvironment: { catalog: () => ({ kind: "catalog" }) },
}));
vi.mock("../../state/vcs", () => ({
  vcsEnvironment: {
    status: () => ({ kind: "status" }),
    refreshStatus: "refresh",
    stageFiles: "stage",
    unstageFiles: "unstage",
  },
}));
vi.mock("../../state/gitManager", () => ({
  gitManagerEnvironment: {
    getRefs: () => ({ kind: "refs" }),
    signalWithDegradedFocusRefresh: () => ({ kind: "signal" }),
    getCommits: () => ({ kind: "commits" }),
    getHistoryFirstPage: () => ({ kind: "history" }),
    getRetainedCommitPages: () => ({ kind: "retained" }),
    getDiff: h.getDiff,
    getStashes: () => ({ kind: "stashes" }),
    commit: "commit",
    undoCommit: "undo",
    discard: "discard",
    stagePartial: "stage-partial",
    unstagePartial: "unstage-partial",
  },
  runGitManagerOperation: () => ({
    cancel: h.noop,
    result: Promise.resolve({ _tag: "Success", value: undefined }),
  }),
}));
vi.mock("../../state/shell", () => ({ shellEnvironment: { openInEditor: "open-editor" } }));
vi.mock("../../state/use-atom-command", () => ({ useAtomCommand: () => h.command }));
vi.mock("../../editorPreferences", () => ({ useOpenInPreferredEditor: () => h.noop }));
// Only the code-worker command boundary is inert. Panel, tabs, store, history, changes,
// rows, installed LegendList, image panes, parser and staging gutter remain real.
vi.mock("../diffs/AnnotatableCodeView", () => ({ AnnotatableCodeView: () => <div /> }));
vi.mock("../DiffWorkerPoolProvider", () => ({
  DiffWorkerPoolProvider: ({ children }: { children: ReactNode }) => children,
}));

import { GitManagerPanel } from "./GitManagerPanel";

it("returns from real History PNG selection to exactly one visible working-tree text option", async () => {
  // Two inert 64px PNG swatches keep this regression independent of desktop QA tooling.
  h.swatches = [
    "iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAIAAAAlC+aJAAAAeklEQVR4nO3PUQkAIBTAwBfHsEY0jCH8OITBAtxm7fN1wwUNaEEDWtCAFjSgBQ1oQQNa0IAWNKAFDWhBA1rQgBY0oAUNaEEDWtCAFjSgBQ1oQQNa0IAWNKAFDWhBA1rQgBY0oAUNaEEDWtCAFjSgBQ1oQQNa0IAWPHYBSIgBeLUco5MAAAAASUVORK5CYII=",
    "iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAIAAAAlC+aJAAAAeklEQVR4nO3PUQkAIBTAwBfH2EY0giH8OITBAtzm7PV1wwUNaEEDWtCAFjSgBQ1oQQNa0IAWNKAFDWhBA1rQgBY0oAUNaEEDWtCAFjSgBQ1oQQNa0IAWNKAFDWhBA1rQgBY0oAUNaEEDWtCAFjSgBQ1oQQNa0IAWPHYBks2BloRD0lIAAAAASUVORK5CYII=",
  ];
  const environmentId = EnvironmentId.make("local"),
    projectId = ProjectId.make(h.project.id);
  const branch = "codex/delivery-retry-light";
  const refs: GitManagerRefsSnapshot = {
    conflictedPaths: [],
    defaultBranch: "main",
    detachedSha: null,
    generation: 1,
    headRef: branch,
    inProgressOperation: null,
    isDirty: true,
    localBranches: [
      {
        ahead: 0,
        behind: 0,
        blocked: [],
        current: true,
        isDefault: false,
        name: branch,
        tipSha: "a".repeat(40),
        upstream: null,
        worktreePath: "/owned/managed",
      },
    ],
    remoteBranches: [],
    remotes: [],
    tags: [],
    worktrees: [],
  };
  const commit: GitManagerCommitEntry = {
    sha: "a".repeat(40),
    shortSha: "aaaaaaa",
    parents: ["b".repeat(40)],
    decorations: ["visual-base"],
    subject: "Visual qualification baseline",
    body: "",
    authorName: "Owned fixture",
    authorEmail: "fixture@example.test",
    authoredAtMs: 1000,
    committerName: "Owned fixture",
    committerEmail: "fixture@example.test",
    committedAtMs: 1000,
    changedFiles: ["pierre-step5.ts", "visual-swatch.png"],
  };
  h.refs = refs;
  h.page = {
    generation: 1,
    pinnedTips: [commit.sha],
    commits: [commit],
    exhausted: true,
    nextOffset: null,
    degradedToAllPaging: false,
  };
  h.status = {
    isRepo: true,
    hasPrimaryRemote: false,
    hasWorkingTreeChanges: true,
    isDefaultRef: false,
    defaultRefName: "main",
    refName: branch,
    aheadCount: 0,
    behindCount: 0,
    aheadOfDefaultCount: 0,
    hasUpstream: false,
    pr: null,
    sourceControlProvider: null,
    workingTree: {
      insertions: 1,
      deletions: 1,
      files: [
        {
          path: "pierre-step5.ts",
          status: "modified",
          area: "unstaged",
          insertions: 1,
          deletions: 1,
        },
        {
          path: "visual-swatch.png",
          status: "modified",
          area: "unstaged",
          insertions: 0,
          deletions: 0,
        },
      ],
    },
  };
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  const rectangle = function (this: HTMLElement) {
    return this.closest("[hidden],[inert]") ? new DOMRect() : new DOMRect(0, 0, 900, 450);
  };
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockImplementation(rectangle);
  vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockImplementation(function (
    this: HTMLElement,
  ) {
    return this.closest("[hidden],[inert]") ? 0 : 450;
  });
  vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockReturnValue(900);
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  const tab = (name: string) =>
    [...container.querySelectorAll<HTMLElement>('[role="tab"]')].find(
      (node) => node.textContent?.trim() === name,
    )!;
  const firstText = () =>
    container.querySelector<HTMLElement>('[role="option"][data-path="pierre-step5.ts"]');
  try {
    await act(async () =>
      root.render(<GitManagerPanel projectRef={{ environmentId, projectId }} />),
    );
    await act(async () => container.querySelector<HTMLElement>('[aria-label="Worktree"]')!.click());
    const managed = [...document.querySelectorAll<HTMLElement>('[role="option"]')].find((node) =>
      node.textContent?.includes(branch),
    )!;
    expect(managed).toBeDefined();
    await act(async () => managed.click());
    await act(async () => tab("Changes").click());
    expect(firstText()).not.toBeNull();
    expect(firstText()!.getBoundingClientRect().height).toBeGreaterThan(0);
    await act(async () =>
      container
        .querySelector<HTMLElement>('[role="option"][data-path="visual-swatch.png"]')!
        .click(),
    );
    const workingPane = container.querySelector(
      'section[aria-label="Diff for visual-swatch.png"]',
    )!;
    expect(
      workingPane.querySelector('aside[aria-label="Partial staging selection gutter"]'),
    ).not.toBeNull();
    expect(workingPane.querySelectorAll("img")).toHaveLength(0);
    const commitForm = container.querySelector<HTMLFormElement>(
      'form[aria-label="Commit Changes"]',
    )!;
    const summary = commitForm.querySelector<HTMLInputElement>(
      'input[name="git-manager-summary"]',
    )!;
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(
        summary,
        "Owned layout draft",
      );
      summary.dispatchEvent(new Event("input", { bubbles: true }));
      summary.dispatchEvent(new Event("change", { bubbles: true }));
    });
    expect(summary.value).toBe("Owned layout draft");
    await act(async () => tab("History").click());
    const baseline = [...container.querySelectorAll<HTMLElement>('button[role="option"]')].find(
      (node) => node.getAttribute("aria-label")?.includes("Visual qualification baseline"),
    )!;
    expect(baseline).toBeDefined();
    await act(async () => baseline.click());
    await act(async () =>
      container.querySelector<HTMLElement>('[data-changed-file-path="visual-swatch.png"]')!.click(),
    );
    expect(
      container.querySelector('section[aria-label="Image diff"]')?.querySelectorAll("img"),
    ).toHaveLength(2);
    expect(firstText()).toBeNull();
    await act(async () => tab("Changes").click());
    expect(tab("Changes").getAttribute("aria-selected")).toBe("true");
    expect(container.querySelectorAll('[role="option"][data-path="pierre-step5.ts"]')).toHaveLength(
      1,
    );
    const text = firstText()!;
    const changes = text.closest('section[aria-label="Changes"]')!;
    const list = changes.querySelector<HTMLElement>(
      '[role="listbox"][aria-label="Changed files"]',
    )!;
    const retainedImage = changes.querySelector<HTMLElement>(
      'section[aria-label="Diff for visual-swatch.png"]',
    )!;
    const retainedCommit = changes.querySelector<HTMLFormElement>(
      'form[aria-label="Commit Changes"]',
    )!;
    expect(retainedImage).not.toBeNull();
    expect(list.parentElement).toBe(changes);
    expect(retainedImage.parentElement).toBe(changes);
    expect(retainedCommit.parentElement).toBe(changes);
    expect(
      retainedCommit.querySelector<HTMLInputElement>('input[name="git-manager-summary"]')?.value,
    ).toBe("Owned layout draft");
    // Declared allocation contract only: Happy DOM does not resolve native flex heights.
    for (const flexible of [list, retainedImage]) {
      expect(flexible.classList.contains("flex-1")).toBe(true);
      expect(flexible.classList.contains("min-h-0")).toBe(true);
      expect(
        [...flexible.classList].some(
          (token) =>
            token === "shrink-0" ||
            token.startsWith("h-") ||
            (token.startsWith("min-h-") && token !== "min-h-0"),
        ),
      ).toBe(false);
    }
    expect(
      retainedImage.querySelector('aside[aria-label="Partial staging selection gutter"]'),
    ).not.toBeNull();
    expect(changes.querySelectorAll('section[aria-label^="Diff for "]')).toHaveLength(1);
    expect(text.closest("[hidden],[inert]")).toBeNull();
    expect(text.getBoundingClientRect().height).toBeGreaterThan(0);
    await act(async () => text.click());
    expect(text.getAttribute("aria-selected")).toBe("true");
    expect(
      container.querySelector('section[aria-label="Diff for pierre-step5.ts"]'),
    ).not.toBeNull();
  } finally {
    await act(async () => root.unmount());
    container.remove();
    vi.restoreAllMocks();
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
  }
}, 5000);
