import type { AtomCommandResult } from "@coddy/client-runtime/state/runtime";
import type {
  EnvironmentId,
  PreviewCloseInput,
  PreviewSessionSnapshot,
  ScopedThreadRef,
} from "@coddy/contracts";

import { releasePreviewTab } from "~/browser/previewGateway";
import { beginPreviewSessionClose, cancelPreviewSessionClose } from "~/previewStateStore";

interface ClosePreviewSessionInput<E> {
  readonly closePreview: (input: {
    readonly environmentId: EnvironmentId;
    readonly input: PreviewCloseInput;
  }) => Promise<AtomCommandResult<void, E>>;
  readonly snapshot: PreviewSessionSnapshot | null;
  readonly tabId: string;
  readonly threadRef: ScopedThreadRef;
}

/**
 * Optimistically closes a preview while suppressing stale list responses for
 * the same tab. A failed close restores the last known snapshot.
 */
export async function closePreviewSession<E>(
  input: ClosePreviewSessionInput<E>,
): Promise<AtomCommandResult<void, E>> {
  beginPreviewSessionClose(input.threadRef, input.tabId);
  const result = await input.closePreview({
    environmentId: input.threadRef.environmentId,
    input: { threadId: input.threadRef.threadId, tabId: input.tabId },
  });
  if (result._tag === "Failure") {
    cancelPreviewSessionClose(input.threadRef, input.snapshot, input.tabId);
  } else {
    // A closed tab never comes back: its SSH forwards go now, not after its grace period.
    releasePreviewTab(input.threadRef.environmentId, input.tabId);
  }
  return result;
}
