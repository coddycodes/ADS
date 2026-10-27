import type { PreviewReportStatusInput, ScopedThreadRef } from "@coddy/contracts";

import { setPreviewLocalFailure } from "~/previewStateStore";

import type { PreviewNavigationResolution } from "./previewGateway";

/**
 * Shows why a tab's page couldn't load when nothing will load to say so.
 * A gateway refusal is the same for every client, so it fails the shared tab;
 * a failure of this client's own reach (SSH forward, session, transport) fails
 * the tab on this client only, because another client may load it fine.
 * Either way the tab's Reload resolves the URL again.
 */
export async function failPreviewTabNavigation(input: {
  readonly threadRef: ScopedThreadRef;
  readonly tabId: string;
  readonly url: string;
  readonly failure: Extract<PreviewNavigationResolution, { kind: "unreachable" }>;
  readonly reportStatus: (input: {
    readonly environmentId: ScopedThreadRef["environmentId"];
    readonly input: PreviewReportStatusInput;
  }) => Promise<unknown>;
}): Promise<void> {
  const { threadRef, tabId, url, failure } = input;
  // Code 0: the description is a sentence from Coddy Studio, not a network error name.
  if (!failure.refusedByServer) {
    setPreviewLocalFailure(threadRef, tabId, { url, code: 0, description: failure.message });
    return;
  }
  // The server's reason supersedes an earlier failure of this client's own reach.
  setPreviewLocalFailure(threadRef, tabId, null);
  await input.reportStatus({
    environmentId: threadRef.environmentId,
    input: {
      threadId: threadRef.threadId,
      tabId,
      canGoBack: false,
      canGoForward: false,
      navStatus: { _tag: "LoadFailed", url, title: "", code: 0, description: failure.message },
    },
  });
}
