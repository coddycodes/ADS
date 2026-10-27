import { MessageId } from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";

import { buildBootstrapInput } from "./historyBootstrap";

const messageId = (value: string) => MessageId.make(value);

describe("buildBootstrapInput", () => {
  it("includes full transcript when under budget", () => {
    const result = buildBootstrapInput(
      [
        {
          id: messageId("u-1"),
          role: "user",
          text: "hello",
          createdAt: "2026-02-09T00:00:00.000Z",
          turnId: null,
          updatedAt: "2026-02-09T00:00:00.000Z",
          streaming: false,
        },
        {
          id: messageId("a-1"),
          role: "assistant",
          text: "world",
          createdAt: "2026-02-09T00:00:01.000Z",
          turnId: null,
          updatedAt: "2026-02-09T00:00:01.000Z",
          streaming: false,
        },
      ],
      "what's next?",
      1_500,
    );

    expect(result.includedCount).toBe(2);
    expect(result.omittedCount).toBe(0);
    expect(result.truncated).toBe(false);
    expect(result.text).toContain("USER:\nhello");
    expect(result.text).toContain("ASSISTANT:\nworld");
    expect(result.text).toContain("Latest user request (answer this now):");
    expect(result.text).toContain("what's next?");
  });

  it("truncates older transcript messages when over budget", () => {
    const result = buildBootstrapInput(
      [
        {
          id: messageId("u-1"),
          role: "user",
          text: "first question with details",
          createdAt: "2026-02-09T00:00:00.000Z",
          turnId: null,
          updatedAt: "2026-02-09T00:00:00.000Z",
          streaming: false,
        },
        {
          id: messageId("a-1"),
          role: "assistant",
          text: "first answer with details",
          createdAt: "2026-02-09T00:00:01.000Z",
          turnId: null,
          updatedAt: "2026-02-09T00:00:01.000Z",
          streaming: false,
        },
        {
          id: messageId("u-2"),
          role: "user",
          text: "second question with details",
          createdAt: "2026-02-09T00:00:02.000Z",
          turnId: null,
          updatedAt: "2026-02-09T00:00:02.000Z",
          streaming: false,
        },
      ],
      "final request",
      320,
    );

    expect(result.truncated).toBe(true);
    expect(result.omittedCount).toBeGreaterThan(0);
    expect(result.includedCount).toBeLessThan(3);
    expect(result.text).toContain("omitted to stay within input limits");
    expect(result.text.length).toBeLessThanOrEqual(320);
  });

  it("preserves the latest prompt when prompt-only fallback is required", () => {
    const latestPrompt = "Please keep this exact latest prompt.";
    const result = buildBootstrapInput(
      [
        {
          id: messageId("u-1"),
          role: "user",
          text: "old context",
          createdAt: "2026-02-09T00:00:00.000Z",
          turnId: null,
          updatedAt: "2026-02-09T00:00:00.000Z",
          streaming: false,
        },
      ],
      latestPrompt,
      latestPrompt.length + 3,
    );

    expect(result.text).toBe(latestPrompt);
    expect(result.includedCount).toBe(0);
    expect(result.omittedCount).toBe(1);
    expect(result.truncated).toBe(true);
  });

  it("captures user image attachment context in transcript blocks", () => {
    const result = buildBootstrapInput(
      [
        {
          id: messageId("u-image"),
          role: "user",
          text: "",
          attachments: [
            {
              type: "image",
              id: "img-1",
              name: "screenshot.png",
              mimeType: "image/png",
              sizeBytes: 2_048,
            },
          ],
          createdAt: "2026-02-09T00:00:00.000Z",
          turnId: null,
          updatedAt: "2026-02-09T00:00:00.000Z",
          streaming: false,
        },
      ],
      "What does this error mean?",
      1_500,
    );

    expect(result.text).toContain("Attached Image:");
    expect(result.text).toContain("screenshot.png");
  });

  it("summarizes mixed attachments from the first attachment type", () => {
    const result = buildBootstrapInput(
      [
        {
          id: messageId("u-file"),
          role: "user",
          text: "",
          attachments: [
            {
              type: "file",
              id: "file-1",
              name: "notes.txt",
              mimeType: "text/plain",
              sizeBytes: 12,
            },
            {
              type: "image",
              id: "image-1",
              name: "shot.png",
              mimeType: "image/png",
              sizeBytes: 12,
            },
          ],
          createdAt: "2026-02-09T00:00:00.000Z",
          turnId: null,
          updatedAt: "2026-02-09T00:00:00.000Z",
          streaming: false,
        },
      ],
      "Continue",
      1_500,
    );

    expect(result.text).toContain("File: notes.txt");
    expect(result.text).toContain("shot.png");
  });

  it("summarizes text plus multiple images and caps the attachment name list", () => {
    const attachments = ["one.png", "two.png", "three.png", "four.png"].map((name, index) => ({
      type: "image" as const,
      id: `img-${index}`,
      name,
      mimeType: "image/png",
      sizeBytes: 100,
    }));
    const result = buildBootstrapInput(
      [
        {
          id: messageId("u-many-images"),
          role: "user",
          text: "Please inspect these.",
          attachments,
          createdAt: "2026-02-09T00:00:00.000Z",
          turnId: null,
          updatedAt: "2026-02-09T00:00:00.000Z",
          streaming: false,
        },
      ],
      "Which one fails?",
      1_500,
    );

    expect(result.text).toContain(
      "Please inspect these.\n[Attached Image: one.png, two.png, three.png (+1 more)]",
    );
  });

  it("renders empty and text-only messages without attachment summaries", () => {
    const result = buildBootstrapInput(
      [
        {
          id: messageId("empty"),
          role: "user",
          text: "",
          attachments: [],
          createdAt: "2026-02-09T00:00:00.000Z",
          turnId: null,
          updatedAt: "2026-02-09T00:00:00.000Z",
          streaming: false,
        },
        {
          id: messageId("answer"),
          role: "assistant",
          text: "Text only",
          createdAt: "2026-02-09T00:00:01.000Z",
          turnId: null,
          updatedAt: "2026-02-09T00:00:01.000Z",
          streaming: false,
        },
      ],
      "continue",
      1_500,
    );

    expect(result.text).toContain("USER:\n(empty message)");
    expect(result.text).toContain("ASSISTANT:\nText only");
  });

  it("handles sparse transcripts and invalid or fractional budgets", () => {
    const sparseEntries: unknown[] = [];
    sparseEntries.length = 2;
    const sparse = sparseEntries as Parameters<typeof buildBootstrapInput>[0];
    expect(buildBootstrapInput(sparse, "latest", 100)).toEqual({
      text: "latest",
      includedCount: 0,
      omittedCount: 2,
      truncated: true,
    });

    expect(buildBootstrapInput([], "latest", Number.NaN)).toEqual({
      text: "l",
      includedCount: 0,
      omittedCount: 0,
      truncated: true,
    });
    expect(buildBootstrapInput([], "latest", 3.9).text).toBe("lat");
    expect(buildBootstrapInput([], "latest", 0).text).toBe("l");
  });
});
