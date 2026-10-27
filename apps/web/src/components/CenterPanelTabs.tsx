import type { ContextMenuItem } from "@coddy/contracts";
import { getTerminalLabel } from "@coddy/shared/terminalLabels";
import { useSortable } from "@dnd-kit/sortable";
import {
  Bot,
  ChevronLeftIcon,
  ChevronRightIcon,
  MessageSquare,
  Rows3Icon,
  TerminalSquare,
  X,
} from "lucide-react";
import {
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent,
  type WheelEvent as ReactWheelEvent,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";

import type { CenterPanelSplitDirection } from "~/centerPanelLayout";
import type { CenterPanelKind, CenterSurface } from "~/centerPanelStore";
import { cn } from "~/lib/utils";
import { readLocalApi } from "~/localApi";
import { Tooltip, TooltipPopup, TooltipTrigger } from "~/components/ui/tooltip";
import { ScrollArea } from "~/components/ui/scroll-area";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "~/components/ui/menu";
import { CenterHeaderIconButton } from "./CenterHeaderIconButton";
import type { CenterPanelTabDragData } from "./centerPanelDnd";

interface CenterPanelTabsProps {
  groupId: string;
  hostLabel: string;
  surfaces: readonly CenterSurface[];
  activeSurfaceId: string | null;
  terminalLabelsById?: ReadonlyMap<string, string>;
  canMoveToSplit: (direction: CenterPanelSplitDirection) => boolean;
  dragInProgress: boolean;
  onActivate: (groupId: string, surface: CenterSurface) => void;
  onCloseSurface: (groupId: string, surface: CenterSurface) => void;
  onCloseOtherSurfaces: (groupId: string, surface: CenterSurface) => void;
  onCloseSurfacesToRight: (groupId: string, surface: CenterSurface) => void;
  onCloseAllSurfaces: (groupId: string) => void;
  onMoveToSplit: (
    groupId: string,
    surface: CenterSurface,
    direction: CenterPanelSplitDirection,
  ) => void;
}

type TabContextMenuAction =
  | "move-to-split"
  | `move-to-split:${CenterPanelSplitDirection}`
  | "close"
  | "close-others"
  | "close-to-right"
  | "close-all";

const SPLIT_DIRECTIONS = ["left", "right", "up", "down"] as const;

function centerSurfaceTitle(
  surface: CenterSurface,
  hostLabel: string,
  terminalLabelsById: ReadonlyMap<string, string> | undefined,
): string {
  switch (surface.kind) {
    case "chat-host":
      return hostLabel;
    case "chat":
      return surface.providerLabel ?? "Chat";
    case "terminal":
      return (
        surface.label ??
        terminalLabelsById?.get(surface.terminalId) ??
        getTerminalLabel(surface.terminalId)
      );
  }
}

export function CenterSurfaceIcon({ kind }: { kind: CenterPanelKind }) {
  switch (kind) {
    case "chat-host":
      return <MessageSquare className="size-3.5 shrink-0" />;
    case "chat":
      return <Bot className="size-3.5 shrink-0" />;
    case "terminal":
      return <TerminalSquare className="size-3.5 shrink-0" />;
  }
}

interface SortableCenterTabProps {
  readonly groupId: string;
  readonly surface: CenterSurface;
  readonly title: string;
  readonly active: boolean;
  readonly surfaceIndex: number;
  readonly dragInProgress: boolean;
  readonly onActivate: (surface: CenterSurface) => void;
  readonly onClose: (surface: CenterSurface) => void;
  readonly onMouseDown: (event: ReactMouseEvent) => void;
  readonly onAuxClick: (event: ReactMouseEvent, surface: CenterSurface) => void;
  readonly onContextMenu: (event: ReactMouseEvent, surface: CenterSurface) => void;
  readonly onKeyDown: (event: ReactKeyboardEvent<HTMLButtonElement>, surfaceIndex: number) => void;
}

function SortableCenterTab({
  groupId,
  surface,
  title,
  active,
  surfaceIndex,
  dragInProgress,
  onActivate,
  onClose,
  onMouseDown,
  onAuxClick,
  onContextMenu,
  onKeyDown,
}: SortableCenterTabProps) {
  const { attributes, isDragging, listeners, setActivatorNodeRef, setNodeRef } = useSortable({
    id: surface.id,
    data: {
      type: "center-panel-tab",
      surfaceId: surface.id,
      groupId,
      surfaceKind: surface.kind,
      title,
    } satisfies CenterPanelTabDragData,
  });

  return (
    <div
      ref={setNodeRef}
      data-center-panel-tab-id={surface.id}
      data-center-panel-group-id={groupId}
      data-active-tab={active}
      data-dragging={isDragging}
      onMouseDown={onMouseDown}
      onAuxClick={(event) => onAuxClick(event, surface)}
      onContextMenu={(event) => onContextMenu(event, surface)}
      className={cn(
        "group flex h-7 min-w-25 max-w-44 shrink-0 items-center gap-1.5 rounded-md px-2 text-sm",
        active
          ? "bg-accent text-foreground"
          : "text-muted-foreground hover:bg-accent/60 hover:text-foreground",
        isDragging && "opacity-40",
      )}
    >
      <Tooltip>
        <TooltipTrigger
          render={
            <button
              ref={setActivatorNodeRef}
              {...attributes}
              {...listeners}
              type="button"
              role="tab"
              aria-selected={active}
              data-center-panel-tab-activation
              className="flex min-w-0 flex-1 items-center gap-1.5 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring/70"
              onClick={() => !dragInProgress && onActivate(surface)}
              onKeyDown={(event) => onKeyDown(event, surfaceIndex)}
            >
              <CenterSurfaceIcon kind={surface.kind} />
              <span className="truncate">{title}</span>
            </button>
          }
        />
        <TooltipPopup>{title}</TooltipPopup>
      </Tooltip>
      <button
        type="button"
        className="flex size-4 shrink-0 items-center justify-center rounded opacity-0 hover:bg-muted focus:opacity-100 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring/70 group-hover:opacity-100"
        aria-label={`Close ${title}`}
        onPointerDown={(event) => event.stopPropagation()}
        onClick={() => onClose(surface)}
      >
        <X className="size-3" />
      </button>
    </div>
  );
}

export function CenterPanelTabs(props: CenterPanelTabsProps) {
  const tabListRef = useRef<HTMLDivElement>(null);
  const [hasOverflow, setHasOverflow] = useState(false);

  const getTabViewport = useCallback(
    () =>
      tabListRef.current?.querySelector<HTMLElement>('[data-slot="scroll-area-viewport"]') ?? null,
    [],
  );

  const handleWheel = useCallback(
    (event: ReactWheelEvent<HTMLDivElement>) => {
      const viewport = getTabViewport();
      if (!viewport || viewport.scrollWidth <= viewport.clientWidth) return;
      if (event.deltaY === 0 || Math.abs(event.deltaX) >= Math.abs(event.deltaY)) return;

      viewport.scrollLeft += event.deltaY;
      event.preventDefault();
    },
    [getTabViewport],
  );

  const scrollTabPage = useCallback(
    (direction: -1 | 1) => {
      const viewport = getTabViewport();
      if (!viewport) return;
      viewport.scrollBy({
        left: direction * Math.round(viewport.clientWidth * 0.9),
        behavior: "smooth",
      });
    },
    [getTabViewport],
  );

  const activateFromAllTabs = useCallback(
    (surface: CenterSurface, surfaceIndex: number) => {
      props.onActivate(props.groupId, surface);
      requestAnimationFrame(() => {
        const activationButtons = tabListRef.current?.querySelectorAll<HTMLButtonElement>(
          "[data-center-panel-tab-activation]",
        );
        activationButtons?.[surfaceIndex]?.scrollIntoView({ block: "nearest", inline: "nearest" });
      });
    },
    [props],
  );

  const handleTabKeyDown = useCallback(
    (event: ReactKeyboardEvent<HTMLButtonElement>, surfaceIndex: number) => {
      if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
      const direction = event.key === "ArrowRight" ? 1 : -1;
      const nextIndex = surfaceIndex + direction;
      const nextSurface = props.surfaces[nextIndex];
      if (!nextSurface) return;

      event.preventDefault();
      props.onActivate(props.groupId, nextSurface);
      requestAnimationFrame(() => {
        requestAnimationFrame(() => {
          const activationButtons = tabListRef.current?.querySelectorAll<HTMLButtonElement>(
            "[data-center-panel-tab-activation]",
          );
          const nextButton = activationButtons?.[nextIndex];
          nextButton?.focus();
          nextButton?.scrollIntoView({ block: "nearest", inline: "nearest" });
        });
      });
    },
    [props],
  );

  const handleTabContextMenu = useCallback(
    async (event: ReactMouseEvent, surface: CenterSurface) => {
      event.preventDefault();
      event.stopPropagation();

      const api = readLocalApi();
      if (!api) return;

      const surfaceIndex = props.surfaces.findIndex((entry) => entry.id === surface.id);
      if (surfaceIndex < 0) return;

      const items: ContextMenuItem<TabContextMenuAction>[] = [
        {
          id: "move-to-split",
          label: "Move Tab to Split",
          disabled: !SPLIT_DIRECTIONS.some(props.canMoveToSplit),
          children: SPLIT_DIRECTIONS.map((direction) => ({
            id: `move-to-split:${direction}`,
            label: direction[0]!.toUpperCase() + direction.slice(1),
            disabled: !props.canMoveToSplit(direction),
          })),
        },
        { id: "close", label: "Close" },
        { id: "close-others", label: "Close others", disabled: props.surfaces.length <= 1 },
        {
          id: "close-to-right",
          label: "Close to the right",
          disabled: surfaceIndex >= props.surfaces.length - 1,
        },
        { id: "close-all", label: "Close all", disabled: props.surfaces.length === 0 },
      ];

      const action = await api.contextMenu.show(items, { x: event.clientX, y: event.clientY });
      switch (action) {
        case "move-to-split:left":
        case "move-to-split:right":
        case "move-to-split:up":
        case "move-to-split:down":
          props.onMoveToSplit(
            props.groupId,
            surface,
            action.slice("move-to-split:".length) as CenterPanelSplitDirection,
          );
          break;
        case "close":
          props.onCloseSurface(props.groupId, surface);
          break;
        case "close-others":
          props.onCloseOtherSurfaces(props.groupId, surface);
          break;
        case "close-to-right":
          props.onCloseSurfacesToRight(props.groupId, surface);
          break;
        case "close-all":
          props.onCloseAllSurfaces(props.groupId);
          break;
        case "move-to-split":
        case null:
          break;
      }
    },
    [props],
  );

  const handleTabMouseDown = useCallback((event: ReactMouseEvent) => {
    if (event.button !== 1) return;
    event.preventDefault();
  }, []);

  const handleTabAuxClick = useCallback(
    (event: ReactMouseEvent, surface: CenterSurface) => {
      if (event.button !== 1) return;
      event.preventDefault();
      event.stopPropagation();
      props.onCloseSurface(props.groupId, surface);
    },
    [props],
  );

  const activeSurfaceId = props.activeSurfaceId;
  useEffect(() => {
    if (activeSurfaceId === null) return;
    const activeTab = tabListRef.current?.querySelector<HTMLElement>("[data-active-tab='true']");
    activeTab?.scrollIntoView({ block: "nearest", inline: "nearest" });
  }, [activeSurfaceId]);

  const surfaceCount = props.surfaces.length;
  useEffect(() => {
    // An empty tab bar renders without the scroll area, so there is nothing to observe.
    if (surfaceCount === 0) return;
    const boundary = tabListRef.current;
    const viewport = getTabViewport();
    if (!boundary || !viewport) return;
    const content = boundary.querySelector<HTMLElement>('[role="tablist"]');

    const syncOverflow = () => {
      setHasOverflow(viewport.scrollWidth > boundary.clientWidth + 1);
    };
    syncOverflow();
    viewport.addEventListener?.("scroll", syncOverflow, { passive: true });
    const resizeObserver =
      typeof ResizeObserver === "undefined" ? null : new ResizeObserver(syncOverflow);
    resizeObserver?.observe(boundary);
    resizeObserver?.observe(viewport);
    if (content) resizeObserver?.observe(content);
    return () => {
      viewport.removeEventListener?.("scroll", syncOverflow);
      resizeObserver?.disconnect();
    };
  }, [getTabViewport, surfaceCount]);

  if (props.surfaces.length === 0) {
    return (
      <div
        className="flex min-w-0 flex-1 items-center gap-2 self-stretch px-2"
        data-center-panel-tabbar
      >
        <div role="tablist" aria-label="Workspace panels" />
        <span className="truncate text-sm text-muted-foreground">No chat panels open</span>
      </div>
    );
  }

  return (
    <div
      ref={tabListRef}
      className="group/tabbar relative isolate flex min-w-0 flex-1 self-stretch items-center overflow-hidden"
      data-center-panel-tabbar
      data-center-panel-overflow-boundary
      data-overflow={hasOverflow}
    >
      <ScrollArea
        hideScrollbars
        scrollFade
        className="min-w-0 flex-1 self-stretch rounded-none"
        data-center-panel-tab-list
        onWheel={handleWheel}
      >
        <div
          className="flex h-full w-max min-w-full items-center gap-1 px-2"
          role="tablist"
          aria-label="Workspace panels"
        >
          {props.surfaces.map((surface, surfaceIndex) => {
            const active = surface.id === props.activeSurfaceId;
            const title = centerSurfaceTitle(surface, props.hostLabel, props.terminalLabelsById);
            return (
              <SortableCenterTab
                key={surface.id}
                groupId={props.groupId}
                surface={surface}
                title={title}
                active={active}
                surfaceIndex={surfaceIndex}
                dragInProgress={props.dragInProgress}
                onActivate={(entry) => props.onActivate(props.groupId, entry)}
                onClose={(entry) => props.onCloseSurface(props.groupId, entry)}
                onMouseDown={handleTabMouseDown}
                onAuxClick={handleTabAuxClick}
                onContextMenu={(event, entry) => void handleTabContextMenu(event, entry)}
                onKeyDown={handleTabKeyDown}
              />
            );
          })}
        </div>
      </ScrollArea>
      <div
        className="relative z-10 hidden shrink-0 items-center gap-1 border-l border-border/60 bg-background px-1 group-data-[overflow=true]/tabbar:flex"
        data-center-panel-overflow-navigator
      >
        <CenterHeaderIconButton
          aria-label="Previous tabs"
          title="Previous tabs"
          onClick={() => scrollTabPage(-1)}
        >
          <ChevronLeftIcon className="size-4" />
        </CenterHeaderIconButton>
        <CenterHeaderIconButton
          aria-label="Next tabs"
          title="Next tabs"
          onClick={() => scrollTabPage(1)}
        >
          <ChevronRightIcon className="size-4" />
        </CenterHeaderIconButton>
        <DropdownMenu>
          <DropdownMenuTrigger
            render={<CenterHeaderIconButton aria-label="All tabs" title="All tabs" />}
          >
            <Rows3Icon className="size-4" />
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="max-h-80 w-64 overflow-y-auto">
            {props.surfaces.map((surface, surfaceIndex) => {
              const title = centerSurfaceTitle(surface, props.hostLabel, props.terminalLabelsById);
              return (
                <DropdownMenuItem
                  key={surface.id}
                  data-center-panel-all-tab-id={surface.id}
                  aria-current={surface.id === props.activeSurfaceId ? "page" : undefined}
                  onClick={() => activateFromAllTabs(surface, surfaceIndex)}
                >
                  <CenterSurfaceIcon kind={surface.kind} />
                  <span className="truncate">{title}</span>
                </DropdownMenuItem>
              );
            })}
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
    </div>
  );
}
