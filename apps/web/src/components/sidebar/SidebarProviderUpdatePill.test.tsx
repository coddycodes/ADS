import React, { type ReactElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const harness = vi.hoisted(() => ({
  providers: [] as Array<{ checkedAt: string }>,
  view: null as Record<string, unknown> | null,
  stateValues: [] as unknown[],
  stateIndex: 0,
  setters: [] as Array<ReturnType<typeof vi.fn>>,
  effects: [] as Array<() => void | (() => void)>,
  navigate: vi.fn(),
  getView: vi.fn(),
}));

vi.mock("react", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react")>()),
  useCallback: (callback: unknown) => callback,
  useEffect: (effect: () => void | (() => void)) => {
    harness.effects.push(effect);
  },
  useState: (initial: unknown) => {
    const index = harness.stateIndex++;
    const resolved = typeof initial === "function" ? (initial as () => unknown)() : initial;
    const value = harness.stateValues[index] ?? resolved;
    const setter = vi.fn();
    harness.setters[index] = setter;
    return [value, setter];
  },
}));
vi.mock("@tanstack/react-router", () => ({ useNavigate: () => harness.navigate }));
vi.mock("@effect/atom-react", () => ({ useAtomValue: () => harness.providers }));
vi.mock("../../state/server", () => ({ primaryServerProvidersAtom: { name: "providers" } }));
vi.mock("../ProviderUpdateLaunchNotification.logic", () => ({
  getProviderUpdateSidebarPillView: (...args: unknown[]) => {
    harness.getView(...args);
    return harness.view;
  },
}));
vi.mock("../ui/tooltip", () => ({
  Tooltip: ({ children }: { children: React.ReactNode }) => <>{children}</>,
  TooltipTrigger: ({ render }: { render: React.ReactNode }) => <>{render}</>,
  TooltipPopup: ({ children }: { children: React.ReactNode }) => <span>{children}</span>,
}));

import { SidebarProviderUpdatePill } from "./SidebarProviderUpdatePill";

const views = {
  loading: {
    key: "loading",
    title: "Checking providers",
    description: "Checking for updates",
    tone: "loading",
    dismissible: false,
  },
  success: {
    key: "success",
    title: "Providers current",
    description: "All providers are current",
    tone: "success",
    dismissible: false,
    dismissAfterVisibleMs: 500,
  },
  warning: {
    key: "warning",
    title: "Update available",
    description: "A provider update is available",
    tone: "warning",
    dismissible: true,
    dismissAfterVisibleMs: 500,
  },
  error: {
    key: "error",
    title: "Update failed",
    description: "Provider update failed",
    tone: "error",
    dismissible: true,
  },
} as const;

function visit(node: React.ReactNode, entries: ReactElement[] = []): ReactElement[] {
  if (Array.isArray(node)) {
    for (const child of node) visit(child, entries);
    return entries;
  }
  if (!React.isValidElement(node)) return entries;
  entries.push(node);
  visit((node.props as { children?: React.ReactNode }).children, entries);
  const render = (node.props as { render?: React.ReactNode }).render;
  if (render) visit(render, entries);
  return entries;
}

function renderPill() {
  const tree = SidebarProviderUpdatePill();
  return { tree, markup: renderToStaticMarkup(tree) };
}

