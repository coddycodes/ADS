/**
 * Browser-mode preview host: gateway previews shown in sandboxed iframes inside
 * the preview panel's slots, driven through the same `DesktopPreviewBridge` the
 * desktop's native child webviews implement. The gateway's navigation reporter
 * (`/__bibcode/frame.js`) tells each frame's URL, title, and history to this
 * window, and Back/Forward/Reload go back to the page as messages.
 */
import type { DesktopPreviewBridge, DesktopPreviewTabState } from "@bibcode/contracts";

import { registerPreviewRuntimeCapabilities } from "~/previewRuntimeCapabilities";

const SANDBOX =
  "allow-scripts allow-same-origin allow-forms allow-popups allow-popups-to-escape-sandbox allow-modals allow-downloads";
const BOOTSTRAP_PATH = "/__bibcode/bootstrap";
const FRAME_MESSAGE = "bibcode-preview-frame";
const COMMAND_MESSAGE = "bibcode-preview-command";
/** A link to the server's localhost clicked in the page: BiBCode opens it as a new tab. */
const OPEN_MESSAGE = "bibcode-preview-open";
/** A loaded page that hasn't reported by then has no reporter (its CSP blocked it). */
const SETTLE_WITHOUT_REPORT_MS = 5_000;

export const FRAME_UNSUPPORTED_DESCRIPTION =
  "In a browser tab, BiBCode shows here only dev servers on the machine serving this page. Open this one in a new tab.";

interface FrameTab {
  readonly iframe: HTMLIFrameElement;
  /** The gateway origin the frame shows; messages are accepted only from it. */
  origin: string | null;
  /** The page the frame last reported (or the bootstrap's target), which a moved frame reopens. */
  currentUrl: string | null;
  /**
   * A navigation started and its document hasn't loaded: reports still queued
   * from the replaced page must not overwrite it.
   */
  awaitingLoad: boolean;
  /** Settles a loaded page that never reports; a report cancels it. */
  settleTimer: ReturnType<typeof setTimeout> | undefined;
  state: DesktopPreviewTabState;
}

interface FrameReport {
  readonly url: string;
  readonly title: string;
  readonly canGoBack: boolean;
  readonly canGoForward: boolean;
}

function readReport(data: unknown): FrameReport | null {
  if (typeof data !== "object" || data === null) return null;
  const report = data as Record<string, unknown>;
  if (
    report.type !== FRAME_MESSAGE ||
    typeof report.url !== "string" ||
    typeof report.title !== "string" ||
    typeof report.canGoBack !== "boolean" ||
    typeof report.canGoForward !== "boolean"
  ) {
    return null;
  }
  return {
    url: report.url,
    title: report.title,
    canGoBack: report.canGoBack,
    canGoForward: report.canGoForward,
  };
}

/** The origin of a gateway bootstrap URL this page may frame: the gateway lets only its own host frame it. */
function isOnOrigin(url: string, origin: string | null): boolean {
  try {
    return new URL(url).origin === origin;
  } catch {
    return false;
  }
}

function readOpenRequest(data: unknown): string | null {
  if (typeof data !== "object" || data === null) return null;
  const request = data as Record<string, unknown>;
  if (request.type !== OPEN_MESSAGE || typeof request.url !== "string") return null;
  try {
    const { protocol } = new URL(request.url);
    return protocol === "http:" || protocol === "https:" ? request.url : null;
  } catch {
    return null;
  }
}

function bootstrapOrigin(url: string, pageHost: string): string | null {
  try {
    const parsed = new URL(url);
    return parsed.protocol === "http:" &&
      parsed.pathname === BOOTSTRAP_PATH &&
      parsed.hostname === pageHost
      ? parsed.origin
      : null;
  } catch {
    return null;
  }
}

/**
 * The page a bootstrap URL opens (`to` on the gateway origin). Once the bootstrap
 * set the gateway cookie, it reopens that page; the bootstrap link itself is single-use.
 */
function bootstrapTarget(url: string, origin: string): string {
  const to = new URL(url).searchParams.get("to");
  return `${origin}${to?.startsWith("/") ? to : "/"}`;
}

function unsupported(capability: string): () => Promise<never> {
  return () =>
    Promise.reject(new Error(`${capability} isn't available for previews in a browser tab.`));
}

export interface FramePreviewBridgeDeps {
  readonly document: Document;
  readonly window: Window;
  /** The frame's window, which page messages come from and commands go to. */
  readonly frameWindow?: (iframe: HTMLIFrameElement) => Window | null;
}

