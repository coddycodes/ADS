import type { DesktopPreviewBridge } from "@coddy/contracts";

import { useBrowserSurfaceStore } from "./browserSurfaceStore";

interface SyncedPresentation {
  readonly x: number;
  readonly y: number;
  readonly width: number;
  readonly height: number;
  readonly visible: boolean;
}

interface TabSyncState {
  applied: SyncedPresentation | undefined;
  scheduled: SyncedPresentation | undefined;
  tail: Promise<void> | undefined;
}

function presentationsEqual(
  left: SyncedPresentation | undefined,
  right: SyncedPresentation,
): boolean {
  return (
    left !== undefined &&
    left.x === right.x &&
    left.y === right.y &&
    left.width === right.width &&
    left.height === right.height &&
    left.visible === right.visible
  );
}

/**
 * Streams `browserSurfaceStore` rects to the native preview host so each
 * tab's child webview tracks the right-panel slot. Started once when the
 * Tauri preview bridge installs; safe to call `stop()` in tests.
 */
export function startBrowserSurfaceSync(
  bridge: Pick<DesktopPreviewBridge, "setBounds">,
): () => void {
  const stateByTabId = new Map<string, TabSyncState>();
  let active = true;

  const schedule = (tabId: string, next: SyncedPresentation): void => {
    let tabState = stateByTabId.get(tabId);
    if (!tabState) {
      tabState = { applied: undefined, scheduled: undefined, tail: undefined };
      stateByTabId.set(tabId, tabState);
    }

    if (presentationsEqual(tabState.scheduled ?? tabState.applied, next)) return;
    tabState.scheduled = next;

    const run = async (): Promise<void> => {
      if (!active) {
        if (tabState.scheduled === next) tabState.scheduled = undefined;
        return;
      }

      try {
        await bridge.setBounds(
          tabId,
          { x: next.x, y: next.y, width: next.width, height: next.height },
          next.visible,
        );
        tabState.applied = next;
        if (tabState.scheduled === next) tabState.scheduled = undefined;
      } catch (error) {
        if (tabState.scheduled === next) tabState.scheduled = undefined;
        console.error("Could not sync browser surface bounds.", { tabId, error });
      }
    };

    const previous = tabState.tail;
    const operation = previous === undefined ? run() : previous.then(run);
    const clearTail = () => {
      if (tabState.tail !== tail) return;
      tabState.tail = undefined;
      if (!active) stateByTabId.delete(tabId);
    };
    const tail = operation.then(clearTail, clearTail);
    tabState.tail = tail;
  };

  const push = (state: ReturnType<typeof useBrowserSurfaceStore.getState>) => {
    const occluded = state.occlusionOwners.size > 0;
    for (const [tabId, presentation] of Object.entries(state.byTabId)) {
      if (!presentation.rect) continue;
      const next: SyncedPresentation = {
        x: presentation.rect.x,
        y: presentation.rect.y,
        width: presentation.rect.width,
        height: presentation.rect.height,
        visible: presentation.visible && !occluded,
      };
      schedule(tabId, next);
    }
  };

  push(useBrowserSurfaceStore.getState());
  const unsubscribe = useBrowserSurfaceStore.subscribe(push);
  return () => {
    if (!active) return;
    active = false;
    unsubscribe();
  };
}
