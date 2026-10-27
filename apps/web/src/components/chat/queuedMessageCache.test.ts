import { describe, expect, it } from "vite-plus/test";
import { MessageId, type ChatAttachmentId } from "@coddy/contracts";
import { queuedMessageCache } from "./queuedMessageCache";

describe("queuedMessageCache", () => {
  it("returns the session-local Files once and forgets delivered/withdrawn entries", () => {
    const id = MessageId.make("queued-cache");
    const file = new File(["hello"], "hello.txt");
    const attachments = [
      {
        type: "file" as const,
        id: "file" as ChatAttachmentId,
        name: file.name,
        mimeType: "text/plain",
        sizeBytes: file.size,
        file,
      },
    ];
    queuedMessageCache.remember(id, { attachments });
    expect(queuedMessageCache.take(id)?.attachments[0]?.file).toBe(file);
    expect(queuedMessageCache.take(id)).toBeUndefined();
    queuedMessageCache.remember(id, { attachments });
    queuedMessageCache.forget(id);
    expect(queuedMessageCache.take(id)).toBeUndefined();
  });
});
