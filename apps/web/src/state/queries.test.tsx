// @vitest-environment happy-dom

import type {
  CheckpointDiffTarget,
  ComposerPathSearchTarget,
} from "@coddy/client-runtime/state/threads";
import * as Option from "effect/Option";
import type { Dispatch, SetStateAction } from "react";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

type EffectCallback = () => void | (() => void);

const hooks = vi.hoisted(() => {
  let cursor = 0;
  let stateSlots = new Map<number, unknown>();

  return {
    effects: [] as EffectCallback[],
    beginRender(): void {
      cursor = 0;
      this.effects = [];
    },
    reset(): void {
      cursor = 0;
      stateSlots = new Map();
      this.effects = [];
    },
    useCallback<T>(callback: T): T {
      cursor += 1;
      return callback;
    },
    useEffect(effect: EffectCallback): void {
      cursor += 1;
      hooks.effects.push(effect);
    },
    useMemo<T>(factory: () => T): T {
      cursor += 1;
      return factory();
    },
    useState<T>(initialValue: T | (() => T)): [T, Dispatch<SetStateAction<T>>] {
      const index = cursor;
      cursor += 1;
      if (!stateSlots.has(index)) {
        stateSlots.set(
          index,
          typeof initialValue === "function" ? (initialValue as () => T)() : initialValue,
        );
      }
      const setValue: Dispatch<SetStateAction<T>> = (nextValue) => {
        const previous = stateSlots.get(index) as T;
        stateSlots.set(
          index,
          typeof nextValue === "function" ? (nextValue as (value: T) => T)(previous) : nextValue,
        );
      };
      return [stateSlots.get(index) as T, setValue];
    },
    useRef<T>(initialValue: T): { current: T } {
      const index = cursor;
      cursor += 1;
      if (!stateSlots.has(index)) {
        stateSlots.set(index, { current: initialValue });
      }
      return stateSlots.get(index) as { current: T };
    },
  };
});

interface QueryDescriptor {
  readonly kind: "fullThreadDiff" | "listEntries" | "searchEntries" | "turnDiff";
  readonly args: {
    readonly environmentId: string;
    readonly input: Readonly<Record<string, unknown>>;
  };
  readonly key: string;
}

interface QueryView {
  readonly data: unknown;
  readonly error: string | null;
  readonly isPending: boolean;
  readonly refresh: () => void;
}

const testState = vi.hoisted(() => ({
  queryDescriptors: [] as Array<QueryDescriptor | null>,
  queryViews: new Map<QueryDescriptor["kind"], QueryView>(),
  threadState: null as unknown as {
    data: Option.Option<unknown>;
    error: Option.Option<string>;
    status: string;
  },
}));

function descriptor(kind: QueryDescriptor["kind"], args: QueryDescriptor["args"]): QueryDescriptor {
  return {
    kind,
    args,
    key: `${kind}:${JSON.stringify(args)}`,
  };
}

vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  return {
    ...actual,
    useCallback: hooks.useCallback,
    useEffect: hooks.useEffect,
    useMemo: hooks.useMemo,
    useRef: hooks.useRef,
    useState: hooks.useState,
  };
});

vi.mock("./threads", () => ({
  useEnvironmentThread: () => testState.threadState,
}));

vi.mock("./query", async (importOriginal) => ({
  // The real message rule; only the hook is replaced.
  ...(await importOriginal<typeof import("./query")>()),
  useEnvironmentQuery: (query: QueryDescriptor | null) => {
    testState.queryDescriptors.push(query);
    if (query === null) {
      return {
        data: null,
        error: null,
        isPending: false,
        refresh: vi.fn(),
      };
    }
    return (
      testState.queryViews.get(query.kind) ?? {
        data: null,
        error: null,
        isPending: false,
        refresh: vi.fn(),
      }
    );
  },
}));

vi.mock("./projects", () => ({
  projectEnvironment: {
    listEntries: (args: QueryDescriptor["args"]) => descriptor("listEntries", args),
    searchEntries: (args: QueryDescriptor["args"]) => descriptor("searchEntries", args),
  },
}));

vi.mock("./orchestration", () => ({
  orchestrationEnvironment: {
    fullThreadDiff: (args: QueryDescriptor["args"]) => descriptor("fullThreadDiff", args),
    turnDiff: (args: QueryDescriptor["args"]) => descriptor("turnDiff", args),
  },
}));

import { useCheckpointDiff, useComposerPathSearch, useThreadDetail } from "./queries";

let captured: unknown;

function HookHarness({ run }: { readonly run: () => unknown }) {
  captured = run();
  return null;
}

function renderHook<A>(run: () => A): A {
  captured = undefined;
  hooks.beginRender();
  renderToStaticMarkup(createElement(HookHarness, { run }));
  return captured as A;
}

function runEffects(): Array<() => void> {
  const cleanups: Array<() => void> = [];
  for (const effect of hooks.effects) {
    const cleanup = effect();
    if (typeof cleanup === "function") {
      cleanups.push(cleanup);
    }
  }
  return cleanups;
}

