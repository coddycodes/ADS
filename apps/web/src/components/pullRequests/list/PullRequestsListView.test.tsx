// @vitest-environment happy-dom
import { environmentRpcKey } from "@coddy/client-runtime/state/runtime";
import {
  EnvironmentId,
  PullRequestsOperationError,
  type PullRequestsChanged,
  type PullRequestsListInput,
  type PullRequestsListPage,
  type PullRequestsSnapshot,
} from "@coddy/contracts";
import * as Cause from "effect/Cause";
import { act, createRef, useReducer, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { DEFAULT_PULL_REQUESTS_FILTERS, usePullRequestsStore } from "../../../pullRequestsStore";
import { buildListInput } from "./pullRequestsList.logic";
import { PullRequestsContextRefresh } from "../pullRequestsContextRefresh";
import { context, row } from "../testFixtures";
import { gitlabContext } from "../detail/testFixtures";
const h = vi.hoisted(() => ({
  first: null as PullRequestsListPage | null,
  second: null as PullRequestsListPage | null,
  error: null as PullRequestsOperationError | null,
  pending: false,
  awaitingRetry: false,
  autoCompleteRefresh: true,
  requests: vi.fn((args: { input: PullRequestsListInput }) => ({ kind: "list", ...args })),
  requestTotals: vi.fn(),
  refresh: vi.fn(),
  listProps: null as Record<string, unknown> | null,
  // One emission per failure, as an atom keeps it; a refresh answers with a new one.
  failures: new Map<object, unknown>(),
  snapshot: null as PullRequestsSnapshot | null,
  // What the next snapshot read answers with, as the poller stored it.
  nextSnapshot: null as PullRequestsSnapshot | null,
  changed: null as PullRequestsChanged | null,
  snapshotReads: vi.fn(),
  subscribe: vi.fn((args: { input: unknown }) => ({ kind: "subscribe", ...args })),
}));
vi.mock("../../../state/pullRequests", () => ({
  pullRequestsEnvironment: {
    list: h.requests,
    requestListTotalsRefresh: h.requestTotals,
    getVocabulary: vi.fn(() => ({ kind: "vocabulary" })),
    get: vi.fn((args: unknown) => ({ kind: "get", args })),
    getTimeline: vi.fn((args: unknown) => ({ kind: "getTimeline", args })),
    readSnapshot: vi.fn((args: { input: unknown }) => ({ kind: "readSnapshot", ...args })),
    subscribe: h.subscribe,
  },
}));
vi.mock("../../../state/query", () => ({
  useEnvironmentQuery: (atom: { kind: string; input: PullRequestsListInput } | null) => {
    const [, publish] = useReducer((value: number) => value + 1, 0);
    if (atom !== null && atom.kind !== "list") {
      const value = atom.kind === "readSnapshot" ? h.snapshot : h.changed;
      const read = () => {
        h.snapshotReads(atom.kind);
        if (atom.kind === "readSnapshot" && h.nextSnapshot) h.snapshot = h.nextSnapshot;
        publish();
      };
      return {
        data: value,
        emission: value ? { _tag: "Success", value } : { _tag: "Initial" },
        error: null,
        isPending: false,
        refresh: read,
        revalidate: read,
        requiresRetry: false,
      };
    }
    const data = atom?.kind === "list" ? (atom.input.cursor === null ? h.first : h.second) : null;
    const error = atom?.kind === "list" ? h.error : null;
    if (error && !h.failures.has(error))
      h.failures.set(error, { _tag: "Failure", cause: Cause.fail(error) });
    const refresh = () => {
      h.refresh(atom?.input.cursor);
      if (h.autoCompleteRefresh && atom?.kind === "list") {
        if (atom.input.cursor === null && h.first)
          h.first = { ...h.first, rows: [...h.first.rows] };
        if (atom.input.cursor !== null && h.second)
          h.second = { ...h.second, rows: [...h.second.rows] };
        if (h.error) h.failures.delete(h.error);
        publish();
      }
    };
    return {
      data,
      emission: error ? h.failures.get(error) : { _tag: "Success", value: data },
      error: error?.message ?? null,
      isPending: h.pending,
      refresh,
      revalidate: refresh,
      requiresRetry: h.awaitingRetry,
    };
  },
}));
vi.mock("@legendapp/list/react", () => ({
  LegendList: (props: {
    data: readonly unknown[];
    keyExtractor: (item: unknown) => string;
    renderItem: (p: { item: unknown; index: number }) => ReactNode;
  }) => {
    h.listProps = props;
    return (
      <div role="list">
        {props.data.map((item, index) => (
          <div key={props.keyExtractor(item)}>{props.renderItem({ item, index })}</div>
        ))}
      </div>
    );
  },
}));
vi.mock("@tanstack/react-router", () => ({
  Link: ({ params, children }: { params: { number: string }; children: ReactNode }) => (
    <a href={`/pull-requests/${params.number}`}>{children}</a>
  ),
}));
import { PullRequestsListView, type PullRequestsListViewHandle } from "./PullRequestsListView";
function setVisibility(state: DocumentVisibilityState) {
  Object.defineProperty(document, "visibilityState", { configurable: true, get: () => state });
  document.dispatchEvent(new Event("visibilitychange"));
}
async function renderGitlab() {
  await act(async () =>
    root.render(
      <PullRequestsListView
        scope={{ environmentId: "env" as never, cwd: "/repo" }}
        projectRef={projectRef}
        context={gitlabContext}
      />,
    ),
  );
}
function storedList(title: string, observedAt: number): PullRequestsSnapshot {
  return {
    list: {
      payload: { rows: [{ ...row, title }], nextCursor: null, totalCount: 1, counts: null },
      observedAt,
    },
    detail: null,
    tab: null,
  };
}
const LIST_EVENT: PullRequestsChanged = {
  list: true,
  detail: false,
  timeline: false,
  commits: false,
  checks: false,
  files: false,
};
let container: HTMLDivElement;
let root: Root;
const projectRef = { environmentId: "env", projectId: "p" } as never;
const handle = createRef<PullRequestsListViewHandle>();
async function render(combined = true) {
  await act(async () =>
    root.render(
      <PullRequestsListView
        ref={handle}
        scope={{ environmentId: "env" as never, cwd: "/repo" }}
        projectRef={projectRef}
        context={{
          ...context,
          capabilities: { ...context.capabilities, closedTabIncludesMerged: combined },
        }}
      />,
    ),
  );
}
function button(text: string) {
  const b = [...container.querySelectorAll("button")].find((b) => b.textContent?.trim() === text);
  if (!b) throw new Error(`Missing ${text}`);
  return b;
}
beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  usePullRequestsStore.setState({ byProjectKey: {} });
  h.first = {
    rows: [row],
    nextCursor: "next",
    totalCount: 2,
    counts: { open: 2, closed: 0, merged: 0 },
  };
  h.second = {
    rows: [{ ...row, number: 15, title: "Second page" }],
    nextCursor: null,
    totalCount: 2,
    counts: null,
  };
  h.error = null;
  h.failures.clear();
  h.pending = false;
  h.awaitingRetry = false;
  h.autoCompleteRefresh = true;
  h.requests.mockClear();
  h.requestTotals.mockClear();
  h.refresh.mockClear();
  h.listProps = null;
  h.snapshot = null;
  h.nextSnapshot = null;
  h.changed = null;
  h.snapshotReads.mockClear();
  h.subscribe.mockClear();
  setVisibility("visible");
});
afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
});
describe("PullRequestsListView", () => {
  it("names the state tabs and empty list with GitLab vocabulary", async () => {
    h.first = { rows: [], nextCursor: null, totalCount: 0, counts: null };
    await act(async () =>
      root.render(
        <PullRequestsListView
          scope={{ environmentId: "env" as never, cwd: "/repo" }}
          projectRef={projectRef}
          context={gitlabContext}
        />,
      ),
    );
    expect(container.querySelector('[role="tablist"]')?.getAttribute("aria-label")).toBe(
      "merge request state",
    );
    expect(container.textContent).toContain("No open merge requests");
  });
  it("renders atom rows using fixed-height virtualization and count tabs", async () => {
    await render();
    expect(container.querySelector('[role="tabpanel"]')?.textContent).toContain(
      "Fix connection recovery",
    );
    expect(container.textContent).toContain("Open 2");
    expect(h.listProps?.estimatedItemSize).toBe(56);
    expect((h.listProps!.getFixedItemSize as () => number)()).toBe(56);
    expect(h.requests).toHaveBeenCalledWith(
      expect.objectContaining({
        input: expect.objectContaining({ cwd: "/repo", state: "open", cursor: null }),
      }),
    );
  });
  it("rebuilds the input when switching tabs, with GitLab's four tabs", async () => {
    await render(false);
    await act(async () => button("Merged 0").click());
    expect(h.requests).toHaveBeenLastCalledWith(
      expect.objectContaining({
        input: expect.objectContaining({ state: "merged", cursor: null }),
      }),
    );
    expect(button("All 2")).toBeDefined();
  });
  it("appends Load more, keeps earlier rows, and hides it for a null next cursor", async () => {
    await render();
    await act(async () => button("Load more").click());
    expect(container.textContent).toContain("Fix connection recovery");
    expect(container.textContent).toContain("Second page");
    expect(container.textContent).not.toContain("Load more");
    expect(h.requests).toHaveBeenLastCalledWith(
      expect.objectContaining({ input: expect.objectContaining({ cursor: "next" }) }),
    );
  });
  it("Refresh resets cursors and refreshes the first page", async () => {
    await render();
    await act(async () => button("Load more").click());
    h.refresh.mockClear();
    await act(async () => handle.current!.refresh());
    expect(h.refresh).toHaveBeenCalledWith(null);
    // An explicit Refresh also re-reads the repository-wide tab totals, for this page only.
    expect(h.requestTotals).toHaveBeenCalledWith({
      environmentId: "env",
      input: expect.objectContaining({ cwd: "/repo", state: "open", cursor: null }),
    });
    expect(h.requestTotals.mock.invocationCallOrder[0]).toBeLessThan(
      h.refresh.mock.invocationCallOrder[0]!,
    );
    expect(container.textContent).not.toContain("Second page");
    expect(container.textContent).toContain("Load more");
  });
  it("shows a first page read alongside the context without reading it again", async () => {
    const firstPage = buildListInput(
      { filters: DEFAULT_PULL_REQUESTS_FILTERS, listTab: "open", sort: "newest" },
      "/repo",
    );
    const onFreshPageConsumed = vi.fn();
    const view = (
      <PullRequestsListView
        scope={{ environmentId: "env" as never, cwd: "/repo" }}
        projectRef={projectRef}
        context={context}
        freshPageKey={environmentRpcKey({
          environmentId: EnvironmentId.make("env"),
          input: firstPage,
        })}
        onFreshPageConsumed={onFreshPageConsumed}
      />
    );
    await act(async () => root.render(view));
    expect(container.textContent).toContain("Fix connection recovery");
    // The list takes the page once, at its first open; re-renders keep the page as it is.
    await act(async () => root.render(view));
    expect(onFreshPageConsumed).toHaveBeenCalledExactlyOnceWith(
      environmentRpcKey({ environmentId: EnvironmentId.make("env"), input: firstPage }),
    );
    expect(h.refresh).not.toHaveBeenCalled();
    expect(h.requestTotals).not.toHaveBeenCalled();
    // Leaving the page and coming back is a later open: it refreshes as usual.
    await act(async () => usePullRequestsStore.getState().setListTab(projectRef, "closed"));
    await act(async () => usePullRequestsStore.getState().setListTab(projectRef, "open"));
    expect(h.refresh).toHaveBeenCalledWith(null);

    // A cached page from an earlier open is still refreshed before it is shown.
    h.refresh.mockClear();
    await act(async () => root.unmount());
    root = createRoot(container);
    await render();
    expect(h.refresh).toHaveBeenCalledWith(null);
  });
  it("re-reads a failure cached by an earlier open and hides it until the read answers", async () => {
    h.first = null;
    h.error = new PullRequestsOperationError({
      operation: "list",
      code: "not_authenticated",
      message: "Sign in again.",
      hostDetail: null,
      retryable: false,
    });
    h.autoCompleteRefresh = false;
    const rescan = vi.fn();
    await act(async () =>
      root.render(
        <PullRequestsContextRefresh value={rescan}>
          <PullRequestsListView
            ref={handle}
            scope={{ environmentId: "env" as never, cwd: "/repo" }}
            projectRef={projectRef}
            context={context}
          />
        </PullRequestsContextRefresh>,
      ),
    );
    expect(h.refresh).toHaveBeenCalledWith(null);
    expect(container.querySelector('[role="alert"]')).toBeNull();
    expect(container.textContent).not.toContain("Sign in again.");
    expect(rescan).not.toHaveBeenCalled();

    // The new read answers with a failure of its own, which is shown and acted on.
    h.autoCompleteRefresh = true;
    await act(async () => handle.current!.refresh());
    expect(container.textContent).toContain("Sign in again.");
    expect(rescan).toHaveBeenCalledOnce();
  });
  it("renders a GitLab unavailable filter error with detail and recovery, never as empty", async () => {
    usePullRequestsStore.getState().setFilters(projectRef, { reviewStatus: "approved" });
    h.first = null;
    h.error = new PullRequestsOperationError({
      operation: "list",
      code: "unavailable",
      message: "Approval data is unavailable. Clear the review-status filter.",
      hostDetail: "No approval state in list data.",
      retryable: false,
    });
    await render(false);
    expect(container.textContent).toContain(h.error.message);
    expect(container.textContent).toContain(h.error.hostDetail);
    expect(container.textContent).not.toContain("Nothing matches");
    expect(button("Retry")).toBeDefined();
    await act(async () =>
      [...container.querySelectorAll("button")]
        .findLast((b) => b.textContent === "Clear filters")!
        .click(),
    );
    expect(
      usePullRequestsStore.getState().selectViewState(projectRef).filters.reviewStatus,
    ).toBeNull();
  });
  it("shows filter-empty recovery and clears all filters", async () => {
    h.first = { rows: [], nextCursor: null, totalCount: null, counts: null };
    usePullRequestsStore
      .getState()
      .setFilters(projectRef, { search: "missing", author: "@me", labels: ["bug"] });
    await render();
    expect(container.textContent).toContain("Nothing matches these filters");
    const emptyState = [...container.querySelectorAll("p")].find(
      (p) => p.textContent === "Nothing matches these filters",
    )!.parentElement!;
    expect(emptyState.querySelector("button")?.textContent).toBe("Clear filters");
    await act(async () =>
      [...container.querySelectorAll("button")]
        .findLast((b) => b.textContent === "Clear filters")!
        .click(),
    );
    expect(container.textContent).toContain("No open pull requests");
    const unfilteredEmptyState = [...container.querySelectorAll("p")].find(
      (p) => p.textContent === "No open pull requests",
    )!.parentElement!;
    expect(unfilteredEmptyState.textContent).toBe("No open pull requests");
    expect(unfilteredEmptyState.querySelector("button")).toBeNull();
    expect(h.requests).toHaveBeenLastCalledWith(
      expect.objectContaining({
        input: expect.objectContaining({ search: null, author: null, labels: [] }),
      }),
    );
  });
  it("does not offer Load more or a zero comment badge on a search page", async () => {
    h.first = {
      rows: [{ ...row, commentCount: 0 }],
      nextCursor: null,
      totalCount: null,
      counts: null,
    };
    await render();
    expect(container.textContent).toContain(row.title);
    expect(container.textContent).not.toContain("Load more");
    expect(container.querySelector('[aria-label="0 comments"]')).toBeNull();
  });
});

