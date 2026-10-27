import { EnvironmentId, ThreadId } from "@coddy/contracts";
import { AsyncResult, Atom, AtomRegistry } from "effect/reactivity";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";

const harness = vi.hoisted(() => ({
  capturedAtom: null as unknown,
  sessionsAtoms: [] as unknown[],
  eventsAtoms: [] as unknown[],
  sessionsInitial: null as unknown,
  eventsInitial: null as unknown,
  runAtomCommand: vi.fn(),
  reconcileSessions: vi.fn(),
  applySnapshot: vi.fn(),
  applyEvent: vi.fn(),
  readState: vi.fn(),
  releasePreviewTab: vi.fn(),
  openCommand: { label: "preview-open" },
}));

vi.mock("~/browser/previewGateway", () => ({ releasePreviewTab: harness.releasePreviewTab }));

vi.mock("@effect/atom-react", () => ({
  useAtomValue: (atom: unknown) => {
    harness.capturedAtom = atom;
  },
}));

vi.mock("@coddy/client-runtime/state/runtime", () => ({
  runAtomCommand: harness.runAtomCommand,
}));

vi.mock("~/previewStateStore", () => ({
  applyPreviewServerEvent: harness.applyEvent,
  applyPreviewServerSnapshot: harness.applySnapshot,
  readThreadPreviewState: harness.readState,
  reconcilePreviewServerSessions: harness.reconcileSessions,
}));

vi.mock("~/state/preview", () => ({
  previewEnvironment: {
    list: () => {
      const atom = Atom.make(harness.sessionsInitial);
      harness.sessionsAtoms.push(atom);
      return atom;
    },
    events: () => {
      const atom = Atom.make(harness.eventsInitial);
      harness.eventsAtoms.push(atom);
      return atom;
    },
    open: harness.openCommand,
  },
}));

import { usePreviewSession } from "./usePreviewSession";

const environmentId = EnvironmentId.make("env-preview");

function threadRef(id: string) {
  return { environmentId, threadId: ThreadId.make(id) };
}

function sessions(value: ReadonlyArray<Record<string, unknown>>) {
  return AsyncResult.success({ sessions: value });
}

function event(threadId: string, type: string = "navigated") {
  return AsyncResult.success({ threadId: ThreadId.make(threadId), type });
}

function captureSyncAtom(id: string) {
  usePreviewSession(threadRef(id));
  if (!harness.capturedAtom) throw new Error("Preview sync atom was not captured");
  return harness.capturedAtom as Atom.Atom<unknown>;
}

beforeEach(() => {
  harness.capturedAtom = null;
  harness.sessionsAtoms.length = 0;
  harness.eventsAtoms.length = 0;
  harness.sessionsInitial = AsyncResult.initial(false);
  harness.eventsInitial = AsyncResult.initial(false);
  harness.runAtomCommand.mockReset();
  harness.runAtomCommand.mockResolvedValue({ _tag: "Success", value: { id: "opened" } });
  harness.reconcileSessions.mockReset();
  harness.applySnapshot.mockReset();
  harness.applyEvent.mockReset();
  harness.releasePreviewTab.mockReset();
  harness.readState.mockReset();
  harness.readState.mockReturnValue({ snapshot: null });
});

