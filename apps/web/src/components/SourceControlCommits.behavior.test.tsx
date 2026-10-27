import { scopeThreadRef } from "@coddy/client-runtime/environment";
import { EnvironmentId, ThreadId } from "@coddy/contracts";
import React, { type ReactElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";

const harness = vi.hoisted(() => {
  const refreshQuery = vi.fn();
  return {
    stateValues: [] as unknown[],
    stateIndex: 0,
    setters: [] as Array<ReturnType<typeof vi.fn>>,
    effects: [] as Array<() => void | (() => void)>,
    queryInput: null as unknown,
    query: {
      data: null as null | { commits: Array<Record<string, unknown>>; nextCursor: number | null },
      error: null as unknown,
      isPending: false,
      refresh: refreshQuery,
      revalidate: refreshQuery,
      requiresRetry: false,
    },
    listCommits: vi.fn((input: unknown) => ({ query: input })),
  };
});

vi.mock("react", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react")>()),
  useEffect: (effect: () => void | (() => void)) => harness.effects.push(effect),
  useEffectEvent: <T,>(callback: T) => callback,
  useState: (initial: unknown) => {
    const index = harness.stateIndex++;
    const value = index < harness.stateValues.length ? harness.stateValues[index] : initial;
    const setter = vi.fn();
    harness.setters[index] = setter;
    return [value, setter];
  },
}));
vi.mock("~/state/query", () => ({
  useEnvironmentQuery: (input: unknown) => {
    harness.queryInput = input;
    return harness.query;
  },
}));
vi.mock("~/state/vcs", () => ({
  vcsEnvironment: { listCommits: harness.listCommits },
}));
vi.mock("./SourceControlCommits.logic", () => ({
  formatCommitTimestamp: (value: number, now: number) => `${value}/${now}`,
}));

import { SourceControlCommits } from "./SourceControlCommits";

const threadRef = scopeThreadRef(EnvironmentId.make("environment-1"), ThreadId.make("thread-1"));
const firstCommit = {
  sha: "abcdef123456",
  shortSha: "abcdef1",
  subject: "First commit",
  authorName: "Ada",
  authoredAtMs: 100,
};
const secondCommit = {
  sha: "123456abcdef",
  shortSha: "123456a",
  subject: "Second commit",
  authorName: "Grace",
  authoredAtMs: 200,
};

function visit(node: React.ReactNode, entries: ReactElement[] = []): ReactElement[] {
  if (Array.isArray(node)) {
    for (const child of node) visit(child, entries);
    return entries;
  }
  if (!React.isValidElement(node)) return entries;
  entries.push(node);
  visit((node.props as { children?: React.ReactNode }).children, entries);
  return entries;
}

function renderCommits(overrides: Record<string, unknown> = {}) {
  const tree = SourceControlCommits({
    threadRef,
    gitCwd: "/repo",
    nowMs: 1_000,
    ...overrides,
  });
  return { tree, markup: renderToStaticMarkup(tree) };
}

function clickButton(tree: React.ReactNode, text: string): void {
  const button = visit(tree).find(
    (element) =>
      element.type === "button" &&
      String((element.props as { children?: React.ReactNode }).children).includes(text),
  );
  const onClick = (button?.props as Record<string, unknown> | undefined)?.onClick;
  if (typeof onClick !== "function") throw new Error(`Missing ${text} button`);
  onClick();
}

beforeEach(() => {
  harness.stateValues = [];
  harness.stateIndex = 0;
  harness.setters.length = 0;
  harness.effects.length = 0;
  harness.queryInput = null;
  harness.query.data = null;
  harness.query.error = null;
  harness.query.isPending = false;
  harness.query.refresh.mockReset();
  harness.listCommits.mockClear();
});

