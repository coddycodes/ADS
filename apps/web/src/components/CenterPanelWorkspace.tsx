import {
  DndContext,
  DragOverlay,
  PointerSensor,
  closestCenter,
  pointerWithin,
  useSensor,
  useSensors,
  type CollisionDetection,
  type DragEndEvent,
  type DragMoveEvent,
  type DragStartEvent,
  type Over,
} from "@dnd-kit/core";
import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
} from "react";

import {
  MAX_CENTER_PANEL_GROUPS,
  canDropCenterPanelSurface,
  findCenterPanelGroup,
  findCenterPanelGroupForSurface,
  type CenterPanelDropRequest,
  type CenterPanelLayoutPath,
  type CenterPanelSplitDirection,
} from "~/centerPanelLayout";
import type { CenterSurface, ThreadCenterPanelState } from "~/centerPanelStore";

import {
  canCenterPanelPaneSplit,
  captureCenterPanelDropGeometry,
  isCenterPanelPaneDropData,
  isCenterPanelTabDragData,
  resolveCenterPanelDropIntent,
  type CenterPanelDropGeometry,
  type CenterPanelDropIntent,
  type CenterPanelPoint,
  type CenterPanelRect,
  type CenterPanelTabDragData,
  type CenterPanelTabRect,
} from "./centerPanelDnd";
import { CenterPanelSplitLayout } from "./CenterPanelSplitLayout";
import type { CenterPaneHeaderDensity } from "./centerPaneHeaderDensity";
import {
  type CenterPanelBodyTargetRegistry,
  CenterPanelSurfaceHosts,
  type CenterPanelSurfaceHostsHandle,
  type CenterPanelSurfaceRenderContext,
  useCenterPanelBodyTargets,
} from "./CenterPanelSurfaceHosts";
import { CenterSurfaceIcon } from "./CenterPanelTabs";

export interface CenterPanelWorkspaceProps {
  readonly state: ThreadCenterPanelState;
  readonly hostLabel: string;
  readonly terminalLabelsById?: ReadonlyMap<string, string>;
  readonly renderFocusedActions: (density: CenterPaneHeaderDensity) => ReactNode;
  readonly renderSurface: (
    surface: CenterSurface,
    context: CenterPanelSurfaceRenderContext,
  ) => ReactNode;
  readonly onFocusGroup: (groupId: string) => void;
  readonly onActivate: (groupId: string, surface: CenterSurface) => void;
  readonly onCloseSurface: (groupId: string, surface: CenterSurface) => void;
  readonly onCloseOtherSurfaces: (groupId: string, surface: CenterSurface) => void;
  readonly onCloseSurfacesToRight: (groupId: string, surface: CenterSurface) => void;
  readonly onCloseAllSurfaces: (groupId: string) => void;
  readonly onDropSurface: (surfaceId: string, target: CenterPanelDropRequest) => void;
  readonly onMergeGroup: (groupId: string) => void;
  readonly onSetSplitRatio: (path: CenterPanelLayoutPath, ratio: number) => void;
}

export interface CenterPanelWorkspaceHandle {
  canSplitGroup(groupId: string, direction: "right" | "down"): boolean;
}

interface TabSnapshot {
  readonly groupId: string;
  readonly index: number;
  readonly rect: CenterPanelTabRect;
}

interface BoundsSnapshot {
  readonly left: number;
  readonly top: number;
  readonly width: number;
  readonly height: number;
}

interface DropSnapshot {
  readonly bounds: BoundsSnapshot;
  readonly geometriesByGroupId: ReadonlyMap<string, CenterPanelDropGeometry>;
  readonly tabsBySurfaceId: ReadonlyMap<string, TabSnapshot>;
}

interface LastDragPosition {
  readonly delta: { readonly x: number; readonly y: number };
  readonly over: Over | null;
}

const NONE_INTENT: CenterPanelDropIntent = { type: "none" };

function finitePointFromEvent(event: Event): CenterPanelPoint | null {
  const candidate = event as Event & { readonly clientX?: unknown; readonly clientY?: unknown };
  return typeof candidate.clientX === "number" &&
    Number.isFinite(candidate.clientX) &&
    typeof candidate.clientY === "number" &&
    Number.isFinite(candidate.clientY)
    ? { x: candidate.clientX, y: candidate.clientY }
    : null;
}