describe("usePreviewSession", () => {
  it("reconciles initial sessions and applies a matching opened event", async () => {
    harness.sessionsInitial = sessions([{ id: "session-1" }]);
    harness.eventsInitial = event("thread-live", "opened");
    const atom = captureSyncAtom("thread-live");
    const registry = AtomRegistry.make();

    registry.mount(atom);
    await vi.waitFor(() => expect(harness.reconcileSessions).toHaveBeenCalledTimes(1));
    expect(harness.reconcileSessions).toHaveBeenCalledWith(threadRef("thread-live"), [
      { id: "session-1" },
    ]);
    expect(harness.applyEvent).toHaveBeenCalledWith(
      threadRef("thread-live"),
      expect.objectContaining({ type: "opened" }),
    );
    expect(harness.runAtomCommand).not.toHaveBeenCalled();
    registry.dispose();
  });

  it("clears server state when no live or recoverable session exists", async () => {
    harness.sessionsInitial = sessions([]);
    const atom = captureSyncAtom("thread-empty");
    const registry = AtomRegistry.make();

    registry.mount(atom);
    await vi.waitFor(() => expect(harness.applySnapshot).toHaveBeenCalledTimes(1));
    expect(harness.applySnapshot).toHaveBeenCalledWith(threadRef("thread-empty"), null);
    expect(harness.runAtomCommand).not.toHaveBeenCalled();
    registry.dispose();
  });

  it("reopens a recoverable local session and refreshes the session list", async () => {
    harness.sessionsInitial = sessions([]);
    harness.readState.mockReturnValue({
      snapshot: { navStatus: { _tag: "Success", url: "https://example.test" } },
    });
    const atom = captureSyncAtom("thread-recover");
    const registry = AtomRegistry.make();

    registry.mount(atom);
    await vi.waitFor(() => expect(harness.runAtomCommand).toHaveBeenCalledTimes(1));
    expect(harness.runAtomCommand).toHaveBeenCalledWith(
      registry,
      harness.openCommand,
      {
        environmentId,
        input: { threadId: ThreadId.make("thread-recover"), url: "https://example.test" },
      },
      { reportDefect: false, reportFailure: false },
    );
    await vi.waitFor(() =>
      expect(harness.applySnapshot).toHaveBeenCalledWith(threadRef("thread-recover"), {
        id: "opened",
      }),
    );
    expect(harness.runAtomCommand).toHaveBeenCalledTimes(1);
    registry.dispose();
  });

  it("does not duplicate an in-flight recovery and ignores a failed open", async () => {
    let resolveOpen!: (value: unknown) => void;
    harness.sessionsInitial = sessions([]);
    harness.readState.mockReturnValue({
      snapshot: { navStatus: { _tag: "Loading", url: "https://pending.test" } },
    });
    harness.runAtomCommand.mockReturnValue(
      new Promise((resolve) => {
        resolveOpen = resolve;
      }),
    );
    const atom = captureSyncAtom("thread-pending");
    const registry = AtomRegistry.make();
    registry.mount(atom);
    await vi.waitFor(() => expect(harness.runAtomCommand).toHaveBeenCalledTimes(1));

    const sessionsAtom = harness.sessionsAtoms[0] as Atom.Writable<unknown>;
    registry.set(sessionsAtom, sessions([]));
    expect(harness.runAtomCommand).toHaveBeenCalledTimes(1);
    resolveOpen({ _tag: "Failure" });
    await Promise.resolve();
    expect(harness.applySnapshot).not.toHaveBeenCalled();
    registry.dispose();
  });

  it("invalidates an in-flight recovery when live sessions arrive", async () => {
    let resolveOpen!: (value: unknown) => void;
    harness.sessionsInitial = sessions([]);
    harness.readState.mockReturnValue({
      snapshot: { navStatus: { _tag: "Success", url: "https://stale.test" } },
    });
    harness.runAtomCommand.mockReturnValue(
      new Promise((resolve) => {
        resolveOpen = resolve;
      }),
    );
    const atom = captureSyncAtom("thread-stale");
    const registry = AtomRegistry.make();
    registry.mount(atom);
    await vi.waitFor(() => expect(harness.runAtomCommand).toHaveBeenCalledTimes(1));

    const sessionsAtom = harness.sessionsAtoms[0] as Atom.Writable<unknown>;
    registry.set(sessionsAtom, sessions([{ id: "live" }]));
    expect(harness.reconcileSessions).toHaveBeenCalledWith(threadRef("thread-stale"), [
      { id: "live" },
    ]);
    resolveOpen({ _tag: "Success", value: { id: "stale-open" } });
    await Promise.resolve();
    expect(harness.applySnapshot).not.toHaveBeenCalled();
    registry.dispose();
  });

  it("ignores unrelated and unsuccessful events but refreshes for close events", async () => {
    harness.sessionsInitial = AsyncResult.initial(false);
    harness.eventsInitial = event("other-thread");
    const atom = captureSyncAtom("thread-events");
    const registry = AtomRegistry.make();
    registry.mount(atom);
    await Promise.resolve();
    expect(harness.applyEvent).not.toHaveBeenCalled();

    const eventsAtom = harness.eventsAtoms[0] as Atom.Writable<unknown>;
    registry.set(eventsAtom, AsyncResult.initial(false));
    registry.set(eventsAtom, event("thread-events", "navigated"));
    registry.set(eventsAtom, event("thread-events", "closed"));
    await vi.waitFor(() => expect(harness.applyEvent).toHaveBeenCalledTimes(2));
    expect(harness.applyEvent.mock.calls.map(([, value]) => value.type)).toEqual([
      "navigated",
      "closed",
    ]);
    registry.dispose();
  });

  it("releases a closed tab's SSH forwards at once", async () => {
    harness.eventsInitial = AsyncResult.initial(false);
    const atom = captureSyncAtom("thread-close");
    const registry = AtomRegistry.make();
    registry.mount(atom);
    await Promise.resolve();

    const eventsAtom = harness.eventsAtoms[0] as Atom.Writable<unknown>;
    registry.set(
      eventsAtom,
      AsyncResult.success({
        threadId: ThreadId.make("thread-close"),
        type: "navigated",
        tabId: "t",
      }),
    );
    registry.set(
      eventsAtom,
      AsyncResult.success({ threadId: ThreadId.make("thread-close"), type: "closed", tabId: "t" }),
    );
    await vi.waitFor(() => expect(harness.releasePreviewTab).toHaveBeenCalledTimes(1));
    expect(harness.releasePreviewTab).toHaveBeenCalledWith(environmentId, "t");
    registry.dispose();
  });

  it("does not apply a recovery that resolves after disposal", async () => {
    let resolveOpen!: (value: unknown) => void;
    harness.sessionsInitial = sessions([]);
    harness.readState.mockReturnValue({
      snapshot: { navStatus: { _tag: "Success", url: "https://disposed.test" } },
    });
    harness.runAtomCommand.mockReturnValue(
      new Promise((resolve) => {
        resolveOpen = resolve;
      }),
    );
    const atom = captureSyncAtom("thread-disposed");
    const registry = AtomRegistry.make();
    registry.mount(atom);
    await vi.waitFor(() => expect(harness.runAtomCommand).toHaveBeenCalledTimes(1));

    registry.dispose();
    resolveOpen({ _tag: "Success", value: { id: "late" } });
    await Promise.resolve();
    expect(harness.applySnapshot).not.toHaveBeenCalled();
  });
});
