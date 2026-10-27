import type { EnvironmentId } from "@coddy/contracts";
import { beforeEach, describe, expect, it } from "vite-plus/test";
import {
  createPrefetchGate,
  MAX_HOVER_PREFETCHES,
  prefetchedWithin,
  recordPrefetch,
  resetPrefetchGates,
} from "./hoverPrefetch.logic";

describe("createPrefetchGate", () => {
  it("allows two acquisitions and refuses the third until one releases", () => {
    const gate = createPrefetchGate();
    expect(MAX_HOVER_PREFETCHES).toBe(2);
    expect(gate.tryAcquire()).toBe(true);
    expect(gate.tryAcquire()).toBe(true);
    expect(gate.tryAcquire()).toBe(false);
    gate.release();
    expect(gate.tryAcquire()).toBe(true);
  });
});

describe("prefetchedWithin", () => {
  const request = { environmentId: "env" as EnvironmentId, input: { cwd: "/repo", number: 14 } };
  beforeEach(() => resetPrefetchGates());

  it("reports a prefetch for its read and request for 5 s only", () => {
    recordPrefetch("get", request, 1_000);
    expect(prefetchedWithin("get", request, 6_000)).toBe(true);
    expect(prefetchedWithin("get", request, 6_001)).toBe(false);
    expect(prefetchedWithin("getTimeline", request, 1_000)).toBe(false);
    expect(
      prefetchedWithin("get", { ...request, input: { cwd: "/repo", number: 15 } }, 1_000),
    ).toBe(false);
  });

  it("forgets prefetches older than 5 s when another is recorded", () => {
    recordPrefetch("get", request, 1_000);
    recordPrefetch("getTimeline", request, 7_000);
    expect(prefetchedWithin("get", request, 1_000)).toBe(false);
  });
});