it("restores scroll after leaving the list without persisting every scroll event", async () => {
  await render();
  const scroll = h.listProps!.onScroll as (event: unknown) => void;
  await act(async () => scroll({ nativeEvent: { contentOffset: { y: 112 } } }));
  expect(usePullRequestsStore.getState().selectViewState(projectRef).scrollTop).toBe(0);
  await act(async () => root.render(null));
  expect(usePullRequestsStore.getState().selectViewState(projectRef).scrollTop).toBe(112);
  await render();
  expect(h.listProps?.initialScrollOffset).toBe(112);
});
it("tab changes reset both old cursors and scroll without stale cleanup overwriting the reset", async () => {
  await render();
  await act(async () => button("Load more").click());
  await act(async () =>
    (h.listProps!.onScroll as (event: unknown) => void)({
      nativeEvent: { contentOffset: { y: 112 } },
    }),
  );
  await act(async () => button("Closed 0").click());
  expect(container.textContent).not.toContain("Second page");
  expect(usePullRequestsStore.getState().selectViewState(projectRef).scrollTop).toBe(0);
  expect(h.requests).toHaveBeenLastCalledWith(
    expect.objectContaining({ input: expect.objectContaining({ state: "closed", cursor: null }) }),
  );
});
it("keeps an explicit filter reset at the top even when the query values were already clear", async () => {
  await render();
  await act(async () =>
    (h.listProps!.onScroll as (event: unknown) => void)({
      nativeEvent: { contentOffset: { y: 112 } },
    }),
  );
  await act(async () => button("Clear filters").click());
  expect(usePullRequestsStore.getState().selectViewState(projectRef).scrollTop).toBe(0);
  await act(async () => root.render(null));
  expect(usePullRequestsStore.getState().selectViewState(projectRef).scrollTop).toBe(0);
});

