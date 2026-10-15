// @vitest-environment happy-dom

import {
  GitManagerOperationError,
  type GitManagerCommitEntry,
  type GitManagerCommitPage,
  type ScopedProjectRef,
} from "@bibcode/contracts";
import * as Cause from "effect/Cause";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import { useGitManagerStore } from "~/gitManagerStore";
import { textSizesBelowTextXs } from "~/test/uiTypography";

interface NextPageRequest {
  readonly offset: number;
  readonly attempt: number;
}

interface NextPageResult {
  readonly data: GitManagerCommitPage | null;
  readonly error: string | null;
  readonly failure?: GitManagerOperationError;
}

const PENDING_NEXT_PAGE: NextPageResult = { data: null, error: null };

const h = vi.hoisted(() => ({
  listProps: null as Record<string, unknown> | null,
  commitPage: null as {
    generation: number;
    pinnedTips: ReadonlyArray<string>;
    commits: ReadonlyArray<GitManagerCommitEntry>;
    nextOffset: number | null;
    exhausted: boolean;
    degradedToAllPaging: boolean;
  } | null,
  retainedPages: null as ReadonlyArray<GitManagerCommitPage> | null,
  retainedInputs: [] as unknown[],
  firstPageInputs: [] as Array<{ input: { refreshCacheKey: string } }>,
  firstPageWaiting: false,
  firstPageError: null as string | null,
  retainedError: null as string | null,
  retainedWaiting: false,
  retryRetained: vi.fn(),
  contextMenuShow: vi.fn(),
  dndProps: null as Record<string, unknown> | null,
  refreshCommits: vi.fn(),
  revalidateFirstPage: vi.fn(),
  scrollNode: null as HTMLElement | null,
  nextPageRequests: [] as number[],
  nextPageResult: (_request: NextPageRequest): NextPageResult => PENDING_NEXT_PAGE,
  retryNextPage: vi.fn(),
}));

vi.mock("@legendapp/list/react", async () => {
  const React = await import("react");
  return {
    LegendList: (props: {
      data: ReadonlyArray<GitManagerCommitEntry>;
      keyExtractor: (commit: GitManagerCommitEntry) => string;
      renderItem: (input: { item: GitManagerCommitEntry; index: number }) => React.ReactNode;
      ref?: React.Ref<unknown>;
    }) => {
      h.listProps = props as unknown as Record<string, unknown>;
      // Only tests that measure the scroll node get a list handle.
      React.useImperativeHandle(props.ref, () =>
        h.scrollNode === null ? null : { getScrollableNode: () => h.scrollNode },
      );
      return (
        <div>
          {props.data.map((item, index) => (
            <div key={props.keyExtractor(item)}>{props.renderItem({ item, index })}</div>
          ))}
        </div>
      );
    },
  };
});

vi.mock("../../../localApi", () => ({
  readLocalApi: () => ({ contextMenu: { show: h.contextMenuShow } }),
}));

vi.mock("../../../state/gitManager", () => ({
  gitManagerEnvironment: {
    getCommits: vi.fn((request: { input: { offset: number } }) => {
      const offset = request.input.offset;
      h.nextPageRequests.push(offset);
      const attempt = h.nextPageRequests.filter((requested) => requested === offset).length;
      return { kind: "next", offset, attempt };
    }),
    getHistoryFirstPage: vi.fn((input: { input: { refreshCacheKey: string } }) => {
      h.firstPageInputs.push(input);
      return { kind: "commits" };
    }),
    getRetainedCommitPages: vi.fn((input: unknown) => {
      h.retainedInputs.push(input);
      return { kind: "retained" };
    }),
  },
}));

vi.mock("../../../state/query", () => ({
  useEnvironmentQuery: (atom: ({ kind: string } & Partial<NextPageRequest>) | null) => {
    const next =
      atom?.kind === "next"
        ? h.nextPageResult({ offset: atom.offset ?? 0, attempt: atom.attempt ?? 0 })
        : null;
    return {
      data:
        atom?.kind === "commits"
          ? h.commitPage
          : atom?.kind === "retained"
            ? h.retainedPages
            : (next?.data ?? null),
      emission:
        next?.failure === undefined
          ? {
              _tag: "Initial",
              waiting:
                atom?.kind === "commits"
                  ? h.firstPageWaiting
                  : atom?.kind === "retained"
                    ? h.retainedWaiting
                    : false,
            }
          : { _tag: "Failure", cause: Cause.fail(next.failure), waiting: false },
      error:
        atom?.kind === "retained"
          ? h.retainedError
          : atom?.kind === "commits"
            ? h.firstPageError
            : (next?.error ?? null),
      isPending: false,
      refresh:
        atom?.kind === "commits"
          ? h.refreshCommits
          : atom?.kind === "next"
            ? h.retryNextPage
            : h.retryRetained,
      revalidate: atom?.kind === "commits" ? h.revalidateFirstPage : vi.fn(),
      requiresRetry: false,
    };
  },
}));

vi.mock("../rewrite/gitManagerCommitDrag", () => ({
  GitManagerCommitDndContext: (props: Record<string, unknown>) => {
    h.dndProps = props;
    return <>{props.children as React.ReactNode}</>;
  },
  GitManagerCommitInsertionTarget: () => null,
  useGitManagerCommitDragSource: () => ({
    attributes: {},
    listeners: {},
    setNodeRef: () => undefined,
    isDragging: false,
    transform: undefined,
  }),
}));

import { GitManagerCommitList } from "./GitManagerCommitList";
import { GitManagerHistoryView } from "./GitManagerHistoryView";

let container: HTMLDivElement;
let root: Root | null;

