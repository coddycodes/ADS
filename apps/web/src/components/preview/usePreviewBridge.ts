"use client";

import type {
  DesktopPreviewTabState,
  PreviewReportStatusInput,
  ScopedThreadRef,
  ThreadId,
} from "@bibcode/contracts";
import { useEffect, useRef } from "react";

import { useBrowserPointerStore } from "~/browser/browserPointerStore";
import { canonicalizePreviewUrl, isGatewayClientUrl } from "~/browser/previewGateway";
import {
  applyPreviewDesktopState,
  type DesktopPreviewOverlay,
  setPreviewLocalFailure,
} from "~/previewStateStore";
import { previewEnvironment } from "~/state/preview";
import { useAtomCommand } from "~/state/use-atom-command";

import { isBrowserMode, previewBridge } from "./previewBridge";

/**
 * Mirrors low-latency desktop state into the store and reflects navigation
 * events back to the server. The native preview host owns webview lifetime.
 */
export function usePreviewBridge(input: { threadRef: ScopedThreadRef; tabId: string }): void {
  const { threadRef, tabId } = input;
  const clearBrowserPointer = useBrowserPointerStore((state) => state.clear);
  const reportStatus = useAtomCommand(previewEnvironment.reportStatus, "preview status report");
  const bridge = previewBridge;

  // One bridge subscription does both jobs (mirror state + forward to
  // server) so the desktop bridge keeps a single listener entry per tab.
  const lastReportedUrl = useRef<string | null>(null);
  const lastReportedKind = useRef<DesktopPreviewTabState["navStatus"]["kind"] | null>(null);
  const lastDesktopNavStatus = useRef<DesktopPreviewTabState["navStatus"] | null>(null);
  useEffect(() => {
    if (!bridge || typeof window === "undefined") return;
    lastReportedUrl.current = null;
    lastReportedKind.current = null;
    lastDesktopNavStatus.current = null;
    const unsubscribe = bridge.onStateChange((changedTabId, nativeState) => {
      if (changedTabId !== tabId) return;
      // The webview may sit on a client-specific gateway origin; everything
      // shared or compared below uses the canonical URL.
      const state = canonicalizeDesktopState(nativeState);
      if (shouldClearBrowserPointer(lastDesktopNavStatus.current, state.navStatus)) {
        clearBrowserPointer(tabId);
      }
      if (startsNavigation(lastDesktopNavStatus.current, state.navStatus)) {
        setPreviewLocalFailure(threadRef, tabId, null);
      }
      lastDesktopNavStatus.current = state.navStatus;
      applyPreviewDesktopState(threadRef, tabId, projectDesktopState(state));
      // This client couldn't reach its own gateway origin (a dropped SSH
      // forward, a LAN route): another client may load the page fine, so the
      // failure stays on this client instead of failing the shared tab.
      // In browser mode every failure is the frame's own (it frames only gateway pages).
      if (
        state.navStatus.kind === "LoadFailed" &&
        nativeState.navStatus.kind === "LoadFailed" &&
        (isGatewayClientUrl(nativeState.navStatus.url) || isBrowserMode())
      ) {
        const { url, code, description } = state.navStatus;
        setPreviewLocalFailure(threadRef, tabId, { url, code, description });
        return;
      }
      const reported = buildReportInput({
        threadId: threadRef.threadId,
        tabId,
        state,
        lastReportedUrl: lastReportedUrl.current,
        lastReportedKind: lastReportedKind.current,
      });
      if (!reported) return;
      lastReportedUrl.current = reported.lastReportedUrl;
      lastReportedKind.current = reported.lastReportedKind;
      void reportStatus({
        environmentId: threadRef.environmentId,
        input: reported.input,
      });
    });
    return unsubscribe;
  }, [bridge, clearBrowserPointer, reportStatus, tabId, threadRef]);
}

function canonicalizeDesktopState(state: DesktopPreviewTabState): DesktopPreviewTabState {
  if (state.navStatus.kind === "Idle") return state;
  const url = canonicalizePreviewUrl(state.navStatus.url);
  return url === state.navStatus.url ? state : { ...state, navStatus: { ...state.navStatus, url } };
}

/**
 * A new page load began or finished: Loading, or a page newly settled (after
 * a failure or another URL). A repeated Success for the same page, such as a
 * zoom change, is not one; the native host may skip Loading on history moves.
 */
function startsNavigation(
  previous: DesktopPreviewTabState["navStatus"] | null,
  current: DesktopPreviewTabState["navStatus"],
): boolean {
  if (current.kind === "Loading") return true;
  if (current.kind !== "Success" || previous === null || previous.kind === "Idle") return false;
  return previous.kind !== "Success" || previous.url !== current.url;
}

function shouldClearBrowserPointer(
  previous: DesktopPreviewTabState["navStatus"] | null,
  current: DesktopPreviewTabState["navStatus"],
): boolean {
  if (!previous) return false;
  if (current.kind === "Loading" && previous.kind !== "Loading") return true;
  if (current.kind === "Idle" || previous.kind === "Idle") return false;
  return current.url !== previous.url;
}

function projectDesktopState(state: DesktopPreviewTabState): DesktopPreviewOverlay {
  return {
    url: state.navStatus.kind === "Idle" ? null : state.navStatus.url,
    title: state.navStatus.kind === "Idle" ? "" : state.navStatus.title,
    canGoBack: state.canGoBack,
    canGoForward: state.canGoForward,
    loading: state.navStatus.kind === "Loading",
    zoomFactor: state.zoomFactor,
    controller: state.controller,
  };
}

/**
 * Decide whether a state change warrants an RPC to the server, and shape
 * the report payload.
 *
 * - Idle never reports — the tab is post-close or pre-load and the server
 *   already knows the canonical state from `open` / `closed`.
 * - We dedupe on (kind, url): consecutive Loading→Loading→Loading for the
 *   same URL collapses to a single RPC, ditto Success.
 * - LoadFailed always reports (the server uses it to emit `failed`).
 */
function buildReportInput(args: {
  readonly threadId: ThreadId;
  readonly tabId: string;
  readonly state: DesktopPreviewTabState;
  readonly lastReportedUrl: string | null;
  readonly lastReportedKind: DesktopPreviewTabState["navStatus"]["kind"] | null;
}): {
  readonly input: PreviewReportStatusInput;
  readonly lastReportedUrl: string;
  readonly lastReportedKind: DesktopPreviewTabState["navStatus"]["kind"];
} | null {
  const { threadId, tabId, state, lastReportedUrl, lastReportedKind } = args;
  const status = state.navStatus;
  if (status.kind === "Idle") return null;

  // Skip if we've already reported the same kind+url. LoadFailed always
  // reports (rapid duplicate failures are unusual and worth surfacing).
  const sameAsLast =
    status.kind !== "LoadFailed" &&
    status.kind === lastReportedKind &&
    status.url === lastReportedUrl;
  if (sameAsLast) return null;

  const base = {
    threadId,
    tabId,
    canGoBack: state.canGoBack,
    canGoForward: state.canGoForward,
  };
  if (status.kind === "LoadFailed") {
    return {
      input: {
        ...base,
        navStatus: {
          _tag: "LoadFailed",
          url: status.url,
          title: status.title,
          code: status.code,
          description: status.description,
        },
      },
      lastReportedUrl: status.url,
      lastReportedKind: "LoadFailed",
    };
  }
  return {
    input: {
      ...base,
      navStatus: { _tag: status.kind, url: status.url, title: status.title },
    },
    lastReportedUrl: status.url,
    lastReportedKind: status.kind,
  };
}
