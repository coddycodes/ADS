import { scopeThreadRef } from "@coddy/client-runtime/environment";
import type { EnvironmentId, ThreadId } from "@coddy/contracts";
import { beforeEach, describe, expect, it } from "vite-plus/test";
import { useCenterPanelStore } from "./centerPanelStore";
import { useRightPanelStore } from "./rightPanelStore";
import { selectThreadHasTerminalSurface } from "./terminalSurfaceState";

const ref = scopeThreadRef("local" as EnvironmentId, "thread-a" as ThreadId);
const otherRef = scopeThreadRef("local" as EnvironmentId, "thread-b" as ThreadId);

function selected(refToRead = ref): boolean {
  return selectThreadHasTerminalSurface(
    useCenterPanelStore.getState().byThreadKey,
    useRightPanelStore.getState().byThreadKey,
    refToRead,
  );
}

beforeEach(() => {
  useCenterPanelStore.setState({ byThreadKey: {} });
  useRightPanelStore.setState({ byThreadKey: {} });
});

describe("selectThreadHasTerminalSurface", () => {
  it("returns true for a center terminal", () => {
    useCenterPanelStore.getState().openTerminalPanel(ref, "term-1");
    expect(selected()).toBe(true);
  });

  it("returns true for a right-panel terminal", () => {
    useRightPanelStore.getState().openTerminal(ref, "term-2");
    expect(selected()).toBe(true);
  });

  it("returns false for an empty or unrelated thread", () => {
    expect(selected()).toBe(false);
    expect(selectThreadHasTerminalSurface({}, {}, null)).toBe(false);

    useCenterPanelStore.getState().openTerminalPanel(otherRef, "term-3");
    expect(selected()).toBe(false);
    expect(selected(otherRef)).toBe(true);
  });
});
