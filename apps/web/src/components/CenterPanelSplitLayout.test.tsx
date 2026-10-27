// @vitest-environment happy-dom

import { ThreadId } from "@coddy/contracts";
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import type { ThreadCenterPanelState } from "~/centerPanelStore";

const harness = vi.hoisted(() => ({
  useDroppable: vi.fn((input: { id: string; data: unknown }) => ({
    isOver: false,
    setNodeRef: (node: HTMLElement | null) => {
      if (node) node.dataset.droppableId = input.id;
    },
  })),
}));

vi.mock("@dnd-kit/core", () => ({ useDroppable: harness.useDroppable }));
vi.mock("./CenterPanelTabs", () => ({
  CenterPanelTabs: (props: { readonly groupId: string }) => (
    <div role="tablist" aria-label={`Tabs for ${props.groupId}`} />
  ),
}));
vi.mock("~/components/ui/menu", () => ({
  Menu: ({ children }: { readonly children: ReactNode }) => <div>{children}</div>,
  MenuTrigger: (props: React.ComponentProps<"button">) => <button type="button" {...props} />,
  MenuPopup: ({ children }: { readonly children: ReactNode }) => <div>{children}</div>,
  MenuItem: (props: React.ComponentProps<"button">) => <button type="button" {...props} />,
}));

import { CenterPanelSplitLayout, type CenterPanelSplitLayoutProps } from "./CenterPanelSplitLayout";

const host = { id: "chat:host", kind: "chat-host" } as const;
const chat = {
  id: "chat:thread-b",
  kind: "chat",
  threadId: ThreadId.make("thread-b"),
  providerLabel: "Claude",
} as const;
const terminal = {
  id: "terminal:term-c",
  kind: "terminal",
  terminalId: "term-c",
  label: "Build",
} as const;

const threeLeafState: ThreadCenterPanelState = {
  surfaces: [host, chat, terminal],
  groups: [
    { id: "group-a", surfaceIds: [host.id], activeSurfaceId: host.id },
    { id: "group-b", surfaceIds: [chat.id], activeSurfaceId: chat.id },
    { id: "group-c", surfaceIds: [terminal.id], activeSurfaceId: terminal.id },
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
      second: { type: "leaf", groupId: "group-c" },
    },
  },
  focusedGroupId: "group-b",
};

const singleLeafState: ThreadCenterPanelState = {
  surfaces: [host],
  groups: [{ id: "group-a", surfaceIds: [host.id], activeSurfaceId: host.id }],
  layout: { type: "leaf", groupId: "group-a" },
  focusedGroupId: "group-a",
};

let axisWidth = 1_000;
let axisHeight = 1_000;
let container: HTMLDivElement;
let root: Root;
let resizeObservers: FakeResizeObserver[];

class FakeResizeObserver {
  readonly observed = new Set<Element>();

  constructor(readonly callback: ResizeObserverCallback) {
    resizeObservers.push(this);
  }

  observe(target: Element): void {
    this.observed.add(target);
  }

  disconnect(): void {
    this.observed.clear();
  }

  trigger(): void {
    this.callback([], this as unknown as ResizeObserver);
  }
}

function emitHeaderWidth(width: number): void {
  const observer = resizeObservers.find((candidate) =>
    [...candidate.observed].some(
      (element) =>
        element.hasAttribute("data-center-panel-group-header") &&
        element.closest("[data-center-panel-group]")?.getAttribute("data-focused") === "true",
    ),
  );
  if (!observer) throw new Error("Center header ResizeObserver was not installed");
  act(() => {
    observer.callback(
      [{ contentRect: { width } as DOMRectReadOnly } as ResizeObserverEntry],
      observer as unknown as ResizeObserver,
    );
  });
}

const renderFocusedActions: CenterPanelSplitLayoutProps["renderFocusedActions"] = (density) => (
  <button type="button" data-density={density}>
    New panel
  </button>
);