beforeEach(() => {
  harness.providers = [];
  harness.view = null;
  harness.stateValues = [];
  harness.stateIndex = 0;
  harness.setters.length = 0;
  harness.effects.length = 0;
  harness.navigate.mockReset();
  harness.getView.mockClear();
  vi.useRealTimers();
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("SidebarProviderUpdatePill", () => {
  it("returns null and derives the latest provider check time", () => {
    harness.providers = [
      { checkedAt: "2026-07-15T00:00:00.000Z" },
      { checkedAt: "2026-07-16T00:00:00.000Z" },
      { checkedAt: "2026-07-14T00:00:00.000Z" },
    ];
    expect(SidebarProviderUpdatePill()).toBeNull();
    expect(harness.getView).toHaveBeenCalledWith(
      harness.providers,
      expect.objectContaining({ visibleAfterIso: "2026-07-16T00:00:00.000Z" }),
    );
    expect(harness.setters[5]).toHaveBeenCalledWith("2026-07-16T00:00:00.000Z");
  });

  it.each(Object.values(views))("renders the $tone tone", (view) => {
    harness.view = view;
    const { markup } = renderPill();
    expect(markup).toContain(view.title);
    expect(markup).toContain(view.description);
    if (view.tone === "loading") expect(markup).toContain("animate-spin");
    if (view.dismissible) expect(markup).toContain("Dismiss provider update notice");
    else expect(markup).not.toContain("Dismiss provider update notice");
    expect(harness.setters[1]).toHaveBeenCalledWith(view);
  });

  it("opens provider settings and begins a manual dismissal", () => {
    harness.view = views.warning;
    const { tree } = renderPill();
    const elements = visit(tree);
    const main = elements.find(
      (element) =>
        element.type === "button" &&
        (element.props as Record<string, unknown>)["aria-label"] === views.warning.description,
    );
    const dismiss = elements.find(
      (element) =>
        element.type === "button" &&
        (element.props as Record<string, unknown>)["aria-label"] ===
          "Dismiss provider update notice",
    );
    if (!main || !dismiss) throw new Error("Provider update buttons not found");
    (main.props as { onClick: () => void }).onClick();
    expect(harness.navigate).toHaveBeenCalledWith({ to: "/settings/providers" });
    (dismiss.props as { onClick: () => void }).onClick();
    expect(harness.setters[2]).toHaveBeenCalledWith(null);
    expect(harness.setters[3]).toHaveBeenCalledWith("warning");
    expect(harness.setters[4]).toHaveBeenCalledWith("warning");
  });

  it("does not restart an exit already in progress", () => {
    harness.view = views.warning;
    harness.stateValues = [new Set(), views.warning, null, "warning", null, undefined];
    const { tree, markup } = renderPill();
    expect(markup).toContain("pointer-events-none");
    const dismiss = visit(tree).find(
      (element) =>
        element.type === "button" &&
        (element.props as Record<string, unknown>)["aria-label"] ===
          "Dismiss provider update notice",
    );
    if (!dismiss) throw new Error("Dismiss button not found");
    (dismiss.props as { onClick: () => void }).onClick();
    expect(harness.setters[2]).not.toHaveBeenCalled();
  });

  it("starts exits when views disappear or change", () => {
    harness.stateValues = [new Set(), views.warning, null, null, null, undefined];
    harness.view = null;
    SidebarProviderUpdatePill();
    expect(harness.setters[3]).toHaveBeenCalledWith("warning");

    harness.stateIndex = 0;
    harness.setters.length = 0;
    harness.effects.length = 0;
    harness.stateValues = [new Set(), views.warning, null, null, null, undefined];
    harness.view = views.error;
    SidebarProviderUpdatePill();
    expect(harness.setters[2]).toHaveBeenCalledWith(views.error);
    expect(harness.setters[3]).toHaveBeenCalledWith("warning");
  });

  it("schedules automatic dismissal and clears its timer", () => {
    vi.useFakeTimers();
    vi.stubGlobal("window", {
      setTimeout: globalThis.setTimeout,
      clearTimeout: globalThis.clearTimeout,
    });
    harness.view = views.success;
    harness.stateValues = [new Set(), views.success, null, null, null, undefined];
    renderPill();
    const cleanup = harness.effects[0]?.();
    vi.advanceTimersByTime(500);
    expect(harness.setters[3]).toHaveBeenCalledWith("success");
    expect(harness.setters[4]).toHaveBeenCalledWith("success");
    if (typeof cleanup === "function") cleanup();
  });

  it("finishes transitions, dismisses keys, and ignores bubbled or stale transitions", () => {
    harness.view = views.error;
    harness.stateValues = [new Set(), views.warning, views.error, "warning", "warning", undefined];
    const { tree } = renderPill();
    const root = visit(tree).find(
      (element) => element.type === "div" && "onTransitionEnd" in (element.props as object),
    );
    if (!root) throw new Error("Transition root not found");
    const transition = (root.props as { onTransitionEnd: (event: unknown) => void })
      .onTransitionEnd;
    const currentTarget = {};
    transition({ target: {}, currentTarget });
    expect(harness.setters[1]).not.toHaveBeenCalled();
    transition({ target: currentTarget, currentTarget });
    const updater = harness.setters[0]!.mock.calls[0]?.[0] as
      | ((value: ReadonlySet<string>) => ReadonlySet<string>)
      | undefined;
    expect(updater?.(new Set()).has("warning")).toBe(true);
    expect(harness.setters[1]).toHaveBeenCalledWith(views.error);
    expect(harness.setters[2]).toHaveBeenCalledWith(null);
    expect(harness.setters[3]).toHaveBeenCalledWith(null);
    expect(harness.setters[4]).toHaveBeenCalledWith(null);
  });
});
