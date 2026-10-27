import { scopedThreadKey, scopeThreadRef } from "@coddy/client-runtime/environment";
import {
  EnvironmentId,
  ProviderDriverKind,
  ProviderInstanceId,
  ThreadId,
} from "@coddy/contracts";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import { CENTER_PANEL_ROOT_GROUP_ID } from "./centerPanelLayout";
import {
  HOST_SURFACE_ID,
  migratePersistedCenterPanelState,
  selectActiveCenterSurface,
  selectFocusedCenterPanelGroup,
  selectFocusedCenterSurface,
  selectThreadCenterPanelState,
  selectVisibleCenterSurfaces,
  useCenterPanelStore,
} from "./centerPanelStore";
import { reserveTerminalId } from "./terminalIdReservations";

const HOST = scopeThreadRef(EnvironmentId.make("environment-1"), ThreadId.make("host-1"));
const PANEL_A = ThreadId.make("panel-a");
const PANEL_B = ThreadId.make("panel-b");

const store = () => useCenterPanelStore.getState();
const stateOf = (ref = HOST) => selectThreadCenterPanelState(store().byThreadKey, ref);
const surfaceIds = (ref = HOST) => stateOf(ref).surfaces.map((surface) => surface.id);
const rootGroup = () => stateOf().groups.find((group) => group.id === CENTER_PANEL_ROOT_GROUP_ID)!;