function commit(index: number): GitManagerCommitEntry {
  const sha = index.toString(16).padStart(40, "0");
  return {
    sha,
    shortSha: sha.slice(0, 7),
    parents: [],
    decorations: [],
    subject: `Commit ${index}`,
    body: "",
    authorName: "Local Author",
    authorEmail: "local@example.test",
    authoredAtMs: index,
    committerName: "Local Author",
    committerEmail: "local@example.test",
    committedAtMs: index,
    changedFiles: [],
  };
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  h.listProps = null;
  h.scrollNode = null;
  h.commitPage = null;
  h.retainedPages = null;
  h.retainedInputs.length = 0;
  h.firstPageInputs.length = 0;
  h.firstPageWaiting = false;
  h.firstPageError = null;
  h.retainedError = null;
  h.retainedWaiting = false;
  h.retryRetained.mockReset();
  h.contextMenuShow.mockReset();
  h.dndProps = null;
  h.refreshCommits.mockReset();
  h.revalidateFirstPage.mockReset();
  h.nextPageRequests.length = 0;
  h.nextPageResult = () => PENDING_NEXT_PAGE;
  h.retryNextPage.mockReset();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  vi.restoreAllMocks();
  await act(async () => root?.unmount());
  root = null;
  container.remove();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("GitManagerCommitList", () => {
  it("sets every row's text at 12 px or larger (UI.md typography)", async () => {
    const decorated = { ...commit(1), decorations: ["main"] };
    await act(async () =>
      root?.render(
        <GitManagerCommitList
          commits={[decorated, commit(2)]}
          selectedSha={null}
          onSelect={() => undefined}
          onReachEnd={() => undefined}
          isLoadingMore={false}
        />,
      ),
    );
    const markup = container.innerHTML;
    expect(markup).toContain(decorated.shortSha);
    expect(markup).toContain(">main<");
    expect(textSizesBelowTextXs(markup)).toEqual([]);
  });

  it("requests one next page when the tenth-from-last row becomes visible", async () => {
    const commits = Array.from({ length: 100 }, (_, index) => commit(index));
    const onReachEnd = vi.fn();
    vi.spyOn(Date, "now").mockReturnValue(1_000);

    await act(async () =>
      root?.render(
        <GitManagerCommitList
          commits={commits}
          selectedSha={null}
          onSelect={() => undefined}
          onReachEnd={onReachEnd}
          isLoadingMore={false}
        />,
      ),
    );

    const onViewableItemsChanged = h.listProps?.onViewableItemsChanged as
      | ((input: { end: number }) => void)
      | undefined;
    expect(onViewableItemsChanged).toBeTypeOf("function");
    act(() => {
      onViewableItemsChanged?.({ end: 90 });
      onViewableItemsChanged?.({ end: 90 });
    });

    expect(onReachEnd).toHaveBeenCalledTimes(1);
    expect(h.listProps?.estimatedItemSize).toBe(50);
    expect((h.listProps?.getFixedItemSize as (() => number) | undefined)?.()).toBe(50);
  });

  function scrollNode(scrollHeight: number, clientHeight: number): HTMLElement {
    const node = document.createElement("div");
    Object.defineProperty(node, "scrollHeight", { configurable: true, value: scrollHeight });
    Object.defineProperty(node, "clientHeight", { configurable: true, value: clientHeight });
    return node;
  }

  it("asks for the next page when a short page does not fill the list", async () => {
    // A byte-capped History page can hold a few large commits that fit the viewport.
    h.scrollNode = scrollNode(600, 700);
    const onReachEnd = vi.fn();
    vi.spyOn(Date, "now").mockReturnValue(1_000);
    await act(async () =>
      root?.render(
        <GitManagerCommitList
          commits={Array.from({ length: 12 }, (_, index) => commit(index))}
          selectedSha={null}
          onSelect={() => undefined}
          onReachEnd={onReachEnd}
          isLoadingMore={false}
        />,
      ),
    );
    expect(onReachEnd).toHaveBeenCalledTimes(1);
    // Layout reports again within the throttle window: still one request.
    act(() => (h.listProps?.onLayout as (() => void) | undefined)?.());
    expect(onReachEnd).toHaveBeenCalledTimes(1);
  });

  function renderList(
    commits: ReadonlyArray<GitManagerCommitEntry>,
    isLoadingMore: boolean,
    onReachEnd: () => void,
  ) {
    return act(async () =>
      root?.render(
        <GitManagerCommitList
          commits={commits}
          selectedSha={null}
          onSelect={() => undefined}
          onReachEnd={onReachEnd}
          isLoadingMore={isLoadingMore}
        />,
      ),
    );
  }

  it("keeps paging while the loaded pages still do not fill the list", async () => {
    vi.useFakeTimers({ now: 1_000 });
    try {
      h.scrollNode = scrollNode(600, 700);
      const onReachEnd = vi.fn();
      const firstPage = Array.from({ length: 12 }, (_, index) => commit(index));
      await renderList(firstPage, false, onReachEnd);
      expect(onReachEnd).toHaveBeenCalledTimes(1);
      await renderList(firstPage, true, onReachEnd);
      // The second short page lands 100 ms later, inside the request interval.
      vi.advanceTimersByTime(100);
      h.scrollNode = scrollNode(1_200, 700);
      const twoPages = Array.from({ length: 24 }, (_, index) => commit(index));
      await renderList(twoPages, false, onReachEnd);
      expect(onReachEnd).toHaveBeenCalledTimes(1);
      await act(async () => {
        vi.advanceTimersByTime(400);
      });
      expect(onReachEnd).toHaveBeenCalledTimes(2);
    } finally {
      vi.useRealTimers();
    }
  });

  it("does not ask again after a page that added no rows (failed, latched or exhausted)", async () => {
    vi.useFakeTimers({ now: 1_000 });
    try {
      h.scrollNode = scrollNode(600, 700);
      const onReachEnd = vi.fn();
      const firstPage = Array.from({ length: 12 }, (_, index) => commit(index));
      await renderList(firstPage, false, onReachEnd);
      await renderList(firstPage, true, onReachEnd);
      await act(async () => {
        vi.advanceTimersByTime(5_000);
      });
      // The request settled without new rows: loading ends, the same rows remain.
      await renderList(firstPage, false, onReachEnd);
      act(() => (h.listProps?.onLayout as (() => void) | undefined)?.());
      await act(async () => {
        vi.advanceTimersByTime(5_000);
      });
      expect(onReachEnd).toHaveBeenCalledTimes(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("does not ask while the rows fill the list or before it is laid out", async () => {
    const onReachEnd = vi.fn();
    vi.spyOn(Date, "now").mockReturnValue(1_000);
    for (const node of [scrollNode(5_000, 700), scrollNode(600, 0)]) {
      h.scrollNode = node;
      await act(async () =>
        root?.render(
          <GitManagerCommitList
            commits={Array.from({ length: 100 }, (_, index) => commit(index))}
            selectedSha={null}
            onSelect={() => undefined}
            onReachEnd={onReachEnd}
            isLoadingMore={false}
          />,
        ),
      );
      act(() => (h.listProps?.onLayout as (() => void) | undefined)?.());
    }
    expect(onReachEnd).not.toHaveBeenCalled();
  });

  it("moves selection with the arrow keys and exposes a useful row name", async () => {
    const commits = [commit(0), commit(1), commit(2)];
    const onSelect = vi.fn();

    await act(async () =>
      root?.render(
        <GitManagerCommitList
          commits={commits}
          selectedSha={commits[0]!.sha}
          onSelect={onSelect}
          onReachEnd={() => undefined}
          isLoadingMore={false}
        />,
      ),
    );

    const firstRow = container.querySelector<HTMLButtonElement>(
      `button[aria-label="${commits[0]!.shortSha} ${commits[0]!.subject}"]`,
    );
    expect(firstRow).not.toBeNull();
    act(() => {
      firstRow?.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
    });

    expect(onSelect).toHaveBeenCalledWith(commits[1]!.sha);
  });
});

describe("GitManagerHistoryView rewrite reachability", () => {
  it("opens the existing commit menu from the list and forwards the chosen operation", async () => {
    const selectedCommit = commit(7);
    h.commitPage = {
      generation: 1,
      pinnedTips: [selectedCommit.sha],
      commits: [selectedCommit],
      nextOffset: null,
      exhausted: true,
      degradedToAllPaging: false,
    };
    h.contextMenuShow.mockResolvedValue("cherry-pick");
    const onAction = vi.fn();

    await act(async () =>
      root?.render(
        <GitManagerHistoryView
          branchSyncDisabledReason={null}
          blockedReasons={[
            {
              operation: "reset",
              code: "operation-in-flight",
              message: "Server says the repository mutation lane is occupied.",
            },
          ]}
          repositoryGeneration={null}
          signalGeneration={null}
          signalPending={false}
          projectRef={{ environmentId: "environment-1", projectId: "project-1" } as never}
          rewriteDisabledReason={null}
          scope={{ environmentId: "environment-1" as never, cwd: "/opaque/repository" }}
          tagDisabledReason={null}
          onAction={onAction}
        />,
      ),
    );

    const row = container.querySelector<HTMLButtonElement>(
      `button[data-commit-sha="${selectedCommit.sha}"]`,
    );
    expect(row).not.toBeNull();
    await act(async () => {
      row?.dispatchEvent(
        new MouseEvent("contextmenu", {
          bubbles: true,
          clientX: 17,
          clientY: 29,
        }),
      );
      await Promise.resolve();
    });

    expect(h.contextMenuShow).toHaveBeenCalledOnce();
    const [items, position] = h.contextMenuShow.mock.calls[0] ?? [];
    expect(position).toEqual({ x: 17, y: 29 });
    expect(items).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ id: "cherry-pick", label: "Cherry-Pick", disabled: false }),
        expect.objectContaining({
          id: "reset",
          disabled: true,
          label: expect.stringContaining("Server says the repository mutation lane is occupied."),
        }),
      ]),
    );
    expect(onAction).toHaveBeenCalledWith({
      _tag: "cherry-pick",
      shas: [selectedCommit.sha],
    });
  });

  it("forwards commit-list drag reorder results through the history parent", async () => {
    const selectedCommit = commit(8);
    h.commitPage = {
      generation: 1,
      pinnedTips: [selectedCommit.sha],
      commits: [selectedCommit],
      nextOffset: null,
      exhausted: true,
      degradedToAllPaging: false,
    };
    const onAction = vi.fn();

    await act(async () =>
      root?.render(
        <GitManagerHistoryView
          branchSyncDisabledReason={null}
          blockedReasons={[]}
          repositoryGeneration={null}
          signalGeneration={null}
          signalPending={false}
          projectRef={{ environmentId: "environment-1", projectId: "project-1" } as never}
          rewriteDisabledReason={null}
          scope={{ environmentId: "environment-1" as never, cwd: "/opaque/repository" }}
          tagDisabledReason={null}
          onAction={onAction}
        />,
      ),
    );

    const onCommitDrop = h.dndProps?.onCommitDrop;
    expect(onCommitDrop).toBeTypeOf("function");
    await act(async () =>
      (onCommitDrop as (resolution: unknown) => void)({
        _tag: "reorder",
        shas: [selectedCommit.sha],
        insertBeforeSha: null,
      }),
    );

    expect(onAction).toHaveBeenCalledWith({
      _tag: "reorder",
      shas: [selectedCommit.sha],
      insertBeforeSha: null,
    });
  });

  it("makes contiguous multi-commit actions reachable with standard range selection", async () => {
    const commits = [commit(10), commit(11), commit(12)];
    h.commitPage = {
      generation: 1,
      pinnedTips: commits.map((entry) => entry.sha),
      commits,
      nextOffset: null,
      exhausted: true,
      degradedToAllPaging: false,
    };
    h.contextMenuShow.mockResolvedValue(null);

    await act(async () =>
      root?.render(
        <GitManagerHistoryView
          branchSyncDisabledReason={null}
          blockedReasons={[]}
          repositoryGeneration={null}
          signalGeneration={null}
          signalPending={false}
          projectRef={{ environmentId: "environment-1", projectId: "project-1" } as never}
          rewriteDisabledReason={null}
          scope={{ environmentId: "environment-1" as never, cwd: "/opaque/repository" }}
          tagDisabledReason={null}
          onAction={vi.fn()}
        />,
      ),
    );
    const rows = commits.map((entry) =>
      container.querySelector<HTMLButtonElement>(`button[data-commit-sha="${entry.sha}"]`),
    );
    await act(async () => rows[0]?.click());
    await act(async () =>
      rows[2]?.dispatchEvent(new MouseEvent("click", { bubbles: true, shiftKey: true })),
    );
    await act(async () => {
      rows[2]?.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true }));
      await Promise.resolve();
    });

    const [items] = h.contextMenuShow.mock.calls[0] ?? [];
    expect(items).toEqual(
      expect.arrayContaining([
        expect.objectContaining({ id: "squash", label: "Squash 3", disabled: false }),
        expect.objectContaining({ id: "reorder", label: "Reorder 3", disabled: false }),
      ]),
    );
    expect(h.dndProps?.multiCommitSelection).toEqual(commits.map((entry) => entry.sha));
  });

  it("keeps rewrite entries disabled with their reason while branch, tag, and copy remain available", async () => {
    const selectedCommit = commit(13);
    const reason = "This environment does not support Git Manager rewrite operations.";
    h.commitPage = {
      generation: 1,
      pinnedTips: [selectedCommit.sha],
      commits: [selectedCommit],
      nextOffset: null,
      exhausted: true,
      degradedToAllPaging: false,
    };
    h.contextMenuShow.mockResolvedValue(null);

    await act(async () =>
      root?.render(
        <GitManagerHistoryView
          branchSyncDisabledReason={null}
          blockedReasons={[]}
          repositoryGeneration={null}
          signalGeneration={null}
          signalPending={false}
          projectRef={{ environmentId: "environment-1", projectId: "project-1" } as never}
          rewriteDisabledReason={reason}
          scope={{ environmentId: "environment-1" as never, cwd: "/opaque/repository" }}
          tagDisabledReason={null}
          onAction={vi.fn()}
        />,
      ),
    );
    await act(async () => {
      container
        .querySelector<HTMLButtonElement>(`button[data-commit-sha="${selectedCommit.sha}"]`)
        ?.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true }));
      await Promise.resolve();
    });

    const [items] = h.contextMenuShow.mock.calls[0] ?? [];
    expect(items).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          id: "reset",
          disabled: true,
          label: expect.stringContaining(reason),
        }),
        expect.objectContaining({
          id: "cherry-pick",
          disabled: true,
          label: expect.stringContaining(reason),
        }),
        expect.objectContaining({ id: "create-branch", disabled: false }),
        expect.objectContaining({ id: "create-tag", disabled: false }),
        expect.objectContaining({ id: "copy-sha", disabled: false }),
      ]),
    );
    expect(container.textContent).toContain(reason);
  });

  it("keeps explicit history refresh available without a repository generation", async () => {
    const selectedCommit = commit(14);
    h.commitPage = {
      generation: 1,
      pinnedTips: [selectedCommit.sha],
      commits: [selectedCommit],
      nextOffset: null,
      exhausted: true,
      degradedToAllPaging: false,
    };

    await act(async () =>
      root?.render(
        <GitManagerHistoryView
          branchSyncDisabledReason={null}
          blockedReasons={[]}
          repositoryGeneration={null}
          signalGeneration={null}
          signalPending={false}
          projectRef={{ environmentId: "environment-1", projectId: "project-1" } as never}
          rewriteDisabledReason={null}
          scope={{ environmentId: "environment-1" as never, cwd: "/opaque/repository" }}
          tagDisabledReason={null}
          onAction={vi.fn()}
        />,
      ),
    );

    expect(h.refreshCommits).not.toHaveBeenCalled();
    expect(container.textContent).toContain("Commit 14");
    expect(
      container.querySelector<HTMLButtonElement>('[aria-label="Refresh history"]'),
    ).not.toBeNull();
  });
});

