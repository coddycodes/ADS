// @vitest-environment happy-dom

import type {
  DesktopPreviewTabState,
  PreviewSessionSnapshot,
  ScopedThreadRef,
} from "@bibcode/contracts";
import { createElement, StrictMode, type ComponentType, type ReactElement } from "react";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const h = vi.hoisted(() => ({
  applyPreviewDesktopState: vi.fn(),
  clearBrowserPointer: vi.fn(),
  closeTab: vi.fn<(tabId: string) => Promise<void>>(),
  createTab: vi.fn<(tabId: string) => Promise<void>>(),
  events: [] as string[],
  listeners: new Set<(tabId: string, state: DesktopPreviewTabState) => void>(),
  navigate: vi.fn<(tabId: string, url: string) => Promise<void>>(),
  present: vi.fn(),
  releaseSurface: vi.fn(),
  reportStatus: vi.fn(async () => undefined),
}));

vi.mock("~/components/preview/previewBridge", () => ({
  previewBridge: {
    closeTab: h.closeTab,
    createTab: h.createTab,
    navigate: h.navigate,
    onStateChange: (listener: (tabId: string, state: DesktopPreviewTabState) => void) => {
      h.events.push("subscribe");
      h.listeners.add(listener);
      return () => {
        h.events.push("unsubscribe");
        h.listeners.delete(listener);
      };
    },
  },
}));

vi.mock("./previewPartition", () => ({ previewPartitionFor: () => null }));
vi.mock("~/browser/browserPointerStore", () => ({
  useBrowserPointerStore: (selector: (state: { clear: typeof h.clearBrowserPointer }) => unknown) =>
    selector({ clear: h.clearBrowserPointer }),
}));

vi.mock("~/previewStateStore", () => ({
  applyPreviewDesktopState: h.applyPreviewDesktopState,
  readThreadPreviewState: () => ({ sessions: {}, desktopByTabId: {} }),
}));

vi.mock("~/state/preview", () => ({
  previewEnvironment: { reportStatus: { key: "preview.reportStatus" } },
}));

vi.mock("~/state/use-atom-command", () => ({
  useAtomCommand: () => h.reportStatus,
}));

vi.mock("./browserSurfaceStore", () => ({
  acquireBrowserSurface: (tabId: string) => ({
    present: (...args: unknown[]) => {
      h.events.push(`bounds:${tabId}:${String(args[1])}`);
      h.present(...args);
    },
    release: h.releaseSurface,
  }),
}));

import * as browserSurfaceModule from "./BrowserSurfaceSlot";
import * as desktopPreviewTabHostsModule from "./DesktopPreviewTabHosts";
import type { RightPanelSurface } from "~/rightPanelStore";

const { BrowserSurfaceSlot } = browserSurfaceModule;

type NativePreviewTabHostComponent = ComponentType<{
  readonly threadRef: ScopedThreadRef;
  readonly tabId: string;
  readonly initialUrl: string | null;
}>;

type DesktopPreviewTabHostsComponent = ComponentType<{
  readonly threadRef: ScopedThreadRef;
  readonly surfaces: readonly RightPanelSurface[];
  readonly sessions: Readonly<Record<string, PreviewSessionSnapshot>>;
  readonly activeSurfaceId: string | null;
}>;

interface Deferred {
  readonly promise: Promise<void>;
  readonly resolve: () => void;
}

interface MountedTree {
  readonly container: HTMLDivElement;
  readonly root: Root;
}

const threadRef = {
  environmentId: "environment-1",
  threadId: "thread-1",
} as ScopedThreadRef;

const mountedTrees: MountedTree[] = [];

function deferred(): Deferred {
  let resolve!: () => void;
  const promise = new Promise<void>((next) => {
    resolve = next;
  });
  return { promise, resolve };
}

function requireNativePreviewTabHost(): NativePreviewTabHostComponent {
  const component = Reflect.get(desktopPreviewTabHostsModule, "NativePreviewTabHost") as unknown;
  expect(component).toEqual(expect.any(Function));
  return component as NativePreviewTabHostComponent;
}

