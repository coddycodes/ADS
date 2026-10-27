// @vitest-environment happy-dom

import type { PreviewSessionSnapshot } from "@coddy/contracts";
import { act, type ReactElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterAll, afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import type { RightPanelSurface } from "~/rightPanelStore";

const testState = vi.hoisted(() => ({
  contextMenuShow: vi.fn(),
  localApiAvailable: true,
}));

vi.mock("~/localApi", () => ({
  readLocalApi: () =>
    testState.localApiAvailable ? { contextMenu: { show: testState.contextMenuShow } } : undefined,
}));

vi.mock("~/hooks/useTheme", () => ({ useTheme: () => ({ resolvedTheme: "light" }) }));

vi.mock("~/components/ui/tooltip", () => ({
  Tooltip: ({ children }: { children?: React.ReactNode }) => <>{children}</>,
  TooltipTrigger: ({
    render,
    children,
  }: {
    render?: React.ReactNode;
    children?: React.ReactNode;
  }) => <>{render ?? children}</>,
  TooltipPopup: ({ children }: { children?: React.ReactNode }) => (
    <span data-testid="tooltip">{children}</span>
  ),
}));

vi.mock("~/components/ui/menu", () => ({
  Menu: ({ children }: { children?: React.ReactNode }) => <div>{children}</div>,
  MenuTrigger: ({ children, ...props }: React.ComponentProps<"button">) => (
    <button type="button" {...props}>
      {children}
    </button>
  ),
  MenuPopup: ({ children }: { children?: React.ReactNode }) => <div>{children}</div>,
  MenuItem: ({ children, ...props }: React.ComponentProps<"button">) => (
    <button type="button" {...props}>
      {children}
    </button>
  ),
}));

vi.mock("~/components/ui/scroll-area", () => ({
  ScrollArea: ({
    children,
    ref,
    hideScrollbars,
    scrollFade,
    ...props
  }: React.ComponentProps<"div"> & { hideScrollbars?: boolean; scrollFade?: boolean }) => {
    void hideScrollbars;
    void scrollFade;
    return (
      <div ref={ref} {...props}>
        {children}
      </div>
    );
  },
}));

vi.mock("./preview/PreviewPanelShell", () => ({
  PreviewPanelShell: ({ children, mode }: { children?: React.ReactNode; mode: string }) => (
    <section data-mode={mode}>{children}</section>
  ),
}));

vi.mock("./chat/PierreEntryIcon", () => ({
  PierreEntryIcon: ({ pathValue }: { pathValue: string }) => <span data-file-icon={pathValue} />,
}));

import { RightPanelTabs } from "./RightPanelTabs";

type TabsProps = Parameters<typeof RightPanelTabs>[0];

interface MountedTree {
  readonly container: HTMLDivElement;
  readonly root: Root;
}

const mountedTrees: MountedTree[] = [];
const suiteScrollIntoViewDescriptor = Object.getOwnPropertyDescriptor(
  HTMLElement.prototype,
  "scrollIntoView",
);
let originalScrollIntoViewDescriptor: PropertyDescriptor | undefined;

async function mount(element: ReactElement): Promise<void> {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  mountedTrees.push({ container, root });
  await act(async () => root.render(element));
}

async function click(element: HTMLElement): Promise<void> {
  await act(async () => element.click());
}

function buttonWithText(text: string): HTMLButtonElement {
  const button = Array.from(document.querySelectorAll<HTMLButtonElement>("button")).find(
    (candidate) => candidate.textContent?.trim() === text,
  );
  expect(button).toBeDefined();
  return button!;
}

function snapshot(navStatus: PreviewSessionSnapshot["navStatus"]): PreviewSessionSnapshot {
  return {
    threadId: "thread-1",
    tabId: "preview-1",
    navStatus,
    canGoBack: false,
    canGoForward: false,
    updatedAt: "2026-07-14T00:00:00.000Z",
  };
}

const fileSurface: RightPanelSurface = {
  id: "file:src/main.ts",
  kind: "file",
  relativePath: "src/main.ts",
  revealLine: null,
  revealRequestId: 0,
};
const previewSurface: RightPanelSurface = {
  id: "browser:1",
  kind: "preview",
  resourceId: "preview-1",
};

function tabsProps(overrides: Partial<TabsProps> = {}): TabsProps {
  return {
    mode: "inline",
    surfaces: [fileSurface, previewSurface],
    activeSurfaceId: fileSurface.id,
    pendingSurfaceIds: new Set<string>(),
    previewSessions: {
      "preview-1": snapshot({
        _tag: "Success",
        url: "https://example.test/docs",
        title: "Project docs",
      }),
    },
    previewDesktopByTabId: {},
    terminalLabelsById: new Map<string, string>(),
    onActivate: vi.fn(),
    onCloseSurface: vi.fn(),
    onCloseOtherSurfaces: vi.fn(),
    onCloseSurfacesToRight: vi.fn(),
    onCloseAllSurfaces: vi.fn(),
    onCopyFilePath: vi.fn(),
    onAddBrowser: vi.fn(),
    onAddTerminal: vi.fn(),
    onAddDiff: vi.fn(),
    onAddFiles: vi.fn(),
    onAddSourceControl: vi.fn(),
    browserAvailable: true,
    diffAvailable: true,
    filesAvailable: true,
    sourceControlAvailable: true,
    children: <div>active content</div>,
    ...overrides,
  };
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  testState.localApiAvailable = true;
  testState.contextMenuShow.mockReset().mockResolvedValue(null);
  originalScrollIntoViewDescriptor = Object.getOwnPropertyDescriptor(
    HTMLElement.prototype,
    "scrollIntoView",
  );
  Object.defineProperty(HTMLElement.prototype, "scrollIntoView", {
    configurable: true,
    value: vi.fn(),
    writable: true,
  });
});

