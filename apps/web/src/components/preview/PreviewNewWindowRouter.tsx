"use client";

import { useEffect } from "react";

import { resolvePreviewTarget } from "~/browser/browserTargetResolver";
import { showPreviewUnreachableNotice } from "~/browser/linkNotices";
import { openUrlInPreview } from "~/browser/openFileInPreview";
import { canonicalizePreviewUrl } from "~/browser/previewGateway";
import { findPreviewThreadForTab } from "~/previewStateStore";
import { previewEnvironment } from "~/state/preview";
import { useAtomCommand } from "~/state/use-atom-command";

import { previewBridge } from "./previewBridge";

export function PreviewNewWindowRouter() {
  const openPreview = useAtomCommand(previewEnvironment.open, { reportFailure: true });
  useEffect(() => {
    if (!previewBridge) return;
    return previewBridge.onNewWindowRequest((tabId, url) => {
      const threadRef = findPreviewThreadForTab(tabId);
      if (!threadRef) return;
      // Popups always become internal tabs, but a server-loopback URL must
      // still be resolved for the thread's environment first. A popup from a
      // gateway page names this client's gateway origin; the new tab's shared
      // state takes the canonical URL, which its native host resolves again.
      const resolution = resolvePreviewTarget(
        threadRef.environmentId,
        canonicalizePreviewUrl(url, tabId),
      );
      if (resolution.kind === "unreachable") {
        showPreviewUnreachableNotice(resolution);
        return;
      }
      void openUrlInPreview({ threadRef, url: resolution.url, openPreview });
    });
  }, [openPreview]);
  return null;
}