describe("SourceControlCommits", () => {
  it("defers loading while collapsed and toggles the section", () => {
    const { tree, markup } = renderCommits();
    expect(markup).toContain("-rotate-90");
    expect(harness.queryInput).toBeNull();
    expect(harness.effects[0]?.()).toBeUndefined();
    clickButton(tree, "Commits");
    const update = harness.setters[0]?.mock.calls[0]?.[0] as
      | ((value: boolean) => boolean)
      | undefined;
    expect(update?.(false)).toBe(true);
    expect(update?.(true)).toBe(false);
  });

  it("builds first and subsequent page queries and refreshes expansion", () => {
    harness.stateValues = [true, null, [], 1];
    renderCommits({ reloadToken: 2 });
    expect(harness.listCommits).toHaveBeenCalledWith({
      environmentId: threadRef.environmentId,
      input: { cwd: "/repo", limit: 30 },
    });
    expect(harness.setters[3]).toHaveBeenCalledWith(2);
    expect(harness.setters[2]).toHaveBeenCalledWith([]);
    expect(harness.setters[1]).toHaveBeenCalledWith(null);
    harness.effects[0]?.();
    expect(harness.query.refresh).toHaveBeenCalledOnce();

    harness.stateIndex = 0;
    harness.stateValues = [true, 30, []];
    harness.effects.length = 0;
    harness.listCommits.mockClear();
    renderCommits();
    expect(harness.listCommits).toHaveBeenCalledWith(
      expect.objectContaining({ input: { cwd: "/repo", limit: 30, cursor: 30 } }),
    );

    harness.stateIndex = 0;
    harness.stateValues = [true, null, []];
    harness.effects.length = 0;
    harness.listCommits.mockClear();
    renderCommits({ gitCwd: null });
    expect(harness.queryInput).toBeNull();
    expect(harness.listCommits).not.toHaveBeenCalled();
  });

  it("renders error, loading, and empty states", () => {
    harness.stateValues = [true, null, []];
    harness.query.error = new Error("failed");
    expect(renderCommits().markup).toContain("load commits");

    harness.stateIndex = 0;
    harness.effects.length = 0;
    harness.query.error = null;
    harness.query.isPending = true;
    expect(renderCommits().markup).toContain("Loading commits");

    harness.stateIndex = 0;
    harness.effects.length = 0;
    harness.query.isPending = false;
    expect(renderCommits().markup).toContain("No commits yet");
  });

  it("combines prior pages, renders rows, and loads another page", () => {
    harness.stateValues = [true, 30, [firstCommit]];
    harness.query.data = { commits: [secondCommit], nextCursor: 60 };
    const { tree, markup } = renderCommits();
    expect(markup).toContain("First commit");
    expect(markup).toContain("Second commit");
    expect(markup).toContain("Ada");
    expect(markup).toContain("100/1000");
    expect(markup).toContain("Load more");
    clickButton(tree, "Load more");
    const append = harness.setters[2]?.mock.calls[0]?.[0] as
      | ((value: readonly unknown[]) => readonly unknown[])
      | undefined;
    expect(append?.([firstCommit])).toEqual([firstCommit, secondCommit]);
    expect(harness.setters[1]).toHaveBeenCalledWith(60);
  });

  it("guards load-more calls when data or cursors disappear", () => {
    harness.stateValues = [true, null, [firstCommit]];
    harness.query.data = { commits: [], nextCursor: 30 };
    const { tree } = renderCommits();
    harness.query.data = null;
    clickButton(tree, "Load more");
    expect(harness.setters[2]).not.toHaveBeenCalled();

    harness.stateIndex = 0;
    harness.effects.length = 0;
    harness.stateValues = [true, null, [firstCommit]];
    harness.query.data = { commits: [], nextCursor: 30 };
    const second = renderCommits();
    harness.query.data.nextCursor = null;
    clickButton(second.tree, "Load more");
    expect(harness.setters[2]).not.toHaveBeenCalled();

    harness.stateIndex = 0;
    harness.effects.length = 0;
    harness.query.data = { commits: [], nextCursor: null };
    expect(renderCommits().markup).not.toContain("Load more");
  });
});
