// @vitest-environment happy-dom

import { act, type ReactNode } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";

const navigate = vi.fn();
let menuListener: ((action: string) => void) | undefined;
const initialInnerWidth = window.innerWidth;
// Backs the mocked `readStoredSidebarWidth` below: swap it per test to return
// a saved (already-clamped) width, or null for a first launch. The real
// export already handles "nothing saved" and "storage blocked" the same way
// (see its own tests in ui/sidebar.rail.test.tsx), so AppSidebarLayout only
// ever needs to handle its two-value contract: a number, or null.
let readStoredSidebarWidthMock: (
  storageKey: string,
  options: { minWidth: number; maxWidth: number },
) => number | null = () => null;

vi.mock("@effect/atom-react", () => ({
  useAtomValue: () => [],
}));

vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => navigate,
}));

vi.mock("./Sidebar", () => ({
  default: () => <div data-testid="thread-sidebar-mock" />,
}));

vi.mock("./sidebar/EnvironmentRail", () => ({
  EnvironmentRail: () => <div data-testid="environment-rail-mock" />,
}));

const sidebarProviderProps: Array<Record<string, unknown>> = [];

const sidebarProps: Array<Record<string, unknown>> = [];

vi.mock("./ui/sidebar", () => ({
  Sidebar: ({ children, ...props }: { children?: ReactNode } & Record<string, unknown>) => {
    sidebarProps.push(props);
    return <>{children}</>;
  },
  SidebarProvider: ({ children, ...props }: { children?: ReactNode } & Record<string, unknown>) => {
    sidebarProviderProps.push(props);
    return <>{children}</>;
  },
  SidebarRail: () => null,
  SidebarTrigger: () => null,
  useSidebar: () => ({ toggleSidebar: vi.fn() }),
  readStoredSidebarWidth: (storageKey: string, options: { minWidth: number; maxWidth: number }) =>
    readStoredSidebarWidthMock(storageKey, options),
}));

vi.mock("./ui/tooltip", () => ({
  Tooltip: ({ children }: { children?: ReactNode }) => <>{children}</>,
  TooltipPopup: () => null,
  TooltipTrigger: ({ render }: { render?: ReactNode }) => <>{render}</>,
}));

import { useUiStateStore } from "../uiStateStore";
import { AppSidebarLayout } from "./AppSidebarLayout";
import { toastManager } from "./ui/toast";

const mounted: Array<{ root: ReturnType<typeof createRoot>; container: HTMLDivElement }> = [];

// `resizable.defaultWidth` is a function evaluated at double-click-reset time
// against the sidebar wrapper (see ui/sidebar.tsx); resolve it the same way
// against a fake wrapper of the given width so tests can assert its result.
function resolveDefaultWidth(
  resizable: Record<string, unknown> | undefined,
  wrapperClientWidth: number,
): unknown {
  const defaultWidth = resizable?.["defaultWidth"];
  return typeof defaultWidth === "function"
    ? (defaultWidth as (context: { wrapper: { clientWidth: number } }) => number)({
        wrapper: { clientWidth: wrapperClientWidth },
      })
    : defaultWidth;
}

afterEach(async () => {
  await act(async () => {
    for (const entry of mounted.splice(0)) entry.root.unmount();
  });
  navigate.mockReset();
  menuListener = undefined;
  delete (window as { desktopBridge?: unknown }).desktopBridge;
  Object.defineProperty(window, "innerWidth", { configurable: true, value: initialInnerWidth });
  readStoredSidebarWidthMock = () => null;
  useUiStateStore.setState({ sidebarView: "environments" });
});