describe("centerPanelStore", () => {
  beforeEach(() =>
    useCenterPanelStore.setState({ byThreadKey: {}, pendingChatPanelThreadKeys: new Set() }),
  );
  afterEach(() => vi.restoreAllMocks());

  describe("default / host surface", () => {
    it("returns a root host group for an unknown thread", () => {
      expect(stateOf()).toEqual({
        surfaces: [{ id: HOST_SURFACE_ID, kind: "chat-host" }],
        groups: [
          {
            id: CENTER_PANEL_ROOT_GROUP_ID,
            surfaceIds: [HOST_SURFACE_ID],
            activeSurfaceId: HOST_SURFACE_ID,
          },
        ],
        layout: { type: "leaf", groupId: CENTER_PANEL_ROOT_GROUP_ID },
        focusedGroupId: CENTER_PANEL_ROOT_GROUP_ID,
      });
    });

    it("selects the focused host surface by default", () => {
      expect(selectActiveCenterSurface(store().byThreadKey, HOST)).toEqual({
        id: HOST_SURFACE_ID,
        kind: "chat-host",
      });
      expect(selectFocusedCenterSurface(stateOf())).toEqual({
        id: HOST_SURFACE_ID,
        kind: "chat-host",
      });
    });

    it("does not persist an unchanged implicit host state", () => {
      store().activateSurface(HOST, CENTER_PANEL_ROOT_GROUP_ID, HOST_SURFACE_ID);
      expect(store().byThreadKey).toEqual({});
    });
  });

  describe("creation and activation", () => {
    it("opens chat and terminal surfaces in the focused group", () => {
      store().openChatPanel(HOST, PANEL_A, "Claude");
      store().openTerminalPanel(HOST, "term-1");

      expect(surfaceIds()).toEqual([HOST_SURFACE_ID, `chat:${PANEL_A}`, "terminal:term-1"]);
      expect(rootGroup()).toMatchObject({
        surfaceIds: [HOST_SURFACE_ID, `chat:${PANEL_A}`, "terminal:term-1"],
        activeSurfaceId: "terminal:term-1",
      });
      expect(stateOf().surfaces[1]).toEqual({
        id: `chat:${PANEL_A}`,
        kind: "chat",
        threadId: PANEL_A,
        providerLabel: "Claude",
      });
    });

    it("re-activates existing surfaces without duplication", () => {
      store().openChatPanel(HOST, PANEL_A);
      store().openChatPanel(HOST, PANEL_B);
      store().openChatPanel(HOST, PANEL_A);

      expect(surfaceIds()).toEqual([HOST_SURFACE_ID, `chat:${PANEL_A}`, `chat:${PANEL_B}`]);
      expect(rootGroup().activeSurfaceId).toBe(`chat:${PANEL_A}`);
    });

    it("persists terminal launch metadata", () => {
      const command = {
        executable: "/opt/codex",
        args: ["--dangerously-bypass-approvals-and-sandbox"],
        label: "Codex Terminal",
        activity: {
          driverKind: ProviderDriverKind.make("codex"),
          providerInstanceId: ProviderInstanceId.make("codex_personal"),
        },
      };
      store().openTerminalPanel(HOST, "term-1", { label: "Codex Terminal", command });
      expect(stateOf().surfaces[1]).toMatchObject({ label: "Codex Terminal", command });
    });

    it("replaces Main with one terminal-only root group", () => {
      const ref = scopeThreadRef(
        EnvironmentId.make("environment-1"),
        ThreadId.make("default-thread"),
      );
      const terminalId = store().replaceMainWithTerminal(ref, ["term-1"], {
        label: "Claude Terminal",
        command: { executable: "claude", args: [] },
      });

      expect(terminalId).toBe("term-2");
      expect(selectThreadCenterPanelState(store().byThreadKey, ref)).toMatchObject({
        surfaces: [{ id: "terminal:term-2", kind: "terminal", terminalId: "term-2" }],
        groups: [
          {
            id: CENTER_PANEL_ROOT_GROUP_ID,
            surfaceIds: ["terminal:term-2"],
            activeSurfaceId: "terminal:term-2",
          },
        ],
      });
    });

    it("allocates replacement terminals through the shared pending reservation authority", () => {
      const pending = reserveTerminalId(HOST, []);

      const terminalId = store().replaceMainWithTerminal(HOST, [], {
        label: "Codex Terminal",
      });

      expect(pending.terminalId).toBe("term-1");
      expect(terminalId).toBe("term-2");
      pending.release();
    });
  });

  describe("terminal placement", () => {
    it("places and activates a terminal tab in the requested group", () => {
      const groupRight = createRightGroup();
      expect(stateOf().focusedGroupId).toBe(groupRight);

      const result = store().placeTerminalPanel(HOST, "term-7", {
        type: "tab",
        groupId: CENTER_PANEL_ROOT_GROUP_ID,
      });

      expect(result).toBe(true);
      expect(rootGroup()).toMatchObject({
        surfaceIds: [HOST_SURFACE_ID, "terminal:term-7"],
        activeSurfaceId: "terminal:term-7",
      });
      expect(stateOf().focusedGroupId).toBe(CENTER_PANEL_ROOT_GROUP_ID);
    });

    it("places a terminal in a right split and preserves its launch options", () => {
      vi.spyOn(crypto, "randomUUID").mockReturnValue("00000000-0000-4000-8000-000000000007");
      const command = { executable: "/opt/codex", args: ["--full-auto"] };

      expect(
        store().placeTerminalPanel(
          HOST,
          "term-7",
          { type: "split", groupId: CENTER_PANEL_ROOT_GROUP_ID, direction: "right" },
          { label: "Codex Terminal", command },
        ),
      ).toBe(true);

      expect(stateOf().layout).toEqual({
        type: "split",
        direction: "horizontal",
        ratio: 0.5,
        first: { type: "leaf", groupId: CENTER_PANEL_ROOT_GROUP_ID },
        second: {
          type: "leaf",
          groupId: "center-group:00000000-0000-4000-8000-000000000007",
        },
      });
      expect(stateOf()).toMatchObject({
        surfaces: [
          { id: HOST_SURFACE_ID, kind: "chat-host" },
          {
            id: "terminal:term-7",
            kind: "terminal",
            terminalId: "term-7",
            label: "Codex Terminal",
            command,
          },
        ],
        groups: [
          {
            id: CENTER_PANEL_ROOT_GROUP_ID,
            surfaceIds: [HOST_SURFACE_ID],
            activeSurfaceId: HOST_SURFACE_ID,
          },
          {
            id: "center-group:00000000-0000-4000-8000-000000000007",
            surfaceIds: ["terminal:term-7"],
            activeSurfaceId: "terminal:term-7",
          },
        ],
        focusedGroupId: "center-group:00000000-0000-4000-8000-000000000007",
      });
    });

    it("places a terminal in a down split", () => {
      vi.spyOn(crypto, "randomUUID").mockReturnValue("00000000-0000-4000-8000-000000000008");

      expect(
        store().placeTerminalPanel(HOST, "term-8", {
          type: "split",
          groupId: CENTER_PANEL_ROOT_GROUP_ID,
          direction: "down",
        }),
      ).toBe(true);

      expect(stateOf().layout).toEqual({
        type: "split",
        direction: "vertical",
        ratio: 0.5,
        first: { type: "leaf", groupId: CENTER_PANEL_ROOT_GROUP_ID },
        second: {
          type: "leaf",
          groupId: "center-group:00000000-0000-4000-8000-000000000008",
        },
      });
    });

    it("rejects a fifth terminal pane without generating an id or mutating state", () => {
      const randomUUID = createFourGroups();
      const callsBeforePlacement = randomUUID.mock.calls.length;
      const before = store().byThreadKey;
      const placement = {
        type: "split" as const,
        groupId: CENTER_PANEL_ROOT_GROUP_ID,
        direction: "right" as const,
      };

      expect(store().validateTerminalPanelPlacement(HOST, placement)).toEqual({
        ok: false,
        reason: "pane-limit",
      });
      expect(store().placeTerminalPanel(HOST, "term-5", placement)).toBe(false);
      expect(store().byThreadKey).toBe(before);
      expect(surfaceIds()).not.toContain("terminal:term-5");
      expect(randomUUID).toHaveBeenCalledTimes(callsBeforePlacement);
    });

    it("rejects a missing terminal target without generating an id or mutating state", () => {
      const randomUUID = vi.spyOn(crypto, "randomUUID");
      const before = store().byThreadKey;
      const placement = {
        type: "split" as const,
        groupId: "missing",
        direction: "down" as const,
      };

      expect(store().validateTerminalPanelPlacement(HOST, placement)).toEqual({
        ok: false,
        reason: "missing-group",
      });
      expect(store().placeTerminalPanel(HOST, "term-missing", placement)).toBe(false);
      expect(store().byThreadKey).toBe(before);
      expect(surfaceIds()).not.toContain("terminal:term-missing");
      expect(randomUUID).not.toHaveBeenCalled();
    });
  });

  describe("group mutations", () => {
    it("uses the focused group for creation after a split", () => {
      store().openChatPanel(HOST, PANEL_A);
      vi.spyOn(crypto, "randomUUID").mockReturnValue("00000000-0000-4000-8000-000000000002");
      const groupRight = "center-group:00000000-0000-4000-8000-000000000002";

      expect(
        store().dropSurface(HOST, `chat:${PANEL_A}`, {
          groupId: CENTER_PANEL_ROOT_GROUP_ID,
          splitDirection: "right",
        }),
      ).toBe(true);
      expect(stateOf().groups.some((group) => group.id === groupRight)).toBe(true);

      store().focusGroup(HOST, groupRight);
      store().openTerminalPanel(HOST, "term-2");
      expect(stateOf().groups.find((group) => group.id === groupRight)?.surfaceIds).toContain(
        "terminal:term-2",
      );
    });

    it("keeps invalid or no-op drops atomic", () => {
      store().openChatPanel(HOST, PANEL_A);
      const before = store().byThreadKey;
      expect(
        store().dropSurface(HOST, "chat:missing", { groupId: CENTER_PANEL_ROOT_GROUP_ID }),
      ).toBe(false);
      expect(store().byThreadKey).toBe(before);

      expect(
        store().dropSurface(HOST, `chat:${PANEL_A}`, {
          groupId: CENTER_PANEL_ROOT_GROUP_ID,
          index: 1,
        }),
      ).toBe(false);
      expect(store().byThreadKey).toBe(before);
    });

    it("merges a group without changing the surface descriptor order", () => {
      const groupRight = createRightGroup();
      const allIdsBeforeMerge = surfaceIds();

      expect(store().mergeGroup(HOST, groupRight)).toBe(true);
      expect(surfaceIds()).toEqual(allIdsBeforeMerge);
      expect(stateOf().groups).toHaveLength(1);
    });

    it("updates only the addressed split ratio", () => {
      createRightGroup();
      store().focusGroup(HOST, CENTER_PANEL_ROOT_GROUP_ID);
      store().openTerminalPanel(HOST, "term-1");
      vi.spyOn(crypto, "randomUUID").mockReturnValue("00000000-0000-4000-8000-000000000003");
      expect(
        store().dropSurface(HOST, "terminal:term-1", {
          groupId: CENTER_PANEL_ROOT_GROUP_ID,
          splitDirection: "down",
        }),
      ).toBe(true);

      const before = stateOf();
      if (before.layout.type !== "split" || before.layout.first.type !== "split") {
        throw new Error("expected a nested split layout");
      }
      store().setSplitRatio(HOST, ["first"], 0.3);
      expect(stateOf().layout).toEqual({
        ...before.layout,
        first: { ...before.layout.first, ratio: 0.3 },
      });
      expect(stateOf().groups).toEqual(before.groups);
    });
  });

  describe("group-local closes and returned removals", () => {
    it("closes only surfaces to the right in the specified group and returns them", () => {
      const groupRight = createRightGroup();
      store().focusGroup(HOST, groupRight);
      store().openTerminalPanel(HOST, "term-2");
      const rootIdsBefore = rootGroup().surfaceIds;

      const removed = store().closeSurfacesToRight(HOST, groupRight, `chat:${PANEL_A}`);
      expect(removed.map((surface) => surface.id)).toEqual(["terminal:term-2"]);
      expect(rootGroup().surfaceIds).toEqual(rootIdsBefore);
    });

    it("keeps the host only when closing others within its group", () => {
      store().openChatPanel(HOST, PANEL_A);
      const removed = store().closeOtherSurfaces(
        HOST,
        CENTER_PANEL_ROOT_GROUP_ID,
        `chat:${PANEL_A}`,
      );

      expect(removed.map((surface) => surface.id)).toEqual([]);
      expect(rootGroup().surfaceIds).toEqual([HOST_SURFACE_ID, `chat:${PANEL_A}`]);
    });

    it("returns no removals and preserves identity for invalid group-local closes", () => {
      const before = store().byThreadKey;
      expect(store().closeSurface(HOST, "missing", HOST_SURFACE_ID)).toEqual([]);
      expect(store().closeAllSurfaces(HOST, "missing")).toEqual([]);
      expect(store().byThreadKey).toBe(before);
    });

    it("persists an explicit empty root group after closing all surfaces", () => {
      const removed = store().closeAllSurfaces(HOST, CENTER_PANEL_ROOT_GROUP_ID);
      expect(removed.map((surface) => surface.id)).toEqual([HOST_SURFACE_ID]);
      expect(store().byThreadKey).toEqual({
        "environment-1:host-1": {
          surfaces: [],
          groups: [{ id: CENTER_PANEL_ROOT_GROUP_ID, surfaceIds: [], activeSurfaceId: null }],
          layout: { type: "leaf", groupId: CENTER_PANEL_ROOT_GROUP_ID },
          focusedGroupId: CENTER_PANEL_ROOT_GROUP_ID,
        },
      });
    });
  });

  describe("selectors", () => {
    it("exposes focused and visible surfaces", () => {
      const groupRight = createRightGroup();
      store().focusGroup(HOST, groupRight);
      store().openTerminalPanel(HOST, "term-2");

      expect(selectFocusedCenterPanelGroup(stateOf()).id).toBe(groupRight);
      expect(selectFocusedCenterSurface(stateOf())?.id).toBe("terminal:term-2");
      expect(selectVisibleCenterSurfaces(stateOf())).toEqual([
        {
          groupId: CENTER_PANEL_ROOT_GROUP_ID,
          surface: { id: HOST_SURFACE_ID, kind: "chat-host" },
          focused: false,
        },
        {
          groupId: groupRight,
          surface: { id: "terminal:term-2", kind: "terminal", terminalId: "term-2" },
          focused: true,
        },
      ]);
      expect(selectVisibleCenterSurfaces(stateOf())).not.toContainEqual({
        groupId: groupRight,
        surface: { id: `chat:${PANEL_A}`, kind: "chat", threadId: PANEL_A },
        focused: true,
      });
    });
  });

  describe("migration", () => {
    it("migrates the flat v2 state into one root group", () => {
      const migrated = migratePersistedCenterPanelState({
        byThreadKey: {
          "environment-1:host-1": {
            surfaces: [
              { id: HOST_SURFACE_ID, kind: "chat-host" },
              { kind: "terminal", terminalId: "term-1" },
            ],
            activeSurfaceId: "terminal:term-1",
          },
        },
      });
      expect(migrated.byThreadKey["environment-1:host-1"]).toMatchObject({
        groups: [
          {
            id: CENTER_PANEL_ROOT_GROUP_ID,
            surfaceIds: [HOST_SURFACE_ID, "terminal:term-1"],
            activeSurfaceId: "terminal:term-1",
          },
        ],
        layout: { type: "leaf", groupId: CENTER_PANEL_ROOT_GROUP_ID },
        focusedGroupId: CENTER_PANEL_ROOT_GROUP_ID,
      });
    });

    it("preserves host-closed, explicit-empty, and sanitized terminal states", () => {
      const migrated = migratePersistedCenterPanelState({
        byThreadKey: {
          closed: {
            activeSurfaceId: "terminal:term-1",
            surfaces: [{ kind: "terminal", terminalId: "term-1", label: " Terminal " }],
          },
          empty: { activeSurfaceId: null, surfaces: [] },
          invalid: {
            activeSurfaceId: "terminal:term-2",
            surfaces: [
              { id: HOST_SURFACE_ID, kind: "chat-host" },
              { kind: "terminal", terminalId: "term-2", command: { executable: " ", args: [] } },
            ],
          },
        },
      });

      expect(migrated.byThreadKey.closed).toMatchObject({
        surfaces: [
          { id: "terminal:term-1", kind: "terminal", terminalId: "term-1", label: "Terminal" },
        ],
      });
      expect(migrated.byThreadKey.empty).toMatchObject({
        surfaces: [],
        groups: [{ id: CENTER_PANEL_ROOT_GROUP_ID, surfaceIds: [], activeSurfaceId: null }],
      });
      expect(migrated.byThreadKey.invalid?.surfaces[1]).toEqual({
        id: "terminal:term-2",
        kind: "terminal",
        terminalId: "term-2",
      });
    });

    it("drops only the exact implicit host default", () => {
      const migrated = migratePersistedCenterPanelState({
        byThreadKey: {
          implicit: {
            activeSurfaceId: HOST_SURFACE_ID,
            surfaces: [{ id: HOST_SURFACE_ID, kind: "chat-host" }],
          },
          explicit: {
            surfaces: [{ id: HOST_SURFACE_ID, kind: "chat-host" }],
            groups: [
              { id: "other", surfaceIds: [HOST_SURFACE_ID], activeSurfaceId: HOST_SURFACE_ID },
            ],
            layout: { type: "leaf", groupId: "other" },
            focusedGroupId: "other",
          },
        },
      });
      expect(migrated.byThreadKey.implicit).toBeUndefined();
      expect(migrated.byThreadKey.explicit).toBeDefined();
    });
  });

  describe("thread cleanup", () => {
    it("removes a stored thread and preserves absent state identity", () => {
      store().openChatPanel(HOST, PANEL_A);
      store().removeThread(HOST);
      expect(store().byThreadKey).toEqual({});
      const before = store();
      store().removeThread(HOST);
      expect(store()).toBe(before);
    });

    it("removes every nested chat surface that references the deleted thread", () => {
      const otherHost = scopeThreadRef(
        HOST.environmentId,
        ThreadId.make("host-with-panel-reference"),
      );
      const deletedPanelRef = scopeThreadRef(HOST.environmentId, PANEL_A);
      store().openTerminalPanel(deletedPanelRef, "term-deleted");
      store().openChatPanel(HOST, PANEL_A, "Codex");
      store().openTerminalPanel(HOST, "term-host");
      store().openChatPanel(otherHost, PANEL_A, "Codex");

      store().removeThread(deletedPanelRef);

      expect(store().byThreadKey[scopedThreadKey(deletedPanelRef)]).toBeUndefined();
      expect(surfaceIds()).toEqual([HOST_SURFACE_ID, "terminal:term-host"]);
      expect(surfaceIds(otherHost)).toEqual([HOST_SURFACE_ID]);
    });
  });
});

