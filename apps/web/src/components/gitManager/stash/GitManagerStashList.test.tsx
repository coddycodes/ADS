// @vitest-environment happy-dom

import type {
  GitManagerBlockedReason,
  GitManagerDiff,
  GitManagerStashEntry,
  ScopedProjectRef,
} from "@bibcode/contracts";
import { GitManagerOperationError } from "@bibcode/contracts";
import * as Cause from "effect/Cause";
import { AsyncResult } from "effect/reactivity";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const h = vi.hoisted(() => ({
  buttons: [] as Array<Record<string, unknown>>,
  listProps: null as Record<string, unknown> | null,
  diff: null as GitManagerDiff | null,
  emission: { _tag: "Success" } as unknown,
  error: null as string | null,
  getDiff: vi.fn(() => ({ kind: "diff" })),
  parser: vi.fn(() => ({
    kind: "raw" as const,
    text: "raw patch",
    reason: "Parser fallback reason.",
  })),
}));

vi.mock("@legendapp/list/react", () => ({
  LegendList: (props: {
    data: ReadonlyArray<GitManagerStashEntry>;
    keyExtractor: (entry: GitManagerStashEntry) => string;
    renderItem: (input: { item: GitManagerStashEntry; index: number }) => React.ReactNode;
  }) => {
    h.listProps = props as unknown as Record<string, unknown>;
    return (
      <div>
        {props.data.map((item, index) => (
          <div key={props.keyExtractor(item)}>{props.renderItem({ item, index })}</div>
        ))}
      </div>
    );
  },
}));

vi.mock("~/components/ui/button", () => ({
  Button: (props: Record<string, unknown>) => {
    h.buttons.push(props);
    return (
      <button
        aria-describedby={props["aria-describedby"] as string | undefined}
        aria-label={props["aria-label"] as string | undefined}
        className={props.className as string | undefined}
        disabled={props.disabled as boolean | undefined}
        title={props.title as string | undefined}
        type="button"
      >
        {props.children as React.ReactNode}
      </button>
    );
  },
}));

vi.mock("~/components/ui/dialog", () => ({
  Dialog: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  DialogDescription: ({ children }: { children: React.ReactNode }) => <p>{children}</p>,
  DialogFooter: ({ children }: { children: React.ReactNode }) => <footer>{children}</footer>,
  DialogHeader: ({ children }: { children: React.ReactNode }) => <header>{children}</header>,
  DialogPopup: ({ children }: { children: React.ReactNode }) => <section>{children}</section>,
  DialogTitle: ({ children }: { children: React.ReactNode }) => <h2>{children}</h2>,
}));

vi.mock("../../../state/gitManager", () => ({
  gitManagerEnvironment: { getDiff: h.getDiff },
}));

vi.mock("../../../state/query", () => ({
  useEnvironmentQuery: () => ({
    data: h.diff,
    emission: h.emission,
    error: h.error,
    isPending: false,
    refresh: () => undefined,
  }),
}));

vi.mock("../../../hooks/useTheme", () => ({
  useTheme: () => ({ resolvedTheme: "dark" }),
}));

vi.mock("../../../lib/diffRendering", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../../lib/diffRendering")>();
  return { ...actual, getRenderablePatch: h.parser };
});

vi.mock("../../diffs/AnnotatableCodeView", () => ({
  AnnotatableCodeView: () => <div data-testid="annotatable-diff" />,
}));

vi.mock("../../DiffWorkerPoolProvider", () => ({
  DiffWorkerPoolProvider: ({ children }: { children?: React.ReactNode }) => children,
}));

import { GitManagerStashDiff } from "./GitManagerStashDiff";
import { GitManagerStashList } from "./GitManagerStashList";

const entry: GitManagerStashEntry = {
  index: 0,
  sha: "stash-sha",
  message: "On main: checkpoint",
  committedAtMs: 1,
  parents: ["parent-sha"],
  files: [{ path: "src/file.ts", status: "modified", insertions: 3, deletions: 1 }],
};
const blocked: GitManagerBlockedReason = {
  operation: "stash-apply",
  code: "operation-in-flight",
  message: "Server says this repository operation must finish first.",
};
const projectRef = { environmentId: "env-a", projectId: "project-a" } as ScopedProjectRef;

function renderList(overrides: Partial<React.ComponentProps<typeof GitManagerStashList>> = {}) {
  const onSelectStash = vi.fn();
  const markup = renderToStaticMarkup(
    <GitManagerStashList
      scope={{ environmentId: projectRef.environmentId, cwd: "/repo" }}
      projectRef={projectRef}
      entries={[entry]}
      blockedReasons={[blocked]}
      selectedSha={null}
      operationInFlight
      onApply={() => undefined}
      onDrop={() => undefined}
      onPop={() => undefined}
      onSelectStash={onSelectStash}
      {...overrides}
    />,
  );
  return { markup, onSelectStash };
}

