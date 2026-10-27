import { scopedThreadKey, scopeThreadRef } from "@coddy/client-runtime/environment";
import { EnvironmentId, ThreadId, type TerminalSummary } from "@coddy/contracts";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";

import {
  applyPanelThreadAdoption,
  applyTerminalMetadataAdoption,
  createCenterPanelAdoptionTracker,
} from "./centerPanelAdoption";
import {
  HOST_SURFACE_ID,
  selectThreadCenterPanelState,
  useCenterPanelStore,
} from "./centerPanelStore";
import { reserveTerminalId } from "./terminalIdReservations";

const release = vi.hoisted(() => vi.fn());
vi.mock("./components/ThreadTerminalPanel", () => ({ releaseTerminalUiResources: release }));

const ENV = EnvironmentId.make("environment-1");
const HOST_ID = ThreadId.make("host-1");
const HOST = scopeThreadRef(ENV, HOST_ID);
const CLAUDE = { executable: "claude", args: [], label: "Claude" };

const summary = (
  terminalId: string,
  overrides: Partial<TerminalSummary> = {},
): TerminalSummary => ({
  threadId: HOST_ID,
  terminalId,
  cwd: "/repo",
  worktreePath: null,
  status: "running",
  pid: 1,
  exitCode: null,
  exitSignal: null,
  hasRunningSubprocess: false,
  label: terminalId,
  updatedAt: "2026-10-08T00:00:00.000Z",
  centerPanel: true,
  command: CLAUDE,
  ...overrides,
});

const panelThread = (id: string, hostThreadId: string | undefined = HOST_ID) => ({
  id: ThreadId.make(id),
  kind: "panel" as const,
  ...(hostThreadId !== undefined ? { hostThreadId: ThreadId.make(hostThreadId) } : {}),
  title: "Panel — Claude",
  archivedAt: null,
});
const hostThread = { id: HOST_ID, kind: "workspace" as const, title: "Host", archivedAt: null };

const hostState = () =>
  selectThreadCenterPanelState(useCenterPanelStore.getState().byThreadKey, HOST);
const surfaceIds = () => hostState().surfaces.map((surface) => surface.id);
const activeSurfaceId = () => hostState().groups[0]?.activeSurfaceId;