describe("GitManagerHistoryView repository generation tracking", () => {
  function renderHistory(
    repositoryGeneration: number | null,
    signalGeneration: number | null = null,
    signalPending = false,
  ) {
    return act(async () =>
      root?.render(
        <GitManagerHistoryView
          branchSyncDisabledReason={null}
          blockedReasons={[]}
          repositoryGeneration={repositoryGeneration}
          signalGeneration={signalGeneration}
          signalPending={signalPending}
          projectRef={{ environmentId: "environment-1", projectId: "project-1" } as never}
          rewriteDisabledReason={null}
          scope={{ environmentId: "environment-1" as never, cwd: "/opaque/repository" }}
          tagDisabledReason={null}
          onAction={vi.fn()}
        />,
      ),
    );
  }

  function page(generation: number, commits: ReadonlyArray<GitManagerCommitEntry>) {
    return {
      generation,
      pinnedTips: commits.length === 0 ? [] : [commits[0]!.sha],
      commits,
      nextOffset: null,
      exhausted: true,
      degradedToAllPaging: false,
    };
  }

  /** Every first-page read so far; each must use a cache key never used before. */
  function firstPageReads(): ReadonlyArray<string> {
    const keys = h.firstPageInputs.map((target) => target.input.refreshCacheKey);
    expect(new Set(keys).size).toBe(keys.length);
    return keys;
  }

  function renderedShas(): ReadonlyArray<string | undefined> {
    return [...container.querySelectorAll<HTMLButtonElement>("button[data-commit-sha]")].map(
      (row) => row.dataset.commitSha,
    );
  }

  it("moves HEAD decorations on retained rows after an external commit", async () => {
    const oldTip = { ...commit(10), decorations: ["HEAD -> main"] };
    const oldRows = [oldTip, { ...commit(9), decorations: ["tag: removed"] }];
    h.commitPage = {
      generation: 1,
      pinnedTips: [oldTip.sha],
      commits: oldRows,
      nextOffset: null,
      exhausted: true,
      degradedToAllPaging: false,
    };
    await renderHistory(1);
    await act(async () =>
      container
        .querySelector<HTMLButtonElement>(`button[data-commit-sha="${oldTip.sha}"]`)
        ?.click(),
    );
    const newTip = { ...commit(11), decorations: ["HEAD -> main"] };
    h.commitPage = {
      generation: 2,
      pinnedTips: [newTip.sha],
      commits: [newTip],
      nextOffset: null,
      exhausted: true,
      degradedToAllPaging: false,
    };
    h.retainedPages = [
      {
        generation: 2,
        pinnedTips: [oldTip.sha],
        commits: oldRows.map((entry) => ({ ...entry, decorations: [] })),
        nextOffset: null,
        exhausted: true,
        degradedToAllPaging: false,
      },
    ];
    await renderHistory(2, 2);
    const rows = [...container.querySelectorAll<HTMLButtonElement>("button[data-commit-sha]")];
    expect(rows.map((row) => row.dataset.commitSha)).toEqual([
      newTip.sha,
      ...oldRows.map((entry) => entry.sha),
    ]);
    expect(rows.filter((row) => row.textContent?.includes("HEAD -> main"))).toHaveLength(1);
    expect(rows[0]?.textContent).toContain("HEAD -> main");
    expect(rows[1]?.textContent).not.toContain("HEAD ->");
    expect(rows[1]?.getAttribute("aria-selected")).toBe("true");
    expect(rows[2]?.textContent).not.toContain("tag: removed");
    expect(h.retainedInputs).toHaveLength(1);
    expect(firstPageReads()).toHaveLength(2);
    // The refs snapshot catching up to the signal read's generation is the
    // same change: no second first-page or retained read.
    await renderHistory(2, 2);
    expect(firstPageReads()).toHaveLength(2);
    expect(h.retainedInputs).toHaveLength(1);
  });

  it("refreshes after an in-app operation advances the repository generation without a signal change", async () => {
    // A degraded watcher reports no signal change for History's own
    // operations; the refs refresh after the operation still advances the
    // repository generation.
    const tip = { ...commit(40), decorations: ["HEAD -> main"] };
    h.commitPage = page(2, [tip, commit(39)]);
    await renderHistory(2, 7);
    expect(firstPageReads()).toHaveLength(1);

    await renderHistory(3, 7);
    expect(firstPageReads()).toHaveLength(2);
    // The new read returns the revert.
    const revert = { ...commit(41), decorations: ["HEAD -> main"] };
    h.commitPage = page(3, [revert, { ...tip, decorations: [] }, commit(39)]);
    await renderHistory(3, 7);
    expect(firstPageReads()).toHaveLength(2);
    expect(renderedShas()).toEqual([revert.sha, tip.sha, commit(39).sha]);
    expect(container.textContent).toContain("3 commits loaded");

    await renderHistory(3, 7);
    expect(firstPageReads()).toHaveLength(2);
  });

  it("waits for an in-flight first-page read instead of starting another", async () => {
    h.commitPage = page(4, [commit(50)]);
    await renderHistory(4, 1);
    h.firstPageWaiting = true;
    await renderHistory(4, 2);
    expect(firstPageReads()).toHaveLength(2);
    // The signal read is still in flight when the refs snapshot advances.
    await renderHistory(5, 2);
    expect(firstPageReads()).toHaveLength(2);
    // It returns that generation, so the advance needs no read of its own.
    h.commitPage = page(5, [commit(51), commit(50)]);
    h.firstPageWaiting = false;
    await renderHistory(5, 2);
    expect(firstPageReads()).toHaveLength(2);
    expect(renderedShas()).toEqual([commit(51).sha, commit(50).sha]);
  });

  it("reads again once an in-flight read returns behind the repository generation", async () => {
    h.commitPage = page(4, [commit(60)]);
    await renderHistory(4, 1);
    h.firstPageWaiting = true;
    await renderHistory(4, 2);
    await renderHistory(5, 2);
    expect(firstPageReads()).toHaveLength(2);
    // The in-flight read observed the repository before the operation landed.
    h.firstPageWaiting = false;
    await renderHistory(5, 2);
    expect(firstPageReads()).toHaveLength(3);
    // One read per repository generation, even if it answers behind again.
    await renderHistory(5, 2);
    expect(firstPageReads()).toHaveLength(3);
  });

  it("treats the watcher echo of a repository-triggered read as the same change", async () => {
    h.commitPage = page(1, [commit(70)]);
    await renderHistory(1, 10);
    expect(firstPageReads()).toHaveLength(1);
    // The refs refresh after an in-app operation arrives before the watcher's
    // signal for that same operation.
    await renderHistory(2, 10);
    expect(firstPageReads()).toHaveLength(2);
    h.commitPage = page(2, [commit(71), commit(70)]);
    await renderHistory(2, 10);
    await renderHistory(2, 11);
    expect(firstPageReads()).toHaveLength(2);
    // A later signal is a new change and reads immediately.
    await renderHistory(2, 12);
    expect(firstPageReads()).toHaveLength(3);
  });

  it("refreshes retained server decorations when a signal changes without a page generation change", async () => {
    const old = {
      ...commit(15),
      decorations: ["HEAD -> main", "tag: v1", "origin/main", "origin/HEAD", "feature"],
    };
    h.commitPage = {
      generation: 1,
      pinnedTips: [old.sha],
      commits: [old],
      nextOffset: null,
      exhausted: true,
      degradedToAllPaging: false,
    };
    await renderHistory(1, 10);
    h.commitPage = {
      ...h.commitPage,
      commits: [
        {
          ...old,
          decorations: ["HEAD -> main", "tag: v2", "origin/main", "origin/HEAD", "renamed"],
        },
      ],
    };
    await renderHistory(1, 11);
    const data = h.listProps?.data as ReadonlyArray<GitManagerCommitEntry>;
    expect(data[0]?.decorations).toEqual([
      "HEAD -> main",
      "tag: v2",
      "origin/main",
      "origin/HEAD",
      "renamed",
    ]);
    expect(h.retainedInputs).toEqual([]); // No duplicate pinned offset-zero read.
    expect(firstPageReads()).toHaveLength(2);
    expect(h.refreshCommits).not.toHaveBeenCalled();
  });

  it("makes one first-page read when History mounts before refs and the signal load", async () => {
    const tip = commit(20);
    h.commitPage = page(3, [tip, commit(19)]);

    await renderHistory(null, null, true);
    expect(firstPageReads()).toHaveLength(0);
    expect(container.textContent).toContain("Loading commit history…");
    await renderHistory(3, null, true);
    expect(firstPageReads()).toHaveLength(0);
    await renderHistory(3, 9);
    expect(firstPageReads()).toHaveLength(1);
    expect(container.textContent).toContain("Commit 20");
    await renderHistory(3, 9);
    expect(firstPageReads()).toHaveLength(1);
  });

  it("does not read again for the server's first signature after generation 0", async () => {
    // A repository's first subscriber sees generation 0 until the server
    // computes its baseline signature; that bump is not a repository change.
    h.commitPage = page(3, [commit(21), commit(20)]);
    await renderHistory(null, null, true);
    await renderHistory(3, 0);
    expect(firstPageReads()).toHaveLength(1);
    await renderHistory(3, 1);
    expect(firstPageReads()).toHaveLength(1);
    // A real change hidden in that bump still arrives through the refs the
    // panel re-reads on every signal change.
    await renderHistory(4, 1);
    expect(firstPageReads()).toHaveLength(2);
    await renderHistory(4, 2);
    await renderHistory(4, 3);
    expect(firstPageReads()).toHaveLength(3);
  });

  it("reads without waiting when the environment has no signal", async () => {
    const tip = commit(20);
    h.commitPage = page(1, [tip, commit(19), commit(18), commit(17)]);

    await renderHistory(null);
    expect(firstPageReads()).toHaveLength(1);
    await renderHistory(2);
    expect(firstPageReads()).toHaveLength(2);
    expect(container.textContent).toContain("Commit 20");
  });

  it("refreshes once per newer generation and splices the new tip without losing rows", async () => {
    const commits = [commit(23), commit(22), commit(21)];
    h.commitPage = {
      generation: 4,
      pinnedTips: [commits[0]!.sha],
      commits,
      nextOffset: null,
      exhausted: true,
      degradedToAllPaging: false,
    };

    await renderHistory(4);
    expect(h.refreshCommits).not.toHaveBeenCalled();
    expect(firstPageReads()).toHaveLength(1);

    await renderHistory(5);
    expect(firstPageReads()).toHaveLength(2);
    await renderHistory(5);
    expect(firstPageReads()).toHaveLength(2);

    const newTip = commit(24);
    h.commitPage = {
      generation: 5,
      pinnedTips: [newTip.sha],
      commits: [newTip, ...commits],
      nextOffset: null,
      exhausted: true,
      degradedToAllPaging: false,
    };
    await renderHistory(5);

    expect(firstPageReads()).toHaveLength(2);
    expect(renderedShas()).toEqual([newTip.sha, ...commits.map((entry) => entry.sha)]);

    await renderHistory(5);
    expect(firstPageReads()).toHaveLength(2);
  });

  it("does not read when the repository generation goes backwards without a signal", async () => {
    const commits = [commit(26), commit(25)];
    h.commitPage = page(5, commits);
    await renderHistory(5);
    expect(firstPageReads()).toHaveLength(1);

    await renderHistory(3);
    expect(firstPageReads()).toHaveLength(1);
    await renderHistory(5);
    expect(firstPageReads()).toHaveLength(1);
    expect(renderedShas()).toEqual(commits.map((entry) => entry.sha));
  });

  it("shows the committed tip after remount without an explicit refresh click", async () => {
    // History was loaded at generation 1, the user committed from Changes while
    // History was unmounted, and the panel's refs snapshot now reports 2.
    const preCommit = [{ ...commit(31), decorations: ["HEAD -> main"] }, commit(30)];
    h.commitPage = {
      generation: 1,
      pinnedTips: [preCommit[0]!.sha],
      commits: preCommit,
      nextOffset: null,
      exhausted: true,
      degradedToAllPaging: false,
    };
    // The first-page atom has zero idle TTL and reads the current server page.
    {
      const committed = { ...commit(32), decorations: ["HEAD -> main"] };
      h.commitPage = {
        generation: 2,
        pinnedTips: [committed.sha],
        commits: [committed, ...preCommit.map((entry) => ({ ...entry, decorations: [] }))],
        nextOffset: null,
        exhausted: true,
        degradedToAllPaging: false,
      };
    }

    await renderHistory(2);
    expect(firstPageReads()).toHaveLength(1);
    await renderHistory(2);

    expect(renderedShas()).toEqual([commit(32).sha, ...preCommit.map((entry) => entry.sha)]);
    expect(container.textContent).toContain("3 commits loaded");
    const decoratedRows = [
      ...container.querySelectorAll<HTMLButtonElement>("button[data-commit-sha]"),
    ];
    expect(decoratedRows.filter((row) => row.textContent?.includes("HEAD -> main"))).toHaveLength(
      1,
    );
    expect(decoratedRows[0]?.textContent).toContain("HEAD -> main");
    expect(firstPageReads()).toHaveLength(1);
  });

  it("ignores a stale first page that resolves behind the loaded generation", async () => {
    const newTip = commit(44);
    const older = [commit(43), commit(42)];
    h.commitPage = {
      generation: 6,
      pinnedTips: [newTip.sha],
      commits: [newTip, ...older],
      nextOffset: null,
      exhausted: true,
      degradedToAllPaging: false,
    };
    await renderHistory(6);
    expect(container.textContent).toContain("3 commits loaded");

    // An older in-flight read completing late must not restore pre-commit history
    // or trigger another refresh cycle.
    h.commitPage = {
      generation: 5,
      pinnedTips: [older[0]!.sha],
      commits: older,
      nextOffset: null,
      exhausted: true,
      degradedToAllPaging: false,
    };
    await renderHistory(6);

    expect(renderedShas()).toEqual([newTip.sha, ...older.map((entry) => entry.sha)]);
    expect(container.textContent).toContain("3 commits loaded");
    expect(h.refreshCommits).not.toHaveBeenCalled();
    expect(firstPageReads()).toHaveLength(1);
  });

  /** Loads a page, then a signal read whose first page misses the old rows, capturing a retained batch. */
  async function loadWithRetainedBatch() {
    const oldTip = { ...commit(90), decorations: ["HEAD -> main"] };
    const older = commit(89);
    h.commitPage = page(1, [oldTip, older]);
    await renderHistory(1, 1);
    const newTip = { ...commit(91), decorations: ["HEAD -> main"] };
    h.commitPage = page(2, [newTip]);
    h.retainedPages = [{ ...page(2, [{ ...oldTip, decorations: [] }, older]) }];
    await renderHistory(2, 2);
    expect(h.retainedInputs).toHaveLength(1);
  }

  function retryButton(): HTMLButtonElement | undefined {
    return [...container.querySelectorAll<HTMLButtonElement>("button")].find((button) =>
      button.textContent?.startsWith("Retry"),
    );
  }

  it("explains a failed background refresh, shows the retry in progress, and clears on success", async () => {
    h.commitPage = page(1, [commit(80)]);
    await renderHistory(1, 1);
    h.firstPageError = "The environment request failed.";
    await renderHistory(1, 1);
    expect(container.textContent).toContain(
      "Couldn’t refresh history: The environment request failed. Your loaded commits are still available.",
    );
    expect(retryButton()?.textContent).toBe("Retry");
    expect(retryButton()?.getAttribute("aria-disabled")).toBeNull();
    await act(async () => retryButton()?.click());
    expect(h.refreshCommits).toHaveBeenCalledOnce();

    h.firstPageWaiting = true;
    await renderHistory(1, 1);
    expect(retryButton()?.textContent).toBe("Retrying…");
    expect(retryButton()?.getAttribute("aria-disabled")).toBe("true");
    expect(container.textContent).toContain("Commit 80");

    h.firstPageWaiting = false;
    h.firstPageError = null;
    await renderHistory(1, 1);
    expect(container.textContent).not.toContain("Couldn’t refresh history");
    expect(retryButton()).toBeUndefined();
    expect(firstPageReads()).toHaveLength(1);
  });

  it("retries a failed retained batch with its cause and progress", async () => {
    await loadWithRetainedBatch();
    h.retainedError = "Git returned malformed commit history.";
    await renderHistory(2, 2);
    expect(container.textContent).toContain(
      "Couldn’t refresh history: Git returned malformed commit history. Your loaded commits are still available.",
    );
    await act(async () => retryButton()?.click());
    expect(h.retryRetained).toHaveBeenCalledOnce();
    expect(h.refreshCommits).not.toHaveBeenCalled();
    h.retainedWaiting = true;
    await renderHistory(2, 2);
    expect(retryButton()?.textContent).toBe("Retrying…");
    h.retainedWaiting = false;
    h.retainedError = null;
    await renderHistory(2, 2);
    expect(retryButton()).toBeUndefined();
  });

  it("ends a cause without final punctuation before the banner's next sentence", async () => {
    h.commitPage = page(1, [commit(80)]);
    await renderHistory(1, 1);
    h.firstPageError = "connection reset by peer";
    await renderHistory(1, 1);
    expect(container.textContent).toContain(
      "Couldn’t refresh history: connection reset by peer. Your loaded commits are still available.",
    );
  });

  it("retries a failed first page ahead of a failed retained batch", async () => {
    await loadWithRetainedBatch();
    h.retainedError = "Git returned malformed commit history.";
    h.firstPageError = "The environment request failed.";
    await renderHistory(2, 2);
    expect(container.textContent).toContain(
      "Couldn’t refresh history: The environment request failed.",
    );
    await act(async () => retryButton()?.click());
    expect(h.refreshCommits).toHaveBeenCalledOnce();
    expect(h.retryRetained).not.toHaveBeenCalled();
  });

  it("names the cause when history cannot load at all", async () => {
    h.firstPageError = "Git returned malformed commit history.";
    await renderHistory(1);
    expect(container.textContent).toContain(
      "Couldn’t load history: Git returned malformed commit history.",
    );
    const retry = [...container.querySelectorAll<HTMLButtonElement>("button")].find(
      (button) => button.textContent === "Retry",
    );
    expect(retry).toBeDefined();
    await act(async () => retry?.click());
    expect(h.refreshCommits).toHaveBeenCalledOnce();
  });

  it("requests history only for the scoped repository", async () => {
    const getCommits = vi.mocked(
      (await import("../../../state/gitManager")).gitManagerEnvironment.getHistoryFirstPage,
    );
    getCommits.mockClear();
    h.commitPage = {
      generation: 1,
      pinnedTips: [],
      commits: [],
      nextOffset: null,
      exhausted: true,
      degradedToAllPaging: false,
    };

    await act(async () =>
      root?.render(
        <GitManagerHistoryView
          branchSyncDisabledReason={null}
          blockedReasons={[]}
          repositoryGeneration={1}
          signalGeneration={null}
          signalPending={false}
          projectRef={{ environmentId: "environment-1", projectId: "project-a" } as never}
          rewriteDisabledReason={null}
          scope={{ environmentId: "environment-1" as never, cwd: "/repositories/a" }}
          tagDisabledReason={null}
          onAction={vi.fn()}
        />,
      ),
    );

    const requestedTargets = getCommits.mock.calls.map(
      (call) => call[0] as { environmentId: string; input: { cwd: string } },
    );
    expect(requestedTargets.length).toBeGreaterThan(0);
    for (const target of requestedTargets) {
      expect(target.environmentId).toBe("environment-1");
      expect(target.input.cwd).toBe("/repositories/a");
    }
  });
});

