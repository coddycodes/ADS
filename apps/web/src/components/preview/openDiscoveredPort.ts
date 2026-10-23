import type { DiscoveredLocalServer, ScopedThreadRef } from "@bibcode/contracts";
import { AsyncResult } from "effect/reactivity";
import {
  mapAtomCommandResult,
  type AtomCommandResult,
} from "@bibcode/client-runtime/state/runtime";

import { resolvePreviewTarget } from "~/browser/browserTargetResolver";
import { showPreviewUnreachableNotice } from "~/browser/linkNotices";
import type { OpenPreviewMutation } from "~/browser/openFileInPreview";
import { useRightPanelStore } from "~/rightPanelStore";
import { openPreviewSession } from "./openPreviewSession";

export async function openDiscoveredPort<E>(input: {
  readonly threadRef: ScopedThreadRef;
  readonly port: DiscoveredLocalServer;
  readonly openPreview: OpenPreviewMutation<E>;
}): Promise<AtomCommandResult<void, E>> {
  const resolution = resolvePreviewTarget(input.threadRef.environmentId, input.port.url);
  if (resolution.kind === "unreachable") {
    showPreviewUnreachableNotice(resolution);
    return AsyncResult.success(undefined);
  }
  const result = await openPreviewSession({
    openPreview: input.openPreview,
    threadRef: input.threadRef,
    url: resolution.url,
  });
  return mapAtomCommandResult(result, (snapshot) => {
    useRightPanelStore.getState().openBrowser(input.threadRef, snapshot.tabId);
  });
}
