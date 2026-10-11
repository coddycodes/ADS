// @vitest-environment happy-dom

import { EnvironmentId, ProjectId, ThreadId } from "@bibcode/contracts";
import { act, type ComponentPropsWithoutRef, type ReactElement, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const h = vi.hoisted(() => ({
  auth: { isLoaded: false, isSignedIn: false },
  authPrompt: null as ReactNode,
  canGoBack: false,
  cloudConfigured: false,
  draftThread: null as Record<string, unknown> | null,
  panelProps: null as Record<string, unknown> | null,
  knownSessions: [] as Array<Record<string, any>>,
  lease: {
    present: vi.fn(),
    release: vi.fn(),
  },
  navigate: vi.fn(),
  openAuthPrompt: vi.fn(),
  previewSupported: false,
  previewViewProps: null as Record<string, unknown> | null,
  project: null as Record<string, unknown> | null,
  restore: vi.fn(),
  restoreLabels: [] as string[],
  routeContext: {} as Record<string, unknown>,
  routeSearch: {} as Record<string, unknown>,
  serverThread: null as Record<string, unknown> | null,
}));

vi.mock("@tanstack/react-router", () => ({
  Outlet: () => <main data-outlet />,
  createFileRoute: (path: string) => (options: Record<string, unknown>) => ({
    ...options,
    path,
    useRouteContext: () => h.routeContext,
    useSearch: () => h.routeSearch,
    useNavigate: () => h.navigate,
  }),
  redirect: (options: unknown) => ({ _tag: "Redirect", options }),
  useCanGoBack: () => h.canGoBack,
  useLocation: () => h.routeContext.location ?? { pathname: "/settings/general" },
  useNavigate: () => h.navigate,
}));

vi.mock("./components/settings/SettingsPanels", () => ({
  useGeneralSettingsRestore: (onRestored: () => void) => ({
    changedSettingLabels: h.restoreLabels,
    restoreDefaults: async () => {
      h.restore();
      onRestored();
    },
  }),
}));

vi.mock("./components/ui/button", () => ({
  Button: ({
    variant: _variant,
    size: _size,
    ...props
  }: ComponentPropsWithoutRef<"button"> & {
    variant?: string;
    size?: string;
  }) => <button {...props} />,
}));

vi.mock("./components/ui/sidebar", () => {
  const Container = ({ children, ...props }: ComponentPropsWithoutRef<"div">) => (
    <div {...props}>{children}</div>
  );
  return {
    SidebarInset: Container,
    SidebarMenu: Container,
    SidebarMenuButton: (props: ComponentPropsWithoutRef<"button">) => <button {...props} />,
    SidebarMenuItem: Container,
  };
});

vi.mock("./components/auth/PairingRouteSurface", () => ({
  HostedPairingRouteSurface: () => <div>Hosted pairing</div>,
  PairingPendingSurface: () => <div>Pairing pending</div>,
  PairingRouteSurface: (props: Record<string, unknown>) => (
    <button onClick={props.onAuthenticated as () => void}>
      Pairing surface {String(props.initialErrorMessage ?? "")}
    </button>
  ),
}));

vi.mock("@clerk/react", () => ({
  UserButton: () => <div data-user-button />,
  useAuth: () => h.auth,
}));

vi.mock("./cloud/publicConfig", () => ({
  hasCloudPublicConfig: () => h.cloudConfigured,
}));

vi.mock("./components/clerk/useBiBCodeConnectAuthPrompt", () => ({
  useBiBCodeConnectAuthPrompt: () => ({
    authPrompt: h.authPrompt,
    openAuthPrompt: h.openAuthPrompt,
  }),
}));

vi.mock("./composerDraftStore", () => ({
  useComposerDraftStore: (selector: (state: unknown) => unknown) =>
    selector({ getDraftThreadByRef: () => h.draftThread }),
}));

vi.mock("./state/entities", () => ({
  useProject: () => h.project,
  useThread: () => h.serverThread,
}));

vi.mock("./state/terminalSessions", () => ({
  useKnownTerminalSessions: () => h.knownSessions,
}));

vi.mock("./components/ThreadTerminalPanel", () => ({
  default: (props: Record<string, unknown>) => {
    h.panelProps = props;
    return <div data-terminal-panel>{String(props.cwd)}</div>;
  },
}));

vi.mock("./previewStateStore", () => ({
  isPreviewSupportedInRuntime: () => h.previewSupported,
  canPreviewAnyUrlInRuntime: () => h.previewSupported,
}));

vi.mock("./components/preview/PreviewPanelShell", () => ({
  PreviewPanelShell: ({ children, mode }: { children?: ReactNode; mode: string }) => (
    <section data-mode={mode}>{children}</section>
  ),
}));

vi.mock("./components/preview/PreviewView", () => ({
  PreviewView: (props: Record<string, unknown>) => {
    h.previewViewProps = props;
    return <div>Preview view</div>;
  },
}));

vi.mock("./browser/browserSurfaceStore", () => ({
  acquireBrowserSurface: () => h.lease,
  // Browser mode's frame preview bridge streams surface bounds from the store.
  useBrowserSurfaceStore: {
    getState: () => ({ byTabId: {}, occlusionOwners: new Set() }),
    subscribe: () => () => undefined,
  },
}));

vi.mock("./components/preview/usePreviewBridge", () => ({
  usePreviewBridge: () => undefined,
}));

import { CenterTerminalPanel } from "./components/CenterTerminalPanel";
import {
  BiBCodeConnectSidebarAvatar,
  BiBCodeConnectSidebarSignIn,
} from "./components/clerk/BiBCodeConnectSidebarSignIn";
import { PreviewPanel } from "./components/preview/PreviewPanel";
import { PreviewUnreachable } from "./components/preview/PreviewUnreachable";
import { useLoadingProgress } from "./components/preview/useLoadingProgress";
import { BrowserSurfaceSlot } from "./browser/BrowserSurfaceSlot";
import {
  isCommandPaletteOpen,
  OpenAddProjectCommandPaletteProvider,
  useOpenAddProjectCommandPalette,
} from "./commandPaletteContext";
import { Route as PairRoute } from "./routes/pair";
import { Route as SettingsRoute } from "./routes/settings";

interface MountedTree {
  readonly container: HTMLDivElement;
  readonly root: Root;
}

const mountedTrees: MountedTree[] = [];

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

async function click(element: HTMLElement): Promise<void> {
  await act(async () => element.click());
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  h.auth = { isLoaded: false, isSignedIn: false };
  h.authPrompt = null;
  h.canGoBack = false;
  h.cloudConfigured = false;
  h.draftThread = null;
  h.panelProps = null;
  h.knownSessions = [];
  h.navigate.mockReset().mockResolvedValue(undefined);
  h.openAuthPrompt.mockReset();
  h.previewSupported = false;
  h.previewViewProps = null;
  h.project = null;
  h.restore.mockReset();
  h.restoreLabels = [];
  h.routeContext = {};
  h.routeSearch = {};
  h.serverThread = null;
  h.lease.present.mockReset();
  h.lease.release.mockReset();
});