describe("GitManagerHistoryView paging", () => {
  const pagingProjectRef = {
    environmentId: "environment-1",
    projectId: "project-paging",
  } as unknown as ScopedProjectRef;

  afterEach(() => {
    useGitManagerStore.getState().setLoadedPageCursors(pagingProjectRef, []);
    useGitManagerStore.getState().setLoadedPageCount(pagingProjectRef, 0);
  });

  function historyPage(
    commits: ReadonlyArray<GitManagerCommitEntry>,
    nextOffset: number | null,
  ): GitManagerCommitPage {
    return {
      generation: 1,
      pinnedTips: [commit(1).sha],
      commits,
      nextOffset,
      exhausted: nextOffset === null,
      degradedToAllPaging: false,
    };
  }

  function renderPagedHistory() {
    return act(async () =>
      root?.render(
        <GitManagerHistoryView
          branchSyncDisabledReason={null}
          blockedReasons={[]}
          repositoryGeneration={null}
          signalGeneration={null}
          signalPending={false}
          projectRef={pagingProjectRef}
          rewriteDisabledReason={null}
          scope={{ environmentId: "environment-1" as never, cwd: "/opaque/repository" }}
          tagDisabledReason={null}
          onAction={vi.fn()}
        />,
      ),
    );
  }

  function reachListEnd() {
    const onViewableItemsChanged = h.listProps?.onViewableItemsChanged as
      | ((info: { end: number }) => void)
      | undefined;
    const rows = (h.listProps?.data as ReadonlyArray<GitManagerCommitEntry> | undefined) ?? [];
    return act(async () => onViewableItemsChanged?.({ end: rows.length - 1 }));
  }

  function loadedCursors() {
    return useGitManagerStore.getState().selectViewState(pagingProjectRef).loadedPageCursors;
  }

  it("attempts a persisted next page that keeps failing once and asks again only when the user does", async () => {
    vi.spyOn(Date, "now").mockReturnValue(1_000);
    h.commitPage = historyPage([commit(1), commit(2), commit(3)], 3);
    // Each request's failure stays cached. A later request succeeds, so a
    // re-request loop shows up as extra requests instead of hanging the test,
    // and a render loop that never settles fails fast.
    let reads = 0;
    h.nextPageResult = ({ attempt }) => {
      reads += 1;
      if (reads > 200) throw new Error("History kept re-rendering the failed page.");
      return attempt <= 5
        ? { data: null, error: "The next page could not load." }
        : { data: historyPage([commit(4)], null), error: null };
    };
    useGitManagerStore.getState().setLoadedPageCursors(pagingProjectRef, [0, 3]);

    await renderPagedHistory();

    expect(h.nextPageRequests).toEqual([3]);
    expect(container.textContent).toContain("The next page could not load.");
    expect(container.textContent).not.toContain("Loading more commits…");

    await renderPagedHistory();
    expect(h.nextPageRequests).toEqual([3]);
    expect(h.retryNextPage).not.toHaveBeenCalled();

    await reachListEnd();
    expect(h.retryNextPage).toHaveBeenCalledOnce();
    expect(h.nextPageRequests).toEqual([3]);
  });

  it("clears a failed page's error when the user refreshes history", async () => {
    h.commitPage = historyPage([commit(1), commit(2), commit(3)], 3);
    h.nextPageResult = () => ({ data: null, error: "The next page could not load." });
    useGitManagerStore.getState().setLoadedPageCursors(pagingProjectRef, [0, 3]);

    await renderPagedHistory();
    expect(container.textContent).toContain("The next page could not load.");

    await act(async () =>
      container.querySelector<HTMLButtonElement>('[aria-label="Refresh history"]')?.click(),
    );

    expect(h.refreshCommits).toHaveBeenCalledOnce();
    expect(container.textContent).not.toContain("The next page could not load.");
    expect(container.textContent).toContain("3 commits loaded");
    expect(h.nextPageRequests).toEqual([3]);
    expect(loadedCursors()).toEqual([0]);
  });

  it("loads persisted pages one after another and keeps their cursors", async () => {
    h.commitPage = historyPage([commit(1), commit(2), commit(3)], 3);
    const second = historyPage([commit(4), commit(5), commit(6)], 6);
    const third = historyPage([commit(7)], null);
    h.nextPageResult = ({ offset }) => ({
      data: offset === 3 ? second : offset === 6 ? third : null,
      error: null,
    });
    useGitManagerStore.getState().setLoadedPageCursors(pagingProjectRef, [0, 3, 6]);

    await renderPagedHistory();

    expect(h.nextPageRequests).toEqual([3, 6]);
    expect(container.textContent).toContain("7 commits loaded");
    expect(container.textContent).not.toContain("Loading more commits…");
    expect(loadedCursors()).toEqual([0, 3, 6]);
  });

  it("loads the next page when the user reaches the end and records its cursor", async () => {
    vi.spyOn(Date, "now").mockReturnValue(1_000);
    h.commitPage = historyPage([commit(1), commit(2), commit(3)], 3);
    let resolved = false;
    h.nextPageResult = () => ({
      data: resolved ? historyPage([commit(4)], null) : null,
      error: null,
    });

    await renderPagedHistory();
    expect(h.nextPageRequests).toEqual([]);

    await reachListEnd();
    expect(h.nextPageRequests).toEqual([3]);
    expect(container.textContent).toContain("Loading more commits…");

    resolved = true;
    await renderPagedHistory();
    expect(container.textContent).toContain("4 commits loaded");
    expect(container.textContent).not.toContain("Loading more commits…");
    expect(loadedCursors()).toEqual([0, 3]);
  });

  it("re-reads history from the first page when a next page's pinned tips no longer resolve", async () => {
    h.commitPage = historyPage([commit(1), commit(2), commit(3)], 3);
    h.nextPageResult = () => ({
      data: null,
      error: "The pinned history tips no longer resolve.",
      failure: new GitManagerOperationError({
        operation: "history",
        code: "history-tips-unresolvable",
        message: "The pinned history tips no longer resolve.",
        blocked: null,
      }),
    });
    useGitManagerStore.getState().setLoadedPageCursors(pagingProjectRef, [0, 3]);

    await renderPagedHistory();

    expect(h.nextPageRequests).toEqual([3]);
    expect(h.revalidateFirstPage).toHaveBeenCalledOnce();
    expect(container.textContent).toContain("3 commits loaded");
    expect(container.textContent).not.toContain("no longer resolve");
    expect(loadedCursors()).toEqual([0]);
  });
});
