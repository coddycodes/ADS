// @vitest-environment happy-dom

import type { ReactNode } from "react";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import { startBrowserSurfaceSync } from "~/browser/browserSurfaceSync";
import { acquireBrowserSurface, useBrowserSurfaceStore } from "~/browser/browserSurfaceStore";

const h = vi.hoisted(() => ({
  desktopHost: true,
  onOpenChange: undefined as ((open: boolean) => void) | undefined,
}));

vi.mock("~/env", () => ({
  get isDesktopHost() {
    return h.desktopHost;
  },
}));

vi.mock("../ui/popover", () => ({
  Popover: ({
    children,
    onOpenChange,
  }: {
    children?: ReactNode;
    onOpenChange?: (open: boolean) => void;
  }) => {
    h.onOpenChange = onOpenChange;
    return <>{children}</>;
  },
  PopoverTrigger: ({ children, ...props }: { children?: ReactNode }) => (
    <button {...props}>{children}</button>
  ),
  PopoverContent: ({ children }: { children?: ReactNode }) => <section>{children}</section>,
}));

import { ResourceUsageSegment } from "./ResourceUsageSegment";

const mounted: Array<{ readonly root: Root; readonly container: HTMLDivElement }> = [];
const diagnostics = { diagnostics: null, queryError: null } as const;

async function mountResourceUsage(): Promise<{
  readonly root: Root;
  readonly container: HTMLDivElement;
}> {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  const entry = { root, container };
  mounted.push(entry);
  await act(async () => {
    root.render(
      <ResourceUsageSegment
        diagnostics={diagnostics}
        localDiagnostics={null}
        terminalCount={0}
        iconOnly={false}
      />,
    );
  });
  return entry;
}

async function flushPromises(): Promise<void> {
  await act(async () => {
    for (let turn = 0; turn < 12; turn += 1) await Promise.resolve();
  });
}

async function setPopoverOpen(open: boolean): Promise<void> {
  expect(h.onOpenChange).toEqual(expect.any(Function));
  await act(async () => h.onOpenChange?.(open));
  await flushPromises();
}

async function unmount(entry: { readonly root: Root; readonly container: HTMLDivElement }) {
  await act(async () => entry.root.unmount());
  entry.container.remove();
  mounted.splice(mounted.indexOf(entry), 1);
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  h.desktopHost = true;
  h.onOpenChange = undefined;
  vi.stubGlobal("navigator", { platform: "MacIntel" });
  useBrowserSurfaceStore.setState({ byTabId: {}, occlusionOwners: new Set() } as never);
});

afterEach(async () => {
  for (const entry of mounted.splice(0)) {
    await act(async () => entry.root.unmount());
    entry.container.remove();
  }
  document.body.replaceChildren();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("ResourceUsageSegment native preview occlusion", () => {
  it("hides a macOS native preview while Resource Manager is open and restores it on close", async () => {
    const setBounds = vi.fn().mockResolvedValue(undefined);
    const stopSync = startBrowserSurfaceSync({ setBounds });
    const surface = acquireBrowserSurface("resource-manager-preview");
    const rect = { x: 10, y: 20, width: 300, height: 400 };
    surface.present(rect, true);
    await mountResourceUsage();

    await setPopoverOpen(true);
    await setPopoverOpen(false);

    expect(setBounds.mock.calls).toEqual([
      ["resource-manager-preview", rect, true],
      ["resource-manager-preview", rect, false],
      ["resource-manager-preview", rect, true],
    ]);
    stopSync();
  });

  it("releases its occlusion lease when Resource Manager unmounts", async () => {
    const setBounds = vi.fn().mockResolvedValue(undefined);
    const stopSync = startBrowserSurfaceSync({ setBounds });
    const surface = acquireBrowserSurface("resource-manager-unmount");
    const rect = { x: 1, y: 2, width: 30, height: 40 };
    surface.present(rect, true);
    const resourceUsage = await mountResourceUsage();

    await setPopoverOpen(true);
    await unmount(resourceUsage);
    await flushPromises();

    expect(setBounds.mock.calls).toEqual([
      ["resource-manager-unmount", rect, true],
      ["resource-manager-unmount", rect, false],
      ["resource-manager-unmount", rect, true],
    ]);
    stopSync();
  });

  it.each([
    { label: "Windows desktop", desktopHost: true, platform: "Win32" },
    { label: "macOS web", desktopHost: false, platform: "MacIntel" },
  ])("leaves native preview visibility unchanged on $label", async ({ desktopHost, platform }) => {
    const setBounds = vi.fn().mockResolvedValue(undefined);
    const stopSync = startBrowserSurfaceSync({ setBounds });
    const surface = acquireBrowserSurface("resource-manager-non-macos-desktop");
    const rect = { x: 5, y: 6, width: 70, height: 80 };
    surface.present(rect, true);
    h.desktopHost = desktopHost;
    vi.stubGlobal("navigator", { platform });
    await mountResourceUsage();

    await setPopoverOpen(true);
    await setPopoverOpen(false);

    expect(setBounds.mock.calls).toEqual([["resource-manager-non-macos-desktop", rect, true]]);
    stopSync();
  });
});

describe("ResourceUsageSegment memory metric", () => {
  it("says what the memory figure measures on each server OS (#159)", async () => {
    const entry = await mountResourceUsage();
    const text = entry.container.textContent ?? "";
    expect(text).toContain("physical footprint on macOS, as in Activity Monitor");
    expect(text).toContain("resident memory on Linux and Windows");
  });
});