function boundsFromElement(element: Element): BoundsSnapshot | null {
  const rect = element.getBoundingClientRect();
  return [rect.left, rect.top, rect.width, rect.height].every(Number.isFinite)
    ? { left: rect.left, top: rect.top, width: rect.width, height: rect.height }
    : null;
}

function centerPanelRectFromElement(element: Element): CenterPanelRect | null {
  const bounds = boundsFromElement(element);
  if (!bounds || bounds.width <= 0 || bounds.height <= 0) return null;
  return {
    ...bounds,
    right: bounds.left + bounds.width,
    bottom: bounds.top + bounds.height,
  };
}

function captureDropSnapshot(root: HTMLDivElement): DropSnapshot | null {
  const bounds = boundsFromElement(root);
  if (!bounds) return null;

  const geometriesByGroupId = new Map<string, CenterPanelDropGeometry>();
  for (const pane of root.querySelectorAll<HTMLElement>(
    "[data-center-panel-group][data-center-panel-group-id]",
  )) {
    const groupId = pane.dataset.centerPanelGroupId;
    const header = pane.querySelector<HTMLElement>("[data-center-panel-group-header]");
    if (!groupId || !header) continue;
    const paneRect = centerPanelRectFromElement(pane);
    const headerRect = centerPanelRectFromElement(header);
    if (!paneRect || !headerRect) continue;
    const geometry = captureCenterPanelDropGeometry(paneRect, headerRect);
    if (geometry) geometriesByGroupId.set(groupId, geometry);
  }

  const tabsBySurfaceId = new Map<string, TabSnapshot>();
  const nextIndexByGroupId = new Map<string, number>();
  for (const tab of root.querySelectorAll<HTMLElement>(
    "[data-center-panel-tab-id][data-center-panel-group-id]",
  )) {
    const surfaceId = tab.dataset.centerPanelTabId;
    const groupId = tab.dataset.centerPanelGroupId;
    if (!surfaceId || !groupId) continue;
    const rect = centerPanelRectFromElement(tab);
    if (!rect) continue;
    const index = nextIndexByGroupId.get(groupId) ?? 0;
    nextIndexByGroupId.set(groupId, index + 1);
    tabsBySurfaceId.set(surfaceId, {
      groupId,
      index,
      rect: { left: rect.left, width: rect.width },
    });
  }

  return { bounds, geometriesByGroupId, tabsBySurfaceId };
}

function sameBounds(left: BoundsSnapshot, right: BoundsSnapshot): boolean {
  return (
    left.left === right.left &&
    left.top === right.top &&
    left.width === right.width &&
    left.height === right.height
  );
}

function sameIntent(left: CenterPanelDropIntent, right: CenterPanelDropIntent): boolean {
  if (left.type !== right.type) return false;
  switch (left.type) {
    case "split":
      return (
        right.type === "split" &&
        left.groupId === right.groupId &&
        left.direction === right.direction
      );
    case "insert":
      return (
        right.type === "insert" && left.groupId === right.groupId && left.index === right.index
      );
    case "append":
      return right.type === "append" && left.groupId === right.groupId;
    case "none":
      return right.type === "none";
  }
}

function requestForIntent(
  state: ThreadCenterPanelState,
  surfaceId: string,
  intent: Exclude<CenterPanelDropIntent, { readonly type: "none" }>,
): CenterPanelDropRequest {
  switch (intent.type) {
    case "split":
      return { groupId: intent.groupId, splitDirection: intent.direction };
    case "insert": {
      const source = findCenterPanelGroupForSurface(state, surfaceId);
      const sourceIndex = source?.surfaceIds.indexOf(surfaceId) ?? -1;
      const index =
        source?.id === intent.groupId && sourceIndex >= 0 && sourceIndex < intent.index
          ? intent.index - 1
          : intent.index;
      return { groupId: intent.groupId, index };
    }
    case "append":
      return { groupId: intent.groupId };
  }
}

function isLegalIntent(
  state: ThreadCenterPanelState,
  rects: CenterPanelBodyTargetRegistry["rects"],
  activeDrag: CenterPanelTabDragData,
  intent: CenterPanelDropIntent,
): boolean {
  if (intent.type === "none") return false;
  const request = requestForIntent(state, activeDrag.surfaceId, intent);
  if (!canDropCenterPanelSurface(state, activeDrag.surfaceId, request)) {
    return false;
  }
  if (intent.type !== "split") return true;
  const bodyRect = rects.get(intent.groupId);
  return bodyRect !== undefined && canCenterPanelPaneSplit(bodyRect, intent.direction);
}

