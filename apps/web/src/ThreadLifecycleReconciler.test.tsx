// @vitest-environment happy-dom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

interface TestThread {
  readonly id: string;
  readonly kind: "default" | "panel";
}

interface TestSnapshot {
  readonly snapshotSequence: number;
  readonly threads: ReadonlyArray<TestThread>;
}

const h = vi.hoisted(() => ({
  shell: { status: "live", snapshot: { _tag: "None" } } as {
    status: string;
    snapshot: { readonly _tag: "Some"; readonly value: unknown } | { readonly _tag: "None" };
  },
  archived: {
    snapshots: [] as ReadonlyArray<{ environmentId: string; snapshot: unknown }>,
    error: null as string | null,
    isLoading: false,
  },
  // Server sequence at which each archived request still in flight started.
  inFlight: [] as number[],
  maxInFlight: 0,
  serverSequence: 0,
  refresh: vi.fn(),
  centerByThreadKey: {} as Record<string, { surfaces: [] }>,
  removeCenterThread: vi.fn<(ref: { environmentId: string; threadId: string }) => void>(),
  removeRightThread: vi.fn(),
  emptyDrafts: {},
}));

vi.mock("@effect/atom-react", () => ({
  useAtomValue: () => h.shell,
  useAtomSubscribe: () => undefined,
}));
vi.mock("./state/shell", () => ({
  environmentShell: { stateValueAtom: () => "shell-atom" },
}));
vi.mock("./state/terminal", () => ({
  terminalEnvironment: { metadataEvents: () => "terminal-metadata-atom" },
}));
vi.mock("./state/environments", () => ({
  useEnvironments: () => ({ environments: [{ environmentId: "primary" }] }),
}));
vi.mock("./centerPanelAdoption", () => ({
  applyPanelThreadAdoption: () => undefined,
  applyTerminalMetadataAdoption: () => undefined,
  createCenterPanelAdoptionTracker: () => ({}),
}));
vi.mock("./centerPanelStore", () => ({
  useCenterPanelStore: {
    getState: () => ({
      byThreadKey: h.centerByThreadKey,
      pendingChatPanelThreadKeys: new Set<string>(),
      releaseChatPanelReservation: () => undefined,
      removeThread: h.removeCenterThread,
    }),
  },
}));
vi.mock("./rightPanelStore", () => ({
  useRightPanelStore: {
    getState: () => ({ byThreadKey: {}, removeThread: h.removeRightThread }),
  },
}));
vi.mock("./composerDraftStore", () => ({
  useComposerDraftStore: (select: (state: { draftThreadsByThreadKey: object }) => unknown) =>
    select({ draftThreadsByThreadKey: h.emptyDrafts }),
}));
vi.mock("./lib/archivedThreadsState", () => ({
  useArchivedThreadSnapshots: () => ({ ...h.archived, refresh: h.refresh }),
}));

import { ThreadLifecycleReconciler } from "./ThreadLifecycleReconciler";

let container: HTMLDivElement;
let root: Root;

function thread(id: string, kind: TestThread["kind"] = "default"): TestThread {
  return { id, kind };
}

function render(): void {
  act(() => {
    root.render(<ThreadLifecycleReconciler />);
  });
}

function setLive(snapshotSequence: number, threads: ReadonlyArray<TestThread>): void {
  const snapshot: TestSnapshot = { snapshotSequence, threads };
  h.shell = { status: "live", snapshot: { _tag: "Some", value: snapshot } };
}

/** Starts an archived request: what the archived atom does on mount and on refresh. */
function startArchivedRequest(): void {
  h.inFlight.push(h.serverSequence);
  h.maxInFlight = Math.max(h.maxInFlight, h.inFlight.length);
  h.archived = { ...h.archived, isLoading: true };
}

/** Mounts with the live shell at sequence 1 and the archived atom's first request running. */
function mount(threads: ReadonlyArray<TestThread>): void {
  h.serverSequence = 1;
  setLive(1, threads);
  startArchivedRequest();
  render();
}

/** Commits a server event and delivers the live shell snapshot it produces. */
function advanceLive(threads: ReadonlyArray<TestThread>): void {
  h.serverSequence += 1;
  setLive(h.serverSequence, threads);
  render();
}