it("rescans context once for each authentication failure without retrying list reads", async () => {
  h.first = null;
  h.error = new PullRequestsOperationError({
    operation: "list",
    code: "not_authenticated",
    message: "Sign in again.",
    hostDetail: null,
    retryable: false,
  });
  const rescan = vi.fn();
  const element = (
    <PullRequestsContextRefresh value={rescan}>
      <PullRequestsListView
        scope={{ environmentId: "env" as never, cwd: "/repo" }}
        projectRef={projectRef}
        context={context}
      />
    </PullRequestsContextRefresh>
  );
  await act(async () => root.render(element));
  expect(rescan).toHaveBeenCalledOnce();
  await act(async () => root.render(element));
  expect(rescan).toHaveBeenCalledOnce();
  // Opening re-reads the cached failure once; the answer rescans without list retries.
  expect(h.refresh).toHaveBeenCalledOnce();
});
it("persists scroll on pagehide so a reload does not depend on React unmount", async () => {
  await render();
  const scroll = h.listProps!.onScroll as (event: unknown) => void;
  await act(async () => scroll({ nativeEvent: { contentOffset: { y: 112 } } }));
  await act(async () => window.dispatchEvent(new Event("pagehide")));
  expect(usePullRequestsStore.getState().selectViewState(projectRef).scrollTop).toBe(112);
});
it("restores enough cursor pages for the saved scroll position when returning from detail", async () => {
  await render();
  await act(async () => button("Load more").click());
  const scroll = h.listProps!.onScroll as (event: unknown) => void;
  await act(async () => scroll({ nativeEvent: { contentOffset: { y: 56 } } }));
  await act(async () => root.render(null));
  await render();
  expect(container.textContent).toContain("Second page");
  expect(container.textContent).toContain("Fix connection recovery");
  expect(h.requests).toHaveBeenLastCalledWith(
    expect.objectContaining({ input: expect.objectContaining({ cursor: "next" }) }),
  );
});

