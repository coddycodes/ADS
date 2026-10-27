import { type ReactNode, useEffect, useState } from "react";

import { useResizableWidth } from "~/hooks/useResizableWidth";
import { cn } from "~/lib/utils";

import { RightPanelResizeHandle } from "./RightPanelResizeHandle";

export type PreviewPanelMode = "inline" | "sheet" | "sidebar" | "embedded";

const PREVIEW_PANEL_WIDTH_STORAGE_KEY = "coddy-studio:preview-panel-width";
const PREVIEW_PANEL_MIN_WIDTH = 360;
/** Hard ceiling so a wide monitor can't yield a panel that swallows the chat. */
const PREVIEW_PANEL_MAX_WIDTH_PX = 1400;
/** Fraction of the viewport allowed; the panel is min(this · vw, MAX_PX). */
const PREVIEW_PANEL_MAX_WIDTH_FRACTION = 0.7;
const PREVIEW_PANEL_DEFAULT_WIDTH = 540;

/**
 * Shell for the preview panel. In inline mode the panel is user-resizable
 * via a drag handle on the left edge; width persists per browser. In
 * sheet/sidebar modes the parent owns the size.
 */
export function PreviewPanelShell(props: {
  mode: PreviewPanelMode;
  maximized?: boolean;
  children: ReactNode;
}) {
  const isInline = props.mode === "inline";
  const maxWidth = useViewportClampedMaxWidth();
  const { width, handlers } = useResizableWidth({
    storageKey: PREVIEW_PANEL_WIDTH_STORAGE_KEY,
    defaultWidth: PREVIEW_PANEL_DEFAULT_WIDTH,
    minWidth: PREVIEW_PANEL_MIN_WIDTH,
    maxWidth,
    edge: "left",
  });

  return (
    <div
      className={cn(
        "relative flex h-full min-h-0 min-w-0 flex-col self-stretch bg-background",
        isInline
          ? props.maximized
            ? "flex-1 border-l border-panel-separator"
            : "shrink-0 border-l border-panel-separator"
          : "w-full",
      )}
      style={isInline && !props.maximized ? { width: `${width}px` } : undefined}
      data-preview-panel-mode={props.mode}
      data-preview-panel-maximized={props.maximized ? "true" : "false"}
    >
      {isInline && !props.maximized ? <RightPanelResizeHandle handlers={handlers} /> : null}
      {props.children}
    </div>
  );
}

/**
 * Track viewport width to derive a sensible upper bound for the panel.
 * Resize-aware so dragging the OS window narrower re-clamps the stored
 * width on the next render (the hook's clamp picks this up automatically).
 */
function useViewportClampedMaxWidth(): number {
  const [vw, setVw] = useState(() => (typeof window === "undefined" ? 1280 : window.innerWidth));
  useEffect(() => {
    if (typeof window === "undefined") return;
    let frame = 0;
    const onResize = () => {
      // Coalesce rapid resize events into one rAF tick.
      if (frame !== 0) return;
      frame = window.requestAnimationFrame(() => {
        frame = 0;
        setVw(window.innerWidth);
      });
    };
    window.addEventListener("resize", onResize);
    return () => {
      window.removeEventListener("resize", onResize);
      if (frame !== 0) window.cancelAnimationFrame(frame);
    };
  }, []);
  return Math.min(PREVIEW_PANEL_MAX_WIDTH_PX, Math.floor(vw * PREVIEW_PANEL_MAX_WIDTH_FRACTION));
}
