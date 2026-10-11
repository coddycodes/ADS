import type { DesktopPreviewBridge } from "@bibcode/contracts";

export type PreviewRuntimeCapability =
  | "picker"
  | "recording"
  | "automation"
  | "imageClipboard"
  /** Capturing the page as an image. */
  | "screenshot"
  /** Zoom, developer tools, and clearing site data. */
  | "pageTools";

export interface PreviewRuntimeCapabilities {
  readonly picker: boolean;
  readonly recording: boolean;
  readonly automation: boolean;
  readonly imageClipboard: boolean;
  readonly screenshot: boolean;
  readonly pageTools: boolean;
}

const capabilitiesByBridge = new WeakMap<DesktopPreviewBridge, PreviewRuntimeCapabilities>();

export function registerPreviewRuntimeCapabilities(
  bridge: DesktopPreviewBridge,
  capabilities: PreviewRuntimeCapabilities,
): void {
  capabilitiesByBridge.set(bridge, capabilities);
}

export function supportsPreviewRuntimeCapability(
  bridge: DesktopPreviewBridge | null | undefined,
  capability: PreviewRuntimeCapability,
): boolean {
  if (!bridge) return false;
  return capabilitiesByBridge.get(bridge)?.[capability] ?? true;
}
