import { cn } from "~/lib/utils";
import {
  HOST_SURFACE_ID,
  type CenterSurface,
  type ThreadCenterPanelState,
} from "~/centerPanelStore";
import {
  forwardRef,
  memo,
  useCallback,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  useState,
  type ReactNode,
} from "react";

export interface CenterPanelBodyRect {
  readonly left: number;
  readonly top: number;
  readonly width: number;
  readonly height: number;
}

export interface CenterPanelBodyTargetRegistry {
  readonly rootRef: (node: HTMLDivElement | null) => void;
  readonly registerBodyTarget: (groupId: string) => (node: HTMLDivElement | null) => void;
  readonly rects: ReadonlyMap<string, CenterPanelBodyRect>;
  readonly readBodyRect: (groupId: string) => CenterPanelBodyRect | null;
}

export interface CenterPanelSurfaceRenderContext {
  readonly groupId: string;
  readonly visible: boolean;
  readonly focused: boolean;
}

export interface CenterPanelSurfaceHostsHandle {
  readonly syncRects: () => void;
}

interface CenterPanelSurfaceHostsProps {
  readonly state: ThreadCenterPanelState;
  readonly rects: ReadonlyMap<string, CenterPanelBodyRect>;
  readonly readBodyRect: (groupId: string) => CenterPanelBodyRect | null;
  readonly onFocusGroup: (groupId: string) => void;
  readonly renderSurface: (
    surface: CenterSurface,
    context: CenterPanelSurfaceRenderContext,
  ) => ReactNode;
}

interface CenterPanelSurfaceContentProps {
  readonly surface: CenterSurface;
  readonly groupId: string;
  readonly visible: boolean;
  readonly focused: boolean;
  readonly renderSurface: CenterPanelSurfaceHostsProps["renderSurface"];
}

const CenterPanelSurfaceContent = memo(function CenterPanelSurfaceContent(
  props: CenterPanelSurfaceContentProps,
) {
  return props.renderSurface(props.surface, {
    groupId: props.groupId,
    visible: props.visible,
    focused: props.focused,
  });
});

function sameRect(left: CenterPanelBodyRect, right: CenterPanelBodyRect): boolean {
  return (
    left.left === right.left &&
    left.top === right.top &&
    left.width === right.width &&
    left.height === right.height
  );
}

function sameRectMap(
  left: ReadonlyMap<string, CenterPanelBodyRect>,
  right: ReadonlyMap<string, CenterPanelBodyRect>,
): boolean {
  if (left.size !== right.size) return false;
  for (const [groupId, rect] of left) {
    const other = right.get(groupId);
    if (!other || !sameRect(rect, other)) return false;
  }
  return true;
}

function relativeRect(root: Element, target: Element): CenterPanelBodyRect | null {
  const rootRect = root.getBoundingClientRect();
  const targetRect = target.getBoundingClientRect();
  const values = [
    rootRect.left,
    rootRect.top,
    targetRect.left,
    targetRect.top,
    targetRect.width,
    targetRect.height,
  ];
  if (!values.every(Number.isFinite) || targetRect.width < 0 || targetRect.height < 0) return null;
  return {
    left: targetRect.left - rootRect.left,
    top: targetRect.top - rootRect.top,
    width: targetRect.width,
    height: targetRect.height,
  };
}

/** Tracks structural pane body targets in the coordinate space of their shared workspace root. */
export function useCenterPanelBodyTargets(
  groups: ThreadCenterPanelState["groups"],
): CenterPanelBodyTargetRegistry {
  const activationKey = JSON.stringify(groups.map((group) => [group.id, group.activeSurfaceId]));
  const measuredActivationRef = useRef<string | null>(null);
  const rootElementRef = useRef<HTMLDivElement | null>(null);
  const targetElementsRef = useRef(new Map<string, HTMLDivElement>());
  const targetRefCallbacks = useRef(new Map<string, (node: HTMLDivElement | null) => void>());
  const observerRef = useRef<ResizeObserver | null>(null);
  const frameRef = useRef<number | null>(null);
  const [rects, setRects] = useState<ReadonlyMap<string, CenterPanelBodyRect>>(() => new Map());

  const readBodyRect = useCallback((groupId: string): CenterPanelBodyRect | null => {
    const root = rootElementRef.current;
    const target = targetElementsRef.current.get(groupId);
    return root && target ? relativeRect(root, target) : null;
  }, []);

  const publishMeasurements = useCallback(() => {
    frameRef.current = null;
    const root = rootElementRef.current;
    if (!root) return;
    const next = new Map<string, CenterPanelBodyRect>();
    for (const [groupId, target] of targetElementsRef.current) {
      const rect = relativeRect(root, target);
      if (rect) next.set(groupId, rect);
    }
    setRects((previous) => (sameRectMap(previous, next) ? previous : next));
  }, []);

  const scheduleMeasurement = useCallback(() => {
    if (frameRef.current !== null) return;
    frameRef.current = requestAnimationFrame(publishMeasurements);
  }, [publishMeasurements]);

  const rootRef = useCallback(
    (node: HTMLDivElement | null) => {
      const previous = rootElementRef.current;
      if (previous === node) return;
      measuredActivationRef.current = null;
      if (previous) observerRef.current?.unobserve(previous);
      rootElementRef.current = node;
      if (node) observerRef.current?.observe(node);
      scheduleMeasurement();
    },
    [scheduleMeasurement],
  );

  const registerBodyTarget = useCallback(
    (groupId: string) => {
      const cached = targetRefCallbacks.current.get(groupId);
      if (cached) return cached;
      const callback = (node: HTMLDivElement | null) => {
        const previous = targetElementsRef.current.get(groupId);
        if (previous === node) return;
        measuredActivationRef.current = null;
        if (previous) observerRef.current?.unobserve(previous);
        if (node) {
          targetElementsRef.current.set(groupId, node);
          observerRef.current?.observe(node);
        } else {
          targetElementsRef.current.delete(groupId);
        }
        scheduleMeasurement();
      };
      targetRefCallbacks.current.set(groupId, callback);
      return callback;
    },
    [scheduleMeasurement],
  );

  useLayoutEffect(() => {
    const ResizeObserverConstructor = globalThis.ResizeObserver;
    if (ResizeObserverConstructor) {
      const observer = new ResizeObserverConstructor(scheduleMeasurement);
      observerRef.current = observer;
      if (rootElementRef.current) observer.observe(rootElementRef.current);
      for (const target of targetElementsRef.current.values()) observer.observe(target);
    }
    scheduleMeasurement();
    return () => {
      observerRef.current?.disconnect();
      observerRef.current = null;
      if (frameRef.current !== null) {
        cancelAnimationFrame(frameRef.current);
        frameRef.current = null;
      }
    };
  }, [scheduleMeasurement]);

  useLayoutEffect(() => {
    if (measuredActivationRef.current === activationKey) return;
    measuredActivationRef.current = activationKey;
    // A newly active surface must not stay hidden behind a stale rectangle map
    // when WebKit defers paint. Keep ordinary resize notifications frame-batched.
    if (frameRef.current !== null) {
      cancelAnimationFrame(frameRef.current);
      frameRef.current = null;
    }
    publishMeasurements();
  });

  return { rootRef, registerBodyTarget, rects, readBodyRect };
}