/** Completes the oldest archived request with the archived threads at its start. */
function settleArchived(threads: ReadonlyArray<TestThread>): void {
  const startedAt = h.inFlight.shift();
  if (startedAt === undefined) throw new Error("no archived request in flight");
  const snapshot: TestSnapshot = { snapshotSequence: startedAt, threads };
  h.archived = {
    snapshots: [{ environmentId: "primary", snapshot }],
    error: null,
    isLoading: h.inFlight.length > 0,
  };
  render();
}

function removedThreadIds(): string[] {
  return h.removeCenterThread.mock.calls.map(([ref]) => ref.threadId);
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  h.shell = { status: "live", snapshot: { _tag: "None" } };
  h.archived = { snapshots: [], error: null, isLoading: false };
  h.inFlight = [];
  h.maxInFlight = 0;
  h.serverSequence = 0;
  h.centerByThreadKey = {};
  h.refresh.mockReset();
  h.refresh.mockImplementation(startArchivedRequest);
  h.removeCenterThread.mockReset();
  h.removeCenterThread.mockImplementation(({ environmentId, threadId }) => {
    delete h.centerByThreadKey[`${environmentId}:${threadId}`];
  });
  h.removeRightThread.mockReset();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => {
    root.unmount();
  });
  container.remove();
});

describe("ThreadLifecycleReconciler", () => {
  it("does not refetch archived threads while a turn advances the live shell", () => {
    const live = [thread("a"), thread("b")];
    mount(live);
    for (let event = 0; event < 20; event += 1) {
      advanceLive(live);
      if (h.inFlight.length > 0) settleArchived([]);
    }

    expect(h.refresh).not.toHaveBeenCalled();
    expect(h.maxInFlight).toBe(1);
  });

  it("refetches once per lifecycle change and never overlaps archived requests", () => {
    h.centerByThreadKey = {
      "primary:a": { surfaces: [] },
      "primary:b": { surfaces: [] },
      "primary:gone": { surfaces: [] },
    };
    mount([thread("a"), thread("b")]);
    settleArchived([]);
    // Authoritative: the thread missing from both lists loses its state.
    expect(removedThreadIds()).toEqual(["gone"]);
    h.removeCenterThread.mockClear();

    // Thread b is archived: it leaves the live list.
    advanceLive([thread("a")]);
    expect(h.refresh).toHaveBeenCalledTimes(1);
    // The turn keeps streaming while the archived request runs.
    advanceLive([thread("a")]);
    advanceLive([thread("a")]);
    expect(h.refresh).toHaveBeenCalledTimes(1);
    // No archived snapshot has seen b's archive yet, so b's state must survive.
    expect(h.removeCenterThread).not.toHaveBeenCalled();

    settleArchived([thread("b")]);
    advanceLive([thread("a")]);
    advanceLive([thread("a")]);
    expect(h.refresh).toHaveBeenCalledTimes(1);
    expect(h.removeCenterThread).not.toHaveBeenCalled();
    expect(h.maxInFlight).toBe(1);
  });

  it("refetches when a lifecycle change lands during an archived request", () => {
    h.centerByThreadKey = { "primary:a": { surfaces: [] }, "primary:b": { surfaces: [] } };
    mount([thread("a"), thread("b")]);
    // Thread b is archived while the mount request is still running.
    advanceLive([thread("a")]);
    expect(h.refresh).not.toHaveBeenCalled();

    // The mount request predates the archive, so it cannot decide b's fate.
    settleArchived([]);
    expect(h.removeCenterThread).not.toHaveBeenCalled();
    expect(h.refresh).toHaveBeenCalledTimes(1);

    settleArchived([thread("b")]);
    expect(h.removeCenterThread).not.toHaveBeenCalled();
    expect(h.refresh).toHaveBeenCalledTimes(1);
    expect(h.maxInFlight).toBe(1);
  });

  it("waits for the live shell to catch up with a newer archived snapshot", () => {
    h.centerByThreadKey = { "primary:a": { surfaces: [] }, "primary:c": { surfaces: [] } };
    // Thread c is unarchived at sequence 2; the archived request sees it, the live shell not yet.
    h.serverSequence = 2;
    setLive(1, [thread("a")]);
    startArchivedRequest();
    render();
    settleArchived([]);
    expect(h.removeCenterThread).not.toHaveBeenCalled();

    setLive(2, [thread("a"), thread("c")]);
    render();
    expect(h.removeCenterThread).not.toHaveBeenCalled();
    expect(h.refresh).not.toHaveBeenCalled();
  });
});
