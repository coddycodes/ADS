// @vitest-environment happy-dom

import { afterEach, describe, expect, it, vi } from "vite-plus/test";

import { bucketOf, createClientCollector } from "./clientCollector";
import { rememberProfilerRefused } from "./profilerSupport";

function setVisibility(state: "visible" | "hidden"): void {
  Object.defineProperty(document, "visibilityState", { configurable: true, value: state });
  document.dispatchEvent(new Event("visibilitychange"));
}

/** Whether BiBCode's window has focus; the page stays visible either way (#99). */
function setFocus(focused: boolean): void {
  vi.spyOn(document, "hasFocus").mockReturnValue(focused);
  window.dispatchEvent(new Event(focused ? "focus" : "blur"));
}

/** Runs frame callbacks at once, so a slow record is ready for the next flush. */
const nextFrame = (callback: () => void) => callback();

afterEach(() => {
  Reflect.deleteProperty(document, "visibilityState");
  vi.restoreAllMocks();
  vi.useRealTimers();
});

describe("clientCollector", () => {
  it("matches the Rust bucket function", () => {
    expect(bucketOf(1)).toBe(0);
    expect(bucketOf(1.12 ** 10 + 0.001)).toBe(10);
    expect(bucketOf(1e9)).toBe(99);
    expect(bucketOf(Number.NaN)).toBe(0);
  });

  it("batches only enabled environments and individual records over 250 ms", async () => {
    let t = 0;
    const send = vi.fn(async (_environmentId: string, _batch: unknown) => ({ accepted: true }));
    const c = createClientCollector({
      now: () => t,
      send,
      supportedEntryTypes: [],
      clientId: "c1",
      raf: nextFrame,
    });
    c.setEnabledEnvironments(new Set(["env-a"]));
    const fast = c.observeRequest("env-a", "server.getConfig");
    t = 20;
    fast.complete();
    const slow = c.observeRequest("env-a", "orchestration.dispatch");
    expect(slow.correlationId).toBe("c1:2");
    t = 400;
    slow.complete();
    const ignored = c.observeRequest("env-b", "server.getConfig");
    expect(ignored.correlationId).toBeNull();
    t = 900;
    ignored.complete();
    await c.flush();
    expect(send).toHaveBeenCalledTimes(1);
    const [envId, batch] = send.mock.calls[0]! as [string, any];
    expect(envId).toBe("env-a");
    expect(batch.records.map((r: { method: string }) => r.method)).toEqual([
      "orchestration.dispatch",
    ]);
    expect(batch.records[0]).toMatchObject({ correlationId: "c1:2", rttMs: 380, startedAtMs: 20 });
    expect(batch.histograms.map((h: { method: string }) => h.method).sort()).toEqual([
      "orchestration.dispatch",
      "server.getConfig",
    ]);
    c.dispose();
  });

  it("caps records at 64 per batch", async () => {
    let t = 0;
    const send = vi.fn(async (_environmentId: string, _batch: any) => ({ accepted: true }));
    const c = createClientCollector({
      now: () => t,
      send,
      supportedEntryTypes: [],
      clientId: "c1",
      raf: nextFrame,
    });
    c.setEnabledEnvironments(new Set(["e"]));
    for (let i = 0; i < 100; i += 1) {
      const done = c.observeRequest("e", "m");
      t += 300;
      done.complete();
    }
    await c.flush();
    expect(send.mock.calls[0]![1].records).toHaveLength(64);
    expect(send.mock.calls[0]![1].histograms).toEqual([{ method: "m", buckets: [[50, 100]] }]);
    c.dispose();
  });

  it("drops samples that span a hidden document", async () => {
    let t = 0;
    const send = vi.fn(async (_environmentId: string, _batch: any) => ({ accepted: true }));
    const c = createClientCollector({
      now: () => t,
      send,
      supportedEntryTypes: [],
      clientId: "c1",
    });
    c.setEnabledEnvironments(new Set(["e"]));
    const done = c.observeRequest("e", "m");
    setVisibility("hidden");
    t = 5_000;
    setVisibility("visible");
    done.complete();
    await c.flush();
    expect(send.mock.calls[0]?.[1].records ?? []).toHaveLength(0);
    expect(send.mock.calls[0]?.[1].histograms ?? []).toHaveLength(0);
    c.dispose();
  });

  it("fills render time in on the second animation frame", async () => {
    let t = 0;
    const frames: Array<() => void> = [];
    const send = vi.fn(async (_environmentId: string, _batch: any) => ({ accepted: true }));
    const c = createClientCollector({
      now: () => t,
      send,
      supportedEntryTypes: [],
      clientId: "c1",
      raf: (callback) => {
        frames.push(callback);
      },
    });
    c.setEnabledEnvironments(new Set(["e"]));
    const done = c.observeRequest("e", "m");
    t = 300;
    done.complete();
    // A flush before the frames keeps the record for the next batch.
    await c.flush();
    expect(send.mock.calls[0]![1].records).toEqual([]);
    t = 316;
    frames.shift()!();
    t = 345;
    frames.shift()!();
    await c.flush();
    expect(send.mock.calls[1]![1].records).toEqual([
      { correlationId: "c1:1", method: "m", rttMs: 300, renderMs: 45, startedAtMs: 0 },
    ]);
    c.dispose();
  });

  it("counts heartbeat lateness over 50 ms as a stall", async () => {
    vi.useFakeTimers();
    let t = 0;
    const send = vi.fn(async (_environmentId: string, _batch: any) => ({ accepted: true }));
    const c = createClientCollector({
      now: () => t,
      send,
      supportedEntryTypes: [],
      clientId: "c1",
    });
    c.setEnabledEnvironments(new Set(["e"]));
    t = 700;
    vi.advanceTimersByTime(700);
    await c.flush();
    expect(send.mock.calls[0]![1].stalls).toEqual({ count: 1, totalMs: 200, maxMs: 200 });
    expect(send.mock.calls[0]![1].capabilities).toContain("heartbeat");
    c.dispose();
  });

  it("starts nothing until an environment is enabled and stops when none is", () => {
    vi.useFakeTimers();
    const send = vi.fn(async () => ({ accepted: true }));
    const c = createClientCollector({
      now: () => 0,
      send,
      supportedEntryTypes: [],
      clientId: "c1",
    });
    expect(vi.getTimerCount()).toBe(0);
    c.setEnabledEnvironments(new Set(["e"]));
    expect(vi.getTimerCount()).toBe(2); // flush interval and heartbeat
    setVisibility("hidden");
    expect(vi.getTimerCount()).toBe(1); // the heartbeat pauses while hidden
    setVisibility("visible");
    c.setEnabledEnvironments(new Set());
    expect(vi.getTimerCount()).toBe(0);
    c.dispose();
  });

  it("never observes its own reports", () => {
    const c = createClientCollector({
      now: () => 0,
      send: async () => undefined,
      supportedEntryTypes: [],
      clientId: "c1",
    });
    c.setEnabledEnvironments(new Set(["e"]));
    expect(c.observeRequest("e", "server.reportProfilerClientBatch").correlationId).toBeNull();
    c.dispose();
  });

  it("stops reporting to an environment that refuses authorization, without surfacing it", async () => {
    let t = 0;
    const send = vi.fn(async (environmentId: string, _batch: unknown) => {
      if (environmentId === "denied") {
        throw { _tag: "EnvironmentAuthorizationError", message: "missing scope" };
      }
      if (environmentId === "flaky") throw new Error("socket closed");
      if (environmentId === "rejected") {
        // A refused batch is dropped; the next one is still sent.
        throw { _tag: "ProfilerClientBatchTooLarge", message: "The batch is over 64 KiB." };
      }
      return { accepted: true };
    });
    const c = createClientCollector({
      now: () => t,
      send,
      supportedEntryTypes: [],
      clientId: "c1",
    });
    c.setEnabledEnvironments(new Set(["denied", "flaky", "rejected"]));
    const report = () => {
      for (const environmentId of ["denied", "flaky", "rejected"]) {
        const done = c.observeRequest(environmentId, "m");
        t += 10;
        done.complete();
      }
    };
    report();
    await expect(c.flush()).resolves.toBeUndefined();
    report();
    await c.flush();
    expect(send.mock.calls.map(([environmentId]) => environmentId)).toEqual([
      "denied",
      "flaky",
      "rejected",
      "flaky",
      "rejected",
    ]);
    expect(c.observeRequest("denied", "m").correlationId).toBeNull();
    c.dispose();
  });
  it("stops reporting to an older server that does not know the batch method", async () => {
    let t = 0;
    // An older server's connection-level answer; squashed, the die is its defect string.
    const send = vi.fn(async (_environmentId: string, _batch: unknown) => {
      throw "Unknown request tag: server.reportProfilerClientBatch";
    });
    const c = createClientCollector({
      now: () => t,
      send,
      supportedEntryTypes: [],
      clientId: "c1",
    });
    c.setEnabledEnvironments(new Set(["old"]));
    const done = c.observeRequest("old", "m");
    t += 10;
    done.complete();
    await expect(c.flush()).resolves.toBeUndefined();
    expect(send).toHaveBeenCalledTimes(1);
    expect(c.observeRequest("old", "m").correlationId).toBeNull();
    // Coming back into the enabled set does not revive it.
    c.setEnabledEnvironments(new Set());
    c.setEnabledEnvironments(new Set(["old"]));
    expect(c.observeRequest("old", "m").correlationId).toBeNull();
    c.dispose();
  });
  it("sends nothing to an environment the Diagnostics page saw refuse the profiler", async () => {
    let t = 0;
    const send = vi.fn(async (_environmentId: string, _batch: unknown) => ({ accepted: true }));
    const c = createClientCollector({
      now: () => t,
      send,
      supportedEntryTypes: [],
      clientId: "c1",
    });
    c.setEnabledEnvironments(new Set(["refused-by-status"]));
    rememberProfilerRefused("refused-by-status");
    const done = c.observeRequest("refused-by-status", "m");
    t += 10;
    done.complete();
    await c.flush();
    expect(send).not.toHaveBeenCalled();
    expect(c.observeRequest("refused-by-status", "m").correlationId).toBeNull();
    c.dispose();
  });
  it("drops samples that span a Windows sleep, whichever wakes first", async () => {
    vi.useFakeTimers();
    let t = 0;
    let wall = 1_000_000;
    const send = vi.fn(async (_environmentId: string, _batch: any) => ({ accepted: true }));
    const c = createClientCollector({
      now: () => t,
      wallNow: () => wall,
      send,
      // WebView2: Long Animation Frames replace the heartbeat's stall role, not its suspend role.
      supportedEntryTypes: ["long-animation-frame"],
      clientId: "c1",
      raf: nextFrame,
    });
    c.setEnabledEnvironments(new Set(["e"]));
    const repliesFirst = c.observeRequest("e", "m");
    const heartbeatFirst = c.observeRequest("e", "m");
    // Two minutes asleep. On Windows both clocks count them, and the document stays visible.
    t = 120_000;
    wall += 120_000;
    repliesFirst.complete();
    vi.advanceTimersByTime(500);
    heartbeatFirst.complete();
    const afterWaking = c.observeRequest("e", "m");
    t = 120_300;
    wall += 300;
    afterWaking.complete();
    await c.flush();
    const batch = send.mock.calls[0]![1];
    expect(batch.records.map((r: { rttMs: number }) => r.rttMs)).toEqual([300]);
    expect(batch.histograms).toEqual([{ method: "m", buckets: [[50, 1]] }]);
    expect(batch.stalls.count).toBe(0);
    c.dispose();
  });

  it("drops samples across a sleep that stopped the monotonic clock, whichever wakes first", async () => {
    vi.useFakeTimers();
    let t = 0;
    let wall = 1_000_000;
    const send = vi.fn(async (_environmentId: string, _batch: any) => ({ accepted: true }));
    const c = createClientCollector({
      now: () => t,
      wallNow: () => wall,
      send,
      supportedEntryTypes: [],
      clientId: "c1",
      raf: nextFrame,
    });
    c.setEnabledEnvironments(new Set(["e"]));
    const repliesFirst = c.observeRequest("e", "m");
    const heartbeatFirst = c.observeRequest("e", "m");
    // Two minutes asleep on WebKitGTK or WKWebView: the monotonic clock barely moved.
    t = 800;
    wall += 120_800;
    repliesFirst.complete();
    vi.advanceTimersByTime(500);
    heartbeatFirst.complete();
    const afterWaking = c.observeRequest("e", "m");
    t = 1_100;
    wall += 300;
    afterWaking.complete();
    await c.flush();
    const batch = send.mock.calls[0]![1];
    expect(batch.records.map((r: { rttMs: number }) => r.rttMs)).toEqual([300]);
    expect(batch.histograms).toEqual([{ method: "m", buckets: [[50, 1]] }]);
    // The heartbeat arrived 300 ms late, but across a suspend: not a stall.
    expect(batch.stalls.count).toBe(0);
    c.dispose();
  });

  it("counts a 70 s hang as a stall when the wall clock agrees", async () => {
    vi.useFakeTimers();
    let t = 0;
    let wall = 1_000_000;
    const send = vi.fn(async (_environmentId: string, _batch: any) => ({ accepted: true }));
    const c = createClientCollector({
      now: () => t,
      wallNow: () => wall,
      send,
      supportedEntryTypes: [],
      clientId: "c1",
      raf: nextFrame,
    });
    c.setEnabledEnvironments(new Set(["e"]));
    const waited = c.observeRequest("e", "m");
    t = 70_000;
    wall += 70_000;
    vi.advanceTimersByTime(500);
    waited.complete();
    await c.flush();
    const batch = send.mock.calls[0]![1];
    expect(batch.stalls).toEqual({ count: 1, totalMs: 69_500, maxMs: 69_500 });
    // The user really waited through the hang, so the request still counts.
    expect(batch.records.map((r: { rttMs: number }) => r.rttMs)).toEqual([70_000]);
    c.dispose();
  });

  it("keeps one batch per environment on the wire, dropping windows meanwhile", async () => {
    let t = 0;
    const send = vi.fn((_environmentId: string, _batch: unknown) => new Promise(() => undefined));
    const c = createClientCollector({
      now: () => t,
      send,
      supportedEntryTypes: [],
      clientId: "c1",
    });
    c.setEnabledEnvironments(new Set(["e"]));
    for (let round = 0; round < 3; round += 1) {
      const done = c.observeRequest("e", "m");
      t += 10;
      done.complete();
      void c.flush();
    }
    expect(send).toHaveBeenCalledTimes(1);
    c.dispose();
  });
  it("carries slow records over a batch still on the wire", async () => {
    let t = 0;
    let release!: () => void;
    const onTheWire = new Promise<void>((resolve) => {
      release = resolve;
    });
    const send = vi.fn((_environmentId: string, _batch: any) =>
      send.mock.calls.length === 1 ? onTheWire : Promise.resolve(),
    );
    const c = createClientCollector({
      now: () => t,
      send,
      supportedEntryTypes: [],
      clientId: "c1",
      raf: nextFrame,
    });
    c.setEnabledEnvironments(new Set(["e"]));
    const slow = (rttMs: number) => {
      const done = c.observeRequest("e", "m");
      t += rttMs;
      done.complete();
    };
    slow(300);
    const first = c.flush();
    slow(400);
    // The first batch is still on the wire: this window waits.
    await c.flush();
    expect(send).toHaveBeenCalledTimes(1);
    release();
    await first;
    slow(500);
    await c.flush();
    expect(send).toHaveBeenCalledTimes(2);
    expect(send.mock.calls[1]![1].records.map((r: { rttMs: number }) => r.rttMs)).toEqual([
      400, 500,
    ]);
    c.dispose();
  });
  it("keeps an environment's request in flight while another one reconnects", async () => {
    let t = 0;
    const send = vi.fn(async (_environmentId: string, _batch: any) => ({ accepted: true }));
    const c = createClientCollector({
      now: () => t,
      send,
      supportedEntryTypes: [],
      clientId: "c1",
      raf: nextFrame,
    });
    c.setEnabledEnvironments(new Set(["a", "b"]));
    const inFlight = c.observeRequest("a", "m");
    // B disconnects and comes back, which leaves and re-enters the enabled set.
    c.setEnabledEnvironments(new Set(["a"]));
    c.setEnabledEnvironments(new Set(["a", "b"]));
    t = 300;
    inFlight.complete();
    await c.flush();
    expect(send).toHaveBeenCalledTimes(1);
    expect(send.mock.calls[0]![0]).toBe("a");
    expect(send.mock.calls[0]![1].records.map((r: { rttMs: number }) => r.rttMs)).toEqual([300]);
    c.dispose();
  });

  it("drops a request that spans its environment being switched off and on", async () => {
    let t = 0;
    const send = vi.fn(async (_environmentId: string, _batch: any) => ({ accepted: true }));
    const c = createClientCollector({
      now: () => t,
      send,
      supportedEntryTypes: [],
      clientId: "c1",
      raf: nextFrame,
    });
    c.setEnabledEnvironments(new Set(["a", "b"]));
    const spanning = c.observeRequest("a", "m");
    c.setEnabledEnvironments(new Set(["b"]));
    t = 2_000;
    c.setEnabledEnvironments(new Set(["a", "b"]));
    spanning.complete();
    await c.flush();
    expect(send).not.toHaveBeenCalled();
    c.dispose();
  });

  it("counts no stalls while the window is in the background, where macOS throttles its timers", async () => {
    vi.useFakeTimers();
    let t = 0;
    const send = vi.fn(async (_environmentId: string, _batch: any) => ({ accepted: true }));
    const c = createClientCollector({
      now: () => t,
      send,
      supportedEntryTypes: [],
      clientId: "c1",
      raf: nextFrame,
    });
    c.setEnabledEnvironments(new Set(["e"]));
    // Covered, or on another Space: App Nap aligns the timers to about 1 s (#99).
    setFocus(false);
    const occluded = c.observeRequest("e", "m");
    for (let beat = 0; beat < 10; beat += 1) {
      t += 1_050;
      vi.advanceTimersByTime(1_050);
    }
    occluded.complete();
    await c.flush();
    expect(send).not.toHaveBeenCalled();
    // In front again, a late heartbeat is a stall as before.
    setFocus(true);
    t += 500;
    vi.advanceTimersByTime(500);
    t += 930;
    vi.advanceTimersByTime(500);
    await c.flush();
    expect(send.mock.calls[0]![1].stalls).toEqual({ count: 1, totalMs: 430, maxMs: 430 });
    c.dispose();
  });

  it("ignores the heartbeat while the system catches up after waking", async () => {
    vi.useFakeTimers();
    let t = 0;
    let wall = 1_000_000;
    const send = vi.fn(async (_environmentId: string, _batch: any) => ({ accepted: true }));
    const c = createClientCollector({
      now: () => t,
      wallNow: () => wall,
      send,
      supportedEntryTypes: [],
      clientId: "c1",
      raf: nextFrame,
    });
    c.setEnabledEnvironments(new Set(["e"]));
    // An hour with the display off and the machine asleep: the monotonic clock stopped.
    t = 500;
    wall += 3_600_000;
    vi.advanceTimersByTime(500);
    // The ticks after waking run late while the system catches up (1,220 ms, then 300 ms).
    for (const late of [1_220, 300, 0, 0]) {
      t += 500 + late;
      wall += 500 + late;
      vi.advanceTimersByTime(500);
    }
    await c.flush();
    expect(send).not.toHaveBeenCalled();
    // Once caught up, a real stall counts.
    while (t < 40_000) {
      t += 500;
      wall += 500;
      vi.advanceTimersByTime(500);
    }
    t += 900;
    wall += 900;
    vi.advanceTimersByTime(500);
    await c.flush();
    expect(send.mock.calls[0]![1].stalls).toEqual({ count: 1, totalMs: 400, maxMs: 400 });
    c.dispose();
  });

  it("ignores the catch-up after waking when the window was in the background through the sleep", async () => {
    vi.useFakeTimers();
    let t = 0;
    let wall = 1_000_000;
    const send = vi.fn(async (_environmentId: string, _batch: any) => ({ accepted: true }));
    const c = createClientCollector({
      now: () => t,
      wallNow: () => wall,
      send,
      supportedEntryTypes: [],
      clientId: "c1",
      raf: nextFrame,
    });
    c.setEnabledEnvironments(new Set(["e"]));
    // The screen locked, the machine slept, and the window got focus back on unlocking.
    setFocus(false);
    t = 200;
    wall += 3_600_000;
    setFocus(true);
    t += 500 + 1_220;
    wall += 500 + 1_220;
    vi.advanceTimersByTime(500);
    await c.flush();
    expect(send).not.toHaveBeenCalled();
    c.dispose();
  });
});