describe("center panel adoption", () => {
  beforeEach(() => {
    useCenterPanelStore.setState({ byThreadKey: {}, pendingChatPanelThreadKeys: new Set() });
    release.mockClear();
  });

  it("adopts a center terminal another client opened as an inactive tab with its command", () => {
    const tracker = createCenterPanelAdoptionTracker();
    applyTerminalMetadataAdoption(ENV, tracker, { type: "snapshot", terminals: [] });
    applyTerminalMetadataAdoption(ENV, tracker, { type: "upsert", terminal: summary("term-2") });

    expect(surfaceIds()).toEqual([HOST_SURFACE_ID, "terminal:term-2"]);
    expect(activeSurfaceId()).toBe(HOST_SURFACE_ID);
    expect(hostState().surfaces[1]).toMatchObject({ label: "Claude", command: CLAUDE });
  });

  it("adopts center terminals from the first snapshot and ignores drawer shells", () => {
    const tracker = createCenterPanelAdoptionTracker();
    applyTerminalMetadataAdoption(ENV, tracker, {
      type: "snapshot",
      terminals: [summary("term-1", { centerPanel: false, command: undefined }), summary("term-2")],
    });

    expect(surfaceIds()).toEqual([HOST_SURFACE_ID, "terminal:term-2"]);
  });

  it("does not re-add a terminal this client closed when later updates arrive", () => {
    const tracker = createCenterPanelAdoptionTracker();
    applyTerminalMetadataAdoption(ENV, tracker, { type: "upsert", terminal: summary("term-2") });
    useCenterPanelStore.getState().closeAllSurfaces(HOST, hostState().groups[0]!.id);
    applyTerminalMetadataAdoption(ENV, tracker, {
      type: "upsert",
      terminal: summary("term-2", { status: "exited" }),
    });

    expect(surfaceIds()).not.toContain("terminal:term-2");
  });

  it("leaves terminals reserved by a local open to that open", () => {
    const tracker = createCenterPanelAdoptionTracker();
    const reservation = reserveTerminalId(HOST, []);
    try {
      applyTerminalMetadataAdoption(ENV, tracker, {
        type: "upsert",
        terminal: summary(reservation.terminalId),
      });
    } finally {
      reservation.release();
    }

    expect(surfaceIds()).toEqual([HOST_SURFACE_ID]);
  });

  it("closes the tab when the server removes the session", () => {
    const tracker = createCenterPanelAdoptionTracker();
    applyTerminalMetadataAdoption(ENV, tracker, { type: "upsert", terminal: summary("term-2") });
    applyTerminalMetadataAdoption(ENV, tracker, {
      type: "remove",
      threadId: HOST_ID,
      terminalId: "term-2",
    });

    expect(surfaceIds()).toEqual([HOST_SURFACE_ID]);
  });

  it("releases this client's retained terminal input when another client closes it", async () => {
    const tracker = createCenterPanelAdoptionTracker();
    applyTerminalMetadataAdoption(ENV, tracker, { type: "upsert", terminal: summary("term-2") });
    applyTerminalMetadataAdoption(ENV, tracker, {
      type: "remove",
      threadId: HOST_ID,
      terminalId: "term-2",
      reason: "closed",
    });

    await vi.waitFor(() => expect(release).toHaveBeenCalledWith(ENV, HOST_ID, "term-2"));
  });

  it("keeps the tab when the server shuts down or a restart fails", async () => {
    const tracker = createCenterPanelAdoptionTracker();
    applyTerminalMetadataAdoption(ENV, tracker, { type: "upsert", terminal: summary("term-2") });
    for (const reason of ["shutdown", "restarted"] as const) {
      applyTerminalMetadataAdoption(ENV, tracker, {
        type: "remove",
        threadId: HOST_ID,
        terminalId: "term-2",
        reason,
      });
    }
    applyTerminalMetadataAdoption(ENV, tracker, { type: "upsert", terminal: summary("term-2") });

    expect(surfaceIds()).toEqual([HOST_SURFACE_ID, "terminal:term-2"]);
    await Promise.resolve();
    expect(release).not.toHaveBeenCalled();
  });

  it("follows the command the session runs on tabs this client still shows", () => {
    const tracker = createCenterPanelAdoptionTracker();
    const codex = { executable: "codex", args: [], label: "Codex" };
    applyTerminalMetadataAdoption(ENV, tracker, { type: "upsert", terminal: summary("term-2") });
    applyTerminalMetadataAdoption(ENV, tracker, {
      type: "upsert",
      terminal: summary("term-2", { command: codex }),
    });
    expect(hostState().surfaces[1]).toMatchObject({ label: "Codex", command: codex });

    applyTerminalMetadataAdoption(ENV, tracker, {
      type: "upsert",
      terminal: summary("term-2", { command: undefined }),
    });
    expect(hostState().surfaces[1]).not.toHaveProperty("command");
    expect(hostState().surfaces[1]).not.toHaveProperty("label");
  });

  it("keeps a custom tab label when the command changes", () => {
    const tracker = createCenterPanelAdoptionTracker();
    useCenterPanelStore
      .getState()
      .openTerminalPanel(HOST, "term-5", { label: "My agent", command: CLAUDE });
    applyTerminalMetadataAdoption(ENV, tracker, {
      type: "upsert",
      terminal: summary("term-5", { command: { executable: "codex", args: [], label: "Codex" } }),
    });

    expect(hostState().surfaces.at(-1)).toMatchObject({ label: "My agent" });
  });

  it("does not touch the persisted layout when the command is unchanged", () => {
    const tracker = createCenterPanelAdoptionTracker();
    applyTerminalMetadataAdoption(ENV, tracker, { type: "upsert", terminal: summary("term-2") });
    const layout = useCenterPanelStore.getState().byThreadKey;
    const original = useCenterPanelStore.persist.getOptions().storage;
    const setItem = vi.fn();
    useCenterPanelStore.persist.setOptions({
      storage: { getItem: () => null, setItem, removeItem: () => undefined },
    });
    try {
      for (let index = 0; index < 5; index += 1) {
        applyTerminalMetadataAdoption(ENV, tracker, {
          type: "upsert",
          terminal: summary("term-2"),
        });
      }
    } finally {
      useCenterPanelStore.persist.setOptions({ storage: original });
    }

    expect(setItem).not.toHaveBeenCalled();
    expect(useCenterPanelStore.getState().byThreadKey).toBe(layout);
  });

  it("keeps the tab when a reconnect snapshot omits the session", () => {
    const tracker = createCenterPanelAdoptionTracker();
    applyTerminalMetadataAdoption(ENV, tracker, { type: "upsert", terminal: summary("term-2") });
    applyTerminalMetadataAdoption(ENV, tracker, { type: "snapshot", terminals: [] });

    expect(surfaceIds()).toContain("terminal:term-2");
  });

  it("adopts a chat panel under its live host thread as an inactive tab", () => {
    const tracker = createCenterPanelAdoptionTracker();
    applyPanelThreadAdoption(ENV, tracker, [hostThread, panelThread("panel-a")]);

    expect(surfaceIds()).toEqual([HOST_SURFACE_ID, "chat:panel-a"]);
    expect(hostState().surfaces[1]).toMatchObject({ providerLabel: "Claude" });
    expect(activeSurfaceId()).toBe(HOST_SURFACE_ID);
  });

  it("skips panels without a live host, pending local panels, and panels already seen", () => {
    const tracker = createCenterPanelAdoptionTracker();
    useCenterPanelStore.setState({
      pendingChatPanelThreadKeys: new Set([
        scopedThreadKey(scopeThreadRef(ENV, ThreadId.make("panel-pending"))),
      ]),
    });
    applyPanelThreadAdoption(ENV, tracker, [
      hostThread,
      panelThread("panel-orphan", "missing-host"),
      panelThread("panel-legacy", undefined),
      panelThread("panel-pending"),
      panelThread("panel-a"),
    ]);
    useCenterPanelStore.getState().closeAllSurfaces(HOST, hostState().groups[0]!.id);
    applyPanelThreadAdoption(ENV, tracker, [hostThread, panelThread("panel-a")]);

    expect(surfaceIds()).not.toContain("chat:panel-a");
    expect(useCenterPanelStore.getState().byThreadKey).not.toHaveProperty(
      scopedThreadKey(scopeThreadRef(ENV, ThreadId.make("missing-host"))),
    );
  });
  it("drops a tracked panel's tab when it leaves the live list and re-adopts it on return", () => {
    const tracker = createCenterPanelAdoptionTracker();
    applyPanelThreadAdoption(ENV, tracker, [
      hostThread,
      panelThread("panel-a"),
      panelThread("panel-b"),
    ]);
    expect(surfaceIds()).toEqual([HOST_SURFACE_ID, "chat:panel-a", "chat:panel-b"]);
    useCenterPanelStore.setState({
      pendingChatPanelThreadKeys: new Set([
        scopedThreadKey(scopeThreadRef(ENV, ThreadId.make("panel-b"))),
      ]),
    });

    // Archived or deleted elsewhere; a panel still pending locally keeps its tab.
    applyPanelThreadAdoption(ENV, tracker, [hostThread]);
    expect(surfaceIds()).toEqual([HOST_SURFACE_ID, "chat:panel-b"]);

    // Reopened elsewhere: adopted again as an inactive tab.
    applyPanelThreadAdoption(ENV, tracker, [hostThread, panelThread("panel-a")]);
    expect(surfaceIds()).toEqual([HOST_SURFACE_ID, "chat:panel-b", "chat:panel-a"]);
    expect(activeSurfaceId()).toBe(HOST_SURFACE_ID);
  });
});
