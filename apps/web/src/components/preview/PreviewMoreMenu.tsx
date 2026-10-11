"use client";

import { Minus, MoreVertical, Plus as PlusIcon, RotateCcw } from "lucide-react";

import { Button } from "~/components/ui/button";
import { Menu, MenuItem, MenuPopup, MenuSeparator, MenuTrigger } from "~/components/ui/menu";
import { Tooltip, TooltipPopup, TooltipTrigger } from "~/components/ui/tooltip";

import { previewBridge } from "./previewBridge";

interface Props {
  /** Active preview tab id. Tab-targeting actions are disabled without it. */
  tabId: string | null;
  /** Current zoom factor as a number (1.0 = 100%). */
  zoomFactor: number;
  /** Fixed viewport modes expose the device toolbar and resize rails. */
  deviceToolbarVisible: boolean;
  /** Switches between fill-panel mode and a fixed responsive viewport. */
  onToggleDeviceToolbar: () => void;
  /** Lets the native browser surface yield while the portaled menu is open. */
  onOpenChange?: (open: boolean) => void;
}

/**
 * Three-dot menu in the chrome row. Wires Hard reload, DevTools, zoom
 * controls, and storage-clearing actions. Only mounted by `PreviewView`
 * when the desktop bridge is present, so we can call it unconditionally.
 */
export function PreviewMoreMenu({
  tabId,
  zoomFactor,
  deviceToolbarVisible,
  onToggleDeviceToolbar,
  onOpenChange,
}: Props) {
  if (!previewBridge) return null;
  const bridge = previewBridge;
  const tabDisabled = !tabId;
  const callTab = (op: (tabId: string) => Promise<void>) => () => {
    if (!tabId) return;
    void op(tabId).catch(() => undefined);
  };

  const zoomLabel = `${Math.round(zoomFactor * 100)}%`;
  return (
    <Menu onOpenChange={onOpenChange}>
      <Tooltip>
        <TooltipTrigger
          render={
            <MenuTrigger
              render={
                <Button variant="ghost" size="icon-xs" type="button" aria-label="Preview menu" />
              }
            />
          }
        >
          <MoreVertical />
        </TooltipTrigger>
        <TooltipPopup>More</TooltipPopup>
      </Tooltip>
      <MenuPopup align="end" sideOffset={6} className="min-w-56">
        <MenuItem onClick={callTab(bridge.hardReload)} disabled={tabDisabled}>
          Hard reload
        </MenuItem>
        <MenuItem onClick={callTab(bridge.openDevTools)} disabled={tabDisabled}>
          Open DevTools
        </MenuItem>
        <MenuItem onClick={onToggleDeviceToolbar} disabled={tabDisabled}>
          {deviceToolbarVisible ? "Hide device toolbar" : "Show device toolbar"}
        </MenuItem>
        <MenuSeparator />
        <div
          role="group"
          aria-label="Zoom"
          className="flex min-h-7 items-center justify-between gap-2 rounded-sm px-2 py-1 text-sm text-foreground"
        >
          <span>Zoom</span>
          <span className="flex items-center gap-1">
            <Button
              variant="outline"
              size="icon-xs"
              type="button"
              onClick={callTab(bridge.zoomOut)}
              aria-label="Zoom out"
              disabled={tabDisabled}
            >
              <Minus />
            </Button>
            <span className="min-w-12 text-center text-xs tabular-nums text-muted-foreground">
              {zoomLabel}
            </span>
            <Button
              variant="outline"
              size="icon-xs"
              type="button"
              onClick={callTab(bridge.zoomIn)}
              aria-label="Zoom in"
              disabled={tabDisabled}
            >
              <PlusIcon />
            </Button>
            <Button
              variant="ghost"
              size="icon-xs"
              type="button"
              onClick={callTab(bridge.resetZoom)}
              aria-label="Reset zoom"
              disabled={tabDisabled}
            >
              <RotateCcw />
            </Button>
          </span>
        </div>
        <MenuSeparator />
        <MenuItem onClick={callTab(bridge.clearCookies)} disabled={tabDisabled}>
          Clear cookies
        </MenuItem>
        <MenuItem onClick={callTab(bridge.clearCache)} disabled={tabDisabled}>
          Clear cache
        </MenuItem>
      </MenuPopup>
    </Menu>
  );
}