function input(state: ThreadCenterPanelState = threeLeafState): CenterPanelSplitLayoutProps {
  return {
    state,
    hostLabel: "Codex",
    terminalLabelsById: new Map([["term-c", "Build terminal"]]),
    dragInProgress: false,
    renderFocusedActions,
    registerBodyTarget: (groupId) => (node) => {
      if (node) node.dataset.registeredBodyTarget = groupId;
    },
    onResizeFrame: vi.fn(),
    onFocusGroup: vi.fn(),
    onActivate: vi.fn(),
    onCloseSurface: vi.fn(),
    onCloseOtherSurfaces: vi.fn(),
    onCloseSurfacesToRight: vi.fn(),
    onCloseAllSurfaces: vi.fn(),
    canMoveToSplit: vi.fn(() => true),
    onMoveToSplit: vi.fn(),
    onMergeGroup: vi.fn(),
    onSetSplitRatio: vi.fn(),
  };
}

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  axisWidth = 1_000;
  axisHeight = 1_000;
  resizeObservers = [];
  harness.useDroppable.mockClear();
  vi.stubGlobal("ResizeObserver", FakeResizeObserver);
  vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockImplementation(function (
    this: HTMLElement,
  ) {
    return this.hasAttribute("data-center-panel-split") ? axisWidth : 0;
  });
  vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockImplementation(function (
    this: HTMLElement,
  ) {
    return this.hasAttribute("data-center-panel-split") ? axisHeight : 0;
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

async function renderLayout(props: CenterPanelSplitLayoutProps): Promise<void> {
  await act(async () => root.render(<CenterPanelSplitLayout {...props} />));
}

function splitAt(path: string): HTMLDivElement {
  const split = container.querySelector<HTMLDivElement>(`[data-center-panel-split-path="${path}"]`);
  expect(split).not.toBeNull();
  return split!;
}

function separatorAt(path: string): HTMLDivElement {
  const separator = splitAt(path).querySelector<HTMLDivElement>(":scope > [role='separator']");
  expect(separator).not.toBeNull();
  Object.defineProperties(separator!, {
    setPointerCapture: { configurable: true, value: vi.fn() },
    hasPointerCapture: { configurable: true, value: vi.fn(() => true) },
    releasePointerCapture: { configurable: true, value: vi.fn() },
  });
  return separator!;
}

function dispatchPointer(target: Element, type: string, coordinate: number, pointerId = 7): void {
  const event = new Event(type, { bubbles: true });
  Object.defineProperties(event, {
    pointerId: { value: pointerId },
    button: { value: 0 },
    clientX: { value: coordinate },
    clientY: { value: coordinate },
  });
  target.dispatchEvent(event);
}

function dispatchKey(target: Element, key: string): void {
  target.dispatchEvent(new KeyboardEvent("keydown", { bubbles: true, key }));
}

describe("CenterPanelSplitLayout", () => {
  it("shows orange separator lines at rest in both split directions", async () => {
    await renderLayout(input());

    for (const path of ["root", "second"]) {
      const line = separatorAt(path).querySelector<HTMLElement>(":scope > [aria-hidden='true']");
      expect(line?.classList.contains("bg-panel-separator")).toBe(true);
    }
  });

  it("uses the same framing for focused and unfocused panes", async () => {
    await renderLayout(input());

    const focusedPane = container.querySelector<HTMLElement>(
      '[data-center-panel-group][data-focused="true"]',
    );
    const unfocusedPane = container.querySelector<HTMLElement>(
      '[data-center-panel-group][data-focused="false"]',
    );

    expect(focusedPane).not.toBeNull();
    expect(unfocusedPane).not.toBeNull();
    expect(focusedPane?.className).toBe(unfocusedPane?.className);
    expect(focusedPane?.className).not.toContain("data-[focused=true]:after:ring");
    expect(focusedPane?.className).toContain("focus-visible:after:ring-2");
    expect(focusedPane?.className).toContain("focus-visible:after:ring-border");
    expect(focusedPane?.className).not.toContain("focus-visible:after:ring-ring");
    expect(container.querySelectorAll("[data-center-panel-focused-actions]")).toHaveLength(1);
  });

  it("renders compact actions for a narrow focused pane", async () => {
    await renderLayout(input());
    emitHeaderWidth(420);
    const compactAction = container.querySelector("[data-density='compact']");
    const paneMenu = container.querySelector('[aria-label="Pane actions"]');
    expect(compactAction).not.toBeNull();
    expect(paneMenu?.compareDocumentPosition(compactAction!) ?? 0).toBe(
      Node.DOCUMENT_POSITION_FOLLOWING,
    );
  });

  it("renders expanded actions for a wide focused pane", async () => {
    await renderLayout(input());
    emitHeaderWidth(800);
    const expandedAction = container.querySelector("[data-density='expanded']");
    const paneMenu = container.querySelector('[aria-label="Pane actions"]');
    expect(expandedAction).not.toBeNull();
    expect(paneMenu?.compareDocumentPosition(expandedAction!) ?? 0).toBe(
      Node.DOCUMENT_POSITION_FOLLOWING,
    );
  });

  it("keeps the top-right titlebar reservation after every pane action", async () => {
    const topRightActions = (
      <div data-chat-header-actions className="pr-16">
        Header actions
      </div>
    );
    await renderLayout({
      ...input(threeLeafState),
      renderFocusedActions: () => topRightActions,
    });

    const topRightCluster = container.querySelector<HTMLElement>(
      "[data-center-panel-focused-actions]",
    );
    const topRightMenu = topRightCluster?.querySelector('[aria-label="Pane actions"]');
    const topRightHeaderActions = topRightCluster?.querySelector("[data-chat-header-actions]");
    expect(topRightCluster?.dataset.touchesTopRight).toBe("true");
    expect(topRightHeaderActions?.classList.contains("pr-16")).toBe(true);
    expect(topRightCluster?.lastElementChild).toBe(topRightHeaderActions);
    expect(topRightMenu?.compareDocumentPosition(topRightHeaderActions!) ?? 0).toBe(
      Node.DOCUMENT_POSITION_FOLLOWING,
    );

    const lowerRightState = { ...threeLeafState, focusedGroupId: "group-c" };
    await renderLayout({
      ...input(lowerRightState),
      renderFocusedActions: () => <div data-chat-header-actions className="pr-0" />,
    });
    const lowerRightCluster = container.querySelector<HTMLElement>(
      "[data-center-panel-focused-actions]",
    );
    expect(lowerRightCluster?.dataset.touchesTopRight).toBe("false");
    expect(
      lowerRightCluster?.querySelector("[data-chat-header-actions]")?.classList.contains("pr-16"),
    ).toBe(false);

    const topLeftState = { ...threeLeafState, focusedGroupId: "group-a" };
    await renderLayout({
      ...input(topLeftState),
      renderFocusedActions: () => <div data-chat-header-actions className="pr-0" />,
    });
    const topLeftCluster = container.querySelector<HTMLElement>(
      "[data-center-panel-focused-actions]",
    );
    expect(topLeftCluster?.dataset.touchesTopRight).toBe("false");
    expect(
      topLeftCluster?.querySelector("[data-chat-header-actions]")?.classList.contains("pr-16"),
    ).toBe(false);
  });

  it("renders recursive group chrome, targets, edge-aware headers, and focused actions", async () => {
    const props = input();
    await renderLayout(props);

    expect(container.querySelectorAll("[data-center-panel-group]")).toHaveLength(3);
    expect(container.querySelectorAll('[role="tablist"]')).toHaveLength(3);
    expect(container.querySelectorAll('[role="region"][aria-label^="Center pane"]')).toHaveLength(
      3,
    );
    expect(container.querySelector('[aria-label="Center pane 2: Claude"]')).not.toBeNull();
    expect(container.querySelectorAll("[data-center-panel-focused-actions]")).toHaveLength(1);
    expect(
      container
        .querySelector("[data-center-panel-focused-actions]")
        ?.closest("[data-center-panel-group]")
        ?.getAttribute("data-center-panel-group-id"),
    ).toBe("group-b");
    expect(container.querySelectorAll(".workspace-topbar")).toHaveLength(2);
    expect(
      container.querySelector('[data-center-panel-group-id="group-c"] .workspace-topbar'),
    ).toBeNull();
    expect(
      container.querySelector('[data-center-panel-group-id="group-a"] header')?.className,
    ).toContain("workspace-titlebar-content-left");

    for (const groupId of ["group-a", "group-b", "group-c"]) {
      expect(
        container
          .querySelector<HTMLElement>(`[data-center-panel-group-id="${groupId}"] header`)
          ?.classList.contains("@container/header-actions"),
      ).toBe(true);
      const target = container.querySelector<HTMLElement>(
        `[data-center-panel-body-target="${groupId}"]`,
      );
      expect(target?.dataset.registeredBodyTarget).toBe(groupId);
      expect(target?.dataset.droppableId).toBe(`center-pane:${groupId}`);
    }
    expect(harness.useDroppable).toHaveBeenCalledWith({
      id: "center-pane:group-b",
      data: { type: "center-panel-pane", groupId: "group-b" },
    });

    const groupB = container.querySelector<HTMLElement>('[data-center-panel-group-id="group-b"]')!;
    dispatchPointer(groupB, "pointerdown", 0);
    expect(props.onFocusGroup).not.toHaveBeenCalled();

    const groupC = container.querySelector<HTMLElement>('[data-center-panel-group-id="group-c"]')!;
    dispatchPointer(groupC, "pointerdown", 0);
    expect(props.onFocusGroup).toHaveBeenCalledWith("group-c");

    const closeSplit = Array.from(container.querySelectorAll<HTMLButtonElement>("button")).find(
      (button) => button.textContent === "Close Split Pane",
    );
    expect(closeSplit).toBeDefined();
    closeSplit!.click();
    expect(props.onMergeGroup).toHaveBeenCalledWith("group-b");
  });

  it("omits Close Split Pane when the layout has one group", async () => {
    await renderLayout(input(singleLeafState));

    expect(container.textContent).not.toContain("Close Split Pane");
  });

  it("resizes immediate children imperatively and commits the root path once", async () => {
    const state: ThreadCenterPanelState = {
      ...threeLeafState,
      layout: {
        type: "split",
        direction: "horizontal",
        ratio: 0.5,
        first: { type: "leaf", groupId: "group-a" },
        second: { type: "leaf", groupId: "group-b" },
      },
      groups: threeLeafState.groups.slice(0, 2),
    };
    const props = input(state);
    await renderLayout(props);
    vi.mocked(props.onResizeFrame).mockClear();
    const split = splitAt("root");
    const separator = separatorAt("root");
    const first = split.querySelector<HTMLElement>(
      ":scope > [data-center-panel-split-child='first']",
    )!;
    const second = split.querySelector<HTMLElement>(
      ":scope > [data-center-panel-split-child='second']",
    )!;

    expect(separator.getAttribute("aria-orientation")).toBe("vertical");
    expect(separator.getAttribute("aria-valuemin")).toBe("15");
    expect(separator.getAttribute("aria-valuemax")).toBe("85");
    expect(separator.getAttribute("aria-valuenow")).toBe("50");

    dispatchPointer(separator, "pointerdown", 500);
    dispatchPointer(separator, "pointermove", 650);
    expect(first.style.flexBasis).toBe("65%");
    expect(second.style.flexBasis).toBe("35%");
    expect(props.onResizeFrame).toHaveBeenCalled();
    expect(props.onSetSplitRatio).not.toHaveBeenCalled();

    dispatchPointer(separator, "pointerup", 650);
    dispatchPointer(separator, "lostpointercapture", 650);
    expect(props.onSetSplitRatio).toHaveBeenCalledOnce();
    expect(props.onSetSplitRatio).toHaveBeenCalledWith([], 0.65);
  });

  it("accumulates consecutive keyboard resize steps before a parent rerender", async () => {
    const props = input({
      ...threeLeafState,
      groups: threeLeafState.groups.slice(0, 2),
      layout: {
        type: "split",
        direction: "horizontal",
        ratio: 0.5,
        first: { type: "leaf", groupId: "group-a" },
        second: { type: "leaf", groupId: "group-b" },
      },
    });
    await renderLayout(props);
    const split = splitAt("root");
    const separator = separatorAt("root");
    const first = split.querySelector<HTMLElement>(
      ":scope > [data-center-panel-split-child='first']",
    )!;

    dispatchKey(separator, "ArrowRight");
    expect(props.onSetSplitRatio).toHaveBeenLastCalledWith([], 0.55);
    dispatchKey(separator, "ArrowRight");
    expect(props.onSetSplitRatio).toHaveBeenLastCalledWith([], 0.6);
    expect(first.style.flexBasis).toBe("60%");
    expect(separator.getAttribute("aria-valuenow")).toBe("60");
  });

  it("commits pointer cancellation and lost capture once using the nested layout path", async () => {
    const props = input();
    await renderLayout(props);
    const separator = separatorAt("second");

    dispatchPointer(separator, "pointerdown", 500);
    dispatchPointer(separator, "pointermove", 600);
    dispatchPointer(separator, "pointercancel", 600);
    dispatchPointer(separator, "lostpointercapture", 600);
    expect(props.onSetSplitRatio).toHaveBeenCalledOnce();
    expect(props.onSetSplitRatio).toHaveBeenCalledWith(["second"], 0.6);

    vi.mocked(props.onSetSplitRatio).mockClear();
    dispatchPointer(separator, "pointerdown", 500, 8);
    dispatchPointer(separator, "pointermove", 550, 8);
    dispatchPointer(separator, "lostpointercapture", 550, 8);
    dispatchPointer(separator, "pointerup", 550, 8);
    expect(props.onSetSplitRatio).toHaveBeenCalledOnce();
    expect(props.onSetSplitRatio).toHaveBeenCalledWith(["second"], 0.65);
  });

  it("keeps an active resize alive when the workspace rerenders", async () => {
    const state: ThreadCenterPanelState = {
      ...threeLeafState,
      groups: threeLeafState.groups.slice(0, 2),
      layout: {
        type: "split",
        direction: "horizontal",
        ratio: 0.5,
        first: { type: "leaf", groupId: "group-a" },
        second: { type: "leaf", groupId: "group-b" },
      },
    };
    const props = input(state);
    await renderLayout(props);
    const separator = separatorAt("root");

    dispatchPointer(separator, "pointerdown", 500);
    dispatchPointer(separator, "pointermove", 600);
    await act(async () =>
      root.render(<CenterPanelSplitLayout {...props} onResizeFrame={vi.fn()} />),
    );
    dispatchPointer(separator, "pointermove", 650);
    dispatchPointer(separator, "pointerup", 650);

    expect(props.onSetSplitRatio).toHaveBeenCalledOnce();
    expect(props.onSetSplitRatio).toHaveBeenCalledWith([], 0.65);
  });

  it("aborts cleanly when pointer capture is unavailable and permits a later gesture", async () => {
    const state: ThreadCenterPanelState = {
      ...threeLeafState,
      groups: threeLeafState.groups.slice(0, 2),
      layout: {
        type: "split",
        direction: "horizontal",
        ratio: 0.5,
        first: { type: "leaf", groupId: "group-a" },
        second: { type: "leaf", groupId: "group-b" },
      },
    };
    const props = input(state);
    await renderLayout(props);
    const separator = separatorAt("root");
    Object.defineProperty(separator, "setPointerCapture", {
      configurable: true,
      value: vi.fn(() => {
        throw new Error("capture unavailable");
      }),
    });

    dispatchPointer(separator, "pointerdown", 500);
    dispatchPointer(separator, "pointermove", 700);
    dispatchPointer(separator, "pointerup", 700);
    expect(props.onSetSplitRatio).not.toHaveBeenCalled();

    Object.defineProperty(separator, "setPointerCapture", {
      configurable: true,
      value: vi.fn(),
    });
    dispatchPointer(separator, "pointerdown", 500, 8);
    dispatchPointer(separator, "pointermove", 600, 8);
    dispatchPointer(separator, "pointerup", 600, 8);
    expect(props.onSetSplitRatio).toHaveBeenCalledOnce();
    expect(props.onSetSplitRatio).toHaveBeenCalledWith([], 0.6);
  });

  it("clamps persisted ratios for rendering without rewriting store state", async () => {
    const horizontalState: ThreadCenterPanelState = {
      ...threeLeafState,
      groups: threeLeafState.groups.slice(0, 2),
      layout: {
        type: "split",
        direction: "horizontal",
        ratio: 0.15,
        first: { type: "leaf", groupId: "group-a" },
        second: { type: "leaf", groupId: "group-b" },
      },
    };
    const props = input(horizontalState);
    await renderLayout(props);
    const horizontal = splitAt("root");
    const first = horizontal.querySelector<HTMLElement>(
      ":scope > [data-center-panel-split-child='first']",
    )!;
    const second = horizontal.querySelector<HTMLElement>(
      ":scope > [data-center-panel-split-child='second']",
    )!;
    const observer = resizeObservers.find((entry) => entry.observed.has(horizontal));
    expect(observer).toBeDefined();

    axisWidth = 1_000;
    observer!.trigger();
    expect(first.style.flexBasis).toBe("24%");
    expect(second.style.flexBasis).toBe("76%");

    axisWidth = 400;
    observer!.trigger();
    expect(first.style.flexBasis).toBe("50%");
    expect(second.style.flexBasis).toBe("50%");
    expect(props.onSetSplitRatio).not.toHaveBeenCalled();
  });

  it("does not persist a responsive clamp on a click and keyboards from the displayed ratio", async () => {
    const props = input({
      ...threeLeafState,
      groups: threeLeafState.groups.slice(0, 2),
      layout: {
        type: "split",
        direction: "horizontal",
        ratio: 0.15,
        first: { type: "leaf", groupId: "group-a" },
        second: { type: "leaf", groupId: "group-b" },
      },
    });
    await renderLayout(props);
    const separator = separatorAt("root");
    expect(separator.getAttribute("aria-valuenow")).toBe("24");

    dispatchPointer(separator, "pointerdown", 500);
    dispatchPointer(separator, "pointerup", 500);
    expect(props.onSetSplitRatio).not.toHaveBeenCalled();

    dispatchKey(separator, "ArrowRight");
    expect(props.onSetSplitRatio).toHaveBeenCalledOnce();
    expect(props.onSetSplitRatio).toHaveBeenCalledWith([], 0.29);
  });

  it("persists pointer resizing only when the divider moves from a responsive clamp", async () => {
    const props = input({
      ...threeLeafState,
      groups: threeLeafState.groups.slice(0, 2),
      layout: {
        type: "split",
        direction: "horizontal",
        ratio: 0.15,
        first: { type: "leaf", groupId: "group-a" },
        second: { type: "leaf", groupId: "group-b" },
      },
    });
    await renderLayout(props);
    const separator = separatorAt("root");

    expect(separator.getAttribute("aria-valuenow")).toBe("24");
    dispatchPointer(separator, "pointerdown", 500);
    dispatchPointer(separator, "pointermove", 400);
    expect(separator.getAttribute("aria-valuenow")).toBe("24");
    dispatchPointer(separator, "pointerup", 400);

    expect(props.onSetSplitRatio).not.toHaveBeenCalled();

    dispatchPointer(separator, "pointerdown", 500, 8);
    dispatchPointer(separator, "pointermove", 550, 8);
    dispatchPointer(separator, "pointerup", 550, 8);

    expect(props.onSetSplitRatio).toHaveBeenCalledOnce();
    expect(props.onSetSplitRatio).toHaveBeenCalledWith([], 0.29);
  });

  it("does not persist a keyboard step further into a non-round responsive clamp", async () => {
    axisWidth = 700;
    const props = input({
      ...threeLeafState,
      groups: threeLeafState.groups.slice(0, 2),
      layout: {
        type: "split",
        direction: "horizontal",
        ratio: 0.15,
        first: { type: "leaf", groupId: "group-a" },
        second: { type: "leaf", groupId: "group-b" },
      },
    });
    await renderLayout(props);
    const separator = separatorAt("root");

    expect(separator.getAttribute("aria-valuenow")).toBe("34");
    dispatchKey(separator, "ArrowLeft");

    expect(separator.getAttribute("aria-valuenow")).toBe("34");
    expect(props.onSetSplitRatio).not.toHaveBeenCalled();
  });

  it("does not persist pointer or keyboard no-ops in an undersized split", async () => {
    axisWidth = 400;
    const props = input({
      ...threeLeafState,
      groups: threeLeafState.groups.slice(0, 2),
      layout: {
        type: "split",
        direction: "horizontal",
        ratio: 0.15,
        first: { type: "leaf", groupId: "group-a" },
        second: { type: "leaf", groupId: "group-b" },
      },
    });
    await renderLayout(props);
    const separator = separatorAt("root");

    expect(separator.getAttribute("aria-valuenow")).toBe("50");
    dispatchPointer(separator, "pointerdown", 200);
    dispatchPointer(separator, "pointermove", 300);
    dispatchPointer(separator, "pointerup", 300);
    dispatchKey(separator, "ArrowRight");

    expect(separator.getAttribute("aria-valuenow")).toBe("50");
    expect(props.onSetSplitRatio).not.toHaveBeenCalled();
  });

  it("uses the 160-pixel vertical minimum and ignores zero-sized resize gestures", async () => {
    const props = input({
      ...threeLeafState,
      groups: threeLeafState.groups.slice(0, 2),
      layout: {
        type: "split",
        direction: "vertical",
        ratio: 0.15,
        first: { type: "leaf", groupId: "group-a" },
        second: { type: "leaf", groupId: "group-b" },
      },
    });
    await renderLayout(props);
    const split = splitAt("root");
    const first = split.querySelector<HTMLElement>(
      ":scope > [data-center-panel-split-child='first']",
    )!;
    const second = split.querySelector<HTMLElement>(
      ":scope > [data-center-panel-split-child='second']",
    )!;
    const observer = resizeObservers.find((entry) => entry.observed.has(split));

    axisHeight = 1_000;
    observer!.trigger();
    expect(first.style.flexBasis).toBe("16%");
    expect(second.style.flexBasis).toBe("84%");
    expect(split.querySelector('[role="separator"]')?.getAttribute("aria-orientation")).toBe(
      "horizontal",
    );

    axisHeight = 0;
    const separator = separatorAt("root");
    dispatchPointer(separator, "pointerdown", 500);
    dispatchPointer(separator, "pointermove", 700);
    dispatchPointer(separator, "pointerup", 700);
    expect(props.onSetSplitRatio).not.toHaveBeenCalled();
  });
});