it("refreshes a cached first page on a fresh context mount and hides its old repository rows", async () => {
  h.autoCompleteRefresh = false;
  await render();
  expect(h.refresh).toHaveBeenCalledWith(null);
  expect(container.textContent).not.toContain("Fix connection recovery");
  h.first = { ...h.first!, rows: [{ ...row, title: "New repository result" }] };
  await render();
  expect(container.textContent).toContain("New repository result");
});

it("hides cached rows when opened during revalidation without starting another refresh", async () => {
  h.pending = true;
  h.autoCompleteRefresh = false;
  await render();
  expect(container.textContent).not.toContain("Fix connection recovery");
  expect(h.refresh).not.toHaveBeenCalled();

  h.first = { ...h.first!, rows: [{ ...row, title: "New repository result" }] };
  h.pending = false;
  await render();
  expect(container.textContent).toContain("New repository result");
  expect(h.refresh).not.toHaveBeenCalled();
});

it("hides a cached failure when opened during revalidation until the existing read answers", async () => {
  h.first = null;
  h.error = new PullRequestsOperationError({
    operation: "list",
    code: "not_authenticated",
    message: "Sign in again.",
    hostDetail: null,
    retryable: false,
  });
  h.pending = true;
  h.autoCompleteRefresh = false;
  await render();
  expect(container.querySelector('[role="alert"]')).toBeNull();
  expect(container.textContent).not.toContain("Sign in again.");
  expect(h.refresh).not.toHaveBeenCalled();

  h.error = null;
  h.first = { rows: [row], nextCursor: null, totalCount: 1, counts: null };
  h.pending = false;
  await render();
  expect(container.textContent).toContain("Fix connection recovery");
  expect(h.refresh).not.toHaveBeenCalled();
});

