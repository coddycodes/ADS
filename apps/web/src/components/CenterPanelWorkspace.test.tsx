// @vitest-environment happy-dom

import { ThreadId } from "@coddy/contracts";
import type {
  CollisionDetection,
  DragEndEvent,
  DragMoveEvent,
  DragStartEvent,
} from "@dnd-kit/core";
import { act, createRef, forwardRef, useImperativeHandle, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import { dropCenterPanelSurface } from "~/centerPanelLayout";
import type { ThreadCenterPanelState } from "~/centerPanelStore";

const harness = vi.hoisted(() => ({
  dndProps: null as Record<string, unknown> | null,
  layoutProps: null as Record<string, unknown> | null,
  pointerWithin: vi.fn(),
  closestCenter: vi.fn(),
  sensorCalls: [] as Array<{ readonly sensor: unknown; readonly options: unknown }>,
  syncRects: vi.fn(),
}));

vi.mock("@dnd-kit/core", () => ({
  DndContext: (props: Record<string, unknown>) => {
    harness.dndProps = props;
    return <>{props.children as ReactNode}</>;
  },
  DragOverlay: ({ children }: { readonly children: ReactNode }) => (
    <div data-drag-overlay>{children}</div>
  ),
  PointerSensor: function PointerSensor() {},
  closestCenter: harness.closestCenter,
  pointerWithin: harness.pointerWithin,
  useSensor: (sensor: unknown, options: unknown) => {
    harness.sensorCalls.push({ sensor, options });
    return { sensor, options };
  },
  useSensors: (...sensors: unknown[]) => sensors,
}));

vi.mock("./CenterPanelSplitLayout", () => ({
  CenterPanelSplitLayout: (props: Record<string, unknown>) => {
    harness.layoutProps = props;
    const state = props.state as ThreadCenterPanelState;
    const registerBodyTarget = props.registerBodyTarget as (
      groupId: string,
    ) => (node: HTMLDivElement | null) => void;
    return (
      <div data-center-panel-split-layout>
        {state.groups.map((group) => (
          <section key={group.id} data-center-panel-group data-center-panel-group-id={group.id}>
            <header data-center-panel-group-header>
              <div data-center-panel-tab-list>
                <div data-slot="scroll-area-viewport">
                  {group.surfaceIds.map((surfaceId) => (
                    <div
                      key={surfaceId}
                      data-center-panel-tab-id={surfaceId}
                      data-center-panel-group-id={group.id}
                    />
                  ))}
                </div>
              </div>
            </header>
            <div ref={registerBodyTarget(group.id)} data-center-panel-body-target={group.id} />
          </section>
        ))}
      </div>
    );
  },
}));

vi.mock("./CenterPanelSurfaceHosts", async (importOriginal) => {
  const original = await importOriginal<typeof import("./CenterPanelSurfaceHosts")>();
  return {
    ...original,
    CenterPanelSurfaceHosts: forwardRef(function MockCenterPanelSurfaceHosts(_, ref) {
      useImperativeHandle(ref, () => ({ syncRects: harness.syncRects }), []);
      return <div data-center-panel-surface-hosts />;
    }),
  };
});

vi.mock("./CenterPanelTabs", () => ({
  CenterSurfaceIcon: ({ kind }: { readonly kind: string }) => (
    <span data-center-surface-icon={kind} />
  ),
}));

import {
  CenterPanelWorkspace,
  type CenterPanelWorkspaceHandle,
  type CenterPanelWorkspaceProps,
} from "./CenterPanelWorkspace";

const chatA = {
  id: "chat:a",
  kind: "chat",
  threadId: ThreadId.make("a"),
  providerLabel: "A",
} as const;
const chatB = {
  id: "chat:b",
  kind: "chat",
  threadId: ThreadId.make("b"),
  providerLabel: "B",
} as const;
const chatC = {
  id: "chat:c",
  kind: "chat",
  threadId: ThreadId.make("c"),
  providerLabel: "C",
} as const;
const terminalD = {
  id: "terminal:d",
  kind: "terminal",
  terminalId: "d",
} as const;
const terminalE = {
  id: "terminal:e",
  kind: "terminal",
  terminalId: "e",
} as const;

const twoGroupState: ThreadCenterPanelState = {
  surfaces: [chatA, chatB, chatC],
  groups: [
    { id: "group-a", surfaceIds: [chatA.id, chatB.id], activeSurfaceId: chatA.id },
    { id: "group-b", surfaceIds: [chatC.id], activeSurfaceId: chatC.id },
  ],
  layout: {
    type: "split",
    direction: "horizontal",
    ratio: 0.5,
    first: { type: "leaf", groupId: "group-a" },
    second: { type: "leaf", groupId: "group-b" },
  },
  focusedGroupId: "group-a",
};

const soleTabState: ThreadCenterPanelState = {
  surfaces: [chatA],
  groups: [{ id: "group-a", surfaceIds: [chatA.id], activeSurfaceId: chatA.id }],
  layout: { type: "leaf", groupId: "group-a" },
  focusedGroupId: "group-a",
};

const fourGroupState: ThreadCenterPanelState = {
  surfaces: [chatA, chatB, chatC, terminalD, terminalE],
  groups: [
    { id: "group-a", surfaceIds: [chatA.id, chatB.id], activeSurfaceId: chatA.id },
    { id: "group-b", surfaceIds: [chatC.id], activeSurfaceId: chatC.id },
    { id: "group-c", surfaceIds: [terminalD.id], activeSurfaceId: terminalD.id },
    { id: "group-d", surfaceIds: [terminalE.id], activeSurfaceId: terminalE.id },
  ],
  layout: {
    type: "split",
    direction: "horizontal",
    ratio: 0.5,
    first: { type: "leaf", groupId: "group-a" },
    second: {
      type: "split",
      direction: "vertical",
      ratio: 0.5,
      first: { type: "leaf", groupId: "group-b" },
      second: {
        type: "split",
        direction: "horizontal",
        ratio: 0.5,
        first: { type: "leaf", groupId: "group-c" },
        second: { type: "leaf", groupId: "group-d" },
      },
    },
  },
  focusedGroupId: "group-a",
};

interface RectInput {
  readonly left: number;
  readonly top: number;
  readonly width: number;
  readonly height: number;
}

let container: HTMLDivElement;
let root: Root;
let frameCallbacks: FrameRequestCallback[];
let resizeObservers: FakeResizeObserver[];
let workspaceRect: RectInput;
let groupABodyRect: RectInput;
let groupBBodyRect: RectInput;
let tabLeftBySurfaceId: Map<string, number>;
let rectReadCounts: Map<string, number>;

class FakeResizeObserver {
  readonly observed = new Set<Element>();

  constructor(readonly callback: ResizeObserverCallback) {
    resizeObservers.push(this);
  }

  observe(target: Element): void {
    this.observed.add(target);
  }

  unobserve(target: Element): void {
    this.observed.delete(target);
  }

  disconnect(): void {
    this.observed.clear();
  }

  trigger(): void {
    this.callback([], this as unknown as ResizeObserver);
  }
}

function domRect(input: RectInput): DOMRect {
  return {
    ...input,
    x: input.left,
    y: input.top,
    right: input.left + input.width,
    bottom: input.top + input.height,
    toJSON: () => ({}),
  } as DOMRect;
}

function elementRect(element: Element): RectInput {
  if (element.hasAttribute("data-center-panel-workspace")) return workspaceRect;
  const groupId =
    element.getAttribute("data-center-panel-group-id") ??
    element.getAttribute("data-center-panel-body-target") ??
    element.closest("[data-center-panel-group-id]")?.getAttribute("data-center-panel-group-id");
  if (element.hasAttribute("data-center-panel-tab-id")) {
    const surfaceId = element.getAttribute("data-center-panel-tab-id");
    return {
      left: tabLeftBySurfaceId.get(surfaceId ?? "") ?? 0,
      top: 100,
      width: 100,
      height: 32,
    };
  }
  if (element.hasAttribute("data-center-panel-group-header")) {
    return groupId === "group-b"
      ? { left: 200, top: 100, width: 500, height: 32 }
      : { left: 0, top: 100, width: 500, height: 32 };
  }
  if (element.hasAttribute("data-center-panel-body-target")) {
    return groupId === "group-b" ? groupBBodyRect : groupABodyRect;
  }
  if (element.hasAttribute("data-center-panel-group")) {
    return groupId === "group-b"
      ? { left: 200, top: 100, width: 500, height: 400 }
      : { left: 0, top: 100, width: 500, height: 400 };
  }
  return { left: 0, top: 0, width: 0, height: 0 };
}

function rectReadKey(element: Element): string {
  const bodyGroupId = element.getAttribute("data-center-panel-body-target");
  if (bodyGroupId) return `body:${bodyGroupId}`;
  return (
    element.getAttribute("data-center-panel-tab-id") ??
    element.getAttribute("data-center-panel-group-id") ??
    (element.hasAttribute("data-center-panel-workspace") ? "workspace" : "other")
  );
}

function input(state: ThreadCenterPanelState = twoGroupState) {
  return {
    state,
    hostLabel: "Codex",
    renderFocusedActions: () => <button type="button">New panel</button>,
    renderSurface: (surface) => <div data-surface={surface.id} />,
    onFocusGroup: vi.fn(),
    onActivate: vi.fn(),
    onCloseSurface: vi.fn(),
    onCloseOtherSurfaces: vi.fn(),
    onCloseSurfacesToRight: vi.fn(),
    onCloseAllSurfaces: vi.fn(),
    onDropSurface: vi.fn<CenterPanelWorkspaceProps["onDropSurface"]>(),
    onMergeGroup: vi.fn(),
    onSetSplitRatio: vi.fn(),
  } satisfies CenterPanelWorkspaceProps;
}

function pointerDragStart(
  surfaceId: string,
  groupId: string,
  point: { readonly x: number; readonly y: number },
): DragStartEvent {
  return {
    active: {
      id: surfaceId,
      data: {
        current: {
          type: "center-panel-tab",
          surfaceId,
          groupId,
          surfaceKind: "chat",
          title: "Dragged",
        },
      },
    },
    activatorEvent: Object.assign(new Event("pointerdown"), {
      clientX: point.x,
      clientY: point.y,
    }),
  } as unknown as DragStartEvent;
}

function dragMoveOverPane(
  groupId: string,
  start: { readonly x: number; readonly y: number },
  point: { readonly x: number; readonly y: number },
): DragMoveEvent {
  return {
    delta: { x: point.x - start.x, y: point.y - start.y },
    over: {
      id: `center-pane:${groupId}`,
      data: { current: { type: "center-panel-pane", groupId } },
    },
  } as unknown as DragMoveEvent;
}

function dragOverTab(
  surfaceId: string,
  groupId: string,
  start: { readonly x: number; readonly y: number },
  point: { readonly x: number; readonly y: number },
): DragMoveEvent {
  return {
    delta: { x: point.x - start.x, y: point.y - start.y },
    over: {
      id: surfaceId,
      data: {
        current: {
          type: "center-panel-tab",
          surfaceId,
          groupId,
          surfaceKind: "chat",
          title: "Target",
        },
      },
    },
  } as unknown as DragMoveEvent;
}

function dragEndFrom(move: DragMoveEvent): DragEndEvent {
  return move as unknown as DragEndEvent;
}

function handlers() {
  expect(harness.dndProps).not.toBeNull();
  return harness.dndProps as {
    readonly collisionDetection: CollisionDetection;
    readonly onDragStart: (event: DragStartEvent) => void;
    readonly onDragMove: (event: DragMoveEvent) => void;
    readonly onDragEnd: (event: DragEndEvent) => void;
    readonly onDragCancel: () => void;
  };
}

async function flushFrames(): Promise<void> {
  while (frameCallbacks.length > 0) {
    const callbacks = frameCallbacks;
    frameCallbacks = [];
    await act(async () => callbacks.forEach((callback) => callback(0)));
  }
}

async function renderWorkspace(props: CenterPanelWorkspaceProps): Promise<void> {
  await act(async () => root.render(<CenterPanelWorkspace {...props} />));
  await flushFrames();
}

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  frameCallbacks = [];
  resizeObservers = [];
  workspaceRect = { left: 0, top: 0, width: 700, height: 500 };
  groupABodyRect = { left: 0, top: 132, width: 500, height: 368 };
  groupBBodyRect = { left: 200, top: 132, width: 500, height: 368 };
  tabLeftBySurfaceId = new Map([
    ["chat:a", 100],
    ["chat:b", 200],
    ["chat:c", 300],
    ["terminal:d", 400],
    ["terminal:e", 500],
  ]);
  rectReadCounts = new Map();
  harness.dndProps = null;
  harness.layoutProps = null;
  harness.pointerWithin.mockReset();
  harness.closestCenter.mockReset();
  harness.sensorCalls = [];
  harness.syncRects.mockReset();
  vi.stubGlobal("ResizeObserver", FakeResizeObserver);
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
    frameCallbacks.push(callback);
    return frameCallbacks.length;
  });
  vi.stubGlobal("cancelAnimationFrame", vi.fn());
  vi.spyOn(Element.prototype, "getBoundingClientRect").mockImplementation(function (this: Element) {
    const key = rectReadKey(this);
    rectReadCounts.set(key, (rectReadCounts.get(key) ?? 0) + 1);
    return domRect(elementRect(this));
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

describe("CenterPanelWorkspace", () => {
  it("uses a 12px pointer sensor and pointer-first collision detection", async () => {
    await renderWorkspace(input());

    expect(container.querySelector("[data-center-panel-workspace]")?.className).toContain(
      "relative flex min-h-0 min-w-0 flex-1 overflow-hidden",
    );
    expect(
      container.querySelector("[data-center-panel-surface-hosts]")?.parentElement?.className,
    ).toContain("z-10");
    expect(harness.sensorCalls[0]?.options).toEqual({ activationConstraint: { distance: 12 } });
    harness.pointerWithin.mockReturnValueOnce([{ id: "pointer" }]);
    expect(handlers().collisionDetection({} as never)).toEqual([{ id: "pointer" }]);
    expect(harness.closestCenter).not.toHaveBeenCalled();

    harness.pointerWithin.mockReturnValueOnce([]);
    harness.closestCenter.mockReturnValueOnce([{ id: "center" }]);
    expect(handlers().collisionDetection({} as never)).toEqual([{ id: "center" }]);
  });

  it("shows a directional preview and dispatches one final recomputed split", async () => {
    const props = input();
    await renderWorkspace(props);
    const start = { x: 300, y: 200 };
    const move = dragMoveOverPane("group-b", start, { x: 699, y: 300 });

    act(() => handlers().onDragStart(pointerDragStart(chatA.id, "group-a", start)));
    act(() => handlers().onDragMove(move));

    const preview = container.querySelector("[data-center-panel-split-preview='right']");
    expect(preview).not.toBeNull();
    expect(preview?.className).toContain("z-30");
    expect(preview?.textContent).toContain("New split: Right");
    expect(container.querySelector("[aria-label='Dragging Dragged']")).not.toBeNull();
    expect(container.querySelector("[data-center-surface-icon='chat']")).not.toBeNull();

    act(() => handlers().onDragEnd(dragEndFrom(move)));
    act(() => handlers().onDragEnd(dragEndFrom(move)));
    expect(props.onDropSurface).toHaveBeenCalledOnce();
    expect(props.onDropSurface).toHaveBeenCalledWith(chatA.id, {
      groupId: "group-b",
      splitDirection: "right",
    });
    expect(container.querySelector("[data-center-panel-split-preview]")).toBeNull();
  });

  it("normalizes a same-group drop before the next tab to preserve its order", async () => {
    const props = input();
    await renderWorkspace(props);
    const start = { x: 150, y: 116 };
    const move = dragOverTab(chatB.id, "group-a", start, { x: 201, y: 116 });

    act(() => handlers().onDragStart(pointerDragStart(chatA.id, "group-a", start)));
    act(() => handlers().onDragEnd(dragEndFrom(move)));

    expect(props.onDropSurface).toHaveBeenCalledWith(chatA.id, {
      groupId: "group-a",
      index: 0,
    });
    const request = props.onDropSurface.mock.calls[0]?.[1];
    if (!request || "splitDirection" in request) throw new Error("Expected an insertion request");
    const result = dropCenterPanelSurface(twoGroupState, chatA.id, request);
    expect(result.changed).toBe(false);
    expect(result.state).toBe(twoGroupState);
  });

  it("normalizes a same-group drop after the next tab to the post-removal index", async () => {
    const props = input();
    await renderWorkspace(props);
    const start = { x: 150, y: 116 };
    const move = dragOverTab(chatB.id, "group-a", start, { x: 299, y: 116 });

    act(() => handlers().onDragStart(pointerDragStart(chatA.id, "group-a", start)));
    act(() => handlers().onDragEnd(dragEndFrom(move)));

    expect(props.onDropSurface).toHaveBeenCalledWith(chatA.id, {
      groupId: "group-a",
      index: 1,
    });
    const request = props.onDropSurface.mock.calls[0]?.[1];
    if (!request || "splitDirection" in request) throw new Error("Expected an insertion request");
    const result = dropCenterPanelSurface(twoGroupState, chatA.id, request);
    expect(result.changed).toBe(true);
    expect(result.state.groups[0]?.surfaceIds).toEqual([chatB.id, chatA.id]);
  });

  it("appends a tab dropped over another pane body", async () => {
    const props = input();
    await renderWorkspace(props);
    const start = { x: 300, y: 200 };
    const move = dragMoveOverPane("group-b", start, { x: 450, y: 300 });

    act(() => handlers().onDragStart(pointerDragStart(chatA.id, "group-a", start)));
    act(() => handlers().onDragEnd(dragEndFrom(move)));

    expect(props.onDropSurface).toHaveBeenCalledWith(chatA.id, { groupId: "group-b" });
  });

  it("clears cancellation and window blur without dispatching", async () => {
    const props = input();
    await renderWorkspace(props);
    const removeEventListener = vi.spyOn(window, "removeEventListener");
    const start = { x: 300, y: 200 };
    const move = dragMoveOverPane("group-b", start, { x: 699, y: 300 });

    act(() => handlers().onDragStart(pointerDragStart(chatA.id, "group-a", start)));
    act(() => handlers().onDragMove(move));
    const dragObserver = resizeObservers.find(
      (observer) =>
        observer.observed.size === 1 &&
        Array.from(observer.observed).some((element) =>
          element.hasAttribute("data-center-panel-workspace"),
        ),
    );
    expect(dragObserver).toBeDefined();
    act(() => handlers().onDragCancel());
    expect(props.onDropSurface).not.toHaveBeenCalled();
    expect(container.querySelector("[data-center-panel-split-preview]")).toBeNull();
    expect(removeEventListener).toHaveBeenCalledWith("blur", expect.any(Function));
    expect(dragObserver?.observed.size).toBe(0);

    act(() => handlers().onDragStart(pointerDragStart(chatA.id, "group-a", start)));
    act(() => handlers().onDragMove(move));
    act(() => window.dispatchEvent(new Event("blur")));
    expect(props.onDropSurface).not.toHaveBeenCalled();
    expect(container.querySelector("[data-center-panel-split-preview]")).toBeNull();
  });

  it("rejects a stale over target when the final pointer is outside its geometry", async () => {
    const props = input();
    await renderWorkspace(props);
    const start = { x: 300, y: 200 };
    const validMove = dragMoveOverPane("group-b", start, { x: 699, y: 300 });
    const outsideEnd = dragMoveOverPane("group-b", start, { x: 900, y: 300 });

    act(() => handlers().onDragStart(pointerDragStart(chatA.id, "group-a", start)));
    act(() => handlers().onDragMove(validMove));
    expect(container.querySelector("[data-center-panel-split-preview]")).not.toBeNull();
    act(() => handlers().onDragEnd(dragEndFrom(outsideEnd)));

    expect(props.onDropSurface).not.toHaveBeenCalled();
    expect(container.querySelector("[data-center-panel-split-preview]")).toBeNull();
  });

  it("hides a cached split preview when current state reaches the four-pane cap", async () => {
    const props = input();
    await renderWorkspace(props);
    const start = { x: 300, y: 200 };
    const move = dragMoveOverPane("group-b", start, { x: 699, y: 300 });

    act(() => handlers().onDragStart(pointerDragStart(chatA.id, "group-a", start)));
    act(() => handlers().onDragMove(move));
    expect(container.querySelector("[data-center-panel-split-preview]")).not.toBeNull();

    await renderWorkspace({ ...props, state: fourGroupState });
    expect(container.querySelector("[data-center-panel-split-preview]")).toBeNull();

    act(() => handlers().onDragEnd(dragEndFrom(move)));
    expect(props.onDropSurface).not.toHaveBeenCalled();
  });

  it("hides a cached split preview after a body-target resize makes its axis infeasible", async () => {
    const props = input();
    await renderWorkspace(props);
    const start = { x: 300, y: 200 };
    const move = dragMoveOverPane("group-b", start, { x: 699, y: 300 });

    act(() => handlers().onDragStart(pointerDragStart(chatA.id, "group-a", start)));
    act(() => handlers().onDragMove(move));
    expect(container.querySelector("[data-center-panel-split-preview]")).not.toBeNull();

    groupBBodyRect = { ...groupBBodyRect, width: 479 };
    const bodyTargetObserver = resizeObservers.find((observer) =>
      Array.from(observer.observed).some(
        (element) => element.getAttribute("data-center-panel-body-target") === "group-b",
      ),
    );
    expect(bodyTargetObserver).toBeDefined();
    act(() => bodyTargetObserver!.trigger());
    await flushFrames();

    expect(container.querySelector("[data-center-panel-split-preview]")).toBeNull();
    act(() => handlers().onDragEnd(dragEndFrom(move)));
    expect(props.onDropSurface).not.toHaveBeenCalled();
  });

  it("suppresses a sole-tab self split and a fifth final pane", async () => {
    for (const state of [soleTabState, fourGroupState]) {
      const props = input(state);
      await renderWorkspace(props);
      const start = { x: 300, y: 200 };
      const move = dragMoveOverPane("group-a", start, { x: 1, y: 300 });

      act(() => handlers().onDragStart(pointerDragStart(chatA.id, "group-a", start)));
      act(() => handlers().onDragMove(move));
      expect(container.querySelector("[data-center-panel-split-preview]")).toBeNull();
      act(() => handlers().onDragEnd(dragEndFrom(move)));
      expect(props.onDropSurface).not.toHaveBeenCalled();
    }
  });

  it("gates native split moves by live body size and store legality", async () => {
    const props = input();
    await renderWorkspace(props);
    const layoutProps = harness.layoutProps as {
      readonly canMoveToSplit: (groupId: string, direction: "left" | "up") => boolean;
      readonly onMoveToSplit: (
        groupId: string,
        surface: typeof chatA,
        direction: "left" | "up",
      ) => void;
    };

    groupABodyRect = { ...groupABodyRect, height: 319 };
    expect(layoutProps.canMoveToSplit("group-a", "left")).toBe(true);
    expect(layoutProps.canMoveToSplit("group-a", "up")).toBe(false);
    act(() => layoutProps.onMoveToSplit("group-a", chatA, "left"));
    expect(props.onDropSurface).toHaveBeenCalledWith(chatA.id, {
      groupId: "group-a",
      splitDirection: "left",
    });

    await renderWorkspace(input(soleTabState));
    const soleLayoutProps = harness.layoutProps as {
      readonly canMoveToSplit: (groupId: string, direction: "left") => boolean;
    };
    expect(soleLayoutProps.canMoveToSplit("group-a", "left")).toBe(false);

    await renderWorkspace(input(fourGroupState));
    const fourGroupLayoutProps = harness.layoutProps as {
      readonly canMoveToSplit: (groupId: string, direction: "left") => boolean;
    };
    expect(fourGroupLayoutProps.canMoveToSplit("group-a", "left")).toBe(false);
  });

  it("exposes current split feasibility through its narrow workspace handle", async () => {
    const workspaceRef = createRef<CenterPanelWorkspaceHandle>();
    const props = input();
    await act(async () => root.render(<CenterPanelWorkspace ref={workspaceRef} {...props} />));
    await flushFrames();

    groupABodyRect = { ...groupABodyRect, width: 480, height: 319 };
    expect(workspaceRef.current?.canSplitGroup("group-a", "right")).toBe(true);
    expect(workspaceRef.current?.canSplitGroup("group-a", "down")).toBe(false);
    expect(workspaceRef.current?.canSplitGroup("missing", "right")).toBe(false);

    await act(async () =>
      root.render(<CenterPanelWorkspace ref={workspaceRef} {...input(fourGroupState)} />),
    );
    await flushFrames();
    expect(workspaceRef.current?.canSplitGroup("group-a", "right")).toBe(false);
  });

  it("snapshots geometry once per drag and recaptures only after workspace bounds change", async () => {
    await renderWorkspace(input());
    const start = { x: 300, y: 200 };
    const move = dragMoveOverPane("group-b", start, { x: 699, y: 300 });

    act(() => handlers().onDragStart(pointerDragStart(chatA.id, "group-a", start)));
    const readsAfterStart = rectReadCounts.get("group-b") ?? 0;
    act(() => handlers().onDragMove(move));
    act(() => handlers().onDragMove(move));
    expect(rectReadCounts.get("group-b") ?? 0).toBe(readsAfterStart);

    const activeObserver = resizeObservers.find(
      (observer) =>
        observer.observed.size === 1 &&
        Array.from(observer.observed).some((element) =>
          element.hasAttribute("data-center-panel-workspace"),
        ),
    );
    expect(activeObserver).toBeDefined();
    act(() => activeObserver!.trigger());
    expect(rectReadCounts.get("group-b") ?? 0).toBe(readsAfterStart);

    workspaceRect = { ...workspaceRect, width: 701 };
    act(() => activeObserver!.trigger());
    expect(rectReadCounts.get("group-b") ?? 0).toBeGreaterThan(readsAfterStart);
  });

  it("does not read workspace or body geometry after capturing the drag snapshot", async () => {
    const props = input();
    await renderWorkspace(props);
    const start = { x: 300, y: 200 };
    const move = dragMoveOverPane("group-b", start, { x: 699, y: 300 });

    act(() => handlers().onDragStart(pointerDragStart(chatA.id, "group-a", start)));
    const workspaceReadsAfterStart = rectReadCounts.get("workspace") ?? 0;
    const bodyReadsAfterStart = rectReadCounts.get("body:group-b") ?? 0;

    act(() => handlers().onDragMove(move));
    act(() => handlers().onDragMove(move));
    act(() => handlers().onDragEnd(dragEndFrom(move)));

    expect(props.onDropSurface).toHaveBeenCalledWith(chatA.id, {
      groupId: "group-b",
      splitDirection: "right",
    });
    expect(rectReadCounts.get("workspace") ?? 0).toBe(workspaceReadsAfterStart);
    expect(rectReadCounts.get("body:group-b") ?? 0).toBe(bodyReadsAfterStart);
  });

  it("refreshes the hovered tab midpoint when its tab strip scrolls during a drag", async () => {
    const props = input();
    await renderWorkspace(props);
    const start = { x: 150, y: 116 };
    const move = dragOverTab(chatC.id, "group-b", start, { x: 300, y: 116 });

    act(() => handlers().onDragStart(pointerDragStart(chatA.id, "group-a", start)));
    act(() => handlers().onDragMove(move));

    tabLeftBySurfaceId.set(chatC.id, 200);
    const viewport = container.querySelector(
      "[data-center-panel-group-id='group-b'] [data-slot='scroll-area-viewport']",
    );
    expect(viewport).not.toBeNull();
    act(() => viewport!.dispatchEvent(new Event("scroll")));
    act(() => handlers().onDragEnd(dragEndFrom(move)));

    expect(props.onDropSurface).toHaveBeenCalledWith(chatA.id, {
      groupId: "group-b",
      index: 1,
    });
  });

  it("keeps the resize-frame callback stable across rerenders and syncs hosts synchronously", async () => {
    const props = input();
    await renderWorkspace(props);
    const first = (harness.layoutProps as { readonly onResizeFrame: () => void }).onResizeFrame;

    await renderWorkspace({
      ...props,
      renderFocusedActions: () => <button type="button">Changed</button>,
    });
    const second = (harness.layoutProps as { readonly onResizeFrame: () => void }).onResizeFrame;
    expect(second).toBe(first);

    first();
    expect(harness.syncRects).toHaveBeenCalledOnce();
  });
});
