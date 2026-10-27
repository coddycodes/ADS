// @vitest-environment happy-dom

import { ThreadId } from "@coddy/contracts";
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import type { ThreadCenterPanelState } from "~/centerPanelStore";

vi.mock("@dnd-kit/core", () => ({
  DndContext: ({ children }: { readonly children: ReactNode }) => <>{children}</>,
  DragOverlay: ({ children }: { readonly children: ReactNode }) => <>{children}</>,
  PointerSensor: function PointerSensor() {},
  closestCenter: vi.fn(() => []),
  pointerWithin: vi.fn(() => []),
  useSensor: (sensor: unknown, options: unknown) => ({ sensor, options }),
  useSensors: (...sensors: unknown[]) => sensors,
}));

vi.mock("./CenterPanelSplitLayout", () => ({
  CenterPanelSplitLayout: (props: {
    readonly state: ThreadCenterPanelState;
    readonly registerBodyTarget: (groupId: string) => (node: HTMLDivElement | null) => void;
  }) => (
    <div data-center-panel-split-layout>
      {props.state.groups.map((group) => (
        <div
          key={group.id}
          ref={props.registerBodyTarget(group.id)}
          data-center-panel-body-target={group.id}
        />
      ))}
    </div>
  ),
}));

vi.mock("./CenterPanelTabs", () => ({
  CenterSurfaceIcon: () => null,
}));

import { CenterPanelWorkspace, type CenterPanelWorkspaceProps } from "./CenterPanelWorkspace";

const host = { id: "chat:host", kind: "chat-host" } as const;
const chat = {
  id: "chat:thread-1",
  kind: "chat",
  threadId: ThreadId.make("thread-1"),
  providerLabel: "Codex",
} as const;

const chatInLeft: ThreadCenterPanelState = {
  surfaces: [host, chat],
  groups: [
    { id: "group-left", surfaceIds: [host.id, chat.id], activeSurfaceId: chat.id },
    { id: "group-right", surfaceIds: [], activeSurfaceId: null },
  ],
  layout: {
    type: "split",
    direction: "horizontal",
    ratio: 0.5,
    first: { type: "leaf", groupId: "group-left" },
    second: { type: "leaf", groupId: "group-right" },
  },
  focusedGroupId: "group-left",
};

const chatInRight: ThreadCenterPanelState = {
  ...chatInLeft,
  groups: [
    { id: "group-left", surfaceIds: [host.id], activeSurfaceId: host.id },
    { id: "group-right", surfaceIds: [chat.id], activeSurfaceId: chat.id },
  ],
  focusedGroupId: "group-right",
};

let container: HTMLDivElement;
let root: Root;
let frameCallbacks: FrameRequestCallback[];

class FakeResizeObserver {
  observe(): void {}
  unobserve(): void {}
  disconnect(): void {}
}

function workspaceProps(state: ThreadCenterPanelState): CenterPanelWorkspaceProps {
  return {
    state,
    hostLabel: "Codex",
    renderFocusedActions: () => null,
    renderSurface: (surface) => (
      <div className="min-h-0 flex-1" data-live-surface-content={surface.id} />
    ),
    onFocusGroup: vi.fn(),
    onActivate: vi.fn(),
    onCloseSurface: vi.fn(),
    onCloseOtherSurfaces: vi.fn(),
    onCloseSurfacesToRight: vi.fn(),
    onCloseAllSurfaces: vi.fn(),
    onDropSurface: vi.fn(),
    onMergeGroup: vi.fn(),
    onSetSplitRatio: vi.fn(),
  };
}

async function flushFrames(): Promise<void> {
  while (frameCallbacks.length > 0) {
    const callbacks = frameCallbacks;
    frameCallbacks = [];
    await act(async () => callbacks.forEach((callback) => callback(0)));
  }
}

async function renderWorkspace(state: ThreadCenterPanelState): Promise<void> {
  await act(async () => root.render(<CenterPanelWorkspace {...workspaceProps(state)} />));
  await flushFrames();
}

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  frameCallbacks = [];
  vi.stubGlobal("ResizeObserver", FakeResizeObserver);
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
    frameCallbacks.push(callback);
    return frameCallbacks.length;
  });
  vi.stubGlobal("cancelAnimationFrame", vi.fn());
  vi.spyOn(Element.prototype, "getBoundingClientRect").mockImplementation(function (this: Element) {
    const groupId = this.getAttribute("data-center-panel-body-target");
    if (groupId === "group-left") {
      return new DOMRect(0, 32, 320, 368);
    }
    if (groupId === "group-right") {
      return new DOMRect(320, 32, 320, 368);
    }
    return new DOMRect(0, 0, 640, 400);
  });
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(async () => {
  await act(async () => root.unmount());
  document.body.replaceChildren();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("CenterPanelWorkspace surface hosts", () => {
  it("gives live surface roots a bounded flex column while preserving their mounted content", async () => {
    await renderWorkspace(chatInLeft);
    const beforeHost = container.querySelector<HTMLElement>(
      '[data-center-surface-host="chat:thread-1"]',
    );
    const beforeContent = beforeHost?.querySelector('[data-live-surface-content="chat:thread-1"]');

    expect(beforeHost?.classList.contains("absolute")).toBe(true);
    expect(beforeHost?.classList.contains("overflow-hidden")).toBe(true);
    expect(beforeHost?.classList.contains("flex")).toBe(true);
    expect(beforeHost?.classList.contains("flex-col")).toBe(true);
    expect(beforeHost?.classList.contains("min-h-0")).toBe(true);
    expect(beforeHost?.classList.contains("min-w-0")).toBe(true);
    expect(beforeContent?.classList.contains("flex-1")).toBe(true);

    await renderWorkspace(chatInRight);
    const afterHost = container.querySelector<HTMLElement>(
      '[data-center-surface-host="chat:thread-1"]',
    );

    expect(afterHost).toBe(beforeHost);
    expect(afterHost?.querySelector('[data-live-surface-content="chat:thread-1"]')).toBe(
      beforeContent,
    );
    expect(afterHost?.dataset.centerSurfaceGroupId).toBe("group-right");
    expect(afterHost?.style.left).toBe("320px");
  });
});
