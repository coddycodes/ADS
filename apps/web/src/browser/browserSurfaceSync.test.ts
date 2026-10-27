import type { DesktopPreviewBridge } from "@coddy/contracts";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import { startBrowserSurfaceSync } from "./browserSurfaceSync";
import {
  acquireBrowserSurface,
  acquireBrowserSurfaceOcclusion,
  useBrowserSurfaceStore,
} from "./browserSurfaceStore";

function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

async function flushPromises(): Promise<void> {
  for (let turn = 0; turn < 12; turn += 1) await Promise.resolve();
}

describe("browserSurfaceSync", () => {
  const stops: Array<() => void> = [];

  const startSync = (setBounds: DesktopPreviewBridge["setBounds"]): (() => void) => {
    const stop = startBrowserSurfaceSync({ setBounds });
    stops.push(stop);
    return stop;
  };

  beforeEach(() => {
    useBrowserSurfaceStore.setState({ byTabId: {}, occlusionOwners: new Set() } as never);
  });

  afterEach(() => {
    for (const stop of stops.splice(0)) stop();
    vi.restoreAllMocks();
  });

  it("forwards presented rects to setBounds and dedupes identical updates", async () => {
    const setBounds = vi.fn().mockResolvedValue(undefined);
    const stop = startSync(setBounds);

    const lease = acquireBrowserSurface("sync-t1");
    lease.present({ x: 10, y: 20, width: 300, height: 400 }, true);
    lease.present({ x: 10, y: 20, width: 300, height: 400 }, true); // no-op (store dedupes)
    lease.present({ x: 10, y: 20, width: 300, height: 401 }, true);
    lease.release(); // visible -> false
    await flushPromises();

    expect(setBounds.mock.calls).toEqual([
      ["sync-t1", { x: 10, y: 20, width: 300, height: 400 }, true],
      ["sync-t1", { x: 10, y: 20, width: 300, height: 401 }, true],
      ["sync-t1", { x: 10, y: 20, width: 300, height: 401 }, false],
    ]);
    stop();
  });

  it("pushes the initial store state and stops forwarding after unsubscribe", () => {
    const lease = acquireBrowserSurface("sync-initial");
    lease.present({ x: 1, y: 2, width: 30, height: 40 }, true);
    const setBounds = vi.fn().mockResolvedValue(undefined);

    const stop = startSync(setBounds);

    expect(setBounds).toHaveBeenCalledWith(
      "sync-initial",
      { x: 1, y: 2, width: 30, height: 40 },
      true,
    );

    stop();
    lease.present({ x: 5, y: 6, width: 70, height: 80 }, false);
    expect(setBounds).toHaveBeenCalledTimes(1);
  });

  it("dedupes presentations when unrelated store fields update", () => {
    const setBounds = vi.fn().mockResolvedValue(undefined);
    const stop = startSync(setBounds);
    const lease = acquireBrowserSurface("sync-content");
    lease.present({ x: 10, y: 20, width: 300, height: 400 }, true);

    useBrowserSurfaceStore.getState().presentContent("sync-content", {
      x: 0,
      y: 0,
      width: 300,
      height: 400,
      scale: 1,
      scrollLeft: 0,
      scrollTop: 0,
    });

    expect(setBounds).toHaveBeenCalledTimes(1);
    stop();
  });

  it("serializes same-tab presentations in store order", async () => {
    const first = deferred<void>();
    const second = deferred<void>();
    const setBounds = vi
      .fn()
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise);
    const stop = startSync(setBounds);
    const lease = acquireBrowserSurface("sync-serial");

    lease.present({ x: 1, y: 2, width: 30, height: 40 }, true);
    lease.present({ x: 5, y: 6, width: 70, height: 80 }, false);

    expect(setBounds.mock.calls).toEqual([
      ["sync-serial", { x: 1, y: 2, width: 30, height: 40 }, true],
    ]);

    first.resolve();
    await flushPromises();
    expect(setBounds.mock.calls).toEqual([
      ["sync-serial", { x: 1, y: 2, width: 30, height: 40 }, true],
      ["sync-serial", { x: 5, y: 6, width: 70, height: 80 }, false],
    ]);

    second.resolve();
    await flushPromises();
    stop();
  });

  it("keeps a live surface hidden until every occlusion lease releases", async () => {
    const setBounds = vi.fn().mockResolvedValue(undefined);
    const stop = startSync(setBounds);
    const surface = acquireBrowserSurface("sync-occluded");
    const rect = { x: 10, y: 20, width: 300, height: 400 };
    surface.present(rect, true);

    const firstBlocker = acquireBrowserSurfaceOcclusion();
    const secondBlocker = acquireBrowserSurfaceOcclusion();
    firstBlocker.release();
    firstBlocker.release();
    await flushPromises();

    expect(setBounds.mock.calls).toEqual([
      ["sync-occluded", rect, true],
      ["sync-occluded", rect, false],
    ]);

    secondBlocker.release();
    await flushPromises();

    expect(setBounds.mock.calls).toEqual([
      ["sync-occluded", rect, true],
      ["sync-occluded", rect, false],
      ["sync-occluded", rect, true],
    ]);
    stop();
  });

  it("restores the latest desired bounds after an occlusion", async () => {
    const setBounds = vi.fn().mockResolvedValue(undefined);
    const stop = startSync(setBounds);
    const surface = acquireBrowserSurface("sync-occlusion-bounds");
    const initial = { x: 1, y: 2, width: 30, height: 40 };
    const latest = { x: 5, y: 6, width: 70, height: 80 };
    surface.present(initial, true);
    const blocker = acquireBrowserSurfaceOcclusion();
    surface.present(latest, true);
    blocker.release();
    await flushPromises();

    expect(setBounds.mock.calls).toEqual([
      ["sync-occlusion-bounds", initial, true],
      ["sync-occlusion-bounds", initial, false],
      ["sync-occlusion-bounds", latest, false],
      ["sync-occlusion-bounds", latest, true],
    ]);
    stop();
  });

  it("allows different tabs to progress independently", () => {
    const first = deferred<void>();
    const setBounds = vi.fn().mockReturnValueOnce(first.promise).mockResolvedValueOnce(undefined);
    const stop = startSync(setBounds);

    acquireBrowserSurface("sync-independent-a").present(
      { x: 1, y: 2, width: 30, height: 40 },
      true,
    );
    acquireBrowserSurface("sync-independent-b").present(
      { x: 5, y: 6, width: 70, height: 80 },
      true,
    );

    expect(setBounds.mock.calls).toEqual([
      ["sync-independent-a", { x: 1, y: 2, width: 30, height: 40 }, true],
      ["sync-independent-b", { x: 5, y: 6, width: 70, height: 80 }, true],
    ]);
    first.resolve();
    stop();
  });

  it("makes a failed current presentation eligible for an identical retry", async () => {
    const first = deferred<void>();
    const error = new Error("set bounds failed");
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    const setBounds = vi.fn().mockReturnValueOnce(first.promise).mockResolvedValueOnce(undefined);
    const stop = startSync(setBounds);
    const lease = acquireBrowserSurface("sync-retry");
    lease.present({ x: 1, y: 2, width: 30, height: 40 }, true);

    first.reject(error);
    await flushPromises();
    useBrowserSurfaceStore.getState().presentContent("sync-retry", {
      x: 0,
      y: 0,
      width: 30,
      height: 40,
      scale: 1,
      scrollLeft: 0,
      scrollTop: 0,
    });

    expect(setBounds).toHaveBeenCalledTimes(2);
    expect(consoleError).toHaveBeenCalledWith("Could not sync browser surface bounds.", {
      tabId: "sync-retry",
      error,
    });
    stop();
  });

  it("keeps newer queued state when an older presentation fails", async () => {
    const first = deferred<void>();
    const second = deferred<void>();
    vi.spyOn(console, "error").mockImplementation(() => undefined);
    const setBounds = vi
      .fn()
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise);
    const stop = startSync(setBounds);
    const lease = acquireBrowserSurface("sync-newer");
    lease.present({ x: 1, y: 2, width: 30, height: 40 }, true);
    lease.present({ x: 5, y: 6, width: 70, height: 80 }, true);

    first.reject(new Error("older failed"));
    await flushPromises();
    expect(setBounds).toHaveBeenCalledTimes(2);

    useBrowserSurfaceStore.getState().presentContent("sync-newer", {
      x: 0,
      y: 0,
      width: 70,
      height: 80,
      scale: 1,
      scrollLeft: 0,
      scrollTop: 0,
    });
    expect(setBounds).toHaveBeenCalledTimes(2);

    second.resolve();
    await flushPromises();
    stop();
  });

  it("dedupes applied state without dropping a return behind newer scheduled state", async () => {
    const first = deferred<void>();
    const second = deferred<void>();
    const third = deferred<void>();
    const setBounds = vi
      .fn()
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise)
      .mockReturnValueOnce(third.promise);
    const stop = startSync(setBounds);
    const lease = acquireBrowserSurface("sync-applied");
    const original = { x: 1, y: 2, width: 30, height: 40 };
    const newer = { x: 5, y: 6, width: 70, height: 80 };

    lease.present(original, true);
    first.resolve();
    await flushPromises();
    useBrowserSurfaceStore.getState().presentContent("sync-applied", {
      x: 0,
      y: 0,
      width: 30,
      height: 40,
      scale: 1,
      scrollLeft: 0,
      scrollTop: 0,
    });
    expect(setBounds).toHaveBeenCalledTimes(1);

    lease.present(newer, true);
    lease.present(original, true);
    expect(setBounds).toHaveBeenCalledTimes(2);

    second.resolve();
    await flushPromises();
    expect(setBounds.mock.calls.at(-1)).toEqual(["sync-applied", original, true]);

    third.resolve();
    await flushPromises();
    stop();
  });

  it("stops queued work while safely settling an in-flight rejection", async () => {
    const first = deferred<void>();
    const error = new Error("in-flight failed after stop");
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    const setBounds = vi.fn().mockReturnValueOnce(first.promise);
    const stop = startSync(setBounds);
    const lease = acquireBrowserSurface("sync-stop");
    lease.present({ x: 1, y: 2, width: 30, height: 40 }, true);
    lease.present({ x: 5, y: 6, width: 70, height: 80 }, false);

    stop();
    first.reject(error);
    await flushPromises();

    expect(setBounds.mock.calls).toEqual([
      ["sync-stop", { x: 1, y: 2, width: 30, height: 40 }, true],
    ]);
    expect(consoleError).toHaveBeenCalledWith("Could not sync browser surface bounds.", {
      tabId: "sync-stop",
      error,
    });
  });
});
