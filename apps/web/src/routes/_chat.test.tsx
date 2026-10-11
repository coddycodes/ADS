import { renderToStaticMarkup } from "react-dom/server";
import { createElement, type ComponentType } from "react";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";

const harness = vi.hoisted(() => ({
  routeConfig: null as Record<string, unknown> | null,
  effects: [] as Array<() => void | (() => void)>,
  addEventListener: vi.fn(),
  removeEventListener: vi.fn(),
  selectedSize: 0,
  clearSelection: vi.fn(),
  routeThreadRef: null as Record<string, unknown> | null,
  activeDraftThread: { draft: true } as unknown,
  activeThread: null as unknown,
  defaultProjectRef: { project: true } as unknown,
  handleNewThread: vi.fn(),
  terminalSurfaceOpen: false,
  previewPanel: null as string | null,
  command: null as string | null,
  paletteOpen: false,
  terminalFocused: false,
  previewFocused: false,
  previewSupported: true,
  pageTools: true,
  startLocal: vi.fn(),
  startThread: vi.fn(),
  dispatchPreview: vi.fn(),
  toastAdd: vi.fn(),
  stackedToast: vi.fn((value: unknown) => value),
  redirect: vi.fn((value: unknown) => ({ redirect: value })),
  resolveCommand: vi.fn(),
}));

vi.mock("react", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react")>()),
  useEffect: (effect: () => void | (() => void)) => harness.effects.push(effect),
}));
vi.mock("@tanstack/react-router", () => ({
  Outlet: () => <div data-outlet />,
  createFileRoute: () => (config: Record<string, unknown>) => {
    harness.routeConfig = config;
    return config;
  },
  redirect: (value: unknown) => harness.redirect(value),
}));
vi.mock("@effect/atom-react", () => ({ useAtomValue: () => [] }));
vi.mock("../commandPaletteContext", () => ({
  isCommandPaletteOpen: () => harness.paletteOpen,
}));
vi.mock("../components/preview/previewActionBus", () => ({
  dispatchPreviewAction: (action: unknown) => harness.dispatchPreview(action),
}));
vi.mock("../hooks/useHandleNewThread", () => ({
  useHandleNewThread: () => ({
    activeDraftThread: harness.activeDraftThread,
    activeThread: harness.activeThread,
    defaultProjectRef: harness.defaultProjectRef,
    handleNewThread: harness.handleNewThread,
    routeThreadRef: harness.routeThreadRef,
  }),
}));
vi.mock("../lib/chatThreadActions", () => ({
  startNewLocalThreadFromContext: (value: unknown) => harness.startLocal(value),
  startNewThreadFromContext: (value: unknown) => harness.startThread(value),
}));
vi.mock("../lib/previewFocus", () => ({
  isPreviewFocused: () => harness.previewFocused,
}));
vi.mock("../lib/terminalFocus", () => ({
  isTerminalFocused: () => harness.terminalFocused,
}));
vi.mock("../keybindings", () => ({
  resolveShortcutCommand: (...args: unknown[]) => {
    harness.resolveCommand(...args);
    return harness.command;
  },
}));
vi.mock("../terminalSurfaceState", () => ({
  useThreadHasTerminalSurface: () => harness.terminalSurfaceOpen,
}));
vi.mock("../previewRuntimeCapabilities", () => ({
  supportsPreviewRuntimeCapability: (_bridge: unknown, capability: string) =>
    capability !== "pageTools" || harness.pageTools,
}));
vi.mock("../previewStateStore", () => ({
  isPreviewSupportedInRuntime: () => harness.previewSupported,
  canPreviewAnyUrlInRuntime: () => harness.previewSupported,
}));
vi.mock("../rightPanelStore", () => ({
  selectActiveRightPanel: () => harness.previewPanel,
  useRightPanelStore: (selector: (state: unknown) => unknown) => selector({ byThreadKey: {} }),
}));
vi.mock("../threadSelectionStore", () => ({
  useThreadSelectionStore: (selector: (state: unknown) => unknown) =>
    selector({
      clearSelection: harness.clearSelection,
      selectedThreadKeys: { size: harness.selectedSize },
    }),
}));
vi.mock("~/components/ui/toast", () => ({
  toastManager: { add: harness.toastAdd },
  stackedThreadToast: harness.stackedToast,
}));
vi.mock("~/state/server", () => ({ primaryServerKeybindingsAtom: {} }));