export function createFramePreviewBridge(deps: FramePreviewBridgeDeps): DesktopPreviewBridge {
  const { document } = deps;
  const frameWindow = deps.frameWindow ?? ((iframe) => iframe.contentWindow);
  const tabs = new Map<string, FrameTab>();
  // Latest visibility per tab, kept across close: the panel slot presents before
  // the tab host creates the tab, and the bounds stream doesn't resend unchanged ones.
  // ponytail: one entry per tab id ever shown; clear on panel teardown if it matters.
  const shown = new Map<string, boolean>();
  const listeners = new Set<(tabId: string, state: DesktopPreviewTabState) => void>();
  const newWindowListeners = new Set<(tabId: string, url: string) => void>();
  /**
   * Puts the frame in its tab's panel slot (`BrowserSurfaceSlot`), inside the
   * panel's own stacking and focus context. A slot that remounted (the panel
   * moved into the narrow-window sheet) takes the frame with it.
   */
  const mount = (tabId: string, tab: FrameTab) => {
    const slot = document.querySelector(
      `[data-browser-surface-slot="${tabId.replace(/["\\]/g, "\\$&")}"]`,
    );
    if (!slot || tab.iframe.parentElement === slot) return;
    if (tab.iframe.parentElement) {
      // Moving a frame reloads its `src`, and the bootstrap link is single-use:
      // once it loaded (the gateway cookie is set), reopen the page it last
      // reported. Before that only the bootstrap can sign in; if the gateway
      // already redeemed it, the frame explains so and Reload resolves anew.
      if (tab.currentUrl && !tab.awaitingLoad) tab.iframe.setAttribute("src", tab.currentUrl);
      tab.awaitingLoad = true;
    }
    slot.append(tab.iframe);
  };

  const publish = (tabId: string, tab: FrameTab, update: Partial<DesktopPreviewTabState>) => {
    tab.state = { ...tab.state, ...update, updatedAt: new Date().toISOString() };
    for (const listener of listeners) listener(tabId, tab.state);
  };

  const tabFor = (tabId: string): FrameTab | undefined => tabs.get(tabId);

  deps.window.addEventListener("message", (event: MessageEvent) => {
    for (const [tabId, tab] of tabs) {
      if (event.source !== frameWindow(tab.iframe) || event.origin !== tab.origin) continue;
      // A click needs no load event: an interactive page may still be loading images.
      const open = readOpenRequest(event.data);
      if (open) {
        for (const listener of newWindowListeners) listener(tabId, open);
        return;
      }
      const report = readReport(event.data);
      // The page writes its own reports: believe only an address on its own
      // gateway origin (a moved frame reopens it, and BiBCode shares it).
      if (!report || tab.awaitingLoad || !isOnOrigin(report.url, tab.origin)) return;
      clearTimeout(tab.settleTimer);
      tab.currentUrl = report.url;
      publish(tabId, tab, {
        navStatus: { kind: "Success", url: report.url, title: report.title },
        canGoBack: report.canGoBack,
        canGoForward: report.canGoForward,
      });
      return;
    }
  });

  const command = (tabId: string, name: "back" | "forward" | "reload") => {
    const tab = tabFor(tabId);
    if (!tab?.origin) return Promise.resolve();
    frameWindow(tab.iframe)?.postMessage({ type: COMMAND_MESSAGE, command: name }, tab.origin);
    return Promise.resolve();
  };

  const bridge: DesktopPreviewBridge = {
    createTab: (tabId) => {
      if (tabs.has(tabId)) return Promise.resolve();
      const iframe = document.createElement("iframe");
      iframe.setAttribute("sandbox", SANDBOX);
      iframe.setAttribute("allow", "clipboard-read; clipboard-write; fullscreen");
      iframe.setAttribute("referrerpolicy", "no-referrer");
      iframe.title = "Preview";
      Object.assign(iframe.style, {
        position: "absolute",
        inset: "0",
        width: "100%",
        height: "100%",
        border: "0",
        background: "#fff",
        display: shown.get(tabId) ? "block" : "none",
      });
      const tab: FrameTab = {
        iframe,
        origin: null,
        currentUrl: null,
        awaitingLoad: false,
        settleTimer: undefined,
        state: {
          tabId,
          webContentsId: null,
          navStatus: { kind: "Idle" },
          canGoBack: false,
          canGoForward: false,
          zoomFactor: 1,
          controller: "human",
          updatedAt: new Date().toISOString(),
        },
      };
      // Fires before the new document's own report (posted from its load
      // handler). Every document has no history controls until it reports its
      // own, and a page whose CSP blocks the reporter still settles, later.
      iframe.addEventListener("load", () => {
        if (tabs.get(tabId) !== tab || !tab.origin) return;
        tab.awaitingLoad = false;
        publish(tabId, tab, { canGoBack: false, canGoForward: false });
        const nav = tab.state.navStatus;
        if (nav.kind !== "Loading") return;
        // Settling now would share an empty title: the page's own report, posted
        // from its load handler, follows this event and settles it instead.
        clearTimeout(tab.settleTimer);
        tab.settleTimer = setTimeout(() => {
          if (tabs.get(tabId) !== tab || tab.state.navStatus !== nav) return;
          publish(tabId, tab, { navStatus: { kind: "Success", url: nav.url, title: "" } });
        }, SETTLE_WITHOUT_REPORT_MS);
      });
      mount(tabId, tab);
      tabs.set(tabId, tab);
      publish(tabId, tab, {});
      return Promise.resolve();
    },
    forgetEnvironment: () => Promise.resolve(),
    closeTab: (tabId) => {
      clearTimeout(tabs.get(tabId)?.settleTimer);
      tabs.get(tabId)?.iframe.remove();
      tabs.delete(tabId);
      return Promise.resolve();
    },
    // The frame fills its slot, so only visibility matters; every presentation
    // (a remounted slot presents again) re-checks where the frame lives.
    setBounds: (tabId, _bounds, visible) => {
      shown.set(tabId, visible);
      const tab = tabFor(tabId);
      if (!tab) return Promise.resolve();
      if (visible) mount(tabId, tab);
      tab.iframe.style.display = visible ? "block" : "none";
      return Promise.resolve();
    },
    navigate: (tabId, url) => {
      const tab = tabFor(tabId);
      if (!tab) return Promise.resolve();
      const origin = bootstrapOrigin(url, deps.window.location.hostname);
      if (!origin) {
        // The previous page stays under the failure, but no longer reports or obeys.
        tab.origin = null;
        publish(tabId, tab, {
          canGoBack: false,
          canGoForward: false,
          navStatus: {
            kind: "LoadFailed",
            url,
            title: "",
            code: 0,
            description: FRAME_UNSUPPORTED_DESCRIPTION,
          },
        });
        return Promise.resolve();
      }
      tab.origin = origin;
      tab.currentUrl = bootstrapTarget(url, origin);
      tab.awaitingLoad = true;
      tab.iframe.setAttribute("src", url);
      // History comes only from the new page's own reports.
      publish(tabId, tab, {
        navStatus: { kind: "Loading", url, title: "" },
        canGoBack: false,
        canGoForward: false,
      });
      return Promise.resolve();
    },
    goBack: (tabId) => command(tabId, "back"),
    goForward: (tabId) => command(tabId, "forward"),
    refresh: (tabId) => command(tabId, "reload"),
    hardReload: (tabId) => command(tabId, "reload"),
    zoomIn: unsupported("preview.zoom"),
    zoomOut: unsupported("preview.zoom"),
    resetZoom: unsupported("preview.zoom"),
    openDevTools: unsupported("preview.openDevTools"),
    clearCookies: unsupported("preview.clearData"),
    clearCache: unsupported("preview.clearData"),
    setAnnotationTheme: () => Promise.resolve(),
    pickElement: unsupported("preview.pickElement"),
    cancelPickElement: () => Promise.resolve(),
    captureScreenshot: unsupported("preview.captureScreenshot"),
    revealArtifact: unsupported("preview.revealArtifact"),
    copyArtifactToClipboard: unsupported("preview.copyArtifactToClipboard"),
    recording: {
      startScreencast: unsupported("preview.recording"),
      stopScreencast: unsupported("preview.recording"),
      save: unsupported("preview.recording"),
      onFrame: () => () => {},
    },
    automation: {
      status: async (tabId) => {
        const nav = tabFor(tabId)?.state.navStatus;
        const loaded = nav && nav.kind !== "Idle" ? nav : null;
        return {
          available: tabs.has(tabId),
          visible: true,
          tabId,
          url: loaded?.url ?? null,
          title: loaded?.title ?? null,
          loading: nav?.kind === "Loading",
        };
      },
      snapshot: unsupported("preview.automation"),
      click: unsupported("preview.automation"),
      type: unsupported("preview.automation"),
      press: unsupported("preview.automation"),
      scroll: unsupported("preview.automation"),
      evaluate: unsupported("preview.automation"),
      waitFor: unsupported("preview.automation"),
    },
    onStateChange: (listener) => {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    // Popups open as browser tabs on their own (`allow-popups`); this carries
    // only the page's links to the server's localhost.
    onNewWindowRequest: (listener) => {
      newWindowListeners.add(listener);
      return () => {
        newWindowListeners.delete(listener);
      };
    },
    onPointerEvent: () => () => {},
  };

  registerPreviewRuntimeCapabilities(bridge, {
    picker: false,
    recording: false,
    automation: false,
    imageClipboard: false,
    screenshot: false,
    pageTools: false,
  });
  return bridge;
}