beforeEach(() => {
  h.buttons.length = 0;
  h.listProps = null;
  h.diff = {
    _tag: "patch",
    generation: 1,
    source: { _tag: "stash", sha: entry.sha, path: "src/file.ts" },
    byteLength: 9,
    longestLineLength: 9,
    patch: "not a structured patch",
  };
  h.emission = AsyncResult.success(h.diff);
  h.error = null;
  h.getDiff.mockClear();
  h.parser.mockClear();
});

describe("GitManagerStashList", () => {
  it("virtualizes fixed 29px rows and selects the stable stash sha", () => {
    const { markup, onSelectStash } = renderList();

    expect(markup).toContain("h-[29px]");
    expect(h.listProps?.estimatedItemSize).toBe(29);
    expect((h.listProps?.getFixedItemSize as (() => number) | undefined)?.()).toBe(29);

    const select = h.buttons.find((props) => props["aria-label"] === "Select stash stash@{0}");
    expect(select).toBeDefined();
    (select?.onClick as (() => void) | undefined)?.();
    expect(onSelectStash).toHaveBeenCalledOnce();
    expect(onSelectStash).toHaveBeenCalledWith("stash-sha");
  });

  it("labels every icon action and exposes a server block by tooltip and description", () => {
    const { markup } = renderList();

    for (const label of ["Apply stash@{0}", "Pop stash@{0}", "Drop stash@{0}"]) {
      const action = h.buttons.find((props) => props["aria-label"] === label);
      expect(action).toMatchObject({
        disabled: true,
        title: blocked.message,
      });
      expect(action?.["aria-describedby"]).toBe("git-manager-stash-stash-sha-blocked");
    }
    expect(markup).toContain(blocked.message);
  });

  it("disables every stash mutation with the capability reason while stash browsing still works", () => {
    const reason = "This environment does not support Git Manager stash and merge operations.";
    const { markup, onSelectStash } = renderList({
      blockedReasons: [],
      disabledReason: reason,
      operationInFlight: false,
    });

    for (const label of ["Apply stash@{0}", "Pop stash@{0}", "Drop stash@{0}"]) {
      expect(h.buttons.find((props) => props["aria-label"] === label)).toMatchObject({
        disabled: true,
        title: reason,
      });
    }
    expect(markup).toContain(reason);
    const select = h.buttons.find((props) => props["aria-label"] === "Select stash stash@{0}");
    expect(select?.disabled).not.toBe(true);
    (select?.onClick as (() => void) | undefined)?.();
    expect(onSelectStash).toHaveBeenCalledWith("stash-sha");
  });
});

describe("GitManagerStashDiff", () => {
  let container: HTMLDivElement;
  let root: Root | null;

  async function renderDiff(
    entries: ReadonlyArray<GitManagerStashEntry>,
    selectedStashSha: string | null,
    onRefreshStashes: () => void,
  ) {
    await act(async () =>
      root?.render(
        <GitManagerStashDiff
          scope={{ environmentId: "env-a" as never, cwd: "/repo" }}
          projectRef={projectRef}
          entries={entries}
          selectedStashSha={selectedStashSha}
          selectedPath="src/file.ts"
          onRefreshStashes={onRefreshStashes}
          onSelectPath={() => undefined}
        />,
      ),
    );
  }

  beforeEach(() => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
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

  it("requests one path by stash sha and renders a raw parser fallback reason", async () => {
    await renderDiff([entry], entry.sha, () => undefined);

    expect(h.getDiff).toHaveBeenCalledWith({
      environmentId: "env-a",
      input: {
        cwd: "/repo",
        source: { _tag: "stash", sha: "stash-sha", path: "src/file.ts" },
      },
    });
    expect(h.parser).toHaveBeenCalledWith("not a structured patch", "git-manager-stash");
    expect(container.textContent).toContain("Parser fallback reason.");
    expect(container.textContent).toContain("raw patch");
  });

  it("refetches the list without requesting a diff when the selected sha is gone", async () => {
    const refresh = vi.fn();
    await renderDiff([], entry.sha, refresh);

    expect(container.textContent).toContain("entry no longer present");
    expect(refresh).toHaveBeenCalledOnce();
    expect(h.getDiff).not.toHaveBeenCalled();
  });

  it("treats a structured stash-not-found diff failure as an expected refetch", async () => {
    const refresh = vi.fn();
    h.diff = null;
    h.error = "The selected stash was not found.";
    h.emission = AsyncResult.failure(
      Cause.fail(
        new GitManagerOperationError({
          operation: "get-diff",
          code: "stash-not-found",
          message: "The selected stash was not found.",
          blocked: null,
        }),
      ),
    );

    await renderDiff([entry], entry.sha, refresh);

    expect(refresh).toHaveBeenCalledOnce();
    expect(container.textContent).toContain("entry no longer present");
    expect(container.textContent).not.toContain("The selected stash was not found.");
  });
});
