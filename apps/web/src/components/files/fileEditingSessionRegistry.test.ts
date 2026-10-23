import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import { AsyncResult } from "effect/reactivity";

import { FileEditingSession } from "./fileEditingSession";
import { FileEditingSessionRegistry } from "./fileEditingSessionRegistry";

function fakeSession(relativePath: string) {
  return {
    relativePath,
    flush: vi.fn(async () => "saved" as const),
    settle: vi.fn<() => Promise<"saved" | "failed">>(async () => "saved"),
    setAutosaveEnabled: vi.fn(),
    pauseSaving: vi.fn(),
    resumeSaving: vi.fn(),
    discardPendingSave: vi.fn(),
    rename: vi.fn(function rename(this: { relativePath: string }, next: string) {
      this.relativePath = next;
    }),
    dispose: vi.fn(),
  };
}

function deferredResult<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

describe("FileEditingSessionRegistry", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("reuses one session per exact open file", () => {
    const registry = new FileEditingSessionRegistry<ReturnType<typeof fakeSession>>();
    const create = vi.fn(() => fakeSession("src/app.ts"));

    expect(registry.getOrCreate("src/app.ts", create)).toBe(
      registry.getOrCreate("src/app.ts", create),
    );
    expect(create).toHaveBeenCalledOnce();
  });

  it("disables autosave for the active session and flushes it when another file activates", () => {
    const registry = new FileEditingSessionRegistry<ReturnType<typeof fakeSession>>();
    const first = registry.getOrCreate("src/a.ts", () => fakeSession("src/a.ts"));
    const second = registry.getOrCreate("src/b.ts", () => fakeSession("src/b.ts"));

    registry.setActivePath("src/a.ts");
    expect(first.setAutosaveEnabled).toHaveBeenLastCalledWith(false);
    expect(first.flush).not.toHaveBeenCalled();

    registry.setActivePath("src/b.ts");
    expect(first.setAutosaveEnabled).toHaveBeenLastCalledWith(true);
    expect(first.flush).toHaveBeenCalledOnce();
    expect(second.setAutosaveEnabled).toHaveBeenLastCalledWith(false);
  });

  it("flushes the active file when a non-file surface activates", () => {
    const registry = new FileEditingSessionRegistry<ReturnType<typeof fakeSession>>();
    const session = registry.getOrCreate("src/app.ts", () => fakeSession("src/app.ts"));

    registry.setActivePath("src/app.ts");
    registry.setActivePath(null);

    expect(session.setAutosaveEnabled.mock.calls).toEqual([[false], [true]]);
    expect(session.flush).toHaveBeenCalledOnce();
  });

  it("pauses every session across workspace loss without flushing on deactivation", () => {
    const registry = new FileEditingSessionRegistry<ReturnType<typeof fakeSession>>();
    const active = registry.getOrCreate("src/app.ts", () => fakeSession("src/app.ts"));
    registry.setActivePath("src/app.ts");

    registry.setSavingEnabled(false);
    registry.setActivePath(null);
    const addedWhileUnavailable = registry.getOrCreate("src/other.ts", () =>
      fakeSession("src/other.ts"),
    );

    expect(active.pauseSaving).toHaveBeenCalledOnce();
    expect(active.setAutosaveEnabled).toHaveBeenLastCalledWith(true);
    expect(active.flush).not.toHaveBeenCalled();
    expect(addedWhileUnavailable.pauseSaving).toHaveBeenCalledOnce();

    registry.setSavingEnabled(true);
    expect(active.resumeSaving).toHaveBeenCalledOnce();
    expect(addedWhileUnavailable.resumeSaving).toHaveBeenCalledOnce();
  });

  it("composes workspace suspension with an active path-mutation lease", async () => {
    const registry = new FileEditingSessionRegistry<ReturnType<typeof fakeSession>>();
    const session = registry.getOrCreate("src/app.ts", () => fakeSession("src/app.ts"));
    const lease = await registry.beginPathMutation({
      kind: "rename",
      fromRelativePath: "src/app.ts",
      toRelativePath: "src/renamed.ts",
    });

    registry.setSavingEnabled(false);
    registry.setSavingEnabled(true);
    expect(session.resumeSaving).not.toHaveBeenCalled();

    registry.setSavingEnabled(false);
    lease!.release();
    await vi.waitFor(() => expect(session.resumeSaving).not.toHaveBeenCalled());

    registry.setSavingEnabled(true);
    expect(session.resumeSaving).toHaveBeenCalledOnce();
  });

  it("does not resume a mutation-owned session while acquisition is settling", async () => {
    const registry = new FileEditingSessionRegistry<ReturnType<typeof fakeSession>>();
    const session = registry.getOrCreate("src/app.ts", () => fakeSession("src/app.ts"));
    const settlement = deferredResult<"saved" | "failed">();
    session.settle.mockReturnValue(settlement.promise);

    const leasePromise = registry.beginPathMutation({
      kind: "rename",
      fromRelativePath: "src/app.ts",
      toRelativePath: "src/renamed.ts",
    });
    registry.setSavingEnabled(false);
    registry.setSavingEnabled(true);

    expect(session.resumeSaving).not.toHaveBeenCalled();
    settlement.resolve("saved");
    const lease = await leasePromise;
    lease!.release();
    await vi.waitFor(() => expect(session.resumeSaving).toHaveBeenCalledOnce());
  });

  it("persists a paused active edit immediately when a deactivation is released", async () => {
    vi.useFakeTimers();
    const persist = vi.fn(async () => AsyncResult.success(undefined));
    const registry = new FileEditingSessionRegistry<FileEditingSession<never, void, never>>();
    const session = registry.getOrCreate(
      "src/app.ts",
      () =>
        new FileEditingSession({
          cwd: "/repo",
          relativePath: "src/app.ts",
          debounceMs: 500,
          persist,
          onPendingChange: vi.fn(),
          onConfirmed: vi.fn(),
        }),
    );
    registry.setActivePath("src/app.ts");
    const lease = await registry.beginPathMutation({
      kind: "rename",
      fromRelativePath: "src/app.ts",
      toRelativePath: "src/renamed.ts",
    });

    session.changeOutsideEditor("paused draft");
    registry.setActivePath(null);
    expect(persist).not.toHaveBeenCalled();

    lease!.release();
    await Promise.resolve();

    expect(persist).toHaveBeenCalledOnce();
    expect(persist).toHaveBeenCalledWith("src/app.ts", "paused draft");
  });

  it("keeps a paused edit explicit when its session reactivates before release", async () => {
    vi.useFakeTimers();
    const persist = vi.fn(async () => AsyncResult.success(undefined));
    const registry = new FileEditingSessionRegistry<FileEditingSession<never, void, never>>();
    const session = registry.getOrCreate(
      "src/app.ts",
      () =>
        new FileEditingSession({
          cwd: "/repo",
          relativePath: "src/app.ts",
          debounceMs: 500,
          persist,
          onPendingChange: vi.fn(),
          onConfirmed: vi.fn(),
        }),
    );
    registry.setActivePath("src/app.ts");
    const lease = await registry.beginPathMutation({
      kind: "rename",
      fromRelativePath: "src/app.ts",
      toRelativePath: "src/renamed.ts",
    });

    session.changeOutsideEditor("paused draft");
    const flush = vi.spyOn(session, "flush");
    registry.setActivePath(null);
    const pausedFlush = flush.mock.results[0]!.value;
    registry.setActivePath("src/app.ts");
    await expect(pausedFlush).resolves.toBe("saving");
    lease!.release();
    await Promise.resolve();
    await vi.advanceTimersByTimeAsync(5_000);

    expect(persist).not.toHaveBeenCalled();
    expect(session.getSnapshot().save).toMatchObject({ phase: "pending", canSave: true });
  });

  it("reports a rejected paused lifecycle write without an unhandled rejection or retry", async () => {
    vi.useFakeTimers();
    const saveError = new Error("write crashed");
    const persist = vi.fn().mockRejectedValue(saveError);
    const reportError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    const unhandledRejections: unknown[] = [];
    const onUnhandledRejection = (reason: unknown) => {
      unhandledRejections.push(reason);
    };
    process.on("unhandledRejection", onUnhandledRejection);

    try {
      const registry = new FileEditingSessionRegistry<FileEditingSession<never, void, never>>();
      const session = registry.getOrCreate(
        "src/app.ts",
        () =>
          new FileEditingSession({
            cwd: "/repo",
            relativePath: "src/app.ts",
            debounceMs: 500,
            persist,
            onPendingChange: vi.fn(),
            onConfirmed: vi.fn(),
          }),
      );
      registry.setActivePath("src/app.ts");
      const lease = await registry.beginPathMutation({
        kind: "rename",
        fromRelativePath: "src/app.ts",
        toRelativePath: "src/renamed.ts",
      });

      session.changeOutsideEditor("paused draft");
      registry.setActivePath(null);
      lease!.release();
      await vi.advanceTimersByTimeAsync(0);
      await Promise.resolve();

      expect(reportError).toHaveBeenCalledWith(
        "[file-editing-session-registry] session cleanup failed",
        saveError,
      );
      expect(session.getSnapshot().save).toMatchObject({ phase: "failed", canSave: true });
      await vi.advanceTimersByTimeAsync(5_000);
      expect(persist).toHaveBeenCalledOnce();
      expect(unhandledRejections).toEqual([]);
    } finally {
      process.off("unhandledRejection", onUnhandledRejection);
      reportError.mockRestore();
    }
  });

  it("applies the active policy to a session created after activation", () => {
    const registry = new FileEditingSessionRegistry<ReturnType<typeof fakeSession>>();

    registry.setActivePath("src/later.ts");
    const session = registry.getOrCreate("src/later.ts", () => fakeSession("src/later.ts"));

    expect(session.setAutosaveEnabled).toHaveBeenCalledOnce();
    expect(session.setAutosaveEnabled).toHaveBeenCalledWith(false);
  });

  it("does not flush when the same active path is published twice", () => {
    const registry = new FileEditingSessionRegistry<ReturnType<typeof fakeSession>>();
    const session = registry.getOrCreate("src/app.ts", () => fakeSession("src/app.ts"));

    registry.setActivePath("src/app.ts");
    registry.setActivePath("src/app.ts");

    expect(session.setAutosaveEnabled).toHaveBeenCalledOnce();
    expect(session.flush).not.toHaveBeenCalled();
  });

  it("keeps a source session active when it replaces the active rename destination", async () => {
    const registry = new FileEditingSessionRegistry<ReturnType<typeof fakeSession>>();
    const source = registry.getOrCreate("src/a.ts", () => fakeSession("src/a.ts"));
    registry.getOrCreate("lib/a.ts", () => fakeSession("lib/a.ts"));
    registry.setActivePath("lib/a.ts");
    source.setAutosaveEnabled.mockClear();

    const lease = await registry.beginPathMutation({
      kind: "rename",
      fromRelativePath: "src/a.ts",
      toRelativePath: "lib/a.ts",
    });
    lease!.commitRename("lib/a.ts");
    lease!.release();

    expect(registry.get("lib/a.ts")).toBe(source);
    expect(source.setAutosaveEnabled).toHaveBeenCalledExactlyOnceWith(false);
    expect(source.flush).not.toHaveBeenCalled();
    registry.setActivePath("lib/a.ts");
    expect(source.setAutosaveEnabled).toHaveBeenCalledExactlyOnceWith(false);
  });

  it("remaps the active path so leaving a renamed file still flushes its session", async () => {
    const registry = new FileEditingSessionRegistry<ReturnType<typeof fakeSession>>();
    const session = registry.getOrCreate("src/old.ts", () => fakeSession("src/old.ts"));
    registry.setActivePath("src/old.ts");
    const lease = await registry.beginPathMutation({
      kind: "rename",
      fromRelativePath: "src/old.ts",
      toRelativePath: "src/new.ts",
    });

    lease!.commitRename("src/new.ts");
    lease!.release();
    session.flush.mockClear();
    registry.setActivePath(null);

    expect(session.flush).toHaveBeenCalledOnce();
  });

  it("clears an active path when its session is deleted", async () => {
    const registry = new FileEditingSessionRegistry<ReturnType<typeof fakeSession>>();
    const session = registry.getOrCreate("src/app.ts", () => fakeSession("src/app.ts"));
    registry.setActivePath("src/app.ts");
    const lease = await registry.beginPathMutation({ kind: "delete", relativePath: "src/app.ts" });

    lease!.commitDelete();
    lease!.release();
    session.flush.mockClear();
    registry.setActivePath(null);

    expect(session.flush).not.toHaveBeenCalled();
  });

  it("does not apply the active policy to a replacement created after deletion", async () => {
    const registry = new FileEditingSessionRegistry<ReturnType<typeof fakeSession>>();
    registry.getOrCreate("src/app.ts", () => fakeSession("src/app.ts"));
    registry.setActivePath("src/app.ts");
    const lease = await registry.beginPathMutation({ kind: "delete", relativePath: "src/app.ts" });

    lease!.commitDelete();
    lease!.release();
    const replacement = registry.getOrCreate("src/app.ts", () => fakeSession("src/app.ts"));

    expect(replacement.setAutosaveEnabled).not.toHaveBeenCalled();
  });

  it("reports a rejected background flush without throwing from an active transition", async () => {
    const reportError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    try {
      const registry = new FileEditingSessionRegistry<ReturnType<typeof fakeSession>>();
      const session = registry.getOrCreate("src/app.ts", () => fakeSession("src/app.ts"));
      session.flush.mockRejectedValue(new Error("save crashed"));
      registry.setActivePath("src/app.ts");

      expect(() => registry.setActivePath(null)).not.toThrow();
      await Promise.resolve();

      expect(reportError).toHaveBeenCalledWith(
        "[file-editing-session-registry] session cleanup failed",
        expect.objectContaining({ message: "save crashed" }),
      );
    } finally {
      reportError.mockRestore();
    }
  });

  it("settles exact and descendant paths but not prefix lookalikes", async () => {
    const registry = new FileEditingSessionRegistry<ReturnType<typeof fakeSession>>();
    const child = registry.getOrCreate("src/a.ts", () => fakeSession("src/a.ts"));
    const lookalike = registry.getOrCreate("srcfoo/b.ts", () => fakeSession("srcfoo/b.ts"));

    const lease = await registry.beginPathMutation({
      kind: "rename",
      fromRelativePath: "src",
      toRelativePath: "lib",
    });
    expect(lease).not.toBeNull();
    expect(child.settle).toHaveBeenCalledOnce();
    expect(child.pauseSaving).toHaveBeenCalledOnce();
    expect(lookalike.settle).not.toHaveBeenCalled();
    expect(lookalike.pauseSaving).not.toHaveBeenCalled();
    lease!.release();
    expect(child.resumeSaving).toHaveBeenCalledOnce();
  });

  it("aborts path preparation when any session fails to settle", async () => {
    const registry = new FileEditingSessionRegistry<ReturnType<typeof fakeSession>>();
    const session = registry.getOrCreate("src/a.ts", () => fakeSession("src/a.ts"));
    session.settle.mockResolvedValue("failed");

    await expect(
      registry.beginPathMutation({ kind: "delete", relativePath: "src" }),
    ).resolves.toBeNull();
    expect(session.pauseSaving).not.toHaveBeenCalled();
  });

  it("remaps descendants and disposes deleted sessions through mutation leases", async () => {
    const registry = new FileEditingSessionRegistry<ReturnType<typeof fakeSession>>();
    const session = registry.getOrCreate("src/nested/a.ts", () => fakeSession("src/nested/a.ts"));

    const renameLease = await registry.beginPathMutation({
      kind: "rename",
      fromRelativePath: "src",
      toRelativePath: "lib",
    });
    renameLease!.commitRename("lib");
    renameLease!.release();
    expect(session.rename).toHaveBeenCalledWith("lib/nested/a.ts");
    expect(registry.get("lib/nested/a.ts")).toBe(session);

    const deleteLease = await registry.beginPathMutation({
      kind: "delete",
      relativePath: "lib",
    });
    deleteLease!.commitDelete();
    deleteLease!.release();
    expect(session.dispose).toHaveBeenCalledOnce();
    expect(registry.get("lib/nested/a.ts")).toBeUndefined();
  });

  it("contains per-session delete cleanup failures and removes every descendant", async () => {
    const reportError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    try {
      const registry = new FileEditingSessionRegistry<ReturnType<typeof fakeSession>>();
      const discardRejected = registry.getOrCreate("src/a.ts", () => fakeSession("src/a.ts"));
      const disposeRejected = registry.getOrCreate("src/b.ts", () => fakeSession("src/b.ts"));
      const cleaned = registry.getOrCreate("src/nested/c.ts", () => fakeSession("src/nested/c.ts"));
      const unrelated = registry.getOrCreate("other.ts", () => fakeSession("other.ts"));
      discardRejected.discardPendingSave.mockImplementation(() => {
        throw new Error("discard failed");
      });
      disposeRejected.dispose.mockImplementation(() => {
        throw new Error("dispose failed");
      });
      const lease = await registry.beginPathMutation({
        kind: "delete",
        relativePath: "src",
      });

      expect(() => lease!.commitDelete()).not.toThrow();
      lease!.release();

      expect(registry.get("src/a.ts")).toBeUndefined();
      expect(registry.get("src/b.ts")).toBeUndefined();
      expect(registry.get("src/nested/c.ts")).toBeUndefined();
      expect(registry.get("other.ts")).toBe(unrelated);
      for (const session of [discardRejected, disposeRejected, cleaned]) {
        expect(session.discardPendingSave).toHaveBeenCalledOnce();
        expect(session.dispose).toHaveBeenCalledOnce();
        expect(session.resumeSaving).not.toHaveBeenCalled();
      }
      expect(unrelated.discardPendingSave).not.toHaveBeenCalled();
      expect(unrelated.dispose).not.toHaveBeenCalled();
      expect(reportError).toHaveBeenCalledWith(
        "[file-editing-session-registry] session cleanup failed",
        expect.objectContaining({ message: "discard failed" }),
      );
      expect(reportError).toHaveBeenCalledWith(
        "[file-editing-session-registry] session cleanup failed",
        expect.objectContaining({ message: "dispose failed" }),
      );
      const nextLease = await registry.beginPathMutation({
        kind: "delete",
        relativePath: "src",
      });
      expect(nextLease).not.toBeNull();
      nextLease!.release();
    } finally {
      reportError.mockRestore();
    }
  });

  it("reconciles removed sessions after settling them", async () => {
    const registry = new FileEditingSessionRegistry<ReturnType<typeof fakeSession>>();
    const retained = registry.getOrCreate("a.ts", () => fakeSession("a.ts"));
    const removed = registry.getOrCreate("b.ts", () => fakeSession("b.ts"));
    const calls: string[] = [];
    removed.settle.mockImplementation(async () => {
      calls.push("settle");
      return "saved";
    });
    removed.dispose.mockImplementation(() => calls.push("dispose"));

    await registry.reconcile(["a.ts"]);

    expect(registry.get("a.ts")).toBe(retained);
    expect(registry.get("b.ts")).toBeUndefined();
    expect(removed.settle).toHaveBeenCalledOnce();
    expect(removed.dispose).toHaveBeenCalledOnce();
    expect(calls).toEqual(["settle", "dispose"]);
  });

  it.each([
    ["rename", "saved"],
    ["rename", "failed"],
    ["delete", "saved"],
    ["delete", "failed"],
  ] as const)(
    "keeps a closing session visible while a deferred %s mutation follows a %s settle",
    async (operation, settleResult) => {
      const registry = new FileEditingSessionRegistry<ReturnType<typeof fakeSession>>();
      const session = registry.getOrCreate("src/nested/app.ts", () =>
        fakeSession("src/nested/app.ts"),
      );
      const settlement = deferredResult<"saved" | "failed">();
      session.settle.mockReturnValueOnce(settlement.promise);

      const closing = registry.reconcile([]);
      let acquisitionCompleted = false;
      const acquisition = registry
        .beginPathMutation(
          operation === "rename"
            ? {
                kind: "rename",
                fromRelativePath: "src",
                toRelativePath: "lib",
              }
            : { kind: "delete", relativePath: "src" },
        )
        .then((lease) => {
          acquisitionCompleted = true;
          return lease;
        });
      await Promise.resolve();

      expect(acquisitionCompleted).toBe(false);
      expect(session.dispose).not.toHaveBeenCalled();

      settlement.resolve(settleResult);
      const lease = await acquisition;
      await closing;

      if (settleResult === "failed") {
        expect(lease).toBeNull();
        expect(registry.get("src/nested/app.ts")).toBe(session);
        expect(session.pauseSaving).not.toHaveBeenCalled();
        expect(session.dispose).not.toHaveBeenCalled();
        return;
      }

      expect(lease).not.toBeNull();
      expect(session.pauseSaving).toHaveBeenCalledOnce();
      expect(session.dispose).not.toHaveBeenCalled();
      if (operation === "rename") {
        lease!.commitRename("lib");
        expect(session.rename).toHaveBeenCalledWith("lib/nested/app.ts");
      } else {
        lease!.commitDelete();
      }
      lease!.release();

      expect(session.settle).toHaveBeenCalledOnce();
      await vi.waitFor(() => expect(session.dispose).toHaveBeenCalledOnce());
    },
  );

  it("settles and disposes a concurrently reconciled session exactly once", async () => {
    const registry = new FileEditingSessionRegistry<ReturnType<typeof fakeSession>>();
    const session = registry.getOrCreate("src/app.ts", () => fakeSession("src/app.ts"));
    const settlement = deferredResult<"saved">();
    session.settle.mockReturnValueOnce(settlement.promise);

    const firstClosing = registry.reconcile([]);
    const secondClosing = registry.reconcile([]);
    settlement.resolve("saved");
    await Promise.all([firstClosing, secondClosing]);

    expect(session.settle).toHaveBeenCalledOnce();
    expect(session.dispose).toHaveBeenCalledOnce();
    expect(registry.get("src/app.ts")).toBeUndefined();
  });

  it("retains rejected sessions and disposes successfully settled sessions during reconciliation", async () => {
    const reportError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    try {
      const registry = new FileEditingSessionRegistry<ReturnType<typeof fakeSession>>();
      const retained = registry.getOrCreate("a.ts", () => fakeSession("a.ts"));
      const rejected = registry.getOrCreate("b.ts", () => fakeSession("b.ts"));
      const removed = registry.getOrCreate("c.ts", () => fakeSession("c.ts"));
      rejected.settle.mockRejectedValue(new Error("save failed"));

      await expect(registry.reconcile(["a.ts"])).resolves.toBeUndefined();

      expect(registry.get("a.ts")).toBe(retained);
      expect(registry.get("b.ts")).toBe(rejected);
      expect(registry.get("c.ts")).toBeUndefined();
      expect(retained.settle).not.toHaveBeenCalled();
      expect(retained.dispose).not.toHaveBeenCalled();
      expect(rejected.dispose).not.toHaveBeenCalled();
      expect(removed.dispose).toHaveBeenCalledOnce();
      expect(reportError).toHaveBeenCalledWith(
        "[file-editing-session-registry] session cleanup failed",
        expect.objectContaining({ message: "save failed" }),
      );
    } finally {
      reportError.mockRestore();
    }
  });

  it("settles every session before disposing the registry", async () => {
    const registry = new FileEditingSessionRegistry<ReturnType<typeof fakeSession>>();
    const first = registry.getOrCreate("a.ts", () => fakeSession("a.ts"));
    const second = registry.getOrCreate("b.ts", () => fakeSession("b.ts"));
    const calls: string[] = [];
    for (const [name, session] of [
      ["a", first],
      ["b", second],
    ] as const) {
      session.settle.mockImplementation(async () => {
        calls.push(`settle-${name}`);
        return "saved";
      });
      session.dispose.mockImplementation(() => calls.push(`dispose-${name}`));
    }

    await registry.dispose();

    expect(first.settle).toHaveBeenCalledOnce();
    expect(first.dispose).toHaveBeenCalledOnce();
    expect(second.settle).toHaveBeenCalledOnce();
    expect(second.dispose).toHaveBeenCalledOnce();
    expect(calls.indexOf("settle-a")).toBeLessThan(calls.indexOf("dispose-a"));
    expect(calls.indexOf("settle-b")).toBeLessThan(calls.indexOf("dispose-b"));
  });

  it("disposes every session and contains settle rejections during registry disposal", async () => {
    const reportError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    try {
      const registry = new FileEditingSessionRegistry<ReturnType<typeof fakeSession>>();
      const rejected = registry.getOrCreate("a.ts", () => fakeSession("a.ts"));
      const settled = registry.getOrCreate("b.ts", () => fakeSession("b.ts"));
      rejected.settle.mockRejectedValue(new Error("save failed"));

      await expect(registry.dispose()).resolves.toBeUndefined();

      expect(rejected.dispose).toHaveBeenCalledOnce();
      expect(settled.dispose).toHaveBeenCalledOnce();
      expect(reportError).toHaveBeenCalledWith(
        "[file-editing-session-registry] session cleanup failed",
        expect.objectContaining({ message: "save failed" }),
      );
    } finally {
      reportError.mockRestore();
    }
  });

  it("contains synchronous session disposal errors and continues registry disposal", async () => {
    const reportError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    try {
      const registry = new FileEditingSessionRegistry<ReturnType<typeof fakeSession>>();
      const rejected = registry.getOrCreate("a.ts", () => fakeSession("a.ts"));
      const disposed = registry.getOrCreate("b.ts", () => fakeSession("b.ts"));
      rejected.dispose.mockImplementation(() => {
        throw new Error("dispose failed");
      });

      await expect(registry.dispose()).resolves.toBeUndefined();

      expect(rejected.dispose).toHaveBeenCalledOnce();
      expect(disposed.dispose).toHaveBeenCalledOnce();
      expect(reportError).toHaveBeenCalledWith(
        "[file-editing-session-registry] session cleanup failed",
        expect.objectContaining({ message: "dispose failed" }),
      );
    } finally {
      reportError.mockRestore();
    }
  });

  it("disposes a remap collision and retains the renamed source session", async () => {
    const registry = new FileEditingSessionRegistry<ReturnType<typeof fakeSession>>();
    const source = registry.getOrCreate("src/a.ts", () => fakeSession("src/a.ts"));
    const displaced = registry.getOrCreate("lib/a.ts", () => fakeSession("lib/a.ts"));

    const lease = await registry.beginPathMutation({
      kind: "rename",
      fromRelativePath: "src",
      toRelativePath: "lib",
    });
    lease!.commitRename("lib");
    lease!.release();

    expect(displaced.dispose).toHaveBeenCalledOnce();
    expect(source.dispose).not.toHaveBeenCalled();
    expect(source.rename).toHaveBeenCalledWith("lib/a.ts");
    expect(registry.get("lib/a.ts")).toBe(source);
  });

  it("leases a rename destination and contains displaced-session cleanup failures", async () => {
    const reportError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    try {
      const registry = new FileEditingSessionRegistry<ReturnType<typeof fakeSession>>();
      const source = registry.getOrCreate("src/a.ts", () => fakeSession("src/a.ts"));
      const displaced = registry.getOrCreate("lib/a.ts", () => fakeSession("lib/a.ts"));
      displaced.dispose.mockImplementation(() => {
        throw new Error("destination cleanup failed");
      });

      const lease = await registry.beginPathMutation({
        kind: "rename",
        fromRelativePath: "src",
        toRelativePath: "lib",
      });

      expect(source.settle).toHaveBeenCalledOnce();
      expect(displaced.settle).toHaveBeenCalledOnce();
      expect(source.pauseSaving).toHaveBeenCalledOnce();
      expect(displaced.pauseSaving).toHaveBeenCalledOnce();
      expect(() => lease!.commitRename("lib")).not.toThrow();
      lease!.release();

      expect(displaced.discardPendingSave).toHaveBeenCalledOnce();
      expect(displaced.dispose).toHaveBeenCalledOnce();
      expect(displaced.resumeSaving).not.toHaveBeenCalled();
      expect(source.rename).toHaveBeenCalledWith("lib/a.ts");
      expect(registry.get("lib/a.ts")).toBe(source);
      expect(reportError).toHaveBeenCalledWith(
        "[file-editing-session-registry] session cleanup failed",
        expect.objectContaining({ message: "destination cleanup failed" }),
      );
    } finally {
      reportError.mockRestore();
    }
  });

  it("defers reconciliation teardown until an active mutation lease releases", async () => {
    const registry = new FileEditingSessionRegistry<ReturnType<typeof fakeSession>>();
    const session = registry.getOrCreate("src/a.ts", () => fakeSession("src/a.ts"));
    const lease = await registry.beginPathMutation({
      kind: "delete",
      relativePath: "src/a.ts",
    });
    expect(lease).not.toBeNull();

    await registry.reconcile([]);
    expect(session.dispose).not.toHaveBeenCalled();

    lease!.release();
    expect(session.resumeSaving).toHaveBeenCalledOnce();
    await vi.waitFor(() => expect(session.dispose).toHaveBeenCalledOnce());
  });

  it("waits to dispose a leased session until its rename outcome is known", async () => {
    const registry = new FileEditingSessionRegistry<ReturnType<typeof fakeSession>>();
    const session = registry.getOrCreate("src/old.ts", () => fakeSession("src/old.ts"));
    const lease = await registry.beginPathMutation({
      kind: "rename",
      fromRelativePath: "src/old.ts",
      toRelativePath: "src/new.ts",
    });
    expect(lease).not.toBeNull();

    let disposed = false;
    const disposal = registry.dispose().then(() => {
      disposed = true;
    });
    await Promise.resolve();
    expect(disposed).toBe(false);
    expect(session.dispose).not.toHaveBeenCalled();

    lease!.commitRename("src/new.ts");
    lease!.release();
    await disposal;
    expect(session.rename).toHaveBeenCalledWith("src/new.ts");
    expect(session.resumeSaving).toHaveBeenCalledOnce();
    expect(session.dispose).toHaveBeenCalledOnce();
  });

  it("remaps the reconciled open path before releasing a rename lease", async () => {
    const registry = new FileEditingSessionRegistry<ReturnType<typeof fakeSession>>();
    const session = registry.getOrCreate("src/old.ts", () => fakeSession("src/old.ts"));
    const lookalike = registry.getOrCreate("src/old.ts.bak", () => fakeSession("src/old.ts.bak"));
    await registry.reconcile(["src/old.ts", "src/old.ts.bak"]);
    const lease = await registry.beginPathMutation({
      kind: "rename",
      fromRelativePath: "src/old.ts",
      toRelativePath: "src/new.ts",
    });

    lease!.commitRename("src/new.ts");
    lease!.release();
    await Promise.resolve();
    await Promise.resolve();

    expect(registry.get("src/new.ts")).toBe(session);
    expect(registry.get("src/old.ts.bak")).toBe(lookalike);
    expect(session.dispose).not.toHaveBeenCalled();
    expect(lookalike.rename).not.toHaveBeenCalled();
  });

  it("always completes disposal and reports a rejected leased-session cleanup", async () => {
    const reportError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    try {
      const registry = new FileEditingSessionRegistry<ReturnType<typeof fakeSession>>();
      const session = registry.getOrCreate("src/app.ts", () => fakeSession("src/app.ts"));
      session.settle.mockResolvedValueOnce("saved").mockRejectedValueOnce(new Error("save failed"));
      const lease = await registry.beginPathMutation({
        kind: "delete",
        relativePath: "src/app.ts",
      });
      let disposalCompleted = false;
      const disposal = registry.dispose().then(() => {
        disposalCompleted = true;
      });

      lease!.release();
      await expect(disposal).resolves.toBeUndefined();
      expect(disposalCompleted).toBe(true);
      expect(session.dispose).toHaveBeenCalledOnce();
      expect(reportError).toHaveBeenCalledWith(
        "[file-editing-session-registry] session cleanup failed",
        expect.objectContaining({ message: "save failed" }),
      );
    } finally {
      reportError.mockRestore();
    }
  });
});
