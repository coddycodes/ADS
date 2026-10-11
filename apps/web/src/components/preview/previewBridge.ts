import { startBrowserSurfaceSync } from "~/browser/browserSurfaceSync";
import { createFramePreviewBridge } from "~/browser/framePreviewBridge";

/**
 * Browser mode: no desktop host, so no `DesktopBridge.openExternal`, no SSH
 * forwards and no native preview views. A desktop host without preview support
 * (`previewBridge` null) is still not browser mode. Read live, not at import.
 */
export function isBrowserMode(): boolean {
  return typeof window === "undefined" || !window.desktopBridge;
}

/**
 * Browser mode on plain HTTP frames gateway previews in the panel. An HTTPS
 * page cannot frame the HTTP gateway (mixed content), and an IPv6 page's policy
 * cannot name its host's gateway ports, so their previews keep opening in a new
 * tab. Read live, not at import.
 */
export function canFrameGatewayPreviews(): boolean {
  return (
    typeof window !== "undefined" &&
    !window.desktopBridge &&
    typeof location !== "undefined" &&
    location.protocol === "http:" &&
    !location.hostname.startsWith("[")
  );
}

/**
 * True when `host` is this page's own host. A gateway lets only a page on its
 * own host frame it, so browser mode frames no other host's gateway.
 */
export function isThisPageHost(host: string): boolean {
  return (
    typeof location !== "undefined" &&
    location.hostname.replace(/^\[|\]$/g, "").toLowerCase() === host.toLowerCase()
  );
}

/** Why a browser tab served over HTTPS opens previews in a new tab instead of the panel. */
export const HTTPS_PREVIEW_NOTICE =
  "Previews can't show inside a BiBCode page served over HTTPS, so they open in a new tab. The desktop app shows them here.";

/**
 * Module-level handle to the preview host: the desktop's native views, or
 * gateway previews framed in the page in browser mode (see
 * `canFrameGatewayPreviews`). Resolved once at import time so React hooks don't
 * pay for repeated lookups on every render; `null` where neither is available.
 */
export const previewBridge =
  typeof window === "undefined"
    ? null
    : (window.desktopBridge?.preview ?? (canFrameGatewayPreviews() ? framePreviewBridge() : null));

function framePreviewBridge() {
  const bridge = createFramePreviewBridge({ document, window });
  // The Tauri bridge starts its own sync when it installs.
  startBrowserSurfaceSync(bridge);
  return bridge;
}
