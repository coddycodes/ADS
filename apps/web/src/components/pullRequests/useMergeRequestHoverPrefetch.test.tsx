// @vitest-environment happy-dom
import type { EnvironmentId } from "@coddy/contracts";
import { act, createElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { HOVER_PREFETCH_MS, prefetchedWithin, resetPrefetchGates } from "./hoverPrefetch.logic";

const h = vi.hoisted(() => ({
  settled: false,
  get: vi.fn((args: unknown) => ({ kind: "get", args })),
  getTimeline: vi.fn((args: unknown) => ({ kind: "getTimeline", args })),
  handlers: new Map<number, { onPointerEnter(): void; onPointerLeave(): void }>(),
}));

vi.mock("../../state/pullRequests", () => ({
  pullRequestsEnvironment: {
    get: h.get,
    getTimeline: h.getTimeline,
  },
}));

vi.mock("./shared/usePullRequestsQuery", () => ({
  usePullRequestsQuery: (atom: { kind: string; args: unknown } | null) => ({
    data: h.settled && atom ? { kind: atom.kind } : null,
    error: null,
    isPending: !h.settled && atom !== null,
    emission: { _tag: "Initial" },
  }),
}));

import { useMergeRequestHoverPrefetch } from "./useMergeRequestHoverPrefetch";

function Probe({
  enabled,
  environmentId,
  cwd,
  number,
}: {
  enabled: boolean;
  environmentId: string;
  cwd: string;
  number: number;
}) {
  const handlers = useMergeRequestHoverPrefetch({
    enabled,
    environmentId: environmentId as EnvironmentId,
    cwd,
    number,
  });
  h.handlers.set(number, handlers);
  return createElement("div", { "data-number": String(number), ...handlers });
}

function MultiProbe({ numbers }: { numbers: readonly number[] }) {
  return createElement(
    "div",
    null,
    numbers.map((number) =>
      createElement(Probe, {
        key: number,
        enabled: true,
        environmentId: "env",
        cwd: "/repo",
        number,
      }),
    ),
  );
}

describe("useMergeRequestHoverPrefetch", () => {
  let root: Root;
  let node: HTMLDivElement;

  beforeEach(() => {
    vi.useFakeTimers();
    resetPrefetchGates();
    h.settled = false;
    h.handlers.clear();
    h.get.mockClear();
    h.getTimeline.mockClear();
    node = document.createElement("div");
    document.body.appendChild(node);
    root = createRoot(node);
  });

  afterEach(() => {
    act(() => root.unmount());
    node.remove();
    vi.useRealTimers();
  });

  function renderProbe(number: number) {
    act(() => {
      root.render(
        createElement(Probe, { enabled: true, environmentId: "env", cwd: "/repo", number }),
      );
    });
    return number;
  }

  function renderProbes<const T extends readonly number[]>(numbers: T): T {
    act(() => {
      root.render(createElement(MultiProbe, { numbers }));
    });
    return numbers;
  }

  function pointerEnter(number: number) {
    act(() => h.handlers.get(number)?.onPointerEnter());
  }

  function pointerLeave(number: number) {
    act(() => h.handlers.get(number)?.onPointerLeave());
  }

  it("prefetches detail and timeline after 150ms hover", () => {
    const number = renderProbe(14);
    pointerEnter(number);
    act(() => vi.advanceTimersByTime(HOVER_PREFETCH_MS - 1));
    expect(h.get).not.toHaveBeenCalled();
    act(() => vi.advanceTimersByTime(1));
    expect(h.get).toHaveBeenCalledTimes(1);
    expect(h.getTimeline).toHaveBeenCalledTimes(1);
    expect(h.get).toHaveBeenCalledWith({
      environmentId: "env",
      input: { cwd: "/repo", number: 14 },
    });
    expect(h.getTimeline).toHaveBeenCalledWith({
      environmentId: "env",
      input: { cwd: "/repo", number: 14 },
    });
  });

  it("records answered prefetches so an open within 5 s shows them", () => {
    const request = { environmentId: "env" as never, input: { cwd: "/repo", number: 14 } };
    const number = renderProbe(14);
    pointerEnter(number);
    act(() => vi.advanceTimersByTime(HOVER_PREFETCH_MS));
    expect(prefetchedWithin("get", request, Date.now())).toBe(false);
    h.settled = true;
    renderProbe(14);
    expect(prefetchedWithin("get", request, Date.now())).toBe(true);
    expect(prefetchedWithin("getTimeline", request, Date.now())).toBe(true);
  });

  it("does not prefetch when pointer leaves before 150ms", () => {
    const number = renderProbe(14);
    pointerEnter(number);
    act(() => vi.advanceTimersByTime(HOVER_PREFETCH_MS - 1));
    pointerLeave(number);
    act(() => vi.advanceTimersByTime(1));
    expect(h.get).not.toHaveBeenCalled();
    expect(h.getTimeline).not.toHaveBeenCalled();
  });

  it("allows at most two in-flight prefetches per environment", () => {
    const numbers = renderProbes([1, 2, 3]);
    pointerEnter(numbers[0]);
    pointerEnter(numbers[1]);
    pointerEnter(numbers[2]);
    act(() => vi.advanceTimersByTime(HOVER_PREFETCH_MS));
    expect(h.get).toHaveBeenCalledTimes(2);
    expect(h.getTimeline).toHaveBeenCalledTimes(2);
  });

  it("releases the gate after queries settle so another row can prefetch", () => {
    const numbers = renderProbes([1, 2, 3]);
    pointerEnter(numbers[0]);
    pointerEnter(numbers[1]);
    pointerEnter(numbers[2]);
    act(() => vi.advanceTimersByTime(HOVER_PREFETCH_MS));
    expect(h.get).toHaveBeenCalledTimes(2);

    h.settled = true;
    act(() => {
      root.render(createElement(MultiProbe, { numbers: [1, 2, 3] }));
    });
    pointerEnter(numbers[2]);
    act(() => vi.advanceTimersByTime(HOVER_PREFETCH_MS));
    expect(h.get).toHaveBeenCalledTimes(3);
    expect(h.get).toHaveBeenLastCalledWith({
      environmentId: "env",
      input: { cwd: "/repo", number: 3 },
    });
  });
});
