// @vitest-environment happy-dom

import { scopeThreadRef } from "@coddy/client-runtime/environment";
import { EnvironmentId, ThreadId } from "@coddy/contracts";
import { act, useEffect } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import { CENTER_PANEL_ROOT_GROUP_ID } from "./centerPanelLayout";
import { useCenterPanelStore } from "./centerPanelStore";
import { useRightPanelStore } from "./rightPanelStore";
import { useThreadHasTerminalSurface } from "./terminalSurfaceState";

const ref = scopeThreadRef(EnvironmentId.make("local"), ThreadId.make("thread-a"));
const otherRef = scopeThreadRef(EnvironmentId.make("local"), ThreadId.make("thread-b"));

let container: HTMLDivElement;
let root: Root;
let renderCount: number;

function Probe() {
  const hasTerminal = useThreadHasTerminalSurface(ref);
  useEffect(() => {
    renderCount += 1;
  });
  return <output>{String(hasTerminal)}</output>;
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  useCenterPanelStore.setState({ byThreadKey: {} });
  useRightPanelStore.setState({ byThreadKey: {} });
  renderCount = 0;
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  container.remove();
  vi.restoreAllMocks();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("useThreadHasTerminalSurface subscriptions", () => {
  it("does not rerender when the requested thread split ratio changes", async () => {
    useCenterPanelStore.getState().openTerminalPanel(ref, "terminal-a");
    useCenterPanelStore.getState().openChatPanel(ref, ThreadId.make("panel-a"), "Codex");
    vi.spyOn(crypto, "randomUUID").mockReturnValue("00000000-0000-4000-8000-000000000001");
    expect(
      useCenterPanelStore.getState().dropSurface(ref, "chat:panel-a", {
        groupId: CENTER_PANEL_ROOT_GROUP_ID,
        splitDirection: "right",
      }),
    ).toBe(true);
    await act(async () => root.render(<Probe />));
    expect(renderCount).toBe(1);
    expect(container.textContent).toBe("true");

    await act(async () => useCenterPanelStore.getState().setSplitRatio(ref, [], 0.3));

    expect(renderCount).toBe(1);
  });

  it("does not rerender for unrelated center or right thread mutations", async () => {
    useCenterPanelStore.getState().openTerminalPanel(ref, "terminal-a");
    await act(async () => root.render(<Probe />));
    expect(renderCount).toBe(1);

    await act(async () => useCenterPanelStore.getState().openTerminalPanel(otherRef, "terminal-b"));
    await act(async () => useRightPanelStore.getState().openTerminal(otherRef, "terminal-c"));

    expect(renderCount).toBe(1);
    expect(container.textContent).toBe("true");
  });
});