afterEach(async () => {
  for (const mounted of mountedTrees.splice(0)) {
    await act(async () => mounted.root.unmount());
    mounted.container.remove();
  }
  document.body.replaceChildren();
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("settings and pairing routes", () => {
  it("enforces route auth and canonical settings redirects", async () => {
    const beforeLoad = (SettingsRoute as unknown as { beforeLoad: (input: any) => Promise<void> })
      .beforeLoad;
    await expect(
      beforeLoad({
        context: { authGateState: { status: "pairing" } },
        location: { pathname: "/settings" },
      }),
    ).rejects.toEqual({
      _tag: "Redirect",
      options: { to: "/pair", replace: true },
    });
    await expect(
      beforeLoad({
        context: { authGateState: { status: "authenticated" } },
        location: { pathname: "/settings" },
      }),
    ).rejects.toEqual({
      _tag: "Redirect",
      options: { to: "/settings/general", replace: true },
    });
    await expect(
      beforeLoad({
        context: { authGateState: { status: "hosted-static" } },
        location: { pathname: "/settings/providers" },
      }),
    ).resolves.toBeUndefined();
  });

  it("restores defaults and routes Escape through history or home", async () => {
    const Component = (SettingsRoute as unknown as { component: () => ReactElement }).component;
    h.restoreLabels = ["Theme"];
    h.routeContext = { location: { pathname: "/settings/general" } };
    const mounted = await mount(<Component />);
    expect(mounted.container.textContent).toContain("Restore General defaults");
    await click(mounted.container.querySelector("button")!);
    expect(h.restore).toHaveBeenCalledOnce();

    window.dispatchEvent(
      new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true }),
    );
    expect(h.navigate).toHaveBeenCalledWith({ to: "/" });

    const back = vi.spyOn(window.history, "back").mockImplementation(() => undefined);
    h.canGoBack = true;
    h.routeContext = { location: { pathname: "/settings/providers" } };
    await rerender(mounted, <Component key="providers" />);
    expect(mounted.container.querySelector("button")).toBeNull();
    const prevented = new KeyboardEvent("keydown", {
      key: "Escape",
      bubbles: true,
      cancelable: true,
    });
    window.dispatchEvent(prevented);
    expect(prevented.defaultPrevented).toBe(true);
    expect(back).toHaveBeenCalledOnce();

    const ignored = new KeyboardEvent("keydown", { key: "Enter", bubbles: true });
    window.dispatchEvent(ignored);
    const alreadyPrevented = new KeyboardEvent("keydown", {
      key: "Escape",
      bubbles: true,
      cancelable: true,
    });
    alreadyPrevented.preventDefault();
    window.dispatchEvent(alreadyPrevented);
    expect(back).toHaveBeenCalledOnce();
  });

  it("selects hosted, authenticated, pending, and regular pairing states", async () => {
    const beforeLoad = (PairRoute as unknown as { beforeLoad: (input: any) => Promise<unknown> })
      .beforeLoad;
    await expect(
      beforeLoad({ context: { authGateState: { status: "hosted-pairing" } } }),
    ).resolves.toEqual({ authGateState: { status: "hosted-pairing" } });
    await expect(
      beforeLoad({ context: { authGateState: { status: "authenticated" } } }),
    ).rejects.toEqual({ _tag: "Redirect", options: { to: "/", replace: true } });
    await expect(
      beforeLoad({ context: { authGateState: { status: "pairing", auth: "auth" } } }),
    ).resolves.toEqual({ authGateState: { status: "pairing", auth: "auth" } });

    const Component = (PairRoute as unknown as { component: () => ReactElement | null }).component;
    h.routeContext = { authGateState: null };
    const mounted = await mount(<Component />);
    expect(mounted.container.innerHTML).toBe("");
    h.routeContext = { authGateState: { status: "hosted-pairing" } };
    await rerender(mounted, <Component />);
    expect(mounted.container.textContent).toContain("Hosted pairing");
    h.routeContext = {
      authGateState: { status: "pairing", auth: "auth", errorMessage: "Try again" },
    };
    await rerender(mounted, <Component />);
    expect(mounted.container.textContent).toContain("Try again");
    await click(mounted.container.querySelector("button")!);
    expect(h.navigate).toHaveBeenCalledWith({ to: "/", replace: true });

    const Pending = (PairRoute as unknown as { pendingComponent: () => ReactElement })
      .pendingComponent;
    await rerender(mounted, <Pending />);
    expect(mounted.container.textContent).toContain("Pairing pending");
  });
});