import { Route } from "./_chat";

function renderRoute(): void {
  harness.effects.length = 0;
  void Route;
  const component = harness.routeConfig?.component;
  if (typeof component !== "function") throw new Error("Missing route component");
  renderToStaticMarkup(createElement(component as ComponentType));
}

function installHandler(): (event: Record<string, unknown>) => void {
  renderRoute();
  const cleanup = harness.effects[0]?.();
  expect(harness.addEventListener).toHaveBeenCalledWith("keydown", expect.any(Function));
  const handler = harness.addEventListener.mock.calls.at(-1)?.[1];
  if (typeof handler !== "function") throw new Error("Missing keydown handler");
  if (typeof cleanup === "function") cleanup();
  return handler;
}

function keyboardEvent(overrides: Record<string, unknown> = {}) {
  return {
    key: "k",
    defaultPrevented: false,
    preventDefault: vi.fn(),
    stopPropagation: vi.fn(),
    ...overrides,
  };
}

beforeEach(() => {
  harness.effects.length = 0;
  harness.addEventListener.mockReset();
  harness.removeEventListener.mockReset();
  vi.stubGlobal("window", {
    addEventListener: harness.addEventListener,
    removeEventListener: harness.removeEventListener,
  });
  harness.selectedSize = 0;
  harness.clearSelection.mockReset();
  harness.routeThreadRef = { environmentId: "environment-1", threadId: "thread-1" };
  harness.activeDraftThread = { draft: true };
  harness.activeThread = null;
  harness.defaultProjectRef = { project: true };
  harness.handleNewThread.mockReset();
  harness.terminalSurfaceOpen = false;
  harness.previewPanel = null;
  harness.command = null;
  harness.paletteOpen = false;
  harness.terminalFocused = false;
  harness.previewFocused = false;
  harness.previewSupported = true;
  harness.startLocal.mockReset();
  harness.startThread.mockReset();
  harness.dispatchPreview.mockReset();
  harness.toastAdd.mockReset();
  harness.stackedToast.mockClear();
  harness.redirect.mockClear();
  harness.resolveCommand.mockClear();
});

