import type { MessageId } from "@coddy/contracts";
import type { ComposerAttachment } from "../../composerDraftStore";

interface QueuedMessageFiles {
  attachments: ComposerAttachment[];
}

// Files cannot be recovered from the server's attachment metadata. This is only
// an enqueue-session file cache; the thread snapshot owns queue state and order.
const filesByMessageId = new Map<MessageId, QueuedMessageFiles>();

export const queuedMessageCache = {
  remember(messageId: MessageId, value: QueuedMessageFiles): void {
    filesByMessageId.set(messageId, { attachments: [...value.attachments] });
  },
  take(messageId: MessageId): QueuedMessageFiles | undefined {
    const value = filesByMessageId.get(messageId);
    filesByMessageId.delete(messageId);
    return value;
  },
  forget(messageId: MessageId): void {
    filesByMessageId.delete(messageId);
  },
};