describe("AppSidebarLayout", () => {
  it("opens the left sidebar at 422px on a wide window with no saved width", async () => {
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 1400 });
    sidebarProviderProps.length = 0;
    sidebarProps.length = 0;
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);
    await act(async () => {
      root.render(<AppSidebarLayout>Workspace</AppSidebarLayout>);
    });
    const style = sidebarProviderProps[0]?.["style"] as Record<string, string> | undefined;
    expect(style?.["--sidebar-width"]).toBe("422px");
    const resizable = sidebarProps[0]?.["resizable"] as Record<string, unknown> | undefined;
    expect(resolveDefaultWidth(resizable, 1400)).toBe(422);
    await act(async () => {
      root.unmount();
    });
    container.remove();
  });

  it("shrinks the first-launch width to keep the main area's 640px minimum on a 1000px window", async () => {
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 1000 });
    sidebarProviderProps.length = 0;
    sidebarProps.length = 0;
    const container = document.createElement("div");
    const root = createRoot(container);
    mounted.push({ root, container });

    await act(async () => {
      root.render(<AppSidebarLayout>Workspace</AppSidebarLayout>);
    });

    const style = sidebarProviderProps[0]?.["style"] as Record<string, string> | undefined;
    expect(style?.["--sidebar-width"]).toBe("360px");
    // The double-click "reset to default" target fits the same way, evaluated
    // against the wrapper's width at reset time rather than baked in here.
    const resizable = sidebarProps[0]?.["resizable"] as Record<string, unknown> | undefined;
    expect(resolveDefaultWidth(resizable, 1000)).toBe(360);
  });

  it("never shrinks the first-launch width below the 260px sidebar minimum", async () => {
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 700 });
    sidebarProviderProps.length = 0;
    sidebarProps.length = 0;
    const container = document.createElement("div");
    const root = createRoot(container);
    mounted.push({ root, container });

    await act(async () => {
      root.render(<AppSidebarLayout>Workspace</AppSidebarLayout>);
    });

    const style = sidebarProviderProps[0]?.["style"] as Record<string, string> | undefined;
    expect(style?.["--sidebar-width"]).toBe("260px");
    // The reset target hits the same 260px floor on an equally narrow wrapper.
    const resizable = sidebarProps[0]?.["resizable"] as Record<string, unknown> | undefined;
    expect(resolveDefaultWidth(resizable, 700)).toBe(260);
  });

  it("seeds the initial width from a saved value, with no flash of the default", async () => {
    // A narrow window that would otherwise clamp an unsaved default to 360px;
    // the saved value wins outright, painted directly on first render so
    // there is no flash of the default before the primitive's restore effect
    // (which still runs, harmlessly writing the same value back) corrects it.
    Object.defineProperty(window, "innerWidth", { configurable: true, value: 1000 });
    const readStoredSidebarWidthSpy = vi.fn(() => 500);
    readStoredSidebarWidthMock = readStoredSidebarWidthSpy;
    sidebarProviderProps.length = 0;
    sidebarProps.length = 0;
    const container = document.createElement("div");
    const root = createRoot(container);
    mounted.push({ root, container });

    await act(async () => {
      root.render(<AppSidebarLayout>Workspace</AppSidebarLayout>);
    });

    const style = sidebarProviderProps[0]?.["style"] as Record<string, string> | undefined;
    expect(style?.["--sidebar-width"]).toBe("500px");
    // Reads and clamps through the same bounds passed to the resizable
    // sidebar itself (`minWidth`, and no configured ceiling).
    expect(readStoredSidebarWidthSpy).toHaveBeenCalledWith("coddy-studio:sidebar-width:v2", {
      minWidth: 260,
      maxWidth: Number.POSITIVE_INFINITY,
    });
  });

  it("passes one shared resizable options object to every mount", async () => {
    // A fresh object per render would defeat Sidebar's `useMemo([resizable])`,
    // re-running the storage-restore effect (and re-reading localStorage). The
    // React Compiler may skip re-rendering an unchanged Sidebar, so count on
    // mounts instead: its cache is per component instance, so only a
    // module-level object reaches two separate mounts as the same reference.
    sidebarProps.length = 0;
    for (const label of ["First", "Second"]) {
      const container = document.createElement("div");
      const root = createRoot(container);
      mounted.push({ root, container });
      await act(async () => {
        root.render(<AppSidebarLayout>{label}</AppSidebarLayout>);
      });
    }

    expect(sidebarProps.length).toBeGreaterThanOrEqual(2);
    const first = sidebarProps[0]?.["resizable"];
    expect(first).not.toBeUndefined();
    for (const props of sidebarProps) {
      expect(props["resizable"]).toBe(first);
    }
  });

  it("mounts the environment rail before the panel content inside the left sidebar", async () => {
    const container = document.createElement("div");
    const root = createRoot(container);
    mounted.push({ root, container });

    await act(async () => {
      root.render(<AppSidebarLayout>Workspace</AppSidebarLayout>);
    });

    const rail = container.querySelector('[data-testid="environment-rail-mock"]');
    const panel = container.querySelector('[data-testid="thread-sidebar-mock"]');
    expect(rail).not.toBeNull();
    expect(panel).not.toBeNull();
    if (rail === null || panel === null) throw new Error("sidebar layout markers missing");
    expect(rail.compareDocumentPosition(panel) & Node.DOCUMENT_POSITION_FOLLOWING).not.toBe(0);
  });

  it("does not render the environment rail in the Repositories view", async () => {
    const container = document.createElement("div");
    const root = createRoot(container);
    mounted.push({ root, container });
    const rail = () => container.querySelector('[data-testid="environment-rail-mock"]');

    useUiStateStore.setState({ sidebarView: "repositories" });
    await act(async () => {
      root.render(<AppSidebarLayout>Workspace</AppSidebarLayout>);
    });
    expect(rail()).toBeNull();
    expect(container.querySelector('[data-testid="thread-sidebar-mock"]')).not.toBeNull();

    await act(async () => {
      useUiStateStore.setState({ sidebarView: "environments" });
    });
    expect(rail()).not.toBeNull();
  });

  it("routes supported desktop menu actions independently", async () => {
    const checkForUpdate = vi.fn(() => Promise.resolve({ checked: true }));
    Object.assign(window, {
      desktopBridge: {
        checkForUpdate,
        onMenuAction: (listener: (action: string) => void) => {
          menuListener = listener;
          return vi.fn();
        },
      },
    });
    const container = document.createElement("div");
    const root = createRoot(container);
    mounted.push({ root, container });

    await act(async () => {
      root.render(<AppSidebarLayout>Workspace</AppSidebarLayout>);
    });

    menuListener?.("check-for-updates");
    expect(checkForUpdate).toHaveBeenCalledTimes(1);
    expect(navigate).not.toHaveBeenCalled();

    menuListener?.("open-settings");
    expect(navigate).toHaveBeenCalledWith({ to: "/settings" });
  });

  it("suppresses rejected desktop update checks", async () => {
    let rejectedUpdate: Promise<never> | undefined;
    const handleRejection = vi.fn();
    const addToast = vi.spyOn(toastManager, "add");
    const checkForUpdate = vi.fn(() => {
      rejectedUpdate = Promise.reject(new Error("IPC unavailable"));
      const originalCatch = rejectedUpdate.catch.bind(rejectedUpdate);
      vi.spyOn(rejectedUpdate, "catch").mockImplementation((onRejected) => {
        handleRejection();
        return originalCatch(onRejected);
      });
      return rejectedUpdate;
    });
    Object.assign(window, {
      desktopBridge: {
        checkForUpdate,
        onMenuAction: (listener: (action: string) => void) => {
          menuListener = listener;
          return vi.fn();
        },
      },
    });
    const container = document.createElement("div");
    const root = createRoot(container);
    mounted.push({ root, container });

    try {
      await act(async () => {
        root.render(<AppSidebarLayout>Workspace</AppSidebarLayout>);
      });

      menuListener?.("check-for-updates");
      await Promise.resolve();

      expect(checkForUpdate).toHaveBeenCalledTimes(1);
      expect(handleRejection).toHaveBeenCalledTimes(1);
      expect(navigate).not.toHaveBeenCalled();
      expect(addToast).not.toHaveBeenCalled();
    } finally {
      addToast.mockRestore();
    }
  });
});