function searchTarget(overrides: Partial<ComposerPathSearchTarget> = {}): ComposerPathSearchTarget {
  return {
    environmentId: "environment-1" as ComposerPathSearchTarget["environmentId"],
    cwd: "/repo",
    query: "src",
    ...overrides,
  };
}

function checkpointTarget(overrides: Partial<CheckpointDiffTarget> = {}): CheckpointDiffTarget {
  return {
    environmentId: "environment-1" as CheckpointDiffTarget["environmentId"],
    threadId: "thread-1" as CheckpointDiffTarget["threadId"],
    fromTurnCount: 0,
    toTurnCount: 3,
    ignoreWhitespace: true,
    ...overrides,
  };
}

beforeEach(() => {
  hooks.reset();
  testState.queryDescriptors = [];
  testState.queryViews.clear();
  testState.threadState = {
    data: Option.none(),
    error: Option.none(),
    status: "empty",
  };
});

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("useThreadDetail", () => {
  it.each([
    ["live", false, false],
    ["synchronizing", true, false],
    ["deleted", false, true],
  ] as const)("maps the %s thread status", (status, isPending, isDeleted) => {
    const thread = { id: "thread-1" };
    testState.threadState = {
      data: Option.some(thread),
      error: Option.some("sync warning"),
      status,
    };

    expect(renderHook(() => useThreadDetail(null, null))).toEqual({
      data: thread,
      error: "sync warning",
      isPending,
      isDeleted,
    });
  });

  it("maps absent thread data and errors to null", () => {
    expect(renderHook(() => useThreadDetail(null, null))).toEqual({
      data: null,
      error: null,
      isPending: false,
      isDeleted: false,
    });
  });
});

