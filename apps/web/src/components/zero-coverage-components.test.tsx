// @vitest-environment happy-dom

import { ApprovalRequestId, EnvironmentId } from "@bibcode/contracts";
import { act, type ComponentPropsWithoutRef, type ReactElement, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const h = vi.hoisted(() => ({
  atomValue: [] as unknown[],
  canGoBack: false,
  menuItems: [] as Array<Record<string, unknown>>,
  menuOnOpenChange: null as ((open: boolean) => void) | null,
  navigate: vi.fn(),
  previewServers: [] as unknown[],
  setOpenMobile: vi.fn(),
  sheetOnOpenChange: null as
    | ((
        open: boolean,
        details?: {
          cancel: () => void;
          event: KeyboardEvent;
          reason: string;
        },
      ) => void)
    | null,
  sidebarIsMobile: false,
  sidebarProps: null as Record<string, unknown> | null,
  textareaProps: null as Record<string, unknown> | null,
  toggleSidebar: vi.fn(),
  workerPool: null as null | {
    getDiffRenderOptions: () => { theme: string; marker?: string };
    setRenderOptions: (options: unknown) => Promise<void>;
  },
  workerProviderProps: null as Record<string, unknown> | null,
  workerThrows: false,
}));

const previewBridgeMock = vi.hoisted(() => ({
  clearCache: vi.fn(() => Promise.resolve()),
  clearCookies: vi.fn(() => Promise.resolve()),
  hardReload: vi.fn(() => Promise.resolve()),
  openDevTools: vi.fn(() => Promise.resolve()),
  resetZoom: vi.fn(() => Promise.resolve()),
  zoomIn: vi.fn(() => Promise.resolve()),
  zoomOut: vi.fn(() => Promise.resolve()),
}));

vi.mock("@effect/atom-react", () => ({
  useAtomValue: () => h.atomValue,
}));

vi.mock("@tanstack/react-router", () => ({
  useCanGoBack: () => h.canGoBack,
  useNavigate: () => h.navigate,
}));

vi.mock("./Sidebar", () => ({
  default: () => <nav data-thread-sidebar />,
}));

vi.mock("./sidebar/EnvironmentRail", () => ({
  EnvironmentRail: () => <nav data-environment-rail />,
}));

vi.mock("./ui/sidebar", () => {
  const Container = ({ children, ...props }: ComponentPropsWithoutRef<"div">) => (
    <div {...props}>{children}</div>
  );
  return {
    Sidebar: ({ children, resizable, ...props }: Record<string, unknown>) => {
      h.sidebarProps = { resizable, ...props };
      return <aside>{children as ReactNode}</aside>;
    },
    // AppSidebarLayout reads this to seed its initial width; this suite
    // doesn't exercise sidebar-width behavior, so simulate "nothing saved".
    readStoredSidebarWidth: () => null,
    SidebarContent: Container,
    SidebarFooter: Container,
    SidebarGroup: Container,
    SidebarMenu: Container,
    SidebarMenuButton: ({ isActive, size: _size, ...props }: Record<string, unknown>) => (
      <button
        data-active={String(Boolean(isActive))}
        {...(props as ComponentPropsWithoutRef<"button">)}
      />
    ),
    SidebarMenuItem: Container,
    SidebarProvider: ({
      children,
      defaultOpen: _defaultOpen,
      ...props
    }: ComponentPropsWithoutRef<"div"> & { defaultOpen?: boolean }) => (
      <div {...props}>{children}</div>
    ),
    SidebarRail: () => <div data-sidebar-rail />,
    SidebarSeparator: () => <hr />,
    SidebarTrigger: (props: ComponentPropsWithoutRef<"button">) => <button {...props} />,
    useSidebar: () => ({
      isMobile: h.sidebarIsMobile,
      setOpenMobile: h.setOpenMobile,
      toggleSidebar: h.toggleSidebar,
    }),
  };
});

vi.mock("./clerk/BiBCodeConnectSidebarSignIn", () => ({
  BiBCodeConnectSidebarAvatar: () => <span data-connect-avatar />,
  BiBCodeConnectSidebarSignIn: () => <span data-connect-sign-in />,
}));

vi.mock("./ui/tooltip", () => ({
  Tooltip: ({ children }: { children?: ReactNode }) => <>{children}</>,
  TooltipPopup: ({ children }: { children?: ReactNode }) => <span>{children}</span>,
  TooltipTrigger: ({ children, render }: { children?: ReactNode; render?: ReactNode }) => (
    <>
      {render ?? children}
      {render ? children : null}
    </>
  ),
}));

vi.mock("./ui/button", () => ({
  Button: ({
    variant: _variant,
    size: _size,
    ...props
  }: ComponentPropsWithoutRef<"button"> & { variant?: string; size?: string }) => (
    <button {...props} />
  ),
}));

vi.mock("./ui/textarea", () => ({
  Textarea: ({ size: _size, ...props }: Record<string, unknown>) => {
    h.textareaProps = props;
    return <textarea {...(props as ComponentPropsWithoutRef<"textarea">)} />;
  },
}));

vi.mock("./ui/alert", () => ({
  Alert: ({ children }: { children?: ReactNode }) => <section>{children}</section>,
  AlertAction: ({ children }: { children?: ReactNode }) => <div>{children}</div>,
  AlertDescription: ({ children }: { children?: ReactNode }) => <p>{children}</p>,
}));

vi.mock("./ui/badge", () => ({
  Badge: ({ children }: { children?: ReactNode }) => <span>{children}</span>,
}));

vi.mock("./ui/empty", () => {
  const Container = ({ children }: { children?: ReactNode }) => <div>{children}</div>;
  return {
    Empty: Container,
    EmptyDescription: Container,
    EmptyMedia: Container,
    EmptyTitle: Container,
  };
});

vi.mock("./ui/sheet", () => ({
  Sheet: ({
    children,
    onOpenChange,
  }: {
    children?: ReactNode;
    onOpenChange: (open: boolean) => void;
  }) => {
    h.sheetOnOpenChange = onOpenChange;
    return <div>{children}</div>;
  },
  SheetPopup: ({ children }: { children?: ReactNode }) => <aside>{children}</aside>,
}));

vi.mock("./ui/menu", () => ({
  Menu: ({
    children,
    onOpenChange,
  }: {
    children?: ReactNode;
    onOpenChange?: (open: boolean) => void;
  }) => {
    h.menuOnOpenChange = onOpenChange ?? null;
    return <div>{children}</div>;
  },
  MenuItem: ({
    children,
    closeOnClick: _closeOnClick,
    variant: _variant,
    ...props
  }: Record<string, unknown>) => {
    h.menuItems.push({ children, ...props });
    return (
      <button {...(props as ComponentPropsWithoutRef<"button">)}>{children as ReactNode}</button>
    );
  },
  MenuPopup: ({ children, anchor: _anchor, ...props }: Record<string, unknown>) => (
    <div {...(props as ComponentPropsWithoutRef<"div">)}>{children as ReactNode}</div>
  ),
  MenuSeparator: () => <hr />,
  MenuTrigger: ({ children, render }: { children?: ReactNode; render?: ReactNode }) => (
    <>{render ?? children}</>
  ),
}));

vi.mock("./preview/useDiscoveredLocalServers", () => ({
  useDiscoveredLocalServers: () => h.previewServers,
}));

vi.mock("./preview/previewBridge", () => ({
  previewBridge: previewBridgeMock,
}));

vi.mock("@pierre/diffs/react", () => ({
  WorkerPoolContextProvider: ({ children, ...props }: Record<string, unknown>) => {
    h.workerProviderProps = props;
    return <>{children as ReactNode}</>;
  },
  useWorkerPool: () => h.workerPool,
}));

vi.mock("@pierre/diffs/worker/worker.js?worker", () => ({
  default: function MockDiffWorker() {
    if (h.workerThrows) throw new Error("worker unavailable");
  },
}));

vi.mock("../hooks/useTheme", () => ({
  useTheme: () => ({ resolvedTheme: "dark" }),
}));

vi.mock("../state/server", () => ({
  primaryServerKeybindingsAtom: { key: "keybindings" },
}));

import { AppSidebarLayout } from "./AppSidebarLayout";
import { DiffWorkerError, DiffWorkerPoolProvider } from "./DiffWorkerPoolProvider";
import { RightPanelSheet } from "./RightPanelSheet";
import { ComposerPendingApprovalPanel } from "./chat/ComposerPendingApprovalPanel";
import { ComposerPendingReviewComments } from "./chat/ComposerPendingReviewComments";
import { ComposerPlanFollowUpBanner } from "./chat/ComposerPlanFollowUpBanner";
import { ThreadErrorBanner } from "./chat/ThreadErrorBanner";
import FileTreeContextMenu from "./files/FileTreeContextMenu";
import { LocalCommentAnnotation } from "./files/LocalCommentAnnotation";
import { PreviewEmptyState } from "./preview/PreviewEmptyState";
import { PreviewLocalServerCard } from "./preview/PreviewLocalServerCard";
import { PreviewMoreMenu } from "./preview/PreviewMoreMenu";
import { ZoomIndicator } from "./preview/ZoomIndicator";
import { SettingsSidebarNav } from "./settings/SettingsSidebarNav";
import {
  SettingResetButton,
  SettingsPageContainer,
  SettingsRow,
  SettingsSection,
} from "./settings/settingsLayout";

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

function buttonWithText(container: HTMLElement, text: string): HTMLButtonElement {
  const button = Array.from(container.querySelectorAll("button")).find(
    (candidate) => candidate.textContent?.trim() === text,
  );
  if (!button) throw new Error(`Missing button ${text}`);
  return button;
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  h.atomValue = [];
  h.canGoBack = false;
  h.menuItems = [];
  h.menuOnOpenChange = null;
  h.navigate.mockReset().mockResolvedValue(undefined);
  h.previewServers = [];
  h.setOpenMobile.mockReset();
  h.sheetOnOpenChange = null;
  h.sidebarIsMobile = false;
  h.sidebarProps = null;
  h.textareaProps = null;
  h.toggleSidebar.mockReset();
  h.workerPool = null;
  h.workerProviderProps = null;
  h.workerThrows = false;
  for (const mock of Object.values(previewBridgeMock)) mock.mockClear();
});

