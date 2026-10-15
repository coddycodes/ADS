import { useDroppable } from "@dnd-kit/core";
import { getTerminalLabel } from "@bibcode/shared/terminalLabels";
import { MoreHorizontal } from "lucide-react";
import {
  type FocusEvent,
  type KeyboardEvent,
  type PointerEvent,
  type ReactNode,
  useCallback,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import {
  MAX_CENTER_PANEL_SPLIT_RATIO,
  MIN_CENTER_PANEL_SPLIT_RATIO,
  type CenterPanelGroup,
  type CenterPanelLayoutNode,
  type CenterPanelLayoutPath,
  type CenterPanelSplitDirection,
} from "~/centerPanelLayout";
import type { CenterSurface, ThreadCenterPanelState } from "~/centerPanelStore";
import { cn } from "~/lib/utils";
import { COLLAPSED_SIDEBAR_TITLEBAR_INSET_CLASS } from "~/workspaceTitlebar";
import { Menu, MenuItem, MenuPopup, MenuTrigger } from "~/components/ui/menu";

import { CenterPanelTabs } from "./CenterPanelTabs";
import {
  resolveCenterPaneHeaderDensity,
  type CenterPaneHeaderDensity,
} from "./centerPaneHeaderDensity";

const HORIZONTAL_MINIMUM_PIXELS = 240;
const VERTICAL_MINIMUM_PIXELS = 160;
const KEYBOARD_RATIO_STEP = 0.05;

export interface CenterPanelSplitLayoutProps {
  readonly state: ThreadCenterPanelState;
  readonly hostLabel: string;
  readonly terminalLabelsById?: ReadonlyMap<string, string>;
  readonly dragInProgress: boolean;
  readonly renderFocusedActions: (density: CenterPaneHeaderDensity) => ReactNode;
  readonly registerBodyTarget: (groupId: string) => (node: HTMLDivElement | null) => void;
  readonly onResizeFrame: () => void;
  readonly onFocusGroup: (groupId: string) => void;
  readonly onActivate: (groupId: string, surface: CenterSurface) => void;
  readonly onCloseSurface: (groupId: string, surface: CenterSurface) => void;
  readonly onCloseOtherSurfaces: (groupId: string, surface: CenterSurface) => void;
  readonly onCloseSurfacesToRight: (groupId: string, surface: CenterSurface) => void;
  readonly onCloseAllSurfaces: (groupId: string) => void;
  readonly canMoveToSplit: (groupId: string, direction: CenterPanelSplitDirection) => boolean;
  readonly onMoveToSplit: (
    groupId: string,
    surface: CenterSurface,
    direction: CenterPanelSplitDirection,
  ) => void;
  readonly onMergeGroup: (groupId: string) => void;
  readonly onSetSplitRatio: (path: CenterPanelLayoutPath, ratio: number) => void;
}

interface LayoutEdges {
  readonly touchesTopEdge: boolean;
  readonly touchesLeftEdge: boolean;
  readonly touchesRightEdge: boolean;
}

interface RecursiveLayoutProps extends LayoutEdges {
  readonly node: CenterPanelLayoutNode;
  readonly path: CenterPanelLayoutPath;
  readonly groupNumbers: ReadonlyMap<string, number>;
  readonly groupsById: ReadonlyMap<string, CenterPanelGroup>;
  readonly surfacesById: ReadonlyMap<string, CenterSurface>;
  readonly rootProps: CenterPanelSplitLayoutProps;
}

interface ActiveResize {
  readonly pointerId: number;
  readonly startCoordinate: number;
  readonly startRatio: number;
  readonly axisSize: number;
  readonly minimumPixels: number;
  readonly path: CenterPanelLayoutPath;
}

function collectLeafGroupIds(node: CenterPanelLayoutNode, result: string[] = []): string[] {
  if (node.type === "leaf") {
    result.push(node.groupId);
  } else {
    collectLeafGroupIds(node.first, result);
    collectLeafGroupIds(node.second, result);
  }
  return result;
}

function surfaceTitle(
  surface: CenterSurface | undefined,
  hostLabel: string,
  terminalLabelsById: ReadonlyMap<string, string> | undefined,
): string {
  if (!surface) return "Empty";
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

function minimumRatio(axisSize: number, minimumPixels: number): number {
  if (!Number.isFinite(axisSize) || axisSize <= 0) return 0.5;
  return Math.min(0.5, Math.max(MIN_CENTER_PANEL_SPLIT_RATIO, minimumPixels / axisSize));
}

function clampRatio(ratio: number, axisSize: number, minimumPixels: number): number {
  const minimum = minimumRatio(axisSize, minimumPixels);
  return Math.min(1 - minimum, Math.max(minimum, ratio));
}

function normalizeRatio(ratio: number): number {
  return Math.round(ratio * 1_000_000_000_000) / 1_000_000_000_000;
}

function ratioPercent(ratio: number): string {
  return `${ratio * 100}%`;
}

function setChildBases(
  first: HTMLDivElement | null,
  second: HTMLDivElement | null,
  ratio: number,
): void {
  if (!first || !second) return;
  first.style.flexBasis = ratioPercent(ratio);
  second.style.flexBasis = ratioPercent(1 - ratio);
}

function RecursiveLayout(props: RecursiveLayoutProps) {
  if (props.node.type === "leaf") return <GroupLeaf {...props} node={props.node} />;
  return <SplitNode {...props} node={props.node} />;
}

function GroupLeaf(
  props: RecursiveLayoutProps & { readonly node: Extract<CenterPanelLayoutNode, { type: "leaf" }> },
) {
  const headerRef = useRef<HTMLElement>(null);
  const [density, setDensity] = useState<CenterPaneHeaderDensity>("compact");
  const group = props.groupsById.get(props.node.groupId) ?? {
    id: props.node.groupId,
    surfaceIds: [],
    activeSurfaceId: null,
  };
  const surfaces = group.surfaceIds.flatMap((surfaceId) => {
    const surface = props.surfacesById.get(surfaceId);
    return surface ? [surface] : [];
  });
  const activeSurface = group.activeSurfaceId
    ? props.surfacesById.get(group.activeSurfaceId)
    : undefined;
  const focused = props.rootProps.state.focusedGroupId === group.id;
  const groupNumber = props.groupNumbers.get(group.id) ?? 1;
  const title = surfaceTitle(
    activeSurface,
    props.rootProps.hostLabel,
    props.rootProps.terminalLabelsById,
  );
  const registeredTargetRef = props.rootProps.registerBodyTarget(group.id);
  const { isOver: dropOver, setNodeRef: setDropNodeRef } = useDroppable({
    id: `center-pane:${group.id}`,
    data: { type: "center-panel-pane", groupId: group.id },
  });
  const bodyTargetRef = useCallback(
    (node: HTMLDivElement | null) => {
      registeredTargetRef(node);
      setDropNodeRef(node);
    },
    [registeredTargetRef, setDropNodeRef],
  );
  const focusGroup = useCallback(() => {
    if (!focused) props.rootProps.onFocusGroup(group.id);
  }, [focused, group.id, props.rootProps]);
  const handleFocusCapture = useCallback(
    (_event: FocusEvent<HTMLElement>) => focusGroup(),
    [focusGroup],
  );

  useLayoutEffect(() => {
    const header = headerRef.current;
    if (!header) return;
    const update = (width: number) => {
      const next = resolveCenterPaneHeaderDensity(width);
      setDensity((current) => (current === next ? current : next));
    };
    update(header.getBoundingClientRect().width);
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(([entry]) => {
      if (entry) update(entry.contentRect.width);
    });
    observer.observe(header);
    return () => observer.disconnect();
  }, []);

  return (
    <section
      role="region"
      aria-label={`Center pane ${groupNumber}: ${title}`}
      tabIndex={-1}
      data-center-panel-group
      data-center-panel-group-id={group.id}
      data-focused={focused}
      className={cn(
        "relative flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden bg-background outline-none",
        "after:pointer-events-none after:absolute after:inset-0 after:z-50 after:ring-inset after:content-['']",
        "focus-visible:after:ring-2 focus-visible:after:ring-border",
      )}
      onPointerDownCapture={focusGroup}
      onFocusCapture={handleFocusCapture}
    >
      <header
        ref={headerRef}
        data-center-panel-group-header
        data-touches-top-edge={props.touchesTopEdge}
        data-touches-left-edge={props.touchesLeftEdge}
        data-touches-right-edge={props.touchesRightEdge}
        className={cn(
          "@container/header-actions relative z-30 flex min-w-0 shrink-0 items-center border-b border-panel-separator bg-background",
          props.touchesTopEdge ? "workspace-topbar" : "h-8",
          props.touchesTopEdge &&
            props.touchesLeftEdge &&
            "pl-[calc(env(safe-area-inset-left)+0.75rem)] sm:pl-[calc(env(safe-area-inset-left)+1.25rem)]",
          props.touchesTopEdge && props.touchesLeftEdge && COLLAPSED_SIDEBAR_TITLEBAR_INSET_CLASS,
          props.touchesTopEdge &&
            props.touchesRightEdge &&
            "pr-[calc(env(safe-area-inset-right)+0.75rem)] sm:pr-[calc(env(safe-area-inset-right)+1.25rem)]",
        )}
      >
        <div className="flex h-8 min-w-0 flex-1 items-center">
          <CenterPanelTabs
            groupId={group.id}
            hostLabel={props.rootProps.hostLabel}
            surfaces={surfaces}
            activeSurfaceId={group.activeSurfaceId}
            {...(props.rootProps.terminalLabelsById
              ? { terminalLabelsById: props.rootProps.terminalLabelsById }
              : {})}
            canMoveToSplit={(direction) => props.rootProps.canMoveToSplit(group.id, direction)}
            dragInProgress={props.rootProps.dragInProgress}
            onActivate={props.rootProps.onActivate}
            onCloseSurface={props.rootProps.onCloseSurface}
            onCloseOtherSurfaces={props.rootProps.onCloseOtherSurfaces}
            onCloseSurfacesToRight={props.rootProps.onCloseSurfacesToRight}
            onCloseAllSurfaces={props.rootProps.onCloseAllSurfaces}
            onMoveToSplit={(sourceGroupId, surface, direction) =>
              props.rootProps.onMoveToSplit(sourceGroupId, surface, direction)
            }
          />
        </div>
        {focused ? (
          <div
            data-center-panel-focused-actions
            data-touches-top-right={props.touchesTopEdge && props.touchesRightEdge}
            className="relative z-10 flex h-8 shrink-0 items-center gap-1 bg-background [-webkit-app-region:no-drag]"
          >
            {props.rootProps.state.groups.length > 1 ? (
              <Menu>
                <MenuTrigger
                  aria-label="Pane actions"
                  title="Pane actions"
                  className="flex size-7 items-center justify-center rounded-md text-muted-foreground outline-none hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/70"
                >
                  <MoreHorizontal className="size-4" />
                </MenuTrigger>
                <MenuPopup align="end">
                  <MenuItem onClick={() => props.rootProps.onMergeGroup(group.id)}>
                    Close Split Pane
                  </MenuItem>
                </MenuPopup>
              </Menu>
            ) : null}
            {props.rootProps.renderFocusedActions(density)}
          </div>
        ) : null}
      </header>
      <div
        ref={bodyTargetRef}
        data-center-panel-body-target={group.id}
        data-drop-over={dropOver}
        className="pointer-events-none relative min-h-0 min-w-0 flex-1"
      />
    </section>
  );
}

function SplitNode(
  props: RecursiveLayoutProps & {
    readonly node: Extract<CenterPanelLayoutNode, { type: "split" }>;
  },
) {
  const splitRef = useRef<HTMLDivElement>(null);
  const firstRef = useRef<HTMLDivElement>(null);
  const secondRef = useRef<HTMLDivElement>(null);
  const separatorRef = useRef<HTMLDivElement>(null);
  const activeResizeRef = useRef<ActiveResize | null>(null);
  const pendingRatioRef = useRef<number | null>(null);
  const renderedRatioRef = useRef(props.node.ratio);
  const persistedRatioRef = useRef(props.node.ratio);
  const onResizeFrameRef = useRef(props.rootProps.onResizeFrame);
  const onSetSplitRatioRef = useRef(props.rootProps.onSetSplitRatio);
  const horizontal = props.node.direction === "horizontal";
  const minimumPixels = horizontal ? HORIZONTAL_MINIMUM_PIXELS : VERTICAL_MINIMUM_PIXELS;
  const pathKey = props.path.length === 0 ? "root" : props.path.join(".");

  const readAxisSize = useCallback((): number => {
    const split = splitRef.current;
    if (!split) return 0;
    return horizontal ? split.clientWidth : split.clientHeight;
  }, [horizontal]);

  const applyRenderedRatio = useCallback(
    (ratio: number, axisSize: number) => {
      const renderedRatio = clampRatio(ratio, axisSize, minimumPixels);
      renderedRatioRef.current = renderedRatio;
      setChildBases(firstRef.current, secondRef.current, renderedRatio);
      separatorRef.current?.setAttribute("aria-valuenow", String(Math.round(renderedRatio * 100)));
      onResizeFrameRef.current();
    },
    [minimumPixels],
  );

  useLayoutEffect(() => {
    onResizeFrameRef.current = props.rootProps.onResizeFrame;
    onSetSplitRatioRef.current = props.rootProps.onSetSplitRatio;
    persistedRatioRef.current = props.node.ratio;
  });

  useLayoutEffect(() => {
    if (!activeResizeRef.current) applyRenderedRatio(props.node.ratio, readAxisSize());
  }, [applyRenderedRatio, props.node.ratio, readAxisSize]);

  useLayoutEffect(() => {
    const split = splitRef.current;
    if (!split) return;
    const sync = () => {
      if (activeResizeRef.current) return;
      applyRenderedRatio(persistedRatioRef.current, readAxisSize());
    };
    const observer =
      typeof ResizeObserver === "undefined" ? null : new ResizeObserver(() => sync());
    observer?.observe(split);
    return () => {
      observer?.disconnect();
      activeResizeRef.current = null;
      pendingRatioRef.current = null;
    };
  }, [applyRenderedRatio, readAxisSize]);

  const commitResize = useCallback((pointerId: number) => {
    const resize = activeResizeRef.current;
    if (!resize || resize.pointerId !== pointerId) return;
    const ratio = pendingRatioRef.current;
    activeResizeRef.current = null;
    pendingRatioRef.current = null;
    if (ratio !== null && ratio !== resize.startRatio) {
      onSetSplitRatioRef.current(resize.path, ratio);
    }
  }, []);

  const releasePointer = useCallback((element: HTMLDivElement, pointerId: number) => {
    try {
      if (element.hasPointerCapture(pointerId)) element.releasePointerCapture(pointerId);
    } catch {
      // Pointer capture can already be gone after a platform-level cancellation.
    }
  }, []);

  const handlePointerDown = useCallback(
    (event: PointerEvent<HTMLDivElement>) => {
      if (event.button !== 0 || activeResizeRef.current) return;
      const axisSize = readAxisSize();
      if (!Number.isFinite(axisSize) || axisSize <= 0) return;
      const startRatio = renderedRatioRef.current;
      activeResizeRef.current = {
        pointerId: event.pointerId,
        startCoordinate: horizontal ? event.clientX : event.clientY,
        startRatio,
        axisSize,
        minimumPixels,
        path: props.path,
      };
      pendingRatioRef.current = null;
      try {
        event.currentTarget.setPointerCapture(event.pointerId);
      } catch {
        activeResizeRef.current = null;
        pendingRatioRef.current = null;
        return;
      }
      event.preventDefault();
    },
    [horizontal, minimumPixels, props.path, readAxisSize],
  );

  const handlePointerMove = useCallback(
    (event: PointerEvent<HTMLDivElement>) => {
      const resize = activeResizeRef.current;
      if (!resize || resize.pointerId !== event.pointerId || resize.axisSize <= 0) return;
      const coordinate = horizontal ? event.clientX : event.clientY;
      const rawRatio = resize.startRatio + (coordinate - resize.startCoordinate) / resize.axisSize;
      const ratio = clampRatio(rawRatio, resize.axisSize, resize.minimumPixels);
      setChildBases(firstRef.current, secondRef.current, ratio);
      separatorRef.current?.setAttribute("aria-valuenow", String(Math.round(ratio * 100)));
      renderedRatioRef.current = ratio;
      pendingRatioRef.current = ratio;
      onResizeFrameRef.current();
      event.preventDefault();
    },
    [horizontal],
  );

  const handlePointerEnd = useCallback(
    (event: PointerEvent<HTMLDivElement>) => {
      if (activeResizeRef.current?.pointerId !== event.pointerId) return;
      commitResize(event.pointerId);
      releasePointer(event.currentTarget, event.pointerId);
      event.preventDefault();
    },
    [commitResize, releasePointer],
  );

  const handleLostPointerCapture = useCallback(
    (event: PointerEvent<HTMLDivElement>) => commitResize(event.pointerId),
    [commitResize],
  );

  const handleKeyDown = useCallback(
    (event: KeyboardEvent<HTMLDivElement>) => {
      const delta =
        event.key === "ArrowLeft" || event.key === "ArrowUp"
          ? -KEYBOARD_RATIO_STEP
          : event.key === "ArrowRight" || event.key === "ArrowDown"
            ? KEYBOARD_RATIO_STEP
            : null;
      if (delta === null) return;
      const axisSize = readAxisSize();
      if (!Number.isFinite(axisSize) || axisSize <= 0) return;
      const displayedRatio = normalizeRatio(
        clampRatio(renderedRatioRef.current, axisSize, minimumPixels),
      );
      const ratio = normalizeRatio(clampRatio(displayedRatio + delta, axisSize, minimumPixels));
      if (ratio !== displayedRatio) {
        renderedRatioRef.current = ratio;
        setChildBases(firstRef.current, secondRef.current, ratio);
        separatorRef.current?.setAttribute("aria-valuenow", String(Math.round(ratio * 100)));
        onResizeFrameRef.current();
        onSetSplitRatioRef.current(props.path, ratio);
      }
      event.preventDefault();
    },
    [minimumPixels, props.path, readAxisSize],
  );

  const firstEdges: LayoutEdges = horizontal
    ? {
        touchesTopEdge: props.touchesTopEdge,
        touchesLeftEdge: props.touchesLeftEdge,
        touchesRightEdge: false,
      }
    : {
        touchesTopEdge: props.touchesTopEdge,
        touchesLeftEdge: props.touchesLeftEdge,
        touchesRightEdge: props.touchesRightEdge,
      };
  const secondEdges: LayoutEdges = horizontal
    ? {
        touchesTopEdge: props.touchesTopEdge,
        touchesLeftEdge: false,
        touchesRightEdge: props.touchesRightEdge,
      }
    : {
        touchesTopEdge: false,
        touchesLeftEdge: props.touchesLeftEdge,
        touchesRightEdge: props.touchesRightEdge,
      };

  return (
    <div
      ref={splitRef}
      data-center-panel-split
      data-center-panel-split-direction={props.node.direction}
      data-center-panel-split-path={pathKey}
      className={cn(
        "relative flex min-h-0 min-w-0 flex-1 overflow-hidden",
        horizontal ? "flex-row" : "flex-col",
      )}
    >
      <div
        ref={firstRef}
        data-center-panel-split-child="first"
        className="flex min-h-0 min-w-0 shrink-0 overflow-hidden"
        style={{ flexBasis: ratioPercent(props.node.ratio) }}
      >
        <RecursiveLayout
          {...props}
          {...firstEdges}
          node={props.node.first}
          path={[...props.path, "first"]}
        />
      </div>
      <div
        ref={separatorRef}
        role="separator"
        tabIndex={0}
        aria-label={`Resize ${horizontal ? "horizontal" : "vertical"} center panes`}
        aria-orientation={horizontal ? "vertical" : "horizontal"}
        aria-valuemin={Math.round(MIN_CENTER_PANEL_SPLIT_RATIO * 100)}
        aria-valuemax={Math.round(MAX_CENTER_PANEL_SPLIT_RATIO * 100)}
        aria-valuenow={Math.round(props.node.ratio * 100)}
        className={cn(
          "group/separator relative z-40 shrink-0 touch-none select-none outline-none focus-visible:ring-2 focus-visible:ring-ring/70",
          horizontal ? "-mx-0.75 w-1.5 cursor-col-resize" : "-my-0.75 h-1.5 cursor-row-resize",
        )}
        onPointerDown={handlePointerDown}
        onPointerMove={handlePointerMove}
        onPointerUp={handlePointerEnd}
        onPointerCancel={handlePointerEnd}
        onLostPointerCapture={handleLostPointerCapture}
        onKeyDown={handleKeyDown}
      >
        <span
          aria-hidden="true"
          className={cn(
            "pointer-events-none absolute bg-panel-separator transition-colors group-hover/separator:bg-ring group-focus-visible/separator:bg-ring",
            horizontal
              ? "inset-y-0 left-1/2 w-px -translate-x-1/2"
              : "inset-x-0 top-1/2 h-px -translate-y-1/2",
          )}
        />
      </div>
      <div
        ref={secondRef}
        data-center-panel-split-child="second"
        className="flex min-h-0 min-w-0 shrink-0 overflow-hidden"
        style={{ flexBasis: ratioPercent(1 - props.node.ratio) }}
      >
        <RecursiveLayout
          {...props}
          {...secondEdges}
          node={props.node.second}
          path={[...props.path, "second"]}
        />
      </div>
    </div>
  );
}

export function CenterPanelSplitLayout(props: CenterPanelSplitLayoutProps) {
  const groupsById = useMemo(
    () => new Map(props.state.groups.map((group) => [group.id, group])),
    [props.state.groups],
  );
  const surfacesById = useMemo(
    () => new Map(props.state.surfaces.map((surface) => [surface.id, surface])),
    [props.state.surfaces],
  );
  const groupNumbers = useMemo(
    () =>
      new Map(
        collectLeafGroupIds(props.state.layout).map((groupId, index) => [groupId, index + 1]),
      ),
    [props.state.layout],
  );

  return (
    <div
      data-center-panel-split-layout
      className="flex min-h-0 min-w-0 flex-1 overflow-hidden bg-background"
    >
      <RecursiveLayout
        node={props.state.layout}
        path={[]}
        groupNumbers={groupNumbers}
        groupsById={groupsById}
        surfacesById={surfacesById}
        rootProps={props}
        touchesTopEdge
        touchesLeftEdge
        touchesRightEdge
      />
    </div>
  );
}
