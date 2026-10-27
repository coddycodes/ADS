import type { CenterSurface } from "~/centerPanelStore";
import { ThreadId } from "@coddy/contracts";
import React, { type ReactElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";

const harness = vi.hoisted(() => ({
  api: null as null | { contextMenu: { show: ReturnType<typeof vi.fn> } },
  refCurrent: null as null | {
    clientWidth?: number;
    dataset?: Record<string, string>;
    querySelector: ReturnType<typeof vi.fn>;
    querySelectorAll: ReturnType<typeof vi.fn>;
  },
  effects: [] as Array<() => void>,
  animationFrames: [] as FrameRequestCallback[],
  overflowState: false,
  useSortable: vi.fn(() => ({
    attributes: { "data-sortable": true },
    listeners: {},
    setActivatorNodeRef: vi.fn(),
    setNodeRef: vi.fn(),
    isDragging: false,
  })),
}));

vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  return {
    ...actual,
    useCallback: (callback: unknown) => callback,
    useEffect: (effect: () => void) => {
      harness.effects.push(effect);
      effect();
    },
    useRef: () => ({ current: harness.refCurrent }),
    useState: () => [
      harness.overflowState,
      (next: boolean | ((current: boolean) => boolean)) => {
        harness.overflowState = typeof next === "function" ? next(harness.overflowState) : next;
      },
    ],
  };
});
vi.mock("~/localApi", () => ({ readLocalApi: () => harness.api }));
vi.mock("~/components/ui/tooltip", () => ({
  Tooltip: ({ children }: { children: React.ReactNode }) => <>{children}</>,
  TooltipTrigger: ({ render }: { render: React.ReactNode }) => <>{render}</>,
  TooltipPopup: ({ children }: { children: React.ReactNode }) => <span>{children}</span>,
}));
vi.mock("~/components/ui/scroll-area", () => ({
  ScrollArea: ({ children, ...props }: React.ComponentProps<"div">) => (
    <div {...props}>{children}</div>
  ),
}));
vi.mock("~/components/ui/menu", () => ({
  DropdownMenu: ({ children }: { children: React.ReactNode }) => <>{children}</>,
  DropdownMenuTrigger: ({
    children,
    render,
    ...props
  }: React.ComponentProps<"button"> & { render?: React.ReactNode }) =>
    render && React.isValidElement(render) ? (
      React.cloneElement(render, props, children)
    ) : (
      <button {...props}>{children}</button>
    ),
  DropdownMenuContent: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  DropdownMenuItem: (props: React.ComponentProps<"button">) => <button {...props} />,
}));
vi.mock("./CenterHeaderIconButton", () => ({
  CenterHeaderIconButton: ({ children, ...props }: React.ComponentProps<"button">) => (
    <button type="button" data-center-header-icon-control {...props}>
      {children}
    </button>
  ),
}));
vi.mock("@dnd-kit/sortable", () => ({
  useSortable: harness.useSortable,
}));

import { CenterPanelTabs } from "./CenterPanelTabs";

const host = { id: "chat:host", kind: "chat-host" } as const;
const chat = {
  id: "chat:thread-2",
  kind: "chat",
  threadId: ThreadId.make("thread-2"),
  providerLabel: "Claude",
} as const;
const terminal = {
  id: "terminal:terminal-1",
  kind: "terminal",
  terminalId: "terminal-1",
  label: "Codex Terminal",
} as const;

type WheelEventStub = {
  deltaX: number;
  deltaY: number;
  preventDefault: ReturnType<typeof vi.fn>;
};

type KeyboardEventStub = {
  key: string;
  preventDefault: ReturnType<typeof vi.fn>;
};

function props(surfaces: CenterSurface[] = [host, chat, terminal]) {
  return {
    groupId: "group-a",
    hostLabel: "Codex",
    surfaces,
    activeSurfaceId: chat.id,
    terminalLabelsById: new Map([["terminal-1", "Build terminal"]]),
    canMoveToSplit: (_direction: string) => true,
    dragInProgress: false,
    onActivate: vi.fn(),
    onCloseSurface: vi.fn(),
    onCloseOtherSurfaces: vi.fn(),
    onCloseSurfacesToRight: vi.fn(),
    onCloseAllSurfaces: vi.fn(),
    onMoveToSplit: vi.fn(),
  };
}