describe("useComposerPathSearch", () => {
  it("lists a bounded set of workspace entries for an empty reference query", () => {
    const entries = Array.from({ length: 100 }, (_, index) => ({
      path: `file-${String(index).padStart(3, "0")}.ts`,
      kind: "file" as const,
    }));
    testState.queryViews.set("listEntries", {
      data: { entries, truncated: false },
      error: null,
      isPending: false,
      refresh: vi.fn(),
    });

    const result = renderHook(() => useComposerPathSearch(searchTarget({ query: "   " })));

    expect(testState.queryDescriptors).toEqual([
      {
        kind: "listEntries",
        key: expect.any(String),
        args: {
          environmentId: "environment-1",
          input: { cwd: "/repo", limit: 80 },
        },
      },
    ]);
    expect(result.entries).toEqual(entries.slice(0, 80));
  });

  it("debounces a trimmed search and exposes the query result", () => {
    vi.useFakeTimers();
    const refresh = vi.fn();
    testState.queryViews.set("searchEntries", {
      data: { entries: [{ path: "src/main.ts", kind: "file" }] },
      error: null,
      isPending: false,
      refresh,
    });

    const initial = renderHook(() => useComposerPathSearch(searchTarget({ query: "   " })));
    const cleanups = runEffects();
    const changed = renderHook(() => useComposerPathSearch(searchTarget({ query: "  src  " })));
    runEffects();

    expect(initial.entries).toEqual([]);
    expect(changed.isPending).toBe(true);
    expect(testState.queryDescriptors.at(-1)).toMatchObject({
      kind: "listEntries",
      args: { input: { cwd: "/repo" } },
    });

    vi.advanceTimersByTime(120);
    const settled = renderHook(() => useComposerPathSearch(searchTarget({ query: "  src  " })));

    expect(testState.queryDescriptors.at(-1)).toMatchObject({
      kind: "searchEntries",
      args: {
        environmentId: "environment-1",
        input: { cwd: "/repo", query: "src", limit: 80 },
      },
    });
    expect(settled).toEqual({
      entries: [{ path: "src/main.ts", kind: "file" }],
      error: null,
      isPending: false,
      refresh,
    });
    for (const cleanup of cleanups) cleanup();
  });

  it("cancels a superseded debounce timer", () => {
    vi.useFakeTimers();
    const clearTimeout = vi.spyOn(window, "clearTimeout");
    renderHook(() => useComposerPathSearch(searchTarget({ query: "first" })));
    const [cleanup] = runEffects();
    cleanup!();

    renderHook(() => useComposerPathSearch(searchTarget({ query: "second" })));
    runEffects();
    vi.advanceTimersByTime(119);
    const pending = renderHook(() => useComposerPathSearch(searchTarget({ query: "second" })));

    expect(pending.isPending).toBe(true);
    expect(clearTimeout).toHaveBeenCalledTimes(1);
    expect(testState.queryDescriptors.at(-1)).toMatchObject({
      kind: "searchEntries",
      args: { input: { query: "first" } },
    });

    vi.advanceTimersByTime(1);
    renderHook(() => useComposerPathSearch(searchTarget({ query: "second" })));
    expect(testState.queryDescriptors.at(-1)).toMatchObject({
      kind: "searchEntries",
      args: { input: { query: "second" } },
    });
  });

  it("retains the loaded entries for the same target while a refreshed search is pending", () => {
    vi.useFakeTimers();
    const readme = { path: "README.md", kind: "file" as const };
    testState.queryViews.set("searchEntries", {
      data: { entries: [readme] },
      error: null,
      isPending: false,
      refresh: vi.fn(),
    });
    const loaded = renderHook(() => useComposerPathSearch(searchTarget({ query: "READ" })));
    runEffects();
    expect(loaded.entries).toEqual([readme]);

    renderHook(() => useComposerPathSearch(searchTarget({ query: "README" })));
    runEffects();
    testState.queryViews.set("searchEntries", {
      data: null,
      error: null,
      isPending: true,
      refresh: vi.fn(),
    });
    vi.advanceTimersByTime(120);
    const refreshing = renderHook(() => useComposerPathSearch(searchTarget({ query: "README" })));

    expect(testState.queryDescriptors.at(-1)).toMatchObject({
      kind: "searchEntries",
      args: { input: { query: "README" } },
    });
    expect(refreshing.isPending).toBe(true);
    expect(refreshing.entries).toEqual([readme]);

    const narrowed = { path: "docs/README.md", kind: "file" as const };
    testState.queryViews.set("searchEntries", {
      data: { entries: [narrowed] },
      error: null,
      isPending: false,
      refresh: vi.fn(),
    });
    const settled = renderHook(() => useComposerPathSearch(searchTarget({ query: "README" })));
    expect(settled.isPending).toBe(false);
    expect(settled.entries).toEqual([narrowed]);
  });

  it("drops retained entries when the search target changes or searching stops", () => {
    vi.useFakeTimers();
    const readme = { path: "README.md", kind: "file" as const };
    testState.queryViews.set("searchEntries", {
      data: { entries: [readme] },
      error: null,
      isPending: false,
      refresh: vi.fn(),
    });
    renderHook(() => useComposerPathSearch(searchTarget({ query: "READ" })));
    runEffects();

    testState.queryViews.set("searchEntries", {
      data: null,
      error: null,
      isPending: true,
      refresh: vi.fn(),
    });
    renderHook(() => useComposerPathSearch(searchTarget({ cwd: "/other", query: "READ" })));
    runEffects();
    vi.advanceTimersByTime(120);
    const retargeted = renderHook(() =>
      useComposerPathSearch(searchTarget({ cwd: "/other", query: "READ" })),
    );
    expect(retargeted.isPending).toBe(true);
    expect(retargeted.entries).toEqual([]);

    renderHook(() => useComposerPathSearch(searchTarget({ cwd: null, query: "READ" })));
    runEffects();
    vi.advanceTimersByTime(120);
    const disabled = renderHook(() =>
      useComposerPathSearch(searchTarget({ cwd: null, query: "READ" })),
    );
    expect(disabled.isPending).toBe(false);
    expect(disabled.entries).toEqual([]);
  });

  it("disables searching when the environment or cwd is absent", () => {
    renderHook(() => useComposerPathSearch(searchTarget({ environmentId: null })));
    expect(testState.queryDescriptors).toEqual([null]);

    testState.queryDescriptors = [];
    hooks.reset();
    renderHook(() => useComposerPathSearch(searchTarget({ cwd: null })));
    expect(testState.queryDescriptors).toEqual([null]);
  });
});

describe("useCheckpointDiff", () => {
  it("selects the full-thread request for a zero turn count", () => {
    renderHook(() => useCheckpointDiff(checkpointTarget()));

    expect(testState.queryDescriptors).toEqual([
      {
        kind: "fullThreadDiff",
        key: expect.any(String),
        args: {
          environmentId: "environment-1",
          input: {
            threadId: "thread-1",
            toTurnCount: 3,
            ignoreWhitespace: true,
          },
        },
      },
      null,
    ]);
  });

  it("selects the bounded turn request for a nonzero turn count", () => {
    renderHook(() => useCheckpointDiff(checkpointTarget({ fromTurnCount: 1, toTurnCount: 4 })));

    expect(testState.queryDescriptors).toEqual([
      null,
      {
        kind: "turnDiff",
        key: expect.any(String),
        args: {
          environmentId: "environment-1",
          input: {
            threadId: "thread-1",
            fromTurnCount: 1,
            toTurnCount: 4,
            ignoreWhitespace: true,
          },
        },
      },
    ]);
  });

  it.each([
    [{ environmentId: null }, undefined],
    [{ threadId: null }, undefined],
    [{ fromTurnCount: null }, undefined],
    [{ toTurnCount: null }, undefined],
    [{}, { enabled: false }],
  ] as const)("disables incomplete or explicitly disabled requests", (overrides, options) => {
    renderHook(() => useCheckpointDiff(checkpointTarget(overrides), options));

    expect(testState.queryDescriptors).toEqual([null, null]);
  });
});
