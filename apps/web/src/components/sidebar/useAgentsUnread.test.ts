// @vitest-environment happy-dom

import type { OrchestrationLatestTurnState } from "@coddy/contracts";
import { act, createElement } from "react";
import { createRoot } from "react-dom/client";
import { describe, expect, it, vi } from "vite-plus/test";

const h = vi.hoisted(() => ({
  markUnread: vi.fn(),
  router: { navigate: vi.fn() },
}));

vi.mock("@tanstack/react-router", () => ({
  useRouter: () => h.router,
}));

vi.mock("../../sidebarWorkspaceMetaStore", () => ({
  useSidebarWorkspaceMetaStore: (
    selector: (state: { markUnread: typeof h.markUnread }) => unknown,
  ) => selector({ markUnread: h.markUnread }),
}));

import type { AgentRow } from "./agentsSection.logic";
import { detectUnreadTransitions, useAgentsUnread } from "./useAgentsUnread";

function rowWithTurn(key: string, turnId: string, state: OrchestrationLatestTurnState): AgentRow {
  return {
    key,
    shell: {
      latestTurn: { turnId, state },
    },
  } as unknown as AgentRow;
}

function rowWithoutTurn(key: string): AgentRow {
  return {
    key,
    shell: { latestTurn: null },
  } as unknown as AgentRow;
}

describe("detectUnreadTransitions", () => {
  it("marks unread when the latest turn transitions into a settled state", () => {
    const previous = new Map([["k1", "turn-1:running"]]);

    const result = detectUnreadTransitions({
      previous,
      rows: [rowWithTurn("k1", "turn-1", "completed")],
      openThreadKey: null,
    });

    expect(result.markUnreadKeys).toEqual(["k1"]);
    expect(result.next).toEqual(new Map([["k1", "turn-1:completed"]]));
  });

  it("does not mark the open route thread, unchanged states, or first observations", () => {
    const openThread = detectUnreadTransitions({
      previous: new Map([["k1", "turn-1:running"]]),
      rows: [rowWithTurn("k1", "turn-1", "completed")],
      openThreadKey: "k1",
    });
    const unchanged = detectUnreadTransitions({
      previous: new Map([["k1", "turn-1:completed"]]),
      rows: [rowWithTurn("k1", "turn-1", "completed")],
      openThreadKey: null,
    });
    const firstObservation = detectUnreadTransitions({
      previous: new Map(),
      rows: [rowWithTurn("k1", "turn-1", "completed")],
      openThreadKey: null,
    });

    expect(openThread.markUnreadKeys).toEqual([]);
    expect(openThread.next).toEqual(new Map([["k1", "turn-1:completed"]]));
    expect(unchanged.markUnreadKeys).toEqual([]);
    expect(unchanged.next).toEqual(new Map([["k1", "turn-1:completed"]]));
    expect(firstObservation.markUnreadKeys).toEqual([]);
    expect(firstObservation.next).toEqual(new Map([["k1", "turn-1:completed"]]));
  });

  it("treats interrupted and error like completed, and running/null as not-settled", () => {
    const result = detectUnreadTransitions({
      previous: new Map([
        ["interrupted", "turn-1:running"],
        ["error", "turn-2:running"],
        ["running", "turn-3:completed"],
        ["without-turn", "turn-4:running"],
      ]),
      rows: [
        rowWithTurn("interrupted", "turn-1", "interrupted"),
        rowWithTurn("error", "turn-2", "error"),
        rowWithTurn("running", "turn-3", "running"),
        rowWithoutTurn("without-turn"),
      ],
      openThreadKey: null,
    });

    expect(result.markUnreadKeys).toEqual(["interrupted", "error"]);
    expect(result.next).toEqual(
      new Map([
        ["interrupted", "turn-1:interrupted"],
        ["error", "turn-2:error"],
        ["running", "turn-3:running"],
      ]),
    );
  });
});

describe("useAgentsUnread", () => {
  it("tracks unread transitions when the router omits state and subscription APIs", async () => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    h.markUnread.mockReset();
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);
    const Probe = ({ rows }: { readonly rows: ReadonlyArray<AgentRow> }) => {
      useAgentsUnread(rows);
      return null;
    };

    try {
      await act(async () =>
        root.render(createElement(Probe, { rows: [rowWithTurn("k1", "turn-1", "running")] })),
      );
      await act(async () =>
        root.render(createElement(Probe, { rows: [rowWithTurn("k1", "turn-1", "completed")] })),
      );

      expect(h.markUnread).toHaveBeenCalledWith("k1");
    } finally {
      await act(async () => root.unmount());
      container.remove();
      (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
    }
  });
});
