import { beforeEach, describe, expect, it, vi } from "vite-plus/test";
import type { AtomCommandResult } from "@coddy/client-runtime/state/runtime";
import * as Cause from "effect/Cause";
import { AsyncResult } from "effect/reactivity";

const pierre = vi.hoisted(() => {
  const instances: Editor[] = [];
  class Editor {
    readonly options: Record<string, unknown>;
    canUndo = false;
    canRedo = false;
    readonly undo = vi.fn();
    readonly redo = vi.fn();
    readonly cleanUp = vi.fn();

    constructor(options: Record<string, unknown>) {
      this.options = options;
      instances.push(this);
    }
  }
  return { Editor, instances };
});

vi.mock("@pierre/diffs/edit", () => ({ Editor: pierre.Editor }));

import { FileEditingSession } from "./fileEditingSession";
import { FileEditingSessionRegistry } from "./fileEditingSessionRegistry";

function deferred() {
  let resolve!: () => void;
  const promise = new Promise<void>((resolvePromise) => {
    resolve = resolvePromise;
  });
  return { promise, resolve };
}

function deferredResult<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, resolve, reject };
}

describe("FileEditingSession", () => {
  beforeEach(() => {
    vi.useRealTimers();
    pierre.instances.length = 0;
  });

  it("creates Pierre lazily for source access and reuses that editor", () => {
    const session = new FileEditingSession({
      cwd: "/repo",
      relativePath: "README.md",
      debounceMs: 500,
      persist: vi.fn(async () => AsyncResult.success(undefined)),
      onPendingChange: vi.fn(),
      onConfirmed: vi.fn(),
    });

    expect(pierre.instances).toHaveLength(0);
    session.changeOutsideEditor("- [x] rendered-only edit\n");
    expect(pierre.instances).toHaveLength(0);

    const editor = session.editor;
    expect(session.editor).toBe(editor);
    expect(pierre.instances).toEqual([editor]);
  });

  it("publishes native history capabilities from the persistent source editor", () => {
    const session = new FileEditingSession({
      cwd: "/repo",
      relativePath: "src/app.ts",
      debounceMs: 500,
      persist: vi.fn(async () => AsyncResult.success(undefined)),
      onPendingChange: vi.fn(),
      onConfirmed: vi.fn(),
    });
    const editor = session.editor as unknown as InstanceType<typeof pierre.Editor>;
    expect(editor.options["persistState"]).toBe(true);
    expect(session.cacheKey).toMatch(/^\/repo:file-editing-session:/);

    editor.canUndo = true;
    (editor.options["onAttach"] as () => void)();
    expect(session.getSnapshot().canUndo).toBe(true);

    session.undo();
    expect(editor.undo).toHaveBeenCalledOnce();
  });

  it("retains its editor's edited document, and an outside change only until it is saved", async () => {
    const session = new FileEditingSession({
      cwd: "/repo",
      relativePath: "README.md",
      debounceMs: 500,
      persist: vi.fn(async () => AsyncResult.success(undefined)),
      onPendingChange: vi.fn(),
      onConfirmed: vi.fn(),
    });
    expect(session.retainedContents).toBeNull();

    const editor = session.editor as unknown as InstanceType<typeof pierre.Editor>;
    (editor.options["onChange"] as (file: { contents: string }) => void)({
      contents: "- [ ] typed\n",
    });
    expect(session.retainedContents).toBe("- [ ] typed\n");
    await expect(session.flush()).resolves.toBe("saved");
    expect(session.retainedContents).toBe("- [ ] typed\n");

    // A rendered-view toggle replaces the editor document; once saved, a newer
    // read of the file seeds the next one.
    session.changeOutsideEditor("- [x] typed\n");
    expect(session.retainedContents).toBe("- [x] typed\n");
    await expect(session.flush()).resolves.toBe("saved");
    expect(session.retainedContents).toBeNull();
  });

  it("exposes its contents as unsaved until a save of them is confirmed", async () => {
    const save = deferredResult<AtomCommandResult<void, string>>();
    const persist = vi
      .fn<(relativePath: string, contents: string) => Promise<AtomCommandResult<void, string>>>()
      .mockResolvedValueOnce(AsyncResult.failure(Cause.fail("disk full")))
      .mockReturnValueOnce(save.promise);
    const session = new FileEditingSession({
      cwd: "/repo",
      relativePath: "README.md",
      debounceMs: 500,
      persist,
      onPendingChange: vi.fn(),
      onConfirmed: vi.fn(),
    });
    session.setAutosaveEnabled(false);
    expect(session.unsavedContents).toBeNull();

    const editor = session.editor as unknown as InstanceType<typeof pierre.Editor>;
    (editor.options["onChange"] as (file: { contents: string }) => void)({
      contents: "- [ ] typed\n",
    });
    expect(session.unsavedContents).toBe("- [ ] typed\n");

    await expect(session.flush()).resolves.toBe("failed");
    expect(session.unsavedContents).toBe("- [ ] typed\n");

    const flushed = session.flush();
    await vi.waitFor(() => expect(persist).toHaveBeenCalledTimes(2));
    expect(session.unsavedContents).toBe("- [ ] typed\n");
    save.resolve(AsyncResult.success(undefined));
    await expect(flushed).resolves.toBe("saved");
    expect(session.unsavedContents).toBeNull();
    expect(session.retainedContents).toBe("- [ ] typed\n");
  });

  it("uses the current renamed path for later persistence", async () => {
    vi.useFakeTimers();
    const persist = vi.fn(async () => AsyncResult.success(undefined));
    const session = new FileEditingSession({
      cwd: "/repo",
      relativePath: "src/old.ts",
      debounceMs: 500,
      persist,
      onPendingChange: vi.fn(),
      onConfirmed: vi.fn(),
    });
    session.rename("src/new.ts");
    const editor = session.editor as unknown as InstanceType<typeof pierre.Editor>;

    (editor.options["onChange"] as (file: { contents: string }) => void)({
      contents: "renamed contents",
    });
    await vi.advanceTimersByTimeAsync(500);

    expect(persist).toHaveBeenCalledWith("src/new.ts", "renamed contents");
  });

  it("invalidates existing source history without eagerly constructing a replacement", () => {
    const session = new FileEditingSession({
      cwd: "/repo",
      relativePath: "README.md",
      debounceMs: 500,
      persist: vi.fn(async () => AsyncResult.success(undefined)),
      onPendingChange: vi.fn(),
      onConfirmed: vi.fn(),
    });
    const firstEditor = session.editor;
    const firstCacheKey = session.cacheKey;

    session.changeOutsideEditor("- [x] done\n");

    expect(firstEditor.cleanUp).toHaveBeenCalledOnce();
    expect(pierre.instances).toEqual([firstEditor]);
    expect(session.cacheKey).not.toBe(firstCacheKey);
    expect(session.getSnapshot().save.phase).toBe("pending");
    expect(session.getSnapshot().canUndo).toBe(false);
    expect(session.editor).not.toBe(firstEditor);
    expect(pierre.instances).toHaveLength(2);
  });

  it("migrates a pending marker from the old file path to the renamed path", () => {
    const onPendingChange = vi.fn();
    const session = new FileEditingSession({
      cwd: "/repo",
      relativePath: "src/old.ts",
      debounceMs: 500,
      persist: vi.fn(async () => AsyncResult.success(undefined)),
      onPendingChange,
      onConfirmed: vi.fn(),
    });
    session.pauseSaving();
    const editor = session.editor as unknown as InstanceType<typeof pierre.Editor>;
    (editor.options["onChange"] as (file: { contents: string }) => void)({
      contents: "edited during rename",
    });

    session.rename("src/new.ts");

    expect(onPendingChange.mock.calls).toEqual([
      ["src/old.ts", true],
      ["src/old.ts", false],
      ["src/new.ts", true],
    ]);
  });

  it("migrates pending descendant markers during a directory rename", async () => {
    const onPendingChange = vi.fn();
    const registry = new FileEditingSessionRegistry<FileEditingSession<never>>();
    const session = registry.getOrCreate(
      "src/nested/a.ts",
      () =>
        new FileEditingSession({
          cwd: "/repo",
          relativePath: "src/nested/a.ts",
          debounceMs: 500,
          persist: vi.fn(async () => AsyncResult.success(undefined)),
          onPendingChange,
          onConfirmed: vi.fn(),
        }),
    );
    const lease = await registry.beginPathMutation({
      kind: "rename",
      fromRelativePath: "src",
      toRelativePath: "lib",
    });
    const editor = session.editor as unknown as InstanceType<typeof pierre.Editor>;
    (editor.options["onChange"] as (file: { contents: string }) => void)({
      contents: "edited during directory rename",
    });

    lease!.commitRename("lib");
    lease!.release();

    expect(onPendingChange.mock.calls).toEqual([
      ["src/nested/a.ts", true],
      ["src/nested/a.ts", false],
      ["lib/nested/a.ts", true],
    ]);
  });

  it("keeps active edits pending while preserving explicit Save", async () => {
    vi.useFakeTimers();
    const persist = vi.fn(async () => AsyncResult.success(undefined));
    const session = new FileEditingSession({
      cwd: "/repo",
      relativePath: "src/app.ts",
      debounceMs: 500,
      persist,
      onPendingChange: vi.fn(),
      onConfirmed: vi.fn(),
    });
    const editor = session.editor as unknown as InstanceType<typeof pierre.Editor>;
    const onChange = editor.options["onChange"] as (file: { contents: string }) => void;

    session.setAutosaveEnabled(false);
    onChange({ contents: "active draft" });
    await vi.advanceTimersByTimeAsync(500);

    expect(persist).not.toHaveBeenCalled();
    expect(session.getSnapshot().save).toMatchObject({ phase: "pending", canSave: true });
    await expect(session.flush()).resolves.toBe("saved");
    expect(persist).toHaveBeenCalledWith("src/app.ts", "active draft");
  });

  it("delegates redo, flush, and settle and stops notifications after unsubscribe/dispose", async () => {
    const persist = vi.fn(async () => AsyncResult.success(undefined));
    const session = new FileEditingSession({
      cwd: "/repo",
      relativePath: "src/app.ts",
      debounceMs: 500,
      persist,
      onPendingChange: vi.fn(),
      onConfirmed: vi.fn(),
    });
    const editor = session.editor as unknown as InstanceType<typeof pierre.Editor>;
    const listener = vi.fn();
    const unsubscribe = session.subscribe(listener);
    editor.canRedo = true;
    (editor.options["onAttach"] as () => void)();
    session.redo();
    expect(editor.redo).toHaveBeenCalledOnce();

    (editor.options["onChange"] as (file: { contents: string }) => void)({
      contents: "flush me",
    });
    await expect(session.flush()).resolves.toBe("saved");
    expect(persist).toHaveBeenCalledWith("src/app.ts", "flush me");
    (editor.options["onChange"] as (file: { contents: string }) => void)({
      contents: "settle me",
    });
    await expect(session.settle()).resolves.toBe("saved");
    expect(persist).toHaveBeenLastCalledWith("src/app.ts", "settle me");

    unsubscribe();
    const notificationsBeforeDispose = listener.mock.calls.length;
    (editor.options["onAttach"] as () => void)();
    expect(listener).toHaveBeenCalledTimes(notificationsBeforeDispose);

    session.dispose();
    session.dispose();
    expect(editor.cleanUp).toHaveBeenCalledOnce();
    const postDisposeListener = vi.fn();
    session.subscribe(postDisposeListener);
    (editor.options["onAttach"] as () => void)();
    expect(postDisposeListener).not.toHaveBeenCalled();
  });

  it("retains a failed closing session so reopening preserves its editor, history, and retryable save", async () => {
    vi.useFakeTimers();
    const persist = vi.fn<
      (relativePath: string, contents: string) => Promise<AtomCommandResult<void, Error>>
    >(async () => AsyncResult.failure(Cause.fail(new Error("save failed"))));
    const onPendingChange = vi.fn();
    const registry = new FileEditingSessionRegistry<FileEditingSession<never>>();
    const createSession = vi.fn(
      () =>
        new FileEditingSession<never, void, Error>({
          cwd: "/repo",
          relativePath: "src/app.ts",
          debounceMs: 500,
          persist,
          onPendingChange,
          onConfirmed: vi.fn(),
        }),
    );
    const session = registry.getOrCreate("src/app.ts", createSession);
    const editor = session.editor as unknown as InstanceType<typeof pierre.Editor>;
    editor.canUndo = true;
    (editor.options["onChange"] as (file: { contents: string }) => void)({
      contents: "retryable contents",
    });
    (editor.options["onAttach"] as () => void)();

    await registry.reconcile([]);

    expect(registry.get("src/app.ts")).toBe(session);
    expect(editor.cleanUp).not.toHaveBeenCalled();
    expect(session.getSnapshot()).toMatchObject({
      save: { phase: "failed", canSave: true },
      canUndo: true,
    });
    expect(onPendingChange).toHaveBeenLastCalledWith("src/app.ts", true);

    const reopened = registry.getOrCreate("src/app.ts", createSession);
    expect(reopened).toBe(session);
    expect(reopened.editor).toBe(editor);
    expect(createSession).toHaveBeenCalledOnce();
    await expect(reopened.flush()).resolves.toBe("failed");
    expect(persist).toHaveBeenLastCalledWith("src/app.ts", "retryable contents");
    expect(reopened.getSnapshot().save).toMatchObject({
      phase: "failed",
      canSave: true,
    });
  });

  it("keeps Save disabled when Undo edits during a deferred write or mutation pause", async () => {
    vi.useFakeTimers();
    const write = deferredResult<AtomCommandResult<void, never>>();
    const persist = vi
      .fn<() => Promise<AtomCommandResult<void, never>>>()
      .mockReturnValueOnce(write.promise)
      .mockResolvedValue(AsyncResult.success(undefined));
    const session = new FileEditingSession({
      cwd: "/repo",
      relativePath: "src/app.ts",
      debounceMs: 500,
      persist: (_relativePath, _contents) => persist(),
      onPendingChange: vi.fn(),
      onConfirmed: vi.fn(),
    });
    const editor = session.editor as unknown as InstanceType<typeof pierre.Editor>;
    const onChange = editor.options["onChange"] as (file: { contents: string }) => void;
    editor.canUndo = true;
    editor.undo.mockImplementation(() => onChange({ contents: "undo result" }));

    onChange({ contents: "first write" });
    await vi.advanceTimersByTimeAsync(500);
    session.undo();
    expect(session.getSnapshot().save).toMatchObject({
      phase: "saving",
      canSave: false,
    });

    write.resolve(AsyncResult.success(undefined));
    await vi.runAllTimersAsync();
    session.pauseSaving();
    session.undo();
    expect(session.getSnapshot().save).toMatchObject({
      phase: "pending",
      canSave: false,
    });

    session.resumeSaving();
    expect(session.getSnapshot().save).toMatchObject({
      phase: "pending",
      canSave: true,
    });
  });

  it("keeps autosaving after its editor change handler is cleared", async () => {
    vi.useFakeTimers();
    const persist = vi.fn(async () => AsyncResult.success(undefined));
    const handler = vi.fn();
    const session = new FileEditingSession({
      cwd: "/repo",
      relativePath: "src/app.ts",
      debounceMs: 500,
      persist,
      onPendingChange: vi.fn(),
      onConfirmed: vi.fn(),
    });
    const editor = session.editor as unknown as InstanceType<typeof pierre.Editor>;
    const onChange = editor.options["onChange"] as (file: { contents: string }) => void;

    session.setEditorChangeHandler(handler);
    onChange({ contents: "with handler" });

    expect(handler).toHaveBeenCalledWith({ contents: "with handler" }, undefined);
    expect(session.getSnapshot().save.phase).toBe("pending");

    session.setEditorChangeHandler(null);
    onChange({ contents: "without handler" });
    await vi.advanceTimersByTimeAsync(500);

    expect(handler).toHaveBeenCalledOnce();
    expect(persist).toHaveBeenCalledWith("src/app.ts", "without handler");
  });

  it("holds edits during a delayed rename and saves them to the renamed path after release", async () => {
    vi.useFakeTimers();
    const persist = vi.fn(async () => AsyncResult.success(undefined));
    const registry = new FileEditingSessionRegistry<FileEditingSession<never>>();
    const session = registry.getOrCreate(
      "src/old.ts",
      () =>
        new FileEditingSession({
          cwd: "/repo",
          relativePath: "src/old.ts",
          debounceMs: 500,
          persist,
          onPendingChange: vi.fn(),
          onConfirmed: vi.fn(),
        }),
    );
    const editor = session.editor as unknown as InstanceType<typeof pierre.Editor>;
    const command = deferred();
    const lease = await registry.beginPathMutation({
      kind: "rename",
      fromRelativePath: "src/old.ts",
      toRelativePath: "src/new.ts",
    });
    expect(lease).not.toBeNull();

    const mutation = command.promise.then(() => {
      lease!.commitRename("src/new.ts");
      lease!.release();
    });
    (editor.options["onChange"] as (file: { contents: string }) => void)({
      contents: "edited during rename",
    });
    await vi.advanceTimersByTimeAsync(500);
    expect(persist).not.toHaveBeenCalled();

    command.resolve();
    await mutation;
    await vi.advanceTimersByTimeAsync(499);
    expect(persist).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1);
    expect(persist).toHaveBeenCalledOnce();
    expect(persist).toHaveBeenCalledWith("src/new.ts", "edited during rename");
  });

  it("releases edits to the original path after a delayed rename fails", async () => {
    vi.useFakeTimers();
    const persist = vi.fn(async () => AsyncResult.success(undefined));
    const registry = new FileEditingSessionRegistry<FileEditingSession<never>>();
    const session = registry.getOrCreate(
      "src/old.ts",
      () =>
        new FileEditingSession({
          cwd: "/repo",
          relativePath: "src/old.ts",
          debounceMs: 500,
          persist,
          onPendingChange: vi.fn(),
          onConfirmed: vi.fn(),
        }),
    );
    const editor = session.editor as unknown as InstanceType<typeof pierre.Editor>;
    const command = deferred();
    const lease = await registry.beginPathMutation({
      kind: "rename",
      fromRelativePath: "src/old.ts",
      toRelativePath: "src/new.ts",
    });
    expect(lease).not.toBeNull();

    const mutation = command.promise.then(() => lease!.release());
    (editor.options["onChange"] as (file: { contents: string }) => void)({
      contents: "edited during failed rename",
    });
    await vi.advanceTimersByTimeAsync(500);
    expect(persist).not.toHaveBeenCalled();

    command.resolve();
    await mutation;
    await vi.advanceTimersByTimeAsync(500);
    expect(persist).toHaveBeenCalledWith("src/old.ts", "edited during failed rename");
  });

  it("discards destination edits after delayed rename success and contains cleanup failure", async () => {
    vi.useFakeTimers();
    const reportError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    try {
      const persist = vi.fn(async () => AsyncResult.success(undefined));
      const registry = new FileEditingSessionRegistry<FileEditingSession<never>>();
      const createSession = (relativePath: string) =>
        new FileEditingSession<never>({
          cwd: "/repo",
          relativePath,
          debounceMs: 500,
          persist,
          onPendingChange: vi.fn(),
          onConfirmed: vi.fn(),
        });
      const source = registry.getOrCreate("src/old.ts", () => createSession("src/old.ts"));
      const destination = registry.getOrCreate("src/new.ts", () => createSession("src/new.ts"));
      const sourceEditor = source.editor as unknown as InstanceType<typeof pierre.Editor>;
      const destinationEditor = destination.editor as unknown as InstanceType<typeof pierre.Editor>;
      destinationEditor.cleanUp.mockImplementation(() => {
        throw new Error("destination cleanup failed");
      });
      const command = deferredResult<"success">();
      const lease = await registry.beginPathMutation({
        kind: "rename",
        fromRelativePath: "src/old.ts",
        toRelativePath: "src/new.ts",
      });
      const mutation = command.promise
        .then(() => lease!.commitRename("src/new.ts"))
        .finally(() => lease!.release());

      (sourceEditor.options["onChange"] as (file: { contents: string }) => void)({
        contents: "source edit",
      });
      (destinationEditor.options["onChange"] as (file: { contents: string }) => void)({
        contents: "destination edit",
      });
      await vi.advanceTimersByTimeAsync(500);
      expect(persist).not.toHaveBeenCalled();

      command.resolve("success");
      await mutation;
      await vi.advanceTimersByTimeAsync(500);

      expect(persist).toHaveBeenCalledOnce();
      expect(persist).toHaveBeenCalledWith("src/new.ts", "source edit");
      expect(destinationEditor.cleanUp).toHaveBeenCalledOnce();
      expect(registry.get("src/new.ts")).toBe(source);
      expect(reportError).toHaveBeenCalledWith(
        "[file-editing-session-registry] session cleanup failed",
        expect.objectContaining({ message: "destination cleanup failed" }),
      );
    } finally {
      reportError.mockRestore();
    }
  });

  it("resumes source and destination edits unchanged after delayed rename failure", async () => {
    vi.useFakeTimers();
    const persist = vi.fn(async () => AsyncResult.success(undefined));
    const registry = new FileEditingSessionRegistry<FileEditingSession<never>>();
    const createSession = (relativePath: string) =>
      new FileEditingSession<never>({
        cwd: "/repo",
        relativePath,
        debounceMs: 500,
        persist,
        onPendingChange: vi.fn(),
        onConfirmed: vi.fn(),
      });
    const source = registry.getOrCreate("src/old.ts", () => createSession("src/old.ts"));
    const destination = registry.getOrCreate("src/new.ts", () => createSession("src/new.ts"));
    const sourceEditor = source.editor as unknown as InstanceType<typeof pierre.Editor>;
    const destinationEditor = destination.editor as unknown as InstanceType<typeof pierre.Editor>;
    const command = deferredResult<"failure">();
    const lease = await registry.beginPathMutation({
      kind: "rename",
      fromRelativePath: "src/old.ts",
      toRelativePath: "src/new.ts",
    });
    const mutation = command.promise.finally(() => lease!.release());

    (sourceEditor.options["onChange"] as (file: { contents: string }) => void)({
      contents: "source edit",
    });
    (destinationEditor.options["onChange"] as (file: { contents: string }) => void)({
      contents: "destination edit",
    });
    await vi.advanceTimersByTimeAsync(500);
    expect(persist).not.toHaveBeenCalled();

    command.resolve("failure");
    await mutation;
    await vi.advanceTimersByTimeAsync(500);

    expect(persist.mock.calls).toEqual([
      ["src/old.ts", "source edit"],
      ["src/new.ts", "destination edit"],
    ]);
    expect(registry.get("src/old.ts")).toBe(source);
    expect(registry.get("src/new.ts")).toBe(destination);
  });

  it("resumes source and destination edits unchanged after a delayed rename throw", async () => {
    vi.useFakeTimers();
    const persist = vi.fn(async () => AsyncResult.success(undefined));
    const registry = new FileEditingSessionRegistry<FileEditingSession<never>>();
    const createSession = (relativePath: string) =>
      new FileEditingSession<never>({
        cwd: "/repo",
        relativePath,
        debounceMs: 500,
        persist,
        onPendingChange: vi.fn(),
        onConfirmed: vi.fn(),
      });
    const source = registry.getOrCreate("src/old.ts", () => createSession("src/old.ts"));
    const destination = registry.getOrCreate("src/new.ts", () => createSession("src/new.ts"));
    const sourceEditor = source.editor as unknown as InstanceType<typeof pierre.Editor>;
    const destinationEditor = destination.editor as unknown as InstanceType<typeof pierre.Editor>;
    const command = deferredResult<never>();
    const lease = await registry.beginPathMutation({
      kind: "rename",
      fromRelativePath: "src/old.ts",
      toRelativePath: "src/new.ts",
    });
    const mutation = command.promise.catch(() => undefined).finally(() => lease!.release());

    (sourceEditor.options["onChange"] as (file: { contents: string }) => void)({
      contents: "source edit",
    });
    (destinationEditor.options["onChange"] as (file: { contents: string }) => void)({
      contents: "destination edit",
    });
    await vi.advanceTimersByTimeAsync(500);
    expect(persist).not.toHaveBeenCalled();

    command.reject(new Error("rename command threw"));
    await mutation;
    await vi.advanceTimersByTimeAsync(500);

    expect(persist.mock.calls).toEqual([
      ["src/old.ts", "source edit"],
      ["src/new.ts", "destination edit"],
    ]);
    expect(registry.get("src/old.ts")).toBe(source);
    expect(registry.get("src/new.ts")).toBe(destination);
  });

  it("discards edits made during a delayed delete", async () => {
    vi.useFakeTimers();
    const persist = vi.fn(async () => AsyncResult.success(undefined));
    const registry = new FileEditingSessionRegistry<FileEditingSession<never>>();
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
    const editor = session.editor as unknown as InstanceType<typeof pierre.Editor>;
    const command = deferred();
    const lease = await registry.beginPathMutation({
      kind: "delete",
      relativePath: "src/app.ts",
    });
    expect(lease).not.toBeNull();

    const mutation = command.promise.then(() => {
      lease!.commitDelete();
      lease!.release();
    });
    (editor.options["onChange"] as (file: { contents: string }) => void)({
      contents: "must be discarded",
    });
    await vi.advanceTimersByTimeAsync(500);
    expect(persist).not.toHaveBeenCalled();

    command.resolve();
    await mutation;
    await vi.runAllTimersAsync();
    expect(persist).not.toHaveBeenCalled();
    expect(registry.get("src/app.ts")).toBeUndefined();
    expect(editor.cleanUp).toHaveBeenCalledOnce();
  });

  it("does not pause autosaves for sessions outside the mutation path", async () => {
    vi.useFakeTimers();
    const persist = vi.fn(async () => AsyncResult.success(undefined));
    const registry = new FileEditingSessionRegistry<FileEditingSession<never>>();
    const createSession = (relativePath: string) =>
      new FileEditingSession<never>({
        cwd: "/repo",
        relativePath,
        debounceMs: 500,
        persist,
        onPendingChange: vi.fn(),
        onConfirmed: vi.fn(),
      });
    const affected = registry.getOrCreate("src/app.ts", () => createSession("src/app.ts"));
    const unrelated = registry.getOrCreate("docs/readme.md", () => createSession("docs/readme.md"));
    const lease = await registry.beginPathMutation({
      kind: "delete",
      relativePath: "src",
    });
    expect(lease).not.toBeNull();

    const affectedEditor = affected.editor as unknown as InstanceType<typeof pierre.Editor>;
    const unrelatedEditor = unrelated.editor as unknown as InstanceType<typeof pierre.Editor>;
    (affectedEditor.options["onChange"] as (file: { contents: string }) => void)({
      contents: "affected",
    });
    (unrelatedEditor.options["onChange"] as (file: { contents: string }) => void)({
      contents: "unrelated",
    });
    await vi.advanceTimersByTimeAsync(500);

    expect(persist).toHaveBeenCalledOnce();
    expect(persist).toHaveBeenCalledWith("docs/readme.md", "unrelated");
    lease!.release();
  });
});