function visit(node: React.ReactNode, entries: ReactElement[] = []): ReactElement[] {
  if (Array.isArray(node)) {
    for (const child of node) visit(child, entries);
    return entries;
  }
  if (!React.isValidElement(node)) return entries;
  if (typeof node.type === "function") {
    entries.push(node);
    const Component = node.type as unknown as (props: unknown) => React.ReactNode;
    visit(Component(node.props), entries);
    return entries;
  }
  entries.push(node);
  visit((node.props as { children?: React.ReactNode }).children, entries);
  const render = (node.props as { render?: React.ReactNode }).render;
  if (render) visit(render, entries);
  return entries;
}

beforeEach(() => {
  harness.api = null;
  harness.refCurrent = null;
  harness.effects.length = 0;
  harness.animationFrames.length = 0;
  harness.overflowState = false;
  harness.useSortable.mockClear();
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
    harness.animationFrames.push(callback);
    return harness.animationFrames.length;
  });
});

describe("CenterPanelTabs", () => {
  it("names the host from its current provider and preserves other surface labels", () => {
    const scrollIntoView = vi.fn();
    harness.refCurrent = {
      querySelector: vi.fn(() => ({ scrollIntoView })),
      querySelectorAll: vi.fn(),
    };
    const input = props();
    const markup = renderToStaticMarkup(<CenterPanelTabs {...input} />);

    expect(markup).toContain("data-center-panel-overflow-boundary");
    expect(markup).toContain("isolate");
    expect(markup).toContain("Codex");
    expect(markup).not.toContain("Main");
    expect(markup).toContain("Claude");
    expect(markup).toContain("Codex Terminal");
    expect(markup).not.toContain("Build terminal");
    expect(harness.useSortable).toHaveBeenCalledWith(
      expect.objectContaining({
        id: chat.id,
        data: expect.objectContaining({
          type: "center-panel-tab",
          surfaceId: chat.id,
          groupId: "group-a",
          surfaceKind: "chat",
          title: "Claude",
        }),
      }),
    );
    expect(scrollIntoView).toHaveBeenCalledWith({ block: "nearest", inline: "nearest" });

    const { terminalLabelsById: _terminalLabelsById, ...unlabeledProps } = props([
      { id: chat.id, kind: "chat", threadId: chat.threadId },
      { id: terminal.id, kind: terminal.kind, terminalId: terminal.terminalId },
    ]);
    const unlabeled = renderToStaticMarkup(<CenterPanelTabs {...unlabeledProps} />);
    expect(unlabeled).toContain("Chat");
    expect(unlabeled).toContain("Terminal 1");
  });

  it("keeps an accessible tablist and empty state when a pane has no surfaces", () => {
    const markup = renderToStaticMarkup(<CenterPanelTabs {...props([])} />);

    expect(markup).toContain('role="tablist"');
    expect(markup).toContain('aria-label="Workspace panels"');
    expect(markup).toContain("No chat panels open");
  });

  it("translates vertical wheel input only when the tab viewport overflows", () => {
    const viewport = { scrollWidth: 640, clientWidth: 240, scrollLeft: 12 };
    const activeTab = { scrollIntoView: vi.fn() };
    const activationButtons = [
      { focus: vi.fn(), scrollIntoView: vi.fn() },
      { focus: vi.fn(), scrollIntoView: vi.fn() },
      { focus: vi.fn(), scrollIntoView: vi.fn() },
    ];
    harness.refCurrent = {
      querySelector: vi.fn((selector: string) =>
        selector === '[data-slot="scroll-area-viewport"]' ? viewport : activeTab,
      ),
      querySelectorAll: vi.fn(() => activationButtons),
    };
    const input = props();
    const tree = CenterPanelTabs(input);
    const scrollArea = visit(tree).find(
      (element) =>
        (element.props as Record<string, unknown>)["data-center-panel-tab-list"] === true,
    );
    if (!scrollArea) throw new Error("Tab scroll area not found");

    const event: WheelEventStub = { deltaX: 0, deltaY: 48, preventDefault: vi.fn() };
    (scrollArea.props as { onWheel: (event: WheelEventStub) => void }).onWheel(event);

    expect(viewport.scrollLeft).toBe(60);
    expect(event.preventDefault).toHaveBeenCalledOnce();

    viewport.clientWidth = viewport.scrollWidth;
    (scrollArea.props as { onWheel: (event: WheelEventStub) => void }).onWheel(event);
    expect(viewport.scrollLeft).toBe(60);
  });

  it("moves to and reveals the adjacent tab with horizontal arrow keys", () => {
    const viewport = { scrollWidth: 640, clientWidth: 240, scrollLeft: 12 };
    const activeTab = { scrollIntoView: vi.fn() };
    const activationButtons = [
      { focus: vi.fn(), scrollIntoView: vi.fn() },
      { focus: vi.fn(), scrollIntoView: vi.fn() },
      { focus: vi.fn(), scrollIntoView: vi.fn() },
    ];
    harness.refCurrent = {
      querySelector: vi.fn((selector: string) =>
        selector === '[data-slot="scroll-area-viewport"]' ? viewport : activeTab,
      ),
      querySelectorAll: vi.fn(() => activationButtons),
    };
    const input = props();
    const tree = CenterPanelTabs(input);
    const elements = visit(tree);
    const activeButton = elements.find(
      (element) =>
        element.type === "button" &&
        (element.props as Record<string, unknown>)["aria-selected"] === true,
    );
    if (!activeButton) throw new Error("Active tab button not found");

    const event: KeyboardEventStub = { key: "ArrowRight", preventDefault: vi.fn() };
    (activeButton.props as { onKeyDown: (event: KeyboardEventStub) => void }).onKeyDown(event);

    expect(event.preventDefault).toHaveBeenCalledOnce();
    expect(input.onActivate).toHaveBeenCalledWith("group-a", terminal);
    expect(activationButtons[2]?.focus).not.toHaveBeenCalled();
    expect(harness.animationFrames).toHaveLength(1);
    const firstFrame = harness.animationFrames.splice(0);
    for (const callback of firstFrame) callback(0);
    expect(activationButtons[2]?.focus).not.toHaveBeenCalled();
    expect(harness.animationFrames).toHaveLength(1);
    const secondFrame = harness.animationFrames.splice(0);
    for (const callback of secondFrame) callback(16);
    expect(activationButtons[2]?.focus).toHaveBeenCalledOnce();
    expect(activationButtons[2]?.scrollIntoView).toHaveBeenCalledWith({
      block: "nearest",
      inline: "nearest",
    });
  });

  it("shows overflow navigation only for an overflowing rail and pages or jumps to hidden tabs", () => {
    const scrollBy = vi.fn();
    const viewport = {
      scrollWidth: 800,
      clientWidth: 240,
      scrollLeft: 0,
      scrollBy,
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
    };
    const activationButtons = [
      { scrollIntoView: vi.fn() },
      { scrollIntoView: vi.fn() },
      { scrollIntoView: vi.fn() },
    ];
    const boundary = {
      clientWidth: 320,
      dataset: {} as Record<string, string>,
      querySelector: vi.fn((selector: string) =>
        selector === '[data-slot="scroll-area-viewport"]' ? viewport : activationButtons[1],
      ),
      querySelectorAll: vi.fn(() => activationButtons),
    };
    harness.refCurrent = boundary;
    const input = props();
    const tree = CenterPanelTabs(input);
    const elements = visit(tree);

    expect(harness.overflowState).toBe(true);
    const navigator = elements.find(
      (element) =>
        (element.props as Record<string, unknown>)["data-center-panel-overflow-navigator"] === true,
    );
    const navigatorClassName = (navigator?.props as Record<string, unknown> | undefined)?.[
      "className"
    ];
    expect(navigatorClassName).toContain("hidden");
    expect(navigatorClassName).toContain("group-data-[overflow=true]/tabbar:flex");

    const previous = elements.find(
      (element) =>
        element.type === "button" &&
        (element.props as Record<string, unknown>)["aria-label"] === "Previous tabs",
    );
    const next = elements.find(
      (element) =>
        element.type === "button" &&
        (element.props as Record<string, unknown>)["aria-label"] === "Next tabs",
    );
    const allTabs = elements.find(
      (element) =>
        element.type === "button" &&
        (element.props as Record<string, unknown>)["aria-label"] === "All tabs",
    );
    expect(previous).toBeDefined();
    expect(next).toBeDefined();
    expect(allTabs).toBeDefined();
    if (!previous || !next || !allTabs) throw new Error("Overflow controls not found");
    for (const control of [previous, next, allTabs]) {
      expect(
        (control?.props as Record<string, unknown> | undefined)?.[
          "data-center-header-icon-control"
        ],
      ).toBe(true);
    }
    expect(
      renderToStaticMarkup((allTabs.props as { children?: React.ReactNode }).children),
    ).toContain("<svg");
    expect(String(navigatorClassName)).toContain("gap-1");
    expect(String(navigatorClassName)).toContain("border-l");
    expect(String(navigatorClassName)).not.toContain("gap-0.5");
    const allTabsPopup = elements.find(
      (element) =>
        (element.props as Record<string, unknown>)["className"] === "max-h-80 w-64 overflow-y-auto",
    );
    expect(allTabsPopup).toBeDefined();

    (next.props as { onClick: () => void }).onClick();
    (previous.props as { onClick: () => void }).onClick();
    expect(scrollBy).toHaveBeenNthCalledWith(1, { behavior: "smooth", left: 216 });
    expect(scrollBy).toHaveBeenNthCalledWith(2, { behavior: "smooth", left: -216 });

    const terminalJump = elements.find(
      (element) =>
        (element.props as Record<string, unknown>)["data-center-panel-all-tab-id"] === terminal.id,
    );
    expect(terminalJump).toBeDefined();
    if (!terminalJump) throw new Error("All tabs terminal item not found");
    (terminalJump.props as { onClick: () => void }).onClick();
    expect(input.onActivate).toHaveBeenCalledWith("group-a", terminal);
    const frames = harness.animationFrames.splice(0);
    for (const callback of frames) callback(0);
    expect(activationButtons[2]?.scrollIntoView).toHaveBeenCalledWith({
      block: "nearest",
      inline: "nearest",
    });

    const rerendered = CenterPanelTabs({ ...input, activeSurfaceId: terminal.id });
    const rerenderedBoundary = visit(rerendered).find(
      (element) =>
        (element.props as Record<string, unknown>)["data-center-panel-overflow-boundary"] === true,
    );
    expect(
      (rerenderedBoundary?.props as Record<string, unknown> | undefined)?.["data-overflow"],
    ).toBe(true);

    viewport.scrollWidth = 300;
    boundary.clientWidth = 320;
    for (const effect of harness.effects) effect();
    expect(harness.overflowState).toBe(false);
  });

  it("restores adjacent-tab focus after activation rerenders the active chat", () => {
    let focusedElement = "chat";
    const activationButtons = [
      { focus: () => (focusedElement = "host"), scrollIntoView: vi.fn() },
      { focus: () => (focusedElement = "chat"), scrollIntoView: vi.fn() },
      { focus: () => (focusedElement = "terminal"), scrollIntoView: vi.fn() },
    ];
    harness.refCurrent = {
      querySelector: vi.fn(() => ({ scrollIntoView: vi.fn() })),
      querySelectorAll: vi.fn(() => activationButtons),
    };
    const input = props();
    input.onActivate.mockImplementation(() => {
      focusedElement = "composer";
    });
    const tree = CenterPanelTabs(input);
    const activeButton = visit(tree).find(
      (element) =>
        element.type === "button" &&
        (element.props as Record<string, unknown>)["aria-selected"] === true,
    );
    if (!activeButton) throw new Error("Active tab button not found");

    const event: KeyboardEventStub = { key: "ArrowRight", preventDefault: vi.fn() };
    (activeButton.props as { onKeyDown: (event: KeyboardEventStub) => void }).onKeyDown(event);

    expect(focusedElement).toBe("composer");
    expect(harness.animationFrames).toHaveLength(1);
    requestAnimationFrame(() => {
      focusedElement = "composer";
    });
    const firstFrame = harness.animationFrames.splice(0);
    for (const callback of firstFrame) callback(0);
    expect(focusedElement).toBe("composer");
    expect(harness.animationFrames).toHaveLength(1);
    const secondFrame = harness.animationFrames.splice(0);
    for (const callback of secondFrame) callback(16);
    expect(focusedElement).toBe("terminal");
  });

  it("does not activate a tab while a drag is in progress", () => {
    harness.refCurrent = {
      querySelector: vi.fn(() => ({ scrollIntoView: vi.fn() })),
      querySelectorAll: vi.fn(),
    };
    const input = { ...props(), dragInProgress: true };
    const tree = CenterPanelTabs(input);
    const activeButton = visit(tree).find(
      (element) =>
        element.type === "button" &&
        (element.props as Record<string, unknown>)["aria-selected"] === true,
    );
    if (!activeButton) throw new Error("Active tab button not found");

    (activeButton.props as { onClick: () => void }).onClick();

    expect(input.onActivate).not.toHaveBeenCalled();
  });

  it("handles activation, close buttons, middle click, and context-menu actions", async () => {
    const surfaces: CenterSurface[] = [host, chat, terminal];
    const input = props(surfaces);
    const tree = CenterPanelTabs(input);
    const elements = visit(tree);
    const chatTab = elements.find(
      (element) =>
        element.type === "div" &&
        (element.props as Record<string, unknown>)["data-active-tab"] === true,
    );
    if (!chatTab) throw new Error("Active chat tab not found");
    const tabProps = chatTab.props as Record<string, unknown>;
    expect(tabProps["data-center-panel-tab-id"]).toBe(chat.id);
    expect(tabProps["data-center-panel-group-id"]).toBe("group-a");
    expect(tabProps.style).toBeUndefined();

    const mouseEvent = (button: number) => ({
      button,
      clientX: 10,
      clientY: 20,
      preventDefault: vi.fn(),
      stopPropagation: vi.fn(),
    });
    const leftDown = mouseEvent(0);
    (tabProps.onMouseDown as (event: unknown) => void)(leftDown);
    expect(leftDown.preventDefault).not.toHaveBeenCalled();
    const middleDown = mouseEvent(1);
    (tabProps.onMouseDown as (event: unknown) => void)(middleDown);
    expect(middleDown.preventDefault).toHaveBeenCalled();

    const leftAux = mouseEvent(0);
    (tabProps.onAuxClick as (event: unknown) => void)(leftAux);
    expect(input.onCloseSurface).not.toHaveBeenCalled();
    const middleAux = mouseEvent(1);
    (tabProps.onAuxClick as (event: unknown) => void)(middleAux);
    expect(input.onCloseSurface).toHaveBeenCalledWith("group-a", chat);
    expect(middleAux.stopPropagation).toHaveBeenCalled();

    const chatElements = visit(chatTab, []);
    const activate = chatElements.find(
      (element) =>
        element.type === "button" &&
        (element.props as Record<string, unknown>)["data-center-panel-tab-activation"] === true,
    );
    if (!activate) throw new Error("Activate button not found");
    (activate.props as { onClick: () => void }).onClick();
    expect(input.onActivate).toHaveBeenCalledWith("group-a", chat);
    const close = chatElements.find(
      (element) =>
        element.type === "button" &&
        (element.props as Record<string, unknown>)["aria-label"] === "Close Claude",
    );
    if (!close) throw new Error("Close button not found");
    const closePointerDown = mouseEvent(0);
    (close.props as { onPointerDown: (event: typeof closePointerDown) => void }).onPointerDown(
      closePointerDown,
    );
    expect(closePointerDown.stopPropagation).toHaveBeenCalledOnce();
    (close.props as { onClick: () => void }).onClick();
    expect(input.onCloseSurface).toHaveBeenCalledWith("group-a", chat);

    const contextEvent = mouseEvent(0);
    await (tabProps.onContextMenu as (event: unknown) => Promise<void>)(contextEvent);
    expect(contextEvent.preventDefault).toHaveBeenCalled();
    expect(contextEvent.stopPropagation).toHaveBeenCalled();

    const show = vi.fn();
    harness.api = { contextMenu: { show } };
    for (const [action, callback] of [
      ["close", input.onCloseSurface],
      ["close-others", input.onCloseOtherSurfaces],
      ["close-to-right", input.onCloseSurfacesToRight],
      ["close-all", input.onCloseAllSurfaces],
      [null, vi.fn()],
    ] as const) {
      show.mockResolvedValueOnce(action);
      await (tabProps.onContextMenu as (event: unknown) => Promise<void>)(mouseEvent(0));
      if (action === "close-all") expect(callback).toHaveBeenCalledWith("group-a");
      else if (action !== null) expect(callback).toHaveBeenCalledWith("group-a", chat);
    }
    expect(show).toHaveBeenCalledWith(
      expect.arrayContaining([
        expect.objectContaining({
          id: "move-to-split",
          label: "Move Tab to Split",
          disabled: false,
          children: [
            { id: "move-to-split:left", label: "Left", disabled: false },
            { id: "move-to-split:right", label: "Right", disabled: false },
            { id: "move-to-split:up", label: "Up", disabled: false },
            { id: "move-to-split:down", label: "Down", disabled: false },
          ],
        }),
        expect.objectContaining({ id: "close-others", disabled: false }),
        expect.objectContaining({ id: "close-to-right", disabled: false }),
      ]),
      { x: 10, y: 20 },
    );

    show.mockResolvedValueOnce("move-to-split:down");
    await (tabProps.onContextMenu as (event: unknown) => Promise<void>)(mouseEvent(0));
    expect(input.onMoveToSplit).toHaveBeenCalledWith("group-a", chat, "down");

    input.canMoveToSplit = (direction: string) => direction === "left";
    show.mockClear();
    show.mockResolvedValueOnce(null);
    await (tabProps.onContextMenu as (event: unknown) => Promise<void>)(mouseEvent(0));
    expect(show).toHaveBeenCalledWith(
      expect.arrayContaining([
        expect.objectContaining({
          id: "move-to-split",
          disabled: false,
          children: [
            { id: "move-to-split:left", label: "Left", disabled: false },
            { id: "move-to-split:right", label: "Right", disabled: true },
            { id: "move-to-split:up", label: "Up", disabled: true },
            { id: "move-to-split:down", label: "Down", disabled: true },
          ],
        }),
      ]),
      { x: 10, y: 20 },
    );

    input.canMoveToSplit = () => false;
    show.mockClear();
    show.mockResolvedValueOnce(null);
    await (tabProps.onContextMenu as (event: unknown) => Promise<void>)(mouseEvent(0));
    expect(show).toHaveBeenCalledWith(
      expect.arrayContaining([
        expect.objectContaining({
          id: "move-to-split",
          disabled: true,
          children: [
            { id: "move-to-split:left", label: "Left", disabled: true },
            { id: "move-to-split:right", label: "Right", disabled: true },
            { id: "move-to-split:up", label: "Up", disabled: true },
            { id: "move-to-split:down", label: "Down", disabled: true },
          ],
        }),
      ]),
      { x: 10, y: 20 },
    );

    surfaces.splice(1, 1);
    show.mockClear();
    await (tabProps.onContextMenu as (event: unknown) => Promise<void>)(mouseEvent(0));
    expect(show).not.toHaveBeenCalled();
  });
});
