import type { DesktopPreviewPointerEvent } from "@coddy/contracts";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";

const harness = vi.hoisted(() => ({
  event: null as DesktopPreviewPointerEvent | null,
  content: null as {
    x: number;
    y: number;
    scale: number;
    scrollLeft: number;
    scrollTop: number;
  } | null,
  active: true,
  nextActive: null as boolean | null,
  effects: [] as Array<() => void | (() => void)>,
  timeout: vi.fn(),
  clearTimeout: vi.fn(),
}));

vi.mock("react", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react")>()),
  useEffect: (effect: () => void | (() => void)) => harness.effects.push(effect),
  useState: () => [harness.active, (value: boolean) => (harness.nextActive = value)],
}));
vi.mock("~/browser/browserPointerStore", () => ({
  useBrowserPointerStore: (selector: (state: unknown) => unknown) =>
    selector({ byTabId: harness.event ? { tab: harness.event } : {} }),
}));
vi.mock("~/browser/browserSurfaceStore", () => ({
  useBrowserSurfaceStore: (selector: (state: unknown) => unknown) =>
    selector({ byTabId: harness.content ? { tab: { content: harness.content } } : {} }),
}));

import { AgentBrowserCursor } from "./AgentBrowserCursor";

beforeEach(() => {
  harness.event = null;
  harness.content = null;
  harness.active = true;
  harness.nextActive = null;
  harness.effects.length = 0;
  harness.timeout.mockReset();
  harness.clearTimeout.mockReset();
  harness.timeout.mockImplementation((callback: () => void) => {
    callback();
    return 17;
  });
  vi.stubGlobal("window", { setTimeout: harness.timeout, clearTimeout: harness.clearTimeout });
});

function pointer(overrides: Partial<DesktopPreviewPointerEvent> = {}): DesktopPreviewPointerEvent {
  return {
    tabId: "tab",
    sequence: 1,
    phase: "move",
    x: 10,
    y: 20,
    ...overrides,
  } as DesktopPreviewPointerEvent;
}

function render(controller: "human" | "agent" | "none" = "agent"): string {
  return renderToStaticMarkup(
    <AgentBrowserCursor tabId="tab" zoomFactor={2} controller={controller} />,
  );
}

describe("AgentBrowserCursor", () => {
  it("renders nothing without a pointer event", () => {
    expect(render()).toBe("");
  });

  it("positions movement with default surface values and expires activity", () => {
    harness.event = pointer();
    const markup = render("human");

    expect(markup).toContain("translate3d(20px, 40px, 0)");
    expect(markup).not.toContain("animate-ping");
    const cleanup = harness.effects[0]?.();
    expect(harness.nextActive).toBe(false);
    if (typeof cleanup === "function") cleanup();
    expect(harness.clearTimeout).toHaveBeenCalledWith(17);
  });

  it("accounts for surface scale, offsets, scroll, and click phase", () => {
    harness.event = pointer({ phase: "click", sequence: 2 });
    harness.content = { x: 5, y: 7, scale: 0.5, scrollLeft: 2, scrollTop: 3 };
    harness.active = false;
    const markup = render("none");

    expect(markup).toContain("translate3d(13px, 24px, 0)");
    expect(markup).toContain("opacity:0.35");
    expect(markup).toContain("animate-ping");
  });
});
