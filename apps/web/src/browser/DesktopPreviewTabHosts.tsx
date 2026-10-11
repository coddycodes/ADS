"use client";

import type { PreviewSessionSnapshot, ScopedThreadRef } from "@bibcode/contracts";
import { useEffect, useRef } from "react";

import type { RightPanelSurface } from "~/rightPanelStore";
import { usePreviewBridge } from "~/components/preview/usePreviewBridge";
import { readThreadPreviewState } from "~/previewStateStore";
import { previewEnvironment } from "~/state/preview";
import { useAtomCommand } from "~/state/use-atom-command";

import { acquireDesktopTab } from "./desktopTabLifetime";
import {
  releasePreviewTabAfterGrace,
  resolveForNavigation,
  retainPreviewTab,
} from "./previewGateway";
import { previewPartitionFor } from "./previewPartition";
import { failPreviewTabNavigation } from "./previewTabFailure";

export interface DesktopPreviewTabHostDescriptor {
  readonly tabId: string;
  readonly initialUrl: string | null;
}

export function selectDesktopPreviewTabHosts(
  surfaces: readonly RightPanelSurface[],
  sessions: Readonly<Record<string, PreviewSessionSnapshot>>,
  activeSurfaceId: string | null,
): readonly DesktopPreviewTabHostDescriptor[] {
  const surface = surfaces.find((candidate) => candidate.id === activeSurfaceId);
  if (surface?.kind !== "preview" || surface.resourceId === null) return [];
  const session = sessions[surface.resourceId];
  if (!session) return [];
  return [
    {
      tabId: surface.resourceId,
      initialUrl: session.navStatus._tag === "Idle" ? null : session.navStatus.url,
    },
  ];
}

const readNativeUrl = (threadRef: ScopedThreadRef, tabId: string) =>
  readThreadPreviewState(threadRef).desktopByTabId[tabId]?.url ?? null;

/**
 * True once the tab moved on while its initial URL was resolving: the URL bar
 * commits a new shared URL, and an agent navigates the native view directly.
 */
function isSuperseded(
  threadRef: ScopedThreadRef,
  tabId: string,
  initialUrl: string,
  nativeUrlAtMount: string | null,
): boolean {
  const navStatus = readThreadPreviewState(threadRef).sessions[tabId]?.navStatus;
  if (navStatus !== undefined && navStatus._tag !== "Idle" && navStatus.url !== initialUrl) {
    return true;
  }
  const nativeUrl = readNativeUrl(threadRef, tabId);
  return nativeUrl !== nativeUrlAtMount && nativeUrl !== null && nativeUrl !== initialUrl;
}

export function NativePreviewTabHost(props: {
  readonly threadRef: ScopedThreadRef;
  readonly tabId: string;
  readonly initialUrl: string | null;
}) {
  const { threadRef, tabId, initialUrl } = props;
  const initialUrlRef = useRef(initialUrl);
  // Gateway refusals become the tab's failed state with their own copy.
  const gatewayOpen = useAtomCommand(previewEnvironment.gatewayOpen, { reportFailure: false });
  const reportStatus = useAtomCommand(previewEnvironment.reportStatus, "preview status report");
  // Captured at mount like the initial URL: the host is keyed by tab, and the
  // effect must not re-acquire the native tab when these identities change.
  const navigationRef = useRef({ threadRef, gatewayOpen, reportStatus });

  usePreviewBridge({ threadRef, tabId });

  useEffect(() => {
    let disposed = false;
    retainPreviewTab(tabId);
    const lease = acquireDesktopTab(
      tabId,
      previewPartitionFor(navigationRef.current.threadRef.environmentId),
    );
    const initialUrl = initialUrlRef.current;
    const {
      threadRef: ref,
      gatewayOpen: openGateway,
      reportStatus: report,
    } = navigationRef.current;
    if (initialUrl !== null) {
      const nativeUrlAtMount = readNativeUrl(ref, tabId);
      // The shared URL is canonical; resolve it for this client's webview.
      void resolveForNavigation({
        environmentId: ref.environmentId,
        threadId: ref.threadId,
        canonicalUrl: initialUrl,
        gatewayOpen: openGateway,
        tabId,
      })
        .then(async (target) => {
          if (disposed || isSuperseded(ref, tabId, initialUrl, nativeUrlAtMount)) return;
          if (target.kind === "ok") {
            await lease.navigate(target.url, () => !disposed);
            return;
          }
          // Something loaded meanwhile (a Reload or an agent, even to this same
          // URL): this late failure must not cover it. A fresh native view
          // resetting to Idle (no URL) loaded nothing.
          const nativeUrl = readNativeUrl(ref, tabId);
          if (nativeUrl !== null && nativeUrl !== nativeUrlAtMount) return;
          await failPreviewTabNavigation({
            threadRef: ref,
            tabId,
            url: initialUrl,
            failure: target,
            reportStatus: report,
          });
        })
        .catch(() => undefined);
    }
    return () => {
      disposed = true;
      lease.release();
      // Switching tabs unmounts this host; switching back soon reuses the forwards.
      releasePreviewTabAfterGrace(ref.environmentId, tabId);
    };
  }, [tabId]);

  return null;
}

export function DesktopPreviewTabHosts(props: {
  readonly threadRef: ScopedThreadRef;
  readonly surfaces: readonly RightPanelSurface[];
  readonly sessions: Readonly<Record<string, PreviewSessionSnapshot>>;
  readonly activeSurfaceId: string | null;
}) {
  const { threadRef, surfaces, sessions, activeSurfaceId } = props;
  return selectDesktopPreviewTabHosts(surfaces, sessions, activeSurfaceId).map(
    ({ tabId, initialUrl }) => (
      <NativePreviewTabHost
        key={tabId}
        threadRef={threadRef}
        tabId={tabId}
        initialUrl={initialUrl}
      />
    ),
  );
}
