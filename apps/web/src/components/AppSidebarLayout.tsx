import { useAtomValue } from "@effect/atom-react";
import { useEffect, useState, type CSSProperties, type ReactNode } from "react";
import * as TanStackRouter from "@tanstack/react-router";

import { resolveShortcutCommand, shortcutLabelForCommand } from "../keybindings";
import { primaryServerKeybindingsAtom } from "../state/server";
import { useUiStateStore } from "../uiStateStore";
import ThreadSidebar from "./Sidebar";
import { EnvironmentRail } from "./sidebar/EnvironmentRail";
import { ENVIRONMENT_RAIL_WIDTH_PX } from "./sidebar/environmentRail.logic";
import {
  readStoredSidebarWidth,
  Sidebar,
  SidebarProvider,
  SidebarRail,
  SidebarTrigger,
  useSidebar,
  type SidebarResizableOptions,
} from "./ui/sidebar";
import { Tooltip, TooltipPopup, TooltipTrigger } from "./ui/tooltip";

// v2: widths stored under the retired key belong to the 256px default era.
const THREAD_SIDEBAR_WIDTH_STORAGE_KEY = "coddy-studio:sidebar-width:v2";
const THREAD_SIDEBAR_MIN_WIDTH = 13 * 16 + ENVIRONMENT_RAIL_WIDTH_PX;
// Measured from the user's screenshot: a 370px projects panel beside the 52px
// rail (422px total). A width the user has dragged to is stored under the
// storage key and wins over this default.
const THREAD_SIDEBAR_DEFAULT_WIDTH = 370 + ENVIRONMENT_RAIL_WIDTH_PX;
const THREAD_MAIN_CONTENT_MIN_WIDTH = 40 * 16;
// No configured ceiling: matches how `ui/sidebar.tsx` resolves an omitted
// `maxWidth`. Shared between the resizable options below and
// `readStoredSidebarWidth`, so both clamp a width the same way.
const THREAD_SIDEBAR_WIDTH_BOUNDS = {
  minWidth: THREAD_SIDEBAR_MIN_WIDTH,
  maxWidth: Number.POSITIVE_INFINITY,
};

// The largest sidebar width that leaves THREAD_MAIN_CONTENT_MIN_WIDTH for the
// main area, given the total width available (a window or the sidebar
// wrapper). The one place that rule is expressed; `shouldAcceptWidth` below
// and `fitDefaultSidebarWidth` both build on it instead of restating it.
function maxSidebarWidthFor(availableWidth: number): number {
  return availableWidth - THREAD_MAIN_CONTENT_MIN_WIDTH;
}

// Fits THREAD_SIDEBAR_DEFAULT_WIDTH into what maxSidebarWidthFor allows,
// never below THREAD_SIDEBAR_MIN_WIDTH. Shared by the first-launch default
// and the double-click "reset to default" target below, so neither can open
// a sidebar that leaves the main area squeezed.
function fitDefaultSidebarWidth(availableWidth: number): number {
  return Math.max(
    THREAD_SIDEBAR_MIN_WIDTH,
    Math.min(THREAD_SIDEBAR_DEFAULT_WIDTH, maxSidebarWidthFor(availableWidth)),
  );
}

// A stable, module-level reference: passed as-is to `Sidebar`'s `resizable`
// prop so its internal `useMemo([resizable])` — and the storage-restore
// effect that depends on the memoized result — only recompute when
// collapsible/isMobile actually change, not on every AppSidebarLayout
// render (e.g. every navigation). None of these fields close over anything
// per-render, so there is nothing to lose by hoisting them.
const THREAD_SIDEBAR_RESIZABLE_OPTIONS: SidebarResizableOptions = {
  ...THREAD_SIDEBAR_WIDTH_BOUNDS,
  // Fit the reset target to the window at reset time too, so double-clicking
  // on a narrow window can't reopen the main-area squeeze the first-launch
  // clamp already avoids.
  defaultWidth: ({ wrapper }) => fitDefaultSidebarWidth(wrapper.clientWidth),
  shouldAcceptWidth: ({ nextWidth, wrapper }) =>
    nextWidth <= maxSidebarWidthFor(wrapper.clientWidth),
  storageKey: THREAD_SIDEBAR_WIDTH_STORAGE_KEY,
};