describe("cloud sign-in surfaces", () => {
  it("hides unconfigured and unresolved auth before rendering sign-in or avatar", async () => {
    const mounted = await mount(<BiBCodeConnectSidebarSignIn key="unconfigured" />);
    expect(mounted.container.innerHTML).toBe("");

    h.cloudConfigured = true;
    await rerender(mounted, <BiBCodeConnectSidebarSignIn key="auth-loading" />);
    expect(mounted.container.innerHTML).toBe("");

    h.auth = { isLoaded: true, isSignedIn: false };
    h.authPrompt = <div>Auth dialog</div>;
    await rerender(mounted, <BiBCodeConnectSidebarSignIn key="signed-out" />);
    await click(mounted.container.querySelector("button")!);
    expect(h.openAuthPrompt).toHaveBeenCalledOnce();
    expect(mounted.container.textContent).toContain("Auth dialog");

    h.auth = { isLoaded: true, isSignedIn: true };
    await rerender(mounted, <BiBCodeConnectSidebarSignIn key="signed-in" />);
    expect(mounted.container.innerHTML).toBe("");
    await rerender(mounted, <BiBCodeConnectSidebarAvatar key="configured-avatar" />);
    expect(mounted.container.querySelector("[data-user-button]")).not.toBeNull();
    h.cloudConfigured = false;
    await rerender(mounted, <BiBCodeConnectSidebarAvatar key="unconfigured-avatar" />);
    expect(mounted.container.innerHTML).toBe("");
  });
});