function requireDesktopPreviewTabHosts(): DesktopPreviewTabHostsComponent {
  const component = Reflect.get(desktopPreviewTabHostsModule, "DesktopPreviewTabHosts") as unknown;
  expect(component).toEqual(expect.any(Function));
  return component as DesktopPreviewTabHostsComponent;
}

function host(
  Host: NativePreviewTabHostComponent,
  tabId: string,
  initialUrl: string | null,
): ReactElement {
  return createElement(Host, { key: tabId, threadRef, tabId, initialUrl });
}

function snapshot(tabId: string, url: string | null): PreviewSessionSnapshot {
  return {
    threadId: threadRef.threadId,
    tabId,
    navStatus: url === null ? { _tag: "Idle" } : { _tag: "Success", url, title: `Title ${tabId}` },
    canGoBack: false,
    canGoForward: false,
    updatedAt: "2026-07-20T00:00:00.000Z",
  };
}

function previewSurface(tabId: string): RightPanelSurface {
  return { id: `browser:${tabId}`, kind: "preview", resourceId: tabId };
}

function hosts(
  Hosts: DesktopPreviewTabHostsComponent,
  surfaces: readonly RightPanelSurface[],
  sessions: Readonly<Record<string, PreviewSessionSnapshot>>,
  activeSurfaceId: string | null,
): ReactElement {
  return (
    <Hosts
      threadRef={threadRef}
      surfaces={surfaces}
      sessions={sessions}
      activeSurfaceId={activeSurfaceId}
    />
  );
}

async function mount(element: ReactElement): Promise<MountedTree> {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  const mounted = { container, root };
  mountedTrees.push(mounted);
  await act(async () => root.render(element));
  return mounted;
}

async function rerender(mounted: MountedTree, element: ReactElement): Promise<void> {
  await act(async () => mounted.root.render(element));
}

async function unmount(mounted: MountedTree): Promise<void> {
  await act(async () => mounted.root.unmount());
  mounted.container.remove();
  mountedTrees.splice(mountedTrees.indexOf(mounted), 1);
}

async function flush(): Promise<void> {
  await act(async () => {
    for (let index = 0; index < 6; index += 1) await Promise.resolve();
  });
}

async function runCloseTimers(): Promise<void> {
  await act(async () => vi.runAllTimersAsync());
}

beforeEach(() => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.stubGlobal(
    "ResizeObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
    x: 1,
    y: 2,
    width: 300,
    height: 200,
    top: 2,
    right: 301,
    bottom: 202,
    left: 1,
    toJSON: () => ({}),
  });
  h.applyPreviewDesktopState.mockClear();
  h.clearBrowserPointer.mockClear();
  h.closeTab.mockReset().mockImplementation(async (tabId) => {
    h.events.push(`close:${tabId}`);
  });
  h.createTab.mockReset().mockImplementation(async (tabId) => {
    h.events.push(`create:${tabId}`);
  });
  h.events.length = 0;
  h.listeners.clear();
  h.navigate.mockReset().mockImplementation(async (tabId, url) => {
    h.events.push(`navigate:${tabId}:${url}`);
  });
  h.present.mockClear();
  h.releaseSurface.mockClear();
  h.reportStatus.mockClear();
});