// First launch only: computed once, synchronously, so the sidebar never
// visibly resizes after its first paint. If a width is already saved, seed
// it here too (clamped exactly as the sidebar primitive itself clamps it,
// via the same `readStoredSidebarWidth`), so there is no flash of the
// default before the primitive's own restore effect corrects it — that
// effect still runs, but writing the same value back is harmless.
function resolveInitialSidebarWidth(): number {
  if (typeof window === "undefined") {
    return THREAD_SIDEBAR_DEFAULT_WIDTH;
  }
  const savedWidth = readStoredSidebarWidth(
    THREAD_SIDEBAR_WIDTH_STORAGE_KEY,
    THREAD_SIDEBAR_WIDTH_BOUNDS,
  );
  return savedWidth ?? fitDefaultSidebarWidth(window.innerWidth);
}

const useAppPathname =
  "useLocation" in TanStackRouter
    ? () => TanStackRouter.useLocation({ select: (location) => location.pathname })
    : () => "/";

function SidebarControl() {
  const keybindings = useAtomValue(primaryServerKeybindingsAtom);
  const { toggleSidebar } = useSidebar();
  const shortcutLabel = shortcutLabelForCommand(keybindings, "sidebar.toggle");

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.defaultPrevented) return;
      if (resolveShortcutCommand(event, keybindings) !== "sidebar.toggle") return;

      event.preventDefault();
      event.stopPropagation();
      toggleSidebar();
    };

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [keybindings, toggleSidebar]);

  return (
    <div
      className="pointer-events-none fixed left-[var(--workspace-controls-left)] top-[var(--workspace-controls-top)] z-50 flex h-[var(--workspace-topbar-height)] items-center"
      data-sidebar-control=""
    >
      <Tooltip>
        <TooltipTrigger
          render={
            <SidebarTrigger className="pointer-events-auto" aria-label="Toggle main sidebar" />
          }
        />
        <TooltipPopup side="bottom">
          Toggle main sidebar{shortcutLabel ? ` (${shortcutLabel})` : ""}
        </TooltipPopup>
      </Tooltip>
    </div>
  );
}

export function AppSidebarLayout({ children }: { children: ReactNode }) {
  const navigate = TanStackRouter.useNavigate();
  const pathname = useAppPathname();
  const [initialSidebarWidth] = useState(resolveInitialSidebarWidth);
  const sidebarView = useUiStateStore((state) => state.sidebarView);
  useEffect(() => {
    const onMenuAction = window.desktopBridge?.onMenuAction;
    if (typeof onMenuAction !== "function") {
      return;
    }

    const unsubscribe = onMenuAction((action) => {
      if (action === "check-for-updates") {
        void window.desktopBridge?.checkForUpdate().catch(() => undefined);
        return;
      }

      if (action === "open-settings") {
        void navigate({ to: "/settings" });
      }
    });

    return () => {
      unsubscribe?.();
    };
  }, [navigate]);

  if (pathname === "/agents") {
    return (
      <SidebarProvider className="h-dvh! min-h-0! border-t border-panel-separator" defaultOpen>
        {children}
      </SidebarProvider>
    );
  }

  return (
    <SidebarProvider
      className="h-dvh! min-h-0! border-t border-panel-separator"
      defaultOpen
      style={{ "--sidebar-width": `${initialSidebarWidth}px` } as CSSProperties}
    >
      <Sidebar
        side="left"
        collapsible="offcanvas"
        className="border-t border-r border-panel-separator bg-card text-foreground"
        resizable={THREAD_SIDEBAR_RESIZABLE_OPTIONS}
      >
        <div className="flex h-full min-h-0 flex-row">
          {/* Each environment card names its environment, so the Repositories view drops the rail. */}
          {sidebarView === "repositories" ? null : <EnvironmentRail />}
          <div className="flex h-full min-h-0 min-w-0 flex-1 flex-col">
            <ThreadSidebar />
          </div>
        </div>
        <SidebarRail />
      </Sidebar>
      {children}
      <SidebarControl />
    </SidebarProvider>
  );
}