describe("center terminal and preview surfaces", () => {
  const threadRef = {
    environmentId: EnvironmentId.make("environment-local"),
    threadId: ThreadId.make("thread-1"),
  };

  it("requires and forwards resolved center terminal launch state", async () => {
    const onAddTerminalContext = vi.fn();
    const onClose = vi.fn();
    const renderPanel = (
      launchContext: {
        cwd: string;
        worktreePath: string | null;
        runtimeEnv: Record<string, string>;
      } | null = null,
    ) => (
      <CenterTerminalPanel
        threadRef={threadRef}
        projectId={ProjectId.make("project-1")}
        surface={{
          id: "terminal:terminal-1",
          kind: "terminal",
          terminalId: "terminal-1",
        }}
        launchContext={launchContext}
        keybindings={{} as never}
        focusRequestId={3}
        focusEligible={true}
        onAddTerminalContext={onAddTerminalContext}
        onClose={onClose}
      />
    );
    const mounted = await mount(renderPanel());
    expect(mounted.container.innerHTML).toBe("");

    await rerender(
      mounted,
      renderPanel({
        cwd: "/repo/worktree",
        worktreePath: "/repo/worktree",
        runtimeEnv: { BIBCODE_PROJECT_ROOT: "/repo" },
      }),
    );
    expect(h.panelProps).toMatchObject({
      owner: "center-panel",
      cwd: "/repo/worktree",
      worktreePath: "/repo/worktree",
      terminalIds: ["terminal-1"],
      activeTerminalId: "terminal-1",
      focusRequestId: 3,
    });

    h.knownSessions = [
      {
        target: { terminalId: "terminal-1" },
        state: { summary: { cwd: "/custom", worktreePath: "/custom-tree", title: "Shell" } },
      },
    ];
    await rerender(
      mounted,
      renderPanel({
        cwd: "/repo/current-worktree",
        worktreePath: "/repo/current-worktree",
        runtimeEnv: { BIBCODE_PROJECT_ROOT: "/repo" },
      }),
    );
    expect(h.panelProps).toMatchObject({
      cwd: "/repo/current-worktree",
      worktreePath: "/repo/current-worktree",
    });
    (h.panelProps!.onCloseTerminal as () => void)();
    (h.panelProps!.onAddTerminalContext as (value: unknown) => void)({
      terminalId: "terminal-1",
    });
    (h.panelProps!.onActiveTerminalChange as () => void)();
    expect(onClose).toHaveBeenCalledOnce();
    expect(onAddTerminalContext).toHaveBeenCalledOnce();
  });

  it("shows unsupported preview messaging or forwards supported preview props", async () => {
    const mounted = await mount(
      <PreviewPanel mode="embedded" threadRef={threadRef} visible configuredUrls={[]} />,
    );
    expect(mounted.container.textContent).toContain("desktop app");

    h.previewSupported = true;
    await rerender(
      mounted,
      <PreviewPanel
        mode="sheet"
        threadRef={threadRef}
        tabId={null}
        configuredUrls={["http://localhost:3000"]}
        visible={false}
      />,
    );
    expect(h.previewViewProps).toMatchObject({
      threadRef,
      tabId: null,
      configuredUrls: ["http://localhost:3000"],
      visible: false,
    });
    await rerender(mounted, <PreviewPanel mode="sheet" threadRef={threadRef} visible />);
    expect(h.previewViewProps).not.toHaveProperty("tabId");
  });

  it("toggles unreachable details, formats invalid URLs, and reloads", async () => {
    const onReload = vi.fn();
    const mounted = await mount(
      <PreviewUnreachable
        url="https://example.test/path"
        code={-105}
        description="ERR_NAME_NOT_RESOLVED"
        onReload={onReload}
      />,
    );
    expect(mounted.container.textContent).toContain("example.test");
    await click(Array.from(mounted.container.querySelectorAll("button"))[0]!);
    expect(mounted.container.textContent).toContain("Checking your connection");
    expect(mounted.container.textContent).toContain("Hide details");
    await click(Array.from(mounted.container.querySelectorAll("button"))[0]!);
    await click(Array.from(mounted.container.querySelectorAll("button"))[1]!);
    expect(onReload).toHaveBeenCalledOnce();

    await rerender(
      mounted,
      <PreviewUnreachable url="not a url" code={0} description="" onReload={onReload} />,
    );
    expect(mounted.container.textContent).toContain("not a url");
    expect(mounted.container.textContent).toContain("ERR_FAILED");
  });
});