function createRightGroup(): string {
  store().openChatPanel(HOST, PANEL_A);
  vi.spyOn(crypto, "randomUUID").mockReturnValue("00000000-0000-4000-8000-000000000002");
  const groupRight = "center-group:00000000-0000-4000-8000-000000000002";
  expect(
    store().dropSurface(HOST, `chat:${PANEL_A}`, {
      groupId: CENTER_PANEL_ROOT_GROUP_ID,
      splitDirection: "right",
    }),
  ).toBe(true);
  return groupRight;
}

function createFourGroups() {
  const randomUUID = vi.spyOn(crypto, "randomUUID");
  for (const suffix of ["000000000002", "000000000003", "000000000004"] as const) {
    randomUUID.mockReturnValueOnce(`00000000-0000-4000-8000-${suffix}`);
    store().focusGroup(HOST, CENTER_PANEL_ROOT_GROUP_ID);
    const terminalId = `seed-${suffix.at(-1)}`;
    store().openTerminalPanel(HOST, terminalId);
    expect(
      store().dropSurface(HOST, `terminal:${terminalId}`, {
        groupId: CENTER_PANEL_ROOT_GROUP_ID,
        splitDirection: "right",
      }),
    ).toBe(true);
  }
  expect(stateOf().groups).toHaveLength(4);
  return randomUUID;
}