it("shows an exhausted cached failure on remount without issuing another request", async () => {
  h.first = null;
  h.awaitingRetry = true;
  h.error = new PullRequestsOperationError({
    operation: "list",
    code: "not_authenticated",
    message: "The connection dropped.",
    hostDetail: null,
    retryable: false,
  });
  await render();
  expect(container.textContent).toContain("The connection dropped.");
  await act(async () => root.render(null));
  await render();
  expect(container.textContent).toContain("The connection dropped.");
  expect(h.refresh).not.toHaveBeenCalled();
});

describe("PullRequestsListView GitHub snapshots", () => {
  it("paints the stored list until the live first page answers", async () => {
    h.snapshot = storedList("Stored copy", 1_000);
    h.autoCompleteRefresh = false;
    await render();
    expect(container.textContent).toContain("Stored copy");
    h.first = { ...h.first!, rows: [{ ...row, title: "Live copy" }] };
    await render();
    expect(container.textContent).toContain("Live copy");
    expect(container.textContent).not.toContain("Stored copy");
  });
  it("keeps the live page when the stored copy answers after it", async () => {
    // A server clock ahead of the browser must not let a late stored page win.
    h.first = { ...h.first!, rows: [{ ...row, title: "Live copy" }] };
    await render();
    expect(container.textContent).toContain("Live copy");
    h.snapshot = storedList("Stored copy", Number.MAX_SAFE_INTEGER);
    await render();
    expect(container.textContent).toContain("Live copy");
    expect(container.textContent).not.toContain("Stored copy");
  });
});