describe("browser surface and progress lifecycles", () => {
  it("presents measured browser bounds, reacts to observers, and releases leases", async () => {
    const resizeCallbacks: { current?: () => void } = {};
    const disconnect = vi.fn();
    vi.stubGlobal(
      "ResizeObserver",
      class {
        constructor(callback: () => void) {
          resizeCallbacks.current = callback;
        }
        observe() {}
        disconnect() {
          disconnect();
        }
      },
    );
    vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
      x: 1.4,
      y: 2.6,
      width: 300.2,
      height: 199.7,
      top: 2.6,
      right: 301.6,
      bottom: 202.3,
      left: 1.4,
      toJSON: () => ({}),
    });
    const mounted = await mount(<BrowserSurfaceSlot tabId="tab-1" visible className="slot" />);
    expect(h.lease.present).toHaveBeenCalledWith({ x: 1, y: 3, width: 300, height: 200 }, true);
    resizeCallbacks.current?.();
    window.dispatchEvent(new Event("resize"));
    window.dispatchEvent(new Event("scroll"));
    expect(h.lease.present.mock.calls.length).toBeGreaterThan(3);

    await rerender(mounted, <BrowserSurfaceSlot tabId="tab-2" visible={false} />);
    expect(h.lease.release).toHaveBeenCalledOnce();
    expect(h.lease.present).toHaveBeenLastCalledWith(expect.any(Object), false);
    await act(async () => mounted.root.unmount());
    mountedTrees.splice(mountedTrees.indexOf(mounted), 1);
    expect(h.lease.release).toHaveBeenCalledTimes(2);
    expect(disconnect).toHaveBeenCalledTimes(2);
  });

  it("seeds, advances, completes, and resets loading progress", async () => {
    vi.useFakeTimers();
    let progress = -1;
    const Harness = ({ loading }: { loading: boolean }) => {
      progress = useLoadingProgress(loading);
      return <span>{progress}</span>;
    };
    const mounted = await mount(<Harness loading={false} />);
    expect(progress).toBe(0);
    await rerender(mounted, <Harness loading />);
    expect(progress).toBe(4);
    await act(async () => vi.advanceTimersByTime(120));
    expect(progress).toBeGreaterThan(4);
    await rerender(mounted, <Harness loading={false} />);
    expect(progress).toBe(100);
    await act(async () => vi.advanceTimersByTime(220));
    expect(progress).toBe(0);
  });
});

describe("command palette presentation", () => {
  it("provides command-palette actions and reads live DOM state", async () => {
    const open = vi.fn();
    const captured: { current?: () => void } = {};
    const Consumer = () => {
      captured.current = useOpenAddProjectCommandPalette();
      return null;
    };
    const mounted = await mount(
      <OpenAddProjectCommandPaletteProvider openAddProject={open}>
        <Consumer />
      </OpenAddProjectCommandPaletteProvider>,
    );
    captured.current?.();
    expect(open).toHaveBeenCalledOnce();
    expect(isCommandPaletteOpen()).toBe(false);
    const marker = document.createElement("div");
    marker.dataset.commandPalette = "";
    document.body.append(marker);
    expect(isCommandPaletteOpen()).toBe(true);

    await act(async () => mounted.root.unmount());
    mountedTrees.splice(mountedTrees.indexOf(mounted), 1);
    expect(() => renderToStaticMarkup(<Consumer />)).toThrow(
      "Command palette actions must be used inside CommandPalette",
    );
  });
});