afterEach(async () => {
  for (const mounted of mountedTrees.splice(0)) {
    await act(async () => mounted.root.unmount());
    mounted.container.remove();
  }
  document.body.replaceChildren();
  vi.useRealTimers();
  vi.restoreAllMocks();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("small composer surfaces", () => {
  it("renders every approval summary and pending position", async () => {
    const mounted = await mount(
      <ComposerPendingApprovalPanel
        approval={{
          requestId: ApprovalRequestId.make("approval-1"),
          requestKind: "command",
          createdAt: "2026-07-16T00:00:00.000Z",
        }}
        pendingCount={1}
      />,
    );
    expect(mounted.container.textContent).toContain("Command approval requested");
    expect(mounted.container.textContent).not.toContain("1/1");

    for (const [requestKind, summary] of [
      ["file-read", "File-read approval requested"],
      ["file-change", "File-change approval requested"],
    ] as const) {
      await rerender(
        mounted,
        <ComposerPendingApprovalPanel
          approval={{
            requestId: ApprovalRequestId.make(`approval-${requestKind}`),
            requestKind,
            createdAt: "2026-07-16T00:00:00.000Z",
          }}
          pendingCount={3}
        />,
      );
      expect(mounted.container.textContent).toContain(summary);
      expect(mounted.container.textContent).toContain("1/3");
    }
  });

  it("renders optional plan and error content and invokes dismiss", async () => {
    const onDismiss = vi.fn();
    const mounted = await mount(<ComposerPlanFollowUpBanner planTitle={null} />);
    expect(mounted.container.textContent).toBe("Plan Ready");
    await rerender(mounted, <ComposerPlanFollowUpBanner planTitle="Ship it" />);
    expect(mounted.container.textContent).toContain("Ship it");

    await rerender(mounted, <ThreadErrorBanner error={null} />);
    expect(mounted.container.innerHTML).toBe("");
    await rerender(mounted, <ThreadErrorBanner error="Provider failed" />);
    expect(mounted.container.textContent).toContain("Provider failed");
    expect(mounted.container.querySelector("button")).toBeNull();
    await rerender(mounted, <ThreadErrorBanner error="Provider failed" onDismiss={onDismiss} />);
    await click(mounted.container.querySelector("button")!);
    expect(onDismiss).toHaveBeenCalledOnce();
  });

  it("hides empty review comments and removes populated comments", async () => {
    const onRemove = vi.fn();
    const mounted = await mount(
      <ComposerPendingReviewComments comments={[]} onRemove={onRemove} />,
    );
    expect(mounted.container.innerHTML).toBe("");

    await rerender(
      mounted,
      <ComposerPendingReviewComments
        className="custom-comments"
        comments={[
          {
            id: "comment-1",
            sectionId: "section-1",
            sectionTitle: "Review",
            filePath: "src/app.ts",
            startIndex: 1,
            endIndex: 2,
            rangeLabel: "lines 2-3",
            text: "Please simplify this.",
            diff: "+const value = true;",
          },
        ]}
        onRemove={onRemove}
      />,
    );
    expect(mounted.container.textContent).toContain("src/app.ts lines 2-3");
    await click(mounted.container.querySelector("button")!);
    expect(onRemove).toHaveBeenCalledWith("comment-1");
  });
});

describe("file annotations and context menus", () => {
  it("handles saved-comment deletion and draft keyboard/button actions", async () => {
    const onCancel = vi.fn();
    const onComment = vi.fn();
    const onDelete = vi.fn();
    const mounted = await mount(
      <LocalCommentAnnotation
        kind="comment"
        rangeLabel="4-6"
        text="Stored comment"
        onCancel={onCancel}
        onComment={onComment}
        onDelete={onDelete}
      />,
    );
    expect(mounted.container.textContent).toContain("Stored comment");
    await click(mounted.container.querySelector("button")!);
    expect(onDelete).toHaveBeenCalledOnce();

    await rerender(
      mounted,
      <LocalCommentAnnotation
        kind="draft"
        rangeLabel="4-6"
        text=""
        onCancel={onCancel}
        onComment={onComment}
        onDelete={onDelete}
      />,
    );
    const textarea = mounted.container.querySelector("textarea")!;
    await act(async () => {
      textarea.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    });
    expect(onCancel).toHaveBeenCalledOnce();
    await click(buttonWithText(mounted.container, "Cancel"));
    expect(onCancel).toHaveBeenCalledTimes(2);

    await act(async () => {
      (h.textareaProps!.onChange as (event: { target: { value: string } }) => void)({
        target: { value: "  Please change this.  " },
      });
    });
    await click(buttonWithText(mounted.container, "Comment"));
    expect(onComment).toHaveBeenCalledWith("Please change this.");
    await act(async () => {
      textarea.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Enter", ctrlKey: true, bubbles: true }),
      );
    });
    expect(onComment).toHaveBeenCalledTimes(2);
  });

  it("filters unavailable menu actions and closes after actions or dismissal", async () => {
    const onCopyPath = vi.fn();
    const onDelete = vi.fn();
    const onClose = vi.fn();
    const model = {
      groups: [
        [
          { id: "copy-path" as const, label: "Copy Path", enabled: true },
          { id: "rename" as const, label: "Rename", enabled: true },
        ],
        [{ id: "delete" as const, label: "Delete", enabled: false, destructive: true }],
      ],
    };
    const mounted = await mount(
      <FileTreeContextMenu model={model} actions={{}} anchor={null} onClose={onClose} />,
    );
    expect(mounted.container.innerHTML).toBe("");

    h.menuItems = [];
    await rerender(
      mounted,
      <FileTreeContextMenu
        model={model}
        actions={{ onCopyPath, onDelete }}
        anchor={null}
        onClose={onClose}
      />,
    );
    expect(mounted.container.textContent).toContain("Copy Path");
    expect(mounted.container.textContent).not.toContain("Rename");
    expect(buttonWithText(mounted.container, "Delete").disabled).toBe(true);
    await click(buttonWithText(mounted.container, "Copy Path"));
    expect(onCopyPath).toHaveBeenCalledOnce();
    expect(onClose).toHaveBeenCalledOnce();
    h.menuOnOpenChange?.(true);
    h.menuOnOpenChange?.(false);
    expect(onClose).toHaveBeenCalledTimes(2);
  });
});

describe("preview empty and menu surfaces", () => {
  const environmentId = EnvironmentId.make("environment-local");

  it("renders empty discovery and all server-label variants", async () => {
    const onOpenUrl = vi.fn();
    const mounted = await mount(
      <PreviewEmptyState environmentId={environmentId} onOpenUrl={onOpenUrl} />,
    );
    expect(mounted.container.textContent).toContain("No preview yet");

    h.previewServers = [
      {
        host: "localhost",
        port: 3000,
        url: "http://localhost:3000",
        processName: "Vite",
        pid: 1,
        terminal: null,
        source: "scanner",
        listening: true,
      },
    ];
    await rerender(
      mounted,
      <PreviewEmptyState environmentId={environmentId} onOpenUrl={onOpenUrl} />,
    );
    expect(mounted.container.textContent).toContain("Vite");
    await click(mounted.container.querySelector("button")!);
    expect(onOpenUrl).toHaveBeenCalledWith("http://localhost:3000");

    for (const [source, listening, expected] of [
      ["scanner", true, "Listening"],
      ["configured", false, "Configured"],
      ["recent", false, "Recently seen"],
    ] as const) {
      await rerender(
        mounted,
        <PreviewLocalServerCard
          server={{
            host: "127.0.0.1",
            port: 4000,
            url: "http://127.0.0.1:4000",
            processName: null,
            pid: null,
            terminal: null,
            source,
            listening,
          }}
          onOpen={onOpenUrl}
        />,
      );
      expect(mounted.container.textContent).toContain(expected);
    }
  });

  it("keeps only the device toolbar for a host without page tools", async () => {
    h.menuItems = [];
    const mounted = await mount(
      <PreviewMoreMenu
        tabId="tab-1"
        pageTools={false}
        zoomFactor={1}
        deviceToolbarVisible={false}
        onToggleDeviceToolbar={vi.fn()}
        onOpenChange={vi.fn()}
      />,
    );
    // The toolbar's Reload reloads a framed page; it needs no page script.
    expect(h.menuItems.map((item) => item.children)).toEqual(["Show device toolbar"]);
    expect(mounted.container.querySelector('[role="group"][aria-label="Zoom"]')).toBeNull();
  });

  it("guards tab operations and invokes bridge/device/storage actions", async () => {
    const onToggleDeviceToolbar = vi.fn();
    const mounted = await mount(
      <PreviewMoreMenu
        tabId={null}
        pageTools
        zoomFactor={1}
        deviceToolbarVisible={false}
        onToggleDeviceToolbar={onToggleDeviceToolbar}
        onOpenChange={vi.fn()}
      />,
    );
    expect(mounted.container.textContent).toContain("100%");
    expect(mounted.container.querySelector('[role="group"][aria-label="Zoom"]')).not.toBeNull();
    const hardReloadWithoutTab = h.menuItems.find((item) => item.children === "Hard reload")
      ?.onClick as (() => void) | undefined;
    hardReloadWithoutTab?.();
    expect(previewBridgeMock.hardReload).not.toHaveBeenCalled();

    h.menuItems = [];
    await rerender(
      mounted,
      <PreviewMoreMenu
        tabId="tab-1"
        pageTools
        zoomFactor={1.25}
        deviceToolbarVisible
        onToggleDeviceToolbar={onToggleDeviceToolbar}
        onOpenChange={vi.fn()}
      />,
    );
    expect(mounted.container.textContent).toContain("Hide device toolbar");
    await click(buttonWithText(mounted.container, "Hard reload"));
    await click(buttonWithText(mounted.container, "Open DevTools"));
    await click(buttonWithText(mounted.container, "Hide device toolbar"));
    await click(mounted.container.querySelector('button[aria-label="Zoom out"]')!);
    await click(mounted.container.querySelector('button[aria-label="Zoom in"]')!);
    await click(mounted.container.querySelector('button[aria-label="Reset zoom"]')!);
    await click(buttonWithText(mounted.container, "Clear cookies"));
    await click(buttonWithText(mounted.container, "Clear cache"));
    await act(async () => Promise.resolve());

    expect(previewBridgeMock.hardReload).toHaveBeenCalledWith("tab-1");
    expect(previewBridgeMock.openDevTools).toHaveBeenCalledWith("tab-1");
    expect(previewBridgeMock.zoomOut).toHaveBeenCalledWith("tab-1");
    expect(previewBridgeMock.zoomIn).toHaveBeenCalledWith("tab-1");
    expect(previewBridgeMock.resetZoom).toHaveBeenCalledWith("tab-1");
    expect(previewBridgeMock.clearCookies).toHaveBeenCalledExactlyOnceWith("tab-1");
    expect(previewBridgeMock.clearCache).toHaveBeenCalledExactlyOnceWith("tab-1");
    expect(onToggleDeviceToolbar).toHaveBeenCalledOnce();
  });

  it("shows zoom changes, replaces pending timers, and hides after the delay", async () => {
    vi.useFakeTimers();
    const mounted = await mount(<ZoomIndicator zoomFactor={1} />);
    expect(mounted.container.firstElementChild?.getAttribute("aria-hidden")).toBe("true");

    await rerender(mounted, <ZoomIndicator zoomFactor={1.2} />);
    expect(mounted.container.textContent).toBe("120%");
    expect(mounted.container.firstElementChild?.getAttribute("aria-hidden")).toBe("false");
    await rerender(mounted, <ZoomIndicator zoomFactor={1.3} />);
    await act(async () => vi.advanceTimersByTime(1_500));
    expect(mounted.container.firstElementChild?.getAttribute("aria-hidden")).toBe("true");
  });
});

describe("layout and navigation surfaces", () => {
  it("closes right-panel sheets only when their open state becomes false", async () => {
    const onClose = vi.fn();
    const mounted = await mount(
      <RightPanelSheet open onClose={onClose}>
        Panel content
      </RightPanelSheet>,
    );
    expect(mounted.container.textContent).toContain("Panel content");
    const eventDetails = {
      cancel: vi.fn(),
      event: new KeyboardEvent("keydown"),
      reason: "none",
    };
    h.sheetOnOpenChange?.(true, eventDetails);
    h.sheetOnOpenChange?.(false, eventDetails);
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("collapses an expanded child on first Escape and closes on second Escape", async () => {
    const onClose = vi.fn();
    const order: string[] = [];
    let expanded = true;
    const consumeEscapeClose = vi.fn(() => {
      if (!expanded) return false;
      expanded = false;
      order.push("collapse");
      return true;
    });
    onClose.mockImplementation(() => order.push("close"));
    await mount(
      <RightPanelSheet open onClose={onClose} consumeEscapeClose={consumeEscapeClose}>
        <button aria-expanded="true">Expanded child</button>
      </RightPanelSheet>,
    );
    const event = new KeyboardEvent("keydown", { key: "Escape", bubbles: true });
    Object.defineProperty(event, "target", { value: document.body });
    const firstCancel = vi.fn(() => order.push("cancel"));

    h.sheetOnOpenChange?.(false, {
      cancel: firstCancel,
      event,
      reason: "escape-key",
    });

    expect(consumeEscapeClose).toHaveBeenCalledOnce();
    expect(expanded).toBe(false);
    expect(firstCancel).toHaveBeenCalledOnce();
    expect(onClose).not.toHaveBeenCalled();
    expect(order).toEqual(["collapse", "cancel"]);

    const secondCancel = vi.fn();
    h.sheetOnOpenChange?.(false, {
      cancel: secondCancel,
      event,
      reason: "escape-key",
    });

    expect(consumeEscapeClose).toHaveBeenCalledTimes(2);
    expect(secondCancel).not.toHaveBeenCalled();
    expect(onClose).toHaveBeenCalledOnce();
    expect(order).toEqual(["collapse", "cancel", "close"]);
  });

  it("renders settings sections/rows and resets values", async () => {
    const onReset = vi.fn();
    const mounted = await mount(
      <SettingsPageContainer className="custom-page">
        <SettingsSection title="Editor" icon={<span>Icon</span>} headerAction={<b>Action</b>}>
          <SettingsRow
            title="Theme"
            description="Choose a theme"
            status="Customized"
            resetAction={<SettingResetButton label="theme" onClick={onReset} />}
            control={<select aria-label="Theme" />}
          >
            Extra content
          </SettingsRow>
          <SettingsRow title="Compact" description="No optional content" />
        </SettingsSection>
      </SettingsPageContainer>,
    );
    expect(mounted.container.textContent).toContain("Editor");
    expect(mounted.container.textContent).toContain("Customized");
    expect(mounted.container.textContent).toContain("Extra content");
    await click(mounted.container.querySelector('button[aria-label="Reset theme to default"]')!);
    expect(onReset).toHaveBeenCalledOnce();
  });

  it("navigates settings on mobile and chooses history or home for Back", async () => {
    h.sidebarIsMobile = true;
    const mounted = await mount(<SettingsSidebarNav pathname="/settings/providers" />);
    expect(mounted.container.querySelector('button[data-active="true"]')?.textContent).toContain(
      "Providers",
    );
    await click(buttonWithText(mounted.container, "General"));
    expect(h.setOpenMobile).toHaveBeenCalledWith(false);
    expect(h.navigate).toHaveBeenCalledWith({ to: "/settings/general", replace: true });

    await click(buttonWithText(mounted.container, "Back"));
    expect(h.navigate).toHaveBeenCalledWith({ to: "/" });

    const back = vi.spyOn(window.history, "back").mockImplementation(() => undefined);
    h.canGoBack = true;
    h.sidebarIsMobile = false;
    await rerender(mounted, <SettingsSidebarNav pathname="/settings/keybindings" />);
    await click(buttonWithText(mounted.container, "Back"));
    expect(back).toHaveBeenCalledOnce();
  });

  it("handles desktop menu navigation, shortcut toggles, and resize acceptance", async () => {
    const unsubscribe = vi.fn();
    const menuActions: { current?: (action: string) => void } = {};
    Object.assign(window, {
      desktopBridge: {
        onMenuAction: (listener: (action: string) => void) => {
          menuActions.current = listener;
          return unsubscribe;
        },
      },
    });
    h.atomValue = [
      {
        command: "sidebar.toggle",
        shortcut: {
          key: "b",
          metaKey: false,
          ctrlKey: false,
          shiftKey: false,
          altKey: false,
          modKey: true,
        },
      },
    ];
    const mounted = await mount(<AppSidebarLayout>Workspace</AppSidebarLayout>);
    expect(mounted.container.textContent).toContain("Workspace");
    menuActions.current?.("unknown");
    menuActions.current?.("open-settings");
    expect(h.navigate).toHaveBeenCalledWith({ to: "/settings" });

    window.dispatchEvent(new KeyboardEvent("keydown", { key: "x" }));
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "b", ctrlKey: true }));
    expect(h.toggleSidebar).toHaveBeenCalledOnce();

    const shouldAcceptWidth = (
      h.sidebarProps!.resizable as {
        shouldAcceptWidth: (input: {
          nextWidth: number;
          wrapper: { clientWidth: number };
        }) => boolean;
      }
    ).shouldAcceptWidth;
    expect(shouldAcceptWidth({ nextWidth: 300, wrapper: { clientWidth: 1_000 } })).toBe(true);
    expect(shouldAcceptWidth({ nextWidth: 500, wrapper: { clientWidth: 1_000 } })).toBe(false);

    await act(async () => mounted.root.unmount());
    mountedTrees.splice(mountedTrees.indexOf(mounted), 1);
    expect(unsubscribe).toHaveBeenCalledOnce();
  });
});

