import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import type { AtomCommandResult } from "@coddy/client-runtime/state/runtime";
import * as Cause from "effect/Cause";
import { AsyncResult } from "effect/reactivity";

import { FileSaveCoordinator, type FileSaveSnapshot } from "./fileSaveCoordinator";

function deferred() {
  let resolve!: (result: AtomCommandResult<void, never>) => void;
  const promise = new Promise<AtomCommandResult<void, never>>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

describe("FileSaveCoordinator", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("debounces edits and persists only the latest contents", async () => {
    vi.useFakeTimers();
    const persist = vi
      .fn<(contents: string) => Promise<AtomCommandResult<void, never>>>()
      .mockResolvedValue(AsyncResult.success(undefined));
    const onPendingChange = vi.fn();
    const onConfirmed = vi.fn();
    const coordinator = new FileSaveCoordinator({
      debounceMs: 500,
      persist,
      onPendingChange,
      onConfirmed,
    });

    coordinator.change("first");
    await vi.advanceTimersByTimeAsync(300);
    coordinator.change("latest");
    await vi.advanceTimersByTimeAsync(499);
    expect(persist).not.toHaveBeenCalled();

    await vi.advanceTimersByTimeAsync(1);
    expect(persist).toHaveBeenCalledOnce();
    expect(persist).toHaveBeenCalledWith("latest");
    expect(onConfirmed).toHaveBeenCalledWith("latest");
    expect(onPendingChange.mock.calls).toEqual([[true], [true], [false]]);
  });

  it("keeps the latest contents after they are saved", async () => {
    const coordinator = new FileSaveCoordinator({
      debounceMs: 500,
      persist: vi.fn().mockResolvedValue(AsyncResult.success(undefined)),
      onPendingChange: vi.fn(),
      onConfirmed: vi.fn(),
    });
    expect(coordinator.getLatestContents()).toBeNull();

    coordinator.change("first");
    coordinator.change("latest");
    expect(coordinator.getLatestContents()).toBe("latest");

    await expect(coordinator.flush()).resolves.toBe("saved");
    expect(coordinator.getLatestContents()).toBe("latest");

    coordinator.change("discarded");
    coordinator.discardPendingSave();
    expect(coordinator.getLatestContents()).toBe("discarded");
  });

  it("keeps autosave-disabled edits pending and explicitly saveable", async () => {
    vi.useFakeTimers();
    const persist = vi
      .fn<(contents: string) => Promise<AtomCommandResult<void, never>>>()
      .mockResolvedValue(AsyncResult.success(undefined));
    const coordinator = new FileSaveCoordinator({
      debounceMs: 500,
      persist,
      onPendingChange: vi.fn(),
      onConfirmed: vi.fn(),
    });

    coordinator.setAutosaveEnabled(false);
    coordinator.change("active draft");
    await vi.advanceTimersByTimeAsync(5_000);

    expect(persist).not.toHaveBeenCalled();
    expect(coordinator.getSnapshot()).toMatchObject({ phase: "pending", canSave: true });
    await expect(coordinator.flush()).resolves.toBe("saved");
    expect(persist).toHaveBeenCalledOnce();
    expect(persist).toHaveBeenCalledWith("active draft");
  });

  it("cancels an existing debounce when autosave is disabled", async () => {
    vi.useFakeTimers();
    const persist = vi.fn().mockResolvedValue(AsyncResult.success(undefined));
    const coordinator = new FileSaveCoordinator({
      debounceMs: 500,
      persist,
      onPendingChange: vi.fn(),
      onConfirmed: vi.fn(),
    });

    coordinator.change("draft");
    coordinator.setAutosaveEnabled(false);
    await vi.advanceTimersByTimeAsync(500);

    expect(persist).not.toHaveBeenCalled();
    expect(coordinator.getSnapshot()).toMatchObject({ phase: "pending", canSave: true });
  });

  it("shares and settles repeated paused flushes when autosave is disabled", async () => {
    const persist = vi.fn().mockResolvedValue(AsyncResult.success(undefined));
    const coordinator = new FileSaveCoordinator({
      debounceMs: 500,
      persist,
      onPendingChange: vi.fn(),
      onConfirmed: vi.fn(),
    });

    coordinator.change("draft");
    coordinator.pauseSaving();
    const firstFlush = coordinator.flush();
    const secondFlush = coordinator.flush();

    expect(secondFlush).toBe(firstFlush);
    coordinator.setAutosaveEnabled(false);
    await expect(firstFlush).resolves.toBe("saving");
    await expect(secondFlush).resolves.toBe("saving");
    expect(persist).not.toHaveBeenCalled();
  });

  it("settles a paused flush when its pending edit is discarded", async () => {
    const persist = vi.fn().mockResolvedValue(AsyncResult.success(undefined));
    const coordinator = new FileSaveCoordinator({
      debounceMs: 500,
      persist,
      onPendingChange: vi.fn(),
      onConfirmed: vi.fn(),
    });

    coordinator.change("draft");
    coordinator.pauseSaving();
    const pausedFlush = coordinator.flush();
    coordinator.discardPendingSave();

    await expect(pausedFlush).resolves.toBe("unchanged");
    expect(persist).not.toHaveBeenCalled();
    expect(coordinator.getSnapshot()).toMatchObject({ phase: "clean", canSave: false });
  });

  it("settles a paused flush when the coordinator is disposed", async () => {
    const persist = vi.fn().mockResolvedValue(AsyncResult.success(undefined));
    const coordinator = new FileSaveCoordinator({
      debounceMs: 500,
      persist,
      onPendingChange: vi.fn(),
      onConfirmed: vi.fn(),
    });

    coordinator.change("draft");
    coordinator.pauseSaving();
    const pausedFlush = coordinator.flush();
    coordinator.dispose();

    await expect(pausedFlush).resolves.toBe("unchanged");
    expect(persist).not.toHaveBeenCalled();
  });

  it("does not autosave a newer edit after the file becomes active during a write", async () => {
    vi.useFakeTimers();
    const firstWrite = deferred();
    const persist = vi
      .fn<(contents: string) => Promise<AtomCommandResult<void, never>>>()
      .mockReturnValueOnce(firstWrite.promise)
      .mockResolvedValueOnce(AsyncResult.success(undefined));
    const coordinator = new FileSaveCoordinator({
      debounceMs: 500,
      persist,
      onPendingChange: vi.fn(),
      onConfirmed: vi.fn(),
    });

    coordinator.setAutosaveEnabled(false);
    coordinator.change("leaving revision");
    coordinator.setAutosaveEnabled(true);
    const leavingSave = coordinator.flush();
    coordinator.setAutosaveEnabled(false);
    coordinator.change("new active revision");
    await Promise.resolve();

    expect(persist).toHaveBeenCalledWith("leaving revision");
    firstWrite.resolve(AsyncResult.success(undefined));
    await expect(leavingSave).resolves.toBe("saved");
    await vi.runAllTimersAsync();

    expect(persist).toHaveBeenCalledOnce();
    expect(coordinator.getSnapshot()).toMatchObject({ phase: "pending", canSave: true });
  });

  it("publishes stable clean, pending, saving, and clean snapshots", async () => {
    vi.useFakeTimers();
    const write = deferred();
    const coordinator = new FileSaveCoordinator({
      debounceMs: 500,
      persist: vi.fn(() => write.promise),
      onPendingChange: vi.fn(),
      onConfirmed: vi.fn(),
    });
    const snapshots: FileSaveSnapshot[] = [];
    const unsubscribe = coordinator.subscribe(() => {
      snapshots.push(coordinator.getSnapshot());
    });

    expect(coordinator.getSnapshot()).toEqual({
      phase: "clean",
      canSave: false,
      confirmedRevision: 0,
    });

    coordinator.change("draft");
    expect(coordinator.getSnapshot()).toEqual({
      phase: "pending",
      canSave: true,
      confirmedRevision: 0,
    });

    await vi.advanceTimersByTimeAsync(500);
    expect(coordinator.getSnapshot()).toEqual({
      phase: "saving",
      canSave: false,
      confirmedRevision: 0,
    });

    write.resolve(AsyncResult.success(undefined));
    await Promise.resolve();
    expect(coordinator.getSnapshot()).toEqual({
      phase: "clean",
      canSave: false,
      confirmedRevision: 1,
    });
    expect(snapshots.map(({ phase }) => phase)).toEqual(["pending", "saving", "clean"]);

    unsubscribe();
    coordinator.change("after unsubscribe");
    expect(snapshots.map(({ phase }) => phase)).toEqual(["pending", "saving", "clean"]);
  });

  it("publishes a retryable failed snapshot without confirming the revision", async () => {
    vi.useFakeTimers();
    const coordinator = new FileSaveCoordinator({
      debounceMs: 500,
      persist: vi.fn().mockResolvedValue(AsyncResult.failure(Cause.fail(new Error("disk full")))),
      onPendingChange: vi.fn(),
      onConfirmed: vi.fn(),
    });

    coordinator.change("draft");
    await vi.advanceTimersByTimeAsync(500);
    await Promise.resolve();

    expect(coordinator.getSnapshot()).toEqual({
      phase: "failed",
      canSave: true,
      confirmedRevision: 0,
    });
  });

  it("does not start another write when a saving listener settles", async () => {
    vi.useFakeTimers();
    const write = deferred();
    const persist = vi
      .fn<(contents: string) => Promise<AtomCommandResult<void, never>>>()
      .mockReturnValue(write.promise);
    const coordinator = new FileSaveCoordinator({
      debounceMs: 500,
      persist,
      onPendingChange: vi.fn(),
      onConfirmed: vi.fn(),
    });
    let settled: Promise<unknown> | undefined;
    coordinator.subscribe(() => {
      if (coordinator.getSnapshot().phase === "saving") settled = coordinator.settle();
    });

    coordinator.change("draft");
    await vi.advanceTimersByTimeAsync(500);

    expect(persist).toHaveBeenCalledOnce();

    write.resolve(AsyncResult.success(undefined));
    await expect(settled).resolves.toBe("saved");
  });

  it("settle waits for an in-flight write and then persists a newer revision", async () => {
    vi.useFakeTimers();
    const firstWrite = deferred();
    const persist = vi
      .fn<(contents: string) => Promise<AtomCommandResult<void, never>>>()
      .mockReturnValueOnce(firstWrite.promise)
      .mockResolvedValueOnce(AsyncResult.success(undefined));
    const coordinator = new FileSaveCoordinator({
      debounceMs: 500,
      persist,
      onPendingChange: vi.fn(),
      onConfirmed: vi.fn(),
    });

    coordinator.change("first");
    await vi.advanceTimersByTimeAsync(500);
    coordinator.change("latest");
    const settled = coordinator.settle();
    expect(persist).toHaveBeenCalledTimes(1);

    firstWrite.resolve(AsyncResult.success(undefined));
    await expect(settled).resolves.toBe("saved");
    expect(persist).toHaveBeenCalledTimes(2);
    expect(persist).toHaveBeenLastCalledWith("latest");
    expect(coordinator.getSnapshot().phase).toBe("clean");
  });

  it("settle reports failure and leaves the latest revision retryable", async () => {
    const coordinator = new FileSaveCoordinator({
      debounceMs: 500,
      persist: vi.fn().mockResolvedValue(AsyncResult.failure(Cause.fail(new Error("read only")))),
      onPendingChange: vi.fn(),
      onConfirmed: vi.fn(),
    });
    coordinator.change("draft");

    await expect(coordinator.settle()).resolves.toBe("failed");
    expect(coordinator.getSnapshot()).toMatchObject({ phase: "failed", canSave: true });
  });

  it("keeps pending state until an edit made during a write is also saved", async () => {
    vi.useFakeTimers();
    const firstWrite = deferred();
    const persist = vi
      .fn<(contents: string) => Promise<AtomCommandResult<void, never>>>()
      .mockReturnValueOnce(firstWrite.promise)
      .mockResolvedValueOnce(AsyncResult.success(undefined));
    const onPendingChange = vi.fn();
    const coordinator = new FileSaveCoordinator({
      debounceMs: 500,
      persist,
      onPendingChange,
      onConfirmed: vi.fn(),
    });

    coordinator.change("first");
    await vi.advanceTimersByTimeAsync(500);
    coordinator.change("latest");
    await vi.advanceTimersByTimeAsync(500);
    expect(persist).toHaveBeenCalledTimes(1);

    firstWrite.resolve(AsyncResult.success(undefined));
    await vi.runAllTimersAsync();
    expect(persist).toHaveBeenCalledTimes(2);
    expect(persist).toHaveBeenLastCalledWith("latest");
    expect(onPendingChange.mock.calls.at(-1)).toEqual([false]);
  });

  it("keeps Save disabled and the saving phase while a newer edit waits behind a write", async () => {
    vi.useFakeTimers();
    const firstWrite = deferred();
    const coordinator = new FileSaveCoordinator({
      debounceMs: 500,
      persist: vi
        .fn<(contents: string) => Promise<AtomCommandResult<void, never>>>()
        .mockReturnValueOnce(firstWrite.promise)
        .mockResolvedValueOnce(AsyncResult.success(undefined)),
      onPendingChange: vi.fn(),
      onConfirmed: vi.fn(),
    });

    coordinator.change("first");
    await vi.advanceTimersByTimeAsync(500);
    coordinator.change("edited while saving");

    expect(coordinator.getSnapshot()).toMatchObject({
      phase: "saving",
      canSave: false,
    });

    firstWrite.resolve(AsyncResult.success(undefined));
    await vi.runAllTimersAsync();
    expect(coordinator.getSnapshot()).toMatchObject({
      phase: "clean",
      canSave: false,
    });
  });

  it("keeps pending edits unflushable while saving is paused for a mutation", () => {
    const coordinator = new FileSaveCoordinator({
      debounceMs: 500,
      persist: vi.fn().mockResolvedValue(AsyncResult.success(undefined)),
      onPendingChange: vi.fn(),
      onConfirmed: vi.fn(),
    });

    coordinator.change("before mutation");
    coordinator.pauseSaving();
    expect(coordinator.getSnapshot()).toMatchObject({
      phase: "pending",
      canSave: false,
    });

    coordinator.change("edited during mutation");
    expect(coordinator.getSnapshot()).toMatchObject({
      phase: "pending",
      canSave: false,
    });

    coordinator.resumeSaving();
    expect(coordinator.getSnapshot()).toMatchObject({
      phase: "pending",
      canSave: true,
    });
  });

  it("does not carry a satisfied paused flush request into a later mutation", async () => {
    vi.useFakeTimers();
    const firstWrite = deferred();
    const persist = vi
      .fn<(contents: string) => Promise<AtomCommandResult<void, never>>>()
      .mockReturnValueOnce(firstWrite.promise)
      .mockResolvedValueOnce(AsyncResult.success(undefined));
    const coordinator = new FileSaveCoordinator({
      debounceMs: 500,
      persist,
      onPendingChange: vi.fn(),
      onConfirmed: vi.fn(),
    });

    coordinator.change("first");
    const firstSave = coordinator.flush();
    await Promise.resolve();
    coordinator.pauseSaving();
    const pausedFlush = coordinator.flush();
    firstWrite.resolve(AsyncResult.success(undefined));
    await expect(firstSave).resolves.toBe("saved");
    await expect(pausedFlush).resolves.toBe("saved");
    coordinator.resumeSaving();

    coordinator.pauseSaving();
    coordinator.change("second");
    coordinator.resumeSaving();
    await Promise.resolve();

    expect(persist).toHaveBeenCalledOnce();
    await vi.advanceTimersByTimeAsync(499);
    expect(persist).toHaveBeenCalledOnce();
    await vi.advanceTimersByTimeAsync(1);
    expect(persist).toHaveBeenCalledTimes(2);
    expect(persist).toHaveBeenLastCalledWith("second");
  });

  it("leaves the file pending when the latest write fails", async () => {
    vi.useFakeTimers();
    const onPendingChange = vi.fn();
    const coordinator = new FileSaveCoordinator({
      debounceMs: 500,
      persist: vi
        .fn()
        .mockResolvedValue(AsyncResult.failure(Cause.fail(new Error("write failed")))),
      onPendingChange,
      onConfirmed: vi.fn(),
    });

    coordinator.change("latest");
    await vi.advanceTimersByTimeAsync(500);
    await Promise.resolve();
    expect(onPendingChange).toHaveBeenCalledWith(true);
    expect(onPendingChange).not.toHaveBeenCalledWith(false);
  });

  it("flush persists the pending edit immediately without waiting for the debounce", async () => {
    vi.useFakeTimers();
    const persist = vi
      .fn<(contents: string) => Promise<AtomCommandResult<void, never>>>()
      .mockResolvedValue(AsyncResult.success(undefined));
    const onPendingChange = vi.fn();
    const onConfirmed = vi.fn();
    const coordinator = new FileSaveCoordinator({
      debounceMs: 500,
      persist,
      onPendingChange,
      onConfirmed,
    });

    coordinator.change("draft");
    expect(coordinator.hasPendingWork()).toBe(true);

    expect(await coordinator.flush()).toBe("saved");
    expect(persist).toHaveBeenCalledOnce();
    expect(persist).toHaveBeenCalledWith("draft");
    expect(onConfirmed).toHaveBeenCalledWith("draft");
    expect(onPendingChange.mock.calls.at(-1)).toEqual([false]);

    // Nothing left pending: a second flush is a no-op and the debounce never fires.
    expect(coordinator.hasPendingWork()).toBe(false);
    expect(await coordinator.flush()).toBe("unchanged");
    await vi.advanceTimersByTimeAsync(500);
    expect(persist).toHaveBeenCalledOnce();
  });

  it("flush is a no-op when there is nothing pending", async () => {
    const persist = vi
      .fn<(contents: string) => Promise<AtomCommandResult<void, never>>>()
      .mockResolvedValue(AsyncResult.success(undefined));
    const coordinator = new FileSaveCoordinator({
      debounceMs: 500,
      persist,
      onPendingChange: vi.fn(),
      onConfirmed: vi.fn(),
    });

    expect(coordinator.hasPendingWork()).toBe(false);
    expect(await coordinator.flush()).toBe("unchanged");
    expect(persist).not.toHaveBeenCalled();
  });

  it("flush reports 'saving' while a write is in flight and does not start a second write", async () => {
    vi.useFakeTimers();
    const inFlight = deferred();
    const persist = vi
      .fn<(contents: string) => Promise<AtomCommandResult<void, never>>>()
      .mockReturnValueOnce(inFlight.promise)
      .mockResolvedValue(AsyncResult.success(undefined));
    const coordinator = new FileSaveCoordinator({
      debounceMs: 500,
      persist,
      onPendingChange: vi.fn(),
      onConfirmed: vi.fn(),
    });

    coordinator.change("draft");
    await vi.advanceTimersByTimeAsync(500);
    expect(persist).toHaveBeenCalledTimes(1);
    expect(coordinator.hasPendingWork()).toBe(true);

    expect(await coordinator.flush()).toBe("saving");
    expect(persist).toHaveBeenCalledTimes(1);

    inFlight.resolve(AsyncResult.success(undefined));
    await vi.runAllTimersAsync();
    expect(persist).toHaveBeenCalledTimes(1);
  });

  it("flush returns 'failed' and leaves the file pending when the write fails", async () => {
    vi.useFakeTimers();
    const onPendingChange = vi.fn();
    const coordinator = new FileSaveCoordinator({
      debounceMs: 500,
      persist: vi
        .fn()
        .mockResolvedValue(AsyncResult.failure(Cause.fail(new Error("write failed")))),
      onPendingChange,
      onConfirmed: vi.fn(),
    });

    coordinator.change("draft");
    expect(await coordinator.flush()).toBe("failed");
    expect(onPendingChange).toHaveBeenCalledWith(true);
    expect(onPendingChange).not.toHaveBeenCalledWith(false);
  });

  it("flush retries after a failed write instead of reporting 'unchanged'", async () => {
    vi.useFakeTimers();
    const persist = vi
      .fn<(contents: string) => Promise<AtomCommandResult<void, unknown>>>()
      .mockResolvedValueOnce(AsyncResult.failure(Cause.fail(new Error("write failed"))))
      .mockResolvedValueOnce(AsyncResult.success(undefined));
    const coordinator = new FileSaveCoordinator({
      debounceMs: 500,
      persist,
      onPendingChange: vi.fn(),
      onConfirmed: vi.fn(),
    });

    coordinator.change("draft");
    expect(await coordinator.flush()).toBe("failed");
    expect(await coordinator.flush()).toBe("saved");
    expect(persist).toHaveBeenCalledTimes(2);
  });

  it("dispose does not write again after a clean save (renamed/deleted files must stay gone)", async () => {
    vi.useFakeTimers();
    const persist = vi
      .fn<(contents: string) => Promise<AtomCommandResult<void, never>>>()
      .mockResolvedValue(AsyncResult.success(undefined));
    const coordinator = new FileSaveCoordinator({
      debounceMs: 500,
      persist,
      onPendingChange: vi.fn(),
      onConfirmed: vi.fn(),
    });

    coordinator.change("contents");
    expect(await coordinator.flush()).toBe("saved");
    expect(persist).toHaveBeenCalledOnce();

    // Unmount after the save settled — e.g. the file was just renamed and the
    // surface remapped. A second write here would resurrect the old path.
    coordinator.dispose();
    await vi.runAllTimersAsync();
    expect(persist).toHaveBeenCalledOnce();
  });

  it("dispose persists edits that were never saved (hot exit)", async () => {
    vi.useFakeTimers();
    const persist = vi
      .fn<(contents: string) => Promise<AtomCommandResult<void, never>>>()
      .mockResolvedValue(AsyncResult.success(undefined));
    const coordinator = new FileSaveCoordinator({
      debounceMs: 500,
      persist,
      onPendingChange: vi.fn(),
      onConfirmed: vi.fn(),
    });

    coordinator.change("unsaved");
    coordinator.dispose();
    await vi.runAllTimersAsync();
    expect(persist).toHaveBeenCalledOnce();
    expect(persist).toHaveBeenCalledWith("unsaved");
  });
});
