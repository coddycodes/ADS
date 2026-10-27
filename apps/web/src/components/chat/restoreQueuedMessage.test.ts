import { describe, expect, it } from "vite-plus/test";
import { type ChatAttachmentId } from "@coddy/contracts";
import type { ComposerAttachment } from "../../composerDraftStore";
import { mergeQueuedMessageIntoDraft } from "./restoreQueuedMessage";

const attachment = (id: string): ComposerAttachment => ({
  type: "file",
  id: id as ChatAttachmentId,
  name: `${id}.txt`,
  mimeType: "text/plain",
  sizeBytes: 1,
  file: new File(["a"], `${id}.txt`, { type: "text/plain" }),
});

describe("mergeQueuedMessageIntoDraft", () => {
  it.each([
    ["", "queued"],
    ["draft", "draft\n\nqueued"],
  ])("restores text into %j without replacing existing work", (prompt, expected) => {
    const draft = { prompt, attachments: [], other: "preserved" };
    const result = mergeQueuedMessageIntoDraft(draft, { text: "queued" }, { maxAttachments: 2 });
    expect(result).toEqual({
      draft: { ...draft, prompt: expected },
      droppedAttachmentCount: 0,
      unrestoredAttachmentCount: 0,
    });
    expect(draft.prompt).toBe(prompt);
  });

  it("appends cached Files up to the limit and counts overflow", () => {
    const current = attachment("current");
    const first = attachment("first");
    const second = attachment("second");
    const result = mergeQueuedMessageIntoDraft(
      { prompt: "draft", attachments: [current] },
      { text: "queued", attachments: [first, second], cachedAttachments: [first, second] },
      { maxAttachments: 2 },
    );
    expect(result.draft.attachments).toEqual([current, first]);
    expect(result.draft.attachments[1]?.file).toBe(first.file);
    expect(result.droppedAttachmentCount).toBe(1);
    expect(result.unrestoredAttachmentCount).toBe(0);
  });

  it("reports remote or reloaded attachments without pretending to restore bytes", () => {
    const result = mergeQueuedMessageIntoDraft(
      { prompt: "", attachments: [] },
      { text: "queued", attachments: [attachment("a"), attachment("b")] },
      { maxAttachments: 2 },
    );
    expect(result).toEqual({
      draft: { prompt: "queued", attachments: [] },
      droppedAttachmentCount: 0,
      unrestoredAttachmentCount: 2,
    });
  });

  it("counts only missing cache entries and leaves a full draft intact", () => {
    const cached = attachment("a");
    const current = attachment("current");
    const result = mergeQueuedMessageIntoDraft(
      { prompt: "draft", attachments: [current] },
      { text: "", attachments: [cached, attachment("b")], cachedAttachments: [cached] },
      { maxAttachments: 1 },
    );
    expect(result.draft).toEqual({ prompt: "draft", attachments: [current] });
    expect(result.droppedAttachmentCount).toBe(1);
    expect(result.unrestoredAttachmentCount).toBe(1);
  });
});