function directionLabel(direction: CenterPanelSplitDirection): string {
  return direction[0]!.toUpperCase() + direction.slice(1);
}

function previewStyle(
  rect: {
    readonly left: number;
    readonly top: number;
    readonly width: number;
    readonly height: number;
  },
  direction: CenterPanelSplitDirection,
): CSSProperties {
  const horizontal = direction === "left" || direction === "right";
  const width = horizontal ? rect.width / 2 : rect.width;
  const height = horizontal ? rect.height : rect.height / 2;
  return {
    left: rect.left + (direction === "right" ? width : 0),
    top: rect.top + (direction === "down" ? height : 0),
    width,
    height,
  };
}

export const CenterPanelWorkspace = forwardRef<
  CenterPanelWorkspaceHandle,
  CenterPanelWorkspaceProps
>(function CenterPanelWorkspace(props, ref) {
  const { readBodyRect, registerBodyTarget, rects, rootRef } = useCenterPanelBodyTargets(
    props.state.groups,
  );
  const surfaceHostsRef = useRef<CenterPanelSurfaceHostsHandle>(null);
  const workspaceElementRef = useRef<HTMLDivElement | null>(null);
  const propsRef = useRef(props);
  useLayoutEffect(() => {
    propsRef.current = props;
  }, [props]);
  const activeRef = useRef<CenterPanelTabDragData | null>(null);
  const startPointRef = useRef<CenterPanelPoint | null>(null);
  const snapshotRef = useRef<DropSnapshot | null>(null);
  const lastDragPositionRef = useRef<LastDragPosition | null>(null);
  const mountedRef = useRef(true);
  const [active, setActive] = useState<CenterPanelTabDragData | null>(null);
  const [previewIntent, setPreviewIntent] = useState<CenterPanelDropIntent>(NONE_INTENT);

  const sensors = useSensors(useSensor(PointerSensor, { activationConstraint: { distance: 12 } }));
  const collisionDetection = useCallback<CollisionDetection>((args) => {
    const pointer = pointerWithin(args);
    return pointer.length > 0 ? pointer : closestCenter(args);
  }, []);

  const setChangedPreview = useCallback((intent: CenterPanelDropIntent) => {
    if (!mountedRef.current) return;
    setPreviewIntent((current) => (sameIntent(current, intent) ? current : intent));
  }, []);

  const clearDragState = useCallback(() => {
    activeRef.current = null;
    startPointRef.current = null;
    snapshotRef.current = null;
    lastDragPositionRef.current = null;
    if (!mountedRef.current) return;
    setActive((current) => (current === null ? current : null));
    setPreviewIntent((current) => (current.type === "none" ? current : NONE_INTENT));
  }, []);

  const resolveIntent = useCallback(
    (position: LastDragPosition): CenterPanelDropIntent => {
      const activeDrag = activeRef.current;
      const startPoint = startPointRef.current;
      const snapshot = snapshotRef.current;
      if (
        !activeDrag ||
        !startPoint ||
        !snapshot ||
        !Number.isFinite(position.delta.x) ||
        !Number.isFinite(position.delta.y)
      ) {
        return NONE_INTENT;
      }

      const point = {
        x: startPoint.x + position.delta.x,
        y: startPoint.y + position.delta.y,
      };
      const overData = position.over?.data.current;
      let groupId: string;
      let hoveredTab: { readonly rect: CenterPanelTabRect; readonly index: number } | undefined;
      if (isCenterPanelPaneDropData(overData)) {
        groupId = overData.groupId;
      } else if (isCenterPanelTabDragData(overData)) {
        const tab = snapshot.tabsBySurfaceId.get(overData.surfaceId);
        if (!tab || tab.groupId !== overData.groupId) return NONE_INTENT;
        groupId = tab.groupId;
        hoveredTab = { rect: tab.rect, index: tab.index };
      } else {
        return NONE_INTENT;
      }

      const geometry = snapshot.geometriesByGroupId.get(groupId);
      if (!geometry) return NONE_INTENT;
      const intent = resolveCenterPanelDropIntent({
        point,
        geometry,
        groupId,
        ...(hoveredTab ? { hoveredTab } : {}),
      });
      return isLegalIntent(propsRef.current.state, rects, activeDrag, intent)
        ? intent
        : NONE_INTENT;
    },
    [rects],
  );

  const handleDragStart = useCallback(
    (event: DragStartEvent) => {
      const activeData = event.active.data.current;
      const point = finitePointFromEvent(event.activatorEvent);
      const root = workspaceElementRef.current;
      if (!isCenterPanelTabDragData(activeData) || !point || !root) {
        clearDragState();
        return;
      }
      const snapshot = captureDropSnapshot(root);
      if (!snapshot) {
        clearDragState();
        return;
      }
      activeRef.current = activeData;
      startPointRef.current = point;
      snapshotRef.current = snapshot;
      lastDragPositionRef.current = null;
      setChangedPreview(NONE_INTENT);
      if (mountedRef.current) setActive(activeData);
    },
    [clearDragState, setChangedPreview],
  );

  const handleDragMove = useCallback(
    (event: DragMoveEvent) => {
      const position = { delta: event.delta, over: event.over };
      lastDragPositionRef.current = position;
      setChangedPreview(resolveIntent(position));
    },
    [resolveIntent, setChangedPreview],
  );

  const handleDragEnd = useCallback(
    (event: DragEndEvent) => {
      const activeDrag = activeRef.current;
      const intent = resolveIntent({ delta: event.delta, over: event.over });
      if (activeDrag && intent.type !== "none") {
        const currentProps = propsRef.current;
        currentProps.onDropSurface(
          activeDrag.surfaceId,
          requestForIntent(currentProps.state, activeDrag.surfaceId, intent),
        );
      }
      clearDragState();
    },
    [clearDragState, resolveIntent],
  );

  const workspaceRef = useCallback(
    (node: HTMLDivElement | null) => {
      workspaceElementRef.current = node;
      rootRef(node);
    },
    [rootRef],
  );
  const syncSurfaceRects = useCallback(() => {
    surfaceHostsRef.current?.syncRects();
  }, []);
  const recaptureDropSnapshot = useCallback(
    (root: HTMLDivElement) => {
      const next = captureDropSnapshot(root);
      if (!next) return;
      snapshotRef.current = next;
      const lastPosition = lastDragPositionRef.current;
      if (lastPosition) setChangedPreview(resolveIntent(lastPosition));
    },
    [resolveIntent, setChangedPreview],
  );
  const canSplitGroup = useCallback(
    (groupId: string, direction: "right" | "down"): boolean => {
      const currentProps = propsRef.current;
      if (
        !findCenterPanelGroup(currentProps.state, groupId) ||
        currentProps.state.groups.length >= MAX_CENTER_PANEL_GROUPS
      ) {
        return false;
      }
      const rect = readBodyRect(groupId);
      return rect !== null && canCenterPanelPaneSplit(rect, direction);
    },
    [readBodyRect],
  );
  useImperativeHandle(ref, () => ({ canSplitGroup }), [canSplitGroup]);
  const canMoveToSplit = useCallback(
    (groupId: string, direction: CenterPanelSplitDirection): boolean => {
      const currentProps = propsRef.current;
      const group = findCenterPanelGroup(currentProps.state, groupId);
      const surfaceId = group?.surfaceIds[0];
      const rect = readBodyRect(groupId);
      return Boolean(
        group &&
        group.surfaceIds.length > 1 &&
        surfaceId &&
        rect &&
        canCenterPanelPaneSplit(rect, direction) &&
        canDropCenterPanelSurface(currentProps.state, surfaceId, {
          groupId,
          splitDirection: direction,
        }),
      );
    },
    [readBodyRect],
  );
  const moveToSplit = useCallback(
    (groupId: string, surface: CenterSurface, direction: CenterPanelSplitDirection) => {
      const currentProps = propsRef.current;
      const request = { groupId, splitDirection: direction } as const;
      const rect = readBodyRect(groupId);
      if (
        rect &&
        canCenterPanelPaneSplit(rect, direction) &&
        canDropCenterPanelSurface(currentProps.state, surface.id, request)
      ) {
        currentProps.onDropSurface(surface.id, request);
      }
    },
    [readBodyRect],
  );

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      clearDragState();
    };
  }, [clearDragState]);

  useEffect(() => {
    if (!active) return;
    window.addEventListener("blur", clearDragState);
    return () => window.removeEventListener("blur", clearDragState);
  }, [active, clearDragState]);

  useEffect(() => {
    const root = workspaceElementRef.current;
    if (!active || !root) return;
    const handleTabStripScroll = (event: Event) => {
      const target = event.target;
      if (
        !activeRef.current ||
        !(target instanceof Element) ||
        !target.matches('[data-slot="scroll-area-viewport"]') ||
        !target.closest("[data-center-panel-tab-list]")
      ) {
        return;
      }
      recaptureDropSnapshot(root);
    };
    root.addEventListener("scroll", handleTabStripScroll, true);
    return () => root.removeEventListener("scroll", handleTabStripScroll, true);
  }, [active, recaptureDropSnapshot]);

  useEffect(() => {
    const root = workspaceElementRef.current;
    if (!active || !root || typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(() => {
      const previous = snapshotRef.current;
      const currentBounds = boundsFromElement(root);
      if (!previous || !currentBounds || sameBounds(previous.bounds, currentBounds)) return;
      recaptureDropSnapshot(root);
    });
    observer.observe(root);
    return () => observer.disconnect();
  }, [active, recaptureDropSnapshot]);

  const previewRect = previewIntent.type === "split" ? rects.get(previewIntent.groupId) : undefined;
  const renderedPreviewIntent =
    active &&
    previewIntent.type === "split" &&
    previewRect &&
    isLegalIntent(props.state, rects, active, previewIntent)
      ? previewIntent
      : null;
  const splitPreview =
    renderedPreviewIntent && previewRect ? (
      <div
        data-center-panel-split-preview={renderedPreviewIntent.direction}
        className="pointer-events-none absolute z-30 flex items-center justify-center border border-primary/60 bg-primary/15 text-primary"
        style={previewStyle(previewRect, renderedPreviewIntent.direction)}
      >
        <span className="rounded-md border border-border bg-popover px-2 py-1 text-xs font-medium text-popover-foreground shadow-sm">
          New split: {directionLabel(renderedPreviewIntent.direction)}
        </span>
      </div>
    ) : null;
  const dragOverlay = active ? (
    <div
      role="status"
      aria-label={`Dragging ${active.title}`}
      className="flex max-w-56 items-center gap-1.5 rounded-md border border-border bg-popover px-2 py-1.5 text-sm text-popover-foreground shadow-lg"
    >
      <CenterSurfaceIcon kind={active.surfaceKind} />
      <span className="truncate">{active.title}</span>
    </div>
  ) : null;

  return (
    <DndContext
      sensors={sensors}
      collisionDetection={collisionDetection}
      onDragStart={handleDragStart}
      onDragMove={handleDragMove}
      onDragEnd={handleDragEnd}
      onDragCancel={clearDragState}
    >
      <div
        ref={workspaceRef}
        data-center-panel-workspace
        className="relative flex min-h-0 min-w-0 flex-1 overflow-hidden"
      >
        <CenterPanelSplitLayout
          state={props.state}
          hostLabel={props.hostLabel}
          {...(props.terminalLabelsById ? { terminalLabelsById: props.terminalLabelsById } : {})}
          dragInProgress={active !== null}
          renderFocusedActions={props.renderFocusedActions}
          registerBodyTarget={registerBodyTarget}
          onResizeFrame={syncSurfaceRects}
          onFocusGroup={props.onFocusGroup}
          onActivate={props.onActivate}
          onCloseSurface={props.onCloseSurface}
          onCloseOtherSurfaces={props.onCloseOtherSurfaces}
          onCloseSurfacesToRight={props.onCloseSurfacesToRight}
          onCloseAllSurfaces={props.onCloseAllSurfaces}
          canMoveToSplit={canMoveToSplit}
          onMoveToSplit={moveToSplit}
          onMergeGroup={props.onMergeGroup}
          onSetSplitRatio={props.onSetSplitRatio}
        />
        <div className="pointer-events-none absolute inset-0 z-10">
          <CenterPanelSurfaceHosts
            ref={surfaceHostsRef}
            state={props.state}
            rects={rects}
            readBodyRect={readBodyRect}
            onFocusGroup={props.onFocusGroup}
            renderSurface={props.renderSurface}
          />
        </div>
        {splitPreview}
        <DragOverlay zIndex={60}>{dragOverlay}</DragOverlay>
      </div>
    </DndContext>
  );
});