afterEach(async () => {
  for (const mounted of mountedTrees.splice(0)) {
    await act(async () => mounted.root.unmount());
    mounted.container.remove();
  }
  await runCloseTimers();
  document.body.replaceChildren();
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("BrowserSurfaceSlot bounds lifecycle", () => {
  it("only publishes native bounds and visibility", async () => {
    const mounted = await mount(<BrowserSurfaceSlot tabId="tab-1" visible />);

    expect(h.present).toHaveBeenCalledOnce();
    expect(h.events).toEqual(["bounds:tab-1:true"]);
    expect(h.createTab).not.toHaveBeenCalled();
    expect(h.navigate).not.toHaveBeenCalled();
    expect(h.listeners).toHaveLength(0);

    await rerender(mounted, <BrowserSurfaceSlot tabId="tab-1" visible={false} />);

    expect(h.events).toEqual(["bounds:tab-1:true", "bounds:tab-1:false"]);
    expect(h.releaseSurface).toHaveBeenCalledOnce();
    expect(h.createTab).not.toHaveBeenCalled();

    await unmount(mounted);
    await runCloseTimers();
    expect(h.closeTab).not.toHaveBeenCalled();
    expect(h.listeners).toHaveLength(0);
  });
});

describe("NativePreviewTabHost native lifecycle", () => {
  it("materializes only the active preview surface", async () => {
    const Hosts = requireDesktopPreviewTabHosts();
    const surfaces = [previewSurface("tab-a"), previewSurface("tab-b")];
    const sessions = {
      "tab-a": snapshot("tab-a", "https://a.test/"),
      "tab-b": snapshot("tab-b", "https://b.test/"),
    };

    await mount(hosts(Hosts, surfaces, sessions, "browser:tab-b"));
    await flush();

    expect(h.createTab.mock.calls).toEqual([["tab-b", null]]);
    expect(h.navigate.mock.calls).toEqual([["tab-b", "https://b.test/"]]);
    expect(h.listeners).toHaveLength(1);
  });

  it("closes the inactive native tab before creating the next active tab", async () => {
    const Hosts = requireDesktopPreviewTabHosts();
    const surfaces = [previewSurface("tab-a"), previewSurface("tab-b")];
    const sessions = {
      "tab-a": snapshot("tab-a", "https://a.test/"),
      "tab-b": snapshot("tab-b", "https://b.test/"),
    };
    const closing = deferred();
    const mounted = await mount(hosts(Hosts, surfaces, sessions, "browser:tab-a"));
    await flush();
    h.closeTab.mockImplementationOnce((tabId) => {
      h.events.push(`close:${tabId}`);
      return closing.promise;
    });

    await rerender(mounted, hosts(Hosts, surfaces, sessions, "browser:tab-b"));
    await runCloseTimers();
    await flush();

    expect(h.closeTab).toHaveBeenCalledExactlyOnceWith("tab-a");
    expect(h.createTab.mock.calls).toEqual([["tab-a", null]]);

    closing.resolve();
    await flush();

    expect(h.createTab.mock.calls).toEqual([
      ["tab-a", null],
      ["tab-b", null],
    ]);
    expect(h.navigate).toHaveBeenLastCalledWith("tab-b", "https://b.test/");
  });

  it("subscribes before creating while the active slot publishes bounds first", async () => {
    const Host = requireNativePreviewTabHost();
    const creation = deferred();
    h.createTab.mockImplementationOnce((tabId) => {
      h.events.push(`create:${tabId}`);
      return creation.promise;
    });

    const mounted = await mount(
      <>
        {host(Host, "tab-1", "https://initial.test/")}
        <BrowserSurfaceSlot tabId="tab-1" visible />
      </>,
    );
    await rerender(
      mounted,
      <>
        {host(Host, "tab-1", "https://stale.test/")}
        <BrowserSurfaceSlot tabId="tab-1" visible />
      </>,
    );

    expect(h.events).toEqual(["bounds:tab-1:true", "subscribe", "create:tab-1"]);
    expect(h.navigate).not.toHaveBeenCalled();

    creation.resolve();
    await flush();

    expect(h.navigate).toHaveBeenCalledExactlyOnceWith("tab-1", "https://initial.test/");
  });

  it("releases the active native tab when the panel is hidden", async () => {
    const Hosts = requireDesktopPreviewTabHosts();
    const surfaces = [previewSurface("tab-a"), previewSurface("tab-b")];
    const sessions = {
      "tab-a": snapshot("tab-a", "https://a.test/"),
      "tab-b": snapshot("tab-b", "https://b.test/"),
    };
    const mounted = await mount(hosts(Hosts, surfaces, sessions, "browser:tab-a"));
    await flush();

    expect(h.createTab.mock.calls).toEqual([["tab-a", null]]);
    expect(h.listeners).toHaveLength(1);

    await rerender(mounted, hosts(Hosts, surfaces, sessions, null));
    await runCloseTimers();

    expect(h.closeTab).toHaveBeenCalledExactlyOnceWith("tab-a");
    expect(h.listeners).toHaveLength(0);
  });

  it("recreates a browser after it is closed and reopened", async () => {
    const Hosts = requireDesktopPreviewTabHosts();
    const bothSurfaces = [previewSurface("tab-a"), previewSurface("tab-b")];
    const initialSessions = {
      "tab-a": snapshot("tab-a", "https://a.test/"),
      "tab-b": snapshot("tab-b", "https://b.test/"),
    };
    const mounted = await mount(hosts(Hosts, bothSurfaces, initialSessions, "browser:tab-a"));
    await flush();

    await rerender(
      mounted,
      hosts(
        Hosts,
        [previewSurface("tab-b")],
        { "tab-b": initialSessions["tab-b"] },
        "browser:tab-b",
      ),
    );
    await runCloseTimers();

    expect(h.closeTab.mock.calls).toEqual([["tab-a"]]);
    expect(h.listeners).toHaveLength(1);

    await rerender(
      mounted,
      hosts(
        Hosts,
        bothSurfaces,
        {
          "tab-a": snapshot("tab-a", "https://a-reopened.test/"),
          "tab-b": initialSessions["tab-b"],
        },
        "browser:tab-a",
      ),
    );
    await flush();

    expect(h.createTab.mock.calls).toEqual([
      ["tab-a", null],
      ["tab-b", null],
      ["tab-a", null],
    ]);
    expect(h.navigate.mock.calls).toEqual([
      ["tab-a", "https://a.test/"],
      ["tab-b", "https://b.test/"],
      ["tab-a", "https://a-reopened.test/"],
    ]);
    expect(h.closeTab).toHaveBeenCalledWith("tab-b");
  });

  it("hosts an active idle preview surface without navigating it", async () => {
    const Hosts = requireDesktopPreviewTabHosts();
    const mounted = await mount(
      hosts(
        Hosts,
        [
          { id: "browser:new", kind: "preview", resourceId: null },
          previewSurface("tab-idle"),
          previewSurface("tab-missing"),
          previewSurface("tab-live"),
          { id: "plan", kind: "plan" },
        ],
        {
          "tab-idle": snapshot("tab-idle", null),
          "tab-live": snapshot("tab-live", "https://live.test/"),
        },
        "browser:tab-idle",
      ),
    );
    await flush();

    expect(h.createTab.mock.calls).toEqual([["tab-idle", null]]);
    expect(h.navigate).not.toHaveBeenCalled();
    expect(h.listeners).toHaveLength(1);

    await unmount(mounted);
  });

  it("shares creation across StrictMode reacquisition and closes after final release", async () => {
    const Host = requireNativePreviewTabHost();
    const creation = deferred();
    h.createTab.mockImplementationOnce((tabId) => {
      h.events.push(`create:${tabId}`);
      return creation.promise;
    });

    const mounted = await mount(
      <StrictMode>{host(Host, "tab-strict", "https://strict.test/")}</StrictMode>,
    );

    expect(h.createTab).toHaveBeenCalledOnce();
    expect(h.closeTab).not.toHaveBeenCalled();
    expect(h.listeners).toHaveLength(1);

    creation.resolve();
    await flush();
    expect(h.navigate).toHaveBeenCalledExactlyOnceWith("tab-strict", "https://strict.test/");

    await unmount(mounted);
    expect(h.closeTab).not.toHaveBeenCalled();
    await runCloseTimers();
    expect(h.closeTab).toHaveBeenCalledExactlyOnceWith("tab-strict");
  });
});
