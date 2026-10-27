// @vitest-environment happy-dom

import { ThreadId } from "@coddy/contracts";
import { act, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

vi.mock("~/localApi", () => ({ readLocalApi: () => null }));
vi.mock("~/components/ui/tooltip", () => ({
  Tooltip: ({ children }: { children: ReactNode }) => <>{children}</>,
  TooltipTrigger: ({ render }: { render: ReactNode }) => <>{render}</>,
  TooltipPopup: ({ children }: { children: ReactNode }) => <>{children}</>,
}));
vi.mock("~/components/ui/scroll-area", () => ({
  ScrollArea: ({
    children,
    hideScrollbars: _hideScrollbars,
    scrollFade: _scrollFade,
    ...props
  }: React.ComponentProps<"div"> & { hideScrollbars?: boolean; scrollFade?: boolean }) => (
    <div {...props}>
      <div data-slot="scroll-area-viewport">{children}</div>
    </div>
  ),
}));

import { CenterPanelTabs } from "./CenterPanelTabs";

const host = { id: "chat:host", kind: "chat-host" } as const;
const chat = {
  id: "chat:thread-2",
  kind: "chat",
  threadId: ThreadId.make("thread-2"),
  providerLabel: "Claude",
} as const;

let tabContentWidth = 200;
let resizeObservers: FakeResizeObserver[] = [];

class FakeResizeObserver {
  readonly observed = new Set<Element>();

  constructor(private readonly callback: ResizeObserverCallback) {
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

  trigger(target: Element): void {
    if (!this.observed.has(target)) return;
    this.callback([], this as unknown as ResizeObserver);
  }
}

function input(hostLabel: string) {
  return {
    groupId: "group-a",
    hostLabel,
    surfaces: [host, chat],
    activeSurfaceId: host.id,
    canMoveToSplit: () => true,
    dragInProgress: false,
    onActivate: vi.fn(),
    onCloseSurface: vi.fn(),
    onCloseOtherSurfaces: vi.fn(),
    onCloseSurfacesToRight: vi.fn(),
    onCloseAllSurfaces: vi.fn(),
    onMoveToSplit: vi.fn(),
  };
}

beforeEach(() => {
  tabContentWidth = 200;
  resizeObservers = [];
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("ResizeObserver", FakeResizeObserver);
  vi.spyOn(HTMLElement.prototype, "scrollIntoView").mockImplementation(() => undefined);
  vi.spyOn(HTMLElement.prototype, "scrollBy").mockImplementation(() => undefined);
  vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockImplementation(function (
    this: HTMLElement,
  ) {
    return this.hasAttribute("data-center-panel-overflow-boundary") ? 320 : 300;
  });
  vi.spyOn(HTMLElement.prototype, "scrollWidth", "get").mockImplementation(function (
    this: HTMLElement,
  ) {
    return this.getAttribute("data-slot") === "scroll-area-viewport"
      ? tabContentWidth
      : this.clientWidth;
  });
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  document.body.replaceChildren();
});

describe("CenterPanelTabs DOM overflow measurement", () => {
  it("composes the All tabs icon into a trigger that opens its menu", async () => {
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);

    await act(async () => root.render(<CenterPanelTabs {...input("Codex")} />));
    const allTabs = container.querySelector<HTMLButtonElement>('[aria-label="All tabs"]');
    expect(allTabs?.querySelector("svg")).not.toBeNull();

    await act(async () => allTabs?.click());
    expect(document.body.querySelector('[role="menu"]')).not.toBeNull();

    await act(async () => root.unmount());
  });

  it("remeasures when a same-count label change expands the scroll content", async () => {
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);

    await act(async () => root.render(<CenterPanelTabs {...input("Codex")} />));
    const boundary = container.querySelector<HTMLElement>("[data-center-panel-overflow-boundary]");
    expect(boundary?.dataset.overflow).toBe("false");

    tabContentWidth = 560;
    await act(async () =>
      root.render(<CenterPanelTabs {...input("Codex Personal With A Much Longer Name")} />),
    );
    const tabContent = container.querySelector<HTMLElement>('[role="tablist"]');
    if (!tabContent) throw new Error("Tab content not found");
    await act(async () => {
      for (const observer of resizeObservers) observer.trigger(tabContent);
    });

    expect(boundary?.dataset.overflow).toBe("true");
    await act(async () => root.unmount());
  });
});
