// @vitest-environment happy-dom

import { EnvironmentId, ProjectId } from "@coddy/contracts";
import { act, useState, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vite-plus/test";

import { buildChangeRows, type ChangeRow } from "./changesList.logic";
import { GitManagerChangesList } from "./GitManagerChangesList";
import { GitManagerDiffPane } from "./GitManagerDiffPane";

const query = vi.hoisted(() => ({
  getDiff: vi.fn((request: unknown) => request),
}));
vi.mock("../../../state/gitManager", () => ({
  gitManagerEnvironment: {
    getDiff: query.getDiff,
    stagePartial: "stage",
    unstagePartial: "unstage",
  },
}));
vi.mock("../../../state/query", () => ({
  useEnvironmentQuery: (request: { input: { source: { path: string; staged: boolean } } }) => {
    const { path, staged } = request.input.source;
    const prefix = `:100644 100644 1111111 0000000 M\0${path}\0\0diff --git a/${path} b/${path}\nindex 1111111..2222222 100644\n`;
    const patch =
      prefix +
      (path.endsWith(".png")
        ? `Binary files a/${path} and b/${path} differ\n`
        : `--- a/${path}\n+++ b/${path}\n@@ -1 +1 @@\n-before\n+after\n`);
    return {
      data: {
        _tag: "patch",
        generation: 1,
        source: { _tag: "working-tree", path, staged },
        byteLength: patch.length,
        longestLineLength: 80,
        patch,
      },
      error: null,
      isPending: false,
      revalidate: () => {},
    };
  },
}));
vi.mock("../../../state/use-atom-command", () => ({ useAtomCommand: () => vi.fn() }));
vi.mock("../../../hooks/useTheme", () => ({ useTheme: () => ({ resolvedTheme: "light" }) }));
vi.mock("../../../composerDraftStore", () => ({ DraftId: { make: (value: string) => value } }));
vi.mock("../../../gitManagerStore", () => {
  const defaults = { lineSelectionByPath: {} };
  const snapshot = {
    byProjectKey: {},
    setLineSelection: vi.fn(),
    selectViewState: () => defaults,
  };
  return {
    DEFAULT_GIT_MANAGER_VIEW_STATE: defaults,
    useGitManagerStore: Object.assign(
      (select: (state: typeof snapshot) => unknown) => select(snapshot),
      { getState: () => snapshot },
    ),
  };
});
// Query/worker boundaries are inert; the installed list, row, parser, pane, and gutter execute.
vi.mock("../../diffs/AnnotatableCodeView", () => ({
  AnnotatableCodeView: () => <div data-testid="inert-code-renderer" />,
}));
vi.mock("../../DiffWorkerPoolProvider", () => ({
  DiffWorkerPoolProvider: ({ children }: { children: ReactNode }) => children,
}));

const environmentId = EnvironmentId.make("local");
const projectId = ProjectId.make("owned");
const projectRef = { environmentId, projectId };
const scope = { environmentId, cwd: "/owned" };
const imagePath = "visual-swatch.png";
const textPath = "pierre-step5.ts";
const rows = Object.freeze(
  buildChangeRows({
    files: [
      { path: textPath, status: "modified", area: "unstaged", insertions: 1, deletions: 1 },
      { path: imagePath, status: "modified", area: "unstaged", insertions: 0, deletions: 0 },
    ],
    conflictedPaths: [],
    submodulePaths: [],
    filterText: "",
    excludedPaths: new Set(),
  }),
);
const noop = () => {};

function SelectionFixture() {
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const [currentRows, setCurrentRows] = useState<ReadonlyArray<ChangeRow>>(() => rows);
  return (
    <>
      <output data-testid="selected-path">{selectedPath ?? "none"}</output>
      <button type="button" onClick={() => setSelectedPath(null)}>
        Clear selection
      </button>
      <button
        type="button"
        onClick={() => setCurrentRows((current) => current.map((row) => ({ ...row })))}
      >
        Refresh unchanged status
      </button>
      <GitManagerChangesList
        rows={currentRows}
        selectedPath={selectedPath}
        onSelect={setSelectedPath}
        onToggle={noop}
        onContextMenu={noop}
        onOpenExternal={noop}
      />
      {selectedPath === null ? null : (
        <GitManagerDiffPane
          key={selectedPath}
          scope={scope}
          projectRef={projectRef}
          path={selectedPath}
          availableAreas={["unstaged"]}
          mutationBusy={false}
          partialStagingDisabledReason={null}
        />
      )}
    </>
  );
}

it("keeps real virtualized row selection aligned with the selected diff for unchanged rows", async () => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue(
    new DOMRect(0, 0, 900, 290),
  );
  vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(290);
  vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockReturnValue(900);
  const fetchSpy = vi.fn(() => {
    throw new Error("No network in the inert selection seam.");
  });
  vi.stubGlobal("fetch", fetchSpy);
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  const option = (path: string) =>
    container.querySelector<HTMLElement>(`[role="option"][data-path="${path}"]`);
  const selectedOptions = () =>
    Array.from(container.querySelectorAll('[role="option"][aria-selected="true"]')).map((element) =>
      element.getAttribute("data-path"),
    );
  try {
    await act(async () => root.render(<SelectionFixture />));
    expect(container.querySelectorAll('[role="option"][data-path]')).toHaveLength(2);
    expect(selectedOptions()).toEqual([]);
    expect(container.querySelector('section[aria-label^="Diff for"]')).toBeNull();

    await act(async () => option(imagePath)!.click());
    expect(container.querySelector('[data-testid="selected-path"]')?.textContent).toBe(imagePath);
    const imagePane = container.querySelector('section[aria-label="Diff for visual-swatch.png"]');
    expect(imagePane).not.toBeNull();
    const gutter = imagePane?.querySelector('aside[aria-label="Partial staging selection gutter"]');
    expect(gutter).not.toBeNull();
    const stage = Array.from(gutter!.querySelectorAll("button")).find(
      (button) => button.textContent === "Stage selected lines",
    );
    expect(stage?.disabled).toBe(true);
    expect(gutter!.querySelectorAll('button[aria-label^="Toggle changed-line run"]')).toHaveLength(
      0,
    );
    expect(imagePane!.querySelectorAll("img")).toHaveLength(0);
    expect
      .soft(selectedOptions(), "PNG callback and pane must agree with its selected row")
      .toEqual([imagePath]);

    await act(async () => option(textPath)!.click());
    expect(container.querySelector('[data-testid="selected-path"]')?.textContent).toBe(textPath);
    expect(container.querySelector('section[aria-label="Diff for visual-swatch.png"]')).toBeNull();
    expect(
      container.querySelector('section[aria-label="Diff for pierre-step5.ts"]'),
    ).not.toBeNull();
    expect
      .soft(selectedOptions(), "text callback and pane must agree with its selected row")
      .toEqual([textPath]);
    expect(option(imagePath)?.getAttribute("aria-selected")).toBe("false");

    const control = (label: string) =>
      Array.from(container.querySelectorAll("button")).find(
        (button) => button.textContent === label,
      )!;
    const selectedTextRow = option(textPath);
    await act(async () => control("Refresh unchanged status").click());
    expect(option(textPath)).toBe(selectedTextRow);
    expect
      .soft(selectedOptions(), "an unchanged status tick must retain the current selection")
      .toEqual([textPath]);
    expect(
      container.querySelector('section[aria-label="Diff for pierre-step5.ts"]'),
    ).not.toBeNull();

    await act(async () => control("Clear selection").click());
    expect(container.querySelector('[data-testid="selected-path"]')?.textContent).toBe("none");
    expect(selectedOptions()).toEqual([]);
    expect(option(textPath)?.getAttribute("aria-selected")).toBe("false");
    expect(container.querySelector('section[aria-label^="Diff for"]')).toBeNull();
    expect(fetchSpy).not.toHaveBeenCalled();
  } finally {
    await act(async () => root.unmount());
    container.remove();
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
  }
}, 5_000);