afterEach(async () => {
  for (const mounted of mountedTrees.splice(0)) {
    await act(async () => mounted.root.unmount());
    mounted.container.remove();
  }
  document.body.replaceChildren();
  if (originalScrollIntoViewDescriptor) {
    Object.defineProperty(
      HTMLElement.prototype,
      "scrollIntoView",
      originalScrollIntoViewDescriptor,
    );
  } else {
    Reflect.deleteProperty(HTMLElement.prototype, "scrollIntoView");
  }
  originalScrollIntoViewDescriptor = undefined;
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
  vi.restoreAllMocks();
});

afterAll(() => {
  expect(Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollIntoView")).toEqual(
    suiteScrollIntoViewDescriptor,
  );
});

describe("RightPanelTabs mounted interactions", () => {
  it("activates, closes, and middle-clicks rendered tabs", async () => {
    const onActivate = vi.fn();
    const onCloseSurface = vi.fn();
    await mount(<RightPanelTabs {...tabsProps({ onActivate, onCloseSurface })} />);

    expect(document.body.textContent).toContain("main.ts");
    expect(document.body.textContent).toContain("Project docs");
    await click(buttonWithText("Project docs"));
    expect(onActivate).toHaveBeenCalledWith(previewSurface);

    await click(document.querySelector<HTMLButtonElement>('[aria-label="Close main.ts"]')!);
    expect(onCloseSurface).toHaveBeenCalledWith(fileSurface);

    const previewTab = buttonWithText("Project docs").parentElement!;
    await act(async () => {
      previewTab.dispatchEvent(new MouseEvent("auxclick", { bubbles: true, button: 1 }));
      previewTab.dispatchEvent(new MouseEvent("auxclick", { bubbles: true, button: 0 }));
      previewTab.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, button: 0 }));
      previewTab.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, button: 1 }));
    });
    expect(onCloseSurface).toHaveBeenLastCalledWith(previewSurface);
  });

  it("opens the native tab context menu and copies a file path", async () => {
    const onCopyFilePath = vi.fn();
    testState.contextMenuShow.mockResolvedValue("copy-path");
    await mount(<RightPanelTabs {...tabsProps({ onCopyFilePath })} />);

    const fileTab = buttonWithText("main.ts").parentElement!;
    await act(async () => {
      fileTab.dispatchEvent(
        new MouseEvent("contextmenu", {
          bubbles: true,
          cancelable: true,
          clientX: 12,
          clientY: 34,
        }),
      );
    });

    expect(testState.contextMenuShow).toHaveBeenCalledWith(
      expect.arrayContaining([
        expect.objectContaining({ id: "copy-path", label: "Copy path" }),
        expect.objectContaining({ id: "close", label: "Close" }),
      ]),
      { x: 12, y: 34 },
    );
    expect(onCopyFilePath).toHaveBeenCalledWith("src/main.ts");
  });

  it("uses real empty-state buttons and respects unavailable surfaces", async () => {
    const onAddTerminal = vi.fn();
    const onAddBrowser = vi.fn();
    await mount(
      <RightPanelTabs
        {...tabsProps({
          surfaces: [],
          activeSurfaceId: null,
          browserAvailable: false,
          onAddTerminal,
          onAddBrowser,
        })}
      />,
    );

    await click(buttonWithText("TerminalStart a shell in this workspace."));
    expect(onAddTerminal).toHaveBeenCalledOnce();

    const browserButton = buttonWithText("BrowserOpen a local app or URL.");
    expect(browserButton.getAttribute("aria-disabled")).toBe("true");
    await click(browserButton);
    expect(onAddBrowser).not.toHaveBeenCalled();
    expect(document.body.textContent).not.toContain("Activity");
  });

  it("adds a surface from the rendered panel menu", async () => {
    const onAddFiles = vi.fn();
    await mount(<RightPanelTabs {...tabsProps({ onAddFiles })} />);

    await click(document.querySelector<HTMLButtonElement>('[aria-label="Add panel surface"]')!);
    await click(buttonWithText("Files"));
    expect(onAddFiles).toHaveBeenCalledOnce();
    expect(document.body.textContent).not.toContain("Activity");
  });

  it("hides the device surface until device previews are on", async () => {
    await mount(<RightPanelTabs {...tabsProps({ surfaces: [], activeSurfaceId: null })} />);
    expect(document.body.textContent).not.toContain("Device");
  });

  it("offers the device surface and titles its tab", async () => {
    const onAddDevice = vi.fn();
    await mount(
      <RightPanelTabs
        {...tabsProps({
          surfaces: [{ id: "device", kind: "device" }],
          activeSurfaceId: "device",
          onAddDevice,
        })}
      />,
    );
    expect(document.querySelector('[aria-label="Close Device"]')).not.toBeNull();
    await click(document.querySelector<HTMLButtonElement>('[aria-label="Add panel surface"]')!);
    // The tab itself is also titled "Device"; the menu item comes after it.
    const menuItem = Array.from(document.querySelectorAll<HTMLButtonElement>("button")).findLast(
      (candidate) => candidate.textContent?.trim() === "Device",
    )!;
    await click(menuItem);
    expect(onAddDevice).toHaveBeenCalledOnce();
  });

  it("offers the device surface in the empty state", async () => {
    const onAddDevice = vi.fn();
    await mount(
      <RightPanelTabs {...tabsProps({ surfaces: [], activeSurfaceId: null, onAddDevice })} />,
    );
    await click(buttonWithText("DeviceRun an Android emulator."));
    expect(onAddDevice).toHaveBeenCalledOnce();
  });

  it("can suppress generic add-surface controls for an activity-only panel", async () => {
    await mount(<RightPanelTabs {...tabsProps({ allowAddSurfaces: false })} />);

    expect(document.querySelector('[aria-label="Add panel surface"]')).toBeNull();
  });

  it("renders the Activity singleton with the Bot icon", async () => {
    const onActivate = vi.fn();
    const activitySurface: RightPanelSurface = {
      id: "activity",
      kind: "activity",
      scope: { _tag: "thread" },
      section: "subagents",
      selectedRecordKind: null,
      selectedRecordId: null,
    };
    await mount(
      <RightPanelTabs
        {...tabsProps({
          surfaces: [activitySurface],
          activeSurfaceId: "activity",
          onActivate,
        })}
      />,
    );

    const activityButton = buttonWithText("Activity");
    expect(activityButton).toBeDefined();
    expect(document.querySelector(".lucide-bot")).not.toBeNull();
    expect(document.querySelector('[data-testid="tooltip"]')).toBeNull();
    await click(activityButton);
    expect(onActivate).toHaveBeenCalledWith(activitySurface);
  });

  it("renders every surface title, icon, preview fallback, and terminal label", async () => {
    const surfaces = [
      { id: "diff", kind: "diff" },
      { id: "source", kind: "sourceControl" },
      { id: "files", kind: "files" },
      { id: "terminal:1", kind: "terminal", activeTerminalId: "term-1" },
      { id: "terminal:2", kind: "terminal", activeTerminalId: "term-2" },
      { id: "plan", kind: "plan" },
      { id: "preview:idle", kind: "preview", resourceId: "idle" },
      { id: "preview:host", kind: "preview", resourceId: "host" },
      { id: "preview:invalid", kind: "preview", resourceId: "invalid" },
      { id: "preview:missing", kind: "preview", resourceId: "missing" },
    ] as RightPanelSurface[];
    await mount(
      <RightPanelTabs
        {...tabsProps({
          maximized: true,
          surfaces,
          activeSurfaceId: "diff",
          pendingSurfaceIds: new Set(["diff"]),
          terminalLabelsById: new Map([["term-1", "Build shell"]]),
          previewSessions: {
            idle: snapshot({ _tag: "Idle" }),
            host: snapshot({ _tag: "Success", url: "https://docs.example.test/path", title: " " }),
            invalid: snapshot({ _tag: "Success", url: "not a url", title: "" }),
          },
        })}
      />,
    );

    expect(document.body.textContent).toContain("Diff");
    expect(document.body.textContent).toContain("Source Control");
    expect(document.body.textContent).toContain("Files");
    expect(document.body.textContent).toContain("Build shell");
    expect(document.body.textContent).toContain("Terminal 2");
    expect(document.body.textContent).toContain("Plan");
    expect(document.body.textContent).toContain("docs.example.test");
    expect(document.body.textContent?.match(/Browser/g)?.length).toBeGreaterThanOrEqual(3);
    expect(document.querySelector('[aria-label="Close Diff"] span')).not.toBeNull();
  });

  it("titles pull request tabs by the host's number format with the request icon", async () => {
    await mount(
      <RightPanelTabs
        {...tabsProps({
          surfaces: [
            {
              id: "pullRequest:12",
              kind: "pullRequest",
              number: 12,
              provider: "github",
              tab: "conversation",
            },
            {
              id: "pullRequest:89",
              kind: "pullRequest",
              number: 89,
              provider: "gitlab",
              tab: "files",
            },
          ] as RightPanelSurface[],
          activeSurfaceId: "pullRequest:12",
        })}
      />,
    );
    for (const title of ["#12", "!89"]) {
      const tab = buttonWithText(title);
      expect(tab.querySelector("svg.lucide-git-pull-request")).not.toBeNull();
      expect(document.querySelector(`[aria-label="Close ${title}"]`)).not.toBeNull();
    }
  });

  it("renders a local globe icon for preview tabs without a remote image", async () => {
    await mount(<RightPanelTabs {...tabsProps()} />);
    expect(buttonWithText("Project docs").querySelector("svg.lucide-earth")).not.toBeNull();
    expect(document.querySelector("img")).toBeNull();
  });

  it("uses the live desktop title instead of a stale server title", async () => {
    await mount(
      <RightPanelTabs
        {...tabsProps({
          previewDesktopByTabId: {
            "preview-1": {
              url: "https://example.test/current",
              title: "Current page",
              canGoBack: true,
              canGoForward: false,
              loading: false,
              zoomFactor: 1,
              controller: "human",
            },
          },
        })}
      />,
    );

    expect(buttonWithText("Current page")).toBeDefined();
    expect(
      Array.from(document.querySelectorAll('[data-testid="tooltip"]')).find(
        (tooltip) => tooltip.textContent === "Current page",
      ),
    ).toBeDefined();
    expect(document.body.textContent).not.toContain("Project docs");
  });

  it("dispatches every native context-menu close action and ignores unavailable menus", async () => {
    const onCloseSurface = vi.fn();
    const onCloseOtherSurfaces = vi.fn();
    const onCloseSurfacesToRight = vi.fn();
    const onCloseAllSurfaces = vi.fn();
    await mount(
      <RightPanelTabs
        {...tabsProps({
          onCloseSurface,
          onCloseOtherSurfaces,
          onCloseSurfacesToRight,
          onCloseAllSurfaces,
        })}
      />,
    );
    const previewTab = buttonWithText("Project docs").parentElement!;
    for (const action of ["close", "close-others", "close-to-right", "close-all", null]) {
      testState.contextMenuShow.mockResolvedValueOnce(action);
      await act(async () => {
        previewTab.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true }));
      });
    }
    expect(onCloseSurface).toHaveBeenCalledWith(previewSurface);
    expect(onCloseOtherSurfaces).toHaveBeenCalledWith(previewSurface);
    expect(onCloseSurfacesToRight).toHaveBeenCalledWith(previewSurface);
    expect(onCloseAllSurfaces).toHaveBeenCalledOnce();

    testState.contextMenuShow.mockResolvedValueOnce("copy-path");
    await act(async () => {
      previewTab.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true }));
    });

    testState.localApiAvailable = false;
    const callsBeforeUnavailable = testState.contextMenuShow.mock.calls.length;
    await act(async () => {
      previewTab.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true }));
    });
    expect(testState.contextMenuShow).toHaveBeenCalledTimes(callsBeforeUnavailable);
  });

  it("ignores non-middle auxiliary clicks and exposes every disabled add action", async () => {
    const onCloseSurface = vi.fn();
    await mount(
      <RightPanelTabs
        {...tabsProps({
          surfaces: [],
          activeSurfaceId: null,
          browserAvailable: false,
          diffAvailable: false,
          filesAvailable: false,
          sourceControlAvailable: false,
          onCloseSurface,
        })}
      />,
    );
    expect(document.querySelectorAll('[aria-disabled="true"]')).toHaveLength(4);

    await act(async () => {
      document.body.dispatchEvent(new MouseEvent("auxclick", { bubbles: true, button: 0 }));
      document.body.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, button: 0 }));
    });
    expect(onCloseSurface).not.toHaveBeenCalled();
  });
});