export const CenterPanelSurfaceHosts = forwardRef<
  CenterPanelSurfaceHostsHandle,
  CenterPanelSurfaceHostsProps
>(function CenterPanelSurfaceHosts(props, ref) {
  const hostElementsRef = useRef(new Map<string, HTMLDivElement>());
  const setHostElement = useCallback((surfaceId: string, node: HTMLDivElement | null) => {
    if (node) hostElementsRef.current.set(surfaceId, node);
    else hostElementsRef.current.delete(surfaceId);
  }, []);
  const { readBodyRect } = props;
  const syncRects = useCallback(() => {
    for (const element of hostElementsRef.current.values()) {
      const groupId = element.dataset.centerSurfaceGroupId;
      const rect = groupId ? readBodyRect(groupId) : null;
      if (!rect) {
        element.dataset.centerSurfaceGeometry = "invalid";
        element.style.left = "";
        element.style.top = "";
        element.style.width = "";
        element.style.height = "";
        element.style.visibility = "hidden";
        element.style.pointerEvents = "none";
        continue;
      }
      element.dataset.centerSurfaceGeometry = "valid";
      element.style.left = `${rect.left}px`;
      element.style.top = `${rect.top}px`;
      element.style.width = `${rect.width}px`;
      element.style.height = `${rect.height}px`;
      if (element.dataset.visible === "true") {
        element.style.visibility = "";
        element.style.pointerEvents = "";
      } else {
        element.style.visibility = "hidden";
        element.style.pointerEvents = "none";
      }
    }
  }, [readBodyRect]);
  useImperativeHandle(ref, () => ({ syncRects }), [syncRects]);
  useLayoutEffect(syncRects, [props.rects, props.state, syncRects]);

  const membership = new Map<string, string>();
  for (const group of props.state.groups) {
    for (const surfaceId of group.surfaceIds) membership.set(surfaceId, group.id);
  }
  const visibleIds = new Set(
    props.state.groups.flatMap((group) =>
      group.activeSurfaceId === null ? [] : [group.activeSurfaceId],
    ),
  );
  const mounted = props.state.surfaces.filter(
    (surface) =>
      membership.has(surface.id) && (surface.id === HOST_SURFACE_ID || visibleIds.has(surface.id)),
  );

  return (
    <div className="pointer-events-none absolute inset-0">
      {mounted.map((surface) => {
        const groupId = membership.get(surface.id);
        if (!groupId) return null;
        const rect = props.rects.get(groupId);
        const visible = visibleIds.has(surface.id) && rect !== undefined;
        const focusGroup = () => {
          if (props.state.focusedGroupId !== groupId) props.onFocusGroup(groupId);
        };
        return (
          <div
            key={surface.id}
            ref={(node) => setHostElement(surface.id, node)}
            data-center-surface-host={surface.id}
            data-center-surface-group-id={groupId}
            data-visible={String(visible)}
            data-center-surface-geometry={rect ? "valid" : "invalid"}
            className={cn(
              "pointer-events-auto absolute flex min-h-0 min-w-0 flex-col overflow-hidden",
              !visible && "invisible pointer-events-none",
            )}
            style={
              rect ? { left: rect.left, top: rect.top, width: rect.width, height: rect.height } : {}
            }
            onPointerDownCapture={focusGroup}
            onFocusCapture={focusGroup}
          >
            <CenterPanelSurfaceContent
              surface={surface}
              groupId={groupId}
              visible={visible}
              focused={props.state.focusedGroupId === groupId}
              renderSurface={props.renderSurface}
            />
          </div>
        );
      })}
    </div>
  );
});