describe("diff worker provider", () => {
  it("sizes the pool, synchronizes themes, and wraps creation failures", async () => {
    const setRenderOptions = vi.fn(() => Promise.resolve());
    h.workerPool = {
      getDiffRenderOptions: () => ({ theme: "pierre-light", marker: "keep" }),
      setRenderOptions,
    };
    Object.defineProperty(navigator, "hardwareConcurrency", { configurable: true, value: 20 });
    const mounted = await mount(
      <DiffWorkerPoolProvider>
        <span>Diff content</span>
      </DiffWorkerPoolProvider>,
    );
    await act(async () => Promise.resolve());
    expect(mounted.container.textContent).toContain("Diff content");
    expect((h.workerProviderProps!.poolOptions as { poolSize: number }).poolSize).toBe(6);
    expect(setRenderOptions).toHaveBeenCalledWith({ theme: "pierre-dark", marker: "keep" });

    const workerFactory = (h.workerProviderProps!.poolOptions as { workerFactory: () => unknown })
      .workerFactory;
    expect(workerFactory()).toBeDefined();
    h.workerThrows = true;
    expect(workerFactory).toThrow(DiffWorkerError);
    expect(
      () =>
        new DiffWorkerError({
          operation: "create-worker",
          themeName: "pierre-dark",
          cause: new Error("failure"),
        }).message,
    ).not.toThrow();
  });

  it("does nothing without a pool and skips matching themes", async () => {
    h.workerPool = null;
    const mounted = await mount(<DiffWorkerPoolProvider />);
    await act(async () => Promise.resolve());

    const setRenderOptions = vi.fn(() => Promise.resolve());
    h.workerPool = {
      getDiffRenderOptions: () => ({ theme: "pierre-dark" }),
      setRenderOptions,
    };
    await rerender(mounted, <DiffWorkerPoolProvider />);
    await act(async () => Promise.resolve());
    expect(setRenderOptions).not.toHaveBeenCalled();
  });
});