describe("_chat route", () => {
  it("guards unauthenticated route loads", async () => {
    const beforeLoad = harness.routeConfig?.beforeLoad;
    if (typeof beforeLoad !== "function") throw new Error("Missing beforeLoad");
    await expect(
      beforeLoad({ context: { authGateState: { status: "authenticated" } } }),
    ).resolves.toBeUndefined();
    await expect(
      beforeLoad({ context: { authGateState: { status: "hosted-static" } } }),
    ).resolves.toBeUndefined();
    await expect(beforeLoad({ context: { authGateState: { status: "pairing" } } })).rejects.toEqual(
      { redirect: { to: "/pair", replace: true } },
    );
  });

  it("ignores handled events and command-palette shortcuts", () => {
    const handler = installHandler();
    handler(keyboardEvent({ defaultPrevented: true }));
    expect(harness.resolveCommand).not.toHaveBeenCalled();
    harness.paletteOpen = true;
    handler(keyboardEvent());
    expect(harness.resolveCommand).toHaveBeenCalledOnce();
    expect(harness.dispatchPreview).not.toHaveBeenCalled();
    expect(harness.removeEventListener).toHaveBeenCalledWith("keydown", handler);
  });

  it("clears multi-selection on Escape", () => {
    harness.selectedSize = 2;
    const handler = installHandler();
    const event = keyboardEvent({ key: "Escape" });
    handler(event);
    expect(event.preventDefault).toHaveBeenCalledOnce();
    expect(harness.clearSelection).toHaveBeenCalledOnce();

    harness.selectedSize = 0;
    installHandler()(keyboardEvent({ key: "Escape" }));
    expect(harness.clearSelection).toHaveBeenCalledOnce();
  });

  it("starts local and ordinary threads with nullable active context", () => {
    harness.command = "chat.newLocal";
    const localHandler = installHandler();
    const localEvent = keyboardEvent();
    localHandler(localEvent);
    expect(localEvent.preventDefault).toHaveBeenCalledOnce();
    expect(localEvent.stopPropagation).toHaveBeenCalledOnce();
    expect(harness.startLocal).toHaveBeenCalledWith(
      expect.objectContaining({ activeThread: undefined }),
    );

    harness.command = "chat.new";
    installHandler()(keyboardEvent());
    expect(harness.startThread).toHaveBeenCalledWith(
      expect.objectContaining({ activeThread: undefined }),
    );

    harness.activeThread = { id: "thread" };
    const threadEvent = keyboardEvent();
    installHandler()(threadEvent);
    expect(harness.startThread).toHaveBeenCalledWith(
      expect.objectContaining({ activeThread: harness.activeThread }),
    );
  });

  it("toggles previews only for routed desktop threads", () => {
    harness.command = "preview.toggle";
    harness.routeThreadRef = null;
    installHandler()(keyboardEvent());
    expect(harness.dispatchPreview).not.toHaveBeenCalled();

    harness.routeThreadRef = { environmentId: "environment-1", threadId: "thread-1" };
    harness.previewSupported = false;
    // A browser tab without previews is an HTTPS page that cannot frame them.
    installHandler()(keyboardEvent());
    expect(harness.toastAdd).toHaveBeenLastCalledWith(
      expect.objectContaining({ title: "Previews open in a new tab" }),
    );
    window.desktopBridge = {} as never;
    installHandler()(keyboardEvent());
    delete window.desktopBridge;
    expect(harness.toastAdd).toHaveBeenLastCalledWith(
      expect.objectContaining({ title: "Preview is desktop-only" }),
    );

    harness.previewSupported = true;
    installHandler()(keyboardEvent());
    expect(harness.dispatchPreview).toHaveBeenCalledWith("toggle-panel");
  });

  it("leaves zoom keys to the browser when the preview host has no zoom", () => {
    harness.pageTools = false;
    harness.command = "preview.zoomIn";
    harness.previewPanel = "preview";
    harness.previewFocused = true;
    const event = keyboardEvent();
    installHandler()(event);
    harness.pageTools = true;
    expect(event.preventDefault).not.toHaveBeenCalled();
    expect(harness.dispatchPreview).not.toHaveBeenCalled();
  });

  it.each([
    ["preview.refresh", "refresh"],
    ["preview.focusUrl", "focus-url"],
    ["preview.zoomIn", "zoom-in"],
    ["preview.zoomOut", "zoom-out"],
    ["preview.resetZoom", "reset-zoom"],
  ])("dispatches %s as %s", (command, action) => {
    harness.command = command;
    harness.terminalSurfaceOpen = true;
    harness.previewPanel = "preview";
    harness.terminalFocused = true;
    harness.previewFocused = true;
    const event = keyboardEvent();
    installHandler()(event);
    expect(event.preventDefault).toHaveBeenCalledOnce();
    expect(event.stopPropagation).toHaveBeenCalledOnce();
    expect(harness.dispatchPreview).toHaveBeenCalledWith(action);
    expect(harness.resolveCommand).toHaveBeenCalledWith(
      event,
      [],
      expect.objectContaining({
        context: {
          terminalFocus: true,
          terminalOpen: true,
          previewFocus: true,
          previewOpen: true,
        },
      }),
    );
  });

  it("derives closed terminal and preview context without a route thread", () => {
    harness.routeThreadRef = null;
    const event = keyboardEvent();
    installHandler()(event);
    expect(harness.resolveCommand).toHaveBeenCalledWith(
      event,
      [],
      expect.objectContaining({
        context: expect.objectContaining({ terminalOpen: false, previewOpen: false }),
      }),
    );
  });
});