describe("PullRequestsListView GitLab snapshots", () => {
  it("paints the stored list until the live first page answers", async () => {
    h.snapshot = storedList("Stored copy", 1_000);
    h.autoCompleteRefresh = false;
    await renderGitlab();
    expect(container.textContent).toContain("Stored copy");
    h.first = { ...h.first!, rows: [{ ...row, title: "Live copy" }] };
    await renderGitlab();
    expect(container.textContent).toContain("Live copy");
    expect(container.textContent).not.toContain("Stored copy");
  });
  it("a list event replaces a list that already shows live data with the newer stored copy", async () => {
    h.snapshot = storedList("Stored copy", 1_000);
    h.first = { ...h.first!, rows: [{ ...row, title: "Live copy" }] };
    await renderGitlab();
    expect(container.textContent).toContain("Live copy");
    h.nextSnapshot = storedList("Poller copy", 2_000);
    h.changed = { ...LIST_EVENT };
    await renderGitlab();
    expect(h.snapshotReads).toHaveBeenCalledWith("readSnapshot");
    expect(container.textContent).toContain("Poller copy");
    expect(container.textContent).not.toContain("Live copy");
  });
  it("keeps the live page when a re-read answers with a copy no newer than it", async () => {
    h.snapshot = storedList("Stored copy", 1_000);
    h.first = { ...h.first!, rows: [{ ...row, title: "Live copy" }] };
    await renderGitlab();
    h.nextSnapshot = storedList("Stored copy", 1_000);
    h.changed = { ...LIST_EVENT };
    await renderGitlab();
    expect(h.snapshotReads).toHaveBeenCalledWith("readSnapshot");
    expect(container.textContent).toContain("Live copy");
    expect(container.textContent).not.toContain("Stored copy");
  });
  it("drops the subscription while the document is hidden and subscribes again when shown", async () => {
    setVisibility("hidden");
    await renderGitlab();
    expect(h.subscribe).not.toHaveBeenCalled();
    await act(async () => setVisibility("visible"));
    expect(h.subscribe).toHaveBeenCalledWith({
      environmentId: "env",
      input: expect.objectContaining({ cwd: "/repo", state: "open", number: null, tab: null }),
    });
  });
});
