import type { ScopedThreadRef } from "@coddy/contracts";

import {
  selectThreadCenterPanelState,
  type ThreadCenterPanelState,
  useCenterPanelStore,
} from "./centerPanelStore";
import {
  selectThreadRightPanelState,
  type ThreadRightPanelState,
  useRightPanelStore,
} from "./rightPanelStore";

function centerHasTerminalSurface(
  centerByThreadKey: Record<string, ThreadCenterPanelState>,
  ref: ScopedThreadRef | null | undefined,
): boolean {
  return selectThreadCenterPanelState(centerByThreadKey, ref).surfaces.some(
    (surface) => surface.kind === "terminal",
  );
}

function rightHasTerminalSurface(
  rightByThreadKey: Record<string, ThreadRightPanelState>,
  ref: ScopedThreadRef | null | undefined,
): boolean {
  return selectThreadRightPanelState(rightByThreadKey, ref).surfaces.some(
    (surface) => surface.kind === "terminal",
  );
}

export function selectThreadHasTerminalSurface(
  centerByThreadKey: Record<string, ThreadCenterPanelState>,
  rightByThreadKey: Record<string, ThreadRightPanelState>,
  ref: ScopedThreadRef | null | undefined,
): boolean {
  return (
    centerHasTerminalSurface(centerByThreadKey, ref) ||
    rightHasTerminalSurface(rightByThreadKey, ref)
  );
}

export function useThreadHasTerminalSurface(ref: ScopedThreadRef | null | undefined): boolean {
  const centerHasTerminal = useCenterPanelStore((state) =>
    centerHasTerminalSurface(state.byThreadKey, ref),
  );
  const rightHasTerminal = useRightPanelStore((state) =>
    rightHasTerminalSurface(state.byThreadKey, ref),
  );
  return centerHasTerminal || rightHasTerminal;
}
