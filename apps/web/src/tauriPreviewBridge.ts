import type {
  DesktopPreviewBounds,
  DesktopPreviewBridge,
  DesktopPreviewScreenshotArtifact,
  DesktopPreviewTabState,
} from "@bibcode/contracts";

import { TauriDesktopCapabilityUnsupportedError } from "./tauriDesktopBridge";
import { registerPreviewRuntimeCapabilities } from "./previewRuntimeCapabilities";

interface PreviewBridgeDeps {
  readonly invoke: <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
  readonly listen: <T>(event: string, listener: (payload: T) => void) => () => void;
}

interface PreviewStateEventPayload {
  readonly tabId: string;
  readonly state: DesktopPreviewTabState;
}

function unsupported(capability: string): () => Promise<never> {
  return () =>
    Promise.reject(
      new TauriDesktopCapabilityUnsupportedError(
        capability,
        capability,
        `preview capability not supported yet on this host: ${capability}`,
      ),
    );
}

export function createTauriPreviewBridge(deps: PreviewBridgeDeps): DesktopPreviewBridge {
  const { invoke, listen } = deps;
  const zoomByTab = new Map<string, number>();
  const stateByTab = new Map<string, DesktopPreviewTabState>();
  const boundsByTab = new Map<
    string,
    { readonly bounds: DesktopPreviewBounds; readonly visible: boolean }
  >();
  const pendingByTab = new Map<string, Promise<void>>();
  const stateListeners = new Set<(tabId: string, state: DesktopPreviewTabState) => void>();
  let stopStateEvents: (() => void) | null = null;
  // Recreating child webviews while switching logical tabs can disconnect the parent app.
  // Keep one native child per preview storage partition (one per environment) for the
  // desktop process lifetime and rebind its logical tabs to it.
  const hostByPartition = new Map<string, string>();
  const partitionOfTab = new Map<string, string>();
  /** Native host -> the logical tab it shows (`null` while hidden with none). */
  const activeByHost = new Map<string, string | null>();

  const hostOf = (tabId: string): string | undefined => {
    const partition = partitionOfTab.get(tabId);
    return partition === undefined ? undefined : hostByPartition.get(partition);
  };
  /** The logical tab for a native host's event; `undefined` when `nativeTabId` is no host. */
  const logicalTabOf = (nativeTabId: string): string | null | undefined =>
    activeByHost.has(nativeTabId) ? (activeByHost.get(nativeTabId) ?? null) : undefined;

  const publishState = (tabId: string, state: DesktopPreviewTabState) => {
    stateByTab.set(tabId, state);
    for (const listener of stateListeners) listener(tabId, state);
  };

  // The native host emits no event for its initial about:blank page, so a new
  // blank tab would otherwise never report itself as present.
  const publishIdleIfUnknown = (tabId: string) => {
    if (stateByTab.has(tabId)) return;
    publishState(tabId, {
      tabId,
      webContentsId: null,
      navStatus: { kind: "Idle" },
      canGoBack: false,
      canGoForward: false,
      zoomFactor: zoomByTab.get(tabId) ?? 1,
      controller: "human",
      updatedAt: new Date().toISOString(),
    });
  };

  const startStateEvents = () => {
    if (stopStateEvents) return;
    stopStateEvents = listen<PreviewStateEventPayload>("preview://state", (payload) => {
      const logicalTabId = logicalTabOf(payload.tabId);
      if (logicalTabId !== undefined) {
        if (logicalTabId === null) return;
        const state = { ...payload.state, tabId: logicalTabId };
        zoomByTab.set(logicalTabId, state.zoomFactor);
        publishState(logicalTabId, state);
        return;
      }
      zoomByTab.set(payload.tabId, payload.state.zoomFactor);
      publishState(payload.tabId, payload.state);
    });
  };

  const enqueueTabOperation = (tabId: string, operation: () => Promise<void>): Promise<void> => {
    const pending = pendingByTab.get(tabId);
    let result: Promise<void>;
    try {
      result = pending === undefined ? operation() : pending.then(operation);
    } catch (error) {
      result = Promise.reject(error);
    }

    const tail = result.then(
      () => {
        if (pendingByTab.get(tabId) === tail) pendingByTab.delete(tabId);
      },
      () => {
        if (pendingByTab.get(tabId) === tail) pendingByTab.delete(tabId);
      },
    );
    pendingByTab.set(tabId, tail);
    return result;
  };

  const invokeForTab = <T>(
    command: string,
    tabId: string,
    args: Record<string, unknown> = {},
  ): Promise<T> => invoke<T>(command, { ...args, tabId: hostOf(tabId) ?? tabId });

  const setZoom = (tabId: string, getFactor: (committed: number) => number): Promise<void> =>
    enqueueTabOperation(tabId, async () => {
      const factor = Math.min(3, Math.max(0.25, getFactor(zoomByTab.get(tabId) ?? 1)));
      await invokeForTab("desktop_preview_set_zoom", tabId, { factor });
      zoomByTab.set(tabId, factor);
      const state = stateByTab.get(tabId);
      if (state) publishState(tabId, { ...state, zoomFactor: factor });
    });

  const bridge: DesktopPreviewBridge = {
    createTab: (tabId, environmentId) =>
      enqueueTabOperation(tabId, async () => {
        // Only an absent environment is the local one: any id, even a falsy one,
        // gets its own storage, so isolation never fails open.
        const isLocal = environmentId === null || environmentId === undefined;
        const partition = isLocal ? "local" : `environment:${environmentId}`;
        partitionOfTab.set(tabId, partition);
        const presentation = boundsByTab.get(tabId);
        const host = hostByPartition.get(partition);
        if (host !== undefined) {
          activeByHost.set(host, tabId);
          if (presentation) {
            await invoke("desktop_preview_set_bounds", {
              tabId: host,
              bounds: presentation.bounds,
              visible: presentation.visible,
            });
          }
          publishIdleIfUnknown(tabId);
          return;
        }
        // While other hosts exist, setBounds held this tab's bounds back.
        const boundsHeldBack = hostByPartition.size > 0;
        await invoke("desktop_preview_create_tab", isLocal ? { tabId } : { tabId, environmentId });
        hostByPartition.set(partition, tabId);
        activeByHost.set(tabId, tabId);
        // The latest bounds: a resize or occlusion may have landed during creation.
        const latest = boundsByTab.get(tabId);
        if (boundsHeldBack && latest) {
          await invoke("desktop_preview_set_bounds", {
            tabId,
            bounds: latest.bounds,
            visible: latest.visible,
          });
        }
        publishIdleIfUnknown(tabId);
      }),
    closeTab: (tabId) =>
      enqueueTabOperation(tabId, async () => {
        const host = hostOf(tabId);
        if (host === undefined) {
          await invoke("desktop_preview_close_tab", { tabId });
        } else if (activeByHost.get(host) === tabId) {
          const presentation = boundsByTab.get(tabId);
          if (presentation) {
            await invoke("desktop_preview_set_bounds", {
              tabId: host,
              bounds: presentation.bounds,
              visible: false,
            });
          }
          activeByHost.set(host, null);
        }
        partitionOfTab.delete(tabId);
        zoomByTab.delete(tabId);
        stateByTab.delete(tabId);
        boundsByTab.delete(tabId);
      }),
    setBounds: (tabId, bounds: DesktopPreviewBounds, visible) => {
      boundsByTab.set(tabId, { bounds, visible });
      const host = hostOf(tabId);
      // A tab not shown on its host, or not yet on one while other hosts exist,
      // gets its bounds from createTab.
      if (host !== undefined ? activeByHost.get(host) !== tabId : hostByPartition.size > 0) {
        return Promise.resolve();
      }
      return invoke("desktop_preview_set_bounds", {
        tabId: host ?? tabId,
        bounds,
        visible,
      });
    },
    navigate: async (tabId, url) => {
      // Until the native load event arrives, status would report the old page as
      // loaded and readiness waits would return early; report the target as loading.
      // A fragment change on the same document fires no page-load event, so it stays as is.
      const previous = stateByTab.get(tabId);
      const currentUrl = previous?.navStatus.kind === "Idle" ? null : previous?.navStatus.url;
      const sameDocument = url.includes("#") && currentUrl?.split("#")[0] === url.split("#")[0];
      const loading: DesktopPreviewTabState | undefined =
        previous && !sameDocument
          ? { ...previous, navStatus: { kind: "Loading", url, title: "" } }
          : undefined;
      if (loading) publishState(tabId, loading);
      try {
        await invokeForTab("desktop_preview_navigate", tabId, { url });
      } catch (error) {
        if (previous && stateByTab.get(tabId) === loading) publishState(tabId, previous);
        throw error;
      }
    },
    goBack: (tabId) => invokeForTab("desktop_preview_go_back", tabId),
    goForward: (tabId) => invokeForTab("desktop_preview_go_forward", tabId),
    refresh: (tabId) => invokeForTab("desktop_preview_refresh", tabId),
    zoomIn: (tabId) => setZoom(tabId, (factor) => factor + 0.1),
    zoomOut: (tabId) => setZoom(tabId, (factor) => factor - 0.1),
    resetZoom: (tabId) => setZoom(tabId, () => 1),
    hardReload: (tabId) => invokeForTab("desktop_preview_hard_reload", tabId),
    openDevTools: (tabId) => invokeForTab("desktop_preview_open_devtools", tabId),
    clearCookies: (tabId) =>
      invokeForTab("desktop_preview_clear_data", tabId, {
        cookies: true,
        cache: false,
        storage: true,
      }),
    clearCache: (tabId) =>
      invokeForTab("desktop_preview_clear_data", tabId, {
        cookies: false,
        cache: true,
        storage: false,
      }),
    setAnnotationTheme: () => Promise.resolve(),
    pickElement: unsupported("preview.pickElement"),
    cancelPickElement: () => Promise.resolve(),
    captureScreenshot: (tabId) =>
      invokeForTab<DesktopPreviewScreenshotArtifact>(
        "desktop_preview_capture_screenshot",
        tabId,
      ).then((artifact) => ({ ...artifact, tabId })),
    revealArtifact: (path) => invoke("desktop_preview_reveal_artifact", { path }),
    forgetEnvironment: (environmentId) =>
      invoke("desktop_preview_forget_environment", { environmentId }),
    copyArtifactToClipboard: unsupported("preview.copyArtifactToClipboard"),
    recording: {
      startScreencast: unsupported("preview.recording"),
      stopScreencast: unsupported("preview.recording"),
      save: unsupported("preview.recording"),
      onFrame: () => () => {},
    },
    automation: {
      status: async (tabId) => {
        const state = stateByTab.get(tabId);
        const nav = state?.navStatus;
        const loaded = nav && nav.kind !== "Idle" ? nav : null;
        return {
          available: state !== undefined,
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
      stateListeners.add(listener);
      startStateEvents();
      let active = true;
      return () => {
        if (!active) return;
        active = false;
        stateListeners.delete(listener);
        if (stateListeners.size > 0) return;
        stopStateEvents?.();
        stopStateEvents = null;
      };
    },
    onNewWindowRequest: (listener) =>
      listen<{ tabId: string; url: string }>("preview://new-window", (payload) => {
        // Native children are reused across logical tabs (see hostByPartition);
        // remap exactly like the preview://state handler does.
        const logicalTabId = logicalTabOf(payload.tabId);
        const tabId = logicalTabId === undefined ? payload.tabId : logicalTabId;
        if (tabId !== null) listener(tabId, payload.url);
      }),
    onPointerEvent: () => () => {},
  };

  registerPreviewRuntimeCapabilities(bridge, {
    picker: false,
    recording: false,
    automation: false,
    imageClipboard: false,
    screenshot: true,
    pageTools: true,
  });
  return bridge;
}
